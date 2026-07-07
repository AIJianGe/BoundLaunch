<script setup lang="ts">
/**
 * transformers 版本切换面板（v3.7 新增）
 *
 * 详见 `PR/06-界面设计.md §5.3 设置页 - transformers 版本切换`
 *
 * 功能：
 * - 显示当前安装的 transformers 版本（从 dependencies 列表提取）
 * - 列出所有可用版本（从后端 TransformersVersionIndex 获取，含 PyPI 最新版）
 * - 5.x 标记为「实验」（破坏性 API 变更，红色 tag）
 * - 切换到指定版本（异步任务，通过 TaskScheduler 调度，复用 F32 waitForTask 模式）
 * - 恢复到默认版本（按 ComfyUI requirements.txt 约束选最新 4.x）
 *
 * 异步方案（v3.7 复用 v3.6 CancellationToken + TaskScheduler）：
 * - switchTransformers / restoreTransformersDefault 返回 task_id
 * - store 内部 waitForTask 等待终态
 * - 后端 emit RequirementsInstalled 让 env cache 失效
 *
 * 设计模式：
 * - **Facade**：通过 envStore 访问后端
 * - **State Machine**：idle / switching / restoring
 */

import { ref, computed, onMounted } from "vue";
import { NCard, NSelect, NButton, NTag, NAlert } from "naive-ui";
import { useEnvStore } from "@/stores/env";
import { useToast } from "@/composables/useToast";
import { useConfirm } from "@/composables/useConfirm";

const envStore = useEnvStore();
const toast = useToast();
const confirm = useConfirm();

const selectedVersion = ref<string | null>(null);

// 当前安装的 transformers 版本（从 dependencies 列表查找）
const currentVersion = computed(() => {
  const dep = envStore.dependencies.find((d) => d.name === "transformers");
  return dep?.version ?? null;
});

// 当前版本是否为 5.x 实验版
const isCurrentExperimental = computed(
  () => currentVersion.value?.startsWith("5.") ?? false,
);

// 版本选项（5.x 加「实验」标签）
const versionOptions = computed(() => {
  return envStore.transformersVersions.map((v) => {
    const isExperimental = v.startsWith("5.");
    return {
      label: isExperimental ? `${v}（实验）` : v,
      value: v,
    };
  });
});

// 选中的版本是否为 5.x（实验）
const isSelectedExperimental = computed(
  () => selectedVersion.value?.startsWith("5.") ?? false,
);

// 是否与当前版本相同
const isCurrent = computed(
  () =>
    selectedVersion.value !== null &&
    selectedVersion.value === currentVersion.value,
);

// 是否有任何任务进行中
const busy = computed(
  () => envStore.switchingTransformers || envStore.restoringTransformers,
);

onMounted(async () => {
  // 加载版本列表（如未加载；后端启动时已 spawn_refresh，此处取缓存）
  if (envStore.transformersVersions.length === 0) {
    await envStore.loadTransformersVersions();
  }
  // 默认选中当前版本
  if (currentVersion.value) {
    selectedVersion.value = currentVersion.value;
  } else if (envStore.transformersVersions.length > 0) {
    // 无当前版本时默认选第一个（最新 4.x，后端列表已降序）
    const firstStable = envStore.transformersVersions.find(
      (v) => !v.startsWith("5."),
    );
    selectedVersion.value = firstStable ?? envStore.transformersVersions[0];
  }
});

async function onSwitch() {
  if (!selectedVersion.value) return;
  if (isCurrent.value) {
    toast.info("已选中版本与当前一致，无需切换");
    return;
  }

  // 5.x 实验版本：二次确认 + 警告破坏性 API 变更
  if (isSelectedExperimental.value) {
    const ok = await confirm.danger(
      "切换到实验版本",
      `transformers ${selectedVersion.value} 是 5.x 实验版本，存在破坏性 API 变更，可能导致 ComfyUI 或自定义节点不兼容。\n\n是否继续？`,
    );
    if (!ok) return;
  } else {
    const ok = await confirm.warn(
      "切换 transformers 版本",
      `将执行 uv pip install transformers==${selectedVersion.value}，可能耗时 10s-2min。是否继续？`,
    );
    if (!ok) return;
  }

  try {
    await envStore.switchTransformers(selectedVersion.value);
    toast.success(`transformers 已切换到 ${selectedVersion.value}`);
  } catch (e) {
    toast.error("切换失败", e);
  }
}

async function onRestoreDefault() {
  const ok = await confirm.warn(
    "恢复默认版本",
    "将根据 ComfyUI requirements.txt 约束选择最新兼容的 4.x 版本切换。\n\n是否继续？",
  );
  if (!ok) return;

  try {
    const version = await envStore.restoreTransformersDefault();
    selectedVersion.value = version;
    toast.success(`transformers 已恢复到默认版本: ${version}`);
  } catch (e) {
    toast.error("恢复失败", e);
  }
}
</script>

<template>
  <NCard class="transformers-panel" :bordered="true" size="small">
    <template #header>
      <span class="header-title">🤗 transformers 版本切换</span>
    </template>

    <div class="form-row">
      <div class="form-item">
        <label class="form-label">目标版本</label>
        <NSelect
          v-model:value="selectedVersion"
          :options="versionOptions"
          :loading="envStore.transformersVersions.length === 0"
          :disabled="busy"
          filterable
          placeholder="选择版本"
        />
      </div>
      <div class="form-item">
        <label class="form-label">当前版本</label>
        <NTag
          v-if="currentVersion"
          size="small"
          :type="isCurrentExperimental ? 'warning' : 'success'"
        >
          {{ currentVersion }}
          <template v-if="isCurrentExperimental">（实验）</template>
        </NTag>
        <NTag v-else size="small" type="default">未安装</NTag>
      </div>
    </div>

    <NAlert
      v-if="isSelectedExperimental"
      type="warning"
      :bordered="false"
      class="warning-alert"
    >
      ⚠ transformers 5.x 是实验版本，存在破坏性 API 变更，可能导致 ComfyUI 或自定义节点不兼容。
    </NAlert>

    <NAlert v-else type="info" :bordered="false" class="info-alert">
      ℹ 列表包含所有 PyPI 发布的版本。5.x 标记为「实验」（破坏性 API 变更）。
      「恢复默认版本」将按 ComfyUI requirements.txt 约束选最新 4.x。
    </NAlert>

    <div class="action-row">
      <NButton
        :loading="envStore.restoringTransformers"
        :disabled="busy"
        @click="onRestoreDefault"
      >
        {{ envStore.restoringTransformers ? "恢复中..." : "恢复默认版本" }}
      </NButton>
      <NButton
        type="primary"
        :loading="envStore.switchingTransformers"
        :disabled="busy || isCurrent"
        @click="onSwitch"
      >
        {{ envStore.switchingTransformers ? "切换中..." : "应用" }}
      </NButton>
    </div>
  </NCard>
</template>

<style scoped>
.transformers-panel {
  margin-bottom: 16px;
}

.header-title {
  font-weight: 600;
}

.form-row {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 12px 16px;
}

.form-item {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.form-label {
  font-size: 12px;
  color: var(--app-text-muted, #999);
}

.warning-alert {
  margin-top: 12px;
}

.info-alert {
  margin-top: 12px;
}

.action-row {
  margin-top: 12px;
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}
</style>
