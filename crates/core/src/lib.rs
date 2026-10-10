use anyhow::{Result, anyhow, ensure};
use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceTask {
    pub guid: String,
    pub title: String,
    pub description: String,
    pub url: String,
    pub due: Option<String>,
    pub completed: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Draft {
    pub title: String,
    pub description: String,
    pub workspace_id: String,
    pub owner: String,
    pub due: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TargetTask {
    pub id: String,
    pub url: String,
    pub kind: String,
    pub title: String,
    pub description: String,
    pub owner: String,
    pub due: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncRecord {
    pub source: SourceTask,
    pub baseline: Option<SourceTask>,
    pub target: Option<TargetTask>,
    pub target_workspace: Option<String>,
    pub draft: Option<Draft>,
    pub uncertain: bool,
    pub last_error: Option<String>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SyncStatus {
    Unimported,
    Synced,
    Changed,
    Uncertain,
}
impl SyncRecord {
    pub fn status(&self) -> SyncStatus {
        if self.uncertain {
            SyncStatus::Uncertain
        } else if self.target.is_none() {
            SyncStatus::Unimported
        } else if self.baseline.as_ref() == Some(&self.source) {
            SyncStatus::Synced
        } else {
            SyncStatus::Changed
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Settings {
    pub language: String,
    pub theme: String,
    pub poll_enabled: bool,
    pub poll_minutes: u32,
    pub workspace_id: String,
    pub owner: String,
    pub lark_path: String,
    pub tapd_path: String,
    pub tapd_endpoint: String,
    /// 最近一次已验证的账户命名空间，仅用于离线展示缓存，不能作为写入授权。
    #[serde(default)]
    pub last_profile: Option<String>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            language: "zh-CN".into(),
            theme: "system".into(),
            poll_enabled: false,
            poll_minutes: 15,
            workspace_id: String::new(),
            owner: String::new(),
            lark_path: "lark-cli".into(),
            tapd_path: "tapd-cli".into(),
            tapd_endpoint: "https://api.tapd.cn".into(),
            last_profile: None,
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Snapshot {
    pub settings: Settings,
    pub records: Vec<SyncRecord>,
    pub last_checked: Option<String>,
}

// 网关保持同步；桌面端必须在后台 actor 线程调用，不能阻塞 UI。
pub trait Gateway: Send {
    /// 仅做本地预检；不得进行外部写入，失败时不生成不确定日志。
    fn validate_draft(&self, draft: &Draft) -> Result<()> {
        validate_draft(draft)
    }
    /// 实际传输应覆盖为单条读取；默认实现兼容已有网关。
    fn get_source(&self, guid: &str) -> Result<SourceTask> {
        validated_tasks(self)?
            .into_iter()
            .find(|source| source.guid == guid)
            .ok_or_else(|| anyhow!("Source disappeared; refresh and review again"))
    }
    fn fetch_tasks(&self) -> Result<Vec<SourceTask>>;
    fn get_target(&self, workspace: &str, id: &str, kind: &str) -> Result<TargetTask>;
    fn create_target(&self, draft: &Draft, source: &SourceTask) -> Result<TargetTask>;
    fn update_target(
        &self,
        old: &TargetTask,
        draft: &Draft,
        source: &SourceTask,
    ) -> Result<TargetTask>;
}
pub fn source_marker(guid: &str) -> String {
    format!("[lark-task:{guid}]")
}

// 单行快照使草稿、基线和写入前日志在一次 SQLite upsert 中原子提交。
// ponytail: 当前是单 actor、少量任务；数据量显著增长后再拆分逐任务表。
#[derive(Debug, toasty::Model)]
struct StoredState {
    #[key]
    key: String,
    payload: String,
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct State {
    snapshot: Snapshot,
    #[serde(default)]
    draft_sources: BTreeMap<String, SourceTask>,
}

/// 一个数据库仅由一个后台 actor 持有；调用方按账户及 mock/live 隔离路径。
pub struct Engine {
    db: toasty::Db,
    state: State,
}
impl Engine {
    pub async fn open(path: &Path) -> Result<Self> {
        let mut db = toasty::Db::builder()
            .models(toasty::models!(StoredState))
            .max_pool_size(1)
            .build(toasty_driver_sqlite::Sqlite::open(path))
            .await?;
        // Toasty 0.11 push_schema 不带 IF NOT EXISTS，只在空库建表，绝不 reset。
        // 已有库直接读取；结构或 JSON 不兼容时停止，不能用默认值覆盖用户数据。
        let tables = toasty::sql::query(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        )
        .exec(&mut db)
        .await?;
        if tables.is_empty() {
            db.push_schema().await?;
        }
        let row = StoredState::filter_by_key("snapshot")
            .first()
            .exec(&mut db)
            .await?;
        let state = match row {
            Some(row) => serde_json::from_str(&row.payload)?,
            None => State::default(),
        };
        Ok(Self { db, state })
    }
    pub fn snapshot(&self) -> &Snapshot {
        &self.state.snapshot
    }
    /// 手工恢复不确定写入；远端只读核对，绑定后仍需用户确认一次更新。
    pub async fn bind_existing(
        &mut self,
        gateway: &impl Gateway,
        guid: &str,
        workspace: &str,
        id: &str,
        kind: &str,
    ) -> Result<()> {
        let index = self.index(guid)?;
        let record = &self.snapshot().records[index];
        ensure!(record.uncertain, "Only uncertain writes need recovery");
        let draft = record
            .draft
            .as_ref()
            .ok_or_else(|| anyhow!("Missing recovery draft"))?;
        ensure!(
            draft.workspace_id == workspace,
            "Recovery workspace does not match the attempted write"
        );
        if let Some(old) = &record.target {
            ensure!(
                old.id == id && old.kind == kind,
                "Cannot bind a different target for an uncertain update"
            );
        }
        let target = gateway.get_target(workspace, id, kind)?;
        validate_target(&target)?;
        ensure!(
            target.id == id && target.kind == kind,
            "Target identity changed"
        );
        ensure!(
            target.description.contains(&source_marker(guid)),
            "Source marker missing: inspect TAPD manually; automatic retry remains blocked"
        );
        ensure!(
            !self
                .snapshot()
                .records
                .iter()
                .enumerate()
                .any(|(other, record)| other != index
                    && record.target_workspace.as_deref() == Some(workspace)
                    && record
                        .target
                        .as_ref()
                        .is_some_and(|target| target.id == id && target.kind == kind)),
            "Target is already bound to another source"
        );
        let mut state = self.state.clone();
        let record = &mut state.snapshot.records[index];
        record.target = Some(target);
        record.target_workspace = Some(workspace.into());
        // 读取到对象不等于确认所有字段写入成功；Changed 强制下一次人工核对。
        record.baseline = None;
        record.uncertain = false;
        record.last_error = None;
        self.persist(state).await
    }

    // 先落盘再替换内存；磁盘错误不得把 UI 推进到尚未持久化的状态。
    async fn persist(&mut self, state: State) -> Result<()> {
        let payload = serde_json::to_string(&state)?;
        StoredState::upsert_by_key("snapshot")
            .payload(payload)
            .exec(&mut self.db)
            .await?;
        self.state = state;
        Ok(())
    }
    fn index(&self, guid: &str) -> Result<usize> {
        self.snapshot()
            .records
            .iter()
            .position(|r| r.source.guid == guid)
            .ok_or_else(|| anyhow!("Task not found: {guid}"))
    }
    pub async fn refresh(&mut self, gateway: &impl Gateway) -> Result<usize> {
        let tasks = validated_tasks(gateway)?;
        let mut state = self.state.clone();
        let mut discovered = 0;
        for source in tasks {
            if let Some(record) = state
                .snapshot
                .records
                .iter_mut()
                .find(|r| r.source.guid == source.guid)
            {
                // 保留导入基线和人工草稿；缺失任务也不会从缓存删除。
                record.source = source;
            } else {
                state.snapshot.records.push(SyncRecord {
                    source,
                    baseline: None,
                    target: None,
                    target_workspace: None,
                    draft: None,
                    uncertain: false,
                    last_error: None,
                });
                discovered += 1;
            }
        }
        state.snapshot.last_checked = Some(Utc::now().to_rfc3339());
        self.persist(state).await?;
        Ok(discovered)
    }
    pub async fn save_settings(&mut self, settings: Settings) -> Result<()> {
        ensure!(
            ["zh-CN", "en"].contains(&settings.language.as_str()),
            "Unsupported language"
        );
        ensure!(
            ["system", "light", "dark"].contains(&settings.theme.as_str()),
            "Unsupported theme"
        );
        ensure!(
            (1..=1440).contains(&settings.poll_minutes),
            "Polling interval must be 1..1440 minutes"
        );
        ensure!(
            !settings.poll_enabled || !settings.workspace_id.trim().is_empty(),
            "Configure a workspace before enabling polling"
        );
        let mut state = self.state.clone();
        state.snapshot.settings = settings;
        self.persist(state).await
    }
    /// 兼容旧调用方；已有过期草稿必须显式丢弃或携带来源重新保存。
    pub async fn save_draft(&mut self, guid: &str, draft: &Draft) -> Result<()> {
        self.prepare(guid)?;
        let source = self.snapshot().records[self.index(guid)?].source.clone();
        self.save_draft_for_source(guid, draft, &source).await
    }
    /// expected_source 必须是打开编辑器时实际展示的来源，不可在保存时补取。
    /// 明确核对最新来源后，可通过此方法替换旧草稿并重建草稿基线。
    pub async fn save_draft_for_source(
        &mut self,
        guid: &str,
        draft: &Draft,
        expected_source: &SourceTask,
    ) -> Result<()> {
        let index = self.index(guid)?;
        ensure!(
            &self.snapshot().records[index].source == expected_source,
            "Source changed since the editor was opened; reload and review again"
        );
        ensure!(
            !self.snapshot().records[index].uncertain,
            "Uncertain write: bind the existing remote task before editing"
        );
        let mut state = self.state.clone();
        state.snapshot.records[index].draft = Some(draft.clone());
        state
            .draft_sources
            .insert(guid.into(), state.snapshot.records[index].source.clone());
        self.persist(state).await
    }
    /// 丢弃草稿后 prepare 使用最新来源；不允许丢弃恢复不确定写入所需的信息。
    pub async fn discard_draft(&mut self, guid: &str) -> Result<()> {
        let index = self.index(guid)?;
        ensure!(
            !self.snapshot().records[index].uncertain,
            "Uncertain write: bind the existing remote task before discarding the draft"
        );
        let mut state = self.state.clone();
        state.snapshot.records[index].draft = None;
        state.draft_sources.remove(guid);
        self.persist(state).await
    }
    pub fn prepare(&self, guid: &str) -> Result<Draft> {
        let record = &self.snapshot().records[self.index(guid)?];
        // 不能把旧草稿返回给新编辑器，再通过保存悄悄洗成最新来源的草稿。
        ensure!(
            record.draft.is_none() || self.state.draft_sources.get(guid) == Some(&record.source),
            "Saved draft is stale; discard it and reload the latest source, or explicitly review and save for the latest source"
        );
        Ok(record.draft.clone().unwrap_or_else(|| Draft {
            title: record.source.title.clone(),
            description: record.source.description.clone(),
            workspace_id: record
                .target_workspace
                .clone()
                .unwrap_or_else(|| self.snapshot().settings.workspace_id.clone()),
            owner: record.target.as_ref().map_or_else(
                || self.snapshot().settings.owner.clone(),
                |t| t.owner.clone(),
            ),
            due: record.source.due.clone(),
        }))
    }
    pub fn preview_target(&self, gateway: &impl Gateway, guid: &str) -> Result<Option<TargetTask>> {
        let record = &self.snapshot().records[self.index(guid)?];
        match &record.target {
            Some(target) => Ok(Some(
                gateway.get_target(
                    record
                        .target_workspace
                        .as_deref()
                        .ok_or_else(|| anyhow!("Missing target workspace"))?,
                    &target.id,
                    &target.kind,
                )?,
            )),
            None => Ok(None),
        }
    }
    pub async fn commit(
        &mut self,
        gateway: &impl Gateway,
        guid: &str,
        draft: &Draft,
        expected_target: Option<TargetTask>,
    ) -> Result<()> {
        let index = self.index(guid)?;
        let record = self.snapshot().records[index].clone();
        ensure!(
            !record.uncertain,
            "Uncertain write: do not retry; inspect TAPD and bind the existing remote task"
        );
        ensure!(
            record.status() != SyncStatus::Synced,
            "Already imported; duplicate writes are blocked"
        );
        validate_draft(draft)?;
        gateway.validate_draft(draft)?;
        ensure!(
            record.draft.as_ref() == Some(draft),
            "Save the draft before confirming"
        );
        ensure!(
            self.state.draft_sources.get(guid) == Some(&record.source),
            "Source changed since the draft was saved; review and save again"
        );
        // 提交前重读所选来源；避免全列表读取，但仍校验身份及完整字段。
        let fresh_source = gateway.get_source(guid)?;
        validate_source(&fresh_source)?;
        ensure!(
            fresh_source == record.source,
            "Source changed or disappeared; refresh and review again"
        );
        let current_target = match &record.target {
            Some(old) => {
                ensure!(
                    record.target_workspace.as_deref() == Some(draft.workspace_id.as_str()),
                    "Cannot move an imported task to another workspace"
                );
                let fresh = gateway.get_target(&draft.workspace_id, &old.id, &old.kind)?;
                ensure!(
                    fresh.id == old.id && fresh.kind == old.kind,
                    "Target identity changed"
                );
                // 新预览可明确接受历史差异，但预览后发生的远端修改必须阻止。
                ensure!(
                    expected_target.as_ref() == Some(&fresh),
                    "Target changed since preview; compare and confirm again"
                );
                Some(fresh)
            }
            None => {
                ensure!(
                    expected_target.is_none(),
                    "Unexpected target for a new import"
                );
                None
            }
        };
        // 外部副作用之前持久化日志；崩溃、超时及最终落盘失败都停在不确定状态。
        let mut journal = self.state.clone();
        journal.snapshot.records[index].uncertain = true;
        journal.snapshot.records[index].last_error =
            Some("Write started; inspect TAPD before retrying".into());
        self.persist(journal).await?;
        let result = match current_target {
            Some(old) => gateway.update_target(&old, draft, &record.source),
            None => gateway.create_target(draft, &record.source),
        };
        let target = match result.and_then(|target| {
            validate_target(&target)?;
            if let Some(old) = &record.target {
                ensure!(
                    target.id == old.id && target.kind == old.kind,
                    "Write returned a different target identity; inspect TAPD before recovery"
                );
            }
            Ok(target)
        }) {
            Ok(target) => target,
            Err(error) => {
                let mut failed = self.state.clone();
                failed.snapshot.records[index].last_error = Some(error.to_string());
                self.persist(failed).await?;
                return Err(error);
            }
        };
        let mut state = self.state.clone();
        let record = &mut state.snapshot.records[index];
        record.target = Some(target);
        record.target_workspace = Some(draft.workspace_id.clone());
        record.baseline = Some(record.source.clone());
        record.draft = None;
        record.uncertain = false;
        record.last_error = None;
        state.draft_sources.remove(guid);
        self.persist(state).await
    }
}

fn validate_due(due: &Option<String>) -> Result<()> {
    if let Some(due) = due {
        let parsed = NaiveDate::parse_from_str(due, "%Y-%m-%d")?;
        ensure!(
            parsed.format("%Y-%m-%d").to_string() == *due,
            "Date must use YYYY-MM-DD"
        );
    }
    Ok(())
}
fn validate_draft(draft: &Draft) -> Result<()> {
    ensure!(!draft.title.trim().is_empty(), "Title is required");
    ensure!(
        !draft.workspace_id.trim().is_empty(),
        "Workspace is required"
    );
    validate_due(&draft.due)
}
fn validate_target(target: &TargetTask) -> Result<()> {
    ensure!(!target.id.trim().is_empty(), "Missing target ID");
    ensure!(
        ["story", "task"].contains(&target.kind.as_str()),
        "Unsupported target kind"
    );
    ensure!(!target.title.trim().is_empty(), "Missing target title");
    validate_due(&target.due)
}
fn validate_source(task: &SourceTask) -> Result<()> {
    ensure!(
        !task.guid.trim().is_empty() && !task.guid.contains(['[', ']', '\n', '\r']),
        "Invalid source GUID"
    );
    ensure!(!task.title.trim().is_empty(), "Missing source title");
    validate_due(&task.due)
}
fn validated_tasks(gateway: &(impl Gateway + ?Sized)) -> Result<Vec<SourceTask>> {
    let tasks = gateway.fetch_tasks()?;
    let mut guids = HashSet::new();
    for task in &tasks {
        validate_source(task)?;
        ensure!(
            guids.insert(&task.guid),
            "Duplicate source GUID: {}",
            task.guid
        );
    }
    Ok(tasks)
}

#[derive(Clone)]
pub struct MockGateway {
    store: Arc<Mutex<MockStore>>,
}
struct MockStore {
    tasks: Vec<SourceTask>,
    targets: BTreeMap<(String, String, String), TargetTask>,
}
impl Default for MockGateway {
    fn default() -> Self {
        Self {
            store: Arc::new(Mutex::new(MockStore {
                tasks: ["设计同步流程", "补充接口测试", "整理发布清单"]
                    .into_iter()
                    .enumerate()
                    .map(|(index, title)| SourceTask {
                        guid: format!("demo-{}", index + 1),
                        title: title.into(),
                        description: format!("来自飞书的演示任务 {}\n保留原始描述。", index + 1),
                        url: format!(
                            "https://applink.feishu.cn/client/todo/detail?guid=demo-{}",
                            index + 1
                        ),
                        due: Some("2026-07-01".into()),
                        completed: false,
                    })
                    .collect(),
                targets: BTreeMap::new(),
            })),
        }
    }
}
impl MockGateway {
    pub fn from_snapshot(snapshot: &Snapshot) -> Self {
        if snapshot.records.is_empty() {
            return Self::default();
        }
        let tasks = snapshot
            .records
            .iter()
            .map(|record| record.source.clone())
            .collect();
        let targets = snapshot
            .records
            .iter()
            .filter_map(|record| {
                Some((
                    (
                        record.target_workspace.clone()?,
                        record.target.as_ref()?.id.clone(),
                        record.target.as_ref()?.kind.clone(),
                    ),
                    record.target.clone()?,
                ))
            })
            .collect();
        Self {
            store: Arc::new(Mutex::new(MockStore { tasks, targets })),
        }
    }
    // 演示按钮与端到端测试共用同一个真实状态变更，不引入测试专用控制开关。
    pub fn change_demo_task(&self) -> Result<()> {
        let mut store = self
            .store
            .lock()
            .map_err(|_| anyhow!("Mock store lock poisoned"))?;
        let task = store
            .tasks
            .first_mut()
            .ok_or_else(|| anyhow!("No demo tasks"))?;
        task.title.push_str(" · 已更新");
        task.description.push_str("\n新增验收要求。");
        Ok(())
    }
}
impl Gateway for MockGateway {
    fn fetch_tasks(&self) -> Result<Vec<SourceTask>> {
        Ok(self
            .store
            .lock()
            .map_err(|_| anyhow!("Mock store lock poisoned"))?
            .tasks
            .clone())
    }
    fn get_target(&self, workspace: &str, id: &str, kind: &str) -> Result<TargetTask> {
        self.store
            .lock()
            .map_err(|_| anyhow!("Mock store lock poisoned"))?
            .targets
            .get(&(workspace.into(), id.into(), kind.into()))
            .cloned()
            .ok_or_else(|| anyhow!("Target not found"))
    }
    fn create_target(&self, draft: &Draft, source: &SourceTask) -> Result<TargetTask> {
        let mut store = self
            .store
            .lock()
            .map_err(|_| anyhow!("Mock store lock poisoned"))?;
        let id = format!("mock-{}", source.guid);
        let key = (draft.workspace_id.clone(), id.clone(), "story".into());
        ensure!(!store.targets.contains_key(&key), "Mock duplicate target");
        let target = mock_target(id, "story".into(), draft, source);
        store.targets.insert(key, target.clone());
        Ok(target)
    }
    fn update_target(
        &self,
        old: &TargetTask,
        draft: &Draft,
        source: &SourceTask,
    ) -> Result<TargetTask> {
        let mut store = self
            .store
            .lock()
            .map_err(|_| anyhow!("Mock store lock poisoned"))?;
        let key = (draft.workspace_id.clone(), old.id.clone(), old.kind.clone());
        ensure!(store.targets.contains_key(&key), "Mock target not found");
        let target = mock_target(old.id.clone(), old.kind.clone(), draft, source);
        store.targets.insert(key, target.clone());
        Ok(target)
    }
}
fn mock_target(id: String, kind: String, draft: &Draft, source: &SourceTask) -> TargetTask {
    TargetTask {
        url: format!(
            "https://www.tapd.cn/{}/prong/stories/view/{id}",
            draft.workspace_id
        ),
        id,
        kind,
        title: draft.title.clone(),
        description: format!("{}\n\n{}", draft.description, source_marker(&source.guid)),
        owner: draft.owner.clone(),
        due: draft.due.clone(),
    }
}
#[cfg(test)]
mod tests;
