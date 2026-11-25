//! Bounded two-ended assembly of authored entrance-to-station socket chains.
use crate::{
    dungeon_base::Pose,
    environment::{Transform, multiply, rotate},
    tunnel_network::{Failure, Meter},
};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Socket {
    pub kind: u8,
    /// The Transform local pose used by DungeonGridLink.
    pub pose: Pose,
}

#[derive(Clone, Debug)]
pub struct Module {
    pub id: u32,
    pub up: Socket,
    pub down: Socket,
    pub priority: i32,
    pub rotation_limit: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct End {
    pub socket: Socket,
    pub pose: Pose,
}
impl End {
    pub fn world(self) -> Socket {
        Socket {
            kind: self.socket.kind,
            pose: Pose {
                position: self.pose.point(self.socket.pose.position),
                rotation: multiply(self.pose.rotation, self.socket.pose.rotation),
            },
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Policy {
    pub default_type: u8,
    pub elevator_type: u8,
    pub transition_type: u8,
    pub link_radius: f32,
    pub link_transition: f32,
}

pub struct Request<'a> {
    pub entrance: End,
    pub station: End,
    pub modules: &'a [Module],
    pub policy: Policy,
    pub seed: u64,
    pub maximum_work: usize,
    pub maximum_segments: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub prefab_id: u32,
    pub pose: Pose,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Connection {
    pub downwards: Vec<Segment>,
    pub upwards: Vec<Segment>,
    pub work: usize,
    pub gap: f32,
    pub angle: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    Invalid(String),
    Work(Failure),
    SegmentLimit,
    Disconnected {
        gap: f32,
        angle: f32,
        types: [u8; 2],
        work: usize,
    },
}
impl From<Failure> for Error {
    fn from(value: Failure) -> Self {
        Self::Work(value)
    }
}

fn validate(pose: Pose) -> Result<(), Error> {
    Transform {
        position: pose.position,
        rotation: pose.rotation,
        scale: [1.; 3],
    }
    .validate()
    .map_err(Error::Invalid)
}
fn inverse(q: [f32; 4]) -> [f32; 4] {
    let norm = q.iter().map(|v| v * v).sum::<f32>();
    [-q[0] / norm, -q[1] / norm, -q[2] / norm, q[3] / norm]
}
fn angle(a: [f32; 4], b: [f32; 4]) -> f32 {
    let dot = a
        .iter()
        .zip(b)
        .map(|(a, b)| a * b)
        .sum::<f32>()
        .abs()
        .min(1.);
    if dot > 0.999999 {
        0.
    } else {
        dot.acos() * 2. * 180. / std::f32::consts::PI
    }
}
fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| a[i] + b[i])
}
fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}
fn magnitude(v: [f32; 3]) -> f32 {
    v.iter().map(|v| v * v).sum::<f32>().sqrt()
}
fn masked(v: [f32; 3], horizontal: bool) -> f32 {
    if horizontal { magnitude(v) } else { v[1].abs() }
}

#[derive(Clone, Copy)]
enum Side {
    Down,
    Up,
}
impl Side {
    fn sockets(self, module: &Module) -> (Socket, Socket) {
        match self {
            Self::Down => (module.up, module.down),
            Self::Up => (module.down, module.up),
        }
    }
    fn residual(self, remaining: [f32; 3], displacement: [f32; 3]) -> [f32; 3] {
        match self {
            Self::Down => add(remaining, displacement),
            Self::Up => sub(remaining, displacement),
        }
    }
}

struct Candidate<'a> {
    module: &'a Module,
    pose: Pose,
    displacement: [f32; 3],
}
struct Search<'a> {
    pool: Vec<&'a Module>,
    policy: Policy,
    random: u64,
}
impl Search<'_> {
    fn choose(
        &mut self,
        side: Side,
        ends: [Socket; 2],
        remaining: [f32; 3],
        relaxation: [bool; 2],
        horizontal: bool,
        meter: &mut Meter<'_>,
    ) -> Result<Option<Candidate<'_>>, Error> {
        let [relaxed, tight] = relaxation;
        let mut best: Option<Candidate<'_>> = None;
        let mut priority = i32::MIN;
        for &module in &self.pool {
            meter.spend(1)?;
            let random = crate::decor::unit(&mut self.random);
            let (input, output) = side.sockets(module);
            if ends[0].kind != input.kind
                || (module.down.kind == self.policy.elevator_type && (relaxed || horizontal))
                || (module.down.kind == self.policy.transition_type && horizontal)
            {
                continue;
            }
            let rank = if relaxed { 0 } else { module.priority };
            if priority > rank {
                continue;
            }
            let mut rotation = multiply(ends[0].pose.rotation, inverse(input.pose.rotation));
            let mut output_rotation = multiply(rotation, output.pose.rotation);
            if module.rotation_limit > 0. {
                if angle(ends[1].pose.rotation, output_rotation) > module.rotation_limit {
                    continue;
                }
                let correction = multiply(ends[1].pose.rotation, inverse(output_rotation));
                rotation = multiply(rotation, correction);
                output_rotation = multiply(output_rotation, correction);
            }
            let displacement = rotate(rotation, sub(output.pose.position, input.pose.position));
            let previous =
                side.residual(remaining, best.as_ref().map_or([0.; 3], |v| v.displacement));
            let next = side.residual(remaining, displacement);
            let previous_distance = masked(previous, horizontal);
            let next_distance = masked(next, horizontal);
            if best.is_some() {
                if previous_distance < next_distance
                    || (previous_distance == next_distance && magnitude(previous) < magnitude(next))
                    || (previous_distance == next_distance
                        && magnitude(previous) == magnitude(next)
                        && random < 0.5)
                {
                    continue;
                }
            } else if previous_distance <= next_distance {
                continue;
            }
            if (0..3).any(|axis| {
                (horizontal || axis == 1)
                    && (next[axis].abs() - previous[axis].abs() > 0.01
                        || (next[axis].abs() > 0.01 && previous[axis] * next[axis] < 0.))
            }) {
                continue;
            }
            // The inspected selector advances priority before its final clearance guards.
            priority = rank;
            if !horizontal {
                if (!relaxed && next[1].abs() < self.policy.link_transition - 0.1)
                    || (relaxed
                        && output.kind == self.policy.default_type
                        && [next[0], next[2]].iter().any(|v| {
                            v.abs() > 0.01 && v.abs() < self.policy.link_radius * 2. - 0.1
                        }))
                {
                    continue;
                }
            } else if next_distance > 0.01
                && ((!relaxed
                    && [next[0], next[2]]
                        .iter()
                        .any(|v| v.abs() < self.policy.link_radius * 2. - 0.1))
                    || (tight
                        && [next[0], next[2]]
                            .iter()
                            .any(|v| v.abs() < self.policy.link_radius - 0.1)))
            {
                continue;
            }
            if relaxed
                && next_distance < 0.01
                && magnitude(next) < 0.01
                && angle(ends[1].pose.rotation, output_rotation) > 10.
            {
                continue;
            }
            let pose = Pose {
                position: sub(ends[0].pose.position, rotate(rotation, input.pose.position)),
                rotation,
            };
            validate(pose)?;
            best = Some(Candidate {
                module,
                pose,
                displacement,
            });
        }
        Ok(best)
    }
}

pub fn assemble(request: Request<'_>, cancelled: impl Fn() -> bool) -> Result<Connection, Error> {
    let p = request.policy;
    if p.default_type == p.elevator_type
        || p.default_type == p.transition_type
        || p.elevator_type == p.transition_type
        || ![p.link_radius, p.link_transition]
            .iter()
            .all(|v| v.is_finite() && (0.001..=10000.).contains(v))
        || request.modules.is_empty()
        || request.modules.len() > 4096
        || !(1..=128_000_000).contains(&request.maximum_work)
        || request.maximum_segments > 131072
    {
        return Err(Error::Invalid("Invalid tunnel connection inputs".into()));
    }
    let mut meter = Meter {
        used: 0,
        limit: request.maximum_work,
        cancelled: &cancelled,
    };
    for end in [request.entrance, request.station] {
        meter.spend(1)?;
        validate(end.pose)?;
        validate(end.socket.pose)?;
        validate(end.world().pose)?;
    }
    let mut ids = BTreeSet::new();
    let mut pool = Vec::new();
    for module in request.modules {
        meter.spend(1)?;
        if module.id == 0
            || !ids.insert(module.id)
            || !module.rotation_limit.is_finite()
            || !(0. ..=360.).contains(&module.rotation_limit)
        {
            return Err(Error::Invalid("Invalid tunnel link module".into()));
        }
        validate(module.up.pose)?;
        validate(module.down.pose)?;
        pool.push(module);
    }
    pool.sort_by_key(|m| std::cmp::Reverse(m.priority));
    let mut search = Search {
        pool,
        policy: p,
        random: request.seed,
    };
    let mut ends = [request.entrance.world(), request.station.world()];
    let mut remaining = sub(ends[0].pose.position, ends[1].pose.position);
    let mut segments = [Vec::new(), Vec::new()];
    for horizontal in [false, true] {
        let mut misses = [0; 2];
        while magnitude(remaining) > 1. && misses.iter().any(|&n| n < 8) {
            meter.spend(1)?;
            let relaxation = [
                misses.iter().all(|&n| n > 2),
                !misses.iter().all(|&n| n > 4),
            ];
            for (index, side) in [Side::Down, Side::Up].into_iter().enumerate() {
                if index == 1 && !horizontal {
                    misses[index] += 1;
                    continue;
                }
                let found = search.choose(
                    side,
                    [ends[index], ends[1 - index]],
                    remaining,
                    relaxation,
                    horizontal,
                    &mut meter,
                )?;
                if let Some(candidate) = found
                    && magnitude(candidate.displacement) >= 0.00001
                {
                    if segments[0].len() + segments[1].len() >= request.maximum_segments {
                        return Err(Error::SegmentLimit);
                    }
                    remaining = side.residual(remaining, candidate.displacement);
                    ends[index] = End {
                        socket: side.sockets(candidate.module).1,
                        pose: candidate.pose,
                    }
                    .world();
                    segments[index].push(Segment {
                        prefab_id: candidate.module.id,
                        pose: candidate.pose,
                    });
                } else {
                    misses[index] += 1;
                }
            }
        }
    }
    let gap = magnitude(sub(ends[0].pose.position, ends[1].pose.position));
    let angle = angle(ends[0].pose.rotation, ends[1].pose.rotation);
    meter.spend(1)?;
    if gap > 1. || angle > 10. || ends[0].kind != ends[1].kind {
        return Err(Error::Disconnected {
            gap,
            angle,
            types: [ends[0].kind, ends[1].kind],
            work: meter.used,
        });
    }
    let [downwards, upwards] = segments;
    Ok(Connection {
        downwards,
        upwards,
        work: meter.used,
        gap,
        angle,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn pose(position: [f32; 3]) -> Pose {
        Pose {
            position,
            rotation: [0., 0., 0., 1.],
        }
    }
    fn socket(position: [f32; 3]) -> Socket {
        Socket {
            kind: 0,
            pose: pose(position),
        }
    }
    fn module() -> Module {
        Module {
            id: 1,
            up: socket([0., 3., 0.]),
            down: socket([0.; 3]),
            priority: 0,
            rotation_limit: 0.,
        }
    }
    fn request(modules: &[Module]) -> Request<'_> {
        Request {
            entrance: End {
                socket: socket([0.; 3]),
                pose: pose([0., 30., 0.]),
            },
            station: End {
                socket: socket([0.; 3]),
                pose: pose([0.; 3]),
            },
            modules,
            policy: Policy {
                default_type: 0,
                elevator_type: 1,
                transition_type: 2,
                link_radius: 3.,
                link_transition: 9.,
            },
            seed: 3,
            maximum_work: 10000,
            maximum_segments: 100,
        }
    }

    #[test]
    fn vertical_chain_preserves_every_socket_and_is_repeatable() {
        let modules = [module()];
        let first = assemble(request(&modules), || false).unwrap();
        assert_eq!(assemble(request(&modules), || false), Ok(first.clone()));
        assert_eq!(first.downwards.len(), 10);
        assert!(first.upwards.is_empty());
        assert_eq!(first.gap, 0.);
        let mut cursor = request(&modules).entrance.world();
        for segment in first.downwards {
            let input = End {
                socket: modules[0].up,
                pose: segment.pose,
            }
            .world();
            assert_eq!(cursor, input);
            cursor = End {
                socket: modules[0].down,
                pose: segment.pose,
            }
            .world();
        }
        assert_eq!(cursor, request(&modules).station.world());
    }

    #[test]
    fn horizontal_phase_grows_both_ends() {
        let modules = [Module {
            up: socket([0., 0., 3.]),
            down: socket([0., 0., -3.]),
            ..module()
        }];
        let mut request = request(&modules);
        request.entrance.pose.position = [0., 0., 24.];
        let result = assemble(request, || false).unwrap();
        assert!(!result.downwards.is_empty());
        assert!(!result.upwards.is_empty());
        assert_eq!(result.downwards.len() + result.upwards.len(), 4);
        assert_eq!(result.gap, 0.);
    }

    #[test]
    fn unmatched_type_and_rotation_do_not_publish_a_connection() {
        let modules = [module()];
        let mut mismatched = request(&modules);
        mismatched.entrance.pose = pose([0.; 3]);
        mismatched.entrance.socket.kind = 1;
        assert!(matches!(
            assemble(mismatched, || false),
            Err(Error::Disconnected { .. })
        ));
        let mut rotated = request(&modules);
        rotated.entrance.pose = pose([0.; 3]);
        rotated.entrance.pose.rotation = [0., 1., 0., 0.];
        assert!(matches!(
            assemble(rotated, || false),
            Err(Error::Disconnected {
                gap: 0.,
                angle: 180.,
                ..
            })
        ));
    }

    #[test]
    fn cancellation_work_and_segment_limits_are_atomic() {
        let modules = [module()];
        let complete = assemble(request(&modules), || false).unwrap();
        let mut limited = request(&modules);
        limited.maximum_segments = 2;
        assert_eq!(assemble(limited, || false), Err(Error::SegmentLimit));
        let mut aligned = request(&modules);
        aligned.maximum_segments = 0;
        aligned.entrance.pose = aligned.station.pose;
        let connected = assemble(aligned, || false).unwrap();
        assert!(connected.downwards.is_empty() && connected.upwards.is_empty());
        let mut limited = request(&modules);
        limited.maximum_segments = 0;
        assert_eq!(assemble(limited, || false), Err(Error::SegmentLimit));
        let mut limited = request(&modules);
        limited.maximum_work = complete.work - 1;
        assert_eq!(
            assemble(limited, || false),
            Err(Error::Work(Failure::WorkLimit))
        );
        let calls = Cell::new(0);
        assert_eq!(
            assemble(request(&modules), || {
                calls.set(calls.get() + 1);
                calls.get() == complete.work
            }),
            Err(Error::Work(Failure::Cancelled))
        );
    }

    #[test]
    fn duplicate_ids_and_nonfinite_geometry_are_rejected() {
        let mut modules = [module(), module()];
        assert!(matches!(
            assemble(request(&modules), || false),
            Err(Error::Invalid(_))
        ));
        modules[1].id = 2;
        modules[1].up.pose.position[0] = f32::NAN;
        assert!(matches!(
            assemble(request(&modules), || false),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn authored_rotation_limit_allows_the_transition_to_turn() {
        let mut module = Module {
            up: Socket {
                kind: 1,
                pose: pose([0., 6., 0.]),
            },
            down: socket([9., 0., 0.]),
            rotation_limit: 90.,
            ..module()
        };
        let run = |modules: &[Module]| {
            let mut request = request(modules);
            request.entrance.socket.kind = 1;
            request.entrance.pose.position = [0., 6., 0.];
            request.station.pose.position = [0., 0., -9.];
            request.station.pose.rotation = [
                0.,
                std::f32::consts::FRAC_1_SQRT_2,
                0.,
                std::f32::consts::FRAC_1_SQRT_2,
            ];
            assemble(request, || false)
        };
        let result = run(std::slice::from_ref(&module)).unwrap();
        assert_eq!(result.downwards.len(), 1);
        assert!(result.gap < 0.00001);
        module.rotation_limit = 89.;
        assert!(matches!(run(&[module]), Err(Error::Disconnected { .. })));
    }
}
