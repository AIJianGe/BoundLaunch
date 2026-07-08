<script setup lang="ts">
/**
 * InstallLogsTab.vue — 安装 / 环境日志 Tab
 *
 * 数据源：
 * - 历史：logQuery({ limit: 500 }) → 客户端按 source 前缀过滤（"ui:" = 安装/业务日志）
 * - 实时：监听 `business_log` 事件，追加到列表
 *
 * 安装日志涵盖：
 * - venv 创建/重建
 * - torch / torchvision 安装
 * - requirements.txt 安装
 * - ComfyUI 克隆 / 版本切换
 * - 插件安装
 * - 启动前环境检查
 * - 任何通过 useToast.error/warn 输出的业务日志
 *
 * 与 RunningLogsTab 的区别：
 * - RunningLogsTab：ComfyUI 进程 stdout/stderr（comfyui:stdout / comfyui:stderr）
 * - InstallLogsTab：应用层日志（ui:*），与 ComfyUI 是否运行无关
 *
 * 阶段高亮：
 * - venv  → 蓝色
 * - torch → 紫色
 * - requirements → 绿色
 * - clone/checkout → 橙色
 * - plugin → 青色
 * - 错误 → 红色
 */

import { ref, computed, watch, nextTick, onMounted, onUnmounted } from "vue";
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
import { useToast } from "@/composables/useToast";
import { useErrorLog } from "@/composables/useErrorLog";
import { logQuery, logClear } from "@/api/log";
import { listen, type UnlistenFn } from "@/api";
import type { LogEntry, LogLevel, BusinessLogEvent } from "@/api/types";

const toast = useToast();
const errorLog = useErrorLog();

const logContainerRef = ref<HTMLElement | null>(null);
const autoScroll = ref(true);
const searchKeyword = ref("");
const useRegex = ref(false);
const filterLevel = ref<LogLevel | "all">("all");
const filterSource = ref<string>("all");
const historyLogs = ref<LogEntry[]>([]);
const liveLogs = ref<LogEntry[]>([]);

let unlistenBusinessLog: UnlistenFn | null = null;
let nextLogId = -1; // 实时日志用负数 ID（避免和数据库 id 冲突）

// ============================================================================
// Source 选项（安装/环境相关 source 分类）
// ============================================================================

const sourceOptions = [
  { label: "全部阶段", value: "all" },
  { label: "Python venv", value: "ui:venv" },
  { label: "PyTorch", value: "ui:torch" },
  { label: "依赖安装", value: "ui:requirements" },
  { label: "ComfyUI 克隆/切换", value: "ui:version" },
  { label: "插件安装", value: "ui:plugin" },
  { label: "环境检查", value: "ui:env" },
  { label: "进程启动", value: "ui:launcher" },
  { label: "其他业务", value: "ui:other" },
];

// ============================================================================
// 阶段颜色（按 source 自动分色）
// ============================================================================

function getSourceClass(source: string): string {
  if (source.includes("venv")) return "stage-venv";
  if (source.includes("torch")) return "stage-torch";
  if (source.includes("requirements") || source.includes("deps")) return "stage-deps";
  if (source.includes("version") || source.includes("clone") || source.includes("checkout")) return "stage-version";
  if (source.includes("plugin")) return "stage-plugin";
  if (source.includes("env")) return "stage-env";
  if (source.includes("launcher")) return "stage-launcher";
  return "stage-default";
}

// ============================================================================
// 日志合并 + 过滤
// ============================================================================

function formatLogEntry(entry: LogEntry): string {
  const ts = entry.timestamp.split("T")[1]?.split(".")[0] || entry.timestamp;
  return `${ts}  ${entry.level.toUpperCase().padEnd(5)}  [${entry.source}]  ${entry.message}`;
}

const allLogs = computed(() => {
  // 合并历史 + 实时，历史在前，实时在后
  return [
    ...historyLogs.value.map((e) => formatLogEntry(e)),
    ...liveLogs.value.map((e) => formatLogEntry(e)),
  ];
});

/** 应用过滤 */
const filteredLogs = computed(() => {
  let logs = allLogs.value;

  // 1. 阶段过滤（按 source 前缀）
  if (filterSource.value !== "all") {
    // "ui:venv" → 匹配 source 包含 "venv"
    // "ui:torch" → 匹配 source 包含 "torch"
    const filter = filterSource.value;
    logs = logs.filter((line) => {
      // 提取 source（line 格式: "HH:MM:SS  LEVEL  [source]  message"）
      const match = line.match(/\[[^\]]+\]/);
      if (!match) return false;
      const source = match[0].slice(1, -1).toLowerCase();
      // 映射关系
      if (filter === "ui:venv") return source.includes("venv");
      if (filter === "ui:torch") return source.includes("torch");
      if (filter === "ui:requirements") return source.includes("requirement") || source.includes("deps");
      if (filter === "ui:version") return source.includes("version") || source.includes("clone") || source.includes("checkout");
      if (filter === "ui:plugin") return source.includes("plugin");
      if (filter === "ui:env") return source.includes("env");
      if (filter === "ui:launcher") return source.includes("launcher") || source.includes("process");
      if (filter === "ui:other") return source.startsWith("ui:") && !["venv", "torch", "requirement", "deps", "version", "clone", "checkout", "plugin", "env", "launcher", "process"].some((k) => source.includes(k));
      return true;
    });
  }

  // 2. 级别过滤
  if (filterLevel.value !== "all") {
    const level = filterLevel.value.toUpperCase();
    logs = logs.filter((line) => line.includes(level));
  }

  // 3. 关键词搜索
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

/** 提取 source 用于行级 class（不过滤也能上色） */
function getRowSource(line: string): string {
  const match = line.match(/\[[^\]]+\]/);
  return match?.[0]?.slice(1, -1) ?? "";
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
    historyLogs.value = [];
    liveLogs.value = [];
    toast.success("安装日志已清空");
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
  a.download = `boundlaunch-install-logs-${new Date().toISOString().replace(/[:.]/g, "-")}.txt`;
  a.click();
  URL.revokeObjectURL(url);
  toast.success("已导出安装日志");
}

async function onRefresh() {
  try {
    historyLogs.value = await logQuery({ limit: 500 });
    toast.success("安装日志已刷新");
  } catch (e) {
    toast.error("刷新失败", e);
  }
}

// ============================================================================
// 实时业务日志监听
// ============================================================================

async function setupListeners() {
  unlistenBusinessLog = await listen<BusinessLogEvent>("business_log", (event) => {
    const payload = event.payload;
    // 只接收 ui:* 前缀的（安装/业务日志）
    if (!payload.source || !payload.source.startsWith("ui:")) return;

    const entry: LogEntry = {
      id: nextLogId--,
      timestamp: payload.ts || new Date().toISOString(),
      level: payload.level,
      source: payload.source,
      message: payload.message,
    };
    liveLogs.value.push(entry);
  });
}

// ============================================================================
// 生命周期
// ============================================================================

onMounted(async () => {
  // 首次加载：拉全量历史日志，然后客户端按 source 前缀过滤
  try {
    const all = await logQuery({ limit: 500 });
    historyLogs.value = all.filter((e) => e.source.startsWith("ui:"));
  } catch (e) {
    console.warn("[InstallLogsTab] load history logs failed:", e);
  }
  await setupListeners();
});

onUnmounted(() => {
  unlistenBusinessLog?.();
});
</script>

<template>
  <div class="install-logs-tab">
    <!-- 工具栏 -->
    <NCard class="toolbar" :bordered="true" size="small">
      <div class="toolbar-row">
        <NSelect
          v-model:value="filterSource"
          :options="sourceOptions"
          size="small"
          class="filter-select"
          placeholder="选择阶段"
        />
        <NSelect
          v-model:value="filterLevel"
          :options="[
            { label: '全部', value: 'all' },
            { label: 'ERROR', value: 'error' },
            { label: 'WARN', value: 'warn' },
            { label: 'INFO', value: 'info' },
            { label: 'DEBUG', value: 'debug' },
          ]"
          size="small"
          class="filter-level"
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
          确认清空所有安装日志？此操作不可恢复
        </NPopconfirm>
        <NButton size="small" @click="onExport">导出</NButton>
      </div>
    </NCard>

    <!-- 日志区 -->
    <NCard :bordered="true" size="small" class="log-card">
      <template #header>
        <div class="card-header">
          <span class="header-title">🛠 安装/环境日志</span>
          <NTag size="small">
            {{ filteredLogs.length }} 行
            <span
              v-if="filterSource !== 'all' || filterLevel !== 'all' || searchKeyword"
              class="filtered-hint"
            >
              （已过滤）
            </span>
          </NTag>
        </div>
      </template>

      <div v-if="filteredLogs.length === 0" class="empty-state">
        <NEmpty
          v-if="historyLogs.length === 0 && liveLogs.length === 0"
          description="暂无安装日志（启动 ComfyUI 或进行环境操作后会显示）"
          size="small"
        />
        <NEmpty v-else description="暂无匹配日志" size="small" />
      </div>

      <div v-else ref="logContainerRef" class="log-container">
        <div
          v-for="(line, idx) in filteredLogs"
          :key="idx"
          class="log-line"
          :class="[
            `log-${getLogLevel(line)}`,
            getSourceClass(getRowSource(line)),
          ]"
          :style="{ opacity: isMatched(line) ? 1 : 0.3 }"
        >
          {{ line }}
        </div>
      </div>
    </NCard>
  </div>
</template>

<style scoped>
.install-logs-tab {
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
  flex-wrap: wrap;
}

.filter-select {
  width: 160px;
}

.filter-level {
  width: 100px;
}

.search-input {
  flex: 1;
  max-width: 360px;
  min-width: 200px;
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

/* 阶段颜色（在 level 颜色之上叠加边框/背景标识） */
.log-line.stage-venv {
  border-left: 2px solid #2472c8;
  padding-left: 4px;
}

.log-line.stage-torch {
  border-left: 2px solid #bc3fbc;
  padding-left: 4px;
}

.log-line.stage-deps {
  border-left: 2px solid #0dbc79;
  padding-left: 4px;
}

.log-line.stage-version {
  border-left: 2px solid #e5e510;
  padding-left: 4px;
}

.log-line.stage-plugin {
  border-left: 2px solid #11a8cd;
  padding-left: 4px;
}

.log-line.stage-env {
  border-left: 2px solid #cd3131;
  padding-left: 4px;
}

.log-line.stage-launcher {
  border-left: 2px solid #f14c4c;
  padding-left: 4px;
}

/* level 颜色（按行级内容） */
.log-line.log-error {
  color: #f14c4c;
  font-weight: 600;
}

.log-line.log-warn {
  color: #f5f543;
}

.log-line.log-info {
  color: #d4d4d4;
}

.log-line.log-debug {
  color: #888;
}
</style>
