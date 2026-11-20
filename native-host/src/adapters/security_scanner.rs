// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 sovith3 - https://github.com/sovith3

//! Adapter 1 - Security & Antivirus scanner.
//!
//! * Streaming SHA-256 (64 KiB blocks, gigabytes never touch RAM at once).
//! * Native Windows Authenticode verification through `WinVerifyTrust`.
//! * Heuristic triage: PE header, Shannon entropy, suspicious API strings.

use std::env;
use std::fs::{self, File};
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::adapters::{payload_str, truncate, AdapterFuture, BridgeAdapter, Request};
use crate::bridge_core::{now_ms, Emitter};

pub const ACTIONS: [&str; 2] = ["SCAN_FILE", "SCAN_DOWNLOADS_LATEST"];

const HASH_BLOCK: usize = 64 * 1024;
const SAMPLE_EDGE: u64 = 2 * 1024 * 1024;
const EXECUTABLE_EXTENSIONS: [&str; 13] = [
    "exe", "dll", "sys", "scr", "com", "msi", "bat", "cmd", "ps1", "vbs", "js", "lnk", "hta",
];

/// Executable-laced API names correlated with injection / dropper behaviour.
const SUSPICIOUS_PATTERNS: [(&str, &str); 14] = [
    ("createremotethread", "Remote thread injection API"),
    ("virtualallocex", "Remote memory allocation API"),
    ("writeprocessmemory", "Cross-process memory patching"),
    ("ntcreatethreadex", "Native thread injection"),
    ("setwindowshookex", "Global input hooking"),
    ("urldownloadtofile", "Dropper download API"),
    ("powershell -enc", "Encoded PowerShell payload"),
    ("-encodedcommand", "Encoded PowerShell payload"),
    ("cmd.exe /c", "Command shell execution"),
    ("mshta ", "HTML application proxy execution"),
    ("regsvr32 /s", "Squiblydoo-style execution"),
    ("wmic process call create", "Remote process creation"),
    ("mimikatz", "Credential dumping toolkit"),
    ("reflective", "Reflective DLL loading"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum SignatureState {
    Trusted,
    Unsigned,
    Invalid,
}

impl SignatureState {
    fn as_str(self) -> &'static str {
        match self {
            SignatureState::Trusted => "TRUSTED",
            SignatureState::Unsigned => "UNSIGNED",
            SignatureState::Invalid => "INVALID",
        }
    }
}

pub struct SecurityScanner;

impl SecurityScanner {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SecurityScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl BridgeAdapter for SecurityScanner {
    fn name(&self) -> &'static str {
        "security_scanner"
    }

    fn actions(&self) -> &'static [&'static str] {
        &ACTIONS
    }

    fn handle<'a>(&'a self, request: Request<'a>, out: &'a Emitter) -> AdapterFuture<'a> {
        Box::pin(async move {
            let path = match request.action {
                "SCAN_FILE" => match payload_str(request.payload, "path") {
                    Some(value) if !value.trim().is_empty() => PathBuf::from(value.trim()),
                    _ => {
                        out.error("INVALID_PAYLOAD", "payload.path must be a non-empty string");
                        return;
                    }
                },
                "SCAN_DOWNLOADS_LATEST" => match latest_download() {
                    Some(path) => path,
                    None => {
                        out.error(
                            "NO_DOWNLOADS",
                            format!("no files found in {}", downloads_dir().display()),
                        );
                        return;
                    }
                },
                other => {
                    out.error(
                        "UNKNOWN_ACTION",
                        format!(
                            "security_scanner does not support '{other}' (available: {})",
                            self.actions().join(", ")
                        ),
                    );
                    return;
                }
            };

            let progress = out.clone();
            let task_path = path.clone();
            let worker = tokio::task::spawn_blocking(move || scan_file(&task_path, &progress));

            match worker.await {
                Ok(Ok(report)) => out.result(report),
                Ok(Err(message)) => out.error("SCAN_FAILED", message),
                Err(err) => out.error("SCAN_FAILED", format!("scanner task crashed: {err}")),
            }
        })
    }
}

fn scan_file(path: &Path, out: &Emitter) -> Result<Value, String> {
    let started = Instant::now();
    let metadata = fs::metadata(path).map_err(|err| {
        format!(
            "cannot open '{}': {err}. Use an absolute path such as C:\\\\Users\\\\me\\\\Downloads\\\\file.exe",
            path.display()
        )
    })?;

    if !metadata.is_file() {
        return Err(format!("'{}' is not a regular file", path.display()));
    }

    let size = metadata.len();
    let extension = path
        .extension()
        .map(|value| value.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    out.status(
        "hashing file with SHA-256",
        json!({ "path": path.to_string_lossy(), "sizeBytes": size }),
    );
    let sha256 = hash_file(path, out)?;

    out.status(
        "verifying Authenticode signature",
        json!({ "path": path.to_string_lossy() }),
    );
    let (signature, trust_code) = verify_authenticode(path);

    out.status(
        "running heuristics",
        json!({ "path": path.to_string_lossy() }),
    );
    let sample = read_sample(path, size);
    let entropy = shannon_entropy(&sample);
    let is_pe = sample.starts_with(b"MZ");
    let suspicious = find_suspicious_strings(&sample);

    let (risk_level, mut findings, risk_score) =
        classify(&extension, signature, entropy, &suspicious, is_pe, size);

    if !suspicious.is_empty() {
        findings.push(format!(
            "{} suspicious API string(s) detected",
            suspicious.len()
        ));
    }

    let recommendation = match risk_level.as_str() {
        "HIGH_RISK" => {
            "Do not execute this file. Upload the SHA-256 to VirusTotal and quarantine it."
        }
        "UNSIGNED_WARNING" => {
            "No trusted publisher. Verify the source before running; prefer a signed build."
        }
        _ => "No blocking indicators found. Keep Windows Defender real-time protection enabled.",
    };

    Ok(json!({
        "path": path.to_string_lossy(),
        "fileName": path.file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or_default(),
        "extension": extension,
        "sizeBytes": size,
        "sizeMb": ((size as f64) / (1024.0 * 1024.0) * 100.0).round() / 100.0,
        "sha256": sha256,
        "entropy": (entropy * 10000.0).round() / 10000.0,
        "isPortableExecutable": is_pe,
        "signature": {
            "signed": signature == SignatureState::Trusted,
            "status": signature.as_str(),
            "winVerifyTrustCode": format!("0x{:08X}", trust_code as u32),
            "winVerifyTrustTrusted": trust_code == 0
        },
        "suspiciousStrings": suspicious,
        "findings": findings,
        "riskLevel": risk_level,
        "riskScore": risk_score,
        "recommendation": recommendation,
        "scannedAt": now_ms(),
        "durationMs": started.elapsed().as_millis() as u64
    }))
}

fn hash_file(path: &Path, out: &Emitter) -> Result<String, String> {
    let file =
        File::open(path).map_err(|err| format!("cannot open '{}': {err}", path.display()))?;
    let mut reader = BufReader::with_capacity(HASH_BLOCK, file);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; HASH_BLOCK];
    let mut total: u64 = 0;
    let mut last_report: u64 = 0;

    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|err| format!("read error on '{}': {err}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        total += read as u64;

        if total - last_report >= 64 * 1024 * 1024 {
            last_report = total;
            out.status("hashing file with SHA-256", json!({ "bytesRead": total }));
        }
    }

    Ok(hex::encode(hasher.finalize()))
}

/// Reads the head and the tail of the file so gigabyte-sized installers are
/// inspected without loading them into memory.
fn read_sample(path: &Path, size: u64) -> Vec<u8> {
    let mut sample = Vec::new();
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return sample,
    };

    let head_len = size.min(SAMPLE_EDGE) as usize;
    if head_len > 0 {
        let mut head = vec![0u8; head_len];
        if file.read_exact(&mut head).is_ok() {
            sample.extend_from_slice(&head);
        }
    }

    if size > SAMPLE_EDGE * 2 && file.seek(SeekFrom::End(-(SAMPLE_EDGE as i64))).is_ok() {
        let mut tail = Vec::new();
        if file.take(SAMPLE_EDGE).read_to_end(&mut tail).is_ok() {
            sample.extend_from_slice(&tail);
        }
    }

    sample
}

fn shannon_entropy(data: &[u8]) -> f64 {
    if data.is_empty() {
        return 0.0;
    }

    let mut counts = [0u64; 256];
    for byte in data {
        counts[*byte as usize] += 1;
    }

    let length = data.len() as f64;
    counts
        .iter()
        .filter(|count| **count > 0)
        .map(|count| {
            let probability = *count as f64 / length;
            -probability * probability.log2()
        })
        .sum()
}

fn find_suspicious_strings(sample: &[u8]) -> Vec<Value> {
    let mut hits = Vec::new();

    for (needle, label) in SUSPICIOUS_PATTERNS {
        let pattern = needle.as_bytes();
        if pattern.is_empty() || pattern.len() > sample.len() {
            continue;
        }
        if sample
            .windows(pattern.len())
            .any(|window| window.eq_ignore_ascii_case(pattern))
        {
            hits.push(json!({ "pattern": truncate(needle, 64), "label": label }));
        }
    }

    hits
}

fn classify(
    extension: &str,
    signature: SignatureState,
    entropy: f64,
    suspicious: &[Value],
    is_pe: bool,
    size: u64,
) -> (String, Vec<String>, u32) {
    let mut score: u32 = 0;
    let mut findings: Vec<String> = Vec::new();

    match signature {
        SignatureState::Trusted => {
            findings.push("Authenticode signature is valid and trusted by Windows".to_string())
        }
        SignatureState::Unsigned => {
            score += 40;
            findings.push("No Authenticode signature: publisher cannot be verified".to_string());
        }
        SignatureState::Invalid => {
            score += 70;
            findings.push("Authenticode signature present but INVALID (tampered?)".to_string());
        }
    }

    if EXECUTABLE_EXTENSIONS.contains(&extension) {
        findings.push(format!("Executable payload (.{extension})"));
    } else {
        score += 5;
        findings.push(format!("Non-executable extension (.{extension})"));
    }

    if entropy >= 7.5 {
        score += 30;
        findings.push(format!(
            "Very high entropy {entropy:.2}/8.0: packed or encrypted payload"
        ));
    } else if entropy >= 7.0 {
        score += 15;
        findings.push(format!(
            "Elevated entropy {entropy:.2}/8.0: compressed payload"
        ));
    }

    if matches!(extension, "exe" | "dll" | "scr" | "sys") && !is_pe {
        score += 15;
        findings.push("Extension claims a PE file but the MZ header is missing".to_string());
    }

    if size == 0 {
        score += 10;
        findings.push("Empty file".to_string());
    }

    score += (suspicious.len() as u32 * 12).min(36);

    let risk_level = if score >= 70 {
        "HIGH_RISK"
    } else if score >= 25 {
        "UNSIGNED_WARNING"
    } else {
        "SAFE"
    };

    (risk_level.to_string(), findings, score.min(100))
}

fn downloads_dir() -> PathBuf {
    match env::var_os("USERPROFILE") {
        Some(profile) => Path::new(&profile).join("Downloads"),
        None => PathBuf::from(r"C:\Users\Public\Downloads"),
    }
}

fn latest_download() -> Option<PathBuf> {
    let entries = fs::read_dir(downloads_dir()).ok()?;
    let mut newest: Option<(SystemTime, PathBuf)> = None;

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(UNIX_EPOCH);

        if newest
            .as_ref()
            .map(|(current, _)| modified > *current)
            .unwrap_or(true)
        {
            newest = Some((modified, path));
        }
    }

    newest.map(|(_, path)| path)
}

#[cfg(windows)]
fn verify_authenticode(path: &Path) -> (SignatureState, i32) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::WinTrust::{
        WinVerifyTrust, WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0,
        WINTRUST_FILE_INFO, WTD_CHOICE_FILE, WTD_REVOCATION_CHECK_NONE, WTD_REVOKE_NONE,
        WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE,
    };

    // TRUST_E_NOSIGNATURE: "The file is not digitally signed".
    const TRUST_E_NOSIGNATURE: i32 = 0x800B_0100u32 as i32;
    // TRUST_E_SUBJECT_FORM_UNKNOWN / TRUST_E_PROVIDER_UNKNOWN also mean "unsigned" for us.
    const TRUST_E_SUBJECT_FORM_UNKNOWN: i32 = 0x800B_0003u32 as i32;
    const TRUST_E_PROVIDER_UNKNOWN: i32 = 0x800B_0001u32 as i32;

    let wide_path: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut file_info = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: wide_path.as_ptr(),
        hFile: std::ptr::null_mut(),
        pgKnownSubject: std::ptr::null_mut(),
    };

    let mut trust_data = WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        pPolicyCallbackData: std::ptr::null_mut(),
        pSIPClientData: std::ptr::null_mut(),
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 {
            pFile: &mut file_info,
        },
        dwStateAction: WTD_STATEACTION_VERIFY,
        hWVTStateData: std::ptr::null_mut(),
        pwszURLReference: std::ptr::null_mut(),
        dwProvFlags: WTD_REVOCATION_CHECK_NONE,
        dwUIContext: 0,
        pSignatureSettings: std::ptr::null_mut(),
    };

    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let data_ptr: *mut WINTRUST_DATA = &mut trust_data;

    let status = unsafe { WinVerifyTrust(std::ptr::null_mut(), &mut action, data_ptr.cast()) };

    // Release the state data allocated by the first call. The mutation happens
    // through the raw pointer because that is the buffer WinVerifyTrust reads.
    unsafe {
        (*data_ptr).dwStateAction = WTD_STATEACTION_CLOSE;
        WinVerifyTrust(std::ptr::null_mut(), &mut action, data_ptr.cast());
    }

    let state = match status {
        0 => SignatureState::Trusted,
        TRUST_E_NOSIGNATURE | TRUST_E_SUBJECT_FORM_UNKNOWN | TRUST_E_PROVIDER_UNKNOWN => {
            SignatureState::Unsigned
        }
        _ => SignatureState::Invalid,
    };

    (state, status)
}

#[cfg(not(windows))]
fn verify_authenticode(_path: &Path) -> (SignatureState, i32) {
    (SignatureState::Unsigned, 0x800B_0100u32 as i32)
}
