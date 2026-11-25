//! Bounded hydraulic job processor with retained, unquantized erosion deltas.
//! Callers supply installation-derived coefficients and per-vertex lower bounds.
//! Boundary slopes use explicit zero angles for deterministic safe execution.
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

pub const VERSION: &str = "papyrus-erosion-grid-1";

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub iterations: usize,
    pub time_step: f32,
    pub water_fill_rate: f32,
    pub evaporation_rate: f32,
    pub gravity: f32,
    pub pipe_area: f32,
    pub pipe_length: f32,
    pub cell_area: f32,
    pub normal_y: f32,
    pub ocean_level: f32,
    pub minimum_angle: f32,
    pub sediment_capacity: f32,
    pub dissolve_rate: f32,
    pub depth_fade: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// Heap payload for processor-owned grid buffers. Caller inputs and the
    /// caller's worker pool are accounted for by the generation coordinator.
    pub maximum_bytes: usize,
    pub maximum_work: u64,
}

pub struct Input<'a> {
    pub resolution: usize,
    pub heights: &'a [f32],
    pub minimum_heights: &'a [f32],
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub iterations: usize,
    pub cells: usize,
    pub buffer_bytes: usize,
    pub work: u64,
    pub maximum_lowering: f32,
    pub maximum_raise: f32,
}

pub struct Output {
    pub heights: Vec<f32>,
    /// Original minus final float heights, retained before terrain quantization.
    pub height_delta: Vec<f32>,
    pub report: Report,
}

impl Policy {
    pub fn validate(self) -> Result<(), String> {
        if !(1..=512).contains(&self.iterations)
            || [
                self.time_step,
                self.pipe_area,
                self.pipe_length,
                self.cell_area,
                self.normal_y,
                self.depth_fade,
            ]
            .iter()
            .any(|v| !v.is_finite() || *v <= 0. || *v > 1_000_000.)
            || [
                self.water_fill_rate,
                self.evaporation_rate,
                self.gravity,
                self.minimum_angle,
                self.sediment_capacity,
                self.dissolve_rate,
            ]
            .iter()
            .any(|v| !v.is_finite() || *v < 0. || *v > 1000.)
            || !self.ocean_level.is_finite()
            || self.ocean_level.abs() > 65536.
            || self.minimum_angle > std::f32::consts::FRAC_PI_2
            || self.evaporation_rate * self.time_step > 1.
        {
            return Err("Invalid hydraulic erosion coefficients".into());
        }
        Ok(())
    }
}

fn validated_size(
    input: &Input<'_>,
    policy: Policy,
    limits: Limits,
) -> Result<(usize, u64), String> {
    policy.validate()?;
    let n = input.resolution;
    if !(3..=4097).contains(&n)
        || input.heights.len() != n * n
        || input.minimum_heights.len() != n * n
        || input
            .heights
            .iter()
            .zip(input.minimum_heights)
            .any(|(&h, &min)| {
                !h.is_finite()
                    || !min.is_finite()
                    || h.abs() > 65536.
                    || min.abs() > 65536.
                    || min > h
            })
    {
        return Err("Invalid hydraulic erosion input grids".into());
    }
    // Two height buffers, water, two sediment buffers, velocities, four fluxes.
    // The returned delta reuses one of the height buffers after the final pass.
    let bytes = n * n * 11 * std::mem::size_of::<f32>();
    // Every iteration accounts for six complete grid sweeps plus preparation
    // and the final height-delta sweep, including fused execution.
    let work = (n * n) as u64 * (policy.iterations as u64 * 6 + 2);
    if bytes > limits.maximum_bytes || work > limits.maximum_work {
        return Err("Hydraulic erosion exceeds its memory or work allowance".into());
    }
    Ok((bytes, work))
}

/// Runs privately and publishes only after every pass and final finite checks.
/// The supplied pool remains owned by the caller across the complete operation.
pub fn run(
    input: Input<'_>,
    policy: Policy,
    limits: Limits,
    pool: &rayon::ThreadPool,
    cancelled: impl Fn() -> bool + Sync,
    mut progress: impl FnMut(usize, usize) + Send,
) -> Result<Option<Output>, String> {
    let (bytes, work) = validated_size(&input, policy, limits)?;
    if cancelled() {
        return Ok(None);
    }
    let n = input.resolution;
    let cells = n * n;
    let mut heights = buffer(cells, 0.)?;
    heights.copy_from_slice(input.heights);
    let mut next_heights = buffer(cells, 0.)?;
    next_heights.copy_from_slice(input.heights);
    let mut water = buffer(cells, 0.)?;
    let mut sediment = buffer(cells, 0.)?;
    let mut next_sediment = buffer(cells, 0.)?;
    let mut velocity = buffer(cells, [0.; 2])?;
    let mut flux = buffer(cells, [0.; 4])?;
    let fill = policy.water_fill_rate * policy.time_step;
    let evaporation = 1. - policy.evaporation_rate * policy.time_step;
    let boundary_capacity = capacity_per_speed(0., policy);
    progress(0, policy.iterations);
    let complete = pool.install(|| {
        for iteration in 0..policy.iterations {
            if cancelled() {
                return false;
            }
            if iteration == 0 {
                water
                    .par_iter_mut()
                    .zip(input.heights)
                    .for_each(|(water, &height)| {
                        if height <= policy.ocean_level {
                            *water = policy.ocean_level - height;
                        }
                        *water += fill;
                    });
            }
            // The outer row and column fluxes stay at their initialized zero values.
            flux[n..cells - n]
                .par_chunks_exact_mut(n)
                .enumerate()
                .for_each(|(row, flux)| {
                    let first = (row + 1) * n + 1;
                    let surface_at = |i: usize| heights[i] + water[i];
                    // Adjacent cells share the last two surface samples.
                    let mut left = surface_at(first - 1);
                    let mut surface = surface_at(first);
                    for (column, value) in flux[1..n - 1].iter_mut().enumerate() {
                        let i = first + column;
                        let water_height = water[i];
                        let right = surface_at(i + 1);
                        let neighbours = [left, right, surface_at(i + n), surface_at(i - n)];
                        for (value, neighbouring_surface) in value.iter_mut().zip(neighbours) {
                            *value = (*value
                                + policy.time_step
                                    * policy.pipe_area
                                    * (policy.gravity * (surface - neighbouring_surface)
                                        / policy.pipe_length))
                                .max(0.);
                        }
                        left = surface;
                        surface = right;
                        let sum = ((value[0] + value[1]) + value[2]) + value[3];
                        let factor =
                            (water_height * policy.cell_area / (sum * policy.time_step)).min(1.);
                        for direction in value {
                            *direction *= factor;
                        }
                    }
                });
            if cancelled() {
                return false;
            }
            let refill = iteration + 1 < policy.iterations;
            next_heights
                .par_chunks_exact_mut(n)
                .zip(sediment.par_chunks_exact_mut(n))
                .zip(water.par_chunks_exact_mut(n))
                .zip(velocity.par_chunks_exact_mut(n))
                .enumerate()
                .with_min_len(4)
                .for_each_init(
                    SlopeCache::default,
                    |slopes, (row, (((next, sediment), water), velocity))| {
                        let first = row * n;
                        let neighbours = (row > 0 && row < n - 1).then(|| {
                            (
                                &heights[first - n..first],
                                &heights[first + n..first + n * 2],
                            )
                        });
                        for (column, (((next, sediment), water), velocity)) in
                            next.iter_mut().zip(sediment).zip(water).zip(velocity).enumerate()
                        {
                            let i = first + column;
                            // Erosion reads only this cell's updated water and
                            // velocity. Its height neighbors still come from the
                            // previous grid, so transport can run in the same pass.
                            if neighbours.is_some() && column > 0 && column < n - 1 {
                                let own = flux[i];
                                let incoming = [
                                    flux[i - 1][1],
                                    flux[i + 1][0],
                                    flux[i + n][3],
                                    flux[i - n][2],
                                ];
                                let inflow = ((incoming[0] + incoming[1]) + incoming[2]) + incoming[3];
                                let outflow = ((own[0] + own[1]) + own[2]) + own[3];
                                *water += policy.time_step * (inflow - outflow) * (1. / policy.cell_area);
                                velocity[0] += ((incoming[0] - own[0] + own[1] - incoming[1]) * 0.5)
                                    * policy.time_step;
                                velocity[1] += ((incoming[2] - own[2] + own[3] - incoming[3]) * 0.5)
                                    * policy.time_step;
                            }
                            let height = heights[i];
                            // The deep-water fade makes the returned sediment zero.
                            // Heights already respect the floor and sediment starts
                            // at zero with only nonnegative additions and copies.
                            // Keep the ordinary arithmetic at zero height for its sign.
                            if *water >= policy.depth_fade && height != 0. {
                                *next = height;
                            } else {
                                let depth = (*water / policy.depth_fade).clamp(0., 1.);
                                let shallow = 1. - depth * depth * (3. - 2. * depth);
                                let amount = if shallow == 0. && height != 0. {
                                    0.
                                } else {
                                    let coefficient = match neighbours {
                                        Some((north, south)) if column > 0 && column < n - 1 => {
                                            let x = (north[column + 1] - north[column - 1]) * -0.5;
                                            let z = (south[column - 1] - north[column - 1]) * -0.5;
                                            slopes.capacity(x, z, policy)
                                        }
                                        _ => boundary_capacity,
                                    };
                                    let speed =
                                        ((velocity[0] * velocity[0] + velocity[1] * velocity[1])
                                            as f64)
                                            .sqrt() as f32;
                                    let capacity = coefficient * speed;
                                    let change = policy.time_step
                                        * policy.dissolve_rate
                                        * (capacity - *sediment)
                                        * shallow;
                                    (if capacity > *sediment {
                                        change
                                    } else {
                                        -change
                                    })
                                    .max(0.)
                                };
                                let lowered = height - amount;
                                *next = lowered.max(input.minimum_heights[i]);
                                let returned = *next - lowered;
                                *sediment += returned;
                                *water += returned;
                            }
                            // Sediment advection does not read water. Prepare the
                            // next iteration here, using its finalized height.
                            if refill && input.heights[i] <= policy.ocean_level {
                                *water = policy.ocean_level - *next;
                            } else {
                                *water *= evaporation;
                            }
                            if refill {
                                *water += fill;
                            }
                        }
                    },
                );
            if cancelled() {
                return false;
            }
            next_sediment
                .par_chunks_exact_mut(n)
                .zip(velocity.par_chunks_exact(n))
                .enumerate()
                .for_each(|(row, (next, velocity))| {
                    for (column, (value, velocity)) in next.iter_mut().zip(velocity).enumerate() {
                        let x = column as f32 - policy.time_step * velocity[0];
                        let z = row as f32 - policy.time_step * velocity[1];
                        let x = (x as isize).clamp(0, n as isize - 1) as usize;
                        let z = (z as isize).clamp(0, n as isize - 1) as usize;
                        *value = sediment[z * n + x];
                    }
                });
            std::mem::swap(&mut sediment, &mut next_sediment);
            std::mem::swap(&mut heights, &mut next_heights);
            // Independent workers observe cancellation between bounded sweeps.
            if cancelled() {
                return false;
            }
            progress(iteration + 1, policy.iterations);
        }
        true
    });
    if !complete || cancelled() {
        return Ok(None);
    }
    let mut maximum_lowering = 0f32;
    let mut maximum_raise = 0f32;
    for (i, delta) in next_heights.iter_mut().enumerate() {
        if i % 65536 == 0 && cancelled() {
            return Ok(None);
        }
        if !heights[i].is_finite()
            || heights[i] < input.minimum_heights[i]
            || heights[i] > input.heights[i]
            || !water[i].is_finite()
            || !sediment[i].is_finite()
            || velocity[i].iter().any(|v| !v.is_finite())
            || flux[i].iter().any(|v| !v.is_finite())
        {
            return Err("Hydraulic erosion produced an invalid simulation state".into());
        }
        *delta = input.heights[i] - heights[i];
        maximum_lowering = maximum_lowering.max(*delta);
        maximum_raise = maximum_raise.max(-*delta);
    }
    Ok(Some(Output {
        heights,
        height_delta: next_heights,
        report: Report {
            engine: VERSION,
            iterations: policy.iterations,
            cells,
            buffer_bytes: bytes,
            work,
            maximum_lowering,
            maximum_raise,
        },
    }))
}

fn buffer<T: Copy>(cells: usize, value: T) -> Result<Vec<T>, String> {
    let mut data = Vec::new();
    data.try_reserve_exact(cells)
        .map_err(|error| format!("Erosion buffer allocation failed: {error}"))?;
    data.resize(cells, value);
    Ok(data)
}

struct SlopeCache([Option<(u64, f32)>; 256]);

impl Default for SlopeCache {
    fn default() -> Self {
        Self([None; 256])
    }
}

impl SlopeCache {
    fn capacity(&mut self, x: f32, z: f32, policy: Policy) -> f32 {
        // Only identical float inputs skip the trigonometry. The policy is
        // constant across the sweep and each Rayon partition owns its table.
        let key = u64::from(x.to_bits()) << 32 | u64::from(z.to_bits());
        let mixed = key.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        let entry = &mut self.0[(mixed >> 56) as usize];
        if let Some((cached, coefficient)) = *entry
            && cached == key
        {
            return coefficient;
        }
        let coefficient = capacity_per_speed(angle(x, z, policy.normal_y), policy);
        *entry = Some((key, coefficient));
        coefficient
    }
}

fn capacity_per_speed(angle: f32, policy: Policy) -> f32 {
    policy.sediment_capacity * (angle.max(policy.minimum_angle) as f64).sin() as f32
}

fn angle(x: f32, z: f32, normal_y: f32) -> f32 {
    let inverse_length = 1. / ((x * x + normal_y * normal_y + z * z) as f64).sqrt() as f32;
    let normal = [
        x * inverse_length,
        normal_y * inverse_length,
        z * inverse_length,
    ];
    let square = normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2];
    let second_length = (square as f64).sqrt() as f32;
    ((normal[1] / second_length / square).clamp(-1., 1.) as f64).acos() as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn policy() -> Policy {
        Policy {
            iterations: 32,
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
        }
    }
    fn limits() -> Limits {
        Limits {
            maximum_bytes: 16 * 1024 * 1024,
            maximum_work: 10_000_000,
        }
    }
    fn pool(threads: usize) -> rayon::ThreadPool {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap()
    }
    fn input<'a>(resolution: usize, heights: &'a [f32], minimum: &'a [f32]) -> Input<'a> {
        Input {
            resolution,
            heights,
            minimum_heights: minimum,
        }
    }
    #[test]
    fn flat_and_protected_grids_retain_original_heights_and_exact_progress() {
        let pool = pool(2);
        let h = vec![10.; 81];
        for iterations in [1, 32, 512] {
            let mut steps = Vec::new();
            let mut p = policy();
            p.iterations = iterations;
            let out = run(
                input(9, &h, &h),
                p,
                limits(),
                &pool,
                || false,
                |step, total| steps.push((step, total)),
            )
            .unwrap()
            .unwrap();
            assert_eq!(out.heights, h);
            assert!(out.height_delta.iter().all(|v| *v == 0.));
            assert_eq!(
                steps,
                (0..=iterations)
                    .map(|i| (i, iterations))
                    .collect::<Vec<_>>()
            );
            assert_eq!(out.report.buffer_bytes, 81 * 44);
            assert_eq!(out.report.work, 81 * (iterations as u64 * 6 + 2));
        }
    }
    #[test]
    fn float_deltas_are_retained_and_worker_count_does_not_change_results() {
        let n = 17;
        let h: Vec<_> = (0..n * n)
            .map(|i| 10. + (i % n) as f32 * 0.7 + ((i / n) % 4) as f32 * 0.3)
            .collect();
        let lower: Vec<_> = h
            .iter()
            .enumerate()
            .map(|(i, h)| if i % 7 == 0 { *h } else { *h - 0.02 })
            .collect();
        for iterations in [32, 512] {
            let mut p = policy();
            p.iterations = iterations;
            let a = run(
                input(n, &h, &lower),
                p,
                limits(),
                &pool(1),
                || false,
                |_, _| {},
            )
            .unwrap()
            .unwrap();
            let b = run(
                input(n, &h, &lower),
                p,
                limits(),
                &pool(3),
                || false,
                |_, _| {},
            )
            .unwrap()
            .unwrap();
            assert_eq!(a.heights, b.heights);
            assert_eq!(a.height_delta, b.height_delta);
            assert!(a.height_delta.iter().any(|v| *v > 0.));
            for (i, &value) in a.heights.iter().enumerate() {
                assert!(value >= lower[i] && value <= h[i]);
                assert_eq!(a.height_delta[i], h[i] - value);
                if i % 7 == 0 {
                    assert_eq!(value, h[i]);
                }
            }
            assert!(a.report.maximum_lowering <= 0.021);
            assert_eq!(a.report.maximum_raise, 0.);
        }
    }
    #[test]
    fn cancellation_leaves_borrowed_inputs_unchanged_and_reports_completed_iterations() {
        let h: Vec<_> = (0..81).map(|i| i as f32 * 0.1).collect();
        let before = h.clone();
        let lower: Vec<_> = h.iter().map(|h| h - 1.).collect();
        let signal = AtomicBool::new(false);
        let mut steps = Vec::new();
        let result = run(
            input(9, &h, &lower),
            policy(),
            limits(),
            &pool(2),
            || signal.load(Ordering::Relaxed),
            |step, _| {
                steps.push(step);
                if step == 2 {
                    signal.store(true, Ordering::Relaxed);
                }
            },
        )
        .unwrap();
        assert!(result.is_none());
        assert_eq!(steps, [0, 1, 2]);
        assert_eq!(h, before);
    }
    #[test]
    fn invalid_grids_coefficients_and_allowances_fail_before_progress() {
        assert!(buffer(usize::MAX, 0u8).is_err());
        let h = vec![10.; 81];
        let pool = pool(1);
        let p = policy();
        let mut calls = 0;
        assert!(
            run(
                input(9, &h, &h[..80]),
                p,
                limits(),
                &pool,
                || false,
                |_, _| calls += 1
            )
            .is_err()
        );
        let mut bad = p;
        bad.evaporation_rate = 1000.;
        assert!(
            run(
                input(9, &h, &h),
                bad,
                limits(),
                &pool,
                || false,
                |_, _| calls += 1
            )
            .is_err()
        );
        let mut invalid = h.clone();
        invalid[40] = f32::NAN;
        assert!(
            run(
                input(9, &invalid, &h),
                p,
                limits(),
                &pool,
                || false,
                |_, _| calls += 1
            )
            .is_err()
        );
        let mut bound = limits();
        bound.maximum_bytes = 81 * 44 - 1;
        assert!(
            run(
                input(9, &h, &h),
                p,
                bound,
                &pool,
                || false,
                |_, _| calls += 1
            )
            .is_err()
        );
        bound = limits();
        bound.maximum_work = 81 * (32 * 6 + 2) - 1;
        assert!(
            run(
                input(9, &h, &h),
                p,
                bound,
                &pool,
                || false,
                |_, _| calls += 1
            )
            .is_err()
        );
        assert_eq!(calls, 0);
    }
    #[test]
    fn dry_zero_flux_does_not_introduce_nan_and_invalid_arithmetic_is_not_published() {
        let h = vec![0.; 25];
        let mut p = policy();
        p.water_fill_rate = 0.;
        p.ocean_level = -10.;
        let result = run(input(5, &h, &h), p, limits(), &pool(2), || false, |_, _| {})
            .unwrap()
            .unwrap();
        assert_eq!(result.heights, h);
        p.cell_area = f32::MIN_POSITIVE;
        p.pipe_length = f32::MIN_POSITIVE;
        let h: Vec<_> = (0..25).map(|i| i as f32).collect();
        let lower: Vec<_> = h.iter().map(|h| h - 1.).collect();
        p.water_fill_rate = 0.04;
        assert!(
            run(
                input(5, &h, &lower),
                p,
                limits(),
                &pool(2),
                || false,
                |_, _| {}
            )
            .is_err()
        );
    }

    #[test]
    #[cfg(windows)]
    fn scalar_reference_fixtures_match_all_float_heights_and_retained_deltas() {
        use sha2::{Digest, Sha256};
        // Independently executed scalar C# reference, including fractional
        // terrain, initially submerged cells and alternating protected floors.
        let n = 17;
        let heights: Vec<_> = (0..n * n)
            .map(|i| {
                (i % n - 4) as f32 * 0.07 + ((i / n) % 4) as f32 * 0.03 + (i % 13) as f32 * 0.001
            })
            .collect();
        let minimum: Vec<_> = heights
            .iter()
            .enumerate()
            .map(|(i, h)| if i % 7 == 0 { *h } else { *h - 0.0003 })
            .collect();
        let cases = [
            (
                32,
                "4cb838435611022e47f75dc1c25f669db0858595162289dc2c37c5edf39de27a",
                "73391d372f03eec4005c5dab76dbc6902ea3a0e83628b5eb6f3c62f0317caa31",
            ),
            (
                512,
                "91d3f0003c85e1120807a7280eb2973a73c374f547228a044f4a806336832ee9",
                "93e9ebfeeb6441c02f29f0f5a4d24fc305b790044cc2cf7b4756b72f1b1c0242",
            ),
        ];
        let pool = pool(2);
        for (iterations, height_hash, delta_hash) in cases {
            let mut p = policy();
            p.iterations = iterations;
            let output = run(
                input(n as usize, &heights, &minimum),
                p,
                limits(),
                &pool,
                || false,
                |_, _| {},
            )
            .unwrap()
            .unwrap();
            let hash = |values: &[f32]| {
                let mut digest = Sha256::new();
                for value in values {
                    digest.update(value.to_le_bytes());
                }
                format!("{:x}", digest.finalize())
            };
            assert_eq!(hash(&output.heights), height_hash);
            assert_eq!(hash(&output.height_delta), delta_hash);
        }
    }
}
