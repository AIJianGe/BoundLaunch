<script setup lang="ts">
/**
 * Python 版本切换面板（5 步事务 + 进度条）
 *
 * 详见 `PR/06-界面设计.md §5.3 设置页 - Python 版本切换`
 *
 * 后端流程：[03-模块设计/02-PythonEnvManager.md §5.1]
 *   Step 1: 安装 Python
 *   Step 2: 备份旧 venv
 *   Step 3: 创建新 venv
 *   Step 4: 安装 torch
 *   Step 5: 安装 requirements.txt
 *
 * 行为：
 * - 切换前若 ComfyUI 运行中，弹确认框，用户确认后 stop()
 * - 切换中显示 5 步进度条
 * - 失败回滚到旧 venv
 *
 * 设计模式：
 * - **Template Method**：5 步事务后端实现，前端镜像显示
 * - **State Machine**：idle / running / success / failed
 */

import { ref, computed } from "vue";
import {
  NCard,
  NForm,
  NFormItem,
  NSelect,
  NButton,
  NAlert,
  NProgress,
  NTag,
  NSpace,
} from "naive-ui";
import { useEnvStore } from "@/stores/env";
import { useProcessStore } from "@/stores/process";
import { useToast } from "@/composables/useToast";
import { useConfirm } from "@/composables/useConfirm";

const envStore = useEnvStore();
const processStore = useProcessStore();
const toast = useToast();
const confirm = useConfirm();

const pythonVersionOptions = [
  { label: "3.10", value: "3.10" },
  { label: "3.11（推荐）", value: "3.11" },
  { label: "3.12", value: "3.12" },
];

const selectedVersion = ref<string>("3.11");
const switching = ref(false);
const currentStep = ref(0);
const stepProgress = ref(0);
const switchError = ref<string | null>(null);

const totalSteps = 5;
const stepLabels = [
  "安装 Python",
  "备份旧 venv",
  "创建新 venv",
  "安装 torch",
  "安装 requirements.txt",
];

const currentVersion = computed(
  () => envStore.pythonEnvStatus?.venv_python_version || "未配置",
);

const isCurrent = computed(
  () => selectedVersion.value === currentVersion.value,
);

async function onApply() {
  if (isCurrent.value) {
    toast.info("已选中版本与当前一致，无需切换");
    return;
  }

  // 运行中需确认
  if (processStore.isAlive) {
    const ok = await confirm.warn(
      "停止 ComfyUI",
      `ComfyUI 正在运行 (PID: ${processStore.pid || "?"})，切换 Python 版本需要先停止进程，是否继续？`,
    );
    if (!ok) return;
    try {
      await processStore.stop();
    } catch (e) {
      toast.error("停止失败", e);
      return;
    }
  }

  // 二次确认（5-15 分钟长任务）
  const ok = await confirm.warn(
    "确认切换 Python 版本",
    `将重建 venv 并重装 torch + requirements.txt，预计耗时 5-15 分钟。是否继续？`,
  );
  if (!ok) return;

  switching.value = true;
  switchError.value = null;
  currentStep.value = 0;
  stepProgress.value = 0;

  try {
    // 模拟 5 步进度（实际进度需后端通过 task_progress 事件推送）
    // TODO: Phase 10 TaskScheduler 接入后，订阅 task_progress 事件更新 currentStep / stepProgress
    for (let i = 0; i < totalSteps; i++) {
      currentStep.value = i + 1;
      stepProgress.value = 0;
      // 等待该步骤完成（实际通过事件更新）
      // 这里简化为顺序调用 envStore.switchPython
      if (i === 0) {
        await envStore.switchPython(selectedVersion.value);
        stepProgress.value = 100;
      }
    }
    toast.success(`Python 已切换到 ${selectedVersion.value}`);
  } catch (e) {
    switchError.value = e instanceof Error ? e.message : String(e);
    toast.error("切换失败，旧 venv 已恢复", e);
  } finally {
    switching.value = false;
    currentStep.value = 0;
    stepProgress.value = 0;
  }
}
</script>

<template>
  <NCard class="python-panel" :bordered="true" size="small">
    <template #header>
      <span class="header-title">🐍 Python 版本切换</span>
    </template>

    <NForm label-placement="top" :show-feedback="false" size="small">
      <div class="form-row">
        <NFormItem label="目标版本">
          <NSelect
            v-model:value="selectedVersion"
            :options="pythonVersionOptions"
            :disabled="switching"
          />
        </NFormItem>
        <NFormItem label="当前版本">
          <NTag size="small" :type="isCurrent ? 'success' : 'warning'">
            {{ currentVersion }}
          </NTag>
        </NFormItem>
      </div>

      <NAlert type="info" :bordered="false" class="info-alert">
        ℹ 切换将重建 venv 并重装 torch + requirements.txt，预计耗时 5-15 分钟。
      </NAlert>

      <div class="action-row">
        <NButton
          type="primary"
          :loading="switching"
          :disabled="switching || isCurrent"
          @click="onApply"
        >
          {{ switching ? "切换中..." : "应用" }}
        </NButton>
      </div>
    </NForm>

    <!-- 5 步进度条 -->
    <div v-if="switching || switchError" class="progress-area">
      <div class="step-list">
        <div
          v-for="(label, idx) in stepLabels"
          :key="idx"
          class="step-item"
          :class="{
            'step-done': switching && idx < currentStep - 1,
            'step-active': switching && idx === currentStep - 1,
            'step-pending': switching && idx > currentStep - 1,
          }"
        >
          <span class="step-icon">
            <template v-if="switching && idx < currentStep - 1">✓</template>
            <template v-else-if="switching && idx === currentStep - 1">⏳</template>
            <template v-else>○</template>
          </span>
          <span class="step-label">Step {{ idx + 1 }}/{{ totalSteps }}: {{ label }}</span>
        </div>
      </div>

      <NProgress
        v-if="switching"
        type="line"
        :percentage="Math.round(((currentStep - 1) / totalSteps) * 100 + (stepProgress / totalSteps))"
        :height="8"
        :bordered="false"
      />

      <NAlert
        v-if="switchError"
        type="error"
        :bordered="false"
        class="error-alert"
      >
        切换失败：{{ switchError }}
        <NButton size="tiny" @click="switchError = null">关闭</NButton>
      </NAlert>
    </div>
  </NCard>
</template>

<style scoped>
.python-panel {
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

.info-alert {
  margin-top: 12px;
}

.action-row {
  margin-top: 12px;
  display: flex;
  justify-content: flex-end;
}

.progress-area {
  margin-top: 16px;
  padding-top: 12px;
  border-top: 1px solid var(--app-border, rgba(127, 127, 127, 0.1));
}

.step-list {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin-bottom: 12px;
}

.step-item {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 13px;
}

.step-icon {
  width: 18px;
  text-align: center;
  font-weight: 600;
}

.step-done .step-icon {
  color: var(--app-success, #18a058);
}

.step-active .step-icon {
  color: var(--app-warning, #f0a020);
}

.step-pending {
  color: var(--app-text-muted, #999);
}

.error-alert {
  margin-top: 12px;
}
</style>
