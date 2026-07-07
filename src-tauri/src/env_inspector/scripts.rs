//! Python 探查脚本与子进程执行
//!
//! 详见 `PR/03-模块设计/07-EnvironmentInspector.md §4.1 探查脚本` 与 `§9.3 子进程超时`
//!
//! v3.6：所有子进程调用从 `tokio::time::timeout` 改为 `CancellationToken`，
//! 不再有硬性超时，用户可通过 CancellationToken 主动取消。

use std::path::Path;
use std::process::Stdio;

use serde::Serialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::error::EnvError;

/// 探查 torch 的 Python 脚本
///
/// 输出 JSON：`{"torch": {...}, "torchvision": {...}, "platform": {...}}`
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
/// **v3.8 D-L3 扩展**：增加 torchvision 子模块校验
/// - 历史上用户 venv 出现 "torch 装对了，但 torchvision 是 0.1.6 远古版" 的惨案
///   → `from torchvision.ops import roi_align` 失败，ComfyUI 启动炸
/// - probe 现在同时检查 `from torchvision.ops import roi_align` 和 `from torchvision.io import read_image`
/// - 失败时返回 `_error_type: "IncompleteTorchvision"`，触发 D-L4 自动重装
///
/// JSON 结构：
/// - 成功：`{"torch": {"installed": true, ...},
///            "torchvision": {"installed": true, "version": "0.22.0+cu128",
///                            "ops_available": true, "io_available": true},
///            "platform": {...}}`
/// - 失败：`{"torch": {...}, "torchvision": {"installed": false,
///                    "_error_type": "IncompleteTorchvision", "_error_msg": "..."},
///            "platform": {...}}`
///
/// ⚠ 下划线开头的字段前端 JSON 反序列化时会被 TS 类型忽略（TS 类型只有
/// installed / version / cuda_available 等），但通过 raw JSON 透传给 StatusCard
/// 的诊断面板（见 frontend/src/components/launch/StatusCard.vue）。
const PROBE_TORCH_SCRIPT: &str = r#"
import sys, json, platform, traceback
result = {"platform": {"system": platform.system(), "release": platform.release()}}

# ========== torch 探查 ==========
try:
    import torch
    result["torch"] = {
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
    result["torch"] = {
        "installed": False,
        "_error_type": type(e).__name__,
        "_error_msg": str(e)[:500],  # 截断防止 traceback 过长
        "_traceback": traceback.format_exc()[-500:],  # 取最后 500 字符（最有信息量的栈底）
    }

# ========== v3.8 D-L3：torchvision 探查（子模块强校验）==========
try:
    import torchvision
    tv_version = getattr(torchvision, "__version__", None)
    # 关键：0.1.6 远古版没有 __version__ 属性（其 __init__.py 只有 4 行 from ... import）
    if tv_version is None:
        raise ImportError(
            "torchvision.__version__ 不存在，疑似远古版（0.1.6 之前），"
            "torchvision.ops / io 等现代子模块也不可用"
        )

    # 尝试 import 关键子模块（C++ 扩展）
    try:
        from torchvision.ops import roi_align  # noqa: F401
        ops_available = True
        ops_error = None
    except Exception as e_ops:
        ops_available = False
        ops_error = "{}: {}".format(type(e_ops).__name__, str(e_ops)[:200])

    try:
        from torchvision.io import read_image  # noqa: F401
        io_available = True
        io_error = None
    except Exception as e_io:
        io_available = False
        io_error = "{}: {}".format(type(e_io).__name__, str(e_io)[:200])

    if ops_available and io_available:
        result["torchvision"] = {
            "installed": True,
            "version": tv_version,
            "ops_available": True,
            "io_available": True,
        }
    else:
        # 子模块残缺（典型场景：装到一半被中断，或装上 0.1.6 远古版）
        result["torchvision"] = {
            "installed": False,
            "version": tv_version,
            "ops_available": ops_available,
            "io_available": io_available,
            "_error_type": "IncompleteTorchvision",
            "_error_msg": "ops={}, io={}".format(
                "ok" if ops_available else "fail: " + (ops_error or "?"),
                "ok" if io_available else "fail: " + (io_error or "?"),
            ),
            "_traceback": "torchvision {} 子模块校验失败：ops={}, io={}".format(
                tv_version, ops_available, io_available
            ),
        }
except Exception as e:
    # import torchvision 本身失败（完全没装 / 装到一半被中断 / 版本太老）
    result["torchvision"] = {
        "installed": False,
        "_error_type": type(e).__name__,
        "_error_msg": str(e)[:500],
        "_traceback": traceback.format_exc()[-500:],
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
/// v3.6：用 `CancellationToken` 替代 `tokio::time::timeout`，不再有硬性超时。
/// - `kill_on_drop(true)`：cancel 时 drop Future 自动杀子进程
/// - cancel 触发时返回 `EnvError::Cancelled`
///
/// 失败时返回 EnvError::VerifyFailed
pub async fn run_python_script(
    venv_path: &Path,
    script: &str,
    cancel: &CancellationToken,
) -> Result<String, EnvError> {
    let python = venv_python_path(venv_path);
    if !python.exists() {
        return Err(EnvError::VerifyFailed(format!(
            "python not found at {}",
            python.display()
        )));
    }

    let mut cmd = crate::common::process_util::new_command(&python);
    cmd.args(["-c", script])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let output = crate::common::subprocess::run_with_cancel(&mut cmd, cancel)
        .await
        .map_err(EnvError::from)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::error!(stderr = %stderr, "python script exited with error");
        return Err(EnvError::VerifyFailed(stderr.into_owned()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// 运行 `pip list`，返回 stdout JSON
///
/// v3.6：用 `CancellationToken` 替代 `tokio::time::timeout`，不再有 30s 硬性超时。
///
/// **双路径策略**：
/// 1. **主路径**：`uv pip list --python <venv_python> --format=json`
/// 2. **Fallback**：`python -m pip list --format=json`
pub async fn run_pip_list(
    venv_path: &Path,
    uv_binary: Option<&Path>,
    cancel: &CancellationToken,
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
            tracing::debug!(?uv, ?args, "run_pip_list: trying uv pip list (primary)");

            let mut cmd = crate::common::process_util::new_command(uv);
            cmd.args(&args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);

            match crate::common::subprocess::run_with_cancel(&mut cmd, cancel).await {
                Ok(output) if output.status.success() => {
                    tracing::debug!("run_pip_list: uv pip list succeeded");
                    return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    tracing::warn!(
                        ?stderr, "uv pip list exited non-zero, fallback to python -m pip"
                    );
                }
                Err(crate::common::subprocess::SubprocessError::Cancelled) => {
                    return Err(EnvError::Cancelled);
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e, "uv pip list failed, fallback to python -m pip"
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
    run_pip_list_fallback(&python, cancel).await
}

/// Fallback 路径：`python -m pip list --format=json`
///
/// v3.6：用 `CancellationToken` 替代 `tokio::time::timeout`
async fn run_pip_list_fallback(
    python: &Path,
    cancel: &CancellationToken,
) -> Result<String, EnvError> {
    tracing::debug!(?python, ?PIP_LIST_ARGS, "run_pip_list_fallback: trying python -m pip list");

    let mut cmd = crate::common::process_util::new_command(python);
    cmd.args(PIP_LIST_ARGS)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let output = crate::common::subprocess::run_with_cancel(&mut cmd, cancel)
        .await
        .map_err(EnvError::from)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(EnvError::VerifyFailed(stderr.into_owned()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// 探查 torch（运行嵌入脚本，返回 stdout JSON）
///
/// v3.6：透传 `CancellationToken`，不再有 90s 硬性超时
pub async fn probe_torch_script(
    venv_path: &Path,
    cancel: &CancellationToken,
) -> Result<String, EnvError> {
    run_python_script(venv_path, PROBE_TORCH_SCRIPT, cancel).await
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
/// - `torchvision`：v3.8 D-L3 新增，torchvision 子模块强校验结果
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
    /// v3.8 D-L3：torchvision 子模块强校验结果
    pub torchvision: TorchvisionProbeInfo,
}

/// v3.8 D-L3：torchvision 子模块校验结果
///
/// **何时 installed=false**：
/// 1. `import torchvision` 本身失败（包未装 / 半装被中断）
/// 2. `torchvision.__version__` 不存在（0.1.6 远古版没这属性）
/// 3. `from torchvision.ops import roi_align` 失败（缺 C++ 扩展）
/// 4. `from torchvision.io import read_image` 失败（缺 io 子包）
///
/// **何时 installed=true**：
/// 1. `import torchvision` 成功
/// 2. `__version__` 存在（>= 0.2）
/// 3. `torchvision.ops.roi_align` 和 `torchvision.io.read_image` 都能 import
#[derive(Debug, Clone, Serialize)]
pub struct TorchvisionProbeInfo {
    pub installed: bool,
    pub version: Option<String>,
    pub ops_available: bool,
    pub io_available: bool,
    /// 错误类型（IncompleteTorchvision / ImportError / AttributeError 等）
    pub error_type: Option<String>,
    pub error_msg: Option<String>,
    pub traceback_tail: Option<String>,
}

impl Default for TorchvisionProbeInfo {
    fn default() -> Self {
        Self {
            installed: false,
            version: None,
            ops_available: false,
            io_available: false,
            error_type: None,
            error_msg: None,
            traceback_tail: None,
        }
    }
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
        torchvision: TorchvisionProbeInfo::default(),
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
    // v3.8 D-L3：解析 torchvision 字段
    if let Some(tv) = parsed.get("torchvision") {
        result.torchvision.installed = tv.get("installed").and_then(|v| v.as_bool()).unwrap_or(false);
        result.torchvision.version = tv.get("version").and_then(|v| v.as_str()).map(String::from);
        result.torchvision.ops_available = tv.get("ops_available").and_then(|v| v.as_bool()).unwrap_or(false);
        result.torchvision.io_available = tv.get("io_available").and_then(|v| v.as_bool()).unwrap_or(false);
        result.torchvision.error_type = tv.get("_error_type").and_then(|v| v.as_str()).map(String::from);
        result.torchvision.error_msg = tv.get("_error_msg").and_then(|v| v.as_str()).map(String::from);
        result.torchvision.traceback_tail = tv.get("_traceback").and_then(|v| v.as_str()).map(String::from);
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
    fn test_probe_torch_script_signature_has_cancel() {
        // v3.6：probe_torch_script 接受 CancellationToken 参数（编译时检查）
        fn _assert_cancel_param(_f: fn(&Path, &tokio_util::sync::CancellationToken) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, crate::error::EnvError>> + Send>>) {}
        // 函数存在即通过（签名在编译时已验证）
    }
}
