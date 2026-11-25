//! Transactional module assignment over an underground topology plan.
use crate::{
    tunnel_grid::{Cell, Direction, Grid, choose_checked, transition_matches},
    tunnel_network::{self, Failure, Meter, Network},
};
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
pub struct Station<'a> {
    pub cell: [usize; 2],
    pub module: &'a Cell,
}

pub struct Request<'a> {
    pub grid: Grid,
    pub tunnels: &'a [&'a Cell],
    pub stations: &'a [Station<'a>],
    pub transitions: &'a [&'a Cell],
    pub blocked: &'a [bool],
    pub seed: u64,
    pub maximum_work: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Tunnel,
    Station,
    Transition(Direction),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    pub prefab_id: u32,
    pub cell: [usize; 2],
    /// Grid-relative world frame. The depth stage supplies the vertical offset.
    pub position: [f32; 3],
    pub role: Role,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub network: Network,
    pub placements: Vec<Placement>,
    pub work: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Routing(Failure),
    MissingCell([usize; 2]),
    MissingTransition([usize; 2], Direction),
}
impl From<Failure> for Error {
    fn from(value: Failure) -> Self {
        Self::Routing(value)
    }
}

pub fn assemble(request: Request<'_>, cancelled: impl Fn() -> bool) -> Result<Layout, Error> {
    let Request {
        grid,
        tunnels,
        stations,
        transitions,
        blocked,
        seed,
        maximum_work,
    } = request;
    if tunnels.is_empty()
        || tunnels.len() > 4096
        || transitions.len() > 4096
        || stations.len() > 1024
        || maximum_work == 0
    {
        return Err(Failure::Invalid("Invalid tunnel module pools").into());
    }
    let mut meter = Meter {
        used: 0,
        limit: maximum_work,
        cancelled: &cancelled,
    };
    let mut identities = BTreeMap::new();
    for module in tunnels
        .iter()
        .copied()
        .chain(transitions.iter().copied())
        .chain(stations.iter().map(|s| s.module))
    {
        meter.spend(1 + module.avoid_neighbours.len())?;
        if module.id == 0 || module.avoid_neighbours.len() > 256 {
            return Err(Failure::Invalid("Invalid tunnel module identity").into());
        }
        if let Some(previous) = identities.insert(module.id, module)
            && previous != module
        {
            return Err(Failure::Invalid("Conflicting tunnel module identity").into());
        }
    }
    meter.spend(stations.len())?;
    let roots: Vec<_> = stations
        .iter()
        .map(|station| tunnel_network::Station {
            cell: station.cell,
            connections: station.module.connections.map(|kind| kind != 0),
        })
        .collect();
    let remaining = maximum_work
        .checked_sub(meter.used)
        .filter(|&remaining| remaining > 0)
        .ok_or(Failure::WorkLimit)?;
    let network = tunnel_network::plan(grid, &roots, blocked, seed, remaining, &cancelled)?;
    meter.spend(network.work)?;
    let n = grid.count();
    meter.spend(n * n)?;
    let mut chosen = vec![None; n * n];
    let mut fixed = vec![false; n * n];
    for station in stations {
        let index = station.cell[1] * n + station.cell[0];
        let neighbours = std::array::from_fn(|side| {
            grid.neighbour(station.cell, Direction::ALL[side])
                .and_then(|[x, z]| chosen[z * n + x])
        });
        meter.spend(1 + station.module.avoid_neighbours.len())?;
        let required = station.module.connections.map(|kind| kind != 0);
        if !station.module.matches(required, neighbours) {
            return Err(Failure::Invalid("Fixed station modules disagree").into());
        }
        chosen[index] = Some(station.module);
        fixed[index] = true;
    }
    let mut pool = tunnels.to_vec();
    let mut random = seed;
    for (index, &mask) in network.connections.iter().enumerate() {
        meter.spend(1)?;
        if mask == 0 || chosen[index].is_some() {
            continue;
        }
        meter.spend(pool.len())?;
        for i in (1..pool.len()).rev() {
            let j = (crate::decor::random(&mut random) % (i as u64 + 1)) as usize;
            pool.swap(i, j);
        }
        let cell = [index % n, index / n];
        let neighbours = std::array::from_fn(|side| {
            grid.neighbour(cell, Direction::ALL[side])
                .and_then(|[x, z]| chosen[z * n + x])
        });
        let required = std::array::from_fn(|side| mask & (1 << side) != 0);
        chosen[index] = Some(
            choose_checked(pool.iter().copied(), required, neighbours, |module| {
                meter.spend(1 + module.avoid_neighbours.len())
            })?
            .ok_or(Error::MissingCell(cell))?,
        );
    }
    let placements = publish(grid, &network, &chosen, &fixed, transitions, &mut meter)?;
    meter.spend(1)?;
    Ok(Layout {
        network,
        placements,
        work: meter.used,
    })
}

fn publish(
    grid: Grid,
    network: &Network,
    chosen: &[Option<&Cell>],
    fixed: &[bool],
    transitions: &[&Cell],
    meter: &mut Meter<'_>,
) -> Result<Vec<Placement>, Error> {
    let n = grid.count();
    let mut placements = Vec::new();
    for (index, module) in chosen.iter().enumerate() {
        let Some(module) = module else {
            continue;
        };
        meter.spend(5)?;
        let cell = [index % n, index / n];
        let position = grid
            .world(cell)
            .map_err(|_| Failure::Invalid("Invalid tunnel placement cell"))?;
        let mask = module
            .connections
            .iter()
            .enumerate()
            .fold(0u8, |mask, (side, &kind)| {
                mask | (u8::from(kind != 0) << side)
            });
        if mask != network.connections[index] {
            return Err(Failure::Invalid("Module assignment changed its routed sides").into());
        }
        placements.push(Placement {
            prefab_id: module.id,
            cell,
            position,
            role: if fixed[index] {
                Role::Station
            } else {
                Role::Tunnel
            },
        });
        for direction in Direction::ALL {
            let kind = module.connections[direction as usize];
            if kind == 0 {
                continue;
            }
            let next_cell = grid
                .neighbour(cell, direction)
                .ok_or(Failure::Invalid("Tunnel module exits the grid"))?;
            let other = chosen[next_cell[1] * n + next_cell[0]]
                .ok_or(Failure::Invalid("Missing connected tunnel module"))?;
            if other.connections[direction.opposite() as usize] != kind {
                return Err(Failure::Invalid("Tunnel module connection kinds disagree").into());
            }
            if !matches!(direction, Direction::East | Direction::North)
                || module.variants[direction as usize]
                    == other.variants[direction.opposite() as usize]
            {
                continue;
            }
            let mut transition = None;
            for &candidate in transitions {
                meter.spend(1)?;
                if transition_matches(candidate, module, other, direction) {
                    transition = Some(candidate);
                    break;
                }
            }
            let transition = transition.ok_or(Error::MissingTransition(cell, direction))?;
            let other_position = grid
                .world(next_cell)
                .map_err(|_| Failure::Invalid("Invalid transition cell"))?;
            placements.push(Placement {
                prefab_id: transition.id,
                cell,
                position: std::array::from_fn(|axis| (position[axis] + other_position[axis]) * 0.5),
                role: Role::Transition(direction),
            });
        }
    }
    Ok(placements)
}

pub struct Replacement<'a> {
    pub cell: [usize; 2],
    pub original_prefab_id: u32,
    pub module: &'a Cell,
}

/// Replace selected buried cells atomically and rebuild every variant transition.
/// The allowance includes the original layout's work and the returned work is cumulative.
pub fn replace(
    grid: Grid,
    layout: &Layout,
    replacements: &[Replacement<'_>],
    definitions: &[&Cell],
    transitions: &[&Cell],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Layout, Error> {
    let n = grid.count();
    if layout.network.connections.len() != n * n
        || layout.placements.len() > n * n * 3
        || replacements.len() > n * n
        || definitions.len() > 4096
        || transitions.len() > 4096
        || !(1..=128_000_000).contains(&maximum_work)
    {
        return Err(Failure::Invalid("Invalid tunnel replacement inputs").into());
    }
    let mut meter = Meter {
        used: layout.work,
        limit: maximum_work,
        cancelled: &cancelled,
    };
    meter.spend(n * n)?;
    let mut modules = BTreeMap::new();
    for &module in definitions {
        meter.spend(1)?;
        if module.id == 0
            || module.avoid_neighbours.len() > 256
            || modules.insert(module.id, module).is_some()
        {
            return Err(
                Failure::Invalid("Invalid or duplicate tunnel replacement definition").into(),
            );
        }
    }
    for transition in transitions {
        meter.spend(1)?;
        if modules.get(&transition.id) != Some(transition) {
            return Err(Failure::Invalid("Unbound tunnel transition definition").into());
        }
    }
    let mut chosen = vec![None; n * n];
    let mut fixed = vec![false; n * n];
    for placement in &layout.placements {
        meter.spend(1)?;
        if matches!(placement.role, Role::Transition(_)) {
            continue;
        }
        let position = grid
            .world(placement.cell)
            .map_err(|_| Failure::Invalid("Invalid replacement grid cell"))?;
        if position != placement.position {
            return Err(Failure::Invalid("Tunnel replacement requires grid-relative roots").into());
        }
        let index = placement.cell[1] * n + placement.cell[0];
        let module = *modules
            .get(&placement.prefab_id)
            .ok_or(Failure::Invalid("Missing original tunnel cell definition"))?;
        if chosen[index].replace(module).is_some() {
            return Err(Failure::Invalid("Duplicate tunnel replacement root").into());
        }
        fixed[index] = placement.role == Role::Station;
    }
    let mut replaced = std::collections::BTreeSet::new();
    for replacement in replacements {
        meter.spend(1)?;
        grid.world(replacement.cell)
            .map_err(|_| Failure::Invalid("Invalid replacement grid cell"))?;
        let index = replacement.cell[1] * n + replacement.cell[0];
        let original = chosen[index].ok_or(Failure::Invalid("Replacement cell is empty"))?;
        if !replaced.insert(index)
            || replacement.original_prefab_id != original.id
            || replacement.module.id == 0
            || modules.get(&replacement.module.id) != Some(&replacement.module)
            || replacement.module.connections != original.connections
        {
            return Err(Failure::Invalid("Stale or incompatible tunnel replacement").into());
        }
        chosen[index] = Some(replacement.module);
    }
    for (index, &mask) in layout.network.connections.iter().enumerate() {
        meter.spend(1)?;
        if (mask == 0) != chosen[index].is_none() {
            return Err(
                Failure::Invalid("Replacement roots disagree with the routed network").into(),
            );
        }
    }
    let placements = publish(
        grid,
        &layout.network,
        &chosen,
        &fixed,
        transitions,
        &mut meter,
    )?;
    meter.spend(1)?;
    Ok(Layout {
        network: layout.network.clone(),
        placements,
        work: meter.used,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cells() -> Vec<Cell> {
        (1u8..16)
            .filter(|mask| mask.count_ones() >= 2)
            .map(|mask| Cell {
                id: u32::from(mask),
                connections: std::array::from_fn(|side| u8::from(mask & (1 << side) != 0)),
                variants: [0; 4],
                avoid_neighbours: Vec::new(),
            })
            .collect()
    }

    fn replacement_fixture() -> (Grid, Layout, Vec<Cell>) {
        let grid = Grid::new(2000., 216.).unwrap();
        let cells = vec![
            Cell {
                id: 1,
                connections: [0, 0, 0, 1],
                variants: [0; 4],
                avoid_neighbours: vec![],
            },
            Cell {
                id: 2,
                connections: [0, 0, 1, 0],
                variants: [0; 4],
                avoid_neighbours: vec![],
            },
            Cell {
                id: 3,
                connections: [0, 0, 0, 1],
                variants: [0, 0, 0, 1],
                avoid_neighbours: vec![],
            },
            Cell {
                id: 4,
                connections: [0, 0, 1, 1],
                variants: [0, 0, 1, 0],
                avoid_neighbours: vec![],
            },
        ];
        let mut connections = vec![0; grid.count() * grid.count()];
        connections[4 * grid.count() + 4] = 8;
        connections[4 * grid.count() + 5] = 4;
        let placements = [[4, 4], [5, 4]]
            .into_iter()
            .enumerate()
            .map(|(i, cell)| Placement {
                prefab_id: i as u32 + 1,
                cell,
                position: grid.world(cell).unwrap(),
                role: if i == 0 { Role::Tunnel } else { Role::Station },
            })
            .collect();
        (
            grid,
            Layout {
                network: Network {
                    connections,
                    work: 10,
                },
                placements,
                work: 100,
            },
            cells,
        )
    }
    #[test]
    fn portal_replacement_rebuilds_and_removes_changed_variant_transitions() {
        let (grid, layout, cells) = replacement_fixture();
        let definitions: Vec<_> = cells.iter().collect();
        let replacements = [Replacement {
            cell: [4, 4],
            original_prefab_id: 1,
            module: &cells[2],
        }];
        let result = replace(
            grid,
            &layout,
            &replacements,
            &definitions,
            &[&cells[3]],
            10000,
            || false,
        )
        .unwrap();
        assert_eq!(result.network, layout.network);
        assert!(result.work > layout.work && result.work <= 10000);
        assert_eq!(layout.placements.len(), 2);
        assert_eq!(result.placements.len(), 3);
        assert!(
            result
                .placements
                .iter()
                .any(|p| p.prefab_id == 3 && p.role == Role::Tunnel)
        );
        let transition = result.placements.iter().find(|p| p.prefab_id == 4).unwrap();
        assert_eq!(transition.role, Role::Transition(Direction::East));
        assert_eq!(
            transition.position,
            std::array::from_fn(|i| (layout.placements[0].position[i]
                + layout.placements[1].position[i])
                * 0.5)
        );
        let undo = [Replacement {
            cell: [4, 4],
            original_prefab_id: 3,
            module: &cells[0],
        }];
        let restored = replace(
            grid,
            &result,
            &undo,
            &definitions,
            &[&cells[3]],
            10000,
            || false,
        )
        .unwrap();
        assert_eq!(restored.placements, layout.placements);
    }
    #[test]
    fn replacement_missing_transition_stale_identity_and_cancellation_are_atomic() {
        let (grid, layout, cells) = replacement_fixture();
        let definitions: Vec<_> = cells.iter().collect();
        let replacements = [Replacement {
            cell: [4, 4],
            original_prefab_id: 1,
            module: &cells[2],
        }];
        assert!(matches!(
            replace(
                grid,
                &layout,
                &replacements,
                &definitions,
                &[],
                10000,
                || false
            ),
            Err(Error::MissingTransition([4, 4], Direction::East))
        ));
        assert!(matches!(
            replace(
                grid,
                &layout,
                &replacements,
                &definitions,
                &[&cells[3]],
                101,
                || false
            ),
            Err(Error::Routing(Failure::WorkLimit))
        ));
        assert!(matches!(
            replace(
                grid,
                &layout,
                &replacements,
                &definitions,
                &[&cells[3]],
                10000,
                || true
            ),
            Err(Error::Routing(Failure::Cancelled))
        ));
        let stale = [Replacement {
            cell: [4, 4],
            original_prefab_id: 99,
            module: &cells[2],
        }];
        assert!(matches!(
            replace(
                grid,
                &layout,
                &stale,
                &definitions,
                &[&cells[3]],
                10000,
                || false
            ),
            Err(Error::Routing(Failure::Invalid(_)))
        ));
        assert_eq!(layout.placements[0].prefab_id, 1);
        assert_eq!(layout.placements.len(), 2);
    }
    #[test]
    fn assignment_replays_and_keeps_routed_cells_and_station_roles() {
        let cells = cells();
        let pool: Vec<_> = cells.iter().collect();
        let grid = Grid::new(2000., 216.).unwrap();
        let blocked = vec![false; grid.count() * grid.count()];
        let stations = [Station {
            cell: [4, 4],
            module: &cells[0],
        }];
        let run = || {
            assemble(
                Request {
                    grid,
                    tunnels: &pool,
                    stations: &stations,
                    transitions: &[],
                    blocked: &blocked,
                    seed: 7,
                    maximum_work: 100000,
                },
                || false,
            )
        };
        let layout = run().unwrap();
        assert_eq!(layout, run().unwrap());
        assert!(layout.work >= layout.network.work && layout.work <= 100000);
        assert_eq!(
            layout
                .placements
                .iter()
                .filter(|p| p.role == Role::Station)
                .count(),
            1
        );
        assert_eq!(
            layout.placements.len(),
            layout
                .network
                .connections
                .iter()
                .filter(|&&mask| mask != 0)
                .count()
        );
        assert!(
            layout
                .placements
                .iter()
                .all(|p| p.position == grid.world(p.cell).unwrap())
        );
    }

    #[test]
    fn failure_and_cancellation_discard_the_staged_layout() {
        let cells = cells();
        let grid = Grid::new(2000., 216.).unwrap();
        let blocked = vec![false; grid.count() * grid.count()];
        let stations = [Station {
            cell: [4, 4],
            module: &cells[0],
        }];
        let pool = [&cells[0]];
        let request = |maximum_work| Request {
            grid,
            tunnels: &pool,
            stations: &stations,
            transitions: &[],
            blocked: &blocked,
            seed: 7,
            maximum_work,
        };
        assert!(matches!(
            assemble(request(100000), || false),
            Err(Error::MissingCell(_))
        ));
        assert_eq!(
            assemble(request(1), || false),
            Err(Error::Routing(Failure::WorkLimit))
        );
        assert_eq!(
            assemble(request(100000), || true),
            Err(Error::Routing(Failure::Cancelled))
        );
    }

    #[test]
    fn required_transitions_are_emitted_at_shared_boundaries() {
        let cells = cells();
        let pool: Vec<_> = cells.iter().collect();
        let mut station = cells[0].clone();
        station.id = 1003;
        station.variants = [1; 4];
        let mut first = cells[0].clone();
        first.id = 2003;
        first.variants = [1, 0, 0, 0];
        let mut second = first.clone();
        second.id = 3003;
        second.variants = [0, 1, 0, 0];
        let transitions = [&first, &second];
        let grid = Grid::new(2000., 216.).unwrap();
        let blocked = vec![false; grid.count() * grid.count()];
        let stations = [Station {
            cell: [4, 4],
            module: &station,
        }];
        for (transitions, success) in [(&[][..], false), (&transitions[..], true)] {
            let result = assemble(
                Request {
                    grid,
                    tunnels: &pool,
                    stations: &stations,
                    transitions,
                    blocked: &blocked,
                    seed: 7,
                    maximum_work: 100000,
                },
                || false,
            );
            if !success {
                assert!(matches!(
                    result,
                    Err(Error::MissingTransition(_, Direction::North))
                ));
                continue;
            }
            let layout = result.unwrap();
            let placements: Vec<_> = layout
                .placements
                .iter()
                .filter(|placement| matches!(placement.role, Role::Transition(_)))
                .collect();
            assert_eq!(placements.len(), 2);
            assert_eq!(placements[0].position, [0., 0., -108.]);
            assert_eq!(placements[1].position, [0., 0., 108.]);
        }
    }

    #[test]
    fn cancel_and_budget_at_publication_return_no_partial_layout() {
        let cells = cells();
        let pool: Vec<_> = cells.iter().collect();
        let grid = Grid::new(2000., 216.).unwrap();
        let blocked = vec![false; grid.count() * grid.count()];
        let stations = [Station {
            cell: [4, 4],
            module: &cells[0],
        }];
        let polls = std::cell::Cell::new(0usize);
        let request = |maximum_work| Request {
            grid,
            tunnels: &pool,
            stations: &stations,
            transitions: &[],
            blocked: &blocked,
            seed: 7,
            maximum_work,
        };
        let complete = assemble(request(100000), || {
            polls.set(polls.get() + 1);
            false
        })
        .unwrap();
        let last = polls.get();
        polls.set(0);
        assert_eq!(
            assemble(request(100000), || {
                polls.set(polls.get() + 1);
                polls.get() >= last
            }),
            Err(Error::Routing(Failure::Cancelled))
        );
        assert_eq!(
            assemble(request(complete.work - 1), || false),
            Err(Error::Routing(Failure::WorkLimit))
        );
    }
}
