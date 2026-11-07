use crate::config::{Config, Model};
use crate::{opencode, server, tune};
use anyhow::Result;
use std::sync::atomic::AtomicBool;

/// Starts `model` (replacing whatever is running) and syncs opencode.json when `auto_sync` is on.
pub fn start_model(cfg: &Config, model: &Model) -> Result<String> {
    let mut notes = Vec::new();
    if cfg.opencode.auto_sync {
        notes.push(opencode::sync(cfg, model)?);
    }
    let pid = server::start(cfg, model)?;
    notes.push(format!("started {} (pid {pid}), loading takes ~10 s", model.file));
    Ok(notes.join("; "))
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
