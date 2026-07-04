/**
 * Task Store
 *
 * 设计模式：
 * - **Store (Flux)**：集中管理任务列表
 * - **Observer**：监听 `task_queued` / `task_progress` / `task_completed` 事件
 *
 * 注意：任务提交不暴露为 Tauri command，仅由后端各 Service 调用。
 * 前端通过 listen 接收进度。
 */

import { defineStore } from "pinia";
import { ref, computed } from "vue";
import { taskList, taskCancel, taskGet } from "@/api/task";
import { listen, type UnlistenFn } from "@/api";
import type { TaskInfo } from "@/api/types";

export const useTaskStore = defineStore("task", () => {
  // ========== State ==========
  const tasks = ref<TaskInfo[]>([]);
  const loading = ref(false);
  const error = ref<string | null>(null);
  const unlisteners: UnlistenFn[] = [];

  // ========== Getters ==========
  const runningTasks = computed(() =>
    tasks.value.filter((t) => t.status.phase === "running"),
  );
  const queuedTasks = computed(() =>
    tasks.value.filter((t) => t.status.phase === "queued"),
  );
  const completedTasks = computed(() =>
    tasks.value.filter((t) => t.status.phase === "completed"),
  );
  const failedTasks = computed(() =>
    tasks.value.filter((t) => t.status.phase === "failed"),
  );
  const hasRunning = computed(() => runningTasks.value.length > 0);

  // ========== Actions ==========

  /** 加载任务列表 */
  async function load() {
    loading.value = true;
    error.value = null;
    try {
      tasks.value = await taskList();
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e);
      throw e;
    } finally {
      loading.value = false;
    }
  }

  /** 取消任务（已终态任务返回 Ok，幂等） */
  async function cancel(id: string) {
    await taskCancel(id);
    // 立即更新本地状态（不等事件）
    const idx = tasks.value.findIndex((t) => t.id === id);
    if (idx >= 0) {
      tasks.value[idx] = {
        ...tasks.value[idx],
        status: { phase: "cancelled" },
        completed_at: new Date().toISOString(),
      };
    }
  }

  /** 查询单个任务（同步从本地缓存查，找不到则调后端） */
  async function get(id: string): Promise<TaskInfo | null> {
    const local = tasks.value.find((t) => t.id === id);
    if (local) return local;
    return taskGet(id);
  }

  /** 订阅任务事件 */
  async function subscribe() {
    if (unlisteners.length > 0) return;
    unlisteners.push(
      await listen<TaskInfo>("task_queued", (e) => {
        // 新任务入队：追加到列表
        const exists = tasks.value.find((t) => t.id === e.payload.id);
        if (!exists) {
          tasks.value.unshift(e.payload);
        }
      }),
      await listen<{ id: string; status: TaskInfo["status"]; progress?: number }>(
        "task_progress",
        (e) => {
          const idx = tasks.value.findIndex((t) => t.id === e.payload.id);
          if (idx >= 0) {
            tasks.value[idx] = { ...tasks.value[idx], status: e.payload.status };
          }
        },
      ),
      await listen<TaskInfo>("task_completed", (e) => {
        const idx = tasks.value.findIndex((t) => t.id === e.payload.id);
        if (idx >= 0) {
          tasks.value[idx] = e.payload;
        } else {
          // 任务列表未含此任务（可能是其他实例触发），追加
          tasks.value.unshift(e.payload);
        }
      }),
    );
  }

  function unsubscribe() {
    unlisteners.forEach((un) => un());
    unlisteners.length = 0;
  }

  return {
    // state
    tasks,
    loading,
    error,
    // getters
    runningTasks,
    queuedTasks,
    completedTasks,
    failedTasks,
    hasRunning,
    // actions
    load,
    cancel,
    get,
    subscribe,
    unsubscribe,
  };
});
