//! CONFIG SAVED_BUNDLE NEW_REPORT. Candidate-only owned-input layout study.
//! Saved postprocessed terrain; no terrain writes, meshes or map publication.
use mapper_studio::{
    Project, composition, infrastructure, native_processing, prefab_assets, prefab_textures,
    road_paint_source, roadside_source, source_road_assets, source_road_inputs,
};
use papyrus_terrain::{
    native_layers::Layers,
    path_geometry, path_terrain,
    placement::{Outcome, Position, Solution},
    rail_reservations, source_road_layout, source_road_ring, source_road_route,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    sync::atomic::AtomicBool,
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
fn freeze(
    project: &Project,
    layout: &Value,
    maps: &papyrus_terrain::monument_composition::Composed,
    destination: &Path,
) -> Result<Value, String> {
    if destination.exists() {
        return Err("Fresh pre-road fixture must be new".into());
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let stage = tempfile::Builder::new()
        .prefix(".source-pre-road-")
        .tempdir_in(parent)
        .map_err(|e| e.to_string())?;
    project.save(&stage.path().join("project.mapper"))?;
    let mut files = serde_json::Map::new();
    let project_bytes = read(&stage.path().join("project.mapper"), 1024 * 1024)?;
    files.insert(
        "project.mapper".into(),
        json!({"bytes":project_bytes.len(),"sha256":hash(&project_bytes)}),
    );
    let mut write = |name: &str, bytes: &[u8]| -> Result<(), String> {
        let mut file = fs::File::options()
            .write(true)
            .create_new(true)
            .open(stage.path().join(name))
            .map_err(|e| e.to_string())?;
        file.write_all(bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        files.insert(
            name.into(),
            json!({"bytes":bytes.len(),"sha256":hash(bytes)}),
        );
        Ok(())
    };
    write(
        "layout.json",
        &serde_json::to_vec_pretty(layout).map_err(|e| e.to_string())?,
    )?;
    write(
        "height.i16le",
        &maps
            .layers
            .height
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>(),
    )?;
    write(
        "topology.i32le",
        &maps
            .layers
            .topology
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>(),
    )?;
    write("biome.u8", &maps.layers.biome)?;
    write("splat.u8", &maps.layers.splat)?;
    write("alpha.u8", &maps.alpha)?;
    let manifest = json!({"format":"mapper-private-pre-road-fixture-1","size":project.size,
        "height_resolution":maps.layers.height_resolution,"surface_resolution":maps.layers.surface_resolution,"files":files,
        "scope":"Fresh native terrain with fixed non-roadside poses from the saved reference. Monument/ocean composition only; no roads, rails, late objects, topology overrides, meshes or complete map."});
    fs::write(
        stage.path().join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    fs::rename(stage.path(), destination).map_err(|e| e.to_string())?;
    Ok(json!({"directory":destination,"manifest":manifest}))
}
fn scalar(value: &Value, key: &str) -> Result<f32, String> {
    value[key]
        .as_f64()
        .map(|v| v as f32)
        .filter(|v| v.is_finite())
        .ok_or(format!("Missing finite {key}"))
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
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err("CONFIG SAVED_BUNDLE NEW_REPORT".into());
    }
    let output = Path::new(&args[2]);
    if output.exists() {
        return Err("Report must be new".into());
    }
    let config: Value = serde_json::from_slice(&read(Path::new(&args[0]), 1024 * 1024)?)
        .map_err(|e| e.to_string())?;
    let path = |key: &str| {
        config[key]
            .as_str()
            .map(Path::new)
            .ok_or(format!("Missing {key}"))
    };
    let number = |key: &str| {
        config[key]
            .as_u64()
            .map(|v| v as usize)
            .ok_or(format!("Missing {key}"))
    };
    let bundle = Path::new(&args[1]);
    let manifest_bytes = read(&bundle.join("manifest.json"), 4 * 1024 * 1024)?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes).map_err(|e| e.to_string())?;
    let checked = |name: &str| -> Result<Vec<u8>, String> {
        let raw = read(&bundle.join(name), 128 * 1024 * 1024)?;
        if manifest["files"][name]["sha256"].as_str() != Some(hash(&raw).as_str())
            || manifest["files"][name]["bytes"].as_u64() != Some(raw.len() as u64)
        {
            return Err(format!("Changed bundle input {name}"));
        }
        Ok(raw)
    };
    let project_bytes = checked("project.mapper")?;
    let mut project = Project::load(&bundle.join("project.mapper"))?;
    if project.rails.is_some() {
        return Err("This component study requires an explicit retained-rail adapter for rail-enabled inputs".into());
    }
    let layout_bytes = checked("layout.json")?;
    let mut saved = solution(&serde_json::from_slice(&layout_bytes).map_err(|e| e.to_string())?)?;
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
    eprintln!("Verifying selected texture bindings for monument blockers");
    let bindings = prefab_textures::Bindings::load(root, path("bindings")?, &catalog, || false)?;
    let source = infrastructure::Source::load(path("infrastructure")?, &catalog, || false)?;
    let assets = source_road_assets::Assets::load(
        path("source_layout_assets")?,
        root,
        &catalog,
        &source,
        || false,
    )?
    .ok_or("Cancelled")?;
    let roles = source_road_inputs::Roles::load(path("source_client_roles")?, &source, || false)?
        .ok_or("Cancelled")?;
    let roadside =
        roadside_source::Source::load(path("roadside_source")?, root, &catalog, || false)?;
    let paint = road_paint_source::Profile::for_source(&roadside)?
        .ok_or("Missing source height-scale policy")?;
    let recipe = project.roads.as_ref().ok_or("Missing road recipe")?;
    let costs = recipe
        .ring_costmap
        .as_ref()
        .ok_or("Missing source road cost profile")?;
    costs.check(&source)?;
    let joins = costs.joins.as_ref().ok_or("Missing source join policy")?;
    let reservation_bytes = read(path("source_reservation_profile")?, 4 * 1024 * 1024)?;
    let reservation_profile: mapper_studio::road_cost_source::Profile =
        serde_json::from_slice(&reservation_bytes).map_err(|e| e.to_string())?;
    reservation_profile.check(&source)?;
    if reservation_profile.policy != costs.policy {
        return Err("Study reservation profile and saved source cost policies differ".into());
    }
    let reserves = reservation_profile
        .path_reservations
        .as_ref()
        .ok_or("Missing source placement policy")?;
    reserves.validate()?;
    if reserves.terrain_origin_y != paint.height_origin
        || reserves.terrain_height != paint.vertical_span
    {
        return Err("Bound height scales differ".into());
    }
    let ring_policy = source.profile.ring.ok_or("Missing ring policy")?;
    let shaping = source.profile.shaping.ok_or("Missing shaping policy")?;
    shaping.validate()?;
    let shape = path_geometry::Settings {
        horizontal_iterations: shaping.horizontal_iterations,
        vertical_iterations: shaping.vertical_iterations,
        resample_distance: shaping.resample_distance,
        maximum_work: shaping.maximum_work,
    };
    let ring_ref_bytes = read(path("source_ring_reference")?, 4 * 1024 * 1024)?;
    let layout_ref_bytes = read(path("source_layout_reference")?, 4 * 1024 * 1024)?;
    let ring_ref: Value = serde_json::from_slice(&ring_ref_bytes).map_err(|e| e.to_string())?;
    let layout_ref: Value = serde_json::from_slice(&layout_ref_bytes).map_err(|e| e.to_string())?;
    let paint_value = serde_json::to_value(&paint).map_err(|e| e.to_string())?;
    let server = &paint_value["reference_server_assembly_sha256"];
    if ring_ref["assembly_sha256"] != *server
        || layout_ref["assemblies"]["Assembly-CSharp.dll"] != *server
    {
        return Err("Owned control references differ from the selected compatibility profile's reference server".into());
    }
    let ring_depth = ring_ref["source_depth"]
        .as_u64()
        .ok_or("Missing owned ring literal")? as usize;
    let layout_cases = layout_ref["cases"]
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or("Missing owned layout control cases")?;
    let layout_depth = layout_cases[0]["source_depth"]
        .as_u64()
        .ok_or("Missing owned layout literal")? as usize;
    let push = layout_cases[0]["input"]["PushMultiplier"]
        .as_u64()
        .ok_or("Missing control-study push policy")? as u32;
    if layout_cases.iter().any(|c| {
        c["source_depth"].as_u64() != Some(layout_depth as u64)
            || c["input"]["PushMultiplier"].as_u64() != Some(push as u64)
    }) {
        return Err("Mixed reference layout policies".into());
    }
    let work = number("source_layout_maximum_work")?;
    let search = source_road_route::Settings {
        maximum_expansions: layout_depth,
        maximum_work: work,
        directional_penalty: joins.directional_penalty,
        directional_radius: joins.directional_radius_cells,
    };
    let policy = source_road_layout::Policy {
        network_sample_stride: ring_policy.network_sample_stride,
        push_multiplier: push,
        maximum_queries: number("source_layout_maximum_queries")?,
        maximum_node_storage: number("source_layout_maximum_nodes")?,
    };
    policy.validate()?;
    let height_bytes = checked("height.i16le")?;
    let topology_bytes = checked("topology.i32le")?;
    if height_bytes.len() % 2 != 0 || topology_bytes.len() % 4 != 0 {
        return Err("Invalid encoded terrain lengths".into());
    }
    let mut layers = Layers {
        size: project.size,
        terrain_seed: project.terrain_key().seed,
        biome_seed: project.terrain_key().biome_seed.unwrap_or(0),
        height_resolution: manifest["height_resolution"]
            .as_u64()
            .ok_or("Missing height resolution")? as usize,
        surface_resolution: manifest["surface_resolution"]
            .as_u64()
            .ok_or("Missing surface resolution")? as usize,
        height: height_bytes
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect(),
        topology: topology_bytes
            .chunks_exact(4)
            .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect(),
        biome: checked("biome.u8")?,
        splat: checked("splat.u8")?,
        normalized_biome_cells: 0,
        normalized_splat_cells: 0,
    };
    let fresh = match config.get("source_rebuild_pre_road") {
        None => false,
        Some(v) => v.as_bool().ok_or("Invalid pre-road rebuild flag")?,
    };
    let connected = match config.get("source_connected_entrances") {
        None => false,
        Some(value) => value.as_bool().ok_or("Invalid connected entrance flag")?,
    };
    let mut fixture = Value::Null;
    if fresh {
        let key = project.terrain_key();
        let lattice = papyrus_terrain::road_costmap::CellLattice::new(
            project.size as f32,
            joins.source_grid_spacing,
        )?;
        let inputs =
            source_road_inputs::prepare(&project, &saved, &assets, &roles, lattice, false, || {
                false
            })?
            .ok_or("Cancelled")?;
        let skipped: std::collections::BTreeSet<_> = inputs.skipped_roadside.into_iter().collect();
        project
            .targets
            .retain(|t| !skipped.contains(&t.instance_id));
        saved.outcomes.retain(|o| !skipped.contains(&o.id));
        if project.terrain_key() != key {
            return Err("Removing deferred roadside targets changed terrain identity".into());
        }
        eprintln!(
            "Rebuilding immutable terrain and composing fixed main-monument poses before roads"
        );
        let processed = native_processing::build(
            root,
            path("worker")?,
            &key,
            None,
            &AtomicBool::new(false),
            |label, _| eprintln!("{label}"),
        )?
        .ok_or("Cancelled")?;
        let maps = composition::compose_monuments(
            &processed.source,
            &project,
            &saved,
            &catalog,
            &bindings,
            || false,
        )?
        .ok_or("Cancelled")?;
        if maps.roads.is_some()
            || maps.rails.is_some()
            || maps.decoration.is_some()
            || maps.objects.is_some()
            || maps.spawn_audit.is_some()
            || maps.topology_overrides.is_some()
        {
            return Err("Pre-road revision contains later passes".into());
        }
        let mut saved_layout: Value =
            serde_json::from_slice(&layout_bytes).map_err(|e| e.to_string())?;
        saved_layout["outcomes"]
            .as_array_mut()
            .ok_or("Missing saved outcomes")?
            .retain(|o| o["id"].as_u64().is_some_and(|id| !skipped.contains(&id)));
        fixture = freeze(
            &project,
            &saved_layout,
            &maps,
            path("source_fresh_fixture")?,
        )?;
        layers = maps.layers;
    }
    let height_grid = path_terrain::Grid {
        size: project.size as f32,
        resolution: layers.height_resolution,
        origin_y: paint.height_origin,
        vertical_span: paint.vertical_span,
        height: &layers.height,
    };
    eprintln!("Preparing actual monument placement blockers");
    let mut placement = costs
        .prepare_blockers(&project, &saved, &catalog, &bindings, &source, || false)?
        .ok_or("Cancelled")?;
    let initial_placement_hash = hash(&placement);
    let effective = |name: &str, original: String| {
        if fresh {
            fixture["manifest"]["files"][name]["sha256"]
                .as_str()
                .unwrap_or("")
                .to_string()
        } else {
            original
        }
    };
    let mut report = json!({"format":"mapper-owned-source-road-network-study-1","size":project.size,"source_sha256":source.sha256,
        "assets_sha256":assets.sha256,"roles":roles,"project_sha256":effective("project.mapper",hash(&project_bytes)),"layout_sha256":effective("layout.json",hash(&layout_bytes)),
        "height_sha256":effective("height.i16le",hash(&height_bytes)),"topology_sha256":effective("topology.i32le",hash(&topology_bytes)),"initial_placement_sha256":initial_placement_hash,
        "reference_project_sha256":hash(&project_bytes),"fresh_pre_road_fixture":fixture,"source_rebuild_pre_road":fresh,"source_connected_entrances":connected,
        "ring_reference_sha256":hash(&ring_ref_bytes),"layout_reference_sha256":hash(&layout_ref_bytes),
        "reservation_profile_sha256":hash(&reservation_bytes),
        "search":search,"layout_policy":policy,"source_height_policy":paint.id,
        "scope":"Candidate layout with actual monument blockers and ordered posed connectors. Optional fresh pre-road terrain keeps fixed main-monument poses from the saved map and omits roadside stamps/later passes. Independent noise/seeds; owned-server references do not prove selected-client native method-body compatibility. No complete map, meshes or gameplay validation.",
        "gui_launched":false,"runtime_validated":false,"full_map_validated":false});
    let result = (|| -> Result<(), String> {
        let mut existing: Vec<(
            papyrus_terrain::road_costmap::CellLattice,
            Vec<source_road_ring::RawNode>,
        )> = Vec::new();
        let reserve_policy = |main: bool| rail_reservations::Policy {
            engine: rail_reservations::VERSION.into(),
            width: if main {
                reserves.main_road_width
            } else {
                reserves.width
            },
            sample_spacing: reserves.sample_spacing,
            terrain_origin_y: reserves.terrain_origin_y,
            terrain_height: reserves.terrain_height,
            spline: reserves.spline,
        };
        if recipe.main_ring.unwrap_or(true) {
            eprintln!("Preparing and routing source-lattice ring candidate");
            let grid = costs
                .build_cell_family(
                    &source,
                    &layers,
                    &placement,
                    project.seeds[3].value,
                    false,
                    || false,
                )?
                .ok_or("Cancelled")?;
            let outcome = source_road_ring::assemble(
                &grid,
                ring_policy,
                joins,
                source_road_route::Settings {
                    maximum_expansions: ring_depth,
                    ..search
                },
                work,
                || false,
            )?;
            report["ring_cost_counts"] = json!(grid.grid.counts);
            report["ring_cost_work"] = json!(grid.grid.work);
            match outcome {
                source_road_ring::Outcome::Found(ring) => {
                    let shaped = ring
                        .shape(
                            shape,
                            |uv| path_terrain::sample_height_uv(height_grid, uv),
                            || false,
                        )?
                        .ok_or("Cancelled")?;
                    let reserved = rail_reservations::generate(
                        project.size as f32,
                        layers.surface_resolution,
                        &placement,
                        &[rail_reservations::Track {
                            id: 1,
                            path: &shaped.path,
                        }],
                        &reserve_policy(true),
                        reserves.maximum_work,
                        || false,
                    )?
                    .ok_or("Cancelled")?;
                    report["ring"] = json!({"raw":ring,"shaped":shaped.path,"shape_work":shaped.total_work,"reservations":reserved.report});
                    placement = reserved.raster;
                    existing.push((ring.lattice, ring.nodes));
                }
                source_road_ring::Outcome::Skipped(a) => {
                    report["ring"] = json!({"skipped":a});
                }
                source_road_ring::Outcome::Unresolved(a) => {
                    report["ring"] = json!({"unresolved":a});
                    return Err("Configured source ring unresolved".into());
                }
                source_road_ring::Outcome::Budget(a) => {
                    report["ring"] = json!({"budget":a});
                    return Err("Source ring work exhausted".into());
                }
                source_road_ring::Outcome::Cancelled(_) => return Err("Cancelled".into()),
            }
        }
        let mut families = Vec::new();
        for trail in [false, true] {
            if trail && recipe.include_trails == Some(false) {
                continue;
            }
            eprintln!("Preparing source connector family: trail={trail}");
            let seed = if trail {
                let mut h = Sha256::new();
                h.update(b"papyrus-road-family-seed-1\0");
                h.update(project.seeds[3].value.to_le_bytes());
                h.update(
                    source
                        .profile
                        .trails
                        .as_ref()
                        .ok_or("Missing trail policy")?
                        .seed_domain
                        .as_bytes(),
                );
                u32::from_le_bytes(
                    h.finalize()[..4]
                        .try_into()
                        .map_err(|_| "Seed conversion")?,
                )
            } else {
                project.seeds[3].value
            };
            let grid = costs
                .build_cell_family(&source, &layers, &placement, seed, trail, || false)?
                .ok_or("Cancelled")?;
            let inputs = source_road_inputs::prepare(
                &project,
                &saved,
                &assets,
                &roles,
                grid.lattice,
                trail,
                || false,
            )?
            .ok_or("Cancelled")?;
            let paths = existing
                .iter()
                .map(|(lattice, nodes)| source_road_layout::ExistingPath {
                    lattice: *lattice,
                    nodes,
                })
                .collect::<Vec<_>>();
            let outcome = if connected {
                source_road_layout::generate_connected(
                    &grid,
                    &inputs.connectors,
                    &paths,
                    policy,
                    search,
                    work,
                    || false,
                )
            } else {
                source_road_layout::generate(
                    &grid,
                    &inputs.connectors,
                    &paths,
                    policy,
                    search,
                    work,
                    || false,
                )
            }?;
            let layout = match outcome {
                source_road_layout::Outcome::Found(layout) => layout,
                source_road_layout::Outcome::Budget(a) => {
                    families.push(json!({"trail":trail,"inputs":inputs,"budget":a}));
                    report["families"] = json!(families);
                    return Err("Source connector layout work exhausted".into());
                }
                source_road_layout::Outcome::Cancelled(_) => return Err("Cancelled".into()),
            };
            let mut shaped = Vec::new();
            let mut shape_work = 0;
            for segment in &layout.segments {
                let path = segment
                    .shape(
                        shape,
                        |id| {
                            inputs
                                .endpoints
                                .get(id)
                                .map(|e| [e.world[0], e.world[2]])
                                .ok_or("Invalid endpoint identity".into())
                        },
                        |uv| path_terrain::sample_height_uv(height_grid, uv),
                        || false,
                    )?
                    .ok_or("Cancelled")?;
                shape_work += path.total_work;
                shaped.push(path.path);
            }
            let tracks = shaped
                .iter()
                .enumerate()
                .map(|(i, path)| rail_reservations::Track {
                    id: i as u64 + 1,
                    path,
                })
                .collect::<Vec<_>>();
            let reserved = rail_reservations::generate(
                project.size as f32,
                layers.surface_resolution,
                &placement,
                &tracks,
                &reserve_policy(false),
                reserves.maximum_work,
                || false,
            )?
            .ok_or("Cancelled")?;
            placement = reserved.raster;
            families.push(json!({"trail":trail,"seed":seed,"inputs":inputs,"layout":layout,"shaped":shaped,
                "shape_work":shape_work,"reservations":reserved.report,"cost_counts":grid.grid.counts,"cost_work":grid.grid.work}));
            existing.extend(layout.segments.into_iter().map(|s| (s.lattice, s.nodes)));
        }
        report["families"] = json!(families);
        Ok(())
    })();
    report["layout_study_succeeded"] = result.is_ok().into();
    report["error"] = result.err().map_or(Value::Null, Value::String);
    report["final_placement_sha256"] = hash(&placement).into();
    if checked("project.mapper")? != project_bytes
        || checked("layout.json")? != layout_bytes
        || checked("height.i16le")? != height_bytes
        || checked("topology.i32le")? != topology_bytes
        || read(&bundle.join("manifest.json"), 4 * 1024 * 1024)? != manifest_bytes
    {
        return Err("Saved inputs changed during study".into());
    }
    fs::File::options()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| e.to_string())?
        .write_all(&serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    println!(
        "Candidate source layout study completed; accepted stages={}, report {}",
        report["layout_study_succeeded"],
        output.display()
    );
    Ok(())
}
