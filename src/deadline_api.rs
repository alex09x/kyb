/*
 * KYB (Know Your Business) — git-backed knowledge base and incident tracker for AI agent fleets
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/kyb
 * SPDX-License-Identifier: MIT
 */

//! Deadlines over HTTP: the block every read response carries, and
//! `GET /deadlines`, the listing behind `kyb deadlines`.
//!
//! The block is attached by a layer on the read routes rather than by each
//! handler, so a route cannot forget it - and /mcp replays its tool calls
//! through these same routes, so MCP clients get it without a line of their own.

use crate::deadline::{self, Clock, Item, Task};
use crate::{index, model, AppState, Reply, St, Q};
use axum::body::{to_bytes, Body};
use axum::extract::{Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{NaiveDate, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

/// The caller's calendar date, `YYYY-MM-DD`. The CLI sends the date of the
/// machine it runs on; a caller that sends nothing gets the server's date in
/// its own time zone (`TZ`).
pub const TODAY_HEADER: &str = "x-kyb-today";

fn bad_request(msg: String) -> Reply {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": msg })))
}

fn clock(headers: &HeaderMap) -> Result<Clock, Reply> {
    let Some(raw) = headers.get(TODAY_HEADER) else {
        return Ok(Clock::new(Utc::now(), None));
    };
    raw.to_str()
        .ok()
        .and_then(|s| NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok())
        .map(|today| Clock::new(Utc::now(), Some(today)))
        .ok_or_else(|| bad_request(format!("{TODAY_HEADER}: expected YYYY-MM-DD, got {raw:?}")))
}

/// Every live task that has a date, as of `clock`. Closed tasks keep their
/// deadline in the record but are never a reminder: closing one removes it from
/// every deadline answer at once, because this reads the index the close just
/// committed. Neither is a task archived without closing (`kyb rm`): its file has
/// left the tree, so it is not in `kyb tasks` either.
fn open_items(st: &AppState, clock: &Clock) -> anyhow::Result<Vec<Item>> {
    // every task head, however many: a capped listing would drop a deadline
    // without saying so
    let limit = st.index.reader.searcher().num_docs().max(1) as usize;
    let opts = index::SearchOpts { limit, kind: Some(model::KIND_TASK.into()), ..Default::default() };
    let hits = st.index.search("", &opts)?;
    Ok(hits
        .iter()
        .filter(|h| h.is_head && model::TASK_LIVE_STATUSES.contains(&h.status.as_str()))
        .filter_map(|h| {
            let task = Task {
                key: &h.key,
                title: &h.title,
                status: &h.status,
                assignee: &h.assignee,
                tags: &h.tags,
                deadline: &h.deadline,
            };
            deadline::item(&task, clock)
        })
        // a file read per dated task only, after the cheap filters
        .filter(|i| !crate::is_archived(st, &i.key))
        .collect())
}

/// The layer on the read routes: lets the handler answer, then adds
/// `"deadlines": {...}` to a successful JSON object answer when anything is
/// late or inside its reminder window. `?deadlines=false` leaves it off; any
/// other malformed value is the handler's to refuse, like every parameter.
pub async fn attach(State(st): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let params: HashMap<String, String> =
        Query::try_from_uri(req.uri()).map(|Query(p)| p).unwrap_or_default();
    if params.get("deadlines").map(String::as_str) == Some("false") {
        return next.run(req).await;
    }
    let clock = match clock(req.headers()) {
        Ok(c) => c,
        Err(reply) => return reply.into_response(),
    };
    let scope: Vec<String> = params
        .get("tag")
        .map(|t| t.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();

    let resp = next.run(req).await;
    let is_json = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if resp.status() != StatusCode::OK || !is_json {
        return resp;
    }
    let (mut parts, body) = resp.into_parts();
    let bytes = match to_bytes(body, usize::MAX).await {
        Ok(b) => b,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
                .into_response()
        }
    };
    let mut value: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return Response::from_parts(parts, Body::from(bytes)),
    };
    let Some(obj) = value.as_object_mut() else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    // the reminder is a courtesy on top of the answer: if it cannot be computed,
    // the answer still goes out, and the log says why the block is missing
    match open_items(&st, &clock) {
        Ok(items) => match deadline::block(items, &clock, &scope) {
            Some(block) => {
                obj.insert("deadlines".into(), block);
            }
            None => return Response::from_parts(parts, Body::from(bytes)),
        },
        Err(e) => {
            eprintln!("kyb: deadline block skipped: {e:#}");
            return Response::from_parts(parts, Body::from(bytes));
        }
    }
    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(value.to_string()))
}

/// `deny_unknown_fields`, like every query struct: see IncidentsQ in main.rs.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeadlinesQ {
    /// Only what is due within N days (`30` or `30d`); late ones always count.
    within: Option<String>,
    /// Only what is already late. Like every switch here, an empty value - which
    /// is what the CLI sends for a flag not given - means "no filter".
    overdue: Option<String>,
    /// Only hard deadlines.
    hard: Option<String>,
    /// Comma-separated; every tag must match.
    tag: Option<String>,
    assignee: Option<String>,
}

/// A switch: absent or empty = off; anything but a plain yes or no is refused.
fn switch(name: &str, raw: &Option<String>) -> Result<bool, Reply> {
    match raw.as_deref().map(str::trim).unwrap_or("") {
        "" | "false" | "0" | "no" => Ok(false),
        "true" | "1" | "yes" => Ok(true),
        other => Err(bad_request(format!("{name}: expected true or false, got '{other}'"))),
    }
}

fn parse_within(raw: &str) -> Option<i64> {
    let s = raw.trim();
    s.strip_suffix('d').unwrap_or(s).parse::<i64>().ok().filter(|d| *d >= 0)
}

/// `GET /deadlines`: every live task with a date, soonest first, each in its
/// group (overdue / this_week / later). Unlike the block, nothing is capped and
/// no reminder window applies - this is the calendar, asked for on purpose.
pub async fn list(State(st): St, headers: HeaderMap, Q(q): Q<DeadlinesQ>) -> Reply {
    let clock = match clock(&headers) {
        Ok(c) => c,
        Err(reply) => return reply,
    };
    let within = match q.within.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some(raw) => match parse_within(raw) {
            Some(d) => Some(d),
            None => return bad_request(format!("within: expected a number of days like 30 or 30d, got '{raw}'")),
        },
    };
    let (overdue, hard) = match (switch("overdue", &q.overdue), switch("hard", &q.hard)) {
        (Ok(o), Ok(h)) => (o, h),
        (Err(r), _) | (_, Err(r)) => return r,
    };
    let tags: Vec<String> = q
        .tag
        .as_deref()
        .map(|t| t.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    let assignee = q.assignee.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let mut items = match open_items(&st, &clock) {
        Ok(items) => items,
        Err(e) => return crate::err500(e),
    };
    items.retain(|i| {
        within.is_none_or(|d| i.overdue || i.days_left <= d)
            && (!overdue || i.overdue)
            && (!hard || i.hard)
            && i.in_scope(&tags)
            && assignee.is_none_or(|a| i.assignee == a)
    });
    // soonest first here: the hard-first order is for the short block, where
    // only a few lines fit; a calendar reads by date
    items.sort_by(|a, b| {
        a.days_left
            .cmp(&b.days_left)
            .then(b.overdue.cmp(&a.overdue))
            .then(b.hard.cmp(&a.hard))
            .then(a.key.cmp(&b.key))
    });
    let rows: Vec<Value> = items
        .iter()
        .map(|i| {
            let mut row = serde_json::to_value(i).expect("an item always serializes");
            row["bucket"] = json!(i.bucket());
            row
        })
        .collect();
    (
        StatusCode::OK,
        Json(json!({"today": clock.today.to_string(), "count": rows.len(), "deadlines": rows})),
    )
}

#[cfg(test)]
#[path = "deadline_api_tests.rs"]
mod tests;
