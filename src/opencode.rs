use crate::config::{Config, Model};
use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use std::fs;

/// Writes the model's context/output limits (and the compaction buffer) into opencode.json,
/// leaving every other key untouched. A `.llm-switch.bak` copy of the original is kept once.
pub fn sync(cfg: &Config, model: &Model) -> Result<String> {
    let path = &cfg.opencode.config_path;
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut root: Value = serde_json::from_str(&text)
        .with_context(|| format!("{} is not plain JSON (comments or trailing commas?); not touching it", path.display()))?;
    if !root.is_object() {
        return Err(anyhow!("{} does not contain a JSON object", path.display()));
    }

    let before = root.clone();
    let pid = &cfg.opencode.provider_id;
    let provider = &mut root["provider"][pid];
    provider["npm"] = json!("@ai-sdk/openai-compatible");
    provider["name"] = json!("llama.cpp");
    provider["options"]["baseURL"] = json!(cfg.base_url());
    provider["models"][&model.opencode_id] = json!({
        "name": model.name,
        "limit": { "context": model.settings.ctx, "output": cfg.opencode.output_limit },
    });
    if let Some(buf) = cfg.opencode.compaction_buffer {
        root["compaction"]["auto"] = json!(true);
        root["compaction"]["buffer"] = json!(buf);
    }

    if root == before {
        return Ok("opencode.json already up to date".into());
    }
    let bak = path.with_extension("json.llm-switch.bak");
    if !bak.exists() {
        fs::write(&bak, &text).with_context(|| format!("writing {}", bak.display()))?;
    }
    fs::write(path, serde_json::to_string_pretty(&root)? + "\n")?;
    Ok(format!(
        "opencode.json updated: {}/{} context {}",
        pid, model.opencode_id, model.settings.ctx
    ))
}
