//! 来源展示和操作使用真实 GPUI 点击/撤销；远端由既有隔离测试网关替代。
use super::*;

#[gpui_kit::test]
fn target_icon_opens_old_mapping_without_api_url(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    load_task(&events, &model, &view, handle, cx);
    model.update(cx, |model, cx| {
        let record = &mut model.snapshot.as_mut().unwrap().records[0];
        record.target_workspace = Some("42".into());
        record.target = Some(lark2tapd_core::TargetTask {
            workitem_type_id: String::new(),
            id: "113000000000000001".into(),
            kind: "task".into(),
            url: String::new(),
            title: "Renamed target".into(),
            description: String::new(),
            owner: String::new(),
            due: None,
        });
        cx.notify();
    });
    click(cx, handle, "open-target");
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://www.tapd.cn/42/prong/tasks/view/113000000000000001")
    );
}

#[gpui_kit::test]
fn source_link_toggle_is_saved_and_restored_per_task(cx: &mut TestAppContext) {
    let (dir, events, model, handle, view) = setup(cx);
    let guid = load_task(&events, &model, &view, handle, cx);
    assert!(cx.read(|cx| view.read(cx).draft(cx).include_source_link));
    click(cx, handle, "include-source-link");
    assert!(!cx.read(|cx| view.read(cx).draft(cx).include_source_link));
    click(cx, handle, "save-draft");
    pump(&events, &model, cx, |cx| {
        model.read(cx).snapshot.as_ref().unwrap().records[0]
            .draft
            .as_ref()
            .is_some_and(|d| !d.include_source_link)
    });
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let engine = lark2tapd_core::Engine::open(&dir.path().join("mock.db"))
            .await
            .unwrap();
        assert!(!engine.prepare(&guid).unwrap().include_source_link);
        assert!(engine.prepare("demo-2").unwrap().include_source_link);
    });
}

// 真实确认框及后台提交；只注入远端读回，以覆盖取消、等待期间编辑和明确覆盖三个分支。
#[gpui_kit::test]
fn conflicting_submission_requires_current_draft_confirmation(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    let guid = load_task(&events, &model, &view, handle, cx);
    click(cx, handle, "submit");
    pump(&events, &model, cx, |cx| {
        model.read(cx).snapshot.as_ref().unwrap().records[0]
            .target
            .is_some()
    });
    let remote = cx.read(|cx| {
        model.read(cx).snapshot.as_ref().unwrap().records[0]
            .target
            .clone()
            .unwrap()
    });
    for mode in ["cancel", "edit", "session", "confirm"] {
        cx.update_window(handle, |_, window, cx| {
            let title = view.read(cx).title.clone();
            set(&title, "New title".into(), window, cx);
        })
        .unwrap();
        let (session, profile) = view.update(cx, |view, cx| {
            view.pending_submission = Some((view.draft(cx), view.edit_source.clone().unwrap()));
            (view.editor_session, view.edit_profile.clone())
        });
        model.update(cx, |model, cx| {
            // 新的提交会清除上一轮表单错误；注入预检响应时保持同样的入口条件。
            model.errors.remove("submit");
            model.snapshot.as_mut().unwrap().records[0]
                .target
                .as_mut()
                .unwrap()
                .title = "Old baseline".into();
            model.receive(
                Event::RemoteTarget {
                    guid: guid.clone(),
                    profile,
                    session,
                    target: remote.clone(),
                },
                cx,
            );
        });
        cx.run_until_parked();
        let before = cx.read(|cx| model.read(cx).next_request_sequence);
        if mode == "edit" {
            cx.update_window(handle, |_, window, cx| {
                let title = view.read(cx).title.clone();
                set(&title, "Changed while confirming".into(), window, cx);
            })
            .unwrap();
        } else if mode == "session" {
            view.update(cx, |view, _| {
                view.editor_session = editor_session();
                view.pending_submission = None;
            });
        }
        cx.simulate_prompt_answer(if mode == "cancel" {
            "取消"
        } else {
            "覆盖并更新"
        });
        cx.run_until_parked();
        if mode != "confirm" {
            assert_eq!(
                cx.read(|cx| model.read(cx).next_request_sequence),
                before,
                "{mode} must not submit"
            );
        } else {
            pump(&events, &model, cx, |cx| {
                model.read(cx).snapshot.as_ref().unwrap().records[0]
                    .target
                    .as_ref()
                    .is_some_and(|target| target.title == "New title")
            });
            assert_eq!(
                cx.read(|cx| model.read(cx).snapshot.as_ref().unwrap().records[0]
                    .target
                    .as_ref()
                    .unwrap()
                    .id
                    .clone()),
                remote.id
            );
        }
    }
}

#[gpui_kit::test]
fn task_cards_sort_by_creation_time_with_missing_dates_last(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    let selected = load_task(&events, &model, &view, handle, cx);
    model.update(cx, |model, cx| {
        let records = &mut model.snapshot.as_mut().unwrap().records;
        records[0].source.metadata.created_at = Some("1735689600000".into());
        records[1].source.metadata.created_at = Some("1735776000000".into());
        records[2].source.metadata.created_at = None;
        cx.notify();
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.find("select-demo-2").bounds().top()
                < window.find("select-demo-1").bounds().top()
        );
        assert!(
            window.find("select-demo-1").bounds().top()
                < window.find("select-demo-3").bounds().top()
        );
    })
    .unwrap();
    cx.read(|cx| assert_eq!(view.read(cx).selected.as_deref(), Some(selected.as_str())));
}

#[gpui_kit::test]
fn check_failure_remains_clickable_until_a_check_succeeds(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    load_task(&events, &model, &view, handle, cx);
    model.update(cx, |model, cx| {
        model.receive(
            Event::FormError {
                field: "tasks",
                message: "Lark request failed: test detail".into(),
            },
            cx,
        );
        model.receive(Event::Error("Lark request failed: test detail".into()), cx);
        model.receive(
            Event::Activity {
                key: "tasks".into(),
                active: false,
            },
            cx,
        );
        model.receive(
            Event::State {
                mode: Mode::Mock,
                snapshot: model.snapshot.clone().unwrap(),
                profile: model.profile.clone(),
                message: "settings_saved",
                new_count: 0,
            },
            cx,
        );
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("check-error").label(), Some("检查失败"));
    })
    .unwrap();
    click(cx, handle, "check-error");
    cx.update_window(handle, |_, window, cx| {
        assert_eq!(window.notifications(cx).len(), 1)
    })
    .unwrap();
    cx.update_window(handle, |_, window, cx| {
        let note = window.notifications(cx)[0].clone();
        note.update(cx, |note, cx| note.dismiss(window, cx));
    })
    .unwrap();
    cx.run_until_parked();
    cx.background_executor.advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    cx.read(|cx| {
        assert!(
            model.read(cx).pending_error.is_none(),
            "dismissed details must not replay on reopen"
        );
        assert!(
            model.read(cx).errors.contains_key("tasks"),
            "check status still requires a successful check"
        );
    });
    model.update(cx, |model, cx| {
        model.receive(
            Event::Activity {
                key: "tasks".into(),
                active: true,
            },
            cx,
        )
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("check-error").is_none());
    })
    .unwrap();
    assert!(
        VisualTestContext::from_window(handle, cx)
            .debug_bounds("check-running")
            .is_some()
    );
    model.update(cx, |model, cx| {
        model.receive(
            Event::Activity {
                key: "tasks".into(),
                active: false,
            },
            cx,
        );
        model.receive(
            Event::State {
                mode: Mode::Mock,
                snapshot: model.snapshot.clone().unwrap(),
                profile: model.profile.clone(),
                message: "refreshed",
                new_count: 0,
            },
            cx,
        );
        assert!(!model.errors.contains_key("tasks"));
    });
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    assert!(
        VisualTestContext::from_window(handle, cx)
            .debug_bounds("check-success")
            .is_some()
    );
}

// 构造已加载的同一来源快照，后续交互不替换编辑器/撤销栈。
fn with_conversation(model: &Entity<Model>, view: &Entity<Workspace>, cx: &mut TestAppContext) {
    let source = model.update(cx, |model, cx| {
        let source = &mut model.snapshot.as_mut().unwrap().records[0].source;
        source.title = "真实任务标题".into();
        source.metadata.source_messages = vec![lark2tapd_core::SourceMessage {
            message_id: "numeric-message".into(),
            content: "**会话正文**\n\n第二段".into(),
        }];
        let source = source.clone();
        cx.notify();
        source
    });
    view.update(cx, |view, cx| {
        view.edit_source = Some(source);
        cx.notify();
    });
}

#[gpui_kit::test]
fn conversation_is_dynamic_and_prepends_as_one_undoable_edit(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    load_task(&events, &model, &view, handle, cx);
    with_conversation(&model, &view, cx);
    let before = cx.read(|cx| view.read(cx).description.read(cx).text().to_owned());
    let requests = cx.read(|cx| model.read(cx).next_request_sequence);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("insert-conversation").label(),
            Some("插入正文前")
        );
    })
    .unwrap();
    cx.update_window(handle, |_, window, _| {
        assert!(
            window.try_find("toggle-source-details").is_none(),
            "metadata and conversation must be directly visible"
        );
    })
    .unwrap();
    click(cx, handle, "insert-conversation");
    let inserted = cx.read(|cx| view.read(cx).description.read(cx).text().to_owned());
    assert!(inserted.contains("> **会话正文**\n>\n> 第二段"));
    assert!(!inserted.contains("原文匹配"));
    assert!(inserted.ends_with(&before));
    cx.read(|cx| {
        assert!(view.read(cx).has_unsaved_draft(cx));
        assert_eq!(
            model.read(cx).next_request_sequence,
            requests,
            "inserting must not save or submit"
        );
    });
    assert!(inserted.find("会话正文").unwrap() < inserted.len() - before.len());
    click(cx, handle, "insert-conversation");
    cx.read(|cx| assert_eq!(view.read(cx).description.read(cx).text(), inserted));
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let editor = view.read(cx).description.clone();
        editor.update(cx, |editor, cx| editor.focus(window, cx));
        window.press("cmd-z", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        assert_eq!(view.read(cx).description.read(cx).text(), before);
        assert!(!view.read(cx).has_unsaved_draft(cx));
    });
    // 刷新后的内容动态替换卡片，不能自动改写用户正文。
    model.update(cx, |model, cx| {
        model.snapshot.as_mut().unwrap().records[0]
            .source
            .metadata
            .source_messages[0]
            .content = "更新后的会话".into();
        cx.notify();
    });
    click(cx, handle, "insert-conversation");
    cx.read(|cx| assert_eq!(view.read(cx).description.read(cx).text(), before));
    let latest = cx.read(|cx| {
        model.read(cx).snapshot.as_ref().unwrap().records[0]
            .source
            .clone()
    });
    view.update(cx, |view, cx| {
        view.edit_source = Some(latest);
        cx.notify();
    });
    click(cx, handle, "insert-conversation");
    cx.read(|cx| {
        let content = view.read(cx).description.read(cx).text();
        assert!(content.contains("更新后的会话") && !content.contains("**会话正文**"));
        assert!(content.ends_with(&before));
    });
}

#[gpui_kit::test]
fn matched_conversation_keeps_preview_provenance_but_inserts_plain_quotes(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    load_task(&events, &model, &view, handle, cx);
    with_conversation(&model, &view, cx);
    let before = cx.read(|cx| view.read(cx).description.read(cx).text().to_owned());
    model.update(cx, |model, cx| {
        let metadata = &mut model.snapshot.as_mut().unwrap().records[0].source.metadata;
        metadata.matched_messages.insert(
            "numeric-message".into(),
            vec![
                lark2tapd_core::SourceMessage {
                    message_id: "om_a".into(),
                    content: "**会话正文**".into(),
                },
                lark2tapd_core::SourceMessage {
                    message_id: "om_b".into(),
                    content: "第二段".into(),
                },
            ],
        );
        for (id, name) in [("om_a", "Alice"), ("om_b", "Bob")] {
            metadata.message_details.insert(
                id.into(),
                lark2tapd_core::SourceMessageDetails {
                    sender: Some(lark2tapd_core::SourcePerson {
                        id: format!("ou_{name}"),
                        name: name.into(),
                    }),
                    ..Default::default()
                },
            );
        }
        cx.notify();
    });
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    let mut visual = VisualTestContext::from_window(handle, cx);
    assert!(
        visual.debug_bounds("source-message-1").is_some(),
        "matched original messages must be separate items"
    );
    assert!(visual.debug_bounds("source-match-label").is_some());
    cx.read(|cx| assert_eq!(view.read(cx).description.read(cx).text(), before));
    click(cx, handle, "insert-conversation");
    cx.read(|cx| {
        let content = view.read(cx).description.read(cx).text();
        assert!(content.contains("> Alice\n>\n") && content.contains("> Bob\n>\n"));
        assert!(!content.contains("原文匹配"));
        assert!(!content.contains("发送人未知"));
        assert!(content.ends_with(&before));
    });
}

#[gpui_kit::test]
fn conversation_feedback_is_bound_to_account_task_and_editor_session(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    load_task(&events, &model, &view, handle, cx);
    with_conversation(&model, &view, cx);
    let (source, session, profile, before) = cx.read(|cx| {
        let view = view.read(cx);
        (
            view.edit_source.clone().unwrap(),
            view.editor_session,
            view.edit_profile.clone(),
            view.description.read(cx).text().to_owned(),
        )
    });
    for variant in 0..3 {
        let mut stale_source = source.clone();
        if variant == 2 {
            stale_source.description.push_str("changed");
        }
        model.update(cx, |model, cx| {
            model.receive(
                Event::Conversation(ConversationResult {
                    source: Box::new(stale_source),
                    session: if variant == 1 { session + 1 } else { session },
                    profile: if variant == 0 {
                        "other-account".into()
                    } else {
                        profile.clone()
                    },
                    error: Some("stale error".into()),
                }),
                cx,
            )
        });
        cx.run_until_parked();
        cx.read(|cx| assert!(view.read(cx).conversation_error.is_none()));
    }
    model.update(cx, |model, cx| {
        model.snapshot.as_mut().unwrap().records[0]
            .source
            .description
            .push_str("new snapshot");
        model.receive(
            Event::Conversation(ConversationResult {
                source: Box::new(source.clone()),
                session,
                profile: profile.clone(),
                error: Some("old source failure".into()),
            }),
            cx,
        );
    });
    cx.run_until_parked();
    cx.read(|cx| {
        assert!(
            view.read(cx).conversation_error.is_none(),
            "an old editor must not accept errors for a refreshed source"
        )
    });
    model.update(cx, |model, cx| {
        model.snapshot.as_mut().unwrap().records[0].source = source.clone();
        model.receive(
            Event::Conversation(ConversationResult {
                source: Box::new(source),
                session,
                profile,
                error: Some("matching unavailable".into()),
            }),
            cx,
        )
    });
    cx.run_until_parked();
    cx.read(|cx| {
        assert_eq!(
            view.read(cx).conversation_error.as_deref(),
            Some("matching unavailable")
        );
        assert_eq!(view.read(cx).description.read(cx).text(), before);
    });
}

#[gpui_kit::test]
fn uncertain_receipt_prefills_recovery_and_empty_ids_do_not_submit(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    load_task(&events, &model, &view, handle, cx);
    model.update(cx, |model, cx| {
        let mut snapshot = model.snapshot.clone().unwrap();
        snapshot.records[0].uncertain = true;
        snapshot.records[0].target_workspace = Some("demo".into());
        snapshot.records[0].target = Some(lark2tapd_core::TargetTask {
            workitem_type_id: String::new(),
            id: "113000000000000001".into(),
            kind: "story".into(),
            title: "Target".into(),
            description: String::new(),
            owner: String::new(),
            due: None,
            url: String::new(),
        });
        model.receive(
            Event::State {
                mode: Mode::Mock,
                snapshot,
                profile: model.profile.clone(),
                message: "",
                new_count: 0,
            },
            cx,
        );
    });
    cx.run_until_parked();
    cx.read(|cx| {
        assert_eq!(
            view.read(cx).recover_id.read(cx).value().as_ref(),
            "113000000000000001"
        )
    });
    for invalid in ["", "https://example.invalid/task", "not-an-id"] {
        cx.update_window(handle, |_, window, cx| {
            set(
                &view.read(cx).recover_id.clone(),
                invalid.into(),
                window,
                cx,
            )
        })
        .unwrap();
        let before = cx.read(|cx| model.read(cx).next_request_sequence);
        click(cx, handle, "recover");
        cx.read(|cx| assert_eq!(model.read(cx).next_request_sequence, before));
    }
}

#[gpui_kit::test]
fn source_image_insertion_tracks_attachment_references_through_undo(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    load_task(&events, &model, &view, handle, cx);
    let root = cx.read(|cx| worker::media_root(&model.read(cx).data_root, "mock").unwrap());
    let image = lark2tapd_core::save_pasted_image(&root, &png_fixture(), "png").unwrap();
    let url = "http://p2.pstatp.com/origin/test.png";
    let source = model.update(cx, |model, cx| {
        let source = &mut model.snapshot.as_mut().unwrap().records[0].source;
        source.metadata.source_messages = vec![lark2tapd_core::SourceMessage {
            message_id: "image-source".into(),
            content: format!("<p>正文</p><figure><img src=\"{url}\"/></figure>"),
        }];
        source
            .metadata
            .source_images
            .insert(url.into(), image.clone());
        let source = source.clone();
        cx.notify();
        source
    });
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.edit_source = Some(source);
            view.load_images(window, cx);
            cx.notify();
        })
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        assert!(view.read(cx).draft(cx).attachments.is_empty());
        assert!(
            view.read(cx)
                .images
                .contains_key(&format!("attachment:{}", image.id)),
            "source preview must have a decoded render image before insertion"
        );
    });
    click(cx, handle, "insert-conversation");
    cx.read(|cx| {
        let draft = view.read(cx).draft(cx);
        assert!(
            draft
                .description
                .contains(&format!("attachment:{}", image.id))
        );
        assert_eq!(
            lark2tapd_bridge::referenced_image_ids(&draft.description),
            std::collections::HashSet::from([image.id.clone()])
        );
        assert_eq!(draft.attachments.len(), 1);
        assert_eq!(draft.attachments[0].id, image.id);
    });
    cx.update_window(handle, |_, window, cx| {
        let editor = view.read(cx).description.clone();
        editor.update(cx, |editor, cx| editor.focus(window, cx));
        window.press("cmd-z", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        assert!(
            view.read(cx).draft(cx).attachments.is_empty(),
            "undo must not leave an unreferenced image to upload"
        )
    });
}

#[gpui_kit::test]
fn conversation_insertion_respects_busy_and_ime(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    load_task(&events, &model, &view, handle, cx);
    with_conversation(&model, &view, cx);
    let before = cx.read(|cx| view.read(cx).description.read(cx).text().to_owned());
    model.update(cx, |model, cx| {
        model.busy = true;
        cx.notify();
    });
    click(cx, handle, "insert-conversation");
    cx.read(|cx| assert_eq!(view.read(cx).description.read(cx).text(), before));
    model.update(cx, |model, cx| {
        model.busy = false;
        cx.notify();
    });
    cx.update_window(handle, |_, window, cx| {
        let editor = view.read(cx).description.clone();
        editor.update(cx, |editor, cx| {
            editor.focus(window, cx);
            editor.replace_and_mark_text_in_range(None, "ni", Some(2..2), window, cx);
        });
    })
    .unwrap();
    let composing = cx.read(|cx| view.read(cx).description.read(cx).text().to_owned());
    click(cx, handle, "insert-conversation");
    cx.read(|cx| assert_eq!(view.read(cx).description.read(cx).text(), composing));
}

// 来源图片须经过同一草稿/回执规则，保存重开和成功提交不能改变其语义。
fn stage_source_image(
    model: &Entity<Model>,
    view: &Entity<Workspace>,
    handle: AnyWindowHandle,
    cx: &mut TestAppContext,
) {
    let root = cx.read(|cx| worker::media_root(&model.read(cx).data_root, "mock").unwrap());
    let image = lark2tapd_core::save_pasted_image(&root, &png_fixture(), "png").unwrap();
    let source = model.update(cx, |model, cx| {
        let source = &mut model.snapshot.as_mut().unwrap().records[0].source;
        source
            .metadata
            .source_images
            .insert("https://p2.pstatp.com/a".into(), image.clone());
        let source = source.clone();
        cx.notify();
        source
    });
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.edit_source = Some(source);
            view.description.update(cx, |editor, cx| {
                editor.set_text(format!("![source](attachment:{})", image.id), cx)
            });
            view.load_images(window, cx);
            cx.notify();
        })
    })
    .unwrap();
    cx.run_until_parked();
}
#[gpui_kit::test]
fn successful_source_image_submit_is_clean(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    load_task(&events, &model, &view, handle, cx);
    stage_source_image(&model, &view, handle, cx);
    click(cx, handle, "submit");
    pump(&events, &model, cx, |cx| {
        model.read(cx).snapshot.as_ref().unwrap().records[0]
            .last_synced_at
            .is_some()
    });
    assert!(
        !cx.read(|cx| view.read(cx).has_unsaved_draft(cx)),
        "successful source image submit still dirty"
    );
}
#[gpui_kit::test]
fn removed_source_image_after_reopen_is_not_uploaded(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    let guid = load_task(&events, &model, &view, handle, cx);
    stage_source_image(&model, &view, handle, cx);
    click(cx, handle, "save-draft");
    pump(&events, &model, cx, |cx| {
        model.read(cx).snapshot.as_ref().unwrap().records[0]
            .draft
            .is_some()
    });
    let session = cx.read(|cx| view.read(cx).editor_session);
    model.update(cx, |model, cx| {
        model.send(Job::OpenDraft(guid, false, session), cx)
    });
    pump(&events, &model, cx, |cx| {
        !view.read(cx).attachments.is_empty()
    });
    view.update(cx, |view, cx| {
        view.description
            .update(cx, |editor, cx| editor.set_text("image removed", cx))
    });
    cx.run_until_parked();
    click(cx, handle, "submit");
    pump(&events, &model, cx, |cx| {
        model.read(cx).snapshot.as_ref().unwrap().records[0]
            .last_synced_at
            .is_some()
    });
    assert!(
        cx.read(|cx| model.read(cx).snapshot.as_ref().unwrap().records[0]
            .last_draft
            .as_ref()
            .unwrap()
            .attachments
            .is_empty()),
        "removed source image was uploaded"
    );
}

#[gpui_kit::test]
fn submit_button_shows_activity_and_blocks_repeated_clicks(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    load_task(&events, &model, &view, handle, cx);
    click(cx, handle, "submit");
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("submit").label(), Some("导入中…"));
    })
    .unwrap();
    let sequence = cx.read(|cx| model.read(cx).next_request_sequence);
    click(cx, handle, "submit");
    assert_eq!(cx.read(|cx| model.read(cx).next_request_sequence), sequence);
}

#[gpui_kit::test]
fn workitem_type_clears_on_workspace_change_and_is_required(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    load_task(&events, &model, &view, handle, cx);
    assert_eq!(cx.read(|cx| view.read(cx).draft(cx).workitem_type_id), "7");
    choose(cx, handle, "draft-workspace", "演示项目二");
    assert!(cx.read(|cx| view.read(cx).draft(cx).workitem_type_id.is_empty()));
    let sequence = cx.read(|cx| model.read(cx).next_request_sequence);
    click(cx, handle, "submit");
    assert_eq!(cx.read(|cx| model.read(cx).next_request_sequence), sequence);
    pump(&events, &model, cx, |cx| {
        model
            .read(cx)
            .snapshot
            .as_ref()
            .unwrap()
            .directory
            .workitem_types
            .contains_key("demo-2")
    });
    assert!(cx.read(|cx| view.read(cx).draft(cx).workitem_type_id.is_empty()));
    choose(cx, handle, "draft-workitem-type", "开发任务");
    assert_eq!(cx.read(|cx| view.read(cx).draft(cx).workitem_type_id), "8");
    click(cx, handle, "submit");
    pump(&events, &model, cx, |cx| {
        model.read(cx).snapshot.as_ref().unwrap().records[0]
            .last_synced_at
            .is_some()
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.within("draft-workitem-type").click("input", cx);
        window.render_frame(cx);
        assert!(
            window
                .try_find(gpui_kit::component::IndexPath::new(0))
                .is_none(),
            "imported type selector must not open its option list"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn choosing_bug_creates_and_reopens_a_bug_without_story_type(cx: &mut TestAppContext) {
    let (dir, events, model, handle, view) = setup(cx);
    let guid = load_task(&events, &model, &view, handle, cx);
    choose(cx, handle, "draft-workitem-type", "Bug");
    assert_eq!(cx.read(|cx| view.read(cx).draft(cx).workitem_kind), "bug");
    assert!(cx.read(|cx| view.read(cx).draft(cx).workitem_type_id.is_empty()));
    click(cx, handle, "submit");
    pump(&events, &model, cx, |cx| {
        model.read(cx).snapshot.as_ref().unwrap().records[0]
            .last_synced_at
            .is_some()
    });
    assert_eq!(
        cx.read(|cx| model.read(cx).snapshot.as_ref().unwrap().records[0]
            .target
            .as_ref()
            .unwrap()
            .kind
            .clone()),
        "bug"
    );
    // 即使旧表单选择被替换，草稿仍使用绑定身份。
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.fill_workitem_types("story:7", window, cx)
        });
    })
    .unwrap();
    assert_eq!(cx.read(|cx| view.read(cx).draft(cx).workitem_kind), "bug");
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let engine = lark2tapd_core::Engine::open(&dir.path().join("mock.db"))
            .await
            .unwrap();
        let draft = engine.prepare(&guid).unwrap();
        assert_eq!(draft.workitem_kind, "bug");
        assert!(draft.workitem_type_id.is_empty());
    });
}

// 后台刷新不能锁住已缓存选项；只有成功的新目录才能使选择失效。
#[gpui_kit::test]
fn cached_types_remain_selectable_during_refresh_and_failure(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    load_task(&events, &model, &view, handle, cx);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| view.fill_workitem_types("", window, cx));
    })
    .unwrap();
    model.update(cx, |model, cx| {
        model.receive(
            Event::Activity {
                key: "workitem-types:demo".into(),
                active: true,
            },
            cx,
        );
    });
    choose(cx, handle, "draft-workitem-type", "开发任务");
    assert_eq!(cx.read(|cx| view.read(cx).draft(cx).workitem_type_id), "7");
    model.update(cx, |model, cx| {
        model.receive(
            Event::Activity {
                key: "workitem-types:demo".into(),
                active: false,
            },
            cx,
        );
        model.receive(
            Event::FormError {
                field: "workitem-types",
                message: "Offline".into(),
            },
            cx,
        );
    });
    cx.run_until_parked();
    assert_eq!(cx.read(|cx| view.read(cx).draft(cx).workitem_type_id), "7");
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("reload-workitem-types").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn refreshed_types_clear_removed_selection_without_crossing_workspaces(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    load_task(&events, &model, &view, handle, cx);
    choose(cx, handle, "draft-workspace", "演示项目二");
    pump(&events, &model, cx, |cx| {
        model
            .read(cx)
            .snapshot
            .as_ref()
            .unwrap()
            .directory
            .workitem_types
            .contains_key("demo-2")
    });
    choose(cx, handle, "draft-workitem-type", "开发任务");
    model.update(cx, |model, cx| {
        // 切换后的晚到结果只能更新旧项目缓存，不能改写当前选择。
        model
            .snapshot
            .as_mut()
            .unwrap()
            .directory
            .workitem_types
            .insert("demo".into(), vec![]);
        cx.notify();
    });
    cx.run_until_parked();
    assert_eq!(cx.read(|cx| view.read(cx).draft(cx).workitem_type_id), "8");
    model.update(cx, |model, cx| {
        model
            .snapshot
            .as_mut()
            .unwrap()
            .directory
            .workitem_types
            .insert("demo-2".into(), vec![]);
        cx.notify();
    });
    cx.run_until_parked();
    assert!(cx.read(|cx| view.read(cx).draft(cx).workitem_type_id.is_empty()));
}

#[gpui_kit::test]
fn missing_image_resource_is_explained_without_rewriting_the_draft(cx: &mut TestAppContext) {
    let (_dir, events, model, handle, view) = setup(cx);
    load_task(&events, &model, &view, handle, cx);
    model.update(cx, |model, cx| {
        model.snapshot.as_mut().unwrap().records[0]
            .source
            .description = "[Image]".into();
        cx.notify();
    });
    view.update(cx, |view, cx| {
        view.description
            .update(cx, |editor, cx| editor.set_text("[Image]", cx))
    });
    assert!(
        VisualTestContext::from_window(handle, cx)
            .debug_bounds("source-image-unavailable")
            .is_some()
    );
    assert_eq!(
        cx.read(|cx| view.read(cx).description.read(cx).text().to_owned()),
        "[Image]"
    );
}
