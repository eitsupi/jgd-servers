//! TUI rendering: layout and widgets.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui_image::{Resize, StatefulImage};

use super::app::App;

/// Render the TUI interface.
pub fn render(f: &mut Frame, app: &mut App) {
    let [image_area, status_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(f.area());

    render_image(f, app, image_area);
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

    let line = Line::from(spans);
    let status_bar =
        Paragraph::new(line).style(Style::default().bg(Color::DarkGray).fg(Color::White));
    f.render_widget(status_bar, area);
}
