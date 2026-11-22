# Wire protocol

The host speaks Chrome/Edge **Native Messaging**: every message is a UTF-8 JSON object
prefixed with its length as a 4-byte **little-endian** `u32`.

```
┌────────────┬───────────────────────────────┐
│ u32 length │ JSON payload (that many bytes)│
└────────────┴───────────────────────────────┘
```

* Frames are capped at **8 MiB** (`MAX_FRAME_BYTES`). A larger payload is rejected with an
  `ERROR` frame instead of being silently truncated.
* Reads and writes are split across two dedicated OS threads; a `tokio` runtime owns the
  adapter dispatch, so a slow adapter never blocks stdin.
* One request id can emit many frames, always in order, always finishing with exactly one
  `RESULT` or `ERROR`.

## Request

```json
{
  "id": "0b39e1f2-8a51-4c22-9d64-6a1c1c0e5b77",
  "adapter": "security_scanner",
  "action": "SCAN_FILE",
  "payload": { "path": "C:\\Users\\me\\Downloads\\setup.exe" }
}
```

| Field | Required | Notes |
| --- | --- | --- |
| `id` | no | Echoed on every frame belonging to this request. Defaults to `"unknown"`. |
| `adapter` | no | Defaults to `core`. Unknown adapter → `UNKNOWN_ADAPTER`. |
| `action` | no | Defaults to `PING`. Unknown action → `UNKNOWN_ACTION`. |
| `payload` | no | Object; adapters validate their own fields. |

Malformed JSON is answered with `INVALID_JSON`; a non-string `adapter` with
`INVALID_REQUEST`. Both terminate the request immediately.

## Response frames

Every frame mirrors `id` and `adapter` and carries `event` + `data`.

```json
{ "id": "…", "adapter": "system_monitor", "event": "STATUS", "data": { "message": "sampling hardware", "timestamp": 1766000000000 } }
{ "id": "…", "adapter": "ollama_bridge", "event": "STREAM", "data": { "content": "Native", "index": 3, "done": false } }
{ "id": "…", "adapter": "core",           "event": "RESULT", "data": { "pong": true, "uptimeMs": 42 } }
{ "id": "…", "adapter": "lol_lcu",        "event": "ERROR",  "data": { "code": "LCU_NOT_RUNNING", "message": "…", "timestamp": 1766000000000 } }
```

| `event` | Meaning |
| --- | --- |
| `STATUS` | Progress breadcrumb. `data.message` plus flattened adapter extra fields. |
| `STREAM` | Incremental payload (tokens, telemetry snapshots, LCU events). |
| `RESULT` | Terminal success. Exactly one per request. |
| `ERROR` | Terminal failure. `data.code` is stable and machine readable; `data.message` is human text. |

## `core`

### `PING` / `INFO`
Both return the same health payload (verified against the host binary):

```json
{
  "ok": true, "pong": true,
  "host": "rust-sentinel-bridge", "version": "1.0.0",
  "pid": 12345, "uptimeMs": 42, "timestamp": 1766000000000,
  "adapters": ["security_scanner", "ollama_bridge", "system_monitor", "lol_lcu"],
  "capabilities": { "security_scanner": ["SCAN_FILE", "..."], "...": ["..."] }
}
```

Unknown adapter/action errors are self-documenting: `message` lists the registered
names, e.g. `adapter 'nope' is not registered (available: core, security_scanner,
ollama_bridge, system_monitor, lol_lcu)`.

## `security_scanner`

### `SCAN_FILE`
| Field | Type | Notes |
| --- | --- | --- |
| `path` | string | **Absolute** path. Relative paths are rejected. |

### `SCAN_DOWNLOADS_LATEST`
No payload. Picks the newest regular file in `%USERPROFILE%\Downloads`.

Response (`RESULT`):

```json
{
  "path": "C:\\Users\\me\\Downloads\\setup.exe",
  "fileName": "setup.exe",
  "extension": "exe",
  "sizeBytes": 5242880,
  "sizeMb": 5.0,
  "sha256": "9f2c…",
  "entropy": 7.8123,
  "isPortableExecutable": true,
  "signature": {
    "signed": false,
    "status": "UNSIGNED",
    "winVerifyTrustCode": "0x800B0100",
    "winVerifyTrustTrusted": false
  },
  "suspiciousStrings": [ { "pattern": "VirtualAllocEx", "label": "process injection" } ],
  "findings": ["No Authenticode signature: publisher cannot be verified", "…"],
  "riskLevel": "UNSIGNED_WARNING",
  "riskScore": 52,
  "recommendation": "No trusted publisher. Verify the source before running; prefer a signed build.",
  "scannedAt": 1766000000000,
  "durationMs": 318
}
```

* `signature.status` ∈ `TRUSTED` | `UNSIGNED` | `INVALID`.
* `riskLevel` ∈ `SAFE` (score < 25) | `UNSIGNED_WARNING` (25-69) | `HIGH_RISK` (≥ 70).
* Only the first and last 2 MiB of a large file are sampled for heuristics; the hash always
  covers the whole file.
* Errors: `INVALID_PAYLOAD`, `SCAN_FAILED`, `NO_DOWNLOADS`.

## `ollama_bridge`

Every request accepts an optional `endpoint` (default `http://127.0.0.1:11434`). Only
`http`/`https` on `127.0.0.1`, `localhost` or `[::1]` is accepted; anything else fails with
`ENDPOINT_NOT_LOOPBACK`.

### `LIST_MODELS`
Response: `{ "endpoint", "reachable": true, "count", "models": [...] }` where each model is
`{ "name", "sizeBytes", "modifiedAt", "family", "parameterSize", "quantization" }`.

### `CHAT_STREAM`
| Field | Type | Notes |
| --- | --- | --- |
| `model` | string | Required. |
| `prompt` | string | Used when `messages` is absent. |
| `messages` | array | `[{ "role": "user" \| "assistant" \| "system", "content": "…" }]`. Wins over `prompt`. |
| `options` | object | Forwarded verbatim (`temperature`, `num_predict`, …). |
| `keep_alive` | string/number | Forwarded verbatim. |
| `format` | string/object | Forwarded verbatim (e.g. `"json"`). |

`STREAM` frame: `{ "content": "…", "index": 7, "done": false, "model": "…" }`.

`RESULT`: `{ "model", "content" (full text), "chunks", "done": true, "doneReason",
"promptEvalCount", "evalCount", "totalDurationNs", "durationMs" }`.

Errors: `INVALID_ENDPOINT` (bad or non-loopback URL), `INVALID_PAYLOAD`, `OLLAMA_UNREACHABLE`,
`OLLAMA_BAD_RESPONSE`, `OLLAMA_HTTP_ERROR`, `OLLAMA_ERROR`, `STREAM_INTERRUPTED`.

## `system_monitor`

### `GET_SNAPSHOT`
No payload. Performs a warm-up CPU sample (~200 ms) so the first reading is not `0 %`,
then returns one frame:

```json
{
  "timestamp": 1766000000000,
  "host": { "name": "…", "osName": "…", "osVersion": "…", "kernelVersion": "…",
            "cpuArch": "x86_64", "cpuBrand": "AMD Ryzen 7 …", "physicalCores": 8, "logicalCores": 16 },
  "cpu": { "usagePercent": 12.34, "perCorePercent": [8.1, 22.0, …], "logicalCores": 16 },
  "memory": { "totalBytes": 34359738368, "usedBytes": 17179869184, "availableBytes": 17179869184,
              "usagePercent": 50.0, "swapTotalBytes": 0, "swapUsedBytes": 0, "swapUsagePercent": 0.0 },
  "uptimeSeconds": 51234,
  "processCount": 312,
  "disks": [ { "name": "C:\\", "mountPoint": "C:\\", "fileSystem": "NTFS", "totalBytes": 0,
               "usedBytes": 0, "availableBytes": 0, "usagePercent": 0.0, "removable": false } ],
  "topCpuProcesses": [ { "pid": 4242, "name": "…", "cpuPercent": 12.5, "memoryBytes": 0 } ],
  "topMemoryProcesses": [ … ]
}
```

`perCorePercent` is a plain number array indexed like the CPU list, not objects.

### `START_TELEMETRY`
| Field | Type | Notes |
| --- | --- | --- |
| `intervalMs` | number | Clamped to `500 … 60000`, default `1500`. |

Immediately answers `RESULT { "streaming": true, "intervalMs": … }`, then pushes one
`STREAM` frame per tick with the same shape as `GET_SNAPSHOT` **minus** the process lists.
Only one telemetry loop can exist per host process; starting it again replaces the old one.

### `STOP_TELEMETRY`
Response: `{ "streaming": false, "stopped": true|false }`.

## `lol_lcu`

Reads the Riot Client lockfile at
`%LOCALAPPDATA%\Riot Games\League of Legends\lockfile` (also probes the legacy League
install path, the registry, `%PROGRAMDATA%\Riot Games\RiotClientInstalls.json` and every
drive root) and authenticates with HTTP Basic as `riot:<password>`. Override the search
with `RUST_SENTINEL_LCU_LOCKFILE=<absolute path>`.

### `GET_STATUS` / `PING`
Response: `{ "connected": true, "port", "protocol", "processId", "lockfile", "baseUrl",
"summoner": { "displayName", "gameName", "tagLine", "summonerLevel", "profileIconId", "puuid" },
"gameflowPhase": "ChampSelect" }`.

### `GET_CHAMP_SELECT`
Response: `{ "inChampSelect": true, "session": <raw LCU session> }`, or
`{ "inChampSelect": false, "reason": "…" }` when the client answers HTTP 404.

### `SUBSCRIBE_CHAMP_SELECT`
Response:

```json
{
  "streaming": true,
  "event": "OnJsonApiEvent_lol-champ-select_v1_session",
  "events": [
    "OnJsonApiEvent_lol-champ-select_v1_session",
    "OnJsonApiEvent_lol-gameflow_v1_gameflow-phase"
  ],
  "pollIntervalMs": 1500,
  "reconnectDelayMs": 3000
}
```

Two transports feed the same log and both are always armed:

* `wss://127.0.0.1:<port>` WAMP subscription (`[5, "EventName"]` out, `[8, "EventName", payload]`
  in). The handshake is bounded to 10 s and every disconnect reconnects after 3 s.
* An HTTPS poller (`/lol-gameflow/v1/gameflow-phase` every 1.5 s, plus
  `/lol-champ-select/v1/session` while the phase is `ChampSelect`). It stays idle for 6 s
  after a websocket frame, and always samples twice right after subscribing.

A missing lockfile is **never** fatal: the subscription reports
`waiting for the League client` once and keeps retrying, so subscribing before the game
client starts still works.

Every real change is published once, whichever transport notices it first:

```json
{
  "event": "OnJsonApiEvent_lol-champ-select_v1_session",
  "channel": "session",
  "source": "ws",
  "sequence": 7,
  "inChampSelect": true,
  "summary": {
    "phase": "BAN_PICK",
    "timeLeftMs": 21,
    "isSpectating": false,
    "localPlayerCellId": 2,
    "myTeam": [{ "cellId": 1, "championId": 64, "championPickIntent": 0,
                 "assignedPosition": "jungle", "summonerId": 123 }],
    "theirTeam": [],
    "bannedChampions": []
  },
  "payload": {}
}
```

* `source` ∈ `ws` | `poll`.
* `payload` is the untouched LCU object; `summary` is the stable projection used by the UIs.
* Champion select ending is `STREAM { "inChampSelect": false, "payload": null }`.
* `timeLeftMs` and the raw `payload` change every second, but they are excluded from the
  change detection: one log line per pick/ban, not one per tick.
* Phase frames use `channel: "phase"` and `event: "OnJsonApiEvent_lol-gameflow_v1_gameflow-phase"`.

### `STOP_SUBSCRIPTION`
Response: `{ "streaming": false, "stopped": true|false }`. Aborts both transports.

Errors: `LCU_NOT_RUNNING`, `LCU_AUTH_FAILED`, `LCU_REQUEST_FAILED`, `LCU_HTTP_ERROR`.

## Extension bridge (`background.js`)

The service worker adds a thin browser-side envelope on top of this protocol:

* `chrome.runtime.connectNative("com.rustsentinel.bridge")` is re-established with a
  2 s backoff whenever the port disconnects.
* Requests time out after **120 s** and answer `REQUEST_TIMEOUT` instead of hanging a UI.
* Streaming requests (`SUBSCRIBE_CHAMP_SELECT`, `START_TELEMETRY`) are remembered in
  memory and in `chrome.storage.session` (hence the `storage` permission) and replayed on
  every native reconnect and whenever a new page port connects — a service-worker restart
  no longer kills the champion-select log. `STOP_SUBSCRIPTION` / `STOP_TELEMETRY` forget
  the entry.
* `runtime.connect({ name: "sentinel-page" })` is the channel used by `content.js`; frames
  whose `event` is `STREAM` are broadcast to every connected page so unsolicited telemetry
  and LCU events reach whichever tab is listening.
* `chrome.downloads.onChanged` fires `SCAN_DOWNLOADS_LATEST` for each completed download and
  broadcasts the resulting frames to every connected page (there is no `notifications`
  permission, by design — the popup downloads list and the demo page surface the verdict).
