//! ProcessLauncher 的 Tauri commands
//!
//! 设计模式：门面（Facade）- 前端仅与本层交互，不直接访问 Service
//!
//! 详见 `PR/03-模块设计/06-ProcessLauncher.md §3 接口签名`
//!
//! ## 命令清单
//! | 命令 | 说明 | 事件 |
//! |---|---|---|
//! | `process_start` | 启动 ComfyUI（从 Config 读取参数） | `process_starting` / `process_started` |
//! | `process_stop` | 停止 ComfyUI（幂等） | `process_stopping` / `process_stopped` |
//! | `process_status` | 查询当前状态 | - |
//! | `process_tail_log` | 读取最近 N 行日志 | - |
//! | `process_kill_stale` | 强制杀死遗留进程 | - |
//! | `shutdown_all` | F24 退出流程（弹确认 + 联动关闭 ComfyUI + 30s 超时兜底） | `app_exiting` / `app_exited` |
//!
//! ## 设计要点
//! - `process_start` 不接收 LaunchArgs 参数：从 `ConfigService.get().launch` 构造
//!   这样前端只需调用 `invoke('process_start')`，参数来自用户在设置页保存的配置
//! - 错误统一序列化为字符串（前端通过 `Err(String)` 接收）
//! - 状态变更通过事件推送，前端 listen 即可
//! - `shutdown_all` 触发 F24 5 步事务：防重入 → 广播 AppExiting → 进程组终止 → 资源释放 → app.exit

use tauri::{AppHandle, State};

use crate::app_state::AppState;
use crate::event_bus::ShutdownReason;
use crate::process_launcher::models::{LaunchArgs, ProcessStatus, ShutdownReport};

/// 从 Config 构造 LaunchArgs（运行时快照）
///
/// 设计意图：解耦 Config 与运行时参数。
/// Config 变更（如用户在设置页修改端口）不影响已启动的进程；
/// 只有下次 `process_start` 调用时才读取最新 Config。
fn build_launch_args_from_config(cfg: &crate::config::Config) -> LaunchArgs {
    let launch = &cfg.launch;
    // 空字符串视为 None（避免 ComfyUI 收到空 custom_args 参数）
    let custom_args = if launch.custom_args.trim().is_empty() {
        None
    } else {
        Some(launch.custom_args.clone())
    };

    LaunchArgs {
        mode: launch.mode,
        listen_host: launch.listen_host.clone(),
        listen_port: launch.listen_port,
        preview_method: launch.preview_method,
        auto_launch: launch.auto_open_browser,
        advanced: launch.advanced.clone(),
        custom_args,
    }
}

/// 启动 ComfyUI 进程
///
/// 从 ConfigService 读取最新配置，构造 LaunchArgs 后调用 ProcessLauncherService::start。
///
/// # 事件
/// - `process_starting`：状态置为 Starting 时立即 emit
/// - `process_started`：健康检查通过后 emit
/// - `process_stopped`：启动失败 / 健康检查超时后 emit
///
/// # 错误
/// - `AlreadyRunning`：已有进程在运行
/// - `PortInUse`：端口被占用
/// - `EnvironmentNotReady`：venv 未就绪 / dirty 标记存在
/// - `PythonNotFound` / `MainNotFound` / `SpawnFailed`
#[tauri::command]
pub async fn process_start(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let args = {
        let cfg = state.config.get();
        build_launch_args_from_config(&cfg)
    };

    tracing::info!(
        mode = ?args.mode,
        host = %args.listen_host,
        port = args.listen_port,
        "process_start invoked"
    );

    state
        .process_launcher
        .start(args, app)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "process_start failed");
            e.to_string()
        })
}

/// 停止 ComfyUI 进程（幂等）
///
/// 未运行时直接返回 Ok(())。
///
/// # 事件
/// - `process_stopping`：状态置为 Stopping 时 emit
/// - `process_stopped`：进程退出后 emit
#[tauri::command]
pub async fn process_stop(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    tracing::info!("process_stop invoked");
    state
        .process_launcher
        .stop(app)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "process_stop failed");
            e.to_string()
        })
}

/// 查询当前进程状态
///
/// 内部会调用 `refresh_status_inner()` 检测自然退出（非阻塞）。
#[tauri::command]
pub async fn process_status(
    state: State<'_, AppState>,
) -> Result<ProcessStatus, String> {
    Ok(state.process_launcher.status().await)
}

/// 读取最近 N 行日志
///
/// 从环形缓冲读取（默认容量 5000 行）。
/// 若进程未启动或缓冲为空，返回空 Vec。
///
/// # 参数
/// - `lines`：读取行数（建议 100-500，过大无意义且增加序列化开销）
#[tauri::command]
pub async fn process_tail_log(
    lines: usize,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    Ok(state.process_launcher.tail_log(lines).await)
}

/// 强制杀死遗留的 ComfyUI 进程
///
/// 用户在前端确认「检测到遗留进程」提示后调用。
/// 流程：terminate_process(force=true) → 清理 PID 文件
///
/// # 参数
/// - `pid`：遗留进程 PID（来自 `stale_process_detected` 事件载荷）
#[tauri::command]
pub async fn process_kill_stale(
    pid: u32,
    state: State<'_, AppState>,
) -> Result<(), String> {
    tracing::info!(pid, "process_kill_stale invoked");
    state
        .process_launcher
        .kill_stale_process(pid)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, pid, "process_kill_stale failed");
            e.to_string()
        })
}

/// F24 退出流程：联动关闭 ComfyUI + 资源释放 + app.exit
///
/// 由前端在弹确认对话框后调用。`ShutdownCoordinator` 内部：
/// 1. CAS 防重入（多次调用仅首次执行）
/// 2. 广播 `AppExiting` 事件
/// 3. 调用 `process_launcher.stop_with_reason(StopReason::Shutdown)` 走进程组终止
/// 4. 资源释放（500ms）
/// 5. 广播 `AppExited` + `app.exit(0)`
///
/// 30s 总超时兜底：超时时 `std::process::exit(0)` 强制退出。
///
/// # 参数
/// - `reason`: 退出原因（前端从 [WindowClose / TrayQuit / ShortcutCtrlQ / Restart] 中选）
///
/// # 返回
/// `ShutdownReport { comfyui_was_running, stop_elapsed_ms, reason }`
#[tauri::command]
pub async fn shutdown_all(
    app: AppHandle,
    state: State<'_, AppState>,
    reason: ShutdownReason,
) -> Result<ShutdownReport, String> {
    tracing::info!(?reason, "shutdown_all invoked");
    state
        .shutdown_coordinator
        .shutdown_all(app, reason)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AdvancedArgs, Config, LaunchConfig, LaunchMode, PathsConfig, PreviewMethod,
        TorchConfig, UiConfig,
    };
    use std::path::PathBuf;

    fn make_test_config(mode: LaunchMode, custom_args: &str) -> Config {
        Config {
            paths: PathsConfig {
                comfyui_root: PathBuf::from("/tmp/comfyui"),
                venv_path: PathBuf::from("/tmp/venv"),
                python_version: "3.11".into(),
            },
            launch: LaunchConfig {
                mode,
                listen_host: "127.0.0.1".into(),
                listen_port: 8188,
                auto_open_browser: true,
                preview_method: PreviewMethod::Latent,
                custom_args: custom_args.into(),
                advanced: AdvancedArgs::default(),
            },
            torch: TorchConfig {
                cuda_version: crate::config::CudaVersion::Cu121,
            },
            models: crate::config::ModelsConfig {
                mode: crate::config::ModelsMode::Default,
                custom_root: PathBuf::new(),
                advanced: Default::default(),
            },
            ui: UiConfig {
                theme: crate::config::Theme::Auto,
                language: "zh-CN".into(),
                auto_check_update: true,
                minimize_to_tray: true,
            },
            schema_version: 1,
        }
    }

    #[test]
    fn test_build_launch_args_from_config_gpu_high() {
        let cfg = make_test_config(LaunchMode::GpuHigh, "");
        let args = build_launch_args_from_config(&cfg);
        assert_eq!(args.mode, LaunchMode::GpuHigh);
        assert_eq!(args.listen_port, 8188);
        assert_eq!(args.listen_host, "127.0.0.1");
        assert_eq!(args.preview_method, PreviewMethod::Latent);
        assert!(args.auto_launch);
        assert!(args.custom_args.is_none(), "空字符串应转为 None");
    }

    #[test]
    fn test_build_launch_args_from_config_custom_with_args() {
        let cfg = make_test_config(
            LaunchMode::Custom,
            "--disable-smart-memory --reserve-vram 1",
        );
        let args = build_launch_args_from_config(&cfg);
        assert_eq!(args.mode, LaunchMode::Custom);
        assert_eq!(
            args.custom_args.as_deref(),
            Some("--disable-smart-memory --reserve-vram 1")
        );
    }

    #[test]
    fn test_build_launch_args_from_config_whitespace_only() {
        let cfg = make_test_config(LaunchMode::Custom, "   \n\t  ");
        let args = build_launch_args_from_config(&cfg);
        assert!(
            args.custom_args.is_none(),
            "纯空白字符串应转为 None（避免 ComfyUI 收到空参数）"
        );
    }

    #[test]
    fn test_build_launch_args_preserves_advanced() {
        let mut cfg = make_test_config(LaunchMode::GpuLow, "");
        cfg.launch.advanced.force_fp32 = true;
        cfg.launch.advanced.no_half = true;
        let args = build_launch_args_from_config(&cfg);
        assert!(args.advanced.force_fp32);
        assert!(args.advanced.no_half);
        assert!(!args.advanced.fp16_vae);
    }

    #[test]
    fn test_build_launch_args_auto_launch_maps_from_auto_open_browser() {
        let mut cfg = make_test_config(LaunchMode::GpuHigh, "");
        cfg.launch.auto_open_browser = false;
        let args = build_launch_args_from_config(&cfg);
        assert!(!args.auto_launch, "auto_open_browser=false 应映射为 auto_launch=false");
    }
}
