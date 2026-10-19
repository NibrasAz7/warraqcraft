//! AI assist panel — bring-your-own-provider chat over the current
//! document, backed by craft-ai's `OpenAiCompat` (any
//! `/v1/chat/completions` endpoint: Ollama, LM Studio, OpenAI, …).
//!
//! All state is plain data: `lib.rs` renders it in an `egui::Window`,
//! persists the provider fields via craft-settings, and calls
//! [`AiPanel::poll`] every frame. Requests run on a worker thread so a
//! slow endpoint never stalls the UI; failures land in `response` /
//! `status`, never a panic. The web build has no provider stack — a
//! same-shaped stub keeps the call sites unconditional.

#[cfg(not(target_arch = "wasm32"))]
use std::fs::{self, File};
#[cfg(not(target_arch = "wasm32"))]
use std::io::{Read, Seek, SeekFrom};
#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};
#[cfg(not(target_arch = "wasm32"))]
use std::process::{Command, Stdio};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::mpsc::{channel, Receiver, Sender};
#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use warraqcraft_engine::Session;

#[cfg(not(target_arch = "wasm32"))]
use craft_ai::{Provider, ProviderInfo};

/// Context bytes prepended to a prompt — the selection/document rides
/// in a fenced block; 24 KiB keeps small local models sane.
const MAX_CONTEXT: usize = 24 * 1024;
/// Ollama's OpenAI-compatible endpoint and the lightweight default pull.
const OLLAMA_ENDPOINT: &str = "http://localhost:11434/v1";
const OLLAMA_MODEL: &str = "qwen2.5:0.5b";
const OPENAI_ENDPOINT: &str = "https://api.openai.com/v1";
const OPENAI_MODEL: &str = "gpt-4o-mini";
const OPENROUTER_ENDPOINT: &str = "https://openrouter.ai/api/v1";
const OPENROUTER_MODEL: &str = "openai/gpt-4o-mini";

/// Provider presets — the user chooses one name and the matching
/// endpoint/model are filled in. `Custom` leaves every field untouched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiProviderPreset {
    Ollama,
    OpenAi,
    OpenRouter,
    Custom,
}

impl AiProviderPreset {
    pub fn key(self) -> &'static str {
        match self {
            Self::Ollama => "ollama",
            Self::OpenAi => "openai",
            Self::OpenRouter => "openrouter",
            Self::Custom => "custom",
        }
    }

    pub fn from_key(key: &str) -> Self {
        match key {
            "ollama" => Self::Ollama,
            "openai" => Self::OpenAi,
            "openrouter" => Self::OpenRouter,
            _ => Self::Custom,
        }
    }

    /// Infer an older pref that stored only an endpoint.
    pub fn from_endpoint(endpoint: &str) -> Self {
        let e = endpoint.trim().to_ascii_lowercase();
        if e.contains("localhost:11434") || e.contains("127.0.0.1:11434") {
            Self::Ollama
        } else if e.contains("api.openai.com") {
            Self::OpenAi
        } else if e.contains("openrouter.ai") {
            Self::OpenRouter
        } else {
            Self::Custom
        }
    }
}

/// A finished worker job: an answer for [`AiPanel::ask`], a probe for
/// [`AiPanel::probe`], or an Ollama setup/install result.
#[cfg(not(target_arch = "wasm32"))]
enum AiReply {
    Answer(Result<String, String>),
    Probe(Result<ProviderInfo, String>),
    Setup(Result<SetupReady, SetupFailure>),
}

#[cfg(not(target_arch = "wasm32"))]
struct SetupReady {
    label: String,
    models: Vec<String>,
    model: String,
}

#[cfg(not(target_arch = "wasm32"))]
struct SetupFailure {
    missing: bool,
    message: String,
}

/// The AI assist panel: provider fields (persisted by the parent via
/// craft-settings), a prompt box, a response, and the worker channel.
/// All state is plain data — the parent renders it in an `egui::Window`.
#[cfg(not(target_arch = "wasm32"))]
pub struct AiPanel {
    pub open: bool,
    /// Which provider preset owns the endpoint/model fields.
    pub provider: AiProviderPreset,
    /// Base URL of an OpenAI-compatible endpoint, e.g.
    /// `http://localhost:11434/v1`.
    pub endpoint: String,
    /// Optional bearer token — never echoed into errors or status.
    pub token: String,
    pub model: String,
    /// Model ids returned by the latest endpoint probe.
    pub models: Vec<String>,
    pub prompt: String,
    pub response: String,
    /// A request is in flight — `ask`/`probe`/`setup` no-op while set.
    pub busy: bool,
    /// Whether the last `probe` succeeded, plus its label.
    pub status: Option<(bool, String)>,
    /// Ollama is absent; the UI can offer an explicit install step.
    pub needs_install: bool,
    tx: Sender<AiReply>,
    rx: Receiver<AiReply>,
}

#[cfg(not(target_arch = "wasm32"))]
impl AiPanel {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            open: false,
            provider: AiProviderPreset::Ollama,
            endpoint: OLLAMA_ENDPOINT.into(),
            token: String::new(),
            model: OLLAMA_MODEL.into(),
            models: Vec::new(),
            prompt: String::new(),
            response: String::new(),
            busy: false,
            status: None,
            needs_install: false,
            tx,
            rx,
        }
    }

    /// Switch the provider preset, filling its endpoint/model defaults.
    /// `Custom` keeps every editable field exactly as typed.
    pub fn set_provider(&mut self, preset: AiProviderPreset) {
        self.provider = preset;
        self.needs_install = false;
        self.models.clear();
        match preset {
            AiProviderPreset::Ollama => {
                self.endpoint = OLLAMA_ENDPOINT.into();
                self.model = OLLAMA_MODEL.into();
                self.token.clear();
            }
            AiProviderPreset::OpenAi => {
                self.endpoint = OPENAI_ENDPOINT.into();
                self.model = OPENAI_MODEL.into();
            }
            AiProviderPreset::OpenRouter => {
                self.endpoint = OPENROUTER_ENDPOINT.into();
                self.model = OPENROUTER_MODEL.into();
            }
            AiProviderPreset::Custom => {}
        }
    }

    /// Send `context` + `prompt` to the provider on a worker thread —
    /// the context is prepended as a fenced block the model can
    /// reference. No-ops while a request is in flight; an empty
    /// endpoint short-circuits without spawning.
    pub fn ask(&mut self, context: &str) {
        if self.busy {
            return;
        }
        if self.endpoint.trim().is_empty() {
            self.response = "configure an endpoint first".into();
            return;
        }
        self.busy = true;
        let prompt = Self::build_prompt(context, &self.prompt);
        let (endpoint, token, model) = (self.endpoint.clone(), self.token.clone(), self.model.clone());
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let mut provider = provider(endpoint, token, model);
            let out = provider.complete(&prompt, &craft_ai::GenOpts::default()).map_err(|e| e.to_string());
            let _ = tx.send(AiReply::Answer(out));
        });
    }

    /// Probe the endpoint's `/v1/models` handshake on a worker →
    /// `status`. Sends no document text.
    pub fn probe(&mut self) {
        if self.busy {
            return;
        }
        if self.endpoint.trim().is_empty() {
            self.status = Some((false, "configure an endpoint first".into()));
            return;
        }
        self.busy = true;
        let (endpoint, token, model) = (self.endpoint.clone(), self.token.clone(), self.model.clone());
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let mut provider = provider(endpoint, token, model);
            let _ = tx.send(AiReply::Probe(provider.test().map_err(|e| e.to_string())));
        });
    }

    /// Fully automatic local setup: find Ollama (optionally installing
    /// it first on supported platforms), start its service, pull the
    /// selected model when absent, then probe `/v1/models`. Everything
    /// runs on the worker thread; the result lands in `status`.
    pub fn setup_ollama(&mut self, allow_install: bool) {
        if self.busy {
            return;
        }
        if self.provider != AiProviderPreset::Ollama {
            self.set_provider(AiProviderPreset::Ollama);
        }
        self.busy = true;
        let model = self.model.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(AiReply::Setup(setup_ollama_worker(allow_install, model)));
        });
    }

    /// Drain finished replies — call every frame.
    pub fn poll(&mut self) {
        while let Ok(reply) = self.rx.try_recv() {
            self.busy = false;
            match reply {
                AiReply::Answer(Ok(text)) => self.response = text,
                AiReply::Answer(Err(e)) => self.response = format!("AI: {e}"),
                AiReply::Probe(Ok(info)) => {
                    self.models = info.models;
                    if self.model.is_empty() {
                        if let Some(model) = self.models.first() {
                            self.model = model.clone();
                        }
                    }
                    self.status = Some((true, info.label));
                }
                AiReply::Probe(Err(e)) => self.status = Some((false, e)),
                AiReply::Setup(Ok(ready)) => {
                    self.model = ready.model;
                    self.models = ready.models;
                    self.needs_install = false;
                    self.status = Some((true, ready.label));
                }
                AiReply::Setup(Err(failure)) => {
                    self.needs_install = failure.missing;
                    self.status = Some((false, failure.message));
                }
            }
        }
    }

    /// The assembled outbound prompt (context block + question) —
    /// exposed for tests/debugging.
    pub fn build_prompt(context: &str, question: &str) -> String {
        assemble_prompt(context, question)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Default for AiPanel {
    fn default() -> Self {
        Self::new()
    }
}

/// Inspectable native AI harness state shown under the provider panel.
/// All reads/writes go through `Session::execute`, so the GUI sees the
/// same durable store and safety policy as CLI/control/MCP callers.
pub struct HarnessPanel {
    pub tasks: Vec<Value>,
    pub selected_task: String,
    pub task_detail: Option<Value>,
    pub task_title: String,
    pub task_prompt: String,
    pub memory_scope: String,
    pub memory_key: String,
    pub memory_value: String,
    pub memory_pinned: bool,
    pub memory_entries: Vec<Value>,
    pub plan_json: String,
    pub plan_result: Option<Value>,
    pub permissions: Vec<Value>,
    pub prompts: Vec<Value>,
    pub audit: Vec<Value>,
    pub status: String,
    pub needs_refresh: bool,
}

impl HarnessPanel {
    pub fn new() -> Self {
        Self {
            tasks: Vec::new(),
            selected_task: String::new(),
            task_detail: None,
            task_title: String::new(),
            task_prompt: String::new(),
            memory_scope: "project".into(),
            memory_key: String::new(),
            memory_value: String::new(),
            memory_pinned: false,
            memory_entries: Vec::new(),
            plan_json: "[\n  { \"command\": \"source.get\" }\n]".into(),
            plan_result: None,
            permissions: Vec::new(),
            prompts: Vec::new(),
            audit: Vec::new(),
            status: String::new(),
            needs_refresh: true,
        }
    }

    /// Refresh the read-only sections. Local JSON reads stay synchronous;
    /// they are capped and cheap compared with provider requests.
    pub fn refresh(&mut self, session: &mut Session) {
        self.needs_refresh = false;
        self.status.clear();
        if let Err(e) = self.refresh_inner(session) {
            self.status = e;
        }
    }

    fn refresh_inner(&mut self, session: &mut Session) -> Result<(), String> {
        self.tasks = Self::execute(session, "ai.task.list", json!({"limit": 100}))?["tasks"].as_array().cloned().unwrap_or_default();
        self.permissions = Self::execute(session, "ai.permissions", json!({}))?["commands"].as_array().cloned().unwrap_or_default();
        self.prompts = Self::execute(session, "ai.prompt.list", json!({}))?["prompts"].as_array().cloned().unwrap_or_default();
        self.audit = Self::execute(session, "ai.audit.list", json!({"limit": 50}))?["events"].as_array().cloned().unwrap_or_default();
        self.refresh_memory(session)?;
        if !self.selected_task.is_empty() {
            self.task_detail =
                Self::execute(session, "ai.task.get", json!({"task_id": self.selected_task})).ok().and_then(|v| v.get("task").cloned());
        }
        Ok(())
    }

    pub fn create_task(&mut self, session: &mut Session) {
        let title = self.task_title.trim();
        let prompt = self.task_prompt.trim();
        if title.is_empty() && prompt.is_empty() {
            self.status = "task needs a title or prompt".into();
            return;
        }
        match Self::execute(session, "ai.task.create", json!({"title": title, "prompt": prompt, "owner": "ui"})) {
            Ok(v) => {
                self.selected_task = v.get("task_id").and_then(Value::as_str).unwrap_or_default().to_string();
                self.task_detail = v.get("task").cloned();
                self.task_title.clear();
                self.task_prompt.clear();
                self.needs_refresh = true;
            }
            Err(e) => self.status = e,
        }
    }

    pub fn select_task(&mut self, session: &mut Session, id: &str) {
        self.selected_task = id.to_string();
        self.task_detail = Self::execute(session, "ai.task.get", json!({"task_id": id})).ok().and_then(|v| v.get("task").cloned());
        if self.memory_scope == "task" {
            let _ = self.refresh_memory(session);
        }
    }

    pub fn approve_task(&mut self, session: &mut Session, approve: bool) {
        if self.selected_task.is_empty() {
            return;
        }
        match Self::execute(session, "ai.task.approve", json!({"task_id": self.selected_task, "approve": approve})) {
            Ok(v) => {
                self.task_detail = v.get("task").cloned();
                self.needs_refresh = true;
            }
            Err(e) => self.status = e,
        }
    }

    pub fn cancel_task(&mut self, session: &mut Session) {
        if self.selected_task.is_empty() {
            return;
        }
        match Self::execute(session, "ai.task.cancel", json!({"task_id": self.selected_task, "reason": "cancelled from AI panel"})) {
            Ok(v) => {
                self.task_detail = v.get("task").cloned();
                self.needs_refresh = true;
            }
            Err(e) => self.status = e,
        }
    }

    pub fn preview_plan(&mut self, session: &mut Session) {
        let parsed: Result<Value, _> = serde_json::from_str(&self.plan_json);
        let mut params = match parsed {
            Ok(Value::Array(commands)) => json!({"commands": commands}),
            Ok(v) => v,
            Err(e) => {
                self.status = format!("invalid plan JSON: {e}");
                return;
            }
        };
        if params.get("commands").is_none() {
            self.status = "plan JSON needs a commands array".into();
            return;
        }
        if !self.selected_task.is_empty() && params.get("task_id").is_none() {
            params["task_id"] = Value::from(self.selected_task.clone());
        }
        match Self::execute(session, "ai.plan.preview", params) {
            Ok(v) => {
                self.plan_result = Some(v.clone());
                self.task_detail = v.get("task").cloned().or(self.task_detail.take());
                self.needs_refresh = true;
            }
            Err(e) => self.status = e,
        }
    }

    pub fn set_memory_scope(&mut self, session: &mut Session, scope: &str) {
        self.memory_scope = scope.to_string();
        if let Err(e) = self.refresh_memory(session) {
            self.status = e;
        }
    }

    pub fn refresh_memory(&mut self, session: &mut Session) -> Result<(), String> {
        let mut params = json!({"scope": self.memory_scope});
        if self.memory_scope == "task" {
            if self.selected_task.is_empty() {
                self.memory_entries.clear();
                return Ok(());
            }
            params["task_id"] = Value::from(self.selected_task.clone());
        }
        self.memory_entries = Self::execute(session, "ai.memory.list", params)?["entries"].as_array().cloned().unwrap_or_default();
        Ok(())
    }

    pub fn set_memory(&mut self, session: &mut Session) {
        let key = self.memory_key.trim();
        if key.is_empty() {
            self.status = "memory needs a key".into();
            return;
        }
        let value = serde_json::from_str::<Value>(&self.memory_value).unwrap_or_else(|_| Value::from(self.memory_value.clone()));
        let mut params = json!({"scope": self.memory_scope, "key": key, "value": value, "source": "ui", "pinned": self.memory_pinned});
        if self.memory_scope == "task" {
            if self.selected_task.is_empty() {
                self.status = "select a task first".into();
                return;
            }
            params["task_id"] = Value::from(self.selected_task.clone());
        }
        match Self::execute(session, "ai.memory.set", params) {
            Ok(_) => {
                self.memory_key.clear();
                self.memory_value.clear();
                self.needs_refresh = true;
            }
            Err(e) => self.status = e,
        }
    }

    pub fn delete_memory(&mut self, session: &mut Session, key: &str) {
        let mut params = json!({"scope": self.memory_scope, "key": key});
        if self.memory_scope == "task" {
            if self.selected_task.is_empty() {
                return;
            }
            params["task_id"] = Value::from(self.selected_task.clone());
        }
        match Self::execute(session, "ai.memory.delete", params) {
            Ok(_) => self.needs_refresh = true,
            Err(e) => self.status = e,
        }
    }

    fn execute(session: &mut Session, command: &str, params: Value) -> Result<Value, String> {
        session.execute(command, &params).map_err(|e| e.to_string())
    }
}

impl Default for HarnessPanel {
    fn default() -> Self {
        Self::new()
    }
}

/// The provider for the current fields — an empty bearer token means
/// no `Authorization` header (unauthenticated local endpoints).
#[cfg(not(target_arch = "wasm32"))]
fn provider(endpoint: String, token: String, model: String) -> craft_ai::OpenAiCompat {
    craft_ai::OpenAiCompat::new(endpoint, (!token.is_empty()).then_some(token), model)
}

/// The setup path used by the Ollama preset: locate (or install) the
/// CLI, ensure `ollama serve` answers `/v1/models`, pull the model when
/// absent, then return the final endpoint state.
#[cfg(not(target_arch = "wasm32"))]
fn setup_ollama_worker(allow_install: bool, model: String) -> Result<SetupReady, SetupFailure> {
    let exe = match ollama_program() {
        Some(exe) => exe,
        None if allow_install => {
            install_ollama().map_err(|message| SetupFailure { missing: false, message })?;
            ollama_program()
                .ok_or_else(|| SetupFailure { missing: false, message: "Ollama installed but is not on PATH — reopen the app".into() })?
        }
        None => {
            return Err(SetupFailure { missing: true, message: "ollama missing".into() });
        }
    };

    let mut info = ensure_ollama_server(&exe, &model).map_err(|message| SetupFailure { missing: false, message })?;
    let wanted = if model.trim().is_empty() { OLLAMA_MODEL } else { model.trim() };
    if !info.models.iter().any(|m| m == wanted) {
        run_capture(Command::new(&exe).arg("pull").arg(wanted), Duration::from_secs(15 * 60))
            .map_err(|message| SetupFailure { missing: false, message })?;
        info = probe_ollama(wanted).map_err(|message| SetupFailure { missing: false, message })?;
    }
    Ok(SetupReady { label: info.label, models: info.models, model: wanted.to_string() })
}

#[cfg(not(target_arch = "wasm32"))]
fn probe_ollama(model: &str) -> Result<ProviderInfo, String> {
    provider(OLLAMA_ENDPOINT.into(), String::new(), model.into()).test().map_err(|e| e.to_string())
}

/// Start `ollama serve` when the endpoint is down, then wait briefly
/// for the OpenAI-compatible handshake to come up.
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::disallowed_methods)] // Instant::now — native-only service wait
fn ensure_ollama_server(exe: &Path, model: &str) -> Result<ProviderInfo, String> {
    if let Ok(info) = probe_ollama(model) {
        return Ok(info);
    }
    Command::new(exe)
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start ollama serve: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut last = "endpoint did not answer".to_string();
    while Instant::now() < deadline {
        match probe_ollama(model) {
            Ok(info) => return Ok(info),
            Err(e) => last = e,
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    Err(last)
}

/// Find an installed `ollama`: PATH first, then the standard install
/// locations a package manager may not have exported to this process.
#[cfg(not(target_arch = "wasm32"))]
fn ollama_program() -> Option<PathBuf> {
    if Command::new("ollama").arg("--version").output().is_ok_and(|o| o.status.success()) {
        return Some(PathBuf::from("ollama"));
    }
    ollama_candidates().into_iter().find(|p| p.is_file())
}

#[cfg(not(target_arch = "wasm32"))]
fn ollama_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    #[cfg(target_os = "windows")]
    {
        for var in ["LOCALAPPDATA", "ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(dir) = std::env::var_os(var) {
                out.push(PathBuf::from(dir).join("Ollama").join("ollama.exe"));
            }
        }
        if let Some(dir) = std::env::var_os("LOCALAPPDATA") {
            out.push(PathBuf::from(dir).join("Programs").join("Ollama").join("ollama.exe"));
        }
    }
    #[cfg(target_os = "macos")]
    out.extend(["/opt/homebrew/bin/ollama", "/usr/local/bin/ollama"].into_iter().map(PathBuf::from));
    #[cfg(all(unix, not(target_os = "macos")))]
    out.extend(["/usr/bin/ollama", "/usr/local/bin/ollama", "/snap/bin/ollama"].into_iter().map(PathBuf::from));
    out
}

/// Run the platform's fixed package-manager command. Linux is left as
/// an explicit manual install: piping a remote script into a shell is
/// not a default WarraqCraft takes on the user's behalf.
#[cfg(not(target_arch = "wasm32"))]
fn install_ollama() -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        run_capture(
            Command::new("winget").args(["install", "-e", "--id", "Ollama.Ollama", "--accept-source-agreements", "--accept-package-agreements"]),
            Duration::from_secs(5 * 60),
        )
        .map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        run_capture(Command::new("brew").args(["install", "--cask", "ollama"]), Duration::from_secs(5 * 60)).map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Err("automatic Ollama install is not supported on this platform — install it from https://ollama.com/download".into())
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
    {
        Err("automatic Ollama install is not supported on this platform — install it from https://ollama.com/download".into())
    }
}

/// Run a bounded subprocess and keep at most the tail of its output.
/// Output goes to a temp file rather than a pipe: installers and model
/// pulls can emit enough progress text to deadlock a piped child that
/// nobody drains while `try_wait` polls.
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::disallowed_methods)] // Instant::now — native-only subprocess deadline
fn run_capture(command: &mut Command, timeout: Duration) -> Result<String, String> {
    let log_path = std::env::temp_dir().join(format!("warraqcraft-ai-{}.log", std::process::id()));
    let stdout = File::create(&log_path).map_err(|e| e.to_string())?;
    let stderr = stdout.try_clone().map_err(|e| e.to_string())?;
    let mut child = command.stdin(Stdio::null()).stdout(Stdio::from(stdout)).stderr(Stdio::from(stderr)).spawn().map_err(|e| e.to_string())?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = fs::remove_file(&log_path);
                return Err("operation timed out".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(250)),
            Err(e) => {
                let _ = fs::remove_file(&log_path);
                return Err(e.to_string());
            }
        }
    };
    let text = read_log_tail(&log_path).unwrap_or_default();
    let _ = fs::remove_file(&log_path);
    let msg = text.trim();
    let detail = if msg.is_empty() { format!("exit status {status}") } else { msg.to_string() };
    if status.success() {
        Ok(detail)
    } else {
        Err(detail)
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn read_log_tail(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let offset = len.saturating_sub(8192);
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let tail: String = text.chars().rev().take(4000).collect::<Vec<_>>().into_iter().rev().collect();
    Some(tail)
}

/// The AI assist panel, wasm stub: same fields, no provider stack —
/// `ask`/`probe`/`poll` are no-ops so the call sites stay unconditional.
#[cfg(target_arch = "wasm32")]
pub struct AiPanel {
    pub open: bool,
    pub provider: AiProviderPreset,
    pub endpoint: String,
    pub token: String,
    pub model: String,
    pub models: Vec<String>,
    pub prompt: String,
    pub response: String,
    pub busy: bool,
    pub status: Option<(bool, String)>,
    pub needs_install: bool,
}

#[cfg(target_arch = "wasm32")]
impl AiPanel {
    pub fn new() -> Self {
        Self {
            open: false,
            provider: AiProviderPreset::Ollama,
            endpoint: OLLAMA_ENDPOINT.into(),
            token: String::new(),
            model: OLLAMA_MODEL.into(),
            models: Vec::new(),
            prompt: String::new(),
            response: String::new(),
            busy: false,
            status: None,
            needs_install: false,
        }
    }

    /// `Custom` preserves typed fields; named presets fill defaults.
    pub fn set_provider(&mut self, preset: AiProviderPreset) {
        self.provider = preset;
        self.needs_install = false;
        self.models.clear();
        match preset {
            AiProviderPreset::Ollama => {
                self.endpoint = OLLAMA_ENDPOINT.into();
                self.model = OLLAMA_MODEL.into();
                self.token.clear();
            }
            AiProviderPreset::OpenAi => {
                self.endpoint = OPENAI_ENDPOINT.into();
                self.model = OPENAI_MODEL.into();
            }
            AiProviderPreset::OpenRouter => {
                self.endpoint = OPENROUTER_ENDPOINT.into();
                self.model = OPENROUTER_MODEL.into();
            }
            AiProviderPreset::Custom => {}
        }
    }

    /// No provider stack on wasm — requests never leave the tab.
    pub fn ask(&mut self, _context: &str) {}

    pub fn probe(&mut self) {}

    pub fn setup_ollama(&mut self, _allow_install: bool) {}

    pub fn poll(&mut self) {}

    /// The assembled outbound prompt (context block + question) —
    /// exposed for tests/debugging.
    pub fn build_prompt(context: &str, question: &str) -> String {
        assemble_prompt(context, question)
    }
}

#[cfg(target_arch = "wasm32")]
impl Default for AiPanel {
    fn default() -> Self {
        Self::new()
    }
}

/// The outbound prompt: `context` (clipped to [`MAX_CONTEXT`] bytes on
/// a char boundary) in a fenced `tex` block, then the question.
fn assemble_prompt(context: &str, question: &str) -> String {
    format!("Context (LaTeX):\n```tex\n{}\n```\n\n{}", clip_context(context), question)
}

/// `context` cut to at most [`MAX_CONTEXT`] bytes without splitting a
/// char.
fn clip_context(context: &str) -> &str {
    if context.len() <= MAX_CONTEXT {
        return context;
    }
    let mut end = MAX_CONTEXT;
    while end > 0 && !context.is_char_boundary(end) {
        end -= 1;
    }
    context.get(..end).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_prompt_shape() {
        let p = AiPanel::build_prompt("x = 1", "what does this do?");
        assert_eq!(p, "Context (LaTeX):\n```tex\nx = 1\n```\n\nwhat does this do?");
    }

    #[test]
    fn build_prompt_context_capped() {
        let big = "a".repeat(MAX_CONTEXT + 4096);
        let p = AiPanel::build_prompt(&big, "q");
        let wire = "Context (LaTeX):\n```tex\n\n```\n\nq".len() + MAX_CONTEXT;
        assert!(p.len() <= wire, "prompt {} bytes > cap {}", p.len(), wire);
        // A multi-byte char straddling the cap is cut on a boundary.
        let mut s = "a".repeat(MAX_CONTEXT - 1);
        s.push('汉'); // 3 bytes — the boundary lands before it
        let p = AiPanel::build_prompt(&s, "q");
        assert!(p.contains(&"a".repeat(MAX_CONTEXT - 1)));
        assert!(!p.contains('汉'));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn ask_without_endpoint_is_fast() {
        let mut panel = AiPanel::new();
        panel.endpoint.clear();
        panel.prompt = "hi".into();
        panel.ask("ctx");
        assert!(!panel.busy);
        assert!(panel.response.contains("configure an endpoint first"));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn ask_while_busy_noops() {
        let mut panel = AiPanel::new();
        panel.busy = true;
        panel.ask("ctx");
        assert!(panel.response.is_empty());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn probe_without_endpoint_sets_status() {
        let mut panel = AiPanel::new();
        panel.endpoint.clear();
        panel.probe();
        assert!(!panel.busy);
        assert_eq!(panel.status, Some((false, "configure an endpoint first".to_string())));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn poll_collects_replies() {
        let mut panel = AiPanel::new();
        panel.busy = true;
        let _ = panel.tx.send(AiReply::Answer(Ok("answer!".into())));
        panel.poll();
        assert_eq!(panel.response, "answer!");
        assert!(!panel.busy);

        let _ = panel.tx.send(AiReply::Answer(Err("boom".into())));
        panel.poll();
        assert_eq!(panel.response, "AI: boom");

        let _ = panel.tx.send(AiReply::Probe(Ok(ProviderInfo { label: "reachable: http://x".into(), models: vec!["m1".into()] })));
        panel.poll();
        assert_eq!(panel.status, Some((true, "reachable: http://x".to_string())));
        assert_eq!(panel.models, vec!["m1"]);
    }

    #[test]
    fn provider_preset_round_trip_and_infer() {
        for preset in [AiProviderPreset::Ollama, AiProviderPreset::OpenAi, AiProviderPreset::OpenRouter, AiProviderPreset::Custom] {
            assert_eq!(AiProviderPreset::from_key(preset.key()), preset);
        }
        assert_eq!(AiProviderPreset::from_endpoint("http://localhost:11434/v1"), AiProviderPreset::Ollama);
        assert_eq!(AiProviderPreset::from_endpoint("https://api.openai.com/v1"), AiProviderPreset::OpenAi);
        assert_eq!(AiProviderPreset::from_endpoint("https://openrouter.ai/api/v1"), AiProviderPreset::OpenRouter);
        assert_eq!(AiProviderPreset::from_endpoint("https://example.test/v1"), AiProviderPreset::Custom);
    }

    #[test]
    fn provider_preset_populates_defaults() {
        let mut panel = AiPanel::new();
        panel.set_provider(AiProviderPreset::OpenAi);
        assert_eq!(panel.endpoint, OPENAI_ENDPOINT);
        assert_eq!(panel.model, OPENAI_MODEL);
        panel.token = "secret".into();
        panel.set_provider(AiProviderPreset::Ollama);
        assert_eq!(panel.endpoint, OLLAMA_ENDPOINT);
        assert_eq!(panel.model, OLLAMA_MODEL);
        assert!(panel.token.is_empty(), "local preset does not keep a cloud token");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn setup_missing_marks_install_needed() {
        let mut panel = AiPanel::new();
        panel.busy = true;
        let _ = panel.tx.send(AiReply::Setup(Err(SetupFailure { missing: true, message: "ollama missing".into() })));
        panel.poll();
        assert!(panel.needs_install);
        assert!(!panel.busy);
    }
}
