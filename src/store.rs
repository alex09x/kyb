use crate::model::{Entry, INCIDENT_PREFIX, KIND_KNOWLEDGE, TASK_PREFIX};
use anyhow::{Context, Result};
use git2::{Delta, Repository, Signature, Sort};
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// The agent, session and workspace the client named for this write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<crate::auth::By>,
}

/// One version of one key from history — the unit of indexing.
pub struct HistoryDoc {
    pub sha: String,
    pub committed_at: i64,
    pub entry: Entry,
}

/// A knowledge entry taken out of the base: the commit that removed its file.
/// Archiving an incident or task also removes a file, but that record stays
/// findable by design, so only knowledge is retracted. `after` is how many
/// history documents precede the retraction in walk order: the index places it
/// between versions by that position, not by the 1s-granular commit clock.
pub struct Retraction {
    pub key: String,
    pub committed_at: i64,
    pub after: usize,
}

/// Every key's versions, newest first, for the whole canon. Built by one walk
/// over history and then extended by the commits since - the repository is
/// never walked twice for the same commits, and a key's history is a lookup.
#[derive(Debug, Clone, Default)]
pub struct HistoryIndex {
    /// The commit this index describes; None for an empty repository.
    pub head: Option<String>,
    by_key: HashMap<String, Vec<VersionInfo>>,
}

impl HistoryIndex {
    pub fn versions(&self, key: &str) -> Vec<VersionInfo> {
        self.by_key.get(key).cloned().unwrap_or_default()
    }

    /// Every key that ever had a version, retracted ones included.
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.by_key.keys()
    }
}

/// The key a repository path belongs to: `<kind>/<key>.md` in the current
/// layout, `<key>.md` in the pre-migration one. Any other file is not an entry.
fn key_of_path(path: &str) -> Option<String> {
    let key = path.rsplit('/').next()?.strip_suffix(".md")?;
    if path == Store::file_rel(key) || path == Store::legacy_rel(key) {
        Some(key.to_string())
    } else {
        None
    }
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
        // The caller of the request being served, when it presented a token;
        // otherwise the service's own signature. Never the global git config.
        let (name, email) = crate::auth::signature();
        Ok(Signature::now(&name, &email)?)
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
        let msg = crate::auth::message_with_trailers(msg, crate::auth::by().as_ref());
        let oid = repo.commit(Some("HEAD"), &sig, &sig, &msg, &tree, &parents)?;
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

    /// Commit time of any revision git can resolve - a sha, a prefix, a tag.
    /// Lets `as_of` accept "the state at this commit" as well as a timestamp,
    /// which is the form an agent has to hand after a `history` call.
    pub fn rev_time(&self, rev: &str) -> Result<Option<i64>> {
        let repo = self.repo()?;
        let Ok(obj) = repo.revparse_single(rev) else {
            return Ok(None);
        };
        let Ok(commit) = obj.peel_to_commit() else {
            return Ok(None);
        };
        Ok(Some(commit.time().seconds()))
    }

    /// Commit details (sha, time, author, message) of any resolvable git revision.
    pub fn commit_details(&self, rev: &str) -> Result<Option<(String, i64, String, String)>> {
        let repo = self.repo()?;
        let Ok(obj) = repo.revparse_single(rev) else {
            return Ok(None);
        };
        let Ok(commit) = obj.peel_to_commit() else {
            return Ok(None);
        };
        let sha = commit.id().to_string();
        let time = commit.time().seconds();
        let who = commit.author();
        let author = crate::auth::shown_author(who.name().unwrap_or_default(), who.email().unwrap_or_default());
        let msg = commit.summary().unwrap_or("").to_string();
        Ok(Some((sha, time, author, msg)))
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
        let msg = crate::auth::message_with_trailers(msg, crate::auth::by().as_ref());
        let oid = repo.commit(Some("HEAD"), &sig, &sig, &msg, &tree, &parents)?;
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
        Ok(self.walk_history_and_retractions()?.0)
    }

    /// Every version of every key in chronological order, and every retraction
    /// of a knowledge entry, each placed after the versions that precede it.
    pub fn walk_history_and_retractions(&self) -> Result<(Vec<HistoryDoc>, Vec<Retraction>)> {
        let repo = self.repo()?;
        if repo.head().is_err() {
            return Ok((vec![], vec![])); // empty repo
        }
        let mut walk = repo.revwalk()?;
        walk.push_head()?;
        // topological order: same-second commits don't shuffle
        walk.set_sorting(Sort::TOPOLOGICAL | Sort::REVERSE)?;
        let mut out = vec![];
        let mut retractions = vec![];
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
            // keys whose file this commit removed, and keys it (re)wrote: a key in
            // both moved between layouts and was not retracted
            let mut gone: Vec<String> = vec![];
            let mut written: HashSet<String> = HashSet::new();
            for d in diff.deltas() {
                if d.status() == Delta::Deleted {
                    if let Some(k) = d.old_file().path().and_then(|p| p.to_str()).and_then(key_of_path) {
                        gone.push(k);
                    }
                    continue;
                }
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
                        written.insert(entry.key.clone());
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
            for key in gone {
                if written.contains(&key) {
                    continue;
                }
                // forgotten here too: written again later, the entry is a new
                // version even when its text is the same as before the retraction
                let Some(prev) = last_content.remove(&key) else { continue };
                if prev.kind != KIND_KNOWLEDGE {
                    continue;
                }
                retractions.push(Retraction {
                    key,
                    committed_at: commit.time().seconds(),
                    after: out.len(),
                });
            }
        }
        Ok((out, retractions))
    }

    /// The history of every key at HEAD. With `prev`, only the commits since
    /// `prev.head` are walked and their versions put in front of what `prev`
    /// knew; a `prev` whose head is no longer in this history (the canon was
    /// rewritten by hand while the service was down) is discarded and the walk
    /// starts over. Walking a commit means one tree diff against its parent,
    /// no blob is read.
    pub fn history_index(&self, prev: Option<&HistoryIndex>) -> Result<HistoryIndex> {
        let repo = self.repo()?;
        let Ok(head) = repo.head() else {
            return Ok(HistoryIndex::default());
        };
        let head_oid = head.peel_to_commit()?.id();
        let base = prev
            .and_then(|p| p.head.as_deref())
            .and_then(|h| git2::Oid::from_str(h).ok())
            .filter(|&old| old == head_oid || repo.graph_descendant_of(head_oid, old).unwrap_or(false));
        if base == Some(head_oid) {
            return Ok(prev.expect("a base comes from prev").clone());
        }
        let mut walk = repo.revwalk()?;
        walk.push(head_oid)?;
        if let Some(old) = base {
            walk.hide(old)?;
        }
        // topological: newest first, stable even for same-second commits
        walk.set_sorting(Sort::TOPOLOGICAL)?;
        let mut fresh: HashMap<String, Vec<VersionInfo>> = HashMap::new();
        for oid in walk {
            let oid = oid?;
            let commit = repo.find_commit(oid)?;
            let tree = commit.tree()?;
            let parent_tree = match commit.parent(0) {
                Ok(p) => Some(p.tree()?),
                Err(_) => None,
            };
            let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)?;
            let mut changes: Vec<(String, &str)> = vec![];
            for d in diff.deltas() {
                let change = match d.status() {
                    Delta::Added => "added",
                    Delta::Modified => "modified",
                    Delta::Deleted => "deleted",
                    _ => continue,
                };
                let file = if change == "deleted" { d.old_file() } else { d.new_file() };
                let Some(path) = file.path().and_then(|p| p.to_str()) else { continue };
                if let Some(key) = key_of_path(path) {
                    changes.push((key, change));
                }
            }
            // a key both added and deleted in one commit moved between layouts: not a change to it
            let moved: HashSet<&str> = changes
                .iter()
                .filter(|(k, c)| *c == "added" && changes.iter().any(|(k2, c2)| k2 == k && *c2 == "deleted"))
                .map(|(k, _)| k.as_str())
                .collect();
            let who = commit.author();
            let author = crate::auth::shown_author(who.name().unwrap_or_default(), who.email().unwrap_or_default());
            let by = crate::auth::By::from_message(commit.message().unwrap_or_default());
            for (key, change) in &changes {
                if moved.contains(key.as_str()) {
                    continue;
                }
                fresh.entry(key.clone()).or_default().push(VersionInfo {
                    sha: oid.to_string(),
                    committed_at: iso(commit.time().seconds()),
                    message: commit.summary().unwrap_or("").to_string(),
                    change: (*change).into(),
                    author: Some(author.clone()),
                    by: by.clone(),
                });
            }
        }
        let mut by_key = match (base, prev) {
            (Some(_), Some(p)) => p.by_key.clone(),
            _ => HashMap::new(),
        };
        for (key, mut versions) in fresh {
            let older = by_key.remove(&key).unwrap_or_default();
            versions.extend(older);
            by_key.insert(key, versions);
        }
        Ok(HistoryIndex { head: Some(head_oid.to_string()), by_key })
    }

    /// History of one key (deletions included), newest first - the index's
    /// answer; the service keeps one index and asks it, this is for tests.
    #[cfg(test)]
    pub fn history(&self, key: &str) -> Result<Vec<VersionInfo>> {
        Ok(self.history_index(None)?.versions(key))
    }

    /// Recent commits across the entire database canon with document titles and actions.
    pub fn recent_commits(&self, limit: usize) -> Result<Vec<ActivityEvent>> {
        let repo = self.repo()?;
        if repo.head().is_err() {
            return Ok(vec![]);
        }
        let mut walk = repo.revwalk()?;
        walk.push_head()?;
        walk.set_sorting(Sort::TOPOLOGICAL)?;
        let mut out = vec![];
        for oid in walk.take(limit) {
            let oid = oid?;
            let commit = repo.find_commit(oid)?;
            let tree = commit.tree()?;
            let parent_tree = match commit.parent(0) {
                Ok(p) => Some(p.tree()?),
                Err(_) => None,
            };
            let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)?;
            let mut files = vec![];
            let mut detected_key = None;
            let mut action = "updated".to_string();
            for d in diff.deltas() {
                if let Some(path) = d.new_file().path().and_then(|p| p.to_str()) {
                    files.push(path.to_string());
                    if detected_key.is_none() {
                        if let Some(filename) = path.rsplit('/').next() {
                            if let Some(k) = filename.strip_suffix(".md") {
                                detected_key = Some(k.to_string());
                            }
                        }
                    }
                }
                match d.status() {
                    Delta::Added => action = "created".to_string(),
                    Delta::Deleted => action = "archived".to_string(),
                    _ => {}
                }
            }
            let msg = commit.summary().unwrap_or("").trim().to_string();
            if msg.contains("resolve") {
                action = "resolved".to_string();
            } else if msg.contains("delete") {
                action = "retracted".to_string();
            }

            let who = commit.author();
            let author = crate::auth::shown_author(who.name().unwrap_or_default(), who.email().unwrap_or_default());

            let time_sec = commit.time().seconds();
            let ts = iso(time_sec);

            let (kind, title) = if let Some(k) = &detected_key {
                let kind_str = if k.starts_with(INCIDENT_PREFIX) {
                    "incident"
                } else if k.starts_with(TASK_PREFIX) {
                    "task"
                } else {
                    "knowledge"
                };
                let doc_title = self.get_at(k, &oid.to_string()).ok().flatten().map(|e| e.title);
                (kind_str.to_string(), doc_title.or_else(|| Some(k.clone())))
            } else {
                ("system".to_string(), Some("Fleet Infrastructure Canon".to_string()))
            };

            out.push(ActivityEvent {
                sha: oid.to_string(),
                ts,
                author,
                by: crate::auth::By::from_message(commit.message().unwrap_or_default()),
                message: msg,
                key: detected_key,
                kind,
                title,
                action,
                files,
            });
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

#[derive(Debug, Clone, serde::Serialize)]
pub struct ActivityEvent {
    pub sha: String,
    pub ts: String,
    pub author: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<crate::auth::By>,
    pub message: String,
    pub key: Option<String>,
    pub kind: String,
    pub title: Option<String>,
    pub action: String,
    pub files: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_index_extends_instead_of_rewalking() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let empty = store.history_index(None).unwrap();
        assert!(empty.head.is_none() && empty.versions("alpha").is_empty());

        store.upsert(entry("alpha", "one"), "2026-07-19").unwrap();
        store.upsert(entry("beta", "one"), "2026-07-19").unwrap();
        let first = store.history_index(None).unwrap();
        assert_eq!(first.versions("alpha").len(), 1);
        assert_eq!(first.versions("beta")[0].change, "added");
        assert!(first.versions("nobody").is_empty());

        // the same head: the previous index is handed back untouched
        let same = store.history_index(Some(&first)).unwrap();
        assert_eq!(same.head, first.head);

        store.upsert(entry("alpha", "two"), "2026-07-20").unwrap();
        store.delete("beta").unwrap();
        let extended = store.history_index(Some(&first)).unwrap();
        let rebuilt = store.history_index(None).unwrap();
        assert_eq!(extended.head, rebuilt.head);
        assert_ne!(extended.head, first.head);
        for key in ["alpha", "beta", "nobody"] {
            let (e, r): (Vec<_>, Vec<_>) = (extended.versions(key), rebuilt.versions(key));
            assert_eq!(e.len(), r.len(), "{key}");
            for (x, y) in e.iter().zip(r.iter()) {
                assert_eq!((&x.sha, &x.change, &x.message), (&y.sha, &y.change, &y.message), "{key}");
            }
        }
        let alpha = extended.versions("alpha");
        assert_eq!((alpha[0].change.as_str(), alpha[1].change.as_str()), ("modified", "added"), "newest first");
        assert_eq!(extended.versions("beta")[0].change, "deleted");
        let mut keys: Vec<&String> = extended.keys().collect();
        keys.sort();
        assert_eq!(keys, ["alpha", "beta"], "a retracted key is still a key that was used");
        assert_eq!(store.history("alpha").unwrap().len(), 2, "the per-key history is the index's answer");

        // a previous index whose head this history does not contain is not trusted
        let foreign = HistoryIndex { head: Some("0".repeat(40)), by_key: HashMap::new() };
        let again = store.history_index(Some(&foreign)).unwrap();
        assert_eq!(again.versions("alpha").len(), 2);

        // deleted in one commit and filed again in the next: two versions, not a move
        store.upsert(entry("beta", "back"), "2026-07-21").unwrap();
        let back = store.history_index(Some(&extended)).unwrap();
        let beta: Vec<String> = back.versions("beta").into_iter().map(|v| v.change).collect();
        assert_eq!(beta, ["added", "deleted", "added"]);

        // a move between layouts (the key removed at one path and added at the other in
        // one commit) is not a version, on the incremental path as on the full walk
        let repo = store.repo().unwrap();
        let mut idx = repo.index().unwrap();
        let (from, to) = (Store::file_rel("alpha"), Store::legacy_rel("alpha"));
        fs::rename(dir.path().join(&from), dir.path().join(&to)).unwrap();
        idx.remove_path(Path::new(&from)).unwrap();
        idx.add_path(Path::new(&to)).unwrap();
        store.commit_index(&repo, &mut idx, "move alpha to the flat layout").unwrap();
        let moved = store.history_index(Some(&back)).unwrap();
        assert_ne!(moved.head, back.head, "the move is a commit");
        assert_eq!(moved.versions("alpha").len(), 2, "but not a version of alpha");
        assert_eq!(moved.versions("alpha").len(), store.history_index(None).unwrap().versions("alpha").len());
    }

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
