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
//! - **F24 退出流程**（ShutdownCoordinator，详见 `shutdown.rs`）
//! - 进程组隔离（spawn 时 setsid / CREATE_NEW_PROCESS_GROUP；终止时用进程组）
//!
//! ## 设计模式
//! - **State**：ProcessStatus 状态机
//! - **Decorator**：LogPipeline 在 stdout/stderr 流上叠加聚合 / 持久化
//! - **Adapter**：terminate_process + terminate_process_group 跨平台终止
//! - **Template Method**：start / shutdown 5 步事务的步骤序列固定
//! - **Singleton**：通过 AppState 全局共享
//! - **Facade**：Tauri commands 层封装服务接口
//! - **Reentrant Guard**：ShutdownCoordinator AtomicBool 防重入

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use parking_lot::RwLock as PlRwLock;
use tauri::{AppHandle, Emitter};
use tokio::process::Child;
use tokio::sync::Mutex as TokioMutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::config::ConfigService;
use crate::error::ProcessError;
use crate::log_store::LogStoreService;
use crate::model_path::ModelPathService;
use crate::python_env::PythonEnvService;
use crate::task_scheduler::progress::ProgressSender;

use self::command_builder::build_command;
use self::log_pipeline::{DEFAULT_BUFFER_CAPACITY, LogPipeline};
use self::models::StopReason;
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

// 重新导出数据模型供外部模块使用（v3.4：task_scheduler factory 需要 LaunchArgs）
pub use self::models::{HealthInfo, LaunchArgs, ProcessStatus, ShutdownReport};

pub mod command_builder;
pub mod log_pipeline;
pub mod models;
pub mod ring_buffer;
pub mod shutdown;
pub mod start;
pub mod state_machine;
pub mod stop;

// F24 公开 re-export：让 AppState / commands 等可以引用 ShutdownCoordinator
pub use self::shutdown::ShutdownCoordinator;

/// 监控任务轮询间隔（try_wait 检测自然退出）
const MONITOR_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// 共享内部状态（spawn 闭包捕获用 Arc）
///
/// v3.4：改为 `pub` 以便 `start.rs::spawn_health_check` 接收 `Arc<Inner>` 后能 try_wait 共享 child
/// （修复"child 0.5s 死，health_check 傻等 60s"的关键）
pub struct Inner {
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
    /// v3.4.2：取消令牌（用于终止 health_check / monitor / early_detect 三个 detached task）
    ///
    /// 之前用 `JoinHandle::abort()` 强杀，但 abort 不优雅（无法区分"自然完成"和"被取消"）。
    /// 改用 `CancellationToken`：
    /// - start() 启动时新建一个 token，存到这里
    /// - 把 clone 分发给 health_check / monitor / early_detect 三个 task
    /// - stop_impl() 时调 `token.cancel()` → 三个 task 在合适位置 `token.is_cancelled()` 检查后退出
    /// - start() 完成（spawn 成功）后保留 token（直到 stop 时 cancel）
    cancel_token: PlRwLock<Option<CancellationToken>>,

    // 依赖服务
    python_env: Arc<PythonEnvService>,
    model_path: Arc<ModelPathService>,
    log_store: Arc<LogStoreService>,
    config: Arc<ConfigService>,

    // 路径（仅 pid_file_path 保留固定路径；comfyui_root/venv_path 热加载）
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
    ///
    /// **路径热加载**：`comfyui_root` / `venv_path` 每次需要时从 ConfigService
    /// 读取最新的 `paths.comfyui_root` / `paths.venv_path`，实现"修改 config 后无需重启立即生效"。
    /// `pid_file_path` 属于 app data 目录状态，构造时固定一次。
    pub fn new(
        python_env: Arc<PythonEnvService>,
        model_path: Arc<ModelPathService>,
        log_store: Arc<LogStoreService>,
        config: Arc<ConfigService>,
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
                cancel_token: PlRwLock::new(None),
                python_env,
                model_path,
                log_store,
                config,
                pid_file_path,
            }),
        }
    }

    /// 读取当前 comfyui_root（每次调用读最新 config）
    fn current_comfyui_root(&self) -> PathBuf {
        self.inner.config.get().paths.comfyui_root.clone()
    }

    /// 读取当前 venv_path（每次调用读最新 config）
    fn current_venv_path(&self) -> PathBuf {
        self.inner.config.get().paths.venv_path.clone()
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
    ///
    /// **v3.4 增强**：新增 `progress: Option<&ProgressSender>` 参数，
    /// 拆分 5 阶段上报进度（10% 校验 / 20% 端口 / 30% yaml / 50% spawn / 60% 健康检查）。
    /// - `None` 时走原行为（向后兼容 `kill_stale_process` 等内部调用）
    /// - `Some(p)` 时每个关键点调 `p.send_percent(percent)` + `p.send_message(msg)`
    /// - 注意：start() 本身不等 health_check（v3.2.2 修复），60% 阶段由 health_check 后续 task 推进
    pub async fn start(
        &self,
        args: LaunchArgs,
        app: AppHandle,
        progress: Option<&ProgressSender>,
    ) -> Result<(), ProcessError> {
        // 进度汇报辅助闭包（None 时静默 no-op）
        let report = |percent: u8, msg: &str| {
            if let Some(p) = progress {
                p.send_percent(percent);
                p.send_message(msg);
            }
        };
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

        // 5. verify_preconditions（路径热加载：读最新 venv + comfyui_root）
        //
        // v3.4 增强：阶段 1 - 校验环境（10%）
        report(10, "校验环境...");
        let _env_info = match verify_preconditions(
            &self.inner.python_env,
            &self.current_venv_path(),
            &self.current_comfyui_root(),
            args.mode,  // v3.10 新增：检查 cuda_available 与 launch_mode 匹配
        )
        .await
        {
            Ok(info) => {
                report(15, &format!("环境校验通过（python={:?}）", info.python_path));
                info
            }
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
        //
        // v3.4 增强：阶段 2 - 检查端口（20%）
        report(20, &format!("检查端口 {} 是否空闲...", port));
        if let Err(e) = check_port_available(&args.listen_host, port).await {
            *self.inner.state.write() = ProcessStatus::Stopped;
            return Err(e);
        }
        report(25, &format!("端口 {} 空闲", port));

        // 7. ensure yaml
        //
        // v3.4 增强：阶段 3 - 生成 extra_model_paths.yaml（30%）
        report(30, "生成 extra_model_paths.yaml...");
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
        report(40, "extra_model_paths.yaml 已生成");

        // 8. build_command + spawn
        //
        // v3.4 增强：阶段 4 - spawn ComfyUI 进程（50%）
        report(50, "启动 ComfyUI 进程...");
        let cmd_args = build_command(&args);
        let venv_python = venv_python_path(&self.current_venv_path());
        tracing::info!(
            ?venv_python,
            ?cmd_args,
            "spawning ComfyUI process"
        );
        let mut child = match spawn_process(&venv_python, &self.current_comfyui_root(), cmd_args) {
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
        report(55, &format!("ComfyUI 进程已 spawn（pid={}, port={}）", pid, port));

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

        // 10.5 v3.4.2：创建取消令牌（控制 health_check / monitor / early_detect）
        //
        // 完全异步架构改造：
        // - 之前 start() 内部有 5s 早期死亡检测的 `tokio::time::sleep(5s)`，
        //   阻塞 start() 流程最多 5s + await time。
        //   但 task_factory 已经把 start 包装成 detached task，sleep 不会阻塞前端 invoke，
        //   但是 sleep 期间 start() 内部逻辑（包括 progress 推送、process_starting 事件）也无法被外部观察。
        // - 现在彻底移除 5s 阻塞 sleep，改用 monitor 任务以 500ms 间隔轮询 try_wait，
        //   0.5s 内 child 死了 monitor 立即检测到 → emit process_crashed + stop_impl。
        // - health_check 改用 cancel_token 控制（移除 60s 超时限制，无限轮询直到 ready/cancelled/crash）。
        // - start() 立即返回 Ok(())，task 完成；前端通过 process_started / process_crashed 事件跟踪。
        let cancel_token = CancellationToken::new();
        *self.inner.cancel_token.write() = Some(cancel_token.clone());

        // 11. spawn health check task（detached + cancel_token 控制）
        //
        // v3.2.2 关键修复：start() 立即返回，不等 health_check 完成
        // v3.4.2 增强：
        // - health_check 接收 cancel_token，取消时立即退出（无需等待 join handle abort）
        // - 移除 60s 超时限制，无限轮询直到：
        //   a) child 死了 → emit process_crashed
        //   b) ComfyUI 就绪 → emit process_started
        //   c) stop() 调用 → cancel_token.cancel() 触发 health_check 退出
        //   d) 用户应用关闭 → cancel_token.cancel() 兜底
        // - 每 30s 推一次 process_health_warning 事件（前端可显示"启动较慢"提示）
        //
        // v3.4 增强：spawn_health_check 额外接收 `Arc<Inner>`，每次轮询前 try_wait 共享 child
        // 修复"child 启动 5s~60s 期间死亡，health_check 傻等 60s 才超时"的问题
        report(60, &format!("等待 ComfyUI 在 {}:{} 就绪...", args.listen_host, port));
        let inner_for_health = self.inner.clone();
        let app_for_health = app.clone();
        let progress_for_health = progress.cloned();
        let cancel_for_health = cancel_token.clone();
        let health_handle = tokio::spawn(async move {
            // spawn_health_check 返回 JoinHandle<HealthCheckOutcome>，需要 await
            // 但 await 在独立 task 内，不阻塞 start() 主流程
            let outcome = spawn_health_check(
                port,
                app_for_health.clone(),
                inner_for_health.clone(),
                progress_for_health,
                cancel_for_health,
            )
            .await
            .unwrap_or(HealthCheckOutcome::Cancelled);
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
                    // v3.4.2：移除 60s 超时后，这个分支基本不会触发了（health_check 现在无限轮询）
                    // 保留兜底：万一未来又加了超时
                    tracing::warn!("health check timeout (should not happen with v3.4.2), triggering stop");
                    let _ = stop_impl(&inner_for_health, &app_for_health, StopReason::HealthCheckTimeout).await;
                }
                HealthCheckOutcome::Crashed(exit_code) => {
                    // v3.4.2：health_check 检测到 child 死亡（任何时间点）
                    // 走标准停止流程，emit process_stopped 让前端状态机更新
                    tracing::warn!(?exit_code, "health check detected child exit, triggering stop");
                    let _ = stop_impl(&inner_for_health, &app_for_health, StopReason::ExternalSignal).await;
                }
                HealthCheckOutcome::Cancelled => {
                    // v3.4.2：cancel_token 被 cancel（stop_impl 调用）→ 直接退出，不动状态
                    // stop_impl 会负责把状态推进到 Stopped/Crashed
                    tracing::info!("health check cancelled (likely stop requested)");
                }
            }
        });
        *self.inner.health_check_handle.lock().await = Some(health_handle);

        // 12. spawn monitor task（detached + cancel_token 控制）
        //
        // v3.4.2 改造：
        // - monitor 接收 cancel_token，cancel 时立即退出（无需 abort）
        // - monitor 0.5s 轮询，**承担"早期死亡检测"职责**（替代之前的 5s sleep）
        //   → 0.5s 内 child 死了 monitor 立即检测到 → emit process_crashed
        //   → 之前要 5s sleep 才能检测到，现在 0.5s 即可
        // - 移除 5s 早期死亡检测的 sleep（这是阻塞！），start() 立即返回
        let monitor_handle = spawn_monitor(self.inner.clone(), app.clone(), cancel_token);
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

    /// 停止 ComfyUI 进程（带 reason，泛型 Runtime 版本）
    ///
    /// F24 退出流程专用：`ShutdownCoordinator` 调此方法传 `StopReason::Shutdown`，
    /// 与 `stop()` 行为一致（流程相同），但保留 reason 用于日志/审计。
    /// 使用泛型 R 兼容 ShutdownCoordinator 的 R: Runtime 调用。
    ///
    /// 幂等：未运行直接返回 Ok
    pub async fn stop_with_reason<R: tauri::Runtime>(
        &self,
        reason: StopReason,
        app: tauri::AppHandle<R>,
    ) -> Result<(), ProcessError> {
        stop_impl_generic(&self.inner, &app, reason).await
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
    stop_impl_generic(inner, app, reason).await
}

/// 停止流程实现（泛型 Runtime 版本，F24 ShutdownCoordinator 调用）
async fn stop_impl_generic<R: tauri::Runtime>(
    inner: &Arc<Inner>,
    app: &tauri::AppHandle<R>,
    reason: StopReason,
) -> Result<(), ProcessError> {
    let _lock = inner.instance_lock.lock().await;

    // v3.4.2：先触发取消令牌（health_check / monitor 立即退出）
    // - 必须在 stop_with_grace 之前 cancel，让 health_check 不再继续发 HTTP 请求
    // - clone 出 token 立即调 cancel()，再 take 出 inner.cancel_token 字段
    // - 用 swap_remove 模式：take 出来 + cancel
    {
        if let Some(token) = inner.cancel_token.read().clone() {
            token.cancel();
        }
    }

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
    emit_status_generic(app, &next, "process_stopping");

    // Take child out for stop_with_grace
    let child = inner.child.lock().await.take();
    if let Some(child) = child {
        tracing::info!(pid, port, reason = ?reason, "stopping ComfyUI process");
        // v3.6：stop_with_grace 接受 CancellationToken 用于「Force Stop」场景。
        // 此处创建本地非可取消 token：保持原 grace period 语义（5s SIGTERM + 2s SIGKILL）。
        // 注：inner.cancel_token 已在上方 cancel()（用于停 health_check/monitor），不能复用。
        // 未来如需「Force Stop」按钮，可在此注入可取消 token。
        let stop_cancel = CancellationToken::new();
        match stop_with_grace(child, pid, port, &stop_cancel).await {
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

/// 退出后清理：PID 文件、pid、launch_args、cancel_token、monitor/health task
///
/// v3.4.2 修复：**不再清空 log_pipeline**
/// - 之前 `*inner.log_pipeline.write() = None;` 会让 LogPipeline drop，里面的 stdout/stderr reader 收到 EOF 退出
/// - 后果：LogsPage 切换页面后再切回来，加载历史日志用的是 SQL（持久化的），内存里的就丢了
/// - 修复：保留 log_pipeline 实例（reader 因 stdout/stderr EOF 已经自然退出，Arc 引用计数 0 后才 drop）
/// - 用户体验：切换页面再回来，能看到完整的最近 5000 行日志
async fn cleanup_after_exit(inner: &Arc<Inner>) {
    // Take child out (drop if Some)
    *inner.child.lock().await = None;

    // Remove PID file
    let pid_path = inner.pid_file_path.clone();
    remove_pid_file(&pid_path).await;

    // Clear state
    *inner.pid.write() = None;
    *inner.launch_args.write() = None;

    // v3.4.2：清 cancel_token（释放 Arc 引用计数）
    *inner.cancel_token.write() = None;

    // v3.4.2：不再清 log_pipeline
    // - 保留 pipeline 实例，LogsPage 切换页面后回来还能看到完整最近日志
    // - reader task 已在 stdout/stderr EOF 时自然退出（Arc 引用计数降为 0 时 drop）
    // *inner.log_pipeline.write() = None; // ← 注释掉

    // Abort monitor task（cancel_token 兜底，理论上已 cancel 退出）
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
///
/// v3.4 增强：检测到 child exit 时，emit `process_crashed` 事件 + 附 stderr tail（最近 50 行）。
/// 修复了之前"monitor 检测到退出只 log 一行，前端收不到任何崩溃通知"的问题。
///
/// v3.4.2 改造：
/// - 接收 `cancel_token: CancellationToken`：
///   - stop_impl 调用时调 `cancel_token.cancel()` → monitor 立即退出（无需 abort）
///   - cancel_token 兜底（用户应用关闭）
/// - 移除 5s 早期死亡检测的 sleep：
///   - 之前 start() 内部有 5s 阻塞 sleep，作用是 spawn 后 5s 内检测 child 死亡
///   - 现在 monitor 0.5s 间隔轮询，**0.5s 内即可检测**（甚至更早，因为 sleep 之前的部分逻辑也要耗时）
///   - 0.5s vs 5s：提升响应速度 10 倍
/// - tokio::select! 让 sleep 与 cancel 竞争，cancel 立即生效
/// - 接收 monitor_handle 由 `monitor_handle: JoinHandle<()>` 注册到 inner，
///   停止时 abort 兜底（cancel_token 失效时）
fn spawn_monitor(
    inner: Arc<Inner>,
    app: AppHandle,
    cancel_token: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            // v3.4.2：用 tokio::select! 让 sleep 与 cancel 竞争
            tokio::select! {
                _ = tokio::time::sleep(MONITOR_POLL_INTERVAL) => {}
                _ = cancel_token.cancelled() => {
                    tracing::info!("monitor cancelled (likely stop requested)");
                    return;
                }
            }

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

                // v3.4 增强：emit process_crashed 事件（前端跳转 + 弹窗用）
                // 载荷：exit_code + stderr_tail（最近 50 行来自 LogPipeline）
                let exit_code = status.code();
                let stderr_tail = inner
                    .log_pipeline
                    .read()
                    .as_ref()
                    .map(|p| p.tail(50))
                    .unwrap_or_default();

                // v3.4.2：根据 elapsed 决定 reason 标签
                // - 0~5s 内 child 死 → reason = "early_exit"（前端显示"早期退出"）
                // - 5s 后 child 死 → reason = "monitor_detected"（前端显示"运行中崩溃"）
                // 用 start_time 跟踪：monitor 自身在 spawn 后立即运行，state 里的 started_at 来自 Starting
                let reason = {
                    let current = inner.state.read();
                    match &*current {
                        ProcessStatus::Starting { started_at, .. }
                        | ProcessStatus::Running { started_at, .. } => {
                            let since_start = Utc::now()
                                .signed_duration_since(*started_at)
                                .num_seconds();
                            if since_start <= 5 {
                                "early_exit"
                            } else {
                                "monitor_detected"
                            }
                        }
                        _ => "monitor_detected",
                    }
                };

                use tauri::Emitter;
                let _ = app.emit(
                    "process_crashed",
                    serde_json::json!({
                        "exit_code": exit_code,
                        "stderr_tail": stderr_tail,
                        "reason": reason,
                    }),
                );

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

/// 泛型 Runtime 版本（F24 ShutdownCoordinator / stop_with_reason 走此路径）
fn emit_status_generic<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    status: &ProcessStatus,
    event_name: &str,
) {
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
