//! HTTP downloads that survive being interrupted: partial files are kept as `<name>.part` and resumed,
//! the size and SHA-256 are checked when they are known, and a finished file only appears under its
//! real name once everything matches.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use ureq::Agent;

const USER_AGENT: &str = concat!("llm-switch/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, Copy, Default)]
pub struct Progress {
    pub done: u64,
    pub total: Option<u64>,
    pub bytes_per_sec: f64,
}

/// What to download and what to check afterwards.
pub struct Job<'a> {
    pub url: &'a str,
    /// Sent only to Hugging Face hosts, never to the CDN a request is redirected to.
    pub token: Option<&'a str>,
    pub size: Option<u64>,
    pub sha256: Option<&'a str>,
}

fn agent() -> Agent {
    let config = Agent::config_builder()
        .user_agent(USER_AGENT)
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .timeout_global(None)
        // Redirects are followed by hand in `open`, so credentials can be withheld from other hosts.
        .max_redirects(0)
        .http_status_as_error(false)
        .build();
    Agent::new_with_config(config)
}

fn host_of(url: &str) -> &str {
    let rest = url.split("://").nth(1).unwrap_or(url);
    rest.split(['/', '?', '#']).next().unwrap_or("").split('@').next_back().unwrap_or("")
}

pub fn is_hf_host(url: &str) -> bool {
    let host = host_of(url).split(':').next().unwrap_or("");
    host == "huggingface.co" || host.ends_with(".huggingface.co") || host == "hf.co"
}

/// Resolves a `Location` header against the URL that produced it.
pub fn resolve_redirect(base: &str, location: &str) -> String {
    if location.starts_with("http://") || location.starts_with("https://") {
        return location.to_string();
    }
    let scheme_end = base.find("://").map_or(0, |i| i + 3);
    let origin_end = base[scheme_end..].find('/').map_or(base.len(), |i| scheme_end + i);
    if location.starts_with('/') {
        format!("{}{}", &base[..origin_end], location)
    } else {
        let dir_end = base.rfind('/').filter(|i| *i >= origin_end).unwrap_or(origin_end);
        format!("{}/{}", &base[..dir_end], location)
    }
}

/// Opens `url`, following redirects, optionally asking for the bytes from `range_from` onwards.
pub fn open(url: &str, token: Option<&str>, range_from: Option<u64>) -> Result<ureq::http::Response<ureq::Body>> {
    let agent = agent();
    let mut current = url.to_string();
    for _ in 0..8 {
        let mut req = agent.get(&current);
        if let Some(t) = token {
            if is_hf_host(&current) {
                req = req.header("Authorization", format!("Bearer {t}"));
            }
        }
        if let Some(from) = range_from {
            req = req.header("Range", format!("bytes={from}-"));
        }
        let resp = req.call().with_context(|| format!("requesting {current}"))?;
        if matches!(resp.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
            let location = resp
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| anyhow!("redirect from {current} had no Location header"))?;
            current = resolve_redirect(&current, location);
            continue;
        }
        return Ok(resp);
    }
    bail!("too many redirects starting from {url}")
}

pub fn get_json(url: &str, token: Option<&str>) -> Result<Value> {
    let resp = open(url, token, None)?;
    let status = resp.status().as_u16();
    if status != 200 {
        bail!("{url} replied with HTTP {status}{}", status_hint(status));
    }
    let text = resp.into_body().read_to_string()?;
    serde_json::from_str(&text).with_context(|| format!("{url} did not return JSON"))
}

fn status_hint(status: u16) -> &'static str {
    match status {
        // Hugging Face answers 401 for repositories that do not exist, so a typo looks like a login problem.
        401 | 403 => {
            " (check the name for typos; if it is a gated or private model, accept its terms on the website and set HF_TOKEN)"
        }
        404 => " (not found: check the link)",
        429 => " (rate limited: try again later)",
        _ => "",
    }
}

/// Parses a `Content-Range: bytes 100-199/200` header into (start, total).
pub fn parse_content_range(v: &str) -> Option<(u64, Option<u64>)> {
    let spec = v.trim().strip_prefix("bytes ")?;
    let (range, total) = spec.split_once('/')?;
    let start = range.split('-').next()?.trim().parse().ok()?;
    Some((start, total.trim().parse().ok()))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Free bytes on the disk that holds `path` (or its nearest existing parent).
pub fn free_space(path: &Path) -> Option<u64> {
    let mut dir = path.to_path_buf();
    while !dir.exists() {
        if !dir.pop() {
            return None;
        }
    }
    let disks = sysinfo::Disks::new_with_refreshed_list().ok()?;
    disks
        .iter()
        .filter(|d| dir.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| d.available_space())
}

fn hash_file(path: &Path, hasher: &mut Sha256) -> Result<()> {
    let mut f = File::open(path)?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        hasher.update(&buf[..n]);
    }
}

/// Downloads `job.url` to `dest`, resuming `<dest>.part` if it exists. Calls `on_progress` about four
/// times a second. If `cancel` is set the transfer stops and the partial file is kept for next time.
pub fn fetch_to_file(job: &Job, dest: &Path, on_progress: &mut dyn FnMut(Progress), cancel: &AtomicBool) -> Result<()> {
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let part = PathBuf::from(format!("{}.part", dest.display()));
    let mut offset = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if job.size.is_some_and(|s| offset > s) {
        let _ = fs::remove_file(&part);
        offset = 0;
    }

    // Ask for the rest of the file; fall back to the whole file if the server cannot resume.
    let mut resume = offset > 0;
    let (resp, total) = loop {
        let resp = open(job.url, job.token, resume.then_some(offset))?;
        let status = resp.status().as_u16();
        let len = resp.body().content_length();
        match status {
            206 if resume => {
                let range = resp.headers().get("content-range").and_then(|v| v.to_str().ok()).and_then(parse_content_range);
                match range {
                    Some((start, total)) if start == offset => break (resp, total.or(len.map(|l| l + offset))),
                    _ => {
                        let _ = fs::remove_file(&part);
                        offset = 0;
                        resume = false;
                    }
                }
            }
            200 => {
                offset = 0;
                break (resp, len);
            }
            416 if resume => {
                let _ = fs::remove_file(&part);
                offset = 0;
                resume = false;
            }
            s => bail!("{} replied with HTTP {s}{}", job.url, status_hint(s)),
        }
    };
    if let (Some(t), Some(s)) = (total, job.size) {
        if t != s {
            bail!("the server reports {t} bytes but {s} were expected");
        }
    }

    let mut hasher = job.sha256.map(|_| Sha256::new());
    let mut file = if offset > 0 {
        if let Some(h) = hasher.as_mut() {
            hash_file(&part, h)?;
        }
        OpenOptions::new().append(true).open(&part)?
    } else {
        File::create(&part).with_context(|| format!("creating {}", part.display()))?
    };

    let mut reader = resp.into_body().into_reader();
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = offset;
    let mut speed = 0.0_f64;
    let mut tick = Instant::now();
    let mut tick_done = done;
    loop {
        if cancel.load(Ordering::Relaxed) {
            file.flush()?;
            bail!("cancelled");
        }
        let n = reader.read(&mut buf).context("the connection was interrupted (run it again to resume)")?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        if let Some(h) = hasher.as_mut() {
            h.update(&buf[..n]);
        }
        done += n as u64;
        if tick.elapsed() >= Duration::from_millis(250) {
            let instant = (done - tick_done) as f64 / tick.elapsed().as_secs_f64();
            speed = if speed == 0.0 { instant } else { 0.7 * speed + 0.3 * instant };
            on_progress(Progress { done, total, bytes_per_sec: speed });
            tick = Instant::now();
            tick_done = done;
        }
    }
    file.flush()?;
    drop(file);
    on_progress(Progress { done, total, bytes_per_sec: speed });

    if let Some(t) = total {
        if done != t {
            bail!("the connection closed early: got {done} of {t} bytes (run it again to resume)");
        }
    }
    if let (Some(expect), Some(h)) = (job.sha256, hasher) {
        let got = hex(&h.finalize());
        if !got.eq_ignore_ascii_case(expect) {
            let _ = fs::remove_file(&part);
            bail!("checksum mismatch: expected {expect}, got {got}. The partial file was deleted");
        }
    }
    fs::rename(&part, dest).with_context(|| format!("moving {} into place", part.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::sync::Arc;

    #[test]
    fn host_parsing_and_token_scope() {
        assert!(is_hf_host("https://huggingface.co/org/repo/resolve/main/a.gguf"));
        assert!(is_hf_host("https://hf.co/org/repo"));
        assert!(!is_hf_host("https://cdn-lfs.example.com/x"));
        assert!(!is_hf_host("https://evil.com/huggingface.co/x"));
        assert!(!is_hf_host("https://huggingface.co.evil.com/x"));
        assert!(!is_hf_host("https://user@evil.com/?huggingface.co"));
    }

    #[test]
    fn resolves_redirects() {
        assert_eq!(resolve_redirect("https://a.com/x/y", "https://b.com/z"), "https://b.com/z");
        assert_eq!(resolve_redirect("https://a.com/x/y", "/z"), "https://a.com/z");
        assert_eq!(resolve_redirect("https://a.com/x/y", "z"), "https://a.com/x/z");
        assert_eq!(resolve_redirect("https://a.com", "z"), "https://a.com/z");
    }

    #[test]
    fn parses_content_range() {
        assert_eq!(parse_content_range("bytes 100-199/200"), Some((100, Some(200))));
        assert_eq!(parse_content_range("bytes 0-9/*"), Some((0, None)));
        assert_eq!(parse_content_range("nonsense"), None);
    }

    /// A tiny HTTP server that serves `body` and understands `Range: bytes=N-`.
    fn serve(body: Arc<Vec<u8>>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let mut reader = BufReader::new(s.try_clone().unwrap());
                let mut range_from = None;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(v) = line.to_lowercase().strip_prefix("range: bytes=") {
                        range_from = v.trim().trim_end_matches('-').parse::<usize>().ok();
                    }
                }
                let _ = match range_from {
                    Some(from) if from < body.len() => {
                        let rest = &body[from..];
                        write!(
                            s,
                            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",
                            rest.len(),
                            from,
                            body.len() - 1,
                            body.len()
                        )
                        .and_then(|_| s.write_all(rest))
                    }
                    Some(_) => write!(s, "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
                    None => write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())
                        .and_then(|_| s.write_all(&body)),
                };
            }
        });
        port
    }

    fn sample() -> Arc<Vec<u8>> {
        Arc::new((0..2_000_000u32).map(|i| (i % 251) as u8).collect())
    }

    fn sha(b: &[u8]) -> String {
        hex(&Sha256::digest(b))
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("llm-switch-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn downloads_and_verifies() {
        let body = sample();
        let port = serve(body.clone());
        let dir = scratch("full");
        let dest = dir.join("m.gguf");
        let url = format!("http://127.0.0.1:{port}/m.gguf");
        let want = sha(&body);
        let job = Job { url: &url, token: None, size: Some(body.len() as u64), sha256: Some(&want) };
        fetch_to_file(&job, &dest, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), *body);
        assert!(!PathBuf::from(format!("{}.part", dest.display())).exists());
    }

    #[test]
    fn resumes_a_partial_file() {
        let body = sample();
        let port = serve(body.clone());
        let dir = scratch("resume");
        let dest = dir.join("m.gguf");
        fs::write(format!("{}.part", dest.display()), &body[..700_000]).unwrap();
        let url = format!("http://127.0.0.1:{port}/m.gguf");
        let want = sha(&body);
        let job = Job { url: &url, token: None, size: Some(body.len() as u64), sha256: Some(&want) };
        let mut first_progress = None;
        fetch_to_file(&job, &dest, &mut |p| { first_progress.get_or_insert(p.done); }, &AtomicBool::new(false)).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), *body, "resumed file must equal the original");
        assert!(first_progress.is_none_or(|d| d >= 700_000), "progress should start from the resume point");
    }

    #[test]
    fn rejects_a_wrong_checksum_and_deletes_the_partial_file() {
        let body = sample();
        let port = serve(body.clone());
        let dir = scratch("badsum");
        let dest = dir.join("m.gguf");
        let url = format!("http://127.0.0.1:{port}/m.gguf");
        let job = Job { url: &url, token: None, size: None, sha256: Some(&"0".repeat(64)) };
        let err = fetch_to_file(&job, &dest, &mut |_| {}, &AtomicBool::new(false)).unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"), "{err}");
        assert!(!dest.exists());
        assert!(!PathBuf::from(format!("{}.part", dest.display())).exists());
    }

    #[test]
    fn rejects_an_unexpected_size() {
        let body = sample();
        let port = serve(body.clone());
        let dir = scratch("badsize");
        let url = format!("http://127.0.0.1:{port}/m.gguf");
        let job = Job { url: &url, token: None, size: Some(123), sha256: None };
        let err = fetch_to_file(&job, &dir.join("m.gguf"), &mut |_| {}, &AtomicBool::new(false)).unwrap_err();
        assert!(err.to_string().contains("expected"), "{err}");
    }

    #[test]
    fn cancel_keeps_the_partial_file() {
        let body = sample();
        let port = serve(body.clone());
        let dir = scratch("cancel");
        let dest = dir.join("m.gguf");
        let url = format!("http://127.0.0.1:{port}/m.gguf");
        let job = Job { url: &url, token: None, size: Some(body.len() as u64), sha256: None };
        let err = fetch_to_file(&job, &dest, &mut |_| {}, &AtomicBool::new(true)).unwrap_err();
        assert!(err.to_string().contains("cancelled"));
        assert!(!dest.exists());
    }

    #[test]
    fn free_space_is_reported_for_an_existing_folder() {
        let dir = scratch("space");
        assert!(free_space(&dir.join("does").join("not").join("exist")).is_some_and(|n| n > 0));
    }
}
