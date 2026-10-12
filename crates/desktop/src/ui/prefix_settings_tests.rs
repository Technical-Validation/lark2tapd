use super::*;

fn wait_for_prefix_saves(
    events: &async_channel::Receiver<Event>,
    model: &Entity<Model>,
    count: usize,
    cx: &mut TestAppContext,
) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut saved = 0;
    while saved < count {
        let event = runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), events.recv())
                .await
                .unwrap()
                .unwrap()
        });
        if matches!(
            &event,
            Event::State {
                message: "settings_saved",
                ..
            }
        ) {
            saved += 1;
        }
        if let Event::FormError { message, .. } = &event {
            panic!("{message}");
        }
        cx.update(|cx| model.update(cx, |m, cx| m.receive(event, cx)));
        cx.run_until_parked();
    }
}

// 不处理 actor 回传事件就连续点击，模拟用户仍看到旧列表时的快速操作。
#[gpui_kit::test]
fn rapid_prefix_deletes_do_not_resurrect_removed_values(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    load_task(&events, &model, &view, handle, cx);
    for prefix in [" A ", "[B]", "C"] {
        model.update(cx, |m, cx| {
            m.send(
                Job::SetPreference(Preference::AddWorkspacePrefix {
                    workspace: "demo".into(),
                    prefix: prefix.into(),
                }),
                cx,
            )
        });
    }
    wait_for_prefix_saves(&events, &model, 3, cx);
    click(cx, handle, "nav-settings");
    cx.update_window(handle, |_, w, cx| {
        w.scroll(
            "lark-cli-help",
            ScrollDelta::Pixels(point(px(0.), px(-1100.))),
            cx,
        );
    })
    .unwrap();
    click(cx, handle, "remove-prefix-0");
    click(cx, handle, "remove-prefix-1");
    wait_for_prefix_saves(&events, &model, 2, cx);
    assert_eq!(
        cx.read(|cx| model
            .read(cx)
            .snapshot
            .as_ref()
            .unwrap()
            .settings
            .workspace_prefixes["demo"]
            .clone()),
        vec!["C"]
    );
}

#[gpui_kit::test]
fn rapid_prefix_adds_preserve_both_verbatim_values(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    load_task(&events, &model, &view, handle, cx);
    click(cx, handle, "nav-settings");
    cx.update_window(handle, |_, w, cx| {
        w.scroll(
            "lark-cli-help",
            ScrollDelta::Pixels(point(px(0.), px(-1100.))),
            cx,
        );
    })
    .unwrap();
    click(cx, handle, "add-prefix");
    for value in [" A ", "[B]"] {
        cx.update_window(handle, |_, w, cx| {
            w.click("workspace-prefixes", cx);
            w.press("cmd-a", cx);
            w.input(value, cx);
        })
        .unwrap();
        click(cx, handle, "confirm-prefixes");
    }
    wait_for_prefix_saves(&events, &model, 2, cx);
    assert_eq!(
        cx.read(|cx| model
            .read(cx)
            .snapshot
            .as_ref()
            .unwrap()
            .settings
            .workspace_prefixes["demo"]
            .clone()),
        vec![" A ", "[B]"]
    );
}

#[gpui_kit::test]
fn prefixes_add_one_verbatim_value_only_after_confirmation(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    load_task(&events, &model, &view, handle, cx);
    click(cx, handle, "nav-settings");
    cx.update_window(handle, |_, w, cx| {
        w.scroll(
            "lark-cli-help",
            ScrollDelta::Pixels(point(px(0.), px(-1100.))),
            cx,
        );
    })
    .unwrap();
    assert_eq!(
        cx.read(|cx| selected(&view.read(cx).prefix_workspace, cx)),
        "demo"
    );
    cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("workspace-prefixes").is_none());
    })
    .unwrap();
    click(cx, handle, "add-prefix");
    cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        w.click("workspace-prefixes", cx);
        w.input("  [原样] ", cx);
    })
    .unwrap();
    click(cx, handle, "cancel-prefix");
    assert!(cx.read(|cx| {
        model
            .read(cx)
            .snapshot
            .as_ref()
            .unwrap()
            .settings
            .workspace_prefixes
            .is_empty()
    }));
    click(cx, handle, "add-prefix");
    cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        w.click("workspace-prefixes", cx);
        w.input("  [原样] ", cx);
    })
    .unwrap();
    click(cx, handle, "confirm-prefixes");
    pump(&events, &model, cx, |cx| {
        model
            .read(cx)
            .snapshot
            .as_ref()
            .unwrap()
            .settings
            .workspace_prefixes
            .get("demo")
            .is_some_and(|v| v == &["  [原样] "])
    });
    cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("workspace-prefixes").is_none());
        assert!(w.try_find("remove-prefix-0").is_some());
    })
    .unwrap();
    click(cx, handle, "add-prefix");
    cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        w.click("workspace-prefixes", cx);
        w.input("  [原样] ", cx);
    })
    .unwrap();
    click(cx, handle, "confirm-prefixes");
    pump(&events, &model, cx, |cx| {
        model.read(cx).errors.contains_key("prefixes")
    });
    assert_eq!(
        cx.read(|cx| view.read(cx).prefix_options.read(cx).value().to_string()),
        "  [原样] "
    );
    assert!(cx.read(|cx| view.read(cx).new_prefix_open));
    click(cx, handle, "cancel-prefix");
    click(cx, handle, "remove-prefix-0");
    pump(&events, &model, cx, |cx| {
        model
            .read(cx)
            .snapshot
            .as_ref()
            .unwrap()
            .settings
            .workspace_prefixes
            .get("demo")
            .is_some_and(Vec::is_empty)
    });
}

#[gpui_kit::test]
fn detected_cli_paths_ignore_mouse_input_and_endpoint_is_absent(cx: &mut TestAppContext) {
    let (_dir, _events, _model, handle, view) = setup(cx);
    click(cx, handle, "nav-settings");
    let old = cx.read(|cx| view.read(cx).lark_path.read(cx).value());
    cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("tapd-endpoint").is_none());
        w.click("lark-path", cx);
        w.press("cmd-a", cx);
        w.input("should-not-change", cx);
        assert!(
            !view
                .read(cx)
                .lark_path
                .read(cx)
                .focus_handle(cx)
                .is_focused(w)
        );
    })
    .unwrap();
    assert_eq!(cx.read(|cx| view.read(cx).lark_path.read(cx).value()), old);
}
