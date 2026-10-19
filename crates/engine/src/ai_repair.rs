//! Bounded compile-repair workflow (M7.2).
//!
//! `ai.repair.preview` gathers diagnostics (structure issues always; a
//! fresh compile only when the buffer is clean and saved) and stores a
//! minimal line-level patch on a durable task as a `compile_repair`
//! plan. `ai.repair.apply` runs only after `ai.task.approve`, verifies
//! every `before` line against the live buffer, applies the patch, then
//! saves and recompiles so the outcome is recorded on the task.
//!
//! Repairs are deliberately convergent: unclosed environments, `\end`
//! mismatches and stray `\end` commands. Ambiguous issues (unclosed
//! braces, undefined commands) are surfaced as diagnostics only.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde_json::{json, Value};
use texparse::DocStructure;

use crate::ai_harness::{
    checkpoint, load_task, now_ms, permission, required_task_id, task_create, tier_name, touch_and_write_task, valid_task_id, GUARDRAILS_VERSION,
    SYSTEM_PROMPT_VERSION, TOOL_GUIDANCE_VERSION,
};
use crate::{EngineError, Session};

const PLAN_KIND: &str = "compile_repair";
const MAX_EDITS: usize = 32;
const MAX_DIAGNOSTICS: usize = 64;
const MAX_ERROR_SAMPLES: usize = 16;
const MAX_EDIT_TEXT: usize = 4096;

/// `ai.repair.preview` — gather diagnostics and store a minimal patch on
/// a durable task. Never mutates the buffer.
pub fn preview(sess: &mut Session, params: &Value) -> Result<Value, EngineError> {
    let want_compile = params.get("compile").and_then(Value::as_bool).unwrap_or(true);
    let structure = sess.structure();
    let mut diagnostics = Vec::new();
    for issue in &structure.issues {
        if diagnostics.len() >= MAX_DIAGNOSTICS {
            break;
        }
        diagnostics.push(json!({"source": "structure", "line": issue.pos.line, "message": issue.message}));
    }

    let mut compile_ctx = json!({"requested": want_compile});
    let mut before_errors: Option<usize> = None;
    if want_compile {
        if sess.dirty() {
            compile_ctx["skipped"] = json!("buffer has unsaved changes");
        } else if sess.path().is_none() {
            compile_ctx["skipped"] = json!("untitled buffer");
        } else {
            match sess.compile() {
                Ok(report) => {
                    before_errors = Some(report.errors.len());
                    compile_ctx["engine"] = json!(report.engine);
                    compile_ctx["errors"] = json!(report.errors.len());
                    compile_ctx["warnings"] = json!(report.warnings.len());
                    compile_ctx["pages"] = json!(report.pages);
                    compile_ctx["pdf"] = json!(report.pdf.as_ref().map(|p| p.display().to_string()));
                    for e in report.errors.iter().take(MAX_ERROR_SAMPLES) {
                        if diagnostics.len() >= MAX_DIAGNOSTICS {
                            break;
                        }
                        diagnostics.push(json!({
                            "source": "compile",
                            "file": e.file.as_ref().map(|p| p.display().to_string()),
                            "line": e.line,
                            "message": e.message,
                        }));
                    }
                }
                Err(e) => compile_ctx["error"] = json!(e.to_string()),
            }
        }
    }

    let lines: Vec<&str> = sess.source().split('\n').collect();
    let edits = propose_edits(&structure, &lines);
    if edits.is_empty() {
        if let Some(id) = params.get("task_id").and_then(Value::as_str).and_then(valid_task_id) {
            let mut task = load_task(sess, id)?;
            if !crate::ai_harness::TERMINAL_STATES.contains(&task.get("state").and_then(Value::as_str).unwrap_or("draft")) {
                checkpoint(&mut task, "repair preview: no repairable diagnostics", None);
                touch_and_write_task(sess, &mut task)?;
            }
        }
        return Ok(json!({
            "ok": true,
            "repairable": false,
            "diagnostics": diagnostics,
            "compile": compile_ctx,
            "plan": Value::Null,
        }));
    }

    // Follow-on commands, classified through the existing policy matrix.
    let steps = [("source.set", json!({"text": {"type": "string", "note": "patched buffer"}})), ("document.compile", json!({}))];
    let commands: Vec<Value> = steps
        .iter()
        .enumerate()
        .map(|(i, (command, p))| {
            let policy = permission(command, p);
            json!({
                "index": i,
                "command": command,
                "tier": policy.map(|pol| tier_name(pol.tier)).unwrap_or("unknown"),
                "approval_required": policy.map(|pol| pol.approval_required).unwrap_or(true),
                "reason": policy.map(|pol| pol.reason).unwrap_or("unclassified"),
                "params": p,
            })
        })
        .collect();

    let plan = json!({
        "version": 1,
        "kind": PLAN_KIND,
        "dry_run": true,
        "requires_approval": true,
        "prompt_version": SYSTEM_PROMPT_VERSION,
        "guardrails_version": GUARDRAILS_VERSION,
        "tool_guidance_version": TOOL_GUIDANCE_VERSION,
        "diagnostics": diagnostics,
        "edits": edits,
        "commands": commands,
        "context": {
            "path": sess.path().map(|p| p.display().to_string()),
            "dirty": sess.dirty(),
            "compile": compile_ctx,
            "before_errors": before_errors,
            "buffer_hash": source_hash(sess.source()),
        },
    });

    let (id, mut task) = match params.get("task_id").and_then(Value::as_str) {
        Some(id) => {
            valid_task_id(id).ok_or(EngineError::BadParam("task_id"))?;
            (id.to_string(), load_task(sess, id)?)
        }
        None => {
            let created = task_create(
                sess,
                &json!({
                    "title": "Repair compile diagnostics",
                    "prompt": format!("Apply a bounded repair patch of {} line edit(s), then recompile and record the result.", edits.len()),
                    "owner": "repair-agent",
                }),
            )?;
            let id = created.get("task_id").and_then(Value::as_str).unwrap_or_default().to_string();
            (id, created.get("task").cloned().unwrap_or_else(|| json!({})))
        }
    };
    let state = task.get("state").and_then(Value::as_str).unwrap_or("draft");
    if crate::ai_harness::TERMINAL_STATES.contains(&state) {
        return Err(EngineError::Tool("task is already in a terminal state".into()));
    }
    task["plan"] = plan.clone();
    task["state"] = Value::from("waiting_approval");
    checkpoint(&mut task, "repair previewed", Some(&format!("{} line edit(s)", edits.len())));
    touch_and_write_task(sess, &mut task)?;
    Ok(json!({"ok": true, "repairable": true, "task_id": id, "task": task, "plan": plan}))
}

/// `ai.repair.apply` — apply the approved `compile_repair` plan, then
/// save and recompile unless `recompile: false`. Fails closed when the
/// buffer no longer matches the previewed `before` lines.
pub fn apply(sess: &mut Session, params: &Value) -> Result<Value, EngineError> {
    let id = required_task_id(params)?;
    let recompile = params.get("recompile").and_then(Value::as_bool).unwrap_or(true);
    let mut task = load_task(sess, id)?;
    let state = task.get("state").and_then(Value::as_str).unwrap_or("draft").to_string();
    if crate::ai_harness::TERMINAL_STATES.contains(&state.as_str()) {
        return Err(EngineError::Tool("task is already in a terminal state".into()));
    }
    let plan = task.get("plan").cloned().ok_or_else(|| EngineError::Tool("task has no repair plan".into()))?;
    if plan.get("kind").and_then(Value::as_str) != Some(PLAN_KIND) {
        return Err(EngineError::Tool("task plan is not a compile_repair plan".into()));
    }
    if plan.get("approved").and_then(Value::as_bool) != Some(true) {
        return Err(EngineError::Tool("repair plan is not approved".into()));
    }
    task["state"] = Value::from("running");
    checkpoint(&mut task, "repair applying", None);
    touch_and_write_task(sess, &mut task)?;

    if plan.pointer("/context/buffer_hash").and_then(Value::as_u64) != Some(source_hash(sess.source())) {
        return fail_task(sess, &mut task, "buffer changed since the repair was previewed — preview again");
    }
    let edits = plan.get("edits").and_then(Value::as_array).cloned().unwrap_or_default();
    if edits.is_empty() {
        return fail_task(sess, &mut task, "repair plan has no edits");
    }
    let new_src = match apply_edits(sess.source(), &edits) {
        Ok(src) => src,
        Err(message) => return fail_task(sess, &mut task, &message),
    };
    if sess.disk_changed().unwrap_or(false) {
        return fail_task(sess, &mut task, "file changed on disk since it was loaded — repair not applied");
    }
    sess.set_source(new_src);
    checkpoint(&mut task, "patch applied to buffer", None);

    let mut result = json!({"applied": true, "edits": edits.len()});
    let mut final_state = "completed";
    if recompile {
        if sess.path().is_none() {
            result["verified"] = json!(false);
            result["verify_note"] = json!("untitled buffer — nothing to compile");
        } else {
            match sess.compile() {
                Ok(report) => {
                    let before = plan.pointer("/context/before_errors").and_then(Value::as_u64).map(|n| n as usize);
                    result["verified"] = json!(true);
                    result["before_errors"] = json!(before);
                    result["after_errors"] = json!(report.errors.len());
                    result["pages"] = json!(report.pages);
                    result["pdf"] = json!(report.pdf.as_ref().map(|p| p.display().to_string()));
                    let improved = report.errors.is_empty() || before.is_some_and(|b| report.errors.len() < b);
                    if !improved {
                        final_state = "failed";
                        task["error"] = json!(format!("{} error(s) remain after the patch", report.errors.len()));
                    }
                    checkpoint(&mut task, "recompiled", Some(&format!("{} error(s)", report.errors.len())));
                }
                Err(e) => {
                    result["verified"] = json!(false);
                    result["verify_error"] = json!(e.to_string());
                    final_state = "blocked";
                    task["error"] = json!(format!("recompile unavailable: {e}"));
                }
            }
        }
    } else {
        result["verified"] = json!(false);
        result["verify_note"] = json!("recompile skipped by request");
    }

    task["state"] = Value::from(final_state);
    if crate::ai_harness::TERMINAL_STATES.contains(&final_state) {
        task["finished_ms"] = json!(now_ms());
    }
    task["result"] = result.clone();
    task["changed_paths"] = json!([sess.path().map(|p| p.display().to_string()).unwrap_or_else(|| "buffer".to_string())]);
    checkpoint(&mut task, &format!("repair {final_state}"), None);
    touch_and_write_task(sess, &mut task)?;
    Ok(json!({"ok": true, "task": task, "result": result}))
}

/// Mark the task failed, persist it, and surface the reason as a tool
/// error so callers see both the durable state and the failure.
fn fail_task(sess: &Session, task: &mut Value, message: &str) -> Result<Value, EngineError> {
    task["state"] = Value::from("failed");
    task["finished_ms"] = json!(now_ms());
    task["error"] = Value::from(message);
    checkpoint(task, "repair failed", Some(message));
    let _ = touch_and_write_task(sess, task);
    Err(EngineError::Tool(message.to_string()))
}

/// Build the minimal edit list from structural issues. Line numbers are
/// 1-based against `lines` (`src.split('\n')`); `before` is the current
/// line text the applier must see before mutating.
///
/// `closed` is a multiset ledger of environments our own fixes close, so
/// later issues are judged against the patched state rather than the raw
/// parse. That keeps a patch from fixing one issue by creating another.
fn propose_edits(d: &DocStructure, lines: &[&str]) -> Vec<Value> {
    let mut unclosed: Vec<String> = Vec::new();
    for issue in &d.issues {
        if let Some(name) = parse_unclosed_begin(&issue.message) {
            unclosed.push(name);
        }
    }

    let mut edits = Vec::new();
    let mut closed: Vec<String> = Vec::new();
    for issue in &d.issues {
        if edits.len() >= MAX_EDITS {
            break;
        }
        let message = issue.message.as_str();
        let line = issue.pos.line;
        if parse_unclosed_begin(message).is_some() {
            continue;
        }
        if let Some((got, want)) = parse_end_mismatch(message) {
            if closed.iter().any(|n| n == &want) {
                // An earlier fix already closed `want`; this `\end{got}`
                // either legitimately closes its own open env or is stray.
                if unclosed.iter().any(|n| n == &got) {
                    closed.push(got);
                } else {
                    stray_end_edit(&mut edits, lines, line, &got);
                }
            } else if has_later_end(lines, &want, line) {
                // A proper `\end{want}` exists further down, so this
                // `\end{got}` is junk, not a typo of the wanted name.
                stray_end_edit(&mut edits, lines, line, &got);
            } else if got == "document" {
                // `\end{document}` is almost certainly intentional; close
                // the inner env just before it instead of rewriting it.
                insert_before_edit(
                    &mut edits,
                    lines,
                    line,
                    &format!("\\end{{{want}}}"),
                    &format!("close \\begin{{{want}}} before \\end{{document}}"),
                );
                closed.push(want);
                closed.push("document".to_string());
            } else {
                let before = line_at(lines, line);
                let after = before.replacen(&format!("\\end{{{got}}}"), &format!("\\end{{{want}}}"), 1);
                if after != before {
                    closed.push(want.clone());
                    edits.push(json!({
                        "op": "replace_line",
                        "line": line,
                        "before": before,
                        "after": after,
                        "reason": format!("\\end{{{got}}} does not match open \\begin{{{want}}}"),
                    }));
                }
            }
            continue;
        }
        if let Some(name) = parse_stray_end(message) {
            stray_end_edit(&mut edits, lines, line, &name);
        }
    }

    // Close still-open environments, innermost first, in one insert so
    // ordering inside the patch cannot depend on edit sequencing.
    let mut texts = Vec::new();
    for name in unclosed.iter().rev() {
        if let Some(i) = closed.iter().position(|n| n == name) {
            closed.remove(i);
            continue;
        }
        texts.push(format!("\\end{{{name}}}"));
    }
    if !texts.is_empty() && edits.len() < MAX_EDITS {
        let anchor = d.envs.iter().find(|e| e.name == "document").map(|e| e.end.line);
        let line = anchor.unwrap_or_else(|| eof_anchor(lines));
        insert_before_edit(&mut edits, lines, line, &texts.join("\n"), "close unclosed environment(s)");
    }
    edits.retain(|e| e.get("after").and_then(Value::as_str).map(|a| a.len() <= MAX_EDIT_TEXT).unwrap_or(true));
    edits
}

fn insert_before_edit(edits: &mut Vec<Value>, lines: &[&str], line: usize, text: &str, reason: &str) {
    edits.push(json!({
        "op": "insert_before",
        "line": line,
        "before": line_at(lines, line),
        "after": text,
        "reason": reason,
    }));
}

/// Remove a stray `\end{name}` from its line — delete the line when the
/// command was the only content, otherwise rewrite the line.
fn stray_end_edit(edits: &mut Vec<Value>, lines: &[&str], line: usize, name: &str) {
    let before = line_at(lines, line);
    let after = before.replacen(&format!("\\end{{{name}}}"), "", 1);
    if after == before {
        return;
    }
    if after.trim().is_empty() {
        edits.push(json!({
            "op": "delete_line",
            "line": line,
            "before": before,
            "after": Value::Null,
            "reason": format!("\\end{{{name}}} has no matching \\begin"),
        }));
    } else {
        edits.push(json!({
            "op": "replace_line",
            "line": line,
            "before": before,
            "after": after,
            "reason": format!("\\end{{{name}}} has no matching \\begin"),
        }));
    }
}

/// Verify every `before` anchor, then apply edits bottom-up so line
/// numbers stay stable. Same-line order: delete → replace → insert.
fn apply_edits(src: &str, edits: &[Value]) -> Result<String, String> {
    let mut lines: Vec<String> = src.split('\n').map(str::to_string).collect();
    let mut order: Vec<usize> = (0..edits.len()).collect();
    order.sort_by(|a, b| {
        let la = edits[*a].get("line").and_then(Value::as_u64).unwrap_or(0);
        let lb = edits[*b].get("line").and_then(Value::as_u64).unwrap_or(0);
        lb.cmp(&la).then_with(|| op_rank(&edits[*a]).cmp(&op_rank(&edits[*b])))
    });
    for &i in &order {
        let edit = &edits[i];
        let op = edit.get("op").and_then(Value::as_str).unwrap_or_default();
        let line = edit.get("line").and_then(Value::as_u64).ok_or("repair edit missing line")? as usize;
        let before = edit.get("before").and_then(Value::as_str).unwrap_or_default();
        let current = lines.get(line.saturating_sub(1)).map(String::as_str).unwrap_or("");
        if current != before {
            return Err(format!("buffer changed since preview at line {line} — run ai.repair.preview again"));
        }
        match op {
            "insert_before" => {
                let after = edit.get("after").and_then(Value::as_str).unwrap_or_default();
                lines.insert(line.saturating_sub(1).min(lines.len()), after.to_string());
            }
            "replace_line" => {
                let after = edit.get("after").and_then(Value::as_str).unwrap_or_default();
                if line == 0 || line > lines.len() {
                    return Err(format!("repair edit line {line} is out of range"));
                }
                lines[line - 1] = after.to_string();
            }
            "delete_line" => {
                if line == 0 || line > lines.len() {
                    return Err(format!("repair edit line {line} is out of range"));
                }
                lines.remove(line - 1);
            }
            _ => return Err(format!("unknown repair edit op '{op}'")),
        }
    }
    Ok(lines.join("\n"))
}

/// Does `\end{name}` appear anywhere after `line` (1-based)? Then the
/// mismatched `\end{got}` is junk rather than a misspelt `\end{name}`.
fn has_later_end(lines: &[&str], name: &str, line: usize) -> bool {
    let needle = format!("\\end{{{name}}}");
    lines.iter().skip(line).any(|l| l.contains(&needle))
}

fn op_rank(edit: &Value) -> u8 {
    match edit.get("op").and_then(Value::as_str) {
        Some("delete_line") => 0,
        Some("replace_line") => 1,
        _ => 2,
    }
}

/// Cheap non-cryptographic fingerprint of the buffer so `apply` fails
/// closed when the source changed after the preview was generated.
fn source_hash(src: &str) -> u64 {
    let mut h = DefaultHasher::new();
    src.hash(&mut h);
    h.finish()
}

fn line_at<'a>(lines: &'a [&'a str], line: usize) -> &'a str {
    lines.get(line.saturating_sub(1)).copied().unwrap_or("")
}

fn eof_anchor(lines: &[&str]) -> usize {
    if lines.last() == Some(&"") {
        lines.len()
    } else {
        lines.len() + 1
    }
}

fn parse_unclosed_begin(message: &str) -> Option<String> {
    message.strip_prefix("unclosed \\begin{")?.strip_suffix('}').map(str::to_string)
}

fn parse_end_mismatch(message: &str) -> Option<(String, String)> {
    let rest = message.strip_prefix("\\end{")?;
    let (got, rest) = rest.split_once("} but \\begin{")?;
    let want = rest.strip_suffix("} is still open")?;
    Some((got.to_string(), want.to_string()))
}

fn parse_stray_end(message: &str) -> Option<String> {
    let rest = message.strip_prefix("\\end{")?;
    let (name, suffix) = rest.split_once('}')?;
    (suffix == " without \\begin").then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let n = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("warraqcraft-repair-test-{}-{tag}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn session(dir: &Path, src: &str) -> Session {
        let path = dir.join("main.tex");
        fs::write(&path, src).unwrap();
        let mut s = Session::new();
        s.open(&path).unwrap();
        s
    }

    fn approve_and_apply(sess: &mut Session, task_id: &str) -> Value {
        crate::ai_harness::task_approve(&*sess, &json!({"task_id": task_id, "approve": true})).unwrap();
        apply(sess, &json!({"task_id": task_id, "recompile": false})).unwrap()
    }

    #[test]
    fn unclosed_envs_close_innermost_first() {
        let dir = TempDir::new("unclosed");
        let mut sess = session(&dir.0, "\\documentclass{article}\n\\begin{document}\n\\begin{itemize}\n\\item x\n");
        let out = preview(&mut sess, &json!({"compile": false})).unwrap();
        assert_eq!(out["repairable"], true);
        let edits = out["plan"]["edits"].as_array().unwrap();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0]["op"], "insert_before");
        assert_eq!(edits[0]["after"], "\\end{itemize}\n\\end{document}");
        assert_eq!(out["task"]["state"], "waiting_approval");
        let applied = approve_and_apply(&mut sess, out["task_id"].as_str().unwrap());
        assert_eq!(applied["task"]["state"], "completed");
        assert!(sess.source().ends_with("\\end{itemize}\n\\end{document}\n"));
        assert_eq!(sess.structure().issues.len(), 0, "issues remain: {:?}", sess.structure().issues);
    }

    #[test]
    fn end_mismatch_replaces_wrong_env_name() {
        let dir = TempDir::new("mismatch");
        let mut sess = session(&dir.0, "\\begin{document}\n\\begin{a}\n\\end{b}\n\\end{document}\n");
        let out = preview(&mut sess, &json!({"compile": false})).unwrap();
        let edits = out["plan"]["edits"].as_array().unwrap();
        // The mismatch fix closes `a`, so no extra unclosed-\begin{a} edit.
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0]["op"], "replace_line");
        assert!(edits[0]["after"].as_str().unwrap().contains("\\end{a}"));
        approve_and_apply(&mut sess, out["task_id"].as_str().unwrap());
        assert!(sess.source().contains("\\end{a}"));
        assert!(!sess.structure().issues.iter().any(|i| i.message.contains("begin{a}")));
    }

    #[test]
    fn stray_end_is_deleted() {
        let dir = TempDir::new("stray");
        let mut sess = session(&dir.0, "\\begin{document}\ntext\n\\end{foo}\n\\end{document}\n");
        let out = preview(&mut sess, &json!({"compile": false})).unwrap();
        let edits = out["plan"]["edits"].as_array().unwrap();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0]["op"], "delete_line");
        approve_and_apply(&mut sess, out["task_id"].as_str().unwrap());
        assert!(!sess.source().contains("\\end{foo}"));
    }

    #[test]
    fn apply_requires_approval_and_fresh_buffer() {
        let dir = TempDir::new("guard");
        let mut sess = session(&dir.0, "\\begin{document}\n\\begin{a}\n");
        let out = preview(&mut sess, &json!({"compile": false})).unwrap();
        let id = out["task_id"].as_str().unwrap();
        // Not approved yet → refused, and the buffer is untouched.
        assert!(apply(&mut sess, &json!({"task_id": id, "recompile": false})).is_err());
        crate::ai_harness::task_approve(&sess, &json!({"task_id": id, "approve": true})).unwrap();
        // Buffer edited after the preview → fail closed, task marked failed.
        sess.set_source("something else entirely\n".to_string());
        assert!(apply(&mut sess, &json!({"task_id": id, "recompile": false})).is_err());
        let task = crate::ai_harness::task_get(&sess, &json!({"task_id": id})).unwrap();
        assert_eq!(task["task"]["state"], "failed");
    }

    #[test]
    fn nested_stray_end_prefers_delete_over_replace() {
        // `\end{foo}` sits inside an open env that is closed properly below —
        // the minimal patch removes the stray line, it does not rename it.
        let dir = TempDir::new("nested");
        let mut sess = session(&dir.0, "\\begin{document}\n\\begin{a}\nx\n\\end{foo}\n\\end{a}\n\\end{document}\n");
        let out = preview(&mut sess, &json!({"compile": false})).unwrap();
        let edits = out["plan"]["edits"].as_array().unwrap();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0]["op"], "delete_line");
        assert_eq!(edits[0]["line"], 4);
        approve_and_apply(&mut sess, out["task_id"].as_str().unwrap());
        assert_eq!(sess.source(), "\\begin{document}\n\\begin{a}\nx\n\\end{a}\n\\end{document}\n");
        assert!(sess.structure().issues.is_empty(), "issues remain: {:?}", sess.structure().issues);
    }

    #[test]
    fn hostile_input_never_panics_and_stays_bounded() {
        let dir = TempDir::new("hostile");
        let deep = "\\begin{x}".repeat(500);
        let ends = "\\end{y}".repeat(500);
        let mut sess = session(&dir.0, &format!("\\begin{{document}}\n{deep}\n{ends}\n\\end{{document}}\n"));
        let out = preview(&mut sess, &json!({"compile": false})).unwrap();
        if out["repairable"] == true {
            let edits = out["plan"]["edits"].as_array().unwrap();
            assert!(edits.len() <= MAX_EDITS);
        }
        // Garbage lines still produce a bounded, well-formed response.
        let mut sess2 = session(&dir.0, "\\end{}\n\\begin\n\\end\n{{{");
        let out2 = preview(&mut sess2, &json!({"compile": false})).unwrap();
        assert_eq!(out2["ok"], true);
    }

    #[test]
    fn clean_document_is_not_repairable() {
        let dir = TempDir::new("clean");
        let mut sess = session(&dir.0, "\\begin{document}\nok\n\\end{document}\n");
        let out = preview(&mut sess, &json!({"compile": false})).unwrap();
        assert_eq!(out["repairable"], false);
        assert!(out["plan"].is_null());
    }
}
