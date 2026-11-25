//! Independent, bounded socket assembly using installation-authored dungeon data.
use crate::environment::{self, Box3, Transform, multiply, rotate};
use serde::{Deserialize, Serialize};

pub const VERSION: &str = "papyrus-dungeon-base-1";
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pose {
    pub position: [f32; 3],
    pub rotation: [f32; 4],
}
impl Pose {
    pub fn point(self, local: [f32; 3]) -> [f32; 3] {
        add(self.position, rotate(self.rotation, local))
    }
    pub(crate) fn validate(self) -> Result<(), Failure> {
        Transform {
            position: self.position,
            rotation: self.rotation,
            scale: [1.; 3],
        }
        .validate()
        .map_err(Failure::Invalid)
    }
}
#[derive(Clone, Debug)]
pub struct Socket {
    pub kind: u8,
    pub male: bool,
    pub female: bool,
    pub position: [f32; 3],
    pub rotation: [f32; 4],
    /// Authored prefab-world forward, distinct from the local socket rotation.
    pub forward: [f32; 3],
}
#[derive(Clone, Debug)]
pub struct Volume {
    pub pose: Pose,
    pub center: [f32; 3],
    pub size: [f32; 3],
}
impl Volume {
    fn bounds(&self, pose: Pose, extrude: f32) -> Box3 {
        Box3 {
            center: pose.point(self.pose.point(self.center)),
            rotation: multiply(pose.rotation, self.pose.rotation),
            extents: self.size.map(|v| (v + extrude) * 0.5),
        }
    }
}
#[derive(Clone, Debug)]
pub struct Link {
    pub kind: u8,
    pub cost: u32,
    pub maximum_floor: i32,
    pub maximum_local: i32,
    pub maximum_global: i32,
    pub count_identifier: i32,
    pub sockets: Vec<Socket>,
    pub volumes: Vec<Volume>,
}
#[derive(Clone, Copy, Debug)]
pub struct Transition {
    pub kind: u8,
    pub neighbours: [u8; 2],
}
#[derive(Clone, Debug)]
pub struct Prefab {
    pub link: Option<Link>,
    pub transition: Option<Transition>,
}
pub struct Pools {
    pub entrances: Vec<usize>,
    pub links: Vec<usize>,
    pub ends: Vec<usize>,
    pub transitions: Vec<usize>,
}
#[derive(Clone, Copy, Debug)]
pub struct Root {
    pub owner: u64,
    pub prefab: usize,
    /// DungeonBaseInfo's world pose, including its authored root-relative offset.
    pub pose: Pose,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Segment {
    pub owner: u64,
    pub prefab: usize,
    pub pose: Pose,
    pub root: bool,
    pub cost: u32,
    pub floor: u32,
    pub score: usize,
}
#[derive(Debug, PartialEq, Serialize)]
pub struct World {
    pub engine: &'static str,
    pub seed: u32,
    pub segments: Vec<Segment>,
    pub work: usize,
    pub audit: Audit,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Audit {
    pub passage_sockets: usize,
    pub unpaired_support_sockets: usize,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Instance {
    pub owner: u64,
    pub stage: i64,
    pub prefab: String,
    pub pose: Pose,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageReport {
    pub object_id: i64,
    pub order: usize,
    pub roots: Vec<u64>,
    pub work: usize,
    pub instances: usize,
    pub audit: Audit,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scene {
    pub engine: String,
    pub source_sha256: String,
    pub monument_sha256: String,
    pub layout_fingerprint: [u8; 32],
    pub seed: u32,
    pub stages: Vec<StageReport>,
    pub instances: Vec<Instance>,
}
#[derive(Debug, PartialEq)]
pub enum Failure {
    Invalid(String),
    Budget,
    Cancelled,
}
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub maximum_work: usize,
    pub maximum_segments: usize,
}
pub struct Work<'a> {
    pub used: usize,
    maximum: usize,
    cancelled: &'a dyn Fn() -> bool,
}
impl Work<'_> {
    pub fn spend(&mut self, count: usize) -> Result<(), Failure> {
        if (self.cancelled)() {
            return Err(Failure::Cancelled);
        }
        if count > self.maximum.saturating_sub(self.used) {
            return Err(Failure::Budget);
        }
        self.used += count;
        Ok(())
    }
}
fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| a[i] + b[i])
}
fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a.into_iter().zip(b).map(|(a, b)| a * b).sum()
}
fn close(a: [f32; 3], b: [f32; 3]) -> bool {
    let d = sub(a, b);
    dot(d, d) < 0.01
}
fn unit(v: [f32; 3]) -> Option<[f32; 3]> {
    let n = dot(v, v).sqrt();
    (n > 1e-7 && n.is_finite()).then(|| v.map(|v| v / n))
}
fn from_to(a: [f32; 3], b: [f32; 3]) -> [f32; 4] {
    let a = unit(a).expect("validated socket direction");
    let b = unit(b).expect("unit target socket direction");
    let d = dot(a, b).clamp(-1., 1.);
    let cross = |a: [f32; 3], b: [f32; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    if d < -0.999999 {
        let basis = if a[0].abs() < a[2].abs() {
            [1., 0., 0.]
        } else {
            [0., 0., 1.]
        };
        let axis = unit(cross(a, basis)).expect("nonparallel opposite-vector basis");
        return [axis[0], axis[1], axis[2], 0.];
    }
    let v = cross(a, b);
    let q = [v[0], v[1], v[2], 1. + d];
    let length = q.into_iter().map(|v| v * v).sum::<f32>().sqrt();
    q.map(|v| v / length)
}
fn yaw(degrees: f32) -> [f32; 4] {
    let (s, c) = (degrees.to_radians() * 0.5).sin_cos();
    [0., s, 0., c]
}
fn contains(b: Box3, p: [f32; 3]) -> bool {
    let q = [
        -b.rotation[0],
        -b.rotation[1],
        -b.rotation[2],
        b.rotation[3],
    ];
    let local = rotate(q, sub(p, b.center));
    (0..3).all(|i| local[i].abs() <= b.extents[i])
}
fn socket_at(
    link: &Link,
    pose: Pose,
    point: [f32; 3],
    work: &mut Work<'_>,
) -> Result<bool, Failure> {
    work.spend(link.sockets.len())?;
    Ok(link
        .sockets
        .iter()
        .any(|socket| close(pose.point(socket.position), point)))
}
fn shuffle<T>(items: &mut [T], state: &mut u64, work: &mut Work<'_>) -> Result<(), Failure> {
    work.spend(items.len())?;
    for i in (1..items.len()).rev() {
        let j = (crate::decor::random(state) % (i as u64 + 1)) as usize;
        items.swap(i, j);
    }
    Ok(())
}
pub fn validate(prefabs: &[Prefab], pools: &Pools, roots: &[Root]) -> Result<(), Failure> {
    let invalid = || Failure::Invalid("Invalid dungeon source or assembly inputs".into());
    if !(1..=1024).contains(&prefabs.len()) || roots.len() > 64 {
        return Err(invalid());
    }
    let mut owners = std::collections::BTreeSet::new();
    for root in roots {
        root.pose.validate()?;
        if !owners.insert(root.owner) || prefabs.get(root.prefab).is_none_or(|p| p.link.is_none()) {
            return Err(invalid());
        }
    }
    for (transitions, indices) in [
        (false, &pools.links),
        (false, &pools.entrances),
        (false, &pools.ends),
        (true, &pools.transitions),
    ] {
        if !(1..=512).contains(&indices.len())
            || indices
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != indices.len()
            || indices.iter().any(|&i| {
                prefabs.get(i).is_none_or(|p| {
                    if transitions {
                        p.transition.is_none()
                    } else {
                        p.link.is_none()
                    }
                })
            })
        {
            return Err(invalid());
        }
    }
    for p in prefabs {
        if p.link.is_some() == p.transition.is_some() {
            return Err(invalid());
        }
        if let Some(t) = p.transition
            && (t.kind > 2 || t.neighbours.iter().any(|v| *v > 2))
        {
            return Err(invalid());
        }
        if let Some(l) = &p.link {
            if l.kind > 2
                || l.cost > 4096
                || l.maximum_floor < -1
                || l.maximum_local < -1
                || l.maximum_global < -1
                || l.count_identifier < -1
                || !(1..=256).contains(&l.sockets.len())
                || l.volumes.len() > 256
            {
                return Err(invalid());
            }
            for s in &l.sockets {
                Pose {
                    position: s.position,
                    rotation: s.rotation,
                }
                .validate()?;
                if s.kind > 2
                    || s.forward.iter().any(|v| !v.is_finite())
                    || (dot(s.forward, s.forward) - 1.).abs() > 0.001
                {
                    return Err(invalid());
                }
            }
            for v in &l.volumes {
                v.pose.validate()?;
                v.bounds(
                    Pose {
                        position: [0.; 3],
                        rotation: [0., 0., 0., 1.],
                    },
                    -0.01,
                )
                .validate()
                .map_err(Failure::Invalid)?;
            }
        }
    }
    Ok(())
}
struct Runner<'a, 'c, F> {
    prefabs: &'a [Prefab],
    published: Vec<Segment>,
    random: u64,
    work: Work<'c>,
    maximum_segments: usize,
    check: F,
}
struct PlacedSocket {
    segment: usize,
    kind: u8,
    male: bool,
    female: bool,
    position: [f32; 3],
    forward: [f32; 3],
}
impl<F: FnMut(usize, Pose, &mut Work<'_>) -> Result<bool, Failure>> Runner<'_, '_, F> {
    fn audit(&mut self, segments: &[Segment]) -> Result<Audit, Failure> {
        let mut sockets = Vec::new();
        for (index, segment) in segments.iter().enumerate() {
            if let Some(link) = &self.prefabs[segment.prefab].link {
                self.work.spend(link.sockets.len())?;
                for socket in &link.sockets {
                    sockets.push(PlacedSocket {
                        segment: index,
                        kind: socket.kind,
                        male: socket.male,
                        female: socket.female,
                        position: segment.pose.point(socket.position),
                        forward: rotate(
                            multiply(segment.pose.rotation, socket.rotation),
                            [0., 0., 1.],
                        ),
                    });
                }
            }
        }
        let mut audit = Audit::default();
        for socket in &sockets {
            if !socket.male && !socket.female {
                continue;
            }
            let mut paired = false;
            for other in &sockets {
                self.work.spend(1)?;
                if socket.segment != other.segment
                    && socket.kind == other.kind
                    && (socket.male && other.female || socket.female && other.male)
                    && close(socket.position, other.position)
                    && dot(socket.forward, other.forward) < -0.9999
                {
                    paired = true;
                    break;
                }
            }
            if socket.kind == 2 {
                audit.unpaired_support_sockets += usize::from(!paired);
            } else {
                audit.passage_sockets += 1;
                if !paired {
                    return Err(Failure::Invalid(format!(
                        "Dungeon passage could not be closed at {:?}",
                        socket.position
                    )));
                }
            }
        }
        Ok(audit)
    }
    fn link(&self, segment: &Segment) -> &Link {
        self.prefabs[segment.prefab]
            .link
            .as_ref()
            .expect("link segment")
    }
    fn matches(
        &mut self,
        segments: &[Segment],
        prefab: usize,
        pose: Pose,
    ) -> Result<usize, Failure> {
        let candidate = self.prefabs[prefab].link.as_ref().unwrap();
        let mut count = 0;
        for other in segments {
            let link = self.prefabs[other.prefab].link.as_ref().unwrap();
            for (a, sa) in link.sockets.iter().enumerate() {
                for (b, sb) in candidate.sockets.iter().enumerate() {
                    self.work.spend(1)?;
                    if (other.prefab != prefab || a != b)
                        && close(other.pose.point(sa.position), pose.point(sb.position))
                    {
                        count += 1;
                    }
                }
            }
        }
        Ok(count)
    }
    fn occupied(
        &mut self,
        segments: &[Segment],
        prefab: usize,
        socket: usize,
        point: [f32; 3],
    ) -> Result<bool, Failure> {
        for other in segments {
            for (i, s) in self.prefabs[other.prefab]
                .link
                .as_ref()
                .unwrap()
                .sockets
                .iter()
                .enumerate()
            {
                self.work.spend(1)?;
                if (other.prefab != prefab || i != socket)
                    && close(other.pose.point(s.position), point)
                {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
    fn blocked(
        &mut self,
        segments: &[Segment],
        prefab: usize,
        pose: Pose,
    ) -> Result<bool, Failure> {
        let link = self.prefabs[prefab].link.as_ref().unwrap();
        for v in &link.volumes {
            let small = v.bounds(pose, -0.01);
            let large = v.bounds(pose, 0.01);
            for other in segments {
                let other_link = self.prefabs[other.prefab].link.as_ref().unwrap();
                for ov in &other_link.volumes {
                    self.work.spend(1)?;
                    if environment::overlap(small, ov.bounds(other.pose, -0.01), 0.) {
                        return Ok(true);
                    }
                }
                for s in &other_link.sockets {
                    let point = other.pose.point(s.position);
                    self.work.spend(1)?;
                    if contains(large, point) && !socket_at(link, pose, point, &mut self.work)? {
                        return Ok(true);
                    }
                }
            }
        }
        for other in segments {
            for v in &self.prefabs[other.prefab].link.as_ref().unwrap().volumes {
                let large = v.bounds(other.pose, 0.01);
                for s in &link.sockets {
                    let point = pose.point(s.position);
                    self.work.spend(1)?;
                    if contains(large, point)
                        && !socket_at(
                            self.prefabs[other.prefab].link.as_ref().unwrap(),
                            other.pose,
                            point,
                            &mut self.work,
                        )?
                    {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }
    fn count(
        prefabs: &[Prefab],
        segments: &[Segment],
        prefab: usize,
        work: &mut Work<'_>,
    ) -> Result<usize, Failure> {
        work.spend(segments.len())?;
        let id = prefabs[prefab].link.as_ref().unwrap().count_identifier;
        Ok(segments
            .iter()
            .filter(|s| {
                prefabs[s.prefab]
                    .link
                    .as_ref()
                    .is_some_and(|l| s.prefab == prefab || id >= 0 && l.count_identifier == id)
            })
            .count())
    }
    fn place(
        &mut self,
        segments: &[Segment],
        target: &Socket,
        target_pose: Pose,
        prefab: usize,
    ) -> Result<Option<(Pose, usize)>, Failure> {
        let link = self.prefabs[prefab].link.as_ref().unwrap();
        let start = (crate::decor::random(&mut self.random) % link.sockets.len() as u64) as usize;
        let mut rotations: Vec<_> = match target.kind {
            0 => vec![0.],
            1 => (0..8).map(|i| i as f32 * 45.).collect(),
            _ => vec![0., 90., 180., 270.],
        };
        let mut best = None;
        let mut best_score = 0;
        for j in 0..link.sockets.len() {
            let socket = &link.sockets[(j + start) % link.sockets.len()];
            self.work.spend(1)?;
            if socket.kind != target.kind
                || !(socket.male && target.female || socket.female && target.male)
            {
                continue;
            }
            shuffle(&mut rotations, &mut self.random, &mut self.work)?;
            for &angle in &rotations {
                let mut rotation = from_to(
                    socket.forward.map(|v| -v),
                    rotate(target_pose.rotation, [0., 0., 1.]),
                );
                if socket.kind != 1 {
                    rotation = crate::decor_fit::path_rotation(rotate(rotation, [0., 0., 1.]))
                        .ok_or_else(|| {
                            Failure::Invalid("Degenerate dungeon forced-up rotation".into())
                        })?;
                }
                let rotation = multiply(yaw(angle), rotation);
                let pose = Pose {
                    position: sub(target_pose.position, rotate(rotation, socket.position)),
                    rotation,
                };
                pose.validate()?;
                if !self.blocked(segments, prefab, pose)? {
                    let score = self.matches(segments, prefab, pose)?;
                    if score > best_score && (self.check)(prefab, pose, &mut self.work)? {
                        best = Some((pose, score));
                        best_score = score;
                    }
                }
            }
        }
        Ok(best)
    }
    #[allow(clippy::too_many_arguments)]
    fn grow(
        &mut self,
        segments: &mut Vec<Segment>,
        pool: &mut [usize],
        count: usize,
        budget: u32,
        floors: u32,
        female: bool,
        male: bool,
    ) -> Result<(), Failure> {
        let mut added = 0;
        let mut index = 0;
        while index < segments.len() {
            let parent = segments[index].clone();
            index += 1;
            if parent.cost >= budget {
                continue;
            }
            let sockets = self.link(&parent).sockets.clone();
            let start = (crate::decor::random(&mut self.random) % sockets.len() as u64) as usize;
            for j in 0..sockets.len() {
                let socket_index = (j + start) % sockets.len();
                let socket = &sockets[socket_index];
                self.work.spend(1)?;
                if !(socket.female && female || socket.male && male) {
                    continue;
                }
                let pose = Pose {
                    position: parent.pose.point(socket.position),
                    rotation: multiply(parent.pose.rotation, socket.rotation),
                };
                if self.occupied(segments, parent.prefab, socket_index, pose.position)? {
                    continue;
                }
                shuffle(pool, &mut self.random, &mut self.work)?;
                let mut best: Option<Segment> = None;
                for &prefab in pool.iter() {
                    let link = self.prefabs[prefab].link.as_ref().unwrap();
                    let cost = parent.cost.saturating_add(link.cost);
                    let floor = parent.floor + u32::from(socket.kind == 1);
                    if link.maximum_local == 0
                        || link.maximum_global == 0
                        || (link.maximum_floor >= 0 && parent.floor > link.maximum_floor as u32)
                        || cost > budget
                        || floor > floors
                    {
                        continue;
                    }
                    let local = link.maximum_local;
                    let global = link.maximum_global;
                    if let Some((pose, score)) = self.place(segments, socket, pose, prefab)? {
                        if local > 0
                            && Self::count(self.prefabs, segments, prefab, &mut self.work)?
                                >= local as usize
                        {
                            continue;
                        }
                        if global > 0 {
                            let existing =
                                Self::count(self.prefabs, &self.published, prefab, &mut self.work)?;
                            if existing
                                + Self::count(self.prefabs, segments, prefab, &mut self.work)?
                                >= global as usize
                            {
                                continue;
                            }
                        }
                        if best.as_ref().is_none_or(|b| b.score < score) {
                            best = Some(Segment {
                                owner: parent.owner,
                                prefab,
                                pose,
                                root: false,
                                cost,
                                floor,
                                score,
                            });
                        }
                    }
                }
                if let Some(best) = best {
                    if segments.len() + self.published.len() >= self.maximum_segments {
                        return Err(Failure::Budget);
                    }
                    segments.push(best);
                    added += 1;
                    if added >= count {
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }
    fn transitions(
        &mut self,
        segments: &mut Vec<Segment>,
        pool: &mut [usize],
    ) -> Result<(), Failure> {
        let count = segments.len();
        for i in 0..count {
            let parent = segments[i].clone();
            let sockets = self.link(&parent).sockets.clone();
            let start = (crate::decor::random(&mut self.random) % sockets.len() as u64) as usize;
            for j in 0..sockets.len() {
                let socket = &sockets[(j + start) % sockets.len()];
                let point = parent.pose.point(socket.position);
                shuffle(pool, &mut self.random, &mut self.work)?;
                for &prefab in pool.iter() {
                    let transition = self.prefabs[prefab].transition.as_ref().unwrap();
                    if transition.kind != socket.kind {
                        continue;
                    }
                    let mut neighbours = [false; 2];
                    for other in segments.iter() {
                        self.work.spend(1)?;
                        if let Some(link) = &self.prefabs[other.prefab].link {
                            for other_socket in &link.sockets {
                                self.work.spend(1)?;
                                if close(other.pose.point(other_socket.position), point) {
                                    if !neighbours[0] && link.kind == transition.neighbours[0] {
                                        neighbours[0] = true;
                                    } else if !neighbours[1]
                                        && link.kind == transition.neighbours[1]
                                    {
                                        neighbours[1] = true;
                                    }
                                }
                            }
                        } else if close(other.pose.position, point) {
                            neighbours = [false; 2];
                        }
                    }
                    if neighbours == [true; 2] {
                        if segments.len() + self.published.len() >= self.maximum_segments {
                            return Err(Failure::Budget);
                        }
                        segments.push(Segment {
                            owner: parent.owner,
                            prefab,
                            pose: Pose {
                                position: point,
                                rotation: multiply(parent.pose.rotation, socket.rotation),
                            },
                            root: false,
                            cost: 0,
                            floor: 0,
                            score: 0,
                        });
                        break;
                    }
                }
            }
        }
        Ok(())
    }
}
/// The terrain callback must check every relevant environment-volume footprint.
/// Results publish only after all roots finish within the shared work allowance.
pub fn generate(
    prefabs: &[Prefab],
    mut pools: Pools,
    roots: &[Root],
    seed: u32,
    limits: Limits,
    mut check: impl FnMut(usize, Pose, &mut Work<'_>) -> Result<bool, Failure>,
    cancelled: impl Fn() -> bool,
) -> Result<World, Failure> {
    let Limits {
        maximum_work,
        maximum_segments,
    } = limits;
    validate(prefabs, &pools, roots)?;
    if !(1..=256_000_000).contains(&maximum_work) || !(1..=4096).contains(&maximum_segments) {
        return Err(Failure::Invalid(
            "Invalid dungeon work or instance allowance".into(),
        ));
    }
    let mut runner = Runner {
        prefabs,
        published: Vec::new(),
        random: u64::from(seed),
        work: Work {
            used: 0,
            maximum: maximum_work,
            cancelled: &cancelled,
        },
        maximum_segments,
        check: &mut check,
    };
    let mut audit = Audit::default();
    for root in roots {
        runner.work.spend(1)?;
        if runner.published.len() >= runner.maximum_segments {
            return Err(Failure::Budget);
        }
        runner.random = crate::decor::stage_seed(seed, &format!("{VERSION}/{}", root.owner));
        let start = Segment {
            owner: root.owner,
            prefab: root.prefab,
            pose: root.pose,
            root: true,
            cost: 0,
            floor: 0,
            score: 0,
        };
        let mut best = Vec::new();
        let mut best_limited = 0;
        for _ in 0..25 {
            runner.work.spend(1)?;
            let mut trial = vec![start.clone()];
            runner.grow(&mut trial, &mut pools.links, usize::MAX, 3, 2, true, false)?;
            let limited = trial
                .iter()
                .filter(|s| runner.link(s).maximum_local != -1)
                .count();
            if limited > best_limited || limited == best_limited && trial.len() > best.len() {
                best = trial;
                best_limited = limited;
            }
        }
        if best.len() > 5 {
            best.sort_by(|a, b| {
                let distance = |s: &Segment| {
                    let d = sub(s.pose.position, root.pose.position);
                    d[0] * d[0] + d[2] * d[2]
                };
                distance(b).total_cmp(&distance(a))
            });
            runner.grow(&mut best, &mut pools.entrances, 1, 4, 2, true, false)?;
        }
        if best.len() > 25 {
            let end = best.last().unwrap().pose.position;
            best.sort_by(|a, b| {
                let distance = |s: &Segment| {
                    let d = sub(s.pose.position, root.pose.position);
                    let e = sub(s.pose.position, end);
                    (d[0] * d[0] + d[2] * d[2]).min(e[0] * e[0] + e[2] * e[2])
                };
                distance(b).total_cmp(&distance(a))
            });
            runner.grow(&mut best, &mut pools.entrances, 1, 5, 2, true, false)?;
        }
        loop {
            let mut removed = false;
            let mut i = 0;
            while i < best.len() {
                let segment = &best[i];
                let link = runner.link(segment);
                let (cost, socket_count) = (link.cost, link.sockets.len());
                if cost == 0 && runner.matches(&best, segment.prefab, segment.pose)? != socket_count
                {
                    best.remove(i);
                    removed = true;
                } else {
                    i += 1;
                }
            }
            if !removed {
                break;
            }
        }
        runner.grow(
            &mut best,
            &mut pools.ends,
            usize::MAX,
            u32::MAX,
            3,
            true,
            true,
        )?;
        runner.transitions(&mut best, &mut pools.transitions)?;
        let checked = runner.audit(&best)?;
        audit.passage_sockets += checked.passage_sockets;
        audit.unpaired_support_sockets += checked.unpaired_support_sockets;
        runner.work.spend(1)?;
        runner.published.extend(best);
    }
    Ok(World {
        engine: VERSION,
        seed,
        segments: runner.published,
        work: runner.work.used,
        audit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn socket(z: f32) -> Socket {
        let rotation = yaw(if z < 0. { 180. } else { 0. });
        Socket {
            kind: 0,
            male: true,
            female: true,
            position: [0., 0., z],
            rotation,
            forward: rotate(rotation, [0., 0., 1.]),
        }
    }
    fn link(sockets: Vec<Socket>, maximum_local: i32) -> Prefab {
        Prefab {
            link: Some(Link {
                kind: 0,
                cost: 1,
                maximum_floor: -1,
                maximum_local,
                maximum_global: -1,
                count_identifier: -1,
                sockets,
                volumes: vec![Volume {
                    pose: Pose {
                        position: [0.; 3],
                        rotation: [0., 0., 0., 1.],
                    },
                    center: [0.; 3],
                    size: [2.; 3],
                }],
            }),
            transition: None,
        }
    }
    fn fixture() -> (Vec<Prefab>, Pools, Vec<Root>) {
        let root = link(vec![socket(1.)], 0);
        let room = link(vec![socket(-1.), socket(1.)], 1);
        let mut end = link(vec![socket(-1.)], -1);
        let cap = end.link.as_mut().unwrap();
        cap.kind = 2;
        cap.volumes[0].center = [0., 0., -0.95];
        cap.volumes[0].size = [2., 2., 0.1];
        let transition = Prefab {
            link: None,
            transition: Some(Transition {
                kind: 0,
                neighbours: [0, 0],
            }),
        };
        (
            vec![root, room, end, transition],
            Pools {
                entrances: vec![1],
                links: vec![1],
                ends: vec![2],
                transitions: vec![3],
            },
            vec![Root {
                owner: 1,
                prefab: 0,
                pose: Pose {
                    position: [0., 8., 0.],
                    rotation: [0., 0., 0., 1.],
                },
            }],
        )
    }
    #[test]
    fn assembly_closes_passages_adds_one_transition_and_replays() {
        let (prefabs, pools, roots) = fixture();
        let world = generate(
            &prefabs,
            pools,
            &roots,
            7,
            Limits {
                maximum_work: 1_000_000,
                maximum_segments: 64,
            },
            |_, _, _| Ok(true),
            || false,
        )
        .unwrap();
        assert_eq!(world.segments.len(), 4);
        assert_eq!(world.audit.passage_sockets, 4);
        assert_eq!(world.audit.unpaired_support_sockets, 0);
        assert_eq!(world.segments.iter().filter(|s| s.prefab == 3).count(), 1);
        assert!(
            world
                .segments
                .iter()
                .all(|s| (s.pose.position[1] - 8.).abs() < 1e-5)
        );
        let (_, pools, _) = fixture();
        assert_eq!(
            world,
            generate(
                &prefabs,
                pools,
                &roots,
                7,
                Limits {
                    maximum_work: 1_000_000,
                    maximum_segments: 64
                },
                |_, _, _| Ok(true),
                || false
            )
            .unwrap()
        );
    }
    #[test]
    fn failed_caps_cannot_publish_open_passages() {
        let (prefabs, pools, roots) = fixture();
        let error = generate(
            &prefabs,
            pools,
            &roots,
            7,
            Limits {
                maximum_work: 1_000_000,
                maximum_segments: 64,
            },
            |prefab, _, _| Ok(prefab != 2),
            || false,
        )
        .unwrap_err();
        assert!(
            matches!(error,Failure::Invalid(ref message) if message.contains("passage could not be closed"))
        );
    }
    #[test]
    fn global_limits_include_previous_roots_and_cancelled_work_is_retriable() {
        let (mut prefabs, pools, mut roots) = fixture();
        prefabs[1].link.as_mut().unwrap().maximum_global = 1;
        roots.push(Root {
            owner: 2,
            prefab: 0,
            pose: Pose {
                position: [20., 8., 0.],
                rotation: [0., 0., 0., 1.],
            },
        });
        let world = generate(
            &prefabs,
            pools,
            &roots,
            7,
            Limits {
                maximum_work: 1_000_000,
                maximum_segments: 64,
            },
            |_, _, _| Ok(true),
            || false,
        )
        .unwrap();
        assert_eq!(world.segments.iter().filter(|s| s.prefab == 1).count(), 1);
        let (_, pools, _) = fixture();
        assert_eq!(
            generate(
                &prefabs,
                pools,
                &roots,
                7,
                Limits {
                    maximum_work: 1_000_000,
                    maximum_segments: 64
                },
                |_, _, _| Ok(true),
                || true
            ),
            Err(Failure::Cancelled)
        );
        let (_, pools, _) = fixture();
        assert_eq!(
            world,
            generate(
                &prefabs,
                pools,
                &roots,
                7,
                Limits {
                    maximum_work: 1_000_000,
                    maximum_segments: 64
                },
                |_, _, _| Ok(true),
                || false
            )
            .unwrap()
        );
    }
    #[test]
    fn budgets_and_invalid_owners_fail_before_publication() {
        for (work, instances) in [(1, 64), (1_000_000, 1)] {
            let (prefabs, pools, roots) = fixture();
            assert_eq!(
                generate(
                    &prefabs,
                    pools,
                    &roots,
                    7,
                    Limits {
                        maximum_work: work,
                        maximum_segments: instances
                    },
                    |_, _, _| Ok(true),
                    || false
                ),
                Err(Failure::Budget)
            );
        }
        let (prefabs, pools, mut roots) = fixture();
        roots.push(roots[0]);
        assert!(matches!(
            generate(
                &prefabs,
                pools,
                &roots,
                7,
                Limits {
                    maximum_work: 1_000_000,
                    maximum_segments: 64
                },
                |_, _, _| Ok(true),
                || false
            ),
            Err(Failure::Invalid(_))
        ));
    }
    #[test]
    fn blocked_support_sockets_are_reported_without_treating_them_as_passages() {
        let (mut prefabs, pools, roots) = fixture();
        prefabs[0].link.as_mut().unwrap().sockets[0].kind = 2;
        let world = generate(
            &prefabs,
            pools,
            &roots,
            7,
            Limits {
                maximum_work: 1_000_000,
                maximum_segments: 64,
            },
            |_, _, _| Ok(true),
            || false,
        )
        .unwrap();
        assert_eq!(world.segments.len(), 1);
        assert_eq!(world.audit.passage_sockets, 0);
        assert_eq!(world.audit.unpaired_support_sockets, 1);
    }
    #[test]
    fn vertical_sockets_keep_their_axis_through_rotated_attachments() {
        let (mut prefabs, pools, roots) = fixture();
        for prefab in &mut prefabs {
            if let Some(link) = &mut prefab.link {
                for socket in &mut link.sockets {
                    let direction = socket.position[2];
                    socket.kind = 1;
                    socket.position = [0., direction, 0.];
                    socket.rotation = if direction > 0. {
                        [
                            -std::f32::consts::FRAC_1_SQRT_2,
                            0.,
                            0.,
                            std::f32::consts::FRAC_1_SQRT_2,
                        ]
                    } else {
                        [
                            std::f32::consts::FRAC_1_SQRT_2,
                            0.,
                            0.,
                            std::f32::consts::FRAC_1_SQRT_2,
                        ]
                    };
                    socket.forward = rotate(socket.rotation, [0., 0., 1.]);
                }
            }
        }
        let end = prefabs[2].link.as_mut().unwrap();
        end.volumes[0].center = [0., -0.95, 0.];
        end.volumes[0].size = [2., 0.1, 2.];
        prefabs[3].transition.as_mut().unwrap().kind = 1;
        let world = generate(
            &prefabs,
            pools,
            &roots,
            7,
            Limits {
                maximum_work: 1_000_000,
                maximum_segments: 64,
            },
            |_, _, _| Ok(true),
            || false,
        )
        .unwrap();
        assert_eq!(world.audit.passage_sockets, 4);
        assert!(
            world
                .segments
                .iter()
                .all(|s| s.pose.position[0].abs() < 1e-5 && s.pose.position[2].abs() < 1e-5)
        );
    }
}
