<script setup lang="ts">
/**
 * 日志页
 *
 * 详见 `PR/06-界面设计.md §5.4 日志页`
 *
 * 区块：
 * 1. 顶部工具栏：过滤（级别）+ 搜索框 + [清空] [导出]
 * 2. 实时流式日志区（等宽字体）
 * 3. 自动滚动到底部开关
 *
 * 行为：
 * - 实时日志来自 processStore.logBuffer（"log" 事件）
 * - 历史日志通过 logQuery API 查询
 * - 级别着色：ERROR 红 / WARN 黄 / INFO 蓝 / DEBUG 灰
 * - 搜索匹配高亮，非匹配变灰
 *
 * 设计模式：
 * - **Observer**：订阅 processStore.logBuffer 变化
 * - **Strategy**：不同 LogLevel 不同着色
 */

import { ref, computed, watch, nextTick, onMounted } from "vue";
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
} from "naive-ui";
import { useProcessStore } from "@/stores/process";
import { logQuery, logClear } from "@/api/log";
import { useToast } from "@/composables/useToast";
import type { LogEntry, LogLevel } from "@/api/types";

const processStore = useProcessStore();
const toast = useToast();

const logContainerRef = ref<HTMLElement | null>(null);
const autoScroll = ref(true);
const searchKeyword = ref("");
const useRegex = ref(false);
const filterLevel = ref<LogLevel | "all">("all");
const historyLogs = ref<LogEntry[]>([]);

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
    if (useRegex.value) return new RegExp(q).test(line);
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

onMounted(async () => {
  // 加载历史日志
  try {
    historyLogs.value = await logQuery({ limit: 200 });
  } catch (e) {
    console.warn("log query:", e);
  }
  // 加载进程日志缓冲
  try {
    await processStore.loadHistoryLogs(200);
  } catch (e) {
    console.warn("history logs:", e);
  }
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
  a.download = `mycomfyui-logs-${new Date().toISOString().replace(/[:.]/g, "-")}.txt`;
  a.click();
  URL.revokeObjectURL(url);
  toast.success("已导出日志文件");
}

async function onRefresh() {
  try {
    historyLogs.value = await logQuery({ limit: 200 });
    await processStore.loadHistoryLogs(200);
    toast.success("日志已刷新");
  } catch (e) {
    toast.error("刷新失败", e);
  }
}
</script>

<template>
  <div class="logs-page">
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
  </div>
</template>

<style scoped>
.logs-page {
  padding: 16px;
  max-width: 1400px;
  margin: 0 auto;
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
  height: calc(100vh - 220px);
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
</style>
