/*
 * KYB (Know Your Business) — git-backed knowledge base and incident tracker for AI agent fleets
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/kyb
 * SPDX-License-Identifier: MIT
 */

//! Who is asking, and what they may do.
//!
//! A caller presents a bearer token; the server looks its SHA-256 up in a
//! book of principals kept OUTSIDE the knowledge repository (the roster and
//! the hashes are not knowledge, and git never forgets). A token is 256 random
//! bits, so a plain hash is the right lookup - there is no password to stretch.
//!
//! Three roles, each including the one before: `read` (search, read, history),
//! `write` (add and edit entries, file, resolve and archive incidents and
//! tasks), `admin` (retract knowledge, reindex). Tokens are issued and revoked
//! with `kyb-server auth ...` on the machine that holds the book - access to
//! that file is the root of trust, so nobody can lock themselves out.
//!
//! The commit a write produces is authored by the token's principal. That is
//! attribution, not proof: anyone with direct access to the repository can
//! write any author they like.

use axum::extract::{Request, State};
use axum::http::{header, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Read,
    Write,
    Admin,
}

impl Role {
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "read" => Some(Role::Read),
            "write" => Some(Role::Write),
            "admin" => Some(Role::Admin),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Role::Read => "read",
            Role::Write => "write",
            Role::Admin => "admin",
        }
    }
}

/// `off`: nobody is asked for anything (the service trusts its network).
/// `compat`: a token is honoured when sent, and a caller without one keeps
/// full access - for rolling tokens out, on a trusted bind only. It protects
/// nothing: leaving the token out is all it takes.
/// `required`: no token, no access (or the anonymous role, if one is set).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Off,
    Compat,
    Required,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Actor {
    pub id: String,
    pub name: String,
    pub email: String,
    pub role: Role,
}

/// The caller of the request being served, as the handlers see it.
#[derive(Debug, Clone)]
pub struct Session {
    pub actor: Option<Actor>,
    pub role: Role,
}

tokio::task_local! {
    static SESSION: Session;
}

/// The effective role of the request being served. Outside a request (start-up
/// migration, tests that call the store directly) nothing is restricted.
pub fn role() -> Role {
    SESSION.try_with(|s| s.role).unwrap_or(Role::Admin)
}

/// Name and e-mail for the commit being made. Without an identified caller the
/// commit keeps the service's own signature and is shown as unattributed.
pub fn signature() -> (String, String) {
    SESSION
        .try_with(|s| s.actor.as_ref().map(|a| (a.name.clone(), a.email.clone())))
        .ok()
        .flatten()
        .unwrap_or_else(|| (SERVICE_NAME.to_string(), SERVICE_EMAIL.to_string()))
}

pub const SERVICE_NAME: &str = "kyb";
pub const SERVICE_EMAIL: &str = "kyb@local";

/// How a commit's author is shown: `Name <email>`, or nothing for a commit the
/// service signed itself (made before authorship, or by a caller without a token).
pub fn shown_author(name: &str, email: &str) -> String {
    // only the service's own full signature is nobody: a principal may be called anything
    if name.is_empty() || (name == SERVICE_NAME && email == SERVICE_EMAIL) {
        String::new()
    } else if email.is_empty() {
        name.to_string()
    } else {
        format!("{name} <{email}>")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TokenRec {
    pub id: String,
    pub sha256: String,
    #[serde(default)]
    pub label: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Principal {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub email: String,
    pub role: Role,
    /// A revoked principal stays in the book: its name still explains old
    /// commits, it just cannot sign in.
    #[serde(default)]
    pub revoked: bool,
    #[serde(default)]
    pub tokens: Vec<TokenRec>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Book {
    #[serde(default)]
    pub principals: Vec<Principal>,
}

pub fn hash_token(token: &str) -> String {
    Sha256::digest(token.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

impl Book {
    pub fn load(path: &Path) -> anyhow::Result<Book> {
        match std::fs::read_to_string(path) {
            Ok(s) if s.trim().is_empty() => Ok(Book::default()),
            Ok(s) => Ok(serde_json::from_str(&s)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Book::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Written whole to a temporary file of its own and renamed over the old
    /// one, readable by its owner only. Callers that read, change and write
    /// use `Book::update`, which holds the lock across all three.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        // a name of its own per write: two writers never share a temp file
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
        let tmp = path.with_file_name(format!("{}.{}.{nanos}.tmp", path.file_name().and_then(|n| n.to_str()).unwrap_or("auth.json"), std::process::id()));
        // created owner-only from the first byte: the roster is private even before the rename
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        {
            use std::io::Write;
            let mut f = opts.open(&tmp)?;
            f.write_all((serde_json::to_string_pretty(self)? + "\n").as_bytes())?;
            f.sync_all()?;
        }
        if let Err(e) = std::fs::rename(&tmp, path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        Ok(())
    }

    /// Read, change, write - under a lock, so two commands editing the book at
    /// the same time do not lose each other's change. The lock is a sibling
    /// file created exclusively; one left behind by a killed process is taken
    /// over after a few seconds.
    pub fn update<T>(path: &Path, change: impl FnOnce(&mut Book) -> anyhow::Result<T>) -> anyhow::Result<T> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let lock = path.with_file_name(format!("{}.lock", path.file_name().and_then(|n| n.to_str()).unwrap_or("auth.json")));
        let stale = std::time::Duration::from_secs(5);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&lock) {
                Ok(_) => break,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let age = std::fs::metadata(&lock).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
                    if age.map(|a| a > stale).unwrap_or(true) {
                        let _ = std::fs::remove_file(&lock);
                        continue;
                    }
                    anyhow::ensure!(std::time::Instant::now() < deadline, "the book {} is locked by another command ({})", path.display(), lock.display());
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(e) => return Err(e.into()),
            }
        }
        let result = (|| {
            let mut book = Book::load(path)?;
            let out = change(&mut book)?;
            book.save(path)?;
            Ok(out)
        })();
        let _ = std::fs::remove_file(&lock);
        result
    }

    fn new_token() -> anyhow::Result<(String, TokenRec)> {
        let mut raw = [0u8; 32];
        getrandom::getrandom(&mut raw).map_err(|e| anyhow::anyhow!("no randomness: {e}"))?;
        let token = format!("kyb_{}", raw.iter().map(|b| format!("{b:02x}")).collect::<String>());
        let sha256 = hash_token(&token);
        let rec = TokenRec { id: format!("t_{}", &sha256[..8]), sha256, label: String::new(), created_at: now(), revoked_at: None };
        Ok((token, rec))
    }

    /// A new principal with its first token. The token is returned once and
    /// kept nowhere.
    pub fn add(&mut self, id: &str, name: &str, email: &str, role: Role, label: &str) -> anyhow::Result<String> {
        anyhow::ensure!(valid_id(id), "id must be 1-64 of [A-Za-z0-9._-]");
        anyhow::ensure!(!name.trim().is_empty(), "a name is needed: it is what the history shows");
        anyhow::ensure!(!self.principals.iter().any(|p| p.id == id), "'{id}' already exists; use `auth token --id {id}` for another token");
        let (token, mut rec) = Book::new_token()?;
        rec.label = label.to_string();
        self.principals.push(Principal { id: id.into(), name: name.trim().into(), email: email.trim().into(), role, revoked: false, tokens: vec![rec] });
        Ok(token)
    }

    /// Another token for an existing principal: the old ones keep working until
    /// they are revoked, so a rotation has an overlap.
    pub fn issue(&mut self, id: &str, label: &str) -> anyhow::Result<String> {
        let p = self.principals.iter_mut().find(|p| p.id == id).ok_or_else(|| anyhow::anyhow!("no principal '{id}'"))?;
        anyhow::ensure!(!p.revoked, "'{id}' is revoked");
        let (token, mut rec) = Book::new_token()?;
        rec.label = label.to_string();
        p.tokens.push(rec);
        Ok(token)
    }

    /// Returns how many tokens stopped working.
    pub fn revoke_token(&mut self, token_id: &str) -> usize {
        let mut n = 0;
        for t in self.principals.iter_mut().flat_map(|p| p.tokens.iter_mut()) {
            if t.id == token_id && t.revoked_at.is_none() {
                t.revoked_at = Some(now());
                n += 1;
            }
        }
        n
    }

    pub fn revoke_principal(&mut self, id: &str) -> bool {
        match self.principals.iter_mut().find(|p| p.id == id && !p.revoked) {
            None => false,
            Some(p) => {
                p.revoked = true;
                let at = now();
                for t in p.tokens.iter_mut().filter(|t| t.revoked_at.is_none()) {
                    t.revoked_at = Some(at.clone());
                }
                true
            }
        }
    }

    pub fn lookup(&self, token: &str) -> Option<Actor> {
        let h = hash_token(token);
        self.principals.iter().filter(|p| !p.revoked).find_map(|p| {
            p.tokens.iter().any(|t| t.revoked_at.is_none() && t.sha256 == h).then(|| Actor {
                id: p.id.clone(),
                name: p.name.clone(),
                email: if p.email.is_empty() { format!("{}@kyb", p.id) } else { p.email.clone() },
                role: p.role,
            })
        })
    }
}

pub struct Auth {
    pub mode: Mode,
    /// What a caller without a token may do in `required` mode. None = nothing.
    pub anon: Option<Role>,
    path: PathBuf,
    book: RwLock<(Option<SystemTime>, Arc<Book>)>,
}

/// What a route asks of its caller.
#[derive(Debug, PartialEq)]
pub enum Need {
    Nothing,
    Role(Role),
}

pub fn need(method: &Method, path: &str) -> Need {
    if path == "/" || path == "/healthz" || path == "/whoami" {
        return Need::Nothing;
    }
    // The raw request log holds client addresses and other callers' queries.
    if path == "/api/audit" {
        return Need::Role(Role::Admin);
    }
    if method == Method::GET || method == Method::HEAD || method == Method::OPTIONS {
        return Need::Role(Role::Read);
    }
    if path == "/reindex" {
        return Need::Role(Role::Admin);
    }
    Need::Role(Role::Write)
}

#[derive(Debug, PartialEq)]
pub enum Refusal {
    /// 401: no token where one is needed, or a token nobody issued.
    Unknown(&'static str),
    /// 403: a known caller whose role is not enough.
    Forbidden { have: Role, need: Role },
}

impl Auth {
    pub fn new(mode: Mode, anon: Option<Role>, path: PathBuf) -> Auth {
        Auth { mode, anon, path, book: RwLock::new((None, Arc::new(Book::default()))) }
    }

    #[cfg(test)]
    pub fn off() -> Auth {
        Auth::new(Mode::Off, None, PathBuf::new())
    }

    /// `KYB_AUTH` = off | compat | required (default off), `KYB_AUTH_FILE`
    /// (default ./auth.json - beside the data, never inside it),
    /// `KYB_ANON` = read (required mode only: what a caller without a token may do).
    pub fn from_env() -> anyhow::Result<Auth> {
        let mode = match std::env::var("KYB_AUTH").unwrap_or_default().trim() {
            "" | "off" => Mode::Off,
            "compat" => Mode::Compat,
            "required" => Mode::Required,
            other => anyhow::bail!("KYB_AUTH must be off, compat or required, not '{other}'"),
        };
        let anon = match std::env::var("KYB_ANON").unwrap_or_default().trim() {
            "" => None,
            "read" => Some(Role::Read),
            other => anyhow::bail!("KYB_ANON can only be 'read', not '{other}': an anonymous writer is no author"),
        };
        let path = book_path();
        if mode != Mode::Off {
            let canon = std::path::PathBuf::from(std::env::var("KYB_DATA").unwrap_or_else(|_| "./kyb-data".into()));
            anyhow::ensure!(!inside(&path, &canon), "KYB_AUTH_FILE {} is inside the git canon {}: the book would be committed and shared; put it beside the data, not in it", path.display(), canon.display());
        }
        Ok(Auth::new(mode, anon, path))
    }

    /// One line for the start-up log, so an operator sees what is in force
    /// instead of assuming it - and a warning when the setting cannot work.
    pub fn describe(&self) -> String {
        let n = self.book().principals.iter().filter(|p| !p.revoked).count();
        match (self.mode, self.anon) {
            (Mode::Off, _) => "auth off - anyone who can reach this address can read and write".to_string(),
            (Mode::Compat, _) => format!("auth compat - tokens honoured, callers without one keep full access ({n} principals in {}); for a trusted bind only", self.path.display()),
            (Mode::Required, anon) => format!(
                "auth required - {n} principals in {}{}{}",
                self.path.display(),
                if anon.is_some() { ", callers without a token may read" } else { "" },
                if n == 0 { " - WARNING: the book is empty or missing, nobody can write; issue a token with `kyb-server auth add`" } else { "" },
            ),
        }
    }

    /// The book as it is on disk now: re-read when the file changed, so a token
    /// issued or revoked from the command line takes effect without a restart.
    fn book(&self) -> Arc<Book> {
        let mtime = std::fs::metadata(&self.path).and_then(|m| m.modified()).ok();
        {
            let held = self.book.read().unwrap();
            if held.0 == mtime {
                return held.1.clone();
            }
        }
        match Book::load(&self.path) {
            Ok(b) => {
                let b = Arc::new(b);
                *self.book.write().unwrap() = (mtime, b.clone());
                b
            }
            Err(e) => {
                // an unreadable book admits nobody rather than everybody
                eprintln!("kyb: auth book {} cannot be read: {e:#}", self.path.display());
                Arc::new(Book::default())
            }
        }
    }

    /// Decide one request. `bearer` is the Authorization header as sent.
    pub fn decide(&self, method: &Method, path: &str, bearer: Option<&str>) -> Result<Session, Refusal> {
        if self.mode == Mode::Off {
            return Ok(Session { actor: None, role: Role::Admin });
        }
        let token = bearer.and_then(|h| h.strip_prefix("Bearer ").or_else(|| h.strip_prefix("bearer "))).map(str::trim).filter(|t| !t.is_empty());
        let wanted = need(method, path);
        let session = match token {
            Some(t) => match self.book().lookup(t) {
                Some(actor) => Session { role: actor.role, actor: Some(actor) },
                // the page itself must load for the person to enter a new token
                None if path == "/" => Session { actor: None, role: Role::Read },
                None => return Err(Refusal::Unknown("this token is not known here, or was revoked")),
            },
            None => match (self.mode, self.anon) {
                (Mode::Compat, _) => Session { actor: None, role: Role::Admin },
                (_, Some(role)) => Session { actor: None, role },
                _ if wanted == Need::Nothing => Session { actor: None, role: Role::Read },
                _ => return Err(Refusal::Unknown("a token is required: send `Authorization: Bearer <token>`")),
            },
        };
        match wanted {
            Need::Role(need) if session.role < need => Err(Refusal::Forbidden { have: session.role, need }),
            _ => Ok(session),
        }
    }
}

/// Whether `path` is `dir` or under it, by the paths as given (both resolved
/// against the same working directory), so the check needs neither to exist.
fn inside(path: &Path, dir: &Path) -> bool {
    let norm = |p: &Path| -> PathBuf {
        let abs = if p.is_absolute() { p.to_path_buf() } else { std::env::current_dir().map(|c| c.join(p)).unwrap_or_else(|_| p.to_path_buf()) };
        let mut out = PathBuf::new();
        for c in abs.components() {
            match c {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => { out.pop(); }
                other => out.push(other.as_os_str()),
            }
        }
        out
    };
    norm(path).starts_with(norm(dir))
}

pub fn book_path() -> PathBuf {
    std::env::var("KYB_AUTH_FILE").unwrap_or_else(|_| "./auth.json".into()).into()
}

/// Who made the request, for the audit log (set on the response by the middleware).
#[derive(Clone)]
pub struct ActorTag(pub String);

pub fn refusal_response(r: &Refusal) -> Response {
    match r {
        Refusal::Unknown(why) => (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            Json(json!({"error": why})),
        )
            .into_response(),
        Refusal::Forbidden { have, need } => (
            StatusCode::FORBIDDEN,
            Json(json!({"error": format!("this needs the {} role; yours is {}", need.name(), have.name()), "role": have.name(), "needs": need.name()})),
        )
            .into_response(),
    }
}

pub async fn auth_mw(State(st): State<Arc<crate::AppState>>, req: Request, next: Next) -> Response {
    let bearer = req.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).map(str::to_string);
    match st.auth.decide(req.method(), req.uri().path(), bearer.as_deref()) {
        Err(refusal) => {
            let mut resp = refusal_response(&refusal);
            resp.extensions_mut().insert(ActorTag("refused".into()));
            resp
        }
        Ok(session) => {
            let tag = ActorTag(session.actor.as_ref().map(|a| a.id.clone()).unwrap_or_else(|| "-".into()));
            let mut resp = SESSION.scope(session, next.run(req)).await;
            resp.extensions_mut().insert(tag);
            resp
        }
    }
}

/// `GET /whoami`: what the page needs to know to show the right controls.
pub async fn whoami(State(st): State<Arc<crate::AppState>>) -> Json<serde_json::Value> {
    let (actor, role) = SESSION.try_with(|s| (s.actor.clone(), s.role)).unwrap_or((None, Role::Admin));
    Json(json!({
        "auth": match st.auth.mode { Mode::Off => "off", Mode::Compat => "compat", Mode::Required => "required" },
        "role": role.name(),
        "actor": actor.map(|a| json!({"id": a.id, "name": a.name})),
    }))
}

/// `kyb-server auth ...`, run on the machine that holds the book.
pub fn cli(args: &[String]) -> anyhow::Result<String> {
    let path = book_path();
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    match args.first().map(String::as_str) {
        Some("add") => {
            let id = flag("--id").ok_or_else(|| anyhow::anyhow!("--id is needed"))?;
            let name = flag("--name").ok_or_else(|| anyhow::anyhow!("--name is needed"))?;
            let role = flag("--role").as_deref().and_then(Role::parse).ok_or_else(|| anyhow::anyhow!("--role read|write|admin is needed"))?;
            let token = Book::update(&path, |book| book.add(&id, &name, &flag("--email").unwrap_or_default(), role, &flag("--label").unwrap_or_default()))?;
            Ok(format!("{token}\n(shown once; {} has the {} role; the book is {})", id, role.name(), path.display()))
        }
        Some("token") => {
            let id = flag("--id").ok_or_else(|| anyhow::anyhow!("--id is needed"))?;
            let token = Book::update(&path, |book| book.issue(&id, &flag("--label").unwrap_or_default()))?;
            Ok(format!("{token}\n(shown once; earlier tokens of {id} keep working until revoked)"))
        }
        Some("revoke") => {
            let which = match (flag("--token"), flag("--id")) {
                (Some(t), None) => Ok(t),
                (None, Some(id)) => Err(id),
                _ => anyhow::bail!("pass exactly one of --token T_ID or --id ID"),
            };
            Book::update(&path, |book| match &which {
                Ok(t) => match book.revoke_token(t) {
                    0 => anyhow::bail!("no live token '{t}'"),
                    _ => Ok(format!("token {t} revoked")),
                },
                Err(id) => {
                    anyhow::ensure!(book.revoke_principal(id), "no live principal '{id}'");
                    Ok(format!("{id} revoked with all its tokens; its name stays in the history"))
                }
            })
        }
        Some("list") => {
            let book = Book::load(&path)?;
            let mut lines = vec![];
            for p in &book.principals {
                lines.push(format!("{}  {}  {}{}", p.id, p.role.name(), p.name, if p.revoked { "  (revoked)" } else { "" }));
                for t in &p.tokens {
                    lines.push(format!("    {}  {}  {}{}", t.id, t.created_at, if t.label.is_empty() { "-" } else { &t.label }, if t.revoked_at.is_some() { "  (revoked)" } else { "" }));
                }
            }
            Ok(if lines.is_empty() { format!("no principals in {}", path.display()) } else { lines.join("\n") })
        }
        _ => anyhow::bail!("usage: kyb-server auth add --id ID --name NAME --role read|write|admin [--email E] [--label L]\n       kyb-server auth token --id ID [--label L]\n       kyb-server auth revoke --token T_ID | --id ID\n       kyb-server auth list"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book_with(role: Role) -> (Book, String) {
        let mut b = Book::default();
        let t = b.add("agent-a", "Agent A", "", role, "laptop").unwrap();
        (b, t)
    }

    fn auth_with(mode: Mode, anon: Option<Role>, book: &Book) -> (Auth, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        book.save(&path).unwrap();
        (Auth::new(mode, anon, path), dir)
    }

    #[test]
    fn a_token_is_shown_once_and_only_its_hash_is_kept() {
        let (book, token) = book_with(Role::Write);
        assert!(token.starts_with("kyb_") && token.len() == 68);
        let stored = serde_json::to_string(&book).unwrap();
        assert!(!stored.contains(&token));
        assert!(stored.contains(&hash_token(&token)));
        let actor = book.lookup(&token).unwrap();
        assert_eq!((actor.id.as_str(), actor.name.as_str(), actor.email.as_str(), actor.role), ("agent-a", "Agent A", "agent-a@kyb", Role::Write));
        assert!(book.lookup("kyb_wrong").is_none());
    }

    #[test]
    fn rotation_overlaps_and_revocation_is_final() {
        let (mut book, first) = book_with(Role::Read);
        let second = book.issue("agent-a", "new").unwrap();
        assert!(book.lookup(&first).is_some() && book.lookup(&second).is_some());
        let first_id = book.principals[0].tokens[0].id.clone();
        assert_eq!(book.revoke_token(&first_id), 1);
        assert_eq!(book.revoke_token(&first_id), 0);
        assert!(book.lookup(&first).is_none() && book.lookup(&second).is_some());
        assert!(book.revoke_principal("agent-a"));
        assert!(!book.revoke_principal("agent-a"));
        assert!(book.lookup(&second).is_none());
        assert_eq!(book.principals[0].name, "Agent A", "a revoked principal still explains old commits");
        assert!(book.issue("agent-a", "").is_err());
        assert!(book.issue("nobody", "").is_err());
    }

    #[test]
    fn ids_and_names_are_checked() {
        let mut b = Book::default();
        assert!(b.add("has space", "N", "", Role::Read, "").is_err());
        assert!(b.add("", "N", "", Role::Read, "").is_err());
        assert!(b.add("ok", "  ", "", Role::Read, "").is_err());
        b.add("ok", "N", "n@x", Role::Read, "").unwrap();
        assert!(b.add("ok", "N", "", Role::Read, "").is_err());
        assert_eq!(b.principals[0].email, "n@x");
    }

    #[test]
    fn the_book_round_trips_and_is_private() {
        let (book, _) = book_with(Role::Admin);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("auth.json");
        book.save(&path).unwrap();
        assert_eq!(Book::load(&path).unwrap(), book);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(Book::load(&dir.path().join("absent.json")).unwrap(), Book::default());
        std::fs::write(&path, "  \n").unwrap();
        assert_eq!(Book::load(&path).unwrap(), Book::default());
        std::fs::write(&path, "{not json").unwrap();
        assert!(Book::load(&path).is_err());
    }

    #[test]
    fn the_book_is_never_allowed_inside_the_canon() {
        assert!(inside(Path::new("./kyb-data/auth.json"), Path::new("./kyb-data")));
        assert!(inside(Path::new("/data/kyb-data/x/auth.json"), Path::new("/data/kyb-data")));
        assert!(!inside(Path::new("/data/auth.json"), Path::new("/data/kyb-data")));
        assert!(!inside(Path::new("./kyb-data-2/auth.json"), Path::new("./kyb-data")));
        assert!(inside(Path::new("./kyb-data/../kyb-data/auth.json"), Path::new("kyb-data")));
    }

    #[test]
    fn concurrent_updates_do_not_lose_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || Book::update(&path, |b| b.add(&format!("p{i}"), "N", "", Role::Read, "")).unwrap())
            })
            .collect();
        let tokens: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let book = Book::load(&path).unwrap();
        assert_eq!(book.principals.len(), 8, "every add is in the book");
        for t in &tokens {
            assert!(book.lookup(t).is_some(), "every printed token works");
        }
        assert!(!dir.path().join("auth.json.lock").exists(), "the lock is released");
        assert!(std::fs::read_dir(dir.path()).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".tmp")), "no temp file is left behind");
        // a lock left behind by a dead process is taken over
        std::fs::write(dir.path().join("auth.json.lock"), "").unwrap();
        let old = SystemTime::now() - std::time::Duration::from_secs(60);
        std::fs::File::options().write(true).open(dir.path().join("auth.json.lock")).unwrap().set_modified(old).unwrap();
        Book::update(&path, |b| b.add("late", "N", "", Role::Read, "")).unwrap();
        assert_eq!(Book::load(&path).unwrap().principals.len(), 9);
        // a change that fails leaves the book as it was and the lock released
        assert!(Book::update(&path, |b| b.add("late", "N", "", Role::Read, "")).is_err());
        assert_eq!(Book::load(&path).unwrap().principals.len(), 9);
        assert!(!dir.path().join("auth.json.lock").exists());
    }

    #[test]
    fn routes_ask_for_the_least_that_fits() {
        assert_eq!(need(&Method::GET, "/"), Need::Nothing);
        assert_eq!(need(&Method::GET, "/healthz"), Need::Nothing);
        assert_eq!(need(&Method::GET, "/whoami"), Need::Nothing);
        assert_eq!(need(&Method::GET, "/search"), Need::Role(Role::Read));
        assert_eq!(need(&Method::GET, "/api/activity"), Need::Role(Role::Read));
        assert_eq!(need(&Method::GET, "/api/audit"), Need::Role(Role::Admin));
        assert_eq!(need(&Method::POST, "/knowledge"), Need::Role(Role::Write));
        assert_eq!(need(&Method::POST, "/incidents/x/resolve"), Need::Role(Role::Write));
        assert_eq!(need(&Method::DELETE, "/knowledge/x"), Need::Role(Role::Write));
        assert_eq!(need(&Method::POST, "/reindex"), Need::Role(Role::Admin));
    }

    #[test]
    fn off_trusts_everyone_and_compat_honours_a_token_but_does_not_ask_for_one() {
        let (book, token) = book_with(Role::Read);
        let (off, _d) = auth_with(Mode::Off, None, &book);
        let s = off.decide(&Method::POST, "/reindex", None).unwrap();
        assert!(s.actor.is_none() && s.role == Role::Admin);
        assert!(off.decide(&Method::POST, "/reindex", Some("Bearer nonsense")).is_ok());

        let (compat, _d) = auth_with(Mode::Compat, None, &book);
        assert_eq!(compat.decide(&Method::POST, "/knowledge", None).unwrap().role, Role::Admin);
        let with = compat.decide(&Method::GET, "/search", Some(&format!("Bearer {token}"))).unwrap();
        assert_eq!(with.actor.unwrap().id, "agent-a");
        // a token that is sent is held to its own role, and a wrong one is refused
        assert_eq!(compat.decide(&Method::POST, "/knowledge", Some(&format!("bearer {token}"))).unwrap_err(), Refusal::Forbidden { have: Role::Read, need: Role::Write });
        assert!(matches!(compat.decide(&Method::GET, "/search", Some("Bearer kyb_nope")).unwrap_err(), Refusal::Unknown(_)));
    }

    #[test]
    fn required_refuses_the_nameless_unless_an_anonymous_role_is_set() {
        let (book, token) = book_with(Role::Write);
        let (strict, _d) = auth_with(Mode::Required, None, &book);
        assert!(matches!(strict.decide(&Method::GET, "/search", None).unwrap_err(), Refusal::Unknown(_)));
        assert!(matches!(strict.decide(&Method::GET, "/search", Some("Bearer ")).unwrap_err(), Refusal::Unknown(_)));
        assert!(matches!(strict.decide(&Method::GET, "/search", Some("Basic abc")).unwrap_err(), Refusal::Unknown(_)));
        // the page, the probe and whoami answer anyone; the page even with a dead token
        for open in ["/", "/healthz", "/whoami"] {
            assert!(strict.decide(&Method::GET, open, None).is_ok(), "{open}");
        }
        assert!(strict.decide(&Method::GET, "/", Some("Bearer kyb_dead")).is_ok());
        assert!(strict.decide(&Method::GET, "/whoami", Some("Bearer kyb_dead")).is_err());
        let bearer = format!("Bearer {token}");
        assert!(strict.decide(&Method::POST, "/knowledge", Some(&bearer)).is_ok());
        assert_eq!(strict.decide(&Method::POST, "/reindex", Some(&bearer)).unwrap_err(), Refusal::Forbidden { have: Role::Write, need: Role::Admin });

        let (demo, _d) = auth_with(Mode::Required, Some(Role::Read), &book);
        assert_eq!(demo.decide(&Method::GET, "/search", None).unwrap().role, Role::Read);
        assert_eq!(demo.decide(&Method::POST, "/knowledge", None).unwrap_err(), Refusal::Forbidden { have: Role::Read, need: Role::Write });
        assert!(demo.decide(&Method::POST, "/knowledge", Some(&bearer)).is_ok());
    }

    #[test]
    fn a_change_to_the_book_is_seen_without_a_restart_and_a_broken_book_admits_nobody() {
        let (mut book, token) = book_with(Role::Read);
        let (auth, dir) = auth_with(Mode::Required, None, &book);
        let bearer = format!("Bearer {token}");
        assert!(auth.decide(&Method::GET, "/search", Some(&bearer)).is_ok());
        book.revoke_principal("agent-a");
        // a later mtime, whatever the clock's resolution
        let path = dir.path().join("auth.json");
        book.save(&path).unwrap();
        let later = SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(later).unwrap();
        assert!(auth.decide(&Method::GET, "/search", Some(&bearer)).is_err());
        std::fs::write(&path, "{broken").unwrap();
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(later + std::time::Duration::from_secs(5)).unwrap();
        assert!(auth.decide(&Method::GET, "/search", Some(&bearer)).is_err());
    }

    #[test]
    fn shown_author_hides_the_service_signature() {
        assert_eq!(shown_author("kyb", "kyb@local"), "");
        assert_eq!(shown_author("kyb", "kyb@fleet"), "kyb <kyb@fleet>", "a principal named like the service is still someone");
        assert_eq!(shown_author("", ""), "");
        assert_eq!(shown_author("Agent A", "a@kyb"), "Agent A <a@kyb>");
        assert_eq!(shown_author("Agent A", ""), "Agent A");
        assert_eq!(signature(), ("kyb".to_string(), "kyb@local".to_string()));
        assert_eq!(role(), Role::Admin);
    }

    #[test]
    fn the_start_up_line_says_what_is_in_force() {
        let (book, _) = book_with(Role::Write);
        let (off, _d) = auth_with(Mode::Off, None, &book);
        assert!(off.describe().starts_with("auth off"));
        let (compat, _d) = auth_with(Mode::Compat, None, &book);
        assert!(compat.describe().contains("1 principals") && compat.describe().contains("trusted bind"));
        let (demo, _d) = auth_with(Mode::Required, Some(Role::Read), &book);
        assert!(demo.describe().contains("may read") && !demo.describe().contains("WARNING"));
        let (empty, _d) = auth_with(Mode::Required, None, &Book::default());
        assert!(empty.describe().contains("WARNING"), "{}", empty.describe());
    }

    #[test]
    fn roles_parse_and_order() {
        assert_eq!(["read", "write", "admin", "root"].map(Role::parse), [Some(Role::Read), Some(Role::Write), Some(Role::Admin), None]);
        assert!(Role::Read < Role::Write && Role::Write < Role::Admin);
        assert_eq!([Role::Read.name(), Role::Write.name(), Role::Admin.name()], ["read", "write", "admin"]);
    }
}
