mod appearance;
use appearance::apply_theme;
mod connection_results;
mod connections;
use connections::{CredentialChange, ServiceConnection};
mod editor;
mod feedback;
mod focus_scroll;
mod help;
mod last_check;
mod media;
use editor::DeferredMedia;
use zorite_editor::EditorState;
mod selectors;
mod settings;
mod settings_navigation;
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
    ActiveTheme, StyledExt, ThemeMode,
    button::{Button, ButtonVariants},
    input::{Input, InputState},
};
use gpui_kit::{prelude::*, *};
use lark2tapd_bridge::{ConnectionReport, Credentials, PermissionState};
use lark2tapd_core::{Draft, Snapshot, SourceTask, SyncRecord, SyncStatus};

#[derive(Clone)]
struct Feedback {
    message: String,
    error: bool,
}

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
    connections_loaded: bool,
    lark_connection: ServiceConnection,
    tapd_connection: ServiceConnection,
    pub notice: String,
    pub error: bool,
    pending_error: Option<String>,
    pub mode: Mode,
    pub install_plan: Option<InstallationPlan>,
    lark_authorization: Option<(String, u64)>,
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
impl EventEmitter<CredentialChange> for Model {}
impl Model {
    fn send(&mut self, job: Job, cx: &mut Context<Self>) {
        self.send_at(job, self.profile.clone(), cx);
    }
    fn send_at(&mut self, job: Job, profile: String, cx: &mut Context<Self>) {
        let field = match &job {
            Job::SetPreference(Preference::PollMinutes(_)) => Some("poll"),
            Job::SetPreference(Preference::DefaultDestination { .. }) => Some("destination"),
            Job::SetPreference(
                Preference::AddWorkspacePrefix { .. } | Preference::RemoveWorkspacePrefix { .. },
            ) => Some("prefixes"),
            Job::CheckWorkspace(_) | Job::AddWorkspace(_) => Some("new-workspace"),
            Job::Submit(..) | Job::CheckTarget { .. } | Job::AcceptTarget { .. } => Some("submit"),
            Job::SaveAndConnect { tool, .. }
            | Job::ClearCredentials(tool)
            | Job::Check(tool)
            | Job::TestConnection { tool, .. } => Some(connections::field_key(*tool)),
            Job::LoadConnectionStatus => Some("connection-status"),
            Job::ProbeCli(tool) => Some(connections::cli_key(*tool)),
            Job::LocalFile { .. } | Job::PastedImage { .. } => Some("attachments"),
            _ => None,
        };
        if let Some(field) = field {
            self.errors.remove(field);
        }
        if let Job::ProbeCli(tool) = &job {
            let state = self.connection_mut(*tool);
            state.cli_error = None;
        }
        if let Job::Check(tool) | Job::TestConnection { tool, .. } = &job {
            if self.activities.contains(connections::field_key(*tool)) {
                return;
            }
            self.activities.insert(connections::field_key(*tool).into());
        }
        // 提交即锁定该服务，避免后台 Activity 到达前的重复保存/清除。
        if let Job::SaveAndConnect { tool, .. } | Job::ClearCredentials(tool) = &job {
            if matches!(&job, Job::SaveAndConnect { .. })
                && self.activities.contains(connections::field_key(*tool))
            {
                return;
            }
            self.activities.insert(connections::field_key(*tool).into());
            self.connection_mut(*tool).invalidate_result();
            if *tool == CliTool::Lark {
                self.lark_authorization = None;
            }
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
            self.queue_feedback(cx);
        }
        cx.notify();
    }
    fn receive(&mut self, event: Event, cx: &mut Context<Self>) {
        // 轻量提示只反馈操作结果；缓存刷新、目录读取等后台活动不反复弹出。
        let feedback = matches!(&event, Event::Error(_))
            || matches!(
                &event,
                Event::State {
                    message: "imported"
                        | "draft_saved"
                        | "settings_saved"
                        | "credentials_saved"
                        | "credentials_cleared"
                        | "recovered"
                        | "authorized"
                        | "target_accepted"
                        | "installed",
                    ..
                }
            );
        match event {
            Event::CliProbe {
                tool,
                version,
                error,
            } => {
                let state = self.connection_mut(tool);
                state.cli_version = version;
                state.cli_error = error;
                if state.cli_version.is_none() {
                    state.verified = false;
                }
            }
            Event::ConnectionReport {
                tool,
                report,
                saved_configuration,
            } => {
                if let Some(error) = &report.failure {
                    self.errors
                        .insert(connections::field_key(tool), error.clone());
                } else {
                    self.errors.remove(connections::field_key(tool));
                }
                let state = self.connection_mut(tool);
                if saved_configuration {
                    state.verified = report.connected;
                }
                state.report = Some(report);
                state.tested_at = Some(chrono::Utc::now().to_rfc3339());
                state.tested_saved_configuration = saved_configuration;
                cx.defer(move |cx| connection_results::show(tool, cx));
            }
            Event::AuthorizationRequired {
                verification_url,
                expires_in,
            } => {
                self.lark_authorization = Some((verification_url, expires_in));
            }
            Event::AuthorizationFinished => self.lark_authorization = None,
            Event::ConnectionStatus {
                lark_saved,
                tapd_saved,
            } => {
                self.connections_loaded = true;
                self.lark_connection.saved = lark_saved;
                self.tapd_connection.saved = tapd_saved;
            }
            Event::CredentialsSaved(tool) => {
                self.connection_mut(tool).saved = true;
                self.connection_mut(tool).invalidate_result();
                cx.emit(CredentialChange {
                    tool,
                    cleared: false,
                });
            }
            Event::CredentialsCleared(tool) => {
                let state = self.connection_mut(tool);
                state.saved = false;
                state.invalidate_result();
                if tool == CliTool::Lark {
                    self.lark_authorization = None;
                }
                cx.emit(CredentialChange {
                    tool,
                    cleared: true,
                });
            }
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
                for (tool, path) in [
                    (CliTool::Lark, &snapshot.settings.lark_path),
                    (CliTool::Tapd, &snapshot.settings.tapd_path),
                ] {
                    let changed_path = self.snapshot.as_ref().is_some_and(|s| match tool {
                        CliTool::Lark => &s.settings.lark_path != path,
                        CliTool::Tapd => &s.settings.tapd_path != path,
                    });
                    if changed_path || message == "installed" {
                        let state = self.connection_mut(tool);
                        state.cli_version = None;
                        state.cli_error = None;
                        state.invalidate_result();
                        let model = cx.entity();
                        cx.defer(move |cx| {
                            model.update(cx, |m, cx| m.send(Job::ProbeCli(tool), cx))
                        });
                    }
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
                for tool in [CliTool::Lark, CliTool::Tapd] {
                    if field == connections::field_key(tool) {
                        self.activities.remove(field);
                        cx.defer(move |cx| connection_results::show(tool, cx));
                    }
                }
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
            Event::Error(error) => {
                self.error = true;
                self.notice = format!(
                    "{} {error}",
                    text(self.language(), "操作未完成：", "Could not complete:")
                );
            }
        }
        if feedback {
            self.queue_feedback(cx);
        }
        cx.notify();
    }
    fn queue_feedback(&mut self, cx: &mut Context<Self>) {
        if self.error {
            self.pending_error = Some(self.notice.clone());
        }
        let note = Feedback {
            message: self.notice.clone(),
            error: self.error,
        };
        cx.defer(move |cx| feedback::present(note, cx));
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
    settings_window: Option<AnyWindowHandle>,
}
impl Global for Shell {}

actions!(
    lark2tapd,
    [
        Quit,
        Refresh,
        OpenPending,
        OpenSettings,
        CloseWindow,
        CancelConfirmation
    ]
);

pub fn start(worker: WorkerHandle, mode: Mode, cx: &mut App) {
    let (native_tx, native_rx) = async_channel::unbounded();
    let tray = StatusMenu::new(native_tx).ok();
    let model = cx.new(|_| Model {
        snapshot: None,
        busy: false,
        connections_loaded: false,
        lark_connection: ServiceConnection::default(),
        tapd_connection: ServiceConnection::default(),
        notice: String::new(),
        error: false,
        pending_error: None,
        mode,
        install_plan: None,
        lark_authorization: None,
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
        settings_window: None,
    });
    configure_actions(cx);
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
            cx.update(|cx| handle_native_action(action, cx));
        }
    })
    .detach();
}

fn configure_actions(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
        KeyBinding::new("escape", CloseWindow, Some("ConnectionResult")),
        KeyBinding::new("escape", CancelConfirmation, Some("Settings")),
        KeyBinding::new("cmd-r", Refresh, None),
        KeyBinding::new("cmd-1", OpenPending, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
    ]);
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Refresh, cx| {
        let m = cx.global::<Shell>().model.clone();
        m.update(cx, |m, cx| m.send(Job::Refresh, cx));
    });
    cx.on_action(|_: &OpenPending, cx| open_pending(cx));
    cx.on_action(|_: &OpenSettings, cx| show_settings(cx));
    set_app_menu("zh-CN", cx);
}

// 菜单栏和页面入口共用窗口路由，不借助主窗口的页面切换。
fn handle_native_action(action: NativeAction, cx: &mut App) {
    match action {
        NativeAction::Open => show_window(cx),
        NativeAction::Pending => open_pending(cx),
        NativeAction::Settings => show_settings(cx),
        NativeAction::Quit => cx.quit(),
        NativeAction::Refresh => {
            let model = cx.global::<Shell>().model.clone();
            model.update(cx, |m, cx| m.send(Job::Refresh, cx));
        }
        NativeAction::TogglePolling => {
            let model = cx.global::<Shell>().model.clone();
            model.update(cx, |m, cx| {
                if let Some(s) = &m.snapshot {
                    let enabled = !s.settings.poll_enabled;
                    m.send(Job::SetPreference(Preference::Polling(enabled)), cx);
                }
            });
        }
    }
}

fn set_app_menu(language: &str, cx: &mut App) {
    cx.set_menus(vec![Menu {
        name: "Lark → TAPD".into(),
        disabled: false,
        items: vec![
            MenuItem::action(text(language, "设置…", "Settings…"), OpenSettings),
            MenuItem::separator(),
            MenuItem::action(text(language, "立即检查", "Check now"), Refresh),
            MenuItem::action(
                text(language, "未同步任务", "Unimported tasks"),
                OpenPending,
            ),
            MenuItem::separator(),
            MenuItem::action(text(language, "关闭窗口", "Close window"), CloseWindow),
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
        feedback::replay(cx);
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
            feedback::replay(cx);
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

fn show_settings(cx: &mut App) {
    if !cx.has_global::<Shell>() {
        return;
    }
    if let Some(handle) = cx.global::<Shell>().settings_window
        && handle
            .update(cx, |_, window, cx| {
                window.activate_window();
                cx.activate(true);
            })
            .is_ok()
    {
        feedback::replay(cx);
        return;
    }
    let model = cx.global::<Shell>().model.clone();
    let title = text(model.read(cx).language(), "设置", "Settings").to_owned();
    // 控件状态绑定各自 Window；共享后台模型，但绝不复用主窗口正在编辑的表单实体。
    let result = gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(760.), px(720.)),
                cx,
            ))),
            window_min_size: Some(size(px(620.), px(520.))),
            titlebar: Some(TitlebarOptions {
                title: Some(title.into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        cx,
        |window, cx| {
            cx.new(|cx| {
                let mut view = Workspace::new(model.clone(), window, cx);
                view.is_settings = true;
                view.load_settings(window, cx);
                view
            })
        },
    );
    match result {
        Ok((handle, _)) => {
            cx.global_mut::<Shell>().settings_window = Some(handle);
            feedback::replay(cx);
            cx.activate(true);
        }
        Err(error) => model.update(cx, |m, cx| m.receive(Event::Error(error.to_string()), cx)),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    Pending,
    Changed,
    Synced,
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
        }
    }
}

pub struct Workspace {
    model: Entity<Model>,
    focus_handle: FocusHandle,
    last_check: Entity<last_check::LastCheck>,
    filter: Filter,
    is_settings: bool,
    settings_loaded: bool,
    settings_scroll: ScrollHandle,
    connection_bounds: [std::rc::Rc<std::cell::Cell<Option<Bounds<Pixels>>>>; 2],
    cli_focus: [FocusHandle; 2],
    settings_target: Option<CliTool>,
    settings_directory_loaded: bool,
    selected: Option<String>,
    edit_source: Option<SourceTask>,
    edit_profile: String,
    editor_session: u64,
    remote_target: Option<lark2tapd_core::TargetTask>,
    source_open: bool,
    new_workspace_open: bool,
    clear_confirmation: Option<CliTool>,
    lark_form_open: bool,
    tapd_form_open: bool,
    last_directory: Option<DirectoryCache>,
    last_prefixes: BTreeMap<String, Vec<String>>,
    seen_state_revision: u64,
    pending_seen: u64,
    search: Entity<InputState>,
    title: Entity<InputState>,
    description: Entity<EditorState>,
    editor_appearance: Option<(bool, String)>,
    description_scroll: ScrollHandle,
    deferred_media: Vec<DeferredMedia>,
    workspace: Entity<ChoiceState>,
    owner: Entity<ChoiceState>,
    prefix: Entity<ChoiceState>,
    due: Entity<DatePickerState>,
    settings_workspace: Entity<ChoiceState>,
    settings_owner: Entity<ChoiceState>,
    prefix_workspace: Entity<ChoiceState>,
    prefix_options: Entity<InputState>,
    new_prefix_open: bool,
    manual_workspace_id: Entity<InputState>,
    manual_workspace_name: Entity<InputState>,
    poll: Entity<InputState>,
    lark_path: Entity<InputState>,
    tapd_path: Entity<InputState>,
    app_id: Entity<InputState>,
    app_secret: Entity<InputState>,
    lark_token: Entity<InputState>,
    tapd_token: Entity<InputState>,
    recover_id: Entity<InputState>,
    recover_kind: String,
    attachments: Vec<DraftAttachment>,
    images: HashMap<String, Arc<RenderImage>>,
    loading_images: BTreeSet<String>,
    pending_pastes: BTreeSet<String>,
    pending_files: usize,
    pending_file_paths: BTreeSet<PathBuf>,
    next_paste: u64,
}
impl Workspace {
    fn new(model: Entity<Model>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // 空白处也有键盘事件路径；子控件仍可接管焦点和 Escape。
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search / 搜索任务"));
        cx.subscribe(
            &search,
            |_, _, _: &gpui_kit::component::input::InputEvent, cx| cx.notify(),
        )
        .detach();
        let description_scroll = ScrollHandle::new();
        let description = editor::create_description(description_scroll.clone(), window, cx);
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
                .update(cx, |s, cx| s.set_text(d.description.clone(), cx));
            this.deferred_media.clear();
            this.attachments = d.attachments.clone();
            this.images.clear();
            this.refresh_editor_images(cx);
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
        cx.subscribe_in(
            &model,
            window,
            |this, _, change: &CredentialChange, window, cx| {
                this.clear_connection_inputs(change.tool, window, cx);
                this.set_connection_form(change.tool, change.cleared);
                this.clear_confirmation = None;
                cx.notify();
            },
        )
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
                if action == "target_accepted" {
                    this.remote_target = None;
                }
                if action == "project_added" {
                    this.new_workspace_open = false;
                }
            }
            if this.is_settings && !this.settings_loaded && settings.is_some() {
                this.load_settings(window, cx);
            }
            if this.is_settings
                && this.settings_loaded
                && let Some(tool) = this.settings_target
            {
                this.navigate_connection(tool, window, cx);
            }
            if this.is_settings {
                if !this.model.read(cx).tapd_configured() {
                    this.settings_directory_loaded = false;
                } else if !this.settings_directory_loaded {
                    this.settings_directory_loaded = true;
                    this.send(Job::LoadProjects, cx);
                    let id = selected(&this.settings_workspace, cx);
                    if !id.is_empty() {
                        this.send(Job::LoadMembers(id), cx);
                    }
                }
            }
            this.sync_directory(window, cx);
            if let Some(settings) = settings
                && this.last_prefixes != settings.workspace_prefixes
            {
                this.last_prefixes = settings.workspace_prefixes;
                let prefix = selected(&this.prefix, cx);
                this.fill_prefixes(&prefix, window, cx);
                this.load_prefix_form(window, cx);
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
                this.refresh_editor_images(cx);
                this.pending_pastes.clear();
                this.deferred_media.clear();
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
            focus_handle,
            last_check: cx.new(|cx| last_check::LastCheck::new(model.clone(), cx)),
            model,
            filter: Filter::Pending,
            is_settings: false,
            settings_loaded: false,
            settings_scroll: ScrollHandle::new(),
            connection_bounds: std::array::from_fn(|_| {
                std::rc::Rc::new(std::cell::Cell::new(None))
            }),
            cli_focus: std::array::from_fn(|_| cx.focus_handle()),
            settings_target: None,
            settings_directory_loaded: false,
            selected: None,
            edit_source: None,
            edit_profile: String::new(),
            source_open: false,
            editor_session: editor_session(),
            remote_target: None,
            new_workspace_open: false,
            clear_confirmation: None,
            lark_form_open: false,
            tapd_form_open: false,
            last_directory: None,
            last_prefixes: BTreeMap::new(),
            seen_state_revision: 0,
            pending_seen: 0,
            search,
            title: cx.new(|cx| InputState::new(window, cx)),
            description,
            editor_appearance: None,
            description_scroll,
            deferred_media: Vec::new(),
            workspace,
            owner: choice_state(window, cx),
            prefix: choice_state(window, cx),
            due,
            settings_workspace,
            settings_owner: choice_state(window, cx),
            prefix_workspace,
            prefix_options: cx.new(|cx| InputState::new(window, cx)),
            new_prefix_open: false,
            manual_workspace_id: cx.new(|cx| InputState::new(window, cx)),
            manual_workspace_name: cx.new(|cx| InputState::new(window, cx)),
            poll,
            lark_path: cx.new(|cx| InputState::new(window, cx)),
            tapd_path: cx.new(|cx| InputState::new(window, cx)),
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
        self.refresh_editor_images(cx);
        self.loading_images.clear();
        set(&self.title, String::new(), window, cx);
        self.description.update(cx, |s, cx| s.set_text("", cx));
        self.deferred_media.clear();
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
        self.refresh_editor_images(cx);
        self.loading_images.clear();
        self.pending_pastes.clear();
        self.deferred_media.clear();
        self.pending_files = 0;
        self.pending_file_paths.clear();
        self.remote_target = None;
        self.send(Job::OpenDraft(guid, true, self.editor_session), cx);
    }
    fn load_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.model.read(cx).snapshot.is_none() {
            return;
        }
        if let Some(s) = self
            .model
            .read(cx)
            .snapshot
            .as_ref()
            .map(|s| s.settings.clone())
        {
            self.settings_loaded = true;
            self.fill_projects(true, &s.workspace_id, window, cx);
            self.fill_members(true, &s.owner, window, cx);
            self.fill_prefix_workspace(&s.workspace_id, window, cx);
            self.load_prefix_form(window, cx);
            for (input, value) in [
                (&self.poll, s.poll_minutes.to_string()),
                (&self.lark_path, s.lark_path),
                (&self.tapd_path, s.tapd_path),
            ] {
                set(input, value, window, cx);
            }
        }
        self.send(Job::LoadConnectionStatus, cx);
        for tool in [CliTool::Lark, CliTool::Tapd] {
            let state = self.model.read(cx).connection(tool);
            if state.cli_version.is_none() && state.cli_error.is_none() {
                self.send(Job::ProbeCli(tool), cx);
            }
        }
        cx.notify();
    }
    // 产品入口与状态分段属于列表栏，不再占用第三个导航栏。
    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_kit::component::tab::{Tab, TabBar};
        let model = self.model.read(cx);
        let records = model
            .snapshot
            .as_ref()
            .map(|s| s.records.as_slice())
            .unwrap_or(&[]);
        let entries = [
            (Filter::Pending, "未同步", "Unimported"),
            (Filter::Changed, "待处理", "Attention"),
            (Filter::Synced, "已同步", "Synced"),
        ];
        div().flex().flex_col().gap_3().p_4().child(
            TabBar::new("task-status")
                .segmented()
                .w_full()
                .selected_index(entries.iter().position(|e| e.0 == self.filter).unwrap_or(0))
                .children(entries.into_iter().map(|(filter, zh, en)| {
                    let count = records.iter().filter(|r| filter.matches(r)).count();
                    Tab::new()
                        .flex_1()
                        .label(format!("{} {count}", self.tr(cx, zh, en)))
                }))
                .on_click(cx.listener(|this, index, _, cx| {
                    if let Some(filter) =
                        [Filter::Pending, Filter::Changed, Filter::Synced].get(*index)
                    {
                        this.filter = *filter;
                        cx.notify();
                    }
                })),
        )
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.is_settings {
            window.set_window_title(self.tr(cx, "设置", "Settings"));
            let content = if self.settings_loaded {
                self.settings_view(cx).into_any_element()
            } else {
                div()
                    .p_6()
                    .child(self.tr(cx, "正在读取配置…", "Loading settings…"))
                    .into_any_element()
            };
            return div()
                .key_context("Settings")
                .on_action(cx.listener(|this, _: &CancelConfirmation, _, cx| {
                    this.clear_confirmation = None;
                    cx.notify();
                }))
                .track_focus(&self.focus_handle)
                .on_action(|_: &CloseWindow, window, _| window.remove_window())
                .size_full()
                .bg(cx.theme().background)
                .text_color(cx.theme().foreground)
                .font_family(cx.theme().font_family.clone())
                .text_sm()
                .flex()
                .child(content);
        }
        self.sync_description(window, cx);
        let model = self.model.read(cx);
        let checking = model.activities.contains("tasks");
        div()
            .track_focus(&self.focus_handle)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .font_family(cx.theme().font_family.clone())
            .text_sm()
            .flex()
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
                                // 应用标识与运行模式属于全局栏，侧栏只保留筛选和搜索。
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .debug_selector(|| "app-identity".into())
                                            .text_lg()
                                            .font_semibold()
                                            .child("Lark → TAPD"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .truncate()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(self.tr(
                                                cx,
                                                "手动确认写入",
                                                "Confirm every write",
                                            )),
                                    ),
                            )
                            .child(self.last_check.clone())
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
                            )
                            .child(
                                Button::new("nav-settings")
                                    .icon(gpui_kit::component::IconName::Settings)
                                    .accessibility_label(self.tr(cx, "设置", "Settings"))
                                    .tooltip(self.tr(cx, "设置", "Settings"))
                                    .ghost()
                                    .on_click(|_, _, cx| show_settings(cx)),
                            ),
                    )
                    .child(settings_navigation::hint(&self.model, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .child(self.task_list(cx))
                            .child(self.detail(cx)),
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
