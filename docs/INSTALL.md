# Install Rust Sentinel Bridge

Windows 10/11 · Chrome, Edge, Brave, Vivaldi or Chromium · **no administrator rights**

<p align="center">

```powershell
irm https://github.com/sovith3/rust-sentinel-bridge/releases/latest/download/install.ps1 | iex
```

</p>

That is the whole installation. Paste it in **PowerShell**, answer one question, done.

* [What the command does](#what-the-command-does)
* [Step by step](#step-by-step)
* [How to check it works](#how-to-check-it-works)
* [Uninstall](#uninstall)
* [Manual or offline install](#manual-or-offline-install)
* [What gets written on disk](#what-gets-written-on-disk)
* [Unattended install](#unattended-install)
* [Troubleshooting](#troubleshooting)

## What the command does

`irm` downloads the installer (`Invoke-RestMethod`), `| iex` runs it in the current session
(`Invoke-Expression`). Nothing is saved to disk by PowerShell and no execution policy has to
be changed. The installer then:

| # | Action | Detail |
| --- | --- | --- |
| 1 | Downloads the host | `rust-sentinel-bridge.exe` from the newest release, not from `main` |
| 2 | Verifies it | SHA-256 compared with the published `SHA256SUMS.txt`; a mismatch discards the file and stops |
| 3 | Installs it | copies the exe to `%LOCALAPPDATA%\RustSentinelBridge` |
| 4 | Writes the manifest | `com.rustsentinel.bridge.json` with `allowed_origins` pinned to your extension id |
| 5 | Registers the host | `HKCU` keys for Chrome, Edge, Brave, Vivaldi and Chromium — only for browsers that exist |
| 6 | Prepares the extension | when no id is known yet, extracts `rust-sentinel-extension.zip` to `%LOCALAPPDATA%\RustSentinelBridge\extension` and prints the folder to pick |

No service, no scheduled task, no firewall rule, no listening port, no `PATH` change, no
reboot. The binary only runs while Chrome or Edge holds the native messaging pipe.

## Step by step

### 1. Run the command

Open **PowerShell** (Win+X → *Terminal*, or Start → *PowerShell*) and paste:

```powershell
irm https://github.com/sovith3/rust-sentinel-bridge/releases/latest/download/install.ps1 | iex
```

The output looks like this:

```
Rust Sentinel Bridge - installer
  no admin rights, no port, no service - files go to C:\Users\you\AppData\Local\RustSentinelBridge

  no local binary next to the script, using the latest release
  downloading rust-sentinel-bridge.exe
  sha-256 verified (c1ae6550ef86...) rust-sentinel-bridge.exe
  downloading rust-sentinel-extension.zip
  sha-256 verified (09af9bf29ec8...) rust-sentinel-extension.zip
  extension extracted to C:\Users\you\AppData\Local\RustSentinelBridge\extension

Step 1 - load the extension (one time only)
  1. Open  chrome://extensions      (Edge: edge://extensions)
  2. Turn on  Developer mode  (top-right switch)
  3. Click  Load unpacked  and select this folder:
       C:\Users\you\AppData\Local\RustSentinelBridge\extension
     Already loaded from another folder? Keep it and paste its id below.
  4. Copy the 32-character id shown on the new card (letters a-p only)

Paste the extension id (Enter to cancel):
```

### 2. Load the extension (one time, four clicks)

A native messaging host is only reachable from the extension id listed in its manifest, and
Chrome creates that id locally when you load an unpacked folder. That is why this single
step cannot be automated away.

1. Open `chrome://extensions` — or `edge://extensions` in Edge.
2. Turn on **Developer mode** (switch in the top-right corner).
3. Click **Load unpacked** and select `%LOCALAPPDATA%\RustSentinelBridge\extension`.
4. Copy the 32-character id from the new card and paste it into PowerShell, then press Enter.

Already using the extension from another folder (an older manual install)? Keep it. The
installer reuses `%LOCALAPPDATA%\RustSentinelBridge\extension_id.txt` when it exists, so
upgrades never ask again.

### 3. Reload the extension

Click the circular arrow on the extension card. The service worker reconnects and the host
is spawned on demand.

## How to check it works

1. Open the [live demo](https://sovith3.github.io/rust-sentinel-bridge/).
2. The badge turns **green** (*Bridge Connected*) once the Rust host answers a `PING`.
3. Open the **League LCU** tab to stream champion-select events, or the popup for a quick
   status line.

If the badge stays amber, read `%LOCALAPPDATA%\RustSentinelBridge\bridge.log` — it names the
problem (host not registered for this extension id, browser not restarted, lockfile not
found, ...).

## Uninstall

```powershell
irm https://github.com/sovith3/rust-sentinel-bridge/releases/latest/download/uninstall.ps1 | iex
```

The uninstaller works for **any** version, including builds from before this guide existed:
it stops a running host, removes every Chrome/Edge/Brave/Vivaldi/Chromium registration that
points at this project, and deletes the install folder (plus legacy folders such as the old
`RustSentinel` layout). It never needs administrator rights. Finally click **Remove** on the
extension card in `chrome://extensions`.

Preview or keep your logs by running the downloaded file instead of the one-liner, because a
piped command cannot pass arguments:

```powershell
powershell -ExecutionPolicy Bypass -File .\uninstall.ps1 -DryRun     # list, touch nothing
powershell -ExecutionPolicy Bypass -File .\uninstall.ps1 -KeepLog    # save bridge.log to the Desktop
```

## Manual or offline install

Use this when PowerShell is locked down, there is no internet access, or you want to read the
script before running it. Everything ships in the release bundle:

1. Download `rust-sentinel-extension.zip` and `rust-sentinel-bridge-win64.zip` from the
   [latest release](https://github.com/sovith3/rust-sentinel-bridge/releases/latest).
2. Extract both. Load the extension folder with **Load unpacked** and copy its id.
3. In the host folder run:

   ```powershell
   powershell -ExecutionPolicy Bypass -File .\install.ps1
   ```

   Paste the id when prompted. The same script supports:

   | Switch | Effect |
   | --- | --- |
   | `-ExtensionId <id>` | skip the question (paste `chrome-extension://<id>/` also works) |
   | `-InstallDir <path>` | install somewhere else than `%LOCALAPPDATA%\RustSentinelBridge` |
   | `-ExePath <file>` | use a binary you built or downloaded yourself |
   | `-DryRun` | write the files and the manifest, but do not touch `HKCU` |
   | `-NoDownload` | never contact GitHub; fail instead if the exe is missing |
   | `-Uninstall` | forward to `uninstall.ps1` when it ships next to the script |

Verify a download at any time with:

```powershell
Get-FileHash .\rust-sentinel-bridge-*-win64.zip -Algorithm SHA256
```

## What gets written on disk

Nothing outside your user profile:

| Path | What it is |
| --- | --- |
| `%LOCALAPPDATA%\RustSentinelBridge\rust-sentinel-bridge.exe` | the native messaging host |
| `%LOCALAPPDATA%\RustSentinelBridge\com.rustsentinel.bridge.json` | manifest; `allowed_origins` pins your extension id |
| `%LOCALAPPDATA%\RustSentinelBridge\extension_id.txt` | the trusted id, reused by later installs |
| `%LOCALAPPDATA%\RustSentinelBridge\extension\` | unpacked copy of the extension (only when the installer had to fetch it) |
| `%LOCALAPPDATA%\RustSentinelBridge\bridge.log` | host log, appended while the pipe is open |
| `HKCU\Software\<browser>\NativeMessagingHosts\com.rustsentinel.bridge` | one default value per browser, pointing at the manifest |

## Unattended install

The one-liner cannot take parameters, so the same settings are available as environment
variables — useful for scripts, images or remote sessions:

```powershell
$env:RUST_SENTINEL_EXTENSION_ID  = 'abcdefghijklmnopabcdefghijklmnop'
$env:RUST_SENTINEL_INSTALL_DIR   = 'D:\Tools\RustSentinelBridge'
irm https://github.com/sovith3/rust-sentinel-bridge/releases/latest/download/install.ps1 | iex
```

## Troubleshooting

| Symptom | Cause and fix |
| --- | --- |
| `Installation failed: Could not download ...` | No internet, a proxy, or a corporate TLS interception. Use the manual install, or set `$env:HTTPS_PROXY` before running the command. |
| `Installation failed: SHA-256 mismatch ...` | The download was altered or truncated; the file was deleted. Run the command again. |
| `Installation failed: Cannot copy the host binary ...` | A browser still holds `rust-sentinel-bridge.exe`. Close every Chromium browser and rerun. |
| `Invalid extension id` loop | The id is 32 characters from `a` to `p` only. Copy it from the card, not from the folder name. |
| Badge stays amber in the demo | The manifest lists a different id than the loaded extension. Reinstall with the right id, or check `bridge.log`. |
| `Specified native messaging host not found` in the console | Chrome and Edge read `HKCU` at start-up: restart the browser after installing. |
| League events stay empty | The LCU socket may be blocked; the host falls back to HTTPS polling. Set `RUST_SENTINEL_LCU_LOCKFILE` if your client uses a non-standard path. |

More detail: [README](../README.md) · [wire protocol](PROTOCOL.md) ·
[issues](https://github.com/sovith3/rust-sentinel-bridge/issues)
