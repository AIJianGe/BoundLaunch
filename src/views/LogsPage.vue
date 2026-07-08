<script setup lang="ts">
/**
 * LogsPage.vue — "终端"菜单主页面
 *
 * 设计：v3.12 NTabs 容器 + 顶部跨 Tab 状态卡
 *
 * 结构：
 * ┌──────────────────────────────────────────────┐
 * │ [StartProgressCard]   ← 启动中时显示           │
 * │ [ErrorPanelCard]      ← 有错误时显示           │
 * ├──────────────────────────────────────────────┤
 * │ ┌─[日志]─[终端]─[安装日志]─────────────────┐ │
 * │ │                                         │ │
 * │ │  RunningLogsTab / TerminalTab /          │ │
 * │ │  InstallLogsTab                         │ │
 * │ │                                         │ │
 * │ └─────────────────────────────────────────┘ │
 * ├──────────────────────────────────────────────┤
 * │  [CrashModal]                                │
 * │  [PortConflictModal]                         │
 * └──────────────────────────────────────────────┘
 *
 * Tab 状态：保存在 NTabs 内部（display: none 切换，组件不销毁）
 *
 * 数据来源：
 * - RunningLogsTab：comfyui:stdout / comfyui:stderr 实时 + 历史
 * - TerminalTab：后端 pty 服务（与日志完全独立）
 * - InstallLogsTab：ui:* 业务日志（安装/环境/版本/插件）
 */

import { computed, onMounted, ref, watch } from "vue";
import { NTabs, NTabPane, NModal, NSpace, NText, NAlert } from "naive-ui";
import { useProcessStore } from "@/stores/process";
import { useErrorLog } from "@/composables/useErrorLog";
import { useErrorClassifier } from "@/composables/useErrorClassifier";
import RunningLogsTab from "@/components/launch/RunningLogsTab.vue";
import TerminalTab from "@/components/launch/TerminalTab.vue";
import InstallLogsTab from "@/components/launch/InstallLogsTab.vue";
import StartProgressCard from "@/components/launch/StartProgressCard.vue";
import ErrorPanelCard from "@/components/launch/ErrorPanelCard.vue";
import PortConflictModal from "@/components/launch/PortConflictModal.vue";

const processStore = useProcessStore();
const errorLog = useErrorLog();
const { classify: classifyError } = useErrorClassifier();

// ============================================================================
// Tab 状态
// ============================================================================

const activeTab = ref<"running" | "terminal" | "install">("running");

// ============================================================================
// 崩溃弹窗
// ============================================================================

const showCrashModal = computed(() => processStore.crashedReason !== null);
const crashClassification = computed(() => {
  const r = processStore.crashedReason;
  if (!r) return null;
  return classifyError({
    exit_code: r.exit_code ?? null,
    stderr_tail: r.stderr_tail,
  });
});

function dismissCrash() {
  processStore.dismissCrashed();
}

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

// ============================================================================
// 进入页面：清空错误未读
// ============================================================================

onMounted(async () => {
  // v3.10：菜单红点清零
  errorLog.markAllRead();
});

// 当 process_crashed 事件触发时，自动切到"日志"Tab（用户能看到崩溃）
watch(
  () => processStore.crashedReason,
  (r) => {
    if (r) {
      activeTab.value = "running";
    }
  },
);
</script>

<template>
  <div class="logs-page">
    <!-- 顶部跨 Tab 状态卡（启动中 + 业务错误） -->
    <StartProgressCard />
    <ErrorPanelCard />

    <!-- Tab 容器 -->
    <div class="tabs-wrapper">
      <NTabs
        v-model:value="activeTab"
        type="line"
        animated
        class="main-tabs"
      >
        <!-- Tab 1: 日志（ComfyUI 运行日志） -->
        <NTabPane name="running" tab="📜 日志" display-directive="show">
          <RunningLogsTab />
        </NTabPane>

        <!-- Tab 2: 终端（伪交互式终端） -->
        <NTabPane name="terminal" tab="💻 终端" display-directive="show">
          <TerminalTab />
        </NTabPane>

        <!-- Tab 3: 安装日志（环境/版本/插件） -->
        <NTabPane name="install" tab="🛠 安装日志" display-directive="show">
          <InstallLogsTab />
        </NTabPane>
      </NTabs>
    </div>

    <!-- 崩溃弹窗（智能错误分类） -->
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
        <NAlert
          v-if="crashClassification"
          :type="crashClassification.severity === 'critical' || crashClassification.severity === 'high' ? 'error' : crashClassification.severity === 'medium' ? 'warning' : 'info'"
          :show-icon="true"
        >
          <template #header>
            <strong>{{ crashClassification.title }}</strong>
          </template>
          <div class="classification-detail">
            <p>{{ crashClassification.description }}</p>
            <p class="root-cause">
              <strong>根因：</strong>{{ crashClassification.root_cause }}
            </p>
            <div
              v-if="crashClassification.recommended_actions.length > 0"
              class="actions-list"
            >
              <strong>建议操作：</strong>
              <ul>
                <li
                  v-for="(action, idx) in crashClassification.recommended_actions"
                  :key="idx"
                  :class="{ primary: action.primary }"
                >
                  <span v-if="action.primary">👉 </span>
                  <span v-else>· </span>
                  {{ action.label }}
                </li>
              </ul>
            </div>
          </div>
        </NAlert>

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
        <pre class="crash-stderr">
          {{ processStore.crashedReason.stderr_tail.join("\n") || "(无 stderr 输出)" }}
        </pre>
      </NSpace>
    </NModal>

    <!-- 端口被占弹窗 -->
    <PortConflictModal />
  </div>
</template>

<style scoped>
.logs-page {
  padding: 16px;
  max-width: 1400px;
  margin: 0 auto;
  height: calc(100vh - 32px);
  display: flex;
  flex-direction: column;
  gap: 0;
  overflow: hidden;
  box-sizing: border-box;
}

/* Tab 容器（占满剩余空间） */
.tabs-wrapper {
  display: flex;
  flex-direction: column;
  flex: 1;
  min-height: 0;
}

.main-tabs {
  display: flex;
  flex-direction: column;
  flex: 1;
  min-height: 0;
}

.main-tabs :deep(.n-tabs-nav) {
  flex-shrink: 0;
}

.main-tabs :deep(.n-tabs-content) {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
}

.main-tabs :deep(.n-tab-pane) {
  display: flex;
  flex-direction: column;
  flex: 1;
  min-height: 0;
  padding-top: 12px;
}

/* 崩溃弹窗样式 */
.crash-info {
  display: flex;
  gap: 8px;
  align-items: center;
}

.classification-detail p {
  margin: 6px 0;
  line-height: 1.5;
}

.classification-detail .root-cause {
  font-size: 13px;
  opacity: 0.85;
}

.classification-detail .actions-list {
  margin-top: 8px;
  font-size: 13px;
}

.classification-detail .actions-list ul {
  margin: 6px 0 0 0;
  padding-left: 0;
  list-style: none;
}

.classification-detail .actions-list li {
  margin: 4px 0;
  padding: 4px 0;
  line-height: 1.4;
}

.classification-detail .actions-list li.primary {
  font-weight: 600;
  color: var(--app-primary, #18a058);
}

.crash-stderr {
  font-family: "Cascadia Code", "Consolas", monospace;
  font-size: 12px;
  background: #1e1e1e;
  color: #d4d4d4;
  padding: 12px;
  border-radius: 4px;
  white-space: pre-wrap;
  word-break: break-word;
  max-height: 400px;
  overflow-y: auto;
  margin: 0;
}
</style>
