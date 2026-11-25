//! Explicit terrain-before-objects ring check using a source-verified Rust folder.
use mapper_studio::{Project, infrastructure, native_processing, prefab_assets, rail_source};
use papyrus_terrain::{rail_costmap, rail_ring};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path,
    sync::atomic::AtomicBool,
};
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 8 {
        return Err("verify_rail_ring PROJECT RUST_ROOT WORKER CATALOG INFRASTRUCTURE RAIL_SOURCE QUERY NEW_REPORT".into());
    }
    let paths: Vec<_> = args.iter().map(Path::new).collect();
    if paths[7].exists() {
        return Err("Choose a new report path".into());
    }
    let project = Project::load(paths[0])?;
    if !project.targets.is_empty()
        || project.decoration.is_some()
        || project.roads.is_some()
        || project.topology_edits.is_some()
    {
        return Err("This fixture must explicitly omit monuments, roads, decoration and manual topology edits; requested stages are not silently skipped".into());
    }
    let catalog = prefab_assets::load(
        paths[1],
        paths[3],
        project
            .installation
            .as_ref()
            .ok_or("Missing installation")?,
        || false,
    )?;
    let infra = infrastructure::Source::load(paths[4], &catalog, || false)?;
    if project
        .monument_assets
        .as_ref()
        .is_some_and(|p| p.catalog_sha256 != catalog.sha256)
    {
        return Err("Fixture catalog differs from its saved pin".into());
    }
    let source = rail_source::Source::load(paths[1], paths[5], &catalog, &infra, || false)?;
    let stage = source
        .stages
        .iter()
        .find(|s| s.kind == rail_source::Kind::GenerateRailRing)
        .ok_or("Missing ring stage")?;
    if let Some(skip) = source.skip(stage, project.size, false, true) {
        return Err(skip.into());
    }
    let policy = source
        .profile
        .ring
        .as_ref()
        .ok_or("Missing source ring policy")?;
    let mut raw = Vec::new();
    File::open(paths[6])
        .map_err(|e| e.to_string())?
        .take(16385)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() > 16384 {
        return Err("Ring query exceeds bound".into());
    }
    let settings: rail_ring::Settings = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    settings.validate()?;
    let native = native_processing::build(
        paths[1],
        paths[2],
        &project.terrain_key(),
        None,
        &AtomicBool::new(false),
        |stage, fraction| eprintln!("{:.0}% {stage}", fraction * 100.),
    )?
    .ok_or("Cancelled")?;
    let layers = &native.source.layers;
    // The fixture requested no objects. Its implemented static placement map is
    // explicitly empty; this is not a reconstructed full Unity placement map.
    let placement = vec![0; layers.surface_resolution.pow(2)];
    let grid = rail_costmap::build(
        rail_costmap::Terrain {
            size: layers.size as f32,
            height_resolution: layers.height_resolution,
            height: &layers.height,
            topology_resolution: layers.surface_resolution,
            topology: native
                .ocean
                .as_ref()
                .map_or(&layers.topology, |o| &o.topology),
            placement_resolution: layers.surface_resolution,
            placement: &placement,
        },
        &source.profile.costmap,
        128000000,
        || false,
    )?
    .ok_or("Cancelled")?;
    let ring = rail_ring::build(&grid, policy, settings, &[], || false)?;
    let report = serde_json::json!({"source_sha256":source.sha256,"terrain_key":project.terrain_key(),
        "fixture":"explicit terrain before objects; no requested monuments/roads/decoration/topology edits",
        "ocean_rebuilt":native.ocean.is_some(),
        "costmap":grid.audit(),"ring":ring,"settings":settings,"policy":policy,
        "terrain_applied":false,"curve_validated":false,"playable":false});
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(paths[7])
        .map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    println!("{}", report["ring"]["status"]);
    Ok(())
}
