//! Replay bounded numeric roadside-decoration inputs without loading game code.
use papyrus_terrain::{decor_stream::Budget, path_geometry::Path, roadside_decor};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Road {
    hierarchy: u8,
    width: f32,
    spline: bool,
    points: Vec<[f32; 3]>,
    min_index: usize,
    max_index: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    format: String,
    settings: roadside_decor::Settings,
    roads: Vec<Road>,
    prefab_count: usize,
    seed: u32,
    maximum_work: usize,
    maximum_candidates: usize,
    accept_first: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err("INPUT EXPECTED_SHA256 NEW_OUTPUT_JSON".into());
    }
    let input_path = PathBuf::from(&args[0]);
    let output = PathBuf::from(&args[2]);
    if output.exists() {
        return Err("Existing roadside replay output was retained".into());
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1);
    }
    let file = options.open(input_path)?;
    let mut raw = Vec::new();
    file.take(64 * 1024 * 1024 + 1).read_to_end(&mut raw)?;
    if raw.len() > 64 * 1024 * 1024 {
        return Err("Roadside replay input exceeds 64 MiB".into());
    }
    let hash = format!("{:x}", Sha256::digest(&raw));
    if args[1].to_str() != Some(&hash) {
        return Err("Roadside replay input hash differs".into());
    }
    let input: Input = serde_json::from_slice(&raw)?;
    if input.format != "mapper-roadside-decoration-replay-1"
        || input.roads.len() > 65536
        || !(1..=100000).contains(&input.maximum_candidates)
        || input
            .roads
            .iter()
            .map(|road| road.points.len())
            .sum::<usize>()
            > 1048576
    {
        return Err("Invalid roadside replay format or bounds".into());
    }
    let mut budget = Budget::new(input.maximum_work)?;
    let mut geometry_work = 0_usize;
    let mut paths = Vec::new();
    for road in &input.roads {
        let remaining = input.maximum_work.saturating_sub(geometry_work);
        let mut path = Path::from_controls(&road.points, remaining, || false)
            .map_err(|error| format!("Roadside replay path failed {error:?}"))?
            .ok_or("Roadside replay path cancelled")?;
        path.set_range(road.min_index, road.max_index)
            .map_err(|error| format!("Roadside replay range failed {error:?}"))?;
        geometry_work += path.work();
        paths.push(path);
    }
    if geometry_work != 0 {
        budget = Budget::new(input.maximum_work.saturating_sub(geometry_work))?;
    }
    let roads: Vec<_> = input
        .roads
        .iter()
        .zip(&paths)
        .map(|(road, path)| roadside_decor::Road {
            hierarchy: road.hierarchy,
            width: road.width,
            spline: road.spline,
            path,
        })
        .collect();
    let mut candidates = Vec::new();
    let report = roadside_decor::sample(
        &roads,
        input.settings,
        input.prefab_count,
        input.seed,
        &mut budget,
        |candidate| {
            if candidates.len() == input.maximum_candidates {
                return Err("Roadside replay candidate allowance reached".into());
            }
            candidates.push(candidate);
            Ok(input.accept_first)
        },
        || false,
    )
    .map_err(|error| format!("Roadside replay sampling failed {error:?}"))?
    .ok_or("Roadside replay cancelled")?;
    let result = serde_json::json!({
        "engine":roadside_decor::VERSION,
        "input_sha256":hash,
        "geometry_work":geometry_work,
        "report":report,
        "candidates":candidates,
        "placement_and_terrain_effects_applied":false,
    });
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    serde_json::to_writer(&mut file, &result)?;
    file.sync_all()?;
    println!(
        "{}",
        serde_json::json!({"engine":roadside_decor::VERSION,"candidates":candidates.len(),"report":report})
    );
    Ok(())
}
