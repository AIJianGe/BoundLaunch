//! 轻量 SemVer 解析 + 比较（v3.3 / F33）
//!
//! 用途：替代 `git_ops::list_tags` / `tags::classify_tags` 中的字符串排序，
//!       修复「v0.27.0 被误判为比 v0.9.2 旧」的根因。
//!
//! ## 范围（v3.3 / F33）
//! - 支持 `vX.Y.Z` 严格 SemVer
//! - 支持 `vX.Y.Z-<suffix>` 预发布后缀（`-rc1` / `-beta` / `-dev1` / `-alpha.1` 等）
//! - 支持 `vX.Y.Z+build` build 元数据（解析时忽略，比较时不参与）
//! - 非 SemVer 格式的 tag（如 `latest` / `master` / `v0.3`）排到列表最末
//!
//! ## 排序规则
//! - stable > prerelease（同号比较时，rc 视为更早）
//! - major / minor / patch 数字按整数比较（避免 `v0.9.x` > `v0.27.x` 字符串序错误）
//! - 预发布后缀的字典序比较（rc1 < rc2 < rc10）
//!
//! ## 为什么不引入 `semver` crate
//! - `semver` crate 接受标准 SemVer 但要求前缀是数字，本项目 tag 有 `v` 前缀
//! - 现有 tag 命名不完全符合 SemVer（如 `v0.3.40`、`v0.16.2` 都正常，但也有 `v0.9.2-rc.2`）
//! - 自己实现 ~80 行，覆盖项目所需场景，避免引入额外依赖
//!
//! 详见 `PR/03-模块设计/03-CoreManager.md §4.2 SemVer 排序（F33 新增）`

use std::cmp::Ordering;

/// 解析后的 SemVer 段
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemVer {
    /// 是否为严格 `vX.Y.Z` 稳定版（无 prerelease 后缀）
    pub is_stable: bool,
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    /// 预发布后缀（如 `rc1` / `beta` / `dev1` / `alpha.1`）
    /// 稳定版时为 `None`，与 `is_stable=true` 等价
    pub prerelease: Option<String>,
}

impl SemVer {
    /// 解析 tag 名（如 `v0.27.0`、`v0.9.2-rc1`、`v0.3.10-beta.2`）
    ///
    /// 非 SemVer 格式返回 `None`
    pub fn parse(tag: &str) -> Option<Self> {
        // 剥离 build metadata（`+...`）
        let core = tag.split('+').next().unwrap_or(tag);

        // 拆分主体与 prerelease 后缀
        let (body, prerelease) = match core.split_once('-') {
            Some((b, p)) => (b, Some(p.to_lowercase())),
            None => (core, None),
        };

        // 必须是 `vX.Y.Z` 格式
        let stripped = body.strip_prefix('v')?;
        let parts: Vec<&str> = stripped.split('.').collect();
        if parts.len() != 3 {
            return None;
        }

        let major: u32 = parts[0].parse().ok()?;
        let minor: u32 = parts[1].parse().ok()?;
        let patch: u32 = parts[2].parse().ok()?;

        // prerelease 后缀必须非空
        let prerelease = prerelease.filter(|p| !p.is_empty());

        Some(Self {
            is_stable: prerelease.is_none(),
            major,
            minor,
            patch,
            prerelease,
        })
    }

    /// 倒序比较：返回 `Ordering::Greater` 表示 self 更新
    ///
    /// 规则：
    /// 1. 都是有效 SemVer → 按 (major, minor, patch) 降序
    /// 2. 数字相同时：stable > prerelease
    /// 3. 都是 prerelease：后缀字典序（数字按数值比较）倒序
    /// 4. 无效 SemVer（None）排到最末
    pub fn cmp_desc(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .reverse()
            .then_with(|| {
                // stable 排前，prerelease 排后
                match (self.is_stable, other.is_stable) {
                    (true, false) => Ordering::Greater,
                    (false, true) => Ordering::Less,
                    (true, true) => Ordering::Equal,
                    (false, false) => {
                        // 两个都是 prerelease：后缀字典序（数字按数值）倒序
                        compare_prerelease_desc(
                            self.prerelease.as_deref().unwrap_or(""),
                            other.prerelease.as_deref().unwrap_or(""),
                        )
                    }
                }
            })
    }
}

/// 比较两个 prerelease 后缀（倒序）
///
/// 规则：
/// - 按 `.` 分段
/// - 段都是数字 → 按数值比较
/// - 段都是字母 → 按字典序
/// - 数字段 < 字母段（符合 SemVer 规范）
/// - 短的后缀（段少）排前（即 rc1 < rc1.0）
fn compare_prerelease_desc(a: &str, b: &str) -> Ordering {
    let segs_a: Vec<&str> = a.split('.').collect();
    let segs_b: Vec<&str> = b.split('.').collect();
    let len = segs_a.len().max(segs_b.len());

    for i in 0..len {
        let sa = segs_a.get(i).copied().unwrap_or("");
        let sb = segs_b.get(i).copied().unwrap_or("");

        let ord = match (sa.parse::<u64>(), sb.parse::<u64>()) {
            (Ok(na), Ok(nb)) => na.cmp(&nb),
            (Ok(_), Err(_)) => return Ordering::Less,    // 数字段 < 字母段 → 倒序后 Less
            (Err(_), Ok(_)) => return Ordering::Greater,
            (Err(_), Err(_)) => match sa.cmp(sb) {
                Ordering::Equal => continue,
                o => return o,
            },
        };
        match ord {
            Ordering::Equal => continue,
            o => return o.reverse(), // 倒序
        }
    }
    Ordering::Equal
}

/// 便利函数：比较两个 tag 名（倒序，None 排末）
///
/// 用于替换 `tags.sort_by(|a, b| b.name.cmp(&a.name))`
///
/// 返回 `Ordering::Greater` 表示 a 更新。
pub fn cmp_tag_desc(a: &str, b: &str) -> Ordering {
    let va = SemVer::parse(a);
    let vb = SemVer::parse(b);
    match (va, vb) {
        (Some(va), Some(vb)) => va.cmp_desc(&vb),
        // 非 SemVer 排到列表最末
        (Some(_), None) => Ordering::Greater,
        (None, Some(_)) => Ordering::Less,
        // 两个都非 SemVer：退回字符串比较
        (None, None) => b.cmp(a),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_stable() {
        let v = SemVer::parse("v0.27.0").unwrap();
        assert!(v.is_stable);
        assert_eq!((v.major, v.minor, v.patch), (0, 27, 0));
        assert_eq!(v.prerelease, None);
    }

    #[test]
    fn test_parse_prerelease_rc() {
        let v = SemVer::parse("v0.3.10-rc1").unwrap();
        assert!(!v.is_stable);
        assert_eq!((v.major, v.minor, v.patch), (0, 3, 10));
        assert_eq!(v.prerelease, Some("rc1".to_string()));
    }

    #[test]
    fn test_parse_dotted_prerelease() {
        let v = SemVer::parse("v0.2.0-alpha.1").unwrap();
        assert!(!v.is_stable);
        assert_eq!(v.prerelease, Some("alpha.1".to_string()));
    }

    #[test]
    fn test_parse_with_build_metadata() {
        let v = SemVer::parse("v0.3.0+build.123").unwrap();
        assert!(v.is_stable);
        assert_eq!((v.major, v.minor, v.patch), (0, 3, 0));
    }

    #[test]
    fn test_parse_invalid() {
        assert!(SemVer::parse("latest").is_none());
        assert!(SemVer::parse("master").is_none());
        assert!(SemVer::parse("v0.2").is_none());
        assert!(SemVer::parse("v0.2.0.0").is_none());
        assert!(SemVer::parse("0.27.0").is_none()); // 无 v 前缀
    }

    /// 关键 bug 场景：v0.9.x < v0.27.x
    #[test]
    fn test_cmp_desc_v0_9_vs_v0_27() {
        // v0.9.2 应被认为比 v0.27.0 旧
        assert_eq!(cmp_tag_desc("v0.9.2", "v0.27.0"), Ordering::Less);
        assert_eq!(cmp_tag_desc("v0.27.0", "v0.9.2"), Ordering::Greater);
    }

    /// 关键 bug 场景：v0.3.9 < v0.3.10
    #[test]
    fn test_cmp_desc_patch_overflow() {
        assert_eq!(cmp_tag_desc("v0.3.9", "v0.3.10"), Ordering::Less);
        assert_eq!(cmp_tag_desc("v0.3.10", "v0.3.9"), Ordering::Greater);
    }

    /// stable > prerelease
    #[test]
    fn test_cmp_desc_stable_vs_prerelease() {
        assert_eq!(cmp_tag_desc("v0.3.10", "v0.3.10-rc1"), Ordering::Greater);
        assert_eq!(cmp_tag_desc("v0.3.10-rc1", "v0.3.10"), Ordering::Less);
    }

    /// 数字段 vs 字母段（rc1 < rc1a，按 SemVer 规范）
    #[test]
    fn test_cmp_desc_prerelease_segments() {
        // rc1 < rc2
        assert_eq!(cmp_tag_desc("v0.3.10-rc2", "v0.3.10-rc1"), Ordering::Greater);
        // rc10 > rc2（数字按数值，不是字典）
        assert_eq!(cmp_tag_desc("v0.3.10-rc10", "v0.3.10-rc2"), Ordering::Greater);
        // alpha < beta
        assert_eq!(cmp_tag_desc("v0.3.10-beta", "v0.3.10-alpha"), Ordering::Greater);
    }

    /// 非 SemVer 排到列表最末
    #[test]
    fn test_cmp_desc_non_semver() {
        assert_eq!(cmp_tag_desc("v0.3.10", "latest"), Ordering::Greater);
        assert_eq!(cmp_tag_desc("latest", "v0.3.10"), Ordering::Less);
    }

    /// 排序稳定性：相同 tag 返回 Equal
    #[test]
    fn test_cmp_desc_equal() {
        assert_eq!(cmp_tag_desc("v0.27.0", "v0.27.0"), Ordering::Equal);
    }
}
