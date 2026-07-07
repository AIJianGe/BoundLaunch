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
export type CudaVersion = "cpu" | "cu118" | "cu126" | "cu128" | "cu130";
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
  /**
   * 自定义 models 路径（v3.1 / F26 决策 12）
   *
   * - `null` / `undefined`：使用 ComfyUI 默认 `<comfyui_root>/models`
   * - 字符串：建立 `<comfyui_root>/models` 软链接到此路径
   *
   * 后端通过 `core_ensure_models_link` 命令建立 / 维护链接。
   */
  models_path?: string | null;
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
  /** 老字段（v3.0 前），保留用于向后兼容 + 老 config 文件 */
  cuda_version: CudaVersion;
  /**
   * v3.0 新增（F25）：多厂商 torch 变体，JSON 字符串形式存储
   * （对应 `crate::python_env::TorchVariant` 的 serde 序列化）
   *
   * - `null` 或 `undefined`：未配置（启动时由前端触发 GPU 检测 + 智能推荐后写入）
   * - `"{\"vendor\":\"nvidia_cuda\",\"version\":\"cu121\"}"`：已配置
   *
   * 解析见 `parseTorchVariant()` 工具函数
   */
  torch_variant: string | null;
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
  | "parent_exit"
  | "shutdown";

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
// F24 退出流程（对应 src-tauri/src/event_bus.rs ShutdownReason +
//                src-tauri/src/process_launcher/models.rs ShutdownReport）
// ============================================================================

/**
 * 退出原因（前端与后端共用枚举值，snake_case 序列化）
 *
 * - `window_close`：主窗口 [X] 关闭按钮
 * - `tray_quit`：托盘菜单「🚪 退出」
 * - `shortcut_ctrl_q`：快捷键 Ctrl+Q
 * - `restart`：重启 launcher（v0.2.0 扩展）
 */
export type ShutdownReason =
  | "window_close"
  | "tray_quit"
  | "shortcut_ctrl_q"
  | "restart";

/**
 * F24 退出流程结果报告（ShutdownCoordinator 5 步事务完成后产出）
 *
 * 前端调用 `invoke('shutdown_all', { reason })` 时接收
 */
export interface ShutdownReport {
  /** ComfyUI 是否在退出前处于运行中 */
  comfyui_was_running: boolean;
  /** ComfyUI 停止阶段耗时（毫秒） */
  stop_elapsed_ms: number;
  /** 退出原因（与入参一致） */
  reason: ShutdownReason;
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
// Environment Readiness（启动 ComfyUI 前的就绪性检查）
// 对应后端 `env_inspector::readiness::ReadinessResult`
// ============================================================================

/** 缺失步骤（前端按顺序自动补齐） */
export type ReadinessStep =
  | { kind: "CloneComfyUI" }
  | { kind: "CreateVenv"; params: { python_version: string } }
  | { kind: "InstallTorch"; params: { cuda_version: string } }
  | { kind: "InstallRequirements" };

/** 分项检查结果 */
export interface ReadinessChecks {
  comfyui_cloned: boolean;
  venv_exists: boolean;
  uv_available: boolean;
  torch_installed: boolean;
  requirements_ok: boolean;
}

/** 就绪性检查返回值 */
export interface ReadinessCheckResult {
  ready: boolean;
  missing_steps: ReadinessStep[];
  checks: ReadinessChecks;
  /**
   * 当前生效的启动模式（来自 `config.launch.mode`）
   *
   * v3.10 新增：前端用于检测模式不匹配。
   * - `mode = "cpu"` 但 `cuda_available = true`：可提示用户切换到 GPU 模式
   * - `mode = "gpu_*"` 但 `cuda_available = false`：阻止启动并引导修复
   */
  launch_mode: LaunchMode;
  /**
   * torch 在当前 venv 中是否实际可用 CUDA
   *
   * v3.10 新增：来源 `EnvSnapshot.cuda_available`。
   * 与 `launch_mode` 配合决定启动按钮状态。
   */
  cuda_available: boolean;
}

// ============================================================================
// v3.0 依赖冲突检测（对应 src-tauri/src/env_inspector/dependency_conflict.rs）
// ============================================================================

/** 冲突严重度 */
export type ConflictSeverity = "patch" | "major" | "minor";

/** 单个包约束（来源 = 某个节点的 requirements.txt） */
export interface PackageConstraint {
  name: string;
  constraint: string;
  node_name: string;
  source_file: string;
}

/** 冲突项 */
export interface Conflict {
  name: string;
  severity: ConflictSeverity;
  constraints: PackageConstraint[];
  suggestion: string;
  affected_nodes: string[];
}

/** 完整冲突报告 */
export interface ConflictReport {
  scanned_nodes: string[];
  total_packages: number;
  unique_packages: number;
  conflicts: Conflict[];
  clean: boolean;
  scan_duration_ms: number;
}

// ============================================================================
// CoreManager 模块（对应 src-tauri/src/core_manager/models.rs）
// ============================================================================

/**
 * ComfyUI 仓库当前状态
 *
 * 与后端 `CoreStatus` 一一对应（v3.1 / F26 同步）
 */
export interface CoreStatus {
  /** 当前 HEAD 对应的 tag 名（无 tag 时为 null） */
  current_version: string | null;
  /** 当前 commit SHA */
  current_commit: string;
  /** 工作区是否有未提交改动 */
  has_local_changes: boolean;
  /** 最新稳定版 tag（list_tags 后填充） */
  latest_stable: string | null;
  /** 仓库是否已克隆 */
  is_clone_done: boolean;
}

/**
 * 单个 Git tag 信息（v3.1 / F26）
 *
 * 与后端 `TagInfo` 一一对应
 */
export interface TagInfo {
  /** tag 名（如 "v0.3.10"） */
  name: string;
  /** 是否为稳定版（严格 vX.Y.Z 格式，无 rc/beta/pre/dev 后缀） */
  is_stable: boolean;
  /** tag 指向的 commit SHA */
  commit: string;
  /** tag 创建时间（ISO8601 / RFC3339） */
  date: string;
}

/**
 * tag 分类（v3.1 / F26 决策 9：SemVer 规则 + 决策 7：NTab 双分类）
 *
 * - stable：严格 `vX.Y.Z` 格式（无后缀），按版本倒序
 * - prerelease：`vX.Y.Z-rc1` / `vX.Y.Z-beta` 等带后缀，按版本倒序
 */
export interface ClassifiedTags {
  stable: TagInfo[];
  prerelease: TagInfo[];
}

/**
 * 切换版本前置检查结果（v3.1 / F26 决策 5）
 *
 * 在调用 `coreSwitchVersion` 前由前端调用 `coreCheckSwitchPrerequisites` 获取。
 */
export interface SwitchPrerequisites {
  /** 是否允许切换 */
  can_switch: boolean;
  /** ComfyUI 是否正在运行（运行中拒绝切换） */
  comfyui_running: boolean;
  /** 工作区是否有未提交改动（脏状态拒绝切换） */
  has_local_changes: boolean;
  /** 当前 tag（用于回滚提示） */
  current_tag: string | null;
  /** 阻止原因（can_switch = false 时填充） */
  block_reason: string | null;
}

/**
 * 切换版本结果（v3.1 / F26 决策 6：全部回滚）
 *
 * 后端使用 `#[serde(tag = "kind", rename_all = "snake_case")]`
 */
export type SwitchVersionResult =
  | {
      kind: "success";
      from: string | null;
      to: string;
      /** venv 是否被重建（决策 3：总是重建） */
      venv_rebuilt: boolean;
      /** models 链接是否重建 */
      models_link_rebuilt: boolean;
      /** requirements 是否已重新安装 */
      requirements_reinstalled: boolean;
    }
  | {
      kind: "rolled_back";
      to: string;
      error: string;
      /** 回滚是否完整（git checkout 已恢复；venv 可能已损坏） */
      rollback_clean: boolean;
    };

/**
 * checkout 操作结果（对应后端 `CheckoutResult`）
 *
 * 后端使用 `#[serde(tag = "kind")]`
 */
export type CheckoutResult =
  | { kind: "Switched"; from: string | null; to: string }
  | { kind: "AlreadyOnTag"; tag: string }
  | { kind: "StashedAndSwitched"; stash_ref: string; from: string; to: string };

/**
 * @deprecated 旧类型，保留用于 core_list_tags 命令（v3.1 已被 TagInfo 取代）
 *
 * 新代码请使用 `TagInfo`。
 */
export interface GitTag {
  name: string;
  is_version: boolean;
}

// ----------------------------------------------------------------------------
// F31：ComfyUI 仓库地址切换与备份恢复
// ----------------------------------------------------------------------------

/** 备份信息（F31） */
export interface BackupInfo {
  name: string;
  path: string;
  backed_up_at: string;
  repo_url_masked: string;
  current_tag: string | null;
  current_commit: string;
  size_bytes: number;
}

/** 切换仓库地址结果（F31） */
export type SwitchRepoResult =
  | {
      kind: "success";
      from_url: string;
      to_url: string;
      backup_name: string | null;
      clone_elapsed_ms: number;
    }
  | {
      kind: "rolled_back";
      to_url: string;
      error: string;
      rollback_clean: boolean;
    };

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
  | "custom"
  // F32 新增：4 个环境长任务
  | "create_venv"
  | "switch_torch_variant"
  | "rebuild_venv"
  | "switch_python"
  // v3.4 新增：启动 ComfyUI 主进程（spawn + 健康检查）
  | "start_comfyui";

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
  /**
   * P2-1：父任务 ID（None = 根任务）。
   * 前端 useTaskProgress 跟踪父任务时，把 parent_id == self 的子任务日志
   * 也一并显示到父任务的实时日志面板。
   */
  parent_id?: string | null;
}

/**
 * `task_progress` 事件 payload
 *
 * 对应后端 `task_scheduler/progress.rs::ProgressEvent`：
 * `{ task_id, progress, message, status }`
 *
 * 注意：`status` 是字符串（"queued" / "running" / "completed" / "failed" / "cancelled"），
 * 不是 TaskStatus discriminated union。
 */
export interface TaskProgressEvent {
  task_id: string;
  progress: number;
  message: string | null;
  status: string;
}

/**
 * `task_completed` 事件 payload
 *
 * 对应后端 `task_scheduler/progress.rs::TerminalEvent`：
 * `{ task_id, status, summary }`
 *
 * 注意：与 TaskInfo 不同，TerminalEvent 只携带终态信息，不包含 kind/name/priority 等。
 */
export interface TaskTerminalEvent {
  task_id: string;
  status: string;
  summary: string | null;
  /**
   * v3.6：任务结果载荷（任意 JSON），由后端 `TaskResult.payload` 携带。
   * - Diagnose 任务：携带 DiagnoseReport
   * - 其他任务：可能为 null
   * 前端通过 `waitForTaskWithPayload<T>` 取出并按需类型断言。
   */
  payload?: unknown;
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

// ============================================================================
// v1.8 / F36-Phase2：环境诊断 + 修复（对应 src-tauri/src/python_env/recovery.rs）
// ============================================================================

/** 问题严重度 */
export type IssueSeverity = "info" | "warning" | "error" | "critical";

/** 单个诊断问题 */
export interface EnvIssue {
  severity: IssueSeverity;
  /** 问题代码（前端国际化用） */
  code: string;
  /** 用户可读描述 */
  message: string;
  /** 详情（错误消息、traceback 等） */
  detail: string | null;
  /** 建议的修复动作 */
  suggested_action: RepairAction;
}

/** 修复动作（前端发命令时序列化为 snake_case 字符串） */
export type RepairAction =
  | "none"
  | "downgrade_numpy"
  | "reinstall_torch"
  | "reinstall_requirements"
  | "rebuild_venv";

/** 完整诊断报告 */
export interface DiagnoseReport {
  venv_exists: boolean;
  torch_import_ok: boolean;
  torch_version: string | null;
  issues: EnvIssue[];
  suggested_action: RepairAction;
  suggested_reason: string;
}

// ============================================================================
// v3.2.2 ProcessLauncher 事件 payload（对应 src-tauri/src/process_launcher/*）
// ============================================================================

/** `comfyui_log` 事件 payload（对应 log_pipeline.rs:185 emit） */
export interface ComfyUILogEvent {
  source: "stdout" | "stderr";
  line: string;
  ts: string; // ISO 8601
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

// ============================================================================
// v3.0 torch 多厂商支持（F25）
// ============================================================================

/**
 * torch 安装变体（5 厂商）
 *
 * 后端 `src-tauri/src/python_env/torch_variant.rs::TorchVariant` 的 serde 形式：
 * `#[serde(tag = "vendor", content = "version", rename_all = "snake_case")]`
 *
 * v3.7：对齐 PyTorch 2.11 官方 wheel
 * - NVIDIA CUDA: cu118 / cu126 / cu128 / cu130（删除已弃用的 cu121/cu124）
 * - AMD ROCm: rocm6.3 / 6.4 / 7.0 / 7.1 / 7.2（删除过时的 5.7/6.0/6.1）
 *
 * 对应格式：
 * - `{ vendor: "nvidia_cuda", version: "cu118" | "cu126" | "cu128" | "cu130" }`
 * - `{ vendor: "amd_rocm",  version: "rocm6.3" | "rocm6.4" | "rocm7.0" | "rocm7.1" | "rocm7.2" }`
 * - `{ vendor: "intel_xpu" }`
 * - `{ vendor: "apple_silicon" }`
 * - `{ vendor: "cpu_only" }`
 */
export type TorchVariant =
  | { vendor: "nvidia_cuda"; version: "cu118" | "cu126" | "cu128" | "cu130" }
  | { vendor: "amd_rocm"; version: "rocm6.3" | "rocm6.4" | "rocm7.0" | "rocm7.1" | "rocm7.2" }
  | { vendor: "intel_xpu" }
  | { vendor: "apple_silicon" }
  | { vendor: "cpu_only" };

/** 厂商枚举（UI 一级 Tab 用） */
export type TorchVendor = TorchVariant["vendor"];

/** torch 变体显示信息（UI 二级选项用） */
export interface TorchVariantOption {
  variant: TorchVariant;
  label: string; // e.g. "CUDA 12.1"
  compatible: boolean; // 平台兼容性（不兼容时 UI 灰显）
  hint?: string; // 兼容性提示
}

// ============================================================================
// v3.0 GPU 自动检测（F25）
// ============================================================================

/** GPU 厂商（与后端 `GpuVendor` 对应） */
export type GpuVendor = "nvidia" | "amd" | "intel" | "apple" | "unknown";

/** 单个 GPU 信息 */
export interface GpuInfo {
  vendor: GpuVendor;
  /** 型号（如 "GeForce RTX 4080" / "Radeon RX 7900 XT" / "Apple M2 Pro"） */
  model: string;
  /** 显存大小（MB），无法探测时为 null */
  vram_mb: number | null;
  /** 驱动版本（NVIDIA 专用） */
  driver_version: string | null;
  /** CUDA 版本（NVIDIA 专用，从 nvidia-smi 头部提取） */
  cuda_version: string | null;
  /** ROCm 版本（AMD 专用，预留） */
  rocm_version: string | null;
}
