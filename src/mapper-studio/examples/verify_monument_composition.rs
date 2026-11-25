//! Explicit local composition check using user-owned, verified asset caches.
use mapper_studio::{
    Project, composition, installation, native_preview, native_processing, prefab_assets,
    prefab_textures,
};
use papyrus_terrain::placement::{Outcome, Position, Solution};
use std::{path::Path, sync::atomic::AtomicBool, time::Instant};

fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 4 {
        return Err("verify_monument_composition <Rust root> <catalog> <bindings> <report>".into());
    }
    let root = Path::new(&args[0]);
    let project = Project {
        installation: Some(installation::inspect(root, || false)?),
        preview_engine: native_preview::VERSION.into(),
        composition_engine: Some(papyrus_terrain::monument_composition::VERSION.into()),
        ..Default::default()
    };
    let base = native_processing::build(
        root,
        Path::new("cached-terrain-worker-absent.exe"),
        &project.terrain_key(),
        None,
        &AtomicBool::new(false),
        |_, _| {},
    )?
    .ok_or("Cancelled")?;
    let catalog = prefab_assets::load(
        root,
        Path::new(&args[1]),
        project.installation.as_ref().unwrap(),
        || false,
    )?;
    let bindings = prefab_textures::Bindings::load(root, Path::new(&args[2]), &catalog, || false)?;
    // Deliberate diagnostic poses; these are not solver-approved playable locations.
    let positions = [[-800., 60., 0.], [500., 100., 500.], [1000., 70., -700.]];
    let solution = Solution {
        outcomes: project
            .targets
            .iter()
            .zip(positions)
            .map(|(t, p)| Outcome {
                id: t.instance_id,
                position: Some(Position {
                    x: p[0],
                    y: p[1],
                    z: p[2],
                    yaw: 37.,
                    clearance: 0.,
                }),
                distance: None,
                within_radius: false,
                reason: "Unvalidated diagnostic pose".into(),
            })
            .collect(),
        required_missing: 0,
        attempts: 0,
    };
    let started = Instant::now();
    let result = composition::compose_layout(
        &base.source,
        &project,
        &solution,
        &catalog,
        &bindings,
        || false,
    )?
    .ok_or("Cancelled")?;
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.;
    let changed_heights = result
        .layers
        .height
        .iter()
        .zip(&base.source.layers.height)
        .filter(|(a, b)| a != b)
        .count();
    let changed_splat = result
        .layers
        .splat
        .iter()
        .zip(&base.source.layers.splat)
        .filter(|(a, b)| a != b)
        .count();
    let changed_topology = result
        .layers
        .topology
        .iter()
        .zip(&base.source.layers.topology)
        .filter(|(a, b)| a != b)
        .count();
    let holes = result.alpha.iter().filter(|&&v| v < 128).count();
    if changed_heights == 0 || changed_splat == 0 || changed_topology == 0 {
        return Err("Real modifiers did not change the expected layers".into());
    }
    let cells = result.layers.surface_resolution.pow(2);
    if (0..cells).any(|i| {
        (0..8)
            .map(|c| u32::from(result.layers.splat[c * cells + i]))
            .sum::<u32>()
            != 255
    }) || (0..cells).any(|i| {
        (0..5)
            .map(|c| u32::from(result.layers.biome[c * cells + i]))
            .sum::<u32>()
            != 255
    }) {
        return Err("Composed weights are not normalized".into());
    }
    let again = composition::compose_layout(
        &base.source,
        &project,
        &solution,
        &catalog,
        &bindings,
        || false,
    )?
    .ok_or("Cancelled")?;
    if result.layers.height != again.layers.height
        || result.layers.splat != again.layers.splat
        || result.layers.biome != again.layers.biome
        || result.layers.topology != again.layers.topology
        || result.alpha != again.alpha
    {
        return Err("Composition is not repeatable".into());
    }
    let empty = Solution {
        outcomes: project
            .targets
            .iter()
            .map(|t| Outcome {
                id: t.instance_id,
                position: None,
                distance: None,
                within_radius: false,
                reason: "Removed for diagnostic".into(),
            })
            .collect(),
        required_missing: 0,
        attempts: 0,
    };
    let restored =
        composition::compose_layout(&base.source, &project, &empty, &catalog, &bindings, || {
            false
        })?
        .ok_or("Cancelled")?;
    if restored.layers.height != base.source.layers.height
        || restored.layers.splat != base.source.layers.splat
        || restored.layers.biome != base.source.layers.biome
        || restored.layers.topology != base.source.layers.topology
        || restored.alpha.iter().any(|&v| v != 255)
    {
        return Err("Removing stamps did not restore raw base".into());
    }
    let report = serde_json::json!({"algorithm":papyrus_terrain::monument_composition::VERSION,"catalog_sha256":catalog.sha256,"bindings_sha256":bindings.sha256,"poses":"diagnostic, not solver-approved or game-validated","yaw_degrees":37,"changed_height_samples":changed_heights,"changed_splat_bytes":changed_splat,"changed_topology_cells":changed_topology,"alpha_samples_below_128":holes,"height_samples_clamped":result.clamped_height_samples,"normalized_weights":true,"repeatable":true,"removal_restores_base":true,"composition_ms":elapsed_ms,"playable_export_enabled":false});
    std::fs::write(
        Path::new(&args[3]),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    println!(
        "Composed verified monument textures in {elapsed_ms:.1} ms; {changed_heights} height samples, {holes} hole samples"
    );
    Ok(())
}
