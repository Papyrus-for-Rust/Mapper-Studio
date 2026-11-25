//! Bounded independent closed-loop search using source-configured ring waypoints.
//! Does not reproduce the game's weighted costmap or linked-list join trimming.
use crate::routing::{self, Grid};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
pub const VERSION: &str = "papyrus-main-road-ring-1";
pub const RECOVERY_VERSION: &str = "papyrus-main-road-ring-recovery-2";

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub minimum_world_size: u32,
    pub large_world_size: u32,
    pub inset_divisor: usize,
    pub inward_step_cells: usize,
    pub maximum_attempts: usize,
    pub width: f32,
    pub hierarchy: u32,
    pub network_sample_stride: usize,
}
impl Policy {
    pub fn validate(self) -> Result<(), String> {
        if !(1000..=6000).contains(&self.minimum_world_size)
            || !(self.minimum_world_size..=6000).contains(&self.large_world_size)
            || !(3..=16).contains(&self.inset_divisor)
            || !(1..=8).contains(&self.inward_step_cells)
            || !(4..=128).contains(&self.maximum_attempts)
            || !self.width.is_finite()
            || !(1. ..=32.).contains(&self.width)
            || self.hierarchy != 0
            || !(1..=1024).contains(&self.network_sample_stride)
        {
            return Err("Unsupported main road ring policy".into());
        }
        Ok(())
    }
}
#[derive(Debug, Serialize)]
pub struct Audit {
    pub engine: &'static str,
    pub status: &'static str,
    pub reason: Option<&'static str>,
    pub policy: Policy,
    pub route_attempts: usize,
    pub waypoint_moves: usize,
    pub waypoints: Vec<[usize; 2]>,
    pub smoothed: bool,
    pub shaping_fallback: Option<&'static str>,
    pub grade_fit_work: usize,
    pub fitted_height_samples: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub joins: Option<crate::road_ring_join::Report>,
}
pub struct Proposal {
    pub audit: Audit,
    pub points: Vec<[f32; 3]>,
}
struct Node {
    position: [usize; 2],
    direction: [isize; 2],
    remaining: usize,
}
fn shift(node: &mut Node, policy: Policy, n: usize) -> bool {
    if node.remaining == 0 {
        return false;
    }
    node.remaining -= 1;
    for axis in 0..2 {
        let Some(v) = node.position[axis]
            .checked_add_signed(node.direction[axis] * policy.inward_step_cells as isize)
            .filter(|v| *v < n)
        else {
            return false;
        };
        node.position[axis] = v;
    }
    true
}
pub(crate) fn simple(points: &[[usize; 2]]) -> bool {
    if points.len() < 5 || points.first() != points.last() {
        return false;
    }
    let mut vertices = BTreeSet::new();
    let mut diagonals = std::collections::BTreeMap::new();
    for pair in points.windows(2) {
        let [a, b] = [pair[0], pair[1]];
        if !vertices.insert(a) || a[0].abs_diff(b[0]).max(a[1].abs_diff(b[1])) != 1 {
            return false;
        }
        if a[0] != b[0] && a[1] != b[1] {
            let sign = (a[0] < b[0]) == (a[1] < b[1]);
            if diagonals
                .insert([a[0].min(b[0]), a[1].min(b[1])], sign)
                .is_some_and(|old| old != sign)
            {
                return false;
            }
        }
    }
    true
}
/// Spatially bucketed straight-segment checks after smoothing. Neighbouring
/// segments share an endpoint; every other touch/crossing invalidates the loop.
pub(crate) fn simple_polyline(
    points: &[[f32; 3]],
    size: f32,
    cancelled: &impl Fn() -> bool,
) -> Result<bool, String> {
    polyline_conflict(points, size, cancelled).map(|conflict| conflict.is_none())
}
pub(crate) fn polyline_conflict(
    points: &[[f32; 3]],
    size: f32,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<[usize; 2]>, String> {
    if points.len() < 5
        || points.len() > 65536
        || points.first() != points.last()
        || !size.is_finite()
        || size <= 0.
        || points.iter().flatten().any(|v| !v.is_finite())
    {
        return Ok(Some([0, 0]));
    }
    let mut buckets = std::collections::BTreeMap::<[i32; 2], Vec<usize>>::new();
    let mut work = 0;
    let cross = |a: [f32; 3], b: [f32; 3], c: [f32; 3]| {
        (b[0] as f64 - a[0] as f64) * (c[2] as f64 - a[2] as f64)
            - (b[2] as f64 - a[2] as f64) * (c[0] as f64 - a[0] as f64)
    };
    for (i, segment) in points.windows(2).enumerate() {
        if cancelled() {
            return Ok(Some([0, 0]));
        }
        let [a, b] = [segment[0], segment[1]];
        let cell = |v: f32| ((v / size + 0.5) * 256.).floor() as i32;
        if [a[0], a[2], b[0], b[2]]
            .iter()
            .any(|v| v.abs() > size * 0.5)
        {
            return Ok(Some([0, 0]));
        }
        let mut checked = BTreeSet::new();
        for z in cell(a[2].min(b[2]))..=cell(a[2].max(b[2])) {
            for x in cell(a[0].min(b[0]))..=cell(a[0].max(b[0])) {
                work += 1;
                if work > 32_000_000 {
                    return Err("Ring intersection work allowance reached".into());
                }
                let items = buckets.entry([x, z]).or_default();
                for &j in items.iter() {
                    work += 1;
                    if work > 32_000_000 {
                        return Err("Ring intersection work allowance reached".into());
                    }
                    if j + 1 == i || (j == 0 && i == points.len() - 2) || !checked.insert(j) {
                        continue;
                    }
                    let (c, d) = (points[j], points[j + 1]);
                    if a[0].min(b[0]) > c[0].max(d[0])
                        || c[0].min(d[0]) > a[0].max(b[0])
                        || a[2].min(b[2]) > c[2].max(d[2])
                        || c[2].min(d[2]) > a[2].max(b[2])
                    {
                        continue;
                    }
                    if cross(a, b, c) * cross(a, b, d) <= 0.
                        && cross(c, d, a) * cross(c, d, b) <= 0.
                    {
                        return Ok(Some([j, i]));
                    }
                }
                items.push(i);
            }
        }
    }
    Ok(None)
}
pub fn propose(
    grid: Grid<'_>,
    policy: Policy,
    settings: routing::Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Proposal>, String> {
    propose_inner(grid, policy, settings, false, None, cancelled)
}
pub fn propose_recovering(
    grid: Grid<'_>,
    policy: Policy,
    settings: routing::Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Proposal>, String> {
    propose_inner(grid, policy, settings, true, None, cancelled)
}
pub fn propose_recovering_with_costs(
    grid: Grid<'_>,
    policy: Policy,
    settings: routing::Settings,
    costs: &[u32],
    cancelled: impl Fn() -> bool,
) -> Result<Option<Proposal>, String> {
    propose_inner(grid, policy, settings, true, Some(costs), cancelled)
}
fn propose_inner(
    grid: Grid<'_>,
    policy: Policy,
    settings: routing::Settings,
    recovering: bool,
    costs: Option<&[u32]>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Proposal>, String> {
    policy.validate()?;
    if grid.resolution < policy.inset_divisor * 2 || grid.resolution > 257 {
        return Err("Invalid ring grid resolution".into());
    }
    // The normal router owns complete input validation, including empty searches.
    if matches!(
        routing::route_with_costs(Grid { ..grid }, [0, 0], [0, 0], settings, costs, &cancelled)?,
        routing::Outcome::Cancelled
    ) {
        return Ok(None);
    }
    let n = grid.resolution;
    let q = n / policy.inset_divisor;
    let (lo, hi, mid) = (q, n - q, n / 2);
    let controls = if grid.size >= policy.large_world_size as f32 {
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
    let mut nodes: Vec<_> = controls
        .into_iter()
        .map(|(position, direction)| Node {
            position,
            direction,
            remaining: q / policy.inward_step_cells,
        })
        .collect();
    let mut audit = Audit {
        engine: if recovering {
            RECOVERY_VERSION
        } else {
            VERSION
        },
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
    let walkable = |p: [usize; 2]| {
        !grid.blocked[p[1] * n + p[0]] && grid.heights[p[1] * n + p[0]] >= settings.minimum_height
    };
    for node in &mut nodes {
        while !walkable(node.position) {
            if cancelled() {
                return Ok(None);
            }
            if !shift(node, policy, n) {
                audit.reason = Some("No walkable ring waypoint");
                return Ok(Some(Proposal {
                    audit,
                    points: vec![],
                }));
            }
            audit.waypoint_moves += 1;
        }
    }
    let mut origin = 0;
    loop {
        if cancelled() {
            return Ok(None);
        }
        audit.waypoints = nodes.iter().map(|n| n.position).collect();
        if audit.waypoints.iter().collect::<BTreeSet<_>>().len() != nodes.len() {
            audit.reason = Some("Ring waypoints collapsed");
            break;
        }
        let mut occupied = grid.blocked.to_vec();
        let mut points = Vec::new();
        let mut cells = Vec::new();
        let mut failed = None;
        for ordinal in 0..nodes.len() {
            let leg = (origin + ordinal) % nodes.len();
            if audit.route_attempts >= policy.maximum_attempts {
                audit.status = "budget_exceeded";
                audit.reason = Some("Ring routing attempt allowance reached");
                return Ok(Some(Proposal {
                    audit,
                    points: vec![],
                }));
            }
            let (start, end) = (nodes[leg].position, nodes[(leg + 1) % nodes.len()].position);
            let mut overlay = occupied.clone();
            for node in &nodes {
                if node.position != start && node.position != end {
                    overlay[node.position[1] * n + node.position[0]] = true;
                }
            }
            for p in [start, end] {
                overlay[p[1] * n + p[0]] = grid.blocked[p[1] * n + p[0]];
            }
            audit.route_attempts += 1;
            match routing::route_with_costs(
                Grid {
                    blocked: &overlay,
                    ..grid
                },
                start,
                end,
                routing::Settings {
                    seed: settings.seed.wrapping_add(leg as u32),
                    ..settings
                },
                costs,
                &cancelled,
            )? {
                routing::Outcome::Cancelled => return Ok(None),
                routing::Outcome::Found(path) => {
                    let spacing = grid.size / (n - 1) as f32;
                    let path_cells: Vec<_> = path
                        .iter()
                        .map(|p| {
                            [
                                ((p[0] + grid.size * 0.5) / spacing).round() as usize,
                                ((p[2] + grid.size * 0.5) / spacing).round() as usize,
                            ]
                        })
                        .collect();
                    for p in &path_cells {
                        occupied[p[1] * n + p[0]] = true;
                    }
                    for pair in path_cells.windows(2) {
                        let [a, b] = [pair[0], pair[1]];
                        if a[0] != b[0] && a[1] != b[1] {
                            occupied[a[1] * n + b[0]] = true;
                            occupied[b[1] * n + a[0]] = true;
                        }
                    }
                    points.extend(path.into_iter().skip(usize::from(ordinal > 0)));
                    cells.extend(path_cells.into_iter().skip(usize::from(ordinal > 0)));
                }
                _ => {
                    failed = Some(leg);
                    break;
                }
            }
        }
        if failed.is_none() && simple(&cells) {
            audit.status = "found";
            return Ok(Some(Proposal { audit, points }));
        }
        let leg = failed.unwrap_or(0);
        let moving = if recovering {
            // Try both ends of the failed edge. A fixed origin can remain in a
            // disconnected corridor while its start is repeatedly pushed inward.
            let center = nodes.iter().fold([0_f64; 2], |mut c, node| {
                c[0] += node.position[0] as f64 / nodes.len() as f64;
                c[1] += node.position[1] as f64 / nodes.len() as f64;
                c
            });
            [leg, (leg + 1) % nodes.len()]
                .into_iter()
                .filter(|&i| nodes[i].remaining > 0)
                .max_by(|&a, &b| {
                    let distance = |i: usize| {
                        (nodes[i].position[0] as f64 - center[0]).powi(2)
                            + (nodes[i].position[1] as f64 - center[1]).powi(2)
                    };
                    distance(a).total_cmp(&distance(b)).then_with(|| b.cmp(&a))
                })
                .unwrap_or(leg)
        } else {
            leg
        };
        let node = &mut nodes[moving];
        loop {
            if cancelled() {
                return Ok(None);
            }
            if !shift(node, policy, n) {
                audit.reason = Some("Ring waypoint movement exhausted");
                return Ok(Some(Proposal {
                    audit,
                    points: vec![],
                }));
            }
            audit.waypoint_moves += 1;
            if walkable(node.position) {
                break;
            }
        }
        if recovering {
            // The occupied-route overlay is order dependent. Rotate its priority
            // so an early interior shortcut cannot permanently trap a later leg.
            origin = (origin + 1) % nodes.len();
        }
    }
    Ok(Some(Proposal {
        audit,
        points: vec![],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn conflict_identifies_actual_crossing_segments_and_preserves_legacy_validation() {
        let square = [
            [-10., 0., -10.],
            [10., 0., -10.],
            [10., 0., 10.],
            [-10., 0., 10.],
            [-10., 0., -10.],
        ];
        assert_eq!(polyline_conflict(&square, 100., &|| false).unwrap(), None);
        assert!(simple_polyline(&square, 100., &|| false).unwrap());
        let crossed = [
            [-10., 0., -10.],
            [10., 0., 10.],
            [-10., 0., 10.],
            [10., 0., -10.],
            [-10., 0., -10.],
        ];
        assert_eq!(
            polyline_conflict(&crossed, 100., &|| false).unwrap(),
            Some([0, 2])
        );
        assert!(!simple_polyline(&crossed, 100., &|| false).unwrap());
        assert!(!simple_polyline(&square, 100., &|| true).unwrap());
        assert!(!simple_polyline(&square[..3], 100., &|| false).unwrap());
    }

    #[test]
    fn recovering_search_escapes_a_legacy_endpoint_trap() {
        let n = 33;
        let heights = vec![20.; n * n];
        let policy = Policy {
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
            seed: 123,
            maximum_grade: 0.2,
            slope_penalty: 4.,
            minimum_height: 1.,
            maximum_expansions: n * n,
        };
        // Two corridors around disconnected blocks trap the legacy failed-leg
        // origin. The alternative must still form a simple, unobstructed cycle.
        let mut blocked = vec![false; n * n];
        for (x, z, w, h) in [
            (8, 19, 9, 4),
            (20, 23, 5, 8),
            (8, 11, 9, 4),
            (20, 7, 5, 8),
            (24, 3, 9, 4),
            (12, 15, 5, 8),
        ] {
            for j in z..(z + h).min(n) {
                for i in x..(x + w).min(n) {
                    blocked[j * n + i] = true;
                }
            }
        }
        let grid = Grid {
            resolution: n,
            size: 1000.,
            heights: &heights,
            blocked: &blocked,
        };
        let old = propose(Grid { ..grid }, policy, settings, || false)
            .unwrap()
            .unwrap();
        let new = propose_recovering(Grid { ..grid }, policy, settings, || false)
            .unwrap()
            .unwrap();
        assert!(old.points.is_empty());
        assert_eq!(new.audit.status, "found");
        assert_eq!(new.audit.engine, RECOVERY_VERSION);
        assert!(new.audit.route_attempts <= policy.maximum_attempts);
        assert!(simple_polyline(&new.points, 1000., &|| false).unwrap());
        for point in &new.points {
            let x = ((point[0] / 1000. + 0.5) * (n - 1) as f32).round() as usize;
            let z = ((point[2] / 1000. + 0.5) * (n - 1) as f32).round() as usize;
            assert!(!blocked[z * n + x]);
        }
        let replay = propose_recovering(Grid { ..grid }, policy, settings, || false)
            .unwrap()
            .unwrap();
        assert_eq!(new.points, replay.points);
        assert!(
            propose_recovering(Grid { ..grid }, policy, settings, || true)
                .unwrap()
                .is_none()
        );
        let bounded = propose_recovering(
            grid,
            Policy {
                maximum_attempts: 4,
                ..policy
            },
            settings,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(bounded.audit.status, "budget_exceeded");
        assert!(bounded.points.is_empty());
    }
    #[test]
    fn refined_loop_rejects_self_crossings_and_nonadjacent_touches() {
        let good = [
            [0., 0., 0.],
            [10., 0., 0.],
            [10., 0., 10.],
            [0., 0., 10.],
            [0., 0., 0.],
        ];
        assert!(simple_polyline(&good, 1000., &|| false).unwrap());
        let bow = [
            [0., 0., 0.],
            [10., 0., 10.],
            [10., 0., 0.],
            [0., 0., 10.],
            [0., 0., 0.],
        ];
        assert!(!simple_polyline(&bow, 1000., &|| false).unwrap());
        assert!(!simple_polyline(&good, 1000., &|| true).unwrap());
    }
    #[test]
    fn ring_is_closed_simple_repeatable_and_budgeted_without_touching_inputs() {
        let n = 33;
        let heights = vec![20.; n * n];
        let blocked = vec![false; n * n];
        let settings = routing::Settings {
            seed: 123,
            maximum_grade: 0.2,
            slope_penalty: 4.,
            minimum_height: 1.,
            maximum_expansions: n * n,
        };
        let mut policy = Policy {
            minimum_world_size: 1000,
            large_world_size: 5000,
            inset_divisor: 4,
            inward_step_cells: 1,
            maximum_attempts: 128,
            width: 12.,
            hierarchy: 0,
            network_sample_stride: 8,
        };
        for count in [4, 8] {
            if count == 8 {
                policy.large_world_size = 1000;
            }
            let run = || {
                propose(
                    Grid {
                        resolution: n,
                        size: 1000.,
                        heights: &heights,
                        blocked: &blocked,
                    },
                    policy,
                    settings,
                    || false,
                )
                .unwrap()
                .unwrap()
            };
            let a = run();
            let b = run();
            assert_eq!(a.points, b.points);
            assert_eq!(a.audit.status, "found");
            assert_eq!(a.audit.waypoints.len(), count);
            assert_eq!(a.points.first(), a.points.last());
            assert!(a.points.len() > 40);
        }
        policy.maximum_attempts = 4;
        let a = propose(
            Grid {
                resolution: n,
                size: 1000.,
                heights: &heights,
                blocked: &blocked,
            },
            policy,
            settings,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(a.audit.status, "budget_exceeded");
        assert!(a.points.is_empty());
        assert!(
            propose(
                Grid {
                    resolution: n,
                    size: 1000.,
                    heights: &heights,
                    blocked: &blocked
                },
                policy,
                settings,
                || true
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(heights, vec![20.; n * n]);
        assert!(!blocked.iter().any(|b| *b));
    }
}
