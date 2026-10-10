//! 单线程工作队列拥有数据库和 CLI；GPUI 主线程只收发消息，不等待网络或 SQLite。
use anyhow::{Result, bail};
use lark2tapd_bridge::installer::{
    CliTool, InstallationPlan, install_cli, installation_plan, probe_cli,
};
use lark2tapd_bridge::{CliGateway, Credentials, load_credentials, save_credentials};
use lark2tapd_core::{
    Draft, Engine, Gateway, MockGateway, Settings, Snapshot, SourceTask, TapdMember, TapdWorkspace,
    TargetTask,
};
use std::{
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Mock,
    Live,
}

pub enum Job {
    LoadProjects,
    LoadMembers(String),
    AddWorkspace(TapdWorkspace),
    PlanInstall(CliTool),
    Install(CliTool),
    CancelInstall,
    Refresh,
    OpenDraft(String, bool),
    ClearCredentials,
    SaveSettings(Settings),
    SaveDraft(String, Draft, SourceTask),
    Preview(String, Draft, SourceTask),
    Commit(String, Draft, Option<TargetTask>),
    SaveCredentials(Credentials),
    Login,
    Check(CliTool),
    ChangeDemo,
    Recover {
        guid: String,
        workspace: String,
        id: String,
        kind: String,
    },
}

#[derive(Clone)]
pub struct Editor {
    pub source: SourceTask,
    pub draft: Draft,
    pub profile: String,
}

#[derive(Clone)]
pub struct Preview {
    pub source: SourceTask,
    pub profile: String,
    pub guid: String,
    pub draft: Draft,
    pub target: Option<TargetTask>,
}

pub enum Event {
    State {
        mode: Mode,
        snapshot: Snapshot,
        profile: String,
        message: &'static str,
        new_count: usize,
    },
    Preview(Preview),
    Editor(Editor),
    InstallPlan(InstallationPlan),
    Diagnostic {
        tool: CliTool,
        version: Option<String>,
        connected: bool,
        detail: String,
    },
    Error(String),
}

pub struct Request {
    pub job: Job,
    pub profile: String,
}

pub struct WorkerHandle {
    pub jobs: mpsc::Sender<Request>,
    pub events: async_channel::Receiver<Event>,
}

// 显式枚举只有两种传输，避免为了 mock 引入通用插件系统。
enum Connection {
    Mock(MockGateway),
    Live(Box<CliGateway>),
}
impl Gateway for Connection {
    fn list_workspaces(&self) -> Result<Vec<TapdWorkspace>> {
        match self {
            Self::Mock(g) => g.list_workspaces(),
            Self::Live(g) => g.list_workspaces(),
        }
    }
    fn list_members(&self, workspace: &str) -> Result<Vec<TapdMember>> {
        match self {
            Self::Mock(g) => g.list_members(workspace),
            Self::Live(g) => g.list_members(workspace),
        }
    }
    fn get_source(&self, guid: &str) -> Result<SourceTask> {
        match self {
            Self::Mock(g) => g.get_source(guid),
            Self::Live(g) => g.get_source(guid),
        }
    }
    fn validate_draft(&self, draft: &Draft) -> Result<()> {
        match self {
            Self::Mock(g) => g.validate_draft(draft),
            Self::Live(g) => g.validate_draft(draft),
        }
    }
    fn fetch_tasks(&self) -> Result<Vec<SourceTask>> {
        match self {
            Self::Mock(g) => g.fetch_tasks(),
            Self::Live(g) => g.fetch_tasks(),
        }
    }
    fn get_target(&self, w: &str, id: &str, k: &str) -> Result<TargetTask> {
        match self {
            Self::Mock(g) => g.get_target(w, id, k),
            Self::Live(g) => g.get_target(w, id, k),
        }
    }
    fn create_target(&self, d: &Draft, s: &SourceTask) -> Result<TargetTask> {
        match self {
            Self::Mock(g) => g.create_target(d, s),
            Self::Live(g) => g.create_target(d, s),
        }
    }
    fn update_target(&self, t: &TargetTask, d: &Draft, s: &SourceTask) -> Result<TargetTask> {
        match self {
            Self::Mock(g) => g.update_target(t, d, s),
            Self::Live(g) => g.update_target(t, d, s),
        }
    }
}

pub fn start(root: PathBuf, mode: Mode) -> WorkerHandle {
    let (jobs, input) = mpsc::channel::<Request>();
    let (output, events) = async_channel::unbounded();
    std::thread::spawn(move || {
        let result = (|| -> Result<()> {
            std::fs::create_dir_all(&root)?;
            // 应用私有目录不允许其他本机用户读取任务缓存。
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
            }
            // 同一数据目录只能有一个写入 actor，进程崩溃时系统自动释放文件锁。
            let lock = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(root.join("app.lock"))?;
            lock.try_lock().map_err(|_| {
                anyhow::anyhow!("This data directory is already open in another app instance")
            })?;
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(async {
                let mut preferences = Engine::open(&root.join(if mode == Mode::Mock {"preferences-mock.db"} else {"preferences-live.db"})).await?;
                let mut settings = preferences.snapshot().settings.clone();
                let mut engine = Engine::open(&root.join(profile_file(mode,settings.last_profile.as_deref())?)).await?;
                if mode == Mode::Mock && settings.workspace_id.is_empty() { settings.workspace_id = "demo".into(); }
                settings.lark_path = discover_executable(&settings.lark_path, "lark-cli");
                settings.tapd_path = discover_executable(&settings.tapd_path, "tapd-cli");
                engine.save_settings(settings.clone()).await?;
                let mock = MockGateway::from_snapshot(engine.snapshot());
                if mode==Mode::Mock && engine.snapshot().directory.workspaces_fetched_at.is_none() {
                    engine.refresh_workspaces(&mock).await?;
                    engine.refresh_members(&mock,"demo").await?;
                    engine.refresh_members(&mock,"demo-2").await?;
                }
                let tools_dir=std::fs::canonicalize(&root)?.join("tools");
                let mut pending_install=None;
                let mut namespace = if mode == Mode::Mock { "mock".to_owned() } else { settings.last_profile.clone().unwrap_or_default() };
                let mut next_check = Instant::now() + interval(&settings);
                output.send(Event::State { mode, snapshot: engine.snapshot().clone(), profile:namespace.clone(), message: "ready", new_count: 0 }).await?;
                loop {
                    let wait = if settings.poll_enabled { next_check.saturating_duration_since(Instant::now()) } else { Duration::from_secs(3600) };
                    let Request { job, profile } = match input.recv_timeout(wait) {
                        Ok(request) => request,
                        Err(mpsc::RecvTimeoutError::Timeout) if settings.poll_enabled => Request { job:Job::Refresh,profile:namespace.clone() },
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    let reschedule=resets_polling(&job,&settings);
                    let refresh_attempt=matches!(&job,Job::Refresh);
                    let mut new_count = 0;
                    let result: Result<&'static str> = async {
                        if mode == Mode::Mock && matches!(job, Job::SaveCredentials(_) | Job::ClearCredentials | Job::Login | Job::Check(_) | Job::PlanInstall(_) | Job::Install(_)) {
                            bail!("Demo mode never accesses credentials or external accounts. Restart with LARK2TAPD_MOCK=0 to connect.");
                        }
                        // 配置与认证不依赖已连通的任务账户，因此未配置时也可修复连接。
                        match job {
                            Job::PlanInstall(tool)=>{
                                let plan=installation_plan(tool,&tools_dir)?;
                                pending_install=Some(tool);
                                output.send(Event::InstallPlan(plan)).await?;
                                return Ok("install_plan");
                            }
                            Job::CancelInstall=>{pending_install=None;return Ok("install_cancelled");}
                            Job::Install(tool)=>{
                                if pending_install.take()!=Some(tool){bail!("Review the installation source and destination before confirming");}
                                let path=install_cli(tool,&tools_dir)?.to_string_lossy().into_owned();
                                let mut next=settings.clone();
                                match tool {CliTool::Lark=>next.lark_path=path,CliTool::Tapd=>next.tapd_path=path}
                                preferences.save_settings(next.clone()).await?;
                                engine.save_settings(next.clone()).await?;
                                settings=next;
                                return Ok("installed");
                            }
                            Job::SaveSettings(next) => {
                                preferences.save_settings(next.clone()).await?;
                                engine.save_settings(next.clone()).await?;
                                settings = next;
                                return Ok("settings_saved");
                            }
                            Job::ClearCredentials => {
                                lark2tapd_bridge::clear_credentials()?;
                                namespace.clear();
                                engine = Engine::open(&root.join("disconnected.db")).await?;
                                engine.save_settings(settings.clone()).await?;
                                return Ok("credentials_cleared");
                            }
                            Job::SaveCredentials(credentials) => {
                                save_credentials(&credentials)?;
                                // 清空界面中旧账户数据。后续操作重新验证账户并选择独立数据库。
                                namespace.clear();
                                engine = Engine::open(&root.join("disconnected.db")).await?;
                                engine.save_settings(settings.clone()).await?;
                                return Ok("credentials_saved");
                            }
                            Job::Login => {
                                CliGateway::new(settings.clone(), load_credentials()?)?.login_lark()?;
                                namespace.clear();
                                engine = Engine::open(&root.join("disconnected.db")).await?;
                                engine.save_settings(settings.clone()).await?;
                                return Ok("authorized");
                            }
                            Job::Check(tool) => {
                                let path=if tool==CliTool::Lark {&settings.lark_path}else{&settings.tapd_path};
                                let mut version=None;
                                let result=(||->Result<()>{
                                    version=Some(probe_cli(tool,std::path::Path::new(path))?);
                                    let cli=CliGateway::new(settings.clone(),load_credentials()?)?;
                                    match tool{CliTool::Lark=>cli.check_lark(),CliTool::Tapd=>cli.check_tapd()}
                                })();
                                let connected=result.is_ok();
                                let detail=result.err().map(|e|e.to_string()).unwrap_or_default();
                                output.send(Event::Diagnostic {tool,version,connected,detail}).await?;
                                return Ok("diagnosed");
                            }
                            _ => {}
                        }
                        // 缓存阅读和草稿保存不依赖网络；远端预览/确认仍重新验证账户。
                        if matches!(&job, Job::OpenDraft(..)|Job::SaveDraft(..)) && profile != namespace {
                            bail!("Account changed. Refresh tasks before editing or importing.");
                        }
                        match job {
                            Job::OpenDraft(guid, discard) => {
                                if discard { engine.discard_draft(&guid).await?; }
                                let draft = engine.prepare(&guid)?;
                                let source = engine.snapshot().records.iter().find(|r|r.source.guid==guid).ok_or_else(||anyhow::anyhow!("Task no longer available"))?.source.clone();
                                output.send(Event::Editor(Editor {source,draft,profile:namespace.clone()})).await?;
                                return Ok("editor_ready");
                            }
                            Job::SaveDraft(guid,draft,source) => { engine.save_draft_for_source(&guid,&draft,&source).await?; return Ok("draft_saved"); }
                            _=>{}
                        }
                        let gateway = if mode == Mode::Mock { Connection::Mock(mock.clone()) } else {
                            let cli = CliGateway::new(settings.clone(), load_credentials()?)?;
                            let current = cli.account_namespace()?;
                            if current != namespace {
                                engine = Engine::open(&root.join(profile_file(Mode::Live,Some(&current))?)).await?;
                                settings.last_profile=Some(current.clone());
                                engine.save_settings(settings.clone()).await?;
                                preferences.save_settings(settings.clone()).await?;
                                namespace = current;
                                // 编辑/确认属于之前展示的账户，不能跨账户复用。
                            }
                            Connection::Live(Box::new(cli))
                        };
                        if !matches!(job, Job::Refresh | Job::ChangeDemo | Job::LoadProjects) && profile != namespace {
                            bail!("Account changed. Refresh tasks before editing or importing.");
                        }
                        match job {
                            Job::LoadProjects=>{engine.refresh_workspaces(&gateway).await?;Ok("projects_loaded")}
                            Job::LoadMembers(workspace)=>{engine.refresh_members(&gateway,&workspace).await?;Ok("members_loaded")}
                            Job::AddWorkspace(workspace)=>{
                                if workspace.name.trim().is_empty(){bail!("Project display name is required");}
                                engine.refresh_members(&gateway,&workspace.id).await?;
                                engine.add_workspace(workspace).await?;
                                Ok("project_added")
                            }
                            Job::Refresh => { new_count = engine.refresh(&gateway).await?; Ok("refreshed") }
                            Job::Preview(guid,draft,source) => {
                                gateway.validate_draft(&draft)?;
                                engine.validate_destination(&draft)?;
                                engine.save_draft_for_source(&guid,&draft,&source).await?;
                                let target = engine.preview_target(&gateway,&guid)?;
                                output.send(Event::Preview(Preview {source,profile:namespace.clone(),guid,draft,target})).await?;
                                Ok("preview_ready")
                            }
                            Job::Commit(guid,draft,target) => { engine.commit(&gateway,&guid,&draft,target).await?; Ok("imported") }
                            Job::ChangeDemo => { mock.change_demo_task()?; new_count = engine.refresh(&gateway).await?; Ok("refreshed") }
                            Job::Recover {guid,workspace,id,kind} => { engine.bind_existing(&gateway,&guid,&workspace,&id,&kind).await?; Ok("recovered") }
                            _ => unreachable!(),
                        }
                    }.await;
                    // 编辑、查看和外观修改不能把固定轮询变成「空闲一段时间后才检查」。
                    if reschedule && (result.is_ok() || refresh_attempt) {
                        next_check = Instant::now() + interval(&settings);
                    }
                    match result {
                        Ok(message) => { output.send(Event::State {mode,snapshot:engine.snapshot().clone(),profile:namespace.clone(),message,new_count}).await?; }
                        Err(error) => {
                            output.send(Event::State {mode,snapshot:engine.snapshot().clone(),profile:namespace.clone(),message:"",new_count:0}).await?;
                            output.send(Event::Error(error.to_string())).await?;
                        }
                    }
                }
                Ok::<_,anyhow::Error>(())
            })
        })();
        if let Err(error) = result {
            let _ = output.send_blocking(Event::Error(error.to_string()));
        }
    });
    WorkerHandle { jobs, events }
}

// 持久化配置仍是不可信输入；缓存定位只接受固定长度散列，不能逃逸应用数据目录。
fn profile_file(mode: Mode, profile: Option<&str>) -> Result<String> {
    if mode == Mode::Mock {
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

fn resets_polling(job: &Job, settings: &Settings) -> bool {
    match job {
        Job::Refresh => true,
        Job::SaveSettings(next) => {
            (next.poll_enabled, next.poll_minutes) != (settings.poll_enabled, settings.poll_minutes)
        }
        _ => false,
    }
}

fn interval(settings: &Settings) -> Duration {
    Duration::from_secs(u64::from(settings.poll_minutes.clamp(1, 1440)) * 60)
}

fn discover_executable(configured: &str, fallback: &str) -> String {
    if std::path::Path::new(configured).is_absolute() {
        return configured.into();
    }
    std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|p| p.join(fallback))
                .find(|p| p.is_file())
        })
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| fallback.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn editor_and_appearance_jobs_do_not_postpone_polling() {
        let settings = Settings::default();
        let mut theme = settings.clone();
        theme.theme = "dark".into();
        for job in [
            Job::OpenDraft("guid".into(), false),
            Job::Login,
            Job::Check(CliTool::Lark),
            Job::SaveSettings(theme),
        ] {
            assert!(!resets_polling(&job, &settings));
        }
        let mut interval = settings.clone();
        interval.poll_minutes = 60;
        assert!(resets_polling(&Job::SaveSettings(interval), &settings));
        assert!(resets_polling(&Job::Refresh, &settings));
    }
    #[test]
    fn cached_identity_cannot_escape_data_directory() {
        assert!(profile_file(Mode::Live, Some("../../secret")).is_err());
        assert!(profile_file(Mode::Live, Some(&"a".repeat(64))).is_ok());
    }
}
