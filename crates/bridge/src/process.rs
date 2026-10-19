use anyhow::{Result, bail};
use std::{
    io::{Read, Write},
    os::{fd::AsRawFd, unix::process::CommandExt},
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;

thread_local! {
    static TASK_CANCELLATION: std::cell::RefCell<Option<Arc<AtomicBool>>> = const { std::cell::RefCell::new(None) };
}

/// 为一个后台任务内所有 CLI 调用设置取消边界（包括版本探测和原生 profile 准备）。
/// 不传播到新线程；显式并行读取仍须传递自己的 cancelled 参数。
pub fn with_process_cancellation<T>(cancelled: Arc<AtomicBool>, action: impl FnOnce() -> T) -> T {
    struct Restore(Option<Arc<AtomicBool>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TASK_CANCELLATION.with(|slot| {
                slot.replace(self.0.take());
            });
        }
    }
    let _restore = Restore(TASK_CANCELLATION.with(|slot| slot.replace(Some(cancelled))));
    action()
}

fn is_cancelled(explicit: &AtomicBool) -> bool {
    explicit.load(Ordering::SeqCst)
        || TASK_CANCELLATION.with(|slot| {
            slot.borrow()
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::SeqCst))
        })
}

#[derive(Debug)]
pub(crate) struct CallTimeout;
impl std::fmt::Display for CallTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CLI timed out; remote result may be uncertain")
    }
}
impl std::error::Error for CallTimeout {}
#[derive(Debug)]
pub(crate) struct Cancelled;
impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CLI operation cancelled")
    }
}
impl std::error::Error for Cancelled {}

const PROXY_ENV: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
];
fn forwarded_environment(
    mut get: impl FnMut(&str) -> Option<std::ffi::OsString>,
) -> Vec<(&'static str, std::ffi::OsString)> {
    [
        "HOME",
        "USER",
        "LOGNAME",
        "TMPDIR",
        "LANG",
        "LC_ALL",
        "XDG_CONFIG_HOME",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
    ]
    .into_iter()
    .chain(PROXY_ENV.iter().copied())
    .filter_map(|key| get(key).map(|value| (key, value)))
    .collect()
}
/// 仅报告有无代理配置，绝不返回可能带密码的地址。
pub fn proxy_environment_present() -> bool {
    PROXY_ENV
        .iter()
        .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()))
}

// 管道必须非阻塞；逃离进程组的后代即使保留句柄，也不能让 join 永久等待。
fn nonblocking(pipe: &impl AsRawFd) -> std::io::Result<()> {
    let fd = pipe.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
fn before_deadline(deadline: Instant) -> std::io::Result<()> {
    if Instant::now() >= deadline {
        return Err(std::io::ErrorKind::TimedOut.into());
    }
    Ok(())
}
// 进程已退出不代表管道读取已完成；管道截止时间也属于同一次 CLI 超时。
fn pipe_failure(error: std::io::Error) -> anyhow::Error {
    if error.kind() == std::io::ErrorKind::TimedOut {
        CallTimeout.into()
    } else {
        anyhow::anyhow!("CLI I/O failed")
    }
}
fn drain(
    mut pipe: impl Read,
    overflow: Arc<AtomicBool>,
    deadline: Instant,
) -> std::io::Result<Vec<u8>> {
    let mut data = Vec::new();
    let mut buf = [0; 8192];
    loop {
        before_deadline(deadline)?;
        let n = match pipe.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
                continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if n == 0 {
            return Ok(data);
        }
        if data.len() + n > OUTPUT_LIMIT {
            overflow.store(true, Ordering::Relaxed);
        } else {
            data.extend_from_slice(&buf[..n]);
        }
    }
}

pub(crate) fn run(
    path: &Path,
    args: &[String],
    env: &[(String, String)],
    input: &[u8],
    timeout: Duration,
) -> Result<Vec<u8>> {
    let output = run_output(path, args, env, input, timeout)?;
    if !output.status.success() {
        return Err(output.failure());
    }
    Ok(output.stdout.to_vec())
}

// 原始输出只在桥接内部短暂保留；错误由白名单字段重建，不携带输出链。
pub(crate) struct ProcessOutput {
    pub status: std::process::ExitStatus,
    pub stdout: zeroize::Zeroizing<Vec<u8>>,
    pub stderr: zeroize::Zeroizing<Vec<u8>>,
}
impl ProcessOutput {
    pub fn failure(&self) -> anyhow::Error {
        let stdout: serde_json::Value = serde_json::from_slice(&self.stdout).unwrap_or_default();
        let value = if stdout.get("error").is_some()
            || stdout.get("code").is_some()
            || stdout.get("type").is_some()
        {
            stdout
        } else {
            serde_json::from_slice(&self.stderr).unwrap_or(stdout)
        };
        crate::diagnostics::api_failure("CLI", &value, self.status.code())
    }
}
pub(crate) fn run_output(
    path: &Path,
    args: &[String],
    env: &[(String, String)],
    input: &[u8],
    timeout: Duration,
) -> Result<ProcessOutput> {
    run_output_cancellable(path, args, env, input, timeout, &AtomicBool::new(false))
}

pub(crate) fn run_output_cancellable(
    path: &Path,
    args: &[String],
    env: &[(String, String)],
    input: &[u8],
    timeout: Duration,
    cancelled: &AtomicBool,
) -> Result<ProcessOutput> {
    if is_cancelled(cancelled) {
        return Err(Cancelled.into());
    }
    let deadline = Instant::now() + timeout;
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let mut paths: Vec<_> = std::env::split_paths(&inherited).collect();
    if let Some(parent) = path.parent() {
        paths.push(parent.to_path_buf());
    }
    paths.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"].map(Into::into));
    let path_env = std::env::join_paths(paths)
        .map_err(|_| anyhow::anyhow!("Invalid executable search path"))?;
    let mut cmd = Command::new(path);
    // GUI 启动保留必要运行环境，但不继承其他账户令牌或 NODE_OPTIONS 等注入入口。
    cmd.env_clear();
    // 保留用户已有的标准网络代理，不继承其他服务令牌或代码注入变量。
    cmd.envs(forwarded_environment(|key| std::env::var_os(key)));
    cmd.args(args)
        .env("PATH", path_env)
        .envs(env.iter().cloned())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = cmd.spawn().map_err(|_| {
        anyhow::anyhow!("Cannot launch CLI; install it and configure an absolute executable path")
    })?;
    let pid = child.id() as i32;
    let overflow = Arc::new(AtomicBool::new(false));
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    if nonblocking(&stdout)
        .and_then(|_| nonblocking(&stderr))
        .and_then(|_| nonblocking(&stdin))
        .is_err()
    {
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
        let _ = child.kill();
        let _ = child.wait();
        bail!("Cannot configure bounded CLI pipes");
    }
    let out_flag = overflow.clone();
    let err_flag = overflow.clone();
    let out = thread::spawn(move || drain(stdout, out_flag, deadline));
    let err = thread::spawn(move || drain(stderr, err_flag, deadline));
    let input = zeroize::Zeroizing::new(input.to_vec());
    let writer = thread::spawn(move || -> std::io::Result<()> {
        let mut offset = 0;
        while offset < input.len() {
            before_deadline(deadline)?;
            match stdin.write(&input[offset..]) {
                Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
                Ok(n) => offset += n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    });
    let result = loop {
        if is_cancelled(cancelled) {
            break Err(anyhow::Error::new(Cancelled));
        }
        if overflow.load(Ordering::Relaxed) {
            break Err(anyhow::anyhow!("CLI output exceeded safety limit"));
        }
        if Instant::now() >= deadline {
            break Err(anyhow::Error::new(CallTimeout));
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(_) => break Err(anyhow::anyhow!("Cannot wait for CLI")),
        }
    };
    // 即使主进程退出也关闭同组后代；逃离该组的后代由管道线程统一截止时间限制。
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
    let write_result = writer.join();
    let output = out.join();
    let errors = err.join();
    let status = result?;
    if overflow.load(Ordering::Relaxed) {
        bail!("CLI output exceeded safety limit");
    }
    if status.success() {
        write_result
            .map_err(|_| anyhow::anyhow!("CLI writer failed"))?
            .map_err(pipe_failure)?;
    }
    let stdout = output
        .map_err(|_| anyhow::anyhow!("CLI reader failed"))?
        .map_err(pipe_failure)?;
    let stderr = errors
        .map_err(|_| anyhow::anyhow!("CLI reader failed"))?
        .map_err(pipe_failure)?;
    Ok(ProcessOutput {
        status,
        stdout: zeroize::Zeroizing::new(stdout),
        stderr: zeroize::Zeroizing::new(stderr),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt, time::Instant};
    fn script(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("fake");
        fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).unwrap();
        (dir, p)
    }
    #[test]
    fn proxy_environment_is_forwarded_without_inheriting_unrelated_credentials() {
        let forwarded = forwarded_environment(|key| match key {
            "HTTPS_PROXY" => Some("http://proxy-user:proxy-secret@127.0.0.1:7890".into()),
            "https_proxy" => Some("http://127.0.0.1:7891".into()),
            "NO_PROXY" => Some("localhost,127.0.0.1".into()),
            "LARKSUITE_CLI_USER_ACCESS_TOKEN" | "TAPD_TOKEN" | "NODE_OPTIONS" => {
                Some("must-not-inherit".into())
            }
            _ => None,
        });
        assert_eq!(
            forwarded.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            ["HTTPS_PROXY", "NO_PROXY", "https_proxy"]
        );
        assert_eq!(
            forwarded[0].1,
            "http://proxy-user:proxy-secret@127.0.0.1:7890"
        );
        assert!(!forwarded.iter().any(|(_, v)| v == "must-not-inherit"));
    }
    #[test]
    fn cancellation_stops_exchange_before_token_write() {
        let (dir, p) =
            script("(sleep 0.4; printf token > \"$MARKER\") & printf ready > \"$READY\"; wait");
        let marker = dir.path().join("token");
        let ready = dir.path().join("ready");
        let cancelled = AtomicBool::new(false);
        let cancelled_at = std::sync::Mutex::new(None);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let deadline = Instant::now() + Duration::from_secs(2);
                while !ready.exists() {
                    assert!(Instant::now() < deadline);
                    thread::sleep(Duration::from_millis(2));
                }
                *cancelled_at.lock().unwrap() = Some(Instant::now());
                cancelled.store(true, Ordering::SeqCst);
            });
            let result = run_output_cancellable(
                &p,
                &[],
                &[
                    ("MARKER".into(), marker.to_string_lossy().into_owned()),
                    ("READY".into(), ready.to_string_lossy().into_owned()),
                ],
                b"",
                Duration::from_secs(5),
                &cancelled,
            );
            assert!(
                result.is_err(),
                "cancelled exchange completed and wrote a token"
            );
        });
        assert!(cancelled_at.lock().unwrap().unwrap().elapsed() < Duration::from_millis(300));
        thread::sleep(Duration::from_millis(500));
        assert!(!marker.exists(), "cancelled descendant persisted a token");
    }
    #[test]
    fn passes_stdin_and_env_without_shell_interpolation() {
        let (_d, p) = script("printf '%s|' \"$TOKEN\"; cat; printf '%s' \"$1\"");
        let out = run(
            &p,
            &["$(echo unsafe)".into()],
            &[("TOKEN".into(), "secret".into())],
            b"json",
            // 此测试断言内容与参数契约；短截止时间另由专用超时测试覆盖。
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(out, b"secret|json$(echo unsafe)");
    }
    #[test]
    fn timeout_kills_descendants_and_reaps() {
        let (_d, p) = script("sleep 30 & wait");
        let now = Instant::now();
        assert!(run(&p, &[], &[], b"", Duration::from_millis(80)).is_err());
        assert!(now.elapsed() < Duration::from_secs(2));
    }
    #[test]
    fn detached_descendant_cannot_hold_pipes_past_deadline() {
        // 孙进程创建新会话后逃离进程组，但只活两秒，不在测试机留下长期进程。
        let (_d, p) = script(
            "python3 - <<'PY'\nimport os,time\nr,w=os.pipe()\nif os.fork()==0:\n os.setsid()\n os.write(w,b'ready')\n time.sleep(2)\nelse:\n os.read(r,5)\nPY",
        );
        let now = Instant::now();
        let result = run(&p, &[], &[], b"", Duration::from_millis(700));
        assert!(now.elapsed() < Duration::from_millis(1500));
        let error = result.unwrap_err();
        assert!(
            error.is::<CallTimeout>(),
            "pipe deadline must retain timeout classification: {error}"
        );
    }
    #[test]
    fn error_never_contains_output_or_arguments() {
        let (_d, p) = script("echo secret; echo secret >&2; exit 4");
        let e = run(&p, &["secret".into()], &[], b"", Duration::from_secs(2)).unwrap_err();
        assert!(!format!("{e:?}").contains("secret"));
    }
    #[test]
    fn structured_stderr_survives_nonzero_with_uninformative_stdout() {
        let (_d, p) = script(
            "echo '{}'; echo '{\"error\":{\"type\":\"network\",\"code\":503,\"message\":\"private-token\"}}' >&2; exit 8",
        );
        let error = run(&p, &[], &[], b"", Duration::from_secs(2))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("network") && error.contains("503") && error.contains("exit 8"),
            "{error}"
        );
        assert!(!error.contains("private-token"));
    }
    #[test]
    fn output_limit_rejects_instead_of_truncating_json() {
        let (_d, p) = script("yes X");
        assert!(run(&p, &[], &[], b"", Duration::from_secs(2)).is_err());
    }
}
