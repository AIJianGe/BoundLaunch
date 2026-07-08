/**
 * 端口诊断 + 进程强杀 API
 *
 * 对应后端 `commands/port_diagnostics.rs`
 *
 * 设计目的：
 * - 解决"ComfyUI 启动卡在 Starting 状态"问题
 * - 当 8188 端口被占用时，给出占用方进程信息
 * - 提供"强制结束占用进程"的能力
 *
 * 使用场景：
 * 1. 启动 ComfyUI 失败（端口被占）→ 后端发 process_start_failed 事件 → 前端弹 PortConflictModal
 * 2. 用户点 PortConflictModal 的"结束该进程"→ 调 forceKillProcess
 * 3. 用户点 StartStopButtons 的"⏹ 强制停止"→ 调 forceKillAllPython
 */

import { invoke } from "./index";

/** 进程信息 */
export interface ProcessInfo {
  pid: number;
  name: string;
  /** 命令行（完整版，可能很长） */
  command: string | null;
  /** 命令行（截断版，UI 展示用） */
  command_short: string;
}

/** 端口诊断结果 */
export interface PortDiagnosis {
  port: number;
  host: string;
  available: boolean;
  /** 占用方进程信息（available=false 时才有） */
  occupied_by: ProcessInfo | null;
  /** 原始错误信息（探测过程失败时填） */
  error: string | null;
}

/** 强杀结果 */
export interface KillResult {
  killed_pids: number[];
  failed: KillFailure[];
}

export interface KillFailure {
  pid: number;
  reason: string;
}

/**
 * 诊断端口占用情况
 *
 * 1. 尝试 bind 端口，失败则继续
 * 2. 用系统命令找占用方进程（Windows: netstat + tasklist + wmic）
 * 3. 返回结构化信息
 *
 * @param host 主机（通常 "127.0.0.1"）
 * @param port 端口
 */
export function diagnosePort(host: string, port: number): Promise<PortDiagnosis> {
  return invoke<PortDiagnosis>("diagnose_port", { host, port });
}

/**
 * 强杀单个进程（按 PID）
 *
 * 不会杀系统关键进程（PID 0/1/4 在 Windows；PID 1 在 Unix）
 *
 * @param pid 进程 ID
 */
export function forceKillProcess(pid: number): Promise<KillResult> {
  return invoke<KillResult>("force_kill_process", { pid });
}

/**
 * 强杀所有 python.exe（兜底）
 *
 * Windows: taskkill /F /IM python.exe /T
 * Unix: pkill -9 -f python
 *
 * 用于紧急情况下"清场"——杀掉所有 Python 进程
 */
export function forceKillAllPython(): Promise<KillResult> {
  return invoke<KillResult>("force_kill_all_python");
}

/**
 * 强杀所有 ComfyUI 相关进程
 *
 * 比 forceKillAllPython 更激进：杀 python.exe + comfyui.exe
 */
export function forceKillAllComfyui(): Promise<KillResult> {
  return invoke<KillResult>("force_kill_all_comfyui");
}

/** process_start_failed 事件 payload */
export interface ProcessStartFailedEvent {
  reason: "port_in_use" | "other";
  port: number;
  host: string;
  diagnosis: PortDiagnosis | null;
  error_message: string;
}
