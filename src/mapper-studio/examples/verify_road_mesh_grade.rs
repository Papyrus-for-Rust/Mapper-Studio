//! Bound source road templates over corrected terrain; no desktop/game startup.
use mapper_studio::{
    Project, infrastructure, prefab_assets, road_paint_source, road_surface_source,
    road_surface_stage, roadside_source,
};
use papyrus_terrain::{
    native_layers::Layers,
    path_terrain, road_mesh_audit,
    road_network::{Destination, Family, Report, Road},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::Path,
};
fn bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, String> {
    let mut raw = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(maximum + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() as u64 > maximum {
        return Err("Input exceeds diagnostic budget".into());
    }
    Ok(raw)
}
fn read(path: &Path) -> Result<Value, String> {
    serde_json::from_slice(&bounded(path, 32 * 1024 * 1024)?).map_err(|e| e.to_string())
}
fn destination(v: &Value) -> Result<Destination, String> {
    if v["loop"] == true {
        return Ok(Destination::Loop);
    }
    if let (Some(m), Some(c)) = (v["monument"].as_u64(), v["connector"].as_i64()) {
        return Ok(Destination::Monument {
            monument: m,
            connector: c,
        });
    }
    if let (Some(r), Some(p)) = (v["network_road"].as_u64(), v["network_point"].as_u64()) {
        return Ok(Destination::Network {
            road: r as usize,
            point: p as usize,
        });
    }
    Err("Invalid road destination".into())
}
fn write(directory: &Path, name: &str, value: &impl serde::Serialize) -> Result<Value, String> {
    let raw = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(directory.join(name))
        .map_err(|e| e.to_string())?;
    file.write_all(&raw).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    Ok(json!({"bytes":raw.len(),"sha256":format!("{:x}",Sha256::digest(&raw))}))
}
fn main() -> Result<(), String> {
    let mut args: Vec<_> = std::env::args_os().skip(1).collect();
    let trace_heights = args.last().is_some_and(|v| v == "--trace-heights");
    if trace_heights {
        args.pop();
    }
    if !(11..=12).contains(&args.len()) {
        return Err("Expected PROJECT RUST_ROOT CATALOG INFRASTRUCTURE MESH_SOURCE ROADSIDE_SOURCE STAMP_DATA CORRECTED_DATA REFERENCE_BUNDLE NEW_DIRECTORY WORKER".into());
    }
    let project = Project::load(Path::new(&args[0]))?;
    let root = Path::new(&args[1]);
    let stamp = Path::new(&args[6]);
    let corrected = Path::new(&args[7]);
    let reference = Path::new(&args[8]);
    let output = Path::new(&args[9]);
    if output.exists() {
        return Err("Output must be new".into());
    }
    let catalog = prefab_assets::load(
        root,
        Path::new(&args[2]),
        project
            .installation
            .as_ref()
            .ok_or("Missing installation")?,
        || false,
    )?;
    let parent = infrastructure::Source::load(Path::new(&args[3]), &catalog, || false)?;
    parent.verify_installation(root, || false)?;
    let source = road_surface_source::Source::read(Path::new(&args[4]), &parent, &|| false)?;
    source.verify_installation(root, || false)?;
    let roadside = roadside_source::Source::load(Path::new(&args[5]), root, &catalog, || false)?;
    let paint =
        road_paint_source::Profile::for_source(&roadside)?.ok_or("Unbound road height geometry")?;
    let captured = read(&stamp.join("road-study.json"))?;
    if captured["profile_sha256"].as_str() != Some(&paint.sha256()?) {
        return Err("Captured road height geometry differs from the installation".into());
    }
    let ref_manifest = read(&reference.join("manifest.json"))?;
    let ref_read = |name: &str| -> Result<Vec<u8>, String> {
        let raw = bounded(&reference.join(name), 32 * 1024 * 1024)?;
        if ref_manifest["files"][name]["bytes"].as_u64() != Some(raw.len() as u64)
            || ref_manifest["files"][name]["sha256"].as_str()
                != Some(format!("{:x}", Sha256::digest(&raw)).as_str())
        {
            return Err("Reference receipt mismatch".into());
        }
        Ok(raw)
    };
    let ref_project: Project =
        serde_json::from_slice(&ref_read("project.mapper")?).map_err(|e| e.to_string())?;
    let launch = project.roads.as_ref().and_then(|r| r.entrance_launch);
    if launch.is_some() != (args.len() == 12) {
        return Err("Checked launch mesh study requires its explicit planned-road report".into());
    }
    let mut terrain_project = project.clone();
    if launch.is_some() {
        terrain_project
            .roads
            .as_mut()
            .ok_or("Missing road recipe")?
            .entrance_launch = None;
    }
    if terrain_project != ref_project {
        return Err("Reference project mismatch".into());
    }
    let mut ref_roads: Value =
        serde_json::from_slice(&ref_read("roads.json")?).map_err(|e| e.to_string())?;
    let result = read(&corrected.join("study.json"))?;
    if result["applied"] != true || result["playable"] != false {
        return Err("Expected a private corrected-terrain study".into());
    }
    let paths = result["roads"]
        .as_array()
        .ok_or("Missing corrected paths")?;
    let ref_paths = ref_roads["roads"]
        .as_array()
        .ok_or("Missing reference paths")?;
    if paths.len() != ref_paths.len() || paths.len() > 256 {
        return Err("Road count mismatch".into());
    }
    let roads = Report {
        families: serde_json::from_value::<Vec<Family>>(ref_roads["families"].clone())
            .map_err(|e| e.to_string())?,
        roads: paths
            .iter()
            .zip(ref_paths)
            .map(|(p, r)| {
                Ok(Road {
                    from: destination(&r["from"])?,
                    to: destination(&r["to"])?,
                    points: serde_json::from_value(p["points"].clone())
                        .map_err(|e| e.to_string())?,
                })
            })
            .collect::<Result<_, String>>()?,
        ..Report::default()
    };
    let (hr, sr) = papyrus_terrain::native_layers::dimensions(project.size)?;
    let raw = bounded(&corrected.join("height.i16le"), hr as u64 * hr as u64 * 2)?;
    if raw.len() != hr * hr * 2 {
        return Err("Invalid corrected height layer".into());
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
    let grid = path_terrain::Grid {
        size: project.size as f32,
        resolution: hr,
        origin_y: paint.height_origin,
        vertical_span: paint.vertical_span,
        height: &layers.height,
    };
    for road in &roads.roads {
        for &p in &road.points {
            if path_terrain::sample_height(grid, p)?.to_bits() != p[1].to_bits() {
                return Err("Corrected control does not match terrain".into());
            }
        }
    }
    let context = road_surface_stage::Context {
        project: &project,
        root,
        worker: Path::new(&args[10]),
        layers: &layers,
    };
    let geometry = road_surface_stage::HeightGeometry {
        origin_y: paint.height_origin,
        vertical_span: paint.vertical_span,
    };
    let (surfaces, traces) = if trace_heights {
        let output = road_surface_stage::generate_traced_with_height_geometry(
            context,
            &roads,
            &source,
            geometry,
            || false,
        )?
        .ok_or("Traced surface generation cancelled")?;
        (output.surfaces, output.traces)
    } else {
        (
            road_surface_stage::generate_with_height_geometry(
                context,
                &roads,
                &source,
                geometry,
                || false,
            )?
            .ok_or("Surface generation cancelled")?,
            Vec::new(),
        )
    };
    let recipe = project.roads.as_ref().ok_or("Missing road recipe")?;
    let mut audits = Vec::new();
    let mut files = BTreeMap::new();
    let parent_dir = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temporary = tempfile::Builder::new()
        .prefix(".road-mesh-grade-")
        .tempdir_in(parent_dir)
        .map_err(|e| e.to_string())?;
    let mut planned_receipt = Value::Null;
    if let Some(policy) = launch {
        let raw = bounded(Path::new(&args[11]), 32 * 1024 * 1024)?;
        ref_roads = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
        if ref_roads["entrance_launch"]
            != serde_json::to_value(policy).map_err(|e| e.to_string())?
        {
            return Err("Planned-road launch policy differs from project".into());
        }
        planned_receipt = json!(format!("{:x}", Sha256::digest(&raw)));
    }
    for chunk in &surfaces.chunks {
        let receipt = write(temporary.path(), &chunk.filename(), chunk)?;
        files.insert(chunk.filename(), receipt);
        if chunk.lod == 0 {
            let audit = road_mesh_audit::audit(
                &chunk.geometry,
                &source.meshes[0].buffer,
                road_mesh_audit::Settings {
                    maximum_grade: recipe.maximum_grade,
                    minimum_up_normal: 0.5,
                    maximum_work: 32_000_000,
                },
                || false,
            )
            .map_err(|e| format!("Road collision audit failed: {e:?}"))?;
            audits.push(json!({"road":chunk.road,"chunk_index":chunk.chunk_index,"audit":audit}));
        }
    }
    if surfaces.report["collision_grade_audits"] != json!(audits) {
        return Err("Shared surface-stage audit differs from explicit chunk audit".into());
    }
    let mut report = json!({"format":"mapper-source-road-mesh-grade-study","schema":1,"playable":false,"normal_generator_activated":false,
        "source_sha256":source.sha256,"paint_profile_sha256":paint.sha256()?,"input_height_sha256":format!("{:x}",Sha256::digest(&raw)),
        "input_planned_roads_sha256":planned_receipt,
        "input_correction_study_sha256":format!("{:x}",Sha256::digest(bounded(&corrected.join("study.json"),32*1024*1024)?)),
        "surface_report":surfaces.report,"collision_audits":audits,"runtime_access_validated":false});
    if trace_heights {
        let mut entries = Vec::new();
        let mut vertices = 0;
        for trace in &traces {
            let chunk = surfaces
                .chunks
                .iter()
                .find(|c| c.road == trace.road && c.lod == 0 && c.chunk_index == trace.chunk_index)
                .ok_or("Height trace has no collision chunk")?;
            if trace.heights.len() != chunk.geometry.mesh.vertices.len() {
                return Err("Height trace count differs".into());
            }
            for (binding, p) in trace.heights.iter().zip(&chunk.geometry.mesh.vertices) {
                let height = binding
                    .resolve(|x, z| path_terrain::sample_height(grid, [x, 0., z]).ok())
                    .ok_or("Height binding cannot resolve")?;
                if (height - chunk.geometry.origin[1]).to_bits() != p[1].to_bits() {
                    return Err("Height binding differs from deformed vertex".into());
                }
                vertices += 1;
            }
            let filename = format!(
                "road-height-trace-{}-chunk-{}.json",
                trace.road, trace.chunk_index
            );
            files.insert(filename.clone(), write(temporary.path(), &filename, trace)?);
            entries.push(json!({"file":filename,"road":trace.road,"chunk_index":trace.chunk_index,"vertices":trace.heights.len()}));
        }
        if traces.len() != audits.len()
            || surfaces.report["height_trace_vertices"] != json!(vertices)
        {
            return Err("Height traces do not cover collision geometry".into());
        }
        report["height_traces"] = json!({"engine":"papyrus-road-height-bindings-1","vertices_bitwise_checked":vertices,"files":entries});
    }
    files.insert(
        "study.json".into(),
        write(temporary.path(), "study.json", &report)?,
    );
    let manifest = json!({"format":"mapper-source-road-mesh-grade-artifacts","schema":1,"playable":false,"files":files});
    write(temporary.path(), "manifest.json", &manifest)?;
    fs::rename(temporary.path(), output).map_err(|e| e.to_string())?;
    println!(
        "Generated {} road chunks and {} collision audits",
        surfaces.chunks.len(),
        audits.len()
    );
    Ok(())
}
