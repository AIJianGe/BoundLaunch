/**
 * TaskScheduler 模块 API
 *
 * 对应后端 `commands/task_scheduler.rs`
 * 详见 `PR/03-模块设计/08-TaskScheduler.md`
 *
 * 事件：
 * - `task_queued`：任务入队（含 TaskInfo）
 * - `task_progress`：进度更新（含 { id, status, progress }）
 * - `task_completed`：任务完成（含 TaskInfo）
 */

import { invoke } from "./index";
import type { TaskInfo } from "./types";

/** 列出所有任务快照（按 started_at 倒序） */
export function taskList(): Promise<TaskInfo[]> {
  return invoke<TaskInfo[]>("task_list");
}

/** 取消任务（已终态任务返回 Ok，幂等） */
export function taskCancel(id: string): Promise<void> {
  return invoke<void>("task_cancel", { id });
}

/** 查询单个任务 */
export function taskGet(id: string): Promise<TaskInfo | null> {
  return invoke<TaskInfo | null>("task_get", { id });
}
