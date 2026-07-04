//! Config 模块 Tauri commands
//!
//! 设计模式：门面 (Facade) - 前端通过 invoke 调用，不直接接触 ConfigService

use crate::app_state::AppState;
use crate::config::Config;
use crate::error::{AppError, ConfigError};
use serde_json::Value;
use tauri::State;

/// 读取当前配置
#[tauri::command]
pub async fn config_get(state: State<'_, AppState>) -> Result<Config, String> {
    let guard = state.config.get();
    Ok((**guard).clone())
}

/// 更新配置的某个 section
///
/// section: "paths" | "launch" | "torch" | "models" | "ui"
/// value: 对应 section 的 JSON 对象
#[tauri::command]
pub async fn config_update(
    section: String,
    value: Value,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state
        .config
        .update(|cfg| {
            let json = serde_json::to_string(&value)
                .map_err(|e| ConfigError::SerializeError(e.to_string()))?;
            match section.as_str() {
                "paths" => cfg.paths = serde_json::from_str(&json)
                    .map_err(|e| ConfigError::ParseError(e.to_string()))?,
                "launch" => cfg.launch = serde_json::from_str(&json)
                    .map_err(|e| ConfigError::ParseError(e.to_string()))?,
                "torch" => cfg.torch = serde_json::from_str(&json)
                    .map_err(|e| ConfigError::ParseError(e.to_string()))?,
                "models" => cfg.models = serde_json::from_str(&json)
                    .map_err(|e| ConfigError::ParseError(e.to_string()))?,
                "ui" => cfg.ui = serde_json::from_str(&json)
                    .map_err(|e| ConfigError::ParseError(e.to_string()))?,
                _ => {
                    return Err(ConfigError::InvalidValue {
                        field: "section".into(),
                        value: section,
                    })
                }
            }
            Ok(())
        })
        .await
        .map_err(|e: AppError| e.to_string())
}

/// 重置配置为默认
#[tauri::command]
pub async fn config_reset(state: State<'_, AppState>) -> Result<(), String> {
    state.config.reset().await.map_err(|e| e.to_string())
}
