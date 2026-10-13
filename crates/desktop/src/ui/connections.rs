//! UI 只保存非敏感连接状态；密钥经串行后台保存，成功后仅清空所属服务表单。
use super::*;

#[derive(Clone, Copy, Default)]
pub(super) struct ServiceConnection {
    pub saved: bool,
    pub verified: bool,
}

pub(super) struct CredentialChange {
    pub tool: CliTool,
    pub cleared: bool,
}

pub(super) fn field_key(tool: CliTool) -> &'static str {
    match tool {
        CliTool::Lark => "lark-connection",
        CliTool::Tapd => "tapd-connection",
    }
}

impl Model {
    pub(super) fn connection(&self, tool: CliTool) -> ServiceConnection {
        match tool {
            CliTool::Lark => self.lark_connection,
            CliTool::Tapd => self.tapd_connection,
        }
    }
    pub(super) fn connection_mut(&mut self, tool: CliTool) -> &mut ServiceConnection {
        match tool {
            CliTool::Lark => &mut self.lark_connection,
            CliTool::Tapd => &mut self.tapd_connection,
        }
    }
    pub(super) fn tapd_configured(&self) -> bool {
        self.mode == Mode::Mock || self.tapd_connection.saved || self.tapd_connection.verified
    }
}

impl Workspace {
    pub(super) fn set_connection_form(&mut self, tool: CliTool, open: bool) {
        match tool {
            CliTool::Lark => self.lark_form_open = open,
            CliTool::Tapd => self.tapd_form_open = open,
        }
    }
    pub(super) fn clear_connection_inputs(&self, tool: CliTool, window: &mut Window, cx: &mut App) {
        match tool {
            CliTool::Lark => {
                for input in [&self.app_id, &self.app_secret, &self.lark_token] {
                    set(input, String::new(), window, cx);
                }
            }
            CliTool::Tapd => set(&self.tapd_token, String::new(), window, cx),
        }
    }
    fn open_connection_form(&mut self, tool: CliTool, window: &mut Window, cx: &mut Context<Self>) {
        self.set_connection_form(tool, true);
        self.clear_confirmation = None;
        match tool {
            CliTool::Lark => self.app_id.read(cx).focus_handle(cx).focus(window, cx),
            CliTool::Tapd => self.tapd_token.read(cx).focus_handle(cx).focus(window, cx),
        }
        cx.notify();
    }
    pub(super) fn connection_card(&self, tool: CliTool, cx: &Context<Self>) -> AnyElement {
        let model = self.model.read(cx);
        let state = model.connection(tool);
        let lark = tool == CliTool::Lark;
        let busy = model.busy || model.activities.contains(field_key(tool));
        let disabled = busy || model.mode == Mode::Mock || !model.connections_loaded;
        let open = if lark {
            self.lark_form_open
        } else {
            self.tapd_form_open
        };
        let (path, diagnostic) = if lark {
            (&self.lark_path, &model.lark_diagnostic)
        } else {
            (&self.tapd_path, &model.tapd_diagnostic)
        };
        let detected = std::path::Path::new(path.read(cx).value().as_ref()).is_file();
        let confirming = self.clear_confirmation == Some(tool);
        let status = if model.mode == Mode::Mock {
            self.tr(
                cx,
                "演示模式 · 不连接真实账号",
                "Demo · no real account connection",
            )
        } else if !model.connections_loaded {
            self.tr(cx, "正在读取连接信息…", "Loading connection information…")
        } else if state.verified {
            self.tr(cx, "连接已验证", "Connection verified")
        } else if state.saved {
            self.tr(
                cx,
                "已保存 · 连接尚未验证",
                "Saved · connection not verified",
            )
        } else {
            self.tr(cx, "未保存连接信息", "No saved connection")
        };
        div().p_4().border_1().border_color(cx.theme().border).rounded_md().flex().flex_col().gap_3()
            .child(div().flex().items_center().gap_3()
                .child(div().flex_1().font_semibold().text_lg().child(if lark { self.tr(cx,"飞书","Lark") } else { "TAPD" }))
                .child(div().text_sm().text_color(if state.verified { cx.theme().success } else { cx.theme().muted_foreground }).child(status)))
            .child(div().flex().gap_2()
                .child(self.focus_region(if lark { "connect-lark" } else { "connect-tapd" }, Button::new(if lark { "connect-lark" } else { "connect-tapd" })
                    .label(if state.saved { self.tr(cx,"编辑连接","Edit connection") } else if lark { self.tr(cx,"连接飞书","Connect Lark") } else { self.tr(cx,"连接 TAPD","Connect TAPD") })
                    .disabled(busy || open)
                    .on_click(cx.listener(move |this,_,window,cx| this.open_connection_form(tool,window,cx)))))
                .when(state.saved, |d| d.child(self.focus_region(if lark { "clear-lark" } else { "clear-tapd" }, Button::new(if lark { "clear-lark" } else { "clear-tapd" })
                    .label(self.tr(cx,"清除凭证并重新连接","Clear credentials and reconnect"))
                    .ghost().disabled(disabled)
                    .on_click(cx.listener(move |this,_,_,cx| { this.clear_confirmation=Some(tool); cx.notify(); }))))))
            .when(confirming, |d| d.child(div().flex().flex_col().gap_2()
                .child(self.tr(cx,"仅清除此服务在应用中保存的凭证，不影响另一服务或外部 CLI 登录。","Clear only this service’s saved app credentials. The other service and external CLI sessions are unchanged."))
                .child(div().flex().gap_2()
                    .child(self.focus_region(if lark { "confirm-clear-lark" } else { "confirm-clear-tapd" }, Button::new(if lark { "confirm-clear-lark" } else { "confirm-clear-tapd" }).label(self.tr(cx,"确认清除","Confirm clear")).disabled(disabled).on_click(cx.listener(move |this,_,_,cx| this.send(Job::ClearCredentials(tool),cx)))))
                    .child(self.focus_region(if lark { "cancel-clear-lark" } else { "cancel-clear-tapd" }, Button::new(if lark { "cancel-clear-lark" } else { "cancel-clear-tapd" }).label(self.tr(cx,"取消","Cancel")).disabled(busy).on_click(cx.listener(|this,_,_,cx| { this.clear_confirmation=None; cx.notify(); })))))))
            .when(open, |d| d.child(self.connection_form(tool,disabled,cx)))
            .child(self.inline_error(field_key(tool),cx))
            .when(!diagnostic.is_empty(), |d| d.child(div().text_sm().child(diagnostic.clone())))
            .child(self.help_field(if lark { self.tr(cx,"飞书 CLI","Lark CLI") } else { "TAPD CLI" },
                if lark { "lark-cli-help" } else { "tapd-cli-help" },
                self.tr(cx,"应用自动检测路径，不能手工修改。安装或连接遇到问题时可查看官方文档。","The app detects this read-only path. See the official documentation for installation or connection help."),
                Some(if lark { "https://github.com/larksuite/cli" } else { "https://cnb.cool/tapd.cn/skills/tapd-cli" }),
                Input::new(path).id(if lark { "lark-path" } else { "tapd-path" }).disabled(true),cx))
            .child(div().flex().gap_2()
                .child(self.focus_region(if lark { "check-lark" } else { "check-tapd" }, Button::new(if lark { "check-lark" } else { "check-tapd" }).label(self.tr(cx,"检查连接","Check connection")).disabled(model.activities.contains(field_key(tool))).on_click(cx.listener(move |this,_,_,cx| this.send(Job::Check(tool),cx)))))
                .when(!detected, |d| d.child(self.focus_region(if lark { "install-lark" } else { "install-tapd" }, Button::new(if lark { "install-lark" } else { "install-tapd" }).label(self.tr(cx,"安装 CLI","Install CLI")).disabled(disabled).on_click(cx.listener(move |this,_,_,cx| this.send(Job::PlanInstall(tool),cx)))))))
            .when_some(model.install_plan.clone().filter(|plan| plan.tool==tool),|d,plan| d.child(self.install_confirmation(plan,cx)))
            .into_any_element()
    }
    fn connection_form(
        &self,
        tool: CliTool,
        disabled: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let lark = tool == CliTool::Lark;
        div().flex().flex_col().gap_3()
            .when(lark, |d| d
                .child(self.help_field("Lark App ID","lark-app-id-help",self.tr(cx,"在飞书开发者后台创建应用并获取 App ID。","Create an app in the Lark developer console to get its App ID."),Some("https://open.feishu.cn/app"),Input::new(&self.app_id).id("lark-app-id").disabled(disabled),cx))
                .child(self.help_field("Lark App Secret","lark-secret-help",self.tr(cx,"在开发者后台获取应用密钥。修改应用时须一起填写 App ID 和 Secret。","Get the app secret in the developer console. Enter both App ID and Secret when changing app credentials."),Some("https://open.feishu.cn/app"),Input::new(&self.app_secret).id("lark-app-secret").disabled(disabled),cx))
                .child(self.help_field(self.tr(cx,"飞书用户 Token（可选）","Lark user token (optional)"),"lark-token-help",self.tr(cx,"手工 Token 仅可保存；同步需 OAuth。首次连接留空；已保存 Token 时先清除飞书凭证，再使用 OAuth。","Manual tokens can only be saved; syncing requires OAuth. Leave blank on first setup. If a token is already saved, clear the Lark credentials before using OAuth."),Some("https://open.feishu.cn/document/authentication-management/access-token/get-user-access-token"),Input::new(&self.lark_token).id("lark-user-token").disabled(disabled),cx)))
            .when(!lark, |d| d.child(self.help_field("TAPD Token","tapd-token-help",self.tr(cx,"在 TAPD 账号设置中获取 API Token。","Get an API token from TAPD account settings."),Some("https://www.tapd.cn"),Input::new(&self.tapd_token).id("tapd-token").disabled(disabled),cx)))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(self.tr(cx,"保存时自动写入本机 Keychain，留空保留已有值。不会写入数据库或日志。","Saving stores credentials in this Mac’s Keychain. Blank fields keep existing values. Secrets never go into the database or logs.")))
            .child(div().flex().gap_2()
                .child(self.focus_region(if lark { "save-lark-connection" } else { "save-tapd-connection" }, Button::new(if lark { "save-lark-connection" } else { "save-tapd-connection" }).primary().label(self.tr(cx,"保存并连接","Save and connect")).disabled(disabled).on_click(cx.listener(move |this,_,_,cx| {
                    let credentials = if lark {
                        Credentials { lark_app_id:this.app_id.read(cx).value().to_string(), lark_app_secret:this.app_secret.read(cx).value().to_string(), lark_user_token:this.lark_token.read(cx).value().to_string(), tapd_token:String::new() }
                    } else { Credentials { tapd_token:this.tapd_token.read(cx).value().to_string(), lark_app_id:String::new(), lark_app_secret:String::new(), lark_user_token:String::new() } };
                    this.send(Job::SaveAndConnect { tool, credentials },cx);
                }))))
                .child(self.focus_region(if lark { "cancel-lark-form" } else { "cancel-tapd-form" }, Button::new(if lark { "cancel-lark-form" } else { "cancel-tapd-form" }).label(self.tr(cx,"取消","Cancel")).disabled(self.model.read(cx).activities.contains(field_key(tool))).on_click(cx.listener(move |this,_,window,cx| {
                    this.clear_connection_inputs(tool,window,cx); this.set_connection_form(tool,false); this.focus_handle.focus(window,cx); cx.notify();
                })))))
    }
    pub(super) fn tapd_required(&self, id: &'static str, cx: &Context<Self>) -> impl IntoElement {
        div()
            .p_4()
            .rounded_md()
            .bg(cx.theme().muted)
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .child(self.tr(cx, "请先配置 TAPD", "Configure TAPD first")),
            )
            .child(
                self.focus_region(
                    id,
                    Button::new(id)
                        .label(self.tr(cx, "连接 TAPD", "Connect TAPD"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.settings_scroll.set_offset(point(px(0.), px(0.)));
                            this.open_connection_form(CliTool::Tapd, window, cx);
                        })),
                ),
            )
    }
}
