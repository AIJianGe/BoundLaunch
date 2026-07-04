<script setup lang="ts">
/**
 * 关于/更新页
 *
 * 详见 `PR/06-界面设计.md §5.6 关于/更新页`
 *
 * 区块：
 * 1. 顶部右侧：[检查更新] 按钮
 * 2. 中央：launcher 版本号 + ComfyUI 核心版本
 * 3. 更新日志（折叠）
 * 4. 技术栈（折叠）
 * 5. 链接区（项目仓库 / 反馈 / 文档）
 * 6. 底部：版权 + License
 *
 * 设计模式：
 * - **Facade**：集中编排 5 个区块的展示
 * - **State**：updateState 状态机管理更新流程
 *   (idle → checking → up_to_date → available → updating → failed)
 *
 * 注意：实际更新检查依赖 Tauri Updater 插件或自建服务器，
 *      当前为前端占位实现，后续接入后端时只需替换 checkForUpdate 函数。
 */

import { ref, computed, onMounted } from "vue";
import {
  NCard,
  NButton,
  NTag,
  NSpace,
  NCollapse,
  NCollapseItem,
  NDescriptions,
  NDescriptionsItem,
  NResult,
  NSpin,
  useOsTheme,
} from "naive-ui";
import { useCoreStore } from "@/stores/core";
import { useToast } from "@/composables/useToast";
import { open as openExternal } from "@tauri-apps/plugin-shell";

const coreStore = useCoreStore();
const toast = useToast();

// launcher 版本（来自 package.json，构建时由 Vite 注入）
const launcherVersion = __APP_VERSION__;

// ComfyUI 核心版本
const comfyuiVersion = computed(() => coreStore.currentVersion ?? "未安装");

// ========== 更新状态机 ==========
type UpdateState =
  | { phase: "idle" }
  | { phase: "checking" }
  | { phase: "up_to_date" }
  | { phase: "available"; version: string; notes: string }
  | { phase: "failed"; error: string };

const updateState = ref<UpdateState>({ phase: "idle" });

const isChecking = computed(() => updateState.value.phase === "checking");
const hasUpdate = computed(() => updateState.value.phase === "available");
const updateVersion = computed(() =>
  updateState.value.phase === "available" ? updateState.value.version : "",
);
const updateNotes = computed(() =>
  updateState.value.phase === "available" ? updateState.value.notes : "",
);

// ========== 静态信息 ==========

interface ChangeEntry {
  version: string;
  date: string;
  notes: string[];
}

const changelog: ChangeEntry[] = [
  {
    version: "v0.1.0",
    date: "2026-07-03",
    notes: [
      "首次发布",
      "ComfyUI 启动器核心功能（启动/停止/状态机）",
      "环境管理（Python 切换 / venv 重建 / torch CUDA 配置）",
      "插件管理（Git 仓库安装 / 启用 / 更新 / 卸载）",
      "模型路径管理（扫描 / extra_model_paths.yaml 生成）",
      "任务调度器 + 日志持久化",
    ],
  },
];

interface TechStackEntry {
  name: string;
  version: string;
  description: string;
}

const techStack: TechStackEntry[] = [
  { name: "Tauri", version: "2.x", description: "跨平台桌面应用框架" },
  { name: "Vue", version: "3.x", description: "前端响应式框架" },
  { name: "Rust", version: "1.75+", description: "后端系统语言" },
  { name: "uv", version: "0.4.x", description: "Python 环境管理" },
  { name: "Naive UI", version: "2.38+", description: "Vue 3 UI 组件库" },
  { name: "Pinia", version: "2.2+", description: "Vue 状态管理" },
  { name: "TypeScript", version: "5.5+", description: "前端类型系统" },
];

interface LinkEntry {
  label: string;
  icon: string;
  url: string;
  description: string;
}

const links: LinkEntry[] = [
  {
    label: "项目仓库",
    icon: "📦",
    url: "https://github.com/AIJianGe/BoundLaunch",
    description: "GitHub 源码仓库",
  },
  {
    label: "反馈问题",
    icon: "🐛",
    url: "https://github.com/your-org/BoundLaunch/issues",
    description: "提交 Bug 或功能建议",
  },
  {
    label: "使用文档",
    icon: "📖",
    url: "https://github.com/AIJianGe/BoundLaunch/wiki",
    description: "Wiki 文档与教程",
  },
  {
    label: "ComfyUI 官方",
    icon: "🔗",
    url: "https://github.com/comfyanonymous/ComfyUI",
    description: "上游 ComfyUI 项目",
  },
];

const osTheme = useOsTheme();

// ========== Actions ==========

async function onCheckUpdate() {
  if (isChecking.value) return;
  updateState.value = { phase: "checking" };

  try {
    // TODO: 接入 Tauri Updater 插件或自建更新服务器
    // 当前为占位实现：模拟网络请求延迟
    await new Promise((resolve) => setTimeout(resolve, 1500));

    // 实际接入时替换为：
    //   const update = await check();  // @tauri-apps/plugin-updater
    //   if (update) {
    //     updateState.value = {
    //       phase: "available",
    //       version: update.version,
    //       notes: update.body,
    //     };
    //   } else {
    //     updateState.value = { phase: "up_to_date" };
    //     toast.success("当前为最新版本");
    //   }

    // 占位：始终返回"已是最新"
    updateState.value = { phase: "up_to_date" };
    toast.success("当前为最新版本");
  } catch (e) {
    updateState.value = {
      phase: "failed",
      error: e instanceof Error ? e.message : String(e),
    };
    toast.error("检查更新失败", e);
  }
}

async function onOpenLink(url: string) {
  try {
    await openExternal(url);
  } catch (e) {
    // 降级到 window.open（如 Tauri shell 插件未启用或 Web 预览环境）
    console.warn("openExternal failed, fallback to window.open:", e);
    window.open(url, "_blank");
  }
}

function onCopyVersion() {
  navigator.clipboard.writeText(
    `无界启动器 ${launcherVersion} / ComfyUI ${comfyuiVersion.value} / OS ${platform.value}` ?? "unknown"}`,
  ).then(
    () => toast.success("版本信息已复制"),
    () => toast.error("复制失败"),
  );
}

// ========== 生命周期 ==========

onMounted(async () => {
  // 加载 ComfyUI 核心状态
  try {
    if (!coreStore.status) {
      await coreStore.refresh();
    }
  } catch (e) {
    console.warn("core refresh:", e);
  }
});
</script>

<template>
  <div class="about-page">
    <!-- 顶部右上角：检查更新按钮 -->
    <div class="top-bar">
      <NButton
        :loading="isChecking"
        :disabled="isChecking"
        size="small"
        @click="onCheckUpdate"
      >
        {{ isChecking ? "检查更新中..." : "🔄 检查更新" }}
      </NButton>
    </div>

    <!-- 版本信息区 -->
    <div class="version-block">
      <div class="app-icon">🚀</div>
      <h1 class="app-name">无界启动器</h1>
      <div class="version-row">
        <span class="launcher-version">v{{ launcherVersion }}</span>
        <NTag
          v-if="hasUpdate"
          size="small"
          type="warning"
          class="version-arrow"
        >
          → v{{ updateVersion }}
        </NTag>
      </div>
      <div class="core-version">
        ComfyUI {{ comfyuiVersion }}
      </div>
      <NButton size="tiny" quaternary @click="onCopyVersion">
        复制版本信息
      </NButton>
    </div>

    <!-- 更新提示（如有新版本） -->
    <NCard
      v-if="hasUpdate"
      :bordered="true"
      size="small"
      class="update-banner"
    >
      <NResult
        status="info"
        :title="`🎉 发现新版本 v${updateVersion}`"
      >
        <template #footer>
          <div class="update-notes">
            <pre>{{ updateNotes }}</pre>
          </div>
          <NSpace justify="center">
            <NButton type="primary" disabled>立即更新</NButton>
            <NButton>稍后提醒</NButton>
            <NButton quaternary>忽略此版本</NButton>
          </NSpace>
          <div class="update-tip">
            ℹ 更新功能将在后续版本接入 Tauri Updater 插件后启用
          </div>
        </template>
      </NResult>
    </NCard>

    <!-- 更新日志 -->
    <NCard :bordered="true" size="small" class="section-card">
      <NCollapse arrow-placement="right" :default-expanded-names="[]">
        <NCollapseItem name="changelog" title="▶ 更新日志">
          <div class="changelog-list">
            <div
              v-for="entry in changelog"
              :key="entry.version"
              class="changelog-entry"
            >
              <div class="changelog-header">
                <span class="changelog-version">{{ entry.version }}</span>
                <span class="changelog-date">{{ entry.date }}</span>
              </div>
              <ul class="changelog-notes">
                <li v-for="(note, idx) in entry.notes" :key="idx">
                  {{ note }}
                </li>
              </ul>
            </div>
          </div>
        </NCollapseItem>
      </NCollapse>
    </NCard>

    <!-- 技术栈 -->
    <NCard :bordered="true" size="small" class="section-card">
      <NCollapse arrow-placement="right" :default-expanded-names="[]">
        <NCollapseItem name="techstack" title="▶ 技术栈">
          <NDescriptions :column="1" size="small" label-placement="left" bordered>
            <NDescriptionsItem
              v-for="tech in techStack"
              :key="tech.name"
              :label="tech.name"
            >
              <span class="tech-version">{{ tech.version }}</span>
              <span class="tech-desc">{{ tech.description }}</span>
            </NDescriptionsItem>
          </NDescriptions>
        </NCollapseItem>
      </NCollapse>
    </NCard>

    <!-- 链接 -->
    <NCard :bordered="true" size="small" class="section-card">
      <template #header>
        <span class="header-title">🔗 链接</span>
      </template>
      <div class="links-list">
        <div
          v-for="link in links"
          :key="link.url"
          class="link-row"
          @click="onOpenLink(link.url)"
        >
          <div class="link-icon">{{ link.icon }}</div>
          <div class="link-info">
            <div class="link-label">
              {{ link.label }}
              <span class="link-url">{{ link.url }}</span>
            </div>
            <div class="link-desc">{{ link.description }}</div>
          </div>
          <NButton size="tiny" quaternary>打开</NButton>
        </div>
      </div>
    </NCard>

    <!-- 加载中遮罩 -->
    <div v-if="coreStore.loading && !coreStore.status" class="loading-overlay">
      <NSpin size="small" />
      <span>正在加载核心版本...</span>
    </div>

    <!-- 底部版权 -->
    <div class="footer">
      © 2026 BoundLaunch · 开源软件 (MIT License)
    </div>
  </div>
</template>

<style scoped>
.about-page {
  padding: 24px 16px 16px;
  max-width: 800px;
  margin: 0 auto;
  position: relative;
  min-height: 100%;
}

.top-bar {
  display: flex;
  justify-content: flex-end;
  margin-bottom: 24px;
}

.version-block {
  text-align: center;
  padding: 32px 0 24px;
  border-bottom: 1px solid var(--app-border, rgba(0, 0, 0, 0.08));
  margin-bottom: 24px;
}

.app-icon {
  font-size: 48px;
  margin-bottom: 8px;
}

.app-name {
  font-size: 28px;
  font-weight: 700;
  margin: 0 0 8px;
  letter-spacing: 0.5px;
}

.version-row {
  display: flex;
  justify-content: center;
  align-items: center;
  gap: 8px;
  margin-bottom: 4px;
}

.launcher-version {
  font-size: 24px;
  font-weight: 600;
  color: var(--app-text, #333);
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
}

.version-arrow {
  font-weight: 600;
}

.core-version {
  font-size: 14px;
  color: var(--app-text-muted, #999);
  margin-bottom: 12px;
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
}

.update-banner {
  margin-bottom: 16px;
}

.update-notes pre {
  margin: 12px 0;
  padding: 12px;
  background: rgba(127, 127, 127, 0.05);
  border-radius: 4px;
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
  font-size: 12px;
  text-align: left;
  white-space: pre-wrap;
  word-break: break-all;
}

.update-tip {
  margin-top: 8px;
  font-size: 12px;
  color: var(--app-text-muted, #999);
}

.section-card {
  margin-bottom: 12px;
}

.header-title {
  font-weight: 600;
}

.changelog-list {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.changelog-entry {
  padding: 8px 0;
}

.changelog-header {
  display: flex;
  align-items: baseline;
  gap: 12px;
  margin-bottom: 8px;
}

.changelog-version {
  font-weight: 600;
  font-size: 14px;
  color: #1890ff;
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
}

.changelog-date {
  font-size: 12px;
  color: var(--app-text-muted, #999);
}

.changelog-notes {
  margin: 0;
  padding-left: 20px;
  font-size: 13px;
  line-height: 1.7;
}

.changelog-notes li {
  list-style: disc;
}

.tech-version {
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
  font-weight: 600;
  margin-right: 8px;
  color: #1890ff;
}

.tech-desc {
  color: var(--app-text-muted, #666);
}

.links-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.link-row {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 10px 12px;
  border-radius: 4px;
  cursor: pointer;
  transition: background 0.15s;
}

.link-row:hover {
  background: rgba(127, 127, 127, 0.06);
}

.link-icon {
  font-size: 20px;
}

.link-info {
  flex: 1;
  min-width: 0;
}

.link-label {
  font-weight: 500;
  display: flex;
  align-items: baseline;
  gap: 8px;
  flex-wrap: wrap;
}

.link-url {
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
  font-size: 11px;
  color: var(--app-text-muted, #999);
  word-break: break-all;
}

.link-desc {
  font-size: 12px;
  color: var(--app-text-muted, #999);
  margin-top: 2px;
}

.loading-overlay {
  position: absolute;
  top: 50%;
  left: 50%;
  transform: translate(-50%, -50%);
  display: flex;
  align-items: center;
  gap: 8px;
  color: var(--app-text-muted, #999);
}

.footer {
  text-align: center;
  padding: 24px 0 12px;
  font-size: 12px;
  color: var(--app-text-muted, #999);
  margin-top: 12px;
  border-top: 1px solid var(--app-border, rgba(0, 0, 0, 0.06));
}
</style>
