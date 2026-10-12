//! 配置中心不直接访问凭据和数据库；所有操作交给后台队列。
use super::*;

impl Workspace {
    fn install_confirmation(&self, plan: InstallationPlan, cx: &Context<Self>) -> impl IntoElement {
        let busy = self.model.read(cx).busy;
        let credential_disabled = busy || self.model.read(cx).mode == Mode::Mock;
        div().p_4().rounded_md().bg(cx.theme().accent).flex().flex_col().gap_2()
                .child(self.tr(cx,"确认安装到应用私有目录","Confirm app-local installation"))
                .child(format!("{}: {}",self.tr(cx,"来源","Source"),plan.source))
                .child(format!("{}: {}",self.tr(cx,"版本","Version"),plan.version))
                .child(format!("{}: {}",self.tr(cx,"目标","Destination"),plan.destination.display()))
                .child(format!("{}: {}",self.tr(cx,"前提与校验说明","Requirements and verification"),plan.requirements))
                .child(self.tr(cx,"不会修改 shell 配置或使用 sudo。TAPD 发布物未提供加密校验值，安装信任官方 HTTPS 来源。","No shell profile changes or sudo. TAPD publishes no cryptographic checksum; installation trusts the official HTTPS source."))
                .child(div().flex().gap_3().child(Button::new("confirm-install").primary().label(self.tr(cx,"确认安装","Install now")).disabled(credential_disabled).on_click(cx.listener(move |this,_,_,cx|this.send(Job::Install(plan.tool),cx)))).child(Button::new("cancel-install").label(self.tr(cx,"取消","Cancel")).disabled(busy).on_click(cx.listener(|this,_,_,cx|this.send(Job::CancelInstall,cx)))))
    }
    pub(super) fn settings_view(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.model.read(cx);
        let busy = model.busy;
        let settings = model
            .snapshot
            .as_ref()
            .map(|s| s.settings.clone())
            .unwrap_or_default();
        let language = model.language().to_owned();
        let mock = model.mode == Mode::Mock;
        let credential_disabled = busy || mock;
        let candidate = TapdWorkspace {
            id: self.manual_workspace_id.read(cx).value().trim().into(),
            name: self.manual_workspace_name.read(cx).value().trim().into(),
        };
        // 检查结果必须对应当前草稿；修改任一字段后必须重新检查。
        let workspace_checked = model.checked_workspace.as_ref() == Some(&candidate);
        let lark_detected = std::path::Path::new(&settings.lark_path).is_file();
        let tapd_detected = std::path::Path::new(&settings.tapd_path).is_file();
        let prefix_workspace = selected(&self.prefix_workspace, cx);
        let prefixes = settings
            .workspace_prefixes
            .get(&prefix_workspace)
            .cloned()
            .unwrap_or_default();
        div().flex_1().min_w_0().h_full().flex().flex_col().text_color(cx.theme().foreground)
            .child(div().px_6().py_4().text_2xl().font_semibold().child(self.tr(cx,"配置中心","Settings")))
            .child(div().id("settings-scroll").flex_1().min_w_0().min_h_0().overflow_y_scroll().p_6().flex().flex_col().gap_5()

            .child(div().text_color(cx.theme().muted_foreground).child(self.tr(cx,"连接账号、设置导入目标，并控制后台检查。","Connect accounts, choose a destination, and control background checks.")))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(if model.mode==Mode::Mock{self.tr(cx,"当前为演示模式；由 LARK2TAPD_MOCK=1 启动。不会执行真实连接、安装或凭证操作；测试按钮会说明此模式边界。","Demo mode started by LARK2TAPD_MOCK=1. No real connection, installation or credential operation is performed; test buttons explain this boundary.")}else{self.tr(cx,"当前为真实模式。LARK2TAPD_MOCK=1 可在下次启动时启用演示。","Live mode. Set LARK2TAPD_MOCK=1 on the next launch for a demo.")}))
            .child(section(self.tr(cx,"外观与语言","Appearance and language")))
            .child(div().flex().gap_3().children([("zh-CN","中文"),("en","English")].into_iter().map(|(locale,label)|Button::new(locale).label(label).ghost().when(language==locale,|b|b.primary()).disabled(busy).on_click(cx.listener(move |this,_,_,cx| {this.preference(Preference::Language(locale.into()),cx);})))))
            .child(div().flex().gap_3().children([("system","跟随系统","System"),("light","亮色","Light"),("dark","暗色","Dark")].into_iter().map(|(mode,zh,en)|Button::new(mode).label(text(&language,zh,en)).ghost().when(settings.theme==mode,|b|b.primary()).disabled(busy).on_click(cx.listener(move |this,_,_,cx|{this.preference(Preference::Theme(mode.into()),cx);})))))
            // 服务专属路径、诊断、安装和凭证紧邻展示；保存仍使用统一 Keychain 边界。
            .child(div().p_4().border_1().border_color(cx.theme().border).rounded_md().flex().flex_col().gap_3()
            .child(section(self.tr(cx,"飞书","Lark")))
            .when_some(model.install_plan.clone().filter(|plan|plan.tool==CliTool::Lark),|d,plan|d.child(self.install_confirmation(plan,cx)))
            .child(help_field(self.tr(cx,"飞书 CLI 可执行文件","Lark CLI executable"),"lark-cli-help",self.tr(cx,"应用自动检测路径；点击查看飞书 CLI 文档。","Path detected by the app. Open Lark CLI documentation."),Some("https://github.com/larksuite/cli"),Input::new(&self.lark_path).id("lark-path").disabled(true)))
            .child(div().flex().gap_3()
                .child(Button::new("check-lark").label(self.tr(cx,"测试飞书 CLI 连接","Test Lark CLI connection")).on_click(cx.listener(|this,_,_,cx|this.send(Job::Check(CliTool::Lark),cx))))
                .when(!lark_detected,|d|d.child(Button::new("install-lark").label(self.tr(cx,"安装缺失的飞书 CLI","Install missing Lark CLI")).disabled(credential_disabled).on_click(cx.listener(|this,_,_,cx|this.send(Job::PlanInstall(CliTool::Lark),cx))))))
            .when(!lark_detected,|d|d.child(div().text_xs().text_color(cx.theme().muted_foreground).child(self.tr(cx,"未检测到可执行文件。安装前可查看来源、目标与校验说明。","Executable not detected. Review the source, destination and verification details before installation."))))
            .child(div().text_sm().child(model.lark_diagnostic.clone()))
            .child(help_field("Lark App ID","lark-app-id-help",self.tr(cx,"在飞书开发者后台创建应用并获取 App ID。","Create an app in the Lark developer console to get its App ID."),Some("https://open.feishu.cn/app"),Input::new(&self.app_id).id("lark-app-id").disabled(credential_disabled)))
            .child(help_field("Lark App Secret","lark-secret-help",self.tr(cx,"从飞书开发者后台获取应用密钥；仅保存到本机 Keychain。","Get the app secret from the developer console; stored only in this Mac’s Keychain."),Some("https://open.feishu.cn/app"),Input::new(&self.app_secret).id("lark-app-secret").disabled(credential_disabled)))
            .child(help_field(self.tr(cx,"飞书用户 Token（可选，或使用 OAuth）","Lark user token (optional, or use OAuth)"),"lark-token-help",self.tr(cx,"手工 Token 仅可保存；当前同步请使用 OAuth，以校验账号身份。","Manual tokens can be saved, but syncing currently requires OAuth to verify account identity."),Some("https://open.feishu.cn/document/authentication-management/access-token/get-user-access-token"),Input::new(&self.lark_token).id("lark-user-token").disabled(credential_disabled)))
            .child(Button::new("lark-oauth").label(self.tr(cx,"连接飞书（OAuth）","Connect Lark (OAuth)")).disabled(credential_disabled).on_click(cx.listener(|this,_,_,cx|this.send(Job::Login,cx))))
            )
            .child(div().p_4().border_1().border_color(cx.theme().border).rounded_md().flex().flex_col().gap_3()
            .child(section("TAPD"))
            .when_some(model.install_plan.clone().filter(|plan|plan.tool==CliTool::Tapd),|d,plan|d.child(self.install_confirmation(plan,cx)))
            .child(help_field(self.tr(cx,"TAPD CLI 可执行文件","TAPD CLI executable"),"tapd-cli-help",self.tr(cx,"应用自动检测路径；点击查看 TAPD CLI 文档。","Path detected by the app. Open TAPD CLI documentation."),Some("https://cnb.cool/tapd.cn/skills/tapd-cli"),Input::new(&self.tapd_path).id("tapd-path").disabled(true)))
            .child(div().flex().gap_3()
                .child(Button::new("check-tapd").label(self.tr(cx,"测试 TAPD CLI 连接","Test TAPD CLI connection")).on_click(cx.listener(|this,_,_,cx|this.send(Job::Check(CliTool::Tapd),cx))))
                .when(!tapd_detected,|d|d.child(Button::new("install-tapd").label(self.tr(cx,"安装缺失的 TAPD CLI","Install missing TAPD CLI")).disabled(credential_disabled).on_click(cx.listener(|this,_,_,cx|this.send(Job::PlanInstall(CliTool::Tapd),cx))))))
            .when(!tapd_detected,|d|d.child(div().text_xs().text_color(cx.theme().muted_foreground).child(self.tr(cx,"未检测到可执行文件。安装前可查看来源、目标与校验说明。","Executable not detected. Review the source, destination and verification details before installation."))))
            .child(div().text_sm().child(model.tapd_diagnostic.clone()))
            .child(help_field("TAPD Token","tapd-token-help",self.tr(cx,"在 TAPD 账号设置中获取 API Token；仅保存到本机 Keychain。","Get an API token from TAPD account settings; stored only in this Mac’s Keychain."),Some("https://www.tapd.cn"),Input::new(&self.tapd_token).id("tapd-token").disabled(credential_disabled)))
            )
            .child(section(self.tr(cx,"凭证与授权","Credentials and authorization")))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(self.tr(cx,"密钥仅保存在此 Mac 的 Keychain。留空表示保留已有值，成功保存后清空输入框。请确认凭证后再连接。","Secrets stay in this Mac’s Keychain. Empty fields preserve existing values; fields clear only after a successful save. Confirm credentials before connecting.")))
            .child(self.inline_error("credentials",cx))
            .child(div().flex().gap_3().child(Button::new("save-credentials").label(self.tr(cx,"确认并保存到 Keychain","Confirm and save to Keychain")).primary().disabled(credential_disabled).on_click(cx.listener(|this,_,_,cx|{
                let credentials=Credentials {lark_app_id:this.app_id.read(cx).value().to_string(),lark_app_secret:this.app_secret.read(cx).value().to_string(),lark_user_token:this.lark_token.read(cx).value().to_string(),tapd_token:this.tapd_token.read(cx).value().to_string()};
                this.send(Job::SaveCredentials(credentials),cx);
            })))
                )
            .child(Button::new("clear-credentials").label(if self.clear_confirmation { self.tr(cx,"再次点击：清除所有保存的凭证","Click again: clear all saved credentials") } else { self.tr(cx,"清除凭证并重新连接","Clear credentials and reconnect") }).ghost().disabled(credential_disabled).on_click(cx.listener(|this,_,_,cx| {if this.clear_confirmation {this.clear_confirmation=false;this.send(Job::ClearCredentials,cx);}else{this.clear_confirmation=true;cx.notify();}})))
            .child(section(self.tr(cx,"默认导入目标","Default import destination")))
            .child(div().p_4().border_1().border_color(cx.theme().border).rounded_md().flex().flex_col().gap_3()
                .child(field(self.tr(cx,"目标项目空间","TAPD workspace"),Select::new(&self.settings_workspace).id("settings-workspace").placeholder(self.tr(cx,"选择目标项目空间","Select TAPD workspace")).search_placeholder(self.tr(cx,"搜索目标项目空间","Search TAPD workspaces"))))
                .child(field(self.tr(cx,"默认处理人","Default assignee"),Select::new(&self.settings_owner).id("settings-owner").placeholder(self.tr(cx,"选择处理人","Select assignee")).search_placeholder(self.tr(cx,"搜索处理人","Search assignees")).cleanable(true)))
                .child(self.inline_error("destination",cx))
                .child(div().flex().gap_3()
                    .child(Button::new("confirm-destination").primary().label(self.tr(cx,"确认默认目标","Confirm destination")).disabled(busy).on_click(cx.listener(|this,_,_,cx|this.preference(Preference::DefaultDestination{workspace:selected(&this.settings_workspace,cx),owner:selected(&this.settings_owner,cx)},cx))))
                    .child(Button::new("settings-project-refresh").label(self.tr(cx,"刷新目标项目空间","Refresh TAPD workspaces")).on_click(cx.listener(|this,_,_,cx|this.send(Job::LoadProjects,cx))))
                    .child(Button::new("settings-member-refresh").label(self.tr(cx,"刷新处理人","Refresh assignees")).disabled(selected(&self.settings_workspace,cx).is_empty()).on_click(cx.listener(|this,_,_,cx|this.send(Job::LoadMembers(selected(&this.settings_workspace,cx)),cx))))))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(self.tr(cx,"CLI 仅列出管理员的目标项目空间。其他空间可按 ID 与名称添加；检查只验证权限，确认后才加入缓存。","The CLI lists administrator workspaces only. Add other workspaces by ID and name; checking only verifies access, and confirmation saves the workspace.")))
            .child(Button::new("add-workspace").label(self.tr(cx,"添加目标项目空间","Add TAPD workspace")).disabled(self.new_workspace_open).on_click(cx.listener(|this,_,_,cx|{this.new_workspace_open=true;cx.notify();})))
            .when(self.new_workspace_open,|d|d.child(div().p_4().border_1().border_color(cx.theme().border).rounded_md().flex().flex_col().gap_3()
                .child(help_field(self.tr(cx,"目标项目空间 ID","TAPD workspace ID"),"workspace-id-help",self.tr(cx,"从 TAPD 项目链接获取空间 ID。先检查权限，再确认加入列表。","Find the workspace ID in its TAPD project URL. Check access before confirming the addition."),Some("https://www.tapd.cn"),Input::new(&self.manual_workspace_id).id("manual-project-id")))
                .child(field(self.tr(cx,"目标项目空间名称","TAPD workspace name"),Input::new(&self.manual_workspace_name).id("manual-project-name")))
                .child(self.inline_error("new-workspace",cx))
                .when(workspace_checked,|d|d.child(div().text_sm().child(self.tr(cx,"检查通过；确认后加入目标项目空间列表。","Check passed. Confirm to add this workspace."))))
                .child(div().flex().gap_3()
                    .child(Button::new("check-workspace").label(self.tr(cx,"检查权限","Check access")).on_click(cx.listener(|this,_,_,cx|this.send(Job::CheckWorkspace(TapdWorkspace{id:this.manual_workspace_id.read(cx).value().trim().into(),name:this.manual_workspace_name.read(cx).value().trim().into()}),cx))))
                    .child(Button::new("confirm-workspace").primary().label(self.tr(cx,"确认添加","Confirm add")).disabled(busy||!workspace_checked).on_click(cx.listener(|this,_,_,cx|this.send(Job::AddWorkspace(TapdWorkspace{id:this.manual_workspace_id.read(cx).value().trim().into(),name:this.manual_workspace_name.read(cx).value().trim().into()}),cx))))
                    .child(Button::new("cancel-workspace").label(self.tr(cx,"取消","Cancel")).on_click(cx.listener(|this,_,_,cx|{this.new_workspace_open=false;cx.notify();}))))))
            .child(section(self.tr(cx,"目标项目空间前缀","Workspace prefixes")))
            .child(div().p_4().border_1().border_color(cx.theme().border).rounded_md().flex().flex_col().gap_3()
                .child(field(self.tr(cx,"目标项目空间","TAPD workspace"),Select::new(&self.prefix_workspace).id("prefix-workspace").placeholder(self.tr(cx,"选择目标项目空间","Select TAPD workspace")).search_placeholder(self.tr(cx,"搜索目标项目空间","Search TAPD workspaces"))))
                // 前缀逐项保存原文；删除和添加都经后台验证后更新列表。
                .children(prefixes.into_iter().enumerate().map(|(index,prefix)|div().id(SharedString::from(format!("prefix-row-{index}"))).flex().items_center().gap_3()
                    .child(div().flex_1().child(prefix.clone()))
                    .child(Button::new(SharedString::from(format!("remove-prefix-{index}"))).ghost().label(self.tr(cx,"删除","Remove")).disabled(busy).on_click(cx.listener(move |this,_,_,cx|{
                        let workspace=selected(&this.prefix_workspace,cx);
                        this.preference(Preference::RemoveWorkspacePrefix{workspace,prefix:prefix.clone()},cx);
                    })))))
                .child(self.inline_error("prefixes",cx))
                .child(Button::new("add-prefix").label(self.tr(cx,"添加前缀","Add prefix")).disabled(busy||prefix_workspace.is_empty()||self.new_prefix_open).on_click(cx.listener(|this,_,window,cx|{
                    this.load_prefix_form(window,cx);this.new_prefix_open=true;this.prefix_options.read(cx).focus_handle(cx).focus(window,cx);cx.notify();
                })))
                .when(self.new_prefix_open,|d|d.child(div().flex().flex_col().gap_3()
                    .child(help_field(self.tr(cx,"前缀原文","Prefix text"),"prefix-help",self.tr(cx,"原样保存并拼接标题，不自动加括号或删除空格。最多 66 字符；不允许全空白或控制字符。","Saved and prepended verbatim, without adding brackets or trimming spaces. Up to 66 characters; no whitespace-only values or control characters."),None,Input::new(&self.prefix_options).id("workspace-prefixes").disabled(busy)))
                    .child(div().flex().gap_3()
                        .child(Button::new("confirm-prefixes").primary().label(self.tr(cx,"确认添加","Confirm add")).disabled(busy).on_click(cx.listener(|this,_,_,cx|{
                            let workspace=selected(&this.prefix_workspace,cx);
                            let prefix=this.prefix_options.read(cx).value().to_string();
                            this.preference(Preference::AddWorkspacePrefix{workspace,prefix},cx);
                        })))
                        .child(Button::new("cancel-prefix").label(self.tr(cx,"取消","Cancel")).disabled(busy).on_click(cx.listener(|this,_,window,cx|this.load_prefix_form(window,cx))))))))
            .child(section(self.tr(cx,"后台检查","Background checks")))
            .child(Switch::new("toggle-poll").label(self.tr(cx,"定时检查新任务","Check for new tasks periodically")).checked(settings.poll_enabled).disabled(busy).on_click(cx.listener(|this,enabled,_,cx|this.preference(Preference::Polling(*enabled),cx))))
            .when(settings.poll_enabled,|d|d.child(field(self.tr(cx,"检查间隔（1–1440 分钟）","Interval (1–1440 minutes)"),Input::new(&self.poll).id("poll-minutes"))).child(self.inline_error("poll",cx)))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(self.tr(cx,"关闭窗口后仍可检查；退出应用后停止。首次读取只建立基线，不发送历史任务通知。","Checks continue when the window closes, but stop when the app quits. Initial fetch establishes a baseline without notifying historical tasks.")))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(self.tr(cx,"演示与真实账户的数据库隔离。启动变量决定模式，运行期间不能切换。TAPD CLI 当前使用 Token，不假设提供 OAuth。","Demo and live accounts use separate databases. The startup variable selects the mode; it cannot change while running. TAPD CLI currently uses tokens, not an assumed OAuth flow.")))
            )
    }
}

// 帮助入口靠近字段标签；说明用 tooltip，外部文档只由显式点击打开。
fn help_field(
    label: &str,
    id: &'static str,
    help: &str,
    url: Option<&'static str>,
    input: impl IntoElement,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_sm()
                .child(label.to_owned())
                .child(
                    Button::new(id)
                        .ghost()
                        .icon(gpui_kit::component::IconName::Info)
                        .accessibility_label(help.to_owned())
                        .tooltip(help.to_owned())
                        .on_click(move |_, _, cx| {
                            if let Some(url) = url {
                                cx.open_url(url);
                            }
                        }),
                ),
        )
        .child(input)
}
