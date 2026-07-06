/**
 * CoreManager 模块 API
 *
 * 对应后端 `commands/core_manager.rs`
 * 详见 `PR/03-模块设计/03-CoreManager.md`
 */

import { invoke } from "./index";
import type {
  BackupInfo,
  CheckoutResult,
  ClassifiedTags,
  CoreStatus,
  SwitchPrerequisites,
  SwitchRepoResult,
  TagInfo,
} from "./types";

/** 是否已克隆 */
export function coreIsCloned(): Promise<boolean> {
  return invoke<boolean>("core_is_cloned");
}

/** 状态总览（当前版本 / 是否有更新 / 克隆状态） */
export function coreStatus(): Promise<CoreStatus> {
  return invoke<CoreStatus>("core_status");
}

/**
 * 克隆 ComfyUI 仓库
 *
 * @param repoUrl 仓库 URL（默认 https://github.com/comfyanonymous/ComfyUI.git）
 */
export function coreClone(repoUrl?: string): Promise<void> {
  return invoke<void>("core_clone", repoUrl ? { repoUrl } : undefined);
}

/**
 * 确保 ComfyUI 仓库已克隆
 *
 * 行为：
 * - 若 `comfyui_root/.git` 已存在 → 直接返回
 * - 若目录不存在或为空 → 自动 clone 默认仓库
 * - 若目录非空且无 `.git` → 返回错误（前端提示用户处理）
 *
 * 用法：OnboardingPage / LaunchPage 在需要 ComfyUI 源码时调用。
 */
export function coreEnsureCloned(): Promise<void> {
  return invoke<void>("core_ensure_cloned");
}

/**
 * 列出所有 tag（v3.1 / F26 返回 TagInfo[]，包含 commit / date / is_stable）
 *
 * @param force 是否强制刷新（跳过内存缓存）；默认 false（用缓存）
 */
export function coreListTags(force: boolean = false): Promise<TagInfo[]> {
  return invoke<TagInfo[]>("core_list_tags", { force });
}

/**
 * 列出所有 tag 并按 SemVer 分类（v3.1 / F26 决策 7：NTab 双分类）
 *
 * 返回 ClassifiedTags：
 * - stable：稳定版（严格 vX.Y.Z）
 * - prerelease：预发布版（vX.Y.Z-rc1 / -beta 等）
 *
 * @param force 是否强制刷新（跳过内存缓存）；默认 false（用缓存）
 */
export function coreListTagsClassified(
  force: boolean = false,
): Promise<ClassifiedTags> {
  return invoke<ClassifiedTags>("core_list_tags_classified", { force });
}

/**
 * 检查切换版本的前置条件（v3.5 异步化）
 *
 * v3.5 改造：原同步命令可能因 git status 阻塞，改为提交 `CheckPrereq` 任务并立即返回 task_id。
 * 前端通过 `useTaskProgress` 跟踪进度，从 `task_completed.payload` 拿 `SwitchPrerequisites`。
 *
 * 前端调用此命令判断是否允许切换：
 * - ComfyUI 运行中 → 拒绝
 * - 工作区有未提交改动 → 拒绝
 *
 * @returns task_id
 */
export function coreCheckSwitchPrerequisites(): Promise<string> {
  return invoke<string>("core_check_switch_prerequisites");
}

/**
 * 切换 ComfyUI 版本（v3.1 / F26 决策 1-12 完整实现）
 *
 * 行为（11 步流程 + 全部回滚）：
 * 1. 前置检查：ComfyUI 已停止 + 工作区干净
 * 2. 记录当前 tag（用于回滚）
 * 3. 解除 models 软链接（避免 git checkout 冲突）
 * 4. fetch 远程 tag（决策 10：本地优先）
 * 5. 检查目标 tag 是否存在
 * 6. checkout 到目标 tag
 * 7. 重新建立 models 软链接
 * 8. 删除旧 venv（决策 3：总是重建）
 * 9. 创建新 venv
 * 10. 安装 torch + requirements
 * 11. 验证 venv 完整性
 *
 * 失败时全部回滚（决策 6）：force_checkout 回原 tag + 恢复 models 链接。
 *
 * @param targetTag 目标 tag（如 "v0.3.10"）
 * @returns task_id，前端通过 listen('task_progress'/'task_completed') 接收进度
 */
export function coreSwitchVersion(targetTag: string): Promise<string> {
  return invoke<string>("core_switch_version", { targetTag });
}

/**
 * 确保 models 软链接已建立（v3.1 / F26 决策 12）
 *
 * 在以下场景调用：
 * - 用户在设置页配置/修改 models_path 后
 * - ComfyUI 启动前（ProcessLauncher 内部调用）
 * - 切换版本任务步骤 7（switcher 内部调用）
 *
 * @returns true 表示链接已建立；false 表示无需链接（models_path 未配置）
 */
export function coreEnsureModelsLink(): Promise<boolean> {
  return invoke<boolean>("core_ensure_models_link");
}

/**
 * 切换到指定 tag（旧版轻量切换，仅 git checkout，不重建 venv）
 *
 * 注意：v3.1 / F26 后推荐使用 `coreSwitchVersion` 进行完整切换。
 * 本命令保留用于调试 / 紧急场景。
 *
 * @param tag Git 引用（如 "v0.6.0"）
 */
export function coreCheckout(tag: string): Promise<CheckoutResult> {
  return invoke<CheckoutResult>("core_checkout", { tag });
}

/**
 * F35-D：在系统文件管理器中打开 ComfyUI 仓库目录
 *
 * 用途：工作区脏时，让用户手动执行 `git stash` / `git clean`
 *
 * 后端：commands/core_manager.rs::core_open_comfyui_dir
 * 跨平台：Windows explorer.exe / macOS open / Linux xdg-open
 */
export function coreOpenComfyuiDir(): Promise<void> {
  return invoke<void>("core_open_comfyui_dir");
}

/** 拉取最新代码（git pull） */
export function coreUpdate(): Promise<string> {
  return invoke<string>("core_update");
}

// ============================================================================
// F31：ComfyUI 仓库地址切换与备份恢复
// ============================================================================

/** 获取当前仓库 URL（脱敏后的，F31） */
export function coreGetRepoUrl(): Promise<string> {
  return invoke<string>("core_get_repo_url");
}

/** 获取官方仓库 URL（F31） */
export function coreOfficialRepoUrl(): Promise<string> {
  return invoke<string>("core_official_repo_url");
}

/** 列出所有备份（F31） */
export function coreListBackups(): Promise<BackupInfo[]> {
  return invoke<BackupInfo[]>("core_list_backups");
}

/** 切换仓库地址（F31） */
export function coreSetRepoUrl(
  url: string,
  migrateCustomNodes: boolean,
): Promise<SwitchRepoResult> {
  return invoke<SwitchRepoResult>("core_set_repo_url", {
    url,
    migrateCustomNodes,
  });
}

/** 恢复备份（F31） */
export function coreRestoreBackup(backupName: string): Promise<SwitchRepoResult> {
  return invoke<SwitchRepoResult>("core_restore_backup", { backupName });
}

// ========== F35-A+：工作区脏原因检查 + 一键清理 ==========

/** 工作区脏的原因（v1.8 / F35-A+） */
export type WorkspaceDirtyReason =
  | { kind: "staged"; count: number; files: string[] }
  | { kind: "unstaged"; count: number; files: string[] }
  | { kind: "untracked"; count: number; files: string[] };

/** F35-A+：获取工作区脏的详细原因（staged / unstaged / untracked） */
export function coreWorkspaceDirtyReason(): Promise<WorkspaceDirtyReason | null> {
  return invoke<WorkspaceDirtyReason | null>("core_workspace_dirty_reason");
}

/** F35-A+：撤销 staging（`git reset HEAD`），不修改 working tree 内容 */
export function coreResetStaged(): Promise<void> {
  return invoke<void>("core_reset_staged");
}

/** F35-A+：强制清理整个工作区（`git checkout .` + `git clean -fd`）— ⚠️ 不可恢复 */
export function coreForceCleanWorkspace(): Promise<void> {
  return invoke<void>("core_force_clean_workspace");
}

// ========== F36：版本切换模式（Clean/Preserve/Skip）==========

/** 切换模式 */
export type SwitchMode = "Clean" | "Preserve" | "Skip";

/** requirements.txt 差异（v1.8 / F36） */
export type RequirementsDiff =
  | { kind: "Identical" }
  | { kind: "OnlyMissing"; missingPackages: string[] }
  | { kind: "HasMajorChange"; changed: [string, string, string][] }; // (name, old, new)

/** 版本兼容性报告（v1.8 / F36） */
export interface VersionCompatReport {
  currentTag: string | null;
  targetTag: string;
  venvExists: boolean;
  currentPython: string | null;
  targetPython: string | null;
  currentTorchVariant: string | null;
  targetTorchVariant: string | null;
  currentTorchInstalled: boolean;
  samePython: boolean;
  sameTorchVariant: boolean;
  requirementsDiff: RequirementsDiff;
  customNodeCount: number;
  recommendedMode: SwitchMode;
  recommendedReason: string;
}

/** F36：切版本前调用的兼容性预检（v3.5 异步化） */
export function coreCheckVersionCompatibility(
  targetTag: string
): Promise<string> {
  return invoke<string>("core_check_version_compatibility", {
    targetTag,
  });
}

/** F36：带 mode 参数切换 ComfyUI 版本 */
export function coreSwitchVersionWithMode(
  targetTag: string,
  mode: SwitchMode
): Promise<string> {
  return invoke<string>("core_switch_version", { targetTag, mode });
}

/** F36：打开 ComfyUI 目录（已存在）— 见 coreOpenComfyuiDir */
export function configValidateVenvPath(): Promise<string | null> {
  return invoke<string | null>("config_validate_venv_path");
}
