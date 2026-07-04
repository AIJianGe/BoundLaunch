//! URL 校验与插件名提取
//!
//! 详见 `PR/03-模块设计/04-PluginManager.md §4.3 插件名解析与 URL 安全`
//!
//! ## 安全约束
//! - 仅允许 `https://` 协议（禁止 `file://` / `ssh://` / `git://`）
//! - 禁止 URL 内嵌凭据（`https://token@host`）
//! - 所有 URL 写日志前必须经过 `sanitize_url_for_log` 脱敏

use regex::Regex;
use std::sync::OnceLock;

use super::models::PluginError;

static URL_CRED_RE: OnceLock<Regex> = OnceLock::new();

/// 从 git URL 提取目录名（作为插件名）
///
/// 示例：
/// - `https://github.com/ltdrdata/ComfyUI-Manager.git` → `ComfyUI-Manager`
/// - `https://github.com/ltdrdata/ComfyUI-Manager` → `ComfyUI-Manager`
/// - `https://github.com/ltdrdata/ComfyUI-Manager/` → `ComfyUI-Manager`
pub fn derive_plugin_name(url: &str) -> String {
    let url = url.trim_end_matches(".git").trim_end_matches('/');
    let name = url.rsplit('/').next().unwrap_or("unknown");
    name.to_string()
}

/// 校验 git URL 合法性
///
/// 安全规则：
/// 1. 仅允许 `https://` 协议（禁止 `file://` / `ssh://` / `git://`）
/// 2. 禁止 URL 内嵌凭据（`https://token@host/...`）
pub fn validate_git_url(url: &str) -> Result<(), PluginError> {
    // 1. 协议白名单
    if !url.starts_with("https://") {
        let scheme = url.split("://").next().unwrap_or("unknown");
        return Err(PluginError::InvalidUrl(format!(
            "仅支持 https:// 协议，收到: {}（禁止 file:// ssh:// git://）",
            scheme
        )));
    }

    // 2. 禁止内嵌凭据
    //    合法：https://github.com/user/repo
    //    合法：https://www.github.com/user/repo
    //    非法：https://token@github.com/user/repo
    //    非法：https://user:pass@github.com/user/repo
    if url.contains('@') && !url.starts_with("https://www.") {
        return Err(PluginError::InvalidUrl(
            "URL 含内嵌凭据，请使用不含 token 的公开仓库地址".into(),
        ));
    }

    Ok(())
}

/// 日志脱敏：写日志前移除 URL 中的凭据
///
/// `https://token@host/...` → `https://***@host/...`
///
/// 即使 validate_git_url 已禁止凭据 URL，本函数仍用于：
/// - 防御性编程（用户可能绕过校验直接调用底层接口）
/// - 第三方仓库地址可能含 token（虽然不允许，但仍要脱敏）
pub fn sanitize_url_for_log(url: &str) -> String {
    let re = URL_CRED_RE.get_or_init(|| Regex::new(r"(https://)([^/@]+)@").unwrap());
    re.replace_all(url, "${1}***@").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_plugin_name_with_git_suffix() {
        assert_eq!(
            derive_plugin_name("https://github.com/ltdrdata/ComfyUI-Manager.git"),
            "ComfyUI-Manager"
        );
    }

    #[test]
    fn test_derive_plugin_name_without_git_suffix() {
        assert_eq!(
            derive_plugin_name("https://github.com/ltdrdata/ComfyUI-Manager"),
            "ComfyUI-Manager"
        );
    }

    #[test]
    fn test_derive_plugin_name_trailing_slash() {
        assert_eq!(
            derive_plugin_name("https://github.com/ltdrdata/ComfyUI-Manager/"),
            "ComfyUI-Manager"
        );
    }

    #[test]
    fn test_validate_git_url_https_ok() {
        assert!(validate_git_url("https://github.com/user/repo").is_ok());
        assert!(validate_git_url("https://github.com/user/repo.git").is_ok());
        assert!(validate_git_url("https://www.github.com/user/repo").is_ok());
    }

    #[test]
    fn test_validate_git_url_disallows_file_protocol() {
        let result = validate_git_url("file:///etc/passwd");
        assert!(matches!(result, Err(PluginError::InvalidUrl(_))));
    }

    #[test]
    fn test_validate_git_url_disallows_ssh_protocol() {
        let result = validate_git_url("ssh://git@github.com/user/repo");
        assert!(matches!(result, Err(PluginError::InvalidUrl(_))));
    }

    #[test]
    fn test_validate_git_url_disallows_git_protocol() {
        let result = validate_git_url("git://github.com/user/repo");
        assert!(matches!(result, Err(PluginError::InvalidUrl(_))));
    }

    #[test]
    fn test_validate_git_url_disallows_embedded_credentials() {
        let result = validate_git_url("https://token@github.com/user/repo");
        assert!(matches!(result, Err(PluginError::InvalidUrl(_))));
    }

    #[test]
    fn test_validate_git_url_allows_www_prefix() {
        // www.github.com 不算凭据
        assert!(validate_git_url("https://www.github.com/user/repo").is_ok());
    }

    #[test]
    fn test_sanitize_url_for_log_masks_credentials() {
        let sanitized = sanitize_url_for_log("https://token@github.com/user/repo");
        assert_eq!(sanitized, "https://***@github.com/user/repo");
    }

    #[test]
    fn test_sanitize_url_for_log_no_credentials() {
        let sanitized = sanitize_url_for_log("https://github.com/user/repo");
        assert_eq!(sanitized, "https://github.com/user/repo");
    }
}
