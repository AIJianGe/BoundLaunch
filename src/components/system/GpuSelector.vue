<!--
  GpuSelector - 多卡选择（仅"全部使用"和"单卡模式"）

  v3.x Phase 5：用户决策
  - 全部使用 → 不设置 CUDA_VISIBLE_DEVICES
  - 单卡模式 → CUDA_VISIBLE_DEVICES=0, CUDA_VISIBLE_DEVICES=1, ...

  通过 env 变量传递（不存到 config.toml，避免多卡机器切换目录时混淆）
  设计原则：简化决策，不考虑 NVLink 集群等高级配置。
-->
<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { NRadioButton, NRadioGroup, NSelect, NSpace, NText } from "naive-ui";
import { systemDetectGpus } from "@/api/env";
import type { GpuInfo } from "@/api/types";

export type GpuSelectionMode = "all" | "single";

export interface GpuSelection {
  mode: GpuSelectionMode;
  /** 单卡模式时选中的 GPU 索引（0-based） */
  singleIndex: number;
}

const props = defineProps<{
  modelValue: GpuSelection;
}>();

const emit = defineEmits<{
  (e: "update:modelValue", val: GpuSelection): void;
}>();

const gpus = ref<GpuInfo[]>([]);
const loading = ref(false);

const mode = computed({
  get: () => props.modelValue.mode,
  set: (v) => emit("update:modelValue", { ...props.modelValue, mode: v }),
});

const singleIndex = computed({
  get: () => props.modelValue.singleIndex,
  set: (v) => emit("update:modelValue", { ...props.modelValue, singleIndex: v }),
});

const gpuOptions = computed(() =>
  gpus.value.map((g, i) => ({
    label: `[${i}] ${g.model}${g.vram_mb ? ` (${Math.round(g.vram_mb / 1024)}GB)` : ""}`,
    value: i,
  })),
);

async function refresh() {
  loading.value = true;
  try {
    gpus.value = await systemDetectGpus(true);
  } catch (err) {
    console.warn("[GpuSelector] 检测 GPU 失败:", err);
    gpus.value = [];
  } finally {
    loading.value = false;
  }
}

watch(
  () => props.modelValue.mode,
  (newMode) => {
    if (newMode === "single" && gpus.value.length > 0 && singleIndex.value >= gpus.value.length) {
      // 修正超出范围的索引
      emit("update:modelValue", { ...props.modelValue, singleIndex: 0 });
    }
  },
  { immediate: true },
);

refresh();
</script>

<template>
  <div>
    <n-space vertical>
      <n-text strong>GPU 选择</n-text>
      <n-radio-group v-model:value="mode" name="gpu-mode">
        <n-radio-button value="all">全部使用</n-radio-button>
        <n-radio-button value="single" :disabled="gpus.length === 0">单卡模式</n-radio-button>
      </n-radio-group>

      <n-select
        v-if="mode === 'single' && gpus.length > 0"
        v-model:value="singleIndex"
        :options="gpuOptions"
        :loading="loading"
        placeholder="选择 GPU"
      />

      <n-text v-if="gpus.length === 0 && !loading" depth="3">
        未检测到 GPU（自动使用 CPU 模式）
      </n-text>
      <n-text v-else-if="gpus.length > 1" depth="3" style="font-size: 12px">
        检测到 {{ gpus.length }} 块 GPU。选择"单卡模式"可指定其中一块。
      </n-text>
    </n-space>
  </div>
</template>
