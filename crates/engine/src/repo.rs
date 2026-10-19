//! Git-native collaboration commands (`repo.*`).
//!
//! The project directory is the repository boundary the user already
//! owns; WarraqCraft only invokes fixed `git` argument vectors — never a
//! shell — and keeps credentials/remotes with the user's Git tooling.
//! Mutating commands are explicit and refuse to run while the open
//! buffer or its file has unresolved local state.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};

use crate::{EngineError, Session};

const CAP: usize = 256 * 1024;
const DEFAULT_TIMEOUT_S: u64 = 30;
const MAX_TIMEOUT_S: u64 = 300;
const NETWORK_TIMEOUT_S: u64 = 120;
const MAX_LIST: usize = 256;
const MAX_MESSAGE: usize = 4096;
const MAX_URL: usize = 2048;
const MAX_NAME: usize = 128;

struct Proc {
    status: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
}

fn project_root(session: &Session) -> Result<PathBuf, EngineError> {
    session.path().and_then(|p| p.parent().map(Path::to_path_buf)).ok_or(EngineError::NoFile)
}

#[allow(clippy::disallowed_methods)] // Instant/threads: repo commands are native desktop engine work.
fn run_capture(cwd: &Path, args: &[String], timeout: Duration) -> Result<Proc, EngineError> {
    let started = std::time::Instant::now();
    let mut child = Command::new("git").args(args).current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let (out_tx, out_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let (err_tx, err_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    if let Some(p) = child.stdout.take() {
        std::thread::spawn(move || {
            let _ = out_tx.send(read_capped(p));
        });
    }
    if let Some(p) = child.stderr.take() {
        std::thread::spawn(move || {
            let _ = err_tx.send(read_capped(p));
        });
    }
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            timed_out = true;
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let drain = Duration::from_secs(2);
    let stdout = out_rx.recv_timeout(drain).unwrap_or_default();
    let stderr = err_rx.recv_timeout(drain).unwrap_or_default();
    Ok(Proc { status: status.and_then(|s| s.code()), stdout, stderr, timed_out })
}

fn read_capped<R: Read>(mut r: R) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    let mut overflow = false;
    loop {
        let n = match r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let room = CAP.saturating_sub(out.len());
        let keep = n.min(room);
        out.extend_from_slice(&buf[..keep]);
        overflow |= keep < n;
    }
    if overflow {
        out.extend_from_slice(b"\n[truncated]");
    }
    out
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_string()
}

fn timeout(params: &Value, default_s: u64) -> Duration {
    Duration::from_secs(params.get("timeout_s").and_then(Value::as_u64).unwrap_or(default_s).clamp(1, MAX_TIMEOUT_S))
}

fn git(cwd: &Path, args: &[&str], timeout: Duration) -> Result<Proc, EngineError> {
    run_capture(cwd, &args.iter().map(|a| (*a).to_string()).collect::<Vec<_>>(), timeout)
}

fn git_ok(cwd: &Path, args: &[&str], timeout: Duration) -> Result<Vec<u8>, EngineError> {
    let out = git(cwd, args, timeout)?;
    if out.timed_out {
        return Err(EngineError::Git(format!("{} timed out", args.first().copied().unwrap_or("command"))));
    }
    if out.status != Some(0) {
        let err = text(&out.stderr);
        let msg = if err.is_empty() { text(&out.stdout) } else { err };
        let msg = if msg.is_empty() { format!("{} failed", args.first().copied().unwrap_or("command")) } else { msg };
        return Err(EngineError::Git(redact_credentials(&msg)));
    }
    Ok(out.stdout)
}

fn repo_root(dir: &Path, timeout: Duration) -> Result<Option<PathBuf>, EngineError> {
    let out = git(dir, &["rev-parse", "--show-toplevel"], timeout)?;
    if out.status != Some(0) || out.timed_out {
        return Ok(None);
    }
    Ok(Some(PathBuf::from(text(&out.stdout))))
}

fn require_repo(session: &Session, timeout: Duration) -> Result<(PathBuf, PathBuf), EngineError> {
    let dir = project_root(session)?;
    let Some(root) = repo_root(&dir, timeout)? else {
        return Err(EngineError::Git("project is not inside a Git repository".into()));
    };
    Ok((dir, root))
}

fn guard_clean(session: &Session) -> Result<(), EngineError> {
    if session.dirty() {
        return Err(EngineError::DirtyBuffer);
    }
    if session.disk_changed()? {
        return Err(EngineError::ExternalChange);
    }
    Ok(())
}

fn remote_name(params: &Value) -> Result<String, EngineError> {
    let name = params.get("remote").and_then(Value::as_str).unwrap_or("origin");
    if name.is_empty() || name.len() > MAX_NAME || !name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')) {
        return Err(EngineError::BadParam("remote"));
    }
    Ok(name.to_string())
}

fn branch_name(params: &Value, key: &'static str) -> Result<Option<String>, EngineError> {
    let Some(name) = params.get(key).and_then(Value::as_str) else { return Ok(None) };
    if name.is_empty()
        || name.len() > MAX_NAME
        || name.starts_with('-')
        || name.bytes().any(|b| b.is_ascii_whitespace() || b == b'~' || b == b'^' || b == b':' || b == b'?' || b == b'*' || b == b'[' || b == b'\\')
    {
        return Err(EngineError::BadParam(key));
    }
    Ok(Some(name.to_string()))
}

fn redact_credentials(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for word in input.split_inclusive([' ', '\n', '\t']) {
        if let Some(scheme_end) = word.find("://") {
            let auth_start = scheme_end + 3;
            let auth_end = word[auth_start..].find(['/', ' ', '\n', '\t']).map(|n| auth_start + n).unwrap_or(word.len());
            if let Some(at) = word.get(auth_start..auth_end).and_then(|a| a.rfind('@')).map(|n| auth_start + n) {
                out.push_str(&word[..auth_start]);
                out.push_str("***");
                out.push_str(&word[at..]);
                continue;
            }
        }
        out.push_str(word);
    }
    out
}

/// `repo.status` — branch/upstream/ahead-behind/worktree state for the
/// project repository, plus the session buffer's dirty/disk state.
pub fn status(session: &Session, params: &Value) -> Result<Value, EngineError> {
    let dir = project_root(session)?;
    let t = timeout(params, DEFAULT_TIMEOUT_S);
    let remote = remote_name(params)?;
    let version = git(&dir, &["--version"], t)?;
    if version.status != Some(0) || version.timed_out {
        return Ok(json!({"git": false, "in_repo": false, "root": dir.display().to_string(), "dirty_buffer": session.dirty()}));
    }
    let Some(root) = repo_root(&dir, t)? else {
        return Ok(json!({"git": true, "in_repo": false, "root": dir.display().to_string(), "dirty_buffer": session.dirty()}));
    };

    let branch = text(&git_ok(&root, &["branch", "--show-current"], t)?);
    let detached = branch.is_empty();
    let branch = if detached { text(&git_ok(&root, &["rev-parse", "--short", "HEAD"], t)?) } else { branch };
    let upstream_out = git(&root, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"], t)?;
    let upstream = (upstream_out.status == Some(0) && !upstream_out.timed_out).then(|| text(&upstream_out.stdout)).filter(|s| !s.is_empty());
    let remote_out = git(&root, &["remote", "get-url", &remote], t)?;
    let remote_url =
        (remote_out.status == Some(0) && !remote_out.timed_out).then(|| redact_credentials(&text(&remote_out.stdout))).filter(|s| !s.is_empty());
    let (ahead, behind) = if upstream.is_some() {
        let counts = git(&root, &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"], t)?;
        if counts.status == Some(0) && !counts.timed_out {
            let counts_text = text(&counts.stdout);
            let mut parts = counts_text.split_whitespace();
            (parts.next().and_then(|n| n.parse::<u64>().ok()).unwrap_or(0), parts.next().and_then(|n| n.parse::<u64>().ok()).unwrap_or(0))
        } else {
            (0, 0)
        }
    } else {
        (0, 0)
    };

    let status_out = git_ok(&root, &["status", "--porcelain=v1", "-z"], t)?;
    let mut changes = Vec::new();
    let mut staged = 0usize;
    let mut modified = 0usize;
    let mut untracked = 0usize;
    let mut conflicts = Vec::new();
    for rec in status_out.split(|b| *b == 0).filter(|r| r.len() >= 4) {
        if rec.starts_with(b"##") {
            continue;
        }
        let code = String::from_utf8_lossy(&rec[..2]).to_string();
        let path = String::from_utf8_lossy(&rec[3..]).to_string();
        let x = rec.first().copied().unwrap_or(b' ');
        let y = rec.get(1).copied().unwrap_or(b' ');
        if code == "??" {
            untracked += 1;
        } else {
            if x != b' ' {
                staged += 1;
            }
            if y != b' ' {
                modified += 1;
            }
        }
        if matches!(code.as_str(), "DD" | "AU" | "UD" | "UA" | "DU" | "AA" | "UU") && conflicts.len() < MAX_LIST {
            conflicts.push(path.clone());
        }
        if changes.len() < MAX_LIST {
            changes.push(json!({"status": code, "path": path}));
        }
    }
    Ok(json!({
        "git": true,
        "in_repo": true,
        "root": root.display().to_string(),
        "project_root": dir.display().to_string(),
        "branch": branch,
        "detached": detached,
        "upstream": upstream,
        "remote": remote,
        "remote_url": remote_url,
        "ahead": ahead,
        "behind": behind,
        "clean": changes.is_empty(),
        "changed": changes.len(),
        "staged": staged,
        "modified": modified,
        "untracked": untracked,
        "conflicts": conflicts,
        "changes": changes,
        "dirty_buffer": session.dirty(),
        "external_changed": session.disk_changed()?,
    }))
}

/// `repo.init` — create a repository in the open document's directory.
/// Optional `remote` + `remote_name` records where the user wants to
/// share it; credentials are not touched.
pub fn init(session: &Session, params: &Value) -> Result<Value, EngineError> {
    let dir = project_root(session)?;
    let t = timeout(params, DEFAULT_TIMEOUT_S);
    git_ok(&dir, &["init"], t)?;
    let mut remote_url = Value::Null;
    if let Some(url) = params.get("remote").and_then(Value::as_str) {
        if url.is_empty() || url.len() > MAX_URL {
            return Err(EngineError::BadParam("remote"));
        }
        let name = params.get("remote_name").and_then(Value::as_str).unwrap_or("origin");
        if name.is_empty() || name.len() > MAX_NAME || !name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')) {
            return Err(EngineError::BadParam("remote_name"));
        }
        git_ok(&dir, &["remote", "add", name, url], t)?;
        remote_url = Value::String(redact_credentials(url));
    }
    Ok(json!({"ok": true, "root": dir.display().to_string(), "remote_url": remote_url}))
}

/// `repo.pull` — fast-forward only. A dirty buffer or externally changed
/// open file blocks the operation; other worktree conflicts are Git's
/// normal actionable error.
pub fn pull(session: &Session, params: &Value) -> Result<Value, EngineError> {
    guard_clean(session)?;
    let t = timeout(params, NETWORK_TIMEOUT_S);
    let (_, root) = require_repo(session, t)?;
    let remote = branch_name(params, "remote")?;
    let branch = branch_name(params, "branch")?;
    let mut args = vec!["pull".to_string(), "--ff-only".to_string()];
    if let Some(r) = remote {
        args.push(r);
        if let Some(b) = branch {
            args.push(b);
        }
    }
    let out = git_ok(&root, &args.iter().map(String::as_str).collect::<Vec<_>>(), t)?;
    Ok(json!({"ok": true, "root": root.display().to_string(), "output": redact_credentials(&text(&out))}))
}

/// `repo.push` — explicit push. `set_upstream: true` maps to `git push
/// -u <remote> <branch>`; otherwise remote/branch args are optional.
pub fn push(session: &Session, params: &Value) -> Result<Value, EngineError> {
    guard_clean(session)?;
    let t = timeout(params, NETWORK_TIMEOUT_S);
    let (_, root) = require_repo(session, t)?;
    let remote = branch_name(params, "remote")?;
    let branch = branch_name(params, "branch")?;
    let set_upstream = params.get("set_upstream").and_then(Value::as_bool).unwrap_or(false);
    let mut args = vec!["push".to_string()];
    if set_upstream {
        args.push("-u".into());
    }
    if let Some(r) = remote {
        args.push(r);
        if let Some(b) = branch {
            args.push(b);
        }
    }
    let out = git_ok(&root, &args.iter().map(String::as_str).collect::<Vec<_>>(), t)?;
    Ok(json!({"ok": true, "root": root.display().to_string(), "output": redact_credentials(&text(&out))}))
}

/// `repo.commit` — optional `stage: true` (`git add -A`) then
/// `git commit -m <message>`. The message is authored by the user/agent.
pub fn commit(session: &Session, params: &Value) -> Result<Value, EngineError> {
    guard_clean(session)?;
    let t = timeout(params, DEFAULT_TIMEOUT_S);
    let (_, root) = require_repo(session, t)?;
    let message = params.get("message").and_then(Value::as_str).ok_or(EngineError::BadParam("message"))?;
    if message.trim().is_empty() || message.len() > MAX_MESSAGE {
        return Err(EngineError::BadParam("message"));
    }
    if params.get("stage").and_then(Value::as_bool).unwrap_or(false) {
        git_ok(&root, &["add", "-A"], t)?;
    }
    let out = git_ok(&root, &["commit", "-m", message], t)?;
    Ok(json!({"ok": true, "root": root.display().to_string(), "output": redact_credentials(&text(&out))}))
}

/// `repo.branches` — list branches by default; `create` runs
/// `git switch -c <name>` and `switch` runs `git switch <name>`, both
/// guarded against an unsaved/stale buffer.
pub fn branches(session: &Session, params: &Value) -> Result<Value, EngineError> {
    let t = timeout(params, DEFAULT_TIMEOUT_S);
    let (_, root) = require_repo(session, t)?;
    let create = branch_name(params, "create")?;
    let switch = branch_name(params, "switch")?;
    if create.is_some() || switch.is_some() {
        guard_clean(session)?;
        if let Some(name) = create {
            git_ok(&root, &["check-ref-format", "--branch", &name], t)?;
            git_ok(&root, &["switch", "-c", &name], t)?;
        }
        if let Some(name) = switch {
            git_ok(&root, &["check-ref-format", "--branch", &name], t)?;
            git_ok(&root, &["switch", &name], t)?;
        }
    }
    let out = git_ok(&root, &["branch", "--format=%(HEAD)%09%(refname:short)%09%(upstream:short)%09%(objectname:short)"], t)?;
    let mut current = String::new();
    let mut list = Vec::new();
    for line in String::from_utf8_lossy(&out).lines().take(MAX_LIST) {
        let mut parts = line.split('\t');
        let cur = parts.next().unwrap_or_default().trim() == "*";
        let name = parts.next().unwrap_or_default().to_string();
        let upstream = parts.next().unwrap_or_default().to_string();
        let commit = parts.next().unwrap_or_default().to_string();
        if cur {
            current = name.clone();
        }
        list.push(json!({"name": name, "current": cur, "upstream": upstream, "commit": commit}));
    }
    Ok(json!({"ok": true, "root": root.display().to_string(), "current": current, "branches": list}))
}

/// `repo.conflicts` — unmerged paths, capped for UI/agent display.
pub fn conflicts(session: &Session) -> Result<Value, EngineError> {
    let t = Duration::from_secs(DEFAULT_TIMEOUT_S);
    let (_, root) = require_repo(session, t)?;
    let out = git_ok(&root, &["diff", "--name-only", "--diff-filter=U", "-z"], t)?;
    let paths: Vec<String> =
        out.split(|b| *b == 0).filter(|p| !p.is_empty()).take(MAX_LIST).map(|p| String::from_utf8_lossy(p).to_string()).collect();
    Ok(json!({"root": root.display().to_string(), "conflicts": paths}))
}

/// `repo.history` — recent commits in a stable tab-parsed shape.
pub fn history(session: &Session, params: &Value) -> Result<Value, EngineError> {
    let t = timeout(params, DEFAULT_TIMEOUT_S);
    let (_, root) = require_repo(session, t)?;
    let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(20).clamp(1, 100).to_string();
    let out = git_ok(&root, &["log", "-n", &limit, "--date=short", "--format=%h%x09%ad%x09%an%x09%s"], t)?;
    let entries: Vec<Value> = String::from_utf8_lossy(&out)
        .lines()
        .take(100)
        .map(|line| {
            let mut p = line.split('\t');
            json!({"commit": p.next().unwrap_or_default(), "date": p.next().unwrap_or_default(), "author": p.next().unwrap_or_default(), "subject": p.next().unwrap_or_default()})
        })
        .collect();
    Ok(json!({"root": root.display().to_string(), "entries": entries}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_remote_credentials() {
        assert_eq!(redact_credentials("https://token@example.com/repo.git"), "https://***@example.com/repo.git");
        assert_eq!(redact_credentials("fatal: https://u:p@example.com/r.git denied"), "fatal: https://***@example.com/r.git denied");
        assert_eq!(redact_credentials("git@example.com:repo.git"), "git@example.com:repo.git");
    }

    #[test]
    fn bad_names_are_rejected_before_git() {
        assert!(remote_name(&json!({"remote": "origin"})).is_ok());
        assert!(remote_name(&json!({"remote": "bad name"})).is_err());
        assert!(branch_name(&json!({"switch": "-d"}), "switch").is_err());
        assert!(branch_name(&json!({"switch": "feature/one"}), "switch").is_ok());
    }
}
