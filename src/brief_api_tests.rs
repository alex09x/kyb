/*
 * KYB (Know Your Business) — git-backed knowledge base and incident tracker for AI agent fleets
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/kyb
 * SPDX-License-Identifier: MIT
 */

//! `GET /brief` end to end through the real router.

use crate::api_tests::{app_with_tmp, call};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::util::ServiceExt;

async fn ok(app: &Router, method: &str, uri: &str, body: Value) {
    let (st, v) = call(app, method, uri, Some(body)).await;
    assert_eq!(st, StatusCode::OK, "{method} {uri}: {v}");
}

async fn incident(
    app: &Router,
    key: &str,
    service: &str,
    severity: &str,
    status: &str,
    tags: &[&str],
) {
    ok(
        app,
        "POST",
        "/incidents",
        json!({"key": key, "title": format!("Title of {key}"),
        "body": "what happened", "service": service, "severity": severity, "status": status,
        "detection": "curl /healthz -> ok", "tags": tags}),
    )
    .await;
}

async fn task(app: &Router, key: &str, extra: Value) {
    let mut body = json!({"key": key, "title": format!("Title of {key}"), "body": "what to do"});
    body.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    ok(app, "POST", "/tasks", body).await;
}

async fn fact(app: &Router, key: &str, tags: &[&str]) {
    ok(
        app,
        "POST",
        "/knowledge",
        json!({"key": key, "title": format!("Title of {key}"),
        "body": "a verified fact", "tags": tags}),
    )
    .await;
}

async fn brief(app: &Router, query: &str) -> Value {
    let (st, v) = call(app, "GET", &format!("/brief{query}"), None).await;
    assert_eq!(st, StatusCode::OK, "{query}: {v}");
    v
}

fn keys(v: &Value, section: &str) -> Vec<String> {
    v[section]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["key"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn the_brief_lists_what_is_live_as_pointers() {
    let (app, _d, _i) = app_with_tmp();
    incident(&app, "inc-edge-down", "edge", "high", "open", &[]).await;
    incident(&app, "inc-db-slow", "db", "critical", "mitigated", &[]).await;
    incident(&app, "inc-old", "db", "low", "open", &[]).await;
    ok(&app, "POST", "/incidents/inc-old/resolve", json!({"resolution": "restarted"})).await;
    task(&app, "task-doing", json!({"status": "in_progress", "assignee": "agent-a"})).await;
    task(&app, "task-stuck", json!({"status": "blocked", "blocked_reason": "vendor answer"})).await;
    task(&app, "task-urgent", json!({"priority": "critical"})).await;
    task(&app, "task-backlog", json!({"priority": "low"})).await;
    task(&app, "task-finished", json!({"status": "in_progress"})).await;
    ok(&app, "POST", "/tasks/task-finished/resolve", json!({"resolution": "shipped"})).await;
    fact(&app, "edge-proxy", &["edge"]).await;
    fact(&app, "old-approach", &["edge"]).await;
    let (st, v) = call(&app, "DELETE", "/knowledge/old-approach", None).await;
    assert_eq!(st, StatusCode::OK, "{v}");

    let b = brief(&app, "").await;
    assert_eq!(
        keys(&b, "incidents"),
        ["inc-db-slow", "inc-edge-down"],
        "critical first, resolved gone: {b}"
    );
    assert_eq!(b["incidents"]["items"][1]["service"], "edge");
    assert_eq!(b["incidents"]["items"][1]["age_days"], 0);
    assert_eq!(
        keys(&b, "work"),
        ["task-doing", "task-stuck", "task-urgent"],
        "no backlog, no closed: {b}"
    );
    assert_eq!(b["work"]["items"][0]["assignee"], "agent-a");
    assert_eq!(b["work"]["items"][1]["blocked_reason"], "vendor answer");
    assert_eq!(keys(&b, "changed"), ["edge-proxy"], "{b}");
    assert_eq!(keys(&b, "retracted"), ["old-approach"], "{b}");
    assert_eq!(b["retracted"]["items"][0]["title"], "Title of old-approach");
    assert!(b["retracted"]["items"][0]["at"].as_str().is_some_and(|s| !s.is_empty()));
    assert_eq!(b["deadlines"]["count"], 0);

    // pointers only: no row carries a body
    for section in ["incidents", "work", "retracted", "changed"] {
        for row in b[section]["items"].as_array().unwrap() {
            assert!(row.get("body").is_none(), "{section} row with a body: {row}");
        }
    }

    // a close takes the item out at once
    ok(&app, "POST", "/incidents/inc-edge-down/resolve", json!({"resolution": "fixed"})).await;
    ok(&app, "POST", "/tasks/task-doing/resolve", json!({"resolution": "done"})).await;
    let b = brief(&app, "").await;
    assert_eq!(keys(&b, "incidents"), ["inc-db-slow"]);
    assert_eq!(keys(&b, "work"), ["task-stuck", "task-urgent"]);

    // written again after the retraction: a change, not a retraction any more
    fact(&app, "old-approach", &["edge"]).await;
    let b = brief(&app, "").await;
    assert!(keys(&b, "retracted").is_empty(), "{b}");
    assert!(keys(&b, "changed").contains(&"old-approach".to_string()), "{b}");
}

#[tokio::test]
async fn tag_and_service_narrow_every_section() {
    let (app, _d, _i) = app_with_tmp();
    incident(&app, "inc-edge", "edge", "high", "open", &["tls"]).await;
    incident(&app, "inc-db", "db", "high", "open", &[]).await;
    task(&app, "task-edge", json!({"status": "in_progress", "tags": ["edge", "tls"]})).await;
    task(&app, "task-db", json!({"status": "in_progress", "tags": ["db"]})).await;
    fact(&app, "edge-notes", &["edge", "tls"]).await;
    fact(&app, "db-notes", &["db"]).await;

    let b = brief(&app, "?service=edge").await;
    assert_eq!(keys(&b, "incidents"), ["inc-edge"], "{b}");
    assert_eq!(keys(&b, "work"), ["task-edge"], "a service is a tag off incidents: {b}");
    assert_eq!(keys(&b, "changed"), ["edge-notes"]);

    let b = brief(&app, "?tag=tls,edge").await;
    assert!(keys(&b, "incidents").is_empty(), "every tag must match: {b}");
    assert_eq!(keys(&b, "work"), ["task-edge"]);
    assert_eq!(keys(&b, "changed"), ["edge-notes"]);
}

#[tokio::test]
async fn every_section_is_capped_and_says_how_many_more() {
    let (app, _d, _i) = app_with_tmp();
    for n in 0..12 {
        task(&app, &format!("task-urgent-{n:02}"), json!({"priority": "high"})).await;
    }
    for n in 0..8 {
        fact(&app, &format!("fact-{n:02}"), &[]).await;
    }
    let b = brief(&app, "").await;
    assert_eq!(b["work"]["items"].as_array().unwrap().len(), 7);
    assert_eq!((b["work"]["count"].clone(), b["work"]["more"].clone()), (json!(12), json!(5)));
    assert_eq!(b["changed"]["items"].as_array().unwrap().len(), 5);
    assert_eq!(b["changed"]["more"], 3);

    let b = brief(&app, "?limit=3").await;
    assert_eq!(b["work"]["items"].as_array().unwrap().len(), 3);
    assert_eq!(b["work"]["more"], 9);
    assert_eq!(b["changed"]["more"], 5);
}

#[tokio::test]
async fn deadlines_ride_along_by_the_callers_calendar() {
    let (app, _d, _i) = app_with_tmp();
    task(&app, "task-renew-cert", json!({"due": "2026-03-08", "hard": true})).await;
    let req = Request::builder()
        .uri("/brief")
        .header("x-kyb-today", "2026-03-10")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let b: Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(b["today"], "2026-03-10");
    assert_eq!(b["deadlines"]["overdue"], 1, "{b}");
    assert_eq!(b["deadlines"]["items"][0]["key"], "task-renew-cert");
    assert_eq!(b["deadlines"]["items"][0]["days_left"], -2);
}

#[tokio::test]
async fn a_malformed_question_is_refused() {
    let (app, _d, _i) = app_with_tmp();
    for query in
        ["?since=soon", "?since=2999-01-01", "?limit=0", "?limit=51", "?limit=many", "?scope=edge"]
    {
        let (st, v) = call(&app, "GET", &format!("/brief{query}"), None).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{query}: {v}");
    }
    // the CLI sends every flag, empty when not given: that is "no filter"
    let b = brief(&app, "?tag=&service=&since=&limit=").await;
    assert!(b["since"].as_str().is_some());
}
