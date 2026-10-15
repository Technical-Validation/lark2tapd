//! UI 只保存非敏感连接状态；密钥经串行后台保存，成功后仅清空所属服务表单。
use super::*;

#[derive(Clone, Default)]
pub(super) struct ServiceConnection {
    pub saved: bool,
    pub verified: bool,
    pub cli_version: Option<String>,
    pub cli_error: Option<String>,
    pub report: Option<ConnectionReport>,
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

pub(super) fn cli_key(tool: CliTool) -> &'static str {
    match tool {
        CliTool::Lark => "lark-cli",
        CliTool::Tapd => "tapd-cli",
    }
}

impl Model {
    pub(super) fn connection(&self, tool: CliTool) -> &ServiceConnection {
        match tool {
            CliTool::Lark => &self.lark_connection,
            CliTool::Tapd => &self.tapd_connection,
        }
    }
    pub(super) fn connection_mut(&mut self, tool: CliTool) -> &mut ServiceConnection {
        match tool {
            CliTool::Lark => &mut self.lark_connection,
            CliTool::Tapd => &mut self.tapd_connection,
        }
    }
    pub(super) fn tapd_configured(&self) -> bool {
        self.tapd_connection.cli_version.is_some()
            && (self.tapd_connection.saved || self.tapd_connection.verified)
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
        if self.model.read(cx).connection(tool).cli_version.is_none() {
            return;
        }
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
        let probing = model.activities.contains(cli_key(tool));
        let ready = state.cli_version.is_some();
        let path = if lark {
            &self.lark_path
        } else {
            &self.tapd_path
        };
        div().p_4().border_1().border_color(cx.theme().border).rounded_md().flex().flex_col().gap_3()
            .child(section(if lark { self.tr(cx,"飞书 CLI","Lark CLI") } else { "TAPD CLI" }))
            .child(self.help_field(self.tr(cx,"可执行文件","Executable"),
                if lark { "lark-cli-help" } else { "tapd-cli-help" },
                self.tr(cx,"应用自动检测路径。通过执行与版本检查后，才开放下方的账号配置。","The app detects the path. Account configuration is available after the executable and version check succeeds."),
                Some(if lark { "https://github.com/larksuite/cli" } else { "https://cnb.cool/tapd.cn/skills/tapd-cli" }),
                Input::new(path).id(if lark { "lark-path" } else { "tapd-path" }).disabled(true),cx))
            .when_some(state.cli_version.clone(), |d,v| d.child(div().text_sm().text_color(cx.theme().success).child(v)))
            .child(div().flex().gap_2()
                .child(self.focus_region(if lark { "probe-lark-cli" } else { "probe-tapd-cli" }, Button::new(if lark { "probe-lark-cli" } else { "probe-tapd-cli" })
                    .label(if probing { self.tr(cx,"检测中…","Checking…") } else { self.tr(cx,"检测 CLI","Check CLI") })
                    .disabled(probing).on_click(cx.listener(move |this,_,_,cx| this.send(Job::ProbeCli(tool),cx)))))
                .when(!ready, |d| d.child(self.focus_region(if lark { "install-lark" } else { "install-tapd" }, Button::new(if lark { "install-lark" } else { "install-tapd" })
                    .label(self.tr(cx,"安装 CLI","Install CLI")).disabled(model.busy || probing)
                    .on_click(cx.listener(move |this,_,_,cx| this.send(Job::PlanInstall(tool),cx)))))))
            .when_some(state.cli_error.clone(), |d,e| d.child(div().text_sm().text_color(cx.theme().danger).child(e)))
            .child(self.inline_error(cli_key(tool),cx))
            .when_some(model.install_plan.clone().filter(|p| p.tool==tool), |d,p| d.child(self.install_confirmation(p,cx)))
            .when(ready, |d| d.child(self.account_section(tool,cx)))
            .into_any_element()
    }

    fn account_section(&self, tool: CliTool, cx: &Context<Self>) -> impl IntoElement {
        let model = self.model.read(cx);
        let state = model.connection(tool);
        let lark = tool == CliTool::Lark;
        let open = if lark {
            self.lark_form_open
        } else {
            self.tapd_form_open
        };
        let busy = model.busy || model.activities.contains(field_key(tool));
        let disabled = busy || !model.connections_loaded;
        let confirming = self.clear_confirmation == Some(tool);
        let authorization = if lark {
            model.lark_authorization.clone()
        } else {
            None
        };
        let active = state.saved || open || authorization.is_some() || state.report.is_some();
        div().mt_2().pt_3().border_t_1().border_color(cx.theme().border).flex().flex_col().gap_3()
            .child(div().font_semibold().child(if lark { self.tr(cx,"飞书连接","Lark connection") } else { self.tr(cx,"TAPD 连接","TAPD connection") }))
            .when(state.saved || state.verified, |d| d.child(div().text_sm().text_color(cx.theme().muted_foreground).child(if state.verified {
                self.tr(cx,"连接已验证；其他能力见下方检查摘要。","Connection verified; see the capability summary below.")
            } else { self.tr(cx,"信息已保存，连接尚未验证。","Configuration saved; connection not verified.") })))
            .child(div().flex().gap_2()
                .child(self.focus_region(if lark { "connect-lark" } else { "connect-tapd" }, Button::new(if lark { "connect-lark" } else { "connect-tapd" })
                    .label(if state.saved { self.tr(cx,"编辑配置","Edit configuration") } else if lark { self.tr(cx,"配置飞书","Configure Lark") } else { self.tr(cx,"配置 TAPD","Configure TAPD") })
                    .disabled(busy || open).on_click(cx.listener(move |this,_,w,cx| this.open_connection_form(tool,w,cx)))))
                .when(state.saved, |d| d.child(self.focus_region(if lark { "clear-lark" } else { "clear-tapd" }, Button::new(if lark { "clear-lark" } else { "clear-tapd" })
                    .label(self.tr(cx,"清除凭证并重新连接","Clear credentials and reconnect")).ghost().disabled(model.busy)
                    .on_click(cx.listener(move |this,_,_,cx| { this.clear_confirmation=Some(tool); cx.notify(); }))))))
            .when(confirming, |d| d.child(div().flex().flex_col().gap_2()
                .child(self.tr(cx,"仅清除此服务在应用中保存的凭证，不影响另一服务或外部 CLI 登录。","Clear only this service’s saved app credentials. The other service and external CLI sessions are unchanged."))
                .child(div().flex().gap_2()
                    .child(self.focus_region(if lark { "confirm-clear-lark" } else { "confirm-clear-tapd" }, Button::new(if lark { "confirm-clear-lark" } else { "confirm-clear-tapd" })
                        .label(self.tr(cx,"确认清除","Confirm clear")).disabled(model.busy).on_click(cx.listener(move |this,_,_,cx| this.send(Job::ClearCredentials(tool),cx)))))
                    .child(self.focus_region(if lark { "cancel-clear-lark" } else { "cancel-clear-tapd" }, Button::new(if lark { "cancel-clear-lark" } else { "cancel-clear-tapd" })
                        .label(self.tr(cx,"取消","Cancel")).on_click(cx.listener(|this,_,_,cx| { this.clear_confirmation=None; cx.notify(); })))))))
            .when(open, |d| d.child(self.connection_form(tool,disabled,cx)))
            .when_some(authorization, |d,(url,seconds)| d.child(div().p_3().rounded_md().bg(cx.theme().muted).flex().flex_col().gap_2()
                .child(self.tr(cx,"等待飞书授权：在浏览器完成后，这里会更新结果。","Waiting for Lark authorization. Complete it in the browser; results will appear here."))
                .child(format!("{} {seconds}s",self.tr(cx,"链接有效期","Link validity")))
                .child(self.focus_region("open-lark-authorization",Button::new("open-lark-authorization").primary()
                    .label(self.tr(cx,"打开飞书授权页面","Open Lark authorization"))
                    .on_click(move |_,_,cx| open_safe_url(&url,cx))))))
            .when(active, |d| d
                .child(self.permission_summary(tool,cx))
                .child(self.focus_region(if lark { "check-lark" } else { "check-tapd" }, Button::new(if lark { "check-lark" } else { "check-tapd" })
                    .label(self.tr(cx,"测试连接与权限","Test connection and permissions")).disabled(model.activities.contains(field_key(tool)))
                    .on_click(cx.listener(move |this,_,_,cx| this.send(Job::Check(tool),cx))))))
            .child(div().debug_selector(move || if lark { "lark-operation-error".into() } else { "tapd-operation-error".into() }).child(self.inline_error(field_key(tool),cx)))
    }

    fn permission_summary(&self, tool: CliTool, cx: &Context<Self>) -> AnyElement {
        let state = self.model.read(cx).connection(tool);
        let lark = tool == CliTool::Lark;
        let mut content = div()
            .debug_selector(move || {
                if lark {
                    "lark-permission-summary".into()
                } else {
                    "tapd-permission-summary".into()
                }
            })
            .flex()
            .flex_col()
            .gap_2()
            .text_sm()
            .child(div().font_semibold().child(self.tr(
                cx,
                "权限与能力摘要",
                "Permissions and capabilities",
            )));
        let Some(report) = &state.report else {
            return content
                .child(self.tr(
                    cx,
                    "尚未测试；保存信息不代表权限已验证。",
                    "Not tested. Saved credentials do not establish permissions.",
                ))
                .into_any_element();
        };
        for check in &report.checks {
            let (label, color) = match check.state {
                PermissionState::Granted => (self.tr(cx, "已核验", "Verified"), cx.theme().success),
                PermissionState::Missing => (self.tr(cx, "缺失", "Missing"), cx.theme().danger),
                PermissionState::Unknown => (
                    self.tr(cx, "未核验", "Unverified"),
                    cx.theme().muted_foreground,
                ),
            };
            content = content.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_color(color)
                            .child(format!("{label} · {}", check.name)),
                    )
                    .when(!check.detail.is_empty(), |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(check.detail.clone()),
                        )
                    }),
            );
        }
        for (label, scopes) in [
            (
                self.tr(cx, "用户授权记录", "Stored user grants"),
                &report.user_scopes,
            ),
            (
                self.tr(cx, "应用已开通权限", "App-enabled scopes"),
                &report.app_scopes,
            ),
        ] {
            if let Some(scopes) = scopes {
                content = content.child(div().text_xs().child(format!(
                    "{label}: {}",
                    if scopes.is_empty() {
                        self.tr(cx, "无", "None").to_owned()
                    } else {
                        scopes.join(", ")
                    }
                )));
            }
        }
        content.into_any_element()
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
                .child(self.help_field(self.tr(cx,"Lark App Secret（首次配置）","Lark App Secret (initial setup)"),"lark-secret-help",self.tr(cx,"首次配置时与 App ID 一起填写。已有 CLI 应用配置不会被覆盖，更新其密钥需使用 CLI。","Enter together with App ID for initial setup. Existing CLI app profiles are preserved; use the CLI to update their secrets."),Some("https://open.feishu.cn/app"),Input::new(&self.app_secret).id("lark-app-secret").disabled(disabled),cx))
                .child(self.help_field(self.tr(cx,"飞书用户 Token（可选）","Lark user token (optional)"),"lark-token-help",self.tr(cx,"手工 Token 仅可保存；同步需 OAuth。首次连接留空；已保存 Token 时先清除飞书凭证，再使用 OAuth。","Manual tokens can only be saved; syncing requires OAuth. Leave blank on first setup. If a token is already saved, clear the Lark credentials before using OAuth."),Some("https://open.feishu.cn/document/authentication-management/access-token/get-user-access-token"),Input::new(&self.lark_token).id("lark-user-token").disabled(disabled),cx)))
            .when(!lark, |d| d.child(self.help_field("TAPD Token","tapd-token-help",self.tr(cx,"在 TAPD 账号设置中获取 API Token。","Get an API token from TAPD account settings."),Some("https://www.tapd.cn"),Input::new(&self.tapd_token).id("tapd-token").disabled(disabled),cx)))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(self.tr(cx,"保存时自动写入本机 Keychain，留空保留已有值。不会写入数据库或日志。","Saving stores credentials in this Mac’s Keychain. Blank fields keep existing values. Secrets never go into the database or logs.")))
            .when(lark, |d| d.child(div().text_xs().text_color(cx.theme().muted_foreground).child(self.tr(cx,"首次连接会为此应用创建专用 CLI 配置，并由 CLI 原生密钥存储保存凭证；已有应用配置和默认配置不会被覆盖。已有 CLI 应用的密钥更新需在 CLI 中完成。","First connection creates an app-specific CLI profile using the CLI’s native secret store. Existing app profiles and the default profile are not overwritten. Update secrets for existing CLI apps through the CLI."))))
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
