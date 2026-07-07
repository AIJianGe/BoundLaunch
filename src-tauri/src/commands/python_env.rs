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
use crate::python_env::recovery::RepairAction;
use crate::task_scheduler::factory::{
    make_create_venv_task, make_diagnose_task, make_env_repair_task,
    make_force_reinstall_torch_consistent_task, make_install_requirements_task,
    make_install_torch_task, make_rebuild_venv_task,
    make_restore_transformers_default_task, make_switch_python_task,
    make_switch_torch_variant_task, make_switch_transformers_task,
};

// ====================================================================
// 查询类命令（不改造，保持同步）
// ====================================================================

/// 查询当前 venv 状态（v2.13）
///
/// v3.6 改造：torch/python 信息从 `snapshot_cache` 提取（≤1ms），不再同步调
/// `verify_venv`（90s 阻塞）。
/// - uv 状态：仍走 `python_env.is_uv_available()` + `uv.get_version()`（<1s）
/// - venv_exists：文件系统检查（<1ms）
/// - venv_python_version / venv_torch_*：从 `EnvSnapshot` 提取
///   - 无 snapshot 时返回 None / false（首次启动，前端等 `env_inspect_updated` 事件）
///
/// 返回类型保持 `PythonEnvStatus`（非 Option），uv/venv_exists 字段始终有值，
/// torch/python 字段可能为 None/false（无 snapshot 时）。
#[tauri::command]
pub async fn env_status(state: State<'_, AppState>) -> Result<PythonEnvStatus, String> {
    let (venv_path, comfyui_root) = {
        let config = state.config.get();
        (
            PathBuf::from(&config.paths.venv_path),
            PathBuf::from(&config.paths.comfyui_root),
        )
    };

    // 1. uv 状态（快速 <1s）
    let uv = state.python_env.uv();
    let (uv_version, uv_installed) = uv.get_version().await;
    let uv_path = if uv_installed {
        Some(uv.binary_path().to_string_lossy().into_owned())
    } else {
        None
    };

    // 2. venv_exists（文件系统检查 <1ms）
    let venv_exists = venv_path.exists();

    // 3. 从 snapshot 提取 torch/python 信息（≤1ms，触发后台刷新）
    let snapshot = state
        .env_inspector
        .inspect_or_cached(&venv_path, &comfyui_root);

    let (venv_python_version, venv_torch_installed, venv_torch_version, venv_torch_cuda) =
        match snapshot {
            Some(s) => (
                Some(s.python_version),
                s.torch_installed,
                s.torch_version,
                s.cuda_available,
            ),
            None => (None, false, None, false),
        };

    Ok(PythonEnvStatus {
        uv_installed,
        uv_path,
        uv_version,
        venv_exists,
        venv_python_version,
        venv_torch_installed,
        venv_torch_version,
        venv_torch_cuda,
    })
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

    // v3.6：探查类命令使用本地不可取消 token（兼容性检查是短操作）
    let cancel = tokio_util::sync::CancellationToken::new();
    state
        .python_env
        .check_requirements_compatibility(&venv_path, &comfyui_root, &cancel)
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
    let (venv_path, req_file, pytorch_index) = {
        let config = state.config.get();
        let venv = PathBuf::from(&config.paths.venv_path);
        let comfyui = PathBuf::from(&config.paths.comfyui_root);
        // v3.10：计算 pytorch.org 源 URL（与 install_torch 内部一致）
        // - 防止 transformers 5.x 等依赖触发 torch 覆盖成 +cpu
        let pytorch_index = crate::python_env::uv_runner::cuda_index_url(&config.torch.cuda_version);
        (venv, comfyui.join("requirements.txt"), pytorch_index)
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
        pytorch_index,
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
///
/// 返回 task_id，实际诊断通过 TaskScheduler 调度：
/// - 进度通过 `task_progress` 事件推送（10% 开始 / 50% torch 探针 / 100% 完成）
/// - 完成后通过 `task_completed` 事件返回 `DiagnoseReport`（在 `payload` 字段）
/// - 用户可通过 `task_cancel` 命令取消（torch 探针可能耗时 90s）
///
/// `DiagnoseReport` 字段：
/// - `venv_exists` / `torch_import_ok` / `torch_version`
/// - `issues[]`：诊断出的所有问题（按严重度排序）
/// - `suggested_action`：综合建议（最严重 action）
/// - `suggested_reason`：建议原因（用户可读）
///
/// **关键**：诊断完成后 `recovery::diagnose` 内部已 emit `RequirementsInstalled`，
/// 触发 env_inspector cache 失效 + 后台刷新 + `env_inspect_updated` 事件，
/// 前端 store 自动拿到最新 EnvSnapshot，无需额外调用 invalidate。
///
/// v3.6：从同步命令改为 TaskScheduler 任务（支持取消 + 进度）
#[tauri::command]
pub async fn env_diagnose(state: State<'_, AppState>) -> Result<String, String> {
    let (venv_path, comfyui_root) = {
        let config = state.config.get();
        (
            PathBuf::from(&config.paths.venv_path),
            PathBuf::from(&config.paths.comfyui_root),
        )
    };

    let task_def = make_diagnose_task(state.python_env.clone(), venv_path, comfyui_root);
    state
        .task_scheduler
        .submit(task_def)
        .await
        .map_err(|e| e.to_string())
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
// v3.7：transformers 版本切换
// ====================================================================

/// 列出所有可用 transformers 版本（v3.7 新增）
///
/// 从 `TransformersVersionIndex` 获取版本列表（三层缓存：L1 内存 → L2 文件 → L3 fallback）。
/// 同步返回，不阻塞。
///
/// 版本列表降序排列（最新在前），包含 4.x 和 5.x。
/// 前端应将 5.x 标记为「实验」（破坏性 API 变更）。
#[tauri::command]
pub async fn env_list_transformers_versions(
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    Ok(state.transformers_index.get_versions())
}

/// 切换 transformers 版本（v3.7 新增）
///
/// 返回 task_id，实际执行由 TaskScheduler 调度。
/// - 进度通过 `task_progress` 事件推送（10% 开始 / 50% uv pip install / 90% 校验 / 100% 完成）
/// - 完成后通过 `task_completed` 事件通知前端
/// - 完成后自动 emit `RequirementsInstalled` 让 env cache 失效
///
/// 参数：
/// - `version`：目标版本号（如 "4.57.3" 或 "5.13.0"）
#[tauri::command]
pub async fn env_switch_transformers(
    version: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let venv_path = {
        let config = state.config.get();
        PathBuf::from(&config.paths.venv_path)
    };

    let task_def =
        make_switch_transformers_task(state.python_env.clone(), venv_path, version);

    state
        .task_scheduler
        .submit(task_def)
        .await
        .map_err(|e| e.to_string())
}

/// 恢复默认 transformers 版本（v3.7 新增）
///
/// 按 ComfyUI `requirements.txt` 中的 `transformers>=X.Y.Z` 约束，
/// 从版本列表选满足约束的最新 4.x 版本（排除 5.x 破坏性变更）切换。
///
/// 返回 task_id，实际执行由 TaskScheduler 调度。
/// `task_completed` 事件的 payload 包含选定的版本号：`{ "version": "4.57.3" }`
#[tauri::command]
pub async fn env_restore_transformers_default(
    state: State<'_, AppState>,
) -> Result<String, String> {
    let (venv_path, comfyui_root) = {
        let config = state.config.get();
        (
            PathBuf::from(&config.paths.venv_path),
            PathBuf::from(&config.paths.comfyui_root),
        )
    };

    let task_def = make_restore_transformers_default_task(
        state.python_env.clone(),
        state.transformers_index.clone(),
        venv_path,
        comfyui_root,
    );

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
///
/// v3.7：支持 cu118 / cu126 / cu128 / cu130
/// 旧值 cu121 / cu124（已弃用）返回 Err，触发前端重新选择
fn parse_cuda_version(s: &str) -> Result<CudaVersion, String> {
    match s.to_lowercase().as_str() {
        "cpu" => Ok(CudaVersion::Cpu),
        "cu118" => Ok(CudaVersion::Cu118),
        "cu126" => Ok(CudaVersion::Cu126),
        "cu128" => Ok(CudaVersion::Cu128),
        "cu130" => Ok(CudaVersion::Cu130),
        _ => Err(format!("unsupported cuda version: {} (v3.7 已弃用 cu121/cu124)", s)),
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

// ====================================================================
// v3.10：torch 一致性诊断（mismatch 检测）
// ====================================================================

/// torch 一致性报告（前端可见）
///
/// v3.10 新增：检测 venv 中的实际 torch 状态是否与 Config 期望一致。
/// 用于解决「Config 写 cu128，但 venv 实际是 +cpu」这种 mismatch 问题。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TorchConsistencyReport {
    /// 是否完全一致
    pub consistent: bool,
    /// Config 期望的 cuda_version（如 "cu128" / "cpu"）
    pub config_cuda_version: String,
    /// venv 中实际 torch 版本（如 "2.12.1+cpu"）
    pub venv_torch_version: Option<String>,
    /// venv 中 torch.cuda.is_available()
    pub venv_cuda_available: bool,
    /// 人类可读的问题列表
    pub issues: Vec<String>,
    /// 修复建议
    pub recommendation: TorchConsistencyRecommendation,
}

/// 一致性问题的修复建议
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TorchConsistencyRecommendation {
    /// 状态完全一致，无需操作
    NoAction,
    /// 建议重装 torch（venv 与 Config 不一致，但 venv 结构良好）
    ReinstallTorch,
    /// 建议重装 venv（严重不一致）
    RebuildVenv,
    /// 建议检查 NVIDIA 驱动（Config 期望 GPU 但 torch 不支持）
    CheckDriver,
}

/// v3.10 新增：检测 venv 中的 torch 状态是否与 Config 一致
///
/// 返回 `TorchConsistencyReport`：
/// - `consistent: true` → venv 状态匹配 Config，不需要重装
/// - `consistent: false` → venv 状态与 Config 不一致，建议重装
/// - `recommendation` → 修复建议
///
/// 调用方式：前端在启动 ComfyUI 前自动调用，或在「一键补装」流程中显式调用
#[tauri::command]
pub async fn env_check_torch_consistency(
    state: State<'_, AppState>,
) -> Result<TorchConsistencyReport, String> {
    use tokio_util::sync::CancellationToken;

    let (venv_path, config_cuda_version) = {
        let config = state.config.get();
        (
            PathBuf::from(&config.paths.venv_path),
            format!("{:?}", config.torch.cuda_version).to_lowercase(),
        )
    };

    // 同步调用 verify_venv（30s 缓存命中时 <1ms，未命中时 <2s）
    let cancel = CancellationToken::new();
    let info = state
        .python_env
        .verify_venv(&venv_path, &cancel)
        .await
        .map_err(|e| e.to_string())?;

    let mut issues = Vec::new();
    let mut recommendation = TorchConsistencyRecommendation::NoAction;

    if !info.torch_installed {
        issues.push(format!("torch 未安装（venv 路径：{}）", venv_path.display()));
        recommendation = TorchConsistencyRecommendation::ReinstallTorch;
    } else {
        let venv_torch_str = info.torch_version.as_deref().unwrap_or("?");
        let config_cuda_lower = config_cuda_version.as_str();

        // 检查 1：Config 期望 CUDA，但 venv 中 torch 不支持 CUDA
        //        （用 cuda_available 推断，cuda_available=false 说明 torch 没有 CUDA 编译）
        if config_cuda_lower != "cpu" && !info.cuda_available {
            issues.push(format!(
                "Config 期望 {}，但 venv 中 torch={}（cuda_available={}）",
                config_cuda_lower, venv_torch_str, info.cuda_available
            ));
            recommendation = TorchConsistencyRecommendation::ReinstallTorch;
        }
        // 检查 2：Config 期望 CPU，但 venv 中 torch 支持 CUDA（warning，不是 error）
        else if config_cuda_lower == "cpu" && info.cuda_available {
            issues.push(format!(
                "Config 期望 CPU 模式，但 venv 中 torch 支持 CUDA（{}）。\n\
                 这不会导致 ComfyUI 启动失败，但会浪费 CUDA 资源。",
                venv_torch_str
            ));
            // 不强制重装，标记为 NoAction（让用户决定）
        }
    }

    Ok(TorchConsistencyReport {
        consistent: issues.is_empty(),
        config_cuda_version,
        venv_torch_version: info.torch_version,
        venv_cuda_available: info.cuda_available,
        issues,
        recommendation,
    })
}

/// v3.10 新增：强制一致重装 torch（修复 venv 状态混乱）
///
/// 用 `--force-reinstall --no-deps --index-url pytorch.org` 强制覆盖重装
/// torch/torchvision/torchaudio，**不破坏 venv 中的其他包**。
///
/// 返回 task_id，由 TaskScheduler 异步执行：
/// - 进度通过 `task_progress` 事件推送（10% / 30% / 60% / 80% / 90% / 100%）
/// - 完成后通过 `task_completed` 事件通知前端
/// - 完成后自动 emit `TorchInstalled` 让 env cache 失效
///
/// **典型调用场景**：
/// - 用户 Config 写 `cu128`，但 venv 中 `torch.cuda.is_available() = False`
/// - `env_check_torch_consistency` 返回 `consistent: false, recommendation: reinstall_torch`
/// - 前端调 `env_repair_consistent("cu128")` 修复
#[tauri::command]
pub async fn env_repair_consistent(
    cuda_version: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let cuda = parse_cuda_version(&cuda_version)?;

    let (venv_path, comfyui_root) = {
        let config = state.config.get();
        (
            PathBuf::from(&config.paths.venv_path),
            PathBuf::from(&config.paths.comfyui_root),
        )
    };

    let task_def = make_force_reinstall_torch_consistent_task(
        state.python_env.clone(),
        venv_path,
        comfyui_root,
        cuda,
    );
    state
        .task_scheduler
        .submit(task_def)
        .await
        .map_err(|e| e.to_string())
}
