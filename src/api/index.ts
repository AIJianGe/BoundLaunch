/**
 * Tauri invoke 统一封装
 *
 * 设计模式：
 * - **Repository**：抽象后端调用，前端不直接接触 Tauri API
 * - **Adapter**：将 Tauri 的 `Promise<T | string>` 转换为强类型 `Promise<T>` 或抛出 `ApiError`
 *
 * 设计要点：
 * - 后端 `#[tauri::command]` 返回 `Result<T, String>`，Tauri 自动转为 `Promise<T>` reject
 * - 本封装将 reject 的 string 包装为 `ApiError` 实例，保留原始错误字符串便于前端展示
 * - 类型安全：每个领域 API 文件（config.ts / process.ts 等）显式声明返回类型
 *
 * 详见 `PR/03-模块设计/` 各模块 §3 接口签名
 */

import { invoke as tauriInvoke } from "@tauri-apps/api/core";

/**
 * API 错误（后端返回的 Err(String)）
 *
 * 后端 `AppError` 实现 `Serialize` 为字符串，前端拿到的就是字符串。
 * 保留 `raw` 字段以便调试或匹配特定错误前缀（如 "已有进程在运行"）。
 */
export class ApiError extends Error {
  /** 原始错误字符串（来自后端 `AppError::to_string()`） */
  readonly raw: string;

  constructor(message: string) {
    super(message);
    this.name = "ApiError";
    this.raw = message;
  }

  /**
   * 判断错误是否包含指定子串（用于按错误类型显示不同 UI）
   *
   * @example
   * ```ts
   * if (err.matches("已有进程在运行")) { ... }
   * if (err.matches("PortInUse")) { ... }
   * ```
   */
  matches(substring: string): boolean {
    return this.raw.includes(substring);
  }
}

/**
 * 调用 Tauri command
 *
 * @param cmd 后端命令名（如 "process_start"）
 * @param args 参数对象（自动 camelCase → snake_case 由 Tauri 处理）
 * @returns 后端返回的数据
 * @throws {ApiError} 后端返回 Err 时抛出
 *
 * @example
 * ```ts
 * const status = await invoke<ProcessStatus>("process_status");
 * ```
 */
export async function invoke<T>(
  cmd: string,
  args?: Record<string, unknown>,
): Promise<T> {
  try {
    return await tauriInvoke<T>(cmd, args);
  } catch (e) {
    // Tauri 将 Err(String) 转为 reject(string)
    const message = typeof e === "string" ? e : String(e);
    throw new ApiError(message);
  }
}

/**
 * Tauri event 监听（带类型）
 *
 * 设计模式：Observer - 前端订阅后端 emit 的事件
 *
 * @param event 事件名（如 "process_started"）
 * @param handler 处理函数（接收 event.payload）
 * @returns unlisten 函数（组件卸载时调用）
 *
 * @example
 * ```ts
 * onMounted(async () => {
 *   const unlisten = await listen<ProcessStatus>("process_started", (e) => {
 *     console.log("进程已启动", e.payload);
 *   });
 *   onUnmounted(() => unlisten());
 * });
 * ```
 */
export async function listen<T>(
  event: string,
  handler: (payload: { payload: T }) => void,
): Promise<UnlistenFn> {
  const { listen: tauriListen } = await import("@tauri-apps/api/event");
  return tauriListen<T>(event, handler);
}

/** unlisten 函数类型（来自 Tauri event） */
export type UnlistenFn = () => void;
