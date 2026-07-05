/**
 * Process Store
 *
 * 设计模式：
 * - **Store (Flux)**：集中管理进程状态
 * - **Observer**：监听后端事件流（starting/started/stopping/stopped/log/stale_process）
 * - **State Machine**：前端镜像后端 ProcessStatus 状态机
 *
 * 事件订阅清单：
 * - `process_starting`：状态 → Starting
 * - `process_started`：状态 → Running（含 pid / port）
 * - `process_stopping`：状态 → Stopping
 * - `process_stopped`：状态 → Stopped / Crashed
 * - `stale_process_detected`：遗留进程检测（前端弹窗确认是否强杀）
 * - `log`：实时日志行（追加到 logBuffer）
 * - `app_exiting` / `app_exited`：F24 退出流程事件
 *
 * 使用方式：
 * ```ts
 * const store = useProcessStore();
 * await store.subscribe(); // App.vue onMounted 调用一次
 * await store.start();      // 启动 ComfyUI
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
import type { ProcessStatus, ShutdownReason } from "@/api/types";

/** 前端日志缓冲最大容量（超出自动剔除最旧） */
const MAX_LOG_BUFFER = 1000;

/** 遗留进程信息（来自 stale_process_detected 事件） */
export interface StaleProcessInfo {
  pid: number;
  started_at: string;
  args: unknown;
}

export const useProcessStore = defineStore("process", () => {
  // ========== State ==========
  const status = ref<ProcessStatus>({ kind: "stopped" });
  const loading = ref(false);
  const error = ref<string | null>(null);
  const logBuffer = ref<string[]>([]);
  const staleProcess = ref<StaleProcessInfo | null>(null);
  /** F24 退出中标记（用于启动页按钮 disabled + spinner + 「正在退出...」） */
  const isExiting = ref(false);
  /** F24 退出原因（来自 AppExiting 事件载荷） */
  const exitingReason = ref<ShutdownReason | null>(null);

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
   * 实时日志通过 "log" 事件追加。
   */
  async function loadHistoryLogs(lines = 200) {
    logBuffer.value = await processTailLog(lines);
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
      await listen<string>("log", (e) => {
        appendLog(e.payload);
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
    staleProcess,
    isExiting,
    exitingReason,
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
    setExiting,
    subscribe,
    unsubscribe,
  };
});
