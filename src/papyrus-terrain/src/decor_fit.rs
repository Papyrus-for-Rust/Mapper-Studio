//! Read-only geometric fitting of static decoration proposals. Terrain effects and
//! runtime collision/occupancy remain separate, required placement stages.
use crate::{
    decor,
    native_layers::{Layers, dimensions},
    spawn,
};
use serde::Serialize;
use std::collections::BTreeMap;
pub const VERSION: &str = "papyrus-decor-fit-2";
pub const EXTENDED_VERSION: &str = "papyrus-decor-fit-transforms-3";
#[derive(Clone, Copy)]
pub struct Water<'a> {
    pub local_heights: &'a [i16],
    pub resolution: usize,
    pub ocean_mask: u32,
    pub ocean_level: f32,
    pub terrain_minimum: f32,
}
impl Water<'_> {
    fn height(self, resolution: usize, u: f32, v: f32) -> f32 {
        // The checked UVs and vertex grid are shared with terrain sampling.
        let point = spawn::HeightPoint::new(resolution, u, v);
        if self.local_heights.is_empty() {
            point.interpolate([crate::erosion::decode(0); 4])
        } else {
            point.sample_grid(resolution, self.local_heights, crate::erosion::decode)
        }
    }
}
pub struct Settings<'a> {
    pub seed: u32,
    pub water: Option<Water<'a>>,
}
struct Options<'a> {
    settings: Settings<'a>,
    extended: bool,
}
type V = [f32; 3];
type Q = [f32; 4];
fn add(a: V, b: V) -> V {
    std::array::from_fn(|i| a[i] + b[i])
}
fn mul(a: V, b: V) -> V {
    std::array::from_fn(|i| a[i] * b[i])
}
fn scale(a: V, s: f32) -> V {
    a.map(|v| v * s)
}
fn dot(a: V, b: V) -> f32 {
    a.iter().zip(b).map(|(a, b)| a * b).sum()
}
fn cross(a: V, b: V) -> V {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm(v: V) -> Option<V> {
    let n = dot(v, v).sqrt();
    (n > 1e-7 && n.is_finite()).then(|| scale(v, 1. / n))
}
fn qnorm(q: Q) -> Option<Q> {
    let n = q.iter().map(|x| x * x).sum::<f32>().sqrt();
    (n > 1e-7 && n.is_finite()).then(|| q.map(|v| v / n))
}
/// Rotate a vector using an authored unit quaternion, shared by fitting and inspection.
pub fn rotate(q: Q, v: V) -> V {
    let a = [q[0], q[1], q[2]];
    let t = scale(cross(a, v), 2.);
    add(v, add(scale(t, q[3]), cross(a, t)))
}
pub(crate) fn qmul(a: Q, b: Q) -> Q {
    let av = [a[0], a[1], a[2]];
    let bv = [b[0], b[1], b[2]];
    let v = add(add(scale(bv, a[3]), scale(av, b[3])), cross(av, bv));
    [v[0], v[1], v[2], a[3] * b[3] - dot(av, bv)]
}
pub(crate) fn euler(v: V) -> Q {
    let [x, y, z] = v.map(|a| a.to_radians() * 0.5);
    let (sx, cx) = x.sin_cos();
    let (sy, cy) = y.sin_cos();
    let (sz, cz) = z.sin_cos();
    qmul(qmul([0., sy, 0., cy], [sx, 0., 0., cx]), [0., 0., sz, cz])
}
pub(crate) fn look(forward: V, up: V) -> Option<Q> {
    let up = norm(up)?;
    let right = norm(cross(up, forward)).or_else(|| {
        norm(cross(
            up,
            if up[2].abs() < 0.9 {
                [0., 0., 1.]
            } else {
                [1., 0., 0.]
            },
        ))
    })?;
    let f = cross(right, up);
    let m = [
        [right[0], up[0], f[0]],
        [right[1], up[1], f[1]],
        [right[2], up[2], f[2]],
    ];
    let trace = m[0][0] + m[1][1] + m[2][2];
    let q = if trace > 0. {
        let s = (trace + 1.).sqrt() * 2.;
        [
            (m[2][1] - m[1][2]) / s,
            (m[0][2] - m[2][0]) / s,
            (m[1][0] - m[0][1]) / s,
            s * 0.25,
        ]
    } else {
        let i = if m[0][0] > m[1][1] && m[0][0] > m[2][2] {
            0
        } else if m[1][1] > m[2][2] {
            1
        } else {
            2
        };
        let j = (i + 1) % 3;
        let k = (i + 2) % 3;
        let s = (1. + m[i][i] - m[j][j] - m[k][k]).max(0.).sqrt() * 2.;
        if s < 1e-7 {
            return None;
        }
        let mut q = [0.; 4];
        q[i] = s * 0.25;
        q[j] = (m[j][i] + m[i][j]) / s;
        q[k] = (m[k][i] + m[i][k]) / s;
        q[3] = (m[k][j] - m[j][k]) / s;
        q
    };
    qnorm(q)
}
pub fn path_rotation(forward: V) -> Option<Q> {
    if !valid(forward) {
        return None;
    }
    if dot(forward, forward) < 1e-12 {
        Some([0., 0., 0., 1.])
    } else {
        look(forward, [0., 1., 0.])
    }
}
#[derive(Clone, Debug, Serialize)]
pub enum Transform {
    Rotate {
        min: V,
        max: V,
    },
    Scale {
        min: V,
        max: V,
    },
    /// Authored offsets use scaled world axes and retain endpoint order.
    /// Sampling follows Mapper's existing deterministic fitting stream.
    Offset {
        min: V,
        max: V,
    },
    Align {
        normal: f32,
        gradient: f32,
    },
    AlignSlope {
        normal: f32,
        gradient: f32,
        offset: V,
        scale: V,
    },
    Flip {
        axis: u8,
    },
    Swim,
}
#[derive(Clone, Debug, Serialize)]
pub struct Anchor {
    pub position: V,
    pub extents: f32,
    pub offset: f32,
    pub radius: f32,
    pub slope_scale: f32,
}
#[derive(Clone, Debug, Serialize)]
pub struct PointFilter {
    pub check_placement: bool,
    pub position: V,
    pub filter: spawn::Filter,
    pub topology_radius: f32,
}
#[derive(Clone, Debug, Serialize)]
pub struct Prefab {
    pub rotation: Q,
    pub scale: V,
    pub transforms: Vec<Transform>,
    pub anchors: Vec<Anchor>,
    pub filters: Vec<PointFilter>,
    pub unsupported: Vec<String>,
    pub pending: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct Pose {
    pub candidate: usize,
    pub position: V,
    pub rotation: Q,
    pub scale: V,
}
#[derive(Debug, Serialize, Default)]
pub struct Counts {
    pub attempted: usize,
    pub fitted: usize,
    pub rejected_filter: usize,
    pub rejected_anchors: usize,
    pub rejected_bounds_or_holes: usize,
    pub unsupported: usize,
    pub budget_limited: usize,
}
#[derive(Debug, Serialize)]
pub struct PrefabReport {
    pub counts: Counts,
    pub unsupported: Vec<String>,
    pub pending: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub poses: Vec<Pose>,
    pub counts: Counts,
    pub prefabs: BTreeMap<String, PrefabReport>,
    pub height_samples: usize,
    pub sample_visits: usize,
    pub unprocessed_candidates: usize,
    pub budget_exhausted: bool,
    pub physics_validated: bool,
}
fn valid(v: V) -> bool {
    v.iter().all(|x| x.is_finite() && x.abs() <= 10000.)
}
impl Prefab {
    fn validate(&self) -> Result<(), String> {
        if qnorm(self.rotation).is_none()
            || (self.rotation.iter().map(|v| v * v).sum::<f32>() - 1.).abs() > 0.001
            || !valid(self.scale)
            || self.scale.iter().any(|v| *v <= 0. || *v > 100.)
            || self.transforms.len() > 128
            || self.anchors.len() > 4096
            || self.filters.len() > 4096
        {
            return Err("Invalid decoration fitting prefab".into());
        }
        for t in &self.transforms {
            match t {
                Transform::Offset { min, max } if !valid(*min) || !valid(*max) => {
                    return Err("Invalid decoration offset range".into());
                }
                Transform::Rotate { min, max } | Transform::Scale { min, max }
                    if !valid(*min) || !valid(*max) || min.iter().zip(max).any(|(a, b)| a > b) =>
                {
                    return Err("Invalid decoration transform range".into());
                }
                Transform::Scale { min, max }
                    if min.iter().chain(max).any(|v| *v <= 0. || *v > 100.) =>
                {
                    return Err("Invalid decoration scale".into());
                }
                Transform::Align { normal, gradient }
                | Transform::AlignSlope {
                    normal, gradient, ..
                } if !normal.is_finite()
                    || !gradient.is_finite()
                    || !(0. ..=1.).contains(normal)
                    || !(0. ..=1.).contains(gradient) =>
                {
                    return Err("Invalid decoration alignment".into());
                }
                Transform::AlignSlope { offset, scale, .. }
                    if !valid(*offset) || !valid(*scale) =>
                {
                    return Err("Invalid decoration slope offset/scale".into());
                }
                Transform::Flip { axis } if *axis > 2 => {
                    return Err("Unknown decoration flip axis".into());
                }
                _ => {}
            }
        }
        for a in &self.anchors {
            if !valid(a.position)
                || !a.extents.is_finite()
                || !(0. ..=100.).contains(&a.extents)
                || !a.offset.is_finite()
                || a.offset.abs() > 100.
                || !a.radius.is_finite()
                || !(0. ..=16.).contains(&a.radius)
                || !a.slope_scale.is_finite()
                || !(0. ..=10.).contains(&a.slope_scale)
            {
                return Err("Invalid terrain anchor".into());
            }
        }
        for f in &self.filters {
            f.filter.validate()?;
            if !valid(f.position)
                || !f.topology_radius.is_finite()
                || !(0. ..=128.).contains(&f.topology_radius)
            {
                return Err("Invalid decoration point filter".into());
            }
        }
        Ok(())
    }
}
// Queries are memoized only while one cliff prefab searches its tilts against
// one immutable borrowed view. A later placement always receives a fresh cache.
#[derive(Clone, Copy)]
struct TopologyEntry {
    query: crate::topology_query::Query,
    topology: u32,
}
#[derive(Default)]
struct CliffTopologyCache {
    entries: [Option<TopologyEntry>; 16],
    centers: [u32; 16],
    next: usize,
}
impl CliffTopologyCache {
    fn reset(&mut self) {
        self.centers.fill(0);
        self.next = 0;
    }

    fn center_key(query: &crate::topology_query::Query) -> u32 {
        let [x, z] = query.center;
        // Valid surface coordinates are at most 4095. Reserve zero for
        // unused entries so misses only read the compact center array.
        1 + (x as u32 | (z as u32) << 16)
    }

    fn get(&self, query: &crate::topology_query::Query) -> Option<u32> {
        let center = Self::center_key(query);
        self.centers
            .iter()
            .enumerate()
            .filter(|&(_, &key)| key == center)
            .find_map(|(i, _)| {
                let entry = self.entries[i]?;
                (entry.query == *query).then_some(entry.topology)
            })
    }
    fn insert(&mut self, query: crate::topology_query::Query, topology: u32) {
        self.centers[self.next] = Self::center_key(&query);
        self.entries[self.next] = Some(TopologyEntry { query, topology });
        self.next = (self.next + 1) % self.entries.len();
    }
}
#[derive(Clone, Copy, Default)]
struct HeightExtrema {
    key: u64,
    generation: u32,
    minimum: i16,
    maximum: i16,
}

// Cliff trials repeatedly decode nearby vertices to the same f32 heights.
// Keep the arithmetic decoder for sequential raster passes.
fn decode_cliff_height(value: i16) -> f32 {
    static HEIGHTS: [f32; 1 << 16] = {
        let mut heights = [0.; 1 << 16];
        let mut index = 0;
        while index < heights.len() {
            heights[index] = crate::erosion::decode(index as u16 as i16);
            index += 1;
        }
        heights
    };
    HEIGHTS[value as u16 as usize]
}

struct CliffHeightCache {
    entries: [HeightExtrema; 256],
    generation: u32,
}

impl Default for CliffHeightCache {
    fn default() -> Self {
        Self {
            entries: [HeightExtrema::default(); 256],
            generation: 1,
        }
    }
}
impl CliffHeightCache {
    fn reset(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.entries.fill(HeightExtrema::default());
            self.generation = 1;
        }
    }

    fn get(&self, key: u64, slot: usize) -> Option<HeightExtrema> {
        let entry = self.entries[slot];
        (entry.key == key && entry.generation == self.generation).then_some(entry)
    }

    fn key(rect: [usize; 4]) -> (u64, usize) {
        // Native dimensions cap vertex coordinates at 4096.
        debug_assert!(rect.iter().all(|&v| v <= 4096));
        let [left, right, top, bottom] = rect.map(|coordinate| coordinate as u64);
        let key = left | (right << 16) | (top << 32) | (bottom << 48);
        let mixed = (key ^ (key >> 33)).wrapping_mul(0xff51afd7ed558ccd);
        (key, (mixed >> 56) as usize)
    }
}

#[derive(Default)]
struct CliffCaches {
    height: CliffHeightCache,
    topology: CliffTopologyCache,
}

impl CliffCaches {
    fn reset(&mut self) {
        self.height.reset();
        self.topology.reset();
    }
}

#[derive(Default)]
struct CliffOriginFilterCache {
    result: Option<Result<(), Failure>>,
    repeat_work: Option<usize>,
}
impl CliffOriginFilterCache {
    fn evaluate(
        &mut self,
        surface: &mut Surface<'_>,
        prefab: &Prefab,
        pose: OrientedPose,
    ) -> Result<(), Failure> {
        if let (Some(result), Some(work)) = (self.result, self.repeat_work) {
            let charged = work.min(surface.maximum_samples - surface.samples);
            surface.samples += charged;
            return if charged == work {
                result
            } else {
                Err(Failure::Filter)
            };
        }
        let before = surface.samples;
        let result = fit_point_filters(surface, prefab, pose);
        // The second call measures the warmed topology cache or the unindexed
        // fallback. Later repeats retain that exact logical sampling charge.
        if self.result.is_some() {
            self.repeat_work = Some(surface.samples - before);
        }
        self.result = Some(result);
        result
    }
}

struct Surface<'a> {
    height_cache: Option<&'a mut CliffHeightCache>,
    topology_cache: Option<&'a mut CliffTopologyCache>,
    maximum_samples: usize,
    layers: crate::native_layers::View<'a>,
    alpha: &'a [u8],
    samples: usize,
    heights: usize,
}
const MAX_SAMPLES: usize = 64_000_000;
pub const MAXIMUM_FITTING_SAMPLES: u64 = 20_000_000_000;
impl Surface<'_> {
    fn uv(&self, p: V) -> Option<(f32, f32)> {
        let size = self.layers.size as f32;
        let u = p[0] / size + 0.5;
        let v = p[2] / size + 0.5;
        // The validated world is at most 6 km across. An in-range UV also
        // proves its horizontal input is finite and within the pose bound.
        (p[1].abs() <= 10000. && (0. ..=1.).contains(&u) && (0. ..=1.).contains(&v))
            .then_some((u, v))
    }
    fn intact(&self, p: V) -> bool {
        self.uv(p)
            .is_some_and(|(u, v)| self.alpha[self.surface_cell(u, v)] >= 128)
    }
    fn surface_cell(&self, u: f32, v: f32) -> usize {
        let n = self.layers.surface_resolution;
        idx(n, v) * n + idx(n, u)
    }
    fn height_uv(&mut self, u: f32, v: f32) -> Option<f32> {
        self.charge_height()?;
        Some(if self.height_cache.is_some() {
            spawn::height_view_with(self.layers, u, v, decode_cliff_height)
        } else {
            spawn::height_view(self.layers, u, v)
        })
    }
    fn charge_height(&mut self) -> Option<()> {
        if self.samples == self.maximum_samples {
            return None;
        }
        self.samples += 1;
        self.heights += 1;
        Some(())
    }
    fn height_extrema(&mut self, rect: [usize; 4]) -> Option<(i16, i16)> {
        let (key, slot) = CliffHeightCache::key(rect);
        if let Some(entry) = self
            .height_cache
            .as_ref()
            .and_then(|cache| cache.get(key, slot))
        {
            if self.samples == self.maximum_samples {
                return None;
            }
            self.samples += 1;
            return Some((entry.minimum, entry.maximum));
        }
        let row_width = rect[1] - rect[0] + 1;
        let count = row_width * (rect[3] - rect[2] + 1);
        let charged = count.min(self.maximum_samples - self.samples);
        self.samples += charged;
        self.heights += charged;
        if charged != count {
            // The former partial scan charged every available sample and
            // discarded the result. Preserve that charge without reading it.
            return None;
        }
        let hr = self.layers.height_resolution;
        let height = self.layers.height;
        let first = rect[2] * hr + rect[0];
        let last = rect[3] * hr + rect[0];
        let (minimum, maximum) = if row_width <= 2 && rect[3] - rect[2] <= 1 {
            // Small authored anchors normally touch just one or two vertices per axis.
            let a = height[first];
            let b = height[first + row_width - 1];
            let c = height[last];
            let d = height[last + row_width - 1];
            (a.min(b).min(c.min(d)), a.max(b).max(c.max(d)))
        } else if row_width == 3 && rect[3] - rect[2] == 2 {
            // A one-metre anchor commonly covers three vertices per axis.
            let window = &height[first..last + 3];
            let top = [window[0], window[1], window[2]];
            let middle = [window[hr], window[hr + 1], window[hr + 2]];
            let bottom = [window[2 * hr], window[2 * hr + 1], window[2 * hr + 2]];
            let minimum = |i: usize| top[i].min(middle[i]).min(bottom[i]);
            let maximum = |i: usize| top[i].max(middle[i]).max(bottom[i]);
            (
                minimum(0).min(minimum(1)).min(minimum(2)),
                maximum(0).max(maximum(1)).max(maximum(2)),
            )
        } else {
            let mut minimum = i16::MAX;
            let mut maximum = i16::MIN;
            for z in rect[2]..=rect[3] {
                let start = z * hr + rect[0];
                for &value in &height[start..start + row_width] {
                    minimum = minimum.min(value);
                    maximum = maximum.max(value);
                }
            }
            (minimum, maximum)
        };
        if let Some(cache) = self.height_cache.as_mut() {
            cache.entries[slot] = HeightExtrema {
                key,
                generation: cache.generation,
                minimum,
                maximum,
            };
        }
        Some((minimum, maximum))
    }
    fn normal(&mut self, p: V) -> Option<V> {
        let size = self.layers.size as f32;
        let half = size * 0.5;
        let d = size / (self.layers.height_resolution - 1) as f32;
        let [x, y, z] = p;
        let x0 = (x - d).max(-half);
        let x1 = (x + d).min(half);
        let z0 = (z - d).max(-half);
        let z1 = (z + d).min(half);
        let axis = |coordinate: f32| coordinate / size + 0.5;
        let (u, v) = (axis(x), axis(z));
        let sample = |surface: &mut Self, x, z, u, v| {
            // Charge each tap in the original order, including an invalid tap.
            surface.charge_height()?;
            (valid([x, y, z]) && (0. ..=1.).contains(&u) && (0. ..=1.).contains(&v))
                .then(|| spawn::height_view(surface.layers, u, v))
        };
        let dx = (sample(self, x1, z, axis(x1), v)? - sample(self, x0, z, axis(x0), v)?)
            / (x1 - x0);
        let dz = (sample(self, x, z1, u, axis(z1))? - sample(self, x, z0, u, axis(z0))?)
            / (z1 - z0);
        norm([-dx, 1., -dz])
    }
    #[cfg(test)]
    fn factor_uv(
        &mut self,
        u: f32,
        v: f32,
        f: &spawn::Filter,
        radius: f32,
        check_placement: bool,
    ) -> f32 {
        self.factor_at(self.surface_cell(u, v), (u, v), f, radius, check_placement)
    }
    fn factor_at(
        &mut self,
        cell: usize,
        (u, v): (f32, f32),
        f: &spawn::Filter,
        radius: f32,
        check_placement: bool,
    ) -> f32 {
        if self.samples == self.maximum_samples {
            return 0.;
        }
        self.samples += 1;
        if check_placement && self.layers.placement_blocked_at(cell) {
            return 0.;
        }
        if radius == 0. {
            return spawn::factor_view_at(self.layers, f, cell, u, v);
        }
        let n = self.layers.surface_resolution;
        let r = radius / self.layers.size as f32;
        let (xm, zm) = (idx(n, u), idx(n, v));
        if let Some(tiles) = self
            .layers
            .topology_tiles
            .filter(|t| t.matches(self.layers.topology, n))
        {
            let relevant = f.topology_all as u32
                | f.topology_not as u32
                | if f.topology_any == -1 {
                    0
                } else {
                    f.topology_any as u32
                };
            let query = crate::topology_query::Query {
                rect: [idx(n, u - r), idx(n, u + r), idx(n, v - r), idx(n, v + r)],
                center: [xm, zm],
                radius,
                relevant,
            };
            let cached = self
                .topology_cache
                .as_ref()
                .and_then(|cache| cache.get(&query));
            let topo = if let Some(topology) = cached {
                topology
            } else {
                let Ok(topology) = tiles.union(query, &mut self.samples, self.maximum_samples)
                else {
                    return 0.;
                };
                if let Some(cache) = self.topology_cache.as_mut() {
                    cache.insert(query, topology);
                }
                topology
            };
            if !f.topology_matches(topo) {
                return 0.;
            }
            return spawn::material_factor_at(self.layers, f, cell, u, v);
        }
        let mut topo = 0_u32;
        // Inspected source limits the grid rectangle in world units, then uses
        // the serialized radius directly for the integer-index circle test.
        for z in idx(n, v - r)..=idx(n, v + r) {
            for x in idx(n, u - r)..=idx(n, u + r) {
                if self.samples == self.maximum_samples {
                    return 0.;
                }
                self.samples += 1;
                let dx = x as f32 - xm as f32;
                let dz = z as f32 - zm as f32;
                if dx * dx + dz * dz <= radius * radius {
                    topo |= self.layers.topology[z * n + x] as u32;
                }
            }
        }
        if !f.topology_matches(topo) {
            return 0.;
        }
        spawn::material_factor_at(self.layers, f, cell, u, v)
    }
}
fn idx(n: usize, u: f32) -> usize {
    // Native raster indices fit in u32. Widen only for indexing the slice.
    (u * n as f32).clamp(0., (n - 1) as f32) as u32 as usize
}
fn rand(state: &mut u64) -> f32 {
    *state = state.wrapping_add(0x9e3779b97f4a7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    ((z ^ (z >> 31)) >> 40) as f32 / 16777216.
}
fn seed_prefix(seed: u32, stage: &str, path: &str) -> u64 {
    b"papyrus-decor-fit-2"
        .iter()
        .copied()
        .chain(seed.to_le_bytes())
        .chain(stage.bytes())
        .chain([0])
        .chain(path.bytes())
        .fold(0xcbf29ce484222325, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
        })
}
fn seed_at(prefix: u64, position: V) -> u64 {
    position
        .into_iter()
        .flat_map(|v| v.to_bits().to_le_bytes())
        .fold(prefix, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
        })
}
fn seed(seed: u32, stage: &str, path: &str, position: V) -> u64 {
    seed_at(seed_prefix(seed, stage, path), position)
}
#[derive(Clone, Copy, Debug)]
enum Failure {
    Filter,
    Anchor,
    HeightBound,
    Reservation,
    Surface,
}
/// Validates immutable authored fitting data once, rather than at every path sample.
#[derive(Clone)]
pub struct PreparedPrefab(Prefab);
impl PreparedPrefab {
    pub fn new(prefab: Prefab) -> Result<Self, String> {
        prefab.validate()?;
        Ok(Self(prefab))
    }
    pub fn metadata(&self) -> &Prefab {
        &self.0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrientedRejection {
    Unsupported,
    Filter,
    Anchor,
    Surface,
    Reservation,
    Budget,
}
impl OrientedRejection {
    fn label(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::Filter => "filter",
            Self::Anchor => "anchor",
            Self::Surface => "surface",
            Self::Reservation => "reservation",
            Self::Budget => "budget",
        }
    }
    pub(crate) fn record(self, counts: &mut BTreeMap<String, usize>) {
        let label = self.label();
        if let Some(count) = counts.get_mut(label) {
            *count += 1;
        } else {
            counts.insert(label.to_owned(), 1);
        }
    }
}

#[derive(Default)]
pub(crate) struct RejectionCounts([usize; Self::REASONS.len()]);

impl RejectionCounts {
    const REASONS: [OrientedRejection; 6] = [
        OrientedRejection::Unsupported,
        OrientedRejection::Filter,
        OrientedRejection::Anchor,
        OrientedRejection::Surface,
        OrientedRejection::Reservation,
        OrientedRejection::Budget,
    ];

    pub(crate) fn record(&mut self, reason: OrientedRejection) {
        self.0[reason as usize] += 1;
    }

    pub(crate) fn finish(self) -> BTreeMap<String, usize> {
        Self::REASONS
            .into_iter()
            .filter_map(|reason| {
                let count = self.0[reason as usize];
                (count != 0).then(|| (reason.label().to_owned(), count))
            })
            .collect()
    }
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct OrientedPose {
    pub position: V,
    pub rotation: Q,
    pub scale: V,
}
#[derive(Clone, Copy)]
pub enum OrientedOutcome {
    Fitted(OrientedPose),
    Rejected(OrientedRejection),
}
pub struct OrientedRequest<'a> {
    pub position: V,
    pub rotation: Q,
    pub filter: &'a spawn::Filter,
    pub prefab: &'a PreparedPrefab,
    pub seed: u64,
    pub apply_decor: bool,
    pub check_anchor_filter: bool,
    pub anchor_mode: i32,
}
/// Shared, bounded surface context for source adapters that supply their own
/// starting orientation. No terrain writes or runtime placement validation.
pub struct OrientedFitter<'a> {
    surface: Surface<'a>,
}
/// A shared fitting allowance for successive borrowed views of a private
/// transaction. Creating a view does not reset sampling work or copy channels.
pub struct OrientedBudget<'a> {
    maximum_samples: usize,
    size: u32,
    hr: usize,
    n: usize,
    alpha: &'a [u8],
    samples: usize,
    heights: usize,
    cliff_caches: Option<Box<CliffCaches>>,
    cliff_anchors: Vec<Anchor>,
}
/// Water inputs validated once before live object iteration.
#[derive(Clone, Copy)]
pub struct ValidatedWater<'a> {
    water: Water<'a>,
    size: u32,
}
impl<'a> ValidatedWater<'a> {
    /// Use the live topology's sea bit with the default dry local-water value.
    /// Keep the same interpolation as a full grid of encoded zeroes.
    pub fn global_ocean(
        layers: &Layers,
        ocean_mask: u32,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        let (hr, n) = dimensions(layers.size)?;
        if layers.height_resolution != hr
            || layers.surface_resolution != n
            || layers.height.len() != hr * hr
            || layers.topology.len() != n * n
            || ocean_mask == 0
            || ocean_mask & 0x8000_0000 != 0
        {
            return Err("Invalid live decoration water context".into());
        }
        for chunk in layers.height.chunks(32768) {
            if cancelled() {
                return Ok(None);
            }
            if chunk.iter().any(|v| !(0..=32766).contains(v)) {
                return Err("Invalid ocean source heights".into());
            }
        }
        Ok((!cancelled()).then_some(Self {
            water: Water {
                local_heights: &[],
                resolution: hr,
                ocean_mask,
                ocean_level: 0.,
                terrain_minimum: -500.,
            },
            size: layers.size,
        }))
    }

    pub fn new(water: Water<'a>, layers: &Layers) -> Result<Self, String> {
        let (hr, _) = dimensions(layers.size)?;
        if layers.height_resolution != hr
            || water.resolution != hr
            || water.local_heights.len() != hr * hr
            || water.local_heights.iter().any(|v| !(0..=32766).contains(v))
            || water.ocean_mask == 0
            || water.ocean_mask & 0x8000_0000 != 0
            || !water.ocean_level.is_finite()
            || !(-500. ..=500.).contains(&water.ocean_level)
            || water.terrain_minimum != -500.
        {
            return Err("Invalid live decoration water context".into());
        }
        Ok(Self {
            water,
            size: layers.size,
        })
    }
}
pub struct StaticRequest<'a> {
    pub candidate: &'a decor::Candidate,
    pub stage: &'a decor::Stage,
    pub prefab: &'a PreparedPrefab,
    pub seed: u32,
    pub water: Option<ValidatedWater<'a>>,
}
pub(crate) struct CoastalRequest<'a> {
    pub request: StaticRequest<'a>,
    pub pose: OrientedPose,
    pub repeat: bool,
}
/// Retains the exact immutable stage whose roster and settings were checked.
pub(crate) struct ValidatedStage<'a>(&'a decor::Stage, Option<(u32, Vec<u64>)>);
impl<'a> ValidatedStage<'a> {
    pub(crate) fn new(stage: &'a decor::Stage) -> Result<Self, String> {
        stage.validate()?;
        Ok(Self(stage, None))
    }

    pub(crate) fn with_seed(mut self, seed: u32) -> Self {
        self.1 = Some((
            seed,
            self.0
                .prefabs
                .iter()
                .map(|path| seed_prefix(seed, &self.0.id, path))
                .collect(),
        ));
        self
    }

    pub(crate) fn stage(&self) -> &'a decor::Stage {
        self.0
    }

    fn seed_prefix(&self, request: &StaticRequest<'_>) -> Option<u64> {
        self.1.as_ref().and_then(|(seed, prefixes)| {
            (*seed == request.seed).then(|| prefixes[request.candidate.prefab])
        })
    }
}
impl<'a> OrientedBudget<'a> {
    pub(crate) fn remaining_samples(&self) -> usize {
        self.maximum_samples - self.samples
    }

    pub(crate) fn cliff_batch_worker(&self) -> Self {
        Self {
            maximum_samples: self.remaining_samples().min(32_768),
            size: self.size,
            hr: self.hr,
            n: self.n,
            alpha: self.alpha,
            samples: 0,
            heights: 0,
            cliff_caches: None,
            cliff_anchors: Vec::new(),
        }
    }

    pub(crate) fn reset_cliff_batch_worker(&mut self) {
        self.samples = 0;
        self.heights = 0;
    }

    pub(crate) fn charge_cliff_prediction(&mut self, samples: usize, heights: usize) -> bool {
        // A rejection on the final available sample can become a budget
        // rejection. Leave that boundary case to the live fitter.
        if samples >= self.remaining_samples() {
            return false;
        }
        self.samples += samples;
        self.heights += heights;
        true
    }
    pub(crate) fn try_fit_coastal_cliff(
        &mut self,
        view: crate::native_layers::View<'_>,
        request: CoastalRequest<'_>,
        stage: &ValidatedStage<'_>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<OrientedOutcome>, String> {
        if cancelled() {
            return Ok(None);
        }
        if !std::ptr::eq(stage.0, request.request.stage) {
            return Err("Coastal cliff stage differs from its validated roster".into());
        }
        self.validate_static_candidate(view, &request.request)?;
        if self.samples == self.maximum_samples {
            return Ok(Some(OrientedOutcome::Rejected(OrientedRejection::Budget)));
        }
        let pose = request.pose;
        if !valid(pose.position)
            || !valid(pose.scale)
            || pose.scale.iter().any(|v| *v <= 0. || *v > 10000.)
            || pose.rotation.iter().any(|v| !v.is_finite())
            || (pose.rotation.iter().map(|v| v * v).sum::<f32>() - 1.).abs() > 0.001
        {
            return Err("Invalid coastal cliff starting pose".into());
        }
        let prefab = &request.request.prefab.0;
        if !prefab.unsupported.is_empty() {
            return Ok(Some(OrientedOutcome::Rejected(
                OrientedRejection::Unsupported,
            )));
        }
        let caches = self.cliff_caches.get_or_insert_with(Box::default);
        caches.reset();
        let mut surface = Surface {
            height_cache: Some(&mut caches.height),
            topology_cache: Some(&mut caches.topology),
            maximum_samples: self.maximum_samples,
            layers: view,
            alpha: self.alpha,
            samples: self.samples,
            heights: self.heights,
        };
        let options = Options {
            settings: Settings {
                seed: request.request.seed,
                water: request.request.water.map(|w| w.water),
            },
            extended: true,
        };
        let result = (|| {
            let transformed = apply_transforms(
                &mut surface,
                prefab,
                pose,
                seed(
                    request.request.seed,
                    &stage.0.id,
                    &stage.0.prefabs[request.request.candidate.prefab],
                    pose.position,
                ),
                &options,
            )?;
            let fitted = fit_pose::<false>(
                &mut surface,
                stage.0,
                prefab,
                transformed,
                &options,
                !request.repeat,
                AnchorSearch::default(),
            )?;
            if request.repeat {
                fit_point_filters(
                    &mut surface,
                    prefab,
                    OrientedPose {
                        position: fitted.0,
                        rotation: fitted.1,
                        scale: fitted.2,
                    },
                )?;
            }
            Ok(fitted)
        })();
        self.samples = surface.samples;
        self.heights = surface.heights;
        if cancelled() {
            return Ok(None);
        }
        Ok(Some(match result {
            Ok((position, rotation, scale)) => OrientedOutcome::Fitted(OrientedPose {
                position,
                rotation,
                scale,
            }),
            Err(_) if self.samples == self.maximum_samples => {
                OrientedOutcome::Rejected(OrientedRejection::Budget)
            }
            Err(Failure::Filter) => OrientedOutcome::Rejected(OrientedRejection::Filter),
            Err(Failure::Anchor | Failure::HeightBound) => {
                OrientedOutcome::Rejected(OrientedRejection::Anchor)
            }
            Err(Failure::Reservation) => OrientedOutcome::Rejected(OrientedRejection::Reservation),
            Err(Failure::Surface) => OrientedOutcome::Rejected(OrientedRejection::Surface),
        }))
    }
    pub fn new(layers: &Layers, alpha: &'a [u8]) -> Result<Self, String> {
        OrientedFitter::new(layers, alpha)?;
        Ok(Self {
            size: layers.size,
            hr: layers.height_resolution,
            n: layers.surface_resolution,
            maximum_samples: MAX_SAMPLES,
            alpha,
            samples: 0,
            heights: 0,
            cliff_caches: None,
            cliff_anchors: Vec::new(),
        })
    }
    pub fn samples(&self) -> usize {
        self.samples
    }
    /// Configure one shared allowance before fitting consumes any samples.
    /// All temporary views retain this limit and their cumulative counters.
    pub fn with_sample_limit(mut self, maximum_samples: usize) -> Result<Self, String> {
        if self.samples != 0 || !(4..=MAXIMUM_FITTING_SAMPLES).contains(&(maximum_samples as u64)) {
            return Err("Invalid or already consumed live fitting allowance".into());
        }
        self.maximum_samples = maximum_samples;
        Ok(self)
    }
    pub fn height_samples(&self) -> usize {
        self.heights
    }
    /// The view must contain validated normalized terrain, as provided by
    /// RoadTransaction. Shape checks remain per call; no full-grid rescan occurs.
    pub fn try_fit(
        &mut self,
        view: crate::native_layers::View<'_>,
        request: OrientedRequest<'_>,
    ) -> Result<OrientedOutcome, String> {
        self.validate_view(view)?;
        let mut fitter = OrientedFitter {
            surface: Surface {
                height_cache: None,
                topology_cache: None,
                maximum_samples: self.maximum_samples,
                layers: view,
                alpha: self.alpha,
                samples: self.samples,
                heights: self.heights,
            },
        };
        let result = fitter.try_fit(request);
        self.samples = fitter.samples();
        self.heights = fitter.height_samples();
        result
    }
    fn validate_view(&self, view: crate::native_layers::View<'_>) -> Result<(), String> {
        if view.size != self.size
            || view.height_resolution != self.hr
            || view.surface_resolution != self.n
            || view.height.len() != self.hr * self.hr
            || view
                .placement_blockers
                .is_some_and(|b| b.len() != self.n * self.n)
            || view.topology.len() != self.n * self.n
            || view.biome.len() != self.n * self.n * 5
            || view.splat.len() != self.n * self.n * 8
        {
            return Err("Live fitting view differs from the validated terrain shape".into());
        }
        Ok(())
    }
    /// Static decoration uses the prefab basis and point filters with this
    /// shared fitting allowance.
    pub fn try_fit_static(
        &mut self,
        view: crate::native_layers::View<'_>,
        request: StaticRequest<'_>,
    ) -> Result<OrientedOutcome, String> {
        request.stage.validate()?;
        self.validate_static_candidate(view, &request)?;
        self.fit_static_checked(view, request, None)
    }

    pub(crate) fn try_fit_static_prepared(
        &mut self,
        view: crate::native_layers::View<'_>,
        request: StaticRequest<'_>,
        stage: &ValidatedStage<'_>,
    ) -> Result<OrientedOutcome, String> {
        if !std::ptr::eq(stage.stage(), request.stage) {
            return Err("Decoration fitting stage differs from its validated roster".into());
        }
        self.validate_static_candidate(view, &request)?;
        let prefix = stage.seed_prefix(&request);
        self.fit_static_checked(view, request, prefix)
    }
    /// Roadside decoration retains full prefab fitting for path-owned folders.
    pub fn try_fit_path_static(
        &mut self,
        view: crate::native_layers::View<'_>,
        request: StaticRequest<'_>,
    ) -> Result<OrientedOutcome, String> {
        request.stage.validate_path_attached()?;
        self.validate_static_candidate(view, &request)?;
        self.fit_static_checked(view, request, None)
    }
    fn fit_static_checked(
        &mut self,
        view: crate::native_layers::View<'_>,
        request: StaticRequest<'_>,
        prefix: Option<u64>,
    ) -> Result<OrientedOutcome, String> {
        let candidate = request.candidate;
        if self.samples == self.maximum_samples {
            return Ok(OrientedOutcome::Rejected(OrientedRejection::Budget));
        }
        let prefab = &request.prefab.0;
        if !prefab.unsupported.is_empty() {
            return Ok(OrientedOutcome::Rejected(OrientedRejection::Unsupported));
        }
        if request.water.is_none()
            && prefab
                .transforms
                .iter()
                .any(|t| matches!(t, Transform::Swim))
        {
            return Err("DecorSwim requires a validated retained water raster".into());
        }
        let mut surface = Surface {
            height_cache: None,
            topology_cache: None,
            maximum_samples: self.maximum_samples,
            layers: view,
            alpha: self.alpha,
            samples: self.samples,
            heights: self.heights,
        };
        let prefix = prefix.unwrap_or_else(|| {
            seed_prefix(request.seed, &request.stage.id, &request.stage.prefabs[candidate.prefab])
        });
        let result = fit(
            &mut surface,
            candidate,
            request.stage,
            prefab,
            seed_at(prefix, candidate.position),
            &Options {
                settings: Settings {
                    seed: request.seed,
                    water: request.water.map(|w| w.water),
                },
                extended: true,
            },
        );
        self.samples = surface.samples;
        self.heights = surface.heights;
        Ok(match result {
            Ok((position, rotation, scale)) => OrientedOutcome::Fitted(OrientedPose {
                position,
                rotation,
                scale,
            }),
            Err(_) if self.samples == self.maximum_samples => {
                OrientedOutcome::Rejected(OrientedRejection::Budget)
            }
            Err(Failure::Filter) => OrientedOutcome::Rejected(OrientedRejection::Filter),
            Err(Failure::Anchor | Failure::HeightBound) => {
                OrientedOutcome::Rejected(OrientedRejection::Anchor)
            }
            Err(Failure::Reservation) => OrientedOutcome::Rejected(OrientedRejection::Reservation),
            Err(Failure::Surface) => OrientedOutcome::Rejected(OrientedRejection::Surface),
        })
    }
    fn validate_static_candidate(
        &self,
        view: crate::native_layers::View<'_>,
        request: &StaticRequest<'_>,
    ) -> Result<(), String> {
        self.validate_view(view)?;
        let candidate = request.candidate;
        if !valid(candidate.position)
            || candidate.stage != request.stage.order
            || candidate.prefab >= request.stage.prefabs.len()
            || request
                .water
                .is_some_and(|w| w.size != self.size || w.water.resolution != self.hr)
        {
            return Err("Invalid live static-decoration fitting request".into());
        }
        Ok(())
    }
    /// Apply decor once, then test every source-ordered hill-cliff tilt against
    /// the same borrowed terrain. Only a complete search can select the highest pose.
    pub fn try_fit_uniform_cliff(
        &mut self,
        view: crate::native_layers::View<'_>,
        request: StaticRequest<'_>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<OrientedOutcome>, String> {
        if cancelled() {
            return Ok(None);
        }
        let stage = ValidatedStage::new(request.stage)?;
        self.try_fit_uniform_cliff_prepared(view, request, &stage, None, cancelled)
    }
    pub(crate) fn try_fit_uniform_cliff_prepared(
        &mut self,
        view: crate::native_layers::View<'_>,
        request: StaticRequest<'_>,
        stage: &ValidatedStage<'_>,
        reserved_scale: Option<f64>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<OrientedOutcome>, String> {
        if cancelled() {
            return Ok(None);
        }
        if !std::ptr::eq(stage.0, request.stage) {
            return Err("Cliff fitting stage differs from its validated roster".into());
        }
        self.validate_static_candidate(view, &request)?;
        if self.samples == self.maximum_samples {
            return Ok(Some(OrientedOutcome::Rejected(OrientedRejection::Budget)));
        }
        let prefab = &request.prefab.0;
        if !prefab.unsupported.is_empty() {
            return Ok(Some(OrientedOutcome::Rejected(
                OrientedRejection::Unsupported,
            )));
        }
        if request.water.is_none()
            && prefab
                .transforms
                .iter()
                .any(|t| matches!(t, Transform::Swim))
        {
            return Err("DecorSwim requires a validated retained water raster".into());
        }
        let caches = self.cliff_caches.get_or_insert_with(Box::default);
        // A previous admission can change the borrowed terrain between fits.
        caches.reset();
        let mut surface = Surface {
            height_cache: Some(&mut caches.height),
            topology_cache: Some(&mut caches.topology),
            maximum_samples: self.maximum_samples,
            layers: view,
            alpha: self.alpha,
            samples: self.samples,
            heights: self.heights,
        };
        let cached_prefix = stage.seed_prefix(&request);
        // Select the anchor loop once for the whole tilt search. The common
        // highest-fit mode needs neither per-anchor filtering nor a mean.
        let result = if stage.0.anchor_mode == 2 && stage.0.filter_mode & 2 == 0 {
            fit_uniform_cliff::<true, true, true>(
                &mut surface,
                &request,
                cached_prefix,
                reserved_scale,
                &mut self.cliff_anchors,
                &cancelled,
            )
        } else {
            fit_uniform_cliff::<true, true, false>(
                &mut surface,
                &request,
                cached_prefix,
                reserved_scale,
                &mut self.cliff_anchors,
                &cancelled,
            )
        };
        self.samples = surface.samples;
        self.heights = surface.heights;
        Ok(match result {
            Ok(Some((position, rotation, scale))) => Some(OrientedOutcome::Fitted(OrientedPose {
                position,
                rotation,
                scale,
            })),
            Ok(None) => None,
            Err(_) if self.samples == self.maximum_samples => {
                Some(OrientedOutcome::Rejected(OrientedRejection::Budget))
            }
            Err(Failure::Filter) => Some(OrientedOutcome::Rejected(OrientedRejection::Filter)),
            Err(Failure::Anchor | Failure::HeightBound) => {
                Some(OrientedOutcome::Rejected(OrientedRejection::Anchor))
            }
            Err(Failure::Reservation) => Some(OrientedOutcome::Rejected(OrientedRejection::Reservation)),
            Err(Failure::Surface) => Some(OrientedOutcome::Rejected(OrientedRejection::Surface)),
        })
    }
}
impl<'a> OrientedFitter<'a> {
    pub fn new(layers: &'a Layers, alpha: &'a [u8]) -> Result<Self, String> {
        let (hr, n) = dimensions(layers.size)?;
        if layers.height_resolution != hr
            || layers.surface_resolution != n
            || layers.height.len() != hr * hr
            || layers.topology.len() != n * n
            || layers.biome.len() != n * n * 5
            || layers.splat.len() != n * n * 8
            || alpha.len() != n * n
            || layers.height.iter().any(|h| !(0..=32766).contains(h))
        {
            return Err("Invalid oriented fitting terrain".into());
        }
        Ok(Self {
            surface: Surface {
                height_cache: None,
                topology_cache: None,
                maximum_samples: MAX_SAMPLES,
                layers: layers.view(),
                alpha,
                samples: 0,
                heights: 0,
            },
        })
    }
    pub fn samples(&self) -> usize {
        self.surface.samples
    }
    pub fn height_samples(&self) -> usize {
        self.surface.heights
    }
    pub fn try_fit(&mut self, request: OrientedRequest<'_>) -> Result<OrientedOutcome, String> {
        request.filter.validate()?;
        if !valid(request.position)
            || request.rotation.iter().any(|v| !v.is_finite())
            || (request.rotation.iter().map(|v| v * v).sum::<f32>() - 1.).abs() > 0.001
            || !(0..=2).contains(&request.anchor_mode)
        {
            return Err("Invalid oriented fitting request".into());
        }
        if self.surface.samples == self.surface.maximum_samples {
            return Ok(OrientedOutcome::Rejected(OrientedRejection::Budget));
        }
        let p = &request.prefab.0;
        if !p.unsupported.is_empty()
            || request.apply_decor && p.transforms.iter().any(|t| matches!(t, Transform::Swim))
        {
            return Ok(OrientedOutcome::Rejected(OrientedRejection::Unsupported));
        }
        let stage = decor::Stage {
            id: String::new(),
            order: 0,
            name: String::new(),
            kind: decor::Kind::Uniform,
            folder: String::new(),
            enabled: true,
            mode: -1,
            filter: request.filter.clone(),
            filter_mode: if request.check_anchor_filter { 2 } else { 0 },
            anchor_mode: request.anchor_mode,
            distance: 1.,
            dithering: 0.,
            density: 1.,
            cluster: None,
            prefabs: vec![],
        };
        let candidate = decor::Candidate {
            stage: 0,
            prefab: 0,
            position: request.position,
        };
        let result = fit_basis(
            &mut self.surface,
            &candidate,
            &stage,
            p,
            request.seed,
            &Options {
                settings: Settings {
                    seed: 0,
                    water: None,
                },
                extended: true,
            },
            request.rotation,
            request.apply_decor,
            false,
        );
        Ok(match result {
            Ok((position, rotation, scale)) => OrientedOutcome::Fitted(OrientedPose {
                position,
                rotation,
                scale,
            }),
            Err(_) if self.surface.samples == self.surface.maximum_samples => {
                OrientedOutcome::Rejected(OrientedRejection::Budget)
            }
            Err(Failure::Filter) => OrientedOutcome::Rejected(OrientedRejection::Filter),
            Err(Failure::Anchor | Failure::HeightBound) => OrientedOutcome::Rejected(OrientedRejection::Anchor),
            Err(Failure::Reservation) => OrientedOutcome::Rejected(OrientedRejection::Reservation),
            Err(Failure::Surface) => OrientedOutcome::Rejected(OrientedRejection::Surface),
        })
    }
}
fn fit(
    surface: &mut Surface<'_>,
    candidate: &decor::Candidate,
    stage: &decor::Stage,
    prefab: &Prefab,
    seed: u64,
    options: &Options<'_>,
) -> Result<(V, Q, V), Failure> {
    fit_basis(
        surface,
        candidate,
        stage,
        prefab,
        seed,
        options,
        prefab.rotation,
        true,
        true,
    )
}
// These tilt ranges are part of the reviewed PlaceCliffsUniform process bound
// by the adapter's gameplay-code receipts. Keep the original order so the
// first equal-height fit wins.
fn cliff_rotations() -> &'static [Q] {
    static ROTATIONS: std::sync::LazyLock<Vec<Q>> = std::sync::LazyLock::new(|| {
        let mut rotations = Vec::with_capacity(112);
        for z in (0..=12).step_by(4) {
            for x in (0..=12).step_by(2) {
                for z_sign in [-1, 1] {
                    for x_sign in [-1, 1] {
                        rotations.push(euler([(x_sign * x) as f32, 0., (z_sign * z) as f32]));
                    }
                }
            }
        }
        rotations
    });
    &ROTATIONS
}
fn distinct_cliff_rotations() -> &'static [Q] {
    static DISTINCT: std::sync::LazyLock<Vec<Q>> = std::sync::LazyLock::new(|| {
        let mut rotations: Vec<Q> = Vec::new();
        for &rotation in cliff_rotations() {
            let bits = rotation.map(f32::to_bits);
            if !rotations.iter().any(|prior| prior.map(f32::to_bits) == bits) {
                rotations.push(rotation);
            }
        }
        rotations
    });
    &DISTINCT
}
fn fit_uniform_cliff<
    const PRUNE: bool,
    const CACHE_ORIGIN: bool,
    const UNFILTERED_HIGHEST: bool,
>(
    surface: &mut Surface<'_>,
    request: &StaticRequest<'_>,
    prefix: Option<u64>,
    reserved_scale: Option<f64>,
    scaled_anchors: &mut Vec<Anchor>,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<(V, Q, V)>, Failure> {
    let prefab = &request.prefab.0;
    let options = Options {
        settings: Settings {
            seed: request.seed,
            water: request.water.map(|w| w.water),
        },
        extended: true,
    };
    let pose = apply_transforms(
        surface,
        prefab,
        OrientedPose {
            position: request.candidate.position,
            rotation: prefab.rotation,
            scale: prefab.scale,
        },
        seed_at(
            prefix.unwrap_or_else(|| {
                seed_prefix(
                    request.seed,
                    &request.stage.id,
                    &request.stage.prefabs[request.candidate.prefab],
                )
            }),
            request.candidate.position,
        ),
        &options,
    )?;
    // An occupied root cell covered by a support for every tilt makes the
    // admission impossible. Use the actual scale selected by the transforms.
    if surface.samples < surface.maximum_samples
        && reserved_scale.is_some_and(|limit| {
            pose.scale.iter().all(|&axis| axis.is_finite() && f64::from(axis.abs()) <= limit)
        })
    {
        return if cancelled() { Ok(None) } else { Err(Failure::Reservation) };
    }
    // A single point filter at the prefab origin is invariant under every
    // tested tilt. Anchor filters use radius zero and leave its cache intact.
    let cache_origin = CACHE_ORIGIN
        && prefab.filters.len() == 1
        && prefab.filters[0].position == [0.; 3];
    let mut anchors_prepared = false;
    let mut origin_filter = CliffOriginFilterCache::default();
    let mut best: Option<(V, Q, V)> = None;
    let mut rejection = Failure::Anchor;
    let rotations = if PRUNE {
        distinct_cliff_rotations()
    } else {
        cliff_rotations()
    };
    for (index, &tilt) in rotations.iter().enumerate() {
        if cancelled() {
            return Ok(None);
        }
        if surface.samples == surface.maximum_samples {
            return Err(Failure::Surface);
        }
        surface.samples += 1;
        let filtered = if cache_origin {
            origin_filter.evaluate(surface, prefab, pose)
        } else {
            Ok(())
        };
        let fitted = filtered.and_then(|()| {
            if UNFILTERED_HIGHEST && !anchors_prepared {
                // Each tilt starts from the same transformed scale.
                scaled_anchors.clear();
                scaled_anchors.extend(prefab.anchors.iter().map(|anchor| Anchor {
                    position: mul(anchor.position, pose.scale),
                    extents: anchor.extents * pose.scale[1],
                    offset: anchor.offset * pose.scale[1],
                    radius: anchor.radius,
                    slope_scale: anchor.slope_scale,
                }));
                anchors_prepared = true;
            }
            fit_pose::<UNFILTERED_HIGHEST>(
                surface,
                request.stage,
                prefab,
                OrientedPose {
                    rotation: qmul(pose.rotation, tilt),
                    ..pose
                },
                &options,
                !cache_origin,
                AnchorSearch {
                    best_height: if PRUNE {
                        best.as_ref().map(|pose| pose.0[1])
                    } else {
                        None
                    },
                    scaled: scaled_anchors,
                },
            )
        });
        match fitted {
            Ok(candidate)
                if best
                    .as_ref()
                    .is_none_or(|previous| candidate.0[1] > previous.0[1]) =>
            {
                best = Some(candidate)
            }
            Ok(_) => {}
            Err(Failure::HeightBound) => {}
            Err(reason) if surface.samples == surface.maximum_samples => return Err(reason),
            Err(reason) => {
                if let (Some(Err(_)), Some(work)) = (origin_filter.result, origin_filter.repeat_work) {
                    // The origin filter failed and its repeated query cost
                    // is known. No later tilt can reach anchor fitting.
                    let cost = 1 + work;
                    for _ in &rotations[index + 1..] {
                        if cancelled() {
                            return Ok(None);
                        }
                        let remaining = surface.maximum_samples - surface.samples;
                        surface.samples += cost.min(remaining);
                        if remaining <= cost {
                            return Err(if remaining == cost { reason } else { Failure::Filter });
                        }
                    }
                    return Err(reason);
                }
                rejection = reason;
            }
        }
    }
    best.map(Some).ok_or(rejection)
}

#[allow(clippy::too_many_arguments)]
fn fit_basis(
    surface: &mut Surface<'_>,
    candidate: &decor::Candidate,
    stage: &decor::Stage,
    prefab: &Prefab,
    seed: u64,
    options: &Options<'_>,
    rotation: Q,
    decor: bool,
    point_filters: bool,
) -> Result<(V, Q, V), Failure> {
    let pose = OrientedPose {
        position: candidate.position,
        rotation,
        scale: prefab.scale,
    };
    let pose = if decor {
        apply_transforms(surface, prefab, pose, seed, options)?
    } else {
        if !surface.intact(pose.position) {
            return Err(Failure::Surface);
        }
        pose
    };
    fit_pose::<false>(
        surface,
        stage,
        prefab,
        pose,
        options,
        point_filters,
        AnchorSearch::default(),
    )
}
fn apply_transforms(
    surface: &mut Surface<'_>,
    prefab: &Prefab,
    pose: OrientedPose,
    seed: u64,
    options: &Options<'_>,
) -> Result<OrientedPose, Failure> {
    let OrientedPose {
        position: mut pos,
        rotation: mut rot,
        scale: mut scl,
    } = pose;
    let mut rng = seed;
    if !surface.intact(pos) {
        return Err(Failure::Surface);
    }
    for t in prefab.transforms.iter() {
        match *t {
            Transform::Rotate { min, max } => {
                let r = std::array::from_fn(|i| min[i] + (max[i] - min[i]) * rand(&mut rng));
                rot = qnorm(qmul(euler(r), rot)).ok_or(Failure::Surface)?;
            }
            Transform::Scale { min, max } => {
                let t = rand(&mut rng);
                scl = mul(scl, std::array::from_fn(|i| min[i] + (max[i] - min[i]) * t));
            }
            Transform::Offset { min, max } => {
                let offset = std::array::from_fn(|i| {
                    scl[i] * (min[i] + (max[i] - min[i]) * rand(&mut rng))
                });
                pos = add(pos, offset);
            }
            Transform::Flip { axis } => {
                if rand(&mut rng) <= 0.5 {
                    // Rotating about an existing local axis is right multiplication.
                    // X/Z use local up; Y uses local forward. This is not negative scale.
                    let turn = if axis == 1 {
                        [0., 0., 1., 0.]
                    } else {
                        [0., 1., 0., 0.]
                    };
                    rot = qnorm(qmul(rot, turn)).ok_or(Failure::Surface)?;
                }
            }
            Transform::Swim => {
                let water = options.settings.water.ok_or(Failure::Surface)?;
                let (u, v) = surface.uv(pos).ok_or(Failure::Surface)?;
                if surface.maximum_samples - surface.samples < 4 {
                    surface.samples = surface.maximum_samples;
                    return Err(Failure::Surface);
                }
                surface.samples += 4;
                let n = surface.layers.surface_resolution;
                let cell = idx(n, v) * n + idx(n, u);
                let level = water.height(water.resolution, u, v);
                pos[1] = if level < water.ocean_level
                    && surface.layers.topology[cell] as u32 & water.ocean_mask != 0
                {
                    water.ocean_level
                } else {
                    level
                };
                if pos[1] <= water.terrain_minimum + 1. {
                    pos[1] = 0.;
                }
            }
            Transform::Align { normal, gradient }
            | Transform::AlignSlope {
                normal, gradient, ..
            } => {
                let n = surface.normal(pos).ok_or(Failure::Surface)?;
                let tangent = if (n[1] - 1.).abs() < 1e-7 {
                    [0., 0., 1.]
                } else {
                    cross(n, [0., 1., 0.])
                };
                let downhill = cross(n, tangent);
                if let Transform::AlignSlope {
                    offset,
                    scale: slope_scale,
                    ..
                } = *t
                {
                    let changes_scale = slope_scale != [1.; 3];
                    let slope = if offset != [0.; 3] || changes_scale {
                        (n[1].clamp(-1., 1.).acos() / std::f32::consts::FRAC_PI_2).clamp(0., 1.)
                    } else {
                        // The angle is nonnegative. Multiplying zero offsets
                        // by zero preserves the same signs as the full angle.
                        0.
                    };
                    pos = add(
                        pos,
                        add(
                            add(
                                scale(tangent, offset[0] * slope),
                                scale(n, offset[1] * slope),
                            ),
                            scale(downhill, -offset[2] * slope),
                        ),
                    );
                    if changes_scale {
                        let inverse = [-rot[0], -rot[1], -rot[2], rot[3]];
                        let delta = rotate(inverse, slope_scale.map(|v| v - 1.));
                        scl = mul(scl, delta.map(|v| 1. + v * slope));
                    }
                }
                let up = add(
                    scale(rotate(rot, [0., 1., 0.]), 1. - normal),
                    scale(n, normal),
                );
                let forward = add(
                    scale(rotate(rot, [0., 0., 1.]), 1. - gradient),
                    scale(downhill, gradient),
                );
                rot = look(forward, up).ok_or(Failure::Surface)?;
            }
        }
    }
    Ok(OrientedPose {
        position: pos,
        rotation: rot,
        scale: scl,
    })
}
fn fit_point_filters(
    surface: &mut Surface<'_>,
    prefab: &Prefab,
    pose: OrientedPose,
) -> Result<(), Failure> {
    for f in &prefab.filters {
        let p = add(
            pose.position,
            rotate(pose.rotation, mul(f.position, pose.scale)),
        );
        let (u, v) = surface.uv(p).ok_or(Failure::Surface)?;
        let cell = surface.surface_cell(u, v);
        if surface.alpha[cell] < 128 {
            return Err(Failure::Surface);
        }
        if surface.factor_at(
            cell,
            (u, v),
            &f.filter,
            f.topology_radius,
            f.check_placement,
        ) <= 0.
        {
            return Err(Failure::Filter);
        }
    }
    Ok(())
}
#[derive(Default)]
struct AnchorSearch<'a> {
    best_height: Option<f32>,
    scaled: &'a [Anchor],
}

fn fit_pose<const UNFILTERED_HIGHEST: bool>(
    surface: &mut Surface<'_>,
    stage: &decor::Stage,
    prefab: &Prefab,
    pose: OrientedPose,
    options: &Options<'_>,
    point_filters: bool,
    search: AnchorSearch<'_>,
) -> Result<(V, Q, V), Failure> {
    let OrientedPose {
        position: mut pos,
        rotation: rot,
        scale: scl,
    } = pose;
    if point_filters {
        fit_point_filters(surface, prefab, pose)?;
    }
    let anchors = if UNFILTERED_HIGHEST {
        search.scaled
    } else {
        &prefab.anchors
    };
    if !anchors.is_empty() {
        let average_height = !UNFILTERED_HIGHEST && stage.anchor_mode == 0;
        let surface_side = surface.layers.surface_resolution;
        let height_intervals = surface.layers.height_resolution - 1;
        let shared_grid_shift = if UNFILTERED_HIGHEST && surface_side.is_power_of_two() {
            if height_intervals == surface_side {
                Some(0)
            } else if height_intervals == surface_side * 2 {
                Some(1)
            } else {
                None
            }
        } else {
            None
        };
        let mut average = 0.;
        let mut lo = f32::NEG_INFINITY;
        let mut hi = f32::INFINITY;
        let mut tilt = None;
        for a in anchors {
            let position = if UNFILTERED_HIGHEST {
                a.position
            } else {
                mul(a.position, scl)
            };
            let local = rotate(rot, position);
            let p = add(pos, local);
            let (u, v) = surface.uv(p).ok_or(Failure::Surface)?;
            let h = if let Some(shift) = shared_grid_shift {
                // These axes are powers of two. Projecting once and shifting
                // the lower vertex gives the same alpha cell as separate UV
                // conversion, including the clamped outer edge.
                let height = spawn::HeightPoint::new(surface.layers.height_resolution, u, v);
                if surface.alpha[height.surface_cell(surface_side, shift)] < 128 {
                    return Err(Failure::Surface);
                }
                surface.charge_height().ok_or(Failure::Surface)?;
                height.sample(surface.layers, decode_cliff_height)
            } else {
                let cell = surface.surface_cell(u, v);
                if surface.alpha[cell] < 128 {
                    return Err(Failure::Surface);
                }
                if !UNFILTERED_HIGHEST
                    && stage.filter_mode & 2 != 0
                    && surface.factor_at(cell, (u, v), &stage.filter, 0., true) == 0.
                {
                    return Err(Failure::Filter);
                }
                surface.height_uv(u, v).ok_or(Failure::Surface)?
            };
            // Most rejected rotations do not get as far as a slope-dependent
            // height interval. Compute the angle only on its first use.
            let slope = if a.slope_scale == 0. {
                1.
            } else {
                1. + a.slope_scale * *tilt.get_or_insert_with(|| {
                    (rotate(rot, [0., 1., 0.])[1].clamp(-1., 1.).acos().to_degrees() / 90.)
                        .clamp(0., 1.)
                })
            };
            let (extent, offset) = if UNFILTERED_HIGHEST {
                (a.extents * slope, a.offset * slope)
            } else {
                (a.extents * scl[1] * slope, a.offset * scl[1] * slope)
            };
            let mut amin = h - offset - extent;
            let mut amax = h - offset + extent;
            // Neighborhood samples can only tighten this point's upper bound.
            if search.best_height.is_some_and(|best| amax - local[1] <= best) {
                return Err(Failure::HeightBound);
            }
            if a.radius > 0. {
                let hr = surface.layers.height_resolution;
                let r = a.radius / surface.layers.size as f32;
                if surface.height_cache.is_some() {
                    let rect = [
                        idx(hr, u - r),
                        idx(hr, u + r),
                        idx(hr, v - r),
                        idx(hr, v + r),
                    ];
                    let (minimum, maximum) =
                        surface.height_extrema(rect).ok_or(Failure::Surface)?;
                    // Decoding and the affine offset/extent operations are monotone.
                    // Only the raw extrema can determine these final bounds.
                    amin = amin.max(decode_cliff_height(maximum) - offset - extent);
                    amax = amax.min(decode_cliff_height(minimum) - offset + extent);
                } else {
                    for z in idx(hr, v - r)..=idx(hr, v + r) {
                        for x in idx(hr, u - r)..=idx(hr, u + r) {
                            if surface.samples == surface.maximum_samples {
                                return Err(Failure::Surface);
                            }
                            surface.samples += 1;
                            surface.heights += 1;
                            let h = crate::erosion::decode(surface.layers.height[z * hr + x]);
                            amin = amin.max(h - offset - extent);
                            amax = amax.min(h - offset + extent);
                        }
                    }
                }
            }
            if average_height {
                average += h - local[1];
            }
            lo = lo.max(amin - local[1]);
            hi = hi.min(amax - local[1]);
            // Later anchors can only lower this upper bound. A tilt that
            // cannot exceed the existing winner cannot change the source result.
            if hi < lo {
                return Err(Failure::Anchor);
            }
            if search.best_height.is_some_and(|best| hi <= best) {
                return Err(Failure::HeightBound);
            }
        }
        if hi > 1. && lo < 1. {
            lo = 1.;
        }
        pos[1] = if UNFILTERED_HIGHEST {
            hi
        } else {
            match stage.anchor_mode {
                0 => (average / prefab.anchors.len() as f32).clamp(lo, hi),
                1 => pos[1].clamp(lo, hi),
                _ => hi,
            }
        };
    }
    if !valid(pos)
        || !valid(scl)
        || scl.iter().any(|v| *v <= 0. || *v > 10000.)
        || (options.extended
            || prefab.transforms.iter().any(|transform| matches!(transform, Transform::Offset { .. })))
            && !surface.intact(pos)
    {
        return Err(Failure::Surface);
    }
    Ok((pos, rot, scl))
}
pub fn generate(
    layers: &Layers,
    alpha: &[u8],
    stages: &[decor::Stage],
    candidates: &[decor::Candidate],
    prefabs: &BTreeMap<String, Prefab>,
    seed_value: u32,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    generate_inner(
        layers,
        alpha,
        stages,
        candidates,
        prefabs,
        Options {
            settings: Settings {
                seed: seed_value,
                water: None,
            },
            extended: false,
        },
        cancelled,
    )
}
pub fn generate_extended(
    layers: &Layers,
    alpha: &[u8],
    stages: &[decor::Stage],
    candidates: &[decor::Candidate],
    prefabs: &BTreeMap<String, Prefab>,
    settings: Settings<'_>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    generate_inner(
        layers,
        alpha,
        stages,
        candidates,
        prefabs,
        Options {
            settings,
            extended: true,
        },
        cancelled,
    )
}
fn generate_inner(
    layers: &Layers,
    alpha: &[u8],
    stages: &[decor::Stage],
    candidates: &[decor::Candidate],
    prefabs: &BTreeMap<String, Prefab>,
    options: Options<'_>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    let (hr, n) = dimensions(layers.size)?;
    let cells = n * n;
    if layers.height_resolution != hr
        || layers.surface_resolution != n
        || layers.height.len() != hr * hr
        || layers.biome.len() != cells * 5
        || layers.splat.len() != cells * 8
        || layers.topology.len() != cells
        || alpha.len() != cells
        || candidates.len() > 100000
        || stages.len() > 128
        || prefabs.len() > 4096
        || layers.height.iter().any(|h| !(0..=32766).contains(h))
    {
        return Err("Invalid static decoration fitting input".into());
    }
    if let Some(water) = options.settings.water
        && (water.resolution != hr
            || water.local_heights.len() != hr * hr
            || water.local_heights.iter().any(|v| !(0..=32766).contains(v))
            || water.ocean_mask == 0
            || water.ocean_mask & 0x8000_0000 != 0
            || !water.ocean_level.is_finite()
            || !water.terrain_minimum.is_finite()
            || !(-500. ..=500.).contains(&water.ocean_level)
            || water.terrain_minimum != -500.)
    {
        return Err("Invalid decoration water context".into());
    }
    if prefabs.values().any(|p| {
        p.unsupported.is_empty() && p.transforms.iter().any(|t| matches!(t, Transform::Swim))
    }) && options.settings.water.is_none()
    {
        return Err("DecorSwim requires a retained water raster".into());
    }
    for s in stages {
        s.validate()?;
        if !(0..=2).contains(&s.anchor_mode) {
            return Err("Unknown terrain anchor mode".into());
        }
    }
    for p in prefabs.values() {
        p.validate()?;
    }
    for c in candidates {
        if !valid(c.position)
            || stages
                .get(c.stage)
                .and_then(|s| s.prefabs.get(c.prefab))
                .is_none_or(|p| !prefabs.contains_key(p))
        {
            return Err("Invalid decoration candidate reference".into());
        }
    }
    let mut surface = Surface {
        height_cache: None,
        topology_cache: None,
        maximum_samples: MAX_SAMPLES,
        layers: layers.view(),
        alpha,
        samples: 0,
        heights: 0,
    };
    let mut out = Report {
        engine: if options.extended {
            EXTENDED_VERSION
        } else {
            VERSION
        },
        poses: Vec::new(),
        counts: Counts::default(),
        prefabs: BTreeMap::new(),
        height_samples: 0,
        sample_visits: 0,
        unprocessed_candidates: 0,
        budget_exhausted: false,
        physics_validated: false,
    };
    for (i, c) in candidates.iter().enumerate() {
        if i % 256 == 0 && cancelled() {
            return Ok(None);
        }
        if surface.samples == surface.maximum_samples {
            out.budget_exhausted = true;
            out.unprocessed_candidates = candidates.len() - i;
            break;
        }
        let stage = &stages[c.stage];
        let path = &stage.prefabs[c.prefab];
        let p = &prefabs[path];
        let row = out
            .prefabs
            .entry(path.clone())
            .or_insert_with(|| PrefabReport {
                counts: Counts::default(),
                unsupported: p.unsupported.clone(),
                pending: p.pending.clone(),
            });
        row.counts.attempted += 1;
        out.counts.attempted += 1;
        if !p.unsupported.is_empty() {
            row.counts.unsupported += 1;
            out.counts.unsupported += 1;
            continue;
        }
        match fit(
            &mut surface,
            c,
            stage,
            p,
            seed(options.settings.seed, &stage.id, path, c.position),
            &options,
        ) {
            Ok((position, rotation, scale)) => {
                out.poses.push(Pose {
                    candidate: i,
                    position,
                    rotation,
                    scale,
                });
                row.counts.fitted += 1;
                out.counts.fitted += 1;
            }
            Err(_) if surface.samples == surface.maximum_samples => {
                row.counts.budget_limited += 1;
                out.counts.budget_limited += 1;
                out.budget_exhausted = true;
                out.unprocessed_candidates = candidates.len() - i - 1;
                break;
            }
            Err(Failure::Filter) => {
                row.counts.rejected_filter += 1;
                out.counts.rejected_filter += 1;
            }
            Err(Failure::Anchor | Failure::HeightBound) => {
                row.counts.rejected_anchors += 1;
                out.counts.rejected_anchors += 1;
            }
            Err(Failure::Surface | Failure::Reservation) => {
                row.counts.rejected_bounds_or_holes += 1;
                out.counts.rejected_bounds_or_holes += 1;
            }
        }
    }
    out.height_samples = surface.heights;
    out.sample_visits = surface.samples;
    Ok((!cancelled()).then_some(out))
}

#[cfg(test)]
mod tests {
    #[test]
    fn cliff_height_table_matches_the_terrain_decoder() {
        for sample in i16::MIN..=i16::MAX {
            assert_eq!(
                decode_cliff_height(sample).to_bits(),
                crate::erosion::decode(std::hint::black_box(sample)).to_bits(),
            );
        }
    }

    #[test]
    fn prepared_cliff_stage_requires_the_checked_roster_and_checks_each_candidate() {
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let s = stage();
        let copied = s.clone();
        let validated = ValidatedStage::new(&s).unwrap();
        let p = PreparedPrefab::new(prefab()).unwrap();
        let c = candidate();
        let mut fitting = OrientedBudget::new(&l, &alpha).unwrap();
        let request = |candidate, stage| StaticRequest {
            candidate,
            stage,
            prefab: &p,
            seed: 42,
            water: None,
        };
        assert!(
            fitting
                .try_fit_uniform_cliff_prepared(l.view(), request(&c, &copied), &validated, None, || {
                    false
                })
                .is_err()
        );
        let bad_candidate = decor::Candidate {
            prefab: s.prefabs.len(),
            ..candidate()
        };
        assert!(
            fitting
                .try_fit_uniform_cliff_prepared(
                    l.view(),
                    request(&bad_candidate, &s),
                    &validated,
                    None,
                    || false
                )
                .is_err()
        );
        let mut view = l.view();
        view.size += 1;
        assert!(
            fitting
                .try_fit_uniform_cliff_prepared(view, request(&c, &s), &validated, None, || false)
                .is_err()
        );
        assert!(
            fitting
                .try_fit_uniform_cliff_prepared(l.view(), request(&c, &s), &validated, None, || true)
                .unwrap()
                .is_none()
        );
        assert_eq!(fitting.samples(), 0);
        let mut invalid = s.clone();
        invalid.prefabs.push("../invalid.prefab".into());
        assert!(ValidatedStage::new(&invalid).is_err());
    }

    #[test]
    fn prepared_cliff_stage_matches_checked_entry_and_work_limits() {
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let s = stage();
        let validated = ValidatedStage::new(&s).unwrap().with_seed(42);
        let mut p = prefab();
        p.transforms.push(Transform::Rotate {
            min: [0., -180., 0.],
            max: [0., 180., 0.],
        });
        p.anchors.push(Anchor {
            position: [2., 0., 3.],
            extents: 2.5,
            offset: 0.,
            radius: 1.,
            slope_scale: 0.5,
        });
        let p = PreparedPrefab::new(p).unwrap();
        let signature = |outcome| match outcome {
            Some(OrientedOutcome::Fitted(pose)) => serde_json::to_string(&pose).unwrap(),
            Some(OrientedOutcome::Rejected(reason)) => format!("{reason:?}"),
            None => "cancelled".into(),
        };
        for limit in [4, 64, 1000, 64000] {
            let mut plain = OrientedBudget::new(&l, &alpha)
                .unwrap()
                .with_sample_limit(limit)
                .unwrap();
            let mut prepared = OrientedBudget::new(&l, &alpha)
                .unwrap()
                .with_sample_limit(limit)
                .unwrap();
            for x in [-100., 0., 100.] {
                let c = decor::Candidate {
                    position: [x, 50., 0.],
                    ..candidate()
                };
                let request = || StaticRequest {
                    candidate: &c,
                    stage: &s,
                    prefab: &p,
                    seed: if x > 0. { 43 } else { 42 },
                    water: None,
                };
                let a = plain
                    .try_fit_uniform_cliff(l.view(), request(), || false)
                    .unwrap();
                let b = prepared
                    .try_fit_uniform_cliff_prepared(l.view(), request(), &validated, None, || false)
                    .unwrap();
                assert_eq!(signature(a), signature(b));
                assert_eq!(plain.samples(), prepared.samples());
                assert_eq!(plain.height_samples(), prepared.height_samples());
            }
        }
    }
    #[test]
    fn cliff_topology_cache_checks_full_queries_at_the_same_center() {
        use crate::topology_query::Query;

        let query = Query {
            rect: [4089, 4095, 0, 8],
            center: [4095, 3],
            radius: 6.,
            relevant: 7,
        };
        let second = Query { relevant: 3, ..query };
        let mut cache = CliffTopologyCache::default();
        assert_eq!(cache.get(&query), None);
        cache.insert(query, 7);
        assert_eq!(cache.get(&second), None);
        cache.insert(second, 2);
        assert_eq!(cache.get(&query), Some(7));
        assert_eq!(cache.get(&second), Some(2));

        for x in 0..15 {
            cache.insert(Query { center: [x, 3], ..query }, 0);
        }
        assert_eq!(cache.get(&query), None);
        assert_eq!(cache.get(&second), Some(2));

        cache.reset();
        assert_eq!(cache.get(&second), None);
        cache.insert(query, 5);
        assert_eq!(cache.get(&query), Some(5));
        assert_eq!(cache.get(&second), None);
    }

    #[test]
    fn shared_height_projection_keeps_anchor_alpha_cells_at_grid_edges() {
        for side in [512, 1024, 2048] {
            for shift in [0, 1] {
                let height = (side << shift) + 1;
                for vertex in 0..height {
                    let coordinate = vertex as f32 / (height - 1) as f32;
                    let bits = coordinate.to_bits();
                    for bits in [bits.saturating_sub(1), bits, bits + 1] {
                        let u = f32::from_bits(bits).min(1.);
                        let v = 1. - u;
                        let point = spawn::HeightPoint::new(height, u, v);
                        assert_eq!(point.surface_cell(side, shift), idx(side, v) * side + idx(side, u));
                    }
                }
            }
        }
    }

    #[test]
    fn reused_cliff_height_cache_starts_each_view_cold_even_after_generation_wrap() {
        let mut layers = layers();
        let mut cache = CliffHeightCache::default();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let rect = [0, 1, 0, 0];
        let check = |layers: &mut Layers, cache: &mut CliffHeightCache, value| {
            layers.height[..2].fill(value);
            let mut surface = Surface {
                height_cache: Some(cache),
                topology_cache: None,
                maximum_samples: 3,
                layers: layers.view(),
                alpha: &alpha,
                samples: 0,
                heights: 0,
            };
            assert_eq!(surface.height_extrema(rect), Some((value, value)));
            assert_eq!(surface.height_extrema(rect), Some((value, value)));
            assert_eq!(surface.height_extrema(rect), None);
            assert_eq!((surface.samples, surface.heights), (3, 2));
        };
        check(&mut layers, &mut cache, 16000);
        cache.reset();
        let (key, slot) = CliffHeightCache::key(rect);
        assert!(cache.get(key, slot).is_none());
        cache.generation = u32::MAX;
        cache.reset();
        assert!(cache.get(key, slot).is_none());
        check(&mut layers, &mut cache, 17000);
        cache.reset();
        check(&mut layers, &mut cache, 18000);
    }

    #[test]
    fn cliff_height_windows_charge_partial_budgets_without_caching_incomplete_reads() {
        let mut layers = layers();
        let hr = layers.height_resolution;
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let rect = [hr - 3, hr - 1, hr - 2, hr - 1];
        layers.height[hr * (hr - 2) + hr - 3..hr * (hr - 1)]
            .copy_from_slice(&[16382, 16383, 16384]);
        layers.height[hr * hr - 3..].copy_from_slice(&[16000, 32766, 16383]);
        let mut cache = CliffHeightCache::default();
        let mut surface = Surface {
            height_cache: Some(&mut cache),
            topology_cache: None,
            maximum_samples: 5,
            layers: layers.view(),
            alpha: &alpha,
            samples: 0,
            heights: 0,
        };

        assert_eq!(surface.height_extrema(rect), None);
        assert_eq!((surface.samples, surface.heights), (5, 5));
        surface.maximum_samples = 12;
        assert_eq!(surface.height_extrema(rect), Some((16000, 32766)));
        assert_eq!((surface.samples, surface.heights), (11, 11));
        assert_eq!(surface.height_extrema(rect), Some((16000, 32766)));
        assert_eq!((surface.samples, surface.heights), (12, 11));
    }

    #[test]
    fn cliff_cached_height_extrema_match_ordered_float_bounds_and_charge_cache_hits() {
        let mut l = layers();
        let hr = l.height_resolution;
        let alpha = vec![255; l.surface_resolution.pow(2)];
        for raw in [
            [16382, 16383, 16384, 16383],
            [0, 32766, 16000, 32766],
            [20000; 4],
        ] {
            for (i, height) in [0, 1, hr, hr + 1].into_iter().zip(raw) {
                l.height[i] = height;
            }
            let mut cache = CliffHeightCache::default();
            let mut surface = Surface {
                height_cache: Some(&mut cache),
                topology_cache: None,
                maximum_samples: MAX_SAMPLES,
                layers: l.view(),
                alpha: &alpha,
                samples: 0,
                heights: 0,
            };
            let mut queries = 0;
            for center in [0., -0., 20.0001, crate::erosion::decode(raw[0])] {
                for offset in [
                    0.,
                    -0.,
                    0.1,
                    -0.1,
                    2.5,
                    -30.,
                    crate::erosion::decode(raw[2]),
                ] {
                    for extent in [0., -0., 0.3, 2.5] {
                        let (mut low, mut high) =
                            (center - offset - extent, center - offset + extent);
                        for height in raw.map(crate::erosion::decode) {
                            low = low.max(height - offset - extent);
                            high = high.min(height - offset + extent);
                        }
                        let (minimum, maximum) = surface.height_extrema([0, 1, 0, 1]).unwrap();
                        queries += 1;
                        let cached_low = (center - offset - extent)
                            .max(crate::erosion::decode(maximum) - offset - extent);
                        let cached_high = (center - offset + extent)
                            .min(crate::erosion::decode(minimum) - offset + extent);
                        assert_eq!(low.to_bits(), cached_low.to_bits());
                        assert_eq!(high.to_bits(), cached_high.to_bits());
                    }
                }
            }
            assert_eq!(surface.heights, 4);
            assert_eq!(surface.samples, queries + 3);
        }
    }

    #[test]
    fn cliff_small_height_windows_match_each_edge_shape() {
        let mut l = layers();
        let n = l.height_resolution;
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let vertices = (n - 3..n).flat_map(|z| (n - 3..n).map(move |x| z * n + x));
        for (index, height) in vertices.zip([12, 260, 54, 180, 140, 64, 18, 211, 105]) {
            l.height[index] = height;
        }
        for (rect, expected, count) in [
            ([n - 1, n - 1, n - 1, n - 1], (105, 105), 1),
            ([n - 2, n - 1, n - 1, n - 1], (105, 211), 2),
            ([n - 1, n - 1, n - 2, n - 1], (64, 105), 2),
            ([n - 2, n - 1, n - 2, n - 1], (64, 211), 4),
            ([n - 3, n - 1, n - 2, n - 1], (18, 211), 6),
            ([n - 3, n - 1, n - 2, n - 2], (64, 180), 3),
            ([n - 3, n - 1, n - 3, n - 1], (12, 260), 9),
        ] {
            let mut cache = CliffHeightCache::default();
            let mut surface = Surface {
                height_cache: Some(&mut cache),
                topology_cache: None,
                maximum_samples: count + 1,
                layers: l.view(),
                alpha: &alpha,
                samples: 0,
                heights: 0,
            };
            assert_eq!(surface.height_extrema(rect), Some(expected));
            assert_eq!(surface.height_extrema(rect), Some(expected));
            assert_eq!(surface.height_extrema(rect), None);
            assert_eq!((surface.samples, surface.heights), (count + 1, count));
        }
    }

    #[test]
    fn cliff_height_pruning_matches_full_trials_across_terrain_and_authored_transforms() {
        let mut l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let mut s = stage();
        s.anchor_mode = 2;
        let mut total_full = 0;
        let mut total_pruned = 0;
        let mut fitted = 0;
        for terrain in 0..4 {
            let hr = l.height_resolution;
            for (i, height) in l.height.iter_mut().enumerate() {
                let x = (i % hr) as f32;
                let z = (i / hr) as f32;
                *height = crate::erosion::encode(
                    30. + terrain as f32 * ((x * 0.09).sin() * 2. + (z * 0.13).cos() * 1.5),
                );
            }
            for sample in 0..64 {
                let mut p = prefab();
                p.transforms = vec![
                    Transform::Align {
                        normal: 1.,
                        gradient: if sample % 2 == 0 { 0. } else { 1. },
                    },
                    Transform::Rotate {
                        min: [0., 15., 0.],
                        max: [0., 175., 0.],
                    },
                    Transform::Scale {
                        min: [0.75; 3],
                        max: [1.5; 3],
                    },
                ];
                for x in [-12., 0., 12.] {
                    for z in [-8., 0., 8.] {
                        p.anchors.push(Anchor {
                            position: [x, 0., z],
                            extents: 2.5,
                            offset: 0.,
                            radius: if sample % 2 == 0 { 0. } else { 1. },
                            slope_scale: 0.5,
                        });
                    }
                }
                let prepared = PreparedPrefab::new(p).unwrap();
                let c = decor::Candidate {
                    position: [
                        sample as f32 * 3. - 100.,
                        30.,
                        (sample % 7) as f32 * 9. - 25.,
                    ],
                    ..candidate()
                };
                let request = StaticRequest {
                    candidate: &c,
                    stage: &s,
                    prefab: &prepared,
                    seed: 42,
                    water: None,
                };
                let surface = || Surface {
                    height_cache: None,
                    topology_cache: None,
                    maximum_samples: MAX_SAMPLES,
                    layers: l.view(),
                    alpha: &alpha,
                    samples: 0,
                    heights: 0,
                };
                let mut full = surface();
                let mut cache = CliffHeightCache::default();
                let mut pruned = Surface {
                    height_cache: Some(&mut cache),
                    ..surface()
                };
                let mut scaled = Vec::new();
                let reference = fit_uniform_cliff::<false, false, false>(
                    &mut full, &request, None, None, &mut scaled, &|| false,
                );
                let optimized = fit_uniform_cliff::<true, true, false>(
                    &mut pruned, &request, None, None, &mut scaled, &|| false,
                );
                match (reference, optimized) {
                    (Ok(Some(a)), Ok(Some(b))) => {
                        fitted += 1;
                        assert_eq!(a.0.map(f32::to_bits), b.0.map(f32::to_bits));
                        assert_eq!(a.1.map(f32::to_bits), b.1.map(f32::to_bits));
                        assert_eq!(a.2.map(f32::to_bits), b.2.map(f32::to_bits));
                    }
                    (Err(a), Err(b)) => assert_eq!(format!("{a:?}"), format!("{b:?}")),
                    _ => panic!("Pruning changed cliff fitting"),
                }
                total_full += full.heights;
                total_pruned += pruned.heights;
            }
        }
        assert!(fitted > 0);
        assert!(total_pruned < total_full);
    }

    fn origin_filtered_cliff() -> Prefab {
        let mut p = prefab();
        p.transforms = vec![
            Transform::Align {
                normal: 1.,
                gradient: 0.,
            },
            Transform::Rotate {
                min: [0., -180., 0.],
                max: [0., 180., 0.],
            },
            Transform::Scale {
                min: [0.75; 3],
                max: [1.1; 3],
            },
        ];
        p.filters.push(PointFilter {
            position: [0.; 3],
            filter: spawn::Filter {
                splat_type: -1,
                biome_type: -1,
                topology_any: -1,
                topology_all: 0,
                topology_not: 8,
            },
            topology_radius: 3.5,
            check_placement: false,
        });
        for x in [-4., 0., 4.] {
            for z in [-4., 0., 4.] {
                p.anchors.push(Anchor {
                    position: [x, 0., z],
                    extents: 1.25,
                    offset: 1.25,
                    radius: 1.,
                    slope_scale: 1.,
                });
            }
        }
        p
    }

    #[test]
    fn cliff_cached_and_specialized_fits_preserve_poses_work_limits_and_cancellation() {
        let mut l = layers();
        let n = l.surface_resolution;
        let hr = l.height_resolution;
        for (i, h) in l.height.iter_mut().enumerate() {
            *h = crate::erosion::encode(50. + (i % hr) as f32 * 0.004);
        }
        for (i, topology) in l.topology.iter_mut().enumerate() {
            *topology = if (i % n / 16).is_multiple_of(5) { 9 } else { 1 };
        }
        let index = crate::topology_query::Index::new(&l.topology, n, &|| false)
            .unwrap()
            .unwrap();
        let foreign = vec![1; n * n];
        let foreign_index = crate::topology_query::Index::new(&foreign, n, &|| false)
            .unwrap()
            .unwrap();
        let mut alpha = vec![255; n * n];
        alpha[n / 2 * n + n / 2] = 0;
        let mut blockers = vec![0; n * n];
        blockers[n / 2 * n + n / 2 + 5] = 1;
        let mut s = stage();
        s.anchor_mode = 2;
        for indexed in 0..6 {
            s.filter_mode = if indexed < 3 { 3 } else { 1 };
            let view = crate::native_layers::View {
                topology_tiles: match indexed % 3 {
                    0 => None,
                    1 => Some(index.view(&l.topology)),
                    _ => Some(foreign_index.view(&foreign)),
                },
                placement_blockers: Some(&blockers),
                ..l.view()
            };
            for sample in 0..12 {
                let mut p = origin_filtered_cliff();
                p.filters[0].position[0] = match sample % 4 {
                    0 => 0.,
                    1 => -0.,
                    _ => 1.,
                };
                p.filters[0].check_placement = sample % 2 == 0;
                p.filters[0].topology_radius = [0., 3.5, 8.][sample % 3];
                if sample % 5 == 0 {
                    p.filters[0].filter.splat_type = 2;
                }
                if sample % 4 == 3 {
                    p.filters.push(p.filters[0].clone());
                }
                let prepared = PreparedPrefab::new(p).unwrap();
                let c = decor::Candidate {
                    position: [
                        [0., 10., -499.8, 499.8][sample % 4],
                        50.,
                        sample as f32 * 3.,
                    ],
                    ..candidate()
                };
                let request = StaticRequest {
                    candidate: &c,
                    stage: &s,
                    prefab: &prepared,
                    seed: 42,
                    water: None,
                };
                for limit in [1, 4, 5, 8, 32, 91, 92, 181, 182, 183, 1000, 10000] {
                    for cancel_after in [7, usize::MAX] {
                        let mut old_heights = CliffHeightCache::default();
                        let mut new_heights = CliffHeightCache::default();
                        let mut old_topology = CliffTopologyCache::default();
                        let mut new_topology = CliffTopologyCache::default();
                        let mut old = Surface {
                            height_cache: Some(&mut old_heights),
                            topology_cache: Some(&mut old_topology),
                            maximum_samples: limit,
                            layers: view,
                            alpha: &alpha,
                            samples: 0,
                            heights: 0,
                        };
                        let mut cached = Surface {
                            height_cache: Some(&mut new_heights),
                            topology_cache: Some(&mut new_topology),
                            maximum_samples: limit,
                            layers: view,
                            alpha: &alpha,
                            samples: 0,
                            heights: 0,
                        };
                        let old_checks = std::cell::Cell::new(0);
                        let new_checks = std::cell::Cell::new(0);
                        let mut scaled = Vec::new();
                        let cancel_reference = || {
                            old_checks.set(old_checks.get() + 1);
                            old_checks.get() >= cancel_after
                        };
                        let reference = fit_uniform_cliff::<true, false, false>(
                            &mut old, &request, None, None, &mut scaled, &cancel_reference,
                        );
                        let cancel_optimized = || {
                            new_checks.set(new_checks.get() + 1);
                            new_checks.get() >= cancel_after
                        };
                        let optimized = if s.filter_mode == 1 {
                            fit_uniform_cliff::<true, true, true>(
                                &mut cached, &request, None, None, &mut scaled, &cancel_optimized,
                            )
                        } else {
                            fit_uniform_cliff::<true, true, false>(
                                &mut cached, &request, None, None, &mut scaled, &cancel_optimized,
                            )
                        };
                        match (reference, optimized) {
                            (Ok(Some(a)), Ok(Some(b))) => {
                                assert_eq!(a.0.map(f32::to_bits), b.0.map(f32::to_bits));
                                assert_eq!(a.1.map(f32::to_bits), b.1.map(f32::to_bits));
                                assert_eq!(a.2.map(f32::to_bits), b.2.map(f32::to_bits));
                            }
                            (Ok(None), Ok(None)) => {}
                            (Err(a), Err(b)) => assert_eq!(format!("{a:?}"), format!("{b:?}")),
                            _ => panic!("Origin filter caching changed cliff fitting"),
                        }
                        assert_eq!(old.samples, cached.samples);
                        assert_eq!(old.heights, cached.heights);
                        assert_eq!(old_checks.get(), new_checks.get());
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "Controlled cliff fitting microbenchmark"]
    fn cliff_origin_filter_cache_benchmark() {
        use std::hash::{Hash, Hasher};
        fn measure<const CACHE: bool, const UNFILTERED_HIGHEST: bool>(
            view: crate::native_layers::View<'_>,
            alpha: &[u8],
            stage: &decor::Stage,
            prefab: &PreparedPrefab,
        ) -> (f64, u64) {
            let mut signature = std::collections::hash_map::DefaultHasher::new();
            let started = std::time::Instant::now();
            let mut anchors = Vec::new();
            for i in 0..10000 {
                let c = decor::Candidate {
                    position: [
                        (i % 128) as f32 * 2. - 128.,
                        50.,
                        (i / 128) as f32 * 2. - 80.,
                    ],
                    ..candidate()
                };
                let request = StaticRequest {
                    candidate: &c,
                    stage,
                    prefab,
                    seed: 42,
                    water: None,
                };
                let mut heights = CliffHeightCache::default();
                let mut topology = CliffTopologyCache::default();
                let mut surface = Surface {
                    height_cache: Some(&mut heights),
                    topology_cache: Some(&mut topology),
                    maximum_samples: MAX_SAMPLES,
                    layers: view,
                    alpha,
                    samples: 0,
                    heights: 0,
                };
                match fit_uniform_cliff::<true, CACHE, UNFILTERED_HIGHEST>(
                    &mut surface, &request, None, None, &mut anchors, &|| false,
                ) {
                    Ok(Some(pose)) => {
                        pose.0.map(f32::to_bits).hash(&mut signature);
                        pose.1.map(f32::to_bits).hash(&mut signature);
                        pose.2.map(f32::to_bits).hash(&mut signature);
                    }
                    Err(reason) => std::mem::discriminant(&reason).hash(&mut signature),
                    Ok(None) => panic!("Unexpected benchmark cancellation"),
                }
                surface.samples.hash(&mut signature);
                surface.heights.hash(&mut signature);
            }
            (started.elapsed().as_secs_f64(), signature.finish())
        }
        let mut l = layers();
        let n = l.surface_resolution;
        let hr = l.height_resolution;
        for (i, h) in l.height.iter_mut().enumerate() {
            *h = crate::erosion::encode(50. + ((i % hr) as f32 * 0.02).sin() * 3.);
        }
        for (i, topology) in l.topology.iter_mut().enumerate() {
            *topology = if (i % n / 13).is_multiple_of(7) { 9 } else { 1 };
        }
        let index = crate::topology_query::Index::new(&l.topology, n, &|| false)
            .unwrap()
            .unwrap();
        let view = crate::native_layers::View {
            topology_tiles: Some(index.view(&l.topology)),
            ..l.view()
        };
        let alpha = vec![255; n * n];
        let mut s = stage();
        s.anchor_mode = 2;
        let p = PreparedPrefab::new(origin_filtered_cliff()).unwrap();
        for mode in [3, 1] {
            s.filter_mode = mode;
            let reference = || {
                if mode == 3 {
                    measure::<false, false>(view, &alpha, &s, &p)
                } else {
                    measure::<true, false>(view, &alpha, &s, &p)
                }
            };
            let candidate = || {
                if mode == 3 {
                    measure::<true, false>(view, &alpha, &s, &p)
                } else {
                    measure::<true, true>(view, &alpha, &s, &p)
                }
            };
            for round in 0..7 {
                let (reference, candidate) = if round % 2 == 0 {
                    (reference(), candidate())
                } else {
                    let candidate = candidate();
                    (reference(), candidate)
                };
                assert_eq!(reference.1, candidate.1);
                println!(
                    "{}",
                    serde_json::json!({"filter_mode":mode,"round":round,"trials":10000,
                        "reference_seconds":reference.0,"candidate_seconds":candidate.0,"signature":reference.1})
                );
            }
        }
    }

    #[test]
    fn cliff_topology_cache_preserves_continuous_weights_and_exact_query_inputs() {
        let mut l = layers();
        let n = l.surface_resolution;
        for (i, value) in l.topology.iter_mut().enumerate() {
            *value = 1 | if i % 17 == 0 { 2 } else { 0 };
        }
        for (i, value) in l.splat.iter_mut().enumerate() {
            *value = (i.wrapping_mul(37) % 256) as u8;
        }
        let index = crate::topology_query::Index::new(&l.topology, n, &|| false)
            .unwrap()
            .unwrap();
        let view = crate::native_layers::View {
            topology_tiles: Some(index.view(&l.topology)),
            ..l.view()
        };
        let alpha = vec![255; n * n];
        let mut cache = CliffTopologyCache::default();
        {
            let mut plain = Surface {
                height_cache: None,
                topology_cache: None,
                maximum_samples: MAX_SAMPLES,
                layers: view,
                alpha: &alpha,
                samples: 0,
                heights: 0,
            };
            let mut cached = Surface {
                height_cache: None,
                topology_cache: Some(&mut cache),
                maximum_samples: MAX_SAMPLES,
                layers: view,
                alpha: &alpha,
                samples: 0,
                heights: 0,
            };
            for i in 0..256 {
                let p = [
                    (0.49 + (i % 4) as f32 * 0.00003 - 0.5) * 1000.,
                    0.,
                    (0.51 + (i % 3) as f32 * 0.00004 - 0.5) * 1000.,
                ];
                for radius in [3.5, 10., 14.7] {
                    for forbidden in [0, 2, 8] {
                        let filter = spawn::Filter {
                            splat_type: if i % 2 == 0 { 1 } else { 3 },
                            biome_type: if i % 3 == 0 { -1 } else { 2 },
                            topology_any: -1,
                            topology_all: 1,
                            topology_not: forbidden,
                        };
                        let (u, v) = plain.uv(p).unwrap();
                        assert_eq!(
                            plain.factor_uv(u, v, &filter, radius, true).to_bits(),
                            cached.factor_uv(u, v, &filter, radius, true).to_bits()
                        );
                    }
                }
            }
            assert!(cached.samples < plain.samples);
        }
        assert!(cache.entries.iter().any(Option::is_some));
        let mut empty = CliffTopologyCache::default();
        {
            let mut bounded = Surface {
                height_cache: None,
                topology_cache: Some(&mut empty),
                maximum_samples: 1,
                layers: view,
                alpha: &alpha,
                samples: 0,
                heights: 0,
            };
            let filter = spawn::Filter {
                splat_type: -1,
                biome_type: -1,
                topology_any: -1,
                topology_all: 1,
                topology_not: 8,
            };
            assert_eq!(bounded.factor_uv(0.5, 0.5, &filter, 14.7, true), 0.);
            assert_eq!(bounded.samples, 1);
        }
        assert!(empty.entries.iter().all(Option::is_none));
    }
    #[test]
    fn cliff_search_applies_decor_once_and_retains_the_first_equal_height_pose() {
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let s = stage();
        let c = candidate();
        let mut p = prefab();
        p.transforms = vec![
            Transform::Align {
                normal: 0.,
                gradient: 0.,
            },
            Transform::Scale {
                min: [2., 3., 4.],
                max: [2., 3., 4.],
            },
        ];
        let prepared = PreparedPrefab::new(p).unwrap();
        let mut fitting = OrientedBudget::new(&l, &alpha).unwrap();
        let Some(OrientedOutcome::Fitted(pose)) = fitting
            .try_fit_uniform_cliff(
                l.view(),
                StaticRequest {
                    candidate: &c,
                    stage: &s,
                    prefab: &prepared,
                    seed: 42,
                    water: None,
                },
                || false,
            )
            .unwrap()
        else {
            panic!("Missing fitted cliff")
        };
        assert_eq!(cliff_rotations().len(), 112);
        assert_eq!(pose.position, c.position);
        assert_eq!(pose.scale, [2., 3., 4.]);
        assert_eq!(pose.rotation, [0., 0., 0., 1.]);
        assert_eq!(fitting.height_samples(), 4);
        assert_eq!(fitting.samples(), 95);
        assert_eq!(distinct_cliff_rotations().len(), 91);
    }
    #[test]
    fn cliff_search_selects_the_highest_anchored_pose_and_rejects_partial_searches() {
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let mut s = stage();
        s.anchor_mode = 2;
        let c = candidate();
        let mut p = prefab();
        p.anchors.push(Anchor {
            position: [20., 0., 0.],
            extents: 0.2,
            offset: 0.,
            radius: 0.,
            slope_scale: 0.,
        });
        let prepared = PreparedPrefab::new(p).unwrap();
        for limit in [181, 182] {
            let mut fitting = OrientedBudget::new(&l, &alpha)
                .unwrap()
                .with_sample_limit(limit)
                .unwrap();
            let outcome = fitting
                .try_fit_uniform_cliff(
                    l.view(),
                    StaticRequest {
                        candidate: &c,
                        stage: &s,
                        prefab: &prepared,
                        seed: 42,
                        water: None,
                    },
                    || false,
                )
                .unwrap()
                .unwrap();
            if limit == 181 {
                assert!(matches!(
                    outcome,
                    OrientedOutcome::Rejected(OrientedRejection::Budget)
                ));
            } else {
                let OrientedOutcome::Fitted(pose) = outcome else {
                    panic!("Missing complete cliff search")
                };
                let expected =
                    crate::erosion::decode(l.height[0]) + 0.2 + 20. * 12_f32.to_radians().sin();
                assert!((pose.position[1] - expected).abs() < 0.0001);
                assert!(rotate(pose.rotation, [1., 0., 0.])[1] < -0.2);
            }
        }
        let polls = std::cell::Cell::new(0);
        let mut fitting = OrientedBudget::new(&l, &alpha).unwrap();
        assert!(
            fitting
                .try_fit_uniform_cliff(
                    l.view(),
                    StaticRequest {
                        candidate: &c,
                        stage: &s,
                        prefab: &prepared,
                        seed: 42,
                        water: None
                    },
                    || {
                        polls.set(polls.get() + 1);
                        polls.get() == 10
                    }
                )
                .unwrap()
                .is_none()
        );
        assert!(fitting.samples() > 0 && fitting.samples() < 182);
    }
    use super::*;
    #[test]
    fn explicit_live_sample_limits_are_bounded_shared_and_cannot_reset_consumed_work() {
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let mut invalid_limits = vec![0, 3];
        if let Ok(invalid) = usize::try_from(MAXIMUM_FITTING_SAMPLES + 1) { invalid_limits.push(invalid); }
        for invalid in invalid_limits {
            assert!(
                OrientedBudget::new(&l, &alpha)
                    .unwrap()
                    .with_sample_limit(invalid)
                    .is_err()
            );
        }
        let s = stage();
        let c = candidate();
        let mut p = prefab();
        p.anchors.push(Anchor {
            position: [0.; 3],
            extents: 2.,
            offset: 0.,
            radius: 0.,
            slope_scale: 0.,
        });
        let prepared = PreparedPrefab::new(p).unwrap();
        let mut limits = vec![MAX_SAMPLES, 256_000_000];
        if let Ok(maximum) = usize::try_from(MAXIMUM_FITTING_SAMPLES) { limits.push(maximum); }
        for limit in limits {
            let mut budget = OrientedBudget::new(&l, &alpha)
                .unwrap()
                .with_sample_limit(limit)
                .unwrap();
            budget.samples = MAX_SAMPLES;
            let result = budget
                .try_fit_static(
                    l.view(),
                    StaticRequest {
                        candidate: &c,
                        stage: &s,
                        prefab: &prepared,
                        seed: 42,
                        water: None,
                    },
                )
                .unwrap();
            if limit == MAX_SAMPLES {
                assert!(matches!(
                    result,
                    OrientedOutcome::Rejected(OrientedRejection::Budget)
                ));
            } else {
                assert!(matches!(result, OrientedOutcome::Fitted(_)));
                let before = budget.samples();
                assert!(before > MAX_SAMPLES);
                let result = budget
                    .try_fit(
                        l.view(),
                        OrientedRequest {
                            position: c.position,
                            rotation: [0., 0., 0., 1.],
                            filter: &s.filter,
                            prefab: &prepared,
                            seed: 42,
                            apply_decor: true,
                            check_anchor_filter: true,
                            anchor_mode: 0,
                        },
                    )
                    .unwrap();
                assert!(matches!(result, OrientedOutcome::Fitted(_)));
                assert!(budget.samples() > before);
            }
            assert!(budget.with_sample_limit(256_000_000).is_err());
        }
        let water = vec![0; l.height_resolution.pow(2)];
        let water = ValidatedWater::new(
            Water {
                local_heights: &water,
                resolution: l.height_resolution,
                ocean_mask: 1,
                ocean_level: 0.,
                terrain_minimum: -500.,
            },
            &l,
        )
        .unwrap();
        let mut p = prefab();
        p.transforms = vec![Transform::Swim];
        let prepared = PreparedPrefab::new(p).unwrap();
        let mut budget = OrientedBudget::new(&l, &alpha)
            .unwrap()
            .with_sample_limit(4)
            .unwrap();
        budget.samples = 1;
        assert!(matches!(
            budget
                .try_fit_static(
                    l.view(),
                    StaticRequest {
                        candidate: &c,
                        stage: &s,
                        prefab: &prepared,
                        seed: 42,
                        water: Some(water)
                    }
                )
                .unwrap(),
            OrientedOutcome::Rejected(OrientedRejection::Budget)
        ));
        assert_eq!(budget.samples(), 4);
    }
    #[test]
    fn placement_map_is_checked_at_the_point_independently_of_topology_radius() {
        let l = layers();
        let n = l.surface_resolution;
        let alpha = vec![255; n * n];
        let mut blocked = vec![0; n * n];
        blocked[(n / 2) * n + n / 2] = 1;
        let view = crate::native_layers::View {
            placement_blockers: Some(&blocked),
            ..l.view()
        };
        let filter = spawn::Filter {
            splat_type: -1,
            biome_type: -1,
            topology_any: -1,
            topology_all: 0,
            topology_not: 0,
        };
        let mut surface = Surface {
            height_cache: None,
            topology_cache: None,
            maximum_samples: MAX_SAMPLES,
            layers: view,
            alpha: &alpha,
            samples: 0,
            heights: 0,
        };
        for radius in [0., 10.] {
            assert_eq!(surface.factor_uv(0.5, 0.5, &filter, radius, true), 0.);
            assert_eq!(
                surface.factor_uv(0.5, 0.5, &filter, radius, false),
                1.
            );
            let (u, v) = surface.uv([50., 0., 0.]).unwrap();
            assert_eq!(surface.factor_uv(u, v, &filter, radius, true), 1.);
        }
        assert_eq!(
            spawn::sample_factor_view(view, &filter, 0.5, 0.5).unwrap(),
            0.
        );
        assert_eq!(
            spawn::sample_factor_view(l.view(), &filter, 0.5, 0.5).unwrap(),
            1.
        );
        let malformed = crate::native_layers::View {
            placement_blockers: Some(&blocked[..1]),
            ..l.view()
        };
        assert!(spawn::sample_factor_view(malformed, &filter, 0.5, 0.5).is_err());
        assert!(malformed.placement_blocked(0.5, 0.5));
        assert!(
            OrientedBudget::new(&l, &alpha)
                .unwrap()
                .validate_view(malformed)
                .is_err()
        );
    }
    #[test]
    fn indexed_live_radius_filters_match_legacy_factors_and_ignore_foreign_summaries() {
        let mut l = layers();
        let n = l.surface_resolution;
        for (i, v) in l.topology.iter_mut().enumerate() {
            *v = if i % 97 == 0 {
                2
            } else if i % 523 == 0 {
                4
            } else {
                1
            };
        }
        let index = crate::topology_query::Index::new(&l.topology, n, &|| false)
            .unwrap()
            .unwrap();
        let alpha = vec![255; n * n];
        let mut plain = Surface {
            height_cache: None,
            topology_cache: None,
            maximum_samples: MAX_SAMPLES,
            layers: l.view(),
            alpha: &alpha,
            samples: 0,
            heights: 0,
        };
        let mut fast = Surface {
            height_cache: None,
            topology_cache: None,
            maximum_samples: MAX_SAMPLES,
            layers: crate::native_layers::View {
                topology_tiles: Some(index.view(&l.topology)),
                ..l.view()
            },
            alpha: &alpha,
            samples: 0,
            heights: 0,
        };
        for (any, all, not) in [(-1, 0, 8), (6, 0, 0), (-1, 2, 4), (1, 2, 0)] {
            let filter = spawn::Filter {
                splat_type: -1,
                biome_type: -1,
                topology_any: any,
                topology_all: all,
                topology_not: not,
            };
            for radius in [1.2, 20., 90.] {
                for x in [-499., -31., 0., 400., 499.] {
                    for z in [-499., 0., 499.] {
                        let (u, v) = plain.uv([x, 0., z]).unwrap();
                        assert_eq!(
                            fast.factor_uv(u, v, &filter, radius, true),
                            plain.factor_uv(u, v, &filter, radius, true)
                        );
                    }
                }
            }
        }
        assert!(fast.samples < plain.samples);
        let foreign = vec![8; l.topology.len()];
        let mut replaced = Surface {
            height_cache: None,
            topology_cache: None,
            maximum_samples: MAX_SAMPLES,
            layers: crate::native_layers::View {
                topology: &foreign,
                ..fast.layers
            },
            alpha: &alpha,
            samples: 0,
            heights: 0,
        };
        let filter = spawn::Filter {
            splat_type: -1,
            biome_type: -1,
            topology_any: -1,
            topology_all: 0,
            topology_not: 8,
        };
        assert_eq!(replaced.factor_uv(0.5, 0.5, &filter, 90., true), 0.);
        assert!(replaced.samples > 1000);
    }
    #[test]
    fn shared_static_fitting_matches_batch_basis_transforms_and_point_filters() {
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let s = stage();
        let c = candidate();
        let mut p = prefab();
        p.rotation = euler([0., 180., 0.]);
        p.scale = [2., 3., 4.];
        p.transforms.push(Transform::Rotate {
            min: [0., 45., 0.],
            max: [0., 45., 0.],
        });
        p.anchors.push(Anchor {
            position: [0.; 3],
            extents: 2.,
            offset: 0.,
            radius: 0.,
            slope_scale: 0.,
        });
        let prepared = PreparedPrefab::new(p.clone()).unwrap();
        let expected = generate_extended(
            &l,
            &alpha,
            std::slice::from_ref(&s),
            std::slice::from_ref(&c),
            &BTreeMap::from([(s.prefabs[0].clone(), p.clone())]),
            Settings {
                seed: 42,
                water: None,
            },
            || false,
        )
        .unwrap()
        .unwrap();
        let mut budget = OrientedBudget::new(&l, &alpha).unwrap();
        let OrientedOutcome::Fitted(actual) = budget
            .try_fit_static(
                l.view(),
                StaticRequest {
                    candidate: &c,
                    stage: &s,
                    prefab: &prepared,
                    seed: 42,
                    water: None,
                },
            )
            .unwrap()
        else {
            panic!("static fit rejected")
        };
        assert_eq!(actual.position, expected.poses[0].position);
        assert_eq!(actual.rotation, expected.poses[0].rotation);
        assert_eq!(actual.scale, expected.poses[0].scale);
        let before = budget.samples();
        let mut forbidden = s.filter.clone();
        forbidden.topology_not = 1;
        p.filters.push(PointFilter {
            check_placement: false,
            position: [0.; 3],
            filter: forbidden,
            topology_radius: 0.,
        });
        let prepared = PreparedPrefab::new(p).unwrap();
        assert!(matches!(
            budget
                .try_fit_static(
                    l.view(),
                    StaticRequest {
                        candidate: &c,
                        stage: &s,
                        prefab: &prepared,
                        seed: 42,
                        water: None
                    }
                )
                .unwrap(),
            OrientedOutcome::Rejected(OrientedRejection::Filter)
        ));
        assert!(matches!(
            budget
                .try_fit(
                    l.view(),
                    OrientedRequest {
                        position: c.position,
                        rotation: path_rotation([1., 0., 0.]).unwrap(),
                        filter: &s.filter,
                        prefab: &prepared,
                        seed: 42,
                        apply_decor: true,
                        check_anchor_filter: true,
                        anchor_mode: 0
                    }
                )
                .unwrap(),
            OrientedOutcome::Fitted(_)
        ));
        assert!(budget.samples() > before);
        budget.samples = MAX_SAMPLES;
        assert!(matches!(
            budget
                .try_fit_static(
                    l.view(),
                    StaticRequest {
                        candidate: &c,
                        stage: &s,
                        prefab: &prepared,
                        seed: 42,
                        water: None
                    }
                )
                .unwrap(),
            OrientedOutcome::Rejected(OrientedRejection::Budget)
        ));
    }
    #[test]
    fn global_ocean_matches_the_default_dry_raster() {
        let mut layers = layers();
        let dry = vec![0; layers.height.len()];
        let water = ValidatedWater::new(
            Water {
                local_heights: &dry,
                resolution: layers.height_resolution,
                ocean_mask: 128,
                ocean_level: 0.,
                terrain_minimum: -500.,
            },
            &layers,
        )
        .unwrap();
        let global = ValidatedWater::global_ocean(&layers, 128, || false)
            .unwrap()
            .unwrap();
        let resolution = layers.height_resolution;
        for (u, v) in [(0., 0.), (0.123, 0.456), (0.87, 0.43), (1., 1.)] {
            assert_eq!(
                global.water.height(resolution, u, v).to_bits(),
                water.water.height(resolution, u, v).to_bits(),
            );
        }
        assert!(ValidatedWater::global_ocean(&layers, 128, || true).unwrap().is_none());
        assert!(ValidatedWater::global_ocean(&layers, 0, || false).is_err());
        *layers.height.last_mut().unwrap() = -1;
        assert!(ValidatedWater::global_ocean(&layers, 128, || false).is_err());
    }

    #[test]
    fn live_static_water_is_validated_once_and_missing_or_wrong_context_cannot_fallback() {
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let s = stage();
        let c = candidate();
        let mut p = prefab();
        p.transforms.push(Transform::Swim);
        let prepared = PreparedPrefab::new(p.clone()).unwrap();
        let mut budget = OrientedBudget::new(&l, &alpha).unwrap();
        assert!(
            budget
                .try_fit_static(
                    l.view(),
                    StaticRequest {
                        candidate: &c,
                        stage: &s,
                        prefab: &prepared,
                        seed: 42,
                        water: None
                    }
                )
                .is_err()
        );
        let heights = vec![crate::erosion::encode(25.); l.height.len()];
        let water = Water {
            local_heights: &heights,
            resolution: l.height_resolution,
            ocean_mask: 384,
            ocean_level: 0.,
            terrain_minimum: -500.,
        };
        assert!(
            ValidatedWater::new(
                Water {
                    local_heights: &heights[..1],
                    ..water
                },
                &l
            )
            .is_err()
        );
        let validated = ValidatedWater::new(water, &l).unwrap();
        let OrientedOutcome::Fitted(actual) = budget
            .try_fit_static(
                l.view(),
                StaticRequest {
                    candidate: &c,
                    stage: &s,
                    prefab: &prepared,
                    seed: 42,
                    water: Some(validated),
                },
            )
            .unwrap()
        else {
            panic!("water fit rejected")
        };
        let expected = generate_extended(
            &l,
            &alpha,
            std::slice::from_ref(&s),
            std::slice::from_ref(&c),
            &BTreeMap::from([(s.prefabs[0].clone(), p)]),
            Settings {
                seed: 42,
                water: Some(water),
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(actual.position, expected.poses[0].position);
        let wrong = ValidatedWater {
            size: 2000,
            ..validated
        };
        assert!(
            budget
                .try_fit_static(
                    l.view(),
                    StaticRequest {
                        candidate: &c,
                        stage: &s,
                        prefab: &prepared,
                        seed: 42,
                        water: Some(wrong)
                    }
                )
                .is_err()
        );
    }
    #[test]
    fn extended_fitting_validates_water_cancels_and_repeats_without_mutation() {
        let l = layers();
        let original = l.height.clone();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let mut p = prefab();
        p.transforms = vec![Transform::Swim];
        let s = stage();
        let prefabs = BTreeMap::from([(s.prefabs[0].clone(), p)]);
        let water = vec![crate::erosion::encode(25.); l.height_resolution.pow(2)];
        let context = Water {
            local_heights: &water,
            resolution: l.height_resolution,
            ocean_mask: 384,
            ocean_level: 0.,
            terrain_minimum: -500.,
        };
        let run = |water, cancel| {
            generate_extended(
                &l,
                &alpha,
                std::slice::from_ref(&s),
                &[candidate()],
                &prefabs,
                Settings { seed: 42, water },
                || cancel,
            )
        };
        assert!(run(None, false).is_err());
        let surface_water = vec![crate::erosion::encode(25.); l.surface_resolution.pow(2)];
        assert!(
            run(
                Some(Water {
                    local_heights: &surface_water,
                    resolution: l.surface_resolution,
                    ..context
                }),
                false
            )
            .is_err()
        );
        assert!(
            run(
                Some(Water {
                    local_heights: &water[..1],
                    ..context
                }),
                false
            )
            .is_err()
        );
        assert!(run(Some(context), true).unwrap().is_none());
        let first = run(Some(context), false).unwrap().unwrap();
        let second = run(Some(context), false).unwrap().unwrap();
        assert_eq!(first.engine, EXTENDED_VERSION);
        assert_eq!(
            serde_json::to_value(&first).unwrap(),
            serde_json::to_value(&second).unwrap()
        );
        assert_eq!(first.counts.fitted, 1);
        assert!((first.poses[0].position[1] - 25.).abs() < 0.04);
        assert_eq!(l.height, original);
        assert!(water.iter().all(|h| *h == crate::erosion::encode(25.)));
    }
    #[test]
    fn inverted_anchor_offsets_and_extents_clamp_at_ninety_degrees_in_all_versions() {
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let mut p = prefab();
        p.rotation = [0., 0., 1., 0.];
        p.anchors.push(Anchor {
            position: [0.; 3],
            extents: 1.,
            offset: 2.,
            radius: 0.,
            slope_scale: 1.,
        });
        let mut s = stage();
        s.anchor_mode = 2;
        let height = crate::erosion::decode(l.height[0]);
        for extended in [false, true] {
            let options = Options {
                settings: Settings {
                    seed: 1,
                    water: None,
                },
                extended,
            };
            let mut surface = Surface {
                height_cache: None,
                topology_cache: None,
                maximum_samples: MAX_SAMPLES,
                layers: l.view(),
                alpha: &alpha,
                samples: 0,
                heights: 0,
            };
            let (pos, _, _) = fit(&mut surface, &candidate(), &s, &p, 1, &options).unwrap();
            assert!((pos[1] - (height - 2.)).abs() < 0.001);
        }
    }
    #[test]
    fn flips_rotate_about_authored_local_axes_without_negative_scale() {
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let options = Options {
            settings: Settings {
                seed: 1,
                water: None,
            },
            extended: true,
        };
        for axis in 0..3 {
            let mut branches = [false; 2];
            for seed in 0..32 {
                let mut p = prefab();
                p.rotation = euler([21., 37., 12.]);
                p.scale = [1., 2., 3.];
                p.transforms = vec![Transform::Flip { axis }];
                let mut surface = Surface {
                    height_cache: None,
                    topology_cache: None,
                    maximum_samples: MAX_SAMPLES,
                    layers: l.view(),
                    alpha: &alpha,
                    samples: 0,
                    heights: 0,
                };
                let (position, actual, scale) =
                    fit(&mut surface, &candidate(), &stage(), &p, seed, &options).unwrap();
                let mut branch_seed = seed;
                let flipped = rand(&mut branch_seed) <= 0.5;
                branches[usize::from(flipped)] = true;
                let up_dot = dot(
                    rotate(actual, [0., 1., 0.]),
                    rotate(p.rotation, [0., 1., 0.]),
                );
                let forward_dot = dot(
                    rotate(actual, [0., 0., 1.]),
                    rotate(p.rotation, [0., 0., 1.]),
                );
                let sign = if flipped { -1. } else { 1. };
                // A local half turn preserves its axis and reverses the orthogonal one.
                assert!((up_dot - if axis == 1 { sign } else { 1. }).abs() < 1e-5);
                assert!((forward_dot - if axis == 1 { 1. } else { sign }).abs() < 1e-5);
                assert_eq!(position, candidate().position);
                assert_eq!(scale, [1., 2., 3.]);
            }
            assert_eq!(branches, [true, true]);
        }
    }
    #[test]
    fn swim_samples_water_bilinearly_and_respects_source_ocean_roles_and_dry_reset() {
        let points = [0., 10., 20., 30.].map(crate::erosion::encode);
        let small = Water {
            local_heights: &points,
            resolution: 2,
            ocean_mask: 384,
            ocean_level: 0.,
            terrain_minimum: -500.,
        };
        assert!((small.height(2, 0.5, 0.5) - 15.).abs() < 0.04);
        let mut l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let mut p = prefab();
        p.transforms = vec![Transform::Swim];
        let s = stage();
        let c = candidate();
        for (local_level, topology, expected) in [
            (25., 0, 25.),
            (-10., 128, 0.),
            (-10., 256, 0.),
            (-10., 0, -10.),
            (-500., 0, 0.),
        ] {
            l.topology.fill(topology);
            let water = vec![crate::erosion::encode(local_level); l.height_resolution.pow(2)];
            let options = Options {
                settings: Settings {
                    seed: 1,
                    water: Some(Water {
                        local_heights: &water,
                        resolution: l.height_resolution,
                        ..small
                    }),
                },
                extended: true,
            };
            let mut surface = Surface {
                height_cache: None,
                topology_cache: None,
                maximum_samples: MAX_SAMPLES,
                layers: l.view(),
                alpha: &alpha,
                samples: 0,
                heights: 0,
            };
            let (pos, _, _) = fit(&mut surface, &c, &s, &p, 1, &options).unwrap();
            assert!(
                (pos[1] - expected).abs() < 0.04,
                "level {local_level}, topology {topology}"
            );
            assert_eq!(surface.samples, 4);
        }
    }
    #[test]
    fn shared_normal_axes_preserve_border_samples_and_budget_order() {
        let mut l = layers();
        let n = l.height_resolution;
        for (i, height) in l.height.iter_mut().enumerate() {
            *height = 16_000 + ((i * 97 + i / n * 13) % 1301) as i16;
        }
        let alpha = vec![255; l.surface_resolution.pow(2)];
        for position in [
            [0., 0., -0.],
            [31.25, 48., -20.75],
            [-500., -0., 500.],
            [-500.1, 0., 499.],
            [499.9, 0., -500.1],
            [0., 10_001., 0.],
            [f32::NAN, 0., 0.],
        ] {
            for maximum_samples in 0..=5 {
                let surface = || Surface {
                    height_cache: None,
                    topology_cache: None,
                    maximum_samples,
                    layers: l.view(),
                    alpha: &alpha,
                    samples: 0,
                    heights: 0,
                };
                let mut scalar = surface();
                let reference = (|| {
                    let d = l.size as f32 / (n - 1) as f32;
                    let mut height = |p| {
                        scalar.charge_height()?;
                        let (u, v) = scalar.uv(p)?;
                        Some(spawn::height_view(scalar.layers, u, v))
                    };
                    let mut gradient = [0.; 3];
                    for axis in [0, 2] {
                        let (mut a, mut b) = (position, position);
                        a[axis] = (a[axis] - d).max(-500.);
                        b[axis] = (b[axis] + d).min(500.);
                        gradient[axis] = -(height(b)? - height(a)?) / (b[axis] - a[axis]);
                    }
                    gradient[1] = 1.;
                    norm(gradient)
                })();
                let mut shared = surface();
                assert_eq!(
                    shared.normal(position).map(|v| v.map(f32::to_bits)),
                    reference.map(|v| v.map(f32::to_bits)),
                    "{position:?}, budget {maximum_samples}"
                );
                assert_eq!((shared.samples, shared.heights), (scalar.samples, scalar.heights));
            }
        }
    }

    #[test]
    fn slope_alignment_uses_normal_tangent_offsets_and_inverse_rotation_scale() {
        let mut l = layers();
        let n = l.height_resolution;
        for z in 0..n {
            for x in 0..n {
                let world_x = (x as f32 / (n - 1) as f32 - 0.5) * l.size as f32;
                l.height[z * n + x] = crate::erosion::encode(50. + world_x * 0.1);
            }
        }
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let mut surface = Surface {
            height_cache: None,
            topology_cache: None,
            maximum_samples: MAX_SAMPLES,
            layers: l.view(),
            alpha: &alpha,
            samples: 0,
            heights: 0,
        };
        let normal = surface.normal(candidate().position).unwrap();
        let tangent = cross(normal, [0., 1., 0.]);
        let down = cross(normal, tangent);
        let slope = normal[1].acos() / std::f32::consts::FRAC_PI_2;
        let mut p = prefab();
        p.rotation = euler([0., 37., 0.]);
        p.transforms = vec![Transform::AlignSlope {
            normal: 0.,
            gradient: 0.,
            offset: [2., 3., 4.],
            scale: [2., 1., 1.],
        }];
        let options = Options {
            settings: Settings {
                seed: 1,
                water: None,
            },
            extended: true,
        };
        let (pos, rotation, actual_scale) =
            fit(&mut surface, &candidate(), &stage(), &p, 1, &options).unwrap();
        let expected = add(
            candidate().position,
            add(
                add(scale(tangent, 2. * slope), scale(normal, 3. * slope)),
                scale(down, -4. * slope),
            ),
        );
        for i in 0..3 {
            assert!((pos[i] - expected[i]).abs() < 1e-5);
        }
        assert!(
            dot(
                rotate(rotation, [0., 0., 1.]),
                rotate(p.rotation, [0., 0., 1.])
            ) > 0.99999
        );
        let yaw = 37_f32.to_radians();
        assert!((actual_scale[0] - (1. + yaw.cos() * slope)).abs() < 1e-5);
        assert!((actual_scale[2] - (1. + yaw.sin() * slope)).abs() < 1e-5);
        let mut bad = p;
        bad.transforms = vec![Transform::Flip { axis: 3 }];
        assert!(bad.validate().is_err());
    }
    #[test]
    fn live_fitting_observes_destination_height_and_filters_without_resetting_work() {
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let mut p = prefab();
        p.anchors.push(Anchor {
            position: [0.; 3],
            extents: 2.,
            offset: 0.,
            radius: 0.,
            slope_scale: 0.,
        });
        let prepared = PreparedPrefab::new(p).unwrap();
        let filter = stage().filter;
        let request = || OrientedRequest {
            position: [0., 50., 0.],
            rotation: [0., 0., 0., 1.],
            filter: &filter,
            prefab: &prepared,
            seed: 1,
            apply_decor: true,
            check_anchor_filter: true,
            anchor_mode: 0,
        };
        let mut budget = OrientedBudget::new(&l, &alpha).unwrap();
        let OrientedOutcome::Fitted(first) = budget.try_fit(l.view(), request()).unwrap() else {
            panic!("initial fit")
        };
        let used = budget.samples();
        let heights = vec![crate::erosion::encode(60.); l.height.len()];
        let view = crate::native_layers::View {
            height: &heights,
            ..l.view()
        };
        let OrientedOutcome::Fitted(second) = budget.try_fit(view, request()).unwrap() else {
            panic!("live fit")
        };
        assert!((first.position[1] - crate::erosion::decode(18000)).abs() < 0.0001);
        assert!((second.position[1] - 60.).abs() < 0.04);
        assert!(budget.samples() > used && budget.height_samples() > 0);
        let mut blocked = filter.clone();
        blocked.topology_not = 2;
        let topo = vec![2; l.topology.len()];
        let changed = crate::native_layers::View {
            topology: &topo,
            ..view
        };
        assert!(matches!(
            budget
                .try_fit(
                    changed,
                    OrientedRequest {
                        filter: &blocked,
                        ..request()
                    }
                )
                .unwrap(),
            OrientedOutcome::Rejected(OrientedRejection::Filter)
        ));
        let before = budget.samples();
        let malformed = crate::native_layers::View {
            height: &heights[..1],
            ..view
        };
        assert!(budget.try_fit(malformed, request()).is_err());
        assert_eq!(budget.samples(), before);
        budget.samples = MAX_SAMPLES;
        assert!(matches!(
            budget.try_fit(view, request()).unwrap(),
            OrientedOutcome::Rejected(OrientedRejection::Budget)
        ));
        assert!(l.height.iter().all(|v| *v == 18000));
    }
    #[test]
    fn oriented_fitter_keeps_path_basis_and_skips_decor_during_anchor_checks() {
        let l = layers();
        let before = l.height.clone();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let mut p = prefab();
        p.rotation = euler([0., 180., 0.]);
        p.scale = [2., 3., 4.];
        p.transforms.push(Transform::Rotate {
            min: [0., 45., 0.],
            max: [0., 45., 0.],
        });
        p.anchors.push(Anchor {
            position: [0.; 3],
            extents: 2.,
            offset: 0.,
            radius: 0.,
            slope_scale: 0.,
        });
        let mut forbidden = stage().filter;
        forbidden.topology_not = 1;
        p.filters.push(PointFilter {
            check_placement: false,
            position: [0.; 3],
            filter: forbidden,
            topology_radius: 0.,
        });
        let prepared = PreparedPrefab::new(p.clone()).unwrap();
        let filter = stage().filter;
        let mut fitter = OrientedFitter::new(&l, &alpha).unwrap();
        let q = path_rotation([1., 0., 0.]).unwrap();
        for (apply_decor, yaw) in [(false, 90_f32), (true, 135.)] {
            let out = fitter
                .try_fit(OrientedRequest {
                    position: [0., 50., 0.],
                    rotation: q,
                    filter: &filter,
                    prefab: &prepared,
                    seed: 123,
                    apply_decor,
                    check_anchor_filter: true,
                    anchor_mode: 0,
                })
                .unwrap();
            let OrientedOutcome::Fitted(out) = out else {
                panic!("valid source road fit rejected");
            };
            let forward = rotate(out.rotation, [0., 0., 1.]);
            assert!((forward[0] - yaw.to_radians().sin()).abs() < 1e-5);
            assert!((forward[2] - yaw.to_radians().cos()).abs() < 1e-5);
            assert_eq!(out.scale, [2., 3., 4.]);
            assert!((out.position[1] - crate::erosion::decode(18000)).abs() < 1e-4);
        }
        assert!(fitter.samples() > 0);
        assert_eq!(l.height, before);
        assert_eq!(
            run(&l, p, vec![candidate()], stage())
                .counts
                .rejected_filter,
            1
        );
        fitter.surface.samples = MAX_SAMPLES;
        assert!(matches!(
            fitter
                .try_fit(OrientedRequest {
                    position: [0., 50., 0.],
                    rotation: q,
                    filter: &filter,
                    prefab: &prepared,
                    seed: 1,
                    apply_decor: true,
                    check_anchor_filter: true,
                    anchor_mode: 0
                })
                .unwrap(),
            OrientedOutcome::Rejected(OrientedRejection::Budget)
        ));
    }
    #[test]
    fn offsets_use_scaled_world_axes_and_preserve_transform_order() {
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let s = stage();
        let c = candidate();
        for yaw in [0., 37., 90.] {
            let mut p = prefab();
            p.rotation = euler([0., yaw, 0.]);
            p.transforms = vec![
                Transform::Scale {
                    min: [2., 3., 4.],
                    max: [2., 3., 4.],
                },
                Transform::Offset {
                    min: [1., 2., 3.],
                    max: [1., 2., 3.],
                },
            ];
            for reverse in [false, true] {
                let mut source = p.clone();
                if reverse {
                    source.transforms.reverse();
                }
                let prepared = PreparedPrefab::new(source).unwrap();
                let mut fitting = OrientedBudget::new(&l, &alpha).unwrap();
                let OrientedOutcome::Fitted(pose) = fitting
                    .try_fit_static(
                        l.view(),
                        StaticRequest {
                            candidate: &c,
                            stage: &s,
                            prefab: &prepared,
                            seed: 42,
                            water: None,
                        },
                    )
                    .unwrap()
                else {
                    panic!("Offset pose was rejected")
                };
                assert_eq!(
                    pose.position,
                    if reverse {
                        [1., 52., 3.]
                    } else {
                        [2., 56., 12.]
                    }
                );
                assert_eq!(pose.scale, [2., 3., 4.]);
                assert_eq!(pose.rotation, p.rotation);
            }
        }
    }

    #[test]
    fn reversed_offset_ranges_are_valid_and_repeatable_in_the_fitting_stream() {
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let s = stage();
        let c = candidate();
        let mut p = prefab();
        p.transforms = vec![Transform::Offset {
            min: [1., -0.4, 3.],
            max: [-1., -0.7, -3.],
        }];
        let prepared = PreparedPrefab::new(p).unwrap();
        let run = |seed| {
            let mut fitting = OrientedBudget::new(&l, &alpha).unwrap();
            let OrientedOutcome::Fitted(pose) = fitting
                .try_fit_static(
                    l.view(),
                    StaticRequest {
                        candidate: &c,
                        stage: &s,
                        prefab: &prepared,
                        seed,
                        water: None,
                    },
                )
                .unwrap()
            else {
                panic!("Reversed offset pose was rejected")
            };
            pose.position
        };
        assert_eq!(run(42), run(42));
        assert_ne!(run(42), run(43));
        for seed in 0..64 {
            let p = run(seed);
            assert!((-1. ..=1.).contains(&p[0]));
            assert!((49.3..=49.60001).contains(&p[1]));
            assert!((-3. ..=3.).contains(&p[2]));
        }
    }

    #[test]
    fn offset_surface_and_point_filters_use_the_shifted_location() {
        let mut l = layers();
        let n = l.surface_resolution;
        let mut alpha = vec![0; n * n];
        alpha[idx(n, 0.5) * n + idx(n, 0.5)] = 255;
        let s = stage();
        let c = candidate();
        let mut p = prefab();
        p.transforms = vec![Transform::Offset {
            min: [100., 0., 0.],
            max: [100., 0., 0.],
        }];
        let prepared = PreparedPrefab::new(p.clone()).unwrap();
        let mut fitting = OrientedBudget::new(&l, &alpha).unwrap();
        assert!(matches!(
            fitting
                .try_fit_static(
                    l.view(),
                    StaticRequest {
                        candidate: &c,
                        stage: &s,
                        prefab: &prepared,
                        seed: 42,
                        water: None,
                    }
                )
                .unwrap(),
            OrientedOutcome::Rejected(OrientedRejection::Surface)
        ));
        let report = generate(
            &l,
            &alpha,
            std::slice::from_ref(&s),
            std::slice::from_ref(&c),
            &BTreeMap::from([(s.prefabs[0].clone(), p.clone())]),
            42,
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(report.poses.is_empty());
        assert_eq!(report.counts.rejected_bounds_or_holes, 1);

        alpha.fill(255);
        l.topology[idx(n, 0.5) * n + idx(n, 0.6)] = 2;
        let mut filter = s.filter.clone();
        filter.topology_any = 1;
        p.filters.push(PointFilter {
            check_placement: false,
            position: [0.; 3],
            filter,
            topology_radius: 0.,
        });
        let prepared = PreparedPrefab::new(p).unwrap();
        let mut fitting = OrientedBudget::new(&l, &alpha).unwrap();
        assert!(matches!(
            fitting
                .try_fit_static(
                    l.view(),
                    StaticRequest {
                        candidate: &c,
                        stage: &s,
                        prefab: &prepared,
                        seed: 42,
                        water: None,
                    }
                )
                .unwrap(),
            OrientedOutcome::Rejected(OrientedRejection::Filter)
        ));
    }

    #[test]
    fn invalid_offsets_fail_during_preparation() {
        for value in [f32::NAN, f32::INFINITY, 10001.] {
            let mut p = prefab();
            p.transforms = vec![Transform::Offset {
                min: [value, 0., 0.],
                max: [0.; 3],
            }];
            assert!(PreparedPrefab::new(p).is_err());
        }
    }
    #[test]
    fn path_static_fitting_validates_its_scope_and_runs_full_prefab_filters() {
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let mut source = stage();
        source.folder = "monument/maintainables".into();
        source.prefabs =
            vec!["assets/bundled/prefabs/autospawn/monument/maintainables/pipe.prefab".into()];
        let c = candidate();
        let mut p = prefab();
        p.transforms = vec![Transform::Offset {
            min: [1., 0., 0.],
            max: [1., 0., 0.],
        }];
        let mut filter = source.filter.clone();
        filter.topology_any = 2;
        p.filters.push(PointFilter {
            check_placement: false,
            position: [0.; 3],
            filter,
            topology_radius: 0.,
        });
        let prepared = PreparedPrefab::new(p.clone()).unwrap();
        let request = || StaticRequest {
            candidate: &c,
            stage: &source,
            prefab: &prepared,
            seed: 42,
            water: None,
        };
        let mut fitting = OrientedBudget::new(&l, &alpha).unwrap();
        assert!(fitting.try_fit_static(l.view(), request()).is_err());
        assert!(matches!(
            fitting.try_fit_path_static(l.view(), request()).unwrap(),
            OrientedOutcome::Rejected(OrientedRejection::Filter)
        ));
        let consumed = fitting.samples();
        assert!(consumed > 0);
        p.filters.clear();
        let prepared = PreparedPrefab::new(p).unwrap();
        let OrientedOutcome::Fitted(pose) = fitting
            .try_fit_path_static(
                l.view(),
                StaticRequest {
                    candidate: &c,
                    stage: &source,
                    prefab: &prepared,
                    seed: 42,
                    water: None,
                },
            )
            .unwrap()
        else {
            panic!("Path-owned prefab did not fit")
        };
        assert_eq!(pose.position, [1., 50., 0.]);
        assert!(fitting.samples() >= consumed);
    }
    fn layers() -> Layers {
        let (hr, n) = dimensions(1000).unwrap();
        let cells = n * n;
        Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: n,
            height: vec![18000; hr * hr],
            biome: vec![51; cells * 5],
            splat: vec![32; cells * 8],
            topology: vec![1; cells],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        }
    }
    fn stage() -> decor::Stage {
        serde_json::from_value(serde_json::json!({"id":"one","order":0,"name":"Rocks","kind":"white_noise","folder":"decor/rocks","enabled":true,"mode":-1,"filter":{"SplatType":-1,"BiomeType":-1,"TopologyAny":-1,"TopologyAll":0,"TopologyNot":0},"filter_mode":1,"anchor_mode":0,"distance":0.,"dithering":0.,"density":10.,"cluster":null,"prefabs":["assets/bundled/prefabs/autospawn/decor/rocks/a.prefab"]})).unwrap()
    }
    fn prefab() -> Prefab {
        Prefab {
            rotation: [0., 0., 0., 1.],
            scale: [1., 1., 1.],
            transforms: vec![],
            anchors: vec![],
            filters: vec![],
            unsupported: vec![],
            pending: vec!["Physics".into()],
        }
    }
    fn run(l: &Layers, p: Prefab, c: Vec<decor::Candidate>, stage: decor::Stage) -> Report {
        let prefabs = BTreeMap::from([(stage.prefabs[0].clone(), p)]);
        generate(
            l,
            &vec![255; l.surface_resolution.pow(2)],
            &[stage],
            &c,
            &prefabs,
            123,
            || false,
        )
        .unwrap()
        .unwrap()
    }
    fn candidate() -> decor::Candidate {
        decor::Candidate {
            stage: 0,
            prefab: 0,
            position: [0., 50., 0.],
        }
    }
    #[test]
    fn quaternion_euler_and_forced_up_are_orthonormal_at_arbitrary_angles() {
        let q = euler([0., 90., 0.]);
        let v = rotate(q, [0., 0., 1.]);
        assert!((v[0] - 1.).abs() < 1e-5);
        assert!(v[2].abs() < 1e-5);
        let up = norm([0.3, 1., -0.2]).unwrap();
        let q = look([1., 0., 1.], up).unwrap();
        let actual = rotate(q, [0., 1., 0.]);
        assert!(dot(actual, up) > 0.99999);
        assert!(dot(rotate(q, [0., 0., 1.]), actual).abs() < 1e-5);
        assert!(look(up, up).is_some());
    }
    #[test]
    fn authored_ranges_and_shared_scale_factor_are_deterministic() {
        let l = layers();
        let mut p = prefab();
        p.transforms = vec![
            Transform::Rotate {
                min: [0., 37., 0.],
                max: [0., 37., 0.],
            },
            Transform::Scale {
                min: [0.5, 1., 1.5],
                max: [1., 2., 3.],
            },
        ];
        let r = run(&l, p.clone(), vec![candidate()], stage());
        let again = run(&l, p, vec![candidate()], stage());
        assert_eq!(
            serde_json::to_value(&r).unwrap(),
            serde_json::to_value(again).unwrap()
        );
        let pose = &r.poses[0];
        assert!((pose.scale[1] - pose.scale[0] * 2.).abs() < 1e-6);
        assert!((pose.scale[2] - pose.scale[0] * 3.).abs() < 1e-6);
        assert!((rotate(pose.rotation, [0., 0., 1.])[0] - 37_f32.to_radians().sin()).abs() < 1e-5);
        assert!(!r.physics_validated);
    }
    #[test]
    fn anchor_interval_modes_fit_height_and_reject_incompatible_support() {
        let mut l = layers();
        let mut p = prefab();
        p.anchors = vec![Anchor {
            position: [0., 2., 0.],
            extents: 1.,
            offset: 0.,
            radius: 0.,
            slope_scale: 0.,
        }];
        let h = crate::erosion::decode(18000);
        let r = run(&l, p.clone(), vec![candidate()], stage());
        assert!((r.poses[0].position[1] - (h - 2.)).abs() < 1e-4);
        let mut s = stage();
        s.anchor_mode = 2;
        let r = run(&l, p.clone(), vec![candidate()], s);
        assert!((r.poses[0].position[1] - (h - 1.)).abs() < 1e-4);
        p.anchors.push(Anchor {
            position: [100., 2., 0.],
            extents: 1.,
            offset: 0.,
            radius: 0.,
            slope_scale: 0.,
        });
        let hr = l.height_resolution;
        for z in 0..hr {
            for x in hr / 2 + 40..hr {
                l.height[z * hr + x] = 20000;
            }
        }
        let r = run(&l, p, vec![candidate()], stage());
        assert_eq!(r.counts.rejected_anchors, 1);
        assert!(r.poses.is_empty());
    }
    #[test]
    fn radius_anchor_checks_all_authored_support_samples() {
        let mut l = layers();
        let hr = l.height_resolution;
        let mid = hr / 2;
        l.height[mid * hr + mid + 1] = 25000;
        let mut p = prefab();
        p.anchors = vec![Anchor {
            position: [0.; 3],
            extents: 0.1,
            offset: 0.,
            radius: 2.,
            slope_scale: 0.,
        }];
        let r = run(&l, p, vec![candidate()], stage());
        assert_eq!(r.counts.rejected_anchors, 1);
        assert!(r.height_samples > 1);
    }
    #[test]
    fn alignment_follows_sloped_terrain_and_retains_unit_quaternion() {
        let mut l = layers();
        let hr = l.height_resolution;
        for z in 0..hr {
            for x in 0..hr {
                let wx = (x as f32 / (hr - 1) as f32 - 0.5) * 1000.;
                let h = 100. + wx * 0.2;
                l.height[z * hr + x] = ((h + 500.) / 1000. * 32766.).round() as i16;
            }
        }
        let mut p = prefab();
        p.transforms = vec![Transform::Align {
            normal: 1.,
            gradient: 0.,
        }];
        let r = run(&l, p, vec![candidate()], stage());
        let up = rotate(r.poses[0].rotation, [0., 1., 0.]);
        assert!(dot(up, norm([-0.2, 1., 0.]).unwrap()) > 0.999);
    }
    #[test]
    fn child_filter_radius_sees_neighboring_forbidden_topology() {
        let mut l = layers();
        let n = l.surface_resolution;
        let i = n / 2 * n + n / 2;
        l.topology[i + 1] = 2;
        let mut p = prefab();
        p.filters = vec![PointFilter {
            check_placement: false,
            position: [0.; 3],
            filter: spawn::Filter {
                splat_type: -1,
                biome_type: -1,
                topology_any: -1,
                topology_all: 0,
                topology_not: 2,
            },
            topology_radius: 3.5,
        }];
        let r = run(&l, p.clone(), vec![candidate()], stage());
        assert_eq!(r.counts.rejected_filter, 1);
        p.filters[0].topology_radius = 100.;
        let r = run(&l, p, vec![candidate()], stage());
        assert_eq!(r.counts.rejected_filter, 1);
    }
    #[test]
    fn unsupported_and_hole_cases_are_explicit_and_inputs_stay_unchanged() {
        let l = layers();
        let before = l.height.clone();
        let mut p = prefab();
        p.unsupported.push("DecorSwim".into());
        let r = run(&l, p, vec![candidate()], stage());
        assert_eq!(r.counts.unsupported, 1);
        assert!(r.poses.is_empty());
        assert_eq!(l.height, before);
        let s = stage();
        let specs = BTreeMap::from([(s.prefabs[0].clone(), prefab())]);
        let r = generate(
            &l,
            &vec![0; l.surface_resolution.pow(2)],
            &[s],
            &[candidate()],
            &specs,
            1,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(r.counts.rejected_bounds_or_holes, 1);
    }
    #[test]
    fn cancelled_or_malformed_fitting_never_publishes_partial_poses() {
        let l = layers();
        let s = stage();
        let specs = BTreeMap::from([(s.prefabs[0].clone(), prefab())]);
        let alpha = vec![255; l.surface_resolution.pow(2)];
        assert!(
            generate(
                &l,
                &alpha,
                std::slice::from_ref(&s),
                &[candidate()],
                &specs,
                1,
                || true
            )
            .unwrap()
            .is_none()
        );
        let mut c = candidate();
        c.stage = 999;
        assert!(
            generate(
                &l,
                &alpha,
                std::slice::from_ref(&s),
                &[c],
                &specs,
                1,
                || false
            )
            .is_err()
        );
        let mut bad = prefab();
        bad.anchors.push(Anchor {
            position: [0.; 3],
            extents: 1.,
            offset: 0.,
            radius: 1000.,
            slope_scale: 0.,
        });
        assert!(bad.validate().is_err());
    }
}
