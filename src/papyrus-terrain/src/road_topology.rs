//! Source-bound post-road topology. Private output keeps cancellation atomic.
use crate::{
    native_layers::{Layers, dimensions},
    rail_costmap::sample_height_slope,
};
use serde::{Deserialize, Serialize};

pub const VERSION: &str = "papyrus-road-topology-1";
pub const NATIVE_VERSION: &str = "papyrus-road-topology-native-normals-1";
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum NormalSampling {
    #[serde(rename = "papyrus-native-terrain-normal-1")]
    Native,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub source_mask: i32,
    pub eligible_mask: i32,
    pub roadside_flag: i32,
    pub cliff_flag: i32,
    pub radius_cells: usize,
    pub cliff_slope_degrees: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal_sampling: Option<NormalSampling>,
}
impl Policy {
    pub fn validate(self) -> Result<(), String> {
        if self.source_mask <= 0
            || self.eligible_mask <= 0
            || (self.roadside_flag as u32).count_ones() != 1
            || (self.cliff_flag as u32).count_ones() != 1
            || self.roadside_flag == self.cliff_flag
            || self.source_mask & self.roadside_flag == 0
            || self.source_mask & self.cliff_flag != 0
            || !(1..=64).contains(&self.radius_cells)
            || !self.cliff_slope_degrees.is_finite()
            || !(1. ..89.).contains(&self.cliff_slope_degrees)
        {
            return Err("Invalid post-road topology policy".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub policy: Policy,
    pub roadside_added: usize,
    pub cliff_added: usize,
    pub affected_cells: usize,
}

/// The inspected stage runs horizontal then vertical masked dilation. The second
/// pass sees Roadside bits added by the first, including the eligibility mask.
/// A square dilation of a frozen binary road mask is therefore not equivalent.
pub fn apply(
    layers: &Layers,
    policy: Policy,
    cancelled: impl Fn() -> bool,
) -> Result<Option<(Vec<i32>, Report)>, String> {
    apply_with_topology(layers, &layers.topology, policy, cancelled)
}
/// Apply a private road topology proposal over borrowed terrain heights without
/// copying unrelated raster channels or modifying the accepted input layers.
pub fn apply_with_topology(
    layers: &Layers,
    input_topology: &[i32],
    policy: Policy,
    cancelled: impl Fn() -> bool,
) -> Result<Option<(Vec<i32>, Report)>, String> {
    apply_with_height(layers, &layers.height, input_topology, policy, cancelled)
}
/// Final road marking over a private accepted-height proposal, borrowing all
/// grids. No height raster copy or mutation of input Layers is required.
pub fn apply_with_height(
    layers: &Layers,
    height: &[i16],
    input_topology: &[i32],
    policy: Policy,
    cancelled: impl Fn() -> bool,
) -> Result<Option<(Vec<i32>, Report)>, String> {
    policy.validate()?;
    let (hr, n) = dimensions(layers.size)?;
    if layers.height_resolution != hr
        || layers.surface_resolution != n
        || height.len() != hr * hr
        || input_topology.len() != n * n
        || height.iter().any(|h| !(0..=32766).contains(h))
    {
        return Err("Invalid terrain for post-road topology".into());
    }
    if cancelled() {
        return Ok(None);
    }
    let mut topology = input_topology.to_vec();
    let mut affected = vec![false; n * n];
    const COLUMN_BAND: usize = 32;
    let mut seeds: [Vec<usize>; COLUMN_BAND] = Default::default();
    let native = policy.normal_sampling.is_some();
    // Native layer dimensions are powers of two plus one height vertex.
    // On a 2:1 grid a surface cell centre lands exactly on an odd vertex.
    let samples_vertices = native && hr - 1 == 2 * n;
    let norm_y = layers.size as f32 / 1000. / hr as f32;
    let mut report = Report {
        engine: if native { NATIVE_VERSION } else { VERSION },
        policy,
        roadside_added: 0,
        cliff_added: 0,
        affected_cells: 0,
    };
    for vertical in [false, true] {
        let band_width = if vertical { COLUMN_BAND } else { 1 };
        for first in (0..n).step_by(band_width) {
            if cancelled() {
                return Ok(None);
            }
            let end = (first + band_width).min(n);
            let band = &mut seeds[..end - first];
            for line_seeds in &mut *band {
                line_seeds.clear();
            }
            // Freeze the seed bits before marking. Columns in the same band
            // are independent, so collect them together in storage order.
            if vertical {
                for (row, cells) in topology.chunks_exact(n).enumerate() {
                    for (&bits, column_seeds) in cells[first..end].iter().zip(&mut *band) {
                        if bits & policy.source_mask != 0 {
                            column_seeds.push(row);
                        }
                    }
                }
            } else {
                band[0].extend(
                    topology[first * n..(first + 1) * n]
                        .iter()
                        .enumerate()
                        .filter_map(|(column, bits)| {
                            (bits & policy.source_mask != 0).then_some(column)
                        }),
                );
            }
            for (offset, line_seeds) in band.iter().enumerate() {
                if vertical && cancelled() {
                    return Ok(None);
                }
                let line = first + offset;
                let index = |k: usize| if vertical { k * n + line } else { line * n + k };
                let mut previous_end = 0;
                for &seed in line_seeds {
                    let start = seed.saturating_sub(policy.radius_cells).max(previous_end);
                    let end = (seed + policy.radius_cells + 1).min(n);
                    for k in start..end {
                        let i = index(k);
                        let bits = &mut topology[i];
                        if *bits & policy.eligible_mask != 0 && *bits & policy.roadside_flag == 0 {
                            *bits |= policy.roadside_flag;
                            report.roadside_added += 1;
                        }
                        if affected[i] {
                            continue;
                        }
                        affected[i] = true;
                        report.affected_cells += 1;
                        if *bits & policy.cliff_flag != 0 {
                            continue;
                        }
                        let (row, column) = if vertical { (k, line) } else { (line, k) };
                        // MarkRoadside passes row, column to the native normal query.
                        let slope = if samples_vertices {
                            crate::terrain_normal::slope_at_vertex(
                                hr,
                                norm_y,
                                height,
                                2 * row + 1,
                                2 * column + 1,
                            )?
                        } else {
                            let uv = [
                                (column as f32 + 0.5) / n as f32,
                                (row as f32 + 0.5) / n as f32,
                            ];
                            if native {
                                crate::terrain_normal::slope(
                                    layers.size,
                                    hr,
                                    height,
                                    [uv[1], uv[0]],
                                )?
                            } else {
                                sample_height_slope(layers.size as f32, hr, height, uv).1
                            }
                        };
                        if slope > policy.cliff_slope_degrees {
                            *bits |= policy.cliff_flag;
                            report.cliff_added += 1;
                        }
                    }
                    previous_end = end;
                }
            }
        }
    }
    if cancelled() {
        return Ok(None);
    }
    Ok(Some((topology, report)))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy() -> Policy {
        Policy {
            source_mask: 6144,
            eligible_mask: 49,
            roadside_flag: 4096,
            cliff_flag: 2,
            radius_cells: 6,
            cliff_slope_degrees: 40.,
            normal_sampling: None,
        }
    }
    fn terrain() -> Layers {
        let (hr, n) = dimensions(1000).unwrap();
        Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: n,
            height: vec![18000; hr * hr],
            topology: vec![1; n * n],
            biome: vec![],
            splat: vec![],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        }
    }
    #[test]
    fn native_marking_keeps_the_source_callback_row_column_order() {
        let mut t = terrain();
        let n = t.surface_resolution;
        let hr = t.height_resolution;
        let row = n / 3;
        let column = n / 2;
        let cell = row * n + column;
        t.topology.fill(0);
        t.topology[cell] = 2048 | 1;
        let x = ((row as f32 + 0.5) / n as f32 * (hr - 1) as f32) as usize;
        let z = ((column as f32 + 0.5) / n as f32 * (hr - 1) as f32) as usize;
        t.height[(z - 1) * hr + x + 1] = 18100;
        let (legacy, _) = apply(&t, policy(), || false).unwrap().unwrap();
        let native = Policy {
            normal_sampling: Some(NormalSampling::Native),
            ..policy()
        };
        let (marked, _) = apply(&t, native, || false).unwrap().unwrap();
        assert_eq!(legacy[cell] & 2, 0);
        assert_ne!(marked[cell] & 2, 0);
    }
    #[test]
    fn native_cliff_marking_preserves_frame_arithmetic_at_thresholds() {
        let mut t = terrain();
        let n = t.surface_resolution;
        let hr = t.height_resolution;
        t.topology.fill(0);
        let cell = (n / 2) * n + n / 2;
        t.topology[cell] = 2048 | 1;
        let corner = hr / 2;
        t.height[corner * hr + corner + 2] = 18050;
        let before = t.topology.clone();
        let uv = [((n / 2) as f32 + 0.5) / n as f32; 2];
        let legacy_slope = sample_height_slope(t.size as f32, hr, &t.height, uv).1;
        let native_slope = crate::terrain_normal::slope(t.size, hr, &t.height, uv).unwrap();
        assert_ne!(legacy_slope.to_bits(), native_slope.to_bits());
        let old = Policy {
            cliff_slope_degrees: legacy_slope.min(native_slope),
            ..policy()
        };
        assert!(
            serde_json::to_value(old)
                .unwrap()
                .get("normal_sampling")
                .is_none()
        );
        let (legacy, _) = apply(&t, old, || false).unwrap().unwrap();
        assert_eq!(
            legacy[cell] & 2 != 0,
            legacy_slope > old.cliff_slope_degrees
        );
        let native = Policy {
            normal_sampling: Some(NormalSampling::Native),
            ..old
        };
        let (marked, report) = apply(&t, native, || false).unwrap().unwrap();
        assert_eq!(
            marked[cell] & 2 != 0,
            native_slope > old.cliff_slope_degrees
        );
        assert_ne!(marked[cell] & 2, legacy[cell] & 2);
        assert_eq!(report.engine, NATIVE_VERSION);
        assert_eq!(t.topology, before);
        assert!(apply(&t, native, || true).unwrap().is_none());
        let mut invalid = serde_json::to_value(native).unwrap();
        invalid["normal_sampling"] = "unknown".into();
        assert!(serde_json::from_value::<Policy>(invalid).is_err());
    }
    #[test]
    fn native_marking_matches_interpolated_slopes_on_both_grid_ratios() {
        for (size, ratio) in [(1000, 2), (2800, 1)] {
            let (hr, n) = dimensions(size).unwrap();
            assert_eq!(hr - 1, ratio * n);
            let mut t = terrain();
            t.size = size;
            t.height_resolution = hr;
            t.surface_resolution = n;
            t.height = (0..hr * hr)
                .map(|i| (18000 + i.wrapping_mul(149) % 127) as i16)
                .collect();
            t.topology = vec![0; n * n];
            let p = Policy {
                normal_sampling: Some(NormalSampling::Native),
                ..policy()
            };
            let [row, col] = [n / 3, n - 1];
            let seeded = row * n + col;
            t.topology[seeded] = p.source_mask & !p.roadside_flag;
            let already_cliff = (row - 1) * n + col;
            t.topology[already_cliff] = p.cliff_flag;
            let mut expected = t.topology.clone();
            let mut affected = 0;
            let mut added = 0;
            let horizontal = (col - p.radius_cells..=col).map(|c| (row, c));
            let vertical = (row - p.radius_cells..=row + p.radius_cells)
                .filter(|&r| r != row)
                .map(|r| (r, col));
            for (r, c) in horizontal.chain(vertical) {
                affected += 1;
                let uv = [r, c].map(|v| (v as f32 + 0.5) / n as f32);
                let slope = crate::terrain_normal::slope(size, hr, &t.height, uv).unwrap();
                let bits = &mut expected[r * n + c];
                if slope > p.cliff_slope_degrees && *bits & p.cliff_flag == 0 {
                    *bits |= p.cliff_flag;
                    added += 1;
                }
            }
            let (actual, report) = apply(&t, p, || false).unwrap().unwrap();
            assert_eq!(actual, expected);
            assert_eq!(report.affected_cells, affected);
            assert_eq!(report.cliff_added, added);
            assert_eq!(report.roadside_added, 0);
        }
    }
    #[test]
    fn ordered_dilation_matches_independent_brute_force_and_keeps_other_bits() {
        let mut t = terrain();
        let n = t.surface_resolution;
        for z in 0..n {
            for x in 0..n {
                t.topology[z * n + x] = if (x * 7 + z * 11) % 19 == 0 {
                    2048
                } else if (x + z) % 3 == 0 {
                    128
                } else {
                    1 | 67108864
                };
            }
        }
        let original = t.topology.clone();
        let p = policy();
        let mut expected = original.clone();
        for vertical in [false, true] {
            let frozen = expected.clone();
            for z in 0..n {
                for x in 0..n {
                    let k = if vertical { z } else { x };
                    let hit = (k.saturating_sub(p.radius_cells)..=(k + p.radius_cells).min(n - 1))
                        .any(|q| {
                            frozen[if vertical { q * n + x } else { z * n + q }] & p.source_mask
                                != 0
                        });
                    if hit && expected[z * n + x] & p.eligible_mask != 0 {
                        expected[z * n + x] |= p.roadside_flag;
                    }
                }
            }
        }
        let (actual, report) = apply(&t, p, || false).unwrap().unwrap();
        assert_eq!(actual, expected);
        assert_eq!(t.topology, original);
        assert!(report.roadside_added > 0);
        assert_eq!(report.cliff_added, 0);
        assert!(
            original
                .iter()
                .zip(&actual)
                .all(|(&a, &b)| b & !p.roadside_flag == a)
        );
    }
    #[test]
    fn eligibility_limits_second_pass_and_steep_ground_is_marked_even_without_field() {
        let mut t = terrain();
        let n = t.surface_resolution;
        let hr = t.height_resolution;
        t.topology.fill(128);
        t.topology[(n / 2) * n + n / 2] = 2048;
        for z in 0..hr {
            for x in 0..hr {
                t.height[z * hr + x] = if x < hr / 2 { 10000 } else { 24000 };
            }
        }
        let (out, r) = apply(&t, policy(), || false).unwrap().unwrap();
        assert_eq!(r.roadside_added, 0);
        assert!(r.cliff_added > 0);
        assert_eq!(out[(n / 2 + 6) * n + n / 2 + 6], 128); // no eligible horizontal seed at this corner
        assert!(out[(n / 2) * n + n / 2 - 1] & 2 != 0);
    }
    #[test]
    fn boundaries_cancellation_and_invalid_policy_never_publish_partial_output() {
        let mut t = terrain();
        let n = t.surface_resolution;
        t.topology[0] = 2048;
        let (out, _) = apply(&t, policy(), || false).unwrap().unwrap();
        assert!(out[6 * n + 6] & 4096 != 0);
        assert_eq!(out[7 * n + 7], 1);
        assert_eq!(out[n - 1], 1);
        let calls = std::cell::Cell::new(0);
        assert!(
            apply(&t, policy(), || {
                calls.set(calls.get() + 1);
                calls.get() > 20
            })
            .unwrap()
            .is_none()
        );
        let mut p = policy();
        p.source_mask |= p.cliff_flag;
        assert!(apply(&t, p, || false).is_err());
        assert_eq!(t.topology[6 * n + 6], 1);
    }
    #[test]
    fn private_topology_uses_its_own_seeds_and_report_baseline_without_mutating_layers() {
        let t = terrain();
        let n = t.surface_resolution;
        let mut proposal = t.topology.clone();
        proposal[0] = 2048;
        // A pre-existing Roadside flag in the proposal must not count as new.
        proposal[n * n - 1] |= 4096;
        let before = proposal.clone();
        let (out, report) = apply_with_topology(&t, &proposal, policy(), || false)
            .unwrap()
            .unwrap();
        assert_eq!(proposal, before);
        assert!(t.topology.iter().all(|&v| v == 1));
        assert_ne!(out[6 * n + 6] & 4096, 0);
        assert_eq!(
            report.roadside_added,
            before
                .iter()
                .zip(&out)
                .filter(|(a, b)| **a & 4096 == 0 && **b & 4096 != 0)
                .count()
        );
        assert!(apply_with_topology(&t, &proposal[..1], policy(), || false).is_err());
        assert!(
            apply_with_topology(&t, &proposal, policy(), || true)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn final_cliff_marking_samples_the_borrowed_height_proposal() {
        let t = terrain();
        let n = t.surface_resolution;
        let hr = t.height_resolution;
        let mut proposal = t.topology.clone();
        proposal[(n / 2) * n + n / 2] = 2048;
        let mut height = t.height.clone();
        for z in 0..hr {
            for x in 0..hr {
                height[z * hr + x] = if x < hr / 2 { 10000 } else { 24000 };
            }
        }
        let (_, baseline) = apply_with_topology(&t, &proposal, policy(), || false)
            .unwrap()
            .unwrap();
        let (_, corrected) = apply_with_height(&t, &height, &proposal, policy(), || false)
            .unwrap()
            .unwrap();
        assert_eq!(baseline.cliff_added, 0);
        assert!(corrected.cliff_added > 0);
        assert!(t.height.iter().all(|&h| h == 18000));
        assert!(apply_with_height(&t, &height[..1], &proposal, policy(), || false).is_err());
    }
}
