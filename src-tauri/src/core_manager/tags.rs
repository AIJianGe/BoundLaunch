//! Stable tag 识别
//!
//! 详见 `PR/03-模块设计/03-CoreManager.md §4.2 Stable 识别`

use once_cell::sync::Lazy;
use regex::Regex;

/// 严格稳定版本号正则：`vX.Y.Z`
static STABLE_TAG_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^v\d+\.\d+\.\d+$").expect("invalid regex"));

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

/// 从 tag 列表过滤出稳定版
pub fn filter_stable_tags(tags: &[super::models::TagInfo]) -> Vec<super::models::TagInfo> {
    tags.iter().filter(|t| t.is_stable).cloned().collect()
}

/// 找出最新的稳定版 tag
///
/// 假设 tag 列表已按版本倒序排列（git tag --sort=-v:refname）
pub fn latest_stable(tags: &[super::models::TagInfo]) -> Option<String> {
    tags.iter()
        .find(|t| t.is_stable)
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
}
