/*
 * KYB (Know Your Business) — tag listing and metadata preservation regressions
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/kyb
 * SPDX-License-Identifier: MIT
 */

use super::*;
use crate::api_tests::call;

fn seeded(entries: Vec<model::Entry>) -> (Router, tempfile::TempDir, tempfile::TempDir) {
    let data = tempfile::tempdir().unwrap();
    let idx = tempfile::tempdir().unwrap();
    let store = store::Store::open(data.path()).unwrap();
    for entry in entries {
        let closed = entry.is_closed();
        let key = entry.key.clone();
        store.upsert(entry, "2026-10-20").unwrap();
        if closed {
            store.archive(&key).unwrap();
        }
    }
    let cfg = config::Config {
        data_dir: data.path().to_path_buf(),
        index_dir: idx.path().to_path_buf(),
        audit_path: idx.path().join("audit.jsonl"),
        model_dir: idx.path().join("no-model"),
        addr: String::new(),
    };
    let state = build_state_with(&cfg, auth::Auth::off()).unwrap();
    (build_app(state), data, idx)
}

#[tokio::test]
async fn tag_listing_includes_every_kind_beyond_default_search_limit() {
    let mut entries: Vec<_> = (0..101)
        .map(|i| model::Entry {
            key: format!("note-{i}"),
            title: "Tagged note".into(),
            tags: vec!["hyperliquid".into()],
            ..Default::default()
        })
        .collect();
    entries.push(model::Entry {
        key: "inc-2026-10-20-tagged".into(),
        title: "Tagged incident".into(),
        kind: "incident".into(),
        service: "hl-node".into(),
        severity: "low".into(),
        status: "open".into(),
        tags: vec!["hyperliquid".into()],
        ..Default::default()
    });
    entries.push(model::Entry {
        key: "task-tagged".into(),
        title: "Tagged task".into(),
        kind: "task".into(),
        status: "open".into(),
        tags: vec!["hyperliquid".into()],
        ..Default::default()
    });
    let (app, _data, _idx) = seeded(entries);
    let (status, result) = call(&app, "GET", "/search?tag=hyperliquid&limit=1000", None).await;
    assert_eq!(status, StatusCode::OK);
    let hits = result["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 103);
    assert!(hits.iter().any(|h| h["kind"] == "incident"));
    assert!(hits.iter().any(|h| h["kind"] == "task"));
    let (_, ordinary) = call(&app, "GET", "/search?limit=1000", None).await;
    assert_eq!(ordinary["hits"].as_array().unwrap().len(), 100);
}

#[tokio::test]
async fn archived_task_tag_edit_preserves_closed_timestamp_and_status() {
    for status in ["done", "dropped"] {
        let entry = model::Entry {
            key: "task-archived".into(),
            title: "Closed task".into(),
            kind: "task".into(),
            status: status.into(),
            resolution: "Already finished".into(),
            resolved_at: "2026-09-01T12:00:00Z".into(),
            body: "Original text".into(),
            ..Default::default()
        };
        let (app, _data, _idx) = seeded(vec![entry]);
        let (_, before) = call(&app, "GET", "/knowledge/task-archived", None).await;
        let mut payload = before.clone();
        payload["tags"] = json!(["hyperliquid"]);
        let (status, result) = call(&app, "POST", "/tasks", Some(payload)).await;
        assert_eq!(status, StatusCode::OK, "{result}");
        let (_, after) = call(&app, "GET", "/knowledge/task-archived", None).await;
        assert_eq!(after["tags"], json!(["hyperliquid"]));
        for field in ["resolved_at", "status", "resolution", "body", "archived"] {
            assert_eq!(after[field], before[field], "changed {field}");
        }
    }
}

#[tokio::test]
async fn omitted_incident_and_task_tags_survive_but_explicit_empty_clears() {
    for (kind, key, route) in [
        ("incident", "inc-2026-10-20-preserve", "/incidents"),
        ("task", "task-preserve", "/tasks"),
    ] {
        let entry = model::Entry {
            key: key.into(),
            title: "Tagged entry".into(),
            kind: kind.into(),
            service: if kind == "incident" { "hl-node".into() } else { String::new() },
            severity: if kind == "incident" { "low".into() } else { String::new() },
            status: "open".into(),
            tags: vec!["hyperliquid".into()],
            ..Default::default()
        };
        let (app, _data, _idx) = seeded(vec![entry]);
        let (_, mut payload) = call(&app, "GET", &format!("/knowledge/{key}"), None).await;
        payload.as_object_mut().unwrap().remove("tags");
        payload["body"] = json!("Updated report");
        assert_eq!(call(&app, "POST", route, Some(payload.clone())).await.0, StatusCode::OK);
        let (_, after) = call(&app, "GET", &format!("/knowledge/{key}"), None).await;
        assert_eq!(after["tags"], json!(["hyperliquid"]));
        payload["tags"] = json!([]);
        assert_eq!(call(&app, "POST", route, Some(payload)).await.0, StatusCode::OK);
        let (_, after) = call(&app, "GET", &format!("/knowledge/{key}"), None).await;
        assert_eq!(after["tags"], json!([]));
    }
}
