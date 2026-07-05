//! PythonEnvManager Tauri commands（门面层）
//!
//! 设计模式：**门面 (Facade)** - 前端只与本层交互，不直接调 PythonEnvService
//!
//! 详见 `PR/03-模块设计/02-PythonEnvManager.md §3 接口签名` 末尾的 `#[tauri::command]` 定义

use std::path::PathBuf;

use tauri::{AppHandle, Emitter, State};

use crate::app_state::AppState;
use crate::config::CudaVersion;
use crate::python_env::models::{CompatibilityReport, PythonEnvStatus};

/// 查询当前 venv 状态（v2.13）
///
/// 返回前端 `PythonEnvStatus` 接口对应的完整结构：
/// uv 状态（uv_installed / uv_path / uv_version）+ venv 状态（venv_exists /
/// venv_python_version / venv_torch_installed / venv_torch_version /
/// venv_torch_cuda）。
///
/// 之前返回 `EnvInfo` 时（v2.10 之前），前端组件 `PythonVersionPanel.vue`
/// 读 `envStore.pythonEnvStatus?.venv_python_version` 永远为 `undefined`，
/// 因为 `EnvInfo` 不含 `venv_python_version` 字段 → 显示「未配置」。
///
/// 所有探测都是只读（不修改 venv），最坏情况 5-30s（verify_venv 的 probe_torch 90s 超时）。
#[tauri::command]
pub async fn env_status(state: State<'_, AppState>) -> Result<PythonEnvStatus, String> {
    let config = state.config.get();
    let venv_path = PathBuf::from(&config.paths.venv_path);

    Ok(state
        .python_env
        .get_status(&venv_path)
        .await)
}

/// 检查 uv 是否可用
#[tauri::command]
pub async fn env_uv_available(state: State<'_, AppState>) -> Result<bool, String> {
    Ok(state.python_env.is_uv_available().await)
}

/// 创建 venv
#[tauri::command]
pub async fn env_create_venv(
    python_version: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let config = state.config.get();
    let venv_path = PathBuf::from(&config.paths.venv_path);

    state
        .python_env
        .create_venv(&venv_path, &python_version)
        .await
        .map_err(|e| e.to_string())
}

/// 安装 torch
#[tauri::command]
pub async fn env_install_torch(
    cuda_version: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    let cuda = parse_cuda_version(&cuda_version)?;
    let config = state.config.get();
    let venv_path = PathBuf::from(&config.paths.venv_path);

    state
        .python_env
        .install_torch(&venv_path, cuda)
        .await
        .map_err(|e| {
            let _ = app.emit("env_error", e.to_string());
            e.to_string()
        })
}

/// 切换 Python 版本
#[tauri::command]
pub async fn env_switch_python(
    python_version: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    let config = state.config.get();
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);

    // 进度推送 task
    let app_clone = app.clone();
    let progress_task = tokio::spawn(async move {
        while let Some(progress) = rx.recv().await {
            let _ = app_clone.emit("env_progress", progress);
        }
    });

    let result = state
        .python_env
        .switch_python_version(&python_version, &config, tx)
        .await
        .map_err(|e| e.to_string());

    progress_task.abort();
    result
}

/// 检查依赖兼容性
#[tauri::command]
pub async fn env_check_compatibility(
    state: State<'_, AppState>,
) -> Result<CompatibilityReport, String> {
    let config = state.config.get();
    let venv_path = PathBuf::from(&config.paths.venv_path);
    let comfyui_root = PathBuf::from(&config.paths.comfyui_root);

    state
        .python_env
        .check_requirements_compatibility(&venv_path, &comfyui_root)
        .await
        .map_err(|e| e.to_string())
}

/// 重建 venv
#[tauri::command]
pub async fn env_rebuild_venv(
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    let config = state.config.get();
    let config_clone = (*config).clone();

    state
        .python_env
        .rebuild_venv(&config_clone)
        .await
        .map_err(|e| {
            let _ = app.emit("env_error", e.to_string());
            e.to_string()
        })
}

/// 解析 CUDA 版本字符串
fn parse_cuda_version(s: &str) -> Result<CudaVersion, String> {
    match s.to_lowercase().as_str() {
        "cpu" => Ok(CudaVersion::Cpu),
        "cu118" => Ok(CudaVersion::Cu118),
        "cu121" => Ok(CudaVersion::Cu121),
        "cu124" => Ok(CudaVersion::Cu124),
        _ => Err(format!("invalid cuda version: {}", s)),
    }
}
