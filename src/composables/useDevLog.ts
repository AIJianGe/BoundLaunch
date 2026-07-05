/**
 * devLog - 把前端日志转发到后端终端（诊断用）
 *
 * 通过 Tauri 命令 `dev_log` 把前端日志输出到后端 tracing 通道，
 * 这样我们能在后端终端看到前端的完整执行链路（否则 Vite 不转发 console.log）。
 *
 * **仅在调试 F24 退出流程时使用**，生产环境应禁用。
 */
import { invoke } from "@/api";

/**
 * 发送前端日志到后端终端
 *
 * @param tag 日志标签（如 "[tray]" / "[useShutdown]"）
 * @param stage 阶段（"enter" / "decision" / "action_start" / "action_done" / "error"）
 * @param data 附加数据（任意可序列化对象，会被 JSON.stringify）
 */
export async function devLog(
  tag: string,
  stage: string,
  data: Record<string, unknown> = {},
): Promise<void> {
  try {
    await invoke("dev_log", {
      tag,
      stage,
      data: JSON.stringify(data),
    });
  } catch (e) {
    // dev_log 失败时静默（不要影响主流程）
    // eslint-disable-next-line no-console
    console.warn("[devLog] invoke failed:", e);
  }
}
