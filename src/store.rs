use crate::model::Entry;
use anyhow::{Context, Result};
use git2::{Delta, DiffOptions, Repository, Signature, Sort};
use std::fs;
use std::path::{Path, PathBuf};

/// The `kyb-data/` git repo is the single source of truth. A Repository is
/// opened per operation (cheap, and sidesteps Send/Sync questions); parallel
/// writes are serialized by the shared write mutex in AppState.
pub struct Store {
    root: PathBuf,
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
        Ok(Store { root: root.to_path_buf() })
    }

    fn repo(&self) -> Result<Repository> {
        Ok(Repository::open(&self.root)?)
    }

    fn sig() -> Result<Signature<'static>> {
        // fixed signature — no dependency on global git config
        Ok(Signature::now("kyb", "kyb@local")?)
    }

    fn file_name(key: &str) -> String {
        format!("{key}.md")
    }

    fn file_path(&self, key: &str) -> PathBuf {
        self.root.join(Self::file_name(key))
    }

    pub fn get(&self, key: &str) -> Result<Option<Entry>> {
        let p = self.file_path(key);
        if !p.exists() {
            return Ok(None);
        }
        let s = fs::read_to_string(&p)?;
        Ok(Some(Entry::from_markdown(&s).with_context(|| format!("corrupt file {key}.md"))?))
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
        let Some(te) = tree.get_name(&Self::file_name(key)) else {
            return Ok(None);
        };
        let blob = te.to_object(&repo)?.peel_to_blob()?;
        let s = std::str::from_utf8(blob.content()).context("file is not utf-8")?;
        Ok(Some(Entry::from_markdown(s)?))
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
        fs::write(self.file_path(&entry.key), entry.to_markdown())?;
        let (sha, committed_at) = self.commit_path(
            &Self::file_name(&entry.key),
            &format!("kyb: upsert {}", entry.key),
            false,
        )?;
        let c = Committed { sha, committed_at, entry };
        Ok(if existing.is_some() { UpsertOutcome::Updated(c) } else { UpsertOutcome::Created(c) })
    }

    pub fn delete(&self, key: &str) -> Result<Option<String>> {
        let p = self.file_path(key);
        if !p.exists() {
            return Ok(None);
        }
        fs::remove_file(&p)?;
        let (sha, _) = self.commit_path(&Self::file_name(key), &format!("kyb: delete {key}"), true)?;
        Ok(Some(sha))
    }

    pub fn list_head(&self) -> Result<Vec<Entry>> {
        let mut out = vec![];
        for e in fs::read_dir(&self.root)? {
            let p = e?.path();
            if p.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let s = fs::read_to_string(&p)?;
            match Entry::from_markdown(&s) {
                Ok(en) => out.push(en),
                Err(err) => eprintln!("kyb: skipping {}: {err:#}", p.display()),
            }
        }
        Ok(out)
    }

    /// Full history: every commit × changed .md files → one version per doc.
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
                    Ok(entry) => out.push(HistoryDoc {
                        sha: oid.to_string(),
                        committed_at: commit.time().seconds(),
                        entry,
                    }),
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
        let rel = Self::file_name(key);
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
            dopts.pathspec(&rel);
            let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut dopts))?;
            for d in diff.deltas() {
                let change = match d.status() {
                    Delta::Added => "added",
                    Delta::Modified => "modified",
                    Delta::Deleted => "deleted",
                    _ => continue,
                };
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
        std::fs::write(dir.path().join("k.md"), manual).unwrap();
        assert!(store.get("k").unwrap().unwrap().body.contains("hand-edited"));
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
