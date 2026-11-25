//! Headless validation of the native GPU erosion kernel on captured terrain.
use mapper_studio::erosion_compute;
use papyrus_terrain::erosion_grid::{self, Policy};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

// Match the production pipeline's reciprocal-multiply terrain encoding.
fn encode_height(height: f32) -> Result<i16, String> {
    let encoded = (0.001 * (height - -500.)) * 32766. + 0.5;
    if !(0. ..=32766.).contains(&encoded.trunc()) {
        return Err("Eroded height exceeds the terrain encoding range".into());
    }
    Ok(encoded as i16)
}

fn read_grid(path: &str, cells: usize) -> Result<Vec<f32>, String> {
    let path = Path::new(path);
    let mut file = File::open(path).map_err(|err| err.to_string())?;
    if file.metadata().map_err(|err| err.to_string())?.len() != cells as u64 * 4 {
        return Err(format!("Invalid grid length for {}", path.display()));
    }
    let mut bytes = vec![0; cells * 4];
    file.read_exact(&mut bytes).map_err(|err| err.to_string())?;
    if file.read(&mut [0]).map_err(|err| err.to_string())? != 0 {
        return Err(format!("Grid changed while reading {}", path.display()));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

fn simulate(
    n: u32,
    original: &[f32],
    floor: &[f32],
    policy: Policy,
    cancelled: &AtomicBool,
) -> Result<Option<serde_json::Value>, String> {
    let identity = erosion_compute::detect()?;
    let Some(run) = erosion_compute::run(
        &identity,
        erosion_grid::Input {
            resolution: n as usize,
            heights: original,
            minimum_heights: floor,
        },
        policy,
        erosion_grid::Limits {
            maximum_bytes: 1024 * 1024 * 1024,
            maximum_work: 64_000_000_000,
        },
        1024 * 1024 * 1024,
        || cancelled.load(Ordering::Relaxed),
        |_, _| {},
    )?
    else {
        return Ok(None);
    };
    let mut encoded = Sha256::new();
    for &height in &run.output.heights {
        encoded.update(encode_height(height)?.to_le_bytes());
    }
    Ok(Some(json!({
        "engine": identity.engine, "adapter": identity.name, "backend": identity.backend,
        "vendor_id": identity.vendor_id, "device_id": identity.device_id, "driver": identity.driver,
        "driver_info": identity.driver_info, "policy": policy,
        "resolution": n, "iterations": policy.iterations, "explicit_buffer_bytes": run.explicit_buffer_bytes,
        "setup_seconds": run.setup_seconds, "simulation_and_copy_seconds": run.simulation_seconds,
        "map_readback_seconds": run.readback_seconds, "verification_seconds": run.verification_seconds,
        "maximum_lowering": run.output.report.maximum_lowering,
        "input_height_sha256": format!("{:x}", Sha256::digest(bytemuck::cast_slice(original))),
        "floor_sha256": format!("{:x}", Sha256::digest(bytemuck::cast_slice(floor))),
        "float_height_sha256": format!("{:x}", Sha256::digest(bytemuck::cast_slice(&run.output.heights))),
        "height_sha256": format!("{:x}", encoded.finalize()),
        "delta_sha256": format!("{:x}", Sha256::digest(bytemuck::cast_slice(&run.output.height_delta))),
    })))
}

fn cpu_reference(
    n: u32,
    heights: &[f32],
    floor: &[f32],
    policy: Policy,
    workers: usize,
    cancelled: &AtomicBool,
) -> Result<serde_json::Value, String> {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .build()
        .map_err(|err| err.to_string())?;
    let started = Instant::now();
    let result = erosion_grid::run(
        erosion_grid::Input {
            resolution: n as usize,
            heights,
            minimum_heights: floor,
        },
        policy,
        erosion_grid::Limits {
            maximum_bytes: 1024 * 1024 * 1024,
            maximum_work: 64_000_000_000,
        },
        &pool,
        || cancelled.load(Ordering::Relaxed),
        |_, _| {},
    )?
    .ok_or("CPU comparison cancelled")?;
    let seconds = started.elapsed().as_secs_f64();
    let mut encoded = Sha256::new();
    for height in &result.heights {
        encoded.update(encode_height(*height)?.to_le_bytes());
    }
    Ok(json!({
        "workers": workers, "simulation_seconds": seconds, "engine": result.report.engine,
        "float_height_sha256": format!("{:x}", Sha256::digest(bytemuck::cast_slice(&result.heights))),
        "height_sha256": format!("{:x}", encoded.finalize()),
        "delta_sha256": format!("{:x}", Sha256::digest(bytemuck::cast_slice(&result.height_delta))),
    }))
}

fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    const USAGE: &str = "Usage: erosion_gpu RESOLUTION HEIGHT_F32LE FLOOR_F32LE POLICY_REPORT [--compare-cpu WORKERS]";
    let [
        resolution,
        height_path,
        floor_path,
        policy_path,
        options @ ..,
    ] = args.as_slice()
    else {
        return Err(USAGE.into());
    };
    let cpu_workers = match options {
        [] => None,
        [flag, workers] if flag == "--compare-cpu" => Some(
            workers
                .parse::<usize>()
                .ok()
                .filter(|workers| (1..=256).contains(workers))
                .ok_or("CPU worker count must be between 1 and 256")?,
        ),
        _ => return Err(USAGE.into()),
    };
    let n = resolution.parse::<u32>().map_err(|err| err.to_string())?;
    if !(3..=4097).contains(&n) {
        return Err("Resolution must be between 3 and 4097".into());
    }
    let cells = (n * n) as usize;
    let heights = read_grid(height_path, cells)?;
    let floor = read_grid(floor_path, cells)?;
    if heights.iter().zip(&floor).any(|(&h, &f)| {
        !h.is_finite() || !f.is_finite() || h.abs() > 65536. || f.abs() > 65536. || f > h
    }) {
        return Err("Invalid erosion height or floor values".into());
    }
    let file = File::open(policy_path).map_err(|err| err.to_string())?;
    const MAX_POLICY_BYTES: u64 = 2 * 1024 * 1024;
    if file.metadata().map_err(|err| err.to_string())?.len() > MAX_POLICY_BYTES {
        return Err("Erosion policy exceeds its size limit".into());
    }
    let report: serde_json::Value =
        serde_json::from_reader(file.take(MAX_POLICY_BYTES)).map_err(|err| err.to_string())?;
    let policy = serde_json::from_value(report.get("grid").cloned().unwrap_or(report))
        .map_err(|err| err.to_string())?;
    let cancelled = std::sync::Arc::new(AtomicBool::new(false));
    let signal = cancelled.clone();
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))
        .map_err(|err| err.to_string())?;
    let mut result =
        simulate(n, &heights, &floor, policy, &cancelled)?.ok_or("Erosion cancelled")?;
    if let Some(workers) = cpu_workers {
        result["cpu"] = cpu_reference(n, &heights, &floor, policy, workers, &cancelled)?;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&result).map_err(|err| err.to_string())?
    );
    Ok(())
}
