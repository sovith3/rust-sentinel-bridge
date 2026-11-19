# Rust Sentinel Bridge

[![CI](https://github.com/sovith3/rust-sentinel-bridge/actions/workflows/ci.yml/badge.svg)](https://github.com/sovith3/rust-sentinel-bridge/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2021-orange.svg)](native-host/Cargo.toml)

A zero-port bridge between **any HTTPS web page** and native Windows capabilities.

No localhost HTTP server. No open TCP port. No CORS workaround. The browser talks to a
Rust binary over **Chrome/Edge Native Messaging**: stdin/stdout framed with a 4-byte
length prefix, spawned by the browser itself and locked to a single extension ID.

```
┌──────────────┐  postMessage   ┌──────────────┐  port   ┌────────────────┐
│  Web page    │ ─────────────► │ content.js   │ ──────► │ service worker │
│  (any origin)│ ◄───────────── │ (isolated)   │ ◄────── │  background.js │
└──────────────┘   to-page      └──────────────┘         └───────┬────────┘
                                                                 │ connectNative
                                                         4-byte LE framing
                                                                 ▼
                                              ┌──────────────────────────────┐
                                              │ rust-sentinel-bridge.exe     │
                                              │  security · ollama · sys · lcu│
                                              └──────────────────────────────┘
```

## Contents

| Path | What it is |
| --- | --- |
| `native-host/` | Rust 2021 native messaging host (the `.exe`). Also installs itself into the registry. |
| `extension/` | Manifest V3 extension: service worker + content script + popup. |
| `web-demo/` | Single-file demo UI that drives all four adapters from a plain HTML page. |
| `docs/PROTOCOL.md` | Wire protocol, frame schema, every action and its payload. |

## Adapters

| Adapter | Actions | What it does |
| --- | --- | --- |
| `core` | `PING`, `INFO` | Health check, adapter discovery, bridge metadata. |
| `security_scanner` | `SCAN_FILE`, `SCAN_DOWNLOADS_LATEST` | SHA-256, Shannon entropy, Authenticode via WinVerifyTrust, suspicious-API heuristics, 0-100 risk score. |
| `ollama_bridge` | `LIST_MODELS`, `CHAT_STREAM` | Streaming client for a **loopback-only** Ollama endpoint (`127.0.0.1`, `localhost`, `[::1]`). |
| `system_monitor` | `GET_SNAPSHOT`, `START_TELEMETRY`, `STOP_TELEMETRY` | CPU per core, memory, swap, disks, uptime, top CPU/RAM processes (sysinfo). |
| `lol_lcu` | `GET_STATUS`, `GET_CHAMP_SELECT`, `SUBSCRIBE_CHAMP_SELECT`, `STOP_SUBSCRIPTION` | Reads the Riot Client `lockfile`, authenticates with HTTP Basic, streams champion-select events over `wss`. |

All adapters run inside one process behind a `tokio` dispatcher with per-request ids, so
streaming telemetry and a long LLM answer can be in flight at the same time.

## Quick start

### 1. Build the host

```bash
cd native-host
cargo build --release
```

Artifacts land in `native-host/target/release/rust-sentinel-bridge.exe`.

* MSVC toolchain (default on Windows): nothing else is needed — the CRT is statically
  linked through `.cargo/config.toml`.
* No MSVC linker? Install the GNU toolchain and a MinGW-w64 GCC, then:

  ```bash
  rustup toolchain install stable-x86_64-pc-windows-gnu
  cargo +stable-x86_64-pc-windows-gnu build --release
  ```

### 2. Install the native host manifest

Run the executable **once without arguments**:

```powershell
.\target\release\rust-sentinel-bridge.exe
```

It copies itself to `%LOCALAPPDATA%\RustSentinelBridge\rust-sentinel-bridge.exe`,
writes `com.rustsentinel.bridge.json` next to it, then registers the manifest under both
browsers:

* `HKCU\Software\Google\Chrome\NativeMessagingHosts\com.rustsentinel.bridge`
* `HKCU\Software\Microsoft\Edge\NativeMessagingHosts\com.rustsentinel.bridge`

Any other Chromium browser (Brave, Vivaldi, Opera) works too — point its
`NativeMessagingHosts\com.rustsentinel.bridge` key at the same JSON file. The manifest pins
`allowed_origins` to exactly one extension id.

### 3. Tell the host which extension id to trust

Load `extension/` as an unpacked extension (`chrome://extensions` → *Load unpacked*),
copy the id Chrome assigns, then pick **one** of:

```powershell
# a) one-shot environment variable
$env:RUST_SENTINEL_EXTENSION_ID = "<32-char-id>"

# b) persistent file (recommended)
"%LOCALAPPDATA%\RustSentinelBridge\extension_id.txt"   # contains the id on one line
```

Re-run the installer, then reload the extension. The id is validated (`[a-p]{32}`), so a
placeholder value fails loudly instead of silently producing a broken registry entry.

### 4. Open the demo

Load `web-demo/index.html` (or serve it) and pick a tab. The extension's content script
injects into `https://*/*`, `http://localhost/*`, `http://127.0.0.1/*` and `file:///*`.

The popup (`chrome://extensions` → *Rust Sentinel Bridge*) exposes the same actions plus
an auto-scan hook for finished downloads.

## Calling the bridge from a web page

The content script listens on `window.postMessage` under a private channel. Nothing is
injected into the page's JavaScript realm, so the site keeps its own globals intact.

```js
const CHANNEL = "rust-sentinel-bridge";

function call(adapter, action, payload = {}, onFrame) {
  const id = crypto.randomUUID();
  const listener = (event) => {
    const data = event.data;
    if (event.source !== window) return;
    if (!data || data.channel !== CHANNEL || data.direction !== "to-page") return;
    if (data.frame.id !== id) return;
    onFrame(data.frame);
    if (data.frame.event === "RESULT" || data.frame.event === "ERROR") {
      window.removeEventListener("message", listener);
    }
  };
  window.addEventListener("message", listener);
  window.postMessage(
    { channel: CHANNEL, direction: "to-extension", message: { id, adapter, action, payload } },
    "*"
  );
}

call(
  "security_scanner",
  "SCAN_FILE",
  { path: "C:\\Users\\me\\Downloads\\setup.exe" },
  (frame) => {
    if (frame.event === "STATUS") console.log(frame.data.message);
    if (frame.event === "RESULT") console.log(frame.data.riskLevel, frame.data.sha256);
    if (frame.event === "ERROR") console.error(frame.data.code, frame.data.message);
  }
);
```

Long-running work answers with `STREAM` frames before the final `RESULT`:

```js
call("ollama_bridge", "CHAT_STREAM",
  { endpoint: "http://127.0.0.1:11434", model: "llama3.1", prompt: "hi" },
  (frame) => { if (frame.event === "STREAM") process.stdout.write(frame.data.content); });
```

Frame schema, every action and every payload field are documented in
[`docs/PROTOCOL.md`](docs/PROTOCOL.md).

## Security model

* **No listening socket.** The host only exists while the browser holds the pipe.
* **Extension pinning.** The manifest's `allowed_origins` is a single validated id; any
  other extension (or a page) cannot spawn the host.
* **Explicit origin opt-in.** Only `https://*/*`, `http://localhost/*`,
  `http://127.0.0.1/*` and `file:///*` get the content script.
* **Ollama stays local.** `ollama_bridge` rejects any endpoint that is not loopback, so
  it can never be used as an open relay to the internet.
* **LCU credentials never leave the process.** The Riot `lockfile` password is read from
  disk, used for the HTTP Basic header, and never echoed in a response.
* **Bounded frames.** Requests and responses are capped at 8 MiB; oversize payloads are
  answered with an error frame instead of being truncated silently.
* **File access is caller-driven.** The scanner reads only absolute paths you send, or the
  newest file in `%USERPROFILE%\Downloads`.

The scanner is a **heuristic triage tool**, not an antivirus: it hashes, checks the
Authenticode chain, measures entropy and greps for injection-flavoured API names. It does
not replace Microsoft Defender.

## Development

```bash
cd native-host
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --release --locked
cargo build --release --locked
```

There is no build step for `extension/` or `web-demo/`: they are plain ES2022 + CSS.
`web-demo/index.html` is intentionally self-contained so it can be opened from disk.

Debugging tips:

* Installer output is appended to `%LOCALAPPDATA%\RustSentinelBridge\bridge.log`.
* `chrome://extensions` → *service worker* → console shows every frame the port receives.
* The demo page prints its own `console.info("[bridge] ping", …)` on every heartbeat.

## License

Apache-2.0. See [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE).

Rust Sentinel Bridge is an independent project. It is not affiliated with, endorsed by,
or sponsored by Riot Games, Microsoft, Google, Brave or Ollama.
