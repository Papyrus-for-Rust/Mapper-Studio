//! Adapt raw source connectivity to shaped road controls before retained planning.
//! Geometry preparation only: terrain, strip, grade and mesh acceptance follow.
use crate::{
    path_geometry::Path,
    road_costmap::{CellGrid, CellLattice},
    source_road_ring::RawNode,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

pub const VERSION: &str = "papyrus-source-shaped-road-junctions-1";
pub const STRIP_CHECKED_VERSION: &str = "papyrus-source-shaped-road-junctions-2";
pub const SOURCE_CHECKED_VERSION: &str = "papyrus-source-shaped-road-junctions-3";
pub const RAW_VERSION: &str = "papyrus-source-raw-road-junctions-1";
pub const JUNCTION_CHECKED_VERSION: &str = "papyrus-source-shaped-road-junctions-4";
/// Proven raw-cell graph relation. Construction is private: callers obtain it
/// only from the shared-node handoff, not by asserting a near road is connected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct RawConnection {
    road: usize,
    endpoint: usize,
    parent_road: usize,
    raw_cell: [usize; 2],
    anchor_xz_bits: [u32; 2],
    reference_parent_point: usize,
}
impl RawConnection {
    pub fn road(self) -> usize {
        self.road
    }
    pub fn endpoint(self) -> usize {
        self.endpoint
    }
    pub fn parent_road(self) -> usize {
        self.parent_road
    }
    pub fn raw_cell(self) -> [usize; 2] {
        self.raw_cell
    }
    pub fn reference_parent_point(self) -> usize {
        self.reference_parent_point
    }
    /// Preserve the source cell and anchor when an editor inserts or removes
    /// controls elsewhere on the parent curve.
    pub fn with_reference_parent_point(mut self, point: usize) -> Self {
        self.reference_parent_point = point;
        self
    }
    pub fn anchor(self) -> [f32; 2] {
        self.anchor_xz_bits.map(f32::from_bits)
    }
    /// Terrain draping can change Y. Source XZ, edge identity and dependency
    /// order cannot change without rebuilding the source handoff.
    pub fn matches(self, road: usize, endpoint: usize, road_count: usize, p: [f32; 3]) -> bool {
        self.road == road
            && self.endpoint == endpoint
            && self.road < road_count
            && self.parent_road < self.road
            && [p[0].to_bits(), p[2].to_bits()] == self.anchor_xz_bits
    }
}
#[derive(Clone, Copy)]
pub struct Routing<'a> {
    /// Original source grids indexed by hierarchy, before retry guidance.
    pub grids: [Option<&'a CellGrid>; 3],
    /// Same closed-cell margin used by final source-route validation.
    pub margin: f32,
}
pub struct Road<'a> {
    pub lattice: CellLattice,
    pub nodes: &'a [RawNode],
    pub path: &'a Path,
    /// True for authored monument terminals, in source start/end order.
    pub terminals: [bool; 2],
    pub hierarchy: u8,
}
#[derive(Clone, Copy)]
pub struct Settings {
    pub maximum_snap_distance: f64,
    pub maximum_work: usize,
}
#[derive(Debug, PartialEq)]
pub enum Failure {
    Invalid(&'static str),
    Budget {
        work: usize,
    },
    MissingParent {
        road: usize,
        endpoint: usize,
    },
    SnapDistance {
        road: usize,
        endpoint: usize,
        distance: f64,
        maximum: f64,
    },
    Geometry {
        road: usize,
    },
    Routing {
        road: usize,
        endpoint: usize,
    },
}
#[derive(Clone, Debug, Serialize)]
pub struct Attachment {
    pub road: usize,
    pub endpoint: usize,
    pub parent_road: usize,
    pub parent_point: usize,
    pub raw_cell: [usize; 2],
    pub before: [f32; 3],
    pub after: [f32; 3],
    pub horizontal_distance: f64,
}
#[derive(Debug, Serialize)]
pub struct Network {
    pub engine: &'static str,
    pub paths: Vec<Path>,
    pub attachments: Vec<Attachment>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub raw_connections: Vec<RawConnection>,
    pub work: usize,
    pub maximum_horizontal_snap: f64,
    pub terrain_and_mesh_validated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_cost_margin: Option<f32>,
}
struct Meter<'a, F> {
    work: usize,
    maximum: usize,
    cancelled: &'a F,
}
impl<F: Fn() -> bool> Meter<'_, F> {
    fn spend(&mut self, n: usize) -> Result<bool, Failure> {
        if (self.cancelled)() {
            return Ok(false);
        }
        if n > self.maximum.saturating_sub(self.work) {
            return Err(Failure::Budget { work: self.work });
        }
        self.work += n;
        Ok(true)
    }
}
/// Candidate parents must share the raw junction cell. A nearby unrelated road
/// is never substituted. Parents belong to the earlier retained raw network.
/// Parent controls are immutable interiors, loop controls
/// or authored terminals, so resolving another branch cannot move the target.
pub fn attach(
    roads: &[Road<'_>],
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Network>, Failure> {
    attach_impl(roads, None, None, false, settings, cancelled)
}

/// Preserve source endpoint positions and prove only the shared-cell graph.
/// Parent control indices are references for later geometry inspection, not
/// exact endpoint targets or physical connectivity certificates.
pub fn attach_raw(
    roads: &[Road<'_>],
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Network>, Failure> {
    attach_impl(roads, None, None, true, settings, cancelled)
}

/// Resolve source-network attachments without introducing a nominal-width fold.
/// Alternatives stay on the same raw-related parent and inside the snap limit.
/// Historical recipes retain `attach` and its original nearest-control policy.
pub fn attach_strip_checked(
    roads: &[Road<'_>],
    widths: &[f32],
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Network>, Failure> {
    attach_impl(
        roads,
        Some(WidthPolicy::all(widths)),
        None,
        false,
        settings,
        cancelled,
    )
}

/// Connect every source branch to its shaped parent and resolve strip folds in
/// the changed endpoint neighbourhoods before downstream recovery pins them.
/// The common terrain-aware stage assesses each complete road after handoff.
pub fn attach_junction_checked(
    roads: &[Road<'_>],
    widths: &[f32],
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Network>, Failure> {
    attach_impl(
        roads,
        Some(WidthPolicy {
            values: widths,
            attachments_only: true,
        }),
        None,
        false,
        settings,
        cancelled,
    )
}

#[derive(Clone, Copy)]
struct WidthPolicy<'a> {
    values: &'a [f32],
    attachments_only: bool,
}
impl<'a> WidthPolicy<'a> {
    fn all(values: &'a [f32]) -> Self {
        Self {
            values,
            attachments_only: false,
        }
    }

    fn check<F: Fn() -> bool>(
        self,
        road: usize,
        path: &Path,
        endpoints: [bool; 2],
        meter: &mut Meter<'_, F>,
    ) -> Result<Option<bool>, Failure> {
        if !self.attachments_only {
            return strip_clear(path, self.values[road], meter);
        }
        if !meter.spend(path.points().len())? {
            return Ok(None);
        }
        let mut affected = vec![false; path.points().len()];
        affected[0] = endpoints[0];
        let last = affected.len() - 1;
        affected[last] = endpoints[1];
        let cancelled = meter.cancelled;
        crate::road_strip::check(
            path,
            &affected,
            self.values[road] * 0.5,
            |n| meter.spend(n).map(|_| ()),
            cancelled,
        )
    }
}

/// Source-only indexed attachments must remain walkable in the child's grid.
/// No parent ownership, blocked cells or snap distance is borrowed/expanded.
pub fn attach_source_checked(
    roads: &[Road<'_>],
    widths: &[f32],
    routing: Routing<'_>,
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Network>, Failure> {
    attach_impl(
        roads,
        Some(WidthPolicy::all(widths)),
        Some(routing),
        false,
        settings,
        cancelled,
    )
}

fn attach_impl(
    roads: &[Road<'_>],
    widths: Option<WidthPolicy<'_>>,
    routing: Option<Routing<'_>>,
    raw: bool,
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Network>, Failure> {
    if widths.is_some_and(|widths| {
        widths.values.len() != roads.len()
            || widths
                .values
                .iter()
                .any(|w| !w.is_finite() || !(0.1..=128.).contains(w))
    }) {
        return Err(Failure::Invalid("junction widths"));
    }
    if roads.len() > 4096
        || !(1..=128_000_000).contains(&settings.maximum_work)
        || !settings.maximum_snap_distance.is_finite()
        || !(0.01..=256.).contains(&settings.maximum_snap_distance)
    {
        return Err(Failure::Invalid("junction settings"));
    }
    let mut meter = Meter {
        work: 0,
        maximum: settings.maximum_work,
        cancelled: &cancelled,
    };
    macro_rules! spend {
        ($n:expr) => {
            if !meter.spend($n)? {
                return Ok(None);
            }
        };
    }
    spend!(0);
    if let Some(routing) = routing {
        if widths.is_none() || !routing.margin.is_finite() || !(0.01..=8.).contains(&routing.margin)
        {
            return Err(Failure::Invalid("junction source cost margin"));
        }
        for (hierarchy, grid) in routing.grids.iter().enumerate() {
            let Some(grid) = grid else {
                continue;
            };
            if grid.grid.engine != crate::road_costmap::CELL_VERSION
                || grid.grid.resolution != grid.lattice.resolution()
                || grid.grid.costs.len() != grid.lattice.resolution().pow(2)
                || grid.grid.trail != (hierarchy == 2)
            {
                return Err(Failure::Invalid("junction source grid"));
            }
            for chunk in grid.grid.costs.chunks(4096) {
                spend!(chunk.len());
                if chunk.iter().any(|&v| v > 1_000_000) {
                    return Err(Failure::Invalid("junction source costs"));
                }
            }
        }
    }
    let mut index: BTreeMap<[usize; 2], BTreeSet<usize>> = BTreeMap::new();
    let mut raw_count = 0usize;
    for (id, road) in roads.iter().enumerate() {
        spend!(1);
        let n = road.lattice.resolution();
        if road.hierarchy > 2
            || roads.first().is_some_and(|r| r.lattice != road.lattice)
            || !(2..=262144).contains(&road.nodes.len())
            || (road.path.is_circular() && road.terminals.iter().any(|v| *v))
            || road.path.range() != (0, road.path.points().len() - 1)
        {
            return Err(Failure::Invalid("road lattice, terminals or hierarchy"));
        }
        if routing.is_some_and(|routing| {
            routing.grids[road.hierarchy as usize].is_none_or(|grid| grid.lattice != road.lattice)
        }) {
            return Err(Failure::Invalid("missing or foreign junction family grid"));
        }
        raw_count = raw_count
            .checked_add(road.nodes.len())
            .filter(|&n| n <= 2_000_000)
            .ok_or(Failure::Invalid("raw node budget"))?;
        for node in road.nodes {
            spend!(index.len().max(1).ilog2() as usize + 2);
            if node.cell.iter().any(|&v| v >= n) {
                return Err(Failure::Invalid("raw junction cell"));
            }
            index.entry(node.cell).or_default().insert(id);
        }
    }
    let mut attachments = Vec::new();
    for (id, road) in roads.iter().enumerate() {
        spend!(1);
        if road.path.is_circular() {
            continue;
        }
        let last = road.path.points().len() - 1;
        for endpoint in 0..2 {
            spend!(1);
            if road.terminals[endpoint] {
                continue;
            }
            let raw_cell = road.nodes[if endpoint == 0 {
                0
            } else {
                road.nodes.len() - 1
            }]
            .cell;
            let before = road.path.points()[if endpoint == 0 { 0 } else { last }];
            if raw {
                let center = road
                    .lattice
                    .point(raw_cell)
                    .ok_or(Failure::Invalid("raw endpoint cell"))?;
                if (before[0] - center[0]).abs() > 0.001 || (before[2] - center[1]).abs() > 0.001 {
                    return Err(Failure::Invalid("source endpoint differs from raw cell"));
                }
            }
            spend!(index.len().max(1).ilog2() as usize + 1);
            let parents = index
                .get(&raw_cell)
                .ok_or(Failure::MissingParent { road: id, endpoint })?;
            let mut best: Option<(f64, u8, usize, usize)> = None;
            for &parent_id in parents {
                spend!(1);
                if parent_id >= id {
                    continue;
                }
                let parent = &roads[parent_id];
                // Later infrastructure cannot be the parent of an earlier family.
                if parent.hierarchy > road.hierarchy {
                    continue;
                }
                let end = parent.path.points().len() - 1;
                for (point_id, &point) in parent.path.points().iter().enumerate() {
                    spend!(1);
                    if (parent.path.is_circular() && point_id == end)
                        || (!parent.path.is_circular()
                            && ((point_id == 0 && !parent.terminals[0])
                                || (point_id == end && !parent.terminals[1])))
                    {
                        continue;
                    }
                    let distance = (f64::from(point[0]) - f64::from(before[0]))
                        .hypot(f64::from(point[2]) - f64::from(before[2]));
                    let candidate = (distance, parent.hierarchy, parent_id, point_id);
                    if best.as_ref().is_none_or(|old| {
                        candidate
                            .0
                            .total_cmp(&old.0)
                            .then(candidate.1.cmp(&old.1))
                            .then(candidate.2.cmp(&old.2))
                            .then(candidate.3.cmp(&old.3))
                            .is_lt()
                    }) {
                        best = Some(candidate);
                    }
                }
            }
            let Some((distance, _, parent_road, parent_point)) = best else {
                return Err(Failure::MissingParent { road: id, endpoint });
            };
            if distance > settings.maximum_snap_distance {
                return Err(Failure::SnapDistance {
                    road: id,
                    endpoint,
                    distance,
                    maximum: settings.maximum_snap_distance,
                });
            }
            attachments.push(Attachment {
                road: id,
                endpoint,
                parent_road,
                parent_point,
                raw_cell,
                before,
                after: roads[parent_road].path.points()[parent_point],
                horizontal_distance: distance,
            });
        }
    }
    let mut paths = Vec::with_capacity(roads.len());
    for (id, road) in roads.iter().enumerate() {
        spend!(1);
        let mut endpoints = [None, None];
        for attachment in &attachments {
            spend!(1);
            if attachment.road == id {
                endpoints[attachment.endpoint] = Some(attachment.after);
            }
        }
        let mut path = if !raw && endpoints.iter().any(Option::is_some) {
            let available = settings
                .maximum_work
                .saturating_sub(meter.work)
                .min(32_000_000);
            if available == 0 {
                return Err(Failure::Budget { work: meter.work });
            }
            match road
                .path
                .reattach_endpoints(endpoints, available, &cancelled)
            {
                Ok(Some(path)) => {
                    spend!(path.work());
                    path
                }
                Ok(None) => return Ok(None),
                Err(crate::path_geometry::Failure::Budget) => {
                    return Err(Failure::Budget {
                        work: meter.work + available,
                    });
                }
                Err(_) => return Err(Failure::Geometry { road: id }),
            }
        } else {
            spend!(road.path.points().len() * 2);
            road.path.clone()
        };
        if path.is_circular() != road.path.is_circular()
            || (!path.is_circular() && path.points().first() == path.points().last())
        {
            return Err(Failure::Geometry { road: id });
        }
        if let Some(widths) = widths {
            let mut cost_clear = true;
            if let Some(routing) = routing {
                let grid = routing.grids[road.hierarchy as usize].unwrap();
                for attachment in attachments.iter().filter(|a| a.road == id) {
                    spend!(1);
                    match source_contact_clear(grid, attachment.after, routing.margin, &mut meter)?
                    {
                        None => return Ok(None),
                        Some(clear) => cost_clear &= clear,
                    }
                }
                if cost_clear {
                    match source_curve_clear(
                        road.path,
                        &path,
                        endpoints.map(|p| p.is_some()),
                        grid,
                        routing.margin,
                        &mut meter,
                    )? {
                        None => return Ok(None),
                        Some(clear) => cost_clear = clear,
                    }
                }
            }
            match widths.check(id, &path, endpoints.map(|p| p.is_some()), &mut meter)? {
                None => return Ok(None),
                Some(true) if cost_clear => (),
                Some(_) => {
                    let Some(repaired) = repair_attachment(
                        id,
                        roads,
                        &mut attachments,
                        widths,
                        routing,
                        settings,
                        &mut meter,
                    )?
                    else {
                        return Ok(None);
                    };
                    path = repaired;
                }
            }
        }
        paths.push(path);
    }
    for a in &attachments {
        spend!(1);
        let point = if a.endpoint == 0 {
            0
        } else {
            paths[a.road].points().len() - 1
        };
        let expected = if raw {
            a.before
        } else {
            paths[a.parent_road].points()[a.parent_point]
        };
        if paths[a.road].points()[point] != expected {
            return Err(Failure::Geometry { road: a.road });
        }
    }
    let maximum_horizontal_snap = attachments
        .iter()
        .map(|a| a.horizontal_distance)
        .fold(0., f64::max);
    let mut raw_connections = Vec::new();
    if raw {
        spend!(attachments.len());
        raw_connections = attachments
            .iter()
            .map(|a| RawConnection {
                road: a.road,
                endpoint: a.endpoint,
                parent_road: a.parent_road,
                raw_cell: a.raw_cell,
                anchor_xz_bits: [a.before[0].to_bits(), a.before[2].to_bits()],
                reference_parent_point: a.parent_point,
            })
            .collect();
    }
    Ok((!cancelled()).then_some(Network {
        engine: if raw {
            RAW_VERSION
        } else if widths.is_some_and(|w| w.attachments_only) {
            JUNCTION_CHECKED_VERSION
        } else if routing.is_some() {
            SOURCE_CHECKED_VERSION
        } else if widths.is_some() {
            STRIP_CHECKED_VERSION
        } else {
            VERSION
        },
        paths,
        attachments,
        raw_connections,
        work: meter.work,
        maximum_horizontal_snap,
        terrain_and_mesh_validated: false,
        source_cost_margin: routing.map(|r| r.margin),
    }))
}

fn strip_clear<F: Fn() -> bool>(
    path: &Path,
    width: f32,
    meter: &mut Meter<'_, F>,
) -> Result<Option<bool>, Failure> {
    if !meter.spend(path.points().len())? {
        return Ok(None);
    }
    let affected = vec![true; path.points().len()];
    let cancelled = meter.cancelled;
    crate::road_strip::check(
        path,
        &affected,
        width * 0.5,
        |n| meter.spend(n).map(|_| ()),
        cancelled,
    )
}

/// Match the final source-route grid's closed-cell contact test. A center in a
/// walkable cell is insufficient if the saved margin touches a blocked neighbour.
fn source_contact_clear<F: Fn() -> bool>(
    grid: &CellGrid,
    point: [f32; 3],
    margin: f32,
    meter: &mut Meter<'_, F>,
) -> Result<Option<bool>, Failure> {
    source_contact_preserved(grid, point, None, margin, meter)
}

fn source_contact_preserved<F: Fn() -> bool>(
    grid: &CellGrid,
    point: [f32; 3],
    original: Option<[f32; 3]>,
    margin: f32,
    meter: &mut Meter<'_, F>,
) -> Result<Option<bool>, Failure> {
    if !meter.spend(32)? {
        return Ok(None);
    }
    let p = [point[0], point[2]];
    let lattice = grid.lattice;
    if p.iter()
        .any(|v| !v.is_finite() || v.abs() > lattice.size() * 0.5)
    {
        return Ok(Some(false));
    }
    let n = lattice.resolution();
    let cell = f64::from(lattice.size()) / n as f64;
    let q =
        p.map(|v| f64::from(((v + lattice.size() * 0.5) * (1. / lattice.size())) * n as f32) - 0.5);
    let original_q = original.map(|p| {
        [p[0], p[2]].map(|v| {
            f64::from(((v + lattice.size() * 0.5) * (1. / lattice.size())) * n as f32) - 0.5
        })
    });
    let r = f64::from(margin) / cell;
    let low = q.map(|v| (v - r - 0.5).ceil().clamp(0., (n - 1) as f64) as usize);
    let high = q.map(|v| (v + r + 0.5).floor().clamp(0., (n - 1) as f64) as usize);
    for z in low[1]..=high[1] {
        for x in low[0]..=high[0] {
            if !meter.spend(1)? {
                return Ok(None);
            }
            if grid.grid.costs[z * n + x] == 0 {
                let nearest = [
                    q[0].clamp(x as f64 - 0.5, x as f64 + 0.5),
                    q[1].clamp(z as f64 - 0.5, z as f64 + 0.5),
                ];
                if (nearest[0] - q[0]).hypot(nearest[1] - q[1]) <= r {
                    if original_q.is_some_and(|q| {
                        let nearest = [
                            q[0].clamp(x as f64 - 0.5, x as f64 + 0.5),
                            q[1].clamp(z as f64 - 0.5, z as f64 + 0.5),
                        ];
                        (nearest[0] - q[0]).hypot(nearest[1] - q[1]) <= r
                    }) {
                        continue;
                    }
                    return Ok(Some(false));
                }
            }
        }
    }
    Ok(Some(true))
}

/// Reattachment changes only the endpoint and neighbouring tangent, affecting
/// the first/last two spans. Reject new blocked cells there. Preserving an exact
/// cell contact at the same original parameter is not an ownership certificate:
/// the final complete route check must still accept the original exit/contact.
fn source_curve_clear<F: Fn() -> bool>(
    original: &Path,
    path: &Path,
    endpoints: [bool; 2],
    grid: &CellGrid,
    margin: f32,
    meter: &mut Meter<'_, F>,
) -> Result<Option<bool>, Failure> {
    let n = path.points().len();
    let step = path.length() / (n - 1) as f32;
    let old_step = original.length() / (n - 1) as f32;
    for i in 0..n - 1 {
        if !meter.spend(1)? {
            return Ok(None);
        }
        if !(endpoints[0] && i <= 1 || endpoints[1] && i + 3 >= n) {
            continue;
        }
        let a = path.points()[i];
        let b = path.points()[i + 1];
        let run = (a[0] - b[0]).hypot(a[2] - b[2]);
        let count = ((1.5 * run + 2. * step) / margin).ceil().max(2.) as usize;
        if count > 65536 {
            return Err(Failure::Budget { work: meter.work });
        }
        for sample in 0..=count {
            if !meter.spend(2)? {
                return Ok(None);
            }
            let parameter = i as f32 + sample as f32 / count as f32;
            let p = path
                .point(parameter * step)
                .map_err(|_| Failure::Invalid("attachment curve"))?;
            let before = original
                .point(parameter * old_step)
                .map_err(|_| Failure::Invalid("original attachment curve"))?;
            match source_contact_preserved(grid, p, Some(before), margin, meter)? {
                None => return Ok(None),
                Some(false) => return Ok(Some(false)),
                Some(true) => (),
            }
        }
    }
    Ok(Some(true))
}

fn repair_attachment<F: Fn() -> bool>(
    id: usize,
    roads: &[Road<'_>],
    attachments: &mut [Attachment],
    widths: WidthPolicy<'_>,
    routing: Option<Routing<'_>>,
    settings: Settings,
    meter: &mut Meter<'_, F>,
) -> Result<Option<Path>, Failure> {
    macro_rules! spend {
        ($n:expr) => {
            if !meter.spend($n)? {
                return Ok(None);
            }
        };
    }
    // Each endpoint has at most sixteen candidates. The original parent is
    // fixed; only its immutable controls may be considered as alternatives.
    let mut choices: [Vec<(usize, [f32; 3], f64)>; 2] = Default::default();
    let mut attachment_ids = [None, None];
    for (a_id, a) in attachments.iter().enumerate() {
        spend!(1);
        if a.road != id {
            continue;
        }
        attachment_ids[a.endpoint] = Some(a_id);
        let parent = &roads[a.parent_road];
        let end = parent.path.points().len() - 1;
        for (point_id, &point) in parent.path.points().iter().enumerate() {
            spend!(1);
            if (parent.path.is_circular() && point_id == end)
                || (!parent.path.is_circular()
                    && ((point_id == 0 && !parent.terminals[0])
                        || (point_id == end && !parent.terminals[1])))
            {
                continue;
            }
            let distance = (f64::from(point[0]) - f64::from(a.before[0]))
                .hypot(f64::from(point[2]) - f64::from(a.before[2]));
            if distance <= settings.maximum_snap_distance {
                if let Some(routing) = routing {
                    match source_contact_clear(
                        routing.grids[roads[id].hierarchy as usize].unwrap(),
                        point,
                        routing.margin,
                        meter,
                    )? {
                        None => return Ok(None),
                        Some(false) => continue,
                        Some(true) => (),
                    }
                }
                let choices = &mut choices[a.endpoint];
                spend!(80);
                choices.push((point_id, point, distance));
                choices.sort_by(|a, b| a.2.total_cmp(&b.2).then(a.0.cmp(&b.0)));
                choices.truncate(16);
            }
        }
    }
    for endpoint in 0..2 {
        if attachment_ids[endpoint].is_none() {
            choices[endpoint].push((0, [0.; 3], 0.));
        } else if routing.is_some() && choices[endpoint].is_empty() {
            return Err(Failure::Routing { road: id, endpoint });
        }
    }
    spend!(256 * 10);
    let mut pairs = Vec::with_capacity(choices[0].len() * choices[1].len());
    for (a, x) in choices[0].iter().enumerate() {
        for (b, y) in choices[1].iter().enumerate() {
            pairs.push((x.2 + y.2, a, b));
        }
    }
    pairs.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    for (_, a, b) in pairs {
        spend!(1);
        let selected = [choices[0][a], choices[1][b]];
        let endpoints = std::array::from_fn(|e| attachment_ids[e].map(|_| selected[e].1));
        let available = settings
            .maximum_work
            .saturating_sub(meter.work)
            .min(32_000_000);
        if available == 0 {
            return Err(Failure::Budget { work: meter.work });
        }
        let path = match roads[id]
            .path
            .reattach_endpoints(endpoints, available, meter.cancelled)
        {
            Ok(Some(path)) => {
                spend!(path.work());
                path
            }
            Ok(None) => return Ok(None),
            Err(crate::path_geometry::Failure::Budget) => {
                return Err(Failure::Budget {
                    work: meter.work + available,
                });
            }
            Err(_) => continue,
        };
        if path.points().first() == path.points().last() {
            continue;
        }
        if let Some(routing) = routing {
            match source_curve_clear(
                roads[id].path,
                &path,
                endpoints.map(|p| p.is_some()),
                routing.grids[roads[id].hierarchy as usize].unwrap(),
                routing.margin,
                meter,
            )? {
                None => return Ok(None),
                Some(false) => continue,
                Some(true) => (),
            }
        }
        match widths.check(id, &path, endpoints.map(|p| p.is_some()), meter)? {
            None => return Ok(None),
            Some(false) => continue,
            Some(true) => (),
        }
        for endpoint in 0..2 {
            if let Some(a_id) = attachment_ids[endpoint] {
                let a = &mut attachments[a_id];
                (a.parent_point, a.after, a.horizontal_distance) = selected[endpoint];
            }
        }
        return Ok(Some(path));
    }
    Err(Failure::Geometry { road: id })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn path(points: &[[f32; 3]]) -> Path {
        Path::from_controls(points, 100000, || false)
            .unwrap()
            .unwrap()
    }
    fn nodes(cells: &[[usize; 2]]) -> Vec<RawNode> {
        cells
            .iter()
            .enumerate()
            .map(|(i, &cell)| RawNode {
                cell,
                cost: i as u64,
            })
            .collect()
    }
    fn settings() -> Settings {
        Settings {
            maximum_snap_distance: 20.,
            maximum_work: 100000,
        }
    }
    #[test]
    fn raw_handoff_preserves_source_positions_and_binds_immutable_edge_identity() {
        let lattice = CellLattice::new(1000., 10.).unwrap();
        let parent = path(&[[-23., 0., 0.], [0., 0., 0.], [12., 0., 0.], [30., 0., 0.]]);
        let child = path(&[[-5., 0., 40.], [-5., 0., 20.], [-5., 0., 5.]]);
        let p = nodes(&[[48, 50], [49, 50], [51, 50]]);
        let c = nodes(&[[49, 54], [49, 50]]);
        let roads = [
            Road {
                lattice,
                nodes: &p,
                path: &parent,
                terminals: [true, true],
                hierarchy: 1,
            },
            Road {
                lattice,
                nodes: &c,
                path: &child,
                terminals: [true, false],
                hierarchy: 1,
            },
        ];
        let raw = attach_raw(&roads, settings(), || false).unwrap().unwrap();
        assert_eq!(raw.engine, RAW_VERSION);
        assert_eq!(raw.paths[0].points(), parent.points());
        assert_eq!(raw.paths[1].points(), child.points());
        assert_eq!(raw.paths[1].tangents(), child.tangents());
        assert_eq!(raw.raw_connections.len(), 1);
        let proof = raw.raw_connections[0];
        assert_eq!(
            (
                proof.road(),
                proof.endpoint(),
                proof.parent_road(),
                proof.raw_cell()
            ),
            (1, 1, 0, [49, 50])
        );
        assert_eq!(proof.reference_parent_point(), 1);
        assert_eq!(proof.anchor(), [-5., 5.]);
        assert!(proof.matches(1, 1, 2, [-5., 999., 5.]));
        assert!(!proof.matches(1, 0, 2, [-5., 0., 5.]));
        assert!(!proof.matches(0, 1, 2, [-5., 0., 5.]));
        assert!(!proof.matches(1, 1, 1, [-5., 0., 5.]));
        assert!(!proof.matches(1, 1, 2, [-4.99, 0., 5.]));
        assert!(!raw.terrain_and_mesh_validated);
        assert!(attach_raw(&roads, settings(), || true).unwrap().is_none());
        assert!(matches!(
            attach_raw(
                &roads,
                Settings {
                    maximum_work: 1,
                    ..settings()
                },
                || false
            ),
            Err(Failure::Budget { .. })
        ));
        let changed = path(&[[-5., 0., 40.], [-5., 0., 20.], [-4.99, 0., 5.]]);
        let moved = [
            Road {
                path: &parent,
                ..roads[0]
            },
            Road {
                path: &changed,
                ..roads[1]
            },
        ];
        assert!(matches!(
            attach_raw(&moved, settings(), || false),
            Err(Failure::Invalid("source endpoint differs from raw cell"))
        ));
    }
    #[test]
    fn original_cost_contacts_do_not_authorize_different_blocked_cells() {
        use crate::road_costmap::{CELL_VERSION, Counts, Grid};
        let lattice = CellLattice::new(1000., 125.).unwrap();
        let mut grid = CellGrid {
            lattice,
            grid: Grid {
                engine: CELL_VERSION,
                resolution: 8,
                seed: 1,
                trail: false,
                costs: vec![101; 64],
                counts: Counts::default(),
                work: 0,
            },
        };
        grid.grid.costs[4 * 8 + 4] = 0;
        grid.grid.costs[4 * 8 + 6] = 0;
        let mut meter = Meter {
            work: 0,
            maximum: 100000,
            cancelled: &|| false,
        };
        assert_eq!(
            source_contact_clear(&grid, [12., 0., 12.], 1., &mut meter).unwrap(),
            Some(false)
        );
        assert_eq!(
            source_contact_preserved(&grid, [12., 0., 12.], Some([10., 0., 10.]), 1., &mut meter)
                .unwrap(),
            Some(true)
        );
        assert_eq!(
            source_contact_preserved(&grid, [260., 0., 12.], Some([10., 0., 10.]), 1., &mut meter)
                .unwrap(),
            Some(false)
        );
        assert_eq!(grid.grid.costs[36], 0);
        assert_eq!(grid.grid.costs[38], 0);
    }
    #[test]
    fn source_attachment_avoids_blocked_parent_controls_without_borrowing_ownership() {
        use crate::road_costmap::{CELL_VERSION, Counts, Grid};
        let lattice = CellLattice::new(1000., 10.).unwrap();
        let parent = path(&[[-23., 0., 0.], [0., 0., 0.], [12., 0., 0.], [30., 0., 0.]]);
        let child = path(&[[-5., 0., 40.], [-5., 0., 20.], [-5., 0., 5.]]);
        let p = nodes(&[[48, 50], [49, 50], [51, 50]]);
        let c = nodes(&[[49, 54], [49, 50]]);
        let roads = [
            Road {
                lattice,
                nodes: &p,
                path: &parent,
                terminals: [true, true],
                hierarchy: 1,
            },
            Road {
                lattice,
                nodes: &c,
                path: &child,
                terminals: [true, false],
                hierarchy: 1,
            },
        ];
        let mut grid = CellGrid {
            lattice,
            grid: Grid {
                engine: CELL_VERSION,
                resolution: 100,
                seed: 1,
                trail: false,
                costs: vec![101; 10000],
                counts: Counts::default(),
                work: 0,
            },
        };
        grid.grid.costs[50 * 100 + 50] = 0;
        fn routing(grid: &CellGrid) -> Routing<'_> {
            Routing {
                grids: [None, Some(grid), None],
                margin: 1.,
            }
        }
        let old = attach_strip_checked(&roads, &[0.1, 0.1], settings(), || false)
            .unwrap()
            .unwrap();
        assert_eq!(old.attachments[0].parent_point, 1);
        // A walkable endpoint can still introduce a blocked bend on approach.
        let crossing = child
            .reattach_endpoints([None, Some(parent.points()[2])], 100000, || false)
            .unwrap()
            .unwrap();
        let mut probe = Meter {
            work: 0,
            maximum: 100000,
            cancelled: &|| false,
        };
        assert_eq!(
            source_contact_clear(&grid, parent.points()[2], 1., &mut probe).unwrap(),
            Some(true)
        );
        assert_eq!(
            source_curve_clear(&child, &crossing, [false, true], &grid, 1., &mut probe).unwrap(),
            Some(false)
        );
        assert_eq!(
            source_curve_clear(&child, &child, [false, true], &grid, 1., &mut probe).unwrap(),
            Some(true)
        );
        let out = attach_source_checked(&roads, &[0.1, 0.1], routing(&grid), settings(), || false)
            .unwrap()
            .unwrap();
        assert_eq!(out.engine, SOURCE_CHECKED_VERSION);
        assert_eq!(out.source_cost_margin, Some(1.));
        assert_eq!(
            (
                out.attachments[0].parent_road,
                out.attachments[0].parent_point
            ),
            (0, 0)
        );
        assert_eq!(out.paths[1].points()[2], parent.points()[0]);
        assert_eq!(&out.paths[1].points()[..2], &child.points()[..2]);
        assert_eq!(out.paths[0].points(), parent.points());
        assert_eq!(grid.grid.costs[5050], 0);
        assert!(out.work <= settings().maximum_work);
        assert!(
            attach_source_checked(&roads, &[0.1, 0.1], routing(&grid), settings(), || true)
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            attach_source_checked(
                &roads,
                &[0.1, 0.1],
                routing(&grid),
                Settings {
                    maximum_work: 100,
                    ..settings()
                },
                || false
            ),
            Err(Failure::Budget { .. })
        ));
        assert!(matches!(
            attach_source_checked(
                &roads,
                &[0.1, 0.1],
                routing(&grid),
                Settings {
                    maximum_snap_distance: 14.,
                    ..settings()
                },
                || false
            ),
            Err(Failure::Routing {
                road: 1,
                endpoint: 1
            })
        ));
        assert!(matches!(
            attach_source_checked(
                &roads,
                &[0.1, 0.1],
                Routing {
                    grids: [None; 3],
                    margin: 1.
                },
                settings(),
                || false
            ),
            Err(Failure::Invalid(_))
        ));
        let mut meter = Meter {
            work: 0,
            maximum: 100000,
            cancelled: &|| false,
        };
        for (point, clear) in [
            ([-0.9, 0., 5.], false),
            ([-1.1, 0., 5.], true),
            ([-0.6, 0., -0.6], false),
            ([-0.8, 0., -0.8], true),
        ] {
            assert_eq!(
                source_contact_clear(&grid, point, 1., &mut meter).unwrap(),
                Some(clear)
            );
        }
        grid.grid.costs.fill(0);
        assert!(matches!(
            attach_source_checked(&roads, &[0.1, 0.1], routing(&grid), settings(), || false),
            Err(Failure::Routing {
                road: 1,
                endpoint: 1
            })
        ));
    }
    #[test]
    fn strip_checked_attachment_uses_same_parent_without_moving_authored_terminal() {
        let lattice = CellLattice::new(1000., 10.).unwrap();
        let parent = path(&[[10., 0., 10.], [20., 0., 3.], [26., 0., 0.], [36., 0., -5.]]);
        let controls = (0..15)
            .map(|i| [-10. + i as f32 * 2., 0., 0.])
            .collect::<Vec<_>>();
        let child = path(&controls);
        let p = nodes(&[[51, 50], [52, 50]]);
        let c = nodes(&[[49, 50], [51, 50]]);
        let roads = [
            Road {
                lattice,
                nodes: &p,
                path: &parent,
                terminals: [true, true],
                hierarchy: 0,
            },
            Road {
                lattice,
                nodes: &c,
                path: &child,
                terminals: [true, false],
                hierarchy: 1,
            },
        ];
        let old = attach(&roads, settings(), || false).unwrap().unwrap();
        assert_eq!(old.attachments[0].parent_point, 1);
        let mut meter = Meter {
            work: 0,
            maximum: 100000,
            cancelled: &|| false,
        };
        assert_eq!(
            strip_clear(&old.paths[1], 10., &mut meter).unwrap(),
            Some(false)
        );
        let out = attach_strip_checked(&roads, &[0.1, 10.], settings(), || false)
            .unwrap()
            .unwrap();
        assert_eq!(out.engine, STRIP_CHECKED_VERSION);
        assert_eq!(
            (
                out.attachments[0].parent_road,
                out.attachments[0].parent_point
            ),
            (0, 2)
        );
        assert_eq!(out.paths[1].points().last(), Some(&parent.points()[2]));
        assert_eq!(&out.paths[1].points()[..14], &child.points()[..14]);
        assert_eq!(out.paths[0].points(), parent.points());
        assert_eq!(
            strip_clear(&out.paths[1], 10., &mut meter).unwrap(),
            Some(true)
        );
        assert!(out.work <= settings().maximum_work);
        let connected = attach_junction_checked(&roads, &[0.1, 10.], settings(), || false)
            .unwrap()
            .unwrap();
        assert_eq!(connected.engine, JUNCTION_CHECKED_VERSION);
        assert_eq!(connected.paths[1].points(), out.paths[1].points());
        assert!(connected.raw_connections.is_empty());
        // Dirt receives the same junction check and exact indexed connection.
        let dirt = [
            Road { ..roads[0] },
            Road {
                hierarchy: 2,
                ..roads[1]
            },
        ];
        let deferred = attach_junction_checked(&dirt, &[0.1, 10.], settings(), || false)
            .unwrap()
            .unwrap();
        assert_eq!(deferred.paths[1].points(), out.paths[1].points());
        assert_eq!(deferred.paths[1].points().first(), child.points().first());
        assert_eq!(deferred.paths[1].points().last(), Some(&parent.points()[2]));
        assert!(deferred.raw_connections.is_empty());
        assert!(!deferred.terrain_and_mesh_validated);
        // An existing fold at the opposite authored end remains visible to the
        // full-road terrain recovery stage and cannot prevent junction repair.
        let mut folded_controls = controls.clone();
        folded_controls[1][0] = -12.;
        let folded = path(&folded_controls);
        let inputs = [
            Road { ..roads[0] },
            Road {
                path: &folded,
                ..roads[1]
            },
        ];
        let connected = attach_junction_checked(&inputs, &[0.1, 10.], settings(), || false)
            .unwrap()
            .unwrap();
        assert_eq!(connected.paths[1].points().first(), folded.points().first());
        assert_eq!(
            connected.paths[1].points().last(),
            Some(&parent.points()[2])
        );
        assert_eq!(
            strip_clear(&connected.paths[1], 10., &mut meter).unwrap(),
            Some(false)
        );
        assert!(matches!(
            attach_strip_checked(&inputs, &[0.1, 10.], settings(), || false),
            Err(Failure::Geometry { road: 1 })
        ));
        assert!(
            attach_junction_checked(&roads, &[0.1, 10.], settings(), || true)
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            attach_junction_checked(&roads, &[10.], settings(), || false),
            Err(Failure::Invalid(_))
        ));
        assert!(matches!(
            attach_junction_checked(
                &roads,
                &[0.1, 10.],
                Settings {
                    maximum_work: 10,
                    ..settings()
                },
                || false
            ),
            Err(Failure::Budget { .. })
        ));
        assert!(
            attach_strip_checked(&roads, &[0.1, 10.], settings(), || true)
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            attach_strip_checked(&roads, &[10.], settings(), || false),
            Err(Failure::Invalid(_))
        ));
        assert!(matches!(
            attach_strip_checked(
                &roads,
                &[0.1, 10.],
                Settings {
                    maximum_work: 10,
                    ..settings()
                },
                || false
            ),
            Err(Failure::Budget { .. })
        ));
        assert!(matches!(
            attach_strip_checked(
                &roads,
                &[0.1, 10.],
                Settings {
                    maximum_snap_distance: 5.,
                    ..settings()
                },
                || false
            ),
            Err(Failure::Geometry { road: 1 })
        ));
    }
    #[test]
    fn branches_join_raw_parent_geometry_without_moving_interiors_or_monument_exits() {
        let lattice = CellLattice::new(1000., 10.).unwrap();
        let parent = path(&[[-20., 2., 0.], [0., 3., 0.], [20., 4., 0.]]);
        let child = path(&[[0., 3., 9.], [0., 5., 30.], [0., 6., 60.]]);
        let other = path(&[[0., 3., 8.], [0., 3., 9.], [0., 3., 10.]]);
        let p = nodes(&[[0, 0], [1, 0], [2, 0]]);
        let c = nodes(&[[1, 0], [1, 1], [1, 2]]);
        let o = nodes(&[[8, 8], [8, 9], [8, 10]]);
        let roads = [
            Road {
                lattice,
                nodes: &p,
                path: &parent,
                terminals: [true, true],
                hierarchy: 1,
            },
            Road {
                lattice,
                nodes: &o,
                path: &other,
                terminals: [true, true],
                hierarchy: 1,
            },
            Road {
                lattice,
                nodes: &c,
                path: &child,
                terminals: [false, true],
                hierarchy: 2,
            },
        ];
        let out = attach(&roads, settings(), || false).unwrap().unwrap();
        let a = &out.attachments[0];
        assert_eq!((a.parent_road, a.parent_point), (0, 1));
        assert_eq!(out.paths[2].points()[0], parent.points()[1]);
        assert_eq!(&out.paths[2].points()[1..], &child.points()[1..]);
        assert_eq!(out.paths[0].points(), parent.points());
        assert_eq!(child.points()[0], [0., 3., 9.]);
        assert!(!out.terrain_and_mesh_validated);
    }
    #[test]
    fn unmatched_or_distant_parents_reject_without_publishing_a_partial_network() {
        let lattice = CellLattice::new(1000., 10.).unwrap();
        let p = path(&[[0., 1., 0.], [0., 1., 5.], [0., 1., 10.]]);
        let nodes = nodes(&[[0, 0], [0, 1], [0, 2]]);
        let roads = [Road {
            lattice,
            nodes: &nodes,
            path: &p,
            terminals: [false, true],
            hierarchy: 1,
        }];
        assert!(matches!(
            attach(&roads, settings(), || false),
            Err(Failure::MissingParent {
                road: 0,
                endpoint: 0
            })
        ));
        assert!(attach(&roads, settings(), || true).unwrap().is_none());
        assert!(matches!(
            attach(
                &roads,
                Settings {
                    maximum_work: 1,
                    ..settings()
                },
                || false
            ),
            Err(Failure::Budget { .. })
        ));
        let distant = path(&[[100., 1., 100.], [100., 1., 110.], [100., 1., 120.]]);
        let roads = [
            Road {
                lattice,
                nodes: &nodes,
                path: &p,
                terminals: [true, true],
                hierarchy: 1,
            },
            Road {
                lattice,
                nodes: &nodes,
                path: &distant,
                terminals: [false, true],
                hierarchy: 2,
            },
        ];
        assert!(matches!(
            attach(&roads, settings(), || false),
            Err(Failure::SnapDistance {
                road: 1,
                endpoint: 0,
                ..
            })
        ));
    }
}
