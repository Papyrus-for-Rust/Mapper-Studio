//! Verify the mixed object runner against receipted local installation data.
use mapper_studio::{
    Project, decor_source, infrastructure,
    object_generation::{self, Settings, Sources},
    prefab_assets, rail_preview, road_object_placement, road_object_source,
};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Write, path::PathBuf};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().map(PathBuf::from).collect();
    if args.len() != 9 {
        return Err("usage: verify_object_generation PROJECT BUNDLE RUST_ROOT CATALOG INFRASTRUCTURE ROAD_SOURCE DECOR_SOURCE NEW_OUTPUT_DIRECTORY".into());
    }
    let output = &args[8];
    if output.exists() {
        return Err("Output already exists".into());
    }
    let parent = output
        .parent()
        .ok_or("Missing output parent")?
        .canonicalize()?;
    let destination = parent.join(output.file_name().ok_or("Invalid output directory")?);
    let input = args[2].canonicalize()?;
    if destination.starts_with(&input) || input.starts_with(&destination) {
        return Err("Output must be separate from input".into());
    }
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
    let infrastructure = infrastructure::Source::load(&args[5], &catalog, || false)?;
    let source = road_object_source::Source::load(&args[3], &args[6], &infrastructure, || false)?;
    let decoration = decor_source::Source::load(&args[3], &args[7], &catalog, || false)?;
    if project
        .roads
        .as_ref()
        .is_none_or(|r| r.source_sha256 != infrastructure.sha256)
    {
        return Err("Project road source differs from mixed source".into());
    }
    let terrain = rail_preview::Terrain::load(&args[2], &project, || false)?;
    let (layers, alpha) = terrain.bed_layers(&args[2], || false)?;
    let road_report = terrain.road_report(&args[2], || false)?;
    let roads = road_object_placement::roads_from_report(
        &road_report,
        infrastructure
            .profile
            .road_hierarchy
            .ok_or("Missing road hierarchy")?,
        infrastructure.profile.width,
    )?;
    let generated = object_generation::generate(
        &layers,
        &alpha,
        &roads,
        Sources {
            utilities: None,
            uniform_cliffs: None,
            jungle_vines: None,
            coastal_cliffs: None,
            early_cliff: None,
            height_reservations: None,
            road: &source,
            infrastructure: &infrastructure,
            decoration: &decoration,
            catalog: &catalog,
        },
        Settings {
            scenery_admission: object_generation::SceneryAdmission::Legacy,
            road: road_object_placement::Settings {
                seed: project.seeds[3].value,
                maximum_work: 2_000_000,
                maximum_poses: 100_000,
            },
            decoration_seed: project.seeds[5].value,
            realm_mask: 2,
            maximum_terrain_work: 256_000_000,
            maximum_sampling_attempts: 64_000_000,
            maximum_road_height_change: project
                .roads
                .as_ref()
                .ok_or("Missing road settings")?
                .maximum_cut_fill,
        },
        None,
        || false,
    )?
    .ok_or("Mixed generation cancelled")?;
    let report = serde_json::json!({"format":"papyrus-object-generation-verification","schema":1,"playable":false,
        "scope":"road and decoration handlers only; other procedural phases not executed; initial blocker raster empty",
        "engine":generated.engine,"input_manifest_sha256":terrain.manifest_digest(),"decoration_source_sha256":generated.decoration_source_sha256,
        "size":layers.size,"height_resolution":layers.height_resolution,"surface_resolution":layers.surface_resolution,
        "initial_blockers_sha256":generated.initial_blockers_sha256,"settings":generated.settings,
        "plan":generated.plan,"steps":generated.steps,"road_fitting":generated.road,"decoration":generated.decoration,
        "admissions":generated.admissions,"connectors":generated.connectors,"connector_connections_built":false,
        "sampling_attempts":generated.sampling_attempts,"transaction":generated.terrain.report,
        "assembled_instances":0,"physics_validated":false});
    let staging = tempfile::Builder::new()
        .prefix(".mixed-objects-")
        .tempdir_in(&parent)?;
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
        &generated
            .terrain
            .height
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>(),
    )?;
    write(
        "topology.i32le",
        &generated
            .terrain
            .topology
            .as_ref()
            .ok_or("Missing topology")?
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>(),
    )?;
    write(
        "splat.u8",
        generated
            .terrain
            .splat
            .as_ref()
            .ok_or("Missing materials")?,
    )?;
    write(
        "blocked.u8",
        &generated
            .terrain
            .report
            .blocking
            .as_ref()
            .ok_or("Missing blockers")?
            .raster,
    )?;
    write("report.json", &serde_json::to_vec_pretty(&report)?)?;
    let manifest = serde_json::json!({"format":"papyrus-object-generation-verification-manifest","schema":1,"playable":false,"files":files});
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(staging.path().join("manifest.json"))?;
    file.write_all(&serde_json::to_vec_pretty(&manifest)?)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(staging.path(), &destination)?;
    drop(staging);
    println!("{}", output.display());
    Ok(())
}
