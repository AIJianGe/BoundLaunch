//! Config 数据模型
//!
//! 详见 `PR/03-模块设计/01-Config.md §4.1 核心结构体`

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 顶层 Config 结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub paths: PathsConfig,
    pub launch: LaunchConfig,
    pub torch: TorchConfig,
    /// v3.x：models section 已废弃（被 paths.models_path 软链接方案取代）
    ///
    /// - 保留字段：**仅用于向后兼容旧 config.toml**（老用户可能有 `[models]` 段）
    /// - 不再写入：UI 已删除"模型路径"菜单项，没有修改入口
    /// - 解析时静默忽略内容（不读、不校验、不参与业务逻辑）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<serde_json::Value>,
    pub ui: UiConfig,
    /// 配置 schema 版本，用于迁移
    pub schema_version: u32,
    /// v3.x：环境名（来自 launcher-portable.dat 或目录名）
    ///
    /// - 用于日志、UI 显示、数据库命名空间
    /// - 不参与路径解析
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_name: Option<String>,
}

/// 路径配置
///
/// **v3.x 绿色版关键设计**：本结构**只在 TOML 持久化时跳过**，不影响 invoke 序列化！
///
/// ## 为什么
///
/// 绿色版（launcher-portable.dat 存在）的核心需求是"**复制多目录 = 多环境**"。
/// 用户经常把整个环境目录用 zip 工具压缩后分发到新位置解压。
/// 如果 config.toml 里存了**绝对路径**（如 `D:\EnvA\ComfyUI`），分发到 `E:\EnvB\` 后
/// 路径失效，配置变成"看上去配好了但找不到 ComfyUI"的状态。
///
/// ## 解决方案（v3.x 修复版）
///
/// - **invoke 序列化**（给前端）：返回完整 PathsConfig
///   - 前端依赖 `cfg.paths.comfyui_root` 做路由守卫、UI 展示
///   - **绝对不能 skip**（否则路由守卫会误判为"未初始化"，把用户弹回 /onboarding）
/// - **TOML 持久化**（写 config.toml）：用 `ConfigForToml` 视图结构**根本不带 paths 段**
///   - 旧版用 `#[serde(skip_serializing)]` 同时影响两个序列化器 → 导致前端拿不到
///   - 修复：去掉字段级 skip_serializing，改用视图结构控制 TOML 序列化
/// - 内存中 cfg.paths 永远从 `launcher-portable.dat` 解析（`apply_default_paths`）
///   - 解压到新目录 → env_paths 自动用新 `<exe_dir>` 解析 → 路径自动适配 ✅
///
/// ## 用户改路径怎么办
///
/// 绿色版用户想改 ComfyUI / venv 路径 → 直接编辑 `launcher-portable.dat`（相对路径）。
/// UI 上"仓库地址管理"页面的路径字段展示的是**当前解析结果**，修改后**不持久化**到 config.toml。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PathsConfig {
    /// ComfyUI 仓库根目录
    ///
    /// 内存中：来自 `env_paths::resolve()` 解析的绝对路径（每次启动重新解析）
    /// 持久化：**不**写入 config.toml（v3.x 绿色版约定）
    ///
    /// v3.x 修复：去掉 `skip_serializing`，让前端 invoke 能拿到这个字段。
    /// TOML 持久化由 `ConfigForToml` 视图结构控制（不写 paths 段）。
    #[serde(default)]
    pub comfyui_root: PathBuf,
    /// venv 虚拟环境路径
    ///
    /// v3.1（F26）：默认独立于 ComfyUI 仓库（位于 app_data_dir/data/venv），
    /// 切换 ComfyUI 版本时不影响 venv。详见 §F26 决策 1：路径布局 B。
    ///
    /// **v1.8 / F36 强约束**：`venv_path` 禁止放在 `src-tauri/` 子目录下！
    /// 原因：Tauri dev 模式自动监视 `src-tauri/` 目录所有文件变化触发 rebuild。
    /// `uv pip install` 修改 venv 内部文件（.lock、site-packages 等）会触发启动器重启，
    /// 导致长任务（torch 安装）被打断、用户环境永远装不全。
    /// 校验函数：[`crate::config::service::validate_paths`]
    ///
    /// 内存中：来自 `env_paths::resolve()` 解析的绝对路径
    /// 持久化：**不**写入 config.toml（v3.x 绿色版约定）
    #[serde(default)]
    pub venv_path: PathBuf,
    /// Python 版本（如 "3.11"）
    ///
    /// 内存中：从已安装的 venv 解析
    /// 持久化：**不**写入 config.toml（避免"版本对了但 venv 在别处"的混乱）
    #[serde(default)]
    pub python_version: String,
    /// 自定义 models 数据目录（v3.1 / F26 决策 12：C. 只允许 models 路径自定义）
    ///
    /// - `None`：使用默认 `<comfyui_root>/models`（向后兼容）
    /// - `Some(path)`：使用自定义路径，并通过 junction/symlink 把
    ///   `<comfyui_root>/models` 软链接到该路径，实现跨版本共享模型数据
    ///
    /// 切换 ComfyUI 版本时，会重新建立软链接关系，确保数据不丢失。
    #[serde(default)]
    pub models_path: Option<PathBuf>,
    /// ComfyUI 仓库 URL（F31 新增）
    ///
    /// - `None`：使用默认常量 `COMFYUI_REPO_URL`（官方仓库）
    /// - `Some(url)`：用户自定义仓库 URL（支持带 token 的私有仓库）
    ///
    /// 日志和 UI 显示时需脱敏（把 token 部分替换为 ***）。
    /// 切换仓库地址时由 `core_set_repo_url` 命令写入。
    ///
    /// 持久化：**不**写入 config.toml（绿色版约定）
    #[serde(default)]
    pub comfyui_repo_url: Option<String>,
    /// 引导安装默认版本（v3.10 新增）
    ///
    /// - `None`：使用自动规则（`tags::latest_stable_for_installation`，
    ///   即「patch = 0/1 + 跳过首次大版本 + SemVer 倒序最大」）
    /// - `Some(tag)`：用户显式指定（如 "v0.3.10"），跳过自动规则直接 checkout
    ///
    /// 用途：用户希望默认装到特定版本（如某次 ComfyUI 大版本升级时
    /// 想继续停留在 v0.x 线，主动锁定 v0.3.10）
    ///
    /// 校验：`update_latest_stable_for_installation` 中若 tag 不存在，
    /// 降级到自动规则 + warn 日志（不阻塞 onboarding）
    ///
    /// 持久化：**不**写入 config.toml（绿色版约定）
    #[serde(default)]
    pub installation_default_version: Option<String>,
    /// v3.x：custom_nodes 绝对路径
    ///
    /// 优先级（启动时由 `paths::env_paths::resolve` 决定）：
    /// 1. `launcher-portable.dat` 的 `paths.custom_nodes`（绝对路径或相对 env_root）
    /// 2. 默认 `<comfyui_root>/custom_nodes/`（即 ComfyUI 内）
    ///
    /// **运行时如何生效**：
    /// - 业务代码（plugin_manager）从这里读
    /// - ComfyUI 启动时是否生效，取决于 `comfyui_base_directory` 字段：
    ///   - `comfyui_base_directory == comfyui_root` → ComfyUI 用 `folder_paths.py:41` 默认 → 找到这里
    ///   - 不等 → launcher 启动 ComfyUI 时加 `--base-directory <...>` 让 ComfyUI 用新 base
    #[serde(default)]
    pub custom_nodes_path: Option<PathBuf>,
    /// v3.x：ComfyUI 启动时要传的 `--base-directory` 参数值
    ///
    /// **何时生效**：
    /// - `Some(path)`：launcher 启动 ComfyUI 时加 `--base-directory <path>`
    ///   → ComfyUI 内部 `folder_paths.py` 的 `base_path` 变成这个值
    ///   → `custom_nodes / models / input / output` 都从这下面找
    /// - `None`：不传 → ComfyUI 用自己的 base_path（`folder_paths.py` 所在目录）
    ///
    /// **典型用法**：
    /// - 默认：`None`（不传）→ ComfyUI 内部默认
    /// - 自定义 custom_nodes 在 ComfyUI 外：`Some(custom_nodes 父目录)`
    #[serde(default)]
    pub comfyui_base_directory: Option<PathBuf>,
}

/// 启动配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchConfig {
    /// 运行模式
    pub mode: LaunchMode,
    /// 监听地址
    pub listen_host: String,
    /// 监听端口
    pub listen_port: u16,
    /// 自动打开浏览器
    pub auto_open_browser: bool,
    /// 预览方式
    pub preview_method: PreviewMethod,
    /// 自定义启动参数（仅 LaunchMode::Custom 时使用）
    pub custom_args: String,
    /// 高级参数
    pub advanced: AdvancedArgs,
    /// **v3.x Phase 5**：多 GPU 选择（仅"全部使用"和"单卡模式"）
    ///
    /// - `None` 或 `{ mode: "all" }`：所有 GPU 都参与（不设 CUDA_VISIBLE_DEVICES）
    /// - `{ mode: "single", single_index: 0 }`：只用第 0 块 GPU（CUDA_VISIBLE_DEVICES=0）
    ///
    /// 不存到 `config.toml` 路径相关字段（与 paths 不同），但作为 launch 配置项持久化。
    /// 简化决策：暂不考虑 NVLink 集群等高级配置。
    #[serde(default)]
    pub gpu_selection: Option<GpuSelectionConfig>,
}

/// v3.x Phase 5：多 GPU 选择配置
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct GpuSelectionConfig {
    /// 选择模式
    pub mode: GpuSelectionMode,
    /// 单卡模式时选中的 GPU 索引（0-based）
    pub single_index: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GpuSelectionMode {
    /// 全部使用
    All,
    /// 单卡模式
    Single,
}

impl Default for GpuSelectionConfig {
    fn default() -> Self {
        Self {
            mode: GpuSelectionMode::All,
            single_index: 0,
        }
    }
}

/// 运行模式
///
/// 设计模式：Strategy - 不同模式构造不同命令参数
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LaunchMode {
    /// CPU 模式（--cpu --lowvram）
    Cpu,
    /// GPU 高显存（--highvram）
    GpuHigh,
    /// GPU 低显存（--lowvram）
    GpuLow,
    /// GPU 无显存（--novram）
    GpuNoVram,
    /// 自定义（使用 custom_args）
    Custom,
}

impl LaunchMode {
    /// 转为 ComfyUI 命令行参数片段
    pub fn to_args(&self) -> &'static [&'static str] {
        match self {
            Self::Cpu => &["--cpu", "--lowvram"],
            Self::GpuHigh => &["--highvram"],
            Self::GpuLow => &["--lowvram"],
            Self::GpuNoVram => &["--novram"],
            Self::Custom => &[],
        }
    }
}

/// 预览方式
///
/// **v3.4.1 修复**：与 ComfyUI main.py argparse 对齐，只接受以下 4 个值：
/// - `none`
/// - `auto`
/// - `latent2rgb`
/// - `taesd`
///
/// 旧版本里使用的 `latent` / `latent-upscale` / `autoencoder` 在新版 ComfyUI 中已被移除，
/// 直接传这些值会导致 main.py 启动时 argparse 失败、进程退出码 2。
/// 旧 config 中的值会在 `load_or_default` 中自动迁移（见 `migrate_legacy_preview_method`）。
///
/// **serde 标签说明**：用 `#[serde(rename = "...")]` 显式指定每个变体名，
/// 不依赖 `rename_all` 的命名规则推断（避免 `Latent2Rgb` 之类的驼峰被错误转换）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PreviewMethod {
    #[serde(rename = "none")]
    None,
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "latent2rgb")]
    Latent2Rgb,
    #[serde(rename = "taesd")]
    Taesd,
}

impl PreviewMethod {
    pub fn to_arg(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Auto => "auto",
            Self::Latent2Rgb => "latent2rgb",
            Self::Taesd => "taesd",
        }
    }

    /// 从字符串解析（用于旧 config 迁移和前端 patch）
    ///
    /// 严格模式：只接受 ComfyUI 实际支持的 4 个值。旧值（`latent`/`latent-upscale`/`autoencoder`）
    /// 返回 `Err`，由调用方走 `migrate_legacy_preview_method` 走迁移。
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "none" => Ok(Self::None),
            "auto" => Ok(Self::Auto),
            "latent2rgb" => Ok(Self::Latent2Rgb),
            "taesd" => Ok(Self::Taesd),
            other => Err(format!("unsupported preview_method: '{}'", other)),
        }
    }
}

/// **v3.4.1 新增**：将旧版 preview_method 字符串迁移到 ComfyUI 实际支持的值
///
/// 旧 → 新映射：
/// - `latent` → `latent2rgb`（最接近的等价物）
/// - `latent-upscale` → `taesd`（高质量预览，新版推荐）
/// - `autoencoder` → `auto`（新版自动选择）
///
/// 返回 `Some(new_value)` 表示发生了迁移，`None` 表示输入已是新格式。
pub fn migrate_legacy_preview_method(s: &str) -> Option<String> {
    match s {
        // 已经是新格式
        "none" | "auto" | "latent2rgb" | "taesd" => None,
        // 旧值 → 新值
        "latent" => Some("latent2rgb".to_string()),
        "latent-upscale" => Some("taesd".to_string()),
        "autoencoder" => Some("auto".to_string()),
        // 未知值：保守起见，强制迁移到 `latent2rgb`（最接近的默认）
        _ => Some("latent2rgb".to_string()),
    }
}

/// 高级启动参数
///
/// **v3.x 新增字段**：
/// - `gpu_only` — "使用共享显存"开关（与 ComfyUI `--gpu-only` 互为反义）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdvancedArgs {
    pub use_split_cross_attention: bool,
    pub use_pytorch_cross_attention: bool,
    pub force_fp32: bool,
    pub fp16_vae: bool,
    pub bf16_vae: bool,
    pub no_half: bool,
    pub no_half_vae: bool,
    pub directml: bool,
    /// **v3.x 新增**：禁用自动 spill 到 CPU 内存（"使用共享显存"开关）
    ///
    /// ## 含义（与 ComfyUI `--gpu-only` 互为反义）
    /// - `false`（默认）：ComfyUI 默认行为 — 模型/CLIP/VAE/conditioning 在显存不够时
    ///   **自动 spill 到 CPU 内存**（"使用共享显存"）
    /// - `true`：传 `--gpu-only` 给 ComfyUI — **所有组件强制在 GPU 显存**，
    ///   OOM 时直接报错，不会 spill 到内存
    ///
    /// ## 与其他参数的关系
    /// | LaunchMode | gpu_only 建议 |
    /// |-----------|---------------|
    /// | `Cpu`     | 强制 false（无 GPU） |
    /// | `GpuHigh` | 任意（highvram 已在显存） |
    /// | `GpuLow`  | 建议 false（lowvram 故意 spill，强制会 OOM） |
    /// | `GpuNo`   | 强制 false（novram 完全 spill，强制会 OOM） |
    ///
    /// UI 层在 `LaunchMode` 为 `GpuLow`/`GpuNo` 时强制禁用该开关并禁灰。
    ///
    /// ## 性能权衡
    /// - **开启**（默认值，spill 模式）：模型大小不受显存限制，性能可能降级
    ///   （PCIe/NVLink 带宽比 HBM/GDDR 低几十倍）
    /// - **关闭**（gpu_only）：性能稳定，但模型总大小 ≤ 显存容量
    pub gpu_only: bool,
}

impl Default for AdvancedArgs {
    fn default() -> Self {
        Self {
            use_split_cross_attention: false,
            use_pytorch_cross_attention: false,
            force_fp32: false,
            fp16_vae: false,
            bf16_vae: false,
            no_half: false,
            no_half_vae: false,
            directml: false,
            gpu_only: false,
        }
    }
}

/// torch 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorchConfig {
    /// CUDA 版本（v3.0 前字段，向后兼容保留）
    ///
    /// 老 config 文件或旧 install_torch 命令使用。
    /// 新版推荐通过 `torch_variant` 字段表达（多厂商支持，F25）。
    /// 切换 torch 时由 `env_change_torch_variant` 同步写入。
    pub cuda_version: CudaVersion,
    /// torch 变体（v3.0 新增，F25）
    ///
    /// 序列化为 JSON 字符串（避免 config 模块与 python_env 模块循环依赖）。
    /// 解析为 `crate::python_env::TorchVariant`，支持多厂商：
    /// NVIDIA CUDA / AMD ROCm / Intel XPU / Apple MPS / CPU。
    ///
    /// 缺省 = None（启动时让前端触发 GPU 检测 + 智能推荐后写入）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub torch_variant: Option<String>,
}

/// CUDA 版本
///
/// 设计模式：Strategy - 不同 CUDA 版本对应不同 torch wheel 索引
///
/// v3.7：对齐 PyTorch 2.11 官方 wheel
/// - 删除 Cu121 / Cu124（PyTorch 2.9+ 不再提供）
/// - 新增 Cu126 / Cu128 / Cu130
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CudaVersion {
    /// CPU 版本
    Cpu,
    /// CUDA 11.8（旧版兼容）
    Cu118,
    /// CUDA 12.6（稳定）
    Cu126,
    /// CUDA 12.8（稳定）
    Cu128,
    /// CUDA 13.0（最新）
    Cu130,
}

impl CudaVersion {
    /// 转为 uv pip install 的 torch 索引 URL 后缀
    pub fn to_torch_index(&self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Cu118 => "cu118",
            Self::Cu126 => "cu126",
            Self::Cu128 => "cu128",
            Self::Cu130 => "cu130",
        }
    }

    /// 显示名
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Cu118 => "CUDA 11.8",
            Self::Cu126 => "CUDA 12.6",
            Self::Cu128 => "CUDA 12.8",
            Self::Cu130 => "CUDA 13.0",
        }
    }
}

/// 模型路径配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsConfig {
    /// 模型路径模式
    pub mode: ModelsMode,
    /// 自定义根目录（ModelsMode::CustomRoot 时使用）
    pub custom_root: PathBuf,
    /// 高级配置（按类型指定，本期不用）
    pub advanced: AdvancedModels,
}

/// 模型路径模式
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelsMode {
    /// 默认（ComfyUI 根目录/models）
    Default,
    /// 自定义根目录
    CustomRoot,
    /// 高级（按类型指定）
    Advanced,
}

/// 高级模型路径配置（本期不实现）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AdvancedModels {
    // 未来按类型扩展
}

/// UI 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    /// 主题
    pub theme: Theme,
    /// 语言（本期仅 zh-CN）
    pub language: String,
    /// 启动时自动检查更新
    pub auto_check_update: bool,
    /// 关闭窗口时最小化到托盘
    pub minimize_to_tray: bool,
}

/// 主题
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    Light,
    Dark,
    Auto,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            paths: PathsConfig {
                comfyui_root: PathBuf::new(),
                venv_path: PathBuf::new(),
                python_version: "3.11".to_string(),
                models_path: None,
                comfyui_repo_url: None,
                installation_default_version: None,
                // v3.x：custom_nodes / comfyui_base_directory 启动时由 env_paths 填充
                custom_nodes_path: None,
                comfyui_base_directory: None,
            },
            launch: LaunchConfig {
                mode: LaunchMode::GpuHigh,
                listen_host: "127.0.0.1".to_string(),
                listen_port: 8188,
                auto_open_browser: true,
                // v3.4.1 修复：旧版 PreviewMethod::Latent（"latent"）已被 ComfyUI 移除
                // 改用 Latent2Rgb（最接近的等价物）
                preview_method: PreviewMethod::Latent2Rgb,
                custom_args: String::new(),
                advanced: AdvancedArgs::default(),
                // v3.x Phase 5：默认全部 GPU
                gpu_selection: Some(GpuSelectionConfig::default()),
            },
            torch: TorchConfig {
                // v3.7：默认 CUDA 12.8（PyTorch 2.11 稳定版推荐）
                cuda_version: CudaVersion::Cu128,
                torch_variant: None,
            },
            // v3.x：models 段已废弃，留 None
            models: None,
            ui: UiConfig {
                theme: Theme::Auto,
                language: "zh-CN".to_string(),
                auto_check_update: true,
                minimize_to_tray: true,
            },
            schema_version: super::CURRENT_SCHEMA_VERSION,
            // v3.x：环境名（首次启动时由 env_paths::resolve 填充）
            env_name: None,
        }
    }
}

// ============================================================================
// Patch 结构（部分更新）
//
// 设计意图：前端 `ConfigUpdate = Partial<{ ... Partial<Section> }>`，每个字段
// 都是可选的。后端收到 patch 后，把 None 的字段跳过，Some 的字段覆盖。
//
// 与「整段 deserialize」的区别：避免调用方为单个字段拼出整段 section。
// 详见 `commands/config.rs::config_update`。
// ============================================================================

/// Paths section 的 patch
#[derive(Debug, Default, Deserialize)]
pub struct PathsConfigPatch {
    pub comfyui_root: Option<PathBuf>,
    pub venv_path: Option<PathBuf>,
    pub python_version: Option<String>,
    /// v3.1 / F26：自定义 models 路径
    ///
    /// 语义：
    /// - `None`（字段未提供）：跳过（不修改）
    /// - `Some(空 PathBuf)`：清空自定义路径，恢复使用 `<comfyui_root>/models` 默认值
    /// - `Some(非空 PathBuf)`：设置自定义 models 路径
    ///
    /// 前端约定：传 `""` 表示清空，传 `"D:/models"` 表示设置，不传字段表示跳过。
    /// apply_paths_patch 中按 `path.as_os_str().is_empty()` 判断清空语义。
    pub models_path: Option<PathBuf>,
    /// F31：ComfyUI 仓库 URL
    pub comfyui_repo_url: Option<String>,
    /// v3.10：引导安装默认版本
    /// - `None`：走自动规则
    /// - `Some("")`：清空（恢复走自动规则）
    /// - `Some(非空)`：用户显式指定
    pub installation_default_version: Option<String>,
    /// v3.x：custom_nodes 绝对路径（高级用户配置）
    /// - `None`：跳过（用 env_paths 解析的默认值）
    /// - `Some(path)`：显式设置
    pub custom_nodes_path: Option<PathBuf>,
    /// v3.x：ComfyUI 启动时传的 `--base-directory` 参数
    /// - `None`：不传
    /// - `Some(path)`：传这个值
    pub comfyui_base_directory: Option<PathBuf>,
}

/// Launch section 的 patch
#[derive(Debug, Default, Deserialize)]
pub struct LaunchConfigPatch {
    pub mode: Option<LaunchMode>,
    pub listen_host: Option<String>,
    pub listen_port: Option<u16>,
    pub auto_open_browser: Option<bool>,
    pub preview_method: Option<PreviewMethod>,
    pub custom_args: Option<String>,
    pub advanced: Option<AdvancedArgs>,
    /// **v3.x Phase 5**：多 GPU 选择 patch
    pub gpu_selection: Option<GpuSelectionConfig>,
}

/// Torch section 的 patch
#[derive(Debug, Default, Deserialize)]
pub struct TorchConfigPatch {
    pub cuda_version: Option<CudaVersion>,
    /// v3.0 新增：多厂商 torch 变体（JSON 字符串）
    pub torch_variant: Option<String>,
}

/// Models section 的 patch
#[derive(Debug, Default, Deserialize)]
pub struct ModelsConfigPatch {
    pub mode: Option<ModelsMode>,
    pub custom_root: Option<PathBuf>,
    pub advanced: Option<AdvancedModels>,
}

/// UI section 的 patch
#[derive(Debug, Default, Deserialize)]
pub struct UiConfigPatch {
    pub theme: Option<Theme>,
    pub language: Option<String>,
    pub auto_check_update: Option<bool>,
    pub minimize_to_tray: Option<bool>,
}

/// 顶层 Config patch
#[derive(Debug, Default, Deserialize)]
pub struct ConfigPatch {
    pub paths: Option<PathsConfigPatch>,
    pub launch: Option<LaunchConfigPatch>,
    pub torch: Option<TorchConfigPatch>,
    pub models: Option<ModelsConfigPatch>,
    pub ui: Option<UiConfigPatch>,
}

/// 把 PathsConfigPatch 合并到 PathsConfig（Some 字段覆盖）
///
/// v3.11.4：路径分隔符规范化（防御性）
/// - 前端可能传来混合分隔符的路径（如 `D:\AIWork\myComfyui/ComfyUI`）
/// - Windows 上统一为 `\`，Unix 上统一为 `/`
/// - 避免错误日志、路径比对、git 操作等场景中的混淆
pub fn apply_paths_patch(cfg: &mut PathsConfig, patch: PathsConfigPatch) {
    if let Some(v) = patch.comfyui_root {
        cfg.comfyui_root = normalize_path_separators(&v);
    }
    if let Some(v) = patch.venv_path {
        cfg.venv_path = normalize_path_separators(&v);
    }
    if let Some(v) = patch.python_version {
        cfg.python_version = v;
    }
    // v3.1 / F26：models_path 三态语义
    // - None：跳过（字段未提供）
    // - Some(空)：清空自定义路径 → None（用默认 <comfyui_root>/models）
    // - Some(非空)：设置自定义路径
    if let Some(v) = patch.models_path {
        if v.as_os_str().is_empty() {
            cfg.models_path = None;
        } else {
            cfg.models_path = Some(v);
        }
    }
    // F31：comfyui_repo_url
    if let Some(v) = patch.comfyui_repo_url {
        if v.is_empty() {
            cfg.comfyui_repo_url = None;
        } else {
            cfg.comfyui_repo_url = Some(v);
        }
    }
    // v3.10：installation_default_version（与 comfyui_repo_url 同款三态语义）
    if let Some(v) = patch.installation_default_version {
        if v.is_empty() {
            cfg.installation_default_version = None;
        } else {
            cfg.installation_default_version = Some(v);
        }
    }
    // v3.x：custom_nodes_path（高级用户配置）
    // - 业务代码通常不通过 patch 改这个字段（启动时由 env_paths 自动设置）
    // - 这里保留 patch 支持，方便测试和未来 UI 直接编辑
    if let Some(v) = patch.custom_nodes_path {
        if v.as_os_str().is_empty() {
            cfg.custom_nodes_path = None;
        } else {
            cfg.custom_nodes_path = Some(normalize_path_separators(&v));
        }
    }
    // v3.x：comfyui_base_directory（高级用户配置）
    if let Some(v) = patch.comfyui_base_directory {
        if v.as_os_str().is_empty() {
            cfg.comfyui_base_directory = None;
        } else {
            cfg.comfyui_base_directory = Some(normalize_path_separators(&v));
        }
    }
}

/// v3.11.4：规范化路径分隔符
///
/// Windows 上 `\` 和 `/` 都能被文件系统接受，但混合使用（如 `D:\AIWork\myComfyui/ComfyUI`）
/// 在错误日志、路径比对、git 操作等场景中会造成困惑。
///
/// - Windows：`/` → `\`
/// - Unix：`\` → `/`
fn normalize_path_separators(path: &std::path::Path) -> std::path::PathBuf {
    let s = path.to_string_lossy();
    if cfg!(windows) {
        std::path::PathBuf::from(s.replace('/', "\\"))
    } else {
        std::path::PathBuf::from(s.replace('\\', "/"))
    }
}

// ============================================================================
// v3.x：TOML 持久化专用视图（关键修复）
// ============================================================================

/// **v3.x 关键修复**：TOML 持久化专用视图结构
///
/// ## 为什么需要这个
///
/// 之前 PathsConfig 字段标了 `#[serde(skip_serializing)]`，意图是不让绝对路径
/// 写入 config.toml（绿色版约定：复制目录后路径自动适配）。
///
/// **但 `skip_serializing` 是 serde 级别标记，对所有序列化器都生效**——
/// 包括 `serde_json`（Tauri invoke 用）。结果：
/// - 前端拿到的 `cfg.paths.comfyui_root` 永远是空字符串
/// - 路由守卫 `!configStore.comfyuiRoot === true` → 把用户弹回 /onboarding
/// - 看起来"按钮没反应"或"页面没跳转"
///
/// ## 修复方案
///
/// - **PathsConfig 字段**：去掉 `skip_serializing`，只保留 `#[serde(default)]`
///   - 反序列化时缺失字段用默认值（PathBuf::new() / 空字符串）
///   - 序列化时正常写出所有字段
/// - **TOML 持久化**：用本视图结构 `ConfigForToml`
///   - **不**包含 `paths` 字段（绝对路径不写到 config.toml）
///   - 也不包含 `listen_port`（来自 `launcher-portable.dat`，每个目录独立）
///   - launch 段保留其他用户配置（mode / listen_host / preview_method / custom_args / advanced / gpu_selection）
/// - **invoke 序列化**（给前端）：走完整 `Config`（带 paths）
///   - 前端 `configStore.comfyuiRoot` 能拿到真实路径
///   - 路由守卫正常工作
///
/// ## 不同目录互不影响（绿色版核心保证）
///
/// - `launcher-portable.dat` 每个目录独立
/// - config.toml **不**含绝对路径 + listen_port
/// - 复制目录到新位置 → 启动时 `env_paths::resolve()` 用新 `<exe_dir>` 重新解析
///   - 绝对路径自动适配 ✅
///   - 端口自动适配（每个目录可独立配置）✅
/// - WebView2 UserData / SQLite 数据库 已在 `exe_dir/.boundlaunch/`（不同目录天然隔离）
#[derive(Debug, Serialize)]
pub struct ConfigForToml<'a> {
    /// 配置 schema 版本
    pub schema_version: u32,
    /// v3.x：环境名（来自 launcher-portable.dat 或目录名）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_name: Option<&'a str>,
    /// 启动配置
    pub launch: LaunchConfigForToml<'a>,
    /// torch 配置
    pub torch: &'a TorchConfig,
    /// UI 配置
    pub ui: &'a UiConfig,
    /// v3.x：models 段已废弃，仅用于向后兼容
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<&'a serde_json::Value>,
}

/// **v3.x 关键修复**：LaunchConfig 的 TOML 视图
///
/// **listen_port 字段说明**：
/// - 字段正常序列化到 TOML（保留老用户的自定义端口）
/// - 但在 portable 模式下，`apply_default_paths` 会**总是**用 `resolved.port` 覆盖
///   - `launcher-portable.dat` 定义每个绿色版目录的端口
///   - 复制目录到新位置 → resolved 用新目录的端口 → listen_port 立即更新
///   - 老用户的自定义端口会被覆盖（这是绿色版"目录级配置"约定的代价）
/// - 非 portable 模式（传统安装）→ `apply_default_paths` 不覆盖 → 老用户端口保留
///
/// 其他字段正常持久化（mode / listen_host / auto_open_browser / preview_method /
/// custom_args / advanced / gpu_selection 都是用户级配置，不依赖目录）。
#[derive(Debug, Serialize)]
pub struct LaunchConfigForToml<'a> {
    pub mode: &'a LaunchMode,
    pub listen_host: &'a str,
    /// 启动端口
    /// - 非便携版：用户配置 → 写 TOML → 重启保留
    /// - 绿色版：被 `apply_default_paths` 每次启动用 `resolved.port` 覆盖
    pub listen_port: u16,
    pub auto_open_browser: bool,
    pub preview_method: &'a PreviewMethod,
    pub custom_args: &'a str,
    pub advanced: &'a AdvancedArgs,
    /// v3.x Phase 5：多 GPU 选择
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu_selection: Option<&'a GpuSelectionConfig>,
}

impl<'a> From<&'a Config> for ConfigForToml<'a> {
    fn from(cfg: &'a Config) -> Self {
        Self {
            schema_version: cfg.schema_version,
            env_name: cfg.env_name.as_deref(),
            launch: LaunchConfigForToml {
                mode: &cfg.launch.mode,
                listen_host: &cfg.launch.listen_host,
                listen_port: cfg.launch.listen_port,
                auto_open_browser: cfg.launch.auto_open_browser,
                preview_method: &cfg.launch.preview_method,
                custom_args: &cfg.launch.custom_args,
                advanced: &cfg.launch.advanced,
                gpu_selection: cfg.launch.gpu_selection.as_ref(),
            },
            torch: &cfg.torch,
            ui: &cfg.ui,
            models: cfg.models.as_ref(),
        }
    }
}

/// 把 LaunchConfigPatch 合并到 LaunchConfig
pub fn apply_launch_patch(cfg: &mut LaunchConfig, patch: LaunchConfigPatch) {
    if let Some(v) = patch.mode {
        cfg.mode = v;
    }
    if let Some(v) = patch.listen_host {
        cfg.listen_host = v;
    }
    if let Some(v) = patch.listen_port {
        cfg.listen_port = v;
    }
    if let Some(v) = patch.auto_open_browser {
        cfg.auto_open_browser = v;
    }
    if let Some(v) = patch.preview_method {
        cfg.preview_method = v;
    }
    if let Some(v) = patch.custom_args {
        cfg.custom_args = v;
    }
    if let Some(v) = patch.advanced {
        cfg.advanced = v;
    }
    // **v3.x Phase 5**：gpu_selection 接受 Some/None 两种语义
    // - `Some(config)`：使用新配置
    // - `None`：保留原值（patch 未传 = 不修改）
    if let Some(v) = patch.gpu_selection {
        cfg.gpu_selection = Some(v);
    }
}

/// 把 TorchConfigPatch 合并到 TorchConfig
pub fn apply_torch_patch(cfg: &mut TorchConfig, patch: TorchConfigPatch) {
    if let Some(v) = patch.cuda_version {
        cfg.cuda_version = v;
    }
    if let Some(v) = patch.torch_variant {
        cfg.torch_variant = Some(v);
    }
}

/// 把 ModelsConfigPatch 合并到 ModelsConfig（v3.x：已废弃，no-op）
///
/// 保留函数仅用于反序列化兼容，调用时直接忽略 patch。
/// 业务代码不应再调用本函数。
pub fn apply_models_patch(_cfg: &mut ModelsConfig, _patch: ModelsConfigPatch) {
    // v3.x：models 段已废弃，不再应用 patch
}

/// 把 UiConfigPatch 合并到 UiConfig
pub fn apply_ui_patch(cfg: &mut UiConfig, patch: UiConfigPatch) {
    if let Some(v) = patch.theme {
        cfg.theme = v;
    }
    if let Some(v) = patch.language {
        cfg.language = v;
    }
    if let Some(v) = patch.auto_check_update {
        cfg.auto_check_update = v;
    }
    if let Some(v) = patch.minimize_to_tray {
        cfg.minimize_to_tray = v;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let cfg = Config::default();
        assert_eq!(cfg.launch.listen_port, 8188);
        assert_eq!(cfg.torch.cuda_version, CudaVersion::Cu128);
        assert_eq!(cfg.ui.language, "zh-CN");
        assert_eq!(cfg.schema_version, 1);
    }

    #[test]
    fn test_launch_mode_to_args() {
        assert_eq!(LaunchMode::Cpu.to_args(), &["--cpu", "--lowvram"]);
        assert_eq!(LaunchMode::GpuHigh.to_args(), &["--highvram"]);
        assert!(LaunchMode::Custom.to_args().is_empty());
    }

    #[test]
    fn test_cuda_version_display() {
        assert_eq!(CudaVersion::Cu128.display_name(), "CUDA 12.8");
        assert_eq!(CudaVersion::Cu130.display_name(), "CUDA 13.0");
        assert_eq!(CudaVersion::Cpu.to_torch_index(), "cpu");
    }

    #[test]
    fn test_serde_roundtrip() {
        let cfg = Config::default();
        let toml_str = toml::to_string(&cfg).unwrap();
        let parsed: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.launch.listen_port, cfg.launch.listen_port);
        assert_eq!(parsed.torch.cuda_version, cfg.torch.cuda_version);
    }
}
