//! Finds, downloads and installs the right llama.cpp build for this machine.

use crate::download::{self, Job, Progress};
use crate::hardware::{Gpu, Hardware};
use crate::platform::{self, Arch, Os, SERVER_EXE};
use anyhow::{Context, Result, anyhow, bail};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::time::Duration;

const RELEASES_URL: &str = "https://api.github.com/repos/ggml-org/llama.cpp/releases?per_page=15";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    /// NVIDIA, with the CUDA version of the build.
    Cuda(String),
    Vulkan,
    Cpu,
    /// Apple GPUs (and Intel Macs).
    Metal,
}

impl Backend {
    pub fn label(&self) -> String {
        match self {
            Backend::Cuda(v) => format!("CUDA {v} (NVIDIA)"),
            Backend::Vulkan => "Vulkan (any GPU)".into(),
            Backend::Cpu => "CPU only".into(),
            Backend::Metal => "Metal (Apple)".into(),
        }
    }
}

/// Which release files to fetch, decided from the machine and the names a release offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub primary: String,
    /// The CUDA runtime libraries, shipped as a separate archive that is unpacked next to the build.
    pub runtime: Option<String>,
    pub backend: Backend,
    pub note: Option<String>,
}

/// Highest CUDA major version this GPU and driver can run, or None when the driver is too old (or unknown).
fn cuda_cap(gpu: &Gpu) -> Option<u32> {
    let driver_major: u32 = gpu.driver.as_deref()?.split('.').next()?.parse().ok()?;
    let mut cap = if driver_major >= 580 {
        13
    } else if driver_major >= 527 {
        12
    } else {
        return None;
    };
    // CUDA 13 dropped GPUs older than Turing (compute capability 7.5).
    if gpu.compute_cap.is_some_and(|c| c < 7.5) {
        cap = cap.min(12);
    }
    Some(cap)
}

/// `llama-b123-bin-win-cuda-12.4-x64.zip` -> (12, 4)
fn cuda_version(name: &str) -> Option<(u32, u32)> {
    let rest = name.split("-cuda-").nth(1)?;
    let ver = rest.split('-').next()?;
    let (maj, min) = ver.split_once('.')?;
    Some((maj.parse().ok()?, min.parse().ok()?))
}

pub fn choose(hw: &Hardware, names: &[String]) -> Option<Choice> {
    let find = |suffix: &str| names.iter().find(|n| n.starts_with("llama-") && n.ends_with(suffix)).cloned();
    let plain = |primary: String, backend: Backend, note: Option<String>| Some(Choice { primary, runtime: None, backend, note });

    match (hw.os, hw.arch) {
        (Os::Windows, Arch::X64) => {
            let mut note = None;
            if let Some(gpu) = hw.nvidia() {
                match cuda_cap(gpu) {
                    Some(cap) => {
                        let best = names
                            .iter()
                            .filter(|n| n.starts_with("llama-") && n.contains("-bin-win-cuda-") && n.ends_with("-x64.zip"))
                            .filter_map(|n| cuda_version(n).map(|v| (v, n)))
                            .filter(|(v, _)| v.0 <= cap)
                            .max_by_key(|(v, _)| *v);
                        if let Some(((maj, min), primary)) = best {
                            let ver = format!("{maj}.{min}");
                            let runtime = names.iter().find(|n| **n == format!("cudart-llama-bin-win-cuda-{ver}-x64.zip")).cloned();
                            return Some(Choice { primary: primary.clone(), runtime, backend: Backend::Cuda(ver), note: None });
                        }
                    }
                    None => {
                        note = Some(
                            "Your NVIDIA driver is too old for the CUDA builds (527 or newer is needed), so the Vulkan build is used. \
                             Updating the driver would make it faster."
                                .to_string(),
                        );
                    }
                }
            }
            if !hw.gpus.is_empty() {
                if let Some(p) = find("-bin-win-vulkan-x64.zip") {
                    return plain(p, Backend::Vulkan, note);
                }
            }
            find("-bin-win-cpu-x64.zip").and_then(|p| plain(p, Backend::Cpu, note))
        }
        (Os::Windows, Arch::Arm64) => find("-bin-win-cpu-arm64.zip").and_then(|p| plain(p, Backend::Cpu, None)),
        (Os::MacOs, Arch::Arm64) => find("-bin-macos-arm64.tar.gz").and_then(|p| plain(p, Backend::Metal, None)),
        (Os::MacOs, Arch::X64) => find("-bin-macos-x64.tar.gz").and_then(|p| plain(p, Backend::Metal, None)),
        (Os::Linux, Arch::X64) => {
            if !hw.gpus.is_empty() {
                if let Some(p) = find("-bin-ubuntu-vulkan-x64.tar.gz") {
                    return plain(
                        p,
                        Backend::Vulkan,
                        Some("For CUDA or ROCm on Linux, build llama.cpp yourself and set llama_server in the config.".into()),
                    );
                }
            }
            find("-bin-ubuntu-x64.tar.gz").and_then(|p| plain(p, Backend::Cpu, None))
        }
        (Os::Linux, Arch::Arm64) => find("-bin-ubuntu-arm64.tar.gz").and_then(|p| plain(p, Backend::Cpu, None)),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub struct AssetRef {
    pub name: String,
    pub size: u64,
    pub url: String,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub release: String,
    pub backend: Backend,
    pub note: Option<String>,
    pub primary: AssetRef,
    pub runtime: Option<AssetRef>,
}

impl Plan {
    pub fn total_bytes(&self) -> u64 {
        self.primary.size + self.runtime.as_ref().map_or(0, |r| r.size)
    }
}

/// Looks through the newest llama.cpp releases for one that has a build for this machine.
pub fn plan(hw: &Hardware) -> Result<Plan> {
    let releases = download::get_json(RELEASES_URL, None).context("asking GitHub for llama.cpp releases")?;
    let releases = releases.as_array().ok_or_else(|| anyhow!("unexpected reply from GitHub"))?;
    for rel in releases {
        if rel["draft"].as_bool().unwrap_or(false) {
            continue;
        }
        let Some(assets) = rel["assets"].as_array() else { continue };
        let names: Vec<String> = assets.iter().filter_map(|a| a["name"].as_str().map(String::from)).collect();
        let Some(choice) = choose(hw, &names) else { continue };

        let asset = |name: &str| -> Option<AssetRef> {
            let a = assets.iter().find(|a| a["name"].as_str() == Some(name))?;
            Some(AssetRef {
                name: name.to_string(),
                size: a["size"].as_u64()?,
                url: a["browser_download_url"].as_str()?.to_string(),
                sha256: a["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).map(String::from),
            })
        };
        let primary = asset(&choice.primary).ok_or_else(|| anyhow!("release asset {} is incomplete", choice.primary))?;
        let runtime = choice.runtime.as_deref().and_then(asset);
        return Ok(Plan {
            release: rel["tag_name"].as_str().unwrap_or("?").to_string(),
            backend: choice.backend,
            note: choice.note,
            primary,
            runtime,
        });
    }
    bail!(
        "no ready-made llama.cpp build was found for this system ({:?} {:?}). Build llama.cpp from source \
         and set llama_server in the config",
        hw.os,
        hw.arch
    )
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<()> {
    let mut zip = zip::ZipArchive::new(File::open(archive)?).with_context(|| format!("opening {}", archive.display()))?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        // `enclosed_name` rejects entries that would escape the destination folder.
        let Some(rel) = entry.enclosed_name() else { continue };
        let out = dest.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        io::copy(&mut entry, &mut File::create(&out)?)?;
    }
    Ok(())
}

fn extract_tar_gz(archive: &Path, dest: &Path) -> Result<()> {
    let gz = flate2::read::GzDecoder::new(File::open(archive)?);
    let mut tar = tar::Archive::new(gz);
    tar.set_preserve_permissions(true);
    tar.unpack(dest).with_context(|| format!("unpacking {}", archive.display()))
}

fn extract(archive: &Path, dest: &Path) -> Result<()> {
    let name = archive.file_name().and_then(|n| n.to_str()).unwrap_or("").to_lowercase();
    if name.ends_with(".zip") {
        extract_zip(archive, dest)
    } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        extract_tar_gz(archive, dest)
    } else {
        bail!("don't know how to unpack {name}")
    }
}

/// The shallowest `llama-server` inside `dir`.
pub fn find_server(dir: &Path) -> Option<PathBuf> {
    fn walk(dir: &Path, depth: u32, best: &mut Option<(u32, PathBuf)>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if depth < 4 {
                    walk(&p, depth + 1, best);
                }
            } else if p.file_name().and_then(|n| n.to_str()) == Some(SERVER_EXE) && best.as_ref().is_none_or(|(d, _)| depth < *d) {
                *best = Some((depth, p));
            }
        }
    }
    let mut best = None;
    walk(dir, 0, &mut best);
    best.map(|(_, p)| p)
}

/// Starts `llama-server --version` to prove the install works. Returns the version line.
pub fn verify_server(exe: &Path) -> Result<String> {
    let exe = exe.to_path_buf();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn({
        let exe = exe.clone();
        move || {
            let mut cmd = std::process::Command::new(&exe);
            cmd.arg("--version").current_dir(exe.parent().unwrap_or_else(|| Path::new(".")));
            let _ = tx.send(platform::hide_console(&mut cmd).output());
        }
    });
    let out = match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => bail!("could not start {}: {e}", exe.display()),
        Err(_) => bail!("{} did not answer within 30 seconds", exe.display()),
    };
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if out.status.success() {
        let line = text.lines().find(|l| l.to_lowercase().contains("version")).unwrap_or("ok").trim().to_string();
        return Ok(line);
    }
    // 0xC0000135 (STATUS_DLL_NOT_FOUND): a Windows machine without the Visual C++ runtime.
    if out.status.code() == Some(-1_073_741_515) {
        bail!(
            "llama-server could not start because a required DLL is missing. Install the Microsoft Visual C++ \
             Redistributable (https://aka.ms/vs/17/release/vc_redist.x64.exe) and try again"
        );
    }
    bail!("llama-server exited with {:?}: {}", out.status.code(), text.trim())
}

/// Downloads and unpacks the plan into `dest`, then checks that the server starts. Returns the path to
/// `llama-server` and its version line. `on_progress` gets the name of the file being fetched.
pub fn install(plan: &Plan, dest: &Path, on_progress: &mut dyn FnMut(&str, Progress), cancel: &AtomicBool) -> Result<(PathBuf, String)> {
    fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;
    // Archives plus what they unpack to.
    let needed = plan.total_bytes() * 3;
    if let Some(free) = download::free_space(dest) {
        if free < needed {
            bail!(
                "not enough free space on the drive for {}: about {:.1} GB needed, {:.1} GB free",
                dest.display(),
                needed as f64 / 1e9,
                free as f64 / 1e9
            );
        }
    }

    let tmp = dest.join(".download");
    let assets: Vec<&AssetRef> = std::iter::once(&plan.primary).chain(plan.runtime.as_ref()).collect();
    for asset in &assets {
        let job = Job { url: &asset.url, token: None, size: Some(asset.size), sha256: asset.sha256.as_deref() };
        download::fetch_to_file(&job, &tmp.join(&asset.name), &mut |p| on_progress(&asset.name, p), cancel)?;
    }
    for asset in &assets {
        extract(&tmp.join(&asset.name), dest)?;
    }
    let _ = fs::remove_dir_all(&tmp);

    let exe = find_server(dest).ok_or_else(|| anyhow!("the download did not contain {SERVER_EXE}"))?;
    let version = verify_server(&exe)?;
    Ok((exe, version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::Vendor;

    /// Asset names copied from a real llama.cpp release (b12128).
    fn names() -> Vec<String> {
        [
            "cudart-llama-bin-win-cuda-12.4-x64.zip",
            "cudart-llama-bin-win-cuda-13.4-x64.zip",
            "cudart-llama-bin-win-cuda-13.4-arm64.zip",
            "llama-b12128-bin-macos-arm64.tar.gz",
            "llama-b12128-bin-macos-x64.tar.gz",
            "llama-b12128-bin-ubuntu-arm64.tar.gz",
            "llama-b12128-bin-ubuntu-cuda-12.8-x64.tar.gz",
            "llama-b12128-bin-ubuntu-vulkan-x64.tar.gz",
            "llama-b12128-bin-ubuntu-x64.tar.gz",
            "llama-b12128-bin-win-cpu-arm64.zip",
            "llama-b12128-bin-win-cpu-x64.zip",
            "llama-b12128-bin-win-cuda-12.4-x64.zip",
            "llama-b12128-bin-win-cuda-13.4-arm64.zip",
            "llama-b12128-bin-win-cuda-13.4-x64.zip",
            "llama-b12128-bin-win-rocm-10.0-x64.zip",
            "llama-b12128-bin-win-sycl-x64.zip",
            "llama-b12128-bin-win-vulkan-x64.zip",
            "llama-b12128-ui.tar.gz",
            "llama-b12128-xcframework.zip",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    fn nvidia(driver: &str, cc: Option<f32>) -> Gpu {
        Gpu { name: "NVIDIA GeForce".into(), vendor: Vendor::Nvidia, vram_mb: Some(12288), driver: Some(driver.into()), compute_cap: cc }
    }

    fn other(vendor: Vendor) -> Gpu {
        Gpu { name: "GPU".into(), vendor, vram_mb: None, driver: None, compute_cap: None }
    }

    fn hw(os: Os, arch: Arch, gpus: Vec<Gpu>) -> Hardware {
        Hardware { os, arch, gpus, ram_mb: 32768 }
    }

    #[test]
    fn rtx_3060_on_a_new_driver_gets_cuda_13_with_its_runtime() {
        let c = choose(&hw(Os::Windows, Arch::X64, vec![nvidia("621.01", Some(8.6))]), &names()).unwrap();
        assert_eq!(c.primary, "llama-b12128-bin-win-cuda-13.4-x64.zip");
        assert_eq!(c.runtime.as_deref(), Some("cudart-llama-bin-win-cuda-13.4-x64.zip"));
        assert_eq!(c.backend, Backend::Cuda("13.4".into()));
    }

    #[test]
    fn a_pascal_card_cannot_use_cuda_13() {
        let c = choose(&hw(Os::Windows, Arch::X64, vec![nvidia("581.15", Some(6.1))]), &names()).unwrap();
        assert_eq!(c.primary, "llama-b12128-bin-win-cuda-12.4-x64.zip");
        assert_eq!(c.runtime.as_deref(), Some("cudart-llama-bin-win-cuda-12.4-x64.zip"));
    }

    #[test]
    fn a_mid_age_driver_gets_cuda_12() {
        let c = choose(&hw(Os::Windows, Arch::X64, vec![nvidia("551.78", Some(8.6))]), &names()).unwrap();
        assert_eq!(c.backend, Backend::Cuda("12.4".into()));
    }

    #[test]
    fn an_old_nvidia_driver_falls_back_to_vulkan_and_says_why() {
        let c = choose(&hw(Os::Windows, Arch::X64, vec![nvidia("472.12", Some(6.1))]), &names()).unwrap();
        assert_eq!(c.backend, Backend::Vulkan);
        assert!(c.note.unwrap().contains("driver"));
    }

    #[test]
    fn unknown_driver_is_treated_as_too_old_rather_than_guessing() {
        let mut g = nvidia("x", None);
        g.driver = None;
        let c = choose(&hw(Os::Windows, Arch::X64, vec![g]), &names()).unwrap();
        assert_eq!(c.backend, Backend::Vulkan);
    }

    #[test]
    fn amd_and_intel_use_vulkan_and_no_gpu_uses_cpu_on_windows() {
        assert_eq!(choose(&hw(Os::Windows, Arch::X64, vec![other(Vendor::Amd)]), &names()).unwrap().backend, Backend::Vulkan);
        assert_eq!(choose(&hw(Os::Windows, Arch::X64, vec![other(Vendor::Intel)]), &names()).unwrap().primary, "llama-b12128-bin-win-vulkan-x64.zip");
        assert_eq!(choose(&hw(Os::Windows, Arch::X64, vec![]), &names()).unwrap().primary, "llama-b12128-bin-win-cpu-x64.zip");
    }

    #[test]
    fn windows_on_arm_uses_the_arm_cpu_build_not_the_x64_one() {
        assert_eq!(choose(&hw(Os::Windows, Arch::Arm64, vec![]), &names()).unwrap().primary, "llama-b12128-bin-win-cpu-arm64.zip");
    }

    #[test]
    fn macs_get_the_matching_architecture() {
        let arm = choose(&hw(Os::MacOs, Arch::Arm64, vec![other(Vendor::Apple)]), &names()).unwrap();
        assert_eq!(arm.primary, "llama-b12128-bin-macos-arm64.tar.gz");
        assert_eq!(arm.backend, Backend::Metal);
        assert_eq!(choose(&hw(Os::MacOs, Arch::X64, vec![]), &names()).unwrap().primary, "llama-b12128-bin-macos-x64.tar.gz");
    }

    #[test]
    fn linux_prefers_vulkan_with_a_gpu_and_never_picks_the_cuda_tarball_blindly() {
        let g = choose(&hw(Os::Linux, Arch::X64, vec![nvidia("580.1", Some(8.6))]), &names()).unwrap();
        assert_eq!(g.primary, "llama-b12128-bin-ubuntu-vulkan-x64.tar.gz");
        assert_eq!(choose(&hw(Os::Linux, Arch::X64, vec![]), &names()).unwrap().primary, "llama-b12128-bin-ubuntu-x64.tar.gz");
        assert_eq!(choose(&hw(Os::Linux, Arch::Arm64, vec![]), &names()).unwrap().primary, "llama-b12128-bin-ubuntu-arm64.tar.gz");
    }

    #[test]
    fn never_selects_the_runtime_ui_or_framework_archives_as_the_server() {
        for os in [Os::Windows, Os::MacOs, Os::Linux] {
            for arch in [Arch::X64, Arch::Arm64] {
                if let Some(c) = choose(&hw(os, arch, vec![other(Vendor::Amd)]), &names()) {
                    assert!(c.primary.starts_with("llama-b"), "{:?}", c);
                    assert!(!c.primary.contains("xcframework") && !c.primary.contains("-ui"), "{:?}", c);
                }
            }
        }
    }

    #[test]
    fn a_release_without_a_matching_build_yields_nothing() {
        assert!(choose(&hw(Os::Windows, Arch::X64, vec![]), &["llama-b1-bin-macos-arm64.tar.gz".to_string()]).is_none());
        assert!(choose(&hw(Os::Windows, Arch::X64, vec![]), &[]).is_none());
    }

    #[test]
    fn cuda_without_a_runtime_archive_still_works() {
        let n: Vec<String> = names().into_iter().filter(|n| !n.starts_with("cudart")).collect();
        let c = choose(&hw(Os::Windows, Arch::X64, vec![nvidia("621.01", Some(8.6))]), &n).unwrap();
        assert_eq!(c.runtime, None);
    }

    #[test]
    fn parses_cuda_versions() {
        assert_eq!(cuda_version("llama-b1-bin-win-cuda-12.4-x64.zip"), Some((12, 4)));
        assert_eq!(cuda_version("llama-b1-bin-win-cpu-x64.zip"), None);
    }

    #[test]
    fn extracts_zip_and_tar_and_blocks_path_traversal() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("llm-switch-llama-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // A zip with a nested server and an entry that tries to escape the folder.
        let zip_path = dir.join("a.zip");
        {
            let mut w = zip::ZipWriter::new(File::create(&zip_path).unwrap());
            let opts = zip::write::SimpleFileOptions::default();
            w.start_file(format!("build/{SERVER_EXE}"), opts).unwrap();
            w.write_all(b"fake").unwrap();
            w.start_file("../escaped.txt", opts).unwrap();
            w.write_all(b"bad").unwrap();
            w.finish().unwrap();
        }
        let out = dir.join("out");
        extract(&zip_path, &out).unwrap();
        assert_eq!(find_server(&out).unwrap(), out.join("build").join(SERVER_EXE));
        assert!(!dir.join("escaped.txt").exists(), "a path-traversal entry must not be written outside the destination");

        // A tar.gz with the server one level down.
        let tgz = dir.join("a.tar.gz");
        {
            let gz = flate2::write::GzEncoder::new(File::create(&tgz).unwrap(), flate2::Compression::fast());
            let mut b = tar::Builder::new(gz);
            let data = b"fake";
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o755);
            h.set_cksum();
            b.append_data(&mut h, format!("llama-b1/{SERVER_EXE}"), &data[..]).unwrap();
            b.into_inner().unwrap().finish().unwrap();
        }
        let out2 = dir.join("out2");
        extract(&tgz, &out2).unwrap();
        assert_eq!(find_server(&out2).unwrap(), out2.join("llama-b1").join(SERVER_EXE));
        let _ = fs::remove_dir_all(&dir);
    }
}
