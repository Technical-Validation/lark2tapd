use super::*;
use std::{path::Path, process::Command};

fn sql(path: &Path, statement: &str) -> String {
    let output = Command::new("sqlite3")
        .arg(path)
        .arg(statement)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

// 故障仅限临时库；INSERT/UPDATE 都拒绝，覆盖首次保存和已有快照。
fn reject_writes(path: &Path) {
    let table = sql(
        path,
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%';",
    );
    let table = table.trim();
    assert!(!table.is_empty() && !table.contains('\n'));
    for operation in ["INSERT", "UPDATE"] {
        sql(
            path,
            &format!(
                "CREATE TRIGGER fail_{operation} BEFORE {operation} ON \"{table}\" BEGIN SELECT RAISE(FAIL, 'injected settings disk failure'); END;"
            ),
        );
    }
}

fn event(worker: &WorkerHandle) -> Event {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        match worker.events.try_recv() {
            Ok(event) => return event,
            Err(async_channel::TryRecvError::Closed) => panic!("worker closed"),
            Err(_) => assert!(Instant::now() < until, "event timed out"),
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn state(worker: &WorkerHandle) -> Snapshot {
    loop {
        match event(worker) {
            Event::State { snapshot, .. } => return snapshot,
            Event::Error(error) | Event::FormError { message: error, .. } => panic!("{error}"),
            _ => {}
        }
    }
}

fn request(worker: &WorkerHandle, job: Job) {
    worker
        .jobs
        .send(Request {
            sequence: 42,
            profile: "mock".into(),
            job,
        })
        .unwrap();
}

fn stop(worker: WorkerHandle) {
    let events = worker.events.clone();
    drop(worker);
    let until = Instant::now() + Duration::from_secs(5);
    while !events.is_closed() {
        assert!(Instant::now() < until, "shutdown timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn persisted(root: &Path) -> Settings {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        Engine::open(&root.join("preferences-mock.db"))
            .await
            .unwrap()
            .snapshot()
            .settings
            .clone()
    })
}

#[test]
fn theme_commit_ignores_profile_disk_failure_and_survives_restart() {
    let root = tempfile::tempdir().unwrap();
    let worker = start(root.path().into(), Mode::Mock);
    state(&worker);
    reject_writes(&root.path().join("mock.db"));
    request(
        &worker,
        Job::SetPreference(Preference::Theme("dark".into())),
    );
    let mut runtime = None;
    loop {
        match event(&worker) {
            Event::State { snapshot, .. } => runtime = Some(snapshot.settings),
            Event::SettingResult { error, changed, .. } => {
                assert_eq!(persisted(root.path()).theme, "dark");
                assert!(
                    error.is_none(),
                    "global commit succeeded but UI reported {error:?}"
                );
                assert!(changed);
                break;
            }
            _ => {}
        }
    }
    assert_eq!(runtime.unwrap().theme, "dark");
    stop(worker);
    let worker = start(root.path().into(), Mode::Mock);
    assert_eq!(state(&worker).settings.theme, "dark");
    stop(worker);
}

fn private_cli(root: &Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let root = root.canonicalize().unwrap();
    let path = installation_plan(CliTool::Lark, &root.join("tools"))
        .unwrap()
        .destination
        .join("lark-cli");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    // Mock ProbeCli 不执行该文件，不安装或调用真实 CLI。
    std::fs::write(&path, "test-only executable").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path.to_string_lossy().into_owned()
}

fn seed_cli(root: &Path) {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let mut engine = Engine::open(&root.join("preferences-mock.db"))
            .await
            .unwrap();
        let mut settings = engine.snapshot().settings.clone();
        settings.lark_path = root.join("original-cli").to_string_lossy().into_owned();
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&settings.lark_path, "test-only executable").unwrap();
        std::fs::set_permissions(&settings.lark_path, std::fs::Permissions::from_mode(0o700))
            .unwrap();
        engine.save_settings(settings).await.unwrap();
    });
}

#[test]
fn cli_rediscovery_commit_ignores_profile_disk_failure_and_survives_restart() {
    let root = tempfile::tempdir().unwrap();
    seed_cli(root.path());
    let worker = start(root.path().into(), Mode::Mock);
    let old = state(&worker).settings;
    reject_writes(&root.path().join("mock.db"));
    let path = private_cli(root.path());
    std::fs::remove_file(&old.lark_path).unwrap();
    request(&worker, Job::ProbeCli(CliTool::Lark));
    let runtime = state(&worker).settings;
    assert_eq!(persisted(root.path()).lark_path, path);
    assert_eq!(runtime.lark_path, path);
    loop {
        if matches!(event(&worker), Event::CliProbe { .. }) {
            break;
        }
    }
    stop(worker);
    let worker = start(root.path().into(), Mode::Mock);
    assert_eq!(state(&worker).settings.lark_path, path);
    stop(worker);
}

#[test]
fn failed_global_commit_keeps_theme_and_cli_runtime_unchanged() {
    let root = tempfile::tempdir().unwrap();
    seed_cli(root.path());
    let worker = start(root.path().into(), Mode::Mock);
    let old = state(&worker).settings;
    reject_writes(&root.path().join("preferences-mock.db"));
    request(
        &worker,
        Job::SetPreference(Preference::Theme("dark".into())),
    );
    loop {
        match event(&worker) {
            Event::SettingResult { error, changed, .. } => {
                assert!(error.is_some());
                assert!(!changed);
                break;
            }
            Event::State { .. } => panic!("failed theme commit advanced runtime"),
            _ => {}
        }
    }
    private_cli(root.path());
    std::fs::remove_file(&old.lark_path).unwrap();
    request(&worker, Job::ProbeCli(CliTool::Lark));
    loop {
        match event(&worker) {
            Event::Error(_) => break,
            Event::State { .. } | Event::CliProbe { .. } => {
                panic!("failed CLI commit advanced runtime")
            }
            _ => {}
        }
    }
    assert_eq!(persisted(root.path()), old);
    // 不经过路径发现的读取公布 actor 当前副本。
    request(&worker, Job::LoadProjects);
    assert_eq!(state(&worker).settings, old);
    stop(worker);
}

struct BlockedProbe {
    release: Mutex<mpsc::Receiver<()>>,
    allow_clear: bool,
}
impl ConnectionBackend for BlockedProbe {
    fn load(&self) -> Result<Credentials> {
        Ok(Credentials::default())
    }
    fn save(&self, _: CliTool, _: &Credentials) -> Result<()> {
        bail!("unexpected credential save")
    }
    fn clear(&self, _: CliTool) -> Result<()> {
        anyhow::ensure!(self.allow_clear, "unexpected credential clear");
        Ok(())
    }
    fn probe(&self, _: CliTool, _: &Settings) -> Result<String> {
        self.release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))?;
        Ok("released fake probe".into())
    }
    fn check(&self, _: CliTool, _: &Settings, _: Credentials) -> Result<ConnectionReport> {
        bail!("unexpected check")
    }
    fn connect(
        &self,
        _: CliTool,
        _: &Settings,
        _: Credentials,
        _: &AtomicBool,
        _: &mut dyn FnMut(String, u64) -> Result<()>,
    ) -> Result<ConnectionReport> {
        bail!("unexpected connect")
    }
}

#[test]
fn unavailable_disconnected_database_prevents_credential_mutation() {
    let root = tempfile::tempdir().unwrap();
    let profile = "a".repeat(64);
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let mut preferences = Engine::open(&root.path().join("preferences-live.db"))
            .await
            .unwrap();
        preferences
            .save_settings(Settings {
                last_profile: Some(profile.clone()),
                ..Default::default()
            })
            .await
            .unwrap();
        let mut account = Engine::open(&root.path().join(format!("live-{profile}.db")))
            .await
            .unwrap();
        account.refresh(&MockGateway::default()).await.unwrap();
    });
    std::fs::create_dir(root.path().join("disconnected.db")).unwrap();
    let (_release, wait) = mpsc::channel();
    let worker = start_with_backend(
        root.path().into(),
        Mode::Live,
        Arc::new(BlockedProbe {
            release: Mutex::new(wait),
            allow_clear: true,
        }),
    );
    assert!(!state(&worker).records.is_empty());
    request(&worker, Job::ClearCredentials(CliTool::Lark));
    let mut mutated = false;
    let mut relabeled_old_data = false;
    loop {
        match event(&worker) {
            Event::CredentialsCleared(_) => mutated = true,
            Event::State {
                snapshot, profile, ..
            } => relabeled_old_data |= profile.is_empty() && !snapshot.records.is_empty(),
            Event::Error(_) => break,
            _ => {}
        }
    }
    stop(worker);
    assert!(
        !mutated,
        "disconnect storage must be prepared before changing credentials"
    );
    assert!(
        !relabeled_old_data,
        "old account data must never be published under the disconnected namespace"
    );
}

#[test]
fn credential_mutation_invalidates_old_reads_even_if_preferences_commit_fails() {
    let root = tempfile::tempdir().unwrap();
    let old_profile = "a".repeat(64);
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let mut preferences = Engine::open(&root.path().join("preferences-live.db"))
            .await
            .unwrap();
        preferences
            .save_settings(Settings {
                last_profile: Some(old_profile.clone()),
                ..Default::default()
            })
            .await
            .unwrap();
    });
    let (release, wait) = mpsc::channel();
    let worker = start_with_backend(
        root.path().into(),
        Mode::Live,
        Arc::new(BlockedProbe {
            release: Mutex::new(wait),
            allow_clear: true,
        }),
    );
    state(&worker);
    reject_writes(&root.path().join("preferences-live.db"));
    request(&worker, Job::ProbeCli(CliTool::Tapd));
    loop {
        if matches!(event(&worker), Event::Activity { key, active: true } if key == "tapd-cli") {
            break;
        }
    }
    request(&worker, Job::ClearCredentials(CliTool::Lark));
    loop {
        if matches!(event(&worker), Event::Error(_)) {
            break;
        }
    }
    // 模拟取消前已完成、但仍排队等待 actor 消费的旧代回执。
    worker
        .jobs
        .0
        .send(Envelope::Complete(Completion {
            key: "tapd-cli".into(),
            generation: 0,
            attachment_origin: None,
            result: Ok((
                old_profile,
                ReadData::CliProbe {
                    tool: CliTool::Tapd,
                    version: Some("stale version".into()),
                    error: None,
                },
            )),
        }))
        .unwrap();
    request(&worker, Job::CancelInstall);
    let mut stale_accepted = false;
    let profile = loop {
        match event(&worker) {
            Event::CliProbe { .. } => stale_accepted = true,
            Event::State {
                profile,
                message: "install_cancelled",
                ..
            } => break profile,
            _ => {}
        }
    };
    release.send(()).unwrap();
    stop(worker);
    assert!(
        !stale_accepted,
        "credential changes must invalidate already queued old results"
    );
    assert!(
        profile.is_empty(),
        "changed credentials must not retain the old account namespace"
    );
}

#[test]
fn failed_cli_global_commit_preserves_inflight_generation() {
    let root = tempfile::tempdir().unwrap();
    seed_cli(root.path());
    std::fs::rename(
        root.path().join("preferences-mock.db"),
        root.path().join("preferences-live.db"),
    )
    .unwrap();
    let (release, wait) = mpsc::channel();
    let worker = start_with_backend(
        root.path().into(),
        Mode::Live,
        Arc::new(BlockedProbe {
            release: Mutex::new(wait),
            allow_clear: false,
        }),
    );
    let old = state(&worker).settings;
    reject_writes(&root.path().join("preferences-live.db"));
    // TAPD probe 保持第 0 代在途；Lark 路径提交失败不得取消该代。
    request(&worker, Job::ProbeCli(CliTool::Tapd));
    loop {
        if matches!(event(&worker), Event::Activity { key, active: true } if key == "tapd-cli") {
            break;
        }
    }
    private_cli(root.path());
    std::fs::remove_file(&old.lark_path).unwrap();
    request(&worker, Job::ProbeCli(CliTool::Lark));
    loop {
        match event(&worker) {
            Event::Error(_) => break,
            Event::Activity { active: false, .. } => {
                panic!("failed commit cancelled the existing generation")
            }
            _ => {}
        }
    }
    worker
        .jobs
        .0
        .send(Envelope::Complete(Completion {
            key: "tapd-cli".into(),
            generation: 0,
            attachment_origin: None,
            result: Ok((
                String::new(),
                ReadData::CliProbe {
                    tool: CliTool::Tapd,
                    version: Some("original generation accepted".into()),
                    error: None,
                },
            )),
        }))
        .unwrap();
    loop {
        if let Event::CliProbe { version, .. } = event(&worker) {
            assert_eq!(version.as_deref(), Some("original generation accepted"));
            break;
        }
    }
    release.send(()).unwrap();
    stop(worker);
}
