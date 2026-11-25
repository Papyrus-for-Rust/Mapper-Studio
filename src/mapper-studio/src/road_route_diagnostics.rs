//! Developer-only pristine route environments and full-family numeric audits.
use crate::{
    Project, road_generation::Context, road_socket_recovery,
    road_transaction_diagnostics::write_words,
};
use papyrus_terrain::{
    monument_composition::Composed,
    native_layers::Layers,
    road_network::{self, socket_recovery},
    road_water,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

fn capture_destination(
    single: Option<PathBuf>,
    root: Option<PathBuf>,
    layout: usize,
) -> Result<Option<PathBuf>, String> {
    match (single, root) {
        (Some(_), Some(_)) => {
            Err("Choose a single route capture or a per-layout capture root".into())
        }
        (Some(path), None) => Ok(Some(path)),
        (None, Some(root)) => Ok(Some(root.join(format!("layout-{layout}")))),
        (None, None) => Ok(None),
    }
}

pub fn capture_if_requested(
    context: Context<'_>,
    base: &Composed,
    plan: &road_network::Report,
    layout_index: usize,
    environment: Option<&road_socket_recovery::Environment<'_>>,
    cancelled: impl Fn() -> bool,
) -> Result<bool, String> {
    let Some(destination) = capture_destination(
        std::env::var_os("MAPPER_ROAD_ROUTE_CAPTURE").map(PathBuf::from),
        std::env::var_os("MAPPER_ROAD_ROUTE_CAPTURES").map(PathBuf::from),
        layout_index,
    )?
    else {
        return Ok(true);
    };
    let destination = destination.as_path();
    if destination.exists() {
        return Err("Route capture destination already exists".into());
    }
    let environment = environment.ok_or("Route capture requires a prepared road environment")?;
    let input = environment.input_with_source_policy(None, None);
    let parent = destination.parent().ok_or("Route capture parent missing")?;
    let private = tempfile::Builder::new()
        .prefix(".road-route-")
        .tempdir_in(parent)
        .map_err(|e| e.to_string())?;
    let mut files = BTreeMap::new();
    files.insert(
        "height.i16le",
        write_words(
            private.path(),
            "height.i16le",
            base.layers.height.iter().map(|v| v.to_le_bytes()),
            &cancelled,
        )?,
    );
    files.insert(
        "topology.i32le",
        write_words(
            private.path(),
            "topology.i32le",
            base.layers.topology.iter().map(|v| v.to_le_bytes()),
            &cancelled,
        )?,
    );
    for (name, values) in [
        ("splat.u8", &base.layers.splat),
        ("biome.u8", &base.layers.biome),
        ("alpha.u8", &base.alpha),
    ] {
        files.insert(
            name,
            write_words(
                private.path(),
                name,
                values.iter().map(|v| [*v]),
                &cancelled,
            )?,
        );
    }
    let recipe = context
        .project
        .roads
        .as_ref()
        .ok_or("Route recipe missing")?;
    let ring_water = recipe
        .ring_costmap
        .as_ref()
        .and_then(|p| p.joins.as_ref())
        .and_then(|j| j.river_water.as_ref());
    let needs_water = ring_water.is_some() || recipe.access_river_water.is_some();
    let water = if needs_water {
        if let Some(rivers) = &base.rivers {
            let Some(water) = rivers.collider_heights(
                base.layers.size,
                base.layers.surface_resolution,
                &cancelled,
            )?
            else {
                return Ok(false);
            };
            Some(water.heights)
        } else {
            None
        }
    } else {
        None
    };
    if let Some(water) = &water {
        files.insert(
            "river-levels.f32le",
            write_words(
                private.path(),
                "river-levels.f32le",
                water.iter().map(|v| v.to_le_bytes()),
                &cancelled,
            )?,
        );
    }
    let data = json!({"format":"mapper-pristine-road-route-environment-2","schema":2,
        "layout_index":layout_index,
        "project_document":serde_json::to_string(context.project).map_err(|e|e.to_string())?,
        "size":base.layers.size,"height_resolution":base.layers.height_resolution,"surface_resolution":base.layers.surface_resolution,
        "height_origin":input.height_origin,"vertical_span":input.vertical_span,"settings_without_water":input.settings,
        "source_family_settings":environment.source_family_settings(),
        "ownership":input.ownership,"launch":input.launch,"owners":input.owners,"obstacles":input.obstacles,
        "planning_report":plan.json(),"ring_water_policy":ring_water,"access_water_policy":recipe.access_river_water,
        "water_levels_available":water.is_some(),"files":files,"runtime_validated":false});
    let raw = serde_json::to_vec(&data).map_err(|e| e.to_string())?;
    if raw.len() > 64 * 1024 * 1024 {
        return Err("Route capture metadata exceeds allowance".into());
    }
    let mut file =
        fs::File::create(private.path().join("environment.json")).map_err(|e| e.to_string())?;
    file.write_all(&raw)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    drop(file);
    if cancelled() {
        return Ok(false);
    }
    fs::rename(private.path(), destination).map_err(|e| e.to_string())?;
    eprintln!(
        "Captured pristine route environment: {}",
        destination.display()
    );
    Ok(true)
}
fn decode<T: DeserializeOwned>(v: &Value) -> Result<T, String> {
    serde_json::from_value(v.clone()).map_err(|e| e.to_string())
}
fn read(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut raw = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() as u64 > limit {
        return Err("Route input exceeds allowance".into());
    }
    Ok(raw)
}
fn hash(raw: &[u8]) -> String {
    format!("{:x}", Sha256::digest(raw))
}
fn payload(root: &Path, files: &Value, name: &str) -> Result<Vec<u8>, String> {
    let canonical = root.canonicalize().map_err(|e| e.to_string())?;
    let path = root.join(name).canonicalize().map_err(|e| e.to_string())?;
    if !path.starts_with(canonical) {
        return Err("Route payload escapes its root".into());
    }
    let raw = read(&path, 128 * 1024 * 1024)?;
    let receipt = &files[name];
    if receipt["bytes"] != raw.len() as u64 || receipt["sha256"] != hash(&raw) {
        return Err(format!("Invalid route payload receipt: {name}"));
    }
    Ok(raw)
}
fn destination(v: &Value) -> Result<road_network::Destination, String> {
    let object = v.as_object().ok_or("Invalid route destination")?;
    if object.contains_key("source_network") {
        return Err(
            "Raw source connections require reconstruction from the trusted source handoff".into(),
        );
    }
    if object.len() == 1 && v["loop"] == true {
        return Ok(road_network::Destination::Loop);
    }
    if object.len() == 2 {
        if let (Some(monument), Some(connector)) = (v["monument"].as_u64(), v["connector"].as_i64())
        {
            return Ok(road_network::Destination::Monument {
                monument,
                connector,
            });
        }
        if let (Some(road), Some(point)) = (v["network_road"].as_u64(), v["network_point"].as_u64())
        {
            return Ok(road_network::Destination::Network {
                road: road.try_into().map_err(|_| "Invalid route road index")?,
                point: point.try_into().map_err(|_| "Invalid route point index")?,
            });
        }
    }
    Err("Invalid route destination".into())
}
fn floats(raw: Vec<u8>) -> Result<Vec<f32>, String> {
    if !raw.len().is_multiple_of(4) {
        return Err("Partial route float".into());
    }
    Ok(raw
        .chunks_exact(4)
        .map(|v| f32::from_le_bytes(v.try_into().unwrap()))
        .collect())
}
fn shorts(raw: Vec<u8>) -> Result<Vec<i16>, String> {
    if !raw.len().is_multiple_of(2) {
        return Err("Partial route short".into());
    }
    Ok(raw
        .chunks_exact(2)
        .map(|v| i16::from_le_bytes(v.try_into().unwrap()))
        .collect())
}
/// Replay just main-road planning on captured pristine terrain. No native worker,
/// monument placement, road painting refinement or complete-world claim.
pub fn replay_main_ring(
    capture: &Path,
    infrastructure: &Path,
    output: &Path,
) -> Result<Value, String> {
    if output.exists() {
        return Err("Main ring replay output exists".into());
    }
    let raw = read(&capture.join("environment.json"), 64 * 1024 * 1024)?;
    let data: Value = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    if data["format"] != "mapper-pristine-road-route-environment-2" || data["schema"] != 2 {
        return Err("Unsupported main ring replay environment".into());
    }
    let project: Project = serde_json::from_str(
        data["project_document"]
            .as_str()
            .ok_or("Missing replay project")?,
    )
    .map_err(|e| e.to_string())?;
    project.validate()?;
    let recipe = project.roads.as_ref().ok_or("Missing replay recipe")?;
    if recipe
        .entrance_launch
        .is_none_or(|p| p.engine != road_network::LaunchEngine::GlobalEarthwork)
    {
        return Err("Main ring replay requires engine 8".into());
    }
    let source_raw = read(infrastructure, 64 * 1024 * 1024)?;
    if hash(&source_raw) != recipe.source_sha256 {
        return Err("Replay infrastructure pin differs".into());
    }
    let source: Value = serde_json::from_slice(&source_raw).map_err(|e| e.to_string())?;
    let profile = &source["profile"];
    let (hr, sr) = papyrus_terrain::native_layers::dimensions(project.size)?;
    let receipts = &data["files"];
    let topo = payload(capture, receipts, "topology.i32le")?;
    if !topo.len().is_multiple_of(4) {
        return Err("Partial replay topology".into());
    }
    let mut layers = Layers {
        size: project.size,
        terrain_seed: project.seeds[0].value,
        biome_seed: project.seeds[1].value,
        height_resolution: hr,
        surface_resolution: sr,
        height: shorts(payload(capture, receipts, "height.i16le")?)?,
        topology: topo
            .chunks_exact(4)
            .map(|v| i32::from_le_bytes(v.try_into().unwrap()))
            .collect(),
        splat: payload(capture, receipts, "splat.u8")?,
        biome: payload(capture, receipts, "biome.u8")?,
        normalized_biome_cells: 0,
        normalized_splat_cells: 0,
    };
    let alpha = payload(capture, receipts, "alpha.u8")?;
    let obstacles: Vec<road_network::Obstacle> = decode(&data["obstacles"])?;
    let settings: road_network::Settings = decode(&data["source_family_settings"]["0"])?;
    let ownership: road_network::EntrancePolicy = decode(&data["ownership"])?;
    let traversal: road_network::TraversalPolicy = decode(&profile["access_routing"])?;
    let policy: papyrus_terrain::road_ring::Policy = decode(&profile["ring"])?;
    let shaping: road_network::ShapingPolicy = decode(&profile["shaping"])?;
    let costs: Vec<u32> = decode(&data["planning_report"]["ring_costmap"]["grid"]["costs"])?;
    let joins = recipe.ring_costmap.as_ref().and_then(|p| p.joins.as_ref());
    let water_policy: Option<road_water::Policy> = decode(&data["ring_water_policy"])?;
    let water = if data["water_levels_available"] == true {
        Some(floats(payload(capture, receipts, "river-levels.f32le")?)?)
    } else {
        None
    };
    let guide = water_policy
        .as_ref()
        .zip(water.as_deref())
        .map(|(policy, heights)| road_water::Guide { policy, heights });
    // The ring builder paints its private terrain. Retain the actual pre-road
    // rasters for the independent, complete source-curve footprint check.
    let pristine = layers.clone();
    let mut report = road_network::build_ring_with_global_earthwork(
        &mut layers,
        &alpha,
        &obstacles,
        settings,
        policy,
        shaping,
        Some((traversal, ownership.owned_topology)),
        Some(&costs),
        joins,
        guide,
        recipe
            .entrance_launch
            .ok_or("Missing global launch policy")?,
        || false,
    )?
    .ok_or("Ring replay cancelled")?;
    let owners: Vec<road_network::Obstacle> = decode(&data["owners"])?;
    let launch: road_network::LaunchPolicy = decode(&data["launch"])?;
    if recipe.entrance_launch != Some(launch) {
        return Err("Main ring replay launch pin differs".into());
    }
    // The standalone ring builder has no entrance phase. Attach the exact
    // captured recipe policy for the shared source-family validator, just as
    // full planning does when combining ring and access reports.
    report.entrance_launch = Some(launch);
    let families: Vec<road_network::Family> = decode(&data["planning_report"]["families"])?;
    let mut families = families
        .into_iter()
        .filter(|f| f.hierarchy == 0)
        .collect::<Vec<_>>();
    if families.len() != 1
        || families[0].width != settings.width
        || families[0].splat_channel != settings.splat_channel
    {
        return Err("Main ring replay family pin differs".into());
    }
    families[0].first_road = 0;
    families[0].road_count = report.roads.len();
    report.families = families;
    let points = report
        .roads
        .iter()
        .map(|r| r.points.clone())
        .collect::<Vec<_>>();
    let selected = (0..points.len()).collect::<Vec<_>>();
    let footprint = if selected.is_empty() {
        json!({"passed":false,"failure":"No main ring was admitted"})
    } else {
        let environment = socket_recovery::Environment {
            layers: &pristine,
            alpha: &alpha,
            obstacles: &obstacles,
            owners: &owners,
            settings: road_network::Settings {
                protected_topology: settings.protected_topology
                    & !guide.map_or(0, |g| g.policy.topology),
                ..settings
            },
            ownership,
            launch,
            costs: Some(&costs),
            cost_lattice: None,
            water: guide,
            height_origin: decode(&data["height_origin"])?,
            vertical_span: decode(&data["vertical_span"])?,
        };
        match socket_recovery::validate_road_set(
            &report,
            &environment,
            &selected,
            &points,
            128_000_000,
            || false,
        ) {
            Ok(Some(audit)) => json!({"passed":true,"audit":audit}),
            Ok(None) => return Err("Main ring footprint unexpectedly cancelled".into()),
            Err(failure) => json!({"passed":false,"failure":failure}),
        }
    };
    let result = json!({"format":"mapper-pristine-main-ring-replay-1","environment_sha256":hash(&raw),"source_sha256":hash(&source_raw),
        "roads":report.json(),"whole_ring_footprint":footprint,"scope":"main ring route preflight only; not a generated world","world_generation_validated":false,"runtime_validated":false});
    let mut out = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(&mut out, &result).map_err(|e| e.to_string())?;
    out.write_all(b"\n")
        .and_then(|_| out.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(result)
}
/// Data-only replay over the actual captured ground and published final controls.
/// Semantic route failures are included in a new audit report, never called success.
pub fn verify(capture: &Path, bundle: &Path, output: &Path) -> Result<Value, String> {
    verify_with_controls(capture, bundle, output, false)
}
pub fn verify_with_controls(
    capture: &Path,
    bundle: &Path,
    output: &Path,
    planned_controls: bool,
) -> Result<Value, String> {
    if output.exists() {
        return Err("Route report already exists".into());
    }
    let raw = read(&capture.join("environment.json"), 64 * 1024 * 1024)?;
    let data: Value = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    if data["format"] != "mapper-pristine-road-route-environment-2" || data["schema"] != 2 {
        return Err("Unsupported route capture".into());
    }
    let manifest_raw = read(&bundle.join("manifest.json"), 64 * 1024 * 1024)?;
    let manifest: Value = serde_json::from_slice(&manifest_raw).map_err(|e| e.to_string())?;
    if manifest["format"] != "papyrus-preview-bundle" || manifest["playable"] != false {
        return Err("Unsupported route bundle".into());
    }
    let files = &manifest["files"];
    for name in files.as_object().ok_or("Missing bundle receipts")?.keys() {
        payload(bundle, files, name)?;
    }
    let project: Project = serde_json::from_slice(&payload(bundle, files, "project.mapper")?)
        .map_err(|e| e.to_string())?;
    project.validate()?;
    let original: Project = serde_json::from_str(
        data["project_document"]
            .as_str()
            .ok_or("Missing captured project")?,
    )
    .map_err(|e| e.to_string())?;
    if original != project {
        return Err("Route capture belongs to a different project".into());
    }
    let recipe = project.roads.as_ref().ok_or("Missing route recipe")?;
    let (hr, sr) = papyrus_terrain::native_layers::dimensions(project.size)?;
    if data["size"] != project.size
        || data["height_resolution"] != hr
        || data["surface_resolution"] != sr
    {
        return Err("Route capture dimensions differ".into());
    }
    let receipts = &data["files"];
    let topo = payload(capture, receipts, "topology.i32le")?;
    if !topo.len().is_multiple_of(4) {
        return Err("Partial route topology".into());
    }
    let layers = Layers {
        size: project.size,
        terrain_seed: project.seeds[0].value,
        biome_seed: project.seeds[1].value,
        height_resolution: hr,
        surface_resolution: sr,
        height: shorts(payload(capture, receipts, "height.i16le")?)?,
        topology: topo
            .chunks_exact(4)
            .map(|v| i32::from_le_bytes(v.try_into().unwrap()))
            .collect(),
        splat: payload(capture, receipts, "splat.u8")?,
        biome: payload(capture, receipts, "biome.u8")?,
        normalized_splat_cells: 0,
        normalized_biome_cells: 0,
    };
    let alpha = payload(capture, receipts, "alpha.u8")?;
    let owners: Vec<road_network::Obstacle> = decode(&data["owners"])?;
    let obstacles: Vec<road_network::Obstacle> = decode(&data["obstacles"])?;
    let family_settings: BTreeMap<u32, road_network::Settings> =
        decode(&data["source_family_settings"])?;
    let ownership: road_network::EntrancePolicy = decode(&data["ownership"])?;
    let launch: road_network::LaunchPolicy = decode(&data["launch"])?;
    if recipe.entrance_launch != Some(launch) {
        return Err("Route launch pin differs".into());
    }
    let road_value: Value = serde_json::from_slice(&payload(bundle, files, "roads.json")?)
        .map_err(|e| e.to_string())?;
    let planned = &data["planning_report"];
    for key in [
        "families",
        "ring_costmap",
        "source_access_costmaps",
        "entrance_launch",
    ] {
        if planned[key] != road_value[key] {
            return Err(format!("Route source family metadata differs: {key}"));
        }
    }
    let original_roads = planned["roads"]
        .as_array()
        .ok_or("Missing original routes")?;
    let final_roads = road_value["roads"]
        .as_array()
        .ok_or("Missing final routes")?;
    if original_roads.len() != final_roads.len() || original_roads.len() > 256 {
        return Err("Route roster differs".into());
    }
    let mut report = road_network::Report {
        entrance_launch: Some(launch),
        entrance_joins: planned
            .get("entrance_joins")
            .map(decode)
            .transpose()?
            .unwrap_or_default(),
        families: decode(&road_value["families"])?,
        ..Default::default()
    };
    let mut points = Vec::new();
    for (old, new) in original_roads.iter().zip(final_roads) {
        if old["from"] != new["from"] || old["to"] != new["to"] {
            return Err("Route endpoint identities differ".into());
        }
        report.roads.push(road_network::Road {
            from: destination(&old["from"])?,
            to: destination(&old["to"])?,
            points: decode(&old["points"])?,
        });
        points.push(decode::<Vec<[f32; 3]>>(
            &if planned_controls { old } else { new }["points"],
        )?);
    }
    let water = if data["water_levels_available"] == true {
        Some(floats(payload(capture, receipts, "river-levels.f32le")?)?)
    } else {
        None
    };
    let ring_water: Option<road_water::Policy> = decode(&data["ring_water_policy"])?;
    let access_water: Option<road_water::Policy> = decode(&data["access_water_policy"])?;
    if ring_water.as_ref()
        != recipe
            .ring_costmap
            .as_ref()
            .and_then(|p| p.joins.as_ref())
            .and_then(|j| j.river_water.as_ref())
        || access_water != recipe.access_river_water
    {
        return Err("Route water policy differs".into());
    }
    let mut audits = Vec::new();
    let mut all_passed = true;
    let mut coverage = vec![false; points.len()];
    for family in &report.families {
        if family.road_count == 0 {
            continue;
        }
        let end = family
            .first_road
            .checked_add(family.road_count)
            .filter(|v| *v <= points.len())
            .ok_or("Invalid route family range")?;
        let selected = (family.first_road..end).collect::<Vec<_>>();
        for &i in &selected {
            if std::mem::replace(&mut coverage[i], true) {
                return Err("Overlapping route families".into());
            }
        }
        let settings = *family_settings
            .get(&family.hierarchy)
            .ok_or("Missing source family shape")?;
        if family.width != settings.width || family.splat_channel != settings.splat_channel {
            return Err("Route family shape differs from source".into());
        }
        let cost_profile = if family.hierarchy == 0 {
            recipe.ring_costmap.as_ref()
        } else {
            recipe.access_costmap.as_ref()
        };
        let grid = if cost_profile.is_none() {
            None
        } else if family.hierarchy == 0 {
            Some(&road_value["ring_costmap"]["grid"])
        } else {
            let label = match family.hierarchy {
                1 => "paved",
                2 => "dirt",
                _ => return Err("Unsupported route hierarchy".into()),
            };
            Some(
                &road_value["source_access_costmaps"]
                    .as_array()
                    .ok_or("Missing family routing grids")?
                    .iter()
                    .find(|v| v["family"] == label)
                    .ok_or("Missing matching family grid")?["grid"],
            )
        };
        let costs = if let Some(grid) = grid {
            if grid["engine"] != papyrus_terrain::road_costmap::VERSION
                || grid["resolution"] != 257
                || grid["seed"] != family.seed
                || grid["trail"] != (family.hierarchy == 2)
            {
                return Err("Route cost grid binding differs".into());
            }
            Some(decode::<Vec<u32>>(&grid["costs"])?)
        } else {
            None
        };
        let policy = match family.hierarchy {
            0 => ring_water.as_ref(),
            1 => access_water.as_ref(),
            2 => None,
            _ => return Err("Unsupported route hierarchy".into()),
        };
        let guide = policy
            .zip(water.as_deref())
            .map(|(policy, heights)| road_water::Guide { policy, heights });
        let environment = socket_recovery::Environment {
            layers: &layers,
            alpha: &alpha,
            obstacles: &obstacles,
            owners: &owners,
            settings: road_network::Settings {
                seed: family.seed,
                protected_topology: settings.protected_topology
                    & !guide.map_or(0, |g| g.policy.topology),
                ..settings
            },
            ownership,
            launch,
            costs: costs.as_deref(),
            cost_lattice: None,
            water: guide,
            height_origin: decode(&data["height_origin"])?,
            vertical_span: decode(&data["vertical_span"])?,
        };
        let result = socket_recovery::validate_road_set(
            &report,
            &environment,
            &selected,
            &points,
            128_000_000,
            || false,
        );
        let result = match result {
            Ok(Some(audit)) => json!({"passed":true,"audit":audit}),
            Ok(None) => return Err("Route audit unexpectedly cancelled".into()),
            Err(error) => {
                all_passed = false;
                json!({"passed":false,"failure":error})
            }
        };
        audits.push(json!({"hierarchy":family.hierarchy,"roads":selected,"source_cost_grid_sha256":costs.as_ref().map(|v|serde_json::to_vec(v).map(|raw|hash(&raw))).transpose().map_err(|e|e.to_string())?,"water_policy":policy,"water_guide_available":guide.is_some(),"result":result}));
    }
    if coverage.iter().any(|v| !*v) {
        return Err("Incomplete full route coverage".into());
    }
    let result = json!({"format":"mapper-full-road-route-audit-1","environment_sha256":hash(&raw),"manifest_sha256":hash(&manifest_raw),"controls":if planned_controls {"original plan"}else{"published final"},
        "pristine_ground":true,"roads":points.len(),"all_routes_passed":all_passed,"families":audits,
        "runtime_validated":false,"playable":false});
    let mut out = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(&mut out, &result).map_err(|e| e.to_string())?;
    out.write_all(b"\n")
        .and_then(|_| out.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(result)
}
