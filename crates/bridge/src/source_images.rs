//! 来源图片只允许已知飞书 CDN；正文中的任意外链、本地路径和代码示例都不会下载。
use anyhow::{Result, ensure};
use html5ever::{parse_document, tendril::TendrilSink};
use lark2tapd_core::{DraftAttachment, SourceTask};
use markdown::mdast::Node;
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

// 同一转换遍历同时收集资源与生成 Markdown，引用式图片和原生富文本不能走不同规则。
fn image_markdown(
    url: &str,
    images: &BTreeMap<String, DraftAttachment>,
    urls: &mut Vec<String>,
) -> String {
    urls.push(url.to_owned());
    images
        .get(url)
        .filter(|image| image.is_image && !image.uncertain)
        .map(|image| format!("![图片](attachment:{})", image.id))
        .unwrap_or_else(|| "[来源图片未加载]".into())
}
fn literal_text(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if "\\`*_{}[]<>()#+-.!|&".contains(c) {
                vec!['\\', c]
            } else {
                vec![c]
            }
        })
        .collect()
}
fn html_markdown(
    raw: &str,
    images: &BTreeMap<String, DraftAttachment>,
    urls: &mut Vec<String>,
) -> String {
    fn plain(node: &Handle) -> String {
        match &node.data {
            NodeData::Text { contents } => contents.borrow().to_string(),
            _ => node.children.borrow().iter().map(plain).collect(),
        }
    }
    fn render(
        node: &Handle,
        images: &BTreeMap<String, DraftAttachment>,
        urls: &mut Vec<String>,
    ) -> String {
        match &node.data {
            NodeData::Text { contents } => literal_text(&contents.borrow()),
            NodeData::Comment { .. } => String::new(),
            NodeData::Element { name, attrs, .. } => {
                let tag = name.local.as_ref();
                let attr = |key: &str| {
                    attrs
                        .borrow()
                        .iter()
                        .find(|a| a.name.local.as_ref() == key)
                        .map(|a| a.value.to_string())
                };
                // 代码和可执行元素里的 img 不属于消息图片，不请求任何隐藏资源。
                if matches!(tag, "script" | "style" | "iframe" | "object") {
                    return String::new();
                }
                if tag == "pre" {
                    return format!(
                        "\n\n{}\n\n",
                        plain(node)
                            .lines()
                            .map(|line| format!("    {line}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    );
                }
                if matches!(tag, "code" | "textarea") {
                    return plain(node)
                        .chars()
                        .map(|c| format!("&#{};", c as u32))
                        .collect();
                }
                if tag == "img" {
                    return attr("src")
                        .map(|url| image_markdown(&url, images, urls))
                        .unwrap_or_else(|| "[来源图片未加载]".into());
                }
                let body: String = node
                    .children
                    .borrow()
                    .iter()
                    .map(|child| render(child, images, urls))
                    .collect();
                match tag {
                    "p" | "div" | "figure" | "figcaption" | "ul" | "ol" => {
                        format!("\n\n{body}\n\n")
                    }
                    "br" => "  \n".into(),
                    "strong" | "b" => format!("**{body}**"),
                    "em" | "i" => format!("*{body}*"),
                    "li" => format!("\n- {}\n", body.trim()),
                    "blockquote" => format!(
                        "\n\n{}\n\n",
                        body.trim()
                            .lines()
                            .map(|line| format!("> {line}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    ),
                    "a" => match attr("href").filter(|url| crate::description::safe_link(url)) {
                        Some(url) => format!(
                            "[{body}](<{}>)",
                            url.replace('&', "&amp;")
                                .replace('<', "%3C")
                                .replace('>', "%3E")
                                .replace('\\', "%5C")
                        ),
                        None => body,
                    },
                    _ => body,
                }
            }
            _ => node
                .children
                .borrow()
                .iter()
                .map(|child| render(child, images, urls))
                .collect(),
        }
    }
    // Markdown 的行内 HTML 标签会分成独立节点；成对强调标签可各自转换，代码示例仍由 AST 排除。
    match raw.trim().to_ascii_lowercase().as_str() {
        "<strong>" | "</strong>" | "<b>" | "</b>" => return "**".into(),
        "<em>" | "</em>" | "<i>" | "</i>" => return "*".into(),
        _ => {}
    }
    let dom = parse_document(RcDom::default(), Default::default()).one(raw);
    if !crate::readback::bounded_tree(&dom.document) {
        return literal_text(raw);
    }
    render(&dom.document, images, urls)
}
fn convert(text: &str, images: &BTreeMap<String, DraftAttachment>) -> (String, Vec<String>) {
    fn visit(
        node: &Node,
        defs: &std::collections::HashMap<String, String>,
        images: &BTreeMap<String, DraftAttachment>,
        urls: &mut Vec<String>,
        edits: &mut Vec<(usize, usize, String)>,
    ) {
        let replacement = match node {
            Node::Image(image) => Some(image_markdown(&image.url, images, urls)),
            Node::ImageReference(image) => defs
                .get(&image.identifier)
                .map(|url| image_markdown(url, images, urls)),
            Node::Html(html) => Some(html_markdown(&html.value, images, urls)),
            _ => None,
        };
        if let Some(replacement) = replacement {
            if let Some(p) = node.position() {
                edits.push((p.start.offset, p.end.offset, replacement));
            }
        } else if let Some(children) = node.children() {
            for child in children {
                visit(child, defs, images, urls, edits);
            }
        }
    }
    let mut urls = Vec::new();
    let mut edits = Vec::new();
    if let Ok(tree) = markdown::to_mdast(text, &Default::default()) {
        let mut defs = std::collections::HashMap::new();
        crate::description::collect_definitions(&tree, &mut defs);
        visit(&tree, &defs, images, &mut urls, &mut edits);
    }
    let mut rendered = text.to_owned();
    for (start, end, replacement) in edits.into_iter().rev() {
        rendered.replace_range(start..end, &replacement);
    }
    (rendered, urls)
}
pub fn source_image_urls(source: &SourceTask) -> Vec<String> {
    source
        .metadata
        .source_messages
        .iter()
        .flat_map(|m| convert(&m.content, &BTreeMap::new()).1)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
pub fn needs_source_images(source: &SourceTask) -> bool {
    source_image_urls(source)
        .iter()
        .any(|url| !source.metadata.source_images.contains_key(url))
}
pub fn render_source_images(text: &str, images: &BTreeMap<String, DraftAttachment>) -> String {
    convert(text, images).0
}
fn download_with(raw: &str, run: impl FnOnce(Vec<String>) -> Result<Vec<u8>>) -> Result<Vec<u8>> {
    let mut url = url::Url::parse(raw).map_err(|_| anyhow::anyhow!("Invalid source image URL"))?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && url.fragment().is_none(),
        "Unsupported source image URL"
    );
    ensure!(
        matches!(
            url.host_str(),
            Some(
                "p1.pstatp.com"
                    | "p2.pstatp.com"
                    | "p3.pstatp.com"
                    | "p4.pstatp.com"
                    | "p5.pstatp.com"
                    | "p6.pstatp.com"
                    | "p7.pstatp.com"
                    | "p8.pstatp.com"
                    | "p9.pstatp.com"
            )
        ),
        "Source image host is not approved"
    );
    // 旧任务保留 HTTP 地址；只尝试同主机 HTTPS，不降级明文、不携带认证、不跟随重定向。
    url.set_scheme("https")
        .map_err(|_| anyhow::anyhow!("Invalid source image scheme"))?;
    let mut args = [
        "--disable",
        "--silent",
        "--show-error",
        "--fail",
        "--proto",
        "=https",
        "--max-redirs",
        "0",
        "--max-time",
        "10",
        "--connect-timeout",
        "5",
        "--max-filesize",
        "4194304",
    ]
    .map(str::to_owned)
    .to_vec();
    args.push(url.to_string());
    let bytes = run(args)?;
    ensure!(bytes.len() <= 4 * 1024 * 1024, "Source image exceeds 4 MiB");
    Ok(bytes)
}
pub fn download_source_image(
    raw: &str,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>> {
    download_with(raw, |args| {
        let timeout = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| anyhow::anyhow!("Source image budget exhausted"))?
            .min(Duration::from_secs(10));
        let output = crate::process::run_output_cancellable(
            std::path::Path::new("/usr/bin/curl"),
            &args,
            &[],
            &[],
            timeout,
            cancelled,
        )
        .map_err(|_| {
            anyhow::anyhow!("Source image download failed; check network or CDN availability")
        })?;
        ensure!(
            output.status.success(),
            "Source image download failed; check network or CDN availability"
        );
        Ok(output.stdout.to_vec())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn downloads_are_https_allowlisted_bounded_and_never_use_credentials_or_redirects() {
        let bytes = download_with("http://p2.pstatp.com/origin/a.png", |args| {
            assert_eq!(args[0], "--disable");
            assert!(args.contains(&"=https".to_owned()));
            assert!(
                !args
                    .iter()
                    .any(|arg| arg == "--location" || arg == "--header")
            );
            assert_eq!(args.last().unwrap(), "https://p2.pstatp.com/origin/a.png");
            Ok(vec![1, 2, 3])
        })
        .unwrap();
        assert_eq!(bytes, [1, 2, 3]);
        for url in [
            "file:///private/key",
            "https://evil.example/a.png",
            "https://p2.pstatp.com.evil.test/a.png",
            "https://u:p@p2.pstatp.com/a.png",
            "https://p2.pstatp.com:444/a.png",
        ] {
            assert!(download_with(url, |_| panic!("unsafe request must not execute")).is_err());
        }
        assert!(
            download_with("https://p2.pstatp.com/a.png", |_| Ok(vec![
                0;
                4 * 1024 * 1024
                    + 1
            ]))
            .is_err()
        );
    }
    #[test]
    fn image_like_text_inside_attributes_is_not_a_resource() {
        assert!(
            convert(
                "<a data-x=\"<img src='https://evil.example/a.png'>\">literal</a>",
                &BTreeMap::new()
            )
            .1
            .is_empty()
        );
        assert!(
            convert(
                "<!-- <img src=\"https://evil.example/a.png\"> -->",
                &BTreeMap::new()
            )
            .1
            .is_empty()
        );
    }

    #[test]
    fn figure_wrapped_images_are_real_markdown_nodes_not_raw_html() {
        let dir = tempfile::tempdir().unwrap();
        let image =
            lark2tapd_core::save_pasted_image(dir.path(), b"\x89PNG\r\n\x1a\nfixture", "png")
                .unwrap();
        let images = BTreeMap::from([("https://p2.pstatp.com/image".into(), image.clone())]);
        let body =
            "<p>说明 &amp; 文本</p><figure><img src=\"https://p2.pstatp.com/image\"/></figure>";
        let rendered = render_source_images(body, &images);
        assert_eq!(
            crate::referenced_image_ids(&rendered),
            std::collections::HashSet::from([image.id])
        );
        assert!(markdown::to_html(&rendered).contains("说明 &amp; 文本"));
        assert!(!rendered.contains("<figure>"));
    }

    #[test]
    fn native_image_tags_become_scoped_markdown_without_touching_code_examples() {
        let mut source = SourceTask {
            guid: "task".into(),
            title: "t".into(),
            description: String::new(),
            url: String::new(),
            due: None,
            completed: false,
            metadata: Default::default(),
        };
        let body = "文字\n\n<img src=\"http://p2.pstatp.com/origin/sample.png\" origin-width=\"120\">\n\n```html\n<img src=\"https://evil.example/secret\">\n```";
        source
            .metadata
            .source_messages
            .push(lark2tapd_core::SourceMessage {
                content: body.into(),
                ..Default::default()
            });
        assert_eq!(
            source_image_urls(&source),
            ["http://p2.pstatp.com/origin/sample.png"]
        );
        let dir = tempfile::tempdir().unwrap();
        let image =
            lark2tapd_core::save_pasted_image(dir.path(), b"\x89PNG\r\n\x1a\nfixture", "png")
                .unwrap();
        let images = BTreeMap::from([(
            "http://p2.pstatp.com/origin/sample.png".into(),
            image.clone(),
        )]);
        let rendered = render_source_images(body, &images);
        assert!(rendered.contains(&format!("![图片](attachment:{})", image.id)));
        assert!(rendered.contains("```html\n<img src=\"https://evil.example/secret\">\n```"));
        assert!(!render_source_images(body, &BTreeMap::new()).contains("<img src=\"http://p2"));
    }
}
