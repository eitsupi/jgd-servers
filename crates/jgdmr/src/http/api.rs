//! REST API for plot listing and server-side rendering.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Deserialize;

use jgd_render::{RasterRenderer, Renderer, SvgRenderer};
use jgd_server::hub::HubHandle;

/// Query parameters for render endpoints.
#[derive(Debug, Deserialize)]
pub struct RenderParams {
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub plot_index: Option<usize>,
}

/// Build the REST API router (headless mode).
pub fn router(hub: HubHandle) -> Router {
    Router::new()
        .route("/plots", get(list_plots))
        .route("/plots/{id}/svg", get(render_svg))
        .route("/plots/{id}/png", get(render_png))
        .with_state(hub)
}

/// Build the full router: REST API + WebSocket + static file serving.
pub fn full_router(hub: HubHandle) -> Router {
    router(hub.clone())
        .merge(super::ws::router(hub))
        .merge(super::web_assets::router())
        .fallback(super::web_assets::fallback)
}

async fn list_plots(State(hub): State<HubHandle>) -> impl IntoResponse {
    axum::Json(hub.get_plots().await)
}

const MAX_DIMENSION: f64 = 10000.0;

fn validate_dimension(v: f64) -> bool {
    v.is_finite() && v > 0.0 && v <= MAX_DIMENSION
}

async fn render_svg(
    State(hub): State<HubHandle>,
    Path(id): Path<String>,
    Query(params): Query<RenderParams>,
) -> Response {
    if let Some(w) = params.width
        && !validate_dimension(w)
    {
        return (StatusCode::BAD_REQUEST, "invalid width").into_response();
    }
    if let Some(h) = params.height
        && !validate_dimension(h)
    {
        return (StatusCode::BAD_REQUEST, "invalid height").into_response();
    }

    let Some(plot) = hub.get_plot(&id, params.plot_index).await else {
        return (StatusCode::NOT_FOUND, "plot not found").into_response();
    };

    let renderer = SvgRenderer {
        output_width: params.width,
        output_height: params.height,
    };

    match renderer.render(&plot) {
        Ok(svg) => ([(CONTENT_TYPE, "image/svg+xml")], svg).into_response(),
        Err(e) => {
            tracing::error!("SVG render failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal render error").into_response()
        }
    }
}

async fn render_png(
    State(hub): State<HubHandle>,
    Path(id): Path<String>,
    Query(params): Query<RenderParams>,
) -> Response {
    if let Some(w) = params.width
        && !validate_dimension(w)
    {
        return (StatusCode::BAD_REQUEST, "invalid width").into_response();
    }
    if let Some(h) = params.height
        && !validate_dimension(h)
    {
        return (StatusCode::BAD_REQUEST, "invalid height").into_response();
    }

    let Some(plot) = hub.get_plot(&id, params.plot_index).await else {
        return (StatusCode::NOT_FOUND, "plot not found").into_response();
    };

    let renderer = RasterRenderer {
        output_width: params.width.map(|w| w.round() as u32),
        output_height: params.height.map(|h| h.round() as u32),
    };

    match renderer.render(&plot) {
        Ok(png) => ([(CONTENT_TYPE, "image/png")], png).into_response(),
        Err(e) => {
            tracing::error!("PNG render failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal render error").into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use axum::http::Response;
    use tower::ServiceExt;

    use jgd_protocol::message::{DeviceInfo, FrameMessage, Message};
    use jgd_protocol::{DrawingOp, GraphicsContext, Plot};

    use jgd_server::hub;

    async fn get(app: Router, uri: &str) -> Response<Body> {
        app.into_service()
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    fn make_frame(session_id: &str) -> Message {
        Message::Frame(FrameMessage {
            plot: Plot {
                session_id: Some(session_id.into()),
                ops: vec![DrawingOp::Line {
                    x1: 0.0,
                    y1: 0.0,
                    x2: 100.0,
                    y2: 100.0,
                    gc: GraphicsContext {
                        col: Some("rgba(0,0,0,1)".into()),
                        ..Default::default()
                    },
                }],
                device: DeviceInfo {
                    width: 800.0,
                    height: 600.0,
                    dpi: None,
                    bg: Some("white".into()),
                },
            },
            incremental: false,
            new_page: None,
            resize_replay: None,
            plot_index: None,
            plot_number: None,
            ext: None,
        })
    }

    /// Inject a frame into the Hub via an R session.
    async fn inject_frame(hub: &HubHandle, session_id: &str) {
        let (conn_id, _rx) = hub.register_session();
        hub.r_message(conn_id, make_frame(session_id));
        // Await a query to guarantee the Hub has processed the frame.
        let _ = hub.get_plots().await;
    }

    #[tokio::test]
    async fn list_plots_empty() {
        let hub = hub::spawn();
        let resp = get(router(hub), "/plots").await;

        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let plots: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(plots.is_empty());
    }

    #[tokio::test]
    async fn list_plots_after_frame() {
        let hub = hub::spawn();
        inject_frame(&hub, "test-session").await;

        let resp = get(router(hub), "/plots").await;

        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let plots: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(plots.len(), 1);
        assert_eq!(plots[0]["session_id"], "test-session");
        assert_eq!(plots[0]["op_count"], 1);
    }

    #[tokio::test]
    async fn render_svg_returns_svg() {
        let hub = hub::spawn();
        inject_frame(&hub, "s1").await;

        let resp = get(router(hub), "/plots/s1/svg").await;

        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get(CONTENT_TYPE).unwrap(), "image/svg+xml");
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let svg = std::str::from_utf8(&body).unwrap();
        assert!(svg.starts_with("<svg xmlns="));
        assert!(svg.contains("width=\"800\""));
    }

    #[tokio::test]
    async fn render_svg_with_custom_size() {
        let hub = hub::spawn();
        inject_frame(&hub, "s1").await;

        let resp = get(router(hub), "/plots/s1/svg?width=400&height=300").await;

        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let svg = std::str::from_utf8(&body).unwrap();
        assert!(svg.contains("width=\"400\""));
        assert!(svg.contains("height=\"300\""));
        // viewBox preserves original device coordinate space (800×600).
        assert!(svg.contains("viewBox=\"0 0 800 600\""));
        assert!(svg.contains("preserveAspectRatio=\"none\""));
    }

    #[tokio::test]
    async fn render_svg_not_found() {
        let hub = hub::spawn();
        let resp = get(router(hub), "/plots/nonexistent/svg").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn render_png_not_found() {
        let hub = hub::spawn();
        let resp = get(router(hub), "/plots/any/png").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn render_png_success() {
        let hub = hub::spawn();
        inject_frame(&hub, "s1").await;
        let resp = get(router(hub), "/plots/s1/png").await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get("content-type").unwrap(), "image/png");
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(body.starts_with(&[0x89, b'P', b'N', b'G']));
    }

    #[tokio::test]
    async fn render_svg_rejects_invalid_dimensions() {
        let hub = hub::spawn();
        inject_frame(&hub, "s1").await;
        let app = router(hub);

        for uri in [
            "/plots/s1/svg?width=-1",
            "/plots/s1/svg?height=0",
            "/plots/s1/svg?width=99999",
        ] {
            let resp = get(app.clone(), uri).await;
            assert_eq!(
                resp.status(),
                StatusCode::BAD_REQUEST,
                "expected 400 for {uri}"
            );
        }
    }
}
