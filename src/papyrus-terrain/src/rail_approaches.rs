//! Source-configured monument-to-primary-network approach proposals.
//! Pose checks concern authored path connectors, not instantiated train splines.
use crate::{
    path_geometry,
    rail_costmap::{self, Grid, Route, RouteSettings, Targets},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
pub const VERSION: &str = "papyrus-rail-approach-candidates-1";
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub engine: String,
    pub extension_spacing: f32,
    pub minimum_extension_steps: usize,
    pub maximum_extension_steps: usize,
    pub join_points: usize,
    pub join_offset: f32,
    pub tangent_probe_points: usize,
    pub endpoint_fade_points: usize,
    pub maximum_segment_expansions: usize,
    pub minimum_height: f32,
    pub position_tolerance: f32,
    pub tangent_tolerance_degrees: f32,
}
impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        if self.engine != VERSION
            || !self.extension_spacing.is_finite()
            || !(0.1..=32.).contains(&self.extension_spacing)
            || !(1..=32).contains(&self.minimum_extension_steps)
            || !(self.minimum_extension_steps..=64).contains(&self.maximum_extension_steps)
            || !(2..=64).contains(&self.join_points)
            || !self.join_offset.is_finite()
            || !(0. ..=64.).contains(&self.join_offset)
            || !(2..=128).contains(&self.tangent_probe_points)
            || !(1..=64).contains(&self.endpoint_fade_points)
            || !(1..=1_000_000).contains(&self.maximum_segment_expansions)
            || !self.minimum_height.is_finite()
            || !(-500. ..=500.).contains(&self.minimum_height)
            || !self.position_tolerance.is_finite()
            || !(0.0001..=1.).contains(&self.position_tolerance)
            || !self.tangent_tolerance_degrees.is_finite()
            || !(0.1..=30.).contains(&self.tangent_tolerance_degrees)
        {
            return Err("Unsupported rail approach source policy".into());
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
    pub maximum_geometry_work: usize,
}
impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if !self.maximum_grade.is_finite()
            || !(0. ..=1.).contains(&self.maximum_grade)
            || !(1..=8_000_000).contains(&self.maximum_total_expansions)
            || !(1..=128_000_000).contains(&self.maximum_geometry_work)
        {
            return Err("Invalid rail approach work limits".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Socket {
    pub monument: u64,
    pub connector: i64,
    pub position: [f32; 3],
    pub forward: [f32; 3],
}
pub struct Network<'a> {
    pub cells: &'a [[usize; 2]],
    pub path: &'a path_geometry::Path,
}
#[derive(Debug, Serialize)]
pub struct Proposal {
    pub socket: Socket,
    pub extension_steps: usize,
    pub routed_cells: Vec<[usize; 2]>,
    pub join_index: usize,
    pub join_direction: i32,
    pub curve: path_geometry::Path,
    pub start_position_gap: f32,
    pub start_tangent_angle: f32,
    pub join_position_gap: f32,
    pub join_tangent_angle: f32,
}
#[derive(Debug, Serialize)]
pub struct Missing {
    pub monument: u64,
    pub connector: i64,
    pub reason: &'static str,
}
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Proposed {
        approaches: Vec<Proposal>,
        unresolved: Vec<Missing>,
        expansions: usize,
        geometry_work: usize,
    },
    BudgetExceeded {
        expansions: usize,
        geometry_work: usize,
    },
    Cancelled,
}
fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}
fn len(v: [f32; 3]) -> f32 {
    v.iter().map(|v| v * v).sum::<f32>().sqrt()
}
fn unit(v: [f32; 3]) -> [f32; 3] {
    let d = len(v);
    if d < 0.00001 {
        [0.; 3]
    } else {
        v.map(|v| v / d)
    }
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a.iter().zip(b).map(|(a, b)| a * b).sum()
}
fn angle(a: [f32; 3], b: [f32; 3]) -> f32 {
    if len(a) < 0.00001 || len(b) < 0.00001 {
        return 180.;
    }
    dot(unit(a), unit(b)).clamp(-1., 1.).acos().to_degrees()
}
fn smooth(t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    t * t * (3. - 2. * t)
}
fn circular_index(index: usize, delta: isize, count: usize) -> usize {
    (index as isize + delta).rem_euclid(count as isize) as usize
}
fn join_neighbor(
    curve: &path_geometry::Path,
    tangent: [f32; 3],
    maximum_shift: f32,
) -> Option<[f32; 3]> {
    if curve.is_circular() || curve.points().len() < 4 {
        return None;
    }
    let points = curve.points();
    let end = points[points.len() - 1];
    let previous = points[points.len() - 2];
    let length = len(sub(end, previous));
    let tangent = unit(tangent);
    let fitted = sub(end, tangent.map(|v| v * length));
    (len(sub(end, points[0])) >= 0.1
        && length > 0.00001
        && len(tangent) > 0.99
        && len(sub(fitted, previous)) <= maximum_shift)
        .then_some(fitted)
}
fn work(spent: &mut usize, limit: usize) -> bool {
    if *spent >= limit {
        false
    } else {
        *spent += 1;
        true
    }
}
fn socket_seed(seed: u32, monument: u64, connector: i64) -> u32 {
    let mut value = monument ^ (connector as u64).rotate_left(29) ^ (u64::from(seed) << 32);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
    (value ^ (value >> 31)) as u32
}
/// Routes every socket toward a common primary line or loop. Two source-family sockets
/// alternate merge direction. Fallback joins to other approaches, full corridor
/// clearance and instantiated train-track collation remain separate stages.
pub fn build(
    grid: &Grid,
    network: Network<'_>,
    sockets: &[Socket],
    p: &Policy,
    s: Settings,
    mut curve_settings: path_geometry::Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Outcome, String> {
    p.validate()?;
    s.validate()?;
    let circular = network.path.is_circular();
    let count = network.path.points().len() - usize::from(circular);
    let lookahead = (p.join_points - 1).max(p.tangent_probe_points);
    if sockets.len() > 256
        || network.path.points().len() < p.join_points + 1
        || (!circular && count <= lookahead)
        || !(2..=32768).contains(&network.cells.len())
        || curve_settings.horizontal_iterations > 256
        || curve_settings.vertical_iterations > 256
        || !curve_settings.resample_distance.is_finite()
        || !(0.1..=100.).contains(&curve_settings.resample_distance)
    {
        return Err("Invalid primary rail network or socket count".into());
    }
    let n = grid.resolution();
    if network
        .cells
        .iter()
        .any(|&[x, z]| x >= n || z >= n || grid.costs()[z * n + x] == 0)
        || network
            .path
            .points()
            .iter()
            .any(|v| grid.cell([v[0], v[2]]).is_err())
    {
        return Err("Primary rail network lies outside passable routing terrain".into());
    }
    let mut seen = BTreeSet::new();
    for socket in sockets {
        if !seen.insert((socket.monument, socket.connector))
            || socket.monument == 0
            || socket.connector == 0
            || socket
                .position
                .iter()
                .chain(&socket.forward)
                .any(|v| !v.is_finite() || v.abs() > 10000.)
            || (len(socket.forward) - 1.).abs() > 0.01
            || socket.forward[0].hypot(socket.forward[2]) < 0.1
        {
            return Err("Invalid or duplicate authored rail socket".into());
        }
    }
    if cancelled() {
        return Ok(Outcome::Cancelled);
    }
    let preparation_work = n * n + network.cells.len() + sockets.len();
    if preparation_work > s.maximum_geometry_work {
        return Ok(Outcome::BudgetExceeded {
            expansions: 0,
            geometry_work: 0,
        });
    }
    let Some(targets) = Targets::new(n, network.cells, &cancelled)? else {
        return Ok(Outcome::Cancelled);
    };
    let (mut spent, mut expansions) = (preparation_work, 0);
    let controls = network.path.points();
    let tangents = network.path.tangents();
    let mut order = Vec::new();
    let mut centers: BTreeMap<u64, ([f32; 3], usize)> = BTreeMap::new();
    let mut indices = (0..sockets.len()).collect::<Vec<_>>();
    indices.sort_by_key(|&i| (sockets[i].monument, sockets[i].connector));
    for index in indices {
        let socket = &sockets[index];
        if cancelled() {
            return Ok(Outcome::Cancelled);
        }
        let mut distance = f32::INFINITY;
        for point in controls {
            if !work(&mut spent, s.maximum_geometry_work) {
                return Ok(Outcome::BudgetExceeded {
                    expansions,
                    geometry_work: spent,
                });
            }
            distance =
                distance.min((point[0] - socket.position[0]).hypot(point[2] - socket.position[2]));
        }
        order.push((index, distance));
        let center = centers.entry(socket.monument).or_insert(([0.; 3], 0));
        for i in 0..3 {
            center.0[i] += socket.position[i];
        }
        center.1 += 1;
    }
    order.sort_by(|&(a, da), &(b, db)| {
        sockets[a]
            .monument
            .cmp(&sockets[b].monument)
            .then(da.total_cmp(&db))
            .then(sockets[a].connector.cmp(&sockets[b].connector))
    });
    let mut directions = BTreeMap::new();
    let mut approaches = Vec::new();
    let mut unresolved = Vec::new();
    for (index, _) in order {
        if cancelled() {
            return Ok(Outcome::Cancelled);
        }
        let socket = &sockets[index];
        let missing = |reason| Missing {
            monument: socket.monument,
            connector: socket.connector,
            reason,
        };
        let forward = unit(socket.forward);
        let mut point = socket.position;
        let mut prefix = vec![point];
        let mut steps = 0;
        let mut outside = false;
        loop {
            let Ok(cell) = grid.cell([point[0], point[2]]) else {
                outside = true;
                break;
            };
            let passable = grid.costs()[cell[1] * n + cell[0]] > 0;
            let extend = (steps < p.minimum_extension_steps && targets.distance(cell).unwrap() > 1)
                || (steps < p.maximum_extension_steps && !passable);
            if !extend {
                break;
            }
            if !work(&mut spent, s.maximum_geometry_work) {
                return Ok(Outcome::BudgetExceeded {
                    expansions,
                    geometry_work: spent,
                });
            }
            point = std::array::from_fn(|i| point[i] + forward[i] * p.extension_spacing);
            prefix.push(point);
            steps += 1;
        }
        if outside {
            unresolved.push(missing("Authored connector extension leaves the world"));
            continue;
        }
        let start = grid.cell([point[0], point[2]])?;
        if grid.costs()[start[1] * n + start[0]] == 0 {
            unresolved.push(missing(
                "Authored connector cannot reach passable ground within its extension allowance",
            ));
            continue;
        }
        if expansions >= s.maximum_total_expansions {
            return Ok(Outcome::BudgetExceeded {
                expansions,
                geometry_work: spent,
            });
        }
        let route = rail_costmap::route_to_any(
            grid,
            start,
            &targets,
            RouteSettings {
                seed: socket_seed(s.seed, socket.monument, socket.connector),
                maximum_grade: s.maximum_grade,
                maximum_expansions: p
                    .maximum_segment_expansions
                    .min(s.maximum_total_expansions - expansions),
            },
            None,
            &cancelled,
        )?;
        let (cells, points) = match route {
            Route::Cancelled => return Ok(Outcome::Cancelled),
            Route::BudgetExceeded { expansions: used } => {
                expansions += used;
                return Ok(Outcome::BudgetExceeded {
                    expansions,
                    geometry_work: spent,
                });
            }
            Route::Unreachable { expansions: used } => {
                expansions += used;
                unresolved.push(missing("No terrain route to the primary rail network"));
                continue;
            }
            Route::Found {
                cells,
                points,
                expansions: used,
                ..
            } => {
                expansions += used;
                (cells, points)
            }
        };
        for point in points.iter().skip(1).take(points.len().saturating_sub(2)) {
            if !work(&mut spent, s.maximum_geometry_work) {
                return Ok(Outcome::BudgetExceeded {
                    expansions,
                    geometry_work: spent,
                });
            }
            let mut point = *point;
            point[1] = point[1].max(p.minimum_height);
            prefix.push(point);
        }
        if prefix.len() < 2 {
            unresolved.push(missing(
                "Connector is already at the network; runtime direct-join inspection required",
            ));
            continue;
        }
        let last = *prefix.last().unwrap();
        let mut closest = 0;
        let mut distance = f32::INFINITY;
        let can_join = |index: usize, direction: i32| {
            circular || if direction > 0 { index + lookahead < count } else { index >= lookahead }
        };
        for (i, &point) in controls.iter().take(count).enumerate() {
            if !work(&mut spent, s.maximum_geometry_work) {
                return Ok(Outcome::BudgetExceeded {
                    expansions,
                    geometry_work: spent,
                });
            }
            let d = len(sub(point, last));
            if d < distance && (can_join(i, 1) || can_join(i, -1)) {
                distance = d;
                closest = i;
            }
        }
        let (center, total) = centers[&socket.monument];
        let outward = if total > 1 {
            sub(socket.position, center.map(|v| v / total as f32))
        } else {
            forward
        };
        let preferred = if dot(tangents[closest], outward) >= 0. {
            1
        } else {
            -1
        };
        let direction = directions
            .get(&socket.monument)
            .copied()
            .unwrap_or(preferred);
        let direction = if can_join(closest, direction) { direction } else { -direction };
        let index_at = |step: usize| {
            let delta = direction as isize * step as isize;
            if circular { circular_index(closest, delta, count) }
            else { closest.checked_add_signed(delta).expect("bounded open rail merge") }
        };
        let probe = index_at(p.tangent_probe_points);
        let tangent = unit(sub(controls[probe], controls[closest]));
        let mut lateral = [tangent[2], tangent[1], -tangent[0]];
        let approach = sub(
            last,
            prefix[prefix.len().saturating_sub(1 + p.tangent_probe_points)],
        );
        let toward = sub(last, controls[closest]);
        if dot(lateral, toward) < 0. {
            lateral = lateral.map(|v| -v);
        }
        if len(approach) < 0.00001 || len(tangent) < 0.00001 {
            unresolved.push(missing("Degenerate rail merge direction"));
            continue;
        }
        let mut end_index = closest;
        for j in 0..p.join_points {
            end_index = index_at(j);
            let offset = smooth(1. - j as f32 / (p.join_points - 1) as f32) * p.join_offset;
            prefix.push(std::array::from_fn(|i| {
                controls[end_index][i] + lateral[i] * offset
            }));
        }
        if prefix.len() > 32768 {
            return Err("Rail approach control count exceeds supported bound".into());
        }
        let last_index = prefix.len() - 1;
        let weights: Vec<_> = (0..prefix.len())
            .map(|i| {
                smooth(
                    (i as f32 / p.endpoint_fade_points as f32)
                        .min((last_index - i) as f32 / p.endpoint_fade_points as f32),
                )
            })
            .collect();
        let remaining = s.maximum_geometry_work.saturating_sub(spent).min(32000000);
        if remaining == 0 {
            return Ok(Outcome::BudgetExceeded {
                expansions,
                geometry_work: spent,
            });
        }
        curve_settings.maximum_work = remaining;
        let mut curve =
            match path_geometry::build(&prefix, curve_settings, Some(&weights), &cancelled) {
                Ok(Some(curve)) => curve,
                Ok(None) => return Ok(Outcome::Cancelled),
                Err(path_geometry::Failure::Budget) => {
                    return Ok(Outcome::BudgetExceeded {
                        expansions,
                        geometry_work: s.maximum_geometry_work,
                    });
                }
                Err(e) => return Err(format!("Invalid rail approach curve: {e:?}")),
            };
        spent += curve.work();
        let cp = curve.points();
        let ct = curve.tangents();
        let last = cp.len() - 1;
        let start_gap = len(sub(cp[0], socket.position));
        let mut start_angle = angle(ct[0], forward);
        let join_gap = len(sub(cp[last], controls[end_index]));
        let join_tangent = tangents[end_index].map(|v| v * direction as f32);
        let mut join_angle = angle(ct[last], join_tangent);
        // Preserve successful source-shaped curves. A failed merge may fit its
        // final handle within the authored lateral offset, keeping both anchor
        // positions and the connector's departure unchanged. The complete curve
        // still passes the world/pose checks here and downstream terrain checks.
        if start_gap < p.position_tolerance
            && start_angle < p.tangent_tolerance_degrees
            && join_gap < p.position_tolerance
            && join_angle >= p.tangent_tolerance_degrees
            && let Some(neighbor) = join_neighbor(&curve, join_tangent, p.join_offset)
        {
            let count = curve.points().len();
            if count >= s.maximum_geometry_work.saturating_sub(spent) {
                return Ok(Outcome::BudgetExceeded {
                    expansions,
                    geometry_work: spent,
                });
            }
            spent += count;
            let mut fitted = curve.points().to_vec();
            fitted[count - 2] = neighbor;
            curve = match path_geometry::Path::from_controls(
                &fitted,
                s.maximum_geometry_work.saturating_sub(spent).min(32000000),
                &cancelled,
            ) {
                Ok(Some(curve)) => curve,
                Ok(None) => return Ok(Outcome::Cancelled),
                Err(path_geometry::Failure::Budget) => {
                    return Ok(Outcome::BudgetExceeded {
                        expansions,
                        geometry_work: s.maximum_geometry_work,
                    });
                }
                Err(error) => return Err(format!("Invalid fitted rail merge: {error:?}")),
            };
            spent += curve.work();
            start_angle = angle(curve.tangents()[0], forward);
            join_angle = angle(curve.tangents()[last], join_tangent);
        }
        if curve
            .points()
            .iter()
            .any(|point| grid.cell([point[0], point[2]]).is_err())
        {
            unresolved.push(missing("Smoothed approach leaves the world bounds"));
            continue;
        }
        if start_gap >= p.position_tolerance
            || start_angle >= p.tangent_tolerance_degrees
            || join_gap >= p.position_tolerance
            || join_angle >= p.tangent_tolerance_degrees
        {
            unresolved.push(missing("Smoothed approach does not satisfy authored connector/network endpoint pose limits"));
            continue;
        }
        approaches.push(Proposal {
            socket: socket.clone(),
            extension_steps: steps,
            routed_cells: cells,
            join_index: end_index,
            join_direction: direction,
            curve,
            start_position_gap: start_gap,
            start_tangent_angle: start_angle,
            join_position_gap: join_gap,
            join_tangent_angle: join_angle,
        });
        directions.insert(socket.monument, -direction);
    }
    Ok(if cancelled() {
        Outcome::Cancelled
    } else {
        Outcome::Proposed {
            approaches,
            unresolved,
            expansions,
            geometry_work: spent,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rail_ring;
    #[test]
    fn captured_merge_can_fit_its_tangent_within_the_authored_offset() {
        let points = [
            [1218.0854, 65.07072, -139.37695],
            [1225.2961, 65.00327, -136.07123],
            [1328.0928, 64.07981, -74.656204],
            [1335.2716, 64.478775, -77.99793],
        ];
        let tangent = [0.8054188, 0.07765239, -0.5875974];
        let curve = path_geometry::Path::from_controls(&points, 1000, || false)
            .unwrap()
            .unwrap();
        let original_angle = angle(curve.tangents()[3], tangent);
        assert!((11. ..11.5).contains(&original_angle));
        let neighbor = join_neighbor(&curve, tangent, 8.).unwrap();
        assert!(len(sub(neighbor, points[2])) < 2.);
        let mut fitted = points;
        fitted[2] = neighbor;
        let result = path_geometry::Path::from_controls(&fitted, 1000, || false)
            .unwrap()
            .unwrap();
        assert!(angle(result.tangents()[3], tangent) < 0.1);
        assert_eq!(result.points()[0], points[0]);
        assert_eq!(result.points()[1], points[1]);
        assert_eq!(result.points()[3], points[3]);
        assert_eq!(curve.points(), points);
        assert!(join_neighbor(&curve, tangent, 0.1).is_none());
        assert!(join_neighbor(&curve, tangent.map(|v| -v), 8.).is_none());
        assert!(join_neighbor(&curve, [0.; 3], 8.).is_none());
        let short = path_geometry::Path::from_controls(&points[..2], 1000, || false)
            .unwrap()
            .unwrap();
        assert!(join_neighbor(&short, tangent, 8.).is_none());
    }
    #[test]
    fn tight_merges_fit_within_the_shared_geometry_allowance() {
        let (grid, cells, path, sockets) = fixture();
        let mut policy = policy();
        policy.tangent_tolerance_degrees = 1.;
        let run = |settings| {
            build(
                &grid,
                Network {
                    cells: &cells,
                    path: &path,
                },
                &sockets,
                &policy,
                settings,
                path_settings(),
                || false,
            )
            .unwrap()
        };
        let full = run(settings());
        let Outcome::Proposed {
            approaches,
            unresolved,
            geometry_work,
            ..
        } = &full
        else {
            panic!("Expected bounded approach proposals");
        };
        assert!(unresolved.is_empty(), "{unresolved:?}");
        assert_eq!(approaches.len(), sockets.len());
        assert!(approaches.iter().all(|a| a.join_tangent_angle < 1.));
        let mut exact = settings();
        exact.maximum_geometry_work = *geometry_work;
        assert_eq!(
            serde_json::to_value(run(exact)).unwrap(),
            serde_json::to_value(full).unwrap()
        );
        exact.maximum_geometry_work -= 1;
        assert!(matches!(run(exact), Outcome::BudgetExceeded { .. }));
    }
    fn policy() -> Policy {
        Policy {
            engine: VERSION.into(),
            extension_spacing: 7.5,
            minimum_extension_steps: 8,
            maximum_extension_steps: 16,
            join_points: 8,
            join_offset: 8.,
            tangent_probe_points: 16,
            endpoint_fade_points: 8,
            maximum_segment_expansions: 250000,
            minimum_height: 1.,
            position_tolerance: 0.1,
            tangent_tolerance_degrees: 10.,
        }
    }
    fn settings() -> Settings {
        Settings {
            seed: 7,
            maximum_grade: 1.,
            maximum_total_expansions: 2000000,
            maximum_geometry_work: 32000000,
        }
    }
    fn path_settings() -> path_geometry::Settings {
        path_geometry::Settings {
            horizontal_iterations: 32,
            vertical_iterations: 64,
            resample_distance: 7.5,
            maximum_work: 32000000,
        }
    }
    fn fixture() -> (Grid, Vec<[usize; 2]>, path_geometry::Path, Vec<Socket>) {
        let heights = vec![crate::erosion::encode(50.); 513 * 513];
        let topo = vec![0; 512 * 512];
        let blocked = vec![0; 512 * 512];
        let grid = rail_costmap::build(
            rail_costmap::Terrain {
                size: 1000.,
                height_resolution: 513,
                height: &heights,
                topology_resolution: 512,
                topology: &topo,
                placement_resolution: 512,
                placement: &blocked,
            },
            &rail_costmap::Policy {
                spacing: 7.5,
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
        .unwrap();
        let ring = rail_ring::build(
            &grid,
            &rail_ring::Policy {
                engine: rail_ring::VERSION.into(),
                inset_divisor: 4,
                large_world_size: 5000,
                inward_step_cells: 1,
                connector_exclusion_cells: 10,
                maximum_segment_expansions: 250000,
                minimum_height: 1.,
            },
            rail_ring::Settings {
                seed: 7,
                maximum_grade: 1.,
                maximum_total_expansions: 2000000,
                maximum_attempts: 64,
            },
            &[],
            || false,
        )
        .unwrap();
        let rail_ring::Outcome::Found { cells, points, .. } = ring else {
            panic!("expected flat ring")
        };
        let path = path_geometry::build(&points, path_settings(), None, || false)
            .unwrap()
            .unwrap();
        let sockets = vec![
            Socket {
                monument: 1,
                connector: 1,
                position: [10., 50., 0.],
                forward: [1., 0., 0.],
            },
            Socket {
                monument: 1,
                connector: 2,
                position: [-10., 50., 0.],
                forward: [-1., 0., 0.],
            },
        ];
        (grid, cells, path, sockets)
    }
    #[test]
    fn opposite_monument_sockets_preserve_authored_poses_and_merge_in_opposite_directions() {
        let (grid, cells, path, sockets) = fixture();
        let run = || {
            build(
                &grid,
                Network {
                    cells: &cells,
                    path: &path,
                },
                &sockets,
                &policy(),
                settings(),
                path_settings(),
                || false,
            )
            .unwrap()
        };
        let first = run();
        let second = run();
        assert_eq!(
            serde_json::to_value(&first).unwrap(),
            serde_json::to_value(second).unwrap()
        );
        let mut reversed = sockets.clone();
        reversed.reverse();
        let reordered = build(
            &grid,
            Network {
                cells: &cells,
                path: &path,
            },
            &reversed,
            &policy(),
            settings(),
            path_settings(),
            || false,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&first).unwrap(),
            serde_json::to_value(reordered).unwrap()
        );
        let Outcome::Proposed {
            approaches,
            unresolved,
            ..
        } = first
        else {
            panic!("expected approach proposals")
        };
        assert!(unresolved.is_empty(), "{unresolved:?}");
        assert_eq!(approaches.len(), 2);
        assert_eq!(approaches[0].join_direction, -approaches[1].join_direction);
        for a in &approaches {
            assert!(a.start_position_gap < 0.1 && a.join_position_gap < 0.1);
            assert!(a.start_tangent_angle < 10. && a.join_tangent_angle < 10.);
            assert!(a.extension_steps >= 8);
            assert_eq!(a.curve.points()[0], a.socket.position);
        }
    }
    #[test]
    fn malformed_cancelled_and_budget_limited_runs_never_return_partial_approaches() {
        let (grid, cells, path, mut sockets) = fixture();
        let network = || Network {
            cells: &cells,
            path: &path,
        };
        assert!(matches!(
            build(
                &grid,
                network(),
                &sockets,
                &policy(),
                settings(),
                path_settings(),
                || true
            )
            .unwrap(),
            Outcome::Cancelled
        ));
        let mut s = settings();
        s.maximum_geometry_work = 1;
        assert!(matches!(
            build(
                &grid,
                network(),
                &sockets,
                &policy(),
                s,
                path_settings(),
                || false
            )
            .unwrap(),
            Outcome::BudgetExceeded { .. }
        ));
        s = settings();
        s.maximum_total_expansions = 1;
        assert!(matches!(
            build(
                &grid,
                network(),
                &sockets,
                &policy(),
                s,
                path_settings(),
                || false
            )
            .unwrap(),
            Outcome::BudgetExceeded { .. }
        ));
        sockets[1] = sockets[0].clone();
        assert!(
            build(
                &grid,
                network(),
                &sockets,
                &policy(),
                settings(),
                path_settings(),
                || false
            )
            .is_err()
        );
    }
    #[test]
    fn outward_extension_failure_remains_an_explicit_unresolved_socket() {
        let (grid, cells, path, _) = fixture();
        let sockets = [Socket {
            monument: 1,
            connector: 1,
            position: [499., 50., 0.],
            forward: [1., 0., 0.],
        }];
        let Outcome::Proposed {
            approaches,
            unresolved,
            ..
        } = build(
            &grid,
            Network {
                cells: &cells,
                path: &path,
            },
            &sockets,
            &policy(),
            settings(),
            path_settings(),
            || false,
        )
        .unwrap()
        else {
            panic!("expected unresolved proposal report")
        };
        assert!(approaches.is_empty());
        assert_eq!(unresolved.len(), 1);
        assert!(unresolved[0].reason.contains("leaves the world"));
    }
}
