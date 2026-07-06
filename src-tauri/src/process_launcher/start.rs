//! 进程启动流程：前置校验 + spawn + 日志 task + 健康检查
//!
//! 详见 `PR/03-模块设计/06-ProcessLauncher.md §5.1 启动流程` 与 `§10 崩溃恢复`
//!
//! 设计要点：
//! - **前置校验**：verify_venv + dirty 标记检查，防止 torch 缺失时启动
//! - **PID 文件**：崩溃恢复用，记录 pid/started_at/args 三元组
//! - **健康检查**：1s 间隔轮询 `/system_stats`，60s 超时触发 stop
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
use tokio::process::{Child, ChildStderr, ChildStdout, Command};
use tokio::task::JoinHandle;

use crate::error::ProcessError;
use crate::process_launcher::models::LaunchArgs;
use crate::process_launcher::log_pipeline::LogPipeline;
use crate::python_env::PythonEnvService;
use crate::python_env::models::EnvInfo;

/// 健康检查轮询间隔
const HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(1);

/// 健康检查总超时（60s）
pub const HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(60);

/// 单次健康检查 HTTP 请求超时
const HEALTH_CHECK_HTTP_TIMEOUT: Duration = Duration::from_secs(2);

/// PID 文件名（位于用户数据目录）
pub const PID_FILE_NAME: &str = "comfyui.pid";

/// dirty 标记文件名（位于 comfyui_root）
pub const DIRTY_MARKER_FILE: &str = ".launcher-dirty";

/// ComfyUI main.py 路径（相对 comfyui_root）
const MAIN_PY: &str = "main.py";

// ============================================================================
// 前置校验
// ============================================================================

/// 启动前置校验：verify_venv + dirty 标记
///
/// 失败时返回的 ProcessError 会让前端展示具体提示：
/// - `EnvironmentNotReady`：venv 损坏 / torch 缺失
/// - `DirtyState`：检测到 .launcher-dirty 标记
/// - `PythonNotFound`：python 二进制不存在
/// - `MainNotFound`：ComfyUI main.py 不存在
pub async fn verify_preconditions(
    python_env: &PythonEnvService,
    venv_path: &Path,
    comfyui_root: &Path,
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
    let info = python_env.verify_venv(venv_path).await?;
    if !info.torch_installed {
        tracing::error!(?venv_path, "torch not installed after verify_venv");
        return Err(ProcessError::EnvironmentNotReady {
            detail: "torch 未安装，请先在设置页安装 torch".into(),
        });
    }

    tracing::info!(
        python = %info.python_version,
        torch = ?info.torch_version,
        cuda_available = info.cuda_available,
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
/// - Windows 隐藏控制台窗口
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
    let mut cmd = Command::new(venv_python);
    cmd.args(&cmd_args)
        .current_dir(comfyui_root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    // Windows 进程标志：CREATE_NO_WINDOW + CREATE_NEW_PROCESS_GROUP
    // 注：tokio::process::Command 在 Windows 上原生提供 `creation_flags` 方法
    // （无需 `use std::os::windows::process::CommandExt`，否则会触发 unused_import 警告）
    #[cfg(target_os = "windows")]
    {
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        // CREATE_NEW_PROCESS_GROUP = 0x00000200
        // 让 ComfyUI 成为新进程组的根，配合 taskkill /T 整体终止
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
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
    /// 就绪
    Ready,
    /// 超时
    Timeout,
}

/// 启动健康检查轮询 task
///
/// 每 `HEALTH_CHECK_INTERVAL` GET `http://127.0.0.1:<port>/system_stats`：
/// - 200 → emit("process_ready") 并返回 `Ready`
/// - 累计 `HEALTH_CHECK_TIMEOUT` 仍未通过 → emit("health_timeout") 并返回 `Timeout`
pub fn spawn_health_check(
    port: u16,
    app: tauri::AppHandle,
) -> JoinHandle<HealthCheckOutcome> {
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{}/system_stats", port);
        let start = std::time::Instant::now();

        loop {
            if start.elapsed() > HEALTH_CHECK_TIMEOUT {
                tracing::warn!(
                    port,
                    timeout = ?HEALTH_CHECK_TIMEOUT,
                    "health check timed out"
                );
                use tauri::Emitter;
                let _ = app.emit("health_timeout", serde_json::json!({ "port": port }));
                return HealthCheckOutcome::Timeout;
            }

            match client
                .get(&url)
                .timeout(HEALTH_CHECK_HTTP_TIMEOUT)
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    tracing::info!(port, "health check passed, ComfyUI ready");
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

            tokio::time::sleep(HEALTH_CHECK_INTERVAL).await;
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
                let cmdline = String::from_utf8_lossy(&o.stdout);
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
