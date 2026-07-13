//! Updater 的 Tauri commands
//!
//! 详见 `src-tauri/src/updater/mod.rs` 模块文档。
//!
//! ## 命令清单
//! | 命令 | 说明 |
//! |---|---|
//! | `updater_check` | 调 GitHub API，返回 UpdateInfo |
//! | `updater_download` | 下载 + 解压 + 应用，返回 ApplyResult |
//!
//! ## 启动期副作用
//! `apply_pending_update()` 在 `lib.rs::run()` 启动早期同步执行，**不需要** tauri command

use tauri::{AppHandle, Manager};

use crate::error::ProcessError;
use crate::updater::{
    self, build_update_info, download_and_extract, ManifestClient, UpdateInfo,
};
use tokio_util::sync::CancellationToken;

/// GitHub 仓库配置（写死，可后续挪到 config）
const GH_OWNER: &str = "AIJianGe";
const GH_REPO: &str = "BoundLaunch";

/// 当前版本号（编译时注入，与 tauri.conf.json 同步）
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 检查更新
///
/// 调 GitHub API 拉最新 release，比对 semver，返回 UpdateInfo
#[tauri::command]
pub async fn updater_check() -> Result<UpdateInfo, String> {
    tracing::info!(current = CURRENT_VERSION, "updater_check invoked");
    let client = ManifestClient::new(GH_OWNER, GH_REPO);
    let release = client
        .fetch_latest()
        .await
        .map_err(|e| format!("检查更新失败: {}", e))?;
    let info = build_update_info(&release, CURRENT_VERSION)
        .map_err(|e| format!("解析 release 失败: {}", e))?;
    tracing::info!(
        has_update = info.has_update,
        latest = %info.latest_version,
        current = %info.current_version,
        "updater_check completed"
    );
    Ok(info)
}

/// 下载 + 解压 + 应用更新
///
/// 完成后：
/// - staging 目录被清空
/// - pending 目录里有 BoundLaunch.exe.new / BoundLaunch.dll.new / resources/uv/
/// - 用户重启后由 `apply_pending_update()` 接管
///
/// **进度**：通过 `update_progress` 事件推送
#[tauri::command]
pub async fn updater_download(app: AppHandle, info: UpdateInfo) -> Result<updater::ApplyResult, String> {
    if !info.has_update {
        return Err("没有可用更新".to_string());
    }
    tracing::info!(latest = %info.latest_version, "updater_download started");

    let cancel = CancellationToken::new();
    let staging = download_and_extract(&app, &info, cancel)
        .await
        .map_err(|e| format!("下载/解压失败: {}", e))?;

    // 立即进入第二阶段：白名单拷贝到 pending
    let apply_result = updater::apply_update(&staging).map_err(|e| format!("应用更新失败: {}", e))?;
    tracing::info!(?apply_result, "updater_download completed, ready to restart");
    Ok(apply_result)
}

/// 重新启动启动器（应用 pending 更新）
///
/// **v0.0.1 实现**：spawn 当前 exe + exit 当前进程
/// - Windows：start 当前 exe
/// - Unix：直接 exec 当前 exe
#[tauri::command]
pub async fn updater_apply_and_restart(app: AppHandle) -> Result<(), String> {
    use std::process::Command;
    let exe = std::env::current_exe().map_err(|e| format!("获取当前 exe 失败: {}", e))?;
    tracing::warn!(?exe, "updater_apply_and_restart: spawning new launcher, exiting current");

    // 先关掉 Tauri app 释放文件锁
    app.exit(0);

    // spawn 新进程（用 Command::new 启动）
    #[cfg(windows)]
    {
        // Windows：detach 到新进程组，不阻塞当前退出
        let _ = Command::new(&exe).spawn();
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let _ = Command::new(&exe).process_group(0).spawn();
    }
    Ok(())
}
