use crate::config::{Config, Model, launch_args};
use crate::platform::{self, SERVER_EXE};
use anyhow::{Context, Result, anyhow};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::Command;
use std::time::{Duration, Instant};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

#[derive(Debug, Clone)]
pub struct Running {
    pub pid: u32,
    /// Value of the `-m` argument, if the process command line could be read.
    pub model_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Down,
    Loading,
    Ready,
}

/// sysinfo's constructor is fallible in recent versions; process listing is all we need.
pub fn new_system() -> System {
    System::new().expect("sysinfo init failed")
}

pub fn running(sys: &mut System) -> Option<Running> {
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::OnlyIfNotSet),
    );
    sys.processes().values().find_map(|p| {
        if !p.name().to_string_lossy().eq_ignore_ascii_case(SERVER_EXE) {
            return None;
        }
        let cmd: Vec<String> = p.cmd().iter().map(|s| s.to_string_lossy().to_string()).collect();
        let model_path = cmd.iter().position(|a| a == "-m").and_then(|i| cmd.get(i + 1)).cloned();
        Some(Running { pid: p.pid().as_u32(), model_path })
    })
}

/// Plain HTTP/1.0 request against llama-server's `/health` (200 = ready, 503 = still loading).
pub fn health(cfg: &Config) -> Health {
    let Ok(ip) = cfg.host.parse() else { return Health::Down };
    let addr = SocketAddr::new(ip, cfg.port);
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(300)) else {
        return Health::Down;
    };
    let _ = s.set_read_timeout(Some(Duration::from_millis(800)));
    let _ = s.set_write_timeout(Some(Duration::from_millis(300)));
    if s.write_all(format!("GET /health HTTP/1.0\r\nHost: {}\r\n\r\n", cfg.host).as_bytes()).is_err() {
        return Health::Down;
    }
    let mut buf = [0u8; 64];
    let n = s.read(&mut buf).unwrap_or(0);
    let head = String::from_utf8_lossy(&buf[..n]);
    match head.split_whitespace().nth(1) {
        Some("200") => Health::Ready,
        Some(_) => Health::Loading,
        None => Health::Down,
    }
}

/// POSTs JSON to the server over plain HTTP/1.0 and parses the JSON reply.
pub fn post_json(cfg: &Config, path: &str, body: &serde_json::Value, timeout: Duration) -> Result<serde_json::Value> {
    let ip = cfg.host.parse().context("invalid host in config")?;
    let mut s = TcpStream::connect_timeout(&SocketAddr::new(ip, cfg.port), Duration::from_secs(2))?;
    s.set_read_timeout(Some(timeout))?;
    s.set_write_timeout(Some(Duration::from_secs(10)))?;
    let payload = body.to_string();
    let head = format!(
        "POST {path} HTTP/1.0\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        cfg.host,
        payload.len()
    );
    s.write_all(head.as_bytes())?;
    s.write_all(payload.as_bytes())?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw)?;
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| anyhow!("malformed HTTP reply"))?;
    let status = String::from_utf8_lossy(&raw[..split]).lines().next().unwrap_or("").to_string();
    if !status.contains(" 200") {
        return Err(anyhow!("server replied: {status}"));
    }
    Ok(serde_json::from_slice(&raw[split + 4..])?)
}

/// True while any request slot of the running server is generating or processing a prompt.
pub fn busy(cfg: &Config) -> bool {
    let Ok(ip) = cfg.host.parse() else { return false };
    let Ok(mut s) = TcpStream::connect_timeout(&SocketAddr::new(ip, cfg.port), Duration::from_millis(300)) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    if s.write_all(format!("GET /slots HTTP/1.0\r\nHost: {}\r\n\r\n", cfg.host).as_bytes()).is_err() {
        return false;
    }
    let mut raw = Vec::new();
    let _ = s.read_to_end(&mut raw);
    String::from_utf8_lossy(&raw).contains("\"is_processing\":true")
}

/// Names of other programs holding a connection to the server (e.g. an open OpenCode session).
pub fn other_clients(cfg: &Config) -> Vec<String> {
    let me = std::process::id();
    let pids: Vec<u32> = platform::connected_pids(cfg.port).into_iter().filter(|p| *p != me).collect();
    let mut sys = new_system();
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let mut names: Vec<String> = pids
        .iter()
        .filter_map(|p| sys.process(sysinfo::Pid::from_u32(*p)).map(|pr| pr.name().to_string_lossy().to_string()))
        // lsof also lists the server's own end of each connection.
        .filter(|n| !n.eq_ignore_ascii_case(SERVER_EXE))
        .collect();
    names.sort();
    names.dedup();
    names
}

/// (used MiB, total MiB) of the GPU's dedicated memory; see `platform::gpu_memory`.
pub fn gpu_vram() -> Result<(u32, u32)> {
    platform::gpu_memory()
}

/// Kills every llama-server process and waits until they are gone. Returns how many were stopped.
pub fn stop() -> Result<usize> {
    let mut sys = new_system();
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let mut killed = 0;
    for p in sys.processes().values() {
        if p.name().to_string_lossy().eq_ignore_ascii_case(SERVER_EXE) && p.kill() {
            killed += 1;
        }
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    while running(&mut sys).is_some() {
        if Instant::now() > deadline {
            return Err(anyhow!("llama-server did not exit within 8 s"));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(killed)
}

/// Stops any running server, then launches `model` detached with its configured flags. Returns the PID.
pub fn start(cfg: &Config, model: &Model) -> Result<u32> {
    if !cfg.llama_server.exists() {
        return Err(anyhow!("llama-server not found at {}", cfg.llama_server.display()));
    }
    if !model.path.exists() {
        return Err(anyhow!("model file not found: {}", model.path.display()));
    }
    stop()?;
    if let Some(dir) = cfg.log_file.parent() {
        fs::create_dir_all(dir)?;
    }
    platform::uninherit_std_handles();
    let log = File::create(&cfg.log_file).with_context(|| format!("creating {}", cfg.log_file.display()))?;
    let mut cmd = Command::new(&cfg.llama_server);
    cmd.args(launch_args(cfg, model))
        .current_dir(cfg.llama_server.parent().unwrap_or_else(|| ".".as_ref()))
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    let child = platform::detach(&mut cmd).spawn().context("spawning llama-server")?;
    Ok(child.id())
}

/// Last `n` lines of the server log.
pub fn log_tail(cfg: &Config, n: usize) -> String {
    let Ok(text) = fs::read(&cfg.log_file) else { return String::new() };
    let text = String::from_utf8_lossy(&text);
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// True if the running server was started from `model`.
pub fn is_model(r: &Running, model: &Model) -> bool {
    r.model_path.as_deref().is_some_and(|p| platform::same_path(p, &model.path.to_string_lossy()))
}
