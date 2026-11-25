//! Compare integer coverage with a locally compiled, user-owned source method.
use papyrus_terrain::source_stamp_raster::Quad;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::Read,
    path::Path,
};
#[derive(Deserialize)]
struct Query {
    resolution: usize,
    vertices: Option<[[i32; 2]; 4]>,
    world: Option<[[f32; 2]; 4]>,
    origin: Option<[f32; 2]>,
    inverse_size: Option<[f32; 2]>,
}
#[derive(Deserialize)]
struct Input {
    format: String,
    queries: Vec<Query>,
}
#[derive(Deserialize)]
struct Row {
    count: usize,
    sha256: String,
    vertices: Option<[[i32; 2]; 4]>,
}
#[derive(Deserialize)]
struct Oracle {
    format: String,
    input_sha256: String,
    oracle_sha256: String,
    source_sha256: String,
    meta_source_sha256: Option<String>,
    results: Vec<Row>,
}
fn read(path: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("Input exceeds bound".into());
    }
    Ok(bytes)
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err("INPUT ORACLE NEW_AUDIT required".into());
    }
    let bytes = read(args[0].as_ref())?;
    let input: Input = serde_json::from_slice(&bytes)?;
    let oracle: Oracle = serde_json::from_slice(&read(args[1].as_ref())?)?;
    if input.format != "mapper-stamp-raster-queries-1"
        || oracle.format != "mapper-owned-stamp-raster-oracle-1"
        || oracle.input_sha256 != digest(&bytes)
        || !valid_hash(&oracle.oracle_sha256)
        || !valid_hash(&oracle.source_sha256)
        || input.queries.len() > 50000
        || input.queries.len() != oracle.results.len()
    {
        return Err("Invalid oracle receipt or query count".into());
    }
    let mut mismatches = 0;
    let mut world_queries = 0;
    for (q, expected) in input.queries.iter().zip(&oracle.results) {
        let quad = if let Some(world) = q.world {
            if q.vertices.is_some()
                || !oracle.meta_source_sha256.as_deref().is_some_and(valid_hash)
                || expected.vertices.is_none()
            {
                return Err("Invalid world receipt".into());
            }
            world_queries += 1;
            Quad::from_world(
                q.resolution,
                q.origin.ok_or("Missing origin")?,
                q.inverse_size.ok_or("Missing inverse size")?,
                world,
            )?
        } else {
            Quad::new(q.resolution, q.vertices.ok_or("Missing vertices")?)?
        };
        let [a, b, c, d] = quad.bounds();
        if (b - a) * (d - c) > 4096 || !valid_hash(&expected.sha256) {
            return Err("Invalid query bound or cell receipt".into());
        }
        let mut hash = Sha256::new();
        let mut count = 0;
        for z in c..d {
            for x in a..b {
                if quad.contains(x, z) {
                    hash.update((x as i32).to_le_bytes());
                    hash.update((z as i32).to_le_bytes());
                    count += 1;
                }
            }
        }
        if count != expected.count
            || format!("{:x}", hash.finalize()) != expected.sha256
            || expected.vertices.is_some_and(|v| v != quad.vertices())
        {
            mismatches += 1;
        }
    }
    let report = serde_json::json!({"format":"mapper-source-stamp-raster-audit-1","engine":papyrus_terrain::source_stamp_raster::VERSION,
        "input_sha256":oracle.input_sha256,"oracle_sha256":oracle.oracle_sha256,"source_sha256":oracle.source_sha256,
        "queries":input.queries.len(),"coverage_mismatches":mismatches,"world_to_index_queries":world_queries,
        "meta_source_sha256":oracle.meta_source_sha256,"world_to_index_validated":world_queries>0,
        "composition_integrated":false,"full_stamp_validated":false,"full_map_validated":false});
    serde_json::to_writer_pretty(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&args[2])?,
        &report,
    )?;
    println!("{report}");
    if mismatches > 0 {
        return Err("Coverage differs from owned source".into());
    }
    Ok(())
}
