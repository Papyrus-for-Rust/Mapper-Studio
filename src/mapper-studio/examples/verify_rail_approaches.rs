//! Native monument/composition/ring/approach diagnostic using verified assets.
//! Publishes a preview bundle and separate report; never a playable map.
use mapper_studio::{Project, headless, infrastructure, prefab_assets, rail_preview, rail_source};
use papyrus_terrain::{rail_approaches, rail_costmap, rail_ring};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path,
    sync::atomic::AtomicBool,
};
fn query<T: DeserializeOwned>(path: &Path) -> Result<T, String> {
    let mut raw = Vec::new();
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(16385)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() > 16384 {
        return Err("Query exceeds 16 KiB".into());
    }
    serde_json::from_slice(&raw).map_err(|e| e.to_string())
}
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 11 {
        return Err("verify_rail_approaches PROJECT RUST_ROOT WORKER CATALOG BINDINGS INFRASTRUCTURE RAIL_SOURCE RING_QUERY APPROACH_QUERY NEW_BUNDLE NEW_REPORT".into());
    }
    let paths: Vec<_> = args.iter().map(Path::new).collect();
    if paths[9].exists() || paths[10].exists() {
        return Err("Choose new bundle/report paths".into());
    }
    let project = Project::load(paths[0])?;
    if project.decoration.is_some() || project.roads.is_some() || project.topology_edits.is_some() {
        return Err("This fixture explicitly runs monuments before roads, decoration or manual edits; no requested stages are silently skipped".into());
    }
    let ring_settings: rail_ring::Settings = query(paths[7])?;
    ring_settings.validate()?;
    let approach_settings: rail_approaches::Settings = query(paths[8])?;
    approach_settings.validate()?;
    let catalog = prefab_assets::load(
        paths[1],
        paths[3],
        project.installation.as_ref().ok_or("No installation")?,
        || false,
    )?;
    let infra = infrastructure::Source::load(paths[5], &catalog, || false)?;
    let source = rail_source::Source::load(paths[1], paths[6], &catalog, &infra, || false)?;
    for kind in [
        rail_source::Kind::GenerateRailRing,
        rail_source::Kind::GenerateRailLayout,
    ] {
        let stage = source
            .stages
            .iter()
            .find(|s| s.kind == kind)
            .ok_or("Missing required rail stage")?;
        if let Some(skip) = source.skip(stage, project.size, false, true) {
            return Err(skip.into());
        }
    }
    let ring_policy = source
        .profile
        .ring
        .as_ref()
        .ok_or("No source ring adapter")?;
    let cancel = AtomicBool::new(false);
    let generated = headless::generate(
        &project,
        &headless::Options {
            rust_root: Some(paths[1].to_owned()),
            worker: Some(paths[2].to_owned()),
            catalog: Some(paths[3].to_owned()),
            bindings: Some(paths[4].to_owned()),
            skip_composition: false,
        },
        &cancel,
        |stage, f| eprintln!("{:.0}% {stage}", f * 100.),
    )?
    .ok_or("Cancelled")?;
    let manifest = headless::save_bundle(&project, &generated, paths[9], &cancel)?;
    let layout_raw = std::fs::read(paths[9].join("layout.json")).map_err(|e| e.to_string())?;
    if manifest["files"]["layout.json"]["sha256"] != format!("{:x}", Sha256::digest(&layout_raw)) {
        return Err("Saved layout receipt mismatch".into());
    }
    let sockets = rail_preview::layout_sockets(
        &project,
        &infra,
        source.profile.rail_type,
        &layout_raw,
        || false,
    )?;
    let maps = generated
        .composed
        .as_ref()
        .ok_or("No composed monument terrain")?;
    let layers = &maps.layers;
    // No decoration was requested. This explicit empty static blocker subset
    // does not reconstruct Unity's complete runtime placement map.
    let placement = vec![0; layers.surface_resolution.pow(2)];
    let grid = rail_costmap::build(
        rail_costmap::Terrain {
            size: layers.size as f32,
            height_resolution: layers.height_resolution,
            height: &layers.height,
            topology_resolution: layers.surface_resolution,
            topology: &layers.topology,
            placement_resolution: layers.surface_resolution,
            placement: &placement,
        },
        &source.profile.costmap,
        128000000,
        || false,
    )?
    .ok_or("Cancelled")?;
    let protected = sockets
        .sockets
        .iter()
        .map(|s| [s.position[0], s.position[2]])
        .collect::<Vec<_>>();
    let ring = rail_ring::build(&grid, ring_policy, ring_settings, &protected, || false)?;
    let approaches =
        rail_preview::approach_report(&grid, &source, &sockets, &ring, approach_settings, || {
            false
        })?;
    let report = serde_json::json!({"source_sha256":source.sha256,"costmap":grid.audit(),"ring":ring,
        "required_monuments_missing":generated.solution.required_missing,"approaches":approaches,
        "bundle_manifest_sha256":format!("{:x}",Sha256::digest(std::fs::read(paths[9].join("manifest.json")).map_err(|e|e.to_string())?)),
        "placement_scope":"Explicit empty decoration blocker subset; complete runtime placement map unavailable",
        "terrain_applied":false,"playable":false});
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(paths[10])
        .map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    println!("{}", report["approaches"]["result"]["status"]);
    Ok(())
}
