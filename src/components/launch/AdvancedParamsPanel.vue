<script setup lang="ts">
/**
 * §3.7 高级参数折叠区
 *
 * 详见 `PR/06-界面设计.md §3.7 高级参数折叠区`
 *
 * 数据来源：`PR/05-依赖与启动参数.md §3.2`
 *
 * 字段（AdvancedArgs）：
 * - use_split_cross_attention
 * - use_pytorch_cross_attention
 * - force_fp32
 * - fp16_vae
 * - bf16_vae
 * - no_half
 * - no_half_vae
 * - directml
 * - gpu_only（v3.x 新增："使用共享显存"开关的反面 → --gpu-only）
 *
 * 行为：
 * - 默认折叠，点击「高级参数」展开
 * - 任一复选框变更时立即调用 configStore.update（部分更新 advanced）
 * - directml 与 no_half 通常互斥（不强制，仅 UI 提示）
 * - gpu_only 与 LaunchMode 联动：GpuLow/GpuNo 时禁灰（lowvram/novram 故意 spill）
 */

import { computed } from "vue";
import {
  NCard,
  NCollapse,
  NCollapseItem,
  NCheckbox,
  NSpace,
  NAlert,
} from "naive-ui";
import { useConfigStore } from "@/stores/config";
import { useToast } from "@/composables/useToast";
import type { AdvancedArgs } from "@/api/types";

const configStore = useConfigStore();
const toast = useToast();

const advanced = computed<AdvancedArgs | null>(
  () => configStore.config?.launch.advanced ?? null,
);

const advancedOptions: Array<{
  key: keyof AdvancedArgs;
  label: string;
  hint: string;
}> = [
  { key: "use_split_cross_attention", label: "--use-split-cross-attention", hint: "使用 split 交叉注意力（显存占用低）" },
  { key: "use_pytorch_cross_attention", label: "--use-pytorch-cross-attention", hint: "使用 PyTorch 2.x scaled_dot_product" },
  { key: "force_fp32", label: "--force-fp32", hint: "强制使用 fp32（精度高但显存大）" },
  { key: "fp16_vae", label: "--fp16-vae", hint: "VAE 使用 fp16（节省显存）" },
  { key: "bf16_vae", label: "--bf16-vae", hint: "VAE 使用 bf16" },
  { key: "no_half", label: "--no-half", hint: "禁用半精度（兼容旧显卡）" },
  { key: "no_half_vae", label: "--no-half-vae", hint: "VAE 不使用半精度" },
  { key: "directml", label: "--directml", hint: "使用 DirectML（Windows 通用 GPU）" },
  // v3.x：使用共享显存开关的反面
  // - 开启：传 --gpu-only → ComfyUI 强制全部在 GPU 显存
  // - 关闭（默认）：ComfyUI 行为不变（spill 到 CPU 内存）
  { key: "gpu_only", label: "--gpu-only", hint: "禁用 spill 到 CPU 内存（强制全部在 GPU，OOM 时报错）" },
];

async function updateAdvanced(key: keyof AdvancedArgs, value: boolean) {
  if (!advanced.value) return;
  try {
    await configStore.update({
      launch: {
        advanced: {
          ...advanced.value,
          [key]: value,
        },
      },
    });
  } catch (e) {
    toast.error("保存失败", e);
  }
}

const isDirectmlNoHalfConflict = computed(() => {
  if (!advanced.value) return false;
  return advanced.value.directml && advanced.value.no_half;
});
</script>

<template>
  <NCard class="advanced-params" :bordered="true" size="small">
    <NCollapse :default-expanded-names="[]">
      <NCollapseItem title="⚙️ 高级参数" name="advanced">
        <div v-if="!advanced" class="empty-state">
          <span class="hint">配置未加载</span>
        </div>

        <template v-else>
          <div class="checkbox-grid">
            <div
              v-for="opt in advancedOptions"
              :key="opt.key"
              class="checkbox-item"
            >
              <NCheckbox
                :checked="advanced[opt.key]"
                @update:checked="(v) => updateAdvanced(opt.key, v)"
              >
                <div class="checkbox-label">
                  <span class="arg-text">{{ opt.label }}</span>
                  <span class="arg-hint">{{ opt.hint }}</span>
                </div>
              </NCheckbox>
            </div>
          </div>

          <NAlert
            v-if="isDirectmlNoHalfConflict"
            type="warning"
            :bordered="false"
            class="conflict-warn"
          >
            ⚠ DirectML 与 --no-half 通常不应同时启用，可能导致显存占用异常。
          </NAlert>

          <div class="advanced-tip">
            ℹ 高级参数仅适用于有经验的用户；不熟悉请保持默认（全部关闭）。
            修改后会立即生效并反映到命令预览。
          </div>
        </template>
      </NCollapseItem>
    </NCollapse>
  </NCard>
</template>

<style scoped>
.advanced-params {
  margin-bottom: 16px;
}

.empty-state {
  padding: 12px;
  text-align: center;
}

.hint {
  color: var(--app-text-muted, #999);
  font-size: 13px;
}

.checkbox-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
  gap: 8px 16px;
}

.checkbox-item {
  padding: 4px 8px;
  border-radius: 4px;
}

.checkbox-item:hover {
  background: var(--app-bg-soft, rgba(127, 127, 127, 0.06));
}

.checkbox-label {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.arg-text {
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
  font-size: 12px;
  font-weight: 500;
}

.arg-hint {
  font-size: 11px;
  color: var(--app-text-muted, #999);
}

.conflict-warn {
  margin-top: 12px;
}

.advanced-tip {
  margin-top: 12px;
  padding: 8px 12px;
  background: var(--app-bg-soft, rgba(127, 127, 127, 0.05));
  border-radius: 4px;
  font-size: 12px;
  color: var(--app-text-muted, #999);
}
</style>
