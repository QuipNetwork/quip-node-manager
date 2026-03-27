// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod cmd;
pub mod checklist;
pub mod config;
pub mod docker;
pub mod hardware;
pub mod log_stream;
pub mod native;
pub mod network;
pub mod secret;
pub mod settings;
pub mod tui_app;
pub mod tui_input;
pub mod tui_ui;
pub mod update;

use log_stream::LogStreamState;
use native::NativeProcessState;
use std::sync::Mutex;
use tauri::image::Image;
use tauri::menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::Manager;

/// Managed state holding the tray icon ID for dynamic updates.
pub struct TrayState {
    pub id: Mutex<Option<tauri::tray::TrayIconId>>,
}

const TRAY_ICON: &[u8] = include_bytes!("../icons/tray-icon.png");
const TRAY_ICON_UPDATE: &[u8] = include_bytes!("../icons/tray-icon-update.png");

/// Swap tray icon between normal and update-available variants.
pub fn set_tray_update(app: &tauri::AppHandle, has_update: bool, tooltip: &str) {
    let state = app.state::<TrayState>();
    let id_guard = state.id.lock().unwrap();
    let Some(id) = id_guard.as_ref() else { return };
    let Some(tray) = app.tray_by_id(id) else { return };

    let icon_bytes = if has_update {
        TRAY_ICON_UPDATE
    } else {
        TRAY_ICON
    };
    if let Ok(img) = Image::from_bytes(icon_bytes) {
        let _ = tray.set_icon(Some(img));
    }
    let _ = tray.set_tooltip(Some(tooltip));
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(LogStreamState::new())
        .manage(NativeProcessState::new())
        .manage(TrayState {
            id: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            settings::get_settings,
            settings::update_settings,
            settings::get_data_dir,
            settings::set_data_dir,
            secret::get_node_secret,
            secret::generate_node_secret,
            config::generate_config_toml,
            // Docker
            docker::check_docker_installed,
            docker::check_docker_hello_world,
            docker::pull_node_image,
            docker::start_node_container,
            docker::stop_node_container,
            docker::get_container_status,
            docker::get_container_config,
            // Hardware
            hardware::detect_gpu_backend,
            hardware::list_gpu_devices,
            hardware::run_hardware_survey,
            // Native
            native::start_native_node,
            native::stop_native_node,
            native::get_native_node_status,
            native::check_native_binary,
            native::download_native_binary,
            native::check_binary_update,
            native::start_native_log_tail,
            // Network & checklist
            network::detect_public_ip,
            checklist::recheck_port_forwarding,
            checklist::run_checklist,
            // Updates
            update::get_app_version,
            update::check_app_update,
            update::check_image_update,
            // Log streaming
            log_stream::start_log_stream,
            log_stream::stop_log_stream,
        ])
        .setup(|app| {
            // ── System tray ──────────────────────────────────────
            let show_i = MenuItemBuilder::with_id("show", "Show Window")
                .build(app)?;
            let start_i = MenuItemBuilder::with_id("start", "Start Node")
                .build(app)?;
            let stop_i = MenuItemBuilder::with_id("stop", "Stop Node")
                .build(app)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let quit_i =
                MenuItemBuilder::with_id("quit", "Quit").build(app)?;
            let menu = MenuBuilder::new(app)
                .items(&[&show_i, &start_i, &stop_i, &sep, &quit_i])
                .build()?;

            let tray = TrayIconBuilder::new()
                .icon(Image::from_bytes(TRAY_ICON)?)
                .tooltip("Quip Node Manager")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| {
                    match event.id().as_ref() {
                        "show" => {
                            if let Some(w) =
                                app.get_webview_window("main")
                            {
                                let _ = w.show();
                                let _ = w.unminimize();
                                let _ = w.set_focus();
                            }
                        }
                        "start" => {
                            let handle = app.clone();
                            tauri::async_runtime::spawn(async move {
                                let settings =
                                    crate::settings::load_settings();
                                let _ = match settings.run_mode {
                                    crate::settings::RunMode::Docker => {
                                        docker::start_node_container(
                                            handle,
                                        )
                                        .await
                                        .map(|_| ())
                                    }
                                    crate::settings::RunMode::Native => {
                                        let state = handle
                                            .state::<NativeProcessState>();
                                        native::start_native_node(
                                            handle.clone(),
                                            state,
                                        )
                                        .await
                                        .map(|_| ())
                                    }
                                };
                            });
                        }
                        "stop" => {
                            let handle = app.clone();
                            tauri::async_runtime::spawn(async move {
                                let settings =
                                    crate::settings::load_settings();
                                let _ = match settings.run_mode {
                                    crate::settings::RunMode::Docker => {
                                        docker::stop_node_container(
                                            handle,
                                        )
                                        .await
                                    }
                                    crate::settings::RunMode::Native => {
                                        let state = handle
                                            .state::<NativeProcessState>();
                                        native::stop_native_node(state)
                                            .await
                                    }
                                };
                            });
                        }
                        "quit" => {
                            app.exit(0);
                        }
                        _ => {}
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(w) =
                            app.get_webview_window("main")
                        {
                            let _ = w.show();
                            let _ = w.unminimize();
                            let _ = w.set_focus();
                        }
                    }
                })
                .build(app)?;

            // Store tray ID for dynamic icon updates
            let tray_state = app.state::<TrayState>();
            *tray_state.id.lock().unwrap() = Some(tray.id().clone());

            // ── Close-to-tray: hide window instead of quitting ───
            let main_window = app.get_webview_window("main").unwrap();
            let w = main_window.clone();
            main_window.on_window_event(move |event| {
                if let tauri::WindowEvent::CloseRequested {
                    api, ..
                } = event
                {
                    api.prevent_close();
                    let _ = w.hide();
                }
            });

            // ── Background update monitor ────────────────────────
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(
                update::background_update_monitor(handle),
            );
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
