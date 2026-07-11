<!--
  TorchInstallFallbackDialog - torch 安装失败 + fallback 建议弹窗

  v3.x Phase 2：捕获 `EnvError::TorchIncompatible` 错误时显示。
  后端返回的字段：
  - `attempted`：用户尝试的 CUDA 版本
  - `reason`：失败原因（pip/uv stderr 摘要）
  - `fallback`：推荐的降级版本（None = 没有合适的 fallback，建议改 CPU）

  3 个选项：
  - "切换到 {fallback} 重试"（有 fallback 时显示）
  - "切换到 CPU"（任何时候都显示）
  - "取消" → 回到设置手动改
-->
<script setup lang="ts">
import { computed, ref } from "vue";
import { NAlert, NCode, NSpace, NTag, NText } from "naive-ui";
import { storeToRefs } from "pinia";
import { useConfigStore } from "@/stores/config";
import { useToast } from "@/composables/useToast";
import router from "@/router";

interface TorchIncompatibleInfo {
  attempted: string;
  reason: string;
  fallback: string | null;
}

const info = ref<TorchIncompatibleInfo | null>(null);
const showDialog = ref(false);

const configStore = useConfigStore();
const { config } = storeToRefs(configStore);
const toast = useToast();

const hasFallback = computed(() => !!info.value?.fallback);

const fallbackLabel = computed(() => {
  if (!info.value?.fallback) return "";
  const m = info.value.fallback.toLowerCase();
  if (m.includes("cu118")) return "CUDA 11.8 (cu118)";
  if (m.includes("cu126")) return "CUDA 12.6 (cu126)";
  if (m.includes("cu128")) return "CUDA 12.8 (cu128)";
  if (m.includes("cu130")) return "CUDA 13.0 (cu130)";
  return info.value.fallback;
});

const attemptedLabel = computed(() => {
  if (!info.value?.attempted) return "";
  return info.value.attempted;
});

/**
 * 由调用方（useEnvInstaller 等）触发
 *
 * @example
 *   const diag.value = showTorchFallback(err.message);
 */
function show(errMsg: string): boolean {
  // 解析后端错误："torch 安装失败 (尝试 Cu128): xxx"
  const m = errMsg.match(/torch 安装失败 \(尝试 ([^)]+)\):([\s\S]*)/);
  if (!m) return false;

  // 尝试从 stderr 末尾提取 fallback 提示（如果有）
  // 实际上后端把 fallback 通过 EnvError::TorchIncompatible.fallback 单独传，
  // 但 Tauri 序列化为 string 时只保留 to_string()。所以我们直接从 stderr 中匹配。
  const reason = m[2].trim();
  const fallbackMatch = reason.match(/推荐[:：]\s*(\w+)/);
  const fallback = fallbackMatch ? fallbackMatch[1] : null;

  info.value = {
    attempted: m[1].trim(),
    reason,
    fallback,
  };
  showDialog.value = true;
  return true;
}

function close() {
  showDialog.value = false;
  info.value = null;
}

/**
 * 选项 1：切换到 fallback 重试
 */
async function useFallback() {
  if (!info.value?.fallback) return;
  const cuda = info.value.fallback.toLowerCase();
  try {
    // 直接修改 config.torch.cuda_version（后续重装流程会用到）
    if (config.value) {
      await configStore.update({
        torch: { cuda_version: cuda as never },
      });
    }
    toast.success(`已切换到 ${fallbackLabel.value}，请重新触发安装`);
    close();
    router.push({ name: "settings" });
  } catch (err) {
    toast.error("切换失败", err);
  }
}

/**
 * 选项 2：切换到 CPU
 */
async function useCpu() {
  try {
    if (config.value) {
      await configStore.update({
        torch: { cuda_version: "cpu" },
      });
    }
    toast.success("已切换到 CPU 模式，请重新触发安装");
    close();
    router.push({ name: "settings" });
  } catch (err) {
    toast.error("切换失败", err);
  }
}

/**
 * 选项 3：取消
 */
function cancel() {
  close();
  router.push({ name: "settings" });
}

defineExpose({ show });
</script>

<template>
  <n-modal
    v-model:show="showDialog"
    :mask-closable="false"
    preset="card"
    style="max-width: 640px"
    title="torch 安装失败"
  >
    <n-alert
      v-if="info"
      type="error"
      title="检测到 GPU 与当前 CUDA 版本不兼容"
      style="margin-bottom: 16px"
    >
      尝试安装的版本：
      <n-tag type="warning">{{ attemptedLabel }}</n-tag>
    </n-alert>

    <div v-if="info" style="margin-bottom: 16px">
      <n-text strong>失败原因：</n-text>
      <n-code
        :code="info.reason"
        language="text"
        style="
          margin-top: 8px;
          max-height: 200px;
          overflow: auto;
          display: block;
          white-space: pre-wrap;
        "
      />
    </div>

    <n-alert
      v-if="hasFallback"
      type="info"
      title="建议降级安装"
      style="margin-bottom: 16px"
    >
      检测到您的 GPU 驱动可能不支持 <n-tag type="warning">{{ attemptedLabel }}</n-tag>，
      建议切换到 <n-tag type="success">{{ fallbackLabel }}</n-tag> 重试。
    </n-alert>

    <n-alert v-else type="warning" title="无可用降级版本">
      未检测到合适的降级 CUDA 版本，建议切换到 CPU 模式或手动检查驱动兼容性。
    </n-alert>

    <template #footer>
      <n-space justify="end">
        <n-button @click="cancel">取消</n-button>
        <n-button @click="useCpu">切换到 CPU</n-button>
        <n-button v-if="hasFallback" type="primary" @click="useFallback">
          切换到 {{ fallbackLabel }} 重试
        </n-button>
      </n-space>
    </template>
  </n-modal>
</template>
