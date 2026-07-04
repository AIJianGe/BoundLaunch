//! AppState - 全局应用状态容器
//!
//! 设计模式：单例 + 依赖注入（通过 `State<'_, AppState>` 注入到 Tauri 命令）
//! 详见 `PR/02-技术架构.md §3 线程模型` 和 `PR/03-模块设计/00-模块总览.md`

use crate::config::ConfigService;
use crate::core_manager::CoreManagerService;
use crate::env_inspector::EnvironmentInspectorService;
use crate::event_bus::EventBus;
use crate::log_store::LogStoreService;
use crate::model_path::ModelPathService;
use crate::plugin_manager::PluginManagerService;
use crate::process_launcher::ProcessLauncherService;
use crate::python_env::PythonEnvService;
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
    pub model_path: Arc<ModelPathService>,
    pub plugin_manager: Arc<PluginManagerService>,
    pub task_scheduler: Arc<TaskSchedulerService>,
    pub process_launcher: Arc<ProcessLauncherService>,
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
        let env_inspector = Arc::new(EnvironmentInspectorService::new((*event_bus).clone()));
        let python_env = Arc::new(PythonEnvService::from_path((*event_bus).clone()));
        // CoreManager 用临时目录（测试不真实操作 git）
        let tmp = std::env::temp_dir().join(format!("boundlaunch-test-{}", uuid::Uuid::new_v4()));
        let core_manager = Arc::new(CoreManagerService::new(
            tmp.clone(),
            (*event_bus).clone(),
            log_store.clone(),
        ));
        // ModelPathService 复用同一临时 comfyui_root
        let model_path = Arc::new(ModelPathService::new(tmp.clone()));
        // PluginManager 用临时 custom_nodes + venv
        let custom_nodes = tmp.join("custom_nodes");
        let venv_path = tmp.join("venv");
        let plugin_manager = Arc::new(PluginManagerService::new(
            custom_nodes,
            venv_path,
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
            model_path.clone(),
            log_store.clone(),
            config.clone(),
            tmp.clone(),
            tmp.join("venv"),
            data_dir,
        ));
        Self {
            event_bus,
            config,
            log_store,
            env_inspector,
            python_env,
            core_manager,
            model_path,
            plugin_manager,
            task_scheduler,
            process_launcher,
        }
    }
}

