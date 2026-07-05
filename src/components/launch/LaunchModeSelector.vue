<script setup lang="ts">
/**
 * §3.3 运行模式选择（v2.17 改下拉）
 *
 * 详见 `PR/06-界面设计.md §3.3 运行模式单选`
 *
 * 5 选项：CPU / GPU 高显存 / GPU 低显存 / GPU 无显存 / 自定义
 *
 * 切换联动：
 * - 更新顶部状态卡片的「运行模式」字段
 * - 更新命令预览框
 * - 不修改 torch 配置（torch 安装需在设置页手动触发）
 *
 * **运行中切换约束**：
 * - ComfyUI 运行中切换时弹出确认框「将停止当前 ComfyUI 并以新参数重启，是否继续？」
 * - 用户确认后自动 stop + 等待 + 切换配置（不自动 start，避免误启动）
 */

import { computed, h } from "vue";
import { NCard, NSelect, NTag, type SelectOption } from "naive-ui";
import { useConfigStore } from "@/stores/config";
import { useProcessStore } from "@/stores/process";
import { useToast } from "@/composables/useToast";
import { useConfirm } from "@/composables/useConfirm";
import type { LaunchMode } from "@/api/types";

const configStore = useConfigStore();
const processStore = useProcessStore();
const toast = useToast();
const { warn: showWarn } = useConfirm();

const currentMode = computed<LaunchMode | null>(() => configStore.launchMode);

const modeOptions: Array<{
  value: LaunchMode;
  label: string;
  hint: string;
  recommended?: boolean;
}> = [
  { value: "cpu", label: "CPU", hint: "--cpu --lowvram" },
  { value: "gpu_high", label: "GPU 高显存", hint: "--highvram", recommended: true },
  { value: "gpu_low", label: "GPU 低显存", hint: "--lowvram" },
  { value: "gpu_no_vram", label: "GPU 无显存", hint: "--novram" },
  { value: "custom", label: "自定义", hint: "用户填 custom_args" },
];

/** NSelect options（label + 描述 hint） */
const selectOptions = computed<SelectOption[]>(() =>
  modeOptions.map((opt) => ({
    label: opt.label + (opt.recommended ? "（推荐）" : ""),
    value: opt.value,
    hint: opt.hint,
  })),
);

const selectedHint = computed(
  () => modeOptions.find((o) => o.value === currentMode.value)?.hint ?? "",
);

async function onChange(mode: LaunchMode) {
  if (mode === currentMode.value) return;

  // 运行中切换需确认
  if (processStore.isAlive) {
    const ok = await showWarn(
      "切换运行模式",
      "ComfyUI 正在运行，切换模式将先停止当前进程，是否继续？",
    );
    if (!ok) return;
    try {
      await processStore.stop();
    } catch (e) {
      toast.error("停止失败", e);
      return;
    }
  }

  try {
    await configStore.update({
      launch: { mode },
    });
    toast.success(
      "运行模式已切换为：" + (modeOptions.find((o) => o.value === mode)?.label || mode),
    );
  } catch (e) {
    toast.error("配置更新失败", e);
  }
}

const renderLabel = (option: SelectOption) => {
  const opt = modeOptions.find((o) => o.value === option.value);
  return h(
    "div",
    { class: "mode-select-row" },
    [
      h("span", { class: "mode-select-label" }, String(option.label)),
      opt?.recommended
        ? h(
            NTag,
            { size: "tiny", type: "success", class: "mode-select-tag" },
            { default: () => "推荐" },
          )
        : null,
      h("span", { class: "mode-select-hint" }, String(option.hint || "")),
    ].filter(Boolean),
  );
};
</script>

<template>
  <NCard class="launch-mode-selector" :bordered="true" size="small">
    <template #header>
      <span class="header-title">⚙️ 运行模式</span>
    </template>

    <div class="mode-select-wrapper">
      <NSelect
        :value="currentMode || undefined"
        :options="selectOptions"
        :render-label="renderLabel"
        placeholder="选择运行模式"
        size="medium"
        @update:value="onChange"
      />
    </div>

    <div v-if="selectedHint" class="mode-current-hint">
      当前参数：<code>{{ selectedHint }}</code>
    </div>

    <div class="mode-tip">
      ℹ 运行模式决定 ComfyUI 加载模型时的显存策略；
      切换模式不会重新安装 PyTorch，需在「设置页」手动操作。
    </div>
  </NCard>
</template>

<style scoped>
.launch-mode-selector {
  margin-bottom: 16px;
}

.header-title {
  font-weight: 600;
}

.mode-select-wrapper {
  margin-bottom: 8px;
}

.mode-current-hint {
  margin-top: 4px;
  font-size: 12px;
  color: var(--app-text-muted, #999);
}

.mode-current-hint code {
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
  background: var(--app-bg-soft, rgba(127, 127, 127, 0.05));
  padding: 2px 6px;
  border-radius: 3px;
  color: var(--app-text-default, #555);
}

.mode-tip {
  margin-top: 12px;
  padding: 8px 12px;
  background: var(--app-bg-soft, rgba(127, 127, 127, 0.05));
  border-radius: 4px;
  font-size: 12px;
  color: var(--app-text-muted, #999);
}

/* NSelect 自定义下拉项 */
:deep(.mode-select-row) {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 13px;
}

:deep(.mode-select-label) {
  font-weight: 500;
}

:deep(.mode-select-tag) {
  font-size: 10px;
  padding: 0 4px;
  height: 16px;
  line-height: 16px;
}

:deep(.mode-select-hint) {
  margin-left: auto;
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
  font-size: 11px;
  color: var(--app-text-muted, #999);
}
</style>
