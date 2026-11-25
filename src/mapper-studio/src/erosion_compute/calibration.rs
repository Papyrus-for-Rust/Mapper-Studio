//! Compare production kernels on a bounded working copy at the requested resolution.
use super::{Identity, MIB, check_buffer_allowance, detect, required_memory_mib, run};
use crate::generation_resources;
use papyrus_terrain::{erosion_grid, native_layers};
use serde::Serialize;
use std::time::Instant;

const WARMUP_ITERATIONS: usize = 16;
const STEADY_END_ITERATION: usize = 48;
const SAMPLE_ITERATIONS: usize = 64;
const REFERENCE_ITERATIONS: usize = 512;

#[derive(Serialize)]
pub struct Recommendation {
    pub resolution: usize,
    pub cpu_workers: usize,
    pub gpu_memory_limit_mib: usize,
    pub terrain_memory_limit_mib: usize,
    pub required_gpu_memory_mib: u64,
    pub gpu: Option<Identity>,
    pub cpu_sample_seconds: Option<f64>,
    pub gpu_sample_seconds: Option<f64>,
    pub cpu_estimated_seconds: Option<f64>,
    pub gpu_estimated_seconds: Option<f64>,
    pub sample_iterations: usize,
    pub estimate_iterations: usize,
    pub prefer_gpu: bool,
    pub reason: String,
}

/// Choose for a new binding. The caller saves the resulting concrete identity
/// to keep subsequent runs independent of machine load and benchmark variation.
pub fn recommend(
    map_size: u32,
    cancelled: impl Fn() -> bool + Sync,
) -> Result<Option<Recommendation>, String> {
    let (resolution, _) = native_layers::dimensions(map_size)?;
    let required_gpu_memory_mib =
        required_memory_mib(resolution).ok_or("Invalid erosion comparison resolution")?;
    generation_resources::with_cpu_pool(&cancelled, |pool, resources| {
        let gpu_limit = resources.gpu_memory_mib as u64 * MIB;
        let mut result = Recommendation {
            resolution,
            cpu_workers: pool.current_num_threads(),
            gpu_memory_limit_mib: resources.gpu_memory_mib,
            terrain_memory_limit_mib: resources.terrain_memory_mib,
            required_gpu_memory_mib,
            gpu: None,
            cpu_sample_seconds: None,
            gpu_sample_seconds: None,
            cpu_estimated_seconds: None,
            gpu_estimated_seconds: None,
            sample_iterations: SAMPLE_ITERATIONS,
            estimate_iterations: REFERENCE_ITERATIONS,
            prefer_gpu: false,
            reason: String::new(),
        };
        let cells = resolution * resolution;
        let memory = resources.terrain_memory_mib * MIB as usize;
        let limit = erosion_grid::Limits {
            // The two caller-owned input grids remain live while either kernel runs.
            maximum_bytes: memory.saturating_sub(cells * 8),
            maximum_work: 64_000_000_000,
        };
        // Require enough space to compare both backends at the actual grid size.
        if limit.maximum_bytes < cells * 44 {
            return Err("Increase the terrain memory allowance to compare erosion backends".into());
        }
        let gpu = match check_buffer_allowance(resolution, gpu_limit).and_then(|_| detect()) {
            Ok(gpu) => gpu,
            Err(reason) => {
                result.reason = reason;
                return Ok((!cancelled()).then_some(result));
            }
        };
        if cancelled() {
            return Ok(None);
        }
        let mut heights = Vec::new();
        let mut floor = Vec::new();
        for buffer in [&mut heights, &mut floor] {
            buffer.try_reserve_exact(cells).map_err(|e| e.to_string())?;
        }
        let radius = (resolution / 2) as f32;
        for row in 0..resolution {
            if cancelled() {
                return Ok(None);
            }
            let z = (row as f32 - radius) / radius;
            for col in 0..resolution {
                let x = (col as f32 - radius) / radius;
                let ridge = ((col % 97) as f32 - (row % 71) as f32) * 0.15;
                let height = 145. - 200. * (x * x + z * z) + ridge;
                heights.push(height);
                floor.push(height - 10.);
            }
        }
        let input = || erosion_grid::Input {
            resolution,
            heights: &heights,
            minimum_heights: &floor,
        };
        let policy = erosion_grid::Policy {
            iterations: SAMPLE_ITERATIONS,
            time_step: 0.06,
            water_fill_rate: 0.04,
            evaporation_rate: 0.015,
            gravity: 10.,
            pipe_area: 1.,
            pipe_length: 1.,
            cell_area: 4.,
            normal_y: 0.5,
            ocean_level: 0.,
            minimum_angle: 0.01047198,
            sediment_capacity: 0.0015,
            dissolve_rate: 0.15,
            depth_fade: 10.,
        };
        let mut gpu_timing = Timing::start();
        let gpu_estimate = match run(
            &gpu,
            input(),
            policy,
            limit,
            gpu_limit,
            &cancelled,
            |done, _| {
                gpu_timing.progress(done);
            },
        ) {
            Ok(Some(output)) => {
                let (sample, estimate) = gpu_timing.finish()?;
                drop(output);
                result.gpu_sample_seconds = Some(sample);
                result.gpu_estimated_seconds = Some(estimate);
                result.gpu = Some(gpu);
                estimate
            }
            Ok(None) => return Ok(None),
            Err(reason) => {
                result.reason = reason;
                return Ok((!cancelled()).then_some(result));
            }
        };
        let mut cpu_timing = Timing::start();
        let Some(output) =
            erosion_grid::run(input(), policy, limit, pool, &cancelled, |done, _| {
                cpu_timing.progress(done);
            })?
        else {
            return Ok(None);
        };
        let (sample, estimate) = cpu_timing.finish()?;
        drop(output);
        result.cpu_sample_seconds = Some(sample);
        result.cpu_estimated_seconds = Some(estimate);
        result.prefer_gpu = gpu_estimate < estimate * 0.9;
        result.reason = if result.prefer_gpu {
            "GPU was faster at the requested erosion resolution"
        } else {
            "Keep CPU when GPU does not have a clear speed advantage"
        }
        .into();
        Ok((!cancelled()).then_some(result))
    })
}

struct Timing {
    start: Instant,
    steady_start: Option<Instant>,
    steady_end: Option<Instant>,
}

impl Timing {
    fn start() -> Self {
        Self {
            start: Instant::now(),
            steady_start: None,
            steady_end: None,
        }
    }

    fn progress(&mut self, done: usize) {
        match done {
            WARMUP_ITERATIONS => self.steady_start = Some(Instant::now()),
            STEADY_END_ITERATION => self.steady_end = Some(Instant::now()),
            _ => {}
        }
    }

    fn finish(self) -> Result<(f64, f64), String> {
        let total = self.start.elapsed().as_secs_f64();
        let (Some(start), Some(end)) = (self.steady_start, self.steady_end) else {
            return Err("Erosion comparison did not finish all iterations".into());
        };
        // GPU initialization and readback run once. Extrapolate from the
        // middle iterations, which exclude both the first and final batches.
        let seconds_per_iteration =
            (end - start).as_secs_f64() / (STEADY_END_ITERATION - WARMUP_ITERATIONS) as f64;
        let estimate =
            total + (REFERENCE_ITERATIONS - SAMPLE_ITERATIONS) as f64 * seconds_per_iteration;
        Ok((total, estimate))
    }
}
