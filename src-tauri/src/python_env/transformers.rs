//! transformers 版本切换服务（v3.7 新增）
//!
//! 提供：
//! - `switch_version`：切换到指定版本
//! - `restore_default`：恢复到 ComfyUI requirements.txt 约束的默认版本
//!
//! 设计模式：
//! - **自由函数 + 依赖注入**：接受 `UvRunner` + `EventBus` 参数，无全局状态
//! - **Strategy**：`select_default_version` 按约束策略选版本
//!
//! 详见 `PR/03-模块设计/02-PythonEnvManager.md §15 transformers 版本切换`

use std::path::Path;

use tokio_util::sync::CancellationToken;

use crate::error::EnvError;
use crate::event_bus::{EventBus, SystemEvent};
use crate::python_env::uv_runner::UvRunner;
use crate::python_env::TransformersVersionIndex;

/// 切换 transformers 到指定版本
///
/// 流程：
/// 1. 调 `uv pip install --upgrade transformers==<version>`
/// 2. emit `RequirementsInstalled` 让 env cache 失效
///
/// 返回：Ok(()) 表示切换成功
///
/// v3.7（F4）：可选 `line_collector` 实时日志（None = 不收集）
pub async fn switch_version(
    uv: &UvRunner,
    event_bus: &EventBus,
    venv_path: &Path,
    version: &str,
    cancel: &CancellationToken,
    line_collector: Option<&std::sync::Arc<crate::common::line_collector::LineCollector>>,
) -> Result<(), EnvError> {
    tracing::info!(version, ?venv_path, "switching transformers version");

    uv.install_package(venv_path, "transformers", version, cancel, line_collector)
        .await?;

    // 通知 EnvironmentInspector 失效 30s 缓存（与 install_requirements 一致）
    event_bus.emit(SystemEvent::RequirementsInstalled);

    tracing::info!(version, "transformers switched");
    Ok(())
}

/// 恢复默认 transformers 版本（按 ComfyUI requirements.txt 约束）
///
/// 流程：
/// 1. 读 `<comfyui_root>/requirements.txt`
/// 2. 解析 `transformers>=X.Y.Z` 约束
/// 3. 从 `TransformersVersionIndex` 取版本列表
/// 4. 选满足约束的最新 4.x 版本（排除 5.x 破坏性变更）
/// 5. 调 `switch_version` 切换
///
/// 返回：选定的版本号（如 "4.57.3"）
///
/// v3.7（F4）：可选 `line_collector` 实时日志（透传给 switch_version）
pub async fn restore_default(
    uv: &UvRunner,
    event_bus: &EventBus,
    venv_path: &Path,
    comfyui_root: &Path,
    version_index: &TransformersVersionIndex,
    cancel: &CancellationToken,
    line_collector: Option<&std::sync::Arc<crate::common::line_collector::LineCollector>>,
) -> Result<String, EnvError> {
    // 1. 读 requirements.txt
    let req_path = comfyui_root.join("requirements.txt");
    let req_content = tokio::fs::read_to_string(&req_path)
        .await
        .map_err(|e| {
            EnvError::RequirementsInstallFailed(format!(
                "读取 requirements.txt 失败: {} (path: {})",
                e,
                req_path.display()
            ))
        })?;

    // 2. 解析 transformers 约束
    let constraint = parse_transformers_constraint(&req_content);

    // 3. 从版本列表选满足约束的最新 4.x
    let versions = version_index.get_versions();
    let target_version = select_default_version(&versions, constraint.as_deref()).ok_or_else(
        || {
            EnvError::RequirementsInstallFailed(format!(
                "无法确定默认 transformers 版本（约束: {:?}，可用 4.x 版本均不满足）",
                constraint
            ))
        },
    )?;

    tracing::info!(
        constraint = ?constraint,
        selected = %target_version,
        "restore_default: selected version"
    );

    // 4. 切换
    switch_version(uv, event_bus, venv_path, &target_version, cancel, line_collector).await?;

    Ok(target_version)
}

/// 解析 requirements.txt 中的 transformers 约束
///
/// 支持格式：
/// - `transformers>=4.50.3`
/// - `transformers>=4.50.3,<5.0.0`
/// - `transformers==4.50.3`
/// - `transformers~=4.50.0`
/// - `transformers`（无约束，单独一行）
/// - `transformers[torch]>=4.50.3`（带 extras）
/// - `transformers>=4.50.3  # required`（带注释）
///
/// 返回约束字符串（如 `">=4.50.3"` 或 `">=4.50.3,<5.0.0"`），
/// 无约束（`transformers` 单独一行）返回 `None`，
/// 文件中无 `transformers` 行也返回 `None`。
pub fn parse_transformers_constraint(content: &str) -> Option<String> {
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // 匹配 transformers 开头（可能带 [extras]）
        if let Some(rest) = line.strip_prefix("transformers") {
            // 去掉 [extras]（如 transformers[torch]>=4.50.3 → >=4.50.3）
            let rest = rest.split('[').next().unwrap_or(rest);
            // 去掉行内注释
            let rest = rest.split('#').next().unwrap_or(rest).trim();

            if rest.is_empty() {
                // 无约束（transformers 单独一行）
                return None;
            }

            // 提取约束运算符 + 版本（支持组合如 >=4.50.3,<5.0.0）
            // 允许字符：数字、点、逗号、< > = ~ !
            let constraint: String = rest
                .chars()
                .take_while(|c| {
                    c.is_ascii_digit() || *c == '.' || *c == ',' || *c == '<' || *c == '>'
                        || *c == '=' || *c == '~' || *c == '!'
                })
                .collect();

            if !constraint.is_empty() {
                return Some(constraint);
            }
        }
    }
    None
}

/// 从版本列表选满足约束的最新 4.x 版本
///
/// 规则：
/// - **排除 5.x**（transformers 5.x 有破坏性 API 变更，ComfyUI 暂未适配）
/// - 满足约束（`>=X.Y.Z` / `==X.Y.Z` / `~=X.Y.Z` / `>X.Y.Z`）
/// - 取最新（版本列表应已降序，最新在前）
///
/// 参数：
/// - `versions`：版本号列表（降序）
/// - `constraint`：约束字符串（如 `">=4.50.3"`），None 表示无约束
///
/// 返回：选定的版本号，无匹配返回 None
pub fn select_default_version(versions: &[String], constraint: Option<&str>) -> Option<String> {
    let min_version = match constraint {
        Some(c) => parse_min_version(c),
        None => None,
    };

    versions
        .iter()
        .filter(|v| v.starts_with("4.")) // 排除 5.x
        .find(|v| {
            let v_parts = parse_version_parts(v);
            match &min_version {
                Some(min) => v_parts >= *min,
                None => true, // 无约束，取第一个 4.x
            }
        })
        .cloned()
}

/// 从约束中解析最低版本（如 `">=4.50.3"` → `[4, 50, 3]`）
///
/// 支持：`>=`, `==`, `~=`, `>`, `<=`, `<`（取版本号部分）
/// 组合约束如 `">=4.50.3,<5.0.0"` 取第一个版本号
///
/// 返回 None 表示无法解析（如通配符 `*` 或格式错误）
fn parse_min_version(constraint: &str) -> Option<Vec<u32>> {
    // 找到第一个数字位置
    let start = constraint.chars().position(|c| c.is_ascii_digit())?;

    let version_str: String = constraint[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();

    if version_str.is_empty() {
        return None;
    }

    Some(parse_version_parts(&version_str))
}

/// 解析版本号为数字数组（用于语义比较）
///
/// `"4.57.3"` → `[4, 57, 3]`
/// `"4.57"` → `[4, 57]`
fn parse_version_parts(v: &str) -> Vec<u32> {
    v.split('.').filter_map(|s| s.parse().ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_constraint_ge() {
        assert_eq!(
            parse_transformers_constraint("transformers>=4.50.3"),
            Some(">=4.50.3".to_string())
        );
    }

    #[test]
    fn test_parse_constraint_eq() {
        assert_eq!(
            parse_transformers_constraint("transformers==4.50.3"),
            Some("==4.50.3".to_string())
        );
    }

    #[test]
    fn test_parse_constraint_combined() {
        assert_eq!(
            parse_transformers_constraint("transformers>=4.50.3,<5.0.0"),
            Some(">=4.50.3,<5.0.0".to_string())
        );
    }

    #[test]
    fn test_parse_constraint_no_version() {
        // transformers 单独一行，无约束
        assert_eq!(parse_transformers_constraint("transformers"), None);
    }

    #[test]
    fn test_parse_constraint_with_extras() {
        assert_eq!(
            parse_transformers_constraint("transformers[torch]>=4.50.3"),
            Some(">=4.50.3".to_string())
        );
    }

    #[test]
    fn test_parse_constraint_with_comment() {
        assert_eq!(
            parse_transformers_constraint("transformers>=4.50.3  # required"),
            Some(">=4.50.3".to_string())
        );
    }

    #[test]
    fn test_parse_constraint_multiline() {
        let content = "torch>=2.0.0\ntokenizers>=0.19\ntransformers>=4.50.3\naccelerate>=0.20\n";
        assert_eq!(
            parse_transformers_constraint(content),
            Some(">=4.50.3".to_string())
        );
    }

    #[test]
    fn test_parse_constraint_absent() {
        let content = "torch>=2.0.0\ntokenizers>=0.19\n";
        assert_eq!(parse_transformers_constraint(content), None);
    }

    #[test]
    fn test_parse_min_version_ge() {
        assert_eq!(parse_min_version(">=4.50.3"), Some(vec![4, 50, 3]));
    }

    #[test]
    fn test_parse_min_version_eq() {
        assert_eq!(parse_min_version("==4.50.3"), Some(vec![4, 50, 3]));
    }

    #[test]
    fn test_parse_min_version_combined() {
        // 组合约束取第一个版本号
        assert_eq!(parse_min_version(">=4.50.3,<5.0.0"), Some(vec![4, 50, 3]));
    }

    #[test]
    fn test_select_default_with_constraint() {
        let versions = vec![
            "5.13.0".to_string(),
            "4.57.3".to_string(),
            "4.55.0".to_string(),
            "4.50.3".to_string(),
            "4.40.0".to_string(),
        ];
        // >=4.50.3 应选 4.57.3（最新 4.x 满足约束）
        let result = select_default_version(&versions, Some(">=4.50.3"));
        assert_eq!(result, Some("4.57.3".to_string()));
    }

    #[test]
    fn test_select_default_no_constraint() {
        let versions = vec![
            "5.13.0".to_string(),
            "4.57.3".to_string(),
            "4.50.3".to_string(),
        ];
        // 无约束应选最新 4.x
        let result = select_default_version(&versions, None);
        assert_eq!(result, Some("4.57.3".to_string()));
    }

    #[test]
    fn test_select_default_exclude_5x() {
        let versions = vec!["5.13.0".to_string(), "4.57.3".to_string()];
        let result = select_default_version(&versions, None);
        assert_eq!(result, Some("4.57.3".to_string()));
    }

    #[test]
    fn test_select_default_no_match() {
        let versions = vec!["5.13.0".to_string(), "3.0.0".to_string()];
        // 无 4.x 版本
        let result = select_default_version(&versions, Some(">=4.50.3"));
        assert_eq!(result, None);
    }

    #[test]
    fn test_select_default_constraint_too_high() {
        let versions = vec!["4.50.3".to_string(), "4.40.0".to_string()];
        // 约束 >=4.60.0 但只有 4.50.3 和 4.40.0，都不满足
        let result = select_default_version(&versions, Some(">=4.60.0"));
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_version_parts() {
        assert_eq!(parse_version_parts("4.57.3"), vec![4, 57, 3]);
        assert_eq!(parse_version_parts("4.57"), vec![4, 57]);
        assert_eq!(parse_version_parts("5.0.0"), vec![5, 0, 0]);
    }
}
