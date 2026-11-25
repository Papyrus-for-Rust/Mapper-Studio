//! Port of the existing bounded-grid hydraulic erosion algorithm.
//! This preserves the independent C# approximation, not the complete official jobs.
//! Each pass writes disjoint cells and observes a complete previous pass.
use rayon::prelude::*;

const DT: f32 = 0.06;
pub const ITERATIONS: usize = 512;

#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub resolution: usize,
    pub world_size: usize,
    pub threads: usize,
    pub iterations: usize,
}

impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if !(33..=4097).contains(&self.resolution) || !(self.resolution - 1).is_power_of_two() {
            return Err("erosion resolution must be 2^n + 1, between 33 and 4097".into());
        }
        if !(1000..=6000).contains(&self.world_size) {
            return Err("world size must be between 1000 and 6000".into());
        }
        if !(1..=1024).contains(&self.threads) {
            return Err("threads must be between 1 and 1024".into());
        }
        if !(1..=ITERATIONS).contains(&self.iterations) {
            return Err("erosion iterations must be between 1 and 512".into());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Default)]
struct Cell {
    height: f32,
    water: f32,
    velocity_x: f32,
    velocity_z: f32,
    sediment: f32,
}

#[derive(Clone, Copy, Default)]
struct Flux {
    left: f32,
    right: f32,
    down: f32,
    up: f32,
}

pub fn apply(encoded: &mut [i16], settings: Settings) -> Result<(), String> {
    settings.validate()?;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(settings.threads.min(settings.resolution.min(257)))
        .build()
        .map_err(|e| e.to_string())?;
    apply_with_pool(encoded, settings, &pool)
}

pub(crate) fn apply_with_pool(
    encoded: &mut [i16],
    settings: Settings,
    pool: &rayon::ThreadPool,
) -> Result<(), String> {
    settings.validate()?;
    if encoded.len() != settings.resolution * settings.resolution
        || encoded.iter().any(|&h| !(0..=32766).contains(&h))
    {
        return Err("invalid encoded erosion height grid".into());
    }
    let resolution = settings.resolution.min(257);
    let cells = resolution * resolution;
    let original: Vec<f32> = (0..cells)
        .map(|index| {
            let x = (index % resolution) as f32 * (settings.resolution as f32 - 1.0)
                / (resolution as f32 - 1.0);
            let z = (index / resolution) as f32 * (settings.resolution as f32 - 1.0)
                / (resolution as f32 - 1.0);
            bilinear(settings.resolution, x, z, |i| decode(encoded[i]))
        })
        .collect();
    let mut state: Vec<Cell> = original
        .iter()
        .map(|&height| Cell {
            height,
            ..Cell::default()
        })
        .collect();
    let mut next = vec![Cell::default(); cells];
    let mut flux = vec![Flux::default(); cells];
    let cell_area = settings.world_size as f32 / resolution as f32 * settings.world_size as f32
        / resolution as f32;
    // A request-local pool respects the queue's allowance and survives all 512 iterations.
    pool.install(|| {
        for _ in 0..settings.iterations {
            state.par_iter_mut().enumerate().for_each(|(i, cell)| {
                if original[i] <= 0.0 {
                    cell.water = (0.0 - cell.height).max(0.0);
                }
                cell.water += 0.04 * DT;
            });
            flux.par_iter_mut().enumerate().for_each(|(i, value)| {
                if boundary(i, resolution) {
                    *value = Flux::default();
                    return;
                }
                let surface = state[i].height + state[i].water;
                value.left = (value.left
                    + DT * 10.0 * (surface - state[i - 1].height - state[i - 1].water))
                    .max(0.0);
                value.right = (value.right
                    + DT * 10.0 * (surface - state[i + 1].height - state[i + 1].water))
                    .max(0.0);
                value.down = (value.down
                    + DT * 10.0
                        * (surface - state[i + resolution].height - state[i + resolution].water))
                    .max(0.0);
                value.up = (value.up
                    + DT * 10.0
                        * (surface - state[i - resolution].height - state[i - resolution].water))
                    .max(0.0);
                let scale = (state[i].water * cell_area
                    / ((value.left + value.right + value.down + value.up) * DT).max(0.000001))
                .min(1.0);
                value.left *= scale;
                value.right *= scale;
                value.down *= scale;
                value.up *= scale;
            });
            state.par_iter_mut().enumerate().for_each(|(i, cell)| {
                if boundary(i, resolution) {
                    cell.velocity_x = 0.0;
                    cell.velocity_z = 0.0;
                    return;
                }
                let f = flux[i];
                let outgoing = f.left + f.right + f.down + f.up;
                let incoming_left = flux[i - 1].right;
                let incoming_right = flux[i + 1].left;
                let incoming_down = flux[i + resolution].up;
                let incoming_up = flux[i - resolution].down;
                cell.water += DT
                    * (incoming_left + incoming_right + incoming_down + incoming_up - outgoing)
                    / cell_area;
                cell.velocity_x += 0.5 * DT * (incoming_left - f.left + f.right - incoming_right);
                cell.velocity_z += 0.5 * DT * (incoming_down - f.down + f.up - incoming_up);
            });
            next.par_iter_mut().enumerate().for_each(|(i, output)| {
                let cell = state[i];
                let angle = if boundary(i, resolution) {
                    0.0
                } else {
                    let dx = (state[i + 1].height - state[i - 1].height) * -0.5;
                    let dz = (state[i + resolution].height - state[i - resolution].height) * -0.5;
                    (dx * dx + dz * dz).sqrt().atan2(1000.0 / resolution as f32)
                };
                let capacity = 0.0015
                    * angle.max(0.01047198).sin()
                    * (cell.velocity_x * cell.velocity_x + cell.velocity_z * cell.velocity_z)
                        .sqrt();
                let t = (cell.water / 10.0).clamp(0.0, 1.0);
                let water_factor = 1.0 - t * t * (3.0 - 2.0 * t);
                let mut change = (DT * 0.15 * (capacity - cell.sediment) * water_factor).max(0.0);
                let minimum = if original[i] <= 0.0 { original[i] } else { 0.0 };
                let updated = (cell.height - change).max(minimum);
                change = updated - (cell.height - change);
                *output = Cell {
                    height: updated,
                    water: cell.water + change,
                    sediment: cell.sediment + change,
                    ..cell
                };
            });
            state.par_iter_mut().enumerate().for_each(|(i, cell)| {
                let updated = next[i];
                let x = i % resolution;
                let z = i / resolution;
                let source_x = ((x as f32 - DT * updated.velocity_x) as i32)
                    .clamp(0, resolution as i32 - 1) as usize;
                let source_z = ((z as f32 - DT * updated.velocity_z) as i32)
                    .clamp(0, resolution as i32 - 1) as usize;
                *cell = Cell {
                    sediment: next[source_z * resolution + source_x].sediment,
                    water: updated.water * (1.0 - 0.015 * DT),
                    ..updated
                };
            });
        }
        encoded.par_iter_mut().enumerate().for_each(|(i, height)| {
            let x = (i % settings.resolution) as f32 * (resolution as f32 - 1.0)
                / (settings.resolution as f32 - 1.0);
            let z = (i / settings.resolution) as f32 * (resolution as f32 - 1.0)
                / (settings.resolution as f32 - 1.0);
            let delta = bilinear(resolution, x, z, |j| state[j].height)
                - bilinear(resolution, x, z, |j| original[j]);
            let metres = (decode(*height) + delta).clamp(-500.0, 500.0);
            *height = encode(metres);
        });
    });
    Ok(())
}

fn boundary(index: usize, resolution: usize) -> bool {
    let x = index % resolution;
    let z = index / resolution;
    x == 0 || z == 0 || x == resolution - 1 || z == resolution - 1
}

pub const fn decode(value: i16) -> f32 {
    -500.0 + value as f32 / 32766.0 * 1000.0
}

pub(crate) fn encode(metres: f32) -> i16 {
    ((metres - -500.0) / 1000.0 * 32766.0 + 0.5) as i16
}

fn bilinear(resolution: usize, x: f32, z: f32, read: impl Fn(usize) -> f32) -> f32 {
    let x0 = (x as usize).min(resolution - 1);
    let z0 = (z as usize).min(resolution - 1);
    let x1 = (x0 + 1).min(resolution - 1);
    let z1 = (z0 + 1).min(resolution - 1);
    let top = read(z0 * resolution + x0)
        + (read(z0 * resolution + x1) - read(z0 * resolution + x0)) * (x - x0 as f32);
    let bottom = read(z1 * resolution + x0)
        + (read(z1 * resolution + x1) - read(z1 * resolution + x0)) * (x - x0 as f32);
    top + (bottom - top) * (z - z0 as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings {
            resolution: 33,
            world_size: 1000,
            threads: 1,
            iterations: 32,
        }
    }

    #[test]
    fn flat_ocean_and_flat_land_are_preserved() {
        for value in [0, 14745, 16383, 20000, 32766] {
            let mut heights = vec![value; 33 * 33];
            apply(&mut heights, settings()).unwrap();
            assert!(heights.iter().all(|&h| h == value));
        }
    }

    #[test]
    fn parallel_workers_produce_identical_height_bytes() {
        let mut one: Vec<i16> = (0..33 * 33).map(|i| 16000 + (i % 997) as i16).collect();
        let mut many = one.clone();
        apply(&mut one, settings()).unwrap();
        apply(
            &mut many,
            Settings {
                threads: 4,
                ..settings()
            },
        )
        .unwrap();
        assert_eq!(one, many);
    }

    #[test]
    fn invalid_grid_and_settings_leave_input_unchanged() {
        let mut heights = vec![-1; 33 * 33];
        assert!(apply(&mut heights, settings()).is_err());
        assert!(heights.iter().all(|&h| h == -1));
        for settings in [
            Settings {
                resolution: usize::MAX,
                ..settings()
            },
            Settings {
                threads: 0,
                ..settings()
            },
            Settings {
                world_size: 0,
                ..settings()
            },
            Settings {
                iterations: 513,
                ..settings()
            },
        ] {
            assert!(apply(&mut [], settings).is_err());
        }
    }
}
