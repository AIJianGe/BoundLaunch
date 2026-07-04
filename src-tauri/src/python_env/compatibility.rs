//! 依赖兼容性检查
//!
//! 详见 `PR/03-模块设计/02-PythonEnvManager.md §3` 中 check_requirements_compatibility
//!
//! 在 ComfyUI 版本切换后调用：比对当前 venv 已装依赖 vs 新版本 requirements.txt
//! 返回不匹配列表（缺失 + 过期）

use std::collections::HashMap;
use std::path::Path;

use crate::env_inspector::deps::{parse_pip_list, parse_requirements_txt};
use crate::env_inspector::scripts::run_pip_list;
use crate::error::EnvError;

use super::models::{CompatibilityReport, PackageMismatch, PackageReq};

/// 比对 venv 已装依赖与 requirements.txt
///
/// - `venv_path`：venv 根目录（用于 `pip list`）
/// - `comfyui_root`：ComfyUI 仓库根（含 requirements.txt）
///
/// 返回 `CompatibilityReport`，`is_compatible` 在 missing + outdated 都为空时为 true
pub async fn check_requirements_compatibility(
    venv_path: &Path,
    comfyui_root: &Path,
) -> Result<CompatibilityReport, EnvError> {
    // 1. 读 venv 已装包
    let pip_json = run_pip_list(venv_path).await?;
    let installed = parse_pip_list(&pip_json)?;

    // 2. 读 requirements.txt
    let req_path = comfyui_root.join("requirements.txt");
    if !req_path.exists() {
        // requirements.txt 不存在视为「全部兼容」（不约束）
        return Ok(CompatibilityReport {
            is_compatible: true,
            ..Default::default()
        });
    }
    let content = tokio::fs::read_to_string(&req_path)
        .await
        .map_err(|e| EnvError::VerifyFailed(e.to_string()))?;
    let required = parse_requirements_txt(&content);

    // 3. 比对
    Ok(compare_packages(&installed, &required))
}

/// 比对已装包与要求包
///
/// - `installed`: (name_lower -> installed_version)
/// - `required`: (name_lower -> required_version_spec)
pub fn compare_packages(
    installed: &HashMap<String, String>,
    required: &HashMap<String, String>,
) -> CompatibilityReport {
    let mut missing = Vec::new();
    let mut outdated = Vec::new();

    for (name_lower, required_version) in required {
        match installed.get(name_lower) {
            Some(installed_v) => {
                if required_version.is_empty() {
                    // 无版本约束，已装即可
                } else if !version_satisfies(installed_v, required_version) {
                    outdated.push(PackageMismatch {
                        name: name_lower.clone(),
                        required_version: required_version.clone(),
                        installed_version: installed_v.clone(),
                    });
                }
            }
            None => {
                // 注：requirements.txt 中包名是小写形式
                missing.push(PackageReq {
                    name: name_lower.clone(),
                    required_version: required_version.clone(),
                });
            }
        }
    }

    let is_compatible = missing.is_empty() && outdated.is_empty();
    CompatibilityReport {
        missing,
        outdated,
        is_compatible,
    }
}

/// 简化版本满足判断
///
/// 不实现完整 PEP 440 解析器，仅支持 `==` 与 `>=` 约束
fn version_satisfies(installed: &str, required: &str) -> bool {
    let required_v = extract_version_number(required);
    let installed_v = extract_version_number(installed);

    if required_v.is_empty() || installed_v.is_empty() {
        return true;
    }

    if required.starts_with(">=") {
        installed_v >= required_v
    } else if required.starts_with("==") {
        installed_v == required_v
    } else {
        installed_v == required_v
    }
}

/// 从版本规范中提取纯版本号
fn extract_version_number(s: &str) -> String {
    s.chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_compare_compatible() {
        let installed = HashMap::from([
            ("torch".to_string(), "2.4.0".to_string()),
            ("numpy".to_string(), "1.26.4".to_string()),
        ]);
        let required = HashMap::from([
            ("torch".to_string(), "==2.4.0".to_string()),
            ("numpy".to_string(), ">=1.26".to_string()),
        ]);

        let report = compare_packages(&installed, &required);
        assert!(report.is_compatible);
        assert!(report.missing.is_empty());
        assert!(report.outdated.is_empty());
    }

    #[test]
    fn test_compare_missing() {
        let installed: HashMap<String, String> = HashMap::new();
        let required = HashMap::from([("torch".to_string(), "==2.4.0".to_string())]);

        let report = compare_packages(&installed, &required);
        assert!(!report.is_compatible);
        assert_eq!(report.missing.len(), 1);
        assert_eq!(report.missing[0].name, "torch");
    }

    #[test]
    fn test_compare_outdated() {
        let installed = HashMap::from([("torch".to_string(), "2.3.0".to_string())]);
        let required = HashMap::from([("torch".to_string(), "==2.4.0".to_string())]);

        let report = compare_packages(&installed, &required);
        assert!(!report.is_compatible);
        assert_eq!(report.outdated.len(), 1);
        assert_eq!(report.outdated[0].installed_version, "2.3.0");
    }

    #[test]
    fn test_compare_no_version_constraint() {
        let installed = HashMap::from([("torch".to_string(), "2.4.0".to_string())]);
        let required = HashMap::from([("torch".to_string(), "".to_string())]);

        let report = compare_packages(&installed, &required);
        assert!(report.is_compatible);
    }

    #[test]
    fn test_compare_empty_required_all_compatible() {
        let installed = HashMap::from([("torch".to_string(), "2.4.0".to_string())]);
        let required: HashMap<String, String> = HashMap::new();

        let report = compare_packages(&installed, &required);
        assert!(report.is_compatible);
    }
}
