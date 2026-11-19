// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 sovith3 - https://github.com/sovith3

//! Chrome / Edge Native Messaging transport.
//!
//! Wire format (both directions, no exceptions):
//!
//! ```text
//! +-------------------+---------------------------+
//! | 4 bytes LE u32    | payload length in bytes   |
//! +-------------------+---------------------------+
//! | N bytes UTF-8     | JSON message              |
//! +-------------------+---------------------------+
//! ```
//!
//! `stdin` is drained by a dedicated OS thread, `stdout` is written by another
//! OS thread fed through `std::sync::mpsc`. Nothing is ever written to the
//! console because Chrome owns the pipes.

use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::adapters::{self, BridgeAdapter, Request};

/// Hard ceiling so a buggy adapter can never deadlock the pipe. Chrome itself
/// rejects host frames larger than 1 MB, adapters keep their frames small.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

static STARTED_AT: OnceLock<Instant> = OnceLock::new();

pub fn uptime_ms() -> u64 {
    STARTED_AT.get_or_init(Instant::now).elapsed().as_millis() as u64
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

/// Envelope used by every adapter to answer a web page request.
#[derive(Clone)]
pub struct Emitter {
    id: String,
    adapter: String,
    sink: Sender<Vec<u8>>,
}

impl Emitter {
    pub fn new(id: impl Into<String>, adapter: impl Into<String>, sink: Sender<Vec<u8>>) -> Self {
        Self {
            id: id.into(),
            adapter: adapter.into(),
            sink,
        }
    }

    /// Progress information that is not a final answer (hashing, connecting...).
    pub fn status(&self, message: impl Into<String>, extra: Value) {
        let message: String = message.into();
        let mut data = extra;
        if let Some(target) = data.as_object_mut() {
            // `message` and `timestamp` always describe this frame, so they are
            // written last and win over any same-named adapter field.
            target.insert("message".to_string(), Value::String(message));
            target.insert("timestamp".to_string(), Value::from(now_ms()));
        } else {
            data = json!({ "message": message, "timestamp": now_ms() });
        }
        self.emit("STATUS", data);
    }

    /// Final answer for the request id.
    pub fn result(&self, data: Value) {
        self.emit("RESULT", data);
    }

    /// Incremental payload (LLM tokens, telemetry frames, LCU events).
    pub fn stream(&self, data: Value) {
        self.emit("STREAM", data);
    }

    /// Machine readable failure.
    pub fn error(&self, code: &str, message: impl Into<String>) {
        self.emit(
            "ERROR",
            json!({ "code": code, "message": message.into(), "timestamp": now_ms() }),
        );
    }

    fn emit(&self, event: &str, data: Value) {
        let message = json!({
            "id": self.id,
            "adapter": self.adapter,
            "event": event,
            "data": data,
        });

        let payload = match serde_json::to_vec(&message) {
            Ok(payload) => payload,
            Err(_) => return,
        };

        if payload.len() > MAX_FRAME_BYTES {
            let fallback = json!({
                "id": self.id,
                "adapter": self.adapter,
                "event": "ERROR",
                "data": {
                    "code": "PAYLOAD_TOO_LARGE",
                    "message": "response exceeded the native messaging frame limit",
                    "timestamp": now_ms()
                }
            });
            if let Ok(payload) = serde_json::to_vec(&fallback) {
                self.write_frame(&payload);
            }
            return;
        }

        self.write_frame(&payload);
    }

    fn write_frame(&self, payload: &[u8]) {
        let mut frame = Vec::with_capacity(payload.len() + 4);
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(payload);
        let _ = self.sink.send(frame);
    }
}

/// Entry point used when the process is launched by Chrome or Edge.
pub fn run() -> std::io::Result<()> {
    STARTED_AT.get_or_init(Instant::now);

    let (sink_tx, sink_rx) = mpsc::channel::<Vec<u8>>();
    spawn_writer(sink_rx);

    let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    spawn_reader(input_tx);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()?;

    runtime.block_on(async move {
        let registry: Arc<Vec<Arc<dyn BridgeAdapter>>> = Arc::new(adapters::registry());

        while let Some(raw) = input_rx.recv().await {
            let registry = Arc::clone(&registry);
            let sink = sink_tx.clone();
            tokio::spawn(async move {
                handle_message(raw, registry, sink).await;
            });
        }

        // stdin closed: the browser closed the port, nothing else to do.
        std::process::exit(0);
    })
}

fn spawn_reader(sender: tokio::sync::mpsc::UnboundedSender<String>) {
    let spawned = thread::Builder::new()
        .name("sentinel-stdin".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut lock = stdin.lock();
            let mut length_buffer = [0u8; 4];

            loop {
                if lock.read_exact(&mut length_buffer).is_err() {
                    std::process::exit(0);
                }
                let length = u32::from_le_bytes(length_buffer) as usize;
                if length == 0 || length > MAX_FRAME_BYTES {
                    std::process::exit(0);
                }

                let mut payload = vec![0u8; length];
                if lock.read_exact(&mut payload).is_err() {
                    std::process::exit(0);
                }

                if let Ok(text) = String::from_utf8(payload) {
                    if sender.send(text).is_err() {
                        std::process::exit(0);
                    }
                }
            }
        });

    if spawned.is_err() {
        std::process::exit(1);
    }
}

fn spawn_writer(receiver: Receiver<Vec<u8>>) {
    let spawned = thread::Builder::new()
        .name("sentinel-stdout".into())
        .spawn(move || {
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            for frame in receiver {
                if lock.write_all(&frame).is_err() || lock.flush().is_err() {
                    std::process::exit(0);
                }
            }
        });

    if spawned.is_err() {
        std::process::exit(1);
    }
}

async fn handle_message(
    raw: String,
    registry: Arc<Vec<Arc<dyn BridgeAdapter>>>,
    sink: Sender<Vec<u8>>,
) {
    let parsed: Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(err) => {
            Emitter::new("unknown", "core", sink)
                .error("INVALID_JSON", format!("malformed request: {err}"));
            return;
        }
    };

    let id = parsed
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let adapter_name = parsed
        .get("adapter")
        .and_then(Value::as_str)
        .unwrap_or("core")
        .to_string();
    let action = parsed
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("PING")
        .to_string();
    let payload = parsed.get("payload").cloned().unwrap_or(Value::Null);

    let out = Emitter::new(id, adapter_name.clone(), sink);

    if adapter_name.is_empty() {
        out.error("INVALID_REQUEST", "adapter must be a non-empty string");
        return;
    }

    let adapter = registry
        .iter()
        .find(|candidate| candidate.name() == adapter_name)
        .cloned();

    match adapter {
        Some(adapter) => {
            adapter
                .handle(
                    Request {
                        action: &action,
                        payload: &payload,
                    },
                    &out,
                )
                .await;
        }
        None => {
            let available: Vec<&str> = registry.iter().map(|item| item.name()).collect();
            out.error(
                "UNKNOWN_ADAPTER",
                format!(
                    "adapter '{adapter_name}' is not registered (available: {})",
                    available.join(", ")
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(receiver: &Receiver<Vec<u8>>) -> Vec<Value> {
        receiver
            .try_iter()
            .map(|frame| {
                assert!(frame.len() >= 4, "frame is missing its length prefix");
                let declared =
                    u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
                assert_eq!(
                    declared,
                    frame.len() - 4,
                    "length prefix must match payload"
                );
                serde_json::from_slice(&frame[4..]).expect("payload must be valid JSON")
            })
            .collect()
    }

    #[test]
    fn frames_carry_request_id_adapter_and_event() {
        let (sender, receiver) = mpsc::channel();
        let out = Emitter::new("req-1", "core", sender);

        out.status("working", json!({ "step": 2 }));
        out.stream(json!({ "token": "hi" }));
        out.result(json!({ "ok": true }));
        out.error("BOOM", "something broke");

        let frames = drain(&receiver);
        assert_eq!(frames.len(), 4);

        for frame in &frames {
            assert_eq!(frame["id"], "req-1");
            assert_eq!(frame["adapter"], "core");
            assert!(frame["data"].is_object());
        }

        assert_eq!(frames[0]["event"], "STATUS");
        assert_eq!(frames[0]["data"]["message"], "working");
        assert_eq!(frames[0]["data"]["step"], 2);
        assert!(frames[0]["data"]["timestamp"].is_u64());

        assert_eq!(frames[1]["event"], "STREAM");
        assert_eq!(frames[2]["event"], "RESULT");
        assert_eq!(frames[3]["event"], "ERROR");
        assert_eq!(frames[3]["data"]["code"], "BOOM");
    }

    #[test]
    fn status_extra_fields_never_override_the_message_or_timestamp() {
        let (sender, receiver) = mpsc::channel();
        let out = Emitter::new("req-2", "system_monitor", sender);

        out.status(
            "telemetry",
            json!({ "intervalMs": 1500, "message": "nope" }),
        );

        let frames = drain(&receiver);
        let data = &frames[0]["data"];
        assert_eq!(data["message"], "telemetry");
        assert_eq!(data["intervalMs"], 1500);
    }

    #[test]
    fn oversized_payload_becomes_a_payload_too_large_error() {
        let (sender, receiver) = mpsc::channel();
        let out = Emitter::new("req-3", "ollama_bridge", sender);

        let huge = "x".repeat(MAX_FRAME_BYTES + 1024);
        out.result(json!({ "content": huge }));

        let frames = drain(&receiver);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["event"], "ERROR");
        assert_eq!(frames[0]["data"]["code"], "PAYLOAD_TOO_LARGE");
    }

    #[test]
    fn uptime_and_clock_are_monotonic_and_plausible() {
        let first = uptime_ms();
        let second = uptime_ms();
        assert!(second >= first);
        // Anything after 2020 is a sane lower bound for the system clock.
        assert!(now_ms() > 1_577_836_800_000);
    }
}
