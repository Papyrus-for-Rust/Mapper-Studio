//! Authored station selection and entrance-grid placement constraints.
use crate::{
    environment::{Box3, Transform, overlap},
    tunnel_grid::{Cell, Direction, Grid},
    tunnel_links::End,
    tunnel_network::{Failure, Meter},
};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug)]
pub struct Snap {
    pub radius: f32,
    pub height: f32,
}
impl Snap {
    pub fn position(self, position: [f32; 3]) -> Result<[f32; 3], String> {
        if ![self.radius, self.height]
            .iter()
            .all(|v| v.is_finite() && (0.001..=4096.).contains(v))
            || position.iter().any(|v| !v.is_finite() || v.abs() > 100000.)
        {
            return Err("Invalid tunnel entrance snap inputs".into());
        }
        Ok([
            (position[0] / self.radius).round_ties_even() * self.radius,
            (position[1] / self.height).ceil() * self.height,
            (position[2] / self.radius).round_ties_even() * self.radius,
        ])
    }
}

/// The placement check uses DungeonVolume.GetBounds with an identity instance rotation.
pub fn valid_spawn(grid: Grid, volume: Box3, position: [f32; 3]) -> Result<bool, String> {
    volume.validate()?;
    let centre = std::array::from_fn(|i| volume.center[i] + position[i]);
    let cell = grid.world(grid.cell(centre)?)?;
    Ok((centre[0] - cell[0]).abs() > 3. || (centre[2] - cell[2]).abs() > 3.)
}

#[derive(Clone, Debug)]
pub struct Station {
    pub cell: Cell,
    /// DungeonVolume.GetBounds in the zero-position, identity-rotation frame.
    pub volume: Box3,
    pub up_position: [f32; 3],
}
#[derive(Clone, Copy, Debug)]
pub struct Entrance {
    pub end: End,
    pub connector: [f32; 3],
    pub volume: Box3,
}
pub struct Request<'a> {
    pub grid: Grid,
    pub stations: &'a [Station],
    pub entrances: &'a [Entrance],
    pub blocked: &'a [bool],
    pub seed: u64,
    pub maximum_work: usize,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Selection {
    pub entrance: usize,
    pub station: usize,
    pub cell: [usize; 2],
    pub position: [f32; 3],
}
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub selections: Vec<Selection>,
    pub work: usize,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    Invalid(String),
    Work(Failure),
    Occupied { entrance: usize, cell: [usize; 2] },
    MissingStation { entrance: usize, cell: [usize; 2] },
}
impl From<Failure> for Error {
    fn from(value: Failure) -> Self {
        Self::Work(value)
    }
}

fn validate(position: [f32; 3], rotation: [f32; 4]) -> Result<(), Error> {
    Transform {
        position,
        rotation,
        scale: [1.; 3],
    }
    .validate()
    .map_err(Error::Invalid)
}

pub fn select(request: Request<'_>, cancelled: impl Fn() -> bool) -> Result<Plan, Error> {
    let n = request.grid.count();
    if request.stations.is_empty()
        || request.stations.len() > 4096
        || request.entrances.is_empty()
        || request.entrances.len() > 1024
        || request.blocked.len() != n * n
        || !(1..=128_000_000).contains(&request.maximum_work)
    {
        return Err(Error::Invalid("Invalid tunnel station inputs".into()));
    }
    let mut meter = Meter {
        used: 0,
        limit: request.maximum_work,
        cancelled: &cancelled,
    };
    let mut ids = BTreeSet::new();
    for station in request.stations {
        meter.spend(1 + station.cell.avoid_neighbours.len())?;
        station.volume.validate().map_err(Error::Invalid)?;
        validate(station.up_position, [0., 0., 0., 1.])?;
        if station.cell.id == 0
            || !ids.insert(station.cell.id)
            || station.cell.avoid_neighbours.len() > 256
        {
            return Err(Error::Invalid("Invalid station module identity".into()));
        }
    }
    for entrance in request.entrances {
        meter.spend(1)?;
        entrance.volume.validate().map_err(Error::Invalid)?;
        validate(entrance.end.pose.position, entrance.end.pose.rotation)?;
        validate(
            entrance.end.socket.pose.position,
            entrance.end.socket.pose.rotation,
        )?;
        validate(entrance.connector, [0., 0., 0., 1.])?;
    }
    meter.spend(n * n)?;
    let mut chosen: Vec<Option<usize>> = vec![None; n * n];
    let mut selections = Vec::new();
    let mut pool: Vec<_> = (0..request.stations.len()).collect();
    let mut random = request.seed;
    for (index, entrance) in request.entrances.iter().enumerate() {
        meter.spend(1)?;
        let cell = request
            .grid
            .cell(entrance.connector)
            .map_err(Error::Invalid)?;
        let location = cell[1] * n + cell[0];
        if request.blocked[location] || chosen[location].is_some() {
            return Err(Error::Occupied {
                entrance: index,
                cell,
            });
        }
        let position = request.grid.world(cell).map_err(Error::Invalid)?;
        let constraints = Direction::ALL.map(|side| match request.grid.neighbour(cell, side) {
            None => Some(0),
            Some([x, z]) => match chosen[z * n + x] {
                Some(other) => {
                    Some(request.stations[other].cell.connections[side.opposite() as usize])
                }
                None if request.blocked[z * n + x] => Some(0),
                None => None,
            },
        });
        let mut entrance_volume = entrance.volume;
        entrance_volume.center =
            std::array::from_fn(|i| entrance_volume.center[i] + entrance.end.pose.position[i]);
        meter.spend(pool.len())?;
        for i in (1..pool.len()).rev() {
            let j = (crate::decor::random(&mut random) % (i as u64 + 1)) as usize;
            pool.swap(i, j);
        }
        let mut best = None;
        let mut nearest = f32::MAX;
        for &candidate in &pool {
            meter.spend(1)?;
            let station = &request.stations[candidate];
            if station
                .cell
                .connections
                .into_iter()
                .zip(constraints)
                .any(|(kind, wanted)| wanted.is_some_and(|v| kind != v))
            {
                continue;
            }
            let mut a = station.volume;
            a.center = std::array::from_fn(|i| a.center[i] + position[i]);
            let mut b = entrance_volume;
            // OBB.Intersects2D retains both rotations and moves the target centre onto this Y.
            b.center[1] = a.center[1];
            if overlap(a, b, 0.) {
                continue;
            }
            let socket = [
                position[0] + station.up_position[0],
                position[2] + station.up_position[2],
            ];
            let distance = ((entrance.connector[0] - socket[0]).powi(2)
                + (entrance.connector[2] - socket[1]).powi(2))
            .sqrt();
            if nearest < distance {
                continue;
            }
            nearest = distance;
            best = Some(candidate);
        }
        let station = best.ok_or(Error::MissingStation {
            entrance: index,
            cell,
        })?;
        chosen[location] = Some(station);
        selections.push(Selection {
            entrance: index,
            station,
            cell,
            position,
        });
    }
    meter.spend(1)?;
    Ok(Plan {
        selections,
        work: meter.used,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{dungeon_base::Pose, tunnel_links::Socket};
    use std::cell::Cell as Counter;

    fn volume(center: [f32; 3], extents: [f32; 3]) -> Box3 {
        Box3 {
            center,
            extents,
            rotation: [0., 0., 0., 1.],
        }
    }
    fn station(id: u32, z: f32) -> Station {
        Station {
            cell: Cell {
                id,
                connections: [0, 0, 1, 1],
                variants: [0; 4],
                avoid_neighbours: Vec::new(),
            },
            volume: volume([0., 18., z], [40., 4., 3.]),
            up_position: [0., 18., z],
        }
    }
    fn entrance(position: [f32; 3]) -> Entrance {
        Entrance {
            end: End {
                socket: Socket {
                    kind: 1,
                    pose: Pose {
                        position: [0., -6., 0.],
                        rotation: [0., 0., 0., 1.],
                    },
                },
                pose: Pose {
                    position,
                    rotation: [0., 0., 0., 1.],
                },
            },
            connector: position,
            volume: volume([0.; 3], [2.; 3]),
        }
    }
    fn request<'a>(
        stations: &'a [Station],
        entrances: &'a [Entrance],
        blocked: &'a [bool],
    ) -> Request<'a> {
        Request {
            grid: Grid::new(1000., 100.).unwrap(),
            stations,
            entrances,
            blocked,
            seed: 9,
            maximum_work: 10000,
        }
    }

    #[test]
    fn snap_uses_even_ties_upward_height_and_spawn_check_uses_volume_centre() {
        let snap = Snap {
            radius: 3.,
            height: 1.5,
        };
        assert_eq!(snap.position([1.5, -1.6, -4.5]).unwrap(), [0., -1.5, -6.]);
        assert!(snap.position([f32::NAN, 0., 0.]).is_err());
        let grid = Grid::new(1000., 100.).unwrap();
        let centre = grid.world([4, 4]).unwrap();
        assert!(!valid_spawn(grid, volume([3., 100., 3.], [2.; 3]), centre).unwrap());
        assert!(valid_spawn(grid, volume([3.01, 100., 3.], [2.; 3]), centre).unwrap());
    }

    #[test]
    fn nearest_socket_wins_and_replays_independently_of_entrance_height() {
        let stations = [station(1, -10.), station(2, 10.)];
        let mut entrances = [entrance([-30., 500., -30.])];
        let blocked = vec![false; 100];
        let first = select(request(&stations, &entrances, &blocked), || false).unwrap();
        assert_eq!(first.selections[0].station, 1);
        assert_eq!(first.selections[0].cell, [4, 4]);
        assert_eq!(
            select(request(&stations, &entrances, &blocked), || false),
            Ok(first.clone())
        );
        entrances[0].end.pose.position[1] = -500.;
        entrances[0].connector[1] = -500.;
        assert_eq!(
            select(request(&stations, &entrances, &blocked), || false),
            Ok(first)
        );
    }

    #[test]
    fn overlapping_volume_loses_even_if_its_socket_is_closer() {
        let mut stations = [station(1, 20.), station(2, -10.)];
        let entrances = [entrance([-30., 500., -30.])];
        let blocked = vec![false; 100];
        let result = select(request(&stations, &entrances, &blocked), || false).unwrap();
        assert_eq!(result.selections[0].station, 1);
        stations[1].volume = stations[0].volume;
        assert!(matches!(
            select(request(&stations, &entrances, &blocked), || false),
            Err(Error::MissingStation { .. })
        ));
    }

    #[test]
    fn edges_blocked_exits_and_previously_chosen_station_types_constrain_selection() {
        let mut stations = [station(1, -10.), station(2, 10.)];
        stations[1].cell.connections = [1, 1, 0, 0];
        let blocked = vec![false; 100];
        let entrances = [entrance([-430., 0., -30.])];
        let result = select(request(&stations, &entrances, &blocked), || false).unwrap();
        assert_eq!(result.selections[0].station, 1);
        let entrances = [entrance([-30., 0., -30.]), entrance([70., 0., -30.])];
        let mut stations = [station(1, 10.), station(2, -10.)];
        stations[1].cell.connections[2] = 2;
        let result = select(request(&stations, &entrances, &blocked), || false).unwrap();
        assert_eq!(result.selections[1].station, 0);
        let mut blocked = blocked;
        blocked[4 * 10 + 5] = true;
        assert!(matches!(
            select(request(&stations, &entrances[..1], &blocked), || false),
            Err(Error::MissingStation { .. })
        ));
    }

    #[test]
    fn occupied_cells_and_exhaustion_never_publish_a_partial_plan() {
        let stations = [station(1, -10.)];
        let entrances = [entrance([-30., 0., -30.]), entrance([-30., 0., -30.])];
        let blocked = vec![false; 100];
        assert!(matches!(
            select(request(&stations, &entrances, &blocked), || false),
            Err(Error::Occupied { entrance: 1, .. })
        ));
        let complete = select(request(&stations, &entrances[..1], &blocked), || false).unwrap();
        let mut limited = request(&stations, &entrances[..1], &blocked);
        limited.maximum_work = complete.work - 1;
        assert_eq!(
            select(limited, || false),
            Err(Error::Work(Failure::WorkLimit))
        );
        let calls = Counter::new(0);
        select(request(&stations, &entrances[..1], &blocked), || {
            calls.set(calls.get() + 1);
            false
        })
        .unwrap();
        let last = calls.get();
        calls.set(0);
        assert_eq!(
            select(request(&stations, &entrances[..1], &blocked), || {
                calls.set(calls.get() + 1);
                calls.get() == last
            }),
            Err(Error::Work(Failure::Cancelled))
        );
    }
}
