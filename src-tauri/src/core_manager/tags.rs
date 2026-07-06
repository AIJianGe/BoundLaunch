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
}
