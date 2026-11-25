//! Shared burial and entrance-chain retries with atomic publication and one budget.
use crate::{
    dungeon_base::Pose,
    tunnel_depth::{self, Depth},
    tunnel_layout::Role,
    tunnel_links::{self, Connection, End, Module, Policy, Socket},
    tunnel_network::{Failure, Meter},
};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug)]
pub struct Entrance {
    pub station_cell: [usize; 2],
    pub entrance: End,
    pub station_socket: Socket,
}
pub struct Request<'a> {
    pub burial: tunnel_depth::Request<'a>,
    pub initial_offset: f32,
    pub entrances: &'a [Entrance],
    pub modules: &'a [Module],
    pub policy: Policy,
    pub seed: u64,
    pub maximum_segments: usize,
}
#[derive(Clone, Debug, PartialEq)]
pub struct LinkedEntrance {
    pub station_cell: [usize; 2],
    pub connection: Connection,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Assembly {
    pub vertical_offset: f32,
    pub entrances: Vec<LinkedEntrance>,
    pub work: usize,
    pub attempts: usize,
    pub terrain_samples: usize,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    Invalid(&'static str),
    Work(Failure),
    Burial(tunnel_depth::Error),
    Link(tunnel_links::Error),
    DepthLimit {
        station_cell: [usize; 2],
        gap: f32,
        angle: f32,
        types: [u8; 2],
    },
}
impl From<Failure> for Error {
    fn from(value: Failure) -> Self {
        Self::Work(value)
    }
}

pub fn assemble(request: Request<'_>, cancelled: impl Fn() -> bool) -> Result<Assembly, Error> {
    let mut burial = request.burial;
    if !(1..=128_000_000).contains(&burial.maximum_work)
        || request.entrances.is_empty()
        || request.entrances.len() > 65536
        || request.maximum_segments > 131072
    {
        return Err(Error::Invalid("Invalid tunnel entrance assembly limits"));
    }
    let mut meter = Meter {
        used: 0,
        limit: burial.maximum_work,
        cancelled: &cancelled,
    };
    let mut stations = BTreeMap::new();
    for placement in burial.placements {
        meter.spend(1)?;
        if placement.role == Role::Station && placement.cell.iter().any(|&n| n >= 256) {
            return Err(Error::Invalid(
                "Tunnel station exceeds its supported lattice",
            ));
        }
        if placement.role == Role::Station
            && stations
                .insert(placement.cell, placement.position)
                .is_some()
        {
            return Err(Error::Invalid("Duplicate tunnel station"));
        }
    }
    if stations.len() != request.entrances.len() {
        return Err(Error::Invalid(
            "Every tunnel station requires an entrance binding",
        ));
    }
    let mut roots = Vec::new();
    for entrance in request.entrances {
        meter.spend(1)?;
        let position = stations
            .remove(&entrance.station_cell)
            .ok_or(Error::Invalid(
                "Missing or duplicated entrance station binding",
            ))?;
        roots.push((entrance, position));
    }
    let mut offset = request.initial_offset;
    let mut attempts = 0;
    let mut terrain_samples = 0;
    loop {
        meter.spend(1)?;
        attempts += 1;
        burial.maximum_work = meter.limit - meter.used;
        if burial.maximum_work == 0 {
            return Err(Failure::WorkLimit.into());
        }
        let Depth {
            vertical_offset,
            work,
            terrain_samples: samples,
            ..
        } = tunnel_depth::place_from(tunnel_depth::Request { ..burial }, offset, &cancelled)
            .map_err(Error::Burial)?;
        meter.spend(work)?;
        terrain_samples += samples;
        offset = vertical_offset;
        let mut connections = Vec::new();
        let mut segments = 0;
        let mut failed = None;
        for (entrance, position) in &roots {
            meter.spend(1)?;
            let remaining = meter.limit - meter.used;
            if remaining == 0 {
                return Err(Failure::WorkLimit.into());
            }
            let mut position = *position;
            position[1] += offset;
            // Bind randomness to the station cell so retry order cannot change another entrance.
            let seed = request.seed
                ^ ((entrance.station_cell[0] as u64) << 32)
                ^ entrance.station_cell[1] as u64;
            let result = tunnel_links::assemble(
                tunnel_links::Request {
                    entrance: entrance.entrance,
                    station: End {
                        socket: entrance.station_socket,
                        pose: Pose {
                            position,
                            rotation: [0., 0., 0., 1.],
                        },
                    },
                    modules: request.modules,
                    policy: request.policy,
                    seed,
                    maximum_work: remaining,
                    maximum_segments: request.maximum_segments - segments,
                },
                &cancelled,
            );
            match result {
                Ok(connection) => {
                    meter.spend(connection.work)?;
                    segments += connection.downwards.len() + connection.upwards.len();
                    connections.push(LinkedEntrance {
                        station_cell: entrance.station_cell,
                        connection,
                    });
                }
                Err(tunnel_links::Error::Disconnected {
                    gap,
                    angle,
                    types,
                    work,
                }) => {
                    meter.spend(work)?;
                    failed = Some((entrance.station_cell, gap, angle, types));
                    break;
                }
                Err(error) => return Err(Error::Link(error)),
            }
        }
        if let Some((station_cell, gap, angle, types)) = failed {
            let next = offset - burial.policy.step;
            if next == offset || next < -burial.policy.maximum_drop {
                return Err(Error::DepthLimit {
                    station_cell,
                    gap,
                    angle,
                    types,
                });
            }
            offset = next;
        } else {
            meter.spend(1)?;
            return Ok(Assembly {
                vertical_offset: offset,
                entrances: connections,
                work: meter.used,
                attempts,
                terrain_samples,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        environment::{Box3, Coverage, Key, Shape, Volume, World},
        path_terrain::Grid,
        tunnel_depth::Volume as BurialVolume,
        tunnel_layout::Placement,
    };
    use std::cell::Cell;

    fn pose(position: [f32; 3]) -> Pose {
        Pose {
            position,
            rotation: [0., 0., 0., 1.],
        }
    }
    fn socket(kind: u8, position: [f32; 3]) -> Socket {
        Socket {
            kind,
            pose: pose(position),
        }
    }
    fn run(
        world: &World,
        maximum_drop: f32,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Assembly, Error> {
        let placements = [
            Placement {
                prefab_id: 1,
                cell: [0, 0],
                position: [0.; 3],
                role: Role::Station,
            },
            Placement {
                prefab_id: 1,
                cell: [1, 0],
                position: [216., 0., 0.],
                role: Role::Station,
            },
        ];
        let geometry = BTreeMap::from([(
            1,
            vec![BurialVolume {
                mask: 33,
                geometry: Box3 {
                    center: [0.; 3],
                    rotation: [0., 0., 0., 1.],
                    extents: [1.; 3],
                },
            }],
        )]);
        let height = [0; 4];
        let modules = [Module {
            id: 2,
            up: socket(1, [0., 9., 0.]),
            down: socket(0, [9., 0., 0.]),
            priority: 0,
            rotation_limit: 0.,
        }];
        let entrances = [
            Entrance {
                station_cell: [0, 0],
                entrance: End {
                    socket: socket(1, [0.; 3]),
                    pose: pose([0.; 3]),
                },
                station_socket: socket(0, [9., 18., 0.]),
            },
            Entrance {
                station_cell: [1, 0],
                entrance: End {
                    socket: socket(1, [0.; 3]),
                    pose: pose([216., 0., 0.]),
                },
                station_socket: socket(0, [9., 18., 0.]),
            },
        ];
        assemble(
            Request {
                burial: tunnel_depth::Request {
                    placements: &placements,
                    geometry: &geometry,
                    terrain: Some(Grid {
                        size: 1000.,
                        resolution: 2,
                        origin_y: 0.,
                        vertical_span: 1000.,
                        height: &height,
                    }),
                    world,
                    policy: tunnel_depth::Policy {
                        underground: 1,
                        train_tunnels: 32,
                        entrance: 4096,
                        forbidden: 3,
                        altitude: 0.,
                        terrain_raise: 9.,
                        collision_raise: 10.,
                        step: 3.,
                        maximum_drop,
                    },
                    maximum_work,
                },
                initial_offset: 0.,
                entrances: &entrances,
                modules: &modules,
                policy: Policy {
                    default_type: 0,
                    elevator_type: 1,
                    transition_type: 2,
                    link_radius: 3.,
                    link_transition: 9.,
                },
                seed: 3,
                maximum_segments: 100,
            },
            cancelled,
        )
    }
    fn world(volumes: Vec<Volume>) -> World {
        World::build(volumes, Coverage::Complete, 10000, || false)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn shallow_grid_is_lowered_until_every_entrance_connects() {
        let result = run(&world(Vec::new()), 60., 100000, || false).unwrap();
        assert_eq!(result.vertical_offset, -27.);
        assert_eq!(result.attempts, 6);
        assert_eq!(result.entrances.len(), 2);
        assert!(result.entrances.iter().all(|e| e.connection.gap == 0.));
        assert_eq!(run(&world(Vec::new()), 60., 100000, || false), Ok(result));
    }

    #[test]
    fn lowered_grid_is_rechecked_against_world_geometry() {
        let obstacle = Volume {
            key: Key {
                instance: 1,
                component: 1,
            },
            mask: 2,
            shape: Shape::Box(Box3 {
                center: [0., -27., 0.],
                rotation: [0., 0., 0., 1.],
                extents: [1.; 3],
            }),
        };
        assert!(matches!(
            run(&world(vec![obstacle]), 60., 100000, || false),
            Err(Error::DepthLimit { .. })
        ));
        assert!(matches!(
            run(&world(Vec::new()), 24., 100000, || false),
            Err(Error::DepthLimit { .. })
        ));
    }

    #[test]
    fn failed_retries_and_late_cancellation_share_the_work_limit() {
        let calls = Cell::new(0);
        let result = run(&world(Vec::new()), 60., 100000, || {
            calls.set(calls.get() + 1);
            false
        })
        .unwrap();
        let final_call = calls.get();
        calls.set(0);
        assert!(
            run(&world(Vec::new()), 60., 100000, || {
                calls.set(calls.get() + 1);
                calls.get() == final_call
            })
            .is_err()
        );
        assert!(run(&world(Vec::new()), 60., result.work - 1, || false).is_err());
    }
}
