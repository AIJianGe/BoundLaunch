<script setup lang="ts">
/**
 * 日志页
 *
 * 详见 `PR/06-界面设计.md §5.4 日志页`
 *
 * 区块：
 * 1. 顶部工具栏：过滤（级别）+ 搜索框 + [清空] [导出]
 * 2. v3.4 新增：ComfyUI 启动进度条（任务进行中时显示）
 * 3. v3.4 新增：失败详情弹窗（process_crashed 事件触发）
 * 4. 实时流式日志区（等宽字体）
 * 5. 自动滚动到底部开关
 *
 * 行为：
 * - 实时日志来自 processStore.logBuffer（"log" 事件）
 * - 历史日志通过 logQuery API 查询
 * - 级别着色：ERROR 红 / WARN 黄 / INFO 蓝 / DEBUG 灰
 * - 搜索匹配高亮，非匹配变灰
 *
 * v3.4 新增：
 * - 启动任务进度条：从 taskStore 找 kind=start_comfyui 的 running 任务，订阅 task_progress 事件实时刷新
 * - 失败弹窗：processStore.crashedReason（来自后端 process_crashed 事件载荷）触发 NModal
 *
 * 设计模式：
 * - **Observer**：订阅 processStore.logBuffer / taskStore.tasks / processStore.crashedReason 变化
 * - **Strategy**：不同 LogLevel 不同着色
 */

import { ref, computed, watch, nextTick, onMounted, onUnmounted } from "vue";
import {
  NCard,
  NSpace,
  NSelect,
  NInput,
  NCheckbox,
  NButton,
  NTag,
  NEmpty,
  NTooltip,
  NPopconfirm,
  NProgress,
  NModal,
  NText,
} from "naive-ui";
import { useProcessStore } from "@/stores/process";
import { useTaskStore } from "@/stores/task";
import { logQuery, logClear } from "@/api/log";
import { listen, type UnlistenFn } from "@/api";
import { useToast } from "@/composables/useToast";
import { useErrorLog } from "@/composables/useErrorLog";
import type { LogEntry, LogLevel, TaskProgressEvent, TaskTerminalEvent } from "@/api/types";

const processStore = useProcessStore();
const taskStore = useTaskStore();
const toast = useToast();
// v3.10：业务错误 store（ErrorPanel + 菜单红点）
const errorLog = useErrorLog();

const logContainerRef = ref<HTMLElement | null>(null);
const autoScroll = ref(true);
const searchKeyword = ref("");
const useRegex = ref(false);
const filterLevel = ref<LogLevel | "all">("all");
const historyLogs = ref<LogEntry[]>([]);

// v3.4.2：启动任务追踪（仅追踪 task_completed 事件，不再依赖 0-100% 进度条）
const startTaskActive = ref(false);
const startTaskMessage = ref<string | null>(null);
let trackedStartTaskId: string | null = null;
const taskUnlisteners: UnlistenFn[] = [];

// v3.4.2：启动耗时计时器（task 开始时启动，task 结束时停止）
const startElapsedSec = ref(0);
let startTimerHandle: number | null = null;

// v3.4：失败详情弹窗（从 processStore.crashedReason 同步）
const showCrashModal = computed(() => processStore.crashedReason !== null);

/** 关闭失败弹窗 */
function dismissCrash() {
  processStore.dismissCrashed();
}

const levelOptions = [
  { label: "全部", value: "all" },
  { label: "ERROR", value: "error" },
  { label: "WARN", value: "warn" },
  { label: "INFO", value: "info" },
  { label: "DEBUG", value: "debug" },
];

/** 合并实时日志与历史日志 */
const allLogs = computed(() => {
  // 历史日志在前，实时日志在后
  return [...historyLogs.value.map((e) => formatLogEntry(e)), ...processStore.logBuffer];
});

/** 应用级别过滤 + 搜索匹配 */
const filteredLogs = computed(() => {
  let logs = allLogs.value;
  if (filterLevel.value !== "all") {
    const level = filterLevel.value.toUpperCase();
    logs = logs.filter((line) => line.includes(level));
  }
  if (searchKeyword.value.trim()) {
    const q = searchKeyword.value;
    try {
      if (useRegex.value) {
        const re = new RegExp(q);
        logs = logs.filter((line) => re.test(line));
      } else {
        const lower = q.toLowerCase();
        logs = logs.filter((line) => line.toLowerCase().includes(lower));
      }
    } catch (e) {
      // 正则无效，忽略过滤
    }
  }
  return logs;
});

function formatLogEntry(entry: LogEntry): string {
  const ts = entry.timestamp.split("T")[1]?.split(".")[0] || entry.timestamp;
  return `${ts}  ${entry.level.toUpperCase().padEnd(5)}  [${entry.source}]  ${entry.message}`;
}

function getLogLevel(line: string): LogLevel | "plain" {
  if (/\bERROR\b|\bError\b|Traceback|Exception/i.test(line)) return "error";
  if (/\bWARN\b|\bWarning\b/i.test(line)) return "warn";
  if (/\bINFO\b/i.test(line)) return "info";
  if (/\bDEBUG\b/i.test(line)) return "debug";
  return "plain";
}

const isMatched = (line: string): boolean => {
  if (!searchKeyword.value.trim()) return true;
  const q = searchKeyword.value;
  try {
    if (useRegex.value) return new RegExp(q).test(q);
    return line.toLowerCase().includes(q.toLowerCase());
  } catch {
    return true;
  }
};

// 自动滚动到底部
watch(
  () => filteredLogs.value.length,
  async () => {
    if (autoScroll.value) {
      await nextTick();
      if (logContainerRef.value) {
        logContainerRef.value.scrollTop = logContainerRef.value.scrollHeight;
      }
    }
  },
);

/**
 * v3.4：找到当前 start_comfyui 任务的 id
 *
 * 在 task_queued 事件后会被调用；后续 task_progress 事件就用这个 id 过滤
 */
function findActiveStartTask(): string | null {
  const running = taskStore.tasks.find(
    (t) => t.kind === "start_comfyui" && t.status.phase === "running",
  );
  return running?.id ?? null;
}

/** v3.4.2：订阅 task 事件，匹配 start_comfyui 任务（仅追踪 active 状态，不再依赖进度） */
async function setupStartTaskListeners() {
  taskUnlisteners.push(
    await listen<TaskInfoLite>("task_queued", (e) => {
      if (e.payload.kind === "start_comfyui") {
        trackedStartTaskId = e.payload.id;
        startTaskActive.value = true;
        startTaskMessage.value = "排队中...";
        startElapsedTimer();
      }
    }),
    await listen<TaskProgressEvent>("task_progress", (e) => {
      if (e.payload.task_id === trackedStartTaskId) {
        // v3.4.2：仅更新阶段消息，不再依赖进度数值
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

/** v3.4.2：启动/重置启动耗时计时器（每秒 +1） */
function startElapsedTimer() {
  stopElapsedTimer();
  startElapsedSec.value = 0;
  startTimerHandle = window.setInterval(() => {
    startElapsedSec.value += 1;
  }, 1000);
}

/** v3.4.2：停止启动耗时计时器 */
function stopElapsedTimer() {
  if (startTimerHandle !== null) {
    clearInterval(startTimerHandle);
    startTimerHandle = null;
  }
}

/** v3.4.2：格式化耗时（秒 → "X分Y秒"） */
function formatElapsed(sec: number): string {
  const m = Math.floor(sec / 60);
  const s = sec % 60;
  return m > 0 ? `${m}分${s}秒` : `${s}秒`;
}

/** v3.10：格式化错误时间（ISO 8601 → "HH:MM:SS"） */
function formatErrorTime(iso: string): string {
  return iso.split("T")[1]?.split(".")[0] || iso;
}

/** v3.10：刷新 ErrorPanel 历史（重新拉 LogStore 一次） */
async function onRefreshErrors() {
  // 重新初始化：把 initialized 重置，调 loadHistory
  errorLog.$patch({ initialized: false });
  await errorLog.loadHistory();
  toast.success("已刷新最近错误");
}

/** task_queued 事件 payload 的最小子集（实际有更多字段） */
interface TaskInfoLite {
  id: string;
  kind: string;
}

onMounted(async () => {
  // 订阅 task_progress / task_completed 事件
  await setupStartTaskListeners();

  // v3.10：用户进入日志页 → 清零未读（菜单红点消失）
  errorLog.markAllRead();

  // 加载历史日志
  try {
    historyLogs.value = await logQuery({ limit: 200 });
  } catch (e) {
    console.warn("log query:", e);
  }
  // 加载进程日志缓冲（v3.4.2：append 模式，避免覆盖 in-memory 已有的日志）
  try {
    await processStore.loadHistoryLogs(200, true);
  } catch (e) {
    console.warn("history logs:", e);
  }

  // 如果已有 start_comfyui 任务在跑，标记追踪（用户从其他页面切过来时）
  try {
    await taskStore.load();
    const existing = findActiveStartTask();
    if (existing) {
      trackedStartTaskId = existing;
      startTaskActive.value = true;
      const t = taskStore.tasks.find((t) => t.id === existing);
      if (t && t.status.phase === "running") {
        // v3.4.2：task status 没有 message 字段，从 startTaskMessage 默认值即可
        startTaskMessage.value = "进行中...";
      }
      // v3.4.2：恢复耗时计时器（从 task 创建时间算起）
      startElapsedTimer();
    }
  } catch (e) {
    console.warn("task load:", e);
  }
});

onUnmounted(() => {
  taskUnlisteners.forEach((un) => un());
  taskUnlisteners.length = 0;
  // v3.4.2：清理计时器
  stopElapsedTimer();
});

async function onClearLogs() {
  try {
    await logClear();
    processStore.clearLogs();
    historyLogs.value = [];
    toast.success("日志已清空");
  } catch (e) {
    toast.error("清空失败", e);
  }
}

function onExport() {
  const content = filteredLogs.value.join("\n");
  const blob = new Blob([content], { type: "text/plain;charset=utf-8" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = `boundlaunch-logs-${new Date().toISOString().replace(/[:.]/g, "-")}.txt`;
  a.click();
  URL.revokeObjectURL(url);
  toast.success("已导出日志文件");
}

async function onRefresh() {
  try {
    // v3.4.2：刷新 = 覆盖模式（强制拉最新 200 行）
    historyLogs.value = await logQuery({ limit: 200 });
    await processStore.loadHistoryLogs(200, false);
    toast.success("日志已刷新");
  } catch (e) {
    toast.error("刷新失败", e);
  }
}

/** v3.4：失败原因格式化 */
function formatCrashReason(reason: string): string {
  switch (reason) {
    case "early_exit":
      return "早期退出（spawn 后 5s 内崩溃）";
    case "health_check_detected":
      return "健康检查发现崩溃（5s~60s 之间）";
    case "monitor_detected":
      return "运行中崩溃（monitor 检测到退出）";
    default:
      return reason;
  }
}
</script>

<template>
  <div class="logs-page">
    <!-- v3.4.2：启动加载提示（task 跟踪中显示，无进度条 → 用户不再被"卡在 X%"误导） -->
    <NCard v-if="startTaskActive" class="start-progress" :bordered="true" size="small">
      <NSpace vertical>
        <div class="start-progress-header">
          <span class="start-progress-title">
            <NSpin size="small" class="start-spin" />
            🚀 ComfyUI 启动中...
          </span>
          <NTag size="small" type="info">已等待 {{ formatElapsed(startElapsedSec) }}</NTag>
        </div>
        <div class="start-progress-message">
          {{ startTaskMessage || "准备中..." }}
        </div>
        <NText depth="3" class="start-progress-hint">
          ⏳ ComfyUI 加载时间取决于机器性能与模型大小，请耐心等待。
        </NText>
      </NSpace>
    </NCard>

    <!-- v3.10：业务错误面板（顶部置顶，不会消失，永远可回溯） -->
    <NCard v-if="errorLog.hasErrors" class="error-panel" :bordered="true" size="small">
      <template #header>
        <div class="error-panel-header">
          <span class="error-panel-title">
            ⚠ 最近错误（{{ errorLog.recentErrors.length }}）
          </span>
          <NTag size="small" type="error">置顶</NTag>
        </div>
      </template>
      <NSpace vertical size="small">
        <div
          v-for="(err, idx) in errorLog.displayErrors"
          :key="err.ts + idx"
          class="error-item"
        >
          <div class="error-item-header">
            <NTag size="small" :type="err.level === 'error' ? 'error' : 'warning'">
              {{ err.level.toUpperCase() }}
            </NTag>
            <span class="error-item-time">{{ formatErrorTime(err.ts) }}</span>
            <span class="error-item-source">[{{ err.source }}]</span>
          </div>
          <div class="error-item-message">{{ err.message }}</div>
          <details v-if="err.detail" class="error-item-detail">
            <summary>展开详情</summary>
            <pre>{{ err.detail }}</pre>
          </details>
        </div>
        <div v-if="errorLog.recentErrors.length > 10" class="error-panel-hint">
          仅显示前 10 条，完整历史见下方日志流（已持久化到 LogStore）
        </div>
        <NSpace size="small">
          <NButton size="tiny" @click="onRefreshErrors">刷新历史</NButton>
          <NPopconfirm
            :on-positive-click="errorLog.clearDisplayed"
            positive-text="确认清空"
            negative-text="取消"
          >
            <template #trigger>
              <NButton size="tiny" type="warning" ghost>清空显示</NButton>
            </template>
            仅清空面板显示，LogStore 数据不动
          </NPopconfirm>
        </NSpace>
      </NSpace>
    </NCard>

    <NCard class="toolbar" :bordered="true" size="small">
      <div class="toolbar-row">
        <NSelect
          v-model:value="filterLevel"
          :options="levelOptions"
          size="small"
          class="filter-select"
        />
        <NInput
          v-model:value="searchKeyword"
          placeholder="搜索..."
          size="small"
          clearable
          class="search-input"
        />
        <NCheckbox v-model:checked="useRegex">正则</NCheckbox>
        <NCheckbox v-model:checked="autoScroll">自动滚动</NCheckbox>
        <NButton size="small" @click="onRefresh">刷新</NButton>
        <NPopconfirm
          :on-positive-click="onClearLogs"
          positive-text="确认清空"
          negative-text="取消"
        >
          <template #trigger>
            <NButton size="small" type="warning" ghost>清空</NButton>
          </template>
          确认清空所有日志？此操作不可恢复（task_history 表保留）
        </NPopconfirm>
        <NButton size="small" @click="onExport">导出</NButton>
      </div>
    </NCard>

    <NCard :bordered="true" size="small" class="log-card">
      <template #header>
        <div class="card-header">
          <span class="header-title">📜 日志面板</span>
          <NTag size="small">
            {{ filteredLogs.length }} 行
            <span v-if="filterLevel !== 'all' || searchKeyword" class="filtered-hint">
              （已过滤）
            </span>
          </NTag>
        </div>
      </template>

      <div
        v-if="filteredLogs.length === 0"
        class="empty-state"
      >
        <NEmpty
          v-if="!processStore.isAlive && historyLogs.length === 0"
          description="ComfyUI 未启动，无日志"
          size="small"
        />
        <NEmpty v-else description="暂无匹配日志" size="small" />
      </div>

      <div v-else ref="logContainerRef" class="log-container">
        <div
          v-for="(line, idx) in filteredLogs"
          :key="idx"
          class="log-line"
          :class="`log-${getLogLevel(line)}`"
          :style="{ opacity: isMatched(line) ? 1 : 0.3 }"
        >
          {{ line }}
        </div>
      </div>
    </NCard>

    <!-- v3.4：失败详情弹窗（来自 process_crashed 事件） -->
    <NModal
      :show="showCrashModal"
      preset="card"
      title="💥 ComfyUI 进程崩溃"
      style="max-width: 900px"
      :bordered="false"
      size="huge"
      :on-update:show="(v: boolean) => !v && dismissCrash()"
    >
      <NSpace v-if="processStore.crashedReason" vertical>
        <div class="crash-info">
          <NText strong>原因：</NText>
          <NText>{{ formatCrashReason(processStore.crashedReason.reason) }}</NText>
        </div>
        <div class="crash-info">
          <NText strong>退出码：</NText>
          <NText>{{ processStore.crashedReason.exit_code ?? "未知（被信号杀死）" }}</NText>
        </div>
        <NText depth="3">
          以下是 ComfyUI 进程崩溃前的最后日志（最多 50 行）。可全选复制后到 GitHub Issues 搜索类似错误。
        </NText>
        <pre class="crash-stderr">{{ processStore.crashedReason.stderr_tail.join("\n") || "(无 stderr 输出)" }}</pre>
      </NSpace>
    </NModal>
  </div>
</template>

<style scoped>
.logs-page {
  padding: 16px;
  max-width: 1400px;
  margin: 0 auto;
}

/* v3.10：错误面板（顶部置顶） */
.error-panel {
  margin-bottom: 12px;
  border-color: #d03050;
  background: linear-gradient(135deg, #fef0f0 0%, #ffffff 100%);
}

.error-panel-header {
  display: flex;
  align-items: center;
  gap: 8px;
}

.error-panel-title {
  font-weight: 600;
  color: #d03050;
}

.error-item {
  padding: 8px 12px;
  border-left: 3px solid #d03050;
  background: #fafafa;
  border-radius: 4px;
}

.error-item-header {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
  color: #888;
}

.error-item-time {
  font-family: monospace;
}

.error-item-source {
  font-family: monospace;
  color: #555;
}

.error-item-message {
  margin-top: 4px;
  font-size: 14px;
  color: #333;
}

.error-item-detail {
  margin-top: 4px;
  font-size: 12px;
}

.error-item-detail summary {
  cursor: pointer;
  color: #888;
  user-select: none;
}

.error-item-detail pre {
  margin-top: 4px;
  padding: 8px;
  background: #fff;
  border: 1px solid #e0e0e0;
  border-radius: 4px;
  white-space: pre-wrap;
  word-break: break-all;
  font-size: 12px;
}

.error-panel-hint {
  font-size: 12px;
  color: #888;
  font-style: italic;
}

.start-progress {
  margin-bottom: 12px;
  background: linear-gradient(135deg, #e3f2fd 0%, #ffffff 100%);
  border-color: #90caf9;
}

.start-progress-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.start-progress-title {
  font-weight: 600;
  color: #1976d2;
  display: flex;
  align-items: center;
  gap: 8px;
}

.start-spin {
  display: inline-flex;
}

.start-progress-message {
  font-size: 12px;
  color: var(--app-text-muted, #666);
  line-height: 1.4;
  word-break: break-all;
}

.start-progress-hint {
  font-size: 11px;
  margin-top: 4px;
}

.toolbar {
  margin-bottom: 12px;
}

.toolbar-row {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.filter-select {
  width: 120px;
}

.search-input {
  flex: 1;
  min-width: 200px;
}

.log-card {
  height: calc(100vh - 280px);
  min-height: 400px;
}

.card-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
}

.header-title {
  font-weight: 600;
}

.filtered-hint {
  margin-left: 6px;
  font-size: 11px;
  color: var(--app-text-muted, #999);
}

.empty-state {
  display: flex;
  align-items: center;
  justify-content: center;
  height: 100%;
}

.log-container {
  height: calc(100% - 40px);
  overflow-y: auto;
  font-family: "JetBrains Mono", "Cascadia Code", "Fira Code", Consolas, monospace;
  font-size: 12px;
  line-height: 1.5;
  background: var(--app-bg-code, rgba(0, 0, 0, 0.06));
  border-radius: 4px;
  padding: 8px;
}

.log-line {
  white-space: pre-wrap;
  word-break: break-all;
  padding: 1px 4px;
}

.log-line:hover {
  background: rgba(127, 127, 127, 0.1);
}

.log-error {
  color: #ff4444;
  background: rgba(255, 68, 68, 0.08);
}

.log-warn {
  color: #ff8c00;
  background: rgba(255, 140, 0, 0.05);
}

.log-info {
  color: #1890ff;
}

.log-debug {
  color: #999;
}

.log-plain {
  color: inherit;
}

/* v3.4：失败弹窗样式 */
.crash-info {
  display: flex;
  gap: 8px;
  align-items: center;
}

.crash-stderr {
  margin: 0;
  padding: 12px;
  background: #1e1e1e;
  color: #d4d4d4;
  border-radius: 4px;
  font-family: "Cascadia Code", "Consolas", "Menlo", monospace;
  font-size: 12px;
  line-height: 1.5;
  max-height: 500px;
  overflow: auto;
  white-space: pre-wrap;
  word-break: break-all;
  user-select: text;
}
</style>
