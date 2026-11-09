/*
 * KYB (Know Your Business) — git-backed knowledge base and incident tracker for AI agent fleets
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/kyb
 * SPDX-License-Identifier: MIT
 */

//! `/confident` end to end through the real router.

use crate::{auth, build_app, build_state_with, config};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::util::ServiceExt;

struct App {
    router: Router,
    data: tempfile::TempDir,
    _idx: tempfile::TempDir,
}

fn cfg(data: &std::path::Path, idx: &std::path::Path) -> config::Config {
    config::Config {
        data_dir: data.to_path_buf(),
        index_dir: idx.to_path_buf(),
        audit_path: idx.join("audit.jsonl"),
        model_dir: idx.join("no-model"),
        addr: String::new(),
    }
}

fn app(deny: &[&str]) -> App {
    let data = tempfile::tempdir().unwrap();
    let idx = tempfile::tempdir().unwrap();
    let mut state = build_state_with(&cfg(data.path(), idx.path()), auth::Auth::off()).unwrap();
    std::sync::Arc::get_mut(&mut state).expect("fresh state").confident_deny =
        deny.iter().map(|s| s.to_string()).collect();
    App { router: build_app(state), data, _idx: idx }
}

/// A request as one agent session in one workspace would send it.
async fn call(
    app: &Router,
    session: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let b = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-kyb-agent", "claude")
        .header("x-kyb-session", session)
        .header("x-kyb-workspace", "infra");
    let req = match body {
        Some(v) => b.header("content-type", "application/json").body(Body::from(v.to_string())).unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let resp = app.clone().oneshot(req).await.unwrap();
    let st = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (st, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn ask_body() -> Value {
    json!({
        "gist": "mitts: release patch 0.3.29 vs minor 0.4.0 after the -q fix",
        "target": ["mitts"],
        "situation": "a bug fix is merged and CI is green",
        "options": ["0.3.29", "0.4.0"],
        "proposed": "0.3.29",
        "tradeoff": "a minor would signal new features that are not there",
        "checked": "tests and CI pass",
        "unchecked": "not tried on the arm build node",
        "question": "Release 0.3.29? And roll it out to every machine?"
    })
}

const CHECK: &str = "/confident/check?gist=mitts%20release%20patch%20or%20minor&target=mitts";

#[tokio::test]
async fn ask_decide_and_find_it_again_from_another_session() {
    let a = app(&[]);
    let r = &a.router;

    let (st, v) = call(r, "s1", "GET", CHECK, None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["precedents"], json!([]), "an empty journal has no precedents");
    assert_eq!(v["kind"], "release_version");

    let (st, v) = call(r, "s1", "POST", "/confident", Some(ask_body())).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let id = v["id"].as_str().unwrap().to_string();
    assert_eq!(v["check"]["lint"].as_array().unwrap().len(), 2, "bundled question + unchecked fact: {v}");

    let (_, v) = call(r, "s1", "GET", &format!("/confident/{id}"), None).await;
    assert_eq!(v["status"], "open");
    assert_eq!(v["by"]["session"], "s1");
    assert_eq!(v["stale"], false);
    let (_, v) = call(r, "s1", "GET", "/confident?status=open", None).await;
    assert_eq!(v["count"], 1);

    // the asking session itself does not find its own fresh question
    let (_, v) = call(r, "s1", "GET", CHECK, None).await;
    assert_eq!(v["precedents"], json!([]));

    let (st, v) = call(
        r,
        "s1",
        "POST",
        &format!("/confident/{id}/decided"),
        Some(json!({"by": "owner", "verdict": "approve_plain", "quote": "ship it", "ref": "mitts@v0.3.29"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["record"]["status"], "decided");
    assert_eq!(v["changed"], true);

    // a retry of the same answer commits nothing
    let (st, v) = call(
        r,
        "s1",
        "POST",
        &format!("/confident/{id}/decided"),
        Some(json!({"by": "owner", "verdict": "approve_plain", "quote": "ship it", "ref": "mitts@v0.3.29"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["changed"], false, "{v}");

    let (_, v) = call(r, "s2", "GET", CHECK, None).await;
    assert_eq!(v["precedents"][0]["id"], id.as_str(), "{v}");
    assert_eq!(v["precedents"][0]["decision"]["verdict"], "approve_plain");
    assert_eq!(v["hazards"], json!([]));

    // an outcome without its evidence is refused; with it, a revert becomes a hazard
    let (st, _) = call(
        r,
        "s2",
        "POST",
        &format!("/confident/{id}/outcome"),
        Some(json!({"result": "reverted", "ref": " "})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, v) = call(
        r,
        "s2",
        "POST",
        &format!("/confident/{id}/outcome"),
        Some(json!({"result": "reverted", "ref": "PR #61", "why": "broke the x86 build"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (_, v) = call(r, "s3", "GET", CHECK, None).await;
    assert_eq!(v["hazards"][0]["id"], id.as_str(), "{v}");
    assert!(v["hazards"][0]["why"].as_str().unwrap().contains("reverted"));
}

#[tokio::test]
async fn records_live_in_git_survive_a_restart_and_stay_out_of_search() {
    let a = app(&[]);
    let (_, v) = call(&a.router, "s1", "POST", "/confident", Some(ask_body())).await;
    let id = v["id"].as_str().unwrap().to_string();
    assert!(a.data.path().join(format!("confident/{id}.json")).is_file());

    let (_, v) = call(&a.router, "s1", "GET", "/search?q=mitts%20release%20patch", None).await;
    assert_eq!(v["count"], 0, "ordinary search never ranks decision records: {v}");

    let idx = tempfile::tempdir().unwrap();
    let again = build_app(build_state_with(&cfg(a.data.path(), idx.path()), auth::Auth::off()).unwrap());
    let (st, v) = call(&again, "s2", "GET", &format!("/confident/{id}"), None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["gist"], ask_body()["gist"]);
}

#[tokio::test]
async fn only_an_open_question_can_be_abandoned() {
    let a = app(&[]);
    let r = &a.router;
    let (_, v) = call(r, "s1", "POST", "/confident", Some(ask_body())).await;
    let id = v["id"].as_str().unwrap().to_string();
    let (st, v) =
        call(r, "s1", "POST", &format!("/confident/{id}/abandon"), Some(json!({"why": "session closed"})))
            .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["record"]["status"], "abandoned");
    let (st, _) = call(r, "s1", "POST", &format!("/confident/{id}/abandon"), Some(json!({}))).await;
    assert_eq!(st, StatusCode::CONFLICT);

    // the owner answering later still counts
    let (st, v) = call(
        r,
        "s1",
        "POST",
        &format!("/confident/{id}/decided"),
        Some(json!({"by": "owner", "verdict": "defer"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["record"]["status"], "decided");
    assert_eq!(v["record"].get("abandoned_why"), None);
}

#[tokio::test]
async fn bad_input_is_refused_with_a_reason() {
    let a = app(&[]);
    let r = &a.router;
    let (st, v) =
        call(r, "s1", "GET", "/confident/check?gist=%D0%B2%D1%8B%D0%BF%D1%83%D1%81%D0%BA", None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(v["error"].as_str().unwrap().contains("English"), "{v}");
    let (st, _) = call(r, "s1", "GET", "/confident/check?gist=x&kind=whatever", None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call(r, "s1", "GET", "/confident/conf-nope", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = call(
        r,
        "s1",
        "POST",
        "/confident/conf-nope/decided",
        Some(json!({"by": "owner", "verdict": "reject"})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let mut body = ask_body();
    body["proposed"] = json!("1.0.0");
    let (st, _) = call(r, "s1", "POST", "/confident", Some(body)).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "proposed must be one of the options");
}

#[tokio::test]
async fn a_denied_workspace_records_nothing() {
    let a = app(&["infra"]);
    let (st, v) = call(&a.router, "s1", "POST", "/confident", Some(ask_body())).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    assert!(!a.data.path().join("confident").exists());
    // reading the journal is not recording
    let (st, _) = call(&a.router, "s1", "GET", CHECK, None).await;
    assert_eq!(st, StatusCode::OK);
}
