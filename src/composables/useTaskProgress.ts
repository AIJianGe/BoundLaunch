/**
 * useTaskProgress — F32 任务进度跟踪 composable（v3.5 扩展）
 *
 * 设计模式：**Observer** - 订阅 TaskScheduler 的 `task_progress` / `task_completed` / `task_log` 事件
 *
 * 使用场景：
 * - OnboardingPage：创建 venv / 安装 torch / 安装依赖时显示进度条
 * - SettingsPage：切换 torch 变体 / 切换 Python 版本 / 重建 venv 时显示进度
 * - 首页「一键补装」按钮：显示 InstallRequirements 进度
 * - **v3.5 新增**：CoreVersionPage 版本切换，显示百分比 + 实时日志
 *
 * 使用方式：
 * ```ts
 * const { progress, message, isRunning, logs, trackTask, cancel } = useTaskProgress();
 *
 * async function createVenv() {
 *   const taskId = await envCreateVenv('3.11');
 *   await trackTask(taskId, {
 *     onComplete: () => toast.success('venv 创建成功'),
 *     onError: (msg) => toast.error('venv 创建失败', msg ?? ''),
 *   });
 * }
 * ```
 *
 * v3.5 实时日志：
 * - 订阅 `task_log` 事件，累积到 `logs` 数组
 * - 每条日志带 source（如 "git fetch" / "uv pip install"）和 ts_ms 时间戳
 * - 适合在对话框下方显示"实时日志"面板
 *
 * 详见 `PR/06-界面设计.md §5.7 任务进度组件`
 */

import { ref, onUnmounted } from "vue";
import { listen, type UnlistenFn } from "@/api";
import { taskCancel } from "@/api/task";
import type {
  TaskProgressEvent,
  TaskTerminalEvent,
  LogEntry as _LogEntry,
} from "@/api/types";

/** 单条实时日志（前端 UI 友好结构） */
export interface TaskLogLine {
  /** 日志来源（"git" / "uv" / "checkout" / "fetch" 等） */
  source: string;
  /** 日志文本（单行） */
  text: string;
  /** 时间戳（ms since epoch，前端可 format） */
  ts_ms: number;
}

/** 任务回调（终态触发一次，进度持续触发） */
export interface TaskCallbacks {
  /** 任务成功完成时触发（summary 为后端 TaskResult.summary） */
  onComplete?: (summary: string | null) => void;
  /** 任务失败或取消时触发（summary 为错误信息） */
  onError?: (summary: string | null) => void;
  /** 进度更新时触发（0..=100） */
  onProgress?: (progress: number, message: string | null) => void;
  /** v3.5 新增：实时日志行触发（多次） */
  onLog?: (log: TaskLogLine) => void;
}

/** 后端 task_log 事件 payload 格式 */
interface TaskLogEventRaw {
  task_id: string;
  source: string;
  text: string;
  ts_ms: number;
}

/**
 * 任务进度跟踪 composable
 *
 * 每次调用创建一个独立的跟踪器，内部订阅全局 `task_progress` / `task_completed` / `task_log` 事件，
 * 通过 `currentTaskId` 过滤只处理自己关心的任务。
 *
 * 组件卸载时自动清理监听器（onUnmounted）。
 */
export function useTaskProgress() {
  // ========== State ==========
  /** 当前跟踪的 task_id（null 表示未跟踪） */
  const currentTaskId = ref<string | null>(null);
  /** 进度百分比 0..=100 */
  const progress = ref(0);
  /** 人类可读消息（如 "下载 torch wheel..."） */
  const message = ref<string | null>(null);
  /** 当前状态字符串：queued / running / completed / failed / cancelled */
  const status = ref<string>("queued");
  /** 是否运行中（queued 或 running） */
  const isRunning = ref(false);
  /** 是否成功完成 */
  const isCompleted = ref(false);
  /** 是否失败或取消 */
  const isFailed = ref(false);
  /** 错误摘要（失败时填充） */
  const errorSummary = ref<string | null>(null);

  /** v3.5 新增：实时日志累积（仅当前任务） */
  const logs = ref<TaskLogLine[]>([]);
  /** v3.5 新增：日志最大保留条数（防止内存爆炸） */
  const MAX_LOGS = 500;

  // ========== Internal ==========
  const unlisteners: UnlistenFn[] = [];
  let callbacks: TaskCallbacks = {};

  // ========== Actions ==========

  /**
   * 设置事件监听器（订阅 task_progress / task_completed / task_log）
   *
   * 内部方法，由 trackTask 调用。
   */
  async function setupListeners() {
    // 进度事件
    unlisteners.push(
      await listen<TaskProgressEvent>("task_progress", (e) => {
        if (e.payload.task_id !== currentTaskId.value) return;
        progress.value = e.payload.progress;
        message.value = e.payload.message;
        status.value = e.payload.status;
        callbacks.onProgress?.(e.payload.progress, e.payload.message);
      }),
    );

    // 终态事件
    unlisteners.push(
      await listen<TaskTerminalEvent>("task_completed", (e) => {
        if (e.payload.task_id !== currentTaskId.value) return;
        status.value = e.payload.status;
        isRunning.value = false;
        if (e.payload.status === "completed") {
          isCompleted.value = true;
          progress.value = 100;
          callbacks.onComplete?.(e.payload.summary);
        } else if (e.payload.status === "failed" || e.payload.status === "cancelled") {
          isFailed.value = true;
          errorSummary.value = e.payload.summary;
          callbacks.onError?.(e.payload.summary);
        }
        // 终态后清理监听器（避免重复触发）
        cleanup();
      }),
    );

    // v3.5：实时日志事件（一次 emit 一批 LogEvent）
    unlisteners.push(
      await listen<TaskLogEventRaw[]>("task_log", (e) => {
        if (!currentTaskId.value) return;
        // 后端 emit 整批（Vec<LogEvent>），按 task_id 过滤
        const events = Array.isArray(e.payload) ? e.payload : [];
        for (const ev of events) {
          if (ev.task_id !== currentTaskId.value) continue;
          const line: TaskLogLine = {
            source: ev.source,
            text: ev.text,
            ts_ms: ev.ts_ms,
          };
          logs.value.push(line);
          callbacks.onLog?.(line);
        }
        // 截断防止内存爆炸
        if (logs.value.length > MAX_LOGS) {
          logs.value.splice(0, logs.value.length - MAX_LOGS);
        }
      }),
    );
  }

  /** 清理所有事件监听器 */
  function cleanup() {
    unlisteners.forEach((un) => un());
    unlisteners.length = 0;
  }

  /**
   * 跟踪指定任务
   *
   * 调用此方法后，composable 会自动订阅事件并更新响应式状态。
   * 任务进入终态（completed / failed / cancelled）后会自动清理监听器。
   *
   * @param taskId 任务 ID（来自 invoke 返回值）
   * @param cb 回调（可选）
   */
  async function trackTask(taskId: string, cb?: TaskCallbacks) {
    // 若已有跟踪中的任务，先清理
    cleanup();

    currentTaskId.value = taskId;
    callbacks = cb ?? {};
    isRunning.value = true;
    isCompleted.value = false;
    isFailed.value = false;
    errorSummary.value = null;
    progress.value = 0;
    message.value = null;
    status.value = "queued";
    logs.value = [];

    await setupListeners();
  }

  /**
   * v3.5 新增：取消当前跟踪的任务
   *
   * 调后端 `task_cancel` 命令（幂等：已终态任务返回 Ok 不报错）。
   * 父任务取消时会级联取消所有子任务（见 task_scheduler::factory::spawn_child_progress_forwarder）。
   *
   * 取消后：
   * - `status` 会变为 "cancelled"
   * - `isFailed` 会变为 true
   * - 触发 `onError` 回调
   */
  async function cancel() {
    if (!currentTaskId.value) return;
    if (!isRunning.value) return; // 终态任务无需 cancel
    try {
      await taskCancel(currentTaskId.value);
    } catch (e) {
      // 静默失败：task_cancel 幂等，但网络/序列化错误 log
      console.warn("[useTaskProgress] task_cancel failed:", e);
    }
  }

  /** 重置状态（不清理监听器，用于复用 composable） */
  function reset() {
    currentTaskId.value = null;
    progress.value = 0;
    message.value = null;
    status.value = "queued";
    isRunning.value = false;
    isCompleted.value = false;
    isFailed.value = false;
    errorSummary.value = null;
    logs.value = [];
    callbacks = {};
  }

  /** 清空日志（保留监听器） */
  function clearLogs() {
    logs.value = [];
  }

  // 组件卸载时自动清理
  onUnmounted(() => {
    cleanup();
  });

  return {
    // state
    currentTaskId,
    progress,
    message,
    status,
    isRunning,
    isCompleted,
    isFailed,
    errorSummary,
    // v3.5：实时日志
    logs,
    // actions
    trackTask,
    cancel,
    cleanup,
    reset,
    clearLogs,
  };
}
