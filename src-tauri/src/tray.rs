//! 系统托盘模块
//!
//! 详见 `PR/06-界面设计.md §7.8 窗口与托盘行为`
//!
//! 职责：
//! - 创建系统托盘图标 + 右键菜单
//! - 监听菜单点击事件，emit("tray_action") 给前端
//! - 监听双击托盘图标事件，显示/隐藏主窗口
//!
//! 事件协议（emit 给前端）：
//! - `tray_action` payload: `{ "action": "start" | "stop" | "show" | "quit" }`
//!
//! 前端处理：
//! - `src/plugins/tray.ts` 监听 tray_action 事件并分发到 processStore / window
//!
//! 设计模式：
//! - **Observer**：菜单事件 → emit 给前端
//! - **Adapter**：将 Rust 端 tauri::tray 事件转换为前端 tray_action 字符串事件

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, Runtime,
};

/// 托盘菜单项 ID（与前端 action 字符串对应）
const MENU_ID_START: &str = "start";
const MENU_ID_STOP: &str = "stop";
const MENU_ID_SHOW: &str = "show";
const MENU_ID_QUIT: &str = "quit";

/// 主窗口标签（在 tauri.conf.json 中定义）
const MAIN_WINDOW_LABEL: &str = "main";

/// 创建系统托盘 + 菜单
///
/// 在 lib.rs setup 钩子中调用一次。
pub fn setup<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    // 菜单项
    let start = MenuItem::with_id(app, MENU_ID_START, "▶ 启动 ComfyUI", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, MENU_ID_STOP, "⏹ 停止 ComfyUI", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let show = MenuItem::with_id(app, MENU_ID_SHOW, "📋 显示主窗口", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, MENU_ID_QUIT, "🚪 退出", true, None::<&str>)?;

    let menu = Menu::with_items(app, &[&start, &stop, &sep, &show, &quit])?;

    // 托盘图标（使用默认窗口图标）
    let default_icon = app
        .default_window_icon()
        .cloned()
        .ok_or_else(|| tauri::Error::Anyhow(anyhow::anyhow!("default window icon not found")))?;

    let _tray = TrayIconBuilder::with_id("main-tray")
        .icon(default_icon)
        .tooltip("myComfyUI")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(handle_menu_event)
        .on_tray_icon_event(handle_tray_icon_event)
        .build(app)?;

    tracing::info!("system tray initialized");
    Ok(())
}

/// 菜单点击事件处理
fn handle_menu_event<R: Runtime>(app: &AppHandle<R>, event: tauri::menu::MenuEvent) {
    let id = event.id().as_ref();
    tracing::debug!(menu_id = id, "tray menu clicked");

    let action = match id {
        MENU_ID_START => Some("start"),
        MENU_ID_STOP => Some("stop"),
        MENU_ID_SHOW => {
            // 直接显示主窗口，不走前端
            if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
            None
        }
        MENU_ID_QUIT => {
            // 直接退出应用（前端如需做清理工作，应在 onCloseRequested 中处理）
            app.exit(0);
            None
        }
        _ => None,
    };

    if let Some(act) = action {
        let _ = app.emit("tray_action", serde_json::json!({ "action": act }));
    }
}

/// 托盘图标事件处理（双击显示主窗口）
fn handle_tray_icon_event<R: Runtime>(
    _tray: &tauri::tray::TrayIcon<R>,
    event: TrayIconEvent,
) {
    if let TrayIconEvent::Click {
        button: MouseButton::Left,
        button_state: MouseButtonState::Up,
        ..
    } = event
    {
        // 双击托盘图标 → 显示/隐藏主窗口
        let app = _tray.app_handle();
        if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
            // 切换可见性
            match window.is_visible() {
                Ok(true) => {
                    let _ = window.hide();
                }
                _ => {
                    let _ = window.show();
                    let _ = window.unminimize();
                    let _ = window.set_focus();
                }
            }
        }
    }
}
