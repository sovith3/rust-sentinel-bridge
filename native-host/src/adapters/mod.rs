// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 sovith3 - https://github.com/sovith3

//! Adapter registry: one trait, four battle-tested natives.

pub mod lol_lcu;
pub mod ollama_bridge;
pub mod security_scanner;
pub mod system_monitor;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::bridge_core::{now_ms, uptime_ms, Emitter};

pub type AdapterFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// A single decoded native messaging request.
pub struct Request<'a> {
    pub action: &'a str,
    pub payload: &'a Value,
}

/// Every adapter implements the same small async surface.
pub trait BridgeAdapter: Send + Sync {
    fn name(&self) -> &'static str;
    fn actions(&self) -> &'static [&'static str];
    fn handle<'a>(&'a self, request: Request<'a>, out: &'a Emitter) -> AdapterFuture<'a>;
}

pub const ADAPTER_NAMES: [&str; 4] = [
    "security_scanner",
    "ollama_bridge",
    "system_monitor",
    "lol_lcu",
];

/// Central registration point. Add new adapters here.
pub fn registry() -> Vec<Arc<dyn BridgeAdapter>> {
    vec![
        Arc::new(CoreAdapter),
        Arc::new(security_scanner::SecurityScanner::new()),
        Arc::new(ollama_bridge::OllamaBridge::new()),
        Arc::new(system_monitor::SystemMonitor::new()),
        Arc::new(lol_lcu::LolLcu::new()),
    ]
}

pub fn payload_str<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload.get(key).and_then(Value::as_str)
}

pub fn payload_u64(payload: &Value, key: &str) -> Option<u64> {
    payload.get(key).and_then(Value::as_u64)
}

pub fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value.to_string()
    } else {
        let mut truncated: String = value.chars().take(max_chars).collect();
        truncated.push('…');
        truncated
    }
}

/// Virtual adapter used by the dashboard for health checks and latency probes.
struct CoreAdapter;

impl BridgeAdapter for CoreAdapter {
    fn name(&self) -> &'static str {
        "core"
    }

    fn actions(&self) -> &'static [&'static str] {
        &["PING", "INFO"]
    }

    fn handle<'a>(&'a self, request: Request<'a>, out: &'a Emitter) -> AdapterFuture<'a> {
        Box::pin(async move {
            match request.action {
                "PING" | "INFO" => out.result(json!({
                    "ok": true,
                    "pong": true,
                    "host": "rust-sentinel-bridge",
                    "version": env!("CARGO_PKG_VERSION"),
                    "pid": std::process::id(),
                    "uptimeMs": uptime_ms(),
                    "timestamp": now_ms(),
                    "adapters": ADAPTER_NAMES,
                    "capabilities": {
                        "security_scanner": security_scanner::ACTIONS,
                        "ollama_bridge": ollama_bridge::ACTIONS,
                        "system_monitor": system_monitor::ACTIONS,
                        "lol_lcu": lol_lcu::ACTIONS
                    }
                })),
                other => out.error(
                    "UNKNOWN_ACTION",
                    format!(
                        "core adapter does not support '{other}' (available: {})",
                        self.actions().join(", ")
                    ),
                ),
            }
        })
    }
}
