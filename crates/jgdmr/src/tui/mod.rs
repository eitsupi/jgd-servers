//! TUI viewer for jgd plots.

pub mod app;
mod ui;

use anyhow::Result;
use crossterm::event::{Event, EventStream};
use futures_util::StreamExt;
use ratatui_image::picker::Picker;

use jgd_protocol::Message;
use jgd_server::hub::HubHandle;

use app::App;

/// Run the TUI event loop. Blocks until the user quits.
pub async fn run(hub: HubHandle) -> Result<()> {
    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    let mut app = App::new(hub.clone(), picker);
    let mut broadcast_rx = hub.subscribe();
    let mut event_stream = EventStream::new();

    // Initial session refresh.
    app.refresh_sessions().await?;
    if app.plot_count > 0 {
        app.refresh_image().await?;
    }

    let mut terminal = ratatui::init();

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
