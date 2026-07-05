//! Config 模块 Tauri commands
//!
//! 设计模式：门面 (Facade) - 前端通过 invoke 调用，不直接接触 ConfigService

use crate::app_state::AppState;
use crate::common::paths;
use crate::config::{
    apply_launch_patch, apply_models_patch, apply_paths_patch, apply_torch_patch,
    apply_ui_patch, Config, ConfigPatch,
};
use crate::error::AppError;
use serde_json::Value;
use tauri::State;

/// 读取当前配置
#[tauri::command]
pub async fn config_get(state: State<'_, AppState>) -> Result<Config, String> {
    let guard = state.config.get();
    Ok((**guard).clone())
}

/// 获取 launcher 工作目录
///
/// 当前进程的工作目录，作为 ComfyUI 根目录的默认值。
/// 前端在初始化向导时调用，把此值作为 comfyui_root 的 placeholder / 初始值。
#[tauri::command]
pub async fn config_launcher_working_dir() -> Result<String, String> {
    Ok(paths::launcher_working_dir().to_string_lossy().to_string())
}

/// 更新配置（部分更新，深合并语义）
///
/// update: 包含 paths/launch/torch/models/ui 的部分对象
///
/// 设计：前端可只传修改过的字段（如 `{ launch: { mode: "gpu_high" } }`），
/// 未传的字段保留原值。这是通过 `ConfigPatch` + `apply_*_patch` 实现的。
#[tauri::command]
pub async fn config_update(
    update: Value,
    state: State<'_, AppState>,
) -> Result<Config, String> {
    // 先把整个 value 解析为 ConfigPatch（None 字段跳过）
    let patch: ConfigPatch = serde_json::from_value(update)
        .map_err(|e| format!("TOML 解析失败: {}", e))?;

    state
        .config
        .update(|cfg| {
            if let Some(p) = patch.paths {
                apply_paths_patch(&mut cfg.paths, p);
            }
            if let Some(p) = patch.launch {
                apply_launch_patch(&mut cfg.launch, p);
            }
            if let Some(p) = patch.torch {
                apply_torch_patch(&mut cfg.torch, p);
            }
            if let Some(p) = patch.models {
                apply_models_patch(&mut cfg.models, p);
            }
            if let Some(p) = patch.ui {
                apply_ui_patch(&mut cfg.ui, p);
            }
            Ok(())
        })
        .await
        .map_err(|e: AppError| e.to_string())?;

    let guard = state.config.get();
    Ok((**guard).clone())
}

/// 重置配置为默认
#[tauri::command]
pub async fn config_reset(state: State<'_, AppState>) -> Result<Config, String> {
    state.config.reset().await.map_err(|e| e.to_string())?;
    let guard = state.config.get();
    Ok((**guard).clone())
}
