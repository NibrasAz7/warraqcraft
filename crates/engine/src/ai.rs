//! Headless AI commands for CLI/MCP/control agents.
//!
//! The interactive panel owns setup automation; the engine owns the
//! provider-neutral request surface. `ai.probe` performs only the
//! `/v1/models` handshake, and `ai.ask` sends a bounded document-context
//! prompt. Tokens are parameters only — they are never persisted or
//! echoed into results.

#[cfg(not(target_arch = "wasm32"))]
use craft_ai::Provider;
use serde_json::{json, Value};

use crate::EngineError;

const MAX_CONTEXT: usize = 24 * 1024;
const MAX_PROMPT: usize = 64 * 1024;
const OLLAMA_ENDPOINT: &str = "http://localhost:11434/v1";
const OLLAMA_MODEL: &str = "qwen2.5:0.5b";
const OPENAI_ENDPOINT: &str = "https://api.openai.com/v1";
const OPENAI_MODEL: &str = "gpt-4o-mini";
const OPENROUTER_ENDPOINT: &str = "https://openrouter.ai/api/v1";
const OPENROUTER_MODEL: &str = "openai/gpt-4o-mini";

#[derive(Clone, Copy)]
struct Preset {
    key: &'static str,
    endpoint: &'static str,
    model: &'static str,
    local: bool,
}

const PRESETS: &[Preset] = &[
    Preset { key: "ollama", endpoint: OLLAMA_ENDPOINT, model: OLLAMA_MODEL, local: true },
    Preset { key: "openai", endpoint: OPENAI_ENDPOINT, model: OPENAI_MODEL, local: false },
    Preset { key: "openrouter", endpoint: OPENROUTER_ENDPOINT, model: OPENROUTER_MODEL, local: false },
];

/// Public provider choices an agent can pass as `provider`.
pub fn providers() -> Value {
    json!({ "providers": PRESETS.iter().map(|p| json!({
        "id": p.key,
        "endpoint": p.endpoint,
        "model": p.model,
        "local": p.local,
        "token_required": !p.local,
    })).collect::<Vec<_>>() })
}

/// Probe the configured endpoint. Sends no document text.
pub fn probe(params: &Value) -> Result<Value, EngineError> {
    probe_impl(params)
}

/// Ask the configured provider with bounded LaTeX context.
pub fn ask(params: &Value) -> Result<Value, EngineError> {
    ask_impl(params)
}

#[cfg(not(target_arch = "wasm32"))]
fn preset(params: &Value) -> Result<Preset, EngineError> {
    let key = params.get("provider").and_then(Value::as_str).unwrap_or("ollama");
    if key == "custom" {
        return Ok(Preset { key: "custom", endpoint: "", model: "", local: false });
    }
    PRESETS.iter().copied().find(|p| p.key == key).ok_or(EngineError::BadParam("provider"))
}

#[cfg(not(target_arch = "wasm32"))]
fn bounded<'a>(params: &'a Value, name: &'static str, max: usize, required: bool) -> Result<&'a str, EngineError> {
    let text = params.get(name).and_then(Value::as_str).unwrap_or_default();
    if required && text.trim().is_empty() {
        return Err(EngineError::BadParam(name));
    }
    if text.len() > max {
        return Err(EngineError::BadParam(name));
    }
    Ok(text)
}

#[cfg(not(target_arch = "wasm32"))]
fn configured(params: &Value) -> Result<(Preset, String, Option<String>, String), EngineError> {
    let preset = preset(params)?;
    let endpoint = bounded(params, "endpoint", 2048, false)?.trim();
    let model = bounded(params, "model", 512, false)?.trim();
    let endpoint = if endpoint.is_empty() { preset.endpoint } else { endpoint };
    if endpoint.is_empty() {
        return Err(EngineError::BadParam("endpoint"));
    }
    let model = if model.is_empty() { preset.model } else { model };
    if model.is_empty() {
        return Err(EngineError::BadParam("model"));
    }
    let token = bounded(params, "token", 4096, false)?;
    Ok((preset, endpoint.to_string(), (!token.is_empty()).then(|| token.to_string()), model.to_string()))
}

#[cfg(not(target_arch = "wasm32"))]
fn probe_impl(params: &Value) -> Result<Value, EngineError> {
    let (preset, endpoint, token, model) = configured(params)?;
    let mut provider = craft_ai::OpenAiCompat::new(endpoint.clone(), token, model.clone());
    let info = provider.test().map_err(|e| EngineError::Tool(format!("ai probe: {e}")))?;
    Ok(json!({
        "provider": preset.key,
        "endpoint": endpoint,
        "model": model,
        "local": provider.is_local(),
        "label": info.label,
        "models": info.models,
    }))
}

#[cfg(not(target_arch = "wasm32"))]
fn ask_impl(params: &Value) -> Result<Value, EngineError> {
    let (preset, endpoint, token, model) = configured(params)?;
    let context = bounded(params, "context", MAX_CONTEXT, false)?;
    let question = bounded(params, "prompt", MAX_PROMPT, true)?;
    let prompt = format!("Context (LaTeX):\n```tex\n{context}\n```\n\n{question}");
    let mut provider = craft_ai::OpenAiCompat::new(endpoint.clone(), token, model.clone());
    let local = provider.is_local();
    let answer = provider.complete(&prompt, &craft_ai::GenOpts::default()).map_err(|e| EngineError::Tool(format!("ai ask: {e}")))?;
    Ok(json!({ "provider": preset.key, "endpoint": endpoint, "model": model, "local": local, "answer": answer }))
}

#[cfg(target_arch = "wasm32")]
fn probe_impl(_params: &Value) -> Result<Value, EngineError> {
    Err(EngineError::Tool("ai probe is unavailable in the web build".into()))
}

#[cfg(target_arch = "wasm32")]
fn ask_impl(_params: &Value) -> Result<Value, EngineError> {
    Err(EngineError::Tool("ai ask is unavailable in the web build".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_lists_presets() {
        let v = providers();
        let ids: Vec<&str> = v["providers"].as_array().unwrap().iter().filter_map(|p| p["id"].as_str()).collect();
        assert_eq!(ids, ["ollama", "openai", "openrouter"]);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn ask_requires_prompt() {
        assert!(matches!(ask(&json!({})), Err(EngineError::BadParam("prompt"))));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn custom_requires_endpoint_and_model() {
        assert!(matches!(probe(&json!({"provider": "custom"})), Err(EngineError::BadParam("endpoint"))));
        assert!(matches!(probe(&json!({"provider": "custom", "endpoint": "http://example.test/v1"})), Err(EngineError::BadParam("model"))));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn hostile_sizes_are_bounded() {
        assert!(matches!(ask(&json!({"prompt": "x".repeat(MAX_PROMPT + 1)})), Err(EngineError::BadParam("prompt"))));
        assert!(matches!(probe(&json!({"token": "x".repeat(5000)})), Err(EngineError::BadParam("token"))));
    }
}
