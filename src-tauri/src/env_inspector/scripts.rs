//! Python 探查脚本与子进程执行
//!
//! 详见 `PR/03-模块设计/07-EnvironmentInspector.md §4.1 探查脚本` 与 `§9.3 子进程超时`

use std::path::Path;
use std::time::Duration;

use crate::error::EnvError;

/// 子进程超时（秒）
const SUBPROCESS_TIMEOUT_SECS: u64 = 10;

/// 探查 torch 的 Python 脚本
///
/// 输出 JSON：`{"torch": {...}, "platform": {...}}`
const PROBE_TORCH_SCRIPT: &str = r#"
import sys, json, platform
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
except ImportError:
    torch_info = {"installed": False}
result = {
    "torch": torch_info,
    "platform": {"system": platform.system(), "release": platform.release()},
}
print(json.dumps(result))
"#;

/// 列出已安装包的命令（pip list --format=json）
pub const PIP_LIST_ARGS: &[&str] = &["-m", "pip", "list", "--format=json"];

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
/// - 超时 10 秒，超时杀进程
/// - 失败时返回 EnvError::PythonInstallFailed 或 EnvError::VerifyFailed
pub async fn run_python_script(venv_path: &Path, script: &str) -> Result<String, EnvError> {
    let python = venv_python_path(venv_path);
    if !python.exists() {
        return Err(EnvError::VerifyFailed(format!(
            "python not found at {}",
            python.display()
        )));
    }

    let output = tokio::time::timeout(
        Duration::from_secs(SUBPROCESS_TIMEOUT_SECS),
        tokio::process::Command::new(&python).args(["-c", script]).output(),
    )
    .await
    .map_err(|_| {
        tracing::error!(timeout = SUBPROCESS_TIMEOUT_SECS, "python subprocess timeout");
        EnvError::VerifyFailed(format!("python subprocess timeout ({}s)", SUBPROCESS_TIMEOUT_SECS))
    })?
    .map_err(|e| {
        tracing::error!(error = %e, "python subprocess spawn failed");
        EnvError::VerifyFailed(e.to_string())
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::error!(stderr = %stderr, "python script exited with error");
        return Err(EnvError::VerifyFailed(stderr.into_owned()));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// 运行 `python -m pip list --format=json`，返回 stdout JSON
pub async fn run_pip_list(venv_path: &Path) -> Result<String, EnvError> {
    let python = venv_python_path(venv_path);
    if !python.exists() {
        return Err(EnvError::VerifyFailed(format!(
            "python not found at {}",
            python.display()
        )));
    }

    let output = tokio::time::timeout(
        Duration::from_secs(SUBPROCESS_TIMEOUT_SECS),
        tokio::process::Command::new(&python).args(PIP_LIST_ARGS).output(),
    )
    .await
    .map_err(|_| {
        EnvError::VerifyFailed(format!("pip list timeout ({}s)", SUBPROCESS_TIMEOUT_SECS))
    })?
    .map_err(|e| EnvError::VerifyFailed(e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(EnvError::VerifyFailed(stderr.into_owned()));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// 探查 torch（运行嵌入脚本，返回 stdout JSON）
pub async fn probe_torch_script(venv_path: &Path) -> Result<String, EnvError> {
    run_python_script(venv_path, PROBE_TORCH_SCRIPT).await
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
}
