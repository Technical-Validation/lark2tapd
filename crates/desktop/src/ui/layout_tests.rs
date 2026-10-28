//! 布局回归使用真实控件边界，验证紧凑信息区和内容可见性。
use super::*;

// 三项来源信息默认可见；长正文不再把基本信息区撑高。
#[gpui_kit::test]
fn compact_source_metadata_is_always_visible(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    load_task(&events, &model, &view, handle, cx);
    model.update(cx, |model, cx| {
        model.snapshot.as_mut().unwrap().records[0]
            .source
            .description = "长正文".repeat(400);
        cx.notify();
    });
    for width in [1440., 920.] {
        cx.simulate_window_resize(handle, size(px(width), px(640.)));
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("toggle-source-details").is_none());
        })
        .unwrap();
        let mut visual = VisualTestContext::from_window(handle, cx);
        let metadata = visual
            .debug_bounds("source-detail-panel")
            .expect("always-visible metadata");
        assert!(
            metadata.size.height <= px(60.),
            "metadata must be compact: {metadata:?}"
        );
        assert!(metadata.right() <= px(width));
        for id in ["source-creator", "source-assignees", "source-created"] {
            let field = visual.debug_bounds(id).expect("required metadata");
            assert!(field.top() >= metadata.top() && field.bottom() <= metadata.bottom());
        }
        for id in ["source-updated", "source-followers", "source-body"] {
            assert!(visual.debug_bounds(id).is_none());
        }
    }
}

// 长简介、普通详情和消息来源必须同时保留，不能挤掉元数据或 TAPD 编辑区。
#[gpui_kit::test]
fn visible_source_keeps_full_title_messages_and_required_metadata(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    load_task(&events, &model, &view, handle, cx);
    let draft = cx.read(|cx| view.read(cx).draft(cx));
    model.update(cx, |m, cx| {
        let source = &mut m.snapshot.as_mut().unwrap().records[0].source;
        source.title = "完整任务简介\n\n".repeat(12);
        source.description = "## 任务详情\n\n**正文**\n\n- 检查项".into();
        source.metadata.source_messages = vec![
            lark2tapd_core::SourceMessage {
                message_id: "om_later".into(),
                content: "## 来源消息\n\n消息正文".into(),
            },
            lark2tapd_core::SourceMessage {
                message_id: "om_earlier".into(),
                content: String::new(),
            },
            lark2tapd_core::SourceMessage {
                message_id: "7684199632590769140".into(),
                content: "没有发送时间的消息".into(),
            },
        ];
        for (id, time) in [
            ("om_later", "1735689602000"),
            ("om_earlier", "1735689600000"),
        ] {
            source.metadata.message_details.insert(
                id.into(),
                lark2tapd_core::SourceMessageDetails {
                    sender: Some(lark2tapd_core::SourcePerson {
                        id: "ou_sender".into(),
                        name: String::new(),
                    }),
                    sent_at: Some(time.into()),
                },
            );
        }
        source
            .metadata
            .resolved_names
            .insert("ou_sender".into(), "发送人姓名".into());
        cx.notify();
    });
    cx.update_window(handle, |_, w, cx| w.render_frame(cx))
        .unwrap();
    let mut visual = VisualTestContext::from_window(handle, cx);
    let summary = visual.debug_bounds("source-summary").expect("full summary");
    assert!(
        summary.size.height > px(100.),
        "summary must not be line-clamped"
    );
    for selector in [
        "source-creator",
        "source-created",
        "source-assignees",
        "source-message-0",
        "source-message-1",
        "source-message-2",
        "source-message-header-om_later",
        "source-message-header-om_earlier",
        "source-message-header-7684199632590769140",
    ] {
        assert!(
            visual.debug_bounds(selector).is_some(),
            "missing {selector}"
        );
    }
    let early = visual
        .debug_bounds("source-message-header-om_earlier")
        .unwrap();
    let late = visual
        .debug_bounds("source-message-header-om_later")
        .unwrap();
    let unknown = visual
        .debug_bounds("source-message-header-7684199632590769140")
        .unwrap();
    assert!(early.top() < late.top() && late.top() < unknown.top());
    cx.read(|cx| assert_eq!(view.read(cx).draft(cx), draft));
}

#[gpui_kit::test]
fn relative_clock_refreshes_without_polling_and_drops_with_window(cx: &mut TestAppContext) {
    let (_dir, _events, model, handle, view) = setup(cx);
    cx.update_window(handle, |_, w, cx| w.render_frame(cx))
        .unwrap();
    let clock = cx.read(|cx| view.read(cx).last_check.clone());
    let ticks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = ticks.clone();
    let _subscription = cx.update(|cx| {
        cx.observe(&clock, move |_, _| {
            observed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        })
    });
    cx.run_until_parked();
    cx.background_executor
        .advance_clock(Duration::from_secs(15));
    cx.run_until_parked();
    assert_eq!(ticks.load(std::sync::atomic::Ordering::Relaxed), 1);
    cx.read(|cx| {
        assert!(model.read(cx).activities.is_empty());
        assert!(
            model
                .read(cx)
                .snapshot
                .as_ref()
                .unwrap()
                .last_checked
                .is_none()
        );
    });
    let weak = clock.downgrade();
    drop(clock);
    drop(view);
    close_test_window(cx, handle);
    assert!(
        weak.upgrade().is_none(),
        "clock must not keep the closed window alive"
    );
}

#[gpui_kit::test]
fn identity_lives_in_the_top_bar_above_search(cx: &mut TestAppContext) {
    let (_dir, _events, model, handle, _view) = setup(cx);
    for language in ["zh-CN", "en"] {
        model.update(cx, |m, cx| {
            m.snapshot.as_mut().unwrap().settings.language = language.into();
            cx.notify();
        });
        for width in [1440., 1180., 920.] {
            cx.simulate_window_resize(handle, size(px(width), px(640.)));
            cx.run_until_parked();
            cx.update_window(handle, |_, w, cx| w.render_frame(cx))
                .unwrap();
            let mut visual = VisualTestContext::from_window(handle, cx);
            let brand = visual.debug_bounds("app-identity").unwrap();
            let time = visual.debug_bounds("last-check").unwrap();
            cx.update_window(handle, |_, w, _| {
                let gear = w.find("nav-settings").bounds();
                let search = w.find("task-search").bounds();
                assert!(
                    brand.top() < gear.bottom() && brand.bottom() > gear.top(),
                    "brand must share the top bar with settings: {brand:?}, {gear:?}"
                );
                assert!(brand.bottom() < search.top());
                assert!(
                    brand.right() < time.left(),
                    "{language} {width}: {brand:?} overlaps {time:?}"
                );
                assert!(time.right() < gear.left());
                assert!(gear.right() <= px(width));
            })
            .unwrap();
        }
    }
}

#[gpui_kit::test]
fn synced_icon_is_reserved_for_synced_task_details(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    load_task(&events, &model, &view, handle, cx);
    cx.update_window(handle, |_, w, cx| w.render_frame(cx))
        .unwrap();
    assert!(
        VisualTestContext::from_window(handle, cx)
            .debug_bounds("synced-icon")
            .is_none(),
        "unimported details and filters must not show a synced icon"
    );
    click(cx, handle, "submit");
    pump(&events, &model, cx, |cx| {
        let m = model.read(cx);
        !m.busy && m.snapshot.as_ref().unwrap().records[0].target.is_some()
    });
    cx.update_window(handle, |_, w, cx| w.render_frame(cx))
        .unwrap();
    assert!(
        VisualTestContext::from_window(handle, cx)
            .debug_bounds("synced-icon")
            .is_some()
    );
}
