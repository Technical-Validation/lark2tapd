//! 启动配置只决定本次进程模式；不从界面或持久化状态切换真实/演示账户。
use crate::worker::Mode;
use std::path::PathBuf;

pub struct LaunchOptions {
    pub mode: Mode,
    pub data_dir: Option<PathBuf>,
}

impl LaunchOptions {
    pub fn parse(args: &[String], mock_env: Option<&str>) -> Result<Self, String> {
        let mut mode = None;
        let mut data_dir = None;
        let mut arguments = args.iter();
        while let Some(arg) = arguments.next() {
            match arg.as_str() {
                "--mock" | "--live" => {
                    let next = if arg == "--mock" {
                        Mode::Mock
                    } else {
                        Mode::Live
                    };
                    if mode.is_some_and(|m| m != next) {
                        return Err("--mock 与 --live 不能同时使用 / Conflicting mode flags".into());
                    }
                    mode = Some(next);
                }
                "--data-dir" => {
                    let path = arguments
                        .next()
                        .filter(|v| !v.is_empty() && !v.starts_with("--"))
                        .ok_or("--data-dir 需要路径 / Missing data path")?;
                    if data_dir.is_some() {
                        return Err("--data-dir 不能重复 / Duplicate data path".into());
                    }
                    data_dir = Some(PathBuf::from(path));
                }
                _ => {
                    return Err("未知启动参数，请使用 --help / Unknown argument; use --help".into());
                }
            }
        }
        let mode = match mode {
            Some(mode) => mode,
            None => match mock_env.unwrap_or("0").trim().to_ascii_lowercase().as_str() {
                "1" | "true" => Mode::Mock,
                "0" | "false" | "" => Mode::Live,
                _ => return Err("LARK2TAPD_MOCK 只接受 1/0/true/false".into()),
            },
        };
        Ok(Self { mode, data_dir })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::prelude::v1::test;
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| s.to_string()).collect()
    }
    #[test]
    fn mock_mode_is_explicit_and_cli_overrides_environment() {
        assert!(LaunchOptions::parse(&[], None).unwrap().mode == Mode::Live);
        assert!(LaunchOptions::parse(&[], Some("true")).unwrap().mode == Mode::Mock);
        assert!(
            LaunchOptions::parse(&args(&["--live"]), Some("1"))
                .unwrap()
                .mode
                == Mode::Live
        );
        assert!(
            LaunchOptions::parse(&args(&["--mock"]), Some("0"))
                .unwrap()
                .mode
                == Mode::Mock
        );
        assert!(LaunchOptions::parse(&args(&["--mock", "--live"]), None).is_err());
        assert!(LaunchOptions::parse(&[], Some("yes-ish")).is_err());
    }
    #[test]
    fn malformed_startup_arguments_never_silently_launch_live() {
        assert!(LaunchOptions::parse(&args(&["--data-dir"]), None).is_err());
        assert!(LaunchOptions::parse(&args(&["--unknown"]), None).is_err());
        let parsed =
            LaunchOptions::parse(&args(&["--mock", "--data-dir", "/tmp/some path"]), None).unwrap();
        assert_eq!(parsed.data_dir, Some(PathBuf::from("/tmp/some path")));
    }
}
