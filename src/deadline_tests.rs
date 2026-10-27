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

fn day(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

fn clock(now: &str, today: &str) -> Clock {
    let now = DateTime::parse_from_rfc3339(now).unwrap().with_timezone(&Utc);
    Clock::new(now, Some(day(today)))
}

fn due(d: &str) -> Deadline {
    Deadline { due: d.into(), ..Default::default() }
}

fn item_of(key: &str, d: &Deadline, c: &Clock) -> Option<Item> {
    let tags: Vec<String> = vec![];
    item(&Task { key, title: "t", status: "open", assignee: "", tags: &tags, deadline: d }, c)
}

fn tagged(key: &str, d: &Deadline, tags: &[&str], c: &Clock) -> Item {
    let tags: Vec<String> = tags.iter().map(|t| t.to_string()).collect();
    item(&Task { key, title: "t", status: "open", assignee: "", tags: &tags, deadline: d }, c).unwrap()
}

#[test]
fn dates_and_instants_parse_and_a_zoneless_time_does_not() {
    assert_eq!(parse_when("2026-03-14"), Some(When::Day(day("2026-03-14"))));
    assert!(matches!(parse_when("2026-03-14T17:00:00-07:00"), Some(When::At(_))));
    assert!(matches!(parse_when("2026-03-14T17:00-07:00"), Some(When::At(_))));
    assert!(matches!(parse_when("2026-03-14T17:00:00Z"), Some(When::At(_))));
    for bad in ["2026-03-14T17:00", "14/03/2026", "2026-02-30", "soon", ""] {
        assert_eq!(parse_when(bad), None, "{bad}");
    }
}

#[test]
fn overdue_within_window_and_outside_window() {
    let c = clock("2026-03-10T12:00:00Z", "2026-03-10");
    let late = item_of("task-late", &due("2026-03-07"), &c).unwrap();
    assert!(late.overdue);
    assert_eq!(late.days_left, -3);
    assert!(late.surfaced());

    let today = item_of("task-today", &due("2026-03-10"), &c).unwrap();
    assert!(!today.overdue, "due today is not late yet");
    assert_eq!(today.days_left, 0);

    let near = item_of("task-near", &due("2026-03-24"), &c).unwrap();
    assert_eq!(near.days_left, 14);
    assert!(near.surfaced(), "the last day of the default 14-day window is inside it");

    let far = item_of("task-far", &due("2026-03-25"), &c).unwrap();
    assert!(!far.surfaced(), "15 days out is outside the default window");
}

#[test]
fn the_window_is_the_tasks_own_or_the_default_for_its_kind() {
    let c = clock("2026-03-10T12:00:00Z", "2026-03-10");
    let in_25 = "2026-04-04";
    assert!(!item_of("task-soft", &due(in_25), &c).unwrap().surfaced());
    let hard = Deadline { hard: true, ..due(in_25) };
    assert!(item_of("task-hard", &hard, &c).unwrap().surfaced(), "hard deadlines default to 30 days");
    let short = Deadline { remind_days: Some(3), ..due("2026-03-14") };
    assert!(!item_of("task-short", &short, &c).unwrap().surfaced());
    let same_day = Deadline { remind_days: Some(0), ..due("2026-03-10") };
    assert!(item_of("task-day", &same_day, &c).unwrap().surfaced(), "0 means on the day");
}

#[test]
fn expected_stands_in_until_the_real_date_is_known() {
    let c = clock("2026-03-10T12:00:00Z", "2026-03-10");
    let waiting = Deadline {
        waits_on: "the vendor's quote arrives".into(),
        expected: "2026-03-12".into(),
        ..Default::default()
    };
    let i = item_of("task-quote", &waiting, &c).unwrap();
    assert!(i.provisional);
    assert_eq!(i.due, "2026-03-12");
    assert_eq!(i.waits_on, "the vendor's quote arrives");
    assert!(i.surfaced());

    let known = Deadline { due: "2026-03-20".into(), ..waiting };
    let i = item_of("task-quote", &known, &c).unwrap();
    assert!(!i.provisional, "a real due date wins over the provisional one");
    assert_eq!(i.due, "2026-03-20");
    assert_eq!(i.waits_on, "", "the event is history once the real date is known");

    assert!(item_of("task-none", &Deadline::default(), &c).is_none());
}

#[test]
fn a_bare_date_is_judged_by_the_callers_calendar() {
    // 19:00 in Los Angeles on the 14th is already the 15th in UTC; a task due on
    // the 14th is not late for the person whose 14th it still is
    let c = clock("2026-03-15T02:00:00Z", "2026-03-14");
    let i = item_of("task-cert", &due("2026-03-14"), &c).unwrap();
    assert!(!i.overdue);
    assert_eq!(i.days_left, 0);

    let utc = clock("2026-03-15T02:00:00Z", "2026-03-15");
    assert!(item_of("task-cert", &due("2026-03-14"), &utc).unwrap().overdue);
}

#[test]
fn an_instant_is_late_once_it_has_passed_in_any_zone() {
    let deadline = due("2026-03-14T17:00:00-07:00"); // 2026-03-15T00:00Z
    let before = clock("2026-03-14T23:30:00Z", "2026-03-14");
    let i = item_of("task-cutoff", &deadline, &before).unwrap();
    assert!(!i.overdue);
    assert_eq!(i.days_left, 0);

    let after = clock("2026-03-15T00:30:00Z", "2026-03-14");
    let i = item_of("task-cutoff", &deadline, &after).unwrap();
    assert!(i.overdue, "the instant passed, whatever the caller's date says");

    // a caller already on the 15th, while the instant is still ahead, is told
    // "today", never a negative count for something not yet late
    let ahead = clock("2026-03-14T23:30:00Z", "2026-03-15");
    let i = item_of("task-cutoff", &deadline, &ahead).unwrap();
    assert!(!i.overdue);
    assert_eq!(i.days_left, 0);
}

#[test]
fn a_passed_instant_in_a_zone_ahead_is_never_counted_as_days_to_go() {
    // 01:00 on the 15th in Tokyo is 16:00 on the 14th in UTC: at 18:00 UTC it has
    // passed, while a caller in UTC or Los Angeles is still on the 14th
    let deadline = due("2026-03-15T01:00:00+09:00");
    let c = clock("2026-03-14T18:00:00Z", "2026-03-14");
    let i = item_of("task-tokyo", &deadline, &c).unwrap();
    assert!(i.overdue);
    assert_eq!(i.days_left, 0, "late, so never +1");

    // and it sorts ahead of a deadline that is merely due today
    let mut items = vec![item_of("task-a-today", &due("2026-03-14"), &c).unwrap(), i];
    sort(&mut items);
    assert_eq!(items[0].key, "task-tokyo");
}

#[test]
fn without_a_callers_date_the_server_uses_its_own() {
    let now = Utc.with_ymd_and_hms(2026, 3, 15, 2, 0, 0).unwrap();
    let c = Clock::new(now, None);
    assert_eq!(c.today, now.with_timezone(&chrono::Local).date_naive());
}

#[test]
fn the_block_is_capped_ordered_and_absent_when_there_is_nothing() {
    let c = clock("2026-03-10T12:00:00Z", "2026-03-10");
    assert!(block(vec![], &c, &[]).is_none());
    let far = item_of("task-far", &due("2027-01-01"), &c).unwrap();
    assert!(block(vec![far.clone()], &c, &[]).is_none(), "outside every window: no block at all");

    let mut items = vec![far];
    for (i, d) in ["2026-03-12", "2026-03-11", "2026-03-09", "2026-03-15", "2026-03-20", "2026-03-13"]
        .iter()
        .enumerate()
    {
        items.push(item_of(&format!("task-{i}"), &due(d), &c).unwrap());
    }
    let hard = Deadline { hard: true, ..due("2026-03-22") };
    items.push(item_of("task-hard", &hard, &c).unwrap());

    let b = block(items, &c, &[]).unwrap();
    assert_eq!(b["today"], "2026-03-10");
    assert_eq!(b["count"], 7);
    assert_eq!(b["overdue"], 1);
    assert_eq!(b["more"], 2);
    let keys: Vec<&str> = b["items"].as_array().unwrap().iter().map(|i| i["key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["task-hard", "task-2", "task-1", "task-0", "task-5"], "hard first, then by date");
}

#[test]
fn a_scoped_question_shows_its_own_area_first() {
    let c = clock("2026-03-10T12:00:00Z", "2026-03-10");
    let items = vec![
        tagged("task-billing", &due("2026-03-08"), &["billing"], &c),
        tagged("task-cert", &due("2026-03-12"), &["infra", "tls"], &c),
    ];
    let b = block(items.clone(), &c, &["TLS".into()]).unwrap();
    assert_eq!(b["items"][0]["key"], "task-cert");
    assert_eq!(b["items"][1]["key"], "task-billing", "the rest still follows, nothing is hidden");
    let b = block(items, &c, &[]).unwrap();
    assert_eq!(b["items"][0]["key"], "task-billing");
}

#[test]
fn buckets_for_the_listing() {
    let c = clock("2026-03-10T12:00:00Z", "2026-03-10");
    assert_eq!(item_of("a", &due("2026-03-09"), &c).unwrap().bucket(), "overdue");
    assert_eq!(item_of("b", &due("2026-03-17"), &c).unwrap().bucket(), "this_week");
    assert_eq!(item_of("c", &due("2026-03-18"), &c).unwrap().bucket(), "later");
}

#[test]
fn validation() {
    let task = |d: Deadline| d.validate("task", true);
    assert!(task(Deadline::default()).is_ok());
    assert!(task(due("2026-03-14")).is_ok());
    assert!(task(Deadline { hard: true, remind_days: Some(60), ..due("2026-03-14T17:00:00Z") }).is_ok());
    assert!(task(Deadline { waits_on: "quote".into(), expected: "2026-03-14".into(), ..Default::default() }).is_ok());

    let err = |d: Deadline| task(d).unwrap_err().to_string();
    assert!(err(due("next friday")).contains("due must be"));
    assert!(err(Deadline { expected: "2026-13-01".into(), ..Default::default() }).contains("expected must be"));
    assert!(err(Deadline { hard: true, ..Default::default() }).contains("set due, or expected"));
    assert!(err(Deadline { waits_on: "quote".into(), ..Default::default() }).contains("set due, or expected"));
    assert!(err(Deadline { remind_days: Some(400), ..due("2026-03-14") }).contains("at most"));
    assert!(err(Deadline { waits_on: "  ".into(), ..due("2026-03-14") }).contains("blank"));
    // assembled at run time, so the repository's leak gate sees no token shape here
    let token = format!("token ghp_{}", "a1".repeat(18));
    assert!(err(Deadline { waits_on: token, ..due("2026-03-14") }).contains("secret"));

    let knowledge = due("2026-03-14").validate("knowledge", false).unwrap_err().to_string();
    assert!(knowledge.contains("task-only"), "{knowledge}");
}

#[test]
fn a_patch_sets_what_it_names_and_keeps_the_rest() {
    let stored = Deadline { hard: true, remind_days: Some(20), ..due("2026-03-14") };
    let patch = |j: &str| serde_json::from_str::<DeadlinePatch>(j).unwrap().apply(stored.clone());

    assert_eq!(patch("{}"), stored, "nothing named, nothing changes");
    assert_eq!(patch(r#"{"due":"2026-03-21"}"#), Deadline { due: "2026-03-21".into(), ..stored.clone() });
    assert_eq!(patch(r#"{"remind_days":null}"#).remind_days, None, "null clears to the default");
    assert_eq!(patch(r#"{"remind_days":5}"#).remind_days, Some(5));
    assert!(!patch(r#"{"hard":false}"#).hard);
    assert_eq!(patch(r#"{"due":""}"#).due, "", "an empty string clears");
}

#[test]
fn clearing_the_date_takes_what_described_it_along() {
    let stored = Deadline {
        hard: true,
        remind_days: Some(20),
        waits_on: "the CA's approval".into(),
        ..due("2026-03-14")
    };
    let patch = |j: &str| serde_json::from_str::<DeadlinePatch>(j).unwrap().apply(stored.clone());

    for clear in [r#"{"due":""}"#, r#"{"due":null}"#] {
        let d = patch(clear);
        assert_eq!(d, Deadline::default(), "{clear}");
        assert!(d.validate("task", true).is_ok(), "{clear}: clearing is a valid write");
    }
    // a request that clears the date but sets a companion again is refused for it
    let d = patch(r#"{"due":"","hard":true}"#);
    assert!(d.hard);
    assert!(d.validate("task", true).is_err());

    // null clears every field, not only remind_days
    assert!(!patch(r#"{"hard":null}"#).hard);
    assert_eq!(patch(r#"{"waits_on":null}"#).waits_on, "");
    let provisional = Deadline { waits_on: "quote".into(), expected: "2026-03-20".into(), ..Default::default() };
    let d = serde_json::from_str::<DeadlinePatch>(r#"{"expected":null}"#).unwrap().apply(provisional);
    assert_eq!(d, Deadline::default());
}

#[test]
fn diff_fields_read_like_the_other_fields() {
    let d = Deadline { hard: true, remind_days: Some(7), ..due("2026-03-14") };
    let fields = d.diff_fields();
    assert_eq!(fields[0], ("due", "2026-03-14".to_string()));
    assert_eq!(fields[1], ("hard", "true".to_string()));
    assert_eq!(fields[2], ("remind_days", "7".to_string()));
    assert_eq!(Deadline::default().diff_fields()[1], ("hard", String::new()));
}

#[test]
fn the_frontmatter_carries_only_the_fields_that_are_set_and_reads_them_back() {
    use crate::model::Entry;
    let mut e = Entry {
        key: "task-renew-cert".into(),
        title: "Renew the certificate".into(),
        kind: "task".into(),
        status: "open".into(),
        body: "before it expires".into(),
        ..Default::default()
    };
    let plain = e.to_markdown();
    for field in ["due:", "hard:", "remind_days:", "waits_on:", "expected:"] {
        assert!(!plain.contains(field), "a task without a deadline is stored as before: {plain}");
    }
    e.deadline = Deadline { hard: true, remind_days: Some(21), ..due("2026-03-14T17:00:00-07:00") };
    let md = e.to_markdown();
    assert!(md.contains("hard: true") && md.contains("remind_days: 21"), "{md}");
    assert!(!md.contains("waits_on:"), "{md}");
    let back = Entry::from_markdown(&md).unwrap();
    assert_eq!(back.deadline, e.deadline);
    assert!(back.same_content(&e));

    let moved = Entry { deadline: due("2026-03-21"), ..e.clone() };
    assert!(!moved.same_content(&e), "moving the date is a change, so it is a version");
    let (fields, _) = crate::model::diff_entries(&e, &moved);
    let names: Vec<&str> = fields.iter().map(|f| f.field).collect();
    assert_eq!(names, ["due", "hard", "remind_days"], "{fields:?}");
}
