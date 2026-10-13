//! 真正的 GPUI 输入事件连接后台 actor 和 SQLite；远端服务及上传使用明确 mock。
use super::*;
use crate::worker;
#[path = "layout_tests.rs"]
mod layout;
#[path = "prefix_settings_tests.rs"]
mod prefix_settings;
use gpui_kit::component::{Theme, WindowExt};
use gpui_kit::test::TestWindowExt;
use std::{
    prelude::v1::test,
    time::{Duration, Instant},
};

type Fixture = (
    tempfile::TempDir,
    async_channel::Receiver<Event>,
    Entity<Model>,
    AnyWindowHandle,
    Entity<Workspace>,
);
fn pump(
    events: &async_channel::Receiver<Event>,
    model: &Entity<Model>,
    cx: &mut TestAppContext,
    until: impl Fn(&App) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(12);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    while !cx.read(|cx| until(cx)) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let event = runtime.block_on(async {
            tokio::time::timeout(remaining, events.recv())
                .await
                .expect("worker event deadline")
                .expect("worker event channel")
        });
        if let Event::FormError { field, message } = &event {
            eprintln!("worker field {field}: {message}");
        }
        cx.update(|cx| model.update(cx, |m, cx| m.receive(event, cx)));
        cx.run_until_parked();
    }
}
fn setup(cx: &mut TestAppContext) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let w = worker::start(dir.path().into(), Mode::Mock);
    cx.update(gpui_kit::init);
    let model = cx.new(|_| Model {
        snapshot: None,
        busy: false,
        notice: String::new(),
        error: false,
        pending_error: None,
        mode: Mode::Mock,
        install_plan: None,
        lark_diagnostic: String::new(),
        tapd_diagnostic: String::new(),
        activities: BTreeSet::new(),
        errors: BTreeMap::new(),
        checked_workspace: None,
        data_root: w.data_root.clone(),
        state_revision: 0,
        last_action: "",
        pending_request: 0,
        profile: String::new(),
        jobs: w.jobs,
        tray: None,
    });
    pump(&w.events, &model, cx, |cx| {
        model.read(cx).snapshot.is_some()
    });
    let (handle, view) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Point::default(),
                    size: size(px(1440.), px(1800.)),
                })),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| Workspace::new(model.clone(), window, cx)),
        )
        .unwrap()
    });
    cx.update(|cx| {
        cx.set_global(Shell {
            model: model.clone(),
            window: Some(handle),
            settings_window: None,
        });
        configure_actions(cx);
    });
    (dir, w.events, model, handle, view)
}
// 既有表单测试沿用大视口；独立窗口生命周期测试不修改默认窗口尺寸。
fn open_settings_for_test(
    cx: &mut TestAppContext,
    main: AnyWindowHandle,
) -> (AnyWindowHandle, Entity<Workspace>) {
    click(cx, main, "nav-settings");
    let handle = cx.read(|cx| cx.global::<Shell>().settings_window.unwrap());
    cx.simulate_window_resize(handle, size(px(1440.), px(1800.)));
    cx.run_until_parked();
    let view = cx
        .update_window(handle, |root, _, cx| {
            root.downcast::<gpui_kit::base::Root>()
                .unwrap()
                .read(cx)
                .view()
                .clone()
                .downcast::<Workspace>()
                .unwrap()
        })
        .unwrap();
    cx.run_until_parked();
    (handle, view)
}

fn close_test_window(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.update_window(handle, |_, w, _| w.remove_window())
        .unwrap();
    cx.run_until_parked();
}

fn click(cx: &mut TestAppContext, handle: AnyWindowHandle, id: &str) {
    cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        w.click(SharedString::from(id.to_owned()), cx);
    })
    .unwrap();
    cx.run_until_parked();
}
fn choose(cx: &mut TestAppContext, handle: AnyWindowHandle, id: &str, query: &str) {
    cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        w.within(SharedString::from(id.to_owned()))
            .click("input", cx);
        w.input(query, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        w.press("down", cx);
        w.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
}
fn load_task(
    events: &async_channel::Receiver<Event>,
    model: &Entity<Model>,
    view: &Entity<Workspace>,
    handle: AnyWindowHandle,
    cx: &mut TestAppContext,
) -> String {
    click(cx, handle, "refresh");
    pump(events, model, cx, |cx| {
        model
            .read(cx)
            .snapshot
            .as_ref()
            .is_some_and(|s| !s.records.is_empty())
    });
    let guid = cx.read(|cx| {
        model.read(cx).snapshot.as_ref().unwrap().records[0]
            .source
            .guid
            .clone()
    });
    click(cx, handle, &format!("select-{guid}"));
    pump(events, model, cx, |cx| {
        view.read(cx)
            .edit_source
            .as_ref()
            .is_some_and(|s| s.guid == guid)
    });
    pump(events, model, cx, |cx| {
        model.read(cx).snapshot.as_ref().is_some_and(|s| {
            !s.directory.workspaces.is_empty() && s.directory.members.contains_key("demo")
        })
    });
    guid
}

#[gpui_kit::test]
fn opening_main_window_checks_automatically(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, _) = setup(cx);
    cx.update(|cx| {
        cx.set_global(Shell {
            model: model.clone(),
            window: Some(handle),
            settings_window: None,
        });
        show_window(cx);
    });
    pump(&events, &model, cx, |cx| {
        model
            .read(cx)
            .snapshot
            .as_ref()
            .is_some_and(|s| s.records.len() == 3)
    });
    assert!(cx.read(|cx| {
        model
            .read(cx)
            .snapshot
            .as_ref()
            .unwrap()
            .last_checked
            .is_some()
    }));
}

#[gpui_kit::test]
fn direct_submit_prefix_and_update_persist_without_preview(cx: &mut TestAppContext) {
    let (dir, events, model, handle, view) = setup(cx);
    let guid = load_task(&events, &model, &view, handle, cx);
    model.update(cx, |m, cx| {
        m.send(
            Job::SetPreference(Preference::AddWorkspacePrefix {
                workspace: "demo".into(),
                prefix: "产品 A".into(),
            }),
            cx,
        )
    });
    pump(&events, &model, cx, |cx| {
        model
            .read(cx)
            .snapshot
            .as_ref()
            .unwrap()
            .settings
            .workspace_prefixes
            .contains_key("demo")
    });
    choose(cx, handle, "title-prefix", "产品 A");
    cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        w.click("draft-title", cx);
        w.press("cmd-a", cx);
        w.input("直接导入的标题", cx);
        assert!(w.try_find("preview").is_none());
    })
    .unwrap();
    click(cx, handle, "submit");
    pump(&events, &model, cx, |cx| {
        model.read(cx).snapshot.as_ref().unwrap().records[0]
            .target
            .is_some()
    });
    let id = cx.read(|cx| {
        let t = model.read(cx).snapshot.as_ref().unwrap().records[0]
            .target
            .as_ref()
            .unwrap();
        assert_eq!(t.title, "产品 A直接导入的标题");
        t.id.clone()
    });
    cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        w.click("draft-title", cx);
        w.press("cmd-a", cx);
        w.input("同一目标的新标题", cx);
    })
    .unwrap();
    click(cx, handle, "submit");
    pump(&events, &model, cx, |cx| {
        model.read(cx).snapshot.as_ref().unwrap().records[0]
            .target
            .as_ref()
            .unwrap()
            .title
            == "产品 A同一目标的新标题"
    });
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let engine = lark2tapd_core::Engine::open(&dir.path().join("mock.db"))
            .await
            .unwrap();
        assert_eq!(engine.snapshot().records[0].target.as_ref().unwrap().id, id);
        assert_eq!(engine.prepare(&guid).unwrap().title_prefix, "产品 A");
    });
}

#[gpui_kit::test]
fn polling_autosaves_and_reports_invalid_input_inline(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, _view) = setup(cx);
    let (handle, view) = open_settings_for_test(cx, handle);
    cx.update_window(handle, |_, w, cx| {
        w.scroll(
            "lark-cli-help",
            ScrollDelta::Pixels(point(px(0.), px(-1100.))),
            cx,
        );
    })
    .unwrap();
    cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("save-settings").is_none());
        assert!(w.try_find("poll-minutes").is_none());
    })
    .unwrap();
    click(cx, handle, "toggle-poll");
    pump(&events, &model, cx, |cx| {
        model
            .read(cx)
            .snapshot
            .as_ref()
            .unwrap()
            .settings
            .poll_enabled
    });
    cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        w.click("poll-minutes", cx);
        w.press("cmd-a", cx);
        w.input("0", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        assert!(model.read(cx).errors.contains_key("poll"));
        assert_eq!(
            model
                .read(cx)
                .snapshot
                .as_ref()
                .unwrap()
                .settings
                .poll_minutes,
            15
        );
    });
    cx.update_window(handle, |_, w, cx| {
        w.click("poll-minutes", cx);
        w.press("cmd-a", cx);
        w.input("60", cx);
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
            == 60
    });
    cx.read(|cx| {
        assert!(!model.read(cx).errors.contains_key("poll"));
        assert_eq!(view.read(cx).poll.read(cx).value().as_ref(), "60");
    });
    click(cx, handle, "toggle-poll");
    pump(&events, &model, cx, |cx| {
        !model
            .read(cx)
            .snapshot
            .as_ref()
            .unwrap()
            .settings
            .poll_enabled
    });
    cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("poll-minutes").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn source_details_expand_and_settings_do_not_clobber_draft(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    load_task(&events, &model, &view, handle, cx);
    cx.read(|cx| assert!(!view.read(cx).source_open));
    click(cx, handle, "toggle-source-details");
    cx.read(|cx| assert!(view.read(cx).source_open));
    click(cx, handle, "toggle-source-details");
    choose(cx, handle, "draft-owner", "alice");
    let before = cx.read(|cx| view.read(cx).draft(cx));
    let (settings, _) = open_settings_for_test(cx, handle);
    close_test_window(cx, settings);
    cx.read(|cx| {
        assert_eq!(view.read(cx).draft(cx), before);
    });
}

#[gpui_kit::test]
fn pasted_image_and_attachment_reach_mock_target_once(cx: &mut TestAppContext) {
    let (dir, events, model, handle, view) = setup(cx);
    let guid = load_task(&events, &model, &view, handle, cx);
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgba8(2, 2)
        .write_to(&mut encoded, image::ImageFormat::Png)
        .unwrap();
    cx.update_window(handle, |_, w, cx| {
        let description = view.read(cx).description.clone();
        description.update(cx, |s, cx| s.focus(w, cx));
        cx.write_to_clipboard(ClipboardItem::new_image(&Image::from_bytes(
            ImageFormat::Png,
            encoded.into_inner(),
        )));
        w.press("cmd-v", cx);
    })
    .unwrap();
    cx.run_until_parked();
    pump(&events, &model, cx, |cx| {
        view.read(cx).attachments.len() == 1 && view.read(cx).pending_pastes.is_empty()
    });
    cx.read(|cx| {
        assert!(
            view.read(cx)
                .description
                .read(cx)
                .value()
                .contains("attachment:")
        )
    });
    click(cx, handle, "submit");
    pump(&events, &model, cx, |cx| {
        model.read(cx).snapshot.as_ref().unwrap().records[0]
            .last_draft
            .as_ref()
            .is_some_and(|d| d.attachments[0].remote_url.is_some())
            && !model.read(cx).busy
    });
    let first = cx.read(|cx| {
        model.read(cx).snapshot.as_ref().unwrap().records[0]
            .target
            .as_ref()
            .unwrap()
            .id
            .clone()
    });
    click(cx, handle, "save-draft");
    pump(&events, &model, cx, |cx| {
        model.read(cx).snapshot.as_ref().unwrap().records[0]
            .draft
            .is_some()
    });
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let e = lark2tapd_core::Engine::open(&dir.path().join("mock.db"))
            .await
            .unwrap();
        let d = e.prepare(&guid).unwrap();
        assert!(d.attachments[0].remote_url.is_some());
        assert_eq!(e.snapshot().records[0].target.as_ref().unwrap().id, first);
    });
}

#[gpui_kit::test]
fn standard_date_picker_and_project_cascade_still_work(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    load_task(&events, &model, &view, handle, cx);
    choose(cx, handle, "draft-owner", "alice");
    choose(cx, handle, "draft-workspace", "演示项目二");
    pump(&events, &model, cx, |cx| {
        model
            .read(cx)
            .snapshot
            .as_ref()
            .unwrap()
            .directory
            .members
            .contains_key("demo-2")
    });
    cx.read(|cx| assert!(view.read(cx).draft(cx).owner.is_empty()));
    choose(cx, handle, "draft-owner", "bob");
    let due = cx.read(|cx| view.read(cx).due.clone());
    let id: ElementId = ("date-picker", due.entity_id()).into();
    cx.update_window(handle, |_, w, cx| {
        due.update(cx, |s, cx| {
            s.set_date(chrono::NaiveDate::from_ymd_opt(2026, 9, 15).unwrap(), w, cx)
        });
        w.render_frame(cx);
        w.click(id.clone(), cx);
        w.click("calendar-2026-09-16-0-2", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        let d = view.read(cx).draft(cx);
        assert_eq!(d.workspace_id, "demo-2");
        assert_eq!(d.owner, "bob");
        assert_eq!(d.due.as_deref(), Some("2026-09-16"));
    });
}

#[gpui_kit::test]
fn text_tokens_are_neutral_while_buttons_keep_brand_fill(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    for mode in ["light", "dark"] {
        cx.update(|cx| {
            apply_theme(mode, cx);
            let t = Theme::global(cx);
            assert_eq!(t.colors.button_primary, t.colors.primary);
            assert_eq!(t.colors.button_secondary_foreground, t.colors.foreground);
            assert_eq!(t.colors.secondary_foreground, t.colors.foreground);
            assert_eq!(t.colors.link, t.colors.foreground);
        });
    }
}

fn png_fixture() -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgba8(2, 2)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

#[gpui_kit::test]
fn attachments_keep_editor_session_and_individual_paste_markers(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    let guid = load_task(&events, &model, &view, handle, cx);
    let session = cx.read(|cx| view.read(cx).editor_session);
    let root = cx.read(|cx| worker::media_root(&model.read(cx).data_root, "mock").unwrap());
    let a = lark2tapd_core::save_pasted_image(&root, &png_fixture(), "png").unwrap();
    let m1 = "![one](attachment:pending-one)".to_owned();
    let m2 = "![two](attachment:pending-two)".to_owned();
    cx.update_window(handle, |_, _w, cx| {
        view.update(cx, |v, cx| {
            v.pending_pastes.extend([m1.clone(), m2.clone()]);
            v.description.update(cx, |s, cx| {
                s.set_text(format!("before{m1}between{m2}after"), cx)
            });
        })
    })
    .unwrap();
    cx.update(|cx| {
        model.update(cx, |m, cx| {
            m.receive(
                Event::AttachmentFailed {
                    guid: guid.clone(),
                    profile: "mock".into(),
                    session,
                    marker: Some(m1.clone()),
                    message: "one failed".into(),
                },
                cx,
            )
        })
    });
    cx.run_until_parked();
    cx.read(|cx| {
        let v = view.read(cx);
        assert!(v.pending_pastes.contains(&m2));
        assert!(!v.description.read(cx).value().contains(&m1));
        assert!(v.description.read(cx).value().contains(&m2));
    });
    cx.update(|cx| {
        model.update(cx, |m, cx| {
            m.receive(
                Event::AttachmentReady {
                    guid: guid.clone(),
                    profile: "mock".into(),
                    session,
                    marker: Some(m2.clone()),
                    attachment: a.clone(),
                },
                cx,
            )
        })
    });
    cx.run_until_parked();
    cx.read(|cx| {
        let v = view.read(cx);
        assert_eq!(v.attachments.len(), 1);
        assert!(v.description.read(cx).value().contains(&a.id));
        assert!(v.description.read(cx).value().starts_with("beforebetween"));
        assert!(v.description.read(cx).value().ends_with("after"));
    });
    view.update(cx, |v, cx| v.reload_source(guid.clone(), cx));
    pump(&events, &model, cx, |cx| {
        view.read(cx).edit_source.is_some()
    });
    cx.update(|cx| {
        model.update(cx, |m, cx| {
            m.receive(
                Event::AttachmentReady {
                    guid: guid.clone(),
                    profile: "mock".into(),
                    session,
                    marker: None,
                    attachment: a,
                },
                cx,
            )
        })
    });
    cx.run_until_parked();
    cx.read(|cx| assert!(view.read(cx).attachments.is_empty()));
}

#[gpui_kit::test]
fn file_selected_image_is_referenced_and_removed_decode_cannot_return(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    let guid = load_task(&events, &model, &view, handle, cx);
    let session = cx.read(|cx| view.read(cx).editor_session);
    let root = cx.read(|cx| worker::media_root(&model.read(cx).data_root, "mock").unwrap());
    let a = lark2tapd_core::save_pasted_image(&root, &png_fixture(), "png").unwrap();
    cx.update_window(handle, |_, _w, cx| {
        view.update(cx, |v, cx| {
            v.description
                .update(cx, |s, cx| s.set_text("Preserved text", cx))
        })
    })
    .unwrap();
    cx.update(|cx| {
        model.update(cx, |m, cx| {
            m.receive(
                Event::AttachmentReady {
                    guid,
                    profile: "mock".into(),
                    session,
                    marker: None,
                    attachment: a.clone(),
                },
                cx,
            )
        })
    });
    cx.run_until_parked();
    cx.read(|cx| {
        assert!(
            view.read(cx)
                .description
                .read(cx)
                .value()
                .contains("Preserved text")
        );
        assert!(
            view.read(cx)
                .description
                .read(cx)
                .value()
                .contains(&format!("attachment:{}", a.id))
        );
    });
    cx.update_window(handle, |_, w, cx| {
        view.update(cx, |v, cx| {
            v.images.clear();
            v.loading_images.clear();
            v.load_images(w, cx);
            v.attachments.clear();
        })
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| assert!(view.read(cx).images.is_empty()));
}

#[gpui_kit::test]
fn successful_action_events_clear_credentials_and_install_confirmation(cx: &mut TestAppContext) {
    let (_dir, _events, model, handle, _view) = setup(cx);
    let (handle, view) = open_settings_for_test(cx, handle);
    cx.update_window(handle, |_, w, cx| {
        view.update(cx, |v, cx| {
            set(&v.app_secret, "dummy-secret".into(), w, cx);
            set(&v.tapd_token, "dummy-token".into(), w, cx);
        })
    })
    .unwrap();
    let snapshot = cx.read(|cx| model.read(cx).snapshot.clone().unwrap());
    cx.update(|cx| {
        model.update(cx, |m, cx| {
            m.receive(
                Event::State {
                    mode: Mode::Mock,
                    snapshot,
                    profile: "mock".into(),
                    message: "credentials_saved",
                    new_count: 0,
                },
                cx,
            )
        })
    });
    cx.run_until_parked();
    cx.read(|cx| {
        assert!(view.read(cx).app_secret.read(cx).value().is_empty());
        assert!(view.read(cx).tapd_token.read(cx).value().is_empty());
    });
}

#[gpui_kit::test]
fn finder_image_paste_stages_content_without_exposing_local_path(cx: &mut TestAppContext) {
    let (dir, events, model, handle, view) = setup(cx);
    load_task(&events, &model, &view, handle, cx);
    let path = dir.path().join("copied-image.png");
    std::fs::write(&path, png_fixture()).unwrap();
    let item = ClipboardItem {
        entries: vec![ClipboardEntry::ExternalPaths(ExternalPaths(
            vec![path.clone()].into(),
        ))],
    };
    cx.update_window(handle, |_, w, cx| {
        let input = view.read(cx).description.clone();
        input.update(cx, |s, cx| s.focus(w, cx));
        cx.write_to_clipboard(item);
        w.press("cmd-v", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(view.read(cx).pending_files, 1));
    click(cx, handle, "submit");
    cx.read(|cx| {
        assert!(
            !model.read(cx).activities.contains("submit"),
            "pending file imports must prevent submission"
        )
    });
    pump(&events, &model, cx, |cx| {
        view.read(cx).attachments.len() == 1 && view.read(cx).pending_files == 0
    });
    cx.read(|cx| {
        let text = view.read(cx).description.read(cx).value();
        assert!(text.contains("attachment:"));
        assert!(!text.contains(path.to_string_lossy().as_ref()));
    });
}

#[gpui_kit::test]
fn pending_paste_reserves_capacity_against_file_batches(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    load_task(&events, &model, &view, handle, cx);
    let marker = "![pending](attachment:reserved)".to_owned();
    cx.update_window(handle, |_, w, cx| {
        view.update(cx, |v, cx| {
            v.pending_pastes.insert(marker.clone());
            v.description.update(cx, |s, cx| {
                s.set_text(marker.clone(), cx);
                s.focus(w, cx);
            });
        })
    })
    .unwrap();
    let paths: Vec<PathBuf> = (0..10)
        .map(|i| PathBuf::from(format!("/not-read/file-{i}")))
        .collect();
    cx.update_window(handle, |_, w, cx| {
        cx.write_to_clipboard(ClipboardItem {
            entries: vec![ClipboardEntry::ExternalPaths(ExternalPaths(paths.into()))],
        });
        w.press("cmd-v", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        assert_eq!(view.read(cx).pending_files, 0);
        assert!(view.read(cx).pending_pastes.contains(&marker));
        assert_eq!(view.read(cx).description.read(cx).value().as_ref(), marker);
        assert!(model.read(cx).errors.contains_key("attachments"));
    });
}

#[gpui_kit::test]
fn attachment_or_body_failure_is_attention_not_synced(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    load_task(&events, &model, &view, handle, cx);
    let mut record = cx.read(|cx| model.read(cx).snapshot.as_ref().unwrap().records[0].clone());
    record.baseline = Some(record.source.clone());
    record.target = Some(lark2tapd_core::TargetTask {
        id: "target".into(),
        url: String::new(),
        kind: "story".into(),
        title: "target".into(),
        description: String::new(),
        owner: String::new(),
        due: None,
    });
    record.media_body_pending = true;
    assert!(Filter::Changed.matches(&record));
    assert!(!Filter::Synced.matches(&record));
    record.media_body_pending = false;
    let mut draft = cx.read(|cx| view.read(cx).draft(cx));
    draft.attachments.push(DraftAttachment {
        id: "file".into(),
        name: "file.txt".into(),
        path: "unused".into(),
        size: 1,
        is_image: false,
        remote_id: None,
        remote_url: None,
        uncertain: true,
    });
    record.last_draft = Some(draft);
    assert!(Filter::Changed.matches(&record));
    assert!(!Filter::Synced.matches(&record));
}

#[gpui_kit::test]
fn workspace_form_requires_check_for_exact_current_fields(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, _view) = setup(cx);
    let (handle, _) = open_settings_for_test(cx, handle);
    cx.update_window(handle, |_, w, cx| {
        w.scroll(
            "lark-cli-help",
            ScrollDelta::Pixels(point(px(0.), px(-1100.))),
            cx,
        );
    })
    .unwrap();
    click(cx, handle, "add-workspace");
    cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        w.click("manual-project-id", cx);
        w.input("demo-2", cx);
        w.click("manual-project-name", cx);
        w.input("成员空间", cx);
    })
    .unwrap();
    click(cx, handle, "check-workspace");
    pump(&events, &model, cx, |cx| {
        model
            .read(cx)
            .checked_workspace
            .as_ref()
            .is_some_and(|w| w.name == "成员空间")
    });
    cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        w.click("manual-project-name", cx);
        w.press("cmd-a", cx);
        w.input("更改后的空间名", cx);
    })
    .unwrap();
    click(cx, handle, "confirm-workspace");
    cx.read(|cx| {
        assert!(
            model
                .read(cx)
                .snapshot
                .as_ref()
                .unwrap()
                .directory
                .manual_workspaces
                .is_empty()
        )
    });
    click(cx, handle, "check-workspace");
    pump(&events, &model, cx, |cx| {
        model
            .read(cx)
            .checked_workspace
            .as_ref()
            .is_some_and(|w| w.name == "更改后的空间名")
    });
    click(cx, handle, "confirm-workspace");
    pump(&events, &model, cx, |cx| {
        model
            .read(cx)
            .snapshot
            .as_ref()
            .unwrap()
            .directory
            .manual_workspaces
            .iter()
            .any(|w| w.id == "demo-2" && w.name == "更改后的空间名")
    });
}

#[gpui_kit::test]
fn cli_test_buttons_remain_clickable_while_busy_and_paths_are_readonly(cx: &mut TestAppContext) {
    let (_dir, _events, model, handle, _view) = setup(cx);
    let (handle, view) = open_settings_for_test(cx, handle);
    model.update(cx, |m, cx| {
        m.busy = true;
        cx.notify();
    });
    let original = cx.read(|cx| view.read(cx).lark_path.read(cx).value());
    cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        w.click("lark-path", cx);
        w.press("cmd-a", cx);
        w.input("must-not-change", cx);
    })
    .unwrap();
    cx.read(|cx| assert_eq!(view.read(cx).lark_path.read(cx).value(), original));
    click(cx, handle, "check-lark");
    cx.read(|cx| {
        assert!(
            model
                .read(cx)
                .lark_diagnostic
                .contains("未执行真实连接测试")
        )
    });
}

#[gpui_kit::test]
fn segmented_status_switches_scope_and_settings_returns_to_tasks(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    load_task(&events, &model, &view, handle, cx);
    for (index, expected) in [
        (1usize, Filter::Changed),
        (2, Filter::Synced),
        (0, Filter::Pending),
    ] {
        cx.update_window(handle, |_, w, cx| {
            w.render_frame(cx);
            w.within("task-status").click(index, cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.read(|cx| assert!(view.read(cx).filter == expected));
    }
    let mut record = cx.read(|cx| model.read(cx).snapshot.as_ref().unwrap().records[0].clone());
    record.uncertain = true;
    assert!(Filter::Changed.matches(&record));
    assert!(!Filter::Pending.matches(&record));
    record.in_scope = false;
    for filter in [Filter::Pending, Filter::Changed, Filter::Synced] {
        assert!(!filter.matches(&record));
    }
    let (settings, _) = open_settings_for_test(cx, handle);
    close_test_window(cx, settings);
    cx.read(|cx| assert!(view.read(cx).filter == Filter::Pending));
}

#[gpui_kit::test]
fn task_card_summary_area_and_keyboard_activate_same_task(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    let guid = load_task(&events, &model, &view, handle, cx);
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.selected = None;
            cx.notify();
        })
    });
    cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        // 点击卡片左侧下方摘要区域，而不是标题文本。
        let id = SharedString::from(format!("select-{guid}"));
        let bounds = w.find(id.clone()).bounds();
        w.click_at(id, point(px(12.), bounds.size.height - px(12.)), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(view.read(cx).selected.as_ref(), Some(&guid)));
    pump(&events, &model, cx, |cx| !model.read(cx).busy);
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.selected = None;
            cx.notify();
        })
    });
    cx.update_window(handle, |_, w, cx| {
        view.update(cx, |v, cx| v.search.update(cx, |s, cx| s.focus(w, cx)));
        w.render_frame(cx);
        w.press("tab", cx);
        w.press("space", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(view.read(cx).selected.as_ref(), Some(&guid)));
}

#[path = "editor_tests.rs"]
mod native_editor;
#[path = "shell_tests.rs"]
mod shell_tests;
