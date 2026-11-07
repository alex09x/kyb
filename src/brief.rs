/*
 * KYB (Know Your Business) — git-backed knowledge base and incident tracker for AI agent fleets
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/kyb
 * SPDX-License-Identifier: MIT
 */

//! `kyb brief`: what is live in the base right now, as pointers. A session
//! start hook injects it into an agent's context, so an agent that never thinks
//! to query still learns about the open incident on the service it is about to
//! touch, or the decision retracted yesterday.
//!
//! Every row is a key, a title and one fact, never a body. Every section is
//! capped, and its `more` says how many did not fit, so the whole default brief
//! stays a few dozen lines. The rules here are pure; brief_api.rs gathers the
//! rows from the index and the history.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::Serialize;
use serde_json::{json, Value};

pub const CAP_DEADLINES: usize = 5;
pub const CAP_INCIDENTS: usize = 5;
pub const CAP_WORK: usize = 7;
pub const CAP_RETRACTED: usize = 5;
pub const CAP_CHANGED: usize = 5;
/// `--limit` replaces every cap; more than this is not a brief any longer.
pub const MAX_LIMIT: usize = 50;
pub const DEFAULT_SINCE: &str = "7d";

/// `7d`, `24h`, a date (its start, UTC) or an RFC3339 instant. None for anything
/// else, and for a moment in the future.
pub fn parse_since(raw: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let s = raw.trim();
    let ago = |n: &str, unit: fn(i64) -> Duration| {
        n.parse::<i64>().ok().filter(|n| *n >= 0).map(|n| now - unit(n))
    };
    let at = if let Some(n) = s.strip_suffix('d') {
        ago(n, Duration::days)
    } else if let Some(n) = s.strip_suffix('h') {
        ago(n, Duration::hours)
    } else if let Ok(day) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        day.and_hms_opt(0, 0, 0).map(|t| t.and_utc())
    } else {
        DateTime::parse_from_rfc3339(s).ok().map(|t| t.with_timezone(&Utc))
    };
    at.filter(|t| *t <= now)
}

/// What `--tag` and `--service` narrow every section to. Every tag must match;
/// tags compare case-insensitively, as the index stores them. Only incidents
/// carry a service field, so for the other kinds an entry belongs to service X
/// when it carries X as a tag.
#[derive(Debug, Default, Clone)]
pub struct Scope {
    pub tags: Vec<String>,
    pub service: Option<String>,
}

impl Scope {
    pub fn admits(&self, tags: &[String], service: &str) -> bool {
        let has = |want: &str| tags.iter().any(|t| t.eq_ignore_ascii_case(want));
        self.tags.iter().all(|t| has(t))
            && self.service.as_deref().is_none_or(|s| service.eq_ignore_ascii_case(s) || has(s))
    }
}

/// `{count, more, items}`: everything that qualified, how many did not fit, and
/// the rows that did.
pub fn section<T: Serialize>(mut rows: Vec<T>, cap: usize) -> Value {
    let count = rows.len();
    rows.truncate(cap);
    json!({"count": count, "more": count - rows.len(), "items": rows})
}

fn rank(order: &[&str], value: &str) -> usize {
    order.iter().position(|x| *x == value).unwrap_or(order.len())
}

/// Highest first; an unranked value goes last.
pub fn severity_rank(severity: &str) -> usize {
    rank(&["critical", "high", "medium", "low"], severity)
}

pub fn priority_rank(priority: &str) -> usize {
    rank(&["critical", "high", "medium", "low"], priority)
}

/// Whole days since an RFC3339 instant; None when it is not one.
pub fn age_days(since: &str, now: DateTime<Utc>) -> Option<i64> {
    let t = DateTime::parse_from_rfc3339(since.trim()).ok()?;
    Some((now - t.with_timezone(&Utc)).num_days().max(0))
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct IncidentRow {
    pub key: String,
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub service: String,
    pub severity: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_days: Option<i64>,
}

/// Highest severity first; an open report before a mitigated one; the oldest
/// first among equals, since it has waited longest.
pub fn sort_incidents(rows: &mut [IncidentRow]) {
    rows.sort_by(|a, b| {
        severity_rank(&a.severity)
            .cmp(&severity_rank(&b.severity))
            .then(
                rank(&["open", "mitigated"], &a.status)
                    .cmp(&rank(&["open", "mitigated"], &b.status)),
            )
            .then(b.age_days.cmp(&a.age_days))
            .then(a.key.cmp(&b.key))
    });
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WorkRow {
    pub key: String,
    pub title: String,
    pub status: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub priority: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub assignee: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub blocked_reason: String,
}

/// Active work: what someone is on (in_progress), what is stuck (blocked), and
/// open tasks ranked high or critical. An ordinary open task is backlog, not news.
pub fn is_active_work(status: &str, priority: &str) -> bool {
    matches!(status, "in_progress" | "blocked")
        || (status == "open" && matches!(priority, "high" | "critical"))
}

pub fn sort_work(rows: &mut [WorkRow]) {
    rows.sort_by(|a, b| {
        rank(&["in_progress", "blocked", "open"], &a.status)
            .cmp(&rank(&["in_progress", "blocked", "open"], &b.status))
            .then(priority_rank(&a.priority).cmp(&priority_rank(&b.priority)))
            .then(a.key.cmp(&b.key))
    });
}

/// A knowledge entry written or retracted inside the window.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ChangeRow {
    pub key: String,
    pub title: String,
    /// The commit time, RFC3339.
    pub at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<crate::auth::By>,
}

/// Newest first.
pub fn sort_changes(rows: &mut [ChangeRow]) {
    rows.sort_by(|a, b| b.at.cmp(&a.at).then(a.key.cmp(&b.key)));
}

#[cfg(test)]
#[path = "brief_tests.rs"]
mod tests;
