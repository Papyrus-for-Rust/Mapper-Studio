//! Source-backed, windowless placement comparison. CONFIG NEW_REPORT.
use mapper_studio::{
    Project, monument_stages, native_processing, placement_preview, prefab_assets, prefab_textures,
};
use papyrus_terrain::placement::Solution;
use serde_json::{Value, json};
use std::{path::Path, sync::atomic::AtomicBool};
fn positions(solution: &Solution) -> Value {
    json!({"required_missing":solution.required_missing,"outcomes":solution.outcomes.iter().map(|o|
        json!({"id":o.id,"position":o.position.as_ref().map(|p|[p.x,p.y,p.z,p.yaw]),"reason":o.reason}))
        .collect::<Vec<_>>()})
}
fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        return Err("CONFIG NEW_REPORT".into());
    }
    let output = Path::new(&args[1]);
    if output.exists() {
        return Err("Report already exists".into());
    }
    let config: Value =
        serde_json::from_slice(&std::fs::read(&args[0]).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let path = |key: &str| {
        config[key]
            .as_str()
            .map(Path::new)
            .ok_or(format!("Missing {key}"))
    };
    let mut project = Project::load(path("project")?)?;
    let original = project.clone();
    let root = path("rust_root")?;
    let catalog = prefab_assets::load(
        root,
        path("catalog")?,
        project
            .installation
            .as_ref()
            .ok_or("Missing installation")?,
        || false,
    )?;
    let bindings = prefab_textures::Bindings::load(root, path("bindings")?, &catalog, || false)?;
    let processed = native_processing::build(
        root,
        path("worker")?,
        &project.terrain_key(),
        None,
        &AtomicBool::new(false),
        |label, _| eprintln!("{label}"),
    )?
    .ok_or("Cancelled")?;
    let baseline = placement_preview::solve_native(
        &project,
        &processed.source.layers,
        &catalog,
        &bindings,
        || false,
    )?
    .ok_or("Cancelled")?;
    let binding = project
        .monument_rules
        .as_mut()
        .and_then(|r| r.stage_rules.as_mut())
        .ok_or("Missing captured stage binding")?;
    if binding.spacing.is_some() {
        return Err("Expected a legacy recipe without source minimum spacing".into());
    }
    binding.spacing = Some(monument_stages::SPACING_VERSION.into());
    let source = monument_stages::Source::cached(binding, root, &catalog, || false)?;
    let shapes=project.targets.iter().filter(|t|t.prefab_path.is_some()).map(|t| {
        let p=t.prefab_path.as_ref().unwrap();
        let b=catalog.entries.get(p).and_then(|a|a.bounds.as_ref()).ok_or("Missing target bounds")?;
        let s=source.spacing(p,b)?;
        Ok(json!({"id":t.instance_id,"prefab":p,"center":s.shape.center,"extent":s.shape.extent,
            "family":s.family,"minimum_same":s.minimum_same,"minimum_different":s.minimum_different}))
    }).collect::<Result<Vec<_>,String>>()?;
    let enabled = placement_preview::solve_native(
        &project,
        &processed.source.layers,
        &catalog,
        &bindings,
        || false,
    )?
    .ok_or("Cancelled")?;
    let repeated = placement_preview::solve_native(
        &project,
        &processed.source.layers,
        &catalog,
        &bindings,
        || false,
    )?
    .ok_or("Cancelled")?;
    if enabled != repeated || project.terrain_key() != original.terrain_key() {
        return Err("Spacing replay or terrain identity changed".into());
    }
    let restored = placement_preview::solve_native(
        &original,
        &processed.source.layers,
        &catalog,
        &bindings,
        || false,
    )?
    .ok_or("Cancelled")?;
    if baseline != restored {
        return Err("Legacy placement changed after spacing run".into());
    }
    let report = json!({"format":"mapper-monument-minimum-spacing-study","schema":1,
        "source_sha256":source.sha256,"policy":monument_stages::SPACING_VERSION,
        "project":project,"shapes":shapes,"baseline":positions(&baseline),"enabled":positions(&enabled),
        "repeatable":true,"legacy_replayed":true,"terrain_identity_preserved":true,
        "scope":"Symmetric source minimum OBB edge distance in independent upright packing; stage count/scoring/relaxation and runtime physics incomplete",
        "game_runtime_validated":false});
    mapper_studio::profile::write_document(output, &report, false)?;
    println!(
        "{}",
        json!({"report":output,"baseline_required_missing":baseline.required_missing,"enabled_required_missing":enabled.required_missing})
    );
    Ok(())
}
