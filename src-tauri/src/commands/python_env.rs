//! PythonEnvManager Tauri commands（门面层）
//!
//! 设计模式：**门面 (Facade)** - 前端只与本层交互，不直接调 PythonEnvService
//!
//! F32 改造（v3.3）：
//! - 6 个长任务命令（env_create_venv / env_install_torch / env_change_torch_variant /
//!   env_switch_python / env_install_requirements / env_rebuild_venv）改为返回 task_id
//! - 命令层只负责「拿 config → 调 factory → submit → 返回 task_id」
//! - 实际执行由 TaskScheduler 调度，进度通过 `task_progress` 事件推送
//! - 完成后通过 `task_completed` 事件通知前端
//! - 详见 `PR/03-模块设计/02-PythonEnvManager.md §14 F32 长任务异步化`
//!
//! 详见 `PR/03-模块设计/02-PythonEnvManager.md §3 接口签名` 末尾的 `#[tauri::command]` 定义

use std::path::PathBuf;

use tauri::{AppHandle, State};

use crate::app_state::AppState;
use crate::config::CudaVersion;
use crate::python_env::models::{CompatibilityReport, PythonEnvStatus};
use crate::python_env::recovery::{self, DiagnoseReport, RepairAction};
use crate::task_scheduler::factory::{
    make_create_venv_task, make_env_repair_task, make_install_requirements_task,
    make_install_torch_task, make_rebuild_venv_task, make_switch_python_task,
    make_switch_torch_variant_task,
};

// ====================================================================
// 查询类命令（不改造，保持同步）
// ====================================================================

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

// ====================================================================
// F32 长任务命令（改为返回 task_id）
// ====================================================================

/// 创建 venv
///
/// F32 改造：返回 task_id，实际执行由 TaskScheduler 调度。
/// 前端通过 `task_progress` 事件接收进度，`task_completed` 事件接收结果。
#[tauri::command]
pub async fn env_create_venv(
    python_version: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let venv_path = {
        let config = state.config.get();
        PathBuf::from(&config.paths.venv_path)
    };

    let task_def = make_create_venv_task(
        state.python_env.clone(),
        venv_path,
        python_version,
    );

    state
        .task_scheduler
        .submit(task_def)
        .await
        .map_err(|e| e.to_string())
}

/// 安装 torch
///
/// F32 改造：返回 task_id，实际执行由 TaskScheduler 调度。
#[tauri::command]
pub async fn env_install_torch(
    cuda_version: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let cuda = parse_cuda_version(&cuda_version)?;
    let venv_path = {
        let config = state.config.get();
        PathBuf::from(&config.paths.venv_path)
    };

    let task_def = make_install_torch_task(
        state.python_env.clone(),
        venv_path,
        cuda,
    );

    state
        .task_scheduler
        .submit(task_def)
        .await
        .map_err(|e| e.to_string())
}

/// 切换 torch 变体（v3.0 新增，F25；F32 改为异步任务）
///
/// 支持多厂商（NVIDIA / AMD / Intel / Apple / CPU）。
/// 切换前会先停止 ComfyUI 进程（如运行）。
/// 失败时返回错误，旧 torch 保留。
///
/// F32 改造：返回 task_id，实际执行由 TaskScheduler 调度。
/// action 内部包含：停 ComfyUI → 切换 torch → 更新 Config。
#[tauri::command]
pub async fn env_change_torch_variant(
    variant: crate::python_env::TorchVariant,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<String, String> {
    let venv_path = {
        let config = state.config.get();
        PathBuf::from(&config.paths.venv_path)
    };

    let task_def = make_switch_torch_variant_task(
        state.python_env.clone(),
        state.config.clone(),
        venv_path,
        variant,
        state.process_launcher.clone(),
        app,
    );

    state
        .task_scheduler
        .submit(task_def)
        .await
        .map_err(|e| e.to_string())
}

/// 切换 Python 版本
///
/// F32 改造：返回 task_id，实际执行由 TaskScheduler 调度。
/// action 内部桥接 mpsc::Sender<InstallProgress> → ProgressSender。
#[tauri::command]
pub async fn env_switch_python(
    python_version: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let config = {
        let guard = state.config.get();
        (**guard).clone()
    };

    let task_def = make_switch_python_task(
        state.python_env.clone(),
        python_version,
        config,
    );

    state
        .task_scheduler
        .submit(task_def)
        .await
        .map_err(|e| e.to_string())
}

/// 安装 ComfyUI requirements.txt 依赖（v2.14；F32 改为异步任务）
///
/// 幂等：`uv pip install -r requirements.txt` 对已满足的包自动跳过
/// 路径：`<comfyui_root>/requirements.txt`（不存在则报错）
///
/// 用例：
/// - OnboardingPage 阶段 5：venv + torch 装完后，装 ComfyUI 必备依赖
/// - 设置页「路径配置」一键补装：envStore.readiness 提示有 InstallRequirements 时
/// - 首页「一键补装」按钮：同上
///
/// F32 改造：返回 task_id，实际执行由 TaskScheduler 调度。
#[tauri::command]
pub async fn env_install_requirements(
    state: State<'_, AppState>,
) -> Result<String, String> {
    let (venv_path, req_file) = {
        let config = state.config.get();
        let venv = PathBuf::from(&config.paths.venv_path);
        let comfyui = PathBuf::from(&config.paths.comfyui_root);
        (venv, comfyui.join("requirements.txt"))
    };

    if !req_file.exists() {
        return Err(format!(
            "requirements.txt 不存在: {}\n提示: 请先克隆 ComfyUI 仓库（请确认 ComfyUI 根目录配置正确）",
            req_file.display()
        ));
    }

    let task_def = make_install_requirements_task(
        state.python_env.clone(),
        venv_path,
        req_file,
    );

    state
        .task_scheduler
        .submit(task_def)
        .await
        .map_err(|e| e.to_string())
}

/// 重建 venv
///
/// F32 改造：返回 task_id，实际执行由 TaskScheduler 调度。
#[tauri::command]
pub async fn env_rebuild_venv(
    state: State<'_, AppState>,
) -> Result<String, String> {
    let config = {
        let guard = state.config.get();
        (**guard).clone()
    };

    let task_def = make_rebuild_venv_task(
        state.python_env.clone(),
        config,
    );

    state
        .task_scheduler
        .submit(task_def)
        .await
        .map_err(|e| e.to_string())
}

// ====================================================================
// v1.8 / F36-Phase2：环境修复（诊断 + 自动修复）
// ====================================================================

/// 环境诊断（v1.8 / F36-Phase2 新增）
///
/// **不会修改任何状态**，纯只读探测。前端可以无副作用地反复调用。
/// 返回 `DiagnoseReport`：
/// - `venv_exists` / `torch_import_ok` / `torch_version`
/// - `issues[]`：诊断出的所有问题（按严重度排序）
/// - `suggested_action`：综合建议（最严重 action）
/// - `suggested_reason`：建议原因（用户可读）
///
/// 对应后端：`python_env/recovery.rs::diagnose`
#[tauri::command]
pub async fn env_diagnose(state: State<'_, AppState>) -> Result<DiagnoseReport, String> {
    let (venv_path, comfyui_root) = {
        let config = state.config.get();
        (
            PathBuf::from(&config.paths.venv_path),
            PathBuf::from(&config.paths.comfyui_root),
        )
    };

    Ok(recovery::diagnose(
        &state.python_env,
        &venv_path,
        &comfyui_root,
        &state.event_bus,
    )
    .await)
}

/// 环境修复（v1.8 / F36-Phase2 新增）
///
/// **F32 改造**：返回 task_id，实际执行由 TaskScheduler 调度。
/// 进度通过 `task_progress` 事件推送，完成通过 `task_completed` 事件通知。
///
/// 参数：
/// - `action`：修复动作（前端根据 `env_diagnose` 的 `suggested_action` 传入）
///
/// 注意：调用方应在调用此命令前先调 `env_diagnose` 拿到报告。
/// 若用户绕过诊断直接传 action，本命令仍可执行（容错）。
#[tauri::command]
pub async fn env_repair(
    action: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let repair_action = parse_repair_action(&action)?;
    let config = {
        let guard = state.config.get();
        (**guard).clone()
    };

    let task_def = make_env_repair_task(state.python_env.clone(), config, repair_action);
    state
        .task_scheduler
        .submit(task_def)
        .await
        .map_err(|e| e.to_string())
}

// ====================================================================
// 辅助函数
// ====================================================================

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

/// 解析 RepairAction 字符串（前端 → 后端）
fn parse_repair_action(s: &str) -> Result<RepairAction, String> {
    match s {
        "none" => Ok(RepairAction::None),
        "downgrade_numpy" => Ok(RepairAction::DowngradeNumpy),
        "reinstall_torch" => Ok(RepairAction::ReinstallTorch),
        "reinstall_requirements" => Ok(RepairAction::ReinstallRequirements),
        "rebuild_venv" => Ok(RepairAction::RebuildVenv),
        _ => Err(format!("invalid repair action: {}", s)),
    }
}
