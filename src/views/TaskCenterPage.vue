<script setup lang="ts">
/**
 * 任务进度中心
 *
 * 详见 `PR/06-界面设计.md §5.5 任务进度中心`
 *
 * 三区结构：
 * 1. 进行中（running + cancelling）
 * 2. 排队中（queued）
 * 3. 历史（从 LogStore task_history 表读取）
 *
 * 实时更新：
 * - 通过 taskStore.subscribe() 订阅 task_queued / task_progress / task_completed 事件
 * - 历史任务通过 taskHistoryList API 查询
 *
 * 设计模式：
 * - **Store (Flux)**：所有任务状态集中在 taskStore
 * - **Observer**：listen 后端任务事件
 * - **Strategy**：不同 TaskStatus 不同 UI 表现（颜色 / 按钮 / 图标）
 */

import { ref, computed, onMounted, onUnmounted } from "vue";
import {
  NCard,
  NSpace,
  NButton,
  NTag,
  NProgress,
  NEmpty,
  NTooltip,
  NPopconfirm,
  NCollapse,
  NCollapseItem,
  NStatistic,
  NDescriptions,
  NDescriptionsItem,
} from "naive-ui";
import { useTaskStore } from "@/stores/task";
import { taskHistoryList } from "@/api/log";
import { useToast } from "@/composables/useToast";
import type { TaskInfo, TaskHistoryRecord, TaskStatus } from "@/api/types";

const taskStore = useTaskStore();
const toast = useToast();

const historyTasks = ref<TaskHistoryRecord[]>([]);
const expandedErrorIds = ref<Set<string>>(new Set());

// ========== 计算属性 ==========

const runningTasks = computed(() =>
  taskStore.tasks.filter(
    (t) => t.status.phase === "running" || t.status.phase === "queued",
  ),
);

const activeRunningTasks = computed(() =>
  taskStore.tasks.filter((t) => t.status.phase === "running"),
);

const queuedTasks = computed(() =>
  taskStore.tasks.filter((t) => t.status.phase === "queued"),
);

const recentFinishedTasks = computed(() =>
  taskStore.tasks.filter(
    (t) =>
      t.status.phase === "completed" ||
      t.status.phase === "failed" ||
      t.status.phase === "cancelled",
  ),
);

// ========== 工具函数 ==========

function getProgress(status: TaskStatus): number {
  if (status.phase === "running") return Math.round((status.progress ?? 0) * 100);
  if (status.phase === "completed") return 100;
  if (status.phase === "failed" || status.phase === "cancelled") {
    // 保留任务失败时的进度
    return 0;
  }
  return 0;
}

function getDuration(startedAt: string | null, completedAt: string | null): string {
  if (!startedAt) return "—";
  const start = new Date(startedAt).getTime();
  const end = completedAt ? new Date(completedAt).getTime() : Date.now();
  const sec = Math.floor((end - start) / 1000);
  if (sec < 60) return `${sec}s`;
  if (sec < 3600) return `${Math.floor(sec / 60)}m${sec % 60}s`;
  return `${Math.floor(sec / 3600)}h${Math.floor((sec % 3600) / 60)}m`;
}

function getRelativeTime(ts: string | null): string {
  if (!ts) return "—";
  const diff = Date.now() - new Date(ts).getTime();
  const sec = Math.floor(diff / 1000);
  if (sec < 60) return `${sec}秒前`;
  if (sec < 3600) return `${Math.floor(sec / 60)}分钟前`;
  if (sec < 86400) return `${Math.floor(sec / 3600)}小时前`;
  return `${Math.floor(sec / 86400)}天前`;
}

function statusColor(phase: TaskStatus["phase"]): "default" | "info" | "success" | "error" | "warning" {
  switch (phase) {
    case "queued": return "default";
    case "running": return "info";
    case "completed": return "success";
    case "failed": return "error";
    case "cancelled": return "warning";
    default: return "default";
  }
}

function statusLabel(phase: TaskStatus["phase"]): string {
  switch (phase) {
    case "queued": return "等待中";
    case "running": return "进行中";
    case "completed": return "成功";
    case "failed": return "失败";
    case "cancelled": return "已取消";
    default: return String(phase);
  }
}

function historyStatusColor(status: string): "default" | "success" | "error" | "warning" {
  if (status === "completed") return "success";
  if (status === "failed") return "error";
  if (status === "cancelled") return "warning";
  return "default";
}

function toggleErrorDetail(id: string) {
  if (expandedErrorIds.value.has(id)) {
    expandedErrorIds.value.delete(id);
  } else {
    expandedErrorIds.value.add(id);
  }
}

function copyError(text: string) {
  navigator.clipboard.writeText(text).then(
    () => toast.success("已复制错误信息"),
    () => toast.error("复制失败"),
  );
}

/** v3.10：截断错误信息用于折叠预览（前 N 字符 + "..."） */
function truncateError(text: string, maxLen: number): string {
  if (text.length <= maxLen) return text;
  // 截取到最近的换行符（避免截半个单词/半个 traceback 行）
  const truncated = text.slice(0, maxLen);
  const lastNewline = truncated.lastIndexOf("\n");
  const cut = lastNewline > maxLen / 2 ? truncated.slice(0, lastNewline) : truncated;
  return `${cut}\n…（共 ${text.length} 字符，点击展开）`;
}

// ========== Actions ==========

async function onRefresh() {
  try {
    await Promise.all([taskStore.load(), loadHistory()]);
    toast.success("已刷新");
  } catch (e) {
    toast.error("刷新失败", e);
  }
}

async function loadHistory() {
  try {
    historyTasks.value = await taskHistoryList(50);
  } catch (e) {
    console.warn("load history:", e);
  }
}

async function onCancelTask(id: string) {
  try {
    await taskStore.cancel(id);
    toast.info("已发送取消请求");
  } catch (e) {
    toast.error("取消失败", e);
  }
}

async function onClearHistory() {
  // LogStore 未暴露清空 task_history 的 API，仅前端清空显示
  // 后端持久化的 task_history 表保留
  historyTasks.value = [];
  toast.success("已清空历史显示");
}

// ========== 生命周期 ==========

onMounted(async () => {
  try {
    await taskStore.subscribe();
  } catch (e) {
    console.warn("task subscribe:", e);
  }
  await Promise.allSettled([taskStore.load(), loadHistory()]);
});

onUnmounted(() => {
  taskStore.unsubscribe();
});
</script>

<template>
  <div class="task-center">
    <!-- 顶部统计栏 -->
    <NCard class="header-card" :bordered="true" size="small">
      <div class="header-row">
        <div class="stats">
          <NStatistic label="进行中" :value="activeRunningTasks.length" />
          <NStatistic label="排队中" :value="queuedTasks.length" />
          <NStatistic label="历史" :value="historyTasks.length" />
        </div>
        <NSpace>
          <NButton size="small" @click="onRefresh" :loading="taskStore.loading">
            刷新
          </NButton>
          <NPopconfirm
            :on-positive-click="onClearHistory"
            positive-text="确认清空"
            negative-text="取消"
          >
            <template #trigger>
              <NButton size="small" type="warning" ghost>清空历史</NButton>
            </template>
            仅清空前端显示，后端 task_history 表保留
          </NPopconfirm>
        </NSpace>
      </div>
    </NCard>

    <!-- 进行中 + 排队中 -->
    <NCard :bordered="true" size="small" class="section-card">
      <template #header>
        <div class="card-header">
          <span class="header-title">⏳ 进行中</span>
          <NTag size="small" :type="runningTasks.length > 0 ? 'info' : 'default'">
            {{ runningTasks.length }}
          </NTag>
        </div>
      </template>

      <NEmpty
        v-if="runningTasks.length === 0"
        description="暂无进行中的任务"
        size="small"
      />
      <div v-else class="task-list">
        <div
          v-for="task in runningTasks"
          :key="task.id"
          class="task-row"
          :class="`task-${task.status.phase}`"
        >
          <div class="task-row-header">
            <NSpace align="center" :size="8">
              <span class="task-kind">{{ task.kind }}</span>
              <span class="task-name">{{ task.name }}</span>
              <NTag size="tiny" :type="statusColor(task.status.phase)">
                {{ statusLabel(task.status.phase) }}
              </NTag>
            </NSpace>
            <NSpace :size="8">
              <span class="task-duration">
                {{ getDuration(task.started_at, task.completed_at) }}
              </span>
              <NPopconfirm
                v-if="task.status.phase === 'running' || task.status.phase === 'queued'"
                :on-positive-click="() => onCancelTask(task.id)"
                positive-text="确认取消"
                negative-text="保留"
              >
                <template #trigger>
                  <NButton size="tiny" type="warning" ghost>取消</NButton>
                </template>
                确认取消任务「{{ task.name }}」？<br />
                正在执行的子进程将被终止。
              </NPopconfirm>
            </NSpace>
          </div>

          <div class="task-progress">
            <NProgress
              type="line"
              :percentage="getProgress(task.status)"
              :status="
                task.status.phase === 'running' ? 'default'
                : task.status.phase === 'completed' ? 'success'
                : task.status.phase === 'failed' ? 'error'
                : 'warning'
              "
              :show-indicator="true"
              :height="8"
            />
          </div>
        </div>
      </div>
    </NCard>

    <!-- 最近完成（来自 taskStore，当前会话内） -->
    <NCard v-if="recentFinishedTasks.length > 0" :bordered="true" size="small" class="section-card">
      <template #header>
        <div class="card-header">
          <span class="header-title">✓ 本次会话完成</span>
          <NTag size="small">{{ recentFinishedTasks.length }}</NTag>
        </div>
      </template>

      <div class="task-list">
        <div
          v-for="task in recentFinishedTasks"
          :key="task.id"
          class="history-row"
        >
          <div class="history-row-main">
            <NSpace align="center" :size="8">
              <span class="task-kind">{{ task.kind }}</span>
              <span class="task-name">{{ task.name }}</span>
              <NTag size="tiny" :type="statusColor(task.status.phase)">
                {{ statusLabel(task.status.phase) }}
              </NTag>
            </NSpace>
            <NSpace :size="8" align="center">
              <span class="task-meta">
                {{ getDuration(task.started_at, task.completed_at) }}
              </span>
              <span class="task-meta">
                {{ getRelativeTime(task.completed_at) }}
              </span>
              <NButton
                v-if="task.status.phase === 'failed'"
                size="tiny"
                ghost
                @click="toggleErrorDetail(task.id)"
              >
                {{ expandedErrorIds.has(task.id) ? '收起' : '错误详情' }}
              </NButton>
            </NSpace>
          </div>

          <div
            v-if="task.status.phase === 'failed' && expandedErrorIds.has(task.id)"
            class="error-detail"
          >
            <div class="error-text">
              <pre>{{ task.status.error }}</pre>
            </div>
            <div class="error-actions">
              <NButton size="tiny" @click="copyError(task.status.error)">
                复制错误
              </NButton>
            </div>
          </div>
        </div>
      </div>
    </NCard>

    <!-- 历史任务（来自 LogStore task_history 表） -->
    <NCard :bordered="true" size="small" class="section-card">
      <template #header>
        <div class="card-header">
          <span class="header-title">📜 历史任务</span>
          <NTag size="small">{{ historyTasks.length }} / 50</NTag>
        </div>
      </template>

      <NEmpty
        v-if="historyTasks.length === 0"
        description="暂无历史任务记录"
        size="small"
      />
      <NCollapse v-else arrow-placement="left">
        <NCollapseItem
          v-for="task in historyTasks.slice(0, 50)"
          :key="task.id"
          :name="String(task.id)"
        >
          <template #header>
            <div class="history-row-header">
              <NSpace align="center" :size="8">
                <span class="task-kind">{{ task.kind }}</span>
                <span class="task-name">{{ task.name }}</span>
                <NTag size="tiny" :type="historyStatusColor(task.status)">
                  {{ task.status }}
                </NTag>
              </NSpace>
              <span class="task-meta">
                {{ getRelativeTime(task.completed_at) }}
              </span>
            </div>
          </template>

          <NDescriptions :column="2" size="small" label-placement="left" bordered>
            <NDescriptionsItem label="任务 ID">
              #{{ task.id }}
            </NDescriptionsItem>
            <NDescriptionsItem label="类型">
              {{ task.kind }}
            </NDescriptionsItem>
            <NDescriptionsItem label="开始时间">
              {{ task.started_at }}
            </NDescriptionsItem>
            <NDescriptionsItem label="完成时间">
              {{ task.completed_at ?? "—" }}
            </NDescriptionsItem>
            <NDescriptionsItem label="退出码">
              {{ task.exit_code ?? "—" }}
            </NDescriptionsItem>
            <NDescriptionsItem label="耗时">
              {{ getDuration(task.started_at, task.completed_at) }}
            </NDescriptionsItem>
            <NDescriptionsItem v-if="task.error" label="错误信息" :span="2">
              <!-- v3.10：错误信息过长时默认折叠 + 截断显示 -->
              <details v-if="task.error.length > 200" class="error-detail-collapsible">
                <summary class="error-summary">
                  <span class="error-preview">{{ truncateError(task.error, 200) }}</span>
                  <NButton text size="tiny">展开</NButton>
                </summary>
                <pre class="error-pre">{{ task.error }}</pre>
              </details>
              <pre v-else class="error-pre">{{ task.error }}</pre>
            </NDescriptionsItem>
          </NDescriptions>

          <div v-if="task.error" class="error-actions">
            <NButton size="tiny" @click="copyError(task.error ?? '')">
              复制错误
            </NButton>
          </div>
        </NCollapseItem>
      </NCollapse>
    </NCard>

    <div class="footer-tip">
      <NTooltip placement="top">
        <template #trigger>
          <span class="tip-icon">ℹ</span>
        </template>
        历史任务持久化到 SQLite，保留最近 50 条
      </NTooltip>
    </div>
  </div>
</template>

<style scoped>
.task-center {
  padding: 16px;
  max-width: 1400px;
  margin: 0 auto;
}

.header-card {
  margin-bottom: 12px;
}

.header-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  flex-wrap: wrap;
  gap: 12px;
}

.stats {
  display: flex;
  gap: 24px;
}

.section-card {
  margin-bottom: 12px;
}

.card-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
}

.header-title {
  font-weight: 600;
}

.task-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.task-row {
  padding: 12px;
  border-radius: 6px;
  background: var(--app-bg-subtle, rgba(0, 0, 0, 0.02));
  border-left: 3px solid transparent;
}

.task-row.task-running {
  border-left-color: #1890ff;
}

.task-row.task-queued {
  border-left-color: #999;
}

.task-row-header,
.history-row-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  flex-wrap: wrap;
  gap: 8px;
  margin-bottom: 8px;
}

.task-kind {
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
  font-size: 12px;
  color: #1890ff;
  background: rgba(24, 144, 255, 0.1);
  padding: 1px 6px;
  border-radius: 3px;
}

.task-name {
  font-weight: 500;
}

.task-duration,
.task-meta {
  font-size: 12px;
  color: var(--app-text-muted, #999);
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
}

.task-progress {
  margin-top: 4px;
}

.history-row {
  padding: 10px 12px;
  border-radius: 6px;
  background: var(--app-bg-subtle, rgba(0, 0, 0, 0.02));
  border-left: 3px solid transparent;
}

.history-row-main {
  display: flex;
  justify-content: space-between;
  align-items: center;
  flex-wrap: wrap;
  gap: 8px;
}

.error-detail {
  margin-top: 8px;
  padding: 8px;
  background: rgba(255, 68, 68, 0.06);
  border-radius: 4px;
}

.error-text pre,
.error-pre {
  margin: 0;
  padding: 0;
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
  font-size: 12px;
  white-space: pre-wrap;
  word-break: break-all;
  color: #ff4444;
}

.error-pre {
  padding: 8px;
  background: rgba(255, 68, 68, 0.06);
  border-radius: 4px;
  max-height: 240px;
  overflow-y: auto;
}

.error-actions {
  margin-top: 8px;
  display: flex;
  justify-content: flex-end;
}

/* v3.10：错误信息折叠样式 */
.error-detail-collapsible {
  margin-top: 4px;
}

.error-detail-collapsible summary {
  cursor: pointer;
  padding: 8px;
  background: #fafafa;
  border: 1px solid #e0e0e0;
  border-radius: 4px;
  user-select: none;
  list-style: none;
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
}

.error-detail-collapsible summary::-webkit-details-marker {
  display: none;
}

.error-preview {
  flex: 1;
  font-family: monospace;
  font-size: 12px;
  color: #666;
  white-space: pre-wrap;
  word-break: break-all;
  max-height: 80px;
  overflow: hidden;
  text-overflow: ellipsis;
}

.error-detail-collapsible[open] summary {
  margin-bottom: 8px;
}

.error-detail-collapsible[open] .error-preview {
  max-height: none;
  overflow: visible;
}

.footer-tip {
  text-align: center;
  padding: 12px 0;
  color: var(--app-text-muted, #999);
  font-size: 12px;
}

.tip-icon {
  cursor: help;
  padding: 2px 8px;
  border-radius: 50%;
  background: rgba(127, 127, 127, 0.15);
}
</style>
