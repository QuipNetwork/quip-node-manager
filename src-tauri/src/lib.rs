// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod checklist;
pub mod config;
pub mod docker;
pub mod log_stream;
pub mod network;
pub mod secret;
pub mod settings;
pub mod update;

use log_stream::LogStreamState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(LogStreamState::new())
        .invoke_handler(tauri::generate_handler![
            settings::get_settings,
            settings::update_settings,
            secret::get_node_secret,
            secret::generate_node_secret,
            config::generate_config_toml,
            docker::check_docker_installed,
            docker::check_docker_hello_world,
            docker::pull_node_image,
            docker::start_node_container,
            docker::stop_node_container,
            docker::get_container_status,
            docker::get_container_config,
            docker::detect_gpu_backend,
            docker::list_gpu_devices,
            network::detect_public_ip,
            network::check_port_forwarding,
            update::check_app_update,
            update::check_image_update,
            log_stream::start_log_stream,
            log_stream::stop_log_stream,
            checklist::run_checklist,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
