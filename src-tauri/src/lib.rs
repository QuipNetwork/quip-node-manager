// SPDX-License-Identifier: AGPL-3.0-or-later

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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(LogStreamState::new())
        .manage(NativeProcessState::new())
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
            // Network & checklist
            network::detect_public_ip,
            checklist::recheck_port_forwarding,
            checklist::run_checklist,
            // Updates
            update::check_app_update,
            update::check_image_update,
            // Log streaming
            log_stream::start_log_stream,
            log_stream::stop_log_stream,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(
                update::background_update_monitor(handle),
            );
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
