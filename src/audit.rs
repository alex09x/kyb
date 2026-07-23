use crate::AppState;
use axum::extract::{ConnectInfo, Request, State};
use axum::middleware::Next;
use axum::response::Response;
use std::io::Write;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Append-only JSONL audit: who (ip), what (method/path/query), how it went
/// (status), how long it took (ms). A request-audit log without the
/// machinery — traffic here is tiny, single-user.
pub struct Audit {
    file: Mutex<std::fs::File>,
}

impl Audit {
    pub fn open(path: &Path) -> anyhow::Result<Audit> {
        let file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Audit { file: Mutex::new(file) })
    }

    fn append(&self, line: &serde_json::Value) {
        if let Ok(mut f) = self.file.lock() {
            let _ = writeln!(f, "{line}");
        }
    }
}

pub async fn audit_mw(State(st): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    // the skill's ensure_up polls healthz on every command — noise, skip it
    if path == "/healthz" {
        return next.run(req).await;
    }
    let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip().to_string())
        .unwrap_or_else(|| "-".into());
    let method = req.method().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let start = std::time::Instant::now();
    let resp = next.run(req).await;
    st.audit.append(&serde_json::json!({
        "ts": ts,
        "ip": ip,
        "method": method,
        "path": path,
        "query": query,
        "status": resp.status().as_u16(),
        "ms": start.elapsed().as_millis() as u64,
    }));
    resp
}
