//! Ordered late material paint from retained erosion deltas and current slopes.
use crate::{
    path_surface,
    topology_query::{Index, Query},
};
use serde::{Deserialize, Serialize};

pub const VERSION: &str = "papyrus-erosion-splat-1";

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub protected_topology: u32,
    pub nearby_topology: u32,
    pub nearby_radius: f32,
    pub minimum_slope: f32,
    pub minimum_curvature: f32,
    pub curvature_scale: f32,
    pub material_threshold: f32,
    /// Snow, sand, soil, stones and gravel indices from the source channel map.
    pub channels: [usize; 5],
    pub soil_power: f32,
    pub stone_power: f32,
    pub snow_gravel_power: f32,
    pub ground_gravel_power: f32,
    pub gradient_power: f32,
    pub soil_gradient_power: f32,
}

pub struct Input<'a> {
    pub height_resolution: usize,
    pub height_delta: &'a [f32],
    pub slopes_degrees: &'a [f32],
    pub topology_resolution: usize,
    pub topology: &'a [i32],
    pub splat_resolution: usize,
    pub splat: &'a [u8],
    pub terrain_one_over_size: f32,
}

#[derive(Clone, Copy)]
pub struct Limits {
    pub maximum_bytes: usize,
    pub maximum_work: usize,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub policy: Policy,
    pub samples: usize,
    pub boundary_samples: usize,
    pub protected_samples: usize,
    pub slope_rejections: usize,
    pub curvature_rejections: usize,
    pub snow_sand_samples: usize,
    pub ground_samples: usize,
    pub material_operations: usize,
    pub changed_cells: usize,
    pub work: usize,
    pub peak_buffer_bytes: usize,
}

pub struct Output {
    pub splat: Vec<u8>,
    pub report: Report,
}

impl Policy {
    pub fn validate(self) -> Result<(), String> {
        if self
            .channels
            .iter()
            .enumerate()
            .any(|(i, c)| *c >= 8 || self.channels[..i].contains(c))
            || !self.nearby_radius.is_finite()
            || !(0. ..=65536.).contains(&self.nearby_radius)
            || !self.minimum_slope.is_finite()
            || !(0. ..=180.).contains(&self.minimum_slope)
            || !self.minimum_curvature.is_finite()
            || !(0. ..=65536.).contains(&self.minimum_curvature)
            || !self.curvature_scale.is_finite()
            || !(0. ..=1024.).contains(&self.curvature_scale)
            || !self.material_threshold.is_finite()
            || !(0. ..=1.).contains(&self.material_threshold)
            || [
                self.soil_power,
                self.stone_power,
                self.snow_gravel_power,
                self.ground_gravel_power,
            ]
            .iter()
            .any(|v| !v.is_finite() || !(0. ..=16.).contains(v))
            || [self.gradient_power, self.soil_gradient_power]
                .iter()
                .any(|v| !v.is_finite() || !(0. ..=4.).contains(v))
        {
            return Err("Invalid erosion material policy".into());
        }
        Ok(())
    }
}

fn charge(work: &mut usize, amount: usize, limit: usize) -> Result<(), String> {
    *work = work
        .checked_add(amount)
        .filter(|v| *v <= limit)
        .ok_or("Erosion material painting exhausted its work allowance")?;
    Ok(())
}

fn index(coordinate: f32, n: usize) -> usize {
    ((coordinate * n as f32) as isize).clamp(0, n as isize - 1) as usize
}

#[derive(Clone, Copy)]
struct Axis {
    uv: f32,
    topology: usize,
    nearby: [usize; 2],
    splat: usize,
}

fn sample(splat: &[u8], n: usize, channel: usize, uv: [f32; 2]) -> f32 {
    let [x, z] = uv.map(|v| v * (n - 1) as f32);
    let a = (x as isize).clamp(0, n as isize - 1) as usize;
    let b = (z as isize).clamp(0, n as isize - 1) as usize;
    let c = (a + 1).min(n - 1);
    let d = (b + 1).min(n - 1);
    let at = |x, z| splat[(channel * n + z) * n + x] as f32 * path_surface::BYTE_TO_FLOAT;
    let lerp = |a, b, t: f32| a + (b - a) * t.clamp(0., 1.);
    lerp(
        lerp(at(a, b), at(c, b), x - a as f32),
        lerp(at(a, d), at(c, d), x - a as f32),
        z - b as f32,
    )
}

fn concavity(data: &[f32], n: usize, i: usize) -> (f32, f32) {
    let [left, right, down, up] = [data[i - 1], data[i + 1], data[i + n], data[i - n]];
    let sides = ((left + right) + down) + up;
    let corners = ((data[i - n - 1] + data[i - n + 1]) + data[i + n - 1]) + data[i + n + 1];
    let dx = right - left;
    let dz = down - up;
    (
        ((sides / 4. + corners / 4.) - data[i]).max(0.),
        ((dx * dx + dz * dz) as f64).sqrt() as f32,
    )
}

fn power(value: f32, exponent: f32) -> f32 {
    (value as f64).powf(exponent as f64) as f32
}

pub fn paint(
    input: Input<'_>,
    policy: Policy,
    limits: Limits,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    policy.validate()?;
    let hr = input.height_resolution;
    let tr = input.topology_resolution;
    let sr = input.splat_resolution;
    if !(3..=4097).contains(&hr)
        || !(1..=4096).contains(&tr)
        || !(1..=4096).contains(&sr)
        || input.height_delta.len() != hr * hr
        || input.slopes_degrees.len() != hr * hr
        || input.topology.len() != tr * tr
        || input.splat.len() != sr * sr * 8
        || input
            .height_delta
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 65536.)
        || input
            .slopes_degrees
            .iter()
            .any(|v| !v.is_finite() || !(0. ..=180.).contains(v))
        || !input.terrain_one_over_size.is_finite()
        || !(f32::MIN_POSITIVE..=1.).contains(&input.terrain_one_over_size)
    {
        return Err("Invalid erosion material grids or world frame".into());
    }
    let peak_bytes = input.splat.len() + Index::allocation_bytes(tr)? as usize;
    if peak_bytes > limits.maximum_bytes
        || 2 * input.splat.len() + input.topology.len() + hr * hr > limits.maximum_work
    {
        return Err("Erosion material painting exceeds its memory or work allowance".into());
    }
    if cancelled() {
        return Ok(None);
    }
    let Some(indexed) = Index::new(input.topology, tr, &cancelled)? else {
        return Ok(None);
    };
    let tiles = indexed.view(input.topology);
    let nearby = input.terrain_one_over_size * policy.nearby_radius;
    let project_axis = |coordinate: usize| {
        let uv = (coordinate as f32 - 0.5) / hr as f32;
        Axis {
            uv,
            topology: index(uv, tr),
            nearby: [index(uv - nearby, tr), index(uv + nearby, tr)],
            splat: index(uv, sr),
        }
    };
    let axis_bytes = hr * std::mem::size_of::<Axis>();
    let mut axis = Vec::new();
    if axis_bytes <= limits.maximum_bytes - peak_bytes && axis.try_reserve_exact(hr).is_ok() {
        axis.extend((0..hr).map(project_axis));
    }
    let cells = sr * sr;
    let axis_bytes = std::mem::size_of_val(axis.as_slice());
    let mut touched = Vec::<u64>::new();
    let words = cells.div_ceil(64);
    let touch_bytes = words * std::mem::size_of::<u64>();
    if touch_bytes <= limits.maximum_bytes - peak_bytes - axis_bytes
        && touched.try_reserve_exact(words).is_ok()
    {
        touched.resize(words, 0);
    }
    let at = |i| axis.get(i).copied().unwrap_or_else(|| project_axis(i));
    let mut splat = Vec::new();
    splat
        .try_reserve_exact(input.splat.len())
        .map_err(|e| format!("Erosion material allocation failed: {e}"))?;
    splat.extend_from_slice(input.splat);
    let mut report = Report {
        engine: VERSION,
        policy,
        samples: 0,
        boundary_samples: 0,
        protected_samples: 0,
        slope_rejections: 0,
        curvature_rejections: 0,
        snow_sand_samples: 0,
        ground_samples: 0,
        material_operations: 0,
        changed_cells: 0,
        work: input.topology.len() + input.splat.len(),
        peak_buffer_bytes: peak_bytes + axis_bytes + std::mem::size_of_val(touched.as_slice()),
    };
    let [snow, sand, soil, stones, gravel] = policy.channels;
    let coordinates = (0..hr).flat_map(|z| (0..hr).map(move |x| (x, z)));
    for (i, (x, z)) in coordinates.enumerate() {
        if cancelled() {
            return Ok(None);
        }
        charge(&mut report.work, 1, limits.maximum_work)?;
        report.samples += 1;
        if x == 0 || z == 0 || x == hr - 1 || z == hr - 1 {
            report.boundary_samples += 1;
            continue;
        }
        let (x, z) = (at(x), at(z));
        let uv = [x.uv, z.uv];
        charge(&mut report.work, 1, limits.maximum_work)?;
        let cell = z.topology * tr + x.topology;
        let near = Query {
            rect: [x.nearby[0], x.nearby[1], z.nearby[0], z.nearby[1]],
            center: [x.topology, z.topology],
            radius: policy.nearby_radius,
            relevant: policy.nearby_topology,
        };
        if input.topology[cell] as u32 & policy.protected_topology != 0
            || tiles
                .union(near, &mut report.work, limits.maximum_work)
                .map_err(|()| "Erosion material topology queries exhausted their work allowance")?
                != 0
        {
            report.protected_samples += 1;
            continue;
        }
        if input.slopes_degrees[i] < policy.minimum_slope {
            report.slope_rejections += 1;
            continue;
        }
        charge(&mut report.work, 9, limits.maximum_work)?;
        let (curvature, gradient) = concavity(input.height_delta, hr, i);
        if curvature < policy.minimum_curvature {
            report.curvature_rejections += 1;
            continue;
        }
        let pixel = z.splat * sr + x.splat;
        charge(&mut report.work, 8, limits.maximum_work)?;
        let frozen = sample(&splat, sr, snow, uv) > policy.material_threshold
            || sample(&splat, sr, sand, uv) > policy.material_threshold;
        let curvature = (curvature * policy.curvature_scale).clamp(0., 1.);
        if frozen {
            report.snow_sand_samples += 1;
        } else {
            report.ground_samples += 1;
        }
        let mut add = |channel: usize, amount: f32| -> Result<(), String> {
            charge(&mut report.work, 9, limits.maximum_work)?;
            report.material_operations += 1;
            let old = splat[channel * cells + pixel] as f32 * path_surface::BYTE_TO_FLOAT;
            path_surface::set_splat(
                &mut splat,
                cells,
                pixel,
                channel,
                (old + amount).clamp(0., 1.),
            );
            Ok(())
        };
        if !frozen {
            add(
                soil,
                power(curvature, policy.soil_power) * power(gradient, policy.soil_gradient_power),
            )?;
        }
        let gradient = power(gradient, policy.gradient_power);
        add(stones, power(curvature, policy.stone_power) * gradient)?;
        let gravel_power = if frozen {
            policy.snow_gravel_power
        } else {
            policy.ground_gravel_power
        };
        add(gravel, power(curvature, gravel_power) * gradient)?;
        if let Some(word) = touched.get_mut(pixel / 64) {
            *word |= 1 << (pixel % 64);
        }
    }
    let changed = |i| (0..8).any(|c| splat[c * cells + i] != input.splat[c * cells + i]);
    for start in (0..cells).step_by(65536) {
        if cancelled() {
            return Ok(None);
        }
        let end = (start + 65536).min(cells);
        // Only paint operations can change a cell. Keep charging the whole
        // logical scan, including cells with no writes or unchanged bytes.
        charge(&mut report.work, (end - start) * 8, limits.maximum_work)?;
        if touched.is_empty() {
            report.changed_cells += (start..end).filter(|&i| changed(i)).count();
        } else {
            let offset = start / 64;
            for (i, &mask) in touched[offset..end.div_ceil(64)].iter().enumerate() {
                let mut pending = mask;
                while pending != 0 {
                    let cell = (offset + i) * 64 + pending.trailing_zeros() as usize;
                    report.changed_cells += usize::from(changed(cell));
                    pending &= pending - 1;
                }
            }
        }
    }
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(Output { splat, report }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    fn policy() -> Policy {
        Policy {
            protected_topology: 0xb4990,
            nearby_topology: 2,
            nearby_radius: 8.,
            minimum_slope: 3.,
            minimum_curvature: 3.5762787e-7,
            curvature_scale: 3.,
            material_threshold: 0.25,
            channels: [1, 2, 0, 6, 7],
            soil_power: 4.,
            stone_power: 0.8,
            snow_gravel_power: 1.5,
            ground_gravel_power: 1.4,
            gradient_power: 2.,
            soil_gradient_power: 1.5,
        }
    }
    fn limits() -> Limits {
        Limits {
            maximum_bytes: 1_000_000,
            maximum_work: 1_000_000,
        }
    }
    fn input<'a>(
        delta: &'a [f32],
        slopes: &'a [f32],
        topo: &'a [i32],
        splat: &'a [u8],
    ) -> Input<'a> {
        Input {
            height_resolution: 7,
            height_delta: delta,
            slopes_degrees: slopes,
            topology_resolution: 4,
            topology: topo,
            splat_resolution: 1,
            splat,
            terrain_one_over_size: 1. / 1000.,
        }
    }
    #[test]
    fn live_material_reads_change_the_later_snow_branch_in_source_order() {
        let delta: Vec<_> = (0..49).map(|i| i as f32 * 0.2).collect();
        let slopes = vec![45.; 49];
        let topo = vec![0; 16];
        let mut splat = vec![0; 8];
        splat[1] = 255;
        let output = paint(
            input(&delta, &slopes, &topo, &splat),
            policy(),
            limits(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(output.report.snow_sand_samples, 1);
        assert_eq!(output.report.ground_samples, 24);
        assert_eq!(output.report.material_operations, 74);
        assert_eq!(output.report.changed_cells, 1);
        assert_eq!(output.report.boundary_samples, 24);
        assert_eq!(output.splat, [0, 0, 0, 0, 0, 0, 0, 255]);
        assert_eq!(splat[1], 255);

        let tight = Limits {
            maximum_bytes: splat.len() + Index::allocation_bytes(4).unwrap() as usize,
            ..limits()
        };
        let uncached = paint(
            input(&delta, &slopes, &topo, &splat),
            policy(),
            tight,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(uncached.splat, output.splat);
        assert_eq!(uncached.report.work, output.report.work);
        assert_eq!(uncached.report.peak_buffer_bytes, tight.maximum_bytes);
        assert!(output.report.peak_buffer_bytes > uncached.report.peak_buffer_bytes);
    }
    #[test]
    fn zero_delta_low_slopes_and_protected_topology_preserve_source_materials() {
        let delta: Vec<_> = (0..49).map(|i| i as f32 * 0.2).collect();
        let slopes = vec![45.; 49];
        let topo = vec![0; 16];
        let splat = vec![100; 8];
        let zero = paint(
            input(&[0.; 49], &slopes, &topo, &splat),
            policy(),
            limits(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(zero.splat, splat);
        assert_eq!(zero.report.curvature_rejections, 25);
        let flat = paint(
            input(&delta, &[0.; 49], &topo, &splat),
            policy(),
            limits(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(flat.splat, splat);
        assert_eq!(flat.report.slope_rejections, 25);
        for mask in [0x10, 2] {
            let protected = paint(
                input(&delta, &slopes, &[mask; 16], &splat),
                policy(),
                limits(),
                || false,
            )
            .unwrap()
            .unwrap();
            assert_eq!(protected.splat, splat);
            assert_eq!(protected.report.protected_samples, 25);
        }
    }

    #[test]
    fn touched_cells_match_a_full_scan_across_bitmap_words_and_work_limits() {
        let hr = 13;
        let sr = 9;
        let delta: Vec<_> = (0..hr * hr)
            .map(|i| ((i * 17 % 29) as f32 - 14.) * 0.08)
            .collect();
        let slopes = vec![45.; hr * hr];
        let topology = vec![0; 16];
        let splat = vec![24; sr * sr * 8];
        let paint_with = |maximum_bytes, maximum_work| {
            paint(
                Input {
                    height_resolution: hr,
                    splat_resolution: sr,
                    ..input(&delta, &slopes, &topology, &splat)
                },
                policy(),
                Limits {
                    maximum_bytes,
                    maximum_work,
                },
                || false,
            )
        };
        let tight = splat.len() + Index::allocation_bytes(4).unwrap() as usize;
        let expected = paint_with(tight, limits().maximum_work).unwrap().unwrap();
        let work = expected.report.work;
        let actual = paint_with(limits().maximum_bytes, work).unwrap().unwrap();
        assert_eq!(actual.splat, expected.splat);
        assert_eq!(actual.report.work, work);
        assert_eq!(actual.report.changed_cells, expected.report.changed_cells);
        assert!((64..sr * sr).any(|i| (0..8).any(|c| {
            let index = c * sr * sr + i;
            actual.splat[index] != splat[index]
        })));
        assert!(paint_with(tight, work - 1).is_err());
        assert!(paint_with(limits().maximum_bytes, work - 1).is_err());
    }
    #[test]
    fn arbitrary_channel_maps_are_resolved_without_normalizing_raw_source_bytes() {
        let delta: Vec<_> = (0..49).map(|i| i as f32 * 0.01).collect();
        let slopes = vec![45.; 49];
        let topo = vec![0; 16];
        let splat = vec![100; 8];
        let original = paint(
            input(&delta, &slopes, &topo, &splat),
            policy(),
            limits(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(original.splat.iter().map(|v| *v as usize).sum::<usize>() > 255);
        assert_eq!(original.report.changed_cells, 1);
        let permutation = [3, 5, 4, 6, 1, 7, 0, 2];
        let mut p = policy();
        p.channels = p.channels.map(|c| permutation[c]);
        let other = paint(input(&delta, &slopes, &topo, &splat), p, limits(), || false)
            .unwrap()
            .unwrap();
        for (c, &mapped) in permutation.iter().enumerate() {
            assert_eq!(original.splat[c], other.splat[mapped]);
        }
    }
    #[test]
    fn cancellation_and_resource_failures_discard_private_paint() {
        let delta: Vec<_> = (0..49).map(|i| i as f32 * 0.01).collect();
        let slopes = vec![45.; 49];
        let topo = vec![0; 16];
        let splat = vec![100; 8];
        let calls = Cell::new(0);
        assert!(
            paint(
                input(&delta, &slopes, &topo, &splat),
                policy(),
                limits(),
                || {
                    calls.set(calls.get() + 1);
                    calls.get() > 20
                }
            )
            .unwrap()
            .is_none()
        );
        let mut small = limits();
        small.maximum_bytes = 11;
        assert!(
            paint(
                input(&delta, &slopes, &topo, &splat),
                policy(),
                small,
                || false
            )
            .is_err()
        );
        small = limits();
        small.maximum_work = 100;
        assert!(
            paint(
                input(&delta, &slopes, &topo, &splat),
                policy(),
                small,
                || false
            )
            .is_err()
        );
        let mut bad = policy();
        bad.channels[1] = bad.channels[0];
        assert!(
            paint(input(&delta, &slopes, &topo, &splat), bad, limits(), || {
                false
            })
            .is_err()
        );
        assert!(
            paint(
                input(&delta[..48], &slopes, &topo, &splat),
                policy(),
                limits(),
                || false
            )
            .is_err()
        );
        assert_eq!(splat, vec![100; 8]);
    }

    fn check_reference(bytes: &[u8]) {
        #[derive(Deserialize)]
        struct Capture {
            format: String,
            schema: u32,
            channels: [usize; 5],
            rows: Vec<Row>,
        }
        #[derive(Deserialize)]
        struct Row {
            height_resolution: usize,
            splat_resolution: usize,
            topology_resolution: usize,
            terrain_one_over_size: f32,
            height_delta: Vec<f32>,
            slopes_degrees: Vec<f32>,
            topology: Vec<i32>,
            splat: Vec<u8>,
            result: Vec<u8>,
        }
        assert!(bytes.len() <= 1024 * 1024);
        let capture: Capture = serde_json::from_slice(bytes).unwrap();
        assert_eq!(capture.format, "papyrus-owned-erosion-splat-reference");
        assert_eq!(capture.schema, 1);
        assert_eq!(capture.rows.len(), 48);
        let mut p = policy();
        p.channels = capture.channels;
        for (i, row) in capture.rows.iter().enumerate() {
            let output = paint(
                Input {
                    height_resolution: row.height_resolution,
                    height_delta: &row.height_delta,
                    slopes_degrees: &row.slopes_degrees,
                    topology_resolution: row.topology_resolution,
                    topology: &row.topology,
                    splat_resolution: row.splat_resolution,
                    splat: &row.splat,
                    terrain_one_over_size: row.terrain_one_over_size,
                },
                p,
                limits(),
                || false,
            )
            .unwrap()
            .unwrap();
            assert_eq!(output.splat, row.result, "managed reference case {i}");
        }
    }

    #[test]
    fn quantized_materials_match_the_managed_numeric_fixture() {
        check_reference(include_bytes!(
            "../tests/fixtures/erosion-splat-bd19c4ac.json"
        ));
    }

    #[test]
    #[ignore = "requires the owned managed erosion material reference capture"]
    fn owned_material_job_matches_all_quantized_channels() {
        let path =
            std::env::var_os("MAPPER_EROSION_SPLAT_REFERENCE").expect("owned reference path");
        assert!(std::fs::metadata(&path).unwrap().len() <= 1024 * 1024);
        check_reference(&std::fs::read(path).unwrap());
    }
}
