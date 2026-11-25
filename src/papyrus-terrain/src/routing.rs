//! Deterministic bounded A* routing on a caller-prepared terrain grid.
//! This produces candidate geometry, not finished Rust roads or rail networks.
//! Source-specific water, clearance, terrain filters and no-build areas must be
//! rasterized into `blocked` by the installation adapter before routing.
use std::{cmp::Reverse, collections::BinaryHeap};
pub const VERSION: &str = "papyrus-route-grid-1";

pub struct Grid<'a> {
    pub resolution: usize,
    pub size: f32,
    /// Metres, rows toward +Z. Immutable final monument terrain, not base terrain.
    pub heights: &'a [f32],
    pub blocked: &'a [bool],
}
#[derive(Clone, Copy)]
pub struct Settings {
    pub seed: u32,
    pub maximum_grade: f32,
    pub slope_penalty: f32,
    pub minimum_height: f32,
    pub maximum_expansions: usize,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Found(Vec<[f32; 3]>),
    Unreachable,
    BudgetExceeded,
    Cancelled,
}
/// Connected cells under the same passability, grade and diagonal rules as A*.
/// Zero denotes blocked ground. Positive traversal prices cannot change reachability.
pub fn components(
    grid: Grid<'_>,
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Vec<u32>>, String> {
    if matches!(
        route(Grid { ..grid }, [0, 0], [0, 0], settings, &cancelled)?,
        Outcome::Cancelled
    ) {
        return Ok(None);
    }
    let n = grid.resolution;
    let passable = |i: usize| !grid.blocked[i] && grid.heights[i] >= settings.minimum_height;
    let spacing = grid.size / (n - 1) as f32;
    let mut labels = vec![0; n * n];
    let mut label = 0;
    let mut queue = std::collections::VecDeque::new();
    for start in 0..n * n {
        if cancelled() {
            return Ok(None);
        }
        if labels[start] != 0 || !passable(start) {
            continue;
        }
        label += 1;
        labels[start] = label;
        queue.push_back(start);
        while let Some(i) = queue.pop_front() {
            if cancelled() {
                return Ok(None);
            }
            for dz in -1_isize..=1 {
                for dx in -1_isize..=1 {
                    if dx == 0 && dz == 0 {
                        continue;
                    }
                    let x = (i % n).checked_add_signed(dx).filter(|&v| v < n);
                    let z = (i / n).checked_add_signed(dz).filter(|&v| v < n);
                    let (Some(x), Some(z)) = (x, z) else {
                        continue;
                    };
                    let j = z * n + x;
                    if labels[j] != 0 || !passable(j) {
                        continue;
                    }
                    let diagonal = dx != 0 && dz != 0;
                    if diagonal && (!passable((i / n) * n + x) || !passable(z * n + i % n)) {
                        continue;
                    }
                    let distance = spacing
                        * if diagonal {
                            std::f32::consts::SQRT_2
                        } else {
                            1.
                        };
                    if (grid.heights[j] - grid.heights[i]).abs() / distance > settings.maximum_grade
                    {
                        continue;
                    }
                    labels[j] = label;
                    queue.push_back(j);
                }
            }
        }
    }
    Ok(Some(labels))
}
/// Endpoints are explicitly snapped cells. The caller must retain and validate
/// authored connector-to-grid joins; this function never invents access sockets.
pub fn route(
    grid: Grid<'_>,
    start: [usize; 2],
    end: [usize; 2],
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Outcome, String> {
    route_with_costs(grid, start, end, settings, None, cancelled)
}
/// Positive source-configured traversal costs. Legacy callers omit costs and
/// retain their original arithmetic and tie breaking. Blocked cells stay blocked.
pub fn route_with_costs(
    grid: Grid<'_>,
    start: [usize; 2],
    end: [usize; 2],
    settings: Settings,
    cell_costs: Option<&[u32]>,
    cancelled: impl Fn() -> bool,
) -> Result<Outcome, String> {
    route_with_height_allowance(grid, start, end, settings, cell_costs, 0., cancelled)
}
/// Candidate search through bounded terrain work. A feasible local edge is not
/// proof that all path heights can be fitted together; the caller must validate
/// the complete path/bed before applying any terrain edits.
#[allow(clippy::too_many_arguments)]
pub fn route_with_height_allowance(
    grid: Grid<'_>,
    start: [usize; 2],
    end: [usize; 2],
    settings: Settings,
    cell_costs: Option<&[u32]>,
    height_allowance: f32,
    cancelled: impl Fn() -> bool,
) -> Result<Outcome, String> {
    let n = grid.resolution;
    if !(2..=513).contains(&n)
        || !height_allowance.is_finite()
        || !(0. ..=24.).contains(&height_allowance)
        || !grid.size.is_finite()
        || !(1. ..=6000.).contains(&grid.size)
        || grid.heights.len() != n * n
        || grid.blocked.len() != n * n
        || grid
            .heights
            .iter()
            .any(|h| !h.is_finite() || h.abs() > 10000.)
        || start.into_iter().chain(end).any(|v| v >= n)
        || !settings.maximum_grade.is_finite()
        || !(0. ..=1.).contains(&settings.maximum_grade)
        || !settings.slope_penalty.is_finite()
        || !(0. ..=100.).contains(&settings.slope_penalty)
        || !settings.minimum_height.is_finite()
        || settings.minimum_height.abs() > 10000.
        || settings.maximum_expansions == 0
        || settings.maximum_expansions > n * n
        || cell_costs
            .is_some_and(|c| c.len() != n * n || c.iter().any(|v| !(1..=1_000_000).contains(v)))
    {
        return Err("Invalid route grid, endpoints or search budget".into());
    }
    if cancelled() {
        return Ok(Outcome::Cancelled);
    }
    let passable = |i: usize| !grid.blocked[i] && grid.heights[i] >= settings.minimum_height;
    let start = start[1] * n + start[0];
    let end = end[1] * n + end[0];
    if !passable(start) || !passable(end) {
        return Ok(Outcome::Unreachable);
    }
    let spacing = grid.size / (n - 1) as f32;
    // A conservative Manhattan/diagonal lower bound in fixed millimetre units.
    let cardinal = (spacing as f64 * 1000.).floor().max(1.) as u64;
    let diagonal = (spacing as f64 * 2_f64.sqrt() * 1000.).floor().max(1.) as u64;
    let minimum_cost = cell_costs.map_or(1, |c| u64::from(*c.iter().min().unwrap()));
    let heuristic = |i: usize| {
        let dx = (i % n).abs_diff(end % n) as u64;
        let dz = (i / n).abs_diff(end / n) as u64;
        (diagonal * dx.min(dz) + cardinal * dx.abs_diff(dz)) * minimum_cost
    };
    let tie = |i: usize| {
        let mut v = (i as u64) ^ (u64::from(settings.seed) << 32);
        v = (v ^ (v >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        v = (v ^ (v >> 27)).wrapping_mul(0x94d049bb133111eb);
        v ^ (v >> 31)
    };
    let mut costs = vec![u64::MAX; n * n];
    let mut parents = vec![usize::MAX; n * n];
    let mut closed = vec![false; n * n];
    let mut open = BinaryHeap::new();
    costs[start] = 0;
    open.push(Reverse((heuristic(start), tie(start), 0_u64, start)));
    let mut expansions = 0;
    while let Some(Reverse((_, _, cost, i))) = open.pop() {
        if expansions % 128 == 0 && cancelled() {
            return Ok(Outcome::Cancelled);
        }
        if closed[i] || costs[i] != cost {
            continue;
        }
        if i == end {
            let mut points = Vec::new();
            let mut cell = end;
            loop {
                points.push([
                    (cell % n) as f32 * spacing - grid.size * 0.5,
                    grid.heights[cell],
                    (cell / n) as f32 * spacing - grid.size * 0.5,
                ]);
                if cell == start {
                    break;
                }
                cell = parents[cell];
            }
            points.reverse();
            return Ok(if cancelled() {
                Outcome::Cancelled
            } else {
                Outcome::Found(points)
            });
        }
        if expansions == settings.maximum_expansions {
            return Ok(Outcome::BudgetExceeded);
        }
        expansions += 1;
        closed[i] = true;
        for dz in -1_isize..=1 {
            for dx in -1_isize..=1 {
                if dx == 0 && dz == 0 {
                    continue;
                }
                let x = (i % n).checked_add_signed(dx).filter(|&v| v < n);
                let z = (i / n).checked_add_signed(dz).filter(|&v| v < n);
                let (Some(x), Some(z)) = (x, z) else {
                    continue;
                };
                let j = z * n + x;
                if closed[j] || !passable(j) {
                    continue;
                }
                let diag = dx != 0 && dz != 0;
                // Never cut diagonally between impassable cells.
                if diag && (!passable((i / n) * n + x) || !passable(z * n + i % n)) {
                    continue;
                }
                let distance = spacing * if diag { std::f32::consts::SQRT_2 } else { 1. };
                let grade = (grid.heights[j] - grid.heights[i]).abs() / distance;
                let feasible_grade = if height_allowance == 0. {
                    grade
                } else {
                    ((grid.heights[j] - grid.heights[i]).abs() - 2. * height_allowance).max(0.)
                        / distance
                };
                if feasible_grade > settings.maximum_grade {
                    continue;
                }
                let base = if diag { diagonal } else { cardinal }
                    * cell_costs.map_or(1, |c| u64::from(c[j]));
                let edge = base
                    + (base as f64 * settings.slope_penalty as f64 * (grade.min(1.) as f64).powi(2))
                        .round() as u64;
                let next = cost + edge;
                if next < costs[j] {
                    costs[j] = next;
                    parents[j] = i;
                    open.push(Reverse((next + heuristic(j), tie(j), next, j)));
                }
            }
        }
    }
    Ok(if cancelled() {
        Outcome::Cancelled
    } else {
        Outcome::Unreachable
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_height_search_keeps_final_grade_separate_and_prices_safe() {
        let n = 5;
        let heights: Vec<_> = (0..n * n).map(|i| 20. + (i % n) as f32 * 5.).collect();
        let blocked = vec![false; n * n];
        let grid = Grid {
            resolution: n,
            size: 40.,
            heights: &heights,
            blocked: &blocked,
        };
        let settings = Settings {
            seed: 1,
            maximum_grade: 0.2,
            slope_penalty: 4.,
            minimum_height: 1.,
            maximum_expansions: n * n,
        };
        assert!(matches!(
            route_with_costs(Grid { ..grid }, [0, 2], [4, 2], settings, None, || false).unwrap(),
            Outcome::Unreachable
        ));
        let Outcome::Found(points) = route_with_height_allowance(
            Grid { ..grid },
            [0, 2],
            [4, 2],
            settings,
            None,
            8.,
            || false,
        )
        .unwrap() else {
            panic!("No candidate");
        };
        assert!(
            points
                .windows(2)
                .any(|w| (w[1][1] - w[0][1]).abs() / 10. > settings.maximum_grade)
        );
        assert!(
            route_with_height_allowance(
                Grid { ..grid },
                [0, 2],
                [4, 2],
                settings,
                None,
                f32::NAN,
                || false
            )
            .is_err()
        );
        assert!(matches!(
            route_with_height_allowance(
                Grid { ..grid },
                [0, 2],
                [4, 2],
                Settings {
                    maximum_expansions: 1,
                    ..settings
                },
                None,
                8.,
                || false
            )
            .unwrap(),
            Outcome::BudgetExceeded
        ));
        assert!(matches!(
            route_with_height_allowance(
                Grid { ..grid },
                [0, 2],
                [4, 2],
                settings,
                None,
                8.,
                || true
            )
            .unwrap(),
            Outcome::Cancelled
        ));
        let tiny: Vec<_> = (0..n * n).map(|i| 20. + (i % n) as f32 * 5.).collect();
        let prices = vec![1_000_000; n * n];
        assert!(matches!(
            route_with_height_allowance(
                Grid {
                    size: 1.,
                    heights: &tiny,
                    ..grid
                },
                [0, 2],
                [4, 2],
                Settings {
                    slope_penalty: 100.,
                    ..settings
                },
                Some(&prices),
                24.,
                || false
            )
            .unwrap(),
            Outcome::Found(_)
        ));
        assert_eq!(
            heights,
            (0..n * n)
                .map(|i| 20. + (i % n) as f32 * 5.)
                .collect::<Vec<_>>()
        );
    }
    #[test]
    fn components_match_astar_across_grade_water_and_diagonal_barriers() {
        let n = 5;
        let settings = Settings {
            seed: 1,
            maximum_grade: 0.2,
            slope_penalty: 4.,
            minimum_height: 1.,
            maximum_expansions: n * n,
        };
        let mut heights = vec![20.; n * n];
        let mut blocked = vec![false; n * n];
        for mode in 0..3 {
            if mode == 0 {
                for z in 0..n {
                    heights[z * n + 2] = 120.;
                }
            }
            if mode == 1 {
                heights.fill(20.);
                for z in 0..n {
                    heights[z * n + 2] = 0.;
                }
            }
            if mode == 2 {
                heights.fill(20.);
                for z in 0..n {
                    for x in 0..n {
                        blocked[z * n + x] = (x + z) % 2 == 1;
                    }
                }
            }
            let grid = Grid {
                resolution: n,
                size: 80.,
                heights: &heights,
                blocked: &blocked,
            };
            let labels = components(Grid { ..grid }, settings, || false)
                .unwrap()
                .unwrap();
            for a in 0..n * n {
                for b in 0..n * n {
                    let path = route(
                        Grid { ..grid },
                        [a % n, a / n],
                        [b % n, b / n],
                        settings,
                        || false,
                    )
                    .unwrap();
                    assert_eq!(
                        matches!(path, Outcome::Found(_)),
                        labels[a] != 0 && labels[a] == labels[b]
                    );
                }
            }
            assert!(
                components(Grid { ..grid }, settings, || true)
                    .unwrap()
                    .is_none()
            );
        }
    }
    #[test]
    fn source_costs_avoid_expensive_ground_but_cross_it_when_it_is_the_only_route() {
        let heights = vec![20.; 81];
        let blocked = vec![false; 81];
        let mut costs = vec![101; 81];
        for x in 1..8 {
            costs[4 * 9 + x] = 5000;
        }
        let run = |blocked: &[bool], costs: Option<&[u32]>| {
            route_with_costs(
                Grid {
                    resolution: 9,
                    size: 800.,
                    heights: &heights,
                    blocked,
                },
                [0, 4],
                [8, 4],
                settings(),
                costs,
                || false,
            )
            .unwrap()
        };
        let Outcome::Found(legacy) = run(&blocked, None) else {
            panic!("flat route");
        };
        assert!(legacy.iter().all(|p| p[2] == 0.));
        let Outcome::Found(avoiding) = run(&blocked, Some(&costs)) else {
            panic!("alternative route");
        };
        assert!(avoiding.iter().any(|p| p[2] != 0.));
        let mut corridor = vec![true; 81];
        corridor[4 * 9..5 * 9].fill(false);
        let Outcome::Found(crossing) = run(&corridor, Some(&costs)) else {
            panic!("expensive ground must remain traversable");
        };
        assert_eq!(legacy, crossing);
        assert_eq!(
            run(&blocked, None),
            route(
                Grid {
                    resolution: 9,
                    size: 800.,
                    heights: &heights,
                    blocked: &blocked
                },
                [0, 4],
                [8, 4],
                settings(),
                || false
            )
            .unwrap()
        );
        let mut bad = costs.clone();
        bad[0] = 0;
        assert!(
            route_with_costs(
                Grid {
                    resolution: 9,
                    size: 800.,
                    heights: &heights,
                    blocked: &blocked
                },
                [0, 4],
                [8, 4],
                settings(),
                Some(&bad),
                || false
            )
            .is_err()
        );
        assert!(matches!(
            route_with_costs(
                Grid {
                    resolution: 9,
                    size: 800.,
                    heights: &heights,
                    blocked: &blocked
                },
                [0, 4],
                [8, 4],
                settings(),
                Some(&costs),
                || true
            )
            .unwrap(),
            Outcome::Cancelled
        ));
    }
    fn settings() -> Settings {
        Settings {
            seed: 7,
            maximum_grade: 0.1,
            slope_penalty: 12.,
            minimum_height: 1.,
            maximum_expansions: 81,
        }
    }
    #[test]
    fn routes_around_clearance_and_water_without_mutating_final_terrain() {
        let mut h = vec![20.; 81];
        let mut blocked = vec![false; 81];
        for z in 0..7 {
            blocked[z * 9 + 4] = true;
        }
        h[7 * 9 + 4] = -1.;
        let original = h.clone();
        let solve = || {
            route(
                Grid {
                    resolution: 9,
                    size: 800.,
                    heights: &h,
                    blocked: &blocked,
                },
                [1, 1],
                [7, 1],
                settings(),
                || false,
            )
            .unwrap()
        };
        let result = solve();
        assert_eq!(result, solve());
        let Outcome::Found(points) = result else {
            panic!("detour must exist")
        };
        assert!(points.iter().any(|p| p[2] == 400.));
        assert_eq!(points.first().unwrap(), &[-300., 20., -300.]);
        assert_eq!(points.last().unwrap(), &[300., 20., -300.]);
        assert_eq!(h, original);
    }
    #[test]
    fn rejects_corner_cutting_steep_edges_and_reports_exhaustion_and_cancel() {
        let mut h = vec![20.; 81];
        let mut blocked = vec![false; 81];
        let solve = |h: &[f32], b: &[bool], s, c| {
            route(
                Grid {
                    resolution: 9,
                    size: 800.,
                    heights: h,
                    blocked: b,
                },
                [0, 0],
                [8, 8],
                s,
                move || c,
            )
            .unwrap()
        };
        blocked[1] = true;
        blocked[9] = true;
        assert_eq!(solve(&h, &blocked, settings(), false), Outcome::Unreachable);
        blocked.fill(false);
        h[1] = 100.;
        h[9] = 100.;
        h[10] = 100.;
        assert_eq!(solve(&h, &blocked, settings(), false), Outcome::Unreachable);
        h.fill(20.);
        let mut s = settings();
        s.maximum_expansions = 1;
        assert_eq!(solve(&h, &blocked, s, false), Outcome::BudgetExceeded);
        assert_eq!(solve(&h, &blocked, settings(), true), Outcome::Cancelled);
        assert!(
            route(
                Grid {
                    resolution: 9,
                    size: 800.,
                    heights: &h,
                    blocked: &blocked
                },
                [9, 0],
                [8, 8],
                settings(),
                || false
            )
            .is_err()
        );
        let cancel_count = std::cell::Cell::new(0);
        assert_eq!(
            route(
                Grid {
                    resolution: 9,
                    size: 800.,
                    heights: &h,
                    blocked: &blocked
                },
                [0, 0],
                [8, 8],
                settings(),
                || {
                    let n = cancel_count.get();
                    cancel_count.set(n + 1);
                    n > 0
                }
            )
            .unwrap(),
            Outcome::Cancelled
        );
    }
}
