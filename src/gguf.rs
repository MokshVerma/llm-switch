use anyhow::{Result, bail};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

/// The few GGUF header fields the tuner needs.
#[derive(Debug, Clone, Default)]
pub struct GgufInfo {
    pub arch: String,
    pub block_count: Option<u32>,
    pub expert_count: Option<u32>,
    pub context_length: Option<u32>,
}

impl GgufInfo {
    pub fn is_moe(&self) -> bool {
        self.expert_count.unwrap_or(0) > 1
    }
}

fn u32le(r: &mut impl Read) -> Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn u64le(r: &mut impl Read) -> Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

fn skip(r: &mut BufReader<File>, n: u64) -> Result<()> {
    r.seek_relative(n as i64)?;
    Ok(())
}

fn read_string(r: &mut BufReader<File>) -> Result<String> {
    let len = u64le(r)?;
    if len > 1 << 20 {
        bail!("implausible string length {len} in GGUF header");
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn fixed_size(ty: u32) -> Option<u64> {
    match ty {
        0 | 1 | 7 => Some(1),
        2 | 3 => Some(2),
        4..=6 => Some(4),
        10..=12 => Some(8),
        _ => None,
    }
}

fn skip_value(r: &mut BufReader<File>, ty: u32) -> Result<()> {
    if let Some(n) = fixed_size(ty) {
        return skip(r, n);
    }
    match ty {
        8 => {
            let len = u64le(r)?;
            skip(r, len)
        }
        9 => {
            let elem = u32le(r)?;
            let n = u64le(r)?;
            if let Some(sz) = fixed_size(elem) {
                skip(r, n * sz)
            } else {
                for _ in 0..n {
                    skip_value(r, elem)?;
                }
                Ok(())
            }
        }
        _ => bail!("unknown GGUF value type {ty}"),
    }
}

pub fn read_info(path: &Path) -> Result<GgufInfo> {
    let mut r = BufReader::new(File::open(path)?);
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)?;
    if &magic != b"GGUF" {
        bail!("{} is not a GGUF file", path.display());
    }
    let _version = u32le(&mut r)?;
    let _tensors = u64le(&mut r)?;
    let kvs = u64le(&mut r)?;

    let mut arch = String::new();
    let mut nums: HashMap<String, u64> = HashMap::new();
    for _ in 0..kvs {
        let key = read_string(&mut r)?;
        let ty = u32le(&mut r)?;
        match ty {
            8 if key == "general.architecture" => arch = read_string(&mut r)?,
            4 | 5 if key.ends_with(".block_count") || key.ends_with(".expert_count") || key.ends_with(".context_length") => {
                nums.insert(key, u32le(&mut r)? as u64);
            }
            10 | 11 if key.ends_with(".block_count") || key.ends_with(".expert_count") || key.ends_with(".context_length") => {
                nums.insert(key, u64le(&mut r)?);
            }
            _ => skip_value(&mut r, ty)?,
        }
    }
    let get = |suffix: &str| nums.get(&format!("{arch}.{suffix}")).map(|v| *v as u32);
    Ok(GgufInfo {
        block_count: get("block_count"),
        expert_count: get("expert_count"),
        context_length: get("context_length"),
        arch,
    })
}
