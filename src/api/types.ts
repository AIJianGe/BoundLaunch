/**
 * 共享类型定义
 *
 * 与后端 `src-tauri/src/` 中的 Rust 类型一一对应（serde 序列化）。
 * 命名约定：前端 TypeScript 类型名与后端 Rust 类型名一致（PascalCase）。
 *
 * serde 配置：
 * - 枚举使用 `#[serde(rename_all = "snake_case")]` → TypeScript 字符串字面量联合类型
 * - `ProcessStatus` / `TaskStatus` 等带数据的枚举使用 `#[serde(tag = "kind")]` → TypeScript discriminated union
 *
 * 详见各模块设计文档 `PR/03-模块设计/` §4 数据模型
 */

// ============================================================================
// Config 模块（对应 src-tauri/src/config/models.rs）
// ============================================================================

export type LaunchMode = "cpu" | "gpu_high" | "gpu_low" | "gpu_no_vram" | "custom";
export type PreviewMethod = "latent" | "latent_upscale" | "autoencoder" | "none";
export type CudaVersion = "cpu" | "cu118" | "cu121" | "cu124";
export type ModelsMode = "default" | "custom_root" | "advanced";
export type Theme = "light" | "dark" | "auto";

/** 高级启动参数 */
export interface AdvancedArgs {
  use_split_cross_attention: boolean;
  use_pytorch_cross_attention: boolean;
  force_fp32: boolean;
  fp16_vae: boolean;
  bf16_vae: boolean;
  no_half: boolean;
  no_half_vae: boolean;
  directml: boolean;
}

export interface PathsConfig {
  comfyui_root: string;
  venv_path: string;
  python_version: string;
}

export interface LaunchConfig {
  mode: LaunchMode;
  listen_host: string;
  listen_port: number;
  auto_open_browser: boolean;
  preview_method: PreviewMethod;
  custom_args: string;
  advanced: AdvancedArgs;
}

export interface TorchConfig {
  cuda_version: CudaVersion;
}

export interface ModelsConfig {
  mode: ModelsMode;
  custom_root: string;
  advanced: Record<string, never>; // 高级配置预留
}

export interface UiConfig {
  theme: Theme;
  language: string;
  auto_check_update: boolean;
  minimize_to_tray: boolean;
}

/** 顶层 Config */
export interface Config {
  paths: PathsConfig;
  launch: LaunchConfig;
  torch: TorchConfig;
  models: ModelsConfig;
  ui: UiConfig;
  schema_version: number;
}

/** Config 部分更新（深合并语义） */
export type ConfigUpdate = Partial<{
  paths: Partial<PathsConfig>;
  launch: Partial<LaunchConfig>;
  torch: Partial<TorchConfig>;
  models: Partial<ModelsConfig>;
  ui: Partial<UiConfig>;
}>;

// ============================================================================
// ProcessLauncher 模块（对应 src-tauri/src/process_launcher/models.rs）
// ============================================================================

/** 进程启动参数 */
export interface LaunchArgs {
  mode: LaunchMode;
  listen_host: string;
  listen_port: number;
  preview_method: PreviewMethod;
  auto_launch: boolean;
  advanced: AdvancedArgs;
  custom_args: string | null;
}

/** 停止原因 */
export type StopReason =
  | "user_requested"
  | "health_check_timeout"
  | "external_signal"
  | "parent_exit";

/**
 * 进程状态机（discriminated union）
 *
 * 后端使用 `#[serde(tag = "kind", rename_all = "snake_case")]`
 */
export type ProcessStatus =
  | { kind: "stopped" }
  | { kind: "starting"; started_at: string; port: number }
  | { kind: "running"; pid: number; started_at: string; port: number }
  | { kind: "stopping"; reason: StopReason }
  | { kind: "crashed"; exit_code: number | null; error: string; at: string };

/** 健康检查结果 */
export interface HealthInfo {
  ready: boolean;
  status_code: number | null;
  elapsed_ms: number;
}

// ============================================================================
// EnvironmentInspector 模块（对应 src-tauri/src/env_inspector/models.rs）
// ============================================================================

export type DependencyStatus = "ok" | "missing" | "outdated" | "unknown";

export interface DependencyInfo {
  name: string;
  version: string | null;
  status: DependencyStatus;
}

export interface EnvInfo {
  python_path: string;
  python_version: string;
  venv_path: string;
  torch_installed: boolean;
  torch_version: string | null;
  cuda_available: boolean;
  cuda_version: string | null;
  gpu_name: string | null;
  dependencies: DependencyInfo[];
  comfyui_cloned: boolean;
  comfyui_root: string;
  last_updated: string;
}

// ============================================================================
// PythonEnvManager 模块（对应 src-tauri/src/python_env/models.rs）
// ============================================================================

export interface PythonEnvStatus {
  uv_installed: boolean;
  uv_path: string | null;
  uv_version: string | null;
  venv_exists: boolean;
  venv_python_version: string | null;
  venv_torch_installed: boolean;
  venv_torch_version: string | null;
  venv_torch_cuda: boolean;
}

export interface CompatibilityResult {
  torch_compatible: boolean;
  cuda_compatible: boolean;
  recommendations: string[];
}

// ============================================================================
// CoreManager 模块（对应 src-tauri/src/core_manager/models.rs）
// ============================================================================

export type CloneStatus = "not_cloned" | "cloning" | "cloned" | "failed";

export interface CoreStatus {
  is_cloned: boolean;
  current_version: string | null;
  latest_version: string | null;
  has_updates: boolean;
  clone_status: CloneStatus;
}

export interface GitTag {
  name: string;
  is_version: boolean;
}

// ============================================================================
// ModelPathManager 模块（对应 src-tauri/src/model_path/models.rs）
// ============================================================================

export interface GenerateYamlResult {
  yaml_path: string;
  backed_up: string | null;
  generated_at: string;
}

export interface ModelFile {
  name: string;
  size: number;
  modified: string;
}

export interface SubdirInfo {
  name: string;
  path: string;
  file_count: number;
  total_size: number;
  models: ModelFile[];
}

export interface ScanResult {
  root: string;
  subdirs: SubdirInfo[];
  scanned_at: string;
}

// ============================================================================
// PluginManager 模块（对应 src-tauri/src/plugin_manager/models.rs）
// ============================================================================

export type PluginStatus = "enabled" | "disabled" | "updating" | "error";

export interface PluginInfo {
  name: string;
  git_url: string;
  enabled: boolean;
  installed: boolean;
  current_commit: string | null;
  latest_commit: string | null;
  has_updates: boolean;
  last_updated: string | null;
}

// ============================================================================
// TaskScheduler 模块（对应 src-tauri/src/task_scheduler/models.rs）
// ============================================================================

export type TaskKind =
  | "clone_repo"
  | "fetch_tags"
  | "checkout"
  | "install_torch"
  | "install_requirements"
  | "plugin_install"
  | "plugin_update"
  | "scan_models"
  | "custom";

export type TaskPriority = "high" | "normal" | "low";

export type TaskStatus =
  | { phase: "queued" }
  | { phase: "running"; progress: number }
  | { phase: "completed" }
  | { phase: "failed"; error: string }
  | { phase: "cancelled" };

export interface TaskInfo {
  id: string;
  kind: TaskKind;
  name: string;
  priority: TaskPriority;
  status: TaskStatus;
  started_at: string | null;
  completed_at: string | null;
}

// ============================================================================
// LogStore 模块（对应 src-tauri/src/log_store/repository.rs）
// ============================================================================

export type LogLevel = "trace" | "debug" | "info" | "warn" | "error";

export interface LogEntry {
  id: number;
  timestamp: string;
  level: LogLevel;
  source: string;
  message: string;
}

export interface LogQueryOptions {
  level?: LogLevel;
  source?: string;
  keyword?: string;
  start_time?: string;
  end_time?: string;
  limit?: number;
  offset?: number;
}

export interface TaskHistoryRecord {
  id: number;
  kind: string;
  name: string;
  status: string;
  started_at: string;
  completed_at: string | null;
  exit_code: number | null;
  error: string | null;
}
