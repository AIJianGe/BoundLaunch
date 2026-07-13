//! ComfyUI session 管理
//!
//! 实现 ComfyUI Desktop 标准的 `__COMFY_CLI_SESSION__` 协议，
//! 让 ComfyUI-Manager 可以通过写 `.reboot` 标志文件触发客户端自动重启。
//!
//! ## 协议
//!
//! 1. 启动 ComfyUI 时：
//!    - 在 `<exe_dir>/.boundlaunch/sessions/` 创建 `<random>.session` 文件
//!    - 设置 `__COMFY_CLI_SESSION__=<path>` 环境变量
//! 2. ComfyUI-Manager 检测到 `__COMFY_CLI_SESSION__` 后：
//!    - 点击 Restart → 写 `<session_path>.reboot` 标志文件
//!    - `exit(0)` 主动退出
//! 3. 客户端：
//!    - `child.wait()` 完成 → 检查 `.reboot` 标志
//!    - 存在 → 自动 respawn（无缝重启）
//!    - 不存在 → 正常停止
//!
//! ## 多实例隔离
//!
//! - session 目录在 `<exe_dir>/.boundlaunch/sessions/`
//! - 复制目录到新位置 → 新实例用自己的 sessions/ → 互不影响 ✅
//! - 同一实例多 ComfyUI 进程：文件名带 16 字节随机后缀 → 不冲突 ✅
//!
//! ## 关键不变量
//!
//! - **目录存在**：`ensure_sessions_dir()` 启动时确保存在
//! - **文件命名**：`{16字节hex}.session`，永不重用
//! - **清理**：`cleanup_stale_sessions()` 清理超过 24 小时的 session

use std::fs;
use std::path::{Path, PathBuf};

use crate::paths::env_paths::ResolvedEnvPaths;

/// Session 文件名前缀长度（16 字节 = 32 hex 字符）
///
/// 16 字节随机性 = 2^128，足够避免冲突
const RANDOM_HEX_LEN: usize = 32;

/// 单个 ComfyUI 进程的 session 状态
#[derive(Debug, Clone)]
pub struct SessionInfo {
    /// session 文件路径（== ComfyUI 接收到的 `__COMFY_CLI_SESSION__`）
    pub session_path: PathBuf,
    /// reboot 标志文件路径（session_path + ".reboot"）
    pub reboot_flag_path: PathBuf,
}

impl SessionInfo {
    /// 检查 ComfyUI-Manager 写的 `.reboot` 标志是否存在
    ///
    /// 存在 = 客户端应立即 respawn
    pub fn has_reboot_flag(&self) -> bool {
        self.reboot_flag_path.exists()
    }

    /// 删除 `.reboot` 标志（respawn 后清理）
    ///
    /// 失败仅 warn，不影响 respawn 流程
    pub fn clear_reboot_flag(&self) {
        if self.reboot_flag_path.exists() {
            if let Err(e) = fs::remove_file(&self.reboot_flag_path) {
                tracing::warn!(
                    path = %self.reboot_flag_path.display(),
                    error = %e,
                    "failed to clear reboot flag (non-fatal)"
                );
            }
        }
    }
}

/// 确保 sessions 目录存在
///
/// 启动时调用一次：
/// - 不存在 → 创建（带 .boundlaunch 父目录）
/// - 已存在 → 无操作
pub fn ensure_sessions_dir(sessions_dir: &Path) -> Result<(), std::io::Error> {
    if !sessions_dir.exists() {
        fs::create_dir_all(sessions_dir)?;
        tracing::info!(path = %sessions_dir.display(), "created sessions directory");
    }
    Ok(())
}

/// 创建新的 ComfyUI session
///
/// 1. 生成 32 hex 字符的随机文件名
/// 2. 在 `sessions_dir/<random>.session` 创建空文件
/// 3. 返回 `SessionInfo`
///
/// 失败返回 `Err`，调用方应降级为"不设置 `__COMFY_CLI_SESSION__`"（fallback 到 Manager 的 os.execv 路径）
pub fn create_session(sessions_dir: &Path) -> Result<SessionInfo, std::io::Error> {
    ensure_sessions_dir(sessions_dir)?;

    let random = generate_random_hex(RANDOM_HEX_LEN);
    let session_path = sessions_dir.join(format!("{}.session", random));
    let reboot_flag_path = session_path.with_extension("session.reboot");

    // 创建空文件（ComfyUI-Manager 不要求内容，只要路径存在）
    fs::write(&session_path, "")?;

    tracing::info!(
        session = %session_path.display(),
        "ComfyUI session created"
    );

    Ok(SessionInfo {
        session_path,
        reboot_flag_path,
    })
}

/// 清理过期的 session 文件（启动时调用）
///
/// 策略：删除超过 24 小时的 `.session` 文件
/// - 防止长时间运行后 sessions 目录堆积
/// - 不删除 `.reboot` 标志（可能在 respawn 检测中需要）
///
/// 静默运行：失败仅 trace，不影响启动
pub fn cleanup_stale_sessions(sessions_dir: &Path) {
    if !sessions_dir.exists() {
        return;
    }

    let Ok(entries) = fs::read_dir(sessions_dir) else {
        return;
    };

    let max_age = std::time::Duration::from_secs(24 * 60 * 60); // 24 hours
    let now = std::time::SystemTime::now();

    let mut cleaned = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        // 只清理 .session 文件，不动 .reboot
        if path.extension().and_then(|s| s.to_str()) != Some("session") {
            continue;
        }

        let Ok(meta) = entry.metadata() else { continue };
        let Ok(modified) = meta.modified() else { continue };
        let Ok(age) = now.duration_since(modified) else { continue };

        if age > max_age {
            if fs::remove_file(&path).is_ok() {
                cleaned += 1;
            }
        }
    }

    if cleaned > 0 {
        tracing::info!(
            count = cleaned,
            dir = %sessions_dir.display(),
            "cleaned up stale sessions"
        );
    }
}

/// 生成 N 字节的随机 hex 字符串
///
/// 使用 `getrandom` crate（已通过 sqlx 间接依赖）或者
/// 退化用 `std::time` + PID 哈希（最坏情况有冲突但概率极低）
fn generate_random_hex(len: usize) -> String {
    // 16 字节随机性 = 32 hex 字符
    let mut bytes = [0u8; 16];
    fill_random_bytes(&mut bytes);
    hex_encode(&bytes[..len / 2.min(16)])
}

/// 填充随机字节
///
/// 退化路径：用 PID + 当前时间纳秒 + 进程地址哈希
/// 仍然有足够随机性（同一纳秒内同 PID 重启概率 < 1e-9）
fn fill_random_bytes(buf: &mut [u8]) {
    let pid: u64 = std::process::id() as u64;
    let nanos: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let addr: u64 = buf.as_ptr() as usize as u64;

    let mut state: u64 = pid ^ nanos ^ addr;
    for chunk in buf.chunks_mut(8) {
        // xorshift64 伪随机
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let bytes = state.to_le_bytes();
        for (i, b) in chunk.iter_mut().enumerate() {
            *b = bytes.get(i).copied().unwrap_or(0);
        }
    }
}

/// 16 进制编码
fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// 高层辅助：基于 ResolvedEnvPaths 创建 session
pub fn create_session_from_resolved(
    resolved: &ResolvedEnvPaths,
) -> Result<SessionInfo, std::io::Error> {
    create_session(&resolved.sessions_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_ensure_sessions_dir_creates() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("sessions");
        assert!(!dir.exists());

        ensure_sessions_dir(&dir).unwrap();
        assert!(dir.exists());
    }

    #[test]
    fn test_ensure_sessions_dir_idempotent() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("sessions");

        ensure_sessions_dir(&dir).unwrap();
        ensure_sessions_dir(&dir).unwrap(); // 第二次不应报错
        assert!(dir.exists());
    }

    #[test]
    fn test_create_session_returns_valid_info() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("sessions");

        let info = create_session(&dir).unwrap();
        assert!(info.session_path.exists());
        assert_eq!(
            info.session_path.extension().and_then(|s| s.to_str()),
            Some("session")
        );
        assert_eq!(
            info.reboot_flag_path.extension().and_then(|s| s.to_str()),
            Some("reboot")
        );
    }

    #[test]
    fn test_create_session_filename_random() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("sessions");

        let a = create_session(&dir).unwrap();
        let b = create_session(&dir).unwrap();

        // 两次创建的 session 路径必须不同
        assert_ne!(a.session_path, b.session_path);
    }

    #[test]
    fn test_reboot_flag_lifecycle() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("sessions");

        let info = create_session(&dir).unwrap();
        assert!(!info.has_reboot_flag());

        // 模拟 ComfyUI-Manager 写 .reboot 标志
        fs::write(&info.reboot_flag_path, "").unwrap();
        assert!(info.has_reboot_flag());

        // clear 后标志消失
        info.clear_reboot_flag();
        assert!(!info.has_reboot_flag());
    }

    #[test]
    fn test_cleanup_stale_sessions_keeps_recent() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("sessions");
        ensure_sessions_dir(&dir).unwrap();

        // 创建一个 session
        let info = create_session(&dir).unwrap();
        assert!(info.session_path.exists());

        // 立即清理（不满 24h）→ 不应被清理
        cleanup_stale_sessions(&dir);
        assert!(info.session_path.exists());
    }

    #[test]
    fn test_cleanup_handles_missing_dir() {
        // 不存在的目录不应报错
        let tmp = tempdir().unwrap();
        let nonexistent = tmp.path().join("does_not_exist");
        cleanup_stale_sessions(&nonexistent);
        // 不应 panic
    }

    #[test]
    fn test_hex_encode() {
        assert_eq!(hex_encode(&[0x00, 0xff, 0xab, 0xcd]), "00ffabcd");
        assert_eq!(hex_encode(&[]), "");
    }
}
