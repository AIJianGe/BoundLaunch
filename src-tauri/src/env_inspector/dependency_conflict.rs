//! v3.0 依赖冲突检测模块
//!
//! **目标**：扫描 `<comfyui_root>/custom_nodes/*/requirements.txt`，
//! 检测同一 Python 包被多个自定义节点以不同版本约束引用的情况。
//!
//! **设计原则**：
//! - **只检测不解决**：v3.0 让用户自己找兼容版本（升级节点 / 降级包版本）
//! - **不阻塞启动**：检测到冲突只 toast 提示，不影响 ComfyUI 启动
//! - **简化解析**：用正则提取包名 + 版本约束，跳过环境标记 / extras / URL
//!
//! **后续演进**（v4+）：
//! - 升级为「自动降级到最低版本」决策
//! - 引入 pip-audit / uv lock 风格的强校验
//!
//! ---
//!
//! ## requirements.txt 语法子集（v3.0 支持）
//! ```text
//! # 注释：以 # 开头
//! 包名
//! 包名==版本
//! 包名>=版本
//! 包名<=版本
//! 包名~=版本
//! 包名!=版本
//! 包名[extras]
//! 包名>=1.0 ; python_version >= '3.10'
//! ```
//!
//! ## 不支持的语法（v3.0 warn 跳过）
//! ```text
//! -r other.txt            ← 嵌套文件
//! git+https://...         ← VCS 安装
//! package @ url           ← URL 安装
//! ./local/path            ← 本地路径
//! ```
//!
//! ## 单元测试覆盖
//! - 空 custom_nodes 目录
//! - 单一节点、单一包
//! - 多节点同一包不同版本（冲突）
//! - 多节点同一包相同版本（不冲突）
//! - 注释行、空行、嵌套文件（跳过）
//! - 环境标记、extras、URL 跳过

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;
use tracing::{debug, warn};

/// 单个包约束（来源 = 某个节点的 requirements.txt）
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PackageConstraint {
    /// 包名（小写）
    pub name: String,
    /// 原始约束字符串（不含 extras / 环境标记）
    /// 例：`==4.30.0` / `>=4.0` / ``（无版本约束）
    pub constraint: String,
    /// 节点名（custom_nodes 子目录名）
    pub node_name: String,
    /// 源文件相对路径
    pub source_file: String,
}

/// 冲突严重度
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ConflictSeverity {
    /// 小版本冲突（如 4.30.0 vs 4.30.1）—— pip 自动选最高版本即可
    Patch,
    /// 严格不兼容（如 4.30.0 vs 5.0.0，节点 A 锁 4.30，节点 B 锁 5.0）
    Major,
    /// 范围冲突（如 >=4.0 vs ==4.30.0）—— 自动选 4.30.0 即可
    Minor,
}

impl ConflictSeverity {
    /// 根据约束列表推断严重度
    ///
    /// **简化策略**：
    /// - 任一约束是 `==X.Y.0` 且主版本号不同 → Major
    /// - 主版本相同、次版本号不同 → Patch（pip 会选最高）
    /// - 含范围约束（>= <=）且严格冲突 → Minor
    pub fn from_constraints(constraints: &[PackageConstraint]) -> Self {
        if constraints.len() < 2 {
            return ConflictSeverity::Minor;
        }

        let eq_versions: Vec<String> = constraints
            .iter()
            .filter_map(|c| {
                c.constraint
                    .strip_prefix("==")
                    .map(|v| v.to_string())
            })
            .collect();

        if eq_versions.len() < 2 {
            // 没有足够的 == 约束，无法判断
            return ConflictSeverity::Minor;
        }

        // 取主版本号
        let majors: Vec<String> = eq_versions
            .iter()
            .filter_map(|v| v.split('.').next().map(|s| s.to_string()))
            .collect();

        let minors: Vec<String> = eq_versions
            .iter()
            .filter_map(|v| {
                let parts: Vec<&str> = v.split('.').collect();
                if parts.len() >= 2 {
                    Some(format!("{}.{}", parts[0], parts[1]))
                } else {
                    None
                }
            })
            .collect();

        let unique_majors: std::collections::HashSet<_> = majors.iter().collect();
        let unique_minors: std::collections::HashSet<_> = minors.iter().collect();

        if unique_majors.len() > 1 {
            ConflictSeverity::Major
        } else if unique_minors.len() > 1 {
            ConflictSeverity::Patch
        } else {
            ConflictSeverity::Minor
        }
    }

    /// 中文描述
    pub fn label(&self) -> &'static str {
        match self {
            ConflictSeverity::Patch => "小版本冲突（pip 会自动选最高版本）",
            ConflictSeverity::Major => "主版本冲突（需用户决策）",
            ConflictSeverity::Minor => "范围冲突（一般可自动解决）",
        }
    }
}

/// 冲突项
#[derive(Debug, Clone, Serialize)]
pub struct Conflict {
    /// 包名
    pub name: String,
    /// 严重度
    pub severity: ConflictSeverity,
    /// 受影响的约束
    pub constraints: Vec<PackageConstraint>,
    /// 建议
    pub suggestion: String,
    /// 受影响的节点列表
    pub affected_nodes: Vec<String>,
}

/// 完整冲突报告
#[derive(Debug, Clone, Serialize)]
pub struct ConflictReport {
    /// 扫描到的节点列表
    pub scanned_nodes: Vec<String>,
    /// 解析出的总包数（去重前）
    pub total_packages: usize,
    /// 唯一包数
    pub unique_packages: usize,
    /// 冲突列表
    pub conflicts: Vec<Conflict>,
    /// 无冲突
    pub clean: bool,
    /// 扫描耗时（毫秒）
    pub scan_duration_ms: u64,
}

/// 扫描 `<comfyui_root>/custom_nodes/*/requirements.txt`
///
/// **不递归**（v3.0 不支持 `-r other.txt` 嵌套）
pub fn scan_custom_node_requirements(comfyui_root: &Path) -> ConflictReport {
    let start = std::time::Instant::now();
    let custom_nodes_dir = comfyui_root.join("custom_nodes");

    // 1. 检查 custom_nodes 目录
    if !custom_nodes_dir.exists() {
        debug!(
            path = %custom_nodes_dir.display(),
            "custom_nodes 目录不存在，跳过冲突检测"
        );
        return ConflictReport {
            scanned_nodes: vec![],
            total_packages: 0,
            unique_packages: 0,
            conflicts: vec![],
            clean: true,
            scan_duration_ms: start.elapsed().as_millis() as u64,
        };
    }

    // 2. 扫描所有节点的 requirements.txt
    let mut all_constraints: Vec<PackageConstraint> = Vec::new();
    let mut scanned_nodes: Vec<String> = Vec::new();

    let entries = match std::fs::read_dir(&custom_nodes_dir) {
        Ok(e) => e,
        Err(e) => {
            warn!(error = %e, "读取 custom_nodes 目录失败");
            return ConflictReport {
                scanned_nodes: vec![],
                total_packages: 0,
                unique_packages: 0,
                conflicts: vec![],
                clean: true,
                scan_duration_ms: start.elapsed().as_millis() as u64,
            };
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let node_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };

        // 跳过以 . 开头的目录（隐藏目录）
        if node_name.starts_with('.') {
            continue;
        }

        let req_file = path.join("requirements.txt");
        if !req_file.exists() {
            debug!(node = %node_name, "节点无 requirements.txt，跳过");
            continue;
        }

        scanned_nodes.push(node_name.clone());

        let content = match std::fs::read_to_string(&req_file) {
            Ok(c) => c,
            Err(e) => {
                warn!(node = %node_name, error = %e, "读取 requirements.txt 失败");
                continue;
            }
        };

        let relative_path = format!("custom_nodes/{}/requirements.txt", node_name);
        let constraints = parse_requirements(&content, &node_name, &relative_path);
        all_constraints.extend(constraints);
    }

    // 3. 按包名分组
    let mut by_name: HashMap<String, Vec<PackageConstraint>> = HashMap::new();
    for c in &all_constraints {
        by_name.entry(c.name.clone()).or_default().push(c.clone());
    }

    // 4. 检测冲突（同一包名有多个不同约束）
    let mut conflicts: Vec<Conflict> = Vec::new();
    for (name, constraints) in by_name.iter() {
        if constraints.len() < 2 {
            continue; // 只被一个节点引用，无冲突
        }

        // 检查是否所有约束都相同（无冲突）
        let unique_constraints: std::collections::HashSet<&str> = constraints
            .iter()
            .map(|c| c.constraint.as_str())
            .collect();
        if unique_constraints.len() < 2 {
            continue; // 多节点引用同一版本，无冲突
        }

        let severity = ConflictSeverity::from_constraints(constraints);
        let suggestion = make_suggestion(constraints);
        let affected_nodes: Vec<String> = constraints
            .iter()
            .map(|c| c.node_name.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        conflicts.push(Conflict {
            name: name.clone(),
            severity,
            constraints: constraints.clone(),
            suggestion,
            affected_nodes,
        });
    }

    // 5. 按严重度排序（Major > Minor > Patch）
    conflicts.sort_by(|a, b| {
        let order = |s: &ConflictSeverity| match s {
            ConflictSeverity::Major => 0,
            ConflictSeverity::Minor => 1,
            ConflictSeverity::Patch => 2,
        };
        order(&a.severity).cmp(&order(&b.severity))
    });

    let total_packages = all_constraints.len();
    let unique_packages = by_name.len();
    let clean = conflicts.is_empty();
    let scan_duration_ms = start.elapsed().as_millis() as u64;

    ConflictReport {
        scanned_nodes,
        total_packages,
        unique_packages,
        conflicts,
        clean,
        scan_duration_ms,
    }
}

/// 解析 requirements.txt 内容
///
/// 返回所有解析出的 PackageConstraint
fn parse_requirements(
    content: &str,
    node_name: &str,
    source_file: &str,
) -> Vec<PackageConstraint> {
    let mut result = Vec::new();

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // 跳过嵌套引用（-r other.txt）
        if line.starts_with("-r ") || line.starts_with("--requirement ") {
            debug!(line = line, "跳过嵌套 -r 引用");
            continue;
        }

        // 跳过 URL / 本地路径安装
        if line.contains(" @ ")
            || line.starts_with("git+")
            || line.starts_with("http://")
            || line.starts_with("https://")
            || line.starts_with("./")
            || line.starts_with("../")
        {
            debug!(line = line, "跳过 URL/路径安装");
            continue;
        }

        // 切掉环境标记：包名; python_version >= '3.10'
        let line = line.split(';').next().unwrap_or(line).trim();

        // 切掉 extras：[torch]
        let line = if let Some(idx) = line.find('[') {
            let end = line.find(']').unwrap_or(line.len());
            format!("{}{}", &line[..idx], &line[end + 1..])
        } else {
            line.to_string()
        };

        // 提取包名 + 约束
        // 正则：[a-zA-Z0-9_.-]+ 开头，后跟可选的 (==|>=|<=|~=|!=|>|<) 约束
        let (name, constraint) = split_name_constraint(&line);

        if name.is_empty() {
            warn!(line = raw_line, "无法解析 requirements.txt 行");
            continue;
        }

        result.push(PackageConstraint {
            name: name.to_lowercase(),
            constraint,
            node_name: node_name.to_string(),
            source_file: source_file.to_string(),
        });
    }

    result
}

/// 拆分包名和版本约束
///
/// 简化实现：扫描到首个 `==|>=|<=|~=|!=|>|<` 视为约束起点
fn split_name_constraint(line: &str) -> (String, String) {
    let operators = ["==", ">=", "<=", "~=", "!=", ">", "<"];
    for op in operators {
        if let Some(idx) = line.find(op) {
            let name = line[..idx].trim().to_string();
            let constraint = line[idx..].trim().to_string();
            return (name, constraint);
        }
    }
    // 无约束
    (line.trim().to_string(), String::new())
}

/// 根据约束列表生成建议
fn make_suggestion(constraints: &[PackageConstraint]) -> String {
    let eq_versions: Vec<&str> = constraints
        .iter()
        .filter_map(|c| c.constraint.strip_prefix("=="))
        .collect();

    if eq_versions.is_empty() {
        return "建议在所有节点中升级到最新稳定版，或保持当前 pip 默认选择".to_string();
    }

    if eq_versions.len() == 1 {
        return format!(
            "保留 {} 版本，但其他节点可能需要降级或升级",
            eq_versions[0]
        );
    }

    // 多个 == 约束：选最高的
    let max_version = eq_versions
        .iter()
        .max_by(|a, b| compare_version_strings(a, b))
        .copied()
        .unwrap_or("");

    if max_version.is_empty() {
        return "请用户决策保留哪个版本".to_string();
    }

    let mut low_version_nodes: Vec<&str> = constraints
        .iter()
        .filter(|c| {
            c.constraint.strip_prefix("==") != Some(max_version) && !c.constraint.is_empty()
        })
        .map(|c| c.node_name.as_str())
        .collect();
    low_version_nodes.sort();
    low_version_nodes.dedup();

    if low_version_nodes.is_empty() {
        format!("保留版本 {}", max_version)
    } else {
        format!(
            "建议装 {}（最高），但节点 [{}] 可能不兼容，请用户决策",
            max_version,
            low_version_nodes.join(", ")
        )
    }
}

/// 比较版本字符串（简化：按 . 分割后按段比较）
fn compare_version_strings(a: &str, b: &str) -> std::cmp::Ordering {
    let va: Vec<u32> = a
        .split('.')
        .filter_map(|s| {
            // 截掉非数字后缀（如 "4.30.0a1"）
            s.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .ok()
        })
        .collect();
    let vb: Vec<u32> = b
        .split('.')
        .filter_map(|s| {
            s.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .ok()
        })
        .collect();
    va.cmp(&vb)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_parse_requirements_simple() {
        let content = "torch>=2.0\nnumpy\nsafetensors==0.4.0\n# comment\n\n";
        let result = parse_requirements(content, "node1", "node1/requirements.txt");
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].name, "torch");
        assert_eq!(result[0].constraint, ">=2.0");
        assert_eq!(result[1].name, "numpy");
        assert_eq!(result[1].constraint, "");
        assert_eq!(result[2].name, "safetensors");
        assert_eq!(result[2].constraint, "==0.4.0");
    }

    #[test]
    fn test_parse_requirements_with_extras_and_markers() {
        let content = "transformers[torch]==4.38.0\ntorchsde; python_version >= '3.10'";
        let result = parse_requirements(content, "node1", "node1/requirements.txt");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].name, "transformers");
        assert_eq!(result[0].constraint, "==4.38.0");
        assert_eq!(result[1].name, "torchsde");
        assert_eq!(result[1].constraint, ""); // 环境标记被切掉，无约束
    }

    #[test]
    fn test_parse_requirements_skip_url() {
        let content = "git+https://github.com/xxx/yyy.git\npackage @ file:///local\n-r other.txt\nnumpy";
        let result = parse_requirements(content, "node1", "node1/requirements.txt");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "numpy");
    }

    #[test]
    fn test_severity_major() {
        let constraints = vec![
            PackageConstraint {
                name: "transformers".to_string(),
                constraint: "==4.30.0".to_string(),
                node_name: "a".to_string(),
                source_file: "a/req.txt".to_string(),
            },
            PackageConstraint {
                name: "transformers".to_string(),
                constraint: "==5.0.0".to_string(),
                node_name: "b".to_string(),
                source_file: "b/req.txt".to_string(),
            },
        ];
        assert_eq!(
            ConflictSeverity::from_constraints(&constraints),
            ConflictSeverity::Major
        );
    }

    #[test]
    fn test_severity_patch() {
        let constraints = vec![
            PackageConstraint {
                name: "transformers".to_string(),
                constraint: "==4.30.0".to_string(),
                node_name: "a".to_string(),
                source_file: "a/req.txt".to_string(),
            },
            PackageConstraint {
                name: "transformers".to_string(),
                constraint: "==4.30.1".to_string(),
                node_name: "b".to_string(),
                source_file: "b/req.txt".to_string(),
            },
        ];
        assert_eq!(
            ConflictSeverity::from_constraints(&constraints),
            ConflictSeverity::Patch
        );
    }

    #[test]
    fn test_compare_version_strings() {
        assert_eq!(
            compare_version_strings("4.30.0", "5.0.0"),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            compare_version_strings("5.0.0", "4.30.0"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare_version_strings("4.30.0", "4.30.0"),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn test_scan_empty_dir() {
        let tmp = tempdir();
        let report = scan_custom_node_requirements(&tmp);
        assert!(report.clean);
        assert_eq!(report.conflicts.len(), 0);
    }

    #[test]
    fn test_scan_with_conflict() {
        let tmp = tempdir();
        fs::create_dir(tmp.join("custom_nodes").join("node_a")).unwrap();
        fs::create_dir(tmp.join("custom_nodes").join("node_b")).unwrap();
        fs::write(
            tmp.join("custom_nodes")
                .join("node_a")
                .join("requirements.txt"),
            "transformers==4.30.0",
        )
        .unwrap();
        fs::write(
            tmp.join("custom_nodes")
                .join("node_b")
                .join("requirements.txt"),
            "transformers==5.0.0",
        )
        .unwrap();

        let report = scan_custom_node_requirements(&tmp);
        assert!(!report.clean);
        assert_eq!(report.conflicts.len(), 1);
        assert_eq!(report.conflicts[0].name, "transformers");
        assert_eq!(report.conflicts[0].severity, ConflictSeverity::Major);
        assert_eq!(report.scanned_nodes.len(), 2);
    }

    #[test]
    fn test_scan_no_conflict_same_version() {
        let tmp = tempdir();
        fs::create_dir(tmp.join("custom_nodes").join("node_a")).unwrap();
        fs::create_dir(tmp.join("custom_nodes").join("node_b")).unwrap();
        fs::write(
            tmp.join("custom_nodes")
                .join("node_a")
                .join("requirements.txt"),
            "transformers==4.38.0",
        )
        .unwrap();
        fs::write(
            tmp.join("custom_nodes")
                .join("node_b")
                .join("requirements.txt"),
            "transformers==4.38.0",
        )
        .unwrap();

        let report = scan_custom_node_requirements(&tmp);
        assert!(report.clean);
        assert_eq!(report.conflicts.len(), 0);
    }

    fn tempdir() -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "boundlaunch_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }
}
