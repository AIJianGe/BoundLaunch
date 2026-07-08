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
 * v3.4 变更（异步启动 + 进度可视化）：
 * - 用 `useStartComfyui` 替代 `processStore.start()`，提交后立即返回 task_id
 * - 启动后自动 `router.push('/logs')` 跳到日志页
 * - 按钮下方加 NProgress 进度条 + 阶段消息（10%→100% 实时刷新）
 * - 失败时 NModal 显示 stderr tail（来自 ProcessError::EarlyExit Display）
 * - 5s~60s 期间 child 死亡 → process_crashed 事件 → 同样弹 stderr tail
 *
 * v3.4.2 变更（完全异步 + elapsed 倒计时）：
 * - 启动按钮下方加 elapsed 倒计时（"已等待 Xs"）
 * - 后端无 60s 超时限制：取消后由 cancel_token 控制，UI 不再被"超时"误导
 * - 启动中加 NSpin 旋转图标
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

import { computed, onUnmounted, ref, watch } from "vue";
import { NButton, NSpin, NProgress, NModal, NSpace, NText, useDialog } from "naive-ui";
import { useRouter } from "vue-router";
import { useProcessStore } from "@/stores/process";
import { useEnvStore } from "@/stores/env";
import { useConfigStore } from "@/stores/config";
import { useToast } from "@/composables/useToast";
import { useConfirm } from "@/composables/useConfirm";
import { useEnvInstaller } from "@/composables/useEnvInstaller";
import { useStartComfyui } from "@/composables/useStartComfyui";
import { useErrorClassifier } from "@/composables/useErrorClassifier";
import { forceKillAllPython } from "@/api/port_diagnostics";
import PortConflictModal from "./PortConflictModal.vue";

const router = useRouter();
const processStore = useProcessStore();
const envStore = useEnvStore();
const configStore = useConfigStore();
const toast = useToast();
const dialog = useDialog();
const { confirm: showConfirm } = useConfirm();
const { installMissingSteps, installing: installingEnv } = useEnvInstaller();
const { classify: classifyError } = useErrorClassifier();

// v3.4：启动 ComfyUI 专用 composable（包装 processStart + useTaskProgress + 跳转 + 崩溃弹窗）
const startComfyui = useStartComfyui();

/** v3.4：失败详情弹窗（显示 stderr tail） */
const showCrashModal = ref(false);
const crashModalContent = ref("");
/** v3.11：智能错误分类结果（用于 crashModal 顶部展示） */
const crashClassification = ref<ReturnType<typeof classifyError> | null>(null);

/** v3.11：强杀按钮 loading 状态 */
const forceKilling = ref(false);

// v3.4.2：启动耗时倒计时（提交启动后每秒 +1）
const startElapsedSec = ref(0);
let startTimerHandle: number | null = null;
function startElapsedTimer() {
  stopElapsedTimer();
  startElapsedSec.value = 0;
  startTimerHandle = window.setInterval(() => {
    startElapsedSec.value += 1;
  }, 1000);
}
function stopElapsedTimer() {
  if (startTimerHandle !== null) {
    clearInterval(startTimerHandle);
    startTimerHandle = null;
  }
}
function formatElapsed(sec: number): string {
  const m = Math.floor(sec / 60);
  const s = sec % 60;
  return m > 0 ? `${m}分${s}秒` : `${s}秒`;
}
// 启动/恢复时启动计时器，task 终态时停止
// 监听 startComfyui.isRunning 的变化
watch(
  () => startComfyui.isRunning.value,
  (running) => {
    if (running) {
      startElapsedTimer();
    } else {
      stopElapsedTimer();
    }
  },
);

onUnmounted(() => {
  stopElapsedTimer();
});

/** v3.4：把 stderr tail 转成纯文本（用于 NModal 展示） */
function formatStderrTail(stderrTail: string[]): string {
  if (!stderrTail || stderrTail.length === 0) {
    return "(无 stderr 输出，可能 stdout 报错或 main.py 启动前崩溃)";
  }
  return stderrTail.join("\n");
}

/** 6 态枚举（按优先级排序） */
type ButtonState =
  | "exiting"
  | "submitting" // v3.4.1：本地提交中（防连点，最高优先级）
  | "env_switching"
  | "starting"
  | "running"
  | "crashed"
  | "stopped";

/** 当前按钮状态 */
const currentState = computed<ButtonState>(() => {
  // 1. v3.4.1：本地提交中（防连点）— 同步检查，第一时间 disable
  // 优先级最高，阻止任何后续点击进入 onStart()
  if (startComfyui.submitting.value) return "submitting";
  // 2. F24 退出流程中（次高优先级，禁用所有按钮）
  if (processStore.isExiting) return "exiting";
  // 3. 环境切换中（torch 切换等，由 envStore.switchingTorch 驱动）
  if (envStore.switchingTorch) return "env_switching";
  // 4. 启动中（process 状态机驱动）
  if (processStore.isStarting) return "starting";
  // 5. 运行中
  if (processStore.isRunning) return "running";
  // 6. 进程崩溃（可重启）
  if (processStore.isCrashed) return "crashed";
  // 7. 已就绪或未就绪，统一归 stopped 态（未就绪由 sublabel + 点击 toast 引导）
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

/**
 * v3.10：检测 torch.cuda_available 与 launch_mode 是否匹配
 *
 * 解决「torch+cpu + GpuHigh 启动后 AssertionError」问题：
 * - 当用户在设置页选了 cu128，但 venv 中 torch+cpu（config 与 venv 不一致）
 * - 当用户选 GpuHigh 模式但 torch 不支持 CUDA
 * → mismatch → 启动按钮显示警告，sublabel 引导去设置修复
 *
 * 优先级：mismatch > envNotReady（mismatch 是更具体的错误）
 */
const cudaMismatch = computed(() => {
  if (!envStore.isLoaded) return false;
  const env = envStore.envInfo;
  if (!env || !env.torch_installed) return false;
  // v3.10：launchMode 来自 configStore
  const mode = configStore.launchMode;
  const gpuMode =
    mode === "gpu_high" || mode === "gpu_low" || mode === "gpu_no_vram";
  // gpuMode 缺失（null）时不视为 mismatch（让后端 verify_preconditions 处理）
  if (mode !== null && gpuMode && !env.cuda_available) {
    return true;
  }
  return false;
});

/** 按钮配置 */
const buttonConfig = computed(() => {
  switch (currentState.value) {
    // v3.4.1：提交中（连点守卫）— 立即 disabled，loading 显示防止误操作
    case "submitting":
      return {
        type: "primary" as const,
        loading: true,
        disabled: true,
        label: "🚀 正在提交",
        showSublabel: true,
      };
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
      // v3.10：mismatch 时按钮变 warning + disabled，引导去设置修复
      if (cudaMismatch.value) {
        return {
          type: "warning" as const,
          loading: false,
          disabled: true,
          label: "⚠ PyTorch 不支持 CUDA",
          showSublabel: true,
        };
      }
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
  if (currentState.value === "submitting") {
    return "启动请求已提交，请勿重复点击...";
  }
  if (currentState.value === "exiting") {
    return "正在停止 ComfyUI 进程组并释放资源...";
  }
  if (currentState.value === "env_switching") {
    return "正在切换 torch 变体，请稍候...";
  }
  if (currentState.value === "crashed") {
    return processStore.error || "ComfyUI 进程已崩溃";
  }
  // v3.10：mismatch 优先级最高（mismatch 是更具体的错误）
  if (currentState.value === "stopped" && cudaMismatch.value) {
    return "⚠ PyTorch 不支持 CUDA：请到「设置 → 关键依赖」点击「重新安装 PyTorch」";
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
    case "submitting":
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
  // 守卫 0: v3.4.1 连点守卫（最优先级，同步检查）
  // 必须在**任何 await 之前**置 submitting=true，这样按钮状态能同步变化，
  // 后续连点全部在 guard 0 处被拦截。
  if (startComfyui.submitting.value) {
    return;
  }
  startComfyui.markSubmitting();

  // 守卫 1: 进程状态机
  if (processStore.isRunning || processStore.isStarting) {
    startComfyui.unmarkSubmitting();
    toast.info("ComfyUI 已在运行中");
    return;
  }

  // v3.10 守卫 1.5：torch cuda_available vs launch_mode mismatch
  // - 比 readiness 守卫更具体（readiness 不知道 launch_mode × cuda_available 关系）
  // - 弹窗让用户选择：去设置页修复 / 仍然启动（可能失败）
  if (cudaMismatch.value) {
    const env = envStore.envInfo;
    const torchVer = env?.torch_version ?? "?";
    const ok = await showConfirm({
      title: "PyTorch 不支持 CUDA",
      content:
        `检测到 venv 中的 PyTorch 不支持 CUDA：\n` +
        `• PyTorch 版本：${torchVer}\n` +
        `• torch.cuda.is_available() = ${env?.cuda_available ?? "未知"}\n` +
        `• 启动模式：${configStore.launchMode ?? "gpu_high"}（需要 GPU）\n\n` +
        `这会导致 ComfyUI 启动后立即报错：\n` +
        `  AssertionError: Torch not compiled with CUDA enabled\n\n` +
        `建议：\n` +
        `1. 到「设置 → 关键依赖」点击「重新安装 PyTorch」（用 --force-reinstall 强制一致）\n` +
        `2. 或先在「基础参数」切换到「CPU 模式」`,
      positiveText: "去设置页修复",
      negativeText: "仍然启动",
    });
    if (ok) {
      startComfyui.unmarkSubmitting();
      // 跳到 PyTorch 设置页（用户最关心的修复入口）
      router.push("/settings/torch");
      return;
    }
    // 用户选择"仍然启动"→ 不阻断，让后端 verify_preconditions 给出具体错误
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
    const missingKinds = envStore.readiness?.missing_steps?.map(s => s.kind) ?? [];

    // v3.11.7：环境被重置（ComfyUI 仓库被删）→ 引导回引导页
    // 区分"部分丢失"和"全部重置"：
    // - CloneComfyUI 在 missing_steps 里 = ComfyUI 仓库不存在 = 环境被重置
    // - 此时弹 venv 对话框没用（install_requirements 会因找不到 requirements.txt 失败）
    // - 应该引导用户回引导页重新安装
    if (missingKinds.includes("CloneComfyUI")) {
      const ok = await showConfirm({
        title: "检测到环境未初始化",
        content:
          "ComfyUI 仓库不存在，需要重新进行引导安装。\n\n" +
          "是否进入引导安装？",
        positiveText: "进入引导安装",
        negativeText: "取消",
      });
      if (ok) {
        router.push("/onboarding");
      }
      startComfyui.unmarkSubmitting();
      return;
    }

    // v3.2.1：仅 venv 不存在（部分丢失，ComfyUI 仓库还在）
    // - 用户改 venv 路径后新路径下没 venv
    // - 弹窗引导用户立即创建
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
            startComfyui.unmarkSubmitting();
            return;
          }
        } else {
          toast.error("环境安装失败", "请到「设置 → 路径配置」重试");
          startComfyui.unmarkSubmitting();
          return;
        }
      } else {
        // 取消：明确告知去设置页（更具体的引导）
        toast.error(
          "环境未就绪",
          `请前往「设置 → 路径配置」\n手动操作 venv 路径或点击「一键补装」`,
        );
        startComfyui.unmarkSubmitting();
        return;
      }
    } else {
      // v3.2：未就绪时引导用户去设置页（首页不再触发安装）
      toast.error(
        "环境未就绪",
        "请前往「设置 → 路径配置」点击「一键补装」",
      );
      startComfyui.unmarkSubmitting();
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

  // v3.4：用 useStartComfyui 替代 processStore.start()
  // 行为变化：
  // - 立即返回 task_id（不阻塞）
  // - 进度通过 task_progress 事件推送（按钮下方 NProgress 显示）
  // - 终态自动 router.push('/logs')
  // - 失败时 NModal 显示 stderr tail
  // - 5s~60s 期间 child 死亡 → process_crashed 事件 → 同样弹 stderr tail
  try {
    await startComfyui.start({
      onCrashed: (event) => {
        // 5s~60s 期间 child 死亡：弹窗显示 stderr tail
        const reasonLabel =
          event.reason === "health_check_detected"
            ? "健康检查发现崩溃"
            : "monitor 检测到退出";
        crashModalContent.value = `ComfyUI ${reasonLabel}（exit code: ${event.exit_code ?? "未知"}）\n\n${formatStderrTail(event.stderr_tail)}`;
        // v3.11：智能错误分类
        crashClassification.value = classifyError({
          exit_code: event.exit_code ?? null,
          stderr_tail: event.stderr_tail,
        });
        showCrashModal.value = true;
      },
    });
  } catch {
    // 错误已由 useStartComfyui 内部 toast.error 提示 + onError 回调
    // 此处仅 catch 防止异常冒泡（按钮状态机回到 stopped）
    // v3.4.1：start() 内部的 finally 块已经会把 submitting 置回 false，
    // 这里不需要再 unmarkSubmitting
  }
  // 兜底：万一 start() 同步完成且没有触发终态回调，确保 submitting 被重置
  // （实际上 start() 内的 finally 已经处理了，但这里保留为双保险）
  startComfyui.unmarkSubmitting();
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

/**
 * v3.11：强制停止（兜底机制）
 *
 * 场景：进程卡在"启动中"或健康检查一直不通过时，用户需要快速脱困
 * 流程：
 * 1. 二次确认（提示用户会结束所有 Python 进程）
 * 2. 先尝试 processStore.stop()（优雅停止）
 * 3. 等 1.5s，如果状态没变回 stopped，调用 forceKillAllPython 兜底
 * 4. 不管结果如何，最后强制重置 status 到 stopped
 */
async function onForceKill() {
  if (forceKilling.value) return;

  const confirmed = await new Promise<boolean>((resolve) => {
    dialog.warning({
      title: "⏹ 强制停止确认",
      content:
        "此操作将强制结束 ComfyUI 进程（包括所有相关 Python 子进程）。\n\n" +
        "如果 ComfyUI 正在加载模型或处理请求，可能导致未保存的数据丢失。\n\n" +
        "确定要继续吗？",
      positiveText: "强制结束",
      negativeText: "取消",
      onPositiveClick: () => resolve(true),
      onNegativeClick: () => resolve(false),
      onClose: () => resolve(false),
    });
  });
  if (!confirmed) return;

  forceKilling.value = true;
  toast.warn("正在强制结束 ComfyUI...");

  try {
    // 1. 先尝试优雅停止
    try {
      await processStore.stop();
    } catch (e) {
      console.warn("[onForceKill] processStore.stop failed:", e);
    }

    // 2. 等 1.5s，看是否回到 stopped
    await new Promise((r) => setTimeout(r, 1500));

    if (processStore.isRunning || processStore.isStarting) {
      // 3. 兜底：杀所有 Python 进程
      console.warn("[onForceKill] graceful stop failed, force killing all python");
      try {
        await forceKillAllPython();
        toast.success("已强制结束所有 Python 进程");
      } catch (e) {
        console.error("[onForceKill] forceKillAllPython failed:", e);
        toast.error("强杀失败，请手动结束 Python 进程", String(e));
      }
    }
  } finally {
    forceKilling.value = false;
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

      <!-- 旁置指示：提交中 / 启动中 / 环境切换中 -->
      <div
        v-if="currentState === 'submitting' || currentState === 'starting' || currentState === 'env_switching'"
        class="side-indicator indicator-warning"
      >
        <NSpin size="small" />
      </div>

      <!-- v3.11：强制停止按钮（兜底）
           在启动中 / 运行中 / 卡死等场景出现，让用户有"脱困"按钮 -->
      <NButton
        v-if="currentState === 'starting' || currentState === 'submitting' || (currentState === 'running' && forceKilling)"
        type="error"
        size="large"
        :loading="forceKilling"
        :disabled="forceKilling"
        class="force-kill-button"
        @click="onForceKill"
      >
        ⏹ 强制停止
      </NButton>
    </div>

    <div v-if="buttonConfig.showSublabel && sublabel" class="sublabel">
      {{ sublabel }}
    </div>

    <!-- v3.4.2：启动进度条 + elapsed 倒计时（useStartComfyui.isRunning 时显示） -->
    <div v-if="startComfyui.isRunning.value" class="progress-section">
      <NProgress
        type="line"
        :percentage="startComfyui.progress.value"
        :indicator-placement="'inside'"
        :height="14"
        :border-radius="4"
        processing
      />
      <div class="progress-message">
        {{ startComfyui.message.value || "准备中..." }}
      </div>
      <div class="progress-elapsed">
        <NSpin size="small" class="progress-spin" />
        <span>已等待 {{ formatElapsed(startElapsedSec) }}</span>
      </div>
    </div>

    <!-- v3.4：失败详情弹窗（显示 stderr tail） -->
    <NModal
      v-model:show="showCrashModal"
      preset="card"
      title="ComfyUI 启动失败"
      style="max-width: 800px"
      :bordered="false"
      size="huge"
    >
      <NSpace vertical>
        <!-- v3.11：智能错误分类展示 -->
        <div v-if="crashClassification" class="classification-block">
          <NAlert
            :type="crashClassification.severity === 'critical' ? 'error' : crashClassification.severity === 'high' ? 'error' : crashClassification.severity === 'medium' ? 'warning' : 'info'"
            :show-icon="true"
          >
            <template #header>
              <strong>{{ crashClassification.title }}</strong>
            </template>
            <div class="classification-detail">
              <p>{{ crashClassification.description }}</p>
              <p class="root-cause">
                <strong>根因：</strong>{{ crashClassification.root_cause }}
              </p>
              <div v-if="crashClassification.recommended_actions.length > 0" class="actions-list">
                <strong>建议操作：</strong>
                <ul>
                  <li
                    v-for="(action, idx) in crashClassification.recommended_actions"
                    :key="idx"
                    :class="{ primary: action.primary }"
                  >
                    <span v-if="action.primary">👉 </span>
                    <span v-else>· </span>
                    {{ action.label }}
                  </li>
                </ul>
              </div>
            </div>
          </NAlert>
        </div>

        <NText depth="3">
          以下是 ComfyUI 进程崩溃前的最后日志（最多 50 行）。可全选复制后到 GitHub Issues 搜索类似错误。
        </NText>
        <pre class="crash-stderr">{{ crashModalContent }}</pre>
      </NSpace>
    </NModal>

    <!-- v3.11：端口被占弹窗（processStore.startFailedReason 触发） -->
    <PortConflictModal />
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

/* v3.11：强制停止按钮（与主按钮同行） */
.force-kill-button {
  height: 56px;
  font-size: 16px;
  font-weight: 600;
  min-width: 130px;
}

/* v3.11：智能错误分类展示 */
.classification-block {
  margin-bottom: 12px;
}

.classification-detail p {
  margin: 6px 0;
  line-height: 1.5;
}

.classification-detail .root-cause {
  font-size: 13px;
  opacity: 0.85;
}

.classification-detail .actions-list {
  margin-top: 8px;
  font-size: 13px;
}

.classification-detail .actions-list ul {
  margin: 6px 0 0 0;
  padding-left: 0;
  list-style: none;
}

.classification-detail .actions-list li {
  margin: 4px 0;
  padding: 4px 0;
  line-height: 1.4;
}

.classification-detail .actions-list li.primary {
  font-weight: 600;
  color: var(--app-primary, #18a058);
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

/* v3.4 进度条样式 */
.progress-section {
  margin-top: 8px;
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.progress-message {
  font-size: 12px;
  color: var(--app-text-muted, #666);
  line-height: 1.4;
  word-break: break-all;
}

.progress-elapsed {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
  color: var(--app-warning, #f0a020);
  font-weight: 500;
}

.progress-spin {
  display: inline-flex;
}

/* v3.4 失败弹窗：stderr 区域 */
.crash-stderr {
  margin: 0;
  padding: 12px;
  background: #1e1e1e;
  color: #d4d4d4;
  border-radius: 4px;
  font-family: "Cascadia Code", "Consolas", "Menlo", monospace;
  font-size: 12px;
  line-height: 1.5;
  max-height: 400px;
  overflow: auto;
  white-space: pre-wrap;
  word-break: break-all;
  user-select: text;
}
</style>
