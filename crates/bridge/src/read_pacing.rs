//! 仅供任务刷新中的只读请求使用：统一错峰启动、共享冷却，不重试写入。
use super::*;
use std::sync::{Mutex, atomic::Ordering};

pub(super) struct ReadPacer {
    next: Mutex<Instant>,
    #[cfg(test)]
    admissions: Mutex<Vec<Instant>>,
}
impl ReadPacer {
    pub fn new() -> Self {
        Self {
            next: Mutex::new(Instant::now()),
            #[cfg(test)]
            admissions: Mutex::new(Vec::new()),
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
                    // 在同一把锁内记录许可时刻，避免线程或子进程入口的调度抖动。
                    #[cfg(test)]
                    self.admissions.lock().unwrap().push(now);
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
    fn admissions_are_paced_across_concurrent_batches() {
        let pacer = ReadPacer::new();
        let cancelled = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(5);
        // 模拟两页：列表读取和每页四个竞争的详情读取共享同一个限速器。
        for _ in 0..2 {
            pacer
                .read(deadline, &cancelled, || Ok(()), |_, _| panic!("no failure"))
                .unwrap();
            let ready = std::sync::Barrier::new(4);
            std::thread::scope(|scope| {
                for _ in 0..4 {
                    scope.spawn(|| {
                        ready.wait();
                        pacer
                            .read(deadline, &cancelled, || Ok(()), |_, _| panic!("no failure"))
                            .unwrap();
                    });
                }
            });
        }
        let admissions = pacer.admissions.lock().unwrap();
        assert_eq!(admissions.len(), 10);
        // 不给调度误差留宽限：采样是锁内实际许可时间，含两批之间的边界。
        for pair in admissions.windows(2) {
            assert!(
                pair[1].duration_since(pair[0]) >= Duration::from_millis(125),
                "admissions must not burst: {admissions:?}"
            );
        }
    }

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
