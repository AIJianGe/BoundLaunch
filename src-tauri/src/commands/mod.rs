//! Tauri commands 入口聚合
//!
//! 每个 Phase 完成后在此添加 #[tauri::command] 注册
//! 设计模式：门面 (Facade) - 前端只与本层交互，不直接调 Service

pub mod config;
pub mod dev_log;
pub mod log_store;
pub mod env_inspector;
pub mod python_env;
pub mod core_manager;
pub mod plugin_manager;
pub mod port_diagnostics;
pub mod task_scheduler;
pub mod process_launcher;
pub mod pseudo_terminal;
pub mod system;
pub mod updater;
