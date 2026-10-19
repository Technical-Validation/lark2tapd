//! 仅在用户确认来源与安装目录后调用；不读写应用配置或业务凭据。
use anyhow::{Result, bail, ensure};
use std::{
    fs,
    io::Read,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    time::Duration,
};

pub(crate) const CLI_PATH_ERROR: &str =
    "CLI executable not found at a usable absolute path; restore its installation and check again";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CliTool {
    Lark,
    Tapd,
}
#[derive(Clone, Debug)]
pub struct InstallationPlan {
    pub tool: CliTool,
    pub source: String,
    pub destination: PathBuf,
    pub version: &'static str,
    pub requirements: &'static str,
}

// 来源经只读核对：
// https://registry.npmjs.org/@larksuite/cli/1.0.96
// https://raw.githubusercontent.com/larksuite/cli/v1.0.96/scripts/install.js
// https://cnb.cool/tapd.cn/skills/tapd-cli/-/git/raw/main/install.sh
// TAPD 官方脚本规定 releases/download/<tag>/tapd-cli-<os>-<arch>。
// 不执行 TAPD 安装脚本：它可能调用 sudo 或修改 shell 配置，且未公布校验和。
pub fn installation_plan(tool: CliTool, tools_dir: &Path) -> Result<InstallationPlan> {
    safe_directory_path(tools_dir)?;
    ensure!(
        matches!(std::env::consts::OS, "macos" | "linux")
            && matches!(std::env::consts::ARCH, "aarch64" | "x86_64"),
        "CLI installation supports only macOS/Linux arm64/x64"
    );
    let (name, version, source, requirements) = match tool {
        CliTool::Lark => (
            "lark",
            "1.0.96",
            "https://registry.npmjs.org/@larksuite/cli/1.0.96".to_owned(),
            "Requires Node.js >=20, npm, curl and tar. Official npm installer downloads from GitHub, with npmmirror fallback, and verifies its bundled SHA-256 checksums. No global installation or shell profile changes.",
        ),
        CliTool::Tapd => {
            let os = if cfg!(target_os = "macos") {
                "darwin"
            } else {
                "linux"
            };
            let arch = if cfg!(target_arch = "aarch64") {
                "arm64"
            } else {
                "x64"
            };
            (
                "tapd",
                "1.15.0",
                format!(
                    "https://cnb.cool/tapd.cn/skills/tapd-cli/-/releases/download/v1.15.0/tapd-cli-{os}-{arch}"
                ),
                "Requires curl. Direct download from official TAPD CNB release over HTTPS. No published checksum/signature verification; architecture and version checks are not authenticity verification. No sudo or shell profile changes.",
            )
        }
    };
    let destination = tools_dir.join(format!("{name}-{version}"));
    safe_directory_path(&destination)?;
    Ok(InstallationPlan {
        tool,
        source,
        destination,
        version,
        requirements,
    })
}

// 拒绝相对路径、遍历和现有软链；调用方应传入已规范化的应用私有根目录。
fn safe_directory_path(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute()
            && !path
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir)),
        "Installation directory must be an absolute path without traversal"
    );
    let mut prefix = PathBuf::new();
    for component in path.components() {
        prefix.push(component);
        match fs::symlink_metadata(&prefix) {
            Ok(meta) => ensure!(
                meta.is_dir() && !meta.file_type().is_symlink(),
                "Installation directory must not contain symlinks or files"
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => bail!("Cannot inspect installation directory"),
        }
    }
    Ok(())
}

/// 必须先展示 installation_plan 的来源、目录和安全说明，并取得用户明确确认。
/// 在后台线程执行；成功后由调用方保存返回路径，失败时保留原配置。
pub fn install_cli(tool: CliTool, tools_dir: &Path) -> Result<PathBuf> {
    install_with(tool, tools_dir, &mut crate::process::run)
}

/// 版本诊断不是身份认证或远程连接测试；不返回未经筛选的 CLI 输出。
/// TAPD 自身可能查询更新并写入版本缓存，因此也必须由用户主动触发。
pub fn probe_cli(tool: CliTool, path: &Path) -> Result<String> {
    ensure!(path.is_absolute() && path.is_file(), CLI_PATH_ERROR);
    probe_with(tool, path, &[], &mut crate::process::run)
}

/// 产品配置入口与连接诊断共用同一份已验证的协议版本范围。
pub fn probe_supported_cli(tool: CliTool, path: &Path) -> Result<String> {
    let version = probe_cli(tool, path)?;
    let supported = match tool {
        CliTool::Lark => version == "1.0.96",
        CliTool::Tapd => matches!(version.as_str(), "1.8.1" | "1.15.0"),
    };
    ensure!(
        supported,
        "Unsupported CLI version {version}; supported versions: {}",
        match tool {
            CliTool::Lark => "Lark 1.0.96",
            CliTool::Tapd => "TAPD 1.8.1 or 1.15.0",
        }
    );
    Ok(version)
}

type Runner<'a> =
    dyn FnMut(&Path, &[String], &[(String, String)], &[u8], Duration) -> Result<Vec<u8>> + 'a;
fn probe_with(
    tool: CliTool,
    path: &Path,
    env: &[(String, String)],
    run: &mut Runner<'_>,
) -> Result<String> {
    let bytes = run(
        path,
        &["--version".into()],
        env,
        b"",
        Duration::from_secs(10),
    )
    .map_err(|_| anyhow::anyhow!("CLI version check failed"))?;
    let version = parse_version(tool, &bytes)?;
    // TAPD 的版本输出只有数字；额外核对帮助标识，避免误认 Node 等其他程序。
    if tool == CliTool::Tapd {
        let help = run(path, &["--help".into()], env, b"", Duration::from_secs(10))
            .map_err(|_| anyhow::anyhow!("TAPD identity check failed"))?;
        ensure!(
            help.starts_with(b"Usage: tapd-cli [options] [command]"),
            "Executable is not TAPD CLI"
        );
    }
    Ok(version)
}
fn parse_version(tool: CliTool, bytes: &[u8]) -> Result<String> {
    let text = std::str::from_utf8(bytes).unwrap_or("").trim();
    let version = match tool {
        CliTool::Lark => text.strip_prefix("lark-cli version ").unwrap_or(""),
        CliTool::Tapd => text,
    };
    ensure!(
        version.len() <= 32
            && version.split('.').count() == 3
            && version
                .split('.')
                .all(|p| !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit())),
        "CLI returned an unexpected version identity"
    );
    Ok(version.to_owned())
}

fn install_with(tool: CliTool, root: &Path, run: &mut Runner<'_>) -> Result<PathBuf> {
    let plan = installation_plan(tool, root)?;
    ensure!(
        !plan.destination.exists(),
        "Private installation already exists; configure its executable path instead of overwriting it"
    );
    // 前置检查不下载、不授权；错误不携带子进程输出或外部错误链。
    if tool == CliTool::Lark {
        let node = run(
            Path::new("node"),
            &["--version".into()],
            &[],
            b"",
            Duration::from_secs(10),
        )
        .map_err(|_| anyhow::anyhow!("Install Node.js >=20 and npm before installing Lark CLI"))?;
        let major = std::str::from_utf8(&node)
            .unwrap_or("")
            .trim()
            .strip_prefix('v')
            .unwrap_or("")
            .split('.')
            .next()
            .unwrap_or("")
            .parse::<u32>()
            .unwrap_or(0);
        ensure!(
            major >= 20,
            "Lark installation requires Node.js >=20 and npm"
        );
        run(
            Path::new("npm"),
            &["--version".into()],
            &[],
            b"",
            Duration::from_secs(10),
        )
        .map_err(|_| anyhow::anyhow!("npm is missing or cannot run; install Node.js with npm"))?;
    }
    // create_dir 独占预留版本目录，防止并发安装覆盖；失败只清理本次创建的目录。
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(root)?;
    safe_directory_path(root)?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&plan.destination)
        .map_err(|_| anyhow::anyhow!("Cannot reserve private installation directory"))?;
    let result = install_reserved(&plan, run);
    if result.is_err() {
        let _ = fs::remove_dir_all(&plan.destination);
    }
    result
}

fn install_reserved(plan: &InstallationPlan, run: &mut Runner<'_>) -> Result<PathBuf> {
    let dir = &plan.destination;
    for name in ["home", "tmp", "cache"] {
        fs::create_dir(dir.join(name))?;
    }
    let env: Vec<_> = [
        ("HOME", dir.join("home")),
        ("TMPDIR", dir.join("tmp")),
        ("XDG_CONFIG_HOME", dir.join("home")),
        ("XDG_CACHE_HOME", dir.join("cache")),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_string_lossy().into_owned()))
    .collect();
    let staged = match plan.tool {
        CliTool::Lark => {
            fs::write(dir.join("npm-userconfig"), b"")?;
            fs::write(dir.join("npm-globalconfig"), b"")?;
            let args = vec![
                "install".into(),
                "--prefix".into(),
                dir.to_string_lossy().into_owned(),
                "--ignore-scripts".into(),
                "--no-audit".into(),
                "--no-fund".into(),
                "--global=false".into(),
                "--registry=https://registry.npmjs.org".into(),
                format!("--cache={}", dir.join("cache").display()),
                format!("--userconfig={}", dir.join("npm-userconfig").display()),
                format!("--globalconfig={}", dir.join("npm-globalconfig").display()),
                "@larksuite/cli@1.0.96".into(),
            ];
            run(Path::new("npm"), &args, &env, b"", Duration::from_secs(300)).map_err(|_| {
                anyhow::anyhow!("Lark npm download failed; original configuration was not changed")
            })?;
            // 只执行已核对的官方安装入口，禁用 npm 自动执行依赖的生命周期脚本。
            let package = dir.join("node_modules/@larksuite/cli");
            run(
                Path::new("node"),
                &[package
                    .join("scripts/install.js")
                    .to_string_lossy()
                    .into_owned()],
                &env,
                b"",
                Duration::from_secs(300),
            )
            .map_err(|_| {
                anyhow::anyhow!(
                    "Official Lark binary installer failed; check curl, tar and network access"
                )
            })?;
            package.join("bin/lark-cli")
        }
        CliTool::Tapd => {
            let target = dir.join("download");
            let args = [
                "--fail",
                "--location",
                "--silent",
                "--show-error",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--connect-timeout",
                "10",
                "--max-time",
                "180",
                "--max-redirs",
                "3",
                "--max-filesize",
                "268435456",
                "--output",
            ]
            .map(String::from)
            .into_iter()
            .chain([target.to_string_lossy().into_owned(), plan.source.clone()])
            .collect::<Vec<_>>();
            run(
                Path::new("curl"),
                &args,
                &env,
                b"",
                Duration::from_secs(190),
            )
            .map_err(|_| {
                anyhow::anyhow!(
                    "Official TAPD release download failed; original configuration was not changed"
                )
            })?;
            check_native(&target)?;
            fs::set_permissions(&target, fs::Permissions::from_mode(0o700))?;
            target
        }
    };
    safe_directory_path(staged.parent().unwrap())?;
    let metadata = fs::symlink_metadata(&staged)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "Downloaded CLI must be a regular file"
    );
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o700))?;
    ensure!(
        probe_with(plan.tool, &staged, &env, run)? == plan.version,
        "Downloaded CLI version differs from the confirmed version"
    );
    let target = dir.join(match plan.tool {
        CliTool::Lark => "lark-cli",
        CliTool::Tapd => "tapd-cli",
    });
    // 仅在版本核对完成后原子发布文件；已有用户配置从未成为写入目标。
    fs::rename(staged, &target)?;
    Ok(target)
}

// 检查本机可执行文件头，不把该检查冒充签名或来源认证。
fn check_native(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file()
            && !meta.file_type().is_symlink()
            && (1_048_576..=268_435_456).contains(&meta.len()),
        "Downloaded TAPD file has an invalid type or size"
    );
    let mut header = [0u8; 20];
    fs::File::open(path)?.read_exact(&mut header)?;
    let valid = if cfg!(target_os = "macos") {
        let cpu = if cfg!(target_arch = "aarch64") {
            0x0100000cu32
        } else {
            0x01000007u32
        };
        header[..4] == [0xcf, 0xfa, 0xed, 0xfe] && header[4..8] == cpu.to_le_bytes()
    } else {
        let machine = if cfg!(target_arch = "aarch64") {
            183u16
        } else {
            62u16
        };
        header[..6] == [0x7f, b'E', b'L', b'F', 2, 1] && header[18..20] == machine.to_le_bytes()
    };
    ensure!(
        valid,
        "Downloaded TAPD binary is not for this operating system and architecture"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    // 子进程边界完全伪造；测试只生成稀疏文件头，绝不下载或运行真实安装器。
    fn native_fixture(path: &Path) {
        let mut header = [0u8; 20];
        if cfg!(target_os = "macos") {
            header[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
            header[4..8].copy_from_slice(
                &(if cfg!(target_arch = "aarch64") {
                    0x0100000cu32
                } else {
                    0x01000007u32
                })
                .to_le_bytes(),
            );
        } else {
            header[..6].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1]);
            header[18..20].copy_from_slice(
                &(if cfg!(target_arch = "aarch64") {
                    183u16
                } else {
                    62u16
                })
                .to_le_bytes(),
            );
        }
        fs::write(path, header).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_len(1_048_576)
            .unwrap();
    }
    #[test]
    fn tapd_download_is_private_https_pinned_and_checked_before_publish() {
        for version in ["1.15.0", "9.9.9"] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().canonicalize().unwrap().join("tools");
            let mut calls = 0;
            let result = install_with(CliTool::Tapd, &root, &mut |p, args, env, _, _| {
                calls += 1;
                if p == Path::new("curl") {
                    assert!(args.last().unwrap().contains("/download/v1.15.0/tapd-cli-"));
                    assert!(args.windows(2).any(|a| a == ["--proto-redir", "=https"]));
                    assert!(
                        env.iter()
                            .any(|(k, v)| k == "HOME" && Path::new(v).starts_with(&root))
                    );
                    let i = args.iter().position(|a| a == "--output").unwrap();
                    native_fixture(Path::new(&args[i + 1]));
                    Ok(vec![])
                } else if args == ["--version"] {
                    Ok(version.as_bytes().to_vec())
                } else {
                    assert_eq!(args, ["--help"]);
                    Ok(b"Usage: tapd-cli [options] [command]\n".to_vec())
                }
            });
            assert_eq!(calls, 3);
            if version == "1.15.0" {
                let path = result.unwrap();
                assert_eq!(path.file_name().unwrap(), "tapd-cli");
                assert_eq!(
                    fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    0o700
                );
            } else {
                assert!(result.is_err());
                assert!(!root.join("tapd-1.15.0").exists());
            }
        }
    }
    #[test]
    fn invalid_download_is_never_executed_and_failure_can_retry() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap().join("tools");
        for network_failure in [true, false] {
            assert!(
                install_with(CliTool::Tapd, &root, &mut |p, args, _, _, _| {
                    assert_eq!(p, Path::new("curl"));
                    if network_failure {
                        bail!("secret stderr");
                    }
                    let i = args.iter().position(|a| a == "--output").unwrap();
                    fs::write(&args[i + 1], b"<html>not a release</html>").unwrap();
                    Ok(vec![])
                })
                .is_err()
            );
            assert!(!root.join("tapd-1.15.0").exists());
        }
        let p = dir.path().join("wrong-architecture");
        native_fixture(&p);
        {
            use std::io::Write;
            let mut f = fs::OpenOptions::new().write(true).open(&p).unwrap();
            f.write_all(&[0; 20]).unwrap();
        }
        assert!(check_native(&p).is_err());
    }
    #[test]
    fn bare_semver_of_another_program_is_not_tapd() {
        assert!(
            probe_with(
                CliTool::Tapd,
                Path::new("/fake"),
                &[],
                &mut |_, args, _, _, _| {
                    Ok(if args == ["--version"] {
                        b"1.15.0".to_vec()
                    } else {
                        b"Usage: unrelated-tool".to_vec()
                    })
                }
            )
            .is_err()
        );
    }
    #[test]
    fn plan_is_private_pinned_and_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap().join("tools");
        let plan = installation_plan(CliTool::Lark, &root).unwrap();
        assert!(plan.destination.starts_with(&root));
        assert_eq!(plan.version, "1.0.96");
        assert!(!root.exists());
        assert!(plan.source.contains("@larksuite/cli"));
    }
    #[test]
    fn rejects_relative_traversal_and_symlink_destinations() {
        assert!(installation_plan(CliTool::Lark, Path::new("tools")).is_err());
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        assert!(installation_plan(CliTool::Lark, &root.join("x/../tools")).is_err());
        std::os::unix::fs::symlink(&root, root.join("link")).unwrap();
        assert!(installation_plan(CliTool::Lark, &root.join("link/tools")).is_err());
    }
    #[test]
    fn lark_install_checks_prerequisites_and_returns_verified_private_native_binary() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap().join("tools");
        let mut installs = 0;
        let path = install_with(
            CliTool::Lark,
            &root,
            &mut |program, args, env, input, timeout| {
                assert!(input.is_empty());
                assert!(timeout <= Duration::from_secs(300));
                assert!(
                    env.iter()
                        .all(|(k, _)| !k.contains("TOKEN") && !k.contains("SECRET"))
                );
                if program == Path::new("node") && args == ["--version"] {
                    return Ok(b"v22.0.0".to_vec());
                }
                if program == Path::new("npm") && args == ["--version"] {
                    return Ok(b"10.0.0".to_vec());
                }
                if program == Path::new("npm") {
                    installs += 1;
                    assert!(args.contains(&"--ignore-scripts".into()));
                    assert!(args.contains(&"--registry=https://registry.npmjs.org".into()));
                    return Ok(vec![]);
                }
                if program == Path::new("node") {
                    let script = Path::new(&args[0]);
                    let bin = script
                        .parent()
                        .unwrap()
                        .parent()
                        .unwrap()
                        .join("bin/lark-cli");
                    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
                    std::fs::write(bin, b"fake native executable").unwrap();
                    return Ok(vec![]);
                }
                assert_eq!(args, ["--version"]);
                Ok(b"lark-cli version 1.0.96\n".to_vec())
            },
        )
        .unwrap();
        assert!(path.is_absolute() && path.is_file() && path.starts_with(&root));
        assert_eq!(installs, 1);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(
            install_with(CliTool::Lark, &root, &mut |_, _, _, _, _| panic!(
                "must not overwrite"
            ))
            .is_err()
        );
    }
    #[test]
    fn missing_npm_and_failed_installs_do_not_touch_existing_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let configured = base.join("existing-cli");
        std::fs::write(&configured, b"unchanged").unwrap();
        let err = install_with(CliTool::Lark, &base.join("tools"), &mut |p, _, _, _, _| {
            if p == Path::new("node") {
                Ok(b"v22.0.0".to_vec())
            } else {
                bail!("secret child error")
            }
        })
        .unwrap_err();
        assert!(err.to_string().contains("npm"));
        assert!(!format!("{err:#}").contains("secret"));
        assert_eq!(std::fs::read(configured).unwrap(), b"unchanged");
        assert!(!base.join("tools/lark-1.0.96").exists());
    }
    #[test]
    fn version_parser_rejects_wrong_tool_and_secret_output() {
        assert_eq!(
            parse_version(CliTool::Lark, b"lark-cli version 1.0.96\n").unwrap(),
            "1.0.96"
        );
        assert_eq!(parse_version(CliTool::Tapd, b"1.15.0\n").unwrap(), "1.15.0");
        for bytes in [
            b"secret TOKEN".as_slice(),
            b"node 1.0.96",
            b"lark-cli 1.0.96\nsecret",
        ] {
            let err = parse_version(CliTool::Lark, bytes).unwrap_err();
            assert!(!err.to_string().contains("TOKEN"));
        }
    }
}
