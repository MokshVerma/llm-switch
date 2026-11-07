//! Everything that differs between operating systems lives here, behind `cfg`.
//! The rest of the crate calls these functions and never mentions an OS.
//!
//! Windows is the tested target. The Unix (macOS / Linux) side is written to the same interface
//! but has not been run on real hardware.

use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};
use std::process::Command;

/// File name of llama.cpp's server binary.
pub const SERVER_EXE: &str = if cfg!(windows) { "llama-server.exe" } else { "llama-server" };

pub fn home_dir() -> PathBuf {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// Where llm-switch keeps its config file.
pub fn config_dir() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")).join("llm-switch")
    }
    #[cfg(target_os = "macos")]
    {
        home_dir().join("Library").join("Application Support").join("llm-switch")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".config"))
            .join("llm-switch")
    }
}

/// OpenCode reads `~/.config/opencode/opencode.json` on every OS.
pub fn opencode_config() -> PathBuf {
    home_dir().join(".config").join("opencode").join("opencode.json")
}

/// First-run defaults for the folders the user is most likely to have.
pub struct DefaultPaths {
    pub llama_server: PathBuf,
    pub models_dir: PathBuf,
    pub log_file: PathBuf,
}

pub fn default_paths() -> DefaultPaths {
    #[cfg(windows)]
    {
        DefaultPaths {
            llama_server: PathBuf::from(r"D:\llama.cpp\llama-server.exe"),
            models_dir: PathBuf::from(r"D:\models"),
            log_file: PathBuf::from(r"D:\llama.cpp\server.log"),
        }
    }
    #[cfg(target_os = "macos")]
    {
        DefaultPaths {
            // Homebrew's `brew install llama.cpp` location on Apple Silicon.
            llama_server: PathBuf::from("/opt/homebrew/bin/llama-server"),
            models_dir: home_dir().join("models"),
            log_file: home_dir().join("Library").join("Logs").join("llm-switch").join("server.log"),
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        DefaultPaths {
            llama_server: home_dir().join("llama.cpp").join("build").join("bin").join("llama-server"),
            models_dir: home_dir().join("models"),
            log_file: config_dir().join("server.log"),
        }
    }
}

/// Windows paths are case-insensitive; elsewhere they are not.
pub fn same_path(a: &str, b: &str) -> bool {
    if cfg!(windows) { a.eq_ignore_ascii_case(b) } else { a == b }
}

/// Keeps a child process from flashing a console window (no-op outside Windows).
pub fn hide_console(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000) // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    {
        cmd
    }
}

/// Makes the child outlive this program and ignore signals sent to our terminal.
pub fn detach(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000 | 0x0000_0200) // CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0)
    }
}

/// Windows hands every inheritable handle to a child process, including our own stdout pipe.
/// Without this, whoever is reading this program's output (a shell, a script) would wait until the
/// long-running server exits, because the server would still hold the pipe open.
/// On Unix, Rust opens descriptors close-on-exec, so there is nothing to do.
pub fn uninherit_std_handles() {
    #[cfg(windows)]
    {
        unsafe extern "system" {
            fn GetStdHandle(id: u32) -> isize;
            fn SetHandleInformation(handle: isize, mask: u32, flags: u32) -> i32;
        }
        const HANDLE_FLAG_INHERIT: u32 = 1;
        for id in [-10i32, -11, -12] {
            // STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE
            unsafe {
                let h = GetStdHandle(id as u32);
                if h != 0 && h != -1 {
                    SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
                }
            }
        }
    }
}

/// Opens a text file in the system's editor.
pub fn open_in_editor(path: &Path) {
    #[cfg(windows)]
    let _ = Command::new("notepad").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg("-t").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = Command::new("xdg-open").arg(path).spawn();
}

/// Shows an error to the user. The Windows release build has no console, so it uses a message box;
/// macOS uses an AppleScript dialog; everywhere else it goes to stderr.
pub fn show_error(text: &str) {
    #[cfg(windows)]
    unsafe {
        unsafe extern "system" {
            fn MessageBoxW(hwnd: isize, text: *const u16, caption: *const u16, flags: u32) -> i32;
        }
        let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        MessageBoxW(0, wide(text).as_ptr(), wide("LLM Switch").as_ptr(), 0x10);
    }
    #[cfg(target_os = "macos")]
    {
        let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
        let _ = Command::new("osascript")
            .args(["-e", &format!("display dialog \"{escaped}\" with title \"LLM Switch\" buttons {{\"OK\"}} with icon stop")])
            .output();
    }
    eprintln!("{text}");
}

/// PIDs of processes that have an established TCP connection involving `port` (our own is excluded
/// by the caller). Windows reads `netstat`; Unix reads `lsof`.
pub fn connected_pids(port: u16) -> Vec<u32> {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("netstat");
        let Ok(out) = hide_console(cmd.args(["-ano", "-p", "tcp"])).output() else { return vec![] };
        let suffix = format!(":{port}");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| {
                let c: Vec<&str> = l.split_whitespace().collect();
                (c.len() >= 5 && c[2].ends_with(&suffix) && c[3] == "ESTABLISHED").then(|| c[4].parse().ok()).flatten()
            })
            .collect()
    }
    #[cfg(unix)]
    {
        // -F p prints one "p<pid>" line per process.
        let Ok(out) = Command::new("lsof")
            .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:ESTABLISHED", "-Fp"])
            .output()
        else {
            return vec![];
        };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.strip_prefix('p').and_then(|p| p.parse().ok()))
            .collect()
    }
}

/// (used MiB, total MiB) of the GPU's dedicated memory.
///
/// Windows and Linux ask `nvidia-smi`. Apple Silicon has one pool of unified memory shared by the
/// CPU and GPU, so "VRAM headroom" and "experts on the CPU" do not mean the same thing there, and
/// the tuner is not available yet.
pub fn gpu_memory() -> Result<(u32, u32)> {
    #[cfg(target_os = "macos")]
    {
        Err(anyhow!(
            "auto-tune is not available on macOS yet: Apple Silicon shares one memory pool between CPU and GPU, \
             so the VRAM-based search does not apply"
        ))
    }
    #[cfg(not(target_os = "macos"))]
    {
        use anyhow::Context;
        let mut cmd = Command::new("nvidia-smi");
        let out = hide_console(cmd.args(["--query-gpu=memory.used,memory.total", "--format=csv,noheader,nounits"]))
            .output()
            .context("running nvidia-smi (is an NVIDIA driver installed?)")?;
        let text = String::from_utf8_lossy(&out.stdout);
        let line = text.lines().next().unwrap_or("");
        let mut it = line.split(',').map(|s| s.trim().parse::<u32>());
        match (it.next(), it.next()) {
            (Some(Ok(used)), Some(Ok(total))) => Ok((used, total)),
            _ => Err(anyhow!("could not parse nvidia-smi output: '{line}'")),
        }
    }
}
