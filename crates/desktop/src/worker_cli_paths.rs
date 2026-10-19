//! 只检查本地文件，不执行 CLI、不修改安装、不访问凭据。
use super::{CliTool, installation_plan};
use std::path::{Path, PathBuf};

pub(super) fn discover_executable(configured: &str, name: &str, tools: &Path) -> String {
    let search = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        // 与 CLI 子进程的运行环境保持一致，覆盖 Finder 启动时的精简 PATH。
        .chain([
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ]);
    discover_in(configured, name, tools, search)
}

fn discover_in(
    configured: &str,
    name: &str,
    tools: &Path,
    search: impl IntoIterator<Item = PathBuf>,
) -> String {
    let tool = if name == "lark-cli" {
        CliTool::Lark
    } else {
        CliTool::Tapd
    };
    let private_binary = installation_plan(tool, tools)
        .ok()
        .map(|plan| plan.destination.join(name));
    // 有效的已配置路径优先，其次应用私有安装，再查 PATH。绝不把安装目录当作可执行文件。
    std::iter::once(PathBuf::from(configured))
        .chain(private_binary)
        .chain(search.into_iter().map(|dir| dir.join(name)))
        .find(|path| executable(path))
        .map(|path| path.to_string_lossy().into_owned())
        // 未找到时保留原配置，尤其不能丢掉暂时不可用的绝对路径。
        .unwrap_or_else(|| configured.to_owned())
}

fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.is_absolute()
        && path
            .metadata()
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };

    fn binary(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "test executable").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn discovery_preserves_explicit_path_and_recovers_both_private_binaries() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let tools = root.join("tools");
        for (tool, name) in [(CliTool::Lark, "lark-cli"), (CliTool::Tapd, "tapd-cli")] {
            let installed = installation_plan(tool, &tools)
                .unwrap()
                .destination
                .join(name);
            let configured = root.join("configured").join(name);
            let system = root.join("bin").join(name);
            binary(&installed);
            binary(&configured);
            binary(&system);
            assert_eq!(
                discover_in(
                    configured.to_str().unwrap(),
                    name,
                    &tools,
                    [root.join("bin")]
                ),
                configured.to_str().unwrap()
            );
            assert_eq!(
                discover_in(name, name, &tools, [root.join("bin")]),
                installed.to_str().unwrap()
            );
            assert!(installed.exists() && configured.exists() && system.exists());
        }
    }

    #[test]
    fn discovery_skips_directories_broken_links_and_non_executable_files() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let tools = root.join("tools");
        let missing = root.join("missing/lark-cli");
        let directory = root.join("directory/lark-cli");
        fs::create_dir_all(&directory).unwrap();
        let plain = root.join("plain/lark-cli");
        binary(&plain);
        fs::set_permissions(&plain, fs::Permissions::from_mode(0o600)).unwrap();
        let link = root.join("link/lark-cli");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink(&missing, &link).unwrap();
        let search = [
            root.join("directory"),
            root.join("plain"),
            root.join("link"),
        ];
        assert_eq!(
            discover_in(
                missing.to_str().unwrap(),
                "lark-cli",
                &tools,
                search.clone()
            ),
            missing.to_str().unwrap()
        );
        let available = root.join("available/lark-cli");
        binary(&available);
        assert_eq!(
            discover_in(
                "lark-cli",
                "lark-cli",
                &tools,
                search.into_iter().chain([root.join("available")])
            ),
            available.to_str().unwrap()
        );
        assert_eq!(
            discover_in("lark-cli", "lark-cli", &tools, [PathBuf::from("relative")]),
            "lark-cli"
        );
    }
}
