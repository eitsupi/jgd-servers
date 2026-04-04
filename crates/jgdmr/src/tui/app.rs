//! TUI application state and event handling.

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use image::ImageReader;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use std::io::Cursor;
use std::path::PathBuf;

use jgd_render::{RasterRenderer, Renderer};
use jgd_server::hub::{HubHandle, PlotSummary};

/// TUI application state.
pub struct App {
    pub hub: HubHandle,
    /// Known session IDs in insertion order.
    pub sessions: Vec<String>,
    /// Index into `sessions` for the currently viewed session.
    pub active_session_idx: usize,
    /// Plot index within the active session.
    pub current_plot_index: usize,
    /// Total number of plots in the active session.
    pub plot_count: usize,
    /// Whether the cursor follows the latest plot automatically.
    pub following_latest: bool,
    /// Terminal graphics protocol picker.
    pub picker: Picker,
    /// Current image state for ratatui-image rendering.
    pub image_state: Option<StatefulProtocol>,
    /// Status message shown in the status bar.
    pub status: Option<String>,
    /// True when the terminal lacks a graphics protocol (Sixel/Kitty/iTerm2).
    pub halfblock_mode: bool,
    /// Set to true to exit the event loop.
    pub should_quit: bool,
    /// Terminal size in character cells (cols, rows).
    pub terminal_size: (u16, u16),
}

impl App {
    pub fn new(hub: HubHandle, picker: Picker) -> Self {
        let terminal_size =
            crossterm::terminal::size().unwrap_or((80, 24));
        Self {
            hub,
            sessions: Vec::new(),
            active_session_idx: 0,
            current_plot_index: 0,
            plot_count: 0,
            following_latest: true,
            picker,
            image_state: None,
            status: None,
            halfblock_mode: false,
            should_quit: false,
            terminal_size,
        }
    }

    /// Handle a key press event.
    pub async fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.should_quit = true;
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.prev_plot().await?;
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.next_plot().await?;
            }
            KeyCode::Tab => {
                self.next_session().await?;
            }
            KeyCode::BackTab => {
                self.prev_session().await?;
            }
            KeyCode::Char('s') => {
                self.save_current_plot().await?;
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            _ => {}
        }
        Ok(())
    }

    /// Handle a new frame broadcast from the Hub.
    pub async fn handle_frame(&mut self, session_id: Option<&str>) -> Result<()> {
        if self.following_latest {
            // Fetch summaries once and reuse.
            let summaries = self.hub.get_plots().await;
            self.update_from_summaries(&summaries);

            // Switch to the session that received the frame, then
            // recalculate plot_count for the new active session.
            if let Some(sid) = session_id
                && let Some(idx) = self.sessions.iter().position(|s| s == sid)
            {
                self.active_session_idx = idx;
                self.update_from_summaries(&summaries);
            }
            self.current_plot_index = self.plot_count.saturating_sub(1);
            self.refresh_image().await?;
        } else {
            self.status = Some("New plot available".into());
        }

        Ok(())
    }

    /// Handle terminal resize.
    pub async fn handle_resize(&mut self, width: u16, height: u16) -> Result<()> {
        self.terminal_size = (width, height);
        // Invalidate the cached image so it gets re-rendered at the new size.
        self.image_state = None;
        self.refresh_image().await?;
        Ok(())
    }

    /// Compute the pixel dimensions for rendering based on terminal size and font metrics.
    fn render_pixel_size(&self) -> (u32, u32) {
        let (cols, rows) = self.terminal_size;
        let (fw, fh) = self.picker.font_size();
        // Reserve 1 row for the status bar.
        let usable_rows = rows.saturating_sub(1);
        let w = (cols as u32) * (fw as u32);
        let h = (usable_rows as u32) * (fh as u32);
        // Ensure minimum size.
        (w.max(64), h.max(64))
    }

    async fn prev_plot(&mut self) -> Result<()> {
        if self.current_plot_index > 0 {
            self.current_plot_index -= 1;
            self.following_latest = false;
            self.refresh_image().await?;
        }
        Ok(())
    }

    async fn next_plot(&mut self) -> Result<()> {
        if self.current_plot_index + 1 < self.plot_count {
            self.current_plot_index += 1;
            if self.current_plot_index == self.plot_count - 1 {
                self.following_latest = true;
            }
            self.refresh_image().await?;
        }
        Ok(())
    }

    async fn prev_session(&mut self) -> Result<()> {
        if self.active_session_idx > 0 {
            self.active_session_idx -= 1;
            self.following_latest = true;
            self.refresh_session_state().await?;
        }
        Ok(())
    }

    async fn next_session(&mut self) -> Result<()> {
        if self.active_session_idx + 1 < self.sessions.len() {
            self.active_session_idx += 1;
            self.following_latest = true;
            self.refresh_session_state().await?;
        }
        Ok(())
    }

    async fn save_current_plot(&mut self) -> Result<()> {
        let Some(session_id) = self.active_session_id() else {
            self.status = Some("No session".into());
            return Ok(());
        };
        let session_id = session_id.to_owned();

        let plot = self
            .hub
            .get_plot(&session_id, Some(self.current_plot_index))
            .await;
        let Some(plot) = plot else {
            self.status = Some("No plot to save".into());
            return Ok(());
        };

        let png_bytes = self.render_plot_to_png(&plot)?;
        let path = self.default_save_path();
        std::fs::write(&path, &png_bytes)?;
        self.status = Some(format!("Saved to {}", path.display()));
        Ok(())
    }

    /// Refresh session list and plot count from the Hub.
    pub async fn refresh_sessions(&mut self) -> Result<()> {
        let summaries = self.hub.get_plots().await;
        self.update_from_summaries(&summaries);
        Ok(())
    }

    fn update_from_summaries(&mut self, summaries: &[PlotSummary]) {
        // Rebuild session list preserving order of first appearance.
        let mut new_sessions: Vec<String> = Vec::new();
        for s in summaries {
            if !new_sessions.contains(&s.session_id) {
                new_sessions.push(s.session_id.clone());
            }
        }
        self.sessions = new_sessions;

        // Clamp active session index.
        if self.active_session_idx >= self.sessions.len() && !self.sessions.is_empty() {
            self.active_session_idx = self.sessions.len() - 1;
        }

        // Update plot count for active session.
        if let Some(sid) = self.active_session_id() {
            self.plot_count = summaries.iter().filter(|s| s.session_id == sid).count();
        } else {
            self.plot_count = 0;
        }

        // Clamp plot index.
        if self.plot_count > 0 && self.current_plot_index >= self.plot_count {
            self.current_plot_index = self.plot_count - 1;
        }
    }

    /// Refresh state when switching sessions.
    async fn refresh_session_state(&mut self) -> Result<()> {
        self.refresh_sessions().await?;
        self.current_plot_index = self.plot_count.saturating_sub(1);
        self.refresh_image().await?;
        Ok(())
    }

    /// Render a plot to PNG at the appropriate resolution for this terminal.
    fn render_plot_to_png(&self, plot: &jgd_protocol::Plot) -> Result<Vec<u8>> {
        let (pixel_w, pixel_h) = self.render_pixel_size();
        let (render_w, render_h) =
            fit_uniform(plot.device.width, plot.device.height, pixel_w, pixel_h);
        let renderer = RasterRenderer {
            output_width: Some(render_w),
            output_height: Some(render_h),
        };
        renderer.render(plot).map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Fetch the current plot from Hub and render it to an image for display.
    pub async fn refresh_image(&mut self) -> Result<()> {
        let Some(session_id) = self.active_session_id() else {
            self.image_state = None;
            return Ok(());
        };
        let session_id = session_id.to_owned();

        let plot = self
            .hub
            .get_plot(&session_id, Some(self.current_plot_index))
            .await;
        let Some(plot) = plot else {
            self.image_state = None;
            return Ok(());
        };

        let png_bytes = self.render_plot_to_png(&plot)?;
        let dyn_image = ImageReader::new(Cursor::new(png_bytes))
            .with_guessed_format()?
            .decode()?;
        self.image_state = Some(self.picker.new_resize_protocol(dyn_image));
        self.status = None;
        Ok(())
    }

    pub fn active_session_id(&self) -> Option<&str> {
        self.sessions
            .get(self.active_session_idx)
            .map(|s| s.as_str())
    }

    /// Format the position indicator: "2/5" style.
    pub fn plot_position(&self) -> String {
        if self.plot_count == 0 {
            "0/0".into()
        } else {
            format!("{}/{}", self.current_plot_index + 1, self.plot_count)
        }
    }

    pub fn session_position(&self) -> String {
        let total = self.sessions.len();
        if total == 0 {
            "0/0".into()
        } else {
            format!("{}/{}", self.active_session_idx + 1, total)
        }
    }

    fn default_save_path(&self) -> PathBuf {
        let session = self
            .active_session_id()
            .unwrap_or("plot")
            .replace(['/', '\\', ':'], "_");
        PathBuf::from(format!("{session}_{}.png", self.current_plot_index + 1))
    }
}

/// Compute the largest output size that fits within `max_w × max_h` while
/// preserving the aspect ratio of `dev_w × dev_h` (uniform scaling).
fn fit_uniform(dev_w: f64, dev_h: f64, max_w: u32, max_h: u32) -> (u32, u32) {
    if dev_w <= 0.0 || dev_h <= 0.0 || max_w == 0 || max_h == 0 {
        return (max_w.max(1), max_h.max(1));
    }
    let scale = f64::min(max_w as f64 / dev_w, max_h as f64 / dev_h);
    let w = (dev_w * scale).round() as u32;
    let h = (dev_h * scale).round() as u32;
    (w.max(1), h.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jgd_protocol::{DeviceInfo, FrameMessage, Plot};
    use jgd_server::hub::PlotSummary;

    fn make_summaries(sessions: &[(&str, usize)]) -> Vec<PlotSummary> {
        let mut out = Vec::new();
        for (sid, count) in sessions {
            for i in 0..*count {
                out.push(PlotSummary {
                    session_id: sid.to_string(),
                    plot_index: i,
                    width: 640.0,
                    height: 480.0,
                    op_count: 0,
                });
            }
        }
        out
    }

    #[tokio::test]
    async fn update_from_summaries_single_session() {
        let hub = jgd_server::hub::spawn();
        let picker = Picker::halfblocks();
        let mut app = App::new(hub, picker);

        let summaries = make_summaries(&[("r-1", 3)]);
        app.update_from_summaries(&summaries);

        assert_eq!(app.sessions, vec!["r-1"]);
        assert_eq!(app.active_session_idx, 0);
        assert_eq!(app.plot_count, 3);
    }

    #[tokio::test]
    async fn update_from_summaries_multi_session() {
        let hub = jgd_server::hub::spawn();
        let picker = Picker::halfblocks();
        let mut app = App::new(hub, picker);

        let summaries = make_summaries(&[("r-1", 2), ("r-2", 5)]);
        app.update_from_summaries(&summaries);

        assert_eq!(app.sessions, vec!["r-1", "r-2"]);
        assert_eq!(app.plot_count, 2); // active is r-1
    }

    #[tokio::test]
    async fn update_from_summaries_clamps_indices() {
        let hub = jgd_server::hub::spawn();
        let picker = Picker::halfblocks();
        let mut app = App::new(hub, picker);

        // Set out-of-bounds indices.
        app.active_session_idx = 10;
        app.current_plot_index = 100;

        let summaries = make_summaries(&[("r-1", 3)]);
        app.update_from_summaries(&summaries);

        assert_eq!(app.active_session_idx, 0);
        assert_eq!(app.current_plot_index, 2); // clamped to last
    }

    #[tokio::test]
    async fn plot_position_display() {
        let hub = jgd_server::hub::spawn();
        let picker = Picker::halfblocks();
        let mut app = App::new(hub, picker);

        assert_eq!(app.plot_position(), "0/0");

        let summaries = make_summaries(&[("r-1", 5)]);
        app.update_from_summaries(&summaries);
        app.current_plot_index = 2;
        assert_eq!(app.plot_position(), "3/5");
    }

    #[tokio::test]
    async fn default_save_path_format() {
        let hub = jgd_server::hub::spawn();
        let picker = Picker::halfblocks();
        let mut app = App::new(hub, picker);

        let summaries = make_summaries(&[("r-100-1", 3)]);
        app.update_from_summaries(&summaries);
        app.current_plot_index = 1;

        let path = app.default_save_path();
        assert_eq!(path, PathBuf::from("r-100-1_2.png"));
    }

    #[tokio::test]
    async fn handle_frame_following_latest() {
        let hub = jgd_server::hub::spawn();
        let picker = Picker::halfblocks();
        let mut app = App::new(hub.clone(), picker);

        // Register a session and send a frame.
        let (conn_id, _rx) = hub.register_session();
        let frame = FrameMessage {
            plot: Plot {
                session_id: Some("r-1".into()),
                ops: vec![],
                device: DeviceInfo {
                    width: 640.0,
                    height: 480.0,
                    dpi: None,
                    bg: None,
                },
            },
            incremental: false,
            new_page: Some(true),
            resize_replay: None,
            plot_index: None,
            plot_number: None,
            ext: None,
        };
        hub.r_message(conn_id, jgd_protocol::Message::Frame(frame));

        // Give the hub actor time to process.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        app.handle_frame(Some("r-1")).await.unwrap();

        assert_eq!(app.sessions, vec!["r-1"]);
        assert_eq!(app.plot_count, 1);
        assert_eq!(app.current_plot_index, 0);
        assert!(app.following_latest);
    }

    #[tokio::test]
    async fn handle_frame_not_following() {
        let hub = jgd_server::hub::spawn();
        let picker = Picker::halfblocks();
        let mut app = App::new(hub.clone(), picker);
        app.following_latest = false;

        // Register and send two frames.
        let (conn_id, _rx) = hub.register_session();
        for _ in 0..2 {
            let frame = FrameMessage {
                plot: Plot {
                    session_id: Some("r-1".into()),
                    ops: vec![],
                    device: DeviceInfo {
                        width: 640.0,
                        height: 480.0,
                        dpi: None,
                        bg: None,
                    },
                },
                incremental: false,
                new_page: Some(true),
                resize_replay: None,
                plot_index: None,
                plot_number: None,
                ext: None,
            };
            hub.r_message(conn_id, jgd_protocol::Message::Frame(frame));
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        app.handle_frame(Some("r-1")).await.unwrap();

        // Should NOT auto-advance since following_latest is false.
        assert_eq!(app.current_plot_index, 0);
        assert_eq!(app.status.as_deref(), Some("New plot available"));
    }
}
