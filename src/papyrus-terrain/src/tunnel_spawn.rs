//! Fresh dungeon spawn order, final buried frames and world occurrence identities.
use crate::{
    dungeon_base::Pose,
    tunnel_entrances::Assembly,
    tunnel_grid::{Direction, Grid},
    tunnel_layout::{Layout, Placement, Role},
    tunnel_network::{Failure, Meter},
};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Downwards([usize; 2]),
    Upwards([usize; 2]),
    Grid(Role),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Spawn {
    pub instance: u64,
    pub prefab_id: u32,
    pub pose: Pose,
    pub origin: Origin,
}

pub struct Request<'a> {
    pub grid: Grid,
    pub layout: &'a Layout,
    pub entrances: &'a Assembly,
    pub occupied: &'a [u64],
    pub first_instance: u64,
    pub maximum_work: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    /// Entrance chains, all primary cells, then all variant transitions.
    pub spawns: Vec<Spawn>,
    /// None when the final allocated identity is u64::MAX.
    pub next_instance: Option<u64>,
    pub work: usize,
}

/// Build a private World.AddPrefab roster. Both grid passes visit X before Z.
/// A cell's east transition precedes its north transition. Link chains retain
/// entrance registration order and each chain's downwards/upwards segment order.
/// Planning changes neither the input layout nor the registered world.
pub fn plan(request: Request<'_>, cancelled: impl Fn() -> bool) -> Result<Plan, Failure> {
    const MAX_SPAWNS: usize = 65536;
    let n = request.grid.count();
    if request.layout.network.connections.len() != n * n
        || request.layout.placements.len() > MAX_SPAWNS
        || request.entrances.entrances.len() > 1024
        || request.occupied.len() > MAX_SPAWNS
        || request.first_instance == 0
        || !request.entrances.vertical_offset.is_finite()
        || !(-10000. ..=0.).contains(&request.entrances.vertical_offset)
        || !(1..=128_000_000).contains(&request.maximum_work)
    {
        return Err(Failure::Invalid("Invalid tunnel spawn inputs"));
    }
    let mut meter = Meter {
        used: 0,
        limit: request.maximum_work,
        cancelled: &cancelled,
    };
    let count = request.entrances.entrances.iter().try_fold(
        request.layout.placements.len(),
        |count, entrance| {
            meter.spend(1)?;
            count
                .checked_add(entrance.connection.downwards.len())
                .and_then(|count| count.checked_add(entrance.connection.upwards.len()))
                .filter(|&count| count <= MAX_SPAWNS)
                .ok_or(Failure::Invalid("Tunnel spawn count exceeds its allowance"))
        },
    )?;
    // Reserve sorting and identity-index work before allocation. Every grid
    // cell is checked under the same meter below.
    let sorting = request
        .layout
        .placements
        .len()
        .saturating_mul(request.layout.placements.len().max(1).ilog2() as usize + 1);
    let identities = count.saturating_add(request.occupied.len());
    meter.spend(sorting + identities.saturating_mul(identities.max(1).ilog2() as usize + 1))?;
    let mut occupied = BTreeSet::new();
    for &id in request.occupied {
        meter.spend(1)?;
        if id == 0 || !occupied.insert(id) {
            return Err(Failure::Invalid(
                "Invalid existing world occurrence identity",
            ));
        }
    }
    let mut primaries = Vec::new();
    let mut transitions = Vec::new();
    let mut cells = BTreeSet::new();
    let mut stations = BTreeSet::new();
    let mut seams = BTreeSet::new();
    for placement in &request.layout.placements {
        meter.spend(1)?;
        validate(request.grid, placement)?;
        match placement.role {
            Role::Transition(direction) => {
                if !seams.insert((placement.cell, direction as usize)) {
                    return Err(Failure::Invalid("Duplicate tunnel transition spawn"));
                }
                transitions.push(placement);
            }
            role => {
                let [x, z] = placement.cell;
                if request.layout.network.connections[z * n + x] == 0
                    || !cells.insert(placement.cell)
                {
                    return Err(Failure::Invalid("Invalid or duplicate tunnel cell spawn"));
                }
                if role == Role::Station {
                    stations.insert(placement.cell);
                }
                primaries.push(placement);
            }
        }
    }
    let mut routed = 0;
    for (index, &mask) in request.layout.network.connections.iter().enumerate() {
        meter.spend(5)?;
        if mask & !15 != 0 {
            return Err(Failure::Invalid("Invalid tunnel network connection mask"));
        }
        routed += usize::from(mask != 0);
        for direction in Direction::ALL {
            if mask & (1 << direction as usize) == 0 {
                continue;
            }
            let next = request
                .grid
                .neighbour([index % n, index / n], direction)
                .ok_or(Failure::Invalid("Tunnel network exits the grid"))?;
            if request.layout.network.connections[next[1] * n + next[0]]
                & (1 << direction.opposite() as usize)
                == 0
            {
                return Err(Failure::Invalid(
                    "Tunnel network connection is not reciprocal",
                ));
            }
        }
    }
    if cells.len() != routed {
        return Err(Failure::Invalid("Tunnel spawn roster omits a routed cell"));
    }
    for transition in &transitions {
        let Role::Transition(direction) = transition.role else {
            unreachable!()
        };
        let next = request
            .grid
            .neighbour(transition.cell, direction)
            .ok_or(Failure::Invalid("Tunnel transition exits the grid"))?;
        let mask = request.layout.network.connections[transition.cell[1] * n + transition.cell[0]];
        if !cells.contains(&transition.cell)
            || !cells.contains(&next)
            || mask & (1 << direction as usize) == 0
        {
            return Err(Failure::Invalid(
                "Tunnel transition has no adjacent cell pair",
            ));
        }
    }
    primaries.sort_unstable_by_key(|placement| placement.cell);
    transitions.sort_unstable_by_key(|placement| {
        (
            placement.cell,
            match placement.role {
                Role::Transition(Direction::East) => 0,
                _ => 1,
            },
        )
    });
    let mut spawns = Vec::with_capacity(count);
    let mut next_instance = Some(request.first_instance);
    let mut append = |prefab_id, pose: Pose, origin| -> Result<(), Failure> {
        meter.spend(1)?;
        if prefab_id == 0 || pose.validate().is_err() {
            return Err(Failure::Invalid("Invalid tunnel spawn prefab or frame"));
        }
        let instance = loop {
            meter.spend(1)?;
            let id =
                next_instance.ok_or(Failure::Invalid("World occurrence identities exhausted"))?;
            next_instance = id.checked_add(1);
            if occupied.insert(id) {
                break id;
            }
        };
        spawns.push(Spawn {
            instance,
            prefab_id,
            pose,
            origin,
        });
        Ok(())
    };
    for entrance in &request.entrances.entrances {
        if !stations.remove(&entrance.station_cell) {
            return Err(Failure::Invalid(
                "Missing or duplicate tunnel entrance station",
            ));
        }
        for (segments, origin) in [
            (
                &entrance.connection.downwards,
                Origin::Downwards(entrance.station_cell),
            ),
            (
                &entrance.connection.upwards,
                Origin::Upwards(entrance.station_cell),
            ),
        ] {
            for segment in segments {
                append(segment.prefab_id, segment.pose, origin)?;
            }
        }
    }
    if !stations.is_empty() {
        return Err(Failure::Invalid("Tunnel station has no entrance chain"));
    }
    for placement in primaries.into_iter().chain(transitions) {
        let mut position = placement.position;
        position[1] += request.entrances.vertical_offset;
        append(
            placement.prefab_id,
            Pose {
                position,
                rotation: [0., 0., 0., 1.],
            },
            Origin::Grid(placement.role),
        )?;
    }
    meter.spend(1)?;
    Ok(Plan {
        spawns,
        next_instance,
        work: meter.used,
    })
}

fn validate(grid: Grid, placement: &Placement) -> Result<(), Failure> {
    let position = grid
        .world(placement.cell)
        .map_err(|_| Failure::Invalid("Tunnel spawn lies outside its grid"))?;
    let expected = match placement.role {
        Role::Transition(direction @ (Direction::East | Direction::North)) => {
            let cell = grid
                .neighbour(placement.cell, direction)
                .ok_or(Failure::Invalid("Tunnel transition exits the grid"))?;
            let other = grid
                .world(cell)
                .map_err(|_| Failure::Invalid("Invalid transition cell"))?;
            std::array::from_fn(|i| (position[i] + other[i]) * 0.5)
        }
        Role::Transition(_) => return Err(Failure::Invalid("Invalid tunnel transition direction")),
        _ => position,
    };
    if placement.prefab_id == 0 || placement.position != expected {
        return Err(Failure::Invalid(
            "Tunnel placement differs from its grid frame",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        tunnel_entrances::LinkedEntrance,
        tunnel_links::{Connection, Segment},
        tunnel_network::Network,
    };

    fn fixture() -> (Grid, Layout, Assembly) {
        let grid = Grid::new(2000., 216.).unwrap();
        let mut connections = vec![0; grid.count() * grid.count()];
        let cell = |prefab_id, cell, role| Placement {
            prefab_id,
            cell,
            position: grid.world(cell).unwrap(),
            role,
        };
        let seam = |prefab_id, direction| {
            let cell = [2, 2];
            let a = grid.world(cell).unwrap();
            let b = grid
                .world(grid.neighbour(cell, direction).unwrap())
                .unwrap();
            Placement {
                prefab_id,
                cell,
                role: Role::Transition(direction),
                position: std::array::from_fn(|i| (a[i] + b[i]) * 0.5),
            }
        };
        connections[2 * grid.count() + 2] = 1 | 8;
        connections[2 * grid.count() + 3] = 4;
        connections[3 * grid.count() + 2] = 2;
        let layout = Layout {
            network: Network {
                connections,
                work: 0,
            },
            // Deliberately interleaved in a different axis order, matching the
            // layout solver's representation rather than World.AddPrefab order.
            placements: vec![
                cell(3, [3, 2], Role::Tunnel),
                seam(5, Direction::North),
                cell(1, [2, 2], Role::Station),
                seam(4, Direction::East),
                cell(2, [2, 3], Role::Tunnel),
            ],
            work: 0,
        };
        let segment = |prefab_id| Segment {
            prefab_id,
            pose: Pose {
                position: [0., -1., 0.],
                rotation: [0., 0., 0., 1.],
            },
        };
        let assembly = Assembly {
            vertical_offset: -30.,
            entrances: vec![LinkedEntrance {
                station_cell: [2, 2],
                connection: Connection {
                    downwards: vec![segment(10), segment(11)],
                    upwards: vec![segment(12)],
                    work: 0,
                    gap: 0.,
                    angle: 0.,
                },
            }],
            work: 0,
            attempts: 1,
            terrain_samples: 0,
        };
        (grid, layout, assembly)
    }

    fn run(grid: Grid, layout: &Layout, entrances: &Assembly) -> Result<Plan, Failure> {
        plan(
            Request {
                grid,
                layout,
                entrances,
                occupied: &[1, 2, 4],
                first_instance: 1,
                maximum_work: 10000,
            },
            || false,
        )
    }

    #[test]
    fn source_spawn_order_preserves_links_then_x_major_cells_then_east_north_seams() {
        let (grid, layout, assembly) = fixture();
        let before = layout.clone();
        let result = run(grid, &layout, &assembly).unwrap();
        assert_eq!(
            result
                .spawns
                .iter()
                .map(|s| s.prefab_id)
                .collect::<Vec<_>>(),
            [10, 11, 12, 1, 2, 3, 4, 5]
        );
        assert_eq!(
            result.spawns.iter().map(|s| s.instance).collect::<Vec<_>>(),
            [3, 5, 6, 7, 8, 9, 10, 11]
        );
        assert_eq!(result.next_instance, Some(12));
        assert_eq!(result.spawns[0].pose.position[1], -1.);
        assert!(
            result.spawns[3..]
                .iter()
                .all(|s| s.pose.position[1] == -30.)
        );
        assert_eq!(layout, before);
        assert_eq!(result, run(grid, &layout, &assembly).unwrap());
    }

    #[test]
    fn entrance_registration_order_precedes_grid_sorting() {
        let (grid, mut layout, mut assembly) = fixture();
        layout.placements[0].role = Role::Station;
        let mut first = assembly.entrances[0].clone();
        first.station_cell = [3, 2];
        first.connection.downwards[0].prefab_id = 20;
        first.connection.downwards.truncate(1);
        first.connection.upwards.clear();
        assembly.entrances.insert(0, first);
        let result = run(grid, &layout, &assembly).unwrap();
        assert_eq!(
            result
                .spawns
                .iter()
                .take(4)
                .map(|s| s.prefab_id)
                .collect::<Vec<_>>(),
            [20, 10, 11, 12]
        );
    }

    #[test]
    fn malformed_rosters_and_frames_cannot_produce_partial_spawns() {
        let (grid, layout, assembly) = fixture();
        let mut missing = assembly.clone();
        missing.entrances.clear();
        assert!(run(grid, &layout, &missing).is_err());
        let mut duplicate = assembly.clone();
        duplicate.entrances.push(duplicate.entrances[0].clone());
        assert!(run(grid, &layout, &duplicate).is_err());
        let mut changed = layout.clone();
        changed.placements[0].position[0] += 1.;
        assert!(run(grid, &changed, &assembly).is_err());
        changed = layout.clone();
        changed.placements.remove(0);
        assert!(run(grid, &changed, &assembly).is_err());
        changed = layout.clone();
        changed.placements.push(changed.placements[1].clone());
        assert!(run(grid, &changed, &assembly).is_err());
        missing = assembly.clone();
        missing.entrances[0].connection.downwards[0].pose.rotation = [0.; 4];
        assert!(run(grid, &layout, &missing).is_err());
    }

    #[test]
    fn occurrence_exhaustion_cancellation_and_budget_return_no_plan() {
        let (grid, layout, assembly) = fixture();
        let request = || Request {
            grid,
            layout: &layout,
            entrances: &assembly,
            occupied: &[],
            first_instance: u64::MAX,
            maximum_work: 10000,
        };
        assert!(plan(request(), || false).is_err());
        assert_eq!(plan(request(), || true), Err(Failure::Cancelled));
        assert_eq!(
            plan(
                Request {
                    maximum_work: 1,
                    ..request()
                },
                || false
            ),
            Err(Failure::WorkLimit)
        );
        let mut one = layout.clone();
        one.placements
            .retain(|p| p.role == Role::Station || p.prefab_id == 3);
        one.network.connections.fill(0);
        one.network.connections[2 * grid.count() + 2] = 8;
        one.network.connections[2 * grid.count() + 3] = 4;
        let mut empty_links = assembly.clone();
        empty_links.entrances[0].connection.downwards.clear();
        empty_links.entrances[0].connection.upwards.clear();
        let last = plan(
            Request {
                layout: &one,
                entrances: &empty_links,
                first_instance: u64::MAX - 1,
                ..request()
            },
            || false,
        )
        .unwrap();
        assert_eq!(last.spawns[0].instance, u64::MAX - 1);
        assert_eq!(last.spawns[1].instance, u64::MAX);
        assert_eq!(last.next_instance, None);
    }

    #[test]
    fn invalid_networks_and_reserved_identities_are_rejected() {
        let (grid, layout, assembly) = fixture();
        for mask in [16, 0, 1] {
            let mut changed = layout.clone();
            changed.network.connections[2 * grid.count() + 2] = mask;
            assert!(run(grid, &changed, &assembly).is_err());
        }
        for occupied in [&[0][..], &[1, 1][..]] {
            assert!(
                plan(
                    Request {
                        grid,
                        layout: &layout,
                        entrances: &assembly,
                        occupied,
                        first_instance: 1,
                        maximum_work: 10000,
                    },
                    || false
                )
                .is_err()
            );
        }
    }

    #[test]
    fn late_cancellation_and_exact_work_boundary_keep_inputs_private() {
        let (grid, layout, assembly) = fixture();
        let before = layout.clone();
        let request = |maximum_work| Request {
            grid,
            layout: &layout,
            entrances: &assembly,
            occupied: &[],
            first_instance: 1,
            maximum_work,
        };
        let polls = std::cell::Cell::new(0);
        let result = plan(request(10000), || {
            polls.set(polls.get() + 1);
            false
        })
        .unwrap();
        for cutoff in [polls.get() / 2, polls.get() - 1] {
            let calls = std::cell::Cell::new(0);
            assert_eq!(
                plan(request(10000), || {
                    calls.set(calls.get() + 1);
                    calls.get() > cutoff
                }),
                Err(Failure::Cancelled)
            );
            assert_eq!(layout, before);
        }
        assert_eq!(plan(request(result.work), || false).unwrap(), result);
        assert_eq!(
            plan(request(result.work - 1), || false),
            Err(Failure::WorkLimit)
        );
    }
}
