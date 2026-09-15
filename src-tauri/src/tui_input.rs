// SPDX-License-Identifier: AGPL-3.0-or-later
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use crate::tui_app::{Action, EditMode, FocusId, TuiApp};

/// Handle a terminal event and return the resulting action.
/// Pure state mutations; async work is done by the caller in `TuiApp::run`.
pub fn handle_event(app: &mut TuiApp, event: Event) -> Action {
    match event {
        Event::Key(key) => handle_key(app, key),
        Event::Mouse(mouse) => handle_mouse(app, mouse),
        _ => Action::None,
    }
}

// ─── Keyboard ─────────────────────────────────────────────────────────────────

fn handle_key(app: &mut TuiApp, key: KeyEvent) -> Action {
    // Ctrl-C always quits
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Action::Quit;
    }

    handle_key_main(app, key)
}

fn handle_key_main(app: &mut TuiApp, key: KeyEvent) -> Action {
    // Edit mode: route chars to the buffer
    if matches!(app.edit_mode, EditMode::EditingField(_)) {
        return handle_key_edit(app, key);
    }

    match key.code {
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Char('l') => Action::ToggleLogs,
        KeyCode::Char('c') => Action::ClearLogs,
        // Jump straight into the log filter from anywhere.
        KeyCode::Char('/') => {
            app.focus = FocusId::LogFilter;
            start_edit(app);
            Action::None
        }

        // Navigation
        KeyCode::Up | KeyCode::BackTab => {
            app.prev_focus();
            Action::None
        }
        KeyCode::Down | KeyCode::Tab => {
            app.next_focus();
            Action::None
        }

        // Scroll the main view
        KeyCode::PageUp => {
            app.scroll_offset = app.scroll_offset.saturating_sub(5);
            Action::None
        }
        KeyCode::PageDown => {
            app.scroll_offset = app.scroll_offset.saturating_add(5);
            Action::None
        }

        KeyCode::Enter => activate(app),
        KeyCode::Char(' ') => toggle_or_activate(app),

        _ => Action::None,
    }
}

fn handle_key_edit(app: &mut TuiApp, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Esc => {
            // Cancel — discard buffer
            app.form.edit_buf.clear();
            app.edit_mode = EditMode::None;
            Action::None
        }
        KeyCode::Enter => {
            // Confirm — apply buffer to the field
            commit_edit(app);
            Action::None
        }
        KeyCode::Backspace => {
            app.form.edit_buf.pop();
            Action::None
        }
        KeyCode::Char(c) => {
            app.form.edit_buf.push(c);
            Action::None
        }
        _ => Action::None,
    }
}

// ─── Activation ───────────────────────────────────────────────────────────────

fn activate(app: &mut TuiApp) -> Action {
    // Cloned: the check ids carry a String, so matching the field in place
    // would move out of it.
    let focus = app.focus.clone();
    match focus {
        FocusId::StartNode => Action::StartNode,
        FocusId::StopNode => Action::StopNode,
        FocusId::CheckUpdates => Action::CheckUpdates,
        FocusId::UpdateRestart => Action::UpdateRestart,
        FocusId::ChecklistToggle => {
            app.checklist_expanded = !app.checklist_expanded;
            Action::None
        }
        FocusId::RunChecklist => Action::RunChecklist,
        FocusId::CheckRetry(id) => Action::RetryCheck(id),
        FocusId::CheckFix(id) => Action::FixCheck(id),
        FocusId::Save => Action::Save,
        FocusId::ResetDashboardDb => Action::ResetDashboardDb,
        FocusId::ConfigToggle => {
            app.config_expanded = !app.config_expanded;
            Action::None
        }
        FocusId::CustomToggle => {
            app.custom_expanded = !app.custom_expanded;
            Action::None
        }
        FocusId::QpuToggle => {
            app.qpu_expanded = !app.qpu_expanded;
            Action::None
        }
        FocusId::SecretShow => Action::ToggleSecretVisible,
        FocusId::SecretRegenerate => Action::RegenerateSecret,
        FocusId::ApplyRestart => Action::ApplyRestart,

        // Solver pickers — cycle the default plus whatever that backend ships.
        // A successful read is kept. A failed read offers only the current
        // selection for this press, says why in the status line, and is read
        // again on the next press.
        FocusId::Solver(backend) => {
            let available = match app.solvers.get(&backend) {
                Some(cached) => cached.clone(),
                None => {
                    let catalog = tokio::runtime::Runtime::new()
                        .ok()
                        .map(|rt| rt.block_on(crate::solvers::list_solvers(backend)));
                    match catalog {
                        Some(c) => {
                            if let Some(reason) = &c.unavailable {
                                app.set_status(format!("could not list solvers: {reason}"));
                            }
                            if let Some(solvers) = crate::tui_app::cacheable_solvers(&c) {
                                app.solvers.insert(backend, solvers);
                            }
                            c.solvers
                        }
                        None => Vec::new(),
                    }
                }
            };
            let next = crate::tui_app::next_solver(app.form.solver(backend), &available);
            app.form.set_solver(backend, next);
            app.dirty = true;
            Action::None
        }

        // Run Mode — cycle Docker/Native
        FocusId::RunMode => {
            if cfg!(target_os = "macos") {
                app.form.run_mode_idx = (app.form.run_mode_idx + 1) % 2;
            }
            app.dirty = true;
            Action::None
        }

        // Update Channel — cycle Release/Beta. When no stable release exists
        // Release isn't selectable, so lock to Beta (mirrors the web gray-out).
        FocusId::UpdateChannel => {
            app.form.update_channel_idx = if app.release_channel_available() {
                (app.form.update_channel_idx + 1) % 2
            } else {
                1 // Beta
            };
            app.dirty = true;
            Action::None
        }

        // Checkboxes — toggle on Enter too
        FocusId::NativeRpcPublic => {
            if let Some(binding) = app
                .form
                .service_ports
                .get_mut(&crate::service_ports::PortId::ValidatorRpc)
            {
                binding.0 = !binding.0;
            }
            app.dirty = true;
            Action::None
        }
        FocusId::ServicePortEnable(id) => {
            app.form.toggle_service_port(id);
            app.dirty = true;
            Action::None
        }
        FocusId::PublicHostEnable => {
            app.form.public_host_enabled = !app.form.public_host_enabled;
            app.dirty = true;
            Action::None
        }
        FocusId::TlsEnable => {
            app.form.tls_enabled = !app.form.tls_enabled;
            app.dirty = true;
            Action::None
        }
        FocusId::CpuEnable => {
            app.form.cpu_enabled = !app.form.cpu_enabled;
            app.dirty = true;
            Action::None
        }
        // Same stepping as the utilization cap, and the same 1-100 range.
        FocusId::MetalActiveUtil => {
            app.form.metal_active_util = if app.form.metal_active_util >= 100 {
                10
            } else {
                app.form.metal_active_util + 10
            };
            app.dirty = true;
            Action::None
        }
        FocusId::GpuDevice(index) => {
            crate::tui_app::toggle_gpu_device(
                &mut app.settings.node_config.gpu_device_configs,
                index,
            );
            app.dirty = true;
            Action::None
        }
        FocusId::GpuYielding => {
            app.form.gpu_yielding = !app.form.gpu_yielding;
            app.dirty = true;
            Action::None
        }
        // GPU Utilization — increase by 10 (wraps at 100)
        FocusId::GpuUtilization => {
            app.form.gpu_utilization = if app.form.gpu_utilization >= 100 {
                10
            } else {
                app.form.gpu_utilization + 10
            };
            app.dirty = true;
            Action::None
        }

        // Log Level — cycle through levels
        FocusId::LogLevel => {
            let levels = ["info", "debug", "warn", "error"];
            let next = levels
                .iter()
                .position(|&l| l == app.form.log_level.as_str())
                .map(|i| levels[(i + 1) % levels.len()])
                .unwrap_or("info");
            app.form.log_level = next.to_string();
            app.dirty = true;
            Action::None
        }

        // Text fields — enter edit mode
        FocusId::DataDir
        | FocusId::ServicePortNumber(_)
        | FocusId::NodeName
        | FocusId::TlsHostname
        | FocusId::TlsCertEmail
        | FocusId::TlsZerosslKey
        | FocusId::PublicHostInput
        | FocusId::PublicPortInput
        | FocusId::CpuCores
        | FocusId::MetalIdleAfter
        | FocusId::QpuApiKey
        | FocusId::QpuBudget
        | FocusId::QpuBudgetResetDay
        | FocusId::NodeLog
        | FocusId::HttpLog
        | FocusId::LogFilter => {
            start_edit(app);
            Action::None
        }
    }
}

/// Space behaves like Enter everywhere: `activate` already toggles the
/// checkboxes and cycles the selectors.
fn toggle_or_activate(app: &mut TuiApp) -> Action {
    activate(app)
}

// ─── Edit mode helpers ────────────────────────────────────────────────────────

fn start_edit(app: &mut TuiApp) {
    let current = match &app.focus {
        FocusId::DataDir => app.form.data_dir.clone(),
        FocusId::ServicePortNumber(id) => app.form.service_port_value(*id).to_string(),
        FocusId::NodeName => app.form.node_name.clone(),
        FocusId::TlsHostname => app.form.hostname.clone(),
        FocusId::TlsCertEmail => app.form.cert_email.clone(),
        FocusId::TlsZerosslKey => app.form.zerossl_api_key.clone(),
        FocusId::PublicHostInput => app.form.public_host.clone(),
        FocusId::PublicPortInput => app.form.public_port.clone(),
        FocusId::CpuCores => app.form.cpu_cores.clone(),
        FocusId::MetalIdleAfter => app.form.metal_idle_after.clone(),
        FocusId::QpuApiKey => app.form.qpu_api_key.clone(),
        FocusId::QpuBudget => app.form.qpu_budget.clone(),
        FocusId::QpuBudgetResetDay => app.form.qpu_budget_reset_day.clone(),
        FocusId::NodeLog => app.form.node_log.clone(),
        FocusId::HttpLog => app.form.http_log.clone(),
        FocusId::LogFilter => app.log_filter.clone(),
        _ => return,
    };
    app.form.edit_buf = current;
    app.edit_mode = EditMode::EditingField(app.focus.clone());
}

fn commit_edit(app: &mut TuiApp) {
    let buf = app.form.edit_buf.clone();
    if let EditMode::EditingField(FocusId::ServicePortNumber(_)) = &app.edit_mode {
        if buf.parse::<u16>().ok().filter(|port| *port > 0).is_none() {
            app.set_status("Choose a host port from 1 to 65535");
            return;
        }
    }
    // The log filter is view state, not a setting: it never dirties the form.
    if let EditMode::EditingField(FocusId::LogFilter) = &app.edit_mode {
        app.log_filter = buf;
        app.form.edit_buf.clear();
        app.edit_mode = EditMode::None;
        return;
    }
    match &app.edit_mode {
        EditMode::EditingField(id) => match id {
            FocusId::DataDir => app.form.data_dir = buf,
            FocusId::ServicePortNumber(id) => app.form.set_service_port_value(*id, buf),
            FocusId::NodeName => app.form.node_name = buf,
            FocusId::TlsHostname => app.form.hostname = buf,
            FocusId::TlsCertEmail => app.form.cert_email = buf,
            FocusId::TlsZerosslKey => app.form.zerossl_api_key = buf,
            FocusId::PublicHostInput => app.form.public_host = buf,
            FocusId::PublicPortInput => app.form.public_port = buf,
            FocusId::CpuCores => app.form.cpu_cores = buf,
            FocusId::MetalIdleAfter => app.form.metal_idle_after = buf,
            FocusId::QpuApiKey => app.form.qpu_api_key = buf,
            FocusId::QpuBudget => app.form.qpu_budget = buf,
            FocusId::QpuBudgetResetDay => app.form.qpu_budget_reset_day = buf,
            FocusId::NodeLog => app.form.node_log = buf,
            FocusId::HttpLog => app.form.http_log = buf,
            _ => {}
        },
        EditMode::None => {}
    }
    app.dirty = true;
    app.form.edit_buf.clear();
    app.edit_mode = EditMode::None;
}

// ─── Mouse ────────────────────────────────────────────────────────────────────

fn handle_mouse(app: &mut TuiApp, mouse: MouseEvent) -> Action {
    match mouse.kind {
        MouseEventKind::ScrollDown => {
            app.scroll_offset = app.scroll_offset.saturating_add(3);
            Action::None
        }
        MouseEventKind::ScrollUp => {
            app.scroll_offset = app.scroll_offset.saturating_sub(3);
            Action::None
        }
        MouseEventKind::Down(MouseButton::Left) => {
            let row = mouse.row.saturating_sub(1) + app.scroll_offset;
            handle_click(app, row)
        }
        _ => Action::None,
    }
}

/// Row-based click handling for the main view.
/// Rows are estimated by counting lines in order (same order as render_main).
fn handle_click(_app: &mut TuiApp, row: u16) -> Action {
    // Row 0: status line — ignore
    // Row 1: Start/Stop buttons
    // Row 2: status message / empty
    if row == 1 {
        // Estimate column split: Start ~col 2-17, Stop ~col 21-35
        // Without column info we can't distinguish, so just focus the area
        return Action::None;
    }
    // For simplicity, clicking anywhere just does nothing beyond mouse scroll.
    // Full hit-test requires storing Rect per FocusId from render (see plan).
    Action::None
}
