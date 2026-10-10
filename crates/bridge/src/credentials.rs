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
pub fn save_credentials(input: &Credentials) -> Result<()> {
    let mut saved = load_credentials()?;
    merge_nonempty(&mut saved, input)?;
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
pub fn save_credentials(_: &Credentials) -> Result<()> {
    anyhow::bail!("macOS Keychain is required")
}
fn merge_nonempty(saved: &mut Credentials, input: &Credentials) -> Result<()> {
    // 换应用必须显式清除旧凭据，不能把新 App ID 与旧密钥或令牌拼接。
    anyhow::ensure!(
        saved.lark_app_id.is_empty()
            || input.lark_app_id.is_empty()
            || saved.lark_app_id == input.lark_app_id,
        "Clear saved credentials before changing Lark App ID"
    );
    for (dst, src) in [
        (&mut saved.lark_app_id, &input.lark_app_id),
        (&mut saved.lark_app_secret, &input.lark_app_secret),
        (&mut saved.lark_user_token, &input.lark_user_token),
        (&mut saved.tapd_token, &input.tapd_token),
    ] {
        if !src.is_empty() {
            dst.zeroize();
            dst.clone_from(src);
        }
    }
    Ok(())
}
// 不存在的项视为已清除；不读取或回显 Keychain 内容。
#[cfg(target_os = "macos")]
pub fn clear_credentials() -> Result<()> {
    match security_framework::passwords::delete_generic_password(
        "io.lark2tapd.desktop",
        "credentials-v1",
    ) {
        Ok(()) => Ok(()),
        Err(e) if e.code() == -25300 => Ok(()),
        Err(_) => anyhow::bail!("Cannot clear credentials from macOS Keychain"),
    }
}
#[cfg(not(target_os = "macos"))]
pub fn clear_credentials() -> Result<()> {
    anyhow::bail!("macOS Keychain is required")
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
    fn empty_inputs_preserve_saved_secrets() {
        let mut saved = Credentials::default();
        saved.lark_app_secret = "keep".into();
        saved.tapd_token = "old".into();
        let mut input = Credentials::default();
        input.tapd_token = "new".into();
        merge_nonempty(&mut saved, &input).unwrap();
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
        assert!(merge_nonempty(&mut saved, &input).is_err());
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
