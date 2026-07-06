//! Python 探查脚本与子进程执行
//!
//! 详见 `PR/03-模块设计/07-EnvironmentInspector.md §4.1 探查脚本` 与 `§9.3 子进程超时`

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::error::EnvError;

/// 子进程默认超时（秒）
///
/// v2.10：从 10s → 30s。
/// 原因：Windows 首次 `python -m pip list` 在 venv 刚创建（含 `--seed` 安装的
/// pip/setuptools/wheel）后，受 Defender 实时扫描 + 字节码编译影响，耗时可达 8-15s，
/// 10s 超时处于边缘导致 onboarding 第 4 步「创建虚拟环境」误报失败。
/// 30s 提供足够冗余，配合 uv pip list 主路径（启动 < 100ms）几乎不会触发。
const SUBPROCESS_TIMEOUT_SECS: u64 = 30;

/// `import torch` 探查超时（秒）
///
/// v2.11：单独为 `probe_torch_script` 设置更长超时。
/// 原因：torch 是 2GB+ 大库，首次 `import torch` 在 Windows 上受以下因素影响：
/// - 加载 torch/_C.pyd（C++ 扩展，2GB+）
/// - Windows Defender 实时扫描每个 .dll / .pyd
/// - 字节码编译 .pyc
/// - CUDA 初始化
/// 实测首次 import 可达 30-60s，30s 超时处于边缘导致 verify_venv 误报失败。
/// 90s 提供足够冗余，后续 import 会快很多（缓存命中后 3-5s）。
const PROBE_TORCH_TIMEOUT_SECS: u64 = 90;

/// 探查 torch 的 Python 脚本
///
/// 输出 JSON：`{"torch": {...}, "platform": {...}}`
///
/// **v1.8 关键修复（torch "已装但显示未装" 问题）**：
/// 之前的 `except ImportError` 会默默吞掉所有 `import torch` 失败的真实原因
/// （如 numpy 2.4.4 wheel 缺 `exceptions.py` 导致 `import torch` 抛 ImportError，
/// 或 torch C 扩展加载失败抛 RuntimeError），前端只看到 `installed: false`，
/// 用户不知道为什么。
///
/// 现在：
/// 1. catch 所有 Exception（不再只 catch ImportError）
/// 2. 把错误类型 / 消息 / traceback 一并写入 JSON
/// 3. 前端 StatusCard 看到 `_error_type` 时直接显示原文
///
/// JSON 结构：
/// - 成功：`{"torch": {"installed": true, "version": ..., "cuda_available": ...}}`
/// - 失败：`{"torch": {"installed": false, "_error_type": "ImportError",
///                    "_error_msg": "...", "_traceback": "..."}}`
///
/// ⚠ 下划线开头的字段前端 JSON 反序列化时会被 TS 类型忽略（TS 类型只有
/// installed / version / cuda_available 等），但通过 raw JSON 透传给 StatusCard
/// 的诊断面板（见 frontend/src/components/launch/StatusCard.vue）。
const PROBE_TORCH_SCRIPT: &str = r#"
import sys, json, platform, traceback
try:
    import torch
    torch_info = {
        "installed": True,
        "version": torch.__version__,
        "cuda_available": torch.cuda.is_available(),
        "cuda_version": str(torch.version.cuda) if torch.version.cuda else None,
        "device_count": torch.cuda.device_count() if torch.cuda.is_available() else 0,
        "device_name": torch.cuda.get_device_name(0) if torch.cuda.is_available() else None,
        "total_memory_mb": (torch.cuda.get_device_properties(0).total_memory // (1024*1024))
                          if torch.cuda.is_available() else None,
    }
except Exception as e:
    # 关键：捕获所有异常（不再只 catch ImportError），暴露真实失败原因
    # 典型场景：
    # - ImportError: numpy 2.4.4 wheel 缺 exceptions.py → import torch 失败
    # - RuntimeError: torch C 扩展加载失败（CUDA driver 不匹配等）
    # - OSError: torch 共享库加载失败
    torch_info = {
        "installed": False,
        "_error_type": type(e).__name__,
        "_error_msg": str(e)[:500],  # 截断防止 traceback 过长
        "_traceback": traceback.format_exc()[-500:],  # 取最后 500 字符（最有信息量的栈底）
    }
result = {
    "torch": torch_info,
    "platform": {"system": platform.system(), "release": platform.release()},
}
print(json.dumps(result))
"#;

/// 列出已安装包的命令（pip list --format=json）
///
/// Fallback 路径用：当 uv binary 不可用或 uv pip list 失败时使用。
pub const PIP_LIST_ARGS: &[&str] = &["-m", "pip", "list", "--format=json"];

/// 构造 `uv pip list` 命令参数
///
/// 命令：`uv pip list --python <venv_python> --format=json`
///
/// - `--python <venv_python>`：指定 venv 中的 python 二进制，让 uv 知道要列哪个 venv
/// - `--format=json`：与 pip 完全兼容的 JSON 输出格式
///
/// 性能优势：uv 是 Rust 实现，启动 < 100ms（vs `python -m pip` 3-5s）
pub fn uv_pip_list_args(venv_python: &Path) -> Vec<String> {
    vec![
        "pip".to_string(),
        "list".to_string(),
        "--python".to_string(),
        venv_python.to_string_lossy().into_owned(),
        "--format=json".to_string(),
    ]
}

/// venv 中的 python 二进制文件名（跨平台）
fn python_binary_name() -> &'static str {
    if cfg!(windows) {
        "python.exe"
    } else {
        "python"
    }
}

/// 获取 venv 中 python 可执行文件路径
pub fn venv_python_path(venv_path: &Path) -> std::path::PathBuf {
    // Windows: <venv>/Scripts/python.exe
    // Unix:    <venv>/bin/python
    let subdir = if cfg!(windows) { "Scripts" } else { "bin" };
    venv_path.join(subdir).join(python_binary_name())
}

/// 运行 Python 探查脚本，返回 stdout
///
/// **v2.11 关键修复**：
/// - 使用 `kill_on_drop(true)`：超时 drop Future 时自动杀死子进程
///   原因：tokio 默认 `kill_on_drop = false`，超时后 python.exe 仍残留
///   持有 venv 文件锁 → uv venv 删除目录报"拒绝访问 (os error 5)"
/// - `timeout_secs` 参数化：probe_torch 用 90s，其他用 30s
///
/// 失败时返回 EnvError::VerifyFailed
pub async fn run_python_script(
    venv_path: &Path,
    script: &str,
    timeout_secs: u64,
) -> Result<String, EnvError> {
    let python = venv_python_path(venv_path);
    if !python.exists() {
        return Err(EnvError::VerifyFailed(format!(
            "python not found at {}",
            python.display()
        )));
    }

    // kill_on_drop(true)：超时 drop 时自动杀子进程，避免残留 python.exe 持有文件锁
    let child = tokio::process::Command::new(&python)
        .args(["-c", script])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            tracing::error!(error = %e, "python subprocess spawn failed");
            EnvError::VerifyFailed(e.to_string())
        })?;

    match tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        child.wait_with_output(),
    )
    .await
    {
        Ok(Ok(output)) => {
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::error!(stderr = %stderr, "python script exited with error");
                return Err(EnvError::VerifyFailed(stderr.into_owned()));
            }
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        }
        Ok(Err(e)) => {
            tracing::error!(error = %e, "python subprocess wait failed");
            Err(EnvError::VerifyFailed(e.to_string()))
        }
        Err(_) => {
            // 超时：child 在 drop 时被 kill_on_drop 自动杀死
            tracing::error!(timeout = timeout_secs, "python subprocess timeout");
            Err(EnvError::VerifyFailed(format!(
                "python subprocess timeout ({}s)",
                timeout_secs
            )))
        }
    }
}

/// 运行 `pip list`，返回 stdout JSON
///
/// **v2.10 起采用双路径策略**：
/// 1. **主路径**：`uv pip list --python <venv_python> --format=json`
///    - uv 是 Rust 实现，启动 < 100ms（vs `python -m pip` 3-5s）
///    - 根本解决 Windows 首次 pip list 超时问题
///    - 输出格式与 pip 完全兼容（`parse_pip_list` 无需修改）
/// 2. **Fallback**：`python -m pip list --format=json`
///    - uv binary 不存在 / uv 调用失败 / uv 超时 → 自动回退
///    - 保证 venv 中即使无 uv 也能正常探查
///
/// **v2.11 关键修复**：两路径均加 `kill_on_drop(true)`，超时后自动杀子进程
///
/// 两路径均受 `SUBPROCESS_TIMEOUT_SECS`（30s）保护。
pub async fn run_pip_list(
    venv_path: &Path,
    uv_binary: Option<&Path>,
) -> Result<String, EnvError> {
    let python = venv_python_path(venv_path);
    if !python.exists() {
        return Err(EnvError::VerifyFailed(format!(
            "python not found at {}",
            python.display()
        )));
    }

    // ========== 主路径：uv pip list ==========
    if let Some(uv) = uv_binary {
        if uv.exists() {
            let args = uv_pip_list_args(&python);
            tracing::debug!(
                ?uv, ?args, timeout = SUBPROCESS_TIMEOUT_SECS,
                "run_pip_list: trying uv pip list (primary)"
            );
            // kill_on_drop(true)：超时 drop 时自动杀子进程
            let child = match tokio::process::Command::new(uv)
                .args(&args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(
                        error = %e, "uv pip list spawn failed, fallback to python -m pip"
                    );
                    return run_pip_list_fallback(&python).await;
                }
            };

            match tokio::time::timeout(
                Duration::from_secs(SUBPROCESS_TIMEOUT_SECS),
                child.wait_with_output(),
            )
            .await
            {
                Ok(Ok(output)) if output.status.success() => {
                    tracing::debug!("run_pip_list: uv pip list succeeded");
                    return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
                }
                Ok(Ok(output)) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    tracing::warn!(
                        ?stderr, "uv pip list exited non-zero, fallback to python -m pip"
                    );
                }
                Ok(Err(e)) => {
                    tracing::warn!(
                        error = %e, "uv pip list wait failed, fallback to python -m pip"
                    );
                }
                Err(_) => {
                    // 超时：child 已被 kill_on_drop 自动杀死
                    tracing::warn!(
                        timeout = SUBPROCESS_TIMEOUT_SECS,
                        "uv pip list timeout, fallback to python -m pip"
                    );
                }
            }
        } else {
            tracing::warn!(?uv, "uv binary not found, fallback to python -m pip");
        }
    } else {
        tracing::debug!("run_pip_list: uv_binary is None, using python -m pip directly");
    }

    // ========== Fallback：python -m pip list ==========
    run_pip_list_fallback(&python).await
}

/// Fallback 路径：`python -m pip list --format=json`
///
/// v2.11：抽取出独立函数，加 `kill_on_drop(true)`
async fn run_pip_list_fallback(python: &Path) -> Result<String, EnvError> {
    tracing::debug!(
        ?python, ?PIP_LIST_ARGS, timeout = SUBPROCESS_TIMEOUT_SECS,
        "run_pip_list_fallback: trying python -m pip list"
    );
    // kill_on_drop(true)：超时 drop 时自动杀子进程
    let child = tokio::process::Command::new(python)
        .args(PIP_LIST_ARGS)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| EnvError::VerifyFailed(e.to_string()))?;

    match tokio::time::timeout(
        Duration::from_secs(SUBPROCESS_TIMEOUT_SECS),
        child.wait_with_output(),
    )
    .await
    {
        Ok(Ok(output)) => {
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(EnvError::VerifyFailed(stderr.into_owned()));
            }
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        }
        Ok(Err(e)) => Err(EnvError::VerifyFailed(e.to_string())),
        Err(_) => {
            // 超时：child 已被 kill_on_drop 自动杀死
            tracing::error!(timeout = SUBPROCESS_TIMEOUT_SECS, "python -m pip list timeout");
            Err(EnvError::VerifyFailed(format!(
                "pip list timeout ({}s)",
                SUBPROCESS_TIMEOUT_SECS
            )))
        }
    }
}

/// 探查 torch（运行嵌入脚本，返回 stdout JSON）
///
/// v2.11：使用 `PROBE_TORCH_TIMEOUT_SECS`（90s）而非默认 30s
pub async fn probe_torch_script(venv_path: &Path) -> Result<String, EnvError> {
    run_python_script(venv_path, PROBE_TORCH_SCRIPT, PROBE_TORCH_TIMEOUT_SECS).await
}

/// v1.8：torch 探针原始结果（带错误详情）
///
/// **关键**：区分 "torch 真的没装"（installed=false 且无 _error_type）
/// 和 "torch 装了但 import 失败"（installed=false 且有 _error_type）。
/// 前端应给后者提供"诊断"按钮（RecoveryWizard 入口）。
///
/// 字段说明：
/// - `installed`：是否能成功 import torch
/// - `version` / `cuda_available` 等：仅在 installed=true 时有值
/// - `error_type` / `error_msg` / `traceback`：仅在 installed=false 且 import 抛异常时有值
#[derive(Debug, Clone, Serialize)]
pub struct TorchProbeResult {
    pub installed: bool,
    pub version: Option<String>,
    pub cuda_available: bool,
    pub cuda_version: Option<String>,
    pub device_name: Option<String>,
    /// 错误类型（如 "ImportError" / "RuntimeError"）
    pub error_type: Option<String>,
    /// 错误消息（截断到 500 字符）
    pub error_msg: Option<String>,
    /// traceback 末尾（截断到 500 字符）
    pub traceback_tail: Option<String>,
}

/// 解析 torch 探针 JSON 输出
///
/// 不会失败（解析失败时返回 installed=false，所有可选字段 None）
pub fn parse_torch_probe(json_output: &str) -> TorchProbeResult {
    let mut result = TorchProbeResult {
        installed: false,
        version: None,
        cuda_available: false,
        cuda_version: None,
        device_name: None,
        error_type: None,
        error_msg: None,
        traceback_tail: None,
    };
    let parsed: Value = match serde_json::from_str(json_output) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "torch probe json parse failed");
            // 解析失败 → 把原文存到 error_msg（极少见，但能帮用户看到 "JSON 烂了"）
            result.error_type = Some("JsonParseError".to_string());
            result.error_msg = Some(json_output.chars().take(200).collect());
            return result;
        }
    };
    let torch = parsed.get("torch");
    if let Some(t) = torch {
        if t.get("installed").and_then(|v| v.as_bool()) == Some(true) {
            result.installed = true;
            result.version = t.get("version").and_then(|v| v.as_str()).map(String::from);
            result.cuda_available = t.get("cuda_available").and_then(|v| v.as_bool()).unwrap_or(false);
            result.cuda_version = t.get("cuda_version").and_then(|v| v.as_str()).map(String::from);
            result.device_name = t.get("device_name").and_then(|v| v.as_str()).map(String::from);
        } else {
            // installed=false → 读错误详情（如果有）
            result.error_type = t.get("_error_type").and_then(|v| v.as_str()).map(String::from);
            result.error_msg = t.get("_error_msg").and_then(|v| v.as_str()).map(String::from);
            result.traceback_tail = t.get("_traceback").and_then(|v| v.as_str()).map(String::from);
        }
    }
    result
}

/// v1.8 / F36：快速读 torch variant（cu118/cu121/cu124/cpu）
///
/// 用于版本切换兼容性预检。直接解析 `version.py` 而不跑 python（避免 90s 探查超时）。
/// 返回 None 表示未安装 torch。
pub fn read_torch_variant_fast(venv_path: &Path) -> Option<String> {
    // 候选路径
    let candidates = [
        venv_path.join("Lib/site-packages/torch/version.py"),  // Windows
        venv_path.join("lib/python3.11/site-packages/torch/version.py"),  // Linux
        venv_path.join("lib/python3.10/site-packages/torch/version.py"),
        venv_path.join("lib/python3.12/site-packages/torch/version.py"),
    ];
    let content = candidates
        .iter()
        .filter(|p| p.exists())
        .next()
        .and_then(|p| std::fs::read_to_string(p).ok())?;
    // 找 __version__ = '2.4.0+cu121' 行
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("__version__") {
            if let Some(eq_pos) = trimmed.find('=') {
                let val = trimmed[eq_pos + 1..].trim().trim_matches(|c| c == '\'' || c == '"');
                // val 形如 "2.4.0+cu121" 或 "2.4.0"
                if let Some(plus_pos) = val.find('+') {
                    return Some(val[plus_pos + 1..].to_string());
                }
                return Some("cpu".to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_venv_python_path_windows_layout() {
        // 验证路径拼接（不依赖真实文件存在）
        let venv = Path::new("/tmp/venv");
        let py = venv_python_path(venv);
        if cfg!(windows) {
            assert!(py.to_string_lossy().contains("Scripts"));
            assert!(py.to_string_lossy().ends_with("python.exe"));
        } else {
            assert!(py.to_string_lossy().contains("bin"));
            assert!(py.to_string_lossy().ends_with("python"));
        }
    }

    #[test]
    fn test_uv_pip_list_args_contains_required_flags() {
        // 验证 uv pip list 命令参数包含 pip / list / --python / --format=json
        let venv = Path::new("/tmp/venv");
        let py = venv_python_path(venv);
        let args = uv_pip_list_args(&py);

        assert!(args.iter().any(|a| a == "pip"));
        assert!(args.iter().any(|a| a == "list"));
        assert!(args.iter().any(|a| a == "--python"));
        assert!(args.iter().any(|a| a == "--format=json"));
        // --python 后必须紧跟 venv python 路径
        let python_idx = args.iter().position(|a| a == "--python").unwrap();
        assert_eq!(args[python_idx + 1], py.to_string_lossy());
    }

    #[test]
    fn test_pip_list_args_constant_unchanged() {
        // Fallback 路径常量保持不变
        assert_eq!(PIP_LIST_ARGS, &["-m", "pip", "list", "--format=json"]);
    }

    #[test]
    fn test_probe_torch_timeout_greater_than_default() {
        // v2.11：probe_torch 超时（90s）必须大于默认子进程超时（30s）
        assert!(PROBE_TORCH_TIMEOUT_SECS > SUBPROCESS_TIMEOUT_SECS);
        assert_eq!(PROBE_TORCH_TIMEOUT_SECS, 90);
        assert_eq!(SUBPROCESS_TIMEOUT_SECS, 30);
    }
}
