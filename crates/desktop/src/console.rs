//! 仅终端输出；日志只接收静态事件、白名单分类和计数，不输出凭证或业务正文。
use lark2tapd_core::{Snapshot, SyncStatus};
use std::{collections::HashMap, io::Write};

// worker/UI 事件携带脱敏后的字符串；只解析 bridge::diagnostics 生成的固定头部，
// 不把后面的恢复建议当成错误原因，也不把任意 CLI 文本直接写入日志。
struct RequestDiagnostic {
    category: &'static str,
    exit: Option<i32>,
    code: Option<i64>,
}
fn request_diagnostic(message: &str) -> Option<RequestDiagnostic> {
    let header = ["CLI", "Lark", "TAPD"].into_iter().find_map(|provider| {
        message
            .split_once(&format!("{provider} request failed ("))
            .and_then(|(_, rest)| rest.split_once(')').map(|(header, _)| header))
    })?;
    let mut fields = header.split(", ");
    let category = match fields.next()? {
        "authentication" => "authentication",
        "authorization" | "policy" => "permission",
        "network" => "network",
        "validation" | "config" | "confirmation" => "validation",
        "api" => "api",
        "rate_limit" => "rate_limit",
        "internal" | "unknown" => "unknown",
        _ => return None,
    };
    let mut diagnostic = RequestDiagnostic {
        category,
        exit: None,
        code: None,
    };
    for field in fields {
        if let Some(exit) = field.strip_prefix("exit ") {
            diagnostic.exit = Some(exit.parse().ok()?);
        } else if let Some(code) = field.strip_prefix("API code ") {
            diagnostic.code = Some(code.parse().ok()?);
        } else {
            return None;
        }
    }
    if diagnostic.code == Some(99991400) {
        diagnostic.category = "rate_limit";
    }
    Some(diagnostic)
}

pub fn category(message: &str) -> &'static str {
    if let Some(diagnostic) = request_diagnostic(message) {
        return diagnostic.category;
    }
    let text = message.to_ascii_lowercase();
    if [
        "keychain", "sqlite", "database", "disk", "storage", "persist",
    ]
    .iter()
    .any(|v| text.contains(v))
    {
        "storage"
    } else if ["cancel", "取消"].iter().any(|v| text.contains(v)) {
        "cancelled"
    } else if [
        "timeout",
        "timed out",
        "deadline exceeded",
        "expired",
        "过期",
    ]
    .iter()
    .any(|v| text.contains(v))
    {
        "timeout"
    } else if ["permission", "scope", "consent", "权限"]
        .iter()
        .any(|v| text.contains(v))
    {
        "permission"
    } else if [
        "oauth",
        "token",
        "identity",
        "authentication",
        "authorize",
        "认证",
    ]
    .iter()
    .any(|v| text.contains(v))
    {
        "authentication"
    } else if ["network", "dns", "tls", "connection refused", "网络"]
        .iter()
        .any(|v| text.contains(v))
    {
        "network"
    } else if [
        "invalid",
        "must",
        "select",
        "enter",
        "configure",
        "cli",
        "不允许",
        "请输入",
    ]
    .iter()
    .any(|v| text.contains(v))
    {
        "validation"
    } else {
        "unknown"
    }
}
fn output(level: &str, module: &str, body: &str) {
    let line = format!(
        "{} [{level}] [{module}] {body}",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f")
    );
    #[cfg(not(test))]
    let _ = writeln!(std::io::stderr().lock(), "{line}");
    #[cfg(test)]
    eprintln!("{line}");
}
pub fn event(module: &'static str, action: &'static str) {
    output("INFO", module, action);
}
fn failure_text(error: &str) -> String {
    let mut text = format!("operation_failed category={}", category(error));
    if let Some(diagnostic) = request_diagnostic(error) {
        if let Some(exit) = diagnostic.exit {
            text.push_str(&format!(" exit_code={exit}"));
        }
        if let Some(code) = diagnostic.code {
            text.push_str(&format!(" api_code={code}"));
        }
    }
    text
}
pub fn failure(module: &'static str, error: &str) {
    output("ERROR", module, &failure_text(error));
}
pub fn setting_module(field: &str) -> &'static str {
    match field {
        "language" => "settings.language",
        "theme" => "settings.theme",
        "poll" | "poll-enabled" => "settings.polling",
        "destination" => "settings.destination",
        "prefixes" => "settings.prefixes",
        "new-workspace" => "settings.workspaces",
        "lark-connection" => "connection.lark",
        "tapd-connection" => "connection.tapd",
        "lark-cli" => "cli.lark",
        "tapd-cli" => "cli.tapd",
        "tasks" => "tasks.lark",
        "projects" | "members" => "directory.tapd",
        "submit" => "sync.tapd",
        _ => "settings",
    }
}
pub fn change(field: &'static str, phase: &'static str) {
    event(setting_module(field), phase);
}

pub fn install_panic_hook() {
    // 不打印可能包含凭证/正文的 panic payload；保留来源位置，按需打印符号堆栈。
    std::panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|l| {
                format!(
                    "{}:{}",
                    l.file().rsplit('/').next().unwrap_or("unknown"),
                    l.line()
                )
            })
            .unwrap_or_else(|| "unknown".into());
        output(
            "ERROR",
            "app.panic",
            &format!("panic_detected source={location} payload=redacted"),
        );
        if matches!(
            std::env::var("RUST_BACKTRACE").ok().as_deref(),
            Some("1" | "full")
        ) {
            let _ = writeln!(
                std::io::stderr().lock(),
                "{}",
                std::backtrace::Backtrace::force_capture()
            );
        }
    }));
}

#[derive(Default, Debug, PartialEq, Eq)]
struct Counts {
    read: usize,
    unfinished: usize,
    added: usize,
    changed: usize,
    unimported: usize,
    attention: usize,
    unsynced: usize,
}
fn counts(before: &Snapshot, after: &Snapshot) -> Counts {
    let previous: HashMap<_, _> = before
        .records
        .iter()
        .map(|r| (r.source.guid.as_str(), &r.source))
        .collect();
    let mut counts = Counts::default();
    for record in after.records.iter().filter(|r| r.in_scope) {
        counts.read += 1;
        counts.unfinished += usize::from(!record.source.completed);
        match previous.get(record.source.guid.as_str()) {
            None => counts.added += 1,
            Some(old) if **old != record.source => counts.changed += 1,
            _ => {}
        }
        let status = record.status();
        counts.unimported += usize::from(status == SyncStatus::Unimported);
        counts.attention += usize::from(
            record.has_pending_attachments()
                || matches!(status, SyncStatus::Changed | SyncStatus::Uncertain),
        );
        counts.unsynced +=
            usize::from(status != SyncStatus::Synced || record.has_pending_attachments());
    }
    counts
}
static REFRESH_IDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
pub fn refresh_start(trigger: &'static str) -> u64 {
    let id = REFRESH_IDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    output(
        "INFO",
        "tasks.refresh",
        &format!(
            "fetch_started id={id} trigger={trigger} proxy_env_set={}",
            lark2tapd_bridge::proxy_environment_present()
        ),
    );
    id
}
pub fn refresh_boundary(id: u64, phase: &'static str, event: &'static str) {
    output(
        "INFO",
        "tasks.refresh",
        &format!("id={id} phase={phase} event={event}"),
    );
}
fn refresh_text(id: u64, event: lark2tapd_bridge::RefreshEvent) -> String {
    use lark2tapd_bridge::RefreshOutcome;
    let outcome = match event.outcome {
        RefreshOutcome::Started => "started",
        RefreshOutcome::Progress => "progress",
        RefreshOutcome::Finished => "finished",
        RefreshOutcome::RateLimited { .. } => "rate_limit_wait",
        RefreshOutcome::Failed(kind) => kind.as_str(),
    };
    let retry = match event.outcome {
        RefreshOutcome::RateLimited { attempt, wait_ms } => {
            format!(" retry={attempt} wait_ms={wait_ms}")
        }
        _ => String::new(),
    };
    format!(
        "id={id} phase={} event={outcome} page={} completed={} scheduled={} detail={} elapsed_ms={} remaining_ms={}{retry}",
        event.phase.as_str(),
        event.page,
        event.completed,
        event.scheduled,
        event.detail_index.unwrap_or(0),
        event.elapsed_ms,
        event.remaining_ms
    )
}
pub fn refresh_progress(id: u64, event: lark2tapd_bridge::RefreshEvent) {
    let level = if matches!(event.outcome, lark2tapd_bridge::RefreshOutcome::Failed(_)) {
        "ERROR"
    } else if matches!(
        event.outcome,
        lark2tapd_bridge::RefreshOutcome::RateLimited { .. }
    ) {
        "WARN"
    } else {
        "INFO"
    };
    output(level, "tasks.refresh", &refresh_text(id, event));
}

pub fn tasks(before: &Snapshot, after: &Snapshot) {
    let c = counts(before, after);
    output(
        "INFO",
        "tasks.lark",
        &format!(
            "read={} unfinished={} added={} changed={} unimported={} attention={} unsynced={}",
            c.read, c.unfinished, c.added, c.changed, c.unimported, c.attention, c.unsynced
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use lark2tapd_core::{SourceTask, SyncRecord};
    fn record(id: &str) -> SyncRecord {
        SyncRecord {
            media_body_pending: false,
            title_prefix: String::new(),
            last_draft: None,
            in_scope: true,
            source: SourceTask {
                guid: id.into(),
                title: "private title".into(),
                description: "private body".into(),
                url: String::new(),
                due: None,
                completed: false,
                metadata: Default::default(),
            },
            baseline: None,
            target: None,
            target_workspace: None,
            draft: None,
            uncertain: false,
            last_error: None,
        }
    }
    #[test]
    fn summary_counts_actual_deltas_and_ignores_out_of_scope_records() {
        let before = Snapshot {
            records: vec![record("old"), record("gone")],
            ..Default::default()
        };
        let mut after = before.clone();
        after.records[0].source.completed = true;
        after.records[1].in_scope = false;
        after.records.push(record("new"));
        assert_eq!(
            counts(&before, &after),
            Counts {
                read: 2,
                unfinished: 1,
                added: 1,
                changed: 1,
                unimported: 2,
                attention: 0,
                unsynced: 2
            }
        );
    }
    #[test]
    fn pending_content_and_changed_tasks_are_included_in_unsynced_total() {
        let mut synced = record("synced");
        synced.target = Some(lark2tapd_core::TargetTask {
            id: "1".into(),
            url: String::new(),
            kind: "story".into(),
            title: String::new(),
            description: String::new(),
            owner: String::new(),
            due: None,
        });
        synced.baseline = Some(synced.source.clone());
        let mut changed = synced.clone();
        changed.source.guid = "changed".into();
        changed.baseline = Some(changed.source.clone());
        let mut pending = synced.clone();
        pending.source.guid = "pending".into();
        pending.baseline = Some(pending.source.clone());
        pending.media_body_pending = true;
        let before = Snapshot {
            records: vec![synced, changed, pending],
            ..Default::default()
        };
        let mut after = before.clone();
        after.records[1].source.title = "updated".into();
        assert_eq!(
            counts(&before, &after),
            Counts {
                read: 3,
                unfinished: 3,
                added: 0,
                changed: 1,
                unimported: 0,
                attention: 2,
                unsynced: 2
            }
        );
    }
    #[test]
    fn refresh_trace_identifies_provider_phase_and_timeout_kind() {
        use lark2tapd_bridge::{RefreshEvent, RefreshFailure, RefreshOutcome, RefreshPhase};
        let text = refresh_text(
            7,
            RefreshEvent {
                phase: RefreshPhase::TapdIdentity,
                outcome: RefreshOutcome::Failed(RefreshFailure::RequestTimeout),
                page: 0,
                completed: 0,
                scheduled: 0,
                detail_index: None,
                elapsed_ms: 31000,
                remaining_ms: 89000,
            },
        );
        assert!(text.contains("id=7 phase=tapd_identity event=request_timeout"));
        assert!(text.contains("elapsed_ms=31000 remaining_ms=89000"));
        assert_eq!(category("Lark refresh deadline exceeded"), "timeout");
    }
    #[test]
    fn request_categories_use_the_cli_type_not_recovery_advice() {
        for (kind, expected) in [
            ("api", "api"),
            ("unknown", "unknown"),
            ("authentication", "authentication"),
            ("authorization", "permission"),
            ("network", "network"),
            ("validation", "validation"),
            ("config", "validation"),
            ("policy", "permission"),
            ("internal", "unknown"),
            ("confirmation", "validation"),
        ] {
            let error = format!(
                "Task refresh request_failed at lark_task_details: CLI request failed ({kind}, exit 1, API code 999); check authentication, configuration and remote service status. Cached tasks unchanged"
            );
            assert_eq!(category(&error), expected, "{kind}");
        }
    }
    #[test]
    fn refresh_trace_reports_rate_limit_wait_without_payloads() {
        let line = refresh_text(
            9,
            lark2tapd_bridge::RefreshEvent {
                phase: lark2tapd_bridge::RefreshPhase::TaskDetails,
                outcome: lark2tapd_bridge::RefreshOutcome::RateLimited {
                    attempt: 2,
                    wait_ms: 2000,
                },
                page: 3,
                completed: 90,
                scheduled: 110,
                detail_index: Some(5),
                elapsed_ms: 20000,
                remaining_ms: 100000,
            },
        );
        assert!(line.contains("id=9 phase=lark_task_details event=rate_limit_wait"));
        assert!(line.contains("detail=5"));
        assert!(line.contains("retry=2 wait_ms=2000"));
    }
    #[test]
    fn lark_rate_limit_code_has_actionable_category() {
        assert_eq!(
            category("CLI request failed (api, exit 1, API code 99991400); inspect API code"),
            "rate_limit"
        );
        assert_eq!(
            category(
                "CLI request failed (rate_limit, exit 1, API code 99991400); wait before retrying"
            ),
            "rate_limit"
        );
        assert_eq!(
            category("CLI request failed (api, exit 1, API code 99991672); token expired"),
            "api"
        );
    }
    #[test]
    fn request_logs_include_only_validated_codes() {
        let line = failure_text(
            "CLI request failed (api, exit 8, API code 42); token=private-token; body=private-task",
        );
        assert!(line.contains("category=api"));
        assert!(line.contains("exit_code=8"));
        assert!(line.contains("api_code=42"));
        assert!(!line.contains("private"));
        for error in [
            "CLI request failed (api, exit private-token, API code private-task)",
            "CLI request failed (private-token, exit 8, API code 1)",
            "CLI request failed (api, exit 99999999999999999999999999, API code 1\\nprivate-task)",
        ] {
            let line = failure_text(error);
            assert!(!line.contains("private"));
            assert!(!line.contains("exit_code=8"));
        }
        assert_eq!(
            category("Lark request failed (api, API code 42); check authentication"),
            "api"
        );
        assert_eq!(
            category("TAPD request failed (unknown); check authentication"),
            "unknown"
        );
    }
    #[test]
    fn errors_log_category_without_echoing_sensitive_payloads() {
        let line = failure_text("missing scope; token=private-token; body=private-task");
        assert!(line.contains("category=permission"));
        assert!(!line.contains("private"));
        assert_eq!(category("Cannot save credentials in Keychain"), "storage");
    }
}
