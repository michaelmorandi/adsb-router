use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{
        Axis, Block, Borders, Chart, Dataset, Gauge, GraphType, Paragraph,
    },
};

use crate::app::{App, ConnectionStatus, HISTORY_LEN};

// Fixed palette for up to 8 sources; cycles if more
const SOURCE_COLORS: [Color; 8] = [
    Color::Cyan,
    Color::Green,
    Color::Magenta,
    Color::Yellow,
    Color::Blue,
    Color::Red,
    Color::LightCyan,
    Color::LightGreen,
];

pub fn draw(f: &mut Frame, app: &App) {
    let n_sources = app.source_order.len().max(1);
    // Each source row: 2 lines (label + gauge) inside the block
    let sources_height = (n_sources as u16 * 2 + 2).max(4);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),              // Header
            Constraint::Length(sources_height), // Per-source throughput
            Constraint::Min(8),                 // Timeseries chart
            Constraint::Length(7),              // Emergency panel
            Constraint::Length(1),              // Footer
        ])
        .split(f.area());

    draw_header(f, chunks[0], app);
    draw_sources(f, chunks[1], app);
    draw_chart(f, chunks[2], app);
    draw_emergencies(f, chunks[3], app);
    draw_footer(f, chunks[4]);
}

// ── Header ─────────────────────────────────────────────────────────────────

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let (conn_style, conn_text) = match &app.connection_status {
        ConnectionStatus::Connecting => (Style::default().fg(Color::Yellow), "● Connecting…"),
        ConnectionStatus::Connected => (Style::default().fg(Color::Green), "● Connected"),
        ConnectionStatus::Error(_) => (Style::default().fg(Color::Red), "● Disconnected"),
    };

    let err_suffix = if let ConnectionStatus::Error(e) = &app.connection_status {
        format!(" ({e})")
    } else {
        String::new()
    };

    let title = Line::from(vec![
        Span::styled(
            " ADS-B Source Monitor ",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::raw("│ "),
        Span::styled(conn_text, conn_style),
        Span::styled(err_suffix, Style::default().fg(Color::Red)),
        Span::raw("   "),
        Span::styled("Planes: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            app.plane_count.to_string(),
            Style::default().fg(Color::White),
        ),
        Span::raw("  "),
        Span::styled("Total frames: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            fmt_large(app.total_frames),
            Style::default().fg(Color::White),
        ),
        Span::raw("  "),
        Span::styled("Uptime: ", Style::default().fg(Color::DarkGray)),
        Span::styled(fmt_uptime(app.uptime_secs), Style::default().fg(Color::White)),
    ]);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(Paragraph::new(title), inner);
}

// ── Per-source throughput gauges ───────────────────────────────────────────

fn draw_sources(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(" Throughput per Source ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.source_order.is_empty() {
        f.render_widget(
            Paragraph::new("Waiting for data…").style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    }

    let max_fps = app.max_fps();

    // Build row constraints: 2 lines per source
    let constraints: Vec<Constraint> = app
        .source_order
        .iter()
        .flat_map(|_| [Constraint::Length(1), Constraint::Length(1)])
        .collect();

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    for (i, addr) in app.source_order.iter().enumerate() {
        let Some(src) = app.sources.get(addr) else {
            continue;
        };
        let color = SOURCE_COLORS[i % SOURCE_COLORS.len()];
        let label_row = rows[i * 2];
        let gauge_row = rows[i * 2 + 1];

        // Label line: ● Name (addr)   fps   total
        let (dot, dot_style) = if src.connected {
            ("● ", Style::default().fg(Color::Green))
        } else {
            ("○ ", Style::default().fg(Color::DarkGray))
        };
        let primary_style = if src.connected {
            Style::default().fg(color).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let addr_hint = if src.name.is_empty() {
            String::new()
        } else {
            format!(" ({})", src.address)
        };
        let status_label = if src.connected {
            format!(
                "  {:>7.1} fps   total {}",
                src.current_fps,
                fmt_large(src.frames_total)
            )
        } else {
            "  disconnected".to_string()
        };

        let label = Line::from(vec![
            Span::styled(dot, dot_style),
            Span::styled(src.label(), primary_style),
            Span::styled(addr_hint, Style::default().fg(Color::DarkGray)),
            Span::styled(status_label, Style::default().fg(Color::Gray)),
        ]);
        f.render_widget(Paragraph::new(label), label_row);

        // Gauge (progress bar showing fps relative to observed max)
        let ratio = if max_fps > 0.0 {
            (src.current_fps / max_fps).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let gauge = Gauge::default()
            .gauge_style(Style::default().fg(color).bg(Color::Black))
            .ratio(ratio)
            .label(""); // label already on the line above
        f.render_widget(gauge, gauge_row);
    }
}

// ── Timeseries chart ───────────────────────────────────────────────────────

fn draw_chart(f: &mut Frame, area: Rect, app: &App) {
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let window = HISTORY_LEN as f64; // seconds of history shown
    let x_min = now_secs - window;
    let x_max = now_secs;

    let max_fps = app.max_fps();
    let y_max = (max_fps * 1.15).max(10.0); // 15% headroom

    // Collect owned data vecs so they outlive the Dataset borrows
    let source_data: Vec<(String, Color, Vec<(f64, f64)>)> = app
        .source_order
        .iter()
        .enumerate()
        .filter_map(|(i, addr)| {
            let src = app.sources.get(addr)?;
            let color = SOURCE_COLORS[i % SOURCE_COLORS.len()];
            let pts: Vec<(f64, f64)> = src.samples.iter().map(|s| (s.time_secs, s.fps)).collect();
            Some((src.label().to_string(), color, pts))
        })
        .collect();

    let datasets: Vec<Dataset> = source_data
        .iter()
        .map(|(name, color, pts)| {
            Dataset::default()
                .name(name.as_str())
                .marker(symbols::Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(*color))
                .data(pts.as_slice())
        })
        .collect();

    let chart = Chart::new(datasets)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(format!(
                    " Frame Rate History — last {}m ",
                    HISTORY_LEN / 60
                )),
        )
        .x_axis(
            Axis::default()
                .bounds([x_min, x_max])
                .labels(vec![
                    Span::styled(
                        format!("{}m ago", HISTORY_LEN / 60),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled("now", Style::default().fg(Color::DarkGray)),
                ]),
        )
        .y_axis(
            Axis::default()
                .bounds([0.0, y_max])
                .labels(vec![
                    Span::styled("0", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        format!("{:.0}", y_max / 2.0),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!("{:.0} fps", y_max),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]),
        );

    f.render_widget(chart, area);
}

// ── Emergency panel ────────────────────────────────────────────────────────

fn draw_emergencies(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(" Emergency Codes — last 72 h ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Count by type
    let mut counts = [0u32; 7]; // indices 1-6 map to proto values 1-6
    for ev in &app.emergencies {
        let idx = ev.status as usize;
        if idx >= 1 && idx <= 6 {
            counts[idx - 1] += 1;
        }
    }

    let labels = [
        ("GENERAL (7700)", counts[0], Color::Red),
        ("NO COMM (7600)", counts[3], Color::Yellow),
        ("HIJACK  (7500)", counts[4], Color::Red),
        ("MEDICAL       ", counts[1], Color::Magenta),
        ("MIN FUEL      ", counts[2], Color::Yellow),
        ("DOWNED        ", counts[5], Color::Red),
    ];

    // Split inner into two vertical chunks: summary counts + recent list
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(2)])
        .split(inner);

    // Summary row
    let count_spans: Vec<Span> = labels
        .iter()
        .flat_map(|(label, count, color)| {
            let style = if *count > 0 {
                Style::default().fg(*color).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            vec![
                Span::styled(format!("{label}: "), Style::default().fg(Color::DarkGray)),
                Span::styled(count.to_string(), style),
                Span::raw("   "),
            ]
        })
        .collect();
    f.render_widget(Paragraph::new(Line::from(count_spans)), chunks[0]);

    // Recent events list (most recent first, up to available lines)
    let avail_lines = chunks[1].height as usize;
    let recent: Vec<Line> = app
        .emergencies
        .iter()
        .rev()
        .take(avail_lines)
        .map(|ev| {
            let ts = fmt_timestamp(ev.timestamp_ms);
            let icao = ev.icao_address.to_uppercase();
            let call = if ev.callsign.is_empty() {
                "—".to_string()
            } else {
                ev.callsign.clone()
            };
            let kind = emergency_label(ev.status);
            let sq = if ev.squawk.is_empty() {
                String::new()
            } else {
                format!("  squawk {}", ev.squawk)
            };
            Line::from(vec![
                Span::styled(format!("{ts}  "), Style::default().fg(Color::DarkGray)),
                Span::styled(icao, Style::default().fg(Color::Cyan)),
                Span::raw(format!(" ({call})  ")),
                Span::styled(
                    kind,
                    Style::default()
                        .fg(Color::Red)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(sq, Style::default().fg(Color::Gray)),
            ])
        })
        .collect();

    if recent.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                "No emergencies observed",
                Style::default().fg(Color::DarkGray),
            )])),
            chunks[1],
        );
    } else {
        f.render_widget(Paragraph::new(recent), chunks[1]);
    }
}

// ── Footer ─────────────────────────────────────────────────────────────────

fn draw_footer(f: &mut Frame, area: Rect) {
    let key = |k: &str| Span::styled(k.to_string(), Style::default().fg(Color::Yellow));
    let desc = |d: &str| Span::styled(d.to_string(), Style::default().fg(Color::DarkGray));
    let text = Line::from(vec![key(" q"), desc(":Quit")]);
    f.render_widget(Paragraph::new(text), area);
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn emergency_label(code: i32) -> &'static str {
    match code {
        1 => "GENERAL EMERGENCY (7700)",
        2 => "MEDICAL EMERGENCY",
        3 => "MINIMUM FUEL",
        4 => "NO COMMUNICATIONS (7600)",
        5 => "HIJACK (7500)",
        6 => "DOWNED AIRCRAFT",
        _ => "UNKNOWN",
    }
}

fn fmt_large(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn fmt_uptime(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}

fn fmt_timestamp(ts_ms: i64) -> String {
    // Very lightweight: just show HH:MM:SS relative to epoch
    let secs = (ts_ms / 1000) as u64;
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}
