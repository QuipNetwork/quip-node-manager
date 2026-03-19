// SPDX-License-Identifier: AGPL-3.0-or-later
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};

use crate::tui_app::{Action, EditMode, FocusId, Screen, TuiApp};

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

    match app.screen {
        Screen::Logs => handle_key_logs(app, key),
        Screen::Main => handle_key_main(app, key),
    }
}

fn handle_key_logs(_app: &mut TuiApp, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => Action::ExitLogs,
        _ => Action::None,
    }
}

fn handle_key_main(app: &mut TuiApp, key: KeyEvent) -> Action {
    // Edit mode: route chars to the buffer
    if matches!(app.edit_mode, EditMode::EditingField(_)) {
        return handle_key_edit(app, key);
    }

    match key.code {
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Char('l') => Action::EnterLogs,

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
    match app.focus {
        FocusId::StartNode => Action::StartNode,
        FocusId::StopNode => Action::StopNode,
        FocusId::ChecklistToggle => {
            app.checklist_expanded = !app.checklist_expanded;
            Action::None
        }
        FocusId::RunChecklist => Action::RunChecklist,
        FocusId::CheckPort => Action::CheckPort,
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
        FocusId::ViewLogs => Action::EnterLogs,

        // Checkboxes — toggle on Enter too
        FocusId::AutoMine => {
            app.form.auto_mine = !app.form.auto_mine;
            app.dirty = true;
            Action::None
        }
        FocusId::PublicHostEnable => {
            app.form.public_host_enabled = !app.form.public_host_enabled;
            app.dirty = true;
            Action::None
        }
        FocusId::GpuEnable => {
            app.form.gpu_enabled = !app.form.gpu_enabled;
            app.dirty = true;
            Action::None
        }
        FocusId::GpuYielding => {
            app.form.gpu_yielding = !app.form.gpu_yielding;
            app.dirty = true;
            Action::None
        }

        // GPU Backend — cycle through options
        FocusId::GpuBackend => {
            app.form.gpu_backend_idx = (app.form.gpu_backend_idx + 1) % 3;
            app.dirty = true;
            Action::None
        }

        // GPU Utilization — increase by 10 (wraps at 100)
        FocusId::GpuUtilization => {
            app.form.gpu_utilization =
                if app.form.gpu_utilization >= 100 { 10 } else { app.form.gpu_utilization + 10 };
            app.dirty = true;
            Action::None
        }

        // Image tag — toggle cpu/cuda
        FocusId::ImageTag => {
            app.form.image_tag = if app.form.image_tag == "cpu" {
                "cuda".to_string()
            } else {
                "cpu".to_string()
            };
            app.dirty = true;
            Action::None
        }

        // Verify SSL checkbox
        FocusId::VerifySsl => {
            app.form.verify_ssl = !app.form.verify_ssl;
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
        FocusId::Port
        | FocusId::NodeName
        | FocusId::PublicHostInput
        | FocusId::Peers
        | FocusId::CpuCores
        | FocusId::QpuApiKey
        | FocusId::QpuSolver
        | FocusId::QpuRegionUrl
        | FocusId::QpuDailyBudget
        | FocusId::Timeout
        | FocusId::HeartbeatInterval
        | FocusId::HeartbeatTimeout
        | FocusId::Fanout => {
            start_edit(app);
            Action::None
        }
    }
}

fn toggle_or_activate(app: &mut TuiApp) -> Action {
    match app.focus {
        FocusId::AutoMine => {
            app.form.auto_mine = !app.form.auto_mine;
            app.dirty = true;
            Action::None
        }
        FocusId::PublicHostEnable => {
            app.form.public_host_enabled = !app.form.public_host_enabled;
            app.dirty = true;
            Action::None
        }
        FocusId::GpuEnable => {
            app.form.gpu_enabled = !app.form.gpu_enabled;
            app.dirty = true;
            Action::None
        }
        FocusId::GpuYielding => {
            app.form.gpu_yielding = !app.form.gpu_yielding;
            app.dirty = true;
            Action::None
        }
        FocusId::VerifySsl => {
            app.form.verify_ssl = !app.form.verify_ssl;
            app.dirty = true;
            Action::None
        }
        _ => activate(app),
    }
}

// ─── Edit mode helpers ────────────────────────────────────────────────────────

fn start_edit(app: &mut TuiApp) {
    let current = match &app.focus {
        FocusId::Port => app.form.port.clone(),
        FocusId::NodeName => app.form.node_name.clone(),
        FocusId::PublicHostInput => app.form.public_host.clone(),
        FocusId::Peers => app.form.peers.clone(),
        FocusId::CpuCores => app.form.cpu_cores.clone(),
        FocusId::QpuApiKey => app.form.qpu_api_key.clone(),
        FocusId::QpuSolver => app.form.qpu_solver.clone(),
        FocusId::QpuRegionUrl => app.form.qpu_region_url.clone(),
        FocusId::QpuDailyBudget => app.form.qpu_daily_budget.clone(),
        FocusId::Timeout => app.form.timeout.clone(),
        FocusId::HeartbeatInterval => app.form.heartbeat_interval.clone(),
        FocusId::HeartbeatTimeout => app.form.heartbeat_timeout.clone(),
        FocusId::Fanout => app.form.fanout.clone(),
        _ => return,
    };
    app.form.edit_buf = current;
    app.edit_mode = EditMode::EditingField(app.focus.clone());
}

fn commit_edit(app: &mut TuiApp) {
    let buf = app.form.edit_buf.clone();
    match &app.edit_mode {
        EditMode::EditingField(id) => match id {
            FocusId::Port => app.form.port = buf,
            FocusId::NodeName => app.form.node_name = buf,
            FocusId::PublicHostInput => app.form.public_host = buf,
            FocusId::Peers => app.form.peers = buf,
            FocusId::CpuCores => app.form.cpu_cores = buf,
            FocusId::QpuApiKey => app.form.qpu_api_key = buf,
            FocusId::QpuSolver => app.form.qpu_solver = buf,
            FocusId::QpuRegionUrl => app.form.qpu_region_url = buf,
            FocusId::QpuDailyBudget => app.form.qpu_daily_budget = buf,
            FocusId::Timeout => app.form.timeout = buf,
            FocusId::HeartbeatInterval => app.form.heartbeat_interval = buf,
            FocusId::HeartbeatTimeout => app.form.heartbeat_timeout = buf,
            FocusId::Fanout => app.form.fanout = buf,
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
            if app.screen == Screen::Logs {
                // logs are always tail — no scroll needed
            } else {
                app.scroll_offset = app.scroll_offset.saturating_add(3);
            }
            Action::None
        }
        MouseEventKind::ScrollUp => {
            if app.screen == Screen::Main {
                app.scroll_offset = app.scroll_offset.saturating_sub(3);
            }
            Action::None
        }
        MouseEventKind::Down(MouseButton::Left) => {
            if app.screen == Screen::Logs {
                Action::None
            } else {
                // Without stored hit-test rects, we do basic row-based matching.
                // The content starts at row 1 (inside the outer block border).
                let row = mouse.row.saturating_sub(1) + app.scroll_offset;
                handle_click(app, row)
            }
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
