//! Embedded web assets served as static files.

use axum::Router;
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use axum::routing::get;

const INDEX_HTML: &str = include_str!("../assets/index.html");
const APP_JS: &str = include_str!("../assets/app.js");
const STYLE_CSS: &str = include_str!("../assets/style.css");

/// Build a router serving embedded web assets.
pub fn router() -> Router {
    Router::new()
        .route("/", get(serve_index))
        .route("/index.html", get(serve_index))
        .route("/app.js", get(serve_js))
        .route("/style.css", get(serve_css))
}

async fn serve_index() -> Response {
    ([(CONTENT_TYPE, "text/html; charset=utf-8")], INDEX_HTML).into_response()
}

async fn serve_js() -> Response {
    ([(CONTENT_TYPE, "text/javascript; charset=utf-8")], APP_JS).into_response()
}

async fn serve_css() -> Response {
    ([(CONTENT_TYPE, "text/css; charset=utf-8")], STYLE_CSS).into_response()
}

/// Fallback handler for unknown paths.
pub async fn fallback() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, "not found")
}
