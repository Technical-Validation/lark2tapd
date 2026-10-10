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
        window.click("save-settings", cx);
    })
    .unwrap();
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
        Job::Check,
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
    cx.update_window(window, |_, window, cx| {
        window.click("draft-workspace", cx);
        window.press("cmd-a", cx);
        window.input("123456", cx);
        window.click("draft-owner", cx);
        window.input("task-owner", cx);
    })
    .unwrap();
    click(cx, window, "nav-settings");
    click(cx, window, "nav-pending");
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("draft-workspace").value(), Some("123456"));
        assert_eq!(window.find("draft-owner").value(), Some("task-owner"));
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
