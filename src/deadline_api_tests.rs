/*
 * KYB (Know Your Business) — git-backed knowledge base and incident tracker for AI agent fleets
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/kyb
 * SPDX-License-Identifier: MIT
 */

//! Deadlines over HTTP, end to end through the real router.

use crate::api_tests::{app_with_tmp, call, upsert_body};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::util::ServiceExt;

/// A GET as a caller whose calendar says `today`.
async fn get_on(app: &Router, today: &str, uri: &str) -> (StatusCode, Value) {
    let req = Request::builder().uri(uri).header("x-kyb-today", today).body(Body::empty()).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn task(key: &str, extra: Value) -> Value {
    let mut body = json!({"key": key, "title": format!("Title of {key}"), "body": "what to do"});
    body.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    body
}

async fn file(app: &Router, key: &str, extra: Value) {
    let (st, v) = call(app, "POST", "/tasks", Some(task(key, extra))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
}

fn keys(block: &Value) -> Vec<String> {
    block["items"].as_array().unwrap().iter().map(|i| i["key"].as_str().unwrap().to_string()).collect()
}

#[tokio::test]
async fn a_task_stores_returns_and_lists_its_deadline() {
    let (app, _d, _i) = app_with_tmp();
    file(&app, "task-renew-cert", json!({"due": "2026-03-14", "hard": true, "remind_days": 21})).await;

    let (_, e) = call(&app, "GET", "/knowledge/task-renew-cert?deadlines=false", None).await;
    assert_eq!(e["due"], "2026-03-14");
    assert_eq!(e["hard"], true);
    assert_eq!(e["remind_days"], 21);
    assert!(e.get("expected").is_none(), "unset fields stay out: {e}");

    let (_, l) = get_on(&app, "2026-01-01", "/tasks").await;
    assert_eq!(l["tasks"][0]["due"], "2026-03-14");
    let (_, s) = get_on(&app, "2026-01-01", "/search?q=renew").await;
    assert_eq!(s["hits"][0]["due"], "2026-03-14");

    // a task without a deadline has no deadline fields at all
    file(&app, "task-plain", json!({})).await;
    let (_, e) = call(&app, "GET", "/knowledge/task-plain", None).await;
    assert!(e.get("due").is_none() && e.get("hard").is_none(), "{e}");
}

#[tokio::test]
async fn every_read_response_carries_the_block_only_when_something_is_due() {
    let (app, _d, _i) = app_with_tmp();
    call(&app, "POST", "/knowledge", Some(upsert_body("svc-notes", "unrelated knowledge"))).await;
    file(&app, "task-renew-cert", json!({"due": "2026-03-14", "hard": true})).await;

    // two months out: outside even the 30-day hard window - no block anywhere
    for uri in ["/search?q=unrelated", "/knowledge/svc-notes", "/tasks", "/incidents"] {
        let (st, v) = get_on(&app, "2026-01-10", uri).await;
        assert_eq!(st, StatusCode::OK, "{uri}: {v}");
        assert!(v.get("deadlines").is_none(), "{uri} has a block with nothing due: {v}");
    }

    // three days late: every read says so, whatever it was asked about
    for uri in ["/search?q=unrelated", "/knowledge/svc-notes", "/tasks", "/incidents"] {
        let (_, v) = get_on(&app, "2026-03-17", uri).await;
        let block = &v["deadlines"];
        assert_eq!(block["today"], "2026-03-17", "{uri}: {v}");
        assert_eq!(block["count"], 1);
        assert_eq!(block["overdue"], 1);
        assert_eq!(block["items"][0]["key"], "task-renew-cert");
        assert_eq!(block["items"][0]["days_left"], -3);
        assert_eq!(block["items"][0]["hard"], true);
    }

    // suppressible per call
    for uri in ["/search?q=unrelated&deadlines=false", "/knowledge/svc-notes?deadlines=false", "/tasks?deadlines=false"] {
        let (_, v) = get_on(&app, "2026-03-17", uri).await;
        assert!(v.get("deadlines").is_none(), "{uri}: {v}");
    }
    // a malformed switch is refused like any other parameter, not ignored
    let (st, _) = get_on(&app, "2026-03-17", "/search?q=x&deadlines=nope").await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, v) = get_on(&app, "14/03/2026", "/tasks").await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
}

#[tokio::test]
async fn closing_a_task_removes_it_from_every_deadline_answer_at_once() {
    let (app, _d, _i) = app_with_tmp();
    file(&app, "task-renew-cert", json!({"due": "2026-03-14"})).await;
    let (_, v) = get_on(&app, "2026-03-15", "/tasks").await;
    assert_eq!(v["deadlines"]["count"], 1);

    let (st, v) = call(&app, "POST", "/tasks/task-renew-cert/resolve", Some(json!({"status": "done", "resolution": "renewed"}))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (_, v) = get_on(&app, "2026-03-15", "/tasks").await;
    assert!(v.get("deadlines").is_none(), "{v}");
    let (_, v) = get_on(&app, "2026-03-15", "/deadlines").await;
    assert_eq!(v["count"], 0, "{v}");
    // the deadline stays in the record of the closed task
    let (_, e) = call(&app, "GET", "/knowledge/task-renew-cert", None).await;
    assert_eq!(e["due"], "2026-03-14");
}

#[tokio::test]
async fn rewriting_a_task_keeps_the_deadline_it_does_not_mention() {
    let (app, _d, _i) = app_with_tmp();
    file(&app, "task-renew-cert", json!({"due": "2026-03-14", "hard": true, "waits_on": "the CA's approval"})).await;
    // fixing the body says nothing about the deadline
    file(&app, "task-renew-cert", json!({"body": "what to do, spelled right"})).await;
    let (_, e) = call(&app, "GET", "/knowledge/task-renew-cert", None).await;
    assert_eq!(e["due"], "2026-03-14");
    assert_eq!(e["hard"], true);
    assert_eq!(e["waits_on"], "the CA's approval");
    // naming a field changes it; an empty value clears it
    file(&app, "task-renew-cert", json!({"hard": false, "waits_on": ""})).await;
    let (_, e) = call(&app, "GET", "/knowledge/task-renew-cert", None).await;
    assert!(e.get("hard").is_none() && e.get("waits_on").is_none(), "{e}");
    assert_eq!(e["due"], "2026-03-14");
}

#[tokio::test]
async fn moving_a_due_date_is_a_version_with_a_diff() {
    let (app, _d, _i) = app_with_tmp();
    file(&app, "task-freeze", json!({"due": "2026-03-14", "status": "in_progress", "assignee": "agent-a"})).await;

    // a transition that only moves the date keeps the status and the owner
    let (st, v) = call(&app, "POST", "/tasks/task-freeze/transition", Some(json!({"due": "2026-03-21"}))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (_, e) = call(&app, "GET", "/knowledge/task-freeze", None).await;
    assert_eq!(e["due"], "2026-03-21");
    assert_eq!(e["status"], "in_progress");
    assert_eq!(e["assignee"], "agent-a");

    let (_, d) = call(&app, "GET", "/knowledge/task-freeze/diff", None).await;
    let moved: Vec<&Value> = d["fields"].as_array().unwrap().iter().filter(|f| f["field"] == "due").collect();
    assert_eq!(moved.len(), 1, "{d}");
    assert_eq!(moved[0]["from"], "2026-03-14");
    assert_eq!(moved[0]["to"], "2026-03-21");

    let (_, h) = call(&app, "GET", "/knowledge/task-freeze/history", None).await;
    assert_eq!(h["versions"].as_array().map(Vec::len), Some(2), "{h}");

    // a closed task is not moved silently back to life by a date change
    call(&app, "POST", "/tasks/task-freeze/resolve", Some(json!({"status": "dropped", "resolution": "launch cancelled"}))).await;
    let (st, v) = call(&app, "POST", "/tasks/task-freeze/transition", Some(json!({"due": "2026-04-01"}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    assert!(v["error"].as_str().unwrap().contains("reopen"), "{v}");
}

#[tokio::test]
async fn a_provisional_date_is_surfaced_and_marked_until_the_real_one_is_set() {
    let (app, _d, _i) = app_with_tmp();
    file(&app, "task-answer-quote", json!({"waits_on": "the vendor's quote", "expected": "2026-03-20"})).await;
    let (_, v) = get_on(&app, "2026-03-10", "/tasks").await;
    let item = &v["deadlines"]["items"][0];
    assert_eq!(item["provisional"], true, "{v}");
    assert_eq!(item["due"], "2026-03-20");
    assert_eq!(item["waits_on"], "the vendor's quote");

    call(&app, "POST", "/tasks/task-answer-quote/transition", Some(json!({"due": "2026-03-24", "expected": ""}))).await;
    let (_, v) = get_on(&app, "2026-03-10", "/tasks").await;
    let item = &v["deadlines"]["items"][0];
    assert!(item.get("provisional").is_none(), "{v}");
    assert_eq!(item["due"], "2026-03-24");
}

#[tokio::test]
async fn the_listing_filters_and_groups() {
    let (app, _d, _i) = app_with_tmp();
    file(&app, "task-late", json!({"due": "2026-03-08", "tags": ["billing"]})).await;
    file(&app, "task-soon", json!({"due": "2026-03-12", "hard": true, "assignee": "agent-a", "tags": ["infra"]})).await;
    file(&app, "task-later", json!({"due": "2026-05-01", "tags": ["infra"]})).await;
    file(&app, "task-none", json!({})).await;

    let (st, v) = get_on(&app, "2026-03-10", "/deadlines").await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["count"], 3, "a task without a date is not on the calendar: {v}");
    let rows = v["deadlines"].as_array().unwrap();
    let pairs: Vec<(&str, &str)> =
        rows.iter().map(|r| (r["key"].as_str().unwrap(), r["bucket"].as_str().unwrap())).collect();
    assert_eq!(pairs, [("task-late", "overdue"), ("task-soon", "this_week"), ("task-later", "later")]);

    let only = |v: &Value| -> Vec<String> {
        v["deadlines"].as_array().unwrap().iter().map(|r| r["key"].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(only(&get_on(&app, "2026-03-10", "/deadlines?within=30d").await.1), ["task-late", "task-soon"]);
    assert_eq!(only(&get_on(&app, "2026-03-10", "/deadlines?within=0").await.1), ["task-late"]);
    assert_eq!(only(&get_on(&app, "2026-03-10", "/deadlines?overdue=true").await.1), ["task-late"]);
    assert_eq!(only(&get_on(&app, "2026-03-10", "/deadlines?hard=true").await.1), ["task-soon"]);
    assert_eq!(only(&get_on(&app, "2026-03-10", "/deadlines?tag=infra").await.1), ["task-soon", "task-later"]);
    assert_eq!(only(&get_on(&app, "2026-03-10", "/deadlines?assignee=agent-a").await.1), ["task-soon"]);

    // an empty switch is no filter, as the CLI sends it for a flag not given
    assert_eq!(get_on(&app, "2026-03-10", "/deadlines?within=&overdue=&hard=&tag=&assignee=").await.1["count"], 3);
    assert_eq!(get_on(&app, "2026-03-10", "/deadlines?hard=maybe").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(get_on(&app, "2026-03-10", "/deadlines?within=soon").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(get_on(&app, "2026-03-10", "/deadlines?wihtin=3").await.0, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_tag_scoped_search_shows_that_areas_deadlines_first() {
    let (app, _d, _i) = app_with_tmp();
    file(&app, "task-late", json!({"due": "2026-03-08", "tags": ["billing"]})).await;
    file(&app, "task-soon", json!({"due": "2026-03-12", "tags": ["infra"]})).await;
    let (_, v) = get_on(&app, "2026-03-10", "/search?q=&tag=infra").await;
    assert_eq!(keys(&v["deadlines"]), ["task-soon", "task-late"], "{v}");
    let (_, v) = get_on(&app, "2026-03-10", "/search?q=title").await;
    assert_eq!(keys(&v["deadlines"]), ["task-late", "task-soon"], "{v}");
}

#[tokio::test]
async fn deadline_fields_belong_to_tasks_and_are_validated() {
    let (app, _d, _i) = app_with_tmp();
    let (st, v) = call(&app, "POST", "/tasks", Some(task("task-x", json!({"due": "next friday"})))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    let (st, v) = call(&app, "POST", "/tasks", Some(task("task-x", json!({"hard": true})))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    let (st, v) = call(&app, "POST", "/tasks", Some(task("task-x", json!({"due": "2026-03-14T17:00"})))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "a time without a zone is refused: {v}");
    let (st, v) = call(&app, "POST", "/tasks", Some(task("task-x", json!({"due": "2026-03-14T17:00:00-07:00"})))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
}
