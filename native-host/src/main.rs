// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 sovith3 - https://github.com/sovith3

#![windows_subsystem = "windows"]

mod adapters;
mod bridge_core;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use winreg::enums::HKEY_CURRENT_USER;
use winreg::RegKey;

/// Chrome extension identifier allowed to talk to this host. Override at
/// runtime with `RUST_SENTINEL_EXTENSION_ID` or with a
/// `%LOCALAPPDATA%\RustSentinelBridge\extension_id.txt` file (no rebuild needed).
const EXTENSION_ID: &str = "EXTENSION_ID_PLACEHOLDER";

/// Native messaging host name, must match `extension/background.js`.
const HOST_NAME: &str = "com.rustsentinel.bridge";
const HOST_DESCRIPTION: &str = "Rust Sentinel Bridge native messaging host (Apache-2.0).";
const PROJECT_URL: &str = "https://github.com/sovith3/rust-sentinel-bridge";

/// HKCU registration paths, no administrator rights required.
const REGISTRY_KEYS: [&str; 2] = [
    r"Software\Google\Chrome\NativeMessagingHosts\com.rustsentinel.bridge",
    r"Software\Microsoft\Edge\NativeMessagingHosts\com.rustsentinel.bridge",
];

fn main() {
    if is_native_host_invocation() {
        run_native_host();
    } else {
        install_host();
    }
}

/// Chrome and Edge invoke the host with the caller origin as first argument,
/// for example `chrome-extension://abcdefghijklmnopabcdefghijklmnop/`.
fn is_native_host_invocation() -> bool {
    env::args()
        .skip(1)
        .any(|arg| arg.starts_with("chrome-extension://") || arg.starts_with("--parent-window"))
}

fn run_native_host() {
    if let Err(err) = bridge_core::run() {
        append_log(&format!("[fatal] native host loop stopped: {err}"));
    }
}

fn install_host() {
    match perform_install() {
        Ok(report) => show_message(
            &format!(
                "Rust Sentinel Bridge v{} installed successfully.\n\n\
                 Host name: {HOST_NAME}\n\
                 Extension ID: {}\n\
                 Binary: {}\n\
                 Manifest: {}\n\
                 Registered: {}\n\n\
                 Next step: load the extension, then open\n\
                 web-demo/index.html and run a health check.\n\n\
                 Documentation: {PROJECT_URL}",
                env!("CARGO_PKG_VERSION"),
                report.extension_id,
                report.binary.display(),
                report.manifest.display(),
                report.registered.join(", ")
            ),
            MB_ICON_INFORMATION,
        ),
        Err(err) => show_message(
            &format!(
                "Installation failed.\n\n{err}\n\n\
                 Manual fix: run this .exe from a normal user account and\n\
                 verify the HKCU native messaging keys.\n\n{PROJECT_URL}"
            ),
            MB_ICON_ERROR,
        ),
    }
}

struct InstallReport {
    extension_id: String,
    binary: PathBuf,
    manifest: PathBuf,
    registered: Vec<String>,
}

fn perform_install() -> Result<InstallReport, String> {
    let extension_id = resolve_extension_id()?;
    let dir = data_dir();
    fs::create_dir_all(&dir).map_err(|err| format!("cannot create {}: {err}", dir.display()))?;

    let source =
        env::current_exe().map_err(|err| format!("cannot resolve current executable: {err}"))?;
    let binary = dir.join("rust-sentinel-bridge.exe");
    if !is_same_file(&source, &binary) {
        fs::copy(&source, &binary).map_err(|err| {
            format!(
                "cannot copy {} to {}: {err}",
                source.display(),
                binary.display()
            )
        })?;
    }

    let manifest = dir.join(format!("{HOST_NAME}.json"));
    let manifest_json = serde_json::json!({
        "name": HOST_NAME,
        "description": HOST_DESCRIPTION,
        "path": binary.to_string_lossy(),
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{extension_id}/")]
    });
    let serialized = serde_json::to_vec_pretty(&manifest_json)
        .map_err(|err| format!("cannot serialize native host manifest: {err}"))?;
    fs::write(&manifest, serialized)
        .map_err(|err| format!("cannot write {}: {err}", manifest.display()))?;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let manifest_path = manifest.to_string_lossy().to_string();
    let mut registered = Vec::new();
    let mut failures = Vec::new();

    for key_path in REGISTRY_KEYS {
        let result = hkcu
            .create_subkey(key_path)
            .map_err(|err| err.to_string())
            .and_then(|(key, _)| {
                key.set_value("", &manifest_path)
                    .map_err(|err| err.to_string())
                    .map(|_| key)
            });
        match result {
            Ok(_) => registered.push(browser_label(key_path).to_string()),
            Err(err) => failures.push(format!("{} ({err})", browser_label(key_path))),
        }
    }

    if registered.is_empty() {
        return Err(format!(
            "no browser registry key could be written: {}",
            failures.join(", ")
        ));
    }

    append_log(&format!(
        "installed extension={extension_id} manifest={} browsers={} failures={}",
        manifest.display(),
        registered.join(","),
        failures.join(",")
    ));

    Ok(InstallReport {
        extension_id,
        binary,
        manifest,
        registered,
    })
}

fn browser_label(key_path: &str) -> &'static str {
    if key_path.contains("Google") {
        "Chrome"
    } else {
        "Edge"
    }
}

fn resolve_extension_id() -> Result<String, String> {
    let mut candidates: Vec<String> = Vec::new();

    if let Ok(value) = env::var("RUST_SENTINEL_EXTENSION_ID") {
        candidates.push(value);
    }

    let id_file = data_dir().join("extension_id.txt");
    if let Ok(text) = fs::read_to_string(&id_file) {
        if let Some(line) = text.lines().map(str::trim).find(|line| !line.is_empty()) {
            candidates.push(line.to_string());
        }
    }
    candidates.push(EXTENSION_ID.to_string());

    for candidate in candidates {
        let cleaned = candidate
            .trim()
            .trim_start_matches("chrome-extension://")
            .trim_end_matches('/')
            .to_string();
        if is_valid_extension_id(&cleaned) {
            return Ok(cleaned);
        }
    }

    Err(format!(
        "no valid extension ID found.\n\n\
         1. Open chrome://extensions and enable Developer mode.\n\
         2. Load the unpacked 'extension' folder and copy the 32 character ID.\n\
         3. Save it into {}\n\
         (or set RUST_SENTINEL_EXTENSION_ID, or edit EXTENSION_ID in\n\
         native-host/src/main.rs and rebuild).",
        id_file.display()
    ))
}

/// Chrome extension IDs are 32 characters of the alphabet `a..=p`.
fn is_valid_extension_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| (b'a'..=b'p').contains(&byte))
}

fn is_same_file(left: &Path, right: &Path) -> bool {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn data_dir() -> PathBuf {
    match env::var_os("LOCALAPPDATA") {
        Some(base) => Path::new(&base).join("RustSentinelBridge"),
        None => env::temp_dir().join("RustSentinelBridge"),
    }
}

fn append_log(message: &str) {
    let dir = data_dir();
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("bridge.log"))
    {
        let _ = std::io::Write::write_all(&mut file, format!("{message}\n").as_bytes());
    }
}

#[cfg(windows)]
type MessageBoxStyle = windows_sys::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE;

#[cfg(not(windows))]
type MessageBoxStyle = u32;

#[cfg(windows)]
fn show_message(message: &str, icon: MessageBoxStyle) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_OK};

    let body: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();
    let caption: Vec<u16> = "Rust Sentinel Bridge"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            body.as_ptr(),
            caption.as_ptr(),
            MB_OK | icon,
        );
    }
}

#[cfg(not(windows))]
fn show_message(message: &str, _icon: MessageBoxStyle) {
    append_log(message);
}

#[cfg(windows)]
const MB_ICON_INFORMATION: MessageBoxStyle =
    windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONINFORMATION;
#[cfg(windows)]
const MB_ICON_ERROR: MessageBoxStyle = windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR;

#[cfg(not(windows))]
const MB_ICON_INFORMATION: MessageBoxStyle = 0;
#[cfg(not(windows))]
const MB_ICON_ERROR: MessageBoxStyle = 0;
