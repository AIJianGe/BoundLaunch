//! ModelPathManager 的 Tauri commands
//!
//! 设计模式：门面（Facade）- 前端仅与本层交互，不直接访问 Service
//!
//! 详见 `PR/03-模块设计/05-ModelPathManager.md §3 接口签名`

use std::path::PathBuf;

use tauri::State;

use crate::app_state::AppState;
use crate::config::{ModelsConfig, ModelsMode};
use crate::model_path::{validate_root, GenerateYamlResult, ScanResult};

/// 生成 `extra_model_paths.yaml`
///
/// 按 `custom_root` 构造 ModelsConfig（mode=CustomRoot）后调用 service。
/// - 若当前 yaml 是用户手动配置的，会自动备份为 `<name>.yaml.user-bak-<ts>`
/// - 若当前 yaml 是 launcher 生成的，直接覆盖
#[tauri::command]
pub async fn modelpath_generate(
    custom_root: String,
    state: State<'_, AppState>,
) -> Result<GenerateYamlResult, String> {
    let path = PathBuf::from(&custom_root);
    let cfg = ModelsConfig {
        mode: ModelsMode::CustomRoot,
        custom_root: path,
        advanced: Default::default(),
    };
    state.model_path.generate_yaml(&cfg).await.map_err(|e| {
        tracing::error!(error = %e, ?custom_root, "modelpath_generate failed");
        e.to_string()
    })
}

/// 删除 launcher 生成的 yaml
///
/// - yaml 不存在 → `Ok(())`
/// - launcher 生成的 → 删除
/// - 用户手动 yaml → 跳过（不删除，仅 warn）
#[tauri::command]
pub async fn modelpath_remove(state: State<'_, AppState>) -> Result<(), String> {
    state.model_path.remove_yaml().await.map_err(|e| {
        tracing::error!(error = %e, "modelpath_remove failed");
        e.to_string()
    })
}

/// 扫描根目录下所有 ComfyUI 子目录
///
/// - 60s 缓存命中（同 root + 同 mtime）直接返回
/// - 未命中：spawn_blocking + rayon par_iter 并行扫描 16 子目录
/// - `force=true` 强制刷新缓存
#[tauri::command]
pub async fn modelpath_scan(
    root: String,
    force: bool,
    state: State<'_, AppState>,
) -> Result<ScanResult, String> {
    let path = PathBuf::from(&root);
    state.model_path.scan_subdirs(&path, force).await.map_err(|e| {
        tracing::error!(error = %e, ?root, "modelpath_scan failed");
        e.to_string()
    })
}

/// 校验根目录合法性
///
/// 错误类型：
/// - `EmptyRoot`：路径为空
/// - `RootNotFound`：目录不存在
/// - `RootNotReadable`：目录不可读
#[tauri::command]
pub async fn modelpath_validate(root: String) -> Result<(), String> {
    let path = PathBuf::from(&root);
    // validate_root 是同步无状态函数，无需 AppState
    validate_root(&path).map_err(|e| {
        tracing::error!(error = %e, ?root, "modelpath_validate failed");
        e.to_string()
    })
}
