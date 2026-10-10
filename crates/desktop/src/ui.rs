mod settings;
mod tasks;
// 主界面只管理视图和输入；同步、认证和持久化通过工作队列执行。
use crate::{
    i18n::text,
    native::{self, NativeAction, StatusMenu},
    worker::{Editor, Event, Job, Mode, Preview, Request, WorkerHandle},
};
use gpui_kit::base::Disableable;
use gpui_kit::component::{
    ActiveTheme, StyledExt, Theme, ThemeMode,
    button::{Button, ButtonVariants},
    input::{Input, InputState, Textarea, TextareaState},
};
use gpui_kit::{prelude::*, *};
use lark2tapd_bridge::Credentials;
use lark2tapd_core::{Draft, Settings, Snapshot, SourceTask, SyncRecord, SyncStatus};

pub struct Model {
    pub snapshot: Option<Snapshot>,
    pub busy: bool,
    pub notice: String,
    pub error: bool,
    pub preview: Option<Preview>,
    pub mode: Mode,
    pending_request: u64,
    profile: String,
    jobs: std::sync::mpsc::Sender<Request>,
    tray: Option<StatusMenu>,
}

impl EventEmitter<Editor> for Model {}
impl Model {
    fn send(&mut self, job: Job, cx: &mut Context<Self>) {
        self.send_at(job, self.profile.clone(), cx);
    }
    fn send_at(&mut self, job: Job, profile: String, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        match self.jobs.send(Request { job, profile }) {
            Ok(()) => {
                self.busy = true;
                self.error = false;
                self.notice = text(self.language(), "正在处理…", "Working…").into();
            }
            Err(_) => {
                self.error = true;
                self.notice = text(
                    self.language(),
                    "后台服务已停止，请重启应用。",
                    "Background service stopped. Restart the app.",
                )
                .into();
            }
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
                let profile_changed = self.profile != profile;
                if profile_changed {
                    self.preview = None;
                }
                if self.language() != snapshot.settings.language {
                    set_app_menu(&snapshot.settings.language, cx);
                }
                self.profile = profile;
                self.mode = mode;
                let old_theme = self.snapshot.as_ref().map(|s| s.settings.theme.clone());
                let first = profile_changed
                    || self
                        .snapshot
                        .as_ref()
                        .is_none_or(|s| s.last_checked.is_none());
                if old_theme.as_deref() != Some(&snapshot.settings.theme) {
                    apply_theme(&snapshot.settings.theme, cx);
                }
                let count = snapshot
                    .records
                    .iter()
                    .filter(|r| r.status() == SyncStatus::Unimported)
                    .count();
                if let Some(tray) = &self.tray {
                    tray.update(
                        &snapshot.settings.language,
                        count,
                        snapshot.settings.poll_enabled,
                    );
                }
                if new_count > 0 && !first && self.mode == Mode::Live {
                    native::notify_new(&snapshot.settings.language, new_count);
                }
                self.notice = message_text(&snapshot.settings.language, message).into();
                if matches!(
                    message,
                    "imported"
                        | "recovered"
                        | "credentials_saved"
                        | "credentials_cleared"
                        | "authorized"
                ) {
                    self.preview = None;
                }
                self.snapshot = Some(snapshot);
                self.busy = false;
                self.error = false;
            }
            Event::Preview(preview) => {
                self.preview = Some(preview);
            }
            Event::Editor(editor) => {
                self.preview = None;
                cx.emit(editor);
            }
            Event::Error(error) => {
                self.busy = false;
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
        preview: None,
        mode,
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
                            m.send(Job::SaveSettings(s), cx);
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
        theme.colors.primary_foreground = rgb(if dark { 0x052925 } else { 0xffffff }).into();
        theme.colors.ring = rgb(0x008c8c).into();
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
        match self {
            Self::Pending => r.status() == SyncStatus::Unimported,
            Self::Changed => matches!(r.status(), SyncStatus::Changed | SyncStatus::Uncertain),
            Self::Synced => r.status() == SyncStatus::Synced,
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
    clear_confirmation: bool,
    display_mode: Mode,
    search: Entity<InputState>,
    title: Entity<InputState>,
    description: Entity<TextareaState>,
    owner: Entity<InputState>,
    workspace: Entity<InputState>,
    settings_workspace: Entity<InputState>,
    settings_owner: Entity<InputState>,
    due: Entity<InputState>,
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
    pending_seen: u64,
}

impl Workspace {
    fn new(model: Entity<Model>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search / 搜索任务"));
        cx.subscribe(
            &search,
            |_, _, _: &gpui_kit::component::input::InputEvent, cx| cx.notify(),
        )
        .detach();
        cx.subscribe_in(&model, window, |this, _, editor: &Editor, window, cx| {
            this.edit_source = Some(editor.source.clone());
            this.edit_profile = editor.profile.clone();
            this.selected = Some(editor.source.guid.clone());
            let d = &editor.draft;
            set(&this.title, d.title.clone(), window, cx);
            set(&this.workspace, d.workspace_id.clone(), window, cx);
            set(&this.owner, d.owner.clone(), window, cx);
            set(&this.due, d.due.clone().unwrap_or_default(), window, cx);
            this.description
                .update(cx, |s, cx| s.set_value(d.description.clone(), window, cx));
            cx.notify();
        })
        .detach();
        cx.observe_in(&model, window, |this, _, window, cx| {
            let mode = this.model.read(cx).mode;
            if this.display_mode != mode {
                this.display_mode = mode;
                if this.filter == Filter::Settings {
                    this.open_settings(window, cx);
                }
            }
            if !this.edit_profile.is_empty() && this.edit_profile != this.model.read(cx).profile {
                this.selected = None;
                this.edit_source = None;
                this.edit_profile.clear();
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
        let display_mode = model.read(cx).mode;
        Self {
            model,
            display_mode,
            filter: Filter::Pending,
            selected: None,
            edit_source: None,
            edit_profile: String::new(),
            clear_confirmation: false,
            search,
            title: cx.new(|cx| InputState::new(window, cx)),
            description: cx.new(|cx| TextareaState::new(window, cx)),
            owner: cx.new(|cx| InputState::new(window, cx)),
            workspace: cx.new(|cx| InputState::new(window, cx)),
            settings_workspace: cx.new(|cx| InputState::new(window, cx)),
            settings_owner: cx.new(|cx| InputState::new(window, cx)),
            due: cx.new(|cx| InputState::new(window, cx).placeholder("YYYY-MM-DD")),
            poll: cx.new(|cx| InputState::new(window, cx)),
            lark_path: cx.new(|cx| InputState::new(window, cx)),
            tapd_path: cx.new(|cx| InputState::new(window, cx)),
            endpoint: cx.new(|cx| InputState::new(window, cx)),
            app_id: cx.new(|cx| InputState::new(window, cx)),
            app_secret: cx.new(|cx| InputState::new(window, cx).masked(true)),
            lark_token: cx.new(|cx| InputState::new(window, cx).masked(true)),
            tapd_token: cx.new(|cx| InputState::new(window, cx).masked(true)),
            recover_id: cx.new(|cx| InputState::new(window, cx)),
            recover_kind: "story".into(),
            pending_seen: 0,
        }
    }
    fn tr<'a>(&self, cx: &App, zh: &'a str, en: &'a str) -> &'a str {
        text(self.model.read(cx).language(), zh, en)
    }
    fn send(&self, job: Job, cx: &mut Context<Self>) {
        self.model.update(cx, |model, cx| model.send(job, cx));
    }
    fn send_editor(&self, job: Job, cx: &mut Context<Self>) {
        self.model
            .update(cx, |m, cx| m.send_at(job, self.edit_profile.clone(), cx));
    }
    fn draft(&self, cx: &App) -> Draft {
        Draft {
            title: self.title.read(cx).value().to_string(),
            description: self.description.read(cx).value().to_string(),
            workspace_id: self.workspace.read(cx).value().trim().into(),
            owner: self.owner.read(cx).value().trim().into(),
            due: nonempty(self.due.read(cx).value().as_ref()),
        }
    }
    fn select(&mut self, record: SyncRecord, window: &mut Window, cx: &mut Context<Self>) {
        if self.model.read(cx).busy {
            return;
        }
        for input in [&self.title, &self.workspace, &self.owner, &self.due] {
            set(input, String::new(), window, cx);
        }
        self.description
            .update(cx, |s, cx| s.set_value(String::new(), window, cx));
        self.selected = Some(record.source.guid.clone());
        self.edit_source = None;
        self.edit_profile = self.model.read(cx).profile.clone();
        self.model.update(cx, |m, cx| {
            m.preview = None;
            cx.notify();
        });
        self.send(Job::OpenDraft(record.source.guid, false), cx);
        cx.notify();
    }
    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter = Filter::Settings;
        if let Some(snapshot) = &self.model.read(cx).snapshot {
            let s = snapshot.settings.clone();
            for (field, value) in [
                (&self.settings_workspace, s.workspace_id),
                (&self.settings_owner, s.owner),
                (&self.poll, s.poll_minutes.to_string()),
                (&self.lark_path, s.lark_path),
                (&self.tapd_path, s.tapd_path),
                (&self.endpoint, s.tapd_endpoint),
            ] {
                set(field, value, window, cx);
            }
        }
        cx.notify();
    }
    fn settings(&self, cx: &App) -> Option<Settings> {
        let mut s = self.model.read(cx).snapshot.as_ref()?.settings.clone();
        s.workspace_id = self.settings_workspace.read(cx).value().trim().into();
        s.owner = self.settings_owner.read(cx).value().trim().into();
        s.poll_minutes = self.poll.read(cx).value().parse().unwrap_or(0);
        s.lark_path = self.lark_path.read(cx).value().trim().into();
        s.tapd_path = self.tapd_path.read(cx).value().trim().into();
        s.tapd_endpoint = self.endpoint.read(cx).value().trim().into();
        Some(s)
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
                                    .label(if busy {
                                        self.tr(cx, "处理中…", "Working…")
                                    } else {
                                        self.tr(cx, "立即检查", "Check now")
                                    })
                                    .primary()
                                    .disabled(busy)
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
fn nonempty(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() { None } else { Some(s.into()) }
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
        "editor_ready" => text(language, "已读取草稿", "Draft loaded"),
        "mode_changed" => text(
            language,
            "模式已切换，配置与缓存已隔离加载",
            "Mode changed. Isolated settings and cache loaded.",
        ),
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
        "preview_ready" => text(
            language,
            "预览已就绪，尚未写入 TAPD",
            "Preview ready. Nothing written to TAPD yet.",
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
