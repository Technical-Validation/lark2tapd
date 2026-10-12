//! 原生 Markdown 宿主：正文只存于编辑器，媒体与账户权限仍由 Workspace 管理。
use super::*;
use zorite_editor::{ChangeOrigin, EditorEvent, Labels, SyntaxStyle};

pub(super) enum DeferredMedia {
    Ready(MediaReady),
    Failed(MediaFailure),
}

pub(super) fn create_description(
    scroll: ScrollHandle,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Entity<EditorState> {
    zorite_editor::bind_keys(cx);
    let editor = cx.new(|cx| EditorState::new(window, cx));
    let image_scroll = scroll.clone();
    editor.update(cx, |editor, _| {
        editor.set_scroll_compensator(move |delta, _, _| {
            let offset = image_scroll.offset();
            image_scroll.set_offset(point(offset.x, offset.y - delta));
        })
    });
    cx.subscribe_in(&editor, window, move |this, editor, event, window, cx| {
        // 事件在编辑器借用释放后派发；不可从编辑器同步回调再次修改同一实体。
        if let EditorEvent::PasteMedia(item) = event {
            this.paste_image(item, window, cx);
        } else if matches!(
            event,
            EditorEvent::Changed {
                origin: ChangeOrigin::User
            } | EditorEvent::SelectionChanged
        ) {
            // 新布局完成后仅为主动编辑滚动，图片刷新通知不把阅读位置拉回光标。
            let editor = editor.clone();
            let scroll = scroll.clone();
            window.on_next_frame(move |window, cx| {
                if !editor.read(cx).focus_handle(cx).is_focused(window) {
                    return;
                }
                let viewport = scroll.bounds();
                let Some(caret) = editor.read(cx).caret_screen_bounds() else {
                    return;
                };
                if viewport.size.height <= px(0.) {
                    return;
                }
                let delta = if caret.top() < viewport.top() + px(12.) {
                    viewport.top() + px(12.) - caret.top()
                } else if caret.bottom() > viewport.bottom() - px(12.) {
                    viewport.bottom() - px(12.) - caret.bottom()
                } else {
                    px(0.)
                };
                if delta != px(0.) {
                    let offset = scroll.offset();
                    scroll.set_offset(point(offset.x, offset.y + delta));
                    window.refresh();
                }
            });
        }
    })
    .detach();
    cx.observe_in(&editor, window, |this, _, window, cx| {
        this.flush_media(window, cx);
        cx.notify();
    })
    .detach();
    editor
}

impl Workspace {
    pub(super) fn can_edit_description(&self, cx: &App) -> bool {
        let model = self.model.read(cx);
        self.filter != Filter::Settings
            && !model.busy
            && self.edit_profile == model.profile
            && self.edit_source.as_ref().is_some_and(|source| {
                model.snapshot.as_ref().is_some_and(|snapshot| {
                    snapshot.records.iter().any(|record| {
                        record.in_scope && !record.uncertain && &record.source == source
                    })
                })
            })
    }

    pub(super) fn sync_description(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_edit_description(cx) {
            let focused = self
                .description
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx);
            if focused {
                window.blur(cx);
                self.description
                    .update(cx, |editor, cx| editor.unmark_text(window, cx));
            }
        }
        let key = (
            cx.theme().mode == ThemeMode::Dark,
            self.model.read(cx).language().to_owned(),
        );
        if self.editor_appearance.as_ref() != Some(&key) {
            let style = markdown_style(cx);
            let labels = labels(&key.1);
            self.editor_appearance = Some(key);
            self.description.update(cx, |editor, cx| {
                editor.set_markdown_style(style, cx);
                editor.set_labels(labels, cx);
            });
        }
    }

    pub(super) fn description_editor(&self, disabled: bool, cx: &App) -> AnyElement {
        // 移除可编辑视图而不是仅遮罩：已有焦点、右键菜单和输入法不能绕过禁用态。
        let content = if disabled {
            self.markdown(
                "description-readonly",
                self.description.read(cx).text().to_owned(),
            )
            .into_any_element()
        } else {
            self.description.clone().into_any_element()
        };
        div()
            .id("draft-description")
            .w_full()
            .min_h(px(180.))
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .bg(if disabled {
                cx.theme().muted
            } else {
                cx.theme().background
            })
            .text_color(cx.theme().foreground)
            .child(content)
            .into_any_element()
    }

    pub(super) fn flush_media(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.description.read(cx).is_composing() || self.deferred_media.is_empty() {
            return;
        }
        // 先取走队列再改正文，后续通知不会再次消费相同结果；会话校验仍在原处理器中。
        for result in std::mem::take(&mut self.deferred_media) {
            match result {
                DeferredMedia::Ready(ready) => self.accept_attachment(&ready, window, cx),
                DeferredMedia::Failed(failure) => self.reject_attachment(&failure, window, cx),
            }
        }
    }

    pub(super) fn refresh_editor_images(&self, cx: &mut App) {
        let images = self.images.clone();
        self.description.update(cx, |editor, cx| {
            editor.set_block_image_provider(move |source| images.get(source).cloned());
            cx.notify();
        });
    }
}

fn markdown_style(cx: &App) -> SyntaxStyle {
    let theme = cx.theme();
    SyntaxStyle {
        block_label: None,
        block_label_gen: 0,
        block_ref_count: None,
        marker: theme.muted_foreground,
        code: theme.foreground,
        code_bg: theme.muted,
        link: theme.foreground,
        tag: theme.foreground,
        quote: theme.muted_foreground,
        alert_note: theme.primary,
        alert_tip: theme.primary,
        alert_important: theme.primary,
        alert_warning: theme.danger,
        alert_caution: theme.danger,
        alert_icons: None,
        rule: theme.border,
        mark_bg: theme.accent,
        popover_bg: theme.background,
        popover_border: theme.border,
        popover_fg: theme.foreground,
        popover_hover: theme.accent,
        popover_divider: theme.border,
        popover_danger: theme.danger,
        mono: font("Menlo"),
        property_icon: None,
    }
}

fn labels(language: &str) -> Labels {
    if language != "zh-CN" {
        return Labels::default();
    }
    Labels {
        cut: "剪切".into(),
        copy: "复制".into(),
        copy_as_markdown: "复制 Markdown".into(),
        paste: "粘贴".into(),
        code_copy: "复制".into(),
        math_copy: "复制".into(),
        turn_into: "转换为".into(),
        text: "正文".into(),
        heading_1: "一级标题".into(),
        heading_2: "二级标题".into(),
        heading_3: "三级标题".into(),
        bulleted_list: "无序列表".into(),
        numbered_list: "有序列表".into(),
        todo: "待办".into(),
        quote: "引用".into(),
        callout: "提示".into(),
        code_block: "代码块".into(),
        math_block: "公式块".into(),
        insert_row_above: "在上方插入行".into(),
        insert_row_below: "在下方插入行".into(),
        duplicate_row: "复制行".into(),
        insert_column_left: "在左侧插入列".into(),
        insert_column_right: "在右侧插入列".into(),
        align_left: "左对齐".into(),
        align_center: "居中".into(),
        align_right: "右对齐".into(),
        grid_style: "网格".into(),
        striped_style: "条纹".into(),
        header_style: "表头".into(),
        minimal_style: "简洁".into(),
        delete_row: "删除行".into(),
        delete_column: "删除列".into(),
        delete_table: "删除表格".into(),
        edit_properties: "编辑属性".into(),
        delete_property: "删除属性".into(),
        delete_image: "删除图片".into(),
    }
}
