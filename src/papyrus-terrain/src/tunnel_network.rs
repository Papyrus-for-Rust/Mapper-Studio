//! Bounded cardinal routing between authored underground station exits.
use crate::tunnel_grid::{Direction, Grid};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Station {
    pub cell: [usize; 2],
    pub connections: [bool; 4],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Network {
    /// Row-major north/south/west/east connection bits, including stations.
    pub connections: Vec<u8>,
    pub work: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    Invalid(&'static str),
    Cancelled,
    WorkLimit,
    Disconnected,
}

pub(crate) struct Meter<'a> {
    pub(crate) used: usize,
    pub(crate) limit: usize,
    pub(crate) cancelled: &'a dyn Fn() -> bool,
}
impl Meter<'_> {
    pub(crate) fn spend(&mut self, amount: usize) -> Result<(), Failure> {
        if (self.cancelled)() {
            return Err(Failure::Cancelled);
        }
        self.used = self.used.checked_add(amount).ok_or(Failure::WorkLimit)?;
        if self.used > self.limit {
            return Err(Failure::WorkLimit);
        }
        Ok(())
    }
}

fn adjacent(grid: Grid, index: usize, direction: Direction) -> Option<usize> {
    let n = grid.count();
    grid.neighbour([index % n, index / n], direction)
        .map(|[x, z]| z * n + x)
}

struct Search {
    parents: Vec<usize>,
    queue: Vec<usize>,
}

fn route(
    grid: Grid,
    starts: &[usize],
    goals: &[bool],
    blocked: &[bool],
    random: &mut u64,
    meter: &mut Meter<'_>,
    search: &mut Search,
) -> Result<Vec<usize>, Failure> {
    meter.spend(blocked.len())?;
    let Search { parents, queue } = search;
    parents.fill(usize::MAX);
    queue.clear();
    for &start in starts {
        if !blocked[start] && parents[start] == usize::MAX {
            parents[start] = start;
            queue.push(start);
        }
    }
    let mut directions = Direction::ALL;
    directions.rotate_left((crate::decor::random(random) % 4) as usize);
    let mut head = 0;
    while head < queue.len() {
        meter.spend(5)?;
        let current = queue[head];
        head += 1;
        for direction in directions {
            let Some(next) = adjacent(grid, current, direction) else {
                continue;
            };
            if blocked[next] || parents[next] != usize::MAX {
                continue;
            }
            parents[next] = current;
            if goals[next] {
                let mut path = vec![next];
                let mut node = next;
                while parents[node] != node {
                    meter.spend(1)?;
                    node = parents[node];
                    path.push(node);
                }
                path.reverse();
                return Ok(path);
            }
            queue.push(next);
        }
    }
    Err(Failure::Disconnected)
}

fn connect(grid: Grid, masks: &mut [u8], path: &[usize]) {
    for pair in path.windows(2) {
        let [a, b] = [pair[0], pair[1]];
        let direction = if b == a + grid.count() {
            Direction::North
        } else if a == b + grid.count() {
            Direction::South
        } else if b > a {
            Direction::East
        } else {
            Direction::West
        };
        masks[a] |= 1 << direction as usize;
        masks[b] |= 1 << direction.opposite() as usize;
    }
}

fn components(
    grid: Grid,
    masks: &[u8],
    meter: &mut Meter<'_>,
) -> Result<(Vec<usize>, usize), Failure> {
    meter.spend(masks.len())?;
    let mut labels = vec![usize::MAX; masks.len()];
    let mut count = 0;
    let mut stack = Vec::new();
    for origin in 0..masks.len() {
        if masks[origin] == 0 || labels[origin] != usize::MAX {
            continue;
        }
        labels[origin] = count;
        stack.push(origin);
        while let Some(node) = stack.pop() {
            meter.spend(5)?;
            for direction in Direction::ALL {
                if masks[node] & (1 << direction as usize) == 0 {
                    continue;
                }
                let next = adjacent(grid, node, direction)
                    .ok_or(Failure::Invalid("Tunnel exits the grid"))?;
                if masks[next] & (1 << direction.opposite() as usize) == 0 {
                    return Err(Failure::Invalid("Tunnel connection has no reciprocal side"));
                }
                if labels[next] == usize::MAX {
                    labels[next] = count;
                    stack.push(next);
                }
            }
        }
        count += 1;
    }
    Ok((labels, count))
}

/// Independent deterministic routing. Each station retains its authored sides.
/// Every nonstation path cell has at least two connections, so the captured
/// through-cell roster can cover it. Results publish only after connectivity
/// validation. Burial, geometry selection and entrance chains are later stages.
pub fn plan(
    grid: Grid,
    stations: &[Station],
    blocked: &[bool],
    seed: u64,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Network, Failure> {
    let n = grid.count();
    if blocked.len() != n * n || stations.len() > 1024 || maximum_work == 0 {
        return Err(Failure::Invalid("Invalid tunnel routing inputs"));
    }
    let mut meter = Meter {
        used: 0,
        limit: maximum_work,
        cancelled: &cancelled,
    };
    meter.spend(blocked.len() + stations.len())?;
    let mut masks = vec![0u8; n * n];
    let mut station_cells = vec![false; n * n];
    let mut obstacles = blocked.to_vec();
    for station in stations {
        let [x, z] = station.cell;
        if x >= n || z >= n || !station.connections.iter().any(|&open| open) {
            return Err(Failure::Invalid("Invalid tunnel station cell"));
        }
        let index = z * n + x;
        if obstacles[index] {
            return Err(Failure::Invalid("Blocked or duplicate tunnel station"));
        }
        obstacles[index] = true;
        station_cells[index] = true;
        masks[index] = station
            .connections
            .iter()
            .enumerate()
            .fold(0, |mask, (side, &open)| mask | (u8::from(open) << side));
    }
    for station in stations {
        meter.spend(5)?;
        let index = station.cell[1] * n + station.cell[0];
        for direction in Direction::ALL {
            if !station.connections[direction as usize] {
                continue;
            }
            let next = adjacent(grid, index, direction)
                .ok_or(Failure::Invalid("Station exit is outside the grid"))?;
            if blocked[next] {
                return Err(Failure::Disconnected);
            }
            let reciprocal = 1 << direction.opposite() as usize;
            if station_cells[next] && masks[next] & reciprocal == 0 {
                return Err(Failure::Invalid("Adjacent station exits disagree"));
            }
            masks[next] |= reciprocal;
        }
    }
    let mut random = seed;
    let mut search = Search {
        parents: vec![usize::MAX; masks.len()],
        queue: Vec::with_capacity(masks.len()),
    };
    let mut terminals: Vec<_> = (0..masks.len())
        .filter(|&i| !station_cells[i] && masks[i] != 0)
        .collect();
    meter.spend(masks.len() + terminals.len())?;
    for i in (1..terminals.len()).rev() {
        let j = (crate::decor::random(&mut random) % (i as u64 + 1)) as usize;
        terminals.swap(i, j);
    }
    for terminal in terminals {
        if masks[terminal].count_ones() >= 2 {
            continue;
        }
        meter.spend(masks.len())?;
        let goals: Vec<_> = masks
            .iter()
            .enumerate()
            .map(|(i, &mask)| i != terminal && mask != 0 && !obstacles[i])
            .collect();
        let path = route(
            grid,
            &[terminal],
            &goals,
            &obstacles,
            &mut random,
            &mut meter,
            &mut search,
        )?;
        meter.spend(path.len())?;
        connect(grid, &mut masks, &path);
    }
    loop {
        let (labels, count) = components(grid, &masks, &mut meter)?;
        if count <= 1 {
            break;
        }
        meter.spend(masks.len())?;
        let starts: Vec<_> = (0..masks.len())
            .filter(|&i| labels[i] == 0 && !obstacles[i])
            .collect();
        let goals: Vec<_> = labels
            .iter()
            .map(|&label| label != 0 && label != usize::MAX)
            .collect();
        let path = route(
            grid,
            &starts,
            &goals,
            &obstacles,
            &mut random,
            &mut meter,
            &mut search,
        )?;
        meter.spend(path.len())?;
        connect(grid, &mut masks, &path);
    }
    meter.spend(masks.len())?;
    if masks
        .iter()
        .enumerate()
        .any(|(i, &mask)| !station_cells[i] && mask.count_ones() == 1)
    {
        return Err(Failure::Invalid("Tunnel routing left an open path end"));
    }
    Ok(Network {
        connections: masks,
        work: meter.used,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolated_station_exits_form_a_connected_loop_and_replay() {
        let grid = Grid::new(2000., 216.).unwrap();
        let stations = [Station {
            cell: [4, 4],
            connections: [true, true, false, false],
        }];
        let blocked = vec![false; grid.count() * grid.count()];
        let network = plan(grid, &stations, &blocked, 7, 100000, || false).unwrap();
        assert_eq!(
            network,
            plan(grid, &stations, &blocked, 7, 100000, || false).unwrap()
        );
        assert_eq!(network.connections[4 * grid.count() + 4], 3);
        assert!(
            network
                .connections
                .iter()
                .filter(|&&mask| mask != 0)
                .all(|mask| mask.count_ones() >= 2)
        );
        assert!(
            network
                .connections
                .iter()
                .filter(|&&mask| mask != 0)
                .count()
                >= 6
        );
    }

    #[test]
    fn station_groups_join_and_obstacles_cancellation_and_limits_are_atomic() {
        let grid = Grid::new(2000., 216.).unwrap();
        let stations = [
            Station {
                cell: [2, 2],
                connections: [true, true, false, false],
            },
            Station {
                cell: [6, 6],
                connections: [false, false, true, true],
            },
        ];
        let mut blocked = vec![false; grid.count() * grid.count()];
        blocked[4 * grid.count() + 4] = true;
        let before = blocked.clone();
        let network = plan(grid, &stations, &blocked, 9, 100000, || false).unwrap();
        assert_eq!(network.connections[4 * grid.count() + 4], 0);
        let mut meter = Meter {
            used: 0,
            limit: 100000,
            cancelled: &|| false,
        };
        assert_eq!(
            components(grid, &network.connections, &mut meter)
                .unwrap()
                .1,
            1
        );
        assert_eq!(
            plan(grid, &stations, &blocked, 9, 100000, || true),
            Err(Failure::Cancelled)
        );
        assert_eq!(
            plan(grid, &stations, &blocked, 9, 1, || false),
            Err(Failure::WorkLimit)
        );
        let polls = std::cell::Cell::new(0);
        assert_eq!(
            plan(grid, &stations, &blocked, 9, 100000, || {
                polls.set(polls.get() + 1);
                polls.get() > 15
            }),
            Err(Failure::Cancelled)
        );
        assert_eq!(blocked, before);
        for x in 0..grid.count() {
            blocked[4 * grid.count() + x] = true;
        }
        assert_eq!(
            plan(grid, &stations, &blocked, 9, 100000, || false),
            Err(Failure::Disconnected)
        );
    }

    #[test]
    fn invalid_station_sockets_and_empty_networks_are_explicit() {
        let grid = Grid::new(1000., 216.).unwrap();
        let blocked = vec![false; grid.count() * grid.count()];
        assert!(
            plan(grid, &[], &blocked, 0, 1000, || false)
                .unwrap()
                .connections
                .iter()
                .all(|&mask| mask == 0)
        );
        let station = Station {
            cell: [0, 0],
            connections: [false, true, false, false],
        };
        assert!(matches!(
            plan(grid, &[station], &blocked, 0, 1000, || false),
            Err(Failure::Invalid(_))
        ));
        let station = Station {
            cell: [1, 1],
            connections: [true, true, false, false],
        };
        assert!(matches!(
            plan(grid, &[station.clone(), station], &blocked, 0, 1000, || {
                false
            }),
            Err(Failure::Invalid(_))
        ));
    }
}
