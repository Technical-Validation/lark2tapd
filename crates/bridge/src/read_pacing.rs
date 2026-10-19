//! 仅供任务刷新中的只读请求使用：统一错峰启动、共享冷却，不重试写入。
use super::*;
use std::sync::{Mutex, atomic::Ordering};

pub(super) struct ReadPacer {
    next: Mutex<Instant>,
}
impl ReadPacer {
    pub fn new() -> Self {
        Self {
            next: Mutex::new(Instant::now()),
        }
    }

    fn acquire(&self, deadline: Instant, cancelled: &AtomicBool) -> Result<()> {
        loop {
            if cancelled.load(Ordering::SeqCst) {
                return Err(process::Cancelled.into());
            }
            remaining(deadline)?;
            let delay = {
                let mut next = self.next.lock().unwrap();
                let now = Instant::now();
                if now >= *next {
                    // 不预订多个未来时隙，避免服务端冷却期间仍有旧预约越过暂停。
                    *next = now + Duration::from_millis(125);
                    return Ok(());
                }
                next.saturating_duration_since(now)
            };
            std::thread::sleep(delay.min(Duration::from_millis(10)));
        }
    }

    pub fn read<T>(
        &self,
        deadline: Instant,
        cancelled: &AtomicBool,
        mut operation: impl FnMut() -> Result<T>,
        mut on_retry: impl FnMut(u32, Duration),
    ) -> Result<T> {
        for attempt in 0..=3 {
            self.acquire(deadline, cancelled)?;
            match operation() {
                Ok(value) => return Ok(value),
                Err(error) => {
                    let Some(failure) = error
                        .downcast_ref::<diagnostics::ApiFailure>()
                        .filter(|failure| failure.code == Some(99991400))
                    else {
                        return Err(error);
                    };
                    if attempt == 3 {
                        return Err(error);
                    }
                    let delay = failure
                        .retry_after
                        .unwrap_or(Duration::from_secs(1 << attempt));
                    let now = Instant::now();
                    // 绝不截短服务端等待后提前重试；预算放不下冷却就终止整轮。
                    if delay >= deadline.saturating_duration_since(now) {
                        return Err(refresh::BudgetExceeded.into());
                    }
                    {
                        let mut next = self.next.lock().unwrap();
                        *next = (*next).max(now + delay);
                    }
                    on_retry(attempt + 1, delay);
                }
            }
        }
        unreachable!("last attempt returns its error")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_readers_cooldown_also_pauses_other_readers() {
        let pacer = ReadPacer::new();
        let cancelled = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(5);
        let (send, recv) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let mut attempts = 0;
                pacer
                    .read(
                        deadline,
                        &cancelled,
                        || {
                            attempts += 1;
                            if attempts == 1 {
                                Err(diagnostics::api_failure(
                                    "Lark",
                                    &json!({"error":{"code":99991400,"retry_after_seconds":1}}),
                                    None,
                                ))
                            } else {
                                Ok(())
                            }
                        },
                        |_, _| send.send(Instant::now()).unwrap(),
                    )
                    .unwrap();
                assert_eq!(attempts, 2);
            });
            let cooled_at = recv.recv().unwrap();
            let other_started = pacer
                .read(
                    deadline,
                    &cancelled,
                    || Ok(Instant::now()),
                    |_, _| panic!("no failure"),
                )
                .unwrap();
            assert!(other_started.duration_since(cooled_at) >= Duration::from_millis(990));
        });
    }

    #[test]
    fn invalid_retry_delays_use_fallback_and_huge_delays_do_not_overflow() {
        for delay in [
            json!(0),
            json!(-1),
            json!("private-token"),
            json!(1.5),
            Value::Null,
        ] {
            let pacer = ReadPacer::new();
            let cancelled = AtomicBool::new(false);
            let mut reported = Vec::new();
            let error = pacer
                .read::<()>(
                    Instant::now() + Duration::from_secs(5),
                    &cancelled,
                    || {
                        Err(diagnostics::api_failure(
                            "Lark",
                            &json!({"error":{"code":99991400,"retry_after_seconds":delay}}),
                            Some(1),
                        ))
                    },
                    |attempt, wait| {
                        reported.push((attempt, wait));
                        cancelled.store(true, Ordering::SeqCst);
                    },
                )
                .unwrap_err();
            assert!(error.is::<process::Cancelled>());
            assert_eq!(reported, [(1, Duration::from_secs(1))]);
        }
        let error = ReadPacer::new()
            .read::<()>(
                Instant::now() + Duration::from_secs(5),
                &AtomicBool::new(false),
                || {
                    Err(diagnostics::api_failure(
                        "Lark",
                        &json!({"error":{"code":99991400,"retry_after_seconds":u64::MAX}}),
                        None,
                    ))
                },
                |_, _| panic!("cooldown does not fit the budget"),
            )
            .unwrap_err();
        assert!(error.is::<refresh::BudgetExceeded>());
    }
}
