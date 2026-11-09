/*
 * KYB (Know Your Business) — git-backed knowledge base and incident tracker for AI agent fleets
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/kyb
 * SPDX-License-Identifier: MIT
 */

//! Decision records and the precedent match, below the HTTP layer.

use super::*;
use crate::auth::By;
use crate::confident_index::{lexical, targets_overlap, Index, Query};
use chrono::{Duration, Utc};

fn rec(id: &str, gist: &str, kind: &str, target: &[&str], session: &str, ago_mins: i64) -> Record {
    Record {
        id: id.into(),
        gist: gist.into(),
        kind: kind.into(),
        target: target.iter().map(|s| s.to_string()).collect(),
        status: STATUS_OPEN.into(),
        asked_at: (Utc::now() - Duration::minutes(ago_mins)).to_rfc3339(),
        by: By { session: session.into(), workspace: "ws".into(), ..Default::default() },
        ..Default::default()
    }
}

fn query<'a>(gist: &'a str, kind: &'a str, target: &'a [String], session: &'a str) -> Query<'a> {
    Query { gist, kind, target, session, question: "", unchecked: "", now: Utc::now() }
}

#[test]
fn kinds_are_derived_from_the_gist() {
    let cases = [
        ("mitts: release patch 0.3.29 vs minor 0.4.0 after a bug fix", "release_version"),
        ("Bump the major version to 2.0 for the breaking change", "release_version"),
        ("Bump the bot-core dependency from 1.12.12 to 1.12.16", "dependency"),
        ("Deploy the KYB server and CLI to the fleet", "deploy"),
        ("Restart the backend while no tasks run", "restart"),
        ("Push 45 local commits to origin/main", "push_merge"),
        ("Post a reply under the article", "publish_external"),
        ("Prune snapshots older than 7 days", "destructive"),
        ("Add a session start hook for the brief", "config"),
        ("Traits or generics for the indicator interface", "other"),
    ];
    for (gist, kind) in cases {
        assert_eq!(derive_kind(gist), kind, "{gist}");
    }
}

#[test]
fn a_version_number_alone_is_a_release() {
    assert_eq!(derive_kind("ship v0.4 today"), "release_version");
    assert_eq!(derive_kind("raise the timeout to 30 seconds"), "other");
}

#[test]
fn the_gist_is_one_english_line() {
    assert!(validate_gist("release patch 0.3.29 vs minor").is_ok());
    assert!(validate_gist("  ").is_err());
    assert!(validate_gist("line one\nline two").is_err());
    assert!(validate_gist("выпустить патч").is_err(), "another script would only ever match itself");
    assert!(validate_gist(&"x".repeat(GIST_MAX_CHARS + 1)).is_err());
}

#[test]
fn a_record_is_validated_field_by_field() {
    let mut r = rec("conf-20261108-x-0a0b", "release patch vs minor", "release_version", &[], "s", 0);
    assert!(r.validate().is_ok());

    r.options = vec!["0.3.29".into(), "0.4.0".into()];
    r.proposed = "1.0.0".into();
    assert!(r.validate().is_err(), "proposed must be one of the options");
    r.proposed = "0.3.29".into();
    assert!(r.validate().is_ok());

    r.decision = Some(Decision { by: "owner".into(), verdict: "maybe".into(), ..Default::default() });
    assert!(r.validate().is_err(), "unknown verdict");
    r.decision = Some(Decision {
        by: "owner".into(),
        verdict: "choose_option".into(),
        chosen: "something nobody listed".into(),
        ..Default::default()
    });
    assert!(r.validate().is_ok(), "the owner may choose an option that was not offered");

    r.outcomes = vec![Outcome { result: "reverted".into(), ..Default::default() }];
    assert!(r.validate().is_err(), "an outcome needs a ref");
    r.outcomes[0].r#ref = "PR #61".into();
    assert!(r.validate().is_ok());

    r.tradeoff = format!("token ghp_{}", "a1".repeat(18));
    assert!(r.validate().is_err(), "secrets never land in a record");
}

#[test]
fn ids_are_valid_keys_with_the_prefix() {
    let id = new_id("mitts: release patch 0.3.29 vs minor 0.4.0", Utc::now());
    assert!(id.starts_with(PREFIX), "{id}");
    assert!(crate::model::is_valid_key(&id), "{id}");
    assert!(id.contains("mitts-release-patch-0-3-29"), "{id}");
    assert_ne!(id, new_id("mitts: release patch 0.3.29 vs minor 0.4.0", Utc::now()), "random suffix");
}

#[test]
fn negative_and_stale_are_read_from_the_record() {
    let mut r = rec("conf-a", "g", "other", &[], "s", 0);
    assert!(!r.is_negative());
    r.decision = Some(Decision { by: "owner".into(), verdict: "correction".into(), ..Default::default() });
    assert!(r.is_negative());
    r.decision = None;
    r.outcomes.push(Outcome { result: "reverted".into(), r#ref: "x".into(), ..Default::default() });
    assert!(r.is_negative());

    let old = rec("conf-b", "g", "other", &[], "s", 49 * 60);
    assert!(old.is_stale(Utc::now()));
    assert!(!rec("conf-c", "g", "other", &[], "s", 60).is_stale(Utc::now()));
}

#[test]
fn denied_workspaces_match_a_whole_name() {
    let deny = vec!["tax".to_string(), " Legal_Vault ".to_string()];
    assert!(denied("tax", &deny));
    assert!(denied("TAX/2025", &deny), "a subdirectory of a denied workspace is denied");
    assert!(denied("legal_vault", &deny));
    assert!(!denied("ridx-syntax", &deny), "a name that merely contains a denied one is not refused");
    assert!(!denied("legal_vault-notes", &deny));
    assert!(!denied("infra", &deny));
    assert!(!denied("", &deny), "an unnamed workspace is not refused by name");
    assert!(!denied("anything", &[]));
}

#[test]
fn targets_overlap_by_path_prefix() {
    let t = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert!(targets_overlap(&t(&["kyb"]), &t(&["kyb/src/main.rs"])));
    assert!(targets_overlap(&t(&["Mitts/"]), &t(&["mitts"])));
    assert!(!targets_overlap(&t(&["mitts"]), &t(&["kyb"])));
    assert!(!targets_overlap(&t(&["kyb"]), &t(&["kybmemory"])), "a prefix of a name is not the name");
}

#[test]
fn lexical_match_ignores_filler_words() {
    assert!(lexical("release patch 0.3.29 vs minor", "release a patch or a minor") > 0.3);
    assert_eq!(lexical("the and of", "a an"), 0.0);
}

#[test]
fn a_check_finds_an_earlier_decision_from_another_session() {
    let ix = Index::default();
    let mut earlier =
        rec("conf-old", "mitts release patch vs minor", "release_version", &["mitts"], "s1", 3 * 24 * 60);
    earlier.status = STATUS_DECIDED.into();
    earlier.decision =
        Some(Decision { by: "owner".into(), verdict: "approve_plain".into(), ..Default::default() });
    ix.put(earlier, None);
    ix.put(rec("conf-other", "deploy kyb to the fleet", "deploy", &["kyb"], "s1", 60), None);

    let target = vec!["mitts".to_string()];
    let c = ix.check(&query("mitts release patch or minor", "release_version", &target, "s2"), None);
    assert_eq!(c.matched_by, "words");
    assert_eq!(c.precedents.len(), 1, "{c:?}");
    assert_eq!(c.precedents[0].record.id, "conf-old");
    assert!(c.hazards.is_empty(), "an approved precedent is not a hazard");
}

#[test]
fn the_same_conversation_is_not_a_precedent() {
    let ix = Index::default();
    ix.put(rec("conf-now", "mitts release patch vs minor", "release_version", &[], "s1", 5), None);
    let none: Vec<String> = vec![];
    assert!(ix
        .check(&query("mitts release patch vs minor", "release_version", &none, "s1"), None)
        .precedents
        .is_empty());
    assert_eq!(
        ix.check(&query("mitts release patch vs minor", "release_version", &none, "s2"), None)
            .precedents
            .len(),
        1
    );

    // the same session two hours later is a new decision, not the same question again
    ix.put(rec("conf-then", "mitts release patch vs minor", "release_version", &[], "s1", 120), None);
    let c = ix.check(&query("mitts release patch vs minor", "release_version", &none, "s1"), None);
    assert_eq!(c.precedents.iter().map(|p| p.record.id.as_str()).collect::<Vec<_>>(), ["conf-then"]);
}

#[test]
fn a_correction_on_the_same_target_is_a_hazard() {
    let ix = Index::default();
    let mut bad =
        rec("conf-bad", "add a session start hook for the kyb brief", "config", &["kyb"], "s1", 24 * 60);
    bad.status = STATUS_DECIDED.into();
    bad.decision = Some(Decision {
        by: "owner".into(),
        verdict: "correction".into(),
        quote: "no hooks for agent context".into(),
        ..Default::default()
    });
    ix.put(bad, None);
    let kyb = vec!["kyb".to_string()];
    let c = ix.check(&query("add a session start hook for the kyb digest", "config", &kyb, "s2"), None);
    assert_eq!(c.hazards.len(), 1, "{c:?}");
    assert!(c.hazards[0].why.contains("correction"), "{}", c.hazards[0].why);
    let mitts = vec!["mitts".to_string()];
    let c = ix.check(&query("add a session start hook for the kyb digest", "config", &mitts, "s2"), None);
    assert!(c.hazards.is_empty(), "another target is not the same hazard");
}

#[test]
fn the_same_step_on_the_same_target_matches_however_it_is_worded() {
    let ix = Index::default();
    let mut reverted = rec(
        "conf-rel",
        "orders-api release patch vs minor",
        "release_version",
        &["orders-api"],
        "s1",
        24 * 60,
    );
    reverted.outcomes.push(Outcome {
        result: "reverted".into(),
        r#ref: "PR 61".into(),
        ..Default::default()
    });
    ix.put(reverted, None);
    let api = vec!["orders-api".to_string()];
    // no word in common with the record: only the passport (kind + target) links them
    let c = ix.check(&query("bump the last digit and ship it", "release_version", &api, "s2"), None);
    assert_eq!(c.precedents.len(), 1, "{c:?}");
    assert_eq!(c.hazards.len(), 1, "{c:?}");
    let other = vec!["billing".to_string()];
    let c = ix.check(&query("bump the last digit and ship it", "release_version", &other, "s2"), None);
    assert!(c.precedents.is_empty() && c.hazards.is_empty(), "{c:?}");
}

#[test]
fn lint_flags_bundled_and_unchecked_questions() {
    let ix = Index::default();
    let none: Vec<String> = vec![];
    let mut q = query("release and deploy", "release_version", &none, "s");
    q.question = "Release it? And deploy to all nine machines?";
    q.unchecked = "not tried on the arm node";
    let c = ix.check(&q, None);
    assert_eq!(c.lint.len(), 2, "{:?}", c.lint);
}
