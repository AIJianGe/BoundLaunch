//! Readiness check - 启动 ComfyUI 前的环境就绪性检查
//!
//! 设计目标：
//! - 把"启动 ComfyUI 需要哪些前置条件"集中在一处
//! - 启动按钮（前端）调一次 `env_readiness_check` 即可决定：
//!   - 直接调 `process_start`（ready=true）
//!   - 还是按 missing_steps 顺序引导用户/前端自动补齐
//!
//! 与 `EnvironmentInspector::inspect_all` 的区别：
//! - `inspect_all` 给出完整环境快照（前端 UI 用）
//! - `readiness_check` 给出"能否启动 ComfyUI"的判断 + 缺失步骤清单
//!
//! 详见 `PR/03-模块设计/07-EnvironmentInspector.md`

use std::path::Path;

use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::config::{Config, LaunchMode};
use crate::core_manager::CoreManagerService;
use crate::env_inspector::models::{DepStatus, DependencyInfo, EnvInfo};
use crate::python_env::PythonEnvService;

/// 环境就绪性检查结果
#[derive(Debug, Clone, Serialize)]
pub struct ReadinessResult {
    /// 整体是否就绪（所有步骤都通过）
    pub ready: bool,
    /// 缺失的步骤（按执行顺序）
    pub missing_steps: Vec<ReadinessStep>,
    /// 各分项状态（前端 UI 详情展示用）
    pub checks: ReadinessChecks,
    /// 当前生效的启动模式（来自 `config.launch.mode`，前端用其判断 CUDA 模式冲突）
    ///
    /// v3.10 新增：与 `cuda_available` 配合，前端可在用户点击"启动"时
    /// 检测"模式 = CPU / 实际 CUDA 可用"或"模式 = GPU / 实际 CUDA 不可用"两类不匹配，
    /// 给出引导（避免启动时 AssertionError）。
    pub launch_mode: LaunchMode,
    /// torch 在当前 venv 中是否实际可用 CUDA
    ///
    /// v3.10 新增：来源 `EnvSnapshot.cuda_available`。
    /// 注意：此字段反映"torch 库是否就绪"，不等同于"驱动 / 硬件是否支持 CUDA"。
    pub cuda_available: bool,
}

/// 分项检查结果
#[derive(Debug, Clone, Serialize)]
pub struct ReadinessChecks {
    pub comfyui_cloned: bool,
    pub venv_exists: bool,
    pub uv_available: bool,
    pub torch_installed: bool,
    pub requirements_ok: bool,
}

/// 单个缺失步骤（前端按此顺序自动补齐）
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", content = "params")]
pub enum ReadinessStep {
    /// 克隆 ComfyUI 仓库（无参数，用 core_ensure_cloned 默认仓库）
    CloneComfyUI,
    /// 创建 venv
    CreateVenv {
        python_version: String,
    },
    /// 安装 torch
    InstallTorch {
        cuda_version: String,
    },
    /// 安装 requirements.txt 中的依赖
    InstallRequirements,
}

/// 执行 readiness 检查
///
/// 不修改任何状态（不克隆、不安装），仅做只读检测。
pub async fn check_readiness(
    config: &Config,
    core_manager: &CoreManagerService,
    env_inspector: &crate::env_inspector::EnvironmentInspectorService,
    python_env: &PythonEnvService,
) -> ReadinessResult {
    let venv_path = Path::new(&config.paths.venv_path);
    let comfyui_root = Path::new(&config.paths.comfyui_root);

    // 1. ComfyUI 仓库是否克隆
    let comfyui_cloned = core_manager.is_cloned().await;

    // 2. venv 是否存在
    let venv_exists = venv_path.exists() && venv_path.join("pyvenv.cfg").exists();

    // 3. uv 是否可用
    let uv_available = python_env.is_uv_available().await;

    // 4. torch 是否安装
    //    readiness::check_readiness 是死代码（无调用方），用本地不可取消 token
    //    前端实际走 env_readiness_check 命令（基于 inspect_or_cached 快速返回）
    let cancel = CancellationToken::new();
    let env_info: EnvInfo = match env_inspector.inspect_all(venv_path, comfyui_root, &cancel).await {
        Ok(info) => info,
        Err(e) => {
            tracing::warn!(error = %e, "env_inspect failed during readiness check");
            // 检测失败：按"venv 不存在 + torch 未安装"兜底，避免误报 ready
            return ReadinessResult {
                ready: false,
                missing_steps: vec![
                    ReadinessStep::CreateVenv {
                        python_version: config.paths.python_version.clone(),
                    },
                    ReadinessStep::InstallTorch {
                        cuda_version: cuda_version_to_string(&config.torch.cuda_version),
                    },
                ],
                checks: ReadinessChecks {
                    comfyui_cloned,
                    venv_exists: false,
                    uv_available,
                    torch_installed: false,
                    requirements_ok: false,
                },
                launch_mode: config.launch.mode,
                // 兜底：检测失败时假设 CUDA 不可用（保守）
                cuda_available: false,
            };
        }
    };

    let torch_installed = env_info.torch.installed;
    let cuda_available = env_info.torch.cuda_available;

    // 5. requirements 是否全部满足
    let (requirements_ok, missing_required) =
        count_requirements_status(&env_info.dependencies);

    // 构造 missing_steps
    let mut missing_steps: Vec<ReadinessStep> = Vec::new();

    if !comfyui_cloned {
        missing_steps.push(ReadinessStep::CloneComfyUI);
    }
    if !venv_exists {
        missing_steps.push(ReadinessStep::CreateVenv {
            python_version: config.paths.python_version.clone(),
        });
    }
    if !torch_installed {
        missing_steps.push(ReadinessStep::InstallTorch {
            cuda_version: cuda_version_to_string(&config.torch.cuda_version),
        });
    }
    if !requirements_ok {
        missing_steps.push(ReadinessStep::InstallRequirements);
        // 仅当确实有 Missing 项时才加入（避免空跑）
        let _ = missing_required; // 当前仅作为占位以便将来扩展
    }

    let ready = missing_steps.is_empty();

    ReadinessResult {
        ready,
        missing_steps,
        checks: ReadinessChecks {
            comfyui_cloned,
            venv_exists,
            uv_available,
            torch_installed,
            requirements_ok,
        },
        launch_mode: config.launch.mode,
        cuda_available,
    }
}

/// 统计 requirements 状态
///
/// 返回 (全部满足, 缺失的依赖数)
fn count_requirements_status(deps: &[DependencyInfo]) -> (bool, usize) {
    let missing = deps
        .iter()
        .filter(|d| matches!(d.status, DepStatus::Missing | DepStatus::NeedsUpgrade { .. }))
        .count();
    (missing == 0, missing)
}

/// 把 `CudaVersion` 枚举转为后端命令接受的字符串
///
/// v3.7：支持 cu118 / cu126 / cu128 / cu130
pub fn cuda_version_to_string(cuda: &crate::config::CudaVersion) -> String {
    use crate::config::CudaVersion::*;
    match cuda {
        Cpu => "cpu".to_string(),
        Cu118 => "cu118".to_string(),
        Cu126 => "cu126".to_string(),
        Cu128 => "cu128".to_string(),
        Cu130 => "cu130".to_string(),
    }
}
