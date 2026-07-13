//! GitHub Release manifest 解析
//!
//! ## 协议
//!
//! 客户端通过 `GET https://api.github.com/repos/{owner}/{repo}/releases/latest` 拉取最新 release。
//! 不需要 GitHub token（匿名 60 次/小时，够用）。
//!
//! ## Release Asset 约定
//!
//! 每个 release 必须有恰好一个 portable zip，命名规范：
//! `BoundLaunch-portable-vX.Y.Z.zip`
//!
//! 可选的 SHA256 校验文件：
//! `BoundLaunch-portable-vX.Y.Z.zip.sha256`（文本内容为 hex 字符串）
//!
//! ## 版本号来源
//!
//! release 的 `tag_name` 字段（如 "v0.0.2"），去掉 `v` 前缀后用 `semver` crate 解析。

use serde::{Deserialize, Serialize};

/// GitHub Release JSON（仅取需要的字段）
///
/// API 完整字段参考：
/// <https://docs.github.com/en/rest/releases/releases#get-the-latest-release>
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GithubRelease {
    pub tag_name: String,
    pub name: String,
    pub body: String,
    pub html_url: String,
    pub published_at: String,
    pub assets: Vec<GithubAsset>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GithubAsset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
    pub content_type: String,
}

/// 内部统一格式：解析后的可更新信息
///
/// **序列化要求**：
/// - `Serialize`：由后端返回前端
/// - `Deserialize`：由前端 invoke `updater_download` 时回传给后端
///   （前端拿到的 UpdateInfo 直接交给 download command）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateInfo {
    /// 是否有可用更新
    pub has_update: bool,
    /// 当前版本（X.Y.Z）
    pub current_version: String,
    /// 最新版本（X.Y.Z）
    pub latest_version: String,
    /// release 名称
    pub release_name: String,
    /// release notes
    pub release_notes: String,
    /// release 页面 URL
    pub release_url: String,
    /// 下载 URL（zip）
    pub download_url: String,
    /// zip 大小（字节）
    pub zip_size: u64,
    /// SHA256 校验值（hex 字符串，小写），无校验文件时为 None
    pub sha256: Option<String>,
    /// 发布时间
    pub published_at: String,
}

/// GitHub API 客户端配置
pub struct ManifestClient {
    /// 仓库 owner（如 "AIJianGe"）
    pub owner: String,
    /// 仓库名（如 "BoundLaunch"）
    pub repo: String,
    /// HTTP 客户端
    pub http: reqwest::Client,
}

impl ManifestClient {
    pub fn new(owner: impl Into<String>, repo: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent("BoundLaunch-Updater/0.0.1")
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("failed to build http client");
        Self {
            owner: owner.into(),
            repo: repo.into(),
            http,
        }
    }

    /// 拉取最新 release JSON
    pub async fn fetch_latest(&self) -> Result<GithubRelease, ManifestError> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/releases/latest",
            self.owner, self.repo
        );
        tracing::info!(url = %url, "fetching latest release from GitHub");

        let resp = self
            .http
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| ManifestError::Network(e.to_string()))?;

        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(ManifestError::NoRelease);
        }
        if !status.is_success() {
            return Err(ManifestError::Network(format!(
                "GitHub API returned status {}",
                status
            )));
        }

        let release: GithubRelease = resp
            .json()
            .await
            .map_err(|e| ManifestError::Parse(e.to_string()))?;

        Ok(release)
    }
}

/// 解析 tag 为 semver 字符串（去掉 "v" 前缀）
///
/// "v0.0.2" → "0.0.2"
/// "0.0.2"  → "0.0.2"
pub fn strip_v_prefix(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}

/// 在 release 的 assets 里找 portable zip
///
/// 匹配规则：`name` 以 `BoundLaunch-portable-v` 开头且以 `.zip` 结尾
pub fn find_portable_zip<'a>(release: &'a GithubRelease) -> Option<&'a GithubAsset> {
    release.assets.iter().find(|a| {
        a.name.starts_with("BoundLaunch-portable-v") && a.name.ends_with(".zip")
    })
}

/// 在 release 的 assets 里找 SHA256 校验文件
///
/// 匹配规则：`<zip_name>.sha256`，内容为 hex 字符串
pub fn find_sha256_asset<'a>(release: &'a GithubRelease, zip_name: &str) -> Option<&'a GithubAsset> {
    let target = format!("{}.sha256", zip_name);
    release.assets.iter().find(|a| a.name == target)
}

/// 比较两个 semver 版本
pub fn is_newer(latest: &str, current: &str) -> bool {
    let Ok(latest_v) = semver::Version::parse(latest) else {
        return false;
    };
    let Ok(current_v) = semver::Version::parse(current) else {
        // 当前版本解析失败 → 假定有更新（让用户主动选择）
        return true;
    };
    latest_v > current_v
}

/// 构造完整 UpdateInfo
///
/// - 当前版本由调用方提供
/// - 若 latest <= current → has_update = false
/// - 若找不到 portable zip → 报错
pub fn build_update_info(
    release: &GithubRelease,
    current_version: &str,
) -> Result<UpdateInfo, ManifestError> {
    let latest_version = strip_v_prefix(&release.tag_name).to_string();

    let zip_asset = find_portable_zip(release).ok_or_else(|| {
        ManifestError::Parse(format!(
            "no portable zip found in release {} (assets: {:?})",
            release.tag_name,
            release.assets.iter().map(|a| &a.name).collect::<Vec<_>>()
        ))
    })?;

    let sha256 = find_sha256_asset(release, &zip_asset.name).map(|a| a.browser_download_url.clone());

    let has_update = is_newer(&latest_version, current_version);

    Ok(UpdateInfo {
        has_update,
        current_version: current_version.to_string(),
        latest_version,
        release_name: release.name.clone(),
        release_notes: release.body.clone(),
        release_url: release.html_url.clone(),
        download_url: zip_asset.browser_download_url.clone(),
        zip_size: zip_asset.size,
        sha256,
        published_at: release.published_at.clone(),
    })
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("network error: {0}")]
    Network(String),

    #[error("no release published yet")]
    NoRelease,

    #[error("parse release failed: {0}")]
    Parse(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_v_prefix() {
        assert_eq!(strip_v_prefix("v0.0.2"), "0.0.2");
        assert_eq!(strip_v_prefix("0.0.2"), "0.0.2");
        assert_eq!(strip_v_prefix("v1.0.0-beta.1"), "1.0.0-beta.1");
    }

    #[test]
    fn test_is_newer() {
        assert!(is_newer("0.0.2", "0.0.1"));
        assert!(is_newer("0.1.0", "0.0.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.0.1", "0.0.1"));
        assert!(!is_newer("0.0.1", "0.0.2"));
        // 当前版本解析失败 → 假定有更新
        assert!(is_newer("0.0.1", "invalid"));
    }

    #[test]
    fn test_find_portable_zip() {
        let release = GithubRelease {
            tag_name: "v0.0.2".into(),
            name: "v0.0.2".into(),
            body: "".into(),
            html_url: "".into(),
            published_at: "".into(),
            assets: vec![
                GithubAsset {
                    name: "BoundLaunch-portable-v0.0.2.zip".into(),
                    browser_download_url: "https://example.com/zip".into(),
                    size: 1000,
                    content_type: "application/zip".into(),
                },
                GithubAsset {
                    name: "BoundLaunch-portable-v0.0.2.zip.sha256".into(),
                    browser_download_url: "https://example.com/sha".into(),
                    size: 64,
                    content_type: "text/plain".into(),
                },
            ],
        };
        let zip = find_portable_zip(&release).unwrap();
        assert_eq!(zip.name, "BoundLaunch-portable-v0.0.2.zip");
        assert_eq!(zip.size, 1000);
    }

    #[test]
    fn test_find_sha256_asset() {
        let release = GithubRelease {
            tag_name: "v0.0.2".into(),
            name: "v0.0.2".into(),
            body: "".into(),
            html_url: "".into(),
            published_at: "".into(),
            assets: vec![
                GithubAsset {
                    name: "BoundLaunch-portable-v0.0.2.zip".into(),
                    browser_download_url: "https://example.com/zip".into(),
                    size: 1000,
                    content_type: "application/zip".into(),
                },
                GithubAsset {
                    name: "BoundLaunch-portable-v0.0.2.zip.sha256".into(),
                    browser_download_url: "https://example.com/sha".into(),
                    size: 64,
                    content_type: "text/plain".into(),
                },
            ],
        };
        let sha = find_sha256_asset(&release, "BoundLaunch-portable-v0.0.2.zip").unwrap();
        assert_eq!(sha.name, "BoundLaunch-portable-v0.0.2.zip.sha256");
    }

    #[test]
    fn test_build_update_info() {
        let release = GithubRelease {
            tag_name: "v0.0.2".into(),
            name: "v0.0.2 - 测试".into(),
            body: "## 更新内容".into(),
            html_url: "https://github.com/AIJianGe/BoundLaunch/releases/tag/v0.0.2".into(),
            published_at: "2026-08-01T00:00:00Z".into(),
            assets: vec![GithubAsset {
                name: "BoundLaunch-portable-v0.0.2.zip".into(),
                browser_download_url: "https://github.com/.../BoundLaunch-portable-v0.0.2.zip".into(),
                size: 12345678,
                content_type: "application/zip".into(),
            }],
        };
        let info = build_update_info(&release, "0.0.1").unwrap();
        assert!(info.has_update);
        assert_eq!(info.current_version, "0.0.1");
        assert_eq!(info.latest_version, "0.0.2");
        assert_eq!(info.zip_size, 12345678);

        // 同样版本 → has_update = false
        let info2 = build_update_info(&release, "0.0.2").unwrap();
        assert!(!info2.has_update);

        // 更新到 0.0.3 → has_update = false
        let info3 = build_update_info(&release, "0.0.3").unwrap();
        assert!(!info3.has_update);
    }

    #[test]
    fn test_build_update_info_no_zip() {
        let release = GithubRelease {
            tag_name: "v0.0.2".into(),
            name: "v0.0.2".into(),
            body: "".into(),
            html_url: "".into(),
            published_at: "".into(),
            assets: vec![],
        };
        let err = build_update_info(&release, "0.0.1").unwrap_err();
        assert!(matches!(err, ManifestError::Parse(_)));
    }
}
