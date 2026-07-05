/**
 * ProcessLauncher 模块 API
 *
 * 对应后端 `commands/process_launcher.rs`
 * 详见 `PR/03-模块设计/06-ProcessLauncher.md §3 接口签名`
 *
 * 事件（前端 listen）：
 * - `process_starting`：状态置为 Starting 时 emit
 * - `process_started`：健康检查通过，状态置 Running 时 emit
 * - `process_stopping`：状态置 Stopping 时 emit
 * - `process_stopped`：进程退出后 emit
 * - `stale_process_detected`：启动器启动时检测到遗留 ComfyUI 进程
 * - `log`：实时日志行（来自 LogPipeline 推送）
 */

import { invoke } from "./index";
import type { ProcessStatus, ShutdownReason, ShutdownReport } from "./types";

/**
 * 启动 ComfyUI 进程
 *
 * 参数从 ConfigService 读取最新配置构造，前端无需传参。
 * 调用后立即返回（不等 ComfyUI 启动完成）。
 *
 * @throws `ApiError` 可能的错误：
 *   - "已有进程在运行" (AlreadyRunning)
 *   - "端口 X 已被占用" (PortInUse)
 *   - "环境未就绪" (EnvironmentNotReady)
 *   - "环境脏状态" (DirtyState)
 */
export function processStart(): Promise<void> {
  return invoke<void>("process_start");
}

/**
 * 停止 ComfyUI 进程（幂等）
 *
 * 未运行时直接返回 Ok。
 * 停止流程：POST /interrupt → SIGTERM(5s) → SIGKILL(2s)
 */
export function processStop(): Promise<void> {
  return invoke<void>("process_stop");
}

/**
 * 查询当前进程状态
 *
 * 内部会触发 refresh_status 检测自然退出（非阻塞）。
 */
export function processStatus(): Promise<ProcessStatus> {
  return invoke<ProcessStatus>("process_status");
}

/**
 * 读取最近 N 行日志
 *
 * 从环形缓冲读取（默认容量 5000 行）。
 * 若进程未启动或缓冲为空，返回空数组。
 *
 * @param lines 读取行数（建议 100-500）
 */
export function processTailLog(lines: number): Promise<string[]> {
  return invoke<string[]>("process_tail_log", { lines });
}

/**
 * 强制杀死遗留的 ComfyUI 进程
 *
 * 用户在前端确认 "检测到遗留进程" 提示后调用。
 *
 * @param pid 遗留进程 PID（来自 `stale_process_detected` 事件载荷）
 */
export function processKillStale(pid: number): Promise<void> {
  return invoke<void>("process_kill_stale", { pid });
}

/**
 * F24 退出流程：联动关闭 ComfyUI + 资源释放 + app.exit
 *
 * 由前端在弹确认对话框后调用。`ShutdownCoordinator` 内部 5 步事务：
 * 1. CAS 防重入（多次调用仅首次执行）
 * 2. 广播 `app_exiting` 事件
 * 3. 调用 `process_launcher.stop_with_reason(StopReason::Shutdown)` 走进程组终止
 * 4. 资源释放（500ms）
 * 5. 广播 `app_exited` + `app.exit(0)`
 *
 * 30s 总超时兜底：超时时 `std::process::exit(0)` 强制退出（不返回）。
 *
 * 事件（前端 listen）：
 * - `app_exiting`：订阅者清理本地缓存 + 拒绝新操作
 * - `app_exited`：资源清理完毕，即将 `app.exit(0)`
 *
 * @param reason 退出原因（[window_close / tray_quit / shortcut_ctrl_q / restart]）
 * @returns `ShutdownReport` 含 ComfyUI 运行时状态 + 实际停止耗时
 * @throws `ApiError` 当 `ShutdownCoordinator` 因 30s 超时已强制退出时
 */
export function shutdownAll(reason: ShutdownReason): Promise<ShutdownReport> {
  return invoke<ShutdownReport>("shutdown_all", { reason });
}
