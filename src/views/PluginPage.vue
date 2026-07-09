<script setup lang="ts">
/**
 * 插件管理页
 *
 * 详见 `PR/06-界面设计.md §5.2 插件管理页`
 *
 * 区块：
 * 1. 顶部工具栏：Git URL 输入 + [获取版本] + [安装] + 搜索框 + [批量检查更新]
 * 2. 统计栏：总数 / 启用 / 禁用 / 待更新
 * 3. 插件列表表格：插件名 | commit | 启用开关 | [更新] [装依赖] [卸载]
 * 4. 空状态：提示输入 Git URL
 * 5. 操作确认弹窗
 *
 * Tag 选择流程：
 * 1. 用户输入 URL → 点"获取版本"
 * 2. 调 pluginListRemoteTags(url) 获取 tag 列表
 * 3. 有 tag → NSelect 下拉选择（默认"最新 commit（默认分支）"）
 * 4. 无 tag → 提示"无 tag，直接安装最新版"
 * 5. 点"安装" → pluginInstall(url, selectedTag)
 */

import { h, ref, computed, onMounted } from "vue";
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
  type DataTableColumns,
} from "naive-ui";
import { usePluginStore } from "@/stores/plugin";
import { useToast } from "@/composables/useToast";
import { pluginListRemoteTags } from "@/api/plugin";
import type { PluginInfo, RemoteTagInfo } from "@/api/types";

const pluginStore = usePluginStore();
const toast = useToast();

const gitUrlInput = ref("");
const searchQuery = ref("");
const installing = ref(false);
const fetchingTags = ref(false);

// Tag 选择相关
const remoteTags = ref<RemoteTagInfo[]>([]);
const selectedTag = ref<string | null>(null);

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
    width: 320,
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
});

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
  try {
    await pluginStore.installRequirements(name);
    toast.success(`${name} 依赖安装完成`);
    // 刷新列表以更新 requirements_installed 字段
    await pluginStore.refresh(true);
  } catch (e) {
    toast.error("依赖安装失败", e);
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
      </div>
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
    </div>
  </div>
</template>

<style scoped>
.plugin-page {
  padding: 16px;
  max-width: 1200px;
  margin: 0 auto;
}

.toolbar,
.stats {
  margin-bottom: 16px;
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
</style>
