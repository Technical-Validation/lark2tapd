use super::*;
use anyhow::bail;
use std::sync::atomic::{AtomicUsize, Ordering};

// 只替换外部网关故障；SQLite、状态推进及重启均走真实实现。
struct FaultGateway {
    inner: MockGateway,
    writes: AtomicUsize,
    reads: AtomicUsize,
    reject_draft: bool,
    source_override: Option<SourceTask>,
    fail_write: bool,
    tasks: Option<Vec<SourceTask>>,
    remote_title: Option<String>,
    journal_path: Option<std::path::PathBuf>,
    replace_result_id: bool,
    workspaces: Option<Vec<TapdWorkspace>>,
    members: Option<Vec<TapdMember>>,
}
impl FaultGateway {
    fn new(inner: MockGateway) -> Self {
        Self {
            inner,
            writes: AtomicUsize::new(0),
            reads: AtomicUsize::new(0),
            reject_draft: false,
            source_override: None,
            fail_write: false,
            tasks: None,
            remote_title: None,
            journal_path: None,
            replace_result_id: false,
            workspaces: None,
            members: None,
        }
    }
}
impl Gateway for FaultGateway {
    fn list_workspaces(&self) -> Result<Vec<TapdWorkspace>> {
        self.workspaces
            .clone()
            .ok_or_else(|| anyhow!("Directory unavailable"))
    }
    fn list_members(&self, _workspace: &str) -> Result<Vec<TapdMember>> {
        self.members
            .clone()
            .ok_or_else(|| anyhow!("Directory unavailable"))
    }
    fn validate_draft(&self, _draft: &Draft) -> Result<()> {
        ensure!(!self.reject_draft, "Transport draft rejected");
        Ok(())
    }
    fn get_source(&self, guid: &str) -> Result<SourceTask> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.source_override
            .clone()
            .map_or_else(|| self.inner.get_source(guid), Ok)
    }
    fn fetch_tasks(&self) -> Result<Vec<SourceTask>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.tasks
            .clone()
            .map_or_else(|| self.inner.fetch_tasks(), Ok)
    }
    fn get_target(&self, workspace: &str, id: &str, kind: &str) -> Result<TargetTask> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let mut target = self.inner.get_target(workspace, id, kind)?;
        if let Some(title) = &self.remote_title {
            target.title = title.clone();
        }
        Ok(target)
    }
    fn create_target(&self, draft: &Draft, source: &SourceTask) -> Result<TargetTask> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        if let Some(path) = self.journal_path.clone() {
            let observed = std::thread::spawn(move || {
                tokio::runtime::Runtime::new().unwrap().block_on(async {
                    let engine = Engine::open(&path).await.unwrap();
                    engine.snapshot().records[0].uncertain
                })
            })
            .join()
            .unwrap();
            ensure!(
                observed,
                "External write started before journal was durable"
            );
        }
        let target = self.inner.create_target(draft, source)?;
        if self.fail_write {
            bail!("response lost after remote write");
        }
        Ok(target)
    }
    fn update_target(
        &self,
        old: &TargetTask,
        draft: &Draft,
        source: &SourceTask,
    ) -> Result<TargetTask> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        let mut target = self.inner.update_target(old, draft, source)?;
        if self.replace_result_id {
            target.id = "wrong-returned-id".into();
        }
        Ok(target)
    }
}
// 本地预检失败不能留下不确定日志，也不能触发任何外部读写。
#[tokio::test]
async fn transport_preflight_failure_keeps_persisted_state_unchanged() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("state.sqlite");
    let mut gateway = FaultGateway::new(MockGateway::default());
    let mut engine = configured(&path, &gateway).await?;
    let guid = engine.snapshot().records[0].source.guid.clone();
    let draft = engine.prepare(&guid)?;
    engine.save_draft(&guid, &draft).await?;
    let before = engine.snapshot().clone();
    let reads = gateway.reads.load(Ordering::SeqCst);
    gateway.reject_draft = true;
    assert!(engine.commit(&gateway, &guid, &draft, None).await.is_err());
    assert_eq!(gateway.reads.load(Ordering::SeqCst), reads);
    assert_eq!(gateway.writes.load(Ordering::SeqCst), 0);
    assert_eq!(engine.snapshot(), &before);
    drop(engine);
    let engine = Engine::open(&path).await?;
    assert_eq!(engine.snapshot(), &before);
    assert_eq!(
        engine.snapshot().records[0].status(),
        SyncStatus::Unimported
    );
    assert!(!engine.snapshot().records[0].uncertain);
    Ok(())
}

// 轮询和重启不能把旧草稿伪装成基于新来源的编辑。
#[tokio::test]
async fn stale_draft_requires_explicit_latest_source_rebase() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("state.sqlite");
    let gateway = MockGateway::default();
    let mut engine = configured(&path, &gateway).await?;
    let old_source = engine.snapshot().records[0].source.clone();
    let guid = &old_source.guid;
    let old_draft = engine.prepare(guid)?;
    engine.save_draft(guid, &old_draft).await?;
    gateway.change_demo_task()?;
    engine.refresh(&gateway).await?;
    drop(engine);
    let mut engine = Engine::open(&path).await?;
    assert!(engine.prepare(guid).is_err());
    let before = engine.snapshot().clone();
    assert!(engine.save_draft(guid, &old_draft).await.is_err());
    assert!(
        engine
            .save_draft_for_source(guid, &old_draft, &old_source)
            .await
            .is_err()
    );
    assert_eq!(engine.snapshot(), &before);
    let latest = engine.snapshot().records[0].source.clone();
    let fresh = Draft {
        title: latest.title.clone(),
        description: latest.description.clone(),
        due: latest.due.clone(),
        ..old_draft
    };
    engine.save_draft_for_source(guid, &fresh, &latest).await?;
    assert_eq!(engine.prepare(guid)?, fresh);
    engine.commit(&gateway, guid, &fresh, None).await?;
    assert_eq!(
        engine.snapshot().records[0].baseline.as_ref(),
        Some(&latest)
    );
    Ok(())
}

// 丢弃是用户明确放弃编辑，不得删除不确定写入的恢复依据。
#[tokio::test]
async fn discard_reloads_latest_source_but_preserves_uncertain_draft() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("state.sqlite");
    let mut gateway = FaultGateway::new(MockGateway::default());
    let mut engine = configured(&path, &gateway).await?;
    let guid = engine.snapshot().records[0].source.guid.clone();
    let draft = engine.prepare(&guid)?;
    engine.save_draft(&guid, &draft).await?;
    gateway.inner.change_demo_task()?;
    engine.refresh(&gateway).await?;
    engine.discard_draft(&guid).await?;
    drop(engine);
    let mut engine = Engine::open(&path).await?;
    assert!(engine.snapshot().records[0].draft.is_none());
    let fresh = engine.prepare(&guid)?;
    assert_eq!(fresh.title, "设计同步流程 · 已更新");
    engine.save_draft(&guid, &fresh).await?;
    gateway.fail_write = true;
    assert!(engine.commit(&gateway, &guid, &fresh, None).await.is_err());
    let before = engine.snapshot().clone();
    assert!(engine.discard_draft(&guid).await.is_err());
    assert_eq!(engine.snapshot(), &before);
    Ok(())
}

// 提交只重读所选来源；网关返回错误身份或无效内容时必须阻止写入。
#[tokio::test]
async fn commit_reads_selected_source_and_checks_identity() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut gateway = FaultGateway::new(MockGateway::default());
    let mut engine = configured(&dir.path().join("state.sqlite"), &gateway).await?;
    let source = engine.snapshot().records[0].source.clone();
    let guid = &source.guid;
    let draft = engine.prepare(guid)?;
    engine.save_draft(guid, &draft).await?;
    let mut wrong = source.clone();
    wrong.guid = "another-guid".into();
    gateway.source_override = Some(wrong);
    assert!(engine.commit(&gateway, guid, &draft, None).await.is_err());
    assert_eq!(gateway.writes.load(Ordering::SeqCst), 0);
    // 列表故意不可用，单条读取仍能完成提交。
    gateway.source_override = Some(source.clone());
    gateway.tasks = Some(vec![source.clone(), source.clone()]);
    engine.commit(&gateway, guid, &draft, None).await?;
    assert_eq!(engine.snapshot().records[0].status(), SyncStatus::Synced);
    Ok(())
}

async fn configured(path: &Path, gateway: &impl Gateway) -> Result<Engine> {
    let mut engine = Engine::open(path).await?;
    engine
        .save_settings(Settings {
            workspace_id: "demo".into(),
            ..Settings::default()
        })
        .await?;
    engine.refresh(gateway).await?;
    let directory = MockGateway::default();
    engine.refresh_workspaces(&directory).await?;
    engine.refresh_members(&directory, "demo").await?;
    Ok(engine)
}

#[tokio::test]
async fn ambiguous_write_is_journaled_and_never_retried_after_restart() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("state.sqlite");
    let mut gateway = FaultGateway::new(MockGateway::default());
    gateway.fail_write = true;
    gateway.journal_path = Some(path.clone());
    let mut engine = configured(&path, &gateway).await?;
    let guid = engine.snapshot().records[0].source.guid.clone();
    let draft = engine.prepare(&guid)?;
    engine.save_draft(&guid, &draft).await?;
    assert!(engine.commit(&gateway, &guid, &draft, None).await.is_err());
    assert_eq!(engine.snapshot().records[0].status(), SyncStatus::Uncertain);
    drop(engine);
    let mut engine = Engine::open(&path).await?;
    assert_eq!(engine.snapshot().records[0].status(), SyncStatus::Uncertain);
    assert!(engine.commit(&gateway, &guid, &draft, None).await.is_err());
    assert_eq!(gateway.writes.load(Ordering::SeqCst), 1);
    assert!(engine.snapshot().records[0].last_error.is_some());
    Ok(())
}

#[tokio::test]
async fn target_changes_after_preview_are_not_overwritten() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut gateway = FaultGateway::new(MockGateway::default());
    let mut engine = configured(&dir.path().join("state.sqlite"), &gateway).await?;
    let guid = engine.snapshot().records[0].source.guid.clone();
    let draft = engine.prepare(&guid)?;
    engine.save_draft(&guid, &draft).await?;
    engine.commit(&gateway, &guid, &draft, None).await?;
    gateway.inner.change_demo_task()?;
    engine.refresh(&gateway).await?;
    let draft = engine.prepare(&guid)?;
    engine.save_draft(&guid, &draft).await?;
    let preview = engine.preview_target(&gateway, &guid)?;
    gateway.remote_title = Some("别人修改了目标".into());
    assert!(
        engine
            .commit(&gateway, &guid, &draft, preview)
            .await
            .is_err()
    );
    assert_eq!(gateway.writes.load(Ordering::SeqCst), 1);
    assert_eq!(engine.snapshot().records[0].status(), SyncStatus::Changed);
    // 用户重新预览并确认后，允许覆盖已明确展示的远端差异。
    let preview = engine.preview_target(&gateway, &guid)?;
    engine.commit(&gateway, &guid, &draft, preview).await?;
    assert_eq!(engine.snapshot().records[0].status(), SyncStatus::Synced);
    Ok(())
}

#[tokio::test]
async fn stale_source_and_unsaved_drafts_cannot_commit() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let gateway = FaultGateway::new(MockGateway::default());
    let mut engine = configured(&dir.path().join("state.sqlite"), &gateway).await?;
    let guid = engine.snapshot().records[0].source.guid.clone();
    let draft = engine.prepare(&guid)?;
    assert!(engine.commit(&gateway, &guid, &draft, None).await.is_err());
    engine.save_draft(&guid, &draft).await?;
    gateway.inner.change_demo_task()?;
    assert!(engine.commit(&gateway, &guid, &draft, None).await.is_err());
    engine.refresh(&gateway).await?;
    assert!(engine.commit(&gateway, &guid, &draft, None).await.is_err());
    assert_eq!(gateway.writes.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn invalid_fetch_is_atomic_and_disappeared_tasks_are_retained() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut gateway = FaultGateway::new(MockGateway::default());
    let mut engine = configured(&dir.path().join("state.sqlite"), &gateway).await?;
    let before = engine.snapshot().clone();
    let task = before.records[0].source.clone();
    gateway.tasks = Some(vec![task.clone(), task.clone()]);
    assert!(engine.refresh(&gateway).await.is_err());
    assert_eq!(engine.snapshot(), &before);
    let mut malformed = task;
    malformed.due = Some("2025-02-30".into());
    gateway.tasks = Some(vec![malformed]);
    assert!(engine.refresh(&gateway).await.is_err());
    assert_eq!(engine.snapshot(), &before);
    gateway.tasks = Some(vec![]);
    engine.refresh(&gateway).await?;
    let mut hidden = before.records;
    for record in &mut hidden {
        record.in_scope = false;
    }
    assert_eq!(engine.snapshot().records, hidden);
    Ok(())
}

#[tokio::test]
async fn settings_validation_and_invalid_draft_block_writes() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let gateway = FaultGateway::new(MockGateway::default());
    let mut engine = configured(&dir.path().join("state.sqlite"), &gateway).await?;
    let original = engine.snapshot().settings.clone();
    for minutes in [0, 1441] {
        let mut settings = original.clone();
        settings.poll_minutes = minutes;
        assert!(engine.apply_committed_settings(settings.clone()).is_err());
        assert!(engine.save_settings(settings).await.is_err());
    }
    for (language, theme) in [("fr", "system"), ("zh-CN", "blue")] {
        let mut settings = original.clone();
        settings.language = language.into();
        settings.theme = theme.into();
        assert!(engine.apply_committed_settings(settings.clone()).is_err());
        assert!(engine.save_settings(settings).await.is_err());
    }
    assert_eq!(&engine.snapshot().settings, &original);
    let guid = engine.snapshot().records[0].source.guid.clone();
    for field in ["title", "workspace", "due"] {
        let mut draft = engine.prepare(&guid)?;
        match field {
            "title" => draft.title = " ".into(),
            "workspace" => draft.workspace_id.clear(),
            _ => draft.due = Some("2024-1-1".into()),
        }
        engine.save_draft(&guid, &draft).await?;
        assert!(engine.commit(&gateway, &guid, &draft, None).await.is_err());
    }
    assert_eq!(gateway.writes.load(Ordering::SeqCst), 0);
    Ok(())
}

#[test]
fn status_uses_every_source_field_and_uncertainty_takes_priority() -> Result<()> {
    let source = MockGateway::default().fetch_tasks()?.remove(0);
    let draft = Draft {
        title: source.title.clone(),
        title_prefix: String::new(),
        attachments: vec![],
        description: source.description.clone(),
        workspace_id: "demo".into(),
        owner: String::new(),
        due: source.due.clone(),
    };
    let mut record = SyncRecord {
        media_body_pending: false,
        title_prefix: String::new(),
        last_draft: None,
        in_scope: true,
        source: source.clone(),
        baseline: Some(source.clone()),
        target: Some(mock_target("1".into(), "story".into(), &draft, &source)),
        target_workspace: Some("demo".into()),
        draft: None,
        uncertain: false,
        last_error: None,
    };
    assert_eq!(record.status(), SyncStatus::Synced);
    for field in ["title", "description", "due", "completed", "url", "guid"] {
        record.source = source.clone();
        match field {
            "title" => record.source.title.push('!'),
            "description" => record.source.description.push('!'),
            "due" => record.source.due = None,
            "completed" => record.source.completed = true,
            "url" => record.source.url.push('!'),
            _ => record.source.guid.push('!'),
        }
        assert_eq!(record.status(), SyncStatus::Changed, "field: {field}");
        assert_eq!(record.baseline.as_ref(), Some(&source));
    }
    record.target = None;
    assert_eq!(record.status(), SyncStatus::Unimported);
    record.uncertain = true;
    assert_eq!(record.status(), SyncStatus::Uncertain);
    Ok(())
}

#[tokio::test]
async fn mismatched_write_result_remains_uncertain() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut gateway = FaultGateway::new(MockGateway::default());
    let mut engine = configured(&dir.path().join("state.sqlite"), &gateway).await?;
    let guid = engine.snapshot().records[0].source.guid.clone();
    let draft = engine.prepare(&guid)?;
    engine.save_draft(&guid, &draft).await?;
    engine.commit(&gateway, &guid, &draft, None).await?;
    gateway.inner.change_demo_task()?;
    engine.refresh(&gateway).await?;
    let old = engine.snapshot().records[0].target.clone();
    let draft = engine.prepare(&guid)?;
    engine.save_draft(&guid, &draft).await?;
    let preview = engine.preview_target(&gateway, &guid)?;
    gateway.replace_result_id = true;
    assert!(
        engine
            .commit(&gateway, &guid, &draft, preview)
            .await
            .is_err()
    );
    assert_eq!(engine.snapshot().records[0].status(), SyncStatus::Uncertain);
    assert_eq!(engine.snapshot().records[0].target, old);
    Ok(())
}

// 恢复只绑定带稳定来源标记的远端，绑定后仍需确认更新，不能猜测写入成功。
#[tokio::test]
async fn recovery_binds_readback_without_assuming_success() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("state.sqlite");
    let mut gateway = FaultGateway::new(MockGateway::default());
    gateway.fail_write = true;
    let mut engine = configured(&path, &gateway).await?;
    let guid = engine.snapshot().records[0].source.guid.clone();
    let draft = engine.prepare(&guid)?;
    engine.save_draft(&guid, &draft).await?;
    assert!(engine.commit(&gateway, &guid, &draft, None).await.is_err());
    assert!(
        engine
            .bind_existing(&gateway, &guid, "demo", "mock-demo-2", "story")
            .await
            .is_err()
    );
    assert_eq!(engine.snapshot().records[0].status(), SyncStatus::Uncertain);
    engine
        .bind_existing(&gateway, &guid, "demo", "mock-demo-1", "story")
        .await?;
    assert_eq!(engine.snapshot().records[0].status(), SyncStatus::Changed);
    drop(engine);
    let mut engine = Engine::open(&path).await?;
    let preview = engine.preview_target(&gateway, &guid)?;
    engine.commit(&gateway, &guid, &draft, preview).await?;
    assert_eq!(engine.snapshot().records[0].status(), SyncStatus::Synced);
    assert_eq!(gateway.writes.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
async fn mock_remote_and_source_can_be_restored_from_snapshot() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let gateway = MockGateway::default();
    let mut engine = configured(&dir.path().join("state.sqlite"), &gateway).await?;
    let guid = engine.snapshot().records[0].source.guid.clone();
    let draft = engine.prepare(&guid)?;
    engine.save_draft(&guid, &draft).await?;
    engine.commit(&gateway, &guid, &draft, None).await?;
    gateway.change_demo_task()?;
    engine.refresh(&gateway).await?;
    let restarted = MockGateway::from_snapshot(engine.snapshot());
    assert_eq!(
        engine.preview_target(&restarted, &guid)?,
        engine.snapshot().records[0].target
    );
    let before = engine.snapshot().records.clone();
    assert_eq!(engine.refresh(&restarted).await?, 0);
    assert_eq!(engine.snapshot().records, before);
    Ok(())
}

// 完整链路会发现：丢失缓存、草稿覆盖、重复创建或更新后未推进基线。
#[tokio::test]
async fn mock_sync_survives_restart_and_requires_confirmation() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("state.sqlite");
    let gateway = MockGateway::default();
    let mut engine = Engine::open(&path).await?;
    let settings = Settings {
        workspace_id: "demo".into(),
        owner: "alice".into(),
        ..Settings::default()
    };
    engine.save_settings(settings.clone()).await?;
    assert_eq!(engine.refresh(&gateway).await?, 3);
    engine.refresh_workspaces(&gateway).await?;
    engine.refresh_members(&gateway, "demo").await?;
    let guid = engine.snapshot().records[0].source.guid.clone();
    let mut draft = engine.prepare(&guid)?;
    draft.title = "人工编辑标题".into();
    draft.description = "<原样保留> & \"内容\"\n第二行".into();
    engine.save_draft(&guid, &draft).await?;
    assert_eq!(engine.refresh(&gateway).await?, 0);
    drop(engine);
    let mut engine = Engine::open(&path).await?;
    assert_eq!(engine.snapshot().settings, settings);
    assert_eq!(engine.prepare(&guid)?, draft);
    assert_eq!(
        engine.snapshot().records[0].status(),
        SyncStatus::Unimported
    );
    assert_eq!(engine.preview_target(&gateway, &guid)?, None);
    engine.commit(&gateway, &guid, &draft, None).await?;
    assert_eq!(engine.snapshot().records[0].status(), SyncStatus::Synced);
    assert!(engine.commit(&gateway, &guid, &draft, None).await.is_err());
    gateway.change_demo_task()?;
    engine.refresh(&gateway).await?;
    assert_eq!(engine.snapshot().records[0].status(), SyncStatus::Changed);
    let draft = engine.prepare(&guid)?;
    engine.save_draft(&guid, &draft).await?;
    let target = engine.preview_target(&gateway, &guid)?;
    assert!(
        target
            .as_ref()
            .unwrap()
            .description
            .contains("<原样保留> & \"内容\"")
    );
    engine.commit(&gateway, &guid, &draft, target).await?;
    drop(engine);
    let engine = Engine::open(&path).await?;
    assert_eq!(engine.snapshot().records[0].status(), SyncStatus::Synced);
    assert!(engine.snapshot().last_checked.is_some());
    Ok(())
}

// 旧缓存缺少新字段时，来源信息未知且任务必须先重新验证范围。
#[test]
fn legacy_snapshot_defaults_hide_unverified_records() -> Result<()> {
    let mut value = serde_json::to_value(Snapshot::default())?;
    value.as_object_mut().unwrap().remove("directory");
    value["records"] = serde_json::json!([{
        "source":{"guid":"old","title":"Old","description":"","url":"","due":null,"completed":false},
        "baseline":null,"target":null,"target_workspace":null,"draft":null,"uncertain":false,"last_error":null
    }]);
    let snapshot: Snapshot = serde_json::from_value(value)?;
    let serialized = serde_json::to_value(snapshot)?;
    assert_eq!(serialized["records"][0]["in_scope"], false);
    assert_eq!(
        serialized["records"][0]["source"]["metadata"]["assignees"],
        serde_json::json!([])
    );
    assert_eq!(serialized["directory"]["workspaces"], serde_json::json!([]));
    Ok(())
}

// 完整刷新后只隐藏已离开范围的记录，不能丢弃草稿或映射。
#[tokio::test]
async fn absent_record_cannot_be_edited_or_written() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut gateway = FaultGateway::new(MockGateway::default());
    let mut engine = configured(&dir.path().join("scope.sqlite"), &gateway).await?;
    let source = engine.snapshot().records[0].source.clone();
    let draft = engine.prepare(&source.guid)?;
    engine.save_draft(&source.guid, &draft).await?;
    gateway.tasks = Some(vec![]);
    engine.refresh(&gateway).await?;
    assert!(engine.prepare(&source.guid).is_err());
    assert!(
        engine
            .save_draft_for_source(&source.guid, &draft, &source)
            .await
            .is_err()
    );
    assert!(
        engine
            .commit(&gateway, &source.guid, &draft, None)
            .await
            .is_err()
    );
    assert_eq!(gateway.writes.load(Ordering::SeqCst), 0);
    assert_eq!(engine.snapshot().records[0].draft.as_ref(), Some(&draft));
    Ok(())
}

#[test]
fn self_scope_matches_creator_or_assignee_not_unknown_identity() -> Result<()> {
    let mut source = MockGateway::default().fetch_tasks()?.remove(0);
    source.metadata = SourceMetadata {
        creator: Some(SourcePerson {
            id: "creator".into(),
            name: String::new(),
        }),
        assignees: vec![SourcePerson {
            id: "assignee".into(),
            name: "同名".into(),
        }],
        ..Default::default()
    };
    assert!(source.is_related_to("creator"));
    assert!(source.is_related_to("assignee"));
    assert!(!source.is_related_to("同名"));
    assert!(!source.is_related_to(""));
    source.metadata = SourceMetadata::default();
    assert!(!source.is_related_to("creator"));
    Ok(())
}

// 缓存走真实持久化；项目间成员不能串用，失败不能覆盖旧内容。
#[tokio::test]
async fn directory_cache_is_atomic_isolated_and_durable() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("directory.sqlite");
    let gateway = MockGateway::default();
    let mut engine = Engine::open(&path).await?;
    engine.refresh_workspaces(&gateway).await?;
    engine.refresh_members(&gateway, "demo").await?;
    let first = engine.snapshot().directory.members["demo"].clone();
    engine.refresh_members(&gateway, "demo-2").await?;
    assert_eq!(engine.snapshot().directory.members["demo"], first);
    assert_ne!(
        first.items,
        engine.snapshot().directory.members["demo-2"].items
    );
    let before = engine.snapshot().clone();
    let unsupported = FaultGateway::new(gateway.clone());
    assert!(engine.refresh_workspaces(&unsupported).await.is_err());
    assert!(engine.refresh_members(&unsupported, "demo").await.is_err());
    assert!(engine.refresh_members(&gateway, " ").await.is_err());
    assert_eq!(engine.snapshot(), &before);
    drop(engine);
    let engine = Engine::open(&path).await?;
    assert_eq!(engine.snapshot(), &before);
    chrono::DateTime::parse_from_rfc3339(&first.fetched_at)?;
    chrono::DateTime::parse_from_rfc3339(before.directory.workspaces_fetched_at.as_ref().unwrap())?;
    Ok(())
}

#[tokio::test]
async fn explicit_workspace_add_validates_and_updates_unique_id() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut engine = Engine::open(&dir.path().join("directory.sqlite")).await?;
    for (id, name) in [("", "name"), ("id", " ")] {
        assert!(
            engine
                .add_workspace(TapdWorkspace {
                    id: id.into(),
                    name: name.into()
                })
                .await
                .is_err()
        );
    }
    for name in ["Old", "New"] {
        engine
            .add_workspace(TapdWorkspace {
                id: "id".into(),
                name: name.into(),
            })
            .await?;
    }
    assert_eq!(
        engine.snapshot().directory.workspaces,
        vec![TapdWorkspace {
            id: "id".into(),
            name: "New".into()
        }]
    );
    assert!(engine.snapshot().directory.workspaces_fetched_at.is_none());
    Ok(())
}

// 任意一条目录数据损坏都会拒绝整批，不能写入部分项目或成员。
#[tokio::test]
async fn malformed_directory_batch_preserves_both_caches() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut engine = Engine::open(&dir.path().join("cache.sqlite")).await?;
    let mut gateway = FaultGateway::new(MockGateway::default());
    engine.refresh_workspaces(&gateway.inner).await?;
    engine.refresh_members(&gateway.inner, "demo").await?;
    let before = engine.snapshot().clone();
    for (id, name) in [("", "name"), ("id", " "), ("same", "same")] {
        gateway.workspaces = Some(vec![
            TapdWorkspace {
                id: "same".into(),
                name: "valid".into(),
            },
            TapdWorkspace {
                id: id.into(),
                name: name.into(),
            },
        ]);
        gateway.members = Some(vec![
            TapdMember {
                account: "same".into(),
                name: "valid".into(),
            },
            TapdMember {
                account: id.into(),
                name: name.into(),
            },
        ]);
        assert!(engine.refresh_workspaces(&gateway).await.is_err());
        assert!(engine.refresh_members(&gateway, "demo").await.is_err());
        assert_eq!(engine.snapshot(), &before);
    }
    // 空目录是有效全量结果，但不能误删另一个资源的缓存。
    gateway.workspaces = Some(vec![]);
    engine.refresh_workspaces(&gateway).await?;
    assert!(engine.snapshot().directory.workspaces.is_empty());
    assert_eq!(
        engine.snapshot().directory.members,
        before.directory.members
    );
    gateway.members = Some(vec![]);
    engine.refresh_members(&gateway, "demo").await?;
    assert!(engine.snapshot().directory.members["demo"].items.is_empty());
    Ok(())
}

// 离开范围不等于撤销已发生的远端写入；恢复日志仍可内部核对绑定。
#[tokio::test]
async fn hidden_uncertain_record_retains_recovery_across_restart() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("hidden.sqlite");
    let mut gateway = FaultGateway::new(MockGateway::default());
    let mut engine = configured(&path, &gateway).await?;
    let guid = engine.snapshot().records[0].source.guid.clone();
    let draft = engine.prepare(&guid)?;
    engine.save_draft(&guid, &draft).await?;
    gateway.fail_write = true;
    assert!(engine.commit(&gateway, &guid, &draft, None).await.is_err());
    let mut retained = engine.snapshot().records[0].clone();
    gateway.tasks = Some(vec![]);
    engine.refresh(&gateway).await?;
    retained.in_scope = false;
    assert_eq!(engine.snapshot().records[0], retained);
    drop(engine);
    let mut engine = Engine::open(&path).await?;
    assert_eq!(engine.snapshot().records[0], retained);
    engine
        .bind_existing(&gateway, &guid, "demo", "mock-demo-1", "story")
        .await?;
    assert!(!engine.snapshot().records[0].in_scope);
    assert!(engine.snapshot().records[0].target.is_some());
    gateway.tasks = None;
    engine.refresh(&gateway).await?;
    assert!(engine.snapshot().records[0].in_scope);
    assert!(engine.snapshot().records[0].target.is_some());
    Ok(())
}

#[tokio::test]
async fn refreshing_admin_projects_preserves_manual_member_projects() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("cache.db");
    let mut engine = Engine::open(&path).await?;
    engine
        .add_workspace(TapdWorkspace {
            id: "member-only".into(),
            name: "成员项目".into(),
        })
        .await?;
    engine.refresh_workspaces(&MockGateway::default()).await?;
    drop(engine);
    let engine = Engine::open(&path).await?;
    assert!(
        engine
            .snapshot()
            .directory
            .workspaces
            .iter()
            .any(|p| p.id == "member-only" && p.name == "成员项目")
    );
    Ok(())
}

#[tokio::test]
async fn owner_removed_after_preview_is_blocked_before_write_journal() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut gateway = FaultGateway::new(MockGateway::default());
    let mut engine = configured(&dir.path().join("removed-owner.db"), &gateway).await?;
    engine.refresh_workspaces(&gateway.inner).await?;
    engine.refresh_members(&gateway.inner, "demo").await?;
    let guid = engine.snapshot().records[0].source.guid.clone();
    let mut draft = engine.prepare(&guid)?;
    draft.owner = "alice".into();
    engine.save_draft(&guid, &draft).await?;
    let preview = engine.preview_target(&gateway, &guid)?;
    gateway.members = Some(vec![]);
    engine.refresh_members(&gateway, "demo").await?;
    assert!(
        engine
            .commit(&gateway, &guid, &draft, preview)
            .await
            .is_err()
    );
    assert!(!engine.snapshot().records[0].uncertain);
    assert_eq!(gateway.writes.load(Ordering::SeqCst), 0);
    Ok(())
}

// 前缀按项目校验；非法输入必须在写入日志之前失败，旧格式保持可读。
#[tokio::test]
async fn prefix_configuration_is_atomic_and_commit_preserves_selection() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("prefix.sqlite");
    let gateway = FaultGateway::new(MockGateway::default());
    let mut engine = configured(&path, &gateway).await?;
    let mut settings = serde_json::to_value(engine.snapshot().settings.clone())?;
    settings["workspace_prefixes"] = serde_json::json!({"demo":["【需求】"],"demo-2":["其他"]});
    engine
        .save_settings(serde_json::from_value(settings.clone())?)
        .await?;
    for labels in [
        serde_json::json!([" "]),
        serde_json::json!(["需求", "需求"]),
        serde_json::json!(["a\nb"]),
        serde_json::json!(["a".repeat(67)]),
    ] {
        let mut invalid = settings.clone();
        invalid["workspace_prefixes"]["demo"] = labels;
        assert!(
            engine
                .save_settings(serde_json::from_value(invalid)?)
                .await
                .is_err()
        );
    }
    let guid = engine.snapshot().records[0].source.guid.clone();
    let mut value = serde_json::to_value(engine.prepare(&guid)?)?;
    value["title_prefix"] = "其他".into();
    let invalid: Draft = serde_json::from_value(value.clone())?;
    engine.save_draft(&guid, &invalid).await?;
    let before = engine.snapshot().clone();
    assert!(
        engine
            .commit(&gateway, &guid, &invalid, None)
            .await
            .is_err()
    );
    assert_eq!(engine.snapshot(), &before);
    assert_eq!(gateway.writes.load(Ordering::SeqCst), 0);
    value["title_prefix"] = "【需求】".into();
    value["title"] = "【需求】【需求】人工标题".into();
    let draft: Draft = serde_json::from_value(value)?;
    engine.save_draft(&guid, &draft).await?;
    engine.commit(&gateway, &guid, &draft, None).await?;
    assert_eq!(
        engine.snapshot().records[0].target.as_ref().unwrap().title,
        "【需求】人工标题"
    );
    gateway.inner.change_demo_task()?;
    engine.refresh(&gateway).await?;
    drop(engine);
    let engine = Engine::open(&path).await?;
    let draft = serde_json::to_value(engine.prepare(&guid)?)?;
    assert_eq!(draft["title_prefix"], "【需求】");
    assert_eq!(draft["title"], "设计同步流程 · 已更新");
    assert_eq!(
        serde_json::to_value(&engine.snapshot().settings)?["workspace_prefixes"],
        settings["workspace_prefixes"]
    );
    Ok(())
}

// 重启后读到旧的重复后缀，也只显示一个更新标记；描述继续变化。
#[test]
fn mock_updates_normalize_existing_suffixes() -> Result<()> {
    let gateway = MockGateway::default();
    gateway.store.lock().unwrap().tasks[0].title = "任务 · 已更新 · 已更新".into();
    gateway.change_demo_task()?;
    let first = gateway.fetch_tasks()?.remove(0);
    gateway.change_demo_task()?;
    let second = gateway.fetch_tasks()?.remove(0);
    assert_eq!(second.title, "任务 · 已更新");
    assert_ne!(first.description, second.description);
    Ok(())
}

#[test]
fn old_drafts_default_prefix_and_attachments() -> Result<()> {
    let draft: Draft = serde_json::from_value(
        serde_json::json!({"title":"Old","description":"","workspace_id":"demo","owner":"","due":null}),
    )?;
    let value = serde_json::to_value(draft)?;
    assert_eq!(value["title_prefix"], "");
    assert_eq!(value["attachments"], serde_json::json!([]));
    Ok(())
}

// 已成功导入的编辑与附件不因重启丢失，显式保存允许更新同一目标。
#[tokio::test]
async fn successful_draft_survives_restart_and_explicit_update() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("edited.sqlite");
    let gateway = FaultGateway::new(MockGateway::default());
    let mut engine = configured(&path, &gateway).await?;
    let guid = engine.snapshot().records[0].source.guid.clone();
    let mut draft = engine.prepare(&guid)?;
    draft.title = "人工标题".into();
    engine.save_draft(&guid, &draft).await?;
    engine.commit(&gateway, &guid, &draft, None).await?;
    let target = engine.snapshot().records[0].target.clone();
    drop(engine);
    let mut engine = Engine::open(&path).await?;
    assert_eq!(engine.prepare(&guid)?, draft);
    draft.title = "第二次人工标题".into();
    engine.save_draft(&guid, &draft).await?;
    engine
        .commit(&gateway, &guid, &draft, target.clone())
        .await?;
    assert_eq!(
        engine.snapshot().records[0].target.as_ref().unwrap().id,
        target.unwrap().id
    );
    assert_eq!(gateway.writes.load(Ordering::SeqCst), 2);
    Ok(())
}

// 每个上传步骤先写持久化日志；回执不能替换附件身份或改写本地内容。
#[tokio::test]
async fn attachment_upload_progress_is_durable_and_bound_to_target() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("attachments.sqlite");
    let gateway = MockGateway::default();
    let mut engine = configured(&path, &gateway).await?;
    let guid = engine.snapshot().records[0].source.guid.clone();
    let mut draft = engine.prepare(&guid)?;
    let mut attachment: DraftAttachment = serde_json::from_value(
        serde_json::json!({"id":"abc", "name":"test.txt", "path":"/cache/abc", "size":3, "is_image":false}),
    )?;
    draft.attachments.push(attachment.clone());
    engine.save_draft(&guid, &draft).await?;
    assert!(
        engine
            .attachment_progress(&guid, &attachment)
            .await
            .is_err()
    );
    engine.commit(&gateway, &guid, &draft, None).await?;
    attachment.uncertain = true;
    engine.attachment_progress(&guid, &attachment).await?;
    drop(engine);
    let mut engine = Engine::open(&path).await?;
    assert!(engine.prepare(&guid)?.attachments[0].uncertain);
    let mut forged = attachment.clone();
    forged.path = "/outside".into();
    assert!(engine.attachment_progress(&guid, &forged).await.is_err());
    attachment.remote_id = Some("remote-1".into());
    attachment.remote_url = Some("https://www.tapd.cn/attachment/remote-1".into());
    attachment.uncertain = false;
    engine.attachment_progress(&guid, &attachment).await?;
    gateway.change_demo_task()?;
    engine.refresh(&gateway).await?;
    assert_eq!(engine.prepare(&guid)?.attachments, vec![attachment]);
    Ok(())
}

// 缓存只依赖内容；同名文件不覆盖，扩展名不能伪装图片，读取时重新验哈希。
#[test]
fn media_cache_is_stable_bounded_and_verified() -> Result<()> {
    use crate::media::*;
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("cache");
    let source = dir.path().join("notes.txt");
    std::fs::write(&source, b"first")?;
    let first = copy_attachment(&root, &source)?;
    assert_eq!(first, copy_attachment(&root, &source)?);
    assert!(!first.is_image);
    assert_eq!(read_attachment(&root, &first)?, b"first");
    std::fs::write(&source, b"second")?;
    let second = copy_attachment(&root, &source)?;
    assert_ne!(first.id, second.id);
    assert_eq!(read_attachment(&root, &first)?, b"first");
    let png = b"\x89PNG\r\n\x1a\nimage bytes";
    let image = save_pasted_image(&root, png, "png")?;
    assert!(image.is_image);
    assert_eq!(image, save_pasted_image(&root, png, "png")?);
    assert!(save_pasted_image(&root, b"not an image", "png").is_err());
    assert!(save_pasted_image(&root, png, "jpeg").is_err());
    assert!(save_pasted_image(&root, png, "../png").is_err());
    assert!(save_pasted_image(&root, &vec![0; 20 * 1024 * 1024 + 1], "png").is_err());
    std::fs::write(&source, vec![0; 20 * 1024 * 1024 + 1])?;
    assert!(copy_attachment(&root, &source).is_err());
    let mut forged = first.clone();
    forged.path = source.to_string_lossy().into_owned();
    assert!(read_attachment(&root, &forged).is_err());
    forged = first.clone();
    forged.id = "../notes".into();
    assert!(verify_attachment(&root, &forged).is_err());
    std::fs::write(&first.path, b"wrong")?;
    assert!(verify_attachment(&root, &first).is_err());
    std::fs::write(&source, b"first")?;
    assert!(copy_attachment(&root, &source).is_err());
    assert_eq!(std::fs::read(&first.path)?, b"wrong");
    Ok(())
}

#[cfg(unix)]
#[test]
fn media_rejects_symlinks_and_hardlinked_cache_entries() -> Result<()> {
    use crate::media::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("cache");
    let source = dir.path().join("source.txt");
    std::fs::write(&source, b"owned")?;
    let link = dir.path().join("link.txt");
    symlink(&source, &link)?;
    assert!(copy_attachment(&root, &link).is_err());
    let attachment = copy_attachment(&root, &source)?;
    assert_eq!(
        std::fs::metadata(&attachment.path)?.permissions().mode() & 0o777,
        0o600
    );
    std::fs::remove_file(&attachment.path)?;
    symlink(&source, &attachment.path)?;
    assert!(verify_attachment(&root, &attachment).is_err());
    assert!(copy_attachment(&root, &source).is_err());
    std::fs::remove_file(&attachment.path)?;
    std::fs::hard_link(&source, &attachment.path)?;
    assert!(verify_attachment(&root, &attachment).is_err());
    let fake_root = dir.path().join("fake-cache");
    symlink(&root, &fake_root)?;
    assert!(copy_attachment(&fake_root, &source).is_err());
    let other = dir.path().join("other-cache");
    std::fs::create_dir(&other)?;
    symlink(&root, other.join("media"))?;
    assert!(copy_attachment(&other, &source).is_err());
    Ok(())
}

// 原样前缀不改写原始编辑值，也不会删除不相干的标题前缀。
#[test]
fn rendered_title_preserves_unrelated_text_and_empty_selection() -> Result<()> {
    let mut draft: Draft = serde_json::from_value(
        serde_json::json!({"title":"【其他】原文", "title_prefix":"【需求】", "description":"", "workspace_id":"demo", "owner":"", "due":null}),
    )?;
    assert_eq!(draft.rendered_title(), "【需求】【其他】原文");
    assert_eq!(draft.title, "【其他】原文");
    draft.title_prefix.clear();
    assert_eq!(draft.rendered_title(), "【其他】原文");
    Ok(())
}

// 所有支持的图片都依赖签名；普通文件的伪图片扩展名不能启用图片展示。
#[test]
fn media_image_detection_uses_content_not_names() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("cache");
    for (bytes, extension) in [
        (b"\xff\xd8\xffpayload".as_slice(), "jpeg"),
        (b"GIF89apayload".as_slice(), "gif"),
        (b"RIFF1234WEBPpayload".as_slice(), "webp"),
    ] {
        let attachment = save_pasted_image(&root, bytes, extension)?;
        assert!(attachment.is_image);
        assert_eq!(read_attachment(&root, &attachment)?, bytes);
    }
    let source = dir.path().join("fake.png");
    std::fs::write(&source, "this is not a PNG")?;
    assert!(!copy_attachment(&root, &source)?.is_image);
    Ok(())
}

// 附件上限与去重是提交边界规则，不能只依赖界面限制。
#[tokio::test]
async fn invalid_attachment_lists_never_start_write_journal() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let gateway = FaultGateway::new(MockGateway::default());
    let mut engine = configured(&dir.path().join("count.sqlite"), &gateway).await?;
    let guid = engine.snapshot().records[0].source.guid.clone();
    let attachment: DraftAttachment = serde_json::from_value(
        serde_json::json!({"id":"a", "name":"test.txt", "path":"/cache/a", "size":3, "is_image":false}),
    )?;
    for count in [2, 11] {
        let mut draft = engine.prepare(&guid)?;
        draft.attachments = (0..count)
            .map(|index| {
                let mut item = attachment.clone();
                if count == 11 {
                    item.id = index.to_string();
                }
                item
            })
            .collect();
        engine.save_draft(&guid, &draft).await?;
        let before = engine.snapshot().clone();
        assert!(engine.commit(&gateway, &guid, &draft, None).await.is_err());
        assert_eq!(engine.snapshot(), &before);
        assert_eq!(gateway.writes.load(Ordering::SeqCst), 0);
    }
    Ok(())
}

// 过期编辑器不能清空已经落盘的上传日志或回执；本地身份更改也不能借用回执。
#[tokio::test]
async fn stale_editor_preserves_attachment_progress() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("receipts.sqlite");
    let gateway = MockGateway::default();
    let mut engine = configured(&path, &gateway).await?;
    let source = engine.snapshot().records[0].source.clone();
    let mut draft = engine.prepare(&source.guid)?;
    let mut attachment: DraftAttachment = serde_json::from_value(
        serde_json::json!({"id":"a", "name":"test.txt", "path":"/cache/a", "size":3, "is_image":false}),
    )?;
    draft.attachments.push(attachment.clone());
    engine.save_draft(&source.guid, &draft).await?;
    engine.commit(&gateway, &source.guid, &draft, None).await?;
    attachment.uncertain = true;
    engine
        .attachment_progress(&source.guid, &attachment)
        .await?;
    engine
        .save_draft_for_source(&source.guid, &draft, &source)
        .await?;
    assert!(engine.prepare(&source.guid)?.attachments[0].uncertain);
    attachment.uncertain = false;
    attachment.remote_id = Some("remote-1".into());
    engine
        .attachment_progress(&source.guid, &attachment)
        .await?;
    engine
        .save_draft_for_source(&source.guid, &draft, &source)
        .await?;
    assert_eq!(
        engine.prepare(&source.guid)?.attachments[0]
            .remote_id
            .as_deref(),
        Some("remote-1")
    );
    draft.attachments[0].path = "/different".into();
    assert!(
        engine
            .save_draft_for_source(&source.guid, &draft, &source)
            .await
            .is_err()
    );
    drop(engine);
    let engine = Engine::open(&path).await?;
    assert_eq!(
        engine.prepare(&source.guid)?.attachments[0]
            .remote_id
            .as_deref(),
        Some("remote-1")
    );
    Ok(())
}

// 待上传和结果不确定都需要显式显示；有效 ID 或 URL 任一回执即可表示已上传。
#[tokio::test]
async fn pending_attachments_are_visible_after_target_sync() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let gateway = MockGateway::default();
    let mut engine = configured(&dir.path().join("pending.sqlite"), &gateway).await?;
    assert!(!engine.snapshot().records[0].has_pending_attachments());
    let source = engine.snapshot().records[0].source.clone();
    let mut draft = engine.prepare(&source.guid)?;
    let mut attachment: DraftAttachment = serde_json::from_value(
        serde_json::json!({"id":"a", "name":"test.txt", "path":"/cache/a", "size":3, "is_image":false}),
    )?;
    draft.attachments.push(attachment.clone());
    engine.save_draft(&source.guid, &draft).await?;
    engine.commit(&gateway, &source.guid, &draft, None).await?;
    assert_eq!(engine.snapshot().records[0].status(), SyncStatus::Synced);
    assert!(engine.snapshot().records[0].has_pending_attachments());
    attachment.remote_id = Some(" ".into());
    engine
        .attachment_progress(&source.guid, &attachment)
        .await?;
    assert!(engine.snapshot().records[0].has_pending_attachments());
    attachment.remote_id = Some("remote".into());
    engine
        .attachment_progress(&source.guid, &attachment)
        .await?;
    assert!(!engine.snapshot().records[0].has_pending_attachments());
    attachment.uncertain = true;
    engine
        .attachment_progress(&source.guid, &attachment)
        .await?;
    assert!(engine.snapshot().records[0].has_pending_attachments());
    Ok(())
}

#[tokio::test]
async fn image_body_pending_is_not_hidden_by_successful_upload_receipts() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let gateway = MockGateway::default();
    let mut engine = configured(&dir.path().join("body.sqlite"), &gateway).await?;
    let guid = engine.snapshot().records[0].source.guid.clone();
    let draft = engine.prepare(&guid)?;
    engine.save_draft(&guid, &draft).await?;
    engine.commit(&gateway, &guid, &draft, None).await?;
    let mut value = serde_json::to_value(&engine.snapshot().records[0])?;
    value["media_body_pending"] = true.into();
    let record: SyncRecord = serde_json::from_value(value)?;
    assert!(record.has_pending_attachments());
    Ok(())
}

#[tokio::test]
async fn image_finalization_failure_before_write_survives_restart() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("finalize.sqlite");
    let mut gateway = FaultGateway::new(MockGateway::default());
    let mut engine = configured(&path, &gateway).await?;
    let source = engine.snapshot().records[0].source.clone();
    let mut draft = engine.prepare(&source.guid)?;
    let mut attachment: DraftAttachment = serde_json::from_value(
        serde_json::json!({"id":"image","name":"picture.png","path":"/cache/picture.png","size":100,"is_image":true}),
    )?;
    draft.attachments.push(attachment.clone());
    engine.save_draft(&source.guid, &draft).await?;
    engine.commit(&gateway, &source.guid, &draft, None).await?;
    engine.set_media_body_pending(&source.guid, true).await?;
    attachment.remote_url = Some("/tfl/pictures/image.png".into());
    engine
        .attachment_progress(&source.guid, &attachment)
        .await?;
    draft.attachments[0] = attachment;
    engine.save_draft(&source.guid, &draft).await?;
    let target = engine.snapshot().records[0].target.clone();
    let mut changed = source.clone();
    changed.title.push('!');
    gateway.source_override = Some(changed);
    assert!(
        engine
            .commit(&gateway, &source.guid, &draft, target)
            .await
            .is_err()
    );
    assert!(!engine.snapshot().records[0].uncertain);
    drop(engine);
    let engine = Engine::open(&path).await?;
    let record = &engine.snapshot().records[0];
    assert!(record.media_body_pending);
    assert!(record.has_pending_attachments());
    assert!(
        record.last_draft.as_ref().unwrap().attachments[0]
            .remote_url
            .is_some()
    );
    Ok(())
}

#[tokio::test]
async fn accepted_remote_target_retains_local_draft_and_mapping() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut gateway = FaultGateway::new(MockGateway::default());
    let mut engine = configured(&dir.path().join("accept.sqlite"), &gateway).await?;
    let guid = engine.snapshot().records[0].source.guid.clone();
    let mut draft = engine.prepare(&guid)?;
    engine.save_draft(&guid, &draft).await?;
    engine.commit(&gateway, &guid, &draft, None).await?;
    let old = engine.snapshot().records[0].target.clone();
    draft.title = "本地编辑".into();
    engine.save_draft(&guid, &draft).await?;
    gateway.remote_title = Some("远端编辑".into());
    assert!(engine.commit(&gateway, &guid, &draft, old).await.is_err());
    let fresh = engine.preview_target(&gateway, &guid)?.unwrap();
    let id = fresh.id.clone();
    engine.accept_target(&guid, &fresh).await?;
    assert_eq!(engine.prepare(&guid)?.title, "本地编辑");
    engine.commit(&gateway, &guid, &draft, Some(fresh)).await?;
    assert_eq!(engine.snapshot().records[0].target.as_ref().unwrap().id, id);
    Ok(())
}

#[tokio::test]
async fn restored_mock_titles_do_not_keep_legacy_repeated_update_markers() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let gateway = MockGateway::default();
    let engine = configured(&dir.path().join("legacy-marker.sqlite"), &gateway).await?;
    let mut snapshot = engine.snapshot().clone();
    snapshot.records[0].source.title = "任务 · 已更新 · 已更新 · 已更新".into();
    let restored = MockGateway::from_snapshot(&snapshot);
    assert_eq!(restored.fetch_tasks()?[0].title, "任务 · 已更新");
    Ok(())
}

#[tokio::test]
async fn runtime_settings_do_not_weaken_persistent_settings_or_business_writes() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("runtime.sqlite");
    let gateway = FaultGateway::new(MockGateway::default());
    let mut engine = configured(&path, &gateway).await?;
    let original = engine.snapshot().clone();
    let mut next = original.settings.clone();
    next.theme = "dark".into();
    engine.apply_committed_settings(next.clone())?;
    assert_eq!(engine.snapshot().settings, next);
    assert_eq!(Engine::open(&path).await?.snapshot(), &original);
    // 受控故障只破坏临时库；设置保存、草稿保存仍必须失败且不发布内存变更。
    toasty::sql::query("DROP TABLE stored_states")
        .exec(&mut engine.db)
        .await?;
    let mut invalid = next.clone();
    invalid.theme = "invalid".into();
    assert!(engine.apply_committed_settings(invalid).is_err());
    let mut other = next.clone();
    other.theme = "light".into();
    assert!(engine.save_settings(other).await.is_err());
    assert_eq!(engine.snapshot().settings, next);
    let before = engine.snapshot().clone();
    let guid = &before.records[0].source.guid;
    let mut draft = engine.prepare(guid)?;
    draft.title = "not committed".into();
    assert!(engine.save_draft(guid, &draft).await.is_err());
    assert_eq!(engine.snapshot(), &before);
    Ok(())
}
