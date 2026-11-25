//! A bounded detour for an isolated obstruction on a retained road.
use papyrus_terrain::{
    path_geometry::{self, Path},
    placement::Solution,
    road_costmap::{CellGrid, CellLattice},
    source_road_ring::RawNode,
    source_road_route,
};
use std::collections::BTreeSet;

const MAX_CROP_CELLS: usize = 96;
const MAX_WORK: usize = 400_000;

#[derive(Clone, Copy)]
pub(super) struct Obstruction {
    center: [f32; 2],
    radius: f32,
}

impl Obstruction {
    fn distance_to_segment(self, a: [f32; 2], b: [f32; 2]) -> f32 {
        let axis = [b[0] - a[0], b[1] - a[1]];
        let delta = [self.center[0] - a[0], self.center[1] - a[1]];
        let length = axis[0] * axis[0] + axis[1] * axis[1];
        let t = if length > 0. {
            ((delta[0] * axis[0] + delta[1] * axis[1]) / length).clamp(0., 1.)
        } else {
            0.
        };
        (delta[0] - axis[0] * t).hypot(delta[1] - axis[1] * t)
    }
}

pub(super) fn moved_footprints(old: &Solution, next: &Solution) -> Vec<Obstruction> {
    let old: std::collections::BTreeMap<_, _> = old
        .outcomes
        .iter()
        .map(|item| (item.id, item.position.as_ref()))
        .collect();
    next.outcomes
        .iter()
        .filter_map(|item| {
            let now = item.position.as_ref()?;
            if old.get(&item.id).copied().flatten() == Some(now) {
                return None;
            }
            Some(Obstruction {
                center: [now.x, now.z],
                radius: now.clearance,
            })
        })
        .collect()
}

pub(super) struct Detour {
    pub nodes: Vec<RawNode>,
    pub path: Path,
    pub cells: usize,
}

pub(super) fn detour(
    source: &CellGrid,
    nodes: &[RawNode],
    path: &Path,
    protected: &BTreeSet<[usize; 2]>,
    obstructions: &[Obstruction],
    shoulder: f32,
    mut height: impl FnMut([f32; 2]) -> Result<f32, String>,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Detour>, String> {
    let xz = |p: [f32; 3]| [p[0], p[2]];
    let blocked = |a, b, margin| {
        obstructions.iter().any(|obstacle| {
            obstacle.distance_to_segment(a, b) < obstacle.radius + shoulder + margin
        })
    };
    if cancelled() {
        return Ok(None);
    }
    let points = path.points();
    let mut hits = points
        .windows(2)
        .enumerate()
        .filter_map(|(i, pair)| blocked(xz(pair[0]), xz(pair[1]), 0.).then_some(i));
    let Some(first) = hits.next() else {
        return Ok(None);
    };
    let last = hits.last().unwrap_or(first) + 1;
    let lattice = source.lattice;
    let spacing = lattice.spacing();
    let left = (0..=first)
        .rev()
        .find(|&i| !blocked(xz(points[i]), xz(points[i]), spacing * 2.))
        .ok_or("Monument edit reaches the beginning of a retained road")?;
    let right = (last..points.len())
        .find(|&i| !blocked(xz(points[i]), xz(points[i]), spacing * 2.))
        .ok_or("Monument edit reaches the end of a retained road")?;
    if left == 0 || right + 1 == points.len() {
        return Err("Monument edit reaches a retained road attachment".into());
    }
    let raw_anchor = |p: [f32; 3]| {
        nodes
            .iter()
            .enumerate()
            .filter_map(|(i, node)| {
                let [x, z] = lattice.point(node.cell)?;
                let d = (p[0] - x).hypot(p[2] - z);
                (d <= spacing * 1.5).then_some((i, d))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
            .ok_or("Cannot tie the local detour to the original road lattice")
    };
    let raw_left = raw_anchor(points[left])?;
    let raw_right = raw_anchor(points[right])?;
    if raw_left >= raw_right
        || nodes[raw_left + 1..raw_right]
            .iter()
            .any(|node| protected.contains(&node.cell))
    {
        return Err("Local detour crosses a retained road junction".into());
    }

    // Keep the road search private and restricted to the affected section's
    // neighborhood. Coordinates are translated back to the original lattice.
    let a = nodes[raw_left].cell;
    let b = nodes[raw_right].cell;
    let low = [a[0].min(b[0]), a[1].min(b[1])];
    let high = [a[0].max(b[0]), a[1].max(b[1])];
    let n = lattice.resolution();
    let span = (high[0] - low[0]).max(high[1] - low[1]);
    let side = (span + 33).min(n);
    if side > MAX_CROP_CELLS {
        return Err("Monument move spans more than one local road section".into());
    }
    let origin: [usize; 2] =
        std::array::from_fn(|i| (low[i] + high[i]).saturating_sub(side - 1) / 2);
    let origin = origin.map(|offset| offset.min(n - side));
    let cropped = CellGrid {
        lattice: CellLattice::new(side as f32, 1.)?,
        grid: papyrus_terrain::road_costmap::Grid {
            engine: source.grid.engine,
            resolution: side,
            seed: source.grid.seed,
            trail: source.grid.trail,
            counts: Default::default(),
            work: 0,
            costs: (0..side * side)
                .map(|i| {
                    let cell = [origin[0] + i % side, origin[1] + i / side];
                    let p = lattice.point(cell).unwrap();
                    if blocked(p, p, spacing * 1.5)
                        || (cell != a && cell != b && protected.contains(&cell))
                    {
                        0
                    } else {
                        source.grid.costs[cell[1] * n + cell[0]]
                    }
                })
                .collect(),
        },
    };
    let local = |p: [usize; 2]| [p[0] - origin[0], p[1] - origin[1]];
    let routed = match source_road_route::find_path(
        &cropped,
        local(a),
        local(b),
        source_road_route::Settings {
            maximum_expansions: side * side,
            maximum_work: MAX_WORK,
            directional_penalty: 0,
            directional_radius: 1,
        },
        cancelled,
    )? {
        source_road_route::Outcome::Found(route) => route,
        source_road_route::Outcome::Cancelled { .. } => return Ok(None),
        _ => return Err("No clear local road detour around the monument".into()),
    };
    let middle: Vec<_> = routed
        .cells
        .iter()
        .zip(&routed.cumulative_costs)
        .skip(1)
        .take(routed.cells.len().saturating_sub(2))
        .map(|(cell, &cost)| RawNode {
            cell: [cell[0] + origin[0], cell[1] + origin[1]],
            cost: nodes[raw_right].cost.saturating_add(cost),
        })
        .collect();
    let mut controls = vec![points[left]];
    for node in &middle {
        let [x, z] = lattice.point(node.cell).unwrap();
        controls.push([x, height(lattice.normalized(node.cell).unwrap())?, z]);
    }
    controls.push(points[right]);
    let Some(shaped) = path_geometry::build(
        &controls,
        path_geometry::Settings {
            horizontal_iterations: 3,
            vertical_iterations: 2,
            resample_distance: spacing * 0.75,
            maximum_work: MAX_WORK,
        },
        None,
        cancelled,
    )
    .map_err(|error| format!("Invalid local road curve: {error:?}"))?
    else {
        return Ok(None);
    };
    if shaped
        .points()
        .windows(2)
        .any(|pair| blocked(xz(pair[0]), xz(pair[1]), 0.))
    {
        return Err("Smoothed local detour would enter the monument footprint".into());
    }
    let joined = points[..left]
        .iter()
        .copied()
        .chain(shaped.points().iter().copied())
        .chain(points[right + 1..].iter().copied())
        .collect::<Vec<_>>();
    let Some(path) = Path::from_controls(&joined, MAX_WORK, cancelled)
        .map_err(|error| format!("Invalid joined road detour: {error:?}"))?
    else {
        return Ok(None);
    };
    let nodes = nodes[..=raw_left]
        .iter()
        .copied()
        .chain(middle)
        .chain(nodes[raw_right..].iter().copied())
        .collect();
    Ok(Some(Detour {
        nodes,
        path,
        cells: side * side,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use papyrus_terrain::road_costmap::{CELL_VERSION, Grid};

    #[test]
    fn detour_preserves_controls_outside_the_crop_and_refuses_owned_junctions() {
        let n = 128;
        let lattice = CellLattice::new(1024., 8.).unwrap();
        let grid = CellGrid {
            lattice,
            grid: Grid {
                engine: CELL_VERSION,
                resolution: n,
                seed: 1,
                trail: false,
                counts: Default::default(),
                work: 0,
                costs: vec![100; n * n],
            },
        };
        let nodes = (0..n)
            .map(|x| RawNode {
                cell: [x, 64],
                cost: 0,
            })
            .collect::<Vec<_>>();
        let points = nodes
            .iter()
            .map(|node| {
                let [x, z] = lattice.point(node.cell).unwrap();
                [x, 20., z]
            })
            .collect::<Vec<_>>();
        let original = Path::from_controls(&points, MAX_WORK, || false)
            .unwrap()
            .unwrap();
        let center = lattice.point([64, 64]).unwrap();
        let obstruction = Obstruction {
            center,
            radius: 24.,
        };
        let run = |obstructions: &[Obstruction], protected: &BTreeSet<_>| {
            detour(
                &grid,
                &nodes,
                &original,
                protected,
                obstructions,
                5.,
                |_| Ok(20.),
                &|| false,
            )
        };
        let edited = run(&[obstruction], &BTreeSet::new()).unwrap().unwrap();
        assert!(edited.cells < n * n / 4);
        assert!(edited.path.points().starts_with(&points[..56]));
        assert!(edited.path.points().ends_with(&points[73..]));
        assert!(edited.path.points().windows(2).all(|p| {
            obstruction.distance_to_segment([p[0][0], p[0][2]], [p[1][0], p[1][2]]) >= 29.
        }));
        assert!(run(&[obstruction], &BTreeSet::from([[64, 64]])).is_err());
        let off_route = Obstruction {
            center: [center[0], center[1] + 200.],
            ..obstruction
        };
        assert!(run(&[off_route], &BTreeSet::new()).unwrap().is_none());
    }
}
