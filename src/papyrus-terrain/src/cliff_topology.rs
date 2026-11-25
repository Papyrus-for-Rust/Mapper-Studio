//! Whole-map cliff classification and ordered cliffside dilation.
use crate::{clutter_topology, native_layers::View, terrain_normal};
use serde::{Deserialize, Serialize};

pub const VERSION: &str = "papyrus-cliff-topology-1";

#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub cliff_flag: u32,
    pub cliffside_flag: u32,
    pub protected_topology: u32,
    pub rock_channel: usize,
    pub slope_cutoff: f32,
    pub rock_cutoff: f32,
    pub radius_cells: usize,
}
impl Policy {
    pub fn validate(self) -> Result<(), String> {
        self.dilation().validate()?;
        if self.protected_topology & (self.cliff_flag | self.cliffside_flag) != 0
            || self.rock_channel >= 8
            || !self.slope_cutoff.is_finite()
            || !(0. ..=90.).contains(&self.slope_cutoff)
            || !self.rock_cutoff.is_finite()
            || !(0. ..=1.).contains(&self.rock_cutoff)
        {
            return Err("Invalid cliff topology policy".into());
        }
        Ok(())
    }
    fn dilation(self) -> clutter_topology::Policy {
        clutter_topology::Policy {
            decor_flag: self.cliff_flag,
            clutter_flag: self.cliffside_flag,
            radius_cells: self.radius_cells,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub policy: Policy,
    pub keep_existing: bool,
    pub added_cliff_cells: usize,
    pub removed_cliff_cells: usize,
    pub added_cliffside_cells: usize,
    pub protected_cells: usize,
    pub work: usize,
}

/// Source scene parameters and code receipts are bound by the caller. The input
/// remains unchanged on errors and cancellation, including during dilation.
pub fn apply(
    layers: View<'_>,
    policy: Policy,
    keep_existing: bool,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<(Vec<i32>, Report)>, String> {
    apply_inner(layers, policy, keep_existing, maximum_work, None, cancelled)
}

/// Apply a resolved slope decision grid in the native height raster frame.
/// The producer must match the source slope cutoff for every sampled vertex.
pub fn apply_with_vertex_mask(
    layers: View<'_>,
    policy: Policy,
    keep_existing: bool,
    maximum_work: usize,
    decisions: &[u32],
    cancelled: impl Fn() -> bool,
) -> Result<Option<(Vec<i32>, Report)>, String> {
    apply_inner(
        layers,
        policy,
        keep_existing,
        maximum_work,
        Some(decisions),
        cancelled,
    )
}

fn apply_inner(
    layers: View<'_>,
    policy: Policy,
    keep_existing: bool,
    maximum_work: usize,
    decisions: Option<&[u32]>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<(Vec<i32>, Report)>, String> {
    if cancelled() {
        return Ok(None);
    }
    policy.validate()?;
    let n = layers.surface_resolution;
    let hr = layers.height_resolution;
    if !(1000..=6000).contains(&layers.size)
        || !(2..=2048).contains(&n)
        || !(2..=4097).contains(&hr)
        || layers.topology.len() != n * n
        || layers.splat.len() != n * n * 8
        || layers.height.len() != hr * hr
        || policy.radius_cells >= n
    {
        return Err("Invalid cliff topology terrain".into());
    }
    // Four corner normals, rock interpolation, classification and morphology.
    let work = 32 * n * n + hr * hr + 2 * n;
    if work > maximum_work {
        return Err("Cliff topology exceeds its work allowance".into());
    }
    for row in layers.height.chunks(hr) {
        if cancelled() {
            return Ok(None);
        }
        if row.iter().any(|v| !(0..=32766).contains(v)) {
            return Err("Invalid cliff topology height encoding".into());
        }
    }
    let mut topology = layers.topology.to_vec();
    let mut report = Report {
        engine: VERSION,
        policy,
        keep_existing,
        added_cliff_cells: 0,
        removed_cliff_cells: 0,
        added_cliffside_cells: 0,
        protected_cells: 0,
        work,
    };
    let rock = &layers.splat[policy.rock_channel * n * n..][..n * n];
    let vertex_centres = n.is_power_of_two() && (hr - 1).is_multiple_of(2 * n);
    if decisions.is_some_and(|words| !vertex_centres || words.len() != (hr * hr).div_ceil(16)) {
        return Err("Invalid cliff slope decision grid".into());
    }
    let stride = (hr - 1) / n;
    let norm_y = layers.size as f32 / 1000. / hr as f32;
    for (z, row) in topology.chunks_mut(n).enumerate() {
        if cancelled() {
            return Ok(None);
        }
        for (x, flags) in row.iter_mut().enumerate() {
            if *flags as u32 & policy.protected_topology != 0 {
                report.protected_cells += 1;
                continue;
            }
            let weight =
                crate::monument_composition::source_rock_sample(n, x, z, |x, z| rock[z * n + x]);
            let before = *flags as u32 & policy.cliff_flag != 0;
            let steep = || {
                if let Some(words) = decisions {
                    let i = (z * stride + stride / 2) * hr + x * stride + stride / 2;
                    return Ok(words[i / 16] & (1 << ((i % 16) * 2)) != 0);
                }
                if vertex_centres {
                    terrain_normal::slope_at_vertex(
                        hr,
                        norm_y,
                        layers.height,
                        x * stride + stride / 2,
                        z * stride + stride / 2,
                    )
                    .map(|slope| slope > policy.slope_cutoff)
                } else {
                    let uv = [(x as f32 + 0.5) / n as f32, (z as f32 + 0.5) / n as f32];
                    terrain_normal::slope(layers.size, hr, layers.height, uv)
                        .map(|slope| slope > policy.slope_cutoff)
                }
            };
            if weight > policy.rock_cutoff || steep()? {
                *flags |= policy.cliff_flag as i32;
                report.added_cliff_cells += usize::from(!before);
            } else if !keep_existing {
                *flags &= !(policy.cliff_flag as i32);
                report.removed_cliff_cells += usize::from(before);
            }
        }
    }
    let Some(dilation) = clutter_topology::dilate(
        &mut topology,
        n,
        policy.dilation(),
        maximum_work,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    report.added_cliffside_cells = dilation.added_cells;
    Ok(Some((topology, report)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_layers::Layers;

    fn policy() -> Policy {
        Policy {
            cliff_flag: 2,
            cliffside_flag: 4194304,
            protected_topology: 8389632,
            rock_channel: 3,
            slope_cutoff: 30.,
            rock_cutoff: 0.4,
            radius_cells: 1,
        }
    }
    fn terrain() -> Layers {
        Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: 33,
            surface_resolution: 8,
            height: vec![16383; 33 * 33],
            biome: vec![0; 64 * 5],
            splat: vec![0; 64 * 8],
            topology: vec![i32::MIN | 1; 64],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        }
    }
    // Independent morphology over frozen rows and then frozen columns.
    fn dilation(mut cells: Vec<i32>, n: usize) -> Vec<i32> {
        for vertical in [false, true] {
            let before = cells.clone();
            for z in 0..n {
                for x in 0..n {
                    let center = if vertical { z } else { x };
                    let affected = (center.saturating_sub(1)..=(center + 1).min(n - 1)).any(|k| {
                        before[if vertical { k * n + x } else { z * n + k }] & 4194306 != 0
                    });
                    if affected && cells[z * n + x] & 2 == 0 {
                        cells[z * n + x] |= 4194304;
                    }
                }
            }
        }
        cells
    }
    #[test]
    fn rock_threshold_is_strict_and_existing_flags_follow_scene_policy() {
        // Reciprocal-byte multiplication rounds 102 above the stored 0.4 cutoff.
        for (value, cliff) in [(101, false), (102, true), (103, true), (255, true)] {
            let mut layers = terrain();
            layers.splat[3 * 64..4 * 64].fill(value);
            layers.topology[0] |= 2;
            for keep in [false, true] {
                let (out, report) = apply(layers.view(), policy(), keep, usize::MAX, || false)
                    .unwrap()
                    .unwrap();
                let mut expected = vec![i32::MIN | 1 | if cliff { 2 } else { 0 }; 64];
                if keep {
                    expected[0] |= 2;
                }
                assert_eq!(out, dilation(expected, 8));
                assert_eq!(report.added_cliff_cells, if cliff { 63 } else { 0 });
                assert_eq!(report.removed_cliff_cells, usize::from(!cliff && !keep));
            }
        }
    }
    #[test]
    fn protected_cliffs_survive_and_existing_cliffsides_expand_in_source_order() {
        let mut layers = terrain();
        layers.topology[27] |= 1024 | 2;
        layers.topology[28] |= 8388608;
        layers.topology[63] |= 4194304;
        layers.topology[0] |= 2;
        let mut expected = layers.topology.clone();
        expected[0] &= !2;
        let (out, report) = apply(layers.view(), policy(), false, usize::MAX, || false)
            .unwrap()
            .unwrap();
        assert_eq!(out, dilation(expected, 8));
        assert_eq!(report.protected_cells, 2);
        assert_eq!(out[28] & 4194304, 4194304);
        assert_eq!(report.removed_cliff_cells, 1);
        for (a, b) in layers.topology.iter().zip(&out) {
            assert_eq!(a & !4194306, b & !4194306);
        }
    }
    #[test]
    fn steep_height_and_bilinear_rock_use_surface_cell_centres() {
        let mut layers = terrain();
        for z in 0..33 {
            layers.height[z * 33..z * 33 + 33].fill(if z >= 14 { 20000 } else { 12000 });
        }
        layers.splat[3 * 64 + 2 * 8 + 2] = 255;
        let mut expected = layers.topology.clone();
        for z in 0..8 {
            for x in 0..8 {
                let uv = [(x as f32 + 0.5) / 8., (z as f32 + 0.5) / 8.];
                let [px, pz] = uv.map(|v| v * 7.);
                let (a, b) = (px as usize, pz as usize);
                let at = |x, z| f32::from(layers.splat[3 * 64 + z * 8 + x]) * (1. / 255.);
                let lo = at(a, b) + (at(a + 1, b) - at(a, b)) * px.fract();
                let hi = at(a, b + 1) + (at(a + 1, b + 1) - at(a, b + 1)) * px.fract();
                let rock = lo + (hi - lo) * pz.fract();
                if terrain_normal::slope(1000, 33, &layers.height, uv).unwrap() > 30. || rock > 0.4
                {
                    expected[z * 8 + x] |= 2;
                }
            }
        }
        let (out, report) = apply(layers.view(), policy(), false, usize::MAX, || false)
            .unwrap()
            .unwrap();
        assert!(report.added_cliff_cells > 0);
        assert_eq!(out, dilation(expected, 8));
    }
    #[test]
    fn invalid_inputs_work_limits_and_late_cancellation_publish_nothing() {
        let layers = terrain();
        let (_, report) = apply(layers.view(), policy(), false, usize::MAX, || false)
            .unwrap()
            .unwrap();
        assert!(apply(layers.view(), policy(), false, report.work - 1, || false).is_err());
        assert!(
            apply(layers.view(), policy(), false, report.work, || false)
                .unwrap()
                .is_some()
        );
        let calls = std::cell::Cell::new(0);
        assert!(
            apply(layers.view(), policy(), false, usize::MAX, || {
                calls.set(calls.get() + 1);
                calls.get() > 45
            })
            .unwrap()
            .is_none()
        );
        assert_eq!(layers.topology, vec![i32::MIN | 1; 64]);
        let mut bad = layers.clone();
        bad.height[0] = -1;
        assert!(apply(bad.view(), policy(), false, usize::MAX, || false).is_err());
        let mut bad = policy();
        bad.rock_channel = 8;
        assert!(apply(layers.view(), bad, false, usize::MAX, || false).is_err());
    }
}
