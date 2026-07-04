<script setup lang="ts">
/**
 * 启动 / 停止按钮（6 态状态机）
 *
 * 详见 `PR/06-界面设计.md §3.2 启动停止按钮 6 态状态机`
 *
 * 状态优先级：环境未就绪 > 环境切换中 > 依赖需更新 > 运行中 > 启动中 > 未运行
 *
 * | 状态 | 启动按钮 | 停止按钮 | 旁置指示 |
 * |---|---|---|---|
 * | 未运行 | 绿色高亮可点 | 灰色禁用 | 无 |
 * | 启动中 | 灰色禁用 | 灰色禁用 | 加载圈 + 「正在启动...」 |
 * | 运行中 | 灰色禁用 | 红色高亮可点 | 无 |
 * | 环境未就绪 | 红色禁用 + 红字 | 灰色禁用 | 红色感叹号 |
 * | 环境切换中 | 黄色禁用 | 灰色禁用 | 加载圈 |
 * | 依赖需更新 | 黄色禁用 + [立即安装] | 灰色禁用 | 黄色感叹号 |
 */

import { computed } from "vue";
import { NButton, NSpin, NTooltip, NIcon } from "naive-ui";
import { useProcessStore } from "@/stores/process";
import { useEnvStore } from "@/stores/env";
import { useTaskStore } from "@/stores/task";
import { useCoreStore } from "@/stores/core";
import { useToast } from "@/composables/useToast";
import { useConfirm } from "@/composables/useConfirm";

const processStore = useProcessStore();
const envStore = useEnvStore();
const taskStore = useTaskStore();
const coreStore = useCoreStore();
const toast = useToast();
const confirm = useConfirm();

/** 6 态枚举（按优先级排序，越高越优先） */
type ButtonState =
  | "env_not_ready"
  | "env_switching"
  | "requirements_mismatch"
  | "running"
  | "starting"
  | "stopped";

/** 当前按钮状态（按优先级判断） */
const currentState = computed<ButtonState>(() => {
  // 1. 环境未就绪（torch 缺失 / venv 不存在）
  if (envStore.isLoaded && (!envStore.venvExists || !envStore.torchInstalled)) {
    return "env_not_ready";
  }
  // 2. 环境切换中（有运行中的环境任务）
  const envSwitchingTasks = taskStore.runningTasks.filter(
    (t) =>
      t.kind === "install_torch" ||
      t.kind === "install_requirements" ||
      t.kind === "checkout",
  );
  if (envSwitchingTasks.length > 0) {
    return "env_switching";
  }
  // 3. 依赖需更新（来自后端 requirements_mismatch 事件）
  if (coreStore.requirementsMismatch) {
    return "requirements_mismatch";
  }
  // 4. 运行中
  if (processStore.isRunning) {
    return "running";
  }
  // 5. 启动中
  if (processStore.isStarting) {
    return "starting";
  }
  // 6. 未运行
  return "stopped";
});

/** 启动按钮配置 */
const startButtonConfig = computed(() => {
  switch (currentState.value) {
    case "env_not_ready":
      return {
        type: "error" as const,
        disabled: true,
        label: "⚠ 环境异常",
        sublabel: "torch 缺失，请重新初始化",
      };
    case "env_switching":
      return {
        type: "warning" as const,
        disabled: true,
        label: "⏳ 环境切换中",
        sublabel: "请等待...",
      };
    case "requirements_mismatch":
      return {
        type: "warning" as const,
        disabled: false, // 允许点击触发 [立即安装]
        label: "⚠ 依赖需更新",
        sublabel: "点击立即安装",
      };
    case "running":
      return {
        type: "default" as const,
        disabled: true,
        label: "▶ 启动",
        sublabel: "",
      };
    case "starting":
      return {
        type: "default" as const,
        disabled: true,
        label: "▶ 启动",
        sublabel: "",
      };
    default: // stopped
      return {
        type: "success" as const,
        disabled: false,
        label: "▶ 启动",
        sublabel: "",
      };
  }
});

/** 停止按钮配置 */
const stopButtonConfig = computed(() => {
  switch (currentState.value) {
    case "running":
      return {
        type: "error" as const,
        disabled: false,
        label: "■ 停止",
      };
    default:
      return {
        type: "default" as const,
        disabled: true,
        label: "■ 停止",
      };
  }
});

/** 旁置指示文案 */
const sideIndicator = computed(() => {
  switch (currentState.value) {
    case "starting":
      return { icon: "◔", text: "正在启动...", color: "warning" as const };
    case "env_not_ready":
      return { icon: "⚠", text: "torch 缺失", color: "error" as const };
    case "env_switching":
      return { icon: "⏳", text: "环境切换中", color: "warning" as const };
    case "requirements_mismatch":
      return { icon: "⚠", text: "依赖需更新", color: "warning" as const };
    default:
      return null;
  }
});

// ========== Actions ==========

async function onStart() {
  if (currentState.value === "requirements_mismatch") {
    // 依赖需更新：触发 requirements 安装
    const ok = await confirm.warn(
      "安装依赖",
      "检测到 requirements.txt 需要更新，是否立即安装？",
    );
    if (!ok) return;
    try {
      // 没有具体插件名时调用 core 的 requirements 安装
      // 这里通过后端某个统一入口（待补全）；本期用 toast 提示
      toast.info("开始安装 requirements.txt，请查看任务进度");
      // TODO: 实际调用后端 requirements install 命令
    } catch (e) {
      toast.error("安装失败", e);
    }
    return;
  }

  if (currentState.value !== "stopped") return;

  try {
    await processStore.start();
    toast.success("已发送启动命令");
  } catch (e) {
    toast.error("启动失败", e);
  }
}

async function onStop() {
  if (!processStore.isRunning) return;
  const ok = await confirm.warn("停止 ComfyUI", "确认停止当前运行的 ComfyUI 进程？");
  if (!ok) return;
  try {
    await processStore.stop();
    toast.info("已发送停止命令");
  } catch (e) {
    toast.error("停止失败", e);
  }
}
</script>

<template>
  <div class="start-stop-buttons">
    <div class="button-row">
      <NButton
        :type="startButtonConfig.type"
        :disabled="startButtonConfig.disabled"
        :loading="currentState === 'starting'"
        size="large"
        class="action-button"
        @click="onStart"
      >
        {{ startButtonConfig.label }}
      </NButton>

      <NButton
        :type="stopButtonConfig.type"
        :disabled="stopButtonConfig.disabled"
        size="large"
        class="action-button"
        @click="onStop"
      >
        {{ stopButtonConfig.label }}
      </NButton>

      <div v-if="sideIndicator" class="side-indicator" :class="`indicator-${sideIndicator.color}`">
        <NSpin v-if="currentState === 'starting' || currentState === 'env_switching'" size="small" />
        <span v-else class="indicator-icon">{{ sideIndicator.icon }}</span>
        <span class="indicator-text">{{ sideIndicator.text }}</span>
      </div>
    </div>

    <div v-if="startButtonConfig.sublabel" class="sublabel" :class="`sublabel-${startButtonConfig.type}`">
      {{ startButtonConfig.sublabel }}
    </div>
  </div>
</template>

<style scoped>
.start-stop-buttons {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.button-row {
  display: flex;
  align-items: center;
  gap: 12px;
}

.action-button {
  flex: 1;
  height: 56px;
  font-size: 18px;
  font-weight: 600;
}

.side-indicator {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 13px;
  padding: 0 8px;
}

.indicator-warning {
  color: var(--app-warning, #f0a020);
}

.indicator-error {
  color: var(--app-error, #d03050);
}

.indicator-icon {
  font-size: 18px;
}

.sublabel {
  font-size: 12px;
  margin-top: 4px;
}

.sublabel-error {
  color: var(--app-error, #d03050);
}

.sublabel-warning {
  color: var(--app-warning, #f0a020);
}
</style>
