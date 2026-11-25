//! Run captured pipe prefabs at the road-terrain/texture boundary on a flat fixture.
use mapper_studio::{
    Project, infrastructure, installation, prefab_assets, road_object_source, road_paint_source,
    road_paint_stage, road_terrain_source, water_treatment_source, water_treatment_stage,
};
use papyrus_terrain::{
    decor_fit, decor_live,
    decor_stream::Budget,
    erosion, footprints,
    native_layers::{self, Layers},
    path_geometry::Path,
    road_paint, roadside_decor, roadside_live,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs::OpenOptions, io::Write, path::Path as FilePath};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 7
        && !((args.len() == 9 || (args.len() == 11 && args[9] == "--tag-catalog"))
            && args[7] == "--tags")
    {
        return Err(
            "RUST_ROOT CATALOG PROJECT INFRASTRUCTURE ROAD_OBJECTS PIPE_CAPTURE NEW_OUTPUT_JSON [--tags TAG_CAPTURE [--tag-catalog CATALOG]]"
                .into(),
        );
    }
    let output = FilePath::new(&args[6]);
    if output.exists() {
        return Err("Existing road-phase verification was retained".into());
    }
    let _verification = installation::VerificationScope::default();
    let root = FilePath::new(&args[0]);
    let project = Project::load(FilePath::new(&args[2]))?;
    let catalog = prefab_assets::load(
        root,
        FilePath::new(&args[1]),
        project
            .installation
            .as_ref()
            .ok_or("Missing installation")?,
        || false,
    )?;
    let infrastructure = infrastructure::Source::load(FilePath::new(&args[3]), &catalog, || false)?;
    let road_source =
        road_object_source::Source::load(root, FilePath::new(&args[4]), &infrastructure, || false)?;
    if project
        .roads
        .as_ref()
        .is_none_or(|recipe| recipe.source_sha256 != infrastructure.sha256)
        || project
            .objects
            .as_ref()
            .is_none_or(|recipe| recipe.road_source_sha256 != road_source.sha256)
    {
        return Err("Road-phase source differs from the saved project".into());
    }
    let flags = infrastructure
        .profile
        .topology_flags
        .as_ref()
        .ok_or("Missing topology roles")?;
    // This fixture isolates the pipe's authored terrain effects from world blockers.
    let protected = *flags.get("Monument").ok_or("Missing monument topology")?;
    let source = water_treatment_source::Source::load(
        root,
        FilePath::new(&args[5]),
        &catalog,
        &road_source,
        flags,
        protected,
        || false,
    )?;
    if source.programs().len() != 1 {
        return Err("Fixture requires one pipe program".into());
    }
    let program = &source.programs()[0];
    let tag_catalog = if args.len() == 11 {
        Some(prefab_assets::load(
            root,
            FilePath::new(&args[10]),
            &catalog.installation,
            || false,
        )?)
    } else {
        None
    };
    let registry = if args.len() >= 9 {
        let tags = mapper_studio::monument_tag_source::Source::load(
            root,
            FilePath::new(&args[8]),
            tag_catalog.as_ref().unwrap_or(&catalog),
            || false,
        )?;
        let mut plant = None;
        for (path, prefab) in tags.prefabs() {
            if !prefab
                .matching_monuments(water_treatment_stage::MONUMENT_TOKEN)?
                .is_empty()
            {
                plant = Some(path.clone());
                break;
            }
        }
        let mut controlled = project.clone();
        controlled.targets.clear();
        let layout = papyrus_terrain::placement::Solution {
            outcomes: Vec::new(),
            required_missing: 0,
            attempts: 0,
        };
        let roadside = [mapper_studio::roadside_stamps::Placement {
            request: 1000007,
            prefab: plant.ok_or("Missing captured plant")?,
            position: [400., 20., 400.],
            yaw: 37.,
            road: 0,
            start_index: 0,
            end_index: 1,
        }];
        Some((
            water_treatment_stage::registry(&controlled, &layout, &roadside, &tags, || false)?
                .ok_or("Controlled registry cancelled")?,
            tags.sha256().to_owned(),
        ))
    } else {
        None
    };
    let identities = if registry.is_some() {
        Some(
            mapper_studio::prefab_identities::Source::prepare(root, &catalog, || false)?
                .ok_or("Prefab identity preparation cancelled")?,
        )
    } else {
        None
    };
    let raw = match catalog.installation.native.sha256.as_str() {
        "8dd4e23570729ddbd72844bdb8a5e04bfa671612b3e7d34841d17d2353f7015c" => {
            include_str!("../../../compatibility/infrastructure/client-8dd4e235-road-paint.json")
        }
        "0f703d43b57672b8bb3dfd29bab0d484f8622fbac66dee7f1f058893ef22a99c" => {
            include_str!("../../../compatibility/infrastructure/client-0f703d43-road-paint.json")
        }
        _ => return Err("No inspected road-paint fixture policy".into()),
    };
    let paint: road_paint_source::Profile = serde_json::from_str(raw)?;
    let terrain =
        road_terrain_source::Profile::for_paint(&paint)?.ok_or("Missing road terrain policy")?;
    let (hr, n) = native_layers::dimensions(1000)?;
    let cells = n * n;
    let mut splat = vec![0; cells * 8];
    splat[..cells].fill(255);
    let layers = Layers {
        size: 1000,
        terrain_seed: 1,
        biome_seed: 2,
        height_resolution: hr,
        surface_resolution: n,
        height: vec![17038; hr * hr],
        topology: vec![*flags.get("Field").ok_or("Missing field topology")? as i32; cells],
        splat,
        biome: vec![51; cells * 5],
        normalized_biome_cells: cells,
        normalized_splat_cells: cells,
    };
    let original = (
        layers.height.clone(),
        layers.topology.clone(),
        layers.splat.clone(),
    );
    let alpha = vec![255; cells];
    let level = erosion::decode(17038);
    let path = Path::from_controls(&[[0., level, -500.], [0., level, 500.]], 10000, || false)
        .map_err(|error| format!("Fixture path failed {error:?}"))?
        .ok_or("Fixture cancelled")?;
    let roads = [road_paint::Road {
        path: &path,
        hierarchy: 0,
    }];
    let input = || road_paint_stage::Input {
        layers: &layers,
        roads: &roads,
        junctions: &[],
        paint: &paint,
        terrain: &terrain,
    };
    let baseline = road_paint_stage::paint(
        input(),
        32_000_000,
        |p| Ok(Some(vec![0.; p.len()])),
        |_| Ok(Some(terrain.water_no_hit_level)),
        || false,
    )?
    .ok_or("Baseline cancelled")?;
    let fitting = source
        .prefabs()
        .iter()
        .map(|(path, prefab)| (path.clone(), prefab.fitting.clone()))
        .collect::<BTreeMap<_, _>>();
    let terrain_prefabs = source
        .prefabs()
        .iter()
        .map(|(path, prefab)| (path.clone(), prefab.terrain.clone()))
        .collect();
    let mut emission: Option<serde_json::Value> = None;
    let mut exported_prefabs = None;
    let mut changes = None;
    let emit_pipes = |frame: road_paint_stage::RoadsideInput<'_, '_>| {
        if frame.layers.height != baseline.height
            || frame.layers.topology != baseline.topology
            || frame.layers.splat != layers.splat
        {
            return Err("Roadside boundary does not match the completed earthwork revision".into());
        }
        let live = Layers {
            height: frame.layers.height.to_vec(),
            topology: frame.layers.topology.to_vec(),
            ..layers.clone()
        };
        let paths = frame
            .roads
            .iter()
            .map(|road| {
                let family = paint
                    .policy
                    .families
                    .iter()
                    .find(|family| family.hierarchy == road.hierarchy)
                    .ok_or("Missing road width")?;
                Ok(roadside_decor::Road {
                    hierarchy: road.hierarchy,
                    path: road.path,
                    spline: true,
                    width: family.width,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        if let Some((registry, _)) = &registry {
            let generated = water_treatment_stage::generate(
                water_treatment_stage::Input {
                    layers: &live,
                    alpha: &alpha,
                    roads: &paths,
                    source: &source,
                    registry,
                    known_topology: flags.values().fold(0, |mask, flag| mask | flag),
                    protected_topology: protected,
                    water: None,
                    height_reservations: None,
                },
                water_treatment_stage::Settings {
                    seed: 42,
                    realm_mask: 2,
                    maximum_terrain_work: 32_000_000,
                    maximum_sampling_attempts: 10000,
                    maximum_fitting_samples: 1_000_000,
                    maximum_clearance_work: 1_000_000,
                },
                || false,
            )?
            .ok_or("Pipe stage cancelled")?;
            if generated.report.admitted_instances == 0 {
                return Err("No pipe fit the controlled registry fixture".into());
            }
            exported_prefabs = water_treatment_stage::prefab_records(
                &generated.report,
                |path| identities.as_ref().and_then(|source| source.resolve(path)),
                || false,
            )?;
            let output = generated
                .terrain
                .ok_or("Plant gate did not enable pipe terrain")?;
            changes = Some(json!({"terrain":output.report,
                "height_samples":output.height.iter().zip(frame.layers.height).filter(|(a,b)|a!=b).count(),
                "topology_cells":output.topology.as_ref().ok_or("Missing pipe topology")?.iter().zip(frame.layers.topology).filter(|(a,b)|a!=b).count()}));
            emission = Some(serde_json::to_value(generated.report).map_err(|e| e.to_string())?);
            return Ok(Some(road_paint_stage::RoadsideChanges {
                height: Some(output.height),
                topology: output.topology,
                splat: output.splat,
            }));
        }
        let known_mask = flags.values().fold(0, |mask, flag| mask | flag);
        let mut transaction = footprints::RoadTransaction::new_objects(
            &live,
            &alpha,
            terrain_prefabs,
            known_mask,
            protected,
            || false,
        )?
        .ok_or("Transaction cancelled")?;
        let mut budget = decor_fit::OrientedBudget::new(&live, &alpha)?;
        let mut attempts = Budget::new(10000)?;
        let mut next_pose = 0;
        let stage = program.descriptor(0, 2)?;
        let request = roadside_live::Request {
            stage: &stage,
            sampling: program.sampling(0.25)?,
            roads: &paths,
            seed: 42,
            realm_mask: 2,
        };
        let context = decor_live::Context {
            transaction: &mut transaction,
            alpha: &alpha,
            fitting: &mut budget,
            prefabs: &fitting,
            attempts: &mut attempts,
            next_pose: &mut next_pose,
            water: None,
        };
        let report = roadside_live::emit(
            request,
            context,
            |pose, view| program.accepts_slope(view, pose.position),
            || false,
        )?
        .ok_or("Pipe emission cancelled")?;
        if report.admitted.is_empty() {
            return Err("No pipe fit the road-phase fixture".into());
        }
        let output = transaction
            .finish(|| false)?
            .ok_or("Pipe transaction cancelled")?;
        changes = Some(json!({"terrain":output.report,
            "height_samples":output.height.iter().zip(frame.layers.height).filter(|(a,b)|a!=b).count(),
            "topology_cells":output.topology.as_ref().ok_or("Missing pipe topology")?.iter().zip(frame.layers.topology).filter(|(a,b)|a!=b).count()}));
        emission = Some(serde_json::to_value(report).map_err(|e| e.to_string())?);
        Ok(Some(road_paint_stage::RoadsideChanges {
            height: Some(output.height),
            topology: output.topology,
            splat: output.splat,
        }))
    };
    let painted = road_paint_stage::paint_with_roadside(
        input(),
        None,
        32_000_000,
        |p| Ok(Some(vec![0.; p.len()])),
        |_| Ok(Some(terrain.water_no_hit_level)),
        emit_pipes,
        || false,
    )?
    .ok_or("Road-phase fixture cancelled")?;
    if (&layers.height, &layers.topology, &layers.splat) != (&original.0, &original.1, &original.2)
    {
        return Err("Road-phase fixture changed its source layers".into());
    }
    let height_bytes: Vec<_> = painted
        .height
        .iter()
        .flat_map(|h| h.to_le_bytes())
        .collect();
    let topology_bytes: Vec<_> = painted
        .topology
        .iter()
        .flat_map(|t| t.to_le_bytes())
        .collect();
    let changed_topology: Vec<_> = painted
        .topology
        .iter()
        .zip(&baseline.topology)
        .enumerate()
        .filter(|(_, (after, before))| after != before)
        .map(|(index, (after, before))| json!({"index":index,"before":before,"after":after}))
        .collect();
    let report = json!({"format":"mapper-pipe-road-phase-verification","schema":1,"source_sha256":source.sha256(),
        "fixture":"flat 1000 metre terrain, straight paved road, seed 42, quarter-distance spacing, anchor filter 2",
        "water_treatment_gate":if registry.is_some(){"captured plant placed as controlled roadside root"}else{"assumed present in fixture"},
        "tag_clearance":if registry.is_some(){"captured authored plant volumes at [400,20,400], yaw 37"}else{"empty fixture registry"},
        "tag_source_sha256":registry.as_ref().map(|(_,sha)|sha),
        "tag_catalog_sha256":registry.as_ref().map(|_|&tag_catalog.as_ref().unwrap_or(&catalog).sha256),
        "exported_prefabs":exported_prefabs,
        "phase_order":["road topology","road terrain","pipes","road texture"],
        "world_size":layers.size,"surface_resolution":layers.surface_resolution,
        "program_object_id":program.object_id,"changed_topology":changed_topology,
        "emission":emission,"changes":changes,"road_paint":painted.report,
        "height_sha256":format!("{:x}",Sha256::digest(height_bytes)),"topology_sha256":format!("{:x}",Sha256::digest(topology_bytes)),
        "splat_sha256":format!("{:x}",Sha256::digest(&painted.splat)),
        "source_layers_preserved":true,"alpha_preserved":true,"runtime_holes_applied":false,
        "runtime_holes_retained":source.prefabs().values().map(|p|p.runtime_holes.len()).sum::<usize>(),
        "normal_generation_integrated":false,"game_code_executed":false});
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    file.write_all(&serde_json::to_vec_pretty(&report)?)?;
    file.sync_all()?;
    println!(
        "{}",
        json!({"source_sha256":source.sha256(),
            "admitted":report["emission"].get("admitted_instances").cloned().unwrap_or_else(||
                json!(report["emission"]["admitted"].as_array().map(Vec::len))),
            "changes":report["changes"]})
    );
    Ok(())
}
