//! Source-configured ring proposals. Discrete simple loops only; no track beds,
//! smoothed spline clearance, physical assembly or train acceptance.
use crate::rail_costmap::{self, Grid, Route, RouteSettings};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
pub const VERSION: &str = "papyrus-rail-ring-candidates-1";
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub engine: String,
    pub inset_divisor: usize,
    pub large_world_size: u32,
    pub inward_step_cells: usize,
    pub connector_exclusion_cells: usize,
    pub maximum_segment_expansions: usize,
    pub minimum_height: f32,
}
impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        if self.engine != VERSION
            || !(3..=16).contains(&self.inset_divisor)
            || !(1000..=6000).contains(&self.large_world_size)
            || !(1..=8).contains(&self.inward_step_cells)
            || !(1..=64).contains(&self.connector_exclusion_cells)
            || !(1..=1_000_000).contains(&self.maximum_segment_expansions)
            || !self.minimum_height.is_finite()
            || !(-500. ..=500.).contains(&self.minimum_height)
        {
            return Err("Unsupported rail ring source policy".into());
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub seed: u32,
    pub maximum_grade: f32,
    pub maximum_total_expansions: usize,
    pub maximum_attempts: usize,
}
impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if !self.maximum_grade.is_finite()
            || !(0. ..=1.).contains(&self.maximum_grade)
            || !(1..=8_000_000).contains(&self.maximum_total_expansions)
            || !(1..=256).contains(&self.maximum_attempts)
        {
            return Err("Invalid rail ring search limits".into());
        }
        Ok(())
    }
}
#[derive(Debug, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Found {
        waypoints: Vec<[usize; 2]>,
        cells: Vec<[usize; 2]>,
        points: Vec<[f32; 3]>,
        route_attempts: usize,
        expansions: usize,
        waypoint_moves: usize,
    },
    Unresolved {
        reason: &'static str,
        route_attempts: usize,
        expansions: usize,
        waypoint_moves: usize,
    },
    BudgetExceeded {
        route_attempts: usize,
        expansions: usize,
        waypoint_moves: usize,
    },
    Cancelled,
}
struct Node {
    cell: [usize; 2],
    direction: [isize; 2],
    remaining: usize,
}
fn nodes(n: usize, size: f32, p: &Policy) -> Vec<Node> {
    let q = n / p.inset_divisor;
    let (lo, hi, mid) = (q, n - q, n / 2);
    let controls = if size >= p.large_world_size as f32 {
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
    controls
        .into_iter()
        .map(|(cell, direction)| Node {
            cell,
            direction,
            remaining: q / p.inward_step_cells,
        })
        .collect()
}
fn move_inward(node: &mut Node, p: &Policy, n: usize) -> bool {
    if node.remaining == 0 {
        return false;
    }
    node.remaining -= 1;
    for axis in 0..2 {
        let Some(next) = node.cell[axis]
            .checked_add_signed(node.direction[axis] * p.inward_step_cells as isize)
            .filter(|&v| v < n)
        else {
            return false;
        };
        node.cell[axis] = next;
    }
    true
}
fn walkable(grid: &Grid, cell: [usize; 2], blocked: &[bool]) -> bool {
    let n = grid.resolution();
    let [x, z] = cell;
    x > 0
        && z > 0
        && x + 1 < n
        && z + 1 < n
        && (z - 1..=z + 1)
            .all(|z| (x - 1..=x + 1).all(|x| grid.costs()[z * n + x] > 0 && !blocked[z * n + x]))
}
fn simple(cells: &[[usize; 2]]) -> bool {
    if cells.len() < 5 || cells.first() != cells.last() {
        return false;
    }
    let mut vertices = BTreeSet::new();
    let mut diagonals = std::collections::BTreeMap::new();
    for pair in cells.windows(2) {
        let [a, b] = [pair[0], pair[1]];
        if !vertices.insert(a) {
            return false;
        }
        let (dx, dz) = (a[0].abs_diff(b[0]), a[1].abs_diff(b[1]));
        if dx.max(dz) != 1 {
            return false;
        }
        if dx == 1 && dz == 1 {
            let key = [a[0].min(b[0]), a[1].min(b[1])];
            let sign = (a[0] < b[0]) == (a[1] < b[1]);
            if diagonals.insert(key, sign).is_some_and(|old| old != sign) {
                return false;
            }
        }
    }
    true
}
/// Four/eight source-style waypoints move inward on failed legs. The independent
/// router reserves prior legs and future waypoints to avoid collapsed loops.
/// No partial loop is published, even when one segment exhausts its allowance.
pub fn build(
    grid: &Grid,
    p: &Policy,
    s: Settings,
    connectors: &[[f32; 2]],
    cancelled: impl Fn() -> bool,
) -> Result<Outcome, String> {
    p.validate()?;
    s.validate()?;
    if connectors.len() > 256 {
        return Err("Invalid rail ring search limits".into());
    }
    if cancelled() {
        return Ok(Outcome::Cancelled);
    }
    let n = grid.resolution();
    let mut base = vec![false; n * n];
    for &position in connectors {
        if cancelled() {
            return Ok(Outcome::Cancelled);
        }
        let [cx, cz] = grid.cell(position)?;
        let r = p.connector_exclusion_cells - 1;
        for z in cz.saturating_sub(r)..=cz.saturating_add(r).min(n - 1) {
            for x in cx.saturating_sub(r)..=cx.saturating_add(r).min(n - 1) {
                base[z * n + x] = true;
            }
        }
    }
    let mut nodes = nodes(n, grid.size(), p);
    let (mut attempts, mut expansions, mut moves) = (0, 0, 0);
    for node in &mut nodes {
        while !walkable(grid, node.cell, &base) {
            if cancelled() {
                return Ok(Outcome::Cancelled);
            }
            if !move_inward(node, p, n) {
                return Ok(Outcome::Unresolved {
                    reason: "No walkable source ring waypoint",
                    route_attempts: attempts,
                    expansions,
                    waypoint_moves: moves,
                });
            }
            moves += 1;
        }
    }
    loop {
        if cancelled() {
            return Ok(Outcome::Cancelled);
        }
        let distinct = nodes.iter().map(|n| n.cell).collect::<BTreeSet<_>>().len();
        if distinct != nodes.len() {
            return Ok(Outcome::Unresolved {
                reason: "Inward ring waypoints collapsed",
                route_attempts: attempts,
                expansions,
                waypoint_moves: moves,
            });
        }
        let mut occupied = base.clone();
        let mut cells = Vec::new();
        let mut failed = None;
        for leg in 0..nodes.len() {
            if attempts >= s.maximum_attempts || expansions >= s.maximum_total_expansions {
                return Ok(Outcome::BudgetExceeded {
                    route_attempts: attempts,
                    expansions,
                    waypoint_moves: moves,
                });
            }
            let start = nodes[leg].cell;
            let end = nodes[(leg + 1) % nodes.len()].cell;
            let mut overlay = occupied.clone();
            // Preserve future junctions; a leg cannot consume another waypoint.
            for node in &nodes {
                if node.cell != start && node.cell != end {
                    overlay[node.cell[1] * n + node.cell[0]] = true;
                }
            }
            for c in [start, end] {
                overlay[c[1] * n + c[0]] = base[c[1] * n + c[0]];
            }
            attempts += 1;
            let remaining = s.maximum_total_expansions - expansions;
            let settings = RouteSettings {
                seed: s.seed.wrapping_add(leg as u32),
                maximum_grade: s.maximum_grade,
                maximum_expansions: p.maximum_segment_expansions.min(remaining),
            };
            match rail_costmap::route_excluding(
                grid,
                start,
                end,
                settings,
                Some(&overlay),
                &cancelled,
            )? {
                Route::Cancelled => return Ok(Outcome::Cancelled),
                Route::BudgetExceeded { expansions: used } => {
                    expansions += used;
                    if expansions >= s.maximum_total_expansions {
                        return Ok(Outcome::BudgetExceeded {
                            route_attempts: attempts,
                            expansions,
                            waypoint_moves: moves,
                        });
                    }
                    failed = Some(leg);
                    break;
                }
                Route::Unreachable { expansions: used } => {
                    expansions += used;
                    failed = Some(leg);
                    break;
                }
                Route::Found {
                    cells: path,
                    expansions: used,
                    ..
                } => {
                    expansions += used;
                    for c in &path {
                        occupied[c[1] * n + c[0]] = true;
                    }
                    for pair in path.windows(2) {
                        let [a, b] = [pair[0], pair[1]];
                        if a[0] != b[0] && a[1] != b[1] {
                            // Prevent later opposite diagonals from crossing
                            // this segment without sharing a vertex.
                            occupied[a[1] * n + b[0]] = true;
                            occupied[b[1] * n + a[0]] = true;
                        }
                    }
                    cells.extend(path.into_iter().skip(usize::from(leg > 0)));
                }
            }
        }
        if failed.is_none() && simple(&cells) {
            let points = cells
                .iter()
                .map(|&c| {
                    let mut point = grid.point(c).unwrap();
                    point[1] = point[1].max(p.minimum_height);
                    point
                })
                .collect();
            return Ok(if cancelled() {
                Outcome::Cancelled
            } else {
                Outcome::Found {
                    waypoints: nodes.iter().map(|n| n.cell).collect(),
                    cells,
                    points,
                    route_attempts: attempts,
                    expansions,
                    waypoint_moves: moves,
                }
            });
        }
        let leg = failed.unwrap_or(0);
        let node = &mut nodes[leg];
        loop {
            if cancelled() {
                return Ok(Outcome::Cancelled);
            }
            if !move_inward(node, p, n) {
                return Ok(Outcome::Unresolved {
                    reason: "Ring waypoint movement exhausted after failed search",
                    route_attempts: attempts,
                    expansions,
                    waypoint_moves: moves,
                });
            }
            moves += 1;
            if walkable(grid, node.cell, &base) {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    fn policy() -> Policy {
        Policy {
            engine: VERSION.into(),
            inset_divisor: 4,
            large_world_size: 5000,
            inward_step_cells: 1,
            connector_exclusion_cells: 2,
            maximum_segment_expansions: 10000,
            minimum_height: 1.,
        }
    }
    fn settings() -> Settings {
        Settings {
            seed: 42,
            maximum_grade: 0.15,
            maximum_total_expansions: 100000,
            maximum_attempts: 64,
        }
    }
    fn grid(blocked: bool) -> Grid {
        let h = vec![crate::erosion::encode(50.); 33 * 33];
        let topology = vec![i32::from(blocked); 32 * 32];
        let placement = vec![0; 32 * 32];
        rail_costmap::build(
            rail_costmap::Terrain {
                size: 1000.,
                height_resolution: 33,
                height: &h,
                topology_resolution: 32,
                topology: &topology,
                placement_resolution: 32,
                placement: &placement,
            },
            &rail_costmap::Policy {
                spacing: 25.,
                near_radius: 5.,
                clearance_radius: 25.,
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
            },
            128000000,
            || false,
        )
        .unwrap()
        .unwrap()
    }
    #[test]
    fn four_and_eight_waypoint_loops_are_closed_simple_deterministic_and_read_only() {
        let g = grid(false);
        let before = g.costs().to_vec();
        let mut p = policy();
        for count in [4, 8] {
            if count == 8 {
                p.large_world_size = 1000;
            }
            let first = build(&g, &p, settings(), &[], || false).unwrap();
            assert_eq!(first, build(&g, &p, settings(), &[], || false).unwrap());
            let Outcome::Found {
                cells,
                points,
                waypoints,
                ..
            } = first
            else {
                panic!("flat ring must exist")
            };
            assert_eq!(waypoints.len(), count);
            assert!(simple(&cells));
            assert_eq!(points.first(), points.last());
            assert_eq!(cells.len(), points.len());
            assert!(cells.len() > 40);
        }
        assert_eq!(before, g.costs());
    }
    #[test]
    fn connector_protection_moves_waypoints_inland_and_excludes_reserved_cells() {
        let g = grid(false);
        let p = policy();
        let n = g.resolution();
        let point = g.point([n - n / 4, n - n / 4]).unwrap();
        let Outcome::Found {
            cells,
            waypoint_moves,
            ..
        } = build(&g, &p, settings(), &[[point[0], point[2]]], || false).unwrap()
        else {
            panic!("protected ring should fit")
        };
        assert!(waypoint_moves > 0);
        let center = g.cell([point[0], point[2]]).unwrap();
        assert!(
            cells
                .iter()
                .all(|c| c[0].abs_diff(center[0]).max(c[1].abs_diff(center[1]))
                    >= p.connector_exclusion_cells)
        );
    }
    #[test]
    fn cancellation_exhaustion_blocked_terrain_and_invalid_inputs_never_publish_legs() {
        let g = grid(false);
        let p = policy();
        assert_eq!(
            build(&g, &p, settings(), &[], || true).unwrap(),
            Outcome::Cancelled
        );
        let calls = Cell::new(0);
        assert_eq!(
            build(&g, &p, settings(), &[], || {
                calls.set(calls.get() + 1);
                calls.get() > 20
            })
            .unwrap(),
            Outcome::Cancelled
        );
        let mut s = settings();
        s.maximum_total_expansions = 1;
        assert!(matches!(
            build(&g, &p, s, &[], || false).unwrap(),
            Outcome::BudgetExceeded { .. }
        ));
        assert!(matches!(
            build(&grid(true), &p, settings(), &[], || false).unwrap(),
            Outcome::Unresolved { .. }
        ));
        assert!(build(&g, &p, settings(), &[[f32::NAN, 0.]], || false).is_err());
        let mut p = p;
        p.inset_divisor = 0;
        assert!(build(&g, &p, settings(), &[], || false).is_err());
    }
    #[test]
    fn loop_validation_rejects_repeated_vertices_and_crossing_diagonals() {
        assert!(!simple(&[[0, 0], [1, 1], [0, 1], [1, 0], [0, 0]]));
        assert!(!simple(&[[0, 0], [1, 0], [2, 0], [1, 0], [0, 0]]));
        assert!(simple(&[[0, 0], [1, 0], [1, 1], [0, 1], [0, 0]]));
    }
    #[test]
    fn segment_search_limits_retry_inland_until_the_shared_attempt_limit() {
        let g = grid(false);
        let mut p = policy();
        p.maximum_segment_expansions = 1;
        let mut s = settings();
        s.maximum_attempts = 4;
        let Outcome::BudgetExceeded {
            route_attempts,
            expansions,
            waypoint_moves,
        } = build(&g, &p, s, &[], || false).unwrap()
        else {
            panic!("expected shared attempt exhaustion")
        };
        assert_eq!(route_attempts, 4);
        assert_eq!(expansions, 4);
        assert_eq!(waypoint_moves, 4);
    }
}
