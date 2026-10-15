use serde_json::Value;

pub(crate) const TASK_SCOPE: &str = "task:task:read";
pub(crate) const PROFILE_HELP: &str = "Lark CLI 1.0.96 requires a matching native profile. Use Save and connect to add a Feishu profile, or lark-cli profile add --name <NEW_NAME> --app-id <APP_ID> --app-secret-stdin --brand feishu (or lark). Supply the secret through stdin, never argv. Existing profile secrets are not replaced: update an invalid native secret explicitly with lark-cli config init --name <EXISTING_NAME> --app-id <APP_ID> --app-secret-stdin --brand <BRAND>. Environment-only app credentials cannot authorize native OAuth.";

// 仅发布已知类别、整数 API code 和应用确实需要的 scope；忽略 message/hint/log_id。
pub(crate) fn api_failure(provider: &str, value: &Value, exit: Option<i32>) -> anyhow::Error {
    let error = value
        .get("error")
        .filter(|v| v.is_object())
        .unwrap_or(value);
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
    let mut text = format!("{provider} request failed ({category}");
    if let Some(exit) = exit {
        text.push_str(&format!(", exit {exit}"));
    }
    if let Some(code) = error
        .get("code")
        .or_else(|| value.get("code"))
        .and_then(Value::as_i64)
    {
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
        "config" => PROFILE_HELP,
        "authentication" => "; authorize the user again and check token expiry",
        "authorization" => "; check user consent, published app permissions and resource access",
        "network" => "; check network, DNS, TLS certificates and service availability",
        "validation" => "; check CLI version, arguments and native profile configuration",
        "policy" => "; resolve the account security policy or browser challenge",
        "confirmation" => "; explicit operation confirmation is required",
        "internal" => "; check native CLI storage access and supported version",
        _ => "; check authentication, configuration and remote service status",
    });
    anyhow::anyhow!(text)
}
