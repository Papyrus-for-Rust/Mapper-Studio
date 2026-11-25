//! Cell-centred road search with the inspected PathFinder cost/visited contract.
//! Includes the inspected intrusive queue's head/tie rules. This is not a full
//! layout or mesh stage, and overflow is checked rather than emulated.
use crate::road_costmap::{CELL_VERSION, CellGrid};
use serde::{Deserialize, Serialize};
use std::{cmp::Reverse, collections::BinaryHeap};
#[cfg(feature = "road-diagnostics")]
mod access_capture;
mod goal_index;
use goal_index::GoalIndex;
pub const VERSION: &str = "papyrus-source-road-search-1";
pub const CONNECTED_ENTRANCE_VERSION: &str = "papyrus-connected-road-entrance-1";
pub const TERRAIN_ENTRANCE_VERSION: &str = "papyrus-terrain-road-entrance-1";
pub const TERRAIN_ENTRANCE_ROUTED_VERSION: &str = "papyrus-terrain-road-entrance-2";
pub const TERRAIN_ENTRANCE_BOOTSTRAP_VERSION: &str = "papyrus-terrain-road-entrance-3";
pub const TERRAIN_ENTRANCE_PINNED_VERSION: &str = "papyrus-terrain-road-entrance-4";
pub const TERRAIN_ACCESS_VERSION: &str = "papyrus-terrain-road-access-search-1";
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AccessDiagnostics {
    pub stop: String,
    pub work: usize,
    pub work_limit: usize,
    pub state_limit: usize,
    pub goal_samples: usize,
    pub walkable_goals: usize,
    pub expansions: usize,
    pub labels: usize,
    pub maximum_queue: usize,
    pub direct_comparisons: usize,
    pub indexed: bool,
    #[serde(default)]
    pub sparse_index: bool,
    #[serde(default)]
    pub heading_states: bool,
}
pub type AccessResult = Result<Outcome, String>;
/// Exact unobstructed Moore-neighbor distances, including through source zeros:
/// this is a lower bound, not a replacement walkability mask.
fn goal_distances<F: Fn() -> bool>(
    n: usize,
    goals: &[[usize; 2]],
    meter: &mut Meter<'_, F>,
) -> Result<Vec<u16>, Outcome> {
    meter.spend(n * n)?;
    let mut values = vec![u16::MAX; n * n];
    for &[x, z] in goals {
        meter.spend(1)?;
        values[z * n + x] = 0;
    }
    for reverse in [false, true] {
        for row in 0..n {
            let z = if reverse { n - 1 - row } else { row };
            for col in 0..n {
                meter.spend(5)?; // Cell plus four bounded neighbor checks.
                let x = if reverse { n - 1 - col } else { col };
                let offsets = if reverse {
                    [(1, 0), (-1, 1), (0, 1), (1, 1)]
                } else {
                    [(-1, 0), (-1, -1), (0, -1), (1, -1)]
                };
                let mut best = values[z * n + x];
                for (dx, dz) in offsets {
                    let a = x.checked_add_signed(dx).filter(|&v| v < n);
                    let b = z.checked_add_signed(dz).filter(|&v| v < n);
                    if let (Some(a), Some(b)) = (a, b) {
                        best = best.min(values[b * n + a].saturating_add(1));
                    }
                }
                values[z * n + x] = best;
            }
        }
    }
    Ok(values)
}
struct GoalHeuristic<'a> {
    n: usize,
    goals: &'a [[usize; 2]],
    minimum: u32,
    field: Option<Vec<u16>>,
    sparse: Option<GoalIndex>,
    comparisons: usize,
}

/// Foreign-footprint clearance for entrance recovery on the source lattice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EntranceFootprint {
    #[default]
    Clear,
    Monument(u64),
    Overlap,
}

impl GoalHeuristic<'_> {
    fn estimate<F: Fn() -> bool>(
        &mut self,
        cell: [usize; 2],
        meter: &mut Meter<'_, F>,
    ) -> Result<u64, Outcome> {
        let preparation = self.n * self.n * 11 + self.goals.len();
        let sparse_preparation = GoalIndex::preparation(self.goals.len());
        if self.field.is_none()
            && self.sparse.is_none()
            && self.goals.len() >= 32
            && sparse_preparation < preparation
            && self.comparisons >= sparse_preparation
            && meter.limit.saturating_sub(meter.work) >= sparse_preparation
        {
            self.sparse = Some(GoalIndex::new(self.goals, meter)?);
        }
        if self.field.is_none()
            && self.sparse.is_none()
            && self.comparisons >= preparation
            && meter.limit.saturating_sub(meter.work) >= preparation
        {
            self.field = Some(goal_distances(self.n, self.goals, meter)?);
        }
        let distance = if let Some(index) = &self.sparse {
            index.nearest(cell, &mut self.comparisons, meter)?
        } else if let Some(field) = &self.field {
            meter.spend(1)?;
            usize::from(field[cell[1] * self.n + cell[0]])
        } else {
            meter.spend(self.goals.len())?;
            self.comparisons = self.comparisons.saturating_add(self.goals.len());
            self.goals.iter().map(|&g| distance(cell, g)).min().unwrap()
        };
        Ok(distance as u64 * u64::from(self.minimum))
    }
}
/// Independent soft guidance for blocked entrance chains. This never changes
/// source walkability or certifies earthwork, smoothed geometry or a mesh.
#[derive(Clone, Copy)]
pub struct TerrainRecovery<'a> {
    lattice: crate::road_costmap::CellLattice,
    heights: &'a [f32],
    maximum_grade: f32,
    slope_penalty: u32,
    extra_radius: usize,
    entrance_exclusions: Option<&'a [u8]>,
    entrance_penalties: Option<&'a [u8]>,
    foreign_footprints: Option<(&'a [EntranceFootprint], &'a [u64])>,
    entrance_owner: Option<u64>,
}
impl<'a> TerrainRecovery<'a> {
    /// Independent A* proposals for connecting recovered entrances. Small
    /// lattices retain heading states. Large lattices retain one best label
    /// per cell, pricing turns from its parent without promising cost optimality.
    /// Source prices/zeros and push rules remain inputs, never rewritten.
    #[allow(clippy::too_many_arguments)]
    pub fn find_undirected(
        self,
        grid: &CellGrid,
        starts: &[[usize; 2]],
        ends: &[[usize; 2]],
        settings: Settings,
        push: Option<Push>,
        maximum_nodes: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Outcome, String> {
        self.find_undirected_diagnosed(grid, starts, ends, settings, push, maximum_nodes, cancelled)
            .0
    }
    #[allow(clippy::too_many_arguments)]
    pub fn find_undirected_diagnosed(
        self,
        grid: &CellGrid,
        starts: &[[usize; 2]],
        ends: &[[usize; 2]],
        settings: Settings,
        push: Option<Push>,
        maximum_nodes: usize,
        cancelled: impl Fn() -> bool,
    ) -> (AccessResult, AccessDiagnostics) {
        let mut trace = AccessDiagnostics {
            stop: "invalid".into(),
            work_limit: settings.maximum_work,
            state_limit: maximum_nodes,
            ..Default::default()
        };
        let result = self.find_undirected_inner(
            grid,
            starts,
            ends,
            settings,
            push,
            maximum_nodes,
            cancelled,
            &mut trace,
        );
        trace.work = match &result {
            Ok(Outcome::Found(r)) => r.work,
            Ok(
                Outcome::Budget { work }
                | Outcome::DepthLimit { work }
                | Outcome::Cancelled { work }
                | Outcome::Unreachable { work },
            ) => *work,
            Err(_) => 0,
        };
        #[cfg(feature = "road-diagnostics")]
        if let Err(error) = access_capture::capture(access_capture::Input {
            terrain: self,
            grid,
            starts,
            ends,
            settings,
            push,
            maximum_nodes,
            trace: &trace,
        }) {
            return (Err(error), trace);
        }
        (result, trace)
    }
    #[allow(clippy::too_many_arguments)]
    fn find_undirected_inner(
        self,
        grid: &CellGrid,
        starts: &[[usize; 2]],
        ends: &[[usize; 2]],
        settings: Settings,
        push: Option<Push>,
        maximum_nodes: usize,
        cancelled: impl Fn() -> bool,
        trace: &mut AccessDiagnostics,
    ) -> AccessResult {
        let n = self.lattice.resolution();
        let heading_states = n * n * 9 <= maximum_nodes;
        trace.heading_states = heading_states;
        if grid.lattice != self.lattice
            || grid.grid.engine != CELL_VERSION
            || grid.grid.resolution != n
            || grid.grid.costs.len() != n * n
            || starts.len() > 4096
            || ends.len() > 4096
            || starts.iter().chain(ends).flatten().any(|&v| v >= n)
            || !(1..=1_000_000).contains(&settings.maximum_expansions)
            || !(1..=128_000_000).contains(&settings.maximum_work)
            || settings.directional_penalty > 1_000_000
            || !(1..=4).contains(&settings.directional_radius)
            || push.is_some_and(|p| !p.valid(n))
            || !(1..=262144).contains(&maximum_nodes)
        {
            return Err("Invalid terrain access search".into());
        }
        let mut meter = Meter {
            work: 0,
            limit: settings.maximum_work,
            cancelled: &cancelled,
        };
        macro_rules! spend {
            ($amount:expr) => {
                if let Err(o) = meter.spend($amount) {
                    trace.stop = if matches!(&o, Outcome::Cancelled { .. }) {
                        "cancelled"
                    } else {
                        "work"
                    }
                    .into();
                    return Ok(o);
                }
            };
        }
        let mut minimum = u32::MAX;
        for chunk in grid.grid.costs.chunks(1024) {
            spend!(chunk.len());
            for &price in chunk {
                if price > 1_000_000 {
                    return Err("Invalid source road price".into());
                }
                if price > 0 {
                    minimum = minimum.min(price);
                }
            }
        }
        if starts.is_empty() || ends.is_empty() || minimum == u32::MAX {
            trace.stop = "unreachable".into();
            return Ok(Outcome::Unreachable { work: meter.work });
        }
        let (seeds, goals) = if starts.len() > ends.len() {
            (ends, starts)
        } else {
            (starts, ends)
        };
        trace.goal_samples = goals.len();
        spend!(goals.len());
        let mut eligible = goals
            .iter()
            .copied()
            .filter(|p| grid.grid.costs[p[1] * n + p[0]] > 0)
            .collect::<Vec<_>>();
        spend!(eligible.len() * (eligible.len().max(1).ilog2() as usize + 1));
        eligible.sort_unstable();
        eligible.dedup();
        trace.walkable_goals = eligible.len();
        if eligible.is_empty() {
            trace.stop = "unreachable".into();
            return Ok(Outcome::Unreachable { work: meter.work });
        }
        let goals = eligible.as_slice();
        let mut heuristic = GoalHeuristic {
            n,
            goals,
            minimum,
            field: None,
            sparse: None,
            comparisons: 0,
        };
        macro_rules! estimate {
            ($cell:expr) => {
                match heuristic.estimate($cell, &mut meter) {
                    Ok(h) => {
                        trace.direct_comparisons = heuristic.comparisons;
                        trace.indexed = heuristic.field.is_some() || heuristic.sparse.is_some();
                        trace.sparse_index = heuristic.sparse.is_some();
                        h
                    }
                    Err(o) => {
                        trace.stop = if matches!(&o, Outcome::Cancelled { .. }) {
                            "cancelled"
                        } else {
                            "work"
                        }
                        .into();
                        return Ok(o);
                    }
                }
            };
        }
        let price = |cell: [usize; 2]| {
            let base = grid.grid.costs[cell[1] * n + cell[0]];
            if base == 0 {
                0
            } else {
                base + push.map_or(0, |p| p.price(cell))
            }
        };
        #[derive(Clone, Copy)]
        struct Label {
            cell: [usize; 2],
            heading: usize,
            cost: u64,
            parent: Option<usize>,
            closed: bool,
        }
        let mut labels: Vec<Label> = Vec::new();
        // Lattices have at most 1024² cells. Heading slots are only enabled
        // when they fit the label allowance, so this table uses at most 4 MiB.
        // Label insertion order and heap ties remain independent of storage.
        let states = n * n * if heading_states { 9 } else { 1 };
        let mut indices = vec![u32::MAX; states];
        let mut open = BinaryHeap::new();
        for &cell in seeds {
            spend!(1);
            let base = price(cell);
            if base == 0 {
                continue;
            }
            let key = if heading_states {
                (cell[1] * n + cell[0]) * 9 + 8
            } else {
                cell[1] * n + cell[0]
            };
            if indices[key] != u32::MAX {
                continue;
            }
            if labels.len() == maximum_nodes || open.len() == maximum_nodes {
                trace.stop = "labels".into();
                return Ok(Outcome::Budget { work: meter.work });
            }
            let id = labels.len();
            let cost = u64::from(base);
            let h = estimate!(cell);
            labels.push(Label {
                cell,
                heading: 8,
                cost,
                parent: None,
                closed: false,
            });
            indices[key] = id as u32;
            open.push(Reverse((cost + h, h, id, cost)));
            trace.labels = labels.len();
            trace.maximum_queue = trace.maximum_queue.max(open.len());
        }
        let mut expansions = 0;
        while let Some(Reverse((_, h, id, queued))) = open.pop() {
            spend!(1);
            let node = labels[id];
            if node.closed || node.cost != queued {
                continue;
            }
            if expansions == settings.maximum_expansions {
                trace.stop = "expansions".into();
                return Ok(Outcome::DepthLimit { work: meter.work });
            }
            expansions += 1;
            trace.expansions = expansions;
            labels[id].closed = true;
            if h == 0 {
                let mut cells = Vec::new();
                let mut cumulative_costs = Vec::new();
                let mut next = Some(id);
                while let Some(at) = next {
                    spend!(1);
                    cells.push(labels[at].cell);
                    cumulative_costs.push(labels[at].cost);
                    if cells.len() > maximum_nodes {
                        return Err("Invalid terrain access chain".into());
                    }
                    next = labels[at].parent;
                }
                trace.stop = "found".into();
                return Ok(Outcome::Found(Route {
                    engine: TERRAIN_ACCESS_VERSION,
                    cells,
                    cumulative_costs,
                    expansions,
                    work: meter.work,
                }));
            }
            for (heading, (dx, dz)) in NEIGHBORS.into_iter().enumerate() {
                spend!(1);
                let x = node.cell[0].checked_add_signed(dx).filter(|&v| v < n);
                let z = node.cell[1].checked_add_signed(dz).filter(|&v| v < n);
                let (Some(x), Some(z)) = (x, z) else {
                    continue;
                };
                let cell = [x, z];
                let base = price(cell);
                if base == 0 {
                    continue;
                }
                let turn = if node.heading < 8 {
                    let (px, pz) = NEIGHBORS[node.heading];
                    if (px + dx).unsigned_abs().max((pz + dz).unsigned_abs())
                        <= settings.directional_radius
                    {
                        settings.directional_penalty
                    } else {
                        0
                    }
                } else {
                    0
                };
                let terrain =
                    (self.excess(node.cell, cell) * f64::from(self.slope_penalty)).ceil() as u64;
                let cost = node
                    .cost
                    .checked_add(u64::from(base) + u64::from(turn))
                    .and_then(|v| v.checked_add(terrain))
                    .ok_or("Terrain access cost overflow")?;
                let key = if heading_states {
                    (z * n + x) * 9 + heading
                } else {
                    z * n + x
                };
                let existing = (indices[key] != u32::MAX).then_some(indices[key] as usize);
                if let Some(at) = existing {
                    if labels[at].closed || cost >= labels[at].cost {
                        continue;
                    }
                } else if labels.len() == maximum_nodes {
                    trace.stop = "labels".into();
                    return Ok(Outcome::Budget { work: meter.work });
                }
                if open.len() == maximum_nodes {
                    trace.stop = "queue".into();
                    return Ok(Outcome::Budget { work: meter.work });
                }
                spend!(1);
                let next = existing.unwrap_or(labels.len());
                let label = Label {
                    cell,
                    heading,
                    cost,
                    parent: Some(id),
                    closed: false,
                };
                if existing.is_some() {
                    labels[next] = label;
                } else {
                    labels.push(label);
                    indices[key] = next as u32;
                }
                trace.labels = labels.len();
                let h = estimate!(cell);
                open.push(Reverse((cost + h, h, next, cost)));
                trace.labels = labels.len();
                trace.maximum_queue = trace.maximum_queue.max(open.len());
            }
        }
        trace.stop = "unreachable".into();
        Ok(Outcome::Unreachable { work: meter.work })
    }
    pub fn new(
        lattice: crate::road_costmap::CellLattice,
        heights: &'a [f32],
        maximum_grade: f32,
        slope_penalty: u32,
        extra_radius: usize,
    ) -> Result<Self, String> {
        if heights.len() != lattice.resolution().pow(2)
            || heights.iter().any(|v| !v.is_finite() || v.abs() > 10000.)
            || !maximum_grade.is_finite()
            || !(0.001..=1.).contains(&maximum_grade)
            || !(1..=1_000_000).contains(&slope_penalty)
            || !(1..=1024).contains(&extra_radius)
        {
            return Err("Invalid terrain entrance guidance".into());
        }
        Ok(Self {
            lattice,
            heights,
            maximum_grade,
            slope_penalty,
            extra_radius,
            entrance_exclusions: None,
            entrance_penalties: None,
            foreign_footprints: None,
            entrance_owner: None,
        })
    }
    /// Hard clearance for entrance chains, separate from source cost zeros
    /// that an authored exit may legitimately traverse.
    pub fn with_entrance_exclusions(mut self, excluded: &'a [u8]) -> Result<Self, String> {
        if excluded.len() != self.heights.len() || excluded.iter().any(|&v| v > 1) {
            return Err("Invalid terrain entrance exclusions".into());
        }
        self.entrance_exclusions = Some(excluded);
        Ok(self)
    }
    /// Prefer dry approach corridors while retaining source recovery through
    /// blocked cells. Final constructed-ground checks still decide acceptance.
    pub fn with_entrance_penalties(mut self, penalties: &'a [u8]) -> Result<Self, String> {
        if penalties.len() != self.heights.len() || penalties.iter().any(|&v| v > 1) {
            return Err("Invalid terrain entrance penalties".into());
        }
        self.entrance_penalties = Some(penalties);
        Ok(self)
    }
    pub fn with_foreign_footprints(
        mut self,
        footprints: &'a [EntranceFootprint],
        connector_owners: &'a [u64],
    ) -> Result<Self, String> {
        if footprints.len() != self.heights.len() || connector_owners.len() > 1024 {
            return Err("Invalid terrain entrance footprints".into());
        }
        self.foreign_footprints = Some((footprints, connector_owners));
        Ok(self)
    }

    pub(crate) fn matches_connectors(self, count: usize) -> bool {
        self.foreign_footprints
            .is_none_or(|(_, owners)| owners.len() == count)
    }

    pub(crate) fn for_connector(mut self, connector: usize) -> Self {
        self.entrance_owner = self
            .foreign_footprints
            .and_then(|(_, owners)| owners.get(connector))
            .copied();
        self
    }
    pub(crate) fn entrance_penalized(self, cell: [usize; 2]) -> bool {
        self.entrance_penalties
            .is_some_and(|mask| mask[cell[1] * self.lattice.resolution() + cell[0]] != 0)
    }
    pub(crate) fn entrance_clear(self, a: [usize; 2], b: [usize; 2]) -> Result<bool, String> {
        let n = self.lattice.resolution();
        if a.iter().chain(&b).any(|&i| i >= n) {
            return Err("Invalid terrain entrance exclusion sample".into());
        }
        let cells = [a, b, [a[0], b[1]], [b[0], a[1]]];
        let holes_clear = self.entrance_exclusions.is_none_or(|mask| {
            cells.iter().all(|p| mask[p[1] * n + p[0]] == 0)
        });
        Ok(holes_clear
            && self.foreign_footprints.is_none_or(|(mask, _)| {
                cells.iter().all(|p| match mask[p[1] * n + p[0]] {
                    EntranceFootprint::Clear => true,
                    EntranceFootprint::Monument(owner) => self.entrance_owner == Some(owner),
                    EntranceFootprint::Overlap => false,
                })
            }))
    }
    pub(crate) fn has_entrance_guidance(self) -> bool {
        self.entrance_exclusions.is_some()
            || self.entrance_penalties.is_some()
            || self.foreign_footprints.is_some()
    }
    pub fn preparation_work(self) -> usize {
        self.heights.len()
            + self.entrance_exclusions.map_or(0, <[u8]>::len)
            + self.entrance_penalties.map_or(0, <[u8]>::len)
            + self.foreign_footprints.map_or(0, |(_, owners)| owners.len())
    }
    pub fn lattice(self) -> crate::road_costmap::CellLattice {
        self.lattice
    }
    fn excess(self, a: [usize; 2], b: [usize; 2]) -> f64 {
        let n = self.lattice.resolution();
        let rise = (f64::from(self.heights[a[1] * n + a[0]])
            - f64::from(self.heights[b[1] * n + b[0]]))
        .abs();
        let run = (a[0].abs_diff(b[0]) as f64).hypot(a[1].abs_diff(b[1]) as f64)
            * f64::from(self.lattice.spacing());
        (rise - run * f64::from(self.maximum_grade)).max(0.)
    }
    pub fn is_steep(self, a: [usize; 2], b: [usize; 2]) -> Result<bool, String> {
        if a.iter().chain(&b).any(|&i| i >= self.lattice.resolution()) {
            return Err("Invalid terrain entrance sample".into());
        }
        Ok(self.excess(a, b) > 0.)
    }
    /// Dijkstra ranking uses local grade excess as a soft cost. Every original
    /// blocked cell remains blocked for ordinary network routing. The returned
    /// chain, like source recovery, may traverse them to an eligible goal.
    #[allow(clippy::too_many_arguments)]
    pub fn find_closest(
        self,
        grid: &CellGrid,
        start: [usize; 2],
        goals: Option<&ConnectedGoals<'_>>,
        baseline_radius: usize,
        settings: Settings,
        cancelled: impl Fn() -> bool,
    ) -> Result<Outcome, String> {
        let n = self.lattice.resolution();
        if grid.lattice != self.lattice
            || grid.grid.engine != CELL_VERSION
            || grid.grid.resolution != n
            || grid.grid.costs.len() != n * n
            || start.iter().any(|&i| i >= n)
            || goals.is_some_and(|g| !std::ptr::eq(g.grid, grid))
            || baseline_radius > 1024
            || !(1..=1_000_000).contains(&settings.maximum_expansions)
            || !(1..=128_000_000).contains(&settings.maximum_work)
        {
            return Err("Invalid terrain entrance request".into());
        }
        let mut meter = Meter {
            work: 0,
            limit: settings.maximum_work,
            cancelled: &cancelled,
        };
        macro_rules! spend {
            ($n:expr) => {
                if let Err(outcome) = meter.spend($n) {
                    return Ok(outcome);
                }
            };
        }
        spend!(0);
        if !self.entrance_clear(start, start)? {
            return Ok(Outcome::Unreachable { work: meter.work });
        }
        if goals.is_some_and(|g| !g.has_goals) {
            return Ok(Outcome::Unreachable { work: meter.work });
        }
        if goals.is_none() {
            for chunk in grid.grid.costs.chunks(1024) {
                spend!(chunk.len());
                if chunk.iter().any(|&c| c > 1_000_000) {
                    return Err("Invalid source road price".into());
                }
            }
        }
        let radius = baseline_radius.saturating_add(self.extra_radius).min(n - 1);
        spend!(n * n * 2);
        let mut costs = vec![u64::MAX; n * n];
        let mut parents = vec![usize::MAX; n * n];
        let first = start[1] * n + start[0];
        costs[first] = 1;
        let mut open = BinaryHeap::new();
        open.push(Reverse((1_u64, first)));
        let mut expansions = 0;
        while let Some(Reverse((cost, at))) = open.pop() {
            spend!(1);
            if cost != costs[at] {
                continue;
            }
            if expansions == settings.maximum_expansions {
                return Ok(Outcome::DepthLimit { work: meter.work });
            }
            expansions += 1;
            let cell = [at % n, at / n];
            if goals.map_or(grid.grid.costs[at] != 0, |g| g.mask[at]) {
                let mut cells = Vec::new();
                let mut cumulative_costs = Vec::new();
                let mut next = at;
                loop {
                    spend!(1);
                    cells.push([next % n, next / n]);
                    cumulative_costs.push(costs[next]);
                    if next == first {
                        break;
                    }
                    next = parents[next];
                    if next == usize::MAX || cells.len() > n * n {
                        return Err("Invalid terrain entrance chain".into());
                    }
                }
                return Ok(Outcome::Found(Route {
                    engine: TERRAIN_ENTRANCE_VERSION,
                    cells,
                    cumulative_costs,
                    expansions,
                    work: meter.work,
                }));
            }
            for (dx, dz) in NEIGHBORS {
                spend!(1);
                let x = cell[0].checked_add_signed(dx).filter(|&v| v < n);
                let z = cell[1].checked_add_signed(dz).filter(|&v| v < n);
                let (Some(x), Some(z)) = (x, z) else {
                    continue;
                };
                let other = [x, z];
                if distance(start, other) > radius || !self.entrance_clear(cell, other)? {
                    continue;
                }
                let id = z * n + x;
                let penalty =
                    (self.excess(cell, other) * f64::from(self.slope_penalty)).ceil() as u64;
                let candidate = cost
                    .checked_add(1)
                    .and_then(|v| v.checked_add(penalty))
                    .and_then(|v| {
                        v.checked_add(if self.entrance_penalized(other) {
                            u64::from(self.slope_penalty)
                        } else {
                            0
                        })
                    })
                    .ok_or("Terrain entrance cost overflow")?;
                if candidate < costs[id] {
                    spend!(1);
                    costs[id] = candidate;
                    parents[id] = at;
                    open.push(Reverse((candidate, id)));
                }
            }
        }
        Ok(Outcome::Unreachable { work: meter.work })
    }
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Settings {
    pub maximum_expansions: usize,
    pub maximum_work: usize,
    pub directional_penalty: u32,
    pub directional_radius: usize,
}
/// Temporary monument repulsion used when the source layout promotes a
/// deferred connector group. Radius/distance use Chebyshev grid cells.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Push {
    pub point: [usize; 2],
    pub radius: usize,
    pub distance: usize,
    pub multiplier: u32,
}
impl Push {
    fn valid(self, n: usize) -> bool {
        self.point.iter().all(|&v| v < n)
            && self.radius <= 1024
            && (1..=1024).contains(&self.distance)
            && self.multiplier <= 1_000_000
    }
    fn price(self, cell: [usize; 2]) -> u32 {
        let separation = if self.radius > 0 {
            distance(cell, self.point).saturating_sub(self.radius)
        } else {
            self.distance * 2
        };
        let weight = self.distance.saturating_sub(separation) as f32 / self.distance as f32;
        (self.multiplier as f32 * weight).ceil() as u32
    }
}
#[derive(Debug, Serialize)]
pub struct Route {
    pub engine: &'static str,
    pub cells: Vec<[usize; 2]>,
    pub cumulative_costs: Vec<u64>,
    pub expansions: usize,
    pub work: usize,
}
#[derive(Debug)]
pub enum Outcome {
    Found(Route),
    Unreachable { work: usize },
    Budget { work: usize },
    DepthLimit { work: usize },
    Cancelled { work: usize },
}
/// Borrowed prices for infrastructure families using the same PathFinder rules.
/// Public family adapters retain responsibility for their source-version gate.
#[derive(Clone, Copy)]
pub(crate) struct CostGrid<'a> {
    pub resolution: usize,
    pub costs: &'a [u32],
}
#[derive(Clone, Copy)]
struct Node {
    cell: [usize; 2],
    cost: u64,
    parent: Option<usize>,
}
/// The inspected IntrusiveMinHeap retains its head while inserting into a sorted
/// child chain, except for the singleton <= case. Its tail uses newest-first
/// equal priorities. A head plus binary tail reproduces that observable order
/// without the source's linear linked-list insertion cost.
#[derive(Default)]
struct SourceQueue {
    head: Option<(u64, usize)>,
    tail: BinaryHeap<Reverse<(u64, Reverse<usize>)>>,
}
impl SourceQueue {
    fn push(&mut self, priority: u64, id: usize) {
        match self.head {
            None => self.head = Some((priority, id)),
            Some(old) if self.tail.is_empty() && priority <= old.0 => {
                self.head = Some((priority, id));
                self.tail.push(Reverse((old.0, Reverse(old.1))));
            }
            Some(_) => self.tail.push(Reverse((priority, Reverse(id)))),
        }
    }
    fn pop(&mut self) -> Option<usize> {
        let (_, id) = self.head.take()?;
        self.head = self
            .tail
            .pop()
            .map(|Reverse((priority, Reverse(id)))| (priority, id));
        Some(id)
    }
}
fn distance(a: [usize; 2], b: [usize; 2]) -> usize {
    a[0].abs_diff(b[0]).max(a[1].abs_diff(b[1]))
}
struct Meter<'a, F> {
    work: usize,
    limit: usize,
    cancelled: &'a F,
}
impl<F: Fn() -> bool> Meter<'_, F> {
    fn spend(&mut self, amount: usize) -> Result<(), Outcome> {
        if (self.cancelled)() {
            return Err(Outcome::Cancelled { work: self.work });
        }
        if amount > self.limit.saturating_sub(self.work) {
            return Err(Outcome::Budget { work: self.work });
        }
        self.work += amount;
        Ok(())
    }
}
pub fn find_path(
    grid: &CellGrid,
    start: [usize; 2],
    end: [usize; 2],
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Outcome, String> {
    // Source FindPath searches backwards, then follows parents start -> end.
    search(grid, &[end], &[start], settings, None, cancelled)
}
pub fn find_undirected(
    grid: &CellGrid,
    starts: &[[usize; 2]],
    ends: &[[usize; 2]],
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Outcome, String> {
    find_undirected_with_push(grid, starts, ends, settings, None, cancelled)
}
pub fn find_undirected_with_push(
    grid: &CellGrid,
    starts: &[[usize; 2]],
    ends: &[[usize; 2]],
    settings: Settings,
    push: Option<Push>,
    cancelled: impl Fn() -> bool,
) -> Result<Outcome, String> {
    if starts.len() > ends.len() {
        search(grid, ends, starts, settings, push, cancelled)
    } else {
        search(grid, starts, ends, settings, push, cancelled)
    }
}
/// Source entrance recovery returns a chain from the nearest walkable cell
/// back to the authored entrance. Blocked cells are traversed at unit cost;
/// ordinary cost-map pathfinding cannot substitute for this search.
pub fn find_closest_walkable(
    grid: &CellGrid,
    start: [usize; 2],
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Outcome, String> {
    recover_entrance(grid, start, None, None, settings, cancelled)
}
/// Independent entrance recovery: only positive-cost cells connected to the
/// supplied network are eligible. The recovery chain may cross blocked cells,
/// just as the source entrance search does; the routing grid is never changed.
/// An empty or entirely blocked network has no eligible cells.
pub fn find_closest_connected(
    grid: &CellGrid,
    start: [usize; 2],
    network: &[[usize; 2]],
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Outcome, String> {
    recover_entrance(grid, start, Some(network), None, settings, cancelled)
}
/// Eligibility is scoped to an immutable borrowed cost grid. It cannot be
/// reused after the caller changes prices, reservations or the grid lattice.
pub struct ConnectedGoals<'a> {
    grid: &'a CellGrid,
    mask: Vec<bool>,
    has_goals: bool,
    work: usize,
}
pub enum ConnectedPreparation<'a> {
    Ready(ConnectedGoals<'a>),
    Budget { work: usize },
    Cancelled { work: usize },
}
impl ConnectedGoals<'_> {
    pub fn preparation_work(&self) -> usize {
        self.work
    }
    /// Only query work is returned here; charge preparation_work once to the
    /// enclosing transaction before making any entrance queries.
    pub fn find_closest(
        &self,
        start: [usize; 2],
        settings: Settings,
        cancelled: impl Fn() -> bool,
    ) -> Result<Outcome, String> {
        recover_entrance(
            self.grid,
            start,
            None,
            Some((&self.mask, self.has_goals)),
            settings,
            cancelled,
        )
    }
}
fn connected_mask<F: Fn() -> bool>(
    grid: CostGrid<'_>,
    network: &[[usize; 2]],
    meter: &mut Meter<'_, F>,
) -> Result<(Vec<bool>, bool), Outcome> {
    let n = grid.resolution;
    meter.spend(n * n)?;
    let mut mask = vec![false; n * n];
    let mut frontier = Vec::new();
    for &cell in network {
        meter.spend(1)?;
        let i = cell[1] * n + cell[0];
        if grid.costs[i] != 0 && !mask[i] {
            mask[i] = true;
            frontier.push(cell);
        }
    }
    let has_goals = !frontier.is_empty();
    while let Some(cell) = frontier.pop() {
        meter.spend(1)?;
        for (dx, dz) in NEIGHBORS {
            meter.spend(1)?;
            let x = cell[0].checked_add_signed(dx).filter(|&v| v < n);
            let z = cell[1].checked_add_signed(dz).filter(|&v| v < n);
            let (Some(x), Some(z)) = (x, z) else {
                continue;
            };
            let i = z * n + x;
            if grid.costs[i] != 0 && !mask[i] {
                mask[i] = true;
                frontier.push([x, z]);
            }
        }
    }
    Ok((mask, has_goals))
}
pub fn prepare_connected<'a>(
    grid: &'a CellGrid,
    network: &[[usize; 2]],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<ConnectedPreparation<'a>, String> {
    let n = grid.lattice.resolution();
    if grid.grid.engine != CELL_VERSION
        || grid.grid.resolution != n
        || grid.grid.costs.len() != n * n
        || network.len() > 4096
        || network.iter().flatten().any(|&v| v >= n)
        || !(1..=128_000_000).contains(&maximum_work)
    {
        return Err("Invalid connected-road preparation request".into());
    }
    let mut meter = Meter {
        work: 0,
        limit: maximum_work,
        cancelled: &cancelled,
    };
    let halt = |outcome| match outcome {
        Outcome::Budget { work } => ConnectedPreparation::Budget { work },
        Outcome::Cancelled { work } => ConnectedPreparation::Cancelled { work },
        _ => unreachable!("Mask preparation returns only budget or cancellation"),
    };
    for chunk in grid.grid.costs.chunks(1024) {
        if let Err(outcome) = meter.spend(chunk.len()) {
            return Ok(halt(outcome));
        }
        if chunk.iter().any(|&v| v > 1_000_000) {
            return Err("Invalid source road price".into());
        }
    }
    let prices = CostGrid {
        resolution: n,
        costs: &grid.grid.costs,
    };
    let (mask, has_goals) = match connected_mask(prices, network, &mut meter) {
        Ok(value) => value,
        Err(outcome) => return Ok(halt(outcome)),
    };
    Ok(ConnectedPreparation::Ready(ConnectedGoals {
        grid,
        mask,
        has_goals,
        work: meter.work,
    }))
}
fn recover_entrance(
    grid: &CellGrid,
    start: [usize; 2],
    network: Option<&[[usize; 2]]>,
    prepared: Option<(&[bool], bool)>,
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Outcome, String> {
    let n = grid.lattice.resolution();
    if grid.grid.engine != CELL_VERSION || grid.grid.resolution != n {
        return Err("Invalid closest-walkable entrance request".into());
    }
    recover_costs(
        CostGrid { resolution: n, costs: &grid.grid.costs },
        start,
        network,
        prepared,
        settings,
        cancelled,
    )
}
pub(crate) fn recover_costs(
    grid: CostGrid<'_>,
    start: [usize; 2],
    network: Option<&[[usize; 2]]>,
    prepared: Option<(&[bool], bool)>,
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Outcome, String> {
    let n = grid.resolution;
    if !(2..=1024).contains(&n)
        || grid.costs.len() != n * n
        || start.iter().any(|&v| v >= n)
        || network
            .is_some_and(|cells| cells.len() > 4096 || cells.iter().flatten().any(|&v| v >= n))
        || !(1..=1_000_000).contains(&settings.maximum_expansions)
        || !(1..=128_000_000).contains(&settings.maximum_work)
    {
        return Err("Invalid closest-walkable entrance request".into());
    }
    let mut meter = Meter {
        work: 0,
        limit: settings.maximum_work,
        cancelled: &cancelled,
    };
    macro_rules! spend {
        ($n:expr) => {
            if let Err(outcome) = meter.spend($n) {
                return Ok(outcome);
            }
        };
    }
    if prepared.is_none() {
        for chunk in grid.costs.chunks(1024) {
            spend!(chunk.len());
            if chunk.iter().any(|&v| v > 1_000_000) {
                return Err("Invalid source road price".into());
            }
        }
    }
    let eligible = if let Some(network) = network {
        let (mask, has_goals) = match connected_mask(grid, network, &mut meter) {
            Ok(value) => value,
            Err(outcome) => return Ok(outcome),
        };
        if !has_goals {
            return Ok(Outcome::Unreachable { work: meter.work });
        }
        Some(mask)
    } else {
        None
    };
    if prepared.is_some_and(|(_, has_goals)| !has_goals) {
        spend!(0);
        return Ok(Outcome::Unreachable { work: meter.work });
    }
    let mask = prepared.map(|(mask, _)| mask).or(eligible.as_deref());
    let heuristic = |p: [usize; 2]| {
        let i = p[1] * n + p[0];
        u64::from(mask.map_or(grid.costs[i] == 0, |mask| !mask[i]))
    };
    if prepared.is_some() {
        spend!(n * n);
    } // Preparing a fresh visited raster is query work.
    let mut visited = vec![false; n * n];
    let mut nodes = vec![Node {
        cell: start,
        cost: 1,
        parent: None,
    }];
    visited[start[1] * n + start[0]] = true;
    let mut open = SourceQueue::default();
    open.push(1 + heuristic(start), 0);
    let mut expansions = 0;
    while let Some(id) = open.pop() {
        spend!(1);
        if expansions == settings.maximum_expansions {
            return Ok(Outcome::DepthLimit { work: meter.work });
        }
        expansions += 1;
        let node = nodes[id];
        if heuristic(node.cell) == 0 {
            let mut cells = Vec::new();
            let mut cumulative_costs = Vec::new();
            let mut next = Some(id);
            while let Some(i) = next {
                spend!(1);
                cells.push(nodes[i].cell);
                cumulative_costs.push(nodes[i].cost);
                next = nodes[i].parent;
            }
            return Ok(Outcome::Found(Route {
                engine: if network.is_some() || prepared.is_some() {
                    CONNECTED_ENTRANCE_VERSION
                } else {
                    VERSION
                },
                cells,
                cumulative_costs,
                expansions,
                work: meter.work,
            }));
        }
        for (dx, dz) in NEIGHBORS {
            spend!(1);
            let x = node.cell[0].checked_add_signed(dx).filter(|&v| v < n);
            let z = node.cell[1].checked_add_signed(dz).filter(|&v| v < n);
            let (Some(x), Some(z)) = (x, z) else {
                continue;
            };
            let cell = [x, z];
            let i = z * n + x;
            if !visited[i] {
                spend!(1);
                let next = nodes.len();
                let cost = node.cost + 1;
                nodes.push(Node {
                    cell,
                    cost,
                    parent: Some(id),
                });
                open.push(cost + heuristic(cell), next);
                visited[i] = true;
            }
        }
    }
    Ok(Outcome::Unreachable { work: meter.work })
}
const NEIGHBORS: [(isize, isize); 8] = [
    (0, 1),
    (-1, 0),
    (1, 0),
    (0, -1),
    (-1, 1),
    (1, 1),
    (-1, -1),
    (1, -1),
];
fn search(
    grid: &CellGrid,
    seeds: &[[usize; 2]],
    goals: &[[usize; 2]],
    settings: Settings,
    push: Option<Push>,
    cancelled: impl Fn() -> bool,
) -> Result<Outcome, String> {
    let n = grid.lattice.resolution();
    if grid.grid.engine != CELL_VERSION || grid.grid.resolution != n {
        return Err("Invalid source road search request".into());
    }
    search_costs(
        CostGrid { resolution: n, costs: &grid.grid.costs },
        seeds,
        goals,
        settings,
        push,
        cancelled,
    )
}
pub(crate) fn search_costs(
    grid: CostGrid<'_>,
    seeds: &[[usize; 2]],
    goals: &[[usize; 2]],
    settings: Settings,
    push: Option<Push>,
    cancelled: impl Fn() -> bool,
) -> Result<Outcome, String> {
    let mut heuristic = GoalHeuristic {
        n: grid.resolution,
        goals,
        minimum: 1,
        field: None,
        sparse: None,
        comparisons: 0,
    };
    search_costs_with(
        grid,
        seeds,
        goals,
        settings,
        push,
        cancelled,
        |cell, meter| heuristic.estimate(cell, meter),
    )
}

fn search_costs_with<F: Fn() -> bool>(
    grid: CostGrid<'_>,
    seeds: &[[usize; 2]],
    goals: &[[usize; 2]],
    settings: Settings,
    push: Option<Push>,
    cancelled: F,
    mut estimate: impl FnMut([usize; 2], &mut Meter<'_, F>) -> Result<u64, Outcome>,
) -> Result<Outcome, String> {
    let n = grid.resolution;
    if !(2..=1024).contains(&n)
        || grid.costs.len() != n * n
        || seeds.len() > 4096
        || goals.len() > 4096
        || seeds.iter().chain(goals).flatten().any(|&v| v >= n)
        || !(1..=1_000_000).contains(&settings.maximum_expansions)
        || !(1..=128_000_000).contains(&settings.maximum_work)
        || settings.directional_penalty > 1_000_000
        || !(1..=4).contains(&settings.directional_radius)
        || push.is_some_and(|p| !p.valid(n))
    {
        return Err("Invalid source road search request".into());
    }
    let mut meter = Meter {
        work: 0,
        limit: settings.maximum_work,
        cancelled: &cancelled,
    };
    macro_rules! spend {
        ($n:expr) => {
            if let Err(outcome) = meter.spend($n) {
                return Ok(outcome);
            }
        };
    }
    for chunk in grid.costs.chunks(1024) {
        spend!(chunk.len());
        if chunk.iter().any(|&v| v > 1_000_000) {
            return Err("Invalid source road price".into());
        }
    }
    if seeds.is_empty() || goals.is_empty() {
        return Ok(Outcome::Unreachable { work: meter.work });
    }
    macro_rules! heuristic {
        ($cell:expr) => {
            match estimate($cell, &mut meter) {
                Ok(value) => value,
                Err(outcome) => return Ok(outcome),
            }
        };
    }
    let price = |cell: [usize; 2]| {
        let base = grid.costs[cell[1] * n + cell[0]];
        if base == 0 {
            0
        } else {
            base + push.map_or(0, |p| p.price(cell))
        }
    };
    let mut visited = vec![0_u32; n * n];
    let mut nodes: Vec<Node> = Vec::new();
    let mut open = SourceQueue::default();
    for &cell in seeds {
        spend!(1);
        let i = cell[1] * n + cell[0];
        let cost = price(cell);
        if cost == 0 {
            spend!(goals.len());
            visited[i] = u32::MAX;
            continue;
        }
        let id = nodes.len();
        nodes.push(Node {
            cell,
            cost: u64::from(cost),
            parent: None,
        });
        open.push(u64::from(cost) + heuristic!(cell), id);
        visited[i] = cost;
    }
    let mut expansions = 0;
    while let Some(id) = open.pop() {
        spend!(1);
        if expansions == settings.maximum_expansions {
            return Ok(Outcome::DepthLimit { work: meter.work });
        }
        expansions += 1;
        let node = nodes[id];
        if heuristic!(node.cell) == 0 {
            let mut cells = Vec::new();
            let mut cumulative_costs = Vec::new();
            let mut next = Some(id);
            while let Some(i) = next {
                spend!(1);
                cells.push(nodes[i].cell);
                cumulative_costs.push(nodes[i].cost);
                next = nodes[i].parent;
            }
            return Ok(Outcome::Found(Route {
                engine: VERSION,
                cells,
                cumulative_costs,
                expansions,
                work: meter.work,
            }));
        }
        // Inspected Moore-neighbor ordering. No extra geometric-distance,
        // slope, diagonal-corner or pairwise-grade policy is added here.
        for (dx, dz) in NEIGHBORS {
            spend!(1);
            let x = node.cell[0].checked_add_signed(dx).filter(|&v| v < n);
            let z = node.cell[1].checked_add_signed(dz).filter(|&v| v < n);
            let (Some(x), Some(z)) = (x, z) else {
                continue;
            };
            let cell = [x, z];
            let i = z * n + x;
            let base = price(cell);
            if base == 0 {
                visited[i] = u32::MAX;
                continue;
            }
            let penalty = if node
                .parent
                .is_some_and(|p| distance(cell, nodes[p].cell) <= settings.directional_radius)
            {
                settings.directional_penalty
            } else {
                0
            };
            let step = base + penalty;
            // The inspected visited table records the best incoming STEP cost,
            // not total path cost. It can reopen a penalized cell at base cost.
            if visited[i] == 0 || step < visited[i] {
                spend!(1);
                if nodes.len() >= 2 * n * n + seeds.len() {
                    return Ok(Outcome::Budget { work: meter.work });
                }
                let cost = node
                    .cost
                    .checked_add(u64::from(step))
                    .ok_or("Source road cost overflow")?;
                let next_id = nodes.len();
                nodes.push(Node {
                    cell,
                    cost,
                    parent: Some(id),
                });
                open.push(cost + heuristic!(cell), next_id);
                visited[i] = step;
            }
        }
    }
    Ok(Outcome::Unreachable { work: meter.work })
}

#[cfg(test)]
mod tests {
    #[test]
    fn indexed_network_goals_preserve_routes_and_finish_with_less_search_work() {
        use super::*;
        let n = 128;
        let goals = (0..64).map(|x| [x * 2, 0]).collect::<Vec<_>>();
        for blocked in [false, true] {
            let mut costs = vec![1; n * n];
            if blocked {
                for z in 0..n {
                    if z != 70 {
                        costs[z * n + 64] = 0;
                    }
                }
            }
            let grid = CostGrid {
                resolution: n,
                costs: &costs,
            };
            let limits = Settings {
                directional_penalty: 0,
                ..settings()
            };
            let direct = |limits| {
                search_costs_with(
                    grid,
                    &[[127, 127]],
                    &goals,
                    limits,
                    None,
                    || false,
                    |cell, meter| {
                        meter.spend(goals.len())?;
                        Ok(goals
                            .iter()
                            .map(|&goal| distance(cell, goal))
                            .min()
                            .unwrap() as u64)
                    },
                )
                .unwrap()
            };
            let Outcome::Found(reference) = direct(limits) else {
                panic!("Reference route missing")
            };
            let Outcome::Found(indexed) =
                search_costs(grid, &[[127, 127]], &goals, limits, None, || false).unwrap()
            else {
                panic!("Indexed route missing")
            };
            assert_eq!(indexed.cells, reference.cells);
            assert_eq!(indexed.cumulative_costs, reference.cumulative_costs);
            assert_eq!(indexed.expansions, reference.expansions);
            assert!(indexed.work < reference.work);
            let limited = Settings {
                maximum_work: reference.work - 1,
                ..limits
            };
            assert!(matches!(direct(limited), Outcome::Budget { .. }));
            assert!(matches!(
                search_costs(grid, &[[127, 127]], &goals, limited, None, || false,).unwrap(),
                Outcome::Found(_)
            ));
            assert!(matches!(
                search_costs(grid, &[[127, 127]], &goals, limits, None, || true,).unwrap(),
                Outcome::Cancelled { .. }
            ));
        }
    }
    #[test]
    fn borrowed_infrastructure_prices_preserve_existing_road_outcomes_and_work() {
        for blocked in [false, true] {
            let mut g = grid(17, vec![1; 17 * 17]);
            if blocked {
                for z in 0..17 { g.grid.costs[z * 17 + 8] = 0; }
            }
            let prices = CostGrid { resolution: 17, costs: &g.grid.costs };
            for limit in [1, 128000000] {
                let settings = Settings { maximum_work: limit, ..settings() };
                let old = find_path(&g, [0, 0], [16, 16], settings, || false).unwrap();
                let borrowed = search_costs(prices, &[[16, 16]], &[[0, 0]], settings, None, || false).unwrap();
                assert_eq!(format!("{old:?}"), format!("{borrowed:?}"));
                let old = find_closest_walkable(&g, [8, 8], settings, || false).unwrap();
                let borrowed = recover_costs(prices, [8, 8], None, None, settings, || false).unwrap();
                assert_eq!(format!("{old:?}"), format!("{borrowed:?}"));
            }
            let old = find_path(&g, [0, 0], [16, 16], settings(), || true).unwrap();
            let borrowed = search_costs(prices, &[[16, 16]], &[[0, 0]], settings(), None, || true).unwrap();
            assert_eq!(format!("{old:?}"), format!("{borrowed:?}"));
        }
        let mut stale = grid(17, vec![1; 17 * 17]);
        stale.grid.engine = "unsupported";
        assert!(find_path(&stale, [0, 0], [16, 16], settings(), || false).is_err());
        assert!(find_closest_walkable(&stale, [0, 0], settings(), || false).is_err());
    }
    #[test]
    fn sparse_goal_heuristic_keeps_exact_values_with_less_work_on_large_lattices() {
        use super::*;
        let n = 667;
        let goals = (0..365)
            .map(|i| [(i * 37) % n, (i * 53) % n])
            .collect::<Vec<_>>();
        let mut heuristic = GoalHeuristic {
            n,
            goals: &goals,
            minimum: 101,
            field: None,
            sparse: None,
            comparisons: 0,
        };
        let mut meter = Meter {
            work: 0,
            limit: 128_000_000,
            cancelled: &|| false,
        };
        for i in 0..7000 {
            let p = [(i * 97) % n, (i * 139) % n];
            let expected = goals.iter().map(|&g| distance(p, g)).min().unwrap() as u64 * 101;
            assert_eq!(heuristic.estimate(p, &mut meter).unwrap(), expected);
        }
        assert!(heuristic.sparse.is_some() && heuristic.field.is_none());
        assert!(
            meter.work < 7000 * goals.len() / 2,
            "{} work units for sparse queries",
            meter.work
        );
    }
    use super::*;
    use crate::road_costmap::{CellLattice, Counts, Grid};
    fn grid(n: usize, costs: Vec<u32>) -> CellGrid {
        CellGrid {
            lattice: CellLattice::new(n as f32, 1.).unwrap(),
            grid: Grid {
                engine: CELL_VERSION,
                resolution: n,
                seed: 0,
                trail: false,
                counts: Counts::default(),
                work: 0,
                costs,
            },
        }
    }
    fn settings() -> Settings {
        Settings {
            maximum_expansions: 250000,
            maximum_work: 128000000,
            directional_penalty: 10000,
            directional_radius: 1,
        }
    }
    #[test]
    fn terrain_access_indexes_boundary_cells_and_duplicate_seeds_in_both_state_modes() {
        for n in [5, 1024] {
            let g = grid(n, vec![100; n * n]);
            let heights = vec![0.; n * n];
            let terrain = TerrainRecovery::new(g.lattice, &heights, 0.2, 5000, 2).unwrap();
            let start = [n - 1, n - 1];
            let end = [n - 2, n - 2];
            let (result, trace) = terrain.find_undirected_diagnosed(
                &g,
                &[start, start],
                &[end, end],
                settings(),
                None,
                262144,
                || false,
            );
            let Outcome::Found(path) = result.unwrap() else {
                panic!("Boundary query rejected on {n} cells per side");
            };
            assert_eq!(trace.heading_states, n == 5);
            assert_eq!(path.cells, [end, start]);
            assert_eq!(path.cumulative_costs, [200, 100]);
            assert_eq!(trace.walkable_goals, 1);
            assert!(trace.labels <= 9);
        }
    }
    #[test]
    fn large_terrain_access_queries_keep_positional_workspaces_bounded() {
        let n = 171;
        let mut prices = vec![100; n * n];
        prices[85 * n + 85] = 0;
        let g = grid(n, prices);
        let heights = vec![0.; n * n];
        let terrain = TerrainRecovery::new(g.lattice, &heights, 0.2, 5000, 2).unwrap();
        let query = || {
            terrain.find_undirected_diagnosed(
                &g,
                &[[0, 0]],
                &[[170, 170]],
                settings(),
                None,
                262144,
                || false,
            )
        };
        let (result, trace) = query();
        let Outcome::Found(path) = result.unwrap() else {
            panic!()
        };
        assert!(!trace.heading_states);
        assert!(trace.labels <= n * n && trace.expansions <= n * n);
        assert!(path.cells.iter().all(|p| g.grid.costs[p[1] * n + p[0]] > 0));
        assert_eq!(path.cells.first(), Some(&[170, 170]));
        assert_eq!(path.cells.last(), Some(&[0, 0]));
        let (repeat, _) = query();
        let Outcome::Found(repeat) = repeat.unwrap() else {
            panic!()
        };
        assert_eq!(repeat.cells, path.cells);
        assert_eq!(repeat.cumulative_costs, path.cumulative_costs);
        assert_eq!(g.grid.costs[85 * n + 85], 0);
    }
    #[test]
    fn terrain_access_filters_unreachable_goal_samples_before_searching() {
        let n = 31;
        let mut prices = vec![100; n * n];
        let mut goals = vec![[0, 0]];
        for x in 1..n {
            prices[29 * n + x] = 0;
            goals.push([x, 29]);
        }
        let g = grid(n, prices);
        let heights = vec![0.; n * n];
        let terrain = TerrainRecovery::new(g.lattice, &heights, 0.2, 5000, 2).unwrap();
        let (result, trace) = terrain.find_undirected_diagnosed(
            &g,
            &[[15, 30]],
            &goals,
            Settings {
                maximum_expansions: 256,
                ..settings()
            },
            None,
            10000,
            || false,
        );
        let Outcome::Found(route) = result.unwrap() else {
            panic!()
        };
        assert_eq!(route.cells.first(), Some(&[0, 0]));
        assert_eq!(trace.walkable_goals, 1);
        assert_eq!(trace.goal_samples, 31);
        assert!(
            route
                .cells
                .iter()
                .all(|p| g.grid.costs[p[1] * n + p[0]] > 0)
        );
        let (result, trace) = terrain.find_undirected_diagnosed(
            &g,
            &[[15, 30]],
            &goals[1..],
            settings(),
            None,
            10000,
            || false,
        );
        assert!(matches!(result.unwrap(), Outcome::Unreachable { .. }));
        assert_eq!(trace.expansions, 0);
        assert_eq!(trace.stop, "unreachable");
        let (result, trace) = terrain.find_undirected_diagnosed(
            &g,
            &[[15, 30]],
            &[[0, 0]],
            settings(),
            None,
            1,
            || false,
        );
        assert!(matches!(result.unwrap(), Outcome::Budget { .. }));
        assert_eq!(trace.stop, "labels");
    }
    #[test]
    fn goal_distance_field_matches_brute_force_and_keeps_route_choices() {
        for n in [3, 9, 31] {
            let goals = [[0, 0], [n - 1, n / 2], [n / 2, n - 1]];
            let mut meter = Meter {
                work: 0,
                limit: 128000000,
                cancelled: &|| false,
            };
            let values = goal_distances(n, &goals, &mut meter).unwrap();
            for z in 0..n {
                for x in 0..n {
                    assert_eq!(
                        usize::from(values[z * n + x]),
                        goals.iter().map(|&g| distance([x, z], g)).min().unwrap()
                    );
                }
            }
        }
        let g = grid(9, vec![100; 81]);
        let heights = vec![0.; 81];
        let terrain = TerrainRecovery::new(g.lattice, &heights, 0.2, 5000, 2).unwrap();
        let Outcome::Found(brute) = terrain
            .find_undirected(&g, &[[0, 0]], &[[8, 8]], settings(), None, 10000, || false)
            .unwrap()
        else {
            panic!()
        };
        let Outcome::Found(field) = terrain
            .find_undirected(
                &g,
                &[[0, 0]],
                &[[8, 8]; 40],
                settings(),
                None,
                10000,
                || false,
            )
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(field.cells, brute.cells);
        assert_eq!(field.cumulative_costs, brute.cumulative_costs);
        let mut meter = Meter {
            work: 0,
            limit: 1,
            cancelled: &|| false,
        };
        assert!(matches!(
            goal_distances(9, &[[0, 0]], &mut meter),
            Err(Outcome::Budget { work: 0 })
        ));
        let mut meter = Meter {
            work: 0,
            limit: 10000,
            cancelled: &|| true,
        };
        assert!(matches!(
            goal_distances(9, &[[0, 0]], &mut meter),
            Err(Outcome::Cancelled { work: 0 })
        ));
    }
    #[test]
    fn terrain_access_routes_around_steep_ground_and_preserves_blocked_cells() {
        let mut prices = vec![100; 81];
        prices[6 * 9 + 4] = 0;
        let g = grid(9, prices);
        let original = g.grid.costs.clone();
        let mut heights = vec![6.; 81];
        for x in 0..9 {
            heights[6 * 9 + x] = 0.;
            heights[7 * 9 + x] = 0.;
        }
        for z in 4..=6 {
            heights[z * 9] = 0.;
            heights[z * 9 + 8] = 0.;
        }
        let terrain = TerrainRecovery::new(g.lattice, &heights, 0.2, 5000, 2).unwrap();
        let Outcome::Found(before) = find_path(&g, [0, 4], [8, 4], settings(), || false).unwrap()
        else {
            panic!()
        };
        assert!(before.cells.iter().any(|p| heights[p[1] * 9 + p[0]] > 0.));
        let route = || {
            terrain
                .find_undirected(&g, &[[0, 4]], &[[8, 4]], settings(), None, 10000, || false)
                .unwrap()
        };
        let Outcome::Found(after) = route() else {
            panic!()
        };
        assert_eq!(after.engine, TERRAIN_ACCESS_VERSION);
        assert!(after.cells.iter().all(|p| heights[p[1] * 9 + p[0]] == 0.));
        assert!(
            after
                .cells
                .iter()
                .all(|p| g.grid.costs[p[1] * 9 + p[0]] > 0)
        );
        assert!(after.cells.len() > before.cells.len());
        assert!(after.cumulative_costs.windows(2).all(|p| p[0] > p[1]));
        let Outcome::Found(repeat) = route() else {
            panic!()
        };
        assert_eq!(repeat.cells, after.cells);
        assert_eq!(repeat.cumulative_costs, after.cumulative_costs);
        assert_eq!(g.grid.costs, original);
    }
    #[test]
    fn coastal_penalties_prefer_dry_detours_without_sealing_authored_entrances() {
        let mut costs = vec![0; 81];
        costs[4 * 9] = 100;
        let g = grid(9, costs);
        let original = g.grid.costs.clone();
        let heights = vec![10.; 81];
        let mut coast = vec![0; 81];
        for z in 1..9 {
            coast[z * 9 + 4] = 1;
        }
        let terrain = TerrainRecovery::new(g.lattice, &heights, 0.2, 5000, 4)
            .unwrap()
            .with_entrance_penalties(&coast)
            .unwrap();
        let run = |terrain: TerrainRecovery<'_>, settings| {
            terrain
                .find_closest(&g, [8, 4], None, 8, settings, || false)
                .unwrap()
        };
        let Outcome::Found(route) = run(terrain, settings()) else {
            panic!("Expected a dry detour");
        };
        assert!(route.cells.iter().all(|&p| !terrain.entrance_penalized(p)));
        assert!(route.cells.contains(&[4, 0]));
        let mut exact = settings();
        exact.maximum_work = route.work;
        let Outcome::Found(repeat) = run(terrain, exact) else {
            panic!()
        };
        assert_eq!(repeat.cells, route.cells);
        exact.maximum_work -= 1;
        assert!(matches!(run(terrain, exact), Outcome::Budget { .. }));
        let mut sealed_coast = coast.clone();
        sealed_coast[4] = 1;
        let sealed = terrain.with_entrance_penalties(&sealed_coast).unwrap();
        assert!(matches!(run(sealed, settings()), Outcome::Found(_)));
        let holes = sealed_coast.clone();
        assert!(matches!(
            run(sealed.with_entrance_exclusions(&holes).unwrap(), settings()),
            Outcome::Unreachable { .. }
        ));
        assert_eq!(g.grid.costs, original);
        assert!(terrain.with_entrance_penalties(&[0; 2]).is_err());
        let mut invalid = coast.clone();
        invalid[0] = 2;
        assert!(terrain.with_entrance_penalties(&invalid).is_err());
    }
    #[test]
    fn entrance_recovery_can_leave_its_own_footprint_and_routes_around_foreign_ones() {
        let mut costs = vec![0; 81];
        for z in 0..9 {
            costs[z * 9] = 100;
        }
        let g = grid(9, costs);
        let heights = vec![10.; 81];
        let mut footprints = vec![EntranceFootprint::Clear; 81];
        footprints[4 * 9 + 8] = EntranceFootprint::Monument(42);
        for z in 1..9 {
            footprints[z * 9 + 4] = EntranceFootprint::Monument(21);
        }
        let terrain = TerrainRecovery::new(g.lattice, &heights, 0.2, 5000, 4)
            .unwrap()
            .with_foreign_footprints(&footprints, &[42, 21])
            .unwrap();
        let run = |terrain: TerrainRecovery<'_>| {
            terrain.find_closest(&g, [8, 4], None, 8, settings(), || false).unwrap()
        };
        assert!(matches!(run(terrain.for_connector(1)), Outcome::Unreachable { .. }));
        let own = terrain.for_connector(0);
        assert!(own.entrance_clear([8, 4], [8, 4]).unwrap());
        assert!(!own.entrance_clear([3, 0], [4, 1]).unwrap());
        let Outcome::Found(route) = run(own) else {
            panic!("Expected a detour around the foreign monument");
        };
        assert!(route.cells.contains(&[4, 0]));
        let mut overlapping = footprints.clone();
        overlapping[4 * 9 + 8] = EntranceFootprint::Overlap;
        assert!(matches!(
            run(terrain.with_foreign_footprints(&overlapping, &[42, 21]).unwrap().for_connector(0)),
            Outcome::Unreachable { .. }
        ));
    }

    #[test]
    fn entrance_exclusions_avoid_holes_while_allowing_source_blocked_ground() {
        let mut costs = vec![0; 81];
        for z in 0..9 {
            costs[z * 9] = 100;
        }
        let g = grid(9, costs);
        let original = g.grid.costs.clone();
        let heights = vec![0.; 81];
        let mut excluded = vec![0; 81];
        for z in 0..9 {
            if z != 2 {
                excluded[z * 9 + 2] = 1;
            }
        }
        let terrain = TerrainRecovery::new(g.lattice, &heights, 0.2, 5000, 4)
            .unwrap()
            .with_entrance_exclusions(&excluded)
            .unwrap();
        let run = |settings| {
            terrain
                .find_closest(&g, [8, 4], None, 8, settings, || false)
                .unwrap()
        };
        let Outcome::Found(route) = run(settings()) else {
            panic!("Expected a safe entrance chain");
        };
        assert!(route.cells.contains(&[2, 2]));
        assert!(route.cells.iter().all(|p| excluded[p[1] * 9 + p[0]] == 0));
        assert!(
            route
                .cells
                .iter()
                .any(|p| g.grid.costs[p[1] * 9 + p[0]] == 0)
        );
        assert!(
            route
                .cells
                .windows(2)
                .all(|p| terrain.entrance_clear(p[0], p[1]).unwrap())
        );
        assert_eq!(g.grid.costs, original);
        let mut exact = settings();
        exact.maximum_work = route.work;
        let Outcome::Found(repeat) = run(exact) else {
            panic!();
        };
        assert_eq!(route.cells, repeat.cells);
        exact.maximum_work -= 1;
        assert!(matches!(run(exact), Outcome::Budget { .. }));
        assert!(matches!(
            terrain
                .find_closest(&g, [8, 4], None, 8, settings(), || true)
                .unwrap(),
            Outcome::Cancelled { .. }
        ));
        let mut sealed = excluded.clone();
        sealed[2 * 9 + 2] = 1;
        let sealed = TerrainRecovery::new(g.lattice, &heights, 0.2, 5000, 4)
            .unwrap()
            .with_entrance_exclusions(&sealed)
            .unwrap();
        assert!(matches!(
            sealed
                .find_closest(&g, [8, 4], None, 8, settings(), || false)
                .unwrap(),
            Outcome::Unreachable { .. }
        ));
        assert!(matches!(
            terrain
                .find_closest(&g, [2, 4], None, 8, settings(), || false)
                .unwrap(),
            Outcome::Unreachable { .. }
        ));
        assert!(terrain.with_entrance_exclusions(&[0; 2]).is_err());
        let mut invalid = excluded.clone();
        invalid[0] = 2;
        assert!(terrain.with_entrance_exclusions(&invalid).is_err());
    }
    #[test]
    fn terrain_access_obeys_label_work_and_cancellation_limits() {
        let g = grid(9, vec![100; 81]);
        let heights = vec![0.; 81];
        let terrain = TerrainRecovery::new(g.lattice, &heights, 0.2, 5000, 2).unwrap();
        assert!(matches!(
            terrain
                .find_undirected(&g, &[[0, 0]], &[[8, 8]], settings(), None, 1, || false)
                .unwrap(),
            Outcome::Budget { .. }
        ));
        assert!(matches!(
            terrain
                .find_undirected(
                    &g,
                    &[[0, 0]],
                    &[[8, 8]],
                    Settings {
                        maximum_work: 1,
                        ..settings()
                    },
                    None,
                    100,
                    || false
                )
                .unwrap(),
            Outcome::Budget { .. }
        ));
        assert!(matches!(
            terrain
                .find_undirected(&g, &[[0, 0]], &[[8, 8]], settings(), None, 100, || true)
                .unwrap(),
            Outcome::Cancelled { work: 0 }
        ));
        assert!(
            terrain
                .find_undirected(&g, &[[9, 0]], &[[8, 8]], settings(), None, 100, || false)
                .is_err()
        );
        assert!(
            terrain
                .find_undirected(&g, &[[0, 0]], &[[8, 8]], settings(), None, 0, || false)
                .is_err()
        );
        assert!(g.grid.costs.iter().all(|&v| v == 100));
    }
    #[test]
    fn terrain_entrance_prefers_a_longer_gentle_chain_and_keeps_the_original_network_mask() {
        let mut prices = vec![0; 81];
        for z in 0..9 {
            prices[z * 9 + 1] = 100;
        }
        prices[3 * 9 + 4] = 100; // Isolated walkable island must not be a goal.
        let g = grid(9, prices);
        let original = g.grid.costs.clone();
        let mut heights = vec![6.; 81];
        for z in 0..9 {
            heights[z * 9 + 1] = 0.;
        }
        for [x, z] in [[5, 4], [5, 5], [5, 6], [4, 7], [3, 7], [2, 7], [4, 3]] {
            heights[z * 9 + x] = 0.;
        }
        let ConnectedPreparation::Ready(goals) =
            prepare_connected(&g, &[[1, 0]], 10000, || false).unwrap()
        else {
            panic!()
        };
        let Outcome::Found(before) = goals.find_closest([5, 4], settings(), || false).unwrap()
        else {
            panic!()
        };
        assert!(before.cells.iter().any(|p| heights[p[1] * 9 + p[0]] > 0.));
        let guidance = TerrainRecovery::new(g.lattice, &heights, 0.2, 5000, 2).unwrap();
        let Outcome::Found(after) = guidance
            .find_closest(&g, [5, 4], Some(&goals), 5, settings(), || false)
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(after.engine, TERRAIN_ENTRANCE_VERSION);
        assert!(after.cells.len() > before.cells.len());
        assert!(after.cells.iter().all(|p| heights[p[1] * 9 + p[0]] == 0.));
        assert_eq!(after.cells.first().unwrap()[0], 1);
        assert_eq!(after.cells.last(), Some(&[5, 4]));
        assert!(after.cumulative_costs.windows(2).all(|w| w[0] > w[1]));
        assert_eq!(g.grid.costs, original);
        assert!(matches!(
            find_path(&g, [5, 4], [1, 0], settings(), || false).unwrap(),
            Outcome::Unreachable { .. }
        ));
        let clone = g.clone();
        assert!(
            guidance
                .find_closest(&clone, [5, 4], Some(&goals), 5, settings(), || false)
                .is_err()
        );
    }
    #[test]
    fn terrain_entrance_guidance_keeps_work_cancellation_and_lattice_bounds() {
        let mut prices = vec![0; 81];
        prices[0] = 100;
        let g = grid(9, prices);
        let heights = vec![0.; 81];
        let guidance = TerrainRecovery::new(g.lattice, &heights, 0.2, 5000, 1).unwrap();
        assert!(matches!(
            guidance
                .find_closest(&g, [8, 8], None, 0, settings(), || false)
                .unwrap(),
            Outcome::Unreachable { .. }
        ));
        assert!(matches!(
            guidance
                .find_closest(
                    &g,
                    [8, 8],
                    None,
                    8,
                    Settings {
                        maximum_work: 1,
                        ..settings()
                    },
                    || false
                )
                .unwrap(),
            Outcome::Budget { work: 0 }
        ));
        assert!(matches!(
            guidance
                .find_closest(&g, [8, 8], None, 8, settings(), || true)
                .unwrap(),
            Outcome::Cancelled { work: 0 }
        ));
        assert!(guidance.is_steep([9, 0], [0, 0]).is_err());
        let mut invalid = heights.clone();
        invalid[0] = f32::NAN;
        assert!(TerrainRecovery::new(g.lattice, &invalid, 0.2, 5000, 1).is_err());
        assert_eq!(g.grid.costs[0], 100);
    }
    #[test]
    fn connected_entrance_ignores_isolated_walkable_island_without_changing_prices() {
        let mut costs = vec![0; 81];
        for z in 0..9 {
            costs[z * 9 + 1] = 100;
        }
        costs[4 * 9 + 4] = 100;
        let g = grid(9, costs);
        let before = g.grid.costs.clone();
        let Outcome::Found(original) =
            find_closest_walkable(&g, [4, 5], settings(), || false).unwrap()
        else {
            panic!()
        };
        assert_eq!(original.cells[0], [4, 4]);
        let Outcome::Found(connected) =
            find_closest_connected(&g, [4, 5], &[[1, 0]], settings(), || false).unwrap()
        else {
            panic!()
        };
        assert_eq!(connected.engine, CONNECTED_ENTRANCE_VERSION);
        assert_eq!(connected.cells[0][0], 1);
        assert_eq!(connected.cells.last(), Some(&[4, 5]));
        assert!(matches!(
            find_path(&g, connected.cells[0], [1, 0], settings(), || false).unwrap(),
            Outcome::Found(_)
        ));
        assert!(matches!(
            find_path(&g, original.cells[0], [1, 0], settings(), || false).unwrap(),
            Outcome::Unreachable { .. }
        ));
        assert_eq!(g.grid.costs, before);
    }
    #[test]
    fn prepared_goals_preserve_queue_results_and_charge_one_flood_for_many_queries() {
        let mut costs = vec![0; 81];
        for z in 0..9 {
            costs[z * 9 + 1] = 100;
        }
        costs[4 * 9 + 4] = 100;
        let g = grid(9, costs);
        let original = g.grid.costs.clone();
        let ConnectedPreparation::Ready(goals) =
            prepare_connected(&g, &[[1, 0]], 100000, || false).unwrap()
        else {
            panic!()
        };
        let mut shared = goals.preparation_work();
        let mut repeated = 0;
        for start in [[4, 5], [5, 4], [7, 5], [8, 8]] {
            let Outcome::Found(expected) =
                find_closest_connected(&g, start, &[[1, 0]], settings(), || false).unwrap()
            else {
                panic!()
            };
            let Outcome::Found(actual) = goals.find_closest(start, settings(), || false).unwrap()
            else {
                panic!()
            };
            assert_eq!(actual.cells, expected.cells);
            assert_eq!(actual.cumulative_costs, expected.cumulative_costs);
            assert_eq!(actual.expansions, expected.expansions);
            shared += actual.work;
            repeated += expected.work;
        }
        assert!(shared < repeated, "{shared} >= {repeated}");
        assert_eq!(g.grid.costs, original);
    }
    #[test]
    fn prepared_goals_never_publish_partial_masks_or_free_query_initialization() {
        let g = grid(9, vec![100; 81]);
        assert!(
            matches!(prepare_connected(&g,&[[0,0]],200,||false).unwrap(),ConnectedPreparation::Budget{work} if work<=200)
        );
        assert!(matches!(
            prepare_connected(&g, &[[0, 0]], 10000, || true).unwrap(),
            ConnectedPreparation::Cancelled { work: 0 }
        ));
        assert!(prepare_connected(&g, &[[9, 0]], 10000, || false).is_err());
        let ConnectedPreparation::Ready(goals) =
            prepare_connected(&g, &[[0, 0]], 10000, || false).unwrap()
        else {
            panic!()
        };
        assert!(matches!(
            goals
                .find_closest(
                    [4, 4],
                    Settings {
                        maximum_work: 80,
                        ..settings()
                    },
                    || false
                )
                .unwrap(),
            Outcome::Budget { work: 0 }
        ));
        let ConnectedPreparation::Ready(empty) =
            prepare_connected(&g, &[], 10000, || false).unwrap()
        else {
            panic!()
        };
        assert!(matches!(
            empty.find_closest([4, 4], settings(), || false).unwrap(),
            Outcome::Unreachable { .. }
        ));
    }
    #[test]
    fn connected_entrance_validates_network_and_halts_atomically() {
        use std::cell::Cell;
        let g = grid(9, vec![100; 81]);
        for network in [vec![], vec![[9, 0]]] {
            let result = find_closest_connected(&g, [4, 4], &network, settings(), || false);
            if network.is_empty() {
                assert!(matches!(result.unwrap(), Outcome::Unreachable { .. }));
            } else {
                assert!(result.is_err());
            }
        }
        let blocked = grid(9, vec![0; 81]);
        assert!(matches!(
            find_closest_connected(&blocked, [4, 4], &[[0, 0]], settings(), || false).unwrap(),
            Outcome::Unreachable { .. }
        ));
        assert!(matches!(
            find_closest_connected(
                &g,
                [4, 4],
                &[[0, 0]],
                Settings {
                    maximum_work: 200,
                    ..settings()
                },
                || false
            )
            .unwrap(),
            Outcome::Budget { .. }
        ));
        let polls = Cell::new(0);
        assert!(matches!(
            find_closest_connected(&g, [4, 4], &[[0, 0]], settings(), || {
                polls.set(polls.get() + 1);
                polls.get() > 10
            })
            .unwrap(),
            Outcome::Cancelled { .. }
        ));
    }
    #[test]
    fn owned_entrance_recovery_matches_full_blocked_cell_chains() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/source-entrance-search-bd19c4ac.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let input = &case["input"];
            let rows = input["Rows"].as_array().unwrap();
            let g = grid(
                rows.len(),
                rows.iter()
                    .flat_map(|r| r.as_array().unwrap())
                    .map(|v| v.as_u64().unwrap() as u32)
                    .collect(),
            );
            let start: [usize; 2] = serde_json::from_value(input["Start"].clone()).unwrap();
            let outcome = find_closest_walkable(
                &g,
                start,
                Settings {
                    maximum_expansions: input["Depth"].as_u64().unwrap() as usize,
                    ..settings()
                },
                || false,
            )
            .unwrap();
            let expected: Vec<[usize; 2]> = serde_json::from_value(case["cells"].clone()).unwrap();
            if expected.is_empty() {
                assert!(matches!(
                    outcome,
                    Outcome::Unreachable { .. } | Outcome::DepthLimit { .. }
                ));
                continue;
            }
            let Outcome::Found(route) = outcome else {
                panic!("Missing entrance recovery for {}", input["Name"]);
            };
            let costs: Vec<u64> = serde_json::from_value(case["costs"].clone()).unwrap();
            assert_eq!(route.cells, expected, "{}", input["Name"]);
            assert_eq!(route.cumulative_costs, costs);
            assert_eq!(route.cells.last(), Some(&start));
            let recovered = route.cells[0];
            assert_ne!(g.grid.costs[recovered[1] * rows.len() + recovered[0]], 0);
        }
    }
    #[test]
    fn owned_pushed_searches_match_cells_and_f32_ceiling_prices() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/source-pushed-road-search-bd19c4ac.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let input = &case["input"];
            let rows = input["Rows"].as_array().unwrap();
            let g = grid(
                rows.len(),
                rows.iter()
                    .flat_map(|r| r.as_array().unwrap())
                    .map(|v| v.as_u64().unwrap() as u32)
                    .collect(),
            );
            let point = |name| serde_json::from_value::<[usize; 2]>(input[name].clone()).unwrap();
            let p = &input["Push"];
            let push = Push {
                point: serde_json::from_value(p["Point"].clone()).unwrap(),
                radius: p["Radius"].as_u64().unwrap() as usize,
                distance: p["Distance"].as_u64().unwrap() as usize,
                multiplier: p["Multiplier"].as_u64().unwrap() as u32,
            };
            let Outcome::Found(route) = find_undirected_with_push(
                &g,
                &[point("Start")],
                &[point("End")],
                settings(),
                Some(push),
                || false,
            )
            .unwrap() else {
                panic!("Missing pushed route {}", input["Name"]);
            };
            let cells: Vec<[usize; 2]> = serde_json::from_value(case["cells"].clone()).unwrap();
            let costs: Vec<u64> = serde_json::from_value(case["costs"].clone()).unwrap();
            assert_eq!(route.cells, cells, "{}", input["Name"]);
            assert_eq!(route.cumulative_costs, costs, "{}", input["Name"]);
        }
        let g = grid(5, vec![100; 25]);
        assert!(
            find_undirected_with_push(
                &g,
                &[[0, 0]],
                &[[4, 4]],
                settings(),
                Some(Push {
                    point: [2, 2],
                    radius: 2,
                    distance: 0,
                    multiplier: 50000,
                }),
                || false
            )
            .is_err()
        );
    }
    #[test]
    fn entrance_recovery_is_bounded_atomic_and_keeps_blocked_cells_immutable() {
        use std::cell::Cell;
        let g = grid(33, vec![0; 33 * 33]);
        let before = g.grid.costs.clone();
        assert!(matches!(
            find_closest_walkable(&g, [16, 16], settings(), || false).unwrap(),
            Outcome::Unreachable { .. }
        ));
        assert!(matches!(
            find_closest_walkable(
                &g,
                [16, 16],
                Settings {
                    maximum_work: 1,
                    ..settings()
                },
                || false
            )
            .unwrap(),
            Outcome::Budget { .. }
        ));
        let polls = Cell::new(0);
        let outcome = find_closest_walkable(&g, [16, 16], settings(), || {
            polls.set(polls.get() + 1);
            polls.get() > 100
        })
        .unwrap();
        assert!(matches!(outcome, Outcome::Cancelled { work } if work > 33 * 33));
        assert_eq!(g.grid.costs, before);
        assert!(find_closest_walkable(&g, [33, 0], settings(), || false).is_err());
    }
    #[test]
    fn inspected_queue_keeps_pinned_head_and_newest_first_tail_ties() {
        let mut queue = SourceQueue::default();
        queue.push(5, 0);
        queue.push(10, 1);
        queue.push(1, 2);
        assert_eq!(
            [queue.pop(), queue.pop(), queue.pop()],
            [Some(0), Some(2), Some(1)]
        );
        queue.push(5, 0);
        queue.push(5, 1);
        queue.push(5, 2);
        assert_eq!(
            [queue.pop(), queue.pop(), queue.pop()],
            [Some(1), Some(2), Some(0)]
        );
    }
    #[test]
    fn owned_server_numeric_routes_match_step_costs_and_endpoints() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/path-node-costs-bd19c4ac.json"
        ))
        .unwrap();
        for case in fixture["cases"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["input"]["Chain"].is_null())
        {
            let rows = case["input"]["Rows"].as_array().unwrap();
            let g = grid(
                rows.len(),
                rows.iter()
                    .flat_map(|r| r.as_array().unwrap())
                    .map(|v| v.as_u64().unwrap() as u32)
                    .collect(),
            );
            let point = |name| {
                let a = case["input"][name].as_array().unwrap();
                [
                    a[0].as_u64().unwrap() as usize,
                    a[1].as_u64().unwrap() as usize,
                ]
            };
            let Outcome::Found(route) =
                find_path(&g, point("Start"), point("End"), settings(), || false).unwrap()
            else {
                panic!("Missing source route");
            };
            let expected: Vec<[usize; 2]> = serde_json::from_value(case["cells"].clone()).unwrap();
            let costs: Vec<u64> = serde_json::from_value(case["costs"].clone()).unwrap();
            assert_eq!(route.cells, expected, "{}", case["input"]["Name"]);
            assert_eq!(route.cumulative_costs, costs);
        }
    }
    #[test]
    fn captured_searches_match_queue_ties_and_directional_toggle() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/source-road-search-bd19c4ac.json"
        ))
        .unwrap();
        assert_eq!(
            fixture["assembly_sha256"],
            "bd19c4ac705edd18ff556f5dbb3130e098b380577cc2ab0d73a2ec89ee289cc9"
        );
        assert_eq!(
            fixture["queue_sha256"],
            "04fbac3b53a4e8ee90338cc1c17fb92b07639cbbd70c250740f549fed1c9a8df"
        );
        let mut matched = 0;
        for case in fixture["cases"].as_array().unwrap() {
            assert!(case["input"]["Chain"].is_null());
            let rows = case["input"]["Rows"].as_array().unwrap();
            let g = grid(
                rows.len(),
                rows.iter()
                    .flat_map(|r| r.as_array().unwrap())
                    .map(|v| v.as_u64().unwrap() as u32)
                    .collect(),
            );
            let point = |name| {
                let p = case["input"][name].as_array().unwrap();
                [
                    p[0].as_u64().unwrap() as usize,
                    p[1].as_u64().unwrap() as usize,
                ]
            };
            let mut policy = settings();
            if case["input"]["Directional"] == false {
                policy.directional_penalty = 0;
            }
            let Outcome::Found(route) =
                find_path(&g, point("Start"), point("End"), policy, || false).unwrap()
            else {
                panic!("Missing captured route");
            };
            let expected: Vec<[usize; 2]> = serde_json::from_value(case["cells"].clone()).unwrap();
            let costs: Vec<u64> = serde_json::from_value(case["costs"].clone()).unwrap();
            assert_eq!(route.cells, expected, "{}", case["input"]["Name"]);
            assert_eq!(route.cumulative_costs, costs);
            matched += 1;
        }
        assert!(matched > 0);
        eprintln!("Matched {matched} captured source road queries");
    }
    #[test]
    fn source_diagonal_proposals_and_mid_search_cancellation_keep_inputs_immutable() {
        let mut costs = vec![0; 25];
        for i in 0..5 {
            costs[i * 5 + i] = 101;
        }
        let g = grid(5, costs);
        let before = g.grid.costs.clone();
        let Outcome::Found(route) = find_path(&g, [0, 0], [4, 4], settings(), || false).unwrap()
        else {
            panic!();
        };
        assert_eq!(route.cells, (0..5).map(|i| [i, i]).collect::<Vec<_>>());
        let polls = std::cell::Cell::new(0);
        let stopped = find_path(&g, [0, 0], [4, 4], settings(), || {
            polls.set(polls.get() + 1);
            polls.get() > 5
        })
        .unwrap();
        assert!(matches!(stopped, Outcome::Cancelled { work } if work > 0));
        assert_eq!(g.grid.costs, before);
    }
    #[test]
    fn blocked_cancelled_exhausted_and_multi_endpoint_queries_stay_explicit() {
        let g = grid(5, vec![101; 25]);
        let Outcome::Found(route) =
            find_undirected(&g, &[[0, 0], [4, 0]], &[[4, 4]], settings(), || false).unwrap()
        else {
            panic!();
        };
        // Both targets are four Moore steps away at equal cost. Source queue
        // order chooses the tie; Euclidean proximity is not its objective.
        assert!([[0, 0], [4, 0]].contains(route.cells.first().unwrap()));
        assert_eq!(route.cumulative_costs[0], 505);
        assert_eq!(route.cells.last(), Some(&[4, 4]));
        assert!(matches!(
            find_path(&g, [0, 0], [4, 4], settings(), || true).unwrap(),
            Outcome::Cancelled { .. }
        ));
        assert!(matches!(
            find_path(
                &g,
                [0, 0],
                [4, 4],
                Settings {
                    maximum_work: 1,
                    ..settings()
                },
                || false
            )
            .unwrap(),
            Outcome::Budget { .. }
        ));
        let mut blocked = g;
        blocked.grid.costs.fill(0);
        assert!(matches!(
            find_path(&blocked, [0, 0], [4, 4], settings(), || false).unwrap(),
            Outcome::Unreachable { .. }
        ));
        blocked.grid.engine = crate::road_costmap::VERSION;
        assert!(find_path(&blocked, [0, 0], [4, 4], settings(), || false).is_err());
    }
}
