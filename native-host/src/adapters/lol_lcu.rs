// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 sovith3 - https://github.com/sovith3

//! Adapter 4 - League of Legends Client Update API (LCU).
//!
//! The LCU exposes an HTTPS/WSS server on a random loopback port with a
//! self-signed certificate. Credentials live in the game `lockfile`
//! (`PID:PORT:PASSWORD:PROTOCOL`). Everything here is read-only: the host
//! never sends a mutating request to Riot's client.
//!
//! Champion select is reported twice on purpose: the `wss` subscription is the
//! low latency path, and a small HTTPS poller feeds the same deduplicator as a
//! safety net. Either path alone is enough for the log to keep working, which
//! matters because a loopback socket can be blocked by a firewall or an
//! antivirus, and because the client is often started *after* the page
//! subscribed.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::task::JoinHandle;
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

/// Full champion-select session object.
pub const EVENT_SESSION: &str = "OnJsonApiEvent_lol-champ-select_v1_session";
/// Gameflow phase (`Lobby`, `ChampSelect`, `InProgress`, `EndOfGame`...).
pub const EVENT_PHASE: &str = "OnJsonApiEvent_lol-gameflow_v1_gameflow-phase";

const SUBSCRIBED_EVENTS: [&str; 2] = [EVENT_SESSION, EVENT_PHASE];

const RECONNECT_DELAY: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(1500);
/// How long a websocket frame keeps the HTTP poller idle.
const PUSH_GRACE: Duration = Duration::from_secs(6);
const WS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Collapses websocket pushes and HTTPS polls that describe the same state, so
/// one real change in the client produces exactly one `STREAM` frame with one
/// sequence number, no matter which transport noticed it first.
#[derive(Default)]
struct StreamState {
    fingerprints: Mutex<HashMap<String, u64>>,
    sequence: AtomicU64,
    last_push: Mutex<Option<Instant>>,
}

impl StreamState {
    fn fingerprint(value: &Value) -> u64 {
        let mut hasher = DefaultHasher::new();
        serde_json::to_string(value)
            .unwrap_or_default()
            .hash(&mut hasher);
        hasher.finish()
    }

    /// `true` when `fingerprint` differs from the last one published on
    /// `channel` (session, phase, ...).
    fn is_new(&self, channel: &str, fingerprint: u64) -> bool {
        let mut seen = match self.fingerprints.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if seen.get(channel).copied() == Some(fingerprint) {
            return false;
        }
        seen.insert(channel.to_string(), fingerprint);
        true
    }

    fn next_sequence(&self) -> u64 {
        self.sequence.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn mark_push(&self) {
        let mut last = match self.last_push.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        *last = Some(Instant::now());
    }

    /// The websocket is delivering frames, so the poller can stay quiet.
    fn push_is_fresh(&self) -> bool {
        let last = match self.last_push.lock() {
            Ok(guard) => *guard,
            Err(poisoned) => *poisoned.into_inner(),
        };
        last.map(|at| at.elapsed() < PUSH_GRACE).unwrap_or(false)
    }
}

/// The LCU re-serializes the whole session every second (the timer moves), so
/// the timer is excluded from the change detection while its *phase* is kept:
/// `BAN_PICK` -> `FINALIZATION` is still a new event, a ticking clock is not.
fn session_fingerprint(session: &Value) -> u64 {
    let mut stable = session.clone();
    if let Some(object) = stable.as_object_mut() {
        let phase = object
            .get("timer")
            .and_then(|timer| timer.get("phase"))
            .cloned()
            .unwrap_or(Value::Null);
        object.remove("timer");
        object.insert("phase".to_string(), phase);
    }
    StreamState::fingerprint(&stable)
}

/// Small, stable projection of the session: what a user wants to read in a log.
fn summarize_session(session: &Value) -> Value {
    let timer = session.get("timer").cloned().unwrap_or(Value::Null);
    json!({
        "phase": timer.get("phase").cloned().unwrap_or(Value::Null),
        "timeLeftMs": timer.get("adjustedTimeLeftInPhase").cloned().unwrap_or(Value::Null),
        "isSpectating": session.get("isSpectating").cloned().unwrap_or(Value::Bool(false)),
        "localPlayerCellId": session.get("localPlayerCellId").cloned().unwrap_or(Value::Null),
        "myTeam": summarize_team(session.get("myTeam")),
        "theirTeam": summarize_team(session.get("theirTeam")),
        "bannedChampions": session.get("bans").cloned().unwrap_or(Value::Array(Vec::new())),
    })
}

fn summarize_team(team: Option<&Value>) -> Value {
    let members = match team.and_then(Value::as_array) {
        Some(members) => members,
        None => return Value::Array(Vec::new()),
    };
    Value::Array(
        members
            .iter()
            .map(|member| {
                json!({
                    "cellId": member.get("cellId").cloned().unwrap_or(Value::Null),
                    "championId": member.get("championId").cloned().unwrap_or(Value::Null),
                    "championPickIntent": member
                        .get("championPickIntent")
                        .cloned()
                        .unwrap_or(Value::Null),
                    "assignedPosition": member
                        .get("assignedPosition")
                        .cloned()
                        .unwrap_or(Value::Null),
                    "summonerId": member.get("summonerId").cloned().unwrap_or(Value::Null),
                })
            })
            .collect(),
    )
}

fn publish_session(out: &Emitter, state: &StreamState, session: &Value, source: &str) {
    if !state.is_new("session", session_fingerprint(session)) {
        return;
    }
    let sequence = state.next_sequence();
    out.stream(json!({
        "event": EVENT_SESSION,
        "channel": "session",
        "source": source,
        "sequence": sequence,
        "inChampSelect": true,
        "summary": summarize_session(session),
        "payload": session,
    }));
}

fn publish_phase(out: &Emitter, state: &StreamState, phase: &Value, source: &str) {
    if !state.is_new("phase", StreamState::fingerprint(phase)) {
        return;
    }
    let sequence = state.next_sequence();
    out.stream(json!({
        "event": EVENT_PHASE,
        "channel": "phase",
        "source": source,
        "sequence": sequence,
        "phase": phase,
    }));
}

/// The client answered `404` on the session endpoint: champion select ended.
fn publish_idle(out: &Emitter, state: &StreamState, source: &str) {
    if !state.is_new("session", 0) {
        return;
    }
    let sequence = state.next_sequence();
    out.stream(json!({
        "event": EVENT_SESSION,
        "channel": "session",
        "source": source,
        "sequence": sequence,
        "inChampSelect": false,
        "payload": Value::Null,
    }));
}

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
    for install in riot_client_install_dirs() {
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

/// `%PROGRAMDATA%\Riot Games\RiotClientInstalls.json` is the only file the Riot
/// Client always writes, and its `associated_client` map lists every League
/// install directory - including ones outside `C:\Riot Games`.
fn riot_client_install_dirs() -> Vec<PathBuf> {
    let program_data = match std::env::var_os("PROGRAMDATA") {
        Some(value) => PathBuf::from(value),
        None => return Vec::new(),
    };
    let manifest = program_data
        .join("Riot Games")
        .join("RiotClientInstalls.json");

    let raw = match std::fs::read_to_string(&manifest) {
        Ok(raw) => raw,
        Err(_) => return Vec::new(),
    };
    let parsed: Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };

    let mut found = Vec::new();
    if let Some(associated) = parsed.get("associated_client").and_then(Value::as_object) {
        for install in associated.keys() {
            let trimmed = install.trim().trim_matches('"');
            if !trimmed.is_empty() {
                found.push(PathBuf::from(trimmed));
            }
        }
    }
    for key in ["rc_default", "rc_live", "rc_beta"] {
        if let Some(exe) = parsed.get(key).and_then(Value::as_str) {
            if let Some(parent) = Path::new(exe).parent() {
                found.push(parent.join("League of Legends"));
            }
        }
    }
    found
}

#[cfg(windows)]
fn riot_install_dirs() -> Vec<PathBuf> {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ};
    use winreg::RegKey;

    const CANDIDATES: [(winreg::HKEY, &str); 7] = [
        (HKEY_CURRENT_USER, r"Software\Riot Games\League of Legends"),
        (
            HKEY_CURRENT_USER,
            r"Software\Riot Games\League of Legends\Install",
        ),
        (
            HKEY_CURRENT_USER,
            r"Software\Riot Games, Inc.\League of Legends",
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
        (
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\WOW6432Node\Riot Games, Inc.\League of Legends",
        ),
    ];

    let mut found = Vec::new();

    for (root, subkey) in CANDIDATES {
        let key = match RegKey::predef(root).open_subkey_with_flags(subkey, KEY_READ) {
            Ok(key) => key,
            Err(_) => continue,
        };

        for value_name in [
            "Install Directory",
            "InstallDir",
            "InstallPath",
            "InstallLocation",
            "Location",
            "Path",
        ] {
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
    subscription: Arc<tokio::sync::Mutex<Option<Subscription>>>,
    state: Arc<StreamState>,
}

/// Handle to everything a running subscription owns, so `STOP_SUBSCRIPTION`
/// really stops both the websocket listener and the HTTP poller.
struct Subscription {
    tasks: Vec<JoinHandle<()>>,
}

impl Subscription {
    fn abort(self) {
        for task in self.tasks {
            task.abort();
        }
    }
}

impl LolLcu {
    pub fn new() -> Self {
        Self {
            client: build_client(),
            subscription: Arc::new(tokio::sync::Mutex::new(None)),
            state: Arc::new(StreamState::default()),
        }
    }
}

fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

async fn get_json(
    client: &reqwest::Client,
    lockfile: &Lockfile,
    endpoint: &str,
) -> Result<(u16, Value), String> {
    let response = client
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
                    let stopped = match self.subscription.lock().await.take() {
                        Some(running) => {
                            running.abort();
                            true
                        }
                        None => false,
                    };
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

        let (status_code, summoner_body) =
            match get_json(&self.client, &lockfile, "/lol-summoner/v1/current-summoner").await {
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

        let phase = get_json(&self.client, &lockfile, "/lol-gameflow/v1/gameflow-phase")
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

        match get_json(&self.client, &lockfile, "/lol-champ-select/v1/session").await {
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

        let state = Arc::clone(&self.state);
        let client = self.client.clone();

        let push_emitter = out.clone();
        let push_state = Arc::clone(&state);
        let push_task = tokio::spawn(async move {
            ws_loop(push_emitter, push_state).await;
        });

        let poll_emitter = out.clone();
        let poll_state = Arc::clone(&state);
        let poll_task = tokio::spawn(async move {
            poll_loop(client, poll_emitter, poll_state).await;
        });

        *self.subscription.lock().await = Some(Subscription {
            tasks: vec![push_task, poll_task],
        });

        out.result(json!({
            "streaming": true,
            "event": EVENT_SESSION,
            "events": SUBSCRIBED_EVENTS,
            "pollIntervalMs": POLL_INTERVAL.as_millis() as u64,
            "reconnectDelayMs": RECONNECT_DELAY.as_millis() as u64,
        }));
    }
}

/// Keeps a `wss` subscription alive forever. A missing lockfile, a refused
/// socket or a closed frame is a *retry*, never a reason to stop: the user can
/// queue up minutes after the page subscribed.
async fn ws_loop(out: Emitter, state: Arc<StreamState>) {
    let mut notice = String::new();

    loop {
        let lockfile = match Lockfile::read() {
            Ok(lockfile) => lockfile,
            Err(reason) => {
                if notice != "waiting" {
                    notice = "waiting".to_string();
                    out.status(
                        "waiting for the League client (start it and champion select will stream)",
                        json!({
                            "reason": reason,
                            "retryInMs": RECONNECT_DELAY.as_millis() as u64
                        }),
                    );
                }
                tokio::time::sleep(RECONNECT_DELAY).await;
                continue;
            }
        };
        notice.clear();

        out.status(
            "opening LCU event socket",
            json!({ "url": lockfile.ws_url(), "events": SUBSCRIBED_EVENTS }),
        );

        match stream_events(&lockfile, &out, &state).await {
            Ok(()) => out.status(
                "LCU closed the event socket",
                json!({ "retryInMs": RECONNECT_DELAY.as_millis() as u64 }),
            ),
            Err(reason) => out.status(
                "LCU event socket failed, retrying",
                json!({
                    "reason": reason,
                    "retryInMs": RECONNECT_DELAY.as_millis() as u64
                }),
            ),
        }

        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

/// HTTPS safety net: reads the same endpoints the UI uses and publishes only
/// real changes. It stays quiet while the websocket is delivering frames.
async fn poll_loop(client: reqwest::Client, out: Emitter, state: Arc<StreamState>) {
    // Always sample a couple of times up front: a subscription created while the
    // client is already in champion select must produce a first snapshot even if
    // the socket later reports "connected" without pushing anything.
    let mut warmup = 2u32;

    loop {
        let lockfile = match Lockfile::read() {
            Ok(lockfile) => lockfile,
            Err(_) => {
                // `ws_loop` owns the user-facing message for this state.
                tokio::time::sleep(RECONNECT_DELAY).await;
                continue;
            }
        };

        if warmup == 0 && state.push_is_fresh() {
            tokio::time::sleep(POLL_INTERVAL).await;
            continue;
        }
        warmup = warmup.saturating_sub(1);

        let mut try_session = true;
        match get_json(&client, &lockfile, "/lol-gameflow/v1/gameflow-phase").await {
            Ok((200, phase)) => {
                try_session = phase.as_str() == Some("ChampSelect");
                publish_phase(&out, &state, &phase, "poll");
            }
            // Unknown phase must not hide an active champion select.
            Ok(_) => {}
            Err(_) => {}
        }

        if try_session {
            match get_json(&client, &lockfile, "/lol-champ-select/v1/session").await {
                Ok((200, session)) => publish_session(&out, &state, &session, "poll"),
                Ok((404, _)) => publish_idle(&out, &state, "poll"),
                _ => {}
            }
        } else {
            publish_idle(&out, &state, "poll");
        }

        // A session endpoint that keeps failing (client closing, port rotated)
        // is picked up by the next lockfile read.
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Connects, subscribes to the champion-select and gameflow events and forwards
/// every real change until the socket dies. Returns `Ok(())` for a clean close.
async fn stream_events(
    lockfile: &Lockfile,
    out: &Emitter,
    state: &StreamState,
) -> Result<(), String> {
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

    // A refused or black-holed handshake must not freeze the poller's message
    // queue, so the connect itself is bounded.
    let (mut socket, _response) = tokio::time::timeout(
        WS_HANDSHAKE_TIMEOUT,
        connect_async_tls_with_config(request, None, false, Some(Connector::NativeTls(tls))),
    )
    .await
    .map_err(|_| {
        format!(
            "LCU websocket handshake timed out after {}s",
            WS_HANDSHAKE_TIMEOUT.as_secs()
        )
    })?
    .map_err(|err| truncate(&err.to_string(), 240))?;

    // LCU protocol: [5, "EventName"] subscribes, [8, "EventName", payload] arrives.
    for event in SUBSCRIBED_EVENTS {
        let subscribe = serde_json::to_string(&json!([5, event])).unwrap_or_default();
        socket
            .send(Message::Text(subscribe))
            .await
            .map_err(|err| format!("cannot subscribe to {event}: {err}"))?;
    }

    out.status(
        "subscribed to champion select",
        json!({ "events": SUBSCRIBED_EVENTS }),
    );

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

                state.mark_push();

                let payload = items.get(2).cloned().unwrap_or(Value::Null);
                // Both events wrap their value in `{ "data": ..., "eventType": ... }`.
                let data = payload
                    .get("data")
                    .cloned()
                    .unwrap_or_else(|| payload.clone());

                match event {
                    EVENT_SESSION => publish_session(out, state, &data, "ws"),
                    EVENT_PHASE => publish_phase(out, state, &data, "ws"),
                    _ => {}
                }
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
