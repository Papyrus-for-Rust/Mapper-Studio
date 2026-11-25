//! Opt-in saved-height diagnostic. No source extraction, game or GUI execution.
use mapper_studio::map_normal::{self, Settings};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    time::Instant,
};
fn main() -> Result<(), String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 6 {
        return Err(
            "HEIGHT_I16LE SIZE HEIGHT_RESOLUTION EXPECTED_SHA256 THREADS NEW_REPORT".into(),
        );
    }
    let size = args[1].parse::<u32>().map_err(|e| e.to_string())?;
    let n = args[2].parse::<usize>().map_err(|e| e.to_string())?;
    let threads = args[4].parse::<usize>().map_err(|e| e.to_string())?;
    if !(17..=4097).contains(&n)
        || !(n - 1).is_power_of_two()
        || !(1..=64).contains(&threads)
        || !(1..=10000).contains(&size)
    {
        return Err("Unsupported diagnostic dimensions or thread limit".into());
    }
    let path = Path::new(&args[0]);
    let output = Path::new(&args[5]);
    if output.exists() {
        return Err("Report must be new".into());
    }
    if fs::metadata(path).map_err(|e| e.to_string())?.len() != (n * n * 2) as u64 {
        return Err("Height file size differs".into());
    }
    let raw = fs::read(path).map_err(|e| e.to_string())?;
    let input_sha = format!("{:x}", Sha256::digest(&raw));
    if input_sha != args[3] {
        return Err("Height input differs from requested receipt".into());
    }
    let height = raw
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect::<Vec<_>>();
    drop(raw);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(|e| e.to_string())?;
    let start = Instant::now();
    let grid = pool
        .install(|| {
            map_normal::generate(
                &height,
                Settings {
                    height_resolution: n,
                    size,
                    origin_y: -500.,
                    vertical_span: 1000.,
                    maximum_work: 2_000_000_000,
                    maximum_bytes: 64 * 1024 * 1024,
                },
                || false,
            )
        })?
        .ok_or("Cancelled")?;
    let elapsed = start.elapsed().as_secs_f64();
    let mut hash = Sha256::new();
    for p in grid.pixels() {
        hash.update(p);
    }
    for i in 0..4096 {
        let uv = [(i % 64) as f32 / 63., (i / 64) as f32 / 63.];
        let normal = grid.sample(uv).ok_or("Invalid sampled normal")?;
        if normal.iter().any(|v| !v.is_finite()) {
            return Err("Nonfinite sampled normal".into());
        }
    }
    let report = json!({"format":"papyrus-saved-height-normal-reference-check","schema":1,
        "engine":map_normal::VERSION,"height_sha256":input_sha,"size":size,"height_resolution":n,
        "threads":threads,"resolution":grid.resolution(),"pixel_sha256":format!("{:x}",hash.finalize()),
        "pixel_bytes":grid.report.pixel_bytes,"blocks":grid.report.blocks,"work_reserved":grid.report.work_reserved,
        "build_profile":if cfg!(debug_assertions){"debug"}else{"release"},"elapsed_seconds":elapsed,
        "sampled_points":4096,"sampled_normals_finite":true,"world_generated":false,
        "game_code_executed":false,"renderer_connected":false,"unity_compressor_byte_parity":false});
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output)
        .map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    println!(
        "Prepared {} square normal reference with {threads} threads",
        grid.resolution()
    );
    Ok(())
}
