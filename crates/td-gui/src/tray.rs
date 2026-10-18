//! 托盘图标。
//!
//! 托盘提供两个"不用打开窗口就能做"的动作：
//! * 立即执行开机重建（用户手动补一次，比如 RAM 盘是登录后才挂上的）；
//! * 双击图标或通过菜单显示主窗口 / 退出。
//!
//! 重建动作放到后台线程跑：托盘事件是在主线程派发的，在这里做耗时 IO 会把界面卡死。

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};

use crate::commands::{self, AppState};

/// 托盘动作完成后广播给前端的事件名。
pub const EV_TRAY_BOOT: &str = "td-tray-boot";

pub fn install(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let boot = MenuItem::with_id(app, "boot", "立即执行开机重建", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &boot, &quit])?;

    let mut builder = TrayIconBuilder::with_id("main-tray")
        .tooltip("TempDirector")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => focus_main(app),
            "boot" => spawn_boot(app.clone()),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } = event
            {
                focus_main(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }
    builder.build(app)?;
    Ok(())
}

fn focus_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn spawn_boot(app: AppHandle) {
    focus_main(&app);
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        let payload = match commands::lock_store(&state) {
            Ok(store) => match crate::api::boot_rebuild(&store) {
                Ok(report) => serde_json::json!({
                    "ok": !report.has_problem(),
                    "message": report.one_line(),
                    "warnings": report.warnings,
                    "failed": report.failed,
                }),
                Err(e) => serde_json::json!({ "ok": false, "message": e.to_string(), "warnings": [], "failed": [] }),
            },
            Err(e) => {
                serde_json::json!({ "ok": false, "message": e, "warnings": [], "failed": [] })
            }
        };
        let _ = app.emit(EV_TRAY_BOOT, payload);
    });
}
