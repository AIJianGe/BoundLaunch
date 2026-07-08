//! 伪终端模块 - 跨平台交互式终端
//!
//! 基于 portable-pty crate（Windows ConPTY / Unix PTY），
//! 提供交互式终端会话，支持实时输入输出、窗口大小调整。
//!
//! 使用方式：
//! 1. 前端调用 `pty_create_session` 创建会话，得到 session_id
//! 2. 前端监听 `pty_output` 事件接收终端输出
//! 3. 前端调用 `pty_write` 发送输入
//! 4. 前端调用 `pty_resize` 调整窗口大小
//! 5. 前端调用 `pty_close` 关闭会话，或监听 `pty_exit` 事件

pub mod models;
pub mod service;

pub use models::{PtySize, TerminalSessionInfo};
pub use service::PseudoTerminalService;
