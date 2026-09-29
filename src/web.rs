use crate::AppState;
use axum::extract::{Query, State};
use axum::http::header;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::sync::Arc;

pub const UI_HTML: &str = include_str!("web/index.html");

pub async fn serve_ui() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache, no-store, must-revalidate"),
        ],
        UI_HTML,
    )
}

#[derive(Deserialize)]
pub struct AuditQ {
    pub limit: Option<usize>,
}

pub async fn api_audit(State(st): State<Arc<AppState>>, Query(q): Query<AuditQ>) -> Response {
    let limit = q.limit.unwrap_or(50).min(200);
    let mut entries = Vec::new();

    if let Ok(file) = File::open(&st.audit_path) {
        let reader = BufReader::new(file);
        let mut lines = Vec::new();
        for line in reader.lines() {
            match line {
                Ok(l) => lines.push(l),
                Err(_) => break,
            }
        }
        // Newest lines first
        lines.reverse();
        for line in lines.into_iter().take(limit) {
            if let Ok(val) = serde_json::from_str::<Value>(&line) {
                entries.push(val);
            }
        }
    }

    (
        StatusCode::OK,
        Json(json!({
            "count": entries.len(),
            "entries": entries,
        })),
    )
        .into_response()
}
