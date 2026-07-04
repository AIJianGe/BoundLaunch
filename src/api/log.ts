/**
 * LogStore 模块 API
 *
 * 对应后端 `commands/log_store.rs`
 * 详见 `PR/03-模块设计/09-LogStore.md`
 *
 * 日志来源：
 * - ComfyUI 进程 stdout/stderr（通过 LogPipeline 持久化）
 * - 任务执行日志（TaskScheduler append）
 * - 系统 / 业务日志
 */

import { invoke } from "./index";
import type { LogEntry, LogQueryOptions, TaskHistoryRecord, LogLevel } from "./types";

/**
 * 查询日志（支持多维过滤）
 *
 * @param options 查询条件（均为可选）
 * @returns 日志条目列表（按 timestamp 倒序，limit 默认 100）
 */
export function logQuery(options: LogQueryOptions = {}): Promise<LogEntry[]> {
  return invoke<LogEntry[]>("log_query", { options });
}

/**
 * 读取最近 N 条日志
 *
 * 简化版查询（等价于 logQuery({ limit: n })，但走不同后端路径，更快）。
 *
 * @param lines 行数（默认 100，最大 1000）
 */
export function logTail(lines = 100): Promise<LogEntry[]> {
  return invoke<LogEntry[]>("log_tail", { lines });
}

/**
 * 清空所有日志
 *
 * 危险操作，仅用于设置页 "清空日志" 按钮。
 * 不可恢复，但 task_history 表保留。
 */
export function logClear(): Promise<void> {
  return invoke<void>("log_clear");
}

/** 列出任务历史记录（task_history 表） */
export function taskHistoryList(limit = 50): Promise<TaskHistoryRecord[]> {
  return invoke<TaskHistoryRecord[]>("task_history_list", { limit });
}

export type { LogEntry, LogQueryOptions, TaskHistoryRecord, LogLevel };
