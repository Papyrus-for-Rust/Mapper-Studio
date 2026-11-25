//! Exercise the real shared transaction. CONFIG NEW_BUNDLE; no desktop/server.
use mapper_studio::{
    Project, infrastructure, native_noise, native_profile, prefab_assets, road_paint_source,
    road_paint_stage, road_refinement, road_surface_source, road_surface_stage,
    road_terrain_source, roadside_source,
};
use papyrus_terrain::{
    native_layers::Layers,
    path_geometry::Path as Curve,
    road_bed, road_clearance,
    road_network::{Destination, Family, Report, Road},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Component, Path},
};
fn bounded(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut raw = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() as u64 > limit {
        return Err("Input allowance exceeded".into());
    }
    Ok(raw)
}
fn digest(raw: &[u8]) -> String {
    format!("{:x}", Sha256::digest(raw))
}
fn read(path: &Path) -> Result<Value, String> {
    serde_json::from_slice(&bounded(path, 32 * 1024 * 1024)?).map_err(|e| e.to_string())
}
fn receipt(root: &Path, manifest: &Value, name: &str) -> Result<Vec<u8>, String> {
    let path = Path::new(name);
    if path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        return Err("Invalid filename".into());
    }
    let raw = bounded(&root.join(path), 64 * 1024 * 1024)?;
    if manifest["files"][name]["bytes"].as_u64() != Some(raw.len() as u64)
        || manifest["files"][name]["sha256"].as_str() != Some(&digest(&raw))
    {
        return Err(format!("Receipt differs: {name}"));
    }
    Ok(raw)
}
fn raw_file(root: &Path, name: &str, raw: &[u8]) -> Result<Value, String> {
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(root.join(name))
        .map_err(|e| e.to_string())?;
    f.write_all(raw)
        .and_then(|_| f.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(json!({"bytes":raw.len(),"sha256":digest(raw)}))
}
fn write(root: &Path, name: &str, value: &impl serde::Serialize) -> Result<Value, String> {
    raw_file(
        root,
        name,
        &serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?,
    )
}
fn shorts(raw: Vec<u8>, n: usize) -> Result<Vec<i16>, String> {
    if raw.len() != n * n * 2 {
        return Err("Invalid height dimensions".into());
    }
    Ok(raw
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect())
}
fn destination(value: &Value) -> Result<Destination, String> {
    if value["loop"] == true {
        return Ok(Destination::Loop);
    }
    if let (Some(monument), Some(connector)) =
        (value["monument"].as_u64(), value["connector"].as_i64())
    {
        return Ok(Destination::Monument {
            monument,
            connector,
        });
    }
    if let (Some(road), Some(point)) = (
        value["network_road"].as_u64(),
        value["network_point"].as_u64(),
    ) {
        return Ok(Destination::Network {
            road: road as usize,
            point: point as usize,
        });
    }
    Err("Unknown road destination".into())
}
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("Expected CONFIG NEW_BUNDLE".into());
    }
    let config = read(Path::new(&args[0]))?;
    let path = |key: &str| {
        config[key]
            .as_str()
            .map(Path::new)
            .ok_or(format!("Missing {key}"))
    };
    let stamp = path("stamp")?;
    let initial = path("initial_data")?;
    let root = path("rust_root")?;
    let output = Path::new(&args[1]);
    if output.exists() {
        return Err("Bundle must be new".into());
    }
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let absolute = parent
        .canonicalize()
        .map_err(|e| e.to_string())?
        .join(output.file_name().ok_or("Missing bundle name")?);
    for input in [stamp, initial, root] {
        if absolute.starts_with(input.canonicalize().map_err(|e| e.to_string())?) {
            return Err("Output must be outside inputs".into());
        }
    }
    let project = Project::load(path("project")?)?;
    let recipe = project.roads.as_ref().ok_or("Missing road recipe")?;
    let catalog = prefab_assets::load(
        root,
        path("catalog")?,
        project
            .installation
            .as_ref()
            .ok_or("Missing installation")?,
        || false,
    )?;
    let parent_source = infrastructure::Source::load(path("infrastructure")?, &catalog, || false)?;
    parent_source.verify_installation(root, || false)?;
    let source =
        road_surface_source::Source::read(path("mesh_source")?, &parent_source, &|| false)?;
    source.verify_installation(root, || false)?;
    let roadside =
        roadside_source::Source::load(path("roadside_source")?, root, &catalog, || false)?;
    let paint = road_paint_source::Profile::for_source(&roadside)?
        .ok_or("Unsupported source paint geometry")?;
    let manifest = read(&stamp.join("manifest.json"))?;
    let paint_study: Value = serde_json::from_slice(&receipt(stamp, &manifest, "road-study.json")?)
        .map_err(|e| e.to_string())?;
    if paint_study["profile_sha256"].as_str() != Some(&paint.sha256()?) {
        return Err("Source paint binding differs".into());
    }
    let n = manifest["height_resolution"]
        .as_u64()
        .ok_or("Missing height dimensions")? as usize;
    let sn = manifest["surface_resolution"]
        .as_u64()
        .ok_or("Missing surface dimensions")? as usize;
    if !(2..=4097).contains(&n)
        || !(2..=2048).contains(&sn)
        || manifest["size"].as_u64() != Some(project.size as u64)
    {
        return Err("Invalid snapshot dimensions".into());
    }
    let initial_raw = bounded(&initial.join("height.i16le"), 64 * 1024 * 1024)?;
    let initial_study = read(&initial.join("study.json"))?;
    if initial_study["applied"] != true
        || initial_study["height_sha256"].as_str() != Some(&digest(&initial_raw))
    {
        return Err("Initial height binding differs".into());
    }
    let height = shorts(initial_raw, n)?;
    let ground = shorts(receipt(stamp, &manifest, "after-height.i16le")?, n)?;
    let painted_raw = receipt(stamp, &manifest, "road-study-height.i16le")?;
    let painted_sha = digest(&painted_raw);
    let painted = shorts(painted_raw, n)?;
    let topo_raw = receipt(stamp, &manifest, "road-study-topology.i32le")?;
    if topo_raw.len() != sn * sn * 4 {
        return Err("Invalid topology dimensions".into());
    }
    let topology: Vec<_> = topo_raw
        .chunks_exact(4)
        .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let planned_raw = bounded(path("planned_roads")?, 32 * 1024 * 1024)?;
    let planned: Value = serde_json::from_slice(&planned_raw).map_err(|e| e.to_string())?;
    if initial_study["input_planned_roads_sha256"].as_str() != Some(&digest(&planned_raw))
        || planned["entrance_launch"]
            != serde_json::to_value(recipe.entrance_launch).map_err(|e| e.to_string())?
    {
        return Err("Planned road binding differs".into());
    }
    let paths = initial_study["roads"]
        .as_array()
        .ok_or("Missing initial controls")?;
    let planned_roads = planned["roads"].as_array().ok_or("Missing planned roads")?;
    if paths.len() != planned_roads.len() {
        return Err("Road coverage differs".into());
    }
    let roads = Report {
        families: serde_json::from_value::<Vec<Family>>(planned["families"].clone())
            .map_err(|e| e.to_string())?,
        roads: paths
            .iter()
            .zip(planned_roads)
            .map(|(p, r)| {
                Ok(Road {
                    from: destination(&r["from"])?,
                    to: destination(&r["to"])?,
                    points: serde_json::from_value(p["points"].clone())
                        .map_err(|e| e.to_string())?,
                })
            })
            .collect::<Result<_, String>>()?,
        ..Default::default()
    };
    let from_painted = config["start_from_source_paint"] == true;
    let layers = Layers {
        size: project.size,
        terrain_seed: project.seeds[0].value,
        biome_seed: project.seeds[1].value,
        height_resolution: n,
        surface_resolution: sn,
        height: if from_painted { ground.clone() } else { height },
        biome: vec![],
        splat: vec![],
        topology: vec![],
        normalized_biome_cells: 0,
        normalized_splat_cells: 0,
    };
    let mask = config["protected_envelope_mask"]
        .as_i64()
        .and_then(|v| i32::try_from(v).ok())
        .ok_or("Missing eligible topology")?;
    let protected = initial_study["protected_topology"]
        .as_i64()
        .and_then(|v| i32::try_from(v).ok())
        .ok_or("Missing protected topology")?;
    let fit = road_bed::Settings {
        maximum_grade: recipe.maximum_grade,
        maximum_cut_fill: recipe.maximum_cut_fill,
        minimum_up_normal: 0.5,
        maximum_iterations: config["iterations"].as_u64().unwrap_or(512) as usize,
        maximum_work: config["maximum_work"].as_u64().unwrap_or(2_000_000_000) as usize,
        quantization_reserve: 0.51,
        reserve_roundoff: true,
    };
    let settings = road_refinement::Settings {
        fit,
        clearance: road_clearance::Settings {
            tolerance: 0.02,
            minimum_up_normal: 0.5,
            maximum_work: 10_000_000,
            maximum_probes: 131072,
        },
        maximum_rounds: config["maximum_rounds"].as_u64().unwrap_or(16) as usize,
        maximum_total_work: config["maximum_total_work"]
            .as_u64()
            .unwrap_or(32_000_000_000),
        maximum_reprojection_distance: 0.25,
    };
    let started = std::time::Instant::now();
    let mut progress = Vec::new();
    let observe = |round: &road_refinement::Round| {
        eprintln!(
            "Round {}: {} grade, {} continuous, {} source failures; {} probes",
            round.index,
            round.grade_violations,
            round.clearance_intersections,
            round.source_intersections,
            round.witnesses
        );
        progress.push(serde_json::to_value(round).unwrap());
    };
    let context = road_surface_stage::Context {
        project: &project,
        root,
        worker: path("worker")?,
        layers: &layers,
    };
    let result = if from_painted {
        let terrain =
            road_terrain_source::Profile::for_paint(&paint)?.ok_or("No source terrain profile")?;
        let raw_paths = paint_study["texture_paths"]
            .as_array()
            .ok_or("Missing source-painted paths")?;
        let raw_paths = raw_paths
            .iter()
            .map(|p| {
                let points: Vec<[f32; 3]> =
                    serde_json::from_value(p["points"].clone()).map_err(|e| e.to_string())?;
                Curve::from_controls(&points, 32_000_000, || false)
                    .map_err(|e| format!("{e:?}"))?
                    .ok_or("Cancelled".into())
            })
            .collect::<Result<Vec<_>, String>>()?;
        road_refinement::refine_painted_with_source(
            context,
            &roads,
            &source,
            road_refinement::PaintedSnapshot {
                source_rounding: None,
                retained_height: None,
                height: &painted,
                topology: &topology,
                surface_resolution: sn,
                paths: &raw_paths,
                paint: &paint,
                terrain: &terrain,
                allowed_topology: mask,
                maximum_initial_work: 512_000_000,
            },
            settings,
            observe,
            || false,
        )
    } else {
        road_refinement::refine_with_source(
            context,
            &roads,
            &source,
            road_refinement::Snapshots {
                retained_height: None,
                geometry: road_surface_stage::HeightGeometry {
                    origin_y: paint.height_origin,
                    vertical_span: paint.vertical_span,
                },
                ground: &ground,
                source_painted: &painted,
                source_rounding: None,
                topology: &topology,
                surface_resolution: sn,
                protected_topology: protected,
                allowed_topology: mask,
            },
            settings,
            observe,
            || false,
        )
    };
    let temporary = tempfile::Builder::new()
        .prefix(".road-refinement-")
        .tempdir_in(parent)
        .map_err(|e| e.to_string())?;
    let mut summary = json!({"format":"mapper-shared-road-refinement-study","schema":1,"applied":false,"playable":false,"elapsed_seconds":started.elapsed().as_secs_f64(),"progress":progress,"default_generator_activated":false,"started_from_source_paint":from_painted});
    match result {
        Err(error) => summary["failure"] = json!(format!("{error:?}")),
        Ok(None) => summary["cancelled"] = true.into(),
        Ok(Some(result)) => {
            let heights = temporary.path().join("height");
            let meshes = temporary.path().join("meshes");
            fs::create_dir(&heights)
                .and_then(|_| fs::create_dir(&meshes))
                .map_err(|e| e.to_string())?;
            let raw: Vec<_> = result.height.iter().flat_map(|v| v.to_le_bytes()).collect();
            let height_sha = digest(&raw);
            raw_file(&heights, "height.i16le", &raw)?;
            let paths = result
                .controls
                .iter()
                .map(|p| {
                    Curve::from_controls(p, 32_000_000, || false)
                        .map_err(|e| format!("Path rebuild: {e:?}"))?
                        .ok_or("Path cancelled".into())
                })
                .collect::<Result<Vec<_>, String>>()?;

            if config["finish_surfaces"] == true {
                let raw = receipt(stamp, &manifest, "after-topology.i32le")?;
                if raw.len() != sn * sn * 4 {
                    return Err("Invalid pre-road topology".into());
                }
                let base_layers = Layers {
                    size: project.size,
                    terrain_seed: layers.terrain_seed,
                    biome_seed: layers.biome_seed,
                    height_resolution: n,
                    surface_resolution: sn,
                    height: ground.clone(),
                    topology: raw
                        .chunks_exact(4)
                        .map(|b| i32::from_le_bytes(b.try_into().unwrap()))
                        .collect(),
                    splat: receipt(stamp, &manifest, "after-splat.u8")?,
                    biome: receipt(stamp, &manifest, "after-biome.u8")?,
                    normalized_biome_cells: 0,
                    normalized_splat_cells: 0,
                };
                let alpha = receipt(stamp, &manifest, "after-alpha.u8")?;
                let mut base = papyrus_terrain::monument_composition::compose_with_water(
                    &base_layers,
                    &alpha,
                    &[],
                    || false,
                )?
                .ok_or("Final surface base cancelled")?;
                base.ocean
                    .as_mut()
                    .ok_or("Missing final base ocean")?
                    .local_water =
                    shorts(receipt(stamp, &manifest, "after-local-water.i16le")?, n)?;
                let families = &roads.families;
                let material_roads = paths
                    .iter()
                    .enumerate()
                    .map(|(i, path)| {
                        let family = families
                            .iter()
                            .find(|f| i >= f.first_road && i < f.first_road + f.road_count)
                            .ok_or("Missing final family")?;
                        Ok(papyrus_terrain::road_paint::Road {
                            path,
                            hierarchy: u8::try_from(family.hierarchy).map_err(|e| e.to_string())?,
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                let native = native_profile::resolve(&project.terrain_key())?;
                let mut batches = Vec::new();
                let finished = road_paint_stage::finish_surfaces(
                    &base,
                    &result.height,
                    &material_roads,
                    &paint,
                    512_000_000,
                    |coordinates| {
                        let values = native_noise::sample(
                            root,
                            path("worker")?,
                            project
                                .installation
                                .as_ref()
                                .ok_or("Missing installation")?,
                            &native,
                            coordinates,
                            || false,
                        )?
                        .ok_or("Final noise cancelled")?;
                        batches.push(json!({"coordinates":coordinates,"values":values}));
                        Ok(Some(values))
                    },
                    || false,
                )?
                .ok_or("Final surfaces cancelled")?;
                let directory = temporary.path().join("surfaces");
                fs::create_dir(&directory).map_err(|e| e.to_string())?;
                raw_file(&directory, "splat.u8", &finished.splat)?;
                let raw: Vec<_> = finished
                    .topology
                    .iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect();
                raw_file(&directory, "topology.i32le", &raw)?;
                if let Some(ocean) = finished.ocean {
                    raw_file(&directory, "ocean.u8", &ocean.classification)?;
                    let raw: Vec<_> = ocean
                        .local_water
                        .iter()
                        .flat_map(|v| v.to_le_bytes())
                        .collect();
                    raw_file(&directory, "local-water.i16le", &raw)?;
                }
                write(
                    &directory,
                    "study.json",
                    &json!({"format":"mapper-final-road-surfaces-study","schema":1,
                    "playable":false,"height_sha256":height_sha,"stamp_manifest_sha256":digest(&bounded(&stamp.join("manifest.json"),32*1024*1024)?),
                    "report":finished.report,"noise_batches":batches}),
                )?;
                summary["final_surfaces"] =
                    serde_json::to_value(finished.report).map_err(|e| e.to_string())?;
            }
            let height_study = json!({"format":"mapper-road-bed-fit-study","schema":1,"applied":true,"playable":false,"settings":fit,"height_sha256":height_sha,"input_height_sha256":initial_study["height_sha256"],"input_correction_study_sha256":digest(&bounded(&initial.join("study.json"),32*1024*1024)?),"input_planned_roads_sha256":digest(&planned_raw),"protected_topology":protected,"protected_paint_envelope":{"format":"papyrus-protected-road-paint-envelope-1","allowed_topology":mask,"source_painted_sha256":painted_sha},"roads":paths,"report":result.report});
            let height_receipt = write(&heights, "study.json", &height_study)?;
            let mut files = BTreeMap::new();
            for chunk in &result.geometry.surfaces.chunks {
                files.insert(chunk.filename(), write(&meshes, &chunk.filename(), chunk)?);
            }
            let mut entries = Vec::new();
            let mut vertices = 0;
            for trace in &result.geometry.traces {
                vertices += trace.heights.len();
                let name = format!(
                    "road-height-trace-{}-chunk-{}.json",
                    trace.road, trace.chunk_index
                );
                files.insert(name.clone(), write(&meshes, &name, trace)?);
                entries.push(json!({"file":name,"road":trace.road,"chunk_index":trace.chunk_index,"vertices":trace.heights.len()}));
            }
            let mesh_study = json!({"format":"mapper-source-road-mesh-grade-study","schema":1,"playable":false,"normal_generator_activated":false,"source_sha256":source.sha256,"paint_profile_sha256":paint.sha256()?,"input_height_sha256":height_sha,"input_correction_study_sha256":height_receipt["sha256"],"input_planned_roads_sha256":digest(&planned_raw),"surface_report":result.geometry.surfaces.report,"collision_audits":result.geometry.surfaces.report["collision_grade_audits"],"height_traces":{"engine":"papyrus-road-height-bindings-1","vertices_bitwise_checked":vertices,"files":entries},"runtime_access_validated":false});
            files.insert(
                "study.json".into(),
                write(&meshes, "study.json", &mesh_study)?,
            );
            write(
                &meshes,
                "manifest.json",
                &json!({"format":"mapper-source-road-mesh-grade-artifacts","schema":1,"playable":false,"files":files}),
            )?;
            summary["applied"] = true.into();
            summary["refinement"] = height_study["report"].clone();
            summary["clearance"] = result.clearance.report;
        }
    }
    write(temporary.path(), "study.json", &summary)?;
    fs::rename(temporary.path(), &absolute).map_err(|e| e.to_string())?;
    println!(
        "Applied: {}; failure: {}",
        summary["applied"], summary["failure"]
    );
    Ok(())
}
