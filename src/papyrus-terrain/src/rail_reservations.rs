//! Source-configured PathList placement sweep. Raster output is distinct from
//! terrain topology, geometry occupancy and a complete runtime placement map.
use crate::path_geometry::Path;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
pub const VERSION: &str = "papyrus-rail-placement-sweep-1";
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub engine: String,
    pub width: f32,
    pub sample_spacing: f32,
    pub terrain_origin_y: f32,
    pub terrain_height: f32,
    pub spline: bool,
}
impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        if self.engine != VERSION
            || !self.spline
            || !self.width.is_finite()
            || !(1. ..=128.).contains(&self.width)
            || !self.sample_spacing.is_finite()
            || !(0.125..=8.).contains(&self.sample_spacing)
            || !self.terrain_origin_y.is_finite()
            || self.terrain_origin_y.abs() > 10000.
            || !self.terrain_height.is_finite()
            || !(1. ..=20000.).contains(&self.terrain_height)
        {
            return Err("Unsupported rail placement sweep policy".into());
        }
        Ok(())
    }
}
pub struct Track<'a> {
    pub id: u64,
    pub path: &'a Path,
}
#[derive(Clone, Debug, Serialize)]
pub struct Sample {
    pub track: u64,
    pub distance: f32,
    pub center: [f32; 3],
    pub direction: [f32; 3],
    pub plane_y: f32,
    pub quad: [[i32; 2]; 4],
}
#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub tracks: usize,
    pub sweeps: usize,
    pub initial_blocked: usize,
    pub newly_blocked: usize,
    pub final_blocked: usize,
    pub cell_visits: usize,
    pub work: usize,
}
#[derive(Clone, Serialize)]
pub struct Output {
    pub raster: Vec<u8>,
    pub samples: Vec<Sample>,
    pub report: Report,
}
fn norm(v: [f32; 3]) -> Option<[f32; 3]> {
    let length = v.iter().map(|v| v * v).sum::<f32>().sqrt();
    (length > 0.00001).then(|| v.map(|v| v / length))
}
fn edge(a: [i32; 2], b: [i32; 2], q: [i32; 2]) -> i64 {
    (b[0] - a[0]) as i64 * (q[1] - a[1]) as i64 - (b[1] - a[1]) as i64 * (q[0] - a[0]) as i64
}
fn triangle(a: [i32; 2], b: [i32; 2], c: [i32; 2], q: [i32; 2]) -> bool {
    edge(a, b, q) >= 0 && edge(b, c, q) >= 0 && edge(c, a, q) >= 0
}
pub(crate) fn quad_contains(quad: [[i32; 2]; 4], point: [i32; 2]) -> bool {
    triangle(quad[0], quad[1], quad[2], point) || triangle(quad[3], quad[2], quad[1], point)
}

/// Exact integer coverage of the same two triangles, prepared for row scans.
pub(crate) struct QuadCoverage {
    x: [i64; 5],
    z: [i64; 5],
    origin: [i64; 5],
}

impl QuadCoverage {
    pub(crate) fn new([a, b, c, d]: [[i32; 2]; 4]) -> Self {
        let edges = [(a, b), (b, c), (c, a), (d, c), (b, d)];
        let x = edges.map(|(from, to)| i64::from(from[1]) - i64::from(to[1]));
        let z = edges.map(|(from, to)| i64::from(to[0]) - i64::from(from[0]));
        let origin = std::array::from_fn(|i| {
            -x[i] * i64::from(edges[i].0[0]) - z[i] * i64::from(edges[i].0[1])
        });
        Self { x, z, origin }
    }

    pub(crate) fn row(&self, z: i32) -> QuadRow<'_> {
        QuadRow {
            x: &self.x,
            base: std::array::from_fn(|i| self.origin[i] + self.z[i] * i64::from(z)),
        }
    }
}

pub(crate) struct QuadRow<'a> {
    x: &'a [i64; 5],
    base: [i64; 5],
}

impl QuadRow<'_> {
    pub(crate) fn contains(&self, x: i32) -> bool {
        let edge = |i: usize| self.base[i] + self.x[i] * i64::from(x);
        // Both triangles include the common edge, including degenerate quads.
        let diagonal = edge(1);
        (diagonal >= 0 && edge(0) >= 0 && edge(2) >= 0)
            || (diagonal <= 0 && edge(3) >= 0 && edge(4) >= 0)
    }
}
pub fn generate(
    size: f32,
    resolution: usize,
    initial: &[u8],
    tracks: &[Track<'_>],
    policy: &Policy,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    policy.validate()?;
    if cancelled() {
        return Ok(None);
    }
    let mut ids = BTreeSet::new();
    if !size.is_finite()
        || !(1. ..=10000.).contains(&size)
        || !(1..=4096).contains(&resolution)
        || resolution.checked_mul(resolution) != Some(initial.len())
        || tracks.len() > 257
        || !(1..=128_000_000).contains(&maximum_work)
        || tracks.iter().any(|t| {
            t.id == 0
                || !ids.insert(t.id)
                || t.path.length() <= 0.
                || t.path
                    .points()
                    .iter()
                    .any(|v| v[0].abs() > size / 2. || v[2].abs() > size / 2.)
        })
    {
        return Err("Invalid rail placement sweep inputs".into());
    }
    let mut work = 0;
    macro_rules! visit {
        () => {
            if cancelled() {
                return Ok(None);
            }
            if work >= maximum_work {
                return Err("Rail placement sweep work allowance reached".into());
            }
            work += 1;
        };
    }
    let mut initial_blocked = 0;
    for &value in initial {
        visit!();
        if value > 1 {
            return Err("Invalid initial placement-map cell".into());
        }
        initial_blocked += usize::from(value != 0);
    }
    let mut raster = initial.to_vec();
    let mut samples = Vec::new();
    let mut changed = 0;
    let mut cell_visits = 0;
    let radius = policy.width * 0.5;
    let grid = |v: [f32; 3]| -> [i32; 2] {
        [v[0], v[2]].map(|v| {
            (((v + size * 0.5) * (1. / size)) * resolution as f32)
                .clamp(0., (resolution - 1) as f32) as i32
        })
    };
    let rails = |point: [f32; 3], tangent: [f32; 3]| -> Result<[[f32; 3]; 2], String> {
        let horizontal =
            norm([tangent[0], 0., tangent[2]]).ok_or("Vertical rail placement sweep tangent")?;
        let side = [horizontal[2], 0., -horizontal[0]];
        Ok([
            std::array::from_fn(|i| point[i] - side[i] * radius),
            std::array::from_fn(|i| point[i] + side[i] * radius),
        ])
    };
    for track in tracks {
        let range = track.path.range();
        let start = track.path.points()[range.0];
        let tangent = track.path.tangents()[range.0];
        let mut previous = rails(start, tangent)?;
        let mut distance = 0.;
        let limit = track.path.length() + policy.sample_spacing;
        while distance < limit {
            visit!();
            if samples.len() >= 1_000_000 {
                return Err("Rail placement sweep sample allowance reached".into());
            }
            let center = track
                .path
                .point(distance)
                .map_err(|e| format!("Rail placement point failed: {e:?}"))?;
            let tangent = track
                .path
                .tangent(distance)
                .map_err(|e| {
                    let coordinate = distance / track.path.length()
                        * (track.path.points().len()-1) as f32;
                    let index = (coordinate as usize).min(track.path.points().len()-2);
                    format!("Rail placement tangent failed: {e:?}; path {}, distance {distance}, coordinate {coordinate}, controls {:?}, tangents {:?}",
                        track.id, &track.path.points()[index..=index+1],
                        &track.path.tangents()[index..=index+1])
                })?;
            let current = rails(center, tangent)?;
            let direction = norm(tangent).ok_or("Invalid rail placement direction")?;
            let quad = [
                grid(previous[0]),
                grid(previous[1]),
                grid(current[0]),
                grid(current[1]),
            ];
            let plane_y = policy.terrain_origin_y
                + ((center[1] - policy.terrain_origin_y) * (1. / policy.terrain_height))
                    * policy.terrain_height;
            let min_x = quad.iter().map(|v| v[0]).min().unwrap();
            let max_x = quad.iter().map(|v| v[0]).max().unwrap();
            let min_z = quad.iter().map(|v| v[1]).min().unwrap();
            let max_z = quad.iter().map(|v| v[1]).max().unwrap();
            for z in min_z..=max_z {
                for x in min_x..=max_x {
                    visit!();
                    cell_visits += 1;
                    if !quad_contains(quad, [x, z]) {
                        continue;
                    }
                    let world = [
                        -size * 0.5 + ((x as f32 + 0.5) / resolution as f32) * size,
                        plane_y,
                        -size * 0.5 + ((z as f32 + 0.5) / resolution as f32) * size,
                    ];
                    let difference: [f32; 3] = std::array::from_fn(|i| world[i] - center[i]);
                    let along = difference
                        .iter()
                        .zip(direction)
                        .map(|(a, b)| a * b)
                        .sum::<f32>();
                    let closest: [f32; 3] =
                        std::array::from_fn(|i| center[i] + along * direction[i]);
                    let dx = world[0] - closest[0];
                    let dz = world[2] - closest[2];
                    if (dx * dx + dz * dz).sqrt() <= radius {
                        let index = z as usize * resolution + x as usize;
                        if raster[index] == 0 {
                            raster[index] = 1;
                            changed += 1;
                        }
                    }
                }
            }
            samples.push(Sample {
                track: track.id,
                distance,
                center,
                direction,
                plane_y,
                quad,
            });
            previous = current;
            distance += policy.sample_spacing;
        }
    }
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(Output {
        raster,
        report: Report {
            engine: VERSION,
            tracks: tracks.len(),
            sweeps: samples.len(),
            initial_blocked,
            newly_blocked: changed,
            final_blocked: initial_blocked + changed,
            cell_visits,
            work,
        },
        samples,
    }))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_quad_rows_match_inclusive_triangle_coverage() {
        let points: [[i32; 2]; 9] = std::array::from_fn(|i| [i as i32 % 3, i as i32 / 3]);
        for mut configuration in 0..9_usize.pow(4) {
            let quad = std::array::from_fn(|_| {
                let point = points[configuration % 9];
                configuration /= 9;
                point
            });
            let coverage = QuadCoverage::new(quad);
            for z in -1..=3 {
                let row = coverage.row(z);
                for x in -1..=3 {
                    assert_eq!(row.contains(x), quad_contains(quad, [x, z]), "{quad:?}, [{x}, {z}]");
                }
            }
        }
    }
    fn policy() -> Policy {
        Policy {
            engine: VERSION.into(),
            width: 20.,
            sample_spacing: 1.,
            terrain_origin_y: -500.,
            terrain_height: 1000.,
            spline: true,
        }
    }
    #[test]
    fn near_opposite_sweep_tangents_keep_their_plane_and_finish_the_raster() {
        let path = Path::from_controls(
            &[[0., 0., 0.], [10., 0., 0.], [1., 0., 0.], [-10., 0., 0.1]],
            1000,
            || false,
        )
        .unwrap()
        .unwrap();
        let initial = vec![0; 16 * 16];
        let sweep = generate(
            100.,
            16,
            &initial,
            &[Track {
                id: 42,
                path: &path,
            }],
            &policy(),
            100_000,
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(sweep.report.sweeps > 20);
        assert!(sweep.report.newly_blocked > 0);
        for sample in sweep.samples {
            assert_eq!(sample.track, 42);
            assert!(sample.direction.iter().all(|v| v.is_finite()));
            assert!(sample.direction[1].abs() < 0.000001);
        }
        assert!(initial.iter().all(|&cell| cell == 0));
    }
    #[test]
    fn invalid_tangent_reports_the_path_and_local_controls_without_mutating_input() {
        let path = Path::from_controls(
            &[[0., 0., 0.], [10., 0., 0.], [1., 0., 0.], [-10., 0., 0.]],
            1000,
            || false,
        )
        .unwrap()
        .unwrap();
        let initial = vec![0; 16 * 16];
        let error = generate(
            100.,
            16,
            &initial,
            &[Track {
                id: 42,
                path: &path,
            }],
            &policy(),
            100_000,
            || false,
        )
        .err()
        .expect("Exactly opposite tangents need an explicit orientation policy");
        assert!(error.contains("path 42, distance"), "{error}");
        assert!(
            error.contains("controls") && error.contains("tangents"),
            "{error}"
        );
        assert!(initial.iter().all(|&cell| cell == 0));
    }
    #[test]
    fn quad_coverage_matches_the_installed_server_reference() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/placement-quads-bd19c4ac.json"
        ))
        .unwrap();
        assert_eq!(
            fixture["reference"],
            "installed-server-TerrainMap-ForEach-quad"
        );
        for case in fixture["cases"].as_array().unwrap() {
            let quad: [[i32; 2]; 4] =
                serde_json::from_value(case["input"]["Quad"].clone()).unwrap();
            let mut actual = Vec::new();
            for x in
                quad.iter().map(|v| v[0]).min().unwrap()..=quad.iter().map(|v| v[0]).max().unwrap()
            {
                for z in quad.iter().map(|v| v[1]).min().unwrap()
                    ..=quad.iter().map(|v| v[1]).max().unwrap()
                {
                    if quad_contains(quad, [x, z]) {
                        actual.push([x, z]);
                    }
                }
            }
            let expected: Vec<[i32; 2]> = serde_json::from_value(case["expected"].clone()).unwrap();
            assert_eq!(actual, expected, "{}", case["input"]["Name"]);
        }
    }
    #[test]
    fn straight_sweep_preserves_initial_cells_and_is_idempotent() {
        let path = Path::from_controls(&[[0., 20., -30.], [0., 20., 30.]], 1000, || false)
            .unwrap()
            .unwrap();
        let tracks = [Track { id: 1, path: &path }];
        let mut initial = vec![0; 64 * 64];
        initial[0] = 1;
        let a = generate(100., 64, &initial, &tracks, &policy(), 100000, || false)
            .unwrap()
            .unwrap();
        assert!(a.report.newly_blocked > 0);
        assert_eq!(a.raster[0], 1);
        assert_eq!(initial.iter().sum::<u8>(), 1);
        for z in 0..64 {
            for x in 0..64 {
                let wx = -50. + (x as f32 + 0.5) / 64. * 100.;
                if wx.abs() > 10. && (x != 0 || z != 0) {
                    assert_eq!(a.raster[z * 64 + x], 0);
                }
            }
        }
        let b = generate(100., 64, &a.raster, &tracks, &policy(), 100000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(a.raster, b.raster);
        assert_eq!(b.report.newly_blocked, 0);
        assert_eq!(a.samples.len(), 61);
    }
    #[test]
    fn late_cancellation_budget_and_bad_cells_never_publish_a_partial_sweep() {
        let path = Path::from_controls(&[[0., 20., -30.], [0., 21., 30.]], 1000, || false)
            .unwrap()
            .unwrap();
        let tracks = [Track { id: 1, path: &path }];
        let mut initial = vec![0; 64 * 64];
        let calls = std::cell::Cell::new(0);
        assert!(
            generate(100., 64, &initial, &tracks, &policy(), 100000, || {
                calls.set(calls.get() + 1);
                calls.get() == 4500
            })
            .unwrap()
            .is_none()
        );
        assert!(generate(100., 64, &initial, &tracks, &policy(), 4200, || false).is_err());
        assert!(initial.iter().all(|v| *v == 0));
        initial[5] = 2;
        assert!(generate(100., 64, &initial, &tracks, &policy(), 100000, || false).is_err());
    }
}
