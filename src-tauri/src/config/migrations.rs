//! 配置版本迁移
//!
//! 详见 `PR/03-模块设计/01-Config.md §10 配置版本迁移`
//!
//! 当 schema_version 升级时，按版本号顺序执行迁移函数链
//! 每个迁移函数 vN_to_vN+1 负责一个版本升级

use super::models::Config;
use crate::error::ConfigError;

/// 主迁移入口
///
/// 从 from_version 迁移到 to_version
/// 按版本号顺序依次执行每个迁移函数
///
/// 注：当前无迁移函数（schema_version=1 是初始版本），`_ => break` 是唯一路径，
/// `v += 1` 暂不可达。未来添加 `1 => migrate_v1_to_v2(config)?` 等分支后即可达。
/// 此处 `#[allow(unreachable_code)]` 是预期的设计时屏蔽。
#[allow(unreachable_code)]
pub fn migrate(config: &mut Config, from_version: u32, to_version: u32) -> Result<(), ConfigError> {
    // 注：`let mut v` 之所以不加 mut，是因为当前 `match v` 唯一 arm 是 `_ => break`，
    // `v += 1` 不可达 → 编译器报 unused_mut。未来添加 `1 => migrate_v1_to_v2(config)?`
    // 等分支后，`v += 1` 可达，需把 `let v` 改回 `let mut v`。
    let v = from_version;
    while v < to_version {
        match v {
            // 1 → 2, 未来示例 - 添加新字段
            // 1 => migrate_v1_to_v2(config)?,
            _ => break,
        }
        v += 1;
    }
    // 仅当所有迁移函数执行完毕才更新 schema_version
    // 遇到未知版本提前 break 时保留原版本号，避免错误地标记为已迁移
    if v == to_version {
        config.schema_version = to_version;
    }
    Ok(())
}

// === 迁移函数占位（未来按需实现） ===

/// v1 → v2 迁移示例（未启用）
#[allow(dead_code)]
fn migrate_v1_to_v2(_config: &mut Config) -> Result<(), ConfigError> {
    // 示例：如果新增了字段，旧版本 Config 可能缺失，这里补默认值
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_migrate_same_version_noop() {
        let mut cfg = Config::default();
        let original_version = cfg.schema_version;
        migrate(&mut cfg, original_version, original_version).unwrap();
        assert_eq!(cfg.schema_version, original_version);
    }

    #[test]
    fn test_migrate_unknown_version_breaks_gracefully() {
        let mut cfg = Config::default();
        // 当前 schema_version=1，尝试迁移到 99（无对应迁移函数）
        migrate(&mut cfg, 1, 99).unwrap();
        // 应该静默 break，不报错
        assert_eq!(cfg.schema_version, 1);
    }
}
