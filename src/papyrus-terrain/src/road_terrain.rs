//! Source-configured road height loops over one private sparse transaction.
//! Collider sampling and native noise are explicit caller-supplied operations.
use crate::{
    path_geometry::Path,
    path_terrain::{self, Frame, Sample, Settings, Shape},
};
use serde::{Deserialize, Serialize};
pub const VERSION: &str = "papyrus-road-terrain-loops-1";
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Weight {
    pub topology_mask: i32,
    pub value: f32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub sample_spacing: f32,
    pub noise_frequency: f32,
    pub noise_unsigned_scale: f32,
    pub noise_unsigned_bias: f32,
    pub vertical_iterations: u16,
    pub road_fades: Vec<f32>,
    pub rail_fade_multiplier: f32,
    pub road_intensity: f32,
    pub rail_intensity: f32,
    pub water_topology: i32,
    pub water_clearance: f32,
    pub smoothing_weights: Vec<Weight>,
    pub default_weight: f32,
}
impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        if ![0.125, 0.25, 0.5, 1., 2., 4., 8.].contains(&self.sample_spacing)
            || !self.noise_frequency.is_finite()
            || !(0.000001..=1.).contains(&self.noise_frequency)
            || [self.noise_unsigned_scale, self.noise_unsigned_bias]
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 8.)
            || self.vertical_iterations > 256
            || self.road_fades.is_empty()
            || self.road_fades.len() > 8
            || self
                .road_fades
                .iter()
                .chain([
                    &self.rail_fade_multiplier,
                    &self.road_intensity,
                    &self.rail_intensity,
                    &self.default_weight,
                ])
                .any(|v| !v.is_finite() || !(0. ..=1.).contains(v))
            || self.water_topology == 0
            || !self.water_clearance.is_finite()
            || !(0. ..=64.).contains(&self.water_clearance)
            || self.smoothing_weights.len() > 64
            || self.smoothing_weights.iter().any(|w| {
                w.topology_mask == 0 || !w.value.is_finite() || !(0. ..=1.).contains(&w.value)
            })
        {
            return Err("Invalid road terrain loop policy".into());
        }
        Ok(())
    }
    pub fn weight(&self, topology: i32) -> f32 {
        self.smoothing_weights
            .iter()
            .find(|w| w.topology_mask & topology != 0)
            .map_or(self.default_weight, |w| w.value)
    }
}
pub struct InputPath<'a> {
    pub path: &'a Path,
    pub shape: Shape,
    pub terrain_offset: f32,
    pub random_scale: f32,
}
pub struct Input<'a> {
    pub grid: path_terrain::Grid<'a>,
    pub topology: &'a [i32],
    pub surface_resolution: usize,
    pub roads: &'a [InputPath<'a>],
    pub rails: &'a [InputPath<'a>],
}
#[derive(Debug, Serialize)]
pub struct PathPass {
    pub index: usize,
    pub sweep: path_terrain::SweepReport,
}
#[derive(Debug, Serialize)]
pub struct LoopReport {
    pub index: usize,
    pub road_fade: f32,
    pub rail_fade: f32,
    pub water_queries: usize,
    pub resampled_controls: usize,
    pub noise_samples: usize,
    pub road_passes: Vec<PathPass>,
    pub rail_passes: Vec<PathPass>,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub loops: Vec<LoopReport>,
    pub final_resampled_controls: usize,
    pub height: path_terrain::Report,
    pub runtime_access_validated: bool,
}
pub struct Output {
    pub height: Vec<i16>,
    pub source_rounding: SourceCutRounding,
    pub roads: Vec<Path>,
    pub report: Report,
}
/// Last active road writes whose continuous cut rounded upward. Rails clear
/// eligibility. This records a possible lower quantization, not an accepted edit.
#[derive(Debug)]
pub struct SourceCutRounding {
    cells: usize,
    lower: Vec<u64>,
}
impl SourceCutRounding {
    pub(crate) fn new(cells: usize) -> Self {
        Self {
            cells,
            lower: vec![0; cells.div_ceil(64)],
        }
    }
    pub(crate) fn record(&mut self, rail: bool, write: path_terrain::CellWrite) {
        let bit = 1u64 << (write.cell % 64);
        let word = &mut self.lower[write.cell / 64];
        if !rail
            && write.after > 0
            && write.raw_encoded < write.after as f32
            && write.raw_encoded < write.before as f32
            && write.target_normalized < write.before as f32 * path_terrain::SHORT_TO_FLOAT
        {
            *word |= bit;
        } else {
            *word &= !bit;
        }
    }
    pub fn cells(&self) -> usize {
        self.cells
    }
    /// Caller must bind this source-pass receipt and the exact painted grid.
    pub fn lower(&self, painted: &[i16], cell: usize) -> Option<i16> {
        (painted.len() == self.cells
            && cell < self.cells
            && self.lower[cell / 64] & (1u64 << (cell % 64)) != 0)
            .then(|| painted[cell].saturating_sub(1))
    }
}
fn topology(input: &Input<'_>, p: [f32; 3]) -> i32 {
    let n = input.surface_resolution;
    let [x, z] = [p[0], p[2]].map(|v| {
        (((v + input.grid.size * 0.5) * (1. / input.grid.size)) * n as f32)
            .clamp(0., (n - 1) as f32) as usize
    });
    input.topology[z * n + x]
}
/// None from either provider means cancellation. A collider miss must use the
/// verified installation's explicit fallback level; unknown data is an error.
pub fn run(
    input: Input<'_>,
    policy: &Policy,
    maximum_work: usize,
    noise: impl FnMut(&[f32]) -> Result<Option<Vec<f32>>, String>,
    water_height: impl FnMut([f32; 3]) -> Result<Option<f32>, String>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    run_observed(
        input,
        policy,
        maximum_work,
        noise,
        water_height,
        |_| {},
        cancelled,
    )
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct HeightActivity {
    pub pass: usize,
    pub rail: bool,
    pub path: usize,
    pub write: path_terrain::CellWrite,
}
/// Source-equivalent execution with an observational callback. Recording an
/// active write is not an acceptance certificate or a terrain-edit envelope.
pub fn run_observed(
    input: Input<'_>,
    policy: &Policy,
    maximum_work: usize,
    mut noise: impl FnMut(&[f32]) -> Result<Option<Vec<f32>>, String>,
    mut water_height: impl FnMut([f32; 3]) -> Result<Option<f32>, String>,
    mut observe: impl FnMut(HeightActivity),
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    policy.validate()?;
    let controls = input
        .roads
        .iter()
        .chain(input.rails)
        .map(|p| p.path.points().len())
        .sum::<usize>();
    if input.roads.len() + input.rails.len() > 4096
        || controls > 1_048_576
        || !(2..=2048).contains(&input.surface_resolution)
        || input.topology.len() != input.surface_resolution.pow(2)
    {
        return Err("Invalid road/rail terrain inputs".into());
    }
    for path in input.roads.iter().chain(input.rails) {
        path.shape.validate()?;
        if !path.terrain_offset.is_finite()
            || path.terrain_offset.abs() > 64.
            || !path.random_scale.is_finite()
            || !(0.01..=8.).contains(&path.random_scale)
            || !path.path.length().is_finite()
            || path.path.length() <= 0.
            || path.path.length() > 40000.
            || path.path.range() != (0, path.path.points().len() - 1)
        {
            return Err("Unsupported road/rail terrain path".into());
        }
    }
    let Some(mut frame) = Frame::new(input.grid, maximum_work, &cancelled)? else {
        return Ok(None);
    };
    // One bounded bitmap allocation; per-write updates share the sweep visit.
    let words = input.grid.height.len().div_ceil(64);
    frame.charge_work(words)?;
    let mut source_rounding = SourceCutRounding::new(input.grid.height.len());
    frame.charge_work(controls * 2)?;
    let mut roads: Vec<_> = input.roads.iter().map(|p| p.path.clone()).collect();
    let mut reports = Vec::new();
    for (index, &fade) in policy.road_fades.iter().enumerate() {
        if cancelled() {
            return Ok(None);
        }
        let mut report = LoopReport {
            index,
            road_fade: fade,
            rail_fade: fade * policy.rail_fade_multiplier,
            water_queries: 0,
            resampled_controls: 0,
            noise_samples: 0,
            road_passes: vec![],
            rail_passes: vec![],
        };
        // All road controls see the same current terrain before this loop paints
        // any road. Rails retain their supplied pre-road geometry.
        for path in roads.iter_mut().rev() {
            frame.charge_work(path.points().len() * 2)?;
            let grid = path_terrain::Grid {
                height: frame.heights()?,
                ..input.grid
            };
            let mut heights = Vec::with_capacity(path.points().len());
            let mut weights = Vec::with_capacity(path.points().len());
            for &point in path.points() {
                if cancelled() {
                    return Ok(None);
                }
                let flags = topology(&input, point);
                let mut height = path_terrain::sample_height(grid, point)?;
                if flags & policy.water_topology != 0 {
                    let Some(level) = water_height(point)? else {
                        return Ok(None);
                    };
                    if !level.is_finite() || level.abs() > 10000. {
                        return Err("Invalid road water-collider height".into());
                    }
                    height = height.max(level + policy.water_clearance);
                    report.water_queries += 1;
                }
                heights.push(height);
                weights.push(policy.weight(flags));
                report.resampled_controls += 1;
            }
            let Some(fitted) = path
                .refit_heights(
                    &heights,
                    policy.vertical_iterations,
                    &weights,
                    32_000_000,
                    &cancelled,
                )
                .map_err(|e| format!("Road Y smoothing failed: {e:?}"))?
            else {
                return Ok(None);
            };
            frame.charge_work(fitted.work())?;
            *path = fitted;
        }
        let count = roads
            .iter()
            .chain(input.rails.iter().map(|r| r.path))
            .map(|p| ((p.length() + policy.sample_spacing) / policy.sample_spacing).ceil() as usize)
            .max()
            .unwrap_or(0);
        if count > 1_000_000 {
            return Err("Road terrain noise sample allowance exceeded".into());
        }
        let coordinates: Vec<_> = (0..count)
            .map(|i| i as f32 * policy.sample_spacing * policy.noise_frequency)
            .collect();
        frame.charge_work(count)?;
        let values = if coordinates.is_empty() {
            Vec::new()
        } else {
            let Some(values) = noise(&coordinates)? else {
                return Ok(None);
            };
            values
        };
        if values.len() != count || values.iter().any(|v| !v.is_finite() || v.abs() > 2.) {
            return Err("Invalid road terrain noise batch".into());
        }
        report.noise_samples = count;
        for (is_rail, paths) in [(false, input.roads), (true, input.rails)] {
            for (i, parameters) in paths.iter().enumerate().rev() {
                let path = if is_rail { parameters.path } else { &roads[i] };
                let settings = Settings {
                    sample_spacing: policy.sample_spacing,
                    intensity: if is_rail {
                        policy.rail_intensity
                    } else {
                        policy.road_intensity
                    },
                    fade_multiplier: if is_rail {
                        report.rail_fade
                    } else {
                        report.road_fade
                    },
                };
                let Some(sweep) = frame.sweep_observed(
                    path,
                    parameters.shape,
                    settings,
                    |step, _, _| {
                        let t = (values[step] * policy.noise_unsigned_scale
                            + policy.noise_unsigned_bias)
                            .clamp(0., 1.);
                        let radius = parameters.shape.width * 0.5;
                        Ok(Sample {
                            radius: radius + (radius * parameters.random_scale - radius) * t,
                            depth: parameters.terrain_offset
                                + (parameters.terrain_offset * parameters.random_scale
                                    - parameters.terrain_offset)
                                    * t,
                            inner_scale: 1.,
                        })
                    },
                    |write| {
                        source_rounding.record(is_rail, write);
                        observe(HeightActivity {
                            pass: index,
                            rail: is_rail,
                            path: i,
                            write,
                        })
                    },
                    &cancelled,
                )?
                else {
                    return Ok(None);
                };
                let record = PathPass { index: i, sweep };
                if is_rail {
                    report.rail_passes.push(record);
                } else {
                    report.road_passes.push(record);
                }
            }
        }
        reports.push(report);
    }
    let mut final_resampled_controls = 0;
    for path in &mut roads {
        if cancelled() {
            return Ok(None);
        }
        frame.charge_work(path.points().len())?;
        let grid = path_terrain::Grid {
            height: frame.heights()?,
            ..input.grid
        };
        let heights = path
            .points()
            .iter()
            .map(|&p| path_terrain::sample_height(grid, p))
            .collect::<Result<Vec<_>, _>>()?;
        let Some(fitted) = path
            .refit_heights(
                &heights,
                0,
                &vec![1.; heights.len()],
                32_000_000,
                &cancelled,
            )
            .map_err(|e| format!("Final road height sampling failed: {e:?}"))?
        else {
            return Ok(None);
        };
        frame.charge_work(fitted.work())?;
        final_resampled_controls += heights.len();
        *path = fitted;
    }
    let Some(out) = frame.finish(&cancelled)? else {
        return Ok(None);
    };
    Ok(Some(Output {
        height: out.height,
        source_rounding,
        roads,
        report: Report {
            engine: VERSION,
            loops: reports,
            final_resampled_controls,
            height: out.report,
            runtime_access_validated: false,
        },
    }))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_cut_rounding_requires_a_real_last_road_cut_and_preserves_untouched_cells() {
        let mut receipt = SourceCutRounding::new(130);
        let painted = vec![16522; 130];
        let cut = path_terrain::CellWrite {
            cell: 64,
            opacity: 0.05,
            before: 16522,
            target_normalized: 0.504,
            raw_encoded: 16521.97,
            after: 16522,
        };
        assert_eq!(receipt.lower(&painted, 64), None);
        receipt.record(false, cut);
        assert_eq!(receipt.lower(&painted, 64), Some(16521));
        for i in [0, 63, 65, 129, 130] {
            assert_eq!(receipt.lower(&painted, i), None);
        }
        assert_eq!(receipt.lower(&painted[..129], 64), None);
        receipt.record(true, cut);
        assert_eq!(receipt.lower(&painted, 64), None);
        receipt.record(false, cut);
        receipt.record(
            false,
            path_terrain::CellWrite {
                raw_encoded: 16522.1,
                target_normalized: 0.505,
                ..cut
            },
        );
        assert_eq!(receipt.lower(&painted, 64), None);
        receipt.record(
            false,
            path_terrain::CellWrite {
                target_normalized: 16522. * path_terrain::SHORT_TO_FLOAT,
                ..cut
            },
        );
        assert_eq!(
            receipt.lower(&painted, 64),
            None,
            "round-trip arithmetic alone is not a cut"
        );
    }
    fn policy() -> Policy {
        Policy {
            sample_spacing: 1.,
            noise_frequency: 0.005,
            noise_unsigned_scale: 0.5,
            noise_unsigned_bias: 1.,
            vertical_iterations: 8,
            road_fades: vec![1., 0.5],
            rail_fade_multiplier: 0.25,
            road_intensity: 1.,
            rail_intensity: 1.,
            water_topology: 49152,
            water_clearance: 2.,
            smoothing_weights: vec![
                Weight {
                    topology_mask: 525312,
                    value: 0.,
                },
                Weight {
                    topology_mask: 1048576,
                    value: 0.5,
                },
                Weight {
                    topology_mask: 16384,
                    value: 0.1,
                },
                Weight {
                    topology_mask: 32768,
                    value: 0.3,
                },
            ],
            default_weight: 1.,
        }
    }
    fn path(y: f32, z: f32) -> Path {
        Path::from_controls(&[[-24., y, z], [0., y, z], [24., y, z]], 1000, || false)
            .unwrap()
            .unwrap()
    }
    fn shape(width: f32) -> Shape {
        Shape {
            width,
            padding: [1., 1.],
            fade: [1., 8.],
        }
    }
    fn grid(height: &[i16], n: usize) -> path_terrain::Grid<'_> {
        path_terrain::Grid {
            size: 128.,
            resolution: n,
            origin_y: -500.,
            vertical_span: 1000.,
            height,
        }
    }
    #[test]
    fn source_loops_reverse_roads_then_rails_and_resample_final_heights_without_changing_xz() {
        let a = path(3., 0.);
        let b = path(4., 12.);
        let rail = Path::from_controls(
            &[[0., 60., -24.], [0., 60., 0.], [0., 60., 24.]],
            1000,
            || false,
        )
        .unwrap()
        .unwrap();
        let rail_before = serde_json::to_vec(&rail).unwrap();
        let roads = [
            InputPath {
                path: &a,
                shape: shape(10.),
                terrain_offset: -0.125,
                random_scale: 0.75,
            },
            InputPath {
                path: &b,
                shape: shape(4.),
                terrain_offset: -0.125,
                random_scale: 0.75,
            },
        ];
        let rails = [InputPath {
            path: &rail,
            shape: shape(6.),
            terrain_offset: 0.,
            random_scale: 1.,
        }];
        let base = vec![18000; 65 * 65];
        let mut topology = vec![1; 33 * 33];
        topology[16 * 33 + 16] = 524288;
        let input = || Input {
            grid: grid(&base, 65),
            topology: &topology,
            surface_resolution: 33,
            roads: &roads,
            rails: &rails,
        };
        let out = run(
            input(),
            &policy(),
            1_000_000,
            |c| Ok(Some(vec![0.; c.len()])),
            |_| panic!("No water flags"),
            || false,
        )
        .unwrap()
        .unwrap();
        let replay = run(
            input(),
            &policy(),
            1_000_000,
            |c| Ok(Some(vec![0.; c.len()])),
            |_| panic!("No water flags"),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(out.height, replay.height);
        assert_eq!(
            serde_json::to_vec(&out.roads).unwrap(),
            serde_json::to_vec(&replay.roads).unwrap()
        );
        assert_eq!(out.report.loops.len(), 2);
        assert_eq!(out.report.height.path_passes, 6);
        for (index, pass) in out.report.loops.iter().enumerate() {
            assert_eq!(
                pass.road_passes.iter().map(|p| p.index).collect::<Vec<_>>(),
                vec![1, 0]
            );
            assert_eq!(
                pass.rail_passes.iter().map(|p| p.index).collect::<Vec<_>>(),
                vec![0]
            );
            assert_eq!(pass.road_fade, [1., 0.5][index]);
            assert_eq!(pass.rail_fade, [0.25, 0.125][index]);
        }
        assert_eq!(out.report.final_resampled_controls, 6);
        assert_eq!(out.report.height.full_grid_copies, 1);
        assert!(out.height[32 * 65 + 32] > base[32 * 65 + 32]);
        for (fitted, original) in out.roads.iter().zip([&a, &b]) {
            assert_eq!(fitted.points().len(), original.points().len());
            for (p, old) in fitted.points().iter().zip(original.points()) {
                assert_eq!([p[0], p[2]], [old[0], old[2]]);
                let expected = path_terrain::sample_height(grid(&out.height, 65), *p).unwrap();
                assert_eq!(p[1].to_bits(), expected.to_bits());
            }
        }
        assert_eq!(serde_json::to_vec(&rail).unwrap(), rail_before);
        assert_eq!(base, vec![18000; 65 * 65]);
    }
    #[test]
    fn ordered_topology_weights_and_water_queries_precede_final_ground_resampling() {
        let mut policy = policy();
        assert_eq!(policy.weight(1024 | 16384), 0.);
        assert_eq!(policy.weight(1048576 | 16384), 0.5);
        assert_eq!(policy.weight(16384 | 32768), 0.1);
        assert_eq!(policy.weight(32768), 0.3);
        assert_eq!(policy.weight(1), 1.);
        policy.road_intensity = 0.;
        let p = path(3., 0.);
        let roads = [InputPath {
            path: &p,
            shape: shape(10.),
            terrain_offset: -0.125,
            random_scale: 0.75,
        }];
        let base = vec![18000; 33 * 33];
        let topology = vec![49152; 33 * 33];
        let mut queries = Vec::new();
        let out = run(
            Input {
                grid: grid(&base, 33),
                topology: &topology,
                surface_resolution: 33,
                roads: &roads,
                rails: &[],
            },
            &policy,
            1_000_000,
            |c| Ok(Some(vec![0.; c.len()])),
            |p| {
                queries.push(p);
                Ok(Some(100.))
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(queries.len(), 6);
        assert!(queries[..3].iter().all(|p| p[1] == 3.));
        assert!(queries[3..].iter().all(|p| p[1] == 102.));
        assert!(out.report.loops.iter().all(|p| p.water_queries == 3));
        assert_eq!(out.height, base);
        assert!(out.roads[0].points().iter().all(|p| p[1] != 102.)); // source final pass resamples ground, without a second water clamp
    }
    #[test]
    fn missing_providers_limits_and_cancellation_never_publish_partial_height_or_paths() {
        let p = path(3., 0.);
        let before = serde_json::to_vec(&p).unwrap();
        let roads = [InputPath {
            path: &p,
            shape: shape(10.),
            terrain_offset: -0.125,
            random_scale: 0.75,
        }];
        let base = vec![18000; 33 * 33];
        let topology = vec![49152; 33 * 33];
        let input = || Input {
            grid: grid(&base, 33),
            topology: &topology,
            surface_resolution: 33,
            roads: &roads,
            rails: &[],
        };
        assert!(
            run(
                input(),
                &policy(),
                1_000_000,
                |_| Ok(None),
                |_| Ok(Some(100.)),
                || false
            )
            .unwrap()
            .is_none()
        );
        assert!(
            run(
                input(),
                &policy(),
                1_000_000,
                |c| Ok(Some(vec![0.; c.len()])),
                |_| Ok(None),
                || false
            )
            .unwrap()
            .is_none()
        );
        assert!(
            run(
                input(),
                &policy(),
                1_000_000,
                |_| Ok(Some(vec![])),
                |_| Ok(Some(100.)),
                || false
            )
            .is_err()
        );
        assert!(
            run(
                input(),
                &policy(),
                4000,
                |c| Ok(Some(vec![0.; c.len()])),
                |_| Ok(Some(100.)),
                || false
            )
            .is_err()
        );
        assert!(
            run(
                input(),
                &policy(),
                1_000_000,
                |c| Ok(Some(vec![0.; c.len()])),
                |_| Ok(Some(f32::NAN)),
                || false
            )
            .is_err()
        );
        let calls = std::cell::Cell::new(0);
        assert!(
            run(
                input(),
                &policy(),
                1_000_000,
                |c| Ok(Some(vec![0.; c.len()])),
                |_| Ok(Some(100.)),
                || {
                    calls.set(calls.get() + 1);
                    calls.get() > 300
                }
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(base, vec![18000; 33 * 33]);
        assert_eq!(serde_json::to_vec(&p).unwrap(), before);
    }
}
