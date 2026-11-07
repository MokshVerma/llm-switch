//! Turning a link into a model file on disk: Hugging Face repos and files, or any direct `.gguf` URL.

use crate::config::Config;
use crate::download::{self, Job, Progress};
use anyhow::{Context, Result, anyhow, bail};
use std::fs::{self, File};
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

/// What a pasted link points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// A repository: list its `.gguf` files and let the user pick.
    Repo { repo: String, rev: String },
    /// One file, ready to download.
    File { url: String, name: String },
}

/// One file of a model. Large models are split into several of these ("shards").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub name: String,
    pub url: String,
    pub size: Option<u64>,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteModel {
    pub title: String,
    pub parts: Vec<Part>,
}

impl RemoteModel {
    /// Total size, when every part's size is known.
    pub fn total(&self) -> Option<u64> {
        self.parts.iter().map(|p| p.size).sum()
    }
}

/// `HF_TOKEN` for gated or private repositories. Read from the environment only; never stored.
pub fn hf_token() -> Option<String> {
    ["HF_TOKEN", "HUGGING_FACE_HUB_TOKEN"]
        .iter()
        .find_map(|k| std::env::var(k).ok())
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        // A '%' only starts an escape when two more bytes follow it.
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn percent_encode_path(s: &str) -> String {
    s.bytes()
        .map(|c| match c {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => (c as char).to_string(),
            _ => format!("%{c:02X}"),
        })
        .collect()
}

/// Reduces a name from a link to a safe file name inside the models folder.
pub fn safe_file_name(raw: &str) -> Result<String> {
    let decoded = percent_decode(raw.rsplit('/').next().unwrap_or(raw));
    let name = decoded.trim();
    if name.is_empty() || name == "." || name == ".." {
        bail!("the link does not name a file");
    }
    if name.contains(['/', '\\', ':']) || name.chars().any(|c| c.is_control()) {
        bail!("'{name}' is not a safe file name");
    }
    if !name.to_lowercase().ends_with(".gguf") {
        bail!("'{name}' is not a .gguf file (llama.cpp models must be in GGUF format)");
    }
    if name.len() > 200 {
        bail!("the file name is too long");
    }
    Ok(name.to_string())
}

pub fn parse_link(input: &str) -> Result<Link> {
    let s = input.trim();
    if s.is_empty() {
        bail!("paste a Hugging Face link, or a direct link to a .gguf file");
    }
    if s.contains(char::is_whitespace) {
        bail!("that does not look like a link");
    }

    // Shorthand: `owner/repo`. Repository names often contain dots (Qwen3.5-..., Llama-3.1-...), so the
    // test for "this is a host name, not an owner" looks at the owner part only.
    if !s.contains("://") && s.matches('/').count() == 1 {
        let (owner, name) = s.split_once('/').unwrap_or(("", ""));
        if !owner.is_empty() && !name.is_empty() && !owner.contains('.') && !name.to_lowercase().ends_with(".gguf") {
            return Ok(Link::Repo { repo: s.to_string(), rev: "main".into() });
        }
    }

    let url = if s.contains("://") { s.to_string() } else { format!("https://{s}") };
    let (scheme, rest) = url.split_once("://").ok_or_else(|| anyhow!("not a link"))?;
    if scheme != "https" && scheme != "http" {
        bail!("only http(s) links are supported");
    }
    let rest = rest.split('#').next().unwrap_or("");
    let (host_and_path, _query) = rest.split_once('?').unwrap_or((rest, ""));
    let (host, path) = host_and_path.split_once('/').unwrap_or((host_and_path, ""));
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    let host_lc = host.to_lowercase();
    if matches!(host_lc.as_str(), "huggingface.co" | "www.huggingface.co" | "hf.co") {
        if segs.len() < 2 {
            bail!("that Hugging Face link has no repository in it");
        }
        let repo = format!("{}/{}", segs[0], segs[1]);
        return match segs.get(2).copied() {
            None => Ok(Link::Repo { repo, rev: "main".into() }),
            Some("tree") => Ok(Link::Repo { repo, rev: segs.get(3).unwrap_or(&"main").to_string() }),
            Some("blob") | Some("resolve") if segs.len() >= 5 => {
                let rev = segs[3];
                let file = segs[4..].join("/");
                let name = safe_file_name(&file)?;
                Ok(Link::File { url: format!("https://huggingface.co/{repo}/resolve/{rev}/{file}"), name })
            }
            Some(_) => Ok(Link::Repo { repo, rev: "main".into() }),
        };
    }

    // Any other site must point straight at a .gguf file.
    let name = safe_file_name(path)?;
    let clean = url.split('#').next().unwrap_or(&url).to_string();
    Ok(Link::File { url: clean, name })
}

/// `name-00001-of-00003.gguf` -> ("name", 1, 3)
fn shard_info(name: &str) -> Option<(String, u32, u32)> {
    let stem = name.strip_suffix(".gguf").or_else(|| name.strip_suffix(".GGUF"))?;
    let (left, total) = stem.rsplit_once("-of-")?;
    let (base, idx) = left.rsplit_once('-')?;
    let ok = |s: &str| s.len() == 5 && s.bytes().all(|b| b.is_ascii_digit());
    if !ok(idx) || !ok(total) {
        return None;
    }
    Some((base.to_string(), idx.parse().ok()?, total.parse().ok()?))
}

/// Groups split models into one entry so they are downloaded together.
pub fn group_parts(mut parts: Vec<Part>) -> Vec<RemoteModel> {
    parts.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out: Vec<RemoteModel> = Vec::new();
    let mut keys: Vec<Option<(String, u32)>> = Vec::new(); // (base, total) per output entry
    for part in parts {
        match shard_info(&part.name) {
            Some((base, _, total)) => {
                let key = Some((base, total));
                if let Some(i) = keys.iter().position(|k| *k == key) {
                    out[i].parts.push(part);
                } else {
                    keys.push(key);
                    out.push(RemoteModel { title: part.name.clone(), parts: vec![part] });
                }
            }
            None => {
                keys.push(None);
                out.push(RemoteModel { title: part.name.clone(), parts: vec![part] });
            }
        }
    }
    out
}

/// Resolves a link into downloadable models (one entry for a file link, several for a repository).
pub fn lookup(link: &str, token: Option<&str>) -> Result<Vec<RemoteModel>> {
    match parse_link(link)? {
        Link::File { url, name } => {
            let resp = download::open(&url, token, None)?;
            let status = resp.status().as_u16();
            if status != 200 {
                bail!("{url} replied with HTTP {status}");
            }
            let size = resp.body().content_length();
            Ok(vec![RemoteModel { title: name.clone(), parts: vec![Part { name, url, size, sha256: None }] }])
        }
        Link::Repo { repo, rev } => {
            let api = format!("https://huggingface.co/api/models/{repo}/revision/{rev}?blobs=true");
            let json = download::get_json(&api, token)?;
            let siblings = json["siblings"].as_array().ok_or_else(|| anyhow!("{repo} has no file list"))?;
            let parts: Vec<Part> = siblings
                .iter()
                .filter_map(|s| {
                    let path = s["rfilename"].as_str()?;
                    let lower = path.to_lowercase();
                    if !lower.ends_with(".gguf") || lower.contains("mmproj") {
                        return None;
                    }
                    Some(Part {
                        name: path.rsplit('/').next()?.to_string(),
                        url: format!("https://huggingface.co/{repo}/resolve/{rev}/{}", percent_encode_path(path)),
                        size: s["size"].as_u64().or_else(|| s["lfs"]["size"].as_u64()),
                        sha256: s["lfs"]["sha256"].as_str().map(String::from),
                    })
                })
                .collect();
            if parts.is_empty() {
                bail!("{repo} has no .gguf files. Look for a repository of the same model with \"GGUF\" in its name");
            }
            Ok(group_parts(parts))
        }
    }
}

/// True when the file starts with the GGUF magic number.
pub fn is_gguf(path: &std::path::Path) -> bool {
    let mut magic = [0u8; 4];
    File::open(path).and_then(|mut f| f.read_exact(&mut magic)).is_ok() && &magic == b"GGUF"
}

/// Downloads every part of `model` into the models folder. `on_progress` gets the file name and the
/// progress of the whole model. Returns the paths of the files, in order.
pub fn download_model(
    cfg: &Config,
    model: &RemoteModel,
    token: Option<&str>,
    on_progress: &mut dyn FnMut(&str, Progress),
    cancel: &AtomicBool,
) -> Result<Vec<PathBuf>> {
    let dir = &cfg.models_dir;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;

    let total = model.total();
    if let (Some(total), Some(free)) = (total, download::free_space(dir)) {
        let existing: u64 = model
            .parts
            .iter()
            .filter_map(|p| safe_file_name(&p.name).ok())
            .filter_map(|n| fs::metadata(dir.join(format!("{n}.part"))).ok())
            .map(|m| m.len())
            .sum();
        let needed = total.saturating_sub(existing) + (1 << 30);
        if free < needed {
            bail!(
                "not enough free space on the drive for {}: {:.1} GB needed (including a 1 GB margin), {:.1} GB free",
                dir.display(),
                needed as f64 / 1e9,
                free as f64 / 1e9
            );
        }
    }

    let mut paths = Vec::new();
    let mut base = 0u64;
    for part in &model.parts {
        let name = safe_file_name(&part.name)?;
        let dest = dir.join(&name);
        let already = part.size.is_some_and(|s| fs::metadata(&dest).is_ok_and(|m| m.len() == s)) && is_gguf(&dest);
        if !already {
            let job = Job { url: &part.url, token, size: part.size, sha256: part.sha256.as_deref() };
            download::fetch_to_file(
                &job,
                &dest,
                &mut |p| on_progress(&name, Progress { done: base + p.done, total, bytes_per_sec: p.bytes_per_sec }),
                cancel,
            )?;
            if !is_gguf(&dest) {
                let _ = fs::remove_file(&dest);
                bail!(
                    "{name} downloaded, but it is not a GGUF file. The link may need a login or may point to a web page \
                     instead of the file"
                );
            }
        }
        base += part.size.unwrap_or(0);
        on_progress(&name, Progress { done: base, total, bytes_per_sec: 0.0 });
        paths.push(dest);
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(r: &str, rev: &str) -> Link {
        Link::Repo { repo: r.into(), rev: rev.into() }
    }

    #[test]
    fn parses_hugging_face_repo_links() {
        assert_eq!(parse_link("https://huggingface.co/llmfan46/Qwen3.5-35B-A3B-heretic-GGUF").unwrap(), repo("llmfan46/Qwen3.5-35B-A3B-heretic-GGUF", "main"));
        assert_eq!(parse_link("https://huggingface.co/a/b/tree/dev").unwrap(), repo("a/b", "dev"));
        assert_eq!(parse_link("huggingface.co/a/b").unwrap(), repo("a/b", "main"));
        assert_eq!(parse_link("https://hf.co/a/b?show_file_info=x").unwrap(), repo("a/b", "main"));
        assert_eq!(parse_link("  a/b  ").unwrap(), repo("a/b", "main"));
        // Dots in the repository name are normal and must not be mistaken for a host name.
        assert_eq!(parse_link("llmfan46/Qwen3.5-35B-A3B-heretic-GGUF").unwrap(), repo("llmfan46/Qwen3.5-35B-A3B-heretic-GGUF", "main"));
        assert_eq!(parse_link("meta/Llama-3.1-8B-GGUF").unwrap(), repo("meta/Llama-3.1-8B-GGUF", "main"));
        // ...but a host name with a path is not shorthand.
        assert!(parse_link("example.com/model.gguf").is_ok_and(|l| matches!(l, Link::File { .. })));
        assert!(parse_link("example.com/readme").is_err());
        assert_eq!(parse_link("https://huggingface.co/a/b/discussions").unwrap(), repo("a/b", "main"));
    }

    #[test]
    fn turns_blob_pages_into_download_links() {
        let want = Link::File { url: "https://huggingface.co/a/b/resolve/main/m-Q4_K_M.gguf".into(), name: "m-Q4_K_M.gguf".into() };
        assert_eq!(parse_link("https://huggingface.co/a/b/blob/main/m-Q4_K_M.gguf").unwrap(), want);
        assert_eq!(parse_link("https://huggingface.co/a/b/resolve/main/m-Q4_K_M.gguf?download=true").unwrap(), want);
    }

    #[test]
    fn accepts_direct_gguf_links_on_any_host() {
        assert_eq!(
            parse_link("https://example.com/files/model.GGUF?token=1").unwrap(),
            Link::File { url: "https://example.com/files/model.GGUF?token=1".into(), name: "model.GGUF".into() }
        );
    }

    #[test]
    fn rejects_bad_input_with_a_useful_message() {
        assert!(parse_link("").is_err());
        assert!(parse_link("two words").is_err());
        assert!(parse_link("ftp://example.com/m.gguf").is_err());
        assert!(parse_link("file:///c:/m.gguf").is_err());
        let e = parse_link("https://example.com/model.safetensors").unwrap_err().to_string();
        assert!(e.contains("GGUF"), "{e}");
        assert!(parse_link("https://huggingface.co/onlyowner").is_err());
        assert!(parse_link("https://example.com/").is_err());
    }

    #[test]
    fn file_names_cannot_escape_the_models_folder() {
        assert_eq!(safe_file_name("a/b/model.gguf").unwrap(), "model.gguf");
        assert_eq!(safe_file_name("my%20model.gguf").unwrap(), "my model.gguf");
        for bad in ["..%2F..%2Fevil.gguf", "..%5Cevil.gguf", "C:evil.gguf", "..", "", "a.txt", "nul\u{0}.gguf", "x%3Ay.gguf"] {
            assert!(safe_file_name(bad).is_err(), "{bad:?} should be rejected");
        }
        assert!(safe_file_name(&format!("{}.gguf", "a".repeat(300))).is_err());
    }

    fn part(name: &str, size: u64) -> Part {
        Part { name: name.into(), url: format!("https://x/{name}"), size: Some(size), sha256: None }
    }

    #[test]
    fn groups_split_models_and_keeps_singles_separate() {
        let models = group_parts(vec![
            part("Big-Q4-00002-of-00003.gguf", 20),
            part("Small-Q4.gguf", 5),
            part("Big-Q4-00001-of-00003.gguf", 20),
            part("Big-Q4-00003-of-00003.gguf", 10),
            part("Big-Q8-00001-of-00002.gguf", 40),
            part("Big-Q8-00002-of-00002.gguf", 40),
        ]);
        assert_eq!(models.len(), 3);
        let q4 = models.iter().find(|m| m.title.starts_with("Big-Q4")).unwrap();
        assert_eq!(q4.parts.len(), 3);
        assert_eq!(q4.title, "Big-Q4-00001-of-00003.gguf", "the title is the first shard");
        assert_eq!(q4.total(), Some(50));
        assert_eq!(models.iter().find(|m| m.title == "Small-Q4.gguf").unwrap().parts.len(), 1);
        assert_eq!(models.iter().find(|m| m.title.starts_with("Big-Q8")).unwrap().total(), Some(80));
    }

    #[test]
    fn total_is_unknown_when_any_part_size_is_unknown() {
        let m = RemoteModel { title: "t".into(), parts: vec![part("a.gguf", 1), Part { size: None, ..part("b.gguf", 0) }] };
        assert_eq!(m.total(), None);
    }

    #[test]
    fn percent_encoding_round_trips_awkward_names() {
        assert_eq!(percent_encode_path("dir/my model #1.gguf"), "dir/my%20model%20%231.gguf");
        assert_eq!(percent_decode("a%20b%2"), "a b%2");
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[test]
    fn checks_the_gguf_magic_number() {
        let dir = std::env::temp_dir().join(format!("llm-switch-gguf-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let good = dir.join("g.gguf");
        let html = dir.join("h.gguf");
        fs::write(&good, b"GGUF\x03\x00\x00\x00rest").unwrap();
        fs::write(&html, b"<!DOCTYPE html><html>login</html>").unwrap();
        assert!(is_gguf(&good));
        assert!(!is_gguf(&html));
        assert!(!is_gguf(&dir.join("missing.gguf")));
        let _ = fs::remove_dir_all(&dir);
    }
}
