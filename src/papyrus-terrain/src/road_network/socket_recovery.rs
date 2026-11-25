//! Bounded shared-socket geometry proposals. No painting or final-world acceptance.
use super::*;
use crate::{path_geometry::Path, path_terrain};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
pub const VERSION: &str = "papyrus-road-socket-geometry-1";
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GroundCoverage {
    RoadAndShoulder,
    /// Conservative nominal-width source-Hermite tube. Terrain blend falloff
    /// is not a drivable surface; collision tessellation is audited separately.
    RoadSurface,
}
/// Borrowed final road-stage rasters. Ocean classification may replace only
/// the derived Ocean bit; alpha, ownership and other topology stay pristine.
#[derive(Clone, Copy)]
pub struct ConstructedGround<'a> {
    pub height: &'a [i16],
    pub ocean: Option<&'a crate::ocean::Mask>,
    /// Explicit lower dry-ground bound, supplied by the selected source profile.
    /// Legacy height-only callers keep their independent one-metre reserve.
    pub minimum_dry_height: f32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CostPolicy {
    Required,
    RoutingGuide,
}
#[derive(Clone, Debug, Serialize)]
pub struct CostWitness {
    pub road: usize,
    pub interval: usize,
    pub position: [f32; 3],
    pub owner: Key,
}
#[derive(Clone, Default, Debug, Serialize)]
pub struct CostDiagnostics {
    pub blocked_samples: usize,
    pub witnesses: Vec<CostWitness>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Key {
    pub monument: u64,
    pub connector: i64,
}
#[derive(Clone, Debug, Serialize)]
pub struct Change {
    pub road: usize,
    pub control: usize,
    pub before: [f32; 3],
    pub after: [f32; 3],
}
#[derive(Clone, Debug, Serialize)]
pub struct Audit {
    pub engine: &'static str,
    pub socket: Key,
    pub degrees: f64,
    pub changes: Vec<Change>,
    pub work: usize,
    pub excluded_padding_samples: usize,
    pub routing_ground_checked: bool,
    pub source_costs_applicable: bool,
    pub source_costs_checked: bool,
    pub source_cost_policy: CostPolicy,
    pub source_costs_passed: Option<bool>,
    pub source_cost_diagnostics: CostDiagnostics,
    pub ground_coverage: GroundCoverage,
    pub final_access_validated: bool,
}
pub struct Environment<'a> {
    pub height_origin: f32,
    pub vertical_span: f32,
    pub layers: &'a Layers,
    pub alpha: &'a [u8],
    pub obstacles: &'a [Obstacle],
    pub owners: &'a [Obstacle],
    pub settings: Settings,
    pub ownership: EntrancePolicy,
    pub launch: LaunchPolicy,
    pub costs: Option<&'a [u32]>,
    /// None preserves the historical 257-vertex lattice. Source cell grids
    /// retain their installation-derived spacing and half-cell origin.
    pub cost_lattice: Option<crate::road_costmap::CellLattice>,
    pub water: Option<crate::road_water::Guide<'a>>,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BlockedObstacle {
    pub monument: u64,
    pub center: [f32; 2],
    pub extent: [f32; 2],
    pub yaw: f32,
}
impl From<&Obstacle> for BlockedObstacle {
    fn from(o: &Obstacle) -> Self {
        Self {
            monument: o.monument,
            center: o.center,
            extent: o.extent,
            yaw: o.yaw,
        }
    }
}
#[derive(Debug, PartialEq, Serialize)]
pub enum Failure {
    Invalid(&'static str),
    Rejected(&'static str),
    Budget,
    StripFolded {
        road: usize,
        interval: usize,
        position: [f32; 3],
    },
    GroundBlocked {
        road: usize,
        interval: usize,
        position: [f32; 3],
        radius: f32,
        ground: Option<GroundBlock>,
        #[serde(skip_serializing_if = "Option::is_none")]
        obstacle: Option<Box<BlockedObstacle>>,
        reason: &'static str,
    },
    CostBlocked {
        road: usize,
        interval: usize,
        position: [f32; 3],
        margin: f32,
        owner: Key,
    },
}
pub struct Proposal {
    before: Vec<(Destination, Destination, Vec<[f32; 3]>)>,
    metadata: Vec<u8>,
    points: Vec<Vec<[f32; 3]>>,
    audit: Audit,
}
impl Proposal {
    pub fn audit(&self) -> &Audit {
        &self.audit
    }
    pub fn controls(&self) -> &[Vec<[f32; 3]>] {
        &self.points
    }
    /// Atomic geometry-only handoff. Callers must discard prior roadside
    /// selections, reselect/repaint, and pass all final source/world gates.
    pub fn apply(
        self,
        report: &mut Report,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Audit>, Failure> {
        if cancelled() {
            return Ok(None);
        }
        if report.surfaces.is_some()
            || report.socket_geometry_recovery.len() >= 128
            || report.roads.len() != self.before.len()
            || metadata(report)? != self.metadata
        {
            return Err(Failure::Invalid("stale socket proposal metadata"));
        }
        for (road, (from, to, points)) in report.roads.iter().zip(&self.before) {
            if cancelled() {
                return Ok(None);
            }
            if road.from != *from || road.to != *to || !same_points(&road.points, points) {
                return Err(Failure::Invalid("stale socket proposal controls"));
            }
        }
        if cancelled() {
            return Ok(None);
        }
        let audit = self.audit.clone();
        let mut audits = report.socket_geometry_recovery.clone();
        audits.push(audit.clone());
        if cancelled() {
            return Ok(None);
        }
        // Every check/allocation above precedes the first mutation.
        for (road, points) in report.roads.iter_mut().zip(self.points) {
            road.points = points;
        }
        for join in &mut report.entrance_joins {
            if (join.monument, join.connector)
                != (self.audit.socket.monument, self.audit.socket.connector)
            {
                continue;
            }
            for p in &mut join.points {
                if let Some(change) = self.audit.changes.iter().find(|c| same_xz(*p, c.before)) {
                    *p = change.after;
                }
            }
        }
        report.socket_geometry_recovery = audits;
        Ok(Some(audit))
    }
}
fn metadata(report: &Report) -> Result<Vec<u8>, Failure> {
    serde_json::to_vec(&(
        &report.families,
        report.entrance_launch,
        report.entrance_policy,
    ))
    .map_err(|_| Failure::Invalid("socket proposal metadata"))
}
fn same_xz(a: [f32; 3], b: [f32; 3]) -> bool {
    a[0].to_bits() == b[0].to_bits() && a[2].to_bits() == b[2].to_bits()
}
fn same_points(a: &[[f32; 3]], b: &[[f32; 3]]) -> bool {
    a.len() == b.len()
        && a.iter()
            .flatten()
            .zip(b.iter().flatten())
            .all(|(a, b)| a.to_bits() == b.to_bits())
}
struct Meter<'a, F: Fn() -> bool> {
    work: usize,
    maximum: usize,
    cancel: &'a F,
}
impl<F: Fn() -> bool> Meter<'_, F> {
    fn spend(&mut self, n: usize) -> Result<bool, Failure> {
        if (self.cancel)() {
            return Ok(false);
        }
        self.work = self
            .work
            .checked_add(n)
            .filter(|n| *n <= self.maximum)
            .ok_or(Failure::Budget)?;
        Ok(true)
    }
}
fn owned(e: &Environment<'_>, key: Key, p: [f32; 2]) -> bool {
    e.owners
        .iter()
        .any(|o| o.monument == key.monument && inside(o, p[0], p[1], 0.))
        && !e
            .owners
            .iter()
            .any(|o| o.monument != key.monument && inside(o, p[0], p[1], 0.))
}
/// Certify samples outside the affected source-Hermite tubes using convex
/// hull subdivision. Unresolved cases remain blocked; f32 roundoff is padded.
struct Footprint {
    curves: Vec<[[f64; 2]; 4]>,
    radius: f64,
    spatial: Option<BTreeMap<(i32, i32), Vec<usize>>>,
    build_work: usize,
}
impl Footprint {
    fn new(path: &Path, affected: &[bool], radius: f32) -> Self {
        let mut curves = Vec::new();
        for i in 0..path.points().len() - 1 {
            if !affected[i.saturating_sub(1)..(i + 3).min(affected.len())]
                .iter()
                .any(|v| *v)
            {
                continue;
            }
            let [a, b] = [path.points()[i], path.points()[i + 1]]
                .map(|p| [f64::from(p[0]), f64::from(p[2])]);
            let [ta, tb] = [path.tangents()[i], path.tangents()[i + 1]].map(|t| {
                [
                    f64::from(t[0] * path.step_size()),
                    f64::from(t[2] * path.step_size()),
                ]
            });
            curves.push([
                a,
                std::array::from_fn(|k| a[k] + ta[k] / 3.),
                std::array::from_fn(|k| b[k] - tb[k] / 3.),
                b,
            ]);
        }
        let largest = path
            .points()
            .iter()
            .flat_map(|p| [p[0].abs(), p[2].abs()])
            .fold(0_f32, f32::max);
        let guard =
            f64::from(f32::EPSILON) * f64::from(largest + path.step_size() + radius + 1.) * 64.;
        let radius = f64::from(radius) + guard;
        let mut spatial = BTreeMap::<(i32, i32), Vec<usize>>::new();
        let mut entries = 0_usize;
        let mut build_work = curves.len() * 8;
        let mut indexed = true;
        'index: for (i, curve) in curves.iter().enumerate() {
            let lo = std::array::from_fn::<_, 2, _>(|k| {
                ((curve.iter().map(|p| p[k]).fold(f64::INFINITY, f64::min) - radius) / 32.).floor()
                    as i32
            });
            let hi = std::array::from_fn::<_, 2, _>(|k| {
                ((curve.iter().map(|p| p[k]).fold(f64::NEG_INFINITY, f64::max) + radius) / 32.)
                    .floor() as i32
            });
            for z in lo[1]..=hi[1] {
                for x in lo[0]..=hi[0] {
                    if entries == 262144 {
                        indexed = false;
                        break 'index;
                    }
                    spatial.entry((x, z)).or_default().push(i);
                    entries += 1;
                    build_work += 1;
                }
            }
        }
        Self {
            curves,
            radius,
            spatial: indexed.then_some(spatial),
            build_work,
        }
    }
    fn touches(
        &self,
        point: [f32; 2],
        work: &mut usize,
        maximum: usize,
        cancel: &impl Fn() -> bool,
    ) -> Result<Option<bool>, Failure> {
        if cancel() {
            return Ok(None);
        }
        let q = point.map(f64::from);
        let radius2 = self.radius * self.radius;
        // A Bezier curve is inside its control hull. Only buckets intersected
        // by that hull expanded by the same padded radius can contain a hit.
        let candidates = self.spatial.as_ref().map(|spatial| {
            let key = ((q[0] / 32.).floor() as i32, (q[1] / 32.).floor() as i32);
            spatial.get(&key).map_or(&[][..], Vec::as_slice)
        });
        // Retain the original last-in-first-out traversal and logical charges.
        for i in (0..candidates.map_or(self.curves.len(), |indices| indices.len())).rev() {
            let curve = self.curves[candidates.map_or(i, |indices| indices[i])];
            match curve_touches(curve, 0, q, radius2, work, maximum, cancel)? {
                Some(false) => {}
                result => return Ok(result),
            }
        }
        Ok(Some(false))
    }
}

fn curve_touches(
    p: [[f64; 2]; 4],
    depth: u8,
    q: [f64; 2],
    radius2: f64,
    work: &mut usize,
    maximum: usize,
    cancel: &impl Fn() -> bool,
) -> Result<Option<bool>, Failure> {
    if cancel() {
        return Ok(None);
    }
    *work = work.checked_add(1).filter(|&w| w <= maximum).ok_or(Failure::Budget)?;
    let lo = std::array::from_fn::<_, 2, _>(|k| {
        p.iter().map(|v| v[k]).fold(f64::INFINITY, f64::min)
    });
    let hi = std::array::from_fn::<_, 2, _>(|k| {
        p.iter().map(|v| v[k]).fold(f64::NEG_INFINITY, f64::max)
    });
    let distance = (q[0] - q[0].clamp(lo[0], hi[0])).powi(2)
        + (q[1] - q[1].clamp(lo[1], hi[1])).powi(2);
    if distance > radius2 {
        return Ok(Some(false));
    }
    if depth == 15
        || [p[0], p[3]]
            .iter()
            .any(|v| (v[0] - q[0]).powi(2) + (v[1] - q[1]).powi(2) <= radius2)
    {
        return Ok(Some(true));
    }
    let middle = |a: [f64; 2], b: [f64; 2]| std::array::from_fn(|k| (a[k] + b[k]) * 0.5);
    let a = middle(p[0], p[1]);
    let b = middle(p[1], p[2]);
    let c = middle(p[2], p[3]);
    let d = middle(a, b);
    let e = middle(b, c);
    let f = middle(d, e);
    match curve_touches([p[0], a, d, f], depth + 1, q, radius2, work, maximum, cancel)? {
        Some(false) => curve_touches([f, e, c, p[3]], depth + 1, q, radius2, work, maximum, cancel),
        result => Ok(result),
    }
}
/// Allow a source-blocked cell only in the same ownership exception used by
/// checked launch generation. Unrelated source cells remain blocked, including
/// closed-cell corner contacts and a margin covering unsampled curve motion.
fn valid_cost_grid(e: &Environment<'_>) -> bool {
    if e.cost_lattice
        .is_some_and(|l| l.size() != e.layers.size as f32)
        || (e.cost_lattice.is_some() && e.costs.is_none())
    {
        return false;
    }
    let n = e.cost_lattice.map_or(GRID, |l| l.resolution());
    e.costs
        .is_none_or(|c| c.len() == n * n && c.iter().all(|&v| v <= 1_000_000))
}
fn costs_clear(
    e: &Environment<'_>,
    key: Key,
    p: [f32; 2],
    margin: f32,
    mut checked_exit: impl FnMut([f32; 2]) -> Result<Option<bool>, Failure>,
) -> Result<Option<bool>, Failure> {
    let Some(costs) = e.costs else {
        return Ok(Some(true));
    };
    let (n, cell, q, offset) = if let Some(lattice) = e.cost_lattice {
        let n = lattice.resolution();
        let q = p.map(|v| {
            f64::from(((v + lattice.size() * 0.5) * (1. / lattice.size())) * n as f32) - 0.5
        });
        (n, f64::from(lattice.size()) / n as f64, q, 0.5)
    } else {
        (
            GRID,
            e.layers.size as f64 / (GRID - 1) as f64,
            p.map(|v| (f64::from(v) / f64::from(e.layers.size) + 0.5) * (GRID - 1) as f64),
            0.,
        )
    };
    let r = f64::from(margin) / cell;
    let low = q.map(|v| (v - r - 0.5).ceil().clamp(0., (n - 1) as f64) as usize);
    let high = q.map(|v| (v + r + 0.5).floor().clamp(0., (n - 1) as f64) as usize);
    for z in low[1]..=high[1] {
        for x in low[0]..=high[0] {
            if costs[z * n + x] != 0 {
                continue;
            }
            let nearest = [
                q[0].clamp(x as f64 - 0.5, x as f64 + 0.5),
                q[1].clamp(z as f64 - 0.5, z as f64 + 0.5),
            ];
            if (nearest[0] - q[0]).hypot(nearest[1] - q[1]) > r {
                continue;
            }
            let world =
                nearest.map(|v| ((v + offset) * cell - f64::from(e.layers.size) * 0.5) as f32);
            if !owned(e, key, world) {
                match checked_exit(world)? {
                    Some(true) => {}
                    Some(false) => return Ok(Some(false)),
                    None => return Ok(None),
                }
            }
        }
    }
    Ok(Some(true))
}
/// Source exit search can traverse zero-cost cells along its checked launch
/// chain. Preserve only exact original endpoint prefixes/suffixes, never a
/// whole monument neighbourhood or a newly drawn detour. Native ground, holes
/// and foreign owners are checked before this cost-only exception.
#[allow(clippy::too_many_arguments)]
fn checked_exit_cost(
    report: &Report,
    road: usize,
    key: Key,
    point: [f32; 2],
    cell: [f32; 2],
    radius: f32,
    meter: &mut Meter<'_, impl Fn() -> bool>,
) -> Result<Option<bool>, Failure> {
    let original = &report.roads[road];
    let destination = Destination::Monument {
        monument: key.monument,
        connector: key.connector,
    };
    for join in &report.entrance_joins {
        if join.monument != key.monument
            || join.connector != key.connector
            || join.points.len() < 2
            || join.points.len() > original.points.len()
        {
            continue;
        }
        let matches = |from: bool| {
            join.points.iter().enumerate().all(|(i, p)| {
                same_xz(
                    *p,
                    original.points[if from {
                        i
                    } else {
                        original.points.len() - 1 - i
                    }],
                )
            })
        };
        if !(original.from == destination && matches(true)
            || original.to == destination && matches(false))
        {
            continue;
        }
        let near = |q: [f32; 2], a: [f32; 3], b: [f32; 3]| {
            let a = [f64::from(a[0]), f64::from(a[2])];
            let b = [f64::from(b[0]), f64::from(b[2])];
            let q = q.map(f64::from);
            let d = [b[0] - a[0], b[1] - a[1]];
            let norm = d[0] * d[0] + d[1] * d[1];
            if norm <= 0. {
                return false;
            }
            let t = (((q[0] - a[0]) * d[0] + (q[1] - a[1]) * d[1]) / norm).clamp(0., 1.);
            (q[0] - a[0] - t * d[0]).hypot(q[1] - a[1] - t * d[1]) <= f64::from(radius)
        };
        let (mut point_near, mut cell_near) = (false, false);
        for segment in join.points.windows(2) {
            if !meter.spend(2)? {
                return Ok(None);
            }
            point_near |= near(point, segment[0], segment[1]);
            cell_near |= near(cell, segment[0], segment[1]);
            if point_near && cell_near {
                return Ok(Some(true));
            }
        }
    }
    Ok(Some(false))
}
/// Controls forming one straight launch, including subdivisions of that run.
fn departure_controls(
    points: &[[f32; 3]],
    side: usize,
    required_run: f32,
    spacing: f32,
) -> Result<Vec<usize>, Failure> {
    if !(3..=65536).contains(&points.len())
        || side > 1
        || !required_run.is_finite()
        || required_run <= 0.
        || !spacing.is_finite()
        || spacing <= 0.
        || points.iter().flatten().any(|v| !v.is_finite())
    {
        return Err(Failure::Invalid("socket departure controls"));
    }
    let at = |i| if side == 0 { i } else { points.len() - 1 - i };
    let pivot = points[at(0)];
    let first = points[at(1)];
    let dx = f64::from(first[0]) - f64::from(pivot[0]);
    let dz = f64::from(first[2]) - f64::from(pivot[2]);
    let run = dx.hypot(dz);
    if run <= 1e-4 {
        return Err(Failure::Rejected("degenerate socket departure"));
    }
    let mut indices = Vec::new();
    let mut previous = 0.;
    // Leave the opposite endpoint fixed. A subdivided straight launch is one
    // departure, not a reason to bypass its original minimum-run requirement.
    for offset in 1..points.len() - 1 {
        let i = at(offset);
        let x = f64::from(points[i][0]) - f64::from(pivot[0]);
        let z = f64::from(points[i][2]) - f64::from(pivot[2]);
        let along = (x * dx + z * dz) / run;
        let cross = (x * dz - z * dx).abs() / run;
        if along <= previous || cross > f64::from(spacing) * 0.25 {
            return Err(Failure::Rejected(
                "socket departure bends before its minimum run",
            ));
        }
        indices.push(i);
        if x.hypot(z) >= f64::from(required_run) {
            return Ok(indices);
        }
        previous = along;
    }
    Err(Failure::Rejected(
        "socket departure is shorter than its minimum run",
    ))
}
/// Deterministic two-sided fan scaled by actual launch runs and incident widths.
pub fn angles(report: &Report, key: Key) -> Result<Vec<f64>, Failure> {
    let launch = report
        .entrance_launch
        .ok_or(Failure::Invalid("socket fan launch policy"))?;
    launch
        .validate()
        .map_err(|_| Failure::Invalid("socket fan launch policy"))?;
    let mut maximum_run = 0_f64;
    let mut minimum_width = f64::INFINITY;
    for (ri, road) in report.roads.iter().enumerate() {
        for (side, d) in [road.from, road.to].into_iter().enumerate() {
            if d != (Destination::Monument {
                monument: key.monument,
                connector: key.connector,
            }) {
                continue;
            }
            if road.points.len() < 3 {
                return Err(Failure::Rejected(
                    "socket road has no free departure control",
                ));
            }
            let families = report
                .families
                .iter()
                .filter(|f| ri >= f.first_road && ri < f.first_road.saturating_add(f.road_count))
                .collect::<Vec<_>>();
            if families.len() != 1 || !families[0].width.is_finite() || families[0].width <= 0. {
                return Err(Failure::Invalid("socket fan family"));
            }
            let controls = departure_controls(
                &road.points,
                side,
                families[0].width * launch.minimum_run_widths,
                launch.sample_spacing,
            )?;
            let a = road.points[if side == 0 { 0 } else { road.points.len() - 1 }];
            let b = road.points[*controls.last().unwrap()];
            maximum_run = maximum_run
                .max((f64::from(a[0]) - f64::from(b[0])).hypot(f64::from(a[2]) - f64::from(b[2])));
            minimum_width = minimum_width.min(f64::from(families[0].width));
        }
    }
    if !maximum_run.is_finite() || maximum_run <= 0. || !minimum_width.is_finite() {
        return Err(Failure::Rejected("unknown or degenerate socket fan"));
    }
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    // Cap the displacement before taking fractions, so short departures do
    // not collapse every candidate to the same +/-10 degree pair.
    let displacement = (minimum_width * 0.5).min(2. * maximum_run * 5_f64.to_radians().sin());
    for fraction in [0.5, 0.9, 1.] {
        let angle = (2.
            * (displacement * fraction / (2. * maximum_run))
                .min(1.)
                .asin())
        .to_degrees()
        .min(10.);
        for degrees in [-angle, angle] {
            if seen.insert(degrees.to_bits()) {
                result.push(degrees);
            }
        }
    }
    Ok(result)
}
/// Lazy per-height-row index of cells that might need the exact footprint check.
/// Water and ownership can exempt a candidate, so those decisions stay in the
/// ordered scan that records the first witness and charges geometry work.
struct GroundRows<'a> {
    ground: ConstructedGround<'a>,
    layers: &'a Layers,
    alpha: &'a [u8],
    axes: &'a [(f32, usize)],
    protected: i32,
    maximum_low_height: i32,
    stride: usize,
    words: Vec<u32>,
    // One bit per word distinguishes a cached empty mask from an unread tile.
    initialized: Vec<u64>,
}

impl<'a> GroundRows<'a> {
    fn new(
        ground: ConstructedGround<'a>,
        layers: &'a Layers,
        alpha: &'a [u8],
        axes: &'a [(f32, usize)],
        protected: i32,
    ) -> Self {
        let stride = axes.len().div_ceil(32);
        let words = axes.len() * stride;
        // Find the last encoded height that satisfies the original inclusive
        // comparison. Inverting the floating-point formula can round across
        // the boundary for some dry-height settings.
        let mut low = i32::from(i16::MIN);
        let mut high = i32::from(i16::MAX) + 1;
        while low < high {
            let mid = low + (high - low) / 2;
            if decode(mid as i16) <= ground.minimum_dry_height {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        Self {
            ground,
            layers,
            alpha,
            axes,
            protected,
            maximum_low_height: low - 1,
            stride,
            words: vec![0; words],
            initialized: vec![0; words.div_ceil(64)],
        }
    }

    fn blocking_word(&mut self, z: usize, tile: usize) -> u32 {
        let index = z * self.stride + tile;
        let word = &mut self.words[index];
        let initialized = &mut self.initialized[index / 64];
        let flag = 1 << (index % 64);
        if *initialized & flag == 0 {
            let n = self.axes.len();
            let surface_row = self.axes[z].1 * self.layers.surface_resolution;
            let start = tile * 32;
            let end = (start + 32).min(n);
            let height = &self.ground.height[z * n + start..z * n + end];
            let mut bits = 0;
            for (&height, &(_, sx)) in height.iter().zip(&self.axes[start..end]).rev() {
                let i = surface_row + sx;
                let blocked = i32::from(height) <= self.maximum_low_height
                    || self.alpha[i] < 128
                    || ground_topology(self.layers.topology[i], self.ground.ocean, i)
                        & self.protected != 0;
                bits = (bits << 1) | u32::from(blocked);
            }
            *word = bits;
            *initialized |= flag;
        }
        *word
    }
}

#[allow(clippy::too_many_arguments)]
fn check_paths<'a>(
    report: &Report,
    e: &Environment<'_>,
    key: Key,
    points: &[Vec<[f32; 3]>],
    affected_roads: impl IntoIterator<Item = (&'a usize, &'a Vec<bool>)>,
    full_routes: bool,
    ground_coverage: GroundCoverage,
    constructed_height: Option<ConstructedGround<'_>>,
    cost_policy: CostPolicy,
    cost_diagnostics: &mut CostDiagnostics,
    mut ground_feedback: Option<&mut GroundFeedback>,
    meter: &mut Meter<'_, impl Fn() -> bool>,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<usize>, Failure> {
    let mut excluded_padding_samples = 0;
    let obstacle_rotations = e
        .obstacles
        .iter()
        .map(|o| o.yaw.to_radians().sin_cos())
        .collect::<Vec<_>>();
    // Route probes repeatedly scan the same native grid axes.
    let ground_axes: Vec<_> = if (2..=4097).contains(&e.layers.height_resolution)
        && (1..=2048).contains(&e.layers.surface_resolution)
    {
        (0..e.layers.height_resolution)
            .map(|index| ground_axis(e.layers, index))
            .collect()
    } else {
        Vec::new()
    };
    let ground_rasters = constructed_height.unwrap_or(ConstructedGround {
        height: &e.layers.height,
        ocean: None,
        minimum_dry_height: 1.,
    });
    let mut ground_rows = (full_routes && !ground_axes.is_empty()).then(|| {
        GroundRows::new(
            ground_rasters,
            e.layers,
            e.alpha,
            &ground_axes,
            e.settings.protected_topology,
        )
    });
    for (&ri, affected) in affected_roads {
        let family = report
            .families
            .iter()
            .find(|f| ri >= f.first_road && ri < f.first_road + f.road_count)
            .unwrap();
        let settings = Settings {
            width: family.width,
            ..e.settings
        };
        let Some(path) = Path::from_controls(
            &points[ri],
            meter
                .maximum
                .checked_sub(meter.work)
                .filter(|&left| left > 0)
                .ok_or(Failure::Budget)?
                .min(32_000_000),
            cancelled,
        )
        .map_err(|error| match error {
            crate::path_geometry::Failure::Budget => Failure::Budget,
            crate::path_geometry::Failure::Invalid => Failure::Invalid("socket path inputs"),
        })?
        else {
            return Ok(None);
        };
        if !meter.spend(path.work())? {
            return Ok(None);
        }
        let mut fold = None;
        let strip = crate::road_strip::check_with_witness(
            &path,
            affected,
            family.width * 0.5,
            |n| meter.spend(n).map(|_| ()),
            cancelled,
            |interval, position| fold = Some((interval, position)),
        )?;
        if strip.is_none() {
            return Ok(None);
        }
        if strip == Some(false) {
            if let Some((interval, position)) = fold {
                return Err(Failure::StripFolded {
                    road: ri,
                    interval,
                    position,
                });
            }
            return Err(Failure::Rejected("socket strip folds"));
        }
        let footprint = Footprint::new(
            &path,
            affected,
            family.width * 0.5
                + match ground_coverage {
                    GroundCoverage::RoadAndShoulder => settings.shoulder,
                    GroundCoverage::RoadSurface => 0.,
                },
        );
        if !meter.spend(footprint.build_work)? {
            return Ok(None);
        }
        let mut footprint_cache = BTreeMap::new();
        let step = path.length() / (path.points().len() - 1) as f32;
        let obstacle_padding = family.width * 0.5 + e.launch.sample_spacing;
        let radius = family.width * 0.5 + settings.shoulder + e.launch.sample_spacing;
        let cell = e.layers.size as f32 / (e.layers.height_resolution - 1) as f32;
        let visit_cost = ((radius * 2. / cell).ceil() as usize + 4).pow(2) + e.obstacles.len() + 16;
        'intervals: for i in 0..points[ri].len() - 1 {
            if !affected[i.saturating_sub(1)..(i + 3).min(affected.len())]
                .iter()
                .any(|v| *v)
            {
                continue;
            }
            // Conservative Hermite speed bound: source distance is uniform in
            // control indices, not physical arc length, especially at sockets.
            let run = distance(points[ri][i], points[ri][i + 1]);
            let count = ((1.5 * run + 2. * step) / e.launch.sample_spacing)
                .ceil()
                .max(2.) as usize;
            if count > 65536 {
                return Err(Failure::Budget);
            }
            for sample in 0..=count {
                if !meter.spend(visit_cost)? {
                    return Ok(None);
                }
                let p = path
                    .point((i as f32 + sample as f32 / count as f32) * step)
                    .map_err(|_| Failure::Rejected("invalid socket curve"))?;
                let owner = if full_routes {
                    [report.roads[ri].from, report.roads[ri].to]
                        .into_iter()
                        .enumerate()
                        .filter_map(|(side, d)| match d {
                            Destination::Monument {
                                monument,
                                connector,
                            } => {
                                let endpoint = if side == 0 {
                                    points[ri][0]
                                } else {
                                    *points[ri].last().unwrap()
                                };
                                Some((
                                    distance(p, endpoint),
                                    Key {
                                        monument,
                                        connector,
                                    },
                                ))
                            }
                            _ => None,
                        })
                        .min_by(|a, b| a.0.total_cmp(&b.0))
                        .map(|(_, key)| key)
                        .unwrap_or(key)
                } else {
                    key
                };
                let mut ground = None;
                let mut obstacle = None;
                let mut geometry_work = 0;
                let mut geometry_error = None;
                let mut geometry_cancelled = false;
                let geometry_budget = meter.maximum.saturating_sub(meter.work);
                let reason = if p[0].abs() + radius > e.layers.size as f32 * 0.5
                    || p[2].abs() + radius > e.layers.size as f32 * 0.5
                {
                    Some("world boundary")
                } else if let Some((blocked, _)) = e
                    .obstacles
                    .iter()
                    .zip(&obstacle_rotations)
                    .find(|&(o, &rotation)| {
                        o.monument != owner.monument
                            && inside_with_rotation(o, p[0], p[2], obstacle_padding, rotation)
                    })
                {
                    obstacle = Some(Box::new(BlockedObstacle::from(blocked)));
                    Some("foreign monument obstacle")
                } else if !ground_clear_with_water_filter_on_height(
                    e.layers,
                    ground_rasters.height,
                    ground_rasters.ocean,
                    ground_rasters.minimum_dry_height,
                    e.alpha,
                    settings,
                    [p[0], p[2]],
                    radius,
                    Some((e.ownership, e.owners, Some(owner.monument))),
                    e.water,
                    |index| {
                        ground_axes
                            .get(index)
                            .copied()
                            .unwrap_or_else(|| ground_axis(e.layers, index))
                    },
                    ground_rows.as_mut().map(|rows| move |z, tile| rows.blocking_word(z, tile)),
                    |b| {
                        if geometry_work >= geometry_budget {
                            geometry_error = Some(Failure::Budget);
                            return true;
                        }
                        geometry_work += 1;
                        let key = (b.position[0].to_bits(), b.position[2].to_bits());
                        let touch = if let Some(&hit) = footprint_cache.get(&key) {
                            Ok(Some(hit))
                        } else {
                            let hit = footprint.touches(
                                [b.position[0], b.position[2]],
                                &mut geometry_work,
                                geometry_budget,
                                cancelled,
                            );
                            if let Ok(Some(hit)) = hit
                                && footprint_cache.len() < 65536
                            {
                                footprint_cache.insert(key, hit);
                            }
                            hit
                        };
                        match touch {
                            Ok(Some(false)) => {
                                excluded_padding_samples += 1;
                                false
                            }
                            Ok(Some(true)) => {
                                ground = Some(b);
                                true
                            }
                            Ok(None) => {
                                geometry_cancelled = true;
                                true
                            }
                            Err(e) => {
                                geometry_error = Some(e);
                                true
                            }
                        }
                    },
                ) {
                    Some("native ground footprint")
                } else {
                    None
                };
                if geometry_cancelled {
                    return Ok(None);
                }
                if let Some(error) = geometry_error {
                    return Err(error);
                }
                if !meter.spend(geometry_work)? {
                    return Ok(None);
                }
                if let Some(reason) = reason {
                    let error = Failure::GroundBlocked {
                        road: ri,
                        interval: i,
                        position: p,
                        radius,
                        ground,
                        obstacle,
                        reason,
                    };
                    if constructed_height.is_some()
                        && family.hierarchy == 2
                        && blocked_ground_road(&error).is_some()
                        && let Some(feedback) = ground_feedback.as_deref_mut()
                    {
                        if feedback.record(error, meter) {
                            break 'intervals;
                        }
                        // Move to the next curve interval so repeated probes
                        // of the same patch leave work for other crossings.
                        continue 'intervals;
                    }
                    return Err(error);
                }
                let cost_clear =
                    costs_clear(e, owner, [p[0], p[2]], e.launch.sample_spacing, |world| {
                        if key.monument != 0 {
                            return Ok(Some(false));
                        }
                        checked_exit_cost(
                            report,
                            ri,
                            owner,
                            [p[0], p[2]],
                            world,
                            e.launch.sample_spacing,
                            meter,
                        )
                    })?;
                let Some(cost_clear) = cost_clear else {
                    return Ok(None);
                };
                if !cost_clear {
                    if cost_policy == CostPolicy::RoutingGuide {
                        if !meter.spend(8)? {
                            return Ok(None);
                        }
                        cost_diagnostics.blocked_samples += 1;
                        if cost_diagnostics.witnesses.len() < 8
                            && !cost_diagnostics
                                .witnesses
                                .iter()
                                .any(|w| w.road == ri && w.interval == i)
                        {
                            cost_diagnostics.witnesses.push(CostWitness {
                                road: ri,
                                interval: i,
                                position: p,
                                owner,
                            });
                        }
                        continue;
                    }
                    if key.monument == 0 {
                        return Err(Failure::CostBlocked {
                            road: ri,
                            interval: i,
                            position: p,
                            margin: e.launch.sample_spacing,
                            owner,
                        });
                    }
                    return Err(Failure::Rejected("socket source cost corridor is blocked"));
                }
            }
        }
        if ground_feedback
            .as_ref()
            .is_some_and(|feedback| feedback.first.is_some())
        {
            break;
        }
    }
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(excluded_padding_samples))
}
#[derive(Debug, Serialize)]
pub struct RouteAudit {
    pub engine: &'static str,
    pub socket: Key,
    pub roads: Vec<usize>,
    pub work: usize,
    pub excluded_padding_samples: usize,
    pub ground_coverage: GroundCoverage,
    pub constructed_ground_checked: bool,
    pub constructed_ocean_checked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constructed_minimum_dry_height: Option<f32>,
    pub source_cost_policy: CostPolicy,
    pub source_costs_passed: Option<bool>,
    pub source_cost_diagnostics: CostDiagnostics,
    pub source_costs_applicable: bool,
    pub whole_incident_routes_checked: bool,
    pub runtime_access_validated: bool,
}
/// Recheck complete final incident paths, including intervals remote from the
/// moved control. Uses original ground, holes, owners and optional source costs.
/// Does not certify newly stamped monuments, decoration or runtime physics.
#[derive(Debug, Serialize)]
pub struct RouteFailure {
    pub work: usize,
    pub measured_work: usize,
    pub error: Failure,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub additional_blocked_ground: Vec<Failure>,
}
// Limit follow-up sampling to a small share of the route-family allowance.
const GROUND_FEEDBACK_FOLLOW_UP_WORK: usize = 8_000_000;
const GROUND_FEEDBACK_FOLLOW_UP_LIMIT: usize = 8;
const GROUND_FEEDBACK_PER_ROAD_WORK: usize = 2_000_000;
const GROUND_FEEDBACK_PER_ROAD_LIMIT: usize = 3;

#[derive(Default)]
struct GroundFeedback {
    first: Option<Failure>,
    additional: Vec<Failure>,
    follow_up_limit: usize,
}

impl GroundFeedback {
    fn record(&mut self, error: Failure, meter: &mut Meter<'_, impl Fn() -> bool>) -> bool {
        let Some(first) = &self.first else {
            self.first = Some(error);
            self.follow_up_limit = meter.maximum.min(meter.work + GROUND_FEEDBACK_FOLLOW_UP_WORK);
            meter.maximum = self
                .follow_up_limit
                .min(meter.work + GROUND_FEEDBACK_PER_ROAD_WORK);
            return false;
        };
        let patch = |error: &Failure| match error {
            Failure::GroundBlocked {
                radius,
                ground: Some(ground),
                ..
            } => (ground.position, *radius, ground.reason),
            _ => unreachable!("ground feedback requires a blocked ground sample"),
        };
        let (point, radius, reason) = patch(&error);
        if std::iter::once(first)
            .chain(&self.additional)
            .all(|previous| {
                let (other, other_radius, other_reason) = patch(previous);
                let spacing = if reason == "unowned protected ground" { 0.5 } else { 1. };
                other_reason != reason
                    || (point[0] - other[0]).hypot(point[2] - other[2])
                        > radius.max(other_radius) * spacing
            })
        {
            self.additional.push(error);
        }
        self.additional.len() >= GROUND_FEEDBACK_PER_ROAD_LIMIT
    }
}

fn blocked_ground_road(error: &Failure) -> Option<usize> {
    match error {
        Failure::GroundBlocked {
            road,
            ground:
                Some(GroundBlock {
                    reason: "water or low ground" | "unowned protected ground",
                    ..
                }),
            ..
        } => Some(*road),
        _ => None,
    }
}
pub fn validate_routes(
    report: &Report,
    e: &Environment<'_>,
    key: Key,
    points: &[Vec<[f32; 3]>],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<RouteAudit>, Box<RouteFailure>> {
    validate_scope(
        report,
        e,
        Scope::Socket(key),
        points,
        maximum_work,
        cancelled,
    )
}
#[derive(Debug, Serialize)]
pub struct RoadSetAudit {
    pub engine: &'static str,
    pub roads: Vec<usize>,
    pub work: usize,
    pub excluded_padding_samples: usize,
    pub ground_coverage: GroundCoverage,
    pub constructed_ground_checked: bool,
    pub constructed_ocean_checked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constructed_minimum_dry_height: Option<f32>,
    pub source_cost_policy: CostPolicy,
    pub source_costs_passed: Option<bool>,
    pub source_cost_diagnostics: CostDiagnostics,
    pub source_costs_applicable: bool,
    pub whole_selected_routes_checked: bool,
    pub runtime_access_validated: bool,
}
/// Check all selected final routes using the caller's matching family cost/water
/// policy and pristine ground. Selection must be sorted and unique. This does
/// not certify downstream prefab colliders or runtime physics.
pub fn validate_road_set(
    report: &Report,
    e: &Environment<'_>,
    selected: &[usize],
    points: &[Vec<[f32; 3]>],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<RoadSetAudit>, Box<RouteFailure>> {
    validate_road_set_with_coverage(
        report,
        e,
        selected,
        points,
        maximum_work,
        GroundCoverage::RoadAndShoulder,
        cancelled,
    )
}

/// Source construction checks nominal road ground, excluding terrain-blend
/// falloff. Holes/protection/water inside that tube remain blocked. This is not
/// validation of prefab floors, final mesh ground contact or runtime physics.
pub fn validate_road_surface_set(
    report: &Report,
    e: &Environment<'_>,
    selected: &[usize],
    points: &[Vec<[f32; 3]>],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<RoadSetAudit>, Box<RouteFailure>> {
    validate_road_set_with_coverage(
        report,
        e,
        selected,
        points,
        maximum_work,
        GroundCoverage::RoadSurface,
        cancelled,
    )
}

fn validate_road_set_with_coverage(
    report: &Report,
    e: &Environment<'_>,
    selected: &[usize],
    points: &[Vec<[f32; 3]>],
    maximum_work: usize,
    coverage: GroundCoverage,
    cancelled: impl Fn() -> bool,
) -> Result<Option<RoadSetAudit>, Box<RouteFailure>> {
    validate_road_set_scope(
        report,
        e,
        Scope::Roads(selected, coverage, CostPolicy::Required, None),
        points,
        maximum_work,
        cancelled,
    )
}

/// Original source costs guide routing before smoothing/terrain shaping. Keep
/// their final comparison as measurements; all ground/strip/identity gates run.
pub fn validate_source_road_set(
    report: &Report,
    e: &Environment<'_>,
    selected: &[usize],
    points: &[Vec<[f32; 3]>],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<RoadSetAudit>, Box<RouteFailure>> {
    validate_road_set_scope(
        report,
        e,
        Scope::Roads(
            selected,
            GroundCoverage::RoadSurface,
            CostPolicy::RoutingGuide,
            None,
        ),
        points,
        maximum_work,
        cancelled,
    )
}

/// Check the actual source-constructed ground, retaining pristine alpha,
/// topology and ownership. No raster clones or replacement of protected data.
#[allow(clippy::too_many_arguments)]
pub fn validate_source_road_set_on_height(
    report: &Report,
    e: &Environment<'_>,
    selected: &[usize],
    points: &[Vec<[f32; 3]>],
    constructed: &[i16],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<RoadSetAudit>, Box<RouteFailure>> {
    validate_source_road_set_on_ground(
        report,
        e,
        selected,
        points,
        ConstructedGround {
            height: constructed,
            ocean: None,
            minimum_dry_height: 1.,
        },
        maximum_work,
        cancelled,
    )
}
pub fn validate_source_road_set_on_ground(
    report: &Report,
    e: &Environment<'_>,
    selected: &[usize],
    points: &[Vec<[f32; 3]>],
    constructed: ConstructedGround<'_>,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<RoadSetAudit>, Box<RouteFailure>> {
    validate_road_set_scope(
        report,
        e,
        Scope::Roads(
            selected,
            GroundCoverage::RoadSurface,
            CostPolicy::RoutingGuide,
            Some(constructed),
        ),
        points,
        maximum_work,
        cancelled,
    )
}
fn validate_road_set_scope(
    report: &Report,
    e: &Environment<'_>,
    scope: Scope<'_>,
    points: &[Vec<[f32; 3]>],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<RoadSetAudit>, Box<RouteFailure>> {
    let Scope::Roads(_, coverage, cost_policy, constructed) = scope else {
        return Err(Box::new(RouteFailure {
            work: 0,
            measured_work: 0,
            error: Failure::Invalid("road set scope"),
            additional_blocked_ground: Vec::new(),
        }));
    };
    validate_scope(report, e, scope, points, maximum_work, cancelled).map(|result| {
        result.map(|audit| RoadSetAudit {
            engine: if constructed.is_some_and(|g| g.ocean.is_some()) {
                "papyrus-road-route-footprint-5"
            } else if constructed.is_some() {
                "papyrus-road-route-footprint-4"
            } else if cost_policy == CostPolicy::RoutingGuide {
                "papyrus-road-route-footprint-3"
            } else {
                match coverage {
                    GroundCoverage::RoadAndShoulder => "papyrus-road-route-footprint-1",
                    GroundCoverage::RoadSurface => "papyrus-road-route-footprint-2",
                }
            },
            roads: audit.roads,
            work: audit.work,
            excluded_padding_samples: audit.excluded_padding_samples,
            ground_coverage: audit.ground_coverage,
            constructed_ground_checked: audit.constructed_ground_checked,
            constructed_ocean_checked: audit.constructed_ocean_checked,
            constructed_minimum_dry_height: audit.constructed_minimum_dry_height,
            source_cost_policy: audit.source_cost_policy,
            source_costs_passed: audit.source_costs_passed,
            source_cost_diagnostics: audit.source_cost_diagnostics,
            source_costs_applicable: audit.source_costs_applicable,
            whole_selected_routes_checked: true,
            runtime_access_validated: false,
        })
    })
}
#[derive(Clone, Copy)]
enum Scope<'a> {
    Socket(Key),
    Roads(
        &'a [usize],
        GroundCoverage,
        CostPolicy,
        Option<ConstructedGround<'a>>,
    ),
}
fn validate_scope(
    report: &Report,
    e: &Environment<'_>,
    scope: Scope<'_>,
    points: &[Vec<[f32; 3]>],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<RouteAudit>, Box<RouteFailure>> {
    let key = match scope {
        Scope::Socket(key) => key,
        Scope::Roads(..) => Key {
            monument: 0,
            connector: 0,
        },
    };
    let ground_coverage = match scope {
        Scope::Socket(_) => GroundCoverage::RoadAndShoulder,
        Scope::Roads(_, coverage, _, _) => coverage,
    };
    let cost_policy = match scope {
        Scope::Socket(_) => CostPolicy::Required,
        Scope::Roads(_, _, policy, _) => policy,
    };
    let constructed = match scope {
        Scope::Socket(_) => None,
        Scope::Roads(_, _, _, height) => height,
    };
    let mut meter = Meter {
        work: 0,
        maximum: maximum_work,
        cancel: &cancelled,
    };
    let mut additional_blocked_ground = Vec::new();
    let outcome = (|| {
        if cancelled() {
            return Ok(None);
        }
        if cost_policy == CostPolicy::RoutingGuide
            && (e.costs.is_none() || e.cost_lattice.is_none())
        {
            return Err(Failure::Invalid(
                "source guide comparison requires its original cell grid",
            ));
        }
        if !(1..=128_000_000).contains(&maximum_work)
            || points.len() != report.roads.len()
            || points.len() > 256
            || matches!(scope, Scope::Socket(_))
                && (key.monument == 0 || !e.owners.iter().any(|o| o.monument == key.monument))
            || matches!(scope, Scope::Roads(ids, _, _, _) if ids.is_empty() || ids.len()>points.len() || ids.windows(2).any(|w|w[0]>=w[1]) || ids.last().is_some_and(|i|*i>=points.len()))
            || matches!(scope, Scope::Roads(..))
                && (report.entrance_joins.len() > 1024
                    || report
                        .entrance_joins
                        .iter()
                        .map(|j| j.points.len())
                        .sum::<usize>()
                        > 131072)
            || report.families.len() > 256
            || report.families.iter().any(|f| {
                f.first_road
                    .checked_add(f.road_count)
                    .is_none_or(|end| end > report.roads.len())
            })
            || points.iter().map(Vec::len).sum::<usize>() > 131072
            || points.iter().zip(&report.roads).any(|(p, r)| {
                p.len() != r.points.len()
                    || p.len() < 2
                    || p.iter()
                        .flatten()
                        .any(|v| !v.is_finite() || v.abs() > 10000.)
            })
            || !valid_cost_grid(e)
            || report.entrance_launch != Some(e.launch)
            || e.launch.engine != LaunchEngine::GlobalEarthwork
            || e.owners.len() > 128
            || e.owners.iter().any(|o| {
                !o.yaw.is_finite()
                    || o.center
                        .iter()
                        .chain(&o.extent)
                        .any(|v| !v.is_finite() || v.abs() > 10000.)
                    || o.extent.iter().any(|v| *v < 0.)
            })
        {
            return Err(Failure::Invalid("whole socket route inputs"));
        }
        e.launch
            .validate()
            .map_err(|_| Failure::Invalid("socket launch policy"))?;
        e.ownership
            .validate()
            .map_err(|_| Failure::Invalid("socket ownership policy"))?;
        validate_inputs(e.layers, e.alpha, &[], e.obstacles, e.settings)
            .map_err(|_| Failure::Invalid("socket terrain inputs"))?;
        if let Some(ground) = constructed {
            if !ground.minimum_dry_height.is_finite() || ground.minimum_dry_height.abs() > 500. {
                return Err(Failure::Invalid("constructed dry-ground bound"));
            }
            let height = ground.height;
            if height.len() != e.layers.height.len() {
                return Err(Failure::Invalid("constructed ground dimensions"));
            }
            for chunk in height.chunks(4096) {
                if !meter.spend(chunk.len())? {
                    return Ok(None);
                }
                if chunk.iter().any(|v| !(0..=32766).contains(v)) {
                    return Err(Failure::Invalid("constructed ground encoding"));
                }
            }
            if let Some(ocean) = ground.ocean {
                if ocean.resolution != e.layers.surface_resolution
                    || ocean.classification.len() != e.layers.topology.len()
                {
                    return Err(Failure::Invalid("constructed ocean dimensions"));
                }
                for chunk in ocean.classification.chunks(4096) {
                    if !meter.spend(chunk.len())? {
                        return Ok(None);
                    }
                    if chunk.iter().any(|v| *v > crate::ocean::ENCLOSED) {
                        return Err(Failure::Invalid("constructed ocean classification"));
                    }
                }
            }
        }
        if let Some(water) = e.water
            && !water
                .validate(e.layers, &cancelled)
                .map_err(|_| Failure::Invalid("socket river guide"))?
        {
            return Ok(None);
        }
        let mut affected = BTreeMap::new();
        for (i, road) in report.roads.iter().enumerate() {
            let selected = match scope {
                Scope::Socket(_) => [road.from, road.to].contains(&Destination::Monument {
                    monument: key.monument,
                    connector: key.connector,
                }),
                Scope::Roads(ids, _, _, _) => ids.binary_search(&i).is_ok(),
            };
            if selected {
                let family = report
                    .families
                    .iter()
                    .filter(|f| i >= f.first_road && i < f.first_road.saturating_add(f.road_count))
                    .collect::<Vec<_>>();
                if family.len() != 1 || !(1. ..=32.).contains(&family[0].width) {
                    return Err(Failure::Invalid("whole socket road family"));
                }
                for (side, endpoint) in [0, road.points.len() - 1].into_iter().enumerate() {
                    let destination = if endpoint == 0 { road.from } else { road.to };
                    if let Destination::RawNetwork(connection) = destination {
                        let lattice = e
                            .cost_lattice
                            .ok_or(Failure::Invalid("raw route requires its source lattice"))?;
                        let center = lattice
                            .point(connection.raw_cell())
                            .ok_or(Failure::Invalid("raw route cell"))?;
                        let anchor = connection.anchor();
                        if !connection.matches(i, side, report.roads.len(), road.points[endpoint])
                            || !connection.matches(i, side, points.len(), points[i][endpoint])
                            || (center[0] - anchor[0]).abs() > 0.001
                            || (center[1] - anchor[1]).abs() > 0.001
                        {
                            return Err(Failure::Rejected("raw source route anchor changed"));
                        }
                    }
                    if (matches!(scope, Scope::Socket(_))
                        || matches!(destination, Destination::Monument { .. }))
                        && !same_xz(points[i][endpoint], road.points[endpoint])
                    {
                        return Err(Failure::Rejected("whole socket route endpoint moved"));
                    }
                }
                if matches!(scope, Scope::Roads(..)) {
                    for (endpoint, destination) in [(0, road.from), (points[i].len() - 1, road.to)]
                    {
                        match destination {
                            Destination::Network { road, point } => {
                                let target = points
                                    .get(road)
                                    .and_then(|p| p.get(point))
                                    .ok_or(Failure::Invalid("route network attachment"))?;
                                if !same_points(&[points[i][endpoint]], &[*target]) {
                                    return Err(Failure::Rejected(
                                        "route network attachment moved",
                                    ));
                                }
                            }
                            Destination::Monument { monument, .. }
                                if !e.owners.iter().any(|o| o.monument == monument) =>
                            {
                                return Err(Failure::Invalid("route monument owner"));
                            }
                            Destination::Loop
                                if !same_points(&[points[i][0]], &[*points[i].last().unwrap()]) =>
                            {
                                return Err(Failure::Invalid("route loop closure"));
                            }
                            _ => {}
                        }
                    }
                }
                affected.insert(i, vec![true; points[i].len()]);
            }
        }
        if affected.is_empty() {
            return Err(Failure::Rejected("unknown monument socket"));
        }
        let mut cost_diagnostics = CostDiagnostics::default();
        let mut check =
            |roads, ground_feedback: Option<&mut GroundFeedback>, meter: &mut Meter<'_, _>| {
                check_paths(
                    report,
                    e,
                    key,
                    points,
                    roads,
                    true,
                    ground_coverage,
                    constructed,
                    cost_policy,
                    &mut cost_diagnostics,
                    ground_feedback,
                    meter,
                    &cancelled,
                )
            };
        let mut ground_feedback = GroundFeedback::default();
        let result = check(affected.range(..), Some(&mut ground_feedback), &mut meter);
        let result = if let Some(error) = ground_feedback.first {
            if matches!(result, Ok(None)) {
                return Ok(None);
            }
            if matches!(result, Err(Failure::Budget)) {
                meter.work = meter.maximum;
            }
            meter.maximum = ground_feedback.follow_up_limit;
            additional_blocked_ground = ground_feedback.additional;
            Err(error)
        } else {
            result
        };
        let excluded_padding_samples = match result {
            Ok(Some(samples)) => samples,
            Ok(None) => return Ok(None),
            Err(error) => {
                // Gather separated blocked samples on later dirt roads within
                // the same follow-up allowance.
                if constructed.is_some()
                    && let Some(road) = blocked_ground_road(&error)
                    && let Some(family) = report.families.iter().find(|f| {
                        f.hierarchy == 2
                            && road >= f.first_road
                            && road - f.first_road < f.road_count
                    })
                {
                    let end = family.first_road + family.road_count;
                    let follow_up_limit = meter.maximum;
                    for (&next, _) in affected.range(road + 1..end) {
                        let remaining = GROUND_FEEDBACK_FOLLOW_UP_LIMIT - additional_blocked_ground.len();
                        if remaining == 0 || meter.work >= follow_up_limit {
                            break;
                        }
                        let mut feedback = GroundFeedback::default();
                        match check(affected.range(next..=next), Some(&mut feedback), &mut meter) {
                            Ok(None) => return Ok(None),
                            Err(Failure::Budget) => meter.work = meter.maximum,
                            _ => {}
                        }
                        additional_blocked_ground.extend(
                            feedback
                                .first
                                .into_iter()
                                .chain(feedback.additional)
                                .take(remaining),
                        );
                        meter.maximum = follow_up_limit;
                    }
                }
                return Err(error);
            }
        };
        Ok(Some(RouteAudit {
            engine: VERSION,
            socket: key,
            roads: affected.into_keys().collect(),
            work: meter.work,
            excluded_padding_samples,
            ground_coverage,
            constructed_ground_checked: constructed.is_some(),
            constructed_ocean_checked: constructed.is_some_and(|g| g.ocean.is_some()),
            constructed_minimum_dry_height: constructed.map(|g| g.minimum_dry_height),
            source_cost_policy: cost_policy,
            source_costs_passed: e.costs.map(|_| cost_diagnostics.blocked_samples == 0),
            source_cost_diagnostics: cost_diagnostics,
            source_costs_applicable: e.costs.is_some(),
            whole_incident_routes_checked: true,
            runtime_access_validated: false,
        }))
    })();
    outcome.map_err(|error| Box::new(RouteFailure {
        work: if matches!(error, Failure::Budget) {
            maximum_work
        } else {
            meter.work
        },
        measured_work: meter.work,
        error,
        additional_blocked_ground,
    }))
}
/// Rotate only each incident road's adjacent interior control around the exact
/// authored socket. Includes incoming and outgoing incidence, never just one.
pub fn propose(
    report: &Report,
    e: &Environment<'_>,
    key: Key,
    degrees: f64,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Proposal>, Failure> {
    propose_impl(
        report,
        e,
        key,
        degrees,
        maximum_work,
        CostPolicy::Required,
        cancelled,
    )
}

pub fn propose_source(
    report: &Report,
    e: &Environment<'_>,
    key: Key,
    degrees: f64,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Proposal>, Failure> {
    propose_impl(
        report,
        e,
        key,
        degrees,
        maximum_work,
        CostPolicy::RoutingGuide,
        cancelled,
    )
}

fn propose_impl(
    report: &Report,
    e: &Environment<'_>,
    key: Key,
    degrees: f64,
    maximum_work: usize,
    cost_policy: CostPolicy,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Proposal>, Failure> {
    if cancelled() {
        return Ok(None);
    }
    if cost_policy == CostPolicy::RoutingGuide && (e.costs.is_none() || e.cost_lattice.is_none()) {
        return Err(Failure::Invalid(
            "source proposal requires its original cell grid",
        ));
    }
    if !(1..=32_000_000).contains(&maximum_work)
        || maximum_work > e.launch.maximum_work
        || !degrees.is_finite()
        || degrees == 0.
        || degrees.abs() > 10.
        || report.roads.is_empty()
        || report.roads.len() > 256
        || report.roads.iter().map(|r| r.points.len()).sum::<usize>() > 131072
        || report.surfaces.is_some()
        || report.families.iter().any(|f| {
            f.first_road
                .checked_add(f.road_count)
                .is_none_or(|end| end > report.roads.len())
        })
        || key.monument == 0
        || !valid_cost_grid(e)
        || e.launch.engine != LaunchEngine::GlobalEarthwork
        || report.entrance_launch != Some(e.launch)
    {
        return Err(Failure::Invalid("socket recovery inputs"));
    }
    path_terrain::encoded_height_quantum(path_terrain::Layout {
        size: e.layers.size as f32,
        resolution: e.layers.height_resolution,
        origin_y: e.height_origin,
        vertical_span: e.vertical_span,
    })
    .map_err(|_| Failure::Invalid("socket height layout"))?;
    e.launch
        .validate()
        .map_err(|_| Failure::Invalid("socket launch policy"))?;
    e.ownership
        .validate()
        .map_err(|_| Failure::Invalid("socket ownership policy"))?;
    validate_inputs(e.layers, e.alpha, &[], e.obstacles, e.settings)
        .map_err(|_| Failure::Invalid("socket terrain inputs"))?;
    if e.owners.len() > 128
        || e.owners.iter().any(|o| {
            !o.yaw.is_finite()
                || o.center
                    .iter()
                    .chain(&o.extent)
                    .any(|v| !v.is_finite() || v.abs() > 10000.)
                || o.extent.iter().any(|v| *v < 0.)
        })
    {
        return Err(Failure::Invalid("socket ownership footprints"));
    }
    if let Some(water) = e.water
        && !water
            .validate(e.layers, &cancelled)
            .map_err(|_| Failure::Invalid("socket river guide"))?
    {
        return Ok(None);
    }
    if !e.owners.iter().any(|o| o.monument == key.monument) {
        return Err(Failure::Rejected("missing socket owner footprint"));
    }
    let mut meter = Meter {
        work: 0,
        maximum: maximum_work,
        cancel: &cancelled,
    };
    let mut points = Vec::new();
    let mut changes = BTreeMap::new();
    let mut pivot = None;
    let (s, c) = degrees.to_radians().sin_cos();
    for (ri, road) in report.roads.iter().enumerate() {
        if !meter.spend(road.points.len())? {
            return Ok(None);
        }
        if !(2..=65536).contains(&road.points.len())
            || road
                .points
                .iter()
                .flatten()
                .any(|v| !v.is_finite() || v.abs() > 10000.)
        {
            return Err(Failure::Invalid("socket road controls"));
        }
        let mut next = road.points.clone();
        for (side, destination) in [road.from, road.to].into_iter().enumerate() {
            if destination
                != (Destination::Monument {
                    monument: key.monument,
                    connector: key.connector,
                })
            {
                continue;
            }
            if road.points.len() < 3 || same_xz(road.points[0], *road.points.last().unwrap()) {
                return Err(Failure::Rejected(
                    "socket road has no free departure control",
                ));
            }
            let family = report
                .families
                .iter()
                .filter(|f| ri >= f.first_road && ri < f.first_road.saturating_add(f.road_count))
                .collect::<Vec<_>>();
            if family.len() != 1 || !(1. ..=32.).contains(&family[0].width) {
                return Err(Failure::Invalid("socket road family"));
            }
            if family[0].hierarchy >= 2 {
                return Err(Failure::Rejected(
                    "socket mesh recovery applies to paved paths",
                ));
            }
            let endpoint = if side == 0 { 0 } else { road.points.len() - 1 };
            let socket = road.points[endpoint];
            if pivot.is_some_and(|p| !same_points(&[p], &[socket])) {
                return Err(Failure::Rejected("shared socket poses differ"));
            }
            pivot = Some(socket);
            let departure = departure_controls(
                &road.points,
                side,
                family[0].width * e.launch.minimum_run_widths,
                e.launch.sample_spacing,
            )?;
            for i in departure {
                if !meter.spend(8)? {
                    return Ok(None);
                }
                if changes.contains_key(&(ri, i)) {
                    return Err(Failure::Rejected("overlapping socket controls"));
                }
                let dx = f64::from(road.points[i][0]) - f64::from(socket[0]);
                let dz = f64::from(road.points[i][2]) - f64::from(socket[2]);
                let mut adjusted = [
                    (f64::from(socket[0]) + c * dx - s * dz) as f32,
                    0.,
                    (f64::from(socket[2]) + s * dx + c * dz) as f32,
                ];
                if distance(adjusted, road.points[i]) > family[0].width * 0.5 {
                    return Err(Failure::Rejected(
                        "socket adjustment exceeds departure envelope",
                    ));
                }
                adjusted[1] = path_terrain::sample_height(
                    path_terrain::Grid {
                        size: e.layers.size as f32,
                        resolution: e.layers.height_resolution,
                        origin_y: e.height_origin,
                        vertical_span: e.vertical_span,
                        height: &e.layers.height,
                    },
                    adjusted,
                )
                .map_err(|_| Failure::Invalid("socket height sample"))?;
                next[i] = adjusted;
                changes.insert(
                    (ri, i),
                    Change {
                        road: ri,
                        control: i,
                        before: road.points[i],
                        after: adjusted,
                    },
                );
            }
        }
        points.push(next);
    }
    if changes.is_empty() {
        return Err(Failure::Rejected("unknown monument socket"));
    }
    // Reject aliases rather than silently moving a branch's attachment.
    for (ri, road) in report.roads.iter().enumerate() {
        for (side, d) in [road.from, road.to].into_iter().enumerate() {
            if !meter.spend(1)? {
                return Ok(None);
            }
            if let Destination::Network {
                road: target,
                point,
            } = d
            {
                let Some(expected) = report.roads.get(target).and_then(|r| r.points.get(point))
                else {
                    return Err(Failure::Invalid("socket network attachment"));
                };
                let endpoint = if side == 0 { 0 } else { road.points.len() - 1 };
                if !same_points(&[*expected], &[road.points[endpoint]]) {
                    return Err(Failure::Invalid("disconnected socket input network"));
                }
                if changes.contains_key(&(target, point)) || changes.contains_key(&(ri, endpoint)) {
                    return Err(Failure::Rejected("socket control is a network attachment"));
                }
            }
            if let Destination::RawNetwork(connection) = d {
                let endpoint = if side == 0 { 0 } else { road.points.len() - 1 };
                if !connection.matches(ri, side, report.roads.len(), road.points[endpoint]) {
                    return Err(Failure::Invalid("stale raw socket input"));
                }
                if changes.contains_key(&(ri, endpoint)) {
                    return Err(Failure::Rejected("socket edit moved raw source anchor"));
                }
            }
        }
    }
    let mut affected_roads = BTreeMap::new();
    for &(ri, control) in changes.keys() {
        affected_roads
            .entry(ri)
            .or_insert_with(|| vec![false; points[ri].len()])[control] = true;
    }
    let mut cost_diagnostics = CostDiagnostics::default();
    let ground_coverage = if cost_policy == CostPolicy::RoutingGuide {
        GroundCoverage::RoadSurface
    } else {
        GroundCoverage::RoadAndShoulder
    };
    let Some(excluded_padding_samples) = check_paths(
        report,
        e,
        key,
        &points,
        &affected_roads,
        false,
        ground_coverage,
        None,
        cost_policy,
        &mut cost_diagnostics,
        None,
        &mut meter,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let before = report
        .roads
        .iter()
        .map(|r| (r.from, r.to, r.points.clone()))
        .collect();
    Ok(Some(Proposal {
        before,
        metadata: metadata(report)?,
        points,
        audit: Audit {
            engine: if cost_policy == CostPolicy::RoutingGuide {
                "papyrus-source-road-socket-geometry-2"
            } else {
                VERSION
            },
            socket: key,
            degrees,
            changes: changes.into_values().collect(),
            work: meter.work,
            excluded_padding_samples,
            routing_ground_checked: true,
            source_costs_applicable: e.costs.is_some(),
            source_costs_checked: e.costs.is_some(),
            source_cost_policy: cost_policy,
            source_costs_passed: e.costs.map(|_| cost_diagnostics.blocked_samples == 0),
            source_cost_diagnostics: cost_diagnostics,
            ground_coverage,
            final_access_validated: false,
        },
    }))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cached_ground_rows_preserve_ordered_witnesses_for_constructed_heights() {
        let (mut layers, mut alpha, _, _, _, mut settings, _, _) = fixture();
        let n = layers.height_resolution;
        let sr = layers.surface_resolution;
        let axes: Vec<_> = (0..n).map(|i| ground_axis(&layers, i)).collect();
        let mut height = layers.height.clone();
        let z = n / 2;
        height[z * n + 31] = crate::erosion::encode(-5.);
        alpha[axes[z].1 * sr + axes[34].1] = 0;
        settings.protected_topology = crate::ocean::OCEAN_BIT | 16;
        layers.topology[axes[z + 4].1 * sr + axes[63].1] = 16;
        let mut ocean = crate::ocean::classify_height(&height, n, sr, || false)
            .unwrap()
            .unwrap();
        ocean.classification[axes[z + 6].1 * sr + axes[n - 1].1] = crate::ocean::OCEAN;
        let ground = ConstructedGround {
            height: &height,
            ocean: Some(&ocean),
            minimum_dry_height: 1.,
        };
        let mut rows = GroundRows::new(ground, &layers, &alpha, &axes, settings.protected_topology);
        for limit in [
            f32::NEG_INFINITY,
            decode(i16::MIN),
            0.,
            decode(16417).next_down(),
            decode(16417),
            decode(16417).next_up(),
            decode(i16::MAX),
            f32::INFINITY,
            f32::NAN,
        ] {
            let mut rows = GroundRows::new(
                ConstructedGround {
                    minimum_dry_height: limit,
                    ..ground
                },
                &layers,
                &alpha,
                &axes,
                settings.protected_topology,
            );
            for height in i16::MIN..=i16::MAX {
                assert_eq!(
                    i32::from(height) <= rows.maximum_low_height,
                    decode(height) <= limit
                );
            }
            if limit == f32::INFINITY {
                let tile = (n - 1) / 32;
                for _ in 0..2 {
                    assert_eq!(rows.blocking_word(n - 1, 0), u32::MAX);
                    assert_eq!(
                        rows.blocking_word(n - 1, tile),
                        u32::MAX >> (31 - (n - 1) % 32)
                    );
                }
            }
        }
        for x in [16, 31, 32, 63, 64, n / 2, n - 1] {
            for row in [z, z + 4, z + 6] {
                let center = [axes[x].0, axes[row].0];
                let mut witnesses = [Vec::new(), Vec::new()];
                for (mode, output) in witnesses.iter_mut().enumerate() {
                    assert!(ground_clear_with_water_filter_on_height(
                        &layers,
                        &height,
                        Some(&ocean),
                        1.,
                        &alpha,
                        settings,
                        center,
                        10.,
                        None,
                        None,
                        |index| axes[index],
                        (mode != 0).then_some(|z, tile| rows.blocking_word(z, tile)),
                        |block| {
                            output.push(block);
                            false
                        },
                    ));
                }
                assert_eq!(witnesses[0], witnesses[1]);
            }
        }
    }

    #[test]
    fn only_exact_original_checked_entrance_chains_can_cross_source_cost_buffers() {
        let (layers, alpha, mut report, owners, mut costs, settings, ownership, launch) = fixture();
        for z in 112..=144 {
            for x in 128..=145 {
                costs[z * GRID + x] = 0;
            }
        }
        let points = report
            .roads
            .iter()
            .map(|r| r.points.clone())
            .collect::<Vec<_>>();
        macro_rules! env {
            () => {
                Environment {
                    layers: &layers,
                    alpha: &alpha,
                    obstacles: &[],
                    owners: &owners,
                    settings,
                    ownership,
                    launch,
                    costs: Some(&costs),
                    cost_lattice: None,
                    water: None,
                    height_origin: -500.,
                    vertical_span: 1000.,
                }
            };
        }
        assert!(matches!(
            validate_road_set(&report, &env!(), &[1], &points, 16_000_000, || false)
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::CostBlocked { .. },
                ..
            })
        ));
        report.entrance_joins.push(EntranceJoin {
            monument: 1,
            connector: 11,
            points: points[1][..3].to_vec(),
        });
        assert!(
            validate_road_set(&report, &env!(), &[1], &points, 16_000_000, || false)
                .unwrap()
                .is_some()
        );
        let mut detour = points.clone();
        detour[1][1][2] = 10.;
        assert!(validate_road_set(&report, &env!(), &[1], &detour, 16_000_000, || false).is_err());
        report.entrance_joins[0].points[1][2] = 10.;
        assert!(validate_road_set(&report, &env!(), &[1], &points, 16_000_000, || false).is_err());
    }
    #[test]
    fn constructed_ocean_never_erases_holes_or_other_protected_topology() {
        let (mut layers, mut alpha, report, owners, _, mut settings, ownership, launch) = fixture();
        settings.protected_topology |= crate::ocean::OCEAN_BIT;
        let constructed = layers.height.clone();
        let mask = crate::ocean::classify_height(
            &constructed,
            layers.height_resolution,
            layers.surface_resolution,
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(mask.classification.iter().all(|v| *v == crate::ocean::LAND));
        let lattice = crate::road_costmap::CellLattice::new(1000., 10.).unwrap();
        let costs = vec![101; lattice.resolution().pow(2)];
        let points = report
            .roads
            .iter()
            .map(|r| r.points.clone())
            .collect::<Vec<_>>();
        macro_rules! env {
            () => {
                Environment {
                    layers: &layers,
                    alpha: &alpha,
                    obstacles: &[],
                    owners: &owners,
                    settings,
                    ownership,
                    launch,
                    costs: Some(&costs),
                    cost_lattice: Some(lattice),
                    water: None,
                    height_origin: -500.,
                    vertical_span: 1000.,
                }
            };
        }
        layers.topology.fill(crate::ocean::OCEAN_BIT);
        assert!(matches!(
            validate_source_road_set_on_height(
                &report,
                &env!(),
                &[1],
                &points,
                &constructed,
                32000000,
                || false
            )
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::GroundBlocked { .. },
                ..
            })
        ));
        let ground = ConstructedGround {
            height: &constructed,
            ocean: Some(&mask),
            minimum_dry_height: 1.,
        };
        let audit = validate_source_road_set_on_ground(
            &report,
            &env!(),
            &[1],
            &points,
            ground,
            32000000,
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(audit.constructed_ocean_checked && audit.constructed_ground_checked);
        assert_eq!(audit.engine, "papyrus-road-route-footprint-5");
        assert!(
            layers
                .topology
                .iter()
                .all(|v| *v == crate::ocean::OCEAN_BIT)
        );
        layers.topology.fill(crate::ocean::OCEAN_BIT | 1024);
        assert!(matches!(
            validate_source_road_set_on_ground(
                &report,
                &env!(),
                &[1],
                &points,
                ground,
                32000000,
                || false
            )
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::GroundBlocked {
                    ground: Some(GroundBlock {
                        reason: "unowned protected ground",
                        ..
                    }),
                    ..
                },
                ..
            })
        ));
        layers.topology.fill(crate::ocean::OCEAN_BIT);
        alpha.fill(0);
        assert!(matches!(
            validate_source_road_set_on_ground(
                &report,
                &env!(),
                &[1],
                &points,
                ground,
                32000000,
                || false
            )
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::GroundBlocked {
                    ground: Some(GroundBlock {
                        reason: "terrain hole",
                        ..
                    }),
                    ..
                },
                ..
            })
        ));
        alpha.fill(255);
        let bad = crate::ocean::Mask {
            resolution: mask.resolution,
            classification: vec![255; mask.classification.len()],
        };
        assert!(matches!(
            validate_source_road_set_on_ground(
                &report,
                &env!(),
                &[1],
                &points,
                ConstructedGround {
                    height: &constructed,
                    ocean: Some(&bad),
                    minimum_dry_height: 1.,
                },
                32000000,
                || false
            )
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::Invalid(_),
                ..
            })
        ));
        let wrong = crate::ocean::Mask {
            resolution: mask.resolution + 1,
            classification: mask.classification.clone(),
        };
        assert!(matches!(
            validate_source_road_set_on_ground(
                &report,
                &env!(),
                &[1],
                &points,
                ConstructedGround {
                    height: &constructed,
                    ocean: Some(&wrong),
                    minimum_dry_height: 1.,
                },
                32000000,
                || false
            )
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::Invalid(_),
                ..
            })
        ));
        assert!(
            validate_source_road_set_on_ground(
                &report,
                &env!(),
                &[1],
                &points,
                ground,
                32000000,
                || true
            )
            .unwrap()
            .is_none()
        );
    }
    #[test]
    fn constructed_dry_bound_keeps_low_beaches_but_rejects_water_holes_and_foreign_protection() {
        let (mut layers, mut alpha, mut report, owners, _, settings, ownership, launch) = fixture();
        let low = 16415; // The 5 km witness decodes to about 0.9766 m.
        let lattice = crate::road_costmap::CellLattice::new(1000., 10.).unwrap();
        let costs = vec![101; lattice.resolution().pow(2)];
        let y = crate::erosion::decode(low);
        assert!(y > 0. && y < 1.);
        layers.height.fill(low);
        for road in &mut report.roads {
            for p in &mut road.points {
                p[1] = y;
            }
        }
        let points = report
            .roads
            .iter()
            .map(|r| r.points.clone())
            .collect::<Vec<_>>();
        macro_rules! env {
            () => {
                Environment {
                    height_origin: -500.,
                    vertical_span: 1000.,
                    layers: &layers,
                    alpha: &alpha,
                    obstacles: &[],
                    owners: &owners,
                    settings,
                    ownership,
                    launch,
                    costs: Some(&costs),
                    cost_lattice: Some(lattice),
                    water: None,
                }
            };
        }
        let check = |minimum_dry_height| ConstructedGround {
            height: layers.height.as_slice(),
            ocean: None,
            minimum_dry_height,
        };
        assert!(
            validate_source_road_set_on_height(
                &report,
                &env!(),
                &[1],
                &points,
                &layers.height,
                32_000_000,
                || false
            )
            .is_err()
        );
        let audit = validate_source_road_set_on_ground(
            &report,
            &env!(),
            &[1],
            &points,
            check(0.),
            32_000_000,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(audit.constructed_minimum_dry_height, Some(0.));
        assert!(
            validate_source_road_set_on_ground(
                &report,
                &env!(),
                &[1],
                &points,
                check(0.5),
                32_000_000,
                || false
            )
            .unwrap()
            .is_some()
        );
        for bound in [1., f32::NAN, f32::INFINITY, 501.] {
            assert!(
                validate_source_road_set_on_ground(
                    &report,
                    &env!(),
                    &[1],
                    &points,
                    check(bound),
                    32_000_000,
                    || false
                )
                .is_err()
            );
        }
        alpha.fill(0);
        assert!(matches!(
            validate_source_road_set_on_ground(
                &report,
                &env!(),
                &[1],
                &points,
                check(0.),
                32_000_000,
                || false
            )
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::GroundBlocked {
                    ground: Some(GroundBlock {
                        reason: "terrain hole",
                        ..
                    }),
                    ..
                },
                ..
            })
        ));
        alpha.fill(255);
        layers.topology.fill(settings.protected_topology);
        assert!(
            validate_source_road_set_on_ground(
                &report,
                &env!(),
                &[1],
                &points,
                check(0.),
                32_000_000,
                || false
            )
            .is_err()
        );
        layers.topology.fill(0);
        for h in [16383, 16382] {
            let mut wet = layers.height.clone();
            wet.fill(h);
            assert!(matches!(
                validate_source_road_set_on_ground(
                    &report,
                    &env!(),
                    &[1],
                    &points,
                    ConstructedGround {
                        height: &wet,
                        ocean: None,
                        minimum_dry_height: 0.
                    },
                    32_000_000,
                    || false
                )
                .as_ref()
                .map_err(Box::as_ref),
                Err(RouteFailure {
                    error: Failure::GroundBlocked {
                        ground: Some(GroundBlock {
                            reason: "water or low ground",
                            ..
                        }),
                        ..
                    },
                    ..
                })
            ));
        }
    }
    #[test]
    fn constructed_dirt_roads_report_wet_crossings_on_the_same_and_later_roads() {
        check_dirt_ground_feedback(false, "water or low ground");
    }
    #[test]
    fn constructed_dirt_roads_report_protected_ground_on_the_same_and_later_roads() {
        check_dirt_ground_feedback(true, "unowned protected ground");
    }
    fn check_dirt_ground_feedback(protected: bool, reason: &str) {
        let (mut layers, alpha, mut report, _, _, settings, ownership, launch) = fixture();
        let y = crate::erosion::decode(layers.height[0]);
        let mut owners = Vec::new();
        report.roads = [-100., 0., 100.]
            .into_iter()
            .enumerate()
            .map(|(i, z)| {
                let [from, to] = [-100., 100.].map(|x| {
                    let monument = owners.len() as u64 + 1;
                    owners.push(Obstacle {
                        monument,
                        center: [x, z],
                        extent: [6., 6.],
                        yaw: 0.,
                    });
                    Destination::Monument {
                        monument,
                        connector: i as i64,
                    }
                });
                Road {
                    from,
                    to,
                    points: (-10..=10).map(|x| [x as f32 * 10., y, z]).collect(),
                }
            })
            .collect();
        report.families[0].road_count = report.roads.len();
        report.families[0].hierarchy = 2;
        let points = report
            .roads
            .iter()
            .map(|road| road.points.clone())
            .collect::<Vec<_>>();
        let n = layers.height_resolution;
        let mut height = layers.height.clone();
        let size = layers.size as f32;
        let crosses = |x: f32, z: f32| {
            ((z + 100.).abs() < 2. && (x.abs() - 40.).abs() < 2.)
                || ((z - 100.).abs() < 2. && (x.abs() - 50.).abs() < 2.)
        };
        if protected {
            let s = layers.surface_resolution;
            let world = |cell: usize| ((cell as f32 + 0.5) / s as f32 - 0.5) * size;
            for (z, row) in layers.topology.chunks_exact_mut(s).enumerate() {
                for (x, topology) in row.iter_mut().enumerate() {
                    if crosses(world(x), world(z)) {
                        *topology = settings.protected_topology;
                    }
                }
            }
        } else {
            let world = |cell: usize| (cell as f32 / (n - 1) as f32 - 0.5) * size;
            for (z, row) in height.chunks_exact_mut(n).enumerate() {
                for (x, height) in row.iter_mut().enumerate() {
                    if crosses(world(x), world(z)) {
                        *height = crate::erosion::encode(0.);
                    }
                }
            }
        }
        let lattice = crate::road_costmap::CellLattice::new(1000., 10.).unwrap();
        let costs = vec![101; lattice.resolution().pow(2)];
        let environment = Environment {
            layers: &layers,
            alpha: &alpha,
            obstacles: &[],
            owners: &owners,
            settings,
            ownership,
            launch,
            costs: Some(&costs),
            cost_lattice: Some(lattice),
            height_origin: -500.,
            vertical_span: 1000.,
            water: None,
        };
        let audit = |report: &Report, roads: &[usize], work| {
            validate_source_road_set_on_ground(
                report,
                &environment,
                roads,
                &points,
                ConstructedGround {
                    height: &height,
                    ocean: None,
                    minimum_dry_height: 0.,
                },
                work,
                || false,
            )
        };
        report.families[0].hierarchy = 1;
        let first = audit(&report, &[0], 32_000_000).unwrap_err();
        report.families[0].hierarchy = 2;
        let batch = audit(&report, &[0, 1, 2], 32_000_000).unwrap_err();
        assert_eq!(blocked_ground_road(&batch.error), Some(0));
        let roads: Vec<_> = batch
            .additional_blocked_ground
            .iter()
            .map(blocked_ground_road)
            .collect();
        assert_eq!(roads, [Some(0), Some(2), Some(2)]);
        for (crossing, expected_x) in std::iter::once(&batch.error)
            .chain(&batch.additional_blocked_ground)
            .zip([-40., 40., -50., 50.])
        {
            assert!(matches!(
                crossing,
                Failure::GroundBlocked { ground: Some(ground), .. }
                    if ground.reason == reason && (ground.position[0] - expected_x).abs() < 4.
            ));
        }
        assert!(batch.work > first.work && batch.work <= first.work + GROUND_FEEDBACK_FOLLOW_UP_WORK);

        let bounded = audit(&report, &[0, 1, 2], first.work + 1).unwrap_err();
        assert_eq!(blocked_ground_road(&bounded.error), Some(0));
        assert!(bounded.additional_blocked_ground.is_empty());
        assert_eq!(bounded.work, first.work + 1);

        report.families[0].hierarchy = 1;
        let paved = audit(&report, &[0, 1, 2], 32_000_000).unwrap_err();
        assert_eq!(blocked_ground_road(&paved.error), Some(0));
        assert!(paved.additional_blocked_ground.is_empty());
    }
    #[test]
    fn raw_routes_preserve_source_anchors_without_requiring_indexed_centerlines() {
        let (mut layers, mut alpha, mut report, owners, _, settings, ownership, launch) = fixture();
        let lattice = crate::road_costmap::CellLattice::new(1000., 10.).unwrap();
        let y = crate::erosion::decode(layers.height[0]);
        let parent = Path::from_controls(
            &[[-23., y, 0.], [0., y, 0.], [12., y, 0.], [30., y, 0.]],
            100000,
            || false,
        )
        .unwrap()
        .unwrap();
        let child = Path::from_controls(
            &[[-5., y, 40.], [-5., y, 20.], [-5., y, 5.]],
            100000,
            || false,
        )
        .unwrap()
        .unwrap();
        let nodes = |cells: &[[usize; 2]]| {
            cells
                .iter()
                .map(|&cell| crate::source_road_ring::RawNode { cell, cost: 1 })
                .collect::<Vec<_>>()
        };
        let p = nodes(&[[48, 50], [49, 50], [51, 50]]);
        let c = nodes(&[[49, 54], [49, 50]]);
        let network = crate::source_road_junctions::attach_raw(
            &[
                crate::source_road_junctions::Road {
                    lattice,
                    nodes: &p,
                    path: &parent,
                    terminals: [true, true],
                    hierarchy: 1,
                },
                crate::source_road_junctions::Road {
                    lattice,
                    nodes: &c,
                    path: &child,
                    terminals: [true, false],
                    hierarchy: 1,
                },
            ],
            crate::source_road_junctions::Settings {
                maximum_snap_distance: 20.,
                maximum_work: 100000,
            },
            || false,
        )
        .unwrap()
        .unwrap();
        let proof = network.raw_connections[0];
        report.roads.truncate(2);
        report.roads[0].points = parent.points().to_vec();
        report.roads[1].points = child.points().to_vec();
        report.roads[1].to = Destination::RawNetwork(proof);
        report.families[0].road_count = 2;
        let mut costs = vec![101; 10000];
        let points = report
            .roads
            .iter()
            .map(|r| r.points.clone())
            .collect::<Vec<_>>();
        macro_rules! env {
            () => {
                Environment {
                    layers: &layers,
                    alpha: &alpha,
                    obstacles: &[],
                    owners: &owners,
                    settings,
                    ownership,
                    launch,
                    costs: Some(&costs),
                    cost_lattice: Some(lattice),
                    water: None,
                    height_origin: -500.,
                    vertical_span: 1000.,
                }
            };
        }
        assert!(
            validate_road_surface_set(&report, &env!(), &[1], &points, 32000000, || false)
                .unwrap()
                .is_some()
        );
        let mut moved = points.clone();
        moved[1][2][0] += 0.01;
        assert!(matches!(
            validate_road_surface_set(&report, &env!(), &[1], &moved, 32000000, || false)
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::Rejected("raw source route anchor changed"),
                ..
            })
        ));
        costs[53 * 100 + 49] = 0;
        assert!(matches!(
            validate_road_surface_set(&report, &env!(), &[1], &points, 32000000, || false)
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::CostBlocked { .. },
                ..
            })
        ));
        let guide = validate_source_road_set(&report, &env!(), &[1], &points, 32000000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(guide.source_cost_policy, CostPolicy::RoutingGuide);
        assert_eq!(guide.source_costs_passed, Some(false));
        assert!(guide.source_cost_diagnostics.blocked_samples > 0);
        assert!(!guide.source_cost_diagnostics.witnesses.is_empty());
        assert!(guide.source_cost_diagnostics.witnesses.len() <= 8);
        let mut missing = env!();
        missing.costs = None;
        assert!(matches!(
            validate_source_road_set(&report, &missing, &[1], &points, 32000000, || false)
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::Invalid(_),
                ..
            })
        ));
        let constructed = layers.height.clone();
        layers.height.fill(16414); // 0.9461 m: source construction raises this.
        assert!(matches!(
            validate_source_road_set(&report, &env!(), &[1], &points, 32000000, || false)
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::GroundBlocked { .. },
                ..
            })
        ));
        let final_ground = validate_source_road_set_on_height(
            &report,
            &env!(),
            &[1],
            &points,
            &constructed,
            32000000,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(final_ground.engine, "papyrus-road-route-footprint-4");
        assert!(final_ground.constructed_ground_checked);
        assert_eq!(final_ground.source_costs_passed, Some(false));
        assert!(matches!(
            validate_source_road_set_on_height(
                &report,
                &env!(),
                &[1],
                &points,
                &constructed[..constructed.len() - 1],
                32000000,
                || false
            )
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::Invalid(_),
                ..
            })
        ));
        let mut invalid = constructed.clone();
        invalid[0] = -1;
        assert!(matches!(
            validate_source_road_set_on_height(
                &report,
                &env!(),
                &[1],
                &points,
                &invalid,
                32000000,
                || false
            )
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::Invalid(_),
                ..
            })
        ));
        assert!(
            validate_source_road_set_on_height(
                &report,
                &env!(),
                &[1],
                &points,
                &constructed,
                32000000,
                || true
            )
            .unwrap()
            .is_none()
        );
        assert!(matches!(
            validate_source_road_set_on_height(
                &report,
                &env!(),
                &[1],
                &points,
                &constructed,
                1,
                || false
            )
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::Budget,
                ..
            })
        ));
        layers.topology.fill(1024);
        assert!(matches!(
            validate_source_road_set_on_height(
                &report,
                &env!(),
                &[1],
                &points,
                &constructed,
                32000000,
                || false
            )
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::GroundBlocked { .. },
                ..
            })
        ));
        layers.topology.fill(0);
        alpha.fill(0);
        assert!(matches!(
            validate_source_road_set_on_height(
                &report,
                &env!(),
                &[1],
                &points,
                &constructed,
                32000000,
                || false
            )
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::GroundBlocked {
                    ground: Some(GroundBlock {
                        reason: "terrain hole",
                        ..
                    }),
                    ..
                },
                ..
            })
        ));
        assert!(matches!(
            validate_source_road_set(&report, &env!(), &[1], &points, 32000000, || false)
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::GroundBlocked { .. },
                ..
            })
        ));
        report.roads[1].to = Destination::Network {
            road: 0,
            point: proof.reference_parent_point(),
        };
        assert!(matches!(
            validate_road_surface_set(&report, &env!(), &[1], &points, 32000000, || false)
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::Rejected("route network attachment moved"),
                ..
            })
        ));
    }
    #[test]
    fn source_surface_keeps_holes_blocked_beneath_roads_but_excludes_blend_falloff() {
        let (layers, mut alpha, report, owners, costs, settings, ownership, launch) = fixture();
        let points = report
            .roads
            .iter()
            .map(|r| r.points.clone())
            .collect::<Vec<_>>();
        let cell = |x: f32, z: f32| {
            let n = layers.surface_resolution;
            (((z / 1000. + 0.5) * n as f32) as usize) * n + ((x / 1000. + 0.5) * n as f32) as usize
        };
        macro_rules! env {
            () => {
                Environment {
                    layers: &layers,
                    alpha: &alpha,
                    obstacles: &[],
                    owners: &owners,
                    settings,
                    ownership,
                    launch,
                    costs: Some(&costs),
                    cost_lattice: None,
                    water: None,
                    height_origin: -500.,
                    vertical_span: 1000.,
                }
            };
        }
        alpha[cell(100., 6.)] = 0;
        let original = alpha.clone();
        assert!(matches!(
            validate_road_set(&report, &env!(), &[1], &points, 32_000_000, || false)
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::GroundBlocked {
                    ground: Some(GroundBlock {
                        reason: "terrain hole",
                        ..
                    }),
                    ..
                },
                ..
            })
        ));
        let checked =
            validate_road_surface_set(&report, &env!(), &[1], &points, 32_000_000, || false)
                .unwrap()
                .unwrap();
        assert_eq!(checked.engine, "papyrus-road-route-footprint-2");
        assert_eq!(checked.ground_coverage, GroundCoverage::RoadSurface);
        assert!(checked.whole_selected_routes_checked && checked.source_costs_applicable);
        assert!(!checked.runtime_access_validated);
        assert!(checked.excluded_padding_samples > 0);
        assert_eq!(alpha, original);
        alpha[cell(100., 0.)] = 0;
        assert!(matches!(
            validate_road_surface_set(&report, &env!(), &[1], &points, 32_000_000, || false)
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::GroundBlocked {
                    ground: Some(GroundBlock {
                        reason: "terrain hole",
                        ..
                    }),
                    ..
                },
                ..
            })
        ));
        assert!(
            validate_road_surface_set(&report, &env!(), &[1], &points, 32_000_000, || true)
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            validate_road_surface_set(&report, &env!(), &[1], &points, 1, || false)
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::Budget,
                ..
            })
        ));
        // Dirt uses its own source width, not the paved-road half-width.
        let mut dirt = report;
        dirt.families[0].hierarchy = 2;
        dirt.families[0].width = 4.;
        dirt.families[0].splat_channel = 0;
        alpha.fill(255);
        alpha[cell(100., 4.)] = 0;
        assert!(validate_road_set(&dirt, &env!(), &[1], &points, 32_000_000, || false).is_err());
        assert!(
            validate_road_surface_set(&dirt, &env!(), &[1], &points, 32_000_000, || false)
                .unwrap()
                .is_some()
        );
        alpha[cell(100., 0.)] = 0;
        assert!(matches!(
            validate_road_surface_set(&dirt, &env!(), &[1], &points, 32_000_000, || false)
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::GroundBlocked { .. },
                ..
            })
        ));
    }
    #[test]
    fn full_road_sets_include_loops_and_dirt_and_reject_remote_holes_costs_and_bad_joins() {
        let (mut layers, mut alpha, mut report, owners, mut costs, settings, ownership, launch) =
            fixture();
        let y = report.roads[0].points[0][1];
        report.roads.truncate(2);
        report.roads[0].points = (0..=32)
            .map(|i| {
                let a = (i % 32) as f32 * std::f32::consts::TAU / 32.;
                [120. * a.cos(), y, 160. + 120. * a.sin()]
            })
            .collect();
        report.roads[1].to = Destination::Network { road: 0, point: 0 };
        report.roads[1].points = (0..=4)
            .map(|i| [i as f32 * 30., y, i as f32 * 40.])
            .collect();
        report.families[0].hierarchy = 0;
        report.families[0].width = 12.;
        report.families[0].road_count = 1;
        report.families.push(Family {
            hierarchy: 2,
            width: 4.,
            first_road: 1,
            road_count: 1,
            splat_channel: 0,
            ..report.families[0].clone()
        });
        let points = report
            .roads
            .iter()
            .map(|r| r.points.clone())
            .collect::<Vec<_>>();
        macro_rules! env {
            () => {
                Environment {
                    layers: &layers,
                    alpha: &alpha,
                    obstacles: &[],
                    owners: &owners,
                    settings,
                    ownership,
                    launch,
                    costs: Some(&costs),
                    cost_lattice: None,
                    water: None,
                    height_origin: -500.,
                    vertical_span: 1000.,
                }
            };
        }
        let audit = validate_road_set(&report, &env!(), &[0, 1], &points, 64_000_000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(audit.roads, vec![0, 1]);
        assert!(audit.source_costs_applicable && audit.whole_selected_routes_checked);
        assert!(!audit.runtime_access_validated);
        let sr = layers.surface_resolution;
        let cell = |v: f32, n: usize| ((v / 1000. + 0.5) * n as f32).floor() as usize;
        let index = cell(160., sr) * sr + cell(-120., sr);
        alpha[index] = 0;
        assert!(matches!(
            validate_road_set(&report, &env!(), &[0, 1], &points, 64_000_000, || false)
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::GroundBlocked {
                    ground: Some(GroundBlock {
                        reason: "terrain hole",
                        ..
                    }),
                    ..
                },
                ..
            })
        ));
        alpha[index] = 255;
        layers.topology[index] = settings.protected_topology;
        assert!(matches!(
            validate_road_set(&report, &env!(), &[0, 1], &points, 64_000_000, || false)
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::GroundBlocked { .. },
                ..
            })
        ));
        layers.topology[index] = 0;
        let index = (((160. / 1000. + 0.5) * 256.) as usize) * GRID
            + (((-120. / 1000. + 0.5) * 256.) as usize);
        costs[index] = 0;
        assert!(
            validate_road_set(&report, &env!(), &[0, 1], &points, 64_000_000, || false).is_err()
        );
        costs[index] = 1;
        let mut moved = points.clone();
        moved[1][4][0] += 1.;
        assert!(matches!(
            validate_road_set(&report, &env!(), &[0, 1], &moved, 64_000_000, || false)
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::Rejected("route network attachment moved"),
                ..
            })
        ));
        for invalid in [vec![], vec![0, 0], vec![1, 0], vec![2]] {
            assert!(
                validate_road_set(&report, &env!(), &invalid, &points, 64_000_000, || false)
                    .is_err()
            );
        }
        assert!(
            validate_road_set(&report, &env!(), &[0, 1], &points, 64_000_000, || true)
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            validate_road_set(&report, &env!(), &[0, 1], &points, 1, || false)
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::Budget,
                work: 1,
                ..
            })
        ));
        assert_eq!(
            points,
            report
                .roads
                .iter()
                .map(|r| r.points.clone())
                .collect::<Vec<_>>()
        );
    }
    #[test]
    fn indexed_tube_queries_match_full_scan_across_bucket_boundaries() {
        let points = (0..25)
            .map(|i| {
                let x = -480. + i as f32 * 40.;
                [x, 20. + i as f32 * 0.1, (i as f32 * 0.7).sin() * 65.]
            })
            .collect::<Vec<_>>();
        let path = Path::from_controls(&points, 10000, || false)
            .unwrap()
            .unwrap();
        let indexed = Footprint::new(&path, &vec![true; points.len()], 13.);
        assert!(indexed.spatial.is_some());
        let scanned = Footprint {
            curves: indexed.curves.clone(),
            radius: indexed.radius,
            spatial: None,
            build_work: 0,
        };
        let (mut fast, mut slow) = (0, 0);
        for x in -64..=64 {
            for z in -16..=16 {
                let point = [x as f32 * 8., z as f32 * 8.];
                assert_eq!(
                    indexed
                        .touches(point, &mut fast, 20_000_000, &|| false)
                        .unwrap(),
                    scanned
                        .touches(point, &mut slow, 20_000_000, &|| false)
                        .unwrap(),
                    "{point:?}"
                );
            }
        }
        assert!(fast < slow);
        assert!(
            indexed
                .touches([10000., 10000.], &mut fast, 20_000_000, &|| true)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn whole_route_check_rejects_remote_holes_and_protection_after_local_preflight() {
        let (mut layers, mut alpha, mut report, owners, costs, settings, ownership, launch) =
            fixture();
        // Extend the incident road far beyond the neighbourhood of point 1.
        report.roads.truncate(2);
        report.families[0].road_count = 2;
        let y = report.roads[1].points[0][1];
        report.roads[1].points = (0..=10).map(|i| [i as f32 * 20., y, 0.]).collect();
        let key = Key {
            monument: 1,
            connector: 11,
        };
        macro_rules! env {
            () => {
                Environment {
                    height_origin: -500.,
                    vertical_span: 1000.,
                    layers: &layers,
                    alpha: &alpha,
                    obstacles: &[],
                    owners: &owners,
                    settings,
                    ownership,
                    launch,
                    costs: Some(&costs),
                    cost_lattice: None,
                    water: None,
                }
            };
        }
        let proposal = propose(&report, &env!(), key, -6., 2_000_000, || false)
            .unwrap()
            .unwrap();
        let points = proposal.controls().to_vec();
        let audit = validate_routes(&report, &env!(), key, &points, 16_000_000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(audit.roads, vec![1]);
        assert!(audit.whole_incident_routes_checked && audit.work > 0);
        let larger = validate_routes(&report, &env!(), key, &points, 128_000_000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(larger.work, audit.work);
        assert_eq!(larger.roads, audit.roads);
        let n = layers.surface_resolution;
        let index =
            ((0.5_f32) * n as f32) as usize * n + ((180. / 1000. + 0.5) * n as f32) as usize;
        alpha[index] = 0;
        // The short socket proposal still passes; the complete route must not.
        assert!(
            propose(&report, &env!(), key, -6., 2_000_000, || { false })
                .unwrap()
                .is_some()
        );
        assert!(matches!(
            validate_routes(&report, &env!(), key, &points, 16_000_000, || false)
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::GroundBlocked { .. },
                ..
            })
        ));
        alpha[index] = 255;
        layers.topology[index] = settings.protected_topology;
        assert!(matches!(
            validate_routes(&report, &env!(), key, &points, 16_000_000, || false)
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::GroundBlocked { .. },
                ..
            })
        ));
        layers.topology[index] = 0;
        assert!(matches!(
            validate_routes(&report, &env!(), key, &points, 1, || false)
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::Budget,
                ..
            })
        ));
        assert!(
            validate_routes(&report, &env!(), key, &points, 16_000_000, || true)
                .unwrap()
                .is_none()
        );
        let mut moved_endpoint = points;
        moved_endpoint[1][0][0] += 0.01;
        assert!(matches!(
            validate_routes(&report, &env!(), key, &moved_endpoint, 16_000_000, || false)
                .as_ref()
                .map_err(Box::as_ref),
            Err(RouteFailure {
                error: Failure::Rejected("whole socket route endpoint moved"),
                ..
            })
        ));
    }
    type RecoveryFixture = (
        Layers,
        Vec<u8>,
        Report,
        Vec<Obstacle>,
        Vec<u32>,
        Settings,
        EntrancePolicy,
        LaunchPolicy,
    );
    fn fixture() -> RecoveryFixture {
        let (hr, sr) = crate::native_layers::dimensions(1000).unwrap();
        let h = crate::erosion::encode(20.);
        let layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![h; hr * hr],
            splat: vec![0; sr * sr * 8],
            biome: vec![0; sr * sr * 5],
            topology: vec![0; sr * sr],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        let y = crate::erosion::decode(h);
        let p = |x, z| [x, y, z];
        let socket = Destination::Monument {
            monument: 1,
            connector: 11,
        };
        let road = |from, to, points| Road { from, to, points };
        let launch = LaunchPolicy {
            engine: LaunchEngine::GlobalEarthwork,
            minimum_run_widths: 1.,
            sample_spacing: 1.,
            maximum_work: 2_000_000,
        };
        let report = Report {
            entrance_launch: Some(launch),
            roads: vec![
                road(
                    Destination::Loop,
                    Destination::Loop,
                    vec![p(-200., 0.), p(200., 0.)],
                ),
                road(
                    socket,
                    Destination::Network { road: 0, point: 1 },
                    vec![p(0., 0.), p(40., 0.), p(80., 0.), p(120., 0.), p(200., 0.)],
                ),
                road(
                    socket,
                    Destination::Monument {
                        monument: 2,
                        connector: 22,
                    },
                    vec![p(0., 0.), p(40., 0.), p(80., 0.), p(120., 0.), p(160., 60.)],
                ),
                road(
                    Destination::Monument {
                        monument: 3,
                        connector: 33,
                    },
                    socket,
                    vec![
                        p(160., -60.),
                        p(120., 0.),
                        p(80., 0.),
                        p(40., 0.),
                        p(0., 0.),
                    ],
                ),
            ],
            families: vec![Family {
                infrastructure_type: 0,
                hierarchy: 1,
                seed: 1,
                first_road: 0,
                road_count: 4,
                socket_count: 3,
                unresolved: vec![],
                width: 10.,
                splat_channel: 7,
            }],
            ..Default::default()
        };
        let owners = vec![Obstacle {
            monument: 1,
            center: [0., 0.],
            extent: [30., 30.],
            yaw: 0.,
        }];
        let settings = Settings {
            seed: 1,
            width: 10.,
            shoulder: 2.,
            maximum_grade: 0.2,
            maximum_cut_fill: 8.,
            terrain_offset: 0.,
            road_topology: 2048,
            roadside_topology: 4096,
            protected_topology: 1024,
            splat_channel: 7,
        };
        let ownership = EntrancePolicy {
            minimum_height: 1.,
            owned_topology: 1024,
            maximum_exit_distance: 512.,
            maximum_exit_nodes: 4096,
        };
        (
            layers,
            vec![255; sr * sr],
            report,
            owners,
            vec![1; GRID * GRID],
            settings,
            ownership,
            launch,
        )
    }
    #[test]
    fn source_cell_cost_footprints_keep_half_cell_origin_and_fail_closed_on_wrong_lattices() {
        let (layers, alpha, _, _, _, settings, ownership, launch) = fixture();
        let lattice = crate::road_costmap::CellLattice::new(1000., 125.).unwrap();
        let mut costs = vec![1; 64];
        costs[4 * 8 + 4] = 0;
        let e = Environment {
            height_origin: -500.,
            vertical_span: 1000.,
            layers: &layers,
            alpha: &alpha,
            obstacles: &[],
            owners: &[],
            settings,
            ownership,
            launch,
            costs: Some(&costs),
            cost_lattice: Some(lattice),
            water: None,
        };
        assert!(valid_cost_grid(&e));
        let key = Key {
            monument: 999,
            connector: 9,
        };
        for (p, expected) in [
            ([62.5, 62.5], false),
            ([-62.5, 62.5], true),
            ([0., 62.5], false),
        ] {
            assert_eq!(
                costs_clear(&e, key, p, 1., |_| Ok(Some(false))).unwrap(),
                Some(expected)
            );
        }
        assert_eq!(
            costs_clear(&e, key, [62.5, 62.5], 1., |_| Ok(None)).unwrap(),
            None
        );
        assert!(!valid_cost_grid(&Environment {
            cost_lattice: None,
            ..e
        }));
        assert!(!valid_cost_grid(&Environment { costs: None, ..e }));
        assert!(!valid_cost_grid(&Environment {
            cost_lattice: Some(crate::road_costmap::CellLattice::new(500., 62.5).unwrap()),
            ..e
        }));
    }
    #[test]
    fn convex_tube_never_excludes_dense_samples_of_source_float_curves() {
        let cases = [
            vec![
                [0., 20., 0.],
                [40., 29., -4.],
                [80., 31., 0.],
                [120., 22., 60.],
            ],
            vec![
                [400., 20., -100.],
                [430., 19., -110.],
                [470., 24., -90.],
                [490., 27., -50.],
            ],
            vec![
                [-200., 30., 200.],
                [-180., 34., 150.],
                [-140., 28., 120.],
                [-100., 25., 100.],
            ],
        ];
        let mut checked = 0;
        for points in cases {
            let path = Path::from_controls(&points, 10000, || false)
                .unwrap()
                .unwrap();
            let tube = Footprint::new(&path, &[false, true, false, false], 13.);
            let step = path.length() / (path.points().len() - 1) as f32;
            let mut work = 0;
            for interval in 0..3 {
                for sample in 0..=24 {
                    let p = path
                        .point((interval as f32 + sample as f32 / 24.) * step)
                        .unwrap();
                    for offset in [[12.5, 0.], [-12.5, 0.], [0., 12.5], [0., -12.5]] {
                        assert_eq!(
                            tube.touches(
                                [p[0] + offset[0], p[2] + offset[1]],
                                &mut work,
                                1_000_000,
                                &|| false
                            )
                            .unwrap(),
                            Some(true)
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert_eq!(checked, 900);
    }
    #[test]
    fn curve_tube_does_not_expand_frozen_socket_caps_with_sampling_padding() {
        let points = [
            [0., 20., 0.],
            [40., 20., 0.],
            [80., 20., 0.],
            [120., 20., 0.],
        ];
        let path = Path::from_controls(&points, 10000, || false)
            .unwrap()
            .unwrap();
        let tube = Footprint::new(&path, &[false, true, false, false], 13.);
        let mut work = 0;
        assert_eq!(
            tube.touches([-13.5, 0.], &mut work, 1000, &|| false)
                .unwrap(),
            Some(false)
        );
        assert_eq!(
            tube.touches([0., -12.5], &mut work, 1000, &|| false)
                .unwrap(),
            Some(true)
        );
        assert_eq!(
            tube.touches([20., 0.], &mut work, 1000, &|| false).unwrap(),
            Some(true)
        );
        assert_eq!(
            tube.touches([20., 0.], &mut 0, 0, &|| false),
            Err(Failure::Budget)
        );
        assert_eq!(
            tube.touches([20., 0.], &mut 0, 1000, &|| true).unwrap(),
            None
        );
    }
    #[test]
    fn subdivided_departures_rotate_the_complete_minimum_run_without_moving_sockets() {
        let (layers, alpha, mut report, owners, costs, settings, ownership, launch) = fixture();
        let key = Key {
            monument: 1,
            connector: 11,
        };
        let socket = Destination::Monument {
            monument: 1,
            connector: 11,
        };
        for road in &mut report.roads {
            let pair = if road.from == socket {
                Some((0, 1))
            } else if road.to == socket {
                Some((road.points.len() - 2, road.points.len() - 1))
            } else {
                None
            };
            if let Some((a, b)) = pair {
                let first = road.points[a];
                let last = road.points[b];
                let extra = (1..5)
                    .map(|i| {
                        std::array::from_fn(|axis| {
                            first[axis] + (last[axis] - first[axis]) * i as f32 / 5.
                        })
                    })
                    .collect::<Vec<_>>();
                road.points.splice(b..b, extra);
            }
        }
        let original = report
            .roads
            .iter()
            .map(|r| r.points.clone())
            .collect::<Vec<_>>();
        let fan = angles(&report, key).unwrap();
        assert_eq!(fan.len(), 6);
        assert!(fan[0].abs() < fan[2].abs() && fan[2].abs() < fan[4].abs());
        let env = Environment {
            layers: &layers,
            alpha: &alpha,
            obstacles: &[],
            owners: &owners,
            settings,
            ownership,
            launch,
            costs: Some(&costs),
            cost_lattice: None,
            water: None,
            height_origin: -500.,
            vertical_span: 1000.,
        };
        let proposal = propose(&report, &env, key, fan[0], 2_000_000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(proposal.audit().changes.len(), 6);
        proposal.apply(&mut report, || false).unwrap().unwrap();
        for (road, before) in report.roads.iter().zip(&original) {
            assert_eq!(road.points.first(), before.first());
            assert_eq!(road.points.last(), before.last());
            for side in 0..2 {
                if [road.from, road.to][side] != socket {
                    continue;
                }
                let at = |i| {
                    if side == 0 {
                        i
                    } else {
                        road.points.len() - 1 - i
                    }
                };
                let a = road.points[at(0)];
                let b = road.points[at(1)];
                let c = road.points[at(2)];
                assert!(
                    ((b[0] - a[0]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[0] - a[0])).abs() < 0.001
                );
                assert!(distance(a, c) >= settings.width * launch.minimum_run_widths);
            }
        }
        let bent = [[0., 20., 0.], [7., 20., 0.], [8., 20., 5.], [20., 20., 5.]];
        assert!(departure_controls(&bent, 0, 10., 1.).is_err());
        assert!(departure_controls(&bent[..3], 0, 10., 1.).is_err());
    }
    #[test]
    fn fan_is_two_sided_width_scaled_and_independent_of_seed_or_monument_names() {
        let (_, _, mut report, _, _, _, _, _) = fixture();
        let key = Key {
            monument: 1,
            connector: 11,
        };
        let fan = angles(&report, key).unwrap();
        assert_eq!(fan.len(), 6);
        for pair in fan.chunks_exact(2) {
            assert_eq!(pair[0], -pair[1]);
            assert!(pair[0] < 0. && pair[1] <= 10.);
        }
        report.families[0].seed = 999;
        assert_eq!(angles(&report, key).unwrap(), fan);
        report.families[0].width = 5.;
        assert!(angles(&report, key).unwrap()[0].abs() < fan[0].abs());
    }
    #[test]
    fn source_socket_proposals_retain_cost_comparisons_and_ground_rejections() {
        let (layers, mut alpha, report, owners, _, settings, ownership, launch) = fixture();
        let lattice = crate::road_costmap::CellLattice::new(1000., 10.).unwrap();
        let costs = vec![0; lattice.resolution().pow(2)];
        let key = Key {
            monument: 1,
            connector: 11,
        };
        macro_rules! env {
            () => {
                Environment {
                    height_origin: -500.,
                    vertical_span: 1000.,
                    layers: &layers,
                    alpha: &alpha,
                    obstacles: &[],
                    owners: &owners,
                    settings,
                    ownership,
                    launch,
                    costs: Some(&costs),
                    cost_lattice: Some(lattice),
                    water: None,
                }
            };
        }
        assert!(matches!(
            propose(&report, &env!(), key, -6., 2_000_000, || false),
            Err(Failure::CostBlocked { .. }) | Err(Failure::Rejected(_))
        ));
        let proposal = propose_source(&report, &env!(), key, -6., 2_000_000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(proposal.audit().source_costs_passed, Some(false));
        assert_eq!(
            proposal.audit().source_cost_policy,
            CostPolicy::RoutingGuide
        );
        assert!(proposal.audit().source_cost_diagnostics.blocked_samples > 0);
        assert!(!proposal.audit().final_access_validated);
        let mut missing = env!();
        missing.cost_lattice = None;
        assert!(matches!(
            propose_source(&report, &missing, key, -6., 2_000_000, || false),
            Err(Failure::Invalid(_))
        ));
        alpha.fill(0);
        assert!(matches!(
            propose_source(&report, &env!(), key, -6., 2_000_000, || false),
            Err(Failure::GroundBlocked { .. })
        ));
    }
    #[test]
    fn shared_socket_proposal_moves_all_incidence_and_commits_atomically() {
        let (layers, alpha, mut report, owners, costs, settings, ownership, launch) = fixture();
        let before = report.json();
        let height = layers.height.clone();
        let env = Environment {
            height_origin: -500.,
            vertical_span: 1000.,
            layers: &layers,
            alpha: &alpha,
            obstacles: &[],
            owners: &owners,
            settings,
            ownership,
            launch,
            costs: Some(&costs),
            cost_lattice: None,
            water: None,
        };
        let proposal = propose(
            &report,
            &env,
            Key {
                monument: 1,
                connector: 11,
            },
            -6.,
            2_000_000,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(proposal.audit().changes.len(), 3);
        assert_eq!(report.json(), before);
        let p = proposal.controls();
        assert_eq!(p[1][1], p[2][1]);
        assert_eq!(p[1][1], p[3][3]);
        for (old, new) in report.roads.iter().zip(p) {
            assert_eq!(old.points.first(), new.first());
            assert_eq!(old.points.last(), new.last());
            assert_eq!(old.points.len(), new.len());
        }
        let audit = proposal.apply(&mut report, || false).unwrap().unwrap();
        assert!(
            audit.routing_ground_checked
                && audit.source_costs_checked
                && !audit.final_access_validated
        );
        assert_eq!(report.roads[1].points[1], report.roads[3].points[3]);
        assert_eq!(report.socket_geometry_recovery.len(), 1);
        assert_eq!(layers.height, height);
    }
    #[test]
    fn stale_proposals_cancellation_and_network_aliases_do_not_commit() {
        let (layers, alpha, mut report, owners, costs, settings, ownership, launch) = fixture();
        let env = Environment {
            height_origin: -500.,
            vertical_span: 1000.,
            layers: &layers,
            alpha: &alpha,
            obstacles: &[],
            owners: &owners,
            settings,
            ownership,
            launch,
            costs: Some(&costs),
            cost_lattice: None,
            water: None,
        };
        let key = Key {
            monument: 1,
            connector: 11,
        };
        let proposal = propose(&report, &env, key, -6., 2_000_000, || false)
            .unwrap()
            .unwrap();
        let original = report.json();
        assert!(proposal.apply(&mut report, || true).unwrap().is_none());
        assert_eq!(report.json(), original);
        let proposal = propose(&report, &env, key, -6., 2_000_000, || false)
            .unwrap()
            .unwrap();
        report.roads[0].points[0][1] += 1.;
        let stale = report.json();
        assert!(matches!(
            proposal.apply(&mut report, || false),
            Err(Failure::Invalid(_))
        ));
        assert_eq!(report.json(), stale);
        report.roads[0].from = Destination::Network { road: 1, point: 1 };
        report.roads[0].points[0] = report.roads[1].points[1];
        assert!(matches!(
            propose(&report, &env, key, -6., 2_000_000, || false),
            Err(Failure::Rejected("socket control is a network attachment"))
        ));
    }
    #[test]
    fn holes_foreign_monuments_and_unowned_source_cells_reject_full_footprint() {
        let (layers, alpha, report, owners, costs, settings, ownership, launch) = fixture();
        let key = Key {
            monument: 1,
            connector: 11,
        };
        let mut env = Environment {
            height_origin: -500.,
            vertical_span: 1000.,
            layers: &layers,
            alpha: &alpha,
            obstacles: &[],
            owners: &owners,
            settings,
            ownership,
            launch,
            costs: Some(&costs),
            cost_lattice: None,
            water: None,
        };
        let obstacle = [Obstacle {
            monument: 99,
            center: [40., -4.],
            extent: [2., 2.],
            yaw: 0.,
        }];
        env.obstacles = &obstacle;
        let Err(Failure::GroundBlocked {
            obstacle: Some(blocked),
            reason,
            ..
        }) = propose(&report, &env, key, -6., 2_000_000, || false)
        else {
            panic!("Expected the exact foreign obstacle witness")
        };
        assert_eq!(*blocked, BlockedObstacle::from(&obstacle[0]));
        assert_eq!(reason, "foreign monument obstacle");
        let sr = layers.surface_resolution;
        let cell = |v: f32| ((v / 1000. + 0.5) * sr as f32) as usize;
        let mut hole = alpha.clone();
        hole[cell(-9.) * sr + cell(40.)] = 0;
        env.obstacles = &[];
        env.alpha = &hole;
        assert!(matches!(
            propose(&report, &env, key, -6., 2_000_000, || false),
            Err(Failure::GroundBlocked { .. })
        ));
        let mut blocked = costs.clone();
        let cell = |v: f32| ((v / 1000. + 0.5) * (GRID - 1) as f32).round() as usize;
        blocked[cell(-4.) * GRID + cell(40.)] = 0;
        env.alpha = &alpha;
        env.costs = Some(&blocked);
        assert!(matches!(
            propose(&report, &env, key, -6., 2_000_000, || false),
            Err(Failure::Rejected("socket source cost corridor is blocked"))
        ));
    }
    #[test]
    fn absent_optional_cost_raster_does_not_disable_required_ground_checks() {
        let (layers, alpha, report, owners, _, settings, ownership, launch) = fixture();
        let env = Environment {
            height_origin: -500.,
            vertical_span: 1000.,
            layers: &layers,
            alpha: &alpha,
            obstacles: &[],
            owners: &owners,
            settings,
            ownership,
            launch,
            costs: None,
            cost_lattice: None,
            water: None,
        };
        let proposal = propose(
            &report,
            &env,
            Key {
                monument: 1,
                connector: 11,
            },
            -6.,
            2_000_000,
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(proposal.audit().routing_ground_checked);
        assert!(
            !proposal.audit().source_costs_applicable && !proposal.audit().source_costs_checked
        );
        let holes = vec![0; alpha.len()];
        let env = Environment {
            alpha: &holes,
            ..env
        };
        assert!(matches!(
            propose(
                &report,
                &env,
                Key {
                    monument: 1,
                    connector: 11
                },
                -6.,
                2_000_000,
                || false
            ),
            Err(Failure::GroundBlocked { .. })
        ));
    }
    #[test]
    fn invalid_bounds_budget_and_cancel_never_return_partial_proposal() {
        let (layers, alpha, mut report, owners, costs, settings, ownership, launch) = fixture();
        let env = Environment {
            height_origin: -500.,
            vertical_span: 1000.,
            layers: &layers,
            alpha: &alpha,
            obstacles: &[],
            owners: &owners,
            settings,
            ownership,
            launch,
            costs: Some(&costs),
            cost_lattice: None,
            water: None,
        };
        let key = Key {
            monument: 1,
            connector: 11,
        };
        let before = report.json();
        assert!(matches!(
            propose(&report, &env, key, -6., 1, || false),
            Err(Failure::Budget)
        ));
        assert!(
            propose(&report, &env, key, -6., 2_000_000, || true)
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            propose(&report, &env, key, f64::NAN, 2_000_000, || false),
            Err(Failure::Invalid(_))
        ));
        assert_eq!(report.json(), before);
        report.families[0].first_road = 1;
        report.families[0].road_count = usize::MAX;
        assert!(matches!(
            propose(&report, &env, key, -6., 2_000_000, || false),
            Err(Failure::Invalid(_))
        ));
    }
}
