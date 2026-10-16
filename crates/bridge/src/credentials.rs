use crate::installer::CliTool;
use anyhow::Result;
use zeroize::{Zeroize, ZeroizeOnDrop};
#[derive(Clone, Default, serde::Serialize, serde::Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct Credentials {
    pub lark_app_id: String,
    pub lark_app_secret: String,
    pub lark_user_token: String,
    pub tapd_token: String,
}
impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credentials([REDACTED])")
    }
}
// 只持久化一个 Keychain 项，避免部分字段写入成功后产生混合账户。
#[cfg(target_os = "macos")]
pub fn load_credentials() -> Result<Credentials> {
    match security_framework::passwords::get_generic_password(
        "io.lark2tapd.desktop",
        "credentials-v1",
    ) {
        Ok(bytes) => {
            let bytes = zeroize::Zeroizing::new(bytes);
            serde_json::from_slice(&bytes)
                .map_err(|_| anyhow::anyhow!("Stored credentials could not be decoded"))
        }
        Err(e) if e.code() == -25300 => Ok(Credentials::default()),
        Err(_) => anyhow::bail!("Cannot read credentials from macOS Keychain"),
    }
}
#[cfg(target_os = "macos")]
fn store_credentials(saved: &Credentials) -> Result<()> {
    let bytes = zeroize::Zeroizing::new(
        serde_json::to_vec(&saved).map_err(|_| anyhow::anyhow!("Cannot encode credentials"))?,
    );
    security_framework::passwords::set_generic_password(
        "io.lark2tapd.desktop",
        "credentials-v1",
        &bytes,
    )
    .map_err(|_| anyhow::anyhow!("Cannot save credentials in macOS Keychain"))
}
#[cfg(not(target_os = "macos"))]
pub fn load_credentials() -> Result<Credentials> {
    anyhow::bail!("macOS Keychain is required")
}
#[cfg(not(target_os = "macos"))]
fn store_credentials(_: &Credentials) -> Result<()> {
    anyhow::bail!("macOS Keychain is required")
}
pub fn save_credentials(tool: CliTool, input: &Credentials) -> Result<()> {
    let mut saved = load_credentials()?;
    merge_nonempty(&mut saved, tool, input)?;
    store_credentials(&saved)
}
fn merge_nonempty(saved: &mut Credentials, tool: CliTool, input: &Credentials) -> Result<()> {
    if tool == CliTool::Tapd {
        replace_nonempty(&mut saved.tapd_token, &input.tapd_token);
        return Ok(());
    }
    // 换应用必须清除旧凭据；不能将新应用与旧密钥拼接。
    anyhow::ensure!(
        input.lark_app_id.is_empty()
            || saved.lark_app_id == input.lark_app_id
            || (saved.lark_app_id.is_empty()
                && saved.lark_app_secret.is_empty()
                && saved.lark_user_token.is_empty()),
        "Clear saved credentials before changing Lark App ID"
    );
    anyhow::ensure!(
        input.lark_app_id.is_empty() == input.lark_app_secret.is_empty(),
        "Enter Lark App ID and App Secret together"
    );
    for (dst, src) in [
        (&mut saved.lark_app_id, &input.lark_app_id),
        (&mut saved.lark_app_secret, &input.lark_app_secret),
        (&mut saved.lark_user_token, &input.lark_user_token),
    ] {
        replace_nonempty(dst, src);
    }
    Ok(())
}
fn replace_nonempty(dst: &mut String, src: &str) {
    if !src.is_empty() {
        dst.zeroize();
        dst.push_str(src);
    }
}
pub fn clear_credentials(tool: CliTool) -> Result<()> {
    let mut saved = load_credentials()?;
    clear_service(&mut saved, tool);
    store_credentials(&saved)
}
fn clear_service(saved: &mut Credentials, tool: CliTool) {
    match tool {
        CliTool::Lark => {
            saved.lark_app_id.zeroize();
            saved.lark_app_secret.zeroize();
            saved.lark_user_token.zeroize();
        }
        CliTool::Tapd => saved.tapd_token.zeroize(),
    }
}
pub fn credential_presence(saved: &Credentials) -> (bool, bool) {
    (
        !saved.lark_app_id.is_empty()
            || !saved.lark_app_secret.is_empty()
            || !saved.lark_user_token.is_empty(),
        !saved.tapd_token.is_empty(),
    )
}
/// 只在内存中合并测试输入，不保存、不创建原生 profile。
pub fn test_credentials(
    saved: &Credentials,
    tool: CliTool,
    input: &Credentials,
) -> Result<Credentials> {
    if tool == CliTool::Lark {
        anyhow::ensure!(
            (input.lark_app_id.is_empty() || input.lark_app_id == saved.lark_app_id)
                && (input.lark_app_secret.is_empty()
                    || input.lark_app_secret == saved.lark_app_secret),
            "Unsaved Lark app credentials cannot be verified against an old native profile; use Save and connect first"
        );
    }
    let mut effective = saved.clone();
    merge_nonempty(&mut effective, tool, input)?;
    Ok(effective)
}

/// 单服务配置指纹不含凭证原文；历史时间绝不作为授权依据。
pub fn verification_binding(
    tool: CliTool,
    settings: &lark2tapd_core::Settings,
    c: &Credentials,
) -> String {
    match tool {
        CliTool::Lark => namespace_parts(&[
            "lark",
            &settings.lark_path,
            &c.lark_app_id,
            &c.lark_app_secret,
            &c.lark_user_token,
        ]),
        CliTool::Tapd => namespace_parts(&[
            "tapd",
            &settings.tapd_path,
            &settings.tapd_endpoint,
            &settings.workspace_id,
            &c.tapd_token,
        ]),
    }
}

pub fn credentials_namespace(c: &Credentials) -> String {
    namespace_parts(&[
        &c.lark_app_id,
        &c.lark_app_secret,
        &c.lark_user_token,
        &c.tapd_token,
    ])
}
pub(crate) fn namespace_parts(parts: &[&str]) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    format!("{:x}", hash.finalize())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clearing_one_service_preserves_the_other_and_presence_counts_any_field() {
        let mut saved = Credentials::default();
        saved.lark_user_token = "manual-token".into();
        saved.tapd_token = "tapd-token".into();
        assert_eq!(credential_presence(&saved), (true, true));
        clear_service(&mut saved, CliTool::Lark);
        assert_eq!(credential_presence(&saved), (false, true));
        assert_eq!(saved.tapd_token, "tapd-token");
        saved.lark_app_secret = "secret-only".into();
        clear_service(&mut saved, CliTool::Tapd);
        assert_eq!(credential_presence(&saved), (true, false));
        assert_eq!(saved.lark_app_secret, "secret-only");
    }
    #[test]
    fn lark_form_cannot_change_tapd_and_partial_pairs_are_rejected() {
        let mut saved = Credentials::default();
        saved.tapd_token = "keep-tapd".into();
        let mut input = Credentials::default();
        input.lark_app_id = "app".into();
        input.tapd_token = "unrelated".into();
        assert!(merge_nonempty(&mut saved, CliTool::Lark, &input).is_err());
        assert!(saved.lark_app_id.is_empty());
        input.lark_app_secret = "secret".into();
        merge_nonempty(&mut saved, CliTool::Lark, &input).unwrap();
        assert_eq!(saved.tapd_token, "keep-tapd");
        assert_eq!(saved.lark_app_id, "app");
        assert_eq!(saved.lark_app_secret, "secret");
        input.lark_app_id.clear();
        assert!(merge_nonempty(&mut saved, CliTool::Lark, &input).is_err());
    }
    #[test]
    fn rejects_pairing_new_app_id_with_a_previously_orphaned_secret() {
        let mut saved = Credentials::default();
        saved.lark_app_secret = "old-secret".into();
        let mut input = Credentials::default();
        input.lark_app_id = "new-app".into();
        assert!(merge_nonempty(&mut saved, CliTool::Lark, &input).is_err());
        assert!(saved.lark_app_id.is_empty());
    }
    #[test]
    fn tapd_form_cannot_change_lark_credentials() {
        let mut saved = Credentials::default();
        saved.lark_app_id = "old-app".into();
        saved.lark_app_secret = "old-secret".into();
        let mut input = Credentials::default();
        input.lark_app_secret = "unrelated".into();
        input.tapd_token = "new-token".into();
        merge_nonempty(&mut saved, CliTool::Tapd, &input).unwrap();
        assert_eq!(saved.lark_app_secret, "old-secret");
        assert_eq!(saved.tapd_token, "new-token");
    }
    #[test]
    fn draft_test_does_not_reuse_a_different_lark_profile_or_mutate_saved_tokens() {
        let mut saved = Credentials::default();
        saved.lark_app_id = "app".into();
        saved.lark_app_secret = "old".into();
        saved.tapd_token = "saved".into();
        let mut input = Credentials::default();
        input.tapd_token = "draft".into();
        let effective = test_credentials(&saved, CliTool::Tapd, &input).unwrap();
        assert_eq!(effective.tapd_token, "draft");
        assert_eq!(saved.tapd_token, "saved");
        input.lark_app_id = "app".into();
        input.lark_app_secret = "new".into();
        assert!(
            test_credentials(&saved, CliTool::Lark, &input)
                .unwrap_err()
                .to_string()
                .contains("Save and connect")
        );
        let settings = lark2tapd_core::Settings::default();
        assert_eq!(
            verification_binding(CliTool::Lark, &settings, &saved),
            verification_binding(CliTool::Lark, &settings, &effective)
        );
        assert_ne!(
            verification_binding(CliTool::Tapd, &settings, &saved),
            verification_binding(CliTool::Tapd, &settings, &effective)
        );
    }
    #[test]
    fn empty_inputs_preserve_saved_secrets() {
        let mut saved = Credentials::default();
        saved.lark_app_secret = "keep".into();
        saved.tapd_token = "old".into();
        let mut input = Credentials::default();
        input.tapd_token = "new".into();
        merge_nonempty(&mut saved, CliTool::Tapd, &input).unwrap();
        assert_eq!(saved.lark_app_secret, "keep");
        assert_eq!(saved.tapd_token, "new");
    }
    #[test]
    fn changing_app_requires_clear_before_merging_secrets() {
        let mut saved = Credentials::default();
        saved.lark_app_id = "old-app".into();
        saved.lark_app_secret = "old-secret".into();
        let mut input = Credentials::default();
        input.lark_app_id = "new-app".into();
        assert!(merge_nonempty(&mut saved, CliTool::Lark, &input).is_err());
        assert_eq!(saved.lark_app_id, "old-app");
        assert_eq!(saved.lark_app_secret, "old-secret");
    }
    #[test]
    fn credentials_are_redacted_and_namespace_separates_accounts() {
        let mut a = Credentials::default();
        a.lark_app_id = "a".into();
        a.lark_user_token = "secret".into();
        let mut b = a.clone();
        b.lark_app_id = "b".into();
        assert!(!format!("{a:?}").contains("secret"));
        assert_ne!(credentials_namespace(&a), credentials_namespace(&b));
        assert_eq!(credentials_namespace(&a).len(), 64);
        assert_ne!(namespace_parts(&["a", "bc"]), namespace_parts(&["ab", "c"]));
    }
}
