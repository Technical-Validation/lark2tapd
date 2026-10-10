use super::{escape_html, safe_url, valid_source_id};
use anyhow::Result;
use lark2tapd_core::{Draft, SourceTask};
use markdown::mdast::Node;
use std::collections::HashMap;

/// 纯本地渲染；不会读取 Markdown 指向的文件或请求网络。
pub fn render_description(draft: &Draft, source: &SourceTask) -> Result<String> {
    valid_source_id(&source.guid)?;
    let options = markdown::Options::default();
    let tree = markdown::to_mdast(&draft.description, &options.parse)
        .map_err(|_| anyhow::anyhow!("Invalid Markdown description"))?;
    let mut definitions = HashMap::new();
    collect_definitions(&tree, &mut definitions);
    let mut replacements = Vec::new();
    sanitize(&tree, &definitions, draft, &mut replacements);
    let mut input = draft.description.clone();
    // AST 使用原始字节位置，倒序替换不会破坏中文或后续节点的偏移。
    for (start, end, text) in replacements.into_iter().rev() {
        input.replace_range(start..end, &text);
    }
    let mut html = markdown::to_html_with_options(&input, &options)
        .map_err(|_| anyhow::anyhow!("Invalid Markdown description"))?;
    // 来源标记在 Markdown 之后追加，防止被引用链接或未闭合代码块吞掉。
    html.push_str(&format!("<p>[lark-task:{}]</p>", source.guid));
    let source_url = safe_url(&source.url, true);
    if !source_url.is_empty() {
        html.push_str(&format!("<p>{}</p>", escape_html(&source_url)));
    }
    Ok(html)
}

fn collect_definitions(node: &Node, definitions: &mut HashMap<String, String>) {
    if let Node::Definition(d) = node {
        definitions
            .entry(d.identifier.clone())
            .or_insert_with(|| d.url.clone());
    }
    if let Some(children) = node.children() {
        for child in children {
            collect_definitions(child, definitions);
        }
    }
}

fn safe_link(raw: &str) -> bool {
    url::Url::parse(raw).is_ok_and(|u| {
        matches!(u.scheme(), "http" | "https")
            && u.host_str().is_some()
            && u.username().is_empty()
            && u.password().is_none()
    })
}

// 以实体编码表示不可信纯文本，不能在第二次 Markdown 解析中重新变成语法。
fn literal(text: &str) -> String {
    text.chars().map(|c| format!("&#{};", c as u32)).collect()
}

fn image_markdown(url: &str, alt: &str, draft: &Draft) -> String {
    // 只解析当前草稿的 opaque ID；任意文件路径、外部图片或伪造的回执都不会加载。
    let receipt = url.strip_prefix("attachment:").and_then(|id| {
        let mut matches = draft.attachments.iter().filter(|a| a.id == id);
        let a = matches.next()?;
        if matches.next().is_some() || !a.is_image || a.uncertain {
            return None;
        }
        a.remote_url
            .as_deref()
            .filter(|u| super::media::safe_image_src(u))
    });
    match receipt {
        Some(src) => {
            let src = src
                .replace('&', "&amp;")
                .replace('<', "%3C")
                .replace('>', "%3E")
                .replace('\\', "%5C");
            format!("![{}](<{src}>)", literal(alt))
        }
        None => literal("[图片未上传]"),
    }
}

fn sanitize(
    node: &Node,
    definitions: &HashMap<String, String>,
    draft: &Draft,
    edits: &mut Vec<(usize, usize, String)>,
) {
    let replacement = match node {
        Node::Image(image) => Some(image_markdown(&image.url, &image.alt, draft)),
        Node::ImageReference(image) => Some(image_markdown(
            definitions
                .get(&image.identifier)
                .map(String::as_str)
                .unwrap_or(""),
            &image.alt,
            draft,
        )),
        Node::Link(link) if !safe_link(&link.url) => Some(literal(&node.to_string())),
        Node::LinkReference(link)
            if !definitions
                .get(&link.identifier)
                .is_some_and(|u| safe_link(u)) =>
        {
            Some(literal(&node.to_string()))
        }
        _ => None,
    };
    if let Some(text) = replacement {
        if let Some(p) = node.position() {
            edits.push((p.start.offset, p.end.offset, text));
        }
    } else if let Some(children) = node.children() {
        for child in children {
            sanitize(child, definitions, draft, edits);
        }
    }
}
