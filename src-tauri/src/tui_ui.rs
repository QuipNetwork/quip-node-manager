// SPDX-License-Identifier: AGPL-3.0-or-later
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Paragraph, Wrap},
};

use crate::log_stream::LogEntry;
use crate::tui_app::{EditMode, FocusId, TuiApp};

const ACCENT: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;
const PASS: Color = Color::Green;
const FAIL: Color = Color::Red;
const WARN_COLOR: Color = Color::Yellow;

pub fn render(frame: &mut Frame, app: &mut TuiApp) {
    let area = frame.area();

    // Layout: [main content] [log panel] [footer]
    let log_height = if app.log_expanded {
        Constraint::Percentage(60)
    } else {
        Constraint::Length(7) // 2 border + 5 log lines
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(6),   // main content
            log_height,           // log panel
            Constraint::Length(1), // footer
        ])
        .split(area);

    let content_area = chunks[0];
    let log_area = chunks[1];
    let footer_area = chunks[2];

    render_footer(frame, footer_area);
    render_log_panel(frame, app, log_area);

    // Build all content lines
    let mut lines: Vec<Line> = Vec::new();
    render_status_section(app, &mut lines);
    render_requirements_section(app, &mut lines);
    render_config_section(app, &mut lines);

    let total = lines.len() as u16;
    app.content_height = total;

    // Clamp scroll
    let visible = content_area.height.saturating_sub(2);
    if app.scroll_offset + visible > total {
        app.scroll_offset = total.saturating_sub(visible);
    }

    let text = Text::from(lines);
    let para = Paragraph::new(text)
        .block(Block::bordered().title(title_span()))
        .scroll((app.scroll_offset, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(para, content_area);
}

fn title_span() -> Span<'static> {
    Span::styled(
        " Quip Node Manager v0.1.0 ",
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    )
}

fn render_status_section(app: &TuiApp, lines: &mut Vec<Line>) {
    let (state_color, state_text) = if app.status.running {
        (PASS, "● RUNNING")
    } else {
        (FAIL, "○ STOPPED")
    };
    let id_part = app
        .status
        .container_id
        .as_deref()
        .unwrap_or("—")
        .to_string();
    let img_part = if app.status.image.is_empty() {
        app.status.status_text.clone()
    } else {
        shorten_image(&app.status.image)
    };

    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(state_text, Style::default().fg(state_color).add_modifier(Modifier::BOLD)),
        Span::raw("   "),
        Span::styled(id_part, Style::default().fg(DIM)),
        Span::raw("   "),
        Span::styled(img_part, Style::default().fg(DIM)),
    ]));

    // Start / Stop buttons
    let start_style = focus_style(app, &FocusId::StartNode);
    let stop_style = focus_style(app, &FocusId::StopNode);
    lines.push(Line::from(vec![
        Span::raw("  "),
        btn_span("[ Start Node ]", start_style),
        Span::raw("   "),
        btn_span("[ Stop Node ]", stop_style),
    ]));

    // Status message
    if let Some((msg, _)) = &app.status_message {
        lines.push(Line::from(Span::styled(
            format!("  {}", msg),
            Style::default().fg(WARN_COLOR),
        )));
    } else {
        lines.push(Line::raw(""));
    }
}

fn render_requirements_section(app: &TuiApp, lines: &mut Vec<Line>) {
    let toggle_style = focus_style(app, &FocusId::ChecklistToggle);
    let arrow = if app.checklist_expanded { "▼" } else { "▶" };
    let passed = app.checks.iter().filter(|c| c.passed).count();
    let total = app.checks.len();
    let summary = if total == 0 {
        if app.checklist_running { " (running…)".to_string() } else { String::new() }
    } else {
        format!(" {}/{} passing", passed, total)
    };

    lines.push(Line::from(vec![
        Span::styled(
            format!("  {} Requirements{}", arrow, summary),
            toggle_style,
        ),
    ]));

    if app.checklist_expanded {
        for check in &app.checks {
            let (sym, col) = if check.passed { ("✓", PASS) } else { ("✗", FAIL) };
            if check.id == "port" {
                // Port item gets an inline Recheck button.
                let recheck_style = focus_style(app, &FocusId::CheckPort);
                let btn = if app.port_checking { "[ Checking… ]" } else { "[ Recheck ]" };
                lines.push(Line::from(vec![
                    Span::raw("     "),
                    Span::styled(sym, Style::default().fg(col)),
                    Span::raw(format!("  {}  ", check.label)),
                    btn_span(btn, recheck_style),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::raw("     "),
                    Span::styled(sym, Style::default().fg(col)),
                    Span::raw(format!("  {}", check.label)),
                ]));
            }
        }
        let run_style = focus_style(app, &FocusId::RunChecklist);
        let label = if app.checklist_running { "[ Running… ]" } else { "[ Run Checks ]" };
        lines.push(Line::from(vec![
            Span::raw("     "),
            btn_span(label, run_style),
        ]));
        lines.push(Line::raw(""));
    }
}

fn render_config_section(app: &TuiApp, lines: &mut Vec<Line>) {
    let toggle_style = focus_style(app, &FocusId::ConfigToggle);
    let arrow = if app.config_expanded { "▼" } else { "▶" };
    let suffix = if !app.config_expanded { "  (collapsed)" } else { "" };
    lines.push(Line::from(Span::styled(
        format!("  {} Configuration{}", arrow, suffix),
        toggle_style,
    )));

    if !app.config_expanded {
        return;
    }

    // Storage Directory (read-only info)
    let data_dir = crate::settings::data_dir();
    lines.push(Line::from(vec![
        Span::raw("    "),
        Span::styled(format!("{:<16} ", "Storage Dir"), Style::default().fg(DIM)),
        Span::styled(data_dir.display().to_string(), Style::default().fg(DIM)),
    ]));

    // Run Mode
    let modes = ["Docker", "Native"];
    let mode_display = modes[app.form.run_mode_idx.min(1)];
    lines.push(Line::from(vec![
        Span::raw("    "),
        Span::styled(
            format!("{:<16} {}", "Run Mode", mode_display),
            focus_style(app, &FocusId::RunMode),
        ),
    ]));

    // Port
    lines.push(field_line(
        app, &FocusId::Port, "Port", &field_value(app, &FocusId::Port, &app.form.port),
    ));

    // Listen (read-only)
    lines.push(Line::from(vec![
        Span::raw("    "),
        Span::styled(format!("{:<16} ", "Listen"), Style::default().fg(DIM)),
        Span::styled(
            app.settings.node_config.listen.clone(),
            Style::default().fg(DIM),
        ),
        Span::styled("  (read-only)", Style::default().fg(DIM)),
    ]));

    // Node Secret
    let secret_display = if app.secret_visible {
        app.node_secret.clone()
    } else {
        "●".repeat(app.node_secret.len().min(32))
    };
    lines.push(Line::from(vec![
        Span::raw("    Node Secret   "),
        Span::styled(secret_display, Style::default().fg(DIM)),
        Span::raw("  "),
        btn_span("[ Show ]", focus_style(app, &FocusId::SecretShow)),
        Span::raw("  "),
        btn_span("[ Regenerate ]", focus_style(app, &FocusId::SecretRegenerate)),
    ]));

    // Auto-mine checkbox
    let checked = if app.form.auto_mine { "[x]" } else { "[ ]" };
    lines.push(Line::from(vec![
        Span::raw("    "),
        Span::styled(
            format!("{} Auto-mine", checked),
            focus_style(app, &FocusId::AutoMine),
        ),
    ]));

    // Node Name
    lines.push(field_line(
        app,
        &FocusId::NodeName,
        "Node Name",
        &field_value(app, &FocusId::NodeName, &app.form.node_name),
    ));

    // Custom Settings toggle
    let cs_arrow = if app.custom_expanded { "▼" } else { "▶" };
    lines.push(Line::from(Span::styled(
        format!("    {} Custom Settings", cs_arrow),
        focus_style(app, &FocusId::CustomToggle),
    )));

    if app.custom_expanded {
        let ph_check = if app.form.public_host_enabled { "[x]" } else { "[ ]" };
        lines.push(Line::from(vec![
            Span::raw("      "),
            Span::styled(
                format!("{} Public Host", ph_check),
                focus_style(app, &FocusId::PublicHostEnable),
            ),
        ]));
        if app.form.public_host_enabled {
            lines.push(field_line(
                app,
                &FocusId::PublicHostInput,
                "  Host",
                &field_value(app, &FocusId::PublicHostInput, &app.form.public_host),
            ));
            let port_display = if app.form.public_port.is_empty() {
                "(default)".to_string()
            } else {
                app.form.public_port.clone()
            };
            lines.push(field_line(
                app,
                &FocusId::PublicPortInput,
                "  Port",
                &field_value(app, &FocusId::PublicPortInput, &port_display),
            ));
        }
        lines.push(field_line(
            app,
            &FocusId::Peers,
            "Peers",
            &field_value(app, &FocusId::Peers, &app.form.peers.replace('\n', ", ")),
        ));

        // Advanced fields
        lines.push(field_line(
            app, &FocusId::Timeout, "  Timeout (s)",
            &field_value(app, &FocusId::Timeout, &app.form.timeout),
        ));
        lines.push(field_line(
            app, &FocusId::HeartbeatInterval, "  HB Interval",
            &field_value(app, &FocusId::HeartbeatInterval, &app.form.heartbeat_interval),
        ));
        lines.push(field_line(
            app, &FocusId::HeartbeatTimeout, "  HB Timeout",
            &field_value(app, &FocusId::HeartbeatTimeout, &app.form.heartbeat_timeout),
        ));
        let fanout_display = if app.form.fanout.is_empty() {
            "(default)".to_string()
        } else {
            app.form.fanout.clone()
        };
        lines.push(field_line(
            app, &FocusId::Fanout, "  Fanout",
            &field_value(app, &FocusId::Fanout, &fanout_display),
        ));
        let tls_check = if app.form.verify_tls { "[x]" } else { "[ ]" };
        lines.push(Line::from(vec![
            Span::raw("      "),
            Span::styled(
                format!("{} Verify TLS", tls_check),
                focus_style(app, &FocusId::VerifyTls),
            ),
        ]));
        let log_levels = ["info", "debug", "warn", "error"];
        let ll_display = if log_levels.contains(&app.form.log_level.as_str()) {
            app.form.log_level.clone()
        } else {
            field_value(app, &FocusId::LogLevel, &app.form.log_level)
        };
        lines.push(field_line(
            app, &FocusId::LogLevel, "  Log Level",
            &ll_display,
        ));

        // TLS Certificates
        lines.push(field_line(
            app, &FocusId::TlsCertFile, "  TLS Cert",
            &field_value(app, &FocusId::TlsCertFile,
                &if app.form.tls_cert_file.is_empty() { "(self-signed)".to_string() } else { app.form.tls_cert_file.clone() }),
        ));
        lines.push(field_line(
            app, &FocusId::TlsKeyFile, "  TLS Key",
            &field_value(app, &FocusId::TlsKeyFile,
                &if app.form.tls_key_file.is_empty() { "(self-signed)".to_string() } else { app.form.tls_key_file.clone() }),
        ));

        // REST API
        lines.push(field_line(
            app, &FocusId::RestHost, "  REST Host",
            &field_value(app, &FocusId::RestHost, &app.form.rest_host),
        ));
        lines.push(field_line(
            app, &FocusId::RestPort, "  REST HTTPS",
            &field_value(app, &FocusId::RestPort, &app.form.rest_port),
        ));
        lines.push(field_line(
            app, &FocusId::RestInsecurePort, "  REST HTTP",
            &field_value(app, &FocusId::RestInsecurePort, &app.form.rest_insecure_port),
        ));

        // Telemetry
        let telem_check = if app.form.telemetry_enabled { "[x]" } else { "[ ]" };
        lines.push(Line::from(vec![
            Span::raw("      "),
            Span::styled(
                format!("{} Telemetry", telem_check),
                focus_style(app, &FocusId::TelemetryEnabled),
            ),
        ]));
        if app.form.telemetry_enabled {
            lines.push(field_line(
                app, &FocusId::TelemetryDir, "  Telemetry Dir",
                &field_value(app, &FocusId::TelemetryDir, &app.form.telemetry_dir),
            ));
        }

        // Log files
        lines.push(field_line(
            app, &FocusId::NodeLog, "  Node Log",
            &field_value(app, &FocusId::NodeLog,
                &if app.form.node_log.is_empty() { "(none)".to_string() } else { app.form.node_log.clone() }),
        ));
        lines.push(field_line(
            app, &FocusId::HttpLog, "  HTTP Log",
            &field_value(app, &FocusId::HttpLog,
                &if app.form.http_log.is_empty() { "(none)".to_string() } else { app.form.http_log.clone() }),
        ));

        // Auto-update
        let au_check = if app.form.auto_update { "[x]" } else { "[ ]" };
        lines.push(Line::from(vec![
            Span::raw("      "),
            Span::styled(
                format!("{} Auto-update", au_check),
                focus_style(app, &FocusId::AutoUpdate),
            ),
        ]));
    }

    // CPU Cores
    lines.push(field_line(
        app,
        &FocusId::CpuCores,
        "CPU Cores",
        &field_value(app, &FocusId::CpuCores, &app.form.cpu_cores),
    ));

    // GPU Devices
    let gpu_devices = &app.settings.node_config.gpu_device_configs;
    if gpu_devices.is_empty() {
        lines.push(Line::from(Span::styled(
            "    GPU: No GPUs detected",
            Style::default().fg(DIM),
        )));
    } else {
        for dev in gpu_devices {
            let check = if dev.enabled { "[x]" } else { "[ ]" };
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(
                    format!("{} GPU {}", check, dev.index),
                    focus_style(app, &FocusId::GpuEnable),
                ),
            ]));
        }
        lines.push(field_line(
            app,
            &FocusId::GpuUtilization,
            "  Utilization",
            &format!("{}%", app.form.gpu_utilization),
        ));
        let y_check = if app.form.gpu_yielding { "[x]" } else { "[ ]" };
        lines.push(Line::from(vec![
            Span::raw("      "),
            Span::styled(
                format!("{} Yielding", y_check),
                focus_style(app, &FocusId::GpuYielding),
            ),
        ]));
    }

    // QPU toggle
    let qpu_check = if app.qpu_expanded { "[x]" } else { "[ ]" };
    lines.push(Line::from(vec![
        Span::raw("    "),
        Span::styled(
            format!("{} D-Wave / QPU Access", qpu_check),
            focus_style(app, &FocusId::QpuToggle),
        ),
    ]));
    if app.qpu_expanded {
        lines.push(Line::from(Span::styled(
            "      Solver: Advantage2_System1.13 · Region: NA West 1",
            Style::default().fg(DIM),
        )));
        let masked_key = if app.form.qpu_api_key.is_empty() {
            String::new()
        } else {
            format!("{}…", &app.form.qpu_api_key[..4.min(app.form.qpu_api_key.len())])
        };
        lines.push(field_line(
            app, &FocusId::QpuApiKey, "  Token",
            &field_value(app, &FocusId::QpuApiKey, &masked_key),
        ));
        lines.push(field_line(
            app, &FocusId::QpuDailyBudget, "  Daily Budget",
            &field_value(app, &FocusId::QpuDailyBudget, &app.form.qpu_daily_budget),
        ));
    }

    // Apply & Restart
    let dirty_marker = if app.dirty { " *" } else { "" };
    lines.push(Line::from(vec![
        Span::raw("    "),
        btn_span(
            &format!("[ Apply & Restart{} ]", dirty_marker),
            focus_style(app, &FocusId::ApplyRestart),
        ),
    ]));
    lines.push(Line::raw(""));
}

// ─── Log panel (bottom drawer) ────────────────────────────────────────────────

fn render_log_panel(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let inner_height = area.height.saturating_sub(2) as usize; // borders
    let lines: Vec<Line> = app
        .log_buf
        .iter()
        .rev()
        .take(inner_height)
        .rev()
        .map(|e| log_line(e))
        .collect();

    let arrow = if app.log_expanded { "▼" } else { "▶" };
    let title = Span::styled(
        format!(" {} Logs [l] ", arrow),
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    );
    let block = Block::bordered().title(title);
    let text = Text::from(lines);
    frame.render_widget(Paragraph::new(text).block(block), area);
}

fn log_line(entry: &LogEntry) -> Line<'static> {
    let level_color = match entry.level.as_str() {
        "ERROR" => FAIL,
        "WARN" => WARN_COLOR,
        "INFO" => Color::Cyan,
        "DEBUG" => DIM,
        _ => Color::White,
    };
    let ts = if entry.timestamp.is_empty() {
        String::new()
    } else {
        entry.timestamp.chars().skip(11).take(8).collect::<String>()
    };
    Line::from(vec![
        Span::styled(format!("{:8} ", ts), Style::default().fg(DIM)),
        Span::styled(
            format!("{:<5} ", entry.level),
            Style::default().fg(level_color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(entry.message.clone()),
    ])
}

// ─── Footer ───────────────────────────────────────────────────────────────────

fn render_footer(frame: &mut Frame, area: Rect) {
    let para = Paragraph::new(Span::styled(
        " [↑↓/Tab] Navigate   [Enter] Select/Edit   [Space] Toggle   [l] Logs   [q] Quit ",
        Style::default().fg(Color::Black).bg(ACCENT),
    ));
    frame.render_widget(para, area);
}

// ─── Widget helpers ───────────────────────────────────────────────────────────

fn focus_style(app: &TuiApp, id: &FocusId) -> Style {
    if app.focus == *id {
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    }
}

fn btn_span(label: &str, style: Style) -> Span<'static> {
    Span::styled(label.to_string(), style)
}

fn field_line<'a>(
    app: &TuiApp,
    id: &FocusId,
    label: &str,
    value: &str,
) -> Line<'a> {
    let focused = app.focus == *id;
    let label_style = if focused {
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(DIM)
    };
    let value_style = if focused {
        Style::default().add_modifier(Modifier::UNDERLINED)
    } else {
        Style::default()
    };
    // cursor indicator in edit mode
    let is_editing = matches!(&app.edit_mode, EditMode::EditingField(f) if f == id);
    let display_value = if is_editing {
        format!("{}█", app.form.edit_buf)
    } else {
        value.to_string()
    };
    Line::from(vec![
        Span::raw("    "),
        Span::styled(format!("{:<16} ", label), label_style),
        Span::styled(display_value, value_style),
    ])
}

fn field_value<'a>(app: &TuiApp, id: &FocusId, current: &str) -> String {
    if matches!(&app.edit_mode, EditMode::EditingField(f) if f == id) {
        app.form.edit_buf.clone()
    } else {
        current.to_string()
    }
}

fn shorten_image(image: &str) -> String {
    // registry.gitlab.com/piqued/quip-protocol/quip-network-node-cpu:latest
    // → .../quip-network-node-cpu:latest
    if let Some(slash) = image.rfind('/') {
        format!(".../{}", &image[slash + 1..])
    } else {
        image.to_string()
    }
}
