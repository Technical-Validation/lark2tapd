pub mod media;
pub use media::{copy_attachment, read_attachment, save_pasted_image, verify_attachment};

use anyhow::{Result, anyhow, ensure};
use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourcePerson {
    pub id: String,
    pub name: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SourceMetadata {
    pub creator: Option<SourcePerson>,
    pub assignees: Vec<SourcePerson>,
    // 保留飞书原始毫秒时间戳；显示时再按用户本地时区转换。
    pub created_at: Option<String>,
    pub completed_at: Option<String>,
    pub due_at: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TapdWorkspace {
    pub id: String,
    pub name: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TapdMember {
    pub account: String,
    pub name: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemberCache {
    pub items: Vec<TapdMember>,
    pub fetched_at: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirectoryCache {
    pub workspaces: Vec<TapdWorkspace>,
    /// 管理员枚举不包含普通成员项目；显式补充的项目跨刷新保留。
    #[serde(default)]
    pub manual_workspaces: Vec<TapdWorkspace>,
    pub workspaces_fetched_at: Option<String>,
    pub members: BTreeMap<String, MemberCache>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceTask {
    pub guid: String,
    pub title: String,
    pub description: String,
    pub url: String,
    pub due: Option<String>,
    pub completed: bool,
    #[serde(default)]
    pub metadata: SourceMetadata,
}
impl SourceTask {
    // 仅比较稳定身份；缺少身份或同名不能推断为本人。
    pub fn is_related_to(&self, user_id: &str) -> bool {
        !user_id.trim().is_empty()
            && (self
                .metadata
                .creator
                .as_ref()
                .is_some_and(|person| person.id == user_id)
                || self
                    .metadata
                    .assignees
                    .iter()
                    .any(|person| person.id == user_id))
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Draft {
    pub title: String,
    #[serde(default)]
    pub title_prefix: String,
    #[serde(default)]
    pub attachments: Vec<DraftAttachment>,
    pub description: String,
    pub workspace_id: String,
    pub owner: String,
    pub due: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DraftAttachment {
    pub id: String,
    pub name: String,
    pub path: String,
    pub size: u64,
    pub is_image: bool,
    #[serde(default)]
    pub remote_id: Option<String>,
    #[serde(default)]
    pub remote_url: Option<String>,
    #[serde(default)]
    pub uncertain: bool,
}
impl Draft {
    // 只折叠所选前缀，不吞掉标题中的其他用户文本。
    pub fn rendered_title(&self) -> String {
        if self.title_prefix.is_empty() {
            return self.title.clone();
        }
        let prefix = &self.title_prefix;
        format!("{prefix}{}", self.title.trim_start_matches(prefix.as_str()))
    }
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
    /// 图片已上传不代表正文已回填；独立日志防止失败或重启后误报完成。
    #[serde(default)]
    pub media_body_pending: bool,
    #[serde(default)]
    pub title_prefix: String,
    #[serde(default)]
    pub last_draft: Option<Draft>,
    #[serde(default)]
    pub in_scope: bool,
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
    /// 任务正文成功不代表附件成功；UI 单独显示待上传或需人工核对状态。
    pub fn has_pending_attachments(&self) -> bool {
        self.media_body_pending
            || self.last_draft.as_ref().is_some_and(|draft| {
                draft.attachments.iter().any(|attachment| {
                    attachment.uncertain
                        || ![&attachment.remote_id, &attachment.remote_url]
                            .iter()
                            .any(|value| {
                                value.as_ref().is_some_and(|value| !value.trim().is_empty())
                            })
                })
            })
    }
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
/// 历史验证记录，不是当前授权；binding 仅用于配置变更后失效。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerificationReceipt {
    pub at: String,
    pub binding: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Settings {
    #[serde(default)]
    pub connection_verifications: BTreeMap<String, VerificationReceipt>,
    #[serde(default)]
    pub workspace_prefixes: BTreeMap<String, Vec<String>>,
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
            connection_verifications: BTreeMap::new(),
            workspace_prefixes: BTreeMap::new(),
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
    #[serde(default)]
    pub directory: DirectoryCache,
}

// 网关保持同步；桌面端必须在后台 actor 线程调用，不能阻塞 UI。
pub trait Gateway: Send {
    fn list_workspaces(&self) -> Result<Vec<TapdWorkspace>> {
        Err(anyhow!("Workspace enumeration is not supported"))
    }
    fn list_members(&self, _workspace: &str) -> Result<Vec<TapdMember>> {
        Err(anyhow!("Member enumeration is not supported"))
    }
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
#[derive(Clone, Serialize, Deserialize)]
struct State {
    // 缺失版本只代表旧库；新建状态从原样前缀版本开始。
    #[serde(default)]
    prefix_format_version: u32,
    snapshot: Snapshot,
    #[serde(default)]
    draft_sources: BTreeMap<String, SourceTask>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            prefix_format_version: 1,
            snapshot: Snapshot::default(),
            draft_sources: BTreeMap::new(),
        }
    }
}
impl State {
    fn migrate_prefixes(&mut self) -> Result<bool> {
        ensure!(
            self.prefix_format_version <= 1,
            "Unsupported prefix format version"
        );
        if self.prefix_format_version == 1 {
            return Ok(false);
        }
        // 只转换选择值，不修改已同步标题或源文；保持旧包装和精确去重语义。
        let wrap = |prefix: &mut String| {
            if !prefix.is_empty() {
                *prefix = format!("【{prefix}】");
            }
        };
        for prefixes in self.snapshot.settings.workspace_prefixes.values_mut() {
            for prefix in prefixes {
                wrap(prefix);
            }
        }
        for record in &mut self.snapshot.records {
            wrap(&mut record.title_prefix);
            for draft in [&mut record.draft, &mut record.last_draft]
                .into_iter()
                .flatten()
            {
                wrap(&mut draft.title_prefix);
            }
        }
        self.prefix_format_version = 1;
        Ok(true)
    }
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
        let mut state: State = match row {
            Some(row) => serde_json::from_str(&row.payload)?,
            None => State::default(),
        };
        let migrated = state.migrate_prefixes()?;
        let mut engine = Self { db, state };
        if migrated {
            engine.persist(engine.state.clone()).await?;
        }
        Ok(engine)
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
        // 只有完整读取及验证成功后才更新范围；旧映射和不确定日志永久保留。
        for record in &mut state.snapshot.records {
            record.in_scope = false;
        }
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
                record.in_scope = true;
            } else {
                state.snapshot.records.push(SyncRecord {
                    media_body_pending: false,
                    in_scope: true,
                    title_prefix: String::new(),
                    last_draft: None,
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
    // 全量目录先校验再原子替换；网络、校验或落盘失败都保留旧缓存。
    pub async fn refresh_workspaces(&mut self, gateway: &impl Gateway) -> Result<()> {
        let mut workspaces = gateway.list_workspaces()?;
        validate_directory(
            workspaces
                .iter()
                .map(|item| (item.id.as_str(), item.name.as_str())),
        )?;
        let mut state = self.state.clone();
        for manual in &state.snapshot.directory.manual_workspaces {
            if !workspaces.iter().any(|w| w.id == manual.id) {
                workspaces.push(manual.clone());
            }
        }
        state.snapshot.directory.workspaces = workspaces;
        state.snapshot.directory.workspaces_fetched_at = Some(Utc::now().to_rfc3339());
        self.persist(state).await
    }
    pub async fn refresh_members(&mut self, gateway: &impl Gateway, workspace: &str) -> Result<()> {
        ensure!(!workspace.trim().is_empty(), "Workspace is required");
        let items = gateway.list_members(workspace)?;
        validate_directory(
            items
                .iter()
                .map(|item| (item.account.as_str(), item.name.as_str())),
        )?;
        let mut state = self.state.clone();
        state.snapshot.directory.members.insert(
            workspace.into(),
            MemberCache {
                items,
                fetched_at: Utc::now().to_rfc3339(),
            },
        );
        self.persist(state).await
    }
    // 调用方先验证项目访问权限；手工补充不冒充全量目录刷新。
    pub async fn add_workspace(&mut self, workspace: TapdWorkspace) -> Result<()> {
        validate_directory([(workspace.id.as_str(), workspace.name.as_str())])?;
        let mut state = self.state.clone();
        for items in [
            &mut state.snapshot.directory.workspaces,
            &mut state.snapshot.directory.manual_workspaces,
        ] {
            if let Some(existing) = items.iter_mut().find(|item| item.id == workspace.id) {
                *existing = workspace.clone();
            } else {
                items.push(workspace.clone());
            }
        }
        self.persist(state).await
    }
    /// 旧选中值可以显示，但不能绕过项目及成员缓存校验进入远端写入。
    pub fn validate_destination(&self, draft: &Draft) -> Result<()> {
        let directory = &self.snapshot().directory;
        ensure!(
            directory
                .workspaces
                .iter()
                .any(|w| w.id == draft.workspace_id),
            "Refresh project options or verify and add this project before importing"
        );
        if !draft.owner.is_empty() {
            ensure!(
                directory
                    .members
                    .get(&draft.workspace_id)
                    .is_some_and(|c| c.items.iter().any(|m| m.account == draft.owner)),
                "Refresh assignees and select a current project member before importing"
            );
        }
        Ok(())
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
        // 所有项目一起校验，失败不改变内存或磁盘中的旧配置。
        for (workspace, prefixes) in &settings.workspace_prefixes {
            ensure!(
                !workspace.trim().is_empty() && workspace.trim() == workspace,
                "Invalid prefix workspace"
            );
            let mut seen = HashSet::new();
            for prefix in prefixes {
                validate_prefix(prefix)?;
                ensure!(seen.insert(prefix), "Duplicate title prefix");
            }
        }
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
            self.snapshot().records[index].in_scope,
            "Task is outside the current self scope; refresh first"
        );
        ensure!(
            &self.snapshot().records[index].source == expected_source,
            "Source changed since the editor was opened; reload and review again"
        );
        ensure!(
            !self.snapshot().records[index].uncertain,
            "Uncertain write: bind the existing remote task before editing"
        );
        let mut state = self.state.clone();
        let mut draft = draft.clone();
        // 旧编辑器只能改正文/选择，不能回滚已落盘的上传日志和回执。
        if let Some(last) = &state.snapshot.records[index].last_draft {
            for attachment in &mut draft.attachments {
                if let Some(saved) = last
                    .attachments
                    .iter()
                    .find(|saved| saved.id == attachment.id)
                {
                    ensure!(
                        saved.path == attachment.path
                            && saved.name == attachment.name
                            && saved.size == attachment.size
                            && saved.is_image == attachment.is_image,
                        "Attachment identity changed since the last submission"
                    );
                    attachment.remote_id = saved.remote_id.clone();
                    attachment.remote_url = saved.remote_url.clone();
                    attachment.uncertain = saved.uncertain;
                }
            }
        }
        state.snapshot.records[index].draft = Some(draft);
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
        ensure!(
            record.in_scope,
            "Task is outside the current self scope; refresh first"
        );
        // 不能把旧草稿返回给新编辑器，再通过保存悄悄洗成最新来源的草稿。
        ensure!(
            record.draft.is_none() || self.state.draft_sources.get(guid) == Some(&record.source),
            "Saved draft is stale; discard it and reload the latest source, or explicitly review and save for the latest source"
        );
        // 未改变的来源继续保留上次成功提交的人工作品；来源变化时重建正文。
        if record.draft.is_none()
            && record.baseline.as_ref() == Some(&record.source)
            && let Some(last) = &record.last_draft
        {
            return Ok(last.clone());
        }
        Ok(record.draft.clone().unwrap_or_else(|| Draft {
            title: record.source.title.clone(),
            title_prefix: record.title_prefix.clone(),
            attachments: record
                .last_draft
                .as_ref()
                .map(|d| d.attachments.clone())
                .unwrap_or_default(),
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
    pub async fn set_media_body_pending(&mut self, guid: &str, pending: bool) -> Result<()> {
        let index = self.index(guid)?;
        let mut state = self.state.clone();
        ensure!(
            state.snapshot.records[index].target.is_some(),
            "Persist target before recording image finalization"
        );
        state.snapshot.records[index].media_body_pending = pending;
        self.persist(state).await
    }
    /// 调用方已展示并再次核对远端版本；此处只接受同一目标，绝不执行覆盖写入。
    pub async fn accept_target(&mut self, guid: &str, target: &TargetTask) -> Result<()> {
        let index = self.index(guid)?;
        let record = &self.snapshot().records[index];
        ensure!(
            !record.uncertain,
            "Recover uncertain write before accepting remote edits"
        );
        let old = record
            .target
            .as_ref()
            .ok_or_else(|| anyhow!("No existing target"))?;
        ensure!(
            old.id == target.id && old.kind == target.kind,
            "Cannot accept a different target identity"
        );
        validate_target(target)?;
        let mut state = self.state.clone();
        state.snapshot.records[index].target = Some(target.clone());
        self.persist(state).await
    }
    /// 任务映射持久化后逐个记录附件上传状态；调用方必须先记不确定再发起上传。
    pub async fn attachment_progress(
        &mut self,
        guid: &str,
        attachment: &DraftAttachment,
    ) -> Result<()> {
        let index = self.index(guid)?;
        let mut state = self.state.clone();
        let record = &mut state.snapshot.records[index];
        ensure!(
            record.target.is_some() && !record.uncertain,
            "Persist the target before uploading attachments"
        );
        let last = record
            .last_draft
            .as_mut()
            .ok_or_else(|| anyhow!("Missing successful draft"))?;
        let saved = last
            .attachments
            .iter_mut()
            .find(|item| item.id == attachment.id)
            .ok_or_else(|| anyhow!("Attachment is not part of the submitted draft"))?;
        ensure!(
            saved.name == attachment.name
                && saved.path == attachment.path
                && saved.size == attachment.size
                && saved.is_image == attachment.is_image,
            "Attachment identity changed during upload"
        );
        *saved = attachment.clone();
        // 已打开的待提交草稿也同步回执，避免下次保存时无意重复上传。
        if let Some(draft) = &mut record.draft {
            for item in &mut draft.attachments {
                if item.id == attachment.id {
                    ensure!(
                        item.path == attachment.path && item.size == attachment.size,
                        "Draft attachment identity changed"
                    );
                    *item = attachment.clone();
                }
            }
        }
        self.persist(state).await
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
            record.in_scope,
            "Task is outside the current self scope; refresh first"
        );
        ensure!(
            !record.uncertain,
            "Uncertain write: do not retry; inspect TAPD and bind the existing remote task"
        );
        validate_draft(draft)?;
        // 不能跨项目复用选择；已删除的配置也不能经旧草稿绕过校验。
        ensure!(
            draft.title_prefix.is_empty()
                || self
                    .snapshot()
                    .settings
                    .workspace_prefixes
                    .get(&draft.workspace_id)
                    .is_some_and(|prefixes| prefixes.contains(&draft.title_prefix)),
            "Select a configured title prefix for this project"
        );
        gateway.validate_draft(draft)?;
        self.validate_destination(draft)?;
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
                // 只接受调用方明确核对的目标版本；不能静默覆盖此后出现的远端修改。
                ensure!(
                    expected_target.as_ref() == Some(&fresh),
                    "TAPD changed since the accepted version; review its current contents before updating"
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
        record.title_prefix = draft.title_prefix.clone();
        record.last_draft = Some(draft.clone());
        record.baseline = Some(record.source.clone());
        record.draft = None;
        record.uncertain = false;
        record.last_error = None;
        state.draft_sources.remove(guid);
        self.persist(state).await
    }
}

fn validate_directory<'a>(items: impl IntoIterator<Item = (&'a str, &'a str)>) -> Result<()> {
    let mut ids = HashSet::new();
    for (id, name) in items {
        ensure!(
            !id.trim().is_empty() && !name.trim().is_empty(),
            "Directory identity and name are required"
        );
        ensure!(ids.insert(id), "Duplicate directory identity: {id}");
    }
    Ok(())
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
fn validate_prefix(prefix: &str) -> Result<()> {
    ensure!(
        !prefix.trim().is_empty()
            && prefix.chars().count() <= 66
            && !prefix.chars().any(char::is_control),
        "Title prefixes must be 1..66 characters, contain non-whitespace text, and have no control characters"
    );
    Ok(())
}
fn validate_draft(draft: &Draft) -> Result<()> {
    // 同一任务最多十个附件；重复内容 ID 会使上传日志和回执归属含糊，必须拒绝。
    ensure!(
        draft.attachments.len() <= 10,
        "A draft may contain at most 10 attachments"
    );
    let mut attachment_ids = HashSet::new();
    for attachment in &draft.attachments {
        ensure!(
            !attachment.id.is_empty() && attachment_ids.insert(&attachment.id),
            "Attachment IDs must be nonempty and unique"
        );
    }
    if !draft.title_prefix.is_empty() {
        validate_prefix(&draft.title_prefix)?;
    }
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
                        metadata: SourceMetadata {
                            creator: Some(SourcePerson {
                                id: "demo-self".into(),
                                name: "演示用户".into(),
                            }),
                            assignees: vec![SourcePerson {
                                id: "demo-self".into(),
                                name: "演示用户".into(),
                            }],
                            created_at: Some("1780272000000".into()),
                            completed_at: None,
                            due_at: Some("1782864000000".into()),
                        },
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
            .map(|record| {
                let mut source = record.source.clone();
                if source.title.ends_with(" · 已更新") {
                    source.title = Self::updated_title(&source.title);
                }
                source
            })
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
    fn updated_title(title: &str) -> String {
        format!("{} · 已更新", title.trim_end_matches(" · 已更新"))
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
        // 折叠旧缓存累积的标记，描述仍递增以模拟新版本。
        task.title = Self::updated_title(&task.title);
        task.description.push_str("\n新增验收要求。");
        Ok(())
    }
}
impl Gateway for MockGateway {
    fn list_workspaces(&self) -> Result<Vec<TapdWorkspace>> {
        Ok(vec![
            TapdWorkspace {
                id: "demo".into(),
                name: "演示项目".into(),
            },
            TapdWorkspace {
                id: "demo-2".into(),
                name: "演示项目二".into(),
            },
        ])
    }
    fn list_members(&self, workspace: &str) -> Result<Vec<TapdMember>> {
        ensure!(
            ["demo", "demo-2"].contains(&workspace),
            "Workspace not found"
        );
        // 同名成员使用不同账号，便于演示项目隔离与稳定身份选择。
        let account = if workspace == "demo" { "alice" } else { "bob" };
        Ok(vec![TapdMember {
            account: account.into(),
            name: "演示成员".into(),
        }])
    }
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
        title: draft.rendered_title(),
        description: format!("{}\n\n{}", draft.description, source_marker(&source.guid)),
        owner: draft.owner.clone(),
        due: draft.due.clone(),
    }
}
#[cfg(test)]
mod prefix_tests;
#[cfg(test)]
mod tests;
