<script setup lang="ts">
/**
 * FolderPicker - 路径输入 + 浏览按钮
 *
 * 职责：
 * - 提供可手动输入的路径输入框（保留 NInput 全部行为）
 * - 提供「浏览」按钮，调用 Tauri 2 原生 dialog 选择文件夹
 * - 跨平台（Windows / Linux / macOS 均使用同一 API）
 *
 * 设计模式：
 * - **Adapter**：封装 `@tauri-apps/plugin-dialog` 的 `open` 调用，
 *   屏蔽底层平台差异，对外暴露 v-model + change 事件
 * - **Composite**：组合 NInput + NButton，对外作为单个表单控件
 *
 * 使用方式：
 * ```vue
 * <FolderPicker
 *   v-model="path"
 *   placeholder="如 D:\AIWork\ComfyUI"
 *   :status="pathError ? 'error' : 'success'"
 *   :clearable="true"
 *   title="选择 ComfyUI 根目录"
 *   @change="onPathChange"
 * />
 * ```
 *
 * 注：必须在 `NDialogProvider` 等 Provider 内部使用（toast 依赖）
 */

import { NInput, NButton, NIcon } from "naive-ui";
import { open } from "@tauri-apps/plugin-dialog";
import { useToast } from "@/composables/useToast";

interface Props {
  /** 当前路径（v-model 绑定） */
  modelValue: string;
  /** placeholder */
  placeholder?: string;
  /** 输入框校验状态 */
  status?: "success" | "warning" | "error";
  /** 是否可清空 */
  clearable?: boolean;
  /** 是否禁用 */
  disabled?: boolean;
  /** 选择对话框标题（默认「选择文件夹」） */
  dialogTitle?: string;
  /** 浏览按钮文字（默认「浏览」） */
  buttonText?: string;
  /** 输入框尺寸（默认与父表单一致） */
  size?: "tiny" | "small" | "medium" | "large";
}

const props = withDefaults(defineProps<Props>(), {
  placeholder: "",
  status: "success",
  clearable: false,
  disabled: false,
  dialogTitle: "选择文件夹",
  buttonText: "浏览",
  size: "small",
});

const emit = defineEmits<{
  (e: "update:modelValue", value: string): void;
  /** 用户主动选择文件夹（点浏览按钮选择）后触发 */
  (e: "change", value: string): void;
}>();

const toast = useToast();

/** 调用 Tauri 原生 dialog 选择文件夹 */
async function pickFolder() {
  if (props.disabled) return;
  try {
    const selected = await open({
      directory: true,
      multiple: false,
      // defaultPath 仅在路径非空时传入（避免传空字符串触发 dialog 异常）
      defaultPath: props.modelValue?.trim() || undefined,
      title: props.dialogTitle,
    });
    // open 在用户取消时返回 null
    if (typeof selected === "string" && selected.length > 0) {
      emit("update:modelValue", selected);
      emit("change", selected);
    }
  } catch (e) {
    toast.error("选择文件夹失败", e);
  }
}

/** 输入框手动输入时同步到父组件 */
function onInputUpdate(v: string) {
  emit("update:modelValue", v ?? "");
}
</script>

<template>
  <div class="folder-picker">
    <NInput
      :value="modelValue"
      :placeholder="placeholder"
      :status="status"
      :clearable="clearable"
      :disabled="disabled"
      :size="size"
      class="picker-input"
      @update:value="onInputUpdate"
    />
    <NButton
      :disabled="disabled"
      :size="size"
      class="picker-btn"
      @click="pickFolder"
    >
      <NIcon class="picker-icon">
        <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true">
          <path
            fill="currentColor"
            d="M10 4H4c-1.11 0-2 .89-2 2v12a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2V8c0-1.11-.89-2-2-2h-8l-2-2z"
          />
        </svg>
      </NIcon>
      <span class="picker-text">{{ buttonText }}</span>
    </NButton>
  </div>
</template>

<style scoped>
.folder-picker {
  display: flex;
  gap: 8px;
  align-items: stretch;
  width: 100%;
}

.picker-input {
  flex: 1 1 auto;
  min-width: 0;
}

.picker-btn {
  flex-shrink: 0;
  display: inline-flex;
  align-items: center;
  gap: 4px;
}

.picker-icon {
  display: inline-flex;
  align-items: center;
}

.picker-text {
  line-height: 1;
}
</style>
