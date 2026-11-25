//! CONFIG FIT_CAPTURE PLANNING_CAPTURE ROAD NEW_REPORT [DEGREES_CSV] [--all-chunks] [--at-end].
//! Source mesh rebuilds on frozen terrain; no road or world is accepted here.
use mapper_studio::{Project, infrastructure, prefab_assets, road_surface_source, road_surface_stage};
use papyrus_terrain::{
    native_layers::Layers,
    path_mesh, path_terrain,
    road_network::{Destination, Family, Report, Road},
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
    let at_end = if let Some(i) = args.iter().position(|v| v == "--at-end") {
        args.remove(i);
        true
    } else {
        false
    };
    let all_chunks = args.last().is_some_and(|v| v == "--all-chunks");
    if all_chunks {
        args.pop();
    }
    if !(5..=6).contains(&args.len()) {
        return Err("CONFIG FIT_CAPTURE PLANNING_CAPTURE ROAD NEW_REPORT [DEGREES_CSV]".into());
    }
    let angles = if let Some(csv) = args.get(5) {
        csv.split(',')
            .map(|v| v.parse::<f64>().map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        vec![0., -2., -5., -10., -15., -20., 2., 5., 10.]
    };
    if angles.is_empty()
        || angles.len() > 12
        || angles[0] != 0.
        || angles.iter().any(|v| !v.is_finite() || v.abs() > 45.)
    {
        return Err("Expected 1..12 finite angles within 45 degrees, starting with zero".into());
    }
    let output = Path::new(&args[4]);
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
    let index = args[3].parse::<usize>().map_err(|e| e.to_string())?;
    let project = Project::load(path("project")?)?;
    let planned_project: Project =
        serde_json::from_value(planning["project"].clone()).map_err(|e| e.to_string())?;
    if project != planned_project {
        return Err("Project and planning capture differ".into());
    }
    let raw = fs::read(Path::new(&args[1]).join("height.i16le")).map_err(|e| e.to_string())?;
    if capture["files"]["height.i16le"]["sha256"].as_str()
        != Some(format!("{:x}", Sha256::digest(&raw)).as_str())
    {
        return Err("Height receipt differs".into());
    }
    let (hr, sr) = papyrus_terrain::native_layers::dimensions(project.size)?;
    if raw.len() != hr * hr * 2 {
        return Err("Height dimensions differ".into());
    }
    let layers = Layers {
        size: project.size,
        terrain_seed: project.seeds[0].value,
        biome_seed: project.seeds[1].value,
        height_resolution: hr,
        surface_resolution: sr,
        height: raw
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect(),
        biome: vec![],
        splat: vec![],
        topology: vec![],
        normalized_biome_cells: 0,
        normalized_splat_cells: 0,
    };
    let original: Vec<[f32; 3]> =
        serde_json::from_value(capture["controls"][index].clone()).map_err(|e| e.to_string())?;
    if original.len() < 3 || original.first() == original.last() {
        return Err("Expected open road with interior controls".into());
    }
    let metadata = planning["roads"]["roads"]
        .as_array()
        .and_then(|v| v.get(index))
        .ok_or("Road absent from plan")?;
    let families: Vec<Family> =
        serde_json::from_value(planning["roads"]["families"].clone()).map_err(|e| e.to_string())?;
    let mut family = families
        .into_iter()
        .find(|f| index >= f.first_road && index < f.first_road + f.road_count)
        .ok_or("Missing family")?;
    family.first_road = 0;
    family.road_count = 1;
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
    let captured = capture["collisions"]
        .as_array()
        .ok_or("Missing collisions")?
        .iter()
        .find(|c| c["road"].as_u64() == Some(index as u64) && c["chunk_index"] == 0)
        .ok_or("Missing first chunk")?;
    let baseline: path_mesh::Chunk =
        serde_json::from_value(captured["geometry"].clone()).map_err(|e| e.to_string())?;
    let control_index = if at_end { original.len() - 2 } else { 1 };
    let pivot = if at_end {
        *original.last().unwrap()
    } else {
        original[0]
    };
    let mut variants = Vec::new();
    for degrees in angles {
        eprintln!("Rebuilding departure {degrees} degrees");
        let mut points = original.clone();
        if degrees != 0. {
            let (s, c) = degrees.to_radians().sin_cos();
            let dx = f64::from(original[control_index][0]) - f64::from(pivot[0]);
            let dz = f64::from(original[control_index][2]) - f64::from(pivot[2]);
            points[control_index][0] = (f64::from(pivot[0]) + c * dx - s * dz) as f32;
            points[control_index][2] = (f64::from(pivot[2]) + s * dx + c * dz) as f32;
            points[control_index][1] = path_terrain::sample_height(
                path_terrain::Grid {
                    size: project.size as f32,
                    resolution: hr,
                    origin_y: geometry.origin_y,
                    vertical_span: geometry.vertical_span,
                    height: &layers.height,
                },
                points[control_index],
            )?;
        }
        let report = Report {
            roads: vec![Road {
                from: destination(&metadata["from"])?,
                to: destination(&metadata["to"])?,
                points,
            }],
            families: vec![family.clone()],
            ..Report::default()
        };
        let built = road_surface_stage::generate_traced_on_height(
            road_surface_stage::Context {
                project: &project,
                root,
                worker: path("worker")?,
                layers: &layers,
            },
            &report,
            &source,
            geometry,
            &layers.height,
            || false,
        )?
        .ok_or("Cancelled")?;
        let chunk = built
            .surfaces
            .chunks
            .iter()
            .find(|c| c.lod == 0 && c.chunk_index == 0)
            .ok_or("Missing rebuilt first chunk")?;
        let trace = built
            .traces
            .iter()
            .find(|t| t.chunk_index == 0)
            .ok_or("Missing first trace")?;
        if degrees == 0.
            && (chunk.geometry.origin != baseline.origin
                || chunk.geometry.mesh.vertices != baseline.mesh.vertices
                || chunk.geometry.mesh.triangles != baseline.mesh.triangles)
        {
            return Err("Zero-angle source rebuild differs from capture".into());
        }
        if report.roads[0].points[0] != original[0]
            || report.roads[0].points.last() != original.last()
        {
            return Err("Authored endpoint moved".into());
        }
        let mut additional = Vec::new();
        let mut clearance = Value::Null;
        let mut probes = Value::Null;
        if all_chunks {
            for item in built
                .surfaces
                .chunks
                .iter()
                .filter(|c| c.lod == 0 && c.chunk_index != 0)
            {
                let trace = built
                    .traces
                    .iter()
                    .find(|t| t.chunk_index == item.chunk_index)
                    .ok_or("Missing additional trace")?;
                if degrees == 0. {
                    let value = capture["collisions"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|c| {
                            c["road"].as_u64() == Some(index as u64)
                                && c["chunk_index"].as_u64() == Some(item.chunk_index as u64)
                        })
                        .ok_or("Missing baseline additional chunk")?;
                    let previous: path_mesh::Chunk =
                        serde_json::from_value(value["geometry"].clone())
                            .map_err(|e| e.to_string())?;
                    if item.geometry.origin != previous.origin
                        || item.geometry.mesh.vertices != previous.mesh.vertices
                        || item.geometry.mesh.triangles != previous.mesh.triangles
                    {
                        return Err("Additional baseline chunk differs".into());
                    }
                }
                additional.push(json!({"chunk_index":item.chunk_index,"geometry":item.geometry,"heights":trace.heights}));
            }
            let checked = road_surface_stage::audit_clearance(
                path_terrain::Grid {
                    size: project.size as f32,
                    resolution: hr,
                    origin_y: geometry.origin_y,
                    vertical_span: geometry.vertical_span,
                    height: &layers.height,
                },
                &built.surfaces,
                &source.meshes[0].buffer,
                papyrus_terrain::road_clearance::Settings {
                    tolerance: 0.02,
                    minimum_up_normal: 0.5,
                    maximum_work: 10_000_000,
                    maximum_probes: 131072,
                },
                || false,
            )?
            .ok_or("Cancelled clearance audit")?;
            clearance = checked.report;
            probes = serde_json::to_value(checked.probes).map_err(|e| e.to_string())?;
        }
        variants.push(json!({"degrees":degrees,"control_one":report.roads[0].points[control_index],"geometry":chunk.geometry,"heights":trace.heights,
            "additional_chunks":additional,"clearance":clearance,"probes":probes}));
    }
    write_study(
        output,
        &json!({"format":"mapper-source-road-departure-study","schema":1,"road":index,
        "height_sha256":capture["files"]["height.i16le"]["sha256"],"mesh_source_sha256":source.sha256,
        "socket":pivot,"control_index":control_index,"at_end":at_end,"original_control_one":original[control_index],"template":source.meshes[0].buffer,"variants":variants,
        "baseline_geometry_identical":true,"scope":"Actual installation-bound mesh regeneration on frozen terrain; selected endpoint’s adjacent interior control only. No source paint, routing, placement or accepted-world certificate.","game_runtime_validated":false}),
    )?;
    println!("{}", json!({"report":output}));
    Ok(())
}
