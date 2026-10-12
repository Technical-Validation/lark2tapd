//! 原生正文接入的真实 GPUI 事件回归；后端沿用临时目录与 mock 网关。
use super::*;

// 帧回调也必须执行，否则后台回填触发的滚动会被测试漏掉。
#[gpui_kit::test]
fn async_media_preserves_reading_viewport(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    let guid = load_task(&events, &model, &view, handle, cx);
    let session = cx.read(|cx| view.read(cx).editor_session);
    let root = cx.read(|cx| worker::media_root(&model.read(cx).data_root, "mock").unwrap());
    let attachment = lark2tapd_core::save_pasted_image(&root, &png_fixture(), "png").unwrap();
    let marker = "![loading](attachment:pending-scroll)".to_owned();
    cx.update_window(handle, |_, w, cx| {
        view.update(cx, |v, cx| {
            v.pending_pastes.insert(marker.clone());
            v.description.update(cx, |s, cx| {
                s.set_text(format!("{marker}\n{}", "line of text\n".repeat(200)), cx);
                s.set_cursor(s.text().len(), cx);
                s.focus(w, cx);
            });
        });
        w.render_frame(cx);
        w.simulate_next_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        w.simulate_next_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, w, cx| {
        view.read(cx)
            .description_scroll
            .set_offset(point(px(0.), px(-500.)));
        w.render_frame(cx);
        w.simulate_next_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    let before = cx.read(|cx| view.read(cx).description_scroll.offset());
    cx.update_window(handle, |_, w, cx| {
        view.update(cx, |v, cx| {
            v.accept_attachment(
                &MediaReady {
                    guid,
                    profile: "mock".into(),
                    session,
                    marker: Some(marker),
                    attachment,
                },
                w,
                cx,
            )
        });
        w.render_frame(cx);
        w.simulate_next_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        w.simulate_next_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    let after = cx.read(|cx| view.read(cx).description_scroll.offset());
    // 允许上方图片尺寸改变的小量补偿，不允许跳到200行以后的光标。
    assert!(
        (after.y - before.y).abs() < px(300.),
        "async media jumped from {before:?} to {after:?}"
    );
}

// 旧 Textarea 全篇回填会清空历史；此测试在替换实现前已观察到对应失败。
#[gpui_kit::test]
fn attachment_resolution_preserves_edit_history(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    let guid = load_task(&events, &model, &view, handle, cx);
    let session = cx.read(|cx| view.read(cx).editor_session);
    let root = cx.read(|cx| worker::media_root(&model.read(cx).data_root, "mock").unwrap());
    let attachment = lark2tapd_core::save_pasted_image(&root, &png_fixture(), "png").unwrap();
    let marker = "![loading](attachment:pending-history)".to_owned();
    cx.update_window(handle, |_, w, cx| {
        view.update(cx, |v, cx| {
            v.pending_pastes.insert(marker.clone());
            v.description.update(cx, |s, cx| {
                s.set_text(format!("正文\n\n{marker}"), cx);
                s.set_cursor(s.text().len(), cx);
                s.focus(w, cx);
            });
        });
        w.render_frame(cx);
        w.input("后来输入", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, w, cx| {
        view.update(cx, |v, cx| {
            v.accept_attachment(
                &MediaReady {
                    guid,
                    profile: "mock".into(),
                    session,
                    marker: Some(marker.clone()),
                    attachment,
                },
                w,
                cx,
            )
        });
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        w.press("cmd-z", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        let content = view.read(cx).description.read(cx).value();
        assert!(
            !content.contains("pending-history"),
            "undo revived a resolved placeholder"
        );
        assert!(
            !content.contains("后来输入"),
            "attachment completion destroyed the preceding undo step"
        );
        assert!(content.contains("正文"));
    });
}

#[gpui_kit::test]
fn native_description_defers_attachment_resolution_during_ime(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    let guid = load_task(&events, &model, &view, handle, cx);
    let session = cx.read(|cx| view.read(cx).editor_session);
    let root = cx.read(|cx| worker::media_root(&model.read(cx).data_root, "mock").unwrap());
    let attachment = lark2tapd_core::save_pasted_image(&root, &png_fixture(), "png").unwrap();
    let marker = "![loading](attachment:pending-ime)".to_owned();
    cx.update_window(handle, |_, w, cx| {
        view.update(cx, |v, cx| {
            v.pending_pastes.insert(marker.clone());
            v.description.update(cx, |s, cx| {
                s.set_text(format!("{marker}\n中文"), cx);
                s.set_cursor(s.text().len(), cx);
                s.replace_and_mark_text_in_range(None, "ni", Some(2..2), w, cx);
            });
            v.accept_attachment(
                &MediaReady {
                    guid,
                    profile: "mock".into(),
                    session,
                    marker: Some(marker.clone()),
                    attachment,
                },
                w,
                cx,
            );
            assert_eq!(v.deferred_media.len(), 1);
            assert!(v.description.read(cx).is_composing());
            assert!(v.description.read(cx).text().contains(&marker));
            assert!(v.pending_pastes.contains(&marker));
        });
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, w, cx| {
        let editor = view.read(cx).description.clone();
        editor.update(cx, |s, cx| s.replace_text_in_range(None, "你", w, cx));
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        let v = view.read(cx);
        assert!(v.deferred_media.is_empty());
        assert!(v.pending_pastes.is_empty());
        assert_eq!(v.attachments.len(), 1);
        assert!(v.description.read(cx).text().ends_with("中文你"));
        assert!(!v.description.read(cx).text().contains("pending-ime"));
    });
}

#[gpui_kit::test]
fn native_description_cannot_keep_keyboard_focus_when_busy(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    load_task(&events, &model, &view, handle, cx);
    let editor = cx.read(|cx| view.read(cx).description.clone());
    cx.update_window(handle, |_, w, cx| {
        editor.update(cx, |s, cx| {
            s.set_text("保留正文", cx);
            s.focus(w, cx);
        });
    })
    .unwrap();
    model.update(cx, |m, cx| {
        m.busy = true;
        cx.notify();
    });
    cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        assert!(!editor.read(cx).focus_handle(cx).contains_focused(w, cx));
        w.input("不能写入", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(editor.read(cx).text(), "保留正文"));
}
