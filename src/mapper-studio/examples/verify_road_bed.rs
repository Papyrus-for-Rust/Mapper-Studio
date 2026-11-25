//! Fit a private captured road bed; never publishes a playable world.
use mapper_studio::Project;
use papyrus_terrain::{
    path_mesh::{Chunk, HeightBinding, Template},
    path_terrain::Grid,
    road_bed::{self, Collision, Input, Settings},
    road_grade::Junction,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Component, Path},
};
fn bounded(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("Input exceeds study allowance".into());
    }
    Ok(bytes)
}
fn read(path: &Path) -> Result<Value, String> {
    serde_json::from_slice(&bounded(path, 32 * 1024 * 1024)?).map_err(|e| e.to_string())
}
fn digest(raw: &[u8]) -> String {
    format!("{:x}", Sha256::digest(raw))
}
fn receipt(root: &Path, manifest: &Value, name: &str) -> Result<Vec<u8>, String> {
    let path = Path::new(name);
    if path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        return Err("Invalid artifact filename".into());
    }
    let raw = bounded(&root.join(path), 64 * 1024 * 1024)?;
    if manifest["files"][name]["bytes"].as_u64() != Some(raw.len() as u64)
        || manifest["files"][name]["sha256"].as_str() != Some(&digest(&raw))
    {
        return Err(format!("Artifact receipt differs: {name}"));
    }
    Ok(raw)
}
fn short(raw: Vec<u8>, n: usize) -> Result<Vec<i16>, String> {
    if raw.len() != n * n * 2 {
        return Err("Invalid encoded height grid".into());
    }
    Ok(raw
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect())
}
fn number(value: &Value) -> Result<f32, String> {
    value
        .as_f64()
        .map(|v| v as f32)
        .ok_or("Missing numeric policy".into())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Captured {
    road: usize,
    lod: usize,
    chunk_index: usize,
    geometry: Chunk,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Trace {
    road: usize,
    chunk_index: usize,
    heights: Vec<HeightBinding>,
}
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if ![7, 9, 10, 11, 12, 13].contains(&args.len()) {
        return Err("Expected PROJECT STAMP_DATA CORRECTED_DATA TRACED_MESH SOURCE_MESH PLANNED_ROADS NEW_DIRECTORY [ITERATIONS QUANTIZATION_RESERVE [MAXIMUM_WORK [CLEARANCE_AUDIT [--resample-controls [--source-paint-envelope=MASK]]]]]".into());
    }
    let resample_controls = args.get(11).is_some();
    if args.get(11).is_some_and(|v| v != "--resample-controls") {
        return Err("Unknown control policy".into());
    }
    let envelope_mask = args
        .get(12)
        .map(|arg| {
            arg.to_string_lossy()
                .strip_prefix("--source-paint-envelope=")
                .ok_or("Unknown envelope policy".to_string())?
                .parse::<i32>()
                .map_err(|e| e.to_string())
        })
        .transpose()?;
    let project = Project::load(Path::new(&args[0]))?;
    let recipe = project.roads.as_ref().ok_or("Missing saved road policy")?;
    let stamp = Path::new(&args[1]);
    let corrected = Path::new(&args[2]);
    let traced = Path::new(&args[3]);
    let output = Path::new(&args[6]);
    if output.exists() {
        return Err("Output must be new".into());
    }
    let stamp_manifest = read(&stamp.join("manifest.json"))?;
    let mesh_manifest = read(&traced.join("manifest.json"))?;
    let files = mesh_manifest["files"]
        .as_object()
        .ok_or("Missing mesh receipts")?;
    if files.len() > 16385
        || files
            .values()
            .map(|r| r["bytes"].as_u64().unwrap_or(u64::MAX))
            .try_fold(0_u64, |a, b| a.checked_add(b))
            .is_none_or(|n| n > 512 * 1024 * 1024)
    {
        return Err("Excessive mesh artifacts".into());
    }
    for name in files.keys() {
        receipt(traced, &mesh_manifest, name)?;
    }
    let study: Value = serde_json::from_slice(&receipt(traced, &mesh_manifest, "study.json")?)
        .map_err(|e| e.to_string())?;
    let source_raw = bounded(Path::new(&args[4]), 32 * 1024 * 1024)?;
    let source: Value = serde_json::from_slice(&source_raw).map_err(|e| e.to_string())?;
    if study["source_sha256"].as_str() != Some(&digest(&source_raw))
        || study["height_traces"]["engine"] != "papyrus-road-height-bindings-1"
    {
        return Err("Missing bound source or height traces".into());
    }
    let planned_raw = bounded(Path::new(&args[5]), 32 * 1024 * 1024)?;
    let planned: Value = serde_json::from_slice(&planned_raw).map_err(|e| e.to_string())?;
    if study["input_planned_roads_sha256"].as_str() != Some(&digest(&planned_raw))
        || planned["entrance_launch"]
            != serde_json::to_value(recipe.entrance_launch).map_err(|e| e.to_string())?
    {
        return Err("Planned-road recipe binding differs".into());
    }
    let correction_raw = bounded(&corrected.join("study.json"), 32 * 1024 * 1024)?;
    let correction: Value = serde_json::from_slice(&correction_raw).map_err(|e| e.to_string())?;
    if study["input_correction_study_sha256"].as_str() != Some(&digest(&correction_raw))
        || correction["applied"] != true
        || correction["playable"] != false
    {
        return Err("Corrected study binding differs".into());
    }
    let n = stamp_manifest["height_resolution"]
        .as_u64()
        .ok_or("Missing grid resolution")? as usize;
    let sn = stamp_manifest["surface_resolution"]
        .as_u64()
        .ok_or("Missing topology resolution")? as usize;
    if !(2..=4097).contains(&n)
        || !(2..=2048).contains(&sn)
        || stamp_manifest["size"].as_u64() != Some(project.size as u64)
    {
        return Err("Grid layout differs".into());
    }
    let height_raw = bounded(&corrected.join("height.i16le"), 64 * 1024 * 1024)?;
    if study["input_height_sha256"].as_str() != Some(&digest(&height_raw)) {
        return Err("Height receipt differs".into());
    }
    let height = short(height_raw, n)?;
    let ground = short(receipt(stamp, &stamp_manifest, "after-height.i16le")?, n)?;
    let source_painted = if envelope_mask.is_some() {
        Some(short(
            receipt(stamp, &stamp_manifest, "road-study-height.i16le")?,
            n,
        )?)
    } else {
        None
    };
    let topology_raw = receipt(stamp, &stamp_manifest, "road-study-topology.i32le")?;
    if topology_raw.len() != sn * sn * 4 {
        return Err("Invalid topology grid".into());
    }
    let topology: Vec<_> = topology_raw
        .chunks_exact(4)
        .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let paint: Value = serde_json::from_slice(&receipt(stamp, &stamp_manifest, "road-study.json")?)
        .map_err(|e| e.to_string())?;
    let controls: Vec<Vec<[f32; 3]>> = correction["roads"]
        .as_array()
        .ok_or("Missing current controls")?
        .iter()
        .map(|r| serde_json::from_value(r["points"].clone()).map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    let roads = planned["roads"].as_array().ok_or("Missing planned roads")?;
    if controls.len() != roads.len() {
        return Err("Control/road coverage differs".into());
    }
    let mut junctions = Vec::new();
    for (r, road) in roads.iter().enumerate() {
        for side in ["from", "to"] {
            let dest = &road[side];
            if let (Some(parent), Some(point)) = (
                dest["network_road"].as_u64(),
                dest["network_point"].as_u64(),
            ) {
                let endpoint = if side == "from" {
                    0
                } else {
                    controls[r].len().checked_sub(1).ok_or("Empty road")?
                };
                junctions.push(Junction {
                    a: (r, endpoint),
                    b: (parent as usize, point as usize),
                });
            }
        }
    }
    let mut captured = Vec::<Captured>::new();
    let mut traces = Vec::<Trace>::new();
    for entry in study["height_traces"]["files"]
        .as_array()
        .ok_or("Missing trace entries")?
    {
        let road = entry["road"].as_u64().ok_or("Invalid traced road")? as usize;
        let index = entry["chunk_index"]
            .as_u64()
            .ok_or("Invalid traced chunk")? as usize;
        let file = study["surface_report"]["files"]
            .as_array()
            .ok_or("Missing collision files")?
            .iter()
            .find(|f| {
                f["road"].as_u64() == Some(road as u64)
                    && f["lod"] == 0
                    && f["chunk_index"].as_u64() == Some(index as u64)
            })
            .ok_or("Trace has no collision chunk")?;
        let chunk: Captured = serde_json::from_slice(&receipt(
            traced,
            &mesh_manifest,
            file["file"].as_str().ok_or("Invalid chunk filename")?,
        )?)
        .map_err(|e| e.to_string())?;
        let trace: Trace = serde_json::from_slice(&receipt(
            traced,
            &mesh_manifest,
            entry["file"].as_str().ok_or("Invalid trace filename")?,
        )?)
        .map_err(|e| e.to_string())?;
        if chunk.road != road
            || chunk.chunk_index != index
            || chunk.lod != 0
            || trace.road != road
            || trace.chunk_index != index
        {
            return Err("Trace identity differs".into());
        }
        captured.push(chunk);
        traces.push(trace);
    }
    let audits = study["surface_report"]["collision_grade_audits"]
        .as_array()
        .ok_or("Missing collision audits")?;
    if captured.len() != audits.len() || audits.is_empty() {
        return Err("Collision coverage differs".into());
    }
    let minimum = number(&audits[0]["audit"]["minimum_up_normal"])?;
    if audits.iter().any(|a| {
        number(&a["audit"]["maximum_grade"]).ok() != Some(recipe.maximum_grade)
            || number(&a["audit"]["minimum_up_normal"]).ok() != Some(minimum)
    }) {
        return Err("Saved mesh-grade policies differ".into());
    }
    let template: Template =
        serde_json::from_value(source["meshes"][0]["buffer"].clone()).map_err(|e| e.to_string())?;
    let collisions: Vec<_> = captured
        .iter()
        .zip(&traces)
        .map(|(c, t)| Collision {
            road: c.road,
            chunk_index: c.chunk_index,
            geometry: &c.geometry,
            template: &template,
            heights: &t.heights,
        })
        .collect();
    let quantization_reserve = args.get(8).map_or(Ok(0.51), |v| {
        v.to_string_lossy()
            .parse::<f64>()
            .map_err(|e| e.to_string())
    })?;
    let settings = Settings {
        maximum_grade: recipe.maximum_grade,
        maximum_cut_fill: recipe.maximum_cut_fill,
        minimum_up_normal: minimum,
        maximum_iterations: args.get(7).map_or(Ok(256), |v| {
            v.to_string_lossy()
                .parse::<usize>()
                .map_err(|e| e.to_string())
        })?,
        maximum_work: args.get(9).map_or(Ok(512_000_000), |v| {
            v.to_string_lossy()
                .parse::<usize>()
                .map_err(|e| e.to_string())
        })?,
        quantization_reserve,
        reserve_roundoff: quantization_reserve > 0.,
    };
    let protected = correction["protected_topology"]
        .as_i64()
        .ok_or("Missing protected topology policy")? as i32;
    let mut clearance_audit_sha256 = None;
    let probes: Vec<road_bed::ClearanceProbe> = if let Some(path) = args.get(10) {
        let raw = bounded(Path::new(path), 32 * 1024 * 1024)?;
        let audit: Value = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
        if audit["format"] != "mapper-bilinear-road-clearance-audit"
            || audit["schema"] != 2
            || audit["manifest_sha256"].as_str()
                != Some(&digest(&bounded(
                    &traced.join("manifest.json"),
                    32 * 1024 * 1024,
                )?))
            || audit["height_sha256"] != study["input_height_sha256"]
            || audit["source_sha256"] != study["source_sha256"]
            || audit["stamp_manifest_sha256"].as_str()
                != Some(&digest(&bounded(
                    &stamp.join("manifest.json"),
                    32 * 1024 * 1024,
                )?))
        {
            return Err("Clearance witness receipts differ".into());
        }
        let entries = audit["clearance_probes"]
            .as_array()
            .ok_or("Missing clearance witnesses")?;
        let retained = audit
            .get("retained_clearance_probes")
            .map(|v| v.as_u64().ok_or("Invalid retained witness count"))
            .transpose()?
            .unwrap_or(0);
        if (entries.is_empty() && envelope_mask.is_none())
            || entries.len() > 131072
            || audit["continuous_intersections"]
                .as_u64()
                .and_then(|n| n.checked_add(retained))
                != Some(entries.len() as u64)
        {
            return Err("Clearance witness coverage differs".into());
        }
        clearance_audit_sha256 = Some(digest(&raw));
        serde_json::from_value(audit["clearance_probes"].clone()).map_err(|e| e.to_string())?
    } else {
        Vec::new()
    };
    let started = std::time::Instant::now();
    let mut last_progress = None;
    let input = Input {
        grid: Grid {
            size: project.size as f32,
            resolution: n,
            origin_y: number(&paint["profile"]["height_origin"])?,
            vertical_span: number(&paint["profile"]["vertical_span"])?,
            height: &height,
        },
        ground: &ground,
        topology: &topology,
        surface_resolution: sn,
        protected_topology: protected,
        controls: &controls,
        junctions: &junctions,
        collisions: &collisions,
    };
    let observe = |p: road_bed::Progress| {
        eprintln!(
            "Bed fit {} iteration {}: {} grade and {} clearance violations, linear excess {:.8}, work {}",
            p.phase,
            p.iteration,
            p.last_exact_violations,
            p.last_exact_clearance_violations,
            p.linear_maximum_excess,
            p.work
        );
        last_progress = Some(p);
    };
    let result = if let Some(allowed_topology) = envelope_mask {
        road_bed::fit_in_paint_envelope(
            input,
            settings,
            &probes,
            road_bed::PaintEnvelope {
                source_rounding: None,
                source_painted: source_painted
                    .as_deref()
                    .ok_or("Missing source paint grid")?,
                allowed_topology,
            },
            observe,
            || false,
        )
    } else if resample_controls {
        road_bed::fit_resampling_controls(input, settings, &probes, observe, || false)
    } else {
        road_bed::fit_with_probes(input, settings, &probes, observe, || false)
    };
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temporary = tempfile::Builder::new()
        .prefix(".road-bed-fit-")
        .tempdir_in(parent)
        .map_err(|e| e.to_string())?;
    let mut report = json!({"format":"mapper-road-bed-fit-study","schema":1,"applied":false,"playable":false,"settings":settings,
        "input_height_sha256":study["input_height_sha256"],"input_correction_study_sha256":study["input_correction_study_sha256"],
        "input_mesh_manifest_sha256":digest(&bounded(&traced.join("manifest.json"),32*1024*1024)?),
        "input_planned_roads_sha256":study["input_planned_roads_sha256"],"elapsed_seconds":started.elapsed().as_secs_f64()});
    report["last_progress"] = json!(last_progress);
    report["input_clearance_audit_sha256"] = json!(clearance_audit_sha256);
    report["protected_paint_envelope"] = if let Some(mask) = envelope_mask {
        json!({"format":"papyrus-protected-road-paint-envelope-1","allowed_topology":mask,"source_painted_sha256":stamp_manifest["files"]["road-study-height.i16le"]["sha256"]})
    } else {
        Value::Null
    };
    match result {
        Ok(output) => {
            let raw: Vec<_> = output.height.iter().flat_map(|v| v.to_le_bytes()).collect();
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(temporary.path().join("height.i16le"))
                .map_err(|e| e.to_string())?;
            file.write_all(&raw)
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
            report["applied"] = true.into();
            report["height_sha256"] = digest(&raw).into();
            report["report"] = json!(output.report);
            report["roads"] = if resample_controls {
                let paths = output
                    .controls
                    .iter()
                    .map(|points| {
                        papyrus_terrain::path_geometry::Path::from_controls(points, 32_000_000, || {
                            false
                        })
                        .map_err(|e| format!("Rebuilt path failed: {e:?}"))?
                        .ok_or("Rebuilt path cancelled".into())
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                serde_json::to_value(paths).map_err(|e| e.to_string())?
            } else {
                correction["roads"].clone()
            };
            report["protected_topology"] = json!(protected);
        }
        Err(failure) => report["failure"] = json!(failure),
    }
    let bytes = serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?;
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(temporary.path().join("study.json"))
        .map_err(|e| e.to_string())?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    drop(file);
    fs::rename(temporary.path(), output).map_err(|e| e.to_string())?;
    println!(
        "Road-bed proposal applied: {}; failure: {}",
        report["applied"], report["failure"]
    );
    Ok(())
}
