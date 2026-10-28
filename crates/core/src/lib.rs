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
// 消息正文仍以任务快照为准；通过标识补查发送信息，不拿任务信息代替。
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SourceMessage {
    pub message_id: String,
    pub content: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SourceMessageDetails {
    pub sender: Option<SourcePerson>,
    pub sent_at: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, Eq)]
#[serde(default)]
pub struct SourceMetadata {
    pub creator: Option<SourcePerson>,
    pub assignees: Vec<SourcePerson>,
    pub followers: Vec<SourcePerson>,
    pub source_messages: Vec<SourceMessage>,
    pub updated_at: Option<String>,
    // 保留飞书原始毫秒时间戳；显示时再按用户本地时区转换。
    pub created_at: Option<String>,
    pub completed_at: Option<String>,
    pub due_at: Option<String>,
    pub resolved_names: BTreeMap<String, String>,
    pub message_details: BTreeMap<String, SourceMessageDetails>,
    // 数字来源消息 -> 原文匹配出的独立消息；只用于展示，不能当作来源 ID 转换。
    pub matched_messages: BTreeMap<String, Vec<SourceMessage>>,
    pub source_images: BTreeMap<String, DraftAttachment>,
}
impl SourceMetadata {
    pub fn matched_messages_for(&self, source: &SourceMessage) -> Option<&[SourceMessage]> {
        let messages = self.matched_messages.get(&source.message_id)?;
        // 旧缓存与迟到结果必须仍逐字覆盖当前原文；不能展示只匹配了一部分的会话。
        (!messages.is_empty()
            && messages.iter().all(|message| !message.content.is_empty())
            && messages
                .iter()
                .map(|message| message.content.as_str())
                .collect::<Vec<_>>()
                .join("\n\n")
                == source.content)
            .then_some(messages.as_slice())
    }

    // CLI 的 localized_name 可能退化为 ID；展示层不把它作为姓名。
    pub fn person_name<'a>(&'a self, person: &'a SourcePerson) -> Option<&'a str> {
        [
            &person.name,
            self.resolved_names.get(&person.id).unwrap_or(&person.name),
        ]
        .into_iter()
        .find(|name| !name.trim().is_empty() && **name != person.id)
        .map(String::as_str)
    }
}
impl PartialEq for SourceMetadata {
    // 补查结果仅用于显示：权限变化、超时或人员改名不能令草稿过期或触发待同步。
    fn eq(&self, other: &Self) -> bool {
        (
            &self.creator,
            &self.assignees,
            &self.followers,
            &self.source_messages,
            &self.updated_at,
            &self.created_at,
            &self.completed_at,
            &self.due_at,
        ) == (
            &other.creator,
            &other.assignees,
            &other.followers,
            &other.source_messages,
            &other.updated_at,
            &other.created_at,
            &other.completed_at,
            &other.due_at,
        )
    }
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
pub struct TapdWorkitemType {
    #[serde(default = "story_kind")]
    pub kind: String,
    /// 空值表示该种类不使用类型 ID（例如传统 Bug）。
    pub id: String,
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
    #[serde(default)]
    pub workitem_types: BTreeMap<String, Vec<TapdWorkitemType>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceTask {
    pub guid: String,
    pub title: String,
    pub description: String,
    pub url: String,
    pub due: Option<String>,
    pub completed: bool,
    // 详情随字段扩展，不让它同时膨胀每个携带任务的后台队列消息。
    #[serde(default)]
    pub metadata: Box<SourceMetadata>,
}
impl SourceTask {
    pub fn needs_conversation_resolution(&self) -> bool {
        self.metadata.source_messages.iter().any(|message| {
            !message.message_id.is_empty()
                && message.message_id.bytes().all(|byte| byte.is_ascii_digit())
                && !message.content.trim().is_empty()
                && self
                    .metadata
                    .matched_messages_for(message)
                    .is_none_or(|matched| {
                        matched.iter().any(|message| {
                            self.metadata
                                .message_details
                                .get(&message.message_id)
                                .and_then(|details| details.sender.as_ref())
                                .and_then(|person| self.metadata.person_name(person))
                                .is_none()
                        })
                    })
        })
    }

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
    #[serde(default = "story_kind")]
    pub workitem_kind: String,
    #[serde(default)]
    pub workitem_type_id: String,
    #[serde(default = "include_source_link_default")]
    pub include_source_link: bool,
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
    /// 来源派生图片的待上传意图随正文引用存在，保存/重开也不能变成独立附件。
    #[serde(default)]
    pub source_derived: bool,
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
fn story_kind() -> String {
    "story".into()
}

fn include_source_link_default() -> bool {
    true
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
    #[serde(default)]
    pub workitem_type_id: String,
    pub id: String,
    pub url: String,
    pub kind: String,
    pub title: String,
    pub description: String,
    pub owner: String,
    pub due: Option<String>,
}
// 写入已返回合法目标身份，但读回未确认；保留回执不等于宣布同步成功。
pub struct UnconfirmedWrite {
    pub target: TargetTask,
    pub message: String,
}
impl std::fmt::Display for UnconfirmedWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::fmt::Debug for UnconfirmedWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnconfirmedWrite")
            .field("message", &self.message)
            .finish_non_exhaustive()
    }
}
impl std::error::Error for UnconfirmedWrite {}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncRecord {
    /// 整次提交完成后记录；中间正文写入、附件回执和失败不能推进成功时间。
    #[serde(default)]
    pub last_synced_at: Option<String>,
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
    fn list_workitem_types(&self, _workspace: &str) -> Result<Vec<TapdWorkitemType>> {
        Err(anyhow!("Work item types are unavailable"))
    }
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
            record.target.is_some() || target.description.contains(&source_marker(guid)),
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
        for mut source in tasks {
            if let Some(record) = state
                .snapshot
                .records
                .iter_mut()
                .find(|r| r.source.guid == source.guid)
            {
                // 只有同一原生来源版本才复用匹配缓存；正文/成员/更新时间变化即失效。
                if record.source == source {
                    let previous = &record.source.metadata;
                    source.metadata.matched_messages = previous.matched_messages.clone();
                    source.metadata.source_images = previous.source_images.clone();
                    for (id, details) in &previous.message_details {
                        source
                            .metadata
                            .message_details
                            .entry(id.clone())
                            .or_insert_with(|| details.clone());
                    }
                    for (id, name) in &previous.resolved_names {
                        source
                            .metadata
                            .resolved_names
                            .entry(id.clone())
                            .or_insert_with(|| name.clone());
                    }
                }
                // 保留导入基线和人工草稿；缺失任务也不会从缓存删除。
                record.source = source;
                record.in_scope = true;
            } else {
                state.snapshot.records.push(SyncRecord {
                    last_synced_at: None,
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
    pub async fn cache_conversation(&mut self, source: &SourceTask) -> Result<bool> {
        let mut state = self.state.clone();
        let Some(record) = state
            .snapshot
            .records
            .iter_mut()
            .find(|record| record.in_scope && record.source == *source)
        else {
            return Ok(false);
        };
        // 只更新展示补全信息，不修改原文、草稿、同步基线或任务范围。
        for id in source.metadata.matched_messages.keys() {
            ensure!(
                source
                    .metadata
                    .source_messages
                    .iter()
                    .any(|message| &message.message_id == id
                        && source.metadata.matched_messages_for(message).is_some()),
                "Incomplete conversation match"
            );
        }
        record
            .source
            .metadata
            .matched_messages
            .clone_from(&source.metadata.matched_messages);
        record
            .source
            .metadata
            .message_details
            .extend(source.metadata.message_details.clone());
        record
            .source
            .metadata
            .source_images
            .extend(source.metadata.source_images.clone());
        record
            .source
            .metadata
            .resolved_names
            .extend(source.metadata.resolved_names.clone());
        self.persist(state).await?;
        Ok(true)
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
    pub async fn refresh_workitem_types(
        &mut self,
        gateway: &impl Gateway,
        workspace: &str,
    ) -> Result<()> {
        ensure!(!workspace.trim().is_empty(), "Workspace is required");
        let items = gateway.list_workitem_types(workspace)?;
        // 种类和类型 ID 共同决定目录身份；Bug 可没有类型 ID，但不能借用 Story 的身份。
        let mut identities = HashSet::new();
        for item in &items {
            ensure!(
                matches!(item.kind.as_str(), "story" | "bug")
                    && (item.kind == "bug" || !item.id.trim().is_empty())
                    && !item.name.trim().is_empty()
                    && identities.insert((&item.kind, &item.id)),
                "Invalid or duplicate work item type"
            );
        }
        let mut state = self.state.clone();
        state
            .snapshot
            .directory
            .workitem_types
            .insert(workspace.into(), items);
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
        Self::validate_settings(&settings)?;
        let mut state = self.state.clone();
        state.snapshot.settings = settings;
        self.persist(state).await
    }

    /// 应用已由全局 preferences 提交的权威配置，仅更新运行时副本，不写 profile 库。
    /// 与 save_settings 使用相同校验；不可代替业务数据或日志的持久化。
    /// 后续业务 persist 可携带此副本落盘，但启动/切换 profile 必须重新应用全局配置。
    /// 凭据已被外部存储修改时，也用于立即应用身份失效；不能因偏好保存失败恢复旧授权。
    pub fn apply_committed_settings(&mut self, settings: Settings) -> Result<()> {
        Self::validate_settings(&settings)?;
        self.state.snapshot.settings = settings;
        Ok(())
    }

    fn validate_settings(settings: &Settings) -> Result<()> {
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
        Ok(())
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
            let mut draft = last.clone();
            if let Some(target) = &record.target {
                draft.workitem_kind = target.kind.clone();
                draft.workitem_type_id = target.workitem_type_id.clone();
            }
            return Ok(draft);
        }
        let mut draft = record.draft.clone().unwrap_or_else(|| Draft {
            workitem_kind: record
                .target
                .as_ref()
                .map_or_else(story_kind, |t| t.kind.clone()),
            workitem_type_id: record
                .target
                .as_ref()
                .map(|target| target.workitem_type_id.clone())
                .unwrap_or_default(),
            include_source_link: record
                .last_draft
                .as_ref()
                .is_none_or(|draft| draft.include_source_link),
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
        });
        if let Some(target) = &record.target {
            draft.workitem_kind = target.kind.clone();
            draft.workitem_type_id = target.workitem_type_id.clone();
        }
        Ok(draft)
    }
    pub async fn finish_submission(&mut self, guid: &str) -> Result<()> {
        let index = self.index(guid)?;
        let record = &self.snapshot().records[index];
        ensure!(
            record.target.is_some() && !record.uncertain && !record.has_pending_attachments(),
            "Submission is not fully confirmed"
        );
        let mut state = self.state.clone();
        state.snapshot.records[index].last_synced_at = Some(Utc::now().to_rfc3339());
        self.persist(state).await
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
        if let Some(target) = &record.target {
            let accepted_type = expected_target
                .as_ref()
                .unwrap_or(target)
                .workitem_type_id
                .as_str();
            ensure!(
                target.kind == draft.workitem_kind
                    && (accepted_type.is_empty() || accepted_type == draft.workitem_type_id),
                "Cannot change the type of an imported item"
            );
        } else {
            ensure!(
                self.snapshot()
                    .directory
                    .workitem_types
                    .get(&draft.workspace_id)
                    .is_some_and(
                        |types| types.iter().any(|item| item.kind == draft.workitem_kind
                            && item.id == draft.workitem_type_id)
                    ),
                "Load and select a current TAPD work item type before importing"
            );
        }
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
                // 已返回的合法身份必须保留，但 uncertain 仍为 true，绝不自动重试创建。
                if let Some(receipt) = error.downcast_ref::<UnconfirmedWrite>()
                    && validate_target(&receipt.target).is_ok()
                    && record.target.as_ref().is_none_or(|old| {
                        old.id == receipt.target.id && old.kind == receipt.target.kind
                    })
                {
                    failed.snapshot.records[index].target = Some(receipt.target.clone());
                    failed.snapshot.records[index].target_workspace =
                        Some(draft.workspace_id.clone());
                }
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
        ["story", "task", "bug"].contains(&target.kind.as_str()),
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
                        metadata: Box::new(SourceMetadata {
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
                            ..Default::default()
                        }),
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
    fn list_workitem_types(&self, workspace: &str) -> Result<Vec<TapdWorkitemType>> {
        ensure!(
            ["demo", "demo-2"].contains(&workspace),
            "Workspace not found"
        );
        Ok(vec![
            TapdWorkitemType {
                kind: "story".into(),
                id: if workspace == "demo" { "7" } else { "8" }.into(),
                name: "开发任务".into(),
            },
            TapdWorkitemType {
                kind: "bug".into(),
                id: String::new(),
                name: "Bug".into(),
            },
        ])
    }
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
        let key = (
            draft.workspace_id.clone(),
            id.clone(),
            draft.workitem_kind.clone(),
        );
        ensure!(!store.targets.contains_key(&key), "Mock duplicate target");
        let target = mock_target(id, draft.workitem_kind.clone(), draft, source);
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
        workitem_type_id: draft.workitem_type_id.clone(),
        url: format!(
            "https://www.tapd.cn/{}/prong/{}/view/{id}",
            draft.workspace_id,
            match kind.as_str() {
                "bug" => "bugs",
                "task" => "tasks",
                _ => "stories",
            }
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
