//! Safe starting settings for a model that was just downloaded, before the auto-tuner has measured anything.
//!
//! These are estimates from the model's size and the GPU's memory. They are deliberately a little
//! conservative: a setting that is slightly too cautious is slower, while one that is too bold crashes.
//! The MoE estimate is calibrated against measurements on a 12 GB RTX 3060 with a 19.7 GB, 40-layer
//! Qwen3.5-35B-A3B Q4_K_M, where `--n-cpu-moe 25` was the last value that fit at 64K context.

use crate::config::Overrides;
use crate::gguf::GgufInfo;

/// Memory the desktop and other programs use, plus a safety margin, in MiB.
const RESERVED_MB: u32 = 1500 + 700;
/// Context the starting settings use. Bigger contexts are what the auto-tuner is for.
const START_CTX: u32 = 32768;

#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub ctx: u32,
    pub ngl: u32,
    pub n_cpu_moe: u32,
    pub note: String,
}

impl Suggestion {
    pub fn overrides(&self) -> Overrides {
        Overrides { ctx: Some(self.ctx), ngl: Some(self.ngl), n_cpu_moe: Some(self.n_cpu_moe), ..Default::default() }
    }
}

/// `info` comes from the GGUF header, `size_bytes` is the file size (all shards), and `vram_total_mb` is the
/// GPU's dedicated memory if it is known (NVIDIA). Without it the model is assumed to run entirely on the
/// GPU, which is right for Apple Silicon's unified memory and harmless on a CPU-only machine.
pub fn starting_settings(info: &GgufInfo, size_bytes: u64, vram_total_mb: Option<u32>) -> Suggestion {
    let layers = info.block_count.unwrap_or(32).max(1);
    let ctx = info.context_length.unwrap_or(START_CTX).clamp(4096, START_CTX);
    let size_mb = size_bytes as f64 / (1024.0 * 1024.0);
    // KV cache with a Q8 cache type: roughly 32 MiB per layer for 32K of context, halved for models
    // (like Qwen3.5) that only use full attention in some layers.
    let kv_mb = layers as f64 * ctx as f64 / 32768.0 * 32.0;

    let Some(total) = vram_total_mb else {
        return Suggestion {
            ctx,
            ngl: 99,
            n_cpu_moe: 0,
            note: "GPU memory is unknown here, so everything is offloaded to the GPU / unified memory.".into(),
        };
    };
    let budget = total.saturating_sub(RESERVED_MB) as f64;

    // Fits entirely: use the GPU for everything.
    if size_mb + kv_mb <= budget {
        return Suggestion { ctx, ngl: 99, n_cpu_moe: 0, note: "The whole model fits in GPU memory.".into() };
    }

    if info.is_moe() {
        // Attention, shared weights and the KV cache stay on the GPU; the experts are what spills over.
        let fixed = (0.14 * size_mb).max(1500.0) + kv_mb / 2.0;
        let expert_total = size_mb - 0.14 * size_mb;
        let per_layer = (expert_total / layers as f64).max(1.0);
        let on_gpu = ((budget - fixed).max(0.0) / per_layer).floor() as u32;
        let n_cpu_moe = if on_gpu >= layers { 0 } else { (layers - on_gpu + 1).min(layers) };
        return Suggestion {
            ctx,
            ngl: 99,
            n_cpu_moe,
            note: format!("Keeps the experts of {n_cpu_moe} of {layers} layers in RAM so the rest fits in GPU memory."),
        };
    }

    // Dense model that does not fit: put as many layers as fit on the GPU.
    let usable = (budget - kv_mb).max(0.0);
    let ngl = ((usable / size_mb) * (layers as f64 + 1.0)).floor().clamp(0.0, layers as f64) as u32;
    Suggestion {
        ctx,
        ngl,
        n_cpu_moe: 0,
        note: format!("Only {ngl} of {layers} layers fit in GPU memory; the rest run on the CPU, which is slower."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    fn moe(layers: u32) -> GgufInfo {
        GgufInfo { arch: "x".into(), block_count: Some(layers), expert_count: Some(256), context_length: Some(262144) }
    }

    fn dense(layers: u32) -> GgufInfo {
        GgufInfo { arch: "x".into(), block_count: Some(layers), expert_count: None, context_length: Some(131072) }
    }

    #[test]
    fn matches_the_measured_edge_for_the_calibration_model() {
        // 19.72 GB Qwen3.5-35B-A3B Q4_K_M, 40 layers, on a 12 GB RTX 3060.
        // Measured: --n-cpu-moe 24 crashed, 25 fit at 64K. The estimate must be safe (>= 25) without being wasteful.
        let s = starting_settings(&moe(40), 20_193 * MIB, Some(12288));
        assert!((25..=28).contains(&s.n_cpu_moe), "got {}", s.n_cpu_moe);
        assert_eq!(s.ctx, 32768, "starts at 32K even though the model supports 256K");
        assert_eq!(s.ngl, 99);
    }

    #[test]
    fn a_small_moe_that_fits_uses_no_cpu_offload() {
        let s = starting_settings(&moe(24), 3_000 * MIB, Some(12288));
        assert_eq!((s.ngl, s.n_cpu_moe), (99, 0));
    }

    #[test]
    fn more_vram_means_fewer_experts_in_ram() {
        let small = starting_settings(&moe(40), 20_193 * MIB, Some(8192)).n_cpu_moe;
        let mid = starting_settings(&moe(40), 20_193 * MIB, Some(12288)).n_cpu_moe;
        let big = starting_settings(&moe(40), 20_193 * MIB, Some(24576)).n_cpu_moe;
        assert!(small > mid && mid > big, "{small} {mid} {big}");
    }

    #[test]
    fn a_tiny_gpu_pushes_every_expert_to_ram_but_never_past_the_layer_count() {
        let s = starting_settings(&moe(40), 20_193 * MIB, Some(2048));
        assert_eq!(s.n_cpu_moe, 40);
    }

    #[test]
    fn a_dense_model_that_fits_goes_fully_on_the_gpu() {
        let s = starting_settings(&dense(32), 4_500 * MIB, Some(12288));
        assert_eq!((s.ngl, s.n_cpu_moe), (99, 0));
    }

    #[test]
    fn a_dense_model_too_big_for_the_gpu_gets_partial_offload() {
        let s = starting_settings(&dense(64), 40_000 * MIB, Some(12288));
        assert!(s.ngl > 0 && s.ngl < 64, "got {}", s.ngl);
        assert_eq!(s.n_cpu_moe, 0, "n-cpu-moe is a MoE option and must be off for a dense model");
    }

    #[test]
    fn unknown_gpu_memory_defaults_to_all_gpu() {
        let s = starting_settings(&dense(32), 4_500 * MIB, None);
        assert_eq!((s.ngl, s.n_cpu_moe), (99, 0));
    }

    #[test]
    fn missing_header_fields_do_not_panic() {
        let s = starting_settings(&GgufInfo::default(), 1_000 * MIB, Some(12288));
        assert_eq!(s.ctx, 32768);
        let s = starting_settings(&GgufInfo { context_length: Some(2048), ..Default::default() }, 1_000 * MIB, Some(12288));
        assert_eq!(s.ctx, 4096, "context is never below 4096");
    }
}
