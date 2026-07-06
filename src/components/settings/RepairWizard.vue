<script setup lang="ts">
/**
 * 环境修复向导（v1.8 / F36-Phase2）
 *
 * ## 用途
 *
 * 用户在「环境检查」页 / 首页 StatusCard 看到 torch 未安装或环境异常时，
 * 点击「诊断+修复」按钮触发本组件：
 *
 * 1. 自动调 `envStore.diagnose()` 扫描 venv + torch import + 关键依赖
 * 2. 列出所有 issue（按严重度排序）
 * 3. 展示后端建议的修复动作（suggested_action）
 * 4. 用户可手动切换 action（覆盖默认建议）
 * 5. 点击「一键修复」→ 调 `envStore.repair(action)` 走 TaskScheduler
 * 6. 修复完成后自动重新诊断
 *
 * ## 设计模式
 *
 * - **State Machine**：`idle → diagnosing → reviewing → repairing → done/failed`
 * - **Presentational**：纯展示 + emit close
 * - **Adapter**：将后端 DiagnoseReport 翻译为用户可读的 issues 列表
 *
 * 详见 `PR/03-模块设计/02-PythonEnvManager.md §16 环境修复`
 */

import { ref, computed, watch } from "vue";
import {
  NModal,
  NCard,
  NButton,
  NTag,
  NAlert,
  NSpace,
  NSpin,
  NRadioGroup,
  NRadio,
  NProgress,
} from "naive-ui";
import { useEnvStore } from "@/stores/env";
import { useToast } from "@/composables/useToast";
import { useTaskProgress } from "@/composables/useTaskProgress";
import { envRepair } from "@/api/env";
import type { DiagnoseReport, EnvIssue, RepairAction } from "@/api/types";

const props = defineProps<{
  /** 是否显示 */
  show: boolean;
}>();

const emit = defineEmits<{
  /** 关闭（不重置内部状态，下次打开时重新诊断） */
  (e: "close"): void;
  /** 修复完成（让父组件决定是否刷新页面） */
  (e: "repaired"): void;
}>();

const envStore = useEnvStore();
const toast = useToast();

/** 内部阶段 */
type Phase = "idle" | "diagnosing" | "reviewing" | "repairing" | "done";
const phase = ref<Phase>("idle");

/** 当前诊断报告 */
const report = ref<DiagnoseReport | null>(null);
/** 用户选中的修复动作（默认 = suggested_action） */
const selectedAction = ref<RepairAction>("none");
/** 诊断错误 */
const diagnoseError = ref<string | null>(null);

// ========== 自动诊断：每次打开都跑一次 ==========
watch(
  () => props.show,
  async (show) => {
    if (show) {
      // 每次打开都重新诊断（保持最新状态）
      await runDiagnose();
    }
  },
  { immediate: true },
);

async function runDiagnose() {
  phase.value = "diagnosing";
  diagnoseError.value = null;
  report.value = null;
  try {
    const r = await envStore.diagnose();
    report.value = r;
    selectedAction.value = r.suggested_action;
    phase.value = "reviewing";
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    diagnoseError.value = msg;
    phase.value = "idle";
    toast.error("环境诊断失败", msg);
  }
}

// ========== 任务进度跟踪（修复阶段） ==========
const {
  progress,
  message: progressMessage,
  isRunning: isRepairing,
  isCompleted: repairCompleted,
  isFailed: repairFailed,
  trackTask,
} = useTaskProgress();

watch(isRepairing, (running) => {
  // 状态机由 onStartRepair / watch(repairCompleted|repairFailed) 推进
  // 这里只防止 race：trackTask 之前 phase 已被设为 repairing
  if (running && phase.value === "reviewing") {
    phase.value = "repairing";
  }
});

watch([repairCompleted, repairFailed], ([done, failed]) => {
  if (done) {
    phase.value = "done";
    toast.success("环境修复完成");
    emit("repaired");
  } else if (failed) {
    phase.value = "reviewing";
    toast.error("环境修复失败", progressMessage.value ?? "未知错误");
  }
});

// ========== 发起修复 ==========
async function onStartRepair() {
  if (selectedAction.value === "none") {
    toast.info("无需修复，环境已健康");
    return;
  }
  if (isRepairing.value) return;

  // 危险动作二次确认
  if (selectedAction.value === "rebuild_venv") {
    const ok = window.confirm(
      "⚠ 重建 venv 将删除当前虚拟环境并完全重建，过程中 ComfyUI 必须停止。\n\n" +
        "预计耗时 5-15 分钟，custom_nodes 依赖需要重装。\n\n" +
        "确定继续？",
    );
    if (!ok) return;
  }

  // 立即切到 repairing 阶段（trackTask 是非阻塞的，listen 注册后才真正等事件）
  phase.value = "repairing";

  // 通过 trackTask 跟踪任务进度；store 的 repair() 会同时调用 waitForTask
  // （这里只调底层 API 拿 task_id 给 trackTask，避免 store 中 waitForTask 重复）
  try {
    const taskId = await envRepair(selectedAction.value);
    await trackTask(taskId);
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    toast.error("提交修复任务失败", msg);
    phase.value = "reviewing";
  }
}

// ========== 派生显示 ==========

const severityLabel: Record<EnvIssue["severity"], string> = {
  info: "提示",
  warning: "警告",
  error: "错误",
  critical: "严重",
};

interface ActionOption {
  value: RepairAction;
  title: string;
  desc: string;
  duration: string;
  type: "default" | "info" | "warning" | "error";
}

const actionOptions = computed<ActionOption[]>(() => {
  const opts: ActionOption[] = [];
  if (!report.value) return opts;
  const hasCritical = report.value.issues.some(
    (i) => i.severity === "critical" || i.severity === "error",
  );
  const hasNumpy = report.value.issues.some((i) => i.code.startsWith("numpy."));
  const hasRequirements = report.value.issues.some(
    (i) => i.code.startsWith("deps.") || i.code === "deps.requirements_mismatch",
  );

  if (hasNumpy) {
    opts.push({
      value: "downgrade_numpy",
      title: "降级 numpy（轻量）",
      desc: "仅降级 numpy 到 2.2.6（已知坏版本修复），耗时 30-60 秒",
      duration: "30-60 秒",
      type: "info",
    });
  }
  if (hasRequirements) {
    opts.push({
      value: "reinstall_requirements",
      title: "重装 ComfyUI 依赖",
      desc: "重新安装 ComfyUI requirements.txt 全部依赖，耗时 1-3 分钟",
      duration: "1-3 分钟",
      type: "info",
    });
  }
  if (hasCritical) {
    opts.push({
      value: "rebuild_venv",
      title: "重建 venv（最稳）",
      desc: "删 venv → 重建 → 装 torch + 装 requirements，一次性修所有问题",
      duration: "5-15 分钟",
      type: "warning",
    });
  }
  // 兜底：reinstall_torch
  if (opts.length === 0) {
    opts.push({
      value: "reinstall_torch",
      title: "重装 torch",
      desc: "重新安装 PyTorch（CUDA 版本自动检测）",
      duration: "3-8 分钟",
      type: "info",
    });
  }
  return opts;
});

const torchOk = computed(() => report.value?.torch_import_ok ?? false);
const hasIssues = computed(() => (report.value?.issues.length ?? 0) > 0);

function severityTagType(s: EnvIssue["severity"]): "info" | "warning" | "error" | "success" {
  if (s === "info") return "info";
  if (s === "warning") return "warning";
  return "error";
}

function onClose() {
  emit("close");
}
</script>

<template>
  <NModal
    :show="show"
    @update:show="(v: boolean) => !v && onClose()"
    :mask-closable="!isRepairing"
    :auto-focus="true"
  >
    <NCard
      class="repair-wizard-card"
      :bordered="false"
      size="small"
      role="dialog"
      aria-modal="true"
    >
      <template #header>
        <span class="dialog-title">🔧 环境诊断与修复</span>
      </template>
      <template #header-extra>
        <NButton
          quaternary
          size="small"
          :disabled="isRepairing"
          @click="onClose"
        >
          ✕
        </NButton>
      </template>

      <div class="dialog-body">
        <!-- 阶段 1：诊断中 -->
        <div v-if="phase === 'diagnosing'" class="phase-block">
          <NSpace align="center" :size="12">
            <NSpin size="small" />
            <span>正在扫描环境（venv + torch import + 关键依赖）...</span>
          </NSpace>
        </div>

        <!-- 诊断失败 -->
        <div v-else-if="diagnoseError" class="phase-block">
          <NAlert type="error" :bordered="false" class="status-banner">
            <template #header>诊断失败</template>
            {{ diagnoseError }}
          </NAlert>
          <NSpace :size="8" style="margin-top: 12px" justify="center">
            <NButton size="small" @click="runDiagnose">重试</NButton>
            <NButton size="small" @click="onClose">关闭</NButton>
          </NSpace>
        </div>

        <!-- 阶段 2：查看报告 -->
        <template v-else-if="phase === 'reviewing' && report">
          <!-- 健康状态 -->
          <NAlert
            v-if="!hasIssues"
            type="success"
            :bordered="false"
            class="status-banner"
          >
            <template #header>环境健康</template>
            ✅ 全部检查通过，无需修复
            <NTag size="small" type="success" style="margin-left: 8px">
              torch {{ report.torch_version || "已安装" }}
            </NTag>
          </NAlert>

          <!-- 不健康：列出问题 -->
          <template v-else>
            <NAlert
              v-if="!torchOk"
              type="error"
              :bordered="false"
              class="status-banner"
            >
              ❌ PyTorch 不可用（无法 import 或 import 报错），需要修复
            </NAlert>

            <!-- 问题列表 -->
            <div class="section">
              <div class="section-title">
                🩺 诊断结果（{{ report.issues.length }} 个问题）
              </div>
              <div class="issue-list">
                <div
                  v-for="(issue, idx) in report.issues"
                  :key="idx"
                  class="issue-item"
                >
                  <NTag size="small" :type="severityTagType(issue.severity)">
                    {{ severityLabel[issue.severity] }}
                  </NTag>
                  <div class="issue-content">
                    <div class="issue-message">{{ issue.message }}</div>
                    <div v-if="issue.detail" class="issue-detail">
                      {{ issue.detail }}
                    </div>
                    <code class="issue-code">{{ issue.code }}</code>
                  </div>
                </div>
              </div>
            </div>

            <div class="section">
              <div class="section-title">💡 建议修复动作</div>
              <NAlert
                type="info"
                :bordered="false"
                size="small"
                class="recommend-hint"
              >
                {{ report.suggested_reason }}
              </NAlert>

              <NRadioGroup
                v-model:value="selectedAction"
                class="action-radio-group"
                :disabled="isRepairing"
              >
                <NSpace vertical :size="8">
                  <div
                    v-for="opt in actionOptions"
                    :key="opt.value"
                    class="action-option"
                    :class="{
                      selected: selectedAction === opt.value,
                      recommended: report.suggested_action === opt.value,
                    }"
                    @click="selectedAction = opt.value"
                  >
                    <NRadio :value="opt.value" :disabled="isRepairing" />
                    <div class="action-content">
                      <div class="action-header">
                        <span class="action-title">
                          {{ opt.title }}
                          <NTag
                            v-if="report.suggested_action === opt.value"
                            size="tiny"
                            type="success"
                            class="action-tag"
                          >
                            推荐
                          </NTag>
                        </span>
                        <span class="action-duration">⏱ {{ opt.duration }}</span>
                      </div>
                      <div class="action-desc">{{ opt.desc }}</div>
                    </div>
                  </div>
                </NSpace>
              </NRadioGroup>
            </div>
          </template>
        </template>

        <!-- 阶段 3：修复中 -->
        <div v-else-if="phase === 'repairing'" class="phase-block">
          <div class="repairing-header">
            <NSpin size="small" />
            <span class="repairing-title">正在修复环境...</span>
          </div>
          <NProgress
            type="line"
            :percentage="progress"
            :show-indicator="true"
            :height="10"
            :bordered="false"
            style="margin-top: 12px"
          />
          <div v-if="progressMessage" class="progress-message">
            {{ progressMessage }}
          </div>
          <div class="progress-hint">
            修复过程中请勿关闭应用。预计耗时 30 秒 - 15 分钟。
          </div>
        </div>

        <!-- 阶段 4：完成 -->
        <div v-else-if="phase === 'done'" class="phase-block">
          <NAlert type="success" :bordered="false" class="status-banner">
            <template #header>修复完成</template>
            ✅ 环境已修复，建议重新启动 ComfyUI 验证。
          </NAlert>
          <NSpace :size="8" style="margin-top: 12px" justify="center">
            <NButton type="primary" @click="runDiagnose">再次诊断</NButton>
            <NButton @click="onClose">关闭</NButton>
          </NSpace>
        </div>
      </div>

      <template #footer>
        <div class="dialog-footer">
          <!-- 诊断中或修复中：禁用按钮 -->
          <NButton
            v-if="phase === 'reviewing' && hasIssues"
            type="primary"
            :loading="isRepairing"
            :disabled="
              isRepairing || selectedAction === 'none' || !selectedAction
            "
            @click="onStartRepair"
          >
            {{
              selectedAction === "rebuild_venv"
                ? "重建 venv"
                : selectedAction === "reinstall_requirements"
                  ? "重装依赖"
                  : selectedAction === "reinstall_torch"
                    ? "重装 torch"
                    : selectedAction === "downgrade_numpy"
                      ? "降级 numpy"
                      : "一键修复"
            }}
          </NButton>
          <NButton
            v-else-if="phase === 'reviewing' && !hasIssues"
            type="primary"
            @click="onClose"
          >
            完成
          </NButton>
          <NButton
            v-else
            :disabled="isRepairing"
            @click="onClose"
          >
            关闭
          </NButton>
        </div>
      </template>
    </NCard>
  </NModal>
</template>

<style scoped>
.repair-wizard-card {
  width: 720px;
  max-width: 92vw;
  max-height: 88vh;
  overflow-y: auto;
}

.dialog-title {
  font-weight: 600;
  font-size: 16px;
}

.dialog-body {
  padding: 4px 0;
}

.phase-block {
  padding: 24px 0;
  text-align: center;
}

.status-banner {
  margin-bottom: 16px;
}

.section {
  margin: 12px 0;
}

.section-title {
  font-size: 13px;
  font-weight: 600;
  margin-bottom: 10px;
  color: var(--app-text, #333);
}

.issue-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.issue-item {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  padding: 10px 12px;
  background: var(--app-bg-soft, #f8f9fa);
  border-radius: 6px;
  border-left: 3px solid var(--app-warning, #f0a020);
}

.issue-item :deep(.n-tag[data-type="error"]) {
  /* 让 error tag 充当左边框已够 */
}

.issue-content {
  flex: 1;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.issue-message {
  font-size: 13px;
  font-weight: 500;
  color: var(--app-text, #333);
}

.issue-detail {
  font-size: 12px;
  color: var(--app-text-muted, #666);
  line-height: 1.5;
  white-space: pre-wrap;
  word-break: break-word;
}

.issue-code {
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
  font-size: 11px;
  color: var(--app-text-muted, #999);
  background: var(--app-bg-muted, rgba(127, 127, 127, 0.08));
  padding: 2px 6px;
  border-radius: 3px;
  align-self: flex-start;
}

.recommend-hint {
  margin-bottom: 12px;
  font-size: 12px;
}

.action-radio-group {
  width: 100%;
}

.action-option {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  padding: 12px;
  border: 1px solid var(--app-border, #e0e0e0);
  border-radius: 6px;
  cursor: pointer;
  transition: all 0.2s;
}

.action-option:hover {
  border-color: var(--app-primary, #18a058);
  background: var(--app-bg-hover, #f5f5f5);
}

.action-option.selected {
  border-color: var(--app-primary, #18a058);
  background: var(--app-primary-soft, rgba(24, 160, 88, 0.05));
}

.action-option.recommended {
  border-color: var(--app-success, #18a058);
}

.action-content {
  flex: 1;
}

.action-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 4px;
}

.action-title {
  font-size: 14px;
  font-weight: 600;
  color: var(--app-text, #333);
  display: flex;
  align-items: center;
  gap: 8px;
}

.action-tag {
  flex-shrink: 0;
}

.action-duration {
  font-size: 12px;
  color: var(--app-text-muted, #999);
}

.action-desc {
  font-size: 12px;
  color: var(--app-text-muted, #666);
  line-height: 1.5;
}

.repairing-header {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 12px;
  margin-bottom: 4px;
}

.repairing-title {
  font-size: 15px;
  font-weight: 600;
  color: var(--app-text, #333);
}

.progress-message {
  margin-top: 12px;
  text-align: center;
  font-size: 13px;
  color: var(--app-text, #333);
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
}

.progress-hint {
  margin-top: 8px;
  text-align: center;
  font-size: 12px;
  color: var(--app-text-muted, #999);
}

.dialog-footer {
  display: flex;
  justify-content: flex-end;
  gap: 12px;
}
</style>
