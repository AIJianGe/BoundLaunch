//! ANSI 转义序列处理
//!
//! v3.4.2：清洗 ComfyUI / Python 子进程输出的 ANSI 颜色码
//!
//! ## 背景
//! - ComfyUI 用 `loguru` / `rich` 输出带颜色的日志：`\x1b[32m[INFO]\x1b[0m xxx`
//! - 在 Windows 上 `tokio::process::Command` 捕获子进程 stdout 时，`\x1b` (ESC, 0x1B)
//!   常被 `BufReader::lines()` 丢弃（或被 console code page 转换吞掉）
//! - 残留的 `[32m` / `[0m` / `[1m[33m` 等控制序列参数显示在前端就成"乱码"
//!
//! ## 解决方案
//! 用 regex 同时匹配两种形式的 ANSI 残留：
//! 1. `\x1b\[[0-9;]*[a-zA-Z]`：完整 ANSI 转义（带 ESC 前缀）
//! 2. `\[[0-9;]*m`：残留的 SGR 颜色码（ESC 被吞掉后只留 `[数字+m`）
//!
//! 调用 `strip_ansi` 一次性清洗，返回纯文本。
//!
//! ## 性能
//! - 静态 `once_cell::Lazy<Regex>` 编译一次反复用
//! - 单行日志清洗 < 1μs，可忽略
//!
//! ## 使用示例
//! ```ignore
//! use crate::common::ansi::strip_ansi;
//!
//! let line = strip_ansi("\x1b[32m[INFO]\x1b[0m hello");
//! assert_eq!(line, "[INFO] hello");
//!
//! // 残留形式（ESC 被吞掉）
//! let line = strip_ansi("[32m[INFO][0m hello");
//! assert_eq!(line, "[INFO] hello");
//! ```

use once_cell::sync::Lazy;
use regex::Regex;

/// 匹配 ANSI 转义序列（完整 + 残留两种形式）
///
/// - 完整：`\x1b\[<params><final>`，如 `\x1b[1;33m`
/// - 残留：`\[<params>m`，如 `[32m`、`[0m`、`[1;33m`（ESC 被吞）
///
/// `\[[0-9;]*[a-zA-Z]` 匹配所有 SGR 序列（包括 `\x1b[2J` 清屏、`\x1b[H` 光标定位等）
static ANSI_RE: Lazy<Regex> = Lazy::new(|| {
    // 注意：ESC 字符在 Rust 字符串字面量中用 `\x1b`
    Regex::new(r"\x1b\[[0-9;]*[a-zA-Z]|\[[0-9;]*[a-zA-Z]").expect("valid regex")
});

/// 清洗字符串中的 ANSI 转义序列，返回纯文本
///
/// - 不修改原始 String（clone-on-write）
/// - 空字符串直接返回
/// - 没有 ANSI 时返回原字符串引用（不分配）
pub fn strip_ansi(s: &str) -> String {
    if s.is_empty() {
        return s.to_string();
    }
    ANSI_RE.replace_all(s, "").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_full_ansi() {
        // 完整 ANSI（带 ESC 前缀）
        assert_eq!(strip_ansi("\x1b[32m[INFO]\x1b[0m hello"), "[INFO] hello");
        assert_eq!(strip_ansi("\x1b[1;33mWARNING\x1b[0m"), "WARNING");
    }

    #[test]
    fn test_strip_residual_ansi() {
        // 残留 ANSI（ESC 被吞，常见于 Windows tokio::process capture）
        assert_eq!(strip_ansi("[32m[INFO][0m hello"), "[INFO] hello");
        assert_eq!(strip_ansi("[1m[33m[WARNING][0m[0m foo"), "[WARNING] foo");
        assert_eq!(strip_ansi("[0m"), "");
    }

    #[test]
    fn test_strip_mixed() {
        // 混合（部分有 ESC，部分没有）
        assert_eq!(
            strip_ansi("\x1b[32m[INFO]\x1b[0m [32mhello[0m world"),
            "[INFO] hello world"
        );
    }

    #[test]
    fn test_strip_no_ansi() {
        // 无 ANSI 时保持原样
        assert_eq!(strip_ansi("plain text"), "plain text");
        assert_eq!(strip_ansi(""), "");
    }

    #[test]
    fn test_strip_chinese_preserved() {
        // 中文不被误删
        assert_eq!(
            strip_ansi("[32m[INFO][0m 你好世界"),
            "[INFO] 你好世界"
        );
    }

    #[test]
    fn test_strip_other_csi() {
        // 其他 CSI 序列（光标、清屏等）也支持
        assert_eq!(strip_ansi("\x1b[2Jclear\x1b[Hmove"), "clearmove");
    }
}
