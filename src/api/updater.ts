/**
 * 自动更新 API 封装
 *
 * 与后端 `src-tauri/src/commands/updater.rs` 一一对应
 * 详见 `src-tauri/src/updater/mod.rs` 模块文档
 *
 * ## 用法
 *
 * ```ts
 * import { updater } from "@/api/updater";
 * import { listen } from "@/api";
 *
 * // 1. 检查更新
 * const info = await updater.check();
 * if (info.has_update) {
 *   console.log(`新版本 ${info.latest_version} 可用`);
 * }
 *
 * // 2. 监听进度
 * const unlisten = await listen<UpdateProgress>("update_progress", (e) => {
 *   console.log(`${e.payload.percent}%`);
 * });
 *
 * // 3. 下载 + 应用
 * await updater.download(info);
 *
 * // 4. 重启
 * await updater.applyAndRestart();
 *
 * // 5. 清理监听
 * unlisten();
 * ```
 */

import { invoke } from "./index";

/** UpdateInfo - 与后端 `crate::updater::manifest::UpdateInfo` 一致 */
export interface UpdateInfo {
  has_update: boolean;
  current_version: string;
  latest_version: string;
  release_name: string;
  release_notes: string;
  release_url: string;
  download_url: string;
  zip_size: number;
  sha256: string | null;
  published_at: string;
}

/** ApplyResult - 与后端 `crate::updater::apply::ApplyResult` 一致 */
export interface ApplyResult {
  exe_pending: string | null;
  dll_pending: string | null;
  uv_pending: string | null;
}

/** 进度阶段 */
export type ProgressPhase = "download" | "verify" | "extract";

/** UpdateProgress - 与后端 `crate::updater::download::UpdateProgress` 一致 */
export interface UpdateProgress {
  phase: ProgressPhase;
  percent: number;
  bytes_done: number;
  bytes_total: number;
  speed_bps: number;
  eta_seconds: number;
}

/**
 * 工具函数：把字节数格式化为可读字符串（如 "30.5 MB"）
 */
export function formatBytes(bytes: number): string {
  if (bytes === 0) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  const k = 1024;
  const i = Math.min(Math.floor(Math.log(bytes) / Math.log(k)), units.length - 1);
  return `${(bytes / Math.pow(k, i)).toFixed(2)} ${units[i]}`;
}

/**
 * 工具函数：把下载速度格式化为可读字符串（如 "1.5 MB/s"）
 */
export function formatSpeed(bps: number): string {
  if (bps === 0) return "—";
  return `${formatBytes(bps)}/s`;
}

/**
 * 工具函数：把预计剩余时间格式化为可读字符串（如 "30s" / "2m 15s"）
 */
export function formatEta(seconds: number): string {
  if (seconds === 0) return "—";
  if (seconds < 60) return `${seconds}s`;
  const m = Math.floor(seconds / 60);
  const s = seconds % 60;
  return s === 0 ? `${m}m` : `${m}m ${s}s`;
}

/** updater 命令集合 */
export const updater = {
  /** 检查更新（调 GitHub API） */
  async check(): Promise<UpdateInfo> {
    return invoke<UpdateInfo>("updater_check");
  },

  /** 下载 + 解压 + 应用（白名单替换 + 准备 .new） */
  async download(info: UpdateInfo): Promise<ApplyResult> {
    return invoke<ApplyResult>("updater_download", { info });
  },

  /** 退出当前进程并启动新版本 */
  async applyAndRestart(): Promise<void> {
    return invoke<void>("updater_apply_and_restart");
  },
};
