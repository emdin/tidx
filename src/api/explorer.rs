use axum::{
    extract::State,
    http::header,
    response::{Html, IntoResponse, Response},
};

use super::AppState;

const INDEX_HTML: &str = include_str!("explorer/index.html");
const APP_JS: &str = include_str!("explorer/app.js");
const STYLES_CSS: &str = include_str!("explorer/styles.css");

pub async fn index(State(state): State<AppState>) -> Html<String> {
    let html = INDEX_HTML.replace("__DEFAULT_CHAIN_ID__", &state.default_chain_id.to_string());
    Html(html)
}

pub async fn app_js() -> Response {
    (
        [(header::CONTENT_TYPE, "application/javascript; charset=utf-8")],
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
