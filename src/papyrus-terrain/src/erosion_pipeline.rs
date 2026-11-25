//! Early hydraulic terrain and its retained input for late material painting.
use crate::{erosion_bounds, erosion_grid, erosion_splat, native_layers::Layers, terrain_slope};
use serde::Serialize;

pub const VERSION: &str = "papyrus-erosion-pipeline-1";

#[derive(Clone, Copy)]
pub struct Policy {
    pub grid: erosion_grid::Policy,
    pub bounds: erosion_bounds::Policy,
    pub paint: erosion_splat::Policy,
}

#[derive(Clone, Copy)]
pub struct Limits {
    /// Peak owned buffer payload across these two stages, excluding caller layers.
    pub maximum_bytes: usize,
    /// Shared early/late work allowance, including height conversion.
    pub maximum_work: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Frame {
    size: u32,
    terrain_seed: u32,
    biome_seed: u32,
    height_resolution: usize,
    surface_resolution: usize,
}
impl Frame {
    fn of(layers: &Layers) -> Result<Self, String> {
        let n = layers.height_resolution;
        let s = layers.surface_resolution;
        if !(1000..=6000).contains(&layers.size)
            || !(3..=4097).contains(&n)
            || !(1..=4096).contains(&s)
            || layers.height.len() != n * n
            || layers.topology.len() != s * s
            || layers.splat.len() != s * s * 8
        {
            return Err("Invalid erosion terrain channels".into());
        }
        Ok(Self {
            size: layers.size,
            terrain_seed: layers.terrain_seed,
            biome_seed: layers.biome_seed,
            height_resolution: n,
            surface_resolution: s,
        })
    }
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub bounds: erosion_bounds::Report,
    pub grid: erosion_grid::Report,
    pub work: u64,
    pub peak_buffer_bytes: usize,
    pub changed_height_samples: usize,
}

/// Retains the unquantized original-minus-final delta across intervening stages.
/// Current height/topology/splat channels are supplied only when painting runs.
pub struct Painting {
    frame: Frame,
    delta: Vec<f32>,
    policy: erosion_splat::Policy,
    limits: Limits,
    early_work: u64,
    early_peak: usize,
}
impl Painting {
    pub fn height_delta(&self) -> &[f32] {
        &self.delta
    }

    /// Rebind a verified saved erosion delta to the current terrain and limits.
    pub fn restore(
        layers: &Layers,
        delta: Vec<f32>,
        policy: erosion_splat::Policy,
        limits: Limits,
        early_work: u64,
        early_peak: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        let frame = Frame::of(layers)?;
        policy.validate()?;
        if delta.len() != layers.height.len()
            || early_work < delta.len() as u64 * 2
            || early_work > limits.maximum_work
            || early_peak < std::mem::size_of_val(delta.as_slice())
            || early_peak > limits.maximum_bytes
        {
            return Err("Saved erosion exceeds its terrain or work limits".into());
        }
        for chunk in delta.chunks(16384) {
            if cancelled() {
                return Ok(None);
            }
            if chunk.iter().any(|v| !v.is_finite() || v.abs() > 1001.) {
                return Err("Invalid saved erosion delta".into());
            }
        }
        Ok((!cancelled()).then_some(Self {
            frame,
            delta,
            policy,
            limits,
            early_work,
            early_peak,
        }))
    }

    pub fn paint(
        &self,
        current: &Layers,
        pool: &rayon::ThreadPool,
        cancelled: impl Fn() -> bool + Sync,
    ) -> Result<Option<Painted>, String> {
        if Frame::of(current)? != self.frame {
            return Err("Late erosion terrain differs from its retained world".into());
        }
        let bytes = std::mem::size_of_val(self.delta.as_slice());
        let remaining_bytes = self
            .limits
            .maximum_bytes
            .checked_sub(bytes)
            .ok_or("Retained erosion delta exceeds its memory allowance")?;
        let remaining_work = self
            .limits
            .maximum_work
            .checked_sub(self.early_work)
            .ok_or("Early erosion exhausted the shared work allowance")?;
        let Some(slopes) = terrain_slope::build(
            terrain_slope::Input {
                size: current.size,
                resolution: current.height_resolution,
                heights: &current.height,
            },
            terrain_slope::Limits {
                maximum_bytes: remaining_bytes,
                maximum_work: usize::try_from(remaining_work).unwrap_or(usize::MAX),
            },
            pool,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        let remaining_bytes = remaining_bytes
            .checked_sub(slopes.report.buffer_bytes)
            .ok_or("Current erosion slopes exceed the shared memory allowance")?;
        let remaining_work = remaining_work
            .checked_sub(slopes.report.work as u64)
            .ok_or("Current erosion slopes exceed the shared work allowance")?;
        let Some(output) = erosion_splat::paint(
            erosion_splat::Input {
                height_resolution: current.height_resolution,
                height_delta: &self.delta,
                slopes_degrees: &slopes.slopes_degrees,
                topology_resolution: current.surface_resolution,
                topology: &current.topology,
                splat_resolution: current.surface_resolution,
                splat: &current.splat,
                terrain_one_over_size: 1. / current.size as f32,
            },
            self.policy,
            erosion_splat::Limits {
                maximum_bytes: remaining_bytes,
                maximum_work: usize::try_from(remaining_work).unwrap_or(usize::MAX),
            },
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        if cancelled() {
            return Ok(None);
        }
        let peak_buffer_bytes = self
            .early_peak
            .max(bytes + slopes.report.buffer_bytes + output.report.peak_buffer_bytes);
        let work = self.early_work + slopes.report.work as u64 + output.report.work as u64;
        Ok(Some(Painted {
            output,
            slopes: slopes.report,
            work,
            peak_buffer_bytes,
        }))
    }
}

pub struct Eroded {
    pub height: Vec<i16>,
    pub painting: Painting,
    pub report: Report,
}

pub struct Painted {
    pub output: erosion_splat::Output,
    pub slopes: terrain_slope::Report,
    pub work: u64,
    pub peak_buffer_bytes: usize,
}

// Source BitUtility uses a reciprocal multiply, then world scale and offset.
// Keep conversion separate from the older approximation's division-based codec.
fn decode(height: i16) -> f32 {
    -500. + (height as f32 * (1. / 32766.)) * 1000.
}
fn encode(height: f32) -> Result<i16, String> {
    let encoded = (0.001 * (height - -500.)) * 32766. + 0.5;
    if !encoded.is_finite() || !(0. ..=32766.).contains(&encoded.trunc()) {
        return Err("Eroded height exceeds the terrain encoding range".into());
    }
    Ok(encoded as i16)
}

/// Produce private replacement heights. Cancellation/errors leave caller layers
/// untouched, including failures during final terrain quantization.
pub fn erode(
    layers: &Layers,
    policy: Policy,
    limits: Limits,
    pool: &rayon::ThreadPool,
    cancelled: impl Fn() -> bool + Sync,
    progress: impl FnMut(usize, usize) + Send,
) -> Result<Option<Eroded>, String> {
    erode_with(
        layers,
        policy,
        limits,
        &cancelled,
        |input, policy, limits| {
            erosion_grid::run(input, policy, limits, pool, &cancelled, progress)
        },
    )
}

/// Share source floor construction, height encoding and retained painting across kernels.
pub fn erode_with(
    layers: &Layers,
    policy: Policy,
    limits: Limits,
    cancelled: impl Fn() -> bool + Sync,
    run: impl FnOnce(
        erosion_grid::Input<'_>,
        erosion_grid::Policy,
        erosion_grid::Limits,
    ) -> Result<Option<erosion_grid::Output>, String>,
) -> Result<Option<Eroded>, String> {
    let frame = Frame::of(layers)?;
    policy.grid.validate()?;
    policy.bounds.validate()?;
    policy.paint.validate()?;
    let cells = layers.height.len();
    let conversion_bytes = cells * std::mem::size_of::<f32>();
    let conversion_work = cells as u64 * 2;
    let remaining_bytes = limits
        .maximum_bytes
        .checked_sub(conversion_bytes)
        .ok_or("Erosion height conversion exceeds its memory allowance")?;
    let remaining_work = limits
        .maximum_work
        .checked_sub(conversion_work)
        .ok_or("Erosion height conversion exceeds its work allowance")?;
    let mut heights = Vec::new();
    heights
        .try_reserve_exact(cells)
        .map_err(|e| e.to_string())?;
    for row in layers.height.chunks(frame.height_resolution) {
        if cancelled() {
            return Ok(None);
        }
        for &height in row {
            if !(0..=32766).contains(&height) {
                return Err("Invalid encoded erosion terrain height".into());
            }
            heights.push(decode(height));
        }
    }
    let Some(bounds) = erosion_bounds::build(
        erosion_bounds::Input {
            height_resolution: frame.height_resolution,
            heights: &heights,
            topology_resolution: frame.surface_resolution,
            topology: &layers.topology,
            terrain_one_over_size: 1. / frame.size as f32,
            ocean_level: policy.grid.ocean_level,
        },
        policy.bounds,
        erosion_bounds::Limits {
            maximum_bytes: remaining_bytes,
            maximum_work: usize::try_from(remaining_work).unwrap_or(usize::MAX),
        },
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let grid_limits = erosion_grid::Limits {
        maximum_bytes: remaining_bytes
            .checked_sub(bounds.report.retained_buffer_bytes)
            .ok_or("Erosion floors exceed the shared memory allowance")?,
        maximum_work: remaining_work
            .checked_sub(bounds.report.work as u64)
            .ok_or("Erosion floors exceed the shared work allowance")?,
    };
    let Some(grid) = run(
        erosion_grid::Input {
            resolution: frame.height_resolution,
            heights: &heights,
            minimum_heights: &bounds.minimum_heights,
        },
        policy.grid,
        grid_limits,
    )?
    else {
        return Ok(None);
    };
    if grid.heights.len() != cells
        || grid.height_delta.len() != cells
        || grid.report.buffer_bytes > grid_limits.maximum_bytes
        || grid.report.work > grid_limits.maximum_work
    {
        return Err("Erosion kernel returned invalid dimensions or resource accounting".into());
    }
    let peak_buffer_bytes = (conversion_bytes + bounds.report.peak_buffer_bytes)
        .max(conversion_bytes + bounds.report.retained_buffer_bytes + grid.report.buffer_bytes);
    let work = conversion_work + bounds.report.work as u64 + grid.report.work;
    drop(heights);
    drop(bounds.minimum_heights);
    let mut height = Vec::new();
    height.try_reserve_exact(cells).map_err(|e| e.to_string())?;
    let mut changed_height_samples = 0;
    for (row, original) in grid
        .heights
        .chunks(frame.height_resolution)
        .zip(layers.height.chunks(frame.height_resolution))
    {
        if cancelled() {
            return Ok(None);
        }
        for (&value, &old) in row.iter().zip(original) {
            let value = encode(value)?;
            changed_height_samples += usize::from(value != old);
            height.push(value);
        }
    }
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(Eroded {
        height,
        painting: Painting {
            frame,
            delta: grid.height_delta,
            policy: policy.paint,
            limits,
            early_work: work,
            early_peak: peak_buffer_bytes,
        },
        report: Report {
            engine: VERSION,
            bounds: bounds.report,
            grid: grid.report,
            work,
            peak_buffer_bytes,
            changed_height_samples,
        },
    }))
}

#[cfg(test)]
#[path = "erosion_pipeline_tests.rs"]
mod tests;
