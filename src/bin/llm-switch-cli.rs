use anyhow::Result;
use llm_switch::config::{Config, launch_args};
use llm_switch::download::Progress;
use llm_switch::{actions, download, hardware, llamacpp, models, opencode, platform, server, tune};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

/// Rewrites one status line in place while a download runs.
fn show_progress(name: &str, p: Progress) {
    let line = match p.total {
        Some(t) if t > 0 => format!(
            "  {name}: {} of {} ({:.0}%)  {:.1} MB/s",
            gb(p.done),
            gb(t),
            p.done as f64 / t as f64 * 100.0,
            p.bytes_per_sec / 1e6
        ),
        _ => format!("  {name}: {}  {:.1} MB/s", gb(p.done), p.bytes_per_sec / 1e6),
    };
    print!("\r{line:<100}");
    let _ = std::io::stdout().flush();
}

const USAGE: &str = "llm-switch-cli list       models found in the models folder
llm-switch-cli status     server state
llm-switch-cli start [m]  start model m (partial name; default: first model)
llm-switch-cli stop       stop the server
llm-switch-cli toggle [m] stop if m is running, otherwise start it
llm-switch-cli sync [m]   write m's context limits into opencode.json
llm-switch-cli args [m]   print the llama-server command line
llm-switch-cli config     print the config file path
llm-switch-cli tune [m] [--ctx 32768,65536,131072] [--headroom 700] [--apply]
                          test settings on this PC and recommend the best one
llm-switch-cli hardware   what this machine has (OS, CPU, GPU, RAM)
llm-switch-cli setup      what is installed and what is still missing
llm-switch-cli install-llama [--dir DIR] [--dry-run]
                          download and install the right llama.cpp build for this machine
llm-switch-cli download LINK [--pick N] [--dry-run]
                          download a model from a Hugging Face link or a direct .gguf URL
(run llm-switch with no arguments for the window)";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = cli(&args) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn cli(args: &[String]) -> Result<()> {
    let Some(cmd) = args.first() else {
        println!("{USAGE}");
        return Ok(());
    };
    let cfg = Config::load()?;
    let query = args.get(1).map(|s| s.as_str());
    match cmd.as_str() {
        "list" => {
            for m in cfg.scan_models() {
                let s = &m.settings;
                println!(
                    "{}  {:.1} GB  ctx {}  n-cpu-moe {:?}  {}",
                    m.file,
                    m.size_gb,
                    s.ctx,
                    s.n_cpu_moe,
                    if m.configured { "[configured]" } else { "[defaults]" }
                );
            }
        }
        "status" => {
            let mut sys = server::new_system();
            match server::running(&mut sys) {
                None => println!("stopped"),
                Some(r) => println!(
                    "running pid {} model {} ({:?})",
                    r.pid,
                    r.model_path.as_deref().unwrap_or("?"),
                    server::health(&cfg)
                ),
            }
        }
        "start" => println!("{}", actions::start_model(&cfg, &cfg.find_model(query)?)?),
        "stop" => println!("{}", actions::stop_server()?),
        "toggle" => println!("{}", actions::toggle(&cfg, &cfg.find_model(query)?)?),
        "sync" => println!("{}", opencode::sync(&cfg, &cfg.find_model(query)?)?),
        "args" => {
            let m = cfg.find_model(query)?;
            println!("{} {}", cfg.llama_server.display(), launch_args(&cfg, &m).join(" "));
        }
        "config" => println!("{}", Config::path().display()),
        "hardware" => println!("{}", hardware::detect().summary()),
        "setup" => {
            println!("hardware:      {}", hardware::detect().summary());
            if cfg.llama_server.is_file() {
                match llamacpp::verify_server(&cfg.llama_server) {
                    Ok(v) => println!("llama-server:  {} ({v})", cfg.llama_server.display()),
                    Err(e) => println!("llama-server:  {} is there but does not run: {e:#}", cfg.llama_server.display()),
                }
            } else {
                println!("llama-server:  NOT FOUND at {}\n               -> run: llm-switch-cli install-llama", cfg.llama_server.display());
            }
            let found = cfg.scan_models();
            if found.is_empty() {
                println!("models:        none in {}\n               -> run: llm-switch-cli download <link>", cfg.models_dir.display());
            } else {
                println!("models:        {} in {}", found.len(), cfg.models_dir.display());
            }
        }
        "install-llama" => {
            let mut dir = platform::default_llama_dir();
            let mut dry = false;
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--dir" => dir = PathBuf::from(it.next().ok_or_else(|| anyhow::anyhow!("--dir needs a folder"))?),
                    "--dry-run" => dry = true,
                    other => anyhow::bail!("unknown option {other}"),
                }
            }
            let hw = hardware::detect();
            println!("this machine: {}", hw.summary());
            let plan = llamacpp::plan(&hw)?;
            println!("llama.cpp {} - {}", plan.release, plan.backend.label());
            println!("  {} ({})", plan.primary.name, gb(plan.primary.size));
            if let Some(r) = &plan.runtime {
                println!("  {} ({}), the CUDA runtime libraries", r.name, gb(r.size));
            }
            if let Some(n) = &plan.note {
                println!("  note: {n}");
            }
            println!("install into: {}", dir.display());
            if dry {
                println!("(dry run: nothing was downloaded)");
                return Ok(());
            }
            let cancel = AtomicBool::new(false);
            let mut cfg = cfg;
            let msg = actions::install_llamacpp(&mut cfg, &dir, &plan, &mut show_progress, &cancel)?;
            println!("\n{msg}\nThe config now points at it: {}", Config::path().display());
        }
        "download" => {
            let mut link = None;
            let mut pick: Option<usize> = None;
            let mut dry = false;
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--pick" => pick = Some(it.next().ok_or_else(|| anyhow::anyhow!("--pick needs a number"))?.parse()?),
                    "--dry-run" => dry = true,
                    other if link.is_none() => link = Some(other),
                    other => anyhow::bail!("unexpected argument {other}"),
                }
            }
            let link = link.ok_or_else(|| anyhow::anyhow!("usage: llm-switch-cli download LINK [--pick N]"))?;
            let token = models::hf_token();
            let found = models::lookup(link, token.as_deref())?;
            let chosen = match (found.len(), pick) {
                (1, _) => &found[0],
                (_, Some(n)) if n >= 1 && n <= found.len() => &found[n - 1],
                _ => {
                    println!("{} GGUF models in that repository. Pick one with --pick N:", found.len());
                    for (i, m) in found.iter().enumerate() {
                        let size = m.total().map_or("size unknown".to_string(), gb);
                        let shards = if m.parts.len() > 1 { format!(", {} files", m.parts.len()) } else { String::new() };
                        println!("  {:>2}. {}  ({size}{shards})", i + 1, m.title);
                    }
                    std::process::exit(2);
                }
            };
            let size = chosen.total();
            println!("{}  {}", chosen.title, size.map_or("size unknown".to_string(), gb));
            if let (Some(s), Some(free)) = (size, download::free_space(&cfg.models_dir)) {
                println!("into {}  ({} free{})", cfg.models_dir.display(), gb(free), if free < s + (1 << 30) { ", NOT ENOUGH" } else { "" });
            }
            if dry {
                println!("(dry run: nothing was downloaded)");
                return Ok(());
            }
            let cancel = AtomicBool::new(false);
            let mut cfg = cfg;
            let msg = actions::download_model(&mut cfg, chosen, &mut show_progress, &cancel)?;
            println!("\n{msg}");
        }
        "tune" => {
            let mut contexts = tune::DEFAULT_CONTEXTS.to_vec();
            let mut headroom_mb = tune::DEFAULT_HEADROOM_MB;
            let mut apply = false;
            let mut query = None;
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--ctx" => {
                        let v = it.next().ok_or_else(|| anyhow::anyhow!("--ctx needs a list, e.g. 32768,65536"))?;
                        contexts = v.split(',').map(|c| c.trim().parse()).collect::<Result<_, _>>()?;
                    }
                    "--headroom" => {
                        headroom_mb = it.next().ok_or_else(|| anyhow::anyhow!("--headroom needs MiB"))?.parse()?;
                    }
                    "--apply" => apply = true,
                    other => query = Some(other),
                }
            }
            let model = cfg.find_model(query)?;
            let cancel = AtomicBool::new(false);
            let log = |l: &str| println!("{l}");
            let result = actions::tune_and_restore(&cfg, &model, &tune::Options { contexts, headroom_mb }, &log, &cancel)?;
            println!("\n{}", result.summary());
            match &result.recommended {
                None => println!("No configuration fit; try a smaller context or more headroom."),
                Some(r) if apply => {
                    let mut cfg = cfg;
                    cfg.apply_tuning(&result.file, r.ctx, result.moe, r.knob)?;
                    println!("Saved to {}: context {}, {} {}", Config::path().display(), r.ctx, result.knob_name(), r.knob);
                }
                Some(r) => println!("Recommended: context {}, {} {}  (re-run with --apply to save it)", r.ctx, result.knob_name(), r.knob),
            }
        }
        _ => println!("{USAGE}"),
    }
    Ok(())
}
