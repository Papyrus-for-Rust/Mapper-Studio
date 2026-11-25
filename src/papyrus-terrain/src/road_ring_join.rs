//! Independently routed legs with source-configured neighboring bridge/trim rules.
//! Original bounded routing; no official SeedRandom or PathFinder parity.
use crate::{road_ring, routing};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
pub const VERSION: &str = "papyrus-road-ring-joined-3";
pub const REUSE_VERSION: &str = "papyrus-road-ring-reuse-4";
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub river_water: Option<crate::road_water::Policy>,
    pub engine: String,
    pub source_grid_spacing: f32,
    pub bridge_radius_cells: usize,
    pub trim_radius_cells: usize,
    pub directional_penalty: u32,
    pub directional_radius_cells: usize,
    pub fit_heights_in_search: bool,
    pub maximum_work: usize,
    pub maximum_points: usize,
}
impl Policy {
    pub fn version(&self) -> &'static str {
        if self.engine == REUSE_VERSION {
            REUSE_VERSION
        } else {
            VERSION
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        if let Some(water) = &self.river_water {
            water.validate()?;
            if self.engine != REUSE_VERSION {
                return Err("Road river clearance requires the reuse search policy".into());
            }
        }
        if ![VERSION, REUSE_VERSION].contains(&self.engine.as_str())
            || !self.source_grid_spacing.is_finite()
            || !(0.1..=128.).contains(&self.source_grid_spacing)
            || !(1..=64).contains(&self.bridge_radius_cells)
            || self.trim_radius_cells > self.bridge_radius_cells
            || self.directional_penalty > 1_000_000
            || !(1..=4).contains(&self.directional_radius_cells)
            || !(1..=32_000_000).contains(&self.maximum_work)
            || !(5..=65536).contains(&self.maximum_points)
        {
            return Err("Invalid neighboring road join policy".into());
        }
        Ok(())
    }
}
#[derive(Debug, Default, Serialize)]
pub struct Report {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_conflict: Option<Conflict>,
    #[serde(skip_serializing_if = "is_zero")]
    pub reused_routes: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub cached_path_points: usize,
    pub work: usize,
    pub bridge_attempts: usize,
    pub bridges: usize,
    pub trimmed_controls: usize,
    pub candidate_rounds: usize,
    pub rejected_self_intersections: usize,
    pub official_path_parity: bool,
    pub source_resolution: usize,
    pub bridge_radius_routing_cells: usize,
    pub trim_radius_routing_cells: usize,
    pub height_allowance: f32,
}
#[derive(Debug, Serialize)]
pub struct Conflict {
    pub segments: [usize; 2],
    pub legs: [usize; 2],
    pub endpoints: [[f32; 3]; 4],
}
fn is_zero(v: &usize) -> bool {
    *v == 0
}

/// Private to one immutable grid/cost/settings invocation. Both successful and
/// failed queries are deterministic; seed is part of the key. Only real searches
/// consume the route-attempt budget. Cached copies still consume join work.
#[derive(Default)]
struct RouteMemory {
    entries: BTreeMap<([usize; 2], [usize; 2], u32), routing::Outcome>,
    points: usize,
}
struct Router<'a> {
    grid: routing::Grid<'a>,
    costs: &'a [u32],
    settings: routing::Settings,
    height_allowance: f32,
    memory: RouteMemory,
}
impl Router<'_> {
    fn route(
        &mut self,
        endpoints: [[usize; 2]; 2],
        seed: u32,
        audit: &mut road_ring::Audit,
        report: &mut Report,
        policy: &Policy,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Option<routing::Outcome>, String> {
        if cancelled() {
            return Ok(Some(routing::Outcome::Cancelled));
        }
        let [start, end] = endpoints;
        let reuse = policy.engine == REUSE_VERSION;
        let key = (start, end, seed);
        if reuse && let Some(found) = self.memory.entries.get(&key) {
            let cost = match found {
                routing::Outcome::Found(path) => path.len().max(1),
                _ => 1,
            };
            for _ in 0..cost {
                if !visit(report, policy, cancelled)? {
                    return Ok(Some(routing::Outcome::Cancelled));
                }
            }
            report.reused_routes += 1;
            return Ok(Some(found.clone()));
        }
        if audit.route_attempts >= audit.policy.maximum_attempts {
            return Ok(None);
        }
        audit.route_attempts += 1;
        let result = routing::route_with_height_allowance(
            routing::Grid { ..self.grid },
            start,
            end,
            routing::Settings {
                seed,
                ..self.settings
            },
            Some(self.costs),
            self.height_allowance,
            cancelled,
        )?;
        if reuse && !matches!(result, routing::Outcome::Cancelled) {
            let points = match &result {
                routing::Outcome::Found(path) => path.len(),
                _ => 0,
            };
            // Aggregate point cap, not one independent allowance per route.
            if points <= policy.maximum_points - self.memory.points {
                for _ in 0..points.max(1) {
                    if !visit(report, policy, cancelled)? {
                        return Ok(Some(routing::Outcome::Cancelled));
                    }
                }
                self.memory.points += points;
                report.cached_path_points = self.memory.points;
                self.memory.entries.insert(key, result.clone());
            }
        }
        Ok(Some(result))
    }
}
fn visit(
    report: &mut Report,
    policy: &Policy,
    cancelled: &impl Fn() -> bool,
) -> Result<bool, String> {
    if cancelled() {
        return Ok(false);
    }
    if report.work >= policy.maximum_work {
        return Err("Neighboring road join work allowance reached".into());
    }
    report.work += 1;
    Ok(true)
}
#[derive(Clone, Copy)]
struct PathNode {
    cell: [usize; 2],
    cost: u64,
}
/// FindPath searches from the destination, so returned costs decrease toward
/// the end. Bridge splices preserve their separately based costs.
fn price(
    path: &[[f32; 3]],
    grid: &routing::Grid<'_>,
    costs: &[u32],
    spacing: f32,
    policy: &Policy,
) -> Vec<PathNode> {
    let mut nodes: Vec<_> = path
        .iter()
        .map(|p| PathNode {
            cell: [
                ((p[0] + grid.size * 0.5) / spacing).round() as usize,
                ((p[2] + grid.size * 0.5) / spacing).round() as usize,
            ],
            cost: 0,
        })
        .collect();
    let mut sum = 0;
    for i in (0..nodes.len()).rev() {
        let cell = nodes[i].cell;
        sum += u64::from(costs[cell[1] * grid.resolution + cell[0]]);
        if i + 2 < nodes.len()
            && cell[0]
                .abs_diff(nodes[i + 2].cell[0])
                .max(cell[1].abs_diff(nodes[i + 2].cell[1]))
                <= policy.directional_radius_cells
        {
            sum += u64::from(policy.directional_penalty);
        }
        nodes[i].cost = sum;
    }
    nodes
}
enum Near {
    Cancelled,
    Complete(Option<(usize, usize)>),
}
fn nearby(
    a: &[PathNode],
    b: &[PathNode],
    radius: usize,
    report: &mut Report,
    policy: &Policy,
    cancelled: &impl Fn() -> bool,
) -> Result<Near, String> {
    let mut cells = BTreeMap::new();
    for (i, node) in b.iter().enumerate() {
        if !visit(report, policy, cancelled)? {
            return Ok(Near::Cancelled);
        }
        let entry = cells.entry(node.cell).or_insert(i);
        if node.cost < b[*entry].cost {
            *entry = i;
        }
    }
    let mut found = None;
    for (i, node) in a.iter().enumerate() {
        let [x, z] = node.cell;
        for sz in z.saturating_sub(radius)..=z.saturating_add(radius) {
            for sx in x.saturating_sub(radius)..=x.saturating_add(radius) {
                if !visit(report, policy, cancelled)? {
                    return Ok(Near::Cancelled);
                }
                if let Some(&j) = cells.get(&[sx, sz]) {
                    let (mut ai, mut bj, mut first_a) = found.unwrap_or((i, j, i));
                    if node.cost > a[ai].cost {
                        ai = i;
                    }
                    if (b[j].cost, i, j) < (b[bj].cost, first_a, bj) {
                        bj = j;
                        first_a = i;
                    }
                    found = Some((ai, bj, first_a));
                }
            }
        }
    }
    Ok(Near::Complete(found.map(|(a, b, _)| (a, b))))
}
pub fn propose(
    grid: routing::Grid<'_>,
    policy: road_ring::Policy,
    joins: &Policy,
    settings: routing::Settings,
    costs: &[u32],
    height_allowance: f32,
    cancelled: impl Fn() -> bool,
) -> Result<Option<road_ring::Proposal>, String> {
    let external_cancel = cancelled;
    let stopped = std::cell::Cell::new(false);
    let cancelled = || {
        if stopped.get() {
            true
        } else if external_cancel() {
            stopped.set(true);
            true
        } else {
            false
        }
    };
    policy.validate()?;
    joins.validate()?;
    let n = grid.resolution;
    if n < policy.inset_divisor * 2 || n > 257 {
        return Err("Invalid joined ring grid".into());
    }
    if matches!(
        routing::route_with_height_allowance(
            routing::Grid { ..grid },
            [0, 0],
            [0, 0],
            settings,
            Some(costs),
            height_allowance,
            cancelled
        )?,
        routing::Outcome::Cancelled
    ) {
        return Ok(None);
    }
    let q = n / policy.inset_divisor;
    let (lo, hi, mid) = (q, n - q, n / 2);
    let mut nodes = if grid.size >= policy.large_world_size as f32 {
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
    let mut remaining = vec![q / policy.inward_step_cells; nodes.len()];
    let mut audit = road_ring::Audit {
        engine: joins.version(),
        status: "unresolved",
        reason: None,
        policy,
        route_attempts: 0,
        waypoint_moves: 0,
        waypoints: vec![],
        smoothed: false,
        shaping_fallback: None,
        grade_fit_work: 0,
        fitted_height_samples: 0,
        joins: None,
    };
    let mut report = Report {
        height_allowance,
        ..Report::default()
    };
    let walkable = |p: [usize; 2]| {
        !grid.blocked[p[1] * n + p[0]] && grid.heights[p[1] * n + p[0]] >= settings.minimum_height
    };
    let shift = |index: usize,
                 nodes: &mut Vec<([usize; 2], [isize; 2])>,
                 remaining: &mut Vec<usize>,
                 audit: &mut road_ring::Audit|
     -> bool {
        if remaining[index] == 0 {
            return false;
        }
        remaining[index] -= 1;
        let (p, d) = &mut nodes[index];
        for axis in 0..2 {
            let Some(v) = p[axis]
                .checked_add_signed(d[axis] * policy.inward_step_cells as isize)
                .filter(|v| *v < n)
            else {
                return false;
            };
            p[axis] = v;
        }
        audit.waypoint_moves += 1;
        true
    };
    let spacing = grid.size / (n - 1) as f32;
    let source_resolution = (grid.size / joins.source_grid_spacing) as usize;
    if !(2..=1024).contains(&source_resolution) {
        return Err("Source road join grid exceeds supported resolution".into());
    }
    let source_spacing = grid.size / source_resolution as f32;
    let bridge_radius =
        (joins.bridge_radius_cells as f32 * source_spacing / spacing).floor() as usize;
    let trim_radius = (joins.trim_radius_cells as f32 * source_spacing / spacing).floor() as usize;
    report.source_resolution = source_resolution;
    report.bridge_radius_routing_cells = bridge_radius;
    report.trim_radius_routing_cells = trim_radius;
    if bridge_radius > 64 || trim_radius > 1 {
        return Err("Road join radius exceeds supported routing cells".into());
    }
    for index in 0..nodes.len() {
        while !walkable(nodes[index].0) {
            if cancelled() {
                return Ok(None);
            }
            if !shift(index, &mut nodes, &mut remaining, &mut audit) {
                audit.reason = Some("No walkable joined ring waypoint");
                audit.joins = Some(report);
                return Ok(Some(road_ring::Proposal {
                    audit,
                    points: vec![],
                }));
            }
        }
    }
    let mut router = Router {
        grid: routing::Grid { ..grid },
        costs,
        settings,
        height_allowance,
        memory: RouteMemory::default(),
    };
    let mut accepted = None;
    'round: loop {
        if !visit(&mut report, joins, &cancelled)? {
            return Ok(None);
        }
        report.candidate_rounds += 1;
        audit.waypoints = nodes.iter().map(|v| v.0).collect();
        if audit
            .waypoints
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != nodes.len()
        {
            audit.reason = Some("Joined ring waypoints collapsed");
            break;
        }
        let mut legs = Vec::new();
        let mut failed = None;
        let mut affected = None;
        for i in 0..nodes.len() {
            let Some(outcome) = router.route(
                [nodes[i].0, nodes[(i + 1) % nodes.len()].0],
                settings.seed.wrapping_add(i as u32),
                &mut audit,
                &mut report,
                joins,
                &cancelled,
            )?
            else {
                audit.status = "budget_exceeded";
                audit.reason = Some("Joined ring routing attempt allowance reached");
                break 'round;
            };
            match outcome {
                routing::Outcome::Cancelled => return Ok(None),
                routing::Outcome::Found(path) => {
                    let cells = price(&path, &grid, costs, spacing, joins);
                    if cells.len() > joins.maximum_points {
                        return Err("Joined road path point allowance reached".into());
                    }
                    legs.push(cells);
                }
                _ => {
                    failed = Some(i);
                    break;
                }
            }
        }
        if failed.is_none() {
            for i in 0..legs.len() {
                let j = (i + 1) % legs.len();
                let candidate = match nearby(
                    &legs[i],
                    &legs[j],
                    bridge_radius,
                    &mut report,
                    joins,
                    &cancelled,
                )? {
                    Near::Cancelled => return Ok(None),
                    Near::Complete(v) => v,
                };
                if cancelled() {
                    return Ok(None);
                }
                if let Some((a, b)) = candidate {
                    let attempts = audit.route_attempts;
                    let Some(outcome) = router.route(
                        [legs[i][a].cell, legs[j][b].cell],
                        settings.seed,
                        &mut audit,
                        &mut report,
                        joins,
                        &cancelled,
                    )?
                    else {
                        audit.status = "budget_exceeded";
                        audit.reason = Some("Joined ring bridge attempt allowance reached");
                        break 'round;
                    };
                    report.bridge_attempts += audit.route_attempts - attempts;
                    match outcome {
                        routing::Outcome::Cancelled => return Ok(None),
                        routing::Outcome::Found(path) => {
                            let bridge: Vec<_> = price(&path, &grid, costs, spacing, joins)
                                .into_iter()
                                .skip(1)
                                .collect();
                            if a + 1 + bridge.len() > joins.maximum_points {
                                return Err("Joined road bridge point allowance reached".into());
                            }
                            report.trimmed_controls += legs[i].len() - a - 1 + b;
                            legs[i].truncate(a + 1);
                            legs[i].extend(bridge);
                            legs[j].drain(..b);
                            report.bridges += 1;
                        }
                        _ => {}
                    }
                }
            }
            for i in 0..legs.len() {
                let j = (i + 1) % legs.len();
                let candidate = match nearby(
                    &legs[i],
                    &legs[j],
                    trim_radius,
                    &mut report,
                    joins,
                    &cancelled,
                )? {
                    Near::Cancelled => return Ok(None),
                    Near::Complete(v) => v,
                };
                if cancelled() {
                    return Ok(None);
                }
                if let Some((a, b)) = candidate {
                    report.trimmed_controls += legs[i].len() - a - 1 + b;
                    legs[i].truncate(a + 1);
                    legs[j].drain(..b);
                }
            }
            let mut cells = Vec::new();
            let mut owners = Vec::new();
            for (owner, leg) in legs.iter().enumerate() {
                let skip = usize::from(cells.last() == leg.first().map(|v| &v.cell));
                if cells.len() + leg.len() - skip > joins.maximum_points {
                    return Err("Joined ring control allowance reached".into());
                }
                cells.extend(leg.iter().skip(skip).map(|v| v.cell));
                owners.extend(std::iter::repeat_n(owner, leg.len() - skip));
            }
            if cells.first() != cells.last() {
                if cells.len() >= joins.maximum_points {
                    return Err("Joined ring closing control allowance reached".into());
                }
                cells.push(cells[0]);
                owners.push(legs.len() - 1);
            }
            let points: Vec<_> = cells
                .iter()
                .map(|&[x, z]| {
                    [
                        x as f32 * spacing - grid.size * 0.5,
                        grid.heights[z * n + x],
                        z as f32 * spacing - grid.size * 0.5,
                    ]
                })
                .collect();
            // Source trims link the selected controls directly; a valid join
            // need not remain a single raster step. Audit the resulting line
            // geometry here; the caller still validates the full terrain bed.
            let conflict = if joins.engine == REUSE_VERSION {
                road_ring::polyline_conflict(&points, grid.size, &cancelled)?
            } else if road_ring::simple_polyline(&points, grid.size, &cancelled)? {
                None
            } else {
                Some([0, 0])
            };
            if let Some([a, b]) = conflict {
                if cancelled() {
                    return Ok(None);
                }
                if joins.engine == REUSE_VERSION && a != b && points.len() > a.max(b) + 1 {
                    let pair = [owners[a + 1], owners[b + 1]];
                    affected = Some(pair);
                    if report.first_conflict.is_none() {
                        report.first_conflict = Some(Conflict {
                            segments: [a, b],
                            legs: pair,
                            endpoints: [points[a], points[a + 1], points[b], points[b + 1]],
                        });
                    }
                }
                report.rejected_self_intersections += 1;
                failed = Some(0);
            } else {
                accepted = Some(points);
                audit.status = "found";
                break;
            }
        }
        let leg = failed.unwrap_or(0);
        let center = nodes.iter().fold([0.; 2], |mut c, n| {
            c[0] += n.0[0] as f64 / nodes.len() as f64;
            c[1] += n.0[1] as f64 / nodes.len() as f64;
            c
        });
        let movers = if let Some([a, b]) = affected {
            vec![a, (a + 1) % nodes.len(), b, (b + 1) % nodes.len()]
        } else {
            vec![leg, (leg + 1) % nodes.len()]
        };
        let moving = movers
            .into_iter()
            .filter(|i| remaining[*i] > 0)
            .max_by(|a, b| {
                let d = |i: usize| {
                    (nodes[i].0[0] as f64 - center[0]).powi(2)
                        + (nodes[i].0[1] as f64 - center[1]).powi(2)
                };
                d(*a).total_cmp(&d(*b)).then_with(|| b.cmp(a))
            })
            .unwrap_or(leg);
        loop {
            if cancelled() {
                return Ok(None);
            }
            if !shift(moving, &mut nodes, &mut remaining, &mut audit) {
                audit.reason = Some("Joined ring waypoint movement exhausted");
                break 'round;
            }
            if walkable(nodes[moving].0) {
                break;
            }
        }
    }
    if cancelled() {
        return Ok(None);
    }
    audit.joins = Some(report);
    let points = accepted.unwrap_or_default();
    Ok(Some(road_ring::Proposal { audit, points }))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn joins() -> Policy {
        Policy {
            river_water: None,
            engine: VERSION.into(),
            source_grid_spacing: 7.5,
            bridge_radius_cells: 15,
            trim_radius_cells: 1,
            directional_penalty: 10000,
            directional_radius_cells: 1,
            fit_heights_in_search: true,
            maximum_work: 8_000_000,
            maximum_points: 65536,
        }
    }
    #[test]
    fn reuse_is_seed_bound_private_bounded_and_cancel_safe() {
        let n = 17;
        let heights = vec![20.; n * n];
        let mut blocked = vec![false; n * n];
        blocked[8 * n..9 * n].fill(true);
        let costs = vec![101; n * n];
        let mut p = joins();
        p.engine = REUSE_VERSION.into();
        let settings = routing::Settings {
            seed: 17,
            maximum_grade: 0.2,
            slope_penalty: 4.,
            minimum_height: 1.,
            maximum_expansions: n * n,
        };
        let mut router = Router {
            grid: routing::Grid {
                resolution: n,
                size: 1000.,
                heights: &heights,
                blocked: &blocked,
            },
            costs: &costs,
            settings,
            height_allowance: 4.,
            memory: RouteMemory::default(),
        };
        let mut audit = road_ring::Audit {
            engine: REUSE_VERSION,
            status: "unresolved",
            reason: None,
            policy: road_ring::Policy {
                minimum_world_size: 1000,
                large_world_size: 5000,
                inset_divisor: 4,
                inward_step_cells: 1,
                maximum_attempts: 4,
                width: 12.,
                hierarchy: 0,
                network_sample_stride: 8,
            },
            route_attempts: 0,
            waypoint_moves: 0,
            waypoints: vec![],
            smoothed: false,
            shaping_fallback: None,
            grade_fit_work: 0,
            fitted_height_samples: 0,
            joins: None,
        };
        let mut report = Report::default();
        let ends = [[1, 1], [6, 6]];
        let first = router
            .route(ends, 17, &mut audit, &mut report, &p, &|| false)
            .unwrap()
            .unwrap();
        assert!(matches!(first, routing::Outcome::Found(_)));
        let copy = router
            .route(ends, 17, &mut audit, &mut report, &p, &|| false)
            .unwrap()
            .unwrap();
        assert_eq!(first, copy);
        assert_eq!(audit.route_attempts, 1);
        assert_eq!(report.reused_routes, 1);
        assert!(
            router
                .route(ends, 18, &mut audit, &mut report, &p, &|| false)
                .unwrap()
                .is_some()
        );
        assert_eq!(audit.route_attempts, 2);
        let blocked_ends = [[1, 1], [6, 12]];
        let miss = router
            .route(blocked_ends, 17, &mut audit, &mut report, &p, &|| false)
            .unwrap()
            .unwrap();
        assert_eq!(miss, routing::Outcome::Unreachable);
        assert_eq!(
            router
                .route(blocked_ends, 17, &mut audit, &mut report, &p, &|| false)
                .unwrap(),
            Some(miss)
        );
        assert_eq!(audit.route_attempts, 3);
        assert_eq!(
            router
                .route(ends, 17, &mut audit, &mut report, &p, &|| true)
                .unwrap(),
            Some(routing::Outcome::Cancelled)
        );
        assert_eq!(audit.route_attempts, 3);
        router
            .route(ends, 19, &mut audit, &mut report, &p, &|| false)
            .unwrap();
        assert_eq!(audit.route_attempts, 4);
        assert!(
            router
                .route(ends, 20, &mut audit, &mut report, &p, &|| false)
                .unwrap()
                .is_none()
        );
        // Warm routes remain available after the real search allowance is spent.
        assert_eq!(
            router
                .route(ends, 17, &mut audit, &mut report, &p, &|| false)
                .unwrap(),
            Some(first)
        );
        assert_eq!(audit.route_attempts, 4);
        assert!(report.cached_path_points <= p.maximum_points);
        let mut tiny = p.clone();
        tiny.maximum_work = 1;
        assert!(
            router
                .route(ends, 17, &mut audit, &mut Report::default(), &tiny, &|| {
                    false
                })
                .is_err()
        );
        assert!(heights.iter().all(|&v| v == 20.));
    }

    #[test]
    fn installed_server_node_costs_and_directional_prices_match_numeric_fixture() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/path-node-costs-bd19c4ac.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let rows = case["input"]["Rows"].as_array().unwrap();
            let n = rows.len();
            let costs: Vec<_> = rows
                .iter()
                .flat_map(|r| r.as_array().unwrap())
                .map(|v| v.as_u64().unwrap() as u32)
                .collect();
            let size = (n - 1) as f32;
            let heights = vec![20.; n * n];
            let blocked = vec![false; n * n];
            let grid = routing::Grid {
                resolution: n,
                size,
                heights: &heights,
                blocked: &blocked,
            };
            let path: Vec<_> = case["cells"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| {
                    [
                        p[0].as_u64().unwrap() as f32 - size * 0.5,
                        20.,
                        p[1].as_u64().unwrap() as f32 - size * 0.5,
                    ]
                })
                .collect();
            let nodes = price(&path, &grid, &costs, 1., &joins());
            let expected: Vec<_> = case["costs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap())
                .collect();
            assert_eq!(
                nodes.iter().map(|v| v.cost).collect::<Vec<_>>(),
                expected,
                "{}",
                case["input"]["Name"]
            );
        }
    }
    #[test]
    fn indexed_nearby_matches_source_predicate_and_extreme_cost_indices() {
        let p = joins();
        for case in 0..20 {
            let a: Vec<_> = (0..40)
                .map(|i| PathNode {
                    cell: [(i * 7 + case) % 31, (i * 13 + case * 3) % 31],
                    cost: if case % 2 == 0 {
                        1000 - i as u64 * 7
                    } else {
                        100 + (i as u64 * 17) % 41
                    },
                })
                .collect();
            let b: Vec<_> = (0..45)
                .map(|i| PathNode {
                    cell: [(i * 11 + case * 5) % 31, (i * 3 + case) % 31],
                    cost: if case % 2 == 0 {
                        1000 - i as u64 * 7
                    } else {
                        100 + (i as u64 * 13) % 37
                    },
                })
                .collect();
            for radius in [0, 1, 5, 15] {
                let mut expected = None;
                for (i, x) in a.iter().enumerate() {
                    for (j, y) in b.iter().enumerate() {
                        if x.cell[0].abs_diff(y.cell[0]) <= radius
                            && x.cell[1].abs_diff(y.cell[1]) <= radius
                        {
                            let (mut ai, mut bj) = expected.unwrap_or((i, j));
                            if x.cost > a[ai].cost {
                                ai = i;
                            }
                            if y.cost < b[bj].cost {
                                bj = j;
                            }
                            expected = Some((ai, bj));
                        }
                    }
                }
                let Near::Complete(actual) =
                    nearby(&a, &b, radius, &mut Report::default(), &p, &|| false).unwrap()
                else {
                    panic!("Cancelled");
                };
                assert_eq!(actual, expected);
            }
        }
        let calls = std::cell::Cell::new(0);
        let cancel = || {
            calls.set(calls.get() + 1);
            calls.get() == 17
        };
        assert!(matches!(
            nearby(
                &[PathNode {
                    cell: [1, 1],
                    cost: 100
                }; 40],
                &[PathNode {
                    cell: [2, 2],
                    cost: 100
                }; 40],
                1,
                &mut Report::default(),
                &p,
                &cancel
            )
            .unwrap(),
            Near::Cancelled
        ));
    }
    #[test]
    fn joined_legs_form_replayable_simple_ring_and_fail_atomically() {
        let n = 33;
        let heights = vec![20.; n * n];
        let blocked = vec![false; n * n];
        let costs = vec![101; n * n];
        let grid = routing::Grid {
            resolution: n,
            size: 1000.,
            heights: &heights,
            blocked: &blocked,
        };
        let policy = road_ring::Policy {
            minimum_world_size: 1000,
            large_world_size: 5000,
            inset_divisor: 4,
            inward_step_cells: 1,
            maximum_attempts: 128,
            width: 12.,
            hierarchy: 0,
            network_sample_stride: 8,
        };
        let settings = routing::Settings {
            seed: 17,
            maximum_grade: 0.2,
            slope_penalty: 4.,
            minimum_height: 1.,
            maximum_expansions: n * n,
        };
        let p = joins();
        let run = |p: &Policy| {
            propose(
                routing::Grid { ..grid },
                policy,
                p,
                settings,
                &costs,
                4.,
                || false,
            )
        };
        let a = run(&p).unwrap().unwrap();
        assert_eq!(a.audit.status, "found");
        assert!(road_ring::simple_polyline(&a.points, 1000., &|| false).unwrap());
        assert_eq!(a.points.first(), a.points.last());
        assert!(a.audit.joins.as_ref().unwrap().bridges > 0);
        let b = run(&p).unwrap().unwrap();
        assert_eq!(a.points, b.points);
        assert_eq!(
            serde_json::to_value(&a.audit).unwrap(),
            serde_json::to_value(&b.audit).unwrap()
        );
        let mut tiny = p.clone();
        tiny.maximum_work = 1;
        assert!(run(&tiny).is_err());
        let calls = std::cell::Cell::new(0);
        assert!(
            propose(
                routing::Grid { ..grid },
                policy,
                &p,
                settings,
                &costs,
                4.,
                || {
                    calls.set(calls.get() + 1);
                    calls.get() == 31
                }
            )
            .unwrap()
            .is_none()
        );
        assert!(heights.iter().all(|v| *v == 20.));
        assert!(blocked.iter().all(|v| !*v));
    }
}
