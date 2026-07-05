//! EnvironmentInspector Tauri commands（门面层）
//!
//! 设计模式：**门面 (Facade)** - 前端只与本层交互，不直接调 EnvironmentInspectorService
//!
//! 详见 `PR/03-模块设计/07-EnvironmentInspector.md §3 接口签名` 末尾的 `#[tauri::command]` 定义

use std::path::PathBuf;

use crate::app_state::AppState;
use crate::config::Config;
use crate::env_inspector::dependency_conflict::{scan_custom_node_requirements, ConflictReport};
use crate::env_inspector::models::{DependencyInfo, EnvSnapshot, TorchInfo};
use crate::env_inspector::readiness::{self, ReadinessResult};
use tauri::State;

/// 完整环境探查（前端进入启动页 / 点击刷新时调用）
///
/// v2.13 起返回扁平 `EnvSnapshot`（对应前端 `EnvInfo` 类型），不再返回嵌套 `EnvInfo`。
/// 之前返回嵌套结构时前端读不到 `torch_installed` / `comfyui_cloned` 等扁平字段，
/// 导致首页 PYTORCH 等标签错误显示「未安装」。
///
/// 前端示例：
/// ```ts
/// const info = await invoke('env_inspect')
/// ```
///
/// 注：venv_path 与 comfyui_root 由后端从 Config 读取，前端无需传入
#[tauri::command]
pub async fn env_inspect(state: State<'_, AppState>) -> Result<EnvSnapshot, String> {
    let config = state.config.get();
    let venv_path = config.paths.venv_path.clone();
    let comfyui_root = config.paths.comfyui_root.clone();

    let venv = PathBuf::from(&venv_path);
    let comfyui = PathBuf::from(&comfyui_root);

    state
        .env_inspector
        .inspect_snapshot(&venv, &comfyui)
        .await
        .map_err(|e| e.to_string())
}

/// 仅探查 torch（前端顶部状态卡片实时刷新用）
#[tauri::command]
pub async fn env_probe_torch(state: State<'_, AppState>) -> Result<TorchInfo, String> {
    let config = state.config.get();
    let venv = PathBuf::from(&config.paths.venv_path);

    state
        .env_inspector
        .probe_torch(&venv)
        .await
        .map_err(|e| e.to_string())
}

/// 仅列出关键依赖（前端依赖列表刷新用）
///
/// v2.10 起命令层容错：探查失败时返回空列表 + warn 日志，不阻塞前端流程。
///
/// 原因：onboarding 第 4 步 `createVenv` → `refresh()` → `Promise.all([...,
/// envListDependencies()])`，任一失败即整体失败，导致「venv 已创建但前端误报失败」。
/// 依赖列表失败本身不阻塞 onboarding 流程（torch 已装就能进入下一步），容错更合理。
#[tauri::command]
pub async fn env_list_dependencies(state: State<'_, AppState>) -> Result<Vec<DependencyInfo>, String> {
    let config = state.config.get();
    let venv = PathBuf::from(&config.paths.venv_path);
    let comfyui = PathBuf::from(&config.paths.comfyui_root);

    match state.env_inspector.inspect_dependencies(&venv, &comfyui).await {
        Ok(deps) => Ok(deps),
        Err(e) => {
            tracing::warn!(
                error = %e, venv = ?venv,
                "env_list_dependencies failed, returning empty list (non-blocking)"
            );
            Ok(vec![])
        }
    }
}

/// 主动失效缓存（前端用户手动刷新时调用）
#[tauri::command]
pub async fn env_invalidate_cache(state: State<'_, AppState>) -> Result<(), String> {
    state.env_inspector.invalidate_cache();
    Ok(())
}

/// 环境就绪性检查（启动 ComfyUI 前调用）
///
/// 返回 `ReadinessResult`：
/// - `ready = true`：环境就绪，可直接调 `process_start`
/// - `ready = false`：缺失步骤在 `missing_steps` 中（按顺序），前端可依次引导/自动补齐
///
/// 不修改任何状态（不克隆、不安装），仅做只读检测。
#[tauri::command]
pub async fn env_readiness_check(state: State<'_, AppState>) -> Result<ReadinessResult, String> {
    // Guard 持有期间不能跨 await 调用 — 先克隆出 Config 后再 drop
    let cfg: Config = {
        let guard = state.config.get();
        (**guard).clone()
    };
    Ok(readiness::check_readiness(
        &cfg,
        &state.core_manager,
        &state.env_inspector,
        &state.python_env,
    )
    .await)
}

/// v3.0 依赖冲突检测
///
/// 扫描 `<comfyui_root>/custom_nodes/*/requirements.txt`，检测同一 Python 包被多个节点
/// 以不同版本约束引用的情况。
///
/// **只检测不解决**：返回 ConflictReport，前端展示给用户决策，不阻塞启动。
///
/// **性能**：单目录遍历，无子进程调用。典型 1-10 个自定义节点，< 50ms。
#[tauri::command]
pub async fn env_check_dependency_conflicts(
    state: State<'_, AppState>,
) -> Result<ConflictReport, String> {
    let cfg: Config = {
        let guard = state.config.get();
        (**guard).clone()
    };
    let comfyui_root = PathBuf::from(&cfg.paths.comfyui_root);
    Ok(scan_custom_node_requirements(&comfyui_root))
}
