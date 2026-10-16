//! 未配置提示直接定位原生设置中的对应服务，而不是只打开设置首页。
use super::*;

pub(super) fn open(tool: CliTool, cx: &mut App) {
    show_settings(cx);
    let Some(handle) = cx.try_global::<Shell>().and_then(|s| s.settings_window) else {
        return;
    };
    let _ = handle.update(cx, |root, window, cx| {
        let Ok(root) = root.downcast::<gpui_kit::base::Root>() else {
            return;
        };
        let Ok(view) = root.read(cx).view().clone().downcast::<Workspace>() else {
            return;
        };
        view.update(cx, |view, cx| view.navigate_connection(tool, window, cx));
    });
}
impl Workspace {
    pub(super) fn navigate_connection(
        &mut self,
        tool: CliTool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.settings_loaded {
            self.settings_target = Some(tool);
            return;
        }
        self.settings_target = None;
        let index = if tool == CliTool::Lark { 0 } else { 1 };
        if self.model.read(cx).connection(tool).cli_version.is_some() {
            self.open_connection_form(tool, window, cx);
        } else {
            self.cli_focus[index].focus(window, cx);
        }
        let bounds = self.connection_bounds[index].clone();
        let scroll = self.settings_scroll.clone();
        // 等布局更新后按当前偏移调整，不假设设置窗口原本停在顶部。
        window.on_next_frame(move |window, _| {
            if let Some(target) = bounds.get() {
                scroll.set_offset(
                    scroll.offset() + point(px(0.), scroll.bounds().top() - target.top()),
                );
                window.refresh();
            }
        });
        cx.notify();
    }
}

pub(super) fn hint(model: &Entity<Model>, cx: &App) -> AnyElement {
    let state = model.read(cx);
    let lark =
        state.connections_loaded && !state.lark_connection.saved && !state.lark_connection.verified;
    let tapd =
        state.connections_loaded && !state.tapd_connection.saved && !state.tapd_connection.verified;
    if !lark && !tapd {
        return div().into_any_element();
    }
    div()
        .px_5()
        .py_2()
        .flex()
        .items_center()
        .gap_2()
        .bg(cx.theme().muted)
        .child(text(
            state.language(),
            "连接尚未配置完成：",
            "Finish connection setup:",
        ))
        .when(lark, |d| {
            d.child(link(CliTool::Lark, "setup-lark", state.language(), cx))
        })
        .when(tapd, |d| {
            d.child(link(CliTool::Tapd, "setup-tapd", state.language(), cx))
        })
        .into_any_element()
}
pub(super) fn link(tool: CliTool, id: &'static str, language: &str, cx: &App) -> impl IntoElement {
    Button::new(id)
        .ghost()
        .text_color(cx.theme().primary)
        .label(if tool == CliTool::Lark {
            text(language, "配置飞书", "Configure Lark")
        } else {
            text(language, "配置 TAPD", "Configure TAPD")
        })
        .on_click(move |_, _, cx| open(tool, cx))
}
