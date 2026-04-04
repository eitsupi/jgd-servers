//! TUI application state and event handling.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use image::{DynamicImage, RgbaImage};
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;

use jgd_protocol::ResizeMessage;
use jgd_render::RasterRenderer;
use jgd_server::hub::{HubHandle, PlotSummary};

/// Cache key for rendered plot images.
#[derive(Clone, PartialEq, Eq, Hash)]
struct ImageCacheKey {
    session_id: String,
    plot_index: usize,
    pixel_w: u32,
    pixel_h: u32,
}

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
    /// Whether to display the keybinding help overlay.
    pub show_help: bool,
    /// Terminal size in character cells (cols, rows).
    pub terminal_size: (u16, u16),
    /// Cache of rendered images keyed by (session, plot index, pixel size).
    /// Unbounded; typical usage stays small (one entry per viewed plot at the
    /// current terminal size, cleared on resize and new-frame events).
    image_cache: HashMap<ImageCacheKey, DynamicImage>,
}

impl App {
    pub fn new(hub: HubHandle, picker: Picker) -> Self {
        let terminal_size = crossterm::terminal::size().unwrap_or((80, 24));
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
            show_help: false,
            terminal_size,
            image_cache: HashMap::new(),
        }
    }

    /// Handle a key press event.
    pub async fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        // When help overlay is visible, dismiss it on any key.
        if self.show_help {
            self.show_help = false;
            // Force image re-render: after terminal.clear() the graphics
            // protocol state (e.g. Kitty transmitted flag) is stale.
            self.image_state = None;
            self.refresh_image().await?;
            return Ok(());
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.should_quit = true;
            }
            KeyCode::Char('?') => {
                self.show_help = true;
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
        let was_empty = self.sessions.is_empty();

        // Invalidate cached images for the session that received a new frame,
        // since the latest plot may have been updated incrementally.
        match session_id {
            Some(sid) => self.image_cache.retain(|k, _| k.session_id != sid),
            None => self.image_cache.clear(),
        }

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

        // On the first frame, send a resize so R knows the terminal dimensions
        // and replays at the correct size.
        if was_empty && !self.sessions.is_empty() {
            self.send_resize(session_id.map(String::from));
        }

        Ok(())
    }

    /// Handle terminal resize.
    pub async fn handle_resize(&mut self, width: u16, height: u16) -> Result<()> {
        self.terminal_size = (width, height);
        // Invalidate all cached images — pixel dimensions changed.
        self.image_cache.clear();
        self.image_state = None;
        // Broadcast resize so every R session updates its device size and
        // replays the current display list (= latest plot).
        self.send_resize(None);
        // If viewing a past plot, also send a targeted resize so R replays
        // *that* historical plot at the new aspect ratio.
        if self.plot_count > 0 && !self.following_latest {
            self.send_targeted_resize();
        }
        self.refresh_image().await?;
        Ok(())
    }

    /// Compute the pixel dimensions for rendering based on terminal size.
    ///
    /// Uses `cells × font_size` where font_size has been corrected at startup
    /// via `ceil(window_pixels / cells)` to account for fractional font sizes.
    fn render_pixel_size(&self) -> (u32, u32) {
        let (cols, rows) = self.terminal_size;
        let (fw, fh) = self.picker.font_size();
        // Reserve 1 row for the status bar.
        let usable_rows = rows.saturating_sub(1);
        let w = (cols as u32) * (fw as u32);
        let h = (usable_rows as u32) * (fh as u32);
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

        let (pixel_w, pixel_h) = self.render_pixel_size();
        let renderer = RasterRenderer {
            output_width: Some(pixel_w),
            output_height: Some(pixel_h),
        };
        let png_bytes =
            jgd_render::Renderer::render(&renderer, &plot).map_err(|e| anyhow::anyhow!("{e}"))?;
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

    /// Render a plot to a [`DynamicImage`] at the full terminal pixel size.
    ///
    /// Renders via tiny-skia and converts the premultiplied-alpha pixmap
    /// directly to a straight-alpha `DynamicImage`, avoiding a PNG
    /// encode/decode round-trip.
    fn render_plot_to_image(&self, plot: &jgd_protocol::Plot) -> Result<DynamicImage> {
        let (pixel_w, pixel_h) = self.render_pixel_size();
        let renderer = RasterRenderer {
            output_width: Some(pixel_w),
            output_height: Some(pixel_h),
        };
        let pixmap = renderer
            .render_to_pixmap(plot)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        // Convert premultiplied RGBA → straight RGBA for the image crate.
        let mut rgba = Vec::with_capacity((pixmap.width() * pixmap.height() * 4) as usize);
        for pixel in pixmap.pixels() {
            let c = pixel.demultiply();
            rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
        }
        let img = RgbaImage::from_raw(pixmap.width(), pixmap.height(), rgba)
            .ok_or_else(|| anyhow::anyhow!("pixel buffer size mismatch"))?;
        Ok(DynamicImage::ImageRgba8(img))
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

        // Write debug dimensions to a file (tracing goes to stderr which
        // conflicts with the TUI's alternate screen).
        #[cfg(debug_assertions)]
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/tmp/jgdmr-debug.log")
        {
            use std::io::Write;
            let (pw, ph) = self.render_pixel_size();
            let (fw, fh) = self.picker.font_size();
            let ws = crossterm::terminal::window_size().ok();
            let _ = writeln!(
                f,
                "refresh_image: terminal_size={:?} font_size=({},{}) window_size={:?} render_pixels=({},{}) device_size=({},{})",
                self.terminal_size, fw, fh, ws, pw, ph, plot.device.width, plot.device.height
            );
        }

        let (pixel_w, pixel_h) = self.render_pixel_size();
        let cache_key = ImageCacheKey {
            session_id: session_id.clone(),
            plot_index: self.current_plot_index,
            pixel_w,
            pixel_h,
        };

        let dyn_image = if let Some(img) = self.image_cache.remove(&cache_key) {
            img
        } else {
            self.render_plot_to_image(&plot)?
        };

        // Re-insert into cache, cloning for the protocol.  The clone is
        // unavoidable since new_resize_protocol takes ownership, but this
        // is still cheaper than re-rendering from scratch on next visit.
        self.image_cache.insert(cache_key, dyn_image.clone());
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

    /// Get the actual terminal pixel dimensions for the usable area
    /// (excluding the status bar row).
    fn window_pixel_size(&self) -> (u32, u32) {
        let (_cols, rows) = self.terminal_size;
        let usable_rows = rows.saturating_sub(1);
        if let Ok(ws) = crossterm::terminal::window_size() {
            if ws.width > 0 && ws.height > 0 && ws.rows > 0 {
                let w = ws.width as u32;
                let h = (ws.height as u32) * (usable_rows as u32) / (ws.rows as u32);
                return (w.max(1), h.max(1));
            }
        }
        // Fallback to font-based calculation.
        self.render_pixel_size()
    }

    /// Broadcast a resize to all R sessions so they update their device size.
    ///
    /// Uses the actual window pixel size (not the ceil-rounded render size)
    /// so R lays out the plot to fit the visible terminal area.  Sends
    /// `plot_index: None` which causes each session to replay its current
    /// display list (i.e. the latest plot).
    fn send_resize(&self, session_id: Option<String>) {
        let (w, h) = self.window_pixel_size();
        self.hub.client_resize(ResizeMessage {
            width: w as f64,
            height: h as f64,
            plot_index: None,
            session_id,
        });
    }

    /// Send a targeted resize for the currently viewed plot so R replays that
    /// specific historical plot at the current terminal dimensions.
    fn send_targeted_resize(&self) {
        let Some(session_id) = self.active_session_id().map(String::from) else {
            return;
        };
        let (w, h) = self.window_pixel_size();
        self.hub.client_resize(ResizeMessage {
            width: w as f64,
            height: h as f64,
            plot_index: Some(self.current_plot_index as u32),
            session_id: Some(session_id),
        });
    }

    fn default_save_path(&self) -> PathBuf {
        let session = self
            .active_session_id()
            .unwrap_or("plot")
            .replace(['/', '\\', ':'], "_");
        PathBuf::from(format!("{session}_{}.png", self.current_plot_index + 1))
    }
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
