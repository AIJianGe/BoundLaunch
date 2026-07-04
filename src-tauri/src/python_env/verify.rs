//! venv 校验
//!
//! 详见 `PR/03-模块设计/02-PythonEnvManager.md §1 模块职责` 中 verify_venv

use std::path::Path;

use serde_json::Value;

use crate::env_inspector::scripts::{probe_torch_script, venv_python_path};
use crate::error::EnvError;

use super::models::EnvInfo;

/// 校验 venv 完整性
///
/// - python 二进制存在
/// - torch 可导入
/// - 返回 EnvInfo（含 torch 版本与 CUDA 能力）
pub async fn verify_venv(venv_path: &Path) -> Result<EnvInfo, EnvError> {
    let python_path = venv_python_path(venv_path);

    if !python_path.exists() {
        return Err(EnvError::VerifyFailed(format!(
            "python binary not found at {}",
            python_path.display()
        )));
    }

    // 探查 torch
    let stdout = probe_torch_script(venv_path).await?;
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
pub async fn is_venv_ready(venv_path: &Path) -> bool {
    if !venv_path.exists() {
        return false;
    }
    let python = venv_python_path(venv_path);
    if !python.exists() {
        return false;
    }
    // 尝试 import torch（失败不算 venv 不可用，只是 torch 未装）
    match verify_venv(venv_path).await {
        Ok(info) => info.torch_installed,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_verify_venv_nonexistent_returns_err() {
        let result = verify_venv(Path::new("/nonexistent/venv")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_is_venv_ready_nonexistent_returns_false() {
        assert!(!is_venv_ready(Path::new("/nonexistent/venv")).await);
    }
}
