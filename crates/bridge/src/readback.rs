//! 按浏览器 HTML 结构比较无损格式变化；不删除图片、属性、正文或代码中的空白。
use html5ever::{parse_document, tendril::TendrilSink};
use markup5ever_rcdom::{Handle, NodeData, RcDom};

#[derive(Debug, PartialEq, Eq)]
enum Node {
    Element(String, Vec<(String, String)>, Vec<Node>),
    Text(String),
    Other(String),
}
fn space(value: &str) -> bool {
    value
        .chars()
        .all(|c| matches!(c, ' ' | '\n' | '\r' | '\t' | '\u{c}'))
}
fn block(node: &Handle) -> bool {
    matches!(&node.data, NodeData::Element { name, .. } if matches!(name.local.as_ref(),
        "p" | "div" | "blockquote" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "pre" | "ul" | "ol" | "li" | "hr" | "table" | "thead" | "tbody" | "tr" | "td" | "th" | "html" | "head" | "body"))
}
fn canonical(node: &Handle) -> Option<Node> {
    let children = node.children.borrow();
    let only_blocks = children.iter().all(|child| {
        block(child)
            || matches!(&child.data, NodeData::Text { contents } if space(&contents.borrow()))
    });
    let preserve = matches!(&node.data, NodeData::Element { name, .. } if matches!(name.local.as_ref(), "pre" | "code" | "textarea" | "script" | "style"));
    let normalized = children
        .iter()
        .filter(|child| {
            !(only_blocks
                && !preserve
                && matches!(&child.data, NodeData::Text { contents } if space(&contents.borrow())))
        })
        .filter_map(canonical)
        .collect::<Vec<_>>();
    match &node.data {
        NodeData::Document => Some(Node::Element("document".into(), vec![], normalized)),
        NodeData::Text { contents } => Some(Node::Text(contents.borrow().to_string())),
        NodeData::Element { name, attrs, .. } => {
            // ensureHtml 的非法 p 外包 block 标签经 HTML5 解析后只留下空 p；它们不包含内容。
            if name.local.as_ref() == "p" && attrs.borrow().is_empty() && children.iter().all(|child| matches!(&child.data, NodeData::Text { contents } if space(&contents.borrow()))) { return None; }
            let mut attrs = attrs
                .borrow()
                .iter()
                // TAPD 为链接补上新窗口目标；只忽略这一已验证属性，不忽略 href 或其他属性。
                .filter(|a| {
                    !(name.local.as_ref() == "a"
                        && a.name.ns.as_ref().is_empty()
                        && a.name.local.as_ref() == "target"
                        && a.value.as_ref() == "_blank")
                })
                .map(|a| {
                    (
                        format!("{}:{}", a.name.ns, a.name.local),
                        a.value.to_string(),
                    )
                })
                .collect::<Vec<_>>();
            attrs.sort();
            Some(Node::Element(
                format!("{}:{}", name.ns, name.local),
                attrs,
                normalized,
            ))
        }
        NodeData::Comment { contents } => Some(Node::Other(format!("comment:{contents}"))),
        _ => Some(Node::Other(format!("{:?}", node.data))),
    }
}
pub(crate) fn bounded_tree(root: &Handle) -> bool {
    let mut stack = vec![(root.clone(), 0)];
    let mut count = 0;
    while let Some((node, depth)) = stack.pop() {
        count += 1;
        if depth > 128 || count > 100_000 {
            return false;
        }
        stack.extend(
            node.children
                .borrow()
                .iter()
                .cloned()
                .map(|child| (child, depth + 1)),
        );
    }
    true
}
pub(super) fn equivalent_html(expected: &str, actual: &str) -> bool {
    if expected == actual {
        return true;
    }
    if expected.len() > 1024 * 1024 || actual.len() > 1024 * 1024 {
        return false;
    }
    let a = parse_document(RcDom::default(), Default::default()).one(expected);
    let b = parse_document(RcDom::default(), Default::default()).one(actual);
    bounded_tree(&a.document)
        && bounded_tree(&b.document)
        && canonical(&a.document) == canonical(&b.document)
}
pub(super) fn equivalent_owner(expected: &str, actual: &str) -> bool {
    let members = |s: &str| {
        s.split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect::<std::collections::BTreeSet<_>>()
    };
    members(expected) == members(actual)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tapd_blank_link_target_is_lossless_but_other_changes_are_not() {
        let expected = "<p>[lark-task:source]</p><p><a href=\"https://applink.feishu.cn/client/todo/detail?guid=source&amp;x=1\">飞书任务 / Lark task</a></p>";
        let actual = format!(
            "<p>{}</p>",
            expected.replace("<a href=", "<a target=\"_blank\" href=")
        );
        assert!(equivalent_html(expected, &actual));
        for changed in [
            actual.replace("guid=source", "guid=other"),
            actual.replace("_blank", "named-frame"),
            actual.replace("<a target=", "<a onclick=\"bad()\" target="),
            actual.replace("飞书任务 / Lark task", "other"),
        ] {
            assert!(!equivalent_html(expected, &changed));
        }
    }

    #[test]
    fn excessive_html_nesting_fails_closed() {
        let html = format!("{}body{}", "<div>".repeat(200), "</div>".repeat(200));
        assert!(!equivalent_html(&html, &format!("<p>{html}</p>")));
    }

    #[test]
    fn image_urls_attributes_and_code_spaces_cannot_disappear() {
        let original = "<blockquote><p>A &amp; B</p><img alt=\"x\" src=\"/tfl/pictures/a.png\"></blockquote><pre> a  b\n\n</pre>";
        assert!(equivalent_html(
            original,
            &original.replace("A &amp; B", "A &#38; B")
        ));
        for changed in [
            original.replace("a.png", "b.png"),
            original.replace("<img alt=\"x\" src=\"/tfl/pictures/a.png\">", "[图片]"),
            original.replace("a  b", "a b"),
        ] {
            assert!(!equivalent_html(original, &changed));
        }
    }
}
