mod auth_webview;
mod commands;
mod state;
mod tray;

use tauri_plugin_autostart::MacosLauncher;

pub use state::{AppSnapshot, DesktopState};

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if cnb_bridge::lifecycle::is_shutdown_for_update(args.iter()) {
                state::exit_with_shutdown(app.clone());
                return;
            }
            tray::show_main_window(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![cnb_bridge::lifecycle::AUTOSTART_ARG]),
        ))
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let lifecycle_mutex = cnb_bridge::lifecycle::AppLifecycleMutex::acquire()
                .map_err(std::io::Error::other)?;
            if cnb_bridge::lifecycle::is_shutdown_for_update(std::env::args()) {
                state::exit_with_shutdown(app.handle().clone());
                return Ok(());
            }
            tray::setup(app)?;
            state::initialize(app, lifecycle_mutex)?;
            if !cnb_bridge::lifecycle::is_autostart(std::env::args()) {
                tray::show_main_window(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() != "main" {
                    return;
                }
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_snapshot,
            commands::dismiss_error,
            commands::save_settings,
            commands::start_login,
            commands::login_with_account,
            commands::logout,
            commands::remove_account,
            commands::start_bridge,
            commands::create_cnb_namespace,
            commands::stop_bridge,
            commands::list_namespaces,
            commands::get_cnb_usage,
            commands::open_external,
            commands::open_logs_folder,
            commands::get_logs,
            commands::list_http_traces,
            commands::open_http_trace,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run CNB Bridge");
}
