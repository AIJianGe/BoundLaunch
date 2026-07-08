<script setup lang="ts">
/**
 * RunningLogsTab.vue — ComfyUI 运行日志 Tab
 *
 * 数据源：
 * - 实时：processStore.logBuffer（comfyui_log 事件推送，纯文本）
 * - 历史：logQuery({ limit: 200 }) → historyLogs（结构化 LogEntry）
 *
 * 功能：
 * - 级别过滤（all/error/warn/info/debug）
 * - 关键词搜索（支持正则）
 * - 自动滚动
 * - 清空 + 导出
 * - 空状态提示
 *
 * 颜色规则（行级）：
 * - ERROR → 红色
 * - WARN → 黄色
 * - INFO → 蓝色
 * - DEBUG → 灰色
 * - 其它 → 普通
 *
 * 搜索高亮：未匹配的搜索行 opacity 0.3
 */

import { ref, computed, watch, nextTick, onMounted } from "vue";
import {
  NCard,
  NEmpty,
  NTag,
  NSelect,
  NInput,
  NCheckbox,
  NButton,
  NPopconfirm,
} from "naive-ui";
import { useProcessStore } from "@/stores/process";
import { useToast } from "@/composables/useToast";
import { logQuery, logClear } from "@/api/log";
import type { LogEntry, LogLevel } from "@/api/types";

const processStore = useProcessStore();
const toast = useToast();

const logContainerRef = ref<HTMLElement | null>(null);
const autoScroll = ref(true);
const searchKeyword = ref("");
const useRegex = ref(false);
const filterLevel = ref<LogLevel | "all">("all");
const historyLogs = ref<LogEntry[]>([]);

// ============================================================================
// 级别选项
// ============================================================================

const levelOptions = [
  { label: "全部", value: "all" },
  { label: "ERROR", value: "error" },
  { label: "WARN", value: "warn" },
  { label: "INFO", value: "info" },
  { label: "DEBUG", value: "debug" },
];

// ============================================================================
// 日志合并 + 过滤
// ============================================================================

function formatLogEntry(entry: LogEntry): string {
  const ts = entry.timestamp.split("T")[1]?.split(".")[0] || entry.timestamp;
  return `${ts}  ${entry.level.toUpperCase().padEnd(5)}  [${entry.source}]  ${entry.message}`;
}

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

// ============================================================================
// 行级颜色识别
// ============================================================================

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
    if (useRegex.value) return new RegExp(q).test(line);
    return line.toLowerCase().includes(q.toLowerCase());
  } catch {
    return true;
  }
};

// ============================================================================
// 自动滚动
// ============================================================================

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

// ============================================================================
// 操作
// ============================================================================

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
  a.download = `boundlaunch-running-logs-${new Date().toISOString().replace(/[:.]/g, "-")}.txt`;
  a.click();
  URL.revokeObjectURL(url);
  toast.success("已导出日志文件");
}

async function onRefresh() {
  try {
    historyLogs.value = await logQuery({ limit: 200 });
    await processStore.loadHistoryLogs(200, false);
    toast.success("日志已刷新");
  } catch (e) {
    toast.error("刷新失败", e);
  }
}

// ============================================================================
// 生命周期
// ============================================================================

onMounted(async () => {
  // 首次加载历史日志
  try {
    historyLogs.value = await logQuery({ limit: 200 });
  } catch (e) {
    console.warn("[RunningLogsTab] load history logs failed:", e);
  }
});
</script>

<template>
  <div class="running-logs-tab">
    <!-- 工具栏 -->
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

    <!-- 日志区 -->
    <NCard :bordered="true" size="small" class="log-card">
      <template #header>
        <div class="card-header">
          <span class="header-title">📜 ComfyUI 运行日志</span>
          <NTag size="small">
            {{ filteredLogs.length }} 行
            <span v-if="filterLevel !== 'all' || searchKeyword" class="filtered-hint">
              （已过滤）
            </span>
          </NTag>
        </div>
      </template>

      <div v-if="filteredLogs.length === 0" class="empty-state">
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
  </div>
</template>

<style scoped>
.running-logs-tab {
  display: flex;
  flex-direction: column;
  gap: 12px;
  flex: 1;
  min-height: 0;
}

/* 工具栏 */
.toolbar {
  flex-shrink: 0;
}

.toolbar-row {
  display: flex;
  gap: 8px;
  align-items: center;
}

.filter-select {
  width: 110px;
}

.search-input {
  flex: 1;
  max-width: 360px;
}

/* 日志卡片 */
.log-card {
  display: flex;
  flex-direction: column;
  flex: 1;
  min-height: 0;
}

.log-card :deep(.n-card__content) {
  display: flex;
  flex-direction: column;
  flex: 1;
  min-height: 0;
  padding: 12px;
}

.card-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.header-title {
  font-weight: 600;
  font-size: 14px;
}

.filtered-hint {
  margin-left: 6px;
  color: #f0a020;
  font-size: 12px;
}

.empty-state {
  display: flex;
  align-items: center;
  justify-content: center;
  min-height: 200px;
}

.log-container {
  flex: 1;
  overflow-y: auto;
  background: #1e1e1e;
  color: #d4d4d4;
  padding: 8px 12px;
  border-radius: 4px;
  font-family: "Cascadia Code", "Consolas", monospace;
  font-size: 12px;
  line-height: 1.5;
  min-height: 0;
}

.log-line {
  white-space: pre-wrap;
  word-break: break-all;
}

.log-line.log-error {
  color: #f14c4c;
}

.log-line.log-warn {
  color: #f5f543;
}

.log-line.log-info {
  color: #3b8eea;
}

.log-line.log-debug {
  color: #888;
}
</style>
