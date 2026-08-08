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
pub const TASK_STATUSES: [&str; 3] = ["open", "done", "dropped"];

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
    /// version stays in the default search.
    pub fn is_closed(&self) -> bool {
        (self.is_incident() && self.status == "resolved")
            || (self.is_task() && matches!(self.status.as_str(), "done" | "dropped"))
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
                if self.is_closed() && self.resolution.trim().is_empty() {
                    bail!("closing a task needs a resolution: what came of it, or why it was dropped");
                }
                for k in &self.knowledge {
                    if !is_valid_key(k) {
                        bail!("knowledge link '{k}' is not a valid key slug");
                    }
                }
                if let Some(hit) = find_secret(&self.resolution) {
                    bail!("resolution looks like a secret ({hit}…) — store secrets as pointers in refs");
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
