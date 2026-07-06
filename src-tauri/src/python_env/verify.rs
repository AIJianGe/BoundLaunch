//! venv 校验
//!
//! 详见 `PR/03-模块设计/02-PythonEnvManager.md §1 模块职责` 中 verify_venv
//!
//! v3.6：所有子进程调用用 `CancellationToken` 替代 `tokio::time::timeout`

use std::path::Path;
use std::process::Stdio;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::env_inspector::scripts::{probe_torch_script, venv_python_path};
use crate::error::EnvError;

use super::models::EnvInfo;

/// 探查 venv 中 Python 版本（v2.13）
///
/// 轻量探测：`python -c "import sys; print(sys.version.split()[0])"`
/// v3.6：用 `CancellationToken` 替代 5s timeout
///
/// 返回 `Option<String>`：
/// - `Some("3.11.10")` 成功
/// - `None` 失败（python 不存在 / 取消 / 解析失败）
pub async fn probe_python_version(
    python: &Path,
    cancel: &CancellationToken,
) -> Option<String> {
    let script = "import sys; print(sys.version.split()[0])";
    let mut cmd = crate::common::process_util::new_command(python);
    cmd.args(["-c", script])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let output = crate::common::subprocess::run_with_cancel(&mut cmd, cancel)
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let v = stdout.trim().to_string();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

/// 校验 venv 完整性
///
/// - python 二进制存在
/// - torch 可导入
/// - 返回 EnvInfo（含 torch 版本与 CUDA 能力）
///
/// v3.6：透传 `CancellationToken` 给 `probe_torch_script`
pub async fn verify_venv(
    venv_path: &Path,
    cancel: &CancellationToken,
) -> Result<EnvInfo, EnvError> {
    let python_path = venv_python_path(venv_path);

    if !python_path.exists() {
        return Err(EnvError::VerifyFailed(format!(
            "python binary not found at {}",
            python_path.display()
        )));
    }

    // 探查 torch
    let stdout = probe_torch_script(venv_path, cancel).await?;
    let parsed: Value = serde_json::from_str(&stdout)
        .map_err(|e| EnvError::VerifyFailed(format!("torch script output not JSON: {}", e)))?;

    let torch_obj = parsed
        .get("torch")
        .ok_or_else(|| EnvError::VerifyFailed("missing 'torch' field".to_string()))?;

    let torch_installed = torch_obj
        .get("installed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let torch_version = torch_obj
        .get("version")
        .and_then(|v| v.as_str())
        .map(String::from);

    let cuda_available = torch_obj
        .get("cuda_available")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // 提取 Python 版本（来自 platform 子对象，简化处理）
    let python_version = parsed
        .get("platform")
        .and_then(|p| p.get("release"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    Ok(EnvInfo {
        python_version,
        python_path,
        venv_path: venv_path.to_path_buf(),
        torch_installed,
        torch_version,
        cuda_available,
    })
}

/// 快速判断 venv 是否就绪（不解析 torch 完整信息）
///
/// v3.6：透传 `CancellationToken`
pub async fn is_venv_ready(
    venv_path: &Path,
    cancel: &CancellationToken,
) -> bool {
    if !venv_path.exists() {
        return false;
    }
    let python = venv_python_path(venv_path);
    if !python.exists() {
        return false;
    }
    // 尝试 import torch（失败不算 venv 不可用，只是 torch 未装）
    match verify_venv(venv_path, cancel).await {
        Ok(info) => info.torch_installed,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_verify_venv_nonexistent_returns_err() {
        let cancel = CancellationToken::new();
        let result = verify_venv(Path::new("/nonexistent/venv"), &cancel).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_is_venv_ready_nonexistent_returns_false() {
        let cancel = CancellationToken::new();
        assert!(!is_venv_ready(Path::new("/nonexistent/venv"), &cancel).await);
    }
}
