mod audit;
mod config;
mod embed;
mod index;
mod model;
mod store;

use anyhow::Result;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use tantivy::IndexWriter;
use tokio::sync::Mutex;

pub struct AppState {
    store: store::Store,
    index: index::SearchIndex,
    audit: audit::Audit,
    /// Vectors + model; None when no model is installed (lexical-only mode).
    semantic: Option<embed::Semantic>,
    /// One lock for ALL writes: Tantivy allows a single IndexWriter and git
    /// commits are strictly sequential anyway. Search takes no locks.
    writer: Mutex<IndexWriter>,
}

type St = State<Arc<AppState>>;
type Reply = (StatusCode, Json<Value>);

/// What gets embedded for an entry: the headline plus the head of the body,
/// which is where an entry states what it is about. Incidents also carry
/// their service and hosts — the words people will ask by.
fn embed_text(e: &model::Entry) -> String {
    let body: String = e.body.chars().take(1200).collect();
    if e.is_incident() {
        format!("{}. Incident on {} ({}). {}", e.title, e.service, e.hosts.join(", "), body)
    } else {
        format!("{}. {}", e.title, body)
    }
}

async fn rebuild_vectors(st: &AppState) {
    let Some(sem) = st.semantic.as_ref() else { return };
    // live tree + archived incidents/tasks: everything the default search
    // covers must be reachable semantically too
    let mut entries = match st.store.list_head() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("kyb: cannot list canon for embeddings: {e:#}");
            return;
        }
    };
    match st.store.archived_latest() {
        Ok(mut archived) => entries.append(&mut archived),
        Err(e) => eprintln!("kyb: archived entries not embedded: {e:#}"),
    }
    let docs: Vec<(String, String)> =
        entries.iter().map(|e| (e.key.clone(), embed_text(e))).collect();
    match sem.rebuild(docs).await {
        Ok(n) => eprintln!("kyb: embedded {n} entries"),
        Err(e) => eprintln!("kyb: embedding failed, staying lexical: {e:#}"),
    }
}

fn build_state(cfg: &config::Config) -> Result<Arc<AppState>> {
    let store = store::Store::open(&cfg.data_dir)?;
    let index = index::SearchIndex::open_or_create(&cfg.index_dir)?;
    let mut writer = index.writer()?;
    // the canon may have been hand-edited while the service was down —
    // always rebuild on start
    let (heads, hist) = index.reindex(&mut writer, &store)?;
    eprintln!("kyb: reindex on start — {heads} head entries, {hist} history versions");
    let audit = audit::Audit::open(&cfg.audit_path)?;
    let semantic = match embed::Semantic::load(&cfg.model_dir) {
        Ok(s) => {
            eprintln!("kyb: semantic search on ({})", cfg.model_dir.display());
            Some(s)
        }
        Err(e) => {
            eprintln!("kyb: lexical-only search ({e})");
            None
        }
    };
    Ok(Arc::new(AppState { store, index, audit, semantic, writer: Mutex::new(writer) }))
}

// No auth on purpose: we listen on 127.0.0.1 / the internal network only
fn build_app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/knowledge", post(upsert))
        .route("/knowledge/{key}", get(get_one).delete(remove))
        .route("/knowledge/{key}/history", get(history))
        .route("/incidents", post(upsert_incident).get(list_incidents))
        .route("/incidents/{key}/resolve", post(resolve_incident))
        .route("/tasks", post(upsert_task).get(list_tasks))
        .route("/tasks/{key}/resolve", post(resolve_task))
        .route("/search", get(search))
        .route("/tags", get(tags))
        .route("/reindex", post(reindex))
        .layer(middleware::from_fn_with_state(state.clone(), audit::audit_mw))
        .with_state(state)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = config::Config::from_env();
    let state = build_state(&cfg)?;
    rebuild_vectors(&state).await;
    let app = build_app(state);
    let listener = tokio::net::TcpListener::bind(&cfg.addr).await?;
    eprintln!("kyb: listening on http://{}", cfg.addr);
    // ConnectInfo so the audit log sees the real client IP
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}

fn err500(e: anyhow::Error) -> Reply {
    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("{e:#}")})))
}

#[derive(Deserialize)]
struct UpsertReq {
    key: String,
    title: String,
    body: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    refs: Vec<String>,
}

/// Shared tail of both upsert routes: validate, commit to git, update the
/// index and the vector side. `extra` lets a route add response fields.
async fn commit_entry(st: &Arc<AppState>, entry: model::Entry, extra: Value) -> Reply {
    if let Err(e) = entry.validate() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": e.to_string()})));
    }
    let mut w = st.writer.lock().await;
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let (c, action) = match st.store.upsert(entry, &today) {
        Err(e) => return err500(e),
        Ok(store::UpsertOutcome::Unchanged(entry)) => {
            return (StatusCode::OK, Json(json!({"key": entry.key, "changed": false})));
        }
        Ok(store::UpsertOutcome::Created(c)) => (c, "created"),
        Ok(store::UpsertOutcome::Updated(c)) => (c, "updated"),
    };
    // git is already the truth; a broken index heals via /reindex
    if let Err(e) = st
        .index
        .upsert_head(&mut w, &c.entry, &c.sha, c.committed_at)
        .and_then(|_| st.index.commit_and_reload(&mut w))
    {
        return err500(e.context("git committed but the index was not updated — run POST /reindex"));
    }
    drop(w);
    if let Some(sem) = st.semantic.as_ref() {
        if let Err(e) = sem.upsert(&c.entry.key, &embed_text(&c.entry)).await {
            eprintln!("kyb: embedding not updated for {}: {e:#}", c.entry.key);
        }
    }
    let mut resp = json!({"key": c.entry.key, "sha": c.sha, "changed": true, "action": action});
    if let (Some(obj), Some(add)) = (resp.as_object_mut(), extra.as_object()) {
        obj.extend(add.clone());
    }
    (StatusCode::OK, Json(resp))
}

async fn upsert(State(st): St, Json(r): Json<UpsertReq>) -> Reply {
    let entry = model::Entry {
        key: r.key,
        title: r.title,
        tags: r.tags,
        refs: r.refs,
        body: r.body,
        ..Default::default()
    };
    commit_entry(&st, entry, Value::Null).await
}

fn default_status() -> String {
    "open".to_string()
}

#[derive(Deserialize)]
struct IncidentReq {
    key: String,
    title: String,
    body: String,
    service: String,
    #[serde(default)]
    hosts: Vec<String>,
    severity: String,
    #[serde(default = "default_status")]
    status: String,
    /// Keys of knowledge entries this incident is tied to.
    #[serde(default)]
    knowledge: Vec<String>,
    /// How it ended; required when status=resolved.
    #[serde(default)]
    resolution: String,
    /// Executable "is it still happening?" check + expected healthy result.
    #[serde(default)]
    detection: String,
    /// Machine-readable poisoned windows: [{scope, from, to}].
    #[serde(default)]
    affected: Vec<model::Window>,
    #[serde(default)]
    started_at: String,
    #[serde(default)]
    detected_at: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    refs: Vec<String>,
}

fn now_utc() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The stored version of a key: the tree file, or — for archived
/// incidents/tasks — the latest version from history. Lets wholesale updates
/// and resolves work across the archive boundary.
fn stored_version(st: &AppState, key: &str) -> Option<model::Entry> {
    if let Ok(Some(e)) = st.store.get(key) {
        return Some(e);
    }
    st.store.latest_version(key).ok().flatten().filter(|e| e.kind != model::KIND_KNOWLEDGE)
}

/// Server-managed incident timeline. Wholesale upserts must not wipe stamps
/// the server set earlier, so empty timeline fields inherit from the stored
/// version; then status transitions get stamped if the writer left them empty.
fn stamp_timeline(st: &AppState, entry: &mut model::Entry) {
    if let Some(old) = stored_version(st, &entry.key) {
        let inherit = [
            (&mut entry.started_at, old.started_at),
            (&mut entry.detected_at, old.detected_at),
            (&mut entry.mitigated_at, old.mitigated_at),
            (&mut entry.resolved_at, old.resolved_at),
        ];
        for (field, stored) in inherit {
            if field.trim().is_empty() {
                *field = stored;
            }
        }
    }
    if entry.is_incident() && entry.detected_at.trim().is_empty() {
        entry.detected_at = now_utc();
    }
    if entry.status == "mitigated" && entry.mitigated_at.trim().is_empty() {
        entry.mitigated_at = now_utc();
    }
    // resolved incidents and done/dropped tasks both stamp the close time
    if entry.is_closed() && entry.resolved_at.trim().is_empty() {
        entry.resolved_at = now_utc();
    }
}

async fn upsert_incident(State(st): St, Json(r): Json<IncidentReq>) -> Reply {
    let mut entry = model::Entry {
        key: r.key,
        title: r.title,
        kind: model::KIND_INCIDENT.into(),
        service: r.service,
        hosts: r.hosts,
        severity: r.severity,
        status: r.status,
        knowledge: r.knowledge,
        resolution: r.resolution,
        detection: r.detection,
        affected: r.affected,
        started_at: r.started_at,
        detected_at: r.detected_at,
        tags: r.tags,
        refs: r.refs,
        body: r.body,
        ..Default::default()
    };
    stamp_timeline(&st, &mut entry);
    // linking to a missing entry is allowed (write the knowledge later),
    // but the writer should know the link is dangling right now
    let mut unknown = vec![];
    for k in &entry.knowledge {
        if model::is_valid_key(k) && !matches!(st.store.get(k), Ok(Some(_))) {
            unknown.push(k.clone());
        }
    }
    // Structure is a convention, not a gate: a report missing its actionable
    // parts is accepted but told exactly what a complete one carries.
    let mut hints = vec![];
    if entry.detection.trim().is_empty() {
        hints.push("no detection: add an executable 'is it still happening?' check with the expected healthy result (--detection)");
    }
    if entry.affected.is_empty() {
        hints.push("no affected windows: if data or a period got poisoned, record {scope,from,to} in --affected so backtests can exclude it programmatically");
    }
    if !entry.body.contains("- [ ]") && !entry.body.contains("- [x]") {
        hints.push("no follow-ups: track loose ends in the body as '- [ ]' checklist items so they are not lost");
    }
    if !entry.body.to_lowercase().contains("root cause") {
        hints.push("no 'Root cause' section: state it and mark the confidence — verified | suspected | unknown");
    }
    let mut extra = serde_json::Map::new();
    if !unknown.is_empty() {
        extra.insert("unknown_knowledge".into(), json!(unknown));
    }
    if !hints.is_empty() {
        extra.insert("hints".into(), json!(hints));
    }
    let extra = if extra.is_empty() { Value::Null } else { Value::Object(extra) };
    let closed = entry.is_closed();
    let key = entry.key.clone();
    let mut reply = commit_entry(&st, entry, extra).await;
    if closed {
        archive_closed(&st, &key, &mut reply).await;
    }
    reply
}

#[derive(Deserialize)]
struct ResolveReq {
    /// How it ended: what fixed it, or the accepted outcome / closing comment.
    #[serde(default)]
    resolution: String,
    /// Target status; the kind's closing status when omitted
    /// ("resolved" for incidents, "done" for tasks).
    status: Option<String>,
}

/// Archive a closed entry: the file leaves the working tree, the latest
/// version (already committed with the final status) stays in the default
/// search. The live index doc and the vector are intentionally kept.
async fn archive_closed(st: &Arc<AppState>, key: &str, reply: &mut Reply) {
    if reply.0 != StatusCode::OK {
        return;
    }
    let _w = st.writer.lock().await; // serialize git ops with other writers
    match st.store.archive(key) {
        Err(e) => eprintln!("kyb: archive failed for {key}: {e:#}"),
        Ok(None) => {} // already archived
        Ok(Some(sha)) => {
            if let Some(obj) = reply.1.as_object_mut() {
                obj.insert("archived".into(), json!(true));
                obj.insert("archive_sha".into(), json!(sha));
            }
        }
    }
}

/// Close the loop on an incident or a task without resending the whole entry:
/// flip the status, record how it ended; every other field stays as stored.
/// A closing status also archives the entry.
async fn close_entry(st: &Arc<AppState>, key: String, want_task: bool, r: ResolveReq) -> Reply {
    if let Some(resp) = bad_key(&key) {
        return resp;
    }
    // archived entries can still be closed again (amended resolution) or
    // parked back to a non-closing status, which reopens the file
    let Some(mut e) = stored_version(&st, &key) else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": format!("no entry '{key}'")})));
    };
    if want_task != e.is_task() || (!want_task && !e.is_incident()) {
        let what = if want_task { "a task" } else { "an incident report" };
        return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("'{key}' is not {what}")})));
    }
    let default_close = if want_task { "done" } else { "resolved" };
    e.status = r.status.unwrap_or_else(|| default_close.to_string());
    // an empty resolution keeps whatever was recorded before, so closing
    // twice never wipes the outcome
    if !r.resolution.trim().is_empty() {
        e.resolution = r.resolution;
    }
    stamp_timeline(&st, &mut e);
    // loose ends do not block a close, but they must not go silent either
    let open = e.open_followups();
    let closed = e.is_closed();
    let extra = if open > 0 && closed {
        json!({
            "open_followups": open,
            "warning": format!("{open} unfinished follow-up(s) (`- [ ]`) remain in the body — reassign or finish them"),
        })
    } else {
        Value::Null
    };
    let mut reply = commit_entry(st, e, extra).await;
    if closed {
        archive_closed(st, &key, &mut reply).await;
    }
    reply
}

async fn resolve_incident(
    State(st): St,
    Path(key): Path<String>,
    Json(r): Json<ResolveReq>,
) -> Reply {
    close_entry(&st, key, false, r).await
}

async fn resolve_task(State(st): St, Path(key): Path<String>, Json(r): Json<ResolveReq>) -> Reply {
    close_entry(&st, key, true, r).await
}

#[derive(Deserialize)]
struct TaskReq {
    key: String,
    title: String,
    body: String,
    #[serde(default = "default_status")]
    status: String,
    /// Keys of knowledge entries this task concerns.
    #[serde(default)]
    knowledge: Vec<String>,
    /// What came of it; required when closing (done/dropped).
    #[serde(default)]
    resolution: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    refs: Vec<String>,
}

async fn upsert_task(State(st): St, Json(r): Json<TaskReq>) -> Reply {
    let mut entry = model::Entry {
        key: r.key,
        title: r.title,
        kind: model::KIND_TASK.into(),
        status: r.status,
        knowledge: r.knowledge,
        resolution: r.resolution,
        tags: r.tags,
        refs: r.refs,
        body: r.body,
        ..Default::default()
    };
    // tasks carry one server stamp: when they were closed
    if entry.is_closed() && entry.resolved_at.trim().is_empty() {
        if let Ok(Some(old)) = st.store.get(&entry.key) {
            entry.resolved_at = old.resolved_at;
        }
        if entry.resolved_at.trim().is_empty() {
            entry.resolved_at = now_utc();
        }
    }
    let mut unknown = vec![];
    for k in &entry.knowledge {
        if model::is_valid_key(k) && !matches!(st.store.get(k), Ok(Some(_))) {
            unknown.push(k.clone());
        }
    }
    let extra = if unknown.is_empty() {
        Value::Null
    } else {
        json!({"unknown_knowledge": unknown})
    };
    let closed = entry.is_closed();
    let key = entry.key.clone();
    let mut reply = commit_entry(&st, entry, extra).await;
    if closed {
        archive_closed(&st, &key, &mut reply).await;
    }
    reply
}

#[derive(Deserialize)]
struct IncidentsQ {
    status: Option<String>,
    service: Option<String>,
    /// followups=open keeps only reports with unfinished `- [ ]` items —
    /// the loose ends a session can pick up.
    followups: Option<String>,
    limit: Option<usize>,
}

/// Open followups of a body: `- [ ]` checklist items (same rule as
/// Entry::open_followups, but hits carry plain text).
fn open_followups_of(body: &str) -> usize {
    body.lines().filter(|l| l.trim_start().starts_with("- [ ]")).count()
}

/// Listings come from the index, not the tree: archived (closed) entries are
/// part of the record. Open first, fresher on top within a group.
fn list_kind(
    st: &AppState,
    kind: &str,
    status_order: &[&str],
    q: &IncidentsQ,
) -> Result<Vec<index::Hit>, Reply> {
    // an empty ?status= from the CLI is "no filter", not "match nothing"
    let status = q.status.as_deref().filter(|s| !s.trim().is_empty());
    let service = q.service.as_deref().filter(|s| !s.trim().is_empty());
    let only_open_followups = q.followups.as_deref() == Some("open");
    let opts = index::SearchOpts {
        limit: 500,
        kind: Some(kind.to_string()),
        status: status.map(String::from),
        service: service.map(String::from),
        ..Default::default()
    };
    let mut hits = match st.index.search("", &opts) {
        Err(e) => return Err(err500(e)),
        Ok(h) => h,
    };
    if only_open_followups {
        hits.retain(|h| open_followups_of(&h.body) > 0);
    }
    let rank = |s: &str| status_order.iter().position(|x| *x == s).unwrap_or(9);
    hits.sort_by(|a, b| {
        rank(&a.status)
            .cmp(&rank(&b.status))
            .then(b.updated_at.cmp(&a.updated_at))
            .then(a.key.cmp(&b.key))
    });
    hits.truncate(q.limit.unwrap_or(50).min(200));
    Ok(hits)
}

/// `archived` on a listing row: the file is gone from the tree, the entry
/// lives on in the index and in git history.
fn is_archived(st: &AppState, key: &str) -> bool {
    !matches!(st.store.get(key), Ok(Some(_)))
}

async fn list_incidents(State(st): St, Query(q): Query<IncidentsQ>) -> Reply {
    let hits = match list_kind(&st, model::KIND_INCIDENT, &model::STATUSES, &q) {
        Err(r) => return r,
        Ok(h) => h,
    };
    let rows: Vec<Value> = hits
        .iter()
        .map(|h| {
            json!({
                "key": h.key, "title": h.title, "service": h.service, "hosts": h.hosts,
                "severity": h.severity, "status": h.status, "knowledge": h.knowledge,
                "resolution": h.resolution, "detection": h.detection, "affected": h.affected,
                "started_at": h.started_at, "detected_at": h.detected_at,
                "mitigated_at": h.mitigated_at, "resolved_at": h.resolved_at,
                "open_followups": open_followups_of(&h.body),
                "tags": h.tags, "updated_at": h.updated_at,
                "archived": is_archived(&st, &h.key),
            })
        })
        .collect();
    (StatusCode::OK, Json(json!({"count": rows.len(), "incidents": rows})))
}

async fn list_tasks(State(st): St, Query(q): Query<IncidentsQ>) -> Reply {
    let hits = match list_kind(&st, model::KIND_TASK, &model::TASK_STATUSES, &q) {
        Err(r) => return r,
        Ok(h) => h,
    };
    let rows: Vec<Value> = hits
        .iter()
        .map(|h| {
            json!({
                "key": h.key, "title": h.title, "status": h.status,
                "knowledge": h.knowledge, "resolution": h.resolution,
                "resolved_at": h.resolved_at,
                "open_followups": open_followups_of(&h.body),
                "tags": h.tags, "updated_at": h.updated_at,
                "archived": is_archived(&st, &h.key),
            })
        })
        .collect();
    (StatusCode::OK, Json(json!({"count": rows.len(), "tasks": rows})))
}

#[derive(Deserialize)]
struct GetQ {
    at: Option<String>,
}

fn bad_key(key: &str) -> Option<Reply> {
    if model::is_valid_key(key) {
        None
    } else {
        Some((StatusCode::BAD_REQUEST, Json(json!({"error": "invalid key (slug [a-z0-9-])"}))))
    }
}

fn entry_json(e: &model::Entry, archived: bool) -> Value {
    let mut out = json!({
        "key": e.key, "title": e.title, "kind": e.kind, "tags": e.tags, "refs": e.refs,
        "updated_at": e.updated_at, "body": e.body,
    });
    let obj = out.as_object_mut().expect("out is an object");
    if e.is_incident() {
        obj.insert("service".into(), json!(e.service));
        obj.insert("hosts".into(), json!(e.hosts));
        obj.insert("severity".into(), json!(e.severity));
        obj.insert("detection".into(), json!(e.detection));
        obj.insert("affected".into(), json!(e.affected));
        obj.insert("started_at".into(), json!(e.started_at));
        obj.insert("detected_at".into(), json!(e.detected_at));
        obj.insert("mitigated_at".into(), json!(e.mitigated_at));
    }
    if e.is_incident() || e.is_task() {
        obj.insert("status".into(), json!(e.status));
        obj.insert("knowledge".into(), json!(e.knowledge));
        obj.insert("resolution".into(), json!(e.resolution));
        obj.insert("resolved_at".into(), json!(e.resolved_at));
        obj.insert("open_followups".into(), json!(e.open_followups()));
    }
    if archived {
        obj.insert("archived".into(), json!(true));
    }
    out
}

async fn get_one(State(st): St, Path(key): Path<String>, Query(q): Query<GetQ>) -> Reply {
    if let Some(r) = bad_key(&key) {
        return r;
    }
    let res = match &q.at {
        Some(rev) => st.store.get_at(&key, rev),
        None => st.store.get(&key),
    };
    match res {
        Err(e) => err500(e),
        Ok(Some(e)) => (StatusCode::OK, Json(entry_json(&e, false))),
        Ok(None) if q.at.is_none() => {
            // not in the tree — an archived incident/task is still readable
            // from history; deleted knowledge stays a 404 (retraction)
            match st.store.latest_version(&key) {
                Err(e) => err500(e),
                Ok(Some(e)) if e.kind != model::KIND_KNOWLEDGE => {
                    (StatusCode::OK, Json(entry_json(&e, true)))
                }
                _ => (StatusCode::NOT_FOUND, Json(json!({"error": format!("no entry '{key}'")}))),
            }
        }
        Ok(None) => {
            (StatusCode::NOT_FOUND, Json(json!({"error": format!("no entry '{key}'")})))
        }
    }
}

async fn history(State(st): St, Path(key): Path<String>) -> Reply {
    if let Some(r) = bad_key(&key) {
        return r;
    }
    match st.store.history(&key) {
        Err(e) => err500(e),
        Ok(v) if v.is_empty() => {
            (StatusCode::NOT_FOUND, Json(json!({"error": format!("no history for '{key}'")})))
        }
        Ok(v) => (StatusCode::OK, Json(json!({"key": key, "versions": v}))),
    }
}

#[derive(Deserialize)]
struct SearchQ {
    q: Option<String>,
    tag: Option<String>,
    history: Option<bool>,
    limit: Option<usize>,
    /// sort=recent orders by commit time instead of relevance
    sort: Option<String>,
    /// semantic=false forces pure lexical search
    semantic: Option<bool>,
    /// kind=knowledge|incident; absent = both
    kind: Option<String>,
    /// incident filters (exact terms)
    status: Option<String>,
    service: Option<String>,
}

async fn search(State(st): St, Query(p): Query<SearchQ>) -> Reply {
    let tags: Vec<String> = p
        .tag
        .map(|t| t.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    let q = p.q.as_deref().unwrap_or("");
    let limit = p.limit.unwrap_or(10).min(100);
    let recent = p.sort.as_deref() == Some("recent");
    let history = p.history.unwrap_or(false);
    // Semantic retrieval covers current knowledge only: history questions are
    // "what did this say back then", which is a lexical, not a fuzzy, ask.
    let want_semantic =
        !recent && !history && !q.trim().is_empty() && st.semantic.is_some() && p.semantic != Some(false);
    // an empty ?kind= from the CLI is "no filter", not "match nothing"
    let norm = |o: &Option<String>| o.clone().filter(|s| !s.trim().is_empty());
    let opts = index::SearchOpts {
        tags: tags.clone(),
        history,
        limit: if want_semantic { limit.max(24) } else { limit },
        recent,
        kind: norm(&p.kind),
        status: norm(&p.status),
        service: norm(&p.service),
    };
    let mut hits = match st.index.search(q, &opts) {
        Err(e) => return err500(e),
        Ok(h) => h,
    };
    let mut semantic_used = false;
    if want_semantic {
        semantic_used = hybrid(&st, q, &opts, limit, &mut hits).await;
    }
    hits.truncate(limit);
    let count = hits.len();
    (StatusCode::OK, Json(json!({"count": count, "semantic": semantic_used, "hits": hits})))
}

/// Blend lexical hits with vector search over the whole base.
///
/// The vector side can contribute entries BM25 never saw — that is the point:
/// a question phrased in Russian, or in words the entry does not use, has no
/// lexical anchor at all. Failure is never fatal; the lexical order is already
/// a valid answer, so a broken model just means plain BM25.
async fn hybrid(
    st: &Arc<AppState>,
    q: &str,
    opts: &index::SearchOpts,
    limit: usize,
    hits: &mut Vec<index::Hit>,
) -> bool {
    let Some(sem) = st.semantic.as_ref() else { return false };
    let scored = match sem.search(q, limit.max(24)).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("kyb: semantic search skipped: {e:#}");
            return false;
        }
    };
    let sims: std::collections::HashMap<&str, f32> =
        scored.iter().map(|(k, s)| (k.as_str(), *s)).collect();
    let lexical_keys: Vec<String> = hits.iter().map(|h| h.key.clone()).collect();
    let mut by_key: std::collections::HashMap<String, index::Hit> =
        hits.drain(..).map(|h| (h.key.clone(), h)).collect();

    for key in embed::fuse_lists(&lexical_keys, &scored, 0.6) {
        let hit = match by_key.remove(&key) {
            Some(h) => Some(h),
            // semantic-only candidate: materialize its live doc (tree version
            // or archived latest), but only if it passes the same filters the
            // lexical side applied
            None => match st.index.get_live(&key) {
                Ok(Some(h)) => {
                    let keeps = opts
                        .tags
                        .iter()
                        .all(|t| h.tags.iter().any(|x| x.eq_ignore_ascii_case(t)))
                        && opts.kind.as_deref().is_none_or(|k| h.kind == k)
                        && opts.status.as_deref().is_none_or(|s| h.status == s)
                        && opts
                            .service
                            .as_deref()
                            .is_none_or(|s| h.service.eq_ignore_ascii_case(s));
                    keeps.then_some(h)
                }
                _ => None,
            },
        };
        if let Some(mut h) = hit {
            // report similarity when we have one: it explains the placement
            if let Some(s) = sims.get(h.key.as_str()) {
                h.score = *s;
            }
            hits.push(h);
            if hits.len() >= limit {
                break;
            }
        }
    }
    true
}

/// Which topics does the base actually cover? Without this an agent can only
/// filter by a tag it already guessed. Counted from the canon, not the index.
async fn tags(State(st): St) -> Reply {
    let entries = match st.store.list_head() {
        Err(e) => return err500(e),
        Ok(v) => v,
    };
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for e in &entries {
        for t in &e.tags {
            *counts.entry(t.to_lowercase()).or_default() += 1;
        }
    }
    let mut rows: Vec<(String, usize)> = counts.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let tags: Vec<Value> = rows.into_iter().map(|(t, c)| json!({"tag": t, "count": c})).collect();
    (StatusCode::OK, Json(json!({"count": tags.len(), "tags": tags})))
}

/// DELETE by kind: knowledge is retracted (drops out of the default search),
/// an incident/task is archived (its latest version stays searchable).
async fn remove(State(st): St, Path(key): Path<String>) -> Reply {
    if let Some(r) = bad_key(&key) {
        return r;
    }
    let entry = match st.store.get(&key) {
        Err(e) => return err500(e),
        Ok(None) => {
            return (StatusCode::NOT_FOUND, Json(json!({"error": format!("no entry '{key}'")})))
        }
        Ok(Some(e)) => e,
    };
    let mut w = st.writer.lock().await;
    if entry.kind == model::KIND_KNOWLEDGE {
        match st.store.delete(&key) {
            Err(e) => err500(e),
            Ok(None) => {
                (StatusCode::NOT_FOUND, Json(json!({"error": format!("no entry '{key}'")})))
            }
            Ok(Some(sha)) => {
                st.index.delete_head(&mut w, &key);
                if let Err(e) = st.index.commit_and_reload(&mut w) {
                    return err500(
                        e.context("git committed but the index was not updated — run POST /reindex"),
                    );
                }
                drop(w);
                if let Some(sem) = st.semantic.as_ref() {
                    sem.remove(&key).await;
                }
                (StatusCode::OK, Json(json!({"key": key, "deleted": true, "sha": sha})))
            }
        }
    } else {
        // live index doc and vector stay: archived means findable
        match st.store.archive(&key) {
            Err(e) => err500(e),
            Ok(None) => {
                (StatusCode::NOT_FOUND, Json(json!({"error": format!("no entry '{key}'")})))
            }
            Ok(Some(sha)) => {
                (StatusCode::OK, Json(json!({"key": key, "archived": true, "sha": sha})))
            }
        }
    }
}

async fn reindex(State(st): St) -> Reply {
    let mut w = st.writer.lock().await;
    let res = st.index.reindex(&mut w, &st.store);
    drop(w);
    match res {
        Err(e) => err500(e),
        Ok((heads, hist)) => {
            rebuild_vectors(&st).await;
            (StatusCode::OK, Json(json!({"ok": true, "head_docs": heads, "history_docs": hist})))
        }
    }
}

async fn healthz(State(st): St) -> Reply {
    let heads = st.store.list_head().unwrap_or_default();
    let open_incidents =
        heads.iter().filter(|e| e.is_incident() && e.status == "open").count();
    let open_tasks = heads.iter().filter(|e| e.is_task() && e.status == "open").count();
    let index_docs = st.index.reader.searcher().num_docs();
    let last = st.store.head_info().ok().flatten();
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "entries": heads.len(),
            "open_incidents": open_incidents,
            "open_tasks": open_tasks,
            "index_docs": index_docs,
            "last_commit": last.map(|(sha, time)| json!({"sha": sha, "time": time})),
        })),
    )
}

#[cfg(test)]
mod api_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use rstest::rstest;
    use tower::util::ServiceExt;

    async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let b = Request::builder().method(method).uri(uri);
        let req = match body {
            Some(v) => b
                .header("content-type", "application/json")
                .body(Body::from(v.to_string()))
                .unwrap(),
            None => b.body(Body::empty()).unwrap(),
        };
        let resp = app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let val: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, val)
    }

    // The title is Russian on purpose: one API-level ru-stem check below
    // searches «стримах» and must match «стримы» from this title.
    fn upsert_body(key: &str, body: &str) -> Value {
        json!({"key": key, "title": "NATS стримы", "body": body, "tags": ["nats"]})
    }

    fn app_with_tmp() -> (Router, tempfile::TempDir, tempfile::TempDir) {
        let data = tempfile::tempdir().unwrap();
        let idx = tempfile::tempdir().unwrap();
        let cfg = config::Config {
            data_dir: data.path().to_path_buf(),
            index_dir: idx.path().to_path_buf(),
            audit_path: idx.path().join("audit.jsonl"),
            // API tests assert lexical behaviour; point at a dir with no model
            model_dir: idx.path().join("no-model"),
            addr: String::new(),
        };
        (build_app(build_state(&cfg).unwrap()), data, idx)
    }

    #[tokio::test]
    async fn api_full_flow() {
        let (app, _data, _idx) = app_with_tmp();

        // healthz on an empty registry
        let (st, v) = call(&app, "GET", "/healthz", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["entries"], 0);

        // create
        let (st, v) = call(&app, "POST", "/knowledge", Some(upsert_body("nats-streams", "Primary version about streams."))).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert_eq!(v["action"], "created");
        let sha1 = v["sha"].as_str().unwrap().to_string();
        assert_eq!(sha1.len(), 40);

        // same content -> no-op
        let (_, v) = call(&app, "POST", "/knowledge", Some(upsert_body("nats-streams", "Primary version about streams."))).await;
        assert_eq!(v["changed"], false);

        // update -> new sha
        let (_, v) = call(&app, "POST", "/knowledge", Some(upsert_body("nats-streams", "Secondary version: the rule moved."))).await;
        assert_eq!(v["action"], "updated");
        let sha2 = v["sha"].as_str().unwrap().to_string();
        assert_ne!(sha1, sha2);

        // get current
        let (st, v) = call(&app, "GET", "/knowledge/nats-streams", None).await;
        assert_eq!(st, StatusCode::OK);
        assert!(v["body"].as_str().unwrap().contains("Secondary"));

        // get old version by sha
        let (_, v) = call(&app, "GET", &format!("/knowledge/nats-streams?at={sha1}"), None).await;
        assert!(v["body"].as_str().unwrap().contains("Primary"));

        // history: 2 versions
        let (_, v) = call(&app, "GET", "/knowledge/nats-streams/history", None).await;
        assert_eq!(v["versions"].as_array().unwrap().len(), 2);

        // API-level russian stem: query «стримах» matches «стримы» in the title
        let (_, v) = call(&app, "GET", "/search?q=%D1%81%D1%82%D1%80%D0%B8%D0%BC%D0%B0%D1%85", None).await;
        assert_eq!(v["count"], 1);
        assert_eq!(v["hits"][0]["key"], "nats-streams");
        assert_eq!(v["hits"][0]["is_head"], true);

        // old text: absent in HEAD, present in history
        let q_old = "/search?q=Primary";
        let (_, v) = call(&app, "GET", q_old, None).await;
        assert_eq!(v["count"], 0);
        let (_, v) = call(&app, "GET", &format!("{q_old}&history=true"), None).await;
        assert!(v["count"].as_u64().unwrap() >= 1);
        assert_eq!(v["hits"][0]["is_head"], false);
        assert_eq!(v["hits"][0]["sha"], sha1);

        // tag filter
        let (_, v) = call(&app, "GET", "/search?q=nats&tag=nats", None).await;
        assert_eq!(v["count"], 1);
        let (_, v) = call(&app, "GET", "/search?q=nats&tag=missing", None).await;
        assert_eq!(v["count"], 0);

        // delete: head goes away, history remains searchable
        let (_, v) = call(&app, "DELETE", "/knowledge/nats-streams", None).await;
        assert_eq!(v["deleted"], true);
        let (st, _) = call(&app, "GET", "/knowledge/nats-streams", None).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let (_, v) = call(&app, "GET", "/search?q=nats", None).await;
        assert_eq!(v["count"], 0);
        let (_, v) = call(&app, "GET", "/search?q=nats&history=true", None).await;
        assert!(v["count"].as_u64().unwrap() >= 2);

        // reindex from git restores the picture
        let (_, v) = call(&app, "POST", "/reindex", None).await;
        assert_eq!(v["ok"], true);
        assert_eq!(v["head_docs"], 0);
        assert_eq!(v["history_docs"], 2);

        // validation: bad key and secret in body -> 400
        let (st, _) = call(&app, "POST", "/knowledge", Some(json!({"key": "Bad Key", "title": "t", "body": "x"}))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        let (st, v) = call(&app, "POST", "/knowledge", Some(json!({"key": "leak", "title": "t", "body": "token: ghp_abcdefghijklmnopqrstuvwxyz123456"}))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert!(v["error"].as_str().unwrap().contains("secret"));

        // 404 on missing
        let (st, _) = call(&app, "GET", "/knowledge/nope", None).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let (st, _) = call(&app, "GET", "/knowledge/nope/history", None).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn api_edge_cases() {
        let (app, _data, _idx) = app_with_tmp();

        call(&app, "POST", "/knowledge", Some(upsert_body("alpha", "body one"))).await;

        // unknown/garbage rev in ?at= -> 404, not 500
        let (st, _) = call(
            &app,
            "GET",
            "/knowledge/alpha?at=deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            None,
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let (st, _) = call(&app, "GET", "/knowledge/alpha?at=zzz", None).await;
        assert_eq!(st, StatusCode::NOT_FOUND);

        // delete -> re-add with the same key: created again, chain kept in history
        call(&app, "DELETE", "/knowledge/alpha", None).await;
        let (_, v) = call(&app, "POST", "/knowledge", Some(upsert_body("alpha", "body two"))).await;
        assert_eq!(v["action"], "created");
        let (_, v) = call(&app, "GET", "/knowledge/alpha/history", None).await;
        let changes: Vec<&str> = v["versions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["change"].as_str().unwrap())
            .collect();
        assert_eq!(changes, vec!["added", "deleted", "added"]);

        // empty q = list everything current; limit works
        call(&app, "POST", "/knowledge", Some(upsert_body("beta", "another entry"))).await;
        let (_, v) = call(&app, "GET", "/search?q=", None).await;
        assert_eq!(v["count"], 2);
        let (_, v) = call(&app, "GET", "/search?q=&limit=1", None).await;
        assert_eq!(v["count"], 1);

        // broken query syntax must not fail the search (lenient parser)
        let (st, _) = call(&app, "GET", "/search?q=title%3A%28%28%28%20AND%20OR", None).await;
        assert_eq!(st, StatusCode::OK);

        // tags are case-insensitive
        call(
            &app,
            "POST",
            "/knowledge",
            Some(json!({"key": "gamma", "title": "t", "body": "tagged body", "tags": ["Infra"]})),
        )
        .await;
        let (_, v) = call(&app, "GET", "/search?q=&tag=infra", None).await;
        assert_eq!(v["count"], 1);

        // garbage payload -> 4xx, not 500
        let (st, _) = call(&app, "POST", "/knowledge", Some(json!({"key": "x"}))).await;
        assert!(st.is_client_error());

        // rm of a missing key -> 404
        let (st, _) = call(&app, "DELETE", "/knowledge/void", None).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn audit_log_written() {
        let (app, _data, idx) = app_with_tmp();
        call(&app, "GET", "/healthz", None).await;
        call(&app, "POST", "/knowledge", Some(upsert_body("k", "plain body"))).await;
        call(&app, "GET", "/search?q=plain", None).await;

        let content = std::fs::read_to_string(idx.path().join("audit.jsonl")).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2, "healthz must not be audited:\n{content}");
        let first: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["method"], "POST");
        assert_eq!(first["path"], "/knowledge");
        assert_eq!(first["status"], 200);
        assert!(first["ts"].as_str().unwrap().contains('T'));
        let second: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["path"], "/search");
        assert!(second["query"].as_str().unwrap().starts_with("q="));
    }

    // --- traversal and garbage keys in path params: 400, not a filesystem walk ---
    #[rstest]
    #[case("GET", "/knowledge/..%2F..%2Fetc")]
    #[case("GET", "/knowledge/.git")]
    #[case("GET", "/knowledge/UPPER")]
    #[case("GET", "/knowledge/a.b")]
    #[case("DELETE", "/knowledge/..%2Fx")]
    #[case("DELETE", "/knowledge/a_b")]
    #[case("GET", "/knowledge/..%2Fx/history")]
    #[case("GET", "/knowledge/%D0%BA%D0%BB%D1%8E%D1%87")]
    #[tokio::test]
    async fn traversal_and_bad_keys_rejected(#[case] method: &str, #[case] uri: &str) {
        let (app, _data, _idx) = app_with_tmp();
        let (st, _) = call(&app, method, uri, None).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{method} {uri}");
    }

    // --- status codes per route ---
    #[rstest]
    #[case("GET", "/nope", 404)]
    #[case("POST", "/search", 405)]
    #[case("DELETE", "/search", 405)]
    #[case("GET", "/reindex", 405)]
    #[case("PUT", "/knowledge", 405)]
    #[case("DELETE", "/knowledge", 405)]
    #[case("POST", "/knowledge/some-key", 405)]
    #[case("POST", "/healthz", 405)]
    #[case("GET", "/knowledge/no-such-key", 404)]
    #[case("GET", "/knowledge/no-such-key/history", 404)]
    #[case("DELETE", "/knowledge/no-such-key", 404)]
    #[tokio::test]
    async fn route_status_matrix(#[case] method: &str, #[case] uri: &str, #[case] expect: u16) {
        let (app, _data, _idx) = app_with_tmp();
        let (st, _) = call(&app, method, uri, None).await;
        assert_eq!(st.as_u16(), expect, "{method} {uri}");
    }

    // --- upsert payload validation ---
    #[rstest]
    #[case(json!({"key": "k"}), 422)] // required fields missing
    #[case(json!({"key": "k", "title": "t"}), 422)] // no body
    #[case(json!({"key": "", "title": "t", "body": "x"}), 400)]
    #[case(json!({"key": "K", "title": "t", "body": "x"}), 400)]
    #[case(json!({"key": "../up", "title": "t", "body": "x"}), 400)]
    #[case(json!({"key": "k", "title": "", "body": "x"}), 400)]
    #[case(json!({"key": "k", "title": "   ", "body": "x"}), 400)]
    #[case(json!({"key": "k", "title": "t", "body": "password: super123secret"}), 400)]
    #[case(json!({"key": "k", "title": "t", "body": "x", "refs": ["ghp_abcdefghijklmnopqrstuvwxyz1234"]}), 400)]
    #[case(json!({"key": "k", "title": "t", "body": "clean body"}), 200)]
    #[case(json!({"key": "k2", "title": "t", "body": ""}), 200)] // empty body is allowed
    #[case(json!({"key": "k3", "title": "t", "body": "x", "tags": [], "refs": []}), 200)]
    #[tokio::test]
    async fn upsert_validation_matrix(#[case] payload: Value, #[case] expect: u16) {
        let (app, _data, _idx) = app_with_tmp();
        let (st, v) = call(&app, "POST", "/knowledge", Some(payload)).await;
        assert_eq!(st.as_u16(), expect, "response: {v}");
    }

    // 12 parallel writes to ONE key: the mutex serializes them, history is complete
    #[tokio::test]
    async fn api_concurrent_same_key() {
        let (app, _data, _idx) = app_with_tmp();
        let mut set = tokio::task::JoinSet::new();
        for i in 0..12 {
            let app = app.clone();
            set.spawn(async move {
                let body = json!({
                    "key": "same-key",
                    "title": "Shared key",
                    "body": format!("unique body {i}"),
                });
                call(&app, "POST", "/knowledge", Some(body)).await
            });
        }
        while let Some(res) = set.join_next().await {
            let (st, v) = res.unwrap();
            assert_eq!(st, StatusCode::OK, "{v}");
            assert_eq!(v["changed"], true);
        }
        // all 12 versions landed in git sequentially, none lost
        let (_, v) = call(&app, "GET", "/knowledge/same-key/history", None).await;
        assert_eq!(v["versions"].as_array().unwrap().len(), 12);
        let (_, v) = call(&app, "POST", "/reindex", None).await;
        assert_eq!(v["head_docs"], 1);
        assert_eq!(v["history_docs"], 12);
        // the current body is one of the twelve
        let (_, v) = call(&app, "GET", "/knowledge/same-key", None).await;
        assert!(v["body"].as_str().unwrap().starts_with("unique body"));
    }

    fn incident_body(key: &str, status: &str, body: &str) -> Value {
        json!({
            "key": key, "title": "orders_api OOM on host-a", "body": body,
            "service": "orders_api", "hosts": ["host-a"], "severity": "high", "status": status,
            "knowledge": ["orders-api-architecture"], "tags": ["acme"],
        })
    }

    #[tokio::test]
    async fn incident_full_flow() {
        let (app, _data, _idx) = app_with_tmp();

        // the knowledge entry the incident will link to
        call(&app, "POST", "/knowledge", Some(json!({
            "key": "orders-api-architecture", "title": "orders_api: gRPC gateway",
            "body": "The single write path into ClickHouse.", "tags": ["acme"],
        }))).await;

        // file a report; the link resolves, so no unknown_knowledge warning
        let key = "inc-2026-07-22-orders-api-oom";
        let (st, v) = call(&app, "POST", "/incidents",
            Some(incident_body(key, "open", "What happened: OOM. Workaround: restart."))).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert_eq!(v["action"], "created");
        assert!(v.get("unknown_knowledge").is_none(), "{v}");

        // a dangling knowledge link is allowed but reported
        let (_, v) = call(&app, "POST", "/incidents", Some(json!({
            "key": "inc-2026-07-21-landing-gap", "title": "landing gap", "body": "Data gap.",
            "service": "web_app", "severity": "medium",
            "knowledge": ["landing-architecture"],
        }))).await;
        assert_eq!(v["unknown_knowledge"], json!(["landing-architecture"]));

        // status omitted -> open
        let (_, v) = call(&app, "GET", &format!("/knowledge/inc-2026-07-21-landing-gap"), None).await;
        assert_eq!(v["status"], "open");
        assert_eq!(v["kind"], "incident");

        // GET returns the incident fields
        let (_, v) = call(&app, "GET", &format!("/knowledge/{key}"), None).await;
        assert_eq!(v["service"], "orders_api");
        assert_eq!(v["severity"], "high");
        assert_eq!(v["hosts"], json!(["host-a"]));
        assert_eq!(v["knowledge"], json!(["orders-api-architecture"]));

        // a knowledge entry does not grow incident fields
        let (_, v) = call(&app, "GET", "/knowledge/orders-api-architecture", None).await;
        assert_eq!(v["kind"], "knowledge");
        assert!(v.get("status").is_none(), "{v}");

        // healthz counts open incidents
        let (_, v) = call(&app, "GET", "/healthz", None).await;
        assert_eq!(v["open_incidents"], 2);

        // list: open first, filters work; empty params (CLI style) = no filter
        let (_, v) = call(&app, "GET", "/incidents", None).await;
        assert_eq!(v["count"], 2);
        let (_, v) = call(&app, "GET", "/incidents?status=&service=&limit=50", None).await;
        assert_eq!(v["count"], 2, "{v}");
        let (_, v) = call(&app, "GET", "/incidents?service=ORDERS_API", None).await;
        assert_eq!(v["count"], 1, "service filter is case-insensitive: {v}");
        assert_eq!(v["incidents"][0]["key"], key);

        // search: kind filter separates worlds; incident fields ride on hits
        let (_, v) = call(&app, "GET", "/search?q=&kind=incident", None).await;
        assert_eq!(v["count"], 2);
        let (_, v) = call(&app, "GET", "/search?q=&kind=knowledge", None).await;
        assert_eq!(v["count"], 1);
        // empty filter params (what the CLI sends) mean "no filter", not "match nothing"
        let (_, v) = call(&app, "GET", "/search?q=&kind=&status=&service=", None).await;
        assert_eq!(v["count"], 3);
        // free text "orders api" reaches the incident via service/knowledge meta
        let (_, v) = call(&app, "GET", "/search?q=orders%20api&kind=incident&status=open&service=orders_api", None).await;
        assert_eq!(v["count"], 1, "{v}");
        assert_eq!(v["hits"][0]["key"], key);
        assert_eq!(v["hits"][0]["status"], "open");

        // closing without saying how it ended is refused
        let (st, v) = call(&app, "POST", &format!("/incidents/{key}/resolve"), Some(json!({}))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
        assert!(v["error"].as_str().unwrap().contains("resolution"));

        // close it properly: status flips, the outcome is recorded, and the
        // report is archived — the file leaves the tree
        let (st, v) = call(&app, "POST", &format!("/incidents/{key}/resolve"),
            Some(json!({"resolution": "Raised the memory limit to 2G; fixed the batch flush leak."}))).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert_eq!(v["action"], "updated");
        assert_eq!(v["archived"], true, "{v}");
        let (_, v) = call(&app, "GET", "/incidents?status=open", None).await;
        assert_eq!(v["count"], 1);
        let (_, v) = call(&app, "GET", "/healthz", None).await;
        assert_eq!(v["open_incidents"], 1);
        // the archived report still reads in full, marked as archived
        let (_, v) = call(&app, "GET", &format!("/knowledge/{key}"), None).await;
        assert_eq!(v["status"], "resolved");
        assert_eq!(v["archived"], true, "{v}");
        assert!(v["resolution"].as_str().unwrap().contains("memory limit"));
        assert_eq!(v["service"], "orders_api");
        assert_eq!(v["knowledge"], json!(["orders-api-architecture"]));
        assert!(v["body"].as_str().unwrap().contains("What happened"));
        // resolved incidents stay in the listing (archived), at the bottom
        let (_, v) = call(&app, "GET", "/incidents", None).await;
        assert_eq!(v["count"], 2);
        assert_eq!(v["incidents"][1]["key"], key);
        assert_eq!(v["incidents"][1]["status"], "resolved");
        assert_eq!(v["incidents"][1]["archived"], true);
        // history: filed, resolved, archived
        let (_, v) = call(&app, "GET", &format!("/knowledge/{key}/history"), None).await;
        assert_eq!(v["versions"].as_array().unwrap().len(), 3);
        assert_eq!(v["versions"][0]["change"], "deleted");
        assert!(v["versions"][0]["message"].as_str().unwrap().contains("archive"));

        // the recorded outcome is searchable in the DEFAULT search even though
        // the file is gone — "how did we fix it" lands here
        let (_, v) = call(&app, "GET", "/search?q=memory%20limit&kind=incident", None).await;
        assert_eq!(v["count"], 1, "{v}");
        assert_eq!(v["hits"][0]["resolution"].as_str().unwrap().contains("2G"), true);
        assert_eq!(v["hits"][0]["is_head"], true);

        // parking an archived report back to mitigated reopens it (the file
        // returns); the empty resolution keeps the recorded outcome
        let (_, v) = call(&app, "POST", &format!("/incidents/{key}/resolve"),
            Some(json!({"status": "mitigated"}))).await;
        assert_eq!(v["action"], "created", "reopen recreates the file: {v}");
        let (_, v) = call(&app, "GET", &format!("/knowledge/{key}"), None).await;
        assert_eq!(v["status"], "mitigated");
        assert!(v.get("archived").is_none(), "back in the tree: {v}");
        assert!(v["resolution"].as_str().unwrap().contains("memory limit"), "kept: {v}");
    }

    // detection / affected windows / timeline: the "control panel" fields
    #[tokio::test]
    async fn incident_actionable_fields_flow() {
        let (app, _data, _idx) = app_with_tmp();
        let key = "inc-2026-07-22-symbol-mismap";
        let (st, v) = call(&app, "POST", "/incidents", Some(json!({
            "key": key, "title": "prices 50x off after symbol remap", "service": "web_app2",
            "hosts": ["host-b"], "severity": "high",
            "detection": "per (exchange,symbol): price > 50x yesterday max; healthy = 0 rows",
            "affected": [
                {"scope": "okx",    "from": "2026-07-22T08:09:40Z", "to": "2026-07-22T21:09:37Z"},
                {"scope": "gateio", "from": "2026-07-22T08:20:09Z", "to": "2026-07-22T20:04:41Z"},
            ],
            "started_at": "2026-07-22T08:09:40Z",
            "body": "Symptom: heatmap shows +38,000,000% gainers.\n\nFollow-ups:\n- [ ] guard dev NATS\n- [x] webui sanity filter\n",
        }))).await;
        assert_eq!(st, StatusCode::OK, "{v}");

        // GET: everything back; detected_at was stamped by the server
        let (_, v) = call(&app, "GET", &format!("/knowledge/{key}"), None).await;
        assert_eq!(v["affected"].as_array().unwrap().len(), 2);
        assert_eq!(v["affected"][0]["scope"], "okx");
        assert_eq!(v["started_at"], "2026-07-22T08:09:40Z");
        assert!(v["detected_at"].as_str().unwrap().contains('T'), "stamped: {v}");
        assert_eq!(v["resolved_at"], "");
        assert_eq!(v["open_followups"], 1);

        // listing carries the control-panel fields and the followups filter works
        let (_, v) = call(&app, "GET", "/incidents", None).await;
        assert!(v["incidents"][0]["detection"].as_str().unwrap().contains("50x"));
        assert_eq!(v["incidents"][0]["open_followups"], 1);
        let (_, v) = call(&app, "GET", "/incidents?followups=open", None).await;
        assert_eq!(v["count"], 1);

        // search hit carries them too (backtester can read windows from a hit)
        let (_, v) = call(&app, "GET", "/search?q=mismap&kind=incident", None).await;
        assert_eq!(v["hits"][0]["affected"].as_array().unwrap().len(), 2, "{v}");
        assert!(v["hits"][0]["detection"].as_str().unwrap().contains("healthy"));

        // detection text is searchable
        let (_, v) = call(&app, "GET", "/search?q=yesterday%20max", None).await;
        assert_eq!(v["count"], 1);

        // resolve with an open follow-up: closes (and archives), but warns
        let (_, v) = call(&app, "POST", &format!("/incidents/{key}/resolve"),
            Some(json!({"resolution": "Recorders restarted; windows purged from CH."}))).await;
        assert_eq!(v["open_followups"], 1, "{v}");
        assert!(v["warning"].as_str().unwrap().contains("follow-up"));
        assert_eq!(v["archived"], true);
        let (_, v) = call(&app, "GET", &format!("/knowledge/{key}"), None).await;
        assert!(v["resolved_at"].as_str().unwrap().contains('T'));
        let (_, v) = call(&app, "GET", "/incidents?followups=open", None).await;
        assert_eq!(v["count"], 1, "resolved but loose ends still listed");

        // wholesale re-add without timestamps must NOT wipe server stamps,
        // even across the archive boundary
        let (_, v) = call(&app, "POST", "/incidents", Some(json!({
            "key": key, "title": "prices 50x off after symbol remap", "service": "web_app2",
            "hosts": ["host-b"], "severity": "high", "status": "resolved",
            "resolution": "Recorders restarted; windows purged from CH.",
            "body": "Everything from before, follow-ups all done:\n- [x] guard dev NATS\n",
        }))).await;
        assert_eq!(v["action"], "created", "re-filing an archived report recreates the file: {v}");
        assert_eq!(v["archived"], true, "and a closed status archives it again: {v}");
        let (_, v) = call(&app, "GET", &format!("/knowledge/{key}"), None).await;
        assert_eq!(v["started_at"], "2026-07-22T08:09:40Z", "inherited: {v}");
        assert!(v["resolved_at"].as_str().unwrap().contains('T'), "inherited: {v}");
        assert_eq!(v["open_followups"], 0);
        let (_, v) = call(&app, "GET", "/incidents?followups=open", None).await;
        assert_eq!(v["count"], 0);
    }

    // a mitigated transition stamps mitigated_at
    #[tokio::test]
    async fn mitigated_timestamp_stamped() {
        let (app, _data, _idx) = app_with_tmp();
        let key = "inc-2026-07-23-nats-lag";
        call(&app, "POST", "/incidents", Some(json!({
            "key": key, "title": "lag", "body": "x", "service": "nats", "severity": "low",
        }))).await;
        call(&app, "POST", &format!("/incidents/{key}/resolve"),
            Some(json!({"status": "mitigated", "resolution": "hourly restart cron while the fix bakes"}))).await;
        let (_, v) = call(&app, "GET", &format!("/knowledge/{key}"), None).await;
        assert_eq!(v["status"], "mitigated");
        assert!(v["mitigated_at"].as_str().unwrap().contains('T'), "{v}");
        assert_eq!(v["resolved_at"], "");
    }

    #[rstest]
    #[case::window_missing_to(json!({"key": "inc-w", "title": "t", "body": "x", "service": "s", "severity": "low",
        "affected": [{"scope": "okx", "from": "2026-07-22T08:00:00Z", "to": ""}]}), 400)]
    #[case::window_ok(json!({"key": "inc-w", "title": "t", "body": "x", "service": "s", "severity": "low",
        "affected": [{"scope": "okx", "from": "2026-07-22T08:00:00Z", "to": "2026-07-22T09:00:00Z"}]}), 200)]
    #[tokio::test]
    async fn affected_window_validation(#[case] payload: Value, #[case] expect: u16) {
        let (app, _data, _idx) = app_with_tmp();
        let (st, v) = call(&app, "POST", "/incidents", Some(payload)).await;
        assert_eq!(st.as_u16(), expect, "response: {v}");
    }

    // a bare report is accepted but told what a complete one carries;
    // a structured one gets no hints
    #[tokio::test]
    async fn structure_hints() {
        let (app, _data, _idx) = app_with_tmp();
        let (st, v) = call(&app, "POST", "/incidents", Some(json!({
            "key": "inc-2026-07-23-bare", "title": "t", "body": "something broke",
            "service": "s", "severity": "low",
        }))).await;
        assert_eq!(st, StatusCode::OK);
        let hints = v["hints"].as_array().unwrap();
        assert_eq!(hints.len(), 4, "{v}");
        assert!(hints.iter().any(|h| h.as_str().unwrap().contains("detection")));

        let (_, v) = call(&app, "POST", "/incidents", Some(json!({
            "key": "inc-2026-07-23-full", "title": "t",
            "body": "Symptom: x.\nRoot cause (verified): y.\nFollow-ups:\n- [ ] z\n",
            "service": "s", "severity": "low",
            "detection": "check: 0 rows when healthy",
            "affected": [{"scope": "a", "from": "b", "to": "c"}],
        }))).await;
        assert!(v.get("hints").is_none(), "{v}");
    }

    // resolve endpoint edge cases: missing key, not an incident
    #[tokio::test]
    async fn resolve_edge_cases() {
        let (app, _data, _idx) = app_with_tmp();
        let (st, _) = call(&app, "POST", "/incidents/inc-nope/resolve",
            Some(json!({"resolution": "x"}))).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        call(&app, "POST", "/knowledge", Some(upsert_body("plain", "a fact"))).await;
        let (st, v) = call(&app, "POST", "/knowledge/../resolve", Some(json!({"resolution": "x"}))).await;
        assert!(st.is_client_error(), "{v}");
        let (st, v) = call(&app, "POST", "/incidents/plain/resolve",
            Some(json!({"resolution": "x"}))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
        assert!(v["error"].as_str().unwrap().contains("not an incident"));
    }

    // --- incident payload validation ---
    #[rstest]
    #[case::key_without_prefix(json!({"key": "orders-api-oom", "title": "t", "body": "x", "service": "s", "severity": "high"}), 400)]
    #[case::missing_service(json!({"key": "inc-a", "title": "t", "body": "x", "severity": "high"}), 422)]
    #[case::empty_service(json!({"key": "inc-a", "title": "t", "body": "x", "service": " ", "severity": "high"}), 400)]
    #[case::bad_severity(json!({"key": "inc-a", "title": "t", "body": "x", "service": "s", "severity": "huge"}), 400)]
    #[case::bad_status(json!({"key": "inc-a", "title": "t", "body": "x", "service": "s", "severity": "low", "status": "wip"}), 400)]
    #[case::bad_knowledge(json!({"key": "inc-a", "title": "t", "body": "x", "service": "s", "severity": "low", "knowledge": ["Bad Key"]}), 400)]
    #[case::secret_in_body(json!({"key": "inc-a", "title": "t", "body": "password: super123secret", "service": "s", "severity": "low"}), 400)]
    #[case::resolved_needs_resolution(json!({"key": "inc-a", "title": "t", "body": "x", "service": "s", "severity": "low", "status": "resolved"}), 400)]
    #[case::resolved_with_resolution(json!({"key": "inc-a", "title": "t", "body": "x", "service": "s", "severity": "low", "status": "resolved", "resolution": "fixed by restart"}), 200)]
    #[case::minimal_ok(json!({"key": "inc-a", "title": "t", "body": "x", "service": "s", "severity": "low"}), 200)]
    #[tokio::test]
    async fn incident_validation_matrix(#[case] payload: Value, #[case] expect: u16) {
        let (app, _data, _idx) = app_with_tmp();
        let (st, v) = call(&app, "POST", "/incidents", Some(payload)).await;
        assert_eq!(st.as_u16(), expect, "response: {v}");
    }

    // the inc- namespace is fenced off from plain knowledge writes
    #[tokio::test]
    async fn knowledge_cannot_take_inc_keys() {
        let (app, _data, _idx) = app_with_tmp();
        let (st, v) = call(&app, "POST", "/knowledge",
            Some(json!({"key": "inc-2026-07-22-fake", "title": "t", "body": "x"}))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
        assert!(v["error"].as_str().unwrap().contains("reserved"));
    }

    // filing a report that is already resolved archives it immediately;
    // archived reports stay listed, searchable and readable
    #[tokio::test]
    async fn incident_delete_and_history_search() {
        let (app, _data, _idx) = app_with_tmp();
        let key = "inc-2026-07-20-nats-lag";
        let (_, v) = call(&app, "POST", "/incidents", Some(json!({
            "key": key, "title": "NATS consumer lag", "body": "Consumers fell behind.",
            "service": "nats", "severity": "low", "status": "resolved",
            "resolution": "Consumers caught up after the stream limit was raised.",
        }))).await;
        assert_eq!(v["archived"], true, "{v}");
        // gone from the tree: a second DELETE has nothing to remove
        let (st, _) = call(&app, "DELETE", &format!("/knowledge/{key}"), None).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        // still part of the record: listed, found by default search, readable
        let (_, v) = call(&app, "GET", "/incidents", None).await;
        assert_eq!(v["count"], 1);
        assert_eq!(v["incidents"][0]["archived"], true);
        let (_, v) = call(&app, "GET", "/search?q=consumer&kind=incident", None).await;
        assert_eq!(v["count"], 1, "{v}");
        assert_eq!(v["hits"][0]["is_head"], true);
        assert!(v["hits"][0]["resolution"].as_str().unwrap().contains("stream limit"));
        let (_, v) = call(&app, "GET", "/search?q=consumer&history=true&kind=incident", None).await;
        assert_eq!(v["count"], 1, "{v}");
        assert_eq!(v["hits"][0]["is_head"], false);
        // an OPEN incident deletes as an archive, not a retraction
        let key2 = "inc-2026-07-21-open-one";
        call(&app, "POST", "/incidents", Some(json!({
            "key": key2, "title": "open one", "body": "x", "service": "nats", "severity": "low",
        }))).await;
        let (_, v) = call(&app, "DELETE", &format!("/knowledge/{key2}"), None).await;
        assert_eq!(v["archived"], true, "{v}");
        let (_, v) = call(&app, "GET", &format!("/knowledge/{key2}"), None).await;
        assert_eq!(v["archived"], true, "readable after archive: {v}");
    }

    // tasks: the third kind — lightweight notes/ideas with a resolution loop
    #[tokio::test]
    async fn task_full_flow() {
        let (app, _data, _idx) = app_with_tmp();

        // the task- namespace is fenced off from plain knowledge
        let (st, v) = call(&app, "POST", "/knowledge",
            Some(json!({"key": "task-fake", "title": "t", "body": "x"}))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
        assert!(v["error"].as_str().unwrap().contains("reserved"));
        // and task keys must carry the prefix
        let (st, v) = call(&app, "POST", "/tasks",
            Some(json!({"key": "fix-logs", "title": "t", "body": "x"}))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");

        // file a task; an idea is just a task tagged accordingly
        let key = "task-raise-log-retention";
        let (st, v) = call(&app, "POST", "/tasks", Some(json!({
            "key": key, "title": "Raise container log retention to 72h",
            "body": "Short retention loses evidence.\n\n- [ ] measure log volume first\n",
            "tags": ["idea", "observability"],
            "knowledge": ["web-app-architecture"],
        }))).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert_eq!(v["action"], "created");
        assert_eq!(v["unknown_knowledge"], json!(["web-app-architecture"]));

        let (_, v) = call(&app, "GET", "/tasks", None).await;
        assert_eq!(v["count"], 1);
        assert_eq!(v["tasks"][0]["status"], "open");
        assert_eq!(v["tasks"][0]["archived"], false);
        assert_eq!(v["tasks"][0]["open_followups"], 1);
        let (_, v) = call(&app, "GET", "/healthz", None).await;
        assert_eq!(v["open_tasks"], 1);
        let (_, v) = call(&app, "GET", "/search?q=&kind=task", None).await;
        assert_eq!(v["count"], 1);

        // closing without saying what came of it is refused
        let (st, v) = call(&app, "POST", &format!("/tasks/{key}/resolve"), Some(json!({}))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
        assert!(v["error"].as_str().unwrap().contains("resolution"));

        // done: closed, archived, still listed and searchable
        let (st, v) = call(&app, "POST", &format!("/tasks/{key}/resolve"),
            Some(json!({"resolution": "Retention raised to 72h with a 2G disk budget."}))).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert_eq!(v["archived"], true, "{v}");
        let (_, v) = call(&app, "GET", "/tasks", None).await;
        assert_eq!(v["count"], 1);
        assert_eq!(v["tasks"][0]["status"], "done");
        assert_eq!(v["tasks"][0]["archived"], true);
        let (_, v) = call(&app, "GET", "/healthz", None).await;
        assert_eq!(v["open_tasks"], 0);
        let (_, v) = call(&app, "GET", &format!("/knowledge/{key}"), None).await;
        assert_eq!(v["kind"], "task");
        assert_eq!(v["archived"], true);
        assert!(v["resolved_at"].as_str().unwrap().contains('T'), "stamped: {v}");
        let (_, v) = call(&app, "GET", "/search?q=retention%20disk&kind=task", None).await;
        assert_eq!(v["count"], 1, "{v}");
        assert_eq!(v["hits"][0]["is_head"], true);

        // dropped needs a reason too, and ranks below done in the listing
        call(&app, "POST", "/tasks", Some(json!({
            "key": "task-try-foo", "title": "Try foo", "body": "x",
        }))).await;
        let (_, v) = call(&app, "POST", "/tasks/task-try-foo/resolve",
            Some(json!({"status": "dropped", "resolution": "Obsolete after the bar rewrite."}))).await;
        assert_eq!(v["archived"], true, "{v}");
        let (_, v) = call(&app, "GET", "/tasks", None).await;
        assert_eq!(v["count"], 2);
        assert_eq!(v["tasks"][0]["status"], "done");
        assert_eq!(v["tasks"][1]["status"], "dropped");

        // the incident endpoint refuses tasks
        let (st, v) = call(&app, "POST", &format!("/incidents/{key}/resolve"),
            Some(json!({"resolution": "x"}))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
        assert!(v["error"].as_str().unwrap().contains("not an incident"));
    }

    #[tokio::test]
    async fn tags_endpoint_and_tag_search() {
        let (app, _data, _idx) = app_with_tmp();
        let post = |k: &str, tags: Value| {
            json!({"key": k, "title": "t", "body": "apples and bicycles", "tags": tags})
        };
        call(&app, "POST", "/knowledge", Some(post("a", json!(["Infra", "nats"])))).await;
        call(&app, "POST", "/knowledge", Some(post("b", json!(["infra"])))).await;
        call(&app, "POST", "/knowledge", Some(post("c", json!(["kubernetes"])))).await;

        // the base can report which topics it covers, most used first
        let (st, v) = call(&app, "GET", "/tags", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["count"], 3);
        assert_eq!(v["tags"][0]["tag"], "infra");
        assert_eq!(v["tags"][0]["count"], 2, "case-folded and counted: {v}");

        // a tag is findable by free text even though no body mentions it
        let (_, v) = call(&app, "GET", "/search?q=kubernetes", None).await;
        assert_eq!(v["count"], 1);
        assert_eq!(v["hits"][0]["key"], "c");

        // deleting an entry drops its tag from the listing
        call(&app, "DELETE", "/knowledge/c", None).await;
        let (_, v) = call(&app, "GET", "/tags", None).await;
        assert_eq!(v["count"], 2);
    }

    #[tokio::test]
    async fn search_sort_recent() {
        let (app, _data, _idx) = app_with_tmp();
        call(&app, "POST", "/knowledge", Some(upsert_body("older", "shared shared shared"))).await;
        call(&app, "POST", "/knowledge", Some(upsert_body("newer", "shared once"))).await;

        let (_, v) = call(&app, "GET", "/search?q=shared", None).await;
        assert_eq!(v["hits"][0]["key"], "older", "relevance favours the denser match");
        let (_, v) = call(&app, "GET", "/search?q=shared&sort=recent", None).await;
        assert_eq!(v["hits"][0]["key"], "newer", "recent sort puts the latest first");
    }

    // reindex is idempotent: N runs — same numbers
    #[tokio::test]
    async fn reindex_idempotent() {
        let (app, _data, _idx) = app_with_tmp();
        call(&app, "POST", "/knowledge", Some(upsert_body("a", "one"))).await;
        call(&app, "POST", "/knowledge", Some(upsert_body("a", "two"))).await;
        call(&app, "POST", "/knowledge", Some(upsert_body("b", "three"))).await;
        for _ in 0..3 {
            let (_, v) = call(&app, "POST", "/reindex", None).await;
            assert_eq!(v["head_docs"], 2);
            assert_eq!(v["history_docs"], 3);
        }
        let (_, v) = call(&app, "GET", "/search?q=two", None).await;
        assert_eq!(v["count"], 1, "search must be alive after reindex");
    }

    // 10 parallel writes to distinct keys: the single writer mutex must
    // serialize them with no losses and no broken commits
    #[tokio::test]
    async fn api_concurrent_upserts() {
        let (app, _data, _idx) = app_with_tmp();
        let mut set = tokio::task::JoinSet::new();
        for i in 0..10 {
            let app = app.clone();
            set.spawn(async move {
                let body = json!({
                    "key": format!("key-{i}"),
                    "title": format!("Entry {i}"),
                    "body": format!("body {i}"),
                });
                call(&app, "POST", "/knowledge", Some(body)).await
            });
        }
        while let Some(res) = set.join_next().await {
            let (st, v) = res.unwrap();
            assert_eq!(st, StatusCode::OK, "{v}");
            assert_eq!(v["changed"], true);
        }
        let (_, v) = call(&app, "GET", "/healthz", None).await;
        assert_eq!(v["entries"], 10);
        let (_, v) = call(&app, "GET", "/search?q=&limit=50", None).await;
        assert_eq!(v["count"], 10);
        // history intact: exactly one version per key
        let (_, v) = call(&app, "POST", "/reindex", None).await;
        assert_eq!(v["head_docs"], 10);
        assert_eq!(v["history_docs"], 10);
    }
}
