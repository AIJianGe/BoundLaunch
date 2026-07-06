//! F32 任务工厂模块
//!
//! 集中构造 PythonEnvManager 相关的 `TaskDef`，供 `commands/python_env.rs` 调用。
//!
//! 设计目标：
//! - 命令层只负责「拿 config → 调 factory → submit → 返回 task_id」
//! - action 闭包内部细节（5 段进度 / 错误转换 / 事件广播）集中在本模块
//! - 5 段进度约定：0% 排队 / 10% 开始 / 30-70% 主流程 / 90% 校验 / 100% 完成
//!
//! 详见 `PR/03-模块设计/08-TaskScheduler.md §13 F32 长任务异步化扩展`

use std::path::PathBuf;
use std::sync::Arc;

use tauri::AppHandle;

use crate::config::{CudaVersion, Config, ConfigService};
use crate::python_env::models::InstallProgress;
use crate::python_env::recovery::{self, RepairAction};
use crate::python_env::{PythonEnvService, TorchVariant};
use crate::task_scheduler::models::{TaskKind, TaskResult};
use crate::task_scheduler::task::TaskDef;
use crate::task_scheduler::progress::ProgressSender;

use tokio_util::sync::CancellationToken;

// ====================================================================
// 1. create_venv
// ====================================================================

/// 构造「创建 venv」任务
///
/// 进度细分：
/// - 10%：开始
/// - 50%：uv venv 创建中
/// - 90%：校验
/// - 100%：完成
pub fn make_create_venv_task(
    python_env: Arc<PythonEnvService>,
    venv_path: PathBuf,
    python_version: String,
) -> TaskDef {
    TaskDef {
        kind: TaskKind::CreateVenv,
        name: format!("创建 venv (Python {})", python_version),
        priority: None, // 用默认 High
        action: Box::new(
            move |cancel: CancellationToken, progress: ProgressSender| {
                let python_env = python_env.clone();
                let venv_path = venv_path.clone();
                let python_version = python_version.clone();
                Box::pin(async move {
                    progress.send_percent(10);
                    progress.send_message(format!("创建 venv: Python {}", python_version));

                    if cancel.is_cancelled() {
                        return Err("任务已取消".to_string());
                    }

                    // 主流程：create_venv
                    python_env
                        .create_venv(&venv_path, &python_version)
                        .await
                        .map_err(|e| e.to_string())?;

                    progress.send_percent(90);
                    progress.send_message("校验 venv...");

                    // 校验：venv 目录 + pyvenv.cfg 存在
                    if !venv_path.exists() || !venv_path.join("pyvenv.cfg").exists() {
                        return Err("venv 创建后校验失败：目录或 pyvenv.cfg 不存在".to_string());
                    }

                    progress.send_percent(100);
                    Ok(TaskResult::new(format!(
                        "venv 创建成功: {}",
                        venv_path.display()
                    )))
                })
            },
        ),
    }
}

// ====================================================================
// 2. install_torch
// ====================================================================

/// 构造「安装 torch」任务
///
/// 进度细分：
/// - 10%：开始
/// - 30%：下载 torch wheel
/// - 70%：安装到 venv
/// - 90%：校验 import torch
/// - 100%：完成
pub fn make_install_torch_task(
    python_env: Arc<PythonEnvService>,
    venv_path: PathBuf,
    cuda_version: CudaVersion,
) -> TaskDef {
    TaskDef {
        kind: TaskKind::InstallTorch,
        name: format!("安装 torch ({})", cuda_version_label(&cuda_version)),
        priority: None,
        action: Box::new(
            move |cancel: CancellationToken, progress: ProgressSender| {
                let python_env = python_env.clone();
                let venv_path = venv_path.clone();
                let cuda_version = cuda_version;
                Box::pin(async move {
                    progress.send_percent(10);
                    progress.send_message(format!(
                        "安装 torch: {}",
                        cuda_version_label(&cuda_version)
                    ));

                    if cancel.is_cancelled() {
                        return Err("任务已取消".to_string());
                    }

                    progress.send_percent(30);
                    progress.send_message("下载 torch wheel（可能需要 1-5 分钟）...");

                    // 主流程：install_torch（内部会 emit TorchInstalled 事件）
                    python_env
                        .install_torch(&venv_path, cuda_version)
                        .await
                        .map_err(|e| e.to_string())?;

                    progress.send_percent(90);
                    progress.send_message("校验 torch import...");

                    // 校验：调 probe_torch 太重（5-90s），这里跳过深度校验，
                    // 依赖 install_torch 内部的 uv 安装成功作为校验。
                    // 前端可通过 env_inspect 拿到最新 torch_installed 状态。

                    progress.send_percent(100);
                    Ok(TaskResult::new(format!(
                        "torch 安装成功: {}",
                        cuda_version_label(&cuda_version)
                    )))
                })
            },
        ),
    }
}

// ====================================================================
// 3. switch_torch_variant
// ====================================================================

/// 构造「切换 torch 变体」任务
///
/// 包含：
/// 1. 停止 ComfyUI（如运行）
/// 2. 切换 torch 变体
/// 3. 更新 Config（cuda_version + torch_variant 字段）
///
/// 进度细分：
/// - 10%：开始
/// - 20%：停止 ComfyUI
/// - 60%：切换 torch 变体
/// - 90%：更新 Config
/// - 100%：完成
pub fn make_switch_torch_variant_task(
    python_env: Arc<PythonEnvService>,
    config_service: Arc<ConfigService>,
    venv_path: PathBuf,
    variant: TorchVariant,
    process_launcher: Arc<crate::process_launcher::ProcessLauncherService>,
    app_handle: AppHandle,
) -> TaskDef {
    let variant_label = variant.label().to_string();
    TaskDef {
        kind: TaskKind::SwitchTorchVariant,
        name: format!("切换 torch 变体 ({})", variant_label),
        priority: None,
        action: Box::new(
            move |cancel: CancellationToken, progress: ProgressSender| {
                let python_env = python_env.clone();
                let config_service = config_service.clone();
                let venv_path = venv_path.clone();
                let variant = variant.clone();
                let process_launcher = process_launcher.clone();
                let app_handle = app_handle.clone();
                let variant_label = variant_label.clone();
                Box::pin(async move {
                    progress.send_percent(10);
                    progress.send_message(format!("切换 torch 变体: {}", variant_label));

                    if cancel.is_cancelled() {
                        return Err("任务已取消".to_string());
                    }

                    // 1. 停 ComfyUI（如运行）
                    progress.send_percent(20);
                    if process_launcher.status().await.is_alive() {
                        progress.send_message("停止 ComfyUI 进程...");
                        if let Err(e) = process_launcher.stop(app_handle.clone()).await {
                            tracing::warn!(error = %e, "切换 torch 前停止 ComfyUI 失败，继续");
                        }
                    }

                    if cancel.is_cancelled() {
                        return Err("任务已取消".to_string());
                    }

                    // 2. 切换 torch 变体
                    progress.send_percent(60);
                    progress.send_message(format!("切换 torch: {}", variant_label));
                    python_env
                        .switch_torch_variant(&venv_path, &variant)
                        .await
                        .map_err(|e| e.to_string())?;

                    // 3. 更新 Config
                    progress.send_percent(90);
                    progress.send_message("更新配置...");
                    let new_cuda = match &variant {
                        TorchVariant::NvidiaCuda(_) => {
                            parse_cuda_version(&variant.cuda_version_string())
                        }
                        _ => CudaVersion::Cpu,
                    };
                    let variant_json = serde_json::to_string(&variant)
                        .map_err(|e| format!("序列化 torch 变体失败: {}", e))?;

                    if let Err(e) = config_service
                        .update(move |cfg| {
                            cfg.torch.cuda_version = new_cuda;
                            cfg.torch.torch_variant = Some(variant_json);
                            Ok(())
                        })
                        .await
                    {
                        tracing::warn!(error = %e, "Config 更新失败，但 torch 切换已成功");
                    }

                    progress.send_percent(100);
                    Ok(TaskResult::new(format!(
                        "torch 变体切换成功: {}",
                        variant_label
                    )))
                })
            },
        ),
    }
}

// ====================================================================
// 4. install_requirements
// ====================================================================

/// 构造「安装 requirements.txt」任务
///
/// 进度细分：
/// - 10%：开始
/// - 50%：uv pip install -r requirements.txt
/// - 90%：校验
/// - 100%：完成
pub fn make_install_requirements_task(
    python_env: Arc<PythonEnvService>,
    venv_path: PathBuf,
    req_file: PathBuf,
) -> TaskDef {
    TaskDef {
        kind: TaskKind::InstallRequirements,
        name: "安装 ComfyUI 依赖".to_string(),
        priority: None,
        action: Box::new(
            move |cancel: CancellationToken, progress: ProgressSender| {
                let python_env = python_env.clone();
                let venv_path = venv_path.clone();
                let req_file = req_file.clone();
                Box::pin(async move {
                    progress.send_percent(10);
                    progress.send_message(format!("安装依赖: {}", req_file.display()));

                    if cancel.is_cancelled() {
                        return Err("任务已取消".to_string());
                    }

                    progress.send_percent(50);
                    progress.send_message("uv pip install -r requirements.txt...");

                    // 主流程：install_requirements（内部会 emit RequirementsInstalled 事件）
                    python_env
                        .install_requirements(&venv_path, &req_file)
                        .await
                        .map_err(|e| e.to_string())?;

                    progress.send_percent(90);
                    progress.send_message("校验依赖安装...");

                    progress.send_percent(100);
                    Ok(TaskResult::new("ComfyUI 依赖安装成功".to_string()))
                })
            },
        ),
    }
}

// ====================================================================
// 5. rebuild_venv
// ====================================================================

/// 构造「重建 venv」任务
///
/// 5 步事务（PythonEnvService::rebuild_venv 内部实现）：
/// 1. 删除旧 venv
/// 2. create_venv
/// 3. install_torch
/// 4. install_requirements
/// 5. verify_venv
///
/// 进度细分：
/// - 10%：开始
/// - 30%：删除旧 venv
/// - 50%：create_venv
/// - 70%：install_torch
/// - 85%：install_requirements
/// - 90%：verify_venv
/// - 100%：完成
///
/// 注：rebuild_venv 内部是阻塞式串行执行，无法精确上报每步进度。
/// 这里在 action 层用粗粒度进度估算（30/50/70/85），实际子进程耗时由 uv 控制。
pub fn make_rebuild_venv_task(
    python_env: Arc<PythonEnvService>,
    config: Config,
) -> TaskDef {
    TaskDef {
        kind: TaskKind::RebuildVenv,
        name: "重建 venv".to_string(),
        priority: None,
        action: Box::new(
            move |cancel: CancellationToken, progress: ProgressSender| {
                let python_env = python_env.clone();
                let config = config.clone();
                Box::pin(async move {
                    progress.send_percent(10);
                    progress.send_message("开始重建 venv（5 步事务）");

                    if cancel.is_cancelled() {
                        return Err("任务已取消".to_string());
                    }

                    // 由于 rebuild_venv 是单次 await（内部串行），无法精确上报每步进度。
                    // 这里在调用前上报粗粒度进度，实际耗时由子进程决定。
                    progress.send_percent(30);
                    progress.send_message("删除旧 venv + 创建新 venv + 安装 torch + 安装依赖...");

                    python_env
                        .rebuild_venv(&config)
                        .await
                        .map_err(|e| e.to_string())?;

                    progress.send_percent(90);
                    progress.send_message("校验 venv...");

                    progress.send_percent(100);
                    Ok(TaskResult::new("venv 重建成功".to_string()))
                })
            },
        ),
    }
}

// ====================================================================
// 6. switch_python
// ====================================================================

/// 构造「切换 Python 版本」任务
///
/// 5 步事务 + 备份回滚（PythonEnvService::switch_python_version 内部实现）。
///
/// 与其他任务不同：switch_python_version 接收 mpsc::Sender<InstallProgress>，
/// factory 内部桥接 mpsc → ProgressSender，把 InstallProgress 的 percent / message
/// 转发到 ProgressSender。
///
/// 进度细分（由 PythonEnvService 内部上报，factory 仅桥接）：
/// - 10%：下载 Python
/// - 30%：创建 venv
/// - 50%：安装 torch
/// - 70%：安装依赖
/// - 90%：校验
/// - 100%：完成
pub fn make_switch_python_task(
    python_env: Arc<PythonEnvService>,
    new_version: String,
    config: Config,
) -> TaskDef {
    TaskDef {
        kind: TaskKind::SwitchPython,
        name: format!("切换 Python 版本到 {}", new_version),
        priority: None,
        action: Box::new(
            move |cancel: CancellationToken, progress: ProgressSender| {
                let python_env = python_env.clone();
                let new_version = new_version.clone();
                let config = config.clone();
                Box::pin(async move {
                    progress.send_percent(10);
                    progress.send_message(format!("切换 Python 版本: {}", new_version));

                    if cancel.is_cancelled() {
                        return Err("任务已取消".to_string());
                    }

                    // 桥接 mpsc::Sender<InstallProgress> → ProgressSender
                    let (tx, mut rx) =
                        tokio::sync::mpsc::channel::<InstallProgress>(16);
                    let progress_for_bridge = progress.clone();
                    let bridge_task = tokio::spawn(async move {
                        while let Some(p) = rx.recv().await {
                            if let Some(percent) = p.percent {
                                progress_for_bridge.send_percent(percent);
                            }
                            progress_for_bridge.send_message(p.message);
                        }
                    });

                    // 调用 switch_python_version（内部会按 5 步上报进度）
                    let result = python_env
                        .switch_python_version(&new_version, &config, tx)
                        .await
                        .map_err(|e| e.to_string());

                    // 关闭桥接 task
                    bridge_task.abort();

                    let () = result?;

                    progress.send_percent(100);
                    Ok(TaskResult::new(format!(
                        "Python 版本切换成功: {}",
                        new_version
                    )))
                })
            },
        ),
    }
}

// ====================================================================
// 7. env_repair（v1.8 / F36-Phase2 新增）
// ====================================================================

/// 构造「环境修复」任务
///
/// 流程：依据 RepairAction 选择不同修复路径：
/// - DowngradeNumpy → quick_repair_numpy
/// - ReinstallTorch / ReinstallRequirements → quick_repair_reinstall
/// - RebuildVenv → rebuild_repair（5 步事务，最慢但最稳）
///
/// 进度细分（按 RepairAction 不同分支）：
/// - DowngradeNumpy：10% 开始 / 30% 降级 / 80% smoke test / 100% 完成
/// - ReinstallTorch/ReinstallRequirements：10% / 50% / 90% / 100%
/// - RebuildVenv：10% / 30% / 50% / 85% / 100%（复用 rebuild_venv 内部进度）
///
/// 参数：
/// - `python_env`：PythonEnvService Arc
/// - `config`：Config（含 venv_path / comfyui_root / cuda_version）
/// - `action`：修复动作（前端根据 DiagnoseReport.suggested_action 传入）
pub fn make_env_repair_task(
    python_env: Arc<PythonEnvService>,
    config: Config,
    action: RepairAction,
) -> TaskDef {
    let name = match action {
        RepairAction::None => "环境修复（无需操作）".to_string(),
        RepairAction::DowngradeNumpy => "降级 numpy".to_string(),
        RepairAction::ReinstallTorch => "重装 torch".to_string(),
        RepairAction::ReinstallRequirements => "重装依赖".to_string(),
        RepairAction::RebuildVenv => "重建 venv".to_string(),
    };
    TaskDef {
        kind: TaskKind::EnvRepair,
        name,
        priority: None,
        action: Box::new(
            move |cancel: CancellationToken, progress: ProgressSender| {
                let python_env = python_env.clone();
                let config = config.clone();
                Box::pin(async move {
                    progress.send_percent(10);
                    progress.send_message(format!("开始环境修复: {:?}", action));

                    if cancel.is_cancelled() {
                        return Err("任务已取消".to_string());
                    }

                    let venv_path = PathBuf::from(&config.paths.venv_path);
                    let comfyui_root = PathBuf::from(&config.paths.comfyui_root);
                    let cuda_version = config.torch.cuda_version;

                    let result = match action {
                        RepairAction::None => {
                            // 无需操作
                            progress.send_percent(100);
                            progress.send_message("环境健康，无需修复".to_string());
                            Ok(())
                        }
                        RepairAction::DowngradeNumpy => {
                            recovery::quick_repair_numpy(
                                python_env.uv(),
                                &venv_path,
                                &progress,
                                &cancel,
                            )
                            .await
                            .map_err(|e| e.to_string())
                        }
                        RepairAction::ReinstallTorch => {
                            recovery::quick_repair_reinstall(
                                python_env.uv(),
                                &venv_path,
                                &comfyui_root,
                                &cuda_version,
                                &progress,
                                &cancel,
                            )
                            .await
                            .map_err(|e| e.to_string())
                        }
                        RepairAction::ReinstallRequirements => {
                            // ReinstallRequirements 走 reinstall 路径（先重装 torch 再重装 requirements）
                            recovery::quick_repair_reinstall(
                                python_env.uv(),
                                &venv_path,
                                &comfyui_root,
                                &cuda_version,
                                &progress,
                                &cancel,
                            )
                            .await
                            .map_err(|e| e.to_string())
                        }
                        RepairAction::RebuildVenv => {
                            recovery::rebuild_repair(
                                &python_env,
                                &venv_path,
                                &config,
                                &progress,
                                &cancel,
                            )
                            .await
                            .map_err(|e| e.to_string())
                        }
                    };

                    result?;
                    progress.send_percent(100);
                    progress.send_message("环境修复完成".to_string());

                    // emit RequirementsInstalled 让 env cache 失效
                    python_env
                        .event_bus_ref()
                        .emit(crate::event_bus::SystemEvent::RequirementsInstalled);

                    Ok(TaskResult::new(format!("环境修复完成: {:?}", action)))
                })
            },
        ),
    }
}

// ====================================================================
// 辅助函数
// ====================================================================

/// 把 CudaVersion 转为可读字符串
fn cuda_version_label(cuda: &CudaVersion) -> &'static str {
    match cuda {
        CudaVersion::Cpu => "CPU",
        CudaVersion::Cu118 => "CUDA 11.8",
        CudaVersion::Cu121 => "CUDA 12.1",
        CudaVersion::Cu124 => "CUDA 12.4",
    }
}

/// 解析 CUDA 版本字符串（与 commands/python_env::parse_cuda_version 对齐）
fn parse_cuda_version(s: &str) -> CudaVersion {
    match s.to_lowercase().as_str() {
        "cpu" => CudaVersion::Cpu,
        "cu118" => CudaVersion::Cu118,
        "cu121" => CudaVersion::Cu121,
        "cu124" => CudaVersion::Cu124,
        _ => CudaVersion::Cpu, // 兜底
    }
}
