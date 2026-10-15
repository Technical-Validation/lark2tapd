mod connection;
mod credentials;
pub use connection::{ConnectionReport, DeviceAuthorization, PermissionCheck, PermissionState};
mod description;
mod diagnostics;
pub mod installer;
mod media;
mod process;
use anyhow::{Result, bail, ensure};
use chrono::{DateTime, Local, TimeZone, Utc};
pub use credentials::{
    Credentials, clear_credentials, credential_presence, credentials_namespace, load_credentials,
    save_credentials,
};
pub use description::render_description;
use lark2tapd_core::{
    Draft, Gateway, Settings, SourceMetadata, SourcePerson, SourceTask, TapdMember, TapdWorkspace,
    TargetTask,
};
pub use media::{MAX_INLINE_IMAGE_BYTES, validate_attachment_for_upload};
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
        // 独立连接测试只校验当前使用的平台；实际调用前仍严格校验路径与凭据。
        Ok(Self {
            settings,
            credentials,
        })
    }
    fn validate_provider(&self, lark: bool) -> Result<()> {
        let path = if lark {
            &self.settings.lark_path
        } else {
            &self.settings.tapd_path
        };
        ensure!(
            Path::new(path).is_absolute(),
            "Configure an absolute CLI executable path"
        );
        if lark {
            ensure!(
                (self.credentials.lark_app_secret.is_empty()
                    && self.credentials.lark_user_token.is_empty())
                    || !self.credentials.lark_app_id.is_empty(),
                "Lark App ID is required with a secret or user token"
            );
        } else {
            let endpoint = url::Url::parse(&self.settings.tapd_endpoint)
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
        }
        Ok(())
    }
    fn env(&self, lark: bool) -> Vec<(String, String)> {
        let c = &self.credentials;
        let pairs = if lark {
            // auth 与任务读取必须使用同一原生 profile；AppID/Secret 环境变量会切换 provider。
            vec![("LARKSUITE_CLI_PROFILE", c.lark_app_id.as_str())]
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
        self.validate_provider(lark)?;
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
    fn lark_until(&self, verb: &str, params: Value, deadline: Instant) -> Result<Value> {
        let v = self.invoke(
            true,
            [
                "task", "tasks", verb, "--as", "user", "--format", "json", "--params", "-",
            ]
            .map(String::from)
            .to_vec(),
            params,
            remaining(deadline)?,
        )?;
        if v.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(diagnostics::api_failure("Lark", &v, None));
        }
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
            matches!(kind, "task" | "story")
                || (kind == "user" && matches!(verb, "info" | "members" | "workspaces"))
                || (kind == "attachment" && matches!(verb, "upload" | "upload-image" | "list")),
            "Invalid TAPD resource kind"
        );
        let mut args = vec![kind.into(), verb.into()];
        args.extend(params);
        let v = self.invoke(false, args, Value::Null, Duration::from_secs(30))?;
        if v.get("status").and_then(Value::as_i64) != Some(1) {
            return Err(diagnostics::api_failure("TAPD", &v, None));
        }
        v.get("data")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("TAPD response missing data"))
    }
    pub fn check_lark(&self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(120);
        self.verified_self_until(deadline)?;
        // 只读一页验证最小任务权限，不依赖 TAPD 安装或项目配置。
        self.related_until("", deadline)?;
        Ok(())
    }
    pub fn check_tapd(&self) -> Result<()> {
        self.tapd_user_id()?;
        if !self.settings.workspace_id.is_empty() {
            valid_id(&self.settings.workspace_id)?;
            self.tapd(
                "story",
                "list",
                vec![
                    format!("workspaceid={}", self.settings.workspace_id),
                    "limit=1".into(),
                    "page=1".into(),
                ],
            )?;
        }
        Ok(())
    }
    /// 普通成员可显式输入项目 ID；成员读取成功才视为项目已验证。
    pub fn verify_workspace(&self, id: &str) -> Result<()> {
        self.list_members(id)?;
        Ok(())
    }
    /// 此接口仅列出管理员项目，不能当成当前用户所有可访问项目。
    pub fn list_workspaces(&self) -> Result<Vec<TapdWorkspace>> {
        let data = self.tapd("user", "workspaces", Vec::new())?;
        let items = data
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("TAPD response missing administrator workspace list"))?;
        // 父任务只读观察确认 Workspace 包装；不猜测其他接口的同名字段。
        items
            .iter()
            .map(|item| {
                let workspace = item
                    .get("Workspace")
                    .ok_or_else(|| anyhow::anyhow!("TAPD response missing Workspace"))?;
                let id = required(workspace, "id")?;
                valid_id(&id)?;
                Ok(TapdWorkspace {
                    id,
                    name: required(workspace, "name")?,
                })
            })
            .collect()
    }
    pub fn list_members(&self, workspace: &str) -> Result<Vec<TapdMember>> {
        valid_id(workspace)?;
        let data = self.tapd(
            "user",
            "members",
            vec![
                format!("workspaceid={workspace}"),
                "fields=user,name".into(),
            ],
        )?;
        let items = data
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("TAPD response missing member list"))?;
        // user 是后端接受的昵称账号，不是用户数字 ID；名字缺失时拒绝伪造显示名。
        items
            .iter()
            .map(|item| {
                let member = item
                    .get("UserWorkspace")
                    .ok_or_else(|| anyhow::anyhow!("TAPD response missing UserWorkspace"))?;
                Ok(TapdMember {
                    account: required(member, "user")?,
                    name: required(member, "name")?,
                })
            })
            .collect()
    }
    fn lark_identity(&self) -> Result<Value> {
        self.lark_identity_until(Instant::now() + Duration::from_secs(30))
    }
    fn lark_identity_until(&self, deadline: Instant) -> Result<Value> {
        // whoami 只诊断本地保存身份，不能证明注入令牌的主体；未知主体必须拒绝。
        ensure!(
            self.credentials.lark_user_token.is_empty(),
            "Cannot verify manual Lark user token identity; clear saved credentials and use CLI OAuth login"
        );
        let status = self.invoke(
            true,
            ["auth", "status", "--json", "--verify"]
                .map(String::from)
                .to_vec(),
            Value::Null,
            remaining(deadline)?,
        )?;
        self.validate_lark_profile(&status)?;
        Ok(status)
    }
    pub fn oauth_status(&self) -> Result<AuthStatus> {
        self.oauth_status_until(Instant::now() + Duration::from_secs(30))
    }
    fn oauth_status_until(&self, deadline: Instant) -> Result<AuthStatus> {
        let v = self.lark_identity_until(deadline)?;
        let user = v
            .pointer("/identities/user/openId")
            .and_then(Value::as_str)
            .unwrap_or("");
        let verified = v.get("verified").and_then(Value::as_bool) == Some(true)
            && v.get("identity").and_then(Value::as_str) == Some("user");
        Ok(AuthStatus {
            authenticated: verified && !user.trim().is_empty(),
            user_id: user.to_owned(),
        })
    }
    fn verified_self_until(&self, deadline: Instant) -> Result<String> {
        let status = self.oauth_status_until(deadline)?;
        ensure!(status.authenticated, "Lark user OAuth is not verified");
        Ok(status.user_id)
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
        let description = render_description(draft, source)?;
        let mut params = vec![
            format!("workspaceid={}", draft.workspace_id),
            format!("name={}", draft.rendered_title()),
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
            target.title == draft.rendered_title()
                // 已安装 CLI 会给纯 HTML 加一层 <p>；仅容忍这一精确包装。
                && (target.description == description
                    || target.description == format!("<p>{description}</p>"))
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
        let metadata = source_metadata(task)?;
        Ok(SourceTask {
            metadata,
            guid: guid.to_owned(),
            title: required(task, "summary")?,
            description: text(task, "description"),
            url: safe_url(&text(task, "url"), true),
            due,
            completed: text(task, "status") == "done"
                || !matches!(text(task, "completed_at").as_str(), "" | "0"),
        })
    }
    fn related_until(&self, page: &str, deadline: Instant) -> Result<Value> {
        let mut args: Vec<String> = [
            "task",
            "+get-related-tasks",
            "--as",
            "user",
            "--format",
            "json",
            "--page-limit",
            "1",
        ]
        .map(String::from)
        .to_vec();
        if !page.is_empty() {
            args.extend(["--page-token".into(), page.into()]);
        }
        let response = self.invoke(true, args, Value::Null, remaining(deadline)?)?;
        if response.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(diagnostics::api_failure("Lark", &response, None));
        }
        let data = response
            .get("data")
            .ok_or_else(|| anyhow::anyhow!("Lark response missing data"))?;
        ensure!(
            data.get("items").and_then(Value::as_array).is_some(),
            "Lark response missing task list"
        );
        let more = data
            .get("has_more")
            .and_then(Value::as_bool)
            .ok_or_else(|| anyhow::anyhow!("Lark response missing pagination status"))?;
        if more {
            required(data, "page_token")?;
        }
        Ok(data.clone())
    }
    fn fetch_tasks_until(&self, deadline: Instant) -> Result<Vec<SourceTask>> {
        // 每次刷新只验证一次 OAuth 主体；所有分页和详情共享同一个截止时间。
        let user = self.verified_self_until(deadline)?;
        let mut tasks = Vec::new();
        let mut page = String::new();
        let mut pages = HashSet::new();
        let mut ids = HashSet::new();
        let mut count = 0;
        for page_index in 0..40 {
            let data = self.related_until(&page, deadline)?;
            let items = data["items"]
                .as_array()
                .expect("validated related task list");
            count += items.len();
            ensure!(
                count <= 1000,
                "Lark refresh exceeds 1000 tasks; no partial results accepted"
            );
            for item in items {
                let guid = required(item, "guid")?;
                valid_source_id(&guid)?;
                // 快捷命令的日期已格式化，这里只看身份；原生详情才是元数据来源。
                let (creator, assignees) = source_people(item);
                let related = creator.as_ref().is_some_and(|p| p.id == user)
                    || assignees.iter().any(|p| p.id == user);
                if !related || !ids.insert(guid.clone()) {
                    continue;
                }
                let task = self.source_until(&guid, deadline)?;
                // 从列表到详情之间关系可能已移除，不能把旧列表关系当作授权。
                if task.is_related_to(&user) {
                    tasks.push(task);
                }
            }
            if data["has_more"] == false {
                return Ok(tasks);
            }
            page = required(&data, "page_token")?;
            ensure!(
                pages.insert(page.clone()) && page_index < 39,
                "Lark pagination did not finish within 40 pages; no partial results accepted"
            );
        }
        unreachable!("last page rejects continuation")
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
        valid_source_id(guid)?;
        let deadline = Instant::now() + Duration::from_secs(120);
        let user = self.verified_self_until(deadline)?;
        let task = self.source_until(guid, deadline)?;
        ensure!(
            task.is_related_to(&user),
            "Source task is no longer created by or assigned to the verified user"
        );
        Ok(task)
    }
    fn fetch_tasks(&self) -> Result<Vec<SourceTask>> {
        self.fetch_tasks_until(Instant::now() + Duration::from_secs(120))
    }
    fn list_workspaces(&self) -> Result<Vec<TapdWorkspace>> {
        CliGateway::list_workspaces(self)
    }
    fn list_members(&self, workspace: &str) -> Result<Vec<TapdMember>> {
        CliGateway::list_members(self, workspace)
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
// 普通调用最多 30 秒，整个刷新（含认证）最多 120 秒。
fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .map(|d| d.min(Duration::from_secs(30)))
        .ok_or_else(|| {
            anyhow::anyhow!("Lark refresh deadline exceeded; no partial results accepted")
        })
}
fn source_people(task: &Value) -> (Option<SourcePerson>, Vec<SourcePerson>) {
    let person = |v: &Value| {
        let id = text(v, "id");
        if id.trim().is_empty()
            || v.get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind != "user")
        {
            return None;
        }
        Some(SourcePerson {
            id,
            name: text(v, "name"),
        })
    };
    let creator = task.get("creator").and_then(person);
    let assignees = task
        .get("members")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|v| text(v, "role") == "assignee")
        .filter_map(person)
        .collect();
    (creator, assignees)
}
fn source_metadata(task: &Value) -> Result<SourceMetadata> {
    let (creator, assignees) = source_people(task);
    // 原生时间戳保留毫秒字符串（包括 completed_at="0"），不借用快捷命令的本地日期。
    let raw_time = |value: Option<&Value>| -> Result<Option<String>> {
        match value {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) if s.is_empty() => Ok(None),
            Some(Value::String(s))
                if s.bytes().all(|c| c.is_ascii_digit()) && s.parse::<i64>().is_ok() =>
            {
                Ok(Some(s.clone()))
            }
            _ => bail!("Invalid native Lark millisecond timestamp"),
        }
    };
    Ok(SourceMetadata {
        creator,
        assignees,
        created_at: raw_time(task.get("created_at"))?,
        completed_at: raw_time(task.get("completed_at"))?,
        due_at: raw_time(task.pointer("/due/timestamp"))?,
    })
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
    ensure!(!s.trim().is_empty(), "CLI response missing required field");
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
