//! Source-configured surface-rail terrain costs and independent weighted routing.
//! No terrain writes, rail assembly, socket joins or continuous curve validation.
use crate::erosion::decode;
use serde::{Deserialize, Serialize};
use std::{cmp::Reverse, collections::BinaryHeap};

pub const VERSION: &str = "papyrus-rail-costmap-1";
pub const ROUTING_VERSION: &str = "papyrus-weighted-rail-route-1";
pub const MULTI_ROUTING_VERSION: &str = "papyrus-multitarget-rail-route-1";
const MAX_WORK: usize = 128_000_000;
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub spacing: f32,
    pub near_radius: f32,
    pub clearance_radius: f32,
    pub blocked_topology: u32,
    pub clearance_topology: u32,
    pub water_topology: u32,
    pub cliff_topology: u32,
    pub maximum_slope: f32,
    pub steep_slope: f32,
    pub moderate_slope: f32,
    pub normal_cost: u32,
    pub moderate_cost: u32,
    pub steep_cost: u32,
}
impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        if [self.spacing, self.near_radius, self.clearance_radius]
            .iter()
            .any(|v| !v.is_finite() || !(0.1..=128.).contains(v))
            || [
                self.blocked_topology,
                self.clearance_topology,
                self.water_topology,
                self.cliff_topology,
            ]
            .contains(&0)
            || !self.maximum_slope.is_finite()
            || !(0. ..=90.).contains(&self.maximum_slope)
            || !self.steep_slope.is_finite()
            || !(0. ..self.maximum_slope).contains(&self.steep_slope)
            || !self.moderate_slope.is_finite()
            || !(0. ..self.steep_slope).contains(&self.moderate_slope)
            || self.normal_cost == 0
            || self.normal_cost > self.moderate_cost
            || self.moderate_cost > self.steep_cost
            || self.steep_cost > 1_000_000
        {
            return Err("Invalid surface rail cost policy".into());
        }
        Ok(())
    }
}

/// Row order is world -Z to +Z. Height endpoints and topology/placement texels
/// have different sample conventions; their resolutions need not match.
pub struct Terrain<'a> {
    pub size: f32,
    pub height_resolution: usize,
    pub height: &'a [i16],
    pub topology_resolution: usize,
    pub topology: &'a [i32],
    pub placement_resolution: usize,
    pub placement: &'a [u8],
}
#[derive(Debug, Default, Serialize)]
pub struct Counts {
    pub blocked_slope: usize,
    pub blocked_topology: usize,
    pub blocked_clearance: usize,
    pub water: usize,
    pub steep: usize,
    pub cliff: usize,
    pub placement: usize,
    pub moderate: usize,
    pub normal: usize,
}
#[derive(Debug)]
pub struct Grid {
    resolution: usize,
    size: f32,
    costs: Vec<u32>, // zero is the blocked sentinel, never a free cell
    heights: Vec<f32>,
    counts: Counts,
    work: usize,
}
impl Grid {
    pub fn size(&self) -> f32 {
        self.size
    }
    pub fn resolution(&self) -> usize {
        self.resolution
    }
    pub fn costs(&self) -> &[u32] {
        &self.costs
    }
    pub fn heights(&self) -> &[f32] {
        &self.heights
    }
    pub fn cell(&self, position: [f32; 2]) -> Result<[usize; 2], String> {
        if position
            .iter()
            .any(|v| !v.is_finite() || v.abs() > self.size * 0.5)
        {
            return Err("Rail endpoint lies outside the terrain".into());
        }
        Ok(position.map(|v| index(self.resolution, v / self.size + 0.5)))
    }
    pub fn point(&self, cell: [usize; 2]) -> Option<[f32; 3]> {
        let [x, z] = cell;
        (x < self.resolution && z < self.resolution).then(|| {
            [
                ((x as f32 + 0.5) / self.resolution as f32 - 0.5) * self.size,
                self.heights[z * self.resolution + x],
                ((z as f32 + 0.5) / self.resolution as f32 - 0.5) * self.size,
            ]
        })
    }
    pub fn audit(&self) -> serde_json::Value {
        serde_json::json!({"engine":VERSION,"resolution":self.resolution,"size":self.size,
            "cell_spacing":self.size/self.resolution as f32,"counts":self.counts,"work":self.work,
            "blocked_sentinel":0,"terrain_applied":false,"physics_validated":false})
    }
}
fn index(n: usize, uv: f32) -> usize {
    (uv * n as f32).clamp(0., (n - 1) as f32) as usize
}
fn visit(work: &mut usize, limit: usize) -> Result<(), String> {
    if *work >= limit {
        return Err("Rail terrain query work budget reached".into());
    }
    *work += 1;
    Ok(())
}
pub(crate) fn unit(v: [f32; 3]) -> [f32; 3] {
    let norm = v.iter().map(|v| v * v).sum::<f32>().sqrt();
    v.map(|v| v / norm)
}
/// Interpolate upward terrain unit normals, including nearly parallel normals.
pub fn slerp(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    let dot = a
        .iter()
        .zip(b)
        .map(|(a, b)| a * b)
        .sum::<f32>()
        .clamp(-1., 1.);
    // All terrain normals face upward. Normalized linear interpolation also
    // handles nearly opposing steep normals without dividing by sin(pi).
    if dot.abs() > 0.9995 {
        return unit(std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t));
    }
    if t == 0. && dot.is_finite() {
        // The spherical weights are exactly one and signed zero here.
        return unit(std::array::from_fn(|i| a[i] + b[i] * t));
    }
    let angle = dot.acos();
    let divisor = angle.sin();
    let l = ((1. - t) * angle).sin() / divisor;
    let r = (t * angle).sin() / divisor;
    unit(std::array::from_fn(|i| a[i] * l + b[i] * r))
}
/// Inspected HeightMapData convention: diagonal-corner finite differences,
/// size/res normal Y, followed by spherical interpolation of vertex normals.
/// This uses Rust floating point math; Unity's native Slerp is not executed.
pub(crate) fn sample(t: &Terrain<'_>, uv: [f32; 2]) -> (f32, f32) {
    sample_height_slope(t.size, t.height_resolution, t.height, uv)
}
pub(crate) fn sample_height_slope(
    size: f32,
    n: usize,
    heights: &[i16],
    uv: [f32; 2],
) -> (f32, f32) {
    let (height, normal) = sample_height_normal(size, n, heights, uv);
    (height, normal[1].clamp(-1., 1.).acos().to_degrees())
}
pub(crate) fn sample_height_normal(
    size: f32,
    n: usize,
    heights: &[i16],
    uv: [f32; 2],
) -> (f32, [f32; 3]) {
    let [x, z] = uv.map(|v| v * (n - 1) as f32);
    let (a, b) = (x as usize, z as usize);
    let (c, d) = ((a + 1).min(n - 1), (b + 1).min(n - 1));
    let at = |x: usize, z: usize| decode(heights[z * n + x]);
    let normal = |x: usize, z: usize| {
        let (l, r, f, k) = (
            x.saturating_sub(1),
            (x + 1).min(n - 1),
            z.saturating_sub(1),
            (z + 1).min(n - 1),
        );
        let base = at(l, f);
        unit([
            -(at(r, f) - base) * 0.5,
            size / n as f32,
            -(at(l, k) - base) * 0.5,
        ])
    };
    let lo = at(a, b) + (at(c, b) - at(a, b)) * x.fract();
    let hi = at(a, d) + (at(c, d) - at(a, d)) * x.fract();
    let norm = slerp(
        slerp(normal(a, b), normal(c, b), x.fract()),
        slerp(normal(a, d), normal(c, d), x.fract()),
        z.fract(),
    );
    (lo + (hi - lo) * z.fract(), norm)
}

// Binary row-prefix counts avoid scanning every texel in every radius query.
// Counts fit u16 because each bounded source row has at most 2048 samples.
pub(crate) struct Rows<const K: usize> {
    n: usize,
    counts: Vec<[u16; K]>,
    reciprocal_radius: bool,
}
impl<const K: usize> Rows<K> {
    pub(crate) fn new(
        n: usize,
        mut mask: impl FnMut(usize) -> [bool; K],
        work: &mut usize,
        limit: usize,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        let mut counts = vec![[0; K]; n * (n + 1)];
        for z in 0..n {
            if cancelled() {
                return Ok(None);
            }
            for x in 0..n {
                visit(work, limit)?;
                let m = mask(z * n + x);
                for (k, v) in m.into_iter().enumerate() {
                    counts[z * (n + 1) + x + 1][k] = counts[z * (n + 1) + x][k] + u16::from(v);
                }
            }
        }
        Ok(Some(Self {
            n,
            counts,
            reciprocal_radius: false,
        }))
    }
    /// Source TerrainMeta normalizes radii by multiplying its stored reciprocal.
    pub(crate) fn with_reciprocal_radius(mut self, enabled: bool) -> Self {
        self.reciprocal_radius = enabled;
        self
    }
    pub(crate) fn query(
        &self,
        uv: [f32; 2],
        radius: f32,
        size: f32,
        circular: bool,
        work: &mut usize,
        limit: usize,
    ) -> Result<[bool; K], String> {
        let r = if self.reciprocal_radius {
            radius * (1. / size)
        } else {
            radius / size
        };
        let [cx, cz] = uv.map(|v| index(self.n, v));
        let [lx, lz] = uv.map(|v| index(self.n, v - r));
        let [rx, rz] = uv.map(|v| index(self.n, v + r));
        let mut found = [false; K];
        for z in lz..=rz {
            visit(work, limit)?;
            let (mut lo, mut hi) = (lx, rx);
            if circular {
                // Preserve the inspected mixed-unit rule: the rectangle uses
                // world radius but the circle compares raw texel offsets.
                let dz = z.abs_diff(cz) as f32;
                let remaining = radius * radius - dz * dz;
                if remaining < 0. {
                    continue;
                }
                let mut dx = remaining.sqrt().floor() as usize;
                // A rounded square root can land just above the exact integer
                // boundary. Match the source's integer-square comparison.
                while dx > 0 && (dx * dx + z.abs_diff(cz).pow(2)) as f32 > radius * radius {
                    dx -= 1;
                }
                lo = lo.max(cx.saturating_sub(dx));
                hi = hi.min(cx.saturating_add(dx));
            }
            if lo > hi {
                continue;
            }
            for (k, v) in found.iter_mut().enumerate() {
                *v |= self.counts[z * (self.n + 1) + hi + 1][k]
                    != self.counts[z * (self.n + 1) + lo][k];
            }
            if found.iter().all(|v| *v) {
                break;
            }
        }
        Ok(found)
    }
}
pub fn build(
    t: Terrain<'_>,
    p: &Policy,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Grid>, String> {
    p.validate()?;
    if !t.size.is_finite()
        || !(1. ..=6000.).contains(&t.size)
        || !(2..=4097).contains(&t.height_resolution)
        || t.height.len() != t.height_resolution.pow(2)
        || !(2..=2048).contains(&t.topology_resolution)
        || t.topology.len() != t.topology_resolution.pow(2)
        || !(2..=2048).contains(&t.placement_resolution)
        || t.placement.len() != t.placement_resolution.pow(2)
        || maximum_work == 0
        || maximum_work > MAX_WORK
    {
        return Err("Invalid rail terrain grids or budget".into());
    }
    let n = (t.size / p.spacing) as usize;
    if !(2..=1024).contains(&n) {
        return Err("Rail cost grid exceeds supported resolution".into());
    }
    if cancelled() {
        return Ok(None);
    }
    let mut work = 0;
    let Some(near) = Rows::new(
        t.topology_resolution,
        |i| {
            let flags = t.topology[i] as u32;
            [
                flags & p.blocked_topology != 0,
                flags & p.water_topology != 0,
                flags & p.cliff_topology != 0,
                flags & p.clearance_topology != 0,
            ]
        },
        &mut work,
        maximum_work,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let mut invalid_blocker = false;
    let Some(placement) = Rows::new(
        t.placement_resolution,
        |i| {
            invalid_blocker |= t.placement[i] > 1;
            [t.placement[i] != 0]
        },
        &mut work,
        maximum_work,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    if invalid_blocker {
        return Err("Invalid rail placement-map cell".into());
    }
    let mut grid = Grid {
        resolution: n,
        size: t.size,
        costs: Vec::with_capacity(n * n),
        heights: Vec::with_capacity(n * n),
        counts: Counts::default(),
        work: 0,
    };
    for z in 0..n {
        if cancelled() {
            return Ok(None);
        }
        for x in 0..n {
            visit(&mut work, maximum_work)?;
            let uv = [(x as f32 + 0.5) / n as f32, (z as f32 + 0.5) / n as f32];
            let (height, slope) = sample(&t, uv);
            grid.heights.push(height);
            let cost = if slope > p.maximum_slope {
                grid.counts.blocked_slope += 1;
                0
            } else {
                let flags = near.query(uv, p.near_radius, t.size, true, &mut work, maximum_work)?;
                if flags[0] {
                    grid.counts.blocked_topology += 1;
                    0
                } else if near.query(
                    uv,
                    p.clearance_radius,
                    t.size,
                    true,
                    &mut work,
                    maximum_work,
                )?[3]
                {
                    grid.counts.blocked_clearance += 1;
                    0
                } else if flags[1] {
                    grid.counts.water += 1;
                    p.steep_cost
                } else if slope > p.steep_slope {
                    grid.counts.steep += 1;
                    p.steep_cost
                } else if flags[2] {
                    grid.counts.cliff += 1;
                    p.steep_cost
                } else if placement.query(
                    uv,
                    p.near_radius,
                    t.size,
                    false,
                    &mut work,
                    maximum_work,
                )?[0]
                {
                    grid.counts.placement += 1;
                    p.steep_cost
                } else if slope > p.moderate_slope {
                    grid.counts.moderate += 1;
                    p.moderate_cost
                } else {
                    grid.counts.normal += 1;
                    p.normal_cost
                }
            };
            grid.costs.push(cost);
        }
    }
    if cancelled() {
        return Ok(None);
    }
    grid.work = work;
    Ok(Some(grid))
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RouteSettings {
    pub seed: u32,
    pub maximum_grade: f32,
    pub maximum_expansions: usize,
}
#[derive(Debug, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Route {
    Found {
        cells: Vec<[usize; 2]>,
        points: Vec<[f32; 3]>,
        cost: u64,
        expansions: usize,
    },
    Unreachable {
        expansions: usize,
    },
    BudgetExceeded {
        expansions: usize,
    },
    Cancelled,
}
/// Geometric nearest-network lower bounds, prepared once for all monument
/// approaches. Obstacles are deliberately ignored here to keep it admissible.
pub struct Targets {
    resolution: usize,
    mask: Vec<bool>,
    distance: Vec<u16>,
    indices: Vec<usize>,
}
impl Targets {
    pub fn new(
        n: usize,
        goals: &[[usize; 2]],
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        if !(2..=1024).contains(&n)
            || !(1..=32768).contains(&goals.len())
            || goals.iter().flatten().any(|&v| v >= n)
        {
            return Err("Invalid rail routing target set".into());
        }
        if cancelled() {
            return Ok(None);
        }
        let mut mask = vec![false; n * n];
        let mut distance = vec![u16::MAX; n * n];
        let mut queue = Vec::with_capacity(n * n);
        for &[x, z] in goals {
            let i = z * n + x;
            if !mask[i] {
                mask[i] = true;
                distance[i] = 0;
                queue.push(i);
            }
        }
        let indices = queue.clone();
        let mut head = 0;
        while head < queue.len() {
            if head % 256 == 0 && cancelled() {
                return Ok(None);
            }
            let i = queue[head];
            head += 1;
            for dz in -1isize..=1 {
                for dx in -1isize..=1 {
                    if dx == 0 && dz == 0 {
                        continue;
                    }
                    let (Some(x), Some(z)) = (
                        (i % n).checked_add_signed(dx).filter(|&v| v < n),
                        (i / n).checked_add_signed(dz).filter(|&v| v < n),
                    ) else {
                        continue;
                    };
                    let j = z * n + x;
                    if distance[j] == u16::MAX {
                        distance[j] = distance[i] + 1;
                        queue.push(j);
                    }
                }
            }
        }
        Ok(if cancelled() {
            None
        } else {
            Some(Self {
                resolution: n,
                mask,
                distance,
                indices,
            })
        })
    }
    pub fn distance(&self, cell: [usize; 2]) -> Option<usize> {
        (cell[0] < self.resolution && cell[1] < self.resolution)
            .then(|| usize::from(self.distance[cell[1] * self.resolution + cell[0]]))
    }
}
enum Goal<'a> {
    One(usize),
    Many(&'a Targets),
}
/// Independent A*: accumulated u64 costs, deterministic ties and no diagonal
/// corner cutting. Unlike source PathFinder this guarantees the least-cost
/// discrete route within the budget. It does not validate a smoothed rail spline.
pub fn route(
    grid: &Grid,
    start: [usize; 2],
    end: [usize; 2],
    s: RouteSettings,
    cancelled: impl Fn() -> bool,
) -> Result<Route, String> {
    route_excluding(grid, start, end, s, None, cancelled)
}
/// Additional immutable exclusions used for connector protection and already
/// routed ring legs. A missing overlay retains the original routing behavior.
pub fn route_excluding(
    grid: &Grid,
    start: [usize; 2],
    end: [usize; 2],
    s: RouteSettings,
    exclusions: Option<&[bool]>,
    cancelled: impl Fn() -> bool,
) -> Result<Route, String> {
    let n = grid.resolution;
    if end.iter().any(|&v| v >= n) {
        return Err("Invalid weighted rail route endpoint".into());
    }
    search(
        grid,
        start,
        Goal::One(end[1] * n + end[0]),
        s,
        exclusions,
        cancelled,
    )
}
pub fn route_to_any(
    grid: &Grid,
    start: [usize; 2],
    targets: &Targets,
    s: RouteSettings,
    exclusions: Option<&[bool]>,
    cancelled: impl Fn() -> bool,
) -> Result<Route, String> {
    if targets.resolution != grid.resolution {
        return Err("Rail target grid differs from routing grid".into());
    }
    search(grid, start, Goal::Many(targets), s, exclusions, cancelled)
}
fn search(
    grid: &Grid,
    start: [usize; 2],
    goal: Goal<'_>,
    s: RouteSettings,
    exclusions: Option<&[bool]>,
    cancelled: impl Fn() -> bool,
) -> Result<Route, String> {
    let n = grid.resolution;
    if start.into_iter().any(|v| v >= n)
        || !s.maximum_grade.is_finite()
        || !(0. ..=1.).contains(&s.maximum_grade)
        || s.maximum_expansions == 0
        || s.maximum_expansions > 1_000_000
        || exclusions.is_some_and(|v| v.len() != n * n)
    {
        return Err("Invalid weighted rail route settings".into());
    }
    if cancelled() {
        return Ok(Route::Cancelled);
    }
    let start = start[1] * n + start[0];
    let blocked = |i: usize| grid.costs[i] == 0 || exclusions.is_some_and(|v| v[i]);
    if blocked(start)
        || match &goal {
            Goal::One(end) => blocked(*end),
            Goal::Many(t) => t.indices.iter().all(|&i| blocked(i)),
        }
    {
        return Ok(Route::Unreachable { expansions: 0 });
    }
    let minimum = u64::from(*grid.costs.iter().filter(|&&c| c > 0).min().unwrap());
    let heuristic = |i: usize| match &goal {
        Goal::One(end) => {
            let dx = (i % n).abs_diff(end % n) as u64;
            let dz = (i / n).abs_diff(end / n) as u64;
            minimum * (1414 * dx.min(dz) + 1000 * dx.abs_diff(dz))
        }
        Goal::Many(t) => minimum * 1000 * u64::from(t.distance[i]),
    };
    let tie = |i: usize| {
        let mut v = (i as u64) ^ (u64::from(s.seed) << 32);
        v = (v ^ (v >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        v = (v ^ (v >> 27)).wrapping_mul(0x94d049bb133111eb);
        v ^ (v >> 31)
    };
    let mut best = vec![u64::MAX; n * n];
    let mut parents = vec![usize::MAX; n * n];
    let mut closed = vec![false; n * n];
    let mut queue = BinaryHeap::new();
    best[start] = 0;
    queue.push(Reverse((heuristic(start), tie(start), 0u64, start)));
    let mut expansions = 0;
    let spacing = grid.size / n as f32;
    while let Some(Reverse((_, _, cost, i))) = queue.pop() {
        if cancelled() {
            return Ok(Route::Cancelled);
        }
        if closed[i] || best[i] != cost {
            continue;
        }
        if match &goal {
            Goal::One(end) => i == *end,
            Goal::Many(t) => t.mask[i],
        } {
            let mut cells = Vec::new();
            let mut at = i;
            loop {
                cells.push([at % n, at / n]);
                if at == start {
                    break;
                }
                at = parents[at];
            }
            cells.reverse();
            let points = cells.iter().map(|&c| grid.point(c).unwrap()).collect();
            return Ok(if cancelled() {
                Route::Cancelled
            } else {
                Route::Found {
                    cells,
                    points,
                    cost,
                    expansions,
                }
            });
        }
        if expansions >= s.maximum_expansions {
            return Ok(Route::BudgetExceeded { expansions });
        }
        expansions += 1;
        closed[i] = true;
        for dz in -1isize..=1 {
            for dx in -1isize..=1 {
                if dx == 0 && dz == 0 {
                    continue;
                }
                let (Some(x), Some(z)) = (
                    (i % n).checked_add_signed(dx).filter(|&x| x < n),
                    (i / n).checked_add_signed(dz).filter(|&z| z < n),
                ) else {
                    continue;
                };
                let j = z * n + x;
                if closed[j] || blocked(j) {
                    continue;
                }
                let diagonal = dx != 0 && dz != 0;
                if diagonal && (blocked((i / n) * n + x) || blocked(z * n + i % n)) {
                    continue;
                }
                let distance = spacing
                    * if diagonal {
                        std::f32::consts::SQRT_2
                    } else {
                        1.
                    };
                if (grid.heights[j] - grid.heights[i]).abs() / distance > s.maximum_grade {
                    continue;
                }
                let next = cost + u64::from(grid.costs[j]) * if diagonal { 1414 } else { 1000 };
                if next < best[j] {
                    best[j] = next;
                    parents[j] = i;
                    queue.push(Reverse((next + heuristic(j), tie(j), next, j)));
                }
            }
        }
    }
    Ok(if cancelled() {
        Route::Cancelled
    } else {
        Route::Unreachable { expansions }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn zero_spherical_weight_preserves_literal_interpolation_bits() {
        fn reference(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
            let dot = a
                .iter()
                .zip(b)
                .map(|(a, b)| a * b)
                .sum::<f32>()
                .clamp(-1., 1.);
            if dot.abs() > 0.9995 {
                return unit(std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t));
            }
            let angle = dot.acos();
            let divisor = angle.sin();
            let l = ((1. - t) * angle).sin() / divisor;
            let r = (t * angle).sin() / divisor;
            unit(std::array::from_fn(|i| a[i] * l + b[i] * r))
        }
        let mut values = vec![
            [0., 1., 0.],
            [-0., 1., -0.],
            [1., 0., 0.],
            [-1., 0., -0.],
            [0., -1., 0.],
            [0.; 3],
            [f32::MAX; 3],
            [f32::MIN; 3],
            [f32::INFINITY, 0., 0.],
            [f32::NAN, 1., 0.],
        ];
        let mut seed = 42_u32;
        for _ in 0..128 {
            let v = std::array::from_fn(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                seed as f32 / u32::MAX as f32 * 2. - 1.
            });
            values.push(unit(v));
        }
        for &a in &values {
            for &b in &values {
                for t in [0., -0., f32::MIN_POSITIVE, 0.25, 1., -1., 2., f32::NAN] {
                    let expected = reference(a, b, t);
                    let actual = slerp(a, b, t);
                    for (actual, expected) in actual.into_iter().zip(expected) {
                        assert!(
                            actual.to_bits() == expected.to_bits()
                                || actual.is_nan() && expected.is_nan(),
                            "{a:?}, {b:?}, {t:?}"
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn reciprocal_radius_keeps_source_float_boundary_without_changing_legacy_queries() {
        let mut work = 0;
        let rows = Rows::new(2048, |i| [i == 0], &mut work, 10000000, &|| false)
            .unwrap()
            .unwrap();
        let uv = [1. / 2048. + 5_f32 / 1000., 0.];
        assert_eq!(
            rows.query(uv, 5., 1000., false, &mut work, 10000000)
                .unwrap(),
            [false]
        );
        let rows = rows.with_reciprocal_radius(true);
        assert_eq!(
            rows.query(uv, 5., 1000., false, &mut work, 10000000)
                .unwrap(),
            [true]
        );
    }
    use crate::erosion::encode;
    use std::cell::Cell;
    fn policy() -> Policy {
        Policy {
            spacing: 2.,
            near_radius: 1.,
            clearance_radius: 3.,
            blocked_topology: 1,
            clearance_topology: 2,
            water_topology: 4,
            cliff_topology: 8,
            maximum_slope: 30.,
            steep_slope: 20.,
            moderate_slope: 10.,
            normal_cost: 1000,
            moderate_cost: 1500,
            steep_cost: 5000,
        }
    }
    fn terrain<'a>(h: &'a [i16], top: &'a [i32], block: &'a [u8]) -> Terrain<'a> {
        Terrain {
            size: 16.,
            height_resolution: 17,
            height: h,
            topology_resolution: 16,
            topology: top,
            placement_resolution: 16,
            placement: block,
        }
    }
    #[test]
    fn optimized_radius_queries_match_naive_source_queries_including_mixed_units() {
        let n = 32;
        let masks: Vec<_> = (0..n * n)
            .map(|i| [(i * 17 + 7) % 11 == 0, (i * 19 + 1) % 13 == 0])
            .collect();
        let rows = Rows::new(n, |i| masks[i], &mut 0, MAX_WORK, &|| false)
            .unwrap()
            .unwrap();
        for size in [32., 47., 1000.] {
            for radius in [0.1, 1., std::f32::consts::SQRT_2, 2.9999998, 5., 25.] {
                for uv in [[0., 0.], [0.001, 0.99], [0.47, 0.53], [0.5, 0.5], [1., 1.]] {
                    for circular in [false, true] {
                        let actual = rows
                            .query(uv, radius, size, circular, &mut 0, MAX_WORK)
                            .unwrap();
                        let [cx, cz] = uv.map(|v| index(n, v));
                        let [lx, lz] = uv.map(|v| index(n, v - radius / size));
                        let [rx, rz] = uv.map(|v| index(n, v + radius / size));
                        let mut expected = [false; 2];
                        for z in lz..=rz {
                            for x in lx..=rx {
                                if circular
                                    && (x.abs_diff(cx).pow(2) + z.abs_diff(cz).pow(2)) as f32
                                        > radius * radius
                                {
                                    continue;
                                }
                                for (k, v) in expected.iter_mut().enumerate() {
                                    *v |= masks[z * n + x][k];
                                }
                            }
                        }
                        assert_eq!(
                            actual, expected,
                            "size={size} r={radius} uv={uv:?} circle={circular}"
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn source_masks_radii_and_blocker_costs_drive_grid_without_modifying_inputs() {
        let h = vec![encode(50.); 17 * 17];
        let mut top = vec![0; 256];
        let mut blocked = vec![0; 256];
        let p = policy();
        let plain = build(terrain(&h, &top, &blocked), &p, MAX_WORK, || false)
            .unwrap()
            .unwrap();
        assert_eq!(plain.resolution, 8);
        assert!(plain.costs.iter().all(|&v| v == 1000));
        assert_eq!(plain.counts.normal, 64);
        assert!((plain.point([0, 0]).unwrap()[0] + 7.).abs() < 0.0001);
        assert_eq!(plain.cell([-8., 8.]).unwrap(), [0, 7]);
        assert!(plain.cell([8.01, 0.]).is_err());
        // A placement blocker is costly, not impassable; water and cliff use
        // the same cost category. Clearance reaches farther than near checks.
        blocked[7 * 16 + 7] = 1;
        top[3 * 16 + 3] = 4;
        top[13 * 16 + 13] = 8;
        let modified = build(terrain(&h, &top, &blocked), &p, MAX_WORK, || false)
            .unwrap()
            .unwrap();
        assert!(
            modified.counts.placement > 0 && modified.counts.water > 0 && modified.counts.cliff > 0
        );
        assert!(modified.costs.iter().all(|&v| v > 0));
        top[7 * 16 + 7] = 2;
        let clearance = build(terrain(&h, &top, &blocked), &p, MAX_WORK, || false)
            .unwrap()
            .unwrap();
        assert!(clearance.counts.blocked_clearance > 0);
        top.fill(1);
        let excluded = build(terrain(&h, &top, &blocked), &p, MAX_WORK, || false)
            .unwrap()
            .unwrap();
        assert_eq!(excluded.counts.blocked_topology, 64);
        assert_eq!(h, vec![encode(50.); 17 * 17]);
        assert_eq!(blocked[7 * 16 + 7], 1);
    }
    #[test]
    fn normal_sampling_uses_source_resolution_and_strict_slope_bands() {
        let mut p = policy();
        p.spacing = 125.;
        let top = vec![0; 256];
        let block = vec![0; 256];
        for (slope, category) in [(5f32, 0), (15., 1), (25., 2), (35., 3)] {
            let rise = slope.to_radians().tan() * 1000. / 17.;
            let h: Vec<_> = (0..17 * 17)
                .map(|i| encode(-300. + (i % 17) as f32 * rise))
                .collect();
            let mut t = terrain(&h, &top, &block);
            t.size = 1000.;
            let (_, angle) = sample(&t, [0.5, 0.5]);
            assert!((angle - slope).abs() < 0.1, "{angle} vs {slope}");
            let g = build(t, &p, MAX_WORK, || false).unwrap().unwrap();
            let center = g.costs[4 * 8 + 4];
            assert_eq!(center, [1000, 1500, 5000, 0][category]);
        }
    }
    #[test]
    fn malformed_cancelled_and_exhausted_builds_never_publish_partial_grids() {
        let h = vec![encode(50.); 17 * 17];
        let top = vec![0; 256];
        let mut blocked = vec![0; 256];
        assert!(
            build(terrain(&h, &top, &blocked), &policy(), MAX_WORK, || true)
                .unwrap()
                .is_none()
        );
        let calls = Cell::new(0);
        assert!(
            build(terrain(&h, &top, &blocked), &policy(), MAX_WORK, || {
                calls.set(calls.get() + 1);
                calls.get() > 20
            })
            .unwrap()
            .is_none()
        );
        assert!(build(terrain(&h, &top, &blocked), &policy(), 100, || false).is_err());
        blocked[10] = 2;
        assert!(build(terrain(&h, &top, &blocked), &policy(), MAX_WORK, || false).is_err());
        let mut p = policy();
        p.spacing = f32::NAN;
        assert!(build(terrain(&h, &top, &blocked), &p, MAX_WORK, || false).is_err());
    }
    fn route_grid() -> Grid {
        Grid {
            resolution: 9,
            size: 90.,
            costs: vec![1000; 81],
            heights: vec![20.; 81],
            counts: Counts::default(),
            work: 0,
        }
    }
    fn settings() -> RouteSettings {
        RouteSettings {
            seed: 42,
            maximum_grade: 0.1,
            maximum_expansions: 81,
        }
    }
    #[test]
    fn weighted_route_avoids_costly_corridors_and_preserves_cell_centers_and_inputs() {
        let mut g = route_grid();
        for z in 0..4 {
            g.costs[z * 9 + 4] = 5000;
        }
        let before = g.costs.clone();
        let run = || route(&g, [1, 1], [7, 1], settings(), || false).unwrap();
        let first = run();
        assert_eq!(first, run());
        let Route::Found {
            cells,
            points,
            cost,
            ..
        } = first
        else {
            panic!("expected route")
        };
        assert!(cells.iter().any(|p| p[1] >= 4));
        for (p, expected) in [
            (points.first().unwrap(), [-30., 20., -30.]),
            (points.last().unwrap(), [30., 20., -30.]),
        ] {
            assert!(
                p.iter()
                    .zip(expected)
                    .all(|(a, b)| (*a - b).abs() < 0.00001)
            );
        }
        assert!(cost < 10_000_000);
        assert_eq!(g.costs, before);
    }
    #[test]
    fn route_rejects_grade_and_diagonal_cutting_and_reports_budget_and_cancel() {
        let mut g = route_grid();
        g.costs[1] = 0;
        g.costs[9] = 0;
        assert_eq!(
            route(&g, [0, 0], [8, 8], settings(), || false).unwrap(),
            Route::Unreachable { expansions: 1 }
        );
        g.costs.fill(1000);
        g.heights[1] = 50.;
        g.heights[9] = 50.;
        g.heights[10] = 50.;
        assert_eq!(
            route(&g, [0, 0], [8, 8], settings(), || false).unwrap(),
            Route::Unreachable { expansions: 1 }
        );
        g.heights.fill(20.);
        let mut s = settings();
        s.maximum_expansions = 1;
        assert_eq!(
            route(&g, [0, 0], [8, 8], s, || false).unwrap(),
            Route::BudgetExceeded { expansions: 1 }
        );
        assert_eq!(
            route(&g, [0, 0], [8, 8], settings(), || true).unwrap(),
            Route::Cancelled
        );
        assert!(route(&g, [9, 0], [8, 8], settings(), || false).is_err());
    }
    #[test]
    fn astar_cost_matches_independent_relaxation_oracle_on_weighted_grids() {
        // Bellman-Ford oracle on small generated grids, with identical legal
        // edges but no heuristic, heap, tie breaker or parent reconstruction.
        for seed in 0..16 {
            let mut g = route_grid();
            for (i, c) in g.costs.iter_mut().enumerate() {
                *c = match (i * 17 + seed * 7) % 19 {
                    0..=2 => 0,
                    3..=7 => 5000,
                    8..=10 => 1500,
                    _ => 1000,
                };
            }
            g.costs[0] = 1000;
            g.costs[80] = 1000;
            let mut costs = vec![u64::MAX; 81];
            costs[0] = 0;
            for _ in 0..81 {
                let prev = costs.clone();
                let mut changed = false;
                for (i, &previous) in prev.iter().enumerate() {
                    if previous == u64::MAX {
                        continue;
                    }
                    for (j, current) in costs.iter_mut().enumerate() {
                        let (dx, dz) = ((i % 9).abs_diff(j % 9), (i / 9).abs_diff(j / 9));
                        if dx > 1 || dz > 1 || dx + dz == 0 || g.costs[j] == 0 {
                            continue;
                        }
                        let diagonal = dx == 1 && dz == 1;
                        if diagonal
                            && (g.costs[(i / 9) * 9 + j % 9] == 0
                                || g.costs[(j / 9) * 9 + i % 9] == 0)
                        {
                            continue;
                        }
                        let next =
                            previous + u64::from(g.costs[j]) * if diagonal { 1414 } else { 1000 };
                        if next < *current {
                            *current = next;
                            changed = true;
                        }
                    }
                }
                if !changed {
                    break;
                }
            }
            match route(&g, [0, 0], [8, 8], settings(), || false).unwrap() {
                Route::Found { cost, .. } => assert_eq!(cost, costs[80]),
                Route::Unreachable { .. } => assert_eq!(costs[80], u64::MAX),
                other => panic!("unexpected {other:?}"),
            }
        }
    }
    #[test]
    fn nearest_network_field_matches_direct_geometry_and_cancels_atomically() {
        let goals = [[2, 3], [7, 8], [2, 3]];
        let targets = Targets::new(9, &goals, || false).unwrap().unwrap();
        for z in 0usize..9 {
            for x in 0usize..9 {
                let expected = goals
                    .iter()
                    .map(|&[gx, gz]| x.abs_diff(gx).max(z.abs_diff(gz)))
                    .min()
                    .unwrap();
                assert_eq!(targets.distance([x, z]), Some(expected));
            }
        }
        assert!(Targets::new(9, &[], || false).is_err());
        assert!(Targets::new(9, &[[9, 0]], || false).is_err());
        assert!(Targets::new(9, &goals, || true).unwrap().is_none());
        let g = route_grid();
        let other = Targets::new(8, &[[1, 1]], || false).unwrap().unwrap();
        assert!(route_to_any(&g, [0, 0], &other, settings(), None, || false).is_err());
    }
    #[test]
    fn multi_target_search_matches_the_best_single_target_weighted_route() {
        for seed in 0..16 {
            let mut g = route_grid();
            for (i, c) in g.costs.iter_mut().enumerate() {
                *c = match (i * 17 + seed * 7) % 19 {
                    0..=2 => 0,
                    3..=7 => 5000,
                    8..=10 => 1500,
                    _ => 1000,
                };
            }
            let goals = [[8, 8], [8, 2], [1, 7]];
            g.costs[0] = 1000;
            let target = Targets::new(9, &goals, || false).unwrap().unwrap();
            let expected = goals
                .iter()
                .filter_map(
                    |&goal| match route(&g, [0, 0], goal, settings(), || false).unwrap() {
                        Route::Found { cost, .. } => Some(cost),
                        _ => None,
                    },
                )
                .min();
            match route_to_any(&g, [0, 0], &target, settings(), None, || false).unwrap() {
                Route::Found { cost, cells, .. } => {
                    assert_eq!(Some(cost), expected);
                    assert!(goals.contains(cells.last().unwrap()));
                }
                Route::Unreachable { .. } => assert!(expected.is_none()),
                other => panic!("unexpected multi-goal result {other:?}"),
            }
        }
    }
    #[test]
    fn additional_exclusions_are_immutable_and_never_override_blocked_endpoints() {
        let g = route_grid();
        let mut exclusions = vec![false; 81];
        for z in 0..8 {
            exclusions[z * 9 + 4] = true;
        }
        let before = exclusions.clone();
        let Route::Found { cells, .. } =
            route_excluding(&g, [1, 1], [7, 1], settings(), Some(&exclusions), || false).unwrap()
        else {
            panic!("expected overlay detour")
        };
        assert!(cells.iter().all(|c| !exclusions[c[1] * 9 + c[0]]));
        assert_eq!(exclusions, before);
        exclusions[10] = true;
        assert_eq!(
            route_excluding(&g, [1, 1], [7, 1], settings(), Some(&exclusions), || false).unwrap(),
            Route::Unreachable { expansions: 0 }
        );
        assert!(route_excluding(&g, [1, 1], [7, 1], settings(), Some(&[false]), || false).is_err());
    }
}
