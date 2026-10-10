mod credentials;
mod process;
use anyhow::{Result, bail, ensure};
use chrono::{DateTime, Local, TimeZone, Utc};
pub use credentials::{
    Credentials, clear_credentials, credentials_namespace, load_credentials, save_credentials,
};
use lark2tapd_core::{Draft, Gateway, Settings, SourceTask, TargetTask};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    path::Path,
    time::{Duration, Instant},
};

pub struct CliGateway {
    settings: Settings,
    credentials: Credentials,
}
#[derive(Clone, Debug)]
pub struct AuthStatus {
    pub authenticated: bool,
    pub user_id: String,
}
impl CliGateway {
    pub fn new(settings: Settings, credentials: Credentials) -> Result<Self> {
        ensure!(
            Path::new(&settings.lark_path).is_absolute()
                && Path::new(&settings.tapd_path).is_absolute(),
            "Configure absolute CLI executable paths"
        );
        let endpoint = url::Url::parse(&settings.tapd_endpoint)
            .map_err(|_| anyhow::anyhow!("Invalid TAPD endpoint"))?;
        ensure!(
            endpoint.scheme() == "https"
                && endpoint.host_str() == Some("api.tapd.cn")
                && endpoint.username().is_empty()
                && endpoint.password().is_none()
                && endpoint.port().is_none()
                && endpoint.path() == "/"
                && endpoint.query().is_none()
                && endpoint.fragment().is_none(),
            "TAPD endpoint must be https://api.tapd.cn"
        );
        ensure!(
            (credentials.lark_app_secret.is_empty() && credentials.lark_user_token.is_empty())
                || !credentials.lark_app_id.is_empty(),
            "Lark App ID is required with a secret or user token"
        );
        Ok(Self {
            settings,
            credentials,
        })
    }
    fn env(&self, lark: bool) -> Vec<(String, String)> {
        let c = &self.credentials;
        let pairs = if lark {
            vec![
                ("LARKSUITE_CLI_APP_ID", c.lark_app_id.as_str()),
                ("LARKSUITE_CLI_APP_SECRET", c.lark_app_secret.as_str()),
                (
                    "LARKSUITE_CLI_USER_ACCESS_TOKEN",
                    c.lark_user_token.as_str(),
                ),
            ]
        } else {
            vec![
                ("TAPD_TOKEN", c.tapd_token.as_str()),
                ("TAPD_API_ENDPOINT", self.settings.tapd_endpoint.as_str()),
                ("TAPD_CLI_LOG", "silent"),
            ]
        };
        pairs
            .into_iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| (k.into(), v.into()))
            .collect()
    }
    fn invoke(
        &self,
        lark: bool,
        args: Vec<String>,
        input: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let path = if lark {
            &self.settings.lark_path
        } else {
            &self.settings.tapd_path
        };
        let bytes = if input.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(&input).map_err(|_| anyhow::anyhow!("Invalid CLI request"))?
        };
        let env = zeroize::Zeroizing::new(self.env(lark));
        let output =
            zeroize::Zeroizing::new(process::run(Path::new(path), &args, &env, &bytes, timeout)?);
        serde_json::from_slice(&output).map_err(|_| anyhow::anyhow!("CLI returned invalid JSON"))
    }
    fn lark(&self, verb: &str, params: Value) -> Result<Value> {
        self.lark_until(verb, params, Instant::now() + Duration::from_secs(30))
    }
    fn lark_until(&self, verb: &str, params: Value, deadline: Instant) -> Result<Value> {
        let timeout = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| {
                anyhow::anyhow!("Lark refresh deadline exceeded; no partial results accepted")
            })?
            .min(Duration::from_secs(30));
        let v = self.invoke(
            true,
            [
                "task", "tasks", verb, "--as", "user", "--format", "json", "--params", "-",
            ]
            .map(String::from)
            .to_vec(),
            params,
            timeout,
        )?;
        ensure!(
            v.get("ok").and_then(Value::as_bool) == Some(true),
            "Lark request failed; check task read permission and authentication"
        );
        v.get("data")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Lark response missing data"))
    }
    fn tapd(&self, kind: &str, verb: &str, params: Vec<String>) -> Result<Value> {
        // 不允许回退到 CLI 外部配置中的未知 TAPD 账户。
        ensure!(
            !self.credentials.tapd_token.is_empty(),
            "Configure a TAPD token in the app"
        );
        ensure!(
            matches!(kind, "task" | "story") || (kind == "user" && verb == "info"),
            "Invalid TAPD resource kind"
        );
        let mut args = vec![kind.into(), verb.into()];
        args.extend(params);
        let v = self.invoke(false, args, Value::Null, Duration::from_secs(30))?;
        ensure!(
            v.get("status").and_then(Value::as_i64) == Some(1),
            "TAPD request failed; check workspace and authentication"
        );
        v.get("data")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("TAPD response missing data"))
    }
    pub fn check_lark(&self) -> Result<()> {
        self.lark(
            "list",
            json!({"type":"my_tasks","page_size":1,"user_id_type":"open_id"}),
        )?;
        Ok(())
    }
    pub fn check_tapd(&self) -> Result<()> {
        valid_id(&self.settings.workspace_id)?;
        self.tapd_user_id()?;
        self.tapd(
            "story",
            "list",
            vec![
                format!("workspaceid={}", self.settings.workspace_id),
                "limit=1".into(),
                "page=1".into(),
            ],
        )?;
        Ok(())
    }
    /// 必须由用户主动触发并在工作线程执行；CLI 自行打开浏览器和管理其凭据。
    pub fn login_lark(&self) -> Result<()> {
        let v = self.invoke(
            true,
            ["auth", "login", "--scope", "task:task:read", "--json"]
                .map(String::from)
                .to_vec(),
            Value::Null,
            Duration::from_secs(180),
        )?;
        ensure!(
            v.get("ok").and_then(Value::as_bool) != Some(false),
            "Lark login failed"
        );
        ensure!(
            self.oauth_status()?.authenticated,
            "Lark login is not verified"
        );
        Ok(())
    }
    fn lark_identity(&self) -> Result<Value> {
        // whoami 只诊断本地保存身份，不能证明注入令牌的主体；未知主体必须拒绝。
        ensure!(
            self.credentials.lark_user_token.is_empty(),
            "Cannot verify manual Lark user token identity; clear saved credentials and use CLI OAuth login"
        );
        self.invoke(
            true,
            ["auth", "status", "--json", "--verify"]
                .map(String::from)
                .to_vec(),
            Value::Null,
            Duration::from_secs(30),
        )
    }
    pub fn oauth_status(&self) -> Result<AuthStatus> {
        let v = self.lark_identity()?;
        let user = v
            .pointer("/identities/user/openId")
            .and_then(Value::as_str)
            .unwrap_or("");
        let verified = v.get("verified").and_then(Value::as_bool) == Some(true)
            && v.get("identity").and_then(Value::as_str) == Some("user");
        Ok(AuthStatus {
            authenticated: verified && !user.is_empty(),
            user_id: user.to_owned(),
        })
    }
    fn tapd_user_id(&self) -> Result<String> {
        let user = self.tapd("user", "info", Vec::new())?;
        let id = required(&user, "id")?;
        valid_id(&id)?;
        Ok(id)
    }
    /// 身份键仅由已验证的公开主体组成；凭据轮换与 CLI 安装路径不影响同步库。
    pub fn account_namespace(&self) -> Result<String> {
        ensure!(
            !self.credentials.tapd_token.is_empty(),
            "Configure a TAPD token in the app"
        );
        let status = self.lark_identity()?;
        ensure!(
            status.get("verified").and_then(Value::as_bool) == Some(true)
                && status.get("identity").and_then(Value::as_str) == Some("user"),
            "Cannot verify Lark account identity; synchronization is locked"
        );
        let app = required(&status, "appId")?;
        let brand = required(&status, "brand")?;
        ensure!(
            matches!(brand.as_str(), "feishu" | "lark"),
            "Unknown Lark provider brand"
        );
        ensure!(
            self.credentials.lark_app_id.is_empty() || self.credentials.lark_app_id == app,
            "Verified Lark app does not match configured app"
        );
        let user = status
            .pointer("/identities/user/openId")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("Cannot verify Lark user identity"))?;
        let tapd_user = self.tapd_user_id()?;
        Ok(credentials::namespace_parts(&[
            "verified-account-v2",
            "lark",
            &brand,
            &app,
            user,
            "https://api.tapd.cn",
            &tapd_user,
        ]))
    }
    fn write_target(
        &self,
        old: Option<&TargetTask>,
        draft: &Draft,
        source: &SourceTask,
    ) -> Result<TargetTask> {
        self.validate_draft(draft)?;
        valid_source_id(&source.guid)?;
        let mut description = escape_html(&draft.description).replace('\n', "<br>");
        description.push_str(&format!("<p>[lark-task:{}]</p>", source.guid));
        let source_url = safe_url(&source.url, true);
        if !source_url.is_empty() {
            description.push_str(&format!("<p>{}</p>", escape_html(&source_url)));
        }
        let mut params = vec![
            format!("workspaceid={}", draft.workspace_id),
            format!("name={}", draft.title),
            format!("description={description}"),
            format!("owner={}", draft.owner),
            format!("due={}", draft.due.as_deref().unwrap_or("")),
        ];
        let (kind, verb) = if let Some(old) = old {
            valid_id(&old.id)?;
            params.push(format!("id={}", old.id));
            (old.kind.as_str(), "update")
        } else {
            ("task", "add")
        };
        let result = self.tapd(kind, verb, params)?;
        let (result_kind, fields) = target_fields(&result)?;
        let id = required(fields, "id")?;
        valid_id(&id)?;
        if let Some(old) = old {
            ensure!(
                id == old.id && result_kind == old.kind,
                "TAPD update returned a different target"
            );
        }
        // 写响应可能只包含 ID；读回服务器标准字段后才提交本地冲突基线。
        let target = self.get_target(&draft.workspace_id, &id, result_kind)?;
        // 宁可保留待人工核对状态，也不把服务端忽略的更新（尤其清空日期）标记为成功。
        ensure!(
            target.title == draft.title
                && target.description == description
                && target.owner == draft.owner
                && target.due == draft.due,
            "TAPD readback differs from requested fields; remote result is uncertain (including possible HTML normalization)"
        );
        Ok(target)
    }
}
impl CliGateway {
    fn source_until(&self, guid: &str, deadline: Instant) -> Result<SourceTask> {
        valid_source_id(guid)?;
        let full = self.lark_until(
            "get",
            json!({"task_guid":guid,"user_id_type":"open_id"}),
            deadline,
        )?;
        let task = full
            .get("task")
            .ok_or_else(|| anyhow::anyhow!("Lark response missing task detail"))?;
        ensure!(
            required(task, "guid")? == guid,
            "Lark task identity mismatch"
        );
        let due = match task.pointer("/due/timestamp") {
            None | Some(Value::Null) => None,
            Some(v) => {
                let millis = v
                    .as_str()
                    .and_then(|s| s.parse::<i64>().ok())
                    .ok_or_else(|| anyhow::anyhow!("Invalid Lark due date"))?;
                Some(due_date(millis, Local)?)
            }
        };
        Ok(SourceTask {
            guid: guid.to_owned(),
            title: required(task, "summary")?,
            description: text(task, "description"),
            url: safe_url(&text(task, "url"), true),
            due,
            completed: text(task, "status") == "done"
                || !matches!(text(task, "completed_at").as_str(), "" | "0"),
        })
    }
    fn fetch_tasks_until(&self, deadline: Instant) -> Result<Vec<SourceTask>> {
        let mut tasks = Vec::new();
        let mut page = String::new();
        let mut pages = HashSet::new();
        let mut ids = HashSet::new();
        loop {
            let mut params = json!({"type":"my_tasks","page_size":100,"user_id_type":"open_id"});
            if !page.is_empty() {
                params["page_token"] = json!(page);
            }
            let data = self.lark_until("list", params, deadline)?;
            let items = data
                .get("items")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow::anyhow!("Lark response missing task list"))?;
            ensure!(
                tasks.len() + items.len() <= 1000,
                "Lark refresh exceeds 1000 tasks; no partial results accepted"
            );
            for item in items {
                let guid = required(item, "guid")?;
                ensure!(
                    ids.insert(guid.clone()),
                    "Lark pagination repeated a task; refresh required"
                );
                tasks.push(self.source_until(&guid, deadline)?);
            }
            let more = data
                .get("has_more")
                .and_then(Value::as_bool)
                .ok_or_else(|| anyhow::anyhow!("Lark response missing pagination status"))?;
            if !more {
                return Ok(tasks);
            }
            page = required(&data, "page_token")?;
            ensure!(
                pages.insert(page.clone()) && pages.len() < 1000,
                "Lark pagination did not finish; no partial results accepted"
            );
        }
    }
}
impl Gateway for CliGateway {
    // 本地错误在写日志之前拒绝；直接调用写入口也复用相同规则。
    fn validate_draft(&self, draft: &Draft) -> Result<()> {
        valid_id(&draft.workspace_id)?;
        ensure!(!draft.title.trim().is_empty(), "Target title is required");
        if let Some(due) = &draft.due {
            let date = chrono::NaiveDate::parse_from_str(due, "%Y-%m-%d")
                .map_err(|_| anyhow::anyhow!("Invalid target due date"))?;
            ensure!(
                date.format("%Y-%m-%d").to_string() == *due,
                "Invalid target due date"
            );
        }
        Ok(())
    }
    fn get_source(&self, guid: &str) -> Result<SourceTask> {
        self.source_until(guid, Instant::now() + Duration::from_secs(30))
    }
    fn fetch_tasks(&self) -> Result<Vec<SourceTask>> {
        self.fetch_tasks_until(Instant::now() + Duration::from_secs(120))
    }
    fn get_target(&self, workspace: &str, id: &str, kind: &str) -> Result<TargetTask> {
        valid_id(workspace)?;
        valid_id(id)?;
        let data = self.tapd(
            kind,
            "list",
            vec![
                format!("workspaceid={workspace}"),
                format!("id={id}"),
                "page=1".into(),
                "limit=2".into(),
            ],
        )?;
        let items = data
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("TAPD response missing list"))?;
        ensure!(items.len() == 1, "TAPD target missing or ambiguous");
        let target = parse_target(&items[0])?;
        ensure!(
            target.id == id && target.kind == kind,
            "TAPD returned a different target"
        );
        Ok(target)
    }
    fn create_target(&self, draft: &Draft, source: &SourceTask) -> Result<TargetTask> {
        self.write_target(None, draft, source)
    }
    fn update_target(
        &self,
        old: &TargetTask,
        draft: &Draft,
        source: &SourceTask,
    ) -> Result<TargetTask> {
        self.write_target(Some(old), draft, source)
    }
}
// TAPD 仅支持日期；全天和定时任务均按本机时区取日历日期，明确丢弃时分。
fn due_date(millis: i64, zone: impl TimeZone) -> Result<String> {
    Ok(DateTime::<Utc>::from_timestamp_millis(millis)
        .ok_or_else(|| anyhow::anyhow!("Invalid Lark due date"))?
        .with_timezone(&zone)
        .date_naive()
        .to_string())
}
fn valid_source_id(guid: &str) -> Result<()> {
    ensure!(
        !guid.is_empty() && guid.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'),
        "Invalid source task ID"
    );
    Ok(())
}
fn valid_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty() && id.bytes().all(|c| c.is_ascii_digit()),
        "Workspace and target IDs must be numeric"
    );
    Ok(())
}
fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_owned()
}
fn required(v: &Value, key: &str) -> Result<String> {
    let s = text(v, key);
    ensure!(!s.is_empty(), "CLI response missing required field");
    Ok(s)
}
fn target_fields(v: &Value) -> Result<(&str, &Value)> {
    match (v.get("Task"), v.get("Story")) {
        (Some(t), None) => Ok(("task", t)),
        (None, Some(t)) => Ok(("story", t)),
        _ => bail!("TAPD response missing unambiguous resource kind"),
    }
}
// 缺字段不是清空成功的证据；只接受服务端明确返回的字符串或 null。
fn readback_text(v: &Value, key: &str) -> Result<String> {
    match v.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(Value::Null) => Ok(String::new()),
        _ => bail!("TAPD readback missing a synchronized field"),
    }
}
fn parse_target(v: &Value) -> Result<TargetTask> {
    let (kind, t) = target_fields(v)?;
    let id = required(t, "id")?;
    valid_id(&id)?;
    Ok(TargetTask {
        id,
        url: safe_url(&text(t, "url"), false),
        kind: kind.into(),
        title: required(t, "name")?,
        description: readback_text(t, "description")?,
        owner: readback_text(t, "owner")?,
        due: match readback_text(t, "due")? {
            s if s.is_empty() => None,
            s => Some(s),
        },
    })
}
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
/// 只允许浏览器安全打开产品 HTTPS 域名，绝不接受凭据、任意端口或相似后缀域名。
pub fn safe_url(raw: &str, lark: bool) -> String {
    let Ok(url) = url::Url::parse(raw) else {
        return String::new();
    };
    let hosts: &[&str] = if lark {
        &["feishu.cn", "larksuite.com"]
    } else {
        &["tapd.cn"]
    };
    let allowed = url.host_str().is_some_and(|host| {
        hosts
            .iter()
            .any(|base| host == *base || host.ends_with(&format!(".{base}")))
    });
    if url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && allowed
    {
        url.to_string()
    } else {
        String::new()
    }
}
#[cfg(test)]
mod tests;
