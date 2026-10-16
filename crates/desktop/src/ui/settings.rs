//! 设置按账号、导入、后台检查和外观分类；连接表单只按需展示。
use super::*;

impl Workspace {
    pub(super) fn install_confirmation(
        &self,
        plan: InstallationPlan,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let busy = self.model.read(cx).busy;
        let credential_disabled = busy;
        div().p_4().rounded_md().bg(cx.theme().accent).flex().flex_col().gap_2()
                .child(self.tr(cx,"确认安装到应用私有目录","Confirm app-local installation"))
                .child(format!("{}: {}",self.tr(cx,"来源","Source"),plan.source))
                .child(format!("{}: {}",self.tr(cx,"版本","Version"),plan.version))
                .child(format!("{}: {}",self.tr(cx,"目标","Destination"),plan.destination.display()))
                .child(format!("{}: {}",self.tr(cx,"前提与校验说明","Requirements and verification"),plan.requirements))
                .child(self.tr(cx,"不会修改 shell 配置或使用 sudo。TAPD 发布物未提供加密校验值，安装信任官方 HTTPS 来源。","No shell profile changes or sudo. TAPD publishes no cryptographic checksum; installation trusts the official HTTPS source."))
                .child(div().flex().gap_3().child(self.focus_region("confirm-install", Button::new("confirm-install").primary().label(self.tr(cx,"确认安装","Install now")).disabled(credential_disabled).on_click(cx.listener(move |this,_,_,cx|this.send(Job::Install(plan.tool),cx))))).child(self.focus_region("cancel-install", Button::new("cancel-install").label(self.tr(cx,"取消","Cancel")).disabled(busy).on_click(cx.listener(|this,_,_,cx|this.send(Job::CancelInstall,cx))))))
    }
    pub(super) fn settings_view(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.model.read(cx);
        let configured = model.tapd_configured();
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .px_6()
                    .py_4()
                    .text_2xl()
                    .font_semibold()
                    .child(self.tr(cx, "设置中心", "Settings")),
            )
            .child(
                div()
                    .id("settings-scroll")
                    .track_scroll(&self.settings_scroll)
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_6()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .child(section(self.tr(cx, "账号连接", "Account connections")))
                    .child(self.field_feedback("connection-status", cx))
                    .when(model.errors.contains_key("connection-status"), |d| {
                        d.child(
                            self.focus_region(
                                "reload-connections",
                                Button::new("reload-connections")
                                    .label(self.tr(
                                        cx,
                                        "重新读取连接信息",
                                        "Retry loading connections",
                                    ))
                                    .disabled(model.activities.contains("connection-status"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.send(Job::LoadConnectionStatus, cx)
                                    })),
                            ),
                        )
                    })
                    .child(self.connection_card(CliTool::Lark, cx))
                    .child(self.connection_card(CliTool::Tapd, cx))
                    .child(section(self.tr(cx, "导入设置", "Import settings")))
                    .when(configured, |d| d.child(self.destination_settings(cx)))
                    .when(!configured, |d| {
                        d.child(section(self.tr(cx, "默认导入目标", "Default destination")))
                            .child(self.tapd_required("configure-tapd-destination", cx))
                    })
                    .when(configured, |d| d.child(self.prefix_settings(cx)))
                    .when(!configured, |d| {
                        d.child(section(self.tr(cx, "项目空间前缀", "Workspace prefixes")))
                            .child(self.tapd_required("configure-tapd-prefixes", cx))
                    })
                    .child(self.background_settings(cx))
                    .child(self.appearance_settings(cx)),
            )
    }
    fn destination_settings(&self, cx: &Context<Self>) -> impl IntoElement {
        let model = self.model.read(cx);
        let busy = model.busy;
        let candidate = TapdWorkspace {
            id: self.manual_workspace_id.read(cx).value().trim().into(),
            name: self.manual_workspace_name.read(cx).value().trim().into(),
        };
        // 权限检查结果必须匹配当前输入；修改后不得沿用旧确认。
        let workspace_checked = model.checked_workspace.as_ref() == Some(&candidate);
        div().flex().flex_col().gap_3()
            .child(div().flex().items_center().justify_between()
                .child(section(self.tr(cx,"默认导入目标","Default import destination")))
                .child(self.focus_region("add-workspace", Button::new("add-workspace").icon(gpui_kit::component::IconName::Plus).ghost()
                    .accessibility_label(self.tr(cx,"添加目标项目空间","Add TAPD workspace")).tooltip(self.tr(cx,"添加目标项目空间","Add TAPD workspace"))
                    .disabled(self.new_workspace_open).on_click(cx.listener(|this,_,_,cx|{this.new_workspace_open=true;cx.notify();})))))
            .child(div().p_4().border_1().border_color(cx.theme().border).rounded_md().flex().flex_col().gap_3()
                .child(self.focus_region("destination-workspace-field", field(self.tr(cx,"目标项目空间","TAPD workspace"),div().flex().items_center().gap_2()
                    .child(div().flex_1().min_w_0().child(Select::new(&self.settings_workspace).id("settings-workspace").placeholder(self.tr(cx,"选择目标项目空间","Select TAPD workspace")).search_placeholder(self.tr(cx,"搜索目标项目空间","Search TAPD workspaces"))))
                    .child(self.focus_region("settings-project-refresh", Button::new("settings-project-refresh").icon(gpui_kit::component::IconName::RefreshCw).accessibility_label(self.tr(cx,"刷新目标项目空间","Refresh TAPD workspaces")).tooltip(self.tr(cx,"刷新目标项目空间","Refresh TAPD workspaces")).disabled(busy).on_click(cx.listener(|this,_,_,cx|this.send(Job::LoadProjects,cx))))))))
                .child(self.focus_region("destination-owner-field", field(self.tr(cx,"默认处理人","Default assignee"),div().flex().items_center().gap_2()
                    .child(div().flex_1().min_w_0().child(Select::new(&self.settings_owner).id("settings-owner").placeholder(self.tr(cx,"选择处理人","Select assignee")).search_placeholder(self.tr(cx,"搜索处理人","Search assignees")).cleanable(true)))
                    .child(self.focus_region("settings-member-refresh", Button::new("settings-member-refresh").icon(gpui_kit::component::IconName::RefreshCw).accessibility_label(self.tr(cx,"刷新处理人","Refresh assignees")).tooltip(self.tr(cx,"刷新处理人","Refresh assignees")).disabled(busy||selected(&self.settings_workspace,cx).is_empty()).on_click(cx.listener(|this,_,_,cx|this.send(Job::LoadMembers(selected(&this.settings_workspace,cx)),cx))))))))
                .child(self.field_feedback("destination",cx)))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(self.tr(cx,"CLI 仅列出管理员的目标项目空间。其他空间可按 ID 与名称添加；检查只验证权限，确认后才加入缓存。","The CLI lists administrator workspaces only. Add other workspaces by ID and name; checking only verifies access, and confirmation saves the workspace.")))
            .when(self.new_workspace_open,|d|d.child(div().p_4().border_1().border_color(cx.theme().border).rounded_md().flex().flex_col().gap_3()
                .child(self.help_field(self.tr(cx,"目标项目空间 ID","TAPD workspace ID"),"workspace-id-help",self.tr(cx,"从 TAPD 项目链接获取空间 ID。先检查权限，再确认加入列表。","Find the workspace ID in its TAPD project URL. Check access before confirming the addition."),Some("https://www.tapd.cn"),Input::new(&self.manual_workspace_id).id("manual-project-id"),cx))
                .child(self.focus_region("workspace-name-field", field(self.tr(cx,"目标项目空间名称","TAPD workspace name"),Input::new(&self.manual_workspace_name).id("manual-project-name"))))
                .child(self.field_feedback("new-workspace",cx))
                .when(workspace_checked,|d|d.child(div().text_sm().child(self.tr(cx,"检查通过；确认后加入目标项目空间列表。","Check passed. Confirm to add this workspace."))))
                .child(div().flex().gap_3()
                    .child(self.focus_region("check-workspace", Button::new("check-workspace").label(self.tr(cx,"检查权限","Check access")).on_click(cx.listener(|this,_,_,cx|this.send(Job::CheckWorkspace(TapdWorkspace{id:this.manual_workspace_id.read(cx).value().trim().into(),name:this.manual_workspace_name.read(cx).value().trim().into()}),cx)))))
                    .child(self.focus_region("confirm-workspace", Button::new("confirm-workspace").primary().label(self.tr(cx,"确认添加","Confirm add")).disabled(busy||!workspace_checked).on_click(cx.listener(|this,_,_,cx|this.send(Job::AddWorkspace(TapdWorkspace{id:this.manual_workspace_id.read(cx).value().trim().into(),name:this.manual_workspace_name.read(cx).value().trim().into()}),cx)))))
                    .child(self.focus_region("cancel-workspace", Button::new("cancel-workspace").label(self.tr(cx,"取消","Cancel")).on_click(cx.listener(|this,_,_,cx|{this.new_workspace_open=false;cx.notify();})))))))
            .when(!self.new_workspace_open, |d| d.child(self.field_feedback("new-workspace",cx)))
    }
    fn prefix_settings(&self, cx: &Context<Self>) -> impl IntoElement {
        let model = self.model.read(cx);
        let busy = model.busy;
        let prefix_workspace = selected(&self.prefix_workspace, cx);
        let prefixes = model
            .snapshot
            .as_ref()
            .and_then(|s| s.settings.workspace_prefixes.get(&prefix_workspace))
            .cloned()
            .unwrap_or_default();
        div().flex().flex_col().gap_3()
            .child(section(self.tr(cx,"目标项目空间前缀","Workspace prefixes")))
            .child(div().p_4().border_1().border_color(cx.theme().border).rounded_md().flex().flex_col().gap_3()
                .child(self.focus_region("prefix-workspace-field", field(self.tr(cx,"目标项目空间","TAPD workspace"),Select::new(&self.prefix_workspace).id("prefix-workspace").placeholder(self.tr(cx,"选择目标项目空间","Select TAPD workspace")).search_placeholder(self.tr(cx,"搜索目标项目空间","Search TAPD workspaces")))))
                // 前缀逐项保存原文；删除和添加都经后台验证后更新列表。
                .children(prefixes.into_iter().enumerate().map(|(index,prefix)|div().id(SharedString::from(format!("prefix-row-{index}"))).flex().items_center().gap_3()
                    .child(div().flex_1().child(prefix.clone()))
                    .child(self.focus_region(SharedString::from(format!("remove-prefix-{index}")), Button::new(SharedString::from(format!("remove-prefix-{index}"))).ghost().label(self.tr(cx,"删除","Remove")).disabled(busy).on_click(cx.listener(move |this,_,_,cx|{
                        let workspace=selected(&this.prefix_workspace,cx);
                        this.preference(Preference::RemoveWorkspacePrefix{workspace,prefix:prefix.clone()},cx);
                    }))))))
                .when(!self.new_prefix_open,|d|d.child(self.field_feedback("prefixes",cx)))
                .child(self.focus_region("add-prefix", Button::new("add-prefix").label(self.tr(cx,"添加前缀","Add prefix")).disabled(busy||prefix_workspace.is_empty()||self.new_prefix_open).on_click(cx.listener(|this,_,window,cx|{
                    this.load_prefix_form(window,cx);this.new_prefix_open=true;this.prefix_options.read(cx).focus_handle(cx).focus(window,cx);cx.notify();
                }))))
                .when(self.new_prefix_open,|d|d.child(div().flex().flex_col().gap_3()
                    .child(self.help_field(self.tr(cx,"前缀原文","Prefix text"),"prefix-help",self.tr(cx,"原样保存并拼接标题，不自动加括号或删除空格。最多 66 字符；不允许全空白或控制字符。","Saved and prepended verbatim, without adding brackets or trimming spaces. Up to 66 characters; no whitespace-only values or control characters."),None,Input::new(&self.prefix_options).id("workspace-prefixes").disabled(busy),cx))
                    .child(self.field_feedback("prefixes",cx))
                    .child(div().flex().gap_3()
                        .child(self.focus_region("confirm-prefixes", Button::new("confirm-prefixes").primary().label(self.tr(cx,"确认添加","Confirm add")).disabled(busy).on_click(cx.listener(|this,_,_,cx|{
                            let workspace=selected(&this.prefix_workspace,cx);
                            let prefix=this.prefix_options.read(cx).value().to_string();
                            this.preference(Preference::AddWorkspacePrefix{workspace,prefix},cx);
                        }))))
                        .child(self.focus_region("cancel-prefix", Button::new("cancel-prefix").label(self.tr(cx,"取消","Cancel")).disabled(busy).on_click(cx.listener(|this,_,window,cx|this.load_prefix_form(window,cx)))))))))
    }
    fn background_settings(&self, cx: &Context<Self>) -> impl IntoElement {
        let model = self.model.read(cx);
        let busy = model.busy;
        let settings = model
            .snapshot
            .as_ref()
            .map(|s| s.settings.clone())
            .unwrap_or_default();
        div().flex().flex_col().gap_3()
            .child(section(self.tr(cx,"后台检查","Background checks")))
            .child(self.focus_region("poll-toggle-field", Switch::new("toggle-poll").label(self.tr(cx,"定时检查新任务","Check for new tasks periodically")).checked(settings.poll_enabled).disabled(busy).on_click(cx.listener(|this,enabled,_,cx|this.preference(Preference::Polling(*enabled),cx)))))
            .child(self.field_feedback("poll-enabled",cx))
            .when(settings.poll_enabled,|d|d.child(self.focus_region("poll-interval-field", field(self.tr(cx,"检查间隔（1–1440 分钟）","Interval (1–1440 minutes)"),Input::new(&self.poll).id("poll-minutes")))).child(self.field_feedback("poll",cx)))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(self.tr(cx,"关闭窗口后仍可检查；退出应用后停止。首次读取只建立基线，不发送历史任务通知。","Checks continue when the window closes, but stop when the app quits. Initial fetch establishes a baseline without notifying historical tasks.")))
    }
    fn appearance_settings(&self, cx: &Context<Self>) -> impl IntoElement {
        let model = self.model.read(cx);
        let busy = model.busy;
        let language = model.language().to_owned();
        let settings = model
            .snapshot
            .as_ref()
            .map(|s| s.settings.clone())
            .unwrap_or_default();
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(section(self.tr(
                cx,
                "外观与语言",
                "Appearance and language",
            )))
            .child(
                self.focus_region(
                    "language-options",
                    div().flex().gap_3().children(
                        [("zh-CN", "中文"), ("en", "English")].into_iter().map(
                            |(locale, label)| {
                                Button::new(locale)
                                    .label(label)
                                    .ghost()
                                    .when(language == locale, |b| b.primary())
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.preference(Preference::Language(locale.into()), cx);
                                    }))
                            },
                        ),
                    ),
                ),
            )
            .child(self.field_feedback("language", cx))
            .child(
                self.focus_region(
                    "theme-options",
                    div().flex().gap_3().children(
                        [
                            ("system", "跟随系统", "System"),
                            ("light", "亮色", "Light"),
                            ("dark", "暗色", "Dark"),
                        ]
                        .into_iter()
                        .map(|(mode, zh, en)| {
                            Button::new(mode)
                                .label(text(&language, zh, en))
                                .ghost()
                                .when(settings.theme == mode, |b| b.primary())
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.preference(Preference::Theme(mode.into()), cx);
                                }))
                        }),
                    ),
                ),
            )
            .child(self.field_feedback("theme", cx))
    }
    pub(super) fn help_field(
        &self,
        label: &str,
        id: &'static str,
        help: &str,
        url: Option<&'static str>,
        input: impl IntoElement,
        cx: &App,
    ) -> impl IntoElement {
        self.focus_region(
            id,
            help::field(label, id, help, url, input, self.model.read(cx).language()),
        )
    }
}
