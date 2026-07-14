//! Config 模块 Tauri commands
//!
//! 设计模式：门面 (Facade) - 前端通过 invoke 调用，不直接接触 ConfigService

use crate::app_state::AppState;
use crate::config::{
    apply_launch_patch, apply_models_patch, apply_paths_patch, apply_torch_patch,
    apply_ui_patch, Config, ConfigPatch,
};
use crate::error::AppError;
use crate::paths::env_paths;
use serde_json::Value;
use tauri::State;

/// 读取当前配置
#[tauri::command]
pub async fn config_get(state: State<'_, AppState>) -> Result<Config, String> {
    let guard = state.config.get();
    Ok((**guard).clone())
}

/// **v0.0.2.1**：获取 launcher 工作目录（即 `<exe_dir>`，绑定 portable 模式）
///
/// 前端「设置 → 数据位置」展示用，也是初始化向导的 placeholder。
/// 一律用 `env_paths::resolve().env_root`，**不**再返回 `current_dir()`
/// （dev 模式下 current_dir 是 `target/debug`，会误导用户）。
#[tauri::command]
pub async fn config_launcher_working_dir() -> Result<String, String> {
    match env_paths::resolve() {
        Ok(p) => Ok(p.env_root.to_string_lossy().to_string()),
        Err(e) => Err(format!("解析 launcher 工作目录失败: {}", e)),
    }
}

/// **v0.0.2.1**：数据位置信息（统一从 `env_paths::resolve()` 拿）
///
/// 前端「设置 → 数据位置」展示用，便于用户排查：
/// - 数据存在哪
/// - 实际配置的 ComfyUI / venv / models 位置（vs 默认推导位置）
/// - 可执行文件位置
#[derive(Debug, Clone, serde::Serialize)]
pub struct DataLocationInfo {
    /// 当前生效的数据目录（= `<env_root>/data/`）
    pub data_dir: String,
    /// 当前生效的 cache 目录（= `<env_root>/cache/`）
    pub cache_dir: String,
    /// ComfyUI 根目录的**默认值**（从 launcher-portable.dat 推导）
    pub comfyui_root_default: String,
    /// venv 路径的**默认值**（从 launcher-portable.dat 推导）
    pub venv_path_default: String,
    /// 模式说明（人类可读，给 `?` tooltip 用）
    pub mode_description: String,
    /// 可执行文件绝对路径
    pub executable_path: Option<String>,
    /// 模式来源：固定 `portable`（v0.0.2.1 删除 env / legacy 兼容分支）
    pub mode: String,
    /// launcher 私有数据目录（= `<env_root>/.boundlaunch/`）
    pub boundlaunch_data_dir: String,

    // ===== Config 里的实际值（用户可能改过） =====
    /// Config.paths.comfyui_root 实际值
    pub comfyui_root_actual: Option<String>,
    /// Config.paths.venv_path 实际值
    pub venv_path_actual: Option<String>,
    /// Config.paths.models_path 实际值（None = 未配置）
    pub models_path_actual: Option<String>,
    /// comfyui_root 是否在默认位置（字符串相等，Windows 大小写不敏感由 FS 处理）
    pub comfyui_root_is_default: bool,
    /// venv_path 是否在默认位置
    pub venv_path_is_default: bool,
}

#[tauri::command]
pub async fn config_data_location(
    state: State<'_, AppState>,
) -> Result<DataLocationInfo, String> {
    // v0.0.2.1：统一从 env_paths::resolve() 拿所有路径
    let resolved = env_paths::resolve()
        .map_err(|e| format!("解析路径失败: {}", e))?;

    // 读取 Config 实际值
    let cfg = state.config.get();
    let actual_comfyui = cfg.paths.comfyui_root.clone();
    let actual_venv = cfg.paths.venv_path.clone();
    let actual_models = cfg.paths.models_path.clone();

    // 字符串归一化比较（Windows 路径不区分大小写）
    let normalize = |p: &std::path::Path| p.to_string_lossy().to_lowercase();

    let comfyui_is_default = normalize(&actual_comfyui) == normalize(&resolved.comfyui_root);
    let venv_is_default = normalize(&actual_venv) == normalize(&resolved.venv_path);

    let mode_description = format!(
        "Portable 模式：所有数据存放在 launcher 所在目录（{}）的子目录下",
        resolved.env_root.display()
    );

    let executable = std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().to_string());

    Ok(DataLocationInfo {
        // 推导值
        data_dir: resolved.app_data_dir.to_string_lossy().to_string(),
        cache_dir: resolved.cache_dir.to_string_lossy().to_string(),
        comfyui_root_default: resolved.comfyui_root.to_string_lossy().to_string(),
        venv_path_default: resolved.venv_path.to_string_lossy().to_string(),
        boundlaunch_data_dir: resolved.boundlaunch_data_dir.to_string_lossy().to_string(),
        mode_description,
        executable_path: executable,
        mode: "portable".to_string(),
        // Config 实际值
        comfyui_root_actual: Some(actual_comfyui.to_string_lossy().to_string()),
        venv_path_actual: Some(actual_venv.to_string_lossy().to_string()),
        models_path_actual: actual_models
            .as_ref()
            .map(|p| p.to_string_lossy().to_string()),
        comfyui_root_is_default: comfyui_is_default,
        venv_path_is_default: venv_is_default,
    })
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
            // v3.x：models 段已废弃，patch.models 静默忽略
            let _ = patch.models;
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
