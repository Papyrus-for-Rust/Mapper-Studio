//! Local, asset-free diagnostic driver. Inputs must be verified installed caches.
use mapper_studio::{Project, decor_source, prefab_assets, rail_preview};
use papyrus_terrain::{
    decor_fit::{OrientedBudget, ValidatedWater, Water},
    decor_live::{self, Context},
    decor_stream::Budget,
    footprints::{ObjectPolicy, RoadTransaction},
};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Write, path::PathBuf};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().map(PathBuf::from).collect();
    if !(7..=9).contains(&args.len()) {
        return Err("usage: verify_live_decoration PROJECT BUNDLE RUST_ROOT CATALOG DECOR_SOURCE NEW_OUTPUT_DIRECTORY [STAGE_INDICES_CSV] [MAXIMUM_TERRAIN_WORK]".into());
    }
    let maximum_work = args
        .get(8)
        .map(|p| {
            p.to_str()
                .ok_or("Non-text work allowance")?
                .parse::<usize>()
                .map_err(|_| "Invalid work allowance")
        })
        .transpose()?
        .unwrap_or(128_000_000);
    ObjectPolicy {
        maximum_work,
        role_exceptions: 0,
        material_role_exceptions: 0,
    }
    .validate()?;
    let project = Project::load(&args[1])?;
    let catalog = prefab_assets::load(
        &args[3],
        &args[4],
        project
            .installation
            .as_ref()
            .ok_or("Missing installation")?,
        || false,
    )?;
    let source = decor_source::Source::load(&args[3], &args[5], &catalog, || false)?;
    let terrain = rail_preview::Terrain::load(&args[2], &project, || false)?;
    let (layers, alpha) = terrain.bed_layers(&args[2], || false)?;
    let selected: Vec<usize> = if args.len() >= 8 {
        args[7]
            .to_str()
            .ok_or("Non-text stage selection")?
            .split(',')
            .map(str::parse)
            .collect::<Result<_, _>>()?
    } else {
        vec![0]
    };
    if selected.is_empty()
        || selected.windows(2).any(|w| w[0] >= w[1])
        || selected.iter().any(|i| *i >= source.stages.len())
    {
        return Err("Select ordered unique source-stage indices".into());
    }
    let output = &args[6];
    if output.exists() {
        return Err("Output directory already exists".into());
    }
    let parent = output
        .parent()
        .ok_or("Missing output parent")?
        .canonicalize()?;
    let destination = parent.join(output.file_name().ok_or("Invalid output directory")?);
    let input = args[2].canonicalize()?;
    if destination.starts_with(&input) || input.starts_with(&destination) {
        return Err("Diagnostic output must be separate from the input bundle".into());
    }
    let staging = tempfile::Builder::new()
        .prefix(".live-decor-")
        .tempdir_in(&parent)?;
    let prepared = source.prepare_live(&catalog)?;
    let ocean = papyrus_terrain::ocean::reconstruct(
        &layers.height,
        layers.height_resolution,
        &layers.topology,
        layers.surface_resolution,
        || false,
    )?
    .ok_or("Ocean query cancelled")?;
    let water = ValidatedWater::new(
        Water {
            local_heights: &ocean.local_water,
            resolution: layers.height_resolution,
            ocean_mask: prepared.ocean_mask,
            ocean_level: 0.,
            terrain_minimum: -500.,
        },
        &layers,
    )?;
    let mut transaction = RoadTransaction::new_objects_with_policy(
        &layers,
        &alpha,
        prepared.terrain,
        prepared.known_topology,
        prepared.protected_topology,
        ObjectPolicy {
            maximum_work,
            role_exceptions: prepared.placement_role_exceptions,
            material_role_exceptions: prepared.placement_material_role_exceptions,
        },
        || false,
    )?
    .ok_or("Transaction cancelled")?;
    let mut fitting = OrientedBudget::new(&layers, &alpha)?;
    let mut attempts = Budget::new(64_000_000)?;
    let mut next_pose = 0;
    let mut reports = Vec::new();
    for &index in &selected {
        let started = std::time::Instant::now();
        reports.push(
            decor_live::emit(
                &source.stages[index],
                index,
                project.seeds[5].value,
                2,
                Context {
                    transaction: &mut transaction,
                    alpha: &alpha,
                    fitting: &mut fitting,
                    prefabs: &prepared.fitting,
                    attempts: &mut attempts,
                    next_pose: &mut next_pose,
                    water: Some(water),
                },
                || false,
            )
            .map_err(|error| {
                format!(
                    "Decoration stage {index} ({}) failed after {} completed stages, {} sampling attempts, {next_pose} admission attempts and {} terrain work: {error}",
                    source.stages[index].name,
                    reports.len(),
                    attempts.used(),
                    transaction.sample_visits(),
                )
            })?
            .ok_or("Decoration phase cancelled")?,
        );
        eprintln!(
            "Completed stage {index} ({}) in {:.3}s: {} attempts, {} admissions attempted, {} terrain work (cumulative)",
            source.stages[index].name,
            started.elapsed().as_secs_f64(),
            attempts.used(),
            next_pose,
            transaction.sample_visits()
        );
    }
    let result = transaction
        .finish(|| false)?
        .ok_or("Final transaction cancelled")?;
    let summary = serde_json::json!({"format":"papyrus-live-decoration-verification","schema":1,"playable":false,"scope":"selected static stages only; road objects and full procedural workflow not executed","source_sha256":source.sha256,"input_manifest_sha256":terrain.manifest_digest(),"size":layers.size,"height_resolution":layers.height_resolution,"surface_resolution":layers.surface_resolution,"seed":project.seeds[5].value,"selected_stages":selected,"attempts":attempts.used(),"surface_samples":fitting.samples(),"height_samples":fitting.height_samples(),"next_admission_id":next_pose,"reports":reports,"transaction":result.report});
    let mut files = BTreeMap::new();
    let mut write = |name: &str, bytes: &[u8]| -> Result<(), Box<dyn std::error::Error>> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(staging.path().join(name))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        files.insert(
            name.to_owned(),
            serde_json::json!({"bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(bytes))}),
        );
        Ok(())
    };
    write(
        "height.i16le",
        &result
            .height
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>(),
    )?;
    write(
        "topology.i32le",
        &result
            .topology
            .ok_or("Missing topology")?
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>(),
    )?;
    write("splat.u8", &result.splat.ok_or("Missing splats")?)?;
    write("report.json", &serde_json::to_vec_pretty(&summary)?)?;
    let mut manifest = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(staging.path().join("manifest.json"))?;
    manifest.write_all(&serde_json::to_vec_pretty(&serde_json::json!({"format":"papyrus-live-decoration-diagnostic","schema":1,"playable":false,"files":files}))?)?;
    manifest.sync_all()?;
    drop(manifest);
    std::fs::rename(staging.path(), &destination)?;
    println!("{}", output.display());
    Ok(())
}
