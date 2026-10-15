/*
 * KYB (Know Your Business) — git-backed knowledge base and incident tracker for AI agent fleets
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/kyb
 * SPDX-License-Identifier: MIT
 */

//! `GET /export`: the latest version of every key that ever existed, as of one
//! commit, as JSON lines. Search ranks and caps its answer; this one lists, so an
//! audit can tell "the whole base" from "the first hundred hits".
//!
//! The snapshot is a commit, not a moment: every page of one export names the
//! same `at=<sha>`, so writes made while it is read change nothing in it - no key
//! is skipped or repeated between pages. A key is in one of three states there:
//! `active` (its file is in that commit's tree), `archived` (an incident or task
//! closed out of the tree, still findable) or `retracted` (knowledge taken out as
//! wrong). Each removed key carries the content it had just before its removal.
//! Earlier versions are not here; `/knowledge/{key}/history` lists them.

use crate::{entry_json, AppState, St, Q};
use anyhow::Result;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExportQ {
    /// the snapshot: any revision git resolves; HEAD when absent
    at: Option<String>,
    /// cursor: only keys after this one, in byte order
    after: Option<String>,
    /// page size; absent or 0 = every remaining key
    limit: Option<usize>,
}

/// The kind a key is filed under - the same prefix rule the store uses for its
/// directories, so counting needs no file read.
fn kind_of(key: &str) -> &'static str {
    if key.starts_with(crate::model::INCIDENT_PREFIX) {
        "incident"
    } else if key.starts_with(crate::model::TASK_PREFIX) {
        "task"
    } else {
        "knowledge"
    }
}

pub(crate) async fn export(State(st): St, Q(p): Q<ExportQ>) -> Response {
    match build(&st, &p) {
        Ok(Ok(lines)) => {
            ([(header::CONTENT_TYPE, "application/x-ndjson; charset=utf-8")], lines).into_response()
        }
        Ok(Err((code, msg))) => (code, axum::Json(json!({"error": msg}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, axum::Json(json!({"error": format!("{e:#}")})))
            .into_response(),
    }
}

/// One key at the snapshot: its state and, for a removed key, the removal.
struct Row {
    key: String,
    state: &'static str,
    removed: Option<crate::store::VersionInfo>,
    version: Option<crate::store::VersionInfo>,
    versions: usize,
}

fn build(st: &AppState, p: &ExportQ) -> Result<std::result::Result<String, (StatusCode, String)>> {
    let asked = p.at.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let Some((sha, time, _, _)) = st.store.commit_details(asked.unwrap_or("HEAD"))? else {
        if let Some(rev) = asked {
            return Ok(Err((StatusCode::NOT_FOUND, format!("at: no commit '{rev}'"))));
        }
        // an empty canon: a complete export of nothing
        let head = json!({"type": "snapshot", "snapshot": null, "total": 0, "counts": {}});
        let end = json!({"type": "end", "snapshot": null, "returned": 0, "next_after": null, "complete": true});
        return Ok(Ok(format!("{head}\n{end}\n")));
    };
    // the index is read after the snapshot is fixed, so it covers it; versions
    // committed after the snapshot are filtered out by ancestry
    let ix = st.history_index()?;
    // the index describes the history of HEAD; a commit off that line (a side
    // branch, a dangling commit) has versions it does not know, and an export
    // built from it could drop a key and still say complete
    let covered = match ix.head.as_deref() {
        Some(head) => st.store.in_history_of(head, &sha)?,
        None => false,
    };
    if !covered {
        return Ok(Err((
            StatusCode::BAD_REQUEST,
            format!("at: {sha} is not in the history of HEAD; export reads HEAD or one of its ancestors"),
        )));
    }
    let reach = st.store.ancestry(&sha)?;
    let live = st.store.keys_at(&sha)?;
    let keys: BTreeSet<String> = ix.keys().cloned().chain(live.iter().cloned()).collect();

    let mut rows = vec![];
    for key in keys {
        let versions: Vec<_> = ix.versions(&key).into_iter().filter(|v| reach.contains(&v.sha)).collect();
        let content = versions.iter().find(|v| v.change != "deleted").cloned();
        if live.contains(&key) {
            if content.is_none() {
                anyhow::bail!("'{key}' is in the tree at {sha} but history has no version of it");
            }
            rows.push(Row { key, state: "active", removed: None, version: content, versions: versions.len() });
            continue;
        }
        // not in the tree: either not written yet at the snapshot, or removed
        let Some(last) = versions.first() else { continue };
        if last.change != "deleted" {
            anyhow::bail!("history says '{key}' exists at {sha} but its file is not in that tree");
        }
        let state = if kind_of(&key) == "knowledge" { "retracted" } else { "archived" };
        rows.push(Row { key, state, removed: Some(last.clone()), version: content, versions: versions.len() });
    }

    let mut counts: BTreeMap<&str, BTreeMap<&str, usize>> = BTreeMap::new();
    for r in &rows {
        *counts.entry(kind_of(&r.key)).or_default().entry(r.state).or_default() += 1;
    }
    let total = rows.len();
    let after = p.after.as_deref().unwrap_or("");
    let limit = p.limit.filter(|&n| n > 0).unwrap_or(usize::MAX);
    let rest: Vec<&Row> = rows.iter().filter(|r| r.key.as_str() > after).collect();
    let page = &rest[..rest.len().min(limit)];
    let next_after = (rest.len() > page.len()).then(|| page.last().map(|r| r.key.clone())).flatten();

    let mut out = String::new();
    let head = json!({
        "type": "snapshot", "snapshot": sha, "committed_at": iso(time), "total": total,
        "counts": counts, "after": p.after, "limit": p.limit,
    });
    out.push_str(&format!("{head}\n"));
    for r in page {
        // a removed key reads as it was just before the commit that removed it
        let rev = match &r.removed {
            Some(del) => format!("{}^", del.sha),
            None => sha.clone(),
        };
        let Some(e) = st.store.get_at(&r.key, &rev)? else {
            anyhow::bail!("'{}' has no content at {rev}", r.key);
        };
        let mut line: Value = entry_json(&e, r.state == "archived");
        let obj = line.as_object_mut().expect("entry_json is an object");
        obj.insert("type".into(), json!("entry"));
        obj.insert("state".into(), json!(r.state));
        obj.insert("versions".into(), json!(r.versions));
        if let Some(v) = &r.version {
            obj.insert("version".into(), json!({"sha": v.sha, "committed_at": v.committed_at}));
        }
        if let Some(d) = &r.removed {
            obj.insert("removed".into(), json!({"sha": d.sha, "committed_at": d.committed_at, "message": d.message}));
        }
        out.push_str(&format!("{line}\n"));
    }
    let end = json!({
        "type": "end", "snapshot": sha, "returned": page.len(),
        "next_after": next_after, "complete": next_after.is_none(),
    });
    out.push_str(&format!("{end}\n"));
    Ok(Ok(out))
}

fn iso(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0).map(|t| t.to_rfc3339()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use crate::api_tests::{app_with_tmp, call};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::Router;
    use http_body_util::BodyExt;
    use serde_json::{json, Value};
    use tower::util::ServiceExt;

    async fn lines(app: &Router, uri: &str) -> (StatusCode, Vec<Value>) {
        let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let st = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        (st, text.lines().map(|l| serde_json::from_str(l).unwrap()).collect())
    }

    async fn post(app: &Router, uri: &str, body: Value) {
        let (st, v) = call(app, "POST", uri, Some(body)).await;
        assert_eq!(st, StatusCode::OK, "{uri}: {v}");
    }

    fn know(key: &str, body: &str) -> Value {
        json!({"key": key, "title": key, "body": body})
    }

    /// Writes one key of every state: live and retracted knowledge, a live and
    /// an archived incident, a live and an archived task.
    async fn seed(app: &Router) {
        post(app, "/knowledge", know("kept", "still true")).await;
        post(app, "/knowledge", know("wrong", "turned out false")).await;
        assert_eq!(call(app, "DELETE", "/knowledge/wrong", None).await.0, StatusCode::OK);
        for k in ["inc-open", "inc-closed"] {
            post(app, "/incidents", json!({"key": k, "title": k, "service": "s", "severity": "low", "body": "b"})).await;
        }
        post(app, "/incidents/inc-closed/resolve", json!({"resolution": "fixed"})).await;
        for k in ["task-open", "task-done"] {
            post(app, "/tasks", json!({"key": k, "title": k, "body": "b"})).await;
        }
        post(app, "/tasks/task-done/resolve", json!({"resolution": "shipped"})).await;
    }

    fn entries(v: &[Value]) -> Vec<&Value> {
        v.iter().filter(|l| l["type"] == "entry").collect()
    }

    #[tokio::test]
    async fn every_key_in_every_state_once_with_counts() {
        let (app, _d, _i) = app_with_tmp();
        seed(&app).await;
        let (st, v) = lines(&app, "/export").await;
        assert_eq!(st, StatusCode::OK);
        let head = &v[0];
        assert_eq!(head["type"], "snapshot");
        assert_eq!(head["total"], 6);
        assert_eq!(
            head["counts"],
            json!({"incident": {"active": 1, "archived": 1}, "knowledge": {"active": 1, "retracted": 1},
                   "task": {"active": 1, "archived": 1}})
        );
        let got: Vec<(&str, &str)> =
            entries(&v).iter().map(|e| (e["key"].as_str().unwrap(), e["state"].as_str().unwrap())).collect();
        assert_eq!(
            got,
            [("inc-closed", "archived"), ("inc-open", "active"), ("kept", "active"),
             ("task-done", "archived"), ("task-open", "active"), ("wrong", "retracted")]
        );
        let wrong = entries(&v).into_iter().find(|e| e["key"] == "wrong").unwrap();
        assert_eq!(wrong["body"], "turned out false", "a retracted key reads as it was before removal");
        assert!(wrong["removed"]["sha"].is_string());
        let closed = entries(&v).into_iter().find(|e| e["key"] == "inc-closed").unwrap();
        assert_eq!(closed["status"], "resolved");
        assert_eq!(closed["resolution"], "fixed");
        assert_eq!(closed["archived"], true);
        let end = v.last().unwrap();
        assert_eq!((end["type"].as_str(), end["returned"].as_u64(), end["complete"].as_bool()), (Some("end"), Some(6), Some(true)));

        // health counts the live tree; the export's active rows are that same set
        let (_, h) = call(&app, "GET", "/healthz", None).await;
        assert_eq!(h["entries"], 3);
        assert_eq!(head["snapshot"], h["last_commit"]["sha"]);
    }

    /// Pages of one snapshot stay that snapshot while the base changes between
    /// them: nothing written later appears, nothing removed later disappears,
    /// and no key is skipped or repeated.
    #[tokio::test]
    async fn pinned_pages_ignore_writes_made_between_them() {
        let (app, _d, _i) = app_with_tmp();
        seed(&app).await;
        let (_, first) = lines(&app, "/export?limit=2").await;
        let snap = first[0]["snapshot"].as_str().unwrap().to_string();
        let mut keys: Vec<String> = entries(&first).iter().map(|e| e["key"].as_str().unwrap().into()).collect();
        let mut after = first.last().unwrap()["next_after"].as_str().unwrap().to_string();

        post(&app, "/knowledge", know("aaa-new", "written mid-export")).await;
        post(&app, "/knowledge", know("kept", "rewritten mid-export")).await;
        assert_eq!(call(&app, "DELETE", "/knowledge/kept", None).await.0, StatusCode::OK);

        loop {
            let (st, v) = lines(&app, &format!("/export?at={snap}&limit=2&after={after}")).await;
            assert_eq!(st, StatusCode::OK);
            assert_eq!(v[0]["snapshot"], snap.as_str());
            assert_eq!(v[0]["total"], 6, "every page reports the snapshot's total");
            for e in entries(&v) {
                if e["key"] == "kept" {
                    assert_eq!((e["state"].as_str(), e["body"].as_str()), (Some("active"), Some("still true")));
                }
                keys.push(e["key"].as_str().unwrap().into());
            }
            match v.last().unwrap()["next_after"].as_str() {
                Some(k) => after = k.into(),
                None => break,
            }
        }
        assert_eq!(keys, ["inc-closed", "inc-open", "kept", "task-done", "task-open", "wrong"]);

        // the current snapshot does see the later writes
        let (_, now) = lines(&app, "/export").await;
        assert_eq!(now[0]["total"], 7);
        assert_eq!(now[0]["counts"]["knowledge"], json!({"active": 1, "retracted": 2}));
    }

    /// A commit off HEAD's line has history the index does not hold, so it is
    /// refused rather than exported partially.
    #[tokio::test]
    async fn a_commit_outside_head_history_is_refused() {
        let (app, data, _i) = app_with_tmp();
        post(&app, "/knowledge", know("one", "x")).await;
        let repo = git2::Repository::open(data.path()).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let side = repo.commit(None, &sig, &sig, "side", &head.tree().unwrap(), &[&head]).unwrap();
        let (st, v) = call(&app, "GET", &format!("/export?at={side}"), None).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
        let (st, _) = lines(&app, &format!("/export?at={}", head.id())).await;
        assert_eq!(st, StatusCode::OK);
    }

    #[tokio::test]
    async fn bad_revision_and_unknown_parameter_are_refused() {
        let (app, _d, _i) = app_with_tmp();
        let (st, v) = lines(&app, "/export").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!((v[0]["total"].as_u64(), v[1]["complete"].as_bool()), (Some(0), Some(true)));
        post(&app, "/knowledge", know("one", "x")).await;
        assert_eq!(call(&app, "GET", "/export?at=nope", None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(call(&app, "GET", "/export?limt=5", None).await.0, StatusCode::BAD_REQUEST);
    }
}
