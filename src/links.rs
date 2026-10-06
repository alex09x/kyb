/*
 * KYB (Know Your Business) — git-backed knowledge base and incident tracker for AI agent fleets
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/kyb
 * SPDX-License-Identifier: MIT
 */

//! Relations between entries, derived from what the entries themselves say.
//!
//! An edge exists only where an author wrote one: an incident's or task's
//! `knowledge` list, a task's `parent_task`, or an internal link in a body
//! (`[[key]]`, `[text](key)`, `[text](#/knowledge/key)`). A shared tag is a
//! facet, not a relation, and a key merely mentioned in prose is not a link.
//! Nothing here is stored: the index is rebuilt from the repository and can
//! always be thrown away.

use crate::model::{self, Entry};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

pub const VIA_KNOWLEDGE: &str = "knowledge";
pub const VIA_PARENT: &str = "parent_task";
pub const VIA_LINK: &str = "link";

fn wiki() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"\[\[([^\[\]]+)\]\]").unwrap())
}

fn inline() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"\[[^\[\]]+\]\(([^()\s]+)\)").unwrap())
}

/// Internal links written in a markdown body. Fenced blocks and inline code
/// are skipped: a key inside a command is an example, not a reference.
pub fn body_links(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |k: &str| {
        if model::is_valid_key(k) && !out.iter().any(|o| o == k) {
            out.push(k.to_string());
        }
    };
    let mut fenced = false;
    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        // drop `inline code` spans
        let mut text = String::with_capacity(line.len());
        let mut code = false;
        for c in line.chars() {
            if c == '`' {
                code = !code;
            } else if !code {
                text.push(c);
            }
        }
        // [[key]]
        for m in wiki().captures_iter(&text) {
            push(m[1].trim());
        }
        // [label](target): the whole shape the page renders as a link, not a stray "]("
        for m in inline().captures_iter(&text) {
            let href = m[1].trim();
            push(href.strip_prefix("#/knowledge/").unwrap_or(href));
        }
    }
    out
}

/// What one entry points at, with how it says so. A target reached by a typed
/// field is not repeated as a body link.
pub fn outgoing(e: &Entry) -> Vec<(String, &'static str)> {
    let mut out: Vec<(String, &'static str)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(e.key.clone());
    let mut add = |k: &str, via: &'static str| {
        if !k.is_empty() && seen.insert(k.to_string()) {
            out.push((k.to_string(), via));
        }
    };
    if !e.parent_task.is_empty() {
        add(&e.parent_task, VIA_PARENT);
    }
    for k in &e.knowledge {
        add(k, VIA_KNOWLEDGE);
    }
    for k in body_links(&e.body) {
        add(&k, VIA_LINK);
    }
    out
}

/// The few fields a list row needs.
#[derive(Clone)]
struct Brief {
    title: String,
    kind: String,
    status: String,
    severity: String,
    priority: String,
    service: String,
    hosts: Vec<String>,
    updated_at: String,
    archived: bool,
}

impl Brief {
    fn of(e: &Entry, archived: bool) -> Brief {
        Brief {
            title: e.title.clone(),
            kind: e.kind.clone(),
            status: e.status.clone(),
            severity: e.severity.clone(),
            priority: e.priority.clone(),
            service: e.service.clone(),
            hosts: e.hosts.clone(),
            updated_at: e.updated_at.clone(),
            archived,
        }
    }

    /// Still asking for attention: an incident not resolved, a task not closed.
    fn live(&self) -> bool {
        if self.archived {
            return false;
        }
        match self.kind.as_str() {
            model::KIND_INCIDENT => self.status != "resolved",
            model::KIND_TASK => !matches!(self.status.as_str(), "done" | "closed" | "dropped" | "resolved"),
            _ => false,
        }
    }

    /// Open incidents first, then open tasks, then knowledge, then what is closed.
    fn rank(&self) -> u8 {
        match (self.live(), self.kind.as_str()) {
            (true, model::KIND_INCIDENT) => 0,
            (true, _) => 1,
            (false, model::KIND_KNOWLEDGE) => 2,
            _ => 3,
        }
    }
}

fn severity_rank(s: &str) -> u8 {
    match s {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        "low" => 3,
        _ => 4,
    }
}

#[derive(Default)]
pub struct LinkIndex {
    briefs: HashMap<String, Brief>,
    out: HashMap<String, Vec<(String, &'static str)>>,
    inc: HashMap<String, Vec<(String, &'static str)>>,
}

impl LinkIndex {
    /// `head` is what is in the tree; `archived` is the last version of every
    /// closed incident and task. A closed incident still says which runbook it used.
    pub fn build(head: &[Entry], archived: &[Entry]) -> LinkIndex {
        let mut ix = LinkIndex::default();
        for (list, is_archived) in [(head, false), (archived, true)] {
            for e in list {
                if ix.briefs.contains_key(&e.key) {
                    continue; // the tree wins over an archived namesake
                }
                ix.briefs.insert(e.key.clone(), Brief::of(e, is_archived));
                let edges = outgoing(e);
                for (to, via) in &edges {
                    ix.inc.entry(to.clone()).or_default().push((e.key.clone(), via));
                }
                ix.out.insert(e.key.clone(), edges);
            }
        }
        ix
    }

    fn row(&self, key: &str, via: &str) -> Value {
        match self.briefs.get(key) {
            None => json!({"key": key, "via": via, "missing": true}),
            Some(b) => json!({
                "key": key, "via": via, "title": b.title, "kind": b.kind, "status": b.status,
                "severity": b.severity, "priority": b.priority, "service": b.service,
                "updated_at": b.updated_at, "archived": b.archived, "live": b.live(),
            }),
        }
    }

    fn rows(&self, edges: &[(String, &'static str)]) -> Vec<Value> {
        let mut sorted: Vec<&(String, &'static str)> = edges.iter().collect();
        sorted.sort_by(|a, b| {
            let (ba, bb) = (self.briefs.get(&a.0), self.briefs.get(&b.0));
            let rank = |x: Option<&Brief>| x.map(|b| b.rank()).unwrap_or(4);
            let when = |x: Option<&Brief>| x.map(|b| b.updated_at.clone()).unwrap_or_default();
            rank(ba).cmp(&rank(bb)).then(when(bb).cmp(&when(ba))).then(a.0.cmp(&b.0))
        });
        sorted.into_iter().map(|(k, via)| self.row(k, via)).collect()
    }

    /// Both directions for one key. An unknown key has no outgoing edges but
    /// may still be pointed at (a dangling parent is legal).
    pub fn related(&self, key: &str) -> Value {
        let empty: Vec<(String, &'static str)> = Vec::new();
        let outgoing = self.rows(self.out.get(key).unwrap_or(&empty));
        let incoming = self.rows(self.inc.get(key).unwrap_or(&empty));
        json!({"key": key, "known": self.briefs.contains_key(key), "outgoing": outgoing, "incoming": incoming})
    }

    /// Every service and host an incident names, with how much is open on it.
    pub fn places(&self) -> Value {
        #[derive(Default)]
        struct Tally {
            open: usize,
            total: usize,
            worst: String,
            last: String,
        }
        let mut services: BTreeMap<String, Tally> = BTreeMap::new();
        let mut hosts: BTreeMap<String, Tally> = BTreeMap::new();
        for b in self.briefs.values().filter(|b| b.kind == model::KIND_INCIDENT) {
            let count = |t: &mut Tally| {
                t.total += 1;
                if b.updated_at > t.last {
                    t.last = b.updated_at.clone();
                }
                if b.live() {
                    t.open += 1;
                    if t.worst.is_empty() || severity_rank(&b.severity) < severity_rank(&t.worst) {
                        t.worst = b.severity.clone();
                    }
                }
            };
            let svc = b.service.trim().to_lowercase();
            if !svc.is_empty() {
                count(services.entry(svc).or_default());
            }
            for h in &b.hosts {
                let h = h.trim().to_lowercase();
                if !h.is_empty() {
                    count(hosts.entry(h).or_default());
                }
            }
        }
        let list = |m: BTreeMap<String, Tally>| -> Vec<Value> {
            let mut v: Vec<(String, Tally)> = m.into_iter().collect();
            v.sort_by(|a, b| {
                (b.1.open > 0)
                    .cmp(&(a.1.open > 0))
                    .then(severity_rank(&a.1.worst).cmp(&severity_rank(&b.1.worst)))
                    .then(b.1.open.cmp(&a.1.open))
                    .then(b.1.last.cmp(&a.1.last))
                    .then(a.0.cmp(&b.0))
            });
            v.into_iter()
                .map(|(name, t)| json!({"name": name, "open": t.open, "total": t.total, "worst": t.worst, "last": t.last}))
                .collect()
        };
        json!({"services": list(services), "hosts": list(hosts)})
    }

    /// What is affected on one service or host, and why each row is here.
    /// Incidents are here because they name the place; a runbook because one of
    /// those incidents cites it; a task because it cites one of either. Nothing
    /// is inferred from tags.
    pub fn impact(&self, by_host: bool, name: &str) -> Value {
        let name = name.trim().to_lowercase();
        let mut incidents: Vec<(String, &'static str)> = self
            .briefs
            .iter()
            .filter(|(_, b)| b.kind == model::KIND_INCIDENT)
            .filter(|(_, b)| {
                if by_host {
                    b.hosts.iter().any(|h| h.trim().to_lowercase() == name)
                } else {
                    b.service.trim().to_lowercase() == name
                }
            })
            .map(|(k, _)| (k.clone(), if by_host { "host" } else { "service" }))
            .collect();
        incidents.sort();
        let incident_keys: HashSet<&String> = incidents.iter().map(|(k, _)| k).collect();

        // runbooks: knowledge the incidents cite, each with the incidents citing it
        let mut cited: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (k, _) in &incidents {
            for (to, _) in self.out.get(k).map(|v| v.as_slice()).unwrap_or(&[]) {
                let is_knowledge = self.briefs.get(to).map(|b| b.kind == model::KIND_KNOWLEDGE).unwrap_or(false);
                if is_knowledge {
                    cited.entry(to.clone()).or_default().push(k.clone());
                }
            }
        }
        // tasks: live tasks pointing at one of those incidents or runbooks
        let mut tasks: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (k, _) in self.briefs.iter().filter(|(_, b)| b.kind == model::KIND_TASK && b.live()) {
            for (to, _) in self.out.get(k).map(|v| v.as_slice()).unwrap_or(&[]) {
                if incident_keys.contains(to) || cited.contains_key(to) {
                    tasks.entry(k.clone()).or_default().push(to.clone());
                }
            }
        }
        let with_because = |m: &BTreeMap<String, Vec<String>>, via: &'static str| -> Vec<Value> {
            let edges: Vec<(String, &'static str)> = m.keys().map(|k| (k.clone(), via)).collect();
            self.rows(&edges)
                .into_iter()
                .map(|mut row| {
                    let key = row["key"].as_str().unwrap_or_default().to_string();
                    row["because"] = json!(m.get(&key).cloned().unwrap_or_default());
                    row
                })
                .collect()
        };
        let rows = self.rows(&incidents);
        let open = rows.iter().filter(|r| r["live"] == json!(true)).count();
        json!({
            "kind": if by_host { "host" } else { "service" }, "name": name,
            "open": open, "incidents": rows,
            "runbooks": with_because(&cited, VIA_KNOWLEDGE),
            "tasks": with_because(&tasks, VIA_LINK),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str, kind: &str) -> Entry {
        Entry { key: key.into(), title: key.to_uppercase(), kind: kind.into(), ..Entry::default() }
    }

    #[test]
    fn body_links_take_only_the_documented_forms() {
        let body = "See [[redis-runbook]] and [the note](pg-failover), also [x](#/knowledge/deploy-flow).\n\
                    Plain mention of other-key is not a link. [ext](https://e.com/a-b) [bad](Not_A_Key)\n\
                    `[[in-code]]` and [[redis-runbook]] again.\n```sh\ncat [[fenced-key]] [f](fenced-two)\n```\n[[after-fence]] [[unclosed";
        assert_eq!(body_links(body), vec!["redis-runbook", "pg-failover", "deploy-flow", "after-fence"]);
        assert!(body_links("").is_empty());
        assert!(body_links("[a](unclosed").is_empty());
        // a stray "](" is not a link: the page does not render one either
        assert!(body_links("Syntax reminder: ](runbook-a) and [](empty-label) and [x] (spaced-key)").is_empty());
        assert_eq!(body_links("[one](a-key)[two](b-key) [[c-key]][[d-key]]"), vec!["c-key", "d-key", "a-key", "b-key"]);
    }

    #[test]
    fn outgoing_prefers_the_typed_field_and_never_points_at_itself() {
        let mut t = entry("task-a", model::KIND_TASK);
        t.parent_task = "task-root".into();
        t.knowledge = vec!["runbook".into(), "runbook".into()];
        t.body = "[[runbook]] [[task-a]] [[other]]".into();
        assert_eq!(
            outgoing(&t),
            vec![("task-root".to_string(), VIA_PARENT), ("runbook".to_string(), VIA_KNOWLEDGE), ("other".to_string(), VIA_LINK)]
        );
    }

    fn fixture() -> LinkIndex {
        let mut runbook = entry("redis-runbook", model::KIND_KNOWLEDGE);
        runbook.updated_at = "2026-01-01".into();
        let mut note = entry("pg-note", model::KIND_KNOWLEDGE);
        note.body = "[[redis-runbook]]".into();
        note.updated_at = "2026-03-01".into();
        let mut open = entry("inc-open", model::KIND_INCIDENT);
        open.status = "open".into();
        open.severity = "high".into();
        open.service = "Redis".into();
        open.hosts = vec!["cache-01".into(), " ".into()];
        open.knowledge = vec!["redis-runbook".into(), "gone-note".into()];
        open.updated_at = "2026-02-01".into();
        let mut crit = entry("inc-crit", model::KIND_INCIDENT);
        crit.status = "mitigated".into();
        crit.severity = "critical".into();
        crit.service = "pg".into();
        crit.updated_at = "2026-02-02".into();
        let mut task = entry("task-fix", model::KIND_TASK);
        task.status = "open".into();
        task.body = "fix [[inc-open]] per [[redis-runbook]]".into();
        let mut child = entry("task-child", model::KIND_TASK);
        child.status = "blocked".into();
        child.parent_task = "task-fix".into();
        let mut done = entry("task-done", model::KIND_TASK);
        done.status = "done".into();
        done.knowledge = vec!["redis-runbook".into()];
        let mut resolved = entry("inc-old", model::KIND_INCIDENT);
        resolved.status = "resolved".into();
        resolved.severity = "low".into();
        resolved.service = "redis".into();
        resolved.hosts = vec!["cache-01".into()];
        resolved.knowledge = vec!["redis-runbook".into()];
        resolved.updated_at = "2025-12-01".into();
        let mut quiet = entry("inc-quiet", model::KIND_INCIDENT);
        quiet.status = "resolved".into();
        quiet.service = "mail".into();
        let shadow = entry("inc-open", model::KIND_INCIDENT); // an archived namesake must not replace the live one
        let nowhere = entry("inc-nowhere", model::KIND_INCIDENT);
        LinkIndex::build(&[runbook, note, open, crit, task, child, done, nowhere], &[resolved, quiet, shadow])
    }

    fn keys(v: &Value) -> Vec<String> {
        v.as_array().unwrap().iter().map(|r| format!("{}:{}", r["key"].as_str().unwrap(), r["via"].as_str().unwrap())).collect()
    }

    #[test]
    fn related_lists_both_directions_open_incidents_first() {
        let ix = fixture();
        let r = ix.related("redis-runbook");
        assert_eq!(r["known"], json!(true));
        assert!(r["outgoing"].as_array().unwrap().is_empty());
        // open incident, open task, knowledge, then the closed ones (newest first)
        assert_eq!(keys(&r["incoming"]), vec!["inc-open:knowledge", "task-fix:link", "pg-note:link", "inc-old:knowledge", "task-done:knowledge"]);
        let inc = &r["incoming"][0];
        assert_eq!((inc["live"].clone(), inc["archived"].clone(), inc["severity"].clone()), (json!(true), json!(false), json!("high")));
        assert_eq!(r["incoming"][3]["archived"], json!(true));

        let o = ix.related("inc-open");
        assert_eq!(keys(&o["outgoing"]), vec!["redis-runbook:knowledge", "gone-note:knowledge"]);
        assert_eq!(o["outgoing"][1]["missing"], json!(true));
        assert_eq!(keys(&o["incoming"]), vec!["task-fix:link"]);

        assert_eq!(keys(&ix.related("task-fix")["incoming"]), vec!["task-child:parent_task"]);
        let unknown = ix.related("gone-note");
        assert_eq!((unknown["known"].clone(), keys(&unknown["incoming"])), (json!(false), vec!["inc-open:knowledge".to_string()]));
        assert!(ix.related("never-heard-of").as_object().unwrap()["incoming"].as_array().unwrap().is_empty());
    }

    #[test]
    fn places_count_what_is_open_and_put_the_worst_first() {
        let p = fixture().places();
        let names = |v: &Value| v.as_array().unwrap().iter().map(|r| format!("{}:{}/{}:{}", r["name"].as_str().unwrap(), r["open"], r["total"], r["worst"].as_str().unwrap())).collect::<Vec<_>>();
        assert_eq!(names(&p["services"]), vec!["pg:1/1:critical", "redis:1/2:high", "mail:0/1:"]);
        assert_eq!(names(&p["hosts"]), vec!["cache-01:1/2:high"]);
        assert_eq!(p["services"][1]["last"], json!("2026-02-01"));
    }

    #[test]
    fn impact_explains_every_row() {
        let ix = fixture();
        let s = ix.impact(false, " REDIS ");
        assert_eq!((s["kind"].clone(), s["name"].clone(), s["open"].clone()), (json!("service"), json!("redis"), json!(1)));
        assert_eq!(keys(&s["incidents"]), vec!["inc-open:service", "inc-old:service"]);
        assert_eq!(keys(&s["runbooks"]), vec!["redis-runbook:knowledge"]);
        assert_eq!(s["runbooks"][0]["because"], json!(["inc-old", "inc-open"]));
        assert_eq!(keys(&s["tasks"]), vec!["task-fix:link"]);
        assert_eq!(s["tasks"][0]["because"], json!(["inc-open", "redis-runbook"]));

        let h = ix.impact(true, "cache-01");
        assert_eq!((h["kind"].clone(), keys(&h["incidents"])), (json!("host"), vec!["inc-open:host".to_string(), "inc-old:host".to_string()]));
        let none = ix.impact(false, "nothing-here");
        assert_eq!((none["open"].clone(), none["incidents"].as_array().unwrap().len(), none["runbooks"].as_array().unwrap().len()), (json!(0), 0, 0));
    }
}
