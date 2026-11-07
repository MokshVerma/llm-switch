//! Auto-tuner: restarts the server with trial settings, measures real VRAM use and speed, and
//! searches for the most GPU-heavy configuration that still leaves headroom, per context size.
//!
//! MoE models: the knob is `--n-cpu-moe` (experts of the first N layers stay in RAM). VRAM use only
//! goes down as N goes up, so the tuner binary-searches for the smallest N that fits.
//! Dense models: the knob is `-ngl` (layers on the GPU); it binary-searches for the largest that fits.

use crate::config::{Config, Model};
use crate::{gguf, server};
use anyhow::{Result, anyhow};
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const DEFAULT_CONTEXTS: [u32; 4] = [32768, 65536, 131072, 262144];
pub const DEFAULT_HEADROOM_MB: u32 = 700;
/// A bigger context is preferred as long as decode speed stays within this fraction of the best.
const SPEED_TOLERANCE: f64 = 0.75;

#[derive(Debug, Clone)]
pub struct Options {
    pub contexts: Vec<u32>,
    pub headroom_mb: u32,
}

#[derive(Debug, Clone)]
pub struct Trial {
    pub ctx: u32,
    /// `--n-cpu-moe` for MoE models, `-ngl` for dense ones.
    pub knob: u32,
    pub vram_mb: u32,
    pub decode_tps: f64,
    pub prefill_tps: f64,
}

#[derive(Debug, Clone)]
pub struct Tuning {
    pub file: String,
    pub moe: bool,
    pub layers: u32,
    pub total_mb: u32,
    pub limit_mb: u32,
    pub trials: Vec<Trial>,
    pub skipped: Vec<(u32, String)>,
    pub recommended: Option<Trial>,
}

#[derive(Clone)]
struct Probe {
    fits: bool,
    vram_mb: u32,
}

struct Tuner<'a> {
    cfg: &'a Config,
    model: &'a Model,
    moe: bool,
    limit_mb: u32,
    /// VRAM in use by other programs before any trial started.
    baseline_mb: u32,
    log: &'a dyn Fn(&str),
    cancel: &'a AtomicBool,
    cache: HashMap<(u32, u32), Probe>,
    current: Option<(u32, u32)>,
}

/// Runs the whole tuning session. Leaves the server stopped; the caller decides what to restore.
pub fn run(cfg: &Config, model: &Model, opts: &Options, log: &dyn Fn(&str), cancel: &AtomicBool) -> Result<Tuning> {
    let info = gguf::read_info(&model.path)?;
    let layers = info.block_count.ok_or_else(|| anyhow!("could not read the layer count from {}", model.file))?;
    let moe = info.is_moe();
    log(&format!(
        "{}: {} layers, {}, trained context {}",
        model.file,
        layers,
        if moe { format!("MoE with {} experts (tuning --n-cpu-moe)", info.expert_count.unwrap_or(0)) } else { "dense (tuning -ngl)".into() },
        info.context_length.map_or("unknown".to_string(), |c| c.to_string())
    ));

    let mut sys = server::new_system();
    if server::running(&mut sys).is_some() {
        if server::busy(cfg) {
            return Err(anyhow!(
                "the server is busy answering a request right now (is OpenCode working?). Wait until it is idle and try again"
            ));
        }
        let clients = server::other_clients(cfg);
        if !clients.is_empty() {
            log(&format!(
                "warning: {} is connected to the server. If it sends a request while tuning, the numbers will be off; keep it idle.",
                clients.join(", ")
            ));
        }
    }
    log("stopping the running server to get a clean VRAM baseline...");
    server::stop()?;
    std::thread::sleep(Duration::from_secs(2));
    let (base_used, total_mb) = server::gpu_vram()?;
    let limit_mb = total_mb.saturating_sub(opts.headroom_mb);
    log(&format!(
        "GPU: {total_mb} MiB total, {base_used} MiB used by other programs; trials must stay under {limit_mb} MiB ({} MiB headroom)",
        opts.headroom_mb
    ));

    let mut t = Tuner { cfg, model, moe, limit_mb, baseline_mb: base_used, log, cancel, cache: HashMap::new(), current: None };
    let mut result = Tuning {
        file: model.file.clone(),
        moe,
        layers,
        total_mb,
        limit_mb,
        trials: vec![],
        skipped: vec![],
        recommended: None,
    };

    // Knob range: MoE -> [0, layers] (higher = more experts on the CPU); dense -> [0, layers + 1].
    let top = if moe { layers } else { layers + 1 };
    let mut bound = if moe { 0 } else { top }; // MoE: lowest N worth trying; dense: highest ngl worth trying
    let mut contexts = opts.contexts.clone();
    contexts.sort_unstable();
    for ctx in contexts {
        if info.context_length.is_some_and(|max| ctx > max) {
            result.skipped.push((ctx, "above the model's trained context".into()));
            continue;
        }
        log(&format!("--- context {ctx} ---"));
        let found = if moe { t.search_moe(ctx, bound, top)? } else { t.search_dense(ctx, bound)? };
        let Some(knob) = found else {
            let why = "does not fit in VRAM at any setting".to_string();
            log(&format!("context {ctx}: {why}; larger contexts will not fit either"));
            result.skipped.push((ctx, why));
            break;
        };
        bound = knob; // a larger context needs at least as much offloading
        let mut knob = knob;
        let mut trial = t.measure(ctx, knob)?;
        // The long speed-test prompt can allocate more compute buffers than the probe did; back off if so.
        for _ in 0..4 {
            if trial.vram_mb <= limit_mb {
                break;
            }
            knob = if moe { (knob + 1).min(top) } else { knob.saturating_sub(1) };
            log(&format!("  {} MiB during the speed test is over the limit, backing off to {knob}", trial.vram_mb));
            trial = t.measure(ctx, knob)?;
        }
        log(&format!(
            "context {ctx}: {} {} -> {} MiB, decode {:.1} tok/s, prefill {:.0} tok/s",
            if moe { "--n-cpu-moe" } else { "-ngl" },
            trial.knob,
            trial.vram_mb,
            trial.decode_tps,
            trial.prefill_tps
        ));
        result.trials.push(trial);
    }

    let best = result.trials.iter().map(|t| t.decode_tps).fold(0.0, f64::max);
    result.recommended = result
        .trials
        .iter()
        .filter(|t| t.decode_tps >= best * SPEED_TOLERANCE)
        .max_by_key(|t| t.ctx)
        .cloned();
    server::stop()?;
    Ok(result)
}

impl Tuning {
    pub fn knob_name(&self) -> &'static str {
        if self.moe { "--n-cpu-moe" } else { "-ngl" }
    }

    /// Plain-text summary used by the command line and the log pane.
    pub fn summary(&self) -> String {
        let mut s = format!(
            "{:>9} {:>12} {:>10} {:>14} {:>14}\n",
            "context",
            self.knob_name(),
            "VRAM MiB",
            "decode tok/s",
            "prefill tok/s"
        );
        for t in &self.trials {
            let star = if self.recommended.as_ref().is_some_and(|r| r.ctx == t.ctx) { "  <- recommended" } else { "" };
            s += &format!(
                "{:>9} {:>12} {:>10} {:>14.1} {:>14.0}{star}\n",
                t.ctx, t.knob, t.vram_mb, t.decode_tps, t.prefill_tps
            );
        }
        for (ctx, why) in &self.skipped {
            s += &format!("{ctx:>9}  skipped: {why}\n");
        }
        s
    }
}

impl Tuner<'_> {
    fn check_cancel(&self) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) { Err(anyhow!("cancelled")) } else { Ok(()) }
    }

    fn trial_model(&self, ctx: u32, knob: u32) -> Model {
        let mut m = self.model.clone();
        m.settings.ctx = ctx;
        if self.moe {
            m.settings.ngl = 99;
            m.settings.n_cpu_moe = Some(knob);
        } else {
            m.settings.ngl = knob;
            m.settings.n_cpu_moe = None;
        }
        // Everything is set explicitly, so stop llama.cpp from second-guessing the trial.
        m.settings.extra_args.extend(["--fit".to_string(), "off".to_string()]);
        m
    }

    /// After a crashed trial the driver can take several seconds to release the VRAM; starting the
    /// next trial before that makes it fail for reasons unrelated to its settings.
    fn wait_vram_free(&self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            match server::gpu_vram() {
                Ok((used, _)) if used <= self.baseline_mb + 400 => return,
                Ok(_) => std::thread::sleep(Duration::from_millis(500)),
                Err(_) => return,
            }
        }
    }

    /// Starts the server with these settings. Ok(false) means it crashed or never became ready,
    /// even after one retry.
    fn launch(&mut self, ctx: u32, knob: u32) -> Result<bool> {
        for attempt in 0..2 {
            if self.launch_once(ctx, knob)? {
                return Ok(true);
            }
            if attempt == 0 {
                (self.log)("  start failed, waiting for the GPU to settle and retrying once ...");
            }
        }
        Ok(false)
    }

    fn launch_once(&mut self, ctx: u32, knob: u32) -> Result<bool> {
        self.check_cancel()?;
        let _ = server::stop();
        self.wait_vram_free();
        server::start(self.cfg, &self.trial_model(ctx, knob))?;
        self.current = Some((ctx, knob));
        let deadline = Instant::now() + Duration::from_secs(300);
        let mut sys = server::new_system();
        loop {
            self.check_cancel()?;
            if server::health(self.cfg) == server::Health::Ready {
                return Ok(true);
            }
            if server::running(&mut sys).is_none() || Instant::now() > deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    fn label(&self) -> &'static str {
        if self.moe { "--n-cpu-moe" } else { "-ngl" }
    }

    /// Loads the model with these settings, sends a prompt large enough to allocate the compute
    /// buffers, and reports whether VRAM stayed under the limit.
    fn probe(&mut self, ctx: u32, knob: u32) -> Result<Probe> {
        if let Some(p) = self.cache.get(&(ctx, knob)) {
            return Ok(p.clone());
        }
        (self.log)(&format!("trying ctx {ctx}, {} {knob} ...", self.label()));
        let probe = if !self.launch(ctx, knob)? {
            (self.log)("  server did not start (out of memory?) -> does not fit");
            Probe { fits: false, vram_mb: u32::MAX }
        } else {
            let warm = self.chat(&filler(1500.min(ctx as usize / 4), "warmup"), 16);
            let vram = server::gpu_vram()?.0;
            if let Err(e) = &warm {
                (self.log)(&format!("  the first request failed ({e:#}); the server probably ran out of VRAM and crashed -> does not fit"));
                Probe { fits: false, vram_mb: u32::MAX }
            } else {
                let fits = vram <= self.limit_mb;
                (self.log)(&format!("  {vram} MiB used -> {}", if fits { "fits" } else { "too much" }));
                Probe { fits, vram_mb: vram }
            }
        };
        self.cache.insert((ctx, knob), probe.clone());
        Ok(probe)
    }

    /// Smallest `--n-cpu-moe` in [lo, hi] that fits, or None if even `hi` does not.
    fn search_moe(&mut self, ctx: u32, lo: u32, hi: u32) -> Result<Option<u32>> {
        if !self.probe(ctx, hi)?.fits {
            return Ok(None);
        }
        let (mut lo, mut hi) = (lo, hi);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.probe(ctx, mid)?.fits { hi = mid } else { lo = mid + 1 }
        }
        Ok(Some(hi))
    }

    /// Largest `-ngl` in [0, hi] that fits, or None if not even 0 does.
    fn search_dense(&mut self, ctx: u32, hi: u32) -> Result<Option<u32>> {
        if !self.probe(ctx, 0)?.fits {
            return Ok(None);
        }
        let (mut lo, mut hi) = (0, hi);
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if self.probe(ctx, mid)?.fits { lo = mid } else { hi = mid - 1 }
        }
        Ok(Some(lo))
    }

    fn chat(&self, prompt: &str, max_tokens: u32) -> Result<serde_json::Value> {
        let body = json!({
            "messages": [{ "role": "user", "content": prompt }],
            "max_tokens": max_tokens,
            "temperature": 0.3,
            "cache_prompt": false,
        });
        server::post_json(self.cfg, "/v1/chat/completions", &body, Duration::from_secs(600))
    }

    /// Measures decode and prefill speed at these settings (relaunching if the server is not on them).
    fn measure(&mut self, ctx: u32, knob: u32) -> Result<Trial> {
        if self.current != Some((ctx, knob)) {
            (self.log)(&format!("loading ctx {ctx}, {} {knob} for speed test ...", self.label()));
            if !self.launch(ctx, knob)? {
                return Err(anyhow!("server failed to start for the final measurement"));
            }
        }
        (self.log)("  measuring decode speed ...");
        let essay = "Write a long, detailed essay about the history of computing.";
        let _ = self.chat(essay, 96)?; // warm-up run, discarded
        let r = self.chat(essay, 128)?;
        let decode_tps = r["timings"]["predicted_per_second"].as_f64().unwrap_or(0.0);

        let tokens = (ctx as usize / 4).min(8000);
        (self.log)(&format!("  measuring prefill speed on a ~{tokens}-token prompt ..."));
        let r = self.chat(&format!("{}\n\nIn one sentence, what animal jumps over the dog?", filler(tokens, "prefill")), 8)?;
        let prefill_tps = r["timings"]["prompt_per_second"].as_f64().unwrap_or(0.0);
        let vram_mb = server::gpu_vram()?.0.max(self.cache.get(&(ctx, knob)).map_or(0, |p| p.vram_mb));
        Ok(Trial { ctx, knob, vram_mb, decode_tps, prefill_tps })
    }
}

/// Roughly `tokens` tokens of filler text (about 26 tokens per paragraph), unique per `salt`.
fn filler(tokens: usize, salt: &str) -> String {
    let para = "The quick brown fox jumps over the lazy dog while the committee reviews quarterly logistics, procurement schedules and maintenance records for the northern depot. ";
    format!("[{salt}] {}", para.repeat((tokens / 26).max(1)))
}
