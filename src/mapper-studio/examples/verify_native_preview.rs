//! Explicit local integration check; no game libraries are redistributed.
use mapper_studio::{Project, installation, native_preview};
use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err("verify_native_preview <Rust root> <worker exe> <report json>".into());
    }
    let root = Path::new(&args[0]);
    let worker = Path::new(&args[1]);
    let project = Project {
        installation: Some(installation::inspect(root, || false)?),
        preview_engine: native_preview::VERSION.into(),
        ..Default::default()
    };
    project.validate()?;
    let key = project.terrain_key();
    let cancel = AtomicBool::new(false);
    let mut received = 0;
    let a = native_preview::generate_with_snapshots(
        root,
        worker,
        &key,
        &cancel,
        true,
        |_, _, image| {
            if let Some(image) = image {
                assert_eq!(image.size, [129, 129]);
                received += 1;
            }
        },
    )?
    .ok_or("Unexpected cancellation")?;
    if received == 0 || a.metrics["snapshotCount"] != 3 {
        return Err("Native stage previews missing".into());
    }
    let b = native_preview::generate(root, worker, &key, &cancel, |_, _| {})?
        .ok_or("Unexpected cancellation")?;
    if b.metrics["snapshotCount"] != 0 {
        return Err("Disabled previews still generated snapshots".into());
    }
    if a.heights != b.heights
        || a.image != b.image
        || a.layers.height != b.layers.height
        || a.layers.biome != b.layers.biome
        || a.layers.splat != b.layers.splat
        || a.layers.topology != b.layers.topology
    {
        return Err("Native repeatability failed".into());
    }
    let mut different = project.clone();
    different.seeds[1].inherited = false;
    different.seeds[1].value = 987654;
    let climate =
        native_preview::generate(root, worker, &different.terrain_key(), &cancel, |_, _| {})?
            .ok_or("Unexpected cancellation")?;
    if a.heights != climate.heights
        || a.image == climate.image
        || a.layers.height != climate.layers.height
        || a.layers.biome == climate.layers.biome
    {
        return Err("Independent native biome seed check failed".into());
    }
    if native_preview::generate(
        root,
        &std::env::current_exe().map_err(|e| e.to_string())?,
        &key,
        &cancel,
        |_, _| {},
    )
    .is_ok()
    {
        return Err("Failed companion worker was accepted".into());
    }
    let mut mismatched = key.clone();
    if let Some(manifest) = &mut mismatched.source.as_mut().unwrap().scene_manifest {
        manifest.sha256 = "a".repeat(64);
    }
    if mismatched == key {
        return Err("This integration fixture needs the scene manifest".into());
    }
    if native_preview::generate(root, worker, &mismatched, &cancel, |_, _| {}).is_ok() {
        return Err("Source mismatch was accepted".into());
    }
    let stopped = native_preview::generate(root, worker, &key, &cancel, |label, _| {
        if label == "Native terrain" {
            cancel.store(true, Ordering::Relaxed);
        }
    })?;
    if stopped.is_some() || !cancel.load(Ordering::Relaxed) {
        return Err("Running native cancellation failed".into());
    }
    cancel.store(false, Ordering::Relaxed);
    let stopped = native_preview::generate_with_snapshots(
        root,
        worker,
        &key,
        &cancel,
        true,
        |_, _, image| {
            if image.is_some() {
                cancel.store(true, Ordering::Relaxed);
            }
        },
    )?;
    if stopped.is_some() || !cancel.load(Ordering::Relaxed) {
        return Err("Snapshot cancellation failed".into());
    }
    let report = serde_json::json!({ "adapter": native_preview::VERSION, "received_snapshots": received, "snapshot_cancelled": true, "live_and_disabled_full_layers_equal": true, "full_layer_transport": papyrus_terrain::native_layers::VERSION, "retained_bytes":a.layers.bytes(), "normalized_biome_cells":a.layers.normalized_biome_cells, "normalized_splat_cells":a.layers.normalized_splat_cells, "repeatable_full_layers": true, "biome_seed_retains_full_heights": true, "biome_seed_changes_image": true, "source_mismatch_rejected": true, "worker_failure_rejected": true, "running_cancellation_passed": true, "first_run": a.metrics, "repeat_run": b.metrics, "climate_run": climate.metrics });
    std::fs::write(
        &args[2],
        serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    println!("{}", report);
    Ok(())
}
