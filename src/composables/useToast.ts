/**
 * useToast - 统一 Toast 调用
 *
 * 设计模式：
 * - **Facade**：封装 Naive UI `useMessage()`，提供更简洁的语义化接口
 * - **Adapter**：将 ApiError 转换为友好提示文案
 * - **Observer (v3.10)**：error / warn 自动入 LogStore，弹窗消失≠日志丢失
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
 *
 * **v3.10 自动入档**：
 * - `toast.error` / `toast.warn` 自动 invoke `log_append` 写入 LogStore
 * - 业务代码 0 改动即可覆盖 20+ 个错误源
 * - 后端 `LogStoreService::log_business_error` 内部已经 spawn 异步写库 + emit business_log 事件
 * - 写库失败不影响 toast 显示
 */

import { useMessage, useNotification } from "naive-ui";
import { ApiError } from "@/api";
import { logAppend } from "@/api/log";
import type { LogLevel } from "@/api/types";

export interface ToastOptions {
  /** 显示时长（毫秒），默认 3000 */
  duration?: number;
  /** 是否可关闭，默认 true */
  closable?: boolean;
  /**
   * v3.10：日志来源标识
   *
   * 业务代码可通过 options.source 显式覆盖默认值。
   * 默认自动从调用栈推断（best-effort，详见 inferSource()）。
   */
  source?: string;
}

/**
 * v3.10：推断调用方来源（best-effort）
 *
 * 解析调用栈第二层（跳过 useToast.error / error itself / 业务函数）。
 * 提取模块名（如 "useEnvInstaller" / "PathsPanel" / "useStartComfyui"）。
 *
 * 失败时返回 "ui:unknown"，不阻塞主流程。
 */
function inferSource(): string {
  try {
    const stack = new Error().stack;
    if (!stack) return "ui:unknown";

    // 跳过前 3 行（Error / inferSource / 调用方包装）
    const lines = stack.split("\n").slice(3, 8);
    for (const line of lines) {
      // 提取 "at FunctionName (file:path:col:col)" 或 "at file:path:col:col"
      const match =
        line.match(/at (\w+)\s+\(/) || line.match(/at\s+(?:.*?\/)?([^/\\?]+):\d+:\d+/);
      if (match) {
        const name = match[1];
        // 过滤：跳过 Vue 内部 / 框架层
        if (
          name &&
          !name.startsWith("_") &&
          !["Promise", "async", "Object", "<computed>"].includes(name)
        ) {
          return name;
        }
      }
    }
    return "ui:unknown";
  } catch {
    return "ui:unknown";
  }
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
   *
   * v3.10：info 不入档（避免大量无意义日志）
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
   *
   * v3.10：**自动入档** warn 级，0 业务代码改动
   */
  function warn(content: string, options?: ToastOptions) {
    message.warning(content, {
      duration: options?.duration ?? 5000,
      closable: options?.closable ?? true,
    });

    // 自动入档（异步，不阻塞）
    logAppend({
      level: "warn" as LogLevel,
      source: options?.source ?? inferSource(),
      message: content,
      detail: null,
    });
  }

  /**
   * 错误提示（红色，长时长）
   *
   * 同时支持传入 Error 对象（如 ApiError），自动提取 message。
   *
   * @param content 错误标题
   * @param err 错误对象（可选）
   *
   * v3.10：**自动入档** error 级，0 业务代码改动
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

    // v3.10：自动入档 LogStore（关键！解决"弹窗消失=日志丢失"）
    logAppend({
      level: "error" as LogLevel,
      source: options?.source ?? inferSource(),
      message: content,
      detail: detail || null,
    });
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
