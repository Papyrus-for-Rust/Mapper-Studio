//! Local diagnostic; reads user-owned verified assets, never writes a playable map.
use mapper_studio::{
    AssetIdentity, Project, composition, installation, native_preview, native_processing,
    placement_preview, prefab_assets, prefab_textures, profile,
};
use std::{path::Path, sync::atomic::AtomicBool, time::Instant};
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 4 {
        return Err("verify_earthwork_placement <Rust root> <catalog> <bindings> <report>".into());
    }
    let root = Path::new(&args[0]);
    let mut project = Project {
        installation: Some(installation::inspect(root, || false)?),
        preview_engine: native_preview::VERSION.into(),
        ..Default::default()
    };
    let raw = native_processing::build(
        root,
        &native_preview::worker_path()?,
        &project.terrain_key(),
        None,
        &AtomicBool::new(false),
        |label, _| eprintln!("{label}"),
    )?
    .ok_or("Cancelled")?;
    eprintln!("Verifying source-bound catalog and texture cache");
    let catalog = prefab_assets::load(
        root,
        Path::new(&args[1]),
        project.installation.as_ref().unwrap(),
        || false,
    )?;
    let bindings = prefab_textures::Bindings::load(root, Path::new(&args[2]), &catalog, || false)?;
    let legacy = placement_preview::solve(
        &project,
        &raw.source.heights,
        native_preview::RESOLUTION,
        || false,
    )?
    .ok_or("Cancelled")?;
    let terrain_key = project.terrain_key();
    project.placement_engine = placement_preview::EARTHWORK_VERSION.into();
    project.monument_assets = Some(AssetIdentity {
        catalog_sha256: catalog.sha256.clone(),
        bindings_sha256: bindings.sha256.clone(),
    });
    eprintln!("Searching authored earthwork");
    let start = Instant::now();
    let solution =
        placement_preview::solve_native(&project, &raw.source.layers, &catalog, &bindings, || {
            false
        })?
        .ok_or("Cancelled")?;
    let elapsed = start.elapsed().as_secs_f64() * 1000.;
    let again =
        placement_preview::solve_native(&project, &raw.source.layers, &catalog, &bindings, || {
            false
        })?
        .ok_or("Cancelled")?;
    if solution != again {
        return Err("Search was not repeatable".into());
    }
    let mut pinned = project.clone();
    pinned.targets.retain_mut(|target| {
        let position = solution
            .outcomes
            .iter()
            .find(|o| o.id == target.instance_id)
            .and_then(|o| o.position.as_ref());
        if let Some(p) = position {
            target.x = p.x;
            target.z = p.z;
            target.yaw = Some(p.yaw);
            target.exact = true;
            true
        } else {
            false
        }
    });
    let exact =
        placement_preview::solve_native(&pinned, &raw.source.layers, &catalog, &bindings, || {
            false
        })?
        .ok_or("Cancelled")?;
    if exact.required_missing != 0
        || exact.outcomes.iter().any(|o| {
            o.position
                != solution
                    .outcomes
                    .iter()
                    .find(|a| a.id == o.id)
                    .unwrap()
                    .position
        })
    {
        return Err("Exact placement did not reproduce accepted poses".into());
    }
    let composed = composition::compose_layout(
        &raw.source,
        &project,
        &solution,
        &catalog,
        &bindings,
        || false,
    )?
    .ok_or("Cancelled")?;
    let changed = composed
        .layers
        .height
        .iter()
        .zip(&raw.source.layers.height)
        .filter(|(a, b)| a != b)
        .count();
    if project.terrain_key() != terrain_key {
        return Err("Placement changed base identity".into());
    }
    let outcomes: Vec<_> = solution.outcomes.iter().map(|o|serde_json::json!({"id":o.id,"placed":o.position.is_some(),"position":o.position.as_ref().map(|p|[p.x,p.y,p.z,p.yaw]),"reason":o.reason})).collect();
    let report = serde_json::json!({"engine":placement_preview::EARTHWORK_VERSION,"legacy_required_missing":legacy.required_missing,"earthwork_required_missing":solution.required_missing,"outcomes":outcomes,"search_ms":elapsed,"repeatable":true,"exact_poses_reproduced":true,"base_identity_preserved":true,"changed_height_samples":changed,"clamped_height_samples":composed.clamped_height_samples,"terrain_cache":raw.cache_status,"monument_assets":project.monument_assets,"playable":false,"game_validated":false});
    profile::write_document(Path::new(&args[3]), &report, true)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
    );
    Ok(())
}
