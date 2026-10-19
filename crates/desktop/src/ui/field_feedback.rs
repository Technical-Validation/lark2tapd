//! 保存结果按请求序号确认；旧请求不能把正在编辑的新值标记为已保存。
use super::*;
use gpui_kit::component::{Icon, IconName};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SaveState {
    Dirty,
    Saving(u64),
    Saved,
}
impl Model {
    pub(super) fn dirty_field(&mut self, field: &'static str) {
        self.save_states.insert(field, SaveState::Dirty);
        self.errors.remove(field);
    }
}
impl Workspace {
    pub(super) fn field_feedback(&self, key: &'static str, cx: &Context<Self>) -> AnyElement {
        let model = self.model.read(cx);
        if let Some(error) = model.errors.get(key).cloned() {
            let category = match crate::console::category(&error) {
                "storage" => self.tr(cx, "存储", "Storage"),
                "permission" => self.tr(cx, "权限", "Permission"),
                "authentication" => self.tr(cx, "认证", "Authentication"),
                "network" => self.tr(cx, "网络", "Network"),
                "api" => self.tr(cx, "接口", "API"),
                "rate_limit" => self.tr(cx, "请求过于频繁", "Rate limited"),
                "timeout" => self.tr(cx, "超时", "Timeout"),
                "validation" => self.tr(cx, "配置", "Configuration"),
                "cancelled" => self.tr(cx, "取消", "Cancelled"),
                _ => self.tr(cx, "未知", "Unknown"),
            };
            return Collapsible::new()
                .w_full()
                .open(self.expanded_errors.contains(key))
                .child(
                    self.focus_region(
                        SharedString::from(format!("toggle-error-{key}")),
                        Button::new(SharedString::from(format!("toggle-error-{key}")))
                            .ghost()
                            .w_full()
                            .icon(IconName::ChevronDown)
                            .text_color(cx.theme().danger)
                            .label(format!(
                                "{} · {category}",
                                self.tr(cx, "操作未完成", "Operation failed")
                            ))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.expanded_errors.remove(key) {
                                    this.expanded_errors.insert(key.into());
                                }
                                cx.notify();
                            })),
                    ),
                )
                .content(
                    div()
                        .debug_selector(move || format!("error-detail-{key}"))
                        .p_3()
                        .rounded_md()
                        .bg(cx.theme().muted)
                        .text_sm()
                        .child(error),
                )
                .into_any_element();
        }
        match model.save_states.get(key) {
            Some(SaveState::Saved) => div()
                .debug_selector(move || format!("saved-{key}"))
                .flex()
                .items_center()
                .gap_1()
                .text_sm()
                .text_color(cx.theme().success)
                .child(Icon::new(IconName::Check).size_4())
                .child(self.tr(cx, "已保存", "Saved"))
                .into_any_element(),
            Some(SaveState::Saving(_)) => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(self.tr(cx, "保存中…", "Saving…"))
                .into_any_element(),
            _ => div().into_any_element(),
        }
    }
}
