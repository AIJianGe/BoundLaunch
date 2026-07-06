<script setup lang="ts">
/**
 * 仓库地址切换与备份恢复对话框（F31）
 *
 * 功能：
 * 1. 查看当前仓库地址（脱敏后的，只读）
 * 2. 输入新仓库地址并切换（切换前会备份当前 ComfyUI）
 * 3. 一键恢复为官方仓库地址
 * 4. 可选择是否迁移 custom_nodes 到新仓库
 * 5. 列出所有备份并支持恢复
 *
 * ## 设计模式
 * - **Container**：从 useCoreStore 取数据 / 调 action，自己管理表单状态
 * - **Adapter**：将后端 SwitchRepoResult（success / rolled_back）转为用户可读提示
 *
 * 详见 F31「ComfyUI 仓库地址切换与备份恢复」
 */

import { ref, computed, watch, h } from "vue";
import {
  NModal,
  NCard,
  NInput,
  NButton,
  NAlert,
  NDataTable,
  NCheckbox,
  NSpin,
  NSpace,
  NEmpty,
  type DataTableColumns,
} from "naive-ui";
import { useCoreStore } from "@/stores/core";
import { useToast } from "@/composables/useToast";
import { useConfirm } from "@/composables/useConfirm";
import { coreOfficialRepoUrl } from "@/api/core";
import type { BackupInfo } from "@/api/types";

const props = defineProps<{
  /** 是否显示 */
  show: boolean;
}>();

const emit = defineEmits<{
  (e: "update:show", v: boolean): void;
}>();

const coreStore = useCoreStore();
const toast = useToast();
const confirm = useConfirm();

/** 新仓库地址输入框 */
const newUrl = ref("");
/** 是否迁移 custom_nodes（默认勾选） */
const migrateCustomNodes = ref(true);
/** 当前正在恢复的备份名（null = 无），用于行级 loading */
const restoringName = ref<string | null>(null);

/** 切换中 / 恢复中（来自 store） */
const switching = computed(() => coreStore.switchingRepo);

/** 备份列表（来自 store） */
const backups = computed<BackupInfo[]>(() => coreStore.backups);

/** 当前仓库地址（来自 store，脱敏后的） */
const currentUrl = computed<string>(() => coreStore.repoUrl);

/** 对话框打开时刷新数据 */
watch(
  () => props.show,
  async (v) => {
    if (!v) return;
    newUrl.value = "";
    migrateCustomNodes.value = true;
    restoringName.value = null;
    try {
      await Promise.all([coreStore.refreshRepoUrl(), coreStore.refreshBackups()]);
    } catch (e) {
      toast.error("加载数据失败", e);
    }
  },
);

/** 格式化备份时间为本地时间 */
function formatTime(iso: string): string {
  try {
    const d = new Date(iso);
    if (isNaN(d.getTime())) return iso;
    return d.toLocaleString();
  } catch {
    return iso;
  }
}

/** 格式化备份大小为 MB */
function formatSize(bytes: number): string {
  if (!bytes || bytes < 0) return "—";
  return (bytes / 1024 / 1024).toFixed(2) + " MB";
}

/** 格式化版本信息（tag + 短 commit） */
function formatVersion(row: BackupInfo): string {
  const short = row.current_commit ? row.current_commit.slice(0, 7) : "";
  if (row.current_tag) return `${row.current_tag} (${short})`;
  return short || "—";
}

/** 表格列定义 */
const columns = computed<DataTableColumns<BackupInfo>>(() => [
  {
    title: "备份时间",
    key: "backed_up_at",
    width: 180,
    render: (row) => formatTime(row.backed_up_at),
  },
  {
    title: "仓库地址",
    key: "repo_url_masked",
    ellipsis: { tooltip: true },
  },
  {
    title: "版本",
    key: "version",
    width: 180,
    render: (row) => formatVersion(row),
  },
  {
    title: "大小",
    key: "size_bytes",
    width: 100,
    render: (row) => formatSize(row.size_bytes),
  },
  {
    title: "操作",
    key: "actions",
    width: 90,
    fixed: "right",
    render: (row) =>
      h(
        NButton,
        {
          size: "small",
          type: "warning",
          disabled: switching.value || restoringName.value !== null,
          loading: restoringName.value === row.name,
          onClick: () => onRestore(row.name),
        },
        { default: () => "恢复" },
      ),
  },
]);

/** 一键填充官方仓库地址 */
async function onRestoreOfficial() {
  try {
    newUrl.value = await coreOfficialRepoUrl();
  } catch (e) {
    toast.error("获取官方仓库地址失败", e);
  }
}

/** 确认切换仓库地址 */
async function onConfirmSwitch() {
  const url = newUrl.value.trim();
  if (!url) {
    toast.warn("请输入新仓库地址");
    return;
  }
  const ok = await confirm.warn(
    "切换仓库地址",
    "切换仓库地址会备份当前 ComfyUI 并重新克隆，是否继续？",
  );
  if (!ok) return;
  try {
    const result = await coreStore.switchRepoUrl(url, migrateCustomNodes.value);
    if (result.kind === "success") {
      toast.success("仓库地址切换成功");
      toast.warn("建议重建 venv 以匹配新仓库依赖");
      emit("update:show", false);
    } else {
      toast.error("切换失败已回滚", result.error);
    }
  } catch (e) {
    toast.error("切换失败", e);
  }
}

/** 恢复指定备份 */
async function onRestore(backupName: string) {
  const ok = await confirm.danger(
    "恢复备份",
    "恢复将用备份替换当前 ComfyUI，当前未备份的改动会丢失，是否继续？",
  );
  if (!ok) return;
  restoringName.value = backupName;
  try {
    const result = await coreStore.restoreBackup(backupName);
    if (result.kind === "success") {
      toast.success("备份恢复成功");
      toast.warn("建议重建 venv 以匹配新仓库依赖");
      emit("update:show", false);
    } else {
      toast.error("恢复失败已回滚", result.error);
    }
  } catch (e) {
    toast.error("恢复失败", e);
  } finally {
    restoringName.value = null;
  }
}

function onCancel() {
  emit("update:show", false);
}
</script>

<template>
  <NModal
    :show="show"
    @update:show="(v: boolean) => !v && onCancel()"
    :mask-closable="false"
    :auto-focus="true"
  >
    <NCard
      class="repo-dialog-card"
      :bordered="false"
      size="small"
      role="dialog"
      aria-modal="true"
    >
      <template #header>
        <span class="dialog-title">仓库地址切换与备份恢复</span>
      </template>
      <template #header-extra>
        <NButton
          quaternary
          size="small"
          :disabled="switching"
          @click="onCancel"
        >
          ✕
        </NButton>
      </template>

      <NSpin :show="switching">
        <div class="dialog-body" :class="{ 'is-disabled': switching }">
          <NSpace vertical :size="14">
            <!-- 警告提示 -->
            <NAlert
              type="warning"
              :show-icon="true"
              :bordered="false"
              class="alert-box"
            >
              切换仓库地址会先备份当前 ComfyUI 目录，再重新克隆目标仓库。
              请确保 ComfyUI 已停止运行，且网络可访问目标仓库地址。
            </NAlert>

            <!-- 当前地址 -->
            <div class="field">
              <div class="field-label">当前仓库地址</div>
              <NInput
                :value="currentUrl || '—'"
                readonly
                placeholder="—"
              />
            </div>

            <!-- 新地址 -->
            <div class="field">
              <div class="field-label">新仓库地址</div>
              <div class="url-row">
                <NInput
                  v-model:value="newUrl"
                  placeholder="https://github.com/xxx/ComfyUI.git"
                  :disabled="switching"
                  clearable
                />
                <NButton
                  size="small"
                  type="info"
                  :disabled="switching"
                  @click="onRestoreOfficial"
                >
                  恢复官方
                </NButton>
              </div>
            </div>

            <!-- 迁移 custom_nodes -->
            <NCheckbox
              v-model:checked="migrateCustomNodes"
              :disabled="switching"
            >
              迁移 custom_nodes 到新仓库
            </NCheckbox>

            <!-- 备份列表 -->
            <div class="field">
              <div class="field-label">备份列表</div>
              <NDataTable
                :columns="columns"
                :data="backups"
                :bordered="true"
                :single-line="false"
                size="small"
                :max-height="260"
                :scroll-x="640"
              >
                <template #empty>
                  <NEmpty description="暂无备份" size="small" />
                </template>
              </NDataTable>
            </div>
          </NSpace>
        </div>
      </NSpin>

      <template #footer>
        <div class="dialog-footer">
          <NButton
            :disabled="switching"
            @click="onCancel"
          >
            取消
          </NButton>
          <NButton
            type="primary"
            :loading="switching"
            :disabled="switching || !newUrl.trim()"
            @click="onConfirmSwitch"
          >
            确认切换
          </NButton>
        </div>
      </template>
    </NCard>
  </NModal>
</template>

<style scoped>
.repo-dialog-card {
  width: 680px;
  max-width: 92vw;
  max-height: 88vh;
  overflow-y: auto;
}

.dialog-title {
  font-weight: 600;
  font-size: 16px;
}

.dialog-body {
  padding: 4px 0;
  transition: opacity 0.2s;
}

.dialog-body.is-disabled {
  opacity: 0.6;
  pointer-events: none;
}

.alert-box {
  font-size: 12px;
  line-height: 1.6;
}

.field {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.field-label {
  font-size: 13px;
  font-weight: 600;
  color: var(--app-text, #333);
}

.url-row {
  display: flex;
  gap: 8px;
  align-items: center;
}

.url-row :deep(.n-input) {
  flex: 1;
}

.dialog-footer {
  display: flex;
  justify-content: flex-end;
  gap: 12px;
}
</style>
