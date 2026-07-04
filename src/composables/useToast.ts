/**
 * useToast - 统一 Toast 调用
 *
 * 设计模式：
 * - **Facade**：封装 Naive UI `useMessage()`，提供更简洁的语义化接口
 * - **Adapter**：将 ApiError 转换为友好提示文案
 *
 * 使用方式：
 * ```ts
 * const toast = useToast();
 * toast.success("保存成功");
 * toast.error("启动失败", err);
 * toast.warn("端口被占用");
 * ```
 *
 * 注意：必须在 NMessageProvider 内部使用（App.vue 已配置）。
 */

import { useMessage, useNotification } from "naive-ui";
import { ApiError } from "@/api";

export interface ToastOptions {
  /** 显示时长（毫秒），默认 3000 */
  duration?: number;
  /** 是否可关闭，默认 true */
  closable?: boolean;
}

export function useToast() {
  const message = useMessage();
  const notification = useNotification();

  /**
   * 成功提示（绿色，短时长）
   * 用于：保存成功 / 操作完成
   */
  function success(content: string, options?: ToastOptions) {
    message.success(content, {
      duration: options?.duration ?? 3000,
      closable: options?.closable ?? true,
    });
  }

  /**
   * 普通信息提示（蓝色）
   * 用于：进度提示 / 状态变更
   */
  function info(content: string, options?: ToastOptions) {
    message.info(content, {
      duration: options?.duration ?? 3000,
      closable: options?.closable ?? true,
    });
  }

  /**
   * 警告提示（黄色，较长时长）
   * 用于：端口被占用 / 环境未就绪（可恢复）
   */
  function warn(content: string, options?: ToastOptions) {
    message.warning(content, {
      duration: options?.duration ?? 5000,
      closable: options?.closable ?? true,
    });
  }

  /**
   * 错误提示（红色，长时长）
   *
   * 同时支持传入 Error 对象（如 ApiError），自动提取 message。
   *
   * @param content 错误标题
   * @param err 错误对象（可选）
   */
  function error(content: string, err?: unknown, options?: ToastOptions) {
    const detail = err instanceof Error ? err.message : err ? String(err) : "";
    const fullText = detail ? `${content}: ${detail}` : content;

    // 长错误用 notification（带详情），短错误用 message
    if (detail && detail.length > 80) {
      notification.error({
        title: content,
        content: detail,
        duration: options?.duration ?? 8000,
        closable: options?.closable ?? true,
      });
    } else {
      message.error(fullText, {
        duration: options?.duration ?? 6000,
        closable: options?.closable ?? true,
      });
    }

    // 同时打印到控制台便于调试
    if (err instanceof ApiError) {
      console.error("[ApiError]", err.raw, err);
    } else if (err) {
      console.error("[Error]", content, err);
    }
  }

  /**
   * 加载中提示（带 loading 图标）
   *
   * 返回一个 close 函数，调用后关闭。
   *
   * @example
   * ```ts
   * const close = toast.loading("正在启动 ComfyUI...");
   * try {
   *   await startComfyUI();
   *   toast.success("启动成功");
   * } finally {
   *   close();
   * }
   * ```
   */
  function loading(content: string): () => void {
    const hide = message.loading(content, {
      duration: 0, // 不自动关闭
      closable: false,
    });
    return () => hide.destroy();
  }

  return { success, info, warn, error, loading };
}
