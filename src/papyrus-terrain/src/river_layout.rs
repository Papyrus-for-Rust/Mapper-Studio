//! Source-configured river candidates over immutable native terrain.
//! This does not carve, paint, instantiate water or claim Unity floating-point parity.
use crate::{
    native_layers::{Layers, View}, path_geometry, rail_costmap::sample_height_normal, topology_query,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const VERSION: &str = "papyrus-river-layout-1";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// Distance from the centre, distance from the edge, scan spacing.
    pub scan: [f32; 3],
    pub minimum_start_height: f32,
    pub start_normal_y: [f32; 2],
    pub separation: f32,
    pub step: f32,
    pub maximum_steps: usize,
    pub control_stride: usize,
    pub self_history_skip: usize,
    pub self_clearance: f32,
    pub maximum_rise: f32,
    /// Inland blend, shore blend, shore transition height.
    pub height_blend: [f32; 3],
    /// Steep angle, shallow angle, maximum bend degrees, angular frequency.
    pub bend: [f32; 4],
    pub direction_blend: f32,
    pub width: f32,
    pub padding: [f32; 2],
    pub fade: [f32; 2],
    pub random_scale: f32,
    /// Initial factor, final factor, distance scale.
    pub radius_growth: [f32; 3],
    /// Mouth factor, upstream factor, remaining-length scale.
    pub radius_mouth: [f32; 3],
    pub noise_frequency: f32,
    pub blocked_topology: u32,
    pub ocean_topology: u32,
    pub minimum_controls: usize,
    pub maximum_rivers: usize,
    pub exempt_biome: u32,
    pub occupancy_spacing: f32,
    pub smoothing: [u16; 2],
    pub resample_distance: f32,
}
impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        let positive = self.scan.into_iter().chain([
            self.minimum_start_height,
            self.separation,
            self.step,
            self.self_clearance,
            self.maximum_rise,
            self.height_blend[2],
            self.width,
            self.radius_growth[0],
            self.radius_growth[1],
            self.radius_growth[2],
            self.radius_mouth[0],
            self.radius_mouth[1],
            self.radius_mouth[2],
            self.noise_frequency,
            self.occupancy_spacing,
            self.resample_distance,
        ]);
        if positive
            .into_iter()
            .any(|v| !v.is_finite() || !(0.0001..=10000.).contains(&v))
            || self
                .padding
                .into_iter()
                .chain(self.fade)
                .any(|v| !v.is_finite() || !(0. ..=256.).contains(&v))
            || self
                .bend
                .iter()
                .any(|v| !v.is_finite() || !(0. ..=180.).contains(v))
            || self.bend[0] <= self.bend[1]
            || [
                self.start_normal_y[0],
                self.start_normal_y[1],
                self.height_blend[0],
                self.height_blend[1],
                self.direction_blend,
                self.random_scale,
            ]
            .iter()
            .any(|v| !v.is_finite() || !(0. ..=1.).contains(v))
            || self.start_normal_y[0] >= self.start_normal_y[1]
            || !(1..=10000).contains(&self.maximum_steps)
            || !(1..=64).contains(&self.control_stride)
            || self.self_history_skip > 1024
            || !(2..=10000).contains(&self.minimum_controls)
            || self.maximum_rivers > 64
            || self.blocked_topology == 0
            || self.ocean_topology.count_ones() != 1
            || self.blocked_topology & self.ocean_topology != 0
            || self.exempt_biome.count_ones() != 1
            || self.exempt_biome > 16
            || self.smoothing.iter().any(|&n| n > 256)
            || !(0.1..=100.).contains(&self.resample_distance)
            || self.scan[2] < 1.
            || self.occupancy_spacing < 1.
        {
            return Err("Invalid river layout policy".into());
        }
        Ok(())
    }
    /// Exact requested signed-noise coordinates; reuse the same batch across candidates.
    pub fn noise_coordinates(&self) -> Result<Vec<f32>, String> {
        self.validate()?;
        Ok((0..self.maximum_steps)
            .map(|i| i as f32 * self.step * self.noise_frequency)
            .collect())
    }
    pub fn radius(&self, distance: f32, length: f32, signed_noise: f32) -> f32 {
        let growth = lerp(
            self.radius_growth[0],
            self.radius_growth[1],
            distance / self.radius_growth[2],
        );
        let mouth = if length > 0. {
            lerp(
                self.radius_mouth[0],
                self.radius_mouth[1],
                ((length - distance).max(0.) / self.radius_mouth[2]).sqrt(),
            )
        } else {
            1.
        };
        let radius = self.width * 0.5 * growth * mouth;
        // The inspected Noise.SimplexUnsigned is signed * .5 + 1, followed by
        // Mathf.Lerp's clamping. It is deliberately not the usual +.5 mapping.
        lerp(radius, radius * self.random_scale, signed_noise * 0.5 + 1.)
    }
}

pub struct Settings {
    /// None retains source scan order. Some permutes candidate scan order using
    /// a stable Mapper algorithm, providing an explicit independent river seed.
    pub seed: Option<u32>,
    pub maximum_work: usize,
    pub maximum_candidates: usize,
    pub maximum_controls: usize,
}
#[derive(Serialize)]
pub struct Layout {
    pub paths: Vec<path_geometry::Path>,
    pub report: Report,
}
#[derive(Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub seed: Option<u32>,
    pub starts_scanned: usize,
    pub traces: usize,
    pub candidates: usize,
    pub selected: usize,
    pub work: usize,
    pub terminations: BTreeMap<&'static str, usize>,
    pub terrain_applied: bool,
}
enum Stop {
    Cancelled,
    Failed(String),
}
type ResultT<T> = Result<T, Stop>;
struct Work<'a> {
    used: usize,
    limit: usize,
    cancelled: &'a dyn Fn() -> bool,
}
impl Work<'_> {
    fn charge(&mut self, n: usize) -> ResultT<()> {
        if (self.cancelled)() {
            return Err(Stop::Cancelled);
        }
        if n > self.limit - self.used {
            return Err(Stop::Failed("River layout work budget exceeded".into()));
        }
        self.used += n;
        Ok(())
    }
}
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0., 1.)
}
fn inv(a: f32, b: f32, v: f32) -> f32 {
    ((v - a) / (b - a)).clamp(0., 1.)
}
fn normalized(v: [f32; 2]) -> [f32; 2] {
    let length = (v[0] * v[0] + v[1] * v[1]).sqrt();
    if length > 0.00001 {
        v.map(|v| v / length)
    } else {
        [0.; 2]
    }
}
fn distance(a: [f32; 3], b: [f32; 3], full: bool) -> f32 {
    (a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2) + if full { (a[1] - b[1]).powi(2) } else { 0. }
}
fn uv(l: &View<'_>, p: [f32; 3]) -> [f32; 2] {
    [p[0], p[2]].map(|v| (v / l.size as f32 + 0.5).clamp(0., 1.))
}
fn index(n: usize, v: f32) -> usize {
    (v * n as f32).clamp(0., (n - 1) as f32) as usize
}
fn topology(l: &View<'_>, p: [f32; 3]) -> u32 {
    let [x, z] = uv(l, p).map(|v| index(l.surface_resolution, v));
    l.topology[z * l.surface_resolution + x] as u32
}
fn height_normal(l: &View<'_>, p: [f32; 3]) -> (f32, [f32; 3]) {
    sample_height_normal(l.size as f32, l.height_resolution, l.height, uv(l, p))
}
fn closest(a: [f32; 3], b: [f32; 3], p: [f32; 3]) -> [f32; 3] {
    let d: [f32; 3] = std::array::from_fn(|i| b[i] - a[i]);
    let norm = d.iter().map(|v| v * v).sum::<f32>();
    let t = if norm > 0. {
        (d.iter()
            .enumerate()
            .map(|(i, v)| v * (p[i] - a[i]))
            .sum::<f32>()
            / norm)
            .clamp(0., 1.)
    } else {
        0.
    };
    std::array::from_fn(|i| a[i] + d[i] * t)
}
fn blocked(
    l: &View<'_>,
    tiles: topology_query::Tiles<'_>,
    p: [f32; 3],
    radius: f32,
    mask: u32,
    work: &mut Work<'_>,
) -> ResultT<bool> {
    work.charge(1)?;
    let n = l.surface_resolution;
    let uv = uv(l, p);
    let r = radius / l.size as f32;
    let [x, z] = uv.map(|v| index(n, v));
    let [x0, z0] = uv.map(|v| index(n, v - r));
    let [x1, z1] = uv.map(|v| index(n, v + r));
    // Preserve the inspected topology query's mixed-unit circular boundary.
    tiles
        .union(
            topology_query::Query {
                rect: [x0, x1, z0, z1],
                center: [x, z],
                radius,
                relevant: mask,
            },
            &mut work.used,
            work.limit,
        )
        .map(|m| m != 0)
        .map_err(|_| Stop::Failed("River topology query work budget exceeded".into()))
}
fn trace(
    l: &View<'_>,
    tiles: topology_query::Tiles<'_>,
    policy: &Policy,
    noise: &[f32],
    mut p: [f32; 3],
    mut normal: [f32; 3],
    work: &mut Work<'_>,
) -> ResultT<(Option<Vec<[f32; 3]>>, &'static str)> {
    let mut controls = vec![p];
    let mut direction = normalized([normal[0], normal[2]]);
    let mut previous_height = p[1];
    let mut ocean_steps = 0;
    for (k, &noise) in noise.iter().enumerate() {
        work.charge(1)?;
        let distance_along = k as f32 * policy.step;
        let slope = normal[1].clamp(-1., 1.).acos().to_degrees();
        let bend = (distance_along.to_radians() * policy.bend[3]).sin()
            * inv(policy.bend[0], policy.bend[1], slope)
            * policy.bend[2];
        let (s, c) = bend.to_radians().sin_cos();
        let forward = [
            direction[0] * c - direction[1] * s,
            direction[0] * s + direction[1] * c,
        ];
        p[0] += forward[0] * policy.step;
        p[2] += forward[1] * policy.step;
        for pair in controls
            .windows(2)
            .take(controls.len().saturating_sub(policy.self_history_skip))
        {
            work.charge(1)?;
            if distance(p, closest(pair[0], pair[1], p), false) < policy.self_clearance.powi(2) {
                return Ok((None, "self-intersection"));
            }
        }
        work.charge(8)?;
        let height = height_normal(l, p).0;
        if height > previous_height + policy.maximum_rise {
            return Ok((None, "uphill"));
        }
        let next_height = height.min(previous_height);
        let blend = lerp(
            policy.height_blend[0],
            policy.height_blend[1],
            inv(policy.height_blend[2], 0., next_height),
        );
        p[1] = lerp(p[1], next_height, blend);
        let radius = policy.radius(distance_along, 0., noise);
        let mouth_radius = policy.radius(distance_along, distance_along, noise);
        let reach = mouth_radius + policy.padding[1] + policy.fade[1];
        if blocked(
            l,
            tiles,
            p,
            radius + policy.padding[1] + policy.fade[1],
            policy.blocked_topology,
            work,
        )? {
            return Ok((None, "blocked-topology"));
        }
        // Unity yaw +90 turns +X toward -Z. Keep the source's asymmetric
        // forward probes and all four side probes, rather than an AABB test.
        let side = [forward[1], -forward[0]];
        let mut common = topology(l, p);
        for (axis, d) in [
            (forward, -radius * 1.5),
            (forward, radius * 1.5),
            (forward, reach),
            (side, -radius * 1.5),
            (side, -reach),
            (side, radius * 1.5),
            (side, reach),
        ] {
            work.charge(1)?;
            common &= topology(l, [p[0] + axis[0] * d, p[1], p[2] + axis[1] * d]);
        }
        if common & policy.ocean_topology != 0 {
            controls.push(p);
            return Ok(if controls.len() >= policy.minimum_controls {
                (Some(controls), "mouth")
            } else {
                (None, "short")
            });
        }
        if topology(l, p) & policy.ocean_topology != 0 || p[1] < 0. {
            let allowed = (mouth_radius / policy.step).round_ties_even() as usize;
            if ocean_steps >= allowed {
                return Ok((None, "incomplete-mouth"));
            }
            ocean_steps += 1;
        } else if ocean_steps > 0 {
            return Ok((None, "left-ocean"));
        }
        if k % policy.control_stride == 0 {
            controls.push(p);
        }
        work.charge(8)?;
        normal = height_normal(l, p).1;
        let desired = normalized([normal[0], normal[2]]);
        direction = normalized([
            lerp(direction[0], desired[0], policy.direction_blend),
            lerp(direction[1], desired[1], policy.direction_blend),
        ]);
        previous_height = next_height;
    }
    Ok((None, "step-limit"))
}

/// All output is private until the entire scan, selection and smoothing succeed.
/// The signed noise batch must correspond exactly to Policy::noise_coordinates.
pub fn generate(
    l: &Layers,
    policy: &Policy,
    noise: &[f32],
    settings: &Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Layout>, String> {
    generate_view(&l.view(), policy, noise, settings, cancelled)
}

pub(crate) fn generate_view(
    l: &View<'_>,
    policy: &Policy,
    noise: &[f32],
    settings: &Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Layout>, String> {
    if cancelled() {
        return Ok(None);
    }
    policy.validate()?;
    let (hr, n) = crate::native_layers::dimensions(l.size)?;
    if l.height_resolution != hr
        || l.surface_resolution != n
        || l.height.len() != hr * hr
        || l.topology.len() != n * n
        || l.biome.len() != n * n * 5
        || noise.len() != policy.maximum_steps
        || noise.iter().any(|v| !v.is_finite() || v.abs() > 2.)
        || !(1..=512_000_000).contains(&settings.maximum_work)
        || !(1..=4096).contains(&settings.maximum_candidates)
        || !(2..=1_048_576).contains(&settings.maximum_controls)
        || settings.seed == Some(0)
    {
        return Err("Invalid river terrain, noise or resource limits".into());
    }
    let mut work = Work {
        used: 0,
        limit: settings.maximum_work,
        cancelled: &cancelled,
    };
    match build(l, policy, noise, settings, &mut work) {
        Ok(layout) => Ok(Some(layout)),
        Err(Stop::Cancelled) => Ok(None),
        Err(Stop::Failed(e)) => Err(e),
    }
}
fn build(
    l: &View<'_>,
    policy: &Policy,
    noise: &[f32],
    settings: &Settings,
    work: &mut Work<'_>,
) -> ResultT<Layout> {
    for chunk in l.height.chunks(4096) {
        work.charge(chunk.len())?;
        if chunk.iter().any(|h| !(0..=32766).contains(h)) {
            return Err(Stop::Failed("Invalid river source heights".into()));
        }
    }
    work.charge(l.topology.len())?;
    let tiles = topology_query::Index::new(l.topology, l.surface_resolution, &work.cancelled)
        .map_err(Stop::Failed)?
        .ok_or(Stop::Cancelled)?;
    let tiles = tiles.view(l.topology);
    let span = l.size as f32 * 0.5 - policy.scan[1] - policy.scan[0];
    let side = (span / policy.scan[2]).ceil().max(0.) as usize;
    let total = side * side * 4;
    if total > 1_048_576 {
        return Err(Stop::Failed(
            "River scan exceeds candidate-cell limit".into(),
        ));
    }
    let mut report = Report {
        engine: VERSION,
        seed: settings.seed,
        starts_scanned: 0,
        traces: 0,
        candidates: 0,
        selected: 0,
        work: 0,
        terminations: BTreeMap::new(),
        terrain_applied: false,
    };
    let mut candidates: Vec<Vec<[f32; 3]>> = Vec::new();
    let mut stored_controls = 0;
    let (mut at, stride) = permutation(total, settings.seed);
    for _ in 0..total {
        work.charge(1)?;
        let candidate = at;
        at = (at + stride) % total;
        report.starts_scanned += 1;
        let quadrant = candidate % 4;
        let x = policy.scan[0] + (candidate / 4 % side) as f32 * policy.scan[2];
        let z = policy.scan[0] + (candidate / 4 / side) as f32 * policy.scan[2];
        let mut p = [
            if quadrant < 2 { -x } else { x },
            0.,
            if quadrant % 2 == 0 { -z } else { z },
        ];
        work.charge(8)?;
        let (h, normal) = height_normal(l, p);
        p[1] = h;
        if h <= policy.minimum_start_height
            || normal[1] <= policy.start_normal_y[0]
            || normal[1] >= policy.start_normal_y[1]
        {
            continue;
        }
        let mut near = false;
        'near: for existing in &candidates {
            for &point in existing {
                work.charge(1)?;
                if distance(p, point, false) < policy.separation.powi(2) {
                    near = true;
                    break 'near;
                }
            }
        }
        if near {
            continue;
        }
        report.traces += 1;
        let (controls, reason) = trace(l, tiles, policy, noise, p, normal, work)?;
        *report.terminations.entry(reason).or_default() += 1;
        if let Some(controls) = controls {
            if candidates.len() >= settings.maximum_candidates
                || controls.len() > settings.maximum_controls - stored_controls
            {
                return Err(Stop::Failed(
                    "River candidate/control budget exceeded".into(),
                ));
            }
            stored_controls += controls.len();
            candidates.push(controls);
        }
    }
    report.candidates = candidates.len();
    work.charge(
        candidates
            .len()
            .saturating_mul(candidates.len().max(1).ilog2() as usize + 1),
    )?;
    // Source uses an unspecified unstable tie ordering. Mapper deliberately
    // preserves scan order for equal control counts, making replay explicit.
    candidates.sort_by_key(|p| std::cmp::Reverse(p.len()));
    let occupancy_side = (l.size as f32 / policy.occupancy_spacing) as usize;
    if occupancy_side == 0 || occupancy_side > 2048 {
        return Err(Stop::Failed("Invalid river occupancy resolution".into()));
    }
    work.charge(occupancy_side * occupancy_side)?;
    let mut occupied = vec![false; occupancy_side * occupancy_side];
    let mut selected: Vec<Vec<[f32; 3]>> = Vec::new();
    let mut normal_count = 0;
    'candidate: for points in candidates {
        work.charge(1)?;
        let [x, z] =
            uv(l, *points.last().expect("nonempty river")).map(|v| index(l.surface_resolution, v));
        let cell = z * l.surface_resolution + x;
        let mut best = 0;
        for channel in 1..5 {
            if l.biome[channel * l.surface_resolution * l.surface_resolution + cell]
                > l.biome[best * l.surface_resolution * l.surface_resolution + cell]
            {
                best = channel;
            }
        }
        let exempt = (1 << best) == policy.exempt_biome;
        if normal_count >= policy.maximum_rivers && !exempt {
            continue;
        }
        for existing in &selected {
            for &a in &points {
                for &b in existing {
                    work.charge(1)?;
                    if distance(a, b, true) < policy.separation.powi(2) {
                        continue 'candidate;
                    }
                }
            }
        }
        for &p in &points {
            work.charge(1)?;
            let [x, z] = uv(l, p).map(|v| index(occupancy_side, v));
            if occupied[z * occupancy_side + x] {
                continue 'candidate;
            }
        }
        let mut previous: Option<[usize; 2]> = None;
        for &p in &points {
            work.charge(1)?;
            let [x, z] = uv(l, p).map(|v| index(occupancy_side, v));
            if let Some([px, pz]) = previous {
                occupied[z * occupancy_side + px] = true;
                occupied[pz * occupancy_side + x] = true;
            }
            occupied[z * occupancy_side + x] = true;
            previous = Some([x, z]);
        }
        normal_count += usize::from(!exempt);
        selected.push(points);
    }
    let mut paths = Vec::new();
    let mut final_controls = 0;
    for points in selected {
        work.charge(1)?;
        let remaining = (work.limit - work.used).min(32_000_000);
        let path = path_geometry::build(
            &points,
            path_geometry::Settings {
                horizontal_iterations: policy.smoothing[0],
                vertical_iterations: policy.smoothing[1],
                resample_distance: policy.resample_distance,
                maximum_work: remaining,
            },
            None,
            work.cancelled,
        )
        .map_err(|e| Stop::Failed(format!("River smoothing failed: {e:?}")))?
        .ok_or(Stop::Cancelled)?;
        work.charge(path.work())?;
        final_controls += path.points().len();
        if final_controls > settings.maximum_controls {
            return Err(Stop::Failed(
                "River resampled-control budget exceeded".into(),
            ));
        }
        paths.push(path);
    }
    work.charge(0)?;
    report.selected = paths.len();
    report.work = work.used;
    Ok(Layout { paths, report })
}
fn permutation(total: usize, seed: Option<u32>) -> (usize, usize) {
    let Some(seed) = seed else {
        return (0, 1);
    };
    if total == 0 {
        return (0, 1);
    }
    let mix = seed.wrapping_mul(747796405).wrapping_add(2891336453);
    let mut step = ((mix >> 8) as usize % total).max(1);
    let gcd = |mut a: usize, mut b: usize| {
        while b != 0 {
            let r = a % b;
            a = b;
            b = r;
        }
        a
    };
    while gcd(step, total) != 1 {
        step += 1;
    }
    (mix as usize % total, step)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy() -> Policy {
        let study: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/rivers/server-bd19c4ac-layout.json"
        ))
        .unwrap();
        serde_json::from_value(study["policy"].clone()).unwrap()
    }
    fn terrain() -> Layers {
        let (hr, n) = crate::native_layers::dimensions(1000).unwrap();
        let mut l = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: n,
            height: vec![0; hr * hr],
            topology: vec![0; n * n],
            biome: vec![0; n * n * 5],
            splat: vec![],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        for z in 0..hr {
            for x in 0..hr {
                let wx = x as f32 / (hr - 1) as f32 * 1000. - 500.;
                l.height[z * hr + x] = crate::erosion::encode(150. - wx.abs() * 0.6);
            }
        }
        for z in 0..n {
            for x in 0..n {
                let wx = (x as f32 + 0.5) / n as f32 * 1000. - 500.;
                l.topology[z * n + x] = if wx.abs() > 250. { 128 } else { 1 };
                l.biome[n * n + z * n + x] = 255;
            }
        }
        l
    }
    fn synthetic_policy() -> Policy {
        let mut p = policy();
        p.scan = [40., 100., 120.];
        p.minimum_controls = 6;
        p.separation = 70.;
        p
    }
    fn settings(seed: Option<u32>) -> Settings {
        Settings {
            seed,
            maximum_work: 32_000_000,
            maximum_candidates: 128,
            maximum_controls: 65536,
        }
    }
    #[test]
    fn generated_rivers_reach_ocean_are_deterministic_and_keep_source_immutable() {
        let l = terrain();
        let p = synthetic_policy();
        let noise = vec![-0.5; p.maximum_steps];
        let before = l.height.clone();
        let original_topology = l.topology.clone();
        for seed in [None, Some(123), Some(999)] {
            let a = generate(&l, &p, &noise, &settings(seed), || false)
                .unwrap()
                .unwrap();
            let b = generate(&l, &p, &noise, &settings(seed), || false)
                .unwrap()
                .unwrap();
            assert_eq!(a.paths.len(), 2, "{:?}", a.report.terminations);
            assert_eq!(
                serde_json::to_value(&a).unwrap(),
                serde_json::to_value(&b).unwrap()
            );
            assert!(!a.report.terrain_applied);
            assert!(a.report.candidates >= a.report.selected);
            for path in &a.paths {
                assert!(path.length() > 100.);
                let points = path.points();
                assert!(points[0][1] > p.minimum_start_height);
                assert_ne!(topology(&l.view(), *points.last().unwrap()) & 128, 0);
                assert!(
                    points
                        .windows(2)
                        .all(|pair| pair[1][1] <= pair[0][1] + 0.0001)
                );
            }
        }
        assert_eq!(l.height, before);
        assert_eq!(l.topology, original_topology);
    }
    #[test]
    fn permutation_visits_every_candidate_once_and_seed_changes_the_scan() {
        for n in [0, 1, 4, 36, 257, 640000] {
            for seed in [1, 17, u32::MAX] {
                let (mut at, stride) = permutation(n, Some(seed));
                let mut visited = vec![false; n];
                for _ in 0..n {
                    assert!(!visited[at]);
                    visited[at] = true;
                    at = (at + stride) % n;
                }
                assert!(visited.into_iter().all(|v| v));
            }
        }
        assert_ne!(permutation(36, Some(123)), permutation(36, Some(999)));
        assert_eq!(permutation(36, None), (0, 1));
    }
    #[test]
    fn blocked_short_flat_cancelled_and_over_budget_scans_never_publish_partial_layouts() {
        let mut l = terrain();
        let p = synthetic_policy();
        let noise = vec![0.; p.maximum_steps];
        for cell in &mut l.topology {
            *cell |= 1024;
        }
        let a = generate(&l, &p, &noise, &settings(None), || false)
            .unwrap()
            .unwrap();
        assert!(a.paths.is_empty());
        assert!(
            a.report
                .terminations
                .get("blocked-topology")
                .copied()
                .unwrap_or(0)
                > 0
        );
        let l = terrain();
        let mut short = p.clone();
        short.minimum_controls = 10000;
        let a = generate(&l, &short, &noise, &settings(None), || false)
            .unwrap()
            .unwrap();
        assert!(a.paths.is_empty());
        assert!(a.report.terminations.get("short").copied().unwrap_or(0) > 0);
        let mut low = settings(None);
        low.maximum_work = 100;
        assert!(generate(&l, &p, &noise, &low, || false).is_err());
        let count = std::cell::Cell::new(0);
        assert!(
            generate(&l, &p, &noise, &settings(None), || {
                count.set(count.get() + 1);
                count.get() > 400
            })
            .unwrap()
            .is_none()
        );
        assert!(generate(&l, &p, &noise[..5], &settings(None), || false).is_err());
        let mut invalid = p.clone();
        invalid.scan[2] = f32::NAN;
        assert!(generate(&l, &invalid, &noise, &settings(None), || false).is_err());
        let mut l = terrain();
        l.height.fill(crate::erosion::encode(100.));
        assert!(
            generate(&l, &p, &noise, &settings(None), || false)
                .unwrap()
                .unwrap()
                .paths
                .is_empty()
        );
    }
    #[test]
    fn radius_uses_source_unsigned_conversion_clamped_growth_and_mouth_taper() {
        let p = policy();
        p.validate().unwrap();
        // At distance zero the base radius is four. Signed zero maps to one.
        assert_eq!(p.radius(0., 0., 0.), 3.);
        assert_eq!(p.radius(0., 0., -1.), 3.5);
        assert_eq!(p.radius(0., 0., 1.), 3.);
        assert_eq!(p.radius(1000., 1000., 0.), 72.);
        assert_eq!(p.radius(1000., 1100., 0.), 24.);
        let points = p.noise_coordinates().unwrap();
        assert_eq!(points.len(), 5000);
        assert_eq!(points[0], 0.);
        assert_eq!(points[1], 0.02);
        assert_eq!(
            serde_json::from_str::<Policy>(&serde_json::to_string(&p).unwrap()).unwrap(),
            p
        );
    }

    #[test]
    fn radius_matches_calls_to_the_owned_server_assembly_and_native_library() {
        #[derive(Deserialize)]
        struct Case {
            distance: f32,
            length: f32,
            signed_noise: f32,
            unsigned_noise: f32,
            radius: f32,
        }
        #[derive(Deserialize)]
        struct Reference {
            assembly_sha256: String,
            native_sha256: String,
            cases: Vec<Case>,
        }
        let reference: Reference =
            serde_json::from_str(include_str!("../tests/fixtures/river-radius-bd19c4ac.json"))
                .unwrap();
        assert_eq!(
            reference.assembly_sha256,
            "bd19c4ac705edd18ff556f5dbb3130e098b380577cc2ab0d73a2ec89ee289cc9"
        );
        assert_eq!(
            reference.native_sha256,
            "abe4c229e544e86516b242f0ec5b0b7bdb267869e419ccb7ae3ff9bc7e48bf2b"
        );
        assert_eq!(reference.cases.len(), 60);
        let p = policy();
        for c in reference.cases {
            assert_eq!(
                (c.signed_noise * 0.5 + 1.).to_bits(),
                c.unsigned_noise.to_bits()
            );
            let got = p.radius(c.distance, c.length, c.signed_noise);
            assert!(
                (got - c.radius).abs() < 0.0001,
                "distance {}, length {}, expected {}, got {}",
                c.distance,
                c.length,
                c.radius,
                got
            );
        }
    }

    #[test]
    fn exempt_biome_is_selected_at_the_mouth_and_candidate_caps_are_atomic() {
        let mut l = terrain();
        let mut p = synthetic_policy();
        p.maximum_rivers = 0;
        let noise = vec![0.; p.maximum_steps];
        assert!(
            generate(&l, &p, &noise, &settings(None), || false)
                .unwrap()
                .unwrap()
                .paths
                .is_empty()
        );
        let cells = l.surface_resolution * l.surface_resolution;
        l.biome.fill(0);
        l.biome[4 * cells..].fill(255);
        let result = generate(&l, &p, &noise, &settings(None), || false)
            .unwrap()
            .unwrap();
        assert!(!result.paths.is_empty());
        let mut limited = settings(None);
        limited.maximum_candidates = 1;
        assert!(generate(&l, &p, &noise, &limited, || false).is_err());
        limited = settings(None);
        limited.maximum_controls = 2;
        assert!(generate(&l, &p, &noise, &limited, || false).is_err());
    }
}
