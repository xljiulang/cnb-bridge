mod commands;
mod state;
mod tray;

use tauri::Manager;
use tauri_plugin_autostart::MacosLauncher;

pub use state::{AppSnapshot, DesktopState};

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            tray::setup(app)?;
            state::initialize(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                let minimize = app
                    .try_state::<DesktopState>()
                    .map(|state| state.settings().minimize_to_tray)
                    .unwrap_or(true);
                api.prevent_close();
                if minimize {
                    let _ = window.hide();
                } else {
                    state::exit_with_shutdown(app.clone());
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_snapshot,
            commands::save_settings,
            commands::start_login,
            commands::cancel_login,
            commands::logout,
            commands::start_bridge,
            commands::stop_bridge,
            commands::ensure_ssh_key,
            commands::regenerate_ssh_key,
            commands::list_namespaces,
            commands::open_ssh_keys_page,
            commands::open_external,
            commands::open_logs_folder,
            commands::get_logs,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run CNB Bridge");
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
