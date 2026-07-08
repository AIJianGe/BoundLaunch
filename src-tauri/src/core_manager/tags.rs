//! Stable tag 识别 + 分类（v3.1 / F26 决策 9：SemVer 规则）
//!
//! 详见 `PR/03-模块设计/03-CoreManager.md §4.2 Stable 识别`

use once_cell::sync::Lazy;
use regex::Regex;

use super::models::{ClassifiedTags, TagInfo};

/// 严格稳定版本号正则：`vX.Y.Z`
static STABLE_TAG_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^v\d+\.\d+\.\d+$").expect("invalid regex"));

/// 预发布版本号正则：`vX.Y.Z-<suffix>`（suffix = rc/beta/pre/dev/alpha 等）
static PRERELEASE_TAG_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^v\d+\.\d+\.\d+-(rc|beta|alpha|pre|dev)(\d*|\.\d+)*$")
        .expect("invalid regex")
});

/// 后缀黑名单（rc / beta / pre / dev 等）
const SUFFIX_BLACKLIST: &[&str] = &["rc", "beta", "pre", "dev", "alpha"];

/// 判断 tag 是否为稳定版
///
/// 规则：
/// - 严格匹配 `vX.Y.Z` 格式
/// - 不含 `rc` / `beta` / `pre` / `dev` / `alpha` 后缀
pub fn is_stable_tag(name: &str) -> bool {
    if !STABLE_TAG_RE.is_match(name) {
        return false;
    }
    let lower = name.to_lowercase();
    !SUFFIX_BLACKLIST.iter().any(|s| lower.contains(s))
}

/// 判断 tag 是否为预发布版（v3.1 / F26 决策 9）
///
/// 规则：匹配 `vX.Y.Z-<预发布后缀>` 格式
pub fn is_prerelease_tag(name: &str) -> bool {
    PRERELEASE_TAG_RE.is_match(name)
}

/// 判断 tag 是否为有效版本号（stable 或 prerelease）
pub fn is_version_tag(name: &str) -> bool {
    is_stable_tag(name) || is_prerelease_tag(name)
}

/// 从 tag 列表过滤出稳定版
pub fn filter_stable_tags(tags: &[TagInfo]) -> Vec<TagInfo> {
    tags.iter().filter(|t| t.is_stable).cloned().collect()
}

/// 从 tag 列表过滤出预发布版
pub fn filter_prerelease_tags(tags: &[TagInfo]) -> Vec<TagInfo> {
    tags.iter()
        .filter(|t| !t.is_stable && is_prerelease_tag(&t.name))
        .cloned()
        .collect()
}

/// 把 tag 列表分类为 stable / prerelease 两组（v3.1 / F26 决策 7：NTab 双分类）
///
/// 非 SemVer 格式的 tag（如 `latest` / `master`）会被过滤掉。
pub fn classify_tags(tags: Vec<TagInfo>) -> ClassifiedTags {
    let mut stable: Vec<TagInfo> = tags
        .iter()
        .filter(|t| t.is_stable)
        .cloned()
        .collect();
    let mut prerelease: Vec<TagInfo> = tags
        .into_iter()
        .filter(|t| !t.is_stable && is_prerelease_tag(&t.name))
        .collect();

    // 按 SemVer 倒序（v3.3 / F33：原字符串倒序存在 `v0.9.2` > `v0.27.0` 错误）
    stable.sort_by(|a, b| crate::core_manager::semver::cmp_tag_desc(&a.name, &b.name));
    prerelease.sort_by(|a, b| crate::core_manager::semver::cmp_tag_desc(&a.name, &b.name));

    ClassifiedTags { stable, prerelease }
}

/// 找出最新的稳定版 tag（v3.3 / F33）
///
/// 要求 tags 已按 SemVer 倒序排列（见 `git_ops::list_tags` / `classify_tags`）。
/// 双重保险：本函数内部也会用 `cmp_tag_desc` 找最大稳定版，即使上游排序
/// 退化为字符串序也能得到正确结果。
pub fn latest_stable(tags: &[TagInfo]) -> Option<String> {
    tags.iter()
        .filter(|t| t.is_stable)
        .max_by(|a, b| crate::core_manager::semver::cmp_tag_desc(&a.name, &b.name))
        .map(|t| t.name.clone())
}

/// v3.10 引导安装默认版本选择规则
///
/// 需求：在设置/引导安装流程中，默认安装「发布日期最后 + 后缀为 .0 或 .1」的稳定版。
///
/// 业务含义：
/// - ComfyUI 每次发布"主版本"（如 v0.27.0 / v0.28.0）通常会带若干 patch 修复（v0.27.1 / v0.27.2 / ...）
/// - 用户希望：默认安装的是"主版本刚刚发布"或"第一个 patch"的稳定版
/// - 避免默认选到 v0.27.5（虽然更新，但已经历多次 patch，可能引入未验证的修改）
///
/// 规则：
/// 1. 必须是稳定版（`is_stable_tag` 通过）
/// 2. 版本号的第三段（patch）必须是 `0` 或 `1`
/// 3. **v3.11.8 方案 B 兜底**：若 tags 中存在 `v0.27.0`，直接返回它
///    - 用户明确要求"引导安装固定死安装 v0.27.0"
///    - 这是绝对兜底，确保即使 date 排序/缓存有问题也一定选 v0.27.0
/// 4. **v3.11.8 方案 A**：过滤掉 `major==0 && minor==0` 的远古占位 tag（v0.0.x）
///    - 原因：`is_patch_zero_or_one("v0.0.1")` 返回 true（patch=1），
///      v0.0.1 会通过过滤；如果其 date 解析异常（如 LogStore 持久化缓存残留
///      v3.11.7 修复前的 Utc::now() fallback 值），会错误地赢过 v0.27.0
///    - v0.0.x 是 ComfyUI 早期占位 tag，不应作为安装默认版
/// 5. **v3.11.6 关键修复**：按 **tag date** 倒序选择（不再用 SemVer 比较）
///    - 原因：ComfyUI 的 tag 历史是非单调的（v1.0.0/v1.0.1 是 2017 年的老 tag，
///      v0.27.0 是 2025 年的新版），SemVer 比较会错误地选 v1.0.1
///    - 需求文档本来就写的是"发布日期最后"，现在实现终于与文档一致
/// 6. ~~v3.10 `is_first_major_release` 过滤~~（v3.11.6 移除：改用 date 排序后不再需要）
///
/// 示例（假设 tags: v0.0.1(2017) / v0.27.0(2025) / v0.27.1(2025) / v0.27.5(2025) / v1.0.1(2017)）：
/// - 方案 B 命中：直接返回 **v0.27.0** ✓
/// - 若无 v0.27.0：第一轮过滤（稳定 + patch=0/1 + 非远古）剩 v0.27.0 / v0.27.1
/// - 按 date 倒序取最新：**v0.27.1**（2025 年）✓
///
/// 兜底：若过滤后无任何 tag，回退到 `latest_stable`（行为等同之前）
pub fn latest_stable_for_installation(tags: &[TagInfo]) -> Option<String> {
    // 方案 B：v0.27.0 绝对优先（用户明确要求固定安装 v0.27.0）
    if let Some(v) = tags.iter().find(|t| t.name == "v0.27.0") {
        tracing::info!(
            date = ?v.date,
            "installation-default: hardcoded v0.27.0 found, returning it directly"
        );
        return Some(v.name.clone());
    }

    tags.iter()
        .filter(|t| t.is_stable)
        .filter(|t| is_patch_zero_or_one(&t.name))
        // 方案 A：过滤远古占位 tag（major==0 && minor==0，即 v0.0.x）
        // 防止 v0.0.1 等 patch=1 的远古 tag 在 date 异常时被误选
        .filter(|t| !is_legacy_placeholder_tag(&t.name))
        // v3.11.7：date 为主排序键，SemVer 为 tiebreaker
        // - date 确保老 tag（v1.0.0/v1.0.1 从 2017-2023）不会赢过新 tag（v0.28.0 从 2025）
        // - SemVer tiebreaker 防止相同 date 的 tag 随机选中
        .max_by(|a, b| {
            a.date.cmp(&b.date)
                .then_with(|| crate::core_manager::semver::cmp_tag_desc(&a.name, &b.name))
        })
        .map(|t| t.name.clone())
        .or_else(|| latest_stable(tags))
}

/// 判断 tag 是否为"首次大版本发布"（v3.10 新增）
///
/// 规则：
/// - 必须是稳定版格式 vX.Y.Z
/// - Y == 0 && Z == 0（即 X.0.0）
/// - X 是 tag 列表中最大的主版本号
///
/// 命中示例：
/// - tags = [v0.27.0, v0.27.1, v1.0.0] → v1.0.0 是首次大版本（max_major=1）
/// - tags = [v0.27.0, v1.0.0, v1.0.1] → v1.0.0 仍是首次大版本
///   （v1.x 已发布但首次仍跳过，避免把刚升级的 v1.0.0 当默认）
/// - tags = [v0.27.0, v0.27.1] → 无首次大版本（max_major=0）
pub fn is_first_major_release(name: &str, tags: &[TagInfo]) -> bool {
    let parts: Vec<&str> = name.trim_start_matches('v').split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    let major: u32 = match parts[0].parse() {
        Ok(n) => n,
        Err(_) => return false,
    };
    // 必须是 X.0.0
    if parts[1] != "0" || parts[2] != "0" {
        return false;
    }
    // 必须 major == max_major
    let max_major = tags
        .iter()
        .filter(|t| t.is_stable)
        .filter_map(|t| {
            let ps: Vec<&str> = t.name.trim_start_matches('v').split('.').collect();
            if ps.len() != 3 {
                return None;
            }
            ps[0].parse::<u32>().ok()
        })
        .max();
    match max_major {
        Some(m) if m == major => true,
        _ => false,
    }
}

/// 判断 tag 的 patch 段是否 = 0 或 1
///
/// 例：
/// - `v0.27.0` → true
/// - `v0.27.1` → true
/// - `v0.27.2` → false
/// - `v0.27.10` → false
/// - `v0.27` → false（不符合稳定版格式，先由 is_stable 过滤）
fn is_patch_zero_or_one(name: &str) -> bool {
    // 仅对稳定版格式 vX.Y.Z 解析（v0.27.10 不会和 v0.27.1 混淆）
    let parts: Vec<&str> = name.trim_start_matches('v').split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    match parts[2].parse::<u32>() {
        Ok(p) => p == 0 || p == 1,
        Err(_) => false,
    }
}

/// v3.11.8 方案 A：判断 tag 是否为"远古占位 tag"（major==0 && minor==0，即 v0.0.x）
///
/// 用途：在 `latest_stable_for_installation` 中过滤掉这类 tag。
///
/// 背景：
/// - `is_patch_zero_or_one("v0.0.1")` 返回 true（patch=1），v0.0.1 会通过过滤
/// - ComfyUI 仓库历史中 v0.0.x 是早期占位 tag，不应作为安装默认版
/// - 若其 date 解析异常（如缓存残留旧 fallback 值），会错误地赢过 v0.27.0
///
/// 例：
/// - `v0.0.1` → true（major=0, minor=0）
/// - `v0.0.0` → true
/// - `v0.27.0` → false（minor=27 ≠ 0）
/// - `v1.0.0` → false（major=1 ≠ 0）
fn is_legacy_placeholder_tag(name: &str) -> bool {
    let parts: Vec<&str> = name.trim_start_matches('v').split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    let major: u32 = match parts[0].parse() {
        Ok(n) => n,
        Err(_) => return false,
    };
    let minor: u32 = match parts[1].parse() {
        Ok(n) => n,
        Err(_) => return false,
    };
    major == 0 && minor == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_stable_tag_strict_version() {
        assert!(is_stable_tag("v0.2.0"));
        assert!(is_stable_tag("v1.10.5"));
    }

    #[test]
    fn test_is_stable_tag_rejects_non_version() {
        assert!(!is_stable_tag("latest"));
        assert!(!is_stable_tag("master"));
        assert!(!is_stable_tag("v0.2"));
        assert!(!is_stable_tag("v0.2.0.0"));
    }

    #[test]
    fn test_is_stable_tag_rejects_pre_release() {
        assert!(!is_stable_tag("v0.2.0-rc1"));
        assert!(!is_stable_tag("v0.2.0-beta"));
        assert!(!is_stable_tag("v0.2.0-pre"));
        assert!(!is_stable_tag("v0.2.0-dev1"));
        assert!(!is_stable_tag("v0.2.0-alpha.1"));
    }

    #[test]
    fn test_is_prerelease_tag_accepts_valid() {
        assert!(is_prerelease_tag("v0.2.0-rc1"));
        assert!(is_prerelease_tag("v0.2.0-beta"));
        assert!(is_prerelease_tag("v0.2.0-pre1"));
        assert!(is_prerelease_tag("v0.2.0-dev1"));
        assert!(is_prerelease_tag("v0.2.0-alpha.1"));
        assert!(is_prerelease_tag("v0.2.0-rc.2"));
    }

    #[test]
    fn test_is_prerelease_tag_rejects_stable() {
        assert!(!is_prerelease_tag("v0.2.0"));
        assert!(!is_prerelease_tag("v1.10.5"));
    }

    #[test]
    fn test_is_prerelease_tag_rejects_non_version() {
        assert!(!is_prerelease_tag("latest"));
        assert!(!is_prerelease_tag("master"));
        assert!(!is_prerelease_tag("v0.2.0-unknown"));
    }

    #[test]
    fn test_classify_tags_separates_stable_and_prerelease() {
        let tags = vec![
            TagInfo {
                name: "v0.3.10".to_string(),
                is_stable: true,
                commit: "c1".to_string(),
                date: chrono::Utc::now(),
            },
            TagInfo {
                name: "v0.3.10-rc1".to_string(),
                is_stable: false,
                commit: "c2".to_string(),
                date: chrono::Utc::now(),
            },
            TagInfo {
                name: "v0.3.9".to_string(),
                is_stable: true,
                commit: "c3".to_string(),
                date: chrono::Utc::now(),
            },
            TagInfo {
                name: "latest".to_string(),
                is_stable: false,
                commit: "c4".to_string(),
                date: chrono::Utc::now(),
            },
        ];

        let classified = classify_tags(tags);
        assert_eq!(classified.stable.len(), 2);
        assert_eq!(classified.prerelease.len(), 1);
        assert_eq!(classified.stable[0].name, "v0.3.10");
        assert_eq!(classified.stable[1].name, "v0.3.9");
        assert_eq!(classified.prerelease[0].name, "v0.3.10-rc1");
    }

    fn make_tag(name: &str, is_stable: bool) -> TagInfo {
        TagInfo {
            name: name.to_string(),
            is_stable,
            commit: "c".to_string(),
            date: chrono::Utc::now(),
        }
    }

    #[test]
    fn test_is_patch_zero_or_one() {
        assert!(is_patch_zero_or_one("v0.27.0"));
        assert!(is_patch_zero_or_one("v0.27.1"));
        assert!(!is_patch_zero_or_one("v0.27.2"));
        assert!(!is_patch_zero_or_one("v0.27.10"));
        assert!(!is_patch_zero_or_one("v0.27"));
        assert!(!is_patch_zero_or_one("v0.27.0-rc1"));
    }

    #[test]
    fn test_latest_stable_for_installation_picks_zero_or_one() {
        // 模拟：v0.27.5 是最新 SemVer，但 patch=5 不符合规则
        // 应选 v0.28.1（最大的 patch=0/1）
        let tags = vec![
            make_tag("v0.27.0", true),
            make_tag("v0.27.1", true),
            make_tag("v0.27.5", true),
            make_tag("v0.28.0", true),
            make_tag("v0.28.1", true),
        ];
        assert_eq!(
            latest_stable_for_installation(&tags),
            Some("v0.28.1".to_string())
        );
    }

    #[test]
    fn test_latest_stable_for_installation_prefers_zero() {
        // v0.29.0 应该是首选（patch=0，且 SemVer 更大）
        let tags = vec![
            make_tag("v0.28.0", true),
            make_tag("v0.28.1", true),
            make_tag("v0.29.0", true),
            make_tag("v0.29.1", true),
        ];
        assert_eq!(
            latest_stable_for_installation(&tags),
            Some("v0.29.1".to_string())
        );
    }

    #[test]
    fn test_latest_stable_for_installation_fallback() {
        // 全是 patch >= 2 时，回退到 latest_stable
        let tags = vec![
            make_tag("v0.27.2", true),
            make_tag("v0.27.5", true),
        ];
        assert_eq!(
            latest_stable_for_installation(&tags),
            Some("v0.27.5".to_string())
        );
    }

    #[test]
    fn test_latest_stable_for_installation_ignores_prerelease() {
        // 预发布不应被选（即使符合 patch=0/1）
        let tags = vec![
            make_tag("v0.28.0-rc1", false),
            make_tag("v0.27.0", true),
            make_tag("v0.27.1", true),
        ];
        assert_eq!(
            latest_stable_for_installation(&tags),
            Some("v0.27.1".to_string())
        );
    }

    // ============================================================================
    // v3.10 新增：「首次大版本跳过」规则相关测试
    // ============================================================================

    #[test]
    fn test_is_first_major_release_basic() {
        // v1.0.0 是 max_major=1 的首次大版本
        let tags = vec![make_tag("v0.27.0", true), make_tag("v1.0.0", true)];
        assert!(is_first_major_release("v1.0.0", &tags));
        // v0.27.0 不是首次大版本（max_major=1，major=0 不等）
        assert!(!is_first_major_release("v0.27.0", &tags));
        // v0.28.0 不是首次大版本（min != 0）
        assert!(!is_first_major_release("v0.28.0", &tags));
    }

    #[test]
    fn test_latest_stable_for_installation_skips_v1_first_release() {
        // 核心场景：v0.3.10 + v1.0.0 → 选 v0.3.10
        let tags = vec![
            make_tag("v0.3.9", true),
            make_tag("v0.3.10", true),
            make_tag("v1.0.0", true),
        ];
        assert_eq!(
            latest_stable_for_installation(&tags),
            Some("v0.3.10".to_string())
        );
    }

    #[test]
    fn test_latest_stable_for_installation_still_skips_v1_0_0_even_with_v1_0_1() {
        // tags = [v0.27.1, v1.0.0, v1.0.1]
        // v1.0.0 仍是首次大版本 → 跳过
        // v1.0.1 patch=1 但 max_major=1 → 也是首次大版本的后续 → 不在"patch=0/1 + 非首次"中
        // 应选 v0.27.1
        let tags = vec![
            make_tag("v0.27.1", true),
            make_tag("v1.0.0", true),
            make_tag("v1.0.1", true),
        ];
        assert_eq!(
            latest_stable_for_installation(&tags),
            Some("v0.27.1".to_string())
        );
    }

    #[test]
    fn test_latest_stable_for_installation_fallback_to_latest_stable_when_only_v1() {
        // 极端兜底：tags 里只有 v1.0.0，没有 v0.x
        // 主规则（patch=0/1 + 非首次大版本）过滤后空 → or_else 回退到 latest_stable
        let tags = vec![make_tag("v1.0.0", true)];
        assert_eq!(
            latest_stable_for_installation(&tags),
            Some("v1.0.0".to_string())
        );
    }

    #[test]
    fn test_latest_stable_for_installation_does_not_apply_to_v0() {
        // tags = [v0.27.0, v0.28.0, v0.28.1]
        // max_major=0 → 没有"首次大版本"概念
        // 按原规则选最大的 patch=0/1
        let tags = vec![
            make_tag("v0.27.0", true),
            make_tag("v0.28.0", true),
            make_tag("v0.28.1", true),
        ];
        assert_eq!(
            latest_stable_for_installation(&tags),
            Some("v0.28.1".to_string())
        );
    }
}
