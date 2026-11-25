//! Private river texture/topology passes. Preserves absolute paint weights and
//! ordered callbacks across overlapping sweeps; no water surface or scene is created.
use crate::{bank_topology, path_geometry::Path, river_layout};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
pub const VERSION: &str = "papyrus-river-surface-1";
/// Combined retains the original study API. Source-ordered generation uses
/// Topology on layout paths/base heights, then Texture on carved paths/heights.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pass {
    Combined,
    Topology,
    Texture,
}
#[cfg(test)]
use crate::path_surface::{BYTE_TO_FLOAT, float_byte, ray_distance, visit_quad};
use crate::path_surface::{Failure, Work, set_splat};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub sample_spacing: f32,
    pub splat_channel: Option<usize>,
    pub topology_mask: i32,
    pub topology_threshold: f32,
    pub bank: bank_topology::Policy,
}
impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        self.bank.validate()?;
        if ![0.125, 0.25, 0.5, 1., 2., 4., 8.].contains(&self.sample_spacing)
            || self.splat_channel.is_some_and(|v| v >= 8)
            || self.topology_mask < 0
            || !self.topology_threshold.is_finite()
            || !(0. ..=1.).contains(&self.topology_threshold)
        {
            return Err("Invalid river surface policy".into());
        }
        Ok(())
    }
}
pub struct Grid<'a> {
    pub size: f32,
    pub height_resolution: usize,
    pub surface_resolution: usize,
    pub origin_y: f32,
    pub vertical_span: f32,
    pub height: &'a [i16],
    pub splat: &'a [u8],
    pub topology: &'a [i32],
}
pub struct Output {
    pub splat: Vec<u8>,
    pub topology: Vec<i32>,
    pub report: Report,
}
struct Surface<'a> {
    splat: Cow<'a, [u8]>,
    topology: Cow<'a, [i32]>,
    report: Report,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub paths: usize,
    pub paint_sweeps: usize,
    pub topology_sweeps: usize,
    pub paint_callbacks: usize,
    pub topology_callbacks: usize,
    pub changed_splat_cells: usize,
    pub changed_topology_cells: usize,
    pub minimum_splat_sum: u32,
    pub maximum_splat_sum: u32,
    pub non_normalized_cells: usize,
    pub bank: bank_topology::Report,
    pub work: usize,
}
#[derive(Clone)]
pub struct Prepared {
    paths: Vec<Path>,
    shape: river_layout::Policy,
    policy: Policy,
    noise_count: usize,
    maximum_work: usize,
    preparation_work: usize,
}
pub fn prepare(
    paths: &[Path],
    shape: &river_layout::Policy,
    policy: &Policy,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Prepared>, String> {
    if cancelled() {
        return Ok(None);
    }
    shape.validate()?;
    policy.validate()?;
    let controls = paths.iter().map(|p| p.points().len()).sum::<usize>();
    if paths.len() > 4096
        || controls > 1_048_576
        || !(1..=512_000_000).contains(&maximum_work)
        || paths.iter().any(|p| {
            p.length() <= 0.
                || !p.length().is_finite()
                || p.length() > 40000.
                || p.range() != (0, p.points().len() - 1)
        })
        || controls > maximum_work
    {
        return Err("Invalid river surface paths or resource allowance".into());
    }
    let count = paths
        .iter()
        .map(|p| ((p.length() + policy.sample_spacing) / policy.sample_spacing).ceil() as usize)
        .max()
        .unwrap_or(0);
    if count > 1_000_000 {
        return Err("River surface noise/sample budget exceeded".into());
    }
    Ok((!cancelled()).then_some(Prepared {
        paths: paths.to_vec(),
        shape: shape.clone(),
        policy: policy.clone(),
        noise_count: count,
        maximum_work,
        preparation_work: controls,
    }))
}
struct SweepReport {
    sweeps: usize,
    callbacks: usize,
}
impl Prepared {
    pub fn noise_coordinates(&self) -> Vec<f32> {
        (0..self.noise_count)
            .map(|i| i as f32 * self.policy.sample_spacing * self.shape.noise_frequency)
            .collect()
    }
    pub fn apply(
        &self,
        g: Grid<'_>,
        noise: &[f32],
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Output>, String> {
        self.apply_pass(g, noise, Pass::Combined, cancelled)
    }
    pub fn apply_pass(
        &self,
        g: Grid<'_>,
        noise: &[f32],
        pass: Pass,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Output>, String> {
        let Some(surface) = self.apply_borrowed(g, noise, pass, &cancelled)? else {
            return Ok(None);
        };
        let output = Output {
            splat: surface.splat.into_owned(),
            topology: surface.topology.into_owned(),
            report: surface.report,
        };
        Ok((!cancelled()).then_some(output))
    }

    pub(crate) fn apply_topology(
        &self,
        g: Grid<'_>,
        noise: &[f32],
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<(Vec<i32>, Report)>, String> {
        Ok(self
            .apply_borrowed(g, noise, Pass::Topology, cancelled)?
            .map(|output| (output.topology.into_owned(), output.report)))
    }

    pub(crate) fn apply_texture(
        &self,
        g: Grid<'_>,
        noise: &[f32],
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<(Vec<u8>, Report)>, String> {
        Ok(self
            .apply_borrowed(g, noise, Pass::Texture, &cancelled)?
            .map(|output| (output.splat.into_owned(), output.report))
            .filter(|_| !cancelled()))
    }

    fn apply_borrowed<'a>(
        &self,
        g: Grid<'a>,
        noise: &[f32],
        pass: Pass,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Surface<'a>>, String> {
        if cancelled() {
            return Ok(None);
        }
        let n = g.surface_resolution;
        let hr = g.height_resolution;
        if !(2..=2048).contains(&n)
            || !(2..=4097).contains(&hr)
            || self.policy.bank.radius_cells >= n
            || hr.checked_mul(hr) != Some(g.height.len())
            || n.checked_mul(n).and_then(|v| v.checked_mul(8)) != Some(g.splat.len())
            || n * n != g.topology.len()
            || !g.size.is_finite()
            || !(1. ..=6000.).contains(&g.size)
            || !g.origin_y.is_finite()
            || g.origin_y.abs() > 10000.
            || !g.vertical_span.is_finite()
            || !(1. ..=20000.).contains(&g.vertical_span)
            || noise.len() != self.noise_count
            || noise.iter().any(|v| !v.is_finite() || v.abs() > 2.)
        {
            return Err("Invalid river surface grids or noise".into());
        }
        let mut w = Work {
            used: self.preparation_work,
            limit: self.maximum_work,
            cancelled: &cancelled,
        };
        match self.run(&g, noise, pass, &mut w) {
            Ok(v) => Ok(Some(v)),
            Err(Failure::Cancelled) => Ok(None),
            Err(Failure::Error(e)) => Err(e),
        }
    }
    fn run<'a>(
        &self,
        g: &Grid<'a>,
        noise: &[f32],
        pass: Pass,
        w: &mut Work<'_>,
    ) -> Result<Surface<'a>, Failure> {
        let cells = g.topology.len();
        for i in 0..cells {
            w.visit(1)?;
            if (0..8).all(|c| g.splat[c * cells + i] == 0) {
                return Err(Failure::Error("Empty source splat cell".into()));
            }
        }
        let mut splat = Cow::Borrowed(g.splat);
        let mut topology = Cow::Borrowed(g.topology);
        let empty = SweepReport {
            sweeps: 0,
            callbacks: 0,
        };
        let paint =
            if let Some(channel) = self.policy.splat_channel.filter(|_| pass != Pass::Topology) {
                let splat = splat.to_mut();
                self.walk(g, noise, w, |i, opacity| {
                    set_splat(splat, cells, i, channel, opacity)
                })?
            } else {
                empty
            };
        let topo = if pass != Pass::Texture && self.policy.topology_mask != 0 {
            let topology = topology.to_mut();
            self.walk(g, noise, w, |i, opacity| {
                if opacity > self.policy.topology_threshold {
                    topology[i] |= self.policy.topology_mask;
                }
            })?
        } else {
            SweepReport {
                sweeps: 0,
                callbacks: 0,
            }
        };
        w.visit(0)?;
        let remaining = w.limit - w.used;
        if remaining == 0 {
            return Err(Failure::Error("River surface work budget exceeded".into()));
        }
        let bank = if pass == Pass::Texture {
            bank_topology::Report {
                engine: bank_topology::VERSION,
                policy: self.policy.bank,
                bank_added: 0,
                cliff_added: 0,
                affected_cells: 0,
                work: 0,
            }
        } else {
            let (dilated, report) = bank_topology::apply(
                bank_topology::Terrain {
                    size: g.size,
                    height_resolution: g.height_resolution,
                    surface_resolution: g.surface_resolution,
                    height: g.height,
                    topology: &topology,
                },
                self.policy.bank,
                remaining,
                w.cancelled,
            )
            .map_err(Failure::Error)?
            .ok_or(Failure::Cancelled)?;
            topology = Cow::Owned(dilated);
            report
        };
        w.visit(bank.work)?;
        let mut report = Report {
            engine: VERSION,
            paths: self.paths.len(),
            paint_sweeps: paint.sweeps,
            topology_sweeps: topo.sweeps,
            paint_callbacks: paint.callbacks,
            topology_callbacks: topo.callbacks,
            changed_splat_cells: 0,
            changed_topology_cells: 0,
            minimum_splat_sum: u32::MAX,
            maximum_splat_sum: 0,
            non_normalized_cells: 0,
            bank,
            work: 0,
        };
        let compare_splat = matches!(splat, Cow::Owned(_));
        for (i, (&before_topology, &after_topology)) in g.topology.iter().zip(topology.iter()).enumerate()
        {
            w.visit(1)?;
            let mut sum = 0;
            let mut different = false;
            for c in 0..8 {
                let q = c * cells + i;
                sum += u32::from(splat[q]);
                different |= compare_splat && splat[q] != g.splat[q];
            }
            report.changed_splat_cells += usize::from(different);
            report.changed_topology_cells += usize::from(after_topology != before_topology);
            report.minimum_splat_sum = report.minimum_splat_sum.min(sum);
            report.maximum_splat_sum = report.maximum_splat_sum.max(sum);
            report.non_normalized_cells += usize::from(sum != 255);
        }
        w.visit(0)?;
        report.work = w.used;
        Ok(Surface {
            splat,
            topology,
            report,
        })
    }
    fn walk(
        &self,
        g: &Grid<'_>,
        noise: &[f32],
        w: &mut Work<'_>,
        mut callback: impl FnMut(usize, f32),
    ) -> Result<SweepReport, Failure> {
        let paths: Vec<_> = self
            .paths
            .iter()
            .map(|path| crate::path_surface::PathSpec {
                path,
                width: self.shape.width,
                padding: self.shape.padding,
            })
            .collect();
        let report = crate::path_surface::walk(
            &paths,
            crate::path_surface::Grid {
                size: g.size,
                resolution: g.surface_resolution,
                origin_y: g.origin_y,
                vertical_span: g.vertical_span,
            },
            self.policy.sample_spacing,
            w.limit - w.used,
            |_, step, distance, length| Ok(self.shape.radius(distance, length, noise[step])),
            w.cancelled,
            |_, cell, opacity| callback(cell, opacity),
        )
        .map_err(Failure::Error)?
        .ok_or(Failure::Cancelled)?;
        w.visit(report.work)?;
        Ok(SweepReport {
            sweeps: report.sweeps,
            callbacks: report.callbacks,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn shape() -> river_layout::Policy {
        let v: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/rivers/server-bd19c4ac-layout.json"
        ))
        .unwrap();
        let mut p: river_layout::Policy = serde_json::from_value(v["policy"].clone()).unwrap();
        p.radius_growth = [1., 1., 1000.];
        p.radius_mouth = [1., 1., 100.];
        p.random_scale = 1.;
        p
    }
    fn policy() -> Policy {
        let v: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/rivers/server-bd19c4ac-surface.json"
        ))
        .unwrap();
        serde_json::from_value(v["policy"].clone()).unwrap()
    }
    fn reference() -> serde_json::Value {
        serde_json::from_str(include_str!(
            "../tests/fixtures/river-surface-primitives-bd19c4ac.json"
        ))
        .unwrap()
    }
    #[test]
    fn numeric_helpers_and_ordered_quad_callbacks_match_owned_methods() {
        let r = reference();
        assert_eq!(r["sources"].as_array().unwrap().len(), 4);
        let bytes = r["bytes"].as_array().unwrap();
        assert_eq!(bytes.len(), 256);
        for v in bytes {
            let got = v["value"].as_u64().unwrap() as f32 * BYTE_TO_FLOAT;
            assert_eq!(
                got.to_bits(),
                (v["expected"].as_f64().unwrap() as f32).to_bits()
            );
        }
        let floats = r["floats"].as_array().unwrap();
        assert_eq!(floats.len(), 780);
        for v in floats {
            assert_eq!(
                float_byte(v["value"].as_f64().unwrap() as f32),
                v["expected"].as_u64().unwrap() as u8
            );
        }
        for v in r["rays"].as_array().unwrap() {
            let point = |name| serde_json::from_value::<[f32; 3]>(v[name].clone()).unwrap();
            assert!(
                (ray_distance(point("origin"), point("direction"), point("query"))
                    - v["expected"].as_f64().unwrap() as f32)
                    .abs()
                    < 0.00001
            );
        }
        for v in r["quads"].as_array().unwrap() {
            let q: [[i32; 2]; 4] = serde_json::from_value(v["quad"].clone()).unwrap();
            let expected: Vec<[i32; 2]> = serde_json::from_value(v["expected"].clone()).unwrap();
            let mut work = Work {
                used: 0,
                limit: 10000,
                cancelled: &|| false,
            };
            let mut actual = Vec::new();
            let count = visit_quad(q, &mut work, |x, z| actual.push([x, z])).unwrap();
            assert_eq!(actual, expected);
            assert_eq!(count, expected.len());
        }
    }
    #[test]
    fn material_weight_is_absolute_and_saturated_target_is_retained() {
        let mut cell = [128, 63, 0, 0, 0, 0, 0, 64];
        set_splat(&mut cell, 1, 0, 7, 0.2);
        assert_eq!(cell[7], float_byte(0.2));
        assert!(cell[0] > 128);
        assert!(cell[1] > 63);
        let mut full = [0; 8];
        full[7] = 255;
        set_splat(&mut full, 1, 0, 7, 0.1);
        assert_eq!(full[7], 255);
        assert!(full[..7].iter().all(|&v| v == 0));
        let mut zero = [0; 8];
        zero[0] = 255;
        set_splat(&mut zero, 1, 0, 7, 0.);
        assert_eq!(zero[0], 255);
    }
    fn path(yaw: f32) -> Path {
        let (s, c) = yaw.to_radians().sin_cos();
        Path::from_controls(
            &[[-25. * c, 100., -25. * s], [25. * c, 100., 25. * s]],
            1000,
            || false,
        )
        .unwrap()
        .unwrap()
    }
    #[test]
    fn rotated_river_paint_and_topology_share_geometry_keep_other_bits_and_replay() {
        let n = 64;
        let h = vec![18000; 65 * 65];
        let mut splat = vec![0; 8 * n * n];
        splat[..n * n].fill(255);
        let topo = vec![1 | 67108864; n * n];
        let original = splat.clone();
        for yaw in [0., 37., 123., 271.] {
            let prep = prepare(&[path(yaw)], &shape(), &policy(), 2_000_000, || false)
                .unwrap()
                .unwrap();
            let noise = vec![0.; prep.noise_coordinates().len()];
            let run = || {
                prep.apply(
                    Grid {
                        size: 128.,
                        height_resolution: 65,
                        surface_resolution: n,
                        origin_y: -500.,
                        vertical_span: 1000.,
                        height: &h,
                        splat: &splat,
                        topology: &topo,
                    },
                    &noise,
                    || false,
                )
                .unwrap()
                .unwrap()
            };
            let a = run();
            let b = run();
            assert_eq!(a.splat, b.splat);
            assert_eq!(a.topology, b.topology);
            assert!(a.report.changed_splat_cells > 0);
            assert!(a.report.changed_topology_cells > 0);
            let middle = 32 * n + 32;
            assert_eq!(a.splat[7 * n * n + middle], 255);
            assert_ne!(a.topology[middle] & 16384, 0);
            assert!(a.topology.iter().all(|v| v & 67108864 != 0));
            assert!(a.report.bank.bank_added > 0);
            assert_eq!(a.splat[0], 255);
            assert_eq!(a.topology[0], topo[0]);
            assert_eq!(a.report.paint_callbacks, a.report.topology_callbacks);
        }
        assert_eq!(splat, original);
        assert!(topo.iter().all(|&v| v == 1 | 67108864));
    }
    #[test]
    fn texture_pass_does_not_rerun_bank_dilation_or_change_topology() {
        let h = vec![18000; 65 * 65];
        let mut splat = vec![0; 8 * 64 * 64];
        splat[..64 * 64].fill(255);
        let topo = vec![1; 64 * 64];
        let prep = prepare(&[path(37.)], &shape(), &policy(), 2_000_000, || false)
            .unwrap()
            .unwrap();
        let noise = vec![0.; prep.noise_coordinates().len()];
        let grid = |topology| Grid {
            size: 128.,
            height_resolution: 65,
            surface_resolution: 64,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &h,
            splat: &splat,
            topology,
        };
        let topology = prep
            .apply_pass(grid(&topo), &noise, Pass::Topology, || false)
            .unwrap()
            .unwrap();
        assert_eq!(topology.splat, splat);
        assert_eq!(topology.report.paint_callbacks, 0);
        assert!(topology.report.topology_callbacks > 0);
        assert!(topology.report.bank.bank_added > 0);
        let texture = prep
            .apply_pass(grid(&topology.topology), &noise, Pass::Texture, || false)
            .unwrap()
            .unwrap();
        assert_eq!(texture.topology, topology.topology);
        assert_eq!(texture.report.topology_callbacks, 0);
        assert_eq!(texture.report.bank.work, 0);
        assert_eq!(texture.report.changed_topology_cells, 0);
        assert!(texture.report.changed_splat_cells > 0);
        let topo_view = prep
            .apply_borrowed(grid(&topo), &noise, Pass::Topology, || false)
            .unwrap()
            .unwrap();
        let texture_view = prep
            .apply_borrowed(grid(&topology.topology), &noise, Pass::Texture, || false)
            .unwrap()
            .unwrap();
        assert!(matches!(topo_view.splat, Cow::Borrowed(_)));
        assert!(matches!(texture_view.topology, Cow::Borrowed(_)));
        assert_eq!(topo_view.topology, topology.topology);
        assert_eq!(texture_view.splat, texture.splat);
        let combined = prep.apply(grid(&topo), &noise, || false).unwrap().unwrap();
        assert_eq!(combined.topology, topology.topology);
        assert_eq!(combined.splat, texture.splat);
    }
    #[test]
    fn cancellation_budget_and_missing_inputs_discard_all_surface_changes() {
        let h = vec![18000; 65 * 65];
        let mut splat = vec![0; 8 * 64 * 64];
        splat[..64 * 64].fill(255);
        let topo = vec![1; 64 * 64];
        let grid = || Grid {
            size: 128.,
            height_resolution: 65,
            surface_resolution: 64,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &h,
            splat: &splat,
            topology: &topo,
        };
        let p = prepare(&[path(37.)], &shape(), &policy(), 2_000_000, || false)
            .unwrap()
            .unwrap();
        let noise = vec![0.; p.noise_coordinates().len()];
        let calls = std::cell::Cell::new(0);
        assert!(
            p.apply(grid(), &noise, || {
                calls.set(calls.get() + 1);
                calls.get() > 4200
            })
            .unwrap()
            .is_none()
        );
        let low = prepare(&[path(37.)], &shape(), &policy(), 10000, || false)
            .unwrap()
            .unwrap();
        assert!(low.apply(grid(), &noise, || false).is_err());
        assert!(p.apply(grid(), &[], || false).is_err());
        let blank = vec![0; splat.len()];
        assert!(
            p.apply(
                Grid {
                    splat: &blank,
                    ..grid()
                },
                &noise,
                || false
            )
            .is_err()
        );
        let mut bad = policy();
        bad.splat_channel = Some(8);
        assert!(prepare(&[], &shape(), &bad, 1000, || false).is_err());
        let empty = prepare(&[], &shape(), &policy(), 2_000_000, || false)
            .unwrap()
            .unwrap();
        let a = empty.apply(grid(), &[], || false).unwrap().unwrap();
        assert_eq!(a.splat, splat);
        assert_eq!(a.topology, topo);
    }
}
