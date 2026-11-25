//! Fit captured river-object rules against an owned pre-monument river world.
use mapper_studio::{
    Project, infrastructure, installation, native_processing, prefab_assets,
    road_object_placement::{self, Road, Settings},
    road_object_source::{PathFamily, Source},
};
use serde_json::json;
use std::{io::Write, path::Path, sync::atomic::AtomicBool};

fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 7 {
        return Err("Expected RUST_ROOT CATALOG INFRASTRUCTURE RIVER_OBJECT_SOURCE PROJECT WORKER NEW_REPORT".into());
    }
    let root = Path::new(&args[0]);
    let output = Path::new(&args[6]);
    if output.exists() {
        return Err("River object report must be new".into());
    }
    let _verification = installation::VerificationScope::default();
    let installation = installation::inspect(root, || false)?;
    let catalog = prefab_assets::load(root, Path::new(&args[1]), &installation, || false)?;
    let parent = infrastructure::Source::load(Path::new(&args[2]), &catalog, || false)?;
    let source = Source::load(root, Path::new(&args[3]), &parent, || false)?;
    if source.path_family != PathFamily::River {
        return Err("River verification requires a river-family source".into());
    }
    let project = Project::load(Path::new(&args[4]))?;
    let processed = native_processing::build(
        root,
        Path::new(&args[5]),
        &project.terrain_key(),
        None,
        &AtomicBool::new(false),
        |_, _| {},
    )?
    .ok_or("River terrain preparation cancelled")?;
    let world = processed
        .source
        .rivers
        .as_ref()
        .ok_or("Fixture has no river world")?;
    let paths = world
        .paths
        .iter()
        .map(|path| Road {
            hierarchy: 0,
            width: world.layout_policy.width,
            start: true,
            end: true,
            points: path.points().to_vec(),
        })
        .collect::<Vec<_>>();
    let alpha = vec![255; processed.source.layers.surface_resolution.pow(2)];
    let settings = Settings {
        seed: processed
            .source
            .key
            .rivers
            .as_ref()
            .ok_or("Missing river key")?
            .seed,
        maximum_work: 2_000_000,
        maximum_poses: 100_000,
    };
    let run = || {
        road_object_placement::fit(
            &processed.source.layers,
            &alpha,
            &paths,
            &source,
            settings,
            || false,
        )
    };
    let report = run()?.ok_or("River object fitting cancelled")?;
    let repeated = run()?.ok_or("Repeated river fitting cancelled")?;
    if report.budget_exhausted
        || report.provisional_poses.is_empty()
        || serde_json::to_vec(&report).map_err(|e| e.to_string())?
            != serde_json::to_vec(&repeated).map_err(|e| e.to_string())?
        || road_object_placement::fit(
            &processed.source.layers,
            &alpha,
            &paths,
            &source,
            settings,
            || true,
        )?
        .is_some()
    {
        return Err("River fitting failed completeness, determinism or cancellation checks".into());
    }
    let environments = source
        .environments
        .as_ref()
        .ok_or("River environment roster is missing")?;
    let environment_volumes: usize = environments.values().map(|e| e.volumes.len()).sum();
    let result = json!({"format":"mapper-river-object-fitting-verification","schema":1,
        "source_sha256":source.sha256,"infrastructure_sha256":parent.sha256,
        "stages":source.stages,"prefabs":source.prefabs.len(),"environment_volumes":environment_volumes,
        "river_paths":paths.len(),"report":report,"deterministic":true,"cancellation_retains_input":true,
        "normal_generation_integrated":false,"game_runtime_validated":false,
        "terrain_effects_published":false,"records_exported":false});
    let mut file = std::fs::File::create_new(output).map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(&result).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    println!(
        "Verified {} rivers and {} fitted objects",
        paths.len(),
        report.provisional_poses.len()
    );
    Ok(())
}
