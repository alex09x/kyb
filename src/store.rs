use crate::model::{Entry, INCIDENT_PREFIX, KIND_KNOWLEDGE, TASK_PREFIX};
use anyhow::{Context, Result};
use git2::{Delta, DiffOptions, Repository, Signature, Sort};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// The `kyb-data/` git repo is the single source of truth. A Repository is
/// opened per operation (cheap, and sidesteps Send/Sync questions); parallel
/// writes are serialized by the shared write mutex in AppState.
///
/// Layout v2: one directory per kind — `knowledge/`, `incidents/`, `tasks/` —
/// and the working tree holds only what is live (open incidents/tasks,
/// current knowledge). Closed entries are archived: the file is removed, the
/// content stays in git history and in the search index.
pub struct Store {
    root: PathBuf,
}

const KIND_DIRS: [&str; 3] = ["knowledge", "incidents", "tasks"];

fn dir_for_key(key: &str) -> &'static str {
    if key.starts_with(INCIDENT_PREFIX) {
        "incidents"
    } else if key.starts_with(TASK_PREFIX) {
        "tasks"
    } else {
        "knowledge"
    }
}

pub struct Committed {
    pub sha: String,
    pub committed_at: i64,
    pub entry: Entry,
}

pub enum UpsertOutcome {
    Created(Committed),
    Updated(Committed),
    Unchanged(Entry),
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct VersionInfo {
    pub sha: String,
    pub committed_at: String,
    pub message: String,
    pub change: String, // added|modified|deleted
}

/// One version of one key from history — the unit of indexing.
pub struct HistoryDoc {
    pub sha: String,
    pub committed_at: i64,
    pub entry: Entry,
}

fn iso(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|t| t.to_rfc3339())
        .unwrap_or_default()
}

impl Store {
    pub fn open(root: &Path) -> Result<Store> {
        fs::create_dir_all(root)?;
        if Repository::open(root).is_err() {
            Repository::init(root).context("git init kyb-data")?;
        }
        let store = Store { root: root.to_path_buf() };
        store.migrate_layout()?;
        Ok(store)
    }

    fn repo(&self) -> Result<Repository> {
        Ok(Repository::open(&self.root)?)
    }

    fn sig() -> Result<Signature<'static>> {
        // fixed signature — no dependency on global git config
        Ok(Signature::now("kyb", "kyb@local")?)
    }

    /// Layout-v2 path of a key, plus the legacy flat path — old commits (and
    /// stray hand-made files) live at the repo root.
    fn file_rel(key: &str) -> String {
        format!("{}/{key}.md", dir_for_key(key))
    }

    fn legacy_rel(key: &str) -> String {
        format!("{key}.md")
    }

    fn existing_rel(&self, key: &str) -> Option<String> {
        [Self::file_rel(key), Self::legacy_rel(key)]
            .into_iter()
            .find(|rel| self.root.join(rel).is_file())
    }

    /// One-time move from the flat layout to per-kind directories, done by the
    /// service itself on start: the canon is self-migrating, no manual step on
    /// the server. Closed incidents/tasks are archived in the same pass — the
    /// new invariant is "the tree holds only what is live".
    fn migrate_layout(&self) -> Result<()> {
        let mut moves: Vec<Entry> = vec![];
        for f in fs::read_dir(&self.root)? {
            let p = f?.path();
            if !p.is_file() || p.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let Ok(s) = fs::read_to_string(&p) else { continue };
            let Ok(entry) = Entry::from_markdown(&s) else { continue };
            // only files named after their key move; foreign md stays put
            if p.file_name().and_then(|n| n.to_str()) != Some(&Self::legacy_rel(&entry.key)) {
                continue;
            }
            moves.push(entry);
        }
        if moves.is_empty() {
            return Ok(());
        }
        let repo = self.repo()?;
        let mut idx = repo.index()?;
        for e in &moves {
            let old_rel = Self::legacy_rel(&e.key);
            let new_rel = Self::file_rel(&e.key);
            let new_abs = self.root.join(&new_rel);
            fs::create_dir_all(new_abs.parent().expect("kind dir has a parent"))?;
            fs::rename(self.root.join(&old_rel), &new_abs)?;
            // an uncommitted stray file is not in the index — move it silently
            if idx.get_path(Path::new(&old_rel), 0).is_some() {
                idx.remove_path(Path::new(&old_rel))?;
            }
            idx.add_path(Path::new(&new_rel))?;
        }
        self.commit_index(&repo, &mut idx, &format!("kyb: migrate layout v2 ({} entries)", moves.len()))?;
        eprintln!("kyb: migrated {} entries to the per-kind layout", moves.len());

        let closed: Vec<&Entry> = moves.iter().filter(|e| e.is_closed()).collect();
        if !closed.is_empty() {
            for e in &closed {
                let rel = Self::file_rel(&e.key);
                fs::remove_file(self.root.join(&rel))?;
                idx.remove_path(Path::new(&rel))?;
            }
            self.commit_index(
                &repo,
                &mut idx,
                &format!("kyb: archive {} closed entries (layout migration)", closed.len()),
            )?;
            eprintln!("kyb: archived {} closed entries during migration", closed.len());
        }
        Ok(())
    }

    fn commit_index(&self, repo: &Repository, idx: &mut git2::Index, msg: &str) -> Result<(String, i64)> {
        idx.write()?;
        let tree = repo.find_tree(idx.write_tree()?)?;
        let sig = Self::sig()?;
        let parent = match repo.head() {
            Ok(h) => Some(h.peel_to_commit()?),
            Err(_) => None,
        };
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        let oid = repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &parents)?;
        let at = repo.find_commit(oid)?.time().seconds();
        Ok((oid.to_string(), at))
    }

    pub fn get(&self, key: &str) -> Result<Option<Entry>> {
        let Some(rel) = self.existing_rel(key) else {
            return Ok(None);
        };
        let s = fs::read_to_string(self.root.join(&rel))?;
        Ok(Some(Entry::from_markdown(&s).with_context(|| format!("corrupt file {rel}"))?))
    }

    /// Version of a key at a specific commit (sha or any rev).
    /// Unknown rev yields None (404 at the API layer, not 500).
    pub fn get_at(&self, key: &str, rev: &str) -> Result<Option<Entry>> {
        let repo = self.repo()?;
        let Ok(obj) = repo.revparse_single(rev) else {
            return Ok(None);
        };
        let Ok(commit) = obj.peel_to_commit() else {
            return Ok(None);
        };
        let tree = commit.tree()?;
        // pre-migration commits hold the file at the legacy flat path
        for rel in [Self::file_rel(key), Self::legacy_rel(key)] {
            let Ok(te) = tree.get_path(Path::new(&rel)) else { continue };
            let blob = te.to_object(&repo)?.peel_to_blob()?;
            let s = std::str::from_utf8(blob.content()).context("file is not utf-8")?;
            return Ok(Some(Entry::from_markdown(s)?));
        }
        Ok(None)
    }

    fn commit_path(&self, rel: &str, msg: &str, delete: bool) -> Result<(String, i64)> {
        let repo = self.repo()?;
        let mut idx = repo.index()?;
        if delete {
            idx.remove_path(Path::new(rel))?;
        } else {
            idx.add_path(Path::new(rel))?;
        }
        idx.write()?;
        let tree = repo.find_tree(idx.write_tree()?)?;
        let sig = Self::sig()?;
        // the first commit in an empty repo has no parent
        let parent = match repo.head() {
            Ok(h) => Some(h.peel_to_commit()?),
            Err(_) => None,
        };
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        let oid = repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &parents)?;
        let at = repo.find_commit(oid)?.time().seconds();
        Ok((oid.to_string(), at))
    }

    pub fn upsert(&self, mut entry: Entry, today: &str) -> Result<UpsertOutcome> {
        let existing = self.get(&entry.key)?;
        if let Some(old) = &existing {
            if old.same_content(&entry) {
                return Ok(UpsertOutcome::Unchanged(old.clone()));
            }
        }
        entry.updated_at = today.to_string();
        let rel = Self::file_rel(&entry.key);
        let abs = self.root.join(&rel);
        fs::create_dir_all(abs.parent().expect("kind dir has a parent"))?;
        fs::write(&abs, entry.to_markdown())?;
        let (sha, committed_at) =
            self.commit_path(&rel, &format!("kyb: upsert {}", entry.key), false)?;
        let c = Committed { sha, committed_at, entry };
        Ok(if existing.is_some() { UpsertOutcome::Updated(c) } else { UpsertOutcome::Created(c) })
    }

    fn remove_file(&self, key: &str, msg: &str) -> Result<Option<String>> {
        let Some(rel) = self.existing_rel(key) else {
            return Ok(None);
        };
        fs::remove_file(self.root.join(&rel))?;
        let (sha, _) = self.commit_path(&rel, msg, true)?;
        Ok(Some(sha))
    }

    /// Retraction: the entry was wrong. It leaves the default search
    /// (history keeps it, as everything else).
    pub fn delete(&self, key: &str) -> Result<Option<String>> {
        self.remove_file(key, &format!("kyb: delete {key}"))
    }

    /// Archival: a closed incident/task leaves the working tree, but its
    /// latest version stays in the default search.
    pub fn archive(&self, key: &str) -> Result<Option<String>> {
        self.remove_file(key, &format!("kyb: archive {key}"))
    }

    pub fn list_head(&self) -> Result<Vec<Entry>> {
        let mut out = vec![];
        let mut dirs = vec![self.root.clone()];
        for d in KIND_DIRS {
            let p = self.root.join(d);
            if p.is_dir() {
                dirs.push(p);
            }
        }
        for dir in dirs {
            for e in fs::read_dir(&dir)? {
                let p = e?.path();
                if !p.is_file() || p.extension().and_then(|x| x.to_str()) != Some("md") {
                    continue;
                }
                let s = fs::read_to_string(&p)?;
                match Entry::from_markdown(&s) {
                    Ok(en) => out.push(en),
                    Err(err) => eprintln!("kyb: skipping {}: {err:#}", p.display()),
                }
            }
        }
        Ok(out)
    }

    /// Latest content version of a key straight from history — how archived
    /// entries are read after their file is gone.
    pub fn latest_version(&self, key: &str) -> Result<Option<Entry>> {
        let repo = self.repo()?;
        if repo.head().is_err() {
            return Ok(None);
        }
        let mut walk = repo.revwalk()?;
        walk.push_head()?;
        walk.set_sorting(Sort::TOPOLOGICAL)?;
        for oid in walk {
            let oid = oid?;
            let commit = repo.find_commit(oid)?;
            let tree = commit.tree()?;
            for rel in [Self::file_rel(key), Self::legacy_rel(key)] {
                let Ok(te) = tree.get_path(Path::new(&rel)) else { continue };
                let blob = te.to_object(&repo)?.peel_to_blob()?;
                let Ok(s) = std::str::from_utf8(blob.content()) else { continue };
                return Ok(Entry::from_markdown(s).ok());
            }
        }
        Ok(None)
    }

    /// Latest versions of archived incidents/tasks: keys that are gone from
    /// the tree but must stay findable (feeds the vector side).
    pub fn archived_latest(&self) -> Result<Vec<Entry>> {
        let alive: HashSet<String> = self.list_head()?.into_iter().map(|e| e.key).collect();
        let mut last: HashMap<String, Entry> = HashMap::new();
        for h in self.walk_history()? {
            last.insert(h.entry.key.clone(), h.entry);
        }
        Ok(last
            .into_values()
            .filter(|e| !alive.contains(&e.key) && e.kind != KIND_KNOWLEDGE)
            .collect())
    }

    /// Full history: every commit × changed .md files → one version per doc.
    /// A version identical in content to the key's previous one (the layout
    /// migration is a pure rename) is skipped — moves are not new knowledge.
    pub fn walk_history(&self) -> Result<Vec<HistoryDoc>> {
        let repo = self.repo()?;
        if repo.head().is_err() {
            return Ok(vec![]); // empty repo
        }
        let mut walk = repo.revwalk()?;
        walk.push_head()?;
        // topological order: same-second commits don't shuffle
        walk.set_sorting(Sort::TOPOLOGICAL | Sort::REVERSE)?;
        let mut out = vec![];
        let mut last_content: HashMap<String, Entry> = HashMap::new();
        for oid in walk {
            let oid = oid?;
            let commit = repo.find_commit(oid)?;
            let tree = commit.tree()?;
            let parent_tree = match commit.parent(0) {
                Ok(p) => Some(p.tree()?),
                Err(_) => None,
            };
            let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)?;
            for d in diff.deltas() {
                if !matches!(d.status(), Delta::Added | Delta::Modified) {
                    continue;
                }
                let Some(path) = d.new_file().path() else { continue };
                if path.extension().and_then(|x| x.to_str()) != Some("md") {
                    continue;
                }
                let Ok(te) = tree.get_path(path) else { continue };
                let Ok(obj) = te.to_object(&repo) else { continue };
                let Ok(blob) = obj.peel_to_blob() else { continue };
                let Ok(s) = std::str::from_utf8(blob.content()) else { continue };
                match Entry::from_markdown(s) {
                    Ok(entry) => {
                        if last_content.get(&entry.key).is_some_and(|prev| prev.same_content(&entry)) {
                            continue;
                        }
                        last_content.insert(entry.key.clone(), entry.clone());
                        out.push(HistoryDoc {
                            sha: oid.to_string(),
                            committed_at: commit.time().seconds(),
                            entry,
                        })
                    }
                    Err(err) => eprintln!("kyb: history skip {}@{oid}: {err:#}", path.display()),
                }
            }
        }
        Ok(out)
    }

    /// History of one key (deletions included), newest first.
    pub fn history(&self, key: &str) -> Result<Vec<VersionInfo>> {
        let repo = self.repo()?;
        if repo.head().is_err() {
            return Ok(vec![]);
        }
        let mut walk = repo.revwalk()?;
        walk.push_head()?;
        // topological: newest first, stable even for same-second commits
        walk.set_sorting(Sort::TOPOLOGICAL)?;
        let mut out = vec![];
        for oid in walk {
            let oid = oid?;
            let commit = repo.find_commit(oid)?;
            let tree = commit.tree()?;
            let parent_tree = match commit.parent(0) {
                Ok(p) => Some(p.tree()?),
                Err(_) => None,
            };
            let mut dopts = DiffOptions::new();
            // both layouts: the key's current path and its pre-migration one
            dopts.pathspec(Self::file_rel(key));
            dopts.pathspec(Self::legacy_rel(key));
            let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut dopts))?;
            let changes: Vec<&str> = diff
                .deltas()
                .filter_map(|d| match d.status() {
                    Delta::Added => Some("added"),
                    Delta::Modified => Some("modified"),
                    Delta::Deleted => Some("deleted"),
                    _ => None,
                })
                .collect();
            // added+deleted in one commit = the migration move, not a change
            if changes.contains(&"added") && changes.contains(&"deleted") {
                continue;
            }
            for change in changes {
                out.push(VersionInfo {
                    sha: oid.to_string(),
                    committed_at: iso(commit.time().seconds()),
                    message: commit.summary().unwrap_or("").to_string(),
                    change: change.into(),
                });
            }
        }
        Ok(out)
    }

    pub fn head_info(&self) -> Result<Option<(String, String)>> {
        let repo = self.repo()?;
        let out = match repo.head() {
            Err(_) => None,
            Ok(h) => {
                let c = h.peel_to_commit()?;
                Some((c.id().to_string(), iso(c.time().seconds())))
            }
        };
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str, body: &str) -> Entry {
        Entry {
            key: key.into(),
            title: format!("Title {key}"),
            tags: vec!["test".into()],
            body: body.into(),
            ..Default::default()
        }
    }

    #[test]
    fn upsert_get_history_delete() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();

        // create
        let out = store.upsert(entry("alpha", "first version"), "2026-07-19").unwrap();
        let sha1 = match out {
            UpsertOutcome::Created(c) => c.sha,
            _ => panic!("expected Created"),
        };
        assert_eq!(sha1.len(), 40);
        let got = store.get("alpha").unwrap().unwrap();
        assert_eq!(got.body, "first version");
        assert_eq!(got.updated_at, "2026-07-19");

        // no-op: same content, different date — no commit
        let out = store.upsert(entry("alpha", "first version"), "2026-07-20").unwrap();
        assert!(matches!(out, UpsertOutcome::Unchanged(_)));
        assert_eq!(store.get("alpha").unwrap().unwrap().updated_at, "2026-07-19");

        // update
        let out = store.upsert(entry("alpha", "second version"), "2026-07-20").unwrap();
        let sha2 = match out {
            UpsertOutcome::Updated(c) => c.sha,
            _ => panic!("expected Updated"),
        };
        assert_ne!(sha1, sha2);

        // history straight from git
        let hist = store.history("alpha").unwrap();
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].change, "modified");
        assert_eq!(hist[1].change, "added");

        // old version by sha
        let old = store.get_at("alpha", &sha1).unwrap().unwrap();
        assert_eq!(old.body, "first version");

        // walk_history sees both versions
        let docs = store.walk_history().unwrap();
        assert_eq!(docs.len(), 2);

        // delete: file gone, history stays
        assert!(store.delete("alpha").unwrap().is_some());
        assert!(store.get("alpha").unwrap().is_none());
        let hist = store.history("alpha").unwrap();
        assert_eq!(hist[0].change, "deleted");
        assert_eq!(store.walk_history().unwrap().len(), 2);
        assert!(store.list_head().unwrap().is_empty());
    }

    // 10 versions in a row: full history, every version reachable by its sha
    #[test]
    fn ten_versions_get_at_each() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let mut shas = vec![];
        for i in 0..10 {
            let out = store.upsert(entry("k", &format!("version {i}")), "2026-07-20").unwrap();
            let sha = match out {
                UpsertOutcome::Created(c) | UpsertOutcome::Updated(c) => c.sha,
                UpsertOutcome::Unchanged(_) => panic!("bodies differ, no-op must not happen"),
            };
            shas.push(sha);
        }
        assert_eq!(store.history("k").unwrap().len(), 10);
        assert_eq!(store.walk_history().unwrap().len(), 10);
        for (i, sha) in shas.iter().enumerate() {
            let e = store.get_at("k", sha).unwrap().unwrap();
            assert_eq!(e.body, format!("version {i}"), "sha {sha}");
        }
        // walk_history is chronological (oldest first)
        let bodies: Vec<String> = store.walk_history().unwrap().into_iter().map(|h| h.entry.body).collect();
        assert_eq!(bodies[0], "version 0");
        assert_eq!(bodies[9], "version 9");
    }

    #[test]
    fn fifty_keys_listing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        for i in 0..50 {
            store.upsert(entry(&format!("key-{i:02}"), &format!("body {i}")), "2026-07-20").unwrap();
        }
        assert_eq!(store.list_head().unwrap().len(), 50);
        assert_eq!(store.walk_history().unwrap().len(), 50);
        assert!(store.head_info().unwrap().is_some());
    }

    // meta-only changes (tags/refs/title) are changes, not no-ops
    #[test]
    fn meta_change_is_a_change() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store.upsert(entry("k", "body"), "2026-07-19").unwrap();

        let mut e = entry("k", "body");
        e.tags = vec!["new-tag".into()];
        assert!(matches!(store.upsert(e, "2026-07-20").unwrap(), UpsertOutcome::Updated(_)));

        let mut e = entry("k", "body");
        e.tags = vec!["new-tag".into()];
        e.refs = vec!["pointer".into()];
        assert!(matches!(store.upsert(e, "2026-07-20").unwrap(), UpsertOutcome::Updated(_)));

        let mut e = entry("k", "body");
        e.tags = vec!["new-tag".into()];
        e.refs = vec!["pointer".into()];
        e.title = "Different title".into();
        assert!(matches!(store.upsert(e, "2026-07-20").unwrap(), UpsertOutcome::Updated(_)));
        assert_eq!(store.history("k").unwrap().len(), 4);
    }

    // broken/foreign files inside kyb-data must not break list_head
    #[test]
    fn broken_and_foreign_files_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store.upsert(entry("good", "a proper entry"), "2026-07-20").unwrap();
        std::fs::write(dir.path().join("bad.md"), "garbage without frontmatter").unwrap();
        std::fs::write(dir.path().join("note.txt"), "not md at all").unwrap();
        let heads = store.list_head().unwrap();
        assert_eq!(heads.len(), 1);
        assert_eq!(heads[0].key, "good");
    }

    // a manual file edit (no commit) is visible in get/list_head — canon is live
    #[test]
    fn manual_edit_visible() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store.upsert(entry("k", "original body"), "2026-07-20").unwrap();
        let manual = store.get("k").unwrap().unwrap().to_markdown().replace("original", "hand-edited");
        std::fs::write(dir.path().join("knowledge/k.md"), manual).unwrap();
        assert!(store.get("k").unwrap().unwrap().body.contains("hand-edited"));
    }

    // legacy flat repos migrate themselves on open: files move into per-kind
    // dirs, closed entries are archived, history and old shas stay readable
    #[test]
    fn layout_migration_from_flat() {
        let dir = tempfile::tempdir().unwrap();
        // build a v1 (flat) canon by hand: knowledge + resolved incident at the root
        let legacy_sha = {
            let repo = Repository::init(dir.path()).unwrap();
            let know = entry("alpha", "the knowledge body");
            let mut inc = entry("inc-2026-07-20-oom", "it broke and was fixed");
            inc.kind = "incident".into();
            inc.service = "svc".into();
            inc.severity = "low".into();
            inc.status = "resolved".into();
            inc.resolution = "restarted the recorder".into();
            fs::write(dir.path().join("alpha.md"), know.to_markdown()).unwrap();
            fs::write(dir.path().join("inc-2026-07-20-oom.md"), inc.to_markdown()).unwrap();
            let mut idx = repo.index().unwrap();
            idx.add_path(Path::new("alpha.md")).unwrap();
            idx.add_path(Path::new("inc-2026-07-20-oom.md")).unwrap();
            idx.write().unwrap();
            let tree = repo.find_tree(idx.write_tree().unwrap()).unwrap();
            let sig = Signature::now("kyb", "kyb@local").unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "legacy layout", &tree, &[]).unwrap().to_string()
        };

        let store = Store::open(dir.path()).unwrap(); // migrates here
        // knowledge moved into its dir; the closed incident was archived
        assert!(dir.path().join("knowledge/alpha.md").is_file());
        assert!(!dir.path().join("alpha.md").exists());
        assert!(!dir.path().join("incidents/inc-2026-07-20-oom.md").exists());
        assert_eq!(store.get("alpha").unwrap().unwrap().body, "the knowledge body");
        assert!(store.get("inc-2026-07-20-oom").unwrap().is_none());
        // ...but its latest version is still readable and listed as archived
        let latest = store.latest_version("inc-2026-07-20-oom").unwrap().unwrap();
        assert_eq!(latest.resolution, "restarted the recorder");
        let archived = store.archived_latest().unwrap();
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0].key, "inc-2026-07-20-oom");
        // pre-migration shas resolve through the legacy path
        let old = store.get_at("alpha", &legacy_sha).unwrap().unwrap();
        assert_eq!(old.body, "the knowledge body");
        // the pure rename is not a new version: one content version per key
        assert_eq!(store.walk_history().unwrap().len(), 2);
        let hist = store.history("alpha").unwrap();
        assert_eq!(hist.len(), 1, "rename skipped: {hist:?}");
        assert_eq!(hist[0].change, "added");
        // reopening is idempotent — no new commits on a second open
        let head_before = store.head_info().unwrap();
        let store2 = Store::open(dir.path()).unwrap();
        assert_eq!(store2.head_info().unwrap(), head_before);
    }

    // archive and delete leave different traces in the log
    #[test]
    fn archive_and_delete_messages_differ() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store.upsert(entry("a", "x"), "2026-07-20").unwrap();
        store.upsert(entry("b", "y"), "2026-07-20").unwrap();
        store.archive("a").unwrap().unwrap();
        store.delete("b").unwrap().unwrap();
        assert!(store.history("a").unwrap()[0].message.contains("archive"));
        assert!(store.history("b").unwrap()[0].message.contains("delete"));
    }

    #[test]
    fn delete_readd_cycles() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        for cycle in 0..3 {
            store.upsert(entry("k", &format!("life {cycle}")), "2026-07-20").unwrap();
            store.delete("k").unwrap();
        }
        assert!(store.get("k").unwrap().is_none());
        let hist = store.history("k").unwrap();
        assert_eq!(hist.len(), 6);
        let changes: Vec<&str> = hist.iter().map(|v| v.change.as_str()).collect();
        assert_eq!(changes, vec!["deleted", "added", "deleted", "added", "deleted", "added"]);
        // all three lives intact in walk_history
        assert_eq!(store.walk_history().unwrap().len(), 3);
    }
}
