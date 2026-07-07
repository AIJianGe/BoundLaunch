//! 环境修复模块（v1.8 / F36-Phase2）
//!
//! ## 背景
//!
//! "一键补装"失败后用户会卡在"已装但显示未装"的状态。`scripts.rs` 探针增强
//! 让用户能看到 `error_type` / `error_msg`，但**光看没用，还得能修**。
//!
//! 本模块提供 3 类修复：
//! 1. **诊断**（`diagnose`）：扫描 venv + 关键依赖 + torch import，列出所有问题
//! 2. **轻量修复**（`quick_repair`）：不重建 venv，尝试修常见问题
//!    - numpy 2.4.4 → 2.2.x 降级（已知坏版本）
//!    - 关键依赖（torch / torchvision / torchaudio）重装
//! 3. **重建修复**（`rebuild_repair`）：删 venv 重建（最稳但最慢）
//!
//! ## 调用流程
//!
//! ```text
//! 前端 StatusCard 显示 torch 未安装
//!     ↓
//! 点击 "诊断" 按钮
//!     ↓
//! 调 env_diagnose 命令 → diagnose()
//!     ↓
//! 返回 DiagnoseReport { issues: [...], suggested_action: "QuickRepair" | "RebuildVenv" }
//!     ↓
//! 用户点 "一键修复"
//!     ↓
//! 调 env_repair(repair_type) → quick_repair() / rebuild_repair()
//!     ↓
//! 返回 task_id，长任务异步执行
//! ```
//!
//! 详见 `PR/03-模块设计/02-PythonEnvManager.md §16 环境修复`

use std::path::Path;

use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::config::{Config, CudaVersion};
use crate::error::EnvError;
use crate::event_bus::EventBus;
use crate::python_env::freeze;
use crate::python_env::uv_runner::UvRunner;
use crate::task_scheduler::progress::ProgressSender;

use super::PythonEnvService;

/// 单个诊断问题
#[derive(Debug, Clone, Serialize)]
pub struct Issue {
    /// 问题严重度
    pub severity: IssueSeverity,
    /// 问题代码（前端国际化用）
    pub code: String,
    /// 用户可读描述
    pub message: String,
    /// 详情（错误消息、traceback 等）
    pub detail: Option<String>,
    /// 建议的修复动作
    pub suggested_action: RepairAction,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IssueSeverity {
    /// 信息（如"建议升级"）
    Info,
    /// 警告（如"numpy 2.4.4 已知问题"）
    Warning,
    /// 错误（如"torch 完全无法 import"）
    Error,
    /// 严重错误（如"venv 不可写"）
    Critical,
}

/// 修复动作
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepairAction {
    /// 无需修复
    None,
    /// 降级 numpy（已知坏版本）
    DowngradeNumpy,
    /// 重装 torch
    ReinstallTorch,
    /// 重装所有 requirements
    ReinstallRequirements,
    /// 重建 venv（删 + 重建 + 装 torch + 装 requirements）
    RebuildVenv,
}

/// 完整诊断报告
#[derive(Debug, Clone, Serialize)]
pub struct DiagnoseReport {
    /// venv 是否存在
    pub venv_exists: bool,
    /// torch 是否能 import
    pub torch_import_ok: bool,
    /// torch 版本（import 成功时）
    pub torch_version: Option<String>,
    /// 各问题列表
    pub issues: Vec<Issue>,
    /// 综合建议（最严重的 Action）
    pub suggested_action: RepairAction,
    /// 建议原因（用户可读）
    pub suggested_reason: String,
}

impl DiagnoseReport {
    /// 综合所有 issue 取最严重 action
    fn compute_suggested_action(issues: &[Issue]) -> (RepairAction, String) {
        if issues.is_empty() {
            return (
                RepairAction::None,
                "环境健康，无需修复".to_string(),
            );
        }
        // 优先 RebuildVenv（最稳），其次 DowngradeNumpy（轻量）
        let has_critical = issues
            .iter()
            .any(|i| matches!(i.severity, IssueSeverity::Critical | IssueSeverity::Error));
        if has_critical {
            return (
                RepairAction::RebuildVenv,
                "检测到严重问题，建议重建 venv 一次性修复所有问题".to_string(),
            );
        }
        // 警告级问题
        let has_numpy_issue = issues
            .iter()
            .any(|i| i.code.starts_with("numpy."));
        if has_numpy_issue {
            return (
                RepairAction::DowngradeNumpy,
                "检测到 numpy 已知坏版本，建议降级".to_string(),
            );
        }
        // 兜底
        (
            RepairAction::ReinstallRequirements,
            "检测到依赖问题，建议重装".to_string(),
        )
    }
}

/// 诊断 venv + torch + 关键依赖
///
/// **不会修改任何状态**，纯只读探测。前端可以无副作用地反复调用。
///
/// `python_env` 参数：v1.8 预留给需要访问 PythonEnvService 状态的高级诊断
/// （如检查 install_lock）。当前实现只用 venv_path / comfyui_root，
/// 保留参数用于未来扩展（如"上次安装是否失败"状态查询）。
///
/// **v1.8 / F36-Phase2 重要修复**：函数末尾 emit `RequirementsInstalled` 事件。
/// 原因：diagnose 可能"治愈"了 env cache 的 stale 问题（例如 torch 之前被误判为未装，
/// 现在 probe 看到 import 成功）。前端 `env_store.subscribe` 监听 `env_inspect_updated`
/// → 拿到最新 EnvSnapshot → `torchBroken` 告警条立即消失，避免「检测说没问题但 UI 仍告警」的
/// "幽灵告警"问题。
///
/// 为什么用 `RequirementsInstalled` 而不是新加事件：
/// - `EnvironmentInspectorService` 已经订阅了 `RequirementsInstalled`
/// - 内部走 `spawn_refresh` → emit Tauri Event `env_inspect_updated` → 前端自动更新 envInfo
/// - 复用现有事件链，不增加新事件类型
///
/// venv 不存在时**不**emit（避免无意义的 cache 失效，下一次 envInspect 也会自然刷新）。
#[allow(unused_variables)]
pub async fn diagnose(
    python_env: &PythonEnvService,
    venv_path: &Path,
    comfyui_root: &Path,
    event_bus: &EventBus,
    cancel: &CancellationToken,
) -> DiagnoseReport {
    let mut issues = Vec::new();

    // 1. venv 是否存在
    let venv_exists = venv_path.join("pyvenv.cfg").exists();
    if !venv_exists {
        issues.push(Issue {
            severity: IssueSeverity::Critical,
            code: "venv.missing".to_string(),
            message: "venv 目录不存在".to_string(),
            detail: Some(format!("路径: {}", venv_path.display())),
            suggested_action: RepairAction::RebuildVenv,
        });
        // venv 都没有 → emit 也没意义，env cache 失效后 inspect 也会失败
        return DiagnoseReport {
            venv_exists,
            torch_import_ok: false,
            torch_version: None,
            issues,
            suggested_action: RepairAction::RebuildVenv,
            suggested_reason: "venv 不存在，必须重建".to_string(),
        };
    }

    // 2. torch 探针（带错误详情）
    let torch_result = crate::env_inspector::scripts::probe_torch_script(venv_path, cancel).await;
    let (torch_import_ok, torch_version, error_info) = match torch_result {
        Ok(json) => {
            let probe = crate::env_inspector::scripts::parse_torch_probe(&json);
            (
                probe.installed,
                probe.version.clone(),
                if !probe.installed {
                    Some((
                        probe.error_type.unwrap_or_else(|| "Unknown".to_string()),
                        probe.error_msg.unwrap_or_else(|| "(无)".to_string()),
                        probe.traceback_tail.unwrap_or_default(),
                    ))
                } else {
                    None
                },
            )
        }
        Err(e) => (false, None, Some(("ProbeError".to_string(), e.to_string(), String::new()))),
    };

    if let Some((etype, emsg, tb)) = error_info {
        issues.push(Issue {
            severity: IssueSeverity::Error,
            code: format!("torch.import.{}", etype.to_lowercase()),
            message: format!("torch import 失败: {}", emsg),
            detail: Some(if !tb.is_empty() {
                format!("traceback 末尾: {}", tb)
            } else {
                format!("错误类型: {}", etype)
            }),
            suggested_action: RepairAction::RebuildVenv,
        });
    }

    // 3. 检查 numpy 版本（如果 torch import 失败）
    if !torch_import_ok {
        if let Ok(numpy_info) = check_numpy_version(venv_path).await {
            // numpy 2.4.4 已知坏版本
            if let Some(ver) = &numpy_info {
                if is_numpy_known_bad(ver) {
                    issues.push(Issue {
                        severity: IssueSeverity::Warning,
                        code: "numpy.known_bad".to_string(),
                        message: format!("numpy {} 是已知坏版本（缺 exceptions.py）", ver),
                        detail: Some(
                            "建议降级到 numpy 2.2.6（实测稳定）".to_string(),
                        ),
                        suggested_action: RepairAction::DowngradeNumpy,
                    });
                }
            }
        }
    }

    // 4. 检查关键依赖（torch / torchvision / torchaudio）
    let critical_deps = ["torch", "torchvision", "torchaudio"];
    if torch_import_ok {
        // torch 能 import → 检查 torchaudio 是否一致
        if let Ok(versions) = check_packages(venv_path, &critical_deps, cancel).await {
            for (pkg, ver_opt) in versions {
                if ver_opt.is_none() {
                    issues.push(Issue {
                        severity: IssueSeverity::Warning,
                        code: format!("deps.missing.{}", pkg),
                        message: format!("{} 未安装", pkg),
                        detail: None,
                        suggested_action: RepairAction::ReinstallRequirements,
                    });
                }
            }
        }
    }

    // 5. requirements.txt 是否都装好
    if comfyui_root.join("requirements.txt").exists() {
        if let Ok(report) = check_requirements_match(venv_path, comfyui_root, cancel).await {
            if !report.all_ok {
                issues.push(Issue {
                    severity: IssueSeverity::Warning,
                    code: "deps.requirements_mismatch".to_string(),
                    message: format!(
                        "{} 个依赖版本不满足 ComfyUI requirements.txt",
                        report.mismatched.len()
                    ),
                    detail: Some(report.mismatched.join(", ")),
                    suggested_action: RepairAction::ReinstallRequirements,
                });
            }
        }
    }

    let (suggested_action, suggested_reason) = DiagnoseReport::compute_suggested_action(&issues);

    // v1.8 / F36-Phase2 修复：diagnose 完成后 emit RequirementsInstalled
    //
    // 触发链路：RequirementsInstalled → EnvironmentInspectorService 标记 cache stale
    //   → 下次 inspect_or_cached 触发 spawn_refresh → emit Tauri Event `env_inspect_updated`
    //   → 前端 env_store.subscribe 收到新 EnvSnapshot → torchBroken 告警条立即消失
    //
    // 即使 diagnose 没找到问题也 emit（这是关键）：
    // - "已装但显示未装"是 cache stale 的典型症状
    // - 一次无副作用的 cache 失效可以让下一次 inspect 拿到真值
    // - 用户点"诊断"动作就是"我想看到最新状态"的强信号，必须 invalidate
    tracing::debug!(
        torch_import_ok,
        issue_count = issues.len(),
        "env_diagnose: emitting RequirementsInstalled to refresh env cache"
    );
    event_bus.emit(crate::event_bus::SystemEvent::RequirementsInstalled);

    DiagnoseReport {
        venv_exists,
        torch_import_ok,
        torch_version,
        issues,
        suggested_action,
        suggested_reason,
    }
}

/// 检查 numpy 版本（轻量：直接读 site-packages 下的 metadata）
async fn check_numpy_version(venv_path: &Path) -> Result<Option<String>, EnvError> {
    let candidates = [
        venv_path.join("Lib/site-packages/numpy/__init__.py"), // Windows
        venv_path.join("lib/python3.11/site-packages/numpy/__init__.py"),
        venv_path.join("lib/python3.10/site-packages/numpy/__init__.py"),
        venv_path.join("lib/python3.12/site-packages/numpy/__init__.py"),
    ];
    for p in &candidates {
        if p.exists() {
            // 优先读 METADATA（最稳定），找不到再读 __init__.py
            // 路径：<numpy>/../numpy-<version>.dist-info/METADATA
            if let Some(parent) = p.parent().and_then(|x| x.parent()) {
                if let Ok(entries) = std::fs::read_dir(parent) {
                    for e in entries.flatten() {
                        let name = e.file_name();
                        let name = name.to_string_lossy();
                        if name.starts_with("numpy-") && name.ends_with(".dist-info") {
                            let meta = e.path().join("METADATA");
                            if let Ok(content) = std::fs::read_to_string(&meta) {
                                for line in content.lines() {
                                    if let Some(rest) = line.strip_prefix("Version: ") {
                                        return Ok(Some(rest.trim().to_string()));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(None)
}

/// numpy 已知坏版本列表
///
/// 真实场景：numpy 2.4.4 wheel 在 2025-12 报告缺 `exceptions.py`，导致 torch 2.4.x
/// 的 `import torch` 抛 ImportError（前端看到"未安装"）。
/// 历史：numpy 2.3.x 部分版本有 ABI 警告，2.0-2.2.x 稳定。
fn is_numpy_known_bad(version: &str) -> bool {
    let v = version.trim_start_matches("v");
    // 解析 major.minor.patch
    let mut parts = v.split('.');
    let major: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    // 2.3+ 都视为有风险
    major >= 2 && minor >= 3
}

/// 检查多个包是否已装
///
/// v3.6：接 `CancellationToken`
async fn check_packages(
    venv_path: &Path,
    packages: &[&str],
    cancel: &CancellationToken,
) -> Result<Vec<(String, Option<String>)>, EnvError> {
    let python = crate::python_env::uv_runner::venv_python_path(venv_path);
    if !python.exists() {
        return Err(EnvError::VerifyFailed(format!(
            "python not found at {}",
            python.display()
        )));
    }
    let mut args: Vec<String> = vec![
        "-m".into(),
        "pip".into(),
        "show".into(),
    ];
    for pkg in packages {
        args.push((*pkg).to_string());
    }
    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let mut cmd = crate::common::process_util::new_command(&python);
    cmd.args(&args_ref)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let output = crate::common::subprocess::run_with_cancel(&mut cmd, cancel)
        .await
        .map_err(EnvError::from)?;
    if !output.status.success() {
        // pip show 在部分包缺失时返回非零
        // 解析 stderr 不太有意义，直接用 stdout 部分
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut result = Vec::new();
    for pkg in packages {
        let mut found_version: Option<String> = None;
        let mut in_section = false;
        for line in stdout.lines() {
            if line.starts_with("Name:") && line.contains(pkg) {
                in_section = true;
                continue;
            }
            if in_section && line.starts_with("Version:") {
                if let Some(v) = line.strip_prefix("Version:") {
                    found_version = Some(v.trim().to_string());
                }
                in_section = false;
            }
        }
        result.push((pkg.to_string(), found_version));
    }
    Ok(result)
}

struct RequirementsMatchReport {
    all_ok: bool,
    mismatched: Vec<String>,
}

/// 检查 venv 已装包 vs requirements.txt
async fn check_requirements_match(
    venv_path: &Path,
    comfyui_root: &Path,
    cancel: &CancellationToken,
) -> Result<RequirementsMatchReport, EnvError> {
    let req_file = comfyui_root.join("requirements.txt");
    if !req_file.exists() {
        return Ok(RequirementsMatchReport {
            all_ok: true,
            mismatched: vec![],
        });
    }
    // 用 env_inspector 模块的解析函数（python_env::verify 返回的 EnvInfo 不含 dependencies 字段）
    let required = crate::env_inspector::deps::read_requirements(comfyui_root)
        .await
        .unwrap_or_default();
    // 跑 pip list 拿已装包（不带 uv_binary 参数：fallback 到 python -m pip）
    let pip_list_json = crate::env_inspector::scripts::run_pip_list(venv_path, None, cancel).await?;
    let installed = crate::env_inspector::deps::parse_pip_list(&pip_list_json)?;
    let deps = crate::env_inspector::deps::build_dependency_list(&installed, &required);
    let mut mismatched = Vec::new();
    for d in &deps {
        if matches!(
            d.status,
            crate::env_inspector::models::DepStatus::Missing
                | crate::env_inspector::models::DepStatus::NeedsUpgrade { .. }
        ) {
            mismatched.push(d.name.clone());
        }
    }
    Ok(RequirementsMatchReport {
        all_ok: mismatched.is_empty(),
        mismatched,
    })
}

/// 轻量修复：降级 numpy 到稳定版
///
/// 适用场景：诊断出 numpy 2.4.x 是 torch import 失败的根因
///
/// 命令：`uv pip install "numpy<2.3,<2.4" --python <venv>`
/// 装完再调 smoke_test_torch 验证。
///
/// v3.7（F4）：可选 `line_collector` 实时日志（透传到 uv_run_cmd_with_log）
pub async fn quick_repair_numpy(
    uv: &UvRunner,
    venv_path: &Path,
    progress: &ProgressSender,
    cancel: &CancellationToken,
    line_collector: Option<&std::sync::Arc<crate::common::line_collector::LineCollector>>,
) -> Result<(), EnvError> {
    if cancel.is_cancelled() {
        return Err(EnvError::RebuildFailed {
            detail: "任务已取消".to_string(),
        });
    }
    progress.send_percent(10);
    progress.send_message("降级 numpy 到 < 2.3（修复 import torch 失败问题）".to_string());

    // 写 constraints 文件（用 temp，因为不在 venv 生命周期内）
    let (_tmp_path, _tmp_keep) = freeze::write_constraints_to_temp().map_err(|e| {
        EnvError::RebuildFailed {
            detail: format!("写入 temp constraints 失败: {}", e),
        }
    })?;
    // _tmp_keep 持有 File handle，函数结束时 drop 会自动关闭文件
    // （不需要在这里主动清理，系统 temp 目录会被 OS 周期清理）

    let venv_arg = format!("--python={}", crate::python_env::uv_runner::venv_python_path(venv_path).to_string_lossy());
    let constraints_arg = _tmp_path.to_string_lossy().into_owned();
    let args: Vec<&str> = vec![
        "pip", "install", "--upgrade", &venv_arg, "-c", &constraints_arg, "numpy<2.3",
    ];
    let output = uv_run_cmd_with_log(uv, &args, cancel, line_collector, "uv:repair-numpy").await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(EnvError::RebuildFailed {
            detail: format!("numpy 降级失败: {}", stderr),
        });
    }

    progress.send_percent(70);
    progress.send_message("验证 torch import...".to_string());
    uv.smoke_test_torch(venv_path, cancel).await?;

    progress.send_percent(100);
    progress.send_message("numpy 降级完成，torch import 已恢复".to_string());
    Ok(())
}

/// 轻量修复：重装关键依赖（torch / torchvision / torchaudio / requirements）
///
/// v3.7（F4）：可选 `line_collector` 实时日志（透传到 install_torch / install_requirements）
pub async fn quick_repair_reinstall(
    uv: &UvRunner,
    venv_path: &Path,
    comfyui_root: &Path,
    cuda_version: &CudaVersion,
    progress: &ProgressSender,
    cancel: &CancellationToken,
    line_collector: Option<&std::sync::Arc<crate::common::line_collector::LineCollector>>,
) -> Result<(), EnvError> {
    if cancel.is_cancelled() {
        return Err(EnvError::RebuildFailed {
            detail: "任务已取消".to_string(),
        });
    }
    progress.send_percent(10);
    progress.send_message(format!("重装 torch ({})...", cuda_version.display_name()));
    uv.install_torch(venv_path, cuda_version, cancel, line_collector).await?;

    progress.send_percent(50);
    let req_file = comfyui_root.join("requirements.txt");
    if req_file.exists() {
        progress.send_message("重装 requirements.txt...".to_string());
        let constraints = freeze::write_constraints_to_venv(venv_path).map_err(|e| {
            EnvError::RebuildFailed {
                detail: format!("写 constraints 失败: {}", e),
            }
        })?;
        // v3.10：传 pytorch_index，避免 transformers 5.x 等依赖触发 torch 覆盖成 +cpu
        let pytorch_index = crate::python_env::uv_runner::cuda_index_url(cuda_version);
        uv.install_requirements(venv_path, &req_file, Some(&constraints), pytorch_index.as_deref(), cancel, line_collector).await?;
    }

    progress.send_percent(90);
    progress.send_message("smoke test...".to_string());
    uv.smoke_test_torch(venv_path, cancel).await?;

    progress.send_percent(100);
    progress.send_message("重装完成".to_string());
    Ok(())
}

/// 完整重建：删 venv → 重建 → 装 torch → 装 requirements → smoke test
///
/// 最稳但最慢（2-5 分钟）。其他修复都失败时再用。
///
/// v3.7（F4）：可选 `line_collector` 实时日志（透传到 rebuild_venv）
pub async fn rebuild_repair(
    python_env: &PythonEnvService,
    venv_path: &Path,
    config: &Config,
    progress: &ProgressSender,
    cancel: &CancellationToken,
    line_collector: Option<&std::sync::Arc<crate::common::line_collector::LineCollector>>,
) -> Result<(), EnvError> {
    if cancel.is_cancelled() {
        return Err(EnvError::RebuildFailed {
            detail: "任务已取消".to_string(),
        });
    }
    progress.send_percent(5);
    progress.send_message("重建 venv（5 步事务：删 → 建 → 装 torch → 装依赖 → 验证）".to_string());

    // 复用现有 rebuild_venv（已经是经过验证的事务）
    python_env.rebuild_venv(config, cancel, line_collector).await?;

    progress.send_percent(90);
    progress.send_message("smoke test...".to_string());
    python_env.uv().smoke_test_torch(venv_path, cancel).await?;

    progress.send_percent(100);
    progress.send_message("venv 重建完成".to_string());
    Ok(())
}

/// v3.10 新增：强制一致重装 torch/torchvision/torchaudio
///
/// ## 适用场景
/// 用户 Config 已切换 cuda_version（如 cu128），但 venv 中的 torch/torchvision/torchaudio
/// 来自不同源（PyPI base version + pytorch.org cu128 wheel），导致
/// `torch.cuda.is_available() = False` 而 ComfyUI 启动时
/// `import comfy.model_management` → `torch.cuda.current_device()` 抛
/// `AssertionError: Torch not compiled with CUDA enabled`。
///
/// ## 修复策略
/// **强制从 pytorch.org 源用 `--force-reinstall --no-deps` 重装**：
/// 1. `uv pip install --index-url <pytorch.org/whl/cu{version}> --force-reinstall --no-deps torch torchvision torchaudio`
///    - `--force-reinstall`：覆盖已有 wheel（即使版本号相同）
///    - `--no-deps`：切断依赖链，避免 venv 中其他包影响
///    - `--index-url`：严格只从 pytorch.org 找，**不走 PyPI**
/// 2. 装 torch 关键依赖（numpy / psutil / six / av / Pillow / pycocotools）
/// 3. 装 ComfyUI requirements（已过滤 torch 系列行）
/// 4. smoke test 验证 `import torch; torch.cuda.is_available()` 返回 true
///
/// ## 进度细分
/// - 10%：开始
/// - 30%：强制重装 torch 系列（--force-reinstall --no-deps）
/// - 60%：装 torch 关键依赖
/// - 80%：装 ComfyUI requirements
/// - 90%：smoke test
/// - 100%：完成
///
/// ## 不破坏 venv 的其他包
/// - 只重装 torch/torchvision/torchaudio 三个包
/// - 其他包（transformers / safetensors / comfy-kitchen 等）保持不变
pub async fn quick_repair_reinstall_consistent(
    uv: &UvRunner,
    venv_path: &Path,
    comfyui_root: &Path,
    cuda_version: &CudaVersion,
    progress: &ProgressSender,
    cancel: &CancellationToken,
    line_collector: Option<&std::sync::Arc<crate::common::line_collector::LineCollector>>,
) -> Result<(), EnvError> {
    if cancel.is_cancelled() {
        return Err(EnvError::RebuildFailed {
            detail: "任务已取消".to_string(),
        });
    }

    // 1. 计算 pytorch.org 源 URL（与 install_torch 内部逻辑一致）
    let index_url = crate::python_env::uv_runner::cuda_index_url(cuda_version)
        .ok_or_else(|| EnvError::RebuildFailed {
            detail: format!("无法计算 pytorch.org 源 URL（cuda_version={:?}）", cuda_version),
        })?;

    progress.send_percent(10);
    progress.send_message(format!(
        "强制重装 torch/torchvision/torchaudio（来源：pytorch.org {}）",
        index_url
    ));

    // 2. 强制重装 torch + torchvision + torchaudio
    //    --force-reinstall：覆盖已有 wheel
    //    --no-deps：切断依赖链
    //    --index-url：严格只从 pytorch.org 找（不走 PyPI）
    //    --upgrade：强制选 pytorch.org 上最新版本
    let venv_arg = format!(
        "--python={}",
        crate::python_env::uv_runner::venv_python_path(venv_path).to_string_lossy()
    );
    let args: Vec<String> = vec![
        "pip".to_string(),
        "install".to_string(),
        venv_arg.clone(),
        "--index-url".to_string(),
        index_url.clone(),
        "--upgrade".to_string(),
        "--force-reinstall".to_string(),
        "--no-deps".to_string(),
        "torch".to_string(),
        "torchvision".to_string(),
        "torchaudio".to_string(),
    ];
    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let output = uv_run_cmd_with_log(uv, &args_ref, cancel, line_collector, "uv:repair-force-torch").await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(EnvError::RebuildFailed {
            detail: format!("强制重装 torch 失败: {}", stderr),
        });
    }

    if cancel.is_cancelled() {
        return Err(EnvError::Cancelled);
    }

    // 3. 装 torch 关键依赖（numpy / psutil / six / av / Pillow / pycocotools）
    progress.send_percent(60);
    progress.send_message("装 torch 关键依赖（numpy/psutil/six/av/Pillow/pycocotools）...".to_string());
    uv.install_torch_extras(venv_path, cancel, line_collector).await?;

    if cancel.is_cancelled() {
        return Err(EnvError::Cancelled);
    }

    // 4. 装 ComfyUI requirements（已过滤 torch 系列行）
    progress.send_percent(80);
    let req_file = comfyui_root.join("requirements.txt");
    if req_file.exists() {
        progress.send_message("重装 ComfyUI requirements...".to_string());
        let constraints = freeze::write_constraints_to_venv(venv_path).map_err(|e| {
            EnvError::RebuildFailed {
                detail: format!("写 constraints 失败: {}", e),
            }
        })?;
        // v3.10 关键修复：把 pytorch.org 作为 extra-index-url
        // - 不传 → 走 PyPI 默认源 → transformers 5.x 等依赖触发 uv 重新拉 torch+cpu
        // - 传 → uv 解析时同时查 PyPI 和 pytorch.org，torch 系列自动从 pytorch.org 拉
        uv.install_requirements(venv_path, &req_file, Some(&constraints), Some(&index_url), cancel, line_collector).await?;
    }

    // 5. smoke test：验证 torch.cuda.is_available() = true
    progress.send_percent(90);
    progress.send_message("smoke test: 验证 torch.cuda.is_available()...".to_string());
    uv.smoke_test_torch(venv_path, cancel).await?;

    // v3.10 关键修复：循环兜底
    //
    // 即使步骤 2-4 走 extra-index-url，uv cache 中残留的 cpu wheel
    // 仍可能在 install_requirements 后**触发** torch 重新解析成 cpu 版。
    // 这里加一道兜底：smoke test 通过 + cuda_available=true 才算成功。
    // 否则**强制从 pytorch.org 重装 torch**（不走 PyPI），最多 2 次。
    progress.send_percent(95);
    progress.send_message("校验 torch.cuda.is_available()...".to_string());
    let mut cuda_ok = uv
        .check_torch_cuda_available(venv_path, cancel)
        .await
        .unwrap_or(false);
    let mut retry_count = 0;
    const MAX_REPAIR_RETRY: u32 = 2;
    while !cuda_ok && retry_count < MAX_REPAIR_RETRY {
        retry_count += 1;
        progress.send_message(format!(
            "torch.cuda.is_available()=false，强制重装 torch（{}/{}）",
            retry_count, MAX_REPAIR_RETRY
        ));
        tracing::warn!(
            retry = retry_count,
            max = MAX_REPAIR_RETRY,
            index_url = %index_url,
            "smoke test passed but cuda_available=false, force-reinstalling torch from pytorch.org"
        );
        // 强制从 pytorch.org 重装（不走 PyPI，避免再次被 cpu wheel 覆盖）
        let venv_arg = format!(
            "--python={}",
            crate::python_env::uv_runner::venv_python_path(venv_path).to_string_lossy()
        );
        let args: Vec<String> = vec![
            "pip".to_string(),
            "install".to_string(),
            venv_arg,
            "--index-url".to_string(),
            index_url.clone(),
            "--upgrade".to_string(),
            "--force-reinstall".to_string(),
            "--no-deps".to_string(),
            "torch".to_string(),
            "torchvision".to_string(),
            "torchaudio".to_string(),
        ];
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let output = uv_run_cmd_with_log(uv, &args_ref, cancel, line_collector, "uv:repair-retry-torch")
            .await
            .map_err(|e| EnvError::RebuildFailed {
                detail: format!("重试重装 torch 子进程失败: {}", e),
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(EnvError::RebuildFailed {
                detail: format!("重试重装 torch 失败: {}", stderr),
            });
        }
        // 再次校验
        cuda_ok = uv
            .check_torch_cuda_available(venv_path, cancel)
            .await
            .unwrap_or(false);
    }
    if !cuda_ok {
        return Err(EnvError::RebuildFailed {
            detail: format!(
                "经过 {} 次重试，torch.cuda.is_available() 仍为 false。请检查 NVIDIA 驱动版本是否与 torch 兼容",
                MAX_REPAIR_RETRY
            ),
        });
    }
    if retry_count > 0 {
        progress.send_message(format!(
            "✓ torch CUDA 校验通过（重试 {} 次）",
            retry_count
        ));
    }

    progress.send_percent(100);
    progress.send_message("torch 强制一致重装完成".to_string());
    Ok(())
}

/// 包装 uv 子进程调用（带 CancellationToken）
///
/// v3.6：用 `subprocess::run_with_cancel` 替代 `tokio::time::timeout`（300s）
async fn uv_run_cmd(
    uv: &UvRunner,
    args: &[&str],
    cancel: &CancellationToken,
) -> Result<std::process::Output, EnvError> {
    let mut cmd = crate::common::process_util::new_command(uv.binary_path());
    cmd.args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    crate::common::subprocess::run_with_cancel(&mut cmd, cancel)
        .await
        .map_err(|e| match e {
            crate::common::subprocess::SubprocessError::Cancelled => EnvError::Cancelled,
            crate::common::subprocess::SubprocessError::Io(e) => EnvError::RebuildFailed {
                detail: format!("uv 子进程启动失败: {}", e),
            },
            crate::common::subprocess::SubprocessError::Exit { code, stderr } => {
                EnvError::RebuildFailed {
                    detail: format!("uv 子进程退出码 {}: {}", code, stderr),
                }
            }
        })
}

/// 包装 uv 子进程调用（带 CancellationToken + 实时日志）（v3.7：F4 新增）
///
/// 与 `uv_run_cmd` 区别：stdout/stderr 实时推给 collector。
async fn uv_run_cmd_with_log(
    uv: &UvRunner,
    args: &[&str],
    cancel: &CancellationToken,
    line_collector: Option<&std::sync::Arc<crate::common::line_collector::LineCollector>>,
    source: &str,
) -> Result<std::process::Output, EnvError> {
    if let Some(collector) = line_collector {
        // 带日志的分支
        let mut cmd = crate::common::process_util::new_command(uv.binary_path());
        cmd.args(args);
        crate::common::subprocess::run_with_cancel_and_log(&mut cmd, cancel, collector, source)
            .await
            .map_err(|e| match e {
                crate::common::subprocess::SubprocessError::Cancelled => EnvError::Cancelled,
                crate::common::subprocess::SubprocessError::Io(e) => EnvError::RebuildFailed {
                    detail: format!("uv 子进程启动失败: {}", e),
                },
                crate::common::subprocess::SubprocessError::Exit { code, stderr } => {
                    EnvError::RebuildFailed {
                        detail: format!("uv 子进程退出码 {}: {}", code, stderr),
                    }
                }
            })
    } else {
        uv_run_cmd(uv, args, cancel).await
    }
}

/// v1.8 一次性迁移：检查并降级 numpy 2.4.x → 2.2.x
///
/// **场景**：用户已装 numpy 2.4.4（坏版本），`import torch` 失败。
/// 启动器启动时静默跑一次：探测 → 降级 → smoke test。
/// 成功 emit 事件让前端更新。
///
/// **幂等**：检测到 numpy < 2.3 立即返回，不重装。
///
/// **参数 config**：用于读取 cuda_version（重建 venv 时需要）。
/// 调用方应传 `state.config.get()`。
pub async fn run_startup_numpy_migration(
    python_env: &PythonEnvService,
    venv_path: &Path,
    comfyui_root: &Path,
    config: &Config,
    event_bus: &EventBus,
) -> Result<(), String> {
    // 1. venv 存在才能迁移
    if !venv_path.join("pyvenv.cfg").exists() {
        return Ok(());
    }
    // 2. 探测 torch 是否能 import（能 → 无需迁移）
    let cancel = CancellationToken::new();
    let probe_json = crate::env_inspector::scripts::probe_torch_script(venv_path, &cancel)
        .await
        .map_err(|e| format!("probe_torch 失败: {}", e))?;
    let probe = crate::env_inspector::scripts::parse_torch_probe(&probe_json);
    if probe.installed {
        tracing::debug!("startup migration: torch import OK, no migration needed");
        return Ok(());
    }
    // 3. 检查 numpy 版本
    let numpy_version = check_numpy_version(venv_path)
        .await
        .map_err(|e| format!("check_numpy 失败: {}", e))?;
    let Some(ver) = numpy_version else {
        tracing::info!("startup migration: no numpy installed, skip migration");
        return Ok(());
    };
    if !is_numpy_known_bad(&ver) {
        tracing::debug!(
            numpy = %ver,
            "startup migration: numpy version OK, skip"
        );
        return Ok(());
    }

    // 4. 降级 numpy
    tracing::warn!(
        numpy = %ver,
        "startup migration: detected bad numpy version, downgrading to <2.3"
    );
    let cuda = config.torch.cuda_version;
    let dummy_progress = ProgressSender::no_op();
    let uv = python_env.uv();
    if let Err(e) = quick_repair_numpy(uv, venv_path, &dummy_progress, &cancel, None).await {
        let msg = format!("numpy 启动迁移失败（{}）: {}", ver, e);
        tracing::error!(error = %msg, "startup numpy migration failed");
        event_bus.emit(crate::event_bus::SystemEvent::RequirementsInstalled);
        return Err(msg);
    }

    // 5. 通知 env cache 失效
    event_bus.emit(crate::event_bus::SystemEvent::RequirementsInstalled);
    tracing::info!(numpy = %ver, "startup numpy migration succeeded");
    // 抑制未用参数 comfyui_root 的告警（保留接口对称）
    let _ = comfyui_root;
    let _ = cuda;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_numpy_known_bad() {
        assert!(is_numpy_known_bad("2.4.4"));
        assert!(is_numpy_known_bad("2.3.0"));
        assert!(is_numpy_known_bad("2.10.0"));
        assert!(!is_numpy_known_bad("2.2.6"));
        assert!(!is_numpy_known_bad("2.0.0"));
        assert!(!is_numpy_known_bad("1.26.4"));
    }

    #[test]
    fn test_compute_suggested_action_empty() {
        let (act, reason) = DiagnoseReport::compute_suggested_action(&[]);
        assert_eq!(act, RepairAction::None);
        assert!(reason.contains("无需修复"));
    }

    #[test]
    fn test_compute_suggested_action_critical() {
        let issues = vec![Issue {
            severity: IssueSeverity::Critical,
            code: "venv.missing".to_string(),
            message: "venv 不存在".to_string(),
            detail: None,
            suggested_action: RepairAction::RebuildVenv,
        }];
        let (act, _reason) = DiagnoseReport::compute_suggested_action(&issues);
        assert_eq!(act, RepairAction::RebuildVenv);
    }

    #[test]
    fn test_compute_suggested_action_numpy() {
        let issues = vec![Issue {
            severity: IssueSeverity::Warning,
            code: "numpy.known_bad".to_string(),
            message: "numpy 2.4.4".to_string(),
            detail: None,
            suggested_action: RepairAction::DowngradeNumpy,
        }];
        let (act, _reason) = DiagnoseReport::compute_suggested_action(&issues);
        assert_eq!(act, RepairAction::DowngradeNumpy);
    }
}
