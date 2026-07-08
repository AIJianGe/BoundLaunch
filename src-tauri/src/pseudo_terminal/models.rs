//! 伪终端数据结构

use serde::{Deserialize, Serialize};

/// 终端窗口大小
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PtySize {
    pub cols: u16,
    pub rows: u16,
}

impl Default for PtySize {
    fn default() -> Self {
        Self { cols: 120, rows: 40 }
    }
}

/// 终端会话信息
#[derive(Debug, Clone, Serialize)]
pub struct TerminalSessionInfo {
    pub session_id: String,
    pub shell: String,
    pub cwd: String,
    pub size: PtySize,
    pub is_alive: bool,
    pub exit_code: Option<i32>,
    pub created_at: String,
}

/// pty_output 事件载荷
#[derive(Debug, Clone, Serialize)]
pub struct PtyOutputEvent {
    pub session_id: String,
    /// Base64 编码的原始字节数据（终端控制序列可能含非 UTF-8 字节）
    pub data: String,
}

/// pty_exit 事件载荷
#[derive(Debug, Clone, Serialize)]
pub struct PtyExitEvent {
    pub session_id: String,
    pub exit_code: Option<i32>,
}
