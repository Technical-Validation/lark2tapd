//! Host boundary regressions for the lark2tapd patch.
use super::*;
use gpui::TestAppContext;

#[gpui::test]
fn loading_preserves_source(cx: &mut TestAppContext) {
    let window = cx.add_window(|w, cx| EditorState::new(w, cx).with_text("before $$x$$ after"));
    window
        .update(cx, |editor, _, cx| {
            assert_eq!(editor.text(), "before $$x$$ after");
            editor.set_text("before $$y$$ after", cx);
            assert_eq!(editor.text(), "before $$y$$ after");
        })
        .unwrap();
}

#[gpui::test]
fn composition_selection_is_relative_to_inserted_text(cx: &mut TestAppContext) {
    let window = cx.add_window(|w, cx| EditorState::new(w, cx).with_text("中文"));
    window
        .update(cx, |editor, w, cx| {
            editor.selected_range = 6..6;
            editor.replace_and_mark_text_in_range(None, "ni", Some(2..2), w, cx);
            assert_eq!(editor.selected_range, 8..8);
            editor.replace_and_mark_text_in_range(None, "😀", Some(1..99), w, cx);
            assert_eq!(editor.selected_range, 10..10);
        })
        .unwrap();
}

#[gpui::test]
fn composition_undo_restores_original_selection(cx: &mut TestAppContext) {
    let window = cx.add_window(|w, cx| EditorState::new(w, cx).with_text("中文😀尾"));
    window
        .update(cx, |editor, w, cx| {
            editor.selected_range = 6..10;
            editor.selection_reversed = true;
            editor.replace_and_mark_text_in_range(None, "n", Some(1..1), w, cx);
            editor.replace_and_mark_text_in_range(None, "ni", Some(2..2), w, cx);
            editor.replace_text_in_range(None, "你", w, cx);
            editor.undo(&Undo, w, cx);
            assert_eq!(editor.text(), "中文😀尾");
            assert_eq!(editor.selected_range, 6..10);
            assert!(editor.selection_reversed);
            editor.redo(&Redo, w, cx);
            assert_eq!(editor.text(), "中文你尾");
        })
        .unwrap();
}

#[gpui::test]
fn cancelled_composition_restores_selected_emoji(cx: &mut TestAppContext) {
    let window = cx.add_window(|w, cx| EditorState::new(w, cx).with_text("中😀尾"));
    window
        .update(cx, |e, w, cx| {
            e.selected_range = 3..7;
            e.selection_reversed = true;
            e.replace_and_mark_text_in_range(None, "ni", Some(2..2), w, cx);
            e.replace_and_mark_text_in_range(None, "", None, w, cx);
            assert_eq!(e.text(), "中😀尾");
            assert_eq!(e.selected_range, 3..7);
            assert!(e.selection_reversed);
            assert!(e.undo_stack.is_empty());
        })
        .unwrap();
}

#[gpui::test]
fn every_programmatic_edit_and_history_change_emits_changed(cx: &mut TestAppContext) {
    let window = cx.add_window(EditorState::new);
    let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let observed = events.clone();
    window
        .update(cx, |_, _, cx| {
            cx.subscribe(&cx.entity(), move |_, _, event: &EditorEvent, _| {
                observed.borrow_mut().push(event.clone())
            })
            .detach();
        })
        .unwrap();
    window
        .update(cx, |e, w, cx| {
            e.set_text("start", cx);
            e.replace_range(5..5, "!", cx);
            e.undo(&Undo, w, cx);
            e.redo(&Redo, w, cx);
        })
        .unwrap();
    assert_eq!(
        *events.borrow(),
        vec![
            EditorEvent::Changed {
                origin: ChangeOrigin::Host
            },
            EditorEvent::Changed {
                origin: ChangeOrigin::Host
            },
            EditorEvent::Changed {
                origin: ChangeOrigin::User
            },
            EditorEvent::Changed {
                origin: ChangeOrigin::User
            },
        ]
    );
}

#[gpui::test]
fn image_with_text_never_falls_back_to_text(cx: &mut TestAppContext) {
    let window = cx.add_window(|w, cx| EditorState::new(w, cx).with_text("original"));
    window
        .update(cx, |e, w, cx| {
            let image = gpui::Image::from_bytes(gpui::ImageFormat::Png, vec![1, 2, 3]);
            cx.write_to_clipboard(ClipboardItem {
                entries: vec![
                    gpui::ClipboardEntry::Image(image),
                    gpui::ClipboardEntry::String(gpui::ClipboardString::new("fallback".into())),
                ],
            });
            e.paste(&Paste, w, cx);
            assert_eq!(e.text(), "original");
        })
        .unwrap();
}

#[gpui::test]
fn placeholder_resolution_maps_unicode_selection_and_history(cx: &mut TestAppContext) {
    let window = cx.add_window(|w, cx| EditorState::new(w, cx).with_text("中[p]😀[p]尾"));
    window
        .update(cx, |e, w, cx| {
            e.selected_range = 6..13;
            e.selection_reversed = true;
            e.replace_range(16..16, "!", cx);
            e.selected_range = 6..13;
            e.selection_reversed = true;
            assert!(e.resolve_placeholder("[p]", "图", cx));
            assert_eq!(e.text(), "中图😀图尾!");
            assert_eq!(e.selected_range, 6..13);
            assert!(e.selection_reversed);
            e.undo(&Undo, w, cx);
            assert_eq!(e.text(), "中图😀图尾");
            assert_eq!(e.selected_range, 6..13);
            assert!(e.selection_reversed);
            e.redo(&Redo, w, cx);
            assert_eq!(e.text(), "中图😀图尾!");
        })
        .unwrap();
}

#[gpui::test]
fn deleted_placeholder_is_not_reinserted_or_resurrected_by_undo(cx: &mut TestAppContext) {
    let window = cx.add_window(|w, cx| EditorState::new(w, cx).with_text("中[p]😀尾"));
    window
        .update(cx, |e, w, cx| {
            e.replace_range(3..6, "", cx);
            assert!(e.resolve_placeholder("[p]", "![图](asset)", cx));
            assert_eq!(e.text(), "中😀尾");
            e.undo(&Undo, w, cx);
            assert_eq!(e.text(), "中![图](asset)😀尾");
            e.redo(&Redo, w, cx);
            assert_eq!(e.text(), "中😀尾");
        })
        .unwrap();
}

#[gpui::test]
fn resolution_cleans_redo_without_adding_an_undo_step(cx: &mut TestAppContext) {
    let window = cx.add_window(|w, cx| EditorState::new(w, cx).with_text("😀"));
    window
        .update(cx, |e, w, cx| {
            e.replace_range(4..4, "[p]", cx);
            e.undo(&Undo, w, cx);
            assert!(e.resolve_placeholder("[p]", "图片", cx));
            assert_eq!(e.text(), "😀");
            assert!(e.undo_stack.is_empty());
            e.redo(&Redo, w, cx);
            assert_eq!(e.text(), "😀图片");
            assert_eq!(e.selected_range, 10..10);
        })
        .unwrap();
}

#[gpui::test]
fn placeholder_waits_for_composition_and_append_preserves_selection(cx: &mut TestAppContext) {
    let window = cx.add_window(|w, cx| EditorState::new(w, cx).with_text("中[p]😀尾"));
    window
        .update(cx, |e, w, cx| {
            e.selected_range = 6..10;
            e.selection_reversed = true;
            e.replace_and_mark_text_in_range(None, "ni", Some(2..2), w, cx);
            assert!(e.is_composing());
            assert!(!e.resolve_placeholder("[p]", "图片", cx));
            e.append_text("ignored", cx);
            assert_eq!(e.text(), "中[p]ni尾");
            e.replace_text_in_range(None, "你", w, cx);
            assert!(!e.is_composing());
            e.selected_range = 6..9;
            e.selection_reversed = true;
            assert!(e.resolve_placeholder("[p]", "图片", cx));
            assert_eq!(e.selected_range, 9..12);
            assert!(e.selection_reversed);
            e.append_text("!", cx);
            assert_eq!(e.text(), "中图片你尾!");
            assert_eq!(e.selected_range, 9..12);
            assert!(e.selection_reversed);
            e.undo(&Undo, w, cx);
            assert_eq!(e.text(), "中图片你尾");
            e.undo(&Undo, w, cx);
            assert_eq!(e.text(), "中图片😀尾");
            assert_eq!(e.selected_range, 9..13);
        })
        .unwrap();
}

#[gpui::test]
fn media_events_are_delivered_after_editor_borrow_and_keep_payload(cx: &mut TestAppContext) {
    let window = cx.add_window(|w, cx| EditorState::new(w, cx).with_text("original"));
    let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let observed = events.clone();
    window
        .update(cx, |_, _, cx| {
            cx.subscribe(&cx.entity(), move |e, _, event: &EditorEvent, cx| {
                if let EditorEvent::PasteMedia(item) = event {
                    observed.borrow_mut().push(item.clone());
                    // A host can edit in response without re-entering the paste borrow.
                    e.append_text("!", cx);
                }
            })
            .detach();
        })
        .unwrap();
    let image = ClipboardItem::new_image(&gpui::Image::from_bytes(gpui::ImageFormat::Png, vec![1]));
    let paths = ClipboardItem {
        entries: vec![
            gpui::ClipboardEntry::ExternalPaths(gpui::ExternalPaths(
                vec!["/tmp/picture.png".into()].into(),
            )),
            gpui::ClipboardEntry::String(gpui::ClipboardString::new("fallback".into())),
        ],
    };
    for item in [&image, &paths] {
        window
            .update(cx, |e, w, cx| {
                let before = e.text().to_owned();
                cx.write_to_clipboard(item.clone());
                e.paste(&Paste, w, cx);
                assert_eq!(e.text(), before);
            })
            .unwrap();
    }
    assert_eq!(*events.borrow(), vec![image, paths]);
    window
        .update(cx, |e, w, cx| {
            assert_eq!(e.text(), "original!!");
            e.selected_range = 10..10;
            cx.write_to_clipboard(ClipboardItem::new_string("\r\ntext".into()));
            e.paste(&Paste, w, cx);
            assert_eq!(e.text(), "original!!\ntext");
        })
        .unwrap();
}

#[gpui::test]
fn composition_matching_original_still_records_final_commit(cx: &mut TestAppContext) {
    let window = cx.add_window(|w, cx| EditorState::new(w, cx).with_text("你"));
    window
        .update(cx, |e, w, cx| {
            e.selected_range = 0..3;
            e.replace_and_mark_text_in_range(None, "你", Some(1..1), w, cx);
            e.replace_text_in_range(None, "泥", w, cx);
            e.undo(&Undo, w, cx);
            assert_eq!(e.text(), "你");
            assert_eq!(e.selected_range, 0..3);
        })
        .unwrap();
}

#[gpui::test]
fn unmark_commits_once_and_undo_during_composition_cancels(cx: &mut TestAppContext) {
    let window = cx.add_window(|w, cx| EditorState::new(w, cx).with_text("中"));
    window
        .update(cx, |e, w, cx| {
            e.selected_range = 3..3;
            e.replace_and_mark_text_in_range(None, "ni", None, w, cx);
            e.unmark_text(w, cx);
            assert!(!e.is_composing());
            assert_eq!(e.undo_stack.len(), 1);
            e.undo(&Undo, w, cx);
            assert_eq!(e.text(), "中");
            e.redo(&Redo, w, cx);
            assert_eq!(e.text(), "中ni");
            e.replace_and_mark_text_in_range(None, "😀", None, w, cx);
            e.undo(&Undo, w, cx);
            assert_eq!(e.text(), "中ni");
            assert!(!e.is_composing());
        })
        .unwrap();
}

#[gpui::test]
fn append_is_a_separate_undo_step_and_keeps_end_caret(cx: &mut TestAppContext) {
    let window = cx.add_window(EditorState::new);
    window
        .update(cx, |e, w, cx| {
            e.replace_text_in_range(None, "中", w, cx);
            e.append_text("😀", cx);
            assert_eq!(e.text(), "中😀");
            assert_eq!(e.selected_range, 3..3);
            e.undo(&Undo, w, cx);
            assert_eq!(e.text(), "中");
            assert_eq!(e.selected_range, 3..3);
            e.undo(&Undo, w, cx);
            assert_eq!(e.text(), "");
        })
        .unwrap();
}

#[gpui::test]
fn redo_cancels_preedit_before_recording_stable_history(cx: &mut TestAppContext) {
    let window = cx.add_window(|w, cx| EditorState::new(w, cx).with_text("中😀尾"));
    window
        .update(cx, |e, w, cx| {
            e.selected_range = 3..7;
            e.selection_reversed = true;
            e.replace_range(3..7, "好", cx);
            e.undo(&Undo, w, cx);
            e.replace_and_mark_text_in_range(None, "ni", Some(2..2), w, cx);
            e.redo(&Redo, w, cx);
            assert_eq!(e.text(), "中好尾");
            assert!(!e.is_composing());
            e.undo(&Undo, w, cx);
            assert_eq!(e.text(), "中😀尾");
            assert_eq!(e.selected_range, 3..7);
            assert!(e.selection_reversed);
            assert!(!e.is_composing());
            e.redo(&Redo, w, cx);
            assert_eq!(e.text(), "中好尾");
        })
        .unwrap();
}

#[gpui::test]
fn redo_without_history_cancels_preedit_and_restores_selection(cx: &mut TestAppContext) {
    let window = cx.add_window(|w, cx| EditorState::new(w, cx).with_text("中😀尾"));
    window
        .update(cx, |e, w, cx| {
            e.selected_range = 3..7;
            e.selection_reversed = true;
            e.replace_and_mark_text_in_range(None, "ni", Some(2..2), w, cx);
            e.redo(&Redo, w, cx);
            assert_eq!(e.text(), "中😀尾");
            assert_eq!(e.selected_range, 3..7);
            assert!(e.selection_reversed);
            assert!(!e.is_composing());
            assert!(e.undo_stack.is_empty());
            assert!(e.redo_stack.is_empty());
        })
        .unwrap();
}

#[gpui::test]
fn change_origin_separates_async_host_edits_from_user_actions(cx: &mut TestAppContext) {
    let window = cx.add_window(EditorState::new);
    let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let observed = events.clone();
    window
        .update(cx, |_, _, cx| {
            cx.subscribe(&cx.entity(), move |_, _, event: &EditorEvent, _| {
                if let EditorEvent::Changed { origin } = event {
                    observed.borrow_mut().push(*origin);
                }
            })
            .detach();
        })
        .unwrap();
    window
        .update(cx, |e, w, cx| {
            e.set_text("tag:: [pending]\nbody", cx);
            e.resolve_placeholder("[pending]", "图片", cx);
            e.append_text("!", cx);
            // This menu action shares the range editing implementation with host edits.
            e.delete_property_row(0, cx);
            e.replace_text_in_range(None, "中", w, cx);
            e.replace_and_mark_text_in_range(None, "ni", None, w, cx);
            e.replace_and_mark_text_in_range(None, "", None, w, cx);
        })
        .unwrap();
    assert_eq!(
        *events.borrow(),
        vec![
            ChangeOrigin::Host,
            ChangeOrigin::Host,
            ChangeOrigin::Host,
            ChangeOrigin::User,
            ChangeOrigin::User,
            ChangeOrigin::User,
            ChangeOrigin::User,
        ]
    );
}
