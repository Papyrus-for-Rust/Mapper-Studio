//! Late source-style rail topology: reverse path sweeps, then ordered marking.
//! Input height, topology and paths remain immutable on success or cancellation.
use crate::{
    native_layers::{self, Layers},
    path_geometry::Path,
    path_surface, road_topology,
};
use serde::{Deserialize, Serialize};
pub const VERSION: &str = "papyrus-rail-path-topology-1";
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub width: f32,
    pub padding: [f32; 2],
    pub sample_spacing: f32,
    pub random_scale: f32,
    pub rail_flag: i32,
    pub opacity_threshold: f32,
    pub marking: road_topology::Policy,
}
impl Policy {
    pub fn validate(self) -> Result<(), String> {
        self.marking.validate()?;
        if !self.width.is_finite()
            || !(0.1..=256.).contains(&self.width)
            || self
                .padding
                .iter()
                .any(|v| !v.is_finite() || !(0. ..=256.).contains(v))
            || self.sample_spacing != 1.
            || self.random_scale != 1.
            || self.rail_flag <= 0
            || self.rail_flag.count_ones() != 1
            || self.marking.source_mask != (self.rail_flag | self.marking.roadside_flag)
            || self.rail_flag == self.marking.roadside_flag
            || !self.opacity_threshold.is_finite()
            || !(0. ..=1.).contains(&self.opacity_threshold)
        {
            return Err("Unsupported late rail topology policy".into());
        }
        Ok(())
    }
}
pub struct Input<'a> {
    pub layers: &'a Layers,
    pub height: &'a [i16],
    pub topology: &'a [i32],
    pub tracks: &'a [&'a Path],
    pub geometry: path_surface::Grid,
}
#[derive(Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub policy: Policy,
    pub tracks: usize,
    pub controls: usize,
    pub sweep: path_surface::Report,
    pub marking: road_topology::Report,
    pub changed_cells: usize,
    /// Includes a precharged upper allowance for complete-grid validation,
    /// copies, scans and at most one slope query per cell in the marking pass.
    pub work: usize,
    pub runtime_validated: bool,
}
pub struct Output {
    pub topology: Vec<i32>,
    pub report: Report,
}
pub fn paint(
    input: Input<'_>,
    policy: Policy,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    if cancelled() {
        return Ok(None);
    }
    policy.validate()?;
    let Input {
        layers,
        height,
        topology,
        tracks,
        geometry,
    } = input;
    let (hr, n) = native_layers::dimensions(layers.size)?;
    let controls = tracks.iter().map(|p| p.points().len()).sum::<usize>();
    if layers.height_resolution != hr
        || layers.surface_resolution != n
        || height.len() != hr * hr
        || topology.len() != n * n
        || geometry.size != layers.size as f32
        || geometry.resolution != n
        // Marking uses the native Layers height codec. Do not silently shade
        // slopes in that codec while sweeping a different vertical frame.
        || geometry.origin_y != crate::erosion::decode(0)
        || geometry.vertical_span != crate::erosion::decode(32766) - crate::erosion::decode(0)
        || tracks.len() > 4096
        || controls > 1_048_576
        || !(1..=512_000_000).contains(&maximum_work)
    {
        return Err("Invalid late rail topology grid, paths or work grant".into());
    }
    let overhead = controls
        .checked_add(16 * n * n + 2 * height.len())
        .filter(|&w| w < maximum_work)
        .ok_or("Rail topology grid allowance exhausted")?;
    for chunk in height.chunks(4096) {
        if cancelled() {
            return Ok(None);
        }
        if chunk.iter().any(|&h| !(0..=32766).contains(&h)) {
            return Err("Invalid height for late rail topology".into());
        }
    }
    let paths = tracks
        .iter()
        .map(|path| path_surface::PathSpec {
            path,
            width: policy.width,
            padding: policy.padding,
        })
        .collect::<Vec<_>>();
    let mut painted = topology.to_vec();
    let Some(sweep) = path_surface::walk(
        &paths,
        geometry,
        policy.sample_spacing,
        maximum_work - overhead,
        |_, _, _, _| Ok(policy.width * 0.5),
        &cancelled,
        |_, cell, opacity| {
            if opacity > policy.opacity_threshold {
                painted[cell] |= policy.rail_flag;
            }
        },
    )?
    else {
        return Ok(None);
    };
    let Some((output, marking)) =
        road_topology::apply_with_height(layers, height, &painted, policy.marking, &cancelled)?
    else {
        return Ok(None);
    };
    let mut changed_cells = 0;
    for (old, new) in topology.chunks(4096).zip(output.chunks(4096)) {
        if cancelled() {
            return Ok(None);
        }
        changed_cells += old.iter().zip(new).filter(|(a, b)| a != b).count();
    }
    Ok(Some(Output {
        topology: output,
        report: Report {
            engine: VERSION,
            policy,
            tracks: tracks.len(),
            controls,
            work: overhead + sweep.work,
            sweep,
            marking,
            changed_cells,
            runtime_validated: false,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    fn fixture() -> (Layers, Policy, path_surface::Grid) {
        let (hr, n) = native_layers::dimensions(1000).unwrap();
        (
            Layers {
                size: 1000,
                terrain_seed: 1,
                biome_seed: 2,
                height_resolution: hr,
                surface_resolution: n,
                height: vec![17000; hr * hr],
                topology: vec![1; n * n],
                splat: vec![],
                biome: vec![],
                normalized_biome_cells: 0,
                normalized_splat_cells: 0,
            },
            Policy {
                width: 4.,
                padding: [1., 1.],
                sample_spacing: 1.,
                random_scale: 1.,
                rail_flag: 524288,
                opacity_threshold: 0.3,
                marking: road_topology::Policy {
                    source_mask: 1572864,
                    eligible_mask: 49,
                    roadside_flag: 1048576,
                    cliff_flag: 2,
                    radius_cells: 6,
                    cliff_slope_degrees: 40.,
                    normal_sampling: None,
                },
            },
            path_surface::Grid {
                size: 1000.,
                resolution: n,
                origin_y: -500.,
                vertical_span: 1000.,
            },
        )
    }
    #[test]
    fn late_path_sweep_adds_flags_on_authored_ground_and_uses_current_height() {
        let (mut layers, policy, geometry) = fixture();
        layers.topology.fill(1 | 1024 | 128 | 2048);
        let path = Path::from_controls(
            &[[-200., 20., 0.], [0., 20., 0.], [200., 20., 0.]],
            10000,
            || false,
        )
        .unwrap()
        .unwrap();
        let curves = serde_json::to_vec(&path).unwrap();
        let tracks = [&path];
        let run = |height| {
            paint(
                Input {
                    layers: &layers,
                    height,
                    topology: &layers.topology,
                    tracks: &tracks,
                    geometry,
                },
                policy,
                32_000_000,
                || false,
            )
            .unwrap()
            .unwrap()
        };
        let flat = run(&layers.height);
        let n = layers.surface_resolution;
        let center = (n / 2) * n + n / 2;
        assert_ne!(flat.topology[center] & policy.rail_flag, 0);
        assert_ne!(flat.topology[center] & policy.marking.roadside_flag, 0);
        assert_eq!(flat.topology[0], layers.topology[0]);
        assert!(
            flat.topology
                .iter()
                .all(|v| v & (1 | 1024 | 128 | 2048) == (1 | 1024 | 128 | 2048))
        );
        let mut changed = layers.height.clone();
        let hr = layers.height_resolution;
        for z in 0..hr {
            changed[z * hr + hr / 2..(z + 1) * hr].fill(20000);
        }
        let steep = run(&changed);
        assert_eq!(flat.report.marking.cliff_added, 0);
        assert!(steep.report.marking.cliff_added > 0);
        assert_eq!(serde_json::to_vec(&path).unwrap(), curves);
        assert!(
            layers
                .topology
                .iter()
                .all(|&v| v == (1 | 1024 | 128 | 2048))
        );
        assert!(layers.height.iter().all(|&v| v == 17000));
        assert!(!steep.report.runtime_validated);
    }
    #[test]
    fn empty_roster_still_marks_existing_rail_flags_with_source_axis_propagation() {
        let (mut layers, policy, geometry) = fixture();
        let n = layers.surface_resolution;
        for z in 0..n {
            for x in 0..n {
                layers.topology[z * n + x] = if (x + 2 * z) % 5 == 0 { 128 } else { 1 };
            }
        }
        layers.topology[(n / 2) * n + n / 2] = policy.rail_flag;
        let mut expected = layers.topology.clone();
        for vertical in [false, true] {
            let before = expected.clone();
            for z in 0..n {
                for x in 0..n {
                    let (a, b) = if vertical { (x, z) } else { (z, x) };
                    let hit = (b.saturating_sub(6)..=(b + 6).min(n - 1)).any(|k| {
                        let i = if vertical { k * n + a } else { a * n + k };
                        before[i] & policy.marking.source_mask != 0
                    });
                    if hit && expected[z * n + x] & 49 != 0 {
                        expected[z * n + x] |= 1048576;
                    }
                }
            }
        }
        let output = paint(
            Input {
                layers: &layers,
                height: &layers.height,
                topology: &layers.topology,
                tracks: &[],
                geometry,
            },
            policy,
            32_000_000,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(output.topology, expected);
        assert!(output.report.marking.roadside_added > 0);
        assert_eq!(output.report.sweep.callbacks, 0);
    }
    #[test]
    fn work_cancellation_and_invalid_input_leave_the_accepted_revision_unchanged() {
        let (layers, policy, geometry) = fixture();
        let path = Path::from_controls(
            &[[-200., 20., 0.], [0., 20., 0.], [200., 20., 0.]],
            10000,
            || false,
        )
        .unwrap()
        .unwrap();
        let tracks = [&path];
        let input = || Input {
            layers: &layers,
            height: &layers.height,
            topology: &layers.topology,
            tracks: &tracks,
            geometry,
        };
        let output = paint(input(), policy, 32_000_000, || false)
            .unwrap()
            .unwrap();
        assert!(paint(input(), policy, output.report.work - 1, || false).is_err());
        let calls = Cell::new(0);
        assert!(
            paint(input(), policy, 32_000_000, || {
                calls.set(calls.get() + 1);
                calls.get() > 300
            })
            .unwrap()
            .is_none()
        );
        assert!(paint(input(), policy, 0, || true).unwrap().is_none());
        let mut bad = policy;
        bad.random_scale = 0.75;
        assert!(paint(input(), bad, 32_000_000, || false).is_err());
        let mut bad_grid = geometry;
        bad_grid.resolution /= 2;
        assert!(
            paint(
                Input {
                    geometry: bad_grid,
                    ..input()
                },
                policy,
                32_000_000,
                || false
            )
            .is_err()
        );
        bad_grid = geometry;
        bad_grid.vertical_span *= 0.5;
        assert!(
            paint(
                Input {
                    geometry: bad_grid,
                    ..input()
                },
                policy,
                32_000_000,
                || false
            )
            .is_err()
        );
        assert!(layers.topology.iter().all(|&v| v == 1));
        assert!(layers.height.iter().all(|&v| v == 17000));
    }
}
