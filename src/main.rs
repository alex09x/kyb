mod audit;
mod config;
mod embed;
mod index;
mod model;
mod store;

use anyhow::Result;
use axum::extract::rejection::QueryRejection;
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::http::StatusCode;
use axum::middleware;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;
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

fn vector_docs(st: &AppState) -> Option<Vec<(String, String)>> {
    st.semantic.as_ref()?;
    // live tree + archived incidents/tasks: everything the default search
    // covers must be reachable semantically too
    let mut entries = match st.store.list_head() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("kyb: cannot list canon for embeddings: {e:#}");
            return None;
        }
    };
    match st.store.archived_latest() {
        Ok(mut archived) => entries.append(&mut archived),
        Err(e) => eprintln!("kyb: archived entries not embedded: {e:#}"),
    }
    Some(entries.iter().map(|e| (e.key.clone(), embed_text(e))).collect())
}

async fn finish_vector_rebuild(st: &AppState, job: embed::RebuildJob) {
    let Some(sem) = st.semantic.as_ref() else { return };
    match sem.finish_rebuild(job).await {
        Ok(n) => eprintln!("kyb: embedded {n} entries"),
        Err(e) => eprintln!("kyb: embedding failed, staying lexical: {e:#}"),
    }
}

async fn rebuild_vectors(st: &AppState) {
    let Some(sem) = st.semantic.as_ref() else { return };
    let writer = st.writer.lock().await;
    let Some(docs) = vector_docs(st) else { return };
    let job = sem.begin_rebuild(docs).await;
    drop(writer);
    finish_vector_rebuild(st, job).await;
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
    let model_start = std::time::Instant::now();
    let semantic = match embed::Semantic::load(&cfg.model_dir, cfg.index_dir.join("vectors.cache")) {
        Ok(s) => {
            eprintln!(
                "kyb: semantic search on ({}) — model loaded in {:.1}s",
                cfg.model_dir.display(),
                model_start.elapsed().as_secs_f64()
            );
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
        .route("/knowledge/{key}/diff", get(diff))
        .route("/incidents", post(upsert_incident).get(list_incidents))
        .route("/incidents/{key}/resolve", post(resolve_incident))
        .route("/tasks", post(upsert_task).get(list_tasks))
        .route("/tasks/{key}/resolve", post(resolve_task))
        .route("/tasks/{key}/transition", post(transition_task))
        .route("/search", get(search))
        .route("/tags", get(tags))
        .route("/reindex", post(reindex))
        .layer(middleware::from_fn_with_state(state.clone(), audit::audit_mw))
        .with_state(state)
}

#[tokio::main]
async fn main() -> Result<()> {
    // Timed and logged on EVERY start, not measured once.
    //
    // The whole point of the finding is that this grows with the base's age:
    // the index and the vectors are rebuilt from git each time, so a restart is
    // a window in which the fleet has no shared memory at all. A single
    // measurement gives today's number and says nothing about the trend, and
    // until now nothing recorded the duration - so the symptom was
    // indistinguishable from "the service is still coming up", which it is.
    //
    // The split matters as much as the total: when this gets slow again, the log
    // already says whether it was the embeddings (cached, so probably not) or
    // the per-document indexing that is linear in history.
    let boot = std::time::Instant::now();
    let cfg = config::Config::from_env();
    let state = build_state(&cfg)?;
    let indexed = boot.elapsed();
    rebuild_vectors(&state).await;
    let ready = boot.elapsed();
    let app = build_app(state);
    let listener = tokio::net::TcpListener::bind(&cfg.addr).await?;
    eprintln!(
        "kyb: ready in {:.1}s (canon, index and model load {:.1}s, embeddings {:.1}s)",
        ready.as_secs_f64(),
        indexed.as_secs_f64(),
        (ready - indexed).as_secs_f64()
    );
    eprintln!("kyb: listening on http://{}", cfg.addr);
    // ConnectInfo so the audit log sees the real client IP
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}

fn err500(e: anyhow::Error) -> Reply {
    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("{e:#}")})))
}

/// `Query`, but rejecting in the same JSON shape as every other error here.
///
/// axum's own rejection is plain text, and a client that parses every other
/// failure as `{"error": ...}` would trip over exactly the response it most
/// needs to read - the one telling it the server does not know the parameter it
/// just sent. Serde's message is kept verbatim: it names the offending
/// parameter and lists the ones that exist, which is the whole answer.
pub struct Q<T>(pub T);

impl<S, T> FromRequestParts<S> for Q<T>
where
    Query<T>: FromRequestParts<S, Rejection = QueryRejection>,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        match Query::<T>::from_request_parts(parts, state).await {
            Ok(Query(value)) => Ok(Q(value)),
            Err(rejection) => Err((
                StatusCode::BAD_REQUEST,
                Json(json!({"error": rejection.body_text()})),
            )
                .into_response()),
        }
    }
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

/// What a route hands to [`locked_write`]: the entry to commit plus the extra
/// response fields it wants, or the reply that refuses the write outright.
type Built = Result<(model::Entry, Value), Reply>;

/// The whole read–modify–write of one entry inside a single critical section.
///
/// `build` runs with the global writer lock ALREADY held, so every stored
/// version it reads is still the current one when the commit lands: two
/// partial updates to the same key can no longer both read the same version
/// and silently overwrite each other. A terminal entry is archived in the same
/// section, so no concurrent reopen can slip between the closing commit and
/// the archival and lose its freshly recreated file to it.
///
/// The semantic update gets a generation ticket before the lock is dropped;
/// model computation happens outside the lock and can install its vector only
/// while that ticket still represents the newest committed version.
async fn locked_write(st: &Arc<AppState>, build: impl FnOnce(&AppState) -> Built) -> Reply {
    let mut w = st.writer.lock().await;
    let (entry, extra) = match build(st.as_ref()) {
        Ok(built) => built,
        Err(reply) => return reply,
    };
    let closed = entry.is_closed();
    let key = entry.key.clone();
    let (mut reply, committed) = commit_locked(st, &mut w, entry, extra);
    if closed {
        archive_locked(st, &mut w, &key, &mut reply);
    }
    let pending = match (st.semantic.as_ref(), committed) {
        (Some(sem), Some(entry)) => {
            let ticket = sem.begin_upsert(&entry.key).await;
            Some((ticket, entry))
        }
        _ => None,
    };
    drop(w);
    if let (Some(sem), Some((ticket, entry))) = (st.semantic.as_ref(), pending) {
        if let Err(err) = sem.finish_upsert(ticket, &embed_text(&entry)).await {
            eprintln!("kyb: embedding not updated for {}: {err:#}", entry.key);
        }
    }
    reply
}

/// Shared tail of every write route: validate, commit to git, update the
/// index — all under the caller's writer lock, which the `&mut IndexWriter`
/// proves is held. `extra` lets a route add response fields. Returns the entry
/// that actually landed (None when nothing was committed) so the caller can
/// refresh the vector side once the lock is gone.
fn commit_locked(
    st: &AppState,
    w: &mut IndexWriter,
    entry: model::Entry,
    extra: Value,
) -> (Reply, Option<model::Entry>) {
    if let Err(e) = entry.validate() {
        return ((StatusCode::BAD_REQUEST, Json(json!({"error": e.to_string()}))), None);
    }
    if let Err(e) = validate_task_parent_chain(st, &entry) {
        return ((StatusCode::BAD_REQUEST, Json(json!({"error": e.to_string()}))), None);
    }
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    // Asked before the write, while "absent from the tree but present in
    // history" is still distinguishable from "just created".
    let had_history = matches!(st.store.get(&entry.key), Ok(None))
        && st.store.history(&entry.key).map(|v| !v.is_empty()).unwrap_or(false);
    let (c, action) = match st.store.upsert(entry, &today) {
        Err(e) => return (err500(e), None),
        Ok(store::UpsertOutcome::Unchanged(entry)) => {
            return ((StatusCode::OK, Json(json!({"key": entry.key, "changed": false}))), None);
        }
        Ok(store::UpsertOutcome::Created(c)) => (c, "created"),
        Ok(store::UpsertOutcome::Updated(c)) => (c, "updated"),
    };
    // git is already the truth; a broken index heals via /reindex
    if let Err(e) = st
        .index
        .upsert_head(w, &c.entry, &c.sha, c.committed_at)
        .and_then(|_| st.index.commit_and_reload(w))
    {
        return (
            err500(e.context("git committed but the index was not updated — run POST /reindex")),
            None,
        );
    }
    let mut resp = json!({"key": c.entry.key, "sha": c.sha, "changed": true, "action": action});
    // Only on a create, and only as information. Two near-identical keys split
    // one topic in half while both entries answer and nothing fails - the
    // author is the only one who can tell a family apart from a slip.
    if action == "created" {
        // Every key that ever existed, not only the live ones. A retracted entry
        // leaves the working tree, so comparing against the tree alone would be
        // silent about the worst case: re-creating a key somebody deliberately
        // took away. That is not an accidental twin - it is a decision being
        // undone by someone who cannot see that it was made.
        let mut known = st.store.all_keys_ever().unwrap_or_default();
        known.extend(st.store.list_head().unwrap_or_default().into_iter().map(|e| e.key));
        let similar = model::similar_keys(&c.entry.key, known.iter().map(String::as_str));
        if !similar.is_empty() {
            resp["similar"] = json!(similar);
        }
        // An exact match is a different message: not "looks like a neighbour"
        // but "this existed and was withdrawn - read why before continuing".
        if had_history {
            resp["revived"] = json!(true);
            resp["hint"] = json!(format!(
                "'{}' existed before and was retracted; see `kyb history {}` for why",
                c.entry.key, c.entry.key
            ));
        }
    }
    if let (Some(obj), Some(add)) = (resp.as_object_mut(), extra.as_object()) {
        obj.extend(add.clone());
    }
    ((StatusCode::OK, Json(resp)), Some(c.entry))
}

async fn upsert(State(st): St, Json(r): Json<UpsertReq>) -> Reply {
    locked_write(&st, move |_| {
        Ok((
            model::Entry {
                key: r.key,
                title: r.title,
                tags: r.tags,
                refs: r.refs,
                body: r.body,
                ..Default::default()
            },
            Value::Null,
        ))
    })
    .await
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
    locked_write(&st, move |st| {
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
        stamp_timeline(st, &mut entry);
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
        Ok((entry, extra))
    })
    .await
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
///
/// Runs inside the closing write's critical section — the `&mut IndexWriter`
/// is the proof the writer lock is held — because a terminal commit and its
/// archival must not be separable: a reopen landing in between would have its
/// recreated file deleted by this archive.
fn archive_locked(st: &AppState, _w: &mut IndexWriter, key: &str, reply: &mut Reply) {
    if reply.0 != StatusCode::OK {
        return;
    }
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
///
/// The stored version is read under the global writer lock and stays under it
/// through the commit and the archival, so a close can neither overwrite a
/// concurrent update it never saw nor archive an entry someone just reopened.
async fn close_entry(st: &Arc<AppState>, key: String, want_task: bool, r: ResolveReq) -> Reply {
    if let Some(resp) = bad_key(&key) {
        return resp;
    }
    locked_write(st, move |st| {
        // archived entries can still be closed again (amended resolution) or
        // parked back to a non-closing status, which reopens the file
        let Some(mut e) = stored_version(st, &key) else {
            return Err((
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("no entry '{key}'")})),
            ));
        };
        if want_task != e.is_task() || (!want_task && !e.is_incident()) {
            let what = if want_task { "a task" } else { "an incident report" };
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("'{key}' is not {what}")})),
            ));
        }
        let default_close = if want_task { "done" } else { "resolved" };
        e.status = r.status.unwrap_or_else(|| default_close.to_string());
        // the stored reason describes the block the task is leaving; carrying it
        // into another status would commit a state that contradicts itself
        if e.status != model::STATUS_BLOCKED {
            e.blocked_reason.clear();
        }
        // an empty resolution keeps whatever was recorded before, so closing
        // twice never wipes the outcome
        if !r.resolution.trim().is_empty() {
            e.resolution = r.resolution;
        }
        stamp_timeline(st, &mut e);
        // loose ends do not block a close, but they must not go silent either
        let open = e.open_followups();
        let extra = if open > 0 && e.is_closed() {
            json!({
                "open_followups": open,
                "warning": format!("{open} unfinished follow-up(s) (`- [ ]`) remain in the body — reassign or finish them"),
            })
        } else {
            Value::Null
        };
        Ok((e, extra))
    })
    .await
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

/// A parent link may point forward — a child can be filed before the task it
/// hangs under — so a missing parent is reported back, never refused. An
/// archived (closed) parent counts as known: it is still part of the record.
fn unknown_parent(st: &AppState, entry: &model::Entry) -> Option<String> {
    let parent = entry.parent_task.trim();
    if parent.is_empty() || !model::is_valid_key(parent) {
        return None;
    }
    stored_version(st, parent).is_none().then(|| parent.to_string())
}

/// Keep task relationships a tree/forest, not merely a set of individually
/// valid edges. This runs while the global write lock is held, so two
/// concurrent updates cannot each observe the other edge as absent and commit
/// a cycle together. A missing parent remains a valid forward reference.
fn validate_task_parent_chain(st: &AppState, entry: &model::Entry) -> Result<()> {
    if !entry.is_task() || entry.parent_task.is_empty() {
        return Ok(());
    }
    let mut cursor = entry.parent_task.clone();
    let mut seen = HashSet::new();
    while !cursor.is_empty() {
        if cursor == entry.key {
            anyhow::bail!(
                "parent_task would create a cycle containing '{}'",
                entry.key
            );
        }
        if !seen.insert(cursor.clone()) {
            anyhow::bail!("parent_task points into an existing task cycle at '{cursor}'");
        }
        let Some(parent) = stored_version(st, &cursor) else {
            break;
        };
        cursor = parent.parent_task;
    }
    Ok(())
}

#[derive(Deserialize)]
struct TaskReq {
    key: String,
    title: String,
    body: String,
    #[serde(default = "default_status")]
    status: String,
    /// Optional ranking: "" | low | medium | high | critical.
    #[serde(default)]
    priority: String,
    /// What the task waits on; only valid together with status=blocked.
    #[serde(default)]
    blocked_reason: String,
    /// Who holds the task: a short, stable, non-secret label. Empty = unclaimed.
    #[serde(default)]
    assignee: String,
    /// The `task-` key this task hangs under; empty = top-level.
    #[serde(default)]
    parent_task: String,
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
    locked_write(&st, move |st| {
        let mut entry = model::Entry {
            key: r.key,
            title: r.title,
            kind: model::KIND_TASK.into(),
            status: r.status,
            priority: r.priority,
            blocked_reason: r.blocked_reason,
            assignee: r.assignee.trim().to_string(),
            parent_task: r.parent_task.trim().to_string(),
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
        let mut extra = serde_json::Map::new();
        if !unknown.is_empty() {
            extra.insert("unknown_knowledge".into(), json!(unknown));
        }
        if let Some(parent) = unknown_parent(st, &entry) {
            extra.insert("unknown_parent".into(), json!(parent));
        }
        let extra = if extra.is_empty() { Value::Null } else { Value::Object(extra) };
        Ok((entry, extra))
    })
    .await
}

/// A partial task update: move the task between the LIVE statuses and, when the
/// request says so, change who holds it or what it hangs under. Everything the
/// request does not mention — title, body, tags, priority, knowledge, refs —
/// stays exactly as stored, so claiming a task never means resending (or
/// paraphrasing, or losing) the task being claimed.
#[derive(Deserialize)]
struct TransitionReq {
    status: String,
    /// Present = set it ("" hands the task back to nobody); absent = keep stored.
    assignee: Option<String>,
    /// Present = set it ("" detaches the task); absent = keep stored.
    parent_task: Option<String>,
    /// Only meaningful with status=blocked; leaving blocked always clears it.
    blocked_reason: Option<String>,
}

/// Closing stays a separate act on purpose: `done`/`dropped` demand an outcome,
/// which is what POST /tasks/{key}/resolve (`kyb done`) is for.
async fn transition_task(
    State(st): St,
    Path(key): Path<String>,
    Json(r): Json<TransitionReq>,
) -> Reply {
    if let Some(resp) = bad_key(&key) {
        return resp;
    }
    let status = r.status.trim().to_string();
    if model::TASK_TERMINAL_STATUSES.contains(&status.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!(
                "'{status}' closes a task and needs an outcome — use `kyb done {key}` (POST /tasks/{key}/resolve) with a resolution; a transition only moves between {}",
                model::TASK_LIVE_STATUSES.join("|")
            )})),
        );
    }
    if !model::TASK_LIVE_STATUSES.contains(&status.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!(
                "status must be one of: {}", model::TASK_LIVE_STATUSES.join("|")
            )})),
        );
    }
    // The stored version is read with the writer lock already held and the
    // commit lands before it is released: two transitions on one key are a
    // strict sequence, so the second sees the first instead of overwriting it.
    locked_write(&st, move |st| {
        // an archived task can be picked back up: the stored version comes from
        // history and the write recreates the file
        let Some(mut e) = stored_version(st, &key) else {
            return Err((
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("no entry '{key}'")})),
            ));
        };
        if !e.is_task() {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("'{key}' is not a task")})),
            ));
        }
        e.status = status;
        if let Some(assignee) = r.assignee {
            e.assignee = assignee.trim().to_string();
        }
        if let Some(parent) = r.parent_task {
            e.parent_task = parent.trim().to_string();
        }
        // a reason survives only inside the block it describes
        if e.status == model::STATUS_BLOCKED {
            if let Some(reason) = r.blocked_reason {
                e.blocked_reason = reason;
            }
        } else {
            e.blocked_reason.clear();
        }
        let mut extra = serde_json::Map::new();
        extra.insert("status".into(), json!(e.status));
        extra.insert("assignee".into(), json!(e.assignee));
        if let Some(parent) = unknown_parent(st, &e) {
            extra.insert("unknown_parent".into(), json!(parent));
        }
        Ok((e, Value::Object(extra)))
    })
    .await
}

/// `deny_unknown_fields` on every query struct, deliberately.
///
/// Serde ignores an unknown field by default, and for this service that is the
/// worst available behaviour: a client that asks `?as_of=...` of a server too
/// old to know the parameter gets today's data formatted as a normal 200, and a
/// typo like `?knid=incident` returns the unfiltered set presented as a filtered
/// answer. Nothing fails; the answer is merely wrong, plausibly. For a store
/// whose entire claim is temporal correctness, silence is the one response that
/// must not be available.
///
/// The cost is that an unrecognised parameter is now a 400 rather than a
/// shrug - which is the point: it distinguishes "this server is older than your
/// client" from "nothing changed on that date".
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IncidentsQ {
    status: Option<String>,
    service: Option<String>,
    /// Exact task priority; incidents carry none, so it never matches there.
    priority: Option<String>,
    /// Exact task ownership filters — same story: task-only fields.
    assignee: Option<String>,
    parent_task: Option<String>,
    /// followups=open keeps only reports with unfinished `- [ ]` items —
    /// the loose ends a session can pick up (archived included).
    followups: Option<String>,
    /// all=true includes archived (closed) entries; the default shows only
    /// what is live in the canon.
    all: Option<String>,
    limit: Option<usize>,
}

/// Open followups of a body: `- [ ]` checklist items (same rule as
/// Entry::open_followups, but hits carry plain text).
fn open_followups_of(body: &str) -> usize {
    body.lines().filter(|l| l.trim_start().starts_with("- [ ]")).count()
}

/// Listings come from the index, not the tree, so archived (closed) entries
/// stay part of the record — but the DEFAULT view is what is live in the
/// canon. Archived rows appear with `?all=true`, with an explicit `?status=`
/// filter, or in the loose-ends view (`?followups=open`). Open first,
/// freshest on top within a group.
fn list_kind(
    st: &AppState,
    kind: &str,
    status_order: &[&str],
    q: &IncidentsQ,
) -> Result<Vec<(index::Hit, bool)>, Reply> {
    // an empty ?status= from the CLI is "no filter", not "match nothing"
    let status = q.status.as_deref().filter(|s| !s.trim().is_empty());
    let service = q.service.as_deref().filter(|s| !s.trim().is_empty());
    let priority = q.priority.as_deref().filter(|s| !s.trim().is_empty());
    let assignee = q.assignee.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let parent = q.parent_task.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let only_open_followups = q.followups.as_deref() == Some("open");
    let all = matches!(q.all.as_deref(), Some("true" | "1" | "yes"));
    let opts = index::SearchOpts {
        limit: 500,
        kind: Some(kind.to_string()),
        status: status.map(String::from),
        service: service.map(String::from),
        priority: priority.map(String::from),
        assignee: assignee.map(String::from),
        parent_task: parent.map(String::from),
        ..Default::default()
    };
    let mut hits = match st.index.search("", &opts) {
        Err(e) => return Err(err500(e)),
        Ok(h) => h,
    };
    if only_open_followups {
        hits.retain(|h| open_followups_of(&h.body) > 0);
    }
    let mut rows: Vec<(index::Hit, bool)> = hits
        .into_iter()
        .map(|h| {
            let archived = is_archived(st, &h.key);
            (h, archived)
        })
        .collect();
    if !all && status.is_none() && !only_open_followups {
        rows.retain(|(_, archived)| !archived);
    }
    let rank = |s: &str| status_order.iter().position(|x| *x == s).unwrap_or(9);
    rows.sort_by(|(a, _), (b, _)| {
        rank(&a.status)
            .cmp(&rank(&b.status))
            // committed_at is exact (ISO); updated_at only has day granularity
            .then(b.committed_at.cmp(&a.committed_at))
            .then(b.updated_at.cmp(&a.updated_at))
            .then(a.key.cmp(&b.key))
    });
    rows.truncate(q.limit.unwrap_or(50).min(200));
    Ok(rows)
}

/// `archived` on a listing row: the file is gone from the tree, the entry
/// lives on in the index and in git history.
fn is_archived(st: &AppState, key: &str) -> bool {
    !matches!(st.store.get(key), Ok(Some(_)))
}

async fn list_incidents(State(st): St, Q(q): Q<IncidentsQ>) -> Reply {
    let rows = match list_kind(&st, model::KIND_INCIDENT, &model::STATUSES, &q) {
        Err(r) => return r,
        Ok(h) => h,
    };
    let rows: Vec<Value> = rows
        .iter()
        .map(|(h, archived)| {
            json!({
                "key": h.key, "title": h.title, "service": h.service, "hosts": h.hosts,
                "severity": h.severity, "status": h.status, "knowledge": h.knowledge,
                "resolution": h.resolution, "detection": h.detection, "affected": h.affected,
                "started_at": h.started_at, "detected_at": h.detected_at,
                "mitigated_at": h.mitigated_at, "resolved_at": h.resolved_at,
                "open_followups": open_followups_of(&h.body),
                "tags": h.tags, "updated_at": h.updated_at,
                "archived": archived,
            })
        })
        .collect();
    (StatusCode::OK, Json(json!({"count": rows.len(), "incidents": rows})))
}

async fn list_tasks(State(st): St, Q(q): Q<IncidentsQ>) -> Reply {
    let rows = match list_kind(&st, model::KIND_TASK, &model::TASK_STATUSES, &q) {
        Err(r) => return r,
        Ok(h) => h,
    };
    let rows: Vec<Value> = rows
        .iter()
        .map(|(h, archived)| {
            json!({
                "key": h.key, "title": h.title, "status": h.status,
                "priority": h.priority, "blocked_reason": h.blocked_reason,
                "assignee": h.assignee, "parent_task": h.parent_task,
                "knowledge": h.knowledge, "resolution": h.resolution,
                "resolved_at": h.resolved_at,
                "open_followups": open_followups_of(&h.body),
                "tags": h.tags, "updated_at": h.updated_at,
                "archived": archived,
            })
        })
        .collect();
    (StatusCode::OK, Json(json!({"count": rows.len(), "tasks": rows})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GetQ {
    at: Option<String>,
}

/// `as_of` in the three shapes an agent actually has to hand.
///
/// A bare date resolves to the END of that day: "what did we believe on the 1st"
/// means once the 1st had happened. A git revision is accepted because that is
/// what a `history` call just handed back, and making the caller convert a sha
/// into a timestamp would be busywork with a rounding error in it.
fn parse_as_of(raw: &str, st: &AppState) -> Option<i64> {
    parse_as_of_bound(raw, st, true)
}

/// `upper` decides what a bare date means: the end of that day for an upper
/// bound, the start of it for a lower one. So `2026-08-01,2026-08-01` is the
/// whole of the 1st rather than an empty instant.
fn parse_as_of_bound(raw: &str, st: &AppState, upper: bool) -> Option<i64> {
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Some(t.timestamp());
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        let t = if upper { d.and_hms_opt(23, 59, 59) } else { d.and_hms_opt(0, 0, 0) };
        return t.map(|dt| dt.and_utc().timestamp());
    }
    st.store.rev_time(raw).ok().flatten()
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
    if e.is_task() {
        obj.insert("priority".into(), json!(e.priority));
        obj.insert("blocked_reason".into(), json!(e.blocked_reason));
        obj.insert("assignee".into(), json!(e.assignee));
        obj.insert("parent_task".into(), json!(e.parent_task));
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

async fn get_one(State(st): St, Path(key): Path<String>, Q(q): Q<GetQ>) -> Reply {
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiffQ {
    /// Git revisions. Omit both to compare the two most recent versions -
    /// "what just changed" is the question that gets asked most. Omit `to`
    /// alone to compare a past version against the current one.
    from: Option<String>,
    to: Option<String>,
}

/// What changed between two versions of one entry.
///
/// `/history` reports *that* a version exists; this reports *what* moved in it.
/// Without it, answering "where did this change" means fetching two versions
/// with `?at=` and comparing them by hand, which is the sort of work an agent
/// gets wrong quietly.
async fn diff(State(st): St, Path(key): Path<String>, Q(q): Q<DiffQ>) -> Reply {
    if let Some(r) = bad_key(&key) {
        return r;
    }
    let versions = match st.store.history(&key) {
        Err(e) => return err500(e),
        Ok(v) if v.is_empty() => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("no history for '{key}'")})),
            )
        }
        Ok(v) => v,
    };
    // history is newest-first
    let pick = |o: &Option<String>, fallback: Option<&str>| -> Option<String> {
        o.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| fallback.map(str::to_string))
    };
    let to_rev = pick(&q.to, versions.first().map(|v| v.sha.as_str()));
    let from_rev = pick(&q.from, versions.get(1).map(|v| v.sha.as_str()));
    let (Some(from_rev), Some(to_rev)) = (from_rev, to_rev) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!("'{key}' has only one version; pass ?from=<rev>&to=<rev> explicitly")
            })),
        );
    };

    let mut sides = vec![];
    for rev in [&from_rev, &to_rev] {
        match st.store.get_at(&key, rev) {
            Err(e) => return err500(e),
            Ok(None) => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": format!("'{key}' does not exist at revision '{rev}'")})),
                )
            }
            Ok(Some(entry)) => {
                let at = st.store.rev_time(rev).ok().flatten().map(iso_secs).unwrap_or_default();
                sides.push((entry, at));
            }
        }
    }
    let (to_entry, to_at) = sides.pop().expect("two sides pushed");
    let (from_entry, from_at) = sides.pop().expect("two sides pushed");
    let (fields, body) = model::diff_entries(&from_entry, &to_entry);
    (
        StatusCode::OK,
        Json(json!({
            "key": key,
            "from": {"rev": from_rev, "committed_at": from_at},
            "to": {"rev": to_rev, "committed_at": to_at},
            "changed": !fields.is_empty() || !body.added.is_empty() || !body.removed.is_empty(),
            "fields": fields,
            "body": body,
        })),
    )
}

fn iso_secs(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0).map(|t| t.to_rfc3339()).unwrap_or_default()
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
#[serde(deny_unknown_fields)]
struct SearchQ {
    q: Option<String>,
    tag: Option<String>,
    history: Option<bool>,
    /// as_of=<RFC3339 | YYYY-MM-DD | git revision>: answer as the base stood
    /// then. A bare date means the END of that day, because "what did we
    /// believe on the 1st" means after the 1st happened, not before it began.
    as_of: Option<String>,
    /// changed_between=<start>,<end>: which entries moved inside that window.
    /// Each side takes the same three shapes as as_of; a bare start date means
    /// the START of that day and a bare end date the END, so a single day is
    /// written `2026-08-01,2026-08-01` and means all of it.
    changed_between: Option<String>,
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
    /// task filters (exact terms)
    priority: Option<String>,
    assignee: Option<String>,
    parent_task: Option<String>,
}

async fn search(State(st): St, Q(p): Q<SearchQ>) -> Reply {
    let tags: Vec<String> = p
        .tag
        .map(|t| t.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    let q = p.q.as_deref().unwrap_or("");
    let limit = p.limit.unwrap_or(10).min(100);
    // an empty ?sort= from the CLI means "unspecified"
    let sort = p.sort.as_deref().filter(|s| !s.trim().is_empty());
    // relevance is meaningless without a query: an empty q is a listing,
    // and listings read newest first
    let recent = sort == Some("recent") || (q.trim().is_empty() && sort.is_none());
    let history = p.history.unwrap_or(false);
    let as_of = match p.as_of.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some(raw) => match parse_as_of(raw, &st) {
            Some(t) => Some(t),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "as_of: expected an RFC3339 timestamp, a YYYY-MM-DD date, or a git revision that resolves"})),
                )
            }
        },
    };
    let window = match p.changed_between.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some(raw) => {
            let Some((a, b)) = raw.split_once(',') else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "changed_between: expected <start>,<end>"})),
                );
            };
            match (parse_as_of_bound(a.trim(), &st, false), parse_as_of_bound(b.trim(), &st, true)) {
                (Some(lo), Some(hi)) if lo <= hi => Some((lo, hi)),
                (Some(_), Some(_)) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": "changed_between: start is after end"})),
                    )
                }
                _ => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": "changed_between: each side must be an RFC3339 timestamp, a YYYY-MM-DD date, or a git revision that resolves"})),
                    )
                }
            }
        }
    };
    if as_of.is_some() && window.is_some() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "as_of and changed_between ask different questions; pass one"})),
        );
    }
    // Semantic retrieval covers current knowledge only - the vector index holds
    // one vector per key, for the head version. A history, as_of or
    // changed_between question is about a specific past version, which no head
    // vector can represent, so those queries stay lexical rather than silently
    // matching today's text.
    //
    // PROVISIONAL, and tied to exactly one fact: vectors are head-only. Extend
    // the vector index to (key, sha) and this restriction has no remaining
    // justification - it must be revisited in the same change, not left to
    // outlive the reason it was written for. It is also the reason a versioned
    // search is lexical while a current-state search is hybrid, which is a
    // difference no benchmark comparing the two can be allowed to inherit.
    let want_semantic = !recent
        && !history
        && as_of.is_none()
        && window.is_none()
        && !q.trim().is_empty()
        && st.semantic.is_some()
        && p.semantic != Some(false);
    // an empty ?kind= from the CLI is "no filter", not "match nothing"
    let norm = |o: &Option<String>| o.clone().filter(|s| !s.trim().is_empty());
    let opts = index::SearchOpts {
        tags: tags.clone(),
        history,
        as_of,
        changed_between: window,
        limit: if want_semantic { limit.max(24) } else { limit },
        recent,
        kind: norm(&p.kind),
        status: norm(&p.status),
        service: norm(&p.service),
        priority: norm(&p.priority),
        assignee: norm(&p.assignee),
        parent_task: norm(&p.parent_task),
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
                        && opts.priority.as_deref().is_none_or(|p| h.priority == p)
                        && opts.assignee.as_deref().is_none_or(|a| h.assignee == a)
                        && opts.parent_task.as_deref().is_none_or(|p| h.parent_task == p)
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
    let mut w = st.writer.lock().await;
    let entry = match st.store.get(&key) {
        Err(e) => return err500(e),
        Ok(None) => {
            return (StatusCode::NOT_FOUND, Json(json!({"error": format!("no entry '{key}'")})))
        }
        Ok(Some(e)) => e,
    };
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
                if let Some(sem) = st.semantic.as_ref() {
                    sem.invalidate(&key).await;
                }
                drop(w);
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
    let vector_job = match (&res, st.semantic.as_ref()) {
        (Ok(_), Some(sem)) => match vector_docs(&st) {
            Some(docs) => Some(sem.begin_rebuild(docs).await),
            None => None,
        },
        _ => None,
    };
    drop(w);
    match res {
        Err(e) => err500(e),
        Ok((heads, hist)) => {
            if let Some(job) = vector_job {
                finish_vector_rebuild(&st, job).await;
            }
            (StatusCode::OK, Json(json!({"ok": true, "head_docs": heads, "history_docs": hist})))
        }
    }
}

/// What this build can answer, for a client to check BEFORE it asks.
///
/// `deny_unknown_fields` protects a server that has it. The configuration that
/// will actually be common after a release is the opposite one - a CLI updated
/// through brew talking to a server nobody has redeployed yet - and that server
/// cannot object to a parameter it has never heard of. It will answer a question
/// about the past with today's data and a 200.
///
/// So the check has to happen on the client, before the request, against a list
/// the server publishes. Names match the query parameters exactly; entries are
/// only ever added, never renamed, because an older CLI matches on the string.
const CAPABILITIES: [&str; 3] = ["as_of", "changed_between", "diff"];

async fn healthz(State(st): St) -> Reply {
    let heads = st.store.list_head().unwrap_or_default();
    let open_incidents =
        heads.iter().filter(|e| e.is_incident() && e.status == "open").count();
    // every live status counts: a blocked task is still work waiting on someone
    let open_tasks = heads.iter().filter(|e| e.is_live_task()).count();
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
            "version": env!("CARGO_PKG_VERSION"),
            "capabilities": CAPABILITIES,
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

    pub(super) async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
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
    pub(super) fn upsert_body(key: &str, body: &str) -> Value {
        json!({"key": key, "title": "NATS стримы", "body": body, "tags": ["nats"]})
    }

    /// Same app, plus the state behind it — the interleaving tests need the
    /// writer mutex itself to hold requests at a chosen point.
    fn app_with_state() -> (Router, Arc<AppState>, tempfile::TempDir, tempfile::TempDir) {
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
        let state = build_state(&cfg).unwrap();
        (build_app(state.clone()), state, data, idx)
    }

    pub(super) fn app_with_tmp() -> (Router, tempfile::TempDir, tempfile::TempDir) {
        let (app, _state, data, idx) = app_with_state();
        (app, data, idx)
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
    async fn healthz_advertises_version_and_capabilities() {
        let (app, _data, _idx) = app_with_tmp();
        let (st, v) = call(&app, "GET", "/healthz", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
        let caps: Vec<&str> =
            v["capabilities"].as_array().unwrap().iter().map(|c| c.as_str().unwrap()).collect();
        for want in ["as_of", "changed_between", "diff"] {
            assert!(caps.contains(&want), "healthz must advertise {want}, got {caps:?}");
        }
    }

    /// The failure this prevents is not a crash - it is a plausible answer.
    /// `?knid=incident` used to return the unfiltered set as though it had been
    /// filtered, and `?as_of=...` against a server too old to know the parameter
    /// used to return today's data as a normal 200.
    #[rstest]
    #[case("/search?q=x&knid=incident", "knid")]
    #[case("/search?q=x&zzz_nonsense=1", "zzz_nonsense")]
    #[case("/search?q=x&as_off=2026-08-01", "as_off")]
    #[case("/knowledge/demo?att=abc", "att")]
    #[case("/knowledge/demo/diff?frm=a", "frm")]
    #[case("/incidents?statuss=open", "statuss")]
    #[case("/tasks?priorty=high", "priorty")]
    #[tokio::test]
    async fn unknown_query_parameters_are_refused(#[case] uri: &str, #[case] offender: &str) {
        let (app, _data, _idx) = app_with_tmp();
        call(&app, "POST", "/knowledge", Some(upsert_body("demo", "body"))).await;
        let (st, v) = call(&app, "GET", uri, None).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{uri} must be refused, got {v}");
        let msg = v["error"].as_str().unwrap_or_default();
        assert!(msg.contains(offender), "the error must name the parameter: {msg}");
    }

    /// A silent ignore and a real answer have to be distinguishable, so the
    /// parameters that DO exist must keep working unchanged.
    #[rstest]
    #[case("/search?q=x&kind=knowledge")]
    #[case("/search?q=x&as_of=2999-01-01")]
    #[case("/search?q=x&changed_between=2000-01-01,2999-01-01")]
    #[case("/knowledge/demo")]
    #[case("/incidents?status=open")]
    #[case("/tasks?priority=high")]
    #[tokio::test]
    async fn known_query_parameters_still_pass(#[case] uri: &str) {
        let (app, _data, _idx) = app_with_tmp();
        call(&app, "POST", "/knowledge", Some(upsert_body("demo", "body"))).await;
        let (st, v) = call(&app, "GET", uri, None).await;
        assert_eq!(st, StatusCode::OK, "{uri} must still work, got {v}");
    }

    /// Re-creating a key somebody deliberately retracted is worse than an
    /// accidental twin: a decision is being undone by someone who cannot see it
    /// was made. The tree has forgotten the key; history has not.
    #[tokio::test]
    async fn a_retracted_key_is_flagged_when_it_comes_back() {
        let (app, _data, _idx) = app_with_tmp();
        call(&app, "POST", "/knowledge", Some(upsert_body("withdrawn", "the first take"))).await;
        call(&app, "DELETE", "/knowledge/withdrawn", None).await;
        assert_eq!(call(&app, "GET", "/knowledge/withdrawn", None).await.0, StatusCode::NOT_FOUND);

        let (st, v) =
            call(&app, "POST", "/knowledge", Some(upsert_body("withdrawn", "a second take"))).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["action"], "created", "the tree has forgotten it");
        assert_eq!(v["revived"], true, "history has not, and says so");
        assert!(v["hint"].as_str().unwrap().contains("retracted"), "{v}");

        // a plain create carries neither flag
        let (_, v) = call(&app, "POST", "/knowledge", Some(upsert_body("brand-new", "x"))).await;
        assert!(v["revived"].is_null());
        assert!(v["hint"].is_null());
    }

    /// The twin check looks past the working tree too: a near-twin of a
    /// retracted key is exactly the case a tree-only comparison would miss.
    #[tokio::test]
    async fn a_twin_of_a_retracted_key_is_still_surfaced() {
        let (app, _data, _idx) = app_with_tmp();
        call(&app, "POST", "/knowledge", Some(upsert_body("rule-name-your-comparison-baseline", "x")))
            .await;
        call(&app, "DELETE", "/knowledge/rule-name-your-comparison-baseline", None).await;

        let (_, v) =
            call(&app, "POST", "/knowledge", Some(upsert_body("rule-name-your-comparison-base", "y")))
                .await;
        let similar: Vec<&str> =
            v["similar"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect()).unwrap_or_default();
        assert!(
            similar.contains(&"rule-name-your-comparison-baseline"),
            "a retracted near-twin must still be surfaced, got {v}"
        );
    }

    #[tokio::test]
    async fn diff_endpoint() {
        let (app, _data, _idx) = app_with_tmp();
        call(&app, "POST", "/knowledge", Some(upsert_body("svc", "the port is 8080"))).await;

        // one version: there is nothing to compare it against, and saying so
        // beats inventing an empty diff
        let (st, v) = call(&app, "GET", "/knowledge/svc/diff", None).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert!(v["error"].as_str().unwrap().contains("only one version"));

        call(&app, "POST", "/knowledge", Some(upsert_body("svc", "the port is 9090"))).await;

        // no revisions: the two most recent, which is "what just changed"
        let (st, v) = call(&app, "GET", "/knowledge/svc/diff", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["changed"], true);
        assert_eq!(v["body"]["removed"][0], "the port is 8080");
        assert_eq!(v["body"]["added"][0], "the port is 9090");

        // explicit revisions, and a version compared with itself changes nothing
        let (_, h) = call(&app, "GET", "/knowledge/svc/history", None).await;
        let newest = h["versions"][0]["sha"].as_str().unwrap().to_string();
        let oldest = h["versions"][1]["sha"].as_str().unwrap().to_string();
        let (st, v) =
            call(&app, "GET", &format!("/knowledge/svc/diff?from={oldest}&to={newest}"), None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["body"]["added"][0], "the port is 9090");
        let (_, v) =
            call(&app, "GET", &format!("/knowledge/svc/diff?from={newest}&to={newest}"), None).await;
        assert_eq!(v["changed"], false);
        assert!(v["fields"].as_array().unwrap().is_empty());

        // reversing the pair reverses the diff
        let (_, v) =
            call(&app, "GET", &format!("/knowledge/svc/diff?from={newest}&to={oldest}"), None).await;
        assert_eq!(v["body"]["removed"][0], "the port is 9090");
        assert_eq!(v["body"]["added"][0], "the port is 8080");

        // an unresolvable revision is a 404, never a 500
        let (st, _) = call(&app, "GET", "/knowledge/svc/diff?from=zzz&to=zzz", None).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let (st, _) = call(&app, "GET", "/knowledge/nope/diff", None).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn search_changed_between() {
        let (app, _data, _idx) = app_with_tmp();
        call(&app, "POST", "/knowledge", Some(upsert_body("svc", "the port is 8080"))).await;

        // a window that contains today catches it; one in the past does not
        let (st, v) =
            call(&app, "GET", "/search?q=port&changed_between=2000-01-01,2999-01-01", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["count"], 1);
        assert_eq!(v["semantic"], false, "a window question is not a head question");

        let (_, v) =
            call(&app, "GET", "/search?q=port&changed_between=2000-01-01,2000-12-31", None).await;
        assert_eq!(v["count"], 0);

        // a busy key still reports once
        for body in ["revision a", "revision b", "revision c"] {
            call(&app, "POST", "/knowledge", Some(upsert_body("busy", body))).await;
        }
        let (_, v) =
            call(&app, "GET", "/search?q=revision&changed_between=2000-01-01,2999-01-01", None).await;
        assert_eq!(v["count"], 1, "one row per key that moved");
        assert!(v["hits"][0]["body"].as_str().unwrap().contains("revision c"));
    }

    #[rstest]
    #[case("2026-08-01", "expected <start>,<end>")]
    #[case("nonsense,2026-08-02", "must be an RFC3339")]
    #[case("2026-08-05,2026-08-01", "start is after end")]
    #[tokio::test]
    async fn search_changed_between_rejects_garbage(#[case] value: &str, #[case] hint: &str) {
        let (app, _data, _idx) = app_with_tmp();
        call(&app, "POST", "/knowledge", Some(upsert_body("svc", "body"))).await;
        let (st, v) = call(&app, "GET", &format!("/search?changed_between={value}"), None).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{value} must be refused, got {v}");
        assert!(v["error"].as_str().unwrap().contains(hint), "{v}");
    }

    /// One instant and one window are different questions; answering both at
    /// once would silently pick one.
    #[tokio::test]
    async fn as_of_and_changed_between_are_mutually_exclusive() {
        let (app, _data, _idx) = app_with_tmp();
        call(&app, "POST", "/knowledge", Some(upsert_body("svc", "body"))).await;
        let (st, v) = call(
            &app,
            "GET",
            "/search?as_of=2026-08-01&changed_between=2026-08-01,2026-08-02",
            None,
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert!(v["error"].as_str().unwrap().contains("different questions"));
    }

    #[tokio::test]
    async fn search_as_of_over_http() {
        let (app, _data, _idx) = app_with_tmp();
        call(&app, "POST", "/knowledge", Some(upsert_body("svc", "the port is 8080"))).await;
        call(&app, "POST", "/knowledge", Some(upsert_body("svc", "the port is 9090"))).await;

        // a date in the past: the entry did not exist yet
        let (st, v) = call(&app, "GET", "/search?q=port&as_of=2000-01-01", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["count"], 0);

        // a date in the future: one row, the current value, and never semantic -
        // the vector index holds head vectors only
        let (st, v) = call(&app, "GET", "/search?q=port&as_of=2999-01-01", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["count"], 1, "as_of returns one version per key");
        assert_eq!(v["semantic"], false, "as_of must not be answered from head vectors");
        assert!(v["hits"][0]["body"].as_str().unwrap().contains("9090"));

        // a git revision is accepted: it is what /history just handed back
        let (_, h) = call(&app, "GET", "/knowledge/svc/history", None).await;
        let sha = h["versions"][0]["sha"].as_str().unwrap().to_string();
        let (st, v) = call(&app, "GET", &format!("/search?q=port&as_of={sha}"), None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["count"], 1);

        // an RFC3339 instant is accepted too
        let (st, _) =
            call(&app, "GET", "/search?q=port&as_of=2026-08-01T12%3A00%3A00Z", None).await;
        assert_eq!(st, StatusCode::OK);
    }

    #[rstest]
    #[case("not-a-date")]
    #[case("2026-13-99")]
    #[case("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef")]
    #[tokio::test]
    async fn search_as_of_rejects_garbage(#[case] value: &str) {
        let (app, _data, _idx) = app_with_tmp();
        call(&app, "POST", "/knowledge", Some(upsert_body("svc", "body"))).await;
        let (st, v) = call(&app, "GET", &format!("/search?as_of={value}"), None).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "as_of={value} must be refused, got {v}");
        assert!(v["error"].as_str().unwrap().contains("as_of"));
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
        // the default listing shows only live reports; --all adds the archive
        let (_, v) = call(&app, "GET", "/incidents", None).await;
        assert_eq!(v["count"], 1, "archived hidden by default: {v}");
        let (_, v) = call(&app, "GET", "/incidents?all=true", None).await;
        assert_eq!(v["count"], 2);
        assert_eq!(v["incidents"][1]["key"], key);
        assert_eq!(v["incidents"][1]["status"], "resolved");
        assert_eq!(v["incidents"][1]["archived"], true);
        // an explicit status filter looks into the archive on its own
        let (_, v) = call(&app, "GET", "/incidents?status=resolved", None).await;
        assert_eq!(v["count"], 1, "{v}");
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
        // still part of the record: listed with --all, found by default search
        let (_, v) = call(&app, "GET", "/incidents", None).await;
        assert_eq!(v["count"], 0, "archived hidden from the default listing: {v}");
        let (_, v) = call(&app, "GET", "/incidents?all=true", None).await;
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

        // done: closed, archived — out of the default listing, kept in --all
        let (st, v) = call(&app, "POST", &format!("/tasks/{key}/resolve"),
            Some(json!({"resolution": "Retention raised to 72h with a 2G disk budget."}))).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert_eq!(v["archived"], true, "{v}");
        let (_, v) = call(&app, "GET", "/tasks", None).await;
        assert_eq!(v["count"], 0, "archived hidden by default: {v}");
        let (_, v) = call(&app, "GET", "/tasks?all=true", None).await;
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

        // dropped needs a reason too, and ranks below done in the --all listing
        call(&app, "POST", "/tasks", Some(json!({
            "key": "task-try-foo", "title": "Try foo", "body": "x",
        }))).await;
        let (_, v) = call(&app, "POST", "/tasks/task-try-foo/resolve",
            Some(json!({"status": "dropped", "resolution": "Obsolete after the bar rewrite."}))).await;
        assert_eq!(v["archived"], true, "{v}");
        let (_, v) = call(&app, "GET", "/tasks?all=true", None).await;
        assert_eq!(v["count"], 2);
        assert_eq!(v["tasks"][0]["status"], "done");
        assert_eq!(v["tasks"][1]["status"], "dropped");

        // the incident endpoint refuses tasks
        let (st, v) = call(&app, "POST", &format!("/incidents/{key}/resolve"),
            Some(json!({"resolution": "x"}))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
        assert!(v["error"].as_str().unwrap().contains("not an incident"));
    }

    // priority + the widened lifecycle: create, filter, transition, archive
    #[tokio::test]
    async fn task_priority_and_status_flow() {
        let (app, _data, _idx) = app_with_tmp();

        // priority rides through create -> GET -> listing -> search
        let key = "task-raise-log-retention";
        let (st, v) = call(&app, "POST", "/tasks", Some(json!({
            "key": key, "title": "Raise container log retention to 72h",
            "body": "Short retention loses evidence.", "priority": "high",
        }))).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        let (_, v) = call(&app, "GET", &format!("/knowledge/{key}"), None).await;
        assert_eq!(v["priority"], "high");
        assert_eq!(v["blocked_reason"], "");
        assert_eq!(v["status"], "open");

        // an unranked task is the default and stays empty, never guessed
        call(&app, "POST", "/tasks", Some(json!({
            "key": "task-try-foo", "title": "Try foo", "body": "an idea",
        }))).await;
        let (_, v) = call(&app, "GET", "/knowledge/task-try-foo", None).await;
        assert_eq!(v["priority"], "", "{v}");

        // a second ranked task, plus one that is in flight
        call(&app, "POST", "/tasks", Some(json!({
            "key": "task-swap-disk", "title": "Swap the failing disk", "body": "x",
            "priority": "critical", "status": "in_progress",
        }))).await;

        // the exact priority filter on the listing
        let (_, v) = call(&app, "GET", "/tasks?priority=high", None).await;
        assert_eq!(v["count"], 1, "{v}");
        assert_eq!(v["tasks"][0]["key"], key);
        assert_eq!(v["tasks"][0]["priority"], "high");
        let (_, v) = call(&app, "GET", "/tasks?priority=low", None).await;
        assert_eq!(v["count"], 0);
        // an empty ?priority= (what the CLI sends) is "no filter"
        let (_, v) = call(&app, "GET", "/tasks?status=&priority=&limit=50", None).await;
        assert_eq!(v["count"], 3, "{v}");
        // it combines with the status filter
        let (_, v) = call(&app, "GET", "/tasks?status=in_progress&priority=critical", None).await;
        assert_eq!(v["count"], 1, "{v}");
        assert_eq!(v["tasks"][0]["key"], "task-swap-disk");
        // and search takes the same exact filter
        let (_, v) = call(&app, "GET", "/search?q=&kind=task&priority=critical", None).await;
        assert_eq!(v["count"], 1, "{v}");
        assert_eq!(v["hits"][0]["key"], "task-swap-disk");
        assert_eq!(v["hits"][0]["priority"], "critical");

        // block it: the reason is recorded and comes back everywhere
        let (st, v) = call(&app, "POST", "/tasks", Some(json!({
            "key": "task-swap-disk", "title": "Swap the failing disk", "body": "x",
            "priority": "critical", "status": "blocked",
            "blocked_reason": "waiting on the replacement disk to arrive",
        }))).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert!(v.get("archived").is_none(), "blocked is not an ending: {v}");
        let (_, v) = call(&app, "GET", "/knowledge/task-swap-disk", None).await;
        assert_eq!(v["status"], "blocked");
        assert_eq!(v["blocked_reason"], "waiting on the replacement disk to arrive");
        assert!(v.get("archived").is_none(), "still live in the canon: {v}");
        let (_, v) = call(&app, "GET", "/tasks?status=blocked", None).await;
        assert_eq!(v["tasks"][0]["blocked_reason"], "waiting on the replacement disk to arrive");
        let (_, v) = call(&app, "GET", "/search?q=&kind=task&status=blocked", None).await;
        assert_eq!(v["hits"][0]["blocked_reason"], "waiting on the replacement disk to arrive");

        // all three live statuses are live: listed by default and counted
        let (_, v) = call(&app, "GET", "/tasks", None).await;
        assert_eq!(v["count"], 3, "open + in_progress(now blocked) + open: {v}");
        let (_, v) = call(&app, "GET", "/healthz", None).await;
        assert_eq!(v["open_tasks"], 3, "blocked and in_progress count as work: {v}");

        // moving off blocked clears the stale reason instead of committing a
        // state that contradicts itself
        let (_, v) = call(&app, "POST", "/tasks/task-swap-disk/resolve",
            Some(json!({"status": "in_progress"}))).await;
        assert_eq!(v["changed"], true, "{v}");
        let (_, v) = call(&app, "GET", "/knowledge/task-swap-disk", None).await;
        assert_eq!(v["status"], "in_progress");
        assert_eq!(v["blocked_reason"], "", "stale reason dropped: {v}");
        assert_eq!(v["priority"], "critical", "priority survives the transition: {v}");

        // only the terminal statuses close and archive
        let (_, v) = call(&app, "POST", "/tasks/task-swap-disk/resolve",
            Some(json!({"resolution": "New disk installed and the array rebuilt."}))).await;
        assert_eq!(v["archived"], true, "{v}");
        let (_, v) = call(&app, "GET", "/healthz", None).await;
        assert_eq!(v["open_tasks"], 2);
        let (_, v) = call(&app, "GET", "/tasks", None).await;
        assert_eq!(v["count"], 2, "archived hidden by default: {v}");
        // the archived task keeps its priority in the record
        let (_, v) = call(&app, "GET", "/knowledge/task-swap-disk", None).await;
        assert_eq!(v["archived"], true);
        assert_eq!(v["priority"], "critical", "{v}");
        assert_eq!(v["status"], "done");
        let (_, v) = call(&app, "GET", "/tasks?all=true&priority=critical", None).await;
        assert_eq!(v["count"], 1, "the archive answers the priority filter too: {v}");
        assert_eq!(v["tasks"][0]["archived"], true);

        // closing a blocked task directly: the reason is dropped, not carried
        call(&app, "POST", "/tasks", Some(json!({
            "key": "task-try-foo", "title": "Try foo", "body": "an idea",
            "status": "blocked", "blocked_reason": "waiting on the bar rewrite",
        }))).await;
        let (_, v) = call(&app, "POST", "/tasks/task-try-foo/resolve",
            Some(json!({"status": "dropped", "resolution": "Obsolete after the bar rewrite."}))).await;
        assert_eq!(v["archived"], true, "{v}");
        let (_, v) = call(&app, "GET", "/knowledge/task-try-foo", None).await;
        assert_eq!(v["status"], "dropped");
        assert_eq!(v["blocked_reason"], "", "{v}");
    }

    // ownership and the parent link: create -> GET -> listing -> search
    #[tokio::test]
    async fn task_ownership_and_parent_flow() {
        let (app, _data, _idx) = app_with_tmp();

        // an umbrella task, claimed under a stable public label
        let parent = "task-migrate-logs";
        let (st, v) = call(&app, "POST", "/tasks", Some(json!({
            "key": parent, "title": "Migrate the log pipeline", "body": "The umbrella task.",
            "status": "in_progress", "priority": "high", "assignee": "agent-a",
        }))).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert!(v.get("unknown_parent").is_none(), "a top-level task has no parent: {v}");

        // a child hangs under it
        let child = "task-migrate-logs-step-one";
        let (st, v) = call(&app, "POST", "/tasks", Some(json!({
            "key": child, "title": "Step one: measure the volume", "body": "x",
            "assignee": "agent-b", "parent_task": parent,
        }))).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert!(v.get("unknown_parent").is_none(), "the parent exists: {v}");

        // a forward reference is reported, not refused — the parent may be
        // filed after the child
        let (st, v) = call(&app, "POST", "/tasks", Some(json!({
            "key": "task-orphan", "title": "Filed before its parent", "body": "x",
            "parent_task": "task-not-written-yet",
        }))).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert_eq!(v["unknown_parent"], "task-not-written-yet");

        // GET carries both fields; an unclaimed task carries them empty
        let (_, v) = call(&app, "GET", &format!("/knowledge/{child}"), None).await;
        assert_eq!(v["assignee"], "agent-b");
        assert_eq!(v["parent_task"], parent);
        let (_, v) = call(&app, "GET", "/knowledge/task-orphan", None).await;
        assert_eq!(v["assignee"], "", "{v}");
        assert_eq!(v["parent_task"], "task-not-written-yet");

        // the listing filters exactly
        let (_, v) = call(&app, "GET", "/tasks?assignee=agent-a", None).await;
        assert_eq!(v["count"], 1, "{v}");
        assert_eq!(v["tasks"][0]["key"], parent);
        assert_eq!(v["tasks"][0]["assignee"], "agent-a");
        let (_, v) = call(&app, "GET", &format!("/tasks?parent_task={parent}"), None).await;
        assert_eq!(v["count"], 1, "{v}");
        assert_eq!(v["tasks"][0]["key"], child);
        assert_eq!(v["tasks"][0]["parent_task"], parent);
        let (_, v) = call(&app, "GET", "/tasks?assignee=agent-nobody", None).await;
        assert_eq!(v["count"], 0);
        // empty params (what the CLI sends) are "no filter", not "match nothing"
        let (_, v) = call(&app, "GET", "/tasks?assignee=&parent_task=&limit=50", None).await;
        assert_eq!(v["count"], 3, "{v}");

        // and search takes the same exact filters
        let (_, v) = call(&app, "GET", "/search?q=&kind=task&assignee=agent-b", None).await;
        assert_eq!(v["count"], 1, "{v}");
        assert_eq!(v["hits"][0]["key"], child);
        assert_eq!(v["hits"][0]["assignee"], "agent-b");
        assert_eq!(v["hits"][0]["parent_task"], parent);
        let (_, v) =
            call(&app, "GET", &format!("/search?q=&kind=task&parent_task={parent}"), None).await;
        assert_eq!(v["count"], 1, "{v}");
        let (_, v) = call(&app, "GET", "/search?q=&kind=task&assignee=&parent_task=", None).await;
        assert_eq!(v["count"], 3, "{v}");

        // an archived parent is still a known parent: the record outlives the file
        call(&app, "POST", &format!("/tasks/{parent}/resolve"),
            Some(json!({"resolution": "Pipeline migrated."}))).await;
        let (_, v) = call(&app, "POST", "/tasks", Some(json!({
            "key": "task-migrate-logs-step-two", "title": "Step two", "body": "x",
            "parent_task": parent,
        }))).await;
        assert!(v.get("unknown_parent").is_none(), "an archived parent still exists: {v}");
    }

    // the partial transition: move a task without resending what it says
    #[tokio::test]
    async fn task_transition_partial_updates() {
        let (app, _data, _idx) = app_with_tmp();
        let key = "task-raise-log-retention";
        call(&app, "POST", "/tasks", Some(json!({
            "key": key, "title": "Raise container log retention to 72h",
            "body": "Short retention loses evidence.\n\n- [ ] measure log volume first\n",
            "priority": "high", "tags": ["observability"],
            "knowledge": ["web-app-architecture"], "refs": ["see the compose file on the log host"],
        }))).await;

        // pick it up: status plus an owner label, nothing else resent
        let (st, v) = call(&app, "POST", &format!("/tasks/{key}/transition"),
            Some(json!({"status": "in_progress", "assignee": "agent-a"}))).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert_eq!(v["changed"], true);
        assert_eq!(v["action"], "updated");
        assert_eq!(v["status"], "in_progress");
        assert_eq!(v["assignee"], "agent-a");
        let (_, v) = call(&app, "GET", &format!("/knowledge/{key}"), None).await;
        assert_eq!(v["status"], "in_progress");
        assert_eq!(v["assignee"], "agent-a");
        assert_eq!(v["title"], "Raise container log retention to 72h", "title kept: {v}");
        assert_eq!(v["priority"], "high", "omitted metadata survives: {v}");
        assert_eq!(v["tags"], json!(["observability"]));
        assert_eq!(v["refs"], json!(["see the compose file on the log host"]));
        assert_eq!(v["knowledge"], json!(["web-app-architecture"]));
        assert!(v["body"].as_str().unwrap().contains("measure log volume"), "body kept: {v}");
        assert_eq!(v["open_followups"], 1);
        assert!(v.get("archived").is_none(), "a live transition never archives: {v}");
        let (_, v) = call(&app, "GET", "/healthz", None).await;
        assert_eq!(v["open_tasks"], 1, "still work in flight: {v}");

        // block it: the reason lands, everything else stays
        let (_, v) = call(&app, "POST", &format!("/tasks/{key}/transition"),
            Some(json!({"status": "blocked", "blocked_reason": "waiting on the disk budget approval"}))).await;
        assert_eq!(v["status"], "blocked", "{v}");
        let (_, v) = call(&app, "GET", &format!("/knowledge/{key}"), None).await;
        assert_eq!(v["blocked_reason"], "waiting on the disk budget approval");
        assert_eq!(v["assignee"], "agent-a", "an omitted assignee is kept: {v}");
        assert_eq!(v["priority"], "high");

        // leaving blocked clears the reason even though the request never named it
        let (_, v) = call(&app, "POST", &format!("/tasks/{key}/transition"),
            Some(json!({"status": "in_progress"}))).await;
        assert_eq!(v["changed"], true, "{v}");
        let (_, v) = call(&app, "GET", &format!("/knowledge/{key}"), None).await;
        assert_eq!(v["blocked_reason"], "", "stale reason dropped: {v}");
        assert_eq!(v["status"], "in_progress");

        // hand it back: an explicit empty assignee unclaims it
        let (_, v) = call(&app, "POST", &format!("/tasks/{key}/transition"),
            Some(json!({"status": "open", "assignee": ""}))).await;
        assert_eq!(v["assignee"], "", "{v}");
        let (_, v) = call(&app, "GET", &format!("/knowledge/{key}"), None).await;
        assert_eq!(v["assignee"], "");
        assert_eq!(v["status"], "open");

        // relate it to a parent without touching anything else
        let (_, v) = call(&app, "POST", &format!("/tasks/{key}/transition"),
            Some(json!({"status": "in_progress", "parent_task": "task-migrate-logs"}))).await;
        assert_eq!(v["unknown_parent"], "task-migrate-logs", "forward reference reported: {v}");
        let (_, v) = call(&app, "GET", &format!("/knowledge/{key}"), None).await;
        assert_eq!(v["parent_task"], "task-migrate-logs");
        assert_eq!(v["priority"], "high", "{v}");

        // every transition is a git version: history + get --at reconstruct who
        // held the task, and in which status, at any point
        let (_, v) = call(&app, "GET", &format!("/knowledge/{key}/history"), None).await;
        let versions = v["versions"].as_array().unwrap().clone();
        assert_eq!(versions.len(), 6, "create + 5 transitions: {versions:?}");
        let claimed = versions[versions.len() - 2]["sha"].as_str().unwrap().to_string();
        let (_, v) = call(&app, "GET", &format!("/knowledge/{key}?at={claimed}"), None).await;
        assert_eq!(v["assignee"], "agent-a", "the earlier owner is reconstructable: {v}");
        assert_eq!(v["status"], "in_progress");

        // closing still goes through resolve, and the record keeps the owner
        let (_, v) = call(&app, "POST", &format!("/tasks/{key}/resolve"),
            Some(json!({"resolution": "Raised to 72h with a 2G disk budget."}))).await;
        assert_eq!(v["archived"], true, "{v}");
        let (_, v) = call(&app, "GET", &format!("/knowledge/{key}"), None).await;
        assert_eq!(v["status"], "done");
        assert_eq!(v["parent_task"], "task-migrate-logs", "{v}");

        // an archived task can be picked back up: the file returns
        let (_, v) = call(&app, "POST", &format!("/tasks/{key}/transition"),
            Some(json!({"status": "in_progress", "assignee": "agent-b"}))).await;
        assert_eq!(v["action"], "created", "reopening recreates the file: {v}");
        let (_, v) = call(&app, "GET", &format!("/knowledge/{key}"), None).await;
        assert!(v.get("archived").is_none(), "back in the canon: {v}");
        assert_eq!(v["assignee"], "agent-b");
    }

    // Parent links form a chain, never a ring — for both wholesale writes and
    // partial transitions, including cycles longer than self-parenting.
    #[tokio::test]
    async fn task_parent_cycles_rejected() {
        let (app, _data, _idx) = app_with_tmp();
        for (key, parent) in [
            ("task-project", ""),
            ("task-project-step-one", "task-project"),
            ("task-project-step-two", "task-project-step-one"),
        ] {
            let (st, v) = call(&app, "POST", "/tasks", Some(json!({
                "key": key, "title": key, "body": "x", "parent_task": parent,
            }))).await;
            assert_eq!(st, StatusCode::OK, "{v}");
        }

        // project -> step two -> step one -> project would be a three-node cycle
        let (st, v) = call(&app, "POST", "/tasks", Some(json!({
            "key": "task-project", "title": "task-project", "body": "x",
            "parent_task": "task-project-step-two",
        }))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
        assert!(v["error"].as_str().unwrap().contains("cycle"), "{v}");

        let (st, v) = call(&app, "POST", "/tasks/task-project/transition", Some(json!({
            "status": "in_progress", "parent_task": "task-project-step-two",
        }))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
        assert!(v["error"].as_str().unwrap().contains("cycle"), "{v}");

        // Neither rejected write changed the root.
        let (_, v) = call(&app, "GET", "/knowledge/task-project", None).await;
        assert_eq!(v["status"], "open", "{v}");
        assert_eq!(v["parent_task"], "", "{v}");
    }

    // the transition endpoint is not a closing endpoint, and not a way past
    // validation either
    #[tokio::test]
    async fn task_transition_rejections() {
        let (app, _data, _idx) = app_with_tmp();
        let key = "task-swap-disk";
        call(&app, "POST", "/tasks", Some(json!({
            "key": key, "title": "Swap the failing disk", "body": "x", "priority": "critical",
        }))).await;
        call(&app, "POST", "/knowledge", Some(upsert_body("plain-note", "a fact"))).await;

        // the terminal statuses need an outcome — and say where to give one
        for status in ["done", "dropped"] {
            let (st, v) = call(&app, "POST", &format!("/tasks/{key}/transition"),
                Some(json!({"status": status}))).await;
            assert_eq!(st, StatusCode::BAD_REQUEST, "{status}: {v}");
            let err = v["error"].as_str().unwrap();
            assert!(err.contains("kyb done"), "{status}: {err}");
            assert!(err.contains("/resolve"), "{status}: {err}");
        }

        let bad = [
            json!({"status": "wip"}),
            json!({"status": ""}),
            json!({"status": "mitigated"}),
            json!({"status": "in_progress", "assignee": "token: ghp_abcdefghijklmnopqrstuvwxyz123456"}),
            json!({"status": "in_progress", "assignee": "x".repeat(model::ASSIGNEE_MAX_LEN + 1)}),
            json!({"status": "in_progress", "parent_task": "not-a-task-key"}),
            json!({"status": "in_progress", "parent_task": "inc-2026-08-15-oom"}),
        ];
        for payload in bad {
            let (st, v) =
                call(&app, "POST", &format!("/tasks/{key}/transition"), Some(payload.clone())).await;
            assert_eq!(st, StatusCode::BAD_REQUEST, "payload {payload}: {v}");
        }
        // a task cannot hang under itself — the smallest broken tree
        let (st, v) = call(&app, "POST", &format!("/tasks/{key}/transition"),
            Some(json!({"status": "in_progress", "parent_task": key}))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "self-parenting: {v}");
        assert!(v["error"].as_str().unwrap().contains("own parent"), "{v}");

        // wrong kind, missing key, garbage key
        let (st, v) = call(&app, "POST", "/tasks/plain-note/transition",
            Some(json!({"status": "in_progress"}))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
        assert!(v["error"].as_str().unwrap().contains("not a task"), "{v}");
        let (st, _) = call(&app, "POST", "/tasks/task-nope/transition",
            Some(json!({"status": "in_progress"}))).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let (st, _) = call(&app, "POST", "/tasks/Bad%20Key/transition",
            Some(json!({"status": "in_progress"}))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        let (st, _) = call(&app, "POST", &format!("/tasks/{key}/transition"), Some(json!({}))).await;
        assert!(st.is_client_error(), "status is required");

        // through all of that the task never moved
        let (_, v) = call(&app, "GET", &format!("/knowledge/{key}"), None).await;
        assert_eq!(v["status"], "open", "{v}");
        assert_eq!(v["assignee"], "");
        assert_eq!(v["parent_task"], "");
        assert_eq!(v["priority"], "critical");
    }

    // tasks written before this feature carry neither field and keep working:
    // they list, they filter, and they can be claimed with one transition
    #[tokio::test]
    async fn legacy_task_without_ownership_still_works() {
        let (app, _data, _idx) = app_with_tmp();
        let key = "task-old-note";
        let (st, v) = call(&app, "POST", "/tasks", Some(json!({
            "key": key, "title": "An old note", "body": "written before ownership existed",
        }))).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        let (_, v) = call(&app, "GET", &format!("/knowledge/{key}"), None).await;
        assert_eq!(v["assignee"], "", "{v}");
        assert_eq!(v["parent_task"], "");
        assert_eq!(v["status"], "open");
        let (_, v) = call(&app, "GET", "/tasks", None).await;
        assert_eq!(v["count"], 1);
        assert_eq!(v["tasks"][0]["assignee"], "");
        assert_eq!(v["tasks"][0]["parent_task"], "");

        // claiming it needs neither the title nor the body
        let (st, v) = call(&app, "POST", &format!("/tasks/{key}/transition"),
            Some(json!({"status": "in_progress", "assignee": "agent-a"}))).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        let (_, v) = call(&app, "GET", &format!("/knowledge/{key}"), None).await;
        assert_eq!(v["assignee"], "agent-a");
        assert!(v["body"].as_str().unwrap().contains("before ownership"), "{v}");
    }

    // --- task payload validation: the new fields ---
    #[rstest]
    #[case::priority_high(json!({"key": "task-a", "title": "t", "body": "x", "priority": "high"}), 200)]
    #[case::priority_absent(json!({"key": "task-a", "title": "t", "body": "x"}), 200)]
    #[case::priority_empty(json!({"key": "task-a", "title": "t", "body": "x", "priority": ""}), 200)]
    #[case::priority_unknown(json!({"key": "task-a", "title": "t", "body": "x", "priority": "urgent"}), 400)]
    #[case::priority_wrong_case(json!({"key": "task-a", "title": "t", "body": "x", "priority": "HIGH"}), 400)]
    #[case::status_in_progress(json!({"key": "task-a", "title": "t", "body": "x", "status": "in_progress"}), 200)]
    #[case::status_blocked_bare(json!({"key": "task-a", "title": "t", "body": "x", "status": "blocked"}), 200)]
    #[case::status_blocked_with_reason(json!({"key": "task-a", "title": "t", "body": "x", "status": "blocked", "blocked_reason": "waiting on vendor"}), 200)]
    #[case::status_unknown(json!({"key": "task-a", "title": "t", "body": "x", "status": "wip"}), 400)]
    #[case::stale_reason_on_open(json!({"key": "task-a", "title": "t", "body": "x", "blocked_reason": "waiting on vendor"}), 400)]
    #[case::stale_reason_on_in_progress(json!({"key": "task-a", "title": "t", "body": "x", "status": "in_progress", "blocked_reason": "waiting on vendor"}), 400)]
    #[case::stale_reason_on_done(json!({"key": "task-a", "title": "t", "body": "x", "status": "done", "resolution": "shipped", "blocked_reason": "waiting on vendor"}), 400)]
    #[case::secret_in_reason(json!({"key": "task-a", "title": "t", "body": "x", "status": "blocked", "blocked_reason": "password: super123secret"}), 400)]
    #[case::in_progress_needs_no_resolution(json!({"key": "task-a", "title": "t", "body": "x", "status": "in_progress"}), 200)]
    #[case::done_still_needs_resolution(json!({"key": "task-a", "title": "t", "body": "x", "status": "done"}), 400)]
    #[case::assignee_absent(json!({"key": "task-a", "title": "t", "body": "x"}), 200)]
    #[case::assignee_label(json!({"key": "task-a", "title": "t", "body": "x", "assignee": "agent-a"}), 200)]
    #[case::assignee_blank_is_unclaimed(json!({"key": "task-a", "title": "t", "body": "x", "assignee": "   "}), 200)]
    #[case::assignee_secret(json!({"key": "task-a", "title": "t", "body": "x", "assignee": "password: super123secret"}), 400)]
    #[case::parent_absent(json!({"key": "task-a", "title": "t", "body": "x"}), 200)]
    #[case::parent_task_key(json!({"key": "task-a", "title": "t", "body": "x", "parent_task": "task-parent"}), 200)]
    #[case::parent_not_a_task(json!({"key": "task-a", "title": "t", "body": "x", "parent_task": "nats-streams"}), 400)]
    #[case::parent_malformed(json!({"key": "task-a", "title": "t", "body": "x", "parent_task": "task-Bad Key"}), 400)]
    #[case::parent_traversal(json!({"key": "task-a", "title": "t", "body": "x", "parent_task": "../task-x"}), 400)]
    #[case::parent_self(json!({"key": "task-a", "title": "t", "body": "x", "parent_task": "task-a"}), 400)]
    #[tokio::test]
    async fn task_validation_matrix(#[case] payload: Value, #[case] expect: u16) {
        let (app, _data, _idx) = app_with_tmp();
        let (st, v) = call(&app, "POST", "/tasks", Some(payload)).await;
        assert_eq!(st.as_u16(), expect, "response: {v}");
    }

    // the task-only fields must not leak onto the other kinds
    #[rstest]
    #[case::knowledge_priority("/knowledge", json!({"key": "k", "title": "t", "body": "x", "priority": "high"}))]
    #[case::knowledge_assignee("/knowledge", json!({"key": "k", "title": "t", "body": "x", "assignee": "agent-a"}))]
    #[case::knowledge_parent("/knowledge", json!({"key": "k", "title": "t", "body": "x", "parent_task": "task-x"}))]
    #[case::incident_priority("/incidents", json!({"key": "inc-a", "title": "t", "body": "x", "service": "s", "severity": "low", "priority": "high"}))]
    #[case::incident_assignee("/incidents", json!({"key": "inc-a", "title": "t", "body": "x", "service": "s", "severity": "low", "assignee": "agent-a"}))]
    #[case::incident_parent("/incidents", json!({"key": "inc-a", "title": "t", "body": "x", "service": "s", "severity": "low", "parent_task": "task-x"}))]
    #[tokio::test]
    async fn task_only_fields_ignored_on_other_kinds(#[case] path: &str, #[case] payload: Value) {
        let (app, _data, _idx) = app_with_tmp();
        // unknown fields are dropped by the request types, so the write lands
        // clean rather than smuggling a task field into another kind
        let (st, v) = call(&app, "POST", path, Some(payload)).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        let key = v["key"].as_str().unwrap().to_string();
        let (_, v) = call(&app, "GET", &format!("/knowledge/{key}"), None).await;
        assert!(v.get("priority").is_none(), "priority is task-only: {v}");
        assert!(v.get("blocked_reason").is_none(), "blocked_reason is task-only: {v}");
        assert!(v.get("assignee").is_none(), "assignee is task-only: {v}");
        assert!(v.get("parent_task").is_none(), "parent_task is task-only: {v}");
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
        // an empty query is a listing, and listings read newest first
        let (_, v) = call(&app, "GET", "/search?q=&sort=", None).await;
        assert_eq!(v["hits"][0]["key"], "newer", "empty q defaults to recent: {v}");
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

    // --- same-key interleavings, replayed exactly ---
    //
    // These do not stress the server and hope: the test holds the global writer
    // lock, parks the competing requests on it one after another, and then lets
    // them go. The order is the queue order, so every run reproduces the same
    // interleaving — the one that used to lose a write.

    /// Park a POST on the writer lock the test is holding.
    ///
    /// The test runtime is single-threaded and the request has no other
    /// suspension point on its way to that lock, so by the time the spawned
    /// task has signalled and the scheduler has drained, it is queued on the
    /// mutex — behind whoever was parked before it — with nothing read yet.
    async fn park_on_writer_lock(
        app: &Router,
        uri: String,
        body: Value,
    ) -> tokio::task::JoinHandle<(StatusCode, Value)> {
        let app = app.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = started.send(());
            call(&app, "POST", &uri, Some(body)).await
        });
        ready.await.expect("the request task must start");
        // drain anything already runnable, so the request is parked on the lock
        // before the caller queues the next one behind it
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        task
    }

    fn open_task(key: &str) -> Value {
        json!({
            "key": key, "title": "Rotate the internal TLS certificates",
            "body": "They expire in a week.\n\n- [ ] schedule the restart window\n",
            "priority": "high", "tags": ["security"],
        })
    }

    // Two partial transitions on ONE key: the second must build on the version
    // the first committed, not on the one it replaced.
    #[tokio::test]
    async fn concurrent_transitions_keep_the_earlier_update() {
        let (app, state, _data, _idx) = app_with_state();
        let key = "task-rotate-tls-certs";
        let (st, v) = call(&app, "POST", "/tasks", Some(open_task(key))).await;
        assert_eq!(st, StatusCode::OK, "{v}");

        let guard = state.writer.lock().await;
        let claim = park_on_writer_lock(
            &app,
            format!("/tasks/{key}/transition"),
            json!({"status": "in_progress", "assignee": "agent-a"}),
        )
        .await;
        let block = park_on_writer_lock(
            &app,
            format!("/tasks/{key}/transition"),
            json!({"status": "blocked", "blocked_reason": "waiting on the CA"}),
        )
        .await;
        drop(guard); // the claim parked first, so the claim runs first

        let (st, claimed) = claim.await.unwrap();
        assert_eq!(st, StatusCode::OK, "{claimed}");
        let (st, blocked) = block.await.unwrap();
        assert_eq!(st, StatusCode::OK, "{blocked}");
        assert_eq!(claimed["assignee"], "agent-a");
        assert_eq!(blocked["status"], "blocked");
        assert_eq!(
            blocked["assignee"], "agent-a",
            "the second transition read the first one's commit: {blocked}"
        );

        let (_, v) = call(&app, "GET", &format!("/knowledge/{key}"), None).await;
        assert_eq!(v["status"], "blocked");
        assert_eq!(v["blocked_reason"], "waiting on the CA");
        assert_eq!(v["assignee"], "agent-a", "the claim was not overwritten: {v}");
        assert_eq!(v["priority"], "high", "untouched metadata survives: {v}");
        let (_, v) = call(&app, "GET", &format!("/knowledge/{key}/history"), None).await;
        assert_eq!(
            v["versions"].as_array().unwrap().len(),
            3,
            "create plus both transitions, none dropped: {v}"
        );
    }

    // A close archives the entry it committed — never one that has been
    // reopened behind its back.
    #[tokio::test]
    async fn a_close_cannot_archive_a_task_reopened_behind_it() {
        let (app, state, _data, _idx) = app_with_state();
        let key = "task-rotate-tls-certs";
        call(&app, "POST", "/tasks", Some(open_task(key))).await;

        let guard = state.writer.lock().await;
        let close = park_on_writer_lock(
            &app,
            format!("/tasks/{key}/resolve"),
            json!({"resolution": "Rotated by hand; automation filed separately."}),
        )
        .await;
        let reopen = park_on_writer_lock(
            &app,
            format!("/tasks/{key}/transition"),
            json!({"status": "in_progress", "assignee": "agent-b"}),
        )
        .await;
        drop(guard); // close first, reopen second

        let (st, closed) = close.await.unwrap();
        assert_eq!(st, StatusCode::OK, "{closed}");
        assert_eq!(closed["archived"], true, "the close archived what it closed: {closed}");
        let (st, reopened) = reopen.await.unwrap();
        assert_eq!(st, StatusCode::OK, "{reopened}");
        assert_eq!(reopened["status"], "in_progress");

        // the reopen came after the archival and stays: a live task must never
        // be left with its file deleted under it
        let (_, v) = call(&app, "GET", &format!("/knowledge/{key}"), None).await;
        assert_eq!(v["status"], "in_progress");
        assert_eq!(v["assignee"], "agent-b");
        assert!(v.get("archived").is_none(), "the reopened task is back in the canon: {v}");
        assert!(
            v["resolution"].as_str().unwrap().contains("Rotated by hand"),
            "the recorded outcome survives the reopen: {v}"
        );
        let (_, v) = call(&app, "GET", "/healthz", None).await;
        assert_eq!(v["open_tasks"], 1, "work in flight is still counted: {v}");
        let (_, v) = call(&app, "GET", "/tasks", None).await;
        assert_eq!(v["count"], 1, "and it still lists: {v}");
        assert_eq!(v["tasks"][0]["archived"], false);
    }

    // The mirror image: the reopen wins the lock first, so the close must carry
    // what the reopen wrote instead of the version it read before it.
    #[tokio::test]
    async fn a_close_after_a_reopen_carries_the_reopened_state() {
        let (app, state, _data, _idx) = app_with_state();
        let key = "task-rotate-tls-certs";
        call(&app, "POST", "/tasks", Some(open_task(key))).await;

        let guard = state.writer.lock().await;
        let reopen = park_on_writer_lock(
            &app,
            format!("/tasks/{key}/transition"),
            json!({"status": "in_progress", "assignee": "agent-b"}),
        )
        .await;
        let close = park_on_writer_lock(
            &app,
            format!("/tasks/{key}/resolve"),
            json!({"resolution": "Rotated by hand; automation filed separately."}),
        )
        .await;
        drop(guard); // reopen first, close second

        let (st, reopened) = reopen.await.unwrap();
        assert_eq!(st, StatusCode::OK, "{reopened}");
        let (st, closed) = close.await.unwrap();
        assert_eq!(st, StatusCode::OK, "{closed}");
        assert_eq!(closed["archived"], true, "{closed}");

        let (_, v) = call(&app, "GET", &format!("/knowledge/{key}"), None).await;
        assert_eq!(v["status"], "done");
        assert_eq!(v["archived"], true, "a closed task leaves the tree: {v}");
        assert_eq!(
            v["assignee"], "agent-b",
            "the close committed the claim it never resent: {v}"
        );
        assert!(v["resolution"].as_str().unwrap().contains("Rotated by hand"), "{v}");
        let (_, v) = call(&app, "GET", "/healthz", None).await;
        assert_eq!(v["open_tasks"], 0, "{v}");
        let (_, v) = call(&app, "GET", "/tasks", None).await;
        assert_eq!(v["count"], 0, "closed tasks leave the live listing: {v}");
        let (_, v) = call(&app, "GET", "/tasks?all=true", None).await;
        assert_eq!(v["count"], 1, "and stay in the record: {v}");
        assert_eq!(v["tasks"][0]["assignee"], "agent-b");
    }
}

/// Published claims, checked against the software that is supposed to back them.
///
/// A section that says what a system does NOT do is the only part of a document
/// invalidated by our own progress, so it rots fastest and exactly when nobody
/// is rereading it. "Remember to check it last" is an intention; this is a gate.
///
/// Each claim below pairs a sentence that must still be present in a published
/// file with a check of the behaviour it describes. It fails in both directions
/// on purpose:
///
///   - the sentence is there but the behaviour contradicts it -> the text lies;
///   - the sentence is gone -> the table is stale, and whoever rewrote the
///     paragraph has to say what the new claim is and how it is checked.
///
/// The second direction is the one that matters. Without it, a rewrite silently
/// disables the gate.
#[cfg(test)]
mod published_claims {
    use super::api_tests::*;
    use axum::http::StatusCode;

    const BLOG: &str = "docs/blog/agent-memory-that-keeps-its-mistakes/index.html";

    fn published(rel: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {rel}: {e}"))
    }

    fn claims(rel: &str, phrase: &str) {
        assert!(
            published(rel).contains(phrase),
            "{rel} no longer contains the claim {phrase:?}.\n\
             The published text changed but this check did not. Update the claim \
             and its check together, or the gate stops guarding anything."
        );
    }

    /// The post says as_of, changed_between and /diff work. They must.
    #[tokio::test]
    async fn the_post_promises_three_temporal_questions_and_gets_them() {
        claims(BLOG, "<code>--as-of</code> gives the base as");
        claims(BLOG, "<code>--changed-between</code> gives what moved inside a window");
        claims(BLOG, "<code>/diff</code> says what moved <em>inside</em> one");

        let (app, _data, _idx) = app_with_tmp();
        call(&app, "POST", "/knowledge", Some(upsert_body("svc", "the port is 8080"))).await;
        call(&app, "POST", "/knowledge", Some(upsert_body("svc", "the port is 9090"))).await;

        let (st, v) = call(&app, "GET", "/search?q=port&as_of=2999-01-01", None).await;
        assert_eq!(st, StatusCode::OK, "the post promises --as-of");
        assert_eq!(v["count"], 1, "as_of returns one version per key, as the post says");

        let (st, v) =
            call(&app, "GET", "/search?q=port&changed_between=2000-01-01,2999-01-01", None).await;
        assert_eq!(st, StatusCode::OK, "the post promises --changed-between");
        assert_eq!(v["count"], 1);

        let (st, v) = call(&app, "GET", "/knowledge/svc/diff", None).await;
        assert_eq!(st, StatusCode::OK, "the post promises /diff");
        assert_eq!(v["changed"], true);
    }

    /// The post says all three are lexical, and explains why: one vector per key,
    /// for the head version.
    ///
    /// This one is checked at the source rather than through a response, because
    /// the test app runs without an embedding model and reports semantic=false
    /// for every query - which would make a behavioural assertion pass while
    /// proving nothing. The guard below IS the claim, and removing it is exactly
    /// the change (vectors keyed by version) that makes the sentence false.
    #[test]
    fn the_post_says_the_temporal_questions_are_lexical_and_the_guard_is_still_there() {
        claims(BLOG, "All three are lexical");
        claims(BLOG, "one vector per key,");

        let src = published("src/main.rs");
        let guard = src
            .split("let want_semantic")
            .nth(1)
            .and_then(|tail| tail.split(';').next())
            .unwrap_or_default()
            .to_string();
        for needed in ["!history", "as_of.is_none()", "window.is_none()"] {
            assert!(
                guard.contains(needed),
                "want_semantic no longer excludes {needed}, so a versioned query can now be \
                 answered semantically - which makes {BLOG} wrong where it says the temporal \
                 questions are lexical. Update both together."
            );
        }
    }

    /// The post states the second-granularity limit. The behaviour behind it is
    /// covered by index::tests::as_of_is_second_granular; here the sentence is
    /// tied to the constant that keeps it true.
    #[tokio::test]
    async fn the_post_states_the_second_granularity_limit() {
        claims(BLOG, "two versions written inside the same
      second cannot be told apart");

        let (app, _data, _idx) = app_with_tmp();
        call(&app, "POST", "/knowledge", Some(upsert_body("svc", "value one"))).await;
        call(&app, "POST", "/knowledge", Some(upsert_body("svc", "value two"))).await;
        let (_, h) = call(&app, "GET", "/knowledge/svc/history", None).await;
        let older = h["versions"][1]["sha"].as_str().unwrap().to_string();
        let (st, v) = call(&app, "GET", &format!("/search?q=value&as_of={older}"), None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["count"], 1, "one row per key even when the bound cannot separate versions");
    }

    /// The capability list is a promise to clients that check it before asking.
    /// Every name on it must be backed by an endpoint, and the CLI must gate the
    /// same names - a rename on one side and not the other turns the preflight
    /// into a permanent refusal or a silent pass.
    #[tokio::test]
    async fn every_advertised_capability_is_backed_and_gated() {
        let (app, _data, _idx) = app_with_tmp();
        call(&app, "POST", "/knowledge", Some(upsert_body("svc", "one"))).await;
        call(&app, "POST", "/knowledge", Some(upsert_body("svc", "two"))).await;

        let probes = [
            ("as_of", "/search?q=one&as_of=2999-01-01"),
            ("changed_between", "/search?q=one&changed_between=2000-01-01,2999-01-01"),
            ("diff", "/knowledge/svc/diff"),
        ];
        assert_eq!(probes.len(), super::CAPABILITIES.len(), "a capability was added without a probe");

        let cli = published("skills/kyb/bin/kyb");
        for (cap, uri) in probes {
            assert!(super::CAPABILITIES.contains(&cap), "{cap} is probed but not advertised");
            let (st, v) = call(&app, "GET", uri, None).await;
            assert_eq!(st, StatusCode::OK, "advertised {cap} but {uri} answered {st}: {v}");
            assert!(
                cli.contains(&format!("require_capability {cap} ")),
                "the CLI does not gate {cap}, so it would send it to a server that cannot honour it"
            );
        }
    }

    /// README and the agent skill document the same three commands. An agent
    /// that reads the skill and finds a command that does not exist is worse off
    /// than one that never read it.
    #[tokio::test]
    async fn the_skill_and_readme_document_commands_that_exist() {
        for doc in ["README.md", "skills/kyb/SKILL.md"] {
            claims(doc, "--as-of");
            claims(doc, "--changed-between");
            claims(doc, "kyb diff");
        }
        let cli = published("skills/kyb/bin/kyb");
        for flag in ["--as-of", "--changed-between"] {
            assert!(cli.contains(flag), "the docs promise {flag} but the CLI does not parse it");
        }
        assert!(cli.contains("  diff)"), "the docs promise `kyb diff` but the CLI has no such verb");
    }
}
