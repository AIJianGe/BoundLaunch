//! LogStore Tauri commands（门面层）
//!
//! 设计模式：**门面 (Facade)** - 前端只与本层交互，不直接调 LogStoreService
//!
//! 详见 `PR/03-模块设计/09-LogStore.md §3 接口签名` 末尾的 `#[tauri::command]` 定义

use crate::app_state::AppState;
use crate::log_store::repository::{LogEntry, LogLevel, LogQueryFilter, TaskHistoryRecord};
use chrono::Utc;
use serde::Deserialize;
use tauri::{AppHandle, Emitter, State};

/// 按条件查询历史日志
///
/// 前端示例：
/// ```ts
/// await invoke('log_query', {
///   filter: { level: 'error', limit: 100, offset: 0 }
/// })
/// ```
#[tauri::command]
pub async fn log_query(
    filter: LogQueryFilter,
    state: State<'_, AppState>,
) -> Result<Vec<LogEntry>, String> {
    state.log_store.logs().query(&filter).await.map_err(|e| e.to_string())
}

/// 取最近 N 行日志
///
/// 前端示例：`await invoke('log_tail', { lines: 500 })`
#[tauri::command]
pub async fn log_tail(
    lines: usize,
    state: State<'_, AppState>,
) -> Result<Vec<LogEntry>, String> {
    state.log_store.logs().tail(lines).await.map_err(|e| e.to_string())
}

/// 清空所有日志（调试 / 用户手动清理场景）
#[tauri::command]
pub async fn log_clear(state: State<'_, AppState>) -> Result<(), String> {
    state.log_store.logs().clear_all().await.map_err(|e| e.to_string())
}

/// 查询任务历史（前端「任务进度中心」页）
///
/// 前端示例：`await invoke('task_history_list', { limit: 100 })`
#[tauri::command]
pub async fn task_history_list(
    limit: usize,
    state: State<'_, AppState>,
) -> Result<Vec<TaskHistoryRecord>, String> {
    state.log_store.tasks().query_history(limit).await.map_err(|e| e.to_string())
}

// ============================================================================
// v3.10：业务错误快捷通道（覆盖 toast.error / toast.warn 路径）
// ============================================================================

/// `log_append` 命令入参
///
/// 前端 `useToast` 在调 `toast.error` / `toast.warn` 时自动 invoke 这个命令。
/// 后端写入 LogStore + emit `business_log` 事件给前端（ErrorPanel 实时刷新 + 菜单红点）。
#[derive(Debug, Deserialize)]
pub struct LogAppendPayload {
    /// 日志级别："trace" | "debug" | "info" | "warn" | "error"
    /// 非 warn/error 自动降级为 info（LogStoreService::log_business_error 兜底）
    pub level: String,
    /// 来源标识（前端传入，如 "useEnvInstaller" / "PathsPanel" / "useStartComfyui"）
    /// 后端会加 `ui:` 前缀，便于与 comfyui: / task: 来源区分
    pub source: String,
    /// 主消息（toast 的 content）
    pub message: String,
    /// 详情（错误对象 message / stack 等，可选）
    pub detail: Option<String>,
}

/// 写入一条业务日志（v3.10 新增）
///
/// **核心价值**：让 toast.error / toast.warn 弹窗**自动**有日志备份，
/// 用户切到"日志"菜单能看到完整历史，**不再丢**。
///
/// **入参约束**：
/// - `level`：warn / error 入档为对应级别，其他降级为 info
/// - `source`：≤ 64 字符（过长截断）
/// - `message`：≤ 4096 字符（过长截断 + `…`）
/// - `detail`：拼接在 message 之后（用 `\n\n` 分隔）
///
/// **副作用**：
/// - 写 LogStore（`log_business_error`，异步 spawn 不阻塞）
/// - emit `business_log` 事件给前端（ErrorPanel 实时刷新）
///
/// **不阻塞**：调用立即返回，写库是后台 spawn。
/// 即使 LogStore 不可用（启动期 / 路径错误），也不会影响 toast 显示。
///
/// **AppHandle 注入**：Tauri 2 标准做法，从 `app: AppHandle` 参数取。
/// 这样不污染 AppState 结构，测试 fixture 不需要改。
#[tauri::command]
pub async fn log_append(
    payload: LogAppendPayload,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // 拼装完整 message（detail 拼在主消息后）
    let full_message = match &payload.detail {
        Some(d) if !d.is_empty() => format!("{}\n\n{}", payload.message, d),
        _ => payload.message.clone(),
    };

    // 解析 level
    let level = LogLevel::parse(&payload.level).unwrap_or(LogLevel::Info);

    // 入档（内部已经 tokio::spawn，调用立即返回）
    state
        .log_store
        .log_business_error(level, &payload.source, &full_message);

    // 实时 emit 给前端（ErrorPanel 实时刷新 + 菜单红点 +1）
    // 注意：emit 失败不影响主流程（前端无 ErrorPanel 时事件无消费者）
    if let Err(e) = app.emit(
        "business_log",
        &serde_json::json!({
            "level": payload.level,
            "source": format!("ui:{}", payload.source),
            "message": payload.message,
            "detail": payload.detail,
            "ts": Utc::now().to_rfc3339(),
        }),
    ) {
        tracing::warn!(error = %e, "emit business_log failed");
    }

    Ok(())
}
