<script setup lang="ts">
/**
 * 版本切换确认对话框（v1.8 / F36 重构 → v3.5 异步化）
 *
 * v3.1 / F26 旧版：直接显示 11 步流程，固定"删 venv + 重建"
 * v1.8 / F36 新版：
 *   - 调 core_check_version_compatibility 拿 VersionCompatReport
 *   - 显示当前 / 目标 Python / torch / requirements.txt 差异
 *   - 用户 3 选 1：
 *     1. 全部清除（2-5 分钟，100% 干净，custom_nodes 依赖也清空）
 *     2. 升/降版本（30-60 秒，保留 venv，pip --upgrade --force-reinstall）
 *     3. 不动环境（5-10 秒，只切 git tag，venv 不动）
 *   - 推荐模式（recommend_mode 自动选），用户可改
 *   - 记忆选择（checkbox 写到 localStorage）
 *
 * v3.5 改造：
 *   - 兼容性预检走异步 task（useCheckCompat）
 *   - 加载时显示进度条 + 实时日志（底部可折叠面板）
 *   - 不阻塞 UI，超时由用户取消触发
 *
 * ## 设计模式
 * - **Presentational**：纯展示 + emit confirm/cancel
 * - **Adapter**：将复杂的兼容性问题简化为用户可读的描述
 * - **Template Method**：复用 useCheckCompat 的"提交-跟踪-完成"流程
 */

import { ref, computed, watch } from "vue";
import {
  NModal,
  NCard,
  NButton,
  NTag,
  NAlert,
  NSpace,
  NDivider,
  NRadioGroup,
  NRadio,
  NCheckbox,
  NSpin,
  NIcon,
  NProgress,
} from "naive-ui";
import type { TagInfo } from "@/api/types";
import { type SwitchMode, type VersionCompatReport } from "@/api/core";
import { useCheckCompat } from "@/composables/useSwitchVersion";

const STORAGE_KEY = "switch-mode-preference";

const props = defineProps<{
  /** 是否显示 */
  show: boolean;
  /** 目标版本 tag */
  targetTag: TagInfo | null;
  /** 当前版本 tag 名（null = 未在 tag 上） */
  currentVersion: string | null;
  /** 切换中状态（确认按钮 loading） */
  switching?: boolean;
}>();

const emit = defineEmits<{
  /** 用户确认切换（带 mode） */
  (e: "confirm", mode: SwitchMode): void;
  /** 用户取消 / 关闭 */
  (e: "cancel"): void;
}>();

// ========== 兼容报告加载（v3.5 异步化） ==========
const compat = useCheckCompat();

async function loadReport(tagName: string) {
  await compat.check(tagName);
}

watch(
  () => [props.show, props.targetTag?.name] as const,
  async ([show, tagName]) => {
    if (show && tagName) {
      await loadReport(tagName);
    } else {
      compat.reset();
    }
  },
  { immediate: true },
);

// ========== 用户选择 ==========
const selectedMode = ref<SwitchMode>("Preserve");
const rememberChoice = ref(false);

// 加载报告后默认选推荐模式
watch(
  () => compat.report.value?.recommendedMode,
  (mode) => {
    if (mode) {
      // 检查用户是否记忆了选择
      const saved = localStorage.getItem(STORAGE_KEY);
      if (saved === "Clean" || saved === "Preserve" || saved === "Skip") {
        selectedMode.value = saved;
      } else {
        selectedMode.value = mode;
      }
    }
  },
  { immediate: true },
);

// ========== 派生显示 ==========
const isPrerelease = computed(
  () => props.targetTag !== null && !props.targetTag.is_stable,
);

const diffSummary = computed(() => {
  const r = compat.report.value;
  if (!r) return "";
  switch (r.requirementsDiff.kind) {
    case "Identical":
      return "✓ requirements.txt 完全一致";
    case "OnlyMissing":
      return `+ ${r.requirementsDiff.missingPackages.length} 个新依赖（${r.requirementsDiff.missingPackages
        .slice(0, 3)
        .join(", ")}${r.requirementsDiff.missingPackages.length > 3 ? "..." : ""}）`;
    case "HasMajorChange":
      return `⚠ ${r.requirementsDiff.changed.length} 个包版本变化`;
  }
});

const modeDesc: Record<
  SwitchMode,
  {
    title: string;
    duration: string;
    pros: string;
    cons: string;
    type: "warning" | "info" | "default";
  }
> = {
  Clean: {
    title: "全部清除（100% 干净）",
    duration: "2-5 分钟",
    pros: "删 venv → 重建 → 装 requirements + torch + 重装 custom_nodes 依赖",
    cons: "⚠ custom_nodes 依赖需重装；torch 大文件重下（~700MB）",
    type: "warning",
  },
  Preserve: {
    title: "升/降版本（推荐）",
    duration: "30-60 秒",
    pros: "保留 venv → pip install -r new-req.txt --upgrade --force-reinstall",
    cons: "✓ custom_nodes 保留；✓ torch 保留；跨大版本可能需手动调依赖",
    type: "info",
  },
  Skip: {
    title: "不动环境（最快）",
    duration: "5-10 秒",
    pros: "只切 git tag，venv 不动",
    cons: "⚠ 启动时若缺包会报错（推荐 patch 版本切换）",
    type: "default",
  },
};

function onConfirm() {
  if (rememberChoice.value) {
    localStorage.setItem(STORAGE_KEY, selectedMode.value);
  }
  emit("confirm", selectedMode.value);
}

function onCancel() {
  compat.reset();
  emit("cancel");
}

// ========== 实时日志面板状态 ==========
const showLogPanel = ref(false);

// 自动滚动到日志末尾
const logContainer = ref<HTMLElement | null>(null);
function scrollToBottom() {
  if (logContainer.value) {
    logContainer.value.scrollTop = logContainer.value.scrollHeight;
  }
}

watch(
  () => compat.logs.value.length,
  () => {
    if (showLogPanel.value) {
      // 等待 DOM 更新
      setTimeout(scrollToBottom, 10);
    }
  },
);
</script>

<template>
  <NModal
    :show="show"
    @update:show="(v: boolean) => !v && onCancel()"
    :mask-closable="false"
    :auto-focus="true"
  >
    <NCard
      class="switch-dialog-card"
      :bordered="false"
      size="small"
      role="dialog"
      aria-modal="true"
    >
      <template #header>
        <span class="dialog-title">切换 ComfyUI 版本</span>
      </template>
      <template #header-extra>
        <NButton
          quaternary
          size="small"
          :disabled="switching"
          @click="onCancel"
        >
          ✕
        </NButton>
      </template>

      <div v-if="targetTag" class="dialog-body">
        <!-- 版本变化 -->
        <div class="version-change">
          <div class="version-block">
            <div class="version-label">当前版本</div>
            <div class="version-value">
              {{ currentVersion || "(未在 tag 上)" }}
            </div>
          </div>
          <div class="version-arrow">→</div>
          <div class="version-block">
            <div class="version-label">目标版本</div>
            <div class="version-value">
              {{ targetTag.name }}
              <NTag
                v-if="isPrerelease"
                size="tiny"
                type="warning"
                class="version-tag"
              >
                预发布
              </NTag>
            </div>
          </div>
        </div>

        <NDivider />

        <!-- 兼容性分析（F36） -->
        <div class="section">
          <div class="section-title">📊 兼容性分析</div>
          <NSpin :show="compat.loading.value">
            <div v-if="compat.error.value" class="compat-error">
              <NAlert type="error" :bordered="false">
                加载兼容性报告失败: {{ compat.error.value }}
              </NAlert>
            </div>
            <div v-else-if="compat.report.value" class="compat-grid">
              <div class="compat-row">
                <span class="compat-label">Python</span>
                <span class="compat-value">
                  {{ compat.report.value.currentPython || "未安装" }}
                  <span class="compat-arrow">→</span>
                  <span
                    :class="
                      compat.report.value.samePython
                        ? 'compat-ok'
                        : 'compat-warn'
                    "
                  >
                    {{ compat.report.value.targetPython }}
                    {{ compat.report.value.samePython ? "✓" : "⚠" }}
                  </span>
                </span>
              </div>
              <div class="compat-row">
                <span class="compat-label">torch</span>
                <span class="compat-value">
                  {{ compat.report.value.currentTorchVariant || "未安装" }}
                  <span class="compat-arrow">→</span>
                  <span
                    :class="
                      compat.report.value.sameTorchVariant
                        ? 'compat-ok'
                        : 'compat-warn'
                    "
                  >
                    {{ compat.report.value.targetTorchVariant }}
                    {{
                      compat.report.value.sameTorchVariant ? "✓" : "⚠"
                    }}
                  </span>
                </span>
              </div>
              <div class="compat-row">
                <span class="compat-label">requirements.txt</span>
                <span class="compat-value">{{ diffSummary }}</span>
              </div>
              <div class="compat-row">
                <span class="compat-label">custom_nodes</span>
                <span class="compat-value">
                  {{ compat.report.value.customNodeCount }} 个
                </span>
              </div>
              <NAlert
                v-if="
                  compat.report.value.recommendedMode !== selectedMode
                "
                type="info"
                :bordered="false"
                size="small"
                class="recommend-hint"
              >
                💡 建议：{{ compat.report.value.recommendedReason }}
              </NAlert>
            </div>
          </NSpin>
        </div>

        <!-- v3.5：兼容性预检进度 + 实时日志（折叠面板） -->
        <div v-if="compat.loading.value || compat.logs.value.length > 0" class="compat-progress-section">
          <div class="compat-progress-header" @click="showLogPanel = !showLogPanel">
            <span class="compat-progress-title">
              {{ showLogPanel ? "▼" : "▶" }} 预检进度 ({{ compat.progress.value }}%)
            </span>
            <span class="compat-progress-message">
              {{ compat.message.value || "分析中..." }}
            </span>
          </div>
          <NProgress
            v-if="compat.loading.value"
            type="line"
            :percentage="compat.progress.value"
            :height="6"
            :show-indicator="false"
            status="info"
          />
          <!-- 实时日志面板（可折叠） -->
          <div v-if="showLogPanel" class="log-panel" ref="logContainer">
            <div
              v-for="(line, idx) in compat.logs.value"
              :key="idx"
              class="log-line"
            >
              <span class="log-source">[{{ line.source }}]</span>
              <span class="log-text">{{ line.text }}</span>
            </div>
            <div v-if="compat.logs.value.length === 0" class="log-empty">
              等待日志输出...
            </div>
          </div>
        </div>

        <NDivider />

        <!-- 切换模式选择（F36） -->
        <div class="section">
          <div class="section-title">⚙ 选择切换模式</div>
          <NRadioGroup
            v-model:value="selectedMode"
            :disabled="switching"
            class="mode-radio-group"
          >
            <NSpace vertical :size="12">
              <div
                v-for="(desc, key) in modeDesc"
                :key="key"
                class="mode-option"
                :class="{
                  selected: selectedMode === key,
                  recommended: compat.report.value?.recommendedMode === key,
                }"
                @click="selectedMode = key as SwitchMode"
              >
                <NRadio :value="key" :disabled="switching" />
                <div class="mode-content">
                  <div class="mode-header">
                    <span class="mode-title">
                      {{ desc.title }}
                      <NTag
                        v-if="compat.report.value?.recommendedMode === key"
                        size="tiny"
                        type="success"
                        class="mode-tag"
                      >
                        推荐
                      </NTag>
                    </span>
                    <span class="mode-duration">⏱ {{ desc.duration }}</span>
                  </div>
                  <div class="mode-pros">✓ {{ desc.pros }}</div>
                  <div class="mode-cons">{{ desc.cons }}</div>
                </div>
              </div>
            </NSpace>
          </NRadioGroup>
          <NCheckbox v-model:checked="rememberChoice" class="remember-checkbox">
            记住我的选择（下次切版本不再询问）
          </NCheckbox>
        </div>
      </div>

      <template #footer>
        <div class="dialog-footer">
          <NButton
            :disabled="switching"
            @click="onCancel"
          >
            取消
          </NButton>
          <NButton
            :type="modeDesc[selectedMode].type === 'warning' ? 'warning' : 'primary'"
            :loading="switching"
            :disabled="compat.loading.value"
            @click="onConfirm"
          >
            确认切换
          </NButton>
        </div>
      </template>
    </NCard>
  </NModal>
</template>

<style scoped>
.switch-dialog-card {
  width: 700px;
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

.version-change {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 24px;
  padding: 16px 0;
}

.version-block {
  text-align: center;
  flex: 1;
}

.version-label {
  font-size: 12px;
  color: var(--app-text-muted, #999);
  margin-bottom: 6px;
}

.version-value {
  font-size: 20px;
  font-weight: 700;
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
}

.version-arrow {
  font-size: 24px;
  color: var(--app-primary, #18a058);
  font-weight: 700;
}

.version-tag {
  flex-shrink: 0;
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

.compat-grid {
  background: var(--app-bg-soft, #f8f9fa);
  border-radius: 6px;
  padding: 12px;
}

.compat-row {
  display: flex;
  align-items: center;
  font-size: 13px;
  margin: 4px 0;
  gap: 8px;
}

.compat-label {
  flex-shrink: 0;
  width: 110px;
  color: var(--app-text-muted, #666);
  font-weight: 500;
}

.compat-value {
  flex: 1;
  font-family: "JetBrains Mono", monospace;
  font-size: 12px;
}

.compat-arrow {
  margin: 0 4px;
  color: var(--app-text-muted, #999);
}

.compat-ok {
  color: var(--app-success, #18a058);
  font-weight: 600;
}

.compat-warn {
  color: var(--app-warning, #f0a020);
  font-weight: 600;
}

.recommend-hint {
  margin-top: 12px;
  font-size: 12px;
}

/* v3.5：兼容性预检进度 + 实时日志 */
.compat-progress-section {
  margin: 12px 0;
  border: 1px solid var(--app-border, #e0e0e0);
  border-radius: 6px;
  padding: 8px 12px;
  background: var(--app-bg-soft, #fafbfc);
}

.compat-progress-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  cursor: pointer;
  user-select: none;
  padding: 4px 0;
}

.compat-progress-title {
  font-size: 12px;
  font-weight: 600;
  color: var(--app-text, #333);
}

.compat-progress-message {
  font-size: 11px;
  color: var(--app-text-muted, #999);
  font-family: "JetBrains Mono", monospace;
  flex: 1;
  margin-left: 8px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.log-panel {
  margin-top: 8px;
  max-height: 200px;
  overflow-y: auto;
  background: #1e1e1e;
  border-radius: 4px;
  padding: 8px;
  font-family: "JetBrains Mono", "Consolas", "Cascadia Code", monospace;
  font-size: 11px;
  color: #d4d4d4;
  scroll-behavior: smooth;
}

.log-line {
  display: flex;
  gap: 8px;
  padding: 1px 0;
  word-break: break-all;
}

.log-source {
  color: #569cd6;
  flex-shrink: 0;
  font-weight: 600;
}

.log-text {
  flex: 1;
  color: #d4d4d4;
  white-space: pre-wrap;
}

.log-empty {
  color: #6a6a6a;
  font-style: italic;
  padding: 8px 0;
  text-align: center;
}

.mode-radio-group {
  width: 100%;
}

.mode-option {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  padding: 12px;
  border: 1px solid var(--app-border, #e0e0e0);
  border-radius: 6px;
  cursor: pointer;
  transition: all 0.2s;
}

.mode-option:hover {
  border-color: var(--app-primary, #18a058);
  background: var(--app-bg-hover, #f5f5f5);
}

.mode-option.selected {
  border-color: var(--app-primary, #18a058);
  background: var(--app-primary-soft, rgba(24, 160, 88, 0.05));
}

.mode-option.recommended {
  border-color: var(--app-success, #18a058);
}

.mode-content {
  flex: 1;
}

.mode-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 4px;
}

.mode-title {
  font-size: 14px;
  font-weight: 600;
  color: var(--app-text, #333);
  display: flex;
  align-items: center;
  gap: 8px;
}

.mode-tag {
  flex-shrink: 0;
}

.mode-duration {
  font-size: 12px;
  color: var(--app-text-muted, #999);
}

.mode-pros {
  font-size: 12px;
  color: var(--app-text, #333);
  line-height: 1.5;
  margin: 2px 0;
}

.mode-cons {
  font-size: 12px;
  color: var(--app-text-muted, #666);
  line-height: 1.5;
}

.remember-checkbox {
  margin-top: 12px;
}

.dialog-footer {
  display: flex;
  justify-content: flex-end;
  gap: 12px;
}

.compat-error {
  padding: 8px 0;
}

@media (max-width: 600px) {
  .version-change {
    flex-direction: column;
    gap: 12px;
  }

  .version-arrow {
    transform: rotate(90deg);
  }
}
</style>
