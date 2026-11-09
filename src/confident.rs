/*
 * KYB (Know Your Business) — git-backed knowledge base and incident tracker for AI agent fleets
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/kyb
 * SPDX-License-Identifier: MIT
 */

//! Confident: a decision journal that agents keep while they work.
//!
//! When an agent asks its owner to decide something, it opens a record; when the
//! answer comes it records who decided what; when the result is known (kept,
//! corrected, reverted) anyone appends the outcome. Before the next question the
//! agent asks the journal what was decided before about the same thing.
//!
//! A record is not a knowledge entry: it has its own file (`confident/<id>.json`
//! in the same git repository), its own index and its own routes, and ordinary
//! search never ranks it.

use crate::auth::By;
use crate::model::find_secret;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

pub const DIR: &str = "confident";
pub const PREFIX: &str = "conf-";

pub const STATUS_OPEN: &str = "open";
pub const STATUS_DECIDED: &str = "decided";
pub const STATUS_ABANDONED: &str = "abandoned";

/// Operation classes, in the order the gist is matched against them: a
/// dependency bump names a version too, so it is tried before a release.
pub const KINDS: [&str; 9] = [
    "dependency",
    "release_version",
    "deploy",
    "restart",
    "push_merge",
    "publish_external",
    "destructive",
    "config",
    "other",
];

pub const DECIDERS: [&str; 3] = ["owner", "agent", "review"];

/// How the owner answered. `choose_option` and `approve_with_additions` exist
/// because "do it right here" and "yes, and also..." are neither plain yes nor no.
pub const VERDICTS: [&str; 9] = [
    "approve_plain",
    "approve_with_additions",
    "choose_option",
    "defer",
    "correction",
    "reject",
    "confused",
    "counter_question",
    "redirect",
];

/// Verdicts that say the question or the plan was wrong.
pub const NEGATIVE_VERDICTS: [&str; 3] = ["correction", "reject", "confused"];

pub const RESULTS: [&str; 4] = ["kept", "corrected", "reverted", "dropped"];
pub const NEGATIVE_RESULTS: [&str; 3] = ["corrected", "reverted", "dropped"];

pub const GIST_MAX_CHARS: usize = 160;
const FIELD_MAX_CHARS: usize = 2000;

/// An open record nobody closed for this long is reported as stale.
pub const STALE_SECS: i64 = 48 * 3600;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    pub by: String,
    pub verdict: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub chosen: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub quote: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub r#ref: String,
    pub at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub result: String,
    pub r#ref: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why: String,
    pub at: String,
    #[serde(default)]
    pub by: By,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    /// One English line: action, object, options. The only text that is matched.
    pub gist: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub situation: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub proposed: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tradeoff: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub checked: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub unchecked: String,
    pub status: String,
    pub asked_at: String,
    #[serde(default)]
    pub by: By,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<Decision>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outcomes: Vec<Outcome>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub abandoned_why: String,
}

impl Record {
    pub fn rel_path(&self) -> String {
        format!("{DIR}/{}.json", self.id)
    }

    /// The owner's own answer outranks an agent's or a reviewer's.
    pub fn decided_by_owner(&self) -> bool {
        self.decision.as_ref().is_some_and(|d| d.by == "owner")
    }

    /// The record says the step went wrong somewhere: a negative answer, a
    /// negative outcome, or a question nobody answered.
    pub fn is_negative(&self) -> bool {
        self.status == STATUS_ABANDONED
            || self.decision.as_ref().is_some_and(|d| NEGATIVE_VERDICTS.contains(&d.verdict.as_str()))
            || self.outcomes.iter().any(|o| NEGATIVE_RESULTS.contains(&o.result.as_str()))
    }

    pub fn is_stale(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        self.status == STATUS_OPEN
            && parse_time(&self.asked_at).is_some_and(|t| (now - t).num_seconds() > STALE_SECS)
    }

    pub fn validate(&self) -> Result<()> {
        if !self.id.starts_with(PREFIX) || !crate::model::is_valid_key(&self.id) {
            bail!("id must be a slug starting with '{PREFIX}'");
        }
        validate_gist(&self.gist)?;
        if !KINDS.contains(&self.kind.as_str()) {
            bail!("kind must be one of: {}", KINDS.join("|"));
        }
        if ![STATUS_OPEN, STATUS_DECIDED, STATUS_ABANDONED].contains(&self.status.as_str()) {
            bail!("status must be open|decided|abandoned");
        }
        if !self.proposed.is_empty() && !self.options.is_empty() && !self.options.contains(&self.proposed) {
            bail!("proposed must be one of the options (got '{}')", self.proposed);
        }
        for (name, text) in self.texts() {
            if text.chars().count() > FIELD_MAX_CHARS {
                bail!("{name} is longer than {FIELD_MAX_CHARS} chars: keep pointers, not transcripts");
            }
            if let Some(hit) = find_secret(text) {
                bail!("{name} looks like a secret ({hit}…) — store secrets as pointers, never in a record");
            }
        }
        if let Some(d) = &self.decision {
            if !DECIDERS.contains(&d.by.as_str()) {
                bail!("decision by must be one of: {}", DECIDERS.join("|"));
            }
            if !VERDICTS.contains(&d.verdict.as_str()) {
                bail!("verdict must be one of: {}", VERDICTS.join("|"));
            }
        }
        for o in &self.outcomes {
            if !RESULTS.contains(&o.result.as_str()) {
                bail!("result must be one of: {}", RESULTS.join("|"));
            }
            if o.r#ref.trim().is_empty() {
                bail!("an outcome needs a ref: the commit, PR, task or incident that shows it");
            }
        }
        Ok(())
    }

    /// Every free-text field, named, for the length and secret checks.
    fn texts(&self) -> Vec<(&'static str, &str)> {
        let mut out = vec![
            ("situation", self.situation.as_str()),
            ("proposed", self.proposed.as_str()),
            ("tradeoff", self.tradeoff.as_str()),
            ("checked", self.checked.as_str()),
            ("unchecked", self.unchecked.as_str()),
            ("abandoned_why", self.abandoned_why.as_str()),
        ];
        out.extend(self.target.iter().map(|t| ("target", t.as_str())));
        out.extend(self.options.iter().map(|t| ("options", t.as_str())));
        if let Some(d) = &self.decision {
            out.extend([
                ("chosen", d.chosen.as_str()),
                ("quote", d.quote.as_str()),
                ("ref", d.r#ref.as_str()),
            ]);
        }
        for o in &self.outcomes {
            out.extend([("ref", o.r#ref.as_str()), ("why", o.why.as_str())]);
        }
        out
    }
}

/// The gist is what gets matched, and matching works when every record states
/// it in one language and one shape. A record in another script would only ever
/// find its own kind.
pub fn validate_gist(gist: &str) -> Result<()> {
    let g = gist.trim();
    if g.is_empty() {
        bail!("gist is empty: one English line naming the action, the object and the options");
    }
    if g.contains(['\n', '\r']) {
        bail!("gist must be a single line");
    }
    if g.chars().count() > GIST_MAX_CHARS {
        bail!("gist is longer than {GIST_MAX_CHARS} chars: one line, the decision only");
    }
    if g.chars().any(|c| c.is_alphabetic() && !c.is_ascii()) {
        bail!("gist must be in English: records are matched across sessions by this line, in one language");
    }
    if let Some(hit) = find_secret(g) {
        bail!("gist looks like a secret ({hit}…)");
    }
    Ok(())
}

/// The operation class of a gist, by keyword. Deterministic on purpose: the
/// same words always land in the same class, so a filter on it is predictable.
pub fn derive_kind(gist: &str) -> &'static str {
    let g = format!(" {} ", gist.to_lowercase());
    let has = |words: &[&str]| words.iter().any(|w| g.contains(w));
    let version = regex_lite_version(&g);
    if has(&[" dependency", " dependencies", " dep ", " crate ", " package ", " library ", " lockfile"]) {
        "dependency"
    } else if has(&[" release", " version", " tag ", " semver", " major ", " minor ", " patch "]) || version {
        "release_version"
    } else if has(&[" deploy", " roll out", " rollout", " roll-out", " ship to"]) {
        "deploy"
    } else if has(&[" restart", " reboot", " bounce", " power-cycle", " power cycle"]) {
        "restart"
    } else if has(&[" push", " merge", " commit", " pull request", " pr "]) {
        "push_merge"
    } else if has(&[" post ", " publish", " comment", " reply", " email", " issue", " tweet", " announce"]) {
        "publish_external"
    } else if has(&[
        " delete",
        " remove",
        " drop ",
        " prune",
        " purge",
        " wipe",
        " reset",
        " clean up",
        " cleanup",
    ]) {
        "destructive"
    } else if has(&[" config", " setting", " flag", " env ", " hook", " option "]) {
        "config"
    } else {
        "other"
    }
}

/// `1.2.3` or `v0.4` anywhere in the text.
fn regex_lite_version(g: &str) -> bool {
    let b = g.as_bytes();
    (0..b.len()).any(|i| {
        let start = if b[i] == b'v' { i + 1 } else { i };
        let digits = |mut j: usize| {
            let s = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            (j > s).then_some(j)
        };
        (i == 0 || !b[i - 1].is_ascii_alphanumeric())
            && digits(start)
                .and_then(|j| (j < b.len() && b[j] == b'.').then_some(j + 1))
                .and_then(digits)
                .is_some()
    })
}

/// `conf-<yyyymmdd>-<slug of the gist>-<4 hex>`, within the key length limit.
pub fn new_id(gist: &str, now: chrono::DateTime<chrono::Utc>) -> String {
    let mut slug = String::new();
    for c in gist.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
        if slug.len() >= 48 {
            break;
        }
    }
    let slug = slug.trim_matches('-');
    let mut rnd = [0u8; 2];
    getrandom::getrandom(&mut rnd).expect("os randomness");
    let date = now.format("%Y%m%d");
    if slug.is_empty() {
        format!("{PREFIX}{date}-{:02x}{:02x}", rnd[0], rnd[1])
    } else {
        format!("{PREFIX}{date}-{slug}-{:02x}{:02x}", rnd[0], rnd[1])
    }
}

pub fn parse_time(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|t| t.with_timezone(&chrono::Utc))
}

#[cfg(test)]
#[path = "confident_tests.rs"]
mod tests;
