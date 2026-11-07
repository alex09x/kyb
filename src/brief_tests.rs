/*
 * KYB (Know Your Business) — git-backed knowledge base and incident tracker for AI agent fleets
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/kyb
 * SPDX-License-Identifier: MIT
 */

use super::*;
use chrono::TimeZone;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 3, 10, 12, 0, 0).unwrap()
}

#[test]
fn since_takes_days_hours_dates_and_instants() {
    assert_eq!(parse_since("7d", now()), Some(now() - Duration::days(7)));
    assert_eq!(parse_since(" 24h ", now()), Some(now() - Duration::hours(24)));
    assert_eq!(
        parse_since("2026-03-01", now()),
        Some(Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap())
    );
    assert_eq!(
        parse_since("2026-03-09T10:00:00-07:00", now()),
        Some(Utc.with_ymd_and_hms(2026, 3, 9, 17, 0, 0).unwrap())
    );
    for bad in ["", "d", "-3d", "7w", "yesterday", "2026-13-01", "2026-04-01"] {
        assert_eq!(parse_since(bad, now()), None, "{bad:?} must be refused");
    }
}

#[test]
fn scope_needs_every_tag_and_reads_a_service_as_a_tag_off_incidents() {
    let tags = vec!["TLS".to_string(), "edge".to_string()];
    let all = Scope::default();
    assert!(all.admits(&[], ""));
    let tls = Scope { tags: vec!["tls".into()], service: None };
    assert!(tls.admits(&tags, ""));
    assert!(!tls.admits(&["edge".into()], ""));
    let both = Scope { tags: vec!["tls".into(), "db".into()], service: None };
    assert!(!both.admits(&tags, ""), "every tag must match");
    let edge = Scope { tags: vec![], service: Some("edge".into()) };
    assert!(edge.admits(&[], "Edge"), "the incident's own service");
    assert!(edge.admits(&tags, ""), "a task or a fact tagged with the service");
    assert!(!edge.admits(&[], "orders-api"));
}

#[test]
fn a_section_counts_everything_and_says_how_many_did_not_fit() {
    let s = section((0..9).collect::<Vec<_>>(), 5);
    assert_eq!(s, json!({"count": 9, "more": 4, "items": [0, 1, 2, 3, 4]}));
    let s = section(Vec::<u8>::new(), 5);
    assert_eq!(s, json!({"count": 0, "more": 0, "items": []}));
}

fn incident(key: &str, severity: &str, status: &str, age: i64) -> IncidentRow {
    IncidentRow {
        key: key.into(),
        title: key.into(),
        service: String::new(),
        severity: severity.into(),
        status: status.into(),
        age_days: Some(age),
    }
}

#[test]
fn incidents_go_by_severity_then_open_before_mitigated_then_oldest() {
    let mut rows = vec![
        incident("inc-low", "low", "open", 9),
        incident("inc-high-mitigated", "high", "mitigated", 30),
        incident("inc-high-new", "high", "open", 1),
        incident("inc-high-old", "high", "open", 5),
        incident("inc-critical", "critical", "open", 0),
    ];
    sort_incidents(&mut rows);
    let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(
        keys,
        ["inc-critical", "inc-high-old", "inc-high-new", "inc-high-mitigated", "inc-low"]
    );
}

fn work(key: &str, status: &str, priority: &str) -> WorkRow {
    WorkRow {
        key: key.into(),
        title: key.into(),
        status: status.into(),
        priority: priority.into(),
        assignee: String::new(),
        blocked_reason: String::new(),
    }
}

#[test]
fn active_work_is_in_progress_blocked_and_urgent_open_tasks() {
    assert!(is_active_work("in_progress", ""));
    assert!(is_active_work("blocked", "low"));
    assert!(is_active_work("open", "high") && is_active_work("open", "critical"));
    assert!(!is_active_work("open", "medium") && !is_active_work("open", ""));
    assert!(!is_active_work("done", "critical") && !is_active_work("dropped", "high"));

    let mut rows = vec![
        work("task-open-critical", "open", "critical"),
        work("task-blocked", "blocked", ""),
        work("task-doing-low", "in_progress", "low"),
        work("task-doing-high", "in_progress", "high"),
    ];
    sort_work(&mut rows);
    let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(keys, ["task-doing-high", "task-doing-low", "task-blocked", "task-open-critical"]);
}

#[test]
fn age_is_whole_days_and_never_negative() {
    assert_eq!(age_days("2026-03-07T11:00:00+00:00", now()), Some(3));
    assert_eq!(age_days("2026-03-11T00:00:00Z", now()), Some(0));
    assert_eq!(age_days("", now()), None);
}
