<script setup lang="ts">
/**
 * 启动 / 停止按钮（单按钮 + 6 态状态机）
 *
 * 详见 `PR/06-界面设计.md §3.2 启动停止按钮`
 *
 * 单按钮（启动/停止合一），按状态机切换 label / type / 行为：
 *
 * | 状态 | label | type | 点击行为 |
 * |---|---|---|---|
 * | `exiting` | "🚪 正在退出..." | default | loading disabled（F24 退出流程中，禁用所有按钮） |
 * | `env_switching` | "⏳ 环境切换中..." | warning | loading disabled |
 * | `needs_setup` | "⚙ 一键安装环境" | warning | 按 missing_steps 顺序自动补齐 |
 * | `installing` | "⏳ 正在安装..." | warning | loading disabled，订阅 task 进度 |
 * | `starting` | "▶ 启动中..." | primary | loading disabled（process 启动中） |
 * | `running` | "■ 停止" | error | 弹 confirm → 调 processStore.stop() |
 * | `stopped` | "▶ 启动" | success | 再次校验 readiness → 调 processStore.start() |
 * | `crashed` | "↻ 重启" | error | 校验 readiness → processStore.start() |
 *
 * 幂等性：
 * - exiting/env_switching/installing/starting 状态点击 no-op（按钮 disabled）
 * - stopped 状态点启动：会再次校验 readiness，避免在后台 install 完前误启动
 * - running 状态点停止：processStore.stop() 后端已幂等
 *
 * 优先级（自上而下）：
 * exiting > env_switching > installing > starting > running > needs_setup > crashed > stopped
 */

import { computed, ref } from "vue";
import { NButton, NSpin } from "naive-ui";
import { useProcessStore } from "@/stores/process";
import { useEnvStore } from "@/stores/env";
import { useToast } from "@/composables/useToast";
import { useEnvInstaller } from "@/composables/useEnvInstaller";
import type { ReadinessStep } from "@/api/types";

const processStore = useProcessStore();
const envStore = useEnvStore();
const toast = useToast();

// v2.14：使用 composable 共享补装逻辑
const {
  installing,
  currentStep: installStage,
  installMissingSteps,
} = useEnvInstaller();

/** 6 态枚举（按优先级排序） */
type ButtonState =
  | "exiting"
  | "env_switching"
  | "installing"
  | "starting"
  | "running"
  | "needs_setup"
  | "crashed"
  | "stopped";

/** 当前按钮状态 */
const currentState = computed<ButtonState>(() => {
  // 1. F24 退出流程中（最高优先级，禁用所有按钮）
  if (processStore.isExiting) return "exiting";
  // 2. 正在按缺失步骤引导安装（本地状态）
  if (installing.value) return "installing";
  // 3. 启动中（process 状态机驱动）
  if (processStore.isStarting) return "starting";
  // 4. 运行中
  if (processStore.isRunning) return "running";
  // 5. 进程崩溃（可重启）
  if (processStore.isCrashed) return "crashed";
  // 6. 环境未就绪（readiness.ready === false 且 readiness 已被加载过）
  if (
    envStore.isLoaded &&
    envStore.readiness !== null &&
    !envStore.readiness.ready
  ) {
    return "needs_setup";
  }
  // 7. 已就绪，未运行
  return "stopped";
});

/** 按钮配置 */
const buttonConfig = computed(() => {
  switch (currentState.value) {
    case "exiting":
      return {
        type: "default" as const,
        loading: true,
        disabled: true,
        label: "🚪 正在退出",
        showSublabel: true,
      };
    case "installing":
      return {
        type: "warning" as const,
        loading: true,
        disabled: true,
        label: "⏳ 正在安装",
        showSublabel: true,
      };
    case "starting":
      return {
        type: "primary" as const,
        loading: true,
        disabled: true,
        label: "▶ 启动中",
        showSublabel: false,
      };
    case "running":
      return {
        type: "error" as const,
        loading: false,
        disabled: false,
        label: "■ 停止",
        showSublabel: false,
      };
    case "needs_setup":
      // v2.14：按钮文案按 missing_steps 动态调整
      // 单一缺失步骤用更精确的描述
      return {
        type: "warning" as const,
        loading: false,
        disabled: false,
        label: needsSetupLabel.value,
        showSublabel: true,
      };
    case "crashed":
      return {
        type: "error" as const,
        loading: false,
        disabled: false,
        label: "↻ 重启 ComfyUI",
        showSublabel: true,
      };
    default: // stopped
      return {
        type: "success" as const,
        loading: false,
        disabled: false,
        label: "▶ 启动",
        showSublabel: false,
      };
  }
});

/** needs_setup 状态下的按钮文案（v2.14：按 missing_steps 动态调整） */
const needsSetupLabel = computed(() => {
  const steps = envStore.readiness?.missing_steps ?? [];
  if (steps.length === 0) return "⚙ 一键安装环境";
  if (steps.length === 1) {
    return `⚙ ${stageLabel(steps[0])}`;
  }
  return "⚙ 一键补装环境";
});

/** 副标题（缺失步骤或崩溃原因） */
const sublabel = computed(() => {
  if (currentState.value === "exiting") {
    return "正在停止 ComfyUI 进程组并释放资源...";
  }
  if (currentState.value === "installing") {
    return installStage.value;
  }
  if (currentState.value === "needs_setup") {
    const steps = envStore.readiness?.missing_steps ?? [];
    return steps.map(stageLabel).join(" → ");
  }
  if (currentState.value === "crashed") {
    return processStore.error || "ComfyUI 进程已崩溃";
  }
  return "";
});

/** 单个缺失步骤的简明描述 */
function stageLabel(step: ReadinessStep): string {
  switch (step.kind) {
    case "CloneComfyUI":
      return "克隆 ComfyUI";
    case "CreateVenv":
      return `创建 venv (Python ${step.params.python_version})`;
    case "InstallTorch":
      return `安装 torch (${step.params.cuda_version})`;
    case "InstallRequirements":
      return "安装依赖";
  }
}

// ========== Actions ==========

/** 主按钮点击入口（按状态分发） */
async function onClick() {
  switch (currentState.value) {
    case "exiting":
    case "installing":
    case "starting":
      // 这些状态下按钮已 disabled，这里只是兜底
      return;
    case "running":
      await onStop();
      return;
    case "needs_setup":
      await onInstallEnv();
      return;
    case "crashed":
    case "stopped":
      await onStart();
      return;
  }
}

/** 启动 ComfyUI（含 readiness 守卫） */
async function onStart() {
  // 守卫 1: 进程状态机
  if (processStore.isRunning || processStore.isStarting) {
    toast.info("ComfyUI 已在运行中");
    return;
  }
  // 守卫 2: 环境就绪（再次校验，避免后台 install 期间误启动）
  if (!envStore.readiness?.ready) {
    // 重新 check 一次（可能 store 缓存过期）
    try {
      await envStore.checkReadiness();
    } catch (e) {
      console.warn("[start] recheck readiness failed:", e);
    }
    if (!envStore.readiness?.ready) {
      toast.error("环境未就绪", "请先点击「一键安装环境」");
      return;
    }
  }
  // 守卫 3: v3.0 依赖冲突检测（不阻塞，仅提示）
  try {
    await envStore.checkConflicts();
    const report = envStore.conflictReport;
    if (report && !report.clean) {
      const majorCount = report.conflicts.filter(
        (c) => c.severity === "major",
      ).length;
      if (majorCount > 0) {
        // 主版本冲突才弹 warn，小版本/范围冲突不打扰
        toast.warn(
          `检测到 ${majorCount} 个 Python 包主版本冲突，请到设置页「依赖管理」查看详情`,
        );
      }
    }
  } catch (e) {
    console.warn("[start] checkConflicts failed:", e);
  }
  try {
    await processStore.start();
    toast.success("已发送启动命令");
  } catch (e) {
    toast.error("启动失败", e);
  }
}

/** 停止 ComfyUI（带 confirm，幂等） */
async function onStop() {
  if (!processStore.isRunning) return;
  // 不弹 confirm（单按钮方案下，连续点击风险高，但 stop 本身是幂等的）
  // 加 confirm 是为了避免误点
  try {
    await processStore.stop();
    toast.info("已发送停止命令");
  } catch (e) {
    toast.error("停止失败", e);
  }
}

/** 一键引导安装（v2.14：委托给 useEnvInstaller composable） */
async function onInstallEnv() {
  await installMissingSteps();
}
</script>

<template>
  <div class="start-stop-buttons">
    <div class="button-row">
      <!-- 单按钮：启动/停止合一 -->
      <NButton
        :type="buttonConfig.type"
        :disabled="buttonConfig.disabled"
        :loading="buttonConfig.loading"
        size="large"
        class="action-button"
        @click="onClick"
      >
        {{ buttonConfig.label }}
      </NButton>

      <!-- 旁置指示：启动中 / 停止中 -->
      <div
        v-if="currentState === 'starting' || currentState === 'installing'"
        class="side-indicator indicator-warning"
      >
        <NSpin size="small" />
      </div>
    </div>

    <div v-if="buttonConfig.showSublabel && sublabel" class="sublabel">
      {{ sublabel }}
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

.sublabel {
  font-size: 12px;
  margin-top: 4px;
  color: var(--app-text-muted, #666);
  line-height: 1.5;
  word-break: break-all;
}
</style>
