//! ProcessLauncher 模块 - ComfyUI 进程启停管理
//!
//! 详见 `PR/03-模块设计/06-ProcessLauncher.md`
//!
//! ## 职责
//! - spawn ComfyUI 子进程（`python main.py <args>`）
//! - 实时捕获 stdout/stderr 并流式推送给前端
//! - 健康检查（HTTP GET `/system_stats`）
//! - 优雅停止（interrupt → SIGTERM → SIGKILL）
//! - 进程状态机管理
//! - 日志环形缓冲（保留最近 N 行供前端查询历史）
//! - 崩溃恢复（PID 文件 + 进程身份校验）
//!
//! ## 设计模式
//! - **State**：ProcessStatus 状态机
//! - **Decorator**：LogPipeline 在 stdout/stderr 流上叠加聚合 / 持久化
//! - **Adapter**：terminate_process 跨平台终止
//! - **Template Method**：start 流程的步骤序列固定
//! - **Singleton**：通过 AppState 全局共享
//! - **Facade**：Tauri commands 层封装服务接口

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use parking_lot::RwLock as PlRwLock;
use tauri::{AppHandle, Emitter};
use tokio::process::Child;
use tokio::sync::Mutex as TokioMutex;
use tokio::task::JoinHandle;

use crate::config::ConfigService;
use crate::error::ProcessError;
use crate::log_store::LogStoreService;
use crate::model_path::ModelPathService;
use crate::python_env::PythonEnvService;

use self::command_builder::build_command;
use self::log_pipeline::{DEFAULT_BUFFER_CAPACITY, LogPipeline};
use self::models::{HealthInfo, LaunchArgs, ProcessStatus, StopReason};
use self::start::{
    check_port_available, spawn_health_check, spawn_process, spawn_stderr_reader,
    spawn_stdout_reader, verify_preconditions, venv_python_path, write_pid_file,
    HealthCheckOutcome, PID_FILE_NAME,
};
use self::state_machine::{
    transition_to_crashed, transition_to_running, transition_to_starting,
    transition_to_stopping,
};
use self::stop::{remove_pid_file, status_from_exit, stop_with_grace, terminate_process};

pub mod command_builder;
pub mod log_pipeline;
pub mod models;
pub mod ring_buffer;
pub mod start;
pub mod state_machine;
pub mod stop;

/// 监控任务轮询间隔（try_wait 检测自然退出）
const MONITOR_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// 共享内部状态（spawn 闭包捕获用 Arc）
struct Inner {
    /// 当前进程状态
    state: PlRwLock<ProcessStatus>,
    /// 当前启动参数（运行中才有，stop 后清空）
    launch_args: PlRwLock<Option<LaunchArgs>>,
    /// 子进程句柄（运行中才有，stop / 退出后清空）
    child: TokioMutex<Option<Child>>,
    /// 当前 PID
    pid: PlRwLock<Option<u32>>,
    /// 日志管道（Arc 共享给 reader task）
    log_pipeline: PlRwLock<Option<Arc<LogPipeline>>>,
    /// 单实例互斥（start / stop 串行化）
    instance_lock: TokioMutex<()>,
    /// 监控 task 句柄（停止时 abort）
    monitor_handle: TokioMutex<Option<JoinHandle<()>>>,
    /// 健康检查 task 句柄
    health_check_handle: TokioMutex<Option<JoinHandle<()>>>,

    // 依赖服务
    python_env: Arc<PythonEnvService>,
    model_path: Arc<ModelPathService>,
    log_store: Arc<LogStoreService>,
    config: Arc<ConfigService>,

    // 路径
    comfyui_root: PathBuf,
    venv_path: PathBuf,
    pid_file_path: PathBuf,
}

/// ProcessLauncher 服务主体
///
/// `Clone` 廉价：内部 `Arc<Inner>`，仅增加引用计数。
/// 适合在 setup 初始化时 clone 给后台 task（如 check_stale_process）。
#[derive(Clone)]
pub struct ProcessLauncherService {
    inner: Arc<Inner>,
}

impl ProcessLauncherService {
    /// 构造
    pub fn new(
        python_env: Arc<PythonEnvService>,
        model_path: Arc<ModelPathService>,
        log_store: Arc<LogStoreService>,
        config: Arc<ConfigService>,
        comfyui_root: PathBuf,
        venv_path: PathBuf,
        data_dir: PathBuf,
    ) -> Self {
        let pid_file_path = data_dir.join(PID_FILE_NAME);
        Self {
            inner: Arc::new(Inner {
                state: PlRwLock::new(ProcessStatus::Stopped),
                launch_args: PlRwLock::new(None),
                child: TokioMutex::new(None),
                pid: PlRwLock::new(None),
                log_pipeline: PlRwLock::new(None),
                instance_lock: TokioMutex::new(()),
                monitor_handle: TokioMutex::new(None),
                health_check_handle: TokioMutex::new(None::<JoinHandle<()>>),
                python_env,
                model_path,
                log_store,
                config,
                comfyui_root,
                venv_path,
                pid_file_path,
            }),
        }
    }

    /// 启动 ComfyUI 进程
    ///
    /// 流程：
    /// 1. 持 instance_lock（防并发启动）
    /// 2. refresh_status（检测自然退出）
    /// 3. 状态校验：仅 Stopped/Crashed 允许启动
    /// 4. transition_to_starting
    /// 5. verify_preconditions（venv + dirty + main.py）
    /// 6. check_port_available
    /// 7. model_path.ensure_yaml_for_launch
    /// 8. build_command + spawn_process
    /// 9. write_pid_file
    /// 10. 创建 LogPipeline + spawn reader tasks
    /// 11. spawn health_check task（成功 → Running）
    /// 12. spawn monitor task（轮询 try_wait 检测自然退出）
    pub async fn start(&self, args: LaunchArgs, app: AppHandle) -> Result<(), ProcessError> {
        let _lock = self.inner.instance_lock.lock().await;

        // 2. refresh status（检测自然退出）
        self.refresh_status_inner().await;

        // 3. 状态校验
        let current = self.inner.state.read().clone();
        if current.is_alive() {
            tracing::warn!(?current, "start rejected, process already alive");
            return match current {
                ProcessStatus::Running { pid, .. } => Err(ProcessError::AlreadyRunning { pid }),
                _ => Err(ProcessError::AlreadyRunning { pid: 0 }),
            };
        }

        // 4. transition_to_starting
        let port = args.listen_port;
        let next = transition_to_starting(&current, port)?;
        *self.inner.state.write() = next.clone();
        emit_status(&app, &next, "process_starting");

        // 5. verify_preconditions
        let _env_info = match verify_preconditions(
            &self.inner.python_env,
            &self.inner.venv_path,
            &self.inner.comfyui_root,
        )
        .await
        {
            Ok(info) => info,
            Err(e) => {
                // 启动失败：回滚状态
                *self.inner.state.write() = transition_to_crashed(
                    &current,
                    None,
                    format!("preconditions failed: {}", e),
                );
                return Err(e);
            }
        };

        // 6. check_port_available
        if let Err(e) = check_port_available(&args.listen_host, port).await {
            *self.inner.state.write() = ProcessStatus::Stopped;
            return Err(e);
        }

        // 7. ensure yaml
        let models_config = self.inner.config.get().models.clone();
        if let Err(e) = self
            .inner
            .model_path
            .ensure_yaml_for_launch(&models_config)
            .await
        {
            *self.inner.state.write() = ProcessStatus::Stopped;
            return Err(ProcessError::Io(format!("ensure yaml failed: {}", e)));
        }

        // 8. build_command + spawn
        let cmd_args = build_command(&args);
        let venv_python = venv_python_path(&self.inner.venv_path);
        tracing::info!(
            ?venv_python,
            ?cmd_args,
            "spawning ComfyUI process"
        );
        let mut child = match spawn_process(&venv_python, &self.inner.comfyui_root, cmd_args) {
            Ok(c) => c,
            Err(e) => {
                *self.inner.state.write() = ProcessStatus::Stopped;
                return Err(e);
            }
        };
        let pid = child
            .id()
            .ok_or_else(|| ProcessError::SpawnFailed("no pid after spawn".into()))?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        // 9. write PID file
        let started_at = Utc::now();
        if let Err(e) = write_pid_file(&self.inner.pid_file_path, pid, started_at, &args).await {
            tracing::warn!(error = %e, "failed to write pid file (continuing)");
        }

        // 10. create LogPipeline + spawn readers
        let pipeline = Arc::new(LogPipeline::new(
            DEFAULT_BUFFER_CAPACITY,
            self.inner.log_store.clone(),
            Some(app.clone()),
        ));
        *self.inner.child.lock().await = Some(child);
        *self.inner.pid.write() = Some(pid);
        *self.inner.launch_args.write() = Some(args.clone());
        *self.inner.log_pipeline.write() = Some(pipeline.clone());

        if let Some(stdout) = stdout {
            spawn_stdout_reader(stdout, pipeline.clone());
        }
        if let Some(stderr) = stderr {
            spawn_stderr_reader(stderr, pipeline);
        }

        // 11. spawn health check task
        let health_handle = spawn_health_check(port, app.clone());
        let inner_for_health = self.inner.clone();
        let app_for_health = app.clone();
        let health_handle = tokio::spawn(async move {
            let outcome = health_handle.await.unwrap_or(HealthCheckOutcome::Timeout);
            match outcome {
                HealthCheckOutcome::Ready => {
                    let _lock = inner_for_health.instance_lock.lock().await;
                    let current = inner_for_health.state.read().clone();
                    match transition_to_running(&current, pid, port) {
                        Ok(next) => {
                            *inner_for_health.state.write() = next.clone();
                            let _ = app_for_health.emit("process_started", &next);
                            tracing::info!(pid, port, "ComfyUI process ready");
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, "transition_to_running failed");
                        }
                    }
                }
                HealthCheckOutcome::Timeout => {
                    tracing::warn!("health check timeout, triggering stop");
                    let _ = stop_impl(&inner_for_health, &app_for_health, StopReason::HealthCheckTimeout).await;
                }
            }
        });
        *self.inner.health_check_handle.lock().await = Some(health_handle);

        // 12. spawn monitor task
        let monitor_handle = spawn_monitor(self.inner.clone(), app.clone());
        *self.inner.monitor_handle.lock().await = Some(monitor_handle);

        tracing::info!(pid, port, "ComfyUI process spawned");
        Ok(())
    }

    /// 停止 ComfyUI 进程
    ///
    /// 幂等：未运行直接返回 Ok
    pub async fn stop(&self, app: AppHandle) -> Result<(), ProcessError> {
        stop_impl(&self.inner, &app, StopReason::UserRequested).await
    }

    /// 查询当前状态
    ///
    /// 内部调用 refresh_status 检测自然退出。
    pub async fn status(&self) -> ProcessStatus {
        self.refresh_status_inner().await;
        self.inner.state.read().clone()
    }

    /// 是否正在运行（健康检查已通过）
    pub async fn is_running(&self) -> bool {
        self.status().await.is_running()
    }

    /// 读取最近 n 条日志
    pub async fn tail_log(&self, n: usize) -> Vec<String> {
        let pipeline = self.inner.log_pipeline.read().clone();
        match pipeline {
            Some(p) => p.tail(n),
            None => Vec::new(),
        }
    }

    /// 列出当前运行的启动参数（供 EnvironmentInspector 使用）
    ///
    /// 返回 `[(mode_str, custom_args)]` 列表，长度 0 或 1。
    pub async fn list_running_args(&self) -> Vec<(String, Option<String>)> {
        let args = self.inner.launch_args.read().clone();
        let status = self.inner.state.read().clone();
        if status.is_alive() {
            if let Some(args) = args {
                let mode = format!("{:?}", args.mode).to_lowercase();
                return vec![(mode, args.custom_args.clone())];
            }
        }
        Vec::new()
    }

    /// 构造启动命令（不实际执行，供调试 / UI 预览）
    pub fn build_command(&self, args: &LaunchArgs) -> Vec<String> {
        build_command(args)
    }

    /// 单次健康检查（同步调用，不轮询）
    pub async fn health_check(&self, port: u16) -> Result<HealthInfo, ProcessError> {
        let url = format!("http://127.0.0.1:{}/system_stats", port);
        let client = reqwest::Client::new();
        let start = std::time::Instant::now();
        match client
            .get(&url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
        {
            Ok(resp) => {
                let status_code = resp.status().as_u16();
                let ready = resp.status().is_success();
                Ok(HealthInfo {
                    ready,
                    status_code: Some(status_code),
                    elapsed_ms: start.elapsed().as_millis() as u64,
                })
            }
            Err(_) => Ok(HealthInfo {
                ready: false,
                status_code: None,
                elapsed_ms: start.elapsed().as_millis() as u64,
            }),
        }
    }

    /// 启动器启动时检查上次未正常退出的 ComfyUI 进程
    ///
    /// 流程：
    /// 1. 读 PID 文件
    /// 2. 校验进程身份（防 PID 复用）
    /// 3. 是 ComfyUI → emit("stale_process_detected")
    /// 4. 否 → 清理 PID 文件
    pub async fn check_stale_process(&self, app: &AppHandle) {
        let pid_path = self.inner.pid_file_path.clone();
        if let Some((pid, started_at, args)) =
            self::start::read_pid_file(&pid_path).await
        {
            if self::start::is_comfyui_process(pid).await {
                tracing::warn!(pid, ?started_at, "stale ComfyUI process detected");
                let _ = app.emit(
                    "stale_process_detected",
                    serde_json::json!({
                        "pid": pid,
                        "started_at": started_at,
                        "args": args,
                    }),
                );
                return;
            }
            // PID 已被其他进程复用或已退出
            tracing::info!(pid, "stale pid is not ComfyUI, cleaning up");
        }
        // 清理 PID 文件
        remove_pid_file(&pid_path).await;
    }

    /// 强制杀死遗留进程（前端用户确认后调用）
    pub async fn kill_stale_process(&self, pid: u32) -> Result<(), ProcessError> {
        tracing::warn!(pid, "killing stale process by user request");
        terminate_process(pid, true).await?;
        remove_pid_file(&self.inner.pid_file_path).await;
        Ok(())
    }

    /// 内部：refresh status（检测自然退出，不持 instance_lock）
    ///
    /// 由 status() 调用（无锁），monitor task 也会调用。
    /// 注意：若 child 已退出，会更新状态并 cleanup。
    async fn refresh_status_inner(&self) {
        let exit_status = {
            let mut guard = self.inner.child.lock().await;
            match guard.as_mut() {
                Some(child) => match child.try_wait() {
                    Ok(Some(status)) => Some(status),
                    _ => None,
                },
                None => None,
            }
        };

        if let Some(status) = exit_status {
            // Child 已退出，take 出来（drop）
            {
                let mut guard = self.inner.child.lock().await;
                *guard = None;
            }
            let current = self.inner.state.read().clone();
            if !current.is_terminal() {
                let next = status_from_exit(status, StopReason::ExternalSignal);
                *self.inner.state.write() = next.clone();
                tracing::info!(?next, "process exited (detected by refresh_status)");
                cleanup_after_exit(&self.inner).await;
            }
        }
    }
}

/// 停止流程实现（自由函数，便于 health check / monitor task 复用）
async fn stop_impl(
    inner: &Arc<Inner>,
    app: &AppHandle,
    reason: StopReason,
) -> Result<(), ProcessError> {
    let _lock = inner.instance_lock.lock().await;

    // refresh（可能与 monitor 同时检测到退出，state 已 terminal 时直接返回）
    {
        let exit_status = {
            let mut guard = inner.child.lock().await;
            match guard.as_mut() {
                Some(child) => match child.try_wait() {
                    Ok(Some(status)) => Some(status),
                    _ => None,
                },
                None => None,
            }
        };
        if let Some(status) = exit_status {
            // Child 已退出，take 出来
            *inner.child.lock().await = None;
            let current = inner.state.read().clone();
            if !current.is_terminal() {
                let next = status_from_exit(status, reason.clone());
                *inner.state.write() = next.clone();
                cleanup_after_exit(inner).await;
            }
            return Ok(());
        }
    }

    // 状态校验
    let current = inner.state.read().clone();
    if current.is_terminal() {
        return Ok(()); // 幂等
    }

    // 提取 pid / port
    let (pid, port) = match &current {
        ProcessStatus::Running { pid, port, .. } => (*pid, *port),
        ProcessStatus::Starting { port, .. } => {
            let pid = inner.pid.read().unwrap_or(0);
            (pid, *port)
        }
        _ => return Ok(()),
    };

    // transition to Stopping
    let next = transition_to_stopping(&current, reason.clone())?;
    *inner.state.write() = next.clone();
    emit_status(app, &next, "process_stopping");

    // Take child out for stop_with_grace
    let child = inner.child.lock().await.take();
    if let Some(child) = child {
        tracing::info!(pid, port, reason = ?reason, "stopping ComfyUI process");
        match stop_with_grace(child, pid, port).await {
            Ok(exit_status) => {
                let next = status_from_exit(exit_status, reason);
                *inner.state.write() = next.clone();
                let _ = app.emit("process_stopped", &next);
                tracing::info!(?next, "process stopped");
            }
            Err(e) => {
                let crashed = ProcessStatus::Crashed {
                    exit_code: None,
                    error: format!("stop failed: {}", e),
                    at: Utc::now(),
                };
                *inner.state.write() = crashed.clone();
                let _ = app.emit("process_stopped", &crashed);
                tracing::error!(error = %e, "stop_with_grace failed");
                cleanup_after_exit(inner).await;
                return Err(e);
            }
        }
    }

    cleanup_after_exit(inner).await;
    Ok(())
}

/// 退出后清理：PID 文件、pid、launch_args、log_pipeline、monitor task
async fn cleanup_after_exit(inner: &Arc<Inner>) {
    // Take child out (drop if Some)
    *inner.child.lock().await = None;

    // Remove PID file
    let pid_path = inner.pid_file_path.clone();
    remove_pid_file(&pid_path).await;

    // Clear state
    *inner.pid.write() = None;
    *inner.launch_args.write() = None;

    // Clear log_pipeline（readers 会因 stdout/stderr EOF 自动退出）
    *inner.log_pipeline.write() = None;

    // Abort monitor task
    if let Some(handle) = inner.monitor_handle.lock().await.take() {
        handle.abort();
    }
    // Abort health check task
    if let Some(handle) = inner.health_check_handle.lock().await.take() {
        handle.abort();
    }

    tracing::debug!("cleanup_after_exit done");
}

/// 启动 monitor task：轮询 try_wait 检测自然退出
fn spawn_monitor(inner: Arc<Inner>, app: AppHandle) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(MONITOR_POLL_INTERVAL).await;

            let exit_status = {
                let mut guard = inner.child.lock().await;
                match guard.as_mut() {
                    Some(child) => match child.try_wait() {
                        Ok(Some(status)) => Some(status),
                        _ => None,
                    },
                    None => {
                        tracing::debug!("monitor: no child, exiting");
                        return;
                    }
                }
            };

            if let Some(status) = exit_status {
                tracing::info!(?status, "monitor detected process exit");
                // 调用 stop_impl 处理退出（已退出则 stop_with_grace 立即返回）
                let _ = stop_impl(&inner, &app, StopReason::ExternalSignal).await;
                return;
            }
        }
    })
}

/// 辅助：emit 状态变更事件
fn emit_status(app: &AppHandle, status: &ProcessStatus, event_name: &str) {
    let _ = app.emit(event_name, status);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_status_default() {
        let status = ProcessStatus::default();
        assert_eq!(status, ProcessStatus::Stopped);
        assert!(status.is_terminal());
    }

    #[test]
    fn test_stop_reason_as_str() {
        assert_eq!(StopReason::UserRequested.as_str(), "user_requested");
    }

    #[test]
    fn test_launch_args_defaults_port() {
        let args = LaunchArgs::defaults();
        assert_eq!(args.listen_port, 8188);
    }
}
