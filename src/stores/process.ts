/**
 * Process Store
 *
 * 设计模式：
 * - **Store (Flux)**：集中管理进程状态
 * - **Observer**：监听后端事件流（starting/started/stopping/stopped/log/stale_process/crashed）
 * - **State Machine**：前端镜像后端 ProcessStatus 状态机
 *
 * 事件订阅清单：
 * - `process_starting`：状态 → Starting
 * - `process_started`：状态 → Running（含 pid / port）
 * - `process_stopping`：状态 → Stopping
 * - `process_stopped`：状态 → Stopped / Crashed
 * - `process_crashed`（v3.4 新增）：child 死亡时立即 emit，载荷含 exit_code + stderr_tail
 * - `stale_process_detected`：遗留进程检测（前端弹窗确认是否强杀）
 * - `log`：实时日志行（追加到 logBuffer）
 * - `app_exiting` / `app_exited`：F24 退出流程事件
 *
 * 使用方式：
 * ```ts
 * const store = useProcessStore();
 * await store.subscribe(); // App.vue onMounted 调用一次
 * await store.start();      // 启动 ComfyUI（v3.4：返回 task_id 已废弃，直接用 useStartComfyui）
 * await store.stop();       // 停止 ComfyUI
 * store.setExiting(true);   // F24 退出流程标记（启动页按钮置灰）
 * ```
 */

import { defineStore } from "pinia";
import { ref, computed } from "vue";
import {
  processStart,
  processStop,
  processStatus,
  processTailLog,
  processKillStale,
} from "@/api/process";
import { listen, type UnlistenFn } from "@/api";
import type { ComfyUILogEvent, ProcessStatus, ShutdownReason } from "@/api/types";

/** 前端日志缓冲最大容量（超出自动剔除最旧） */
const MAX_LOG_BUFFER = 1000;

/** 遗留进程信息（来自 stale_process_detected 事件） */
export interface StaleProcessInfo {
  pid: number;
  started_at: string;
  args: unknown;
}

/** v3.4 新增：process_crashed 事件 payload（对应后端 serde_json::json!） */
export interface ProcessCrashedEvent {
  exit_code: number | null;
  stderr_tail: string[];
  reason: "early_exit" | "health_check_detected" | "monitor_detected";
}

export const useProcessStore = defineStore("process", () => {
  // ========== State ==========
  const status = ref<ProcessStatus>({ kind: "stopped" });
  const loading = ref(false);
  const error = ref<string | null>(null);
  const logBuffer = ref<string[]>([]);
  /**
   * v3.2.2 结构化日志条目（含 source / ts）— 供 TerminalPanel 用
   * 与 logBuffer 并行：logBuffer 保留兼容旧 LogsPage，logEntries 供新组件
   */
  const logEntries = ref<ComfyUILogEvent[]>([]);
  const staleProcess = ref<StaleProcessInfo | null>(null);
  /** F24 退出中标记（用于启动页按钮 disabled + spinner + 「正在退出...」） */
  const isExiting = ref(false);
  /** F24 退出原因（来自 AppExiting 事件载荷） */
  const exitingReason = ref<ShutdownReason | null>(null);
  /** v3.4 新增：最近一次 process_crashed 事件详情（前端弹窗 + LogsPage 顶部展示） */
  const crashedReason = ref<ProcessCrashedEvent | null>(null);

  const unlisteners: UnlistenFn[] = [];

  // ========== Getters ==========
  const isRunning = computed(() => status.value.kind === "running");
  const isStarting = computed(() => status.value.kind === "starting");
  const isStopping = computed(() => status.value.kind === "stopping");
  const isAlive = computed(() => isStarting.value || isRunning.value);
  const isCrashed = computed(() => status.value.kind === "crashed");
  const pid = computed<number | null>(() =>
    status.value.kind === "running" ? status.value.pid : null,
  );
  const port = computed<number | null>(() => {
    if (status.value.kind === "running" || status.value.kind === "starting") {
      return status.value.port;
    }
    return null;
  });

  // ========== Actions ==========

  /**
   * 启动 ComfyUI 进程
   *
   * 错误处理：
   * - AlreadyRunning / PortInUse / EnvironmentNotReady 等会被抛出，调用方捕获后展示 UI
   */
  async function start() {
    loading.value = true;
    error.value = null;
    try {
      await processStart();
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e);
      throw e;
    } finally {
      loading.value = false;
    }
  }

  /** 停止 ComfyUI 进程（幂等） */
  async function stop() {
    loading.value = true;
    error.value = null;
    try {
      await processStop();
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e);
      throw e;
    } finally {
      loading.value = false;
    }
  }

  /** 拉取最新状态（手动刷新，正常情况下事件订阅会自动同步） */
  async function refreshStatus() {
    status.value = await processStatus();
  }

  /**
   * 加载历史日志（启动页打开时调用）
   *
   * 后端环形缓冲保留最近 5000 行，前端取 200 行用于初始展示。
   * 实时日志通过 "comfyui_log" 事件追加。
   *
   * v3.2.2：同步填充 logBuffer（兼容 LogsPage）和 logEntries（供 TerminalPanel）
   * 后端 tail_log 不区分 stdout/stderr，统一标记为 stdout
   *
   * v3.4.2：增加 `append` 参数
   * - `append=false`（默认）：**替换**模式。清空 logBuffer + logEntries，加载最新 N 行
   *   - 适用：App.vue 启动初始化、用户主动点"刷新"按钮
   * - `append=true`：**追加**模式。在现有 logBuffer 末尾追加新行
   *   - 适用：用户切换页面（LaunchPage / LogsPage / TerminalPanel）时挂载
   *   - 解决"切换页面再回来，日志消失"问题（之前是替换模式 + 后端 log_pipeline drop → 没了）
   *   - 可能产生少量重复（in-memory logBuffer 与后端 RingBuffer 有重叠区域），但不会丢日志
   */
  async function loadHistoryLogs(lines = 200, append = false) {
    const history = await processTailLog(lines);
    if (append) {
      // v3.4.2：追加模式 → 不清空已有 logBuffer
      // 用 Set 去重：避免 in-memory 与后端 RingBuffer 重叠区域重复
      const existing = new Set(logBuffer.value);
      const newLines = history.filter((line) => !existing.has(line));
      logBuffer.value = [...logBuffer.value, ...newLines];
      // logEntries 同步
      const existingEntries = new Set(
        logEntries.value.map((e) => `${e.ts}:${e.line}`),
      );
      const newEntries = history
        .filter((line) => !existing.has(line))
        .map((line) => ({
          source: "stdout" as const,
          line,
          ts: new Date().toISOString(),
        }))
        .filter((e) => !existingEntries.has(`${e.ts}:${e.line}`));
      logEntries.value = [...logEntries.value, ...newEntries];
    } else {
      // 替换模式（默认）：完全覆盖
      logBuffer.value = history;
      logEntries.value = history.map((line) => ({
        source: "stdout" as const,
        line,
        ts: new Date().toISOString(),
      }));
    }
  }

  /** 追加日志行（来自 "log" 事件） */
  function appendLog(line: string) {
    logBuffer.value.push(line);
    if (logBuffer.value.length > MAX_LOG_BUFFER) {
      logBuffer.value.splice(0, logBuffer.value.length - MAX_LOG_BUFFER);
    }
  }

  /** 清空日志缓冲（切换页面 / 手动清空时调用） */
  function clearLogs() {
    logBuffer.value = [];
    logEntries.value = [];
  }

  /**
   * 强制杀死遗留进程
   *
   * 用户在前端确认弹窗 "检测到遗留进程" 后调用。
   */
  async function killStale(pid: number) {
    await processKillStale(pid);
    staleProcess.value = null;
  }

  /** 忽略遗留进程提示（前端关闭弹窗即可，PID 文件后端会自动清理） */
  function dismissStale() {
    staleProcess.value = null;
  }

  /**
   * v3.4 新增：清掉 crashedReason（用户关闭 LogsPage 顶部弹窗 / 重新启动后调用）
   */
  function dismissCrashed() {
    crashedReason.value = null;
  }

  /**
   * F24 退出流程：设置退出中标记
   *
   * 启动页按钮据此 disabled + 显示 spinner + 「正在退出...」
   *
   * @param exiting true=进入退出态 / false=退出退出态（兜底，正常流程中 app.exit 后进程已终止）
   */
  function setExiting(exiting: boolean, reason?: ShutdownReason) {
    isExiting.value = exiting;
    if (exiting && reason) {
      exitingReason.value = reason;
    } else if (!exiting) {
      exitingReason.value = null;
    }
  }

  /**
   * 订阅后端事件
   *
   * 应在应用启动时（App.vue onMounted）调用一次。
   */
  async function subscribe() {
    if (unlisteners.length > 0) return;

    unlisteners.push(
      await listen<ProcessStatus>("process_starting", (e) => {
        status.value = e.payload;
      }),
      await listen<ProcessStatus>("process_started", (e) => {
        status.value = e.payload;
        error.value = null;
        // 启动成功 → 清掉 crashedReason（如果之前失败过）
        crashedReason.value = null;
      }),
      await listen<ProcessStatus>("process_stopping", (e) => {
        status.value = e.payload;
      }),
      await listen<ProcessStatus>("process_stopped", (e) => {
        status.value = e.payload;
        // 崩溃时记录错误信息
        if (e.payload.kind === "crashed") {
          error.value = e.payload.error;
        }
      }),
      // v3.4 新增：process_crashed 事件
      // - 5s 内 child 死：early_exit 路径（reason）
      // - 5s~60s 之间 child 死：health_check_detected 路径
      // - 运行期间 child 死：monitor_detected 路径
      // 载荷含 exit_code + stderr_tail，前端 LogsPage / StartStopButtons 弹窗用
      await listen<ProcessCrashedEvent>("process_crashed", (e) => {
        crashedReason.value = e.payload;
        error.value = `ComfyUI 已崩溃（exit code: ${e.payload.exit_code ?? "未知"}）`;
        console.error("[processStore] process_crashed", e.payload);
      }),
      // v3.2.2 修复：事件名 `log` → `comfyui_log`（后端 log_pipeline.rs:185）
      // payload 是 { source, line, ts }，不是字符串
      await listen<ComfyUILogEvent>("comfyui_log", (e) => {
        appendLog(e.payload.line);
        logEntries.value.push(e.payload);
        if (logEntries.value.length > MAX_LOG_BUFFER) {
          logEntries.value.splice(0, logEntries.value.length - MAX_LOG_BUFFER);
        }
      }),
      await listen<StaleProcessInfo>("stale_process_detected", (e) => {
        staleProcess.value = e.payload;
      }),
      // F24 退出流程事件
      await listen<{ reason: ShutdownReason }>("app_exiting", (e) => {
        console.info("[processStore] app_exiting", e.payload);
        setExiting(true, e.payload.reason);
      }),
      await listen<void>("app_exited", () => {
        console.info("[processStore] app_exited");
        // 不重置 isExiting（后端即将 app.exit，前端保持显示）
      }),
    );

    // 订阅后立即拉取一次真实状态（防止错过启动期间的事件）
    try {
      await refreshStatus();
    } catch (e) {
      console.warn("refreshStatus failed:", e);
    }
  }

  /** 取消所有订阅（应用卸载时调用） */
  function unsubscribe() {
    unlisteners.forEach((un) => un());
    unlisteners.length = 0;
  }

  return {
    // state
    status,
    loading,
    error,
    logBuffer,
    logEntries, // v3.2.2
    staleProcess,
    isExiting,
    exitingReason,
    crashedReason, // v3.4
    // getters
    isRunning,
    isStarting,
    isStopping,
    isAlive,
    isCrashed,
    pid,
    port,
    // actions
    start,
    stop,
    refreshStatus,
    loadHistoryLogs,
    appendLog,
    clearLogs,
    killStale,
    dismissStale,
    dismissCrashed, // v3.4
    setExiting,
    subscribe,
    unsubscribe,
  };
});
