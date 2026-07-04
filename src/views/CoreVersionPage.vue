<script setup lang="ts">
/**
 * 核心版本页
 *
 * 详见 `PR/06-界面设计.md §5.1 核心版本页`
 *
 * 区块：
 * 1. 当前版本大字显示（顶部居中）
 * 2. 检查更新按钮（右上角）
 * 3. 版本下拉列表（stable / 预发布标记）
 * 4. 切换到选中版本按钮
 * 5. 工作区状态提示（黄色警告条）
 * 6. 运行中状态指示（禁用版本下拉 + 顶部黄色提示条）
 * 7. 切换后依赖需更新警告条（黄色 + [立即安装]）
 * 8. 切换后插件兼容性提示（蓝色信息条）
 *
 * 状态：
 * - loading / ready / running / switching / dirty / requirements_mismatch / plugin_compat
 *
 * 设计模式：
 * - **State Machine**：UI 状态基于 coreStore + processStore 派生
 * - **Strategy**：stable 优先排序
 * - **Facade**：本页面整合 core + process + env store
 */

import { ref, computed, onMounted } from "vue";
import {
  NCard,
  NButton,
  NSelect,
  NTag,
  NAlert,
  NSpace,
  NSpin,
  NEmpty,
  NTooltip,
} from "naive-ui";
import { useCoreStore } from "@/stores/core";
import { useProcessStore } from "@/stores/process";
import { useEnvStore } from "@/stores/env";
import { useToast } from "@/composables/useToast";
import { useConfirm } from "@/composables/useConfirm";

const coreStore = useCoreStore();
const processStore = useProcessStore();
const envStore = useEnvStore();
const toast = useToast();
const confirm = useConfirm();

const selectedRef = ref<string | null>(null);
const switching = ref(false);

const isCloned = computed(() => coreStore.isCloned);
const currentVersion = computed(() => coreStore.currentVersion);
const tags = computed(() => coreStore.tags);
const loading = computed(() => coreStore.loading);

/** 版本下拉选项（stable 优先排序，标记推荐/当前） */
const versionOptions = computed(() => {
  const opts = tags.value
    .filter((t) => t.is_version)
    .map((t) => ({
      label: t.name === currentVersion.value ? `${t.name} (当前)` : t.name,
      value: t.name,
      disabled: t.name === currentVersion.value,
    }));
  return opts;
});

const isRunning = computed(() => processStore.isAlive);
const requirementsMismatch = computed(() => coreStore.requirementsMismatch);

const switchButtonLabel = computed(() => {
  if (switching.value) return "切换中...";
  if (!selectedRef.value) return "请选择版本";
  if (selectedRef.value === currentVersion.value) return "已是当前版本";
  return "切换到选中版本";
});

const canSwitch = computed(() => {
  return (
    !switching.value &&
    !isRunning.value &&
    selectedRef.value !== null &&
    selectedRef.value !== currentVersion.value
  );
});

onMounted(async () => {
  if (!coreStore.status) {
    try {
      await coreStore.refresh();
    } catch (e) {
      console.warn("core refresh:", e);
    }
  }
});

async function onCheckUpdates() {
  try {
    await coreStore.refresh();
    if (coreStore.hasUpdates) {
      toast.success("检测到新版本可用");
    } else {
      toast.info("已是最新版本");
    }
  } catch (e) {
    toast.error("检查更新失败", e);
  }
}

async function onCheckout() {
  if (!selectedRef.value || !canSwitch.value) return;

  // 二次确认（切换会自动 stash 用户改动）
  const ok = await confirm.warn(
    "切换 ComfyUI 版本",
    `将切换到 ${selectedRef.value}，工作区改动会自动 stash。是否继续？`,
  );
  if (!ok) return;

  switching.value = true;
  try {
    await coreStore.checkout(selectedRef.value);
    toast.success(`已切换到 ${selectedRef.value}`);
    selectedRef.value = null;
  } catch (e) {
    toast.error("切换失败", e);
  } finally {
    switching.value = false;
  }
}

async function onInstallRequirements() {
  // TODO: 后端 install_requirements 命令待接入（PythonEnvManager.install_requirements）
  // 本期仅 toast 提示
  toast.info("开始安装 requirements.txt，请查看任务进度中心");
}

async function onClone() {
  const ok = await confirm.warn(
    "克隆 ComfyUI 仓库",
    "将从 github.com/comfyanonymous/ComfyUI 克隆到配置的根目录，是否继续？",
  );
  if (!ok) return;
  try {
    await coreStore.clone();
    toast.success("ComfyUI 仓库克隆完成");
  } catch (e) {
    toast.error("克隆失败", e);
  }
}
</script>

<template>
  <div class="core-version-page">
    <!-- 未克隆状态 -->
    <NCard v-if="!isCloned && !loading" class="not-cloned" :bordered="true" size="small">
      <NEmpty description="ComfyUI 仓库未克隆" size="medium">
        <template #extra>
          <NSpace vertical align="center" :size="12">
            <span class="hint">克隆后将自动检测可用版本</span>
            <NButton type="primary" @click="onClone">克隆 ComfyUI 仓库</NButton>
          </NSpace>
        </template>
      </NEmpty>
    </NCard>

    <!-- 加载中 -->
    <NCard v-else-if="loading && !currentVersion" class="loading-card" :bordered="true" size="small">
      <div class="loading-state">
        <NSpin size="medium" />
        <span class="hint">加载版本信息...</span>
      </div>
    </NCard>

    <template v-else>
      <!-- 当前版本大字显示 + 检查更新按钮 -->
      <NCard class="version-header" :bordered="true" size="small">
        <div class="version-row">
          <div class="version-info">
            <div class="version-label">当前版本</div>
            <div class="version-text">{{ currentVersion || "未知" }}</div>
            <NTag v-if="currentVersion" size="small" type="success">stable</NTag>
          </div>
          <NButton
            size="small"
            :loading="loading"
            @click="onCheckUpdates"
          >
            🔄 检查更新
          </NButton>
        </div>
      </NCard>

      <!-- 运行中提示 -->
      <NAlert
        v-if="isRunning"
        type="warning"
        :bordered="false"
        class="running-alert"
      >
        ⚠ ComfyUI 运行中，请先停止进程再切换版本。
        <NButton size="tiny" type="warning" @click="processStore.stop()">
          停止并切换
        </NButton>
      </NAlert>

      <!-- 切换后依赖需更新提示 -->
      <NAlert
        v-if="requirementsMismatch"
        type="warning"
        :bordered="false"
        class="requirements-alert"
      >
        ⚠ 检测到依赖需更新（切换版本后）
        <NButton size="tiny" type="warning" @click="onInstallRequirements">
          立即安装
        </NButton>
      </NAlert>

      <!-- 切换后插件兼容性提示 -->
      <NAlert
        v-if="currentVersion && !requirementsMismatch"
        type="info"
        :bordered="false"
        class="plugin-alert"
      >
        ℹ 版本切换后请观察插件是否正常工作；如插件报错可在「插件管理页」禁用对应插件。
      </NAlert>

      <!-- 版本选择 + 切换按钮 -->
      <NCard class="version-select" :bordered="true" size="small">
        <template #header>
          <span class="header-title">选择目标版本</span>
        </template>

        <div class="select-row">
          <NSelect
            v-model:value="selectedRef"
            :options="versionOptions"
            :disabled="isRunning || switching"
            placeholder="选择目标版本（stable 优先）"
            filterable
          />
          <NButton
            type="primary"
            :loading="switching"
            :disabled="!canSwitch"
            @click="onCheckout"
          >
            {{ switchButtonLabel }}
          </NButton>
        </div>

        <NTooltip placement="top">
          <template #trigger>
            <div class="info-tip">
              ℹ 版本切换会自动 stash 用户改动；切换后异步检查依赖兼容性。
            </div>
          </template>
          详见 03-模块设计/03-CoreManager.md §4 stash 机制
        </NTooltip>
      </NCard>
    </template>
  </div>
</template>

<style scoped>
.core-version-page {
  padding: 16px;
  max-width: 1200px;
  margin: 0 auto;
}

.not-cloned,
.loading-card,
.version-header,
.version-select {
  margin-bottom: 16px;
}

.loading-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 12px;
  padding: 48px 0;
}

.hint {
  font-size: 13px;
  color: var(--app-text-muted, #999);
}

.version-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
}

.version-info {
  display: flex;
  align-items: baseline;
  gap: 12px;
}

.version-label {
  font-size: 13px;
  color: var(--app-text-muted, #999);
}

.version-text {
  font-size: 24px;
  font-weight: 700;
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
}

.running-alert,
.requirements-alert,
.plugin-alert {
  margin-bottom: 16px;
}

.header-title {
  font-weight: 600;
}

.select-row {
  display: flex;
  gap: 12px;
  align-items: center;
}

.select-row > :first-child {
  flex: 1;
}

.info-tip {
  margin-top: 12px;
  padding: 8px 12px;
  background: var(--app-bg-soft, rgba(127, 127, 127, 0.05));
  border-radius: 4px;
  font-size: 12px;
  color: var(--app-text-muted, #999);
  cursor: help;
}
</style>
