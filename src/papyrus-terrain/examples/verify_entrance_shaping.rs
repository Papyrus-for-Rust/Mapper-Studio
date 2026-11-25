//! CAPTURE_DIRECTORY NUMERIC_ROUTE_JSON NEW_REPORT. No game or GUI is started.
use papyrus_terrain::{path_geometry, path_terrain};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
};
fn read(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("Diagnostic input exceeds limit".into());
    }
    Ok(bytes)
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err("CAPTURE_DIRECTORY NUMERIC_ROUTE_JSON NEW_REPORT".into());
    }
    let root = Path::new(&args[0]);
    let output = Path::new(&args[2]);
    if output.exists() {
        return Err("Report must be new".into());
    }
    let document = read(&root.join("capture.json"), 4 * 1024 * 1024)?;
    let capture: Value = serde_json::from_slice(&document).map_err(|e| e.to_string())?;
    if !matches!(
        capture["format"].as_str(),
        Some("mapper-initial-road-grade-capture-1" | "mapper-initial-road-grade-capture-2")
    ) {
        return Err("Unsupported captured terrain".into());
    }
    let raw = read(&root.join("ground.i16le"), 64 * 1024 * 1024)?;
    if capture["files"]["ground.i16le"]["bytes"] != raw.len() as u64
        || capture["files"]["ground.i16le"]["sha256"] != hash(&raw)
    {
        return Err("Ground receipt differs".into());
    }
    let n = capture["resolution"]
        .as_u64()
        .filter(|v| (2..=4097).contains(v))
        .ok_or("Invalid resolution")? as usize;
    if raw.len() != n * n * 2 {
        return Err("Invalid ground raster length".into());
    }
    let ground: Vec<_> = raw
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    let scalar = |key: &str| {
        capture[key]
            .as_f64()
            .filter(|v| v.is_finite())
            .map(|v| v as f32)
            .ok_or("Missing finite grid scalar")
    };
    let grid = path_terrain::Grid {
        size: scalar("size")?,
        resolution: n,
        origin_y: scalar("origin_y")?,
        vertical_span: scalar("vertical_span")?,
        height: &ground,
    };
    let route_bytes = read(Path::new(&args[1]), 4 * 1024 * 1024)?;
    let route: Value = serde_json::from_slice(&route_bytes).map_err(|e| e.to_string())?;
    if route["capture_sha256"] != hash(&document) {
        return Err("Route belongs to another captured input".into());
    }
    let points: Vec<[f32; 3]> =
        serde_json::from_value(route["route"].clone()).map_err(|e| e.to_string())?;
    let mut variants = Vec::new();
    for (name, path) in [
        (
            "raw_hermite",
            path_geometry::Path::from_controls(&points, 1_000_000, || false),
        ),
        (
            "source_shaped",
            path_geometry::build(
                &points,
                path_geometry::Settings {
                    horizontal_iterations: 4,
                    vertical_iterations: 16,
                    resample_distance: 7.5,
                    maximum_work: 1_000_000,
                },
                None,
                || false,
            ),
        ),
    ] {
        let path = path
            .map_err(|e| format!("Path shaping: {e:?}"))?
            .ok_or("Cancelled")?;
        let heights = path
            .points()
            .iter()
            .map(|&p| path_terrain::sample_height(grid, p))
            .collect::<Result<Vec<_>, _>>()?;
        let snapped = path
            .refit_heights(&heights, 0, &vec![1.; heights.len()], 1_000_000, || false)
            .map_err(|e| format!("Ground resampling: {e:?}"))?
            .ok_or("Cancelled")?;
        let steps = (snapped.length() / 0.25).ceil() as usize;
        if steps > 200000 {
            return Err("Path sampling budget exceeded".into());
        }
        let mut maximum = 0_f64;
        let mut previous = None;
        for step in 0..=steps {
            let mut p = snapped
                .point(snapped.length() * step as f32 / steps as f32)
                .map_err(|e| format!("Curve query: {e:?}"))?;
            p[1] = path_terrain::sample_height(grid, p)?;
            if let Some(q) = previous {
                let q: [f32; 3] = q;
                let run = f64::from(p[0] - q[0]).hypot(f64::from(p[2] - q[2]));
                if run > 1e-6 {
                    maximum = maximum.max(f64::from(p[1] - q[1]).abs() / run);
                }
            }
            previous = Some(p);
        }
        variants.push(json!({"variant":name,"controls":snapped.points(),"length":snapped.length(),"work":path.work()+snapped.work(),"ground_queries":steps+1,
            "maximum_sampled_ground_grade":maximum,"grade_limit":capture["settings"]["maximum_grade"],"sampled_ground_grade_passed":maximum<=capture["settings"]["maximum_grade"].as_f64().ok_or("Missing grade limit")?}));
    }
    let report = json!({"format":"mapper-fine-entrance-shaping-study-1","capture_sha256":hash(&document),"route_sha256":hash(&route_bytes),"variants":variants,
        "terrain_unchanged":true,"full_map_validated":false,"scope":"Source-style 4 XZ / 16 Y smoothing and 7.5 m resampling; final ground sampling. Does not replay road paint or full-world acceptance."});
    let bytes = serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| e.to_string())?
        .write_all(&bytes)
        .map_err(|e| e.to_string())?;
    println!("{report}");
    Ok(())
}
