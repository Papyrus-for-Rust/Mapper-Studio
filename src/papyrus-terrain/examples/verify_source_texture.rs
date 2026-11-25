//! Compare the Rust source sampler to the owned managed TextureData oracle.
use papyrus_terrain::source_texture::Texture;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::Read,
    path::PathBuf,
};
#[derive(Deserialize)]
struct Input {
    width: usize,
    height: usize,
    rgba: PathBuf,
    queries: Vec<[f32; 2]>,
}
#[derive(Deserialize)]
struct Oracle {
    format: String,
    results: Vec<Query>,
    input_sha256: String,
    texture_sha256: String,
    oracle_assembly_sha256: String,
}
#[derive(Deserialize)]
struct Query {
    uv: [f32; 2],
    channels: [u32; 4],
    height: u32,
    topology: i32,
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err("Expected input, owned oracle, new audit output".into());
    }
    let read =
        |path: &std::path::Path, limit: u64| -> Result<Vec<u8>, Box<dyn std::error::Error>> {
            let mut bytes = Vec::new();
            File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
            if bytes.len() as u64 > limit {
                return Err("Input exceeds diagnostic bound".into());
            }
            Ok(bytes)
        };
    let input_bytes = read(args[0].as_ref(), 12 * 1024 * 1024)?;
    let input: Input = serde_json::from_slice(&input_bytes)?;
    if !(3..=4096).contains(&input.width) || !(3..=4096).contains(&input.height) {
        return Err("Invalid dimensions".into());
    }
    let rgba = read(&input.rgba, (input.width * input.height * 4) as u64)?;
    let oracle: Oracle = serde_json::from_slice(&read(args[1].as_ref(), 32 * 1024 * 1024)?)?;
    if oracle.format != "mapper-owned-texture-oracle-1"
        || oracle.results.is_empty()
        || oracle.results.len() > 100000
    {
        return Err("Invalid owned oracle".into());
    }
    if oracle.input_sha256 != format!("{:x}", Sha256::digest(&input_bytes))
        || oracle.texture_sha256 != format!("{:x}", Sha256::digest(&rgba))
        || input.queries.len() != oracle.results.len()
        || input
            .queries
            .iter()
            .zip(&oracle.results)
            .any(|(uv, q)| uv.map(f32::to_bits) != q.uv.map(f32::to_bits))
    {
        return Err("Owned oracle is not bound to this input/texture/query set".into());
    }
    let texture = Texture {
        width: input.width,
        height: input.height,
        rgba: &rgba,
    };
    let mut channels = 0;
    let mut heights = 0;
    let mut topology = 0;
    for q in &oracle.results {
        for c in 0..4 {
            channels += usize::from(texture.channel(q.uv, c)?.to_bits() != q.channels[c]);
        }
        heights += usize::from(texture.height(q.uv)?.to_bits() != q.height);
        topology += usize::from(texture.topology(q.uv)? != q.topology);
    }
    let report = serde_json::json!({"format":"mapper-source-texture-audit-1","engine":papyrus_terrain::source_texture::VERSION,
        "input_sha256":oracle.input_sha256,"texture_sha256":oracle.texture_sha256,"oracle_assembly_sha256":oracle.oracle_assembly_sha256,
        "queries":oracle.results.len(),"channel_bit_mismatches":channels,"height_bit_mismatches":heights,"topology_mismatches":topology,
        "stamp_composition_validated":false,"full_map_validated":false});
    serde_json::to_writer_pretty(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&args[2])?,
        &report,
    )?;
    println!("{report}");
    if channels + heights + topology != 0 {
        return Err("Source texture oracle mismatch".into());
    }
    Ok(())
}
