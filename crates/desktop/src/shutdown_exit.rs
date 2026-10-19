//! Cocoa 正常退出不保证 Application::run 返回；进程退出屏障作为 quit hook 的兜底。
use crate::worker::WorkerSender;
use std::sync::OnceLock;

static WORKER: OnceLock<WorkerSender> = OnceLock::new();

unsafe extern "C" {
    fn atexit(callback: extern "C" fn()) -> std::ffi::c_int;
}

// 只在 main 初始化时注册一次。不能靠静态对象 Drop：正常 exit 不会析构它们。
pub fn install(worker: WorkerSender) -> anyhow::Result<()> {
    WORKER
        .set(worker)
        .map_err(|_| anyhow::anyhow!("Worker exit barrier already installed"))?;
    // SAFETY: 回调为进程期有效的 C ABI 函数；内部不允许 panic 跨越 FFI 边界。
    anyhow::ensure!(
        unsafe { atexit(wait_for_worker) } == 0,
        "Cannot register worker exit barrier"
    );
    Ok(())
}

extern "C" fn wait_for_worker() {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if let Some(worker) = WORKER.get() {
            crate::console::event("app.lifecycle", "exit_waiting_for_worker");
            let _ = worker.shutdown().recv_blocking();
            crate::console::event("app.lifecycle", "exit_worker_reaped");
        }
    }));
    if result.is_err() {
        // 不将 panic 传播给 Cocoa / C runtime；消息固定，不输出潜在凭据。
        let _ = std::panic::catch_unwind(|| {
            crate::console::event("app.lifecycle", "exit_barrier_panicked");
        });
    }
}
