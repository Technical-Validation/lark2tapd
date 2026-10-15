//! 设置连接流程使用真实 GPUI 控件；绝不读取测试机器上的 Keychain。
use super::*;

#[gpui_kit::test]
fn cli_must_be_verified_before_configuration(cx: &mut TestAppContext) {
    let (_dir, _events, model, main, _) = setup(cx);
    let (settings, _) = open_settings_for_test(cx, main);
    model.update(cx, |m, cx| {
        m.lark_connection = ServiceConnection::default();
        m.tapd_connection = ServiceConnection::default();
        cx.notify();
    });
    cx.update_window(settings, |_, w, cx| {
        w.render_frame(cx);
        assert!(
            w.try_find("connect-lark").is_none(),
            "an existing path alone must not enable account configuration"
        );
        assert!(w.try_find("connect-tapd").is_none());
        assert!(w.try_find("check-lark").is_none());
        assert!(w.try_find("probe-lark-cli").is_some());
    })
    .unwrap();
}

#[gpui_kit::test]
fn permission_summary_precedes_test_and_failure_is_below_it(cx: &mut TestAppContext) {
    let (_dir, _events, model, main, _) = setup(cx);
    let (settings, _) = open_settings_for_test(cx, main);
    model.update(cx, |m, cx| {
        m.receive(
            Event::ConnectionReport {
                tool: CliTool::Lark,
                report: ConnectionReport {
                    connected: false,
                    checks: vec![lark2tapd_bridge::PermissionCheck {
                        name: "task:task:read".into(),
                        state: PermissionState::Missing,
                        detail: "Required user grant missing".into(),
                    }],
                    user_scopes: Some(Vec::new()),
                    app_scopes: Some(vec!["task:task:read".into()]),
                    failure: Some(
                        "Lark permission check: missing task:task:read (code 99991672)".into(),
                    ),
                },
            },
            cx,
        )
    });
    cx.run_until_parked();
    let button = cx
        .update_window(settings, |_, w, cx| {
            w.render_frame(cx);
            w.find("check-lark").bounds()
        })
        .unwrap();
    let mut visual = VisualTestContext::from_window(settings, cx);
    let summary = visual.debug_bounds("lark-permission-summary").unwrap();
    let error = visual.debug_bounds("lark-operation-error").unwrap();
    assert!(summary.bottom() <= button.top());
    assert!(error.top() >= button.bottom());
    assert!(error.size.height > px(0.));
    cx.read(|cx| assert!(model.read(cx).errors["lark-connection"].contains("99991672")));
}

#[gpui_kit::test]
fn device_authorization_link_requires_explicit_click_and_is_cleared(cx: &mut TestAppContext) {
    let (_dir, _events, model, main, _) = setup(cx);
    let (settings, _) = open_settings_for_test(cx, main);
    let url = "https://accounts.feishu.cn/accounts/page/device";
    model.update(cx, |m, cx| {
        m.receive(
            Event::AuthorizationRequired {
                verification_url: url.into(),
                expires_in: 180,
            },
            cx,
        )
    });
    cx.run_until_parked();
    assert!(cx.opened_url().is_none());
    click(cx, settings, "open-lark-authorization");
    assert_eq!(cx.opened_url().as_deref(), Some(url));
    model.update(cx, |m, cx| m.receive(Event::AuthorizationFinished, cx));
    cx.run_until_parked();
    cx.update_window(settings, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("open-lark-authorization").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn product_has_no_demo_action(cx: &mut TestAppContext) {
    let (_dir, _events, _model, main, _) = setup(cx);
    cx.update_window(main, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("demo-change").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn connection_forms_are_hidden_until_requested(cx: &mut TestAppContext) {
    let (_dir, _events, _model, main, _) = setup(cx);
    let (settings, view) = open_settings_for_test(cx, main);
    cx.update_window(settings, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("lark-app-id").is_none());
        assert!(w.try_find("tapd-token").is_none());
        assert!(w.try_find("clear-lark").is_none());
        assert!(w.try_find("clear-tapd").is_none());
    })
    .unwrap();
    click(cx, settings, "connect-lark");
    cx.update_window(settings, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("lark-app-id").is_some());
        assert!(w.try_find("tapd-token").is_none());
    })
    .unwrap();
    click(cx, settings, "connect-tapd");
    cx.update_window(settings, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("tapd-token").is_some());
        view.update(cx, |v, cx| set(&v.tapd_token, "dummy-token".into(), w, cx));
    })
    .unwrap();
}

#[gpui_kit::test]
fn unconfigured_tapd_gates_destination_and_prefixes(cx: &mut TestAppContext) {
    let (_dir, _events, model, main, _) = setup(cx);
    let (settings, _) = open_settings_for_test(cx, main);
    // 只改变 UI 的展示模式；后台仍是 mock，不会发起真实账户操作。
    model.update(cx, |m, cx| {
        m.tapd_connection.saved = false;
        m.tapd_connection.verified = false;
        cx.notify();
    });
    cx.update_window(settings, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("settings-workspace").is_none());
        assert!(w.try_find("prefix-workspace").is_none());
        assert!(w.try_find("settings-project-refresh").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn command_w_closes_only_current_window_and_keeps_worker(cx: &mut TestAppContext) {
    let (_dir, events, model, main, _) = setup(cx);
    let (settings, _) = open_settings_for_test(cx, main);
    cx.update_window(settings, |_, w, cx| {
        w.activate_window();
        w.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(settings, |_, w, cx| w.press("cmd-w", cx))
        .unwrap();
    cx.run_until_parked();
    assert!(!cx.windows().contains(&settings));
    assert!(cx.windows().contains(&main));
    click(cx, main, "refresh");
    pump(&events, &model, cx, |cx| {
        model.read(cx).snapshot.as_ref().unwrap().records.len() == 3
    });
}

#[gpui_kit::test]
fn saved_connections_are_distinct_from_verification_and_clear_independently(
    cx: &mut TestAppContext,
) {
    let (_dir, _events, model, main, _) = setup(cx);
    let (settings, view) = open_settings_for_test(cx, main);
    model.update(cx, |m, cx| {
        m.mode = Mode::Live;
        m.receive(
            Event::ConnectionStatus {
                lark_saved: true,
                tapd_saved: true,
            },
            cx,
        );
        m.receive(
            Event::ConnectionReport {
                tool: CliTool::Tapd,
                report: ConnectionReport {
                    connected: true,
                    checks: Vec::new(),
                    user_scopes: None,
                    app_scopes: None,
                    failure: None,
                },
            },
            cx,
        );
    });
    cx.run_until_parked();
    cx.read(|cx| {
        assert!(model.read(cx).tapd_connection.verified);
        assert!(!model.read(cx).lark_connection.verified);
        assert!(
            model
                .read(cx)
                .tapd_connection
                .report
                .as_ref()
                .unwrap()
                .failure
                .is_none()
        );
    });
    cx.update_window(settings, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("clear-lark").is_some());
        assert!(w.try_find("clear-tapd").is_some());
        assert!(w.try_find("settings-workspace").is_some());
    })
    .unwrap();
    click(cx, settings, "clear-lark");
    cx.update_window(settings, |_, w, cx| w.press("escape", cx))
        .unwrap();
    cx.read(|cx| assert!(view.read(cx).clear_confirmation.is_none()));
    model.update(cx, |m, cx| {
        m.receive(Event::CredentialsCleared(CliTool::Lark), cx)
    });
    cx.run_until_parked();
    cx.read(|cx| {
        assert!(model.read(cx).tapd_connection.saved);
        assert!(model.read(cx).tapd_connection.verified);
        assert!(view.read(cx).lark_form_open);
    });
    cx.update_window(settings, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("clear-lark").is_none());
        assert!(w.try_find("clear-tapd").is_some());
    })
    .unwrap();
}

#[gpui_kit::test]
fn failed_save_retains_form_and_untyped_state_cannot_clear_secrets(cx: &mut TestAppContext) {
    let (_dir, _events, model, main, _) = setup(cx);
    let (settings, view) = open_settings_for_test(cx, main);
    click(cx, settings, "connect-tapd");
    cx.update_window(settings, |_, w, cx| {
        view.update(cx, |v, cx| {
            set(&v.tapd_token, "dummy-token".into(), w, cx);
            set(&v.app_secret, "other-secret".into(), w, cx);
        })
    })
    .unwrap();
    model.update(cx, |m, cx| {
        m.receive(
            Event::FormError {
                field: "tapd-connection",
                message: "Cannot save".into(),
            },
            cx,
        );
        m.receive(
            Event::State {
                mode: Mode::Mock,
                snapshot: m.snapshot.clone().unwrap(),
                profile: "mock".into(),
                message: "credentials_saved",
                new_count: 0,
            },
            cx,
        );
    });
    cx.run_until_parked();
    cx.read(|cx| {
        assert!(view.read(cx).tapd_form_open);
        assert_eq!(
            view.read(cx).tapd_token.read(cx).value().as_ref(),
            "dummy-token"
        );
        assert_eq!(
            view.read(cx).app_secret.read(cx).value().as_ref(),
            "other-secret"
        );
        assert!(model.read(cx).errors.contains_key("tapd-connection"));
    });
    model.update(cx, |m, cx| {
        m.receive(Event::CredentialsSaved(CliTool::Tapd), cx)
    });
    cx.run_until_parked();
    cx.read(|cx| {
        assert!(!view.read(cx).tapd_form_open);
        assert!(view.read(cx).tapd_token.read(cx).value().is_empty());
        assert_eq!(
            view.read(cx).app_secret.read(cx).value().as_ref(),
            "other-secret"
        );
    });
}

#[gpui_kit::test]
fn tab_navigation_keeps_bottom_controls_visible(cx: &mut TestAppContext) {
    let (_dir, _events, _model, main, _) = setup(cx);
    let (settings, view) = open_settings_for_test(cx, main);
    for size in [size(px(760.), px(720.)), size(px(620.), px(520.))] {
        cx.simulate_window_resize(settings, size);
        cx.update_window(settings, |_, w, cx| {
            view.read(cx)
                .settings_scroll
                .set_offset(point(px(0.), px(0.)));
            view.read(cx).focus_handle.clone().focus(w, cx);
            w.render_frame(cx);
        })
        .unwrap();
        for (key, target) in [("tab", "en"), ("shift-tab", "connect-lark")] {
            let mut reached = false;
            for _ in 0..80 {
                let found = cx
                    .update_window(settings, |_, w, cx| {
                        w.press(key, cx);
                        w.simulate_next_frame(cx);
                        w.render_frame(cx);
                        let control = w.find(target);
                        if control.focused() == Some(true) {
                            assert!(control.visible(), "{key}: {target} must scroll into view");
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap();
                if found {
                    reached = true;
                    break;
                }
                cx.run_until_parked();
            }
            assert!(reached, "{key} must reach {target}");
            if target == "en" {
                // 保留底部焦点，手工滚回顶部后不能被定时或普通重绘拉回。
                cx.update_window(settings, |_, w, cx| {
                    view.read(cx)
                        .settings_scroll
                        .set_offset(point(px(0.), px(0.)));
                    w.render_frame(cx);
                    w.simulate_next_frame(cx);
                    w.render_frame(cx);
                    assert_eq!(view.read(cx).settings_scroll.offset().y, px(0.));
                })
                .unwrap();
            }
        }
    }
}

#[gpui_kit::test]
fn refresh_buttons_follow_their_own_fields(cx: &mut TestAppContext) {
    let (_dir, _events, _model, main, _) = setup(cx);
    let (settings, _) = open_settings_for_test(cx, main);
    cx.update_window(settings, |_, w, cx| {
        w.render_frame(cx);
        for (field, button) in [
            ("settings-workspace", "settings-project-refresh"),
            ("settings-owner", "settings-member-refresh"),
        ] {
            let input = w.find(field).bounds();
            let refresh = w.find(button).bounds();
            assert!(refresh.left() >= input.right());
            assert!((refresh.center().y - input.center().y).abs() < px(2.));
        }
    })
    .unwrap();
}
