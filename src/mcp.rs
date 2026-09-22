//! MCP served by the knowledge base itself, over HTTP.
//!
//! `POST /mcp`, and `claude mcp add --transport http kyb http://<host>:9310/mcp`
//! is the entire client-side setup. Nothing is installed and nothing needs
//! updating when the server gains a capability.
//!
//! WHY IN THE SERVER RATHER THAN IN A CLIENT.
//!
//! A client cannot know what a server is. It has to ask, and then trust the
//! answer, and then decide what to do when it cannot get one - because a server
//! that predates a parameter does not reject it, it ignores the name and answers
//! the question it DID understand. Ask such a server what was true last Tuesday
//! and you get today's data with a 200, which is indistinguishable from a past in
//! which nothing changed. A client can only defend against that by refusing.
//!
//! A server has no such problem. `tools/list` here is built from the routes this
//! binary actually has, so the advertised surface and the real surface cannot
//! disagree - not "are checked and found to agree", cannot. The failure mode is
//! designed out rather than guarded against.
//!
//! DISPATCH. A tool call is translated into an ordinary HTTP request and run
//! through the same Router that serves the public API, rather than calling the
//! model layer directly. That is deliberate: there is exactly one implementation
//! of what `POST /knowledge` means, the audit log sees MCP-driven writes like any
//! other write, and a route that changes changes here too without anyone
//! remembering to update a second copy.
//!
//! EXPOSURE. This adds no new reach. `POST /knowledge` and `DELETE
//! /knowledge/{key}` already answer unauthenticated on this port, so /mcp is
//! exactly as open as the rest of the API and no more - see the note on binding
//! in config.rs. `?readonly=1` drops the writing tools for one registration; it
//! is a guard rail for an agent, not a security boundary, since anyone may
//! simply not pass it.
//!
//! NOT A REPLACEMENT FOR THE CLI. A deploy script, an ssh one-liner on a fleet
//! node, cron and a person at a terminal all need `kyb`, and MCP reaches none of
//! them.

use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, Request as ExtractRequest, State};
use axum::http::{header, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use tower::util::ServiceExt;

/// Newest first. A client asking for one of these is answered with that exact
/// version; anything else is answered with the newest, which lets the client
/// decide whether to continue.
const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "\
KYB is a shared, versioned knowledge base about this fleet's own infrastructure - hosts, \
services, ports, deploys, architecture decisions, incidents and tasks. Query it before \
reasoning about infrastructure instead of guessing, and write back what you verified. \
Writing the same key again adds a version rather than overwriting, so a superseded fact \
stays searchable and an answer from the past is marked as one.";

/// Deliberately not exposed: `rm` and `reindex`. Retracting an entry and
/// rebuilding the index should not be something an agent reaches for mid-thought,
/// and both stay one CLI command away for a person who means it.
const WRITE_TOOLS: [&str; 6] =
    ["kyb_add", "kyb_incident", "kyb_resolve", "kyb_task", "kyb_task_status", "kyb_done"];

pub struct McpState {
    /// The public API router. Tool calls are replayed through it.
    api: Router,
}

/// `/mcp` is not wrapped in the audit layer: the request it forwards is, so a
/// write shows up once as what it actually did rather than twice.
pub fn router(api: Router) -> Router {
    Router::new()
        .route("/mcp", post(mcp_post).get(no_stream).delete(no_stream))
        .with_state(Arc::new(McpState { api }))
}

async fn no_stream() -> Response {
    // The spec allows a server with no server-to-client stream and no sessions to
    // refuse GET and DELETE. Saying so beats an unexplained 405.
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "POST")],
        axum::Json(json!({
            "error": "this endpoint is stateless: no server-initiated stream, no sessions. \
                      POST JSON-RPC to /mcp."
        })),
    )
        .into_response()
}

/// A tool call carrying a whole knowledge entry is the large case; anything past
/// this is not a request we want to buffer.
const MAX_BODY: usize = 8 * 1024 * 1024;

/// Taking the request whole rather than as extractors: the client address lives
/// in the extensions and has to survive into the forwarded call.
async fn mcp_post(State(st): State<Arc<McpState>>, req: ExtractRequest) -> Response {
    let readonly = req.uri().query().is_some_and(|q| {
        q.split('&').any(|p| matches!(p, "readonly" | "readonly=1" | "readonly=true"))
    });
    let connect = req.extensions().get::<ConnectInfo<SocketAddr>>().cloned();

    let body: Bytes = match axum::body::to_bytes(req.into_body(), MAX_BODY).await {
        Ok(b) => b,
        Err(_) => {
            return json_body(
                StatusCode::PAYLOAD_TOO_LARGE,
                rpc_error(Value::Null, -32600, "request body too large"),
            )
        }
    };

    let parsed: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return json_body(
                StatusCode::BAD_REQUEST,
                rpc_error(Value::Null, -32700, &format!("parse error: {e}")),
            )
        }
    };

    let (messages, was_batch) = match parsed {
        Value::Array(items) => (items, true),
        Value::Object(_) => (vec![parsed], false),
        _ => {
            return json_body(
                StatusCode::BAD_REQUEST,
                rpc_error(Value::Null, -32600, "invalid request: expected an object or an array"),
            )
        }
    };

    let mut out = Vec::new();
    for msg in &messages {
        if let Some(resp) = handle_one(&st, readonly, connect.as_ref(), msg).await {
            out.push(resp);
        }
    }

    // Nothing but notifications: the spec wants an empty 202, not an empty body
    // dressed as a result.
    if out.is_empty() {
        return StatusCode::ACCEPTED.into_response();
    }
    let payload = if was_batch { Value::Array(out) } else { out.swap_remove(0) };
    json_body(StatusCode::OK, payload)
}

async fn handle_one(
    st: &McpState,
    readonly: bool,
    connect: Option<&ConnectInfo<SocketAddr>>,
    msg: &Value,
) -> Option<Value> {
    // No id, or a null one, means a notification: the spec forbids answering it.
    let id = match msg.get("id") {
        None | Some(Value::Null) => return None,
        Some(v) => v.clone(),
    };
    let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
    let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));

    Some(match method {
        "initialize" => rpc_ok(id, initialize(&params)),
        "ping" => rpc_ok(id, json!({})),
        "tools/list" => rpc_ok(id, json!({"tools": tools(readonly)})),
        "tools/call" => match call_tool(st, readonly, connect, &params).await {
            Ok(value) => rpc_ok(id, value),
            Err((code, message)) => rpc_error(id, code, &message),
        },
        other => rpc_error(id, -32601, &format!("method not found: {other}")),
    })
}

fn initialize(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or_default();
    let version =
        if PROTOCOL_VERSIONS.contains(&asked) { asked } else { PROTOCOL_VERSIONS[0] };
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {"listChanged": false}},
        // Straight from the crate: a version printed here can never drift from
        // the one that was built.
        "serverInfo": {"name": "kyb", "version": env!("CARGO_PKG_VERSION")},
        "instructions": INSTRUCTIONS,
    })
}

// --------------------------------------------------------------- JSON-RPC glue

fn rpc_ok(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn json_body(status: StatusCode, payload: Value) -> Response {
    (status, axum::Json(payload)).into_response()
}

/// A failure the model should see and react to, rather than one the client
/// should treat as a protocol fault.
fn tool_error(message: String) -> Value {
    json!({"content": [{"type": "text", "text": message}], "isError": true})
}

fn tool_text(text: String) -> Value {
    json!({"content": [{"type": "text", "text": text}]})
}

// ------------------------------------------------------------------ dispatch

struct Call {
    method: &'static str,
    path: String,
    query: Vec<(&'static str, String)>,
    body: Option<Value>,
}

async fn call_tool(
    st: &McpState,
    readonly: bool,
    connect: Option<&ConnectInfo<SocketAddr>>,
    params: &Value,
) -> Result<Value, (i64, String)> {
    let name = match params.get("name").and_then(Value::as_str) {
        Some(n) if !n.is_empty() => n,
        _ => return Err((-32602, "tools/call requires a string `name`".into())),
    };
    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    if !args.is_object() {
        return Err((-32602, "`arguments` must be an object".into()));
    }
    if readonly && WRITE_TOOLS.contains(&name) {
        return Ok(tool_error(format!(
            "{name} is a write and this endpoint was opened read-only (?readonly=1)."
        )));
    }

    let call = match plan(name, &args) {
        Ok(call) => call,
        Err(message) => return Ok(tool_error(message)),
    };
    let (status, text) = forward(st, connect, call).await;
    if status.is_success() {
        Ok(tool_text(text.trim().to_string()))
    } else {
        Ok(tool_error(format!("kyb answered HTTP {}: {}", status.as_u16(), text.trim())))
    }
}

async fn forward(
    st: &McpState,
    connect: Option<&ConnectInfo<SocketAddr>>,
    call: Call,
) -> (StatusCode, String) {
    let mut uri = call.path;
    if !call.query.is_empty() {
        let pairs: Vec<String> =
            call.query.iter().map(|(k, v)| format!("{k}={}", encode(v))).collect();
        uri.push('?');
        uri.push_str(&pairs.join("&"));
    }
    let builder = Request::builder().method(call.method).uri(uri);
    let mut req = match call.body {
        Some(value) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(value.to_string())),
        None => builder.body(Body::empty()),
    }
    .expect("request built from a known-good method and uri");
    // Carry the caller through, so the audit log attributes an MCP-driven write
    // to the machine that asked for it rather than to nobody.
    if let Some(info) = connect {
        req.extensions_mut().insert(info.clone());
    }

    let resp = st.api.clone().oneshot(req).await.expect("the router is infallible");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap_or_default();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

// ------------------------------------------------------------ argument helpers

fn text_arg(args: &Value, key: &str) -> Option<String> {
    match args.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

/// A model sends a real boolean or the string "true" depending on the day.
/// Accept both; anything else is absent rather than guessed at.
fn flag_arg(args: &Value, key: &str) -> Option<String> {
    match args.get(key) {
        Some(Value::Bool(true)) => Some("true".into()),
        Some(Value::String(s)) if matches!(s.to_lowercase().as_str(), "1" | "true" | "yes") => {
            Some("true".into())
        }
        _ => None,
    }
}

/// Tags and refs arrive as a list or as one comma-separated string. Both are
/// reasonable readings of the schema, so both are accepted.
fn list_arg(args: &Value, key: &str) -> Vec<String> {
    match args.get(key) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::trim).filter(|s| !s.is_empty()))
            .map(str::to_string)
            .collect(),
        Some(Value::String(s)) => {
            s.split(',').map(str::trim).filter(|p| !p.is_empty()).map(str::to_string).collect()
        }
        _ => Vec::new(),
    }
}

fn require(args: &Value, keys: &[&str]) -> Result<(), String> {
    let missing: Vec<&str> = keys.iter().copied().filter(|k| text_arg(args, k).is_none()).collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("missing required argument(s): {}", missing.join(", ")))
    }
}

fn push(query: &mut Vec<(&'static str, String)>, key: &'static str, value: Option<String>) {
    if let Some(v) = value {
        query.push((key, v));
    }
}

fn plan(name: &str, args: &Value) -> Result<Call, String> {
    let call = match name {
        "kyb_query" => {
            require(args, &["q"])?;
            let mut query = Vec::new();
            push(&mut query, "q", text_arg(args, "q"));
            push(&mut query, "tag", text_arg(args, "tag"));
            push(&mut query, "kind", text_arg(args, "kind"));
            push(&mut query, "status", text_arg(args, "status"));
            push(&mut query, "service", text_arg(args, "service"));
            push(&mut query, "priority", text_arg(args, "priority"));
            push(&mut query, "assignee", text_arg(args, "assignee"));
            push(&mut query, "parent_task", text_arg(args, "parent_task"));
            push(&mut query, "limit", text_arg(args, "limit"));
            push(&mut query, "history", flag_arg(args, "history"));
            if flag_arg(args, "recent").is_some() {
                query.push(("sort", "recent".into()));
            }
            if args.get("semantic") == Some(&Value::Bool(false)) {
                query.push(("semantic", "false".into()));
            }
            Call { method: "GET", path: "/search".into(), query, body: None }
        }
        "kyb_get" => {
            require(args, &["key"])?;
            let mut query = Vec::new();
            push(&mut query, "at", text_arg(args, "at"));
            Call {
                method: "GET",
                path: format!("/knowledge/{}", text_arg(args, "key").unwrap_or_default()),
                query,
                body: None,
            }
        }
        "kyb_history" => {
            require(args, &["key"])?;
            Call {
                method: "GET",
                path: format!("/knowledge/{}/history", text_arg(args, "key").unwrap_or_default()),
                query: Vec::new(),
                body: None,
            }
        }
        "kyb_tags" => {
            Call { method: "GET", path: "/tags".into(), query: Vec::new(), body: None }
        }
        "kyb_health" => {
            Call { method: "GET", path: "/healthz".into(), query: Vec::new(), body: None }
        }
        "kyb_incidents" => {
            let mut query = Vec::new();
            push(&mut query, "status", text_arg(args, "status"));
            push(&mut query, "service", text_arg(args, "service"));
            push(&mut query, "all", flag_arg(args, "all"));
            push(&mut query, "limit", text_arg(args, "limit"));
            if flag_arg(args, "followups").is_some() {
                query.push(("followups", "open".into()));
            }
            Call { method: "GET", path: "/incidents".into(), query, body: None }
        }
        "kyb_tasks" => {
            let mut query = Vec::new();
            push(&mut query, "status", text_arg(args, "status"));
            push(&mut query, "priority", text_arg(args, "priority"));
            push(&mut query, "assignee", text_arg(args, "assignee"));
            push(&mut query, "parent_task", text_arg(args, "parent_task"));
            push(&mut query, "all", flag_arg(args, "all"));
            push(&mut query, "limit", text_arg(args, "limit"));
            if flag_arg(args, "followups").is_some() {
                query.push(("followups", "open".into()));
            }
            Call { method: "GET", path: "/tasks".into(), query, body: None }
        }
        "kyb_add" => {
            require(args, &["key", "title", "body"])?;
            Call {
                method: "POST",
                path: "/knowledge".into(),
                query: Vec::new(),
                body: Some(json!({
                    "key": text_arg(args, "key"),
                    "title": text_arg(args, "title"),
                    "body": text_arg(args, "body"),
                    "tags": list_arg(args, "tags"),
                    "refs": list_arg(args, "refs"),
                })),
            }
        }
        "kyb_incident" => {
            require(args, &["key", "title", "service", "severity", "body"])?;
            let affected = match args.get("affected") {
                None | Some(Value::Null) => json!([]),
                Some(Value::Array(items)) => json!(items),
                Some(_) => {
                    return Err("`affected` must be a list of {scope, from, to} objects".into())
                }
            };
            Call {
                method: "POST",
                path: "/incidents".into(),
                query: Vec::new(),
                body: Some(json!({
                    "key": text_arg(args, "key"),
                    "title": text_arg(args, "title"),
                    "body": text_arg(args, "body"),
                    "service": text_arg(args, "service"),
                    "severity": text_arg(args, "severity"),
                    "status": text_arg(args, "status").unwrap_or_else(|| "open".into()),
                    "hosts": list_arg(args, "hosts"),
                    "knowledge": list_arg(args, "knowledge"),
                    "resolution": text_arg(args, "resolution").unwrap_or_default(),
                    "detection": text_arg(args, "detection").unwrap_or_default(),
                    "affected": affected,
                    "started_at": text_arg(args, "started_at").unwrap_or_default(),
                    "detected_at": text_arg(args, "detected_at").unwrap_or_default(),
                    "tags": list_arg(args, "tags"),
                    "refs": list_arg(args, "refs"),
                })),
            }
        }
        "kyb_resolve" => {
            require(args, &["key", "resolution"])?;
            Call {
                method: "POST",
                path: format!(
                    "/incidents/{}/resolve",
                    text_arg(args, "key").unwrap_or_default()
                ),
                query: Vec::new(),
                body: Some(json!({
                    "status": text_arg(args, "status").unwrap_or_else(|| "resolved".into()),
                    "resolution": text_arg(args, "resolution"),
                })),
            }
        }
        "kyb_task" => {
            require(args, &["key", "title", "body"])?;
            Call {
                method: "POST",
                path: "/tasks".into(),
                query: Vec::new(),
                body: Some(json!({
                    "key": text_arg(args, "key"),
                    "title": text_arg(args, "title"),
                    "body": text_arg(args, "body"),
                    "status": text_arg(args, "status").unwrap_or_else(|| "open".into()),
                    "priority": text_arg(args, "priority").unwrap_or_default(),
                    "blocked_reason": text_arg(args, "blocked_reason").unwrap_or_default(),
                    "assignee": text_arg(args, "assignee").unwrap_or_default(),
                    "parent_task": text_arg(args, "parent_task").unwrap_or_default(),
                    "knowledge": list_arg(args, "knowledge"),
                    "resolution": text_arg(args, "resolution").unwrap_or_default(),
                    "tags": list_arg(args, "tags"),
                    "refs": list_arg(args, "refs"),
                })),
            }
        }
        "kyb_task_status" => {
            require(args, &["key", "status"])?;
            // Only the fields actually supplied travel: everything omitted keeps
            // what the task already says. An empty-string assignee is a value -
            // it hands the task back - so presence is what counts, not emptiness.
            let mut body = json!({"status": text_arg(args, "status")});
            for field in ["assignee", "parent_task", "blocked_reason"] {
                if let Some(value) = args.get(field) {
                    if !value.is_null() {
                        body[field] = value.clone();
                    }
                }
            }
            Call {
                method: "POST",
                path: format!(
                    "/tasks/{}/transition",
                    text_arg(args, "key").unwrap_or_default()
                ),
                query: Vec::new(),
                body: Some(body),
            }
        }
        "kyb_done" => {
            require(args, &["key", "resolution"])?;
            Call {
                method: "POST",
                path: format!("/tasks/{}/resolve", text_arg(args, "key").unwrap_or_default()),
                query: Vec::new(),
                body: Some(json!({
                    "status": text_arg(args, "status").unwrap_or_else(|| "done".into()),
                    "resolution": text_arg(args, "resolution"),
                })),
            }
        }
        other => return Err(format!("no such tool: {other}")),
    };
    Ok(call)
}

// ------------------------------------------------------------- tool schemas

/// What this binary can actually do. There is no negotiation and no fallback:
/// the list is the routes that exist in this build, so it cannot promise a
/// capability the server does not have.
fn tools(readonly: bool) -> Vec<Value> {
    let str_t = json!({"type": "string"});
    let int_t = json!({"type": "integer"});
    let list_t = json!({
        "anyOf": [{"type": "array", "items": {"type": "string"}}, {"type": "string"}],
        "description": "a list, or one comma-separated string"
    });

    let all = vec![
        json!({
            "name": "kyb_query",
            "description": "Search the shared infrastructure knowledge base: hosts, services, \
ports, deploys, architecture decisions, incident reports and tasks. Hybrid - BM25 fused with \
multilingual embeddings - so a question phrased differently from the entry still finds it, in \
any language. Query this before reasoning about infrastructure; one phrasing is not a search, \
try a couple.",
            "inputSchema": {"type": "object", "required": ["q"], "properties": {
                "q": {"type": "string", "description": "what you are looking for, in any language"},
                "tag": {"type": "string", "description": "restrict to these tags, comma-separated"},
                "kind": {"type": "string", "enum": ["knowledge", "incident", "task"],
                         "description": "restrict to one kind; absent means all"},
                "status": str_t, "service": str_t, "priority": str_t, "assignee": str_t,
                "parent_task": {"type": "string", "description": "only children of this task key"},
                "history": {"type": "boolean", "description": "search every version ever written \
instead of the current state; hits then carry is_head=false when they come from the past"},
                "semantic": {"type": "boolean", "description": "false forces pure lexical BM25 - \
useful to prove a match is literal rather than by meaning"},
                "recent": {"type": "boolean", "description": "order by commit time, not relevance"},
                "limit": int_t,
            }},
        }),
        json!({
            "name": "kyb_get",
            "description": "Read one entry in full by its key. `at` takes any revision git can \
resolve and returns the entry as it stood at that commit - the answer to 'what did we believe \
then'.",
            "inputSchema": {"type": "object", "required": ["key"], "properties": {
                "key": {"type": "string", "description": "the entry's slug"},
                "at": {"type": "string", "description": "a git revision; absent means current"},
            }},
        }),
        json!({
            "name": "kyb_history",
            "description": "Every version of one entry, newest first, with the commit sha and \
timestamp of each change. Use it to see when a fact moved, then pass a sha to kyb_get.",
            "inputSchema": {"type": "object", "required": ["key"],
                            "properties": {"key": str_t}},
        }),
        json!({
            "name": "kyb_tags",
            "description": "Which topics the base covers, with a count per tag. A cheap way to \
find out what is in there before searching blind.",
            "inputSchema": {"type": "object", "properties": {}},
        }),
        json!({
            "name": "kyb_health",
            "description": "Whether the server is up, how many entries it holds, and how many \
incidents and tasks are open.",
            "inputSchema": {"type": "object", "properties": {}},
        }),
        json!({
            "name": "kyb_incidents",
            "description": "Live incident reports, open first and freshest on top. Check this \
before infrastructure work.",
            "inputSchema": {"type": "object", "properties": {
                "status": str_t, "service": str_t, "limit": int_t,
                "followups": {"type": "boolean", "description": "only reports with unfinished \
follow-ups"},
                "all": {"type": "boolean", "description": "include closed reports, which are \
archived but still readable"},
            }},
        }),
        json!({
            "name": "kyb_tasks",
            "description": "Live tasks - open, in_progress, blocked - freshest on top.",
            "inputSchema": {"type": "object", "properties": {
                "status": str_t, "priority": str_t, "assignee": str_t, "parent_task": str_t,
                "limit": int_t,
                "followups": {"type": "boolean", "description": "only tasks with unfinished \
follow-ups"},
                "all": {"type": "boolean", "description": "include closed tasks from the archive"},
            }},
        }),
        json!({
            "name": "kyb_add",
            "description": "Write or update one entry. Same call for both: reusing a key does not \
overwrite, it adds a version, and the old text stays searchable under history. Only verified \
facts - something read in the code, seen in a config, or returned by a command actually run. \
Never secrets in the body; put a pointer in refs instead. Entries are written in English.",
            "inputSchema": {"type": "object", "required": ["key", "title", "body"], "properties": {
                "key": {"type": "string", "description": "slug [a-z0-9-]; reusing one adds a version"},
                "title": str_t,
                "body": {"type": "string", "description": "the entry itself, markdown"},
                "tags": list_t, "refs": list_t,
            }},
        }),
        json!({
            "name": "kyb_incident",
            "description": "File an incident report. The key must start with inc-. A report that \
cannot tell the next person whether the thing is still happening is a story rather than a \
report, so pass `detection`: a command to run plus the result that means healthy.",
            "inputSchema": {"type": "object",
                "required": ["key", "title", "service", "severity", "body"], "properties": {
                "key": {"type": "string", "description": "inc-<date>-<slug>"},
                "title": {"type": "string", "description": "the symptom, not the diagnosis"},
                "service": str_t,
                "severity": {"type": "string", "enum": ["low", "medium", "high", "critical"]},
                "body": {"type": "string", "description": "what happened, impact, workaround"},
                "hosts": list_t, "knowledge": list_t, "tags": list_t, "refs": list_t,
                "status": {"type": "string", "enum": ["open", "mitigated", "resolved"]},
                "detection": {"type": "string", "description": "an executable check plus its \
healthy result"},
                "affected": {"type": "array",
                    "description": "windows where data or a period got poisoned",
                    "items": {"type": "object", "required": ["scope", "from", "to"],
                              "properties": {"scope": str_t, "from": str_t, "to": str_t}}},
                "started_at": {"type": "string", "description": "RFC3339, if known"},
                "detected_at": {"type": "string", "description": "RFC3339, if known"},
                "resolution": str_t,
            }},
        }),
        json!({
            "name": "kyb_resolve",
            "description": "Close an incident. A resolution is required - what actually fixed it. \
Closing archives the report: it leaves the working set and stays searchable forever.",
            "inputSchema": {"type": "object", "required": ["key", "resolution"], "properties": {
                "key": str_t,
                "resolution": {"type": "string", "description": "what fixed it"},
                "status": {"type": "string", "enum": ["resolved", "mitigated"]},
            }},
        }),
        json!({
            "name": "kyb_task",
            "description": "Create or update a task or idea. The key must start with task-. \
Reusing the key updates it in place.",
            "inputSchema": {"type": "object", "required": ["key", "title", "body"], "properties": {
                "key": {"type": "string", "description": "task-<slug>"},
                "title": str_t, "body": str_t,
                "status": {"type": "string",
                           "enum": ["open", "in_progress", "blocked", "done", "dropped"]},
                "priority": {"type": "string", "enum": ["low", "medium", "high", "critical"]},
                "blocked_reason": {"type": "string", "description": "what it waits on; only with \
status blocked"},
                "assignee": str_t, "parent_task": str_t, "resolution": str_t,
                "knowledge": list_t, "tags": list_t, "refs": list_t,
            }},
        }),
        json!({
            "name": "kyb_task_status",
            "description": "Move a task between open, in_progress and blocked without resending \
it. Title, body, tags, priority and links stay as stored. Closing is kyb_done, not this.",
            "inputSchema": {"type": "object", "required": ["key", "status"], "properties": {
                "key": str_t,
                "status": {"type": "string", "enum": ["open", "in_progress", "blocked"]},
                "assignee": {"type": "string", "description": "an empty string hands it back"},
                "parent_task": str_t,
                "blocked_reason": {"type": "string", "description": "what it waits on; only with \
status blocked"},
            }},
        }),
        json!({
            "name": "kyb_done",
            "description": "Close a task with what came of it. Closing archives it: it leaves the \
working set and stays searchable.",
            "inputSchema": {"type": "object", "required": ["key", "resolution"], "properties": {
                "key": str_t,
                "resolution": {"type": "string", "description": "what came of it"},
                "status": {"type": "string", "enum": ["done", "dropped"]},
            }},
        }),
    ];

    if readonly {
        all.into_iter()
            .filter(|t| {
                !WRITE_TOOLS.contains(&t["name"].as_str().unwrap_or_default())
            })
            .collect()
    } else {
        all
    }
}

#[cfg(test)]
mod mcp_tests {
    use super::*;
    use axum::http::Request as HttpRequest;
    use serde_json::json;
    use std::collections::HashSet;

    fn test_app() -> (Router, tempfile::TempDir, tempfile::TempDir, std::path::PathBuf) {
        let data = tempfile::tempdir().unwrap();
        let idx = tempfile::tempdir().unwrap();
        let audit_path = idx.path().join("audit.jsonl");
        let cfg = crate::config::Config {
            data_dir: data.path().to_path_buf(),
            index_dir: idx.path().to_path_buf(),
            audit_path: audit_path.clone(),
            // these tests assert dispatch, not ranking: no model, lexical only
            model_dir: idx.path().join("no-model"),
            addr: String::new(),
        };
        let state = crate::build_state(&cfg).unwrap();
        (crate::build_app(state), data, idx, audit_path)
    }

    async fn post(app: &Router, uri: &str, payload: Value) -> (StatusCode, Value) {
        let req = HttpRequest::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(payload.to_string()))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn rpc(app: &Router, payload: Value) -> Value {
        post(app, "/mcp", payload).await.1
    }

    fn call(name: &str, args: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
               "params": {"name": name, "arguments": args}})
    }

    fn tool_body(resp: &Value) -> &str {
        resp["result"]["content"][0]["text"].as_str().unwrap_or_default()
    }

    fn errored(resp: &Value) -> bool {
        resp["result"]["isError"] == Value::Bool(true)
    }

    #[tokio::test]
    async fn initialize_answers_the_asked_version_and_the_built_one() {
        let (app, _d, _i, _a) = test_app();
        let resp = rpc(
            &app,
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                   "params": {"protocolVersion": "2025-03-26"}}),
        )
        .await;
        assert_eq!(resp["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(resp["result"]["serverInfo"]["name"], "kyb");
        // straight from Cargo.toml, so it cannot drift from the running binary
        assert_eq!(resp["result"]["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
        assert!(resp["result"]["capabilities"]["tools"].is_object());
    }

    #[tokio::test]
    async fn an_unknown_protocol_version_is_answered_with_one_we_support() {
        let (app, _d, _i, _a) = test_app();
        let resp = rpc(
            &app,
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                   "params": {"protocolVersion": "1999-01-01"}}),
        )
        .await;
        assert_eq!(resp["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
    }

    #[tokio::test]
    async fn a_notification_is_accepted_and_never_answered() {
        let (app, _d, _i, _a) = test_app();
        let (status, body) =
            post(&app, "/mcp", json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
                .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(body, Value::Null, "202 must carry no body");
    }

    #[tokio::test]
    async fn the_tool_list_is_the_routes_this_build_has() {
        let (app, _d, _i, _a) = test_app();
        let resp = rpc(&app, json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).await;
        let tools = resp["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();

        assert_eq!(names.len(), 13, "got {names:?}");
        let unique: HashSet<&&str> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "duplicate tool name in {names:?}");
        for tool in tools {
            assert!(tool["description"].as_str().is_some_and(|d| d.len() > 30));
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
        // retracting an entry and rebuilding the index stay off this surface
        assert!(!names.contains(&"kyb_rm"));
        assert!(!names.contains(&"kyb_reindex"));
        // no diff and no as_of on this build, because the routes do not exist here
        assert!(!names.contains(&"kyb_diff"));
        let query = tools.iter().find(|t| t["name"] == "kyb_query").unwrap();
        assert!(query["inputSchema"]["properties"]["as_of"].is_null());
    }

    #[tokio::test]
    async fn a_tool_call_goes_through_the_real_routes() {
        let (app, _d, _i, audit_path) = test_app();

        let added = rpc(
            &app,
            call("kyb_add", json!({"key": "orders-api", "title": "Orders API",
                                   "body": "Runs on node-7, port 8080.", "tags": "infra,orders"})),
        )
        .await;
        assert!(!errored(&added), "{added}");
        let created: Value = serde_json::from_str(tool_body(&added)).unwrap();
        assert_eq!(created["action"], "created");
        assert_eq!(created["key"], "orders-api");

        // read it back through a different tool: proves the write really landed
        // in the canon rather than in a reply the MCP layer invented
        let got = rpc(&app, call("kyb_get", json!({"key": "orders-api"}))).await;
        let entry: Value = serde_json::from_str(tool_body(&got)).unwrap();
        assert!(entry["body"].as_str().unwrap().contains("port 8080"));

        let found = rpc(&app, call("kyb_query", json!({"q": "orders", "limit": 5}))).await;
        let hits: Value = serde_json::from_str(tool_body(&found)).unwrap();
        assert!(hits["count"].as_u64().unwrap() >= 1, "{hits}");

        // and the write was audited exactly like any other write, because it WAS
        // any other write - the audit layer never saw an MCP request
        let log = std::fs::read_to_string(&audit_path).unwrap();
        assert!(
            log.lines().any(|l| l.contains("\"path\":\"/knowledge\"")
                && l.contains("\"method\":\"POST\"")),
            "no audit line for the forwarded write: {log}"
        );
        assert!(!log.contains("\"/mcp\""), "the /mcp hop should not be audited twice: {log}");
    }

    #[tokio::test]
    async fn a_second_write_to_one_key_adds_a_version() {
        let (app, _d, _i, _a) = test_app();
        for body in ["port 8080", "port 9090"] {
            rpc(
                &app,
                call("kyb_add", json!({"key": "orders-api", "title": "Orders API",
                                       "body": format!("Runs on node-7, {body}.")})),
            )
            .await;
        }
        let history = rpc(&app, call("kyb_history", json!({"key": "orders-api"}))).await;
        let parsed: Value = serde_json::from_str(tool_body(&history)).unwrap();
        assert_eq!(parsed["versions"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_missing_argument_is_refused_before_a_request_is_made() {
        let (app, _d, _i, audit_path) = test_app();
        let resp = rpc(&app, call("kyb_add", json!({"key": "half", "title": "Half"}))).await;
        assert!(errored(&resp));
        assert!(
            tool_body(&resp).starts_with("missing required argument"),
            "{}",
            tool_body(&resp)
        );

        // The point is not that the write failed - the route would have rejected a
        // null body anyway, so asserting only that would pass with this check
        // deleted. The point is that NOTHING was sent: the audit log, which sees
        // every forwarded request, has no line for it.
        let log = std::fs::read_to_string(&audit_path).unwrap_or_default();
        assert!(
            !log.contains("\"path\":\"/knowledge\""),
            "a refused call must not reach the API at all: {log}"
        );

        let got = rpc(&app, call("kyb_get", json!({"key": "half"}))).await;
        assert!(errored(&got), "the refused write must not have created anything");
    }

    #[tokio::test]
    async fn an_unknown_tool_is_readable_by_the_model_not_a_protocol_fault() {
        let (app, _d, _i, _a) = test_app();
        let resp = rpc(&app, call("kyb_delete_everything", json!({}))).await;
        assert!(errored(&resp));
        assert!(tool_body(&resp).contains("no such tool"));
        assert!(resp["error"].is_null(), "a bad tool name is not a JSON-RPC error");
    }

    #[tokio::test]
    async fn an_http_failure_from_the_route_reaches_the_model() {
        let (app, _d, _i, _a) = test_app();
        let resp = rpc(&app, call("kyb_get", json!({"key": "never-written"}))).await;
        assert!(errored(&resp));
        assert!(tool_body(&resp).contains("404"), "{}", tool_body(&resp));
    }

    #[tokio::test]
    async fn a_batch_is_answered_in_order_and_notifications_drop_out() {
        let (app, _d, _i, _a) = test_app();
        let resp = rpc(
            &app,
            json!([
                {"jsonrpc": "2.0", "id": "a", "method": "ping"},
                {"jsonrpc": "2.0", "method": "notifications/initialized"},
                {"jsonrpc": "2.0", "id": "b", "method": "tools/list"},
            ]),
        )
        .await;
        let items = resp.as_array().unwrap();
        assert_eq!(items.len(), 2, "the notification must not produce a response");
        assert_eq!(items[0]["id"], "a");
        assert_eq!(items[1]["id"], "b");
    }

    #[tokio::test]
    async fn protocol_faults_are_json_rpc_errors() {
        let (app, _d, _i, _a) = test_app();

        let unknown = rpc(&app, json!({"jsonrpc": "2.0", "id": 1, "method": "no/such"})).await;
        assert_eq!(unknown["error"]["code"], -32601);

        let nameless = rpc(
            &app,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {}}),
        )
        .await;
        assert_eq!(nameless["error"]["code"], -32602);

        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .body(Body::from("{not json"))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let parsed: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed["error"]["code"], -32700);
    }

    #[tokio::test]
    async fn get_and_delete_say_why_they_are_refused() {
        let (app, _d, _i, _a) = test_app();
        for method in ["GET", "DELETE"] {
            let req =
                HttpRequest::builder().method(method).uri("/mcp").body(Body::empty()).unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED, "{method}");
            assert_eq!(resp.headers().get("allow").unwrap(), "POST");
        }
    }

    #[tokio::test]
    async fn readonly_drops_the_writing_tools_and_refuses_them() {
        let (app, _d, _i, _a) = test_app();
        let listed =
            post(&app, "/mcp?readonly=1", json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
                .await
                .1;
        let names: Vec<&str> = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), 7, "{names:?}");
        for write in WRITE_TOOLS {
            assert!(!names.contains(&write), "{write} must not be advertised read-only");
        }
        assert!(names.contains(&"kyb_query"));

        let refused = post(
            &app,
            "/mcp?readonly=1",
            call("kyb_add", json!({"key": "k", "title": "t", "body": "b"})),
        )
        .await
        .1;
        assert!(errored(&refused));

        let got = rpc(&app, call("kyb_get", json!({"key": "k"}))).await;
        assert!(errored(&got), "a read-only refusal must not have written anything");
    }

    #[tokio::test]
    async fn a_comma_string_and_a_list_of_tags_mean_the_same_thing() {
        let (app, _d, _i, _a) = test_app();
        rpc(&app, call("kyb_add", json!({"key": "a", "title": "A", "body": "x",
                                         "tags": "infra, orders"}))).await;
        rpc(&app, call("kyb_add", json!({"key": "b", "title": "B", "body": "x",
                                         "tags": ["infra", "orders"]}))).await;
        let tags = rpc(&app, call("kyb_tags", json!({}))).await;
        let parsed: Value = serde_json::from_str(tool_body(&tags)).unwrap();
        let text = parsed.to_string();
        assert!(text.contains("infra") && text.contains("orders"), "{text}");
    }
}
