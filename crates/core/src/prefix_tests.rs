use super::*;

fn draft(prefix: &str, title: &str) -> Draft {
    Draft {
        workitem_kind: "story".into(),
        workitem_type_id: "7".into(),
        include_source_link: true,
        title: title.into(),
        title_prefix: prefix.into(),
        attachments: vec![],
        description: String::new(),
        workspace_id: "demo".into(),
        owner: String::new(),
        due: None,
    }
}

#[test]
fn raw_prefix_preserves_spacing_brackets_and_collapses_only_exact_repeats() {
    for (prefix, title, expected) in [
        ("  [需求] ", "标题", "  [需求] 标题"),
        ("需求", "需求需求标题", "需求标题"),
        ("【需求】", "【需求】【需求】标题", "【需求】标题"),
        ("需求 ", "需求需求标题", "需求 需求需求标题"),
        ("", "【其他】标题", "【其他】标题"),
    ] {
        assert_eq!(draft(prefix, title).rendered_title(), expected);
    }
}

#[tokio::test]
async fn raw_prefix_settings_round_trip_without_normalization() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("raw.sqlite");
    let mut engine = Engine::open(&path).await?;
    let values = vec![
        "  [需求] ".into(),
        "【名称】".into(),
        "a b".into(),
        "x".repeat(66),
    ];
    let mut settings = engine.snapshot().settings.clone();
    settings
        .workspace_prefixes
        .insert("demo".into(), values.clone());
    engine.save_settings(settings).await?;
    drop(engine);
    let engine = Engine::open(&path).await?;
    assert_eq!(
        engine.snapshot().settings.workspace_prefixes["demo"],
        values
    );
    for invalid in [
        "".into(),
        " \u{3000} ".into(),
        "a\nb".into(),
        "a\tb".into(),
        "x".repeat(67),
    ] {
        assert!(validate_prefix(&invalid).is_err(), "accepted {invalid:?}");
    }
    Ok(())
}

#[tokio::test]
async fn legacy_prefix_migration_is_persisted_once_without_changing_titles() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("legacy.sqlite");
    let mut engine = Engine::open(&path).await?;
    let source = MockGateway::default().fetch_tasks()?.remove(0);
    let original = SyncRecord {
        last_synced_at: None,
        media_body_pending: false,
        title_prefix: "需求".into(),
        last_draft: Some(draft("需求", "【需求】【需求】旧标题")),
        in_scope: true,
        baseline: Some(source.clone()),
        source,
        target: Some(TargetTask {
            workitem_type_id: String::new(),
            id: "1".into(),
            url: "url".into(),
            kind: "task".into(),
            title: "【需求】旧标题".into(),
            description: String::new(),
            owner: String::new(),
            due: None,
        }),
        target_workspace: Some("demo".into()),
        draft: Some(draft("", "未选前缀")),
        uncertain: false,
        last_error: None,
    };
    let mut snapshot = engine.snapshot().clone();
    snapshot.records.push(original.clone());
    snapshot.records.push(SyncRecord {
        draft: Some(draft("需求", "标题")),
        ..original.clone()
    });
    snapshot
        .settings
        .workspace_prefixes
        .insert("demo".into(), vec!["需求".into(), "x".repeat(64)]);
    // 写入无版本的真实旧存储格式，而不是借助待测迁移构造预期。
    let legacy = serde_json::json!({"snapshot":snapshot,"draft_sources":{}});
    StoredState::upsert_by_key("snapshot")
        .payload(legacy.to_string())
        .exec(&mut engine.db)
        .await?;
    drop(engine);
    let mut engine = Engine::open(&path).await?;
    let migrated = engine.snapshot().clone();
    assert_eq!(migrated.settings.workspace_prefixes["demo"][0], "【需求】");
    assert_eq!(
        migrated.settings.workspace_prefixes["demo"][1]
            .chars()
            .count(),
        66
    );
    let record = &migrated.records[0];
    assert_eq!(record.title_prefix, "【需求】");
    assert_eq!(record.last_draft.as_ref().unwrap().title_prefix, "【需求】");
    assert_eq!(
        record.last_draft.as_ref().unwrap().rendered_title(),
        "【需求】旧标题"
    );
    assert_eq!(
        record.last_draft.as_ref().unwrap().title,
        "【需求】【需求】旧标题"
    );
    assert_eq!(record.draft.as_ref().unwrap().title_prefix, "");
    assert_eq!(
        migrated.records[1].draft.as_ref().unwrap().title_prefix,
        "【需求】"
    );
    assert_eq!(record.source, original.source);
    assert_eq!(record.baseline, original.baseline);
    assert_eq!(record.target, original.target);
    let row = StoredState::filter_by_key("snapshot")
        .first()
        .exec(&mut engine.db)
        .await?
        .unwrap();
    let stored: serde_json::Value = serde_json::from_str(&row.payload)?;
    assert_eq!(stored["prefix_format_version"], 1);
    assert_eq!(
        stored["snapshot"]["settings"]["workspace_prefixes"]["demo"][0],
        "【需求】"
    );
    engine.save_settings(migrated.settings.clone()).await?;
    drop(engine);
    assert_eq!(Engine::open(&path).await?.snapshot(), &migrated);
    Ok(())
}
