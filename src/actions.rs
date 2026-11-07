use crate::config::{Config, Model};
use crate::download::Progress;
use crate::models::{self, RemoteModel};
use crate::{gguf, llamacpp, opencode, server, suggest, tune};
use anyhow::{Result, anyhow};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

/// Starts `model` (replacing whatever is running) and syncs opencode.json when `auto_sync` is on.
pub fn start_model(cfg: &Config, model: &Model) -> Result<String> {
    let mut notes = Vec::new();
    // Someone who has not installed OpenCode has no opencode.json; that is not an error.
    if cfg.opencode.auto_sync && cfg.opencode.config_path.exists() {
        notes.push(opencode::sync(cfg, model)?);
    }
    let pid = server::start(cfg, model)?;
    notes.push(format!("started {} (pid {pid}), loading takes ~10 s", model.file));
    Ok(notes.join("; "))
}

/// Downloads and installs the planned llama.cpp build into `dir`, then points the config at it.
pub fn install_llamacpp(
    cfg: &mut Config,
    dir: &Path,
    plan: &llamacpp::Plan,
    on_progress: &mut dyn FnMut(&str, Progress),
    cancel: &AtomicBool,
) -> Result<String> {
    let (exe, version) = llamacpp::install(plan, dir, on_progress, cancel)?;
    cfg.llama_server = exe.clone();
    cfg.save()?;
    Ok(format!("installed llama.cpp {} [{}] at {} ({version})", plan.release, plan.backend.label(), exe.display()))
}

/// Downloads a model into the models folder and gives it safe starting settings.
pub fn download_model(
    cfg: &mut Config,
    model: &RemoteModel,
    on_progress: &mut dyn FnMut(&str, Progress),
    cancel: &AtomicBool,
) -> Result<String> {
    let token = models::hf_token();
    let paths = models::download_model(cfg, model, token.as_deref(), on_progress, cancel)?;
    finish_model_setup(cfg, &paths)
}

/// Writes starting settings for a freshly downloaded model, unless the user already configured it.
pub fn finish_model_setup(cfg: &mut Config, paths: &[PathBuf]) -> Result<String> {
    let first = paths.first().ok_or_else(|| anyhow!("no files were downloaded"))?;
    let file = first.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if cfg.has_model(&file) {
        return Ok(format!("{file} is ready. It already has its own settings, which were left alone."));
    }
    let info = gguf::read_info(first)?;
    let size: u64 = paths.iter().filter_map(|p| fs::metadata(p).ok()).map(|m| m.len()).sum();
    let vram = server::gpu_vram().ok().map(|(_, total)| total);
    let s = suggest::starting_settings(&info, size, vram);
    cfg.add_model(&file, s.overrides())?;
    Ok(format!(
        "{file} is ready. Starting settings: context {}, GPU layers {}, experts kept on the CPU {}. {} \
         These are estimates; run Auto-tune to find the best values for this machine.",
        s.ctx, s.ngl, s.n_cpu_moe, s.note
    ))
}

pub fn stop_server() -> Result<String> {
    let n = server::stop()?;
    Ok(if n == 0 { "server was not running".into() } else { "server stopped".into() })
}

/// Runs the auto-tuner, then puts the server back the way it was: if it was running before, it is
/// started again with the model's current (untuned) settings, whether tuning succeeded or not.
pub fn tune_and_restore(
    cfg: &Config,
    model: &Model,
    opts: &tune::Options,
    log: &dyn Fn(&str),
    cancel: &AtomicBool,
) -> Result<tune::Tuning> {
    let mut sys = server::new_system();
    let was_running = server::running(&mut sys).is_some();
    let result = tune::run(cfg, model, opts, log, cancel);
    if was_running {
        log("restoring the server with your current settings ...");
        if let Err(e) = server::start(cfg, model) {
            log(&format!("could not restart the server: {e:#}"));
        }
    } else {
        let _ = server::stop();
    }
    result
}

/// Stops the server if `model` is the one running, otherwise starts it.
pub fn toggle(cfg: &Config, model: &Model) -> Result<String> {
    let mut sys = server::new_system();
    match server::running(&mut sys) {
        Some(r) if server::is_model(&r, model) => stop_server(),
        _ => start_model(cfg, model),
    }
}
