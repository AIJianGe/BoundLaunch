/**
 * useErrorLog - 全局错误日志状态（v3.10 新增）
 *
 * 设计模式：
 * - **Singleton**：单例 store，跨组件共享
 * - **Observer**：订阅后端 `business_log` 事件
 * - **Facade**：封装 LogStore query + business_log 事件，对外提供简洁 API
 *
 * **核心价值**：
 * - 解决"toast 几秒消失=日志丢失"问题
 * - ErrorPanel 顶部展示最近错误，**永远不消失**
 * - MainLayout 菜单"日志"红点指示未读错误数
 *
 * **数据流**：
 * ```
 * useToast.error/warn
 *   ↓ invoke('log_append')
 * 后端 LogStoreService::log_business_error
 *   ↓ 异步写 logs 表
 * 后端 emit 'business_log' 事件
 *   ↓
 * useErrorLog.subscribe() 监听
 *   ↓
 * recentErrors.value.unshift(payload)
 *   ↓
 *   ├─ ErrorPanel 实时刷新
 *   └─ MainLayout 菜单红点 +1
 * ```
 *
 * **菜单红点清除策略**（推荐方案 (i)）：
 * - 用户进入 LogsPage 时清零
 * - 通过 `markAllRead()` 主动调用
 *
 * 使用方式：
 * ```ts
 * const errorLog = useErrorLog();
 * await errorLog.subscribe();
 *
 * // LogsPage 顶部 ErrorPanel
 * errorLog.recentErrors  // 最近 50 条
 *
 * // MainLayout 菜单红点
 * errorLog.unreadCount   // 0 = 不显示
 * ```
 */

import { ref, computed } from "vue";
import { defineStore } from "pinia";
import { listen, type UnlistenFn } from "@/api";
import { logQuery } from "@/api/log";
import type { BusinessLogEvent, LogEntry } from "@/api/types";

/** 内存中保留的最大错误数（防止 ref 无限增长） */
const MAX_RECENT_ERRORS = 50;

/** 进入 LogsPage 时拉取的最大历史错误数（初始 mount 用） */
const INITIAL_FETCH_LIMIT = 30;

export const useErrorLog = defineStore("errorLog", () => {
  // ========== State ==========
  /** 最近错误（最新的在前，最多 50 条） */
  const recentErrors = ref<BusinessLogEvent[]>([]);
  /** 未读错误数（用于菜单红点） */
  const unreadCount = ref(0);
  /** 是否已订阅后端 business_log 事件 */
  const subscribed = ref(false);
  /** 是否已初始拉取历史（避免重复拉） */
  const initialized = ref(false);
  /** 拉取历史时的错误（不影响主流程） */
  const initError = ref<string | null>(null);

  const unlisteners: UnlistenFn[] = [];

  // ========== Getters ==========
  /** 最近错误（限制 10 条，给 ErrorPanel 用，避免一次性渲染过多） */
  const displayErrors = computed(() => recentErrors.value.slice(0, 10));

  /** 是否有任何错误（控制 ErrorPanel 是否显示） */
  const hasErrors = computed(() => recentErrors.value.length > 0);

  // ========== Actions ==========

  /**
   * 订阅后端 `business_log` 事件（应用启动时调一次）
   *
   * **幂等**：重复调用不会重复订阅。
   */
  async function subscribe() {
    if (subscribed.value) return;

    unlisteners.push(
      await listen<BusinessLogEvent>("business_log", (e) => {
        const evt = e.payload;
        // 只关注 warn / error
        if (evt.level !== "warn" && evt.level !== "error") return;

        // 1. 追加到 recentErrors（去重：相同 ts + message 不重复）
        const isDuplicate = recentErrors.value.some(
          (existing) => existing.ts === evt.ts && existing.message === evt.message,
        );
        if (!isDuplicate) {
          recentErrors.value.unshift(evt);
          if (recentErrors.value.length > MAX_RECENT_ERRORS) {
            recentErrors.value.length = MAX_RECENT_ERRORS;
          }
        }

        // 2. 未读数 +1（只统计 error，warn 不进红点）
        if (evt.level === "error") {
          unreadCount.value += 1;
        }
      }),
    );

    subscribed.value = true;
  }

  /**
   * 拉取 LogStore 中已存在的 ERROR 日志（应用启动时调一次）
   *
   * **目的**：应用重启后，LogStore 里有历史 error，但 recentErrors 是空的。
   * 拉一次填进 recentErrors，让 ErrorPanel / 任务中心展开能看到历史。
   */
  async function loadHistory() {
    if (initialized.value) return;
    initialized.value = true;

    try {
      const history: LogEntry[] = await logQuery({
        level: "error",
        limit: INITIAL_FETCH_LIMIT,
        offset: 0,
      });

      // LogEntry → BusinessLogEvent（保持字段名一致）
      const events: BusinessLogEvent[] = history.map((e) => ({
        level: e.level,
        source: e.source,
        message: e.message,
        detail: null,
        ts: e.timestamp,
      }));

      // 最新的在前（LogStore::query 已经按时间倒序）
      recentErrors.value = events.slice(0, MAX_RECENT_ERRORS);
      // 历史不算未读（用户重启应用不算"新错误"）
      unreadCount.value = 0;
    } catch (e) {
      initError.value = e instanceof Error ? e.message : String(e);
      // eslint-disable-next-line no-console
      console.warn("[useErrorLog] loadHistory failed:", e);
    }
  }

  /**
   * 标记所有错误为已读（清零未读数）
   *
   * **调用时机**：用户进入 LogsPage 时（MainLayout 监听 route 触发）。
   */
  function markAllRead() {
    unreadCount.value = 0;
  }

  /**
   * 手动清空 recentErrors（保留 LogStore 数据）
   *
   * **使用场景**：用户点 ErrorPanel 上的"清空"按钮。
   * LogStore 数据不动，菜单红点清零。
   */
  function clearDisplayed() {
    recentErrors.value = [];
    unreadCount.value = 0;
  }

  /**
   * 取消所有订阅（应用卸载时调）
   */
  function unsubscribe() {
    unlisteners.forEach((un) => un());
    unlisteners.length = 0;
    subscribed.value = false;
  }

  return {
    // state
    recentErrors,
    unreadCount,
    subscribed,
    initialized,
    initError,
    // getters
    displayErrors,
    hasErrors,
    // actions
    subscribe,
    loadHistory,
    markAllRead,
    clearDisplayed,
    unsubscribe,
  };
});
