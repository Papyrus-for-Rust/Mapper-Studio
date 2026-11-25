//! Bounded deterministic preference search. Does not modify terrain or stamp game assets.
use crate::{erosion::decode, monuments::Entry};
use std::collections::HashSet;

pub const VERSION: &str = "papyrus-placement-search-1";
pub const MAX_TARGETS: usize = 128;
const TRIALS: usize = 8;
const SAMPLES: usize = 160;
const MAX_GRADE: f32 = 0.12;

#[derive(Clone, Debug)]
pub struct Target {
    pub id: u64,
    pub entry: Entry,
    pub x: f32,
    pub z: f32,
    pub radius: f32,
    pub max_displacement_m: Option<u32>,
    pub weight: f32,
    pub exact: bool,
    pub required: bool,
    pub excluded: bool,
    pub bounds: Option<[crate::monument_bounds::Footprint; 2]>,
    pub priority: i32,
    pub spacing: Option<Spacing>,
}
/// A placement accepted by an earlier phase. Its geometry reserves space while
/// this search assesses new targets. It never enters the movable candidate pool.
pub struct Reservation<'a> {
    pub target: &'a Target,
    pub position: &'a Position,
}

/// Source-authored edge distances, separate from terrain stamp reservations.
/// Independent packing applies the stricter of both monuments' requirements.
#[derive(Clone, Debug)]
pub struct Spacing {
    pub shape: crate::monument_spacing::BoxShape,
    pub family: String,
    pub minimum_same: f32,
    pub minimum_different: f32,
    pub entrance: Option<EntranceSpacing>,
}
#[derive(Clone, Copy, Debug)]
pub struct EntranceSpacing {
    pub offset: [f32; 3],
    pub minimum_distance: f32,
}
impl EntranceSpacing {
    pub fn world(self, position: [f32; 3], yaw: f32) -> [f32; 3] {
        let half = -yaw.to_radians() * 0.5;
        let offset = crate::environment::rotate([0., half.sin(), 0., half.cos()], self.offset);
        std::array::from_fn(|i| position[i] + offset[i])
    }
    fn valid(self) -> bool {
        self.offset
            .iter()
            .all(|v| v.is_finite() && v.abs() <= 100000.)
            && self.minimum_distance.is_finite()
            && (0. ..=10240.).contains(&self.minimum_distance)
    }
}
impl Spacing {
    fn valid(&self) -> bool {
        self.shape.valid()
            && self.entrance.is_none_or(EntranceSpacing::valid)
            && !self.family.is_empty()
            && self.family.len() <= 512
            && [self.minimum_same, self.minimum_different]
                .iter()
                .all(|v| v.is_finite() && (0. ..=10000.).contains(v))
    }
}
pub struct Terrain<'a> {
    pub heights: &'a [i16],
    pub resolution: usize,
    pub size: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Position {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub yaw: f32,
    pub clearance: f32,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub id: u64,
    pub position: Option<Position>,
    pub distance: Option<f32>,
    pub within_radius: bool,
    pub reason: String,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Solution {
    pub outcomes: Vec<Outcome>,
    pub required_missing: usize,
    pub attempts: usize,
}
/// Optional, versioned search hints. They never certify placement or bypass the
/// evaluator. Limits also keep a dense family's pool from starving other targets.
pub struct ProposalSet {
    pub points: Vec<[f32; 2]>,
    pub maximum_accepted: usize,
}
/// A shared stage quota, independent of each prefab's candidate pool.
#[derive(Clone, Copy, Debug)]
pub struct SelectionLimit {
    pub group: i64,
    pub maximum: usize,
}
fn limit_reached(
    id: u64,
    placed: impl Iterator<Item = u64>,
    limits: Option<&std::collections::BTreeMap<u64, SelectionLimit>>,
) -> bool {
    limits
        .and_then(|m| m.get(&id).map(|limit| (m, limit)))
        .is_some_and(|(m, limit)| {
            placed
                .filter(|id| m.get(id).is_some_and(|v| v.group == limit.group))
                .count()
                >= limit.maximum
        })
}
#[derive(Clone)]
struct Candidate {
    position: Position,
    /// Source dungeon-distance queries use the original proposal before decor/anchors.
    reference: [f32; 3],
    cost: f64,
    distance: f32,
    relaxation: u8,
}
struct Trial {
    score: (usize, usize, f64, usize, usize, f64),
    chosen: Vec<Option<usize>>,
}

impl Terrain<'_> {
    fn validate(&self) -> Result<(), String> {
        if !(33..=4097).contains(&self.resolution)
            || !(self.resolution - 1).is_power_of_two()
            || self.heights.len() != self.resolution * self.resolution
            || !(1000..=6000).contains(&self.size)
            || self.heights.iter().any(|h| !(0..=32766).contains(h))
        {
            return Err("Invalid placement terrain grid.".into());
        }
        Ok(())
    }
    fn sample(&self, x: f32, z: f32) -> f32 {
        let cells = (self.resolution - 1) as f32;
        let gx = ((x / self.size as f32 + 0.5) * cells).clamp(0., cells);
        let gz = ((z / self.size as f32 + 0.5) * cells).clamp(0., cells);
        let ix = gx.floor() as usize;
        let iz = gz.floor() as usize;
        let right = (ix + 1).min(self.resolution - 1);
        let top = (iz + 1).min(self.resolution - 1);
        let h = |x, z| decode(self.heights[z * self.resolution + x]);
        let lo = h(ix, iz) + (h(right, iz) - h(ix, iz)) * (gx - ix as f32);
        let hi = h(ix, top) + (h(right, top) - h(ix, top)) * (gx - ix as f32);
        lo + (hi - lo) * (gz - iz as f32)
    }
    pub fn check_candidate(
        &self,
        target: &Target,
        x: f32,
        z: f32,
        yaw: f32,
    ) -> Result<CandidateCheck, String> {
        Ok(match self.candidate(target, x, z, yaw) {
            Ok((position, grade)) => CandidateCheck::Accepted(position, grade),
            Err(reason) => CandidateCheck::Rejected(reason.into()),
        })
    }
    pub fn height_at(&self, x: f32, z: f32) -> f32 {
        self.sample(x, z)
    }
    fn candidate(
        &self,
        t: &Target,
        x: f32,
        z: f32,
        yaw: f32,
    ) -> Result<(Position, f32), &'static str> {
        let r = t.entry.extent_x.hypot(t.entry.extent_z).max(1.);
        let half = self.size as f32 / 2.;
        if x.abs() + r > half || z.abs() + r > half {
            return Err("Footprint reaches outside world bounds");
        }
        let spacing = self.size as f32 / (self.resolution - 1) as f32;
        let (s, c) = yaw.to_radians().sin_cos();
        let mut grade = 0.0_f32;
        for iz in -2..=2 {
            for ix in -2..=2 {
                let dx = ix as f32 / 2. * t.entry.extent_x;
                let dz = iz as f32 / 2. * t.entry.extent_z;
                let sx = x + dx * c - dz * s;
                let sz = z + dx * s + dz * c;
                if self.sample(sx, sz) <= 4. {
                    return Err("Footprint intersects low or submerged terrain");
                }
                let gx = (self.sample(sx + spacing, sz) - self.sample(sx - spacing, sz))
                    / (2. * spacing);
                let gz = (self.sample(sx, sz + spacing) - self.sample(sx, sz - spacing))
                    / (2. * spacing);
                grade = grade.max(gx.hypot(gz));
                if grade > MAX_GRADE {
                    return Err("Sampled footprint terrain is too steep");
                }
            }
        }
        Ok((
            Position {
                x,
                y: self.sample(x, z),
                z,
                yaw,
                clearance: r,
            },
            grade,
        ))
    }
}
fn random(state: &mut u64) -> f32 {
    *state = state.wrapping_add(0x9e3779b97f4a7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    ((z ^ (z >> 31)) >> 40) as f32 / 16_777_216.
}
fn overlap(a: &Position, b: &Position) -> bool {
    (a.x - b.x).hypot(a.z - b.z) < a.clearance + b.clearance
}
fn target_overlap(a: &Target, pa: &Position, b: &Target, pb: &Position) -> bool {
    if let (Some(sa), Some(sb)) = (&a.spacing, &b.spacing) {
        let minimum = if sa.family == sb.family {
            sa.minimum_same.max(sb.minimum_same)
        } else {
            sa.minimum_different.max(sb.minimum_different)
        };
        if minimum > 0. {
            let pose = |s: &Spacing, p: &Position| crate::monument_spacing::BoxPose {
                shape: s.shape,
                position: [p.x, p.y, p.z],
                yaw: p.yaw,
            };
            match pose(sa, pa).squared_distance(pose(sb, pb)) {
                Ok(distance) if distance >= f64::from(minimum).powi(2) => {}
                _ => return true,
            }
        }
    }
    if let (Some(a), Some(b)) = (&a.bounds, &b.bounds) {
        a.iter().any(|a| {
            b.iter().any(|b| {
                a.at([pa.x, pa.z], pa.yaw)
                    .intersects(&b.at([pb.x, pb.z], pb.yaw))
            })
        })
    } else {
        overlap(pa, pb)
    }
}
fn candidate_overlap(a: &Target, ca: &Candidate, b: &Target, cb: &Candidate) -> bool {
    candidate_overlaps_position(a, ca, b, &cb.position)
}
fn candidate_overlaps_position(a: &Target, ca: &Candidate, b: &Target, prior: &Position) -> bool {
    if let (Some(a), Some(b)) = (
        a.spacing.as_ref().and_then(|spacing| spacing.entrance),
        b.spacing.as_ref().and_then(|spacing| spacing.entrance),
    ) {
        let prior = b.world([prior.x, prior.y, prior.z], prior.yaw);
        let squared_distance = ca
            .reference
            .iter()
            .zip(prior)
            .map(|(a, b)| (a - b).powi(2))
            .sum::<f32>();
        if squared_distance < a.minimum_distance.powi(2) {
            return true;
        }
    }
    target_overlap(a, &ca.position, b, prior)
}

/// Returns None on cancellation. A fixed search budget makes completed results repeatable.
pub fn solve(
    terrain: Terrain<'_>,
    targets: &[Target],
    seed: u32,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Solution>, String> {
    solve_with_candidates(
        terrain,
        targets,
        seed,
        false,
        &cancelled,
        |terrain, t, x, z, yaw| {
            Ok(match terrain.candidate(t, x, z, yaw) {
                Ok((position, grade)) => CandidateCheck::Accepted(position, grade),
                Err(reason) => CandidateCheck::Rejected(reason.into()),
            })
        },
    )
}

/// Candidate scores use the legacy grade scale (0.12 is a full terrain penalty).
/// A failed assessment must reject the candidate, never silently use legacy checks.
pub enum CandidateCheck {
    Accepted(Position, f32),
    /// Fully checked under an explicitly enabled, versioned relaxation policy.
    AcceptedRelaxed(Position, f32, u8),
    Rejected(String),
    /// Stop further assessments, retaining only candidates already verified.
    Exhausted(String),
    /// Stop this target's assessments without starving the remaining targets.
    TargetExhausted(String),
    Cancelled,
}

pub fn solve_with_candidates(
    terrain: Terrain<'_>,
    targets: &[Target],
    seed: u32,
    detailed_reasons: bool,
    cancelled: impl Fn() -> bool,
    evaluate: impl FnMut(&Terrain<'_>, &Target, f32, f32, f32) -> Result<CandidateCheck, String>,
) -> Result<Option<Solution>, String> {
    solve_with_policy(
        terrain,
        targets,
        seed,
        detailed_reasons,
        false,
        cancelled,
        evaluate,
    )
}

/// Balanced search alternates local/world samples and caps each verified pool at eight.
/// Legacy callers retain their exact sample order and pool sizes.
pub fn solve_with_policy(
    terrain: Terrain<'_>,
    targets: &[Target],
    seed: u32,
    detailed_reasons: bool,
    balanced: bool,
    cancelled: impl Fn() -> bool,
    evaluate: impl FnMut(&Terrain<'_>, &Target, f32, f32, f32) -> Result<CandidateCheck, String>,
) -> Result<Option<Solution>, String> {
    solve_with_policy_samples(
        terrain,
        targets,
        seed,
        detailed_reasons,
        balanced,
        SAMPLES,
        cancelled,
        evaluate,
    )
}
#[allow(clippy::too_many_arguments)]
pub fn solve_with_policy_samples(
    terrain: Terrain<'_>,
    targets: &[Target],
    seed: u32,
    detailed_reasons: bool,
    balanced: bool,
    samples: usize,
    cancelled: impl Fn() -> bool,
    evaluate: impl FnMut(&Terrain<'_>, &Target, f32, f32, f32) -> Result<CandidateCheck, String>,
) -> Result<Option<Solution>, String> {
    solve_with_policy_packing(
        terrain,
        targets,
        seed,
        detailed_reasons,
        balanced,
        samples,
        false,
        cancelled,
        evaluate,
    )
}
#[allow(clippy::too_many_arguments)]
pub fn solve_with_policy_packing(
    terrain: Terrain<'_>,
    targets: &[Target],
    seed: u32,
    detailed_reasons: bool,
    balanced: bool,
    samples: usize,
    recover_required: bool,
    cancelled: impl Fn() -> bool,
    evaluate: impl FnMut(&Terrain<'_>, &Target, f32, f32, f32) -> Result<CandidateCheck, String>,
) -> Result<Option<Solution>, String> {
    solve_with_policy_proposals(
        terrain,
        targets,
        seed,
        detailed_reasons,
        balanced,
        samples,
        recover_required,
        None,
        cancelled,
        evaluate,
    )
}
#[allow(clippy::too_many_arguments)]
pub fn solve_with_policy_proposals(
    terrain: Terrain<'_>,
    targets: &[Target],
    seed: u32,
    detailed_reasons: bool,
    balanced: bool,
    samples: usize,
    recover_required: bool,
    proposals: Option<&std::collections::BTreeMap<u64, ProposalSet>>,
    cancelled: impl Fn() -> bool,
    evaluate: impl FnMut(&Terrain<'_>, &Target, f32, f32, f32) -> Result<CandidateCheck, String>,
) -> Result<Option<Solution>, String> {
    solve_with_policy_limits(
        terrain,
        targets,
        seed,
        detailed_reasons,
        balanced,
        samples,
        recover_required,
        proposals,
        None,
        cancelled,
        evaluate,
    )
}
#[allow(clippy::too_many_arguments)]
pub fn solve_with_policy_limits(
    terrain: Terrain<'_>,
    targets: &[Target],
    seed: u32,
    detailed_reasons: bool,
    balanced: bool,
    samples: usize,
    recover_required: bool,
    proposals: Option<&std::collections::BTreeMap<u64, ProposalSet>>,
    limits: Option<&std::collections::BTreeMap<u64, SelectionLimit>>,
    cancelled: impl Fn() -> bool,
    evaluate: impl FnMut(&Terrain<'_>, &Target, f32, f32, f32) -> Result<CandidateCheck, String>,
) -> Result<Option<Solution>, String> {
    solve_with_reservations(
        terrain,
        targets,
        seed,
        detailed_reasons,
        balanced,
        samples,
        recover_required,
        proposals,
        limits,
        &[],
        cancelled,
        evaluate,
    )
}

/// Search against immutable placements from preceding generation phases.
/// Reservations consume their installation stage quota and retain authored
/// footprint, family-spacing and entrance-distance checks.
#[allow(clippy::too_many_arguments)]
pub fn solve_with_reservations(
    terrain: Terrain<'_>,
    targets: &[Target],
    seed: u32,
    detailed_reasons: bool,
    balanced: bool,
    samples: usize,
    recover_required: bool,
    proposals: Option<&std::collections::BTreeMap<u64, ProposalSet>>,
    limits: Option<&std::collections::BTreeMap<u64, SelectionLimit>>,
    reservations: &[Reservation<'_>],
    cancelled: impl Fn() -> bool,
    mut evaluate: impl FnMut(&Terrain<'_>, &Target, f32, f32, f32) -> Result<CandidateCheck, String>,
) -> Result<Option<Solution>, String> {
    if !(1..=4096).contains(&samples) {
        return Err("Invalid versioned placement sample allowance".into());
    }
    terrain.validate()?;
    if targets.len() + reservations.len() > MAX_TARGETS || seed == 0 || seed > i32::MAX as u32 {
        return Err("Invalid placement search limits or seed.".into());
    }
    let mut ids = HashSet::new();
    for t in targets.iter().chain(reservations.iter().map(|r| r.target)) {
        if t.id == 0
            || !ids.insert(t.id)
            || !t.x.is_finite()
            || !t.z.is_finite()
            || t.x.abs() > terrain.size as f32 / 2.
            || t.z.abs() > terrain.size as f32 / 2.
            || !t.radius.is_finite()
            || !(1. ..=terrain.size as f32).contains(&t.radius)
            || t.max_displacement_m.is_some_and(|limit| !(1..=terrain.size as u32).contains(&limit))
            || !t.weight.is_finite()
            || !(0.1..=10.).contains(&t.weight)
            || !t.entry.extent_x.is_finite()
            || !t.entry.extent_z.is_finite()
            || t.entry.extent_x < 0.
            || t.entry.extent_z < 0.
            || t.entry.minimum_size < 0
            || !t.entry.extent_x.hypot(t.entry.extent_z).is_finite()
            || !t.entry.minimum_yaw.is_finite()
            || !t.entry.maximum_yaw.is_finite()
            || t.entry.maximum_yaw < t.entry.minimum_yaw
            || !(t.entry.maximum_yaw - t.entry.minimum_yaw).is_finite()
            || t.bounds
                .as_ref()
                .is_some_and(|b| b.iter().any(|b| !b.valid()))
            || t.spacing.as_ref().is_some_and(|s| !s.valid())
        {
            return Err("Invalid target identity, position, weight or footprint.".into());
        }
    }
    let all_targets = || targets.iter().chain(reservations.iter().map(|r| r.target));
    if all_targets().any(|t| !t.excluded && t.spacing.is_some())
        && all_targets().any(|t| !t.excluded && t.spacing.is_none())
    {
        return Err("Source spacing requires authored bounds for every active target".into());
    }
    for reservation in reservations {
        let p = reservation.position;
        if reservation.target.excluded
            || [p.x, p.y, p.z, p.yaw, p.clearance]
                .iter()
                .any(|v| !v.is_finite())
            || p.clearance < 0.
            || p.x.abs() > terrain.size as f32 / 2.
            || p.z.abs() > terrain.size as f32 / 2.
        {
            return Err("Invalid earlier-phase placement reservation".into());
        }
    }
    if let Some(limits) = limits {
        let mut groups = std::collections::BTreeMap::new();
        for (id, limit) in limits {
            if !ids.contains(id) || limit.group <= 0 || !(1..=MAX_TARGETS).contains(&limit.maximum)
            {
                return Err("Invalid installation stage selection limit".into());
            }
            if groups
                .insert(limit.group, limit.maximum)
                .is_some_and(|previous| previous != limit.maximum)
            {
                return Err("Conflicting installation stage selection limits".into());
            }
        }
        for (group, maximum) in groups {
            let reserved = reservations
                .iter()
                .filter(|r| limits.get(&r.target.id).is_some_and(|l| l.group == group))
                .count();
            if reserved
                + targets
                    .iter()
                    .filter(|t| {
                        t.required
                            && !t.excluded
                            && limits.get(&t.id).is_some_and(|l| l.group == group)
                    })
                    .count()
                > maximum
            {
                return Err("Required monuments exceed their installation stage quota".into());
            }
        }
    }
    if let Some(proposals) = proposals
        && (proposals.len() > MAX_TARGETS
            || proposals.values().map(|p| p.points.len()).sum::<usize>() > 65536
            || proposals.iter().any(|(id, p)| {
                !ids.contains(id)
                    || p.points.len() > 2048
                    || !(1..=32).contains(&p.maximum_accepted)
                    || p.points
                        .iter()
                        .flatten()
                        .any(|v| !v.is_finite() || v.abs() > terrain.size as f32 * 0.5)
            }))
    {
        return Err("Invalid prepared placement proposals".into());
    }
    let mut ordered: Vec<_> = targets.iter().collect();
    ordered.sort_by_key(|t| t.id);
    let mut pools = Vec::with_capacity(ordered.len());
    let mut reasons = Vec::with_capacity(ordered.len());
    let mut exhausted: Option<String> = None;
    for t in &ordered {
        if cancelled() {
            return Ok(None);
        }
        let mut pool = Vec::new();
        let mut reason = String::new();
        if t.excluded {
            reason = "Excluded by presence policy".into();
        } else if limit_reached(t.id, reservations.iter().map(|r| r.target.id), limits) {
            reason = "Installation stage target count reached".into();
        } else if !t.entry.land {
            reason = "This search supports land monuments only".into();
        } else if t.entry.minimum_size > terrain.size as i32 {
            reason = format!("Requires a world at least {} m wide", t.entry.minimum_size);
        } else if let Some(message) = &exhausted {
            reason = message.clone();
        } else {
            let mut rng = u64::from(seed) ^ t.id.wrapping_mul(0xd6e8feb86659fd93);
            for n in 0..if t.exact { 1 } else { samples } {
                if cancelled() {
                    return Ok(None);
                }
                let (x, z) = if n == 0 {
                    (t.x, t.z)
                } else if t.max_displacement_m.is_some()
                    || (if balanced { n % 2 == 1 } else { n < samples * 3 / 4 })
                {
                    let angle = random(&mut rng) * std::f32::consts::TAU;
                    let radius = random(&mut rng).sqrt() * t.max_displacement_m.map_or(t.radius, |limit| limit as f32);
                    (t.x + angle.cos() * radius, t.z + angle.sin() * radius)
                } else {
                    (
                        (random(&mut rng) - 0.5) * terrain.size as f32,
                        (random(&mut rng) - 0.5) * terrain.size as f32,
                    )
                };
                // Consume the original proposal stream first, preserving legacy
                // output and the RNG sequence for fallback samples and yaw.
                let (x, z) = if !t.exact && n % 2 == 1 {
                    proposals
                        .and_then(|p| p.get(&t.id))
                        .and_then(|p| p.points.get((n - 1) / 2))
                        .filter(|p| t.max_displacement_m.is_none_or(|limit| (p[0] - t.x).hypot(p[1] - t.z) <= limit as f32))
                        .map_or((x, z), |p| (p[0], p[1]))
                } else {
                    (x, z)
                };
                let yaw = t.entry.minimum_yaw
                    + (t.entry.maximum_yaw - t.entry.minimum_yaw) * random(&mut rng);
                let checked = evaluate(&terrain, t, x, z, yaw)?;
                let relaxation = match &checked {
                    CandidateCheck::AcceptedRelaxed(_, _, rank) => *rank,
                    _ => 0,
                };
                match checked {
                    CandidateCheck::Accepted(position, grade)
                    | CandidateCheck::AcceptedRelaxed(position, grade, _) => {
                        if !grade.is_finite()
                            || relaxation > 3
                            || grade < 0.
                            || [
                                position.x,
                                position.y,
                                position.z,
                                position.yaw,
                                position.clearance,
                            ]
                            .iter()
                            .any(|v| !v.is_finite())
                            || position.clearance < 0.
                        {
                            return Err("Invalid candidate assessment".into());
                        }
                        let distance = (position.x - t.x).hypot(position.z - t.z);
                        if t.max_displacement_m.is_some_and(|limit| distance > limit as f32) {
                            reason = "Fitted site is beyond the monument move limit".into();
                            continue;
                        }
                        let preference = t.max_displacement_m.map_or(
                            f64::from(t.weight) * f64::from(distance / t.radius).powi(2),
                            |limit| 128. * f64::from(distance / limit as f32).powi(2),
                        );
                        let cost = preference
                            + f64::from(grade / MAX_GRADE) * 0.35
                            + f64::from(random(&mut rng)) * 0.025;
                        let candidate = Candidate {
                            reference: [x, terrain.height_at(x, z), z],
                            position,
                            cost,
                            distance,
                            relaxation,
                        };
                        if reservations.iter().any(|r| {
                            candidate_overlaps_position(t, &candidate, r.target, r.position)
                        }) {
                            reason = "Candidate conflicts with an earlier-phase placement".into();
                            continue;
                        }
                        pool.push(candidate);
                        let limit = proposals
                            .and_then(|p| p.get(&t.id))
                            .map_or(if recover_required { 32 } else { 8 }, |p| {
                                p.maximum_accepted
                            });
                        if balanced && pool.len() >= limit {
                            break;
                        }
                    }
                    CandidateCheck::Rejected(r) => reason = r,
                    CandidateCheck::Exhausted(r) => {
                        reason = format!("Search budget exhausted: {r}");
                        exhausted = Some(reason.clone());
                        break;
                    }
                    CandidateCheck::TargetExhausted(r) => {
                        reason = format!("Target search budget exhausted: {r}");
                        break;
                    }
                    CandidateCheck::Cancelled => return Ok(None),
                }
            }
            pool.sort_by(|a, b| {
                a.relaxation
                    .cmp(&b.relaxation)
                    .then(a.cost.total_cmp(&b.cost))
            });
            if pool.is_empty() && !t.exact {
                reason = if detailed_reasons {
                    format!("No feasible authored site in sampled search: {reason}")
                } else {
                    "Search budget found no feasible sampled footprint (bounds, water or slope)"
                        .into()
                };
            }
        }
        pools.push(pool);
        reasons.push(reason);
    }
    let mut best: Option<Trial> = None;
    let mut packing_work = 0;
    for pass in 0..TRIALS {
        if cancelled() {
            return Ok(None);
        }
        let mut indices: Vec<_> = (0..ordered.len()).collect();
        indices.sort_by(|&a, &b| {
            let ta = ordered[a];
            let tb = ordered[b];
            tb.required
                .cmp(&ta.required)
                .then(tb.exact.cmp(&ta.exact))
                .then(tb.max_displacement_m.is_some().cmp(&ta.max_displacement_m.is_some()))
                .then(tb.priority.cmp(&ta.priority))
                .then_with(|| {
                    if pass == 0 {
                        tb.weight.total_cmp(&ta.weight)
                    } else {
                        let mut ra = u64::from(seed) ^ ta.id ^ (pass as u64 * 971);
                        let mut rb = u64::from(seed) ^ tb.id ^ (pass as u64 * 971);
                        random(&mut ra).total_cmp(&random(&mut rb))
                    }
                })
                .then(ta.id.cmp(&tb.id))
        });
        let mut chosen = vec![None; ordered.len()];
        let mut placed: Vec<(usize, &Candidate)> = Vec::new();
        let (mut required_missing, mut missing, mut cost) = (0, 0, 0.);
        let mut relaxation = 0;
        for &i in &indices {
            if cancelled() {
                return Ok(None);
            }
            if ordered[i].excluded {
                continue;
            }
            if limit_reached(
                ordered[i].id,
                placed
                    .iter()
                    .map(|(j, _)| ordered[*j].id)
                    .chain(reservations.iter().map(|r| r.target.id)),
                limits,
            ) {
                continue;
            }
            let candidate = pools[i].iter().enumerate().find(|(_, c)| {
                !placed
                    .iter()
                    .any(|(j, p)| candidate_overlap(ordered[i], c, ordered[*j], p))
            });
            if let Some((j, c)) = candidate {
                chosen[i] = Some(j);
                placed.push((i, c));
                cost += c.cost;
                relaxation += usize::from(c.relaxation);
            } else {
                missing += 1;
                cost += 1000. * f64::from(ordered[i].weight);
                if ordered[i].required {
                    required_missing += 1;
                }
            }
        }
        if recover_required && required_missing > 0 {
            if let Some(recovered) = recover_required_candidates(
                &ordered,
                &pools,
                &indices,
                required_missing,
                &mut packing_work,
                &cancelled,
            ) {
                chosen.fill(None);
                placed.clear();
                required_missing = 0;
                missing = 0;
                relaxation = 0;
                cost = 0.;
                for &i in &indices {
                    if ordered[i].excluded {
                        continue;
                    }
                    if limit_reached(
                        ordered[i].id,
                        placed
                            .iter()
                            .map(|(j, _)| ordered[*j].id)
                            .chain(reservations.iter().map(|r| r.target.id)),
                        limits,
                    ) {
                        continue;
                    }
                    let selected = if ordered[i].required {
                        recovered[i]
                    } else {
                        pools[i].iter().position(|c| {
                            !placed
                                .iter()
                                .any(|(j, p)| candidate_overlap(ordered[i], c, ordered[*j], p))
                        })
                    };
                    if let Some(j) = selected {
                        let c = &pools[i][j];
                        chosen[i] = Some(j);
                        placed.push((i, c));
                        relaxation += usize::from(c.relaxation);
                        cost += c.cost;
                    } else {
                        missing += 1;
                        required_missing += usize::from(ordered[i].required);
                        cost += 1000. * f64::from(ordered[i].weight);
                    }
                }
            }
            if cancelled() {
                return Ok(None);
            }
        }
        let (edited_missing, edited_distance) = ordered.iter().zip(&chosen).zip(&pools)
            .filter(|((target, _), _)| !target.excluded && target.max_displacement_m.is_some())
            .fold((0, 0.), |(missing, distance), ((_, choice), pool)| {
                match choice {
                    Some(index) => (missing, distance + f64::from(pool[*index].distance)),
                    None => (missing + 1, distance),
                }
            });
        let score = (required_missing, edited_missing, edited_distance, missing, relaxation, cost);
        if best.as_ref().is_none_or(|previous| score < previous.score) {
            best = Some(Trial { score, chosen });
        }
    }
    let Some(Trial { score, chosen }) = best else {
        return Err("No search attempts completed.".into());
    };
    let outcomes = ordered
        .iter()
        .enumerate()
        .map(|(i, t)| {
            if let Some(j) = chosen[i] {
                let c = &pools[i][j];
                let mut reason: String = if t.exact {
                    "Exact position; sampled footprint checks passed".into()
                } else if c.distance <= t.radius {
                    "Within preferred radius".into()
                } else {
                    "Outside preferred radius; best feasible sampled fallback".into()
                };
                if c.relaxation > 0 {
                    reason.push_str(&format!("; candidate relaxation level {}", c.relaxation));
                }
                Outcome {
                    id: t.id,
                    position: Some(c.position.clone()),
                    distance: Some(c.distance),
                    within_radius: c.distance <= t.radius,
                    reason,
                }
            } else {
                Outcome {
                    id: t.id,
                    position: None,
                    distance: None,
                    within_radius: false,
                    reason: if limit_reached(t.id, chosen.iter().enumerate().filter_map(|(j, c)| c.map(|_| ordered[j].id)).chain(reservations.iter().map(|r| r.target.id)), limits) {
                        "Installation stage target count reached".into()
                    } else if !pools[i].is_empty() {
                        if ordered.iter().any(|t| t.spacing.is_some()) {
                            "Sampled candidates conflict with monument footprints or source minimum spacing".into()
                        } else { "Sampled candidates conflict with other monument footprints".into() }
                    } else {
                        reasons[i].clone()
                    },
                }
            }
        })
        .collect();
    Ok(Some(Solution {
        outcomes,
        required_missing: score.0,
        attempts: TRIALS,
    }))
}

/// Search only already verified required candidates. Improve presence before
/// repacking optional sites; share a strict work ceiling across all trial orders.
fn recover_required_candidates(
    targets: &[&Target],
    pools: &[Vec<Candidate>],
    order: &[usize],
    missing: usize,
    work: &mut usize,
    cancelled: &impl Fn() -> bool,
) -> Option<Vec<Option<usize>>> {
    let required: Vec<_> = order
        .iter()
        .copied()
        .filter(|&i| targets[i].required && !targets[i].excluded)
        .collect();
    struct Search<'a, F> {
        targets: &'a [&'a Target],
        pools: &'a [Vec<Candidate>],
        order: &'a [usize],
        chosen: Vec<Option<usize>>,
        placed: Vec<(usize, usize)>,
        best: Option<Vec<Option<usize>>>,
        best_count: usize,
        work: &'a mut usize,
        cancelled: &'a F,
    }
    impl<F: Fn() -> bool> Search<'_, F> {
        fn visit(&mut self, depth: usize) -> bool {
            if *self.work >= 1_000_000 || (self.cancelled)() {
                return false;
            }
            *self.work += 1;
            if self.placed.len() + self.order.len() - depth <= self.best_count {
                return false;
            }
            if depth == self.order.len() {
                self.best_count = self.placed.len();
                self.best = Some(self.chosen.clone());
                return self.best_count == self.order.len();
            }
            let i = self.order[depth];
            for j in 0..self.pools[i].len() {
                let mut clear = true;
                for &(other, k) in &self.placed {
                    if *self.work >= 1_000_000 {
                        return false;
                    }
                    *self.work += 1;
                    if candidate_overlap(
                        self.targets[i],
                        &self.pools[i][j],
                        self.targets[other],
                        &self.pools[other][k],
                    ) {
                        clear = false;
                        break;
                    }
                }
                if clear {
                    self.chosen[i] = Some(j);
                    self.placed.push((i, j));
                    let complete = self.visit(depth + 1);
                    self.placed.pop();
                    self.chosen[i] = None;
                    if complete {
                        return true;
                    }
                }
            }
            if self.targets[i].exact && !self.pools[i].is_empty() {
                false
            } else {
                self.visit(depth + 1)
            }
        }
    }
    let mut search = Search {
        targets,
        pools,
        order: &required,
        chosen: vec![None; targets.len()],
        placed: vec![],
        best: None,
        best_count: required.len().saturating_sub(missing),
        work,
        cancelled,
    };
    search.visit(0);
    search.best
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dungeon_spacing_uses_rotated_embedded_entrances_and_three_dimensions() {
        let mut a = target(1, -300.);
        let mut b = target(2, 300.);
        let spacing = |offset| Spacing {
            shape: crate::monument_spacing::BoxShape {
                center: [0.; 3],
                extent: [1.; 3],
            },
            family: "test".into(),
            minimum_same: 0.,
            minimum_different: 0.,
            entrance: Some(EntranceSpacing {
                offset,
                minimum_distance: 540.,
            }),
        };
        a.spacing = Some(spacing([200., 0., 0.]));
        b.spacing = Some(spacing([-200., 0., 0.]));
        let pa = Position {
            x: -300.,
            y: 0.,
            z: 0.,
            yaw: 0.,
            clearance: 1.,
        };
        let pb = Position {
            x: 300.,
            ..pa.clone()
        };
        let candidate = |position: Position| Candidate {
            reference: [position.x, position.y, position.z],
            position,
            cost: 0.,
            distance: 0.,
            relaxation: 0,
        };
        let ca = candidate(pa);
        let mut cb = candidate(pb);
        assert!(candidate_overlap(&a, &ca, &b, &cb));
        cb.position.yaw = 180.;
        assert!(!candidate_overlap(&a, &ca, &b, &cb));
        cb.position.yaw = 0.;
        cb.position.y = 600.;
        assert!(!candidate_overlap(&a, &ca, &b, &cb));
        cb.position.y = 0.;
        a.spacing
            .as_mut()
            .unwrap()
            .entrance
            .as_mut()
            .unwrap()
            .minimum_distance = 300.;
        assert!(!candidate_overlap(&a, &ca, &b, &cb));
        assert!(candidate_overlap(&b, &cb, &a, &ca));
        b.spacing.as_mut().unwrap().entrance = None;
        assert!(!candidate_overlap(&a, &ca, &b, &cb));
        a.spacing
            .as_mut()
            .unwrap()
            .entrance
            .as_mut()
            .unwrap()
            .minimum_distance = f32::NAN;
        assert!(!a.spacing.as_ref().unwrap().valid());
    }
    #[test]
    fn fitted_candidate_distance_tracks_the_accepted_position() {
        let heights = vec![18000; 65 * 65];
        let mut target = target(1, 0.);
        target.exact = true;
        let result = solve_with_candidates(
            Terrain {
                heights: &heights,
                resolution: 65,
                size: 1000,
            },
            &[target],
            1,
            true,
            || false,
            |_, _, _, _, yaw| {
                Ok(CandidateCheck::Accepted(
                    Position {
                        x: 3.,
                        y: 20.,
                        z: 4.,
                        yaw,
                        clearance: 1.,
                    },
                    0.,
                ))
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.outcomes[0].distance, Some(5.));
    }
    #[test]
    fn installation_quota_counts_fitted_choices_and_keeps_alternative_prefabs() {
        let heights = vec![18000; 65 * 65];
        let mut targets = [target(1, -200.), target(2, 0.), target(3, 200.)];
        for t in &mut targets {
            t.required = false;
        }
        let limits = targets
            .iter()
            .map(|t| {
                (
                    t.id,
                    SelectionLimit {
                        group: 7,
                        maximum: 1,
                    },
                )
            })
            .collect();
        let run = |targets: &[Target]| {
            solve_with_policy_limits(
                Terrain {
                    heights: &heights,
                    resolution: 65,
                    size: 1000,
                },
                targets,
                123,
                true,
                true,
                8,
                true,
                None,
                Some(&limits),
                || false,
                |_, t, _, _, _| {
                    Ok(if t.id == 1 {
                        CandidateCheck::Rejected("No feasible terrain".into())
                    } else {
                        CandidateCheck::Accepted(
                            Position {
                                x: t.x,
                                y: 20.,
                                z: 0.,
                                yaw: 0.,
                                clearance: 1.,
                            },
                            0.,
                        )
                    })
                },
            )
            .unwrap()
            .unwrap()
        };
        let solution = run(&targets);
        assert_eq!(solution.required_missing, 0);
        assert_eq!(
            solution
                .outcomes
                .iter()
                .filter(|o| o.position.is_some())
                .count(),
            1
        );
        assert!(solution.outcomes[0].position.is_none());
        assert!(
            solution
                .outcomes
                .iter()
                .any(|o| o.reason == "Installation stage target count reached")
        );
        targets.reverse();
        assert_eq!(run(&targets), solution);
        for t in &mut targets {
            t.required = true;
        }
        let error = solve_with_policy_limits(
            Terrain {
                heights: &heights,
                resolution: 65,
                size: 1000,
            },
            &targets,
            123,
            true,
            true,
            8,
            true,
            None,
            Some(&limits),
            || false,
            |_, _, _, _, _| unreachable!(),
        )
        .unwrap_err();
        assert!(error.contains("Required monuments exceed"));
    }
    #[test]
    fn required_packing_retries_validated_choices_and_preserves_exact_pins() {
        let heights = vec![18000; 65 * 65];
        let mut targets = [target(1, 0.), target(2, 0.), target(3, -200.)];
        targets[0].priority = 10;
        let pos = |x| Position {
            x,
            y: 20.,
            z: 0.,
            yaw: 0.,
            clearance: 1.,
        };
        let run = |targets: &[Target], recover| {
            let mut calls = [0; 4];
            solve_with_policy_packing(
                Terrain {
                    heights: &heights,
                    resolution: 65,
                    size: 1000,
                },
                targets,
                123,
                true,
                true,
                16,
                recover,
                || false,
                |_, t, _, _, _| {
                    let n = calls[t.id as usize];
                    calls[t.id as usize] += 1;
                    Ok(if t.id == 1 && n >= 8 {
                        CandidateCheck::AcceptedRelaxed(pos(200.), 0., 1)
                    } else {
                        CandidateCheck::Accepted(pos(if t.id == 3 { -200. } else { 0. }), 0.)
                    })
                },
            )
            .unwrap()
            .unwrap()
        };
        assert_eq!(run(&targets, false).required_missing, 1);
        let recovered = run(&targets, true);
        assert_eq!(recovered.required_missing, 0);
        assert_eq!(recovered.outcomes[0].position.as_ref().unwrap().x, 200.);
        assert_eq!(recovered, run(&targets, true));
        targets[0].exact = true;
        let pinned = run(&targets, true);
        assert_eq!(pinned.required_missing, 1);
        assert_eq!(pinned.outcomes[0].position.as_ref().unwrap().x, 0.);
        let refs: Vec<_> = targets.iter().collect();
        let pools: Vec<_> = targets
            .iter()
            .map(|_| {
                vec![Candidate {
                    reference: [0.; 3],
                    position: pos(0.),
                    cost: 0.,
                    distance: 0.,
                    relaxation: 0,
                }]
            })
            .collect();
        let mut spent = 1_000_000;
        assert!(
            recover_required_candidates(&refs, &pools, &[0, 1, 2], 1, &mut spent, &|| false)
                .is_none()
        );
        assert_eq!(spent, 1_000_000);
        assert!(
            recover_required_candidates(&refs, &pools, &[0, 1, 2], 1, &mut 0, &|| true).is_none()
        );
    }
    #[test]
    fn relaxation_prefers_strict_sites_but_can_fill_a_required_conflict() {
        let heights = vec![18000; 65 * 65];
        let run = |targets: &[Target], mut check: Box<dyn FnMut(&Target) -> CandidateCheck>| {
            solve_with_policy_samples(
                Terrain {
                    heights: &heights,
                    resolution: 65,
                    size: 1000,
                },
                targets,
                123,
                true,
                true,
                16,
                || false,
                |_, t, _, _, _| Ok(check(t)),
            )
            .unwrap()
            .unwrap()
        };
        let pos = |x| Position {
            x,
            y: 20.,
            z: 0.,
            yaw: 0.,
            clearance: 1.,
        };
        let calls = std::cell::Cell::new(0);
        let single = run(
            &[target(1, 0.)],
            Box::new(move |_| {
                let n = calls.get();
                calls.set(n + 1);
                if n == 0 {
                    CandidateCheck::AcceptedRelaxed(pos(0.), 0., 1)
                } else {
                    CandidateCheck::Accepted(pos(200.), 0.)
                }
            }),
        );
        assert_eq!(single.outcomes[0].position.as_ref().unwrap().x, 200.);
        assert!(!single.outcomes[0].reason.contains("relaxation"));
        let mut pinned = target(1, 0.);
        pinned.exact = true;
        let targets = [pinned, target(2, 0.)];
        let calls = std::cell::Cell::new(0);
        let filled = run(
            &targets,
            Box::new(move |t| {
                if t.id == 1 {
                    return CandidateCheck::Accepted(pos(0.), 0.);
                }
                let n = calls.get();
                calls.set(n + 1);
                if n == 0 {
                    CandidateCheck::Accepted(pos(0.), 0.)
                } else {
                    CandidateCheck::AcceptedRelaxed(pos(200.), 0., 1)
                }
            }),
        );
        assert_eq!(filled.required_missing, 0);
        assert_eq!(filled.outcomes[1].position.as_ref().unwrap().x, 200.);
        assert!(filled.outcomes[1].reason.contains("relaxation level 1"));
        let blocked = run(
            &targets,
            Box::new(move |_| CandidateCheck::AcceptedRelaxed(pos(0.), 0., 1)),
        );
        assert_eq!(blocked.required_missing, 1);
    }
    #[test]
    fn earlier_placements_reserve_space_before_required_repacking() {
        let heights = vec![18000; 65 * 65];
        let previous = target(1, 0.);
        let pose = Position {
            x: 0.,
            y: 20.,
            z: 0.,
            yaw: 0.,
            clearance: 50.,
        };
        let reservations = [Reservation {
            target: &previous,
            position: &pose,
        }];
        let mut active = [target(2, 0.), target(3, 0.)];
        for target in &mut active {
            target.priority = 100;
        }
        let run = |targets: &[Target], cancelled| {
            let mut calls = [0; 4];
            solve_with_reservations(
                Terrain {
                    heights: &heights,
                    resolution: 65,
                    size: 1000,
                },
                targets,
                123,
                true,
                true,
                4,
                true,
                None,
                None,
                &reservations,
                || cancelled,
                |_, t, _, _, _| {
                    calls[t.id as usize] += 1;
                    Ok(CandidateCheck::Accepted(
                        Position {
                            x: if calls[t.id as usize] == 1 {
                                0.
                            } else {
                                (t.id as f32 - 1.5) * 300.
                            },
                            ..pose.clone()
                        },
                        0.,
                    ))
                },
            )
            .unwrap()
        };
        let result = run(&active, false).unwrap();
        assert_eq!(result.required_missing, 0);
        assert_eq!(result.outcomes.len(), 2);
        assert!(
            result
                .outcomes
                .iter()
                .all(|o| o.position.as_ref().unwrap().x.abs() >= 150.)
        );
        active.reverse();
        assert_eq!(run(&active, false).unwrap(), result);
        assert!(run(&active, true).is_none());
        assert_eq!(pose.x, 0.);
    }

    #[test]
    fn earlier_placements_consume_stage_quota_without_assessment() {
        let heights = vec![18000; 65 * 65];
        let previous = target(1, -200.);
        let pose = Position {
            x: -200.,
            y: 20.,
            z: 0.,
            yaw: 0.,
            clearance: 1.,
        };
        let mut active = target(2, 200.);
        active.required = false;
        let limits = [
            (
                1,
                SelectionLimit {
                    group: 7,
                    maximum: 1,
                },
            ),
            (
                2,
                SelectionLimit {
                    group: 7,
                    maximum: 1,
                },
            ),
        ]
        .into_iter()
        .collect();
        let run = |active: &Target| {
            solve_with_reservations(
                Terrain {
                    heights: &heights,
                    resolution: 65,
                    size: 1000,
                },
                std::slice::from_ref(active),
                123,
                true,
                true,
                4,
                true,
                None,
                Some(&limits),
                &[Reservation {
                    target: &previous,
                    position: &pose,
                }],
                || false,
                |_, _, _, _, _| panic!("Full reserved quota must skip assessment"),
            )
        };
        let result = run(&active).unwrap().unwrap();
        assert!(result.outcomes[0].position.is_none());
        assert_eq!(
            result.outcomes[0].reason,
            "Installation stage target count reached"
        );
        active.required = true;
        assert!(
            run(&active)
                .unwrap_err()
                .contains("Required monuments exceed")
        );
    }

    #[test]
    fn reservation_overlap_uses_fitted_footprints_and_raw_entrance_reference() {
        let footprint = crate::monument_bounds::Footprint {
            center: [0., 0.],
            extent: [10., 10.],
        };
        let mut previous = target(1, 0.);
        let mut active = target(2, 300.);
        previous.bounds = Some([footprint; 2]);
        active.bounds = Some([footprint; 2]);
        let pose = Position {
            x: 0.,
            y: 20.,
            z: 0.,
            yaw: 45.,
            clearance: 1.,
        };
        let mut candidate = Candidate {
            reference: [300., 20., 0.],
            position: pose.clone(),
            cost: 0.,
            distance: 0.,
            relaxation: 0,
        };
        assert!(candidate_overlaps_position(
            &active, &candidate, &previous, &pose
        ));
        candidate.position.x = 300.;
        assert!(!candidate_overlaps_position(
            &active, &candidate, &previous, &pose
        ));
        let spacing = || Spacing {
            shape: crate::monument_spacing::BoxShape {
                center: [0.; 3],
                extent: [10.; 3],
            },
            family: "terrain".into(),
            minimum_same: 400.,
            minimum_different: 0.,
            entrance: None,
        };
        previous.spacing = Some(spacing());
        active.spacing = Some(spacing());
        assert!(candidate_overlaps_position(
            &active, &candidate, &previous, &pose
        ));
        previous.spacing.as_mut().unwrap().minimum_same = 0.;
        active.spacing.as_mut().unwrap().minimum_same = 0.;
        for target in [&mut previous, &mut active] {
            target.spacing.as_mut().unwrap().entrance = Some(EntranceSpacing {
                offset: [0.; 3],
                minimum_distance: 100.,
            });
        }
        candidate.reference = [0., 20., 0.];
        assert!(candidate_overlaps_position(
            &active, &candidate, &previous, &pose
        ));
        candidate.reference = [300., 20., 0.];
        assert!(!candidate_overlaps_position(
            &active, &candidate, &previous, &pose
        ));
    }

    #[test]
    fn invalid_or_duplicate_reservations_are_rejected_before_assessment() {
        let heights = vec![18000; 65 * 65];
        let previous = target(1, 0.);
        let active = target(2, 200.);
        let pose = Position {
            x: 0.,
            y: 20.,
            z: 0.,
            yaw: 0.,
            clearance: 1.,
        };
        let run = |target, position| {
            solve_with_reservations(
                Terrain {
                    heights: &heights,
                    resolution: 65,
                    size: 1000,
                },
                std::slice::from_ref(&active),
                123,
                true,
                true,
                4,
                true,
                None,
                None,
                &[Reservation { target, position }],
                || false,
                |_, _, _, _, _| panic!("Invalid reservation reached assessment"),
            )
        };
        assert!(run(&active, &pose).is_err());
        let invalid_yaw = Position {
            yaw: f32::NAN,
            ..pose.clone()
        };
        assert!(run(&previous, &invalid_yaw).is_err());
        let outside = Position { x: 501., ..pose };
        assert!(run(&previous, &outside).is_err());
    }

    fn target(id: u64, x: f32) -> Target {
        Target {
            bounds: None,
            spacing: None,
            priority: 0,
            id,
            x,
            z: 0.,
            radius: 120.,
            max_displacement_m: None,
            weight: 1.,
            exact: false,
            required: true,
            excluded: false,
            entry: Entry {
                group: 0,
                resolved: 0,
                land: true,
                minimum_size: 1000,
                extent_x: 20.,
                extent_z: 20.,
                priority: 1,
                minimum_yaw: 0.,
                maximum_yaw: 0.,
            },
        }
    }
    fn run(height: &[i16], targets: &[Target], seed: u32) -> Solution {
        solve(
            Terrain {
                heights: height,
                resolution: 65,
                size: 1000,
            },
            targets,
            seed,
            || false,
        )
        .unwrap()
        .unwrap()
    }
    #[test]
    fn source_spacing_gates_all_packing_and_recovery_without_moving_exact_pins() {
        let heights = vec![17000; 65 * 65];
        let mut a = target(1, 0.);
        let mut b = target(2, 130.);
        a.exact = true;
        b.exact = true;
        a.spacing = Some(Spacing {
            shape: crate::monument_spacing::BoxShape {
                center: [0.; 3],
                extent: [20.; 3],
            },
            family: "same".into(),
            minimum_same: 100.,
            minimum_different: 30.,
            entrance: None,
        });
        b.spacing = a.spacing.clone();
        b.spacing.as_mut().unwrap().minimum_same = 0.;
        let solve = |targets: &[Target], y: f32, recover| {
            solve_with_policy_proposals(
                Terrain {
                    heights: &heights,
                    resolution: 65,
                    size: 1000,
                },
                targets,
                42,
                true,
                true,
                32,
                recover,
                None,
                || false,
                |_, t, x, z, yaw| {
                    Ok(CandidateCheck::Accepted(
                        Position {
                            x,
                            y: if t.id == 2 { y } else { 0. },
                            z,
                            yaw,
                            clearance: 1.,
                        },
                        0.,
                    ))
                },
            )
            .unwrap()
            .unwrap()
        };
        for recover in [false, true] {
            let blocked = solve(&[a.clone(), b.clone()], 0., recover);
            assert_eq!(blocked.required_missing, 1);
            assert!(
                blocked
                    .outcomes
                    .iter()
                    .find(|o| o.position.is_none())
                    .unwrap()
                    .reason
                    .contains("spacing")
            );
            assert_eq!(blocked, solve(&[b.clone(), a.clone()], 0., recover));
            // The source compares box-edge distances in 3D, not pivot circles.
            assert_eq!(
                solve(&[a.clone(), b.clone()], 100., recover).required_missing,
                0
            );
        }
        b.x = 140.;
        assert_eq!(solve(&[a.clone(), b.clone()], 0., true).required_missing, 0);
        b.x = 130.;
        b.spacing.as_mut().unwrap().family = "different".into();
        assert_eq!(solve(&[a.clone(), b.clone()], 0., true).required_missing, 0);
        b.spacing = None;
        assert!(
            solve_with_policy_proposals(
                Terrain {
                    heights: &heights,
                    resolution: 65,
                    size: 1000
                },
                &[a.clone(), b.clone()],
                42,
                true,
                true,
                32,
                true,
                None,
                || false,
                |_, _, _, _, _| unreachable!()
            )
            .is_err()
        );
        a.spacing = None;
        assert_eq!(solve(&[a.clone(), b.clone()], 0., true).required_missing, 0);
        a.spacing = Some(Spacing {
            shape: crate::monument_spacing::BoxShape {
                center: [0.; 3],
                extent: [20.; 3],
            },
            family: "same".into(),
            minimum_same: f32::NAN,
            minimum_different: 0.,
            entrance: None,
        });
        assert!(
            solve_with_policy_proposals(
                Terrain {
                    heights: &heights,
                    resolution: 65,
                    size: 1000
                },
                &[a],
                42,
                true,
                true,
                32,
                true,
                None,
                || false,
                |_, _, _, _, _| unreachable!()
            )
            .is_err()
        );
    }
    #[test]
    fn prepared_proposals_keep_preferences_costs_exact_pins_and_evaluation_authority() {
        let heights = vec![17000; 65 * 65];
        let targets = [target(1, 0.)];
        let terrain = || Terrain {
            heights: &heights,
            resolution: 65,
            size: 1000,
        };
        let proposals = std::collections::BTreeMap::from([(
            1,
            ProposalSet {
                points: vec![[250., 0.]],
                maximum_accepted: 1,
            },
        )]);
        let seen = std::cell::RefCell::new(Vec::new());
        let evaluate = |_: &Terrain<'_>, t: &Target, x: f32, z: f32, yaw: f32| {
            seen.borrow_mut().push([x, z]);
            Ok(if x == 250. && z == 0. {
                CandidateCheck::Accepted(
                    Position {
                        x,
                        y: 20.,
                        z,
                        yaw,
                        clearance: t.entry.extent_x,
                    },
                    0.,
                )
            } else {
                CandidateCheck::Rejected("Synthetic site is not feasible".into())
            })
        };
        let result = solve_with_policy_proposals(
            terrain(),
            &targets,
            42,
            true,
            true,
            32,
            true,
            Some(&proposals),
            || false,
            evaluate,
        )
        .unwrap()
        .unwrap();
        assert_eq!(&*seen.borrow(), &[[0., 0.], [250., 0.]]);
        assert_eq!(result.outcomes[0].distance, Some(250.));
        assert!(!result.outcomes[0].within_radius);
        let mut pinned = targets[0].clone();
        pinned.exact = true;
        seen.borrow_mut().clear();
        let exact = solve_with_policy_proposals(
            terrain(),
            &[pinned],
            42,
            true,
            true,
            32,
            true,
            Some(&proposals),
            || false,
            evaluate,
        )
        .unwrap()
        .unwrap();
        assert_eq!(exact.required_missing, 1);
        assert_eq!(&*seen.borrow(), &[[0., 0.]]);
        let legacy = solve_with_policy_packing(
            terrain(),
            &targets,
            42,
            true,
            true,
            32,
            true,
            || false,
            |raw, t, x, z, yaw| raw.check_candidate(t, x, z, yaw),
        )
        .unwrap();
        let empty = solve_with_policy_proposals(
            terrain(),
            &targets,
            42,
            true,
            true,
            32,
            true,
            None,
            || false,
            |raw, t, x, z, yaw| raw.check_candidate(t, x, z, yaw),
        )
        .unwrap();
        assert_eq!(legacy, empty);
        let invalid = std::collections::BTreeMap::from([(
            1,
            ProposalSet {
                points: vec![[f32::NAN, 0.]],
                maximum_accepted: 1,
            },
        )]);
        assert!(
            solve_with_policy_proposals(
                terrain(),
                &targets,
                42,
                true,
                true,
                32,
                true,
                Some(&invalid),
                || false,
                evaluate
            )
            .is_err()
        );
        assert!(
            solve_with_policy_proposals(
                terrain(),
                &targets,
                42,
                true,
                true,
                32,
                true,
                Some(&proposals),
                || true,
                evaluate
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn hard_move_limit_checks_the_fitted_position_and_rejects_far_fallbacks() {
        let heights = vec![17000; 65 * 65];
        let mut target = target(1, 0.);
        target.max_displacement_m = Some(25);
        let terrain = || Terrain { heights: &heights, resolution: 65, size: 1000 };
        let proposals = std::collections::BTreeMap::from([(
            1, ProposalSet { points: vec![[250., 0.]], maximum_accepted: 1 },
        )]);
        let run = |fitted_x| solve_with_policy_proposals(
            terrain(), std::slice::from_ref(&target), 42, true, true, 32, true,
            Some(&proposals), || false, |_, _, x, z, yaw| {
                assert!(x.hypot(z) <= 25.);
                Ok(CandidateCheck::Accepted(
                    Position { x: fitted_x, y: 20., z: 0., yaw, clearance: 10. }, 0.,
                ))
            },
        ).unwrap().unwrap();
        assert_eq!(run(25.).outcomes[0].distance, Some(25.));
        let outside = run(25.01);
        assert!(outside.outcomes[0].position.is_none());
        assert_eq!(outside.required_missing, 1);
        assert!(outside.outcomes[0].reason.contains("move limit"));
    }

    #[test]
    fn edited_monument_gets_first_choice_of_overlapping_sites() {
        let heights = vec![17000; 65 * 65];
        let mut automatic = target(1, 0.);
        automatic.priority = 5;
        let mut moved = target(2, 0.);
        moved.max_displacement_m = Some(25);
        let result = run(&heights, &[automatic, moved], 42);
        let edited = result.outcomes.iter().find(|item| item.id == 2).unwrap();
        assert!(edited.distance.unwrap() < 0.01);
        assert_eq!(result.required_missing, 0);
    }
    #[test]
    fn repeatable_reordered_and_independent_inputs() {
        let h = vec![17000; 65 * 65];
        let a = target(1, 0.);
        let b = target(2, 0.);
        let s = run(&h, &[a.clone(), b.clone()], 42);
        assert_eq!(s, run(&h, &[b.clone(), a.clone()], 42));
        assert_eq!(s.required_missing, 0);
        assert_ne!(s, run(&h, &[a, b], 43));
        assert!(h.iter().all(|&v| v == 17000));
    }
    #[test]
    fn pins_overlap_exclusions_and_water_are_explicit() {
        let h = vec![17000; 65 * 65];
        let mut a = target(1, 0.);
        a.exact = true;
        let mut b = a.clone();
        b.id = 2;
        let s = run(&h, &[a.clone(), b], 42);
        assert_eq!(s.required_missing, 1);
        assert!(s.outcomes.iter().any(|o| o.reason.contains("conflict")));
        a.excluded = true;
        let s = run(&h, &[a.clone()], 42);
        assert!(s.outcomes[0].reason.contains("Excluded"));
        a.excluded = false;
        let s = run(&vec![10000; 65 * 65], &[a], 42);
        assert_eq!(s.required_missing, 1);
        assert!(s.outcomes[0].reason.contains("submerged"));
    }
    #[test]
    fn checks_footprint_not_just_center() {
        let mut h = vec![17000; 65 * 65];
        h[32 * 65 + 33] = 10000;
        let mut a = target(1, 0.);
        a.exact = true;
        let s = run(&h, &[a], 42);
        assert!(s.outcomes[0].position.is_none());
    }
    #[test]
    fn rejects_invalid_grids_targets_and_honors_cancellation() {
        let h = vec![17000; 65 * 65];
        let mut a = target(1, 0.);
        a.weight = f32::NAN;
        assert!(
            solve(
                Terrain {
                    heights: &h,
                    resolution: 65,
                    size: 1000
                },
                &[a],
                42,
                || false
            )
            .is_err()
        );
        assert!(
            solve(
                Terrain {
                    heights: &h,
                    resolution: 65,
                    size: 1000
                },
                &[target(1, 0.)],
                42,
                || true
            )
            .unwrap()
            .is_none()
        );
        assert!(
            solve(
                Terrain {
                    heights: &h[..5],
                    resolution: 65,
                    size: 1000
                },
                &[],
                42,
                || false
            )
            .is_err()
        );
    }
    #[test]
    fn weight_wins_first_trial_when_stronger_exact_prefs_compete() {
        let h = vec![17000; 65 * 65];
        let mut a = target(1, 0.);
        a.exact = true;
        a.weight = 10.;
        let mut b = target(2, 0.);
        b.exact = true;
        b.weight = 0.1;
        let s = run(&h, &[a, b], 42);
        assert!(s.outcomes[0].position.is_some());
    }
    #[test]
    fn outside_radius_fallback_is_reported_and_an_exact_pin_never_moves() {
        let mut h = vec![10000; 65 * 65];
        for z in 0..65 {
            for x in 43..65 {
                h[z * 65 + x] = 17000;
            }
        }
        let mut target = target(1, 0.);
        target.radius = 20.;
        let result = run(&h, &[target.clone()], 42);
        let o = &result.outcomes[0];
        assert!(o.position.is_some());
        assert!(!o.within_radius);
        assert!(o.reason.contains("Outside"));
        assert!(o.distance.unwrap() > 20.);
        target.exact = true;
        assert!(run(&h, &[target], 42).outcomes[0].position.is_none());
    }
    #[test]
    fn source_bounds_pack_rotated_sites_keep_stamp_reservations_and_order_priority() {
        use crate::monument_bounds::Footprint;
        let h = vec![18000; 65 * 65];
        let f = Footprint {
            center: [0., 0.],
            extent: [100., 10.],
        };
        let mut a = target(1, 0.);
        a.exact = true;
        a.bounds = Some([f, f]);
        let mut b = a.clone();
        b.id = 2;
        b.z = 30.;
        let evaluate = |_: &Terrain<'_>, _: &Target, x, z, yaw| {
            Ok(CandidateCheck::Accepted(
                Position {
                    x,
                    y: 50.,
                    z,
                    yaw,
                    clearance: 101.,
                },
                0.,
            ))
        };
        let search = |targets: &[Target]| {
            solve_with_policy(
                Terrain {
                    heights: &h,
                    resolution: 65,
                    size: 1000,
                },
                targets,
                42,
                true,
                true,
                || false,
                evaluate,
            )
            .unwrap()
            .unwrap()
        };
        assert!(
            search(&[a.clone(), b.clone()])
                .outcomes
                .iter()
                .all(|o| o.position.is_some())
        );
        b.bounds.as_mut().unwrap()[1].extent = [100., 40.];
        assert_eq!(
            search(&[a.clone(), b.clone()])
                .outcomes
                .iter()
                .filter(|o| o.position.is_some())
                .count(),
            1,
            "Object separation does not bypass overlapping earthwork"
        );
        b.bounds = Some([f, f]);
        b.z = 0.;
        b.priority = 4;
        let result = search(&[a, b]);
        assert!(result.outcomes[0].position.is_none());
        assert!(result.outcomes[1].position.is_some());
    }
    #[test]
    fn balanced_search_limits_pools_and_a_target_limit_does_not_starve_later_targets() {
        let h = vec![18000; 65 * 65];
        let targets = [target(3, 250.), target(1, -250.), target(2, 0.)];
        let run_balanced = |targets: &[Target]| {
            let mut calls = std::collections::BTreeMap::<u64, usize>::new();
            let mut samples = Vec::new();
            let solution = solve_with_policy(
                Terrain {
                    heights: &h,
                    resolution: 65,
                    size: 1000,
                },
                targets,
                42,
                true,
                true,
                || false,
                |_, t, x, z, yaw| {
                    *calls.entry(t.id).or_default() += 1;
                    samples.push((t.id, x, z));
                    if t.id == 1 {
                        return Ok(CandidateCheck::TargetExhausted("allowance".into()));
                    }
                    Ok(CandidateCheck::Accepted(
                        Position {
                            x,
                            y: 50.,
                            z,
                            yaw,
                            clearance: 1.,
                        },
                        0.,
                    ))
                },
            )
            .unwrap()
            .unwrap();
            (solution, calls, samples)
        };
        let (solution, calls, samples) = run_balanced(&targets);
        assert_eq!(calls.values().copied().collect::<Vec<_>>(), vec![1, 8, 8]);
        assert!(solution.outcomes[0].position.is_none());
        assert!(
            solution.outcomes[0]
                .reason
                .contains("Target search budget exhausted")
        );
        assert!(solution.outcomes[1..].iter().all(|o| o.position.is_some()));
        let target2: Vec<_> = samples.iter().filter(|p| p.0 == 2).collect();
        assert_eq!((target2[0].1, target2[0].2), (0., 0.));
        assert!(target2[1].1.hypot(target2[1].2) <= targets[2].radius);
        assert!(
            target2[2].1.hypot(target2[2].2) > targets[2].radius,
            "World exploration starts before the local pool is consumed"
        );
        let reversed: Vec<_> = targets.iter().rev().cloned().collect();
        assert_eq!(run_balanced(&reversed), (solution, calls, samples));
    }
    #[test]
    fn exhausted_assessment_budget_preserves_only_verified_candidates() {
        let h = vec![18000; 65 * 65];
        let targets = [target(1, -200.), target(2, 0.), target(3, 200.)];
        let mut calls = 0;
        let result = solve_with_candidates(
            Terrain {
                heights: &h,
                resolution: 65,
                size: 1000,
            },
            &targets,
            42,
            true,
            || false,
            |terrain, t, x, z, yaw| {
                calls += 1;
                if calls > 1 {
                    Ok(CandidateCheck::Exhausted("test work limit".into()))
                } else {
                    terrain.check_candidate(t, x, z, yaw)
                }
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(calls, 2, "No further work after the first exhaustion");
        assert!(result.outcomes[0].position.is_some());
        assert!(
            result.outcomes[1..]
                .iter()
                .all(|o| o.position.is_none() && o.reason.contains("budget exhausted"))
        );
        assert_eq!(result.required_missing, 2);
    }
    #[test]
    fn cancellation_during_candidate_search_returns_no_partial_layout() {
        let h = vec![17000; 65 * 65];
        let ticks = std::cell::Cell::new(0);
        let result = solve(
            Terrain {
                heights: &h,
                resolution: 65,
                size: 1000,
            },
            &[target(1, 0.)],
            42,
            || {
                ticks.set(ticks.get() + 1);
                ticks.get() > 10
            },
        )
        .unwrap();
        assert!(result.is_none());
        assert!(ticks.get() > 10);
    }
}
