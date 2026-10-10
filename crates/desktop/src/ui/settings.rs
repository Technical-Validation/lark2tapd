//! 配置中心不直接访问凭据和数据库；所有操作交给后台队列。
use super::*;

impl Workspace {
    pub(super) fn settings_view(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.model.read(cx);
        let busy = model.busy;
        let settings = model
            .snapshot
            .as_ref()
            .map(|s| s.settings.clone())
            .unwrap_or_default();
        let language = model.language().to_owned();
        let credential_disabled = busy || model.mode == Mode::Mock;
        div().flex_1().min_w_0().h_full().flex().flex_col()
            .child(div().px_6().py_4().flex().flex_shrink_0().min_w_0().items_center().justify_between().child(div().text_2xl().font_semibold().child(self.tr(cx,"配置中心","Settings"))).child(Button::new("save-settings").label(self.tr(cx,"保存配置","Save settings")).primary().disabled(busy).on_click(cx.listener(|this,_,_,cx|{if let Some(s)=this.settings(cx){this.send(Job::SaveSettings(s),cx);}}))))
            .child(div().id("settings-scroll").flex_1().min_w_0().min_h_0().overflow_y_scroll().p_6().flex().flex_col().gap_5()

            .child(div().text_color(cx.theme().muted_foreground).child(self.tr(cx,"连接账号、设置导入目标，并控制后台检查。","Connect accounts, choose a destination, and control background checks.")))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(if model.mode==Mode::Mock{self.tr(cx,"当前为演示模式；由 LARK2TAPD_MOCK=1 启动。真实连接、安装和凭证操作已禁用。","Demo mode started by LARK2TAPD_MOCK=1. Connections, installation and credentials are disabled.")}else{self.tr(cx,"当前为真实模式。LARK2TAPD_MOCK=1 可在下次启动时启用演示。","Live mode. Set LARK2TAPD_MOCK=1 on the next launch for a demo.")}))
            .when_some(model.install_plan.clone(),|d,plan|d.child(div().p_4().rounded_md().bg(cx.theme().accent).flex().flex_col().gap_2()
                .child(self.tr(cx,"确认安装到应用私有目录","Confirm app-local installation"))
                .child(format!("{}: {}",self.tr(cx,"来源","Source"),plan.source))
                .child(format!("{}: {}",self.tr(cx,"版本","Version"),plan.version))
                .child(format!("{}: {}",self.tr(cx,"目标","Destination"),plan.destination.display()))
                .child(format!("{}: {}",self.tr(cx,"前提与校验说明","Requirements and verification"),plan.requirements))
                .child(self.tr(cx,"不会修改 shell 配置或使用 sudo。TAPD 发布物未提供加密校验值，安装信任官方 HTTPS 来源。","No shell profile changes or sudo. TAPD publishes no cryptographic checksum; installation trusts the official HTTPS source."))
                .child(div().flex().gap_3().child(Button::new("confirm-install").primary().label(self.tr(cx,"确认安装","Install now")).disabled(credential_disabled).on_click(cx.listener(move |this,_,_,cx|this.send(Job::Install(plan.tool),cx)))).child(Button::new("cancel-install").label(self.tr(cx,"取消","Cancel")).disabled(busy).on_click(cx.listener(|this,_,_,cx|this.send(Job::CancelInstall,cx)))))))
            .child(section(self.tr(cx,"外观与语言","Appearance and language")))
            .child(div().flex().gap_3().children([("zh-CN","中文"),("en","English")].into_iter().map(|(locale,label)|Button::new(locale).label(label).ghost().when(language==locale,|b|b.primary()).disabled(busy).on_click(cx.listener(move |this,_,_,cx| {if let Some(mut s)=this.model.read(cx).snapshot.as_ref().map(|s|s.settings.clone()){s.language=locale.into();this.send(Job::SaveSettings(s),cx);}})))))
            .child(div().flex().gap_3().children([("system","跟随系统","System"),("light","亮色","Light"),("dark","暗色","Dark")].into_iter().map(|(mode,zh,en)|Button::new(mode).label(text(&language,zh,en)).ghost().when(settings.theme==mode,|b|b.primary()).disabled(busy).on_click(cx.listener(move |this,_,_,cx|{if let Some(mut s)=this.model.read(cx).snapshot.as_ref().map(|s|s.settings.clone()){s.theme=mode.into();this.send(Job::SaveSettings(s),cx);}})))))
            .child(section(self.tr(cx,"导入与后台检查","Import and background checks")))
            .child(div().flex().gap_4().child(field(self.tr(cx,"默认项目","Default project"),Select::new(&self.settings_workspace).id("settings-workspace").placeholder(self.tr(cx,"选择项目","Select project")).search_placeholder(self.tr(cx,"搜索项目","Search projects")).disabled(busy))).child(field(self.tr(cx,"默认处理人","Default assignee"),Select::new(&self.settings_owner).id("settings-owner").placeholder(self.tr(cx,"选择处理人","Select assignee")).search_placeholder(self.tr(cx,"搜索处理人","Search assignees")).cleanable(true).disabled(busy))))
            .child(div().flex().gap_3().child(Button::new("settings-project-refresh").label(self.tr(cx,"从 TAPD 刷新项目","Refresh projects from TAPD")).disabled(busy).on_click(cx.listener(|this,_,_,cx|this.send(Job::LoadProjects,cx)))).child(Button::new("settings-member-refresh").label(self.tr(cx,"刷新处理人","Refresh assignees")).disabled(busy||selected(&self.settings_workspace,cx).is_empty()).on_click(cx.listener(|this,_,_,cx|this.send(Job::LoadMembers(selected(&this.settings_workspace,cx)),cx)))))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(self.tr(cx,"CLI 仅枚举管理员项目。其他项目可在下方输入 ID 与名称，验证成员读取权限后加入下拉列表；缓存可离线使用。","The CLI lists administrator projects only. Add other projects by ID and label below after verifying member access. Cached options remain available offline.")))
            .child(div().flex().gap_3().child(field(self.tr(cx,"补充项目 ID","Additional project ID"),Input::new(&self.manual_workspace_id).id("manual-project-id"))).child(field(self.tr(cx,"项目显示名称","Project display name"),Input::new(&self.manual_workspace_name).id("manual-project-name"))))
            .child(Button::new("add-project").label(self.tr(cx,"验证并加入项目","Verify and add project")).disabled(busy).on_click(cx.listener(|this,_,_,cx|this.send(Job::AddWorkspace(TapdWorkspace{id:this.manual_workspace_id.read(cx).value().trim().into(),name:this.manual_workspace_name.read(cx).value().trim().into()}),cx))))
            .child(div().flex().gap_4().items_end().child(field(self.tr(cx,"检查间隔（1–1440 分钟）","Interval (1–1440 minutes)"),Input::new(&self.poll).id("poll-minutes"))).child(Button::new("toggle-poll").label(if settings.poll_enabled{self.tr(cx,"暂停检查","Pause checks")}else{self.tr(cx,"开启检查","Enable checks")}).ghost().disabled(busy).on_click(cx.listener(|this,_,_,cx|{if let Some(mut s)=this.settings(cx){s.poll_enabled = !s.poll_enabled;this.send(Job::SaveSettings(s),cx);}}))))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(self.tr(cx,"关闭窗口后仍可检查；退出应用后停止。首次读取只建立基线，不发送历史任务通知。","Checks continue when the window closes, but stop when the app quits. Initial fetch establishes a baseline without notifying historical tasks.")))
            .child(section(self.tr(cx,"CLI 连接","CLI connection")))
            .child(field(self.tr(cx,"飞书 CLI 可执行文件","Lark CLI executable"),Input::new(&self.lark_path).id("lark-path")))
            .child(div().flex().gap_3().child(Button::new("check-lark").label(self.tr(cx,"测试飞书 CLI 连接","Test Lark CLI connection")).disabled(credential_disabled).on_click(cx.listener(|this,_,_,cx|this.send(Job::Check(CliTool::Lark),cx)))).child(Button::new("install-lark").label(self.tr(cx,"安装缺失的飞书 CLI","Install missing Lark CLI")).disabled(credential_disabled||std::path::Path::new(&settings.lark_path).is_file()).on_click(cx.listener(|this,_,_,cx|this.send(Job::PlanInstall(CliTool::Lark),cx)))))
            .child(div().text_sm().child(model.lark_diagnostic.clone()))
            .child(field(self.tr(cx,"TAPD CLI 可执行文件","TAPD CLI executable"),Input::new(&self.tapd_path).id("tapd-path")))
            .child(div().flex().gap_3().child(Button::new("check-tapd").label(self.tr(cx,"测试 TAPD CLI 连接","Test TAPD CLI connection")).disabled(credential_disabled).on_click(cx.listener(|this,_,_,cx|this.send(Job::Check(CliTool::Tapd),cx)))).child(Button::new("install-tapd").label(self.tr(cx,"安装缺失的 TAPD CLI","Install missing TAPD CLI")).disabled(credential_disabled||std::path::Path::new(&settings.tapd_path).is_file()).on_click(cx.listener(|this,_,_,cx|this.send(Job::PlanInstall(CliTool::Tapd),cx)))))
            .child(div().text_sm().child(model.tapd_diagnostic.clone()))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(self.tr(cx,"请先保存路径及凭证，再分别测试。结果区区分可执行文件/版本与账号连接状态。","Save paths and credentials before testing. Each result distinguishes executable/version detection from account connectivity.")))
            .child(field(self.tr(cx,"TAPD API 地址（HTTPS）","TAPD API endpoint (HTTPS)"),Input::new(&self.endpoint).id("tapd-endpoint")))
            .child(section(self.tr(cx,"凭证与授权","Credentials and authorization")))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(self.tr(cx,"密钥仅保存在此 Mac 的 Keychain。留空表示保留已有值，保存后清空输入框。请先保存 CLI 配置再连接。","Secrets stay in this Mac’s Keychain. Empty fields preserve existing values; fields clear after saving. Save CLI settings before connecting.")))
            .child(field("Lark App ID",Input::new(&self.app_id).id("lark-app-id")))
            .child(field("Lark App Secret",Input::new(&self.app_secret).id("lark-app-secret")))
            .child(field(self.tr(cx,"飞书用户 Token（可选，或使用 OAuth）","Lark user token (optional, or use OAuth)"),Input::new(&self.lark_token).id("lark-user-token")))
            .child(field("TAPD Token",Input::new(&self.tapd_token).id("tapd-token")))
            .child(div().flex().gap_3().child(Button::new("save-credentials").label(self.tr(cx,"加密保存凭证","Save to Keychain")).primary().disabled(credential_disabled).on_click(cx.listener(|this,_,window,cx|{
                let credentials=Credentials {lark_app_id:this.app_id.read(cx).value().to_string(),lark_app_secret:this.app_secret.read(cx).value().to_string(),lark_user_token:this.lark_token.read(cx).value().to_string(),tapd_token:this.tapd_token.read(cx).value().to_string()};
                this.send(Job::SaveCredentials(credentials),cx);
                for input in [&this.app_id,&this.app_secret,&this.lark_token,&this.tapd_token] {set(input,String::new(),window,cx);}
            })))
                .child(Button::new("lark-oauth").label(self.tr(cx,"连接飞书（OAuth）","Connect Lark (OAuth)")).disabled(credential_disabled).on_click(cx.listener(|this,_,_,cx|this.send(Job::Login,cx))))
                )
            .child(Button::new("clear-credentials").label(if self.clear_confirmation { self.tr(cx,"再次点击：清除所有保存的凭证","Click again: clear all saved credentials") } else { self.tr(cx,"清除凭证并重新连接","Clear credentials and reconnect") }).ghost().disabled(credential_disabled).on_click(cx.listener(|this,_,_,cx| {if this.clear_confirmation {this.clear_confirmation=false;this.send(Job::ClearCredentials,cx);}else{this.clear_confirmation=true;cx.notify();}})))
            .child(div().flex().gap_3().child(Button::new("lark-console").label(self.tr(cx,"飞书开发者后台","Lark developer console")).ghost().on_click(|_,_,cx|cx.open_url("https://open.feishu.cn/app"))).child(Button::new("tapd-console").label(self.tr(cx,"TAPD 账号与令牌","TAPD account and tokens")).ghost().on_click(|_,_,cx|cx.open_url("https://www.tapd.cn"))))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(self.tr(cx,"演示与真实账户的数据库隔离。启动变量决定模式，运行期间不能切换。TAPD CLI 当前使用 Token，不假设提供 OAuth。","Demo and live accounts use separate databases. The startup variable selects the mode; it cannot change while running. TAPD CLI currently uses tokens, not an assumed OAuth flow.")))
            )
    }
}
