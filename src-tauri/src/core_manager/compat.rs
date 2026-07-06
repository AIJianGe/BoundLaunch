//! v1.8 / F36：版本切换兼容性预检
//!
//! 用户在切换 ComfyUI 版本前弹对话框显示该报告，告知：
//! - 当前 venv 的 Python / torch 版本
//! - 目标 tag 要求的 Python / torch 版本
//! - 两者是否兼容
//! - 默认推荐切换模式（Clean / Preserve / Skip）
//!
//! 详见 [03-模块设计/03-CoreManager.md §6.5 / F36]

use std::path::Path;

use serde::{Deserialize, Serialize};

/// 切换模式（v1.8 / F36 新增）
///
/// 用户在前端对话框选择，决定 `run_switch_version` 在切完 git tag 后如何处理 venv。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub enum SwitchMode {
    /// **全部清除**：删 venv → 重建 → 装 requirements.txt → 装 torch
    /// 耗时 2-5 分钟。custom_nodes 依赖也清空，需要重装。
    Clean,
    /// **升/降版本**：保留 venv → `pip install -r new-req.txt --upgrade --force-reinstall`
    /// 耗时 30-60 秒。torch / custom_nodes 依赖都保留。
    Preserve,
    /// **不动环境**：只切 git tag，不动 venv
    /// 耗时 5-10 秒。启动时若缺包会报错。
    Skip,
}

/// requirements.txt 差异
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub enum RequirementsDiff {
    /// 完全一致（pip check 通过）
    Identical,
    /// 只缺包（v0.27 比 v0.26 多了几个 optional 依赖）
    OnlyMissing {
        missing_packages: Vec<String>,
    },
    /// 有 major 版本变化（如 transformers 5.x → 4.x）
    HasMajorChange {
        changed: Vec<(String, String, String)>, // (name, old_version, new_version)
    },
}

/// 版本兼容性报告（前端对话框用）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionCompatReport {
    /// 当前 ComfyUI HEAD tag
    pub current_tag: Option<String>,
    /// 目标 tag
    pub target_tag: String,
    /// venv 是否存在
    pub venv_exists: bool,
    /// 当前 venv 的 Python 版本（major.minor.patch），不存在时为 None
    pub current_python: Option<String>,
    /// 目标 tag 要求的 Python 版本（从 target requirements.txt + 推断），None = 未识别
    pub target_python: Option<String>,
    /// 当前 torch variant（如 "cu121"、"cpu"）
    pub current_torch_variant: Option<String>,
    /// 目标 tag 推断的 torch variant（基于 CUDA 检测 + requirements.txt 关键字）
    pub target_torch_variant: Option<String>,
    /// 是否有 torch
    pub current_torch_installed: bool,
    /// Python 是否一致
    pub same_python: bool,
    /// torch variant 是否一致
    pub same_torch_variant: bool,
    /// requirements.txt 差异
    pub requirements_diff: RequirementsDiff,
    /// custom_nodes 数量
    pub custom_node_count: usize,
    /// 建议的切换模式
    pub recommended_mode: SwitchMode,
    /// 建议模式的原因（前端展示）
    pub recommended_reason: String,
}

/// 推断 venv 当前的 torch variant
///
/// 通过读 `site-packages/torch/version.py` 直接解析（不跑 python，0 成本）
pub async fn detect_current_torch_variant(
    venv_path: &Path,
) -> Option<String> {
    if !venv_path.join("pyvenv.cfg").exists() {
        return None;
    }
    crate::env_inspector::scripts::read_torch_variant_fast(venv_path)
}

/// 比较两个 requirements.txt
///
/// 解析为 `(name, version_spec)` 列表，比较 added / removed / changed
pub fn diff_requirements(
    current_reqs: &[(String, String)],
    target_reqs: &[(String, String)],
) -> RequirementsDiff {
    use std::collections::HashMap;
    let cur: HashMap<_, _> = current_reqs.iter().cloned().collect();
    let tgt: HashMap<_, _> = target_reqs.iter().cloned().collect();

    // 缺失：target 有但 current 没有
    let missing: Vec<String> = tgt
        .keys()
        .filter(|k| !cur.contains_key(*k))
        .cloned()
        .collect();

    // 变化：两边都有但 version_spec 不同
    let changed: Vec<(String, String, String)> = tgt
        .iter()
        .filter_map(|(k, v)| {
            cur.get(k).and_then(|cv| {
                if cv != v {
                    Some((k.clone(), cv.clone(), v.clone()))
                } else {
                    None
                }
            })
        })
        .collect();

    if missing.is_empty() && changed.is_empty() {
        RequirementsDiff::Identical
    } else if changed.is_empty() {
        // 只缺包（不算 major 变化）
        RequirementsDiff::OnlyMissing {
            missing_packages: missing,
        }
    } else {
        // 有变化
        // 简化：所有 changed 都标为 HasMajorChange（前端可以显示）
        // 后续可以加 major/patch 区分
        RequirementsDiff::HasMajorChange { changed }
    }
}

/// 解析 requirements.txt 为 (name, version_spec) 列表
///
/// 简化版：跳过 `#` 注释、空行、`-r` 嵌套、extras、`;` 环境标记
pub fn parse_requirements_simple(content: &str) -> Vec<(String, String)> {
    content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with("-r") {
                return None;
            }
            // 切环境标记：包名; python_version >= '3.10'
            let line = line.split(';').next()?.trim();
            // 切 extras：[torch]
            let line = if let Some(idx) = line.find('[') {
                let end = line.find(']').unwrap_or(line.len());
                format!("{}{}", &line[..idx], &line[end + 1..])
            } else {
                line.to_string()
            };
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            // 分离包名与版本：torch==2.4.0 → ("torch", "==2.4.0")
            // 找第一个版本操作符位置
            let op_pos = line
                .find("==")
                .or_else(|| line.find(">="))
                .or_else(|| line.find("<="))
                .or_else(|| line.find("!="))
                .or_else(|| line.find('>'))
                .or_else(|| line.find('<'))
                .or_else(|| line.find('~'));
            let (name, ver) = match op_pos {
                Some(p) => (line[..p].trim().to_string(), line[p..].trim().to_string()),
                None => (line.to_string(), String::new()),
            };
            // 去掉名字后的空格
            let name = name.split_whitespace().next()?.to_string();
            Some((name, ver))
        })
        .collect()
}

/// 推断 recommended_mode
pub fn recommend_mode(
    same_python: bool,
    same_torch_variant: bool,
    venv_exists: bool,
    diff: &RequirementsDiff,
    current_torch_installed: bool,
) -> (SwitchMode, String) {
    if !venv_exists {
        return (
            SwitchMode::Clean,
            "venv 不存在，必须全部清除 + 重建".to_string(),
        );
    }
    if !same_python {
        return (
            SwitchMode::Clean,
            format!(
                "Python 版本不一致（解释器无法升级，必须重建 venv）"
            ),
        );
    }
    if !same_torch_variant {
        return (
            SwitchMode::Clean,
            "torch CUDA variant 不一致（cu118/cu121/cu124 不能并存）".to_string(),
        );
    }
    if !current_torch_installed {
        return (
            SwitchMode::Clean,
            "torch 未安装，需要重建 venv 装 torch".to_string(),
        );
    }
    match diff {
        RequirementsDiff::Identical => (
            SwitchMode::Skip,
            "Python / torch / requirements.txt 都一致，最快：只切 git tag".to_string(),
        ),
        RequirementsDiff::OnlyMissing { .. } => (
            SwitchMode::Preserve,
            "只缺几个包（可能是新版本新增的 optional 依赖），保留 venv 增量补".to_string(),
        ),
        RequirementsDiff::HasMajorChange { changed } => {
            // 有 major 变化 → 仍然推荐 Preserve（让 pip 自动处理），但附带警告
            (
                SwitchMode::Preserve,
                format!(
                    "有 {} 个包 major 版本变化，保留 venv 用 pip --upgrade 处理",
                    changed.len()
                ),
            )
        }
    }
}
