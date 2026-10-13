//! 字段帮助使用原生浮层；悬停只显示说明，不抢输入焦点、不打开浏览器。
use gpui_kit::base::{Button as BaseButton, Popup};
use gpui_kit::component::{ActiveTheme, Icon, IconName, ThemeStyled};
use gpui_kit::{prelude::*, *};

#[derive(Default)]
struct ActiveHelp(Option<WeakEntity<HelpState>>);
impl Global for ActiveHelp {}

struct HelpState {
    open: bool,
    trigger: FocusHandle,
    documentation: Option<FocusHandle>,
    // 在焦点仍留在别的输入框时也拦截 Esc；订阅随字段销毁自动释放。
    _keyboard: Subscription,
}

impl HelpState {
    fn show(entity: &Entity<Self>, cx: &mut App) {
        // 全局只保留一个帮助；弱引用不延长字段/窗口的生命周期。
        if let Some(previous) = cx
            .default_global::<ActiveHelp>()
            .0
            .replace(entity.downgrade())
            && previous.entity_id() != entity.entity_id()
        {
            let _ = previous.update(cx, |state, cx| {
                state.open = false;
                cx.notify();
            });
        }
        entity.update(cx, |state, cx| {
            state.open = true;
            cx.notify();
        });
    }
}

#[derive(IntoElement)]
struct Help {
    id: &'static str,
    help: SharedString,
    url: Option<&'static str>,
    documentation: &'static str,
}

impl RenderOnce for Help {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state =
            window.use_keyed_state((ElementId::from(self.id), "state"), cx, |window, cx| {
                let weak = cx.weak_entity();
                let owner = window.window_handle();
                let keyboard = cx.intercept_keystrokes(move |event, window, cx| {
                    if window.window_handle() != owner {
                        return;
                    }
                    let _ = weak.update(cx, |state: &mut HelpState, cx| {
                        if !state.open {
                            return;
                        }
                        // Popup 延后绘制；显式连接触发器与链接的正向 Tab 顺序。
                        if event.keystroke.key == "tab"
                            && !event.keystroke.modifiers.shift
                            && state.trigger.is_focused(window)
                            && let Some(link) = &state.documentation
                        {
                            link.focus(window, cx);
                            cx.stop_propagation();
                        }
                        if event.keystroke.key == "tab"
                            && event.keystroke.modifiers.shift
                            && state
                                .documentation
                                .as_ref()
                                .is_some_and(|link| link.is_focused(window))
                        {
                            state.trigger.focus(window, cx);
                            cx.stop_propagation();
                        }
                        if event.keystroke.key == "escape" {
                            state.open = false;
                            state.trigger.focus(window, cx);
                            cx.stop_propagation();
                            cx.notify();
                        }
                    });
                });
                HelpState {
                    open: false,
                    trigger: cx.focus_handle(),
                    documentation: self.url.map(|_| cx.focus_handle()),
                    _keyboard: keyboard,
                }
            });
        let focus = state.read(cx).trigger.clone();
        let open = state.read(cx).open;
        let link_focus = state.read(cx).documentation.clone();
        let hover = state.clone();
        let click = state.clone();
        let trigger = BaseButton::new(self.id)
            .track_focus(&focus)
            .accessibility_label(self.help.clone())
            .p_1()
            .rounded_md()
            .cursor_pointer()
            .text_color(cx.theme().muted_foreground)
            .hover(|s| s.bg(cx.theme().accent))
            .focus_visible(|s| {
                s.bg(cx.theme().accent)
                    .border_1()
                    .border_color(cx.theme().ring)
            })
            .child(Icon::new(IconName::Info).size_4())
            .on_hover(move |hovered, _, cx| {
                if *hovered {
                    HelpState::show(&hover, cx);
                }
            })
            .on_click(move |_, _, cx| HelpState::show(&click, cx));
        Popup::new((ElementId::from(self.id), "popup"), trigger)
            .offset(px(4.))
            .when(open, |popup| {
                let outside = state.clone();
                popup.content(
                    div()
                        .id("help-body")
                        .test_support()
                        .role(Role::Dialog)
                        .occlude()
                        .tab_group()
                        .popover_style(cx)
                        .w(px(320.))
                        .p_3()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .text_sm()
                        .child(self.help)
                        .when_some(self.url, |body, url| {
                            body.child(
                                BaseButton::new("help-documentation")
                                    .track_focus(
                                        link_focus.as_ref().expect("URL has a link focus handle"),
                                    )
                                    .accessibility_label(self.documentation)
                                    .text_color(cx.theme().primary)
                                    .cursor_pointer()
                                    .focus_visible(|s| s.bg(cx.theme().accent))
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(Icon::new(IconName::ExternalLink).size_4())
                                    .child(self.documentation)
                                    .on_click(move |_, _, cx| cx.open_url(url)),
                            )
                        })
                        // 不因离开 Info 而关闭，允许穿过浮层间隙并阅读、点击链接。
                        .on_mouse_down_out(move |_, _, cx| {
                            outside.update(cx, |state, cx| {
                                state.open = false;
                                cx.notify();
                            });
                        }),
                )
            })
    }
}

pub(super) fn field(
    label: &str,
    id: &'static str,
    help: &str,
    url: Option<&'static str>,
    input: impl IntoElement,
    language: &str,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_sm()
                .child(label.to_owned())
                .child(Help {
                    id,
                    help: help.to_owned().into(),
                    url,
                    documentation: if language == "en" {
                        "Open documentation"
                    } else {
                        "打开文档"
                    },
                }),
        )
        .child(input)
}

#[cfg(test)]
mod tests {
    use super::field;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{Context, IntoElement, Render, TestAppContext, Window, div, prelude::*};

    #[derive(Default)]
    struct Harness {
        escaped: bool,
    }
    impl Render for Harness {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .p_8()
                .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, _| {
                    if event.keystroke.key == "escape" {
                        this.escaped = true;
                    }
                }))
                .child(field(
                    "Token",
                    "help-test",
                    "Keep this token private",
                    Some("https://example.com/docs"),
                    gpui_kit::base::Button::new("other-input").child("Input"),
                    "en",
                ))
                .child(div().mt_40().child(field(
                    "Prefix",
                    "help-second",
                    "Saved verbatim",
                    None,
                    div().child("Second input"),
                    "zh-CN",
                )))
        }
    }

    // 若 Info 仍直接打开 URL，或鼠标进入帮助内容时关闭，测试必须失败。
    #[gpui_kit::test]
    fn hover_then_link_is_the_only_external_navigation(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (_, window) = cx.add_window_view(|_, _| Harness::default());
        window.update(|window, cx| {
            window.render_frame(cx);
            window.hover("help-test", cx);
            assert!(
                window.try_find("help-body").is_some(),
                "hover must show help"
            );
            window.hover("help-documentation", cx);
            assert!(window.try_find("help-body").is_some());
        });
        assert_eq!(window.opened_url(), None);
        window.update(|window, cx| window.click("help-documentation", cx));
        assert_eq!(
            window.opened_url().as_deref(),
            Some("https://example.com/docs")
        );
    }

    #[gpui_kit::test]
    fn click_and_keyboard_open_help_and_escape_dismisses(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (_, window) = cx.add_window_view(|_, _| Harness::default());
        window.update(|window, cx| {
            window.render_frame(cx);
            window.click("help-test", cx);
            assert!(
                window.try_find("help-body").is_some(),
                "Info opens help, not URL"
            );
            window.press("escape", cx);
            assert!(window.try_find("help-body").is_none());
            for key in ["enter", "space"] {
                window.press(key, cx);
                assert!(window.try_find("help-body").is_some(), "{key} opens help");
                window.press("escape", cx);
                assert!(window.try_find("help-body").is_none());
            }
        });
        assert_eq!(window.opened_url(), None);
    }
    // 悬停不抢焦点；第二项替换第一项；Esc 不传递到设置页。
    #[gpui_kit::test]
    fn hover_preserves_focus_and_replaces_previous_help(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (view, window) = cx.add_window_view(|_, _| Harness::default());
        window.update(|window, cx| {
            window.render_frame(cx);
            window.click("other-input", cx);
            let focused = window.focused(cx).unwrap();
            window.hover("help-test", cx);
            assert!(focused.is_focused(window));
            window.hover("help-second", cx);
            assert!(focused.is_focused(window));
            assert!(
                window.try_find("help-documentation").is_none(),
                "URL-less help replaces first popup"
            );
            assert!(window.try_find("help-body").is_some());
            window.press("escape", cx);
            assert!(window.try_find("help-body").is_none());
            assert!(!view.read(cx).escaped, "Escape must not reach settings");
            assert_eq!(window.find("help-second").focused(), Some(true));
        });
        assert_eq!(window.opened_url(), None);
    }

    #[gpui_kit::test]
    fn keyboard_can_reach_link_and_outside_click_dismisses(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (_, window) = cx.add_window_view(|_, _| Harness::default());
        window.update(|window, cx| {
            window.render_frame(cx);
            window.click("help-test", cx);
            window.press("tab", cx);
            assert_eq!(window.find("help-documentation").focused(), Some(true));
            window.press("shift-tab", cx);
            assert_eq!(window.find("help-test").focused(), Some(true));
            window.press("escape", cx);
            assert_eq!(window.find("help-test").focused(), Some(true));
            window.click("help-test", cx);
            window.click("other-input", cx);
            assert!(window.try_find("help-body").is_none());
        });
    }
}
