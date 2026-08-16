use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

pub const KIND_KNOWLEDGE: &str = "knowledge";
pub const KIND_INCIDENT: &str = "incident";
pub const KIND_TASK: &str = "task";
/// Incident/task keys carry a prefix so the flat key space stays readable and
/// a knowledge entry can never collide with a report or a task.
pub const INCIDENT_PREFIX: &str = "inc-";
pub const TASK_PREFIX: &str = "task-";

pub const SEVERITIES: [&str; 4] = ["low", "medium", "high", "critical"];
pub const STATUSES: [&str; 3] = ["open", "mitigated", "resolved"];
/// Task lifecycle, listing order included: the live states first, then the two
/// terminal ones. Only the terminal states close (and archive) a task.
pub const TASK_STATUSES: [&str; 5] = ["open", "in_progress", "blocked", "done", "dropped"];
/// The statuses a task can sit in while it is still work: `kyb tasks` and the
/// health counter must see all three, not just "open".
pub const TASK_LIVE_STATUSES: [&str; 3] = ["open", "in_progress", "blocked"];
pub const TASK_TERMINAL_STATUSES: [&str; 2] = ["done", "dropped"];
/// Task-only, optional: an empty priority means "not ranked", which is the
/// default and stays the default — nothing infers one.
pub const PRIORITIES: [&str; 4] = ["low", "medium", "high", "critical"];
pub const STATUS_BLOCKED: &str = "blocked";
/// Task-only, optional: who holds the task right now. A short label, not an
/// identity system — bounded so a whole prose paragraph cannot squat in it.
pub const ASSIGNEE_MAX_LEN: usize = 80;

fn default_kind() -> String {
    KIND_KNOWLEDGE.to_string()
}

fn is_default_kind(k: &str) -> bool {
    k == KIND_KNOWLEDGE
}

/// One poisoned/affected slice of the world: what (`scope` — an exchange, a
/// table, a host) and when (`from`/`to`, UTC). Machine-readable so a backtest
/// can exclude the windows without parsing prose.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Window {
    pub scope: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Meta {
    key: String,
    title: String,
    #[serde(default = "default_kind", skip_serializing_if = "is_default_kind")]
    kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    service: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    hosts: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    severity: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    status: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    priority: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    blocked_reason: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    assignee: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    parent_task: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    knowledge: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    resolution: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    detection: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    affected: Vec<Window>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    started_at: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    detected_at: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    mitigated_at: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    resolved_at: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    refs: Vec<String>,
    #[serde(default)]
    updated_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub key: String,
    pub title: String,
    /// "knowledge" (default) or "incident".
    pub kind: String,
    /// Incident-only fields; empty on knowledge entries.
    pub service: String,
    pub hosts: Vec<String>,
    pub severity: String,
    pub status: String,
    /// Task-only, optional: "" | low | medium | high | critical. Empty means
    /// unranked — the server never guesses one.
    pub priority: String,
    /// Task-only, optional: what the task is waiting on. Only meaningful while
    /// status=blocked; a leftover reason on any other status is a lie about the
    /// state, so it is rejected on write and cleared on a transition.
    pub blocked_reason: String,
    /// Task-only, optional: who holds the task right now — a short, stable,
    /// non-secret label ("agent-a", "ops-rotation"). Empty means unclaimed;
    /// nothing infers an owner. Every change is a new git version, so the
    /// history reconstructs who held it when.
    pub assignee: String,
    /// Task-only, optional: the `task-` key this task hangs under. Empty means
    /// top-level. A dangling parent is allowed (a child may be filed before its
    /// parent), self-parenting is not.
    pub parent_task: String,
    /// Keys of the knowledge entries this incident is tied to.
    pub knowledge: Vec<String>,
    /// How the incident ended: what fixed it, or the accepted outcome.
    /// Required to close (status=resolved).
    pub resolution: String,
    /// Executable "is it still happening?" check: a command/SQL plus what the
    /// healthy result looks like. The most actionable field of a report.
    pub detection: String,
    /// Poisoned/affected windows, machine-readable.
    pub affected: Vec<Window>,
    /// Incident timeline, RFC3339 UTC. started/detected come from the writer
    /// (detected_at defaults to filing time); mitigated/resolved are stamped
    /// by the server on the status transition.
    pub started_at: String,
    pub detected_at: String,
    pub mitigated_at: String,
    pub resolved_at: String,
    pub tags: Vec<String>,
    pub refs: Vec<String>,
    /// YYYY-MM-DD; set by the server, and only when content actually changes.
    pub updated_at: String,
    pub body: String,
}

impl Entry {
    pub fn from_markdown(s: &str) -> Result<Entry> {
        let rest = s
            .strip_prefix("---\n")
            .ok_or_else(|| anyhow!("missing frontmatter (file must start with ---)"))?;
        let end = rest
            .find("\n---\n")
            .ok_or_else(|| anyhow!("unclosed frontmatter (---)"))?;
        let meta: Meta = serde_yaml::from_str(&rest[..end]).context("broken yaml in frontmatter")?;
        let body = rest[end + 5..].trim_start_matches('\n').trim_end().to_string();
        Ok(Entry {
            key: meta.key,
            title: meta.title,
            kind: meta.kind,
            service: meta.service,
            hosts: meta.hosts,
            severity: meta.severity,
            status: meta.status,
            priority: meta.priority,
            blocked_reason: meta.blocked_reason,
            assignee: meta.assignee,
            parent_task: meta.parent_task,
            knowledge: meta.knowledge,
            resolution: meta.resolution,
            detection: meta.detection,
            affected: meta.affected,
            started_at: meta.started_at,
            detected_at: meta.detected_at,
            mitigated_at: meta.mitigated_at,
            resolved_at: meta.resolved_at,
            tags: meta.tags,
            refs: meta.refs,
            updated_at: meta.updated_at,
            body,
        })
    }

    pub fn to_markdown(&self) -> String {
        let meta = Meta {
            key: self.key.clone(),
            title: self.title.clone(),
            kind: self.kind.clone(),
            service: self.service.clone(),
            hosts: self.hosts.clone(),
            severity: self.severity.clone(),
            status: self.status.clone(),
            priority: self.priority.clone(),
            blocked_reason: self.blocked_reason.clone(),
            assignee: self.assignee.clone(),
            parent_task: self.parent_task.clone(),
            knowledge: self.knowledge.clone(),
            resolution: self.resolution.clone(),
            detection: self.detection.clone(),
            affected: self.affected.clone(),
            started_at: self.started_at.clone(),
            detected_at: self.detected_at.clone(),
            mitigated_at: self.mitigated_at.clone(),
            resolved_at: self.resolved_at.clone(),
            tags: self.tags.clone(),
            refs: self.refs.clone(),
            updated_at: self.updated_at.clone(),
        };
        let yaml = serde_yaml::to_string(&meta).expect("meta always serializes");
        format!("---\n{yaml}---\n\n{}\n", self.body.trim_end())
    }

    /// Content comparison ignoring updated_at: identical content makes upsert
    /// a no-op — no date bump, no empty commit.
    pub fn same_content(&self, other: &Entry) -> bool {
        self.key == other.key
            && self.title == other.title
            && self.kind == other.kind
            && self.service == other.service
            && self.hosts == other.hosts
            && self.severity == other.severity
            && self.status == other.status
            && self.priority == other.priority
            && self.blocked_reason == other.blocked_reason
            && self.assignee == other.assignee
            && self.parent_task == other.parent_task
            && self.knowledge == other.knowledge
            && self.resolution == other.resolution
            && self.detection == other.detection
            && self.affected == other.affected
            && self.started_at == other.started_at
            && self.detected_at == other.detected_at
            && self.mitigated_at == other.mitigated_at
            && self.resolved_at == other.resolved_at
            && self.tags == other.tags
            && self.refs == other.refs
            && self.body.trim_end() == other.body.trim_end()
    }

    pub fn is_incident(&self) -> bool {
        self.kind == KIND_INCIDENT
    }

    pub fn is_task(&self) -> bool {
        self.kind == KIND_TASK
    }

    /// A closed entry leaves the working tree (archive-on-close); its latest
    /// version stays in the default search. in_progress and blocked are work in
    /// flight, not an ending: they keep the task live in the canon.
    pub fn is_closed(&self) -> bool {
        (self.is_incident() && self.status == "resolved")
            || (self.is_task() && TASK_TERMINAL_STATUSES.contains(&self.status.as_str()))
    }

    /// A task that is still work — the three live statuses. What `kyb tasks`
    /// and `open_tasks` count.
    pub fn is_live_task(&self) -> bool {
        self.is_task() && TASK_LIVE_STATUSES.contains(&self.status.as_str())
    }

    /// Unfinished follow-ups: `- [ ]` checklist items in the body (a body
    /// convention, not schema). Lets a resolve warn about loose ends and a
    /// listing surface reports that still need work.
    pub fn open_followups(&self) -> usize {
        self.body.lines().filter(|l| l.trim_start().starts_with("- [ ]")).count()
    }

    pub fn validate(&self) -> Result<()> {
        if !is_valid_key(&self.key) {
            bail!("key must be a slug: [a-z0-9-], no leading/trailing dash, ≤100 chars");
        }
        if self.title.trim().is_empty() {
            bail!("title is empty");
        }
        match self.kind.as_str() {
            KIND_KNOWLEDGE => {
                if self.key.starts_with(INCIDENT_PREFIX) {
                    bail!("keys starting with '{INCIDENT_PREFIX}' are reserved for incident reports");
                }
                if self.key.starts_with(TASK_PREFIX) {
                    bail!("keys starting with '{TASK_PREFIX}' are reserved for tasks");
                }
            }
            KIND_TASK => {
                if !self.key.starts_with(TASK_PREFIX) {
                    bail!("task keys must start with '{TASK_PREFIX}' (e.g. task-raise-log-retention)");
                }
                if !TASK_STATUSES.contains(&self.status.as_str()) {
                    bail!("task status must be one of: {}", TASK_STATUSES.join("|"));
                }
                if !self.priority.is_empty() && !PRIORITIES.contains(&self.priority.as_str()) {
                    bail!("task priority must be empty or one of: {}", PRIORITIES.join("|"));
                }
                // a reason left over from an earlier block would describe a
                // state the task is no longer in — refuse instead of lying
                if self.status != STATUS_BLOCKED && !self.blocked_reason.trim().is_empty() {
                    bail!(
                        "blocked_reason belongs to status={STATUS_BLOCKED}, not '{}' — clear it or block the task",
                        self.status
                    );
                }
                if self.is_closed() && self.resolution.trim().is_empty() {
                    bail!("closing a task needs a resolution: what came of it, or why it was dropped");
                }
                // an owner label, not a paragraph and not a second body: one
                // line, bounded, and never a place to park a credential
                if !self.assignee.is_empty() {
                    if self.assignee.trim().is_empty() {
                        bail!("assignee is blank — leave it out to mark the task unclaimed");
                    }
                    if self.assignee.chars().count() > ASSIGNEE_MAX_LEN {
                        bail!(
                            "assignee must be at most {ASSIGNEE_MAX_LEN} chars — it is a short owner label, not a note"
                        );
                    }
                    if self.assignee.contains(&['\n', '\r'][..]) {
                        bail!("assignee must be a single line");
                    }
                }
                if !self.parent_task.is_empty() {
                    // a parent that is not a task key would point the tree at a
                    // knowledge entry or an incident, which is not a parent at all
                    if !is_valid_key(&self.parent_task)
                        || !self.parent_task.starts_with(TASK_PREFIX)
                    {
                        bail!(
                            "parent_task must be empty or a valid task key starting with '{TASK_PREFIX}' (got '{}')",
                            self.parent_task
                        );
                    }
                    if self.parent_task == self.key {
                        bail!("a task cannot be its own parent ('{}')", self.key);
                    }
                }
                for k in &self.knowledge {
                    if !is_valid_key(k) {
                        bail!("knowledge link '{k}' is not a valid key slug");
                    }
                }
                if let Some(hit) = find_secret(&self.resolution) {
                    bail!("resolution looks like a secret ({hit}…) — store secrets as pointers in refs");
                }
                if let Some(hit) = find_secret(&self.blocked_reason) {
                    bail!("blocked_reason looks like a secret ({hit}…) — store secrets as pointers in refs");
                }
                if let Some(hit) = find_secret(&self.assignee) {
                    bail!("assignee looks like a secret ({hit}…) — use a stable public label");
                }
            }
            KIND_INCIDENT => {
                if !self.key.starts_with(INCIDENT_PREFIX) {
                    bail!("incident keys must start with '{INCIDENT_PREFIX}' (e.g. inc-2026-07-22-orders-api-oom)");
                }
                if self.service.trim().is_empty() {
                    bail!("incident needs a service (what broke)");
                }
                if !SEVERITIES.contains(&self.severity.as_str()) {
                    bail!("severity must be one of: {}", SEVERITIES.join("|"));
                }
                if !STATUSES.contains(&self.status.as_str()) {
                    bail!("status must be one of: {}", STATUSES.join("|"));
                }
                if self.status == "resolved" && self.resolution.trim().is_empty() {
                    bail!("closing an incident needs a resolution: what fixed it, or the accepted outcome");
                }
                for k in &self.knowledge {
                    if !is_valid_key(k) {
                        bail!("knowledge link '{k}' is not a valid key slug");
                    }
                }
                for w in &self.affected {
                    if w.scope.trim().is_empty() || w.from.trim().is_empty() || w.to.trim().is_empty()
                    {
                        bail!("affected window needs scope, from and to (got scope='{}' from='{}' to='{}')",
                            w.scope, w.from, w.to);
                    }
                }
                if let Some(hit) = find_secret(&self.resolution) {
                    bail!("resolution looks like a secret ({hit}…) — store secrets as pointers in refs");
                }
                if let Some(hit) = find_secret(&self.detection) {
                    bail!("detection looks like a secret ({hit}…) — store secrets as pointers in refs");
                }
            }
            other => bail!(
                "kind must be '{KIND_KNOWLEDGE}', '{KIND_INCIDENT}' or '{KIND_TASK}', got '{other}'"
            ),
        }
        // priority, blocked_reason, assignee and parent_task are the task lane
        // only: an incident is ranked by severity and owned by whoever is on
        // call, a knowledge entry is a fact and has no owner at all
        if !self.is_task() {
            if !self.priority.trim().is_empty() {
                bail!("priority is a task-only field (got '{}' on kind '{}')", self.priority, self.kind);
            }
            if !self.blocked_reason.trim().is_empty() {
                bail!("blocked_reason is a task-only field (kind '{}')", self.kind);
            }
            if !self.assignee.trim().is_empty() {
                bail!("assignee is a task-only field (kind '{}')", self.kind);
            }
            if !self.parent_task.trim().is_empty() {
                bail!("parent_task is a task-only field (kind '{}')", self.kind);
            }
        }
        if let Some(hit) = find_secret(&self.body) {
            bail!("body looks like a secret ({hit}…) — store secrets as pointers in refs");
        }
        for r in &self.refs {
            if let Some(hit) = find_secret(r) {
                bail!("refs look like a secret ({hit}…) — refs are pointers, not the secrets");
            }
        }
        Ok(())
    }
}

impl Default for Entry {
    fn default() -> Entry {
        Entry {
            key: String::new(),
            title: String::new(),
            kind: default_kind(),
            service: String::new(),
            hosts: vec![],
            severity: String::new(),
            status: String::new(),
            priority: String::new(),
            blocked_reason: String::new(),
            assignee: String::new(),
            parent_task: String::new(),
            knowledge: vec![],
            resolution: String::new(),
            detection: String::new(),
            affected: vec![],
            started_at: String::new(),
            detected_at: String::new(),
            mitigated_at: String::new(),
            resolved_at: String::new(),
            tags: vec![],
            refs: vec![],
            updated_at: String::new(),
            body: String::new(),
        }
    }
}

/// One validator for every key input: upsert payloads AND path params of
/// GET/DELETE/history (otherwise `..%2F` in a URL walks the filesystem).
pub fn is_valid_key(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 100
        && k.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !k.starts_with('-')
        && !k.ends_with('-')
}

static SECRET_RES: OnceLock<Vec<regex::Regex>> = OnceLock::new();

fn secret_res() -> &'static [regex::Regex] {
    SECRET_RES.get_or_init(|| {
        [
            r"ghp_[A-Za-z0-9]{20,}",
            r"github_pat_[A-Za-z0-9_]{20,}",
            r"xox[baprs]-[A-Za-z0-9-]{10,}",
            r"\bsk-[A-Za-z0-9_-]{24,}",
            r"AKIA[0-9A-Z]{16}",
            r"-----BEGIN [A-Z ]*PRIVATE KEY",
            r#"(?i)(password|passwd|пароль)\s*[:=]\s*[^\s"']{6,}"#,
        ]
        .iter()
        .map(|p| regex::Regex::new(p).expect("valid pattern"))
        .collect()
    })
}

pub fn find_secret(text: &str) -> Option<String> {
    secret_res()
        .iter()
        .find_map(|re| re.find(text).map(|m| m.as_str().chars().take(20).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn entry() -> Entry {
        Entry {
            key: "nats-streams".into(),
            title: "NATS — always CreateOrUpdateStream".into(),
            tags: vec!["nats".into(), "infra".into()],
            refs: vec!["see .env on host-a".into()],
            updated_at: "2026-07-19".into(),
            body: "Always use CreateOrUpdateStream.\n\nRelated: [[orders-api]].".into(),
            ..Default::default()
        }
    }

    fn incident() -> Entry {
        Entry {
            key: "inc-2026-07-22-orders-api-oom".into(),
            title: "orders_api OOM on host-a".into(),
            kind: KIND_INCIDENT.into(),
            service: "orders_api".into(),
            hosts: vec!["host-a".into()],
            severity: "high".into(),
            status: "open".into(),
            knowledge: vec!["orders-api-architecture".into()],
            tags: vec!["acme".into()],
            updated_at: "2026-07-22".into(),
            body: "What happened: the container hit the memory limit.\nWorkaround: restart it.".into(),
            ..Default::default()
        }
    }

    #[test]
    fn roundtrip() {
        let e = entry();
        let md = e.to_markdown();
        let back = Entry::from_markdown(&md).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn incident_roundtrip() {
        let e = incident();
        let md = e.to_markdown();
        assert!(md.contains("kind: incident"), "{md}");
        assert!(md.contains("service: orders_api"), "{md}");
        let back = Entry::from_markdown(&md).unwrap();
        assert_eq!(e, back);
        assert!(back.is_incident());
    }

    // knowledge entries must not leak incident frontmatter noise
    #[test]
    fn knowledge_frontmatter_stays_clean() {
        let md = entry().to_markdown();
        for f in ["kind:", "service:", "severity:", "status:", "hosts:", "knowledge:"] {
            assert!(!md.contains(f), "unexpected '{f}' in:\n{md}");
        }
    }

    // files written before the incident feature (no kind field) stay readable
    #[test]
    fn legacy_file_defaults_to_knowledge() {
        let md = "---\nkey: old-entry\ntitle: An old fact\n---\n\nbody\n";
        let e = Entry::from_markdown(md).unwrap();
        assert_eq!(e.kind, KIND_KNOWLEDGE);
        assert!(!e.is_incident());
        assert!(e.validate().is_ok());
    }

    #[test]
    fn incident_validation_ok() {
        assert!(incident().validate().is_ok());
    }

    // --- incident validation: each rule fires ---
    #[rstest]
    #[case::key_without_prefix(|e: &mut Entry| e.key = "orders-api-oom".into())]
    #[case::no_service(|e: &mut Entry| e.service = "  ".into())]
    #[case::bad_severity(|e: &mut Entry| e.severity = "huge".into())]
    #[case::empty_severity(|e: &mut Entry| e.severity = String::new())]
    #[case::bad_status(|e: &mut Entry| e.status = "wip".into())]
    #[case::empty_status(|e: &mut Entry| e.status = String::new())]
    #[case::bad_knowledge_link(|e: &mut Entry| e.knowledge = vec!["Bad Key".into()])]
    #[case::bad_kind(|e: &mut Entry| e.kind = "report".into())]
    #[case::resolved_without_resolution(|e: &mut Entry| e.status = "resolved".into())]
    #[case::secret_in_resolution(|e: &mut Entry| {
        e.status = "resolved".into();
        e.resolution = "rotated to password: super123secret".into();
    })]
    fn incident_validation_rejects(#[case] mutate: fn(&mut Entry)) {
        let mut e = incident();
        mutate(&mut e);
        assert!(e.validate().is_err(), "must be rejected: {e:?}");
    }

    // the actionable fields (detection, windows, timeline) roundtrip intact
    #[test]
    fn incident_actionable_fields_roundtrip() {
        let mut e = incident();
        e.detection = "SELECT count() FROM t WHERE price > 50 * yesterday_max; healthy = 0 rows".into();
        e.affected = vec![
            Window { scope: "okx".into(), from: "2026-07-22T08:09:40Z".into(), to: "2026-07-22T21:09:37Z".into() },
            Window { scope: "gateio".into(), from: "2026-07-22T08:20:09Z".into(), to: "2026-07-22T20:04:41Z".into() },
        ];
        e.started_at = "2026-07-22T08:09:40Z".into();
        e.detected_at = "2026-07-22T19:30:00Z".into();
        e.mitigated_at = "2026-07-22T20:04:41Z".into();
        e.resolved_at = "2026-07-22T21:55:00Z".into();
        assert!(e.validate().is_ok());
        let back = Entry::from_markdown(&e.to_markdown()).unwrap();
        assert_eq!(back, e);
        // and they count as content: adding a window is a new version
        let mut f = back.clone();
        f.affected.pop();
        assert!(!e.same_content(&f));
    }

    #[rstest]
    #[case::empty_scope(Window { scope: " ".into(), from: "a".into(), to: "b".into() })]
    #[case::empty_from(Window { scope: "okx".into(), from: "".into(), to: "b".into() })]
    #[case::empty_to(Window { scope: "okx".into(), from: "a".into(), to: " ".into() })]
    fn bad_affected_window_rejected(#[case] w: Window) {
        let mut e = incident();
        e.affected = vec![w];
        assert!(e.validate().is_err());
    }

    #[test]
    fn secret_in_detection_rejected() {
        let mut e = incident();
        e.detection = "curl -u admin --password: super123secret host".into();
        assert!(e.validate().is_err());
    }

    // follow-ups: a body convention the server can count
    #[test]
    fn open_followups_counted() {
        let mut e = incident();
        assert_eq!(e.open_followups(), 0);
        e.body = "Follow-ups:\n- [ ] guard dev NATS\n  - [ ] nested also counts\n- [x] webui filter\n- [] not a checkbox\n".into();
        assert_eq!(e.open_followups(), 2);
    }

    // closing with a resolution is the happy path; roundtrips through markdown
    #[test]
    fn incident_close_with_resolution() {
        let mut e = incident();
        e.status = "resolved".into();
        e.resolution = "Raised the memory limit to 2G and fixed the batch flush leak.".into();
        assert!(e.validate().is_ok());
        let back = Entry::from_markdown(&e.to_markdown()).unwrap();
        assert_eq!(back, e);
        // mitigated does not demand a resolution yet
        e.status = "mitigated".into();
        e.resolution = String::new();
        assert!(e.validate().is_ok());
    }

    // the inc- prefix is reserved: a knowledge entry cannot squat on it
    #[test]
    fn knowledge_cannot_use_incident_prefix() {
        let mut e = entry();
        e.key = "inc-2026-07-22-fake".into();
        assert!(e.validate().is_err());
    }

    // incident status/severity changes are content changes (versioned), not no-ops
    #[test]
    fn incident_status_change_is_a_change() {
        let a = incident();
        let mut b = incident();
        b.status = "resolved".into();
        assert!(!a.same_content(&b));
        let mut c = incident();
        c.knowledge.push("nats-streams".into());
        assert!(!a.same_content(&c));
    }

    fn task() -> Entry {
        Entry {
            key: "task-raise-log-retention".into(),
            title: "Raise container log retention to 72h".into(),
            kind: KIND_TASK.into(),
            status: "open".into(),
            tags: vec!["idea".into()],
            updated_at: "2026-08-15".into(),
            body: "Short retention loses evidence.\n\n- [ ] measure log volume first".into(),
            ..Default::default()
        }
    }

    // --- task priority: optional, and only the four ranks ---
    #[rstest]
    #[case::unranked("", true)]
    #[case::low("low", true)]
    #[case::medium("medium", true)]
    #[case::high("high", true)]
    #[case::critical("critical", true)]
    #[case::unknown_rank("urgent", false)]
    #[case::wrong_case("HIGH", false)]
    #[case::blank("  ", false)]
    #[case::numeric("1", false)]
    fn task_priority_validation(#[case] priority: &str, #[case] ok: bool) {
        let mut e = task();
        e.priority = priority.into();
        assert_eq!(e.validate().is_ok(), ok, "priority '{priority}'");
    }

    // priority is the task lane only — the other kinds are not ranked this way
    #[rstest]
    #[case::knowledge_priority(entry(), |e: &mut Entry| e.priority = "high".into())]
    #[case::knowledge_blocked_reason(entry(), |e: &mut Entry| e.blocked_reason = "waiting".into())]
    #[case::knowledge_assignee(entry(), |e: &mut Entry| e.assignee = "agent-a".into())]
    #[case::knowledge_parent(entry(), |e: &mut Entry| e.parent_task = "task-parent".into())]
    #[case::incident_priority(incident(), |e: &mut Entry| e.priority = "high".into())]
    #[case::incident_blocked_reason(incident(), |e: &mut Entry| e.blocked_reason = "waiting".into())]
    #[case::incident_assignee(incident(), |e: &mut Entry| e.assignee = "agent-a".into())]
    #[case::incident_parent(incident(), |e: &mut Entry| e.parent_task = "task-parent".into())]
    fn task_only_fields_rejected_elsewhere(#[case] mut e: Entry, #[case] mutate: fn(&mut Entry)) {
        assert!(e.validate().is_ok(), "baseline must be valid: {e:?}");
        mutate(&mut e);
        assert!(e.validate().is_err(), "task-only field must be rejected: {e:?}");
    }

    // --- task statuses: three live, two terminal ---
    #[rstest]
    #[case("open", true, false)]
    #[case("in_progress", true, false)]
    #[case("blocked", true, false)]
    #[case("done", false, true)]
    #[case("dropped", false, true)]
    fn task_status_lifecycle(#[case] status: &str, #[case] live: bool, #[case] closed: bool) {
        let mut e = task();
        e.status = status.into();
        // only the terminal statuses demand an outcome
        e.resolution = "what came of it".into();
        assert!(e.validate().is_ok(), "status '{status}' must be accepted");
        assert_eq!(e.is_live_task(), live, "live('{status}')");
        assert_eq!(e.is_closed(), closed, "closed('{status}')");
    }

    #[rstest]
    #[case::unknown("wip")]
    #[case::empty("")]
    #[case::dash("in-progress")]
    #[case::incident_status("mitigated")]
    fn bad_task_status_rejected(#[case] status: &str) {
        let mut e = task();
        e.status = status.into();
        assert!(e.validate().is_err(), "status '{status}' must be rejected");
    }

    // in_progress and blocked are work in flight: they do NOT need a resolution
    // and do NOT archive
    #[test]
    fn live_statuses_need_no_resolution() {
        for status in ["in_progress", "blocked"] {
            let mut e = task();
            e.status = status.into();
            assert!(e.validate().is_ok(), "{status} must not demand a resolution");
            assert!(!e.is_closed());
        }
        // the terminal ones still do
        let mut e = task();
        e.status = "done".into();
        assert!(e.validate().is_err(), "done still needs a resolution");
    }

    // a blocked reason on any other status describes a state the task is not in
    #[test]
    fn blocked_reason_belongs_to_blocked() {
        let mut e = task();
        e.status = STATUS_BLOCKED.into();
        e.blocked_reason = "waiting on the vendor to ship the fix".into();
        assert!(e.validate().is_ok());
        // blocked without a reason stays legal — the field is optional
        e.blocked_reason = String::new();
        assert!(e.validate().is_ok());
        // but a stale reason on a live or terminal status is refused
        for status in ["open", "in_progress", "done"] {
            let mut stale = task();
            stale.status = status.into();
            stale.resolution = "outcome".into();
            stale.blocked_reason = "waiting on the vendor".into();
            let err = stale.validate().unwrap_err().to_string();
            assert!(err.contains("blocked_reason"), "status '{status}': {err}");
        }
    }

    #[test]
    fn secret_in_blocked_reason_rejected() {
        let mut e = task();
        e.status = STATUS_BLOCKED.into();
        e.blocked_reason = "waiting for the password: super123secret rotation".into();
        assert!(e.validate().is_err());
    }

    // the new fields survive markdown and count as content
    #[test]
    fn task_roundtrip_with_priority_and_block() {
        let mut e = task();
        e.priority = "critical".into();
        e.status = STATUS_BLOCKED.into();
        e.blocked_reason = "waiting on the host-a disk upgrade".into();
        e.knowledge = vec!["web-app-architecture".into()];
        assert!(e.validate().is_ok());
        let md = e.to_markdown();
        assert!(md.contains("kind: task"), "{md}");
        assert!(md.contains("priority: critical"), "{md}");
        assert!(md.contains("blocked_reason: waiting on the host-a disk upgrade"), "{md}");
        let back = Entry::from_markdown(&md).unwrap();
        assert_eq!(back, e);
        assert!(back.is_task());

        // both fields are content: changing either is a new version
        let mut other = back.clone();
        other.priority = "low".into();
        assert!(!e.same_content(&other));
        let mut other = back.clone();
        other.blocked_reason = "waiting on something else".into();
        assert!(!e.same_content(&other));
    }

    // --- task ownership: an optional, bounded, non-secret label ---
    #[rstest]
    #[case::unclaimed("", true)]
    #[case::label("agent-a", true)]
    #[case::rotation("ops-rotation", true)]
    #[case::spaces_inside("release duty", true)]
    #[case::blank("   ", false)]
    #[case::multiline("agent-a\nagent-b", false)]
    #[case::carriage_return("agent-a\ragent-b", false)]
    #[case::too_long("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", false)]
    #[case::secret("token: ghp_abcdefghijklmnopqrstuvwxyz123456", false)]
    fn task_assignee_validation(#[case] assignee: &str, #[case] ok: bool) {
        let mut e = task();
        e.assignee = assignee.into();
        assert_eq!(e.validate().is_ok(), ok, "assignee '{assignee}'");
    }

    #[test]
    fn assignee_length_boundary() {
        let mut e = task();
        e.assignee = "a".repeat(ASSIGNEE_MAX_LEN);
        assert!(e.validate().is_ok(), "exactly the limit is fine");
        e.assignee = "a".repeat(ASSIGNEE_MAX_LEN + 1);
        assert!(e.validate().is_err(), "one over the limit is not");
    }

    // --- parent_task: empty or a real task key, never itself ---
    #[rstest]
    #[case::top_level("", true)]
    #[case::task_key("task-migrate-logs", true)]
    // a forward reference is legal: a child may be filed before its parent
    #[case::dangling_parent("task-not-written-yet", true)]
    #[case::knowledge_key("nats-streams", false)]
    #[case::incident_key("inc-2026-08-15-oom", false)]
    #[case::not_a_slug("task-Bad Key", false)]
    #[case::traversal("../task-x", false)]
    #[case::bare_prefix("task-", false)]
    #[case::blank("  ", false)]
    fn task_parent_validation(#[case] parent: &str, #[case] ok: bool) {
        let mut e = task();
        e.parent_task = parent.into();
        assert_eq!(e.validate().is_ok(), ok, "parent_task '{parent}'");
    }

    // a task that parents itself is a cycle of one — the smallest broken tree
    #[test]
    fn self_parenting_rejected() {
        let mut e = task();
        e.parent_task = e.key.clone();
        let err = e.validate().unwrap_err().to_string();
        assert!(err.contains("own parent"), "{err}");
    }

    // ownership and the parent link survive markdown and count as content
    #[test]
    fn task_assignee_parent_roundtrip() {
        let mut e = task();
        e.status = "in_progress".into();
        e.assignee = "agent-a".into();
        e.parent_task = "task-migrate-logs".into();
        assert!(e.validate().is_ok());
        let md = e.to_markdown();
        assert!(md.contains("assignee: agent-a"), "{md}");
        assert!(md.contains("parent_task: task-migrate-logs"), "{md}");
        let back = Entry::from_markdown(&md).unwrap();
        assert_eq!(back, e);

        // both are content: changing either is a new version, not a no-op
        let mut other = back.clone();
        other.assignee = "agent-b".into();
        assert!(!e.same_content(&other));
        let mut other = back.clone();
        other.parent_task = "task-other-parent".into();
        assert!(!e.same_content(&other));
        let mut other = back.clone();
        other.assignee.clear();
        assert!(!e.same_content(&other), "handing a task back is a change too");
        // and the date still is not
        let mut same = back.clone();
        same.updated_at = "2020-01-01".into();
        assert!(e.same_content(&same));
    }

    // an unranked, unblocked, unclaimed task writes no new frontmatter at all —
    // files from before the feature parse identically
    #[test]
    fn task_frontmatter_stays_clean_without_new_fields() {
        let md = task().to_markdown();
        for f in ["priority:", "blocked_reason:", "assignee:", "parent_task:"] {
            assert!(!md.contains(f), "unexpected '{f}' in:\n{md}");
        }
        let legacy = "---\nkey: task-old\ntitle: An old task\nkind: task\nstatus: open\n---\n\nbody\n";
        let e = Entry::from_markdown(legacy).unwrap();
        assert_eq!(e.priority, "");
        assert_eq!(e.blocked_reason, "");
        assert_eq!(e.assignee, "");
        assert_eq!(e.parent_task, "");
        assert!(e.validate().is_ok());
        assert!(e.is_live_task());
        // and a task written by this version reads back unchanged after a
        // round-trip through the legacy-shaped parser
        assert_eq!(Entry::from_markdown(&e.to_markdown()).unwrap(), e);
    }

    #[test]
    fn roundtrip_empty_lists() {
        let mut e = entry();
        e.tags.clear();
        e.refs.clear();
        let back = Entry::from_markdown(&e.to_markdown()).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn body_with_dashes_survives() {
        let mut e = entry();
        e.body = "before\n---\nafter".into();
        let back = Entry::from_markdown(&e.to_markdown()).unwrap();
        assert_eq!(back.body, e.body);
    }

    // --- keys: valid slugs ---
    #[rstest]
    #[case("a")]
    #[case("key")]
    #[case("nats-streams")]
    #[case("a-b-c-d")]
    #[case("key9")]
    #[case("9key")]
    #[case("a--b")]
    #[case("orders-api-deploy-2026")]
    #[case("x1")]
    #[case("0")]
    fn valid_keys(#[case] k: &str) {
        assert!(is_valid_key(k), "'{k}' must be accepted");
    }

    // --- keys: garbage and traversal ---
    #[rstest]
    #[case("")]
    #[case("-a")]
    #[case("a-")]
    #[case("-")]
    #[case("A")]
    #[case("Key")]
    #[case("a b")]
    #[case("a_b")]
    #[case("a.b")]
    #[case("a/b")]
    #[case("../etc")]
    #[case("..")]
    #[case(".git")]
    #[case("ключ")]
    #[case("key!")]
    #[case("a\\b")]
    fn invalid_keys(#[case] k: &str) {
        assert!(!is_valid_key(k), "'{k}' must be rejected");
    }

    #[test]
    fn key_length_limit() {
        assert!(is_valid_key(&"a".repeat(100)));
        assert!(!is_valid_key(&"a".repeat(101)));
    }

    // --- secret heuristics: must detect ---
    #[rstest]
    #[case("ghp_abcdefghijklmnopqrstuvwxyz123456")]
    #[case("token: github_pat_11ABCDEFGH_abcdefghijklmnopqrst")]
    #[case("xoxb-123456789012-abcdefghijklmnop")]
    #[case("slack xoxp-1-2-3-abcdefghij")]
    #[case("key sk-proj-abcdefghijklmnopqrstuvwxyz")]
    #[case("AKIAIOSFODNN7EXAMPLE")]
    #[case("-----BEGIN RSA PRIVATE KEY-----")]
    #[case("-----BEGIN OPENSSH PRIVATE KEY-----")]
    #[case("-----BEGIN PRIVATE KEY-----")]
    #[case("password: hunter2secret")]
    #[case("password=SuperSecret99")]
    #[case("passwd = qwerty123456")]
    #[case("Пароль: сложный123пароль")] // cyrillic keyword, case-insensitive — data on purpose
    fn secrets_detected(#[case] s: &str) {
        assert!(find_secret(s).is_some(), "must be detected: {s}");
        let mut e = entry();
        e.body = s.into();
        assert!(e.validate().is_err());
    }

    // --- secret heuristics: no false positives ---
    #[rstest]
    #[case("commit a77ed01118d25f8cd76c9ec38f2a77ed01118d25")]
    #[case("mentions the ghp_ prefix, no token")]
    #[case("sk-short")]
    #[case("AKIA alone is not a key")]
    #[case("-----BEGIN CERTIFICATE-----")]
    #[case("-----BEGIN PUBLIC KEY-----")]
    #[case("ssh-rsa AAAAB3NzaC1yc2E user@host is a public key")]
    #[case("пароль лежит в 1Password")] // cyrillic keyword without a value — data on purpose
    #[case("password policy: minimum 12 chars")]
    #[case("plain text about nats and streams")]
    #[case("xoxb- truncated is harmless")]
    #[case("[[task-web-app-symbol-references-coordinate-base]]")]
    fn secrets_not_flagged(#[case] s: &str) {
        assert!(find_secret(s).is_none(), "false positive on: {s}");
    }

    // secrets in refs are rejected too
    #[test]
    fn secret_in_refs_rejected() {
        let mut e = entry();
        e.refs = vec!["ghp_abcdefghijklmnopqrstuvwxyz123456".into()];
        assert!(e.validate().is_err());
    }

    #[test]
    fn empty_title_rejected() {
        let mut e = entry();
        e.title = "   ".into();
        assert!(e.validate().is_err());
    }

    // --- round-trip of wild bodies ---
    #[rstest]
    #[case("")]
    #[case("plain body")]
    #[case("---\nstarts with a delimiter")]
    #[case("```rust\nfn main() {}\n```")]
    #[case("[[link-one]] and [[link-two]]")]
    #[case("emoji 🚀🔥 and «guillemets»")]
    #[case("tabs\tand   spaces")]
    #[case("multiline\n\n\nwith blanks")]
    #[case("internal\r\nCRLF preserved")]
    #[case("yaml-like:\n  key: value\n---\n- list")]
    #[case("# heading\n## another")]
    #[case("<html><b>markup</b></html>")]
    #[case("кириллица, 中文, العربية")] // unicode payloads stay intact — data on purpose
    fn body_roundtrip(#[case] body: &str) {
        let mut e = entry();
        e.body = body.trim_end().to_string();
        let back = Entry::from_markdown(&e.to_markdown()).unwrap();
        assert_eq!(back, e);
    }

    // --- round-trip of yaml-special titles ---
    #[rstest]
    #[case("plain title")]
    #[case("NATS: with a colon")]
    #[case("\"quoted\"")]
    #[case("with 'apostrophes'")]
    #[case("[brackets] and {braces}")]
    #[case("- starts with a dash")]
    #[case("123")]
    #[case("null")]
    #[case("true")]
    #[case("multiline\ntitle")]
    #[case("#hash and *stars*")]
    #[case("| pipe and > arrow")]
    #[case("Русский тайтл со смыслом")] // real titles will be Russian — data on purpose
    fn title_roundtrip(#[case] title: &str) {
        let mut e = entry();
        e.title = title.to_string();
        let back = Entry::from_markdown(&e.to_markdown()).unwrap();
        assert_eq!(back.title, e.title);
    }

    #[test]
    fn tags_refs_special_chars_roundtrip() {
        let mut e = entry();
        e.tags = vec!["UPPER".into(), "with-dash".into(), "кириллица".into(), "a:b".into()];
        e.refs = vec!["see .env on host-a".into(), "https://example.com/path?q=1&x=2".into()];
        let back = Entry::from_markdown(&e.to_markdown()).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn huge_body_roundtrip() {
        let mut e = entry();
        e.body = "a line of knowledge about services and infra\n".repeat(20_000).trim_end().to_string();
        let back = Entry::from_markdown(&e.to_markdown()).unwrap();
        assert_eq!(back.body.len(), e.body.len());
    }

    #[test]
    fn bad_frontmatter_is_error() {
        assert!(Entry::from_markdown("plain text without frontmatter").is_err());
        assert!(Entry::from_markdown("---\nkey: x\ntitle: t\n").is_err()); // unclosed
        assert!(Entry::from_markdown("---\nnot yaml at all: [\n---\n\nbody\n").is_err());
    }

    #[test]
    fn same_content_ignores_date() {
        let a = entry();
        let mut b = entry();
        b.updated_at = "2020-01-01".into();
        assert!(a.same_content(&b));
        b.body = "different".into();
        assert!(!a.same_content(&b));
    }
}
