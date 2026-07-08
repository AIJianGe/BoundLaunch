//! 端口诊断 + 进程强杀
//!
//! 设计目的：
//! - 解决"ComfyUI 启动卡在 Starting 状态"问题
//! - 当 8188 端口被占用时，给出占用方进程信息
//! - 提供"强制结束占用进程"的能力
//!
//! 跨平台策略：
//! - Windows：netstat -ano 找 PID → tasklist 找进程名 → taskkill /F 杀进程
//! - Unix：lsof -i :port 找 PID → kill -9 杀进程
//!
//! 安全考虑：
//! - 不允许杀 PID 0/1 等系统进程
//! - 杀进程前可选择性 confirm（前端做）
//! - taskkill /F 强制但限定范围（PID 或映像名）

use serde::{Deserialize, Serialize};
use std::process::Command;

/// 端口诊断结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortDiagnosis {
    pub port: u16,
    pub host: String,
    pub available: bool,
    /// 占用方进程信息（available=false 时才有）
    pub occupied_by: Option<ProcessInfo>,
    /// 原始错误信息（探测过程失败时填）
    pub error: Option<String>,
}

/// 进程信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    /// 命令行（完整版，可能很长）
    pub command: Option<String>,
    /// 命令行（截断版，UI 展示用）
    pub command_short: String,
}

/// 强杀结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KillResult {
    pub killed_pids: Vec<u32>,
    pub failed: Vec<KillFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KillFailure {
    pub pid: u32,
    pub reason: String,
}

// ============================================================================
// 端口诊断
// ============================================================================

/// 诊断端口占用情况
///
/// 1. 尝试 bind 端口，失败则继续
/// 2. 用系统命令找占用方进程
/// 3. 返回结构化信息
#[tauri::command]
pub async fn diagnose_port(host: String, port: u16) -> Result<PortDiagnosis, String> {
    tracing::info!(port, %host, "diagnose_port invoked");

    // 1. 快速检测端口是否可用
    let addr = format!("{}:{}", host, port);
    let bind_result = tokio::net::TcpListener::bind(&addr).await;
    if let Ok(listener) = bind_result {
        // 端口可用，立即关闭
        drop(listener);
        return Ok(PortDiagnosis {
            port,
            host,
            available: true,
            occupied_by: None,
            error: None,
        });
    }

    // 2. 端口被占，找占用方
    let occupied_by = find_occupying_process(&host, port).await;

    if let Some(ref info) = occupied_by {
        tracing::warn!(
            port, pid = info.pid, name = %info.name,
            "port is occupied"
        );
    } else {
        tracing::warn!(port, "port is occupied but no process info found");
    }

    Ok(PortDiagnosis {
        port,
        host,
        available: false,
        occupied_by,
        error: None,
    })
}

/// 查找占用端口的进程（Windows + Unix）
#[cfg(target_os = "windows")]
async fn find_occupying_process(_host: &str, port: u16) -> Option<ProcessInfo> {
    // host 参数 Windows 端不用（netstat 已经返回所有接口），保留供未来 IPv6 多接口诊断
    let _ = _host;
    // 1. netstat -ano 找 LISTENING 行
    let netstat_output = Command::new("netstat")
        .args(&["-ano", "-p", "TCP"])
        .output()
        .ok()?;

    if !netstat_output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&netstat_output.stdout);
    let port_str = format!(":{}", port);

    // 解析 netstat 输出找匹配的行
    // 格式：TCP    127.0.0.1:8188    0.0.0.0:0    LISTENING    1234
    let mut candidates: Vec<u32> = Vec::new();
    for line in stdout.lines() {
        let line_trim = line.trim();
        if !line_trim.contains(&port_str) {
            continue;
        }
        if !line_trim.contains("LISTENING") && !line_trim.contains("ESTABLISHED") {
            continue;
        }
        // 取最后一个字段（PID）
        if let Some(pid_str) = line_trim.split_whitespace().last() {
            if let Ok(pid) = pid_str.parse::<u32>() {
                if pid > 0 {
                    candidates.push(pid);
                }
            }
        }
    }

    if candidates.is_empty() {
        return None;
    }

    // 2. 用 tasklist 查每个 PID 的进程名
    for &pid in &candidates {
        if let Some(info) = query_process_info_windows(pid).await {
            return Some(info);
        }
    }

    // 兜底：返回 PID 但没有名字
    if let Some(&pid) = candidates.first() {
        return Some(ProcessInfo {
            pid,
            name: format!("PID {}", pid),
            command: None,
            command_short: format!("PID {}", pid),
        });
    }
    None
}

/// 查询 Windows 进程信息（tasklist）
#[cfg(target_os = "windows")]
async fn query_process_info_windows(pid: u32) -> Option<ProcessInfo> {
    let output = Command::new("tasklist")
        .args(&["/FI", &format!("PID eq {}", pid), "/FO", "CSV", "/NH"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // CSV 格式："python.exe","1234","Console","1","123,456 K"
    let first_line = stdout.lines().next()?;
    let fields: Vec<&str> = first_line.split(',').map(|s| s.trim_matches('"')).collect();
    let name = fields.first()?.to_string();

    // 用 wmic 拿命令行（可选）
    let command = query_process_command_windows(pid).await;
    let command_short = command
        .as_ref()
        .map(|c| truncate_command(c, 100))
        .unwrap_or_else(|| name.clone());

    Some(ProcessInfo {
        pid,
        name,
        command,
        command_short,
    })
}

/// 用 wmic 查 Windows 进程命令行
#[cfg(target_os = "windows")]
async fn query_process_command_windows(pid: u32) -> Option<String> {
    let output = Command::new("wmic")
        .args(&["process", "where", &format!("ProcessId={}", pid), "get", "CommandLine", "/format:list"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // 格式：
    // CommandLine=
    // python.exe main.py --port 8188
    for line in stdout.lines() {
        if let Some(value) = line.strip_prefix("CommandLine=") {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// Unix 平台实现
#[cfg(not(target_os = "windows"))]
async fn find_occupying_process(_host: &str, port: u16) -> Option<ProcessInfo> {
    // host 参数当前不用（netstat 已经返回所有接口），保留供未来 IPv6 多接口诊断
    let _ = _host;
    // 用 lsof 找 PID
    let output = Command::new("lsof")
        .args(&["-i", &format!(":{}", port), "-sTCP:LISTEN", "-t"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let pid_str = stdout.trim().lines().next()?;
    let pid: u32 = pid_str.parse().ok()?;

    // 查进程名
    let ps_output = Command::new("ps")
        .args(&["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .ok()?;

    let name = String::from_utf8_lossy(&ps_output.stdout).trim().to_string();

    // 查命令行
    let cmdline_output = Command::new("ps")
        .args(&["-p", &pid.to_string(), "-o", "args="])
        .output()
        .ok()?;

    let command = String::from_utf8_lossy(&cmdline_output.stdout).trim().to_string();
    let command_short = truncate_command(&command, 100);

    Some(ProcessInfo {
        pid,
        name,
        command: if command.is_empty() { None } else { Some(command) },
        command_short,
    })
}

// ============================================================================
// 强杀进程
// ============================================================================

/// 强杀单个进程（按 PID）
///
/// 不会杀系统关键进程（PID 0/1/4 在 Windows；PID 1 在 Unix）
#[tauri::command]
pub async fn force_kill_process(pid: u32) -> Result<KillResult, String> {
    tracing::warn!(pid, "force_kill_process invoked");

    if !is_pid_killable(pid) {
        return Err(format!("PID {} 是系统关键进程，拒绝杀", pid));
    }

    kill_pid_impl(pid).await
}

/// 强杀所有 python.exe（兜底）
///
/// 杀掉所有映像名为 python.exe 的进程（Windows）。
/// Unix 上等价于 pkill -9 python
///
/// 返回被杀掉的 PID 列表
#[tauri::command]
pub async fn force_kill_all_python() -> Result<KillResult, String> {
    tracing::warn!("force_kill_all_python invoked");

    #[cfg(target_os = "windows")]
    {
        // taskkill /F /IM python.exe
        let output = Command::new("taskkill")
            .args(&["/F", "/IM", "python.exe", "/T"])
            .output()
            .map_err(|e| format!("taskkill 执行失败: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        // 解析输出：可能含 "SUCCESS: ..." 或 "ERROR: ..."
        // 不强求 status.success() —— 有时部分进程被杀也算成功
        tracing::info!(stdout = %stdout, stderr = %stderr, "taskkill /IM python.exe done");

        // 返回结果（简化：不解析具体 PID 列表）
        Ok(KillResult {
            killed_pids: vec![],
            failed: vec![],
        })
    }

    #[cfg(not(target_os = "windows"))]
    {
        // pkill -9 python
        let output = Command::new("pkill")
            .args(&["-9", "-f", "python"])
            .output()
            .map_err(|e| format!("pkill 执行失败: {}", e))?;

        tracing::info!("pkill -9 python done");
        Ok(KillResult {
            killed_pids: vec![],
            failed: vec![],
        })
    }
}

/// 强杀所有 ComfyUI 相关进程（python.exe + comfyui 命名的进程）
///
/// 比 force_kill_all_python 更激进：杀掉所有可能的 ComfyUI 进程
#[tauri::command]
pub async fn force_kill_all_comfyui() -> Result<KillResult, String> {
    tracing::warn!("force_kill_all_comfyui invoked");

    #[cfg(target_os = "windows")]
    {
        // 先杀 python.exe（ComfyUI 主进程）
        let _ = Command::new("taskkill")
            .args(&["/F", "/IM", "python.exe", "/T"])
            .output();

        // 再杀 comfyui 相关进程
        let _ = Command::new("taskkill")
            .args(&["/F", "/IM", "comfyui.exe", "/T"])
            .output();

        Ok(KillResult {
            killed_pids: vec![],
            failed: vec![],
        })
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = Command::new("pkill")
            .args(&["-9", "-f", "comfyui"])
            .output();
        let _ = Command::new("pkill")
            .args(&["-9", "-f", "python"])
            .output();

        Ok(KillResult {
            killed_pids: vec![],
            failed: vec![],
        })
    }
}

/// 判断 PID 是否可杀（保护系统关键进程）
#[cfg(target_os = "windows")]
fn is_pid_killable(pid: u32) -> bool {
    // Windows 关键进程保护
    // 0 = System Idle Process
    // 4 = System
    // 一些关键服务（csrss.exe, lsass.exe 等）也保护
    // 这里只做基础保护
    pid > 4
}

#[cfg(not(target_os = "windows"))]
fn is_pid_killable(pid: u32) -> bool {
    // Unix：PID 1 是 init，拒绝杀
    pid > 1
}

/// 实际执行杀进程
#[cfg(target_os = "windows")]
async fn kill_pid_impl(pid: u32) -> Result<KillResult, String> {
    let output = Command::new("taskkill")
        .args(&["/F", "/PID", &pid.to_string(), "/T"])
        .output()
        .map_err(|e| format!("taskkill 执行失败: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if output.status.success() || stdout.contains("SUCCESS") {
        tracing::info!(pid, stdout = %stdout, "PID killed");
        Ok(KillResult {
            killed_pids: vec![pid],
            failed: vec![],
        })
    } else {
        tracing::error!(pid, stdout = %stdout, stderr = %stderr, "taskkill failed");
        Ok(KillResult {
            killed_pids: vec![],
            failed: vec![KillFailure {
                pid,
                reason: stderr.trim().to_string(),
            }],
        })
    }
}

#[cfg(not(target_os = "windows"))]
async fn kill_pid_impl(pid: u32) -> Result<KillResult, String> {
    let output = Command::new("kill")
        .args(&["-9", &pid.to_string()])
        .output()
        .map_err(|e| format!("kill 执行失败: {}", e))?;

    if output.status.success() {
        Ok(KillResult {
            killed_pids: vec![pid],
            failed: vec![],
        })
    } else {
        Ok(KillResult {
            killed_pids: vec![],
            failed: vec![KillFailure {
                pid,
                reason: "kill -9 failed".to_string(),
            }],
        })
    }
}

// ============================================================================
// 工具函数
// ============================================================================

/// 截断命令行到指定长度（带省略号）
fn truncate_command(cmd: &str, max_len: usize) -> String {
    if cmd.chars().count() <= max_len {
        cmd.to_string()
    } else {
        let truncated: String = cmd.chars().take(max_len).collect();
        format!("{}…", truncated)
    }
}
