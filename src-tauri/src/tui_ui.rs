// SPDX-License-Identifier: AGPL-3.0-or-later
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Paragraph, Wrap},
    Frame,
};

use crate::log_stream::LogEntry;
use crate::tui_app::{EditMode, FocusId, StatusKind, TuiApp};

const ACCENT: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;
const PASS: Color = Color::Green;
const FAIL: Color = Color::Red;
const WARN_COLOR: Color = Color::Yellow;
const SYNC_COLOR: Color = Color::Blue;

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
            Constraint::Min(6),    // main content
            log_height,            // log panel
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
        concat!(" Quip Node Manager v", env!("CARGO_PKG_VERSION"), " "),
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    )
}

fn render_status_section(app: &TuiApp, lines: &mut Vec<Line>) {
    let kind = app.status.kind;
    let (symbol, label) = kind.display();
    let state_color = match kind {
        StatusKind::Running => PASS,
        // Blue, not amber: syncing is expected progress, not a fault.
        StatusKind::Syncing => SYNC_COLOR,
        StatusKind::Degraded | StatusKind::Unhealthy | StatusKind::Partial => WARN_COLOR,
        StatusKind::Stopped => FAIL,
    };
    let state_text = format!("{symbol} {label}");
    let id_part = app
        .status
        .container_id
        .as_deref()
        .unwrap_or("—")
        .to_string();
    // On a Partial stack the service list is the point — it tells the operator
    // the miner is the missing piece — so it takes the slot the image uses.
    let img_part = if kind == StatusKind::Partial || app.status.image.is_empty() {
        app.status.status_text.clone()
    } else {
        shorten_image(&app.status.image)
    };

    // Native mode knows the installed miner version; Docker mode does not,
    // for the reason given on `update::get_node_version`.
    let version_part = app
        .node_version
        .as_ref()
        .map(|v| format!("   miner v{v}"))
        .unwrap_or_default();

    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            state_text,
            Style::default()
                .fg(state_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled(id_part, Style::default().fg(DIM)),
        Span::raw("   "),
        Span::styled(img_part, Style::default().fg(DIM)),
        Span::styled(version_part, Style::default().fg(DIM)),
    ]));

    // The three health dimensions the GUI's panel shows, while the miner is up.
    if let Some(report) = app.health.as_ref().filter(|_| kind.miner_running()) {
        for (name, dim) in [
            ("infra", &report.infra),
            ("chain", &report.chain),
            ("participation", &report.participation),
        ] {
            let (sym, col) = match dim.state {
                crate::health::DimensionState::Ok => ("✓", PASS),
                crate::health::DimensionState::Warn => ("⚠", WARN_COLOR),
                crate::health::DimensionState::Fail => ("✗", FAIL),
                crate::health::DimensionState::Unknown => ("○", DIM),
            };
            lines.push(Line::from(vec![
                Span::raw("     "),
                Span::styled(sym, Style::default().fg(col)),
                Span::raw(format!("  {name:<14} ")),
                Span::styled(dim.detail.clone(), Style::default().fg(DIM)),
            ]));
        }
    }

    // Start / Stop / update buttons
    let start_style = focus_style(app, &FocusId::StartNode);
    let stop_style = focus_style(app, &FocusId::StopNode);
    let mut buttons = vec![
        Span::raw("  "),
        btn_span("[ Start Node ]", start_style),
        Span::raw("   "),
        btn_span("[ Stop Node ]", stop_style),
        Span::raw("   "),
        btn_span(
            if app.update_checking {
                "[ Checking… ]"
            } else {
                "[ Check Updates ]"
            },
            focus_style(app, &FocusId::CheckUpdates),
        ),
    ];
    if app.restart_applies_update() {
        buttons.push(Span::raw("   "));
        buttons.push(btn_span(
            if app.updating {
                "[ Updating… ]"
            } else {
                "[ Update & Restart ]"
            },
            focus_style(app, &FocusId::UpdateRestart),
        ));
    }
    lines.push(Line::from(buttons));

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
    let passed = app
        .checks
        .iter()
        .filter(|c| {
            matches!(
                c.state,
                crate::checklist::CheckState::Pass | crate::checklist::CheckState::Skip
            )
        })
        .count();
    let total = app.checks.len();
    let summary = if total == 0 {
        if app.checklist_running {
            " (running…)".to_string()
        } else {
            String::new()
        }
    } else {
        format!(" {}/{} passing", passed, total)
    };

    lines.push(Line::from(vec![Span::styled(
        format!("  {} Requirements{}", arrow, summary),
        toggle_style,
    )]));

    if app.checklist_expanded {
        for check in &app.checks {
            let (sym, col) = match check.state {
                crate::checklist::CheckState::Pass => ("✓", PASS),
                crate::checklist::CheckState::Skip => ("—", Color::DarkGray),
                crate::checklist::CheckState::Warn => ("⚠", WARN_COLOR),
                crate::checklist::CheckState::Running => ("◌", Color::Cyan),
                crate::checklist::CheckState::Idle => ("○", Color::DarkGray),
                crate::checklist::CheckState::Fail => ("✗", FAIL),
            };
            // Every item retries on its own, like the GUI's Retry button. The
            // Docker install fix names its URL, since a terminal cannot open it.
            let retry = if app.retrying.contains(&check.id) {
                "[ Checking… ]"
            } else {
                "[ Retry ]"
            };
            let mut spans = vec![
                Span::raw("     "),
                Span::styled(sym, Style::default().fg(col)),
                Span::raw(format!("  {}  ", check.label)),
                btn_span(
                    retry,
                    focus_style(app, &FocusId::CheckRetry(check.id.clone())),
                ),
            ];
            if app.check_has_fix_button(check) {
                spans.push(Span::raw("  "));
                spans.push(btn_span(
                    "[ Install Docker ]",
                    focus_style(app, &FocusId::CheckFix(check.id.clone())),
                ));
            }
            lines.push(Line::from(spans));
        }
        let run_style = focus_style(app, &FocusId::RunChecklist);
        let label = if app.checklist_running {
            "[ Running… ]"
        } else {
            "[ Run Checks ]"
        };
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
    let suffix = if !app.config_expanded {
        "  (collapsed)"
    } else {
        ""
    };
    lines.push(Line::from(Span::styled(
        format!("  {} Configuration{}", arrow, suffix),
        toggle_style,
    )));

    if !app.config_expanded {
        return;
    }

    // Storage Directory. Editable so a headless install can pick its location
    // without the GUI's first-boot dialog.
    lines.push(field_line(
        app,
        &FocusId::DataDir,
        "Storage Dir",
        &field_value(app, &FocusId::DataDir, &app.form.data_dir),
    ));
    if app.restart_required {
        lines.push(Line::from(Span::styled(
            "      restart required for the new storage dir",
            Style::default().fg(WARN_COLOR),
        )));
    }

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

    // Update channel (Release grays out until a stable release exists)
    let channels = ["Release", "Beta"];
    let channel_display = channels[app.form.update_channel_idx.min(1)];
    let channel_note = if app.release_channel_available() {
        String::new()
    } else {
        "  (no stable release yet — Beta only)".to_string()
    };
    lines.push(Line::from(vec![
        Span::raw("    "),
        Span::styled(
            format!("{:<16} {}", "Update Channel", channel_display),
            focus_style(app, &FocusId::UpdateChannel),
        ),
        Span::styled(channel_note, Style::default().fg(DIM)),
    ]));

    // TLS: Caddy's certificate settings, shown only while TLS is on.
    let tls_check = if app.form.tls_enabled { "[x]" } else { "[ ]" };
    lines.push(Line::from(vec![
        Span::raw("    "),
        Span::styled(
            format!("{tls_check} Enable TLS"),
            focus_style(app, &FocusId::TlsEnable),
        ),
        Span::styled("  (ports 80 + 443 required)", Style::default().fg(DIM)),
    ]));
    if app.form.tls_enabled {
        lines.push(field_line(
            app,
            &FocusId::TlsHostname,
            "  Hostname",
            &field_value(app, &FocusId::TlsHostname, &app.form.hostname),
        ));
        lines.push(field_line(
            app,
            &FocusId::TlsCertEmail,
            "  ACME Email",
            &field_value(app, &FocusId::TlsCertEmail, &app.form.cert_email),
        ));
        let masked_key = if app.form.zerossl_api_key.is_empty() {
            "(none, Let's Encrypt)".to_string()
        } else {
            "●".repeat(app.form.zerossl_api_key.len().min(32))
        };
        lines.push(field_line(
            app,
            &FocusId::TlsZerosslKey,
            "  ZeroSSL Key",
            &field_value(app, &FocusId::TlsZerosslKey, &masked_key),
        ));
    }

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
        btn_span(
            "[ Regenerate ]",
            focus_style(app, &FocusId::SecretRegenerate),
        ),
    ]));

    // Node Name
    lines.push(field_line(
        app,
        &FocusId::NodeName,
        "Node Name",
        &field_value(app, &FocusId::NodeName, &app.form.node_name),
    ));

    // Solver pickers, one per backend this machine can actually run. Empty
    // means that backend's own default, which is what an operator who has never
    // opened the field is running.
    for backend in app.selectable_solver_backends() {
        let selected = app.form.solver(backend);
        let display = if selected.is_empty() {
            format!("default ({})", backend.default_solver())
        } else {
            let track = app
                .solvers
                .get(&backend)
                .and_then(|list| list.iter().find(|s| s.binary == selected))
                .filter(|s| s.track != crate::solvers::Track::Unknown)
                .map(|s| format!("  ({:?})", s.track).to_lowercase())
                .unwrap_or_default();
            format!("{selected}{track}")
        };
        let label = match backend {
            crate::solvers::Backend::Cpu => "CPU Solver",
            crate::solvers::Backend::Cuda => "CUDA Solver",
            crate::solvers::Backend::Metal => "Metal Solver",
        };
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(
                format!("{label:<16} {display}"),
                focus_style(app, &FocusId::Solver(backend)),
            ),
        ]));
    }

    // Custom Settings toggle
    let cs_arrow = if app.custom_expanded { "▼" } else { "▶" };
    lines.push(Line::from(Span::styled(
        format!("    {} Custom Settings", cs_arrow),
        focus_style(app, &FocusId::CustomToggle),
    )));

    if app.custom_expanded {
        let mut service = "";
        for spec in crate::service_ports::PORT_SPECS {
            if service != spec.service {
                service = spec.service;
                lines.push(Line::from(Span::styled(
                    format!("      -- {service} --"),
                    Style::default().fg(ACCENT),
                )));
            }
            let enabled = app.form.service_port_enabled(spec.id);
            let required = spec.id.required(&app.form.run_mode());
            let check = if enabled { "[x]" } else { "[ ]" };
            let suffix = if required {
                " (required in Native mode)"
            } else {
                ""
            };
            let listener = if spec.id == crate::service_ports::PortId::MinerRest && required {
                "native".to_string()
            } else {
                spec.container_port.to_string()
            };
            lines.push(Line::from(Span::styled(
                format!(
                    "      {check} {} ({}/{}){suffix}",
                    spec.label,
                    listener,
                    spec.protocols.join("+")
                ),
                focus_style(app, &FocusId::ServicePortEnable(spec.id)),
            )));
            if enabled {
                let focus = FocusId::ServicePortNumber(spec.id);
                lines.push(field_line(
                    app,
                    &focus,
                    "  Host port",
                    &field_value(app, &focus, app.form.service_port_value(spec.id)),
                ));
            }
            if spec.id == crate::service_ports::PortId::CaddyAdmin {
                lines.push(Line::from(Span::styled(
                    "      Admin API changes Caddy config. Use a trusted network.",
                    Style::default().fg(DIM),
                )));
            }
            if spec.id == crate::service_ports::PortId::ValidatorRpc && required {
                let public = app.form.service_ports[&spec.id].0;
                let check = if public { "[x]" } else { "[ ]" };
                lines.push(Line::from(Span::styled(
                    format!("      {check} Allow public access (otherwise local only)"),
                    focus_style(app, &FocusId::NativeRpcPublic),
                )));
            }
        }
        let ph_check = if app.form.public_host_enabled {
            "[x]"
        } else {
            "[ ]"
        };
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
            lines.push(Line::from(Span::styled(
                "        Only the address peers dial. To move the listening port, edit Public API above.",
                Style::default().fg(DIM),
            )));
        }
        let log_levels = ["info", "debug", "warn", "error"];
        let ll_display = if log_levels.contains(&app.form.log_level.as_str()) {
            app.form.log_level.clone()
        } else {
            field_value(app, &FocusId::LogLevel, &app.form.log_level)
        };
        lines.push(field_line(
            app,
            &FocusId::LogLevel,
            "  Log Level",
            &ll_display,
        ));

        // Log files
        lines.push(field_line(
            app,
            &FocusId::NodeLog,
            "  Node Log",
            &field_value(
                app,
                &FocusId::NodeLog,
                &if app.form.node_log.is_empty() {
                    "(none)".to_string()
                } else {
                    app.form.node_log.clone()
                },
            ),
        ));
        lines.push(field_line(
            app,
            &FocusId::HttpLog,
            "  HTTP Log",
            &field_value(
                app,
                &FocusId::HttpLog,
                &if app.form.http_log.is_empty() {
                    "(none)".to_string()
                } else {
                    app.form.http_log.clone()
                },
            ),
        ));
    }

    // CPU mining, with its core count while it is on.
    let cpu_check = if app.form.cpu_enabled { "[x]" } else { "[ ]" };
    lines.push(Line::from(vec![
        Span::raw("    "),
        Span::styled(
            format!("{cpu_check} CPU Mining"),
            focus_style(app, &FocusId::CpuEnable),
        ),
    ]));
    if app.form.cpu_enabled {
        lines.push(field_line(
            app,
            &FocusId::CpuCores,
            "  CPU Cores",
            &field_value(app, &FocusId::CpuCores, &app.form.cpu_cores),
        ));
    }

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
                    // Each device carries its own focus id. Sharing one id made
                    // every checkbox after the first inert.
                    focus_style(app, &FocusId::GpuDevice(dev.index)),
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
        // Metal adaptive cap, on the machines whose miner reads it.
        if app.metal_knobs_apply() {
            lines.push(Line::from(Span::styled(
                "      Metal adaptive cap: active only while Yielding is on",
                Style::default().fg(DIM),
            )));
            lines.push(field_line(
                app,
                &FocusId::MetalActiveUtil,
                "  Active util",
                &format!("{}%", app.form.metal_active_util),
            ));
            lines.push(field_line(
                app,
                &FocusId::MetalIdleAfter,
                "  Idle after (s)",
                &field_value(app, &FocusId::MetalIdleAfter, &app.form.metal_idle_after),
            ));
        }
    }

    // D-Wave mining toggle
    let qpu_check = if app.qpu_expanded { "[x]" } else { "[ ]" };
    lines.push(Line::from(vec![
        Span::raw("    "),
        Span::styled(
            format!("{} D-Wave Mining", qpu_check),
            focus_style(app, &FocusId::QpuToggle),
        ),
    ]));
    if app.qpu_expanded {
        lines.push(Line::from(Span::styled(
            "      Runs on CPU miner mode · Advantage2_System1.13 · NA West 1",
            Style::default().fg(DIM),
        )));
        let masked_key = if app.form.qpu_api_key.is_empty() {
            String::new()
        } else {
            format!(
                "{}…",
                &app.form.qpu_api_key[..4.min(app.form.qpu_api_key.len())]
            )
        };
        lines.push(field_line(
            app,
            &FocusId::QpuApiKey,
            "  Token",
            &field_value(app, &FocusId::QpuApiKey, &masked_key),
        ));
        lines.push(field_line(
            app,
            &FocusId::QpuBudget,
            "  Monthly Budget",
            &field_value(app, &FocusId::QpuBudget, &app.form.qpu_budget),
        ));
        lines.push(field_line(
            app,
            &FocusId::QpuBudgetResetDay,
            "  Reset Day (UTC)",
            &field_value(
                app,
                &FocusId::QpuBudgetResetDay,
                &app.form.qpu_budget_reset_day,
            ),
        ));
    }

    // Save / Apply & Restart / Reset Dashboard DB
    let dirty_marker = if app.dirty { " *" } else { "" };
    let reset_label = if app.reset_armed.is_some() {
        "[ Reset Dashboard DB: press again to confirm ]"
    } else {
        "[ Reset Dashboard DB ]"
    };
    lines.push(Line::from(vec![
        Span::raw("    "),
        btn_span(
            &format!("[ Save{} ]", dirty_marker),
            focus_style(app, &FocusId::Save),
        ),
        Span::raw("   "),
        btn_span(
            &format!("[ Apply & Restart{} ]", dirty_marker),
            focus_style(app, &FocusId::ApplyRestart),
        ),
        Span::raw("   "),
        btn_span(reset_label, focus_style(app, &FocusId::ResetDashboardDb)),
    ]));
    lines.push(Line::raw(""));
}

// ─── Log panel (bottom drawer) ────────────────────────────────────────────────

fn render_log_panel(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let inner_height = area.height.saturating_sub(2) as usize; // borders
    let needle = app.log_filter.to_lowercase();
    // Newest first, so the filter walks only as far back as the panel shows.
    let mut lines: Vec<Line> = app
        .log_buf
        .iter()
        .rev()
        .filter(|e| needle.is_empty() || e.message.to_lowercase().contains(&needle))
        .take(inner_height)
        .map(|e| log_line(e))
        .collect();
    lines.reverse();

    let arrow = if app.log_expanded { "▼" } else { "▶" };
    let editing = matches!(&app.edit_mode, EditMode::EditingField(FocusId::LogFilter));
    let filter = if editing {
        format!(" filter: {}█", app.form.edit_buf)
    } else if app.log_filter.is_empty() {
        String::new()
    } else {
        format!(" filter: {}", app.log_filter)
    };
    let title = Span::styled(
        format!(" {} Logs [l]{} ", arrow, filter),
        if editing || app.focus == FocusId::LogFilter {
            Style::default()
                .fg(ACCENT)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        },
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
            Style::default()
                .fg(level_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(entry.message.clone()),
    ])
}

// ─── Footer ───────────────────────────────────────────────────────────────────

fn render_footer(frame: &mut Frame, area: Rect) {
    let para = Paragraph::new(Span::styled(
        " [↑↓/Tab] Move [Enter] Edit [Space] Toggle [PgUp/Dn] Scroll [l] Logs [/] Filter [c] Clear [q] Quit ",
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

fn field_line<'a>(app: &TuiApp, id: &FocusId, label: &str, value: &str) -> Line<'a> {
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

fn field_value(app: &TuiApp, id: &FocusId, current: &str) -> String {
    if matches!(&app.edit_mode, EditMode::EditingField(f) if f == id) {
        app.form.edit_buf.clone()
    } else {
        current.to_string()
    }
}

fn shorten_image(image: &str) -> String {
    // registry.gitlab.com/quip.network/quip-miner/quip-miner-cpu:v0.2
    // → .../quip-miner-cpu:v0.2
    if let Some(slash) = image.rfind('/') {
        format!(".../{}", &image[slash + 1..])
    } else {
        image.to_string()
    }
}
