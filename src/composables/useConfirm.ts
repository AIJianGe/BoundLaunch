/**
 * useConfirm - 确认弹窗（Promise 化）
 *
 * 设计模式：
 * - **Adapter**：将 Naive UI `useDialog()` 回调式 API 转换为 Promise，便于 async/await
 *
 * 使用方式：
 * ```ts
 * const confirm = useConfirm();
 * if (await confirm({ title: "确认卸载？", content: "插件将移到 .trash 目录" })) {
 *   await uninstall();
 *   toast.success("已卸载");
 * }
 * ```
 *
 * 注意：必须在 NDialogProvider 内部使用（App.vue 已配置）。
 */

import { useDialog } from "naive-ui";

export interface ConfirmOptions {
  /** 标题（粗体） */
  title: string;
  /** 正文内容 */
  content?: string;
  /** 确认按钮文字（默认 "确认"） */
  positiveText?: string;
  /** 取消按钮文字（默认 "取消"） */
  negativeText?: string;
  /** 类型（影响图标与颜色），默认 "warning" */
  type?: "info" | "success" | "warning" | "error";
  /** 是否遮罩点击关闭（默认 false，强制用户点按钮） */
  maskClosable?: boolean;
}

export function useConfirm() {
  const dialog = useDialog();

  /**
   * 显示确认弹窗
   *
   * @returns true=用户点击确认 / false=用户点击取消或关闭弹窗
   */
  function confirm(options: ConfirmOptions): Promise<boolean> {
    return new Promise((resolve) => {
      dialog[options.type ?? "warning"]({
        title: options.title,
        content: options.content,
        positiveText: options.positiveText ?? "确认",
        negativeText: options.negativeText ?? "取消",
        maskClosable: options.maskClosable ?? false,
        onPositiveClick: () => resolve(true),
        onNegativeClick: () => resolve(false),
        onMaskClick: options.maskClosable ? () => resolve(false) : undefined,
        onClose: () => resolve(false),
      });
    });
  }

  /**
   * 显示警告确认弹窗（默认 type="warning" 的快捷方法）
   */
  function warn(title: string, content?: string): Promise<boolean> {
    return confirm({ title, content, type: "warning" });
  }

  /**
   * 显示危险操作确认弹窗（type="error"）
   * 用于：卸载插件 / 强杀进程 / 清空日志等不可逆操作
   */
  function danger(title: string, content?: string): Promise<boolean> {
    return confirm({
      title,
      content,
      type: "error",
      positiveText: "确认执行",
      negativeText: "取消",
    });
  }

  return { confirm, warn, danger };
}
