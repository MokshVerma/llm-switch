//! What this machine has, as far as choosing a llama.cpp build and sizing a model is concerned.

use crate::platform::{self, Arch, Os};
use sysinfo::System;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    Nvidia,
    Amd,
    Intel,
    Apple,
    Other,
}

#[derive(Debug, Clone)]
pub struct Gpu {
    pub name: String,
    pub vendor: Vendor,
    pub vram_mb: Option<u32>,
    /// NVIDIA driver version such as "621.01".
    pub driver: Option<String>,
    /// NVIDIA compute capability such as 8.6.
    pub compute_cap: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct Hardware {
    pub os: Os,
    pub arch: Arch,
    pub gpus: Vec<Gpu>,
    pub ram_mb: u64,
}

pub fn vendor_of(name: &str) -> Vendor {
    let n = name.to_lowercase();
    if n.contains("nvidia") || n.contains("geforce") || n.contains("quadro") || n.contains("rtx") {
        Vendor::Nvidia
    } else if n.contains("amd") || n.contains("radeon") || n.contains("advanced micro") {
        Vendor::Amd
    } else if n.contains("intel") {
        Vendor::Intel
    } else if n.contains("apple") {
        Vendor::Apple
    } else {
        Vendor::Other
    }
}

impl Hardware {
    /// The first NVIDIA GPU, if any.
    pub fn nvidia(&self) -> Option<&Gpu> {
        self.gpus.iter().find(|g| g.vendor == Vendor::Nvidia)
    }

    pub fn summary(&self) -> String {
        let os = match self.os {
            Os::Windows => "Windows",
            Os::MacOs => "macOS",
            Os::Linux => "Linux",
        };
        let arch = match self.arch {
            Arch::X64 => "x64",
            Arch::Arm64 => "arm64",
            Arch::Other => "unknown CPU",
        };
        let gpus = if self.gpus.is_empty() {
            "no GPU found".to_string()
        } else {
            self.gpus
                .iter()
                .map(|g| {
                    let mut s = g.name.clone();
                    if let Some(v) = g.vram_mb {
                        s += &format!(", {:.0} GB VRAM", v as f64 / 1024.0);
                    }
                    if let Some(d) = &g.driver {
                        s += &format!(", driver {d}");
                    }
                    if let Some(c) = g.compute_cap {
                        s += &format!(", compute {c:.1}");
                    }
                    s
                })
                .collect::<Vec<_>>()
                .join("; ")
        };
        format!("{os} {arch}, {:.0} GB RAM, {gpus}", self.ram_mb as f64 / 1024.0)
    }
}

/// Parses the CSV that `nvidia-smi --query-gpu=name,memory.total,driver_version,compute_cap` prints.
pub fn parse_nvidia_csv(text: &str) -> Vec<Gpu> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
            if f.len() < 3 || f[0].is_empty() {
                return None;
            }
            Some(Gpu {
                name: f[0].to_string(),
                vendor: Vendor::Nvidia,
                vram_mb: f[1].parse().ok(),
                driver: Some(f[2].to_string()).filter(|d| !d.is_empty()),
                compute_cap: f.get(3).and_then(|c| c.parse().ok()),
            })
        })
        .collect()
}

pub fn detect() -> Hardware {
    let mut gpus: Vec<Gpu> = Vec::new();

    // NVIDIA, with details. Older drivers do not know `compute_cap`, so retry without it.
    let nvidia = platform::run_capture(
        "nvidia-smi",
        &["--query-gpu=name,memory.total,driver_version,compute_cap", "--format=csv,noheader,nounits"],
    )
    .or_else(|| {
        platform::run_capture(
            "nvidia-smi",
            &["--query-gpu=name,memory.total,driver_version", "--format=csv,noheader,nounits"],
        )
    });
    if let Some(text) = nvidia {
        gpus.extend(parse_nvidia_csv(&text));
    }

    // Everything else the OS reports (AMD, Intel, Apple); skip what nvidia-smi already described.
    for name in platform::display_adapters() {
        let vendor = vendor_of(&name);
        if vendor == Vendor::Nvidia && gpus.iter().any(|g| g.vendor == Vendor::Nvidia) {
            continue;
        }
        let lower = name.to_lowercase();
        if lower.contains("microsoft basic") || lower.contains("remote display") || lower.contains("virtual") {
            continue;
        }
        gpus.push(Gpu { name, vendor, vram_mb: None, driver: None, compute_cap: None });
    }

    let mut arch = platform::arch();
    if platform::os() == Os::MacOs && platform::apple_silicon() {
        arch = Arch::Arm64;
        if !gpus.iter().any(|g| g.vendor == Vendor::Apple) {
            gpus.push(Gpu { name: "Apple GPU".into(), vendor: Vendor::Apple, vram_mb: None, driver: None, compute_cap: None });
        }
    }

    let mut sys = System::new().expect("sysinfo init failed");
    sys.refresh_memory();
    Hardware { os: platform::os(), arch, gpus, ram_mb: sys.total_memory() / (1024 * 1024) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nvidia_smi_output() {
        let gpus = parse_nvidia_csv("NVIDIA GeForce RTX 3060, 12288, 621.01, 8.6\n");
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].name, "NVIDIA GeForce RTX 3060");
        assert_eq!(gpus[0].vram_mb, Some(12288));
        assert_eq!(gpus[0].driver.as_deref(), Some("621.01"));
        assert_eq!(gpus[0].compute_cap, Some(8.6));
    }

    #[test]
    fn parses_old_driver_output_without_compute_cap() {
        let gpus = parse_nvidia_csv("NVIDIA GeForce GTX 1060 6GB, 6144, 472.12\n");
        assert_eq!(gpus[0].compute_cap, None);
        assert_eq!(gpus[0].vram_mb, Some(6144));
    }

    #[test]
    fn ignores_garbage_lines() {
        assert!(parse_nvidia_csv("").is_empty());
        assert!(parse_nvidia_csv("NVIDIA-SMI has failed because it couldn't communicate with the NVIDIA driver.").is_empty());
    }

    #[test]
    fn classifies_vendors() {
        assert_eq!(vendor_of("NVIDIA GeForce RTX 3060"), Vendor::Nvidia);
        assert_eq!(vendor_of("AMD Radeon RX 7800 XT"), Vendor::Amd);
        assert_eq!(vendor_of("Intel(R) UHD Graphics 730"), Vendor::Intel);
        assert_eq!(vendor_of("Apple M2 Pro"), Vendor::Apple);
        assert_eq!(vendor_of("Matrox G200"), Vendor::Other);
    }
}
