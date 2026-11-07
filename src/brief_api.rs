/*
 * KYB (Know Your Business) — git-backed knowledge base and incident tracker for AI agent fleets
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/kyb
 * SPDX-License-Identifier: MIT
 */

//! `GET /brief`, behind `kyb brief`, the `kyb_brief` MCP tool and the session
//! start hook. A read-only aggregation over what the index and the history
//! already hold: no schema of its own, nothing stored.

use crate::brief::{self, ChangeRow, IncidentRow, Scope, WorkRow};
use crate::deadline::{self, Task};
use crate::{index, model, AppState, Reply, St, Q};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

/// A retraction is read back from the commit before it, one file read each.
/// Retractions are rare; this bounds the cost of a window that holds a mass one.
const RETRACTION_READS: usize = 200;

/// `deny_unknown_fields`, like every query struct: see IncidentsQ in main.rs.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BriefQ {
    /// Comma-separated; every tag must match.
    tag: Option<String>,
    service: Option<String>,
    /// How far back "recently" reaches: `7d`, `24h`, a date or an instant.
    since: Option<String>,
    /// Replaces the cap of every section.
    limit: Option<String>,
}

fn bad_request(msg: String) -> Reply {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": msg })))
}

fn instant(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw.trim()).ok().map(|t| t.with_timezone(&Utc))
}

fn live_hits(st: &AppState, kind: &str) -> anyhow::Result<Vec<index::Hit>> {
    // every head of the kind: a capped search would drop an item without saying so
    let limit = st.index.reader.searcher().num_docs().max(1) as usize;
    let opts = index::SearchOpts { limit, kind: Some(kind.into()), ..Default::default() };
    Ok(st.index.search("", &opts)?.into_iter().filter(|h| h.is_head).collect())
}

pub async fn brief(State(st): St, headers: HeaderMap, Q(q): Q<BriefQ>) -> Reply {
    let clock = match crate::deadline_api::clock(&headers) {
        Ok(c) => c,
        Err(reply) => return reply,
    };
    let now = clock.now;
    let since_raw =
        q.since.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or(brief::DEFAULT_SINCE);
    let Some(since) = brief::parse_since(since_raw, now) else {
        return bad_request(format!(
            "since: expected 7d, 24h, a YYYY-MM-DD date or an RFC3339 time in the past, got '{since_raw}'"
        ));
    };
    let limit = match q.limit.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some(raw) => match raw.parse::<usize>() {
            Ok(n) if (1..=brief::MAX_LIMIT).contains(&n) => Some(n),
            _ => {
                return bad_request(format!(
                    "limit: expected 1 to {}, got '{raw}'",
                    brief::MAX_LIMIT
                ))
            }
        },
    };
    let cap = |default: usize| limit.unwrap_or(default);
    let scope = Scope {
        tags: q
            .tag
            .as_deref()
            .map(|t| t.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
            .unwrap_or_default(),
        service: q.service.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(String::from),
    };

    let gathered = (|| -> anyhow::Result<_> {
        Ok((
            live_hits(&st, model::KIND_INCIDENT)?,
            live_hits(&st, model::KIND_TASK)?,
            live_hits(&st, model::KIND_KNOWLEDGE)?,
            st.history_index()?,
        ))
    })();
    let (incidents, tasks, knowledge, history) = match gathered {
        Ok(g) => g,
        Err(e) => return crate::err500(e),
    };

    // Open incidents. Resolving one archives it, so a file read settles what the
    // index still holds a moment after a close.
    let mut incident_rows: Vec<IncidentRow> = incidents
        .iter()
        .filter(|h| {
            matches!(h.status.as_str(), "open" | "mitigated") && scope.admits(&h.tags, &h.service)
        })
        .filter(|h| !crate::is_archived(&st, &h.key))
        .map(|h| {
            let start = [&h.started_at, &h.detected_at, &h.committed_at]
                .into_iter()
                .find(|s| !s.is_empty());
            IncidentRow {
                key: h.key.clone(),
                title: h.title.clone(),
                service: h.service.clone(),
                severity: h.severity.clone(),
                status: h.status.clone(),
                age_days: start.and_then(|s| brief::age_days(s, now)),
            }
        })
        .collect();
    brief::sort_incidents(&mut incident_rows);

    // Live tasks in scope: the work section and the deadlines both come from them.
    let live_tasks: Vec<&index::Hit> = tasks
        .iter()
        .filter(|h| {
            model::TASK_LIVE_STATUSES.contains(&h.status.as_str())
                && scope.admits(&h.tags, &h.service)
        })
        .collect();
    let mut work_rows: Vec<WorkRow> = live_tasks
        .iter()
        .filter(|h| {
            brief::is_active_work(&h.status, &h.priority) && !crate::is_archived(&st, &h.key)
        })
        .map(|h| WorkRow {
            key: h.key.clone(),
            title: h.title.clone(),
            status: h.status.clone(),
            priority: h.priority.clone(),
            assignee: h.assignee.clone(),
            blocked_reason: h.blocked_reason.clone(),
        })
        .collect();
    brief::sort_work(&mut work_rows);
    let deadline_items: Vec<deadline::Item> = live_tasks
        .iter()
        .filter_map(|h| {
            let task = Task {
                key: &h.key,
                title: &h.title,
                status: &h.status,
                assignee: &h.assignee,
                tags: &h.tags,
                deadline: &h.deadline,
            };
            deadline::item(&task, &clock)
        })
        .filter(|i| !crate::is_archived(&st, &i.key))
        .collect();
    // the block's own order and cap: late first, hard first
    let deadlines = match deadline::block(deadline_items, &clock, &[]) {
        None => json!({"count": 0, "overdue": 0, "more": 0, "items": []}),
        Some(mut block) => {
            let want = cap(brief::CAP_DEADLINES);
            if let Some(items) = block["items"].as_array_mut() {
                items.truncate(want);
                let shown = items.len();
                block["more"] = json!(block["count"].as_u64().unwrap_or(0) as usize - shown);
            }
            if let Some(o) = block.as_object_mut() {
                o.remove("today"); // said once, at the top of the brief
            }
            block
        }
    };

    // Recently changed knowledge: the heads written inside the window.
    let mut changed: Vec<ChangeRow> = knowledge
        .iter()
        .filter_map(|h| {
            let at = instant(&h.committed_at)?;
            (at >= since && scope.admits(&h.tags, &h.service)).then(|| ChangeRow {
                key: h.key.clone(),
                title: h.title.clone(),
                at: at.to_rfc3339(),
                author: None,
                by: None,
            })
        })
        .collect();
    brief::sort_changes(&mut changed);

    // Recently retracted knowledge: keys whose newest change removed the file. The
    // entry itself is read from the commit before, for its title and its tags.
    let mut gone: Vec<(&String, &crate::store::VersionInfo, DateTime<Utc>)> = history
        .latest()
        .filter(|(key, v)| {
            v.change == "deleted"
                && !key.starts_with(model::INCIDENT_PREFIX)
                && !key.starts_with(model::TASK_PREFIX)
        })
        .filter_map(|(key, v)| {
            instant(&v.committed_at).filter(|at| *at >= since).map(|at| (key, v, at))
        })
        .collect();
    gone.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(b.0)));
    gone.truncate(RETRACTION_READS);
    let mut retracted: Vec<ChangeRow> = gone
        .into_iter()
        .filter_map(|(key, v, at)| {
            let before = st.store.get_at(key, &format!("{}^", v.sha)).ok().flatten()?;
            scope.admits(&before.tags, &before.service).then(|| ChangeRow {
                key: key.clone(),
                title: before.title,
                at: at.to_rfc3339(),
                author: v.author.clone(),
                by: v.by.clone(),
            })
        })
        .collect();
    brief::sort_changes(&mut retracted);

    // who wrote it, looked up only for the rows that are shown
    let changed_cap = cap(brief::CAP_CHANGED);
    for row in changed.iter_mut().take(changed_cap) {
        if let Some(v) = history.versions(&row.key).into_iter().next() {
            row.author = v.author;
            row.by = v.by;
        }
    }

    let body: Value = json!({
        "today": clock.today.to_string(),
        "since": since.to_rfc3339(),
        "deadlines": deadlines,
        "incidents": brief::section(incident_rows, cap(brief::CAP_INCIDENTS)),
        "work": brief::section(work_rows, cap(brief::CAP_WORK)),
        "retracted": brief::section(retracted, cap(brief::CAP_RETRACTED)),
        "changed": brief::section(changed, changed_cap),
    });
    (StatusCode::OK, Json(body))
}

#[cfg(test)]
#[path = "brief_api_tests.rs"]
mod tests;
