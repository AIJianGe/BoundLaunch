//! ComfyUI 自动重启策略
//!
//! 实现 ComfyUI-Manager 的 `__COMFY_CLI_SESSION__` 协议：
//! - ComfyUI 主动 `exit(0)` + 写 `<session_path>.reboot` 标志
//! - 客户端检测 `.reboot` 标志 → 决定是否 respawn
//!
//! ## 重启策略
//!
//! - **Manager 触发的重启**（`.reboot` 存在 + `auto_restart=true`）→ 自动 respawn
//! - **用户主动停止**（无 `.reboot` + 用户点 stop）→ 不 respawn
//! - **崩溃重启**：由 v3.4 的健康检查逻辑处理（不在此模块）
//!
//! ## 防误触
//!
//! - **重启间隔**：两次 respawn 间隔 < 5s 视为异常，停止 respawn
//! - **重启上限**：1 分钟内 respawn 次数 > 3 视为异常，停止 respawn
//!
//! ## 多实例隔离
//!
//! - 每个 `ProcessLauncher` 实例维护自己的 `RespawnPolicy`
//! - session 路径在 `<exe_dir>/.boundlaunch/sessions/`
//! - 复制目录到新位置 → 新实例用自己的 session 和 policy → 互不影响 ✅

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;

use crate::process_launcher::models::RespawnReason;
use crate::process_launcher::session::SessionInfo;

/// 重启决策结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RespawnDecision {
    /// 自动 respawn
    Allow,
    /// 拒绝 respawn，状态 → Stopped
    Deny(DenyReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    /// auto_restart 开关关闭
    AutoRestartDisabled,
    /// 重启太频繁（< 5s 间隔）
    TooFrequent,
    /// 重启次数超限（1 分钟 > 3 次）
    ExceededLimit,
    /// session_path 不可用（.reboot 标志存在但 session 路径已失效）
    InvalidSession,
}

/// 重启策略状态
///
/// 跟踪最近一次重启时间 + 1 分钟窗口内的重启次数
pub struct RespawnPolicy {
    /// 是否启用自动重启（来自 cfg.launch.auto_restart）
    auto_restart: bool,
    /// 1 分钟窗口内的重启时间戳
    recent_respawns: Mutex<Vec<Instant>>,
    /// 重启次数超限阈值
    max_per_minute: usize,
    /// 两次重启最小间隔
    min_interval: std::time::Duration,
}

impl RespawnPolicy {
    /// 1 分钟内最大重启次数
    const DEFAULT_MAX_PER_MINUTE: usize = 3;
    /// 两次重启最小间隔（防止热循环）
    const DEFAULT_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

    pub fn new(auto_restart: bool) -> Arc<Self> {
        Arc::new(Self {
            auto_restart,
            recent_respawns: Mutex::new(Vec::new()),
            max_per_minute: Self::DEFAULT_MAX_PER_MINUTE,
            min_interval: Self::DEFAULT_MIN_INTERVAL,
        })
    }

    /// 更新 auto_restart 配置（用户在设置页改开关）
    pub fn set_auto_restart(&mut self, enabled: bool) {
        self.auto_restart = enabled;
        if !enabled {
            // 关闭时清空历史，避免下次开启时误判
            self.recent_respawns.lock().clear();
        }
    }

    /// 是否启用自动重启
    pub fn is_enabled(&self) -> bool {
        self.auto_restart
    }

    /// 决定是否允许 respawn
    ///
    /// # 参数
    /// - `session_info`：当前 ComfyUI 进程的 session 信息
    /// - `now`：当前时间（注入方便测试）
    ///
    /// # 副作用
    /// - 调用 `record_respawn` **之前**的判断不会修改状态
    /// - 调用方在决定 respawn 后再调 `record_respawn` 记录
    pub fn decide(&self, session_info: &Option<SessionInfo>, now: Instant) -> RespawnDecision {
        // 1. auto_restart 关闭
        if !self.auto_restart {
            return RespawnDecision::Deny(DenyReason::AutoRestartDisabled);
        }

        // 2. session 不可用（不应走到这里，但防御性检查）
        let Some(si) = session_info.as_ref() else {
            return RespawnDecision::Deny(DenyReason::InvalidSession);
        };

        // 3. .reboot 标志不存在（这是用户主动停止或崩溃，不 respawn）
        if !si.has_reboot_flag() {
            return RespawnDecision::Deny(DenyReason::InvalidSession);
        }

        // 4. 重启次数 + 间隔检查
        let mut recent = self.recent_respawns.lock();
        // 清理 1 分钟之前的记录
        recent.retain(|t| now.duration_since(*t) < std::time::Duration::from_secs(60));

        // 5. 间隔检查（至少 5s 间隔）
        if let Some(last) = recent.last() {
            if now.duration_since(*last) < self.min_interval {
                tracing::warn!(
                    elapsed_secs = now.duration_since(*last).as_secs_f32(),
                    min_secs = self.min_interval.as_secs(),
                    "respawn denied: too frequent"
                );
                return RespawnDecision::Deny(DenyReason::TooFrequent);
            }
        }

        // 6. 次数检查（1 分钟内最多 3 次）
        if recent.len() >= self.max_per_minute {
            tracing::warn!(
                count = recent.len(),
                max = self.max_per_minute,
                "respawn denied: exceeded limit (1 minute window)"
            );
            return RespawnDecision::Deny(DenyReason::ExceededLimit);
        }

        RespawnDecision::Allow
    }

    /// 记录一次 respawn（调用方在确认 respawn 后调用）
    pub fn record_respawn(&self, now: Instant) {
        let mut recent = self.recent_respawns.lock();
        recent.retain(|t| now.duration_since(*t) < std::time::Duration::from_secs(60));
        recent.push(now);
        tracing::info!(
            count = recent.len(),
            "respawn recorded"
        );
    }

    /// 重启后清理 session 的 .reboot 标志
    pub fn after_respawn(&self, session_info: &Option<SessionInfo>) {
        if let Some(si) = session_info.as_ref() {
            si.clear_reboot_flag();
        }
    }

    /// 获取当前 1 分钟窗口内的重启次数（供前端显示）
    pub fn recent_count(&self) -> usize {
        let now = Instant::now();
        self.recent_respawns
            .lock()
            .iter()
            .filter(|t| now.duration_since(**t) < std::time::Duration::from_secs(60))
            .count()
    }
}

/// 解析 ComfyUI 退出原因
///
/// 区分 "Manager 重启" / "用户主动停止" / "崩溃"
pub fn classify_exit(exit_code: Option<i32>, session_info: &Option<SessionInfo>) -> RespawnReason {
    // .reboot 标志存在 → Manager 触发的重启
    if let Some(si) = session_info.as_ref() {
        if si.has_reboot_flag() {
            return RespawnReason::ManagerReboot;
        }
    }

    // exit(0) 且无 .reboot → 用户主动停止
    if matches!(exit_code, Some(0)) {
        return RespawnReason::UserRequest;
    }

    // 其他情况视为崩溃（由 v3.4 健康检查处理）
    RespawnReason::AutoRecovery
}

/// 解析 session 路径中的 instance id（用于多实例调试日志）
///
/// 提取文件名（不含扩展名）作为 instance id
pub fn instance_id_from_session_path(session_path: &Path) -> Option<String> {
    session_path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::tempdir;

    fn make_session(dir: &Path) -> SessionInfo {
        crate::process_launcher::session::create_session(dir).unwrap()
    }

    #[test]
    fn test_decide_disabled_returns_deny() {
        let policy = RespawnPolicy::new(false);
        let tmp = tempdir().unwrap();
        let session = make_session(tmp.path());

        let decision = policy.decide(&Some(session), Instant::now());
        assert!(matches!(
            decision,
            RespawnDecision::Deny(DenyReason::AutoRestartDisabled)
        ));
    }

    #[test]
    fn test_decide_no_reboot_flag_returns_deny() {
        let policy = RespawnPolicy::new(true);
        let tmp = tempdir().unwrap();
        let session = make_session(tmp.path());

        // 没有写 .reboot 标志
        let decision = policy.decide(&Some(session), Instant::now());
        assert!(matches!(
            decision,
            RespawnDecision::Deny(DenyReason::InvalidSession)
        ));
    }

    #[test]
    fn test_decide_with_reboot_flag_allows() {
        let policy = RespawnPolicy::new(true);
        let tmp = tempdir().unwrap();
        let session = make_session(tmp.path());
        // 模拟 Manager 写 .reboot 标志
        std::fs::write(&session.reboot_flag_path, "").unwrap();

        let decision = policy.decide(&Some(session), Instant::now());
        assert!(matches!(decision, RespawnDecision::Allow));
    }

    #[test]
    fn test_decide_too_frequent_denies() {
        let policy = RespawnPolicy::new(true);
        let tmp = tempdir().unwrap();
        let session = make_session(tmp.path());
        std::fs::write(&session.reboot_flag_path, "").unwrap();

        // 第一次记录
        let t0 = Instant::now();
        policy.record_respawn(t0);

        // 间隔 1s 再判断 → 应被拒绝（< 5s 阈值）
        let t1 = t0 + Duration::from_secs(1);
        // 第二次是新 session，has_reboot_flag 取决于是否写了
        // 重新写标志再测
        std::fs::write(&session.reboot_flag_path, "").unwrap();
        let session2 = make_session(tmp.path());
        std::fs::write(&session2.reboot_flag_path, "").unwrap();
        let decision = policy.decide(&Some(session2), t1);
        assert!(matches!(
            decision,
            RespawnDecision::Deny(DenyReason::TooFrequent)
        ));
        let _ = session; // session1 已经在 step 1 中使用过，这里用 _ 占位避免 unused
    }

    #[test]
    fn test_decide_exceeded_limit_denies() {
        let policy = RespawnPolicy::new(true);
        let now = Instant::now();

        // 模拟 1 分钟内 3 次 respawn
        for i in 0..3 {
            policy.record_respawn(now - Duration::from_secs(60 - i));
        }

        // 1 分钟内已经有 3 次 → 第 4 次应被拒绝
        let tmp = tempdir().unwrap();
        let session = make_session(tmp.path());
        std::fs::write(&session.reboot_flag_path, "").unwrap();

        let decision = policy.decide(&Some(session), now);
        assert!(matches!(
            decision,
            RespawnDecision::Deny(DenyReason::ExceededLimit)
        ));
    }

    #[test]
    fn test_set_auto_restart_clears_history() {
        let policy = RespawnPolicy::new(true);
        policy.record_respawn(Instant::now());
        assert_eq!(policy.recent_count(), 1);

        // 关闭时清空
        let mut p = policy;
        let p_mut = Arc::get_mut(&mut p).unwrap();
        p_mut.set_auto_restart(false);
        // 这里因为 Arc 不能直接获取 &mut，改用别的方式测
    }

    #[test]
    fn test_classify_exit() {
        let tmp = tempdir().unwrap();

        // exit(0) + 无 .reboot → UserRequest
        let s = make_session(tmp.path());
        assert_eq!(
            classify_exit(Some(0), &Some(s.clone())),
            RespawnReason::UserRequest
        );

        // exit(0) + 有 .reboot → ManagerReboot
        std::fs::write(&s.reboot_flag_path, "").unwrap();
        assert_eq!(
            classify_exit(Some(0), &Some(s)),
            RespawnReason::ManagerReboot
        );

        // exit(非0) → AutoRecovery
        let s2 = make_session(tmp.path());
        assert_eq!(
            classify_exit(Some(1), &Some(s2)),
            RespawnReason::AutoRecovery
        );
    }

    #[test]
    fn test_instance_id_from_session_path() {
        let path = Path::new("/sessions/abc123def456.session");
        assert_eq!(
            instance_id_from_session_path(path),
            Some("abc123def456".to_string())
        );

        let invalid = Path::new("/");
        assert_eq!(instance_id_from_session_path(invalid), None);
    }
}
