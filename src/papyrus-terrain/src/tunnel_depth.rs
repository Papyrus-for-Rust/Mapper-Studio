//! Shared-grid burial using authored volume predicates and world clearance.
use crate::{
    environment::{Box3, Verdict, World, rotate},
    path_terrain::{self, Grid, SHORT_TO_FLOAT},
    source_stamp_raster::Quad,
    tunnel_layout::{Placement, Role},
    tunnel_network::{Failure, Meter},
};
use std::{collections::BTreeMap, ops::Range};

#[derive(Clone, Copy, Debug)]
pub struct Volume {
    pub mask: u32,
    pub geometry: Box3,
}

#[derive(Clone, Copy, Debug)]
pub struct Policy {
    pub underground: u32,
    pub train_tunnels: u32,
    pub entrance: u32,
    pub forbidden: u32,
    pub altitude: f32,
    pub terrain_raise: f32,
    pub collision_raise: f32,
    pub step: f32,
    pub maximum_drop: f32,
}

pub struct Request<'a> {
    pub placements: &'a [Placement],
    pub geometry: &'a BTreeMap<u32, Vec<Volume>>,
    pub terrain: Option<Grid<'a>>,
    pub world: &'a World,
    pub policy: Policy,
    pub maximum_work: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Depth {
    pub vertical_offset: f32,
    pub work: usize,
    pub terrain_samples: usize,
    pub passes: usize,
    pub module_checks: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    Invalid(String),
    Work(Failure),
    World(Verdict),
    DepthLimit,
}
impl From<Failure> for Error {
    fn from(value: Failure) -> Self {
        Self::Work(value)
    }
}

struct Prepared {
    mask: u32,
    geometry: Box3,
    base_y: f32,
    minimum_height: Option<Option<f32>>,
}
impl Prepared {
    fn at(&self, offset: f32, raise: f32) -> Box3 {
        let mut geometry = self.geometry;
        geometry.center[1] += (self.base_y + offset) + raise;
        geometry
    }
}

fn minimum_height(
    terrain: Grid<'_>,
    geometry: Box3,
    meter: &mut Meter<'_>,
    samples: &mut usize,
) -> Result<Option<f32>, Error> {
    let corners = [[-1., -1.], [1., -1.], [-1., 1.], [1., 1.]].map(|[x, z]| {
        let p = rotate(
            geometry.rotation,
            [x * geometry.extents[0], 0., z * geometry.extents[2]],
        );
        [geometry.center[0] + p[0], geometry.center[2] + p[2]]
    });
    let quad = Quad::from_world(
        terrain.resolution,
        [-terrain.size * 0.5; 2],
        [1. / terrain.size; 2],
        corners,
    )
    .map_err(Error::Invalid)?;
    let [min_x, max_x, min_z, max_z] = quad.bounds();
    let mut minimum: Option<i16> = None;
    for z in min_z..max_z {
        meter.spend(max_x - min_x)?;
        for x in min_x..max_x {
            if quad.contains(x, z) {
                let value = terrain.height[z * terrain.resolution + x];
                minimum = Some(minimum.map_or(value, |old| old.min(value)));
                *samples += 1;
            }
        }
    }
    Ok(minimum
        .map(|value| terrain.origin_y + (value as f32 * SHORT_TO_FLOAT) * terrain.vertical_span))
}

fn valid(
    volumes: &mut [Prepared],
    offset: f32,
    request: &Request<'_>,
    meter: &mut Meter<'_>,
    samples: &mut usize,
) -> Result<bool, Error> {
    let policy = request.policy;
    let mut below = true;
    for volume in volumes.iter() {
        meter.spend(1)?;
        if volume.mask & policy.underground != 0
            && volume.mask & policy.entrance == 0
            && volume.at(offset, policy.terrain_raise).bounds().maximum[1] as f32 >= policy.altitude
        {
            below = false;
            break;
        }
    }
    if !below && let Some(terrain) = request.terrain {
        for volume in volumes.iter_mut() {
            meter.spend(1)?;
            if volume.mask & policy.train_tunnels == 0 || volume.mask & policy.entrance != 0 {
                continue;
            }
            let minimum = if let Some(minimum) = volume.minimum_height {
                minimum
            } else {
                let minimum = minimum_height(terrain, volume.geometry, meter, samples)?;
                volume.minimum_height = Some(minimum);
                minimum
            };
            let top = volume.at(offset, policy.terrain_raise).bounds().maximum[1] as f32;
            if minimum.is_some_and(|height| height <= top) {
                return Ok(false);
            }
        }
    }
    for raise in [policy.collision_raise, 0.] {
        for volume in volumes.iter() {
            let remaining = meter.limit.saturating_sub(meter.used);
            if remaining == 0 {
                return Err(Failure::WorkLimit.into());
            }
            let assessment = request
                .world
                .query(
                    volume.at(offset, raise),
                    policy.forbidden,
                    0.,
                    remaining,
                    meter.cancelled,
                )
                .map_err(Error::Invalid)?;
            meter.spend(assessment.work)?;
            match assessment.verdict {
                Verdict::Clear => {}
                Verdict::Blocked(_) | Verdict::Unresolved(_) => return Ok(false),
                Verdict::Budget => return Err(Failure::WorkLimit.into()),
                Verdict::Cancelled => return Err(Failure::Cancelled.into()),
                other => return Err(Error::World(other)),
            }
        }
    }
    Ok(true)
}

pub fn place(request: Request<'_>, cancelled: impl Fn() -> bool) -> Result<Depth, Error> {
    place_from(request, 0., cancelled)
}

/// Recheck the whole grid from an existing offset after entrance-chain lowering.
pub fn place_from(
    request: Request<'_>,
    initial_offset: f32,
    cancelled: impl Fn() -> bool,
) -> Result<Depth, Error> {
    validate(&request, initial_offset)?;
    let p = request.policy;
    let mut meter = Meter {
        used: 0,
        limit: request.maximum_work,
        cancelled: &cancelled,
    };
    let (mut queries, groups) = prepare(&request, &mut meter)?;
    let mut result = Depth {
        vertical_offset: initial_offset,
        work: 0,
        terrain_samples: 0,
        passes: 0,
        module_checks: 0,
    };
    loop {
        meter.spend(1)?;
        result.passes += 1;
        let before = result.vertical_offset;
        for group in &groups {
            loop {
                meter.spend(1)?;
                result.module_checks += 1;
                if valid(
                    &mut queries[group.clone()],
                    result.vertical_offset,
                    &request,
                    &mut meter,
                    &mut result.terrain_samples,
                )? {
                    break;
                }
                let next = result.vertical_offset - p.step;
                if next == result.vertical_offset || next < -p.maximum_drop {
                    return Err(Error::DepthLimit);
                }
                result.vertical_offset = next;
            }
        }
        if result.vertical_offset == before {
            break;
        }
    }
    meter.spend(1)?;
    result.work = meter.used;
    Ok(result)
}

fn validate(request: &Request<'_>, initial_offset: f32) -> Result<(), Error> {
    let p = request.policy;
    if ![p.underground, p.train_tunnels, p.entrance]
        .iter()
        .all(|mask| mask.is_power_of_two())
        || p.underground == p.train_tunnels
        || p.underground == p.entrance
        || p.train_tunnels == p.entrance
        || !p.altitude.is_finite()
        || p.altitude.abs() > 10000.
        || ![p.terrain_raise, p.collision_raise]
            .iter()
            .all(|value| value.is_finite() && (0. ..=1000.).contains(value))
        || !p.step.is_finite()
        || !(0.001..=1000.).contains(&p.step)
        || !p.maximum_drop.is_finite()
        || !(0. ..=10000.).contains(&p.maximum_drop)
        || !initial_offset.is_finite()
        || initial_offset > 0.
        || initial_offset < -p.maximum_drop
        || !(1..=128_000_000).contains(&request.maximum_work)
    {
        return Err(Error::Invalid("Invalid tunnel depth policy".into()));
    }
    if let Some(terrain) = request.terrain {
        if terrain.height.len() != terrain.resolution.saturating_mul(terrain.resolution) {
            return Err(Error::Invalid("Invalid tunnel depth heightmap".into()));
        }
        path_terrain::encoded_height_quantum(path_terrain::Layout {
            size: terrain.size,
            resolution: terrain.resolution,
            origin_y: terrain.origin_y,
            vertical_span: terrain.vertical_span,
        })
        .map_err(Error::Invalid)?;
    }
    Ok(())
}
fn prepare(
    request: &Request<'_>,
    meter: &mut Meter<'_>,
) -> Result<(Vec<Prepared>, Vec<Range<usize>>), Error> {
    let mut queries = Vec::new();
    let mut groups: Vec<Range<usize>> = Vec::new();
    for placement in request.placements {
        meter.spend(1)?;
        if matches!(placement.role, Role::Transition(_)) {
            continue;
        }
        if groups.len() >= 65536 {
            return Err(Error::Invalid(
                "Tunnel grid exceeds its supported allocation".into(),
            ));
        }
        if placement
            .position
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 100000.)
        {
            return Err(Error::Invalid("Invalid tunnel depth placement".into()));
        }
        let volumes = request
            .geometry
            .get(&placement.prefab_id)
            .ok_or_else(|| Error::Invalid("Missing authored tunnel environment geometry".into()))?;
        let start = queries.len();
        for volume in volumes {
            meter.spend(1)?;
            if queries.len() >= 131072 {
                return Err(Error::Invalid(
                    "Tunnel environment geometry exceeds its allocation".into(),
                ));
            }
            volume.geometry.validate().map_err(Error::Invalid)?;
            let mut geometry = volume.geometry;
            geometry.center[0] += placement.position[0];
            geometry.center[2] += placement.position[2];
            queries.push(Prepared {
                mask: volume.mask,
                geometry,
                base_y: placement.position[1],
                minimum_height: None,
            });
        }
        groups.push(start..queries.len());
    }
    Ok((queries, groups))
}

#[derive(Clone, Debug, PartialEq)]
pub struct Assessment {
    pub clear: bool,
    pub work: usize,
    pub terrain_samples: usize,
}

/// Evaluate fixed prefab roots with the shared burial/clearance predicates.
/// Upward-portal selection supplies already buried roots and requires a verdict
/// at those exact positions, retaining the consumed work on rejection.
pub fn check(request: Request<'_>, cancelled: impl Fn() -> bool) -> Result<Assessment, Error> {
    validate(&request, 0.)?;
    let mut meter = Meter {
        used: 0,
        limit: request.maximum_work,
        cancelled: &cancelled,
    };
    let (mut queries, groups) = prepare(&request, &mut meter)?;
    let mut samples = 0;
    for group in groups {
        meter.spend(1)?;
        if !valid(&mut queries[group], 0., &request, &mut meter, &mut samples)? {
            return Ok(Assessment {
                clear: false,
                work: meter.used,
                terrain_samples: samples,
            });
        }
    }
    meter.spend(1)?;
    Ok(Assessment {
        clear: true,
        work: meter.used,
        terrain_samples: samples,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::{Coverage, Key, Shape, Volume as WorldVolume};

    fn policy() -> Policy {
        Policy {
            underground: 1,
            train_tunnels: 32,
            entrance: 4096,
            forbidden: 3,
            altitude: 0.,
            terrain_raise: 9.,
            collision_raise: 10.,
            step: 3.,
            maximum_drop: 120.,
        }
    }
    fn geometry() -> BTreeMap<u32, Vec<Volume>> {
        BTreeMap::from([(
            1,
            vec![Volume {
                mask: 33,
                geometry: Box3 {
                    center: [0.; 3],
                    rotation: [0., 0., 0., 1.],
                    extents: [1.; 3],
                },
            }],
        )])
    }
    fn placements() -> Vec<Placement> {
        vec![Placement {
            prefab_id: 1,
            cell: [0, 0],
            position: [0.; 3],
            role: Role::Tunnel,
        }]
    }
    fn world(volumes: Vec<WorldVolume>, coverage: Coverage) -> World {
        World::build(volumes, coverage, 100000, || false)
            .unwrap()
            .unwrap()
    }
    fn terrain(height: &[i16]) -> Grid<'_> {
        Grid {
            size: 1000.,
            resolution: 5,
            origin_y: 0.,
            vertical_span: 1000.,
            height,
        }
    }

    #[test]
    fn fixed_root_checks_keep_rejection_work_and_agree_with_burial_at_accepted_depth() {
        let mut geometry = geometry();
        geometry.get_mut(&1).unwrap()[0].geometry.extents = [200., 1., 200.];
        let placements = placements();
        let world = world(Vec::new(), Coverage::Complete);
        let height = vec![0; 25];
        let request = |placements| Request {
            placements,
            geometry: &geometry,
            terrain: Some(terrain(&height)),
            world: &world,
            policy: policy(),
            maximum_work: 100000,
        };
        let fixed = check(request(&placements), || false).unwrap();
        assert!(!fixed.clear);
        assert!(fixed.work > 0 && fixed.terrain_samples > 0);
        let buried = place(request(&placements), || false).unwrap();
        assert!(buried.vertical_offset < 0.);
        let mut moved = placements.clone();
        moved[0].position[1] = buried.vertical_offset;
        assert!(check(request(&moved), || false).unwrap().clear);
        assert_eq!(placements[0].position[1], 0.);
    }
    #[test]
    fn fixed_root_clearance_budget_and_unknown_world_cannot_publish_clearance() {
        let geometry = geometry();
        let mut placements = placements();
        placements[0].position[1] = -12.;
        let obstacle = WorldVolume {
            key: Key {
                instance: 1,
                component: 1,
            },
            mask: 1,
            shape: Shape::Box(Box3 {
                center: [0., -2., 0.],
                rotation: [0., 0., 0., 1.],
                extents: [2.; 3],
            }),
        };
        let blocked = world(vec![obstacle], Coverage::Complete);
        let request = |world, maximum_work| Request {
            placements: &placements,
            geometry: &geometry,
            terrain: None,
            world,
            policy: policy(),
            maximum_work,
        };
        assert!(!check(request(&blocked, 10000), || false).unwrap().clear);
        let incomplete = world(Vec::new(), Coverage::Incomplete);
        assert!(matches!(
            check(request(&incomplete, 10000), || false),
            Err(Error::World(Verdict::Incomplete))
        ));
        assert!(matches!(
            check(request(&blocked, 1), || false),
            Err(Error::Work(Failure::WorkLimit))
        ));
        assert!(matches!(
            check(request(&blocked, 10000), || true),
            Err(Error::Work(Failure::Cancelled))
        ));
        assert_eq!(placements[0].position[1], -12.);
    }
    #[test]
    fn terrain_raster_is_cached_and_altitude_is_an_alternative() {
        let mut geometry = geometry();
        geometry.get_mut(&1).unwrap()[0].geometry.extents[0] = 200.;
        geometry.get_mut(&1).unwrap()[0].geometry.extents[2] = 200.;
        let placements = placements();
        let world = world(Vec::new(), Coverage::Complete);
        let mut height = vec![1000; 25];
        height[12] = 164;
        let result = place(
            Request {
                placements: &placements,
                geometry: &geometry,
                terrain: Some(terrain(&height)),
                world: &world,
                policy: policy(),
                maximum_work: 100000,
            },
            || false,
        )
        .unwrap();
        assert_eq!(result.vertical_offset, -6.);
        assert_eq!(result.terrain_samples, 9);
        geometry.get_mut(&1).unwrap()[0].geometry.center[1] = -1.;
        height.fill(-3276);
        let result = place(
            Request {
                placements: &placements,
                geometry: &geometry,
                terrain: Some(terrain(&height)),
                world: &world,
                policy: policy(),
                maximum_work: 100000,
            },
            || false,
        )
        .unwrap();
        assert_eq!(result.vertical_offset, -12.);
    }

    #[test]
    fn later_lowering_rechecks_previously_valid_modules() {
        let geometry = geometry();
        let mut placements = placements();
        placements[0].position[0] = -250.;
        placements.push(Placement {
            position: [250., 0., 0.],
            ..placements[0].clone()
        });
        let world = world(
            vec![WorldVolume {
                key: Key {
                    instance: 1,
                    component: 1,
                },
                mask: 2,
                shape: Shape::Box(Box3 {
                    center: [-250., -12., 0.],
                    rotation: [0., 0., 0., 1.],
                    extents: [1.; 3],
                }),
            }],
            Coverage::Complete,
        );
        let mut height = vec![1000; 25];
        height[13] = 0;
        let result = place(
            Request {
                placements: &placements,
                geometry: &geometry,
                terrain: Some(terrain(&height)),
                world: &world,
                policy: policy(),
                maximum_work: 100000,
            },
            || false,
        )
        .unwrap();
        assert_eq!(result.vertical_offset, -15.);
        assert_eq!(result.passes, 3);
    }

    #[test]
    fn unresolved_world_cancellation_and_resource_limits_cannot_publish_depth() {
        let geometry = geometry();
        let placements = placements();
        let height = vec![0; 25];
        let world = world(Vec::new(), Coverage::Incomplete);
        let request = |maximum_work| Request {
            placements: &placements,
            geometry: &geometry,
            terrain: Some(terrain(&height)),
            world: &world,
            policy: policy(),
            maximum_work,
        };
        assert_eq!(
            place(request(100000), || false),
            Err(Error::World(Verdict::Incomplete))
        );
        assert_eq!(
            place(request(100000), || true),
            Err(Error::Work(Failure::Cancelled))
        );
        assert_eq!(
            place(request(1), || false),
            Err(Error::Work(Failure::WorkLimit))
        );
        let mut limited = request(100000);
        limited.policy.maximum_drop = 3.;
        assert_eq!(place(limited, || false), Err(Error::DepthLimit));
    }

    #[test]
    fn bounded_unknown_colliders_can_be_cleared_by_lowering() {
        let geometry = geometry();
        let placements = placements();
        let height = vec![0; 25];
        let obstacle = Box3 {
            center: [0., -12., 0.],
            rotation: [0., 0., 0., 1.],
            extents: [1.; 3],
        };
        let world = world(
            vec![WorldVolume {
                key: Key {
                    instance: 1,
                    component: 1,
                },
                mask: 2,
                shape: Shape::Unresolved {
                    bounds: Some(obstacle.bounds()),
                },
            }],
            Coverage::Complete,
        );
        let result = place(
            Request {
                placements: &placements,
                geometry: &geometry,
                terrain: Some(terrain(&height)),
                world: &world,
                policy: policy(),
                maximum_work: 100000,
            },
            || false,
        )
        .unwrap();
        assert_eq!(result.vertical_offset, -15.);
    }
}
