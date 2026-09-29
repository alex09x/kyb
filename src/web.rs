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

#[derive(Deserialize, Default)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use std::io::Write;
    use tower::ServiceExt;

    #[test]
    fn test_ui_html_structure_and_integrity() {
        assert!(!UI_HTML.is_empty(), "UI HTML must not be empty");
        assert!(UI_HTML.starts_with("<!DOCTYPE html>"), "Must declare html5 doctype");
        assert!(UI_HTML.contains("<title>KYB — Fleet Memory & Control Room</title>"));

        // All 5 core tabs present
        assert!(UI_HTML.contains(r#"id="tab-search""#));
        assert!(UI_HTML.contains(r#"id="tab-incidents""#));
        assert!(UI_HTML.contains(r#"id="tab-tasks""#));
        assert!(UI_HTML.contains(r#"id="tab-graph""#));
        assert!(UI_HTML.contains(r#"id="tab-feed""#));

        // Modals present
        assert!(UI_HTML.contains(r#"id="newModal""#));
        assert!(UI_HTML.contains(r#"id="editModal""#));
        assert!(UI_HTML.contains(r#"id="resolveIncidentModal""#));
        assert!(UI_HTML.contains(r#"id="resolveTaskModal""#));
        assert!(UI_HTML.contains(r#"id="shortcutsModal""#));
        assert!(UI_HTML.contains(r#"id="toastContainer""#));

        // Interactive controls and inputs present
        assert!(UI_HTML.contains(r#"id="globalSearch""#));
        assert!(UI_HTML.contains(r#"id="sortSelect""#));
        assert!(UI_HTML.contains(r#"id="topoCanvas""#));
        assert!(UI_HTML.contains(r#"id="nodeHud""#));
        assert!(UI_HTML.contains(r#"id="feedTableBody""#));
        assert!(UI_HTML.contains(r#"id="colOpenZone""#));
        assert!(UI_HTML.contains(r#"id="colProgZone""#));
        assert!(UI_HTML.contains(r#"id="colBlockZone""#));
        assert!(UI_HTML.contains(r#"id="colDoneZone""#));

        // Scripts and essential functions
        assert_eq!(UI_HTML.matches("<script>").count(), 1);
        assert_eq!(UI_HTML.matches("</script>").count(), 1);
        assert_eq!(UI_HTML.matches("<style>").count(), 1);
        assert_eq!(UI_HTML.matches("</style>").count(), 1);

        let required_fns = [
            "function renderMarkdown",
            "function timeAgo",
            "function refreshAll",
            "function renderHeaderStats",
            "function renderKnowledgeList",
            "function selectEntry",
            "function inspectRevision",
            "function loadDiff",
            "function renderIncidents",
            "function filterIncidents",
            "function filterIncidentsSeverity",
            "function renderTasks",
            "function transitionTask",
            "function submitResolveIncident",
            "function submitResolveTask",
            "function renderFeed",
            "function buildTopology",
            "function initCanvasGraph",
            "function switchTab",
            "function openNewModal",
            "function submitNewModal",
            "function openEditModal",
            "function submitEditModal",
            "function openShortcutsModal",
        ];

        for f in required_fns {
            assert!(UI_HTML.contains(f), "Missing essential JS function: {}", f);
        }
    }

    #[tokio::test]
    async fn test_serve_ui_headers() {
        let resp = serve_ui().await.into_response();
        assert_eq!(resp.status(), StatusCode::OK);

        let ctype = resp.headers().get(header::CONTENT_TYPE).unwrap();
        assert_eq!(ctype, "text/html; charset=utf-8");

        let cache = resp.headers().get(header::CACHE_CONTROL).unwrap();
        assert_eq!(cache, "no-cache, no-store, must-revalidate");

        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body_str = std::str::from_utf8(&bytes).unwrap();
        assert_eq!(body_str, UI_HTML);
    }

    fn test_setup() -> (axum::Router, tempfile::TempDir, tempfile::TempDir, std::path::PathBuf) {
        let data = tempfile::tempdir().unwrap();
        let idx = tempfile::tempdir().unwrap();
        let audit_path = idx.path().join("audit.jsonl");
        let cfg = crate::config::Config {
            data_dir: data.path().to_path_buf(),
            index_dir: idx.path().to_path_buf(),
            audit_path: audit_path.clone(),
            model_dir: idx.path().join("no-model"),
            addr: String::new(),
        };
        let state = crate::build_state(&cfg).unwrap();
        (crate::build_app(state), data, idx, audit_path)
    }

    #[tokio::test]
    async fn test_api_audit_empty_file() {
        let (app, _data, _idx, _audit_path) = test_setup();

        let req = Request::builder()
            .method("GET")
            .uri("/api/audit")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let val: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(val["count"], 0);
        assert_eq!(val["entries"], json!([]));
    }

    #[tokio::test]
    async fn test_api_audit_ordering_and_limit() {
        let (app, _data, _idx, audit_path) = test_setup();

        // Write 5 entries
        {
            let mut f = File::create(&audit_path).unwrap();
            for i in 1..=5 {
                let line = json!({
                    "ts": format!("2026-09-29T12:00:0{}Z", i),
                    "method": "GET",
                    "path": format!("/test/{}", i),
                    "status": 200,
                    "ms": i,
                    "ip": "127.0.0.1"
                });
                writeln!(f, "{}", line).unwrap();
            }
        }

        // Query with limit=3
        let req = Request::builder()
            .method("GET")
            .uri("/api/audit?limit=3")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let val: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(val["count"], 3);
        let entries = val["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 3);

        // Newest lines first
        assert_eq!(entries[0]["path"], "/test/5");
        assert_eq!(entries[1]["path"], "/test/4");
        assert_eq!(entries[2]["path"], "/test/3");
    }

    #[tokio::test]
    async fn test_router_serves_root_ui() {
        let (app, _data, _idx, _audit_path) = test_setup();

        let req = Request::builder()
            .method("GET")
            .uri("/")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let ctype = resp.headers().get(header::CONTENT_TYPE).unwrap();
        assert_eq!(ctype, "text/html; charset=utf-8");

        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.contains("KYB — Fleet Memory & Control Room"));
    }
}
