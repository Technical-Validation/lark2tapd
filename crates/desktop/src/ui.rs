mod media;
mod selectors;
mod settings;
use gpui_kit::component::{
    calendar::Date,
    date_picker::{DatePicker, DatePickerEvent, DatePickerState},
    select::{SearchableVec, Select, SelectEvent, SelectState},
};
use gpui_kit::component::{collapsible::Collapsible, switch::Switch};
use lark2tapd_bridge::installer::{CliTool, InstallationPlan};
use lark2tapd_core::{DirectoryCache, DraftAttachment, TapdWorkspace};
use selectors::{ChoiceList, ChoiceState, selected};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::PathBuf,
    sync::Arc,
};
mod tasks;
// 主界面只管理视图和输入；同步、认证和持久化通过工作队列执行。
use crate::{
    i18n::text,
    native::{self, NativeAction, StatusMenu},
    worker::{Editor, Event, Job, Mode, Preference, Request, WorkerHandle, WorkerSender},
};
use gpui_kit::base::Disableable;
use gpui_kit::component::{
    ActiveTheme, StyledExt, Theme, ThemeMode,
    button::{Button, ButtonVariants},
    input::{Input, InputState, Textarea, TextareaState},
};
use gpui_kit::{prelude::*, *};
use lark2tapd_bridge::Credentials;
use lark2tapd_core::{Draft, Snapshot, SourceTask, SyncRecord, SyncStatus};

#[derive(Clone)]
struct MediaReady {
    guid: String,
    profile: String,
    attachment: DraftAttachment,
    marker: Option<String>,
    session: u64,
}

#[derive(Clone)]
struct MediaFailure {
    guid: String,
    profile: String,
    session: u64,
    marker: Option<String>,
    message: String,
}
#[derive(Clone)]
struct RemoteContext {
    guid: String,
    profile: String,
    session: u64,
    target: lark2tapd_core::TargetTask,
}
static EDITOR_SESSIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
fn editor_session() -> u64 {
    EDITOR_SESSIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

pub struct Model {
    pub snapshot: Option<Snapshot>,
    pub busy: bool,
    pub notice: String,
    pub error: bool,
    pub mode: Mode,
    pub install_plan: Option<InstallationPlan>,
    pub lark_diagnostic: String,
    pub tapd_diagnostic: String,
    pub activities: BTreeSet<String>,
    pub errors: BTreeMap<&'static str, String>,
    pub checked_workspace: Option<TapdWorkspace>,
    pub data_root: PathBuf,
    state_revision: u64,
    last_action: &'static str,
    pending_request: u64,
    profile: String,
    jobs: WorkerSender,
    tray: Option<StatusMenu>,
}
impl EventEmitter<Editor> for Model {}
impl EventEmitter<MediaReady> for Model {}
impl EventEmitter<MediaFailure> for Model {}
impl EventEmitter<RemoteContext> for Model {}
impl Model {
    fn send(&mut self, job: Job, cx: &mut Context<Self>) {
        self.send_at(job, self.profile.clone(), cx);
    }
    fn send_at(&mut self, job: Job, profile: String, cx: &mut Context<Self>) {
        // 测试按钮始终可点；演示模式解释边界，不伪装为真实连接成功。
        if let Job::Check(tool) = &job
            && self.mode == Mode::Mock
        {
            let note=text(self.language(),"演示模式：未执行真实连接测试。以 LARK2TAPD_MOCK=0 重启后可测试真实服务。","Demo mode: no real connection test was run. Restart with LARK2TAPD_MOCK=0 to test real services.").to_owned();
            match tool {
                CliTool::Lark => self.lark_diagnostic = note,
                CliTool::Tapd => self.tapd_diagnostic = note,
            }
            cx.notify();
            return;
        }
        let field = match &job {
            Job::SetPreference(Preference::PollMinutes(_)) => Some("poll"),
            Job::SetPreference(Preference::DefaultDestination { .. }) => Some("destination"),
            Job::SetPreference(Preference::WorkspacePrefixes { .. }) => Some("prefixes"),
            Job::CheckWorkspace(_) | Job::AddWorkspace(_) => Some("new-workspace"),
            Job::Submit(..) | Job::CheckTarget { .. } | Job::AcceptTarget { .. } => Some("submit"),
            Job::SaveCredentials(_) | Job::ClearCredentials | Job::Login => Some("credentials"),
            Job::LocalFile { .. } | Job::PastedImage { .. } => Some("attachments"),
            _ => None,
        };
        if let Some(field) = field {
            self.errors.remove(field);
        }
        if matches!(&job, Job::Submit(..)) {
            if self.activities.contains("submit") {
                return;
            }
            self.activities.insert("submit".into());
            self.busy = true;
        }
        if self.jobs.send(Request { job, profile }).is_err() {
            self.error = true;
            self.notice = text(
                self.language(),
                "后台服务已停止，请重启应用。",
                "Background service stopped. Restart the app.",
            )
            .into();
            self.activities.clear();
            self.busy = false;
        }
        cx.notify();
    }
    fn receive(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::State {
                mode,
                snapshot,
                profile,
                message,
                new_count,
            } => {
                let changed = self.profile != profile;
                let first = changed
                    || self
                        .snapshot
                        .as_ref()
                        .is_none_or(|s| s.last_checked.is_none());
                if changed {
                    self.checked_workspace = None;
                    self.errors.remove("submit");
                    self.errors.remove("attachments");
                }
                if self.language() != snapshot.settings.language {
                    set_app_menu(&snapshot.settings.language, cx);
                }
                if self
                    .snapshot
                    .as_ref()
                    .is_none_or(|s| s.settings.theme != snapshot.settings.theme)
                {
                    apply_theme(&snapshot.settings.theme, cx);
                }
                self.profile = profile;
                self.mode = mode;
                let count = snapshot
                    .records
                    .iter()
                    .filter(|r| r.in_scope && r.status() == SyncStatus::Unimported)
                    .count();
                if let Some(tray) = &self.tray {
                    tray.update(
                        &snapshot.settings.language,
                        count,
                        snapshot.settings.poll_enabled,
                    );
                }
                if new_count > 0 && !first && mode == Mode::Live {
                    native::notify_new(&snapshot.settings.language, new_count);
                }
                if matches!(
                    message,
                    "installed" | "credentials_saved" | "credentials_cleared" | "authorized"
                ) {
                    self.lark_diagnostic.clear();
                    self.tapd_diagnostic.clear();
                }
                if matches!(message, "installed" | "install_cancelled") {
                    self.install_plan = None;
                }
                if !message.is_empty() {
                    self.notice = message_text(&snapshot.settings.language, message).into();
                    self.error = false;
                }
                gpui_kit::component::set_locale(&snapshot.settings.language);
                self.snapshot = Some(snapshot);
                self.state_revision += 1;
                self.last_action = message;
            }
            Event::Activity { key, active } => {
                if active {
                    self.activities.insert(key);
                } else {
                    self.activities.remove(&key);
                }
                self.busy =
                    self.activities.contains("submit") || self.activities.contains("install");
            }
            Event::FormError { field, message } => {
                if field == "attachments" {
                    return;
                }
                if field == "submit" {
                    self.activities.remove("submit");
                    self.busy = false;
                }
                self.errors.insert(field, message);
            }
            Event::WorkspaceChecked(workspace) => {
                self.checked_workspace = Some(workspace);
            }
            Event::AttachmentReady {
                guid,
                profile,
                attachment,
                marker,
                session,
            } => cx.emit(MediaReady {
                guid,
                profile,
                attachment,
                marker,
                session,
            }),
            Event::AttachmentFailed {
                guid,
                profile,
                marker,
                session,
                message,
            } => cx.emit(MediaFailure {
                guid,
                profile,
                marker,
                session,
                message,
            }),
            Event::RemoteTarget {
                guid,
                profile,
                session,
                target,
            } => cx.emit(RemoteContext {
                guid,
                profile,
                session,
                target,
            }),
            Event::Editor(editor) => {
                self.errors.remove("submit");
                self.errors.remove("attachments");
                cx.emit(editor);
            }
            Event::InstallPlan(plan) => {
                self.install_plan = Some(plan);
            }
            Event::Diagnostic {
                tool,
                version,
                connected,
                detail,
            } => {
                let version = version.unwrap_or_else(|| {
                    text(self.language(), "CLI 不可用", "CLI unavailable").into()
                });
                let status = if connected {
                    text(self.language(), "连接通过", "Connected")
                } else {
                    text(self.language(), "连接未通过", "Connection failed")
                };
                let result = format!("{version} · {status} {detail}");
                match tool {
                    CliTool::Lark => self.lark_diagnostic = result,
                    CliTool::Tapd => self.tapd_diagnostic = result,
                }
            }
            Event::Error(error) => {
                self.error = true;
                self.notice = format!(
                    "{} {error}",
                    text(self.language(), "操作未完成：", "Could not complete:")
                );
            }
        }
        cx.notify();
    }
    fn language(&self) -> &str {
        self.snapshot
            .as_ref()
            .map_or("zh-CN", |s| s.settings.language.as_str())
    }
}

struct Shell {
    model: Entity<Model>,
    window: Option<AnyWindowHandle>,
}
impl Global for Shell {}

actions!(lark2tapd, [Quit, Refresh, OpenPending]);

pub fn start(worker: WorkerHandle, mode: Mode, cx: &mut App) {
    let (native_tx, native_rx) = async_channel::unbounded();
    let tray = StatusMenu::new(native_tx).ok();
    let model = cx.new(|_| Model {
        snapshot: None,
        busy: false,
        notice: String::new(),
        error: false,
        mode,
        install_plan: None,
        lark_diagnostic: String::new(),
        tapd_diagnostic: String::new(),
        activities: BTreeSet::new(),
        errors: BTreeMap::new(),
        checked_workspace: None,
        data_root: worker.data_root.clone(),
        state_revision: 0,
        last_action: "",
        pending_request: 0,
        profile: String::new(),
        jobs: worker.jobs,
        tray,
    });
    cx.set_global(Shell {
        model: model.clone(),
        window: None,
    });
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-r", Refresh, None),
        KeyBinding::new("cmd-1", OpenPending, None),
    ]);
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Refresh, cx| {
        let m = cx.global::<Shell>().model.clone();
        m.update(cx, |m, cx| m.send(Job::Refresh, cx));
    });
    cx.on_action(|_: &OpenPending, cx| open_pending(cx));
    set_app_menu("zh-CN", cx);
    show_window(cx);
    // Entity 由 App 全局拥有；关闭窗口不会销毁轮询和消息消费者。
    cx.spawn(async move |cx| {
        while let Ok(event) = worker.events.recv().await {
            model.update(cx, |model, cx| {
                model.receive(event, cx);
            });
        }
    })
    .detach();
    cx.spawn(async move |cx| {
        while let Ok(action) = native_rx.recv().await {
            cx.update(|cx| match action {
                NativeAction::Open => show_window(cx),
                NativeAction::Pending => open_pending(cx),
                NativeAction::Quit => cx.quit(),
                NativeAction::Refresh => {
                    let m = cx.global::<Shell>().model.clone();
                    m.update(cx, |m, cx| m.send(Job::Refresh, cx));
                }
                NativeAction::TogglePolling => {
                    let m = cx.global::<Shell>().model.clone();
                    m.update(cx, |m, cx| {
                        if let Some(s) = &m.snapshot {
                            let mut s = s.settings.clone();
                            s.poll_enabled = !s.poll_enabled;
                            m.send(Job::SetPreference(Preference::Polling(s.poll_enabled)), cx);
                        }
                    });
                }
            });
        }
    })
    .detach();
}

fn set_app_menu(language: &str, cx: &mut App) {
    cx.set_menus(vec![Menu {
        name: "Lark → TAPD".into(),
        disabled: false,
        items: vec![
            MenuItem::action(text(language, "立即检查", "Check now"), Refresh),
            MenuItem::action(
                text(language, "未同步任务", "Unimported tasks"),
                OpenPending,
            ),
            MenuItem::separator(),
            MenuItem::action(text(language, "退出", "Quit"), Quit),
        ],
    }]);
}

fn open_pending(cx: &mut App) {
    let model = cx.global::<Shell>().model.clone();
    model.update(cx, |model, cx| {
        model.pending_request += 1;
        cx.notify();
    });
    show_window(cx);
}

pub fn show_window(cx: &mut App) {
    if !cx.has_global::<Shell>() {
        return;
    }
    if let Some(handle) = cx.global::<Shell>().window
        && handle
            .update(cx, |_, window, cx| {
                window.activate_window();
                cx.activate(true);
            })
            .is_ok()
    {
        let m = cx.global::<Shell>().model.clone();
        m.update(cx, |m, cx| m.send(Job::Refresh, cx));
        return;
    }
    let model = cx.global::<Shell>().model.clone();
    let theme = model
        .read(cx)
        .snapshot
        .as_ref()
        .map_or("system", |s| s.settings.theme.as_str())
        .to_owned();
    apply_theme(&theme, cx);
    let result = gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(1180.), px(780.)),
                cx,
            ))),
            window_min_size: Some(size(px(920.), px(640.))),
            titlebar: Some(TitlebarOptions {
                title: Some("Lark → TAPD".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        cx,
        |window, cx| cx.new(|cx| Workspace::new(model, window, cx)),
    );
    match result {
        Ok((handle, _)) => {
            cx.global_mut::<Shell>().window = Some(handle);
            cx.activate(true);
            let m = cx.global::<Shell>().model.clone();
            m.update(cx, |m, cx| m.send(Job::Refresh, cx));
        }
        Err(error) => {
            eprintln!("Unable to open application window: {error}");
            cx.quit();
        }
    }
}

fn apply_theme(mode: &str, cx: &mut App) {
    match mode {
        "light" => Theme::change(ThemeMode::Light, None, cx),
        "dark" => Theme::change(ThemeMode::Dark, None, cx),
        _ => Theme::sync_system_appearance(None, cx),
    }
    Theme::update(cx, |theme| {
        // 马尔斯绿的深浅变体保证小字号按钮文字对比度，焦点环保留品牌原色。
        let dark = theme.mode == ThemeMode::Dark;
        theme.colors.primary = rgb(if dark { 0x59c3bc } else { 0x006764 }).into();
        theme.colors.primary_foreground = rgb(if dark { 0x171b1b } else { 0xffffff }).into();
        theme.colors.ring = rgb(0x008c8c).into();
        // Kit 的按钮采用独立令牌，不能只改 primary；同时覆盖交互状态和次级操作。
        theme.colors.button_primary = theme.colors.primary;
        theme.colors.button_primary_foreground = theme.colors.primary_foreground;
        theme.colors.button_primary_hover = rgb(if dark { 0x71d1ca } else { 0x007873 }).into();
        theme.colors.button_primary_active = rgb(if dark { 0x46aaa4 } else { 0x00534f }).into();
        theme.colors.button_secondary = rgb(if dark { 0x103d39 } else { 0xedf7f5 }).into();
        theme.colors.button_secondary_foreground = theme.colors.foreground;
        theme.colors.button_secondary_hover = rgb(if dark { 0x194d47 } else { 0xd6efea }).into();
        theme.colors.button_secondary_active = rgb(if dark { 0x235e57 } else { 0xbfe5de }).into();
        theme.colors.secondary_foreground = theme.colors.foreground;
        theme.colors.accent_foreground = theme.colors.foreground;
        theme.colors.link = theme.colors.foreground;
        theme.colors.link_hover = theme.colors.foreground;
        theme.colors.link_active = theme.colors.foreground;
        theme.font_size = px(14.);
        theme.radius = px(7.);
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    Pending,
    Changed,
    Synced,
    All,
    Settings,
}
impl Filter {
    fn matches(self, r: &SyncRecord) -> bool {
        if !r.in_scope {
            return false;
        }
        match self {
            Self::Pending => r.status() == SyncStatus::Unimported,
            Self::Changed => {
                r.has_pending_attachments()
                    || matches!(r.status(), SyncStatus::Changed | SyncStatus::Uncertain)
            }
            Self::Synced => r.status() == SyncStatus::Synced && !r.has_pending_attachments(),
            Self::All => true,
            Self::Settings => false,
        }
    }
}

pub struct Workspace {
    model: Entity<Model>,
    filter: Filter,
    selected: Option<String>,
    edit_source: Option<SourceTask>,
    edit_profile: String,
    editor_session: u64,
    remote_target: Option<lark2tapd_core::TargetTask>,
    source_open: bool,
    new_workspace_open: bool,
    clear_confirmation: bool,
    last_directory: Option<DirectoryCache>,
    last_prefixes: BTreeMap<String, Vec<String>>,
    seen_state_revision: u64,
    pending_seen: u64,
    search: Entity<InputState>,
    title: Entity<InputState>,
    description: Entity<TextareaState>,
    workspace: Entity<ChoiceState>,
    owner: Entity<ChoiceState>,
    prefix: Entity<ChoiceState>,
    due: Entity<DatePickerState>,
    settings_workspace: Entity<ChoiceState>,
    settings_owner: Entity<ChoiceState>,
    prefix_workspace: Entity<ChoiceState>,
    prefix_options: Entity<TextareaState>,
    manual_workspace_id: Entity<InputState>,
    manual_workspace_name: Entity<InputState>,
    poll: Entity<InputState>,
    lark_path: Entity<InputState>,
    tapd_path: Entity<InputState>,
    endpoint: Entity<InputState>,
    app_id: Entity<InputState>,
    app_secret: Entity<InputState>,
    lark_token: Entity<InputState>,
    tapd_token: Entity<InputState>,
    recover_id: Entity<InputState>,
    recover_kind: String,
    attachments: Vec<DraftAttachment>,
    images: HashMap<String, Arc<Image>>,
    loading_images: BTreeSet<String>,
    pending_pastes: BTreeSet<String>,
    pending_files: usize,
    pending_file_paths: BTreeSet<PathBuf>,
    next_paste: u64,
}
impl Workspace {
    fn new(model: Entity<Model>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search / 搜索任务"));
        cx.subscribe(
            &search,
            |_, _, _: &gpui_kit::component::input::InputEvent, cx| cx.notify(),
        )
        .detach();
        let description = cx.new(|cx| TextareaState::new(window, cx));
        cx.subscribe(
            &description,
            |_, _, _: &gpui_kit::component::input::InputEvent, cx| cx.notify(),
        )
        .detach();
        let poll = cx.new(|cx| InputState::new(window, cx));
        cx.subscribe(
            &poll,
            |this, input, event: &gpui_kit::component::input::InputEvent, cx| {
                if matches!(event, gpui_kit::component::input::InputEvent::Change) {
                    match input.read(cx).value().parse::<u32>() {
                        Ok(minutes) if (1..=1440).contains(&minutes) => {
                            this.preference(Preference::PollMinutes(minutes), cx)
                        }
                        _ => this.form_error(
                            "poll",
                            this.tr(cx, "请输入 1–1440 分钟", "Enter 1–1440 minutes")
                                .into(),
                            cx,
                        ),
                    }
                }
            },
        )
        .detach();
        let workspace = choice_state(window, cx);
        let settings_workspace = choice_state(window, cx);
        let prefix_workspace = choice_state(window, cx);
        cx.subscribe_in(
            &workspace,
            window,
            |this, _, _: &SelectEvent<ChoiceList>, window, cx| {
                this.project_changed(false, window, cx)
            },
        )
        .detach();
        cx.subscribe_in(
            &settings_workspace,
            window,
            |this, _, _: &SelectEvent<ChoiceList>, window, cx| {
                this.project_changed(true, window, cx)
            },
        )
        .detach();
        cx.subscribe_in(
            &prefix_workspace,
            window,
            |this, _, _: &SelectEvent<ChoiceList>, window, cx| this.load_prefix_form(window, cx),
        )
        .detach();
        let due = cx.new(|cx| DatePickerState::new(window, cx).date_format("%Y-%m-%d"));
        cx.subscribe(&due, |_, _, _: &DatePickerEvent, cx| cx.notify())
            .detach();
        cx.subscribe_in(&model, window, |this, _, editor: &Editor, window, cx| {
            if this.editor_session != editor.session
                || this.selected.as_ref() != Some(&editor.source.guid)
                || this.edit_profile != editor.profile
            {
                return;
            }
            this.edit_source = Some(editor.source.clone());
            this.edit_profile = editor.profile.clone();
            this.selected = Some(editor.source.guid.clone());
            let d = &editor.draft;
            set(&this.title, d.title.clone(), window, cx);
            this.fill_projects(false, &d.workspace_id, window, cx);
            this.fill_members(false, &d.owner, window, cx);
            this.fill_prefixes(&d.title_prefix, window, cx);
            this.due.update(cx, |s, cx| {
                s.set_date(
                    Date::Single(
                        d.due
                            .as_ref()
                            .and_then(|v| chrono::NaiveDate::parse_from_str(v, "%Y-%m-%d").ok()),
                    ),
                    window,
                    cx,
                )
            });
            this.description
                .update(cx, |s, cx| s.set_value(d.description.clone(), window, cx));
            this.attachments = d.attachments.clone();
            this.images.clear();
            this.loading_images.clear();
            this.load_images(window, cx);
            let workspace = d.workspace_id.clone();
            if !workspace.is_empty() {
                let model = this.model.clone();
                let profile = editor.profile.clone();
                cx.defer(move |cx| {
                    model.update(cx, |m, cx| {
                        m.send_at(Job::LoadMembers(workspace), profile, cx)
                    })
                });
            }
            cx.notify();
        })
        .detach();
        cx.subscribe_in(&model, window, |this, _, ready: &MediaReady, window, cx| {
            this.accept_attachment(ready, window, cx)
        })
        .detach();
        cx.subscribe_in(
            &model,
            window,
            |this, _, failure: &MediaFailure, window, cx| {
                this.reject_attachment(failure, window, cx)
            },
        )
        .detach();
        cx.subscribe(&model, |this, _, remote: &RemoteContext, cx| {
            if this.selected.as_ref() == Some(&remote.guid)
                && this.edit_profile == remote.profile
                && this.editor_session == remote.session
            {
                this.remote_target = Some(remote.target.clone());
                cx.notify();
            }
        })
        .detach();
        cx.observe_in(&model, window, |this, _, window, cx| {
            let (revision, action, settings) = {
                let m = this.model.read(cx);
                (
                    m.state_revision,
                    m.last_action,
                    m.snapshot.as_ref().map(|s| s.settings.clone()),
                )
            };
            if this.seen_state_revision != revision {
                this.seen_state_revision = revision;
                if action == "installed"
                    && let Some(s) = &settings
                {
                    set(&this.lark_path, s.lark_path.clone(), window, cx);
                    set(&this.tapd_path, s.tapd_path.clone(), window, cx);
                }
                if matches!(action, "credentials_saved" | "credentials_cleared") {
                    for input in [
                        &this.app_id,
                        &this.app_secret,
                        &this.lark_token,
                        &this.tapd_token,
                    ] {
                        set(input, String::new(), window, cx);
                    }
                }
                if action == "target_accepted" {
                    this.remote_target = None;
                }
                if action == "project_added" {
                    this.new_workspace_open = false;
                }
            }
            this.sync_directory(window, cx);
            if let Some(settings) = settings
                && this.last_prefixes != settings.workspace_prefixes
            {
                this.last_prefixes = settings.workspace_prefixes;
                let prefix = selected(&this.prefix, cx);
                this.fill_prefixes(&prefix, window, cx);
            }
            if (!this.edit_profile.is_empty() && this.edit_profile != this.model.read(cx).profile)
                || this.selected.as_ref().is_some_and(|id| {
                    this.model.read(cx).snapshot.as_ref().is_some_and(|s| {
                        !s.records.iter().any(|r| &r.source.guid == id && r.in_scope)
                    })
                })
            {
                this.selected = None;
                this.edit_source = None;
                this.edit_profile.clear();
                this.attachments.clear();
                this.images.clear();
                this.pending_pastes.clear();
                this.pending_files = 0;
                this.pending_file_paths.clear();
            }
            if let Some(record) = this.selected.as_ref().and_then(|id| {
                this.model
                    .read(cx)
                    .snapshot
                    .as_ref()?
                    .records
                    .iter()
                    .find(|r| &r.source.guid == id)
            }) && let Some(saved) = &record.last_draft
            {
                for a in &mut this.attachments {
                    if let Some(old) = saved.attachments.iter().find(|v| v.id == a.id) {
                        a.remote_id = old.remote_id.clone();
                        a.remote_url = old.remote_url.clone();
                        a.uncertain = old.uncertain;
                    }
                }
            }
            let request = this.model.read(cx).pending_request;
            if request != this.pending_seen {
                this.pending_seen = request;
                this.filter = Filter::Pending;
            }
            cx.notify();
        })
        .detach();
        let observed = model.clone();
        window
            .observe_window_appearance(move |_, cx| {
                if observed
                    .read(cx)
                    .snapshot
                    .as_ref()
                    .is_some_and(|s| s.settings.theme == "system")
                {
                    apply_theme("system", cx);
                }
            })
            .detach();
        let mut this = Self {
            model,
            filter: Filter::Pending,
            selected: None,
            edit_source: None,
            edit_profile: String::new(),
            source_open: false,
            editor_session: editor_session(),
            remote_target: None,
            new_workspace_open: false,
            clear_confirmation: false,
            last_directory: None,
            last_prefixes: BTreeMap::new(),
            seen_state_revision: 0,
            pending_seen: 0,
            search,
            title: cx.new(|cx| InputState::new(window, cx)),
            description,
            workspace,
            owner: choice_state(window, cx),
            prefix: choice_state(window, cx),
            due,
            settings_workspace,
            settings_owner: choice_state(window, cx),
            prefix_workspace,
            prefix_options: cx.new(|cx| TextareaState::new(window, cx)),
            manual_workspace_id: cx.new(|cx| InputState::new(window, cx)),
            manual_workspace_name: cx.new(|cx| InputState::new(window, cx)),
            poll,
            lark_path: cx.new(|cx| InputState::new(window, cx)),
            tapd_path: cx.new(|cx| InputState::new(window, cx)),
            endpoint: cx.new(|cx| InputState::new(window, cx)),
            app_id: cx.new(|cx| InputState::new(window, cx)),
            app_secret: cx.new(|cx| InputState::new(window, cx).masked(true)),
            lark_token: cx.new(|cx| InputState::new(window, cx).masked(true)),
            tapd_token: cx.new(|cx| InputState::new(window, cx).masked(true)),
            recover_id: cx.new(|cx| InputState::new(window, cx)),
            recover_kind: "story".into(),
            attachments: Vec::new(),
            images: HashMap::new(),
            loading_images: BTreeSet::new(),
            pending_pastes: BTreeSet::new(),
            pending_files: 0,
            pending_file_paths: BTreeSet::new(),
            next_paste: 0,
        };
        this.sync_directory(window, cx);
        this
    }
    fn tr<'a>(&self, cx: &App, zh: &'a str, en: &'a str) -> &'a str {
        text(self.model.read(cx).language(), zh, en)
    }
    fn send(&self, job: Job, cx: &mut Context<Self>) {
        self.model.update(cx, |m, cx| m.send(job, cx));
    }
    fn send_editor(&self, job: Job, cx: &mut Context<Self>) {
        self.model
            .update(cx, |m, cx| m.send_at(job, self.edit_profile.clone(), cx));
    }
    fn preference(&self, p: Preference, cx: &mut Context<Self>) {
        self.send(Job::SetPreference(p), cx);
    }
    fn form_error(&self, key: &'static str, message: String, cx: &mut Context<Self>) {
        self.model.update(cx, |m, cx| {
            m.errors.insert(key, message);
            cx.notify();
        });
    }
    fn inline_error(&self, key: &str, cx: &App) -> AnyElement {
        div()
            .when_some(self.model.read(cx).errors.get(key).cloned(), |d, e| {
                d.mt_1().text_sm().text_color(cx.theme().danger).child(e)
            })
            .into_any_element()
    }
    fn draft(&self, cx: &App) -> Draft {
        Draft {
            title: self.title.read(cx).value().to_string(),
            title_prefix: selected(&self.prefix, cx),
            attachments: self.attachments.clone(),
            description: self.description.read(cx).value().to_string(),
            workspace_id: selected(&self.workspace, cx),
            owner: selected(&self.owner, cx),
            due: match self.due.read(cx).date() {
                Date::Single(d) => d.map(|d| d.format("%Y-%m-%d").to_string()),
                _ => None,
            },
        }
    }
    fn select(&mut self, record: SyncRecord, window: &mut Window, cx: &mut Context<Self>) {
        if self.model.read(cx).busy || !self.pending_pastes.is_empty() || self.pending_files > 0 {
            return;
        }
        self.source_open = false;
        self.editor_session = editor_session();
        self.remote_target = None;
        self.attachments.clear();
        self.images.clear();
        self.loading_images.clear();
        set(&self.title, String::new(), window, cx);
        this_clear_description(&self.description, window, cx);
        self.selected = Some(record.source.guid.clone());
        self.edit_source = None;
        self.edit_profile = self.model.read(cx).profile.clone();
        self.send(
            Job::OpenDraft(record.source.guid, false, self.editor_session),
            cx,
        );
        self.send(Job::LoadProjects, cx);
        if !selected(&self.workspace, cx).is_empty() {
            self.send(Job::LoadMembers(selected(&self.workspace, cx)), cx);
        }
        cx.notify();
    }
    fn reload_source(&mut self, guid: String, cx: &mut Context<Self>) {
        self.editor_session = editor_session();
        self.edit_source = None;
        self.attachments.clear();
        self.images.clear();
        self.loading_images.clear();
        self.pending_pastes.clear();
        self.pending_files = 0;
        self.pending_file_paths.clear();
        self.remote_target = None;
        self.send(Job::OpenDraft(guid, true, self.editor_session), cx);
    }
    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter = Filter::Settings;
        if let Some(s) = self
            .model
            .read(cx)
            .snapshot
            .as_ref()
            .map(|s| s.settings.clone())
        {
            self.fill_projects(true, &s.workspace_id, window, cx);
            self.fill_members(true, &s.owner, window, cx);
            self.fill_prefix_workspace(&s.workspace_id, window, cx);
            self.load_prefix_form(window, cx);
            for (input, value) in [
                (&self.poll, s.poll_minutes.to_string()),
                (&self.lark_path, s.lark_path),
                (&self.tapd_path, s.tapd_path),
                (&self.endpoint, s.tapd_endpoint),
            ] {
                set(input, value, window, cx);
            }
        }
        self.send(Job::LoadProjects, cx);
        let id = selected(&self.settings_workspace, cx);
        if !id.is_empty() {
            self.send(Job::LoadMembers(id), cx);
        }
        cx.notify();
    }
    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.model.read(cx);
        let records = model
            .snapshot
            .as_ref()
            .map(|s| s.records.as_slice())
            .unwrap_or(&[]);
        let entries = [
            (Filter::Pending, "nav-pending", "未同步", "Unimported"),
            (Filter::Changed, "nav-changed", "待处理", "Needs attention"),
            (Filter::Synced, "nav-synced", "已同步", "Synced"),
            (Filter::All, "nav-all", "全部任务", "All tasks"),
        ];
        div()
            .flex()
            .flex_col()
            .w(px(184.))
            .flex_shrink_0()
            .h_full()
            .p_4()
            .gap_2()
            .bg(cx.theme().muted)
            .child(
                div()
                    .pt_3()
                    .pb_6()
                    .text_lg()
                    .font_semibold()
                    .child("Lark → TAPD"),
            )
            .child(
                div()
                    .mb_4()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.tr(cx, "飞书任务，一处跟进", "Your tasks, connected")),
            )
            .children(entries.into_iter().map(|(filter, id, zh, en)| {
                let count = records.iter().filter(|r| filter.matches(r)).count();
                Button::new(id)
                    .label(format!("{}  {count}", self.tr(cx, zh, en)))
                    .ghost()
                    .w_full()
                    .when(self.filter == filter, |b| b.primary())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.filter = filter;
                        cx.notify();
                    }))
            }))
            .child(div().flex_1())
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .mb_2()
                    .child(if model.mode == Mode::Mock {
                        self.tr(cx, "演示模式 · 不会写入远端", "Demo · no remote writes")
                    } else {
                        self.tr(cx, "真实连接 · 手动确认写入", "Live · confirm every write")
                    }),
            )
            .child(
                Button::new("nav-settings")
                    .label(self.tr(cx, "配置中心", "Settings"))
                    .ghost()
                    .w_full()
                    .when(self.filter == Filter::Settings, |b| b.primary())
                    .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx))),
            )
    }
}

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.model.read(cx);
        let busy = model.busy;
        let checking = model.activities.contains("tasks");
        let notice = model.notice.clone();
        let error = model.error;
        let is_mock = model.mode == Mode::Mock;
        let checked = model
            .snapshot
            .as_ref()
            .and_then(|s| s.last_checked.clone())
            .unwrap_or_else(|| self.tr(cx, "尚未检查", "Not checked yet").into());
        div()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .font_family(cx.theme().font_family.clone())
            .text_sm()
            .flex()
            .child(self.sidebar(cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .px_5()
                            .py_3()
                            .flex()
                            .gap_3()
                            .items_center()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .child(
                                div()
                                    .flex_1()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "{}: {checked}",
                                        self.tr(cx, "最近检查", "Last check")
                                    )),
                            )
                            .child(
                                div()
                                    .id("check-status")
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(div().size(px(7.)).rounded_full().bg(if checking {
                                        cx.theme().primary
                                    } else {
                                        cx.theme().muted_foreground
                                    }))
                                    .child(if checking {
                                        self.tr(cx, "正在检查", "Checking")
                                    } else {
                                        self.tr(cx, "就绪", "Ready")
                                    }),
                            )
                            .when(is_mock, |d| {
                                d.child(
                                    Button::new("demo-change")
                                        .label(self.tr(
                                            cx,
                                            "模拟来源变更",
                                            "Simulate a source change",
                                        ))
                                        .ghost()
                                        .disabled(busy)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.send(Job::ChangeDemo, cx)
                                        })),
                                )
                            })
                            .child(
                                Button::new("refresh")
                                    .label(if checking {
                                        self.tr(cx, "检查中…", "Checking…")
                                    } else {
                                        self.tr(cx, "立即检查", "Check now")
                                    })
                                    .primary()
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.send(Job::Refresh, cx)),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .when(self.filter == Filter::Settings, |d| {
                                d.child(self.settings_view(cx))
                            })
                            .when(self.filter != Filter::Settings, |d| {
                                d.child(self.task_list(cx)).child(self.detail(cx))
                            }),
                    )
                    .child(
                        div()
                            .px_5()
                            .py_2()
                            .min_h(px(36.))
                            .bg(cx.theme().muted)
                            .text_xs()
                            .text_color(if error {
                                cx.theme().danger
                            } else {
                                cx.theme().muted_foreground
                            })
                            .child(notice),
                    ),
            )
    }
}

fn set(input: &Entity<InputState>, value: String, window: &mut Window, cx: &mut App) {
    input.update(cx, |s, cx| s.set_value(value, window, cx));
}
fn field(label: &str, input: impl IntoElement) -> impl IntoElement {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_2()
        .child(div().text_sm().child(label.to_owned()))
        .child(input)
}
fn section(label: &str) -> impl IntoElement {
    div()
        .mt_2()
        .font_semibold()
        .text_lg()
        .child(label.to_owned())
}
fn status_text(language: &str, status: SyncStatus) -> &'static str {
    match status {
        SyncStatus::Unimported => text(language, "未同步", "Unimported"),
        SyncStatus::Synced => text(language, "已同步", "Synced"),
        SyncStatus::Changed => text(language, "来源有变更", "Source changed"),
        SyncStatus::Uncertain => text(language, "待核对写入结果", "Verify write result"),
    }
}
fn message_text<'a>(language: &str, key: &'a str) -> &'a str {
    match key {
        "cached" => text(
            language,
            "保留当前内容，正在后台读取",
            "Keeping current content while refreshing in the background",
        ),
        "target_accepted" => text(
            language,
            "已接受远端版本，可以继续编辑和更新",
            "Remote version accepted; continue editing and update when ready",
        ),
        "projects_loaded" => text(
            language,
            "项目列表已刷新并缓存",
            "Project options refreshed and cached",
        ),
        "members_loaded" => text(
            language,
            "处理人列表已刷新并缓存",
            "Assignee options refreshed and cached",
        ),
        "project_added" => text(
            language,
            "项目已验证并加入缓存",
            "Project verified and cached",
        ),
        "install_plan" => text(
            language,
            "请核对安装来源与目录，再确认安装",
            "Review the installation source and destination before confirming",
        ),
        "install_cancelled" => text(
            language,
            "已取消安装，未修改本机工具",
            "Installation cancelled; local tools unchanged",
        ),
        "installed" => text(
            language,
            "CLI 已安装，路径已保存",
            "CLI installed and executable path saved",
        ),
        "diagnosed" => text(
            language,
            "连接诊断完成，请查看各 CLI 结果",
            "Connection diagnostics finished; see each CLI result",
        ),
        "editor_ready" => text(language, "已读取草稿", "Draft loaded"),
        "credentials_cleared" => text(
            language,
            "已清除应用保存的凭证，CLI 外部登录态未修改",
            "Saved app credentials cleared. External CLI login is unchanged.",
        ),
        "ready" => text(language, "已加载本地缓存", "Local cache loaded"),
        "refreshed" => text(language, "检查完成", "Check complete"),
        "settings_saved" => text(language, "配置已保存", "Settings saved"),
        "draft_saved" => text(language, "草稿已保存", "Draft saved"),
        "credentials_saved" => text(
            language,
            "凭证已加密保存，请检查连接后刷新任务",
            "Credentials saved securely. Check connection, then refresh tasks.",
        ),
        "imported" => text(
            language,
            "同步完成，已保存关联记录",
            "Sync complete. Mapping saved.",
        ),
        "recovered" => text(language, "关联已验证", "Mapping verified"),
        "authorized" => text(language, "飞书已授权", "Lark authorized"),
        "connected" => text(language, "连接检查通过", "Connection checks passed"),
        _ => key,
    }
}

// 任务数据中的 URL 不受信任，只允许已知平台的 HTTPS 链接，拒绝 file/javascript 等协议。
fn open_safe_url(raw: &str, cx: &mut App) {
    let lark = lark2tapd_bridge::safe_url(raw, true);
    let safe = if lark.is_empty() {
        lark2tapd_bridge::safe_url(raw, false)
    } else {
        lark
    };
    if !safe.is_empty() {
        cx.open_url(&safe);
    }
}

#[cfg(test)]
mod tests;

fn choice_state(window: &mut Window, cx: &mut Context<Workspace>) -> Entity<ChoiceState> {
    cx.new(|cx| {
        SelectState::new(
            SearchableVec::new(Vec::<selectors::Choice>::new()),
            None,
            window,
            cx,
        )
        .searchable(true)
    })
}
fn this_clear_description(input: &Entity<TextareaState>, window: &mut Window, cx: &mut App) {
    input.update(cx, |s, cx| s.set_value(String::new(), window, cx));
}
