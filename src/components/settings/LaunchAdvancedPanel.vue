<!--
  LaunchAdvancedPanel - 启动高级参数面板

  v3.x 新增：集中展示 ComfyUI 启动时的"高级"参数开关

  当前包含：
  - "使用共享显存"开关（gpu_only）
    · 关闭（默认）：ComfyUI 行为不变，模型不够显存时 spill 到 CPU 内存
    · 开启：传 --gpu-only 给 ComfyUI，强制全部在 GPU 显存

  旧的 8 个 AdvancedArgs 字段（use_split_cross_attention / force_fp32 / ...）
  仍然只在 LaunchPage 的 AdvancedParamsPanel 折叠区可见
  → 这两个面板职责不同：
    · LaunchAdvancedPanel：用户日常会调整的"性能/显存"开关
    · AdvancedParamsPanel：开发/调试向的进阶 flags
-->
<script setup lang="ts">
/**
 * LaunchAdvancedPanel - 启动高级参数面板
 *
 * 详见 `PR/06-界面设计.md §3.x 启动高级参数` (v3.x 新增)
 */
import { computed } from "vue";
import {
  NCard,
  NSwitch,
  NSpace,
  NText,
  NAlert,
  NTag,
} from "naive-ui";
import { storeToRefs } from "pinia";
import { useConfigStore } from "@/stores/config";
import { useToast } from "@/composables/useToast";
import type { AdvancedArgs, LaunchConfig, LaunchMode } from "@/api/types";

const configStore = useConfigStore();
const { config } = storeToRefs(configStore);
const toast = useToast();

// 启动模式（用于联动：LowVram/NoVram 时强制禁灰 gpu_only）
const launchMode = computed<LaunchMode | null>(
  () => config.value?.launch.mode ?? null,
);

// 高级参数（取 launch.advanced）
const advanced = computed<AdvancedArgs | null>(
  () => config.value?.launch.advanced ?? null,
);

// 显存模式（用 LaunchMode 推算 VRAM 占用强度）
const vramLevel = computed<"high" | "low" | "novram" | "cpu" | "custom">(
  () => {
    const m = launchMode.value;
    if (m === "cpu") return "cpu";
    if (m === "gpu_high") return "high";
    if (m === "gpu_low") return "low";
    if (m === "gpu_no_vram") return "novram";
    return "custom";
  },
);

// gpu_only 禁用判断
// - Cpu 模式：禁用
// - GpuLow / GpuNo：禁用（lowvram/novram 故意 spill，强制会 OOM）
// - GpuHigh：可用
// - Custom：按配置由用户控制，但仍然提示风险
const gpuOnlyDisabled = computed(() => {
  const lv = vramLevel.value;
  return lv === "cpu" || lv === "low" || lv === "novram";
});

const gpuOnlyDisabledReason = computed(() => {
  const lv = vramLevel.value;
  if (lv === "cpu") {
    return "CPU 模式不需要 GPU 显存设置";
  }
  if (lv === "low") {
    return "lowvram 模式故意 spill 到 CPU 内存，禁用 gpu_only 开关";
  }
  if (lv === "novram") {
    return "novram 模式完全 spill 到 CPU 内存，禁用 gpu_only 开关";
  }
  return "";
});

async function setGpuOnly(value: boolean) {
  if (!advanced.value) return;
  try {
    await configStore.update({
      launch: {
        advanced: {
          ...advanced.value,
          gpu_only: value,
        },
      } as Partial<LaunchConfig>,
    });
    if (value) {
      toast.success("已启用「不使用共享显存」（--gpu-only）");
    } else {
      toast.success("已关闭「不使用共享显存」");
    }
  } catch (err) {
    toast.error("保存失败", err);
  }
}
</script>

<template>
  <NCard class="launch-advanced-panel" :bordered="true" size="small">
    <template #header>
      <NSpace align="center" :size="8">
        <span class="header-title">🚀 启动高级参数</span>
        <NTag size="small" type="info" :bordered="false">性能调优</NTag>
      </NSpace>
    </template>

    <div v-if="!advanced" class="empty-state">
      <NText depth="3">配置未加载</NText>
    </div>

    <template v-else>
      <div class="setting-row">
        <div class="setting-info">
          <div class="setting-label">
            <span class="label-main">使用共享显存</span>
            <NTag
              v-if="advanced.gpu_only"
              size="tiny"
              type="warning"
              :bordered="false"
            >
              已禁用 spill
            </NTag>
            <NTag v-else size="tiny" type="default" :bordered="false">
              默认（允许 spill）
            </NTag>
          </div>
          <div class="setting-hint">
            关闭该选项可避免在使用过量显存时由于自动通过内存弥补导致的性能降级。
            对应 ComfyUI <code>--gpu-only</code> 参数。
          </div>
        </div>
        <div class="setting-control">
          <NSwitch
            :value="advanced.gpu_only"
            :disabled="gpuOnlyDisabled"
            @update:value="setGpuOnly"
          />
        </div>
      </div>

      <NAlert
        v-if="gpuOnlyDisabled"
        type="info"
        :bordered="false"
        class="mode-conflict-warn"
      >
        ℹ {{ gpuOnlyDisabledReason }}。如需启用此开关，请先将启动模式切换为「高性能」或「自定义」。
      </NAlert>

      <NAlert
        v-else-if="advanced.gpu_only"
        type="warning"
        :bordered="false"
        class="enable-warn"
      >
        ⚠ 启用「不使用共享显存」后，若模型 + 中间结果超过显存大小，ComfyUI
        会直接报错（CUDA OOM），不会自动使用内存补足。
        建议显存 ≤ 12GB 时保持关闭。
      </NAlert>

      <NAlert
        v-else
        type="info"
        :bordered="false"
        class="default-hint"
      >
        ℹ 当前使用 ComfyUI 默认行为 — 模型在显存不够时会自动 spill 到 CPU 内存。
        性能可能因 PCIe 带宽而下降，但能跑更大的模型。
      </NAlert>

      <div class="vram-mode-row">
        <NText depth="3" style="font-size: 12px">
          当前显存模式：
          <NTag
            size="small"
            :type="vramLevel === 'high' ? 'success' : 'default'"
            :bordered="false"
          >
            {{
              vramLevel === "high"
                ? "高性能（--highvram）"
                : vramLevel === "low"
                ? "低显存（--lowvram）"
                : vramLevel === "novram"
                ? "无显存（--novram）"
                : vramLevel === "cpu"
                ? "纯 CPU（--cpu）"
                : "自定义"
            }}
          </NTag>
        </NText>
      </div>
    </template>
  </NCard>
</template>

<style scoped>
.launch-advanced-panel {
  margin-bottom: 16px;
}

.header-title {
  font-weight: 600;
}

.empty-state {
  padding: 12px;
  text-align: center;
}

.setting-row {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
  padding: 12px 4px;
  border-radius: 4px;
}

.setting-row:hover {
  background: var(--app-bg-soft, rgba(127, 127, 127, 0.04));
}

.setting-info {
  flex: 1;
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.setting-label {
  display: flex;
  align-items: center;
  gap: 8px;
}

.label-main {
  font-size: 14px;
  font-weight: 500;
}

.setting-hint {
  font-size: 12px;
  color: var(--app-text-muted, #999);
  line-height: 1.5;
}

.setting-hint code {
  background: var(--app-bg-soft, rgba(127, 127, 127, 0.1));
  padding: 1px 6px;
  border-radius: 3px;
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
  font-size: 11px;
}

.setting-control {
  flex-shrink: 0;
  display: flex;
  align-items: center;
  padding-top: 2px;
}

.mode-conflict-warn,
.enable-warn,
.default-hint {
  margin-top: 12px;
}

.vram-mode-row {
  margin-top: 12px;
  padding: 8px 12px;
  background: var(--app-bg-soft, rgba(127, 127, 127, 0.05));
  border-radius: 4px;
  display: flex;
  align-items: center;
  gap: 8px;
}
</style>
