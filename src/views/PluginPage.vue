<script setup lang="ts">
/**
 * 插件管理页
 *
 * 详见 `PR/06-界面设计.md §5.2 插件管理页`
 *
 * 区块：
 * 1. 顶部工具栏：Git URL 输入 + [获取版本] + [安装] + 搜索框 + [批量检查更新]
 * 2. 实时进度面板（仅 install/update/装依赖时显示）：阶段 + 进度条 + 折叠日志
 * 3. 统计栏：总数 / 启用 / 禁用 / 待更新
 * 4. 插件列表表格：插件名 | commit | 启用开关 | [更新] [装依赖] [卸载]
 * 5. 空状态：提示输入 Git URL
 * 6. 操作确认弹窗
 *
 * Tag 选择流程：
 * 1. 用户输入 URL → 点"获取版本"
 * 2. 调 pluginListRemoteTags(url) 获取 tag 列表
 * 3. 有 tag → NSelect 下拉选择（默认"最新 commit（默认分支）"）
 * 4. 无 tag → 提示"无 tag，直接安装最新版"
 * 5. 点"安装" → pluginInstall(url, selectedTag)
 *
 * 实时进度流程（v3.x）：
 * 1. install() 调 pluginStore.install() → store 立即置 active=true
 * 2. 后端流式 emit plugin_progress (cloning/installing_requirements/...)
 *    + plugin_progress_log (每行 pip 输出)
 * 3. store 订阅事件，更新 installProgress / installLogs
 * 4. 进度面板自动重渲染：进度条 + 阶段 + 日志面板（折叠）
 * 5. 后端 emit Done/Failed → 1.5s/3s 后自动隐藏
 */

import { h, ref, computed, onMounted, watch } from "vue";
import {
  NCard,
  NInput,
  NButton,
  NEmpty,
  NTag,
  NSwitch,
  NSpace,
  NDataTable,
  NPopconfirm,
  NSpin,
  NSelect,
  NProgress,
  NCollapseTransition,
  NLog,
  NModal,
  NAlert,
  type DataTableColumns,
} from "naive-ui";
import { usePluginStore } from "@/stores/plugin";
import { useProcessStore } from "@/stores/process";
import { useToast } from "@/composables/useToast";
import { pluginListRemoteTags } from "@/api/plugin";
import type { LocalRefInfo, PluginInfo, RemoteTagInfo } from "@/api/types";

const pluginStore = usePluginStore();
const processStore = useProcessStore();
const toast = useToast();

const gitUrlInput = ref("");
const searchQuery = ref("");
const installing = ref(false);
const fetchingTags = ref(false);

// Tag 选择相关
const remoteTags = ref<RemoteTagInfo[]>([]);
const selectedTag = ref<string | null>(null);

// v3.x：切版本弹窗
const showSwitchModal = ref(false);
const switchTarget = ref<PluginInfo | null>(null);
const availableVersions = ref<LocalRefInfo[]>([]);
const switchTargetRef = ref<string | null>(null);
const loadingVersions = ref(false);
const switching = ref(false);
const rolling = ref(false);

// 日志面板折叠状态（默认折叠，节省屏幕）
const logsExpanded = ref(false);
const showLogs = computed(() => logsExpanded.value);

// v3.x：venv 健康弹窗
const showVenvHealthModal = ref(false);

// v3.x：ComfyUI 核心依赖状态弹窗
const showComfyuiCoreModal = ref(false);

// v3.x：venv 健康状态可视化
const venvStatusAlertType = computed(() => {
  const s = pluginStore.venvHealth?.status;
  if (!s) return "info";
  switch (s) {
    case "healthy":
      return "success";
    case "has_broken_distributions":
      return "warning";
    case "import_failed":
    case "broken_and_import_failed":
      return "error";
    case "venv_not_found":
    case "site_packages_not_found":
      return "warning";
    default:
      return "info";
  }
});

const venvStatusTitle = computed(() => {
  const s = pluginStore.venvHealth?.status;
  if (!s) return "未检测";
  switch (s) {
    case "healthy":
      return "✓ venv 健康";
    case "has_broken_distributions":
      return "⚠ 有损坏包（import 仍可用）";
    case "import_failed":
      return "✗ 关键 import 失败（严重）";
    case "broken_and_import_failed":
      return "✗ 损坏包 + import 失败（最严重）";
    case "venv_not_found":
      return "⚠ venv 目录不存在";
    case "site_packages_not_found":
      return "⚠ site-packages 目录不存在";
    default:
      return s;
  }
});

const venvStatusMessage = computed(() => {
  const r = pluginStore.venvHealth;
  if (!r) return "";
  const brokenCount = r.broken_distributions.length;
  const failedImports = r.critical_imports.filter((i) => !i.ok);
  if (r.status === "healthy") {
    return `所有 ${r.critical_imports.length} 个关键模块均可正常导入。`;
  }
  if (r.status === "has_broken_distributions") {
    return `检测到 ${brokenCount} 个以 ~ 开头的损坏包目录。建议点"一键清理损坏包"。`;
  }
  if (r.status === "import_failed") {
    return `以下模块无法导入（可能是依赖未装或 venv 损坏）：${failedImports
      .map((i) => i.module)
      .join(", ")}`;
  }
  if (r.status === "broken_and_import_failed") {
    return `先点"一键清理损坏包"清理后，再手动重装失败模块。失败：${failedImports
      .map((i) => i.module)
      .join(", ")}`;
  }
  if (r.status === "venv_not_found") {
    return "请先初始化 venv。";
  }
  if (r.status === "site_packages_not_found") {
    return "venv 结构不完整，请重新初始化。";
  }
  return "";
});

/** 字节数格式化 */
function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(2)} MB`;
}

// 当前 stage 中文显示
const stageLabel = computed(() => {
  switch (pluginStore.installProgress.stage) {
    case "cloning":
      return "克隆";
    case "installing-deps":
      return "装依赖";
    case "pulling":
      return "拉取";
    case "switching":
      return "切版本";
    default:
      return "";
  }
});

// v3.x：切版本弹窗 - 选项构建（current 在前 → tag 倒序 → branch）
const versionOptions = computed(() => {
  return availableVersions.value.map((v: LocalRefInfo) => {
    const label = `${v.kind === "tag" ? "🏷 " : "🌿 "}${v.name}${
      v.is_current ? " （当前）" : ""
    } · ${v.commit.slice(0, 7)}`;
    return { label, value: v.name };
  });
});

const switchWarning = computed(() => {
  if (!switchTarget.value) return "";
  const target = switchTarget.value;
  if (target.is_detached) {
    return "当前插件处于 detached HEAD 状态，切版本前会自动备份当前 commit，可点「回滚」恢复。";
  }
  return "切版本会自动备份当前 commit 并重装依赖。ComfyUI 运行时切换需要重启。";
});

const comfyuiRunning = computed(() => processStore.isRunning);

// 日志内容（多行字符串给 NLog）
const logText = computed(() =>
  pluginStore.installLogs
    .map((l) => `[${l.level.toUpperCase()}] ${l.message}`)
    .join("\n"),
);

/** tag 下拉选项：第一项是"最新 commit（默认分支）"，后面是所有 tag */
const tagOptions = computed(() => {
  const opts = [
    { label: "最新 commit（默认分支）", value: "__latest__" },
  ];
  for (const t of remoteTags.value) {
    opts.push({ label: t.name, value: t.name });
  }
  return opts;
});

const filteredPlugins = computed(() => {
  if (!searchQuery.value.trim()) return pluginStore.plugins;
  const q = searchQuery.value.toLowerCase();
  return pluginStore.plugins.filter((p) => p.name.toLowerCase().includes(q));
});

const columns = computed<DataTableColumns<PluginInfo>>(() => [
  {
    title: "插件名",
    key: "name",
    sorter: (a, b) => a.name.localeCompare(b.name),
  },
  {
    title: "Commit",
    key: "current_commit",
    render: (row) => (row.current_commit ? row.current_commit.slice(0, 7) : "-"),
    width: 100,
  },
  {
    title: "分支",
    key: "current_branch",
    render: (row) => row.current_branch || "-",
    width: 100,
  },
  {
    title: "状态",
    key: "enabled",
    width: 80,
    render: (row) =>
      h(
        NTag,
        { size: "small", type: row.enabled ? "success" : "default" },
        { default: () => (row.enabled ? "启用" : "禁用") },
      ),
  },
  {
    title: "更新",
    key: "has_updates",
    width: 80,
    render: (row) => {
      if (row.has_updates === null) return null;
      return row.has_updates
        ? h(NTag, { size: "small", type: "warning" }, { default: () => "有更新" })
        : h(NTag, { size: "small", type: "info" }, { default: () => "最新" });
    },
  },
  {
    title: "操作",
    key: "actions",
    width: 420,
    render: (row) =>
      h(NSpace, { size: "small" }, {
        default: () => [
          h(NSwitch, {
            size: "small",
            value: row.enabled,
            "onUpdate:value": (v: boolean) => onToggle(row.name, v),
          }),
          h(
            NButton,
            {
              size: "tiny",
              type: "info",
              ghost: true,
              onClick: () => onSwitchVersion(row),
            },
            { default: () => "切版本" },
          ),
          h(
            NButton,
            {
              size: "tiny",
              disabled: row.has_updates !== true,
              onClick: () => onUpdate(row.name),
            },
            { default: () => "更新" },
          ),
          // 依赖安装按钮：有 requirements.txt 且未装时显示
          !row.requirements_installed
            ? h(
                NButton,
                {
                  size: "tiny",
                  type: "warning",
                  ghost: true,
                  onClick: () => onInstallRequirements(row.name),
                },
                { default: () => "装依赖" },
              )
            : null,
          h(
            NPopconfirm,
            {
              onPositiveClick: () => onUninstall(row.name),
              positiveText: "确认",
              negativeText: "取消",
            },
            {
              trigger: () =>
                h(
                  NButton,
                  { size: "tiny", type: "error", ghost: true },
                  { default: () => "卸载" },
                ),
              default: () => `确认卸载 ${row.name}？可从 .trash 恢复`,
            },
          ),
        ],
      }),
  },
]);

onMounted(async () => {
  try {
    await pluginStore.refresh();
  } catch (e) {
    console.warn("plugin refresh:", e);
  }
  // v3.x：进页面静默跑一次 venv 健康检查
  // 失败时降级（不阻塞 UI）
  try {
    await pluginStore.healthCheckVenv();
  } catch (e) {
    console.warn("venv health check:", e);
  }
});

// v3.x：监听 venv 关键 import 失败事件（来自 install_requirements 后验证）
// 弹一个长时间的 warn toast，让用户知道 venv 异常并提供修复入口
watch(
  () => pluginStore.venvImportWarningMessage,
  (msg) => {
    if (!msg) return;
    // naive-ui toast.warn 签名：toast.warn(message, options)
    // 把标题合并到 message 里
    toast.warn(`venv 异常 - ${msg}`, {
      duration: 8000,
      closable: true,
    });
    pluginStore.clearVenvImportWarning();
  },
);

/** 获取远程仓库的 tag 列表 */
async function onFetchTags() {
  const url = gitUrlInput.value.trim();
  if (!url) {
    toast.error("请输入 Git URL");
    return;
  }
  if (!url.startsWith("https://")) {
    toast.error("仅支持 https:// GitHub 仓库");
    return;
  }

  fetchingTags.value = true;
  remoteTags.value = [];
  selectedTag.value = null;
  try {
    const tags = await pluginListRemoteTags(url);
    if (tags.length > 0) {
      remoteTags.value = tags;
      selectedTag.value = "__latest__";
      toast.success(`找到 ${tags.length} 个 tag，请选择版本`);
    } else {
      toast.info("该仓库无 tag，将直接安装最新 commit");
      selectedTag.value = "__latest__";
    }
  } catch (e) {
    toast.error("获取 tag 失败", e);
  } finally {
    fetchingTags.value = false;
  }
}

async function onInstall() {
  const url = gitUrlInput.value.trim();
  if (!url) {
    toast.error("请输入 Git URL");
    return;
  }
  if (!url.startsWith("https://")) {
    toast.error("仅支持 https:// GitHub 仓库");
    return;
  }

  // __latest__ 表示不指定 tag（用默认分支 HEAD）
  const tag = selectedTag.value && selectedTag.value !== "__latest__"
    ? selectedTag.value
    : null;

  installing.value = true;
  // 默认展开日志（用户在等待反馈）
  logsExpanded.value = true;
  try {
    await pluginStore.install(url, tag);
    toast.success("插件安装完成");
    gitUrlInput.value = "";
    remoteTags.value = [];
    selectedTag.value = null;
  } catch (e) {
    toast.error("安装失败", e);
  } finally {
    installing.value = false;
  }
}

async function onToggle(name: string, enabled: boolean) {
  try {
    await pluginStore.toggle(name, enabled);
    toast.success(`${enabled ? "启用" : "禁用"} ${name}`);
  } catch (e) {
    toast.error("切换失败", e);
  }
}

async function onUpdate(name: string) {
  logsExpanded.value = true;
  try {
    await pluginStore.update(name);
    toast.success(`${name} 更新完成`);
  } catch (e) {
    toast.error("更新失败", e);
  }
}

async function onUninstall(name: string) {
  try {
    await pluginStore.uninstall(name);
    toast.success(`${name} 已卸载（可从 .trash 恢复）`);
  } catch (e) {
    toast.error("卸载失败", e);
  }
}

async function onInstallRequirements(name: string) {
  logsExpanded.value = true;
  try {
    await pluginStore.installRequirements(name, false);
    toast.success(`${name} 依赖安装完成`);
    // 刷新列表以更新 requirements_installed 字段
    await pluginStore.refresh(true);
  } catch (e) {
    toast.error("依赖安装失败", e);
  }
}

// v3.x：打开切版本弹窗
async function onSwitchVersion(plugin: PluginInfo) {
  switchTarget.value = plugin;
  showSwitchModal.value = true;
  switchTargetRef.value = plugin.current_ref || null;
  availableVersions.value = [];
  loadingVersions.value = true;
  try {
    availableVersions.value = await pluginStore.listAvailableVersions(plugin.name);
  } catch (e) {
    toast.error("获取版本列表失败", e);
  } finally {
    loadingVersions.value = false;
  }
}

// v3.x：确认切版本
async function onConfirmSwitch() {
  if (!switchTarget.value || !switchTargetRef.value) {
    toast.error("请选择目标版本");
    return;
  }
  // 当前 ref 不允许再选（disable 即可），但兼容 race condition
  if (switchTargetRef.value === switchTarget.value.current_ref) {
    toast.info("目标版本与当前相同");
    return;
  }

  switching.value = true;
  logsExpanded.value = true;
  try {
    const result = await pluginStore.switchVersion(
      switchTarget.value.name,
      switchTargetRef.value,
    );
    showSwitchModal.value = false;
    if (result.need_restart) {
      toast.warn(
        `${switchTarget.value.name} 已切到 ${switchTargetRef.value}，ComfyUI 需重启才能生效`,
      );
    } else {
      toast.success(
        `${switchTarget.value.name} 已切到 ${switchTargetRef.value}`,
      );
    }
  } catch (e) {
    toast.error("切版本失败", e);
  } finally {
    switching.value = false;
  }
}

// v3.x：回滚
async function onRollback(plugin: PluginInfo) {
  rolling.value = true;
  logsExpanded.value = true;
  try {
    await pluginStore.rollbackVersion(plugin.name);
    toast.success(`${plugin.name} 已回滚到上次切版本前的版本`);
  } catch (e) {
    toast.error("回滚失败", e);
  } finally {
    rolling.value = false;
  }
}

async function onCheckAllUpdates() {
  try {
    await pluginStore.checkUpdates();
    const count = pluginStore.plugins.filter((p) => p.has_updates === true).length;
    if (count > 0) {
      toast.success(`检测到 ${count} 个插件有更新`);
    } else {
      toast.info("所有插件均为最新");
    }
  } catch (e) {
    toast.error("检查更新失败", e);
  }
}

// v3.x：venv 健康检查按钮 handler
async function onCheckVenvHealth() {
  try {
    const report = await pluginStore.healthCheckVenv();
    showVenvHealthModal.value = true;
    console.info("[venv health] status=", report.status, "broken=", report.broken_distributions.length, "imports=", report.critical_imports.length);
  } catch (e) {
    toast.error("venv 健康检查失败", e);
  }
}

// v3.x：修复 venv 按钮 handler
async function onFixVenv() {
  try {
    const report = await pluginStore.fixVenv();
    if (report.status === "healthy") {
      toast.success("venv 已修复");
    } else if (report.status === "has_broken_distributions") {
      toast.warn(`已清理损坏包，但还有 ${report.broken_distributions.length} 个未处理`);
    } else if (report.status === "import_failed" || report.status === "broken_and_import_failed") {
      toast.error(
        "venv 仍有 import 失败",
        `失败模块：${report.critical_imports
          .filter((i) => !i.ok)
          .map((i) => i.module)
          .join(", ")}`,
      );
    } else {
      toast.warn(`venv 状态: ${report.status}`);
    }
  } catch (e) {
    toast.error("修复 venv 失败", e);
  }
}

// v3.x：ComfyUI 核心依赖检查 handler
async function onCheckComfyuiCore() {
  try {
    const status = await pluginStore.checkComfyuiCore(false);
    if (status.needs_install) {
      // 弹窗确认装
      showComfyuiCoreModal.value = true;
    } else {
      toast.info(`✓ ComfyUI 核心依赖已是最新（${status.reason}）`);
    }
  } catch (e) {
    toast.error("ComfyUI 核心依赖检查失败", e);
  }
}

// v3.x：装 ComfyUI 核心依赖 handler
async function onInstallComfyuiCore(force = false) {
  try {
    toast.info(force ? "正在强制重装核心依赖..." : "正在装核心依赖...");
    const hash = await pluginStore.installComfyuiCore(force);
    toast.success(`✓ ComfyUI 核心依赖装成功 (hash=${hash.slice(0, 8)}...)`);
    showComfyuiCoreModal.value = false;
  } catch (e) {
    toast.error("ComfyUI 核心依赖装失败", e);
  }
}

function toggleLogs() {
  logsExpanded.value = !logsExpanded.value;
}
</script>

<template>
  <div class="plugin-page">
    <!-- 顶部工具栏 -->
    <NCard class="toolbar" :bordered="true" size="small">
      <div class="toolbar-row">
        <NInput
          v-model:value="gitUrlInput"
          placeholder="https://github.com/user/comfyui-plugin"
          :disabled="installing"
          class="url-input"
          @keyup.enter="onFetchTags"
        />
        <NButton
          :loading="fetchingTags"
          :disabled="fetchingTags || installing || !gitUrlInput.trim()"
          @click="onFetchTags"
        >
          获取版本
        </NButton>
        <NButton
          type="primary"
          :loading="installing"
          :disabled="installing || !gitUrlInput.trim()"
          @click="onInstall"
        >
          安装
        </NButton>
      </div>

      <!-- Tag 选择行（获取版本后显示） -->
      <div v-if="remoteTags.length > 0 || selectedTag === '__latest__'" class="toolbar-row">
        <NSelect
          v-model:value="selectedTag"
          :options="tagOptions"
          :disabled="installing"
          size="small"
          class="tag-select"
          placeholder="选择版本"
        />
        <span class="hint">{{ remoteTags.length }} 个 tag 可选</span>
      </div>

      <div class="toolbar-row">
        <NInput
          v-model:value="searchQuery"
          placeholder="搜索插件名..."
          clearable
          class="search-input"
        />
        <NButton
          size="small"
          :loading="pluginStore.loading"
          @click="pluginStore.refresh()"
        >
          刷新列表
        </NButton>
        <NButton
          size="small"
          type="warning"
          :disabled="pluginStore.totalCount === 0"
          @click="onCheckAllUpdates"
        >
          批量检查更新
        </NButton>
        <!-- v3.x：venv 健康检查按钮（检测 site-packages 损坏包 + 关键 import 验证） -->
        <NButton
          size="small"
          :type="pluginStore.venvHasIssue ? 'error' : 'default'"
          :loading="pluginStore.venvHealthLoading"
          @click="onCheckVenvHealth"
        >
          {{ pluginStore.venvHasIssue ? '⚠ 修复 venv' : '检查 venv' }}
        </NButton>
        <!-- v3.x：ComfyUI 核心依赖管理按钮 -->
        <NButton
          size="small"
          :type="pluginStore.comfyuiCoreNeedsInstall ? 'warning' : 'default'"
          :loading="pluginStore.comfyuiCoreLoading"
          @click="onCheckComfyuiCore"
        >
          {{ pluginStore.comfyuiCoreNeedsInstall ? '⚠ 装核心依赖' : '核心依赖' }}
        </NButton>
      </div>
    </NCard>

    <!-- v3.x：实时进度面板（仅在 install/update/装依赖进行中显示） -->
    <NCard
      v-if="pluginStore.installProgress.active"
      class="progress-panel"
      :bordered="true"
      size="small"
      :class="{ 'is-failed': pluginStore.installProgress.failed }"
    >
      <div class="progress-header">
        <div class="progress-info">
          <NTag
            size="small"
            :type="pluginStore.installProgress.failed ? 'error' : 'info'"
          >
            {{ stageLabel || "处理中" }}
          </NTag>
          <strong>{{ pluginStore.installProgress.plugin }}</strong>
          <span class="hint">{{ pluginStore.installProgress.message }}</span>
        </div>
        <span class="progress-percent">{{ pluginStore.installProgress.percent }}%</span>
      </div>

      <NProgress
        type="line"
        :percentage="pluginStore.installProgress.percent"
        :indicator-placement="'inside'"
        :status="
          pluginStore.installProgress.failed
            ? 'error'
            : pluginStore.installProgress.percent >= 100
            ? 'success'
            : 'default'
        "
        :show-indicator="true"
        :height="14"
      />

      <div class="progress-footer">
        <NButton size="tiny" @click="toggleLogs">
          {{ showLogs ? "隐藏日志" : `查看日志 (${pluginStore.installLogs.length})` }}
        </NButton>
        <NButton
          v-if="!pluginStore.installProgress.failed"
          size="tiny"
          @click="pluginStore.clearLogs"
        >
          清空日志
        </NButton>
      </div>

      <NCollapseTransition :show="showLogs">
        <NLog
          v-if="pluginStore.installLogs.length > 0"
          :log="logText"
          :rows="12"
          :font-size="11"
          class="install-logs"
        />
        <div v-else class="logs-empty">
          <NSpin size="small" />
          <span class="hint">等待后端输出...</span>
        </div>
      </NCollapseTransition>
    </NCard>

    <!-- 统计栏 -->
    <NCard class="stats" :bordered="true" size="small">
      <NSpace size="large">
        <span>共 <strong>{{ pluginStore.totalCount }}</strong> 个插件</span>
        <NTag size="small" type="success">启用 {{ pluginStore.enabledCount }}</NTag>
        <NTag size="small" type="default">禁用 {{ pluginStore.disabledCount }}</NTag>
        <NTag v-if="pluginStore.hasUpdates" size="small" type="warning">有更新可用</NTag>
      </NSpace>
    </NCard>

    <!-- 列表 / 空状态 -->
    <NCard :bordered="true" size="small">
      <div v-if="pluginStore.loading && pluginStore.totalCount === 0" class="loading">
        <NSpin size="medium" />
        <span class="hint">加载插件列表...</span>
      </div>

      <NEmpty
        v-else-if="pluginStore.totalCount === 0"
        description="暂无插件"
        size="medium"
      >
        <template #extra>
          <NSpace vertical align="center" :size="8">
            <span class="hint">输入上方 Git URL 安装第一个插件</span>
            <span class="hint">推荐：was-node-suite / comfyui-impact-pack / rgthree-comfy</span>
          </NSpace>
        </template>
      </NEmpty>

      <NDataTable
        v-else
        :columns="columns"
        :data="filteredPlugins"
        :bordered="false"
        :pagination="{ pageSize: 20 }"
        size="small"
      />
    </NCard>

    <div class="footer-tip">
      ℹ 卸载的插件移到 .trash 子目录，可手动恢复。安装后会自动装依赖，如失败可点「装依赖」重试。
      切版本会自动备份当前 commit 并重装依赖，可点「回滚」恢复。
    </div>

    <!-- v3.x：切版本弹窗 -->
    <NModal
      v-model:show="showSwitchModal"
      preset="card"
      title="切换插件版本"
      style="width: 600px"
    >
      <NSpace vertical :size="12">
        <div>
          插件：<strong>{{ switchTarget?.name }}</strong>
          <NTag
            v-if="switchTarget?.current_ref"
            size="tiny"
            :type="switchTarget.is_detached ? 'warning' : 'info'"
            style="margin-left: 8px"
          >
            当前：{{ switchTarget.current_ref }}
          </NTag>
          <NTag v-if="switchTarget?.is_detached" size="tiny" type="warning" style="margin-left: 4px">
            detached HEAD
          </NTag>
        </div>

        <div class="hint">
          当前 commit：<code>{{ switchTarget?.current_commit?.slice(0, 12) || "-" }}</code>
          <span v-if="switchTarget?.backup_commit" style="margin-left: 12px">
            上次备份：<code>{{ switchTarget.backup_commit.slice(0, 12) }}</code>
          </span>
        </div>

        <NSelect
          v-model:value="switchTargetRef"
          :options="versionOptions"
          :loading="loadingVersions"
          placeholder="选择目标版本（tag / branch）"
          :disabled="switching"
          filterable
        />

        <NAlert v-if="switchWarning" type="warning" :show-icon="false">
          {{ switchWarning }}
        </NAlert>
        <NAlert v-if="comfyuiRunning" type="error" :show-icon="true">
          ComfyUI 正在运行，切换后需重启才能生效。
        </NAlert>
      </NSpace>

      <template #footer>
        <NSpace justify="end">
          <NButton :disabled="switching" @click="showSwitchModal = false">
            取消
          </NButton>
          <NButton
            v-if="switchTarget?.backup_commit"
            :loading="rolling"
            :disabled="switching"
            type="warning"
            ghost
            @click="onRollback(switchTarget!)"
          >
            回滚
          </NButton>
          <NButton
            type="primary"
            :loading="switching"
            :disabled="switching || !switchTargetRef"
            @click="onConfirmSwitch"
          >
            切换并重装依赖
          </NButton>
        </NSpace>
      </template>
    </NModal>

    <!-- v3.x：venv 健康检查弹窗 -->
    <NModal
      v-model:show="showVenvHealthModal"
      preset="card"
      title="venv 健康检查"
      style="width: 720px; max-width: 90vw"
    >
      <NSpace vertical :size="16">
        <!-- 状态卡片 -->
        <NAlert
          v-if="pluginStore.venvHealth"
          :type="venvStatusAlertType"
          :show-icon="true"
        >
          <div style="font-weight: 600">{{ venvStatusTitle }}</div>
          <div style="margin-top: 4px; opacity: 0.9">{{ venvStatusMessage }}</div>
        </NAlert>

        <!-- 损坏包列表 -->
        <div v-if="pluginStore.venvHealth && pluginStore.venvHealth.broken_distributions.length > 0">
          <div class="venv-section-title">
            损坏包（<code>~xxx*</code>，共 {{ pluginStore.venvHealth.broken_distributions.length }} 个）
          </div>
          <NLog
            :log="pluginStore.venvHealth.broken_distributions
              .map((d) => `${d.name}  (${formatBytes(d.size_bytes)})  ${d.last_modified ?? ''}`)
              .join('\n')"
            :rows="6"
            language="text"
          />
        </div>

        <!-- 关键 import 验证 -->
        <div v-if="pluginStore.venvHealth && pluginStore.venvHealth.critical_imports.length > 0">
          <div class="venv-section-title">关键 import 验证</div>
          <NSpace vertical :size="2">
            <div
              v-for="imp in pluginStore.venvHealth.critical_imports"
              :key="imp.module"
              class="venv-import-row"
            >
              <span :class="['venv-import-badge', imp.ok ? 'ok' : 'fail']">
                {{ imp.ok ? '✓' : '✗' }}
              </span>
              <code class="venv-import-name">{{ imp.module }}</code>
              <span v-if="!imp.ok" class="venv-import-error">
                {{ imp.error ?? 'unknown error' }}
              </span>
            </div>
          </NSpace>
        </div>

        <!-- 路径信息 -->
        <div v-if="pluginStore.venvHealth" class="venv-path-info">
          <div><strong>venv:</strong> <code>{{ pluginStore.venvHealth.venv_path }}</code></div>
          <div><strong>site-packages:</strong> <code>{{ pluginStore.venvHealth.site_packages }}</code></div>
          <div><strong>检查耗时:</strong> {{ pluginStore.venvHealth.elapsed_ms }} ms</div>
        </div>
      </NSpace>

      <template #footer>
        <NSpace justify="end">
          <NButton @click="showVenvHealthModal = false">关闭</NButton>
          <NButton
            type="primary"
            :loading="pluginStore.venvFixLoading"
            :disabled="!pluginStore.venvHasIssue"
            @click="async () => { await onFixVenv(); }"
          >
            一键清理损坏包
          </NButton>
        </NSpace>
      </template>
    </NModal>

    <!-- v3.x：ComfyUI 核心依赖管理弹窗 -->
    <NModal
      v-model:show="showComfyuiCoreModal"
      preset="card"
      title="ComfyUI 核心依赖"
      style="max-width: 700px"
      :bordered="false"
      size="huge"
    >
      <NSpace vertical>
        <!-- 状态 Alert -->
        <NAlert
          v-if="pluginStore.comfyuiCoreStatus"
          :type="pluginStore.comfyuiCoreStatus.needs_install ? 'warning' : 'success'"
          :show-icon="true"
        >
          <template #header>
            <strong>
              {{ pluginStore.comfyuiCoreStatus.needs_install
                ? '需要重装依赖'
                : '依赖已是最新' }}
            </strong>
          </template>
          <div>{{ pluginStore.comfyuiCoreStatus.reason }}</div>
        </NAlert>

        <!-- 详细信息 -->
        <div v-if="pluginStore.comfyuiCoreStatus" class="comfyui-core-info">
          <div class="info-row">
            <strong>ComfyUI 核心目录：</strong>
            <code class="path-text">{{ pluginStore.comfyuiCoreStatus.comfyui_root }}</code>
          </div>
          <div class="info-row">
            <strong>requirements.txt：</strong>
            <code class="path-text">
              {{ pluginStore.comfyuiCoreStatus.requirements_path ?? '(不存在)' }}
            </code>
          </div>
          <div class="info-row" v-if="pluginStore.comfyuiCoreStatus.current_hash">
            <strong>当前 hash：</strong>
            <code>{{ pluginStore.comfyuiCoreStatus.current_hash }}</code>
          </div>
          <div class="info-row" v-if="pluginStore.comfyuiCoreStatus.last_installed_hash">
            <strong>上次装成功 hash：</strong>
            <code>{{ pluginStore.comfyuiCoreStatus.last_installed_hash }}</code>
            <span
              v-if="pluginStore.comfyuiCoreStatus.current_hash
                && pluginStore.comfyuiCoreStatus.current_hash
                  === pluginStore.comfyuiCoreStatus.last_installed_hash"
              class="badge-ok"
            >一致</span>
            <span v-else class="badge-diff">已变化</span>
          </div>
          <div class="info-row">
            <strong>检查耗时：</strong>
            {{ pluginStore.comfyuiCoreStatus.elapsed_ms }} ms
          </div>
        </div>

        <!-- 说明 -->
        <NAlert type="info" :show-icon="true">
          <template #header>关于这个功能</template>
          <p>
            ComfyUI-Manager 切 ComfyUI 核心版本时只改 git，<strong>不自动装依赖</strong>。
            本弹窗用 SHA256 跟踪 requirements.txt 内容变化，提示你何时需要重装。
          </p>
          <p style="margin-top: 6px">
            装依赖会复用插件页的实时日志面板，进度条 0% → 100% 实时刷新。
          </p>
        </NAlert>
      </NSpace>

      <template #footer>
        <NSpace justify="end">
          <NButton @click="showComfyuiCoreModal = false">关闭</NButton>
          <NButton
            type="warning"
            :loading="pluginStore.comfyuiCoreInstalling"
            @click="onInstallComfyuiCore(true)"
          >
            强制重装（--force-reinstall）
          </NButton>
          <NButton
            type="primary"
            :loading="pluginStore.comfyuiCoreInstalling"
            :disabled="!pluginStore.comfyuiCoreStatus?.needs_install"
            @click="onInstallComfyuiCore(false)"
          >
            装依赖
          </NButton>
        </NSpace>
      </template>
    </NModal>
  </div>
</template>

<style scoped>
.plugin-page {
  padding: 16px;
  max-width: 1200px;
  margin: 0 auto;
}

.toolbar,
.stats,
.progress-panel {
  margin-bottom: 16px;
}

.progress-panel.is-failed {
  border-color: var(--app-color-error, #d03050);
}

.toolbar-row {
  display: flex;
  gap: 8px;
  margin-bottom: 8px;
  align-items: center;
}

.toolbar-row:last-child {
  margin-bottom: 0;
}

.url-input,
.search-input {
  flex: 1;
}

.tag-select {
  width: 300px;
}

.loading {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 12px;
  padding: 48px 0;
}

.hint {
  font-size: 12px;
  color: var(--app-text-muted, #999);
}

.footer-tip {
  margin-top: 16px;
  padding: 8px 12px;
  background: var(--app-bg-soft, rgba(127, 127, 127, 0.05));
  border-radius: 4px;
  font-size: 12px;
  color: var(--app-text-muted, #999);
}

/* v3.x：实时进度面板 */
.progress-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 8px;
  gap: 12px;
}

.progress-info {
  display: flex;
  align-items: center;
  gap: 8px;
  flex: 1;
  min-width: 0;
}

.progress-percent {
  font-size: 16px;
  font-weight: 600;
  color: var(--app-color-primary, #2080f0);
  font-variant-numeric: tabular-nums;
  min-width: 48px;
  text-align: right;
}

.progress-footer {
  display: flex;
  gap: 8px;
  margin-top: 8px;
}

.install-logs {
  margin-top: 8px;
  border-radius: 4px;
  background: var(--app-bg-code, rgba(127, 127, 127, 0.05));
  padding: 8px;
  max-height: 300px;
}

.logs-empty {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 16px;
  justify-content: center;
}

/* v3.x：venv 健康弹窗 */
.venv-section-title {
  font-size: 13px;
  font-weight: 600;
  margin-bottom: 4px;
  opacity: 0.85;
}

.venv-import-row {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
}

.venv-import-badge {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 18px;
  height: 18px;
  border-radius: 4px;
  font-weight: 600;
  font-size: 11px;
  flex-shrink: 0;
}
.venv-import-badge.ok {
  background: #18a05833;
  color: #18a058;
}
.venv-import-badge.fail {
  background: #d0305033;
  color: #d03050;
}

.venv-import-name {
  flex-shrink: 0;
  min-width: 200px;
}

.venv-import-error {
  font-size: 11px;
  opacity: 0.7;
  word-break: break-all;
}

.venv-path-info {
  font-size: 11px;
  opacity: 0.7;
  line-height: 1.6;
  padding-top: 8px;
  border-top: 1px solid var(--n-border-color, #eee);
}
.venv-path-info code {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 11px;
}

/* v3.x：ComfyUI 核心依赖弹窗 */
.comfyui-core-info {
  font-size: 12px;
  line-height: 1.7;
  padding: 12px;
  background: var(--n-card-color, #fafafa);
  border-radius: 4px;
}
.comfyui-core-info .info-row {
  display: flex;
  align-items: center;
  gap: 6px;
  margin: 4px 0;
  word-break: break-all;
}
.comfyui-core-info .path-text {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 11px;
  color: var(--n-text-color, #333);
}
.comfyui-core-info .badge-ok,
.comfyui-core-info .badge-diff {
  display: inline-block;
  padding: 1px 6px;
  border-radius: 8px;
  font-size: 10px;
  font-weight: 600;
  margin-left: 4px;
}
.comfyui-core-info .badge-ok {
  background: rgba(24, 160, 88, 0.15);
  color: #18a058;
}
.comfyui-core-info .badge-diff {
  background: rgba(240, 160, 32, 0.15);
  color: #f0a020;
}
</style>
