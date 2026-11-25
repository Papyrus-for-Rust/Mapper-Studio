//! CAPTURE_DIRECTORY PARENT_ROAD CHILD_ROAD CHILD_ENDPOINT HALF_EXTENT NEW_REPORT.
//! Reads frozen local captures only. Does not run generation or accept a world.
use papyrus_terrain::{path_mesh, road_network, road_surface_contact};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
};
fn read(path: &Path) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err("Capture exceeds 64 MiB".into());
    }
    Ok(bytes)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Chunk {
    road: usize,
    lod: usize,
    chunk_index: usize,
    geometry: path_mesh::Chunk,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Capture {
    format: String,
    accepted: bool,
    template: path_mesh::Template,
    chunks: Vec<Chunk>,
}
fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 6 {
        return Err("Expected CAPTURE PARENT CHILD ENDPOINT HALF_EXTENT NEW_REPORT".into());
    }
    let number = |i: usize| {
        args[i]
            .to_str()
            .ok_or("Invalid number")?
            .parse::<usize>()
            .map_err(|e| e.to_string())
    };
    let parent = number(1)?;
    let child = number(2)?;
    let side = number(3)?;
    let half = args[4]
        .to_str()
        .ok_or("Invalid extent")?
        .parse::<f64>()
        .map_err(|e| e.to_string())?;
    if side > 1 || !half.is_finite() || !(0.1..=128.).contains(&half) {
        return Err("Invalid contact window".into());
    }
    let destination = Path::new(&args[5]);
    if destination.exists() {
        return Err("Report must be new".into());
    }
    let root = Path::new(&args[0]);
    let report_bytes = read(&root.join("report.json"))?;
    let report: Value = serde_json::from_slice(&report_bytes).map_err(|e| e.to_string())?;
    if report["format"] != "mapper-private-source-road-stages-1" || report["accepted"] != false {
        return Err("Expected an unaccepted source-stage capture".into());
    }
    let roads = report["roads"]["roads"]
        .as_array()
        .filter(|v| v.len() <= 256)
        .ok_or("Missing bounded roads")?;
    if parent >= roads.len() || child >= roads.len() {
        return Err("Road index outside capture".into());
    }
    let controls = roads[child]["points"]
        .as_array()
        .filter(|v| v.len() >= 2 && v.len() <= 131072)
        .ok_or("Missing controls")?;
    let center: [f32; 3] =
        serde_json::from_value(controls[if side == 0 { 0 } else { controls.len() - 1 }].clone())
            .map_err(|e| e.to_string())?;
    let geometry_bytes = read(&root.join("collision-geometry.json"))?;
    let capture: Capture = serde_json::from_slice(&geometry_bytes).map_err(|e| e.to_string())?;
    if capture.format != "mapper-private-source-road-collision-1"
        || capture.accepted
        || capture.chunks.len() > 65536
    {
        return Err("Invalid collision capture".into());
    }
    let chunks = capture
        .chunks
        .into_iter()
        .map(|c| road_network::SurfaceChunk {
            road: c.road,
            lod: c.lod,
            chunk_index: c.chunk_index,
            geometry: c.geometry,
        })
        .collect::<Vec<_>>();
    let contact = road_surface_contact::analyze(
        road_surface_contact::Input {
            template: &capture.template,
            chunks: &chunks,
            roads: [parent, child],
            bounds: [
                f64::from(center[0]) - half,
                f64::from(center[2]) - half,
                f64::from(center[0]) + half,
                f64::from(center[2]) + half,
            ],
            maximum_work: 16_000_000,
        },
        || false,
    )
    .map_err(|e| format!("{e:?}"))?
    .ok_or("Cancelled")?;
    let replay = report["endpoint_replay"]["rows"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|r| r["road"] == child && r["endpoint"] == side)
        })
        .cloned();
    let output = json!({"format":"mapper-local-road-contact-report-1","accepted":false,
        "capture_report_sha256":format!("{:x}",Sha256::digest(&report_bytes)),
        "collision_geometry_sha256":format!("{:x}",Sha256::digest(&geometry_bytes)),
        "ground_height_sha256":report["ground_height_sha256"],"source_height_sha256":report["source_height_sha256"],
        "capture_scope":report["scope"],"endpoint_replay":replay,"child_endpoint":side,"center":center,
        "endpoint_replay_mode":report["endpoint_replay"]["mode"],
        "endpoint_replay_scope":report["endpoint_replay"]["scope"],
        "declared_destination":roads[child][if side==0 {"from"} else {"to"}],
        "contact":contact,"full_world_validated":false});
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(&output).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    println!("Contact report saved; no world accepted");
    Ok(())
}
