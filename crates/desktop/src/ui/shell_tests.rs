//! 独立设置窗口必须复用同一模型，且不得切换或清空主窗口草稿。
use super::*;

#[gpui_kit::test]
fn menu_and_shortcut_reuse_settings_and_reopen_after_close(cx: &mut TestAppContext) {
    let (_dir, events, model, main, _) = setup(cx);
    cx.update_window(main, |_, w, cx| {
        w.render_frame(cx);
        w.press("cmd-,", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let first = cx.read(|cx| cx.global::<Shell>().settings_window.unwrap());
    assert_eq!(cx.windows().len(), 2);
    cx.update(|cx| handle_native_action(NativeAction::Settings, cx));
    assert_eq!(
        cx.read(|cx| cx.global::<Shell>().settings_window.unwrap()),
        first
    );
    let weak = cx
        .update_window(first, |root, _, cx| {
            root.downcast::<gpui_kit::base::Root>()
                .unwrap()
                .read(cx)
                .view()
                .clone()
                .downcast::<Workspace>()
                .unwrap()
                .downgrade()
        })
        .unwrap();
    // 默认窗口尺寸下的真实控件交互，验证与主窗口共享偏好而非独立设置副本。
    click(cx, first, "en");
    pump(&events, &model, cx, |cx| model.read(cx).language() == "en");
    close_test_window(cx, first);
    assert!(
        weak.upgrade().is_none(),
        "closing must release window-bound credential inputs"
    );
    close_test_window(cx, main);
    cx.update(|cx| handle_native_action(NativeAction::Settings, cx));
    assert_eq!(
        cx.windows().len(),
        1,
        "settings must work without reopening main"
    );
    let reopened = cx.read(|cx| cx.global::<Shell>().settings_window.unwrap());
    assert_ne!(first, reopened);
    cx.update_window(reopened, |root, _, cx| {
        let view = root
            .downcast::<gpui_kit::base::Root>()
            .unwrap()
            .read(cx)
            .view()
            .clone()
            .downcast::<Workspace>()
            .unwrap();
        assert!(view.read(cx).is_settings);
        assert_eq!(view.read(cx).model, model);
    })
    .unwrap();
}

#[gpui_kit::test]
fn background_errors_are_retained_until_a_window_can_show_them(cx: &mut TestAppContext) {
    let (_dir, _events, model, main, _) = setup(cx);
    cx.update_window(main, |_, w, _| w.activate_window())
        .unwrap();
    cx.run_until_parked();
    VisualTestContext::from_window(main, cx).deactivate_window();
    cx.run_until_parked();
    cx.update(|cx| {
        model.update(cx, |m, cx| {
            m.receive(Event::Error("background failure".into()), cx)
        })
    });
    cx.run_until_parked();
    cx.update_window(main, |_, w, cx| assert_eq!(w.notifications(cx).len(), 1))
        .unwrap();
    let snapshot = cx.read(|cx| model.read(cx).snapshot.clone().unwrap());
    cx.update(|cx| {
        model.update(cx, |m, cx| {
            m.receive(
                Event::State {
                    mode: Mode::Mock,
                    snapshot,
                    profile: "mock".into(),
                    message: "settings_saved",
                    new_count: 0,
                },
                cx,
            )
        })
    });
    cx.run_until_parked();
    cx.update_window(main, |_, w, cx| {
        assert_eq!(
            w.notifications(cx).len(),
            2,
            "an unrelated success must not replace an error awaiting dismissal"
        )
    })
    .unwrap();
    close_test_window(cx, main);
    cx.update(|cx| {
        model.update(cx, |m, cx| {
            m.receive(Event::Error("closed-window failure".into()), cx)
        })
    });
    cx.run_until_parked();
    cx.update(|cx| handle_native_action(NativeAction::Settings, cx));
    cx.run_until_parked();
    let settings = cx.read(|cx| cx.global::<Shell>().settings_window.unwrap());
    cx.update_window(settings, |_, w, cx| {
        assert_eq!(w.notifications(cx).len(), 1);
        let note = w.notifications(cx)[0].clone();
        note.update(cx, |note, cx| note.dismiss(w, cx));
    })
    .unwrap();
    cx.run_until_parked();
    cx.background_executor.advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    cx.read(|cx| {
        assert!(
            model.read(cx).pending_error.is_none(),
            "explicit dismissal must acknowledge the retained error"
        )
    });
    close_test_window(cx, settings);
    cx.update(|cx| handle_native_action(NativeAction::Settings, cx));
    cx.run_until_parked();
    let reopened = cx.read(|cx| cx.global::<Shell>().settings_window.unwrap());
    cx.update_window(reopened, |_, w, cx| assert!(w.notifications(cx).is_empty()))
        .unwrap();
}

// 使用滚轮而非放大窗口，覆盖默认设置窗口底部控件的可达性。
fn reveal_setting(cx: &mut TestAppContext, handle: AnyWindowHandle, id: &str) {
    cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        let target = w.find(SharedString::from(id.to_owned())).bounds();
        let viewport = w.viewport_size();
        let position = point(viewport.width / 2., viewport.height / 2.);
        w.dispatch_event(
            ScrollWheelEvent {
                position,
                delta: ScrollDelta::Pixels(point(px(0.), position.y - target.center().y)),
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn default_settings_window_can_scroll_to_prefix_and_poll_controls(cx: &mut TestAppContext) {
    let (_dir, events, model, main, view) = setup(cx);
    load_task(&events, &model, &view, main, cx);
    click(cx, main, "nav-settings");
    let settings = cx.read(|cx| cx.global::<Shell>().settings_window.unwrap());
    reveal_setting(cx, settings, "add-prefix");
    click(cx, settings, "add-prefix");
    reveal_setting(cx, settings, "workspace-prefixes");
    cx.update_window(settings, |_, w, cx| {
        w.render_frame(cx);
        w.click("workspace-prefixes", cx);
        w.input("[Small] ", cx);
    })
    .unwrap();
    reveal_setting(cx, settings, "confirm-prefixes");
    click(cx, settings, "confirm-prefixes");
    pump(&events, &model, cx, |cx| {
        model
            .read(cx)
            .snapshot
            .as_ref()
            .unwrap()
            .settings
            .workspace_prefixes
            .get("demo")
            .is_some_and(|p| p.contains(&"[Small] ".into()))
    });
    reveal_setting(cx, settings, "toggle-poll");
    click(cx, settings, "toggle-poll");
    pump(&events, &model, cx, |cx| {
        model
            .read(cx)
            .snapshot
            .as_ref()
            .unwrap()
            .settings
            .poll_enabled
    });
    reveal_setting(cx, settings, "poll-minutes");
    cx.update_window(settings, |_, w, cx| {
        w.render_frame(cx);
        w.click("poll-minutes", cx);
        w.press("cmd-a", cx);
        w.input("30", cx);
    })
    .unwrap();
    pump(&events, &model, cx, |cx| {
        model
            .read(cx)
            .snapshot
            .as_ref()
            .unwrap()
            .settings
            .poll_minutes
            == 30
    });
}

#[gpui_kit::test]
fn settings_opened_before_cache_load_initializes_fields_once(cx: &mut TestAppContext) {
    let (_dir, _events, model, main, _) = setup(cx);
    let mut snapshot = cx.read(|cx| model.read(cx).snapshot.clone().unwrap());
    snapshot.settings.poll_minutes = 42;
    snapshot.settings.lark_path = "/cached/lark-cli".into();
    model.update(cx, |m, cx| {
        m.snapshot = None;
        cx.notify();
    });
    let (_, settings) = open_settings_for_test(cx, main);
    cx.update(|cx| {
        model.update(cx, |m, cx| {
            m.receive(
                Event::State {
                    mode: Mode::Mock,
                    snapshot,
                    profile: "mock".into(),
                    message: "ready",
                    new_count: 0,
                },
                cx,
            )
        })
    });
    cx.run_until_parked();
    cx.read(|cx| {
        assert_eq!(settings.read(cx).poll.read(cx).value().as_ref(), "42");
        assert_eq!(
            settings.read(cx).lark_path.read(cx).value().as_ref(),
            "/cached/lark-cli"
        );
    });
}

#[gpui_kit::test]
fn global_errors_are_toasts_while_background_reads_stay_quiet(cx: &mut TestAppContext) {
    let (_dir, _events, model, main, _) = setup(cx);
    cx.update_window(main, |_, w, cx| {
        w.activate_window();
        w.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(main, |_, w, _| assert!(w.is_window_active()))
        .unwrap();
    cx.update(|cx| {
        model.update(cx, |m, cx| {
            m.receive(Event::Error("test failure".into()), cx)
        })
    });
    cx.run_until_parked();
    cx.update_window(main, |_, w, cx| assert_eq!(w.notifications(cx).len(), 1))
        .unwrap();
    let snapshot = cx.read(|cx| model.read(cx).snapshot.clone().unwrap());
    cx.update(|cx| {
        model.update(cx, |m, cx| {
            m.receive(
                Event::State {
                    mode: Mode::Mock,
                    snapshot,
                    profile: "mock".into(),
                    message: "cached",
                    new_count: 0,
                },
                cx,
            )
        })
    });
    cx.run_until_parked();
    cx.update_window(main, |_, w, cx| assert_eq!(w.notifications(cx).len(), 1))
        .unwrap();
}

#[gpui_kit::test]
fn settings_opens_a_single_native_window_without_navigating_main(cx: &mut TestAppContext) {
    let (_dir, events, model, main, view) = setup(cx);
    load_task(&events, &model, &view, main, cx);
    let before = cx.read(|cx| view.read(cx).draft(cx));
    let count = cx.windows().len();
    click(cx, main, "nav-settings");
    assert_eq!(
        cx.windows().len(),
        count + 1,
        "settings must open a native window"
    );
    click(cx, main, "nav-settings");
    assert_eq!(
        cx.windows().len(),
        count + 1,
        "repeated entry must reuse the window"
    );
    cx.read(|cx| {
        assert!(view.read(cx).filter == Filter::Pending);
        assert_eq!(view.read(cx).draft(cx), before);
    });
}
