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
    loads: AtomicUsize,
    fail_save: bool,
    fail_auth: bool,
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
    fn connect(&self, _: CliTool, settings: &Settings, _: Credentials) -> Result<Option<String>> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        assert!(
            settings.last_profile.is_none(),
            "old namespace survived into auth"
        );
        anyhow::ensure!(!self.fail_auth, "injected-secret-auth-error");
        Ok(None)
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

#[test]
fn failed_persistence_never_authenticates_or_clears_the_form() {
    let backend = Arc::new(Backend {
        fail_save: true,
        ..Default::default()
    });
    let (_root, worker) = startup(Mode::Live, backend.clone());
    assert!(matches!(
        event(&worker),
        Event::ConnectionStatus {
            lark_saved: false,
            tapd_saved: false
        }
    ));
    send(
        &worker,
        Job::SaveAndConnect {
            tool: CliTool::Lark,
            credentials: credentials(),
        },
    );
    assert!(
        matches!(event(&worker), Event::Activity { active: true, key } if key == "lark-connection")
    );
    assert!(
        matches!(event(&worker), Event::Activity { active: false, key } if key == "lark-connection")
    );
    match event(&worker) {
        Event::FormError {
            field: "lark-connection",
            message,
        } => assert!(!message.contains("injected-secret")),
        _ => panic!("failed save must retain the form"),
    }
    match event(&worker) {
        Event::Error(message) => assert!(!message.contains("injected-secret")),
        _ => panic!("expected redacted log error"),
    }
    assert_eq!(backend.connects.load(Ordering::SeqCst), 0);
    assert_eq!(backend.saves.load(Ordering::SeqCst), 1);
}

#[test]
fn auth_failure_after_save_emits_saved_status_and_invalidates_namespace() {
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
    assert!(matches!(
        event(&worker),
        Event::Activity { active: true, .. }
    ));
    assert!(matches!(
        event(&worker),
        Event::CredentialsSaved(CliTool::Lark)
    ));
    assert!(matches!(
        event(&worker),
        Event::Diagnostic {
            tool: CliTool::Lark,
            connected: false,
            ..
        }
    ));
    assert!(matches!(
        event(&worker),
        Event::ConnectionStatus {
            lark_saved: true,
            tapd_saved: false
        }
    ));
    assert!(
        matches!(event(&worker), Event::State { profile, message: "credentials_saved", .. } if profile.is_empty())
    );
    match event(&worker) {
        Event::Diagnostic {
            tool: CliTool::Lark,
            connected: false,
            detail,
            ..
        } => assert!(!detail.contains("injected-secret")),
        _ => panic!("expected failed connection diagnostic"),
    }
    assert!(matches!(
        event(&worker),
        Event::Activity { active: false, .. }
    ));
    // 外层状态通知和错误中不能出现任何输入或 CLI 错误原文。
    loop {
        match event(&worker) {
            Event::State { profile, .. } => assert!(profile.is_empty()),
            Event::FormError {
                field: "lark-connection",
                message,
            } => assert!(!message.contains("injected-secret")),
            Event::Error(message) => {
                assert!(!message.contains("injected-secret"));
                break;
            }
            _ => panic!("unexpected event after failed connection"),
        }
    }
    assert_eq!(backend.connects.load(Ordering::SeqCst), 1);
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let engine = Engine::open(&root.path().join("preferences-live.db"))
            .await
            .unwrap();
        assert!(engine.snapshot().settings.last_profile.is_none());
    });
}

#[test]
fn clear_only_selected_service_emits_typed_confirmation_without_auth() {
    let backend = Arc::new(Backend::default());
    *backend.credentials.lock().unwrap() = credentials();
    let (_root, worker) = startup(Mode::Live, backend.clone());
    assert!(matches!(
        event(&worker),
        Event::ConnectionStatus {
            lark_saved: true,
            tapd_saved: true
        }
    ));
    send(&worker, Job::ClearCredentials(CliTool::Tapd));
    assert!(
        matches!(event(&worker), Event::Activity { active: true, key } if key == "tapd-connection")
    );
    assert!(matches!(
        event(&worker),
        Event::CredentialsCleared(CliTool::Tapd)
    ));
    assert!(matches!(
        event(&worker),
        Event::Diagnostic {
            tool: CliTool::Tapd,
            connected: false,
            ..
        }
    ));
    assert!(matches!(
        event(&worker),
        Event::ConnectionStatus {
            lark_saved: true,
            tapd_saved: false
        }
    ));
    assert!(
        matches!(event(&worker), Event::State { message: "credentials_cleared", profile, .. } if profile.is_empty())
    );
    assert!(
        matches!(event(&worker), Event::Activity { active: false, key } if key == "tapd-connection")
    );
    assert_eq!(backend.connects.load(Ordering::SeqCst), 0);
    assert_eq!(backend.clears.load(Ordering::SeqCst), 1);
}

#[test]
fn successful_tapd_save_preserves_lark_status_and_load_is_read_only() {
    let backend = Arc::new(Backend::default());
    backend.credentials.lock().unwrap().lark_app_id = "stored-app".into();
    let (_root, worker) = startup(Mode::Live, backend.clone());
    assert!(matches!(
        event(&worker),
        Event::ConnectionStatus {
            lark_saved: true,
            tapd_saved: false
        }
    ));
    send(
        &worker,
        Job::SaveAndConnect {
            tool: CliTool::Tapd,
            credentials: credentials(),
        },
    );
    assert!(
        matches!(event(&worker), Event::Activity { active: true, key } if key == "tapd-connection")
    );
    assert!(matches!(
        event(&worker),
        Event::CredentialsSaved(CliTool::Tapd)
    ));
    assert!(matches!(
        event(&worker),
        Event::Diagnostic {
            tool: CliTool::Tapd,
            connected: false,
            ..
        }
    ));
    assert!(matches!(
        event(&worker),
        Event::ConnectionStatus {
            lark_saved: true,
            tapd_saved: true
        }
    ));
    assert!(
        matches!(event(&worker), Event::State { message: "credentials_saved", profile, .. } if profile.is_empty())
    );
    assert!(matches!(
        event(&worker),
        Event::Diagnostic {
            tool: CliTool::Tapd,
            connected: true,
            ..
        }
    ));
    assert!(
        matches!(event(&worker), Event::Activity { active: false, key } if key == "tapd-connection")
    );
    assert!(matches!(
        event(&worker),
        Event::State {
            message: "credentials_saved",
            ..
        }
    ));
    send(&worker, Job::LoadConnectionStatus);
    assert!(matches!(
        event(&worker),
        Event::Activity { active: true, .. }
    ));
    assert!(matches!(
        event(&worker),
        Event::ConnectionStatus {
            lark_saved: true,
            tapd_saved: true
        }
    ));
    assert!(matches!(
        event(&worker),
        Event::Activity { active: false, .. }
    ));
    assert_eq!(backend.saves.load(Ordering::SeqCst), 1);
    assert_eq!(backend.connects.load(Ordering::SeqCst), 1);
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
    let mut saved = false;
    loop {
        match event(&worker) {
            Event::CredentialsSaved(CliTool::Lark) => saved = true,
            Event::Diagnostic {
                connected: true, ..
            } => panic!("manual token cannot be verified"),
            Event::Activity { active: false, .. } => break,
            _ => {}
        }
    }
    assert!(saved);
    assert_eq!(backend.connects.load(Ordering::SeqCst), 0);
}

#[test]
fn mock_status_and_rejected_mutations_never_touch_backend() {
    let backend = Arc::new(Backend::default());
    let (_root, worker) = startup(Mode::Mock, backend.clone());
    assert!(matches!(
        event(&worker),
        Event::ConnectionStatus {
            lark_saved: false,
            tapd_saved: false
        }
    ));
    send(&worker, Job::LoadConnectionStatus);
    assert!(
        matches!(event(&worker), Event::Activity { active: true, key } if key == "connection-status")
    );
    assert!(matches!(
        event(&worker),
        Event::ConnectionStatus {
            lark_saved: false,
            tapd_saved: false
        }
    ));
    assert!(
        matches!(event(&worker), Event::Activity { active: false, key } if key == "connection-status")
    );
    for job in [
        Job::SaveAndConnect {
            tool: CliTool::Lark,
            credentials: credentials(),
        },
        Job::ClearCredentials(CliTool::Tapd),
    ] {
        send(&worker, job);
        assert!(matches!(
            event(&worker),
            Event::Activity { active: true, .. }
        ));
        assert!(matches!(
            event(&worker),
            Event::Activity { active: false, .. }
        ));
        assert!(matches!(event(&worker), Event::FormError { .. }));
        assert!(matches!(event(&worker), Event::Error(_)));
    }
    assert_eq!(backend.loads.load(Ordering::SeqCst), 0);
    assert_eq!(backend.saves.load(Ordering::SeqCst), 0);
    assert_eq!(backend.clears.load(Ordering::SeqCst), 0);
    assert_eq!(backend.connects.load(Ordering::SeqCst), 0);
}
