use serde_json::Value;

pub(crate) const TASK_SCOPE: &str = "task:task:read";
pub(crate) const PROFILE_HELP: &str = "Lark CLI 1.0.96 requires a matching native profile. Use Save and connect to add a Feishu profile, or lark-cli profile add --name <NEW_NAME> --app-id <APP_ID> --app-secret-stdin --brand feishu (or lark). Supply the secret through stdin, never argv. Existing profile secrets are not replaced: update an invalid native secret explicitly with lark-cli config init --name <EXISTING_NAME> --app-id <APP_ID> --app-secret-stdin --brand <BRAND>. Environment-only app credentials cannot authorize native OAuth.";

// 保留机器可读限流信息，重试不得依赖错误文案或远端 message/hint。
#[derive(Debug)]
pub(crate) struct ApiFailure {
    pub code: Option<i64>,
    pub retry_after: Option<std::time::Duration>,
    message: String,
}
impl std::fmt::Display for ApiFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ApiFailure {}

// 仅发布已知类别、整数 API code 和应用确实需要的 scope；忽略 message/hint/log_id。
pub(crate) fn api_failure(provider: &str, value: &Value, exit: Option<i32>) -> anyhow::Error {
    let error = value
        .get("error")
        .filter(|v| v.is_object())
        .unwrap_or(value);
    let code = error
        .get("code")
        .or_else(|| value.get("code"))
        .and_then(Value::as_i64);
    // CLI 1.0.96 的 APIError 可携带来自服务端响应头的 retry_after_seconds。
    let retry_after = error
        .get("retry_after_seconds")
        .and_then(Value::as_u64)
        .filter(|seconds| *seconds > 0)
        .map(std::time::Duration::from_secs);
    let category = error
        .get("type")
        .and_then(Value::as_str)
        .filter(|v| {
            matches!(
                *v,
                "validation"
                    | "authentication"
                    | "authorization"
                    | "config"
                    | "network"
                    | "api"
                    | "policy"
                    | "internal"
                    | "confirmation"
            )
        })
        .unwrap_or("unknown");
    let category = if code == Some(99991400) {
        "rate_limit"
    } else {
        category
    };
    let mut text = format!("{provider} request failed ({category}");
    if let Some(exit) = exit {
        text.push_str(&format!(", exit {exit}"));
    }
    if let Some(code) = code {
        text.push_str(&format!(", API code {code}"));
    }
    text.push(')');
    if ["missing_scopes", "missing", "required_scopes"]
        .iter()
        .any(|key| {
            error
                .get(key)
                .or_else(|| value.get(key))
                .and_then(Value::as_array)
                .is_some_and(|scopes| scopes.iter().any(|s| s.as_str() == Some(TASK_SCOPE)))
        })
    {
        text.push_str("; required scope: task:task:read");
    }
    text.push_str(match category {
        "rate_limit" => "; request frequency limit reached; wait before retrying reads",
        "config" => PROFILE_HELP,
        "authentication" => "; authorize the user again and check token expiry",
        "authorization" => "; check user consent, published app permissions and resource access",
        "network" => "; check network, DNS, TLS certificates and service availability",
        "validation" => "; check CLI version, arguments and native profile configuration",
        "policy" => "; resolve the account security policy or browser challenge",
        "confirmation" => "; explicit operation confirmation is required",
        "internal" => "; check native CLI storage access and supported version",
        _ => "; inspect the API code and remote service status before retrying",
    });
    ApiFailure {
        code,
        retry_after,
        message: text,
    }
    .into()
}
