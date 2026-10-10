use super::{CliGateway, valid_id};
use anyhow::{Result, ensure};
use lark2tapd_core::{DraftAttachment, TargetTask, read_attachment};
use serde_json::Value;
use std::{io::Write, path::Path};

/// 对文档中未明确二进制单位的 5 MB 采用更保守的十进制上限。
pub const MAX_INLINE_IMAGE_BYTES: usize = 5_000_000;

/// 任务首次写入前的纯本地预检。调用方仍须以实际账户的根目录执行 core 校验。
pub fn validate_attachment_for_upload(attachment: &DraftAttachment) -> Result<()> {
    let root = Path::new(&attachment.path)
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| anyhow::anyhow!("Invalid attachment cache path"))?;
    checked_bytes(root, attachment).map(|_| ())
}

fn checked_bytes(root: &Path, a: &DraftAttachment) -> Result<Vec<u8>> {
    ensure!(
        !a.name.is_empty()
            && a.name != "."
            && a.name != ".."
            && !a
                .name
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\')),
        "Invalid attachment name"
    );
    ensure!(
        !a.uncertain,
        "Attachment upload result is uncertain; reconcile manually before retry"
    );
    let bytes = read_attachment(root, a)
        .map_err(|_| anyhow::anyhow!("Attachment cache validation failed"))?;
    if a.is_image {
        ensure!(
            a.size < MAX_INLINE_IMAGE_BYTES as u64,
            "TAPD image must be smaller than 5 MB"
        );
        ensure!(
            bytes.starts_with(b"\x89PNG\r\n\x1a\n")
                || bytes.starts_with(b"\xff\xd8\xff")
                || bytes.starts_with(b"GIF87a")
                || bytes.starts_with(b"GIF89a")
                || bytes.starts_with(b"BM"),
            "TAPD supports PNG, GIF, JPEG and BMP images only"
        );
    } else {
        // 核心目前还有更严格的 20 MiB 本地缓存上限；不能绕过它。
        ensure!(
            a.size < 250 * 1024 * 1024,
            "TAPD attachment must be smaller than 250 MiB"
        );
    }
    Ok(bytes)
}

/// 官方 upload_image 返回 TAPD 内部路径；不得猜测外部域名或信任 html_code。
pub(crate) fn safe_image_src(raw: &str) -> bool {
    if raw.chars().any(char::is_control) {
        return false;
    }
    if let Some(path) = raw.strip_prefix("/tfl/pictures/") {
        return !path.is_empty()
            && path.split('/').all(|s| {
                !s.is_empty()
                    && s != "."
                    && s != ".."
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            });
    }
    !super::safe_url(raw, false).is_empty()
}

// JS CLI 会解析 JSON 数字；超过安全整数的父项 ID 不能作为精确身份回执。
fn receipt_id(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(s) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => {
            Some(s.clone())
        }
        Value::Number(n) => n
            .as_u64()
            .filter(|n| *n <= 9_007_199_254_740_991)
            .map(|n| n.to_string()),
        _ => None,
    }
}
// 非精确数字只用于识别“可能是同一 ID”，绝不能单独作为成功证明。
fn no_identity_conflict(value: Option<&Value>, expected: &str) -> bool {
    match value {
        None => true,
        Some(Value::Number(n)) if n.as_u64().is_some_and(|n| n > 9_007_199_254_740_991) => {
            n.as_f64() == expected.parse::<f64>().ok()
        }
        _ => receipt_id(value).as_deref() == Some(expected),
    }
}
impl CliGateway {
    fn verify_uploaded_file(
        &self,
        data: &Value,
        workspace: &str,
        target: &TargetTask,
    ) -> Result<String> {
        let fields = data
            .get("Attachment")
            .ok_or_else(|| anyhow::anyhow!("Missing attachment receipt"))?;
        let id =
            receipt_id(fields.get("id")).ok_or_else(|| anyhow::anyhow!("Missing attachment ID"))?;
        ensure!(
            no_identity_conflict(fields.get("workspace_id"), workspace)
                && no_identity_conflict(fields.get("entry_id"), &target.id)
                && fields
                    .get("type")
                    .is_none_or(|v| v.as_str() == Some(target.kind.as_str())),
            "Attachment identity mismatch"
        );
        let exact = receipt_id(fields.get("workspace_id")).as_deref() == Some(workspace)
            && receipt_id(fields.get("entry_id")).as_deref() == Some(target.id.as_str())
            && fields.get("type").and_then(Value::as_str) == Some(target.kind.as_str());
        if !exact {
            // 官方 GET /attachments 支持这四个精确筛选；entry_id 必须用下划线兼容旧 CLI。
            let rows = self.tapd(
                "attachment",
                "list",
                vec![
                    format!("workspaceid={workspace}"),
                    format!("entry_id={}", target.id),
                    format!("id={id}"),
                    format!("type={}", target.kind),
                    "limit=2".into(),
                    "page=1".into(),
                ],
            )?;
            let rows = rows
                .as_array()
                .filter(|rows| rows.len() == 1)
                .ok_or_else(|| anyhow::anyhow!("Attachment readback missing or ambiguous"))?;
            let row = rows[0]
                .get("Attachment")
                .ok_or_else(|| anyhow::anyhow!("Missing attachment readback"))?;
            ensure!(
                receipt_id(row.get("id")).as_deref() == Some(id.as_str())
                    && receipt_id(row.get("workspace_id")).as_deref() == Some(workspace)
                    && row.get("type").and_then(Value::as_str) == Some(target.kind.as_str())
                    && no_identity_conflict(row.get("entry_id"), &target.id),
                "Attachment readback identity mismatch"
            );
        }
        Ok(id)
    }
    /// 只发起一次上传。任何远端错误或缺少证明都返回不确定状态，由 actor 保留日志。
    pub fn upload_draft_attachment(
        &self,
        media_root: &Path,
        workspace: &str,
        target: &TargetTask,
        attachment: &DraftAttachment,
    ) -> Result<DraftAttachment> {
        valid_id(workspace)?;
        valid_id(&target.id)?;
        ensure!(
            matches!(target.kind.as_str(), "story" | "task"),
            "Invalid attachment target kind"
        );
        ensure!(
            attachment.remote_id.is_none() && attachment.remote_url.is_none(),
            "Attachment already has an upload receipt"
        );
        let bytes = checked_bytes(media_root, attachment)?;
        // CLI 只能接收文件路径：从已核验字节建立私有暂存副本，不再打开用户缓存路径。
        let staging =
            tempfile::tempdir().map_err(|_| anyhow::anyhow!("Cannot stage attachment"))?;
        let mut path = staging.path().join(&attachment.name);
        // 用户原文件名可能是 .txt；CLI 的图片后缀必须来自已核验缓存格式。
        if attachment.is_image {
            let extension = Path::new(&attachment.path)
                .extension()
                .ok_or_else(|| anyhow::anyhow!("Invalid image cache extension"))?;
            path.set_extension(extension);
        }
        let mut file =
            std::fs::File::create(&path).map_err(|_| anyhow::anyhow!("Cannot stage attachment"))?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| anyhow::anyhow!("Cannot stage attachment"))?;
        let mut params = vec![format!("workspaceid={workspace}")];
        let verb = if attachment.is_image {
            "upload-image"
        } else {
            params.extend([
                format!("type={}", target.kind),
                format!("entry-id={}", target.id),
            ]);
            "upload"
        };
        params.push(format!(
            "file={}",
            path.to_str()
                .ok_or_else(|| anyhow::anyhow!("Invalid staging path"))?
        ));
        let result = (|| -> Result<DraftAttachment> {
            let data = self.tapd("attachment", verb, params)?;
            let mut receipt = attachment.clone();
            if attachment.is_image {
                let src = data
                    .get("image_src")
                    .and_then(Value::as_str)
                    .filter(|s| safe_image_src(s))
                    .ok_or_else(|| anyhow::anyhow!("Missing image receipt"))?;
                // 响应可选身份字段一旦存在，也必须一致；图片接口本身不绑定任务。
                for (key, expected) in [
                    ("workspace_id", workspace),
                    ("entry_id", target.id.as_str()),
                ] {
                    if data.get(key).is_some() {
                        ensure!(
                            receipt_id(data.get(key)).as_deref() == Some(expected),
                            "Image identity mismatch"
                        );
                    }
                }
                receipt.remote_url = Some(src.into());
            } else {
                receipt.remote_id = Some(self.verify_uploaded_file(&data, workspace, target)?);
            }
            receipt.uncertain = false;
            Ok(receipt)
        })();
        result.map_err(|_| anyhow::anyhow!("TAPD attachment upload result is uncertain; verify remotely before retry (server may not support this attachment contract)"))
    }
}
