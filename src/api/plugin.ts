/**
 * PluginManager 模块 API
 *
 * 对应后端 `commands/plugin_manager.rs`
 * 详见 `PR/03-模块设计/04-PluginManager.md`
 */

import { invoke } from "./index";
import type { PluginInfo } from "./types";

/** 列出所有插件（含已安装与可识别的未安装项） */
export function pluginList(): Promise<PluginInfo[]> {
  return invoke<PluginInfo[]>("plugin_list");
}

/**
 * 安装插件（git clone + requirements 安装）
 *
 * @param gitUrl Git URL（如 "https://github.com/author/comfyui-plugin"）
 */
export function pluginInstall(gitUrl: string): Promise<void> {
  return invoke<void>("plugin_install", { gitUrl });
}

/**
 * 更新插件（git pull）
 *
 * @param name 插件目录名（如 "ComfyUI-Manager"）
 */
export function pluginUpdate(name: string): Promise<void> {
  return invoke<void>("plugin_update", { name });
}

/**
 * 卸载插件（移到 .trash 目录，可恢复）
 *
 * @param name 插件目录名
 */
export function pluginUninstall(name: string): Promise<void> {
  return invoke<void>("plugin_uninstall", { name });
}

/**
 * 启用 / 禁用插件（重命名目录加 .disabled 后缀）
 *
 * @param name 插件目录名
 * @param enabled true=启用，false=禁用
 */
export function pluginToggle(name: string, enabled: boolean): Promise<void> {
  return invoke<void>("plugin_toggle", { name, enabled });
}

/**
 * 安装插件的 requirements.txt
 *
 * @param name 插件目录名
 */
export function pluginInstallRequirements(name: string): Promise<void> {
  return invoke<void>("plugin_install_requirements", { name });
}

/** 检查所有插件的更新（批量 git fetch） */
export function pluginCheckUpdates(): Promise<PluginInfo[]> {
  return invoke<PluginInfo[]>("plugin_check_updates");
}

/** 查询单个插件详情 */
export function pluginInfo(name: string): Promise<PluginInfo> {
  return invoke<PluginInfo>("plugin_info", { name });
}
