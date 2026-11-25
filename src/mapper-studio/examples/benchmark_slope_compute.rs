//! Compare the existing CPU slope raster with CPU and GPU cutoff decisions.
//! Usage: benchmark_slope_compute MAP [CPU_WORKERS] [--cliff]
#[path = "../src/slope_compute.rs"]
mod slope_compute;
#[path = "../src/slope_gpu.rs"]
mod slope_gpu;

use papyrus_terrain::{cliff_topology, content, native_layers, terrain_slope};
use std::{env, fs, hint::black_box, time::Instant};

fn main() -> Result<(), String> {
    let mut args = env::args_os().skip(1);
    let path = args.next().ok_or("Supply a Rust .map file")?;
    let workers = args.next().map_or(Ok(8), |arg| {
        arg.to_string_lossy()
            .parse::<usize>()
            .map_err(|e| e.to_string())
    })?;
    let cliff = args.next().is_some_and(|arg| arg == "--cliff");
    let map = content::read_map(&fs::read(path).map_err(|e| e.to_string())?, || false)?;
    let (side, _) = native_layers::dimensions(map.size)?;
    let bytes = &map
        .layers
        .iter()
        .find(|layer| layer.name == "terrain")
        .ok_or("Map has no terrain layer")?
        .data;
    if bytes.len() != side * side * 2 {
        return Err("Incorrect terrain layer length".into());
    }
    let heights: Vec<_> = bytes
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let input = terrain_slope::Input {
        size: map.size,
        resolution: side,
        heights: &heights,
    };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .build()
        .map_err(|e| e.to_string())?;
    let threshold = slope_compute::Threshold::new(map.size, side, 3.)?;
    let start = Instant::now();
    let gpu = slope_gpu::Gpu::new()?;
    let setup_ms = start.elapsed().as_secs_f64() * 1000.;
    if cliff {
        return benchmark_cliff(map, heights, side, workers, &pool, &gpu, setup_ms);
    }
    let mut exact_ms = Vec::new();
    let mut cpu_ms = Vec::new();
    let mut gpu_ms = Vec::new();
    let mut gpu_transfer_compute_ms = Vec::new();
    for round in 0..7 {
        let start = Instant::now();
        let exact = terrain_slope::build(
            terrain_slope::Input { ..input },
            terrain_slope::Limits {
                maximum_bytes: 128 * 1024 * 1024,
                maximum_work: 100_000_000,
            },
            &pool,
            || false,
        )?
        .ok_or("CPU slope calculation cancelled")?;
        exact_ms.push(start.elapsed().as_secs_f64() * 1000.);
        let cpu = || {
            let start = Instant::now();
            let mut decisions = slope_compute::cpu(black_box(&input), &threshold, &pool);
            let resolved = slope_compute::resolve_with(&mut decisions, &pool, |cell| {
                terrain_slope::interior_angle(&input, cell) >= threshold.degrees
            });
            (decisions, resolved, start.elapsed().as_secs_f64() * 1000.)
        };
        let accelerated = || -> Result<_, String> {
            let start = Instant::now();
            let mut decisions = gpu.run(black_box(&input), &threshold, 64 * 1024 * 1024)?;
            let readback_ms = start.elapsed().as_secs_f64() * 1000.;
            let resolved = slope_compute::resolve_with(&mut decisions, &pool, |cell| {
                terrain_slope::interior_angle(&input, cell) >= threshold.degrees
            });
            Ok((
                decisions,
                resolved,
                start.elapsed().as_secs_f64() * 1000.,
                readback_ms,
            ))
        };
        let (cpu, accelerated) = if round % 2 == 0 {
            (cpu(), accelerated()?)
        } else {
            let accelerated = accelerated()?;
            (cpu(), accelerated)
        };
        for (cell, &angle) in exact.slopes_degrees.iter().enumerate() {
            let expected = u32::from(angle >= threshold.degrees);
            let shift = (cell % 16) * 2;
            if (accelerated.0[cell / 16] >> shift) & 3 != expected
                || (cpu.0[cell / 16] >> shift) & 3 != expected
            {
                return Err(format!(
                    "Cutoff decision mismatch at cell {cell}, slope {angle}"
                ));
            }
        }
        if cpu.1 != accelerated.1 {
            return Err("CPU and GPU differ on correction count".into());
        }
        cpu_ms.push(cpu.2);
        gpu_ms.push(accelerated.2);
        gpu_transfer_compute_ms.push(accelerated.3);
        eprintln!(
            "Round {} matched {} cells, {} exact threshold corrections",
            round + 1,
            heights.len(),
            cpu.1
        );
    }
    println!(
        "{}",
        serde_json::json!({
            "adapter": gpu.adapter.name,
            "backend": format!("{:?}", gpu.adapter.backend),
            "side": side, "workers": workers, "threshold": threshold.degrees,
            "setup_ms": setup_ms,
            "exact_cpu_ms": exact_ms, "threshold_cpu_ms": cpu_ms,
            "gpu_total_ms": gpu_ms, "gpu_validation_upload_compute_readback_ms": gpu_transfer_compute_ms,
        })
    );
    Ok(())
}

fn benchmark_cliff(
    mut map: content::MapContent,
    height: Vec<i16>,
    side: usize,
    workers: usize,
    pool: &rayon::ThreadPool,
    gpu: &slope_gpu::Gpu,
    setup_ms: f64,
) -> Result<(), String> {
    let (_, surface_resolution) = native_layers::dimensions(map.size)?;
    let mut layer = |name| {
        map.layers
            .iter_mut()
            .find(|layer| layer.name == name)
            .map(|layer| std::mem::take(&mut layer.data))
            .ok_or_else(|| format!("Map has no {name} layer"))
    };
    let splat = layer("splat")?;
    let topology = layer("topology")?
        .chunks_exact(4)
        .map(|bytes| i32::from_le_bytes(bytes.try_into().unwrap()))
        .collect();
    let layers = native_layers::Layers {
        size: map.size,
        terrain_seed: 0,
        biome_seed: 0,
        height_resolution: side,
        surface_resolution,
        height,
        topology,
        splat,
        biome: Vec::new(),
        normalized_biome_cells: 0,
        normalized_splat_cells: 0,
    };
    let profile: serde_json::Value = serde_json::from_str(include_str!(
        "../../../compatibility/topology/client-0f703d43-cliff.json"
    ))
    .map_err(|e| e.to_string())?;
    let policy: cliff_topology::Policy =
        serde_json::from_value(profile["policy"].clone()).map_err(|e| e.to_string())?;
    let threshold = slope_compute::Threshold::new(layers.size, side, policy.slope_cutoff)?;
    let input = terrain_slope::Input {
        size: layers.size,
        resolution: side,
        heights: &layers.height,
    };
    let mut exact_ms = Vec::new();
    let mut cpu_ms = Vec::new();
    let mut gpu_ms = Vec::new();
    for round in 0..7 {
        let start = Instant::now();
        let exact = cliff_topology::apply(layers.view(), policy, false, 192_000_000, || false)?
            .ok_or("CPU cliff classification cancelled")?;
        exact_ms.push(start.elapsed().as_secs_f64() * 1000.);
        let run = |accelerated: bool| -> Result<_, String> {
            let start = Instant::now();
            let mut words = if accelerated {
                gpu.run(&input, &threshold, 64 * 1024 * 1024)?
            } else {
                slope_compute::cpu(&input, &threshold, pool)
            };
            let corrected = slope_compute::resolve_with(&mut words, pool, |cell| {
                terrain_slope::interior_cliff_angle(&input, cell).unwrap() > threshold.degrees
            });
            let result = cliff_topology::apply_with_vertex_mask(
                layers.view(),
                policy,
                false,
                192_000_000,
                &words,
                || false,
            )?
            .ok_or("Cliff classification cancelled")?;
            let elapsed = start.elapsed().as_secs_f64() * 1000.;
            if result.0 != exact.0
                || serde_json::to_value(result.1).unwrap()
                    != serde_json::to_value(&exact.1).unwrap()
            {
                return Err("Cliff topology output or report changed".into());
            }
            Ok((elapsed, corrected))
        };
        let (cpu, accelerated) = if round % 2 == 0 {
            (run(false)?, run(true)?)
        } else {
            let accelerated = run(true)?;
            (run(false)?, accelerated)
        };
        if cpu.1 != accelerated.1 {
            return Err("Cliff correction counts differ".into());
        }
        cpu_ms.push(cpu.0);
        gpu_ms.push(accelerated.0);
        eprintln!(
            "Round {} matched {} topology cells and the entire stage report",
            round + 1,
            layers.topology.len()
        );
    }
    println!(
        "{}",
        serde_json::json!({
            "adapter": gpu.adapter.name,
            "backend": format!("{:?}", gpu.adapter.backend),
            "surface_side": surface_resolution, "height_side": side, "workers": workers,
            "threshold": threshold.degrees, "setup_ms": setup_ms,
            "exact_cpu_ms": exact_ms, "threshold_cpu_ms": cpu_ms, "gpu_total_ms": gpu_ms,
        })
    );
    Ok(())
}
