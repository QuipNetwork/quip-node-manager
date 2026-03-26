// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Single binary: GUI by default, TUI when --cli flag is passed or no
// display is available (SSH, headless server, etc.).
//
// On Windows the `windows_subsystem = "windows"` attribute is set at
// compile time to hide the console for GUI mode. For TUI mode we
// re-attach the console at runtime.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let force_cli = args.iter().any(|a| a == "--cli" || a == "cli");

    if force_cli || !has_display() {
        run_tui();
    } else {
        quip_node_manager_lib::run();
    }
}

/// Check if a graphical display is available.
fn has_display() -> bool {
    #[cfg(target_os = "macos")]
    {
        // macOS: GUI is available unless we're in an SSH session
        // with no display forwarding.
        std::env::var("SSH_TTY").is_err()
            || std::env::var("DISPLAY").is_ok()
    }

    #[cfg(target_os = "linux")]
    {
        // Linux: need DISPLAY (X11) or WAYLAND_DISPLAY
        std::env::var("DISPLAY").is_ok()
            || std::env::var("WAYLAND_DISPLAY").is_ok()
    }

    #[cfg(target_os = "windows")]
    {
        // Windows: GUI is almost always available.
        // Detect console-only via GetConsoleWindow.
        true
    }

    #[cfg(not(any(
        target_os = "macos",
        target_os = "linux",
        target_os = "windows"
    )))]
    {
        false
    }
}

fn run_tui() {
    use crossterm::{
        event::{DisableMouseCapture, EnableMouseCapture},
        execute,
        terminal::{
            EnterAlternateScreen, LeaveAlternateScreen,
            disable_raw_mode, enable_raw_mode,
        },
    };
    use ratatui::{Terminal, backend::CrosstermBackend};
    use quip_node_manager_lib::tui_app::TuiApp;
    use std::panic;

    // Re-attach console on Windows (needed when built with
    // windows_subsystem = "windows").
    #[cfg(target_os = "windows")]
    {
        unsafe { winapi::um::wincon::AttachConsole(u32::MAX); }
    }

    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture
        );
        default_hook(info);
    }));

    let setup = || -> io::Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        let mut app = TuiApp::new();
        let result = app.run(&mut terminal);
        disable_raw_mode()?;
        execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        result
    };

    if let Err(e) = setup() {
        eprintln!("TUI error: {}", e);
        std::process::exit(1);
    }
}
