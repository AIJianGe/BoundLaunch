//! 进程启动流程：前置校验 + spawn + 日志 task + 健康检查
//!
//! 详见 `PR/03-模块设计/06-ProcessLauncher.md §5.1 启动流程` 与 `§10 崩溃恢复`
//!
//! 设计要点：
//! - **前置校验**：verify_venv + dirty 标记检查，防止 torch 缺失时启动
//! - **PID 文件**：崩溃恢复用，记录 pid/started_at/args 三元组
//! - **健康检查（v3.4.2 完全异步）**：2s 间隔轮询 `/system_stats`，**无超时**（无限轮询直到 ready / cancel / crash）
//!   - 之前 60s 超时触发 stop 会误杀慢启动（机械盘 / 大模型加载）；现在改为事件通知
//!   - 取消由 `CancellationToken` 控制（stop_impl 调用时 cancel）
//!   - 每 30s emit 一次 `process_health_warning` 事件，前端可显示"启动较慢"提示
//! - **stdout/stderr 行读取**：独立 task，无锁 mpsc 推送到 LogPipeline
//!
//! 设计模式：
//! - **Template Method**：start 流程的步骤序列固定（preconditions → spawn → tasks）
//! - **Adapter**：跨平台 venv python 路径推断（Windows 用 `Scripts/python.exe`）

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdout};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::common::process_util::decode_windows_bytes;
use crate::config::LaunchMode;
use crate::error::ProcessError;
use crate::process_launcher::models::LaunchArgs;
use crate::process_launcher::log_pipeline::LogPipeline;
use crate::python_env::PythonEnvService;
use crate::python_env::models::EnvInfo;
use crate::task_scheduler::progress::ProgressSender;

/// 健康检查轮询间隔（v3.4.2 调整为 2s）
///
/// 之前 1s 轮询对冷启动 ComfyUI 太密集（HTTP 请求每秒一次），改 2s 更友好。
const HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(2);

/// v3.4.2：健康检查不再有总超时（之前的 60s 上限是 bug 误杀慢启动）
///
/// 保留常量以备未来扩展用，目前函数内部不引用。
#[allow(dead_code)]
pub const HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(60);

/// 单次健康检查 HTTP 请求超时（v3.4.2 调整为 5s）
///
/// 之前 2s 在 Windows 上 HTTP 客户端初始化慢时容易 false-negative。
/// 5s 留足缓冲，HTTP 失败重试代价小（2s 间隔 + 5s timeout = 7s/轮）。
const HEALTH_CHECK_HTTP_TIMEOUT: Duration = Duration::from_secs(5);

/// v3.4.2：启动缓慢警告间隔（30s 推一次 process_health_warning）
const HEALTH_WARNING_INTERVAL: Duration = Duration::from_secs(30);

/// PID 文件名（位于用户数据目录）
pub const PID_FILE_NAME: &str = "comfyui.pid";

/// dirty 标记文件名（位于 comfyui_root）
pub const DIRTY_MARKER_FILE: &str = ".launcher-dirty";

/// ComfyUI main.py 路径（相对 comfyui_root）
const MAIN_PY: &str = "main.py";

// ============================================================================
// 前置校验
// ============================================================================

/// 启动前置校验：verify_venv + dirty 标记 + CUDA 模式匹配
///
/// 失败时返回的 ProcessError 会让前端展示具体提示：
/// - `EnvironmentNotReady`：venv 损坏 / torch 缺失 / CUDA 模式不匹配
/// - `DirtyState`：检测到 .launcher-dirty 标记
/// - `PythonNotFound`：python 二进制不存在
/// - `MainNotFound`：ComfyUI main.py 不存在
///
/// **v3.10 关键修复**：增加 `mode` 参数，检查 `cuda_available` 与 `LaunchMode` 是否匹配
/// - 背景：之前只检查 `torch_installed`，导致 torch+cpu 也能 spawn，ComfyUI 启动时
///   `import comfy.model_management` → `torch.cuda.current_device()` 抛
///   `AssertionError: Torch not compiled with CUDA enabled`
/// - 修复：Gpu* 模式时强制要求 `cuda_available=true`；Cpu 模式不要求；
///   Custom 模式信任用户（不强制检查）
pub async fn verify_preconditions(
    python_env: &PythonEnvService,
    venv_path: &Path,
    comfyui_root: &Path,
    mode: LaunchMode,
) -> Result<EnvInfo, ProcessError> {
    // 1. venv python 存在性检查（提前失败，避免 spawn 后才发现）
    let python_path = venv_python_path(venv_path);
    if !python_path.exists() {
        tracing::error!(?python_path, "venv python binary not found");
        return Err(ProcessError::PythonNotFound(
            python_path.to_string_lossy().to_string(),
        ));
    }

    // 2. ComfyUI main.py 存在性检查
    let main_py = comfyui_root.join(MAIN_PY);
    if !main_py.exists() {
        tracing::error!(?main_py, "ComfyUI main.py not found");
        return Err(ProcessError::MainNotFound(
            main_py.to_string_lossy().to_string(),
        ));
    }

    // 3. dirty 标记检查
    let dirty_path = comfyui_root.join(DIRTY_MARKER_FILE);
    if dirty_path.exists() {
        tracing::warn!(?dirty_path, "dirty state detected, refuse to start");
        return Err(ProcessError::DirtyState {
            detail: "torch 缺失或环境异常，请到设置页重新初始化".into(),
        });
    }

    // 4. verify_venv 完整性
    //    v3.6：用本地不可取消 token（start() 方法暂未透传 cancel），
    //    已移除 90s 硬超时，最坏情况是用户等 torch import 完成
    let cancel = CancellationToken::new();
    let info = python_env.verify_venv(venv_path, &cancel).await?;
    if !info.torch_installed {
        tracing::error!(?venv_path, "torch not installed after verify_venv");
        return Err(ProcessError::EnvironmentNotReady {
            detail: "torch 未安装，请先在设置页安装 torch".into(),
        });
    }

    // 5. v3.10 新增：检查 cuda_available 与 launch_mode 是否匹配
    //    解决 torch+cpu + GpuHigh 启动后 AssertionError 的问题
    //    Custom 模式信任用户（用户可能自定义了 --cpu / --highvram），不强制检查
    let gpu_mode_required = matches!(
        mode,
        LaunchMode::GpuHigh | LaunchMode::GpuLow | LaunchMode::GpuNoVram
    );
    if gpu_mode_required && !info.cuda_available {
        let torch_ver = info.torch_version.as_deref().unwrap_or("?");
        tracing::error!(
            torch = %torch_ver,
            cuda_available = info.cuda_available,
            mode = ?mode,
            "launch mode requires GPU but torch.cuda_available=false"
        );
        return Err(ProcessError::EnvironmentNotReady {
            detail: format!(
                "检测到 PyTorch 不支持 CUDA（torch={}），\n\
                 但启动模式为 {:?}（需要 GPU）。\n\n\
                 解决方法：\n\
                 1. 到「设置 → 关键依赖」点击「重新安装 PyTorch」（选择正确的 cuda 版本）\n\
                 2. 或在「基础参数 → 运行模式」中切换到「CPU 模式」\n\n\
                 常见原因：\n\
                 • 安装时选错了 cuda 版本\n\
                 • venv 是早期版本装的，torch/torchvision/torchaudio 来自不同源（版本不一致）",
                torch_ver, mode,
            ),
        });
    }

    tracing::info!(
        python = %info.python_version,
        torch = ?info.torch_version,
        cuda_available = info.cuda_available,
        mode = ?mode,
        "preconditions verified"
    );
    Ok(info)
}

/// 端口可用性预检（bind 测试）
///
/// 若端口已被占用，spawn 后 ComfyUI 会立即报错；提前检查可给出更友好的错误。
pub async fn check_port_available(host: &str, port: u16) -> Result<(), ProcessError> {
    let addr = format!("{}:{}", host, port);
    match tokio::net::TcpListener::bind(&addr).await {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            tracing::warn!(port, "port already in use");
            Err(ProcessError::PortInUse { port })
        }
        Err(e) => {
            tracing::warn!(?addr, error = %e, "port bind test failed");
            // 其他错误（如权限不足）不阻塞，让 spawn 阶段决定
            Ok(())
        }
    }
}

// ============================================================================
// spawn
// ============================================================================

/// spawn ComfyUI 子进程
///
/// - 设置工作目录为 `comfyui_root`
/// - 关闭 stdin（避免 ComfyUI 等待输入）
/// - piped stdout/stderr（供 reader task 消费）
/// - **消除 cmd 窗口**：使用 `common::process_util::new_command` 统一加 `CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP`
///   （v3.3 之前在 Windows 上子进程会弹 cmd 窗口；v3.4 统一到 new_command 避免重复实现）
/// - **F24 进程组隔离**：
///   - Unix：`setsid()` 创建新 session（也是新进程组），让 ComfyUI + 其 Python worker 子进程同组
///     → 关闭 launcher 时用 `kill -<pgid>` 整体终止，避免 python worker 残留
///   - Windows：`CREATE_NEW_PROCESS_GROUP` 让 ComfyUI 成为新进程组根
///     → 关闭 launcher 时用 `taskkill /F /T /PID <pid>` 终止进程树（Windows 现有 /T 已覆盖）
pub fn spawn_process(
    venv_python: &Path,
    comfyui_root: &Path,
    cmd_args: Vec<String>,
) -> Result<Child, ProcessError> {
    spawn_process_with_env(venv_python, comfyui_root, cmd_args, &[])
}

/// **v3.x Phase 5**：spawn + 注入额外环境变量（CUDA_VISIBLE_DEVICES 等）
///
/// 为什么不直接在 `spawn_process` 内访问 `ConfigService`：
/// - `process_launcher::start` 已被 `process_launcher::service` 调用（持有 ConfigService）
/// - 避免循环依赖 + 单测 mock
/// - 调用方传入 gpu_selection，决定是否注入 `CUDA_VISIBLE_DEVICES`
pub fn spawn_process_with_env(
    venv_python: &Path,
    comfyui_root: &Path,
    cmd_args: Vec<String>,
    extra_env: &[(&str, String)],
) -> Result<Child, ProcessError> {
    // v3.4 统一改用 process_util::new_command：消除此处重复实现 creation_flags
    // （Windows 上自动加 CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP，非 Windows 等价 Command::new）
    let mut cmd = crate::common::process_util::new_command(venv_python);
    cmd.args(&cmd_args)
        .current_dir(comfyui_root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    // **v3.x Phase 5**：注入额外环境变量（如 CUDA_VISIBLE_DEVICES）
    for (k, v) in extra_env {
        cmd.env(k, v);
    }

    // Unix 进程组隔离：spawn 后立即调 setsid()，让 ComfyUI + 其子进程归属新 session
    // pre_exec 在 fork 之后、exec 之前执行，仍在子进程上下文中
    // 失败返回 Err → spawn 失败向上传播
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                // setsid() 在子进程内调用 → 创建新 session + 新进程组
                // nix::unistd::setsid 返回 Result<Pid>，Err 时返回 std::io::Error
                // 让 pre_exec 闭包返回 Err，spawn 阶段即可捕获
                nix::unistd::setsid().map(|_| ()).map_err(|e| {
                    std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("setsid failed: {}", e),
                    )
                })
            });
        }
    }

    let child = cmd
        .spawn()
        .map_err(|e| ProcessError::SpawnFailed(e.to_string()))?;
    Ok(child)
}

// ============================================================================
// 日志 reader task
// ============================================================================

/// 启动 stdout 读取 task：按行推送到 LogPipeline
pub fn spawn_stdout_reader(stdout: ChildStdout, pipeline: Arc<LogPipeline>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout).lines();
        loop {
            match reader.next_line().await {
                Ok(Some(line)) => {
                    pipeline.push("stdout", line);
                }
                Ok(None) => {
                    tracing::debug!("stdout EOF");
                    break;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "stdout read error");
                    break;
                }
            }
        }
    })
}

/// 启动 stderr 读取 task：按行推送到 LogPipeline
pub fn spawn_stderr_reader(stderr: ChildStderr, pipeline: Arc<LogPipeline>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr).lines();
        loop {
            match reader.next_line().await {
                Ok(Some(line)) => {
                    pipeline.push("stderr", line);
                }
                Ok(None) => {
                    tracing::debug!("stderr EOF");
                    break;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "stderr read error");
                    break;
                }
            }
        }
    })
}

// ============================================================================
// 健康检查
// ============================================================================

/// 健康检查结果回调
pub enum HealthCheckOutcome {
    /// 就绪（GET /system_stats 返回 2xx）
    Ready,
    /// 超时（HEALTH_CHECK_TIMEOUT 60s 内未就绪）
    ///
    /// v3.4.2：移除 60s 超时后此分支基本不会触发，保留兜底
    Timeout,
    /// v3.4：child 在 health_check 期间死亡（try_wait 检测到 exit）
    ///
    /// 载荷 `Option<i32>` 是 exit code（None 表示被信号杀死）
    Crashed(Option<i32>),
    /// v3.4.2 新增：取消令牌触发（stop_impl 调用时）
    ///
    /// health_check 收到 cancel 信号后直接退出，不动状态。
    /// stop_impl 负责把状态推进到 Stopped/Crashed。
    Cancelled,
}

/// 启动健康检查轮询 task（v3.4.2 完全异步版）
///
/// **关键变化**：
/// - 接收 `cancel_token: CancellationToken`：
///   - stop_impl 调用时调 `cancel_token.cancel()` → health_check 立即退出
///   - 之前是 `JoinHandle::abort()`，无法区分"自然完成"和"被取消"
/// - 移除 `HEALTH_CHECK_TIMEOUT` 限制：之前 60s 上限对机械盘 / 大模型加载太短，
///   现在改为无限轮询，直到：
///   - a) child 死了（try_wait 返回 Some）→ Crashed
///   - b) ComfyUI 就绪（HTTP 200）→ Ready
///   - c) cancel_token 被 cancel → Cancelled
///   - d) 用户应用关闭（cancel_token 兜底）→ Cancelled
/// - 每 `HEALTH_WARNING_INTERVAL`（30s）emit 一次 `process_health_warning` 事件，
///   前端可监听并显示"启动较慢"提示
///
/// **进度推送**：
/// - ready → push_percent(100) + send_message("ComfyUI 已就绪")
/// - 每轮：发"等待 ComfyUI 就绪（Xs）"消息，**不再发百分数**（之前 60-90% 进度条不再适用）
///
/// # 参数
/// - `inner`：共享 `Arc<Inner>`，用于 try_wait 共享的 `Child` 句柄 + 读 log_pipeline
/// - `cancel_token`：取消令牌，stop_impl 调用时 cancel
pub fn spawn_health_check(
    port: u16,
    app: tauri::AppHandle,
    inner: Arc<crate::process_launcher::Inner>,
    progress: Option<ProgressSender>,
    cancel_token: CancellationToken,
) -> JoinHandle<HealthCheckOutcome> {
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{}/system_stats", port);
        let start = std::time::Instant::now();

        // 第一次推 health_warning 的时间点（30s 后）
        let mut next_warning_at = HEALTH_WARNING_INTERVAL;

        loop {
            // v3.4.2：检查取消令牌（优先级最高，避免 sleep 阻塞 cancel）
            // 用 `tokio::select!` 同时等 sleep 和 cancel，但这里简单点用 is_cancelled 轮询
            if cancel_token.is_cancelled() {
                tracing::info!(port, "health check cancelled");
                return HealthCheckOutcome::Cancelled;
            }

            // v3.4 关键修复：先检查 child 是否还活着（每次轮询前）
            // 修复了之前"child 启动 0.5s 就死，health_check 傻等 60s 才超时"的 bug
            let child_exit = {
                let mut guard = inner.child.lock().await;
                match guard.as_mut() {
                    Some(child) => child.try_wait().ok().flatten(),
                    None => None, // child 已被 take（cleanup_after_exit）→ 视为已退出
                }
            };
            if let Some(exit_status) = child_exit {
                let exit_code = exit_status.code();
                tracing::warn!(
                    port,
                    ?exit_code,
                    "health_check: child already exited (early death detected)"
                );
                if let Some(ref p) = progress {
                    p.send_percent(95);
                    p.send_message(format!(
                        "ComfyUI 进程已退出（exit code: {:?}）",
                        exit_code
                    ));
                }
                use tauri::Emitter;
                // 拿 log_pipeline 最近的 stderr tail
                let stderr_tail = inner
                    .log_pipeline
                    .read()
                    .as_ref()
                    .map(|p| p.tail(50))
                    .unwrap_or_default();
                let _ = app.emit(
                    "process_crashed",
                    serde_json::json!({
                        "exit_code": exit_code,
                        "stderr_tail": stderr_tail,
                        "reason": "health_check_detected",
                    }),
                );
                return HealthCheckOutcome::Crashed(exit_code);
            }

            // v3.4.2：移除 60s 超时限制。改用 cancel_token 控制生命周期。
            // 慢启动（如机械盘 / 大模型加载）现在不会被误杀。

            // 周期性推 health_warning（30s / 60s / 90s ...）
            let elapsed = start.elapsed();
            if elapsed >= next_warning_at {
                use tauri::Emitter;
                let _ = app.emit(
                    "process_health_warning",
                    serde_json::json!({
                        "elapsed": elapsed.as_secs(),
                        "port": port,
                    }),
                );
                tracing::info!(
                    port,
                    elapsed_secs = elapsed.as_secs(),
                    "health_check: still waiting, emit warning"
                );
                next_warning_at += HEALTH_WARNING_INTERVAL;
            }

            // v3.4.2：每轮仅推消息（不推百分数），让前端显示"等待 ComfyUI 就绪（Xs）"
            if let Some(ref p) = progress {
                p.send_message(format!(
                    "等待 ComfyUI 就绪（{:.0}s）",
                    elapsed.as_secs_f32()
                ));
            }

            // v3.4.2：HTTP 请求失败不报错，仅 debug 记录
            // - 之前 `Ok(resp) =>` 分支只在 2xx 算成功
            // - 现在 reqwest::Client 已配 5s timeout，单次失败不致命
            match client
                .get(&url)
                .timeout(HEALTH_CHECK_HTTP_TIMEOUT)
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    tracing::info!(port, elapsed_secs = elapsed.as_secs(), "health check passed, ComfyUI ready");
                    if let Some(ref p) = progress {
                        p.send_percent(100);
                        p.send_message("ComfyUI 已就绪".to_string());
                    }
                    use tauri::Emitter;
                    let _ = app.emit("process_ready", serde_json::json!({ "port": port }));
                    return HealthCheckOutcome::Ready;
                }
                Ok(resp) => {
                    tracing::debug!(
                        port,
                        status = %resp.status(),
                        "health check non-200, will retry"
                    );
                }
                Err(e) => {
                    tracing::debug!(port, error = %e, "health check request failed, will retry");
                }
            }

            // v3.4.2：用 tokio::select! 让 sleep 与 cancel 竞争
            // - 之前是 `tokio::time::sleep(HEALTH_CHECK_INTERVAL).await`，cancel 时也要等满 2s
            // - 现在 select! 让 cancel 立即生效
            tokio::select! {
                _ = tokio::time::sleep(HEALTH_CHECK_INTERVAL) => {}
                _ = cancel_token.cancelled() => {
                    tracing::info!(port, "health check cancelled (during sleep)");
                    return HealthCheckOutcome::Cancelled;
                }
            }
        }
    })
}

// ============================================================================
// PID 文件管理（崩溃恢复）
// ============================================================================

/// PID 文件内容
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PidFileEntry {
    pid: u32,
    started_at: DateTime<Utc>,
    args: LaunchArgs,
}

/// 写 PID 文件（spawn 成功后调用）
pub async fn write_pid_file(
    path: &Path,
    pid: u32,
    started_at: DateTime<Utc>,
    args: &LaunchArgs,
) -> Result<(), ProcessError> {
    let entry = PidFileEntry {
        pid,
        started_at,
        args: args.clone(),
    };
    let content = serde_json::to_string(&entry)
        .map_err(|e| ProcessError::Io(format!("serialize pid file failed: {}", e)))?;
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }
    tokio::fs::write(path, content).await?;
    tracing::debug!(?path, pid, "pid file written");
    Ok(())
}

/// 读 PID 文件
///
/// 返回 None 的情况：
/// - 文件不存在
/// - 文件损坏（JSON 解析失败）
pub async fn read_pid_file(path: &Path) -> Option<(u32, DateTime<Utc>, LaunchArgs)> {
    let content = tokio::fs::read_to_string(path).await.ok()?;
    let entry: PidFileEntry = serde_json::from_str(&content).ok()?;
    Some((entry.pid, entry.started_at, entry.args))
}

/// 删除 PID 文件（进程正常退出后调用）
pub async fn remove_pid_file(path: &Path) {
    if let Err(e) = tokio::fs::remove_file(path).await {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(?path, error = %e, "failed to remove pid file");
        }
    }
}

/// 检查 PID 对应进程是否为 ComfyUI（防 PID 复用误杀）
///
/// - Windows：调用 wmic 查 CommandLine
/// - Linux：读 /proc/<pid>/cmdline
/// - macOS：调用 `ps -p <pid> -o command=` 查命令行
///
/// 设计要点：
/// - macOS 没有 `/proc` 文件系统（这是 Linux 内核虚拟 FS）
/// - macOS 上必须改用 `ps` 命令查询，否则函数永远返回 false
/// - 三平台分支用 cfg 隔离，避免互相污染
pub async fn is_comfyui_process(pid: u32) -> bool {
    #[cfg(target_os = "windows")]
    {
        // v3.3：使用 new_command 在 Windows 上加 CREATE_NO_WINDOW，避免弹 cmd 窗口
        // v3.4.2：wmic 输出可能含 GBK 编码的中文 CommandLine，按 GBK 解码
        let output = crate::common::process_util::new_command("wmic")
            .args([
                "process",
                "where",
                &format!("ProcessId={}", pid),
                "get",
                "CommandLine",
            ])
            .output()
            .await;
        match output {
            Ok(o) if o.status.success() => {
                let cmdline = decode_windows_bytes(&o.stdout);
                cmdline.contains("main.py") && cmdline.contains("comfyui")
            }
            _ => false,
        }
    }

    #[cfg(target_os = "linux")]
    {
        // Linux 内核提供 /proc/<pid>/cmdline 虚拟文件，读取无需 root
        let path = format!("/proc/{}/cmdline", pid);
        match tokio::fs::read(&path).await {
            Ok(content) => {
                // /proc/<pid>/cmdline 用 \0 分隔参数，故先转空格再判断
                let s = String::from_utf8_lossy(&content).replace('\0', " ");
                s.contains("main.py") && s.contains("comfyui")
            }
            Err(_) => false,
        }
    }

    #[cfg(target_os = "macos")]
    {
        // macOS 无 /proc 文件系统，改用 `ps -p <pid> -o command=` 查命令行
        // -p: 按 PID 过滤；-o command=: 仅输出命令列（无表头）
        let pid_str = pid.to_string();
        // v3.3：使用 new_command 在 Windows 上加 CREATE_NO_WINDOW（macOS 无影响）
        // v3.4.2：ps 输出按 UTF-8 解码（macOS 终端是 UTF-8，与 Linux 相同）
        let output = crate::common::process_util::new_command("ps")
            .args(["-p", &pid_str, "-o", "command="])
            .output()
            .await;
        match output {
            Ok(o) if o.status.success() => {
                let cmdline = String::from_utf8_lossy(&o.stdout);
                // ps 返回空字符串表示进程已退出
                if cmdline.trim().is_empty() {
                    return false;
                }
                cmdline.contains("main.py") && cmdline.contains("comfyui")
            }
            // ps 退出码非 0：通常表示 PID 不存在（已退出）
            _ => false,
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        tracing::warn!("is_comfyui_process: unsupported platform, returning false");
        false
    }
}

// ============================================================================
// 辅助函数
// ============================================================================

/// 推断 venv python 二进制路径
///
/// - Windows：`<venv>/Scripts/python.exe`
/// - Unix：`<venv>/bin/python`
pub fn venv_python_path(venv_path: &Path) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        venv_path.join("Scripts").join("python.exe")
    }
    #[cfg(not(target_os = "windows"))]
    {
        venv_path.join("bin").join("python")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_venv_python_path_windows_layout() {
        // 仅验证 join 逻辑不 panic
        let path = venv_python_path(Path::new("/tmp/venv"));
        assert!(path.to_string_lossy().contains("python"));
    }

    #[test]
    fn test_pid_file_entry_serialize_roundtrip() {
        let entry = PidFileEntry {
            pid: 1234,
            started_at: Utc::now(),
            args: LaunchArgs::defaults(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: PidFileEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.pid, 1234);
    }

    #[tokio::test]
    async fn test_pid_file_write_read_remove_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("comfyui.pid");
        let started_at = Utc::now();
        let args = LaunchArgs::defaults();

        // 写
        write_pid_file(&path, 1234, started_at, &args)
            .await
            .unwrap();

        // 读
        let (pid, ts, _) = read_pid_file(&path).await.unwrap();
        assert_eq!(pid, 1234);
        assert_eq!(ts, started_at);

        // 删
        remove_pid_file(&path).await;
        assert!(read_pid_file(&path).await.is_none());
    }

    #[tokio::test]
    async fn test_read_pid_file_missing_returns_none() {
        let path = Path::new("/nonexistent/path/comfyui.pid");
        assert!(read_pid_file(path).await.is_none());
    }

    #[tokio::test]
    async fn test_read_pid_file_corrupt_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("comfyui.pid");
        tokio::fs::write(&path, "not json")
            .await
            .unwrap();
        assert!(read_pid_file(&path).await.is_none());
    }

    #[tokio::test]
    async fn test_is_comfyui_process_nonexistent_pid() {
        // PID 99999999 几乎不可能存在
        assert!(!is_comfyui_process(99999999).await);
    }

    #[tokio::test]
    async fn test_check_port_available_free_port() {
        // 绑定到临时端口（操作系统分配）
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        // 释放后绑定应成功
        let result = check_port_available("127.0.0.1", port).await;
        assert!(result.is_ok(), "expected Ok for free port: {:?}", result);
    }

    #[test]
    fn test_dirty_marker_file_name() {
        assert_eq!(DIRTY_MARKER_FILE, ".launcher-dirty");
    }

    #[test]
    fn test_pid_file_name() {
        assert_eq!(PID_FILE_NAME, "comfyui.pid");
    }
}
