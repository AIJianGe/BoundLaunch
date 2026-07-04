<script setup lang="ts">
/**
 * §3.3 运行模式单选
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

import { computed } from "vue";
import { NCard, NRadioGroup, NRadio, NSpace, NTag } from "naive-ui";
import { useConfigStore } from "@/stores/config";
import { useProcessStore } from "@/stores/process";
import { useToast } from "@/composables/useToast";
import { useConfirm } from "@/composables/useConfirm";
import type { LaunchMode } from "@/api/types";

const configStore = useConfigStore();
const processStore = useProcessStore();
const toast = useToast();
const confirm = useConfirm();

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

async function onChange(mode: LaunchMode) {
  if (mode === currentMode.value) return;

  // 运行中切换需确认
  if (processStore.isAlive) {
    const ok = await confirm.warn(
      "切换运行模式",
      "ComfyUI 正在运行，切换模式将先停止当前进程，是否继续？",
    );
    if (!ok) return;
    try {
      await processStore.stop();
      // 等待进程完全停止（事件订阅会更新 status）
      // 这里仅等待后端响应，不轮询
    } catch (e) {
      toast.error("停止失败", e);
      return;
    }
  }

  try {
    await configStore.update({
      launch: { mode },
    });
    toast.success(`运行模式已切换为：${modeOptions.find((o) => o.value === mode)?.label}`);
  } catch (e) {
    toast.error("配置更新失败", e);
  }
}
</script>

<template>
  <NCard class="launch-mode-selector" :bordered="true" size="small">
    <template #header>
      <span class="header-title">⚙️ 运行模式</span>
    </template>

    <NRadioGroup
      :value="currentMode || undefined"
      @update:value="onChange"
    >
      <NSpace vertical :size="8">
        <div
          v-for="opt in modeOptions"
          :key="opt.value"
          class="mode-option"
        >
          <NRadio :value="opt.value">
            <span class="mode-label">{{ opt.label }}</span>
            <NTag
              v-if="opt.recommended"
              size="tiny"
              type="success"
              class="recommend-tag"
            >
              推荐
            </NTag>
          </NRadio>
          <span class="mode-hint">{{ opt.hint }}</span>
        </div>
      </NSpace>
    </NRadioGroup>

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

.mode-option {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 4px 0;
}

.mode-label {
  font-weight: 500;
}

.recommend-tag {
  margin-left: 6px;
}

.mode-hint {
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
  font-size: 12px;
  color: var(--app-text-muted, #999);
}

.mode-tip {
  margin-top: 12px;
  padding: 8px 12px;
  background: var(--app-bg-soft, rgba(127, 127, 127, 0.05));
  border-radius: 4px;
  font-size: 12px;
  color: var(--app-text-muted, #999);
}
</style>
