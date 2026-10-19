//! 全轮共享截止时间、最多四路详情读取；进度仅包含阶段和计数，不含账号或任务内容。
use super::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshPhase {
    LarkIdentity,
    TapdIdentity,
    ListPage,
    TaskDetails,
}
impl RefreshPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LarkIdentity => "lark_identity",
            Self::TapdIdentity => "tapd_identity",
            Self::ListPage => "lark_list_page",
            Self::TaskDetails => "lark_task_details",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshFailure {
    RequestTimeout,
    BudgetExceeded,
    RateLimited,
    Cancelled,
    Other,
}
impl RefreshFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RequestTimeout => "request_timeout",
            Self::BudgetExceeded => "refresh_budget_timeout",
            Self::RateLimited => "rate_limited",
            Self::Cancelled => "cancelled",
            Self::Other => "request_failed",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshOutcome {
    Started,
    Progress,
    Finished,
    RateLimited { attempt: u32, wait_ms: u64 },
    Failed(RefreshFailure),
}
#[derive(Clone, Copy, Debug)]
pub struct RefreshEvent {
    pub phase: RefreshPhase,
    pub outcome: RefreshOutcome,
    pub page: usize,
    pub completed: usize,
    pub scheduled: usize,
    pub detail_index: Option<usize>,
    pub elapsed_ms: u128,
    pub remaining_ms: u128,
}
#[derive(Debug)]
pub(crate) struct BudgetExceeded;
impl std::fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "Lark refresh deadline exceeded (refresh budget timeout); no partial results accepted",
        )
    }
}
impl std::error::Error for BudgetExceeded {}

struct Trace<'a> {
    started: Instant,
    deadline: Instant,
    cancelled: &'a AtomicBool,
    observer: &'a mut dyn FnMut(RefreshEvent),
    page: usize,
    completed: usize,
    scheduled: usize,
}
impl Trace<'_> {
    fn emit(&mut self, phase: RefreshPhase, outcome: RefreshOutcome, index: Option<usize>) {
        (self.observer)(RefreshEvent {
            phase,
            outcome,
            page: self.page,
            completed: self.completed,
            scheduled: self.scheduled,
            detail_index: index,
            elapsed_ms: self.started.elapsed().as_millis(),
            remaining_ms: self
                .deadline
                .saturating_duration_since(Instant::now())
                .as_millis(),
        });
    }
    fn checkpoint(&self) -> Result<()> {
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(process::Cancelled.into());
        }
        remaining(self.deadline).map(|_| ())
    }
    fn fail(
        &mut self,
        phase: RefreshPhase,
        index: Option<usize>,
        error: anyhow::Error,
    ) -> anyhow::Error {
        self.fail_at(phase, index, error, Instant::now())
    }
    fn fail_at(
        &mut self,
        phase: RefreshPhase,
        index: Option<usize>,
        error: anyhow::Error,
        failed_at: Instant,
    ) -> anyhow::Error {
        // 并发兄弟的回收可能更晚结束；按原始失败时刻区分单次超时与整轮预算。
        let kind = if error.is::<BudgetExceeded>()
            || (error.is::<process::CallTimeout>() && failed_at >= self.deadline)
        {
            RefreshFailure::BudgetExceeded
        } else if error.is::<process::CallTimeout>() {
            RefreshFailure::RequestTimeout
        } else if error.is::<process::Cancelled>() {
            RefreshFailure::Cancelled
        } else if error
            .downcast_ref::<diagnostics::ApiFailure>()
            .is_some_and(|e| e.code == Some(99991400))
        {
            RefreshFailure::RateLimited
        } else {
            RefreshFailure::Other
        };
        self.emit(phase, RefreshOutcome::Failed(kind), index);
        let reason = match kind {
            RefreshFailure::RequestTimeout => "one CLI request exceeded its time limit".to_owned(),
            RefreshFailure::BudgetExceeded => {
                "the shared refresh time budget was exhausted or cannot fit the required rate-limit cooldown".to_owned()
            }
            RefreshFailure::Cancelled => "the refresh was cancelled".to_owned(),
            RefreshFailure::Other | RefreshFailure::RateLimited => error.to_string(),
        };
        anyhow::anyhow!(
            "Task refresh {} at {} (page={}, detail={}, completed={}, scheduled={}, elapsed_ms={}): {}. Cached tasks unchanged",
            kind.as_str(),
            phase.as_str(),
            self.page,
            index.unwrap_or(0),
            self.completed,
            self.scheduled,
            self.started.elapsed().as_millis(),
            reason
        )
    }
    fn call<T>(
        &mut self,
        phase: RefreshPhase,
        operation: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        self.emit(phase, RefreshOutcome::Started, None);
        let result = self
            .checkpoint()
            .and_then(|_| operation(self))
            .and_then(|value| {
                self.checkpoint()?;
                Ok(value)
            });
        match result {
            Ok(value) => {
                self.emit(phase, RefreshOutcome::Finished, None);
                Ok(value)
            }
            Err(error) => Err(self.fail(phase, None, error)),
        }
    }
}
impl CliGateway {
    /// 身份校验与任务读取共享120秒；同一轮只取得一次飞书已验证主体。
    pub fn fetch_account_tasks(
        &self,
        cancelled: &AtomicBool,
        mut observer: impl FnMut(RefreshEvent),
    ) -> Result<(String, Vec<SourceTask>)> {
        let (namespace, tasks) = read(
            self,
            Instant::now() + Duration::from_secs(120),
            cancelled,
            true,
            &mut observer,
        )?;
        Ok((
            namespace.expect("account refresh validates its namespace"),
            tasks,
        ))
    }
}

pub(super) fn read(
    gateway: &CliGateway,
    deadline: Instant,
    cancelled: &AtomicBool,
    account: bool,
    observer: &mut dyn FnMut(RefreshEvent),
) -> Result<(Option<String>, Vec<SourceTask>)> {
    let mut trace = Trace {
        started: Instant::now(),
        deadline,
        cancelled,
        observer,
        page: 0,
        completed: 0,
        scheduled: 0,
    };
    let (user, identity) = trace.call(RefreshPhase::LarkIdentity, |_| {
        let status = gateway.lark_identity_until_cancellable(deadline, cancelled)?;
        ensure!(
            status.get("verified").and_then(Value::as_bool) == Some(true)
                && status.get("identity").and_then(Value::as_str) == Some("user"),
            "Lark user OAuth is not verified"
        );
        let user = status
            .pointer("/identities/user/openId")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("Lark user identity is missing"))?
            .to_owned();
        let identity = if account {
            Some(gateway.account_identity(&status)?)
        } else {
            None
        };
        Ok((user, identity))
    })?;
    let namespace = if let Some((brand, app, account_user)) = identity {
        Some(trace.call(RefreshPhase::TapdIdentity, |_| {
            let result = gateway.tapd_until("user", "info", Vec::new(), deadline, cancelled)?;
            let tapd_user = required(&result, "id")?;
            valid_id(&tapd_user)?;
            Ok(account_namespace_key(
                &brand,
                &app,
                &account_user,
                &tapd_user,
            ))
        })?)
    } else {
        None
    };
    // 跨分页共享速率与冷却，避免每页重置后突发请求。
    let pacer = read_pacing::ReadPacer::new();
    let mut tasks = Vec::new();
    let mut page = String::new();
    let mut pages = HashSet::new();
    let mut ids = HashSet::new();
    let mut count = 0;
    for page_index in 0..40 {
        trace.page = page_index + 1;
        let (data, jobs) = trace.call(RefreshPhase::ListPage, |trace| {
            let data = pacer.read(
                deadline,
                cancelled,
                || gateway.related_until_cancellable(&page, deadline, cancelled),
                |attempt, delay| {
                    trace.emit(
                        RefreshPhase::ListPage,
                        RefreshOutcome::RateLimited {
                            attempt,
                            wait_ms: delay.as_millis() as u64,
                        },
                        None,
                    )
                },
            )?;
            let items = data["items"]
                .as_array()
                .expect("validated related task list");
            count += items.len();
            ensure!(
                count <= 1000,
                "Lark refresh exceeds 1000 tasks; no partial results accepted"
            );
            let mut jobs = Vec::new();
            for item in items {
                let guid = required(item, "guid")?;
                valid_source_id(&guid)?;
                let (creator, assignees) = source_people(item);
                let related = creator.as_ref().is_some_and(|p| p.id == user)
                    || assignees.iter().any(|p| p.id == user);
                if related && ids.insert(guid.clone()) {
                    jobs.push(guid);
                }
            }
            Ok((data, jobs))
        })?;
        trace.scheduled += jobs.len();
        let details = read_details(gateway, &jobs, &pacer, &mut trace)?;
        // 列表与详情之间可能已完成或移交；只接受仍未完成且属于已验证主体的任务。
        tasks.extend(
            details
                .into_iter()
                .filter(|task| !task.completed && task.is_related_to(&user)),
        );
        if data["has_more"] == false {
            return Ok((namespace, tasks));
        }
        page = required(&data, "page_token")?;
        if !pages.insert(page.clone()) || page_index == 39 {
            return Err(trace.fail(
                RefreshPhase::ListPage,
                None,
                anyhow::anyhow!(
                    "Lark pagination did not finish within 40 pages; no partial results accepted"
                ),
            ));
        }
    }
    unreachable!("last page rejects continuation")
}

enum DetailEvent {
    Retry {
        index: usize,
        attempt: u32,
        delay: Duration,
    },
    Done {
        index: usize,
        finished_at: Instant,
        result: Result<Box<SourceTask>>,
    },
}

fn read_details(
    gateway: &CliGateway,
    jobs: &[String],
    pacer: &read_pacing::ReadPacer,
    trace: &mut Trace<'_>,
) -> Result<Vec<SourceTask>> {
    trace.emit(RefreshPhase::TaskDetails, RefreshOutcome::Started, None);
    trace
        .checkpoint()
        .map_err(|e| trace.fail(RefreshPhase::TaskDetails, None, e))?;
    let next = AtomicUsize::new(0);
    let (send, recv) = mpsc::channel();
    let deadline = trace.deadline;
    let cancelled = trace.cancelled;
    let mut results = vec![None; jobs.len()];
    let mut failure = None;
    std::thread::scope(|scope| {
        for _ in 0..jobs.len().min(4) {
            let send = send.clone();
            let next = &next;
            scope.spawn(move || {
                loop {
                    if cancelled.load(Ordering::SeqCst) {
                        break;
                    }
                    let index = next.fetch_add(1, Ordering::SeqCst);
                    if index >= jobs.len() {
                        break;
                    }
                    let result = pacer.read(
                        deadline,
                        cancelled,
                        || gateway.source_until_cancellable(&jobs[index], deadline, cancelled),
                        |attempt, delay| {
                            let _ = send.send(DetailEvent::Retry {
                                index,
                                attempt,
                                delay,
                            });
                        },
                    );
                    let failed = result.is_err();
                    // 先发送原始失败，随后取消兄弟进程，防止取消错误掩盖真正的超时/权限错误。
                    if send
                        .send(DetailEvent::Done {
                            index,
                            finished_at: Instant::now(),
                            result: result.map(Box::new),
                        })
                        .is_err()
                    {
                        break;
                    }
                    if failed {
                        cancelled.store(true, Ordering::SeqCst);
                        break;
                    }
                }
            });
        }
        drop(send);
        for event in recv {
            let (index, finished_at, result) = match event {
                DetailEvent::Retry {
                    index,
                    attempt,
                    delay,
                } => {
                    if failure.is_none() {
                        trace.emit(
                            RefreshPhase::TaskDetails,
                            RefreshOutcome::RateLimited {
                                attempt,
                                wait_ms: delay.as_millis() as u64,
                            },
                            Some(index + 1),
                        );
                    }
                    continue;
                }
                DetailEvent::Done {
                    index,
                    finished_at,
                    result,
                } => (index, finished_at, result),
            };
            match result {
                Ok(task) => {
                    results[index] = Some(*task);
                    trace.completed += 1;
                    if failure.is_none() && trace.completed.is_multiple_of(10) {
                        trace.emit(RefreshPhase::TaskDetails, RefreshOutcome::Progress, None);
                    }
                }
                Err(error) => {
                    if failure.is_none() {
                        failure = Some((index + 1, error, finished_at));
                    }
                    cancelled.store(true, Ordering::SeqCst);
                }
            }
        }
    });
    // scope 退出保证全部进程已返回/回收；失败不会暴露部分结果。
    if let Some((index, error, failed_at)) = failure {
        return Err(trace.fail_at(RefreshPhase::TaskDetails, Some(index), error, failed_at));
    }
    trace
        .checkpoint()
        .map_err(|e| trace.fail(RefreshPhase::TaskDetails, None, e))?;
    trace.emit(RefreshPhase::TaskDetails, RefreshOutcome::Finished, None);
    Ok(results
        .into_iter()
        .map(|task| task.expect("all scheduled details completed"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delayed_sibling_cleanup_does_not_relabel_the_original_timeout() {
        let now = Instant::now();
        let cancelled = AtomicBool::new(true);
        let mut events = Vec::new();
        {
            let mut observe = |e| events.push(e);
            let mut trace = Trace {
                started: now - Duration::from_secs(3),
                deadline: now - Duration::from_secs(1),
                cancelled: &cancelled,
                observer: &mut observe,
                page: 1,
                completed: 2,
                scheduled: 4,
            };
            let error = trace.fail_at(
                RefreshPhase::TaskDetails,
                Some(2),
                process::CallTimeout.into(),
                now - Duration::from_secs(2),
            );
            assert!(error.to_string().contains("request_timeout"));
        }
        assert_eq!(
            events.last().unwrap().outcome,
            RefreshOutcome::Failed(RefreshFailure::RequestTimeout)
        );
    }
    #[test]
    fn distinguishes_request_timeout_from_expired_refresh_budget() {
        for (expired, expected) in [
            (false, RefreshFailure::RequestTimeout),
            (true, RefreshFailure::BudgetExceeded),
        ] {
            let mut events = Vec::new();
            let now = Instant::now();
            let cancelled = AtomicBool::new(false);
            let error = {
                let mut observe = |e| events.push(e);
                let mut trace = Trace {
                    started: now,
                    deadline: if expired {
                        now - Duration::from_secs(1)
                    } else {
                        now + Duration::from_secs(30)
                    },
                    cancelled: &cancelled,
                    observer: &mut observe,
                    page: 2,
                    completed: 10,
                    scheduled: 15,
                };
                trace
                    .call::<()>(RefreshPhase::ListPage, |_| Err(process::CallTimeout.into()))
                    .unwrap_err()
            };
            let message = error.to_string();
            assert!(message.contains(expected.as_str()) && message.contains("list_page"));
            assert!(message.contains("Cached tasks unchanged"));
            assert_eq!(
                events.last().unwrap().outcome,
                RefreshOutcome::Failed(expected)
            );
            assert_eq!(events.last().unwrap().page, 2);
        }
    }
}
