//! CONFIG FIT_CAPTURE PLANNING_CAPTURE NEW_REPORT [--quantized].
//! Runs the complete source-backed refinement loop on a private candidate.
//! Does not publish a map or approve route/placement changes.
use mapper_studio::{
    Project, infrastructure, prefab_assets, road_refinement, road_surface_source,
    road_surface_stage,
};
use papyrus_terrain::{
    native_layers::Layers,
    road_network::{Destination, Report, Road},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, io::Write, path::Path};
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
    let mut args = std::env::args().skip(1).collect::<Vec<_>>();
    let quantized = args.last().is_some_and(|v| v == "--quantized");
    if quantized {
        args.pop();
    }
    if args.len() != 4 {
        return Err("CONFIG FIT_CAPTURE PLANNING_CAPTURE NEW_REPORT".into());
    }
    let output = Path::new(&args[3]);
    if output.exists() {
        return Err("Report already exists".into());
    }
    let cfg = read(Path::new(&args[0]))?;
    let path = |key: &str| {
        cfg[key]
            .as_str()
            .map(Path::new)
            .ok_or(format!("Missing {key}"))
    };
    let capture = read(&Path::new(&args[1]).join("capture.json"))?;
    let planning = read(&Path::new(&args[2]).join("capture.json"))?;
    if capture["format"] != "mapper-private-road-fit-capture-1"
        || planning["format"] != "mapper-global-road-earthwork-capture"
    {
        return Err("Unsupported captures".into());
    }
    let project = Project::load(path("project")?)?;
    let planned_project: Project =
        serde_json::from_value(planning["project"].clone()).map_err(|e| e.to_string())?;
    if project != planned_project {
        return Err("Project and plan differ".into());
    }
    let bytes = |name: &str| -> Result<Vec<u8>, String> {
        let raw = fs::read(Path::new(&args[1]).join(name)).map_err(|e| e.to_string())?;
        if capture["files"][name]["sha256"].as_str()
            != Some(format!("{:x}", Sha256::digest(&raw)).as_str())
        {
            return Err(format!("Changed receipt: {name}"));
        }
        Ok(raw)
    };
    let shorts = |name: &str| -> Result<Vec<i16>, String> {
        let raw = bytes(name)?;
        if raw.len() % 2 != 0 {
            return Err("Invalid short grid".into());
        }
        Ok(raw
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect())
    };
    let (hr, sr) = papyrus_terrain::native_layers::dimensions(project.size)?;
    let height = shorts("height.i16le")?;
    let ground = shorts("ground.i16le")?;
    let painted = shorts("painted.i16le")?;
    let raw_topology = bytes("topology.i32le")?;
    if height.len() != hr * hr
        || ground.len() != height.len()
        || painted.len() != height.len()
        || raw_topology.len() != sr * sr * 4
    {
        return Err("Grid dimensions differ".into());
    }
    let topology = raw_topology
        .chunks_exact(4)
        .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect::<Vec<_>>();
    let layers = Layers {
        size: project.size,
        terrain_seed: project.seeds[0].value,
        biome_seed: project.seeds[1].value,
        height_resolution: hr,
        surface_resolution: sr,
        height,
        biome: vec![],
        splat: vec![],
        topology: vec![],
        normalized_biome_cells: 0,
        normalized_splat_cells: 0,
    };
    let controls: Vec<Vec<[f32; 3]>> =
        serde_json::from_value(capture["controls"].clone()).map_err(|e| e.to_string())?;
    let metadata = planning["roads"]["roads"]
        .as_array()
        .ok_or("Missing roads")?;
    if metadata.len() != controls.len() {
        return Err("Road count differs".into());
    }
    let roads = Report {
        roads: metadata
            .iter()
            .zip(controls)
            .map(|(m, points)| {
                Ok(Road {
                    from: destination(&m["from"])?,
                    to: destination(&m["to"])?,
                    points,
                })
            })
            .collect::<Result<Vec<_>, String>>()?,
        families: serde_json::from_value(planning["roads"]["families"].clone())
            .map_err(|e| e.to_string())?,
        ..Default::default()
    };
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
    let parent = infrastructure::Source::load(path("infrastructure")?, &catalog, || false)?;
    parent.verify_installation(root, || false)?;
    let source = road_surface_source::Source::read(path("mesh_source")?, &parent, &|| false)?;
    source.verify_installation(root, || false)?;
    let geometry = road_surface_stage::HeightGeometry {
        origin_y: capture["origin_y"]
            .as_f64()
            .ok_or("Missing height origin")? as f32,
        vertical_span: capture["vertical_span"]
            .as_f64()
            .ok_or("Missing height span")? as f32,
    };

    let fit = serde_json::from_value(capture["settings"].clone()).map_err(|e| e.to_string())?;
    let mut observed = Vec::new();
    let started = std::time::Instant::now();
    macro_rules! run {
        ($refine:path) => {
            $refine(
                road_surface_stage::Context {
                    project: &project,
                    root,
                    worker: path("worker")?,
                    layers: &layers,
                },
                &roads,
                &source,
                road_refinement::Snapshots {
                    retained_height: None,
                    geometry,
                    ground: &ground,
                    source_painted: &painted,
                    source_rounding: None,
                    topology: &topology,
                    surface_resolution: sr,
                    protected_topology: capture["protected_topology"]
                        .as_i64()
                        .ok_or("Missing protection")? as i32,
                    allowed_topology: capture["allowed_topology"]
                        .as_i64()
                        .ok_or("Missing allowed mask")? as i32,
                },
                road_refinement::Settings {
                    fit,
                    clearance: papyrus_terrain::road_clearance::Settings {
                        tolerance: 0.02,
                        minimum_up_normal: 0.5,
                        maximum_work: 10_000_000,
                        maximum_probes: 131072,
                    },
                    maximum_rounds: 32,
                    maximum_total_work: 32_000_000_000,
                    maximum_reprojection_distance: 0.25,
                },
                |round| {
                    eprintln!("{}", serde_json::to_string(round).unwrap());
                    observed.push(round.clone());
                },
                || false,
            )
        };
    }
    let result = if quantized {
        run!(road_refinement::refine_quantized_with_source)
    } else {
        run!(road_refinement::refine_with_source)
    };
    let quantized_selected = quantized
        || project
            .roads
            .as_ref()
            .and_then(|r| r.entrance_launch.as_ref())
            .is_some_and(|r| r.engine == papyrus_terrain::road_network::LaunchEngine::GlobalEarthwork);
    let mut report = json!({"format":"mapper-private-source-refinement-study-1","capture":args[1],"planning":args[2],"elapsed_seconds":started.elapsed().as_secs_f64(),"observed_rounds":observed,"world_accepted":false,"route_changes_validated":false,"quantized_strategy":quantized_selected,"quantized_override_requested":quantized});
    match result {
        Ok(Some(value)) => {
            report["numerical_model_passed"] = json!(value.report.numerical_model_passed);
            report["refinement"] =
                serde_json::to_value(&value.report).map_err(|e| e.to_string())?;
            report["controls"] = json!(value.controls);
            report["height_sha256"] = json!(format!(
                "{:x}",
                Sha256::digest(
                    value
                        .height
                        .iter()
                        .flat_map(|v| v.to_le_bytes())
                        .collect::<Vec<_>>()
                )
            ));
        }
        Ok(None) => report["cancelled"] = json!(true),
        Err(error) => {
            report["numerical_model_passed"] = json!(false);
            report["error"] = json!(format!("{error:?}"));
        }
    }
    write_study(output, &report)?;
    eprintln!(
        "Finished: numerical_model_passed={}",
        report["numerical_model_passed"]
    );
    Ok(())
}
