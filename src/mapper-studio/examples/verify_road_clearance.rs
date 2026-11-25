//! Exercise the shared in-process clearance adapter on immutable native studies.
use mapper_studio::road_surface_stage;
use papyrus_terrain::{
    path_mesh::Template,
    path_terrain::Grid,
    road_clearance,
    road_network::{SurfaceChunk, SurfaceOutput},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Component, Path},
};
fn bounded(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut out)
        .map_err(|e| e.to_string())?;
    if out.len() as u64 > limit {
        return Err("Input exceeds allowance".into());
    }
    Ok(out)
}
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn read(path: &Path) -> Result<Value, String> {
    serde_json::from_slice(&bounded(path, 32 * 1024 * 1024)?).map_err(|e| e.to_string())
}
fn receipt(root: &Path, manifest: &Value, name: &str) -> Result<Vec<u8>, String> {
    let path = Path::new(name);
    if path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        return Err("Invalid artifact name".into());
    }
    let raw = bounded(&root.join(path), 64 * 1024 * 1024)?;
    if manifest["files"][name]["bytes"].as_u64() != Some(raw.len() as u64)
        || manifest["files"][name]["sha256"].as_str() != Some(&sha(&raw))
    {
        return Err(format!("Receipt mismatch: {name}"));
    }
    Ok(raw)
}
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 5 {
        return Err("Expected STAMP_DATA HEIGHT_DATA MESH_DATA MESH_SOURCE NEW_REPORT".into());
    }
    let stamp = Path::new(&args[0]);
    let height = Path::new(&args[1]);
    let meshes = Path::new(&args[2]);
    let output = Path::new(&args[4]);
    if output.exists() {
        return Err("Report must be new".into());
    }
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let output = parent
        .canonicalize()
        .map_err(|e| e.to_string())?
        .join(output.file_name().ok_or("Missing report filename")?);
    for input in [stamp, height, meshes] {
        if output.starts_with(input.canonicalize().map_err(|e| e.to_string())?) {
            return Err("Report must be outside input artifacts".into());
        }
    }
    let layout = read(&stamp.join("manifest.json"))?;
    let manifest = read(&meshes.join("manifest.json"))?;
    let study: Value = serde_json::from_slice(&receipt(meshes, &manifest, "study.json")?)
        .map_err(|e| e.to_string())?;
    let source_raw = bounded(Path::new(&args[3]), 32 * 1024 * 1024)?;
    let source: Value = serde_json::from_slice(&source_raw).map_err(|e| e.to_string())?;
    if study["source_sha256"].as_str() != Some(&sha(&source_raw)) {
        return Err("Source binding differs".into());
    }
    let template: Template =
        serde_json::from_value(source["meshes"][0]["buffer"].clone()).map_err(|e| e.to_string())?;
    let n = layout["height_resolution"]
        .as_u64()
        .ok_or("Missing height dimensions")? as usize;
    if !(2..=4097).contains(&n) {
        return Err("Invalid height dimensions".into());
    }
    let height_raw = bounded(&height.join("height.i16le"), 64 * 1024 * 1024)?;
    if height_raw.len() != n * n * 2
        || study["input_height_sha256"].as_str() != Some(&sha(&height_raw))
        || study["input_correction_study_sha256"].as_str()
            != Some(&sha(&bounded(
                &height.join("study.json"),
                32 * 1024 * 1024,
            )?))
    {
        return Err("Height binding differs".into());
    }
    let values: Vec<_> = height_raw
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    let paint: Value = serde_json::from_slice(&receipt(stamp, &layout, "road-study.json")?)
        .map_err(|e| e.to_string())?;
    let mut chunks = Vec::new();
    for entry in study["surface_report"]["files"]
        .as_array()
        .ok_or("Missing surface files")?
    {
        if entry["lod"] != 0 {
            continue;
        }
        if chunks.len() >= 8192 {
            return Err("Too many chunks".into());
        }
        let chunk: Value = serde_json::from_slice(&receipt(
            meshes,
            &manifest,
            entry["file"].as_str().ok_or("Missing chunk name")?,
        )?)
        .map_err(|e| e.to_string())?;
        for field in ["road", "lod", "chunk_index"] {
            if chunk[field] != entry[field] {
                return Err("Chunk identity differs".into());
            }
        }
        chunks.push(SurfaceChunk {
            road: chunk["road"].as_u64().ok_or("Invalid road")? as usize,
            lod: 0,
            chunk_index: chunk["chunk_index"].as_u64().ok_or("Invalid chunk")? as usize,
            geometry: serde_json::from_value(chunk["geometry"].clone())
                .map_err(|e| e.to_string())?,
        });
    }
    let number = |v: &Value| {
        v.as_f64()
            .map(|v| v as f32)
            .ok_or("Missing height geometry".to_string())
    };
    let surfaces = SurfaceOutput {
        report: Value::Null,
        chunks,
    };
    let started = std::time::Instant::now();
    let audit = road_surface_stage::audit_clearance(
        Grid {
            size: number(&layout["size"])?,
            resolution: n,
            height: &values,
            origin_y: number(&paint["profile"]["height_origin"])?,
            vertical_span: number(&paint["profile"]["vertical_span"])?,
        },
        &surfaces,
        &template,
        road_clearance::Settings {
            tolerance: 0.02,
            minimum_up_normal: 0.5,
            maximum_work: 10_000_000,
            maximum_probes: 131072,
        },
        || false,
    )?
    .ok_or("Audit cancelled")?;
    let result = json!({"format":"mapper-inprocess-road-clearance-verification","schema":1,"audit":audit.report,
  "elapsed_audit_seconds":started.elapsed().as_secs_f64(),"manifest_sha256":sha(&bounded(&meshes.join("manifest.json"),32*1024*1024)?),"height_sha256":sha(&height_raw),"source_sha256":sha(&source_raw),"playable":false});
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&output)
        .map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(&result).map_err(|e| e.to_string())?)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    println!("{}", result["audit"]);
    Ok(())
}
