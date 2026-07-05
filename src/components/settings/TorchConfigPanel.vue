<script setup lang="ts">
/**
 * torch 多厂商配置面板（v3.0 重构，F25）
 *
 * UI 结构：
 * - 一级 Tab 选择厂商（NVIDIA / AMD / Intel / Apple / CPU）
 * - 二级选项选择具体版本（CUDA 11.8/12.1/12.4 / ROCm 5.7/6.0/6.1 / XPU / MPS / CPU Only）
 * - 不兼容平台灰显并提示
 *
 * 行为：
 * - 打开面板时自动检测 GPU（带 5 分钟缓存）
 * - 显示"智能推荐"按钮 + 推荐变体
 * - 切换前若 ComfyUI 运行中，弹确认框
 * - 切换是长任务（30 秒-数分钟），按钮转圈
 * - 失败时保留旧版本可用
 *
 * 设计模式：
 * - **Strategy**：TorchVariant 枚举对应不同 torch wheel 索引 URL
 * - **State Machine**：idle / detecting / installing / success / failed
 *
 * 详见 `PR/03-模块设计/02-PythonEnvManager.md §X` 和 `PR/06-界面设计.md §5.3`
 */

import { ref, computed, watch, onMounted } from "vue";
import {
  NCard,
  NForm,
  NFormItem,
  NTabs,
  NTabPane,
  NRadioGroup,
  NRadio,
  NButton,
  NTag,
  NAlert,
  NSpace,
  NSpin,
  NTooltip,
  NDivider,
} from "naive-ui";
import { useConfigStore } from "@/stores/config";
import { useEnvStore } from "@/stores/env";
import { useProcessStore } from "@/stores/process";
import { useToast } from "@/composables/useToast";
import { useConfirm } from "@/composables/useConfirm";
import type {
  TorchVariant,
  TorchVendor,
  GpuInfo,
  TorchVariantOption,
} from "@/api/types";
import {
  variantLabel,
  vendorLabel,
  currentPlatform,
  groupVariantsByVendor,
  compareVariants,
  variantToKey,
  keyToVariant,
} from "@/utils/torchVariant";

const configStore = useConfigStore();
const envStore = useEnvStore();
const processStore = useProcessStore();
const toast = useToast();
const confirm = useConfirm();

const platform = currentPlatform();
const vendorGroups = computed(() => groupVariantsByVendor(platform));

// 一级 Tab 当前选中的厂商
const activeVendor = ref<TorchVendor>("nvidia_cuda");
// 二级当前选中的变体（Naive UI RadioGroup v-model 用 string key 避免对象类型不匹配）
const selectedKey = ref<string>("");
// 真实选中的变体对象（用于调用后端 API）
const selectedVariant = computed<TorchVariant | null>(() =>
  selectedKey.value ? keyToVariant(selectedKey.value) : null,
);
const installError = ref<string | null>(null);

// === 初始化：解析 Config 中的 torch_variant ===
watch(
  () => envStore.activeTorch,
  (val) => {
    if (val) {
      activeVendor.value = val.vendor;
      selectedKey.value = variantToKey(val);
    }
  },
  { immediate: true },
);

// === 打开面板时自动检测 GPU ===
onMounted(async () => {
  try {
    await envStore.detectGpus();
    await envStore.recommendTorch();
  } catch (e) {
    console.warn("[TorchConfigPanel] auto detect failed:", e);
  }
});

const currentVariant = computed(() => envStore.activeTorch);
const isCurrent = computed(() =>
  compareVariants(selectedVariant.value, currentVariant.value),
);
const gpus = computed<GpuInfo[]>(() => envStore.gpus);
const recommended = computed<TorchVariant | null>(() => envStore.recommendedTorch);

// 按 vendor 过滤 GPU
const gpusForVendor = (vendor: TorchVendor) =>
  gpus.value.filter((g) => {
    if (vendor === "nvidia_cuda") return g.vendor === "nvidia";
    if (vendor === "amd_rocm") return g.vendor === "amd";
    if (vendor === "intel_xpu") return g.vendor === "intel";
    if (vendor === "apple_silicon") return g.vendor === "apple";
    return false;
  });

async function onRefreshGpu() {
  try {
    await envStore.refreshGpuAndRecommendation();
    toast.success(`检测到 ${gpus.value.length} 个 GPU`);
  } catch (e) {
    toast.error("GPU 检测失败", e);
  }
}

async function onApplyRecommended() {
  if (!recommended.value) return;
  activeVendor.value = recommended.value.vendor;
  selectedKey.value = variantToKey(recommended.value);
  await onApply();
}

async function onApply() {
  if (!selectedVariant.value) {
    toast.warn("请先选择 torch 变体");
    return;
  }
  if (isCurrent.value) {
    toast.info("已选中版本与当前一致，无需切换");
    return;
  }

  // 运行中需确认
  if (processStore.isAlive) {
    const ok = await confirm.warn(
      "停止 ComfyUI",
      `ComfyUI 正在运行 (PID: ${processStore.pid || "?"})，切换 torch 需要先停止进程，是否继续？`,
    );
    if (!ok) return;
    try {
      await processStore.stop();
    } catch (e) {
      toast.error("停止失败", e);
      return;
    }
  }

  // 二次确认
  const ok = await confirm.warn(
    "确认切换 torch",
    `将切换到 ${variantLabel(selectedVariant.value)}，预计耗时 1-5 分钟。是否继续？`,
  );
  if (!ok) return;

  installError.value = null;

  try {
    await envStore.changeTorchVariant(selectedVariant.value);
    toast.success(`torch 已切换到 ${variantLabel(selectedVariant.value)}`);
  } catch (e) {
    installError.value = e instanceof Error ? e.message : String(e);
    toast.error("torch 切换失败", e);
  }
}
</script>

<template>
  <NCard class="torch-panel" :bordered="true" size="small">
    <template #header>
      <NSpace align="center" :size="12">
        <span class="header-title">🔥 torch 配置</span>
        <NTag v-if="currentVariant" size="small" type="success">
          当前：{{ variantLabel(currentVariant) }}
        </NTag>
        <NTag v-else size="small" type="warning">未配置</NTag>
      </NSpace>
    </template>

    <NForm label-placement="top" :show-feedback="false" size="small">
      <!-- GPU 检测结果展示 -->
      <div class="gpu-section">
        <NSpace align="center" :size="8" style="margin-bottom: 8px">
          <span class="section-label">检测到 GPU：</span>
          <NSpin v-if="envStore.detectingGpus" size="small" />
          <span v-else-if="gpus.length === 0" class="muted">无</span>
          <NTag
            v-for="(gpu, idx) in gpus"
            :key="idx"
            size="small"
            :type="gpu.vendor === 'nvidia' ? 'success' : 'info'"
          >
            {{ gpu.model }}
            <span v-if="gpu.vram_mb"> · {{ Math.round(gpu.vram_mb / 1024) }} GB</span>
            <span v-if="gpu.cuda_version"> · CUDA {{ gpu.cuda_version }}</span>
          </NTag>
          <NButton size="tiny" @click="onRefreshGpu" :loading="envStore.detectingGpus">
            重新检测
          </NButton>
        </NSpace>
        <!-- 智能推荐 -->
        <NSpace v-if="recommended" align="center" :size="8" style="margin-bottom: 12px">
          <span class="muted">推荐：</span>
          <NTag size="small" type="warning">{{ variantLabel(recommended) }}</NTag>
          <NButton
            size="tiny"
            type="primary"
            ghost
            :disabled="isCurrent && compareVariants(recommended, currentVariant)"
            @click="onApplyRecommended"
          >
            一键应用推荐
          </NButton>
        </NSpace>
      </div>

      <NDivider style="margin: 8px 0 12px 0" />

      <!-- 一级 Tab：选厂商 -->
      <NTabs
        v-model:value="activeVendor"
        type="line"
        animated
        :bar-width="20"
        size="small"
      >
        <NTabPane
          v-for="(options, vendor) in vendorGroups"
          :key="vendor"
          :name="vendor"
          :tab="vendorLabel(vendor as TorchVendor)"
        >
          <!-- 二级选项：选具体版本 -->
          <NRadioGroup
            v-model:value="selectedKey"
            :disabled="envStore.switchingTorch"
          >
            <NSpace vertical :size="8">
              <div
                v-for="opt in options"
                :key="variantToKey(opt.variant)"
                class="variant-option"
                :class="{ 'is-incompatible': !opt.compatible }"
              >
                <NTooltip v-if="!opt.compatible" placement="right">
                  <template #trigger>
                    <NRadio
                      :value="variantToKey(opt.variant)"
                      :disabled="true || envStore.switchingTorch"
                    >
                      {{ opt.label }}
                    </NRadio>
                  </template>
                  {{ opt.hint }}
                </NTooltip>
                <NRadio
                  v-else
                  :value="variantToKey(opt.variant)"
                  :disabled="envStore.switchingTorch"
                >
                  {{ opt.label }}
                </NRadio>
                <!-- 该厂商的 GPU 信息（仅显示当前选中的厂商） -->
                <span
                  v-if="
                    opt.variant.vendor === activeVendor &&
                    gpusForVendor(activeVendor).length > 0
                  "
                  class="vendor-gpu-hint muted"
                >
                  {{ gpusForVendor(activeVendor).map((g) => g.model).join("、") }}
                </span>
              </div>
            </NSpace>
          </NRadioGroup>
        </NTabPane>
      </NTabs>

      <NAlert type="info" :bordered="false" class="info-alert">
        ℹ 切换前会自动停止 ComfyUI 进程；切换失败时旧版本 torch 仍可用。
      </NAlert>

      <div class="action-row">
        <NButton
          type="primary"
          :loading="envStore.switchingTorch"
          :disabled="envStore.switchingTorch || isCurrent || !selectedVariant"
          @click="onApply"
        >
          {{ envStore.switchingTorch ? "安装中..." : "应用" }}
        </NButton>
      </div>
    </NForm>

    <NAlert
      v-if="installError"
      type="error"
      :bordered="false"
      class="error-alert"
    >
      切换失败：{{ installError }}
      <NButton size="tiny" @click="installError = null">关闭</NButton>
    </NAlert>
  </NCard>
</template>

<style scoped>
.torch-panel {
  margin-bottom: 16px;
}

.header-title {
  font-weight: 600;
}

.gpu-section {
  margin-top: 4px;
}

.section-label {
  font-size: 13px;
  color: var(--app-text-muted, #999);
}

.muted {
  color: var(--app-text-muted, #999);
  font-size: 12px;
}

.variant-option {
  display: flex;
  align-items: center;
  gap: 8px;
}

.variant-option.is-incompatible {
  opacity: 0.5;
}

.vendor-gpu-hint {
  margin-left: 8px;
  font-size: 12px;
}

.info-alert {
  margin-top: 16px;
}

.action-row {
  margin-top: 16px;
  display: flex;
  justify-content: flex-end;
}

.error-alert {
  margin-top: 12px;
}
</style>
