use axum::{
    extract::{OriginalUri, State},
    http::HeaderMap,
    http::header,
    response::{Html, IntoResponse, Response},
};
use tracing::info;

use super::AppState;

const INDEX_HTML: &str = include_str!("explorer/index.html");
const APP_JS: &str = include_str!("explorer/app.js");
const STYLES_CSS: &str = include_str!("explorer/styles.css");
const FAVICON_SVG: &str = include_str!("explorer/favicon.svg");
const LOGO_PNG: &[u8] = include_bytes!("explorer/logo.png");

pub async fn index(
    State(state): State<AppState>,
    uri: OriginalUri,
    headers: HeaderMap,
) -> Html<String> {
    let path = uri.path();
    if path.starts_with("/explore/address/")
        || path.starts_with("/explore/token/")
        || path.starts_with("/explore/receipt/")
        || path.starts_with("/explore/block/")
    {
        let referer = headers
            .get(header::REFERER)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("-");
        info!(path = %path, referer = %referer, "Explorer route request");
    }
    let html = INDEX_HTML
        .replace("__DEFAULT_CHAIN_ID__", &state.default_chain_id.to_string())
        .replace(
            "__KASPA_EXPLORER_BASE_URL__",
            &serde_json::to_string(&state.kaspa_explorer_base_url)
                .unwrap_or_else(|_| "\"https://kaspa.stream\"".to_string()),
        );
    Html(html)
}

pub async fn app_js() -> Response {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        APP_JS,
    )
        .into_response()
}

pub async fn styles_css() -> Response {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        STYLES_CSS,
    )
        .into_response()
}

pub async fn favicon_svg() -> Response {
    (
        [(header::CONTENT_TYPE, "image/svg+xml; charset=utf-8")],
        FAVICON_SVG,
    )
        .into_response()
}

pub async fn logo_png() -> Response {
    ([(header::CONTENT_TYPE, "image/png")], LOGO_PNG).into_response()
}
