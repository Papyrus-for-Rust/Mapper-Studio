//! CONFIG FROZEN_PRE_ROAD_FIXTURE NEW_REPORT. Calls the normal planner headlessly.
use mapper_studio::{
    Project, infrastructure, prefab_assets, prefab_textures, road_planning, source_road_assets,
    source_road_inputs, source_road_planning,
};
use papyrus_terrain::{
    monument_composition,
    native_layers::Layers,
    placement::{Outcome, Position, Solution},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};
fn read(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("Diagnostic input exceeds bound".into());
    }
    Ok(bytes)
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn scalar(value: &Value, key: &str) -> Result<f32, String> {
    value[key]
        .as_f64()
        .map(|v| v as f32)
        .filter(|v| v.is_finite())
        .ok_or(format!("Missing finite {key}"))
}
fn coupled_engine(config: &Value) -> Result<Option<&str>, String> {
    let enabled = match config.get("coupled_placement") {
        None => false,
        Some(Value::Bool(enabled)) => *enabled,
        _ => return Err("coupled_placement must be a boolean".into()),
    };
    if !enabled {
        return if config.get("coupled_engine").is_some() {
            Err("coupled_engine requires coupled_placement".into())
        } else {
            Ok(None)
        };
    }
    let engine = match config.get("coupled_engine") {
        None => mapper_studio::road_generation::COUPLED_VERSION,
        Some(Value::String(engine)) => engine.as_str(),
        _ => return Err("coupled_engine must be a supported version string".into()),
    };
    if !matches!(
        engine,
        mapper_studio::road_generation::LEGACY_COUPLED_VERSION
            | mapper_studio::road_generation::PROPOSAL_COUPLED_VERSION
            | mapper_studio::road_generation::GRID_COUPLED_VERSION
            | mapper_studio::road_generation::COUPLED_VERSION
    ) {
        return Err("Unsupported diagnostic coupled engine".into());
    }
    Ok(Some(engine))
}
fn solution(value: &Value) -> Result<Solution, String> {
    let rows = value["outcomes"]
        .as_array()
        .filter(|a| a.len() <= 128)
        .ok_or("Invalid layout rows")?;
    let mut outcomes = Vec::new();
    for row in rows {
        let p = &row["position"];
        outcomes.push(Outcome {
            id: row["id"].as_u64().ok_or("Missing instance ID")?,
            position: if p.is_null() {
                None
            } else {
                Some(Position {
                    x: scalar(p, "x")?,
                    y: scalar(p, "y")?,
                    z: scalar(p, "z")?,
                    yaw: scalar(p, "yaw")?,
                    clearance: scalar(p, "clearance")?,
                })
            },
            distance: row["distance"].as_f64().map(|v| v as f32),
            within_radius: row["within_radius"]
                .as_bool()
                .ok_or("Missing radius flag")?,
            reason: row["reason"]
                .as_str()
                .ok_or("Missing placement reason")?
                .into(),
        });
    }
    Ok(Solution {
        outcomes,
        required_missing: value["required_missing"]
            .as_u64()
            .ok_or("Missing placement deficit")? as usize,
        attempts: value["attempts"]
            .as_u64()
            .ok_or("Missing placement attempts")? as usize,
    })
}

fn main() -> Result<(), String> {
    // Match the app's operation-scoped read leases: strongly hash each source
    // once while preventing writers, then reuse that receipt across stages.
    let _source_verification = mapper_studio::installation::VerificationScope::default();
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err("CONFIG FROZEN_PRE_ROAD_FIXTURE NEW_REPORT".into());
    }
    let output = Path::new(&args[2]);
    if output.exists() {
        return Err("Output must be new".into());
    }
    let config: Value = serde_json::from_slice(&read(Path::new(&args[0]), 4 * 1024 * 1024)?)
        .map_err(|e| e.to_string())?;
    let selected_coupled_engine = coupled_engine(&config)?;
    let source_materialization = match config.get("source_materialization") {
        None | Some(Value::Bool(false)) => false,
        Some(Value::Bool(true)) => true,
        _ => return Err("source_materialization must be a boolean".into()),
    };
    if source_materialization && selected_coupled_engine.is_some() {
        return Err(
            "Source materialization and strict coupled fitting are separate contracts".into(),
        );
    }
    let source_raw_junctions = match config.get("source_raw_junctions") {
        None | Some(Value::Bool(false)) => false,
        Some(Value::Bool(true)) if source_materialization => true,
        _ => return Err("source_raw_junctions requires source_materialization".into()),
    };

    let path = |key: &str| -> Result<PathBuf, String> {
        config[key]
            .as_str()
            .map(PathBuf::from)
            .ok_or(format!("Missing {key}"))
    };
    let fixture = Path::new(&args[1]);
    let manifest: Value =
        serde_json::from_slice(&read(&fixture.join("manifest.json"), 1024 * 1024)?)
            .map_err(|e| e.to_string())?;
    if manifest["format"] != "mapper-private-pre-road-fixture-1" {
        return Err("Wrong fixture format".into());
    }
    let checked = |name: &str| -> Result<Vec<u8>, String> {
        let raw = read(&fixture.join(name), 512 * 1024 * 1024)?;
        if manifest["files"][name]["bytes"] != raw.len() as u64
            || manifest["files"][name]["sha256"] != hash(&raw)
        {
            return Err(format!("Fixture changed: {name}"));
        }
        Ok(raw)
    };
    let mut project: Project =
        serde_json::from_slice(&checked("project.mapper")?).map_err(|e| e.to_string())?;
    let solution =
        solution(&serde_json::from_slice(&checked("layout.json")?).map_err(|e| e.to_string())?)?;
    let root = path("rust_root")?;
    let catalog = prefab_assets::load(
        &root,
        &path("catalog")?,
        project
            .installation
            .as_ref()
            .ok_or("Missing installation")?,
        || false,
    )?;
    let bindings = prefab_textures::Bindings::load(&root, &path("bindings")?, &catalog, || false)?;
    let source = infrastructure::Source::load(&path("infrastructure")?, &catalog, || false)?;
    let assets = source_road_assets::Assets::load(
        &path("source_layout_assets")?,
        &root,
        &catalog,
        &source,
        || false,
    )?
    .ok_or("Cancelled")?;
    let inputs = mapper_studio::road_extract::OrderedInputs {
        assets: Arc::new(assets),
        roles: source_road_inputs::Roles::for_source(&source)?.ok_or("Unknown code roles")?,
    };
    let profile: mapper_studio::road_cost_source::Profile =
        serde_json::from_slice(&read(&path("source_reservation_profile")?, 16384)?)
            .map_err(|e| e.to_string())?;
    profile.check(&source)?;
    let roads = project.roads.as_mut().ok_or("Missing roads")?;
    roads.ring_costmap = Some(profile.clone());
    roads.access_costmap = Some(profile);
    roads.staged.as_mut().ok_or("Missing staged recipe")?.layout =
        Some(source_road_planning::Policy::bind(&inputs));
    if config
        .get("global_earthwork")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        project.roads.as_mut().unwrap().set_global_earthwork(true)?;
    }
    let guarded_ring = config
        .get("ring_clearance_cells")
        .map(|v| v.as_u64().ok_or("Invalid ring clearance cells"))
        .transpose()?;
    if let Some(radius) = guarded_ring {
        project
            .roads
            .as_mut()
            .unwrap()
            .staged
            .as_mut()
            .unwrap()
            .layout
            .as_mut()
            .unwrap()
            .ring_clearance = Some(source_road_planning::RingClearance {
            engine: source_road_planning::RING_CLEARANCE_VERSION.into(),
            radius_cells: usize::try_from(radius).map_err(|e| e.to_string())?,
        });
    }
    let terrain_entrances = config
        .get("terrain_entrances")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if terrain_entrances {
        project
            .roads
            .as_mut()
            .unwrap()
            .staged
            .as_mut()
            .unwrap()
            .layout
            .as_mut()
            .unwrap()
            .terrain_entrances = Some(
            config
                .get("terrain_entrance_engine")
                .and_then(Value::as_str)
                .unwrap_or(papyrus_terrain::source_road_route::TERRAIN_ENTRANCE_ROUTED_VERSION)
                .into(),
        );
    }
    let stamp_revision = config
        .get("stamp_revision")
        .map(|value| match value.as_u64() {
            Some(3) => Ok(monument_composition::SOURCE_TEXTURE_VERSION),
            Some(4) => Ok(monument_composition::SOURCE_CLIFF_VERSION),
            Some(5) => Ok(monument_composition::SOURCE_RASTER_VERSION),
            Some(6) => Ok(monument_composition::SOURCE_HEIGHT_VERSION),
            _ => Err("stamp_revision must be 3, 4, 5 or 6"),
        })
        .transpose()?;
    let compare_reference =
        guarded_ring.is_none() && !terrain_entrances && stamp_revision.is_none();
    project.validate()?;
    let (hr, sr) = papyrus_terrain::native_layers::dimensions(project.size)?;
    if manifest["height_resolution"] != hr as u64 || manifest["surface_resolution"] != sr as u64 {
        return Err("Fixture dimensions differ".into());
    }
    let height = checked("height.i16le")?;
    let topology = checked("topology.i32le")?;
    if height.len() != hr * hr * 2 || topology.len() != sr * sr * 4 {
        return Err("Invalid encoded grid length".into());
    }
    let layers = Layers {
        size: project.size,
        terrain_seed: project.terrain_key().seed,
        biome_seed: project.terrain_key().biome_seed.unwrap_or(0),
        height_resolution: hr,
        surface_resolution: sr,
        height: height
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes(b.try_into().unwrap()))
            .collect(),
        topology: topology
            .chunks_exact(4)
            .map(|b| i32::from_le_bytes(b.try_into().unwrap()))
            .collect(),
        biome: checked("biome.u8")?,
        splat: checked("splat.u8")?,
        normalized_biome_cells: 0,
        normalized_splat_cells: 0,
    };
    let mut immutable_source = None;
    let maps = if config
        .get("rebuild_pre_road")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        eprintln!("Rebuilding complete pre-road world with retained river colliders");
        let processed = mapper_studio::native_processing::build(
            &root,
            &path("worker")?,
            &project.terrain_key(),
            None,
            &std::sync::atomic::AtomicBool::new(false),
            |label, _| eprintln!("{label}"),
        )?
        .ok_or("Cancelled")?;
        let rebuilt = mapper_studio::composition::compose_monuments(
            &processed.source,
            &project,
            &solution,
            &catalog,
            &bindings,
            || false,
        )?
        .ok_or("Cancelled")?;
        if rebuilt.layers.height != layers.height
            || rebuilt.layers.topology != layers.topology
            || rebuilt.layers.biome != layers.biome
            || rebuilt.layers.splat != layers.splat
            || rebuilt.alpha != checked("alpha.u8")?
        {
            return Err("Rebuilt pre-road world differs from frozen fixture".into());
        }
        immutable_source = Some(processed.source);
        rebuilt
    } else {
        monument_composition::compose(&layers, &checked("alpha.u8")?, &[], || false)?
            .ok_or("Cancelled")?
    };
    // Verify the original frozen fixture first. A requested composition revision
    // is then rebuilt from the same clean source and poses, never compared to
    // an old geometry baseline as if those terrain inputs were unchanged.
    let maps = if let Some(revision) = stamp_revision {
        project.composition_engine = Some(revision.into());
        project.validate()?;
        mapper_studio::composition::compose_monuments(
            immutable_source
                .as_ref()
                .ok_or("Stamp revision requires pre-road reconstruction")?,
            &project,
            &solution,
            &catalog,
            &bindings,
            || false,
        )?
        .ok_or("Cancelled")?
    } else {
        maps
    };
    eprintln!("Running the normal ordered road planner against verified pre-road inputs");
    let started = Instant::now();
    let plan = road_planning::prepare(&project, &solution, &catalog, &bindings, &maps, || false)?
        .ok_or("Cancelled")?;
    let elapsed = started.elapsed().as_secs_f64() * 1000.;
    plan.check_inputs(&project, &solution, &catalog, &bindings, &maps, || false)?
        .ok_or("Cancelled")?;
    let mut changed = project.clone();
    changed.seeds[3].value = changed.seeds[3].value.wrapping_add(1);
    if plan
        .check_inputs(&changed, &solution, &catalog, &bindings, &maps, || false)
        .is_ok()
    {
        return Err("Stale recipe accepted".into());
    }
    if plan
        .check_inputs(&project, &solution, &catalog, &bindings, &maps, || true)?
        .is_some()
    {
        return Err("Cancelled input check accepted".into());
    }
    let mut changed_layers = maps.layers.clone();
    changed_layers.height[0] = changed_layers.height[0].wrapping_add(1);
    let mut changed_maps =
        monument_composition::compose(&changed_layers, &maps.alpha, &[], || false)?
            .ok_or("Cancelled")?;
    changed_maps.rivers = maps.rivers.clone();
    changed_maps.ocean = maps.ocean.clone();
    if plan
        .check_inputs(
            &project,
            &solution,
            &catalog,
            &bindings,
            &changed_maps,
            || false,
        )
        .is_ok()
    {
        return Err("Stale terrain accepted".into());
    }
    let report = plan.planning_report();
    let expected: Value = serde_json::from_slice(&read(
        &path("source_network_reference")?,
        128 * 1024 * 1024,
    )?)
    .map_err(|e| e.to_string())?;
    let expected_paths = std::iter::once(&expected["ring"]["shaped"])
        .chain(
            expected["families"]
                .as_array()
                .ok_or("Invalid reference families")?
                .iter()
                .flat_map(|f| f["shaped"].as_array().into_iter().flatten()),
        )
        .collect::<Vec<_>>();
    let roads = plan.roads();
    if compare_reference && roads.len() != expected_paths.len() {
        return Err("Reference road count differs".into());
    }
    // Junction adapters move network terminals only; compare complete geometry to
    // the saved typed handoff instead of expecting pre-attachment end controls.
    let handoff: Value = serde_json::from_slice(&read(
        &path("source_handoff_reference")?,
        128 * 1024 * 1024,
    )?)
    .map_err(|e| e.to_string())?;
    let report_json = report.json();
    let expected_roads = handoff["retained_plan"]["roads"]
        .as_array()
        .ok_or("Missing reference roads")?;
    let expected_shapes = handoff["adaptation"]["paths"]
        .as_array()
        .ok_or("Missing reference shapes")?;
    if compare_reference
        && (expected_roads.len() != roads.len() || expected_shapes.len() != roads.len())
    {
        return Err("Handoff count differs".into());
    }
    for (i, road) in roads.iter().enumerate().filter(|_| compare_reference) {
        let points: Vec<[f32; 3]> = serde_json::from_value(expected_shapes[i]["points"].clone())
            .map_err(|e| e.to_string())?;
        let tangents: Vec<[f32; 3]> =
            serde_json::from_value(expected_shapes[i]["tangents"].clone())
                .map_err(|e| e.to_string())?;
        if road.path.points() != points
            || road.path.tangents() != tangents
            || road.path.length()
                != expected_shapes[i]["length"]
                    .as_f64()
                    .ok_or("Missing length")? as f32
            || road.path.step_size()
                != expected_shapes[i]["step_size"]
                    .as_f64()
                    .ok_or("Missing step")? as f32
            || road.path.range()
                != (
                    expected_shapes[i]["min_index"]
                        .as_u64()
                        .ok_or("Missing range")? as usize,
                    expected_shapes[i]["max_index"]
                        .as_u64()
                        .ok_or("Missing range")? as usize,
                )
            || report_json["roads"][i]["from"] != expected_roads[i]["from"]
            || report_json["roads"][i]["to"] != expected_roads[i]["to"]
        {
            return Err(format!(
                "Normal planner changed saved source geometry/identity for road {i}"
            ));
        }
    }
    let mut result = json!({"format":"mapper-normal-ordered-road-planning-1","milliseconds":elapsed,
        "source_sha256":source.sha256,"assets_sha256":inputs.assets.sha256,"recipe":project,
        "pre_road_rebuilt":config.get("rebuild_pre_road").and_then(Value::as_bool).unwrap_or(false),"river_world_retained":maps.rivers.is_some(),
        "input_bound":true,"stale_recipe_rejected":true,"stale_terrain_rejected":true,"cancelled_input_check_stopped":true,"planning":report_json,
        "reference_path_count":expected_paths.len(),"reference_handoff_geometry_checked":compare_reference,"reference_handoff_geometry_exact":compare_reference.then_some(true),
        "terrain_and_mesh_validated":false,"full_map_validated":false,"gui_launched":false});
    if config
        .get("compose")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        if project.rails.is_some() {
            return Err("Initial source-ring audit needs retained-rail placement inputs".into());
        }
        let costs = project
            .roads
            .as_ref()
            .unwrap()
            .access_costmap
            .as_ref()
            .ok_or("Missing costs")?;
        let blockers = costs
            .prepare_blockers(&project, &solution, &catalog, &bindings, &source, || false)?
            .ok_or("Cancelled")?;
        let grid = costs
            .build_cell_family(
                &source,
                &maps.layers,
                &blockers,
                project.seeds[3].value,
                false,
                || false,
            )?
            .ok_or("Cancelled")?;
        let mut rings = Vec::new();
        for road in roads.iter().filter(|r| r.hierarchy == 0) {
            let audit = papyrus_terrain::road_smoothing::curve_cell_cost_check(
                road.path,
                &grid.grid.costs,
                grid.lattice,
                1.,
                2_000_000,
                &|| false,
            )
            .map_err(|e| format!("Ring audit: {e:?}"))?
            .ok_or("Cancelled")?;
            eprintln!(
                "Initial native ring clearance: {} (work {})",
                audit.clear, audit.work
            );
            if guarded_ring.is_some() && !audit.clear {
                return Err("Guarded retained ring lost clearance".into());
            }
            rings.push(json!({"clear":audit.clear,"work":audit.work}));
        }
        result["initial_ring_clearance"] = json!(rings);
        if let Some(monument) = config.get("proposal_replay").and_then(Value::as_u64) {
            #[cfg(feature = "road-diagnostics")]
            {
                result["proposal_replay"] = match mapper_studio::world_recovery::verify_proposal_replay(
                    immutable_source
                        .as_ref()
                        .ok_or("Proposal replay requires immutable source")?,
                    &project,
                    &catalog,
                    &bindings,
                    monument,
                    || false,
                ) {
                    Ok(report) => report,
                    Err(error) => json!({"error":error,"full_map_validated":false}),
                };
            }
            #[cfg(not(feature = "road-diagnostics"))]
            {
                let _ = monument;
                return Err("Proposal replay requires road-diagnostics".into());
            }
        }
        eprintln!("Running normal staged road terrain/surface composition");
        let started = Instant::now();
        let composed = if let Some(engine) = selected_coupled_engine {
            let mut coupled_project = project.clone();
            coupled_project
                .roads
                .as_mut()
                .unwrap()
                .staged
                .as_mut()
                .unwrap()
                .engine = engine.into();
            mapper_studio::composition::compose_resolved_layout_with_worker(
                immutable_source
                    .as_ref()
                    .ok_or("Coupled verification requires immutable pre-road reconstruction")?,
                &coupled_project,
                &solution,
                &catalog,
                &bindings,
                Some(&path("worker")?),
                || false,
            )
            .map(|o| o.map(|resolved| (resolved.maps, Some(resolved.solution))))
            .map_err(mapper_studio::road_generation::Rejection::from)
        } else {
            let mut source_project = project.clone();
            if source_materialization {
                source_project
                    .roads
                    .as_mut()
                    .unwrap()
                    .staged
                    .as_mut()
                    .unwrap()
                    .engine = if source_raw_junctions {
                    mapper_studio::road_generation::RAW_SOURCE_VERSION
                } else {
                    mapper_studio::road_generation::SOURCE_VERSION
                }
                .into();
            }
            mapper_studio::road_generation::compose_with_feedback(
                mapper_studio::road_generation::Context {
                    project: &source_project,
                    solution: &solution,
                    catalog: &catalog,
                    bindings: &bindings,
                    worker: &path("worker")?,
                },
                &maps,
                None,
                || false,
            )
            .map(|o| o.map(|maps| (maps, None)))
        };
        let mut stage = json!({"milliseconds":started.elapsed().as_secs_f64()*1000.,"completed":false,"full_map_validated":false,"source_raw_junctions":source_raw_junctions});
        stage["coupled_engine"] = json!(selected_coupled_engine);
        stage["source_materialization"] = json!(source_materialization);
        match composed {
            Ok(Some((composed, solved))) => {
                if let Some(solved) = solved {
                    stage["resolved_layout"] = json!(solved.outcomes.iter().map(|o|json!({"id":o.id,"position":o.position.as_ref().map(|p|[p.x,p.y,p.z,p.yaw]),"reason":o.reason})).collect::<Vec<_>>());
                }
                let roads = composed.roads.as_ref().ok_or("Composition lost roads")?;
                stage["completed"] = true.into();
                stage["roads"] = roads.json();
            }
            Ok(None) => stage["cancelled"] = true.into(),
            Err(error) => {
                eprintln!("Road composition rejected; recording detailed rejection in the report");
                stage["rejected_layout_feedback"] = json!(error.feedback);
                stage["error"] = error.message.into();
            }
        }
        plan.check_inputs(&project, &solution, &catalog, &bindings, &maps, || false)?
            .ok_or("Cancelled")?;
        stage["input_revision_preserved"] = true.into();
        result["composition"] = stage;
    }
    fs::File::options()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| e.to_string())?
        .write_all(&serde_json::to_vec_pretty(&result).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if result
        .get("composition")
        .is_some_and(|s| s["completed"] != true)
    {
        return Err(format!(
            "Road composition did not pass; rejection saved to {}",
            output.display()
        ));
    }
    println!(
        "Normal bound road plan: {} roads in {:.2} ms; {}",
        roads.len(),
        elapsed,
        output.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diagnostic_recovery_version_is_explicit_and_invalid_modes_fail_before_source_reads() {
        assert_eq!(coupled_engine(&json!({})).unwrap(), None);
        assert_eq!(
            coupled_engine(&json!({"coupled_placement":true})).unwrap(),
            Some(mapper_studio::road_generation::COUPLED_VERSION)
        );
        for version in [
            mapper_studio::road_generation::LEGACY_COUPLED_VERSION,
            mapper_studio::road_generation::PROPOSAL_COUPLED_VERSION,
            mapper_studio::road_generation::GRID_COUPLED_VERSION,
            mapper_studio::road_generation::COUPLED_VERSION,
        ] {
            assert_eq!(
                coupled_engine(&json!({"coupled_placement":true,"coupled_engine":version}))
                    .unwrap(),
                Some(version)
            );
        }
        for invalid in [
            json!({"coupled_placement":"true"}),
            json!({"coupled_engine":mapper_studio::road_generation::COUPLED_VERSION}),
            json!({"coupled_placement":false,"coupled_engine":mapper_studio::road_generation::COUPLED_VERSION}),
            json!({"coupled_placement":true,"coupled_engine":42}),
            json!({"coupled_placement":true,"coupled_engine":"future-engine"}),
        ] {
            assert!(coupled_engine(&invalid).is_err());
        }
    }
}
