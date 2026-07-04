//! 事件总线 - 基于 tokio::sync::broadcast 的多对多消息广播
//!
//! 设计模式：观察者 (Observer)
//! 详见 `PR/02-技术架构.md §8 事件总线`

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// 系统事件枚举
///
/// 各 Service 通过 EventBus.subscribe() 订阅感兴趣的事件
/// 通过 EventBus.emit() 广播
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SystemEvent {
    /// 配置变更（section 标识变更的配置段）
    ConfigChanged { section: String },

    /// torch 安装完成
    TorchInstalled { cuda_version: String },

    /// ComfyUI 核心版本切换
    CoreVersionSwitched {
        from: Option<String>,
        to: String,
    },

    /// 插件列表变更
    PluginListChanged,

    /// extra_model_paths.yaml 重新生成
    YamlRegenerated,

    /// venv 重建完成
    VenvRebuilt,

    /// Python 版本切换完成
    PythonVersionSwitched {
        from: String,
        to: String,
    },

    /// ComfyUI 版本切换后依赖兼容性不匹配
    RequirementsMismatch {
        missing: Vec<String>,
        outdated: Vec<String>,
    },
}

impl SystemEvent {
    /// 事件名称（用于日志）
    pub fn name(&self) -> &'static str {
        match self {
            Self::ConfigChanged { .. } => "ConfigChanged",
            Self::TorchInstalled { .. } => "TorchInstalled",
            Self::CoreVersionSwitched { .. } => "CoreVersionSwitched",
            Self::PluginListChanged => "PluginListChanged",
            Self::YamlRegenerated => "YamlRegenerated",
            Self::VenvRebuilt => "VenvRebuilt",
            Self::PythonVersionSwitched { .. } => "PythonVersionSwitched",
            Self::RequirementsMismatch { .. } => "RequirementsMismatch",
        }
    }
}

/// 事件总线 - 共享 broadcast channel
#[derive(Clone)]
pub struct EventBus {
    sender: broadcast::Sender<SystemEvent>,
}

impl EventBus {
    /// 创建新的事件总线
    ///
    /// capacity: 历史事件缓冲容量（订阅者从 lag 起接收）
    /// 建议 256（足够覆盖短暂离线场景）
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(256);
        Self { sender }
    }

    /// 订阅事件流
    ///
    /// 每个订阅者独立接收所有事件（包括订阅后发出的）
    /// LagTooSlow 错误时自动跳过，记录 warn 日志
    pub fn subscribe(&self) -> broadcast::Receiver<SystemEvent> {
        self.sender.subscribe()
    }

    /// 广播事件
    ///
    /// 无订阅者时静默丢弃（不算错误）
    pub fn emit(&self, event: SystemEvent) {
        let name = event.name();
        if let Err(e) = self.sender.send(event) {
            // 无订阅者时返回 Err，不算错误
            tracing::debug!(event = name, error = %e, "event dropped (no subscribers)");
        } else {
            tracing::debug!(event = name, "event emitted");
        }
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_event_bus_pubsub() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();

        bus.emit(SystemEvent::PluginListChanged);

        let received = rx.recv().await.unwrap();
        assert!(matches!(received, SystemEvent::PluginListChanged));
    }

    #[tokio::test]
    async fn test_multiple_subscribers() {
        let bus = EventBus::new();
        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();

        bus.emit(SystemEvent::YamlRegenerated);

        assert!(matches!(rx1.recv().await.unwrap(), SystemEvent::YamlRegenerated));
        assert!(matches!(rx2.recv().await.unwrap(), SystemEvent::YamlRegenerated));
    }

    #[tokio::test]
    async fn test_no_subscriber_no_panic() {
        let bus = EventBus::new();
        // 无订阅者也不应 panic
        bus.emit(SystemEvent::VenvRebuilt);
    }
}
