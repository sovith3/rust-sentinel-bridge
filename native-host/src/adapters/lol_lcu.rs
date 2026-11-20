// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 sovith3 - https://github.com/sovith3

//! Adapter 4 - League of Legends Client Update API (LCU).
//!
//! The LCU exposes an HTTPS/WSS server on a random loopback port with a
//! self-signed certificate. Credentials live in the game `lockfile`
//! (`PID:PORT:PASSWORD:PROTOCOL`). Everything here is read-only: the host
//! never sends a mutating request to Riot's client.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::{connect_async_tls_with_config, Connector};

use crate::adapters::{truncate, AdapterFuture, BridgeAdapter, Request};
use crate::bridge_core::Emitter;

pub const ACTIONS: [&str; 5] = [
    "GET_STATUS",
    "GET_CHAMP_SELECT",
    "SUBSCRIBE_CHAMP_SELECT",
    "STOP_SUBSCRIPTION",
    "PING",
];

const EVENT_NAME: &str = "OnJsonApiEvent_lol-champ-select_v1_session";
const RECONNECT_DELAY: Duration = Duration::from_secs(3);

/// Parsed contents of `<League of Legends install dir>\lockfile`.
struct Lockfile {
    pid: u32,
    port: u16,
    password: String,
    protocol: String,
    path: PathBuf,
}

impl Lockfile {
    fn read() -> Result<Self, String> {
        let path = locate_lockfile().ok_or_else(|| {
            "League of Legends lockfile not found. Start the Riot Client and log in.".to_string()
        })?;

        let raw = std::fs::read_to_string(&path)
            .map_err(|err| format!("cannot read '{}': {err}", path.display()))?;

        Self::parse(&raw, path)
    }

    /// Parses the `PID:PORT:PASSWORD:PROTOCOL` lockfile format.
    ///
    /// Split out of [`Lockfile::read`] so the format rules can be unit tested
    /// without a Riot Client installation.
    fn parse(raw: &str, path: PathBuf) -> Result<Self, String> {
        let mut parts = raw.trim().split(':');
        let pid = parts.next().and_then(|value| value.parse().ok());
        let port = parts.next().and_then(|value| value.parse().ok());
        let password = parts.next().map(str::to_string);
        let protocol = parts
            .next()
            .map(|value| value.trim().to_string())
            .unwrap_or_else(|| "https".to_string());

        match (pid, port, password) {
            (Some(pid), Some(port), Some(password)) if !password.is_empty() => Ok(Self {
                pid,
                port,
                password,
                protocol,
                path,
            }),
            _ => Err(format!(
                "'{}' is not a valid LCU lockfile (expected PID:PORT:PASSWORD:PROTOCOL)",
                path.display()
            )),
        }
    }

    fn base_url(&self) -> String {
        format!("{}://127.0.0.1:{}", self.protocol, self.port)
    }

    fn ws_url(&self) -> String {
        let scheme = if self.protocol == "https" {
            "wss"
        } else {
            "ws"
        };
        format!("{scheme}://127.0.0.1:{}", self.port)
    }

    fn authorization(&self) -> String {
        let raw = format!("riot:{}", self.password);
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(raw)
        )
    }
}

fn locate_lockfile() -> Option<PathBuf> {
    if let Some(custom) = std::env::var_os("RUST_SENTINEL_LCU_LOCKFILE") {
        let path = PathBuf::from(custom);
        if path.is_file() {
            return Some(path);
        }
    }

    let mut candidates: Vec<PathBuf> = Vec::new();

    for install in riot_install_dirs() {
        candidates.push(install.join("lockfile"));
        candidates.push(install.join("League of Legends").join("lockfile"));
    }

    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(
            PathBuf::from(local)
                .join("Riot Games")
                .join("League of Legends")
                .join("lockfile"),
        );
    }

    for drive in b'C'..=b'Z' {
        candidates.push(
            PathBuf::from(format!("{}:\\", drive as char))
                .join("Riot Games")
                .join("League of Legends")
                .join("lockfile"),
        );
    }

    candidates.into_iter().find(|path| path.is_file())
}

#[cfg(windows)]
fn riot_install_dirs() -> Vec<PathBuf> {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ};
    use winreg::RegKey;

    const CANDIDATES: [(winreg::HKEY, &str); 5] = [
        (HKEY_CURRENT_USER, r"Software\Riot Games\League of Legends"),
        (
            HKEY_CURRENT_USER,
            r"Software\Riot Games\League of Legends\Install",
        ),
        (
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\WOW6432Node\Riot Games\League of Legends",
        ),
        (HKEY_LOCAL_MACHINE, r"SOFTWARE\Riot Games\League of Legends"),
        (
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\WOW6432Node\Riot Games\League of Legends\Install",
        ),
    ];

    let mut found = Vec::new();

    for (root, subkey) in CANDIDATES {
        let key = match RegKey::predef(root).open_subkey_with_flags(subkey, KEY_READ) {
            Ok(key) => key,
            Err(_) => continue,
        };

        for value_name in ["Install Directory", "InstallDir", "Location", "Path"] {
            if let Ok(value) = key.get_value::<String, _>(value_name) {
                let trimmed = value.trim().trim_matches('"').to_string();
                if !trimmed.is_empty() {
                    found.push(PathBuf::from(trimmed));
                }
            }
        }
    }

    found
}

#[cfg(not(windows))]
fn riot_install_dirs() -> Vec<PathBuf> {
    Vec::new()
}

pub struct LolLcu {
    client: reqwest::Client,
    subscription: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl LolLcu {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            client,
            subscription: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    async fn get_json(&self, lockfile: &Lockfile, endpoint: &str) -> Result<(u16, Value), String> {
        let response = self
            .client
            .get(format!("{}{endpoint}", lockfile.base_url()))
            .header(reqwest::header::AUTHORIZATION, lockfile.authorization())
            .send()
            .await
            .map_err(|err| format!("LCU request to {endpoint} failed: {err}"))?;

        let status = response.status().as_u16();
        let text = response
            .text()
            .await
            .map_err(|err| format!("LCU body for {endpoint} failed: {err}"))?;

        let body: Value = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or_else(|_| json!({ "raw": truncate(&text, 800) }))
        };

        Ok((status, body))
    }
}

impl Default for LolLcu {
    fn default() -> Self {
        Self::new()
    }
}

impl BridgeAdapter for LolLcu {
    fn name(&self) -> &'static str {
        "lol_lcu"
    }

    fn actions(&self) -> &'static [&'static str] {
        &ACTIONS
    }

    fn handle<'a>(&'a self, request: Request<'a>, out: &'a Emitter) -> AdapterFuture<'a> {
        Box::pin(async move {
            match request.action {
                "GET_STATUS" | "PING" => self.status(out).await,
                "GET_CHAMP_SELECT" => self.champ_select(out).await,
                "SUBSCRIBE_CHAMP_SELECT" => self.subscribe(out).await,
                "STOP_SUBSCRIPTION" => {
                    let stopped = self.subscription.lock().await.take().is_some();
                    if stopped {
                        out.status("champ select subscription stopped", json!({}));
                    }
                    out.result(json!({ "streaming": false, "stopped": stopped }));
                }
                other => out.error(
                    "UNKNOWN_ACTION",
                    format!(
                        "lol_lcu does not support '{other}' (available: {})",
                        self.actions().join(", ")
                    ),
                ),
            }
        })
    }
}

impl LolLcu {
    async fn status(&self, out: &Emitter) {
        let lockfile = match Lockfile::read() {
            Ok(lockfile) => lockfile,
            Err(message) => {
                out.error("LCU_NOT_RUNNING", message);
                return;
            }
        };

        out.status(
            "authenticating against the LCU",
            json!({ "port": lockfile.port, "protocol": lockfile.protocol }),
        );

        let (status_code, summoner_body) = match self
            .get_json(&lockfile, "/lol-summoner/v1/current-summoner")
            .await
        {
            Ok(value) => value,
            Err(message) => {
                out.error("LCU_REQUEST_FAILED", message);
                return;
            }
        };

        if status_code != 200 {
            out.error(
                "LCU_AUTH_FAILED",
                format!(
                    "LCU answered HTTP {status_code}. Log into the Riot Client and retry. Body: {}",
                    truncate(&summoner_body.to_string(), 300)
                ),
            );
            return;
        }

        let phase = self
            .get_json(&lockfile, "/lol-gameflow/v1/gameflow-phase")
            .await
            .map(|(_, body)| body)
            .unwrap_or(Value::Null);

        out.result(json!({
            "connected": true,
            "port": lockfile.port,
            "protocol": lockfile.protocol,
            "processId": lockfile.pid,
            "lockfile": lockfile.path.to_string_lossy(),
            "baseUrl": lockfile.base_url(),
            "summoner": {
                "displayName": summoner_body.get("displayName").cloned().unwrap_or(Value::Null),
                "gameName": summoner_body.get("gameName").cloned().unwrap_or(Value::Null),
                "tagLine": summoner_body.get("tagLine").cloned().unwrap_or(Value::Null),
                "summonerLevel": summoner_body.get("summonerLevel").cloned().unwrap_or(Value::Null),
                "profileIconId": summoner_body.get("profileIconId").cloned().unwrap_or(Value::Null),
                "puuid": summoner_body.get("puuid").cloned().unwrap_or(Value::Null),
            },
            "gameflowPhase": phase,
        }));
    }

    async fn champ_select(&self, out: &Emitter) {
        let lockfile = match Lockfile::read() {
            Ok(lockfile) => lockfile,
            Err(message) => {
                out.error("LCU_NOT_RUNNING", message);
                return;
            }
        };

        out.status("reading champion select", json!({ "port": lockfile.port }));

        match self
            .get_json(&lockfile, "/lol-champ-select/v1/session")
            .await
        {
            Ok((404, _)) => out.result(json!({
                "inChampSelect": false,
                "reason": "the client is not in champion select",
            })),
            Ok((200, body)) => out.result(json!({
                "inChampSelect": true,
                "session": body,
            })),
            Ok((status_code, body)) => out.error(
                "LCU_HTTP_ERROR",
                format!("HTTP {status_code}: {}", truncate(&body.to_string(), 400)),
            ),
            Err(message) => out.error("LCU_REQUEST_FAILED", message),
        }
    }

    async fn subscribe(&self, out: &Emitter) {
        if let Some(previous) = self.subscription.lock().await.take() {
            previous.abort();
        }

        let emitter = out.clone();
        let handle = tokio::spawn(async move {
            loop {
                let lockfile = match Lockfile::read() {
                    Ok(lockfile) => lockfile,
                    Err(message) => {
                        emitter.error("LCU_NOT_RUNNING", message);
                        return;
                    }
                };

                emitter.status(
                    "opening LCU event socket",
                    json!({ "url": lockfile.ws_url(), "event": EVENT_NAME }),
                );

                match stream_events(&lockfile, &emitter).await {
                    Ok(()) => emitter.status(
                        "LCU closed the event socket",
                        json!({ "retryInMs": RECONNECT_DELAY.as_millis() as u64 }),
                    ),
                    Err(message) => emitter.status(
                        "LCU event socket failed",
                        json!({
                            "reason": message,
                            "retryInMs": RECONNECT_DELAY.as_millis() as u64
                        }),
                    ),
                }

                tokio::time::sleep(RECONNECT_DELAY).await;
            }
        });

        *self.subscription.lock().await = Some(handle);
        out.result(json!({
            "streaming": true,
            "event": EVENT_NAME,
            "reconnectDelayMs": RECONNECT_DELAY.as_millis() as u64,
        }));
    }
}

/// Connects, subscribes to the champ-select event and forwards every frame
/// until the socket dies. Returns `Ok(())` for a clean close.
async fn stream_events(lockfile: &Lockfile, out: &Emitter) -> Result<(), String> {
    let tls = native_tls::TlsConnector::builder()
        .danger_accept_invalid_certs(true)
        .danger_accept_invalid_hostnames(true)
        .build()
        .map_err(|err| format!("cannot build LCU TLS connector: {err}"))?;

    let mut request = lockfile
        .ws_url()
        .into_client_request()
        .map_err(|err| format!("invalid LCU websocket URL: {err}"))?;

    let authorization =
        tokio_tungstenite::tungstenite::http::HeaderValue::from_str(&lockfile.authorization())
            .map_err(|err| format!("invalid LCU credential header: {err}"))?;
    request.headers_mut().insert(
        tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
        authorization,
    );

    let (mut socket, _response) =
        connect_async_tls_with_config(request, None, false, Some(Connector::NativeTls(tls)))
            .await
            .map_err(|err| truncate(&err.to_string(), 240))?;

    // LCU protocol: [5, "EventName"] subscribes, [8, "EventName", payload] arrives.
    let subscribe = serde_json::to_string(&json!([5, EVENT_NAME])).unwrap_or_default();
    socket
        .send(Message::Text(subscribe))
        .await
        .map_err(|err| format!("cannot subscribe to {EVENT_NAME}: {err}"))?;

    out.status(
        "subscribed to champion select",
        json!({ "event": EVENT_NAME }),
    );

    let mut events: u64 = 0;

    loop {
        let frame = match socket.next().await {
            Some(Ok(frame)) => frame,
            Some(Err(err)) => return Err(truncate(&err.to_string(), 240)),
            None => return Ok(()),
        };

        match frame {
            Message::Text(text) => {
                let parsed: Value = match serde_json::from_str(&text) {
                    Ok(value) => value,
                    Err(_) => continue,
                };

                let items = match parsed.as_array() {
                    Some(items) if items.len() >= 2 => items,
                    _ => continue,
                };

                let kind = items.first().and_then(Value::as_u64).unwrap_or(0);
                let event = items.get(1).and_then(Value::as_str).unwrap_or_default();

                // 8 = event payload, 2 = subscribe ack.
                if kind != 8 {
                    continue;
                }

                let payload = items.get(2).cloned().unwrap_or(Value::Null);
                events += 1;

                out.stream(json!({
                    "event": event,
                    "payload": payload,
                    "sequence": events,
                }));
            }
            Message::Close(reason) => {
                let detail = reason
                    .map(|frame| frame.reason.to_string())
                    .unwrap_or_default();
                if detail.is_empty() {
                    return Ok(());
                }
                return Err(format!("closed by client: {detail}"));
            }
            Message::Ping(payload) => {
                let _ = socket.send(Message::Pong(payload)).await;
            }
            _ => {}
        }
    }
}
