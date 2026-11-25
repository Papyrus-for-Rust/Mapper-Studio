//! CAPTURE_DIRECTORY ROAD_INDEX TAIL_START NEW_REPORT. No game or GUI is started.
use papyrus_terrain::{path_geometry, path_terrain, road_entrance_contour};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
};
fn read(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("Diagnostic input exceeds limit".into());
    }
    Ok(bytes)
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 4 {
        return Err("CAPTURE_DIRECTORY ROAD_INDEX TAIL_START NEW_REPORT".into());
    }
    let root = Path::new(&args[0]);
    let output = Path::new(&args[3]);
    if output.exists() {
        return Err("Report must be new".into());
    }
    let document = read(&root.join("capture.json"), 4 * 1024 * 1024)?;
    let capture: Value = serde_json::from_slice(&document).map_err(|e| e.to_string())?;
    if !matches!(
        capture["format"].as_str(),
        Some("mapper-initial-road-grade-capture-1" | "mapper-initial-road-grade-capture-2")
    ) {
        return Err("Unsupported captured terrain".into());
    }
    let raw = read(&root.join("ground.i16le"), 64 * 1024 * 1024)?;
    if capture["files"]["ground.i16le"]["bytes"] != raw.len() as u64
        || capture["files"]["ground.i16le"]["sha256"] != hash(&raw)
    {
        return Err("Ground receipt differs".into());
    }
    let n = capture["resolution"]
        .as_u64()
        .filter(|v| (2..=4097).contains(v))
        .ok_or("Invalid resolution")? as usize;
    if raw.len() != n * n * 2 {
        return Err("Invalid ground raster length".into());
    }
    let ground: Vec<_> = raw
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    let scalar = |key: &str| {
        capture[key]
            .as_f64()
            .filter(|v| v.is_finite())
            .map(|v| v as f32)
            .ok_or("Missing finite grid scalar")
    };
    let grid = path_terrain::Grid {
        size: scalar("size")?,
        resolution: n,
        origin_y: scalar("origin_y")?,
        vertical_span: scalar("vertical_span")?,
        height: &ground,
    };
    let index: usize = args[1]
        .to_str()
        .ok_or("Invalid road index")?
        .parse()
        .map_err(|_| "Invalid road index")?;
    let tail_start: usize = args[2]
        .to_str()
        .ok_or("Invalid tail start")?
        .parse()
        .map_err(|_| "Invalid tail start")?;
    let controls: Vec<[f32; 3]> = serde_json::from_value(
        capture["roads"].get(index).ok_or("Missing road")?["points"].clone(),
    )
    .map_err(|e| e.to_string())?;
    let topology_bytes = read(&root.join("topology.i32le"), 64 * 1024 * 1024)?;
    if capture["files"]["topology.i32le"]["bytes"] != topology_bytes.len() as u64
        || capture["files"]["topology.i32le"]["sha256"] != hash(&topology_bytes)
    {
        return Err("Topology receipt differs".into());
    }
    let topology: Vec<_> = topology_bytes
        .chunks_exact(4)
        .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let mut pinned = vec![false; controls.len()];
    for p in pinned.iter_mut().take(tail_start + 1) {
        *p = true;
    }
    *pinned.last_mut().ok_or("Empty road")? = true;
    let result = road_entrance_contour::propose(
        road_entrance_contour::Input {
            controls: &controls,
            pinned: &pinned,
            tail_start,
            grid,
            topology: &topology,
            surface_resolution: capture["surface_resolution"]
                .as_u64()
                .ok_or("Missing surface resolution")? as usize,
            protected_topology: capture["protected_topology"]
                .as_i64()
                .ok_or("Missing protection")? as i32,
        },
        road_entrance_contour::Settings {
            maximum_grade: capture["settings"]["maximum_grade"]
                .as_f64()
                .ok_or("Missing grade limit")?,
            search_grade: 0.1,
            radius_cells: 48,
            maximum_expansions: 12000,
            maximum_work: 8_000_000,
            shaping: path_geometry::Settings {
                horizontal_iterations: 4,
                vertical_iterations: 16,
                resample_distance: 7.5,
                maximum_work: 1_000_000,
            },
        },
        || false,
    )
    .map_err(|e| format!("Contour: {e:?}"))?
    .ok_or("Cancelled")?;
    let proposed = result.proposal.as_ref();
    let report = json!({"format":"mapper-owned-entrance-contour-study-1","capture_sha256":hash(&document),"road":index,"tail_start":tail_start,"found":proposed.is_some(),"report":result.report,
        "original":controls,"route":proposed.map(|p|&p.controls),"raw":proposed.map(|p|&p.raw),"full_map_validated":false,"terrain_unchanged":true});
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| e.to_string())?
        .write_all(&serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    println!(
        "{}",
        json!({"found":proposed.is_some(),"report":result.report})
    );
    Ok(())
}
