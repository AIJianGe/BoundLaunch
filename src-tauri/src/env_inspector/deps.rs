//! 依赖解析（pip list 解析 + requirements.txt 比对）
//!
//! 详见 `PR/03-模块设计/07-EnvironmentInspector.md §5 数据流` 中 inspect_dependencies 子任务

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use super::models::{DepStatus, DependencyInfo};
use crate::error::EnvError;

/// 关键依赖清单（固定常量）
///
/// (包名, 用途说明)
///
/// v3.2 修复：**移除 torch / torchvision / torchaudio**
///
/// 原因：
/// - 这三个包由 `InstallTorch` 步骤单独安装（不在 ComfyUI requirements.txt 中）
/// - ComfyUI 官方文档明确说"请先单独装 torch"
/// - 之前把它们留在 KEY_DEPENDENCIES 中，导致 venv 没装 torch 时，readiness
///   同时报 `InstallTorch` 和 `InstallRequirements` 两个步骤缺失（误报）
/// - 拆解后：torch 由 `InstallTorch` 步骤管，ComfyUI 运行时其他依赖由 `InstallRequirements` 管
pub const KEY_DEPENDENCIES: &[(&str, &str)] = &[
    // torch / torchvision / torchaudio：已移除（v3.2）
    ("torchsde", "随机微分方程求解器"),
    ("safetensors", "模型文件加载"),
    ("transformers", "CLIP / 文本编码"),
    ("tokenizers", "分词器"),
    ("kornia", "视觉算子库"),
    ("spandrel", "模型架构识别"),
    ("numpy", "数值计算"),
    ("aiohttp", "ComfyUI Web 服务端"),
    ("pydantic", "配置与数据校验"),
];

/// `pip list --format=json` 输出的单条记录
#[derive(Debug, Deserialize)]
struct PipPackage {
    name: String,
    version: String,
}

/// 解析 pip list JSON 输出为 `(name -> version)` 映射
pub fn parse_pip_list(json: &str) -> Result<HashMap<String, String>, EnvError> {
    let packages: Vec<PipPackage> =
        serde_json::from_str(json).map_err(|e| EnvError::VerifyFailed(e.to_string()))?;
    let mut map = HashMap::with_capacity(packages.len());
    for p in packages {
        // pip list 输出名称大小写不固定，统一转小写比较
        map.insert(p.name.to_lowercase(), p.version);
    }
    Ok(map)
}

/// 解析 requirements.txt
///
/// 返回 `(name_lower -> required_version_spec)` 映射
///
/// 支持的行格式：
/// - `package==1.0.0`
/// - `package>=1.0.0`
/// - `package>=1.0.0,<2.0.0`
/// - `package`（无版本约束）
/// - `# comment`（跳过）
/// - 空行（跳过）
pub fn parse_requirements_txt(content: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // 去除 inline 注释
        let line = line.split('#').next().unwrap().trim();
        // 寻找第一个版本操作符
        let split_pos = line.find(|c: char| c == '=' || c == '>' || c == '<' || c == '~' || c == '!');
        if let Some(pos) = split_pos {
            let name = line[..pos].trim().to_lowercase();
            let version_spec = line[pos..].trim().to_string();
            map.insert(name, version_spec);
        } else {
            // 仅包名，无版本约束
            map.insert(line.to_lowercase(), String::new());
        }
    }
    map
}

/// 读取 ComfyUI requirements.txt
pub async fn read_requirements(comfyui_root: &Path) -> Result<HashMap<String, String>, EnvError> {
    let req_path = comfyui_root.join("requirements.txt");
    if !req_path.exists() {
        return Err(EnvError::VerifyFailed(
            "ComfyUI requirements.txt not found".to_string(),
        ));
    }
    let content = tokio::fs::read_to_string(&req_path)
        .await
        .map_err(|e| EnvError::VerifyFailed(e.to_string()))?;
    Ok(parse_requirements_txt(&content))
}

/// 构建关键依赖列表
///
/// - `installed`: 来自 pip list 的实际安装版本
/// - `required`: 来自 requirements.txt 的版本约束
/// - `status`: 比对结果
pub fn build_dependency_list(
    installed: &HashMap<String, String>,
    required: &HashMap<String, String>,
) -> Vec<DependencyInfo> {
    KEY_DEPENDENCIES
        .iter()
        .map(|(name, _)| {
            let name_lower = name.to_lowercase();
            let installed_version = installed.get(&name_lower).cloned();
            let required_version = required.get(&name_lower).cloned();

            let status = match (&installed_version, &required_version) {
                (Some(installed_v), Some(req_v)) => {
                    if req_v.is_empty() {
                        // requirements.txt 中无版本约束
                        DepStatus::Satisfied
                    } else if version_satisfies(installed_v, req_v) {
                        DepStatus::Satisfied
                    } else {
                        DepStatus::NeedsUpgrade {
                            current: installed_v.clone(),
                            required: req_v.clone(),
                        }
                    }
                }
                (Some(_), None) => DepStatus::NotRequired,
                (None, Some(req_v)) => {
                    if req_v.is_empty() {
                        DepStatus::Missing
                    } else {
                        DepStatus::Missing
                    }
                }
                (None, None) => DepStatus::Missing,
            };

            DependencyInfo {
                name: name.to_string(),
                installed_version,
                required_version: required_version.filter(|s| !s.is_empty()),
                status,
            }
        })
        .collect()
}

/// 简化版本满足判断
///
/// 复杂约束（如 `>=1.0,<2.0,!=1.5`）按简化规则处理：
/// - 提取首个版本号与 installed 比较
/// - 不实现完整 PEP 440 解析器（依赖 packaging 库会引入额外依赖）
///
/// v3.2.1 关键修复：
/// 1. **支持 `~=` 约束**（PEP 440 compatible release）
///    - 例如 `pydantic~=2.0` 等价于 `>=2.0, <3.0`
///    - 之前未处理，导致 pydantic 2.13.4 误判为 NeedsUpgrade
/// 2. **用元组比较替代字符串比较**
///    - 字符串 `"0.10.0" >= "0.7.0"` 在 ASCII 比较下错误（'1' < '7'）
///    - 元组比较 `[0,10,0] >= [0,7,0]` 正确（数值 10 > 7）
fn version_satisfies(installed: &str, required: &str) -> bool {
    // 提取 required 中的版本号
    let required_version = extract_version_number(required);
    let installed_version = extract_version_number(installed);

    if required_version.is_empty() || installed_version.is_empty() {
        // 无法解析时按「满足」处理，避免误报
        return true;
    }

    if required.starts_with(">=") {
        return version_gte(&installed_version, &required_version);
    }
    if required.starts_with("==") {
        return installed_version == required_version;
    }
    if required.starts_with("~=") {
        // PEP 440 compatible release: ~=X.Y[.Z] 等价于 >=X.Y[.Z], <(X+1).0
        // 例如 ~=2.0 → [2,0) 即 >=2.0, <3.0
        let parts: Vec<&str> = required_version.split('.').collect();
        if parts.is_empty() {
            return true;
        }
        let major: u32 = parts[0].parse().unwrap_or(0);
        let upper = format!("{}.0", major + 1);
        return version_gte(&installed_version, &required_version)
            && version_lt(&installed_version, &upper);
    }
    // 默认按「相等」处理
    installed_version == required_version
}

/// 从版本字符串中解析出 (major, minor, patch, ...) 数值元组
///
/// - "2.13.4" → [2, 13, 4]
/// - "0.7.1" → [0, 7, 1]
/// - "" → []
///
/// 容错：解析失败的段跳过（不报错），用于版本号末尾的预发布标签如 "1.0.0a1"
fn version_tuple(s: &str) -> Vec<u32> {
    s.split('.').filter_map(|p| p.parse().ok()).collect()
}

/// 元组比较：installed >= required（按数值）
///
/// - 段数不一致时，短的一方补 0
/// - 任一段 installed > required → true
/// - 任一段 installed < required → false
/// - 所有段都相等 → true
fn version_gte(installed: &str, required: &str) -> bool {
    let inst = version_tuple(installed);
    let req = version_tuple(required);
    for (i, r) in req.iter().enumerate() {
        let i_val = inst.get(i).copied().unwrap_or(0);
        if i_val > *r {
            return true;
        }
        if i_val < *r {
            return false;
        }
    }
    true
}

/// 元组比较：installed < upper（按数值）
///
/// - 段数不一致时，短的一方补 0
/// - 任一段 installed < upper → true
/// - 任一段 installed > upper → false
/// - 所有段都相等 → false（严格小于）
fn version_lt(installed: &str, upper: &str) -> bool {
    let inst = version_tuple(installed);
    let up = version_tuple(upper);
    for (i, u) in up.iter().enumerate() {
        let i_val = inst.get(i).copied().unwrap_or(0);
        if i_val < *u {
            return true;
        }
        if i_val > *u {
            return false;
        }
    }
    false
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

    #[test]
    fn test_parse_pip_list_basic() {
        let json = r#"[
            {"name": "torch", "version": "2.4.0+cu121"},
            {"name": "numpy", "version": "1.26.4"}
        ]"#;
        let map = parse_pip_list(json).unwrap();
        assert_eq!(map.get("torch").unwrap(), "2.4.0+cu121");
        assert_eq!(map.get("numpy").unwrap(), "1.26.4");
    }

    #[test]
    fn test_parse_pip_list_lowercase_keys() {
        // pip list 名称大小写不固定，统一转小写
        let json = r#"[{"name": "Torch", "version": "2.4.0"}]"#;
        let map = parse_pip_list(json).unwrap();
        assert!(map.contains_key("torch"));
    }

    #[test]
    fn test_parse_requirements_eq() {
        let content = "torch==2.4.0\nnumpy>=1.26\n";
        let map = parse_requirements_txt(content);
        assert_eq!(map.get("torch").unwrap(), "==2.4.0");
        assert_eq!(map.get("numpy").unwrap(), ">=1.26");
    }

    #[test]
    fn test_parse_requirements_no_version() {
        let content = "torch\nnumpy\n";
        let map = parse_requirements_txt(content);
        assert_eq!(map.get("torch").unwrap(), "");
    }

    #[test]
    fn test_parse_requirements_skip_comments() {
        let content = "# comment\ntorch==2.4.0\n\n# another comment\nnumpy\n";
        let map = parse_requirements_txt(content);
        assert_eq!(map.len(), 2);
        assert!(map.contains_key("torch"));
        assert!(map.contains_key("numpy"));
    }

    #[test]
    fn test_build_dep_list_satisfied() {
        let installed = HashMap::from([
            ("torch".to_string(), "2.4.0".to_string()),
            ("numpy".to_string(), "1.26.4".to_string()),
        ]);
        let required = HashMap::from([
            ("torch".to_string(), "==2.4.0".to_string()),
            ("numpy".to_string(), ">=1.26".to_string()),
        ]);

        let deps = build_dependency_list(&installed, &required);
        let torch_dep = deps.iter().find(|d| d.name == "torch").unwrap();
        assert!(matches!(torch_dep.status, DepStatus::Satisfied));
    }

    #[test]
    fn test_build_dep_list_needs_upgrade() {
        let installed = HashMap::from([("torch".to_string(), "2.3.0".to_string())]);
        let required = HashMap::from([("torch".to_string(), "==2.4.0".to_string())]);

        let deps = build_dependency_list(&installed, &required);
        let torch_dep = deps.iter().find(|d| d.name == "torch").unwrap();
        match &torch_dep.status {
            DepStatus::NeedsUpgrade { current, required } => {
                assert_eq!(current, "2.3.0");
                assert_eq!(required, "==2.4.0");
            }
            _ => panic!("expected NeedsUpgrade"),
        }
    }

    #[test]
    fn test_build_dep_list_missing() {
        let installed: HashMap<String, String> = HashMap::new();
        let required = HashMap::from([("torch".to_string(), "==2.4.0".to_string())]);

        let deps = build_dependency_list(&installed, &required);
        let torch_dep = deps.iter().find(|d| d.name == "torch").unwrap();
        assert!(matches!(torch_dep.status, DepStatus::Missing));
    }

    #[test]
    fn test_build_dep_list_not_required() {
        // 包已装但 requirements.txt 中没列
        let installed = HashMap::from([("torch".to_string(), "2.4.0".to_string())]);
        let required: HashMap<String, String> = HashMap::new();

        let deps = build_dependency_list(&installed, &required);
        let torch_dep = deps.iter().find(|d| d.name == "torch").unwrap();
        assert!(matches!(torch_dep.status, DepStatus::NotRequired));
    }

    #[test]
    fn test_build_dep_list_has_all_key_deps() {
        let installed: HashMap<String, String> = HashMap::new();
        let required: HashMap<String, String> = HashMap::new();
        let deps = build_dependency_list(&installed, &required);
        assert_eq!(deps.len(), KEY_DEPENDENCIES.len());
    }

    // ===== v3.2.1 新增测试：覆盖 `~=` 约束 + 元组比较 =====

    #[test]
    fn test_version_tuple_basic() {
        assert_eq!(version_tuple("2.13.4"), vec![2, 13, 4]);
        assert_eq!(version_tuple("0.7.1"), vec![0, 7, 1]);
        assert_eq!(version_tuple("1.0"), vec![1, 0]);
        assert_eq!(version_tuple(""), Vec::<u32>::new());
    }

    #[test]
    fn test_version_tuple_strip_suffix() {
        // 带预发布标签的版本号：1.0.0a1 → [1, 0, 0]（容错，跳过 a1）
        assert_eq!(version_tuple("1.0.0a1"), vec![1, 0, 0]);
    }

    #[test]
    fn test_version_gte_basic() {
        // 基础：数值比较
        assert!(version_gte("2.13.4", "2.0.0"));
        assert!(version_gte("2.0.0", "2.0.0"));
        assert!(!version_gte("1.0.0", "2.0.0"));
    }

    #[test]
    fn test_version_gte_zero_padding() {
        // 关键 bug 修复：字符串比较 "0.10.0" >= "0.7.0" 错误（'1' < '7'）
        // 元组比较正确
        assert!(version_gte("0.10.0", "0.7.0"));
        assert!(version_gte("0.8.0", "0.4.2"));
        assert!(!version_gte("0.4.2", "0.8.0"));
    }

    #[test]
    fn test_version_lt_basic() {
        assert!(version_lt("2.0.0", "3.0"));
        assert!(!version_lt("3.0.0", "3.0"));
        assert!(!version_lt("2.99.0", "3.0"));
    }

    #[test]
    fn test_version_satisfies_pydantic_compatible_release() {
        // v3.2.1 用户场景：pydantic 2.13.4 vs pydantic~=2.0
        // 之前误报 NeedsUpgrade，修复后应满足
        assert!(version_satisfies("2.13.4", "~=2.0"));
        assert!(version_satisfies("2.0.0", "~=2.0"));
        assert!(version_satisfies("2.99.99", "~=2.0"));
        // 跨主版本不满足
        assert!(!version_satisfies("3.0.0", "~=2.0"));
        assert!(!version_satisfies("1.9.9", "~=2.0"));
    }

    #[test]
    fn test_version_satisfies_ge_zero_padding() {
        // v3.2.1 修复：>=0.7.0 vs installed 0.10.0
        // 之前字符串比较错误，修复后元组比较正确
        assert!(version_satisfies("0.10.0", ">=0.7.0"));
        assert!(version_satisfies("0.8.0", ">=0.4.2"));
        assert!(version_satisfies("3.14.1", ">=3.11.8"));
        assert!(version_satisfies("5.13.0", ">=4.50.3"));
        // 边界
        assert!(version_satisfies("0.7.0", ">=0.7.0"));
        assert!(!version_satisfies("0.6.99", ">=0.7.0"));
    }

    #[test]
    fn test_version_satisfies_eq() {
        assert!(version_satisfies("2.4.0", "==2.4.0"));
        assert!(!version_satisfies("2.4.1", "==2.4.0"));
    }

    #[test]
    fn test_build_dep_list_pydantic_satisfies_compatible_release() {
        // v3.2.1 关键场景：完整端到端测试
        let installed = HashMap::from([("pydantic".to_string(), "2.13.4".to_string())]);
        let required = HashMap::from([("pydantic".to_string(), "~=2.0".to_string())]);

        let deps = build_dependency_list(&installed, &required);
        let pydantic_dep = deps.iter().find(|d| d.name == "pydantic").unwrap();
        assert!(
            matches!(pydantic_dep.status, DepStatus::Satisfied),
            "pydantic 2.13.4 vs ~=2.0 should be Satisfied, got {:?}",
            pydantic_dep.status
        );
    }

    #[test]
    fn test_build_dep_list_pydantic_settings() {
        // requirements.txt 中 pydantic-settings~=2.0 同样场景
        let installed = HashMap::from([("pydantic-settings".to_string(), "2.5.2".to_string())]);
        let required = HashMap::from([("pydantic-settings".to_string(), "~=2.0".to_string())]);

        let deps = build_dependency_list(&installed, &required);
        let dep = deps.iter().find(|d| d.name == "pydantic-settings").unwrap();
        assert!(matches!(dep.status, DepStatus::Satisfied));
    }
}
