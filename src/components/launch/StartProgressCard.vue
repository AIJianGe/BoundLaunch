<script setup lang="ts">
/**
 * StartProgressCard.vue — 启动进度卡（顶部跨 Tab 显示）
 *
 * 设计目的：
 * - 用户在 LogsPage 任何 Tab 都能看到 ComfyUI 启动状态
 * - 不被 NTabs 切换影响（永远在顶部）
 * - 包含强制停止按钮（兜底机制）
 *
 * 数据源：
 * - taskStore.tasks（kind === "start_comfyui"）
 * - task_queued / task_progress / task_completed 事件
 *
 * 显示逻辑：
 * - 有 start_comfyui 任务在跑 → 显示卡片 + 计时器 + 强制停止按钮
 * - 无任务 → 不渲染
 */

import { ref, onMounted, onUnmounted } from "vue";
import {
  NCard,
  NSpace,
  NTag,
  NButton,
  NSpin,
  NText,
  useDialog,
} from "naive-ui";
import { useTaskStore } from "@/stores/task";
import { useProcessStore } from "@/stores/process";
import { useToast } from "@/composables/useToast";
import { listen, type UnlistenFn } from "@/api";
import { forceKillAllPython } from "@/api/port_diagnostics";
import type { TaskProgressEvent, TaskTerminalEvent } from "@/api/types";

const taskStore = useTaskStore();
const processStore = useProcessStore();
const toast = useToast();
const dialog = useDialog();

// ============================================================================
// 状态
// ============================================================================

const startTaskActive = ref(false);
const startTaskMessage = ref<string | null>(null);
const startElapsedSec = ref(0);
const forceKilling = ref(false);

let trackedStartTaskId: string | null = null;
let startTimerHandle: number | null = null;
const taskUnlisteners: UnlistenFn[] = [];

// ============================================================================
// 工具函数
// ============================================================================

function formatElapsed(sec: number): string {
  const m = Math.floor(sec / 60);
  const s = sec % 60;
  return m > 0 ? `${m}分${s}秒` : `${s}秒`;
}

function findActiveStartTask(): string | null {
  const running = taskStore.tasks.find(
    (t) => t.kind === "start_comfyui" && t.status.phase === "running",
  );
  return running?.id ?? null;
}

function startElapsedTimer() {
  stopElapsedTimer();
  startElapsedSec.value = 0;
  startTimerHandle = window.setInterval(() => {
    startElapsedSec.value += 1;
  }, 1000);
}

function stopElapsedTimer() {
  if (startTimerHandle !== null) {
    clearInterval(startTimerHandle);
    startTimerHandle = null;
  }
}

// ============================================================================
// 事件监听
// ============================================================================

async function setupStartTaskListeners() {
  taskUnlisteners.push(
    await listen<{ id: string; kind: string }>("task_queued", (e) => {
      if (e.payload.kind === "start_comfyui") {
        trackedStartTaskId = e.payload.id;
        startTaskActive.value = true;
        startTaskMessage.value = "排队中...";
        startElapsedTimer();
      }
    }),
    await listen<TaskProgressEvent>("task_progress", (e) => {
      if (e.payload.task_id === trackedStartTaskId) {
        startTaskMessage.value = e.payload.message;
      }
    }),
    await listen<TaskTerminalEvent>("task_completed", (e) => {
      if (e.payload.task_id === trackedStartTaskId) {
        startTaskActive.value = false;
        trackedStartTaskId = null;
        startTaskMessage.value = null;
        stopElapsedTimer();
      }
    }),
  );
}

// ============================================================================
// 强制停止
// ============================================================================

async function onForceKill() {
  if (forceKilling.value) return;
  const confirmed = await new Promise<boolean>((resolve) => {
    dialog.warning({
      title: "⏹ 强制停止确认",
      content:
        "此操作将强制结束 ComfyUI 进程（包括所有相关 Python 子进程）。\n\n" +
        "正在加载的模型或处理中的请求可能被中断。\n\n" +
        "确定要继续吗？",
      positiveText: "强制结束",
      negativeText: "取消",
      onPositiveClick: () => resolve(true),
      onNegativeClick: () => resolve(false),
      onClose: () => resolve(false),
    });
  });
  if (!confirmed) return;
  forceKilling.value = true;
  toast.warn("正在强制结束 ComfyUI...");
  try {
    try {
      await processStore.stop();
    } catch (e) {
      console.warn("[onForceKill] processStore.stop failed:", e);
    }
    await new Promise((r) => setTimeout(r, 1500));
    if (processStore.isRunning || processStore.isStarting) {
      try {
        await forceKillAllPython();
        toast.success("已强制结束所有 Python 进程");
      } catch (e) {
        toast.error("强杀失败，请手动结束 Python 进程", String(e));
      }
    }
  } finally {
    forceKilling.value = false;
  }
}

// ============================================================================
// 生命周期
// ============================================================================

onMounted(async () => {
  // 恢复已有任务
  try {
    const existing = findActiveStartTask();
    if (existing) {
      trackedStartTaskId = existing;
      startTaskActive.value = true;
      const t = taskStore.tasks.find((t) => t.id === existing);
      if (t && t.status.phase === "running") {
        startTaskMessage.value = "进行中...";
        startElapsedTimer();
      }
    }
  } catch (e) {
    console.warn("[StartProgressCard] task load:", e);
  }
  await setupStartTaskListeners();
});

onUnmounted(() => {
  taskUnlisteners.forEach((un) => un());
  taskUnlisteners.length = 0;
  stopElapsedTimer();
});
</script>

<template>
  <NCard
    v-if="startTaskActive"
    class="start-progress"
    :bordered="true"
    size="small"
  >
    <NSpace vertical>
      <div class="start-progress-header">
        <span class="start-progress-title">
          <NSpin size="small" class="start-spin" />
          🚀 ComfyUI 启动中...
        </span>
        <NSpace>
          <NTag size="small" type="info">
            已等待 {{ formatElapsed(startElapsedSec) }}
          </NTag>
          <NButton
            type="error"
            size="small"
            :loading="forceKilling"
            :disabled="forceKilling"
            @click="onForceKill"
          >
            ⏹ 强制停止
          </NButton>
        </NSpace>
      </div>
      <div class="start-progress-message">
        {{ startTaskMessage || "准备中..." }}
      </div>
      <NText depth="3" class="start-progress-hint">
        ⏳ ComfyUI 加载时间取决于机器性能与模型大小，请耐心等待。
      </NText>
    </NSpace>
  </NCard>
</template>

<style scoped>
.start-progress {
  flex-shrink: 0;
  border-color: rgba(24, 160, 88, 0.4);
  background: linear-gradient(135deg, #f0f9f3 0%, #ffffff 100%);
  margin-bottom: 12px;
}

.start-progress-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  flex-wrap: wrap;
  gap: 8px;
}

.start-progress-title {
  display: flex;
  align-items: center;
  gap: 8px;
  font-weight: 600;
  font-size: 15px;
}

.start-spin {
  margin-right: 4px;
}

.start-progress-message {
  font-size: 13px;
  color: #555;
  padding: 4px 0;
}

.start-progress-hint {
  font-size: 12px;
  font-style: italic;
}
</style>
