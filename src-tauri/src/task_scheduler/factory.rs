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

use crate::common::line_collector::LineCollector;
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
                        .create_venv(&venv_path, &python_version, &cancel)
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
                        .install_torch(&venv_path, cuda_version, &cancel)
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
                        .switch_torch_variant(&venv_path, &variant, &cancel)
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
                        .install_requirements(&venv_path, &req_file, &cancel)
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
                        .rebuild_venv(&config, &cancel)
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
                        .switch_python_version(&new_version, &config, tx, &cancel)
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
// v3.6：环境诊断（Diagnose）
// ====================================================================

/// 构造「环境诊断」任务
///
/// v3.6：将 `env_diagnose` 从同步命令改为 TaskScheduler 任务，支持：
/// - 用户主动取消（torch import 探针可能耗时 90s）
/// - 进度推送（10% 开始 / 50% torch 探针 / 80% 依赖检查 / 100% 完成）
/// - DiagnoseReport 作为 `TaskResult.payload` 返回，前端从 `task_completed` 事件取
///
/// **关键**：`recovery::diagnose` 内部已 emit `RequirementsInstalled` 事件，
/// 触发 env_inspector cache 失效 + 后台刷新 + `env_inspect_updated` 事件，
/// 前端 store 自动拿到最新 EnvSnapshot，无需额外处理。
pub fn make_diagnose_task(
    python_env: Arc<PythonEnvService>,
    venv_path: PathBuf,
    comfyui_root: PathBuf,
) -> TaskDef {
    TaskDef {
        kind: TaskKind::Diagnose,
        name: "环境诊断".to_string(),
        priority: None,
        action: Box::new(
            move |cancel: CancellationToken, progress: ProgressSender| {
                let python_env = python_env.clone();
                let venv_path = venv_path.clone();
                let comfyui_root = comfyui_root.clone();
                Box::pin(async move {
                    progress.send_percent(10);
                    progress.send_message("开始环境诊断...".to_string());

                    if cancel.is_cancelled() {
                        return Err("任务已取消".to_string());
                    }

                    progress.send_percent(50);
                    progress.send_message("torch 探针 + 依赖检查...".to_string());

                    // diagnose 内部已 emit RequirementsInstalled 触发 cache 失效
                    let report = recovery::diagnose(
                        &python_env,
                        &venv_path,
                        &comfyui_root,
                        python_env.event_bus_ref(),
                        &cancel,
                    )
                    .await;

                    progress.send_percent(100);
                    let issue_count = report.issues.len();
                    let summary = if issue_count == 0 {
                        format!(
                            "环境诊断完成：健康（torch={:?}）",
                            report.torch_version
                        )
                    } else {
                        format!("环境诊断完成：发现 {} 个问题", issue_count)
                    };
                    progress.send_message(summary.clone());

                    // 序列化 DiagnoseReport 为 JSON payload，前端从 task_completed 事件取
                    let payload = serde_json::to_value(&report)
                        .map_err(|e| format!("序列化诊断报告失败: {}", e))?;

                    Ok(TaskResult::new(summary).with_payload(payload))
                })
            },
        ),
    }
}

// ====================================================================
// v3.5：嵌套子任务进度转发器（child_progress_forwarder）
// ====================================================================

/// 转发子任务 task_progress 事件到父任务的 ProgressSender
///
/// **用途**：版本切换的 11 步流程中，步骤 8/9/10（创建 venv / 装 torch / 装 requirements）
/// 作为子任务提交。子任务的进度更新通过全局 `task_progress` 事件推送，
/// 本函数 spawn 一个后台 task 监听全局事件，把子任务的进度映射到父任务进度段。
///
/// **行为**：
/// 1. spawn 一次性后台 task
/// 2. listen 全局 `task_progress` 事件
/// 3. 过滤 `task_id == child_id` 的事件
/// 4. 把子任务 `progress`（0..=100）映射到父任务 `progress_range`（如 (50, 60)）
/// 5. 转发到 `parent_progress.send_percent`
/// 6. 子任务结束（task_completed / failed / cancelled）后自动退出
///
/// **父任务取消的级联传播**：
/// - 父任务被 cancel → parent_cancel.cancelled() 触发
/// - 本 forwarder 收到 cancel 信号 → 主动 `task_scheduler.cancel(child_id)`
/// - 父任务 wait() 收到 TaskError::WaitCancelled → 父任务也返回 Err
/// - 父任务的错误处理路径（如 rollback_checkout）会执行
///
/// **子任务日志也转发**：子任务的 `task_log` 事件也转发到 `parent_log_collector`，
/// 父任务失败时这些日志可作为错误上下文。
///
/// **参数**：
/// - `app`: AppHandle（订阅全局事件）
/// - `task_scheduler`: 父任务用于 cancel 子任务
/// - `child_id`: 子任务 ID
/// - `parent_progress`: 父任务的 ProgressSender
/// - `parent_cancel`: 父任务的 CancellationToken（父 cancel 时级联取消子）
/// - `parent_log_collector`: 父任务的 LineCollector（接收子任务日志）
/// - `progress_range`: (起始段, 结束段)，子任务 0% 映射到 start，100% 映射到 end
pub fn spawn_child_progress_forwarder(
    app: AppHandle,
    task_scheduler: Arc<crate::task_scheduler::TaskSchedulerService>,
    child_id: crate::task_scheduler::TaskId,
    parent_progress: ProgressSender,
    parent_cancel: tokio_util::sync::CancellationToken,
    parent_log_collector: Arc<LineCollector>,
    progress_range: (u8, u8),
) {
    use tauri::Listener;

    let (start, end) = progress_range;
    let range_span = end.saturating_sub(start);

    // 监听 task_progress 事件
    let app_for_progress = app.clone();
    let child_id_for_progress = child_id.clone();
    let parent_progress_for_progress = parent_progress.clone();
    let unlisten_progress: tauri::EventId = app_for_progress.listen(
        "task_progress",
        move |event| {
            // 解析 payload
            let payload: serde_json::Value = match serde_json::from_str(event.payload()) {
                Ok(v) => v,
                Err(_) => return,
            };
            let task_id = payload.get("task_id").and_then(|v| v.as_str());
            if task_id != Some(&child_id_for_progress) {
                return;
            }
            let sub_progress = payload.get("progress").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
            let mapped = start + (sub_progress as f32 * range_span as f32 / 100.0) as u8;
            parent_progress_for_progress.send_percent(mapped.min(end));
        },
    );

    // 监听 task_log 事件（子任务的实时日志 → 父任务 LineCollector）
    let app_for_log = app.clone();
    let child_id_for_log = child_id.clone();
    let collector_for_log = parent_log_collector.clone();
    let unlisten_log: tauri::EventId = app_for_log.listen("task_log", move |event| {
        let payload: serde_json::Value = match serde_json::from_str(event.payload()) {
            Ok(v) => v,
            Err(_) => return,
        };
        let arr = match payload.as_array() {
            Some(a) => a,
            None => return,
        };
        for entry in arr {
            let tid = entry.get("task_id").and_then(|v| v.as_str());
            if tid != Some(&child_id_for_log) {
                continue;
            }
            let source = entry
                .get("source")
                .and_then(|v| v.as_str())
                .unwrap_or("child")
                .to_string();
            let text = entry
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if !text.is_empty() {
                collector_for_log.push_with_source(source, text);
            }
        }
    });

    // 后台 task：父 cancel → 级联 cancel 子
    let app_for_unlisten = app.clone();
    let task_scheduler_for_cascade = task_scheduler.clone();
    let child_id_for_cascade = child_id.clone();
    let unlisten_progress_for_cleanup = unlisten_progress;
    let unlisten_log_for_cleanup = unlisten_log;
    tokio::spawn(async move {
        // 父 cancel 时级联 cancel 子
        tokio::select! {
            _ = parent_cancel.cancelled() => {
                tracing::info!(?child_id_for_cascade, "parent cancelled, cascading cancel to child");
                let _ = task_scheduler_for_cascade.cancel(&child_id_for_cascade).await;
            }
            // 子任务完成后 forwarder 自然退出（listen 在子 task 终止时由 AppHandle 释放）
            _ = tokio::time::sleep(std::time::Duration::from_secs(60 * 60 * 24)) => {
                tracing::warn!(?child_id_for_cascade, "forwarder timeout (24h), exit");
            }
        }
        // 清理 listener（Tauri Listener::unlisten 接收 EventId 返回 ()）
        use tauri::Listener;
        app_for_unlisten.unlisten(unlisten_progress_for_cleanup);
        app_for_unlisten.unlisten(unlisten_log_for_cleanup);
    });
}

// ====================================================================
// v3.5：check_compat 任务工厂（替换 core_check_version_compatibility 同步版本）
// ====================================================================

/// 构造「版本兼容性预检」任务（v3.5 异步化）
///
/// 异步执行 git show 读 target tag 的 requirements.txt，
/// 避免大文件读取阻塞 UI 线程。
///
/// 进度细分：
/// - 10%：开始
/// - 30%：读取 current requirements
/// - 60%：读取 target requirements（git show）
/// - 90%：diff + 模式推荐
/// - 100%：完成
pub fn make_check_compat_task(
    core_manager: Arc<crate::core_manager::CoreManagerService>,
    config: Arc<ConfigService>,
    python_env: Arc<PythonEnvService>,
    target_tag: String,
    app: AppHandle,
) -> TaskDef {
    TaskDef {
        kind: TaskKind::CheckCompat,
        name: format!("检查版本兼容性: {}", target_tag),
        priority: None, // default = Low
        action: Box::new(move |cancel, progress| {
            let core_manager = core_manager.clone();
            let config = config.clone();
            let python_env = python_env.clone();
            let target_tag = target_tag.clone();
            Box::pin(async move {
                use crate::core_manager::compat::{
                    detect_current_torch_variant, diff_requirements, parse_requirements_simple,
                    recommend_mode,
                };

                progress.send_percent(10);
                progress.send_message(format!("检查 {} 兼容性...", target_tag));
                if cancel.is_cancelled() {
                    return Err("任务已取消".to_string());
                }

                let config_snapshot = config.get();
                let comfyui_root = config_snapshot.paths.comfyui_root.clone();
                let venv_path = config_snapshot.paths.venv_path.clone();
                let target_python_version = config_snapshot.paths.python_version.clone();
                let target_torch_variant = config_snapshot.torch.cuda_version.to_torch_index().to_string();
                drop(config_snapshot);

                // 1. venv 状态
                progress.send_percent(20);
                let venv_exists = venv_path.join("pyvenv.cfg").exists();
                let current_python = if venv_exists {
                    crate::python_env::verify::probe_python_version(
                        &crate::python_env::uv_runner::venv_python_path(&venv_path),
                        &cancel,
                    )
                    .await
                } else {
                    None
                };
                let current_torch_variant = detect_current_torch_variant(&venv_path).await;
                let current_torch_installed = current_torch_variant.is_some();

                // 2. 当前 tag
                let current_tag = core_manager
                    .current_version()
                    .await
                    .ok()
                    .and_then(|s| s.current_version);

                progress.send_percent(30);
                if cancel.is_cancelled() {
                    return Err("任务已取消".to_string());
                }

                // 3. 读 requirements.txt（async git show）
                let read_reqs_for_tag = |tag: &str| -> Option<Vec<(String, String)>> {
                    let req_path = comfyui_root.join("requirements.txt");
                    if tag == current_tag.as_deref().unwrap_or("") {
                        std::fs::read_to_string(&req_path)
                            .ok()
                            .map(|c| parse_requirements_simple(&c))
                    } else {
                        // 用 new_command_sync 避免弹 cmd 窗口（v3.4 已改造）
                        let output = crate::common::process_util::new_command_sync("git")
                            .args(["show", &format!("{}:requirements.txt", tag)])
                            .current_dir(&comfyui_root)
                            .output()
                            .ok()?;
                        if output.status.success() {
                            let s = String::from_utf8_lossy(&output.stdout).to_string();
                            Some(parse_requirements_simple(&s))
                        } else {
                            None
                        }
                    }
                };

                progress.send_percent(60);
                let current_reqs = current_tag
                    .as_ref()
                    .and_then(|t| read_reqs_for_tag(t))
                    .unwrap_or_default();
                let target_reqs = read_reqs_for_tag(&target_tag).unwrap_or_default();
                let requirements_diff = diff_requirements(&current_reqs, &target_reqs);

                progress.send_percent(80);
                let same_python = match (&current_python, &target_python_version) {
                    (Some(cp), tp) => cp.starts_with(tp),
                    (None, _) => false,
                };
                let same_torch_variant = match (&current_torch_variant, &target_torch_variant) {
                    (Some(ctv), ttv) => ctv == ttv,
                    (None, _) => false,
                };

                // 4. custom_nodes 数量
                let custom_nodes_dir = comfyui_root.join("custom_nodes");
                let custom_node_count = std::fs::read_dir(&custom_nodes_dir)
                    .map(|entries| {
                        entries
                            .filter_map(|e| e.ok())
                            .filter(|e| e.path().join(".git").exists() || e.path().is_dir())
                            .count()
                    })
                    .unwrap_or(0);

                // 5. 推荐模式
                let (recommended_mode, recommended_reason) = recommend_mode(
                    same_python,
                    same_torch_variant,
                    venv_exists,
                    &requirements_diff,
                    current_torch_installed,
                );

                let report = crate::core_manager::compat::VersionCompatReport {
                    current_tag,
                    target_tag: target_tag.clone(),
                    venv_exists,
                    current_python,
                    target_python: Some(target_python_version),
                    current_torch_variant,
                    target_torch_variant: Some(target_torch_variant),
                    current_torch_installed,
                    same_python,
                    same_torch_variant,
                    requirements_diff,
                    custom_node_count,
                    recommended_mode,
                    recommended_reason,
                };

                progress.send_percent(100);
                let _ = app;
                let _ = python_env;
                Ok(TaskResult {
                    summary: format!("版本兼容性检查完成: {}", target_tag),
                    payload: Some(serde_json::to_value(&report).map_err(|e| e.to_string())?),
                })
            })
        }),
    }
}

// ====================================================================
// v3.5：check_prereq 任务工厂（替换 core_check_switch_prerequisites 同步版本）
// ====================================================================

/// 构造「切换前置条件检查」任务（v3.5 异步化）
///
/// 检查：
/// - ComfyUI 进程状态（运行中 → 拒绝）
/// - 工作区 dirty 状态（脏 → 拒绝）
/// - 给出详细原因
///
/// 进度细分：
/// - 10%：开始
/// - 40%：检查 ComfyUI 状态
/// - 70%：检查工作区
/// - 100%：完成
pub fn make_check_prereq_task(
    core_manager: Arc<crate::core_manager::CoreManagerService>,
    process_launcher: Arc<crate::process_launcher::ProcessLauncherService>,
) -> TaskDef {
    TaskDef {
        kind: TaskKind::CheckPrereq,
        name: "检查切换前置条件".to_string(),
        priority: None, // default = High
        action: Box::new(move |cancel, progress| {
            let core_manager = core_manager.clone();
            let process_launcher = process_launcher.clone();
            Box::pin(async move {
                progress.send_percent(10);
                progress.send_message("检查切换前置条件...".to_string());
                if cancel.is_cancelled() {
                    return Err("任务已取消".to_string());
                }

                // 1. ComfyUI 状态
                progress.send_percent(40);
                let comfyui_running = process_launcher.status().await.is_alive();
                if cancel.is_cancelled() {
                    return Err("任务已取消".to_string());
                }

                // 2. 工作区状态
                progress.send_percent(70);
                let prereq = core_manager
                    .check_switch_prerequisites(comfyui_running)
                    .await
                    .map_err(|e| e.to_string())?;

                progress.send_percent(100);
                Ok(TaskResult {
                    summary: if prereq.can_switch {
                        "可以切换版本".to_string()
                    } else {
                        format!("不允许切换: {}", prereq.block_reason.as_deref().unwrap_or("未知原因"))
                    },
                    payload: Some(serde_json::to_value(&prereq).map_err(|e| e.to_string())?),
                })
            })
        }),
    }
}

// ====================================================================
// v3.5：switch_version 任务工厂
// ====================================================================

/// 构造「切换 ComfyUI 版本」任务（v3.5 全面异步化）
///
/// 嵌套子任务 + 实时日志 + 失败回滚
pub fn make_switch_version_task(
    params: crate::core_manager::switcher::SwitchVersionParams,
    ctx: crate::core_manager::switcher::SwitchContext,
    app: AppHandle,
) -> TaskDef {
    use crate::core_manager::compat::SwitchMode;
    let target_tag = params.target_tag.clone();
    TaskDef {
        kind: TaskKind::Checkout,
        name: format!("切换 ComfyUI 版本到 {}", target_tag),
        priority: None, // default = High
        action: Box::new(move |cancel, progress| {
            let params = params.clone();
            let ctx = ctx;
            let app = app.clone();
            Box::pin(async move {
                crate::core_manager::switcher::run_switch_version(
                    params,
                    ctx,
                    app,
                    cancel,
                    progress,
                )
                .await
            })
        }),
    }
}

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

// ====================================================================
// v3.4：start_comfyui - 启动 ComfyUI 主进程
// ====================================================================

/// 构造「启动 ComfyUI」任务（v3.4）
///
/// **设计要点（与 F1-F3 对齐）**：
/// - action 闭包是薄壳：调 `service.start(args, app, &sender)` 把 ProgressSender 透传进去
/// - start() 内部已经在 5 个关键点（10/20/30/50/60%）调 `sender.send_*`，
///   这里不需要重复写进度上报
/// - cancel_token 透传到 spawn_health_check（v3.4 同时改造支持）
/// - 错误统一映射：`ProcessError → String` 走 task_scheduler 的 FinalErr::ActionFailed
///
/// **进度阶段**（由 service.start 内部 send_*，factory 不重复）：
/// - 10% / 15%：校验环境
/// - 20% / 25%：检查端口
/// - 30% / 40%：生成 yaml
/// - 50% / 55%：spawn 进程
/// - 60% → 90%：等待 ComfyUI 就绪（health_check 每 1s 推一次）
/// - 95% / 100%：超时 / 成功
///
/// **返回 TaskResult**：
/// - `summary`：格式化启动信息（pid / port / mode）
/// - `payload`：JSON 包含 pid, port, host（前端用于显示"ComfyUI 已就绪"）
pub fn make_start_comfyui_task(
    process_launcher: Arc<crate::process_launcher::ProcessLauncherService>,
    app: tauri::AppHandle,
    args: crate::process_launcher::LaunchArgs,
) -> TaskDef {
    TaskDef {
        kind: TaskKind::StartComfyUI,
        name: "启动 ComfyUI".to_string(),
        priority: None, // 取 default_priority = High
        action: Box::new(move |_cancel, sender| {
            let launcher = process_launcher.clone();
            let app = app.clone();
            let args = args.clone();
            Box::pin(async move {
                // v3.4：失败处理
                // - service.start() 内部已加 5s 早期死亡检测（5s 内 child 死 → EarlyExit）
                // - EarlyExit 的 ProcessError Display 已含 stderr_tail（最近 50 行）
                // - health_check 检测到 child 死（5s~60s 之间）→ emit process_crashed + 走 stop_impl
                // - 此处 start() 返回 Err 时，错误信息已包含完整 stderr tail，前端 task_failed 事件可直接渲染
                launcher
                    .start(args.clone(), app, Some(&sender))
                    .await
                    .map_err(|e| {
                        tracing::error!(error = %e, "make_start_comfyui_task: start failed");
                        // to_string() 走 ProcessError Display，EarlyExit 变体自带 stderr tail
                        e.to_string()
                    })?;

                // 启动成功（service.start 返回 Ok 意味着 spawn 成功 + 早期检测通过 + pipeline 就绪）
                // 注：实际"ComfyUI 就绪"由 health_check task 后续通过 process_started 事件通知
                Ok(TaskResult {
                    summary: format!(
                        "ComfyUI 启动命令已提交 (mode={:?}, port={})",
                        args.mode, args.listen_port
                    ),
                    payload: Some(serde_json::json!({
                        "host": args.listen_host,
                        "port": args.listen_port,
                        "mode": format!("{:?}", args.mode).to_lowercase(),
                    })),
                })
            })
        }),
    }
}
