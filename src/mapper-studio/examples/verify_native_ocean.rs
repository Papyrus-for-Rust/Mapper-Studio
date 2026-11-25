//! Explicit local check of native source reuse and independent ocean reconstruction.
use mapper_studio::{Project, installation, native_preview, native_processing};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err("verify_native_ocean <Rust root> <worker> <report json>".into());
    }
    let root = Path::new(&args[0]);
    let cancel = AtomicBool::new(false);
    let project = Project {
        preview_engine: native_preview::VERSION.into(),
        installation: Some(installation::inspect(root, || false)?),
        ..Default::default()
    };
    let raw = native_processing::build(
        root,
        Path::new(&args[1]),
        &project.terrain_key(),
        None,
        &cancel,
        |_, _| {},
    )?
    .ok_or("Unexpected cancellation")?;
    let mut processed_project = project.clone();
    processed_project.preview_engine = native_preview::OCEAN_VERSION.into();
    processed_project.validate()?;
    let key = processed_project.terrain_key();
    let missing = Path::new("worker-must-not-be-needed-for-cached-processing.exe");
    let result = native_processing::build(
        root,
        missing,
        &key,
        Some(raw.source.clone()),
        &cancel,
        |_, _| {},
    )?
    .ok_or("Unexpected cancellation")?;
    let again = native_processing::build(
        root,
        missing,
        &key,
        Some(raw.source.clone()),
        &cancel,
        |_, _| {},
    )?
    .ok_or("Unexpected cancellation")?;
    let ocean = result.ocean.as_ref().ok_or("Missing ocean")?;
    if !result.reused_source
        || !Arc::ptr_eq(&raw.source.layers, &result.source.layers)
        || !Arc::ptr_eq(&raw.source.heights, &result.source.heights)
    {
        return Err("Native source was regenerated or copied".into());
    }
    if result.ocean != again.ocean {
        return Err("Ocean reconstruction is not repeatable".into());
    }
    if ocean
        .topology
        .iter()
        .zip(&raw.source.layers.topology)
        .any(|(a, b)| (a ^ b) & !papyrus_terrain::ocean::OCEAN_BIT != 0)
        || ocean.local_water.iter().any(|w| *w != 0)
    {
        return Err("Ocean processing changed unrelated flags or invented inland water".into());
    }
    let disabled = native_processing::build(
        root,
        missing,
        &project.terrain_key(),
        Some(raw.source.clone()),
        &cancel,
        |_, _| {},
    )?
    .ok_or("Unexpected cancellation")?;
    if disabled.ocean.is_some() || !Arc::ptr_eq(&disabled.source.layers, &raw.source.layers) {
        return Err("Disabling water changed raw terrain".into());
    }
    let stopped = native_processing::build(
        root,
        missing,
        &key,
        Some(raw.source.clone()),
        &cancel,
        |label, _| {
            if label == "Reconstructing boundary-connected ocean" {
                cancel.store(true, Ordering::Relaxed);
            }
        },
    )?;
    if stopped.is_some() {
        return Err("Cancelled water processing was published".into());
    }
    let report = serde_json::json!({"processing_engine":key.engine,"ocean_algorithm":papyrus_terrain::ocean::VERSION,"source_reused_without_worker":true,"source_buffers_shared":true,"repeatable":true,"unrelated_flags_preserved":true,"disabled_restores_raw":true,"cancelled_result_discarded":true,"surface_resolution":ocean.resolution,"ocean_cells":ocean.ocean_cells,"enclosed_cells":ocean.enclosed_cells,"topology_corrections":ocean.changed_topology_cells,"processing_bytes":ocean.bytes(),"source_bytes":raw.source.layers.bytes(),"ocean_ms":result.ocean_ms});
    std::fs::write(
        &args[2],
        serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    println!("{report}");
    Ok(())
}
