//! Versioned Papyrus fill-only footprint policy. It uses authored polygon data;
//! it does not claim parity with the unavailable client TerrainFootprint method.
use crate::{
    decor, decor_fit,
    erosion::{decode, encode},
    native_layers::{Layers, dimensions},
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
pub const VERSION: &str = "papyrus-decor-footprints-3";
pub const BLOCKER_VERSION: &str = "papyrus-decor-blockers-4";
pub const ROAD_OBJECT_VERSION: &str = "papyrus-road-object-effects-2";
pub const OBJECT_VERSION: &str = "papyrus-object-terrain-1";
const MAX_VISITS: usize = 128_000_000;
/// Explicit limits for a complete live object pass. These bound CPU work;
/// individual geometry and private-plan allocation limits remain independent.
#[derive(Clone, Copy, Debug)]
pub struct ObjectPolicy {
    pub maximum_work: usize,
    pub role_exceptions: u32,
    pub material_role_exceptions: u32,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProtectionScope {
    pub namespace: String,
    pub protected_topology: u32,
    pub role_exceptions: u32,
    pub material_role_exceptions: u32,
    pub maximum_height_change: Option<f32>,
}
impl ObjectPolicy {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=1_024_000_000).contains(&self.maximum_work) {
            return Err("Object terrain work allowance must be between 1 and 1024000000".into());
        }
        if self.material_role_exceptions & !self.role_exceptions != 0 {
            return Err("Material placement roles must be permitted object roles".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub struct Footprint {
    pub ring: Vec<[f32; 3]>,
    pub center: [f32; 3],
    pub local_to_root: [[f32; 4]; 4],
    pub fill_offset: f32,
    pub max_fill: f32,
    pub reject_above_gap: f32,
    pub rim_width: f32,
    pub feather: f32,
    pub opacity: f32,
    pub fill_interior: bool,
    pub max_tilt_degrees: f32,
}
#[derive(Clone, Debug)]
pub struct Support {
    pub position: [f32; 3],
    pub radius: f32,
}
#[derive(Clone, Copy)]
pub(crate) struct OriginCheck {
    exception: u32,
    coverage: f32,
}
#[derive(Clone, Copy)]
pub(crate) struct OriginProbe {
    blocked: bool,
    neighbor_distance: f32,
}
impl OriginCheck {
    pub(crate) fn same_policy(self, other: Self) -> bool {
        self.exception == other.exception
    }
    pub(crate) fn scans_neighbors(self) -> bool {
        self.coverage > 0.
    }
    pub(crate) fn conflicts(self, probe: OriginProbe) -> bool {
        probe.blocked || self.coverage > 0. && probe.neighbor_distance <= self.coverage
    }
}
#[derive(Clone, Copy)]
pub(crate) struct CliffScaleBounds {
    pub smallest_axis: f32,
    pub largest_axis: f32,
}
#[derive(Clone, Debug)]
pub struct Blocker {
    pub position: [f32; 3],
    pub radius: f32,
    pub fade: f32,
}
#[derive(Clone, Debug)]
pub struct Raise {
    pub position: [f32; 3],
    pub radius: f32,
    pub fade: f32,
    pub opacity: f32,
}
#[derive(Clone, Debug)]
pub struct Prefab {
    pub support: Vec<Support>,
    pub blockers: Vec<Blocker>,
    pub raises: Vec<Raise>,
    pub topology: Option<crate::decor_topology::Prefab>,
    pub materials: Option<Vec<crate::decor_material::Modifier>>,
    pub height_modifiers: Option<Vec<crate::decor_height::Modifier>>,
    pub contributors: Option<Vec<crate::decor_collider::Contributor>>,
    pub footprint: Option<Footprint>,
    pub unsupported: Vec<String>,
    pub pending: Vec<String>,
}
pub struct Obstacle {
    pub center: [f32; 2],
    pub extent: [f32; 2],
    pub yaw: f32,
}
#[derive(Default, Debug, Serialize)]
pub struct Counts {
    pub attempted: usize,
    pub accepted: usize,
    pub unsupported: usize,
    pub occupied: usize,
    pub protected: usize,
    pub holes: usize,
    pub outside: usize,
    pub tilt: usize,
    pub fill_limit: usize,
    pub budget_limited: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub topology_filter: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub material_filter: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub placement_blocked: usize,
}
fn is_zero(v: &usize) -> bool {
    *v == 0
}
#[derive(Debug, Serialize)]
pub struct Applied {
    pub pose: usize,
    pub height_writes: usize,
    pub reserved_cells: usize,
    pub maximum_fill: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_cells: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protection_exception: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protection_scope: Option<usize>,
}
#[derive(Debug, Serialize)]
pub struct BlockingReport {
    pub instances: usize,
    pub cells: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_cells: Option<usize>,
    #[serde(skip)]
    pub raster: Vec<u8>,
}
#[derive(Debug, Serialize)]
pub struct PrefabReport {
    pub counts: Counts,
    #[serde(skip_serializing_if = "is_zero")]
    pub sample_visits: usize,
    pub unsupported: Vec<String>,
    pub pending: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub resolution: usize,
    pub protected_topology: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permitted_role_exceptions: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permitted_material_role_exceptions: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum_work: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protection_scopes: Option<Vec<ProtectionScope>>,
    pub counts: Counts,
    pub prefabs: BTreeMap<String, PrefabReport>,
    pub applied: Vec<Applied>,
    pub height_writes: usize,
    pub reserved_cells: usize,
    pub initial_protected_cells: usize,
    pub sample_visits: usize,
    pub unprocessed_poses: usize,
    pub budget_exhausted: bool,
    pub physics_validated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocking: Option<BlockingReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topology: Option<crate::decor_topology::Report>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub materials: Option<crate::decor_material::Report>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height_effects: Option<crate::decor_height::Report>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collider_effects: Option<crate::decor_collider::Report>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height_reserved_samples: Option<usize>,
    #[serde(skip)]
    pub occupancy: Vec<u8>,
}
pub struct Output {
    pub height: Vec<i16>,
    pub topology: Option<Vec<i32>>,
    pub splat: Option<Vec<u8>>,
    pub report: Report,
}
fn finite(v: &[f32]) -> bool {
    v.iter().all(|v| v.is_finite() && v.abs() <= 10000.)
}
fn point(m: &[[f32; 4]; 4], v: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2] + m[i][3])
}
impl CliffScaleBounds {
    fn origin_coverage(self, prefab: &Prefab, cell_radius: f32, blocking: bool) -> f32 {
        let minimum = f64::from(self.smallest_axis);
        let maximum = f64::from(self.largest_axis);
        let length = |[x, y, z]: [f32; 3]| f64::from(x).hypot(f64::from(y)).hypot(f64::from(z));
        // Cover the quaternion validation tolerance and world/raster rounding.
        let distance = |length: f64| length * maximum * 1.01 + 0.02;
        let mut coverage = 0_f64;
        if blocking {
            for blocker in &prefab.blockers {
                let radius = (f64::from(blocker.radius) - f64::from(blocker.fade) * 0.5) * minimum;
                coverage = coverage.max(radius - distance(length(blocker.position)));
            }
        }
        if let Some(footprint) = &prefab.footprint
            && footprint.local_to_root == [
                [1., 0., 0., 0.],
                [0., 1., 0., 0.],
                [0., 0., 1., 0.],
                [0., 0., 0., 1.],
            ]
        {
            // Distance to any point on an authored edge bounds the polygon
            // query. Nonuniform scale and rotation are covered by the bounds.
            let nearest = footprint.ring.iter()
                .zip(footprint.ring.iter().cycle().skip(1))
                .map(|(&a, &b)| {
                    let a = a.map(f64::from);
                    let delta: [f64; 3] = std::array::from_fn(|i| f64::from(b[i]) - a[i]);
                    let length_squared = delta.iter().map(|v| v * v).sum::<f64>();
                    let t = (-a.iter().zip(delta).map(|(v, d)| v * d).sum::<f64>()
                        / length_squared).clamp(0., 1.);
                    let [x, y, z] = std::array::from_fn(|i| a[i] + t * delta[i]);
                    x.hypot(y).hypot(z)
                })
                .fold(f64::INFINITY, f64::min);
            let reach = (f64::from(footprint.rim_width) + f64::from(footprint.feather))
                * (minimum * 0.99 - 0.01).max(0.);
            coverage = coverage.max(reach + f64::from(cell_radius) - distance(nearest));
        }
        if coverage > f64::from(cell_radius) { coverage as f32 } else { 0. }
    }
}
fn world(f: &Footprint, p: &decor_fit::Pose, v: [f32; 3]) -> [f32; 3] {
    let v = point(&f.local_to_root, v);
    let v = std::array::from_fn(|i| v[i] * p.scale[i]);
    let v = crate::decor_fit::rotate(p.rotation, v);
    std::array::from_fn(|i| v[i] + p.position[i])
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a.iter().zip(b).map(|(a, b)| a * b).sum()
}
fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}
fn orient(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> f32 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}
fn simple(r: &[[f32; 3]]) -> bool {
    for i in 0..r.len() {
        let a = [r[i][0], r[i][2]];
        let b = [r[(i + 1) % r.len()][0], r[(i + 1) % r.len()][2]];
        if (a[0] - b[0]).hypot(a[1] - b[1]) < 1e-4 {
            return false;
        }
        for j in i + 1..r.len() {
            if j == i + 1 || (i == 0 && j == r.len() - 1) {
                continue;
            }
            let c = [r[j][0], r[j][2]];
            let d = [r[(j + 1) % r.len()][0], r[(j + 1) % r.len()][2]];
            if a[0].min(b[0]) <= c[0].max(d[0])
                && c[0].min(d[0]) <= a[0].max(b[0])
                && a[1].min(b[1]) <= c[1].max(d[1])
                && c[1].min(d[1]) <= a[1].max(b[1])
                && orient(a, b, c) * orient(a, b, d) <= 0.
                && orient(c, d, a) * orient(c, d, b) <= 0.
            {
                return false;
            }
        }
    }
    true
}
impl Footprint {
    pub fn validate(&self) -> Result<(), String> {
        if !(3..=128).contains(&self.ring.len())
            || !finite(&self.center)
            || self.ring.iter().any(|v| !finite(v))
            || self.local_to_root.iter().any(|r| !finite(r))
            || self.local_to_root[3] != [0., 0., 0., 1.]
            || !finite(&[
                self.fill_offset,
                self.max_fill,
                self.reject_above_gap,
                self.rim_width,
                self.feather,
                self.opacity,
                self.max_tilt_degrees,
            ])
            || self.fill_offset.abs() > 16.
            || !(0. ..=24.).contains(&self.max_fill)
            || !(0. ..=24.).contains(&self.reject_above_gap)
            || !(0. ..=32.).contains(&self.rim_width)
            || !(0. ..=32.).contains(&self.feather)
            || !(0. ..=1.).contains(&self.opacity)
            || !(0. ..=80.).contains(&self.max_tilt_degrees)
            || !simple(&self.ring)
        {
            return Err("Invalid decoration footprint polygon or policy".into());
        }
        Ok(())
    }
}
struct Polygon<'a> {
    ring: &'a [[f32; 3]],
    center: [f32; 3],
    normal: [f32; 3],
    rim: f32,
    feather: f32,
    bounds: [f32; 4],
}
#[derive(Clone, Copy)]
enum Reject {
    Unsupported,
    Occupied,
    Protected,
    Holes,
    Outside,
    Tilt,
    Fill,
    Budget,
    TopologyFilter,
    TopologyRead,
    MaterialFilter,
    MaterialRead,
    PlacementBlocked,
}
impl<'a> Polygon<'a> {
    fn new(f: &Footprint, p: &decor_fit::Pose, ring: &'a mut Vec<[f32; 3]>) -> Result<Self, Reject> {
        ring.clear();
        ring.extend(f.ring.iter().map(|v| world(f, p, *v)));
        let mut normal = [0.; 3];
        for i in 1..ring.len() - 1 {
            let n = cross(sub(ring[i], ring[0]), sub(ring[i + 1], ring[0]));
            if dot(n, n) > 1e-8 {
                normal = n;
                break;
            }
        }
        let norm = dot(normal, normal).sqrt();
        if norm < 1e-4 || !norm.is_finite() {
            return Err(Reject::Unsupported);
        }
        normal = normal.map(|v| v / norm);
        if normal[1] < 0. {
            normal = normal.map(|v| -v);
        }
        if normal[1].clamp(-1., 1.).acos().to_degrees() > f.max_tilt_degrees {
            return Err(Reject::Tilt);
        }
        let mut center = world(f, p, f.center);
        if ring
            .iter()
            .any(|v| dot(normal, sub(*v, center)).abs() > 0.02)
        {
            return Err(Reject::Unsupported);
        }
        center[1] += f.fill_offset * p.scale[1];
        let origin = world(f, p, [0.; 3]);
        let sx = sub(world(f, p, [1., 0., 0.]), origin);
        let sz = sub(world(f, p, [0., 0., 1.]), origin);
        let scale = dot(sx, sx).sqrt().max(dot(sz, sz).sqrt());
        let rim = f.rim_width * scale;
        let feather = f.feather * scale;
        if rim + feather > 128. || !simple(ring) {
            return Err(Reject::Unsupported);
        }
        let bounds = [
            ring.iter().map(|v| v[0]).fold(f32::INFINITY, f32::min) - rim - feather,
            ring.iter().map(|v| v[0]).fold(f32::NEG_INFINITY, f32::max) + rim + feather,
            ring.iter().map(|v| v[2]).fold(f32::INFINITY, f32::min) - rim - feather,
            ring.iter().map(|v| v[2]).fold(f32::NEG_INFINITY, f32::max) + rim + feather,
        ];
        Ok(Self {
            ring,
            center,
            normal,
            rim,
            feather,
            bounds,
        })
    }
    fn distance(&self, x: f32, z: f32) -> (bool, f32) {
        let mut inside = false;
        let mut distance = f32::INFINITY;
        let next = self.ring[1..].iter().chain(&self.ring[..1]);
        for (a, b) in self.ring.iter().zip(next) {
            let dx = b[0] - a[0];
            let dz = b[2] - a[2];
            let t = (((x - a[0]) * dx + (z - a[2]) * dz) / (dx * dx + dz * dz)).clamp(0., 1.);
            let ex = x - a[0] - t * dx;
            let ez = z - a[2] - t * dz;
            // A component is a lower bound on the segment distance.
            if ex.abs() < distance && ez.abs() < distance {
                distance = distance.min(ex.hypot(ez));
            }
            if (a[2] > z) != (b[2] > z) && x < (b[0] - a[0]) * (z - a[2]) / (b[2] - a[2]) + a[0] {
                inside = !inside;
            }
        }
        (inside, distance)
    }
    fn target(&self, x: f32, z: f32) -> f32 {
        self.center[1]
            - (self.normal[0] * (x - self.center[0]) + self.normal[2] * (z - self.center[2]))
                / self.normal[1]
    }
    fn weight(&self, inside: bool, d: f32, fill: bool) -> f32 {
        if inside {
            return if fill { 1. } else { 0. };
        }
        if d <= self.rim {
            1.
        } else if self.feather > 0. {
            let t = ((d - self.rim) / self.feather).clamp(0., 1.);
            1. - t * t * (3. - 2. * t)
        } else {
            0.
        }
    }
}
fn index(n: usize, size: f32, x: f32) -> usize {
    ((x / size + 0.5) * n as f32).clamp(0., (n - 1) as f32) as usize
}
fn count(c: &mut Counts, r: Reject) {
    match r {
        Reject::Unsupported => c.unsupported += 1,
        Reject::Occupied => c.occupied += 1,
        Reject::Protected => c.protected += 1,
        Reject::Holes => c.holes += 1,
        Reject::Outside => c.outside += 1,
        Reject::Tilt => c.tilt += 1,
        Reject::Fill => c.fill_limit += 1,
        Reject::Budget => c.budget_limited += 1,
        Reject::TopologyFilter => c.topology_filter += 1,
        Reject::TopologyRead => c.occupied += 1,
        Reject::MaterialFilter => c.material_filter += 1,
        Reject::MaterialRead => c.occupied += 1,
        Reject::PlacementBlocked => c.placement_blocked += 1,
    }
}
struct Stage<'a> {
    height_reservations: Option<&'a crate::height_reservations::HeightReservations>,
    support_only: bool,
    baseline_protected: u32,
    active_exception: u32,
    checked_height: bool,
    visit_limit: usize,
    topology_index: Option<crate::topology_query::Index>,
    scope_index: Option<usize>,
    scope_protected: Option<u32>,
    scope_height_limit: Option<f32>,
    layers: &'a Layers,
    alpha: &'a [u8],
    height: Vec<i16>,
    occupancy: Vec<u8>,
    support_marks: Vec<u64>,
    connected_reserved: Option<Vec<u64>>,
    blocking: Option<Vec<u8>>,
    topology: Option<crate::decor_topology::State>,
    materials: Option<crate::decor_material::State>,
    height_effects: Option<crate::decor_height::Report>,
    collider_effects: Option<crate::decor_collider::Report>,
    visits: usize,
}
impl Stage<'_> {
    fn reserve_supports(
        &mut self,
        supports: &[([f32; 2], f32)],
        reserved: &mut Vec<usize>,
        cancelled: &impl Fn() -> bool,
    ) -> Result<bool, Reject> {
        let n = self.layers.surface_resolution;
        let size = self.layers.size as f32;
        let diag = size / n as f32 * 0.5 * 2_f32.sqrt();
        let mut marks = std::mem::take(&mut self.support_marks);
        marks.resize((n * n).div_ceil(64), 0);
        let start = reserved.len();
        let result = (|| {
            for (center, radius) in supports {
                for z in index(n, size, center[1] - radius - diag)
                    ..=index(n, size, center[1] + radius + diag)
                {
                    if cancelled() {
                        return Ok(false);
                    }
                    for x in index(n, size, center[0] - radius - diag)
                        ..=index(n, size, center[0] + radius + diag)
                    {
                        self.visit(1)?;
                        let cell = z * n + x;
                        let bit = 1_u64 << (cell % 64);
                        if marks[cell / 64] & bit != 0 {
                            continue;
                        }
                        let wx = ((x as f32 + 0.5) / n as f32 - 0.5) * size;
                        let wz = ((z as f32 + 0.5) / n as f32 - 0.5) * size;
                        if (wx - center[0]).hypot(wz - center[1]) <= radius + diag {
                            self.check_cell(cell)?;
                            marks[cell / 64] |= bit;
                            reserved.push(cell);
                        }
                    }
                }
            }
            Ok(true)
        })();
        // Restore scratch on success, rejection, exhaustion and cancellation.
        for &cell in &reserved[start..] {
            marks[cell / 64] &= !(1_u64 << (cell % 64));
        }
        self.support_marks = marks;
        result
    }
    fn visit(&mut self, cost: usize) -> Result<(), Reject> {
        if cost > self.visit_limit.saturating_sub(self.visits) {
            return Err(Reject::Budget);
        }
        self.visits += cost;
        Ok(())
    }
    fn check_cell(&self, i: usize) -> Result<(), Reject> {
        CellGuard {
            alpha: self.alpha,
            occupancy: &self.occupancy,
            connected_reserved: self.connected_reserved.as_deref(),
            original: &self.layers.topology,
            current: self.topology.as_ref().map(|s| s.cells.as_slice()),
            protected: self.scope_protected.unwrap_or(self.baseline_protected),
            exception: self.active_exception,
            dynamic: self.scope_protected.is_some(),
        }
        .check(i)
    }
    fn prepare_topology(
        &mut self,
        spec: &crate::decor_topology::Prefab,
        pose: &decor_fit::Pose,
        source_stage: &decor::Stage,
        reserved: &mut Vec<usize>,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Option<crate::decor_topology::Plan>, Reject> {
        let guard = CellGuard {
            alpha: self.alpha,
            occupancy: &self.occupancy,
            connected_reserved: self.connected_reserved.as_deref(),
            original: &self.layers.topology,
            current: self.topology.as_ref().map(|s| s.cells.as_slice()),
            protected: self.scope_protected.unwrap_or(self.baseline_protected),
            exception: self.active_exception,
            dynamic: self.scope_protected.is_some(),
        };
        if self.checked_height {
            let Some(()) = self
                .topology
                .as_ref()
                .ok_or(Reject::Unsupported)?
                .precheck_additions(
                    spec,
                    pose,
                    self.layers.surface_resolution,
                    self.layers.size as f32,
                    &mut self.visits,
                    self.visit_limit,
                    cancelled,
                    |cell| guard.check(cell),
                )
                .map_err(topology_rejection)?
            else {
                return Ok(None);
            };
        }
        let plan = self
            .topology
            .as_ref()
            .ok_or(Reject::Unsupported)?
            .prepare_checked(
                spec,
                pose,
                source_stage,
                self.layers.surface_resolution,
                self.layers.size as f32,
                &mut self.visits,
                self.visit_limit,
                cancelled,
                |cell| guard.check(cell),
            )
            .map_err(topology_rejection)?;
        let Some(plan) = plan else {
            return Ok(None);
        };
        for (i, _) in &plan.writes {
            self.check_cell(*i)?;
            reserved.push(*i);
        }
        reserved.sort_unstable();
        reserved.dedup();
        Ok(Some(plan))
    }

}
struct CellGuard<'a> {
    alpha: &'a [u8],
    occupancy: &'a [u8],
    connected_reserved: Option<&'a [u64]>,
    original: &'a [i32],
    current: Option<&'a [i32]>,
    protected: u32,
    exception: u32,
    dynamic: bool,
}
impl CellGuard<'_> {
    fn check(&self, i: usize) -> Result<(), Reject> {
        if self.alpha[i] < 128 {
            return Err(Reject::Holes);
        }
        let owned = self
            .connected_reserved
            .is_some_and(|bits| bits[i / 64] & (1 << (i % 64)) != 0);
        if self.exception != 0 || self.dynamic || owned {
            let protected = self.protected & !self.exception;
            if self.original[i] as u32 & protected != 0
                || self.current.is_some_and(|current| current[i] as u32 & protected != 0)
            {
                Err(Reject::Protected)
            } else if self.occupancy[i] == 1 && !owned {
                Err(Reject::Occupied)
            } else {
                Ok(())
            }
        } else {
            match self.occupancy[i] {
                1 => Err(Reject::Occupied),
                2 => Err(Reject::Protected),
                _ => Ok(()),
            }
        }
    }
}
#[derive(Default)]
struct GeometryScratch {
    ring: Vec<[f32; 3]>,
    supports: Vec<([f32; 2], f32)>,
    blockers: Vec<([f32; 2], f32, f32)>,
    raises: Vec<([f32; 3], f32, f32, f32)>,
}

impl Stage<'_> {
    fn prepare(
        &mut self,
        spec: &Prefab,
        p: &decor_fit::Pose,
        cancelled: &impl Fn() -> bool,
        source_stage: &decor::Stage,
        geometry: &mut GeometryScratch,
    ) -> Result<Option<Patch>, Reject> {
        // The game's placement raster is separate from topology and from our
        // support reservations. Honor only explicitly authored checkpoint flags.
        if self.checked_height
            && let (Some(raster), Some(topology)) = (&self.blocking, &spec.topology)
        {
            for q in topology.queries.iter().filter(|q| q.check_placement) {
                if cancelled() {
                    return Ok(None);
                }
                if self.visits >= self.visit_limit {
                    return Err(Reject::Budget);
                }
                self.visits += 1;
                let local = decor_fit::rotate(
                    p.rotation,
                    std::array::from_fn(|i| q.position[i] * p.scale[i]),
                );
                let size = self.layers.size as f32;
                let n = self.layers.surface_resolution;
                let cell = index(n, size, p.position[2] + local[2]) * n
                    + index(n, size, p.position[0] + local[0]);
                if raster[cell] != 0 {
                    return Err(Reject::PlacementBlocked);
                }
            }
        }
        let has_height = self.height_effects.is_some()
            && spec
                .height_modifiers
                .as_ref()
                .is_some_and(|m| !m.is_empty())
            || self.collider_effects.is_some()
                && spec.contributors.as_ref().is_some_and(|c| !c.is_empty());
        let has_topology = self.checked_height
            && self.topology.is_some()
            && spec
                .topology
                .as_ref()
                .is_some_and(|topology| !topology.modifiers.is_empty());
        let GeometryScratch {
            ring,
            supports,
            blockers,
            raises,
        } = geometry;
        let poly = spec
            .footprint
            .as_ref()
            .map(|f| Polygon::new(f, p, ring))
            .transpose()?;
        if !self.support_only
            && poly.is_none()
            && (self.blocking.is_none() || spec.blockers.is_empty())
            && (self.materials.is_none() || spec.materials.as_ref().is_none_or(|m| m.is_empty()))
            && !has_height
        {
            return Err(Reject::Unsupported);
        }
        let size = self.layers.size as f32;
        let n = self.layers.surface_resolution;
        let hr = self.layers.height_resolution;
        let half = size * 0.5;
        let diag = size / n as f32 * 0.5 * 2_f32.sqrt();
        let height_diag = size / (hr - 1) as f32 * 2. * 2_f32.sqrt();
        supports.clear();
        supports.extend(spec.support.iter().map(|s| {
            let local = crate::decor_fit::rotate(
                p.rotation,
                std::array::from_fn(|i| s.position[i] * p.scale[i]),
            );
            (
                [p.position[0] + local[0], p.position[2] + local[2]],
                s.radius * 2_f32.sqrt() + height_diag,
            )
        }));
        // Source TerrainModifierEx scales the authored radius/fade by instance Y,
        // while rotating the prefab-local center using the complete instance pose.
        blockers.clear();
        raises.clear();
        if self.blocking.is_some() {
            blockers.extend(spec.blockers.iter().map(|b| {
                let v = decor_fit::rotate(
                    p.rotation,
                    std::array::from_fn(|i| b.position[i] * p.scale[i]),
                );
                (
                    [p.position[0] + v[0], p.position[2] + v[2]],
                    b.radius * p.scale[1],
                    b.fade * p.scale[1],
                )
            }));
        }
        if blockers.iter().any(|(c, r, f)| {
            !finite(c) || !r.is_finite() || !f.is_finite() || *r > 256. || *f > 256.
        }) {
            return Err(Reject::Unsupported);
        }
        if self.blocking.is_some() {
            raises.extend(spec.raises.iter().map(|r| {
                let v = decor_fit::rotate(
                    p.rotation,
                    std::array::from_fn(|i| r.position[i] * p.scale[i]),
                );
                (
                    std::array::from_fn::<_, 3, _>(|i| p.position[i] + v[i]),
                    r.radius * p.scale[1],
                    r.fade * p.scale[1],
                    r.opacity,
                )
            }));
        }
        if raises.iter().any(|(c, r, f, _)| {
            !finite(c)
                || !r.is_finite()
                || !f.is_finite()
                || *r > 256.
                || *f > 256.
                || !(-500. ..=500.).contains(&c[1])
        }) {
            return Err(Reject::Unsupported);
        }
        let mut bounds = poly.as_ref().map_or(
            [p.position[0], p.position[0], p.position[2], p.position[2]],
            |p| p.bounds,
        );
        for (center, radius, _) in blockers.iter() {
            bounds[0] = bounds[0].min(center[0] - radius);
            bounds[1] = bounds[1].max(center[0] + radius);
            bounds[2] = bounds[2].min(center[1] - radius);
            bounds[3] = bounds[3].max(center[1] + radius);
        }
        for (center, radius, _, _) in raises.iter() {
            bounds[0] = bounds[0].min(center[0] - radius - height_diag);
            bounds[1] = bounds[1].max(center[0] + radius + height_diag);
            bounds[2] = bounds[2].min(center[2] - radius - height_diag);
            bounds[3] = bounds[3].max(center[2] + radius + height_diag);
        }
        for (center, r) in supports.iter() {
            bounds[0] = bounds[0].min(center[0] - r);
            bounds[1] = bounds[1].max(center[0] + r);
            bounds[2] = bounds[2].min(center[1] - r);
            bounds[3] = bounds[3].max(center[1] + r);
        }
        if bounds.iter().any(|v| !v.is_finite() || v.abs() > half) {
            return Err(Reject::Outside);
        }
        if self.checked_height {
            for (center, radius) in supports.iter() {
                let x = index(n, size, center[0]);
                let z = index(n, size, center[1]);
                if let Err(reason) = self.check_cell(z * n + x) {
                    self.visit(1)?;
                    let wx = ((x as f32 + 0.5) / n as f32 - 0.5) * size;
                    let wz = ((z as f32 + 0.5) / n as f32 - 0.5) * size;
                    if (wx - center[0]).hypot(wz - center[1]) <= radius + diag {
                        return Err(reason);
                    }
                }
            }
        }
        // Dense object passes repeatedly propose footprints over occupied ground.
        // Probe the centre and perimeter before scanning the complete bounds.
        // Each rejection still requires an actual footprint cell, including its
        // authored rim. Bounding-box probes cannot reject concave empty regions.
        if self.checked_height
            && let Some(poly) = &poly
        {
            let [left, right, top, bottom] = poly.bounds;
            let middle_x = (left + right) * 0.5;
            let middle_z = (top + bottom) * 0.5;
            for [wx, wz] in [
                [middle_x, middle_z],
                [left, middle_z],
                [right, middle_z],
                [middle_x, top],
                [middle_x, bottom],
                [left, top],
                [right, top],
                [left, bottom],
                [right, bottom],
            ] {
                let x = index(n, size, wx);
                let z = index(n, size, wz);
                if let Err(reason) = self.check_cell(z * n + x) {
                    self.visit(poly.ring.len())?;
                    let wx = ((x as f32 + 0.5) / n as f32 - 0.5) * size;
                    let wz = ((z as f32 + 0.5) / n as f32 - 0.5) * size;
                    let (inside, distance) = poly.distance(wx, wz);
                    if inside || distance <= poly.rim + poly.feather + diag {
                        return Err(reason);
                    }
                }
            }
        }
        let mut reserved = Vec::new();
        let mut blocked = Vec::new();
        if self.checked_height && poly.is_none() && blockers.is_empty() && raises.is_empty() {
            // Sparse authored anchors can span a large empty rectangle. Visit
            // each support disk with the same raster predicate, then union it.
            if !self.reserve_supports(supports, &mut reserved, cancelled)? {
                return Ok(None);
            }
            reserved.sort_unstable();
        } else {
            for z in index(n, size, bounds[2] - diag)..=index(n, size, bounds[3] + diag) {
                if cancelled() {
                    return Ok(None);
                }
                for x in index(n, size, bounds[0] - diag)..=index(n, size, bounds[1] + diag) {
                    self.visit(
                        poly.as_ref().map_or(0, |p| p.ring.len())
                            + supports.len()
                            + blockers.len()
                            + raises.len(),
                    )?;
                    let wx = ((x as f32 + 0.5) / n as f32 - 0.5) * size;
                    let wz = ((z as f32 + 0.5) / n as f32 - 0.5) * size;
                    // ApplyFilter uses surface cell centers and a strict lerp > 0.5.
                    // The blocker map does not use the modifier's Opacity.
                    let is_blocked = blockers.iter().any(|(c, radius, fade)| {
                        let d = (wx - c[0]).hypot(wz - c[1]);
                        if *fade == 0. {
                            d < *radius
                        } else {
                            ((*radius - d) / *fade).clamp(0., 1.) > 0.5
                        }
                    });
                    if poly.as_ref().is_some_and(|p| {
                        let (inside, d) = p.distance(wx, wz);
                        inside || d <= p.rim + p.feather + diag
                    }) || is_blocked
                        || raises.iter().any(|(c, r, _, _)| {
                            (wx - c[0]).hypot(wz - c[2]) <= r + height_diag + diag
                        })
                        || supports
                            .iter()
                            .any(|(c, r)| (wx - c[0]).hypot(wz - c[1]) <= r + diag)
                    {
                        let i = z * n + x;
                        self.check_cell(i)?;
                        reserved.push(i);
                        if is_blocked {
                            blocked.push(i);
                        }
                    }
                }
            }
        }
        if reserved.is_empty()
            && (self.materials.is_none() || spec.materials.as_ref().is_none_or(|m| m.is_empty()))
            && !has_height
            && !has_topology
        {
            return Err(Reject::Unsupported);
        }
        let hindex =
            |x: f32| ((x / size + 0.5) * (hr - 1) as f32).clamp(0., (hr - 1) as f32) as usize;
        // Live transactions resolve height-independent topology dependencies
        // before preparing the height changes that would be discarded on failure.
        let mut topology = None;
        if self.checked_height
            && self.topology.is_some()
            && let Some(spec) = &spec.topology
        {
            let Some(plan) =
                self.prepare_topology(spec, p, source_stage, &mut reserved, cancelled)?
            else {
                return Ok(None);
            };
            topology = Some(plan);
        }
        let mut writes = Vec::new();
        let mut maximum_fill = 0_f32;
        if let (Some(poly), Some(f)) = (&poly, &spec.footprint) {
            for z in hindex(poly.bounds[2])..=(hindex(poly.bounds[3]) + 1).min(hr - 1) {
                if cancelled() {
                    return Ok(None);
                }
                for x in hindex(poly.bounds[0])..=(hindex(poly.bounds[1]) + 1).min(hr - 1) {
                    self.visit(poly.ring.len())?;
                    let wx = (x as f32 / (hr - 1) as f32 - 0.5) * size;
                    let wz = (z as f32 / (hr - 1) as f32 - 0.5) * size;
                    let (inside, d) = poly.distance(wx, wz);
                    let weight = poly.weight(inside, d, f.fill_interior) * f.opacity;
                    if weight <= 0. {
                        continue;
                    }
                    self.check_cell(index(n, size, wz) * n + index(n, size, wx))?;
                    let i = z * hr + x;
                    let h = decode(self.height[i]);
                    let target = poly.target(wx, wz);
                    if !target.is_finite() || !(-500. ..=500.).contains(&target) {
                        return Err(Reject::Fill);
                    }
                    if f.reject_above_gap > 0. && h > target + f.reject_above_gap {
                        return Err(Reject::Fill);
                    }
                    let gap = (target - h).max(0.);
                    if gap > f.max_fill + 0.0001 {
                        return Err(Reject::Fill);
                    }
                    let next = encode(h + gap * weight);
                    if decode(next) - h > f.max_fill + 0.0001 {
                        return Err(Reject::Fill);
                    }
                    if next != self.height[i] {
                        maximum_fill = maximum_fill.max(decode(next) - h);
                        writes.push((i, next));
                    }
                }
            }
        }
        if !raises.is_empty() {
            let mut combined: BTreeMap<_, _> = writes.into_iter().collect();
            for (center, radius, fade, opacity) in raises.iter().copied() {
                let cx = (center[0] / size + 0.5) * hr as f32;
                let cz = (center[2] / size + 0.5) * hr as f32;
                let radius_pixels = radius / size * hr as f32;
                let fade_pixels = fade / size * hr as f32;
                for z in index(hr, size, center[2] - radius)..=index(hr, size, center[2] + radius) {
                    if cancelled() {
                        return Ok(None);
                    }
                    for x in
                        index(hr, size, center[0] - radius)..=index(hr, size, center[0] + radius)
                    {
                        self.visit(1)?;
                        let d = (x as f32 + 0.5 - cx).hypot(z as f32 + 0.5 - cz);
                        let weight = if fade_pixels == 0. {
                            if d < radius_pixels { 1. } else { 0. }
                        } else {
                            ((radius_pixels - d) / fade_pixels).clamp(0., 1.)
                        };
                        let t = (weight * opacity).clamp(0., 1.);
                        if t == 0. {
                            continue;
                        }
                        let i = z * hr + x;
                        let h = decode(self.height[i]);
                        let next = encode(h + (center[1] - h).max(0.) * (t * t * (3. - 2. * t)));
                        let next = next.max(*combined.get(&i).unwrap_or(&self.height[i]));
                        let fill = decode(next) - h;
                        // Explicit Papyrus terrain-change bound, independent of client defaults.
                        if fill > 4.0001 {
                            return Err(Reject::Fill);
                        }
                        if next != self.height[i] {
                            let wx = (x as f32 / (hr - 1) as f32 - 0.5) * size;
                            let wz = (z as f32 / (hr - 1) as f32 - 0.5) * size;
                            self.check_cell(index(n, size, wz) * n + index(n, size, wx))?;
                            combined.insert(i, next);
                            maximum_fill = maximum_fill.max(fill);
                        }
                    }
                }
            }
            writes = combined.into_iter().collect();
        }
        let height_effects = if self.height_effects.is_some() {
            let modifiers = spec.height_modifiers.as_ref().ok_or(Reject::Unsupported)?;
            let input = crate::decor_height::Input {
                heights: &self.height,
                resolution: hr,
                size,
                initial: &writes,
                modifiers,
                pose: p,
            };
            let plan = if self.checked_height && writes.is_empty() {
                let guard = CellGuard {
                    alpha: self.alpha,
                    occupancy: &self.occupancy,
                    connected_reserved: self.connected_reserved.as_deref(),
                    original: &self.layers.topology,
                    current: self.topology.as_ref().map(|s| s.cells.as_slice()),
                    protected: self.scope_protected.unwrap_or(self.baseline_protected),
                    exception: self.active_exception,
                    dynamic: self.scope_protected.is_some(),
                };
                // The checked height adapter has finished every modifier at a
                // sample before calling this guard. With no later collider
                // contribution, its changed value is already final.
                let reservations = self
                    .height_reservations
                    .filter(|_| spec.contributors.as_ref().is_none_or(|c| c.is_empty()));
                crate::decor_height::prepare_checked(
                    input,
                    &mut self.visits,
                    self.visit_limit,
                    cancelled,
                    |i| {
                        if reservations.is_some_and(|mask| mask.contains(i)) {
                            return Err(Reject::Protected);
                        }
                        let wx = ((i % hr) as f32 / (hr - 1) as f32 - 0.5) * size;
                        let wz = ((i / hr) as f32 / (hr - 1) as f32 - 0.5) * size;
                        guard.check(index(n, size, wz) * n + index(n, size, wx))
                    },
                )
                .map_err(|e| match e {
                    crate::decor_height::CheckedFailure::Cell(r) => r,
                    crate::decor_height::CheckedFailure::Height(e) => height_rejection(e),
                })?
            } else {
                let result = if self.support_only {
                    crate::decor_height::prepare_ordered(
                        input,
                        &mut self.visits,
                        self.visit_limit,
                        cancelled,
                        true,
                    )
                } else {
                    crate::decor_height::prepare(
                        input,
                        &mut self.visits,
                        self.visit_limit,
                        cancelled,
                    )
                };
                result.map_err(height_rejection)?
            };
            let Some(plan) = plan else {
                return Ok(None);
            };
            for (i, h) in &plan.writes {
                let wx = ((*i % hr) as f32 / (hr - 1) as f32 - 0.5) * size;
                let wz = ((*i / hr) as f32 / (hr - 1) as f32 - 0.5) * size;
                let cell = index(n, size, wz) * n + index(n, size, wx);
                self.check_cell(cell)?;
                reserved.push(cell);
                maximum_fill = maximum_fill.max((decode(*h) - decode(self.height[*i])).max(0.));
            }
            writes = plan.writes;
            reserved.sort_unstable();
            reserved.dedup();
            Some(plan.report)
        } else {
            None
        };
        let collider_effects = if self.collider_effects.is_some() {
            let plan = crate::decor_collider::prepare(
                crate::decor_collider::Input {
                    heights: &self.height,
                    resolution: hr,
                    size,
                    initial: &writes,
                    contributors: spec.contributors.as_ref().ok_or(Reject::Unsupported)?,
                    pose: p,
                },
                &mut self.visits,
                self.visit_limit,
                cancelled,
            )
            .map_err(|e| match e {
                crate::decor_collider::Failure::Outside => Reject::Outside,
                crate::decor_collider::Failure::Budget => Reject::Budget,
                crate::decor_collider::Failure::Unsupported => Reject::Unsupported,
            })?;
            let Some(plan) = plan else {
                return Ok(None);
            };
            for (i, h) in &plan.writes {
                let wx = ((*i % hr) as f32 / (hr - 1) as f32 - 0.5) * size;
                let wz = ((*i / hr) as f32 / (hr - 1) as f32 - 0.5) * size;
                let cell = index(n, size, wz) * n + index(n, size, wx);
                self.check_cell(cell)?;
                reserved.push(cell);
                maximum_fill = maximum_fill.max((decode(*h) - decode(self.height[*i])).max(0.));
            }
            writes = plan.writes;
            reserved.sort_unstable();
            reserved.dedup();
            Some(plan.report)
        } else {
            None
        };
        if let Some(reservations) = self.height_reservations {
            self.visit(writes.len())?;
            if writes
                .iter()
                .any(|&(i, value)| value != self.height[i] && reservations.contains(i))
            {
                return Err(Reject::Protected);
            }
        }
        if !self.checked_height
            && self.topology.is_some()
            && let Some(spec) = &spec.topology
        {
            let Some(plan) =
                self.prepare_topology(spec, p, source_stage, &mut reserved, cancelled)?
            else {
                return Ok(None);
            };
            topology = Some(plan);
        }
        let materials = if let (Some(state), Some(modifiers), Some(dependencies)) =
            (&self.materials, &spec.materials, &spec.topology)
        {
            let plan = state
                .prepare(
                    crate::decor_material::Input {
                        modifiers,
                        dependencies,
                        pose: p,
                        stage: source_stage,
                        size,
                        terrain: Some(crate::native_layers::View {
                            topology: self.topology.as_ref().map_or(
                                self.layers.topology.as_slice(),
                                |state| state.cells.as_slice(),
                            ),
                            ..self.layers.view()
                        }),
                    },
                    &mut self.visits,
                    self.visit_limit,
                    cancelled,
                )
                .map_err(|e| match e {
                    crate::decor_material::Failure::Filter => Reject::MaterialFilter,
                    crate::decor_material::Failure::Conflict => Reject::MaterialRead,
                    crate::decor_material::Failure::Outside => Reject::Outside,
                    crate::decor_material::Failure::Budget => Reject::Budget,
                    crate::decor_material::Failure::Unsupported => Reject::Unsupported,
                })?;
            let Some(plan) = plan else {
                return Ok(None);
            };
            for (i, _) in &plan.writes {
                self.check_cell(*i)?;
                reserved.push(*i);
            }
            reserved.sort_unstable();
            reserved.dedup();
            Some(plan)
        } else {
            None
        };
        if reserved.is_empty() {
            return Err(Reject::Unsupported);
        }
        if let Some(limit) = self.scope_height_limit
            && writes
                .iter()
                .any(|(i, h)| (decode(*h) - decode(self.height[*i])).abs() > limit + 1000. / 32766.)
        {
            return Err(Reject::Fill);
        }
        let topology_dirty = self
            .topology_index
            .as_ref()
            .zip(topology.as_ref())
            .map(|(index, plan)| index.dirty(plan.writes.iter().map(|p| p.0)));
        if let Some((index, dirty)) = self.topology_index.as_ref().zip(topology_dirty.as_ref()) {
            self.visit(index.refresh_cost(dirty))?;
        }
        Ok(Some(Patch {
            writes,
            reserved,
            blocked,
            topology,
            materials,
            height_effects,
            collider_effects,
            maximum_fill,
            topology_dirty,
        }))
    }
}
struct Patch {
    topology_dirty: Option<BTreeSet<usize>>,
    writes: Vec<(usize, i16)>,
    reserved: Vec<usize>,
    blocked: Vec<usize>,
    topology: Option<crate::decor_topology::Plan>,
    materials: Option<crate::decor_material::Plan>,
    height_effects: Option<crate::decor_height::Report>,
    collider_effects: Option<crate::decor_collider::Report>,
    maximum_fill: f32,
}
#[derive(Clone, Copy)]
pub struct Input<'a, 'm> {
    pub height_reservations: Option<&'a crate::height_reservations::HeightReservations>,
    pub layers: &'a Layers,
    pub alpha: &'a [u8],
    pub candidates: &'m [decor::Candidate],
    pub stages: &'m [decor::Stage],
    pub poses: &'m [decor_fit::Pose],
    pub prefabs: &'m BTreeMap<String, Prefab>,
    pub obstacles: &'m [Obstacle],
    pub protected_topology: u32,
}
pub fn apply(input: Input<'_, '_>, cancelled: impl Fn() -> bool) -> Result<Option<Output>, String> {
    apply_inner(input, false, cancelled)
}
/// Revision 4 adds source-authored blockers and permits blocker-only prefabs.
/// Conservative reservations remain distinct from the exact blocker raster.
pub fn apply_blockers(
    input: Input<'_, '_>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    apply_inner(input, true, cancelled)
}
fn apply_inner(
    input: Input<'_, '_>,
    blocking: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    apply_revision(input, if blocking { 4 } else { 3 }, 0, cancelled)
}
pub fn apply_topology(
    input: Input<'_, '_>,
    known_mask: u32,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    if known_mask == 0 || input.protected_topology & !known_mask != 0 {
        return Err("Unbound topology policy".into());
    }
    apply_revision(input, 5, known_mask, cancelled)
}
pub fn apply_materials(
    input: Input<'_, '_>,
    known_mask: u32,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    if known_mask == 0 || input.protected_topology & !known_mask != 0 {
        return Err("Unbound material/topology policy".into());
    }
    apply_revision(input, 8, known_mask, cancelled)
}
pub fn apply_height(
    input: Input<'_, '_>,
    known_mask: u32,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    if known_mask == 0 || input.protected_topology & !known_mask != 0 {
        return Err("Unbound height/topology policy".into());
    }
    apply_revision(input, 9, known_mask, cancelled)
}
pub fn apply_colliders(
    input: Input<'_, '_>,
    known_mask: u32,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    if known_mask == 0 || input.protected_topology & !known_mask != 0 {
        return Err("Unbound collider/topology policy".into());
    }
    apply_revision(input, 10, known_mask, cancelled)
}
pub fn apply_road_objects(
    input: Input<'_, '_>,
    known_mask: u32,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    if known_mask == 0 || input.protected_topology & !known_mask != 0 {
        return Err("Unbound road-object effect policy".into());
    }
    apply_revision(input, 11, known_mask, cancelled)
}
fn apply_revision(
    input: Input<'_, '_>,
    revision: u8,
    known_mask: u32,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    let Some(mut transaction) = begin_revision(input, revision, known_mask, &cancelled)? else {
        return Ok(None);
    };
    for (i, p) in input.poses.iter().enumerate() {
        let c = &input.candidates[p.candidate];
        let placement = &input.stages[c.stage];
        let path = &placement.prefabs[c.prefab];
        if transaction
            .apply_pose(i, placement, path, &input.prefabs[path], p, &cancelled)?
            .is_none()
        {
            return Ok(None);
        }
        if transaction.report.budget_exhausted {
            transaction.report.unprocessed_poses = input.poses.len() - i - 1;
            break;
        }
    }
    transaction.finish(&cancelled)
}
struct Transaction<'a> {
    stage: Stage<'a>,
    report: Report,
    revision: u8,
    last_rejection: Option<Reject>,
    geometry: GeometryScratch,
    cliff_edits: Option<CliffEdits>,
}

struct CliffEdits {
    side: usize,
    rows: Vec<u128>,
    dirty: Vec<usize>,
}

impl CliffEdits {
    fn clear(&mut self) {
        if !self.dirty.is_empty() {
            self.rows.fill(0);
            self.dirty.clear();
        }
    }

    fn mark(&mut self, x: usize, z: usize) {
        use crate::topology_query::BLOCK;
        let (column, row) = (x / BLOCK, z / BLOCK);
        let bit = 1_u128 << column;
        if self.rows[row] & bit == 0 {
            self.rows[row] |= bit;
            self.dirty.push(row * self.side + column);
        }
    }

    fn intersects(&self, left: usize, right: usize, top: usize, bottom: usize) -> bool {
        let columns = (u128::MAX << left) & (u128::MAX >> (127 - right));
        self.rows[top..=bottom].iter().any(|row| row & columns != 0)
    }
}
/// One private road-object destination shared across incremental fitting and
/// admission. Occupancy, filter read locks and work limits survive every call.
/// Only `finish` can return owned grids; cancellation or invalid input poisons
/// publication even if a later cancellation callback returns false.
pub struct RoadTransaction<'a> {
    inner: Transaction<'a>,
    prefabs: BTreeMap<String, Prefab>,
    seen: BTreeSet<usize>,
    // Live objects allocate contiguous IDs across road and scenery stages.
    // Track their next ID in constant space; rejected attempts consume IDs too.
    next_object_pose: Option<usize>,
    state: RoadState,
    role_exceptions: u32,
    material_role_exceptions: u32,
    namespace: Option<String>,
    bindings: Option<BTreeMap<String, String>>,
}
enum RoadState {
    Ready,
    Cancelled,
    Failed,
}
fn height_rejection(e: crate::decor_height::Failure) -> Reject {
    match e {
        crate::decor_height::Failure::Outside => Reject::Outside,
        crate::decor_height::Failure::Budget => Reject::Budget,
        crate::decor_height::Failure::Unsupported => Reject::Unsupported,
    }
}
fn topology_rejection(e: crate::decor_topology::CheckedFailure<Reject>) -> Reject {
    match e {
        crate::decor_topology::CheckedFailure::Cell(error) => error,
        crate::decor_topology::CheckedFailure::Topology(error) => match error {
            crate::decor_topology::Failure::Filter => Reject::TopologyFilter,
            crate::decor_topology::Failure::Conflict => Reject::TopologyRead,
            crate::decor_topology::Failure::Outside => Reject::Outside,
            crate::decor_topology::Failure::Budget => Reject::Budget,
            crate::decor_topology::Failure::Unsupported => Reject::Unsupported,
        },
    }
}
fn role_exception(
    prefab: &Prefab,
    placement: &decor::Stage,
    roles: u32,
    material_roles: u32,
) -> u32 {
    let required = placement.filter.topology_all as u32
        | if (placement.filter.topology_any as u32).count_ones() == 1 {
            placement.filter.topology_any as u32
        } else {
            0
        };
    if prefab.footprint.is_none()
        && prefab.blockers.is_empty()
        && prefab.raises.is_empty()
        && prefab
            .height_modifiers
            .as_ref()
            .is_none_or(|v| v.is_empty())
        && prefab.contributors.as_ref().is_none_or(|v| v.is_empty())
    {
        required
            & if prefab.materials.as_ref().is_none_or(|v| v.is_empty()) {
                roles
            } else {
                material_roles
            }
    } else {
        0
    }
}
impl<'a> RoadTransaction<'a> {
    pub(crate) fn start_cliff_batch(&mut self) {
        let side = self
            .inner
            .stage
            .layers
            .surface_resolution
            .div_ceil(crate::topology_query::BLOCK);
        let edits = self.inner.cliff_edits.get_or_insert_with(|| CliffEdits {
            side,
            // A validated native raster has at most 2,048 surface cells
            // across, so a row of 16-cell tiles fits in 128 bits.
            rows: vec![0; side],
            dirty: Vec::new(),
        });
        edits.clear();
    }

    pub(crate) fn finish_cliff_batches(&mut self) {
        self.inner.cliff_edits = None;
    }

    pub(crate) fn changed_cliff_tiles(&self) -> &[usize] {
        self.inner.cliff_edits.as_ref().map_or(&[], |edits| &edits.dirty)
    }

    pub(crate) fn changed_during_cliff_batch(&self, [x, z]: [usize; 2], radius: usize) -> bool {
        use crate::topology_query::BLOCK;
        let Some(edits) = &self.inner.cliff_edits else {
            return true;
        };
        if edits.dirty.is_empty() {
            return false;
        }
        let n = self.inner.stage.layers.surface_resolution;
        let left = x.saturating_sub(radius) / BLOCK;
        let right = (x + radius).min(n - 1) / BLOCK;
        let top = z.saturating_sub(radius) / BLOCK;
        let bottom = (z + radius).min(n - 1) / BLOCK;
        edits.intersects(left, right, top, bottom)
    }
    /// Run a whole-map procedural pass between object stages, retaining the
    /// destination's occupancy, filter locks and object-attribute work allowance.
    /// The procedural pass and its index rebuild have their own bounded budget.
    pub fn apply_cliff_topology(
        &mut self,
        policy: crate::cliff_topology::Policy,
        keep_existing: bool,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<crate::cliff_topology::Report>, String> {
        let result = (|| {
            if !matches!(self.state, RoadState::Ready) || self.inner.report.engine != OBJECT_VERSION
            {
                return Err("Cliff topology requires a live object transaction".into());
            }
            let stage = &self.inner.stage;
            let known = stage
                .topology
                .as_ref()
                .ok_or("Missing live topology")?
                .report
                .known_mask;
            if (policy.cliff_flag | policy.cliffside_flag) & !known != 0 {
                return Err("Cliff topology roles differ from the live object source".into());
            }
            let n = stage.layers.surface_resolution;
            let classification_work = maximum_work.saturating_sub(n * n);
            let Some((cells, report)) = crate::cliff_topology::apply(
                self.view(),
                policy,
                keep_existing,
                classification_work,
                &cancelled,
            )?
            else {
                return Ok(None);
            };
            let Some(index) = crate::topology_query::Index::new(&cells, n, &cancelled)? else {
                return Ok(None);
            };
            if cancelled() {
                return Ok(None);
            }
            self.inner
                .stage
                .topology
                .as_mut()
                .ok_or("Missing live topology")?
                .cells = cells;
            self.inner.stage.topology_index = Some(index);
            Ok(Some(report))
        })();
        match &result {
            Ok(None) => self.cancel(),
            Err(_) => self.invalidate(),
            Ok(Some(_)) => {}
        }
        result
    }
    pub fn set_height_reservations(
        &mut self,
        reservations: &'a crate::height_reservations::HeightReservations,
        cancelled: impl Fn() -> bool,
    ) -> Result<bool, String> {
        if !matches!(self.state, RoadState::Ready)
            || !self.seen.is_empty()
            || self.next_object_pose.is_some_and(|id| id != 0)
        {
            self.state = RoadState::Failed;
            return Err("Road height reservations must precede object admission".into());
        }
        match reservations.validate_binding(self.inner.stage.layers, &cancelled) {
            Ok(false) => {
                self.cancel();
                return Ok(false);
            }
            Err(error) => {
                self.state = RoadState::Failed;
                return Err(error);
            }
            Ok(true) => {}
        }
        if self
            .inner
            .stage
            .visit(self.inner.stage.layers.height.len())
            .is_err()
        {
            self.state = RoadState::Failed;
            return Err("Road height reservation validation exhausted object work".into());
        }
        self.inner.stage.height_reservations = Some(reservations);
        self.inner.report.height_reserved_samples = Some(reservations.samples());
        Ok(true)
    }
    /// Change only admission policy, retaining all grids, reservations and work.
    /// Namespace keys distinguish source adapters for the same game prefab.
    pub fn set_scope(&mut self, scope: ProtectionScope) -> Result<(), String> {
        if self.inner.stage.connected_reserved.is_some() {
            self.invalidate();
            return Err("Cannot change terrain scope inside a connected prefab group".into());
        }
        let result = (|| {
            if !matches!(self.state, RoadState::Ready)
                || self.inner.report.engine != OBJECT_VERSION
                || scope.namespace.is_empty()
                || scope.namespace.len() > 32
                || !scope
                    .namespace
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                || scope.protected_topology == 0
                || scope.protected_topology & !self.inner.stage.baseline_protected != 0
                || scope.role_exceptions & !scope.protected_topology != 0
                || scope.material_role_exceptions & !scope.role_exceptions != 0
                || scope.protected_topology & !scope.role_exceptions == 0
                || scope
                    .maximum_height_change
                    .is_some_and(|v| !v.is_finite() || !(0. ..=24.).contains(&v))
            {
                return Err("Invalid live object protection scope".into());
            }
            let scopes = self
                .inner
                .report
                .protection_scopes
                .get_or_insert_with(Vec::new);
            let index = if let Some(i) = scopes.iter().position(|s| s == &scope) {
                i
            } else {
                if scopes.len() == 128 {
                    return Err("Too many live object protection scopes".into());
                }
                scopes.push(scope.clone());
                scopes.len() - 1
            };
            self.inner.stage.scope_index = Some(index);
            self.inner.stage.scope_protected = Some(scope.protected_topology);
            self.inner.stage.scope_height_limit = scope.maximum_height_change;
            self.role_exceptions = scope.role_exceptions;
            self.material_role_exceptions = scope.material_role_exceptions;
            if self.namespace.as_deref() != Some(&scope.namespace) {
                let prefix = format!("{}:", scope.namespace);
                self.bindings = Some(
                    self.prefabs
                        .keys()
                        .filter_map(|key| {
                            key.strip_prefix(&prefix)
                                .map(|path| (path.to_owned(), key.clone()))
                        })
                        .collect(),
                );
                self.namespace = Some(scope.namespace);
            }
            Ok(())
        })();
        if result.is_err() {
            self.state = RoadState::Failed;
        }
        result
    }
    /// Seed the separate authored blocker raster before any admission. The
    /// caller is responsible for the provenance/completeness of earlier phases.
    pub fn initialize_blocking(
        &mut self,
        raster: &[u8],
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<()>, String> {
        if !matches!(self.state, RoadState::Ready)
            || !self.seen.is_empty()
            || self.next_object_pose.is_some_and(|next| next != 0)
            || self.inner.report.engine != OBJECT_VERSION
            || raster.len() != self.inner.stage.alpha.len()
        {
            self.state = RoadState::Failed;
            return Err("Invalid initial live object blocker raster".into());
        }
        let mut copy = Vec::with_capacity(raster.len());
        let mut count = 0;
        for chunk in raster.chunks(65536) {
            if cancelled() {
                self.state = RoadState::Cancelled;
                return Ok(None);
            }
            if chunk.iter().any(|v| *v > 1) {
                self.state = RoadState::Failed;
                return Err("Invalid initial blocker cell".into());
            }
            count += chunk.iter().filter(|v| **v == 1).count();
            copy.extend_from_slice(chunk);
        }
        self.inner.stage.blocking = Some(copy);
        self.inner
            .report
            .blocking
            .as_mut()
            .ok_or("Missing blocker report")?
            .initial_cells = Some(count);
        Ok(Some(()))
    }
    fn binding_key<'s>(
        bindings: &'s Option<BTreeMap<String, String>>,
        path: &'s str,
    ) -> Option<&'s str> {
        match bindings {
            None => Some(path),
            Some(map) => map.get(path).map(String::as_str),
        }
    }
    /// Shared terrain admission work consumed, including rejected placements.
    pub fn sample_visits(&self) -> usize {
        self.inner.stage.visits
    }
    pub fn applied(&self) -> &[Applied] { &self.inner.report.applied }
    /// A guaranteed rejection at an authored root support. Call only when the
    /// fitting transforms cannot move X/Z; no reservations or IDs are consumed.
    pub fn origin_blocked(
        &self,
        placement: &decor::Stage,
        path: &str,
        position: [f32; 3],
    ) -> Result<bool, String> {
        Ok(self
            .origin_check(placement, path, None)?
            .is_some_and(|(check, _)| self.origin_conflicts(position, check)))
    }

    /// Resolve invariant support coverage once. The caller proves that fitting
    /// keeps the origin fixed on X/Z. Cliff callers can also check the
    /// transformed scale when the maximum possible scale does not prove coverage.
    pub(crate) fn origin_check(
        &self,
        placement: &decor::Stage,
        path: &str,
        cliff_scale: Option<CliffScaleBounds>,
    ) -> Result<Option<(OriginCheck, Option<f64>)>, String> {
        if !matches!(self.state, RoadState::Ready) {
            return Err("Object transaction is not ready for fitting".into());
        }
        let prefab = self
            .prefabs
            .get(Self::binding_key(&self.bindings, path).ok_or("Unknown object adapter binding")?)
            .ok_or("Unknown object support prefab")?;
        let stage = &self.inner.stage;
        let cell_radius = stage.layers.size as f32 / stage.layers.surface_resolution as f32
            * 0.5 * 2_f32.sqrt();
        let mut coverage = cliff_scale.map_or(0., |scale| {
            scale.origin_coverage(prefab, cell_radius, stage.blocking.is_some())
        });
        if prefab.support.iter().any(|s| s.position == [0.; 3]) {
            coverage = coverage.max(self.origin_height_margin() + cell_radius);
        }
        let scale_limit = cliff_scale.and_then(|_| {
            let height_margin = self.origin_height_margin();
            prefab.support.iter().filter_map(|support| {
                let [x, y, z] = support.position.map(f64::from);
                // A unit quaternion preserves length. Allow 1% for the
                // validated quaternion tolerance and rotation arithmetic, and
                // 2 cm for world-coordinate and raster rounding.
                let distance = x.hypot(y).hypot(z) * 1.01;
                let coverage = f64::from(support.radius * 2_f32.sqrt() + height_margin) - 0.02;
                (distance > 0. && coverage > 0.).then(|| coverage / distance)
            }).max_by(f64::total_cmp)
        });
        if coverage == 0. && scale_limit.is_none() {
            return Ok(None);
        }
        let check = OriginCheck {
            exception: role_exception(
                prefab,
                placement,
                self.role_exceptions,
                self.material_role_exceptions,
            ),
            coverage,
        };
        let needs_scale = scale_limit.filter(|&limit| {
            coverage == 0.
                && cliff_scale.is_some_and(|scale| f64::from(scale.largest_axis) > limit)
        });
        Ok(Some((check, needs_scale)))
    }

    fn origin_height_margin(&self) -> f32 {
        let layers = self.inner.stage.layers;
        layers.size as f32 / (layers.height_resolution - 1) as f32 * 2. * 2_f32.sqrt()
    }

    pub(crate) fn origin_conflicts(&self, position: [f32; 3], check: OriginCheck) -> bool {
        check.conflicts(self.probe_origin(position, check, check.scans_neighbors()))
    }

    fn probe_origin(
        &self, position: [f32; 3], check: OriginCheck, neighbors: bool,
    ) -> OriginProbe {
        let stage = &self.inner.stage;
        let size = stage.layers.size as f32;
        if !finite(&position) {
            return OriginProbe { blocked: false, neighbor_distance: f32::INFINITY };
        }
        let n = stage.layers.surface_resolution;
        let x = index(n, size, position[0]);
        let z = index(n, size, position[2]);
        self.probe_origin_cell([position[0], position[2]], [x, z], check, neighbors)
    }

    /// Probe the surface cell already computed for a finite, fixed X/Z origin.
    pub(crate) fn probe_origin_cell(
        &self, [px, pz]: [f32; 2], [x, z]: [usize; 2], check: OriginCheck, neighbors: bool,
    ) -> OriginProbe {
        let stage = &self.inner.stage;
        let size = stage.layers.size as f32;
        // Normalization can round an origin just beyond the edge into the last cell.
        if px.abs() > size * 0.5 || pz.abs() > size * 0.5 {
            return OriginProbe { blocked: false, neighbor_distance: f32::INFINITY };
        }
        let guard = CellGuard {
            alpha: stage.alpha,
            occupancy: &stage.occupancy,
            connected_reserved: stage.connected_reserved.as_deref(),
            original: &stage.layers.topology,
            current: stage.topology.as_ref().map(|s| s.cells.as_slice()),
            protected: stage.scope_protected.unwrap_or(stage.baseline_protected),
            exception: check.exception,
            dynamic: stage.scope_protected.is_some(),
        };
        let n = stage.layers.surface_resolution;
        let center = z * n + x;
        let mut result = OriginProbe {
            blocked: guard.check(center).is_err(),
            neighbor_distance: f32::INFINITY,
        };
        if result.blocked || !neighbors {
            return result;
        }
        // A single distance serves all trial prefabs with the same protected
        // cell policy until an admission changes the live terrain.
        for row in z.saturating_sub(1)..=(z + 1).min(n - 1) {
            for column in x.saturating_sub(1)..=(x + 1).min(n - 1) {
                let cell = row * n + column;
                if cell != center && guard.check(cell).is_err() {
                    let wx = ((column as f32 + 0.5) / n as f32 - 0.5) * size;
                    let wz = ((row as f32 + 0.5) / n as f32 - 0.5) * size;
                    result.neighbor_distance = result.neighbor_distance.min(
                        (wx - px).hypot(wz - pz),
                    );
                }
            }
        }
        result
    }
    pub fn new_objects_with_roles(
        layers: &'a Layers,
        alpha: &'a [u8],
        prefabs: BTreeMap<String, Prefab>,
        known_mask: u32,
        protected_topology: u32,
        role_exceptions: u32,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        if role_exceptions & !protected_topology != 0 || protected_topology & !role_exceptions == 0
        {
            return Err("Invalid object placement role exceptions".into());
        }
        Ok(Self::new_objects(
            layers,
            alpha,
            prefabs,
            known_mask,
            protected_topology,
            cancelled,
        )?
        .map(|mut transaction| {
            transaction.role_exceptions = role_exceptions;
            transaction.inner.report.permitted_role_exceptions = Some(role_exceptions);
            transaction
        }))
    }
    pub fn new_objects_with_policy(
        layers: &'a Layers,
        alpha: &'a [u8],
        prefabs: BTreeMap<String, Prefab>,
        known_mask: u32,
        protected_topology: u32,
        policy: ObjectPolicy,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        policy.validate()?;
        Ok(Self::new_objects_with_roles(
            layers,
            alpha,
            prefabs,
            known_mask,
            protected_topology,
            policy.role_exceptions,
            cancelled,
        )?
        .map(|mut transaction| {
            transaction.inner.stage.visit_limit = policy.maximum_work;
            transaction.inner.report.maximum_work = Some(policy.maximum_work);
            transaction.material_role_exceptions = policy.material_role_exceptions;
            transaction.inner.report.permitted_material_role_exceptions =
                Some(policy.material_role_exceptions);
            transaction
        }))
    }
    pub fn new_objects(
        layers: &'a Layers,
        alpha: &'a [u8],
        prefabs: BTreeMap<String, Prefab>,
        known_mask: u32,
        protected_topology: u32,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        let Some(mut transaction) = Self::new(
            layers,
            alpha,
            prefabs,
            known_mask,
            protected_topology,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        let Some(index) = crate::topology_query::Index::new(
            &transaction
                .inner
                .stage
                .topology
                .as_ref()
                .ok_or("Missing live topology")?
                .cells,
            layers.surface_resolution,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        transaction.inner.report.engine = OBJECT_VERSION;
        transaction.next_object_pose = Some(0);
        transaction.inner.stage.checked_height = true;
        transaction.inner.stage.topology_index = Some(index);
        Ok(Some(transaction))
    }
    pub(crate) fn last_rejection(&self) -> Option<&'static str> {
        self.inner.last_rejection.map(|r| match r {
            Reject::Unsupported => "unsupported",
            Reject::Occupied => "occupied",
            Reject::Protected => "protected",
            Reject::Holes => "holes",
            Reject::Outside => "outside",
            Reject::Tilt => "tilt",
            Reject::Fill => "fill",
            Reject::Budget => "budget",
            Reject::TopologyFilter => "topology_filter",
            Reject::TopologyRead => "topology_read",
            Reject::MaterialFilter => "material_filter",
            Reject::MaterialRead => "material_read",
            Reject::PlacementBlocked => "placement_blocked",
        })
    }
    /// Connected prefab pieces may share their own terrain reservations. Earlier
    /// objects, protected topology, holes and authored placement queries stay guarded.
    pub(crate) fn begin_connected_group(&mut self) -> Result<(), String> {
        if !matches!(self.state, RoadState::Ready)
            || self.inner.revision < 11
            || self.inner.stage.connected_reserved.is_some()
        {
            self.invalidate();
            return Err("Invalid connected prefab group".into());
        }
        let cells = self.inner.stage.occupancy.len();
        self.inner.stage.connected_reserved = Some(vec![0; cells.div_ceil(64)]);
        if let Some(topology) = &mut self.inner.stage.topology {
            topology.begin_group();
        }
        Ok(())
    }
    pub(crate) fn end_connected_group(&mut self) -> Result<(), String> {
        if !matches!(self.state, RoadState::Ready)
            || self.inner.stage.connected_reserved.take().is_none()
        {
            self.invalidate();
            return Err("Connected prefab group cannot finish".into());
        }
        if let Some(topology) = &mut self.inner.stage.topology {
            topology.end_group();
        }
        Ok(())
    }
    pub fn cancel(&mut self) {
        if matches!(self.state, RoadState::Ready) {
            self.state = RoadState::Cancelled;
        }
    }
    pub fn invalidate(&mut self) {
        self.state = RoadState::Failed;
    }
    pub fn new(
        layers: &'a Layers,
        alpha: &'a [u8],
        prefabs: BTreeMap<String, Prefab>,
        known_mask: u32,
        protected_topology: u32,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        if known_mask == 0 || protected_topology == 0 || protected_topology & !known_mask != 0 {
            return Err("Road-object transaction requires bound topology roles".into());
        }
        let input = Input {
            height_reservations: None,
            layers,
            alpha,
            candidates: &[],
            stages: &[],
            poses: &[],
            prefabs: &prefabs,
            obstacles: &[],
            protected_topology,
        };
        let Some(inner) = begin_revision(input, 11, known_mask, cancelled)? else {
            return Ok(None);
        };
        Ok(Some(Self {
            inner,
            prefabs,
            seen: BTreeSet::new(),
            next_object_pose: None,
            state: RoadState::Ready,
            role_exceptions: 0,
            material_role_exceptions: 0,
            namespace: None,
            bindings: None,
        }))
    }
    pub fn view(&self) -> crate::native_layers::View<'_> {
        let stage = &self.inner.stage;
        crate::native_layers::View {
            height: &stage.height,
            topology: &stage.topology.as_ref().expect("Road topology state").cells,
            splat: &stage.materials.as_ref().expect("Road material state").cells,
            placement_blockers: stage
                .checked_height
                .then_some(stage.blocking.as_deref())
                .flatten(),
            topology_tiles: stage.topology_index.as_ref().map(|index| {
                index.view(&stage.topology.as_ref().expect("Road topology state").cells)
            }),
            ..stage.layers.view()
        }
    }
    pub(crate) fn topology_sample_matches(&self, cell: usize, filter: &crate::spawn::Filter) -> bool {
        let stage = &self.inner.stage;
        let topology = &stage.topology.as_ref().expect("Road topology state").cells;
        // A transaction validates its private rasters before admitting any edits.
        (!stage.checked_height || stage.blocking.as_ref().is_none_or(|cells| cells[cell] == 0))
            && filter.topology_matches(topology[cell] as u32)
    }

    /// Live object transactions require IDs starting at zero and increasing by
    /// one on every attempt, including rejection. Their caller bounds sampling
    /// and this transaction retains terrain-work limits across all stages.
    /// Legacy road-only transactions retain their bounded arbitrary-ID set.
    pub fn try_apply(
        &mut self,
        pose_index: usize,
        placement: &decor::Stage,
        path: &str,
        pose: &decor_fit::Pose,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<bool>, String> {
        self.try_apply_inner(pose_index, placement, path, pose, true, cancelled)
    }

    /// Static stages have already passed the stricter roster and placement
    /// checks. Resolve the prefab by its validated roster index.
    pub(crate) fn try_apply_validated(
        &mut self,
        pose_index: usize,
        placement: &decor_fit::ValidatedStage<'_>,
        prefab: usize,
        pose: &decor_fit::Pose,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<bool>, String> {
        let stage = placement.stage();
        let Some(path) = stage.prefabs.get(prefab) else {
            self.invalidate();
            return Err("Object prefab index is outside its validated stage".into());
        };
        self.try_apply_inner(pose_index, stage, path, pose, false, cancelled)
    }

    fn try_apply_inner(
        &mut self,
        pose_index: usize,
        placement: &decor::Stage,
        path: &str,
        pose: &decor_fit::Pose,
        validate_stage: bool,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<bool>, String> {
        match self.state {
            RoadState::Cancelled => return Ok(None),
            RoadState::Failed => {
                return Err("Road-object transaction cannot continue after failure".into());
            }
            RoadState::Ready => {}
        }
        let result = (|| {
            if cancelled() {
                return Ok(None);
            }
            if validate_stage {
                placement.validate_path_attached()?;
            }
            if pose.candidate != pose_index
                || !finite(&pose.position)
                || !finite(&pose.rotation)
                || !finite(&pose.scale)
                || pose.scale.iter().any(|v| *v <= 0. || *v > 100.)
                || (pose.rotation.iter().map(|v| v * v).sum::<f32>() - 1.).abs() > 0.001
                || (validate_stage && !placement.prefabs.iter().any(|p| p == path))
            {
                return Err(format!(
                    "Invalid incremental road-object pose or source rule: index={pose_index}, candidate={}, position={:?}, rotation={:?}, scale={:?}, prefab={path}",
                    pose.candidate, pose.position, pose.rotation, pose.scale
                ));
            }
            if let Some(next) = &mut self.next_object_pose {
                if pose_index != *next {
                    return Err("Live object pose IDs must be contiguous and unique".into());
                }
                *next = next.checked_add(1).ok_or("Live object pose ID overflow")?;
            } else if pose_index >= 100000 || !self.seen.insert(pose_index) {
                return Err("Invalid or duplicate incremental road-object pose ID".into());
            }
            let binding =
                Self::binding_key(&self.bindings, path).ok_or("Unknown object adapter binding")?;
            let prefab = self
                .prefabs
                .get(binding)
                .ok_or("Unknown incremental road-object prefab")?;
            self.inner.stage.active_exception = role_exception(
                prefab,
                placement,
                self.role_exceptions,
                self.material_role_exceptions,
            );
            let result = self
                .inner
                .apply_pose(pose_index, placement, binding, prefab, pose, &cancelled);
            self.inner.stage.active_exception = 0;
            let result = result?;
            if self.inner.report.budget_exhausted {
                return Err(format!(
                    "Object terrain transaction exhausted its work allowance after {} visits (limit {}), at pose {pose_index}, stage {}, prefab {binding}",
                    self.inner.stage.visits, self.inner.stage.visit_limit, placement.id
                ));
            }
            Ok(result)
        })();
        match &result {
            Ok(None) => self.state = RoadState::Cancelled,
            Err(_) => self.state = RoadState::Failed,
            _ => {}
        }
        result
    }
    pub fn finish(self, cancelled: impl Fn() -> bool) -> Result<Option<Output>, String> {
        match self.state {
            RoadState::Cancelled => Ok(None),
            RoadState::Failed => Err("Road-object transaction cannot publish after failure".into()),
            RoadState::Ready if self.inner.stage.connected_reserved.is_some() => {
                Err("Connected prefab group remains unfinished".into())
            }
            RoadState::Ready => self.inner.finish(cancelled),
        }
    }
}
impl Transaction<'_> {
    fn apply_pose(
        &mut self,
        i: usize,
        placement: &decor::Stage,
        path: &str,
        prefab: &Prefab,
        p: &decor_fit::Pose,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<bool>, String> {
        if cancelled() {
            return Ok(None);
        }
        self.last_rejection = None;
        let accepted = self.report.counts.accepted;
        let report = &mut self.report;
        let stage = &mut self.stage;
        let revision = self.revision;
        let blocking = revision >= 4;
        let row = if let Some(row) = report.prefabs.get_mut(path) {
            row
        } else {
            report
                .prefabs
                .entry(path.to_owned())
                .or_insert_with(|| PrefabReport {
                    counts: Counts::default(),
                    sample_visits: 0,
                    unsupported: prefab.unsupported.clone(),
                    pending: prefab.pending.clone(),
                })
        };
        report.counts.attempted += 1;
        row.counts.attempted += 1;
        let work_before = stage.visits;
        let result = if !prefab.unsupported.is_empty()
            || (revision < 11
                && prefab.footprint.is_none()
                && (!blocking || prefab.blockers.is_empty())
                && (revision < 8 || prefab.materials.as_ref().is_none_or(|m| m.is_empty()))
                && (revision < 9
                    || prefab
                        .height_modifiers
                        .as_ref()
                        .is_none_or(|m| m.is_empty()))
                && (revision < 10 || prefab.contributors.as_ref().is_none_or(|c| c.is_empty())))
        {
            Err(Reject::Unsupported)
        } else {
            stage.prepare(prefab, p, &cancelled, placement, &mut self.geometry)
        };
        if stage.checked_height {
            row.sample_visits += stage.visits - work_before;
        }
        match result {
            Ok(None) => return Ok(None),
            Ok(Some(patch)) => {
                if let Some(edits) = &mut self.cliff_edits {
                    let n = stage.layers.surface_resolution;
                    let hr = stage.layers.height_resolution;
                    for &(cell, _) in &patch.writes {
                        let x = (cell % hr * n / (hr - 1)).min(n - 1);
                        let z = (cell / hr * n / (hr - 1)).min(n - 1);
                        edits.mark(x, z);
                    }
                    let cells = patch
                        .blocked
                        .iter()
                        .copied()
                        .chain(
                            patch
                                .topology
                                .iter()
                                .flat_map(|plan| plan.writes.iter().map(|&(cell, _)| cell)),
                        )
                        .chain(
                            patch
                                .materials
                                .iter()
                                .flat_map(|plan| plan.writes.iter().map(|&(cell, _)| cell)),
                        );
                    for cell in cells {
                        edits.mark(cell % n, cell / n);
                    }
                }
                if let (Some(state), Some(plan)) = (&mut stage.topology, patch.topology) {
                    state.commit(plan);
                    if let (Some(index), Some(dirty)) =
                        (&mut stage.topology_index, &patch.topology_dirty)
                    {
                        index.refresh(&state.cells, dirty);
                    }
                }
                if let (Some(state), Some(plan)) = (&mut stage.materials, patch.materials) {
                    state.commit(plan);
                }
                if let (Some(state), Some(stats)) =
                    (&mut stage.height_effects, patch.height_effects)
                {
                    state.commit(stats);
                }
                if let (Some(state), Some(stats)) =
                    (&mut stage.collider_effects, patch.collider_effects)
                {
                    state.commit(stats);
                }
                report.counts.accepted += 1;
                row.counts.accepted += 1;
                let h = patch.writes.len();
                let r = patch.reserved.len();
                let blocked_cells = patch.blocked.len();
                if let (Some(raster), Some(stats)) = (&mut stage.blocking, &mut report.blocking) {
                    if !prefab.blockers.is_empty() {
                        stats.instances += 1;
                    }
                    stats.cells += blocked_cells;
                    for i in patch.blocked {
                        raster[i] = 1;
                    }
                }
                for (i, h) in patch.writes {
                    stage.height[i] = h;
                }
                for i in patch.reserved {
                    stage.occupancy[i] = 1;
                    if let Some(bits) = &mut stage.connected_reserved {
                        bits[i / 64] |= 1 << (i % 64);
                    }
                }
                report.height_writes += h;
                report.reserved_cells += r;
                report.applied.push(Applied {
                    pose: i,
                    height_writes: h,
                    reserved_cells: r,
                    maximum_fill: patch.maximum_fill,
                    blocked_cells: blocking.then_some(blocked_cells),
                    protection_exception: (stage.active_exception != 0)
                        .then_some(stage.active_exception),
                    protection_scope: stage.scope_index,
                });
            }
            Err(r) => {
                self.last_rejection = Some(r);
                if let Some(state) = &mut stage.topology {
                    if matches!(r, Reject::TopologyFilter) {
                        state.report.filter_rejections += 1;
                    }
                    if matches!(r, Reject::TopologyRead) {
                        state.report.read_conflicts += 1;
                    }
                }
                if let Some(state) = &mut stage.materials {
                    if matches!(r, Reject::MaterialFilter) {
                        state.report.filter_rejections += 1;
                    }
                    if matches!(r, Reject::MaterialRead) {
                        state.report.read_conflicts += 1;
                    }
                }
                count(&mut report.counts, r);
                count(&mut row.counts, r);
                if matches!(r, Reject::Budget) {
                    report.budget_exhausted = true;
                }
            }
        }

        Ok(Some(report.counts.accepted > accepted))
    }
    fn finish(self, cancelled: impl Fn() -> bool) -> Result<Option<Output>, String> {
        let Self {
            stage, mut report, ..
        } = self;
        if cancelled() {
            return Ok(None);
        }
        if let (Some(raster), Some(stats)) = (stage.blocking, &mut report.blocking) {
            stats.raster = raster;
        }
        let topology = stage.topology.map(|state| {
            report.topology = Some(state.report);
            state.cells
        });
        let splat = stage.materials.map(|state| {
            report.materials = Some(state.report);
            state.cells
        });
        report.height_effects = stage.height_effects;
        report.collider_effects = stage.collider_effects;
        report.sample_visits = stage.visits;
        report.occupancy = stage.occupancy;
        Ok(Some(Output {
            height: stage.height,
            topology,
            splat,
            report,
        }))
    }
}
fn begin_revision<'a>(
    input: Input<'a, '_>,
    revision: u8,
    known_mask: u32,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Transaction<'a>>, String> {
    let blocking = revision >= 4;
    let Input {
        height_reservations,
        layers,
        alpha,
        candidates,
        stages,
        poses,
        prefabs,
        obstacles,
        protected_topology,
    } = input;
    let (hr, n) = dimensions(layers.size)?;
    let cells = n * n;
    if layers.height_resolution != hr
        || layers.surface_resolution != n
        || layers.height.len() != hr * hr
        || layers.topology.len() != cells
        || alpha.len() != cells
        || poses.len() > 100000
        || candidates.len() > 100000
        || stages.len() > 128
        || prefabs.len() > 4096
        || obstacles.len() > 128
        || protected_topology == 0
        || layers.height.iter().any(|h| !(0..=32766).contains(h))
    {
        return Err("Invalid footprint input dimensions or policy".into());
    }
    if let Some(reservations) = height_reservations
        && !reservations.validate_binding(layers, &cancelled)?
    {
        return Ok(None);
    }
    if revision >= 11 {
        for stage in stages {
            stage.validate_path_attached()?;
        }
    }
    for p in prefabs.values() {
        if revision >= 5 {
            crate::decor_topology::validate(
                p.topology
                    .as_ref()
                    .ok_or("Missing topology dependency metadata")?,
                known_mask,
                protected_topology,
            )?;
        }
        if revision >= 8 {
            crate::decor_material::validate(
                p.materials
                    .as_ref()
                    .ok_or("Missing material dependency metadata")?,
            )?;
        }
        if revision >= 9 {
            if !p.raises.is_empty() {
                return Err("Height revision requires explicit ordered modifiers".into());
            }
            crate::decor_height::validate(
                p.height_modifiers
                    .as_ref()
                    .ok_or("Missing height modifier metadata")?,
            )?;
            if revision < 11
                && p.height_modifiers.as_ref().is_some_and(|v| {
                    v.iter()
                        .any(|m| matches!(m.mode, crate::decor_height::Mode::Add { .. }))
                })
            {
                return Err(
                    "Additive height operations require the road-object effect revision".into(),
                );
            }
        }
        if revision >= 10 {
            crate::decor_collider::validate(
                p.contributors
                    .as_ref()
                    .ok_or("Missing collider contributor metadata")?,
            )?;
        }
        if let Some(f) = &p.footprint {
            f.validate()?;
        }
        if p.raises.len() > 4096
            || p.raises.iter().any(|r| {
                !finite(&r.position)
                    || !r.radius.is_finite()
                    || !r.fade.is_finite()
                    || !r.opacity.is_finite()
                    || !(0. ..=128.).contains(&r.radius)
                    || !(0. ..=128.).contains(&r.fade)
                    || !(0. ..=1.).contains(&r.opacity)
            })
        {
            return Err("Invalid decoration height raise".into());
        }
        if p.blockers.len() > 4096
            || p.blockers.iter().any(|b| {
                !finite(&b.position)
                    || !b.radius.is_finite()
                    || !b.fade.is_finite()
                    || !(0. ..=128.).contains(&b.radius)
                    || !(0. ..=128.).contains(&b.fade)
            })
        {
            return Err("Invalid decoration blocker".into());
        }
        if p.support.len() > 4096
            || p.support.iter().any(|s| {
                !finite(&s.position) || !s.radius.is_finite() || !(0. ..=16.).contains(&s.radius)
            })
        {
            return Err("Invalid footprint support reservation".into());
        }
    }
    let mut seen = BTreeSet::new();
    for p in poses {
        if !seen.insert(p.candidate)
            || !finite(&p.position)
            || !finite(&p.rotation)
            || !finite(&p.scale)
            || p.scale.iter().any(|v| *v <= 0. || *v > 100.)
            || (p.rotation.iter().map(|v| v * v).sum::<f32>() - 1.).abs() > 0.001
            || candidates
                .get(p.candidate)
                .and_then(|c| stages.get(c.stage).and_then(|s| s.prefabs.get(c.prefab)))
                .is_none_or(|path| !prefabs.contains_key(path))
        {
            return Err("Invalid footprint pose or prefab reference".into());
        }
    }
    for o in obstacles {
        if !finite(&o.center)
            || !finite(&o.extent)
            || o.extent.iter().any(|v| *v < 0.)
            || !o.yaw.is_finite()
        {
            return Err("Invalid protected footprint obstacle".into());
        }
    }
    let prepared_obstacles: Vec<_> = obstacles
        .iter()
        .map(|o| {
            let (s, c) = o.yaw.to_radians().sin_cos();
            (o.center, o.extent, s, c)
        })
        .collect();
    let mut stage = Stage {
        support_marks: Vec::new(),
        connected_reserved: None,
        height_reservations,
        support_only: revision >= 11,
        baseline_protected: protected_topology,
        active_exception: 0,
        checked_height: false,
        visit_limit: MAX_VISITS,
        topology_index: None,
        scope_index: None,
        scope_protected: None,
        scope_height_limit: None,
        layers,
        alpha,
        height: layers.height.clone(),
        occupancy: vec![0; cells],
        blocking: blocking.then(|| vec![0; cells]),
        topology: (revision >= 5)
            .then(|| crate::decor_topology::State::new(&layers.topology, known_mask)),
        materials: if revision >= 8 {
            Some(crate::decor_material::State::new(layers)?)
        } else {
            None
        },
        height_effects: (revision >= 9).then(crate::decor_height::Report::default),
        collider_effects: (revision >= 10).then(crate::decor_collider::Report::default),
        visits: if height_reservations.is_some() {
            layers.height.len()
        } else {
            0
        },
    };
    let size = layers.size as f32;
    let diag = size / n as f32 * 0.5 * 2_f32.sqrt();
    for z in 0..n {
        if cancelled() {
            return Ok(None);
        }
        for x in 0..n {
            let i = z * n + x;
            if layers.topology[i] as u32 & protected_topology != 0 {
                stage.occupancy[i] = 2;
            }
        }
    }
    for (center, extent, s, c) in prepared_obstacles {
        let ex = extent[0] + diag;
        let ez = extent[1] + diag;
        let bx = c.abs() * ex + s.abs() * ez;
        let bz = s.abs() * ex + c.abs() * ez;
        for z in index(n, size, center[1] - bz)..=index(n, size, center[1] + bz) {
            if cancelled() {
                return Ok(None);
            }
            for x in index(n, size, center[0] - bx)..=index(n, size, center[0] + bx) {
                let wx = ((x as f32 + 0.5) / n as f32 - 0.5) * size;
                let wz = ((z as f32 + 0.5) / n as f32 - 0.5) * size;
                let dx = wx - center[0];
                let dz = wz - center[1];
                if (c * dx + s * dz).abs() <= ex && (-s * dx + c * dz).abs() <= ez {
                    stage.occupancy[z * n + x] = 2;
                }
            }
        }
    }
    let report = Report {
        engine: if revision == 11 {
            ROAD_OBJECT_VERSION
        } else if revision == 10 {
            crate::decor_collider::VERSION
        } else if revision == 9 {
            crate::decor_height::VERSION
        } else if revision == 8 {
            crate::decor_material::VERSION
        } else if revision == 5 {
            crate::decor_topology::VERSION
        } else if blocking {
            BLOCKER_VERSION
        } else {
            VERSION
        },
        resolution: n,
        protected_topology,
        permitted_role_exceptions: None,
        permitted_material_role_exceptions: None,
        counts: Counts::default(),
        prefabs: BTreeMap::new(),
        applied: Vec::new(),
        height_writes: 0,
        reserved_cells: 0,
        initial_protected_cells: stage.occupancy.iter().filter(|v| **v == 2).count(),
        sample_visits: 0,
        maximum_work: None,
        protection_scopes: None,
        unprocessed_poses: 0,
        budget_exhausted: false,
        physics_validated: false,
        blocking: blocking.then(|| BlockingReport {
            instances: 0,
            cells: 0,
            initial_cells: None,
            raster: vec![],
        }),
        topology: None,
        materials: None,
        height_effects: None,
        collider_effects: None,
        height_reserved_samples: height_reservations.map(|mask| mask.samples()),
        occupancy: vec![],
    };
    Ok(Some(Transaction {
        stage,
        report,
        revision,
        last_rejection: None,
        geometry: GeometryScratch::default(),
        cliff_edits: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cliff_batch_write_masks_preserve_rectangles_and_first_write_order() {
        use crate::topology_query::BLOCK;
        let mut edits = CliffEdits {
            side: 128,
            rows: vec![0; 128],
            dirty: Vec::new(),
        };
        let marks = [[127, 127], [63, 64], [64, 64], [0, 0], [64, 64]];
        for [x, z] in marks {
            edits.mark(x * BLOCK + BLOCK - 1, z * BLOCK + BLOCK - 1);
        }
        assert_eq!(edits.dirty, [16_383, 8_255, 8_256, 0]);
        let edges = [0, 1, 62, 63, 64, 65, 126, 127];
        for left in edges {
            for right in edges.into_iter().filter(|&right| right >= left) {
                for top in edges {
                    for bottom in edges.into_iter().filter(|&bottom| bottom >= top) {
                        let expected = marks.iter().any(|&[x, z]| {
                            (left..=right).contains(&x) && (top..=bottom).contains(&z)
                        });
                        assert_eq!(edits.intersects(left, right, top, bottom), expected);
                    }
                }
            }
        }
        edits.clear();
        assert!(edits.dirty.is_empty());
        assert!(!edits.intersects(0, 127, 0, 127));
        edits.mark(64 * BLOCK, 64 * BLOCK);
        assert_eq!(edits.dirty, [8_256]);
    }
    fn cliff_policy() -> crate::cliff_topology::Policy {
        crate::cliff_topology::Policy {
            cliff_flag: 2,
            cliffside_flag: 4,
            protected_topology: 8,
            rock_channel: 3,
            slope_cutoff: 30.,
            rock_cutoff: 0.4,
            radius_cells: 1,
        }
    }
    #[test]
    fn procedural_cliffs_read_live_materials_and_rebind_the_topology_index() {
        let layers = material_layers();
        let n = layers.surface_resolution;
        let cells = n * n;
        let alpha = vec![255; cells];
        let mut transaction = RoadTransaction::new_objects_with_policy(
            &layers,
            &alpha,
            BTreeMap::new(),
            15,
            8,
            ObjectPolicy {
                maximum_work: 1,
                role_exceptions: 0,
                material_role_exceptions: 0,
            },
            || false,
        )
        .unwrap()
        .unwrap();
        transaction.inner.stage.materials.as_mut().unwrap().cells[3 * cells..4 * cells].fill(255);
        transaction.inner.stage.occupancy[0] = 1;
        transaction.inner.stage.blocking.as_mut().unwrap()[0] = 1;
        transaction
            .inner
            .stage
            .topology
            .as_mut()
            .unwrap()
            .commit(crate::decor_topology::Plan {
                writes: vec![],
                reads: vec![0],
                operations: 0,
            });
        assert!(transaction.inner.stage.visit(1).is_ok());
        let visits = transaction.sample_visits();
        let height = transaction.view().height.to_vec();
        let report = transaction
            .apply_cliff_topology(cliff_policy(), false, 50_000_000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(report.added_cliff_cells, cells);
        assert_eq!(transaction.sample_visits(), visits);
        assert!(transaction.inner.stage.visit(1).is_err());
        assert_eq!(transaction.view().height, height);
        assert!(transaction.view().topology.iter().all(|v| *v == 2));
        let view = transaction.view();
        let tiles = view.topology_tiles.unwrap();
        assert!(tiles.matches(view.topology, n));
        let mut work = 0;
        assert_eq!(
            tiles
                .union(
                    crate::topology_query::Query {
                        rect: [0, n - 1, 0, n - 1],
                        center: [n / 2, n / 2],
                        radius: n as f32,
                        relevant: 2,
                    },
                    &mut work,
                    usize::MAX
                )
                .unwrap(),
            2
        );
        let output = transaction.finish(|| false).unwrap().unwrap();
        assert_eq!(output.report.occupancy[0], 1);
        assert_eq!(output.report.blocking.unwrap().raster[0], 1);
        assert_eq!(output.report.topology.unwrap().read_locked_cells, 1);
        assert!(layers.topology.iter().all(|v| *v == 0));
        assert!(layers.splat[3 * cells..4 * cells].iter().all(|v| *v == 0));
    }
    #[test]
    fn procedural_cliffs_sample_the_private_height_revision() {
        let layers = material_layers();
        let n = layers.surface_resolution;
        let hr = layers.height_resolution;
        let alpha = vec![255; n * n];
        let mut transaction = RoadTransaction::new_objects_with_policy(
            &layers,
            &alpha,
            BTreeMap::new(),
            15,
            8,
            ObjectPolicy {
                maximum_work: 50_000_000,
                role_exceptions: 0,
                material_role_exceptions: 0,
            },
            || false,
        )
        .unwrap()
        .unwrap();
        transaction.inner.stage.height[hr * (hr / 2)..].fill(encode(220.));
        let height = transaction.view().height.to_vec();
        let report = transaction
            .apply_cliff_topology(cliff_policy(), false, 50_000_000, || false)
            .unwrap()
            .unwrap();
        assert!(report.added_cliff_cells > 0);
        assert_eq!(transaction.view().height, height);
        assert!(transaction.view().topology.iter().any(|v| *v & 2 != 0));
        assert!(layers.height.iter().all(|v| *v == encode(20.)));
    }
    #[test]
    fn procedural_cliff_cancellation_and_failure_cannot_publish_or_resume() {
        let mut layers = material_layers();
        let n = layers.surface_resolution;
        layers.splat[3 * n * n..4 * n * n].fill(255);
        let alpha = vec![255; n * n];
        let create = |maximum_work| {
            RoadTransaction::new_objects_with_policy(
                &layers,
                &alpha,
                BTreeMap::new(),
                15,
                8,
                ObjectPolicy {
                    maximum_work,
                    role_exceptions: 0,
                    material_role_exceptions: 0,
                },
                || false,
            )
            .unwrap()
            .unwrap()
        };
        let calls = std::cell::Cell::new(0);
        let mut reference = create(50_000_000);
        reference
            .apply_cliff_topology(cliff_policy(), false, 50_000_000, || {
                calls.set(calls.get() + 1);
                false
            })
            .unwrap()
            .unwrap();
        let total = calls.get();
        for stop in [1, 4, total - 2] {
            let calls = std::cell::Cell::new(0);
            let mut transaction = create(50_000_000);
            assert!(
                transaction
                    .apply_cliff_topology(cliff_policy(), false, 50_000_000, || {
                        calls.set(calls.get() + 1);
                        calls.get() >= stop
                    })
                    .unwrap()
                    .is_none()
            );
            assert_eq!(transaction.view().topology, layers.topology);
            assert!(transaction.finish(|| false).unwrap().is_none());
        }
        for (limit, role) in [(1, 2), (50_000_000, 16)] {
            let mut transaction = create(limit);
            let mut policy = cliff_policy();
            policy.cliff_flag = role;
            assert!(
                transaction
                    .apply_cliff_topology(policy, false, limit, || false)
                    .is_err()
            );
            assert_eq!(transaction.view().topology, layers.topology);
            assert!(transaction.finish(|| false).is_err());
        }
    }
    #[test]
    fn live_topology_conflicts_reject_before_large_height_preparations() {
        let layers = material_layers();
        let n = layers.surface_resolution;
        let alpha = vec![255; n * n];
        let placement = stage();
        let path = &placement.prefabs[0];
        let mut spec = session_prefab();
        spec.materials = Some(vec![]);
        spec.height_modifiers.as_mut().unwrap()[0].radius = 100.;
        let cell = index(n, 1000., 0.) * n + index(n, 1000., 0.);
        for live in [false, true] {
            let mut transaction = RoadTransaction::new_objects_with_policy(
                &layers,
                &alpha,
                BTreeMap::from([(path.clone(), spec.clone())]),
                15,
                8,
                ObjectPolicy {
                    maximum_work: 1000,
                    role_exceptions: 0,
                    material_role_exceptions: 0,
                },
                || false,
            )
            .unwrap()
            .unwrap();
            transaction.inner.stage.checked_height = live;
            transaction.inner.stage.topology.as_mut().unwrap().commit(
                crate::decor_topology::Plan {
                    writes: vec![],
                    reads: vec![cell],
                    operations: 0,
                },
            );
            let result = transaction.try_apply(0, &placement, path, &pose(0, 0., 0.), || false);
            assert_eq!(transaction.inner.stage.height, layers.height);
            assert!(
                transaction
                    .inner
                    .stage
                    .support_marks
                    .iter()
                    .all(|word| *word == 0)
            );
            if live {
                assert_eq!(result.unwrap(), Some(false));
                let output = transaction.finish(|| false).unwrap().unwrap();
                assert!(output.report.sample_visits < 1000);
                assert_eq!(
                    output.report.prefabs[path].sample_visits,
                    output.report.sample_visits
                );
                assert_eq!(output.report.topology.as_ref().unwrap().read_conflicts, 1);
                assert_eq!(output.report.counts.occupied, 1);
                assert!(!output.report.budget_exhausted);
                assert!(output.report.applied.is_empty());
                assert_eq!(output.report.height_effects.as_ref().unwrap().instances, 0);
                assert_eq!(output.splat.as_ref().unwrap(), &layers.splat);
                assert_eq!(output.topology.as_ref().unwrap(), &layers.topology);
            } else {
                assert!(result.unwrap_err().contains("work allowance"));
                assert!(transaction.finish(|| false).is_err());
            }
        }
    }
    #[test]
    fn blocked_support_probes_preserve_atomic_rejection_under_tight_budgets() {
        for blocked in ["occupied", "protected", "hole"] {
            let mut layers = material_layers();
            let n = layers.surface_resolution;
            let center = index(n, 1000., 0.) * n + index(n, 1000., 0.);
            let mut alpha = vec![255; n * n];
            if blocked == "protected" {
                layers.topology[center] = 8;
            }
            if blocked == "hole" {
                alpha[center] = 0;
            }
            let placement = stage();
            let path = &placement.prefabs[0];
            let mut transaction = RoadTransaction::new_objects_with_policy(
                &layers,
                &alpha,
                BTreeMap::from([(path.clone(), session_prefab())]),
                15,
                8,
                ObjectPolicy {
                    maximum_work: 1,
                    role_exceptions: 0,
                    material_role_exceptions: 0,
                },
                || false,
            )
            .unwrap()
            .unwrap();
            if blocked == "occupied" {
                transaction.inner.stage.occupancy[center] = 1;
            }
            assert_eq!(
                transaction
                    .try_apply(0, &placement, path, &pose(0, 0., 0.), || false)
                    .unwrap(),
                Some(false),
                "{blocked}"
            );
            let output = transaction.finish(|| false).unwrap().unwrap();
            assert_eq!(output.report.sample_visits, 1);
            assert_eq!(output.report.prefabs[path].sample_visits, 1);
            assert!(!output.report.budget_exhausted);
            assert!(output.report.applied.is_empty());
            assert_eq!(output.height, layers.height);
            assert_eq!(output.topology.as_ref().unwrap(), &layers.topology);
            assert_eq!(output.splat.as_ref().unwrap(), &layers.splat);
        }
    }
    #[test]
    fn blocked_topology_addition_rejects_before_large_query_scan_atomically() {
        let mut layers = material_layers();
        layers.topology.fill(1);
        let n = layers.surface_resolution;
        let alpha = vec![255; n * n];
        let placement = stage();
        let path = &placement.prefabs[0];
        let mut spec = session_prefab();
        spec.support = vec![Support {
            position: [0.; 3],
            radius: 0.,
        }];
        spec.materials = Some(vec![]);
        let topology = spec.topology.as_mut().unwrap();
        topology.modifiers[0].position = [35., 0., 0.];
        topology.modifiers[0].radius = 12.;
        topology.modifiers[0].mask = 4;
        topology.queries.push(crate::decor_topology::Query {
            check_placement: false,
            position: [0.; 3],
            radius: 128.,
            filter: crate::spawn::Filter {
                splat_type: -1,
                biome_type: -1,
                topology_any: -1,
                topology_all: 1,
                topology_not: 0,
            },
        });
        let mut transaction = RoadTransaction::new_objects_with_policy(
            &layers,
            &alpha,
            BTreeMap::from([(path.clone(), spec)]),
            15,
            8,
            ObjectPolicy {
                maximum_work: 256,
                role_exceptions: 0,
                material_role_exceptions: 0,
            },
            || false,
        )
        .unwrap()
        .unwrap();
        let cell = index(n, 1000., 0.) * n + index(n, 1000., 35.);
        transaction.inner.stage.occupancy[cell] = 1;
        assert_eq!(
            transaction
                .try_apply(0, &placement, path, &pose(0, 0., 0.), || false)
                .unwrap(),
            Some(false)
        );
        let output = transaction.finish(|| false).unwrap().unwrap();
        assert_eq!(output.report.counts.occupied, 1);
        assert!(!output.report.budget_exhausted);
        assert!(output.report.sample_visits < 256);
        assert!(output.report.applied.is_empty());
        assert_eq!(output.height, layers.height);
        assert_eq!(output.topology.as_ref().unwrap(), &layers.topology);
        assert_eq!(output.splat.as_ref().unwrap(), &layers.splat);
    }
    #[test]
    fn removal_shares_the_live_height_topology_and_occupancy_transaction() {
        let mut layers = material_layers();
        let cells = layers.surface_resolution.pow(2);
        layers.biome.fill(0);
        layers.biome[..cells].fill(255);
        layers.splat.fill(0);
        layers.splat[3 * cells..4 * cells].fill(255);
        let alpha = vec![255; cells];
        let placement = stage();
        let path = &placement.prefabs[0];
        let mut prefab = session_prefab();
        let material = &mut prefab.materials.as_mut().unwrap()[0];
        material.channel = 3;
        material.opacity = 0.5;
        material.operation =
            crate::decor_material::Operation::Remove(crate::decor_material::Removal {
                replacement_channels: [2, 4, 4, 1, 0],
                excluded_topology: 8,
            });
        let center = index(layers.surface_resolution, 1000., 0.) * layers.surface_resolution
            + index(layers.surface_resolution, 1000., 0.);
        for blocked in [false, true] {
            let mut transaction = RoadTransaction::new_objects_with_policy(
                &layers,
                &alpha,
                BTreeMap::from([(path.clone(), prefab.clone())]),
                15,
                8,
                ObjectPolicy {
                    maximum_work: 100000,
                    role_exceptions: 0,
                    material_role_exceptions: 0,
                },
                || false,
            )
            .unwrap()
            .unwrap();
            if blocked {
                transaction.inner.stage.occupancy[center] = 1;
            }
            assert_eq!(
                transaction
                    .try_apply(0, &placement, path, &pose(0, 0., 0.), || false)
                    .unwrap(),
                Some(!blocked)
            );
            let output = transaction.finish(|| false).unwrap().unwrap();
            if blocked {
                assert_eq!(output.height, layers.height);
                assert_eq!(output.topology.as_ref().unwrap(), &layers.topology);
                assert_eq!(output.splat.as_ref().unwrap(), &layers.splat);
                assert!(output.report.applied.is_empty());
            } else {
                assert_eq!(output.splat.as_ref().unwrap()[2 * cells + center], 128);
                assert_eq!(output.splat.as_ref().unwrap()[3 * cells + center], 128);
                assert_eq!(
                    output.topology.as_ref().unwrap()[center],
                    layers.topology[center] | 2
                );
                assert_ne!(output.height, layers.height);
                assert_eq!(output.report.applied.len(), 1);
            }
        }
    }
    #[test]
    fn sparse_support_union_rejects_without_scanning_empty_bounds() {
        let layers = material_layers();
        let n = layers.surface_resolution;
        let alpha = vec![255; n * n];
        let placement = stage();
        let path = &placement.prefabs[0];
        let mut spec = session_prefab();
        spec.support = vec![
            Support {
                position: [-200., 0., -200.],
                radius: 0.,
            },
            Support {
                position: [200., 0., 200.],
                radius: 0.,
            },
        ];
        spec.materials = Some(vec![]);
        spec.height_modifiers = Some(vec![]);
        spec.topology = Some(crate::decor_topology::Prefab::default());
        let radius = 1000. / (layers.height_resolution - 1) as f32 * 2. * 2_f32.sqrt();
        for fast in [false, true] {
            let mut transaction = RoadTransaction::new_objects_with_policy(
                &layers,
                &alpha,
                BTreeMap::from([(path.clone(), spec.clone())]),
                15,
                8,
                ObjectPolicy {
                    maximum_work: 100,
                    role_exceptions: 0,
                    material_role_exceptions: 0,
                },
                || false,
            )
            .unwrap()
            .unwrap();
            transaction.inner.stage.checked_height = fast;
            let cell = index(n, 1000., -200.) * n + index(n, 1000., -200. + radius);
            transaction.inner.stage.occupancy[cell] = 1;
            let result = transaction.try_apply(0, &placement, path, &pose(0, 0., 0.), || false);
            assert_eq!(transaction.inner.stage.height, layers.height);
            if fast {
                assert_eq!(result.unwrap(), Some(false));
                let output = transaction.finish(|| false).unwrap().unwrap();
                assert_eq!(output.report.counts.occupied, 1);
                assert!(output.report.sample_visits < 100);
                assert!(output.report.applied.is_empty());
            } else {
                assert!(result.unwrap_err().contains("work allowance"));
                assert!(transaction.finish(|| false).is_err());
            }
        }
    }
    #[test]
    fn support_disk_union_matches_rectangle_scan_with_overlaps_and_rotation() {
        let layers = material_layers();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let placement = stage();
        let path = &placement.prefabs[0];
        let mut spec = session_prefab();
        spec.support = vec![
            Support {
                position: [0.; 3],
                radius: 6.,
            },
            Support {
                position: [4., 0., 3.],
                radius: 3.,
            },
            Support {
                position: [0.; 3],
                radius: 6.,
            },
        ];
        spec.topology = Some(crate::decor_topology::Prefab::default());
        spec.materials = Some(vec![]);
        spec.height_modifiers = Some(vec![]);
        let mut object = pose(0, 0., 0.);
        let half = 37_f32.to_radians() * 0.5;
        object.rotation = [0., half.sin(), 0., half.cos()];
        object.scale = [1.25, 0.8, 1.6];
        let mut reference = None;
        for fast in [false, true] {
            let mut transaction = RoadTransaction::new_objects_with_policy(
                &layers,
                &alpha,
                BTreeMap::from([(path.clone(), spec.clone())]),
                15,
                8,
                ObjectPolicy {
                    maximum_work: 100000,
                    role_exceptions: 0,
                    material_role_exceptions: 0,
                },
                || false,
            )
            .unwrap()
            .unwrap();
            transaction.inner.stage.checked_height = fast;
            assert_eq!(
                transaction
                    .try_apply(0, &placement, path, &object, || false)
                    .unwrap(),
                Some(true)
            );
            let state = (
                transaction.inner.stage.occupancy.clone(),
                transaction.inner.report.reserved_cells,
            );
            assert!(
                transaction
                    .inner
                    .stage
                    .support_marks
                    .iter()
                    .all(|word| *word == 0)
            );
            if let Some(reference) = &reference {
                assert_eq!(&state, reference);
            } else {
                reference = Some(state);
            }
            let output = transaction.finish(|| false).unwrap().unwrap();
            assert_eq!(output.height, layers.height);
            assert_eq!(output.topology.as_ref().unwrap(), &layers.topology);
            assert_eq!(output.splat.as_ref().unwrap(), &layers.splat);
        }
    }
    #[test]
    fn authored_cliff_rim_or_blocker_guarantees_nearby_cell_coverage() {
        let scales = CliffScaleBounds { smallest_axis: 0.8, largest_axis: 1.5 };
        let placement = stage();
        let path = &placement.prefabs[0];
        for use_blocker in [false, true] {
            for (x, z, neighbor) in [(0., 0., 0), (1.8, 0.2, 1), (-0.1, -1.8, 1)] {
                let mut layers = material_layers();
                let n = layers.surface_resolution;
                layers.topology[index(n, 1000., z) * n + index(n, 1000., x) + neighbor] = 8;
                let alpha = vec![255; n * n];
                let mut spec = session_prefab();
                spec.support.clear();
                spec.footprint = if use_blocker {
                    spec.blockers = vec![Blocker {
                        position: [0.2, 0.3, 0.4],
                        radius: 6.,
                        fade: 1.,
                    }];
                    None
                } else {
                    let mut footprint = prefab().footprint.unwrap();
                    footprint.rim_width = 10.;
                    Some(footprint)
                };
                let mut tx = RoadTransaction::new_objects(
                    &layers, &alpha, BTreeMap::from([(path.clone(), spec)]), 15, 8, || false,
                ).unwrap().unwrap();
                let (check, limit) = tx.origin_check(&placement, path, Some(scales)).unwrap().unwrap();
                assert!(limit.is_none());
                assert_eq!(tx.probe_origin([x, 22., z], check, false).blocked, neighbor == 0);
                assert!(tx.origin_conflicts([x, 22., z], check));
                assert!(!tx.origin_conflicts([x + 10., 22., z], check));
                let mut object = pose(0, x, z);
                object.scale = [1.5, 0.8, 1.1];
                object.rotation = crate::decor_fit::euler([12., 58., -10.]);
                assert_eq!(tx.try_apply(0, &placement, path, &object, || false).unwrap(), Some(false));
                let output = tx.finish(|| false).unwrap().unwrap();
                assert_eq!(output.height, layers.height);
                assert_eq!(output.report.counts.accepted, 0);
            }
        }
    }
    #[test]
    fn cliff_surface_cell_probe_rejects_an_origin_rounded_into_the_map() {
        let mut layers = material_layers();
        layers.topology.fill(8);
        let n = layers.surface_resolution;
        let alpha = vec![255; n * n];
        let tx = RoadTransaction::new_objects(&layers, &alpha, BTreeMap::new(), 15, 8, || false)
            .unwrap()
            .unwrap();
        let check = OriginCheck { exception: 0, coverage: 1. };
        let edge = layers.size as f32 * 0.5;
        let beyond = edge.next_up();
        let coordinates = [n - 1, n / 2];
        assert_eq!(beyond / layers.size as f32 + 0.5, 1.);
        assert!(check.conflicts(tx.probe_origin_cell([edge, 0.], coordinates, check, true)));
        assert!(!check.conflicts(tx.probe_origin_cell([beyond, 0.], coordinates, check, true)));
    }
    #[test]
    fn blocked_footprint_probes_preserve_work_for_dense_live_object_passes() {
        for blocked_x in [0., -3.] {
            let mut layers = material_layers();
            let n = layers.surface_resolution;
            let center = index(n, 1000., 0.) * n + index(n, 1000., blocked_x);
            layers.topology[center] = 8;
            let alpha = vec![255; n * n];
            let placement = stage();
            let path = &placement.prefabs[0];
            let mut spec = session_prefab();
            spec.footprint = prefab().footprint;
            spec.support.clear();
            spec.topology.as_mut().unwrap().modifiers.clear();
            spec.materials = Some(vec![]);
            spec.height_modifiers = Some(vec![]);
            let mut transaction = RoadTransaction::new_objects_with_policy(
                &layers,
                &alpha,
                BTreeMap::from([(path.clone(), spec)]),
                15,
                8,
                ObjectPolicy {
                    maximum_work: 4,
                    role_exceptions: 0,
                    material_role_exceptions: 0,
                },
                || false,
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                transaction
                    .try_apply(0, &placement, path, &pose(0, 0., 0.), || false)
                    .unwrap(),
                Some(false)
            );
            let output = transaction.finish(|| false).unwrap().unwrap();
            assert_eq!(output.report.sample_visits, 4);
            assert_eq!(output.report.counts.protected, 1);
            assert!(!output.report.budget_exhausted);
            assert!(output.report.applied.is_empty());
            assert_eq!(output.height, layers.height);
        }
    }
    #[test]
    fn blocked_bounding_box_center_does_not_reject_a_concave_footprint() {
        let mut layers = material_layers();
        let n = layers.surface_resolution;
        layers.topology[index(n, 1000., 0.) * n + index(n, 1000., 0.)] = 8;
        let alpha = vec![255; n * n];
        let placement = stage();
        let path = &placement.prefabs[0];
        let mut spec = session_prefab();
        spec.footprint = prefab().footprint;
        spec.footprint.as_mut().unwrap().ring = [
            [-30., -30.],
            [30., -30.],
            [30., -20.],
            [-20., -20.],
            [-20., 20.],
            [30., 20.],
            [30., 30.],
            [-30., 30.],
        ]
        .map(|[x, z]| [x, 0., z])
        .to_vec();
        spec.support.clear();
        spec.topology.as_mut().unwrap().modifiers.clear();
        spec.materials = Some(vec![]);
        spec.height_modifiers = Some(vec![]);
        for degrees in [0_f32, 13., 90., 167.] {
            let mut transaction = RoadTransaction::new_objects(
                &layers,
                &alpha,
                BTreeMap::from([(path.clone(), spec.clone())]),
                15,
                8,
                || false,
            )
            .unwrap()
            .unwrap();
            let mut proposed = pose(0, 0., 0.);
            let (sine, cosine) = (degrees.to_radians() * 0.5).sin_cos();
            proposed.rotation = [0., sine, 0., cosine];
            assert_eq!(
                transaction
                    .try_apply(0, &placement, path, &proposed, || false)
                    .unwrap(),
                Some(true),
                "yaw {degrees}"
            );
            let output = transaction.finish(|| false).unwrap().unwrap();
            assert_eq!(output.report.counts.protected, 0);
            assert_eq!(output.report.counts.accepted, 1);
            assert_eq!(output.height[0], layers.height[0]);
        }
    }
    #[test]
    fn scoped_adapters_share_occupancy_and_work_without_inheriting_the_other_masks() {
        let mut l = material_layers();
        l.topology.fill(4);
        let n = l.surface_resolution;
        l.topology[index(n, l.size as f32, 0.) * n + index(n, l.size as f32, 300.)] = 5;
        let alpha = vec![255; n * n];
        let mut s = stage();
        s.filter.topology_any = 4;
        let path = &s.prefabs[0];
        let road = session_prefab();
        let mut decor = road.clone();
        decor.height_modifiers = Some(vec![]);
        decor.materials = Some(vec![]);
        decor.topology.as_mut().unwrap().modifiers[0].mask = 8;
        let specs = BTreeMap::from([
            (format!("road:{path}"), road),
            (format!("decoration:{path}"), decor),
        ]);
        let mut tx = RoadTransaction::new_objects(&l, &alpha, specs.clone(), 15, 5, || false)
            .unwrap()
            .unwrap();
        let road_scope = ProtectionScope {
            namespace: "road".into(),
            protected_topology: 1,
            role_exceptions: 0,
            material_role_exceptions: 0,
            maximum_height_change: None,
        };
        let decor_scope = ProtectionScope {
            namespace: "decoration".into(),
            protected_topology: 5,
            role_exceptions: 4,
            material_role_exceptions: 0,
            maximum_height_change: None,
        };
        tx.set_scope(road_scope.clone()).unwrap();
        assert_eq!(
            tx.try_apply(0, &s, path, &pose(0, 0., 0.), || false)
                .unwrap(),
            Some(true)
        );
        let work = tx.sample_visits();
        let height = tx.view().height.as_ptr();
        tx.set_scope(decor_scope).unwrap();
        assert_eq!(tx.sample_visits(), work);
        assert_eq!(tx.view().height.as_ptr(), height);
        assert_eq!(
            tx.try_apply(1, &s, path, &pose(1, 100., 0.), || false)
                .unwrap(),
            Some(true)
        );
        assert_eq!(
            tx.try_apply(2, &s, path, &pose(2, 0., 0.), || false)
                .unwrap(),
            Some(false)
        );
        tx.set_scope(road_scope.clone()).unwrap();
        assert_eq!(
            tx.try_apply(3, &s, path, &pose(3, 200., 0.), || false)
                .unwrap(),
            Some(true)
        );
        assert_eq!(
            tx.try_apply(4, &s, path, &pose(4, 300., 0.), || false)
                .unwrap(),
            Some(false)
        );
        let out = tx.finish(|| false).unwrap().unwrap();
        assert_eq!(out.report.protection_scopes.as_ref().unwrap().len(), 2);
        assert_eq!(
            out.report
                .applied
                .iter()
                .map(|p| p.protection_scope)
                .collect::<Vec<_>>(),
            [Some(0), Some(1), Some(0)]
        );
        let topo = out.topology.unwrap();
        assert_eq!(topo[index(n, 1000., 0.) * n + index(n, 1000., 100.)], 12);
        assert_eq!(out.report.counts.accepted, 3);
        assert_eq!(out.report.counts.occupied, 1);
        assert_eq!(out.report.counts.protected, 1);
        let mut limited = RoadTransaction::new_objects(&l, &alpha, specs.clone(), 15, 5, || false)
            .unwrap()
            .unwrap();
        let mut limit_scope = road_scope.clone();
        limit_scope.maximum_height_change = Some(0.);
        limited.set_scope(limit_scope).unwrap();
        assert_eq!(
            limited
                .try_apply(0, &s, path, &pose(0, 0., 0.), || false)
                .unwrap(),
            Some(false)
        );
        let rejected = limited.finish(|| false).unwrap().unwrap();
        assert_eq!(rejected.report.counts.fill_limit, 1);
        assert_eq!(rejected.height, l.height);
        assert_eq!(rejected.topology.as_ref().unwrap(), &l.topology);
        assert_eq!(rejected.splat.as_ref().unwrap(), &l.splat);
        let mut bad = RoadTransaction::new_objects(&l, &alpha, specs, 15, 5, || false)
            .unwrap()
            .unwrap();
        let mut invalid = road_scope;
        invalid.protected_topology = 8;
        assert!(bad.set_scope(invalid).is_err());
        assert!(bad.finish(|| false).is_err());
    }
    #[test]
    fn initial_blocker_raster_is_validated_private_and_cannot_reset_an_active_pass() {
        let l = material_layers();
        let n = l.surface_resolution;
        let mut s = stage();
        let path = s.prefabs.remove(0);
        s.prefabs.push(path.clone());
        let alpha = vec![255; n * n];
        let mut raster = vec![0; n * n];
        raster[index(n, 1000., 0.) * n + index(n, 1000., 20.)] = 1;
        let mut p = session_prefab();
        p.height_modifiers = Some(vec![]);
        p.materials = Some(vec![]);
        p.topology
            .as_mut()
            .unwrap()
            .queries
            .push(crate::decor_topology::Query {
                position: [20., 0., 0.],
                radius: 0.,
                filter: s.filter.clone(),
                check_placement: true,
            });
        let build = || {
            RoadTransaction::new_objects(
                &l,
                &alpha,
                BTreeMap::from([(path.clone(), p.clone())]),
                15,
                8,
                || false,
            )
            .unwrap()
            .unwrap()
        };
        let mut tx = build();
        assert_eq!(tx.initialize_blocking(&raster, || false).unwrap(), Some(()));
        assert_eq!(tx.view().placement_blockers, Some(raster.as_slice()));
        assert_eq!(
            tx.try_apply(0, &s, &path, &pose(0, 0., 0.), || false)
                .unwrap(),
            Some(false)
        );
        let out = tx.finish(|| false).unwrap().unwrap();
        assert_eq!(out.report.counts.placement_blocked, 1);
        assert_eq!(out.report.blocking.as_ref().unwrap().initial_cells, Some(1));
        assert_eq!(out.report.blocking.unwrap().raster, raster);
        let mut tx = build();
        tx.try_apply(0, &s, &path, &pose(0, 0., 0.), || false)
            .unwrap();
        assert!(tx.initialize_blocking(&raster, || false).is_err());
        assert!(tx.finish(|| false).is_err());
        let mut invalid = raster.clone();
        invalid[0] = 2;
        let mut tx = build();
        assert!(tx.initialize_blocking(&invalid, || false).is_err());
        assert!(tx.finish(|| false).is_err());
        let mut tx = build();
        let polls = std::cell::Cell::new(0);
        assert!(
            tx.initialize_blocking(&raster, || {
                polls.set(polls.get() + 1);
                polls.get() == 2
            })
            .unwrap()
            .is_none()
        );
        assert!(tx.finish(|| false).unwrap().is_none());
    }
    #[test]
    fn required_material_roles_allow_paint_but_never_height_holes_or_other_hard_roles() {
        let mut l = material_layers();
        l.topology.fill(4);
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let mut s = stage();
        s.filter.topology_any = -1;
        s.filter.topology_all = 4;
        let path = &s.prefabs[0];
        let run = |layers: &Layers, alpha: &[u8], paint: u32, height: bool| {
            let mut p = session_prefab();
            if !height {
                p.height_modifiers = Some(vec![]);
            }
            let mut tx = RoadTransaction::new_objects_with_policy(
                layers,
                alpha,
                BTreeMap::from([(path.clone(), p)]),
                15,
                15,
                ObjectPolicy {
                    maximum_work: MAX_VISITS,
                    role_exceptions: 4,
                    material_role_exceptions: paint,
                },
                || false,
            )
            .unwrap()
            .unwrap();
            let blocked = tx.origin_blocked(&s, path, [0.; 3]).unwrap();
            let accepted = tx
                .try_apply(0, &s, path, &pose(0, 0., 0.), || false)
                .unwrap()
                .unwrap();
            assert_eq!(blocked, !accepted);
            tx.finish(|| false).unwrap().unwrap()
        };
        let allowed = run(&l, &alpha, 4, false);
        assert_eq!(allowed.report.counts.accepted, 1);
        assert_eq!(allowed.height, l.height);
        assert_ne!(allowed.splat.as_ref().unwrap(), &l.splat);
        assert_eq!(allowed.report.permitted_material_role_exceptions, Some(4));
        assert_eq!(allowed.report.applied[0].protection_exception, Some(4));
        assert_eq!(run(&l, &alpha, 0, false).report.counts.protected, 1);
        assert_eq!(run(&l, &alpha, 4, true).report.counts.protected, 1);
        let mut holes = alpha.clone();
        let n = l.surface_resolution;
        holes[n / 2 * n + n / 2] = 0;
        assert_eq!(run(&l, &holes, 4, false).report.counts.holes, 1);
        l.topology.fill(5);
        assert_eq!(run(&l, &alpha, 4, false).report.counts.protected, 1);
        assert!(
            ObjectPolicy {
                maximum_work: MAX_VISITS,
                role_exceptions: 4,
                material_role_exceptions: 8
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn authored_placement_checkpoints_read_committed_blockers_with_full_pose() {
        let l = material_layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let mut s = stage();
        let a = s.prefabs[0].clone();
        let b = a.replace("a.prefab", "b.prefab");
        assert_ne!(a, b);
        s.prefabs.push(b.clone());
        for check in [false, true] {
            let mut blocker = session_prefab();
            blocker.height_modifiers = Some(vec![]);
            blocker.materials = Some(vec![]);
            blocker.topology = Some(crate::decor_topology::Prefab::default());
            blocker.blockers = vec![Blocker {
                position: [40., 0., 0.],
                radius: 4.,
                fade: 0.,
            }];
            let mut probe = session_prefab();
            probe.height_modifiers = Some(vec![]);
            probe.materials = Some(vec![]);
            probe
                .topology
                .as_mut()
                .unwrap()
                .queries
                .push(crate::decor_topology::Query {
                    check_placement: check,
                    position: [0., 0., -30.],
                    radius: 0.,
                    filter: s.filter.clone(),
                });
            let mut tx = RoadTransaction::new_objects(
                &l,
                &alpha,
                BTreeMap::from([(a.clone(), blocker), (b.clone(), probe)]),
                15,
                8,
                || false,
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                tx.try_apply(0, &s, &a, &pose(0, 0., 0.), || false).unwrap(),
                Some(true)
            );
            let mut moved = pose(1, 100., 0.);
            moved.rotation = [
                0.,
                std::f32::consts::FRAC_1_SQRT_2,
                0.,
                std::f32::consts::FRAC_1_SQRT_2,
            ];
            moved.scale = [1., 1., 2.];
            assert_eq!(
                tx.try_apply(1, &s, &b, &moved, || false).unwrap(),
                Some(!check)
            );
            let out = tx.finish(|| false).unwrap().unwrap();
            assert_eq!(out.report.counts.placement_blocked, usize::from(check));
            assert_eq!(out.report.counts.accepted, if check { 1 } else { 2 });
            assert_eq!(out.height, l.height);
        }
    }
    #[test]
    fn topology_only_admission_uses_authored_cells_without_a_synthetic_root() {
        for rejection in ["none", "protected", "hole", "placement"] {
            let mut l = material_layers();
            let n = l.surface_resolution;
            let size = l.size as f32;
            let origin = index(n, size, 0.) * n + index(n, size, 0.);
            let target = index(n, size, 30.) * n + index(n, size, 30.);
            l.topology[origin] = 8;
            if rejection == "protected" {
                l.topology[target] = 8;
            }
            let mut alpha = vec![255; n * n];
            if rejection == "hole" {
                alpha[target] = 0;
            }
            let s = stage();
            let path = &s.prefabs[0];
            let mut prefab = session_prefab();
            prefab.support.clear();
            prefab.footprint = None;
            prefab.blockers.clear();
            prefab.raises.clear();
            prefab.height_modifiers = Some(vec![]);
            prefab.contributors = Some(vec![]);
            prefab.materials = Some(vec![]);
            prefab.topology = Some(crate::decor_topology::Prefab {
                modifiers: vec![crate::decor_topology::Modifier {
                    position: [30., 0., 30.],
                    radius: 6.,
                    fade: 0.,
                    mask: 2,
                    operation: crate::decor_topology::Operation::Add,
                }],
                queries: vec![crate::decor_topology::Query {
                    check_placement: true,
                    position: [30., 0., 30.],
                    radius: 0.,
                    filter: s.filter.clone(),
                }],
                ..Default::default()
            });
            let mut tx = RoadTransaction::new_objects(
                &l,
                &alpha,
                BTreeMap::from([(path.clone(), prefab)]),
                15,
                8,
                || false,
            )
            .unwrap()
            .unwrap();
            let mut blocked = vec![0; n * n];
            if rejection == "placement" {
                blocked[target] = 1;
            }
            tx.initialize_blocking(&blocked, || false).unwrap().unwrap();
            assert!(!tx.origin_blocked(&s, path, [0.; 3]).unwrap());
            assert_eq!(
                tx.try_apply(0, &s, path, &pose(0, 0., 0.), || false)
                    .unwrap(),
                Some(rejection == "none"),
                "{rejection}"
            );
            let out = tx.finish(|| false).unwrap().unwrap();
            assert_eq!(out.height, l.height);
            let topology = out.topology.unwrap();
            assert_eq!(topology[origin], 8);
            if rejection == "none" {
                assert_eq!(topology[target], 2);
            } else {
                assert_eq!(topology, l.topology);
            }
        }
    }
    #[test]
    fn live_topology_summaries_update_only_with_committed_terrain() {
        let l = material_layers();
        let original = l.topology.clone();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let s = stage();
        let path = &s.prefabs[0];
        let mut tx = RoadTransaction::new_objects(
            &l,
            &alpha,
            BTreeMap::from([(path.clone(), session_prefab())]),
            15,
            8,
            || false,
        )
        .unwrap()
        .unwrap();
        let query = |tx: &RoadTransaction<'_>| {
            let view = tx.view();
            let n = view.surface_resolution;
            let tiles = view.topology_tiles.unwrap();
            assert!(tiles.matches(view.topology, n));
            tiles
                .union(
                    crate::topology_query::Query {
                        rect: [0, n - 1, 0, n - 1],
                        center: [n / 2, n / 2],
                        radius: 10000.,
                        relevant: 2,
                    },
                    &mut 0,
                    100000,
                )
                .unwrap()
        };
        assert_eq!(query(&tx), 0);
        assert_eq!(
            tx.try_apply(0, &s, path, &pose(0, 0., 0.), || false)
                .unwrap(),
            Some(true)
        );
        assert_eq!(query(&tx), 2);
        assert_eq!(
            tx.try_apply(1, &s, path, &pose(1, 0., 0.), || false)
                .unwrap(),
            Some(false)
        );
        assert_eq!(query(&tx), 2);
        assert!(l.view().topology_tiles.is_none());
        assert_eq!(l.topology, original);
    }
    #[test]
    fn explicit_object_work_policy_survives_stages_and_preserves_default_limit() {
        let l = material_layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let s = stage();
        let path = s.prefabs[0].clone();
        for maximum_work in [MAX_VISITS, 2 * MAX_VISITS] {
            let mut tx = RoadTransaction::new_objects_with_policy(
                &l,
                &alpha,
                BTreeMap::from([(path.clone(), session_prefab())]),
                15,
                8,
                ObjectPolicy {
                    maximum_work,
                    role_exceptions: 0,
                    material_role_exceptions: 0,
                },
                || false,
            )
            .unwrap()
            .unwrap();
            // Simulate work already consumed by previous source stages.
            tx.inner.stage.visits = MAX_VISITS;
            let result = tx.try_apply(0, &s, &path, &pose(0, 0., 0.), || false);
            if maximum_work == MAX_VISITS {
                assert!(result.is_err());
                assert!(tx.finish(|| false).is_err());
            } else {
                assert_eq!(result.unwrap(), Some(true));
                let out = tx.finish(|| false).unwrap().unwrap();
                assert_eq!(out.report.maximum_work, Some(maximum_work));
                assert!(
                    out.report.sample_visits > MAX_VISITS
                        && out.report.sample_visits <= maximum_work
                );
            }
        }
        for maximum_work in [0, 1_024_000_001, usize::MAX] {
            assert!(
                ObjectPolicy {
                    maximum_work,
                    role_exceptions: 0,
                    material_role_exceptions: 0,
                }
                .validate()
                .is_err()
            );
        }
        let default = RoadTransaction::new_objects(
            &l,
            &alpha,
            BTreeMap::from([(path, session_prefab())]),
            15,
            8,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(default.inner.stage.visit_limit, MAX_VISITS);
        assert_eq!(default.inner.report.maximum_work, None);
        assert!(
            RoadTransaction::new_objects_with_policy(
                &l,
                &alpha,
                BTreeMap::new(),
                15,
                8,
                ObjectPolicy {
                    maximum_work: 2 * MAX_VISITS,
                    role_exceptions: 0,
                    material_role_exceptions: 0,
                },
                || true
            )
            .unwrap()
            .is_none()
        );
    }
    #[test]
    fn authored_role_exceptions_allow_topology_only_supports_without_earthwork_or_hard_protection_bypass()
     {
        let mut l = material_layers();
        l.topology.fill(4);
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let mut s = stage();
        s.filter.topology_any = 4;
        let path = s.prefabs[0].clone();
        let spec = || {
            let mut p = session_prefab();
            p.height_modifiers = Some(vec![]);
            p.materials = Some(vec![]);
            p
        };
        let run = |layers: &Layers, source: &decor::Stage, p: Prefab, roles: u32| {
            let mut transaction = RoadTransaction::new_objects_with_roles(
                layers,
                &alpha,
                BTreeMap::from([(path.clone(), p)]),
                15,
                15,
                roles,
                || false,
            )
            .unwrap()
            .unwrap();
            let blocked = transaction.origin_blocked(source, &path, [0.; 3]).unwrap();
            assert_eq!(transaction.sample_visits(), 0);
            let accepted = transaction
                .try_apply(0, source, &path, &pose(0, 0., 0.), || false)
                .unwrap()
                .unwrap();
            assert_eq!(blocked, !accepted);
            transaction.finish(|| false).unwrap().unwrap()
        };
        let allowed = run(&l, &s, spec(), 4);
        assert_eq!(allowed.report.counts.accepted, 1);
        assert_eq!(allowed.report.applied[0].protection_exception, Some(4));
        assert_eq!(allowed.height, l.height);
        assert_eq!(allowed.splat.as_ref().unwrap(), &l.splat);
        assert_ne!(allowed.topology.as_ref().unwrap(), &l.topology);
        assert_eq!(run(&l, &s, spec(), 0).report.counts.protected, 1);
        let mut unrestricted = s.clone();
        unrestricted.filter.topology_any = -1;
        assert_eq!(run(&l, &unrestricted, spec(), 4).report.counts.protected, 1);
        let mut ambiguous = s.clone();
        ambiguous.filter.topology_any = 5;
        assert_eq!(run(&l, &ambiguous, spec(), 4).report.counts.protected, 1);
        assert_eq!(run(&l, &s, session_prefab(), 4).report.counts.protected, 1);
        l.topology.fill(5);
        assert_eq!(run(&l, &s, spec(), 4).report.counts.protected, 1);
        assert!(
            RoadTransaction::new_objects_with_roles(
                &l,
                &alpha,
                BTreeMap::new(),
                15,
                15,
                15,
                || false
            )
            .is_err()
        );
    }
    fn session_prefab() -> Prefab {
        let mut p = prefab();
        p.footprint = None;
        p.topology = Some(crate::decor_topology::Prefab {
            modifiers: vec![crate::decor_topology::Modifier {
                position: [0.; 3],
                radius: 4.,
                fade: 0.,
                mask: 2,
                operation: crate::decor_topology::Operation::Add,
            }],
            queries: vec![],
            anchors: vec![],
        });
        p.materials = Some(vec![crate::decor_material::Modifier {
            operation: crate::decor_material::Operation::Set,
            position: [0.; 3],
            radius: 4.,
            fade: 0.,
            opacity: 1.,
            channel: 1,
        }]);
        p.height_modifiers = Some(vec![crate::decor_height::Modifier {
            position: [0.; 3],
            radius: 4.,
            fade: 0.,
            opacity: 1.,
            mode: crate::decor_height::Mode::Add { delta: 2. },
        }]);
        p.contributors = Some(vec![]);
        p
    }
    #[test]
    fn incremental_road_admission_matches_batch_and_reads_live_grids_without_copies() {
        let l = material_layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let s = stage();
        let path = &s.prefabs[0];
        let prefabs = || BTreeMap::from([(path.clone(), session_prefab())]);
        let mut transaction = RoadTransaction::new(&l, &alpha, prefabs(), 15, 1, || false)
            .unwrap()
            .unwrap();
        let pointers = {
            let view = transaction.view();
            assert!(std::ptr::eq(view.biome.as_ptr(), l.biome.as_ptr()));
            (
                view.height.as_ptr(),
                view.topology.as_ptr(),
                view.splat.as_ptr(),
            )
        };
        let poses = [pose(0, 0., 0.), pose(1, 100., 0.)];
        for (i, p) in poses.iter().enumerate() {
            assert_eq!(
                transaction.try_apply(i, &s, path, p, || false).unwrap(),
                Some(true)
            );
            let view = transaction.view();
            assert_eq!(
                pointers,
                (
                    view.height.as_ptr(),
                    view.topology.as_ptr(),
                    view.splat.as_ptr()
                )
            );
            let cell = index(l.surface_resolution, 1000., p.position[2]) * l.surface_resolution
                + index(l.surface_resolution, 1000., p.position[0]);
            assert_eq!(view.topology[cell], 2);
            assert_eq!(view.splat[l.surface_resolution.pow(2) + cell], 255);
            let h = index(l.height_resolution, 1000., p.position[2]) * l.height_resolution
                + index(l.height_resolution, 1000., p.position[0]);
            assert!((decode(view.height[h]) - decode(l.height[h]) - 2.).abs() < 0.04);
        }
        let incremental = transaction.finish(|| false).unwrap().unwrap();
        let candidates: Vec<_> = poses
            .iter()
            .map(|p| decor::Candidate {
                stage: 0,
                prefab: 0,
                position: p.position,
            })
            .collect();
        let batch = apply_road_objects(
            Input {
                height_reservations: None,
                layers: &l,
                alpha: &alpha,
                candidates: &candidates,
                stages: std::slice::from_ref(&s),
                poses: &poses,
                prefabs: &prefabs(),
                obstacles: &[],
                protected_topology: 1,
            },
            15,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(incremental.height, batch.height);
        assert_eq!(incremental.topology, batch.topology);
        assert_eq!(incremental.splat, batch.splat);
        assert_eq!(
            serde_json::to_value(incremental.report).unwrap(),
            serde_json::to_value(batch.report).unwrap()
        );
        assert!(l.topology.iter().all(|v| *v == 0));
        assert!(l.height.iter().all(|v| *v == encode(20.)));
    }
    #[test]
    fn live_object_ids_cross_batch_limit_without_retaining_rejected_attempts() {
        let l = material_layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let s = stage();
        let path = &s.prefabs[0];
        let new = || {
            RoadTransaction::new_objects(
                &l,
                &alpha,
                BTreeMap::from([(path.clone(), session_prefab())]),
                15,
                1,
                || false,
            )
            .unwrap()
            .unwrap()
        };
        let mut transaction = new();
        for id in 0..100_002 {
            assert_eq!(
                transaction
                    .try_apply(id, &s, path, &pose(id, 10000., 0.), || false)
                    .unwrap(),
                Some(false)
            );
        }
        assert!(transaction.seen.is_empty());
        assert!(
            transaction
                .try_apply(100_001, &s, path, &pose(100_001, 0., 0.), || false)
                .is_err()
        );
        assert!(transaction.finish(|| false).is_err());
        let mut transaction = new();
        assert!(
            transaction
                .try_apply(1, &s, path, &pose(1, 0., 0.), || false)
                .is_err()
        );
        assert!(transaction.finish(|| false).is_err());
    }
    #[test]
    fn incremental_rejections_keep_prior_commits_but_cancellation_and_invalid_input_forbid_publication()
     {
        let l = material_layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let s = stage();
        let path = &s.prefabs[0];
        let new = || {
            RoadTransaction::new(
                &l,
                &alpha,
                BTreeMap::from([(path.clone(), session_prefab())]),
                15,
                1,
                || false,
            )
            .unwrap()
            .unwrap()
        };
        let mut transaction = new();
        assert_eq!(
            transaction
                .try_apply(0, &s, path, &pose(0, 0., 0.), || false)
                .unwrap(),
            Some(true)
        );
        let before = (
            transaction.view().height.to_vec(),
            transaction.view().topology.to_vec(),
            transaction.view().splat.to_vec(),
        );
        assert_eq!(
            transaction
                .try_apply(1, &s, path, &pose(1, 0., 0.), || false)
                .unwrap(),
            Some(false)
        );
        assert_eq!(transaction.view().height, before.0);
        assert_eq!(transaction.view().topology, before.1);
        assert_eq!(transaction.view().splat, before.2);
        assert_eq!(
            transaction
                .finish(|| false)
                .unwrap()
                .unwrap()
                .report
                .counts
                .occupied,
            1
        );
        let mut transaction = new();
        transaction
            .try_apply(0, &s, path, &pose(0, 0., 0.), || false)
            .unwrap();
        let once = std::cell::Cell::new(true);
        assert!(
            transaction
                .try_apply(1, &s, path, &pose(1, 100., 0.), || once.replace(false))
                .unwrap()
                .is_none()
        );
        assert!(transaction.finish(|| false).unwrap().is_none());
        let mut transaction = new();
        transaction
            .try_apply(0, &s, path, &pose(0, 0., 0.), || false)
            .unwrap();
        assert!(
            transaction
                .try_apply(0, &s, path, &pose(0, 100., 0.), || false)
                .is_err()
        );
        assert!(transaction.finish(|| false).is_err());
    }
    #[test]
    fn incremental_work_exhaustion_and_protected_terrain_cannot_leak_partial_writes() {
        let mut l = material_layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let s = stage();
        let path = &s.prefabs[0];
        let mut transaction = RoadTransaction::new(
            &l,
            &alpha,
            BTreeMap::from([(path.clone(), session_prefab())]),
            15,
            1,
            || false,
        )
        .unwrap()
        .unwrap();
        transaction.inner.stage.visits = MAX_VISITS;
        assert!(
            transaction
                .try_apply(0, &s, path, &pose(0, 0., 0.), || false)
                .is_err()
        );
        assert_eq!(transaction.view().height, l.height);
        assert_eq!(transaction.view().topology, l.topology);
        assert_eq!(transaction.view().splat, l.splat);
        assert!(transaction.finish(|| false).is_err());
        l.topology.fill(1);
        let mut transaction = RoadTransaction::new(
            &l,
            &alpha,
            BTreeMap::from([(path.clone(), session_prefab())]),
            15,
            1,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            transaction
                .try_apply(0, &s, path, &pose(0, 0., 0.), || false)
                .unwrap(),
            Some(false)
        );
        let out = transaction.finish(|| false).unwrap().unwrap();
        assert_eq!(out.report.counts.protected, 1);
        assert_eq!(out.height, l.height);
        assert_eq!(out.topology.unwrap(), l.topology);
        assert_eq!(out.splat.unwrap(), l.splat);
    }
    fn run_height(l: &Layers, p: Prefab, poses: Vec<decor_fit::Pose>, alpha: &[u8]) -> Output {
        run_height_revision(l, p, poses, alpha, false)
    }
    fn run_height_revision(
        l: &Layers,
        p: Prefab,
        poses: Vec<decor_fit::Pose>,
        alpha: &[u8],
        collider: bool,
    ) -> Output {
        let s = stage();
        let prefabs = BTreeMap::from([(s.prefabs[0].clone(), p)]);
        let candidates: Vec<_> = poses
            .iter()
            .map(|p| decor::Candidate {
                stage: 0,
                prefab: 0,
                position: p.position,
            })
            .collect();
        apply_revision(
            Input {
                height_reservations: None,
                layers: l,
                alpha,
                candidates: &candidates,
                stages: &[s],
                poses: &poses,
                prefabs: &prefabs,
                obstacles: &[],
                protected_topology: 1,
            },
            if collider { 10 } else { 9 },
            15,
            || false,
        )
        .unwrap()
        .unwrap()
    }
    fn height_prefab() -> Prefab {
        let mut p = material_prefab();
        p.footprint = None;
        p.raises.clear();
        p.height_modifiers = Some(vec![crate::decor_height::Modifier {
            position: [35., -10., 0.],
            radius: 5.,
            fade: 0.,
            opacity: 1.,
            mode: crate::decor_height::Mode::Lower,
        }]);
        p
    }
    #[test]
    fn dirt_corridor_rejects_height_modifiers_between_controls_without_clipping_prefabs() {
        let l = material_layers();
        let alpha = vec![255; l.surface_resolution * l.surface_resolution];
        let grid = crate::path_terrain::Grid {
            size: l.size as f32,
            resolution: l.height_resolution,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &l.height,
        };
        let controls = [[-100., 0., 0.], [100., 0., 0.]];
        let curve = crate::path_geometry::Path::from_controls(&controls, 1000, || false)
            .unwrap()
            .unwrap();
        let mut points = crate::height_reservations::HeightReservations::new(grid, || false)
            .unwrap()
            .unwrap();
        for p in controls {
            points.reserve_query([p[0], p[2]]).unwrap();
        }
        let mut corridor = crate::height_reservations::HeightReservations::new(grid, || false)
            .unwrap()
            .unwrap();
        corridor
            .reserve_path(&curve, 11., 1_000_000, || false)
            .unwrap()
            .unwrap();
        let s = stage();
        let path = s.prefabs[0].clone();
        let prefabs = BTreeMap::from([(path.clone(), collider_prefab())]);
        let mut unprotected =
            RoadTransaction::new_objects(&l, &alpha, prefabs.clone(), 15, 8, || false)
                .unwrap()
                .unwrap();
        assert!(
            unprotected
                .set_height_reservations(&points, || false)
                .unwrap()
        );
        assert_eq!(
            unprotected
                .try_apply(0, &s, &path, &pose(0, 0., 0.), || false)
                .unwrap(),
            Some(true)
        );
        assert!(
            unprotected
                .view()
                .height
                .iter()
                .zip(&l.height)
                .any(|(a, b)| a != b)
        );
        let mut protected = RoadTransaction::new_objects(&l, &alpha, prefabs, 15, 8, || false)
            .unwrap()
            .unwrap();
        assert!(
            protected
                .set_height_reservations(&corridor, || false)
                .unwrap()
        );
        assert_eq!(
            protected
                .try_apply(0, &s, &path, &pose(0, 0., 0.), || false)
                .unwrap(),
            Some(false)
        );
        assert_eq!(protected.view().height, l.height);
        assert_eq!(protected.view().topology, l.topology);
        assert_eq!(protected.view().splat, l.splat);
        assert_eq!(
            protected
                .try_apply(1, &s, &path, &pose(1, 150., 0.), || false)
                .unwrap(),
            Some(true)
        );
        let result = protected.finish(|| false).unwrap().unwrap();
        assert_eq!(result.report.counts.accepted, 1);
        assert_eq!(result.report.counts.protected, 1);
        for (i, value) in result.height.iter().enumerate() {
            if corridor.contains(i) {
                assert_eq!(*value, l.height[i]);
            }
        }
    }
    #[test]
    fn final_height_reservations_reject_before_large_rasters_but_allow_collider_restoration() {
        let l = material_layers();
        let mut placement = pose(0, 0., 0.);
        placement.position[1] = 0.;
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let grid = crate::path_terrain::Grid {
            size: l.size as f32,
            resolution: l.height_resolution,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &l.height,
        };
        let mut mask = crate::height_reservations::HeightReservations::new(grid, || false)
            .unwrap()
            .unwrap();
        for i in 0..l.height.len() {
            mask.reserve(i).unwrap();
        }
        let s = stage();
        let path = &s.prefabs[0];
        for restore in [false, true] {
            let allowance = if restore { 10_000 } else { 1000 };
            let mut spec = session_prefab();
            spec.topology = Some(crate::decor_topology::Prefab::default());
            spec.materials = Some(vec![]);
            let modifier = &mut spec.height_modifiers.as_mut().unwrap()[0];
            modifier.radius = if restore { 4. } else { 100. };
            modifier.mode = crate::decor_height::Mode::Add { delta: -1. };
            if restore {
                let mut contributor = collider_prefab().contributors.unwrap().remove(0);
                contributor.local_to_root[0][3] = 0.;
                for vertex in &mut contributor.mesh.vertices {
                    vertex[0] *= 2.;
                    vertex[1] = decode(l.height[0]);
                    vertex[2] *= 2.;
                }
                spec.contributors = Some(vec![contributor]);
            }
            let mut tx = RoadTransaction::new_objects_with_policy(
                &l,
                &alpha,
                BTreeMap::from([(path.clone(), spec)]),
                15,
                8,
                ObjectPolicy {
                    maximum_work: l.height.len() + allowance,
                    role_exceptions: 0,
                    material_role_exceptions: 0,
                },
                || false,
            )
            .unwrap()
            .unwrap();
            assert!(tx.set_height_reservations(&mask, || false).unwrap());
            let before = tx.sample_visits();
            assert_eq!(
                tx.try_apply(0, &s, path, &placement, || false)
                    .unwrap(),
                Some(restore)
            );
            assert!(tx.sample_visits() - before < allowance);
            let out = tx.finish(|| false).unwrap().unwrap();
            assert_eq!(out.height, l.height);
            assert_eq!(out.topology.unwrap(), l.topology);
            assert_eq!(out.splat.unwrap(), l.splat);
            assert_eq!(out.report.counts.protected, usize::from(!restore));
        }
    }
    #[test]
    fn road_height_reservations_reject_whole_prefabs_and_leave_unrelated_placements_available() {
        let l = material_layers();
        let alpha = vec![255; l.surface_resolution * l.surface_resolution];
        let p = collider_prefab();
        let poses = vec![pose(0, 0., 0.)];
        let baseline = run_height_revision(&l, p.clone(), vec![pose(0, 0., 0.)], &alpha, true);
        let changed: Vec<_> = baseline
            .height
            .iter()
            .zip(&l.height)
            .enumerate()
            .filter_map(|(i, (a, b))| (a != b).then_some(i))
            .collect();
        assert!(!changed.is_empty());
        let grid = crate::path_terrain::Grid {
            size: l.size as f32,
            resolution: l.height_resolution,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &l.height,
        };
        let mut mask = crate::height_reservations::HeightReservations::new(grid, || false)
            .unwrap()
            .unwrap();
        for i in changed {
            mask.reserve(i).unwrap();
        }
        let s = stage();
        let path = s.prefabs[0].clone();
        let prefabs = BTreeMap::from([(path.clone(), p.clone())]);
        let candidates = [decor::Candidate {
            stage: 0,
            prefab: 0,
            position: poses[0].position,
        }];
        let legacy = apply_colliders(
            Input {
                height_reservations: Some(&mask),
                layers: &l,
                alpha: &alpha,
                candidates: &candidates,
                stages: std::slice::from_ref(&s),
                poses: &poses,
                prefabs: &prefabs,
                obstacles: &[],
                protected_topology: 8,
            },
            15,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(legacy.height, l.height);
        assert_eq!(legacy.report.counts.accepted, 0);
        assert_eq!(legacy.report.counts.protected, 1);
        assert_eq!(legacy.report.height_reserved_samples, Some(mask.samples()));
        let mut tx = RoadTransaction::new_objects(&l, &alpha, prefabs.clone(), 15, 8, || false)
            .unwrap()
            .unwrap();
        assert!(tx.set_height_reservations(&mask, || false).unwrap());
        assert_eq!(
            tx.try_apply(0, &s, &path, &poses[0], || false).unwrap(),
            Some(false)
        );
        assert_eq!(tx.view().height, l.height);
        assert_eq!(tx.view().topology, l.topology);
        assert_eq!(tx.view().splat, l.splat);
        assert_eq!(
            tx.try_apply(1, &s, &path, &pose(1, 150., 0.), || false)
                .unwrap(),
            Some(true)
        );
        let result = tx.finish(|| false).unwrap().unwrap();
        assert_eq!(result.report.counts.protected, 1);
        assert_eq!(result.report.counts.accepted, 1);
        assert!(result.height.iter().zip(&l.height).any(|(a, b)| a != b));
        for (i, &value) in result.height.iter().enumerate() {
            if mask.contains(i) {
                assert_eq!(value, l.height[i]);
            }
        }
        let mut late = RoadTransaction::new_objects(&l, &alpha, prefabs.clone(), 15, 8, || false)
            .unwrap()
            .unwrap();
        late.try_apply(0, &s, &path, &poses[0], || false).unwrap();
        assert!(late.set_height_reservations(&mask, || false).is_err());
        assert!(late.finish(|| false).is_err());
        let mut cancelled = RoadTransaction::new_objects(&l, &alpha, prefabs, 15, 8, || false)
            .unwrap()
            .unwrap();
        assert!(!cancelled.set_height_reservations(&mask, || true).unwrap());
        assert!(cancelled.finish(|| false).unwrap().is_none());
    }
    fn collider_prefab() -> Prefab {
        let mut p = height_prefab();
        p.contributors = Some(vec![crate::decor_collider::Contributor {
            mesh: crate::height_mesh::Mesh {
                kind: "raw-triangle-mesh".into(),
                enabled: true,
                trigger: false,
                convex: false,
                cooking_options: 30,
                backfaces: false,
                vertices: vec![[-5., 5., -5.], [-5., 5., 5.], [5., 5., -5.], [5., 5., 5.]],
                triangles: vec![[0, 1, 2], [2, 1, 3]],
            },
            local_to_root: [
                [1., 0., 0., 65.],
                [0., 1., 0., 0.],
                [0., 0., 1., 0.],
                [0., 0., 0., 1.],
            ],
        }]);
        p
    }
    #[test]
    fn collider_raises_share_atomic_paint_topology_height_and_blocker_transaction() {
        let mut l = material_layers();
        let n = l.surface_resolution;
        let alpha = vec![255; n * n];
        let result =
            run_height_revision(&l, collider_prefab(), vec![pose(0, 0., 0.)], &alpha, true);
        assert_eq!(result.report.counts.accepted, 1);
        assert!(
            result
                .report
                .collider_effects
                .as_ref()
                .unwrap()
                .changed_samples
                > 0
        );
        assert!(
            result
                .report
                .collider_effects
                .as_ref()
                .unwrap()
                .maximum_raise
                > 4.9
        );
        assert!(
            result
                .report
                .height_effects
                .as_ref()
                .unwrap()
                .changed_samples
                > 0
        );
        assert!(result.report.materials.as_ref().unwrap().changed_cells > 0);
        assert!(result.report.topology.as_ref().unwrap().changed_cells > 0);
        assert!(result.report.blocking.as_ref().unwrap().cells > 0);
        let repeat =
            run_height_revision(&l, collider_prefab(), vec![pose(0, 0., 0.)], &alpha, true);
        assert_eq!(result.height, repeat.height);
        assert_eq!(result.splat, repeat.splat);
        assert_eq!(result.topology, repeat.topology);
        let removed = run_height_revision(&l, collider_prefab(), vec![], &alpha, true);
        assert_eq!(removed.height, l.height);
        assert_eq!(removed.splat.as_ref().unwrap(), &l.splat);
        assert_eq!(removed.topology.as_ref().unwrap(), &l.topology);
        // Protect only the remote collider region: the earlier direct height
        // operations have been prepared but none may leak on rejection.
        l.topology[index(n, 1000., 0.) * n + index(n, 1000., 65.)] = 1;
        let failed =
            run_height_revision(&l, collider_prefab(), vec![pose(0, 0., 0.)], &alpha, true);
        assert_eq!(failed.report.counts.protected, 1);
        assert_eq!(failed.height, l.height);
        assert_eq!(failed.splat.as_ref().unwrap(), &l.splat);
        assert_eq!(failed.topology.as_ref().unwrap(), &l.topology);
        assert_eq!(failed.report.collider_effects.unwrap().changed_samples, 0);
        assert_eq!(failed.report.height_effects.unwrap().changed_samples, 0);
        assert_eq!(failed.report.materials.unwrap().read_locked_cells, 0);
        assert_eq!(failed.report.topology.unwrap().read_locked_cells, 0);
        assert_eq!(failed.report.blocking.unwrap().cells, 0);
    }
    #[test]
    fn later_material_filter_failure_discards_already_prepared_collider_raises() {
        let l = material_layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let mut p = collider_prefab();
        p.topology
            .as_mut()
            .unwrap()
            .queries
            .push(crate::decor_topology::Query {
                check_placement: false,
                position: [80., 0., 0.],
                radius: 0.,
                filter: crate::spawn::Filter {
                    splat_type: 2,
                    biome_type: -1,
                    topology_any: -1,
                    topology_all: 0,
                    topology_not: 0,
                },
            });
        let failed = run_height_revision(&l, p, vec![pose(0, 0., 0.)], &alpha, true);
        assert_eq!(failed.report.counts.material_filter, 1);
        assert_eq!(failed.height, l.height);
        assert_eq!(failed.splat.as_ref().unwrap(), &l.splat);
        assert_eq!(failed.topology.as_ref().unwrap(), &l.topology);
        assert_eq!(failed.report.collider_effects.unwrap().changed_samples, 0);
        assert_eq!(failed.report.blocking.unwrap().cells, 0);
    }
    #[test]
    fn collider_only_prefab_does_not_require_a_fabricated_footprint_or_height_modifier() {
        let l = material_layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let mut p = collider_prefab();
        p.support.clear();
        p.blockers.clear();
        p.materials = Some(vec![]);
        p.height_modifiers = Some(vec![]);
        p.topology.as_mut().unwrap().modifiers.clear();
        let result = run_height_revision(&l, p, vec![pose(0, 0., 0.)], &alpha, true);
        assert_eq!(result.report.counts.accepted, 1);
        assert!(result.report.collider_effects.unwrap().changed_samples > 0);
    }
    #[test]
    fn direct_height_modes_commit_with_paint_and_topology_or_all_roll_back_and_restore() {
        let mut l = material_layers();
        let n = l.surface_resolution;
        let alpha = vec![255; n * n];
        let first = run_height(&l, height_prefab(), vec![pose(0, 0., 0.)], &alpha);
        assert_eq!(first.report.counts.accepted, 1);
        assert!(first.report.height_effects.as_ref().unwrap().maximum_lower > 7.9);
        assert!(first.report.materials.as_ref().unwrap().changed_cells > 0);
        assert!(first.report.topology.as_ref().unwrap().changed_cells > 0);
        assert!(first.report.blocking.as_ref().unwrap().cells > 0);
        let repeat = run_height(&l, height_prefab(), vec![pose(0, 0., 0.)], &alpha);
        assert_eq!(first.height, repeat.height);
        assert_eq!(first.splat, repeat.splat);
        assert_eq!(first.topology, repeat.topology);
        let removed = run_height(&l, height_prefab(), vec![], &alpha);
        assert_eq!(removed.height, l.height);
        assert_eq!(removed.splat.as_ref().unwrap(), &l.splat);
        assert_eq!(removed.topology.as_ref().unwrap(), &l.topology);
        l.topology[index(n, 1000., 0.) * n + index(n, 1000., 35.)] = 1;
        let failed = run_height(&l, height_prefab(), vec![pose(0, 0., 0.)], &alpha);
        assert_eq!(failed.report.counts.protected, 1);
        assert_eq!(failed.height, l.height);
        assert_eq!(failed.splat.as_ref().unwrap(), &l.splat);
        assert_eq!(failed.topology.as_ref().unwrap(), &l.topology);
        assert_eq!(failed.report.height_effects.unwrap().changed_samples, 0);
        assert_eq!(failed.report.materials.unwrap().read_locked_cells, 0);
        assert_eq!(failed.report.blocking.unwrap().cells, 0);
    }
    #[test]
    fn a_later_material_filter_failure_discards_an_already_prepared_height_cut() {
        let l = material_layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let mut p = height_prefab();
        p.topology
            .as_mut()
            .unwrap()
            .queries
            .push(crate::decor_topology::Query {
                check_placement: false,
                position: [80., 0., 0.],
                radius: 0.,
                filter: crate::spawn::Filter {
                    splat_type: 2,
                    biome_type: -1,
                    topology_any: -1,
                    topology_all: 0,
                    topology_not: 0,
                },
            });
        let failed = run_height(&l, p, vec![pose(0, 0., 0.)], &alpha);
        assert_eq!(failed.report.counts.material_filter, 1);
        assert_eq!(failed.height, l.height);
        assert_eq!(failed.splat.as_ref().unwrap(), &l.splat);
        assert_eq!(failed.topology.as_ref().unwrap(), &l.topology);
        assert_eq!(failed.report.height_effects.unwrap().changed_samples, 0);
        assert_eq!(failed.report.materials.unwrap().changed_cells, 0);
        assert_eq!(failed.report.topology.unwrap().changed_cells, 0);
    }
    fn run_material(
        l: &Layers,
        spec: Prefab,
        poses: Vec<decor_fit::Pose>,
        alpha: &[u8],
        cancel: bool,
    ) -> Option<Output> {
        let s = stage();
        let prefabs = BTreeMap::from([(s.prefabs[0].clone(), spec)]);
        let candidates: Vec<_> = poses
            .iter()
            .map(|p| decor::Candidate {
                stage: 0,
                prefab: 0,
                position: p.position,
            })
            .collect();
        apply_materials(
            Input {
                height_reservations: None,
                layers: l,
                alpha,
                candidates: &candidates,
                stages: &[s],
                poses: &poses,
                prefabs: &prefabs,
                obstacles: &[],
                protected_topology: 1,
            },
            15,
            || cancel,
        )
        .unwrap()
    }
    fn material_layers() -> Layers {
        let mut l = layers();
        let cells = l.surface_resolution.pow(2);
        l.splat.fill(0);
        l.splat[..cells].fill(255);
        l
    }
    fn material_prefab() -> Prefab {
        let mut p = topology_prefab();
        p.materials = Some(vec![crate::decor_material::Modifier {
            operation: crate::decor_material::Operation::Set,
            position: [35., 0., 0.],
            radius: 5.,
            fade: 0.,
            opacity: 1.,
            channel: 1,
        }]);
        p
    }
    #[test]
    fn material_height_blocker_and_topology_commit_or_roll_back_together_and_restore_base() {
        let mut l = material_layers();
        let n = l.surface_resolution;
        let cells = n * n;
        let alpha = vec![255; cells];
        let sample = index(n, 1000., 0.) * n + index(n, 1000., 35.);
        let success =
            run_material(&l, material_prefab(), vec![pose(0, 0., 0.)], &alpha, false).unwrap();
        assert_eq!(success.report.counts.accepted, 1);
        assert!(success.report.height_writes > 0);
        assert!(success.report.blocking.as_ref().unwrap().cells > 0);
        assert!(success.report.topology.as_ref().unwrap().changed_cells > 0);
        assert!(success.report.materials.as_ref().unwrap().changed_cells > 0);
        assert_eq!(success.splat.as_ref().unwrap()[cells + sample], 255);
        let repeat =
            run_material(&l, material_prefab(), vec![pose(0, 0., 0.)], &alpha, false).unwrap();
        assert_eq!(success.height, repeat.height);
        assert_eq!(success.splat, repeat.splat);
        assert_eq!(success.topology, repeat.topology);
        let removed = run_material(&l, material_prefab(), vec![], &alpha, false).unwrap();
        assert_eq!(removed.height, l.height);
        assert_eq!(removed.splat.as_ref().unwrap(), &l.splat);
        assert_eq!(removed.topology.as_ref().unwrap(), &l.topology);
        l.topology[sample] = 1;
        let rejected =
            run_material(&l, material_prefab(), vec![pose(0, 0., 0.)], &alpha, false).unwrap();
        assert_eq!(rejected.report.counts.protected, 1);
        assert_eq!(rejected.report.counts.accepted, 0);
        assert_eq!(rejected.height, l.height);
        assert_eq!(rejected.topology.as_ref().unwrap(), &l.topology);
        assert_eq!(rejected.splat.as_ref().unwrap(), &l.splat);
        assert_eq!(rejected.report.blocking.unwrap().cells, 0);
        assert_eq!(rejected.report.materials.unwrap().read_locked_cells, 0);
        assert_eq!(rejected.report.topology.unwrap().read_locked_cells, 0);
        assert!(run_material(&l, material_prefab(), vec![pose(0, 0., 0.)], &alpha, true).is_none());
    }
    #[test]
    fn material_only_modifiers_are_real_effects_and_remote_reads_protect_earlier_filters() {
        let l = material_layers();
        let n = l.surface_resolution;
        let alpha = vec![255; n * n];
        let mut p = material_prefab();
        p.footprint = None;
        p.blockers.clear();
        p.support.clear();
        p.topology.as_mut().unwrap().modifiers.clear();
        let only = run_material(&l, p, vec![pose(0, 0., 0.)], &alpha, false).unwrap();
        assert_eq!(only.report.counts.accepted, 1);
        assert_eq!(only.height, l.height);
        assert!(only.report.materials.unwrap().changed_cells > 0);
        let mut p = material_prefab();
        p.topology
            .as_mut()
            .unwrap()
            .queries
            .push(crate::decor_topology::Query {
                check_placement: false,
                position: [105., 0., 0.],
                radius: 0.,
                filter: crate::spawn::Filter {
                    splat_type: 1,
                    biome_type: -1,
                    topology_any: -1,
                    topology_all: 0,
                    topology_not: 0,
                },
            });
        let first = run_material(&l, p.clone(), vec![pose(0, 0., 0.)], &alpha, false).unwrap();
        let two = run_material(
            &l,
            p,
            vec![pose(0, 0., 0.), pose(1, 70., 0.)],
            &alpha,
            false,
        )
        .unwrap();
        assert_eq!(two.report.counts.accepted, 1);
        assert_eq!(two.report.materials.as_ref().unwrap().read_conflicts, 1);
        assert_eq!(two.height, first.height);
        assert_eq!(two.splat, first.splat);
        assert_eq!(two.topology, first.topology);
    }
    fn layers() -> Layers {
        let (hr, n) = dimensions(1000).unwrap();
        Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: n,
            height: vec![encode(20.); hr * hr],
            biome: vec![51; n * n * 5],
            splat: vec![32; n * n * 8],
            topology: vec![0; n * n],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        }
    }
    fn stage() -> decor::Stage {
        serde_json::from_value(serde_json::json!({"id":"one","order":0,"name":"Rocks","kind":"white_noise","folder":"decor/rocks","enabled":true,"mode":-1,"filter":{"SplatType":-1,"BiomeType":-1,"TopologyAny":-1,"TopologyAll":0,"TopologyNot":0},"filter_mode":1,"anchor_mode":0,"distance":0.,"dithering":0.,"density":10.,"cluster":null,"prefabs":["assets/bundled/prefabs/autospawn/decor/rocks/a.prefab"]})).unwrap()
    }
    fn prefab() -> Prefab {
        Prefab {
            blockers: vec![],
            raises: vec![],
            topology: None,
            materials: None,
            height_modifiers: None,
            contributors: None,
            support: vec![Support {
                position: [0.; 3],
                radius: 0.,
            }],
            footprint: Some(Footprint {
                ring: vec![[-3., 0., -3.], [3., 0., -3.], [3., 0., 3.], [-3., 0., 3.]],
                center: [0.; 3],
                local_to_root: [
                    [1., 0., 0., 0.],
                    [0., 1., 0., 0.],
                    [0., 0., 1., 0.],
                    [0., 0., 0., 1.],
                ],
                fill_offset: 0.,
                max_fill: 4.,
                reject_above_gap: 0.,
                rim_width: 0.,
                feather: 0.,
                opacity: 1.,
                fill_interior: true,
                max_tilt_degrees: 35.,
            }),
            unsupported: vec![],
            pending: vec!["Physics".into()],
        }
    }
    fn pose(candidate: usize, x: f32, z: f32) -> decor_fit::Pose {
        decor_fit::Pose {
            candidate,
            position: [x, 22., z],
            rotation: [0., 0., 0., 1.],
            scale: [1.; 3],
        }
    }
    fn run(
        l: &Layers,
        p: Prefab,
        poses: Vec<decor_fit::Pose>,
        alpha: Vec<u8>,
        obstacles: &[Obstacle],
    ) -> Output {
        run_mode(l, p, poses, alpha, obstacles, false)
    }
    fn run_mode(
        l: &Layers,
        p: Prefab,
        poses: Vec<decor_fit::Pose>,
        alpha: Vec<u8>,
        obstacles: &[Obstacle],
        blocking: bool,
    ) -> Output {
        let s = stage();
        let specs = BTreeMap::from([(s.prefabs[0].clone(), p)]);
        let candidates: Vec<_> = poses
            .iter()
            .map(|p| decor::Candidate {
                stage: 0,
                prefab: 0,
                position: p.position,
            })
            .collect();
        apply_inner(
            Input {
                height_reservations: None,
                layers: l,
                alpha: &alpha,
                candidates: &candidates,
                stages: &[s],
                poses: &poses,
                prefabs: &specs,
                obstacles,
                protected_topology: 1,
            },
            blocking,
            || false,
        )
        .unwrap()
        .unwrap()
    }
    #[test]
    fn rotated_polygons_fill_only_reserve_and_preserve_input_layers() {
        let l = layers();
        let before = l.height.clone();
        let n = l.surface_resolution;
        let mut p = pose(0, 0., 0.);
        let a = 37_f32.to_radians() / 2.;
        p.rotation = [0., a.sin(), 0., a.cos()];
        let out = run(&l, prefab(), vec![p], vec![255; n * n], &[]);
        assert_eq!(out.report.counts.accepted, 1);
        assert!(out.report.height_writes > 0);
        assert!(out.report.reserved_cells > 0);
        assert!(out.height.iter().zip(&before).all(|(a, b)| a >= b));
        assert_eq!(l.height, before);
        assert!(!out.report.physics_validated);
        assert!(out.report.applied[0].maximum_fill <= 4.0001);
        assert_eq!(
            out.report.occupancy.iter().filter(|v| **v == 1).count(),
            out.report.reserved_cells
        );
    }
    #[test]
    fn overlapping_footprints_and_anchor_support_are_reserved_before_later_writes() {
        let l = layers();
        let n = l.surface_resolution;
        let out = run(
            &l,
            prefab(),
            vec![pose(0, 0., 0.), pose(1, 0., 0.)],
            vec![255; n * n],
            &[],
        );
        assert_eq!(out.report.counts.accepted, 1);
        assert_eq!(out.report.counts.occupied, 1);
        let mut p = prefab();
        p.support.push(Support {
            position: [12., 0., 0.],
            radius: 1.,
        });
        let out = run(
            &l,
            p,
            vec![pose(0, 0., 0.), pose(1, 12., 0.)],
            vec![255; n * n],
            &[],
        );
        assert_eq!(out.report.counts.accepted, 1);
        assert_eq!(out.report.counts.occupied, 1);
    }
    #[test]
    fn late_fill_rejection_leaks_neither_height_nor_reservations() {
        let mut l = layers();
        let hr = l.height_resolution;
        let mid = hr / 2;
        l.height[(mid + 2) * hr + mid + 2] = encode(10.);
        let n = l.surface_resolution;
        let out = run(&l, prefab(), vec![pose(0, 0., 0.)], vec![255; n * n], &[]);
        assert_eq!(out.report.counts.fill_limit, 1);
        assert_eq!(out.height, l.height);
        assert_eq!(out.report.reserved_cells, 0);
        assert!(out.report.occupancy.iter().all(|v| *v == 0));
    }
    #[test]
    fn protected_topology_authored_monument_bounds_and_holes_reject_atomically() {
        let mut l = layers();
        let n = l.surface_resolution;
        l.topology[n / 2 * n + n / 2] = 1;
        let out = run(&l, prefab(), vec![pose(0, 0., 0.)], vec![255; n * n], &[]);
        assert_eq!(out.report.counts.protected, 1);
        assert_eq!(out.height, l.height);
        l.topology.fill(0);
        let mut alpha = vec![255; n * n];
        alpha[n / 2 * n + n / 2] = 0;
        let out = run(&l, prefab(), vec![pose(0, 0., 0.)], alpha, &[]);
        assert_eq!(out.report.counts.holes, 1);
        assert_eq!(out.height, l.height);
        let out = run(
            &l,
            prefab(),
            vec![pose(0, 0., 0.)],
            vec![255; n * n],
            &[Obstacle {
                center: [0., 0.],
                extent: [2., 8.],
                yaw: 37.,
            }],
        );
        assert_eq!(out.report.counts.protected, 1);
        assert_eq!(out.height, l.height);
    }
    #[test]
    fn bounds_tilt_and_unsupported_shapes_never_clip_or_fabricate_a_footprint() {
        let l = layers();
        let n = l.surface_resolution;
        let out = run(&l, prefab(), vec![pose(0, 499., 0.)], vec![255; n * n], &[]);
        assert_eq!(out.report.counts.outside, 1);
        let mut p = pose(0, 0., 0.);
        let a = 60_f32.to_radians() / 2.;
        p.rotation = [a.sin(), 0., 0., a.cos()];
        let out = run(&l, prefab(), vec![p], vec![255; n * n], &[]);
        assert_eq!(out.report.counts.tilt, 1);
        let mut p = prefab();
        p.footprint = None;
        p.unsupported.push("No authored ring".into());
        let out = run(&l, p, vec![pose(0, 0., 0.)], vec![255; n * n], &[]);
        assert_eq!(out.report.counts.unsupported, 1);
        assert_eq!(out.height, l.height);
    }
    #[test]
    fn concave_rings_and_nested_component_transforms_use_the_actual_outline() {
        let l = layers();
        let n = l.surface_resolution;
        let hr = l.height_resolution;
        let mut p = prefab();
        let f = p.footprint.as_mut().unwrap();
        f.ring = vec![
            [-4., 0., -4.],
            [4., 0., -4.],
            [4., 0., 0.],
            [0., 0., 0.],
            [0., 0., 4.],
            [-4., 0., 4.],
        ];
        f.center = [-1., 0., -1.];
        f.local_to_root[0][3] = 10.;
        let out = run(&l, p, vec![pose(0, 0., 0.)], vec![255; n * n], &[]);
        assert_eq!(out.report.counts.accepted, 1);
        let hidx = |x: f32, z: f32| {
            let x = ((x / 1000. + 0.5) * (hr - 1) as f32).round() as usize;
            let z = ((z / 1000. + 0.5) * (hr - 1) as f32).round() as usize;
            z * hr + x
        };
        assert!(out.height[hidx(8., 2.)] > l.height[hidx(8., 2.)]);
        assert_eq!(out.height[hidx(12., 2.)], l.height[hidx(12., 2.)]);
        assert_eq!(out.height[hidx(0., 0.)], l.height[hidx(0., 0.)]);
    }
    #[test]
    fn cancelled_or_self_intersecting_inputs_publish_no_partial_revision() {
        let l = layers();
        let n = l.surface_resolution;
        let s = stage();
        let mut p = prefab();
        p.footprint.as_mut().unwrap().ring =
            vec![[-2., 0., -2.], [2., 0., 2.], [-2., 0., 2.], [2., 0., -2.]];
        assert!(p.footprint.as_ref().unwrap().validate().is_err());
        let specs = BTreeMap::from([(s.prefabs[0].clone(), prefab())]);
        let candidates: Vec<_> = (0..10)
            .map(|i| decor::Candidate {
                stage: 0,
                prefab: 0,
                position: [i as f32 * 20., 22., 0.],
            })
            .collect();
        let poses: Vec<_> = (0..10).map(|i| pose(i, i as f32 * 20., 0.)).collect();
        let calls = std::cell::Cell::new(0);
        let result = apply(
            Input {
                height_reservations: None,
                layers: &l,
                alpha: &vec![255; n * n],
                candidates: &candidates,
                stages: &[s],
                poses: &poses,
                prefabs: &specs,
                obstacles: &[],
                protected_topology: 1,
            },
            || {
                calls.set(calls.get() + 1);
                calls.get() > 560
            },
        )
        .unwrap();
        assert!(result.is_none());
        assert!(l.height.iter().all(|h| *h == encode(20.)));
    }
    fn blocked_prefab() -> Prefab {
        let mut p = prefab();
        p.footprint = None;
        p.blockers.push(Blocker {
            position: [0.; 3],
            radius: 12.,
            fade: 4.,
        });
        p
    }
    #[test]
    fn blockers_without_rings_reserve_space_without_filling_terrain() {
        let l = layers();
        let n = l.surface_resolution;
        let out = run_mode(
            &l,
            blocked_prefab(),
            vec![pose(0, 0., 0.), pose(1, 0., 0.)],
            vec![255; n * n],
            &[],
            true,
        );
        assert_eq!(out.report.engine, BLOCKER_VERSION);
        assert_eq!(out.report.counts.accepted, 1);
        assert_eq!(out.report.counts.occupied, 1);
        assert_eq!(out.height, l.height);
        assert_eq!(out.report.height_writes, 0);
        let b = out.report.blocking.as_ref().unwrap();
        assert_eq!(b.instances, 1);
        assert!(b.cells > 0);
        assert_eq!(b.cells, b.raster.iter().filter(|v| **v == 1).count());
        assert!(
            b.raster
                .iter()
                .zip(&out.report.occupancy)
                .all(|(b, o)| *b == 0 || *o == 1)
        );
        assert!(out.report.reserved_cells >= b.cells);
        assert_eq!(out.report.applied[0].blocked_cells, Some(b.cells));
    }
    #[test]
    fn blocker_cell_centers_use_rotated_offsets_y_scale_and_strict_fade_threshold() {
        let l = layers();
        let n = l.surface_resolution;
        let mut spec = blocked_prefab();
        spec.blockers[0].position = [7., 0., 0.];
        let mut p = pose(0, 15., -20.);
        let angle = 90_f32.to_radians() / 2.;
        p.rotation = [0., angle.sin(), 0., angle.cos()];
        p.scale = [3., 2., 4.];
        let out = run_mode(&l, spec, vec![p], vec![255; n * n], &[], true);
        assert_eq!(out.report.counts.accepted, 1);
        let b = out.report.blocking.as_ref().unwrap();
        // X offset becomes -Z, radius and fade scale by Y (not X/Z).
        for z in 0..n {
            for x in 0..n {
                let wx = ((x as f32 + 0.5) / n as f32 - 0.5) * 1000.;
                let wz = ((z as f32 + 0.5) / n as f32 - 0.5) * 1000.;
                let expected = (wx - 15.).hypot(wz + 41.) < 20.;
                assert_eq!(b.raster[z * n + x] != 0, expected);
            }
        }
    }
    #[test]
    fn strict_half_fade_and_zero_radius_never_block_boundary_cells() {
        let l = layers();
        let n = l.surface_resolution;
        let center = (0.5 / n as f32) * 1000.;
        let mut spec = blocked_prefab();
        spec.support.clear();
        spec.blockers[0].radius = 1.;
        spec.blockers[0].fade = 2.; // center is exactly lerp 0.5
        let out = run_mode(
            &l,
            spec.clone(),
            vec![pose(0, center, center)],
            vec![255; n * n],
            &[],
            true,
        );
        assert_eq!(out.report.counts.unsupported, 1);
        assert_eq!(out.report.blocking.unwrap().cells, 0);
        spec.blockers[0].radius = 0.;
        spec.blockers[0].fade = 0.;
        let out = run_mode(
            &l,
            spec,
            vec![pose(0, center, center)],
            vec![255; n * n],
            &[],
            true,
        );
        assert_eq!(out.report.blocking.unwrap().cells, 0);
    }
    #[test]
    fn blockers_and_height_patches_commit_together_or_publish_neither() {
        let mut l = layers();
        let n = l.surface_resolution;
        let hr = l.height_resolution;
        let mut spec = prefab();
        spec.blockers = blocked_prefab().blockers;
        // Rejected late during height inspection, after the blocker patch is prepared.
        l.height[(hr / 2 + 2) * hr + hr / 2 + 2] = encode(10.);
        let out = run_mode(&l, spec, vec![pose(0, 0., 0.)], vec![255; n * n], &[], true);
        assert_eq!(out.report.counts.fill_limit, 1);
        assert_eq!(out.height, l.height);
        assert_eq!(out.report.reserved_cells, 0);
        let b = out.report.blocking.unwrap();
        assert_eq!(b.cells, 0);
        assert_eq!(b.instances, 0);
        assert!(b.raster.iter().all(|b| *b == 0));
        // Protection outside the tiny ring but inside the authored blocker also rejects.
        l.topology[n / 2 * n + n / 2 + 4] = 1;
        let out = run_mode(
            &l,
            blocked_prefab(),
            vec![pose(0, 0., 0.)],
            vec![255; n * n],
            &[],
            true,
        );
        assert_eq!(out.report.counts.protected, 1);
        assert_eq!(out.report.blocking.unwrap().cells, 0);
    }
    #[test]
    fn legacy_footprints_do_not_silently_acquire_blocker_effects_or_fields() {
        let l = layers();
        let n = l.surface_resolution;
        let baseline = run(&l, prefab(), vec![pose(0, 0., 0.)], vec![255; n * n], &[]);
        let mut spec = prefab();
        spec.blockers = blocked_prefab().blockers;
        let out = run(&l, spec, vec![pose(0, 0., 0.)], vec![255; n * n], &[]);
        assert_eq!(
            serde_json::to_value(&baseline.report).unwrap(),
            serde_json::to_value(&out.report).unwrap()
        );
        assert_eq!(baseline.height, out.height);
        assert_eq!(baseline.report.occupancy, out.report.occupancy);
        assert!(
            !serde_json::to_string(&out.report)
                .unwrap()
                .contains("blocked_cells")
        );
        assert!(out.report.blocking.is_none());
    }
    #[test]
    fn authored_height_raises_smooth_opacity_and_overlaps_use_the_same_baseline() {
        let l = layers();
        let n = l.surface_resolution;
        let hr = l.height_resolution;
        let mut spec = blocked_prefab();
        spec.raises = vec![
            Raise {
                position: [0.; 3],
                radius: 12.,
                fade: 4.,
                opacity: 0.5
            };
            2
        ];
        let out = run_mode(&l, spec, vec![pose(0, 0., 0.)], vec![255; n * n], &[], true);
        assert_eq!(out.report.counts.accepted, 1);
        assert!(out.report.height_writes > 0);
        let mut changed = 0;
        for z in 0..hr {
            for x in 0..hr {
                let d = (x as f32 + 0.5 - hr as f32 / 2.).hypot(z as f32 + 0.5 - hr as f32 / 2.);
                let w =
                    ((12. / 1000. * hr as f32 - d) / (4. / 1000. * hr as f32)).clamp(0., 1.) * 0.5;
                let h = decode(l.height[z * hr + x]);
                let expected = encode(h + (22. - h).max(0.) * w * w * (3. - 2. * w));
                assert_eq!(out.height[z * hr + x], expected);
                if expected != l.height[z * hr + x] {
                    changed += 1;
                }
            }
        }
        assert_eq!(changed, out.report.height_writes);
        assert!(out.report.applied[0].maximum_fill < 1.1);
    }
    #[test]
    fn excessive_raise_or_protected_raise_support_discards_blockers_too() {
        let mut l = layers();
        let n = l.surface_resolution;
        let mut spec = blocked_prefab();
        spec.raises = vec![Raise {
            position: [0.; 3],
            radius: 20.,
            fade: 0.,
            opacity: 1.,
        }];
        let mut p = pose(0, 0., 0.);
        p.position[1] = 30.;
        let out = run_mode(&l, spec.clone(), vec![p], vec![255; n * n], &[], true);
        assert_eq!(out.report.counts.fill_limit, 1);
        assert_eq!(out.height, l.height);
        assert_eq!(out.report.blocking.unwrap().cells, 0);
        assert_eq!(out.report.reserved_cells, 0);
        l.topology[n / 2 * n + n / 2 + 9] = 1;
        let out = run_mode(&l, spec, vec![pose(0, 0., 0.)], vec![255; n * n], &[], true);
        assert_eq!(out.report.counts.protected, 1);
        assert_eq!(out.report.blocking.unwrap().cells, 0);
        assert_eq!(out.height, l.height);
    }
    fn run_topology(
        l: &Layers,
        spec: Prefab,
        poses: Vec<decor_fit::Pose>,
        alpha: Vec<u8>,
    ) -> Output {
        let s = stage();
        let specs = BTreeMap::from([(s.prefabs[0].clone(), spec)]);
        let candidates: Vec<_> = poses
            .iter()
            .map(|p| decor::Candidate {
                stage: 0,
                prefab: 0,
                position: p.position,
            })
            .collect();
        apply_topology(
            Input {
                height_reservations: None,
                layers: l,
                alpha: &alpha,
                candidates: &candidates,
                stages: &[s],
                poses: &poses,
                prefabs: &specs,
                obstacles: &[],
                protected_topology: 1,
            },
            15,
            || false,
        )
        .unwrap()
        .unwrap()
    }
    fn topology_prefab() -> Prefab {
        let mut p = prefab();
        p.blockers = blocked_prefab().blockers;
        p.topology = Some(crate::decor_topology::Prefab {
            modifiers: vec![crate::decor_topology::Modifier {
                position: [0.; 3],
                radius: 6.,
                fade: 0.,
                mask: 2,
                operation: crate::decor_topology::Operation::Add,
            }],
            ..Default::default()
        });
        p
    }
    #[test]
    fn topology_height_and_blockers_commit_as_one_reserved_revision() {
        let l = layers();
        let n = l.surface_resolution;
        let out = run_topology(
            &l,
            topology_prefab(),
            vec![pose(0, 0., 0.)],
            vec![255; n * n],
        );
        assert_eq!(out.report.engine, crate::decor_topology::VERSION);
        assert_eq!(out.report.counts.accepted, 1);
        assert!(out.report.height_writes > 0);
        assert!(out.report.blocking.as_ref().unwrap().cells > 0);
        let topo = out.topology.unwrap();
        let changes = topo.iter().zip(&l.topology).filter(|(a, b)| a != b).count();
        assert_eq!(changes, out.report.topology.unwrap().changed_cells);
        assert!(changes > 0);
        for (i, t) in topo.iter().enumerate() {
            if *t != l.topology[i] {
                assert_eq!(out.report.occupancy[i], 1);
                assert_eq!(*t, 2);
            }
        }
        assert!(l.topology.iter().all(|v| *v == 0));
    }
    #[test]
    fn protection_failure_in_a_distant_topology_patch_leaks_no_other_effects() {
        let mut l = layers();
        let n = l.surface_resolution;
        let mut spec = topology_prefab();
        spec.topology.as_mut().unwrap().modifiers[0].position = [25., 0., 0.];
        l.topology[index(n, 1000., 0.) * n + index(n, 1000., 25.)] = 1;
        let out = run_topology(&l, spec, vec![pose(0, 0., 0.)], vec![255; n * n]);
        assert_eq!(out.report.counts.protected, 1);
        assert_eq!(out.height, l.height);
        assert_eq!(out.topology.unwrap(), l.topology);
        assert_eq!(out.report.reserved_cells, 0);
        assert_eq!(out.report.blocking.unwrap().cells, 0);
        assert_eq!(out.report.topology.unwrap().changed_cells, 0);
    }
    #[test]
    fn a_later_topology_write_cannot_invalidate_an_earlier_distant_filter() {
        let l = layers();
        let n = l.surface_resolution;
        let mut spec = topology_prefab();
        spec.topology
            .as_mut()
            .unwrap()
            .queries
            .push(crate::decor_topology::Query {
                check_placement: false,
                position: [60., 0., 0.],
                radius: 0.,
                filter: crate::spawn::Filter {
                    splat_type: -1,
                    biome_type: -1,
                    topology_any: -1,
                    topology_all: 0,
                    topology_not: 2,
                },
            });
        let out = run_topology(
            &l,
            spec,
            vec![pose(0, -30., 0.), pose(1, 30., 0.)],
            vec![255; n * n],
        );
        assert_eq!(out.report.counts.accepted, 1);
        assert_eq!(out.report.counts.occupied, 1);
        assert_eq!(out.report.topology.unwrap().read_conflicts, 1);
        assert_eq!(
            out.topology.unwrap()[index(n, 1000., 0.) * n + index(n, 1000., 30.)],
            0
        );
    }
    #[test]
    fn authored_protection_bits_can_mark_free_ground_but_cannot_rewrite_protected_ground() {
        let mut l = layers();
        let n = l.surface_resolution;
        let mut spec = topology_prefab();
        spec.topology.as_mut().unwrap().modifiers[0].mask = 1;
        let out = run_topology(&l, spec.clone(), vec![pose(0, 0., 0.)], vec![255; n * n]);
        assert_eq!(out.report.counts.accepted, 1);
        assert!(out.topology.unwrap().contains(&1));
        l.topology[index(n, 1000., 0.) * n + index(n, 1000., 0.)] = 1;
        let out = run_topology(&l, spec, vec![pose(0, 0., 0.)], vec![255; n * n]);
        assert_eq!(out.report.counts.protected, 1);
        assert_eq!(out.topology.unwrap(), l.topology);
    }
}


#[cfg(test)]
mod connected_reservation_tests {
    use super::*;
    #[test]
    fn owned_cells_keep_holes_and_original_or_new_protected_roles_guarded() {
        for case in 0..5 {
            let mut alpha = [255; 2];
            let occupancy = [1; 2];
            let mut original = [0; 2];
            let mut current = [0; 2];
            let mut bits = [1u64];
            match case {
                1 => original[0] = 8,
                2 => current[0] = 8,
                3 => alpha[0] = 0,
                4 => bits[0] = 0,
                _ => {}
            }
            let guard = CellGuard {
                alpha: &alpha,
                occupancy: &occupancy,
                connected_reserved: Some(&bits),
                original: &original,
                current: Some(&current),
                protected: 8,
                exception: 0,
                dynamic: false,
            };
            match case {
                0 => assert!(guard.check(0).is_ok()),
                1 | 2 => assert!(matches!(guard.check(0), Err(Reject::Protected))),
                3 => assert!(matches!(guard.check(0), Err(Reject::Holes))),
                _ => assert!(matches!(guard.check(0), Err(Reject::Occupied))),
            }
            assert!(matches!(guard.check(1), Err(Reject::Occupied)));
        }
    }
}
