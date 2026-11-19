// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 sovith3 - https://github.com/sovith3

//! Adapter 2 - Local AI bridge (Ollama).
//!
//! The page speaks to `127.0.0.1:11434` through this host, so no CORS
//! pre-flight ever happens and `OLLAMA_ORIGINS` does not need to be touched.
//! Only loopback endpoints are accepted: the bridge must never become an
//! open proxy for arbitrary hosts.

use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde_json::{json, Value};

use crate::adapters::{payload_str, truncate, AdapterFuture, BridgeAdapter, Request};
use crate::bridge_core::Emitter;

pub const ACTIONS: [&str; 2] = ["LIST_MODELS", "CHAT_STREAM"];
pub const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:11434";

pub struct OllamaBridge {
    client: reqwest::Client,
}

impl OllamaBridge {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(3600))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self { client }
    }
}

impl Default for OllamaBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl BridgeAdapter for OllamaBridge {
    fn name(&self) -> &'static str {
        "ollama_bridge"
    }

    fn actions(&self) -> &'static [&'static str] {
        &ACTIONS
    }

    fn handle<'a>(&'a self, request: Request<'a>, out: &'a Emitter) -> AdapterFuture<'a> {
        Box::pin(async move {
            let endpoint = match resolve_endpoint(request.payload) {
                Ok(endpoint) => endpoint,
                Err(message) => {
                    out.error("INVALID_ENDPOINT", message);
                    return;
                }
            };

            match request.action {
                "LIST_MODELS" => self.list_models(&endpoint, out).await,
                "CHAT_STREAM" => self.chat_stream(&endpoint, request.payload, out).await,
                other => out.error(
                    "UNKNOWN_ACTION",
                    format!(
                        "ollama_bridge does not support '{other}' (available: {})",
                        self.actions().join(", ")
                    ),
                ),
            }
        })
    }
}

impl OllamaBridge {
    async fn list_models(&self, endpoint: &str, out: &Emitter) {
        out.status("contacting Ollama", json!({ "endpoint": endpoint }));

        let response = match self.client.get(format!("{endpoint}/api/tags")).send().await {
            Ok(response) => response,
            Err(err) => {
                out.error(
                    "OLLAMA_UNREACHABLE",
                    format!("cannot reach {endpoint}: {err}. Start it with `ollama serve`."),
                );
                return;
            }
        };

        let status = response.status();
        let body: Value = match response.json().await {
            Ok(body) => body,
            Err(err) => {
                out.error(
                    "OLLAMA_BAD_RESPONSE",
                    format!("invalid JSON from Ollama ({status}): {err}"),
                );
                return;
            }
        };

        let models: Vec<Value> = body
            .get("models")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|item| {
                        let details = item.get("details");
                        json!({
                            "name": item.get("name").cloned().unwrap_or(Value::Null),
                            "sizeBytes": item.get("size").cloned().unwrap_or(Value::Null),
                            "modifiedAt": item.get("modified_at").cloned().unwrap_or(Value::Null),
                            "family": details.and_then(|value| value.get("family")).cloned().unwrap_or(Value::Null),
                            "parameterSize": details.and_then(|value| value.get("parameter_size")).cloned().unwrap_or(Value::Null),
                            "quantization": details.and_then(|value| value.get("quantization_level")).cloned().unwrap_or(Value::Null),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        out.result(json!({
            "endpoint": endpoint,
            "reachable": true,
            "count": models.len(),
            "models": models,
        }));
    }

    async fn chat_stream(&self, endpoint: &str, payload: &Value, out: &Emitter) {
        let body = match build_chat_body(payload) {
            Ok(body) => body,
            Err(message) => {
                out.error("INVALID_PAYLOAD", message);
                return;
            }
        };

        let model = body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        out.status(
            "streaming completion",
            json!({ "endpoint": endpoint, "model": model }),
        );

        let response = match self
            .client
            .post(format!("{endpoint}/api/chat"))
            .json(&body)
            .send()
            .await
        {
            Ok(response) => response,
            Err(err) => {
                out.error(
                    "OLLAMA_UNREACHABLE",
                    format!("cannot reach {endpoint}: {err}. Start it with `ollama serve`."),
                );
                return;
            }
        };

        let status = response.status();
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            out.error(
                "OLLAMA_HTTP_ERROR",
                format!("HTTP {status}: {}", truncate(&detail, 400)),
            );
            return;
        }

        stream_completion(response, &model, out).await;
    }
}

fn build_chat_body(payload: &Value) -> Result<Value, String> {
    let model = payload_str(payload, "model")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            "payload.model is required (use LIST_MODELS to discover names)".to_string()
        })?;

    let messages = if let Some(items) = payload.get("messages").and_then(Value::as_array) {
        if items.is_empty() {
            return Err("payload.messages must contain at least one message".to_string());
        }
        Value::Array(items.clone())
    } else if let Some(prompt) = payload_str(payload, "prompt") {
        let mut messages = Vec::new();
        if let Some(system) = payload_str(payload, "system") {
            messages.push(json!({ "role": "system", "content": system }));
        }
        messages.push(json!({ "role": "user", "content": prompt }));
        Value::Array(messages)
    } else {
        return Err("payload.prompt or payload.messages is required".to_string());
    };

    let mut body = json!({
        "model": model,
        "messages": messages,
        "stream": true
    });

    if let Some(options) = payload.get("options") {
        body["options"] = options.clone();
    }
    if let Some(keep_alive) = payload.get("keep_alive") {
        body["keep_alive"] = keep_alive.clone();
    }
    if let Some(format) = payload.get("format") {
        body["format"] = format.clone();
    }

    Ok(body)
}

async fn stream_completion(response: reqwest::Response, model: &str, out: &Emitter) {
    let started = Instant::now();
    let mut stream = response.bytes_stream();
    let mut buffer: Vec<u8> = Vec::new();
    let mut text = String::new();
    let mut chunks: u64 = 0;
    let mut final_meta = Value::Null;
    let mut done = false;

    while let Some(item) = stream.next().await {
        let bytes = match item {
            Ok(bytes) => bytes,
            Err(err) => {
                out.error("STREAM_INTERRUPTED", format!("Ollama stream broke: {err}"));
                return;
            }
        };

        buffer.extend_from_slice(&bytes);

        while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = buffer.drain(..=position).collect();
            let line = String::from_utf8_lossy(&line);
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let value: Value = match serde_json::from_str(trimmed) {
                Ok(value) => value,
                Err(_) => continue,
            };

            if let Some(error) = value.get("error").and_then(Value::as_str) {
                out.error("OLLAMA_ERROR", truncate(error, 500));
                return;
            }

            let content = value
                .pointer("/message/content")
                .and_then(Value::as_str)
                .unwrap_or_default();
            text.push_str(content);
            chunks += 1;

            let is_done = value.get("done").and_then(Value::as_bool).unwrap_or(false);
            if is_done {
                done = true;
                final_meta = value.clone();
            }

            out.stream(json!({
                "model": value.get("model").cloned().unwrap_or(Value::Null),
                "content": content,
                "done": is_done,
                "index": chunks,
                "chunk": value,
            }));

            if done {
                break;
            }
        }

        if done {
            break;
        }
    }

    out.result(json!({
        "model": model,
        "content": text,
        "chunks": chunks,
        "done": done,
        "doneReason": final_meta.get("done_reason").cloned().unwrap_or(Value::Null),
        "promptEvalCount": final_meta.get("prompt_eval_count").cloned().unwrap_or(Value::Null),
        "evalCount": final_meta.get("eval_count").cloned().unwrap_or(Value::Null),
        "totalDurationNs": final_meta.get("total_duration").cloned().unwrap_or(Value::Null),
        "durationMs": started.elapsed().as_millis() as u64,
    }));
}

/// Rejects anything that is not a loopback address.
fn resolve_endpoint(payload: &Value) -> Result<String, String> {
    let raw = payload_str(payload, "endpoint")
        .unwrap_or(DEFAULT_ENDPOINT)
        .trim()
        .trim_end_matches('/')
        .to_string();

    let parsed = reqwest::Url::parse(&raw).map_err(|err| format!("invalid endpoint: {err}"))?;

    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("endpoint must use http or https".to_string());
    }

    // `host_str` may render IPv6 literals with or without brackets depending on
    // the `url` version, so both spellings of loopback are accepted.
    match parsed.host_str() {
        Some("127.0.0.1") | Some("localhost") | Some("[::1]") | Some("::1") => Ok(raw),
        _ => Err(format!(
            "endpoint '{raw}' is not loopback; only 127.0.0.1 / localhost are allowed"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_defaults_to_the_local_ollama_port() {
        let endpoint = resolve_endpoint(&json!({})).expect("default endpoint is valid");
        assert_eq!(endpoint, DEFAULT_ENDPOINT);
    }

    #[test]
    fn loopback_endpoints_are_accepted_and_normalised() {
        for (raw, expected) in [
            ("http://127.0.0.1:11434", "http://127.0.0.1:11434"),
            ("http://127.0.0.1:11434/", "http://127.0.0.1:11434"),
            ("  http://localhost:8080  ", "http://localhost:8080"),
            ("https://localhost:443", "https://localhost:443"),
            ("http://[::1]:11434", "http://[::1]:11434"),
        ] {
            let endpoint = resolve_endpoint(&json!({ "endpoint": raw }))
                .unwrap_or_else(|err| panic!("'{raw}' must be accepted: {err}"));
            assert_eq!(endpoint, expected);
        }
    }

    #[test]
    fn any_non_loopback_host_is_rejected() {
        for raw in [
            "http://10.0.0.5:11434",
            "https://ollama.example.com",
            "http://169.254.169.254",
            "http://0.0.0.0:11434",
        ] {
            let err = resolve_endpoint(&json!({ "endpoint": raw }))
                .expect_err("remote hosts must be refused");
            assert!(err.contains("loopback"), "unexpected error: {err}");
        }
    }

    #[test]
    fn non_http_schemes_are_rejected() {
        let err = resolve_endpoint(&json!({ "endpoint": "file:///etc/passwd" }))
            .expect_err("file:// must be refused");
        assert!(err.contains("http"), "unexpected error: {err}");
    }

    #[test]
    fn chat_body_requires_a_model_and_a_prompt() {
        assert!(build_chat_body(&json!({ "prompt": "hi" })).is_err());
        assert!(build_chat_body(&json!({ "model": "llama3.1" })).is_err());
    }

    #[test]
    fn messages_take_precedence_over_prompt_and_stream_is_forced() {
        let body = build_chat_body(&json!({
            "model": "llama3.1",
            "prompt": "ignored",
            "messages": [{ "role": "user", "content": "hello" }]
        }))
        .expect("valid chat payload");

        assert_eq!(body["model"], "llama3.1");
        assert_eq!(body["stream"], true);
        assert_eq!(body["messages"][0]["content"], "hello");
        assert_eq!(body["messages"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn prompt_becomes_a_single_user_message() {
        let body = build_chat_body(&json!({ "model": "llama3.1", "prompt": "hola" }))
            .expect("valid payload");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "hola");
    }
}
