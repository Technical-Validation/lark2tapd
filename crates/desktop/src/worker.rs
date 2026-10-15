//! 串行 actor 拥有数据库与写入日志；只读任务并行取回数据，按账户/配置代数合并。
use anyhow::{Result, bail};
use lark2tapd_bridge::installer::{
    CliTool, InstallationPlan, install_cli, installation_plan, probe_supported_cli,
};
use lark2tapd_bridge::{
    CliGateway, ConnectionReport, Credentials, load_credentials, save_credentials,
};
#[cfg(test)]
use lark2tapd_core::MockGateway;
use lark2tapd_core::{
    Draft, DraftAttachment, Engine, Gateway, Settings, Snapshot, SourceTask, TapdMember,
    TapdWorkspace, TargetTask,
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    #[cfg(test)]
    Mock,
    Live,
}

impl Mode {
    pub fn is_mock(self) -> bool {
        #[cfg(test)]
        {
            self == Self::Mock
        }
        #[cfg(not(test))]
        {
            false
        }
    }
}

pub enum Job {
    LoadProjects,
    LoadMembers(String),
    AddWorkspace(TapdWorkspace),
    CheckWorkspace(TapdWorkspace),
    SetPreference(Preference),
    LocalFile {
        guid: String,
        session: u64,
        path: PathBuf,
    },
    PastedImage {
        guid: String,
        session: u64,
        bytes: Vec<u8>,
        extension: String,
        marker: String,
    },
    PlanInstall(CliTool),
    Install(CliTool),
    CancelInstall,
    Refresh,
    OpenDraft(String, bool, u64),
    CheckTarget {
        guid: String,
        session: u64,
    },
    AcceptTarget {
        guid: String,
        target: TargetTask,
    },
    ClearCredentials(CliTool),
    LoadConnectionStatus,
    SaveDraft(String, Draft, SourceTask),
    Submit(String, Draft, SourceTask),
    SaveAndConnect {
        tool: CliTool,
        credentials: Credentials,
    },
    Check(CliTool),
    ProbeCli(CliTool),
    Recover {
        guid: String,
        workspace: String,
        id: String,
        kind: String,
    },
}

#[derive(Clone)]
pub struct Editor {
    pub session: u64,
    pub source: SourceTask,
    pub draft: Draft,
    pub profile: String,
}

pub enum Preference {
    Language(String),
    Theme(String),
    Polling(bool),
    PollMinutes(u32),
    DefaultDestination { workspace: String, owner: String },
    AddWorkspacePrefix { workspace: String, prefix: String },
    RemoveWorkspacePrefix { workspace: String, prefix: String },
}

pub enum Event {
    CliProbe {
        tool: CliTool,
        version: Option<String>,
        error: Option<String>,
    },
    ConnectionReport {
        tool: CliTool,
        report: ConnectionReport,
    },
    AuthorizationRequired {
        verification_url: String,
        expires_in: u64,
    },
    AuthorizationFinished,
    ConnectionStatus {
        lark_saved: bool,
        tapd_saved: bool,
    },
    CredentialsSaved(CliTool),
    CredentialsCleared(CliTool),
    State {
        mode: Mode,
        snapshot: Snapshot,
        profile: String,
        message: &'static str,
        new_count: usize,
    },
    Activity {
        key: String,
        active: bool,
    },
    FormError {
        field: &'static str,
        message: String,
    },
    WorkspaceChecked(TapdWorkspace),
    RemoteTarget {
        guid: String,
        profile: String,
        session: u64,
        target: TargetTask,
    },
    AttachmentFailed {
        guid: String,
        session: u64,
        profile: String,
        marker: Option<String>,
        message: String,
    },
    AttachmentReady {
        guid: String,
        session: u64,
        profile: String,
        attachment: DraftAttachment,
        marker: Option<String>,
    },
    Editor(Editor),
    InstallPlan(InstallationPlan),
    Error(String),
}

pub struct Request {
    pub job: Job,
    pub profile: String,
}

pub struct WorkerHandle {
    pub data_root: PathBuf,
    pub jobs: WorkerSender,
    pub events: async_channel::Receiver<Event>,
}

// 显式枚举只有两种传输，避免为了 mock 引入通用插件系统。
enum Connection {
    #[cfg(test)]
    Mock(MockGateway),
    Live(Box<CliGateway>),
}
impl Gateway for Connection {
    fn list_workspaces(&self) -> Result<Vec<TapdWorkspace>> {
        match self {
            #[cfg(test)]
            Self::Mock(g) => g.list_workspaces(),
            Self::Live(g) => g.list_workspaces(),
        }
    }
    fn list_members(&self, workspace: &str) -> Result<Vec<TapdMember>> {
        match self {
            #[cfg(test)]
            Self::Mock(g) => g.list_members(workspace),
            Self::Live(g) => g.list_members(workspace),
        }
    }
    fn get_source(&self, guid: &str) -> Result<SourceTask> {
        match self {
            #[cfg(test)]
            Self::Mock(g) => g.get_source(guid),
            Self::Live(g) => g.get_source(guid),
        }
    }
    fn validate_draft(&self, draft: &Draft) -> Result<()> {
        match self {
            #[cfg(test)]
            Self::Mock(g) => g.validate_draft(draft),
            Self::Live(g) => g.validate_draft(draft),
        }
    }
    fn fetch_tasks(&self) -> Result<Vec<SourceTask>> {
        match self {
            #[cfg(test)]
            Self::Mock(g) => g.fetch_tasks(),
            Self::Live(g) => g.fetch_tasks(),
        }
    }
    fn get_target(&self, w: &str, id: &str, k: &str) -> Result<TargetTask> {
        match self {
            #[cfg(test)]
            Self::Mock(g) => g.get_target(w, id, k),
            Self::Live(g) => g.get_target(w, id, k),
        }
    }
    fn create_target(&self, d: &Draft, s: &SourceTask) -> Result<TargetTask> {
        match self {
            #[cfg(test)]
            Self::Mock(g) => g.create_target(d, s),
            Self::Live(g) => g.create_target(d, s),
        }
    }
    fn update_target(&self, t: &TargetTask, d: &Draft, s: &SourceTask) -> Result<TargetTask> {
        match self {
            #[cfg(test)]
            Self::Mock(g) => g.update_target(t, d, s),
            Self::Live(g) => g.update_target(t, d, s),
        }
    }
}

// UI 持有唯一强发送端；后台任务只保留弱引用，最后一个窗口关闭即可释放锁。
#[derive(Clone)]
pub struct WorkerSender(Arc<mpsc::Sender<Envelope>>);
impl WorkerSender {
    pub fn send(&self, request: Request) -> Result<()> {
        self.0
            .send(Envelope::Request(request))
            .map_err(|_| anyhow::anyhow!("Worker stopped"))
    }
}
enum Envelope {
    Authorization {
        generation: u64,
        verification_url: String,
        expires_in: u64,
    },
    Request(Request),
    Complete(Completion),
}
type AttachmentOrigin = (String, String, Option<String>, u64);
struct PendingRead {
    generation: u64,
    cancelled: Arc<AtomicBool>,
    attachment_origin: Option<AttachmentOrigin>,
}
struct Completion {
    key: String,
    generation: u64,
    result: Result<(String, ReadData)>,
    attachment_origin: Option<AttachmentOrigin>,
}
enum ReadData {
    CliProbe {
        tool: CliTool,
        version: Option<String>,
        error: Option<String>,
    },
    ConnectionReport {
        tool: CliTool,
        report: ConnectionReport,
        authorization: bool,
    },
    Tasks(Vec<SourceTask>),
    Projects(Vec<TapdWorkspace>),
    Members(String, Vec<TapdMember>),
    Workspace(TapdWorkspace),
    RemoteTarget {
        guid: String,
        session: u64,
        target: TargetTask,
    },
    Attachment {
        guid: String,
        session: u64,
        attachment: DraftAttachment,
        marker: Option<String>,
    },
}
// 远端结果只携带数据；复用核心校验和事务，不将 Engine 移入网络线程。
impl Gateway for ReadData {
    fn fetch_tasks(&self) -> Result<Vec<SourceTask>> {
        match self {
            Self::Tasks(v) => Ok(v.clone()),
            _ => bail!("Unexpected read"),
        }
    }
    fn list_workspaces(&self) -> Result<Vec<TapdWorkspace>> {
        match self {
            Self::Projects(v) => Ok(v.clone()),
            _ => bail!("Unexpected read"),
        }
    }
    fn list_members(&self, _: &str) -> Result<Vec<TapdMember>> {
        match self {
            Self::Members(_, v) => Ok(v.clone()),
            _ => bail!("Unexpected read"),
        }
    }
    fn get_target(&self, _: &str, _: &str, _: &str) -> Result<TargetTask> {
        bail!("Read-only result")
    }
    fn create_target(&self, _: &Draft, _: &SourceTask) -> Result<TargetTask> {
        bail!("Read-only result")
    }
    fn update_target(&self, _: &TargetTask, _: &Draft, _: &SourceTask) -> Result<TargetTask> {
        bail!("Read-only result")
    }
}
// 同键同代只允许一个读取；旧完成消息不能移除新代的在途标记。
fn begin_read(
    pending: &mut BTreeMap<String, PendingRead>,
    key: &str,
    generation: u64,
    attachment_origin: Option<AttachmentOrigin>,
) -> bool {
    if pending
        .get(key)
        .is_some_and(|read| read.generation == generation)
    {
        return false;
    }
    if let Some(old) = pending.get(key) {
        old.cancelled.store(true, Ordering::SeqCst);
    }
    pending.insert(
        key.into(),
        PendingRead {
            generation,
            cancelled: Arc::new(AtomicBool::new(false)),
            attachment_origin,
        },
    );
    true
}
fn finish_read(pending: &mut BTreeMap<String, PendingRead>, key: &str, generation: u64) -> bool {
    if pending
        .get(key)
        .is_none_or(|read| read.generation != generation)
    {
        return false;
    }
    pending.remove(key);
    true
}
// 取消先移除在途记录，迟到完成消息不能再次释放 UI 的同一项预留。
fn cancel_stale_reads(pending: &mut BTreeMap<String, PendingRead>, generation: u64) -> Vec<Event> {
    let keys: Vec<_> = pending
        .iter()
        .filter(|(_, read)| read.generation != generation)
        .map(|(key, _)| key.clone())
        .collect();
    let mut events = Vec::new();
    for key in keys {
        pending[&key].cancelled.store(true, Ordering::SeqCst);
        let read = pending
            .remove(&key)
            .expect("key collected from pending reads");
        if key == "lark-connection" {
            events.push(Event::AuthorizationFinished);
        }
        events.push(Event::Activity { key, active: false });
        if let Some((guid, profile, marker, session)) = read.attachment_origin {
            events.push(Event::AttachmentFailed {
                guid,
                profile,
                marker,
                session,
                message: "Attachment operation cancelled because the connection changed".into(),
            });
        }
    }
    events
}
fn attachment_origin(job: &Job, profile: &str) -> Option<AttachmentOrigin> {
    match job {
        Job::LocalFile { guid, session, .. } => {
            Some((guid.clone(), profile.into(), None, *session))
        }
        Job::PastedImage {
            guid,
            marker,
            session,
            ..
        } => Some((guid.clone(), profile.into(), Some(marker.clone()), *session)),
        _ => None,
    }
}
fn read_key(job: &Job) -> Option<String> {
    Some(match job {
        Job::Refresh => "tasks".into(),
        Job::LoadProjects => "projects".into(),
        Job::LoadMembers(id) => format!("members:{id}"),
        Job::CheckWorkspace(w) => format!("workspace:{}:{}", w.id, w.name),
        Job::CheckTarget { guid, session } => format!("target:{guid}:{session}"),
        Job::LocalFile {
            guid,
            path,
            session,
        } => format!("attachment:{guid}:{session}:{}", path.display()),
        Job::PastedImage {
            guid,
            marker,
            session,
            ..
        } => format!("attachment:{guid}:{session}:{marker}"),
        Job::Check(tool) => connection_key(*tool).into(),
        Job::ProbeCli(tool) => cli_key(*tool).into(),
        _ => return None,
    })
}
fn read_error_field(key: &str) -> &'static str {
    if key.starts_with("attachment:") {
        "attachments"
    } else if key.starts_with("workspace:") {
        "new-workspace"
    } else if key.starts_with("target:") {
        "target"
    } else if key.starts_with("members:") {
        "members"
    } else {
        match key {
            "tasks" => "tasks",
            "projects" => "projects",
            "lark-connection" => "lark-connection",
            "tapd-connection" => "tapd-connection",
            "lark-cli" => "lark-cli",
            "tapd-cli" => "tapd-cli",
            _ => "read",
        }
    }
}
fn field(job: &Job) -> &'static str {
    match job {
        Job::CheckTarget { .. } | Job::AcceptTarget { .. } => "target",
        Job::Refresh => "tasks",
        Job::LoadProjects => "projects",
        Job::LoadMembers(_) => "members",
        Job::Check(tool) => connection_key(*tool),
        Job::ProbeCli(tool) => cli_key(*tool),
        Job::SetPreference(Preference::PollMinutes(_) | Preference::Polling(_)) => "poll",
        Job::SetPreference(Preference::DefaultDestination { .. }) => "destination",
        Job::SetPreference(
            Preference::AddWorkspacePrefix { .. } | Preference::RemoveWorkspacePrefix { .. },
        ) => "prefixes",
        Job::CheckWorkspace(_) | Job::AddWorkspace(_) => "new-workspace",
        Job::SaveAndConnect { tool, .. } | Job::ClearCredentials(tool) => connection_key(*tool),
        Job::LoadConnectionStatus => "connection-status",
        Job::LocalFile { .. } | Job::PastedImage { .. } => "attachments",
        _ => "submit",
    }
}
fn patch(settings: &mut Settings, preference: Preference) -> Result<()> {
    match preference {
        Preference::Language(v) => settings.language = v,
        Preference::Theme(v) => settings.theme = v,
        Preference::Polling(v) => settings.poll_enabled = v,
        Preference::PollMinutes(v) => {
            anyhow::ensure!(
                (1..=1440).contains(&v),
                "Polling must be between 1 and 1440 minutes"
            );
            settings.poll_minutes = v;
        }
        Preference::DefaultDestination { workspace, owner } => {
            settings.workspace_id = workspace;
            settings.owner = owner;
        }
        // 单项操作基于 actor 最新状态，不能用 UI 的旧快照覆盖并发排队的修改。
        Preference::AddWorkspacePrefix { workspace, prefix } => {
            settings
                .workspace_prefixes
                .entry(workspace)
                .or_default()
                .push(prefix);
        }
        Preference::RemoveWorkspacePrefix { workspace, prefix } => {
            if let Some(prefixes) = settings.workspace_prefixes.get_mut(&workspace) {
                prefixes.retain(|value| value != &prefix);
            }
        }
    }
    Ok(())
}
fn same_content(a: &Snapshot, b: &Snapshot) -> bool {
    let mut a = a.clone();
    let mut b = b.clone();
    a.last_checked = None;
    b.last_checked = None;
    a.directory.workspaces_fetched_at = None;
    b.directory.workspaces_fetched_at = None;
    for cache in a
        .directory
        .members
        .values_mut()
        .chain(b.directory.members.values_mut())
    {
        cache.fetched_at.clear();
    }
    a == b
}
struct ReadTask {
    key: String,
    generation: u64,
    job: Job,
    mode: Mode,
    settings: Settings,
    #[cfg(test)]
    mock: MockGateway,
    backend: Arc<dyn ConnectionBackend>,
    data_root: PathBuf,
    origin: String,
    target_identity: Option<(String, TargetTask)>,
}
fn spawn_read(sender: Weak<mpsc::Sender<Envelope>>, task: ReadTask) {
    let ReadTask {
        key,
        generation,
        job,
        mode,
        settings,
        #[cfg(test)]
        mock,
        backend,
        data_root,
        origin,
        target_identity,
    } = task;
    let attachment_origin = attachment_origin(&job, &origin);
    std::thread::spawn(move || {
        let result = (|| -> Result<(String, ReadData)> {
            // 本地附件同样不能占用状态所有者；返回时重新验证账户代数。
            if matches!(job, Job::LocalFile { .. } | Job::PastedImage { .. }) {
                let root = media_root(&data_root, &origin)?;
                let (guid, attachment, marker, session) = match job {
                    Job::LocalFile {
                        guid,
                        path,
                        session,
                    } => (
                        guid,
                        lark2tapd_core::media::copy_attachment(&root, &path)?,
                        None,
                        session,
                    ),
                    Job::PastedImage {
                        guid,
                        bytes,
                        extension,
                        marker,
                        session,
                    } => (
                        guid,
                        lark2tapd_core::media::save_pasted_image(&root, &bytes, &extension)?,
                        Some(marker),
                        session,
                    ),
                    _ => unreachable!(),
                };
                return Ok((
                    origin,
                    ReadData::Attachment {
                        guid,
                        session,
                        attachment,
                        marker,
                    },
                ));
            }
            if let Job::ProbeCli(tool) = job {
                #[cfg(test)]
                let result = if mode.is_mock() {
                    Ok("test-only CLI".into())
                } else {
                    backend.probe(tool, &settings)
                };
                #[cfg(not(test))]
                let result = backend.probe(tool, &settings);
                let (version, error) = match result {
                    Ok(version) => (Some(version), None),
                    Err(error) => (None, Some(error.to_string())),
                };
                return Ok((
                    origin,
                    ReadData::CliProbe {
                        tool,
                        version,
                        error,
                    },
                ));
            }
            if let Job::Check(tool) = job {
                anyhow::ensure!(
                    !mode.is_mock(),
                    "Test mode never accesses external accounts"
                );
                let report = (|| {
                    backend.probe(tool, &settings)?;
                    backend.check(tool, &settings, backend.load()?)
                })()
                .unwrap_or_else(failed_report);
                return Ok((
                    origin,
                    ReadData::ConnectionReport {
                        tool,
                        report,
                        authorization: false,
                    },
                ));
            }
            let (profile, gateway) = connection(
                mode,
                settings,
                #[cfg(test)]
                mock,
            )?;
            let data = match job {
                Job::CheckTarget { guid, session } => {
                    let (workspace, target) =
                        target_identity.ok_or_else(|| anyhow::anyhow!("No persisted target"))?;
                    ReadData::RemoteTarget {
                        guid,
                        session,
                        target: gateway.get_target(&workspace, &target.id, &target.kind)?,
                    }
                }
                Job::Refresh => ReadData::Tasks(gateway.fetch_tasks()?),
                Job::LoadProjects => ReadData::Projects(gateway.list_workspaces()?),
                Job::LoadMembers(id) => {
                    let members = gateway.list_members(&id)?;
                    ReadData::Members(id, members)
                }
                Job::CheckWorkspace(w) => {
                    anyhow::ensure!(
                        !w.name.trim().is_empty() && !w.id.trim().is_empty(),
                        "Project ID and name are required"
                    );
                    gateway.list_members(&w.id)?;
                    ReadData::Workspace(w)
                }
                _ => unreachable!(),
            };
            Ok((profile, data))
        })();
        if let Some(sender) = sender.upgrade() {
            let _ = sender.send(Envelope::Complete(Completion {
                key,
                generation,
                result,
                attachment_origin,
            }));
        }
    });
}

// 授权等待时暂停定时器而非重复拒绝过期刷新；请求通道仍即时响应用户操作。
fn poll_wait(
    settings: &Settings,
    opened: bool,
    deadline: Instant,
    now: Instant,
    pending: &BTreeMap<String, PendingRead>,
) -> Option<Duration> {
    (opened && settings.poll_enabled && !pending.keys().any(|key| key.ends_with("-connection")))
        .then(|| deadline.saturating_duration_since(now))
}

fn connection_key(tool: CliTool) -> &'static str {
    match tool {
        CliTool::Lark => "lark-connection",
        CliTool::Tapd => "tapd-connection",
    }
}
// 只替换系统边界，测试仍运行真实 actor、数据库和代数失效逻辑。
trait ConnectionBackend: Send + Sync {
    fn load(&self) -> Result<Credentials>;
    fn save(&self, tool: CliTool, credentials: &Credentials) -> Result<()>;
    fn clear(&self, tool: CliTool) -> Result<()>;
    fn probe(&self, tool: CliTool, settings: &Settings) -> Result<String> {
        let path = match tool {
            CliTool::Lark => &settings.lark_path,
            CliTool::Tapd => &settings.tapd_path,
        };
        probe_supported_cli(tool, std::path::Path::new(path))
    }
    fn check(
        &self,
        tool: CliTool,
        settings: &Settings,
        credentials: Credentials,
    ) -> Result<ConnectionReport>;
    fn connect(
        &self,
        tool: CliTool,
        settings: &Settings,
        credentials: Credentials,
        cancelled: &AtomicBool,
        authorization: &mut dyn FnMut(String, u64) -> Result<()>,
    ) -> Result<ConnectionReport>;
}
struct SystemConnectionBackend;
impl ConnectionBackend for SystemConnectionBackend {
    fn load(&self) -> Result<Credentials> {
        load_credentials()
    }
    fn save(&self, tool: CliTool, credentials: &Credentials) -> Result<()> {
        save_credentials(tool, credentials)
    }
    fn clear(&self, tool: CliTool) -> Result<()> {
        lark2tapd_bridge::clear_credentials(tool)
    }
    fn check(
        &self,
        tool: CliTool,
        settings: &Settings,
        credentials: Credentials,
    ) -> Result<ConnectionReport> {
        Ok(CliGateway::new(settings.clone(), credentials)?.connection_report(tool))
    }
    fn connect(
        &self,
        tool: CliTool,
        settings: &Settings,
        credentials: Credentials,
        cancelled: &AtomicBool,
        authorization: &mut dyn FnMut(String, u64) -> Result<()>,
    ) -> Result<ConnectionReport> {
        ensure_auth_active(cancelled)?;
        let cli = CliGateway::new(settings.clone(), credentials)?;
        if tool == CliTool::Lark {
            // 只有显式保存并连接才允许创建原生 CLI profile；检查和启动不走此分支。
            ensure_auth_active(cancelled)?;
            cli.prepare_lark_profile()?;
            ensure_auth_active(cancelled)?;
            // 设备码只在此后台线程中存活；actor/UI 只接收经过桥接层验证的 URL。
            let pending = cli.begin_lark_login()?;
            ensure_auth_active(cancelled)?;
            authorization(pending.verification_url.clone(), pending.expires_in)?;
            ensure_auth_active(cancelled)?;
            cli.finish_lark_login_cancellable(&pending, cancelled)?;
        }
        ensure_auth_active(cancelled)?;
        Ok(cli.connection_report(tool))
    }
}
fn cli_key(tool: CliTool) -> &'static str {
    match tool {
        CliTool::Lark => "lark-cli",
        CliTool::Tapd => "tapd-cli",
    }
}
fn failed_report(error: anyhow::Error) -> ConnectionReport {
    ConnectionReport {
        connected: false,
        checks: vec![],
        user_scopes: None,
        app_scopes: None,
        failure: Some(error.to_string()),
    }
}
fn connection(
    mode: Mode,
    settings: Settings,
    #[cfg(test)] mock: MockGateway,
) -> Result<(String, Connection)> {
    #[cfg(test)]
    if mode.is_mock() {
        return Ok(("mock".into(), Connection::Mock(mock)));
    }
    let _ = mode;
    let cli = CliGateway::new(settings, load_credentials()?)?;
    Ok((cli.account_namespace()?, Connection::Live(Box::new(cli))))
}
// 授权等待不占用串行 actor；进度与最终报告都必须通过同一代数检查。
fn ensure_auth_active(cancelled: &AtomicBool) -> Result<()> {
    anyhow::ensure!(!cancelled.load(Ordering::SeqCst), "Authorization cancelled");
    Ok(())
}
fn spawn_connect(
    sender: Weak<mpsc::Sender<Envelope>>,
    backend: Arc<dyn ConnectionBackend>,
    tool: CliTool,
    settings: Settings,
    generation: u64,
    cancelled: Arc<AtomicBool>,
    profile_writes: Arc<Mutex<()>>,
) {
    std::thread::spawn(move || {
        let report = (|| {
            ensure_auth_active(&cancelled)?;
            // 包住 profile 准备、设备授权及最终落盘；替换操作须等旧进程退出。
            let _profile_guard = if tool == CliTool::Lark {
                Some(profile_writes.lock().map_err(|_| {
                    anyhow::anyhow!(
                        "Previous native profile operation stopped unexpectedly; restart the app"
                    )
                })?)
            } else {
                None
            };
            ensure_auth_active(&cancelled)?;
            backend.probe(tool, &settings)?;
            let credentials = backend.load()?;
            anyhow::ensure!(
                tool != CliTool::Lark || credentials.lark_user_token.is_empty(),
                "Manual token cannot be verified; use OAuth instead"
            );
            backend.connect(
                tool,
                &settings,
                credentials,
                &cancelled,
                &mut |verification_url, expires_in| {
                    ensure_auth_active(&cancelled)?;
                    let sender = sender
                        .upgrade()
                        .ok_or_else(|| anyhow::anyhow!("Application closed"))?;
                    sender.send(Envelope::Authorization {
                        generation,
                        verification_url,
                        expires_in,
                    })?;
                    ensure_auth_active(&cancelled)
                },
            )
        })()
        .unwrap_or_else(failed_report);
        if let Some(sender) = sender.upgrade() {
            let _ = sender.send(Envelope::Complete(Completion {
                key: connection_key(tool).into(),
                generation,
                attachment_origin: None,
                result: Ok((
                    String::new(),
                    ReadData::ConnectionReport {
                        tool,
                        report,
                        authorization: tool == CliTool::Lark,
                    },
                )),
            }));
        }
    });
}

fn emit_connection_status(
    mode: Mode,
    backend: &dyn ConnectionBackend,
    output: &async_channel::Sender<Event>,
) -> Result<()> {
    let (lark_saved, tapd_saved) =
        if mode.is_mock() {
            (false, false)
        } else {
            lark2tapd_bridge::credential_presence(&backend.load().map_err(|_| {
                anyhow::anyhow!("Cannot read credential status; check Keychain access")
            })?)
        };
    output.try_send(Event::ConnectionStatus {
        lark_saved,
        tapd_saved,
    })?;
    Ok(())
}
pub fn start(root: PathBuf, mode: Mode) -> WorkerHandle {
    start_with_backend(root, mode, Arc::new(SystemConnectionBackend))
}
fn start_with_backend(
    root: PathBuf,
    mode: Mode,
    backend: Arc<dyn ConnectionBackend>,
) -> WorkerHandle {
    let (sender, input) = mpsc::channel();
    let jobs = WorkerSender(Arc::new(sender));
    let weak = Arc::downgrade(&jobs.0);
    let (output, events) = async_channel::unbounded();
    // 提前规范化共享根路径，UI 与 actor 必须解析到同一个附件目录。
    let data_root = std::fs::create_dir_all(&root)
        .and_then(|_| std::fs::canonicalize(&root))
        .unwrap_or_else(|_| root.clone());
    std::thread::spawn(move || {
        let result = (|| -> Result<()> {
            std::fs::create_dir_all(&root)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
            }
            let root = std::fs::canonicalize(root)?;
            let lock = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(root.join("app.lock"))?;
            lock.try_lock().map_err(|_| {
                anyhow::anyhow!("This data directory is already open in another app instance")
            })?;
            tokio::runtime::Runtime::new()?.block_on(async {
                let mut preferences = Engine::open(&root.join(if mode.is_mock() {
                    "preferences-mock.db"
                } else {
                    "preferences-live.db"
                }))
                .await?;
                let mut settings = preferences.snapshot().settings.clone();
                let mut engine =
                    Engine::open(&root.join(profile_file(mode, settings.last_profile.as_deref())?))
                        .await?;
                if mode.is_mock() && settings.workspace_id.is_empty() {
                    settings.workspace_id = "demo".into();
                }
                settings.lark_path = discover_executable(&settings.lark_path, "lark-cli", &root.join("tools"));
                settings.tapd_path = discover_executable(&settings.tapd_path, "tapd-cli", &root.join("tools"));
                settings.tapd_endpoint = Settings::default().tapd_endpoint;
                engine.save_settings(settings.clone()).await?;
                #[cfg(test)]
                let mock = MockGateway::from_snapshot(engine.snapshot());
                let mut namespace = if mode.is_mock() {
                    "mock".into()
                } else {
                    settings.last_profile.clone().unwrap_or_default()
                };
                let profile_writes = Arc::new(Mutex::new(()));
                let mut generation = 0;
                let mut pending = BTreeMap::<String, PendingRead>::new();
                let mut checked = Vec::<TapdWorkspace>::new();
                let mut pending_install = None;
                let mut next_check = Instant::now() + interval(&settings);
                let mut opened = false;
                output
                    .send(Event::State {
                        mode,
                        snapshot: engine.snapshot().clone(),
                        profile: namespace.clone(),
                        message: "ready",
                        new_count: 0,
                    })
                    .await?;
                if let Err(error) = emit_connection_status(mode, backend.as_ref(), &output) {
                    output.send(Event::FormError { field: "connection-status", message: error.to_string() }).await?;
                }
                loop {
                    let scheduled = poll_wait(&settings, opened, next_check, Instant::now(), &pending);
                    let envelope = match input.recv_timeout(scheduled.unwrap_or(Duration::from_secs(3600))) {
                        Ok(v) => v,
                        Err(mpsc::RecvTimeoutError::Timeout) if scheduled.is_some() => {
                            Envelope::Request(Request {
                                job: Job::Refresh,
                                profile: namespace.clone(),
                            })
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    let mut activity = match &envelope {
                        Envelope::Request(Request { job: Job::SaveAndConnect { tool, .. } | Job::ClearCredentials(tool), .. }) => Some(connection_key(*tool)),
                        Envelope::Request(Request { job: Job::LoadConnectionStatus, .. }) => Some("connection-status"),
                        _ => None,
                    };
                    if let Some(key) = activity { output.send(Event::Activity { key: key.into(), active: true }).await?; }
                    let is_request=matches!(&envelope,Envelope::Request(_));
                    let before = engine.snapshot().clone();
                    let previous_profile = namespace.clone();
                    let mut message = "";
                    let mut new_count = 0;
                    let mut error_field = "submit";
                    let result: Result<()> = async {
                        let Request { job, profile } = match envelope {
                            Envelope::Authorization { generation: current, verification_url, expires_in } => {
                                if current == generation && pending.get("lark-connection").is_some_and(|read| read.generation == current) {
                                    output.send(Event::AuthorizationRequired { verification_url, expires_in }).await?;
                                }
                                return Ok(());
                            }
                            Envelope::Complete(done) => {
                                if !finish_read(&mut pending,&done.key,done.generation) {
                                    return Ok(());
                                }
                                output
                                    .send(Event::Activity {
                                        key: done.key.clone(),
                                        active: false,
                                    })
                                    .await?;
                                if done.generation != generation {
                                    return Ok(());
                                }
                                error_field = read_error_field(&done.key);
                                if let (Err(error),Some((guid,profile,marker,session)))=(&done.result,&done.attachment_origin) && profile==&namespace {output.send(Event::AttachmentFailed{guid:guid.clone(),profile:profile.clone(),marker:marker.clone(),session:*session,message:error.to_string()}).await?;}
                                let (current, data) = done.result?;
                                if matches!(data, ReadData::Members(..) | ReadData::Workspace(_) | ReadData::Attachment {..} | ReadData::RemoteTarget{..}) {
                                    anyhow::ensure!(current == namespace, "Account changed; refresh tasks before checking this project");
                                }
                                if !matches!(data, ReadData::CliProbe { .. } | ReadData::ConnectionReport { .. })
                                    && current != namespace
                                {
                                    engine = Engine::open(
                                        &root.join(profile_file(mode, Some(&current))?),
                                    )
                                    .await?;
                                    namespace = current;
                                    settings.last_profile = Some(namespace.clone());
                                    engine.save_settings(settings.clone()).await?;
                                    preferences.save_settings(settings.clone()).await?;
                                    generation += 1;
                                    checked.clear();
                                    for event in cancel_stale_reads(&mut pending,generation) { output.send(event).await?; }
                                }
                                match &data {
                                    ReadData::CliProbe { tool, version, error } => {
                                        output.send(Event::CliProbe { tool: *tool, version: version.clone(), error: error.clone() }).await?;
                                    }
                                    ReadData::ConnectionReport { tool, report, authorization } => {
                                        if *authorization { output.send(Event::AuthorizationFinished).await?; }
                                        output.send(Event::ConnectionReport { tool: *tool, report: report.clone() }).await?;
                                        if let Some(failure) = &report.failure {
                                            output.send(Event::FormError { field: connection_key(*tool), message: failure.clone() }).await?;
                                        }
                                    }
                                    ReadData::Tasks(_) => {
                                        new_count = engine.refresh(&data).await?;
                                        message = "refreshed";
                                    }
                                    ReadData::Projects(_) => {
                                        engine.refresh_workspaces(&data).await?;
                                        message = "projects_loaded";
                                    }
                                    ReadData::Members(id, _) => {
                                        engine.refresh_members(&data, id).await?;
                                        message = "members_loaded";
                                    }
                                    ReadData::Workspace(w) => {
                                        checked.retain(|old| old.id != w.id);
                                        checked.push(w.clone());
                                        output.send(Event::WorkspaceChecked(w.clone())).await?;
                                    }
                                    ReadData::RemoteTarget{guid,session,target}=>{output.send(Event::RemoteTarget{guid:guid.clone(),profile:namespace.clone(),session:*session,target:target.clone()}).await?;}
                                    ReadData::Attachment{guid,attachment,marker,session}=>{output.send(Event::AttachmentReady{guid:guid.clone(),profile:namespace.clone(),attachment:attachment.clone(),marker:marker.clone(),session:*session}).await?;}

                                }
                                return Ok(());
                            }
                            Envelope::Request(request) => request,
                        };
                        error_field = field(&job);
                        if let Some(key) = read_key(&job) {
                            // Account reads may otherwise adopt the old native profile and cancel OAuth itself.
                            anyhow::ensure!(
                                matches!(job, Job::ProbeCli(_))
                                    || !pending.keys().any(|key| key.ends_with("-connection")),
                                "Connection in progress; finish authorization before refreshing account data"
                            );
                            if matches!(job, Job::LoadMembers(_) | Job::CheckWorkspace(_) | Job::LocalFile{..} | Job::PastedImage{..} | Job::CheckTarget{..}) {
                                anyhow::ensure!(profile == namespace, "Account changed; refresh tasks first");
                            }
                            if let Job::LocalFile{guid,..} | Job::PastedImage{guid,..} = &job {
                                anyhow::ensure!(engine.snapshot().records.iter().any(|r| &r.source.guid==guid), "Task no longer available");
                            }
                            if matches!(job, Job::Refresh) {
                                opened = true;
                                next_check = Instant::now() + interval(&settings);
                            }
                            // 先公布现有缓存；重复读取共享同一在途任务。
                            output
                                .send(Event::State {
                                    mode,
                                    snapshot: engine.snapshot().clone(),
                                    profile: namespace.clone(),
                                    message: "cached",
                                    new_count: 0,
                                })
                                .await?;
                            let target_identity=if let Job::CheckTarget{guid,..}=&job {
                                let r=engine.snapshot().records.iter().find(|r| &r.source.guid==guid).ok_or_else(||anyhow::anyhow!("Task no longer available"))?;
                                Some((r.target_workspace.clone().ok_or_else(||anyhow::anyhow!("Missing target workspace"))?,r.target.clone().ok_or_else(||anyhow::anyhow!("No persisted target"))?))
                            }else{None};
                            if !begin_read(&mut pending,&key,generation,attachment_origin(&job,&namespace)) {
                                return Ok(());
                            }
                            output
                                .send(Event::Activity {
                                    key: key.clone(),
                                    active: true,
                                })
                                .await?;
                            spawn_read(weak.clone(),ReadTask{key,generation,job,mode,settings:settings.clone(),#[cfg(test)] mock:mock.clone(),backend:backend.clone(),data_root:root.clone(),origin:namespace.clone(),target_identity});
                            return Ok(());
                        }
                        if mode.is_mock()
                            && matches!(
                                job,
                                Job::SaveAndConnect { .. }
                                    | Job::ClearCredentials(_)
                                    | Job::PlanInstall(_)
                                    | Job::Install(_)
                            )
                        {
                            bail!("Demo mode never accesses credentials or external accounts");
                        }
                        let reschedule = resets_polling(&job, &settings);
                        match job {
                            Job::SetPreference(p) => {
                                if let Preference::DefaultDestination { workspace, owner } = &p {
                                    anyhow::ensure!(profile == namespace, "Account changed; refresh project options");
                                    let directory = &engine.snapshot().directory;
                                    anyhow::ensure!(directory.workspaces.iter().any(|w| &w.id == workspace), "Select a verified project");
                                    anyhow::ensure!(owner.is_empty() || directory.members.get(workspace).is_some_and(|c| c.items.iter().any(|m| &m.account == owner)), "Select a current project member");
                                }
                                if matches!(p, Preference::AddWorkspacePrefix { .. } | Preference::RemoveWorkspacePrefix { .. }) {
                                    anyhow::ensure!(profile == namespace, "Account changed; refresh project options");
                                }
                                let mut next = settings.clone();
                                patch(&mut next, p)?;
                                preferences.save_settings(next.clone()).await?;
                                engine.save_settings(next.clone()).await?;
                                settings = next;
                                // 偏好不改变连接身份；不能取消窗口打开时已经发出的读取。
                                message = "settings_saved";
                            }
                            Job::PlanInstall(tool) => {
                                message = "install_plan";
                                pending_install = Some(tool);
                                output
                                    .send(Event::InstallPlan(installation_plan(
                                        tool,
                                        &root.join("tools"),
                                    )?))
                                    .await?;
                            }
                            Job::CancelInstall => {
                                pending_install = None;
                                message="install_cancelled";
                            }
                            Job::Install(tool) => {
                                anyhow::ensure!(
                                    pending_install.take() == Some(tool),
                                    "Review installation before confirming"
                                );
                                for read in pending.values() {
                                    read.cancelled.store(true, Ordering::SeqCst);
                                }
                                let path = install_cli(tool, &root.join("tools"))?
                                    .to_string_lossy()
                                    .into_owned();
                                match tool {
                                    CliTool::Lark => settings.lark_path = path,
                                    CliTool::Tapd => settings.tapd_path = path,
                                }
                                preferences.save_settings(settings.clone()).await?;
                                engine.save_settings(settings.clone()).await?;
                                generation += 1;
                                checked.clear();
                                message="installed";
                            }
                            Job::LoadConnectionStatus => {
                                emit_connection_status(mode, backend.as_ref(), &output)?;
                            }
                            Job::SaveAndConnect { tool, .. } | Job::ClearCredentials(tool) => {
                                let saving = matches!(&job, Job::SaveAndConnect { .. });
                                // 请求旧授权停止；后台写入锁确保新原生操作等待旧进程退出。
                                for read in pending.values() {
                                    read.cancelled.store(true, Ordering::SeqCst);
                                }
                                match job {
                                    Job::SaveAndConnect { credentials, .. } => {
                                        backend.probe(tool, &settings)?;
                                        backend.save(tool, &credentials).map_err(|_| anyhow::anyhow!("Cannot save credentials; enter complete credentials and check Keychain access"))?;
                                        output.send(Event::CredentialsSaved(tool)).await?;
                                        message = "credentials_saved";
                                    }
                                    Job::ClearCredentials(_) => {
                                        backend.clear(tool).map_err(|_| anyhow::anyhow!("Cannot clear credentials; check Keychain access"))?;
                                        output.send(Event::CredentialsCleared(tool)).await?;
                                        message = "credentials_cleared";
                                    }
                                    _ => unreachable!(),
                                }
                                // 保存成功即作废旧账户与在途读取，不能等待后续 OAuth 成功。
                                output.send(Event::AuthorizationFinished).await?;
                                namespace.clear();
                                settings.last_profile = None;
                                generation += 1;
                                checked.clear();
                                for event in cancel_stale_reads(&mut pending, generation) { output.send(event).await?; }
                                engine = Engine::open(&root.join("disconnected.db")).await?;
                                engine.save_settings(settings.clone()).await?;
                                preferences.save_settings(settings.clone()).await?;
                                emit_connection_status(mode, backend.as_ref(), &output)?;
                                output.send(Event::State { mode, snapshot: engine.snapshot().clone(), profile: namespace.clone(), message, new_count: 0 }).await?;
                                if saving {
                                    begin_read(&mut pending, connection_key(tool), generation, None);
                                    // 旧授权取消可能刚关闭同名活动，重新标记本代等待状态。
                                    output.send(Event::Activity { key: connection_key(tool).into(), active: true }).await?;
                                    // 本次保存的活动状态由后台完成或代数失效负责释放。
                                    activity = None;
                                    spawn_connect(weak.clone(), backend.clone(), tool, settings.clone(), generation, pending[connection_key(tool)].cancelled.clone(), profile_writes.clone());
                                }

                            }
                            Job::AddWorkspace(w) => {
                                anyhow::ensure!(
                                    profile == namespace && checked.contains(&w),
                                    "Check this project and account before confirming"
                                );
                                engine.add_workspace(w).await?;
                                message = "project_added";
                            }
                            Job::OpenDraft(guid, discard, session) => {
                                anyhow::ensure!(
                                    profile == namespace,
                                    "Account changed; reopen this task"
                                );
                                if discard {
                                    engine.discard_draft(&guid).await?;
                                }
                                message = "editor_ready";
                                let draft = engine.prepare(&guid)?;
                                let source = engine
                                    .snapshot()
                                    .records
                                    .iter()
                                    .find(|r| r.source.guid == guid)
                                    .ok_or_else(|| anyhow::anyhow!("Task no longer available"))?
                                    .source
                                    .clone();
                                output
                                    .send(Event::Editor(Editor {
                                        session,
                                        source,
                                        draft,
                                        profile: namespace.clone(),
                                    }))
                                    .await?;
                            }
                            Job::SaveDraft(guid, draft, source) => {
                                anyhow::ensure!(draft.attachments.len() <= 10, "A task supports at most 10 attachments");
                                anyhow::ensure!(
                                    profile == namespace,
                                    "Account changed; reopen this task"
                                );
                                engine.save_draft_for_source(&guid, &draft, &source).await?;
                                message = "draft_saved";
                            }
                            Job::Submit(..) | Job::Recover { .. } | Job::AcceptTarget{..} => {
                                anyhow::ensure!(
                                    profile == namespace,
                                    "Account changed; reopen this task"
                                );
                                let (current, gateway) = connection(mode, settings.clone(), #[cfg(test)] mock.clone())?;
                                anyhow::ensure!(current == namespace, "Account changed; refresh tasks first");
                                match job {
                                    Job::AcceptTarget{guid,target}=>{
                                        let r=engine.snapshot().records.iter().find(|r|r.source.guid==guid).ok_or_else(||anyhow::anyhow!("Task no longer available"))?;
                                        let old=r.target.as_ref().ok_or_else(||anyhow::anyhow!("No persisted target"))?;
                                        anyhow::ensure!(old.id==target.id&&old.kind==target.kind,"Target identity changed");
                                        let workspace=r.target_workspace.as_deref().ok_or_else(||anyhow::anyhow!("Missing target workspace"))?;
                                        anyhow::ensure!(gateway.get_target(workspace,&target.id,&target.kind)?==target,"Remote target changed again; inspect it before accepting");
                                        engine.accept_target(&guid,&target).await?;message="target_accepted";
                                    }
                                    Job::Submit(guid, draft, source) => {
                                        output
                                            .send(Event::Activity {
                                                key: "submit".into(),
                                                active: true,
                                            })
                                            .await?;
                                        let result = submit(
                                            &mut engine,
                                            &gateway,
                                            &media_root(&root, &namespace)?,
                                            &guid,
                                            draft,
                                            source,
                                        )
                                        .await;
                                        output
                                            .send(Event::Activity {
                                                key: "submit".into(),
                                                active: false,
                                            })
                                            .await?;
                                        result?;
                                        message = "imported";
                                    }
                                    Job::Recover {
                                        guid,
                                        workspace,
                                        id,
                                        kind,
                                    } => {
                                        engine
                                            .bind_existing(&gateway, &guid, &workspace, &id, &kind)
                                            .await?;
                                        message = "recovered";
                                    }
                                    _ => unreachable!(),
                                }
                            }
                            _ => unreachable!(),
                        }
                        if reschedule {
                            next_check = Instant::now() + interval(&settings);
                        }
                        Ok(())
                    }
                    .await;
                    if let Some(key) = activity { output.send(Event::Activity { key: key.into(), active: false }).await?; }
                    // 连接切换后的落盘即使失败，也必须释放旧代附件预留，不能只熄灭指示器。
                    for event in cancel_stale_reads(&mut pending,generation) { output.send(event).await?; }
                    if previous_profile != namespace || !same_content(&before, engine.snapshot()) || message=="refreshed" || (is_request && result.is_ok() && !message.is_empty()) {
                        output
                            .send(Event::State {
                                mode,
                                snapshot: engine.snapshot().clone(),
                                profile: namespace.clone(),
                                message,
                                new_count,
                            })
                            .await?;
                    }
                    if let Err(error) = result {
                        output
                            .send(Event::FormError {
                                field: error_field,
                                message: error.to_string(),
                            })
                            .await?;
                        output.send(Event::Error(error.to_string())).await?;
                    }
                }
                Ok::<_, anyhow::Error>(())
            })
        })();
        if let Err(error) = result {
            let _ = output.send_blocking(Event::Error(error.to_string()));
        }
    });
    WorkerHandle {
        jobs,
        events,
        data_root,
    }
}

// 映射先持久化，附件逐项先记不确定再上传；崩溃后绝不盲目重传。
async fn submit(
    engine: &mut Engine,
    gateway: &Connection,
    media: &std::path::Path,
    guid: &str,
    mut draft: Draft,
    source: SourceTask,
) -> Result<()> {
    anyhow::ensure!(
        draft.attachments.len() <= 10,
        "A task supports at most 10 attachments"
    );
    // 表单可能早于上传回执；持久化回执和不确定标记不能被旧表单覆盖。
    if let Some(saved) = engine
        .snapshot()
        .records
        .iter()
        .find(|r| r.source.guid == guid)
        .and_then(|r| r.last_draft.as_ref())
    {
        for attachment in &mut draft.attachments {
            if let Some(known) = saved.attachments.iter().find(|a| a.id == attachment.id) {
                anyhow::ensure!(
                    known.path == attachment.path
                        && known.name == attachment.name
                        && known.size == attachment.size
                        && known.is_image == attachment.is_image,
                    "Attachment identity changed since the last submission"
                );
                attachment.remote_id = known.remote_id.clone();
                attachment.remote_url = known.remote_url.clone();
                attachment.uncertain = known.uncertain;
            }
        }
    }
    for attachment in &draft.attachments {
        lark2tapd_core::media::verify_attachment(media, attachment)?;
        lark2tapd_bridge::validate_attachment_for_upload(attachment)?;
        anyhow::ensure!(
            !attachment.uncertain,
            "Attachment upload outcome is uncertain; reconcile it before retrying"
        );
    }
    gateway.validate_draft(&draft)?;
    engine.validate_destination(&draft)?;
    engine.save_draft_for_source(guid, &draft, &source).await?;
    let expected = engine
        .snapshot()
        .records
        .iter()
        .find(|r| r.source.guid == guid)
        .and_then(|r| r.target.clone());
    engine.commit(gateway, guid, &draft, expected).await?;
    // 第一次正文写入可能仍是图片占位符；回执落盘不能冒充正文已更新。
    let pending_image_body = draft
        .attachments
        .iter()
        .any(|a| a.is_image && a.remote_url.is_none());
    engine
        .set_media_body_pending(guid, pending_image_body)
        .await?;
    let target = engine
        .snapshot()
        .records
        .iter()
        .find(|r| r.source.guid == guid)
        .and_then(|r| r.target.clone())
        .ok_or_else(|| anyhow::anyhow!("Missing persisted target"))?;
    let mut images = false;
    for attachment in &mut draft.attachments {
        if attachment.remote_id.is_some() || attachment.remote_url.is_some() {
            continue;
        }
        // 网关接收本次确定尚未上传的意图；日志中的不确定标记只用于崩溃/重试保护。
        let upload_intent = attachment.clone();
        attachment.uncertain = true;
        engine.attachment_progress(guid, attachment).await?;
        let mut uploaded = match gateway {
            #[cfg(test)]
            Connection::Mock(_) => {
                let mut a = attachment.clone();
                a.remote_id = Some(format!("mock-{}", a.id));
                a.remote_url = Some(format!("https://example.invalid/attachments/{}", a.id));
                a
            }
            Connection::Live(cli) => {
                cli.upload_draft_attachment(media, &draft.workspace_id, &target, &upload_intent)?
            }
        };
        uploaded.path = attachment.path.clone();
        uploaded.uncertain = false;
        images |= uploaded.is_image;
        engine.attachment_progress(guid, &uploaded).await?;
        *attachment = uploaded;
    }
    if images {
        engine.save_draft_for_source(guid, &draft, &source).await?;
        engine.commit(gateway, guid, &draft, Some(target)).await?;
        engine.set_media_body_pending(guid, false).await?;
    }
    Ok(())
}

pub fn media_root(data_root: &std::path::Path, profile: &str) -> Result<PathBuf> {
    anyhow::ensure!(
        profile == "mock"
            || (profile.len() == 64 && profile.bytes().all(|c| c.is_ascii_hexdigit())),
        "Invalid media profile"
    );
    Ok(data_root.join("profiles").join(profile))
}

// 持久化配置仍是不可信输入；缓存定位只接受固定长度散列，不能逃逸应用数据目录。
fn profile_file(mode: Mode, profile: Option<&str>) -> Result<String> {
    if mode.is_mock() {
        return Ok("mock.db".into());
    }
    match profile {
        None => Ok("disconnected.db".into()),
        Some(p) if p.len() == 64 && p.bytes().all(|c| c.is_ascii_hexdigit()) => {
            Ok(format!("live-{p}.db"))
        }
        Some(_) => bail!("Invalid cached account identity"),
    }
}

fn resets_polling(job: &Job, _settings: &Settings) -> bool {
    matches!(
        job,
        Job::Refresh | Job::SetPreference(Preference::Polling(_) | Preference::PollMinutes(_))
    )
}

fn interval(settings: &Settings) -> Duration {
    Duration::from_secs(u64::from(settings.poll_minutes.clamp(1, 1440)) * 60)
}

fn discover_executable(configured: &str, fallback: &str, tools: &std::path::Path) -> String {
    fn executable(path: &std::path::Path) -> bool {
        let Ok(metadata) = path.metadata() else {
            return false;
        };
        if !metadata.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            true
        }
    }
    if std::path::Path::new(configured).is_absolute()
        && executable(std::path::Path::new(configured))
    {
        return configured.into();
    }
    std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|p| p.join(fallback))
                .find(|p| executable(p))
        })
        .or_else(|| {
            let tool = if fallback == "lark-cli" {
                CliTool::Lark
            } else {
                CliTool::Tapd
            };
            installation_plan(tool, tools)
                .ok()
                .map(|plan| plan.destination)
                .filter(|path| executable(path))
        })
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| fallback.into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn credential_form_errors_are_service_scoped() {
        assert_eq!(
            super::field(&super::Job::SaveAndConnect {
                tool: super::CliTool::Lark,
                credentials: super::Credentials::default()
            }),
            "lark-connection"
        );
    }

    use super::*;
    #[test]
    fn editor_and_appearance_jobs_do_not_postpone_polling() {
        let settings = Settings::default();
        let mut theme = settings.clone();
        theme.theme = "dark".into();
        for job in [
            Job::OpenDraft("guid".into(), false, 0),
            Job::SaveAndConnect {
                tool: CliTool::Lark,
                credentials: Credentials::default(),
            },
            Job::Check(CliTool::Lark),
            Job::SetPreference(Preference::Theme(theme.theme)),
        ] {
            assert!(!resets_polling(&job, &settings));
        }
        let mut interval = settings.clone();
        interval.poll_minutes = 60;
        assert!(resets_polling(
            &Job::SetPreference(Preference::PollMinutes(interval.poll_minutes)),
            &settings
        ));
        assert!(resets_polling(&Job::Refresh, &settings));
    }
    #[test]
    fn startup_does_not_fetch_directory_before_window_refresh() {
        let root = tempfile::tempdir().unwrap();
        let worker = start(root.path().to_owned(), Mode::Mock);
        match worker.events.recv_blocking().unwrap() {
            Event::State { snapshot, .. } => assert!(snapshot.directory.workspaces.is_empty()),
            _ => panic!("expected cached startup state"),
        }
    }
    // 等待带截止时间，actor 泄漏或漏发事件必须使测试失败而非挂住。
    fn event(worker: &WorkerHandle) -> Event {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match worker.events.try_recv() {
                Ok(Event::ConnectionStatus { .. }) => continue,
                Ok(event) => return event,
                Err(async_channel::TryRecvError::Closed) => panic!("worker stopped"),
                Err(_) => {}
            }
            assert!(Instant::now() < deadline, "worker event timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn request(worker: &WorkerHandle, job: Job) {
        worker
            .jobs
            .send(Request {
                job,
                profile: "mock".into(),
            })
            .unwrap();
    }
    #[test]
    fn cached_projects_are_emitted_before_background_update() {
        let root = tempfile::tempdir().unwrap();
        let worker = start(root.path().to_owned(), Mode::Mock);
        event(&worker);
        request(&worker, Job::LoadProjects);
        assert!(
            matches!(event(&worker),Event::State{message:"cached",snapshot,..} if snapshot.directory.workspaces.is_empty())
        );
        assert!(matches!(
            event(&worker),
            Event::Activity { active: true, .. }
        ));
        loop {
            if let Event::State {
                message: "projects_loaded",
                snapshot,
                ..
            } = event(&worker)
            {
                assert!(!snapshot.directory.workspaces.is_empty());
                break;
            }
        }
    }
    #[test]
    fn serial_preference_patches_preserve_each_other_and_reject_bad_interval() {
        let root = tempfile::tempdir().unwrap();
        let worker = start(root.path().to_owned(), Mode::Mock);
        event(&worker);
        request(
            &worker,
            Job::SetPreference(Preference::Theme("dark".into())),
        );
        request(
            &worker,
            Job::SetPreference(Preference::Language("en".into())),
        );
        let mut saved = 0;
        while saved < 2 {
            if let Event::State { snapshot, .. } = event(&worker) {
                saved += 1;
                if saved == 2 {
                    assert_eq!(snapshot.settings.theme, "dark");
                    assert_eq!(snapshot.settings.language, "en");
                }
            }
        }
        request(&worker, Job::SetPreference(Preference::PollMinutes(0)));
        assert!(matches!(
            event(&worker),
            Event::FormError { field: "poll", .. }
        ));
    }
    #[test]
    fn last_sender_drop_releases_actor_and_process_lock() {
        let root = tempfile::tempdir().unwrap();
        let worker = start(root.path().to_owned(), Mode::Mock);
        event(&worker);
        let receiver = worker.events.clone();
        drop(worker);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !receiver.is_closed() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        let next = start(root.path().to_owned(), Mode::Mock);
        assert!(matches!(event(&next), Event::State { .. }));
    }
    #[test]
    fn freshness_does_not_reset_global_content() {
        let a = Snapshot::default();
        let mut b = a.clone();
        b.last_checked = Some("later".into());
        b.directory.workspaces_fetched_at = Some("later".into());
        assert!(same_content(&a, &b));
        b.settings.theme = "dark".into();
        assert!(!same_content(&a, &b));
    }
    #[test]
    fn failed_directory_apply_preserves_cache() {
        let root = tempfile::tempdir().unwrap();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let mut engine = Engine::open(&root.path().join("test.db")).await.unwrap();
            engine
                .refresh_workspaces(&ReadData::Projects(vec![TapdWorkspace {
                    id: "good".into(),
                    name: "Good".into(),
                }]))
                .await
                .unwrap();
            let before = engine.snapshot().clone();
            assert!(
                engine
                    .refresh_workspaces(&ReadData::Projects(vec![TapdWorkspace {
                        id: "".into(),
                        name: "Bad".into()
                    }]))
                    .await
                    .is_err()
            );
            assert_eq!(&before, engine.snapshot());
        });
    }
    #[test]
    fn unchecked_workspace_is_not_added() {
        let root = tempfile::tempdir().unwrap();
        let worker = start(root.path().to_owned(), Mode::Mock);
        event(&worker);
        request(
            &worker,
            Job::AddWorkspace(TapdWorkspace {
                id: "demo".into(),
                name: "Unverified".into(),
            }),
        );
        assert!(matches!(
            event(&worker),
            Event::FormError {
                field: "new-workspace",
                ..
            }
        ));
    }
    #[test]
    fn submit_reuses_persisted_mapping_and_receipt_from_stale_form_after_restart() {
        let root = tempfile::tempdir().unwrap();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let database = root.path().join("test.db");
            let media = media_root(root.path(), "mock").unwrap();
            let mut engine = Engine::open(&database).await.unwrap();
            let mock = MockGateway::from_snapshot(engine.snapshot());
            engine.refresh(&mock).await.unwrap();
            engine.refresh_workspaces(&mock).await.unwrap();
            engine.refresh_members(&mock, "demo").await.unwrap();
            let mut settings = engine.snapshot().settings.clone();
            settings.workspace_id = "demo".into();
            engine.save_settings(settings).await.unwrap();
            let source = engine.snapshot().records[0].source.clone();
            let guid = source.guid.clone();
            let mut draft = engine.prepare(&guid).unwrap();
            let path = root.path().join("note.txt");
            std::fs::write(&path, "selected file").unwrap();
            draft
                .attachments
                .push(lark2tapd_core::media::copy_attachment(&media, &path).unwrap());
            submit(
                &mut engine,
                &Connection::Mock(mock),
                &media,
                &guid,
                draft.clone(),
                source.clone(),
            )
            .await
            .unwrap();
            let first = engine
                .snapshot()
                .records
                .iter()
                .find(|r| r.source.guid == guid)
                .unwrap()
                .clone();
            assert!(
                first.last_draft.as_ref().unwrap().attachments[0]
                    .remote_id
                    .is_some()
            );
            drop(engine);
            let mut engine = Engine::open(&database).await.unwrap();
            let mock = MockGateway::from_snapshot(engine.snapshot());
            draft.title.push_str(" edited");
            submit(
                &mut engine,
                &Connection::Mock(mock),
                &media,
                &guid,
                draft,
                source,
            )
            .await
            .unwrap();
            let next = engine
                .snapshot()
                .records
                .iter()
                .find(|r| r.source.guid == guid)
                .unwrap();
            assert_eq!(next.target.as_ref().unwrap().id, first.target.unwrap().id);
            assert_eq!(
                next.last_draft.as_ref().unwrap().attachments,
                first.last_draft.unwrap().attachments
            );
        });
    }
    #[test]
    fn editor_and_remote_review_echo_sessions_and_accept_explicitly() {
        let root = tempfile::tempdir().unwrap();
        let worker = start(root.path().to_owned(), Mode::Mock);
        event(&worker);
        request(&worker, Job::Refresh);
        let guid = loop {
            if let Event::State {
                message: "refreshed",
                snapshot,
                ..
            } = event(&worker)
            {
                break snapshot.records[0].source.guid.clone();
            }
        };
        request(&worker, Job::LoadProjects);
        loop {
            if matches!(
                event(&worker),
                Event::State {
                    message: "projects_loaded",
                    ..
                }
            ) {
                break;
            }
        }
        request(&worker, Job::OpenDraft(guid.clone(), false, 42));
        let editor = loop {
            if let Event::Editor(editor) = event(&worker) {
                assert_eq!(editor.session, 42);
                break editor;
            }
        };
        request(
            &worker,
            Job::Submit(guid.clone(), editor.draft, editor.source),
        );
        loop {
            if matches!(
                event(&worker),
                Event::State {
                    message: "imported",
                    ..
                }
            ) {
                break;
            }
        }
        request(
            &worker,
            Job::CheckTarget {
                guid: guid.clone(),
                session: 77,
            },
        );
        let target = loop {
            if let Event::RemoteTarget {
                guid: result_guid,
                profile,
                session,
                target,
            } = event(&worker)
            {
                assert_eq!(result_guid, guid);
                assert_eq!(profile, "mock");
                assert_eq!(session, 77);
                break target;
            }
        };
        request(&worker, Job::AcceptTarget { guid, target });
        loop {
            if matches!(
                event(&worker),
                Event::State {
                    message: "target_accepted",
                    ..
                }
            ) {
                break;
            }
        }
    }
    #[test]
    fn failed_paste_reports_only_its_original_marker() {
        let root = tempfile::tempdir().unwrap();
        let worker = start(root.path().to_owned(), Mode::Mock);
        event(&worker);
        request(&worker, Job::Refresh);
        let guid = loop {
            if let Event::State {
                message: "refreshed",
                snapshot,
                ..
            } = event(&worker)
            {
                break snapshot.records[0].source.guid.clone();
            }
        };
        request(
            &worker,
            Job::PastedImage {
                guid: guid.clone(),
                session: 7,
                bytes: b"not an image".to_vec(),
                extension: "png".into(),
                marker: "paste:exact:marker".into(),
            },
        );
        loop {
            if let Event::AttachmentFailed {
                guid: failed,
                profile,
                marker,
                message,
                session,
            } = event(&worker)
            {
                assert_eq!(session, 7);
                assert_eq!(failed, guid);
                assert_eq!(profile, "mock");
                assert_eq!(marker.as_deref(), Some("paste:exact:marker"));
                assert!(!message.is_empty());
                break;
            }
        }
    }
    #[test]
    fn cancelling_install_emits_completion_without_snapshot_change() {
        let root = tempfile::tempdir().unwrap();
        let worker = start(root.path().to_owned(), Mode::Mock);
        event(&worker);
        request(&worker, Job::CancelInstall);
        assert!(matches!(
            event(&worker),
            Event::State {
                message: "install_cancelled",
                ..
            }
        ));
    }
    #[test]
    fn metadata_errors_do_not_use_submit_field() {
        for (key, expected) in [
            ("tasks", "tasks"),
            ("projects", "projects"),
            ("members:demo", "members"),
            ("lark-connection", "lark-connection"),
            ("tapd-connection", "tapd-connection"),
            ("workspace:demo:name", "new-workspace"),
            ("attachment:guid:file", "attachments"),
        ] {
            assert_eq!(read_error_field(key), expected);
        }
    }
    #[test]
    fn cosmetic_preferences_preserve_account_verification() {
        let root = tempfile::tempdir().unwrap();
        let worker = start(root.path().to_owned(), Mode::Mock);
        event(&worker);
        let workspace = TapdWorkspace {
            id: "demo".into(),
            name: "Checked project".into(),
        };
        request(&worker, Job::CheckWorkspace(workspace.clone()));
        loop {
            if matches!(event(&worker), Event::WorkspaceChecked(_)) {
                break;
            }
        }
        request(
            &worker,
            Job::SetPreference(Preference::Theme("dark".into())),
        );
        loop {
            if matches!(
                event(&worker),
                Event::State {
                    message: "settings_saved",
                    ..
                }
            ) {
                break;
            }
        }
        request(&worker, Job::AddWorkspace(workspace));
        match event(&worker) {
            Event::State {
                message: "project_added",
                ..
            } => {}
            _ => panic!("cosmetic preference invalidated verified account state"),
        }
    }
    #[test]
    fn uncertain_attachment_from_journal_refuses_blind_retry() {
        let root = tempfile::tempdir().unwrap();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let mut engine = Engine::open(&root.path().join("test.db")).await.unwrap();
            let mock = MockGateway::from_snapshot(engine.snapshot());
            engine.refresh(&mock).await.unwrap();
            engine.refresh_workspaces(&mock).await.unwrap();
            let source = engine.snapshot().records[0].source.clone();
            let guid = source.guid.clone();
            let mut draft = engine.prepare(&guid).unwrap();
            draft.workspace_id = "demo".into();
            let path = root.path().join("file.txt");
            std::fs::write(&path, "user selected").unwrap();
            let media = media_root(root.path(), "mock").unwrap();
            draft
                .attachments
                .push(lark2tapd_core::media::copy_attachment(&media, &path).unwrap());
            engine
                .save_draft_for_source(&guid, &draft, &source)
                .await
                .unwrap();
            engine.commit(&mock, &guid, &draft, None).await.unwrap();
            let mut uncertain = draft.attachments[0].clone();
            uncertain.uncertain = true;
            engine.attachment_progress(&guid, &uncertain).await.unwrap();
            let before = engine.snapshot().clone();
            assert!(
                submit(
                    &mut engine,
                    &Connection::Mock(mock),
                    &media,
                    &guid,
                    draft,
                    source
                )
                .await
                .unwrap_err()
                .to_string()
                .contains("uncertain")
            );
            assert_eq!(engine.snapshot(), &before);
        });
    }
    #[test]
    fn duplicate_reads_coalesce_and_old_completion_cannot_clear_new_read() {
        let mut pending = BTreeMap::new();
        assert!(begin_read(&mut pending, "tasks", 1, None));
        assert!(!begin_read(&mut pending, "tasks", 1, None));
        assert!(begin_read(&mut pending, "projects", 1, None));
        assert!(begin_read(&mut pending, "tasks", 2, None));
        assert!(!finish_read(&mut pending, "tasks", 1));
        assert!(!begin_read(&mut pending, "tasks", 2, None));
        assert!(finish_read(&mut pending, "tasks", 2));
        assert!(begin_read(&mut pending, "tasks", 2, None));
    }
    #[test]
    fn cancellation_releases_only_stale_attachment_reservations_once() {
        let mut pending = BTreeMap::new();
        begin_read(
            &mut pending,
            "paste",
            1,
            Some(("a".into(), "mock".into(), Some("opaque:marker".into()), 7)),
        );
        begin_read(
            &mut pending,
            "file",
            1,
            Some(("b".into(), "mock".into(), None, 8)),
        );
        begin_read(&mut pending, "tasks", 1, None);
        begin_read(
            &mut pending,
            "new-paste",
            2,
            Some(("c".into(), "mock".into(), Some("keep".into()), 9)),
        );
        let events = cancel_stale_reads(&mut pending, 2);
        let failures: Vec<_> = events
            .iter()
            .filter_map(|event| {
                if let Event::AttachmentFailed {
                    guid,
                    profile,
                    marker,
                    session,
                    ..
                } = event
                {
                    Some((guid.as_str(), profile.as_str(), marker.as_deref(), *session))
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            failures,
            vec![
                ("b", "mock", None, 8),
                ("a", "mock", Some("opaque:marker"), 7)
            ]
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Event::Activity { active: false, .. }))
                .count(),
            3
        );
        assert!(!finish_read(&mut pending, "paste", 1));
        assert!(cancel_stale_reads(&mut pending, 2).is_empty());
        assert!(finish_read(&mut pending, "new-paste", 2));
    }
    #[test]
    fn cached_identity_cannot_escape_data_directory() {
        assert!(profile_file(Mode::Live, Some("../../secret")).is_err());
        assert!(profile_file(Mode::Live, Some(&"a".repeat(64))).is_ok());
    }
}

#[cfg(test)]
#[path = "worker_connection_tests.rs"]
mod connection_tests;
