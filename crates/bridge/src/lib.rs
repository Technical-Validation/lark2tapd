mod connection;
mod credentials;
pub use connection::{ConnectionReport, DeviceAuthorization, PermissionCheck, PermissionState};
mod description;
mod diagnostics;
pub mod installer;
mod media;
mod process;
mod read_pacing;
mod readback;
mod refresh;
mod source_conversation;
mod source_details;
pub mod source_images;
use anyhow::{Result, bail, ensure};
use chrono::{DateTime, Local, TimeZone, Utc};
pub use credentials::{
    Credentials, clear_credentials, credential_presence, credentials_namespace, load_credentials,
    save_credentials, test_credentials, verification_binding,
};
pub use description::{referenced_image_ids, render_description};
use lark2tapd_core::{
    Draft, Gateway, Settings, SourceMetadata, SourcePerson, SourceTask, TapdMember, TapdWorkspace,
    TargetTask,
};
pub use media::{MAX_INLINE_IMAGE_BYTES, validate_attachment_for_upload};
pub use process::{proxy_environment_present, with_process_cancellation};
pub use refresh::{RefreshEvent, RefreshFailure, RefreshOutcome, RefreshPhase};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    path::Path,
    sync::atomic::AtomicBool,
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
        ensure!(Path::new(path).is_absolute(), installer::CLI_PATH_ERROR);
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
    fn invoke_cancellable(
        &self,
        lark: bool,
        args: Vec<String>,
        input: Value,
        timeout: Duration,
        cancelled: &AtomicBool,
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
        let output = process::run_output_cancellable(
            Path::new(path),
            &args,
            &env,
            &bytes,
            timeout,
            cancelled,
        )?;
        if !output.status.success() {
            return Err(output.failure());
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|_| anyhow::anyhow!("CLI returned invalid JSON"))
    }
    fn lark_until(
        &self,
        verb: &str,
        params: Value,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Value> {
        let v = self.invoke_cancellable(
            true,
            [
                "task", "tasks", verb, "--as", "user", "--format", "json", "--params", "-",
            ]
            .map(String::from)
            .to_vec(),
            params,
            remaining(deadline)?,
            cancelled,
        )?;
        if v.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(diagnostics::api_failure("Lark", &v, None));
        }
        v.get("data")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Lark response missing data"))
    }
    fn tapd(&self, kind: &str, verb: &str, params: Vec<String>) -> Result<Value> {
        self.tapd_until(
            kind,
            verb,
            params,
            Instant::now() + Duration::from_secs(30),
            &AtomicBool::new(false),
        )
    }
    fn tapd_until(
        &self,
        kind: &str,
        verb: &str,
        params: Vec<String>,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Value> {
        // 不允许回退到 CLI 外部配置中的未知 TAPD 账户。
        ensure!(
            !self.credentials.tapd_token.is_empty(),
            "Configure a TAPD token in the app"
        );
        ensure!(
            matches!(kind, "task" | "story" | "bug")
                || (kind == "user" && matches!(verb, "info" | "members" | "workspaces"))
                || (kind == "attachment" && matches!(verb, "upload" | "upload-image" | "list")),
            "Invalid TAPD resource kind"
        );
        let mut args = vec![kind.into(), verb.into()];
        args.extend(params);
        let v =
            self.invoke_cancellable(false, args, Value::Null, remaining(deadline)?, cancelled)?;
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
        self.lark_identity_until_cancellable(deadline, &AtomicBool::new(false))
    }
    fn lark_identity_until_cancellable(
        &self,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Value> {
        // whoami 只诊断本地保存身份，不能证明注入令牌的主体；未知主体必须拒绝。
        ensure!(
            self.credentials.lark_user_token.is_empty(),
            "Cannot verify manual Lark user token identity; clear saved credentials and use CLI OAuth login"
        );
        let status = self.invoke_cancellable(
            true,
            ["auth", "status", "--json", "--verify"]
                .map(String::from)
                .to_vec(),
            Value::Null,
            remaining(deadline)?,
            cancelled,
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
        let (brand, app, user) = self.account_identity(&status)?;
        Ok(account_namespace_key(
            &brand,
            &app,
            &user,
            &self.tapd_user_id()?,
        ))
    }
    fn account_identity(&self, status: &Value) -> Result<(String, String, String)> {
        ensure!(
            status.get("verified").and_then(Value::as_bool) == Some(true)
                && status.get("identity").and_then(Value::as_str) == Some("user"),
            "Cannot verify Lark account identity; synchronization is locked"
        );
        let app = required(status, "appId")?;
        let brand = required(status, "brand")?;
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
        Ok((brand, app, user.to_owned()))
    }
    // 字段接口返回此项目真实可用类型；不依赖 CLI 的自动 task 类型猜测。
    pub fn list_workitem_types(
        &self,
        workspace: &str,
    ) -> Result<Vec<lark2tapd_core::TapdWorkitemType>> {
        valid_id(workspace)?;
        let mut items = Vec::new();
        for kind in ["story", "bug"] {
            let fields = self.tapd(kind, "fields", vec![format!("workspaceid={workspace}")])?;
            ensure!(fields.is_object(), "TAPD fields are unavailable");
            // 传统 Bug 没有需求类型字段；有该字段的项目仍使用服务端真实选项。
            if kind == "bug" && fields.get("workitem_type_id").is_none() {
                ensure!(
                    fields.get("title").is_some(),
                    "TAPD bug fields are unavailable"
                );
                items.push(lark2tapd_core::TapdWorkitemType {
                    kind: kind.into(),
                    id: String::new(),
                    name: "Bug".into(),
                });
                continue;
            }
            let options = fields
                .pointer("/workitem_type_id/options")
                .and_then(Value::as_object)
                .ok_or_else(|| anyhow::anyhow!("TAPD work item type options are unavailable"))?;
            for (id, name) in options {
                valid_id(id)?;
                let name = name
                    .as_str()
                    .filter(|name| !name.trim().is_empty())
                    .ok_or_else(|| anyhow::anyhow!("TAPD type has no display name"))?;
                items.push(lark2tapd_core::TapdWorkitemType {
                    kind: kind.into(),
                    id: id.clone(),
                    name: name.into(),
                });
            }
        }
        Ok(items)
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
        // 已绑定目标身份优先于编辑器当前选择，回写图片也走同一字段映射。
        let kind = old.map_or(draft.workitem_kind.as_str(), |target| target.kind.as_str());
        let (title_field, owner_field, due_field) = synchronized_fields(kind)?;
        let mut params = vec![
            format!("workspaceid={}", draft.workspace_id),
            format!("{title_field}={}", draft.rendered_title()),
            format!("description={description}"),
            format!("{owner_field}={}", draft.owner),
            format!("{due_field}={}", draft.due.as_deref().unwrap_or("")),
        ];
        let verb = if let Some(old) = old {
            valid_id(&old.id)?;
            params.push(format!("id={}", old.id));
            "update"
        } else {
            ensure!(matches!(kind, "story" | "bug"), "Unsupported creation kind");
            if kind == "story" || !draft.workitem_type_id.is_empty() {
                valid_id(&draft.workitem_type_id).map_err(|_| {
                    anyhow::anyhow!("Select a TAPD work item type before importing")
                })?;
                params.push(format!("workitem_type_id={}", draft.workitem_type_id));
            }
            "add"
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
        let expected_type = old.map_or(draft.workitem_type_id.as_str(), |target| {
            target.workitem_type_id.as_str()
        });
        let receipt = TargetTask {
            workitem_type_id: expected_type.to_owned(),
            id: id.clone(),
            kind: result_kind.into(),
            url: String::new(),
            title: draft.rendered_title(),
            description: description.clone(),
            owner: draft.owner.clone(),
            due: draft.due.clone(),
        };
        // 返回了另一种资源也不能当作成功，更不能丢掉已知 ID 后重新创建。
        if result_kind != kind {
            return Err(anyhow::Error::new(lark2tapd_core::UnconfirmedWrite {
                target: receipt,
                message: "TAPD write returned a different resource kind; verify the recorded target before updating".into(),
            }));
        }
        let target = self.get_target(&draft.workspace_id, &id, result_kind).map_err(|error| anyhow::Error::new(lark2tapd_core::UnconfirmedWrite {
            target: receipt, message: format!("TAPD write returned a target ID but readback failed: {error}; verify the recorded target before updating"),
        }))?;
        // 仅容忍无损的 HTML/成员列表序列化差异；图片、正文和目标字段丢失仍不确认成功。
        let differences: Vec<_> = [
            (target.title != draft.rendered_title(), "title"),
            (
                !expected_type.is_empty() && target.workitem_type_id != expected_type,
                "workitem_type_id",
            ),
            (
                !readback::equivalent_html(&description, &target.description),
                "description",
            ),
            (
                !readback::equivalent_owner(&draft.owner, &target.owner),
                "owner",
            ),
            (target.due != draft.due, "due"),
        ]
        .into_iter()
        .filter_map(|(different, field)| different.then_some(field))
        .collect();
        if !differences.is_empty() {
            return Err(anyhow::Error::new(lark2tapd_core::UnconfirmedWrite {
                target,
                message: format!(
                    "TAPD readback differs in {}; target ID retained, write remains uncertain. Verify before updating; do not create again",
                    differences.join(", ")
                ),
            }));
        }
        Ok(target)
    }
}
impl CliGateway {
    fn source_until(&self, guid: &str, deadline: Instant) -> Result<SourceTask> {
        self.source_until_cancellable(guid, deadline, &AtomicBool::new(false))
    }
    fn source_until_cancellable(
        &self,
        guid: &str,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<SourceTask> {
        valid_source_id(guid)?;
        let full = self.lark_until(
            "get",
            json!({"task_guid":guid,"user_id_type":"open_id"}),
            deadline,
            cancelled,
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
            metadata: Box::new(metadata),
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
        self.related_until_cancellable(page, deadline, &AtomicBool::new(false))
    }
    fn related_until_cancellable(
        &self,
        page: &str,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Value> {
        let mut args: Vec<String> = [
            "task",
            "+get-related-tasks",
            "--as",
            "user",
            "--format",
            "json",
            "--page-limit",
            "1",
            // CLI 将此条件传给服务端；避免枚举已完成历史任务再逐条取详情。
            "--include-complete=false",
        ]
        .map(String::from)
        .to_vec();
        if !page.is_empty() {
            args.extend(["--page-token".into(), page.into()]);
        }
        let response =
            self.invoke_cancellable(true, args, Value::Null, remaining(deadline)?, cancelled)?;
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
    // +search 会串行补读每个详情；只取原生搜索候选，再交给共享的限流/并发详情读取。
    fn search_tasks_until(
        &self,
        user: &str,
        role: &str,
        page: &str,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Value> {
        let params = if page.is_empty() {
            json!({})
        } else {
            json!({"page_token": page})
        };
        let response = self.invoke_cancellable(
            true,
            vec![
                "api".into(),
                "POST".into(),
                "/open-apis/task/v2/tasks/search".into(),
                "--as".into(),
                "user".into(),
                "--format".into(),
                "json".into(),
                "--params".into(),
                params.to_string(),
                "--data".into(),
                "-".into(),
            ],
            json!({"query":"", "filter":{(role):[user], "is_completed":false}}),
            remaining(deadline)?,
            cancelled,
        )?;
        if response.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(diagnostics::api_failure("Lark", &response, None));
        }
        let data = response
            .get("data")
            .ok_or_else(|| anyhow::anyhow!("Lark response missing search data"))?;
        ensure!(
            data.get("items").and_then(Value::as_array).is_some(),
            "Lark response missing search results"
        );
        let more = data
            .get("has_more")
            .and_then(Value::as_bool)
            .ok_or_else(|| anyhow::anyhow!("Lark response missing pagination status"))?;
        if more {
            required(data, "page_token")?;
        }
        // 搜索服务的非空 notice 可能表示范围受限，不能拿不完整结果替换缓存。
        ensure!(
            data.get("notice")
                .is_none_or(|notice| notice.is_null() || notice.as_str() == Some("")),
            "Lark search returned a notice; result completeness could not be confirmed"
        );
        Ok(data.clone())
    }

    fn fetch_tasks_until(&self, deadline: Instant) -> Result<Vec<SourceTask>> {
        refresh::read(self, deadline, &AtomicBool::new(false), false, &mut |_| {})
            .map(|(_, tasks)| tasks)
    }
}
impl Gateway for CliGateway {
    fn list_workitem_types(
        &self,
        workspace: &str,
    ) -> Result<Vec<lark2tapd_core::TapdWorkitemType>> {
        CliGateway::list_workitem_types(self, workspace)
    }
    // 本地错误在写日志之前拒绝；直接调用写入口也复用相同规则。
    fn validate_draft(&self, draft: &Draft) -> Result<()> {
        valid_id(&draft.workspace_id).map_err(|_| {
            anyhow::anyhow!("Select a valid numeric TAPD workspace before submitting")
        })?;
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
        let mut task = self.source_until(guid, deadline)?;
        ensure!(
            task.is_related_to(&user),
            "Source task is no longer created by or assigned to the verified user"
        );
        self.enrich_sources(
            std::slice::from_mut(&mut task),
            deadline,
            &AtomicBool::new(false),
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
        valid_id(workspace).map_err(|_| {
            anyhow::anyhow!("Invalid TAPD workspace ID; select the intended workspace")
        })?;
        valid_id(id).map_err(|_| {
            anyhow::anyhow!(
                "Invalid TAPD target ID; enter the complete numeric item ID, not a title or URL"
            )
        })?;
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
        let mut target = parse_target(&items[0])?;
        target.url = target_url(&target, workspace);
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
fn account_namespace_key(brand: &str, app: &str, user: &str, tapd_user: &str) -> String {
    credentials::namespace_parts(&[
        "verified-account-v2",
        "lark",
        brand,
        app,
        user,
        "https://api.tapd.cn",
        tapd_user,
    ])
}
// 普通调用最多 30 秒，整个刷新（含认证）最多 120 秒。
fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .map(|d| d.min(Duration::from_secs(30)))
        .ok_or_else(|| anyhow::Error::new(refresh::BudgetExceeded))
}
// 人员身份校验同时用于列表范围判定和详情展示；关注人不参与本人任务范围。
fn source_person(value: &Value) -> Option<SourcePerson> {
    let id = text(value, "id");
    if id.trim().is_empty()
        || value
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind != "user")
    {
        return None;
    }
    Some(SourcePerson {
        id,
        name: text(value, "name"),
    })
}
fn source_members(task: &Value, role: &str) -> Vec<SourcePerson> {
    task.get("members")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|value| text(value, "role") == role)
        .filter_map(source_person)
        .collect()
}
fn source_people(task: &Value) -> (Option<SourcePerson>, Vec<SourcePerson>) {
    (
        task.get("creator").and_then(source_person),
        source_members(task, "assignee"),
    )
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
        followers: source_members(task, "follower"),
        // 任务 description 可能为空，消息正文在来源资源中，不能因此丢失。
        // 未能解码的消息仍保留一个位置，由界面提示去飞书查看。
        source_messages: task
            .pointer("/origin/refer_resources")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|resource| {
                resource.get("source_message").is_some()
                    || matches!(text(resource, "type").as_str(), "message" | "1")
            })
            .map(|resource| lark2tapd_core::SourceMessage {
                message_id: resource
                    .get("source_message")
                    .map(|message| text(message, "message_id"))
                    .unwrap_or_default(),
                content: resource
                    .get("source_message")
                    .map(|message| text(message, "content"))
                    .unwrap_or_default(),
            })
            .collect(),
        updated_at: raw_time(task.get("updated_at"))?,
        created_at: raw_time(task.get("created_at"))?,
        completed_at: raw_time(task.get("completed_at"))?,
        due_at: raw_time(task.pointer("/due/timestamp"))?,
        ..Default::default()
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
    match (v.get("Task"), v.get("Story"), v.get("Bug")) {
        (Some(t), None, None) => Ok(("task", t)),
        (None, Some(t), None) => Ok(("story", t)),
        (None, None, Some(t)) => Ok(("bug", t)),
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
fn synchronized_fields(kind: &str) -> Result<(&'static str, &'static str, &'static str)> {
    match kind {
        "bug" => Ok(("title", "current_owner", "deadline")),
        "story" | "task" => Ok(("name", "owner", "due")),
        _ => bail!("Unsupported target kind"),
    }
}
fn parse_target(v: &Value) -> Result<TargetTask> {
    let (kind, t) = target_fields(v)?;
    let (title_field, owner_field, due_field) = synchronized_fields(kind)?;
    let id = required(t, "id")?;
    valid_id(&id)?;
    Ok(TargetTask {
        workitem_type_id: text(t, "workitem_type_id"),
        id,
        url: safe_url(&text(t, "url"), false),
        kind: kind.into(),
        title: required(t, title_field)?,
        description: readback_text(t, "description")?,
        owner: readback_text(t, owner_field)?,
        due: match readback_text(t, due_field)? {
            s if s.is_empty() => None,
            s => Some(s),
        },
    })
}
/// CLI 常省略 URL；旧缓存也按持久化的项目、类型和 ID 恢复入口，不依赖标题。
pub fn target_url(target: &TargetTask, workspace: &str) -> String {
    let provided = safe_url(&target.url, false);
    if !provided.is_empty() {
        return provided;
    }
    if valid_id(workspace).is_err() || valid_id(&target.id).is_err() {
        return String::new();
    }
    let route = match target.kind.as_str() {
        "task" => "tasks",
        "story" => "stories",
        "bug" => "bugs",
        _ => return String::new(),
    };
    format!(
        "https://www.tapd.cn/{workspace}/prong/{route}/view/{}",
        target.id
    )
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
