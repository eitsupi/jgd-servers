//! TUI rendering: layout and widgets.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui_image::{Resize, StatefulImage};

use super::app::App;

/// Render the TUI interface.
pub fn render(f: &mut Frame, app: &mut App) {
    let [image_area, status_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(f.area());

    if app.show_help {
        // Skip image rendering so the graphics protocol output does not
        // cover the text-based help overlay.
        f.render_widget(Clear, image_area);
        render_help_overlay(f, image_area);
    } else {
        render_image(f, app, image_area);
    }
    render_status_bar(f, app, status_area);
}

fn render_image(f: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    if let Some(ref mut image_state) = app.image_state {
        let image_widget = StatefulImage::default().resize(Resize::Scale(None));
        f.render_stateful_widget(image_widget, area, image_state);
    } else {
        let msg = if app.sessions.is_empty() {
            "Waiting for plots..."
        } else {
            "No plot selected"
        };
        let block = Block::default().borders(Borders::NONE);
        let paragraph = Paragraph::new(msg)
            .centered()
            .block(block)
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(paragraph, area);
    }
}

fn render_status_bar(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let mut spans = Vec::new();

    // Session indicator.
    if let Some(sid) = app.active_session_id() {
        spans.push(Span::styled(
            sid,
            Style::default().add_modifier(Modifier::BOLD),
        ));
        if app.sessions.len() > 1 {
            spans.push(Span::raw(format!(" [{}]", app.session_position())));
        }
    } else {
        spans.push(Span::styled(
            "no session",
            Style::default().fg(Color::DarkGray),
        ));
    }

    spans.push(Span::raw(" | "));

    // Plot position.
    spans.push(Span::raw(app.plot_position()));

    // Following-latest indicator.
    if app.following_latest && app.plot_count > 0 {
        spans.push(Span::raw(" "));
        spans.push(Span::styled("●", Style::default().fg(Color::Green)));
    }

    // Half-block mode warning.
    if app.halfblock_mode {
        spans.push(Span::raw(" | "));
        spans.push(Span::styled(
            "halfblock",
            Style::default().fg(Color::Yellow),
        ));
    }

    // Status message.
    if let Some(ref status) = app.status {
        spans.push(Span::raw(" | "));
        spans.push(Span::styled(
            status.as_str(),
            Style::default().fg(Color::Yellow),
        ));
    }

    // Right-aligned help hint.
    spans.push(Span::raw(" | "));
    spans.push(Span::styled("?", Style::default().add_modifier(Modifier::BOLD)));
    spans.push(Span::raw(":help"));

    let line = Line::from(spans);
    let status_bar =
        Paragraph::new(line).style(Style::default().bg(Color::DarkGray).fg(Color::White));
    f.render_widget(status_bar, area);
}

fn render_help_overlay(f: &mut Frame, area: Rect) {
    let lines = vec![
        Line::from(Span::styled(
            " Keybindings ",
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Yellow),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("  ←/h  ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("Previous plot"),
        ]),
        Line::from(vec![
            Span::styled("  →/l  ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("Next plot"),
        ]),
        Line::from(vec![
            Span::styled("  Tab  ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("Next session"),
        ]),
        Line::from(vec![
            Span::styled("S-Tab  ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("Previous session"),
        ]),
        Line::from(vec![
            Span::styled("    s  ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("Save plot as PNG"),
        ]),
        Line::from(vec![
            Span::styled("    q  ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("Quit"),
        ]),
        Line::from(vec![
            Span::styled("  Esc  ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("Quit"),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            " Press any key to close ",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let height = lines.len() as u16 + 2; // +2 for border
    let width = 30u16;
    let popup = centered_rect(width, height, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .style(Style::default().bg(Color::Black));

    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(lines).alignment(Alignment::Center).block(block),
        popup,
    );
}

/// Return a centered `Rect` of the given size within `area`.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}
