//! 仅终端输出；日志只接收静态事件、白名单分类和计数，不输出凭证或业务正文。
use lark2tapd_core::{Snapshot, SyncStatus};
use std::{collections::HashMap, io::Write};

pub fn category(message: &str) -> &'static str {
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
    } else if ["timeout", "timed out", "expired", "过期"]
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
    format!("operation_failed category={}", category(error))
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
    fn errors_log_category_without_echoing_sensitive_payloads() {
        let line = failure_text("missing scope; token=private-token; body=private-task");
        assert!(line.contains("category=permission"));
        assert!(!line.contains("private"));
        assert_eq!(category("Cannot save credentials in Keychain"), "storage");
    }
}
