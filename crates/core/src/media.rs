//! 仅接收用户明确选择的文件或剪贴板字节；不解析 Markdown 中的任意本地路径。
use crate::DraftAttachment;
use anyhow::{Result, anyhow, ensure};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};

pub const MAX_ATTACHMENT_BYTES: u64 = 20 * 1024 * 1024;

// 剪贴板、来源下载和预览共用像素/解码内存限制，不能只信任 Content-Type 或扩展名。
pub fn bounded_image(bytes: &[u8]) -> Result<image::DynamicImage> {
    use std::io::Cursor;
    ensure!(
        bytes.len() <= MAX_ATTACHMENT_BYTES as usize,
        "Image exceeds 20 MiB"
    );
    let reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let (w, h) = reader.into_dimensions()?;
    ensure!(
        w > 0 && h > 0 && w <= 8192 && h <= 8192 && u64::from(w) * u64::from(h) <= 16 * 1024 * 1024,
        "Image dimensions exceed the safe limit"
    );
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    Ok(reader.decode()?)
}
pub fn normalized_png(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut output = std::io::Cursor::new(Vec::new());
    bounded_image(bytes)?.write_to(&mut output, image::ImageFormat::Png)?;
    Ok(output.into_inner())
}

// 图片类型取自实际签名，不相信文件名；这里只识别格式，解码由 UI 图片库负责。
fn image_extension(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("jpg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("webp")
    } else {
        None
    }
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn bounded_read(file: File) -> Result<Vec<u8>> {
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "Attachment must be a regular file");
    ensure!(
        metadata.len() <= MAX_ATTACHMENT_BYTES,
        "Attachment exceeds 20 MiB"
    );
    // 读取时也加上限，阻止检查长度后继续增长的文件耗尽内存。
    let mut bytes = Vec::new();
    file.take(MAX_ATTACHMENT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_ATTACHMENT_BYTES,
        "Attachment exceeds 20 MiB"
    );
    Ok(bytes)
}

pub fn copy_attachment(root: &Path, source: &Path) -> Result<DraftAttachment> {
    let bytes = bounded_read(safe_fs::open_source(source)?)?;
    let name = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("Attachment name is not UTF-8"))?;
    cache_bytes(root, &bytes, name)
}

pub fn save_pasted_image(root: &Path, bytes: &[u8], extension: &str) -> Result<DraftAttachment> {
    ensure!(
        bytes.len() as u64 <= MAX_ATTACHMENT_BYTES,
        "Attachment exceeds 20 MiB"
    );
    let actual =
        image_extension(bytes).ok_or_else(|| anyhow!("Unsupported clipboard image format"))?;
    ensure!(
        extension == actual || (extension == "jpeg" && actual == "jpg"),
        "Image extension does not match its content"
    );
    cache_bytes(root, bytes, &format!("pasted-{}.{}", digest(bytes), actual))
}

fn cache_bytes(root: &Path, bytes: &[u8], name: &str) -> Result<DraftAttachment> {
    ensure!(
        !name.is_empty() && !name.chars().any(char::is_control),
        "Invalid attachment name"
    );
    let id = digest(bytes);
    let extension = image_extension(bytes);
    let filename = extension.map_or_else(|| id.clone(), |ext| format!("{id}.{ext}"));
    let cache = safe_fs::Cache::open(root, true)?;
    cache.store(&filename, bytes)?;
    let attachment = DraftAttachment {
        source_derived: false,
        id,
        name: name.into(),
        path: cache
            .path
            .join(filename)
            .to_str()
            .ok_or_else(|| anyhow!("Cache path is not UTF-8"))?
            .into(),
        size: bytes.len() as u64,
        is_image: extension.is_some(),
        remote_id: None,
        remote_url: None,
        uncertain: false,
    };
    // 已存在的同哈希条目也必须重新核验，不能覆盖或信任被篡改的缓存。
    verify_attachment(root, &attachment)?;
    Ok(attachment)
}

pub fn verify_attachment(root: &Path, attachment: &DraftAttachment) -> Result<()> {
    read_attachment(root, attachment).map(|_| ())
}

/// 上传方使用这里返回的字节，而不是校验后再通过 path 重新打开文件。
pub fn read_attachment(root: &Path, attachment: &DraftAttachment) -> Result<Vec<u8>> {
    ensure!(
        attachment.id.len() == 64
            && attachment
                .id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "Invalid attachment ID"
    );
    ensure!(
        attachment.size <= MAX_ATTACHMENT_BYTES,
        "Attachment exceeds 20 MiB"
    );
    let cache = safe_fs::Cache::open(root, false)?;
    let path = Path::new(&attachment.path);
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("Invalid cache path"))?;
    ensure!(
        path == cache.path.join(filename),
        "Attachment is outside its owned cache"
    );
    let bytes = bounded_read(cache.read(filename)?)?;
    ensure!(
        bytes.len() as u64 == attachment.size && digest(&bytes) == attachment.id,
        "Attachment content changed"
    );
    let extension = image_extension(&bytes);
    let expected = extension.map_or_else(
        || attachment.id.clone(),
        |ext| format!("{}.{ext}", attachment.id),
    );
    ensure!(
        filename == expected && attachment.is_image == extension.is_some(),
        "Attachment format changed"
    );
    Ok(bytes)
}

#[cfg(unix)]
mod safe_fs {
    use super::*;
    use std::{
        ffi::CString,
        fs::{self, OpenOptions},
        io::Write,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::fs::{MetadataExt, OpenOptionsExt},
        },
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    pub fn open_source(path: &Path) -> Result<File> {
        // NOFOLLOW 在打开时拒绝符号链接；NONBLOCK 防止管道等特殊文件阻塞 actor。
        Ok(OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?)
    }
    fn open_at(dir: &File, name: &str, flags: i32) -> Result<File> {
        let name = CString::new(name)?;
        // 名称由内部固定值或校验后的单文件名组成；成功返回的新描述符交给 File 唯一拥有。
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                0o600,
            )
        };
        ensure!(
            fd >= 0,
            "Cannot open attachment cache entry: {}",
            std::io::Error::last_os_error()
        );
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    pub struct Cache {
        pub path: PathBuf,
        dir: File,
    }
    impl Cache {
        pub fn open(root: &Path, create: bool) -> Result<Self> {
            if create {
                fs::create_dir_all(root)?;
            }
            let root_handle = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
                .open(root)?;
            ensure!(
                root_handle.metadata()?.uid() == unsafe { libc::geteuid() },
                "Cache belongs to another user"
            );
            if create {
                // 从已打开的应用目录创建私有子目录，不经路径重新查找，避免目录链接竞态。
                let result =
                    unsafe { libc::mkdirat(root_handle.as_raw_fd(), c"media".as_ptr(), 0o700) };
                if result != 0 {
                    let error = std::io::Error::last_os_error();
                    ensure!(
                        error.kind() == std::io::ErrorKind::AlreadyExists,
                        "Cannot create cache: {error}"
                    );
                }
            }
            let dir = open_at(&root_handle, "media", libc::O_RDONLY | libc::O_DIRECTORY)?;
            let metadata = dir.metadata()?;
            ensure!(
                metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o077 == 0,
                "Attachment cache must be private"
            );
            Ok(Self {
                path: root.canonicalize()?.join("media"),
                dir,
            })
        }
        pub fn read(&self, name: &str) -> Result<File> {
            let file = open_at(&self.dir, name, libc::O_RDONLY)?;
            let metadata = file.metadata()?;
            ensure!(
                metadata.uid() == unsafe { libc::geteuid() }
                    && metadata.nlink() == 1
                    && metadata.mode() & 0o077 == 0,
                "Attachment cache file is not privately owned"
            );
            Ok(file)
        }
        pub fn store(&self, name: &str, bytes: &[u8]) -> Result<()> {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let temporary = format!(
                ".pending-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            let mut file = open_at(
                &self.dir,
                &temporary,
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            )?;
            let temporary = CString::new(temporary)?;
            let destination = CString::new(name)?;
            let result = (|| -> Result<()> {
                file.write_all(bytes)?;
                file.sync_all()?;
                // linkat 不覆盖已存在的内容地址；完整写入后才发布，崩溃不暴露半个文件。
                let linked = unsafe {
                    libc::linkat(
                        self.dir.as_raw_fd(),
                        temporary.as_ptr(),
                        self.dir.as_raw_fd(),
                        destination.as_ptr(),
                        0,
                    )
                };
                if linked != 0 {
                    let error = std::io::Error::last_os_error();
                    ensure!(
                        error.kind() == std::io::ErrorKind::AlreadyExists,
                        "Cannot publish attachment: {error}"
                    );
                }
                Ok(())
            })();
            let removed = unsafe { libc::unlinkat(self.dir.as_raw_fd(), temporary.as_ptr(), 0) };
            ensure!(removed == 0, "Cannot remove temporary cache entry");
            result?;
            self.dir.sync_all()?;
            Ok(())
        }
    }
}

// 当前桌面目标为 macOS；其他平台未具备同等安全打开语义时必须拒绝，而非降级跟随链接。
#[cfg(not(unix))]
mod safe_fs {
    use super::*;
    use std::path::PathBuf;
    pub fn open_source(_: &Path) -> Result<File> {
        anyhow::bail!("Secure attachment cache requires Unix")
    }
    pub struct Cache {
        pub path: PathBuf,
    }
    impl Cache {
        pub fn open(_: &Path, _: bool) -> Result<Self> {
            anyhow::bail!("Secure attachment cache requires Unix")
        }
        pub fn read(&self, _: &str) -> Result<File> {
            anyhow::bail!("Secure attachment cache requires Unix")
        }
        pub fn store(&self, _: &str, _: &[u8]) -> Result<()> {
            anyhow::bail!("Secure attachment cache requires Unix")
        }
    }
}
