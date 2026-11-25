//! CAPTURE NEW_REPORT. Replay sampled control constraints without a game/GUI.
use papyrus_terrain::{path_geometry::Path as Curve, path_terrain, road_bed, road_grade};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::Path,
};
#[derive(Deserialize)]
struct Receipt {
    bytes: usize,
    sha256: String,
}
#[derive(Deserialize)]
struct Road {
    points: Vec<[f32; 3]>,
    ground: Vec<f32>,
    fixed_topology: Vec<bool>,
    closed: bool,
}
#[derive(Deserialize)]
struct Capture {
    format: String,
    size: f32,
    resolution: usize,
    origin_y: f32,
    vertical_span: f32,
    surface_resolution: usize,
    protected_topology: i32,
    settings: road_bed::Settings,
    maximum_initial_work: usize,
    height_reserve: f32,
    coupled_junctions: bool,
    roads: Vec<Road>,
    junctions: Vec<[(usize, usize); 2]>,
    files: BTreeMap<String, Receipt>,
}
fn read(path: &Path) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(128 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 128 * 1024 * 1024 {
        return Err("Capture buffer exceeds bound".into());
    }
    Ok(bytes)
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("CAPTURE NEW_REPORT".into());
    }
    let root = Path::new(&args[0]);
    let output = Path::new(&args[1]);
    if output.exists() {
        return Err("Report must be new".into());
    }
    let raw = read(&root.join("capture.json"))?;
    let c: Capture = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    if c.format != "mapper-initial-road-grade-capture-1"
        || c.roads.len() > 4096
        || !(2..=4097).contains(&c.resolution)
        || !(2..=2048).contains(&c.surface_resolution)
        || c.files.len() != 3
    {
        return Err("Unsupported capture".into());
    }
    let checked = |name: &str| -> Result<Vec<u8>, String> {
        let raw = read(&root.join(name))?;
        let receipt = c.files.get(name).ok_or("Missing receipt")?;
        if raw.len() != receipt.bytes || hash(&raw) != receipt.sha256 {
            return Err(format!("Capture receipt changed: {name}"));
        }
        Ok(raw)
    };
    let height = checked("height.i16le")?;
    let ground = checked("ground.i16le")?;
    let topology = checked("topology.i32le")?;
    if height.len() != c.resolution * c.resolution * 2
        || ground.len() != height.len()
        || topology.len() != c.surface_resolution * c.surface_resolution * 4
    {
        return Err("Capture dimensions differ".into());
    }
    let height = height
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes(b.try_into().unwrap()))
        .collect::<Vec<_>>();
    let ground = ground
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes(b.try_into().unwrap()))
        .collect::<Vec<_>>();
    let topology = topology
        .chunks_exact(4)
        .map(|b| i32::from_le_bytes(b.try_into().unwrap()))
        .collect::<Vec<_>>();
    let grid = path_terrain::Grid {
        size: c.size,
        resolution: c.resolution,
        origin_y: c.origin_y,
        vertical_span: c.vertical_span,
        height: &height,
    };
    let mut paths = Vec::new();
    for r in &c.roads {
        if r.points.len() != r.ground.len() || r.points.len() != r.fixed_topology.len() {
            return Err("Control dimensions differ".into());
        }
        for (i, &p) in r.points.iter().enumerate() {
            let before = path_terrain::sample_height(
                path_terrain::Grid {
                    height: &ground,
                    ..grid
                },
                p,
            )?;
            let desired = path_terrain::sample_height(grid, p)?;
            let n = c.surface_resolution;
            let [x, z] = [p[0], p[2]].map(|v| {
                (((v + c.size * 0.5) * (1. / c.size)) * n as f32).clamp(0., (n - 1) as f32) as usize
            });
            if before.to_bits() != r.ground[i].to_bits()
                || desired.to_bits() != p[1].to_bits()
                || (topology[z * n + x] & c.protected_topology != 0) != r.fixed_topology[i]
            {
                return Err("Captured sample/flag differs from actual grid".into());
            }
        }
        let path = Curve::from_controls(&r.points, 32_000_000, || false)
            .map_err(|e| format!("Curve: {e:?}"))?
            .ok_or("Cancelled")?;
        if path.is_circular() != r.closed {
            return Err("Captured circular state differs".into());
        }
        paths.push(path);
    }
    let roads = paths
        .iter()
        .zip(&c.roads)
        .map(|(path, r)| road_grade::Road {
            path,
            ground: &r.ground,
            fixed: &r.fixed_topology,
        })
        .collect::<Vec<_>>();
    let joins = c
        .junctions
        .iter()
        .map(|j| road_grade::Junction { a: j[0], b: j[1] })
        .collect::<Vec<_>>();
    let mode = if c.coupled_junctions {
        road_grade::JunctionMode::CoupledNetwork
    } else {
        road_grade::JunctionMode::Pinned
    };
    let mut attempts = Vec::new();
    for reserve in [
        c.height_reserve,
        c.height_reserve * 0.75,
        c.height_reserve * 0.5,
        c.height_reserve * 0.25,
        0.,
    ] {
        let (result, conflict) = road_grade::propose_diagnosed(
            &roads,
            &joins,
            c.settings.maximum_grade,
            c.settings.maximum_cut_fill,
            reserve,
            mode,
            c.maximum_initial_work.min(32_000_000),
            || false,
        );
        let attempt = match result {
            Ok(p) => json!({"reserve":reserve,"feasible":true,"proposal":p.report}),
            Err(e) => {
                json!({"reserve":reserve,"feasible":false,"error":format!("{e:?}"),"conflict":conflict})
            }
        };
        attempts.push(attempt);
    }
    if read(&root.join("capture.json"))? != raw {
        return Err("Capture changed during replay".into());
    }
    let report = json!({"format":"mapper-control-grade-replay-1","capture_sha256":hash(&raw),"roads":roads.len(),
        "attempts":attempts,"sampled_inputs_verified":true,"terrain_or_mesh_accepted":false,"game_runtime_validated":false});
    fs::File::options()
        .create_new(true)
        .write(true)
        .open(output)
        .map_err(|e| e.to_string())?
        .write_all(&serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    println!(
        "{}",
        serde_json::to_string(&report).map_err(|e| e.to_string())?
    );
    Ok(())
}
