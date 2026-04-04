//! TUI viewer for jgd plots.

pub mod app;
mod ui;

use anyhow::Result;
use crossterm::event::{Event, EventStream};
use futures_util::StreamExt;
use ratatui_image::picker::{Picker, ProtocolType};

use jgd_protocol::Message;
use jgd_server::hub::HubHandle;

use app::App;

/// Run the TUI event loop. Blocks until the user quits.
pub async fn run(hub: HubHandle) -> Result<()> {
    // Enter alternate screen first — Picker::from_query_stdio() must run
    // after entering alternate screen for accurate font-size detection.
    let mut terminal = ratatui::init();

    let queried = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    let queried_protocol = queried.protocol_type();
    if queried_protocol == ProtocolType::Halfblocks {
        tracing::warn!("no terminal graphics protocol detected; using half-block rendering");
    }

    // Rebuild the picker with a corrected font size derived from the
    // terminal's actual pixel dimensions.  The stdio-queried font_size
    // is truncated to integers, but real fonts can have fractional
    // sizes (e.g. 7.33px), causing ratatui-image's cell↔pixel math to
    // leave pixel gaps at the terminal edges (garbled characters).
    // Using ceil(window_pixels / cells) ensures the image is at least
    // as large as the terminal area; any overflow is clipped by the
    // terminal.
    let picker = if let Ok(ws) = crossterm::terminal::window_size() {
        if ws.columns > 0 && ws.rows > 0 && ws.width > 0 && ws.height > 0 {
            let fw = (ws.width + ws.columns - 1) / ws.columns;
            let fh = (ws.height + ws.rows - 1) / ws.rows;
            // TODO: from_fontsize is deprecated since ratatui-image 9.0 with
            // no replacement that allows overriding font_size while keeping
            // the queried protocol.  Propose set_font_size() upstream.
            #[allow(deprecated)]
            let mut p = Picker::from_fontsize((fw, fh));
            p.set_protocol_type(queried_protocol);
            p
        } else {
            queried
        }
    } else {
        queried
    };
    tracing::debug!(
        font_size = ?picker.font_size(),
        protocol = ?picker.protocol_type(),
        "terminal graphics capabilities detected"
    );
    let is_halfblock = picker.protocol_type() == ProtocolType::Halfblocks;
    let mut app = App::new(hub.clone(), picker);
    app.halfblock_mode = is_halfblock;
    let mut broadcast_rx = hub.subscribe();
    let mut event_stream = EventStream::new();

    // Initial session refresh.
    app.refresh_sessions().await?;
    if app.plot_count > 0 {
        app.refresh_image().await?;
    }

    let result = event_loop(
        &mut terminal,
        &mut app,
        &mut broadcast_rx,
        &mut event_stream,
    )
    .await;

    ratatui::restore();
    result
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    broadcast_rx: &mut tokio::sync::broadcast::Receiver<Message>,
    event_stream: &mut EventStream,
) -> Result<()> {
    loop {
        terminal.draw(|f| ui::render(f, app))?;

        tokio::select! {
            event = event_stream.next() => {
                match event {
                    Some(Ok(Event::Key(key))) => app.handle_key(key).await?,
                    Some(Ok(Event::Resize(w, h))) => app.handle_resize(w, h).await?,
                    Some(Err(e)) => return Err(anyhow::Error::from(e)),
                    None => break,
                    _ => {}
                }
            }
            msg = broadcast_rx.recv() => {
                match msg {
                    Ok(Message::Frame(frame)) => {
                        let session_id = frame.plot.session_id.as_deref();
                        app.handle_frame(session_id).await?;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("TUI broadcast receiver lagged by {n} messages, refreshing");
                        app.refresh_sessions().await?;
                        app.refresh_image().await?;
                    }
                    _ => {}
                }
            }
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}
