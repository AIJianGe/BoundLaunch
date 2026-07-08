<script setup lang="ts">
/**
 * 核心版本页（v3.1 / F26 重构 → v3.5 异步化 + 实时日志）
 *
 * 详见 `PR/06-界面设计.md §5.1 核心版本页`
 *
 * 区块：
 * 1. 当前版本大字显示（顶部居中）+ 检查更新按钮
 * 2. 状态告警条（运行中 / 工作区脏 / 依赖不匹配）
 * 3. NTab 双分类（stable / prerelease）+ NDataTable 版本列表
 * 4. 切换中进度条 + 实时日志面板（v3.5：useSwitchVersion 注入）
 * 5. 切换确认对话框（SwitchVersionDialog）
 *
 * 决策对应：
 * - 决策 7：NTab + NDataTable（信息密度高）
 * - 决策 8：详细描述操作（SwitchVersionDialog）
 * - 决策 5：前置条件检查（ComfyUI 必须停止 + 工作区干净）
 *
 * 状态机：
 * - loading：初次加载
 * - ready：可切换
 * - running：ComfyUI 运行中（禁用切换）
 * - switching：版本切换任务进行中（v3.5：实时日志 + 进度 + 取消）
 * - dirty：工作区有未提交改动（禁用切换）
 * - requirements_mismatch：切换后依赖需更新
 *
 * v3.5 关键改造：
 * - 用 `useSwitchVersion` composable 替代 v3.4 的 listen 模式
 * - 实时日志通过 `task_log` 事件累积（最大 500 行）
 * - 进度条 + 折叠日志面板（<details>）
 * - 取消按钮调 `useSwitchVersion.cancel()` → `task_cancel` 命令
 * - 不再有 timeout：用户主动取消才退出
 *
 * 设计模式：
 * - **State Machine**：UI 状态基于 coreStore + processStore 派生
 * - **Observer**：通过 useTaskProgress 订阅 task_progress / task_completed / task_log
 * - **Facade**：本页面整合 core + process + task store
 */

import { ref, computed, onMounted, onUnmounted, watch } from "vue";
import {
  NCard,
  NButton,
  NTag,
  NAlert,
  NSpace,
  NSpin,
  NEmpty,
  NTooltip,
  NTabs,
  NTabPane,
  NProgress,
  NBadge,
} from "naive-ui";
import { useCoreStore } from "@/stores/core";
import { useProcessStore } from "@/stores/process";
import { useEnvStore } from "@/stores/env";
import { useConfigStore } from "@/stores/config";
import { useToast } from "@/composables/useToast";
import { useSwitchVersion } from "@/composables/useSwitchVersion";
import {
  coreStatus,
  coreResetStaged,
  coreForceCleanWorkspace,
  coreOpenComfyuiDir,
  type WorkspaceDirtyReason,
  type SwitchMode,
} from "@/api/core";
import { listen, type UnlistenFn } from "@/api";
import type { TagInfo } from "@/api/types";
import VersionTable from "@/components/core/VersionTable.vue";
import SwitchVersionDialog from "@/components/core/SwitchVersionDialog.vue";
import RepairWizard from "@/components/settings/RepairWizard.vue";

const coreStore = useCoreStore();
const processStore = useProcessStore();
const envStore = useEnvStore();
const configStore = useConfigStore();
const toast = useToast();

// ========== v3.5：版本切换 composable（实时进度 + 实时日志 + 取消） ==========
const switcher = useSwitchVersion();

/** v3.5：实时日志面板展开状态 */
const showLogPanel = ref(false);
/** v3.5：自动滚动 DOM ref */
const logContainer = ref<HTMLElement | null>(null);

// ========== 本地状态 ==========
/** 当前选中的 tab（stable / prerelease） */
const activeTab = ref<"stable" | "prerelease">("stable");
/** 切换确认对话框显示状态 */
const dialogShow = ref(false);
/** 切换确认对话框中的目标 tag */
const targetTag = ref<TagInfo | null>(null);
/** 监听器清理函数 */
const unlisteners: UnlistenFn[] = [];
/** F35-A+：工作区脏的详细原因（null = 干净或未知） */
const dirtyReason = ref<WorkspaceDirtyReason | null>(null);
/** F35-A+：重置/强制清理中状态 */
const cleaning = ref(false);
/** v1.8 / F36-Phase2：环境修复向导显示状态 */
const showRepairWizard = ref(false);

// ========== 派生状态 ==========
const isCloned = computed(() => coreStore.isCloned);
const currentVersion = computed(() => coreStore.currentVersion);
const loading = computed(() => coreStore.loading);
const stableTags = computed(() => coreStore.stableTags);
const prereleaseTags = computed(() => coreStore.prereleaseTags);
const hasLocalChanges = computed(() => coreStore.hasLocalChanges);
const hasUpdates = computed(() => coreStore.hasUpdates);

/** v3.10：引导安装默认版本（Config.paths.installation_default_version） */
const defaultVersion = computed(
  () => configStore.config?.paths?.installation_default_version ?? null,
);

const isRunning = computed(() => processStore.isAlive);

/** v3.5：切换状态从 composable 派生（替代 v3.4 的 coreStore.isSwitching） */
const isSwitching = computed(() => switcher.isRunning.value);
const switchProgress = computed(() => switcher.progress.value);
const switchMessage = computed(() => switcher.message.value);
const switchLogs = computed(() => switcher.logs.value);
const requirementsMismatch = computed(() => coreStore.requirementsMismatch);

/** 切换按钮禁用状态（运行中 / 切换中 / 工作区脏） */
const switchDisabled = computed(
  () => isRunning.value || isSwitching.value || hasLocalChanges.value,
);

/** v1.8 / F36-Phase2：PyTorch 未安装（页面顶部告警条用） */
const torchBroken = computed(
  () => envStore.envInfo !== null && !envStore.envInfo.torch_installed,
);

/** 当前版本类型标签 */
const currentVersionType = computed(() => {
  if (!currentVersion.value) return null;
  const tag = [...stableTags.value, ...prereleaseTags.value].find(
    (t) => t.name === currentVersion.value,
  );
  if (!tag) return null;
  return tag.is_stable ? "stable" : "prerelease";
});

/** v3.5：进度阶段文本（用于进度条上方） */
const switchStepText = computed(() => {
  if (switcher.isCancelled.value) return "已取消";
  if (switcher.isCompleted.value) return "完成";
  if (switcher.isFailed.value) return "失败";
  return switchMessage.value || "切换版本中...";
});

/** v3.5：进度条状态（info / success / error / warning） */
const progressStatus = computed<"info" | "success" | "error" | "warning">(() => {
  if (switcher.isFailed.value) return "error";
  if (switcher.isCancelled.value) return "warning";
  if (switcher.isCompleted.value) return "success";
  return "info";
});

// ========== 生命周期 ==========
onMounted(async () => {
  // 订阅 core_version_switched 事件（v3.1 / F26）
  unlisteners.push(
    await listen<{ from: string | null; to: string }>(
      "core_version_switched",
      async (e) => {
        console.info(
          `[core] version switched: ${e.payload.from} → ${e.payload.to}`,
        );
        // v3.5：不在这里 clearSwitchingTask，由 useSwitchVersion.onComplete 处理
        // ✅ P0-4 修复：await refresh + 兜底，确保 currentVersion 同步更新
        // 之前 fire-and-forget + 不 await 导致切菜单后 UI 不刷新
        try {
          await coreStore.refresh();
          console.info("[core] refresh after switch ok, currentVersion =", coreStore.currentVersion);
        } catch (err) {
          console.warn("[core] refresh after switch failed:", err);
        }
      },
    ),
  );

  // 初次加载
  if (!coreStore.status) {
    try {
      await coreStore.refresh();
    } catch (e) {
      console.warn("core refresh:", e);
    }
  }
  // F35-A+：初次加载时拉取工作区脏原因（条件：has_local_changes=true）
  if (coreStore.status?.has_local_changes) {
    await refreshDirtyReason();
  }
});

onUnmounted(() => {
  unlisteners.forEach((un) => un());
  unlisteners.length = 0;
  // 清理 composable
  switcher.reset();
});

// v3.5：实时日志累积时自动滚动到底部
watch(
  () => switchLogs.value.length,
  () => {
    if (showLogPanel.value) {
      setTimeout(() => {
        if (logContainer.value) {
          logContainer.value.scrollTop = logContainer.value.scrollHeight;
        }
      }, 10);
    }
  },
);

// ========== 事件处理 ==========

/** 点击"检查更新" */
async function onCheckUpdates() {
  try {
    await coreStore.refreshTags(true);
    if (coreStore.hasUpdates) {
      toast.success(
        `检测到新版本可用: ${coreStore.status?.latest_stable}`,
      );
    } else {
      toast.info("已是最新版本");
    }
  } catch (e) {
    toast.error("检查更新失败", e);
  }
}

/** 点击"切换到此版本"（来自 VersionTable） */
function onSwitchClick(tag: TagInfo) {
  // 前置条件检查
  if (switchDisabled.value) return;
  targetTag.value = tag;
  dialogShow.value = true;
}

/** 确认切换（F36：带 mode 参数）— v3.5 走 useSwitchVersion 异步流程 */
async function onConfirmSwitch(mode: SwitchMode) {
  if (!targetTag.value) return;
  const tag = targetTag.value;
  dialogShow.value = false;
  // 自动展开日志面板（用户更关心实时进度）
  showLogPanel.value = true;

  // v3.5：调 composable 的 start 方法，传入完整回调
  // 注意：start 内部已经处理了 task_id 跟踪，UI 不用自己 listen
  await switcher.start(tag.name, mode, {
    onComplete: (summary) => {
      const msg = summary || "已切换到目标版本";
      toast.success(`版本切换完成: ${msg}`);
      // 刷新 store 状态
      coreStore.refresh().catch((e) =>
        console.warn("[core] refresh after switch complete failed:", e),
      );
      coreStore.refreshPrerequisites();
    },
    onError: (summary) => {
      const msg = summary || "未知错误";
      if (switcher.isCancelled.value) {
        toast.info("版本切换已取消");
      } else {
        toast.error("版本切换失败", msg);
        // 失败时后端会自动回滚，提示用户
        toast.warn("已回滚到原版本，请检查环境状态");
      }
      // 失败/取消后也刷新状态（确保 UI 与后端一致）
      coreStore.refresh().catch((e) =>
        console.warn("[core] refresh after switch error failed:", e),
      );
    },
  });
}

/** v3.5：取消当前切换 */
async function onCancelSwitching() {
  if (!isSwitching.value) return;
  try {
    await switcher.cancel();
    toast.info("正在取消版本切换...");
  } catch (e) {
    toast.error("取消失败", e);
  }
}

/** 取消切换对话框 */
function onCancelSwitch() {
  dialogShow.value = false;
  targetTag.value = null;
}

/** 点击"停止并切换"（运行中告警条） */
async function onStopAndSwitch() {
  try {
    await processStore.stop();
    toast.info("ComfyUI 已停止，可以切换版本");
  } catch (e) {
    toast.error("停止 ComfyUI 失败", e);
  }
}

/** 点击"克隆 ComfyUI 仓库" */
async function onClone() {
  try {
    await coreStore.clone();
    toast.success("ComfyUI 仓库克隆完成");
  } catch (e) {
    toast.error("克隆失败", e);
  }
}

/** 点击"立即安装"依赖（requirements 不匹配时） */
async function onInstallRequirements() {
  // TODO: 接入 PythonEnvManager.install_requirements
  toast.info("请前往「环境检查」页面重新初始化环境");
}

/** F35-D：打开 ComfyUI 仓库目录（工作区脏时引导用户手动 git stash / clean） */
async function onOpenComfyuiDir() {
  try {
    await coreOpenComfyuiDir();
    toast.success("已打开 ComfyUI 目录，请执行 git stash / git clean 后回到此页面点击「刷新状态」");
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    toast.error("打开目录失败", msg);
  }
}

/** F35-D：刷新工作区状态（用户清理后点击此按钮重新检测） */
async function onRefreshStatus() {
  try {
    await coreStore.refresh();
    // F35-A+：同时拉取详细原因
    await refreshDirtyReason();
    toast.success("状态已刷新");
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    toast.error("刷新失败", msg);
  }
}

/** F35-A+：刷新工作区脏的详细原因 */
async function refreshDirtyReason() {
  try {
    // 后端独立命令，比 CoreStatus 详细
    const { coreWorkspaceDirtyReason } = await import("@/api/core");
    dirtyReason.value = await coreWorkspaceDirtyReason();
  } catch (e) {
    console.warn("refreshDirtyReason:", e);
    dirtyReason.value = null;
  }
}

/** F35-A+：撤销 staging（`git reset HEAD`），保留 working tree 文件内容 */
async function onResetStaged() {
  if (cleaning.value) return;
  cleaning.value = true;
  try {
    await coreResetStaged();
    toast.success("已撤销 staging（文件内容保留为 unstaged 状态）");
    await onRefreshStatus();
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    toast.error("撤销 staging 失败", msg);
  } finally {
    cleaning.value = false;
  }
}

/** F35-A+：强制清理工作区（不可恢复） */
async function onForceClean() {
  if (cleaning.value) return;
  if (!confirm("⚠ 危险操作！\n\n将丢弃所有 tracked 改动和 untracked 文件，不可恢复。\n\n确定继续？")) {
    return;
  }
  cleaning.value = true;
  try {
    await coreForceCleanWorkspace();
    toast.success("已清理工作区");
    await onRefreshStatus();
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    toast.error("清理失败", msg);
  } finally {
    cleaning.value = false;
  }
}
</script>

<template>
  <div class="core-version-page">
    <!-- 未克隆状态 -->
    <NCard
      v-if="!isCloned && !loading"
      class="not-cloned"
      :bordered="true"
      size="small"
    >
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
    <NCard
      v-else-if="loading && !currentVersion"
      class="loading-card"
      :bordered="true"
      size="small"
    >
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
            <div class="version-text">
              {{ currentVersion || "未知" }}
            </div>
            <NTag
              v-if="currentVersionType === 'stable'"
              size="small"
              type="success"
            >
              stable
            </NTag>
            <NTag
              v-else-if="currentVersionType === 'prerelease'"
              size="small"
              type="warning"
            >
              prerelease
            </NTag>
            <NTag
              v-if="hasLocalChanges"
              size="small"
              type="error"
              class="dirty-tag"
            >
              工作区有改动
            </NTag>
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
        class="status-alert"
      >
        ⚠ ComfyUI 运行中，请先停止进程再切换版本。
        <NButton
          size="tiny"
          type="warning"
          :loading="processStore.isStarting || processStore.isStopping"
          @click="onStopAndSwitch"
        >
          停止并切换
        </NButton>
      </NAlert>

      <!-- v1.8 / F36-Phase2：PyTorch 未安装告警条（页面顶部） -->
      <NAlert
        v-if="torchBroken"
        type="error"
        :bordered="false"
        class="status-alert"
      >
        <template #header>⚠ PyTorch 不可用</template>
        检测到 PyTorch 未安装或无法 import。点击「诊断修复」扫描环境问题并自动修复，
        完成后即可正常启动 ComfyUI。
        <NSpace :size="8" style="margin-top: 8px">
          <NButton
            size="small"
            type="warning"
            :loading="envStore.repairing"
            @click="showRepairWizard = true"
          >
            诊断修复
          </NButton>
        </NSpace>
      </NAlert>

      <!-- 工作区脏状态提示（F35-A+：详细原因 + 一键清理） -->
      <NAlert
        v-if="!isRunning && hasLocalChanges"
        type="error"
        :bordered="false"
        class="status-alert"
      >
        <template #header>
          ⚠ 工作区有未提交改动，无法切换版本
          <span v-if="dirtyReason" class="reason-tag">
            （{{ dirtyReason.kind === "staged" ? "staged 改动" : dirtyReason.kind === "unstaged" ? "working tree 改动" : "untracked 文件" }}{{ dirtyReason.count }} 个）
          </span>
        </template>
        <NSpace vertical :size="8">
          <!-- F35-A+：根据原因给针对性提示 -->
          <template v-if="dirtyReason?.kind === 'staged'">
            <span>
              <strong>{{ dirtyReason.count }} 个文件</strong>已 add 但未 commit（staged 改动）。
              <code>git diff</code> 看不到这些，必须用 <code>git diff --cached</code>。
            </span>
            <code class="git-cmd">git diff --cached --stat &nbsp;&nbsp; # 查看 staged 内容</code>
            <code class="git-cmd">git reset HEAD &nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp; # 撤销 staging（保留文件内容）</code>
          </template>
          <template v-else-if="dirtyReason?.kind === 'unstaged'">
            <span><strong>{{ dirtyReason.count }} 个文件</strong>有 working tree 改动（未 add）。</span>
            <code class="git-cmd">git diff --stat &nbsp;&nbsp;&nbsp;&nbsp;&nbsp; # 查看 working tree 改动</code>
            <code class="git-cmd">git checkout . &nbsp;&nbsp;&nbsp;&nbsp; # 丢弃 tracked 改动（不可恢复）</code>
            <code class="git-cmd">git stash push -u &nbsp; # 暂存（含 untracked，可后续恢复）</code>
          </template>
          <template v-else-if="dirtyReason?.kind === 'untracked'">
            <span><strong>{{ dirtyReason.count }} 个</strong> untracked 文件/目录（git 不知道的新文件）。</span>
            <code class="git-cmd">git clean -fd &nbsp;&nbsp;&nbsp;&nbsp; # 清理 untracked（不删被忽略的）</code>
            <code class="git-cmd">git clean -fdx &nbsp;&nbsp;&nbsp;&nbsp; # 清理所有 untracked + 被忽略的</code>
          </template>
          <template v-else>
            <span>请执行以下任一操作后再切换：</span>
            <code class="git-cmd">git stash push -u &nbsp;&nbsp; # 暂存（含 untracked 文件，可后续恢复）</code>
            <code class="git-cmd">git clean -fdx &nbsp;&nbsp;&nbsp;&nbsp; # 清理所有 untracked（不可恢复）</code>
            <code class="git-cmd">git checkout . &nbsp;&nbsp;&nbsp;&nbsp;&nbsp; # 丢弃 tracked 改动</code>
          </template>

          <!-- F35-A+：文件列表（前 20 个） -->
          <details v-if="dirtyReason && dirtyReason.files.length > 0" class="file-list-details">
            <summary>📄 受影响文件（{{ dirtyReason.files.length }}）</summary>
            <div class="file-list">
              <div v-for="f in dirtyReason.files" :key="f" class="file-item">{{ f }}</div>
              <div v-if="dirtyReason.count > dirtyReason.files.length" class="file-item more">
                …还有 {{ dirtyReason.count - dirtyReason.files.length }} 个
              </div>
            </div>
          </details>

          <NSpace :size="8">
            <!-- F35-A+：一键撤销 staging（仅 staged 时显示） -->
            <NButton
              v-if="dirtyReason?.kind === 'staged'"
              size="small"
              type="primary"
              :loading="cleaning"
              @click="onResetStaged"
            >
              ↩ 撤销 Staged（保留文件）
            </NButton>
            <!-- F35-A+：强制清理（所有情况都可点） -->
            <NButton
              size="small"
              type="warning"
              :loading="cleaning"
              @click="onForceClean"
            >
              🗑 强制清理（不可恢复）
            </NButton>
            <NButton size="small" @click="onOpenComfyuiDir">
              📁 打开 ComfyUI 目录
            </NButton>
            <NButton size="small" @click="onRefreshStatus">
              🔄 刷新状态
            </NButton>
          </NSpace>
        </NSpace>
      </NAlert>

      <!-- 切换后依赖需更新提示 -->
      <NAlert
        v-if="requirementsMismatch"
        type="warning"
        :bordered="false"
        class="status-alert"
      >
        ⚠ 检测到依赖需更新（切换版本后）
        <NButton size="tiny" type="warning" @click="onInstallRequirements">
          立即安装
        </NButton>
      </NAlert>

      <!-- v3.5：切换中进度条 + 实时日志面板（useSwitchVersion 驱动） -->
      <NCard v-if="isSwitching || switcher.isCompleted.value || switcher.isFailed.value" class="switch-progress" :bordered="true" size="small">
        <div class="progress-content">
          <div class="progress-header">
            <span class="progress-title">
              <span v-if="switcher.isCompleted.value">✅ 切换完成</span>
              <span v-else-if="switcher.isCancelled.value">⚠ 已取消</span>
              <span v-else-if="switcher.isFailed.value">❌ 切换失败</span>
              <span v-else>🔄 切换版本中...</span>
              <span class="progress-stage">{{ switchStepText }}</span>
            </span>
            <span class="progress-percent">{{ switchProgress }}%</span>
          </div>
          <NProgress
            type="line"
            :percentage="switchProgress"
            :show-indicator="false"
            :height="8"
            :status="progressStatus"
          />
          <div class="progress-meta">
            <span class="progress-hint">
              <template v-if="switcher.isCompleted.value">
                版本切换成功，可继续操作。
              </template>
              <template v-else-if="switcher.isCancelled.value">
                切换已取消，环境已回滚（或正在回滚）。
              </template>
              <template v-else-if="switcher.isFailed.value">
                切换失败：{{ switcher.errorSummary.value || "未知错误" }}
              </template>
              <template v-else>
                切换过程中请勿关闭应用或停止 ComfyUI。点击「取消」可立即中止。
              </template>
            </span>
            <NButton
              v-if="isSwitching"
              size="tiny"
              type="error"
              :ghost="true"
              @click="onCancelSwitching"
            >
              ✕ 取消
            </NButton>
          </div>

          <!-- 实时日志面板（v3.5：可折叠 + 自动滚动） -->
          <details
            class="log-details"
            :open="showLogPanel"
            @toggle="(e: any) => (showLogPanel = e.target.open)"
          >
            <summary class="log-summary">
              📋 实时日志（{{ switchLogs.length }} 行）
              <span class="log-hint">点击展开 / 折叠</span>
            </summary>
            <div class="log-panel" ref="logContainer">
              <div
                v-for="(line, idx) in switchLogs"
                :key="idx"
                class="log-line"
              >
                <span class="log-source">[{{ line.source }}]</span>
                <span class="log-text">{{ line.text }}</span>
              </div>
              <div v-if="switchLogs.length === 0" class="log-empty">
                等待日志输出...
              </div>
            </div>
          </details>
        </div>
      </NCard>

      <!-- 版本列表（NTab + NDataTable） -->
      <NCard class="version-list" :bordered="true" size="small">
        <template #header>
          <span class="header-title">选择目标版本</span>
        </template>
        <template #header-extra>
          <NTooltip placement="top">
            <template #trigger>
              <span class="info-tip">ℹ 切换说明</span>
            </template>
            <div class="tooltip-content">
              切换版本会完全重建 venv（决策 3），custom_nodes 不动（决策 4），<br />
              失败将全部回滚（决策 6）。详见确认对话框。
            </div>
          </NTooltip>
        </template>

        <NTabs v-model:value="activeTab" type="line" animated>
          <NTabPane name="stable" tab="稳定版">
            <template #tab>
              <NBadge
                :value="stableTags.length"
                :max="999"
                type="success"
                :offset="[6, -2]"
              >
                稳定版
              </NBadge>
            </template>
            <VersionTable
              :tags="stableTags"
              :current-version="currentVersion"
              :loading="loading"
              :disabled="switchDisabled"
              :default-version="defaultVersion"
              @switch="onSwitchClick"
            />
          </NTabPane>
          <NTabPane name="prerelease" tab="预发布版">
            <template #tab>
              <NBadge
                :value="prereleaseTags.length"
                :max="999"
                type="warning"
                :offset="[6, -2]"
              >
                预发布版
              </NBadge>
            </template>
            <VersionTable
              :tags="prereleaseTags"
              :current-version="currentVersion"
              :loading="loading"
              :disabled="switchDisabled"
              :default-version="defaultVersion"
              @switch="onSwitchClick"
            />
          </NTabPane>
        </NTabs>
      </NCard>

      <!-- 切换后插件兼容性提示 -->
      <NAlert
        v-if="currentVersion && !requirementsMismatch && !isSwitching && !switcher.isCompleted.value"
        type="info"
        :bordered="false"
        class="status-alert"
      >
        ℹ 版本切换后请观察插件是否正常工作；如插件报错可在「插件管理页」禁用对应插件。
      </NAlert>
    </template>

    <!-- 切换确认对话框 -->
    <SwitchVersionDialog
      :show="dialogShow"
      :target-tag="targetTag"
      :current-version="currentVersion"
      :switching="isSwitching"
      @confirm="onConfirmSwitch"
      @cancel="onCancelSwitch"
    />

    <!-- v1.8 / F36-Phase2：环境修复向导 -->
    <RepairWizard
      :show="showRepairWizard"
      @close="showRepairWizard = false"
      @repaired="
        async () => {
          showRepairWizard = false;
          await envStore.refresh();
          toast.success('环境已修复');
        }
      "
    />
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
.version-list,
.switch-progress {
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
  flex-wrap: wrap;
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

.dirty-tag {
  flex-shrink: 0;
}

.update-hint {
  margin-top: 8px;
  font-size: 13px;
  color: var(--app-primary, #18a058);
}

.status-alert {
  margin-bottom: 16px;
}

/* F35-A+：工作区脏原因标签 + 文件列表样式 */
.reason-tag {
  display: inline-block;
  margin-left: 8px;
  padding: 2px 8px;
  background-color: rgba(208, 48, 80, 0.1);
  color: #d03050;
  border-radius: 4px;
  font-size: 12px;
  font-weight: normal;
}

.file-list-details {
  margin: 4px 0;
  font-size: 13px;
}

.file-list-details summary {
  cursor: pointer;
  color: #666;
  user-select: none;
}

.file-list-details summary:hover {
  color: #333;
}

.file-list {
  margin-top: 6px;
  padding: 8px 12px;
  background-color: rgba(0, 0, 0, 0.03);
  border-radius: 4px;
  max-height: 200px;
  overflow-y: auto;
  font-family: "JetBrains Mono", "Consolas", monospace;
  font-size: 12px;
}

.file-item {
  padding: 2px 0;
  color: #555;
}

.file-item.more {
  color: #999;
  font-style: italic;
}

.git-cmd {
  display: block;
  padding: 6px 10px;
  background-color: rgba(0, 0, 0, 0.04);
  border-radius: 4px;
  font-family: "JetBrains Mono", "Consolas", "Courier New", monospace;
  font-size: 12px;
  color: #d35400;
  user-select: all;
}

.header-title {
  font-weight: 600;
}

.info-tip {
  font-size: 12px;
  color: var(--app-text-muted, #999);
  cursor: help;
  padding: 2px 8px;
  border-radius: 4px;
  background: var(--app-bg-soft, rgba(127, 127, 127, 0.08));
}

.tooltip-content {
  line-height: 1.6;
  font-size: 12px;
}

/* v3.5：切换进度面板（带实时日志） */
.switch-progress .progress-content {
  padding: 4px 0;
}

.progress-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: 8px;
  gap: 12px;
}

.progress-title {
  font-size: 13px;
  font-weight: 600;
  color: var(--app-text, #333);
  display: flex;
  align-items: center;
  gap: 8px;
  flex: 1;
  min-width: 0;
}

.progress-stage {
  font-size: 12px;
  font-weight: 400;
  color: var(--app-text-muted, #666);
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  flex: 1;
  min-width: 0;
}

.progress-percent {
  font-size: 13px;
  font-weight: 600;
  color: var(--app-primary, #18a058);
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
  flex-shrink: 0;
}

.progress-meta {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-top: 8px;
  gap: 12px;
}

.progress-hint {
  font-size: 12px;
  color: var(--app-text-muted, #999);
  line-height: 1.5;
  flex: 1;
}

/* v3.5：实时日志面板（折叠） */
.log-details {
  margin-top: 12px;
  border: 1px solid var(--app-border, #e0e0e0);
  border-radius: 6px;
  background: var(--app-bg-soft, #fafbfc);
}

.log-summary {
  cursor: pointer;
  padding: 8px 12px;
  font-size: 12px;
  font-weight: 600;
  color: var(--app-text, #333);
  user-select: none;
  list-style: none;
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.log-summary::-webkit-details-marker {
  display: none;
}

.log-summary::before {
  content: "▶";
  display: inline-block;
  margin-right: 6px;
  font-size: 10px;
  color: var(--app-text-muted, #999);
  transition: transform 0.2s;
}

.log-details[open] .log-summary::before {
  transform: rotate(90deg);
}

.log-hint {
  font-size: 11px;
  font-weight: 400;
  color: var(--app-text-muted, #999);
}

.log-panel {
  margin: 0 8px 8px 8px;
  max-height: 300px;
  overflow-y: auto;
  background: #1e1e1e;
  border-radius: 4px;
  padding: 8px;
  font-family: "JetBrains Mono", "Consolas", "Cascadia Code", monospace;
  font-size: 11px;
  color: #d4d4d4;
  scroll-behavior: smooth;
}

.log-line {
  display: flex;
  gap: 8px;
  padding: 1px 0;
  word-break: break-all;
  line-height: 1.5;
}

.log-source {
  color: #569cd6;
  flex-shrink: 0;
  font-weight: 600;
  min-width: 90px;
}

.log-text {
  flex: 1;
  color: #d4d4d4;
  white-space: pre-wrap;
}

.log-empty {
  color: #6a6a6a;
  font-style: italic;
  padding: 8px 0;
  text-align: center;
}
</style>
