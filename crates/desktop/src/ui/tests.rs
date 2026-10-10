//! GPUI 真实点击/输入 → 工作线程 → 同步引擎 → SQLite，只有远端服务使用 mock。
use super::*;
use crate::worker;
use gpui_kit::test::TestWindowExt;
use std::prelude::v1::test;
use std::time::Duration;

fn receive(events: &async_channel::Receiver<Event>) -> Event {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(8), events.recv())
            .await
            .expect("worker deadline")
            .expect("worker event")
    })
}

fn drain(
    events: &async_channel::Receiver<Event>,
    model: &Entity<Model>,
    cx: &mut TestAppContext,
) -> bool {
    loop {
        let event = receive(events);
        let done = matches!(&event,Event::State {message,..} if !message.is_empty())
            || matches!(&event, Event::Error(_));
        let success = !matches!(&event, Event::Error(_));
        cx.update(|cx| model.update(cx, |m, cx| m.receive(event, cx)));
        cx.run_until_parked();
        if done {
            return success;
        }
    }
}

fn setup(
    cx: &mut TestAppContext,
) -> (
    tempfile::TempDir,
    async_channel::Receiver<Event>,
    Entity<Model>,
    AnyWindowHandle,
    Entity<Workspace>,
) {
    let dir = tempfile::tempdir().unwrap();
    let worker = worker::start(dir.path().into(), Mode::Mock);
    cx.update(gpui_kit::init);
    let model = cx.new(|_| Model {
        snapshot: None,
        busy: false,
        notice: String::new(),
        error: false,
        preview: None,
        mode: Mode::Mock,
        install_plan: None,
        config_revision: 0,
        lark_diagnostic: String::new(),
        tapd_diagnostic: String::new(),
        pending_request: 0,
        profile: String::new(),
        jobs: worker.jobs,
        tray: None,
    });
    assert!(drain(&worker.events, &model, cx));
    let (window, view) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Point::default(),
                    size: size(px(1440.), px(1200.)),
                })),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| Workspace::new(model.clone(), window, cx)),
        )
        .unwrap()
    });
    (dir, worker.events, model, window, view)
}

fn click(cx: &mut TestAppContext, window: AnyWindowHandle, id: &str) {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(SharedString::from(id.to_owned()), cx);
    })
    .unwrap();
}

fn choose(cx: &mut TestAppContext, handle: AnyWindowHandle, id: &str, query: &str) {
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window
            .within(SharedString::from(id.to_owned()))
            .click("input", cx);
        window.input(query, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.press("down", cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn gui_e2e_import_preview_update_and_sqlite_restart(cx: &mut TestAppContext) {
    let (dir, events, model, window, _view) = setup(cx);
    click(cx, window, "refresh");
    assert!(drain(&events, &model, cx));
    let guid = cx.read(|cx| {
        model.read(cx).snapshot.as_ref().unwrap().records[0]
            .source
            .guid
            .clone()
    });
    click(cx, window, &format!("select-{guid}"));
    assert!(drain(&events, &model, cx));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("draft-title", cx);
        window.press("cmd-a", cx);
        window.input("经过编辑的导入标题", cx);
        assert_eq!(
            window.find("draft-title").value(),
            Some("经过编辑的导入标题")
        );
        window.click("preview", cx);
    })
    .unwrap();
    assert!(drain(&events, &model, cx));
    cx.read(|cx| {
        assert!(
            model.read(cx).snapshot.as_ref().unwrap().records[0]
                .target
                .is_none()
        )
    });
    click(cx, window, "confirm-import");
    assert!(drain(&events, &model, cx));
    let target_id = cx.read(|cx| {
        let r = &model.read(cx).snapshot.as_ref().unwrap().records[0];
        assert_eq!(r.status(), SyncStatus::Synced);
        assert_eq!(r.target.as_ref().unwrap().title, "经过编辑的导入标题");
        r.target.as_ref().unwrap().id.clone()
    });
    click(cx, window, "demo-change");
    assert!(drain(&events, &model, cx));
    click(cx, window, "nav-changed");
    click(cx, window, &format!("select-{guid}"));
    assert!(drain(&events, &model, cx));
    click(cx, window, "preview");
    assert!(drain(&events, &model, cx));
    cx.read(|cx| {
        assert_eq!(
            model
                .read(cx)
                .preview
                .as_ref()
                .unwrap()
                .target
                .as_ref()
                .unwrap()
                .id,
            target_id
        )
    });
    click(cx, window, "confirm-import");
    assert!(drain(&events, &model, cx));
    // 重开真实 SQLite 连接，验证 UI 成功不是仅修改内存状态。
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let reopened = lark2tapd_core::Engine::open(&dir.path().join("mock.db"))
            .await
            .unwrap();
        let record = &reopened.snapshot().records[0];
        assert_eq!(record.status(), SyncStatus::Synced);
        assert_eq!(record.target.as_ref().unwrap().id, target_id);
    });
}

#[gpui_kit::test]
fn gui_settings_switch_language_theme_and_validate_interval(cx: &mut TestAppContext) {
    let (_dir, events, model, window, _view) = setup(cx);
    click(cx, window, "nav-settings");
    click(cx, window, "en");
    assert!(drain(&events, &model, cx));
    click(cx, window, "dark");
    assert!(drain(&events, &model, cx));
    cx.read(|cx| {
        let settings = &model.read(cx).snapshot.as_ref().unwrap().settings;
        assert_eq!(settings.language, "en");
        assert_eq!(settings.theme, "dark");
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("poll-minutes", cx);
        window.press("cmd-a", cx);
        window.input("0", cx);
    })
    .unwrap();
    cx.run_until_parked();
    click(cx, window, "save-settings");
    cx.read(|cx| assert!(model.read(cx).busy, "save button must enqueue validation"));
    assert!(!drain(&events, &model, cx));
    cx.read(|cx| {
        assert_eq!(
            model
                .read(cx)
                .snapshot
                .as_ref()
                .unwrap()
                .settings
                .poll_minutes,
            15
        )
    });
}

#[gpui_kit::test]
fn gui_refresh_invalidates_stale_editor(cx: &mut TestAppContext) {
    let (_dir, events, model, window, _view) = setup(cx);
    click(cx, window, "refresh");
    assert!(drain(&events, &model, cx));
    let guid = cx.read(|cx| {
        model.read(cx).snapshot.as_ref().unwrap().records[0]
            .source
            .guid
            .clone()
    });
    click(cx, window, &format!("select-{guid}"));
    assert!(drain(&events, &model, cx));
    click(cx, window, "demo-change");
    assert!(drain(&events, &model, cx));
    // Kit 未保证每个控件报告 disabled 属性；验证实际点击没有排入写入工作。
    click(cx, window, "preview");
    cx.read(|cx| {
        assert!(!model.read(cx).busy);
        assert!(model.read(cx).preview.is_none());
    });
    click(cx, window, "reload-source");
    assert!(drain(&events, &model, cx));
    click(cx, window, "preview");
    assert!(drain(&events, &model, cx));
    cx.read(|cx| assert!(model.read(cx).preview.is_some()));
}

#[test]
fn actor_rejects_mock_auth_and_cross_profile_requests() {
    let dir = tempfile::tempdir().unwrap();
    let worker = worker::start(dir.path().into(), Mode::Mock);
    let _ = receive(&worker.events);
    for job in [
        Job::Login,
        Job::Check(CliTool::Lark),
        Job::Check(CliTool::Tapd),
        Job::PlanInstall(CliTool::Lark),
        Job::Install(CliTool::Tapd),
        Job::SaveCredentials(Credentials::default()),
        Job::ClearCredentials,
    ] {
        worker
            .jobs
            .send(Request {
                job,
                profile: "mock".into(),
            })
            .unwrap();
        assert!(matches!(receive(&worker.events), Event::State { .. }));
        match receive(&worker.events) {
            Event::Error(e) => assert!(e.contains("Demo mode")),
            _ => panic!("mock auth unexpectedly succeeded"),
        }
    }
    worker
        .jobs
        .send(Request {
            job: Job::OpenDraft("anything".into(), false),
            profile: "old-account".into(),
        })
        .unwrap();
    let _ = receive(&worker.events);
    match receive(&worker.events) {
        Event::Error(e) => assert!(e.contains("Account changed")),
        _ => panic!("stale account accepted"),
    }
}

#[gpui_kit::test]
fn settings_do_not_overwrite_task_destination(cx: &mut TestAppContext) {
    let (_dir, events, model, window, _) = setup(cx);
    click(cx, window, "refresh");
    assert!(drain(&events, &model, cx));
    let guid = cx.read(|cx| {
        model.read(cx).snapshot.as_ref().unwrap().records[0]
            .source
            .guid
            .clone()
    });
    click(cx, window, &format!("select-{guid}"));
    assert!(drain(&events, &model, cx));
    choose(cx, window, "draft-workspace", "演示项目二");
    choose(cx, window, "draft-owner", "bob");
    click(cx, window, "nav-settings");
    click(cx, window, "nav-pending");
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("draft-workspace").value(), Some("演示项目二"));
        assert_eq!(window.find("draft-owner").value(), Some("演示成员 (bob)"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn retained_preview_restores_editor_after_window_reopen(cx: &mut TestAppContext) {
    let (_dir, events, model, window, _) = setup(cx);
    click(cx, window, "refresh");
    assert!(drain(&events, &model, cx));
    let guid = cx.read(|cx| {
        model.read(cx).snapshot.as_ref().unwrap().records[0]
            .source
            .guid
            .clone()
    });
    click(cx, window, &format!("select-{guid}"));
    assert!(drain(&events, &model, cx));
    click(cx, window, "preview");
    assert!(drain(&events, &model, cx));
    let expected = cx.read(|cx| model.read(cx).preview.as_ref().unwrap().draft.title.clone());
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
    let (reopened, _) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Point::default(),
                    size: size(px(1440.), px(1200.)),
                })),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| Workspace::new(model.clone(), window, cx)),
        )
        .unwrap()
    });
    click(cx, reopened, "back-to-edit");
    cx.run_until_parked();
    cx.update_window(reopened, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("draft-title").value(), Some(expected.as_str()));
    })
    .unwrap();
}

#[test]
fn actor_reads_live_cache_without_credentials_or_network() {
    use lark2tapd_core::{Engine, MockGateway};
    let dir = tempfile::tempdir().unwrap();
    let profile = "a".repeat(64);
    let guid = tokio::runtime::Runtime::new().unwrap().block_on(async {
        let mut pref = Engine::open(&dir.path().join("preferences-live.db"))
            .await
            .unwrap();
        let settings = Settings {
            last_profile: Some(profile.clone()),
            lark_path: "/not-installed/lark".into(),
            tapd_path: "/not-installed/tapd".into(),
            ..Settings::default()
        };
        pref.save_settings(settings).await.unwrap();
        let mut cache = Engine::open(&dir.path().join(format!("live-{profile}.db")))
            .await
            .unwrap();
        cache.refresh(&MockGateway::default()).await.unwrap();
        cache.snapshot().records[0].source.guid.clone()
    });
    let worker = worker::start(dir.path().into(), Mode::Live);
    let _ = receive(&worker.events);
    worker
        .jobs
        .send(Request {
            job: Job::OpenDraft(guid.clone(), false),
            profile,
        })
        .unwrap();
    match receive(&worker.events) {
        Event::Editor(e) => assert_eq!(e.source.guid, guid),
        _ => panic!("offline cached task was not readable"),
    }
}

#[test]
fn actor_excludes_second_writer() {
    let dir = tempfile::tempdir().unwrap();
    let first = worker::start(dir.path().into(), Mode::Mock);
    let _ = receive(&first.events);
    let second = worker::start(dir.path().into(), Mode::Mock);
    match receive(&second.events) {
        Event::Error(e) => assert!(e.contains("already open")),
        _ => panic!("second writer accepted"),
    }
}

#[gpui_kit::test]
fn gui_standard_project_member_and_date_selection(cx: &mut TestAppContext) {
    let (_dir, events, model, window, view) = setup(cx);
    click(cx, window, "refresh");
    assert!(drain(&events, &model, cx));
    let guid = cx.read(|cx| {
        model.read(cx).snapshot.as_ref().unwrap().records[0]
            .source
            .guid
            .clone()
    });
    click(cx, window, &format!("select-{guid}"));
    assert!(drain(&events, &model, cx));
    choose(cx, window, "draft-owner", "alice");
    cx.read(|cx| assert_eq!(view.read(cx).draft(cx).owner, "alice"));
    choose(cx, window, "draft-workspace", "演示项目二");
    cx.read(|cx| {
        let d = view.read(cx).draft(cx);
        assert_eq!(d.workspace_id, "demo-2");
        assert!(d.owner.is_empty());
    });
    choose(cx, window, "draft-owner", "bob");
    let due = cx.read(|cx| view.read(cx).due.clone());
    let picker_id: ElementId = ("date-picker", due.entity_id()).into();
    cx.update_window(window, |_, window, cx| {
        due.update(cx, |s, cx| {
            s.set_date(
                chrono::NaiveDate::from_ymd_opt(2026, 9, 15).unwrap(),
                window,
                cx,
            )
        });
        window.render_frame(cx);
        window.click(picker_id.clone(), cx);
        window.click("calendar-2026-09-16-0-2", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(view.read(cx).draft(cx).due.as_deref(), Some("2026-09-16")));
    click(cx, window, "preview");
    assert!(drain(&events, &model, cx));
    cx.read(|cx| {
        let d = &model.read(cx).preview.as_ref().unwrap().draft;
        assert_eq!(d.workspace_id, "demo-2");
        assert_eq!(d.owner, "bob");
        assert_eq!(d.due.as_deref(), Some("2026-09-16"));
    });
    click(cx, window, "confirm-import");
    assert!(drain(&events, &model, cx));
    cx.read(|cx| {
        let r = &model.read(cx).snapshot.as_ref().unwrap().records[0];
        assert_eq!(r.target_workspace.as_deref(), Some("demo-2"));
        assert_eq!(r.target.as_ref().unwrap().owner, "bob");
    });
    // 标准日期组件清空行为也进入相同的业务草稿值。
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.within(picker_id.clone()).click("clean", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| assert!(view.read(cx).draft(cx).due.is_none()));
}

#[gpui_kit::test]
fn branding_reaches_component_button_tokens_in_both_themes(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    for mode in ["light", "dark"] {
        cx.update(|cx| {
            apply_theme(mode, cx);
            let theme = Theme::global(cx);
            assert_eq!(theme.tokens.button_primary, theme.colors.primary.into());
            assert_eq!(
                theme.tokens.button_primary_foreground,
                theme.colors.primary_foreground.into()
            );
            assert_eq!(
                theme.tokens.button_secondary_foreground,
                theme.colors.primary.into()
            );
        });
    }
}

#[gpui_kit::test]
fn mock_settings_exposes_diagnostics_without_mode_switches(cx: &mut TestAppContext) {
    let (_dir, _events, model, window, _view) = setup(cx);
    click(cx, window, "nav-settings");
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("mode-mock").is_none());
        assert!(window.try_find("mode-live").is_none());
    })
    .unwrap();
    // 结果处理是逐平台的，不能把版本存在误报为账号连接通过。
    cx.update(|cx| {
        model.update(cx, |m, cx| {
            m.receive(
                Event::Diagnostic {
                    tool: CliTool::Lark,
                    version: Some("1.0.96".into()),
                    connected: false,
                    detail: "Permission missing".into(),
                },
                cx,
            )
        })
    });
    cx.read(|cx| {
        assert!(model.read(cx).lark_diagnostic.contains("1.0.96"));
        assert!(
            model
                .read(cx)
                .lark_diagnostic
                .contains("Permission missing")
        );
        assert!(model.read(cx).tapd_diagnostic.is_empty());
    });
}

#[test]
fn live_installer_requires_confirmation_and_cancel_has_no_side_effects() {
    let dir = tempfile::tempdir().unwrap();
    let worker = worker::start(dir.path().into(), Mode::Live);
    let _ = receive(&worker.events);
    let send = |job| {
        worker
            .jobs
            .send(Request {
                job,
                profile: String::new(),
            })
            .unwrap()
    };
    send(Job::Install(CliTool::Tapd));
    let _ = receive(&worker.events);
    match receive(&worker.events) {
        Event::Error(e) => assert!(e.contains("before confirming")),
        _ => panic!("install without confirmation"),
    }
    send(Job::PlanInstall(CliTool::Tapd));
    match receive(&worker.events) {
        Event::InstallPlan(p) => {
            assert!(
                p.source
                    .starts_with("https://cnb.cool/tapd.cn/skills/tapd-cli/")
            );
            assert!(
                p.destination
                    .starts_with(dir.path().canonicalize().unwrap())
            );
        }
        _ => panic!("missing install plan"),
    }
    let _ = receive(&worker.events);
    send(Job::CancelInstall);
    let _ = receive(&worker.events);
    send(Job::Install(CliTool::Tapd));
    let _ = receive(&worker.events);
    assert!(matches!(receive(&worker.events), Event::Error(_)));
    assert!(!dir.path().join("tools").exists());
}

#[gpui_kit::test]
fn successful_installation_projects_saved_path_into_settings(cx: &mut TestAppContext) {
    let (_dir, _events, model, window, view) = setup(cx);
    click(cx, window, "nav-settings");
    let mut snapshot = cx.read(|cx| model.read(cx).snapshot.clone().unwrap());
    snapshot.settings.lark_path = "/private/application/tools/lark-cli".into();
    cx.update(|cx| {
        model.update(cx, |m, cx| {
            m.receive(
                Event::State {
                    snapshot,
                    profile: "mock".into(),
                    mode: Mode::Mock,
                    message: "installed",
                    new_count: 0,
                },
                cx,
            )
        })
    });
    cx.run_until_parked();
    cx.read(|cx| {
        assert_eq!(
            view.read(cx).lark_path.read(cx).value().as_ref(),
            "/private/application/tools/lark-cli"
        )
    });
}
