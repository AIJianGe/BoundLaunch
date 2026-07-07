<script setup lang="ts">
/**
 * 首次运行向导
 *
 * 详见 `PR/06-界面设计.md §0 首次运行向导`
 *
 * 5 步向导（v2.15 改版：5 步分两行布局）：
 *
 * 基础配置组（步骤 1-3）：
 *  1. 选择 ComfyUI 根目录
 *  2. 选择 Python venv 位置
 *  3. 选择运行模式（CPU / GPU / Custom）
 *
 * 环境安装组（步骤 4-5）：
 *  4. 环境初始化（创建 venv + 装 torch）
 *  5. 安装 ComfyUI 依赖（requirements.txt）
 *
 * 设计模式：
 * - **Builder**：分步构建 Config，最后一步保存
 * - **State Machine**：currentStep (0-4) 控制 5 态切换
 * - **分组 UI**：currentStepConfig / currentStepInstall 派生自 currentStep
 * - **Observer**（v3.11）：用 `useTaskProgress` 订阅 task_progress / task_log 事件
 *   实现「实时进度 + 实时日志 + ETA」展示
 *
 * 行为：
 * - 任意步骤可点击「上一步」回退（保留已填数据）
 * - 最后一步「开始初始化」进入长任务（订阅 task_progress）
 * - 「跳过向导」直接进入主界面（设置页仍可改配置）
 *
 * v3.11 关键改造：
 * 1. install_requirements 改为**阻塞步骤**（失败即终止，不再"警告"）
 * 2. 进度条展示真实进度（通过 useTaskProgress 订阅 task_progress 事件）
 * 3. ETA 计算：`已用时间 / 当前进度 × (100 - 当前进度)`
 * 4. 实时日志展示：最近 3-5 行 uv 子进程输出
 * 5. 进度区间重新分配：save 5% / venv 5-15% / torch 15-60% / requirements 60-95% / refresh 95-100%
 */

import { ref, computed, reactive, onMounted } from "vue";
import { useRouter } from "vue-router";
import {
  NCard,
  NSteps,
  NStep,
  NButton,
  NSpace,
  NRadioGroup,
  NRadio,
  NForm,
  NFormItem,
  NAlert,
  NProgress,
  NSpin,
  NDivider,
  NTag,
  NScrollbar,
} from "naive-ui";
import { useConfigStore } from "@/stores/config";
import { useEnvStore } from "@/stores/env";
import { useCoreStore } from "@/stores/core";
import { useToast } from "@/composables/useToast";
import { useTaskProgress } from "@/composables/useTaskProgress";
import { configLauncherWorkingDir, configDataLocation, type DataLocationInfo } from "@/api/config";
import { coreListTagsClassified } from "@/api/core";
import { envCreateVenv, envInstallTorch, envInstallRequirements } from "@/api/env";
import { latestStableForInstallation } from "@/composables/useTagRules";
import FolderPicker from "@/components/FolderPicker.vue";
import type { LaunchMode, CudaVersion } from "@/api/types";

const router = useRouter();
const configStore = useConfigStore();
const envStore = useEnvStore();
const coreStore = useCoreStore();
const toast = useToast();

/** v3.11：任务进度跟踪器（订阅 task_progress / task_log / task_completed） */
const taskTracker = useTaskProgress();

// ========== State ==========
const currentStep = ref(0);
const initializing = ref(false);
const initProgress = ref(0);
const initStage = ref("");
const initError = ref<string | null>(null);
const cloning = ref(false);
const cloneStage = ref("");

/**
 * v3.11：进度展示增强
 * - initEtaSeconds：预计剩余秒数（null = 尚未计算出有意义值）
 * - initRecentLogs：最近 5 行 uv 子进程输出（实时刷新）
 * - initStartMs：本次初始化的起始时间戳（用于 ETA 计算）
 */
const initEtaSeconds = ref<number | null>(null);
const initRecentLogs = ref<Array<{ source: string; text: string; ts_ms: number }>>([]);
const initStartMs = ref<number>(0);

/**
 * v3.10：预估的「引导安装默认版本」
 *
 * 计算时机：onMounted（与 config 加载并行）
 * - 优先用 Config.paths.installation_default_version（用户显式配置）
 * - 否则用前端版本的 latest_stable_for_installation（与后端 tags:: 同款规则）
 */
const expectedDefaultVersion = ref<string | null>(null);
const versionHintLoading = ref(false);

// 表单数据（与 Config 字段对应）
const form = reactive({
  comfyui_root: "",
  venv_path: "",
  python_version: "3.11",
  mode: "gpu_high" as LaunchMode,
  cuda_version: "cu128" as CudaVersion,
});

/** v1.8 / F38：数据位置信息（portable 模式下默认路径跟随） */
const dataLocation = ref<DataLocationInfo | null>(null);

// ========== 初始化：自动填充 launcher 工作目录作为默认根目录 ==========
onMounted(async () => {
  try {
    // v1.8 / F38：优先用后端的 dataLocation 拿 portable 模式下的默认路径
    // 旧版用 launcher_working_dir（即 cwd），新版用 portable_base_dir
    // - dev 模式：项目根
    // - prod 模式：exe 旁
    const loc = await configDataLocation();
    dataLocation.value = loc;

    const portableBase = loc.portable_base_dir;
    if (!form.comfyui_root && portableBase) {
      // portable 模式：ComfyUI 根目录 = <portable_base>/ComfyUI
      // 与后端 paths::default_comfyui_root 行为一致
      form.comfyui_root = `${portableBase}/ComfyUI`.replace(/[\\/]+$/, "");
    } else if (!form.comfyui_root) {
      // legacy 模式：fallback 到 launcher_working_dir
      const workDir = await configLauncherWorkingDir();
      form.comfyui_root = `${workDir}/ComfyUI`;
    }
    // v3.2.1：venv 默认路径改到 ComfyUI 外
    // - 之前默认 `<comfyui_root>/venv`，venv 嵌套在 ComfyUI 内
    // - 嵌套问题：ComfyUI 是 git 仓库，venv 在内部会被 git status 看到
    //            切换 ComfyUI 版本（决策 5 工作区干净）时易冲突
    // - v1.8 / F38：改为跟随 app_data_dir（dev → <project_root>/data/venv，prod → <exe_dir>/data/venv）
    //   与 launcher 一起走 portable 模式
    if (!form.venv_path) {
      form.venv_path = `${loc.venv_path_default}`;
    }
  } catch (e) {
    console.warn("[onboarding] failed to get data location:", e);
    // 兜底：老逻辑
    try {
      const workDir = await configLauncherWorkingDir();
      if (!form.comfyui_root) {
        form.comfyui_root = `${workDir}/ComfyUI`;
      }
      if (!form.venv_path) {
        form.venv_path = `${workDir}/venv`;
      }
    } catch (e2) {
      console.warn("[onboarding] fallback to launcher working dir failed:", e2);
    }
  }

  // v3.10：异步加载 tags + 预估默认版本（不阻塞主流程）
  // 用途：在向导第 1 步提示用户"将安装 X 版本"，避免首次装到 v1.0.0
  // 失败不阻塞：仅 console.warn
  await loadExpectedDefaultVersion();
});

/**
 * v3.10：加载 tags 并计算"将安装的默认版本"
 *
 * 优先级：
 * 1. Config.paths.installation_default_version（用户显式）→ 直接展示
 * 2. 自动规则（与后端 tags::latest_stable_for_installation 同款）→ 拉 tags 计算
 * 3. 加载失败 → 留 null，UI 显示"将自动选择"
 */
async function loadExpectedDefaultVersion() {
    versionHintLoading.value = true;
    try {
        // 先取 Config（用 store 缓存，没有就 load）
        const cfg = configStore.config ?? (await configStore.load());
        const override = cfg?.paths?.installation_default_version;
        if (override && override.length > 0) {
            expectedDefaultVersion.value = override;
            return;
        }
        // 拉 tags
        const classified = await coreListTagsClassified(false);
        const allTags = [...classified.stable, ...classified.prerelease];
        const computed = latestStableForInstallation(allTags);
        expectedDefaultVersion.value = computed;
    } catch (e) {
        console.warn("[onboarding] loadExpectedDefaultVersion failed:", e);
        // 不弹错，留 null 让 UI 走"将自动选择"分支
    } finally {
        versionHintLoading.value = false;
    }
}

// ========== Computed ==========
const canNext = computed(() => {
  switch (currentStep.value) {
    case 0: // 根目录
      return form.comfyui_root.trim().length > 0;
    case 1: // venv
      return (
        form.venv_path.trim().length > 0 &&
        form.venv_path !== form.comfyui_root
      );
    case 2: // 模式
      return true;
    case 3: // 环境初始化
      return true; // 可继续到第 5 步浏览依赖安装
    case 4: // 安装 ComfyUI 依赖
      return false; // 最后一步无 next
    default:
      return false;
  }
});

/** 配置组 NSteps 的 current（1-3，仅显示组内 1-3 步） */
const currentStepConfig = computed(() =>
  currentStep.value < 3 ? currentStep.value + 1 : 3,
);

/** 安装组 NSteps 的 current（1-2，仅显示组内 1-2 步） */
const currentStepInstall = computed(() => {
  if (currentStep.value < 3) return 1; // 还在配置组时，安装组全部"待办"
  return currentStep.value - 2; // 3→1, 4→2
});

/** 步骤状态（error 时两组全标 error） */
const stepsStatus = computed(() =>
  initError.value ? ("error" as const) : ("process" as const),
);

// ========== Actions ==========

function next() {
  if (!canNext.value) return;
  currentStep.value = Math.min(4, currentStep.value + 1);
}

function prev() {
  currentStep.value = Math.max(0, currentStep.value - 1);
}

async function saveConfig() {
  /**
   * 部分更新 Config（不重置未传字段）。
   * 与后端 `config_update` 配合：Patch 结构 + apply_*_patch。
   */
  await configStore.update({
    paths: {
      comfyui_root: form.comfyui_root,
      venv_path: form.venv_path,
      python_version: form.python_version,
    },
    launch: {
      mode: form.mode,
    },
    torch: {
      cuda_version: form.cuda_version,
    },
  });
}

/**
 * 跳过向导：仅保存配置，跳转主界面（不触发任何后端长任务）。
 *
 * 行为：
 * - saveConfig 失败 → 弹错并停留向导页
 * - 成功 → toast「已跳过向导」 + 跳 /launch + 后台异步 clone
 *
 * 后台 clone 失败由 ensureClonedInBackground 内部 toast ，
 * 不会让 skipOnboarding 报「保存失败」（因为 saveConfig 实际成功了）。
 */
async function skipOnboarding() {
  try {
    await saveConfig();
    toast.info("已跳过向导，配置已保存");
    router.push("/launch");
    // 后台异步触发 clone（不阻塞跳转；失败由内部 catch + toast）
    void ensureClonedInBackground();
  } catch (e) {
    toast.error("保存配置失败", e);
  }
}

/**
 * 后台异步确保 ComfyUI 仓库已克隆（用于跳过/完成场景）
 *
 * 注意：clone 失败不影响用户主流程（用户已经在主界面了）。
 * 因此所有错误都走 toast，不抛出。
 *
 * v3.10 增强：clone 成功后从 coreStore.status 取实际安装版本，
 * 让用户知道"装到 v0.3.x 了"，而不是空泛的"克隆成功"
 */
async function ensureClonedInBackground() {
  if (cloning.value) return;
  cloning.value = true;
  cloneStage.value = "检查 ComfyUI 仓库...";
  try {
    await coreStore.ensureCloned();
    // 取实际安装版本（ensureCloned 内部已 refresh，status 是最新的）
    const installedTag = coreStore.status?.current_version;
    if (installedTag) {
      toast.success(`ComfyUI 已克隆并切到 ${installedTag}`);
    } else {
      toast.success("ComfyUI 仓库克隆完成");
    }
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    // NotEmptyDir 是预期内的错误（用户用了非空目录），不弹错
    if (msg.includes("NotEmptyDir")) {
      console.info("[onboarding] ComfyUI 根目录已存在但非仓库，跳过 clone");
    } else {
      toast.error("ComfyUI 仓库克隆失败", e);
    }
  } finally {
    cloning.value = false;
    cloneStage.value = "";
  }
}

/**
 * 完成向导：保存配置 + 完整初始化环境
 *
 * v3.11 关键改造：
 * 1. **install_requirements 改为阻塞步骤**（失败即终止）
 *    - 之前只是"警告"+"让用户去设置页补装"，违反"一次性装好"的用户期望
 *    - 现在 5 个阶段全部串行：save → venv → torch → requirements → refresh
 *    - 任一失败抛 `InitStageError`，函数 reject 到 catch 统一处理
 *
 * 2. **实时进度 + ETA + 实时日志**
 *    - 通过 `useTaskProgress` 订阅 task_progress / task_log 事件
 *    - 进度区间：save 5% / venv 5-15% / torch 15-60% / requirements 60-95% / refresh 95-100%
 *    - ETA = elapsed_seconds / progress × (100 - progress)
 *
 * 错误按阶段分别 catch + 明确文案：
 * - 阶段 1：保存配置失败 → 「保存配置失败：<msg>」
 * - 阶段 2：创建 venv 失败 → 「创建虚拟环境失败：<msg>」
 * - 阶段 3：安装 torch 失败 → 「安装 PyTorch 失败：<msg>」
 * - 阶段 4：安装依赖失败 → 「安装 ComfyUI 依赖失败：<msg>」**（v3.11：致命）**
 * - 阶段 5：刷新环境信息失败 → 「环境校验失败：<msg>」（不致命，仅提示）
 *
 * 每次重试前会清空 initError，避免错误提示永久残留。
 */
async function finishWithInit() {
  initializing.value = true;
  initError.value = null; // 重试时清空
  initProgress.value = 0;
  initStage.value = "准备开始...";
  initEtaSeconds.value = null;
  initRecentLogs.value = [];
  initStartMs.value = Date.now();
  // 重置 task tracker（清理之前的监听器）
  taskTracker.reset();

  try {
    // ========== 阶段 1: 保存配置 ==========
    initStage.value = "保存配置中...";
    try {
      await saveConfig();
    } catch (e) {
      throw new InitStageError("保存配置失败", e);
    }
    initProgress.value = 5;
    recomputeEta();

    // ========== 阶段 2: 创建 venv（5% → 15%）==========
    initStage.value = "创建 Python 虚拟环境（venv）...";
    try {
      const venvTaskId = await envCreateVenv(form.python_version);
      await runTrackedTask(venvTaskId, 5, 15, "创建 venv");
    } catch (e) {
      throw new InitStageError("创建虚拟环境失败", e);
    }

    // ========== 阶段 3: 安装 torch（15% → 60%，CPU 模式跳过）==========
    if (form.mode !== "cpu") {
      initStage.value = `安装 PyTorch（${form.cuda_version}）...`;
      try {
        const torchTaskId = await envInstallTorch(form.cuda_version);
        await runTrackedTask(torchTaskId, 15, 60, `安装 PyTorch (${form.cuda_version})`);
      } catch (e) {
        throw new InitStageError("安装 PyTorch 失败", e);
      }
    } else {
      // CPU 模式：把 torch 区间让给 requirements
      initProgress.value = 15;
      recomputeEta();
    }

    // ========== 阶段 4: 安装 ComfyUI 依赖（v3.11：阻塞）==========
    // ComfyUI 启动需要 requirements.txt 里的 8+ 个依赖
    // (torchsde / safetensors / transformers / tokenizers / kornia /
    //  spandrel / aiohttp / pydantic 等)
    // 幂等：uv pip install -r 自动跳过已满足的包
    // v3.11 改造：失败即终止（之前只是 warn）
    initStage.value = "安装 ComfyUI 依赖（requirements.txt）...";
    try {
      const reqTaskId = await envInstallRequirements();
      await runTrackedTask(reqTaskId, 60, 95, "安装 ComfyUI 依赖");
    } catch (e) {
      throw new InitStageError(
        "安装 ComfyUI 依赖失败",
        "可稍后到「设置 → 路径配置」补装，或点击「重试」重新安装",
      );
    }

    // ========== 阶段 5: 刷新环境信息（95% → 100%，非致命）==========
    initStage.value = "校验环境...";
    initProgress.value = 95;
    recomputeEta();
    try {
      await envStore.refresh();
    } catch (e) {
      // 不抛错 — 仅仅是 UI 状态可能滞后，下次手动刷新即可
      console.warn("[onboarding] post-init refresh failed:", e);
    }
    initProgress.value = 100;
    initEtaSeconds.value = 0;
    initStage.value = "环境初始化完成";

    toast.success("环境初始化完成");
    setTimeout(() => {
      router.push("/launch");
    }, 500);
    // 后台异步触发 clone（不阻塞跳转）
    void ensureClonedInBackground();
  } catch (e) {
    // InitStageError 自带中文 prefix；其他错误统一归为「初始化失败」
    const msg = e instanceof InitStageError
      ? `${e.stage}: ${e.cause instanceof Error ? e.cause.message : String(e.cause)}`
      : e instanceof Error
        ? e.message
        : String(e);
    initError.value = msg;
    toast.error("初始化失败", msg);
    // 不重置 initProgress — 让用户看到失败在哪个阶段
  } finally {
    initializing.value = false;
    // 清理 task tracker
    taskTracker.cleanup();
  }
}

/**
 * v3.11 辅助：跟踪一个后端任务，把它的进度映射到 [start, end] 区间
 *
 * 行为：
 * - 调 useTaskProgress.trackTask 订阅 task_progress / task_log
 * - onProgress：把任务自身的 0-100 缩放到 [start, end] 区间，更新 initProgress
 *                同步更新 initStage（用后端 message）
 * - onLog：把日志追加到 initRecentLogs（保留最近 5 行）
 * - onError：抛出带 message 的 Error（被外层 catch 转为 InitStageError）
 * - resolve on onComplete
 */
async function runTrackedTask(
  taskId: string,
  startPct: number,
  endPct: number,
  stageLabel: string,
): Promise<void> {
  const range = endPct - startPct;

  await new Promise<void>((resolve, reject) => {
    taskTracker.trackTask(taskId, {
      onProgress: (subProgress, message) => {
        const mapped = startPct + Math.floor((subProgress / 100) * range);
        initProgress.value = mapped;
        if (message) {
          initStage.value = `${stageLabel} - ${message}`;
        } else {
          initStage.value = stageLabel;
        }
        recomputeEta();
      },
      onLog: (log) => {
        // 追加到最近日志（保留最近 5 行）
        const next = initRecentLogs.value.concat([log]);
        if (next.length > 5) {
          next.splice(0, next.length - 5);
        }
        initRecentLogs.value = next;
      },
      onComplete: () => {
        // 完成后把进度推到 end（onProgress 可能没到 100）
        initProgress.value = endPct;
        recomputeEta();
        resolve();
      },
      onError: (summary) => {
        reject(new Error(summary ?? `${stageLabel} 失败`));
      },
    });
  });
}

/**
 * v3.11 辅助：基于「已用时间 / 当前进度」计算 ETA
 *
 * 公式：total = elapsed / (progress / 100); remaining = total - elapsed
 *
 * 注意：
 * - progress <= 0 时返回 null（不显示 ETA）
 * - progress >= 100 时返回 0（已就绪）
 * - 用 Math.max(0, ...) 避免 elapsed > total 的边缘情况（任务比预期快）
 */
function recomputeEta() {
  if (initProgress.value <= 0) {
    initEtaSeconds.value = null;
    return;
  }
  if (initProgress.value >= 100) {
    initEtaSeconds.value = 0;
    return;
  }
  const elapsedSec = (Date.now() - initStartMs.value) / 1000;
  const totalEstimate = elapsedSec / (initProgress.value / 100);
  const remaining = totalEstimate - elapsedSec;
  initEtaSeconds.value = Math.max(0, Math.round(remaining));
}

/**
 * v3.11 辅助：把秒数格式化为人类可读字符串
 *
 * - < 60s："约 X 秒"
 * - < 3600s："约 X 分 Y 秒"
 * - >= 3600s："约 X 小时 Y 分"
 */
function formatEta(seconds: number): string {
  if (seconds < 60) return `约 ${seconds} 秒`;
  const minutes = Math.floor(seconds / 60);
  const secs = seconds % 60;
  if (minutes < 60) {
    return secs > 0 ? `约 ${minutes} 分 ${secs} 秒` : `约 ${minutes} 分钟`;
  }
  const hours = Math.floor(minutes / 60);
  const mins = minutes % 60;
  return mins > 0 ? `约 ${hours} 小时 ${mins} 分` : `约 ${hours} 小时`;
}

/**
 * 仅保存配置，跳过环境初始化。
 *
 * 与「跳过向导」行为不同：本函数仅在向导第 4 步触发，配置保存后直接
 * 跳转主界面，不触发任何后台 clone（用户可以在启动页手工触发）。
 */
async function finishWithoutInit() {
  try {
    await saveConfig();
    toast.success("配置已保存，可稍后在启动页一键安装环境");
    router.push("/launch");
  } catch (e) {
    toast.error("保存配置失败", e);
  }
}

/**
 * 阶段化错误：在 finishWithInit 流程中标识失败发生在哪个阶段
 */
class InitStageError extends Error {
  constructor(
    public readonly stage: string,
    public readonly cause: unknown,
  ) {
    super(`${stage}: ${cause instanceof Error ? cause.message : String(cause)}`);
    this.name = "InitStageError";
  }
}
</script>

<template>
  <div class="onboarding-page">
    <NCard class="onboarding-card" :bordered="false">
      <template #header>
        <div class="card-header">
          <span class="header-icon">🚀</span>
          <span class="header-title">欢迎使用 无界启动器</span>
        </div>
      </template>

      <!-- v2.15 改版：5 步分两组显示，中间用分组分隔符 -->
      <div class="steps-group">
        <div class="steps-group-header">
          <span class="group-icon">📋</span>
          <span class="group-title">基础配置</span>
          <span class="group-hint">配置 ComfyUI 仓库与环境路径</span>
        </div>
        <NSteps
          :current="currentStepConfig"
          :status="stepsStatus"
          size="small"
          class="steps"
        >
          <NStep title="ComfyUI 根目录" description="ComfyUI 仓库克隆位置" />
          <NStep title="Python 环境" description="venv 虚拟环境路径" />
          <NStep title="运行模式" description="CPU / GPU / 自定义" />
        </NSteps>
      </div>

      <NDivider class="group-divider">
        <span class="divider-content">
          <span class="divider-icon">🛠</span>
          <span>环境安装</span>
        </span>
      </NDivider>

      <div class="steps-group">
        <div class="steps-group-header">
          <span class="group-icon">🛠</span>
          <span class="group-title">环境安装</span>
          <span class="group-hint">创建 venv + 装 PyTorch + 装依赖</span>
        </div>
        <NSteps
          :current="currentStepInstall"
          :status="stepsStatus"
          size="small"
          class="steps"
        >
          <NStep title="环境初始化" description="创建 venv + 安装 PyTorch" />
          <NStep title="安装 ComfyUI 依赖" description="requirements.txt 中的依赖" />
        </NSteps>
      </div>

      <!-- 步骤 1: ComfyUI 根目录 -->
      <div v-if="currentStep === 0" class="step-content">
        <NForm label-placement="top">
          <NFormItem label="ComfyUI 根目录">
            <FolderPicker
              v-model="form.comfyui_root"
              placeholder="如 D:\AIWork\ComfyUI"
              dialog-title="选择 ComfyUI 根目录"
              clearable
              size="medium"
            />
          </NFormItem>
          <NFormItem label="Python 版本">
            <NRadioGroup v-model:value="form.python_version">
              <NRadio value="3.10">3.10</NRadio>
              <NRadio value="3.11">3.11（推荐）</NRadio>
              <NRadio value="3.12">3.12</NRadio>
            </NRadioGroup>
          </NFormItem>
        </NForm>
        <NAlert type="info" :bordered="false">
          此目录将用于克隆 ComfyUI 仓库，建议预留 5GB 磁盘空间。
          <NSpace size="small" align="center" style="margin-top: 8px">
            <span>将安装版本：</span>
            <NTag v-if="expectedDefaultVersion" type="success" size="small">
              {{ expectedDefaultVersion }}
            </NTag>
            <NTag v-else-if="versionHintLoading" size="small">
              检测中...
            </NTag>
            <NTag v-else size="small">将自动选择最新稳定版</NTag>
            <span style="font-size: 12px; opacity: 0.7">
              （可在「设置 → 核心版本」修改默认规则）
            </span>
          </NSpace>
        </NAlert>
      </div>

      <!-- 步骤 2: venv 路径 -->
      <div v-if="currentStep === 1" class="step-content">
        <NForm label-placement="top">
          <NFormItem label="venv 虚拟环境路径">
            <FolderPicker
              v-model="form.venv_path"
              :placeholder="form.comfyui_root ? `${form.comfyui_root}/../venv` : '如 D:\\AIWork\\venv'"
              dialog-title="选择 venv 虚拟环境路径"
              clearable
              size="medium"
            />
          </NFormItem>
        </NForm>
        <NAlert
          :type="form.venv_path === form.comfyui_root ? 'warning' : 'info'"
          :bordered="false"
        >
          <span v-if="form.venv_path === form.comfyui_root">
            ⚠ venv 路径不能与 ComfyUI 根目录相同
          </span>
          <span v-else>
            <strong>建议：</strong>将 venv 放在
            <code>ComfyUI</code>
            外部（如与 ComfyUI 同级的 venv 目录）。<br />
            • 避免污染 ComfyUI 仓库的 git 状态<br />
            • 切换 ComfyUI 版本时无需处理 venv<br />
            venv 是 ComfyUI 专用的 Python 运行环境目录，程序会在此自动下载
            Python 并安装所需依赖（如 torch）。请选择空文件夹或新路径
            （程序会自动创建），请勿指向系统已有的 Python 安装目录。
          </span>
        </NAlert>
      </div>

      <!-- 步骤 3: 运行模式 -->
      <div v-if="currentStep === 2" class="step-content">
        <NRadioGroup v-model:value="form.mode" class="mode-group">
          <div class="mode-option">
            <NRadio value="cpu">CPU 模式（无 GPU 或仅测试）</NRadio>
            <span class="mode-hint">--cpu --lowvram</span>
          </div>
          <div class="mode-option">
            <NRadio value="gpu_high">GPU 高显存（推荐）</NRadio>
            <span class="mode-hint">--highvram</span>
          </div>
          <div class="mode-option">
            <NRadio value="gpu_low">GPU 低显存</NRadio>
            <span class="mode-hint">--lowvram</span>
          </div>
          <div class="mode-option">
            <NRadio value="gpu_no_vram">GPU 无显存</NRadio>
            <span class="mode-hint">--novram</span>
          </div>
          <div class="mode-option">
            <NRadio value="custom">自定义（高级用户）</NRadio>
            <span class="mode-hint">在设置页填写 custom_args</span>
          </div>
        </NRadioGroup>

        <NForm v-if="form.mode !== 'cpu'" label-placement="top" class="cuda-form">
          <NFormItem label="CUDA 版本">
            <NRadioGroup v-model:value="form.cuda_version">
              <NRadio value="cu118">CUDA 11.8</NRadio>
              <NRadio value="cu126">CUDA 12.6</NRadio>
              <NRadio value="cu128">CUDA 12.8（推荐）</NRadio>
              <NRadio value="cu130">CUDA 13.0（最新）</NRadio>
            </NRadioGroup>
          </NFormItem>
        </NForm>
      </div>

      <!-- 步骤 4: 环境初始化（仅展示） -->
      <div v-if="currentStep === 3" class="step-content">
        <NAlert type="info" :bordered="false" class="init-intro">
          <p><strong>即将执行：</strong></p>
          <ol>
            <li>保存配置到 config.toml</li>
            <li>创建 Python 虚拟环境（venv）</li>
            <li v-if="form.mode !== 'cpu'">安装 PyTorch（{{ form.cuda_version }}）</li>
            <li>
              安装 ComfyUI 依赖（<code>requirements.txt</code>）— v3.11 新增，必装项
            </li>
            <li>校验环境（refresh envInfo）</li>
          </ol>
          <p>预计耗时 8-20 分钟，取决于网络速度。</p>
          <p class="hint">
            <strong>v3.11 升级：</strong>
            安装依赖是「阻塞步骤」（失败即终止），确保引导完成后
            <code>isReady</code> 直接为 true，无需再到「设置页」补装。
          </p>
        </NAlert>
      </div>

      <!-- 步骤 5: 安装 ComfyUI 依赖（预览） -->
      <div v-if="currentStep === 4" class="step-content">
        <NAlert type="info" :bordered="false" class="init-intro">
          <p><strong>第 5 步将执行：</strong></p>
          <ol>
            <li>读取 ComfyUI 仓库根目录下的 <code>requirements.txt</code></li>
            <li>用 uv 装入 8+ 个关键依赖（torchsde / safetensors / transformers / tokenizers / kornia / spandrel / aiohttp / pydantic 等）</li>
            <li>幂等安装 — uv 自动跳过已满足的包</li>
          </ol>
          <p>
            预计耗时 3-10 分钟（取决于网络与缺失包数量）。
            失败时可在「设置页 → 路径配置」一键补装。
          </p>
        </NAlert>
      </div>

      <!-- v3.11.1 修复：全局进度区（不受 currentStep 限制）-->
      <!-- 之前进度条在 currentStep=3 容器内，用户在 currentStep=4 点"开始初始化"时
           进度条所在的 div 被 v-if 隐藏，导致用户看不到进度。
           改为提到外面，仅受 initializing / initError 控制显示。 -->
      <div
        v-if="initializing || initError"
        class="init-progress-global"
      >
        <div v-if="initializing" class="init-progress">
          <div class="init-stage-row">
            <NSpin size="small" />
            <div class="init-stage">{{ initStage }}</div>
          </div>
          <NProgress
            type="line"
            :percentage="initProgress"
            :height="12"
            :bordered="false"
            :show-indicator="true"
            :indicator-placement="'inside'"
          />
          <div class="init-meta">
            <span class="init-percent">{{ initProgress }}%</span>
            <span
              v-if="initEtaSeconds !== null && initEtaSeconds > 0"
              class="init-eta"
            >
              ⏱ 预计剩余 {{ formatEta(initEtaSeconds) }}
            </span>
            <span
              v-else-if="initProgress >= 100"
              class="init-eta init-eta-done"
            >
              ✓ 完成
            </span>
          </div>
          <!-- v3.11 实时日志面板：最近 5 行 uv 子进程输出 -->
          <NScrollbar
            v-if="initRecentLogs.length > 0"
            class="init-logs"
            :max-height="120"
            style="margin-top: 8px"
          >
            <div
              v-for="(log, idx) in initRecentLogs"
              :key="idx"
              class="init-log-line"
            >
              <span v-if="log.source" class="log-source">[{{ log.source }}]</span>
              <span class="log-text">{{ log.text }}</span>
            </div>
          </NScrollbar>
        </div>

        <div v-if="initError" class="init-error">
          <NAlert type="error" :bordered="false">
            <strong>初始化失败：</strong>{{ initError }}
          </NAlert>
        </div>
      </div>

      <!-- 底部按钮区 -->
      <div class="footer-actions">
        <NButton @click="skipOnboarding" :disabled="initializing" quaternary>
          跳过向导
        </NButton>
        <NSpace>
          <NButton
            v-if="currentStep > 0 && currentStep < 4"
            @click="prev"
            :disabled="initializing"
          >
            上一步
          </NButton>
          <NButton
            v-if="currentStep < 4"
            type="primary"
            :disabled="!canNext"
            @click="next"
          >
            {{ currentStep < 3 ? "下一步" : "下一步（浏览）" }}
          </NButton>
          <NButton
            v-if="currentStep === 4"
            type="default"
            :disabled="initializing"
            @click="finishWithoutInit"
          >
            跳过初始化
          </NButton>
          <NButton
            v-if="currentStep === 4"
            type="primary"
            :loading="initializing"
            :disabled="initializing || !!initError"
            @click="finishWithInit"
          >
            开始初始化
          </NButton>
        </NSpace>
      </div>
    </NCard>
  </div>
</template>

<style scoped>
.onboarding-page {
  min-height: 100vh;
  display: flex;
  align-items: center;
  justify-content: center;
  background: var(--app-bg-muted, #f5f5f5);
  padding: 24px;
}

.onboarding-card {
  width: 640px;
  max-width: 100%;
}

.card-header {
  display: flex;
  align-items: center;
  gap: 8px;
}

.header-icon {
  font-size: 24px;
}

.header-title {
  font-weight: 600;
  font-size: 18px;
}

.steps {
  margin: 8px 0 16px;
}

.steps-group {
  padding: 12px 0 4px;
}

.steps-group-header {
  display: flex;
  align-items: baseline;
  gap: 8px;
  margin-bottom: 8px;
  padding: 0 4px;
}

.group-icon {
  font-size: 16px;
}

.group-title {
  font-size: 14px;
  font-weight: 600;
  color: var(--app-fg, #333);
}

.group-hint {
  font-size: 12px;
  color: var(--app-fg-muted, #888);
}

.group-divider {
  margin: 12px 0 8px !important;
}

.divider-content {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
  font-weight: 500;
  color: var(--app-fg-muted, #888);
  background: var(--app-bg-muted, #f5f5f5);
  padding: 0 4px;
}

.divider-icon {
  font-size: 14px;
}

.step-content {
  min-height: 180px;
  padding: 16px 0;
}

.mode-group {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.mode-option {
  display: flex;
  align-items: center;
  gap: 12px;
}

.mode-hint {
  font-size: 12px;
  opacity: 0.6;
  font-family: monospace;
}

.cuda-form {
  margin-top: 16px;
}

.init-intro ol {
  margin: 8px 0 8px 20px;
  padding: 0;
}

/* v3.11.1 修复：全局进度区（独立于 currentStep） */
.init-progress-global {
  margin-top: 16px;
  padding: 12px 16px;
  background: var(--app-bg-muted, #f7f7f7);
  border-radius: 6px;
  border: 1px solid var(--app-border, #e0e0e0);
}

.init-progress {
  display: flex;
  flex-direction: column;
  gap: 8px;
  align-items: stretch;
  padding: 4px 0;
}

.init-stage-row {
  display: flex;
  align-items: center;
  gap: 8px;
}

.init-stage {
  font-size: 14px;
  color: var(--app-fg, #333);
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.init-meta {
  display: flex;
  justify-content: space-between;
  align-items: center;
  font-size: 12px;
  color: var(--app-fg-muted, #888);
  margin-top: -2px;
}

.init-percent {
  font-weight: 600;
  color: var(--app-fg, #333);
  font-family: monospace;
}

.init-eta {
  display: inline-flex;
  align-items: center;
  gap: 4px;
}

.init-eta-done {
  color: #18a058;
  font-weight: 600;
}

.init-logs {
  background: var(--app-bg, #1e1e1e);
  color: #d4d4d4;
  border-radius: 4px;
  padding: 8px 10px;
  font-family: ui-monospace, "Cascadia Code", "Source Code Pro", monospace;
  font-size: 12px;
  line-height: 1.5;
}

.init-log-line {
  display: flex;
  gap: 6px;
  white-space: pre-wrap;
  word-break: break-all;
}

.log-source {
  color: var(--app-primary, #2080f0);
  font-weight: 600;
  flex-shrink: 0;
}

.log-text {
  color: inherit;
  flex: 1;
  min-width: 0;
}

.init-error {
  margin-top: 12px;
}

.footer-actions {
  margin-top: 24px;
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding-top: 16px;
  border-top: 1px solid var(--app-border, #e0e0e0);
}
</style>
