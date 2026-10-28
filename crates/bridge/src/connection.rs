use crate::{
    CliGateway,
    diagnostics::{PROFILE_HELP, TASK_SCOPE},
    installer::{self, CliTool},
    process,
};
use anyhow::{Result, ensure};
use serde_json::Value;
use std::{
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
use zeroize::{Zeroize, Zeroizing};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionState {
    Granted,
    Missing,
    Unknown,
}
#[derive(Clone, Debug)]
pub struct PermissionCheck {
    pub name: String,
    pub state: PermissionState,
    pub detail: String,
}
#[derive(Clone, Debug, Default)]
pub struct ConnectionReport {
    pub connected: bool,
    pub checks: Vec<PermissionCheck>,
    pub user_scopes: Option<Vec<String>>,
    pub app_scopes: Option<Vec<String>>,
    pub failure: Option<String>,
}

// 不实现 Debug/Serialize；只有浏览器地址和有效期可以跨 UI 边界。
pub struct DeviceAuthorization {
    pub verification_url: String,
    pub expires_in: u64,
    device_code: String,
    app_id: String,
    deadline: Instant,
}
impl Drop for DeviceAuthorization {
    fn drop(&mut self) {
        self.device_code.zeroize();
    }
}

impl CliGateway {
    pub(crate) fn validate_lark_profile(&self, status: &Value) -> Result<()> {
        let app = status.get("appId").and_then(Value::as_str).unwrap_or("");
        ensure!(!app.is_empty(), "{PROFILE_HELP}");
        ensure!(
            self.credentials.lark_app_id.is_empty() || app == self.credentials.lark_app_id,
            "Native Lark profile App ID does not match the app; select or configure the matching native profile. {PROFILE_HELP}"
        );
        Ok(())
    }
    fn ready(&self, tool: CliTool) -> Result<String> {
        let path = match tool {
            CliTool::Lark => &self.settings.lark_path,
            CliTool::Tapd => &self.settings.tapd_path,
        };
        let version = installer::probe_cli(tool, Path::new(path))?;
        Ok(format!("CLI {version} is ready"))
    }
    // 诊断命令的非零退出可携带有效结构；只有明确的协议分支可以接受它。
    fn auth_json(&self, args: &[String], timeout: Duration) -> Result<(Value, Option<i32>)> {
        self.auth_json_cancellable(args, timeout, &AtomicBool::new(false))
    }
    fn auth_json_cancellable(
        &self,
        args: &[String],
        timeout: Duration,
        cancelled: &AtomicBool,
    ) -> Result<(Value, Option<i32>)> {
        self.validate_provider(true)?;
        ensure!(
            self.credentials.lark_user_token.is_empty(),
            "Cannot verify manual Lark user token identity; clear saved credentials and use CLI OAuth login"
        );
        let env = Zeroizing::new(self.env(true));
        let output = process::run_output_cancellable(
            Path::new(&self.settings.lark_path),
            args,
            &env,
            b"",
            timeout,
            cancelled,
        )?;
        let value: Value = serde_json::from_slice(&output.stdout).map_err(|_| {
            if output.status.success() {
                anyhow::anyhow!("CLI returned invalid JSON")
            } else {
                output.failure()
            }
        })?;
        if !output.status.success() {
            let expected = (args.get(1).map(String::as_str) == Some("check")
                && value.get("ok").and_then(Value::as_bool).is_some()
                && value.get("missing").is_some())
                || (args.get(1).map(String::as_str) == Some("login")
                    && value.get("event").and_then(Value::as_str)
                        == Some("authorization_complete"));
            if !expected {
                return Err(output.failure());
            }
        }
        Ok((
            value,
            if output.status.success() {
                None
            } else {
                output.status.code().or(Some(-1))
            },
        ))
    }
    /// 仅在用户确认保存并连接后调用。追加新原生 profile，不切换默认、不替换已有凭据。
    pub fn prepare_lark_profile(&self) -> Result<()> {
        self.validate_provider(true)?;
        ensure!(
            self.credentials.lark_user_token.is_empty(),
            "Manual Lark user tokens cannot configure native OAuth"
        );
        let app = &self.credentials.lark_app_id;
        let secret = &self.credentials.lark_app_secret;
        if app.is_empty() && secret.is_empty() {
            return Ok(());
        }
        ensure!(
            !app.is_empty()
                && app.len() <= 48
                && app
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-')),
            "Lark App ID must contain only letters, digits, underscore or hyphen (at most 48 bytes)"
        );
        ensure!(
            !secret.is_empty()
                && secret.len() <= 4096
                && !secret.chars().any(char::is_control)
                && secret.trim() == secret,
            "Lark App Secret must be a single non-empty line without surrounding whitespace"
        );
        self.ready(CliTool::Lark)?;
        let (profiles, _) =
            self.auth_json(&strings(&["profile", "list"]), Duration::from_secs(30))?;
        let profiles = profiles.as_array().ok_or_else(|| {
            anyhow::anyhow!(
                "Cannot safely inspect native CLI profiles; no configuration was changed"
            )
        })?;
        let name = format!("lark2tapd-{app}");
        // --name 的别名碰撞优先于 AppID 查找，必须拒绝而非改写不相关 profile。
        ensure!(
            !profiles.iter().any(|p| {
                matches!(p.get("name").and_then(Value::as_str), Some(n) if n == name || n == app)
                    && p.get("appId").and_then(Value::as_str) != Some(app.as_str())
            }),
            "Native CLI profile name collision; no configuration was changed. Select a non-conflicting native profile manually"
        );
        if profiles
            .iter()
            .any(|p| p.get("appId").and_then(Value::as_str) == Some(app.as_str()))
        {
            // 已有 profile 的品牌、默认值与密钥一律保留；轮换须通过原生 CLI 显式完成。
            return Ok(());
        }
        ensure!(
            profiles.iter().all(|p| p
                .get("appId")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty())
                && p.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty())),
            "Cannot safely inspect native CLI profiles; no configuration was changed"
        );
        let args = vec![
            "profile".into(),
            "add".into(),
            "--name".into(),
            name,
            "--app-id".into(),
            app.clone(),
            "--app-secret-stdin".into(),
            "--brand".into(),
            "feishu".into(),
        ];
        let input = Zeroizing::new(format!("{secret}\n"));
        let env = Zeroizing::new(self.env(true));
        process::run(
            Path::new(&self.settings.lark_path),
            &args,
            &env,
            input.as_bytes(),
            Duration::from_secs(30),
        )?;
        Ok(())
    }
    /// 用户显式发起；不自动改写 CLI 配置，不尝试环境凭据引导。
    pub fn begin_lark_login(&self) -> Result<DeviceAuthorization> {
        self.ready(CliTool::Lark)?;
        let (status, _) = self.auth_json(
            &strings(&["auth", "status", "--json"]),
            Duration::from_secs(30),
        )?;
        self.validate_lark_profile(&status)?;
        let (mut value, _) = self.auth_json(
            &strings(&[
                "auth",
                "login",
                "--scope",
                TASK_SCOPE,
                "--no-wait",
                "--json",
            ]),
            Duration::from_secs(30),
        )?;
        let code = value
            .get_mut("device_code")
            .map(Value::take)
            .unwrap_or(Value::Null);
        let device_code = Zeroizing::new(match code {
            Value::String(s) => s,
            _ => String::new(),
        });
        let verification_url = value
            .get("verification_url")
            .and_then(Value::as_str)
            .unwrap_or("");
        let url = url::Url::parse(verification_url)
            .map_err(|_| anyhow::anyhow!("Invalid Lark verification URL"))?;
        ensure!(
            url.scheme() == "https"
                && url.username().is_empty()
                && url.password().is_none()
                && url.port().is_none()
                && matches!(
                    url.host_str(),
                    Some("accounts.feishu.cn" | "accounts.larksuite.com")
                ),
            "Untrusted Lark verification URL; authorization was not opened"
        );
        let expires_in = value.get("expires_in").and_then(Value::as_u64).unwrap_or(0);
        ensure!(
            !device_code.is_empty()
                && device_code.len() <= 4096
                && expires_in > 0
                && expires_in <= 3600,
            "Invalid Lark device authorization response"
        );
        Ok(DeviceAuthorization {
            verification_url: url.to_string(),
            expires_in,
            device_code: device_code.to_string(),
            app_id: status["appId"].as_str().unwrap().to_owned(),
            deadline: Instant::now() + Duration::from_secs(expires_in),
        })
    }
    /// 一次有界交换；CLI 必须使用 argv 传递一次性 device code，永不输出该参数。
    pub fn finish_lark_login(&self, authorization: &DeviceAuthorization) -> Result<()> {
        self.finish_lark_login_cancellable(authorization, &AtomicBool::new(false))
    }
    pub fn finish_lark_login_cancellable(
        &self,
        authorization: &DeviceAuthorization,
        cancelled: &AtomicBool,
    ) -> Result<()> {
        let timeout = authorization
            .deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| anyhow::anyhow!("Lark device authorization expired; start login again"))?
            .min(Duration::from_secs(180));
        ensure!(
            self.credentials.lark_app_id.is_empty()
                || self.credentials.lark_app_id == authorization.app_id,
            "Lark authorization belongs to another app; start login again"
        );
        let args = Zeroizing::new(vec![
            "auth".into(),
            "login".into(),
            "--device-code".into(),
            authorization.device_code.clone(),
            "--json".into(),
        ]);
        let (value, exit) = self.auth_json_cancellable(&args, timeout, cancelled)?;
        ensure!(
            value.get("event").and_then(Value::as_str) == Some("authorization_complete")
                && value
                    .get("user_open_id")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.trim().is_empty()),
            "Lark authorization did not complete; start login again"
        );
        let missing = scopes(value.get("missing"));
        let granted = scopes(value.get("granted")).or_else(|| scopes(value.get("scope")));
        ensure!(
            exit.is_none()
                && missing.as_ref().is_none_or(|s| s.is_empty())
                && granted
                    .as_ref()
                    .is_some_and(|s| s.iter().any(|s| s == TASK_SCOPE)),
            "Lark authorization completed but required scope task:task:read was not granted; enable the published app permission and authorize again"
        );
        Ok(())
    }
    /// 只读诊断；保留成功与失败的证据，绝不通过写入测试权限。
    pub fn connection_report(&self, tool: CliTool) -> ConnectionReport {
        let mut report = ConnectionReport::default();
        if !report.record("CLI readiness", self.ready(tool), true) {
            return report;
        }
        match tool {
            CliTool::Lark => self.lark_report(&mut report),
            CliTool::Tapd => self.tapd_report(&mut report),
        }
        report
    }
    fn lark_report(&self, report: &mut ConnectionReport) {
        let status = self.lark_identity();
        let identity = match status {
            Ok(status) => {
                report.user_scopes = scopes(status.pointer("/identities/user/scope"));
                let verified = status.get("verified").and_then(Value::as_bool) == Some(true)
                    && status.get("identity").and_then(Value::as_str) == Some("user")
                    && status
                        .pointer("/identities/user/openId")
                        .and_then(Value::as_str)
                        .is_some_and(|s| !s.trim().is_empty());
                report.record(
                    "User identity",
                    if verified {
                        Ok("User OAuth verified".into())
                    } else {
                        Err(anyhow::anyhow!(
                            "Lark user OAuth is not verified; authorize this profile"
                        ))
                    },
                    true,
                )
            }
            Err(e) => {
                report.record("User identity", Err(e), true);
                // profile 不可验证时，禁止用另一账户继续探测权限或资源。
                for name in [TASK_SCOPE, "Task read", "App scope metadata"] {
                    report.push(
                        name,
                        PermissionState::Unknown,
                        "Not attempted without a verified native profile".into(),
                    );
                }
                return;
            }
        };
        let check = self.auth_json(
            &strings(&["auth", "check", "--scope", TASK_SCOPE, "--json"]),
            Duration::from_secs(30),
        );
        let mut missing = false;
        match check {
            Ok((v, _)) => {
                let granted = scopes(v.get("granted"));
                let absent = scopes(v.get("missing"));
                let state = if absent
                    .as_ref()
                    .is_some_and(|s| s.iter().any(|s| s == TASK_SCOPE))
                {
                    missing = true;
                    PermissionState::Missing
                } else if v.get("ok").and_then(Value::as_bool) == Some(true)
                    && granted
                        .as_ref()
                        .is_some_and(|s| s.iter().any(|s| s == TASK_SCOPE))
                {
                    PermissionState::Granted
                } else {
                    PermissionState::Unknown
                };
                let detail = match state {
                    PermissionState::Granted => "Stored user consent includes task:task:read",
                    PermissionState::Missing => {
                        "Stored user consent is missing task:task:read; enable the app permission and authorize again"
                    }
                    PermissionState::Unknown => {
                        "Stored user scope metadata is unavailable; the read probe determines access"
                    }
                };
                report.push(TASK_SCOPE, state, detail.into());
            }
            Err(e) => {
                report.record(TASK_SCOPE, Err(e), false);
            }
        }
        let read = if identity {
            report.record(
                "Task read",
                self.related_until("", Instant::now() + Duration::from_secs(30))
                    .map(|_| "Related task read succeeded".into()),
                true,
            )
        } else {
            report.push(
                "Task read",
                PermissionState::Unknown,
                "Not attempted without verified user identity".into(),
            );
            false
        };
        match self.auth_json(
            &strings(&["auth", "scopes", "--json"]),
            Duration::from_secs(30),
        ) {
            Ok((v, _)) => {
                report.app_scopes = scopes(v.get("userScopes"));
                let app_scope_state = match &report.app_scopes {
                    Some(scopes) if scopes.iter().any(|scope| scope == TASK_SCOPE) => {
                        PermissionState::Granted
                    }
                    Some(_) => PermissionState::Missing,
                    None => PermissionState::Unknown,
                };
                report.push("App task:task:read", app_scope_state,
                    "Compare app-enabled user scope with required task:task:read; actual read access is checked separately".into());
                report.push(
                    "App scope metadata",
                    if report.app_scopes.is_some() {
                        PermissionState::Granted
                    } else {
                        PermissionState::Unknown
                    },
                    "Ancillary enabled user-token app scopes; not a task access probe".into(),
                );
            }
            Err(e) => {
                report.push("App task:task:read", PermissionState::Unknown, "App-enabled scope list unavailable; this does not establish a missing permission".into());
                report.record("App scope metadata", Err(e), false);
            }
        }
        report.connected = identity && read && !missing;
        if missing && report.failure.is_none() {
            report.failure = Some("Required user consent task:task:read is missing".into());
        }
    }
    fn tapd_report(&self, report: &mut ConnectionReport) {
        let identity = report.record(
            "User identity",
            self.tapd_user_id()
                .map(|_| "TAPD user identity verified".into()),
            true,
        );
        let read = if self.settings.workspace_id.is_empty() {
            report.push(
                "Workspace read",
                PermissionState::Unknown,
                "Select a workspace to verify read access".into(),
            );
            false
        } else if identity {
            let result = crate::valid_id(&self.settings.workspace_id).and_then(|_| {
                self.tapd(
                    "story",
                    "list",
                    vec![
                        format!("workspaceid={}", self.settings.workspace_id),
                        "limit=1".into(),
                        "page=1".into(),
                    ],
                )
            });
            report.record(
                "Workspace read",
                result.map(|_| "Selected workspace read succeeded".into()),
                true,
            )
        } else {
            report.push(
                "Workspace read",
                PermissionState::Unknown,
                "Not attempted without verified user identity".into(),
            );
            false
        };
        for name in [
            "Token permission enumeration",
            "Task write",
            "Attachment upload",
        ] {
            report.push(name, PermissionState::Unknown, "TAPD does not provide verified permission enumeration here; never tested by writing".into());
        }
        // 首次连接只确认 Token 身份；未选空间是尚未核验，不是认证失败。
        report.connected = identity && (self.settings.workspace_id.is_empty() || read);
    }
}
impl ConnectionReport {
    fn push(&mut self, name: &str, state: PermissionState, detail: String) {
        self.checks.push(PermissionCheck {
            name: name.into(),
            state,
            detail,
        });
    }
    fn record(&mut self, name: &str, result: Result<String>, required: bool) -> bool {
        match result {
            Ok(detail) => {
                self.push(name, PermissionState::Granted, detail);
                true
            }
            Err(error) => {
                let detail = error.to_string();
                if required && self.failure.is_none() {
                    self.failure = Some(detail.clone());
                }
                self.push(name, PermissionState::Unknown, detail);
                false
            }
        }
    }
}
fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_owned()).collect()
}
// Scope 元数据只允许正常的 scope 语法，不转发任意服务端文本或提示。
fn scopes(value: Option<&Value>) -> Option<Vec<String>> {
    let raw: Vec<&str> = match value? {
        Value::String(s) => s.split_whitespace().collect(),
        Value::Array(a) => a.iter().map(Value::as_str).collect::<Option<_>>()?,
        _ => return None,
    };
    let mut out = Vec::new();
    for s in raw {
        if s.len() > 160
            || s.is_empty()
            || !(s == "offline_access" || s.contains(':'))
            || !s.bytes().all(|c| {
                c.is_ascii_lowercase()
                    || c.is_ascii_digit()
                    || matches!(c, b':' | b'_' | b'.' | b'-')
            })
        {
            return None;
        }
        out.push(s.to_owned());
    }
    out.sort();
    out.dedup();
    Some(out)
}
