//! CONFIG PLANNING_CAPTURE FAILURE_REPORT NEW_REPORT. No GUI/game/server.
use mapper_studio::{
    Project, composition, infrastructure, native_processing, placement_preview, prefab_assets,
    prefab_textures, road_generation, road_socket_recovery,
};
use papyrus_terrain::road_network::{self, Destination, socket_recovery};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, io::Write, path::Path, sync::atomic::AtomicBool};
fn write_study(path: &Path, value: &Value) -> Result<(), String> {
    let raw = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if raw.len() > 32 * 1024 * 1024 {
        return Err("Diagnostic exceeds 32 MiB".into());
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    file.write_all(&raw).map_err(|e| e.to_string())?;
    file.as_file().sync_all().map_err(|e| e.to_string())?;
    file.persist_noclobber(path)
        .map_err(|e| e.error.to_string())?;
    Ok(())
}
fn read(p: &Path) -> Result<Value, String> {
    serde_json::from_slice(&fs::read(p).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}
fn destination(v: &Value) -> Result<Destination, String> {
    if v["loop"] == true {
        return Ok(Destination::Loop);
    }
    if let (Some(monument), Some(connector)) = (v["monument"].as_u64(), v["connector"].as_i64()) {
        return Ok(Destination::Monument {
            monument,
            connector,
        });
    }
    if let (Some(road), Some(point)) = (v["network_road"].as_u64(), v["network_point"].as_u64()) {
        return Ok(Destination::Network {
            road: road as usize,
            point: point as usize,
        });
    }
    Err("Unsupported destination".into())
}
fn main() -> Result<(), String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 4 {
        return Err("CONFIG PLANNING_CAPTURE FAILURE_REPORT NEW_REPORT".into());
    }
    let output = Path::new(&args[3]);
    if output.exists() {
        return Err("Output must be new".into());
    }
    let config = read(Path::new(&args[0]))?;
    let path = |key: &str| {
        config[key]
            .as_str()
            .map(Path::new)
            .ok_or(format!("Missing {key}"))
    };
    let mut project = Project::load(path("project")?)?;
    let roadside = fs::read(path("roadside_source")?).map_err(|e| e.to_string())?;
    project.roads.as_mut().ok_or("Missing roads")?.staged = Some(road_generation::Policy {
        layout: None,
        engine: road_generation::VERSION.into(),
        roadside_source_sha256: format!("{:x}", Sha256::digest(&roadside)),
        roadside_preferences: Vec::new(),
    });
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
    let bindings = prefab_textures::Bindings::load(root, path("bindings")?, &catalog, || false)?;
    let raw = native_processing::build(
        root,
        path("worker")?,
        &project.terrain_key(),
        None,
        &AtomicBool::new(false),
        |label, _| eprintln!("{label}"),
    )?
    .ok_or("Cancelled")?;
    let solution =
        placement_preview::solve_native(&project, &raw.source.layers, &catalog, &bindings, || {
            false
        })?
        .ok_or("Cancelled")?;
    let maps = composition::compose_monuments(
        &raw.source,
        &project,
        &solution,
        &catalog,
        &bindings,
        || false,
    )?
    .ok_or("Cancelled")?;

    let planning = read(&Path::new(&args[1]).join("capture.json"))?;
    let raw = maps
        .layers
        .height
        .iter()
        .flat_map(|h| h.to_le_bytes())
        .collect::<Vec<_>>();
    let ground_hash = format!("{:x}", Sha256::digest(&raw));
    if planning["files"]["ground.i16le"]["sha256"].as_str() != Some(ground_hash.as_str()) {
        return Err("Reproduced pre-road terrain differs from planning capture".into());
    }
    let source =
        infrastructure::Source::cached(&catalog, project.roads.as_ref().unwrap(), || false)?;
    let m = &planning["roads"];
    let report = road_network::Report {
        entrance_launch: serde_json::from_value(m["entrance_launch"].clone())
            .map_err(|e| e.to_string())?,
        entrance_policy: serde_json::from_value(m["entrance_policy"].clone())
            .map_err(|e| e.to_string())?,
        families: serde_json::from_value(m["families"].clone()).map_err(|e| e.to_string())?,
        roads: m["roads"]
            .as_array()
            .ok_or("Missing roads")?
            .iter()
            .map(|r| {
                Ok(road_network::Road {
                    from: destination(&r["from"])?,
                    to: destination(&r["to"])?,
                    points: serde_json::from_value(r["points"].clone())
                        .map_err(|e| e.to_string())?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?,
        ..Default::default()
    };
    let failure = read(Path::new(&args[2]))?;
    let witness = &failure["error"]["ClearanceInfeasible"];
    let road_index = witness["road"].as_u64().ok_or("Missing failed road")? as usize;
    let road = report.roads.get(road_index).ok_or("Unknown failed road")?;
    let position: [f64; 2] =
        serde_json::from_value(witness["position"].clone()).map_err(|e| e.to_string())?;
    let mut endpoints = Vec::new();
    for (d, p) in [
        (road.from, road.points.first()),
        (road.to, road.points.last()),
    ] {
        if let (
            Destination::Monument {
                monument,
                connector,
            },
            Some(p),
        ) = (d, p)
        {
            endpoints.push((
                (f64::from(p[0]) - position[0]).hypot(f64::from(p[2]) - position[1]),
                socket_recovery::Key {
                    monument,
                    connector,
                },
            ));
        }
    }
    endpoints.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (distance, key) = endpoints
        .first()
        .ok_or("No monument endpoint near failure")?;
    let family = report
        .families
        .iter()
        .find(|f| road_index >= f.first_road && road_index < f.first_road + f.road_count)
        .ok_or("No failed family")?;
    if *distance > f64::from(family.width * 2.) {
        return Err("Witness is not at an authored socket".into());
    }
    let environment =
        road_socket_recovery::prepare(&project, &solution, &catalog, &bindings, &maps, || false)?
            .ok_or("Cancelled")?;
    let mut variants = Vec::new();
    for degrees in
        socket_recovery::angles(&report, *key).map_err(|e| format!("Socket fan: {e:?}"))?
    {
        eprintln!("Checking socket fan {degrees} degrees");
        match socket_recovery::propose(
            &report,
            &environment.input(),
            *key,
            degrees,
            2_000_000,
            || false,
        ) {
            Ok(Some(p)) => {
                variants.push(json!({"degrees":degrees,"preflight_passed":true,"audit":p.audit()}))
            }
            Ok(None) => return Err("Cancelled".into()),
            Err(e) => variants
                .push(json!({"degrees":degrees,"preflight_passed":false,"error":format!("{e:?}"),"failure":e})),
        }
    }
    write_study(
        output,
        &json!({"format":"mapper-installation-socket-preflight-1","ground_sha256":ground_hash,"catalog_sha256":catalog.sha256,"bindings_sha256":bindings.sha256,"infrastructure_sha256":source.sha256,"socket":key,"owners":environment.ownership_footprints().iter().map(|o|json!({"monument":o.monument,"center":o.center,"extent":o.extent,"yaw":o.yaw})).collect::<Vec<_>>(),"failed_road":road_index,"witness_distance":distance,"variants":variants,"world_accepted":false,"scope":"Checked geometry on reproduced pre-road terrain; candidate painting, source mesh fitting, roadside reselection and whole-world acceptance remain required"}),
    )?;
    Ok(())
}
