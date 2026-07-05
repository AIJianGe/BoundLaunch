//! F24 诊断命令 - 把前端日志转发到后端终端
//!
//! 详见 `PR/03-模块设计/06-ProcessLauncher.md §12` 临时诊断用。
//!
//! 仅在调试 F24 关闭流程时使用，生产环境移除。

use tauri::AppHandle;
use tracing::info;

/// 前端日志转发（仅 dev 模式）
///
/// 接收前端的结构化日志，转发到后端 tracing 通道（输出到终端）。
/// 临时诊断工具，排查 F24 流程链路用。
///
/// # 参数
/// - `tag`: 日志标签（如 "[tray]" / "[useShutdown]" / "[useExitConfirm]"）
/// - `stage`: 当前阶段（"enter" / "decision" / "action_start" / "action_done" / "error"）
/// - `data`: 附加数据（JSON 字符串）
#[tauri::command]
pub async fn dev_log(tag: String, stage: String, data: String) -> Result<(), String> {
    info!(target: "frontend", tag = %tag, stage = %stage, data = %data, "[FE-LOG]");
    Ok(())
}
