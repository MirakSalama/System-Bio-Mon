use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, WindowEvent,
};

fn restore_dashboard(app: &AppHandle) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window("main") {
        window.unminimize()?;
        window.show()?;
        crate::memory::set_idle(&window, false);
        window.set_focus()?;
    }
    Ok(())
}

fn install_icon(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "tray-show", "Open dashboard", true, None::<&str>)?;
    let quit = MenuItem::with_id(
        app,
        "tray-quit",
        "Quit System Bio Mon",
        true,
        None::<&str>,
    )?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    let mut tray = TrayIconBuilder::with_id("monitor")
        .tooltip("System Bio Mon")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|icon, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                let _ = restore_dashboard(icon.app_handle());
            }
        })
        .on_menu_event(|app, event| match event.id().as_ref() {
            "tray-show" => {
                let _ = restore_dashboard(app);
            }
            "tray-quit" => app.exit(0),
            _ => {}
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

pub fn setup(app: &AppHandle) {
    let Some(main) = app.get_webview_window("main") else {
        return;
    };
    // Keep ordinary taskbar minimization if the notification icon could not be created.
    let has_tray = install_icon(app).is_ok();
    let app = app.clone();
    let last_minimized = AtomicBool::new(false);
    main.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            if app.state::<crate::sharing::Sharing>().is_active() {
                api.prevent_close();
                if let Some(window) = app.get_webview_window("main") {
                    if has_tray && window.hide().is_ok() {
                        crate::memory::set_idle(&window, true);
                    } else {
                        let _ = window.minimize();
                    }
                }
            }
            return;
        }
        if !matches!(event, WindowEvent::Resized(_) | WindowEvent::Focused(_)) {
            return;
        }
        let Some(window) = app.get_webview_window("main") else {
            return;
        };
        if let Ok(minimized) = window.is_minimized() {
            if last_minimized.swap(minimized, Ordering::Relaxed) != minimized {
                crate::memory::set_idle(&window, minimized);
                if has_tray && minimized {
                    let _ = window.hide();
                }
            }
        }
    });
}
