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

/// **v1.8 / F38**：获取 portable 模式下的数据位置信息
///
/// 前端「设置 → 数据位置」展示用，便于用户排查：
/// - 数据存在哪
/// - 走的哪种模式（env / portable / legacy）
/// - 可执行文件位置（prod 模式）
/// - ComfyUI / venv / models 实际配置的位置（vs 默认推导位置）
#[derive(Debug, Clone, serde::Serialize)]
pub struct DataLocationInfo {
    /// 当前生效的数据目录（v1.8 / F38：跟随 portable 模式）
    pub data_dir: String,
    /// 当前生效的 cache 目录（v1.8 / F38：与 data/ 分离）
    pub cache_dir: String,
    /// portable 模式下 ComfyUI 根目录的**默认值**（推导）
    pub comfyui_root_default: String,
    /// portable 模式下 venv 路径的**默认值**（推导）
    pub venv_path_default: String,
    /// portable 模式基础目录（dev → 项目根 / prod → exe 旁 / None = 不可解析）
    pub portable_base_dir: Option<String>,
    /// 模式来源：`env` / `portable` / `legacy`
    pub mode: String,
    /// 模式说明（人类可读，给 `?` tooltip 用）
    pub mode_description: String,
    /// 可执行文件绝对路径（仅 prod 模式有效）
    pub executable_path: Option<String>,

    // ===== v1.8 / F38：Config 里的实际值（用户可能改过） =====
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
    // 判断 mode：env 变量 → portable → legacy
    let mode = if std::env::var(paths::ENV_DATA_DIR)
        .ok()
        .filter(|v| !v.is_empty())
        .is_some()
    {
        "env"
    } else if paths::portable_base_dir().is_some() {
        "portable"
    } else {
        "legacy"
    };

    let mode_description = match mode {
        "env" => "由环境变量 BOUND_LAUNCH_DATA_DIR 强制指定",
        "portable" => "Portable 模式：数据存放在 launcher 所在目录的 data/ 子目录下",
        "legacy" => "Legacy 模式：数据存放在系统 APPDATA 目录（仅在 portable 解析失败时降级）",
        _ => "未知",
    }
    .to_string();

    let data_dir = paths::app_data_dir();
    let cache_dir = paths::cache_dir();
    let portable_base = paths::portable_base_dir();
    let executable = std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().to_string());

    // 默认推导值（用户没改过 Config.paths 时的位置）
    let default_comfyui = paths::default_comfyui_root();
    let default_venv = data_dir.join("venv");

    // 读取 Config 实际值
    let cfg = state.config.get();
    let actual_comfyui = cfg.paths.comfyui_root.clone();
    let actual_venv = cfg.paths.venv_path.clone();
    let actual_models = cfg.paths.models_path.clone();

    // 字符串归一化比较（Windows 路径不区分大小写）
    let normalize = |p: &std::path::Path| p.to_string_lossy().to_lowercase();

    let comfyui_is_default = normalize(&actual_comfyui) == normalize(&default_comfyui);
    let venv_is_default = normalize(&actual_venv) == normalize(&default_venv);

    Ok(DataLocationInfo {
        // 推导值
        data_dir: data_dir.to_string_lossy().to_string(),
        cache_dir: cache_dir.to_string_lossy().to_string(),
        comfyui_root_default: default_comfyui.to_string_lossy().to_string(),
        venv_path_default: default_venv.to_string_lossy().to_string(),
        portable_base_dir: portable_base.as_ref().map(|p| p.to_string_lossy().to_string()),
        mode: mode.to_string(),
        mode_description,
        executable_path: executable,
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
