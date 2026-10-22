//! WarraqCraft MCP server (L6): Model Context Protocol over the engine
//! [`Session`].
//!
//! **Opt-in only.** Nothing in WarraqCraft starts this server on its own: it
//! runs when a user (or an agent the user configured) launches it, and it
//! never opens a network port — the transport is newline-delimited JSON-RPC
//! 2.0 (NDJSON) over stdin/stdout, one message per line.
//!
//! Implemented: `initialize`, `ping`, `tools/list`, `tools/call`,
//! `resources/list`, `resources/templates/list`, `resources/read`, and the
//! `notifications/*` a client sends (never answered). Each `tools/call` tool
//! maps one-to-one onto a [`Session::execute`] command; engine failures are
//! reported inside the result (`isError: true`) so the agent can read them,
//! while protocol failures use JSON-RPC error codes. The open document is
//! also exposed read-only as `warraqcraft://doc/source`,
//! `warraqcraft://doc/structure` and `warraqcraft://doc/stats`.
//!
//! Input is untrusted: lines are capped at 4 MiB (longer ones get a parse
//! error and are drained), malformed JSON is a `-32700` reply, and no code
//! path panics.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::io::{self, BufRead, Read, Write};

use serde_json::{json, Value};
use warraqcraft_engine::Session;

/// Protocol revisions we speak, newest first.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// A single NDJSON line is capped at 4 MiB; anything longer is answered
/// with a parse error and drained, never buffered whole.
const MAX_LINE: usize = 4 * 1024 * 1024;
/// Chunk size for draining the rest of an over-long line.
const DRAIN_CHUNK: u64 = 64 * 1024;

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;

const SOURCE_URI: &str = "warraqcraft://doc/source";
const STRUCTURE_URI: &str = "warraqcraft://doc/structure";
const STATS_URI: &str = "warraqcraft://doc/stats";
const AI_TASKS_URI: &str = "warraqcraft://ai/tasks";
const AI_PERMISSIONS_URI: &str = "warraqcraft://ai/permissions";
const AI_PROMPTS_URI: &str = "warraqcraft://ai/prompts";
const AI_AUDIT_URI: &str = "warraqcraft://ai/audit";
const AI_COMPONENTS_URI: &str = "warraqcraft://ai/components";

const INSTRUCTIONS: &str =
    "WarraqCraft edits LaTeX documents. Start an untitled buffer with document_new, open a .tex file with document_open or set the buffer directly \
with source_set, then run the tools: source_get, structure_get and magic_get inspect the buffer, document_compile typesets \
it with tectonic or a system TeX engine, math_preview renders a math fragment, text_stats reports document statistics, \
template_* create documents from the shipped gallery, user_tool_* run configured external tools, \
project_structure merges the outline across the input/include graph, repo_* inspect and run guarded fixed-argv Git collaboration commands, ai_probe/ai_ask call a configured OpenAI-compatible provider without storing its token, ai_task_*/ai_memory_*/ai_permissions/ai_plan_preview/ai_prompt_*/ai_audit_list expose the durable AI harness, ai_repair_preview/ai_repair_apply run the approval-gated compile-repair workflow, ai_skill_*/ai_components/ai_task_evidence expose the inspectable harness component tree, skills and task evidence, ai_eval_*/ai_eval_run list and run deterministic model-free repair evaluations, and \
project_snapshot/snapshot_* keep local file history. The open document is also readable as resources \
warraqcraft://doc/source (text/plain), warraqcraft://doc/structure and warraqcraft://doc/stats (application/json), and AI state is readable as \
warraqcraft://ai/tasks, warraqcraft://ai/permissions, warraqcraft://ai/prompts, warraqcraft://ai/audit and warraqcraft://ai/components — read-only \
snapshots of the live buffer and harness.";

/// MCP tool name → engine command id. The two tables stay in sync — a test
/// pins the count and the name set.
const TOOL_COMMANDS: &[(&str, &str)] = &[
    ("document_new", "document.new"),
    ("document_open", "document.open"),
    ("document_save", "document.save"),
    ("document_disk_state", "document.disk_state"),
    ("document_compile", "document.compile"),
    ("source_set", "source.set"),
    ("source_get", "source.get"),
    ("structure_get", "structure.get"),
    ("magic_get", "magic.get"),
    ("user_tool_list", "user_tool.list"),
    ("user_tool_set", "user_tool.set"),
    ("user_tool_run", "user_tool.run"),
    ("user_tool_clear", "user_tool.clear"),
    ("template_list", "template.list"),
    ("template_read", "template.read"),
    ("template_new", "template.new"),
    ("template_import", "template.import"),
    ("project_graph", "project.graph"),
    ("project_structure", "project.structure"),
    ("toolchain_probe", "toolchain.probe"),
    ("cite_fetch", "cite.fetch"),
    ("bib_audit", "bib.audit"),
    ("bib_import", "bib.import"),
    ("doc_suggest", "doc.suggest"),
    ("doc_suggest_list", "doc.suggest.list"),
    ("doc_suggest_apply", "doc.suggest.apply"),
    ("doc_suggest_reject", "doc.suggest.reject"),
    ("snippet_list", "snippet.list"),
    ("snippet_expand", "snippet.expand"),
    ("library_attach", "library.attach"),
    ("library_list", "library.list"),
    ("library_get", "library.get"),
    ("library_detach", "library.detach"),
    ("repo_status", "repo.status"),
    ("repo_init", "repo.init"),
    ("repo_remote", "repo.remote"),
    ("repo_pull", "repo.pull"),
    ("repo_push", "repo.push"),
    ("repo_commit", "repo.commit"),
    ("repo_branches", "repo.branches"),
    ("repo_conflicts", "repo.conflicts"),
    ("repo_history", "repo.history"),
    ("ai_providers", "ai.providers"),
    ("ai_probe", "ai.probe"),
    ("ai_ask", "ai.ask"),
    ("ai_task_create", "ai.task.create"),
    ("ai_task_list", "ai.task.list"),
    ("ai_task_get", "ai.task.get"),
    ("ai_task_update", "ai.task.update"),
    ("ai_task_cancel", "ai.task.cancel"),
    ("ai_task_approve", "ai.task.approve"),
    ("ai_memory_list", "ai.memory.list"),
    ("ai_memory_set", "ai.memory.set"),
    ("ai_memory_delete", "ai.memory.delete"),
    ("ai_memory_export", "ai.memory.export"),
    ("ai_memory_import", "ai.memory.import"),
    ("ai_permissions", "ai.permissions"),
    ("ai_plan_preview", "ai.plan.preview"),
    ("ai_prompt_list", "ai.prompt.list"),
    ("ai_prompt_get", "ai.prompt.get"),
    ("ai_audit_list", "ai.audit.list"),
    ("ai_repair_preview", "ai.repair.preview"),
    ("ai_repair_apply", "ai.repair.apply"),
    ("ai_skill_list", "ai.skill.list"),
    ("ai_skill_get", "ai.skill.get"),
    ("ai_components", "ai.components"),
    ("ai_task_evidence", "ai.task.evidence"),
    ("ai_eval_list", "ai.eval.list"),
    ("ai_eval_run", "ai.eval.run"),
    ("math_preview", "math.preview"),
    ("text_stats", "text.stats"),
    ("project_snapshot", "project.snapshot"),
    ("snapshot_list", "snapshot.list"),
    ("snapshot_read", "snapshot.read"),
    ("snapshot_restore", "snapshot.restore"),
];

/// The stdio MCP server: one engine [`Session`] behind a line loop.
pub struct McpServer {
    session: Session,
}

impl Default for McpServer {
    fn default() -> Self {
        Self::new()
    }
}

impl McpServer {
    /// Server over a fresh, empty session.
    pub fn new() -> Self {
        Self { session: Session::new() }
    }

    /// Server over an existing session (e.g. an already-open document).
    pub fn with_session(session: Session) -> Self {
        Self { session }
    }

    /// The underlying engine session.
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// The underlying engine session, mutably.
    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    /// Serve newline-delimited JSON-RPC until `input` reaches end of file.
    /// Empty lines are skipped. A line over [`MAX_LINE`] bytes is answered
    /// with a parse error and its remainder is drained in bounded chunks —
    /// a hostile peer cannot make the server buffer an unbounded line.
    pub fn serve(&mut self, mut input: impl BufRead, mut output: impl Write) -> io::Result<()> {
        let mut buf: Vec<u8> = Vec::new();
        loop {
            buf.clear();
            let n = input.by_ref().take(MAX_LINE as u64 + 1).read_until(b'\n', &mut buf)?;
            if n == 0 {
                return Ok(());
            }
            // Line length without the trailing newline is what counts.
            let content_len = buf.len().saturating_sub(usize::from(buf.last() == Some(&b'\n')));
            if content_len > MAX_LINE {
                drain_line(&mut input)?;
                let reply = error(Value::Null, PARSE_ERROR, "line exceeds the 4 MiB limit");
                writeln!(output, "{reply}")?;
                output.flush()?;
                continue;
            }
            let line = String::from_utf8_lossy(&buf);
            if line.trim().is_empty() {
                continue;
            }
            if let Some(reply) = self.handle_line(&line) {
                writeln!(output, "{reply}")?;
                output.flush()?;
            }
        }
    }

    /// Handle one NDJSON line; returns the serialized reply, if one is due.
    /// Pure dispatch — unit-testable without I/O.
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        let reply = match serde_json::from_str::<Value>(line) {
            Ok(msg) => self.handle(&msg),
            Err(e) => Some(error(Value::Null, PARSE_ERROR, &format!("parse error: {e}"))),
        };
        reply.map(|r| r.to_string())
    }

    /// Handle one parsed message. Notifications (no `id`) and
    /// `notifications/*` get no reply.
    pub fn handle(&mut self, msg: &Value) -> Option<Value> {
        let Some(obj) = msg.as_object() else {
            return Some(error(Value::Null, INVALID_REQUEST, "expected a JSON-RPC request object"));
        };
        let id = obj.get("id").cloned();
        let Some(method) = obj.get("method").and_then(Value::as_str) else {
            // A response to something we sent (we send no requests) or garbage.
            return id.map(|id| error(id, INVALID_REQUEST, "missing method"));
        };
        if method.starts_with("notifications/") {
            return None;
        }
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        let id = id?; // requests only — a message without id is a notification
        Some(match self.dispatch(method, &params) {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err((code, message)) => error(id, code, &message),
        })
    }

    fn dispatch(&mut self, method: &str, params: &Value) -> Result<Value, (i64, String)> {
        match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(Value::as_str);
                let version = match asked {
                    Some(v) if PROTOCOL_VERSIONS.contains(&v) => v,
                    _ => PROTOCOL_VERSIONS.first().copied().unwrap_or("2025-06-18"),
                };
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": {}, "resources": {} },
                    "serverInfo": { "name": "warraqcraft", "title": "WarraqCraft", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": INSTRUCTIONS,
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tool_list() })),
            "tools/call" => self.call_tool(params),
            "resources/list" => Ok(json!({ "resources": self.resource_list() })),
            "resources/templates/list" => Ok(json!({ "resourceTemplates": resource_templates() })),
            "resources/read" => self.read_resource(params),
            other => Err((METHOD_NOT_FOUND, format!("method not found: {other}"))),
        }
    }

    /// `tools/call`: map the tool name to its engine command and run it.
    /// Engine errors land in the result (`isError: true`); protocol-level
    /// problems (missing name, unknown tool) are JSON-RPC errors.
    fn call_tool(&mut self, params: &Value) -> Result<Value, (i64, String)> {
        let name = params.get("name").and_then(Value::as_str).ok_or((INVALID_PARAMS, "tools/call needs a tool name".to_string()))?;
        let Some((_, command)) = TOOL_COMMANDS.iter().find(|(n, _)| *n == name) else {
            return Err((INVALID_PARAMS, format!("unknown tool '{name}'")));
        };
        let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
        match self.session.execute(command, &args) {
            Ok(result) => {
                let text = serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string());
                Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": false }))
            }
            Err(e) => Ok(json!({ "content": [{ "type": "text", "text": e.to_string() }], "isError": true })),
        }
    }

    /// `resources/list`: document resources while the session holds a
    /// document, plus the durable AI harness resources that are always
    /// inspectable read-only.
    fn resource_list(&self) -> Vec<Value> {
        let mut resources = Vec::new();
        if self.session.path().is_some() || !self.session.source().is_empty() {
            resources.extend([
                json!({ "uri": SOURCE_URI, "name": "Document source", "mimeType": "text/plain" }),
                json!({ "uri": STRUCTURE_URI, "name": "Document structure", "mimeType": "application/json" }),
                json!({ "uri": STATS_URI, "name": "Document statistics", "mimeType": "application/json" }),
            ]);
        }
        resources.extend([
            json!({ "uri": AI_TASKS_URI, "name": "AI tasks", "mimeType": "application/json" }),
            json!({ "uri": AI_PERMISSIONS_URI, "name": "AI command permissions", "mimeType": "application/json" }),
            json!({ "uri": AI_PROMPTS_URI, "name": "AI prompts and guardrails", "mimeType": "application/json" }),
            json!({ "uri": AI_AUDIT_URI, "name": "AI audit trail", "mimeType": "application/json" }),
            json!({ "uri": AI_COMPONENTS_URI, "name": "AI harness component tree", "mimeType": "application/json" }),
        ]);
        resources
    }

    /// `resources/read`: the live buffer, its structure, or its statistics.
    fn read_resource(&mut self, params: &Value) -> Result<Value, (i64, String)> {
        let uri = params.get("uri").and_then(Value::as_str).ok_or((INVALID_PARAMS, "resources/read needs a uri".to_string()))?;
        let (command, mime, json_text) = match uri {
            SOURCE_URI => ("source.get", "text/plain", false),
            STRUCTURE_URI => ("structure.get", "application/json", true),
            STATS_URI => ("text.stats", "application/json", true),
            AI_TASKS_URI => ("ai.task.list", "application/json", true),
            AI_PERMISSIONS_URI => ("ai.permissions", "application/json", true),
            AI_PROMPTS_URI => ("ai.prompt.list", "application/json", true),
            AI_AUDIT_URI => ("ai.audit.list", "application/json", true),
            AI_COMPONENTS_URI => ("ai.components", "application/json", true),
            _ => return Err((INVALID_PARAMS, format!("unknown resource uri '{uri}'"))),
        };
        let result = self.session.execute(command, &json!({})).map_err(|e| (INTERNAL_ERROR, e.to_string()))?;
        let text = if json_text {
            serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string())
        } else {
            result.get("text").and_then(Value::as_str).unwrap_or_default().to_string()
        };
        Ok(json!({ "contents": [{ "uri": uri, "mimeType": mime, "text": text }] }))
    }
}

/// Consume the rest of an over-long line in bounded chunks so the next read
/// resynchronises at a newline (or EOF).
fn drain_line(input: &mut impl BufRead) -> io::Result<()> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        let n = input.by_ref().take(DRAIN_CHUNK).read_until(b'\n', &mut buf)?;
        if n == 0 || buf.last() == Some(&b'\n') {
            return Ok(());
        }
    }
}

/// A JSON-RPC error response.
fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// The same URIs as `resources/templates/list` entries.
fn resource_templates() -> Value {
    json!([
        { "uriTemplate": SOURCE_URI, "name": "Document source", "mimeType": "text/plain",
          "description": "The current LaTeX source buffer, as text." },
        { "uriTemplate": STRUCTURE_URI, "name": "Document structure", "mimeType": "application/json",
          "description": "The parsed structure of the open document (as structure_get)." },
        { "uriTemplate": STATS_URI, "name": "Document statistics", "mimeType": "application/json",
          "description": "Word/character counts and structural tallies of the open document (as text_stats)." },
        { "uriTemplate": AI_TASKS_URI, "name": "AI tasks", "mimeType": "application/json",
          "description": "Durable AI task summaries (as ai_task_list)." },
        { "uriTemplate": AI_PERMISSIONS_URI, "name": "AI command permissions", "mimeType": "application/json",
          "description": "Inspectable command permission matrix (as ai_permissions)." },
        { "uriTemplate": AI_PROMPTS_URI, "name": "AI prompts and guardrails", "mimeType": "application/json",
          "description": "Versioned AI prompt/guardrail registry entries (as ai_prompt_list)." },
        { "uriTemplate": AI_AUDIT_URI, "name": "AI audit trail", "mimeType": "application/json",
          "description": "Redacted AI/provider/mutation audit events (as ai_audit_list)." },
        { "uriTemplate": AI_COMPONENTS_URI, "name": "AI harness component tree", "mimeType": "application/json",
          "description": "Inspectable prompts, guardrails, skills, memory scopes, tasks, audit and evaluations (as ai_components)." },
    ])
}

/// One `tools/list` entry.
fn tool(name: &str, description: &str, properties: Value, required: &[&str], read_only: bool, destructive: bool) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        },
        "annotations": { "readOnlyHint": read_only, "destructiveHint": destructive },
    })
}

/// A `{"type":"string"}` schema property with a description.
fn s(desc: &str) -> Value {
    json!({ "type": "string", "description": desc })
}

/// The `tools/list` table: one tool per engine command.
fn tool_list() -> Vec<Value> {
    vec![
        tool(
            "document_new",
            "Replace the session with an untitled, unsaved buffer. Refused while the buffer is dirty unless discard=true is supplied.",
            json!({
                "text": s("Optional starter source for the new untitled buffer."),
                "discard": { "type": "boolean", "description": "Explicitly discard the current dirty buffer." },
            }),
            &[],
            false,
            false,
        ),
        tool(
            "document_open",
            "Open a .tex file into the session. Refused while the buffer is dirty unless discard=true is supplied.",
            json!({
                "path": s("Path of the .tex file to open."),
                "discard": { "type": "boolean", "description": "Explicitly discard the current dirty buffer." },
            }),
            &["path"],
            false,
            false,
        ),
        tool(
            "document_save",
            "Write the buffer to its file, or to the given path. If the file changed on disk, save is refused unless overwrite=true is supplied after explicit confirmation.",
            json!({
                "path": s("Optional new path to save to."),
                "overwrite": { "type": "boolean", "description": "Explicit confirmation that the current buffer may overwrite newer disk contents." },
            }),
            &[],
            false,
            true,
        ),
        tool(
            "document_disk_state",
            "Report whether the open file changed or disappeared on disk since it was loaded or last saved.",
            json!({}),
            &[],
            true,
            false,
        ),
        tool(
            "document_compile",
            "Save if dirty, then compile with tectonic or a system TeX engine into .warraqcraft/ next to the document.",
            json!({}),
            &[],
            false,
            true,
        ),
        tool(
            "source_set",
            "Replace the document's LaTeX source buffer.",
            json!({ "text": s("The full LaTeX source text.") }),
            &["text"],
            false,
            false,
        ),
        tool("source_get", "Return the current LaTeX source buffer.", json!({}), &[], true, false),
        tool(
            "structure_get",
            "Return the parsed document structure (class, packages, headings, labels, citations, issues).",
            json!({}),
            &[],
            true,
            false,
        ),
        tool(
            "magic_get",
            "Return the buffer's % !TeX / % !BIB header directives (engine, root, encoding, spellcheck hints).",
            json!({}),
            &[],
            true,
            false,
        ),
        tool("user_tool_list", "List the configured TeXstudio-style user tools.", json!({}), &[], true, false),
        tool(
            "user_tool_set",
            "Replace the user-tool list; each entry has name, argv (array), and optional cwd and capture.",
            json!({ "tools": { "type": "array", "description": "User tools: {name, argv: [..], cwd?, capture?}." } }),
            &["tools"],
            false,
            false,
        ),
        tool(
            "user_tool_run",
            "Run a configured user tool by name, or a one-shot inline tool given argv; capture=false detaches it (background).",
            json!({
                "name": s("Name of a configured user tool."),
                "argv": { "type": "array", "items": { "type": "string" }, "description": "Inline one-shot command argv (fixed argv, no shell)." },
                "cwd": s("Working directory override."),
                "capture": { "type": "boolean", "description": "Capture output (default true); false runs detached." },
                "timeout_s": { "type": "integer", "description": "Timeout in seconds, clamped 1-300 (default 60)." },
            }),
            &[],
            false,
            true,
        ),
        tool("user_tool_clear", "Remove all configured user tools.", json!({}), &[], false, false),
        tool("template_list", "List the shipped document templates.", json!({}), &[], true, false),
        tool("template_read", "Read one template's LaTeX source by id.", json!({ "id": s("Template id from template_list.") }), &["id"], true, false),
        tool(
            "template_new",
            "Create a new .tex file from a template (writes to disk; refuses to overwrite unless force is true).",
            json!({
                "id": s("Template id from template_list."),
                "path": s("Destination file path."),
                "force": { "type": "boolean", "description": "Overwrite an existing file (default false)." },
            }),
            &["id", "path"],
            false,
            true,
        ),
        tool(
            "template_import",
            "Import a user-selected template file or folder into the per-user gallery (copies text sources only; force replaces an existing imported template).",
            json!({
                "path": s("Template source file or folder path."),
                "id": s("Optional safe template id."),
                "name": s("Optional display name."),
                "desc": s("Optional description."),
                "lang": s("Optional document language code."),
                "category": s("Optional gallery category."),
                "engine": s("Optional TeX engine hint."),
                "license": s("License/provenance text supplied by the user."),
                "main": s("Optional main .tex path inside a folder template."),
                "force": { "type": "boolean", "description": "Replace an existing imported template (default false)." },
            }),
            &["path"],
            false,
            true,
        ),
        tool(
            "project_graph",
            "Return the reachable file graph of the open document (sources, resources, unresolved deps).",
            json!({}),
            &[],
            true,
            false,
        ),
        tool(
            "project_structure",
            "Return the merged outline, labels and cite keys across every .tex file reachable from the open document (or its % !TeX root master).",
            json!({}),
            &[],
            true,
            false,
        ),
        tool(
            "toolchain_probe",
            "Probe the TeX toolchain: every known tool (tectonic, xelatex, lualatex, pdflatex, bibtex, biber, latexmk, synctex, git) reported as found/missing/broken with resolved path and version line.",
            json!({}),
            &[],
            true,
            false,
        ),
        tool(
            "cite_fetch",
            "Resolve a pasted identifier (DOI in any form, arXiv id/link, PMID/PMCID) to a BibTeX entry via keyless public resolvers, append it to the project .bib, and return the cite key. Duplicates return the existing key; unresolvable ids become pending stubs.",
            json!({
                "identifier": s("The identifier text — bare DOI, doi.org link, arXiv id/link, pmid:NNN, PMC…."),
                "bib": s("Optional project-relative .bib filename to write into (default: first declared bibliography, else refs.bib)."),
            }),
            &["identifier"],
            false,
            false,
        ),
        tool(
            "doc_suggest",
            "Propose an edit without applying it, anchored by a TextQuoteSelector-style {find + optional prefix/suffix} triple instead of byte offsets. Returns a proposal id and live status (anchored/ambiguous/stale).",
            json!({
                "find": s("Exact text the proposal is about."),
                "replace": s("Replacement text; omit for a comment-only annotation."),
                "comment": s("Reviewer note."),
                "prefix": s("Optional text expected immediately before `find` (disambiguation)."),
                "suffix": s("Optional text expected immediately after `find`."),
            }),
            &["find"],
            false,
            false,
        ),
        tool(
            "doc_suggest_list",
            "List pending anchored edit proposals with live status.",
            json!({}),
            &[],
            true,
            false,
        ),
        tool(
            "doc_suggest_apply",
            "Apply a pending suggestion: re-resolves the quote anchor against the live buffer (survives offset shifts), refuses stale or ambiguous anchors. Requires confirm=true.",
            json!({
                "id": s("Proposal id from doc_suggest/doc_suggest_list."),
                "confirm": { "type": "boolean", "description": "Must be true to apply." },
            }),
            &["id", "confirm"],
            false,
            false,
        ),
        tool(
            "doc_suggest_reject",
            "Drop a pending suggestion without touching the buffer.",
            json!({ "id": s("Proposal id.") }),
            &["id"],
            false,
            false,
        ),
        tool(
            "snippet_list",
            "List all snippets merged from built-ins, the global snippets.json, and the project's .warraqcraft/snippets.json — trigger, description, context (math/text/any), source.",
            json!({}),
            &[],
            true,
            false,
        ),
        tool(
            "snippet_expand",
            "Expand a snippet trigger: returns insertion body with ${n:default} placeholders rendered and the byte offset of the first tabstop ($1, else $0, else end).",
            json!({ "trigger": s("Snippet trigger, e.g. fig, tab, eq.") }),
            &["trigger"],
            true,
            false,
        ),
        tool(
            "library_attach",
            "Attach a PDF to a cite key: copies the file into the project's .warraqcraft/library/ and indexes it. pdf may be project-relative or absolute.",
            json!({ "key": s("Cite key."), "pdf": s("Path to the PDF file.") }),
            &["key", "pdf"],
            false,
            false,
        ),
        tool(
            "library_list",
            "List PDF attachments in the project library.",
            json!({}),
            &[],
            true,
            false,
        ),
        tool(
            "library_get",
            "Get the attached PDF path for a cite key, or null.",
            json!({ "key": s("Cite key.") }),
            &["key"],
            true,
            false,
        ),
        tool(
            "library_detach",
            "Remove a cite key's PDF attachment (index entry and stored file).",
            json!({ "key": s("Cite key.") }),
            &["key"],
            false,
            true,
        ),
        tool(
            "bib_audit",
            "Audit the project's .bib files: probable duplicate entries (shared DOI/eprint/normalized title across different keys), pending cite.fetch stubs awaiting retry, and per-file parse issue counts. Reports only — never merges or rewrites.",
            json!({}),
            &[],
            true,
            false,
        ),
        tool(
            "bib_import",
            "Import an external bibliography export (CSL-JSON from Zotero/Better BibTeX, or RIS) into a project .bib file. Format is sniffed from content; entries are converted to BibTeX, deduplicated by DOI/eprint/normalized title, and appended with generated unique keys.",
            json!({
                "path": s("Source file to read — project-relative or absolute."),
                "data": s("Raw CSL-JSON/RIS text — alternative to path (e.g. piped content)."),
                "bib": s("Optional project-relative .bib filename to write into (default: first declared bibliography, else refs.bib)."),
            }),
            &[],
            false,
            false,
        ),
        tool(
            "repo_status",
            "Inspect the project's Git repository: branch, upstream, ahead/behind, changed paths, conflicts, and buffer staleness.",
            json!({
                "remote": s("Remote name to inspect (default origin)."),
                "timeout_s": { "type": "integer", "description": "Timeout in seconds, clamped 1-300." },
            }),
            &[],
            true,
            false,
        ),
        tool(
            "repo_init",
            "Initialize Git in the project directory; optionally records a remote URL. Writes .git metadata but never stores credentials.",
            json!({
                "remote": s("Optional remote URL to add after git init."),
                "remote_name": s("Remote name for the optional URL (default origin)."),
                "timeout_s": { "type": "integer", "description": "Timeout in seconds, clamped 1-300." },
            }),
            &[],
            false,
            true,
        ),
        tool(
            "repo_remote",
            "Add or update a Git remote on the project repository. Writes the URL to Git config only; credentials are redacted in responses.",
            json!({
                "name": s("Remote name (default origin)."),
                "url": s("Remote URL or local repository path."),
                "timeout_s": { "type": "integer", "description": "Timeout in seconds, clamped 1-300." },
            }),
            &["url"],
            false,
            true,
        ),
        tool(
            "repo_pull",
            "Run git pull with the explicit strategy ff-only (default) or rebase. Network operation; refused while the buffer or file has unsaved/stale state.",
            json!({
                "remote": s("Optional remote name."),
                "branch": s("Optional branch name; requires remote."),
                "strategy": { "type": "string", "enum": ["ff-only", "rebase"], "description": "Pull strategy: fast-forward only or rebase local commits." },
                "timeout_s": { "type": "integer", "description": "Timeout in seconds, clamped 1-300 (default 120)." },
            }),
            &[],
            false,
            true,
        ),
        tool(
            "repo_push",
            "Run git push for the project repository. Network operation; refused while the buffer or file has unsaved/stale state.",
            json!({
                "remote": s("Optional remote name."),
                "branch": s("Optional branch name; requires remote."),
                "set_upstream": { "type": "boolean", "description": "Run git push -u with remote and branch." },
                "timeout_s": { "type": "integer", "description": "Timeout in seconds, clamped 1-300 (default 120)." },
            }),
            &[],
            false,
            true,
        ),
        tool(
            "repo_commit",
            "Create a Git commit with a user-authored message; stage=true runs git add -A first. Refused while the buffer or file has unsaved/stale state.",
            json!({
                "message": s("Commit message (required, ≤4096 bytes)."),
                "stage": { "type": "boolean", "description": "Stage all worktree changes before committing." },
                "timeout_s": { "type": "integer", "description": "Timeout in seconds, clamped 1-300." },
            }),
            &["message"],
            false,
            true,
        ),
        tool(
            "repo_branches",
            "List branches, or create/switch a branch. Branch changes are refused while the buffer or file has unsaved/stale state.",
            json!({
                "create": s("Optional new branch name to create and switch to."),
                "switch": s("Optional existing branch name to switch to."),
                "timeout_s": { "type": "integer", "description": "Timeout in seconds, clamped 1-300." },
            }),
            &[],
            false,
            true,
        ),
        tool("repo_conflicts", "List unmerged/conflicted paths in the project repository.", json!({}), &[], true, false),
        tool(
            "repo_history",
            "List recent Git commits for the project repository (read-only).",
            json!({
                "limit": s("Maximum commits, clamped 1-100 (default 20)."),
                "timeout_s": { "type": "integer", "description": "Timeout in seconds, clamped 1-300." },
            }),
            &[],
            true,
            false,
        ),
        tool("ai_providers", "List the supported AI provider presets and their default endpoints/models.", json!({}), &[], true, false),
        tool(
            "ai_probe",
            "Probe an OpenAI-compatible /v1 provider handshake. Sends no document text; token is a request parameter and is never persisted.",
            json!({
                "provider": s("Provider preset: ollama, openai, openrouter, or custom (default ollama)."),
                "endpoint": s("OpenAI-compatible endpoint override."),
                "model": s("Model id override."),
                "token": s("Optional bearer token for this request only."),
            }),
            &[],
            true,
            false,
        ),
        tool(
            "ai_ask",
            "Ask an OpenAI-compatible provider with bounded LaTeX context. Token is a request parameter and is never persisted.",
            json!({
                "provider": s("Provider preset: ollama, openai, openrouter, or custom (default ollama)."),
                "endpoint": s("OpenAI-compatible endpoint override."),
                "model": s("Model id override."),
                "token": s("Optional bearer token for this request only."),
                "context": s("Optional LaTeX context, capped at 24 KiB."),
                "prompt": s("Question or instruction for the model."),
            }),
            &["prompt"],
            true,
            false,
        ),
        tool(
            "ai_task_create",
            "Create a durable AI task under .warraqcraft/ai/ (or the user AI store for an untitled session).",
            json!({
                "title": s("Short task title; required when prompt is empty."),
                "prompt": s("Task instruction or request, capped at 64 KiB."),
                "context": { "type": ["string", "object", "array"], "description": "Optional bounded task context." },
                "owner": s("Task owner label (default local)."),
                "state": { "type": "string", "enum": ["draft", "queued", "running", "waiting_approval", "blocked", "completed", "failed", "cancelled"] },
            }),
            &[],
            false,
            false,
        ),
        tool(
            "ai_task_list",
            "List durable AI task summaries, optionally filtered by state.",
            json!({
                "state": { "type": "string", "enum": ["draft", "queued", "running", "waiting_approval", "blocked", "completed", "failed", "cancelled"] },
                "limit": { "type": "integer", "description": "Maximum tasks, clamped 1-500 (default 100)." },
            }),
            &[],
            true,
            false,
        ),
        tool("ai_task_get", "Read one complete AI task record.", json!({ "task_id": s("Task id from ai_task_list.") }), &["task_id"], true, false),
        tool(
            "ai_task_update",
            "Record bounded lifecycle, checkpoint, result, error, or changed-path fields on a live task.",
            json!({
                "task_id": s("Task id from ai_task_list."),
                "state": { "type": "string", "enum": ["draft", "queued", "running", "waiting_approval", "blocked", "completed", "failed", "cancelled"] },
                "checkpoint": s("Optional checkpoint label."),
                "detail": s("Optional bounded checkpoint detail."),
                "result": { "type": ["string", "number", "integer", "boolean", "object", "array", "null"], "description": "Bounded task result JSON." },
                "error": s("Optional bounded error message."),
                "changed_paths": { "type": "array", "description": "Paths changed by the task (maximum 256)." },
            }),
            &["task_id"],
            false,
            false,
        ),
        tool(
            "ai_task_cancel",
            "Move a live AI task to cancelled; terminal tasks return unchanged.",
            json!({
                "task_id": s("Task id from ai_task_list."),
                "reason": s("Optional cancellation reason."),
            }),
            &["task_id"],
            false,
            false,
        ),
        tool(
            "ai_task_approve",
            "Record approval or rejection for a task's previewed plan. Approval marks the task queued; rejection cancels it.",
            json!({
                "task_id": s("Task id from ai_task_list."),
                "approve": { "type": "boolean", "description": "true approves (default), false rejects." },
                "note": s("Optional approval note."),
            }),
            &["task_id"],
            false,
            false,
        ),
        tool(
            "ai_memory_list",
            "List inspectable memory entries for scope session, project, task, or user.",
            json!({
                "scope": { "type": "string", "enum": ["session", "project", "task", "user"], "description": "Memory scope (default project)." },
                "task_id": s("Required when scope is task."),
            }),
            &[],
            true,
            false,
        ),
        tool(
            "ai_memory_set",
            "Set one memory record with source, timestamp, and optional pinned flag.",
            json!({
                "scope": { "type": "string", "enum": ["session", "project", "task", "user"], "description": "Memory scope (default project)." },
                "task_id": s("Required when scope is task."),
                "key": s("Memory key."),
                "value": { "type": ["string", "number", "integer", "boolean", "object", "array", "null"], "description": "JSON value to store." },
                "source": s("Source label (default manual)."),
                "pinned": { "type": "boolean", "description": "Pin the memory; omitted preserves the current flag." },
            }),
            &["key", "value"],
            false,
            false,
        ),
        tool(
            "ai_memory_delete",
            "Delete one memory record from the selected scope.",
            json!({
                "scope": { "type": "string", "enum": ["session", "project", "task", "user"], "description": "Memory scope (default project)." },
                "task_id": s("Required when scope is task."),
                "key": s("Memory key."),
            }),
            &["key"],
            false,
            true,
        ),
        tool(
            "ai_memory_export",
            "Export a documented JSON memory scope for backup or transfer.",
            json!({
                "scope": { "type": "string", "enum": ["session", "project", "task", "user"], "description": "Memory scope (default project)." },
                "task_id": s("Required when scope is task."),
            }),
            &[],
            true,
            false,
        ),
        tool(
            "ai_memory_import",
            "Import entries produced by ai_memory_export. replace=true clears the scope first.",
            json!({
                "scope": { "type": "string", "enum": ["session", "project", "task", "user"], "description": "Memory scope (default project)." },
                "task_id": s("Required when scope is task."),
                "data": { "type": ["object", "array"], "description": "Exported memory document or entries array." },
                "replace": { "type": "boolean", "description": "Replace the scope instead of merging (default false)." },
            }),
            &["data"],
            false,
            true,
        ),
        tool(
            "ai_permissions",
            "Read the inspectable command permission matrix used by plan preview and audit records.",
            json!({}),
            &[],
            true,
            false,
        ),
        tool(
            "ai_plan_preview",
            "Classify a proposed command plan and return required approvals without executing any command.",
            json!({
                "task_id": s("Optional task to persist the plan on."),
                "commands": { "type": "array", "description": "Commands as strings or {command, params} objects (1-64)." },
            }),
            &["commands"],
            false,
            false,
        ),
        tool("ai_prompt_list", "List versioned built-in and project prompt/guardrail registry entries.", json!({}), &[], true, false),
        tool(
            "ai_prompt_get",
            "Read one prompt/guardrail registry entry by id.",
            json!({ "id": s("Prompt id from ai_prompt_list.") }),
            &["id"],
            true,
            false,
        ),
        tool(
            "ai_audit_list",
            "Read the bounded, redacted AI audit trail for provider calls, command mutations, approvals, and task transitions.",
            json!({
                "task_id": s("Optional task id filter."),
                "event": s("Optional event name filter."),
                "limit": { "type": "integer", "description": "Maximum events, clamped 1-1000 (default 100)." },
            }),
            &[],
            true,
            false,
        ),
        tool(
            "ai_repair_preview",
            "Gather compile diagnostics and store a bounded, minimal line-edit repair plan on a durable AI task. Never mutates the buffer.",
            json!({
                "task_id": s("Optional existing task; otherwise a new repair task is created."),
                "compile": { "type": "boolean", "description": "Run a fresh compile for diagnostics when the buffer is clean and saved (default true)." },
            }),
            &[],
            false,
            false,
        ),
        tool(
            "ai_repair_apply",
            "Apply a repair task's approved compile_repair plan, save, and recompile. Fails closed when the buffer changed since the preview.",
            json!({
                "task_id": s("Task id whose compile_repair plan was approved via ai_task_approve."),
                "recompile": { "type": "boolean", "description": "Save and recompile after applying (default true)." },
            }),
            &["task_id"],
            false,
            false,
        ),
        tool("ai_skill_list", "List versioned built-in and project AI skill registry entries.", json!({}), &[], true, false),
        tool(
            "ai_skill_get",
            "Read one AI skill registry entry by id.",
            json!({ "id": s("Skill id from ai_skill_list.") }),
            &["id"],
            true,
            false,
        ),
        tool(
            "ai_components",
            "Read the inspectable AI harness component tree: prompts, guardrails, tool guidance, skills, memory scopes, tasks, audit, and evaluations.",
            json!({}),
            &[],
            true,
            false,
        ),
        tool(
            "ai_task_evidence",
            "Read one task's evidence bundle: the task record, matching audit events, and task-scope memory.",
            json!({ "task_id": s("Task id from ai_task_list.") }),
            &["task_id"],
            true,
            false,
        ),
        tool("ai_eval_list", "List deterministic LaTeX-agent evaluation fixtures and recent run summaries.", json!({}), &[], true, false),
        tool(
            "ai_eval_run",
            "Run deterministic, model-free repair evaluations in isolated workspaces and persist a bounded run record.",
            json!({
                "case": s("Optional single fixture id from ai_eval_list; otherwise all fixtures run."),
            }),
            &[],
            false,
            false,
        ),
        tool(
            "math_preview",
            "Compile a LaTeX math fragment to a preview PDF under .warraqcraft/preview/.",
            json!({ "latex": s("The LaTeX math fragment to render.") }),
            &["latex"],
            true,
            false,
        ),
        tool(
            "text_stats",
            "Document statistics: word/character/line counts and structural tallies.",
            json!({ "source": s("Optional text to analyse instead of the buffer.") }),
            &[],
            true,
            false,
        ),
        tool(
            "project_snapshot",
            "Snapshot the project's source files under .warraqcraft/snapshots/ (writes to disk).",
            json!({ "label": s("Optional snapshot label.") }),
            &[],
            false,
            true,
        ),
        tool("snapshot_list", "List saved snapshots, newest first.", json!({}), &[], true, false),
        tool("snapshot_read", "Read the files stored in one snapshot.", json!({ "id": s("Snapshot id from snapshot_list.") }), &["id"], true, false),
        tool(
            "snapshot_restore",
            "Restore a snapshot: overwrites working-tree files with the snapshot's copies — requires confirm: true.",
            json!({
                "id": s("Snapshot id from snapshot_list."),
                "confirm": { "type": "boolean", "description": "Must be true: restoring overwrites working-tree files." },
            }),
            &["id", "confirm"],
            false,
            true,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use warraqcraft_engine::COMMANDS;

    /// One JSON-RPC line in, the parsed reply out (or `None`).
    fn rpc(server: &mut McpServer, msg: &Value) -> Option<Value> {
        let reply = server.handle_line(&msg.to_string())?;
        Some(serde_json::from_str(&reply).unwrap())
    }

    fn call(server: &mut McpServer, id: u64, name: &str, arguments: Value) -> Value {
        rpc(server, &json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": arguments}})).unwrap()
    }

    /// The text payload of a successful tool result, parsed back to JSON.
    fn tool_json(reply: &Value) -> Value {
        let text = reply["result"]["content"][0]["text"].as_str().unwrap();
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn initialize_handshake() {
        let mut s = McpServer::new();
        // Supported version is echoed back.
        let r = rpc(&mut s, &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05"}})).unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(r["result"]["serverInfo"]["name"], "warraqcraft");
        assert_eq!(r["result"]["serverInfo"]["title"], "WarraqCraft");
        assert!(r["result"]["capabilities"]["tools"].is_object());
        assert!(r["result"]["capabilities"]["resources"].is_object());
        let instructions = r["result"]["instructions"].as_str().unwrap();
        assert!(instructions.contains("document_open") && instructions.contains("warraqcraft://doc/source"));
        // Unknown/absent version → newest supported.
        let r = rpc(&mut s, &json!({"jsonrpc": "2.0", "id": 2, "method": "initialize", "params": {"protocolVersion": "1999-01-01"}})).unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        let r = rpc(&mut s, &json!({"jsonrpc": "2.0", "id": 3, "method": "initialize"})).unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
    }

    #[test]
    fn ping_and_notifications() {
        let mut s = McpServer::new();
        let r = rpc(&mut s, &json!({"jsonrpc": "2.0", "id": 1, "method": "ping"})).unwrap();
        assert_eq!(r["result"], json!({}));
        // Notifications never get a reply.
        assert!(rpc(&mut s, &json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).is_none());
        assert!(rpc(&mut s, &json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"id": 1}})).is_none());
        // Any message without an id is a notification — even a known method.
        assert!(rpc(&mut s, &json!({"jsonrpc": "2.0", "method": "ping"})).is_none());
        // A notifications/* method with an id still gets no reply.
        assert!(rpc(&mut s, &json!({"jsonrpc": "2.0", "id": 9, "method": "notifications/progress"})).is_none());
    }

    #[test]
    fn tools_list_covers_every_command() {
        let mut s = McpServer::new();
        let r = rpc(&mut s, &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).unwrap();
        let tools = r["result"]["tools"].as_array().unwrap();
        assert_eq!(TOOL_COMMANDS.len(), COMMANDS.len());
        assert_eq!(tools.len(), COMMANDS.len());
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for (name, command) in TOOL_COMMANDS {
            assert!(COMMANDS.contains(command), "engine commands missing {command}");
            assert!(names.contains(name), "tools/list missing {name}");
        }
        for t in tools {
            assert!(t["description"].as_str().is_some_and(|d| !d.is_empty()), "no description: {t}");
            assert_eq!(t["inputSchema"]["type"], "object");
            assert!(t["inputSchema"]["properties"].is_object());
            assert!(t["inputSchema"]["required"].is_array());
            assert!(t["annotations"]["readOnlyHint"].is_boolean());
            assert!(t["annotations"]["destructiveHint"].is_boolean());
        }
        // Spot-check the required fields.
        let find = |n: &str| tools.iter().find(|t| t["name"] == n).unwrap().clone();
        assert_eq!(find("document_open")["inputSchema"]["required"], json!(["path"]));
        assert_eq!(find("snapshot_restore")["inputSchema"]["required"], json!(["id", "confirm"]));
        assert_eq!(find("snapshot_restore")["annotations"]["destructiveHint"], true);
        assert!(find("snapshot_restore")["description"].as_str().unwrap().contains("overwrites"));
        assert_eq!(find("source_get")["annotations"], json!({"readOnlyHint": true, "destructiveHint": false}));
    }

    #[test]
    fn tools_call_round_trip() {
        let mut s = McpServer::new();
        let r = call(&mut s, 1, "source_set", json!({"text": "\\documentclass{article}\nHi."}));
        assert_eq!(r["result"]["isError"], false);
        let r = call(&mut s, 2, "source_get", json!({}));
        assert_eq!(r["result"]["isError"], false);
        assert_eq!(tool_json(&r)["text"], "\\documentclass{article}\nHi.");
        // Arguments are optional for parameterless tools.
        let r = rpc(&mut s, &json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "structure_get"}})).unwrap();
        assert_eq!(r["result"]["isError"], false);
        assert_eq!(tool_json(&r)["class"], "article");
    }

    #[test]
    fn tool_errors_are_results_not_protocol_errors() {
        let mut s = McpServer::new();
        // Missing required param → engine BadParam → isError content.
        let r = call(&mut s, 1, "document_open", json!({}));
        assert_eq!(r["result"]["isError"], true);
        assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("path"));
        // Engine-level failure (no such file) → isError, server keeps going.
        let r = call(&mut s, 2, "document_open", json!({"path": "no/such/file-xyz.tex"}));
        assert_eq!(r["result"]["isError"], true);
        // Unknown tool → -32602 protocol error.
        let r = call(&mut s, 3, "no_such_tool", json!({}));
        assert_eq!(r["error"]["code"], -32602);
        // Missing tool name → -32602.
        let r = rpc(&mut s, &json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {}})).unwrap();
        assert_eq!(r["error"]["code"], -32602);
        // And the session still works afterwards.
        let r = call(&mut s, 5, "source_set", json!({"text": "x"}));
        assert_eq!(r["result"]["isError"], false);
    }

    #[test]
    fn protocol_error_shapes() {
        let mut s = McpServer::new();
        // Garbage line → -32700, id null.
        let r = serde_json::from_str::<Value>(&s.handle_line("not json {{{").unwrap()).unwrap();
        assert_eq!(r["error"]["code"], -32700);
        assert_eq!(r["id"], Value::Null);
        // Unknown method → -32601, id echoed.
        let r = rpc(&mut s, &json!({"jsonrpc": "2.0", "id": 7, "method": "bogus/method"})).unwrap();
        assert_eq!(r["error"]["code"], -32601);
        assert_eq!(r["id"], 7);
        // Non-object message → -32600.
        let r = serde_json::from_str::<Value>(&s.handle_line("[1,2,3]").unwrap()).unwrap();
        assert_eq!(r["error"]["code"], -32600);
        assert_eq!(r["id"], Value::Null);
        // Object with id but no method → -32600.
        let r = rpc(&mut s, &json!({"jsonrpc": "2.0", "id": 8})).unwrap();
        assert_eq!(r["error"]["code"], -32600);
        assert_eq!(r["id"], 8);
    }

    #[test]
    fn resources_round_trip() {
        let mut s = McpServer::new();
        // No document → the read-only AI resources are still present.
        let r = rpc(&mut s, &json!({"jsonrpc": "2.0", "id": 1, "method": "resources/list"})).unwrap();
        assert_eq!(r["result"]["resources"].as_array().unwrap().len(), 5);
        // Templates list all document and AI URIs.
        let r = rpc(&mut s, &json!({"jsonrpc": "2.0", "id": 2, "method": "resources/templates/list"})).unwrap();
        let templates = r["result"]["resourceTemplates"].as_array().unwrap();
        assert_eq!(templates.len(), 8);
        // Set a buffer → the three live document resources join them.
        call(&mut s, 3, "source_set", json!({"text": "\\section{A}\nHello world."}));
        let r = rpc(&mut s, &json!({"jsonrpc": "2.0", "id": 4, "method": "resources/list"})).unwrap();
        let resources = r["result"]["resources"].as_array().unwrap();
        assert_eq!(resources.len(), 8);
        assert_eq!(resources[0]["uri"], SOURCE_URI);
        assert_eq!(resources[0]["mimeType"], "text/plain");
        // Read each one.
        let read = |s: &mut McpServer, id: u64, uri: &str| {
            rpc(s, &json!({"jsonrpc": "2.0", "id": id, "method": "resources/read", "params": {"uri": uri}})).unwrap()
        };
        let r = read(&mut s, 5, SOURCE_URI);
        let c = &r["result"]["contents"][0];
        assert_eq!(c["uri"], SOURCE_URI);
        assert_eq!(c["mimeType"], "text/plain");
        assert_eq!(c["text"], "\\section{A}\nHello world.");
        let r = read(&mut s, 6, STRUCTURE_URI);
        let c = &r["result"]["contents"][0];
        assert_eq!(c["mimeType"], "application/json");
        let v: Value = serde_json::from_str(c["text"].as_str().unwrap()).unwrap();
        assert_eq!(v["headings"][0]["title"], "A");
        let r = read(&mut s, 7, STATS_URI);
        let v: Value = serde_json::from_str(r["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
        assert!(v["words"].as_u64().is_some());
        let r = read(&mut s, 11, AI_TASKS_URI);
        let v: Value = serde_json::from_str(r["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
        assert!(v["tasks"].is_array());
        let r = read(&mut s, 12, AI_PERMISSIONS_URI);
        let v: Value = serde_json::from_str(r["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(v["commands"].as_array().unwrap().len(), COMMANDS.len());
        // Bad uri → -32602.
        let r = read(&mut s, 8, "warraqcraft://doc/nope");
        assert_eq!(r["error"]["code"], -32602);
        // Missing uri → -32602.
        let r = rpc(&mut s, &json!({"jsonrpc": "2.0", "id": 9, "method": "resources/read", "params": {}})).unwrap();
        assert_eq!(r["error"]["code"], -32602);
    }

    #[test]
    fn serve_skips_empty_lines_and_ends_at_eof() {
        let mut s = McpServer::new();
        let input = b"\n  \n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n\n".as_slice();
        let mut out = Vec::new();
        s.serve(input, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1);
        let r: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(r["result"], json!({}));
    }

    #[test]
    fn serve_hostile_huge_line_is_capped_not_fatal() {
        let mut s = McpServer::new();
        // A ~5 MiB line, then a valid ping — the server must answer both
        // without buffering the monster whole.
        let mut input = vec![b'x'; 5 * 1024 * 1024];
        input.push(b'\n');
        input.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n");
        let mut out = Vec::new();
        s.serve(input.as_slice(), &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let r: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(r["error"]["code"], -32700);
        assert_eq!(r["id"], Value::Null);
        let r: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(r["result"], json!({}));
        assert_eq!(r["id"], 2);
    }

    #[test]
    fn handle_line_never_panics_on_weird_input() {
        let mut s = McpServer::new();
        for line in [
            "",
            "null",
            "5",
            "\"str\"",
            "{}",
            "{\"id\":null,\"method\":\"ping\"}",
            "{\"id\":{\"a\":1},\"method\":\"ping\"}",
            "{\"method\":5}",
            "{\"method\":\"tools/call\",\"params\":\"x\",\"id\":3}",
            "{\"method\":\"tools/call\",\"params\":{\"name\":\"source_get\",\"arguments\":\"nope\"},\"id\":4}",
            "{\"method\":\"resources/read\",\"params\":{\"uri\":5},\"id\":5}",
            "  {\"method\":\"ping\",\"id\":6}  ",
        ] {
            let _ = s.handle_line(line);
        }
        // A null id still counts as a request id in JSON-RPC — it is echoed.
        let r = serde_json::from_str::<Value>(&s.handle_line("{\"id\":null,\"method\":\"ping\"}").unwrap()).unwrap();
        assert_eq!(r["result"], json!({}));
    }
}
