//! 来源会话是当前任务的只读动态视图；只有主动点击才生成可撤销的正文副本。
use super::*;
use lark2tapd_core::SourceMessage;

fn ordered_messages(source: &SourceTask) -> Vec<(usize, &SourceMessage, bool)> {
    let mut entries = Vec::new();
    for message in &source.metadata.source_messages {
        if let Some(matched) = source.metadata.matched_messages_for(message) {
            entries.extend(matched.iter().map(|message| (message, true)));
        } else {
            entries.push((message, false));
        }
    }
    // 先展开匹配，再按最终消息的时间稳定排序；数字来源 ID 本身通常没有时间。
    entries.sort_by_key(|(message, _)| {
        source
            .metadata
            .message_details
            .get(&message.message_id)
            .and_then(|details| tasks::timestamp(details.sent_at.as_deref()))
            .map(|time| time.timestamp_millis())
            .unwrap_or(i64::MAX)
    });
    entries
        .into_iter()
        .enumerate()
        .map(|(index, (message, matched))| (index, message, matched))
        .collect()
}

fn message_header(source: &SourceTask, message: &SourceMessage, language: &str) -> String {
    let details = source.metadata.message_details.get(&message.message_id);
    // 显示和插入副本共用发送人标题；不再附加消息时间或时间缺失提示。
    details
        .and_then(|details| details.sender.as_ref())
        .and_then(|person| source.metadata.person_name(person))
        .unwrap_or_else(|| text(language, "发送人未知", "Unknown sender"))
        .to_owned()
}
fn message_body(source: &SourceTask, message: &SourceMessage, language: &str) -> String {
    if message.content.trim().is_empty() {
        text(
            language,
            "消息内容暂不可用，请在飞书中查看。",
            "Message unavailable. View it in Lark.",
        )
        .to_owned()
    } else {
        lark2tapd_bridge::source_images::render_source_images(
            &message.content,
            &source.metadata.source_images,
        )
    }
}

// 人员名称属于纯文本，不能让名称里的标记/换行改变插入正文的 Markdown 结构。
fn escape_label(label: &str) -> String {
    let mut result = String::new();
    for character in label.chars() {
        if character == '\n' || character == '\r' {
            result.push(' ');
            continue;
        }
        if "\\`*_{}[]<>()#+-.!|".contains(character) {
            result.push('\\');
        }
        result.push(character);
    }
    result
}
fn conversation_markdown(source: &SourceTask, language: &str) -> String {
    if !source
        .metadata
        .source_messages
        .iter()
        .any(|message| !message.content.trim().is_empty())
    {
        return String::new();
    }
    let mut markdown = format!(
        "## {}\n\n",
        text(language, "来源会话", "Source conversation")
    );
    // 每条消息是独立 quote；名称为普通文本，使用编辑器的浅色 quote 样式，不生成标题或折叠标记。
    for (_, message, _) in ordered_messages(source) {
        markdown.push_str(&format!(
            "> {}\n>\n",
            escape_label(&message_header(source, message, language))
        ));
        for line in message_body(source, message, language).split('\n') {
            if line.is_empty() {
                markdown.push_str(">\n");
            } else {
                markdown.push_str(&format!("> {line}\n"));
            }
        }
        markdown.push('\n');
    }
    markdown.push_str("---\n\n");
    markdown
}

impl Workspace {
    pub(super) fn source_conversation(
        &self,
        source: &SourceTask,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let language = self.model.read(cx).language();
        let prefix = conversation_markdown(source, language);
        let images_pending = lark2tapd_bridge::source_images::needs_source_images(source);
        let disabled = !self.can_edit_description(cx)
            || self.description.read(cx).is_composing()
            || prefix.is_empty()
            || images_pending
            || !self.conversation_images_fit(source, &prefix, cx)
            || self.description.read(cx).text().starts_with(&prefix);
        let guid = source.guid.clone();
        let resolve_guid = guid.clone();
        let matching = self
            .model
            .read(cx)
            .activities
            .contains(&format!("conversation:{guid}:{}", self.editor_session));
        let has_matches = source
            .metadata
            .source_messages
            .iter()
            .any(|message| source.metadata.matched_messages_for(message).is_some());
        let match_help = self
            .tr(
                cx,
                "根据原文、会话及顺序匹配，并非消息 ID 的精确关联",
                "Matched by exact text, conversation and order; not by message ID",
            )
            .to_owned();
        div().debug_selector(|| "source-conversation".into()).w_full().min_w_0().flex_shrink_0()
            .rounded_md().border_1().border_color(cx.theme().border).p_3().flex().flex_col().gap_3()
            .child(div().flex().flex_wrap().items_center().gap_2()
                .child(div().flex_1().font_semibold().child(self.tr(cx, "来源会话", "Source conversation")))
                .when(has_matches, |d| d.child(div().id("source-match-label").debug_selector(|| "source-match-label".into()).text_xs().text_color(cx.theme().muted_foreground)
                    .child(self.tr(cx, "原文匹配", "Text-matched"))
                    .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(match_help.clone()).build(window, cx))))
                .when(source.needs_conversation_resolution() || lark2tapd_bridge::source_images::needs_source_images(source), |d| d.child(Button::new("resolve-conversation").ghost().h(px(26.)).text_xs()
                    .label(if matching { self.tr(cx, "匹配中…", "Matching…") } else { self.tr(cx, "补全会话", "Load conversation") })
                    .disabled(matching || self.edit_source.as_ref() != Some(source))
                    .on_click(cx.listener(move |this, _, _, cx| { if this.selected.as_ref() == Some(&resolve_guid) { this.request_conversation(cx); } }))))
                .child(Button::new("insert-conversation").ghost().h(px(26.)).text_xs()
                    .label(self.tr(cx, "插入正文前", "Prepend to description"))
                    .tooltip(if images_pending { self.tr(cx, "来源图片尚未加载，暂不能完整插入正文", "Source images are not loaded; cannot insert the complete conversation yet") } else { self.tr(cx, "插入可编辑副本，支持撤销，不自动保存或同步", "Insert an editable copy; undoable, not automatically saved or synced") })
                    .disabled(disabled)
                    .on_click(cx.listener(move |this, _, window, cx| this.insert_conversation(&guid, window, cx)))))
            .when_some(self.conversation_error.clone(), |d, error| d.child(div().text_xs().text_color(cx.theme().danger).child(error)))
            .child(div().id("source-conversation-list").w_full().min_w_0().max_h(px(360.)).overflow_y_scroll()
                .flex().flex_col().gap_3()
                .children(ordered_messages(source).into_iter().map(|(index, message, _)| {
                    let header_id = format!("source-message-header-{}", message.message_id);
                    div().debug_selector(move || format!("source-message-{index}")).w_full().min_w_0().flex_shrink_0()
                        .border_l_2().border_color(cx.theme().border).pl_3().flex().flex_col().gap_2()
                        .child(div().debug_selector(move || header_id.clone())
                            .text_sm().text_color(cx.theme().muted_foreground).whitespace_normal()
                            .child(message_header(source, message, language)))
                        .child(self.markdown(SharedString::from(format!("source-message-markdown-{index}")), message_body(source, message, language)))
                })))
    }

    pub(super) fn request_conversation(&mut self, cx: &mut Context<Self>) {
        let model = self.model.read(cx);
        if self.is_settings || self.edit_profile != model.profile {
            return;
        }
        let Some(record) = model.snapshot.as_ref().and_then(|snapshot| {
            snapshot.records.iter().find(|record| {
                record.in_scope && Some(&record.source.guid) == self.selected.as_ref()
            })
        }) else {
            return;
        };
        if self.edit_source.as_ref() != Some(&record.source)
            || !(record.source.needs_conversation_resolution()
                || lark2tapd_bridge::source_images::needs_source_images(&record.source))
        {
            return;
        }
        let source = Box::new(record.source.clone());
        let profile = self.edit_profile.clone();
        let session = self.editor_session;
        self.conversation_error = None;
        let model = self.model.clone();
        cx.defer(move |cx| {
            model.update(cx, |model, cx| {
                model.send_at(Job::ResolveConversation { source, session }, profile, cx)
            })
        });
    }

    fn conversation_images_fit(&self, source: &SourceTask, prefix: &str, cx: &App) -> bool {
        let existing = self.draft_attachments(cx);
        let used = lark2tapd_bridge::referenced_image_ids(prefix);
        let extra: BTreeSet<_> = source
            .metadata
            .source_images
            .values()
            .filter(|image| used.contains(&image.id) && !existing.iter().any(|a| a.id == image.id))
            .map(|image| &image.id)
            .collect();
        existing.len() + extra.len() + self.pending_files + self.pending_pastes.len() <= 10
    }

    fn insert_conversation(&mut self, guid: &str, window: &mut Window, cx: &mut Context<Self>) {
        // 按点击时的当前来源重建副本，拒绝迟到的旧任务点击、过期草稿及中文组合输入。
        if self.selected.as_deref() != Some(guid)
            || !self.can_edit_description(cx)
            || self.description.read(cx).is_composing()
        {
            return;
        }
        let model = self.model.read(cx);
        let Some(record) = model.snapshot.as_ref().and_then(|snapshot| {
            snapshot
                .records
                .iter()
                .find(|record| record.source.guid == guid && record.in_scope)
        }) else {
            return;
        };
        let prefix = conversation_markdown(&record.source, model.language());
        if prefix.is_empty()
            || lark2tapd_bridge::source_images::needs_source_images(&record.source)
            || !self.conversation_images_fit(&record.source, &prefix, cx)
            || self.description.read(cx).text().starts_with(&prefix)
        {
            return;
        }
        // 正文引用和获准图片池一并更新，避免异步回执间隙导致提交时遗漏源图片。
        self.edit_source = Some(record.source.clone());
        self.description.update(cx, |editor, cx| {
            editor.replace_range(0..0, &prefix, cx);
            editor.focus(window, cx);
        });
        self.load_images(window, cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::prelude::v1::test;

    #[test]
    fn inserted_markdown_keeps_order_content_and_literal_sender_names() {
        let mut source = SourceTask {
            guid: "task".into(),
            title: "Title".into(),
            description: String::new(),
            url: String::new(),
            due: None,
            completed: false,
            metadata: Default::default(),
        };
        source.metadata.source_messages = vec![
            SourceMessage {
                message_id: "om_late".into(),
                content: "# 后消息\n\n```rs\nfn main() {}\n```".into(),
            },
            SourceMessage {
                message_id: "om_early".into(),
                content: "**先消息**".into(),
            },
            SourceMessage {
                message_id: "numeric".into(),
                content: "未知时间消息".into(),
            },
        ];
        for (id, time) in [("om_late", "1735689602000"), ("om_early", "1735689600000")] {
            source.metadata.message_details.insert(
                id.into(),
                lark2tapd_core::SourceMessageDetails {
                    sender: Some(lark2tapd_core::SourcePerson {
                        id: "ou_sender".into(),
                        name: "A *B* <bot>\nline".into(),
                    }),
                    sent_at: Some(time.into()),
                },
            );
        }
        let markdown = conversation_markdown(&source, "zh-CN");
        assert!(markdown.find("**先消息**").unwrap() < markdown.find("# 后消息").unwrap());
        assert!(markdown.find("# 后消息").unwrap() < markdown.find("未知时间消息").unwrap());
        assert!(!markdown.contains("### "), "senders must not be headings");
        assert!(markdown.contains("> ```rs\n> fn main() {}\n> ```"));
        assert!(!markdown.contains("原文匹配"));
        assert!(markdown.contains(r"A \*B\* \<bot\> line"));
        assert!(!markdown.contains("ou_sender"));
        assert!(markdown.contains("> 发送人未知\n>\n"));
        assert!(!markdown.contains("时间未提供"));
        for message in &source.metadata.source_messages {
            let sender = message_header(&source, message, "zh-CN");
            assert!(
                matches!(sender.as_str(), "A *B* <bot>\nline" | "发送人未知"),
                "message headers must contain only the sender"
            );
        }
        assert!(markdown.ends_with("---\n\n"));
        source.metadata.source_messages.clear();
        assert!(conversation_markdown(&source, "zh-CN").is_empty());
    }
}

#[cfg(test)]
mod resolved_order_tests {
    use super::*;
    #[std::prelude::v1::test]
    fn resolved_message_times_determine_order() {
        let mut s = SourceTask {
            guid: "s".into(),
            title: "t".into(),
            description: String::new(),
            url: String::new(),
            due: None,
            completed: false,
            metadata: Default::default(),
        };
        s.metadata.source_messages = vec![
            SourceMessage {
                message_id: "123".into(),
                content: "Earlier".into(),
            },
            SourceMessage {
                message_id: "om_later".into(),
                content: "Later".into(),
            },
        ];
        s.metadata.matched_messages.insert(
            "123".into(),
            vec![SourceMessage {
                message_id: "om_earlier".into(),
                content: "Earlier".into(),
            }],
        );
        for (id, time) in [
            ("om_earlier", "1735689600000"),
            ("om_later", "1735689601000"),
        ] {
            s.metadata.message_details.insert(
                id.into(),
                lark2tapd_core::SourceMessageDetails {
                    sender: None,
                    sent_at: Some(time.into()),
                },
            );
        }
        assert_eq!(
            ordered_messages(&s)
                .iter()
                .map(|(_, m, _)| m.content.as_str())
                .collect::<Vec<_>>(),
            vec!["Earlier", "Later"]
        );
    }
}
