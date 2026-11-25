//! Source-configured branch joins with Mapper's bounded independent router/RNG.
//! Proposals require subsequent bed, corridor, mesh and runtime validation.
use crate::{
    path_geometry::{self, Path},
    rail_costmap::{self, Grid, Route, RouteSettings},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
pub const VERSION: &str = "papyrus-rail-branch-candidates-1";
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub engine: String,
    pub span_min: usize,
    pub span_max_exclusive: usize,
    pub skip_min: usize,
    pub skip_max_exclusive: usize,
    pub endpoint_margin: usize,
    pub join_points: usize,
    pub tangent_probe: usize,
    pub nearby_track_distance: f32,
    pub connector_exclusion_cells: usize,
    pub maximum_parent_angle: f32,
    pub maximum_join_angle: f32,
    pub lateral_offset: f32,
    pub minimum_height: f32,
    pub maximum_segment_expansions: usize,
    pub horizontal_iterations: u16,
    pub vertical_iterations: u16,
    pub resample_distance: f32,
    pub hierarchy: u32,
    pub start_flag: bool,
    pub end_flag: bool,
}
impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        if self.engine != VERSION
            || !(2..=32).contains(&self.join_points)
            || self.endpoint_margin < self.join_points
            || self.endpoint_margin > 64
            || !(2..=128).contains(&self.tangent_probe)
            || self.span_min <= self.tangent_probe
            || self.span_max_exclusive <= self.span_min
            || self.span_max_exclusive > 512
            || self.skip_min == 0
            || self.skip_max_exclusive <= self.skip_min
            || self.skip_max_exclusive > 512
            || !self.nearby_track_distance.is_finite()
            || !(0.1..=512.).contains(&self.nearby_track_distance)
            || !(1..=64).contains(&self.connector_exclusion_cells)
            || [self.maximum_parent_angle, self.maximum_join_angle]
                .iter()
                .any(|v| !v.is_finite() || !(0.1..=90.).contains(v))
            || !self.lateral_offset.is_finite()
            || !(0. ..=64.).contains(&self.lateral_offset)
            || !self.minimum_height.is_finite()
            || !(-500. ..=500.).contains(&self.minimum_height)
            || !(1..=1_000_000).contains(&self.maximum_segment_expansions)
            || self.horizontal_iterations > 256
            || self.vertical_iterations > 256
            || !self.resample_distance.is_finite()
            || !(0.1..=100.).contains(&self.resample_distance)
            || self.hierarchy != 1
            || self.start_flag
            || self.end_flag
        {
            return Err("Unsupported rail branching source policy".into());
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub seed: u32,
    pub maximum_grade: f32,
    pub maximum_expansions: usize,
    pub maximum_work: usize,
    pub maximum_branches: usize,
}
impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if !self.maximum_grade.is_finite()
            || !(0. ..=1.).contains(&self.maximum_grade)
            || !(1..=8_000_000).contains(&self.maximum_expansions)
            || !(1..=128_000_000).contains(&self.maximum_work)
            || !(1..=256).contains(&self.maximum_branches)
        {
            return Err("Invalid rail branching work limits".into());
        }
        Ok(())
    }
}
pub struct Track<'a> {
    pub id: u64,
    pub path: &'a Path,
}
#[derive(Debug, Serialize)]
pub struct Branch {
    pub parent: u64,
    pub start_control: usize,
    pub end_control: usize,
    pub path: Path,
}
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Proposed {
        branches: Vec<Branch>,
        work: usize,
        expansions: usize,
        rejections: BTreeMap<&'static str, usize>,
    },
    BudgetExceeded {
        work: usize,
        expansions: usize,
    },
    Cancelled,
}
fn delta(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}
fn length(a: [f32; 3]) -> f32 {
    a.iter().map(|v| v * v).sum::<f32>().sqrt()
}
fn unit(a: [f32; 3]) -> [f32; 3] {
    let d = length(a);
    if d > 0.00001 {
        a.map(|v| v / d)
    } else {
        [0.; 3]
    }
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a.iter().zip(b).map(|(a, b)| a * b).sum()
}
fn angle(a: [f32; 3], b: [f32; 3]) -> f32 {
    dot(unit(a), unit(b)).clamp(-1., 1.).acos().to_degrees()
}
fn signed(a: [f32; 3], b: [f32; 3]) -> f32 {
    let s = a[2] * b[0] - a[0] * b[2];
    angle(a, b) * if s >= 0. { 1. } else { -1. }
}
fn smooth(v: f32) -> f32 {
    let t = v.clamp(0., 1.);
    t * t * (3. - 2. * t)
}
fn random(state: &mut u32, min: usize, max: usize) -> usize {
    *state = state.wrapping_add(0x9e3779b9);
    let mut v = *state;
    v = (v ^ (v >> 16)).wrapping_mul(0x85ebca6b);
    v = (v ^ (v >> 13)).wrapping_mul(0xc2b2ae35);
    min + ((v ^ (v >> 16)) as usize % (max - min))
}
/// Router/seed choices are independent of server PathFinder/SeedRandom. Source
/// geometry gates, joins, fade and placement-blocker transitions are retained.
pub fn generate(
    grid: &Grid,
    tracks: &[Track<'_>],
    connectors: &[[f32; 2]],
    p: &Policy,
    s: Settings,
    blocked: impl Fn([f32; 3]) -> bool,
    cancelled: impl Fn() -> bool,
) -> Result<Outcome, String> {
    p.validate()?;
    s.validate()?;
    if cancelled() {
        return Ok(Outcome::Cancelled);
    }
    let mut ids = BTreeSet::new();
    let total: usize = tracks.iter().map(|t| t.path.points().len()).sum();
    if tracks.is_empty()
        || tracks.len() > 257
        || total > 65536
        || connectors.len() > 256
        || tracks.iter().any(|t| {
            t.id == 0
                || !ids.insert(t.id)
                || t.path
                    .points()
                    .iter()
                    .any(|v| grid.cell([v[0], v[2]]).is_err())
        })
    {
        return Err("Invalid rail branching parent roster".into());
    }
    let mut work = 0;
    let mut expansions = 0;
    macro_rules! visit {
        () => {
            if cancelled() {
                return Ok(Outcome::Cancelled);
            }
            if work >= s.maximum_work {
                return Ok(Outcome::BudgetExceeded { work, expansions });
            }
            work += 1;
        };
    }
    let n = grid.resolution();
    let mut overlay = vec![false; n * n];
    for &connector in connectors {
        let [cx, cz] = grid.cell(connector)?;
        let r = p.connector_exclusion_cells - 1;
        for z in cz.saturating_sub(r)..=cz.saturating_add(r).min(n - 1) {
            for x in cx.saturating_sub(r)..=cx.saturating_add(r).min(n - 1) {
                visit!();
                overlay[z * n + x] = true;
            }
        }
    }
    let mut nearby = Vec::new();
    for track in tracks {
        let mut flags = vec![false; track.path.points().len()];
        for (i, &point) in track.path.points().iter().enumerate() {
            'other: for other in tracks.iter().filter(|t| t.id != track.id) {
                for &v in other.path.points() {
                    visit!();
                    if length(delta(point, v)) < p.nearby_track_distance {
                        flags[i] = true;
                        break 'other;
                    }
                }
            }
        }
        nearby.push(flags);
    }
    let mut state = s.seed;
    let mut branches = Vec::new();
    let mut rejections = BTreeMap::new();
    macro_rules! reject {
        ($reason:expr) => {
            *rejections.entry($reason).or_default() += 1;
        };
    }
    for (track, nearby) in tracks.iter().zip(&nearby) {
        let points = track.path.points();
        let tangents = track.path.tangents();
        let Some(last) = points.len().checked_sub(p.endpoint_margin + 1) else {
            continue;
        };
        let mut i = p.endpoint_margin;
        while i <= last {
            visit!();
            let start = i;
            i += 1;
            let span = random(&mut state, p.span_min, p.span_max_exclusive);
            let skip = random(&mut state, p.skip_min, p.skip_max_exclusive);
            let end = start + span;
            if end >= last {
                reject!("short_parent");
                continue;
            }
            if angle(tangents[start], tangents[end]) > p.maximum_parent_angle {
                reject!("parent_angle");
                continue;
            }
            if nearby[start] || nearby[end] {
                reject!("near_other_track");
                continue;
            }
            i += skip;
            let remaining = s.maximum_expansions.saturating_sub(expansions);
            if remaining == 0 {
                return Ok(Outcome::BudgetExceeded { work, expansions });
            }
            let route = rail_costmap::route_excluding(
                grid,
                grid.cell([points[start][0], points[start][2]])?,
                grid.cell([points[end][0], points[end][2]])?,
                RouteSettings {
                    seed: state,
                    maximum_grade: s.maximum_grade,
                    maximum_expansions: p.maximum_segment_expansions.min(remaining),
                },
                Some(&overlay),
                &cancelled,
            )?;
            let mut route = match route {
                Route::Found {
                    points,
                    expansions: spent,
                    ..
                } => {
                    expansions += spent;
                    points
                }
                Route::Unreachable { expansions: spent } => {
                    expansions += spent;
                    reject!("unreachable");
                    continue;
                }
                Route::BudgetExceeded { expansions: spent } => {
                    expansions += spent;
                    return Ok(Outcome::BudgetExceeded { work, expansions });
                }
                Route::Cancelled => return Ok(Outcome::Cancelled),
            };
            if route.len() < 4 {
                reject!("short_route");
                continue;
            }
            route.remove(0);
            route.pop();
            for point in &mut route {
                point[1] = point[1].max(p.minimum_height);
            }
            let start_control = start + 1 - p.join_points;
            let end_control = end + p.join_points - 1;
            let mut controls = points[start_control..=start].to_vec();
            controls.extend(route);
            controls.extend_from_slice(&points[end..=end_control]);
            let head = p.join_points;
            let tail = controls.len() - 1 - p.join_points;
            if head + p.tangent_probe >= controls.len() || tail < p.tangent_probe {
                reject!("short_join");
                continue;
            }
            let a = unit(delta(points[start + p.tangent_probe], points[start]));
            let b = unit(delta(points[end], points[end - p.tangent_probe]));
            let aa = signed(
                a,
                unit(delta(controls[head + p.tangent_probe], controls[head])),
            );
            let bb = -signed(
                b,
                unit(delta(controls[tail], controls[tail - p.tangent_probe])),
            );
            if (aa >= 0.) != (bb >= 0.)
                || aa.abs() > p.maximum_join_angle
                || bb.abs() > p.maximum_join_angle
            {
                reject!("join_angle");
                continue;
            }
            let side = |v: [f32; 3], angle: f32| {
                let rotated = [v[2], v[1], -v[0]];
                rotated.map(|v| v * if angle < 0. { -1. } else { 1. })
            };
            let (a, b) = (side(a, aa), side(b, bb));
            for j in 0..p.join_points * 2 {
                visit!();
                let offset = smooth(j as f32 / p.join_points as f32) * p.lateral_offset;
                let k = controls.len() - 1 - j;
                for axis in 0..3 {
                    controls[j][axis] += a[axis] * offset;
                    controls[k][axis] += b[axis] * offset;
                }
            }
            let mut left = false;
            let mut entered = false;
            let mut crosses = false;
            for &point in &controls {
                visit!();
                let b = blocked(point);
                if !left && !entered && !b {
                    left = true;
                }
                if left && !entered && b {
                    entered = true;
                }
                if left && entered && !b {
                    crosses = true;
                    break;
                }
            }
            if crosses {
                reject!("placement_crossing");
                continue;
            }
            let weights = (0..controls.len())
                .map(|j| smooth((j.min(controls.len() - 1 - j)) as f32 / p.join_points as f32))
                .collect::<Vec<_>>();
            let budget = s.maximum_work.saturating_sub(work).min(32000000);
            if budget == 0 {
                return Ok(Outcome::BudgetExceeded { work, expansions });
            }
            let curve = path_geometry::build(
                &controls,
                path_geometry::Settings {
                    horizontal_iterations: p.horizontal_iterations,
                    vertical_iterations: p.vertical_iterations,
                    resample_distance: p.resample_distance,
                    maximum_work: budget,
                },
                Some(&weights),
                &cancelled,
            );
            let Some(curve) = (match curve {
                Ok(p) => p,
                Err(path_geometry::Failure::Budget) => {
                    return Ok(Outcome::BudgetExceeded {
                        work: s.maximum_work,
                        expansions,
                    });
                }
                Err(e) => return Err(format!("Branch curve failed: {e:?}")),
            }) else {
                return Ok(Outcome::Cancelled);
            };
            work += curve.work();
            // Mapper preserves exact join controls after source-style resampling.
            let mut fitted = curve.points().to_vec();
            fitted[0] = points[start_control];
            let last = fitted.len() - 1;
            fitted[last] = points[end_control];
            let budget = s.maximum_work.saturating_sub(work).min(32000000);
            if budget == 0 {
                return Ok(Outcome::BudgetExceeded { work, expansions });
            }
            let path = match Path::from_controls(&fitted, budget, &cancelled) {
                Ok(Some(v)) => v,
                Ok(None) => return Ok(Outcome::Cancelled),
                Err(path_geometry::Failure::Budget) => {
                    return Ok(Outcome::BudgetExceeded {
                        work: s.maximum_work,
                        expansions,
                    });
                }
                Err(e) => return Err(format!("Branch join failed: {e:?}")),
            };
            work += path.work();
            if path
                .points()
                .iter()
                .any(|v| grid.cell([v[0], v[2]]).is_err())
            {
                reject!("outside_world");
                continue;
            }
            if branches.len() >= s.maximum_branches {
                return Ok(Outcome::BudgetExceeded { work, expansions });
            }
            branches.push(Branch {
                parent: track.id,
                start_control,
                end_control,
                path,
            });
            i += span;
        }
    }
    if cancelled() {
        Ok(Outcome::Cancelled)
    } else {
        Ok(Outcome::Proposed {
            branches,
            work,
            expansions,
            rejections,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy() -> Policy {
        Policy {
            engine: VERSION.into(),
            span_min: 12,
            span_max_exclusive: 20,
            skip_min: 4,
            skip_max_exclusive: 8,
            endpoint_margin: 5,
            join_points: 4,
            tangent_probe: 4,
            nearby_track_distance: 10.,
            connector_exclusion_cells: 2,
            maximum_parent_angle: 30.,
            maximum_join_angle: 60.,
            lateral_offset: 8.,
            minimum_height: 1.,
            maximum_segment_expansions: 250000,
            horizontal_iterations: 2,
            vertical_iterations: 2,
            resample_distance: 4.,
            hierarchy: 1,
            start_flag: false,
            end_flag: false,
        }
    }
    fn settings() -> Settings {
        Settings {
            seed: 17,
            maximum_grade: 0.15,
            maximum_expansions: 1000000,
            maximum_work: 8000000,
            maximum_branches: 64,
        }
    }
    fn grid() -> Grid {
        let p = rail_costmap::Policy {
            spacing: 20.,
            near_radius: 1.,
            clearance_radius: 1.,
            blocked_topology: 1,
            clearance_topology: 2,
            water_topology: 4,
            cliff_topology: 8,
            maximum_slope: 40.,
            steep_slope: 25.,
            moderate_slope: 15.,
            normal_cost: 1,
            moderate_cost: 2,
            steep_cost: 3,
        };
        rail_costmap::build(
            rail_costmap::Terrain {
                size: 1000.,
                height_resolution: 65,
                height: &vec![19660; 65 * 65],
                topology_resolution: 8,
                topology: &vec![0; 64],
                placement_resolution: 8,
                placement: &[0; 64],
            },
            &p,
            8000000,
            || false,
        )
        .unwrap()
        .unwrap()
    }
    fn path() -> Path {
        Path::from_controls(
            &(0..201)
                .map(|i| [0., 100., i as f32 * 4. - 400.])
                .collect::<Vec<_>>(),
            100000,
            || false,
        )
        .unwrap()
        .unwrap()
    }
    #[test]
    fn seeded_branches_keep_exact_parent_joins_and_inputs_immutable() {
        let g = grid();
        let path = path();
        let before = path.points().to_vec();
        let tracks = [Track { id: 1, path: &path }];
        let a = generate(&g, &tracks, &[], &policy(), settings(), |_| false, || false).unwrap();
        let b = generate(&g, &tracks, &[], &policy(), settings(), |_| false, || false).unwrap();
        assert_eq!(
            serde_json::to_vec(&a).unwrap(),
            serde_json::to_vec(&b).unwrap()
        );
        let Outcome::Proposed { branches, .. } = a else {
            panic!("Expected branch proposals");
        };
        assert!(!branches.is_empty());
        for branch in branches {
            assert_eq!(branch.path.points()[0], path.points()[branch.start_control]);
            assert_eq!(
                *branch.path.points().last().unwrap(),
                path.points()[branch.end_control]
            );
            assert_eq!(branch.parent, 1);
        }
        assert_eq!(path.points(), before);
    }
    #[test]
    fn nearby_tracks_limits_and_cancellation_cannot_return_partial_branches() {
        let g = grid();
        let path = path();
        let tracks = [Track { id: 1, path: &path }, Track { id: 2, path: &path }];
        let result =
            generate(&g, &tracks, &[], &policy(), settings(), |_| false, || false).unwrap();
        assert!(matches!(result,Outcome::Proposed{branches,..}if branches.is_empty()));
        let mut s = settings();
        s.maximum_work = 1;
        assert!(matches!(
            generate(&g, &tracks[..1], &[], &policy(), s, |_| false, || false).unwrap(),
            Outcome::BudgetExceeded { .. }
        ));
        let mut s = settings();
        s.maximum_branches = 1;
        assert!(matches!(
            generate(&g, &tracks[..1], &[], &policy(), s, |_| false, || false).unwrap(),
            Outcome::BudgetExceeded { .. }
        ));
        let calls = std::cell::Cell::new(0);
        assert!(matches!(
            generate(
                &g,
                &tracks,
                &[],
                &policy(),
                settings(),
                |_| false,
                || {
                    calls.set(calls.get() + 1);
                    calls.get() == 40
                }
            )
            .unwrap(),
            Outcome::Cancelled
        ));
        let mut bad = policy();
        bad.endpoint_margin = 0;
        assert!(generate(&g, &tracks, &[], &bad, settings(), |_| false, || false).is_err());
    }
}
