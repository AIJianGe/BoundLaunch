<script setup lang="ts">
/**
 * torch CUDA 配置面板
 *
 * 详见 `PR/06-界面设计.md §5.3 设置页 - torch 配置`
 *
 * 字段：cuda_version（cpu / cu118 / cu121 / cu124）
 *
 * 行为：
 * - 切换前若 ComfyUI 运行中，弹确认框，用户确认后 stop()
 * - 切换是长任务（30 秒-数分钟），按钮转圈
 * - 失败时保留旧版本可用
 *
 * 设计模式：
 * - **Strategy**：CudaVersion 枚举对应不同 torch wheel 索引 URL
 * - **State Machine**：idle / installing / success / failed
 */

import { ref, computed, watch } from "vue";
import {
  NCard,
  NForm,
  NFormItem,
  NRadioGroup,
  NRadio,
  NButton,
  NTag,
  NAlert,
  NSpace,
} from "naive-ui";
import { useConfigStore } from "@/stores/config";
import { useEnvStore } from "@/stores/env";
import { useProcessStore } from "@/stores/process";
import { useToast } from "@/composables/useToast";
import { useConfirm } from "@/composables/useConfirm";
import type { CudaVersion } from "@/api/types";

const configStore = useConfigStore();
const envStore = useEnvStore();
const processStore = useProcessStore();
const toast = useToast();
const confirm = useConfirm();

const cudaOptions: Array<{ value: CudaVersion; label: string; hint: string }> = [
  { value: "cpu", label: "CPU", hint: "无 GPU 或仅测试" },
  { value: "cu118", label: "CUDA 11.8", hint: "兼容旧驱动" },
  { value: "cu121", label: "CUDA 12.1（推荐）", hint: "RTX 30/40 系列推荐" },
  { value: "cu124", label: "CUDA 12.4", hint: "最新 CUDA" },
];

const selectedCuda = ref<CudaVersion>("cu121");
const installing = ref(false);
const installError = ref<string | null>(null);

// 同步 store → 本地
watch(
  () => configStore.config?.torch.cuda_version,
  (val) => {
    if (val) selectedCuda.value = val;
  },
  { immediate: true },
);

const currentCuda = computed(() => configStore.config?.torch.cuda_version || "未配置");
const currentTorchVersion = computed(
  () => envStore.envInfo?.torch_version || "未安装",
);
const isCurrent = computed(() => selectedCuda.value === currentCuda.value);

async function onApply() {
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
    "确认切换 torch CUDA",
    `将卸载当前 torch 并安装 ${selectedCuda.value} 版本，预计耗时 1-5 分钟。是否继续？`,
  );
  if (!ok) return;

  installing.value = true;
  installError.value = null;

  try {
    // 先更新 config（后端会感知）
    await configStore.update({
      torch: { cuda_version: selectedCuda.value },
    });
    // 调用后端安装 torch
    await envStore.installTorch(selectedCuda.value);
    toast.success(`torch 已切换到 ${selectedCuda.value}`);
  } catch (e) {
    installError.value = e instanceof Error ? e.message : String(e);
    toast.error("torch 切换失败", e);
  } finally {
    installing.value = false;
  }
}
</script>

<template>
  <NCard class="torch-panel" :bordered="true" size="small">
    <template #header>
      <span class="header-title">🔥 torch 配置</span>
    </template>

    <NForm label-placement="top" :show-feedback="false" size="small">
      <NFormItem label="CUDA 版本">
        <NRadioGroup v-model:value="selectedCuda" :disabled="installing">
          <NSpace>
            <div
              v-for="opt in cudaOptions"
              :key="opt.value"
              class="cuda-option"
            >
              <NRadio :value="opt.value" :disabled="installing">
                {{ opt.label }}
              </NRadio>
              <span class="cuda-hint">{{ opt.hint }}</span>
            </div>
          </NSpace>
        </NRadioGroup>
      </NFormItem>

      <div class="current-info">
        <NSpace size="small" align="center">
          <span class="info-label">当前 CUDA:</span>
          <NTag size="small" :type="isCurrent ? 'success' : 'warning'">
            {{ currentCuda }}
          </NTag>
        </NSpace>
        <NSpace size="small" align="center">
          <span class="info-label">torch 版本:</span>
          <NTag size="small" type="info">{{ currentTorchVersion }}</NTag>
        </NSpace>
      </div>

      <NAlert type="info" :bordered="false" class="info-alert">
        ℹ 切换前会自动停止 ComfyUI 进程；切换失败时旧版本 torch 仍可用。
      </NAlert>

      <div class="action-row">
        <NButton
          type="primary"
          :loading="installing"
          :disabled="installing || isCurrent"
          @click="onApply"
        >
          {{ installing ? "安装中..." : "应用" }}
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

.cuda-option {
  display: flex;
  align-items: center;
  gap: 8px;
}

.cuda-hint {
  font-size: 12px;
  color: var(--app-text-muted, #999);
}

.current-info {
  display: flex;
  gap: 24px;
  margin-top: 12px;
  font-size: 13px;
}

.info-label {
  color: var(--app-text-muted, #999);
}

.info-alert {
  margin-top: 12px;
}

.action-row {
  margin-top: 12px;
  display: flex;
  justify-content: flex-end;
}

.error-alert {
  margin-top: 12px;
}
</style>
