//! OS-level accelerator detection — the *physical* devices present on the host,
//! independent of Foundry Local's execution-provider (EP) registration.
//!
//! Why this exists: Foundry's `discover_eps()` reports which EPs it has *registered*
//! (e.g. `OpenVINOExecutionProvider: registered=false`), which is confusing on a machine
//! that clearly has an Intel Arc GPU and an NPU. This module answers the orthogonal
//! question "what hardware is actually here?" so the UI can pair the two: "Intel Arc GPU
//! present — OpenVINO EP registered" vs "present but EP not registered yet".
//!
//! Detection is best-effort and cached for the process lifetime (hardware doesn't change
//! at runtime). On Windows it shells out to PowerShell CIM queries; on other platforms it
//! returns an empty list (the app targets Windows on-prem). Any failure yields `[]` — the
//! EP list from Foundry is always shown regardless.

use std::sync::OnceLock;

use serde::Serialize;

/// A physical accelerator detected on the host.
#[derive(Debug, Clone, Serialize)]
pub struct DetectedDevice {
    /// Coarse class: `"CPU"`, `"GPU"`, or `"NPU"`.
    pub kind: String,
    /// Device name as reported by the OS (e.g. `"Intel(R) Arc(TM) Graphics"`).
    pub name: String,
    /// Best-effort vendor (`"Intel"`, `"NVIDIA"`, `"AMD"`, `"Qualcomm"`, …).
    pub vendor: String,
}

/// Detected accelerators, computed once and cached (hardware is static at runtime).
pub fn detect() -> &'static [DetectedDevice] {
    static CACHE: OnceLock<Vec<DetectedDevice>> = OnceLock::new();
    CACHE.get_or_init(detect_uncached)
}

fn detect_uncached() -> Vec<DetectedDevice> {
    #[cfg(windows)]
    {
        match detect_windows() {
            Ok(devs) => devs,
            Err(e) => {
                tracing::warn!(error = %e, "hardware: OS-level device detection failed");
                Vec::new()
            }
        }
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Infer a vendor from a device name via substring match (case-insensitive).
fn vendor_of(name: &str) -> String {
    let n = name.to_ascii_lowercase();
    if n.contains("nvidia") || n.contains("geforce") || n.contains("quadro") || n.contains("tesla")
    {
        "NVIDIA"
    } else if n.contains("intel") {
        "Intel"
    } else if n.contains("amd") || n.contains("radeon") || n.contains("ryzen") {
        "AMD"
    } else if n.contains("qualcomm") || n.contains("snapdragon") || n.contains("hexagon") {
        "Qualcomm"
    } else if n.contains("apple") {
        "Apple"
    } else if n.contains("microsoft") {
        "Microsoft"
    } else {
        "Unknown"
    }
    .to_string()
}

#[cfg(windows)]
fn detect_windows() -> Result<Vec<DetectedDevice>, String> {
    use serde::Deserialize;

    // One PowerShell round-trip returning JSON. `Win32_Processor` for the CPU,
    // `Win32_VideoController` for display adapters (GPUs), and PnP entities whose
    // name matches known NPU markers (Intel "AI Boost", Qualcomm "Hexagon", etc.).
    // -Compress keeps it a single line; -ErrorActionPreference swallows CIM hiccups.
    const SCRIPT: &str = r#"
$ErrorActionPreference='SilentlyContinue'
$cpu  = (Get-CimInstance Win32_Processor | Select-Object -First 1).Name
$gpus = @(Get-CimInstance Win32_VideoController | ForEach-Object { $_.Name })
$npus = @(Get-CimInstance Win32_PnPEntity |
    Where-Object { $_.Name -match 'AI Boost|Neural Processor|\bNPU\b|Hexagon|VPU' } |
    ForEach-Object { $_.Name } | Select-Object -Unique)
[pscustomobject]@{ cpu = $cpu; gpus = $gpus; npus = $npus } | ConvertTo-Json -Compress
"#;

    #[derive(Deserialize)]
    struct Raw {
        cpu: Option<String>,
        #[serde(default)]
        gpus: Vec<String>,
        #[serde(default)]
        npus: Vec<String>,
    }

    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
        .output()
        .map_err(|e| format!("spawning powershell: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "powershell exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let raw: Raw = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("parsing device JSON: {e} (got: {})", stdout.trim()))?;

    let mut devices = Vec::new();
    if let Some(cpu) = raw.cpu.filter(|s| !s.trim().is_empty()) {
        let cpu = cpu.trim().to_string();
        let vendor = vendor_of(&cpu);
        devices.push(DetectedDevice {
            kind: "CPU".into(),
            name: cpu,
            vendor,
        });
    }
    for gpu in raw.gpus.into_iter().filter(|s| !s.trim().is_empty()) {
        let gpu = gpu.trim().to_string();
        let vendor = vendor_of(&gpu);
        devices.push(DetectedDevice {
            kind: "GPU".into(),
            name: gpu,
            vendor,
        });
    }
    for npu in raw.npus.into_iter().filter(|s| !s.trim().is_empty()) {
        let npu = npu.trim().to_string();
        let vendor = vendor_of(&npu);
        devices.push(DetectedDevice {
            kind: "NPU".into(),
            name: npu,
            vendor,
        });
    }
    Ok(devices)
}
