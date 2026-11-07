use crate::platform;
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Resolved launch settings for one model (defaults merged with per-model overrides).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub ctx: u32,
    pub ngl: u32,
    pub n_cpu_moe: Option<u32>,
    pub flash_attn: bool,
    pub cache_type_k: Option<String>,
    pub cache_type_v: Option<String>,
    pub jinja: bool,
    pub extra_args: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            ctx: 131072,
            ngl: 99,
            n_cpu_moe: Some(28),
            flash_attn: true,
            cache_type_k: Some("q8_0".into()),
            cache_type_v: Some("q8_0".into()),
            jinja: true,
            extra_args: vec![],
        }
    }
}

/// Per-model overrides; anything left out falls back to `[defaults]`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Overrides {
    pub ctx: Option<u32>,
    pub ngl: Option<u32>,
    pub n_cpu_moe: Option<u32>,
    pub flash_attn: Option<bool>,
    pub cache_type_k: Option<String>,
    pub cache_type_v: Option<String>,
    pub jinja: Option<bool>,
    pub extra_args: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCfg {
    /// GGUF file name inside `models_dir`.
    pub file: String,
    /// Display name (also used as the OpenCode model name).
    pub name: Option<String>,
    /// Model key under `provider.<id>.models` in opencode.json.
    pub opencode_id: Option<String>,
    #[serde(flatten)]
    pub overrides: Overrides,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenCodeCfg {
    pub config_path: PathBuf,
    pub provider_id: String,
    /// Write the model's context/limits into opencode.json every time it is started.
    pub auto_sync: bool,
    pub output_limit: u64,
    pub compaction_buffer: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub llama_server: PathBuf,
    pub models_dir: PathBuf,
    pub host: String,
    pub port: u16,
    pub log_file: PathBuf,
    pub opencode: OpenCodeCfg,
    #[serde(default)]
    pub defaults: Settings,
    #[serde(default, rename = "model")]
    pub models: Vec<ModelCfg>,
}

impl Default for Config {
    fn default() -> Self {
        let paths = platform::default_paths();
        Self {
            llama_server: paths.llama_server,
            models_dir: paths.models_dir,
            host: "127.0.0.1".into(),
            port: 8080,
            log_file: paths.log_file,
            opencode: OpenCodeCfg {
                config_path: platform::opencode_config(),
                provider_id: "llamacpp".into(),
                auto_sync: true,
                output_limit: 8192,
                compaction_buffer: Some(10000),
            },
            defaults: Settings::default(),
            models: vec![ModelCfg {
                file: "Qwen3.5-35B-A3B-heretic-Q4_K_M.gguf".into(),
                name: Some("Qwen3.5 35B-A3B heretic".into()),
                opencode_id: Some("qwen35-heretic".into()),
                overrides: Overrides::default(),
            }],
        }
    }
}

/// A model found in `models_dir`, with its resolved settings.
#[derive(Debug, Clone)]
pub struct Model {
    pub file: String,
    pub path: PathBuf,
    pub size_gb: f64,
    pub name: String,
    pub opencode_id: String,
    pub configured: bool,
    pub settings: Settings,
}

impl Config {
    pub fn path() -> PathBuf {
        if let Some(p) = std::env::var_os("LLM_SWITCH_CONFIG") {
            return p.into();
        }
        platform::config_dir().join("config.toml")
    }

    /// Loads the config, writing a default one on first run.
    pub fn load() -> Result<Self> {
        let path = Self::path();
        if !path.exists() {
            let cfg = Config::default();
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir)?;
            }
            fs::write(&path, toml::to_string_pretty(&cfg)?)?;
            return Ok(cfg);
        }
        let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&path, toml::to_string_pretty(self)?).with_context(|| format!("writing {}", path.display()))
    }

    /// Stores tuned values as per-model overrides (creating the model's block if needed) and saves.
    pub fn apply_tuning(&mut self, file: &str, ctx: u32, moe: bool, knob: u32) -> Result<()> {
        let idx = match self.models.iter().position(|m| m.file.eq_ignore_ascii_case(file)) {
            Some(i) => i,
            None => {
                self.models.push(ModelCfg { file: file.into(), name: None, opencode_id: None, overrides: Overrides::default() });
                self.models.len() - 1
            }
        };
        let o = &mut self.models[idx].overrides;
        o.ctx = Some(ctx);
        if moe {
            o.n_cpu_moe = Some(knob);
        } else {
            o.ngl = Some(knob);
        }
        self.save()
    }

    pub fn base_url(&self) -> String {
        format!("http://{}:{}/v1", self.host, self.port)
    }

    fn resolve(&self, o: &Overrides) -> Settings {
        let d = &self.defaults;
        Settings {
            ctx: o.ctx.unwrap_or(d.ctx),
            ngl: o.ngl.unwrap_or(d.ngl),
            n_cpu_moe: o.n_cpu_moe.or(d.n_cpu_moe),
            flash_attn: o.flash_attn.unwrap_or(d.flash_attn),
            cache_type_k: o.cache_type_k.clone().or_else(|| d.cache_type_k.clone()),
            cache_type_v: o.cache_type_v.clone().or_else(|| d.cache_type_v.clone()),
            jinja: o.jinja.unwrap_or(d.jinja),
            extra_args: o.extra_args.clone().unwrap_or_else(|| d.extra_args.clone()),
        }
    }

    /// Scans `models_dir` for GGUF files. Split shards (`-0000N-of-`) other than the first are skipped,
    /// as are multimodal projector and MTP helper files.
    pub fn scan_models(&self) -> Vec<Model> {
        let Ok(rd) = fs::read_dir(&self.models_dir) else { return vec![] };
        let mut out: Vec<Model> = rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let path = e.path();
                let file = path.file_name()?.to_string_lossy().to_string();
                let lower = file.to_lowercase();
                if !lower.ends_with(".gguf") || lower.starts_with("mmproj") || lower.starts_with("mtp-") {
                    return None;
                }
                if lower.contains("-of-") && !lower.contains("-00001-of-") {
                    return None;
                }
                let size_gb = e.metadata().ok()?.len() as f64 / 1_073_741_824.0;
                let cfg = self.models.iter().find(|m| m.file.eq_ignore_ascii_case(&file));
                let stem = file.trim_end_matches(".gguf").to_string();
                Some(Model {
                    size_gb,
                    name: cfg.and_then(|c| c.name.clone()).unwrap_or_else(|| stem.clone()),
                    opencode_id: cfg.and_then(|c| c.opencode_id.clone()).unwrap_or_else(|| slug(&stem)),
                    configured: cfg.is_some(),
                    settings: self.resolve(&cfg.map(|c| c.overrides.clone()).unwrap_or_default()),
                    file,
                    path,
                })
            })
            .collect();
        out.sort_by(|a, b| a.file.to_lowercase().cmp(&b.file.to_lowercase()));
        out
    }

    /// Finds a model by (partial) file name; with no query, the first model found.
    pub fn find_model(&self, query: Option<&str>) -> Result<Model> {
        let models = self.scan_models();
        if models.is_empty() {
            return Err(anyhow!("no .gguf models found in {}", self.models_dir.display()));
        }
        match query {
            None => Ok(models.into_iter().next().unwrap()),
            Some(q) => {
                let q = q.to_lowercase();
                models
                    .into_iter()
                    .find(|m| m.file.to_lowercase().contains(&q) || m.name.to_lowercase().contains(&q))
                    .ok_or_else(|| anyhow!("no model matching '{q}' in {}", self.models_dir.display()))
            }
        }
    }
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

pub fn launch_args(cfg: &Config, model: &Model) -> Vec<String> {
    let s = &model.settings;
    let mut a: Vec<String> = vec![
        "-m".into(),
        model.path.to_string_lossy().to_string(),
        "-c".into(),
        s.ctx.to_string(),
        "-ngl".into(),
        s.ngl.to_string(),
    ];
    if let Some(n) = s.n_cpu_moe {
        a.push("--n-cpu-moe".into());
        a.push(n.to_string());
    }
    if s.flash_attn {
        a.push("-fa".into());
        a.push("on".into());
    }
    if let Some(k) = &s.cache_type_k {
        a.push("-ctk".into());
        a.push(k.clone());
    }
    if let Some(v) = &s.cache_type_v {
        a.push("-ctv".into());
        a.push(v.clone());
    }
    if s.jinja {
        a.push("--jinja".into());
    }
    a.push("--host".into());
    a.push(cfg.host.clone());
    a.push("--port".into());
    a.push(cfg.port.to_string());
    a.extend(s.extra_args.iter().cloned());
    a
}
