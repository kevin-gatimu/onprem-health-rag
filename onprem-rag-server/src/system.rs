//! Host machine specs for the Settings "Server specs" card — the machine the
//! *server* runs on (operators care about this box, not the client device).
//!
//! Static facts (hostname, OS, CPU, RAM, accelerators) are computed once and
//! cached; disk usage is re-read per call so free space stays current under the
//! Settings page's polling.

use std::sync::OnceLock;

use serde::Serialize;
use sysinfo::{CpuRefreshKind, Disks, MemoryRefreshKind, RefreshKind, System};

/// One mounted volume's capacity snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct DiskSpec {
    pub mount: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
}

/// Snapshot of the server host for the Settings page.
#[derive(Debug, Clone, Serialize)]
pub struct ServerSpecs {
    pub hostname: String,
    /// e.g. `"Windows 11 (26100)"`.
    pub os: String,
    pub arch: String,
    pub cpu_model: String,
    pub logical_cores: usize,
    pub physical_cores: Option<usize>,
    pub total_memory_bytes: u64,
    pub disks: Vec<DiskSpec>,
    /// GPU/NPU names from OS-level detection (shared with `foundry::hardware`).
    pub accelerators: Vec<String>,
    pub server_version: String,
}

/// Everything except disk usage, computed once (hardware/OS are static at runtime).
struct StaticSpecs {
    hostname: String,
    os: String,
    arch: String,
    cpu_model: String,
    logical_cores: usize,
    physical_cores: Option<usize>,
    total_memory_bytes: u64,
}

fn static_specs() -> &'static StaticSpecs {
    static CACHE: OnceLock<StaticSpecs> = OnceLock::new();
    CACHE.get_or_init(|| {
        let sys = System::new_with_specifics(
            RefreshKind::nothing()
                .with_memory(MemoryRefreshKind::everything())
                .with_cpu(CpuRefreshKind::nothing()),
        );
        let os_name = System::name().unwrap_or_else(|| "unknown".into());
        let os_version = System::os_version().unwrap_or_default();
        StaticSpecs {
            hostname: System::host_name().unwrap_or_else(|| "unknown".into()),
            os: if os_version.is_empty() {
                os_name
            } else {
                format!("{os_name} {os_version}")
            },
            arch: System::cpu_arch(),
            cpu_model: sys
                .cpus()
                .first()
                .map(|c| c.brand().trim().to_string())
                .unwrap_or_else(|| "unknown".into()),
            logical_cores: sys.cpus().len(),
            physical_cores: System::physical_core_count(),
            total_memory_bytes: sys.total_memory(),
        }
    })
}

/// Build the full specs snapshot (cached statics + live disk usage).
pub fn specs() -> ServerSpecs {
    let s = static_specs();

    let disks = Disks::new_with_refreshed_list()
        .iter()
        .map(|d| DiskSpec {
            mount: d.mount_point().to_string_lossy().into_owned(),
            total_bytes: d.total_space(),
            available_bytes: d.available_space(),
        })
        .collect();

    let accelerators = crate::foundry::hardware::detect()
        .iter()
        .filter(|d| d.kind != "CPU")
        .map(|d| format!("{} — {}", d.kind, d.name))
        .collect();

    ServerSpecs {
        hostname: s.hostname.clone(),
        os: s.os.clone(),
        arch: s.arch.clone(),
        cpu_model: s.cpu_model.clone(),
        logical_cores: s.logical_cores,
        physical_cores: s.physical_cores,
        total_memory_bytes: s.total_memory_bytes,
        disks,
        accelerators,
        server_version: env!("CARGO_PKG_VERSION").to_string(),
    }
}
