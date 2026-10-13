//! 布局回归使用真实控件边界，防止折叠区再次按按钮文字收缩。
use super::*;

#[gpui_kit::test]
fn expanded_source_trigger_spans_the_detail_content(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    load_task(&events, &model, &view, handle, cx);
    click(cx, handle, "toggle-source-details");
    // 长正文在最小窗口内也不能把背景撑出可用内容区。
    model.update(cx, |m, cx| {
        m.snapshot.as_mut().unwrap().records[0].source.description =
            "长描述需要在详情区域内正确换行。".repeat(40);
        cx.notify();
    });
    cx.simulate_window_resize(handle, size(px(920.), px(640.)));
    cx.run_until_parked();
    cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        let trigger = w.find("toggle-source-details").bounds();
        let title = w.find("tapd-title-group").bounds();
        assert!(
            (trigger.size.width - title.size.width).abs() < px(2.),
            "trigger {trigger:?}, title {title:?}"
        );
        assert!((trigger.left() - title.left()).abs() < px(2.));
    })
    .unwrap();
    let panel = VisualTestContext::from_window(handle, cx)
        .debug_bounds("source-detail-panel")
        .unwrap();
    let trigger = cx
        .update_window(handle, |_, w, _| w.find("toggle-source-details").bounds())
        .unwrap();
    assert!(
        (panel.size.width - trigger.size.width).abs() < px(2.),
        "panel {panel:?}, trigger {trigger:?}"
    );
    assert!(panel.right() <= px(920.));
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
