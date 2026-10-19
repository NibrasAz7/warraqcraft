//! Deterministic LaTeX-agent evaluation fixtures (M7.3).
//!
//! Inspired by Harness²'s evidence-driven evaluation loop, this runs the
//! bounded compile-repair workflow against fixed documents and records
//! every check that passed or failed. Each case executes in an isolated
//! temporary session — fixture tasks and approvals never touch the
//! caller's durable store — then a bounded run record is persisted under
//! `.warraqcraft/ai/evals/` so `ai.eval.list` can compare runs over
//! time. No model provider, subprocess or network call is involved:
//! `compile`/`recompile` are always false, keeping every run identical
//! on every machine.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};

use crate::ai_harness::{now_ms, task_approve};
use crate::ai_repair;
use crate::{EngineError, Session};

const MAX_EVAL_FILES: usize = 128;
const MAX_EVAL_CHECK_DETAIL: usize = 512;
const MAX_EDITS: usize = 32;

static EVAL_SEQ: AtomicU64 = AtomicU64::new(0);

/// What a case expects the repair workflow to do.
struct Expect {
    /// `ai.repair.preview` reports edits for this source.
    repairable: bool,
    /// Exact ordered `op` list the plan must propose; `None` only
    /// checks boundedness (hostile cases where any bounded plan is ok).
    ops: Option<&'static [&'static str]>,
    /// Post-apply `structure().issues` must be at most this many.
    max_issues: usize,
    /// Substrings the patched buffer must contain.
    contains: &'static [&'static str],
    /// Substrings the patched buffer must not contain.
    excludes: &'static [&'static str],
}

struct EvalCase {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    make_src: fn() -> String,
    expect: Expect,
}

fn src_missing_end() -> String {
    "\\documentclass{article}\n\\begin{document}\n\\begin{itemize}\n\\item a\n".to_string()
}

fn src_env_mismatch() -> String {
    "\\begin{document}\n\\begin{itemize}\n\\item a\n\\end{bad}\n\\end{document}\n".to_string()
}

fn src_stray_end() -> String {
    "\\begin{document}\ntext\n\\end{foo}\n\\end{document}\n".to_string()
}

fn src_nested_stray() -> String {
    "\\begin{document}\n\\begin{a}\nx\n\\end{foo}\n\\end{a}\n\\end{document}\n".to_string()
}

fn src_clean() -> String {
    "\\begin{document}\nok\n\\end{document}\n".to_string()
}

fn src_hostile_deep() -> String {
    format!("\\begin{{document}}\n{}\n{}\n\\end{{document}}\n", "\\begin{x}\n".repeat(500), "\\end{y}\n".repeat(500))
}

static CASES: &[EvalCase] = &[
    EvalCase {
        id: "missing_end",
        name: "Unclosed environments",
        description: "Two open environments with no closers: the plan must close them innermost-first in one insert_before.",
        make_src: src_missing_end,
        expect: Expect {
            repairable: true,
            ops: Some(&["insert_before"]),
            max_issues: 0,
            contains: &["\\end{itemize}\n\\end{document}"],
            excludes: &[],
        },
    },
    EvalCase {
        id: "env_mismatch",
        name: "Mismatched \\end name",
        description: "\\end{bad} where \\end{itemize} was meant: a single replace_line, no extra closers.",
        make_src: src_env_mismatch,
        expect: Expect { repairable: true, ops: Some(&["replace_line"]), max_issues: 0, contains: &["\\end{itemize}"], excludes: &["\\end{bad}"] },
    },
    EvalCase {
        id: "stray_end",
        name: "Stray \\end inside document",
        description: "\\end{foo} inside an open document env whose proper closer follows: the line is deleted, not renamed.",
        make_src: src_stray_end,
        expect: Expect { repairable: true, ops: Some(&["delete_line"]), max_issues: 0, contains: &[], excludes: &["\\end{foo}"] },
    },
    EvalCase {
        id: "nested_stray",
        name: "Stray \\end inside a nested env",
        description: "\\end{foo} between \\begin{a} and its real \\end{a}: delete the junk line and keep the structure intact.",
        make_src: src_nested_stray,
        expect: Expect {
            repairable: true,
            ops: Some(&["delete_line"]),
            max_issues: 0,
            contains: &["\\end{a}\n\\end{document}"],
            excludes: &["\\end{foo}"],
        },
    },
    EvalCase {
        id: "clean_doc",
        name: "Already-correct document",
        description: "A valid document produces no edits — the agent must not invent work.",
        make_src: src_clean,
        expect: Expect { repairable: false, ops: Some(&[]), max_issues: 0, contains: &[], excludes: &[] },
    },
    EvalCase {
        id: "hostile_deep",
        name: "Hostile deep mismatch",
        description: "500 unclosed \\begin{x} and 500 stray \\end{y}: preview must stay bounded and never panic.",
        make_src: src_hostile_deep,
        expect: Expect { repairable: true, ops: None, max_issues: usize::MAX, contains: &[], excludes: &[] },
    },
];

/// Public case count for `ai.components`.
pub const CASE_COUNT: usize = CASES.len();

/// Where eval run records are persisted for this session's document.
pub(crate) fn eval_dir(sess: &Session) -> PathBuf {
    sess.ai_root().join("evals")
}

/// `ai.eval.list` — fixture definitions plus recent run summaries.
pub fn eval_list(sess: &Session, _params: &Value) -> Result<Value, EngineError> {
    let cases: Vec<Value> = CASES
        .iter()
        .map(|c| {
            json!({
                "id": c.id,
                "name": c.name,
                "description": c.description,
                "expect": {"repairable": c.expect.repairable, "ops": c.expect.ops},
            })
        })
        .collect();
    let mut runs = Vec::new();
    for path in eval_files(sess)? {
        let Ok(record) = read_eval(&path) else { continue };
        runs.push(json!({
            "case": record.get("case"),
            "run_ms": record.get("run_ms"),
            "pass": record.get("pass"),
            "checks": record.get("checks").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            "failed_checks": record.get("checks").and_then(Value::as_array).map(|cs| cs.iter().filter(|c| c.get("ok").and_then(Value::as_bool) != Some(true)).count()).unwrap_or(0),
        }));
    }
    runs.sort_by_key(|r| r.get("run_ms").and_then(Value::as_u64).unwrap_or(0));
    runs.reverse();
    runs.truncate(50);
    Ok(json!({"cases": cases, "runs": runs}))
}

/// `ai.eval.run` — execute one fixture (`case`) or all of them in
/// isolated temporary sessions, persist bounded run records, and return
/// the per-check detail. Never compiles and never touches the caller's
/// buffer or durable task store.
pub fn eval_run(sess: &Session, params: &Value) -> Result<Value, EngineError> {
    let case_id = params.get("case").and_then(Value::as_str);
    if let Some(id) = case_id {
        if id.len() > 64 || !CASES.iter().any(|c| c.id == id) {
            return Err(EngineError::BadParam("case"));
        }
    }
    let selected: Vec<&EvalCase> = CASES.iter().filter(|c| case_id.is_none_or(|id| c.id == id)).collect();

    let mut records = Vec::new();
    for case in selected {
        let record = run_case(case);
        persist_record(sess, &record)?;
        records.push(record);
    }
    let passed = records.iter().filter(|r| r.get("pass").and_then(Value::as_bool) == Some(true)).count();
    Ok(json!({"ok": true, "cases": records, "passed": passed, "failed": records.len() - passed}))
}

/// Execute one fixture end-to-end in a throwaway session.
fn run_case(case: &EvalCase) -> Value {
    let dir = TempDir::new(case.id);
    let mut checks: Vec<Value> = Vec::new();
    let src = (case.make_src)();
    let path = dir.0.join("main.tex");
    if fs::write(&path, &src).is_err() {
        checks.push(check("write fixture", false, "could not write the temp document"));
        return record(case, &checks, &[]);
    }
    let mut ts = Session::new();
    if let Err(e) = ts.open(&path) {
        checks.push(check("open fixture", false, &e.to_string()));
        return record(case, &checks, &[]);
    }

    let preview = match ai_repair::preview(&mut ts, &json!({"compile": false})) {
        Ok(v) => v,
        Err(e) => {
            checks.push(check("preview", false, &e.to_string()));
            return record(case, &checks, &[]);
        }
    };
    checks.push(check("preview", true, ""));
    let repairable = preview.get("repairable").and_then(Value::as_bool).unwrap_or(false);
    checks.push(check("repairable flag", repairable == case.expect.repairable, &format!("expected {}, got {repairable}", case.expect.repairable)));

    let plan = preview.get("plan").cloned().unwrap_or(Value::Null);
    let edits: Vec<Value> = plan.get("edits").and_then(Value::as_array).cloned().unwrap_or_default();
    let ops: Vec<&str> = edits.iter().filter_map(|e| e.get("op").and_then(Value::as_str)).collect();
    checks.push(check("edit count bounded", edits.len() <= MAX_EDITS, &format!("{} edit(s)", edits.len())));
    if case.expect.repairable {
        if let Some(expected) = case.expect.ops {
            checks.push(check("ops match", ops.as_slice() == expected, &format!("expected {expected:?}, got {ops:?}")));
        }
        let state = preview.pointer("/task/state").and_then(Value::as_str).unwrap_or_default();
        checks.push(check("awaiting approval", state == "waiting_approval", state));
        checks.push(check("buffer untouched", ts.source() == src, "preview must not mutate the buffer"));

        let task_id = preview.get("task_id").and_then(Value::as_str).unwrap_or_default().to_string();
        match task_approve(&ts, &json!({"task_id": task_id, "approve": true})) {
            Ok(_) => checks.push(check("approve", true, "")),
            Err(e) => {
                checks.push(check("approve", false, &e.to_string()));
                return record(case, &checks, &[]);
            }
        }
        match ai_repair::apply(&mut ts, &json!({"task_id": task_id, "recompile": false})) {
            Ok(v) => {
                let state = v.pointer("/task/state").and_then(Value::as_str).unwrap_or_default();
                checks.push(check("apply", state == "completed", state));
            }
            Err(e) => {
                checks.push(check("apply", false, &e.to_string()));
                return record(case, &checks, &[]);
            }
        }
        if case.expect.max_issues != usize::MAX {
            let issues = ts.structure().issues.len();
            checks.push(check("post-apply issues", issues <= case.expect.max_issues, &format!("{issues} issue(s) remain")));
            for needle in case.expect.contains {
                checks.push(check(&format!("contains {needle:?}"), ts.source().contains(needle), ""));
            }
            for needle in case.expect.excludes {
                checks.push(check(&format!("excludes {needle:?}"), !ts.source().contains(needle), ""));
            }
        }
    } else {
        checks.push(check("no plan stored", plan.is_null(), "non-repairable previews must not write a plan"));
    }
    record(case, &checks, &ops.iter().map(|o| o.to_string()).collect::<Vec<_>>())
}

fn check(name: &str, ok: bool, detail: &str) -> Value {
    json!({
        "name": name,
        "ok": ok,
        "detail": detail.chars().take(MAX_EVAL_CHECK_DETAIL).collect::<String>(),
    })
}

fn record(case: &EvalCase, checks: &[Value], ops: &[String]) -> Value {
    let pass = checks.iter().all(|c| c.get("ok").and_then(Value::as_bool) == Some(true));
    json!({
        "version": 1,
        "case": case.id,
        "name": case.name,
        "run_ms": now_ms(),
        "pass": pass,
        "ops": ops,
        "checks": checks,
    })
}

/// Persist a run record, pruning the oldest records beyond the cap.
fn persist_record(sess: &Session, record: &Value) -> Result<(), EngineError> {
    let dir = eval_dir(sess);
    fs::create_dir_all(&dir)?;
    let case = record.get("case").and_then(Value::as_str).unwrap_or("case");
    let seq = EVAL_SEQ.fetch_add(1, Ordering::Relaxed);
    let path = dir.join(format!("{case}-{}-{seq}.json", record.get("run_ms").and_then(Value::as_u64).unwrap_or(0)));
    craft_atomic::atomic_write(&path, record.to_string().as_bytes())?;
    prune_evals(&dir)?;
    Ok(())
}

fn eval_files(sess: &Session) -> Result<Vec<PathBuf>, EngineError> {
    let dir = eval_dir(sess);
    let mut files = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else { return Ok(files) };
    for entry in entries.take(MAX_EVAL_FILES + 1) {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            files.push(path);
        }
    }
    files.sort();
    if files.len() > MAX_EVAL_FILES {
        return Err(EngineError::Tool("too many AI eval records".into()));
    }
    Ok(files)
}

fn read_eval(path: &PathBuf) -> Result<Value, EngineError> {
    if path.metadata().map(|m| m.len()).unwrap_or(u64::MAX) > 1024 * 1024 {
        return Err(EngineError::Tool("AI eval record exceeds the 1 MiB limit".into()));
    }
    let text = fs::read_to_string(path)?;
    serde_json::from_str(&text).map_err(|e| EngineError::Tool(format!("invalid AI eval JSON: {e}")))
}

fn prune_evals(dir: &PathBuf) -> Result<(), EngineError> {
    let mut files: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.take(MAX_EVAL_FILES + 64) {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                files.push(path);
            }
        }
    }
    files.sort();
    while files.len() > MAX_EVAL_FILES {
        if let Some(oldest) = files.first().cloned() {
            let _ = fs::remove_file(&oldest);
        }
        files.remove(0);
    }
    Ok(())
}

/// RAII scratch directory for one fixture's session state.
struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> Self {
        let n = EVAL_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("warraqcraft-eval-{}-{tag}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::create_dir_all(&dir);
        Self(dir)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    struct TestDir(PathBuf);
    impl TestDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("warraqcraft-eval-test-{}-{tag}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn session(dir: &Path) -> Session {
        let path = dir.join("main.tex");
        fs::write(&path, "\\begin{document}\nok\n\\end{document}\n").unwrap();
        let mut s = Session::new();
        s.open(&path).unwrap();
        s
    }

    #[test]
    fn all_cases_pass() {
        let dir = TestDir::new("all");
        let sess = session(&dir.0);
        let out = eval_run(&sess, &json!({})).unwrap();
        assert_eq!(out["failed"], 0, "eval failures: {}", serde_json::to_string_pretty(&out).unwrap());
        assert_eq!(out["passed"].as_u64().unwrap(), CASE_COUNT as u64);
        assert!(eval_dir(&sess).is_dir());
    }

    #[test]
    fn single_case_and_eval_list() {
        let dir = TestDir::new("single");
        let sess = session(&dir.0);
        let out = eval_run(&sess, &json!({"case": "clean_doc"})).unwrap();
        assert_eq!(out["passed"], 1);
        let list = eval_list(&sess, &json!({})).unwrap();
        assert_eq!(list["cases"].as_array().unwrap().len(), CASE_COUNT);
        assert_eq!(list["runs"].as_array().unwrap().len(), 1);
        assert_eq!(list["runs"][0]["case"], "clean_doc");
        assert!(eval_run(&sess, &json!({"case": "nope"})).is_err());
    }

    #[test]
    fn eval_records_do_not_pollute_caller_tasks() {
        let dir = TestDir::new("isolation");
        let sess = session(&dir.0);
        eval_run(&sess, &json!({"case": "env_mismatch"})).unwrap();
        // Fixture tasks live in the throwaway session, not the caller's
        // durable store — only the eval record is persisted.
        let tasks = crate::ai_harness::task_list(&sess, &json!({})).unwrap();
        assert_eq!(tasks["tasks"].as_array().unwrap().len(), 0);
        assert_eq!(eval_files(&sess).unwrap().len(), 1);
    }
}
