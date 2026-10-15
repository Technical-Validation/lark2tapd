//! 启动只允许真实连接；旧演示开关明确报错，避免意外进入真实账户。
use std::path::PathBuf;

pub struct LaunchOptions {
    pub data_dir: Option<PathBuf>,
    pub help: bool,
}

impl LaunchOptions {
    pub fn parse(args: &[String], mock_env: Option<&str>) -> Result<Self, String> {
        if mock_env.is_some_and(|value| !value.is_empty()) {
            return Err("demo mode removed, remove LARK2TAPD_MOCK variable".into());
        }
        let mut data_dir = None;
        let mut help = false;
        let mut arguments = args.iter();
        while let Some(arg) = arguments.next() {
            match arg.as_str() {
                "--help" => help = true,
                "--mock" => return Err("demo mode removed, remove --mock flag".into()),
                "--live" => return Err("--live is redundant; remove this flag".into()),
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
        Ok(Self { data_dir, help })
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
    fn demo_flags_and_every_nonempty_legacy_variable_are_rejected() {
        assert!(LaunchOptions::parse(&[], None).is_ok());
        assert!(LaunchOptions::parse(&[], Some("")).is_ok());
        for value in ["1", "true", "0", "false", " ", "yes-ish"] {
            let error = LaunchOptions::parse(&[], Some(value))
                .err()
                .expect("legacy variable must not launch live");
            assert!(error.contains("demo mode removed"));
        }
        let error = LaunchOptions::parse(&args(&["--mock"]), None)
            .err()
            .expect("demo flag must fail");
        assert!(error.contains("demo mode removed"));
        assert!(LaunchOptions::parse(&args(&["--live"]), None).is_err());
    }
    #[test]
    fn help_does_not_bypass_removed_mode_validation() {
        assert!(LaunchOptions::parse(&args(&["--help"]), None).is_ok());
        assert!(LaunchOptions::parse(&args(&["--help", "--mock"]), None).is_err());
        assert!(LaunchOptions::parse(&args(&["--help"]), Some("0")).is_err());
    }

    #[test]
    fn malformed_startup_arguments_never_silently_launch_live() {
        assert!(LaunchOptions::parse(&args(&["--data-dir"]), None).is_err());
        assert!(LaunchOptions::parse(&args(&["--unknown"]), None).is_err());
        let parsed = LaunchOptions::parse(&args(&["--data-dir", "/tmp/some path"]), None).unwrap();
        assert_eq!(parsed.data_dir, Some(PathBuf::from("/tmp/some path")));
    }
}
