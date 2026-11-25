//! Source-lattice ring candidate assembly. Raw procgen nodes remain separate
//! from shaped controls. Terrain/mesh validity and world publication are later.
use crate::{
    path_geometry,
    road_costmap::{CELL_VERSION, CellGrid, CellLattice},
    road_ring, road_ring_join,
    source_road_route::{self, Settings as SearchSettings},
};
use serde::Serialize;
pub const VERSION: &str = "papyrus-source-road-ring-1";
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct RawNode {
    pub cell: [usize; 2],
    pub cost: u64,
}
#[derive(Debug, Serialize)]
pub struct Audit {
    pub engine: &'static str,
    pub policy: road_ring::Policy,
    pub join_policy: road_ring_join::Policy,
    pub search: SearchSettings,
    pub maximum_work: usize,
    pub work: usize,
    pub join_work: usize,
    pub queries: usize,
    pub depth_rejections: usize,
    pub waypoint_moves: usize,
    pub waypoints: Vec<[usize; 2]>,
    pub bridge_attempts: usize,
    pub bridges: usize,
    pub trimmed_nodes: usize,
    pub reason: Option<&'static str>,
}
#[derive(Debug, Serialize)]
pub struct Ring {
    pub lattice: CellLattice,
    pub nodes: Vec<RawNode>,
    pub audit: Audit,
}

/// The raw attachment lattice and the shaped controls handed to road families.
#[derive(Debug)]
pub struct Layout {
    pub lattice: CellLattice,
    pub nodes: Vec<RawNode>,
    pub path: path_geometry::Path,
}
#[derive(Debug)]
pub enum Outcome {
    Found(Ring),
    Skipped(Audit),
    Unresolved(Audit),
    Budget(Audit),
    Cancelled(Audit),
}
enum Halt {
    Unresolved(&'static str),
    Budget,
    Cancelled,
    Invalid(String),
}
struct Waypoint {
    cell: [usize; 2],
    direction: [isize; 2],
    remaining: usize,
}
struct Context<'a, F> {
    grid: &'a CellGrid,
    policy: road_ring::Policy,
    joins: &'a road_ring_join::Policy,
    search: SearchSettings,
    maximum_work: usize,
    cancelled: &'a F,
    audit: Audit,
}
impl<F: Fn() -> bool> Context<'_, F> {
    fn completed(&mut self, amount: usize) -> Result<(), Halt> {
        if amount > self.maximum_work.saturating_sub(self.audit.work) {
            return Err(Halt::Budget);
        }
        self.audit.work += amount;
        Ok(())
    }
    fn spend(&mut self, amount: usize) -> Result<(), Halt> {
        if (self.cancelled)() {
            return Err(Halt::Cancelled);
        }
        if amount > self.maximum_work.saturating_sub(self.audit.work) {
            return Err(Halt::Budget);
        }
        self.audit.work += amount;
        Ok(())
    }
    fn query(&mut self, a: [usize; 2], b: [usize; 2]) -> Result<Option<Vec<RawNode>>, Halt> {
        self.spend(0)?;
        if self.audit.queries == self.policy.maximum_attempts {
            return Err(Halt::Budget);
        }
        let available = self.maximum_work - self.audit.work;
        if available == 0 {
            return Err(Halt::Budget);
        }
        self.audit.queries += 1;
        let outcome = source_road_route::find_path(
            self.grid,
            a,
            b,
            SearchSettings {
                maximum_work: self.search.maximum_work.min(available),
                ..self.search
            },
            self.cancelled,
        )
        .map_err(Halt::Invalid)?;
        match outcome {
            source_road_route::Outcome::Found(route) => {
                self.completed(route.work)?;
                self.spend(0)?;
                if route.cells.len() > self.joins.maximum_points {
                    return Err(Halt::Budget);
                }
                Ok(Some(
                    route
                        .cells
                        .into_iter()
                        .zip(route.cumulative_costs)
                        .map(|(cell, cost)| RawNode { cell, cost })
                        .collect(),
                ))
            }
            source_road_route::Outcome::Unreachable { work } => {
                self.completed(work)?;
                self.spend(0)?;
                Ok(None)
            }
            source_road_route::Outcome::DepthLimit { work } => {
                self.completed(work)?;
                self.spend(0)?;
                self.audit.depth_rejections += 1;
                Ok(None)
            }
            source_road_route::Outcome::Budget { work } => {
                self.completed(work)?;
                self.spend(0)?;
                Err(Halt::Budget)
            }
            source_road_route::Outcome::Cancelled { work } => {
                self.completed(work)?;
                Err(Halt::Cancelled)
            }
        }
    }
    fn shift(&mut self, node: &mut Waypoint) -> Result<(), Halt> {
        loop {
            self.spend(1)?;
            if node.remaining == 0 {
                return Err(Halt::Unresolved("Ring waypoint movement exhausted"));
            }
            node.remaining -= 1;
            for axis in 0..2 {
                node.cell[axis] = node.cell[axis]
                    .checked_add_signed(
                        node.direction[axis] * self.policy.inward_step_cells as isize,
                    )
                    .filter(|&v| v < self.grid.lattice.resolution())
                    .ok_or(Halt::Unresolved("Ring waypoint left its grid"))?;
            }
            self.audit.waypoint_moves += 1;
            if self.walkable(node.cell) {
                return Ok(());
            }
        }
    }
    fn walkable(&self, cell: [usize; 2]) -> bool {
        self.grid.grid.costs[cell[1] * self.grid.lattice.resolution() + cell[0]] != 0
    }
    fn near(
        &mut self,
        a: &[RawNode],
        b: &[RawNode],
        radius: usize,
    ) -> Result<Option<(usize, usize)>, Halt> {
        let (mut left, mut right): (Option<usize>, Option<usize>) = (None, None);
        for (i, x) in a.iter().enumerate() {
            for (j, y) in b.iter().enumerate() {
                self.spend(1)?;
                if self.audit.join_work == self.joins.maximum_work {
                    return Err(Halt::Budget);
                }
                self.audit.join_work += 1;
                if x.cell[0].abs_diff(y.cell[0]) <= radius
                    && x.cell[1].abs_diff(y.cell[1]) <= radius
                {
                    if left.is_none_or(|old| x.cost > a[old].cost) {
                        left = Some(i);
                    }
                    if right.is_none_or(|old| y.cost < b[old].cost) {
                        right = Some(j);
                    }
                }
            }
        }
        Ok(left.zip(right))
    }
    fn assemble(&mut self) -> Result<Vec<RawNode>, Halt> {
        let n = self.grid.lattice.resolution();
        let q = n / self.policy.inset_divisor;
        if q == 0 {
            return Err(Halt::Invalid("Ring grid has no inset cells".into()));
        }
        let (lo, hi, mid) = (q, n - q, n / 2);
        let positions = if self.grid.lattice.size() >= self.policy.large_world_size as f32 {
            vec![
                ([mid, hi], [0, -1]),
                ([hi, hi], [-1, -1]),
                ([hi, mid], [-1, 0]),
                ([hi, lo], [-1, 1]),
                ([mid, lo], [0, 1]),
                ([lo, lo], [1, 1]),
                ([lo, mid], [1, 0]),
                ([lo, hi], [1, -1]),
            ]
        } else {
            vec![
                ([hi, hi], [-1, -1]),
                ([hi, lo], [-1, 1]),
                ([lo, lo], [1, 1]),
                ([lo, hi], [1, -1]),
            ]
        };
        let mut nodes: Vec<_> = positions
            .into_iter()
            .map(|(cell, direction)| Waypoint {
                cell,
                direction,
                remaining: q / self.policy.inward_step_cells,
            })
            .collect();
        for node in &mut nodes {
            if !self.walkable(node.cell) {
                self.shift(node)?;
            }
        }
        self.audit.waypoints = nodes.iter().map(|w| w.cell).collect();
        let count = nodes.len();
        let mut legs = Vec::with_capacity(count);
        for i in 0..count {
            legs.push(self.query(nodes[i].cell, nodes[(i + 1) % count].cell)?);
        }
        loop {
            self.spend(count)?;
            let center = nodes
                .iter()
                .fold([0; 2], |a, w| [a[0] + w.cell[0], a[1] + w.cell[1]])
                .map(|v| v / count);
            let mut selected = None;
            let mut best = f32::MIN;
            for i in 0..count {
                if legs[i].is_none() {
                    let dx = nodes[i].cell[0] as f32 - center[0] as f32;
                    let dz = nodes[i].cell[1] as f32 - center[1] as f32;
                    let mut score = (dx * dx + dz * dz).sqrt();
                    if legs[(i + count - 1) % count].is_none() {
                        score *= 1.5;
                    }
                    if score > best {
                        best = score;
                        selected = Some(i);
                    }
                }
            }
            let Some(i) = selected else {
                break;
            };
            self.shift(&mut nodes[i])?;
            self.audit.waypoints = nodes.iter().map(|w| w.cell).collect();
            legs[i] = self.query(nodes[i].cell, nodes[(i + 1) % count].cell)?;
            let prev = (i + count - 1) % count;
            legs[prev] = self.query(nodes[prev].cell, nodes[i].cell)?;
        }
        let mut legs: Vec<Vec<RawNode>> = legs.into_iter().map(|v| v.unwrap()).collect();
        // Bridge all neighbors first. Node costs from each query survive the
        // splice; repricing the concatenated route would change later choices.
        for i in 0..count {
            let next = (i + 1) % count;
            if let Some((a, b)) =
                self.near(&legs[i], &legs[next], self.joins.bridge_radius_cells)?
            {
                self.audit.bridge_attempts += 1;
                if let Some(bridge) = self.query(legs[i][a].cell, legs[next][b].cell)?
                    && bridge.len() >= 2
                {
                    let len = a + bridge.len();
                    if len > self.joins.maximum_points {
                        return Err(Halt::Budget);
                    }
                    self.spend(len + legs[next].len())?;
                    legs[i].truncate(a + 1);
                    legs[i].extend_from_slice(&bridge[1..]);
                    legs[next].drain(..b);
                    self.audit.bridges += 1;
                }
            }
        }
        for i in 0..count {
            let next = (i + 1) % count;
            if let Some((a, b)) = self.near(&legs[i], &legs[next], self.joins.trim_radius_cells)? {
                self.spend(legs[i].len() + legs[next].len())?;
                self.audit.trimmed_nodes += legs[i].len() - a - 1 + b;
                legs[i].truncate(a + 1);
                legs[next].drain(..b);
            }
        }
        let total = legs
            .iter()
            .try_fold(1usize, |sum, p| sum.checked_add(p.len()))
            .ok_or(Halt::Budget)?;
        if total > self.joins.maximum_points {
            return Err(Halt::Budget);
        }
        self.spend(total)?;
        let mut raw: Vec<_> = legs.into_iter().flatten().collect();
        let first = *raw
            .first()
            .ok_or(Halt::Unresolved("Ring has no raw nodes"))?;
        raw.push(first);
        Ok(raw)
    }
}
pub fn assemble(
    grid: &CellGrid,
    policy: road_ring::Policy,
    joins: &road_ring_join::Policy,
    search: SearchSettings,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Outcome, String> {
    policy.validate()?;
    joins.validate()?;
    let n = grid.lattice.resolution();
    if grid.grid.engine != CELL_VERSION
        || grid.grid.resolution != n
        || grid.grid.costs.len() != n * n
        || !(1..=128_000_000).contains(&maximum_work)
        || !(1..=128_000_000).contains(&search.maximum_work)
        || !(1..=1_000_000).contains(&search.maximum_expansions)
        || search.directional_penalty != joins.directional_penalty
        || search.directional_radius != joins.directional_radius_cells
        || grid.lattice != CellLattice::new(grid.lattice.size(), joins.source_grid_spacing)?
    {
        return Err("Invalid source ring request".into());
    }
    let mut context = Context {
        grid,
        policy,
        joins,
        search,
        maximum_work,
        cancelled: &cancelled,
        audit: Audit {
            engine: VERSION,
            policy,
            join_policy: joins.clone(),
            search,
            maximum_work,
            work: 0,
            join_work: 0,
            queries: 0,
            depth_rejections: 0,
            waypoint_moves: 0,
            waypoints: vec![],
            bridge_attempts: 0,
            bridges: 0,
            trimmed_nodes: 0,
            reason: None,
        },
    };
    if cancelled() {
        return Ok(Outcome::Cancelled(context.audit));
    }
    if grid.lattice.size() < policy.minimum_world_size as f32 {
        return Ok(Outcome::Skipped(context.audit));
    }
    // Verify caller-owned prices even if all initial waypoints are blocked.
    for chunk in grid.grid.costs.chunks(1024) {
        match context.spend(chunk.len()) {
            Err(Halt::Cancelled) => return Ok(Outcome::Cancelled(context.audit)),
            Err(_) => return Ok(Outcome::Budget(context.audit)),
            Ok(()) => {}
        }
        if chunk.iter().any(|&v| v > 1_000_000) {
            return Err("Invalid source ring price".into());
        }
    }
    match context.assemble() {
        Ok(nodes) => Ok(Outcome::Found(Ring {
            lattice: grid.lattice,
            nodes,
            audit: context.audit,
        })),
        Err(Halt::Unresolved(reason)) => {
            context.audit.reason = Some(reason);
            Ok(Outcome::Unresolved(context.audit))
        }
        Err(Halt::Budget) => Ok(Outcome::Budget(context.audit)),
        Err(Halt::Cancelled) => Ok(Outcome::Cancelled(context.audit)),
        Err(Halt::Invalid(error)) => Err(error),
    }
}
pub struct Shaped {
    pub path: path_geometry::Path,
    pub total_work: usize,
}
impl Ring {
    /// Immutable source raw chain; later monument attachments use these nodes,
    /// not a reconstructed sample of the smoothed Hermite path.
    pub fn shape(
        &self,
        settings: path_geometry::Settings,
        mut height: impl FnMut([f32; 2]) -> Result<f32, String>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Shaped>, String> {
        if !(1..=32_000_000).contains(&settings.maximum_work)
            || settings.horizontal_iterations > 256
            || settings.vertical_iterations > 256
            || !settings.resample_distance.is_finite()
            || !(0.1..=100.).contains(&settings.resample_distance)
            || !(2..=32768).contains(&self.nodes.len())
        {
            return Err("Invalid source ring shaping request".into());
        }
        if self.nodes.len() > settings.maximum_work {
            return Err("Ring conversion work exhausted".into());
        }
        let mut points = Vec::with_capacity(self.nodes.len());
        for node in &self.nodes {
            if cancelled() {
                return Ok(None);
            }
            let uv = self
                .lattice
                .normalized(node.cell)
                .ok_or("Invalid raw ring cell")?;
            let [x, z] = self
                .lattice
                .point(node.cell)
                .ok_or("Invalid raw ring point")?;
            let y = height(uv)?;
            if !y.is_finite() || y.abs() > 10000. {
                return Err("Invalid ring terrain height".into());
            }
            points.push([x, y.max(1.), z]);
        }
        if points.len() < 2 || points.first() != points.last() {
            return Err("Source ring terrain samples lost closure".into());
        }
        let Some(remaining) = settings
            .maximum_work
            .checked_sub(points.len())
            .filter(|&v| v > 0)
        else {
            return Err("Ring shaping work exhausted".into());
        };
        let Some(path) = path_geometry::build(
            &points,
            path_geometry::Settings {
                maximum_work: remaining,
                ..settings
            },
            None,
            cancelled,
        )
        .map_err(|e| format!("Source ring shaping failed: {e:?}"))?
        else {
            return Ok(None);
        };
        let total_work = path.work() + points.len();
        Ok(Some(Shaped { path, total_work }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::road_costmap::{Counts, Grid};
    fn fixture(size: f32) -> CellGrid {
        let lattice = CellLattice::new(size, 7.5).unwrap();
        let n = lattice.resolution();
        CellGrid {
            lattice,
            grid: Grid {
                engine: CELL_VERSION,
                resolution: n,
                seed: 1,
                trail: false,
                counts: Counts::default(),
                work: 0,
                costs: vec![101; n * n],
            },
        }
    }
    fn policy() -> road_ring::Policy {
        road_ring::Policy {
            minimum_world_size: 1000,
            large_world_size: 5000,
            inset_divisor: 4,
            inward_step_cells: 1,
            maximum_attempts: 128,
            width: 12.,
            hierarchy: 0,
            network_sample_stride: 8,
        }
    }
    fn joins() -> road_ring_join::Policy {
        road_ring_join::Policy {
            river_water: None,
            engine: road_ring_join::REUSE_VERSION.into(),
            source_grid_spacing: 7.5,
            bridge_radius_cells: 15,
            trim_radius_cells: 1,
            directional_penalty: 10000,
            directional_radius_cells: 1,
            fit_heights_in_search: true,
            maximum_work: 32_000_000,
            maximum_points: 65536,
        }
    }
    fn search() -> SearchSettings {
        SearchSettings {
            maximum_expansions: 250000,
            maximum_work: 128_000_000,
            directional_penalty: 10000,
            directional_radius: 1,
        }
    }
    #[test]
    fn query_work_exhaustion_does_not_masquerade_as_source_depth_recovery() {
        let grid = fixture(1000.);
        let Outcome::Budget(audit) = assemble(
            &grid,
            policy(),
            &joins(),
            SearchSettings {
                maximum_work: 1,
                ..search()
            },
            128_000_000,
            || false,
        )
        .unwrap() else {
            panic!("Expected a terminal work cap");
        };
        assert_eq!(audit.queries, 1);
        assert_eq!(audit.depth_rejections, 0);
        assert_eq!(audit.waypoint_moves, 0);
        let outcome = assemble(
            &grid,
            policy(),
            &joins(),
            SearchSettings {
                maximum_expansions: 1,
                ..search()
            },
            128_000_000,
            || false,
        )
        .unwrap();
        let audit = match outcome {
            Outcome::Budget(audit) | Outcome::Unresolved(audit) => audit,
            _ => panic!("One-step queries cannot complete this ring"),
        };
        assert!(audit.depth_rejections > 0);
        assert!(audit.waypoint_moves > 0);
    }
    #[test]
    fn raw_ring_repeats_closes_and_keeps_source_nodes_separate_from_shaping() {
        let grid = fixture(1000.);
        let before = grid.grid.costs.clone();
        let run = || {
            let Outcome::Found(ring) =
                assemble(&grid, policy(), &joins(), search(), 128_000_000, || false).unwrap()
            else {
                panic!("No synthetic ring");
            };
            ring
        };
        let ring = run();
        assert_eq!(ring.nodes, run().nodes);
        assert_eq!(ring.nodes.first(), ring.nodes.last());
        assert_eq!(
            ring.audit.waypoints,
            vec![[100, 100], [100, 33], [33, 33], [33, 100]]
        );
        assert!(ring.audit.bridges > 0 && ring.audit.queries >= 4);
        assert!(ring.audit.work <= 128_000_000);
        let raw = ring.nodes.clone();
        let shaped = ring
            .shape(
                path_geometry::Settings {
                    horizontal_iterations: 4,
                    vertical_iterations: 16,
                    resample_distance: 7.5,
                    maximum_work: 2_000_000,
                },
                |_| Ok(20.),
                || false,
            )
            .unwrap()
            .unwrap();
        assert!(shaped.path.is_circular());
        assert!(shaped.total_work <= 2_000_000);
        assert_eq!(ring.nodes, raw);
        assert_eq!(grid.grid.costs, before);
    }
    #[test]
    fn source_large_world_has_eight_waypoints_and_blocked_corners_move_inward() {
        let mut grid = fixture(5000.);
        let n = grid.lattice.resolution();
        grid.grid.costs[500 * n + 500] = 0;
        let Outcome::Found(ring) =
            assemble(&grid, policy(), &joins(), search(), 128_000_000, || false).unwrap()
        else {
            panic!("No large synthetic ring");
        };
        assert_eq!(ring.audit.waypoints.len(), 8);
        assert_eq!(ring.audit.waypoints[0], [333, 500]);
        assert_eq!(ring.audit.waypoints[1], [499, 499]);
        assert_eq!(ring.audit.waypoint_moves, 1);
        assert_eq!(ring.nodes.first(), ring.nodes.last());
    }
    #[test]
    fn cancellation_limits_skip_and_unresolved_do_not_publish_raw_nodes() {
        let mut grid = fixture(1000.);
        assert!(matches!(
            assemble(&grid, policy(), &joins(), search(), 128_000_000, || true).unwrap(),
            Outcome::Cancelled(_)
        ));
        assert!(matches!(
            assemble(&grid, policy(), &joins(), search(), 1, || false).unwrap(),
            Outcome::Budget(_)
        ));
        grid.grid.costs.fill(0);
        assert!(matches!(
            assemble(&grid, policy(), &joins(), search(), 128_000_000, || false).unwrap(),
            Outcome::Unresolved(_)
        ));
        let small = fixture(500.);
        assert!(matches!(
            assemble(&small, policy(), &joins(), search(), 128_000_000, || false).unwrap(),
            Outcome::Skipped(_)
        ));
        let mut mismatch = joins();
        mismatch.source_grid_spacing = 8.;
        assert!(assemble(&grid, policy(), &mismatch, search(), 128_000_000, || false).is_err());
    }
    #[test]
    fn overlap_targets_use_query_cost_extrema_instead_of_array_ends() {
        let grid = fixture(1000.);
        let join = joins();
        let cancelled = || false;
        let mut context = Context {
            grid: &grid,
            policy: policy(),
            joins: &join,
            search: search(),
            maximum_work: 100000,
            cancelled: &cancelled,
            audit: Audit {
                engine: VERSION,
                policy: policy(),
                join_policy: join.clone(),
                search: search(),
                maximum_work: 100000,
                work: 0,
                join_work: 0,
                queries: 0,
                depth_rejections: 0,
                waypoint_moves: 0,
                waypoints: vec![],
                bridge_attempts: 0,
                bridges: 0,
                trimmed_nodes: 0,
                reason: None,
            },
        };
        let a = [
            RawNode {
                cell: [4, 4],
                cost: 8,
            },
            RawNode {
                cell: [4, 5],
                cost: 20,
            },
            RawNode {
                cell: [4, 6],
                cost: 3,
            },
        ];
        let b = [
            RawNode {
                cell: [5, 6],
                cost: 30,
            },
            RawNode {
                cell: [5, 5],
                cost: 10,
            },
            RawNode {
                cell: [5, 4],
                cost: 25,
            },
        ];
        assert_eq!(context.near(&a, &b, 1).ok(), Some(Some((1, 1))));
    }
    #[test]
    fn installed_search_node_splices_and_interpolator_match_numeric_control_study() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/source-road-ring-bd19c4ac.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let input = &case["input"];
            let number = |name: &str| input[name].as_u64().unwrap() as usize;
            let size = input["Size"].as_f64().unwrap() as f32;
            let spacing = input["Spacing"].as_f64().unwrap() as f32;
            let lattice = CellLattice::new(size, spacing).unwrap();
            let grid = CellGrid {
                lattice,
                grid: Grid {
                    engine: CELL_VERSION,
                    resolution: lattice.resolution(),
                    seed: 0,
                    trail: false,
                    work: 0,
                    counts: Counts::default(),
                    costs: input["Rows"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .flat_map(|r| r.as_array().unwrap())
                        .map(|v| v.as_u64().unwrap() as u32)
                        .collect(),
                },
            };
            let before = grid.grid.costs.clone();
            let policy = road_ring::Policy {
                minimum_world_size: number("Minimum") as u32,
                large_world_size: number("Large") as u32,
                inset_divisor: number("Inset"),
                inward_step_cells: number("Step"),
                ..policy()
            };
            let joins = road_ring_join::Policy {
                source_grid_spacing: spacing,
                bridge_radius_cells: number("BridgeRadius"),
                trim_radius_cells: number("TrimRadius"),
                ..joins()
            };
            let search = SearchSettings {
                maximum_expansions: fixture["source_depth"].as_u64().unwrap() as usize,
                ..search()
            };
            let result = assemble(&grid, policy, &joins, search, 128_000_000, || false).unwrap();
            if case["status"] == "found" {
                let Outcome::Found(ring) = result else {
                    panic!("{}: ring not found", input["Name"]);
                };
                let cells: Vec<[usize; 2]> = serde_json::from_value(case["cells"].clone()).unwrap();
                let costs: Vec<u64> = serde_json::from_value(case["costs"].clone()).unwrap();
                assert_eq!(
                    ring.nodes.iter().map(|n| n.cell).collect::<Vec<_>>(),
                    cells,
                    "{}",
                    input["Name"]
                );
                assert_eq!(
                    ring.nodes.iter().map(|n| n.cost).collect::<Vec<_>>(),
                    costs,
                    "{}",
                    input["Name"]
                );
                assert_eq!(
                    ring.audit.queries,
                    case["queries"].as_u64().unwrap() as usize
                );
                assert_eq!(
                    ring.audit.waypoint_moves,
                    case["waypoint_moves"].as_u64().unwrap() as usize
                );
                assert_eq!(
                    ring.audit.bridges,
                    case["bridges"].as_u64().unwrap() as usize
                );
                assert_eq!(
                    ring.audit.trimmed_nodes,
                    case["trimmed_nodes"].as_u64().unwrap() as usize
                );
                assert_eq!(serde_json::json!(ring.audit.waypoints), case["waypoints"]);
                let path = ring
                    .shape(
                        path_geometry::Settings {
                            horizontal_iterations: 4,
                            vertical_iterations: 16,
                            resample_distance: 7.5,
                            maximum_work: 2_000_000,
                        },
                        |_| Ok(20.),
                        || false,
                    )
                    .unwrap()
                    .unwrap()
                    .path;
                let expected: Vec<[f32; 3]> =
                    serde_json::from_value(case["shaped"]["points"].clone()).unwrap();
                let tangents: Vec<[f32; 3]> =
                    serde_json::from_value(case["shaped"]["tangents"].clone()).unwrap();
                assert_eq!(path.points().len(), expected.len());
                let max_error = path
                    .points()
                    .iter()
                    .zip(expected)
                    .flat_map(|(a, b)| (0..3).map(move |i| (a[i] - b[i]).abs()))
                    .fold(0_f32, f32::max);
                assert!(
                    max_error <= 0.001,
                    "{}: point error {max_error}",
                    input["Name"]
                );
                for (a, b) in path.tangents().iter().zip(tangents) {
                    for i in 0..3 {
                        assert!((a[i] - b[i]).abs() <= 1e-5);
                    }
                }
                assert!(
                    (path.length() - case["shaped"]["length"].as_f64().unwrap() as f32).abs()
                        <= 0.01
                );
                assert_eq!(
                    path.is_circular(),
                    case["shaped"]["circular"].as_bool().unwrap()
                );
                eprintln!(
                    "{}: exact raw cells/costs, shaped point error {max_error}",
                    input["Name"]
                );
            } else {
                let Outcome::Unresolved(audit) = result else {
                    panic!("Expected unresolved control case");
                };
                assert_eq!(audit.queries, case["queries"].as_u64().unwrap() as usize);
                assert_eq!(
                    audit.waypoint_moves,
                    case["waypoint_moves"].as_u64().unwrap() as usize
                );
            }
            assert_eq!(grid.grid.costs, before);
        }
    }
    #[test]
    fn interrupted_query_is_charged_and_never_returns_a_partial_ring() {
        let grid = fixture(1000.);
        let before = grid.grid.costs.clone();
        let polls = std::cell::Cell::new(0);
        let result = assemble(&grid, policy(), &joins(), search(), 128_000_000, || {
            polls.set(polls.get() + 1);
            polls.get() > 100
        })
        .unwrap();
        let Outcome::Cancelled(audit) = result else {
            panic!("Query was not cancelled");
        };
        assert!(audit.queries > 0 && audit.work > grid.grid.costs.len());
        assert!(audit.work <= 128_000_000);
        assert_eq!(grid.grid.costs, before);
    }
}
