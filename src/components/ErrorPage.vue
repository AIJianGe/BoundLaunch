<script setup lang="ts">
/**
 * ErrorPage - 全局错误页
 *
 * 详见 `PR/06-界面设计.md §7.7 全局错误页`
 *
 * 触发场景：
 * - Config 文件损坏（已自动备份并新建默认配置）
 * - venv 损坏（.launcher-dirty 标记存在）
 * - LogStore SQLite 数据库损坏（降级为仅内存缓冲）
 * - 关键依赖文件缺失
 * - 404 路由（找不到页面）
 * - 路由懒加载失败（chunk 加载错误）
 *
 * 使用方式：
 * ```vue
 * <ErrorPage
 *   type="config_corrupted"
 *   title="配置文件损坏"
 *   :details="error.stack"
 *   @retry="onRetry"
 *   @feedback="onFeedback"
 * />
 * ```
 *
 * 设计模式：
 * - **Strategy**：不同 ErrorType 不同操作（重试 / 打开备份 / 反馈）
 * - **Facade**：单一入口处理所有全局错误
 */

import { computed } from "vue";
import { NCard, NButton, NSpace, NResult, NCode } from "naive-ui";
import { open as openExternal } from "@tauri-apps/plugin-shell";

export type ErrorType =
  | "config_corrupted"
  | "venv_dirty"
  | "log_db_corrupted"
  | "dependency_missing"
  | "not_found"
  | "chunk_load_failed"
  | "generic";

interface Props {
  type?: ErrorType;
  title?: string;
  details?: string;
  /** 重试按钮可见性（默认 true） */
  retryable?: boolean;
  /** 备份文件路径（仅 config_corrupted 适用） */
  backupPath?: string;
}

const props = withDefaults(defineProps<Props>(), {
  type: "generic",
  retryable: true,
});

const emit = defineEmits<{
  retry: [];
  feedback: [];
  close: [];
}>();

const presets: Record<ErrorType, {
  status: "error" | "warning" | "info" | "404" | "500" | "418";
  defaultTitle: string;
  defaultDesc: string;
}> = {
  config_corrupted: {
    status: "warning",
    defaultTitle: "配置文件损坏",
    defaultDesc: "已自动备份原文件并创建新默认配置，请重新配置后继续。",
  },
  venv_dirty: {
    status: "warning",
    defaultTitle: "环境异常",
    defaultDesc: "检测到 .launcher-dirty 标记，torch 或关键依赖可能缺失。建议重建 venv。",
  },
  log_db_corrupted: {
    status: "info",
    defaultTitle: "日志持久化失败",
    defaultDesc: "SQLite 数据库损坏，已降级为仅内存缓冲。本次会话日志仍可查看，重启后将丢失。",
  },
  dependency_missing: {
    status: "error",
    defaultTitle: "关键依赖缺失",
    defaultDesc: "缺少运行所需的关键文件，请检查安装。",
  },
  not_found: {
    status: "404",
    defaultTitle: "页面不存在",
    defaultDesc: "请求的页面未找到，请检查 URL 或返回首页。",
  },
  chunk_load_failed: {
    status: "warning",
    defaultTitle: "资源加载失败",
    defaultDesc: "前端资源加载失败，可能是版本已更新，请刷新页面或清除缓存。",
  },
  generic: {
    status: "error",
    defaultTitle: "出错了",
    defaultDesc: "发生未知错误，请重试或反馈问题。",
  },
};

const preset = computed(() => presets[props.type]);
const displayTitle = computed(() => props.title || preset.value.defaultTitle);
const displayDesc = computed(() => props.details || preset.value.defaultDesc);

function onRetry() {
  emit("retry");
}

function onFeedback() {
  emit("feedback");
  // 默认打开 GitHub Issues
  openExternal("https://github.com/your-org/BoundLaunch/issues").catch(() => {
          window.open("https://github.com/your-org/BoundLaunch/issues", "_blank");
  });
}

async function onOpenBackup() {
  if (!props.backupPath) return;
  // 打开文件管理器并选中备份文件
  try {
    // Tauri 2 plugin-shell 的 open 仅支持 URL，打开文件管理器需用 revealItemInDir
    // 简化：使用 open 命令打开所在目录
    const dir = props.backupPath.replace(/[/\\][^/\\]+$/, "");
    await openExternal(dir);
  } catch (e) {
    console.warn("open backup dir failed:", e);
  }
}

function onClose() {
  emit("close");
}
</script>

<template>
  <div class="error-page">
    <NCard :bordered="false" size="large" class="error-card">
      <NResult
        :status="preset.status"
        :title="displayTitle"
        :description="displayDesc"
      >
        <template v-if="details" #footer>
          <div class="error-details">
            <div class="details-label">错误详情：</div>
            <NCode :code="details" language="text" word-wrap class="details-code" />
          </div>
        </template>

        <template #footer>
          <div v-if="backupPath" class="backup-info">
            已自动备份为：<code>{{ backupPath }}</code>
          </div>

          <NSpace justify="center" :size="8" class="actions">
            <NButton v-if="retryable" type="primary" @click="onRetry">
              重试
            </NButton>
            <NButton v-if="backupPath" @click="onOpenBackup">
              打开备份
            </NButton>
            <NButton @click="onFeedback">
              反馈问题
            </NButton>
            <NButton quaternary @click="onClose">
              关闭
            </NButton>
          </NSpace>
        </template>
      </NResult>
    </NCard>
  </div>
</template>

<style scoped>
.error-page {
  display: flex;
  align-items: center;
  justify-content: center;
  min-height: 60vh;
  padding: 24px;
}

.error-card {
  max-width: 720px;
  width: 100%;
}

.error-details {
  margin: 16px 0;
  text-align: left;
}

.details-label {
  font-size: 12px;
  color: var(--app-text-muted, #999);
  margin-bottom: 4px;
}

.details-code {
  max-height: 240px;
  overflow-y: auto;
  font-size: 12px;
}

.backup-info {
  margin: 12px 0;
  font-size: 12px;
  color: var(--app-text-muted, #999);
}

.backup-info code {
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
  background: rgba(127, 127, 127, 0.1);
  padding: 1px 6px;
  border-radius: 3px;
}

.actions {
  margin-top: 16px;
}
</style>
