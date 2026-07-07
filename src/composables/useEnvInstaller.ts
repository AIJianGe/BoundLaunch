/**
 * useEnvInstaller — 共享的环境补装逻辑（v2.14）
 *
 * 设计模式：**Strategy Pattern**
 * - 同一份「按 missing_steps 顺序执行」的逻辑被多处复用：
 *   - StartStopButtons.vue（首页启动/停止按钮）
 *   - PathsPanel.vue（设置页「路径配置」面板顶部）
 *   - OnboardingPage.vue（onboarding 流程内部）
 *
 * 提取为 composable 避免代码重复，保证行为一致。
 *
 * 幂等性保证：
 * - uv pip install 对已满足的包自动跳过
 * - ensureCloned 检查 .git 目录，已存在则跳过
 * - 后端 PythonEnvService 持有 op_lock Mutex 防止并发
 * - 上层 installing.value 锁防止前端重复点击
 */

import { ref } from "vue";
import { useEnvStore } from "@/stores/env";
import { useCoreStore } from "@/stores/core";
import { useToast } from "@/composables/useToast";

export interface UseEnvInstallerReturn {
  /** 是否正在执行补装流程（用于按钮 loading 态） */
  installing: ReturnType<typeof ref<boolean>>;
  /** 当前正在执行的步骤描述（用于进度提示） */
  currentStep: ReturnType<typeof ref<string>>;
  /**
   * 按 envStore.readiness.missing_steps 顺序执行补装
   * @returns 是否全部成功（true = 环境已就绪）
   */
  installMissingSteps: () => Promise<boolean>;
}

export function useEnvInstaller(): UseEnvInstallerReturn {
  const envStore = useEnvStore();
  const coreStore = useCoreStore();
  const toast = useToast();

  const installing = ref(false);
  const currentStep = ref("");

  /** 复用的步骤 → store action 映射 */
  const stepActions: Record<string, () => Promise<void>> = {
    CloneComfyUI: async () => {
      await coreStore.ensureCloned();
    },
    CreateVenv: async () => {
      const step = envStore.readiness?.missing_steps.find(
        (s) => s.kind === "CreateVenv",
      );
      const pythonVersion =
        (step && "params" in step && step.params?.python_version) ||
        "3.11";
      await envStore.createVenv(pythonVersion);
    },
    InstallTorch: async () => {
      const step = envStore.readiness?.missing_steps.find(
        (s) => s.kind === "InstallTorch",
      );
      const cudaVersion =
        (step && "params" in step && step.params?.cuda_version) || "cu128";
      await envStore.installTorch(cudaVersion);
    },
    InstallRequirements: async () => {
      await envStore.installRequirements();
    },
  };

  const stepLabels: Record<string, string> = {
    CloneComfyUI: "克隆 ComfyUI 仓库",
    CreateVenv: "创建 Python 虚拟环境",
    InstallTorch: "安装 PyTorch",
    InstallRequirements: "安装 ComfyUI 依赖",
  };

  async function installMissingSteps(): Promise<boolean> {
    if (installing.value) return false;
    const steps = envStore.readiness?.missing_steps ?? [];
    if (steps.length === 0) {
      toast.info("环境已就绪");
      return true;
    }

    installing.value = true;
    try {
      for (const step of steps) {
        const action = stepActions[step.kind];
        const label = stepLabels[step.kind] ?? step.kind;
        currentStep.value = label;
        if (!action) {
          console.warn(`[useEnvInstaller] 未知步骤: ${step.kind}，跳过`);
          continue;
        }
        try {
          await action();
        } catch (e) {
          // 单步骤失败：终止整个流程，保留错误
          currentStep.value = "";
          throw e;
        }
      }
      // 重新校验
      await envStore.checkReadiness();
      if (envStore.readiness?.ready) {
        toast.success("环境已就绪，可以启动 ComfyUI");
        return true;
      } else {
        const remaining = envStore.readiness?.missing_steps ?? [];
        const remainingText = remaining
          .map((s) => stepLabels[s.kind] ?? s.kind)
          .join("、");
        toast.warn(`环境未完全就绪，剩余：${remainingText}`);
        return false;
      }
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      toast.error("环境补装失败", msg);
      return false;
    } finally {
      installing.value = false;
      currentStep.value = "";
    }
  }

  return {
    installing,
    currentStep,
    installMissingSteps,
  };
}
