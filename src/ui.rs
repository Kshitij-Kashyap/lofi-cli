use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Wrap},
    Frame,
};

use crate::app::{fmt_elapsed, App, ConnectionState, Palette, PlayerState, VisMode};

fn status_label(app: &App) -> (String, Color) {
    match app.player_state {
        PlayerState::Playing => ("● PLAYING".to_string(), Color::Green),
        PlayerState::Paused => ("❚❚ PAUSED".to_string(), Color::Yellow),
        PlayerState::Connecting => ("… CONNECTING".to_string(), Color::Cyan),
        PlayerState::Buffering => ("… BUFFERING".to_string(), Color::Cyan),
        PlayerState::Reconnecting => (
            format!("… RECONNECTING (try {})", app.reconnect_attempt.max(1)),
            Color::Yellow,
        ),
        PlayerState::Error => ("✖ ERROR".to_string(), Color::Red),
        PlayerState::Stopped => ("○ STOPPED".to_string(), Color::DarkGray),
    }
}

fn connection_label(conn: ConnectionState) -> (String, Color) {
    match conn {
        ConnectionState::Connected => ("connected".to_string(), Color::Green),
        ConnectionState::Connecting => ("connecting…".to_string(), Color::Cyan),
        ConnectionState::Reconnecting => ("reconnecting…".to_string(), Color::Yellow),
        ConnectionState::Failed => ("failed".to_string(), Color::Red),
        ConnectionState::Disconnected => ("disconnected".to_string(), Color::DarkGray),
    }
}

/// Render the full TUI. Degrades gracefully on small terminals:
/// below 60x12 we show a compact single-column view instead of overflowing.
pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    if area.width < 60 || area.height < 12 {
        render_compact(frame, app);
        return;
    }

    let vis_on = app.vis != VisMode::Off;
    let chrome = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(if vis_on { 10 } else { 0 }),
            Constraint::Length(3),
        ])
        .split(area);
    let (header_area, main_area, vis_area, hints_area) =
        (chrome[0], chrome[1], chrome[2], chrome[3]);

    // Header: title left, session timer right, borders intact.
    let inner_w = header_area.width.saturating_sub(2) as usize;
    let left = "  LOFI RADIO";
    let right = format!("Session {} ", fmt_elapsed(app.elapsed()));
    let pad = inner_w
        .saturating_sub(left.len() + right.len())
        .max(1);
    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            left.to_string(),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" ".repeat(pad)),
        Span::styled(right, Style::default().fg(Color::DarkGray)),
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(header, header_area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(main_area);

    // --- stations pane ---
    let items: Vec<ListItem> = app
        .stations
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let playing = Some(i) == app.current;
            let marker = if i == app.selected {
                "> "
            } else {
                "  "
            };
            let name = if playing {
                format!("{marker}♪ {}", s.name)
            } else {
                format!("{marker}{}", s.name)
            };
            let style = if i == app.selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else if playing {
                Style::default().fg(Color::Green)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(vec![
                Span::styled(name, style),
                Span::styled(
                    format!("  {}", s.description),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();
    let stations =
        List::new(items).block(Block::default().borders(Borders::ALL).title(" STATIONS "));
    frame.render_widget(stations, cols[0]);

    // --- now playing pane ---
    let (status_text, status_color) = status_label(app);
    let (conn_text, conn_color) = connection_label(app.connection);
    let track = app.current_title.as_deref().unwrap_or_else(|| {
        app.current_station()
            .map(|s| s.name.as_str())
            .unwrap_or("—")
    });
    let artist_fallback = if app.current_title.is_some() {
        app.current_station()
            .map(|s| s.name.as_str())
            .unwrap_or("Live Radio")
    } else {
        "Live Radio"
    };

    let vol_pct = (app.effective_volume() * 100.0).round() as u16;
    let vol_line = if app.muted {
        format!("Volume: muted (M to unmute)")
    } else {
        format!("Volume: {vol_pct}%")
    };

    let mut lines = vec![
        Line::from(Span::styled(
            "NOW PLAYING",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            track.to_string(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            artist_fallback.to_string(),
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
        Line::from(Span::styled(
            status_text,
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled("net: ", Style::default().fg(Color::DarkGray)),
            Span::styled(conn_text, Style::default().fg(conn_color)),
        ]),
        Line::from(""),
        Line::from(vol_line),
    ];
    if let Some(msg) = &app.status_message {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            msg.clone(),
            Style::default().fg(Color::Yellow),
        )));
    }
    match app.audio_available {
        Some(false) => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "No audio device — showing stream info only.",
                Style::default().fg(Color::Yellow),
            )));
        }
        None => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Silent build — rebuild with --features audio for sound.",
                Style::default().fg(Color::DarkGray),
            )));
        }
        Some(true) => {}
    }

    let now = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(" NOW PLAYING "))
        .wrap(Wrap { trim: true });
    frame.render_widget(now, cols[1]);

    // Volume gauge row inside right pane bottom: render as separate widget if space.
    let _ = Gauge::default();

    // --- visualizer strip (V cycles mode, C cycles palette) ---
    if vis_on {
        render_visualizer(frame, vis_area, app);
    }

    let hints = Paragraph::new(Line::from(vec![
        Span::styled(" ↑↓ Select  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Enter Play  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Space Pause  ", Style::default().fg(Color::DarkGray)),
        Span::styled("←→ Volume  ", Style::default().fg(Color::DarkGray)),
        Span::styled("M Mute  ", Style::default().fg(Color::DarkGray)),
        Span::styled("R Reconnect  ", Style::default().fg(Color::DarkGray)),
        Span::styled("V Visual  ", Style::default().fg(Color::DarkGray)),
        Span::styled("C Color  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Q Quit", Style::default().fg(Color::DarkGray)),
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(hints, hints_area);
}

/// Bar color for a row at height fraction `frac` (0 = bottom, 1 = top).
fn bar_color(palette: Palette, frac: f32) -> Color {
    match palette {
        Palette::Neon => {
            if frac > 0.75 {
                Color::Magenta
            } else if frac > 0.45 {
                Color::Blue
            } else {
                Color::Cyan
            }
        }
        Palette::Ember => {
            if frac > 0.7 {
                Color::Yellow
            } else if frac > 0.4 {
                Color::LightRed
            } else {
                Color::Red
            }
        }
        Palette::Forest => {
            if frac > 0.7 {
                Color::Yellow
            } else if frac > 0.4 {
                Color::LightGreen
            } else {
                Color::Green
            }
        }
        Palette::Mono => {
            if frac > 0.6 {
                Color::White
            } else {
                Color::Gray
            }
        }
    }
}

/// Resample `values` to exactly `want` columns (average groups when
/// shrinking, nearest-neighbor when stretching).
fn resample(values: &[f32], want: usize) -> Vec<f32> {
    let want = want.max(1);
    if values.is_empty() {
        return vec![0.0; want];
    }
    if values.len() == want {
        return values.to_vec();
    }
    (0..want)
        .map(|i| {
            if want >= values.len() {
                values[i * values.len() / want]
            } else {
                let a = i * values.len() / want;
                let b = ((i + 1) * values.len() / want).max(a + 1);
                values[a..b.min(values.len())]
                    .iter()
                    .sum::<f32>()
                    / (b - a).max(1) as f32
            }
        })
        .collect()
}

fn render_visualizer(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let block = Block::default().borders(Borders::ALL).title(format!(
        " VISUALIZER {} {} [V mode] [C color] ",
        app.vis.name(),
        app.palette.name()
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let (bands, wave) = app.display_levels();
    match app.vis {
        VisMode::Bars => render_bars(frame, inner, app.palette, &bands),
        VisMode::Wave => render_wave(frame, inner, app.palette, &wave),
        VisMode::Off => {}
    }
}

/// Bottom-up spectrum bars, 2 cells wide with a 1-cell gap.
fn render_bars(frame: &mut Frame, area: ratatui::layout::Rect, palette: Palette, bands: &[f32]) {
    let rows = area.height as usize;
    let w = area.width as usize;
    if rows == 0 || w < 3 {
        return;
    }
    let want = (w / 3).clamp(4, 64).max(1);
    let cols = resample(bands, want);
    for r in 0..rows {
        // Row fraction measured at the row's vertical center.
        let frac = (rows - r) as f32 / rows as f32 - 0.5 / rows as f32;
        let mut spans = Vec::with_capacity(cols.len() * 2);
        let mut used = 0usize;
        for v in &cols {
            if used + 3 > w {
                break;
            }
            let cell = if *v >= frac.max(0.02) { "██" } else { "  " };
            spans.push(Span::styled(
                cell,
                Style::default().fg(bar_color(palette, frac)),
            ));
            spans.push(Span::raw(" "));
            used += 3;
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            ratatui::layout::Rect::new(area.x, area.y + r as u16, area.width, 1),
        );
    }
}

/// Oscilloscope waveform with a faint center axis.
fn render_wave(frame: &mut Frame, area: ratatui::layout::Rect, palette: Palette, wave: &[f32]) {
    let rows = area.height as usize;
    let w = area.width as usize;
    if rows == 0 || w == 0 {
        return;
    }
    let pts = resample(wave, w);
    let mid = rows / 2;
    let wave_color = match palette {
        Palette::Neon => Color::Cyan,
        Palette::Ember => Color::Yellow,
        Palette::Forest => Color::LightGreen,
        Palette::Mono => Color::White,
    };
    let style = Style::default().fg(wave_color);
    for r in 0..rows {
        let mut line = String::with_capacity(w);
        for c in 0..w {
            let v = pts.get(c).copied().unwrap_or(0.0).clamp(-1.0, 1.0);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let y = (mid as f32 - v * (mid as f32 - 0.5)).round() as usize;
            let y = y.min(rows - 1);
            if y == r {
                line.push('●');
            } else if r == mid {
                line.push('·');
            } else {
                line.push(' ');
            }
        }
        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(line, style)])),
            ratatui::layout::Rect::new(area.x, area.y + r as u16, area.width, 1),
        );
    }
}

fn render_compact(frame: &mut Frame, app: &App) {
    let (status_text, _) = status_label(app);
    let name = app
        .selected_station()
        .map(|s| s.name.as_str())
        .unwrap_or("—");
    let track = app.current_title.as_deref().unwrap_or(name);
    let text = vec![
        Line::from("LOFI RADIO"),
        Line::from(format!("{track} [{status_text}]")),
        Line::from("↑↓ Enter Space ←→ M R Q"),
    ];
    if let Some(msg) = &app.status_message {
        let mut text = text;
        text.push(Line::from(msg.clone()));
        frame.render_widget(Paragraph::new(text), frame.area());
    } else {
        frame.render_widget(Paragraph::new(text), frame.area());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn renders_full_layout_without_panic() {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let app = App::new(&Config::default());
        terminal.draw(|f| render(f, &app)).unwrap();
    }

    #[test]
    fn renders_compact_layout_on_small_terminal() {
        let backend = TestBackend::new(50, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        let app = App::new(&Config::default());
        terminal.draw(|f| render(f, &app)).unwrap();
    }

    #[test]
    fn renders_wide_terminal_without_panic() {
        let backend = TestBackend::new(250, 60);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(&Config::default());
        app.audio_available = Some(false);
        app.on_metadata("Kick Bong - Flying Crew".to_string());
        terminal.draw(|f| render(f, &app)).unwrap();
    }

    #[test]
    fn renders_all_vis_modes_and_palettes() {
        use crate::app::{Palette, VisMode};
        let modes = [VisMode::Bars, VisMode::Wave, VisMode::Off];
        let palettes = [Palette::Neon, Palette::Ember, Palette::Forest, Palette::Mono];
        for vis in modes {
            for palette in palettes {
                let backend = TestBackend::new(120, 40);
                let mut terminal = Terminal::new(backend).unwrap();
                let mut app = App::new(&Config::default());
                app.vis = vis;
                app.palette = palette;
                app.audio_available = Some(true);
                app.on_levels(vec![0.5; 32], vec![0.2; 48]);
                terminal.draw(|f| render(f, &app)).unwrap();
            }
        }
    }
}
