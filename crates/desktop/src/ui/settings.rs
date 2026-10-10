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
        div().id("settings-scroll").flex_1().h_full().overflow_y_scroll().p_6().flex().flex_col().gap_5()
            .child(div().text_2xl().font_semibold().child(self.tr(cx,"配置中心","Settings")))
            .child(div().text_color(cx.theme().muted_foreground).child(self.tr(cx,"连接账号、设置导入目标，并控制后台检查。","Connect accounts, choose a destination, and control background checks.")))
            .child(div().flex().gap_3().child(Button::new("mode-mock").label(self.tr(cx,"演示模式","Demo mode")).ghost().disabled(busy).when(model.mode==Mode::Mock,|b|b.primary()).on_click(cx.listener(|this,_,_,cx|this.send(Job::SwitchMode(Mode::Mock),cx)))).child(Button::new("mode-live").label(self.tr(cx,"真实连接","Live connections")).ghost().disabled(busy).when(model.mode==Mode::Live,|b|b.primary()).on_click(cx.listener(|this,_,_,cx|this.send(Job::SwitchMode(Mode::Live),cx)))))
            .child(section(self.tr(cx,"外观与语言","Appearance and language")))
            .child(div().flex().gap_3().children([("zh-CN","中文"),("en","English")].into_iter().map(|(locale,label)|Button::new(locale).label(label).ghost().when(language==locale,|b|b.primary()).disabled(busy).on_click(cx.listener(move |this,_,_,cx| {if let Some(mut s)=this.model.read(cx).snapshot.as_ref().map(|s|s.settings.clone()){s.language=locale.into();this.send(Job::SaveSettings(s),cx);}})))))
            .child(div().flex().gap_3().children([("system","跟随系统","System"),("light","亮色","Light"),("dark","暗色","Dark")].into_iter().map(|(mode,zh,en)|Button::new(mode).label(text(&language,zh,en)).ghost().when(settings.theme==mode,|b|b.primary()).disabled(busy).on_click(cx.listener(move |this,_,_,cx|{if let Some(mut s)=this.model.read(cx).snapshot.as_ref().map(|s|s.settings.clone()){s.theme=mode.into();this.send(Job::SaveSettings(s),cx);}})))))
            .child(section(self.tr(cx,"导入与后台检查","Import and background checks")))
            .child(div().flex().gap_4().child(field(self.tr(cx,"默认项目 ID","Default workspace ID"),Input::new(&self.settings_workspace).id("settings-workspace"))).child(field(self.tr(cx,"默认负责人账号","Default owner account"),Input::new(&self.settings_owner).id("settings-owner"))))
            .child(div().flex().gap_4().items_end().child(field(self.tr(cx,"检查间隔（1–1440 分钟）","Interval (1–1440 minutes)"),Input::new(&self.poll).id("poll-minutes"))).child(Button::new("toggle-poll").label(if settings.poll_enabled{self.tr(cx,"暂停检查","Pause checks")}else{self.tr(cx,"开启检查","Enable checks")}).ghost().disabled(busy).on_click(cx.listener(|this,_,_,cx|{if let Some(mut s)=this.settings(cx){s.poll_enabled = !s.poll_enabled;this.send(Job::SaveSettings(s),cx);}}))))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(self.tr(cx,"关闭窗口后仍可检查；退出应用后停止。首次读取只建立基线，不发送历史任务通知。","Checks continue when the window closes, but stop when the app quits. Initial fetch establishes a baseline without notifying historical tasks.")))
            .child(section(self.tr(cx,"CLI 连接","CLI connection")))
            .child(field(self.tr(cx,"飞书 CLI 可执行文件","Lark CLI executable"),Input::new(&self.lark_path).id("lark-path")))
            .child(field(self.tr(cx,"TAPD CLI 可执行文件","TAPD CLI executable"),Input::new(&self.tapd_path).id("tapd-path")))
            .child(field(self.tr(cx,"TAPD API 地址（HTTPS）","TAPD API endpoint (HTTPS)"),Input::new(&self.endpoint).id("tapd-endpoint")))
            .child(Button::new("save-settings").label(self.tr(cx,"保存配置","Save settings")).primary().disabled(busy).on_click(cx.listener(|this,_,_,cx|{if let Some(s)=this.settings(cx){this.send(Job::SaveSettings(s),cx);}})))
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
                .child(Button::new("check-connection").label(self.tr(cx,"检查连接","Check connection")).disabled(credential_disabled).on_click(cx.listener(|this,_,_,cx|this.send(Job::Check,cx)))))
            .child(Button::new("clear-credentials").label(if self.clear_confirmation { self.tr(cx,"再次点击：清除所有保存的凭证","Click again: clear all saved credentials") } else { self.tr(cx,"清除凭证并重新连接","Clear credentials and reconnect") }).ghost().disabled(credential_disabled).on_click(cx.listener(|this,_,_,cx| {if this.clear_confirmation {this.clear_confirmation=false;this.send(Job::ClearCredentials,cx);}else{this.clear_confirmation=true;cx.notify();}})))
            .child(div().flex().gap_3().child(Button::new("lark-console").label(self.tr(cx,"飞书开发者后台","Lark developer console")).ghost().on_click(|_,_,cx|cx.open_url("https://open.feishu.cn/app"))).child(Button::new("tapd-console").label(self.tr(cx,"TAPD 账号与令牌","TAPD account and tokens")).ghost().on_click(|_,_,cx|cx.open_url("https://www.tapd.cn"))))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(self.tr(cx,"演示与真实账户的数据库隔离。在配置中心切换真实模式；默认演示模式不会访问远端。TAPD CLI 当前使用 Token，不假设提供 OAuth。","Demo and live accounts use separate databases. Switch to Live connections in Settings; default demo mode never accesses remote services. TAPD CLI currently uses tokens, not an assumed OAuth flow.")))
    }
}
