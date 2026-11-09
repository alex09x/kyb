/*
 * KYB (Know Your Business) — git-backed knowledge base and incident tracker for AI agent fleets
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/kyb
 * SPDX-License-Identifier: MIT
 */

//! The `/confident` routes, behind `kyb confident` and the `kyb_confident_*` MCP
//! tools: check before asking, open a record when asking, record the decision,
//! append outcomes. See confident.rs for what a record is.

use crate::confident::{self, Decision, Outcome, Record};
use crate::confident_index::{Check, Query};
use crate::{AppState, Reply, St, Q};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

fn reject(code: StatusCode, msg: impl std::fmt::Display) -> Reply {
    (code, Json(json!({ "error": msg.to_string() })))
}

fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

fn stamp(t: chrono::DateTime<chrono::Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn caller() -> crate::auth::By {
    crate::auth::by().unwrap_or_default()
}

fn split_list(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or("").split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

/// The kind a caller named, or the one its gist implies.
fn kind_of(explicit: Option<&str>, gist: &str) -> Result<String, Reply> {
    match explicit.map(str::trim).filter(|k| !k.is_empty()) {
        None => Ok(confident::derive_kind(gist).to_string()),
        Some(k) if confident::KINDS.contains(&k) => Ok(k.to_string()),
        Some(k) => Err(reject(
            StatusCode::BAD_REQUEST,
            format!("kind must be one of: {} (got '{k}')", confident::KINDS.join("|")),
        )),
    }
}

async fn query_vector(st: &AppState, gist: &str) -> Option<Vec<f32>> {
    match st.semantic.as_ref()?.embed_query(gist).await {
        Ok(v) => Some(v),
        Err(e) => {
            eprintln!("kyb: confident check falls back to words: {e:#}");
            None
        }
    }
}

async fn run_check(st: &AppState, q: &Query<'_>) -> Check {
    let qvec = query_vector(st, q.gist).await;
    st.confident.check(q, qvec.as_deref())
}

fn refuse_denied(st: &AppState) -> Option<Reply> {
    let by = caller();
    confident::denied(&by.workspace, &st.confident_deny).then(|| {
        reject(StatusCode::FORBIDDEN, "this workspace does not record decisions (KYB_CONFIDENT_DENY)")
    })
}

/// Validate, commit and index a record. The caller holds the writer lock.
fn save(st: &AppState, rec: Record, msg: &str, vector: Option<Vec<f32>>) -> Result<String, Reply> {
    rec.validate().map_err(|e| reject(StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    let body = serde_json::to_string_pretty(&rec).map_err(|e| crate::err500(e.into()))?;
    let (sha, _) = st.store.write_file(&rec.rel_path(), &body, msg).map_err(crate::err500)?;
    st.confident.put(rec, vector);
    Ok(sha)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckQ {
    gist: Option<String>,
    /// Comma-separated: repositories, services or paths the step touches.
    target: Option<String>,
    kind: Option<String>,
    /// The draft question to the owner, in any language; linted, never stored.
    question: Option<String>,
}

pub async fn check(State(st): St, Q(q): Q<CheckQ>) -> Reply {
    let gist = q.gist.as_deref().unwrap_or("").trim().to_string();
    if let Err(e) = confident::validate_gist(&gist) {
        return reject(StatusCode::BAD_REQUEST, format!("{e:#}"));
    }
    let kind = match kind_of(q.kind.as_deref(), &gist) {
        Ok(k) => k,
        Err(r) => return r,
    };
    let target = split_list(q.target.as_deref());
    let session = caller().session;
    let query = Query {
        gist: &gist,
        kind: &kind,
        target: &target,
        session: &session,
        question: q.question.as_deref().unwrap_or(""),
        unchecked: "",
        now: now(),
    };
    let c = run_check(&st, &query).await;
    (StatusCode::OK, Json(json!(c)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AskReq {
    gist: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    target: Vec<String>,
    #[serde(default)]
    situation: String,
    #[serde(default)]
    options: Vec<String>,
    #[serde(default)]
    proposed: String,
    #[serde(default)]
    tradeoff: String,
    #[serde(default)]
    checked: String,
    #[serde(default)]
    unchecked: String,
    /// The question as it goes to the owner; linted, never stored.
    #[serde(default)]
    question: String,
}

pub async fn ask(State(st): St, Json(r): Json<AskReq>) -> Reply {
    if let Some(no) = refuse_denied(&st) {
        return no;
    }
    let gist = r.gist.trim().to_string();
    if let Err(e) = confident::validate_gist(&gist) {
        return reject(StatusCode::BAD_REQUEST, format!("{e:#}"));
    }
    let kind = match kind_of(r.kind.as_deref(), &gist) {
        Ok(k) => k,
        Err(rep) => return rep,
    };
    let at = now();
    let by = caller();
    let rec = Record {
        id: confident::new_id(&gist, at),
        gist: gist.clone(),
        kind: kind.clone(),
        target: r.target.iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect(),
        situation: r.situation.trim().to_string(),
        options: r.options.iter().map(|o| o.trim().to_string()).filter(|o| !o.is_empty()).collect(),
        proposed: r.proposed.trim().to_string(),
        tradeoff: r.tradeoff.trim().to_string(),
        checked: r.checked.trim().to_string(),
        unchecked: r.unchecked.trim().to_string(),
        status: confident::STATUS_OPEN.into(),
        asked_at: stamp(at),
        by: by.clone(),
        ..Default::default()
    };
    if let Err(e) = rec.validate() {
        return reject(StatusCode::BAD_REQUEST, format!("{e:#}"));
    }
    // the check runs before the new record is indexed, so it never finds itself
    let query = Query {
        gist: &gist,
        kind: &kind,
        target: &rec.target,
        session: &by.session,
        question: &r.question,
        unchecked: &rec.unchecked,
        now: at,
    };
    let c = run_check(&st, &query).await;
    let vector = match st.semantic.as_ref() {
        Some(sem) => {
            sem.embed_passage(&gist).await.map_err(|e| eprintln!("kyb: record not embedded: {e:#}")).ok()
        }
        None => None,
    };
    let id = rec.id.clone();
    let _w = st.writer.lock().await;
    match save(&st, rec, &format!("kyb: confident ask {id}"), vector) {
        Ok(sha) => (StatusCode::OK, Json(json!({ "id": id, "sha": sha, "check": c }))),
        Err(rep) => rep,
    }
}

/// Load, change and save one record under the writer lock. `change` answers
/// whether it changed anything: a retried call that repeats what is already
/// recorded commits nothing and says so.
async fn update(
    st: &AppState,
    id: &str,
    what: &str,
    change: impl FnOnce(&mut Record) -> Result<bool, String>,
) -> Reply {
    if let Some(no) = refuse_denied(st) {
        return no;
    }
    let _w = st.writer.lock().await;
    let Some(mut rec) = st.confident.get(id) else {
        return reject(StatusCode::NOT_FOUND, format!("no decision record '{id}'"));
    };
    match change(&mut rec) {
        Err(msg) => return reject(StatusCode::CONFLICT, msg),
        Ok(false) => return (StatusCode::OK, Json(json!({ "id": id, "changed": false, "record": rec }))),
        Ok(true) => {}
    }
    match save(st, rec.clone(), &format!("kyb: confident {what} {id}"), None) {
        Ok(sha) => (StatusCode::OK, Json(json!({ "id": id, "changed": true, "sha": sha, "record": rec }))),
        Err(rep) => rep,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecidedReq {
    by: String,
    verdict: String,
    #[serde(default)]
    chosen: String,
    #[serde(default)]
    quote: String,
    #[serde(default, rename = "ref")]
    reference: String,
}

pub async fn decided(State(st): St, Path(id): Path<String>, Json(r): Json<DecidedReq>) -> Reply {
    update(&st, &id, "decided", |rec| {
        let mut d = Decision {
            by: r.by.trim().to_string(),
            verdict: r.verdict.trim().to_string(),
            chosen: r.chosen.trim().to_string(),
            quote: r.quote.trim().to_string(),
            r#ref: r.reference.trim().to_string(),
            at: String::new(),
        };
        if let Some(old) = &rec.decision {
            d.at = old.at.clone();
            if *old == d && rec.status == confident::STATUS_DECIDED {
                return Ok(false);
            }
        }
        d.at = stamp(now());
        // an abandoned question answered later is still an answer
        rec.status = confident::STATUS_DECIDED.into();
        rec.abandoned_why.clear();
        rec.decision = Some(d);
        Ok(true)
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutcomeReq {
    result: String,
    #[serde(rename = "ref")]
    reference: String,
    #[serde(default)]
    why: String,
}

pub async fn outcome(State(st): St, Path(id): Path<String>, Json(r): Json<OutcomeReq>) -> Reply {
    let by = caller();
    update(&st, &id, "outcome", |rec| {
        rec.outcomes.push(Outcome {
            result: r.result.trim().to_string(),
            r#ref: r.reference.trim().to_string(),
            why: r.why.trim().to_string(),
            at: stamp(now()),
            by,
        });
        Ok(true)
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AbandonReq {
    #[serde(default)]
    why: String,
}

pub async fn abandon(State(st): St, Path(id): Path<String>, Json(r): Json<AbandonReq>) -> Reply {
    update(&st, &id, "abandon", |rec| {
        if rec.status != confident::STATUS_OPEN {
            return Err(format!("'{}' is {}, only an open question can be abandoned", rec.id, rec.status));
        }
        rec.status = confident::STATUS_ABANDONED.into();
        rec.abandoned_why = r.why.trim().to_string();
        Ok(true)
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListQ {
    status: Option<String>,
    workspace: Option<String>,
    limit: Option<usize>,
}

fn with_stale(rec: &Record, at: chrono::DateTime<chrono::Utc>) -> Value {
    let mut v = json!(rec);
    v["stale"] = json!(rec.is_stale(at));
    v
}

pub async fn list(State(st): St, Q(q): Q<ListQ>) -> Reply {
    let status = q.status.as_deref().unwrap_or("").trim();
    if !status.is_empty()
        && ![confident::STATUS_OPEN, confident::STATUS_DECIDED, confident::STATUS_ABANDONED].contains(&status)
    {
        return reject(StatusCode::BAD_REQUEST, "status must be open|decided|abandoned");
    }
    let at = now();
    let all = st.confident.list(status, q.workspace.as_deref().unwrap_or("").trim());
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let items: Vec<Value> = all.iter().take(limit).map(|r| with_stale(r, at)).collect();
    (StatusCode::OK, Json(json!({ "count": all.len(), "items": items })))
}

pub async fn get_one(State(st): St, Path(id): Path<String>) -> Reply {
    match st.confident.get(&id) {
        Some(rec) => (StatusCode::OK, Json(with_stale(&rec, now()))),
        None => reject(StatusCode::NOT_FOUND, format!("no decision record '{id}'")),
    }
}

#[cfg(test)]
#[path = "confident_api_tests.rs"]
mod tests;
