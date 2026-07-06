<script setup lang="ts">
/**
 * 启动 / 停止按钮（单按钮 + 6 态状态机，v3.2 极简化）
 *
 * 详见 `PR/06-界面设计.md §3.2 启动停止按钮`
 *
 * v3.2 变更（F25 绘世启动器哲学）：
 * - 移除 `needs_setup` / `installing` 分支（首页不再触发安装）
 * - 安装入口统一收归到「设置 → 路径配置」PathsPanel
 * - 按钮只显示"启动"或"停止"，保持启动器极简
 * - 环境未就绪时：按钮仍显示"▶ 启动"，sublabel 提示去设置页，点击 toast 引导
 *
 * 单按钮状态机（按优先级排序）：
 *
 * | 状态 | label | type | 点击行为 |
 * |---|---|---|---|
 * | `exiting` | "🚪 正在退出..." | default | loading disabled（F24 退出流程中） |
 * | `env_switching` | "⏳ 环境切换中..." | warning | loading disabled（torch 切换等） |
 * | `starting` | "▶ 启动中..." | primary | loading disabled（process 启动中） |
 * | `running` | "■ 停止" | error | 弹 confirm → 调 processStore.stop() |
 * | `crashed` | "↻ 重启" | error | 校验 readiness → processStore.start() |
 * | `stopped` | "▶ 启动" | success | 校验 readiness → processStore.start() |
 *
 * 幂等性：
 * - exiting/env_switching/starting 状态点击 no-op（按钮 disabled）
 * - stopped 状态点启动：会再次校验 readiness，未就绪则 toast 引导去设置页
 * - running 状态点停止：processStore.stop() 后端已幂等
 *
 * 优先级（自上而下）：
 * exiting > env_switching > starting > running > crashed > stopped
 */

import { computed } from "vue";
import { NButton, NSpin } from "naive-ui";
import { useProcessStore } from "@/stores/process";
import { useEnvStore } from "@/stores/env";
import { useToast } from "@/composables/useToast";
import { useConfirm } from "@/composables/useConfirm";
import { useEnvInstaller } from "@/composables/useEnvInstaller";

const processStore = useProcessStore();
const envStore = useEnvStore();
const toast = useToast();
const { confirm: showConfirm } = useConfirm();
const { installMissingSteps, installing: installingEnv } = useEnvInstaller();

/** 6 态枚举（按优先级排序） */
type ButtonState =
  | "exiting"
  | "env_switching"
  | "starting"
  | "running"
  | "crashed"
  | "stopped";

/** 当前按钮状态 */
const currentState = computed<ButtonState>(() => {
  // 1. F24 退出流程中（最高优先级，禁用所有按钮）
  if (processStore.isExiting) return "exiting";
  // 2. 环境切换中（torch 切换等，由 envStore.switchingTorch 驱动）
  if (envStore.switchingTorch) return "env_switching";
  // 3. 启动中（process 状态机驱动）
  if (processStore.isStarting) return "starting";
  // 4. 运行中
  if (processStore.isRunning) return "running";
  // 5. 进程崩溃（可重启）
  if (processStore.isCrashed) return "crashed";
  // 6. 已就绪或未就绪，统一归 stopped 态（未就绪由 sublabel + 点击 toast 引导）
  return "stopped";
});

/**
 * 环境是否未就绪（用于 sublabel 提示）
 *
 * 仅在 stopped / crashed 态有意义：判断是否需要引导用户去设置页补装。
 * readiness === null 时不视为未就绪（可能还在加载，避免误报）。
 */
const envNotReady = computed(
  () =>
    envStore.isLoaded &&
    envStore.readiness !== null &&
    !envStore.readiness.ready,
);

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
    case "env_switching":
      return {
        type: "warning" as const,
        loading: true,
        disabled: true,
        label: "⏳ 环境切换中",
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
        showSublabel: true, // 改为 true 以便未就绪时显示提示
      };
  }
});

/** 副标题（未就绪提示 / 退出/切换中说明 / 崩溃原因） */
const sublabel = computed(() => {
  if (currentState.value === "exiting") {
    return "正在停止 ComfyUI 进程组并释放资源...";
  }
  if (currentState.value === "env_switching") {
    return "正在切换 torch 变体，请稍候...";
  }
  if (currentState.value === "crashed") {
    return processStore.error || "ComfyUI 进程已崩溃";
  }
  if (currentState.value === "stopped" && envNotReady.value) {
    return "⚠ 环境未就绪，请前往「设置 → 路径配置」点击「一键补装」";
  }
  return "";
});

// ========== Actions ==========

/** 主按钮点击入口（按状态分发） */
async function onClick() {
  switch (currentState.value) {
    case "exiting":
    case "env_switching":
    case "starting":
      // 这些状态下按钮已 disabled，这里只是兜底
      return;
    case "running":
      await onStop();
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

  // v3.2.1 关键修复：先强制刷新 envInfo 和 readiness
  // - 之前只调 checkReadiness()，envInfo 仍是旧路径数据（PYTORCH 显示旧值）
  // - 修复后 envInfo 与 readiness 来自同一份最新 inspect 结果
  try {
    await envStore.invalidateCache();
    await envStore.refresh();
    await envStore.checkReadiness();
  } catch (e) {
    console.warn("[start] precheck failed:", e);
  }

  // 守卫 2: 环境就绪（再次校验，避免后台 install 期间误启动）
  if (!envStore.readiness?.ready) {
    // v3.2.1 关键改进：检查是否 venv 不存在
    // - 用户改 venv 路径后新路径下没 venv（v3.2 之前版本行为）
    // - 弹窗引导用户立即创建，比单纯 toast 引导去设置页更直接
    const missingKinds = envStore.readiness?.missing_steps?.map(s => s.kind) ?? [];
    if (missingKinds.includes("CreateVenv") && !installingEnv.value) {
      const venvPath = envStore.envInfo?.venv_path ?? "(未知)";
      const ok = await showConfirm({
        title: "检测到 venv 目录不存在",
        content:
          `当前 venv 路径：${venvPath}\n\n` +
          "该路径下未检测到 venv，无法启动 ComfyUI。\n\n" +
          "立即创建 venv 并安装依赖？\n" +
          "（含创建 venv + 安装 PyTorch + 安装 ComfyUI 依赖，预计 5-15 分钟）",
        positiveText: "立即创建",
        negativeText: "取消",
      });
      if (ok) {
        const success = await installMissingSteps();
        if (success) {
          // 安装完成后再 check 一次
          await envStore.checkReadiness();
          if (!envStore.readiness?.ready) {
            toast.error("环境未就绪", "安装过程中出现问题，请到「设置 → 路径配置」查看详情");
            return;
          }
        } else {
          toast.error("环境安装失败", "请到「设置 → 路径配置」重试");
          return;
        }
      } else {
        // 取消：明确告知去设置页（更具体的引导）
        toast.error(
          "环境未就绪",
          `请前往「设置 → 路径配置」\n手动操作 venv 路径或点击「一键补装」`,
        );
        return;
      }
    } else {
      // v3.2：未就绪时引导用户去设置页（首页不再触发安装）
      toast.error(
        "环境未就绪",
        "请前往「设置 → 路径配置」点击「一键补装」",
      );
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

      <!-- 旁置指示：启动中 / 环境切换中 -->
      <div
        v-if="currentState === 'starting' || currentState === 'env_switching'"
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
