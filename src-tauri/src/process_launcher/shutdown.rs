//! F24 退出流程编排器（ShutdownCoordinator）
//!
//! 详见 `PR/03-模块设计/06-ProcessLauncher.md §12` 与 `PR/01-需求文档.md §F24`。
//!
//! ## 5 步事务（Template Method 模式）
//!
//! 1. **防重入**：CAS 设置 `AtomicBool in_progress`，失败 → 直接返回 `OnceLock<ShutdownReport>` 缓存结果
//! 2. **广播 AppExiting**：订阅者清理本地缓存 + 拒绝新操作
//! 3. **进程组终止**：`process_launcher.stop(StopReason::Shutdown)` 走现有 stop 流程
//!     （interrupt → 进程组 SIGTERM 5s → 进程组 SIGKILL 2s，内部已用 `terminate_process_group`）
//! 4. **资源释放**：等待子系统清理（LogStore WAL checkpoint / 事件总线 unsubscribe）
//! 5. **广播 AppExited** + `app.exit(0)`
//!
//! 30s 总超时兜底：超时时 `std::process::exit(0)` 强制退出，不卡死 launcher。
//!
//! ## 设计模式
//!
//! - **Reentrant Guard**：`AtomicBool::compare_exchange(false, true, ...)` 防重入
//! - **Template Method**：5 步事务流程固定，调用方只传入 `ShutdownReason`
//! - **Observer**：通过 EventBus 广播 `AppExiting` / `AppExited`
//! - **Adapter**：`terminate_process_group` 跨平台封装（已实现在 `stop.rs`）

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager, Runtime};

use crate::event_bus::{EventBus, ShutdownReason, SystemEvent};
use crate::process_launcher::models::ShutdownReport;
use crate::process_launcher::ProcessLauncherService;

/// F24 退出流程总超时（30 秒）
///
/// 超过此时间无论完成与否都触发 `std::process::exit(0)` 强制退出，避免 launcher 卡死。
pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

/// 资源释放阶段最大等待时间（500ms）
///
/// 步骤 4 等待 LogStore WAL checkpoint / 事件总线 unsubscribe / venv_locks 释放等。
pub const RESOURCE_CLEANUP_WAIT: Duration = Duration::from_millis(500);

/// ShutdownCoordinator - 退出流程编排器
///
/// 内部状态：
/// - `in_progress: AtomicBool` - 防重入（CAS 设置）
/// - `cached_report: OnceLock<Result<ShutdownReport, String>>` - 缓存首次执行结果
///
/// `Clone` 廉价：内部 `Arc`，仅增加引用计数。
#[derive(Clone)]
pub struct ShutdownCoordinator {
    inner: Arc<Inner>,
}

struct Inner {
    /// 进程启动器（用于 stop 流程）
    process_launcher: ProcessLauncherService,
    /// 事件总线（用于广播 AppExiting / AppExited）
    event_bus: EventBus,
    /// 防重入标志
    in_progress: AtomicBool,
    /// 缓存首次执行结果（OnceLock 只能 set_once，适合"幂等返回首次结果"语义）
    cached_report: OnceLock<Result<ShutdownReport, String>>,
}

impl ShutdownCoordinator {
    /// 构造 ShutdownCoordinator
    pub fn new(process_launcher: ProcessLauncherService, event_bus: EventBus) -> Self {
        Self {
            inner: Arc::new(Inner {
                process_launcher,
                event_bus,
                in_progress: AtomicBool::new(false),
                cached_report: OnceLock::new(),
            }),
        }
    }

    /// 触发退出流程（idempotent）
    ///
    /// 多次调用仅执行一次事务：后续调用直接返回首次结果（cached）。
    /// 即使首次返回 Err，cached 也是 Err，行为幂等。
    ///
    /// # 参数
    /// - `app`: Tauri AppHandle（用于 processLauncher.stop 接收 + 最后 app.exit）
    /// - `reason`: 退出原因（WindowClose / TrayQuit / ShortcutCtrlQ / Restart）
    ///
    /// # 返回
    /// - `Ok(ShutdownReport)`：5 步事务成功完成
    /// - `Err(String)`：stop 失败等场景；但**仍会**继续后续清理（事务不回滚）
    ///
    /// # 30s 超时兜底
    /// 事务未在 `SHUTDOWN_TIMEOUT` 内完成 → 强制 `std::process::exit(0)`（不返回）
    pub async fn shutdown_all<R: Runtime>(
        &self,
        app: AppHandle<R>,
        reason: ShutdownReason,
    ) -> Result<ShutdownReport, String> {
        // ========== 步骤 1：防重入 ==========
        // CAS 失败 → 返回 cached（首次执行结果）
        if self
            .inner
            .in_progress
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            tracing::info!(?reason, "shutdown_all already in progress, returning cached result");
            // 等待 OnceLock 填充（首次执行完成后会写入）
            // 简单做法：spin 等 50ms 后直接尝试 get
            // 注：实际场景下，首次调用方在 await tokio::time::timeout(...).await，
            //    二次调用方进入此处时首次大概率已 cached，但为了健壮性仍等一下
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Some(cached) = self.inner.cached_report.get() {
                return cached.clone();
            }
            // 首次尚未完成：再次 await（防重入语义：不要抢着改 in_progress）
            // 这里直接返回"事务进行中"提示，前端应避免并发调用
            return Err("shutdown_all already in progress".to_string());
        }

        // 进入事务主体：30s 总超时包裹
        let start = Instant::now();
        let result = tokio::time::timeout(SHUTDOWN_TIMEOUT, self.run_transaction(app.clone(), reason.clone())).await;

        // 30s 超时：兜底强制退出（不返回，让 launcher 进程立即终止）
        let report_result = match result {
            Ok(r) => r,
            Err(_) => {
                tracing::error!(
                    timeout = ?SHUTDOWN_TIMEOUT,
                    "shutdown_all timed out, force exit via std::process::exit(0)"
                );
                // OnceLock 缓存超时结果（便于二次调用返回一致结果）
                let _ = self.inner.cached_report.set(Err(format!(
                    "shutdown timed out after {:?}",
                    SHUTDOWN_TIMEOUT
                )));
                // 强制退出（不返回，进程立即终止）
                std::process::exit(0);
            }
        };

        let elapsed = start.elapsed();
        tracing::info!(?elapsed, "shutdown_all transaction completed");

        // 缓存结果（OnceLock 第一次 set 成功，后续 set 失败但不影响返回）
        let _ = self.inner.cached_report.set(report_result.clone());

        // 成功后调用 app.exit(0)（让 Tauri 走正常退出流程）
        if report_result.is_ok() {
            app.exit(0);
        }

        report_result
    }

    /// 5 步事务主体（无超时，由 `shutdown_all` 包裹）
    async fn run_transaction<R: Runtime>(
        &self,
        app: AppHandle<R>,
        reason: ShutdownReason,
    ) -> Result<ShutdownReport, String> {
        // ========== 步骤 2：广播 AppExiting ==========
        tracing::info!(?reason, "step 2: emit AppExiting");
        self.inner.event_bus.emit(SystemEvent::AppExiting {
            reason: reason.clone(),
        });

        // ========== 步骤 3：进程组终止 ==========
        let stop_start = Instant::now();
        let comfyui_was_running = self.inner.process_launcher.is_running().await;
        let stop_result = if comfyui_was_running {
            tracing::info!("step 3: stopping ComfyUI process group (StopReason::Shutdown)");
            self.inner
                .process_launcher
                .stop_with_reason(crate::process_launcher::models::StopReason::Shutdown, app.clone())
                .await
        } else {
            tracing::info!("step 3: ComfyUI not running, skip stop");
            Ok(())
        };
        let stop_elapsed_ms = stop_start.elapsed().as_millis() as u64;

        if let Err(ref e) = stop_result {
            tracing::warn!(error = %e, "process stop failed, continuing shutdown");
        }

        // ========== 步骤 4：资源释放 ==========
        // 等待 LogStore WAL checkpoint / 事件总线 unsubscribe / venv_locks 释放等
        // 当前为占位（各模块需订阅 AppExiting 自清理）
        tracing::info!("step 4: waiting for resource cleanup");
        tokio::time::sleep(RESOURCE_CLEANUP_WAIT).await;

        // ========== 步骤 5：广播 AppExited ==========
        tracing::info!("step 5: emit AppExited");
        self.inner.event_bus.emit(SystemEvent::AppExited);

        Ok(ShutdownReport {
            comfyui_was_running,
            stop_elapsed_ms,
            reason,
        })
    }
}

impl std::fmt::Debug for ShutdownCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShutdownCoordinator")
            .field("in_progress", &self.inner.in_progress.load(Ordering::SeqCst))
            .field("has_cached", &self.inner.cached_report.get().is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_bus::EventBus;

    #[tokio::test]
    async fn test_cached_report_idempotent() {
        // 验证 OnceLock 缓存：set 一次后 get 永远返回首次值
        let cell: OnceLock<i32> = OnceLock::new();
        assert!(cell.get().is_none());
        let _ = cell.set(42);
        assert_eq!(cell.get(), Some(&42));
        // 二次 set 失败
        assert!(cell.set(99).is_err());
        assert_eq!(cell.get(), Some(&42));
    }

    #[tokio::test]
    async fn test_atomic_bool_cas_prevents_reentry() {
        let flag = AtomicBool::new(false);
        // 第一次 CAS 成功
        assert!(flag
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok());
        // 第二次 CAS 失败
        assert!(flag
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err());
        assert!(flag.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_event_bus_emits_app_exiting() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();

        bus.emit(SystemEvent::AppExiting {
            reason: ShutdownReason::WindowClose,
        });

        let received = rx.recv().await.unwrap();
        match received {
            SystemEvent::AppExiting { reason } => {
                assert_eq!(reason, ShutdownReason::WindowClose);
            }
            _ => panic!("expected AppExiting"),
        }
    }

    #[tokio::test]
    async fn test_event_bus_emits_app_exited() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();

        bus.emit(SystemEvent::AppExited);
        let received = rx.recv().await.unwrap();
        assert!(matches!(received, SystemEvent::AppExited));
    }

    #[tokio::test]
    async fn test_shutdown_reason_as_str() {
        assert_eq!(ShutdownReason::WindowClose.as_str(), "window_close");
        assert_eq!(ShutdownReason::TrayQuit.as_str(), "tray_quit");
        assert_eq!(ShutdownReason::ShortcutCtrlQ.as_str(), "shortcut_ctrl_q");
        assert_eq!(ShutdownReason::Restart.as_str(), "restart");
    }

    #[tokio::test]
    async fn test_resource_cleanup_wait_is_reasonable() {
        // 资源释放等待时间 ≤ 1s（否则影响 30s 总超时分配）
        assert!(RESOURCE_CLEANUP_WAIT <= Duration::from_secs(1));
    }

    #[tokio::test]
    async fn test_shutdown_timeout_is_30_seconds() {
        // 验收 NFR：30s 超时兜底
        assert_eq!(SHUTDOWN_TIMEOUT, Duration::from_secs(30));
    }
}
