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
    fail_probe: bool,
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
    fn probe(&self, _: CliTool, _: &Settings) -> Result<String> {
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
