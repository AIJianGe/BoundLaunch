/**
 * ModelPathManager 模块 API
 *
 * 对应后端 `commands/model_path.rs`
 * 详见 `PR/03-模块设计/05-ModelPathManager.md`
 */

import { invoke } from "./index";
import type { GenerateYamlResult, ScanResult, SubdirInfo, ModelFile } from "./types";

/**
 * 生成 extra_model_paths.yaml（仅 custom_root 模式生效）
 *
 * 流程：validate_root → 渲染 yaml → 备份用户 yaml → 原子写入
 */
export function modelpathGenerate(): Promise<GenerateYamlResult> {
  return invoke<GenerateYamlResult>("modelpath_generate");
}

/**
 * 删除 launcher 生成的 yaml（幂等）
 *
 * - yaml 不存在 → Ok
 * - launcher 生成的 → 删除
 * - 用户手动 yaml → 跳过（不删除）
 */
export function modelpathRemove(): Promise<void> {
  return invoke<void>("modelpath_remove");
}

/**
 * 扫描根目录下所有 ComfyUI 子目录
 *
 * @param root 根目录路径
 * @param force 是否强制刷新缓存（默认 false 用 60s TTL 缓存）
 */
export function modelpathScan(root: string, force = false): Promise<ScanResult> {
  return invoke<ScanResult>("modelpath_scan", { root, force });
}

/**
 * 校验根目录合法性
 *
 * @param path 待校验路径
 * @throws `ApiError` 若路径不存在 / 不可读
 */
export function modelpathValidate(path: string): Promise<void> {
  return invoke<void>("modelpath_validate", { path });
}

// 重新导出类型，方便调用方 import
export type { GenerateYamlResult, ScanResult, SubdirInfo, ModelFile };
