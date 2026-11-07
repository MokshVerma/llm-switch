use anyhow::Result;
use llm_switch::config::{Config, launch_args};
use llm_switch::{actions, opencode, server, tune};
use std::sync::atomic::AtomicBool;

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
