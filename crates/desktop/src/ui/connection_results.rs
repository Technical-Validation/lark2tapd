//! 每个服务复用一个只读原生结果窗口；关闭窗口不取消后台验证。
use super::*;

#[derive(Default)]
struct ResultWindows([Option<AnyWindowHandle>; 2]);
impl Global for ResultWindows {}
fn index(tool: CliTool) -> usize {
    if tool == CliTool::Lark { 0 } else { 1 }
}

pub(super) fn show(tool: CliTool, cx: &mut App) {
    let Some(shell) = cx.try_global::<Shell>() else {
        return;
    };
    let model = shell.model.clone();
    let current = cx.default_global::<ResultWindows>().0[index(tool)];
    if let Some(handle) = current
        && handle
            .update(cx, |_, w, cx| {
                w.activate_window();
                cx.activate(true);
            })
            .is_ok()
    {
        return;
    }
    let title = title(tool, model.read(cx).language());
    match gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(700.), px(620.)),
                cx,
            ))),
            window_min_size: Some(size(px(500.), px(400.))),
            titlebar: Some(TitlebarOptions {
                title: Some(title.into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        cx,
        |window, cx| {
            cx.new(|cx| {
                let focus = cx.focus_handle();
                focus.focus(window, cx);
                cx.observe(&model, |_, _, cx| cx.notify()).detach();
                Results {
                    model: model.clone(),
                    tool,
                    focus,
                }
            })
        },
    ) {
        Ok((handle, _)) => {
            cx.global_mut::<ResultWindows>().0[index(tool)] = Some(handle);
            cx.activate(true);
        }
        Err(e) => model.update(cx, |m, cx| m.receive(Event::Error(e.to_string()), cx)),
    }
}
fn title(tool: CliTool, language: &str) -> &'static str {
    if tool == CliTool::Lark {
        text(language, "飞书连接测试", "Lark connection test")
    } else {
        text(language, "TAPD 连接测试", "TAPD connection test")
    }
}
struct Results {
    model: Entity<Model>,
    tool: CliTool,
    focus: FocusHandle,
}
impl Render for Results {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.model.read(cx);
        let language = model.language();
        let state = model.connection(self.tool);
        let running = model.activities.contains(connections::field_key(self.tool));
        let error = model.errors.get(connections::field_key(self.tool));
        window.set_window_title(title(self.tool, language));
        div().size_full().bg(cx.theme().background).text_color(cx.theme().foreground)
            .font_family(cx.theme().font_family.clone()).text_sm().track_focus(&self.focus)
            .key_context("ConnectionResult")
            .on_action(|_:&CloseWindow,window,_|window.remove_window())
            .flex().flex_col()
            .child(div().p_5().text_xl().font_semibold().child(title(self.tool,language)))
            .child(div().id("connection-result-scroll").flex_1().min_h_0().overflow_y_scroll().p_5().flex().flex_col().gap_3()
                .child(div().font_semibold().child(if running {text(language,"正在验证…","Checking…")} else if error.is_some() || state.report.as_ref().is_some_and(|r| !r.connected) {
                    text(language,"本次验证未通过","This check did not pass")
                } else if state.report.as_ref().is_some_and(|r| r.connected) {text(language,"本次验证通过","This check passed")} else {text(language,"等待测试结果","Waiting for results")}))
                .when_some(state.tested_at.as_deref().and_then(connections::local_timestamp),|d,t| d.child(format!("{}: {t}",text(language,"报告时间","Report time"))))
                .when_some(connections::last_verified(model,self.tool),|d,t| d.child(format!("{}: {t}",text(language,"已保存配置最近通过","Saved configuration last verified"))))
                .when(state.report.is_some() && !state.tested_saved_configuration,|d| d.child(text(language,"此结果未更新当前已保存连接的通过记录。","This result does not update the verification history of the currently saved connection.")))
                .when_some(error.cloned(),|d,e| d.child(div().text_color(cx.theme().danger).child(e)))
                .when(self.tool==CliTool::Lark,|d| d.when_some(model.lark_authorization.clone(),|d,(url,_)| d.child(Button::new("result-open-authorization").label(text(language,"打开飞书授权页面","Open Lark authorization")).on_click(move|_,_,cx|open_safe_url(&url,cx)))))
                .child(connections::permission_summary(model,self.tool,cx)))
            .child(div().p_4().child(Button::new("close-connection-result").label(text(language,"关闭","Close")).on_click(|_,w,_|w.remove_window())))
    }
}

#[cfg(test)]
pub(super) fn handle(tool: CliTool, cx: &App) -> Option<AnyWindowHandle> {
    cx.try_global::<ResultWindows>()
        .and_then(|w| w.0[index(tool)])
}
