//! AppState - 全局应用状态容器
//!
//! 设计模式：单例 + 依赖注入（通过 `State<'_, AppState>` 注入到 Tauri 命令）
//! 详见 `PR/02-技术架构.md §3 线程模型` 和 `PR/03-模块设计/00-模块总览.md`

use crate::config::ConfigService;
use crate::core_manager::CoreManagerService;
use crate::env_inspector::EnvironmentInspectorService;
use crate::event_bus::EventBus;
use crate::log_store::LogStoreService;
use crate::plugin_manager::PluginManagerService;
use crate::process_launcher::ProcessLauncherService;
use crate::process_launcher::ShutdownCoordinator;
use crate::pseudo_terminal::PseudoTerminalService;
use crate::python_env::PythonEnvService;
use crate::python_env::TransformersVersionIndex;
use crate::task_scheduler::TaskSchedulerService;
use std::sync::Arc;

/// 全局应用状态
///
/// 各 Service 在对应 Phase 实现后挂载到此处。
/// 通过 `Arc` 共享给 Tauri command handler。
#[derive(Clone)]
pub struct AppState {
    pub event_bus: Arc<EventBus>,
    pub config: Arc<ConfigService>,
    pub log_store: Arc<LogStoreService>,
    pub env_inspector: Arc<EnvironmentInspectorService>,
    pub python_env: Arc<PythonEnvService>,
    pub core_manager: Arc<CoreManagerService>,
    pub plugin_manager: Arc<PluginManagerService>,
    pub task_scheduler: Arc<TaskSchedulerService>,
    pub process_launcher: Arc<ProcessLauncherService>,
    /// F24 退出流程编排器（防重入 + 5 步事务 + 30s 超时兜底）
    pub shutdown_coordinator: Arc<ShutdownCoordinator>,
    /// v3.7 新增：transformers 版本索引（PyPI 拉取 + 三层缓存）
    pub transformers_index: Arc<TransformersVersionIndex>,
    /// 伪终端服务（交互式终端会话）
    pub pseudo_terminal: Arc<PseudoTerminalService>,
}

impl AppState {
    /// 创建测试用 AppState
    ///
    /// 注意：生产环境 AppState 在 `lib.rs` 的 setup hook 中初始化，
    /// 显式装配各 Service（含真实路径的 Config / LogStore）
    pub async fn new_for_test() -> Self {
        let event_bus = Arc::new(EventBus::new());
        let config = Arc::new(ConfigService::new_for_test((*event_bus).clone()));
        let log_store = Arc::new(
            LogStoreService::new(None)
                .await
                .expect("failed to init test LogStore"),
        );
        let env_inspector = Arc::new(EnvironmentInspectorService::new_for_test((*event_bus).clone()));
        let python_env = Arc::new(PythonEnvService::from_path((*event_bus).clone()));
        // CoreManager 测试 fixture（路径热加载：用 config.update 设置 comfyui_root）
        let tmp = std::env::temp_dir().join(format!("boundlaunch-test-{}", uuid::Uuid::new_v4()));
        config
            .update(|cfg| {
                cfg.paths.comfyui_root = tmp.clone();
                cfg.paths.venv_path = tmp.join("venv");
                Ok(())
            })
            .await
            .expect("set paths");
        let core_manager = Arc::new(CoreManagerService::new(
            config.clone(),
            (*event_bus).clone(),
            log_store.clone(),
        ));
        // PluginManager（路径热加载：复用 config）
        let plugin_manager = Arc::new(PluginManagerService::new(
            config.clone(),
            (*event_bus).clone(),
        ));
        // TaskScheduler 测试用 new_for_test（无 AppHandle，emit 跳过）
        let task_scheduler = Arc::new(TaskSchedulerService::new_for_test(
            3,
            20,
            log_store.clone(),
        ));
        // ProcessLauncher 测试用临时 data_dir（PID 文件存放处）
        let data_dir = std::env::temp_dir().join(format!(
            "boundlaunch-test-data-{}",
            uuid::Uuid::new_v4()
        ));
        let process_launcher = Arc::new(ProcessLauncherService::new(
            python_env.clone(),
            log_store.clone(),
            config.clone(),
            data_dir,
        ));
        // F24 退出流程：构造 ShutdownCoordinator（依赖 process_launcher + event_bus + task_scheduler）
        // v3.5：新增 task_scheduler 参数，用于退出时取消阻塞的版本切换类任务
        let shutdown_coordinator = Arc::new(ShutdownCoordinator::new(
            (*process_launcher).clone(),
            (*event_bus).clone(),
            task_scheduler.clone(),
        ));
        // v3.7：transformers 版本索引（测试用临时缓存文件，不 spawn_refresh 避免网络调用）
        let transformers_cache_file = std::env::temp_dir().join(format!(
            "bl-test-transformers-{}.json",
            uuid::Uuid::new_v4()
        ));
        let transformers_index = Arc::new(TransformersVersionIndex::new_for_test(
            transformers_cache_file,
            (*event_bus).clone(),
        ));
        let pseudo_terminal = Arc::new(PseudoTerminalService::new());
        Self {
            event_bus,
            config,
            log_store,
            env_inspector,
            python_env,
            core_manager,
            plugin_manager,
            task_scheduler,
            process_launcher,
            shutdown_coordinator,
            transformers_index,
            pseudo_terminal,
        }
    }
}

