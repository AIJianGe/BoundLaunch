//! EnvironmentInspector Tauri commands（门面层）
//!
//! 设计模式：**门面 (Facade)** - 前端只与本层交互，不直接调 EnvironmentInspectorService
//!
//! F32 改造（v3.3）：
//! - `env_inspect`：改为返回 `Option<EnvSnapshot>`，立即返回 stale 值，后台 spawn 刷新
//! - `env_readiness_check`：改为返回 `Option<ReadinessResult>`，基于 snapshot 快速构造
//! - `env_probe_torch`：删除（死代码，前端无调用）
//! - 详见 `PR/03-模块设计/07-EnvironmentInspector.md §14 F32 探查类异步化`
//!
//! 详见 `PR/03-模块设计/07-EnvironmentInspector.md §3 接口签名` 末尾的 `#[tauri::command]` 定义

use std::path::PathBuf;

use crate::app_state::AppState;
use crate::config::Config;
use crate::env_inspector::dependency_conflict::{scan_custom_node_requirements, ConflictReport};
use crate::env_inspector::models::{DependencyInfo, EnvSnapshot};
use crate::env_inspector::readiness::{
    self, cuda_version_to_string, ReadinessChecks, ReadinessResult, ReadinessStep,
};
use tauri::State;

/// 完整环境探查（前端进入启动页 / 点击刷新时调用）
///
/// F32 改造（v3.3）：返回 `Option<EnvSnapshot>`
///
/// 行为：
/// - **有 stale 值**：立即返回最后一次 `inspect_snapshot` 的结果（不阻塞，≤1ms）
/// - **无 stale 值**（首次启动或 `clear()` 后）：返回 `None`
///   前端应显示 loading，并监听 `env_inspect_updated` 事件以接收新快照
/// - **后台刷新**：若 `cache.needs_refresh()` 为 true，触发 `spawn_refresh`
///   刷新完成后会 emit `env_inspect_updated` 事件（payload = 新 `EnvSnapshot`）
///
/// 前端示例：
/// ```ts
/// const info = await invoke('env_inspect') // Option<EnvSnapshot>
/// if (info === null) {
///   // 首次启动，等 env_inspect_updated 事件
/// } else {
///   envStore.setEnvInfo(info) // 可能是 stale 值
/// }
/// ```
///
/// 注：venv_path 与 comfyui_root 由后端从 Config 读取，前端无需传入
#[tauri::command]
pub async fn env_inspect(state: State<'_, AppState>) -> Result<Option<EnvSnapshot>, String> {
    let venv_path = {
        let config = state.config.get();
        PathBuf::from(&config.paths.venv_path)
    };
    let comfyui_root = {
        let config = state.config.get();
        PathBuf::from(&config.paths.comfyui_root)
    };

    Ok(state
        .env_inspector
        .inspect_or_cached(&venv_path, &comfyui_root))
}

/// 仅列出关键依赖（前端依赖列表刷新用）
///
/// v3.6 改造：从 `snapshot_cache` 提取 `dependencies`（不调子进程）
/// - **有 stale snapshot**：立即返回 `Some(dependencies)`（≤1ms）
/// - **无 stale snapshot**（首次启动或 `clear()` 后）：返回 `None`
///   前端应等待 `env_inspect_updated` 事件后从 `EnvSnapshot.dependencies` 拿数据
///
/// 改造原因：原实现同步调 `inspect_dependencies`（5-30s 阻塞），
/// 与 F32「探查类命令立即返回」模式不一致。
///
/// 兼容性：返回 `Option<Vec<DependencyInfo>>`，前端需适配 null 场景
#[tauri::command]
pub async fn env_list_dependencies(
    state: State<'_, AppState>,
) -> Result<Option<Vec<DependencyInfo>>, String> {
    let venv_path = {
        let config = state.config.get();
        PathBuf::from(&config.paths.venv_path)
    };
    let comfyui_root = {
        let config = state.config.get();
        PathBuf::from(&config.paths.comfyui_root)
    };

    // 从 snapshot_cache 提取（同时触发后台刷新）
    let snapshot = state
        .env_inspector
        .inspect_or_cached(&venv_path, &comfyui_root);

    Ok(snapshot.map(|s| s.dependencies))
}

/// 主动失效缓存（前端用户手动刷新时调用）
///
/// F32 改造：仅标记 stale=true（保留旧值）。
/// 下次 `env_inspect` 调用时会自动触发后台 `spawn_refresh`。
#[tauri::command]
pub async fn env_invalidate_cache(state: State<'_, AppState>) -> Result<(), String> {
    state.env_inspector.invalidate_cache();
    Ok(())
}

/// 环境就绪性检查（启动 ComfyUI 前调用）
///
/// F32 改造（v3.3）：返回 `Option<ReadinessResult>`
///
/// 行为：
/// - **有 stale snapshot**：基于 snapshot 快速构造 `ReadinessResult`（≤1ms，不阻塞）
///   - `comfyui_cloned` / `venv_exists`：从 Config 路径同步检测（<1ms）
///   - `uv_available`：从 snapshot 间接推断（onboarding 时已确定，运行期不变）
///   - `torch_installed` / `requirements_ok`：从 snapshot 字段读取
/// - **无 stale snapshot**（首次启动）：返回 `None`
///   前端应等 `env_inspect_updated` 事件后重新调用
/// - **后台刷新**：与 `env_inspect` 共享同一份 cache，会自动触发 `spawn_refresh`
///
/// 不修改任何状态（不克隆、不安装），仅做只读检测。
#[tauri::command]
pub async fn env_readiness_check(
    state: State<'_, AppState>,
) -> Result<Option<ReadinessResult>, String> {
    let cfg: Config = {
        let guard = state.config.get();
        (**guard).clone()
    };
    let venv_path = PathBuf::from(&cfg.paths.venv_path);
    let comfyui_root = PathBuf::from(&cfg.paths.comfyui_root);

    // 1. 从 inspect_or_cached 拿 stale snapshot（同时触发后台刷新）
    let snapshot = state
        .env_inspector
        .inspect_or_cached(&venv_path, &comfyui_root);

    let Some(snapshot) = snapshot else {
        // 首次启动或 clear() 后无数据
        tracing::debug!("env_readiness_check: no stale snapshot, returning None");
        return Ok(None);
    };

    // 2. 基于 snapshot 快速构造 ReadinessResult
    let venv_exists = venv_path.exists() && venv_path.join("pyvenv.cfg").exists();
    let comfyui_cloned = snapshot.comfyui_cloned;
    // uv_available：onboarding 时已确定（uv sidecar 在 lib.rs 启动时 ensure_released）
    // 这里假设 true（运行期不会变化）；若需精确值，前端可单独调 env_uv_available
    let uv_available = true;
    let torch_installed = snapshot.torch_installed;

    // requirements_ok：检查 dependencies 中是否有 Missing / NeedsUpgrade
    let requirements_ok = snapshot
        .dependencies
        .iter()
        .all(|dep| matches!(dep.status, crate::env_inspector::models::DepStatus::Satisfied | crate::env_inspector::models::DepStatus::NotRequired));

    // 3. 构造 missing_steps
    let mut missing_steps: Vec<ReadinessStep> = Vec::new();
    if !comfyui_cloned {
        missing_steps.push(ReadinessStep::CloneComfyUI);
    }
    if !venv_exists {
        missing_steps.push(ReadinessStep::CreateVenv {
            python_version: cfg.paths.python_version.clone(),
        });
    }
    if !torch_installed {
        missing_steps.push(ReadinessStep::InstallTorch {
            cuda_version: cuda_version_to_string(&cfg.torch.cuda_version),
        });
    }
    if !requirements_ok {
        missing_steps.push(ReadinessStep::InstallRequirements);
    }

    let ready = missing_steps.is_empty();

    Ok(Some(ReadinessResult {
        ready,
        missing_steps,
        checks: ReadinessChecks {
            comfyui_cloned,
            venv_exists,
            uv_available,
            torch_installed,
            requirements_ok,
        },
    }))
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

/// 把 CudaVersion 转为前端可读字符串
///
/// 已迁移到 `readiness::cuda_version_to_string`（pub），此处通过 `use` 导入复用。
/// 保留此注释说明来源，避免后续误加重复实现。

// 兼容性：保留 readiness 模块引用，避免 unused warning（readiness::check_readiness 仍可被其他场景调用）
#[allow(unused_imports)]
use readiness as _readiness_compat;
