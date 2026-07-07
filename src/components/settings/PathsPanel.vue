<script setup lang="ts">
/**
 * 路径配置面板
 *
 * 详见 `PR/06-界面设计.md §5.3 设置页 - 路径配置`
 *
 * 字段：
 * - comfyui_root：ComfyUI 仓库克隆位置
 * - venv_path：Python 虚拟环境路径
 * - python_version：目标 Python 版本（仅记录，实际切换在 PythonVersionPanel）
 * - models_path：自定义 models 路径（v3.1 / F26 决策 12，可选）
 *
 * 行为：
 * - 输入防抖 500ms 后调用 configStore.update
 * - 实时校验：父目录可写 / 路径不互相重复
 * - models_path 修改后自动调用 coreEnsureModelsLink 建立软链接
 *
 * 设计模式：
 * - **Repository**：通过 configStore 持久化
 * - **Validator**：实时校验
 */

import { ref, computed, watch, onMounted } from "vue";
import {
  NCard,
  NForm,
  NFormItem,
  NSelect,
  NAlert,
  NSpace,
  NTooltip,
  NButton,
  NSpin,
  NInput,
  NTag,
} from "naive-ui";
import { useConfigStore } from "@/stores/config";
import { useEnvStore } from "@/stores/env";
import { useCoreStore } from "@/stores/core";
import { useToast } from "@/composables/useToast";
import { useEnvInstaller } from "@/composables/useEnvInstaller";
import { useConfirm } from "@/composables/useConfirm";
import { coreListTagsClassified } from "@/api/core";
import { latestStableForInstallation } from "@/composables/useTagRules";
import FolderPicker from "@/components/FolderPicker.vue";
import RepoUrlDialog from "@/components/settings/RepoUrlDialog.vue";
import RepairWizard from "@/components/settings/RepairWizard.vue";

const configStore = useConfigStore();
const envStore = useEnvStore();
const coreStore = useCoreStore();
const toast = useToast();
const { confirm: showConfirm } = useConfirm();

/** 上一保存的 venv 路径（用于切换时检测"路径真的变了"） */
let lastSavedVenv = "";
const {
  installing: installingEnv,
  currentStep: installStepText,
  installMissingSteps,
} = useEnvInstaller();

const pythonVersionOptions = [
  { label: "3.10", value: "3.10" },
  { label: "3.11", value: "3.11" },
  { label: "3.12", value: "3.12" },
];

const localRoot = ref("");
const localVenv = ref("");
const localPython = ref("3.11");
/** 自定义 models 路径（v3.1 / F26 决策 12，空字符串 = 用默认） */
const localModelsPath = ref("");
/** v3.10：引导安装默认版本（空字符串 = 走自动规则） */
const localDefaultVersion = ref("");

/**
 * v3.10：自动计算的默认版本（用于"自动模式"展示）
 *
 * 来源：coreListTagsClassified → latestStableForInstallation
 */
const autoDefaultVersion = ref<string | null>(null);
const autoDefaultLoading = ref(false);
/**
 * v3.2：venv 路径是否刚切换（用于 NAlert 按钮文案上下文感知）
 *
 * - 用户在 PathsPanel 切换 venv 路径后置 true
 * - 用户点 NAlert 按钮 / env 已就绪后置 false
 * - 控制按钮文案：「一键补装」 vs 「重新安装环境」
 */
const venvPathJustChanged = ref(false);
const debounceTimers: Record<string, ReturnType<typeof setTimeout>> = {};

watch(
  () => configStore.config,
  (cfg) => {
    if (cfg) {
      localRoot.value = cfg.paths.comfyui_root;
      localVenv.value = cfg.paths.venv_path;
      localPython.value = cfg.paths.python_version;
      localModelsPath.value = cfg.paths.models_path ?? "";
      localDefaultVersion.value = cfg.paths.installation_default_version ?? "";
      // 首次加载时初始化 lastSavedVenv
      if (!lastSavedVenv) {
        lastSavedVenv = cfg.paths.venv_path;
      }
    }
  },
  { immediate: true },
);

const pathConflict = computed(() => {
  if (!localRoot.value || !localVenv.value) return false;
  return localRoot.value === localVenv.value;
});

const rootEmpty = computed(() => !localRoot.value.trim());
const venvEmpty = computed(() => !localVenv.value.trim());

const hasError = computed(() => pathConflict.value || rootEmpty.value || venvEmpty.value);

function debouncedUpdate(
  field: "root" | "venv" | "python" | "models" | "defaultVersion",
  value: string,
) {
  if (debounceTimers[field]) clearTimeout(debounceTimers[field]);
  debounceTimers[field] = setTimeout(async () => {
    try {
      if (field === "root") {
        await configStore.update({ paths: { comfyui_root: value } });
      } else if (field === "venv") {
        // v3.2：切换 venv 路径时让用户确认
        const oldPath = lastSavedVenv;
        const isPathChanged = !!(oldPath && oldPath !== value);

        if (isPathChanged) {
          const oldPathStr = oldPath;
          const newPathStr = value;
          const confirmed = await showConfirm({
            title: "切换 venv 路径",
            content:
              "切换 venv 路径后：\n\n" +
              "• 旧路径的 Python 包将不再被使用\n" +
              "• 新路径下如未检测到 venv，需要创建并安装依赖\n" +
              "  （含 torch + ComfyUI requirements.txt，预计 5-15 分钟）\n\n" +
              "旧：" +
              oldPathStr +
              "\n" +
              "新：" +
              newPathStr +
              "\n\n立即开始安装？",
            positiveText: "立即开始",
            negativeText: "稍后手动装",
          });

          // v3.2 关键修复：无论选哪个，路径都切换
          // 区别只在于"是否自动调 installMissingSteps"
          lastSavedVenv = value;
          venvPathJustChanged.value = true; // 标记路径刚切换

          await configStore.update({ paths: { venv_path: value } });
          // 立即重新做 readiness，让 UI 反映新 venv 状态
          try {
            await envStore.invalidateCache();
            await envStore.refresh();
            await envStore.checkReadiness();
          } catch (e) {
            console.warn("[PathsPanel] post-venv-change readiness failed:", e);
          }

          if (confirmed) {
            // 选"立即开始"：直接调 installMissingSteps
            const ok = await installMissingSteps();
            if (ok) {
              venvPathJustChanged.value = false;
            }
          } else {
            // 选"稍后手动装"：保持 venvPathJustChanged=true，让 NAlert 按钮文案变为"重新安装环境"
            toast.info("新 venv 路径已保存，请点击「重新安装环境」按钮开始安装");
          }
          return;
        }

        // 路径未变化（首次输入 / 重新输入相同路径）
        lastSavedVenv = value;
        await configStore.update({ paths: { venv_path: value } });
        try {
          await envStore.invalidateCache();
          await envStore.refresh();
          await envStore.checkReadiness();
        } catch (e) {
          console.warn("[PathsPanel] post-venv-change readiness failed:", e);
        }
      } else if (field === "python") {
        await configStore.update({ paths: { python_version: value } });
      } else if (field === "models") {
        // v3.1 / F26 决策 12：保存 models_path 并建立软链接
        await configStore.update({ paths: { models_path: value || null } });
        // 调用 ensureModelsLink 建立软链接
        try {
          const linked = await coreStore.ensureModelsLink();
          if (value) {
            toast.success(
              linked
                ? `已建立 models 软链接到: ${value}`
                : "models 软链接已存在，无需重复建立",
            );
          } else {
            toast.info("已清除自定义 models 路径，将使用 ComfyUI 默认路径");
          }
        } catch (e) {
          toast.error("建立 models 软链接失败", e);
        }
      } else if (field === "defaultVersion") {
        // v3.10：保存 installation_default_version
        // 空字符串 = 清空（走自动规则）
        const trimmed = value.trim();
        if (trimmed) {
          // 校验 tag 存在（避免用户打错）
          try {
            const classified = await coreListTagsClassified(false);
            const all = [...classified.stable, ...classified.prerelease];
            const exists = all.some((t) => t.name === trimmed);
            if (!exists) {
              toast.error(
                `版本 ${trimmed} 不存在`,
                "请检查拼写或留空使用自动规则",
              );
              // 回退到当前值
              localDefaultVersion.value =
                configStore.config?.paths.installation_default_version ?? "";
              return;
            }
          } catch (e) {
            console.warn("[PathsPanel] tag validation failed:", e);
            // 校验失败不阻塞保存（后端会兜底）
          }
          await configStore.update({
            paths: { installation_default_version: trimmed },
          });
          toast.success(`默认安装版本已锁定为 ${trimmed}`);
        } else {
          await configStore.update({
            paths: { installation_default_version: "" },
          });
          toast.info("已切换为自动规则");
        }
      }
    } catch (e) {
      toast.error("保存失败", e);
    }
  }, 500);
}

// v2.14：环境检测与补装
onMounted(async () => {
  // 初次进入面板时做一次 readiness 检查
  try {
    await envStore.refresh();
    await envStore.checkReadiness();
  } catch (e) {
    console.warn("[PathsPanel] initial env check failed:", e);
  }
  // v3.10：加载自动计算的默认版本（用于 NTag 展示）
  await loadAutoDefaultVersion();
});

/**
 * v3.10：拉 tags 计算"自动模式"下的默认版本
 *
 * 用于 NTag 展示，让用户知道"留空时会装到哪个版本"。
 * 失败不阻塞：留 null，UI 隐藏自动提示。
 */
async function loadAutoDefaultVersion() {
  autoDefaultLoading.value = true;
  try {
    const classified = await coreListTagsClassified(false);
    const all = [...classified.stable, ...classified.prerelease];
    autoDefaultVersion.value = latestStableForInstallation(all);
  } catch (e) {
    console.warn("[PathsPanel] loadAutoDefaultVersion failed:", e);
  } finally {
    autoDefaultLoading.value = false;
  }
}

/** 环境是否完全就绪（readiness.ready === true） */
const envReady = computed(() => envStore.readiness?.ready ?? false);

/** missing_steps 数量（0 = 就绪） */
const missingCount = computed(
  () => envStore.readiness?.missing_steps.length ?? 0,
);

/** missing_steps 简明描述（按 kind 翻译为中文） */
const missingStepsText = computed(() => {
  const steps = envStore.readiness?.missing_steps ?? [];
  const labels: Record<string, string> = {
    CloneComfyUI: "克隆 ComfyUI 仓库",
    CreateVenv: "创建 Python 虚拟环境",
    InstallTorch: "安装 PyTorch",
    InstallRequirements: "安装 ComfyUI 依赖",
  };
  return steps.map((s) => labels[s.kind] ?? s.kind).join("、");
});

/** 点击「一键补装」按钮（v3.2：成功后清除 venvPathJustChanged 标记） */
async function onInstallMissing() {
  const ok = await installMissingSteps();
  if (ok) {
    venvPathJustChanged.value = false;
  }
}

/**
 * v3.10：点击「修复 torch 不一致」按钮
 *
 * 解决"Config 写 cu128，但 venv 中 torch 是 +cpu"这种 mismatch 问题。
 * 用 `--force-reinstall --no-deps --index-url pytorch.org` 强制覆盖重装
 * torch/torchvision/torchaudio，**不破坏 venv 中的其他包**。
 *
 * 进度：1-3 分钟（含下载），异步执行，可观察 TaskPanel 进度。
 */
const isRepairingConsistent = ref(false);
async function onRepairConsistent() {
  if (isRepairingConsistent.value) return;
  const config = configStore.config;
  const cuda = config?.torch?.cuda_version ?? "cpu";
  const cudaLabel =
    cuda === "cpu" ? "CPU" : cuda.toUpperCase();
  const ok = await showConfirm({
    title: "修复 torch 一致性",
    content:
      `将强制重装 torch/torchvision/torchaudio（来源：pytorch.org ${cudaLabel} 源）。\n\n` +
      `操作：\n` +
      `1. 用 --force-reinstall --no-deps 覆盖现有 wheel\n` +
      `2. 装 torch 关键依赖（numpy/psutil/six/av/Pillow/pycocotools）\n` +
      `3. 重装 ComfyUI requirements（已过滤 torch 系列行）\n` +
      `4. smoke test 验证 torch.cuda.is_available()\n\n` +
      `预计耗时 1-3 分钟。`,
    positiveText: "开始修复",
    negativeText: "取消",
  });
  if (!ok) return;

  isRepairingConsistent.value = true;
  try {
    const cudaStr =
      cuda === "cpu" ? "cpu" : (cuda as string);
    await envStore.repairConsistent(cudaStr);
    toast.success("torch 强制一致重装完成");
  } catch (e) {
    toast.error("修复失败", String(e));
  } finally {
    isRepairingConsistent.value = false;
  }
}

/** NAlert 按钮文案（v3.2：上下文感知） */
const installButtonText = computed(() => {
  if (installingEnv.value) return installStepText.value;
  if (venvPathJustChanged.value) return "重新安装环境";
  return "一键补装";
});

/** F31：仓库地址管理对话框显示状态 */
const showRepoDialog = ref(false);

/** v1.8 / F36-Phase2：环境修复向导显示状态（深度诊断按钮触发） */
const showRepairWizard = ref(false);

/** v1.8 / F36-Phase2：torch 未安装（诊断修复按钮高亮） */
const torchBroken = computed(
  () => envStore.envInfo !== null && !envStore.envInfo.torch_installed,
);
</script>

<template>
  <NCard class="paths-panel" :bordered="true" size="small">
    <template #header>
      <span class="header-title">📁 路径配置</span>
    </template>

    <!-- v2.14：环境检测与补装入口 -->
    <div class="env-check-section">
      <NAlert
        v-if="envStore.isLoaded && envReady"
        type="success"
        :show-icon="true"
        :bordered="false"
        class="env-alert"
      >
        ✅ 环境已就绪，可以启动 ComfyUI
      </NAlert>
      <NAlert
        v-else-if="envStore.isLoaded && missingCount > 0"
        type="warning"
        :show-icon="true"
        :bordered="false"
        class="env-alert"
      >
        <div class="env-alert-content">
          <span>
            ⚠ 环境未完全就绪，缺失 {{ missingCount }} 项：
            <strong>{{ missingStepsText }}</strong>
          </span>
          <NButton
            size="small"
            type="warning"
            :loading="installingEnv"
            :disabled="installingEnv"
            @click="onInstallMissing"
          >
            <template #icon>
              <NSpin v-if="installingEnv" size="small" />
            </template>
            {{ installButtonText }}
          </NButton>
        </div>
      </NAlert>

      <!-- v1.8 / F36-Phase2：深度诊断入口（独立于一键补装，可诊断隐蔽问题如 numpy 坏版本） -->
      <NSpace :size="8" align="center" class="diagnose-row">
        <NButton
          size="small"
          :type="torchBroken ? 'error' : 'default'"
          :loading="envStore.repairing"
          :disabled="envStore.repairing"
          @click="showRepairWizard = true"
        >
          🔧 {{ torchBroken ? "诊断修复（推荐）" : "深度诊断" }}
        </NButton>
        <span class="diagnose-hint">
          扫描 venv + torch import + 关键依赖，定位隐蔽问题并自动修复
        </span>
      </NSpace>

      <!-- v3.10：torch 一致性修复（修复 Config 与 venv 不一致） -->
      <NSpace :size="8" align="center" class="diagnose-row" style="margin-top: 8px;">
        <NButton
          size="small"
          type="warning"
          :loading="isRepairingConsistent"
          :disabled="isRepairingConsistent || envStore.repairing"
          @click="onRepairConsistent"
        >
          🔄 修复 torch 不一致
        </NButton>
        <span class="diagnose-hint">
          强制从 pytorch.org 源覆盖重装 torch/torchvision/torchaudio（解决"venv 是混乱状态"问题）
        </span>
      </NSpace>
    </div>

    <NForm label-placement="top" :show-feedback="false" size="small">
      <NFormItem>
        <template #label>
          <span class="label-with-help">
            ComfyUI 根目录
            <NButton
              size="tiny"
              quaternary
              type="primary"
              class="repo-url-btn"
              @click="showRepoDialog = true"
            >
              仓库地址管理
            </NButton>
          </span>
        </template>
        <FolderPicker
          v-model="localRoot"
          placeholder="如 D:\AIWork\ComfyUI"
          :status="rootEmpty ? 'error' : 'success'"
          dialog-title="选择 ComfyUI 根目录"
          clearable
          @update:model-value="(v) => debouncedUpdate('root', v)"
        />
      </NFormItem>

      <NFormItem>
        <template #label>
          <span class="label-with-help">
            venv 路径
            <NTooltip placement="top" trigger="hover">
              <template #trigger>
                <span class="help-icon" aria-label="venv 路径说明">?</span>
              </template>
              <div class="help-content">
                venv 是 ComfyUI 专用的 Python 运行环境目录，程序会在此自动下载
                Python 并安装所需依赖（如 torch）。请选择一个空文件夹或新路径
                （程序会自动创建），请勿指向系统已有的 Python 安装目录，以免冲突。
              </div>
            </NTooltip>
          </span>
        </template>
        <FolderPicker
          v-model="localVenv"
          placeholder="如 D:\AIWork\ComfyUI\venv"
          :status="venvEmpty || pathConflict ? 'error' : 'success'"
          dialog-title="选择 venv 路径"
          clearable
          @update:model-value="(v) => debouncedUpdate('venv', v)"
        />
      </NFormItem>

      <NFormItem label="Python 版本（仅记录，切换在下方面板）">
        <NSelect
          v-model:value="localPython"
          :options="pythonVersionOptions"
          @update:value="(v) => debouncedUpdate('python', v)"
        />
      </NFormItem>

      <NFormItem>
        <template #label>
          <span class="label-with-help">
            models 路径（可选）
            <NTooltip placement="top" trigger="hover">
              <template #trigger>
                <span class="help-icon" aria-label="models 路径说明">?</span>
              </template>
              <div class="help-content">
                自定义 models 目录路径（v3.1 / F26 决策 12）。<br /><br />
                <strong>留空</strong>：使用 ComfyUI 默认路径
                &lt;comfyui_root&gt;/models<br />
                <strong>指定路径</strong>：在 &lt;comfyui_root&gt;/models
                建立软链接（Windows 使用 junction，无需管理员权限）指向此路径。<br /><br />
                用途：跨 ComfyUI 版本共享模型文件，避免版本切换时重复下载。<br />
                注意：若 &lt;comfyui_root&gt;/models
                已是真实目录（非链接），需先迁移数据再删除目录，否则无法建立链接。
              </div>
            </NTooltip>
          </span>
        </template>
        <FolderPicker
          v-model="localModelsPath"
          placeholder="留空则使用 ComfyUI 默认 models 目录"
          dialog-title="选择 models 路径（将建立软链接）"
          clearable
          @update:model-value="(v) => debouncedUpdate('models', v)"
        />
      </NFormItem>

      <!-- v3.10：引导安装默认版本 -->
      <NFormItem>
        <template #label>
          <span class="label-with-help">
            引导安装默认版本
            <NTooltip placement="top" trigger="hover">
              <template #trigger>
                <span class="help-icon" aria-label="引导安装默认版本说明">?</span>
              </template>
              <div class="help-content">
                首次启动克隆 ComfyUI 后，自动 checkout 到的版本。<br /><br />
                <strong>留空</strong>：走自动规则（patch=0/1 + 跳过首次大版本 + SemVer 最大）<br />
                <strong>指定版本</strong>（如 v0.3.10）：跳过自动规则直接装到该版本<br /><br />
                自动规则会自动跳过 v1.0.0 / v2.0.0 等首次大版本发布（可能引入破坏性变更）。<br />
                当 ComfyUI 主版本升级时，可以先留空，等 v1.x 稳定后再锁定具体版本。
              </div>
            </NTooltip>
          </span>
        </template>
        <NSpace size="small" align="center" style="width: 100%">
          <NInput
            v-model:value="localDefaultVersion"
            placeholder="留空走自动规则"
            clearable
            style="max-width: 240px"
            @update:value="(v) => debouncedUpdate('defaultVersion', v)"
          />
          <NTag v-if="!localDefaultVersion && autoDefaultVersion" type="info" size="small">
            自动将安装: {{ autoDefaultVersion }}
          </NTag>
          <NTag v-else-if="!localDefaultVersion && autoDefaultLoading" size="small">
            检测中...
          </NTag>
          <NTag v-else-if="localDefaultVersion" type="warning" size="small">
            已锁定: {{ localDefaultVersion }}
          </NTag>
        </NSpace>
      </NFormItem>
    </NForm>

    <NSpace v-if="hasError" vertical :size="8" class="error-list">
      <NAlert v-if="rootEmpty" type="error" :bordered="false">
        ComfyUI 根目录不能为空
      </NAlert>
      <NAlert v-if="venvEmpty" type="error" :bordered="false">
        venv 路径不能为空
      </NAlert>
      <NAlert v-if="pathConflict" type="error" :bordered="false">
        ⚠ ComfyUI 根目录与 venv 路径不能相同
      </NAlert>
    </NSpace>

    <!-- F31：仓库地址管理对话框 -->
    <RepoUrlDialog v-model:show="showRepoDialog" />

    <!-- v1.8 / F36-Phase2：环境修复向导 -->
    <RepairWizard
      :show="showRepairWizard"
      @close="showRepairWizard = false"
      @repaired="
        async () => {
          showRepairWizard = false;
          await envStore.refresh();
          await envStore.checkReadiness();
        }
      "
    />
  </NCard>
</template>

<style scoped>
.paths-panel {
  margin-bottom: 16px;
}

.env-check-section {
  margin-bottom: 16px;
}

.env-alert {
  font-size: 13px;
}

.env-alert-content {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  flex-wrap: wrap;
}

.header-title {
  font-weight: 600;
}

.error-list {
  margin-top: 12px;
}

.label-with-help {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}

.help-icon {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 14px;
  height: 14px;
  border-radius: 50%;
  background: var(--app-text-muted, #999);
  color: #fff;
  font-size: 10px;
  font-weight: 600;
  cursor: help;
  user-select: none;
  transition: background 0.2s;
}

.help-icon:hover {
  background: var(--app-primary, #18a058);
}

.help-content {
  max-width: 360px;
  line-height: 1.6;
}

/* F31：仓库地址管理按钮（白底可见，遵循 UI 约束） */
.repo-url-btn {
  margin-left: 8px;
  padding: 0 8px;
  font-size: 12px;
  font-weight: 500;
}

/* v1.8 / F36-Phase2：深度诊断入口行 */
.diagnose-row {
  margin-top: 8px;
  flex-wrap: wrap;
}

.diagnose-hint {
  font-size: 12px;
  color: var(--app-text-muted, #999);
  line-height: 1.5;
}
</style>
