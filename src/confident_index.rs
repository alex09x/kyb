/*
 * KYB (Know Your Business) — git-backed knowledge base and incident tracker for AI agent fleets
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/kyb
 * SPDX-License-Identifier: MIT
 */

//! The in-memory index of decision records and the precedent match.
//!
//! Records are few (one per question to an owner), so the whole set lives in
//! memory and a check is a linear scan: one embedding of the query, then a dot
//! product per record. Each record's vector is computed once, when it is written.

use crate::confident::{self, Record};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::RwLock;

/// A precedent from the same session this recent is the same conversation
/// asking again, not an earlier decision.
pub const SAME_SESSION_SECS: i64 = 3600;
pub const MAX_PRECEDENTS: usize = 3;
/// Below this similarity a record is not offered at all. Vectors and token
/// overlap live on different scales, hence two floors. The vector floor is set
/// for multilingual-e5-small on short English gists: on a probe of 15 decisions
/// phrased three ways, unrelated pairs had p95 0.849 and same-decision pairs
/// p50 0.878. A starting point, to be tuned on real records.
pub const VECTOR_FLOOR: f32 = 0.85;
pub const LEXICAL_FLOOR: f32 = 0.20;
const KIND_BONUS: f32 = 0.05;
const TARGET_BONUS: f32 = 0.05;

/// A record and its gist's vector; None until it is embedded, or without a model.
type Item = (Record, Option<Vec<f32>>);

#[derive(Default)]
pub struct Index {
    items: RwLock<HashMap<String, Item>>,
}

pub struct Query<'a> {
    pub gist: &'a str,
    pub kind: &'a str,
    pub target: &'a [String],
    pub session: &'a str,
    pub question: &'a str,
    pub unchecked: &'a str,
    pub now: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize)]
pub struct Precedent {
    pub score: f32,
    #[serde(flatten)]
    pub record: Record,
    pub stale: bool,
}

#[derive(Debug, Serialize)]
pub struct Hazard {
    pub id: String,
    pub gist: String,
    pub why: String,
}

#[derive(Debug, Serialize)]
pub struct Check {
    pub kind: String,
    pub matched_by: &'static str,
    pub precedents: Vec<Precedent>,
    pub hazards: Vec<Hazard>,
    pub lint: Vec<String>,
}

impl Index {
    /// Every `confident/*.json` under the data directory. A file that does not
    /// parse is reported and skipped, like a broken entry file.
    pub fn load_records(data_dir: &Path) -> Vec<Record> {
        let dir = data_dir.join(confident::DIR);
        let Ok(rd) = std::fs::read_dir(&dir) else { return vec![] };
        let mut out = vec![];
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            match std::fs::read_to_string(&p).map_err(anyhow::Error::from).and_then(|s| {
                let r: Record = serde_json::from_str(&s)?;
                r.validate()?;
                Ok(r)
            }) {
                Ok(r) => out.push(r),
                Err(err) => eprintln!("kyb: skipping {}: {err:#}", p.display()),
            }
        }
        out
    }

    pub fn put(&self, rec: Record, vector: Option<Vec<f32>>) {
        let mut items = self.items.write().expect("confident index lock");
        // a status change keeps the vector: the gist never changes after ask
        let vector = vector.or_else(|| items.get(&rec.id).and_then(|(_, v)| v.clone()));
        items.insert(rec.id.clone(), (rec, vector));
    }

    /// Attach a vector to a record already indexed (the startup embedding pass).
    /// A record written meanwhile already carries its own and keeps it.
    pub fn set_vector(&self, id: &str, vector: Vec<f32>) {
        let mut items = self.items.write().expect("confident index lock");
        if let Some((_, v)) = items.get_mut(id) {
            if v.is_none() {
                *v = Some(vector);
            }
        }
    }

    pub fn get(&self, id: &str) -> Option<Record> {
        self.items.read().expect("confident index lock").get(id).map(|(r, _)| r.clone())
    }

    /// Records with the given status (all when empty), newest first.
    pub fn list(&self, status: &str, workspace: &str) -> Vec<Record> {
        let items = self.items.read().expect("confident index lock");
        let mut out: Vec<Record> = items
            .values()
            .map(|(r, _)| r)
            .filter(|r| status.is_empty() || r.status == status)
            .filter(|r| workspace.is_empty() || r.by.workspace == workspace)
            .cloned()
            .collect();
        out.sort_by(|a, b| b.asked_at.cmp(&a.asked_at));
        out
    }

    pub fn check(&self, q: &Query, qvec: Option<&[f32]>) -> Check {
        let items = self.items.read().expect("confident index lock");
        let use_vectors = qvec.is_some() && items.values().any(|(_, v)| v.is_some());
        let mut scored: Vec<(f32, &Record)> = vec![];
        for (rec, vec) in items.values() {
            if same_conversation(rec, q) {
                continue;
            }
            let sim = match (use_vectors, qvec, vec) {
                (true, Some(a), Some(b)) => crate::embed::cosine(a, b),
                (true, _, _) => continue,
                _ => lexical(q.gist, &rec.gist),
            };
            let floor = if use_vectors { VECTOR_FLOOR } else { LEXICAL_FLOOR };
            // the same kind of step on the same target is a precedent however it
            // was worded: that is what the passport is for. Wording alone has to
            // clear the floor.
            let same_passport = rec.kind == q.kind && targets_overlap(&rec.target, q.target);
            if sim < floor && !same_passport {
                continue;
            }
            let mut score = sim;
            if rec.kind == q.kind {
                score += KIND_BONUS;
            }
            if targets_overlap(&rec.target, q.target) {
                score += TARGET_BONUS;
            }
            scored.push((score, rec));
        }
        // best first; at equal score the owner's own answer comes before an agent's
        scored.sort_by(|a, b| {
            b.0.total_cmp(&a.0).then_with(|| b.1.decided_by_owner().cmp(&a.1.decided_by_owner()))
        });
        let hazards = scored
            .iter()
            .filter(|(_, r)| r.is_negative() && r.kind == q.kind)
            .filter(|(_, r)| q.target.is_empty() || targets_overlap(&r.target, q.target))
            .map(|(_, r)| Hazard { id: r.id.clone(), gist: r.gist.clone(), why: hazard_why(r) })
            .collect();
        let precedents = scored
            .iter()
            .take(MAX_PRECEDENTS)
            .map(|(s, r)| Precedent { score: round(*s), record: (*r).clone(), stale: r.is_stale(q.now) })
            .collect();
        Check {
            kind: q.kind.to_string(),
            matched_by: if use_vectors { "vectors" } else { "words" },
            precedents,
            hazards,
            lint: lint(q),
        }
    }
}

fn same_conversation(rec: &Record, q: &Query) -> bool {
    if q.session.is_empty() || rec.by.session != q.session {
        return false;
    }
    confident::parse_time(&rec.asked_at).is_none_or(|t| (q.now - t).num_seconds().abs() < SAME_SESSION_SECS)
}

fn round(x: f32) -> f32 {
    (x * 1000.0).round() / 1000.0
}

fn norm_target(t: &str) -> String {
    t.trim().trim_matches('/').to_lowercase()
}

/// `kyb` overlaps `kyb/src/main.rs`; `mitts` does not overlap `kyb`.
pub fn targets_overlap(a: &[String], b: &[String]) -> bool {
    a.iter().map(|x| norm_target(x)).filter(|x| !x.is_empty()).any(|x| {
        b.iter()
            .map(|y| norm_target(y))
            .filter(|y| !y.is_empty())
            .any(|y| x == y || y.starts_with(&format!("{x}/")) || x.starts_with(&format!("{y}/")))
    })
}

const STOP: [&str; 16] = [
    "the", "a", "an", "to", "of", "and", "or", "vs", "in", "on", "for", "with", "after", "before", "it", "is",
];

fn tokens(s: &str) -> std::collections::HashSet<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '.')
        .map(|w| w.trim_matches('.'))
        .filter(|w| !w.is_empty() && !STOP.contains(w))
        .map(String::from)
        .collect()
}

/// Token overlap (Jaccard) for a server without an embedding model.
pub fn lexical(a: &str, b: &str) -> f32 {
    let (x, y) = (tokens(a), tokens(b));
    if x.is_empty() || y.is_empty() {
        return 0.0;
    }
    x.intersection(&y).count() as f32 / x.union(&y).count() as f32
}

fn hazard_why(r: &Record) -> String {
    if let Some(o) =
        r.outcomes.iter().rev().find(|o| confident::NEGATIVE_RESULTS.contains(&o.result.as_str()))
    {
        return format!("outcome {} ({})", o.result, o.r#ref);
    }
    if r.status == confident::STATUS_ABANDONED {
        return "the question was abandoned without an answer".into();
    }
    match &r.decision {
        Some(d) if d.quote.is_empty() => format!("answered {} by {}", d.verdict, d.by),
        Some(d) => format!("answered {} by {}: \"{}\"", d.verdict, d.by, d.quote),
        None => String::new(),
    }
}

/// Plain checks on the question itself; they need no history.
fn lint(q: &Query) -> Vec<String> {
    let mut out = vec![];
    if q.question.matches('?').count() > 1 {
        out.push("the question asks several things: ask the owner one decision at a time".into());
    }
    if !q.unchecked.trim().is_empty() {
        out.push(format!(
            "unchecked: {} — if you can check it yourself (code, logs, data), do that before asking",
            q.unchecked.trim()
        ));
    }
    out
}
