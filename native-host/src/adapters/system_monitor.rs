// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 sovith3 - https://github.com/sovith3

//! Adapter 3 - Real-time hardware and process telemetry (`sysinfo`, no WMI).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use sysinfo::{Disks, ProcessesToUpdate, System, MINIMUM_CPU_UPDATE_INTERVAL};

use crate::adapters::{payload_u64, AdapterFuture, BridgeAdapter, Request};
use crate::bridge_core::{now_ms, Emitter};

pub const ACTIONS: [&str; 3] = ["GET_SNAPSHOT", "START_TELEMETRY", "STOP_TELEMETRY"];
const DEFAULT_INTERVAL_MS: u64 = 1500;
const MIN_INTERVAL_MS: u64 = 500;
const MAX_INTERVAL_MS: u64 = 60_000;

struct MonitorState {
    system: System,
    disks: Disks,
}

pub struct SystemMonitor {
    state: Arc<Mutex<MonitorState>>,
    telemetry: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl SystemMonitor {
    pub fn new() -> Self {
        let mut system = System::new_all();
        system.refresh_cpu_usage();

        Self {
            state: Arc::new(Mutex::new(MonitorState {
                system,
                disks: Disks::new_with_refreshed_list(),
            })),
            telemetry: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }
}

impl Default for SystemMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl BridgeAdapter for SystemMonitor {
    fn name(&self) -> &'static str {
        "system_monitor"
    }

    fn actions(&self) -> &'static [&'static str] {
        &ACTIONS
    }

    fn handle<'a>(&'a self, request: Request<'a>, out: &'a Emitter) -> AdapterFuture<'a> {
        Box::pin(async move {
            match request.action {
                "GET_SNAPSHOT" => {
                    out.status("sampling hardware", json!({}));
                    let state = Arc::clone(&self.state);
                    let snapshot = tokio::task::spawn_blocking(move || collect(&state, true, true))
                        .await
                        .unwrap_or_else(
                            |err| json!({ "error": format!("telemetry worker crashed: {err}") }),
                        );
                    out.result(snapshot);
                }
                "START_TELEMETRY" => {
                    let interval = payload_u64(request.payload, "intervalMs")
                        .unwrap_or(DEFAULT_INTERVAL_MS)
                        .clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS);

                    if let Some(previous) = self.telemetry.lock().await.take() {
                        previous.abort();
                    }

                    let state = Arc::clone(&self.state);
                    let emitter = out.clone();
                    let handle = tokio::spawn(async move {
                        emitter.status(
                            "telemetry stream started",
                            json!({ "intervalMs": interval }),
                        );

                        loop {
                            let worker_state = Arc::clone(&state);
                            let frame = match tokio::task::spawn_blocking(move || {
                                collect(&worker_state, true, false)
                            })
                            .await
                            {
                                Ok(frame) => frame,
                                Err(_) => break,
                            };

                            emitter.stream(frame);
                            tokio::time::sleep(Duration::from_millis(interval)).await;
                        }
                    });

                    *self.telemetry.lock().await = Some(handle);
                    out.result(json!({ "streaming": true, "intervalMs": interval }));
                }
                "STOP_TELEMETRY" => {
                    let stopped = self.telemetry.lock().await.take().is_some();
                    if stopped {
                        out.status("telemetry stream stopped", json!({}));
                    }
                    out.result(json!({ "streaming": false, "stopped": stopped }));
                }
                other => out.error(
                    "UNKNOWN_ACTION",
                    format!(
                        "system_monitor does not support '{other}' (available: {})",
                        self.actions().join(", ")
                    ),
                ),
            }
        })
    }
}

fn collect(state: &Mutex<MonitorState>, include_processes: bool, warmup: bool) -> Value {
    let mut guard = match state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let inner = &mut *guard;

    if warmup {
        inner.system.refresh_cpu_usage();
        std::thread::sleep(MINIMUM_CPU_UPDATE_INTERVAL);
    }

    inner.system.refresh_cpu_usage();
    inner.system.refresh_memory();
    if include_processes {
        inner.system.refresh_processes(ProcessesToUpdate::All);
    }
    inner.disks.refresh();

    build_frame(&inner.system, &inner.disks, include_processes)
}

fn build_frame(system: &System, disks: &Disks, include_processes: bool) -> Value {
    let total_memory = system.total_memory();
    let used_memory = system.used_memory();
    let swap_total = system.total_swap();
    let swap_used = system.used_swap();

    let per_core: Vec<Value> = system
        .cpus()
        .iter()
        .map(|cpu| json!(round2(cpu.cpu_usage() as f64)))
        .collect();

    let disk_frames: Vec<Value> = disks
        .list()
        .iter()
        .map(|disk| {
            let total = disk.total_space();
            let available = disk.available_space();
            let used = total.saturating_sub(available);
            json!({
                "name": disk.name().to_string_lossy(),
                "mountPoint": disk.mount_point().to_string_lossy(),
                "fileSystem": disk.file_system().to_string_lossy(),
                "totalBytes": total,
                "usedBytes": used,
                "availableBytes": available,
                "usagePercent": percent(used, total),
                "removable": disk.is_removable(),
            })
        })
        .collect();

    let mut frame = json!({
        "timestamp": now_ms(),
        "host": {
            "name": System::host_name().unwrap_or_else(|| "unknown".to_string()),
            "osName": System::name().unwrap_or_else(|| "unknown".to_string()),
            "osVersion": System::os_version().unwrap_or_else(|| "unknown".to_string()),
            "kernelVersion": System::kernel_version().unwrap_or_else(|| "unknown".to_string()),
            "cpuArch": System::cpu_arch(),
            "cpuBrand": system.cpus().first().map(|cpu| cpu.brand().to_string()).unwrap_or_default(),
            "physicalCores": system.physical_core_count().unwrap_or(0),
            "logicalCores": system.cpus().len(),
        },
        "cpu": {
            "usagePercent": round2(system.global_cpu_usage() as f64),
            "perCorePercent": per_core,
            "logicalCores": system.cpus().len(),
        },
        "memory": {
            "totalBytes": total_memory,
            "usedBytes": used_memory,
            "availableBytes": total_memory.saturating_sub(used_memory),
            "usagePercent": percent(used_memory, total_memory),
            "swapTotalBytes": swap_total,
            "swapUsedBytes": swap_used,
            "swapUsagePercent": percent(swap_used, swap_total),
        },
        "uptimeSeconds": System::uptime(),
        "processCount": system.processes().len(),
        "disks": disk_frames,
    });

    if include_processes {
        let processes: Vec<(u32, String, f32, u64)> = system
            .processes()
            .iter()
            .map(|(pid, process)| {
                (
                    pid.as_u32(),
                    process.name().to_string_lossy().to_string(),
                    process.cpu_usage(),
                    process.memory(),
                )
            })
            .collect();

        let mut by_cpu = processes.clone();
        by_cpu.sort_by(|left, right| {
            right
                .2
                .partial_cmp(&left.2)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(right.3.cmp(&left.3))
        });

        let mut by_memory = processes;
        by_memory.sort_by_key(|process| std::cmp::Reverse(process.3));

        frame["topCpuProcesses"] = json!(by_cpu
            .iter()
            .take(10)
            .map(|(pid, name, cpu, memory)| json!({
                "pid": pid,
                "name": name,
                "cpuPercent": round2(*cpu as f64),
                "memoryBytes": memory,
            }))
            .collect::<Vec<_>>());

        frame["topMemoryProcesses"] = json!(by_memory
            .iter()
            .take(10)
            .map(|(pid, name, cpu, memory)| json!({
                "pid": pid,
                "name": name,
                "cpuPercent": round2(*cpu as f64),
                "memoryBytes": memory,
            }))
            .collect::<Vec<_>>());
    }

    frame
}

fn percent(used: u64, total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    round2(used as f64 / total as f64 * 100.0)
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_handles_zero_capacity_and_rounds_to_two_decimals() {
        assert_eq!(percent(0, 0), 0.0);
        assert_eq!(percent(1, 2), 50.0);
        assert_eq!(percent(1, 3), 33.33);
        assert_eq!(percent(2, 3), 66.67);
        assert_eq!(percent(5, 4), 125.0);
    }

    #[test]
    fn round2_keeps_two_decimals() {
        assert_eq!(round2(1.0 / 3.0), 0.33);
        assert_eq!(round2(12.3456), 12.35);
        assert_eq!(round2(-12.3456), -12.35);
    }
}
