use super::*;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

// Keychain 和 CLI 边界使用内存替身，数据库与 actor 保持真实。
#[derive(Default)]
struct Backend {
    credentials: Mutex<Credentials>,
    saves: AtomicUsize,
    clears: AtomicUsize,
    connects: AtomicUsize,
    mutations: AtomicUsize,
    cancellations: AtomicUsize,
    loads: AtomicUsize,
    checks: AtomicUsize,
    fail_checks: AtomicBool,
    tested_tokens: Mutex<Vec<String>>,
    probed_paths: Mutex<Vec<String>>,
    fail_probe: bool,
    native_probe: bool,
    fail_save: bool,
    fail_auth: bool,
    wait_for_auth: Option<Arc<std::sync::Barrier>>,
}
impl ConnectionBackend for Backend {
    fn load(&self) -> Result<Credentials> {
        self.loads.fetch_add(1, Ordering::SeqCst);
        Ok(self.credentials.lock().unwrap().clone())
    }
    fn save(&self, tool: CliTool, input: &Credentials) -> Result<()> {
        self.saves.fetch_add(1, Ordering::SeqCst);
        anyhow::ensure!(!self.fail_save, "injected-secret-save-error");
        let mut saved = self.credentials.lock().unwrap();
        match tool {
            CliTool::Lark => {
                saved.lark_app_id.clone_from(&input.lark_app_id);
                saved.lark_app_secret.clone_from(&input.lark_app_secret);
                saved.lark_user_token.clone_from(&input.lark_user_token);
            }
            CliTool::Tapd => saved.tapd_token.clone_from(&input.tapd_token),
        }
        Ok(())
    }
    fn clear(&self, tool: CliTool) -> Result<()> {
        self.clears.fetch_add(1, Ordering::SeqCst);
        let mut saved = self.credentials.lock().unwrap();
        match tool {
            CliTool::Lark => {
                saved.lark_app_id.clear();
                saved.lark_app_secret.clear();
                saved.lark_user_token.clear();
            }
            CliTool::Tapd => saved.tapd_token.clear(),
        }
        Ok(())
    }
    fn probe(&self, tool: CliTool, settings: &Settings) -> Result<String> {
        self.probed_paths.lock().unwrap().push(match tool {
            CliTool::Lark => settings.lark_path.clone(),
            CliTool::Tapd => settings.tapd_path.clone(),
        });
        if self.native_probe {
            let path = match tool {
                CliTool::Lark => &settings.lark_path,
                CliTool::Tapd => &settings.tapd_path,
            };
            return probe_supported_cli(tool, std::path::Path::new(path));
        }
        anyhow::ensure!(!self.fail_probe, "CLI is not executable");
        Ok("fake CLI 1.0.0".into())
    }
    fn check(
        &self,
        _: CliTool,
        _: &Settings,
        credentials: Credentials,
    ) -> Result<ConnectionReport> {
        self.checks.fetch_add(1, Ordering::SeqCst);
        self.tested_tokens
            .lock()
            .unwrap()
            .push(credentials.tapd_token.clone());
        Ok(report(
            self.fail_auth || self.fail_checks.load(Ordering::SeqCst),
        ))
    }
    fn connect(
        &self,
        tool: CliTool,
        settings: &Settings,
        _: Credentials,
        cancelled: &AtomicBool,
        authorization: &mut dyn FnMut(String, u64) -> Result<()>,
    ) -> Result<ConnectionReport> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        assert!(
            settings.last_profile.is_none(),
            "old namespace survived into auth"
        );
        if tool == CliTool::Lark {
            authorization(
                "https://accounts.feishu.cn/device?user_code=public-code".into(),
                60,
            )?;
        }
        if let Some(barrier) = &self.wait_for_auth {
            barrier.wait();
        }
        if cancelled.load(Ordering::SeqCst) {
            self.cancellations.fetch_add(1, Ordering::SeqCst);
            bail!("Authorization cancelled");
        }
        self.mutations.fetch_add(1, Ordering::SeqCst);
        Ok(report(self.fail_auth))
    }
}
fn event(worker: &WorkerHandle) -> Event {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match worker.events.try_recv() {
            Ok(event) => return event,
            Err(async_channel::TryRecvError::Closed) => panic!("worker stopped"),
            Err(_) => {}
        }
        assert!(Instant::now() < deadline, "worker event timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn send(worker: &WorkerHandle, job: Job) {
    worker
        .jobs
        .send(Request {
            sequence: 0,
            job,
            profile: "old-profile".into(),
        })
        .unwrap();
}
fn startup(mode: Mode, backend: Arc<Backend>) -> (tempfile::TempDir, WorkerHandle) {
    let root = tempfile::tempdir().unwrap();
    if mode == Mode::Live {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let mut engine = Engine::open(&root.path().join("preferences-live.db"))
                .await
                .unwrap();
            let settings = Settings {
                last_profile: Some("a".repeat(64)),
                ..Default::default()
            };
            engine.save_settings(settings).await.unwrap();
        });
    }
    let worker = start_with_backend(root.path().to_owned(), mode, backend);
    assert!(matches!(
        event(&worker),
        Event::State {
            message: "ready",
            ..
        }
    ));
    (root, worker)
}
fn credentials() -> Credentials {
    let mut credentials = Credentials::default();
    credentials.lark_app_id = "injected-secret-app-id".into();
    credentials.lark_app_secret = "injected-secret-app-secret".into();
    credentials.tapd_token = "injected-secret-tapd-token".into();
    credentials
}

fn persisted_verifications(
    root: &std::path::Path,
) -> BTreeMap<String, lark2tapd_core::VerificationReceipt> {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        Engine::open(&root.join("preferences-live.db"))
            .await
            .unwrap()
            .snapshot()
            .settings
            .connection_verifications
            .clone()
    })
}

fn private_cli(root: &std::path::Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let plan =
        installation_plan(CliTool::Lark, &root.canonicalize().unwrap().join("tools")).unwrap();
    std::fs::create_dir_all(&plan.destination).unwrap();
    let binary = plan.destination.join("lark-cli");
    std::fs::write(&binary, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    binary
}

// 固定初始路径，避免测试依赖开发机 PATH 中是否安装了真实 CLI。
fn startup_with_configured_cli(
    backend: Arc<Backend>,
) -> (tempfile::TempDir, WorkerHandle, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let configured = root
        .path()
        .canonicalize()
        .unwrap()
        .join("previous-lark-cli");
    std::fs::rename(private_cli(root.path()), &configured).unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let mut engine = Engine::open(&root.path().join("preferences-live.db"))
            .await
            .unwrap();
        engine
            .save_settings(Settings {
                lark_path: configured.to_string_lossy().into_owned(),
                ..Default::default()
            })
            .await
            .unwrap();
    });
    let worker = start_with_backend(root.path().to_owned(), Mode::Live, backend);
    assert!(matches!(
        event(&worker),
        Event::State {
            message: "ready",
            ..
        }
    ));
    (root, worker, configured)
}

fn persisted_lark_path(root: &std::path::Path) -> String {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        Engine::open(&root.join("preferences-live.db"))
            .await
            .unwrap()
            .snapshot()
            .settings
            .lark_path
            .clone()
    })
}

#[test]
fn startup_recovers_private_cli_and_persists_its_absolute_path() {
    let root = tempfile::tempdir().unwrap();
    let binary = private_cli(root.path());
    let backend = Arc::new(Backend::default());
    let worker = start_with_backend(root.path().to_owned(), Mode::Live, backend.clone());
    match event(&worker) {
        Event::State {
            snapshot,
            message: "ready",
            ..
        } => assert_eq!(snapshot.settings.lark_path, binary.to_str().unwrap()),
        _ => panic!("missing startup snapshot"),
    }
    assert_eq!(persisted_lark_path(root.path()), binary.to_str().unwrap());
    drop(worker);
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .open(root.path().join("app.lock"))
        .unwrap();
    lock.lock().unwrap();
    drop(lock);
    let worker = start_with_backend(root.path().to_owned(), Mode::Live, backend.clone());
    match event(&worker) {
        Event::State { snapshot, .. } => {
            assert_eq!(snapshot.settings.lark_path, binary.to_str().unwrap())
        }
        _ => panic!("missing reopened snapshot"),
    }
    assert!(
        binary.exists(),
        "discovery must never remove an installation"
    );
    assert_eq!(backend.saves.load(Ordering::SeqCst), 0);
    assert_eq!(backend.connects.load(Ordering::SeqCst), 0);
}

#[test]
fn probe_rediscovers_cli_created_after_startup_without_reauthorizing() {
    let backend = Arc::new(Backend::default());
    let (root, worker, previous) = startup_with_configured_cli(backend.clone());
    event(&worker);
    let loads = backend.loads.load(Ordering::SeqCst);
    std::fs::remove_file(previous).unwrap();
    let binary = private_cli(root.path());
    send(&worker, Job::ProbeCli(CliTool::Lark));
    until(&worker, |e| {
        matches!(
            e,
            Event::CliProbe {
                tool: CliTool::Lark,
                version: Some(_),
                error: None
            }
        )
    });
    assert_eq!(
        backend.probed_paths.lock().unwrap().last().unwrap(),
        binary.to_str().unwrap()
    );
    assert_eq!(persisted_lark_path(root.path()), binary.to_str().unwrap());
    assert_eq!(
        backend.loads.load(Ordering::SeqCst),
        loads,
        "local discovery must not read credentials"
    );
    assert_eq!(backend.saves.load(Ordering::SeqCst), 0);
    assert_eq!(backend.clears.load(Ordering::SeqCst), 0);
    assert_eq!(backend.connects.load(Ordering::SeqCst), 0);
}

#[test]
fn verification_receipts_persist_survive_rechecks_and_bind_to_current_inputs() {
    let backend = Arc::new(Backend {
        credentials: Mutex::new(credentials()),
        ..Default::default()
    });
    let (root, worker) = startup(Mode::Live, backend.clone());
    event(&worker);
    send(&worker, Job::Check(CliTool::Tapd));
    until(
        &worker,
        |e| matches!(e,Event::State{snapshot,..} if snapshot.settings.connection_verifications.contains_key("tapd-connection")),
    );
    let first = persisted_verifications(root.path());
    assert!(chrono::DateTime::parse_from_rfc3339(&first["tapd-connection"].at).is_ok());
    assert!(!format!("{first:?}").contains("injected-secret"));
    // 真正释放并重启 actor，证明记录不是仅留在 UI 或进程内存中。
    drop(worker);
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .open(root.path().join("app.lock"))
        .unwrap();
    lock.lock().unwrap();
    drop(lock);
    let worker = start_with_backend(root.path().to_owned(), Mode::Live, backend.clone());
    match event(&worker) {
        Event::State { snapshot, .. } => {
            assert_eq!(snapshot.settings.connection_verifications, first)
        }
        _ => panic!("missing startup snapshot"),
    }
    event(&worker);
    backend.fail_checks.store(true, Ordering::SeqCst);
    send(&worker, Job::Check(CliTool::Tapd));
    until(
        &worker,
        |e| matches!(e,Event::ConnectionReport{report,..} if !report.connected),
    );
    assert_eq!(
        persisted_verifications(root.path()),
        first,
        "a failed recheck must not erase prior success"
    );
    backend.fail_checks.store(false, Ordering::SeqCst);
    let mut input = Credentials::default();
    input.tapd_token = "different-unsaved-token".into();
    send(
        &worker,
        Job::TestConnection {
            tool: CliTool::Tapd,
            credentials: input,
        },
    );
    until(
        &worker,
        |e| matches!(e,Event::ConnectionReport{saved_configuration:false,report,..} if report.connected),
    );
    assert_eq!(
        backend.tested_tokens.lock().unwrap().last().unwrap(),
        "different-unsaved-token"
    );
    assert_eq!(backend.saves.load(Ordering::SeqCst), 0);
    assert_eq!(
        backend.credentials.lock().unwrap().tapd_token,
        "injected-secret-tapd-token"
    );
    assert_eq!(
        persisted_verifications(root.path()),
        first,
        "unsaved test must not stamp saved configuration"
    );
    backend.credentials.lock().unwrap().tapd_token = "externally-changed".into();
    send(&worker, Job::LoadConnectionStatus);
    until(
        &worker,
        |e| matches!(e,Event::State {snapshot,..} if snapshot.settings.connection_verifications.is_empty()),
    );
    assert!(!persisted_verifications(root.path()).contains_key("tapd-connection"));
    send(&worker, Job::Check(CliTool::Tapd));
    until(
        &worker,
        |e| matches!(e,Event::ConnectionReport {report,..} if report.connected),
    );
    assert!(persisted_verifications(root.path()).contains_key("tapd-connection"));
    send(&worker, Job::ClearCredentials(CliTool::Tapd));
    until(&worker, |e| {
        matches!(
            e,
            Event::State {
                message: "credentials_cleared",
                ..
            }
        )
    });
    assert!(!persisted_verifications(root.path()).contains_key("tapd-connection"));
}

#[test]
fn saving_always_revalidates_instead_of_reusing_success() {
    let backend = Arc::new(Backend::default());
    let (root, worker) = startup(Mode::Live, backend.clone());
    event(&worker);
    for expected in [1, 2] {
        send(
            &worker,
            Job::SaveAndConnect {
                tool: CliTool::Tapd,
                credentials: credentials(),
            },
        );
        until(
            &worker,
            |e| matches!(e,Event::ConnectionReport{report,..} if report.connected),
        );
        assert_eq!(backend.connects.load(Ordering::SeqCst), expected);
        assert!(persisted_verifications(root.path()).contains_key("tapd-connection"));
    }
}

#[test]
fn preferences_are_responsive_while_authorization_waits() {
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let backend = Arc::new(Backend {
        wait_for_auth: Some(barrier.clone()),
        ..Default::default()
    });
    let (_root, worker) = startup(Mode::Live, backend);
    event(&worker);
    send(
        &worker,
        Job::SaveAndConnect {
            tool: CliTool::Lark,
            credentials: credentials(),
        },
    );
    send(
        &worker,
        Job::SetPreference(Preference::Theme("dark".into())),
    );
    let deadline = Instant::now() + Duration::from_millis(700);
    let mut saved = false;
    while Instant::now() < deadline {
        if matches!(
            worker.events.try_recv(),
            Ok(Event::State {
                message: "settings_saved",
                ..
            })
        ) {
            saved = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    barrier.wait();
    assert!(saved, "OAuth polling blocked the serial preference actor");
}

fn report(failed: bool) -> ConnectionReport {
    ConnectionReport {
        connected: !failed,
        checks: vec![lark2tapd_bridge::PermissionCheck {
            name: "task-read".into(),
            state: if failed {
                lark2tapd_bridge::PermissionState::Missing
            } else {
                lark2tapd_bridge::PermissionState::Granted
            },
            detail: "safe read permission diagnostic".into(),
        }],
        user_scopes: Some(vec!["task:task:read".into()]),
        app_scopes: None,
        failure: failed.then(|| "LARK_PERMISSION_DENIED: task read permission missing".into()),
    }
}
fn until(worker: &WorkerHandle, predicate: impl Fn(&Event) -> bool) -> Vec<Event> {
    let mut events = vec![];
    loop {
        let next = event(worker);
        let done = predicate(&next);
        events.push(next);
        if done {
            return events;
        }
    }
}
fn finished(worker: &WorkerHandle, tool: CliTool) -> Vec<Event> {
    until(
        worker,
        |event| matches!(event, Event::ConnectionReport { tool: current, .. } if *current == tool),
    )
}
#[test]
fn failed_persistence_never_authenticates_or_clears_the_form() {
    let backend = Arc::new(Backend {
        fail_save: true,
        ..Default::default()
    });
    let (_root, worker) = startup(Mode::Live, backend.clone());
    event(&worker);
    send(
        &worker,
        Job::SaveAndConnect {
            tool: CliTool::Lark,
            credentials: credentials(),
        },
    );
    let events = until(&worker, |event| {
        matches!(
            event,
            Event::Error(_) | Event::SettingResult { error: Some(_), .. }
        )
    });
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::CredentialsSaved(_)))
    );
    assert!(events.iter().any(|event| matches!(event, Event::SettingResult { field: "lark-connection", error:Some(message), .. } if !message.contains("injected-secret"))));
    assert_eq!(backend.connects.load(Ordering::SeqCst), 0);
    assert_eq!(backend.saves.load(Ordering::SeqCst), 1);
}
#[test]
fn auth_failure_retains_saved_state_and_safe_detailed_report() {
    let backend = Arc::new(Backend {
        fail_auth: true,
        ..Default::default()
    });
    let (root, worker) = startup(Mode::Live, backend.clone());
    event(&worker);
    send(
        &worker,
        Job::SaveAndConnect {
            tool: CliTool::Lark,
            credentials: credentials(),
        },
    );
    let events = finished(&worker, CliTool::Lark);
    let saved = events
        .iter()
        .position(|event| matches!(event, Event::CredentialsSaved(CliTool::Lark)))
        .unwrap();
    let invalidated = events.iter().position(|event| matches!(event, Event::State { profile, message: "credentials_saved", .. } if profile.is_empty())).unwrap();
    let required = events
        .iter()
        .position(|event| matches!(event, Event::AuthorizationRequired { .. }))
        .unwrap();
    assert!(saved < invalidated && invalidated < required);
    assert!(events.iter().any(|event| matches!(
        event,
        Event::ConnectionStatus {
            lark_saved: true,
            tapd_saved: false
        }
    )));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::AuthorizationFinished))
    );
    match events.last().unwrap() {
        Event::ConnectionReport { report, .. } => {
            assert!(!report.connected);
            assert_eq!(
                report.failure.as_deref(),
                Some("LARK_PERMISSION_DENIED: task read permission missing")
            );
            assert_eq!(report.checks.len(), 1);
        }
        _ => unreachable!(),
    }
    assert!(
        matches!(event(&worker), Event::FormError { field: "lark-connection", message } if message.contains("LARK_PERMISSION_DENIED"))
    );
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let engine = Engine::open(&root.path().join("preferences-live.db"))
            .await
            .unwrap();
        assert!(engine.snapshot().settings.last_profile.is_none());
    });
}
#[test]
fn clear_only_selected_service_emits_confirmation_without_auth() {
    let backend = Arc::new(Backend::default());
    *backend.credentials.lock().unwrap() = credentials();
    let (_root, worker) = startup(Mode::Live, backend.clone());
    event(&worker);
    send(&worker, Job::ClearCredentials(CliTool::Tapd));
    let events = until(
        &worker,
        |event| matches!(event, Event::Activity { key, active: false } if key == "tapd-connection"),
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::CredentialsCleared(CliTool::Tapd)))
    );
    assert!(events.iter().any(|event| matches!(
        event,
        Event::ConnectionStatus {
            lark_saved: true,
            tapd_saved: false
        }
    )));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::AuthorizationFinished))
    );
    assert_eq!(backend.connects.load(Ordering::SeqCst), 0);
}
#[test]
fn tapd_save_and_check_preserve_lark_and_never_start_oauth() {
    let backend = Arc::new(Backend::default());
    backend.credentials.lock().unwrap().lark_app_id = "stored-app".into();
    let (_root, worker) = startup(Mode::Live, backend.clone());
    event(&worker);
    send(
        &worker,
        Job::SaveAndConnect {
            tool: CliTool::Tapd,
            credentials: credentials(),
        },
    );
    let events = finished(&worker, CliTool::Tapd);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::CredentialsSaved(CliTool::Tapd)))
    );
    assert!(events.iter().any(|event| matches!(
        event,
        Event::ConnectionStatus {
            lark_saved: true,
            tapd_saved: true
        }
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::AuthorizationRequired { .. }))
    );
    send(&worker, Job::Check(CliTool::Tapd));
    let events = finished(&worker, CliTool::Tapd);
    assert!(
        matches!(events.last(), Some(Event::ConnectionReport { report, .. }) if report.connected)
    );
    assert_eq!(backend.saves.load(Ordering::SeqCst), 1);
    assert_eq!(backend.connects.load(Ordering::SeqCst), 1);
    assert_eq!(backend.checks.load(Ordering::SeqCst), 1);
    assert_eq!(
        backend.credentials.lock().unwrap().lark_app_id,
        "stored-app"
    );
}
#[test]
fn manual_lark_token_is_saved_without_starting_oauth() {
    let backend = Arc::new(Backend::default());
    let (_root, worker) = startup(Mode::Live, backend.clone());
    event(&worker);
    let mut input = credentials();
    input.lark_user_token = "injected-secret-manual-token".into();
    send(
        &worker,
        Job::SaveAndConnect {
            tool: CliTool::Lark,
            credentials: input,
        },
    );
    let events = finished(&worker, CliTool::Lark);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::CredentialsSaved(_)))
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::AuthorizationRequired { .. }))
    );
    assert!(
        matches!(events.last(), Some(Event::ConnectionReport { report, .. }) if !report.connected)
    );
    assert_eq!(backend.connects.load(Ordering::SeqCst), 0);
}
#[test]
fn local_probe_does_not_load_credentials_or_check_an_account() {
    let backend = Arc::new(Backend::default());
    let (_root, worker) = startup(Mode::Live, backend.clone());
    event(&worker);
    let loads = backend.loads.load(Ordering::SeqCst);
    send(&worker, Job::ProbeCli(CliTool::Tapd));
    let events = until(&worker, |event| matches!(event, Event::CliProbe { .. }));
    assert!(
        events.iter().any(
            |event| matches!(event, Event::Activity { key, active: true } if key == "tapd-cli")
        )
    );
    assert!(matches!(
        events.last(),
        Some(Event::CliProbe {
            version: Some(_),
            error: None,
            ..
        })
    ));
    assert_eq!(backend.loads.load(Ordering::SeqCst), loads);
    assert_eq!(backend.connects.load(Ordering::SeqCst), 0);
    assert_eq!(backend.checks.load(Ordering::SeqCst), 0);
}
#[test]
fn missing_cli_blocks_save_and_reports_check_failure_for_selected_service() {
    let backend = Arc::new(Backend {
        fail_probe: true,
        ..Default::default()
    });
    let (_root, worker) = startup(Mode::Live, backend.clone());
    event(&worker);
    send(
        &worker,
        Job::SaveAndConnect {
            tool: CliTool::Tapd,
            credentials: credentials(),
        },
    );
    let events = until(&worker, |event| {
        matches!(
            event,
            Event::Error(_) | Event::SettingResult { error: Some(_), .. }
        )
    });
    assert!(events.iter().any(|event| matches!(event, Event::SettingResult { field: "tapd-connection", error:Some(message), .. } if message.contains("not executable"))));
    assert_eq!(backend.saves.load(Ordering::SeqCst), 0);
    send(&worker, Job::Check(CliTool::Tapd));
    let events = finished(&worker, CliTool::Tapd);
    assert!(
        matches!(events.last(), Some(Event::ConnectionReport { report, .. }) if report.failure.as_deref() == Some("CLI is not executable"))
    );
    assert_eq!(backend.checks.load(Ordering::SeqCst), 0);
}
#[test]
fn authorization_url_arrives_before_wait_and_clear_rejects_late_completion() {
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let backend = Arc::new(Backend {
        wait_for_auth: Some(barrier.clone()),
        ..Default::default()
    });
    let (_root, worker) = startup(Mode::Live, backend.clone());
    event(&worker);
    send(
        &worker,
        Job::SaveAndConnect {
            tool: CliTool::Lark,
            credentials: credentials(),
        },
    );
    let events = until(&worker, |event| {
        matches!(event, Event::AuthorizationRequired { .. })
    });
    assert!(
        matches!(events.last(), Some(Event::AuthorizationRequired { verification_url, expires_in: 60 }) if verification_url == "https://accounts.feishu.cn/device?user_code=public-code")
    );
    send(&worker, Job::ClearCredentials(CliTool::Lark));
    let events = until(&worker, |event| {
        matches!(
            event,
            Event::State {
                message: "credentials_cleared",
                ..
            }
        )
    });
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::AuthorizationFinished))
    );
    barrier.wait();
    let deadline = Instant::now() + Duration::from_secs(2);
    while backend.cancellations.load(Ordering::SeqCst) == 0 {
        assert!(
            Instant::now() < deadline,
            "backend did not observe cancellation"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(backend.mutations.load(Ordering::SeqCst), 0);
    // 迟到完成若未被拒绝，会出现在后续状态事件之前或之后的队列中。
    std::thread::sleep(Duration::from_millis(50));
    send(
        &worker,
        Job::SetPreference(Preference::Theme("dark".into())),
    );
    let events = until(&worker, |event| {
        matches!(
            event,
            Event::State {
                message: "settings_saved",
                ..
            }
        )
    });
    assert!(!events.iter().any(|event| matches!(
        event,
        Event::ConnectionReport { .. } | Event::AuthorizationRequired { .. }
    )));
}
#[test]
fn test_mock_probe_and_rejected_mutations_never_touch_system_backend() {
    let backend = Arc::new(Backend::default());
    let (_root, worker) = startup(Mode::Mock, backend.clone());
    event(&worker);
    send(&worker, Job::ProbeCli(CliTool::Lark));
    let events = until(&worker, |event| matches!(event, Event::CliProbe { .. }));
    assert!(matches!(
        events.last(),
        Some(Event::CliProbe {
            version: Some(_),
            error: None,
            ..
        })
    ));
    for job in [
        Job::SaveAndConnect {
            tool: CliTool::Lark,
            credentials: credentials(),
        },
        Job::ClearCredentials(CliTool::Tapd),
    ] {
        send(&worker, job);
        until(&worker, |event| {
            matches!(
                event,
                Event::Error(_) | Event::SettingResult { error: Some(_), .. }
            )
        });
    }
    assert_eq!(backend.loads.load(Ordering::SeqCst), 0);
    assert_eq!(backend.saves.load(Ordering::SeqCst), 0);
    assert_eq!(backend.clears.load(Ordering::SeqCst), 0);
    assert_eq!(backend.connects.load(Ordering::SeqCst), 0);
}
#[test]
fn replacing_authorization_keeps_new_activity_and_ignores_old_progress() {
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let backend = Arc::new(Backend {
        wait_for_auth: Some(barrier.clone()),
        ..Default::default()
    });
    let (_root, worker) = startup(Mode::Live, backend.clone());
    event(&worker);
    send(
        &worker,
        Job::SaveAndConnect {
            tool: CliTool::Lark,
            credentials: credentials(),
        },
    );
    until(&worker, |event| {
        matches!(event, Event::AuthorizationRequired { .. })
    });
    send(
        &worker,
        Job::SaveAndConnect {
            tool: CliTool::Lark,
            credentials: credentials(),
        },
    );
    let mut events = until(&worker, |event| {
        matches!(
            event,
            Event::State {
                message: "credentials_saved",
                ..
            }
        )
    });
    send(
        &worker,
        Job::SetPreference(Preference::Theme("light".into())),
    );
    events.extend(until(&worker, |event| {
        matches!(
            event,
            Event::State {
                message: "settings_saved",
                ..
            }
        )
    }));
    // 旧原生写入区故意保持未退出；替换操作不能仅靠取消信号就进入。
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        backend.connects.load(Ordering::SeqCst),
        1,
        "replacement entered native mutation before old operation exited"
    );
    barrier.wait();
    events.extend(until(&worker, |event| {
        matches!(event, Event::AuthorizationRequired { .. })
    }));
    let active = events
        .iter()
        .filter_map(|event| match event {
            Event::Activity { key, active } if key == "lark-connection" => Some(*active),
            _ => None,
        })
        .next_back();
    // 确定性地将旧授权和旧探测结果排到偏好请求之前，不依赖线程定时。
    worker
        .jobs
        .0
        .send(Envelope::Authorization {
            generation: 1,
            verification_url: "stale-url".into(),
            expires_in: 1,
        })
        .unwrap();
    worker
        .jobs
        .0
        .send(Envelope::Complete(Completion {
            key: "lark-connection".into(),
            generation: 1,
            attachment_origin: None,
            result: Ok((
                String::new(),
                ReadData::ConnectionReport {
                    binding: None,
                    tool: CliTool::Lark,
                    report: report(false),
                    authorization: true,
                },
            )),
        }))
        .unwrap();
    worker
        .jobs
        .0
        .send(Envelope::Complete(Completion {
            key: "lark-cli".into(),
            generation: 1,
            attachment_origin: None,
            result: Ok((
                String::new(),
                ReadData::CliProbe {
                    tool: CliTool::Lark,
                    version: Some("stale-version".into()),
                    error: None,
                },
            )),
        }))
        .unwrap();
    send(
        &worker,
        Job::SetPreference(Preference::Theme("dark".into())),
    );
    let later = until(&worker, |event| {
        matches!(
            event,
            Event::State {
                message: "settings_saved",
                ..
            }
        )
    });
    barrier.wait();
    assert_eq!(
        active,
        Some(true),
        "old cancellation hid the new authorization activity"
    );
    assert!(!later.iter().any(|event| matches!(
        event,
        Event::AuthorizationRequired { .. }
            | Event::AuthorizationFinished
            | Event::ConnectionReport { .. }
            | Event::CliProbe { .. }
    )));
}

#[cfg(unix)]
#[test]
fn readiness_runs_version_and_rejects_a_plain_file() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("lark-cli");
    std::fs::write(&path, "#!/bin/sh\necho 'lark-cli version 1.0.96'\n").unwrap();
    let settings = Settings {
        lark_path: path.to_string_lossy().into_owned(),
        ..Default::default()
    };
    assert!(
        SystemConnectionBackend
            .probe(CliTool::Lark, &settings)
            .is_err()
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        SystemConnectionBackend
            .probe(CliTool::Lark, &settings)
            .is_ok()
    );
}

#[test]
fn expired_poll_deadline_is_suspended_until_authorization_finishes() {
    let settings = Settings {
        poll_enabled: true,
        ..Default::default()
    };
    let now = Instant::now();
    let deadline = now - Duration::from_secs(60);
    let mut pending = BTreeMap::new();
    begin_read(&mut pending, "lark-connection", 1, None);
    for tick in 0..100 {
        assert_eq!(
            poll_wait(
                &settings,
                true,
                deadline,
                now + Duration::from_secs(tick),
                &pending
            ),
            None,
            "an expired deadline must not enqueue repeated rejected refreshes during authorization"
        );
    }
    finish_read(&mut pending, "lark-connection", 1);
    assert_eq!(
        poll_wait(&settings, true, deadline, now, &pending),
        Some(Duration::ZERO)
    );
    assert_eq!(poll_wait(&settings, false, deadline, now, &pending), None);
}

#[test]
fn rediscovery_during_authorization_keeps_the_current_cli_and_login() {
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let backend = Arc::new(Backend {
        wait_for_auth: Some(barrier.clone()),
        ..Default::default()
    });
    let (root, worker, previous) = startup_with_configured_cli(backend.clone());
    event(&worker);
    send(
        &worker,
        Job::SaveAndConnect {
            tool: CliTool::Lark,
            credentials: credentials(),
        },
    );
    until(&worker, |e| {
        matches!(e, Event::AuthorizationRequired { .. })
    });
    let before = persisted_lark_path(root.path());
    std::fs::remove_file(previous).unwrap();
    let binary = private_cli(root.path());
    send(&worker, Job::ProbeCli(CliTool::Lark));
    let events = until(&worker, |e| matches!(e, Event::Error(_)));
    barrier.wait();
    until(&worker, |e| matches!(e, Event::ConnectionReport { .. }));
    assert!(events.iter().any(|e| matches!(e, Event::FormError { field: "lark-cli", message } if message.contains("Connection in progress"))));
    assert_eq!(persisted_lark_path(root.path()), before);
    assert_eq!(backend.cancellations.load(Ordering::SeqCst), 0);
    assert_eq!(backend.mutations.load(Ordering::SeqCst), 1);
    assert!(binary.exists());
}

#[test]
fn failed_install_retains_reviewed_plan_for_explicit_retry() {
    let backend = Arc::new(Backend::default());
    let (root, worker) = startup(Mode::Live, backend);
    event(&worker);
    send(&worker, Job::PlanInstall(CliTool::Lark));
    until(&worker, |e| matches!(e, Event::InstallPlan(_)));
    // 使安全目录检查直接失败，不调用下载器或安装命令。
    std::fs::write(root.path().join("tools"), "not a directory").unwrap();
    for _ in 0..2 {
        send(&worker, Job::Install(CliTool::Lark));
        let events = until(&worker, |e| matches!(e, Event::SettingResult { .. }));
        assert!(events.iter().any(|e| matches!(e, Event::SettingResult { error: Some(message), .. } if message.contains("Installation directory"))), "retry lost reviewed plan");
    }
}

// 子进程仅使用本模块内存后端，绝不启动真实 UI 或触碰 Keychain。
#[test]
fn exit_barrier_child() {
    let Some(root) = std::env::var_os("LARK2TAPD_EXIT_TEST_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let backend = Arc::new(Backend {
        native_probe: true,
        ..Default::default()
    });
    let worker = start_with_backend(root.clone(), Mode::Live, backend);
    until(&worker, |e| matches!(e, Event::ConnectionStatus { .. }));
    crate::shutdown_exit::install(worker.jobs.clone()).unwrap();
    send(&worker, Job::ProbeCli(CliTool::Lark));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root.join("ready").exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    if std::env::var_os("LARK2TAPD_EXIT_TEST_RETURN").is_some() {
        // 正常返回路径仍允许已显式请求退出；重复请求不可消耗掉唯一 ack。
        worker.jobs.shutdown();
        worker.jobs.shutdown();
        return;
    }
    std::process::exit(0);
}

#[test]
fn process_exit_and_normal_return_wait_for_cli_cleanup() {
    use std::os::unix::fs::PermissionsExt;
    for normal_return in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("fake-lark-cli");
        std::fs::write(&path, format!("#!/bin/sh\nprintf ready > '{}'\nsleep 1\nprintf token > '{}'\necho 'lark-cli version 1.0.96'\n", root.path().join("ready").display(), root.path().join("late-token").display())).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            Engine::open(&root.path().join("preferences-live.db"))
                .await
                .unwrap()
                .save_settings(Settings {
                    lark_path: path.display().to_string(),
                    ..Default::default()
                })
                .await
                .unwrap();
        });
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "worker::connection_tests::exit_barrier_child",
                "--nocapture",
            ])
            .env("LARK2TAPD_EXIT_TEST_ROOT", root.path());
        if normal_return {
            command.env("LARK2TAPD_EXIT_TEST_RETURN", "1");
        } else {
            command.env_remove("LARK2TAPD_EXIT_TEST_RETURN");
        }
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                let _ = child.wait();
                panic!("exit barrier deadlocked");
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        assert!(status.success());
        std::thread::sleep(Duration::from_millis(1200));
        assert!(
            !root.path().join("late-token").exists(),
            "exit returned before CLI was cancelled and reaped"
        );
        let lock = std::fs::OpenOptions::new()
            .write(true)
            .open(root.path().join("app.lock"))
            .unwrap();
        lock.try_lock().unwrap();
    }
}

#[test]
fn shutdown_cancels_probe_process_before_ack() {
    use std::os::unix::fs::PermissionsExt;
    let backend = Arc::new(Backend {
        native_probe: true,
        ..Default::default()
    });
    let (root, worker, path) = startup_with_configured_cli(backend);
    event(&worker);
    let ready = root.path().join("probe-ready");
    let marker = root.path().join("late-write");
    std::fs::write(&path, format!("#!/bin/sh\nprintf ready > '{}'\nsleep 1\nprintf token > '{}'\necho 'lark-cli version 1.0.96'\n", ready.display(), marker.display())).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    send(&worker, Job::ProbeCli(CliTool::Lark));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    let done = worker.jobs.shutdown();
    while !done.is_closed() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        !marker.exists(),
        "shutdown allowed in-flight CLI to write after cancellation"
    );
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .open(root.path().join("app.lock"))
        .unwrap();
    lock.try_lock().unwrap();
}

#[test]
fn shutdown_ack_waits_for_native_exit_and_rejects_new_writes() {
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let backend = Arc::new(Backend {
        wait_for_auth: Some(barrier.clone()),
        ..Default::default()
    });
    let (root, worker) = startup(Mode::Live, backend.clone());
    event(&worker);
    send(
        &worker,
        Job::SaveAndConnect {
            tool: CliTool::Lark,
            credentials: credentials(),
        },
    );
    until(&worker, |e| {
        matches!(e, Event::AuthorizationRequired { .. })
    });
    let done = worker.jobs.shutdown();
    assert!(
        worker
            .jobs
            .send(Request {
                sequence: 0,
                profile: String::new(),
                job: Job::ClearCredentials(CliTool::Lark)
            })
            .is_err()
    );
    std::thread::sleep(Duration::from_millis(50));
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .open(root.path().join("app.lock"))
        .unwrap();
    let held = lock.try_lock().is_err();
    let early_ack = done.is_closed();
    if !held {
        lock.unlock().unwrap();
    }
    barrier.wait();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done.is_closed() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        held && !early_ack,
        "shutdown acknowledged before native task exited"
    );
    assert_eq!(backend.cancellations.load(Ordering::SeqCst), 1);
    assert_eq!(backend.mutations.load(Ordering::SeqCst), 0);
    assert_eq!(backend.clears.load(Ordering::SeqCst), 0);
    lock.try_lock().unwrap();
}

#[test]
fn tapd_check_does_not_probe_missing_lark_cli() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let tapd = root.path().join("tapd-cli");
    std::fs::write(&tapd, "#!/bin/sh\ncase \"$1\" in --version) echo 1.8.1;; --help) echo 'Usage: tapd-cli [options] [command]';; *) exit 9;; esac\n").unwrap();
    std::fs::set_permissions(&tapd, std::fs::Permissions::from_mode(0o700)).unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        Engine::open(&root.path().join("preferences-live.db"))
            .await
            .unwrap()
            .save_settings(Settings {
                lark_path: root.path().join("missing-lark-cli").display().to_string(),
                tapd_path: tapd.display().to_string(),
                ..Default::default()
            })
            .await
            .unwrap();
    });
    let backend = Arc::new(Backend {
        native_probe: true,
        ..Default::default()
    });
    let worker = start_with_backend(root.path().into(), Mode::Live, backend.clone());
    until(&worker, |e| matches!(e, Event::ConnectionStatus { .. }));
    send(
        &worker,
        Job::TestConnection {
            tool: CliTool::Tapd,
            credentials: credentials(),
        },
    );
    let events = finished(&worker, CliTool::Tapd);
    assert!(
        events.iter().any(
            |e| matches!(e, Event::ConnectionReport { report, .. } if report.failure.is_none())
        )
    );
    assert_eq!(backend.checks.load(Ordering::SeqCst), 1);
    assert_eq!(
        *backend.probed_paths.lock().unwrap(),
        [tapd.display().to_string()]
    );
    assert_eq!(backend.saves.load(Ordering::SeqCst), 0);
}

// CLI 的业务命令能成功返回也不能绕过协议门禁，更不能先访问凭据。
#[test]
fn unsupported_cli_blocks_account_business_before_credentials_or_commands() {
    use std::os::unix::fs::PermissionsExt;
    for bad_tool in [CliTool::Lark, CliTool::Tapd] {
        let root = tempfile::tempdir().unwrap();
        let marker = root.path().join("business-called");
        let mut settings = Settings {
            last_profile: Some("a".repeat(64)),
            ..Default::default()
        };
        for tool in [CliTool::Lark, CliTool::Tapd] {
            let path = root.path().join(format!("{tool:?}-cli"));
            let version = if tool == bad_tool {
                "99.0.0"
            } else if tool == CliTool::Lark {
                "1.0.96"
            } else {
                "1.15.0"
            };
            let version = if tool == CliTool::Lark {
                format!("lark-cli version {version}")
            } else {
                version.to_string()
            };
            std::fs::write(&path, format!("#!/bin/sh\nif [ \"$1\" = --version ]; then echo '{version}'; exit 0; fi\nif [ \"$1\" = --help ]; then echo 'Usage: tapd-cli [options] [command]'; exit 0; fi\nprintf called >> '{}'\nif [ \"$1\" = auth ]; then echo '{{\"appId\":\"injected-secret-app-id\",\"brand\":\"feishu\",\"identity\":\"user\",\"verified\":true,\"identities\":{{\"user\":{{\"openId\":\"user\"}}}}}}'; else echo '{{\"status\":1,\"data\":{{\"id\":\"1\"}}}}'; fi\n", marker.display())).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            match tool {
                CliTool::Lark => settings.lark_path = path.display().to_string(),
                CliTool::Tapd => settings.tapd_path = path.display().to_string(),
            }
        }
        // 正对照：不经过应用门禁时，未知版本确实能成功执行身份业务命令。
        let mut business_credentials = credentials();
        business_credentials.lark_user_token.clear();
        assert!(
            CliGateway::new(settings.clone(), business_credentials)
                .unwrap()
                .account_namespace()
                .is_ok()
        );
        assert!(marker.exists());
        std::fs::remove_file(&marker).unwrap();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            Engine::open(&root.path().join("preferences-live.db"))
                .await
                .unwrap()
                .save_settings(settings)
                .await
                .unwrap();
        });
        let backend = Arc::new(Backend {
            native_probe: true,
            credentials: Mutex::new(credentials()),
            ..Default::default()
        });
        let worker = start_with_backend(root.path().into(), Mode::Live, backend.clone());
        until(&worker, |e| matches!(e, Event::ConnectionStatus { .. }));
        let loads = backend.loads.load(Ordering::SeqCst);
        let source = SourceTask {
            guid: "task".into(),
            title: "title".into(),
            description: String::new(),
            url: String::new(),
            due: None,
            completed: false,
            metadata: Default::default(),
        };
        let draft = Draft {
            title: "title".into(),
            title_prefix: String::new(),
            attachments: vec![],
            description: String::new(),
            workspace_id: "1".into(),
            owner: String::new(),
            due: None,
        };
        for job in [
            Job::Refresh,
            Job::LoadProjects,
            Job::LoadMembers("1".into()),
            Job::Submit("task".into(), draft, source),
            Job::Recover {
                guid: "task".into(),
                workspace: "1".into(),
                id: "2".into(),
                kind: "story".into(),
            },
        ] {
            worker
                .jobs
                .send(Request {
                    sequence: 0,
                    profile: "a".repeat(64),
                    job,
                })
                .unwrap();
            let events = until(&worker, |e| matches!(e, Event::Error(_)));
            assert!(events.iter().any(|e| matches!(e, Event::Error(message) if message.contains("supported") || message.contains("Unsupported"))), "missing version error");
            assert_eq!(
                backend.loads.load(Ordering::SeqCst),
                loads,
                "credentials accessed before compatibility gate"
            );
            assert!(
                !marker.exists(),
                "business CLI called before compatibility gate"
            );
        }
        assert_eq!(backend.saves.load(Ordering::SeqCst), 0);
        assert_eq!(backend.clears.load(Ordering::SeqCst), 0);
    }
}

// 删除最后一个 sender 时，也必须等已从 pending 移除的原生任务真正退出。
#[test]
fn sender_drop_holds_lock_until_cancelled_native_task_exits() {
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let backend = Arc::new(Backend {
        wait_for_auth: Some(barrier.clone()),
        ..Default::default()
    });
    let (root, worker) = startup(Mode::Live, backend.clone());
    event(&worker);
    send(
        &worker,
        Job::SaveAndConnect {
            tool: CliTool::Lark,
            credentials: credentials(),
        },
    );
    until(&worker, |event| {
        matches!(event, Event::AuthorizationRequired { .. })
    });
    send(&worker, Job::ClearCredentials(CliTool::Lark));
    until(&worker, |event| {
        matches!(event, Event::AuthorizationFinished)
    });
    drop(worker);
    std::thread::sleep(Duration::from_millis(100));
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .open(root.path().join("app.lock"))
        .unwrap();
    let held = lock.try_lock().is_err();
    if !held {
        lock.unlock().unwrap();
    }
    barrier.wait();
    let deadline = Instant::now() + Duration::from_secs(5);
    while backend.cancellations.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        held,
        "actor released app.lock while cancelled native operation was still alive"
    );
    while lock.try_lock().is_err() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn account_refresh_during_authorization_does_not_cancel_login() {
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let backend = Arc::new(Backend {
        wait_for_auth: Some(barrier.clone()),
        ..Default::default()
    });
    let (_root, worker) = startup(Mode::Live, backend.clone());
    event(&worker);
    send(
        &worker,
        Job::SaveAndConnect {
            tool: CliTool::Lark,
            credentials: credentials(),
        },
    );
    until(&worker, |event| {
        matches!(event, Event::AuthorizationRequired { .. })
    });
    send(&worker, Job::Refresh);
    let events = until(&worker, |event| {
        matches!(
            event,
            Event::Error(_) | Event::SettingResult { error: Some(_), .. }
        )
    });
    assert!(events.iter().any(|event| matches!(event, Event::FormError { field: "tasks", message } if message.contains("Connection in progress"))));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::AuthorizationFinished))
    );
    barrier.wait();
    until(&worker, |event| {
        matches!(event, Event::ConnectionReport { .. })
    });
    assert_eq!(backend.cancellations.load(Ordering::SeqCst), 0);
    assert_eq!(backend.mutations.load(Ordering::SeqCst), 1);
}
