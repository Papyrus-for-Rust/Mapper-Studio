//! Transactional source-configured river terrain sweeps over frozen per-path heights.
//! No paint, topology, water bodies or automatic installation compatibility is implied.
use crate::{path_geometry::Path, rail_reservations::quad_contains, river_layout};
use serde::{Deserialize, Serialize};
pub const VERSION: &str = "papyrus-river-carve-1";
use crate::path_terrain::{
    SHORT_TO_FLOAT, SweepLine, blend, distance_xz, inverse, lerp, nearest_sweep_height, smooth,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub vertical_iterations: u16,
    pub sample_spacing: f32,
    pub terrain_offset: f32,
    /// Initial factor, final factor, distance scale.
    pub depth_growth: [f32; 3],
    pub inner_scale: [f32; 3],
    pub intensity: f32,
    pub fade_multiplier: f32,
}
impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        if self.vertical_iterations > 256
            || ![0.125, 0.25, 0.5, 1., 2., 4., 8.].contains(&self.sample_spacing)
            || !self.terrain_offset.is_finite()
            || self.terrain_offset.abs() > 64.
            || self
                .depth_growth
                .into_iter()
                .chain(self.inner_scale)
                .any(|v| !v.is_finite() || !(0.0001..=10000.).contains(&v))
            || [self.intensity, self.fade_multiplier]
                .into_iter()
                .any(|v| !v.is_finite() || !(0. ..=1.).contains(&v))
        {
            return Err("Invalid river terrain policy".into());
        }
        Ok(())
    }
    pub fn depth(&self, distance: f32, signed_noise: f32, random_scale: f32) -> f32 {
        let depth = self.terrain_offset
            * lerp(
                self.depth_growth[0],
                self.depth_growth[1],
                distance / self.depth_growth[2],
            );
        lerp(depth, depth * random_scale, signed_noise * 0.5 + 1.)
    }
}
#[derive(Clone)]
pub struct Prepared {
    paths: Vec<Path>,
    shape: river_layout::Policy,
    policy: Policy,
    noise_count: usize,
    work: usize,
    maximum_work: usize,
}
pub struct Grid<'a> {
    pub size: f32,
    pub resolution: usize,
    pub origin_y: f32,
    pub vertical_span: f32,
    pub height: &'a [i16],
}
pub struct Output {
    pub height: Vec<i16>,
    pub paths: Vec<Path>,
    pub report: Report,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub paths: usize,
    pub frozen_source_passes: usize,
    pub sweeps: usize,
    pub covered_visits: usize,
    pub height_writes: usize,
    pub changed_samples: usize,
    pub maximum_raise: f32,
    pub maximum_lower: f32,
    pub work: usize,
}
/// Smooth all vertical controls first, without resampling XZ or changing order.
/// Resource limits cover preparation and subsequent terrain work together.
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
    if paths.len() > 4096
        || !(1..=512_000_000).contains(&maximum_work)
        || paths.iter().map(|p| p.points().len()).sum::<usize>() > 1_048_576
        || paths.iter().any(|p| {
            p.length() <= 0.
                || !p.length().is_finite()
                || p.length() > 40000.
                || p.range() != (0, p.points().len() - 1)
        })
    {
        return Err("Invalid river carving paths or work limit".into());
    }
    let mut smoothed = Vec::with_capacity(paths.len());
    let mut work = 0;
    for path in paths {
        if cancelled() {
            return Ok(None);
        }
        let remaining = (maximum_work - work).min(32_000_000);
        if remaining == 0 {
            return Err("River preparation work budget exceeded".into());
        }
        let heights = path.points().iter().map(|p| p[1]).collect::<Vec<_>>();
        let weights = vec![1.; heights.len()];
        let Some(path) = path
            .refit_heights(
                &heights,
                policy.vertical_iterations,
                &weights,
                remaining,
                &cancelled,
            )
            .map_err(|e| format!("River vertical smoothing failed: {e:?}"))?
        else {
            return Ok(None);
        };
        work += path.work();
        smoothed.push(path);
    }
    let noise_count = smoothed
        .iter()
        .map(|p| ((p.length() + policy.sample_spacing) / policy.sample_spacing).ceil() as usize)
        .max()
        .unwrap_or(0);
    if noise_count > 1_000_000 {
        return Err("River carving noise/sample budget exceeded".into());
    }
    Ok((!cancelled()).then_some(Prepared {
        paths: smoothed,
        shape: shape.clone(),
        policy: policy.clone(),
        noise_count,
        work,
        maximum_work,
    }))
}
impl Prepared {
    pub fn noise_coordinates(&self) -> Vec<f32> {
        (0..self.noise_count)
            .map(|i| i as f32 * self.policy.sample_spacing * self.shape.noise_frequency)
            .collect()
    }
    pub fn paths(&self) -> &[Path] {
        &self.paths
    }
    pub fn carve(
        &self,
        grid: Grid<'_>,
        noise: &[f32],
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Output>, String> {
        if cancelled() {
            return Ok(None);
        }
        let n = grid.resolution;
        if !(2..=4097).contains(&n)
            || n.checked_mul(n) != Some(grid.height.len())
            || !grid.size.is_finite()
            || !(1. ..=6000.).contains(&grid.size)
            || !grid.origin_y.is_finite()
            || grid.origin_y.abs() > 10000.
            || !grid.vertical_span.is_finite()
            || !(1. ..=20000.).contains(&grid.vertical_span)
            || noise.len() != self.noise_count
            || noise.iter().any(|v| !v.is_finite() || v.abs() > 2.)
        {
            return Err("Invalid river carving grid or signed-noise batch".into());
        }
        let mut work = self.work;
        macro_rules! visit {
            ($count:expr) => {
                if cancelled() {
                    return Ok(None);
                }
                if $count > self.maximum_work - work {
                    return Err("River terrain work budget exceeded".into());
                }
                work += $count;
            };
        }
        for chunk in grid.height.chunks(4096) {
            visit!(chunk.len());
            if chunk.iter().any(|h| !(0..=32766).contains(h)) {
                return Err("Invalid source river terrain heights".into());
            }
        }
        let mut current = grid.height.to_vec();
        let mut destination = Vec::new();
        let mut report = Report {
            engine: VERSION,
            paths: self.paths.len(),
            frozen_source_passes: 0,
            sweeps: 0,
            covered_visits: 0,
            height_writes: 0,
            changed_samples: 0,
            maximum_raise: 0.,
            maximum_lower: 0.,
            work: 0,
        };
        let index = |p: [f32; 3]| {
            [p[0], p[2]].map(|v| {
                (((v + grid.size * 0.5) * (1. / grid.size)) * n as f32).clamp(0., (n - 1) as f32)
                    as i32
            })
        };
        let edges =
            |point: [f32; 3], tangent: [f32; 3], radius: f32| -> Result<[[f32; 3]; 2], String> {
                let length = (tangent[0] * tangent[0] + tangent[2] * tangent[2]).sqrt();
                if length < 0.00001 {
                    return Err("Vertical river sweep tangent is unsupported".into());
                }
                let side = [tangent[2] / length, 0., -tangent[0] / length];
                Ok([
                    std::array::from_fn(|i| point[i] - side[i] * radius),
                    std::array::from_fn(|i| point[i] + side[i] * radius),
                ])
            };
        let inv_height = 1. / grid.vertical_span;
        for path in self.paths.iter().rev() {
            visit!(current.len());
            destination.clone_from(&current);
            report.frozen_source_passes += 1;
            let start = path.points()[0];
            let end = *path.points().last().expect("validated path");
            let mut point = start;
            let mut tangent = path.tangents()[0];
            let mut previous_line = SweepLine::new(point, tangent, self.policy.sample_spacing);
            let mut current_line = previous_line;
            let base_radius = self.shape.width * 0.5;
            let mut previous_edges = edges(
                point,
                tangent,
                base_radius + self.shape.padding[1] + self.shape.fade[1],
            )?;
            let count = ((path.length() + self.policy.sample_spacing) / self.policy.sample_spacing)
                .ceil() as usize;
            for (step, &signed) in noise.iter().take(count).enumerate() {
                visit!(1);
                let distance = step as f32 * self.policy.sample_spacing;
                let next = path
                    .point(distance + self.policy.sample_spacing)
                    .map_err(|e| format!("River sweep position failed: {e:?}"))?;
                let next_tangent = path
                    .tangent(distance + self.policy.sample_spacing)
                    .map_err(|e| format!("River sweep tangent failed: {e:?}"))?;
                // Fail before evaluating a vertical line's horizontal divisor.
                edges(next, next_tangent, 0.)?;
                let next_line = SweepLine::new(next, next_tangent, self.policy.sample_spacing);
                let radius = self.shape.radius(distance, path.length(), signed);
                let depth = self.policy.depth(distance, signed, self.shape.random_scale);
                let opacity = if path.is_circular() {
                    1.
                } else {
                    inverse(
                        0.,
                        base_radius,
                        distance_xz(start, point).min(distance_xz(end, point)),
                    )
                };
                let current_edges = edges(
                    point,
                    tangent,
                    radius + self.shape.padding[1] + self.shape.fade[1],
                )?;
                let quad = [
                    index(previous_edges[0]),
                    index(previous_edges[1]),
                    index(current_edges[0]),
                    index(current_edges[1]),
                ];
                let min_x = quad.iter().map(|q| q[0]).min().unwrap();
                let max_x = quad.iter().map(|q| q[0]).max().unwrap();
                let min_z = quad.iter().map(|q| q[1]).min().unwrap();
                let max_z = quad.iter().map(|q| q[1]).max().unwrap();
                report.sweeps += 1;
                for z in min_z..=max_z {
                    for x in min_x..=max_x {
                        visit!(1);
                        if !quad_contains(quad, [x, z]) {
                            continue;
                        }
                        report.covered_visits += 1;
                        let world = [
                            -grid.size * 0.5 + ((x as f32 + 0.5) / n as f32) * grid.size,
                            0.,
                            -grid.size * 0.5 + ((z as f32 + 0.5) / n as f32) * grid.size,
                        ];
                        let (distance, nearest_height) =
                            nearest_sweep_height([previous_line, current_line, next_line], world);
                        let fade = inverse(
                            radius
                                + self.shape.padding[1]
                                + self.shape.fade[1] * self.policy.fade_multiplier,
                            radius + self.shape.padding[1],
                            distance,
                        );
                        let alpha = self.policy.intensity * opacity * fade;
                        if alpha <= 0. {
                            continue;
                        }
                        let inner = lerp(
                            self.policy.inner_scale[0],
                            self.policy.inner_scale[1],
                            step as f32 * self.policy.sample_spacing / self.policy.inner_scale[2],
                        );
                        let cut = inverse(
                            radius - self.shape.padding[0] * inner,
                            radius - self.shape.padding[0] * inner - self.shape.fade[0] * inner,
                            distance,
                        );
                        let target = (nearest_height - grid.origin_y) * inv_height
                            + smooth(0., depth * inv_height, cut);
                        if !target.is_finite() || !(0. ..=1.).contains(&target) {
                            return Err(
                                "River target exceeds encodable terrain height range".into()
                            );
                        }
                        let i = z as usize * n + x as usize;
                        // Every swept visit reads this path pass's frozen source.
                        // Later visits replace earlier destination writes, not a max
                        // merge or iterative blend. The quad's triangle union
                        // emits one callback per covered cell.
                        destination[i] = blend(current[i], target, alpha);
                        report.height_writes += 1;
                    }
                }
                previous_edges = current_edges;
                previous_line = current_line;
                current_line = next_line;
                point = next;
                tangent = next_tangent;
            }
            std::mem::swap(&mut current, &mut destination);
        }
        for (a, b) in current.chunks(4096).zip(grid.height.chunks(4096)) {
            visit!(a.len());
            for (&after, &before) in a.iter().zip(b) {
                report.changed_samples += usize::from(after != before);
                let difference = (i32::from(after) - i32::from(before)) as f32
                    * SHORT_TO_FLOAT
                    * grid.vertical_span;
                report.maximum_raise = report.maximum_raise.max(difference);
                report.maximum_lower = report.maximum_lower.max(-difference);
            }
        }
        visit!(self.paths.iter().map(|p| p.points().len()).sum::<usize>());
        report.work = work;
        Ok(Some(Output {
            height: current,
            paths: self.paths.clone(),
            report,
        }))
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
        serde_json::from_value(v["policy"].clone()).unwrap()
    }
    fn policy() -> Policy {
        let v: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/rivers/server-bd19c4ac-terrain.json"
        ))
        .unwrap();
        serde_json::from_value(v["policy"].clone()).unwrap()
    }
    fn path(y: f32, yaw: f32) -> Path {
        let (s, c) = yaw.to_radians().sin_cos();
        let p = [-24., -8., 8., 24.].map(|x| [x * c, y, x * s]);
        Path::from_controls(&p, 1000, || false).unwrap().unwrap()
    }
    fn grid(height: &[i16], n: usize) -> Grid<'_> {
        Grid {
            size: 128.,
            resolution: n,
            origin_y: -500.,
            vertical_span: 1000.,
            height,
        }
    }

    #[test]
    fn numerical_primitives_match_actual_owned_server_methods() {
        #[derive(Deserialize)]
        struct LineCase {
            start: [f32; 3],
            end: [f32; 3],
            query: [f32; 3],
            expected: [f32; 3],
        }
        #[derive(Deserialize)]
        struct Blend {
            source: i16,
            target: f32,
            opacity: f32,
            expected: i16,
        }
        #[derive(Deserialize)]
        struct Depth {
            distance: f32,
            signed_noise: f32,
            expected: f32,
        }
        #[derive(Deserialize)]
        struct Source {
            file: String,
            sha256: String,
        }
        #[derive(Deserialize)]
        struct Reference {
            sources: Vec<Source>,
            lines: Vec<LineCase>,
            blends: Vec<Blend>,
            depths: Vec<Depth>,
        }
        let r: Reference = serde_json::from_str(include_str!(
            "../tests/fixtures/river-carve-primitives-bd19c4ac.json"
        ))
        .unwrap();
        assert_eq!(r.sources.len(), 5);
        assert!(r.sources.iter().any(|s| s.file == "Assembly-CSharp.dll"
            && s.sha256 == "bd19c4ac705edd18ff556f5dbb3130e098b380577cc2ab0d73a2ec89ee289cc9"));
        assert_eq!(r.lines.len(), 25);
        assert_eq!(r.blends.len(), 210);
        assert_eq!(r.depths.len(), 24);
        for c in r.lines {
            let got = crate::path_terrain::closest([c.start, c.end], c.query);
            for (got, expected) in got.into_iter().zip(c.expected) {
                assert!((got - expected).abs() < 0.00001, "{got} vs {expected}");
            }
        }
        for c in r.blends {
            assert_eq!(blend(c.source, c.target, c.opacity), c.expected);
        }
        let p = policy();
        for c in r.depths {
            assert!((p.depth(c.distance, c.signed_noise, 0.75) - c.expected).abs() < 0.000001);
        }
    }

    #[test]
    fn overlapping_sweeps_read_frozen_source_but_each_path_has_a_new_snapshot() {
        let mut p = policy();
        p.terrain_offset = 0.;
        p.intensity = 0.1;
        let line = path(220., 0.);
        let prepared = prepare(std::slice::from_ref(&line), &shape(), &p, 2_000_000, || {
            false
        })
        .unwrap()
        .unwrap();
        let noise = vec![0.; prepared.noise_coordinates().len()];
        let before = vec![18000; 33 * 33];
        let centre = 16 * 33 + 16;
        let result = prepared
            .carve(grid(&before, 33), &noise, || false)
            .unwrap()
            .unwrap();
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/river-carve-primitives-bd19c4ac.json"
        ))
        .unwrap();
        let expected = reference["blends"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| {
                v["source"] == 18000
                    && (v["target"].as_f64().unwrap() - 0.72).abs() < 0.00001
                    && (v["opacity"].as_f64().unwrap() - 0.1).abs() < 0.00001
            })
            .unwrap()["expected"]
            .as_i64()
            .unwrap() as i16;
        assert_eq!(result.height[centre], expected);
        assert!(result.report.height_writes > result.report.changed_samples);
        assert_eq!(result.report.frozen_source_passes, 1);
        let twice = prepare(&[line.clone(), line], &shape(), &p, 2_000_000, || false)
            .unwrap()
            .unwrap();
        let result2 = twice
            .carve(grid(&before, 33), &noise, || false)
            .unwrap()
            .unwrap();
        assert_eq!(result2.height[centre], blend(expected, 0.72, 0.1));
        assert_ne!(result2.height[centre], result.height[centre]);
        assert_eq!(result2.report.frozen_source_passes, 2);
        assert!(before.iter().all(|&h| h == 18000));
    }

    #[test]
    fn carving_cuts_at_arbitrary_yaw_preserves_other_cells_and_replays() {
        let original = vec![crate::erosion::encode(100.); 129 * 129];
        for yaw in [0., 37., 123., 271.] {
            let line = path(100., yaw);
            let prepared = prepare(
                std::slice::from_ref(&line),
                &shape(),
                &policy(),
                2_000_000,
                || false,
            )
            .unwrap()
            .unwrap();
            let noise = vec![0.; prepared.noise_coordinates().len()];
            let a = prepared
                .carve(grid(&original, 129), &noise, || false)
                .unwrap()
                .unwrap();
            let b = prepared
                .carve(grid(&original, 129), &noise, || false)
                .unwrap()
                .unwrap();
            assert_eq!(a.height, b.height);
            assert!(a.report.changed_samples > 0);
            assert!(a.report.maximum_lower > 1.);
            assert!(crate::erosion::decode(a.height[64 * 129 + 64]) < 100.);
            assert!(a.report.maximum_raise < 0.1);
            assert_eq!(a.paths[0].points().len(), line.points().len());
            for (before, after) in line.points().iter().zip(a.paths[0].points()) {
                assert_eq!([before[0], before[2]], [after[0], after[2]]);
            }
            // At least the corner outside this short path's sweep remains intact.
            assert!(a.height.iter().zip(&original).any(|(a, b)| a == b));
        }
    }

    #[test]
    fn reverse_path_order_is_preserved_and_vertical_smoothing_does_not_resample() {
        let before = vec![18000; 33 * 33];
        let mut p = policy();
        p.terrain_offset = 0.;
        let a = path(100., 0.);
        let b = path(200., 0.);
        let prep = |paths: &[Path]| {
            prepare(paths, &shape(), &p, 2_000_000, || false)
                .unwrap()
                .unwrap()
        };
        let first = prep(&[a.clone(), b.clone()]);
        let second = prep(&[b, a]);
        let noise = vec![0.; first.noise_coordinates().len()];
        let one = first
            .carve(grid(&before, 33), &noise, || false)
            .unwrap()
            .unwrap();
        let two = second
            .carve(grid(&before, 33), &noise, || false)
            .unwrap()
            .unwrap();
        assert_eq!(one.height[16 * 33 + 16], blend(18000, 0.6, 1.));
        assert_eq!(two.height[16 * 33 + 16], blend(18000, 0.7, 1.));
        let controls = [
            [-24., 80., 0.],
            [-8., 100., 0.],
            [8., 30., 0.],
            [24., 20., 0.],
        ];
        let rough = Path::from_controls(&controls, 1000, || false)
            .unwrap()
            .unwrap();
        let smoothed = prep(&[rough]);
        assert_eq!(smoothed.paths()[0].points().len(), 4);
        assert_ne!(smoothed.paths()[0].points()[1][1], controls[1][1]);
        assert_eq!(smoothed.paths()[0].points()[0], controls[0]);
        assert_eq!(smoothed.paths()[0].points()[3], controls[3]);
    }

    #[test]
    fn cancellation_budgets_and_bad_geometry_cannot_publish_partial_terrain() {
        let before = vec![18000; 129 * 129];
        let p = policy();
        let line = path(100., 0.);
        let prepared = prepare(std::slice::from_ref(&line), &shape(), &p, 2_000_000, || {
            false
        })
        .unwrap()
        .unwrap();
        let noise = vec![0.; prepared.noise_coordinates().len()];
        let calls = std::cell::Cell::new(0);
        assert!(
            prepared
                .carve(grid(&before, 129), &noise, || {
                    calls.set(calls.get() + 1);
                    calls.get() > 200
                })
                .unwrap()
                .is_none()
        );
        let limited = prepare(std::slice::from_ref(&line), &shape(), &p, 1000, || false)
            .unwrap()
            .unwrap();
        assert!(limited.carve(grid(&before, 129), &noise, || false).is_err());
        let mut invalid_noise = noise.clone();
        invalid_noise[0] = f32::NAN;
        assert!(
            prepared
                .carve(grid(&before, 129), &invalid_noise, || false)
                .is_err()
        );
        let high = prepare(&[path(700., 0.)], &shape(), &p, 2_000_000, || false)
            .unwrap()
            .unwrap();
        assert!(high.carve(grid(&before, 129), &noise, || false).is_err());
        let vertical = Path::from_controls(&[[0., 0., 0.], [0., 100., 0.]], 1000, || false)
            .unwrap()
            .unwrap();
        let vertical = prepare(&[vertical], &shape(), &p, 2_000_000, || false)
            .unwrap()
            .unwrap();
        assert!(
            vertical
                .carve(
                    grid(&before, 129),
                    &vec![0.; vertical.noise_coordinates().len()],
                    || false
                )
                .is_err()
        );
        assert!(before.iter().all(|&h| h == 18000));
        let empty = prepare(&[], &shape(), &p, 100000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(empty.noise_coordinates().len(), 0);
        assert_eq!(
            empty
                .carve(grid(&before, 129), &[], || false)
                .unwrap()
                .unwrap()
                .height,
            before
        );
    }
}
