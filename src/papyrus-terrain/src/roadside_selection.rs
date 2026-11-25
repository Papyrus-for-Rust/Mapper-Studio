//! Bounded grouped roadside selection for upright prefabs without dungeon links
//! or dungeon entrances. Uses captured policies and independent deterministic RNG.
//! The caller must supply terrain/placement/water/environment eligibility.
use crate::{
    monument_spacing::{BoxPose, BoxShape, PreparedBox},
    roadside_monuments::Candidate,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
pub const VERSION: &str = "papyrus-roadside-group-selection-1";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DistanceMode {
    Any,
    Min,
    Max,
}
impl TryFrom<u8> for DistanceMode {
    type Error = String;
    fn try_from(v: u8) -> Result<Self, String> {
        match v {
            0 => Ok(Self::Any),
            1 => Ok(Self::Min),
            2 => Ok(Self::Max),
            _ => Err("Unknown roadside distance mode".into()),
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Preference {
    pub position: [f32; 2],
    pub radius: f32,
    pub max_displacement_m: Option<u32>,
    pub weight: f64,
    pub exact: bool,
    pub yaw: Option<f32>,
}
#[derive(Clone, Copy, Debug)]
pub struct Request {
    pub id: u64,
    pub required: bool,
    pub excluded: bool,
    pub preference: Option<Preference>,
}
pub struct Group<'a> {
    pub key: u64,
    pub priority: u8,
    pub shape: BoxShape,
    pub requests: &'a [Request],
    pub candidates: &'a [Candidate],
}
#[derive(Clone, Copy, Debug)]
pub struct Policy {
    pub seed: u32,
    pub attempts: usize,
    pub successful_candidates: usize,
    pub target_count: usize,
    pub minimum_same: f64,
    pub minimum_other: f64,
    pub same_mode: DistanceMode,
    pub other_mode: DistanceMode,
    pub maximum_work: usize,
}
#[derive(Debug, PartialEq)]
pub enum Failure {
    Invalid(&'static str),
    Budget,
    Evaluation(String),
}
pub struct Work {
    used: usize,
    limit: usize,
}
impl Work {
    pub fn used(&self) -> usize {
        self.used
    }
    pub fn remaining(&self) -> usize {
        self.limit.saturating_sub(self.used)
    }
    pub fn spend(&mut self, count: usize) -> Result<(), Failure> {
        self.used = self.used.checked_add(count).ok_or(Failure::Budget)?;
        if self.used > self.limit {
            return Err(Failure::Budget);
        }
        Ok(())
    }
}
pub enum Check {
    Accepted { height: f32 },
    Rejected(&'static str),
    Cancelled,
}
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Selected {
    pub group: u64,
    pub request: u64,
    pub candidate: usize,
    pub road: usize,
    pub start_index: usize,
    pub end_index: usize,
    pub position: [f32; 3],
    pub yaw: f32,
    pub score: f64,
}
#[derive(Debug, Serialize)]
pub struct Selection {
    pub engine: &'static str,
    pub selected: Vec<Selected>,
    pub missing: Vec<u64>,
    pub required_missing: Vec<u64>,
    pub attempts: usize,
    pub work: usize,
    pub rejections: BTreeMap<String, usize>,
}
fn next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e3779b97f4a7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^ (z >> 31)
}
fn shuffle<T>(v: &mut [T], seed: &mut u64, work: &mut Work) -> Result<(), Failure> {
    work.spend(v.len())?;
    for i in (1..v.len()).rev() {
        let j = (next(seed) % (i as u64 + 1)) as usize;
        v.swap(i, j);
    }
    Ok(())
}
fn angle(a: f32, b: f32) -> f64 {
    let d = f64::from(a) - f64::from(b);
    (d + 180.).rem_euclid(360.) - 180.
}
fn preference_cost(p: Preference, c: &Candidate, shape: BoxShape) -> f64 {
    let distance = (f64::from(c.position[0]) - f64::from(p.position[0]))
        .hypot(f64::from(c.position[2]) - f64::from(p.position[1]));
    let outside = (distance - f64::from(p.radius)).max(0.);
    let yaw = p.yaw.map_or(0., |y| {
        angle(c.yaw, y).to_radians() * f64::from(shape.extent[0].max(shape.extent[2]))
    });
    outside * outside + yaw * yaw
}
fn nearest(
    pose: &PreparedBox,
    others: &[PreparedBox],
    work: &mut Work,
) -> Result<Option<f64>, Failure> {
    let mut minimum = None;
    for other in others {
        work.spend(1)?;
        if let Some(distance) = pose.squared_distance(other, minimum) {
            minimum = Some(minimum.map_or(distance, |v: f64| v.min(distance)));
        }
    }
    Ok(minimum)
}
fn spaced(same: Option<f64>, other: Option<f64>, p: Policy) -> bool {
    same.is_none_or(|d| d >= p.minimum_same * p.minimum_same)
        && other.is_none_or(|d| d >= p.minimum_other * p.minimum_other)
}
fn score(priority: u8, same: Option<f64>, other: Option<f64>, p: Policy) -> f64 {
    let term = |v: Option<f64>, mode: DistanceMode, scale: f64| {
        v.map_or(0., |d| match mode {
            DistanceMode::Any => 0.,
            DistanceMode::Min => -(d * scale).round_ties_even(),
            DistanceMode::Max => (d * scale).round_ties_even(),
        })
    };
    // Wider arithmetic intentionally avoids the source's signed-int overflow;
    // source priority/distance scoring structure is retained, not RNG parity.
    100000. * f64::from(priority + 1).powi(4)
        + term(same, p.same_mode, 2.)
        + term(other, p.other_mode, 1.)
}
/// Existing boxes are different-family monuments. Only use this entry point for
/// prefabs proven to have no dungeon links/entrances; dungeon-aware classification
/// and snapping require a separate adapter. Inputs remain immutable on all exits.
pub fn solve(
    groups: &[Group<'_>],
    existing: &[BoxPose],
    policy: Policy,
    mut evaluate: impl FnMut(u64, u64, usize, &Candidate, &mut Work) -> Result<Check, Failure>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Selection>, Failure> {
    if cancelled() {
        return Ok(None);
    }
    if groups.len() > 128
        || existing.len() > 4096
        || !(1..=64).contains(&policy.attempts)
        || !(1..=64).contains(&policy.successful_candidates)
        || policy.target_count > 128
        || !(1..=128_000_000).contains(&policy.maximum_work)
        || [policy.minimum_same, policy.minimum_other]
            .iter()
            .any(|v| !v.is_finite() || !(0. ..=10000.).contains(v))
    {
        return Err(Failure::Invalid("Invalid roadside selection policy"));
    }
    let mut work = Work {
        used: 0,
        limit: policy.maximum_work,
    };
    let mut keys = BTreeSet::new();
    let mut ids = BTreeSet::new();
    let mut total = 0;
    let mut slots = Vec::new();
    let mut ordered: Vec<_> = (0..groups.len()).collect();
    ordered.sort_by_key(|&g| groups[g].key);
    for &index in &ordered {
        let g = &groups[index];
        if !keys.insert(g.key)
            || g.key == 0
            || g.priority > 4
            || !g.shape.valid()
            || g.requests.len() > 128
            || g.candidates.len() > 1_000_000
        {
            return Err(Failure::Invalid("Invalid roadside group"));
        }
        total += g.candidates.len();
        if total > 1_000_000 {
            return Err(Failure::Invalid("Roadside candidate allowance exceeded"));
        }
        work.spend(g.candidates.len())?;
        for c in g.candidates {
            if !(BoxPose {
                shape: g.shape,
                position: c.position,
                yaw: c.yaw,
            })
            .valid()
                || c.start_index > c.end_index
                || !c.sample_distance.is_finite()
            {
                return Err(Failure::Invalid("Invalid roadside candidate pose"));
            }
        }
        let mut requests: Vec<_> = (0..g.requests.len()).collect();
        requests.sort_by_key(|&r| g.requests[r].id);
        for r in requests {
            let request = g.requests[r];
            if request.id == 0 || !ids.insert(request.id) || ids.len() > 128 {
                return Err(Failure::Invalid("Invalid roadside request identities"));
            }
            if let Some(p) = request.preference
                && (p
                    .position
                    .iter()
                    .any(|v| !v.is_finite() || v.abs() > 10000.)
                    || !p.radius.is_finite()
                    || !(0. ..=10000.).contains(&p.radius)
                    || p.max_displacement_m.is_some_and(|limit| !(1..=10000).contains(&limit))
                    || !p.weight.is_finite()
                    || !(0. ..=1_000_000.).contains(&p.weight)
                    || p.yaw.is_some_and(|y| !y.is_finite()))
            {
                return Err(Failure::Invalid("Invalid roadside preference"));
            }
            if !request.excluded {
                slots.push((index, r));
            }
        }
    }
    work.spend(existing.len())?;
    if existing.iter().any(|p| !p.valid()) {
        return Err(Failure::Invalid("Invalid existing monument bounds"));
    }
    let existing: Vec<_> = existing.iter().map(|p| p.prepare()).collect();
    let mut seed = u64::from(policy.seed) ^ 0x726f616473696465;
    let mut pools: Vec<Vec<_>> = groups
        .iter()
        .map(|g| (0..g.candidates.len()).collect())
        .collect();
    // Only placement within a trial changes. A candidate's distance from the
    // fixed monuments at its original height survives shuffles and trials.
    let mut original_distances: Vec<_> = groups
        .iter()
        .map(|g| {
            if existing.is_empty() {
                Vec::new()
            } else {
                vec![None; g.candidates.len()]
            }
        })
        .collect();
    let mut best: Vec<Selected> = Vec::new();
    let mut best_required = 0;
    let mut best_edited = 0;
    let mut best_score = 0.;
    let mut rejections = BTreeMap::<String, usize>::new();
    let has_user_moves = slots.iter().any(|&(group, request)| {
        groups[group].requests[request].preference.is_some_and(|p| p.max_displacement_m.is_some())
    });
    for _ in 0..policy.attempts {
        if cancelled() {
            return Ok(None);
        }
        shuffle(&mut slots, &mut seed, &mut work)?;
        if has_user_moves {
            work.spend(slots.len().saturating_mul(slots.len().max(1).ilog2() as usize + 1))?;
            slots.sort_by_key(|&(group, request)| {
                groups[group].requests[request].preference.is_none_or(|p| p.max_displacement_m.is_none())
            });
        }
        let mut selected: Vec<Selected> = Vec::new();
        let mut boxes = Vec::new();
        let mut trial_required = 0;
        let mut trial_edited = 0;
        let mut trial_score = 0.;
        for &(index, r) in &slots {
            if cancelled() {
                return Ok(None);
            }
            let g = &groups[index];
            let request = g.requests[r];
            shuffle(&mut pools[index], &mut seed, &mut work)?;
            let mut preferred;
            let pool = if let Some(preference) = request.preference {
                preferred = pools[index].clone();
                let factor = usize::BITS - preferred.len().max(1).leading_zeros();
                work.spend(preferred.len().saturating_mul(factor as usize))?;
                // Squared distance and yaw yield nonnegative costs. Sorting
                // their bits preserves numeric order and the shuffled tie order.
                preferred.sort_by_cached_key(|&index| {
                    preference_cost(preference, &g.candidates[index], g.shape).to_bits()
                });
                &preferred
            } else {
                &pools[index]
            };
            let mut chosen = None;
            let mut best_candidate_score = f64::NEG_INFINITY;
            let mut improvements = 0;
            for &candidate in pool {
                if cancelled() {
                    return Ok(None);
                }
                work.spend(selected.len() + 1)?;
                let c = &g.candidates[candidate];
                let rejection = if selected
                    .iter()
                    .any(|s| s.group == g.key && s.candidate == candidate)
                {
                    Some("Candidate already used by this prefab")
                } else if request.preference.is_some_and(|p| {
                    p.exact
                        && ((c.position[0] - p.position[0]).hypot(c.position[2] - p.position[1])
                            > 0.01
                            || p.yaw.is_some_and(|y| angle(c.yaw, y).abs() > 0.01))
                }) {
                    Some("Exact roadside preference requires matching road geometry")
                } else if request.preference.is_some_and(|p| {
                    p.max_displacement_m.is_some_and(|limit| {
                        (c.position[0] - p.position[0]).hypot(c.position[2] - p.position[1]) > limit as f32
                    })
                }) {
                    Some("Roadside site is beyond the monument move limit")
                } else {
                    None
                };
                if let Some(reason) = rejection {
                    *rejections.entry(reason.into()).or_default() += 1;
                    continue;
                }
                let pose = BoxPose {
                    shape: g.shape,
                    position: c.position,
                    yaw: c.yaw,
                };
                let bounds = pose.prepare();
                let same = nearest(&bounds, &boxes, &mut work)?;
                let other = match original_distances[index].get_mut(candidate) {
                    Some(distance @ None) => {
                        *distance = nearest(&bounds, &existing, &mut work)?;
                        *distance
                    }
                    Some(distance) => {
                        work.spend(existing.len())?;
                        *distance
                    }
                    // The cache is empty when there are no existing monuments.
                    None => None,
                };
                if !spaced(same, other, policy) {
                    *rejections
                        .entry("Authored roadside spacing failed".into())
                        .or_default() += 1;
                    continue;
                }
                let raw_score = score(g.priority, same, other, policy);
                if raw_score <= 0. {
                    continue;
                }
                let value = raw_score
                    - request
                        .preference
                        .map_or(0., |p| p.weight * preference_cost(p, c, g.shape));
                if value <= best_candidate_score {
                    continue;
                }
                let height = match evaluate(g.key, request.id, candidate, c, &mut work)? {
                    Check::Accepted { height } => height,
                    Check::Rejected(reason) => {
                        *rejections.entry(reason.into()).or_default() += 1;
                        continue;
                    }
                    Check::Cancelled => return Ok(None),
                };
                let fitted = BoxPose {
                    position: [c.position[0], height, c.position[2]],
                    ..pose
                };
                if !fitted.valid() {
                    return Err(Failure::Invalid(
                        "Eligibility returned an invalid roadside height",
                    ));
                }
                // Height fitting can change 3D spacing. Retest the actual pose;
                // do not certify a pre-anchor distance for a moved monument.
                let adjusted = bounds.with_height(height, g.shape.center[1]);
                let fitted_same = nearest(&adjusted, &boxes, &mut work)?;
                let fitted_other = if height.to_bits() == c.position[1].to_bits() {
                    work.spend(existing.len())?;
                    other
                } else {
                    nearest(&adjusted, &existing, &mut work)?
                };
                if !spaced(fitted_same, fitted_other, policy) {
                    *rejections
                        .entry("Fitted roadside spacing failed".into())
                        .or_default() += 1;
                    continue;
                }
                chosen = Some((
                    Selected {
                        group: g.key,
                        request: request.id,
                        candidate,
                        road: c.road,
                        start_index: c.start_index,
                        end_index: c.end_index,
                        position: fitted.position,
                        yaw: c.yaw,
                        score: value,
                    },
                    adjusted,
                ));
                best_candidate_score = value;
                improvements += 1;
                if improvements >= policy.successful_candidates
                    || (policy.other_mode == DistanceMode::Any && request.preference.is_none())
                {
                    break;
                }
            }
            if let Some((selection, bounds)) = chosen {
                trial_score += selection.score;
                trial_required += usize::from(request.required);
                trial_edited += usize::from(request.preference.is_some_and(|p| p.max_displacement_m.is_some()));
                selected.push(selection);
                boxes.push(bounds);
            }
            if policy.target_count > 0 && selected.len() >= policy.target_count {
                break;
            }
        }
        let counts = (trial_required, trial_edited, selected.len());
        let previous = (best_required, best_edited, best.len());
        if counts > previous || (counts == previous && trial_score > best_score)
        {
            best = selected;
            best_required = trial_required;
            best_edited = trial_edited;
            best_score = trial_score;
        }
    }
    if cancelled() {
        return Ok(None);
    }
    let selected_ids: BTreeSet<_> = best.iter().map(|s| s.request).collect();
    let mut missing = Vec::new();
    let mut required_missing = Vec::new();
    for g in groups {
        for r in g.requests {
            if !r.excluded && !selected_ids.contains(&r.id) {
                missing.push(r.id);
                if r.required {
                    required_missing.push(r.id);
                }
            }
        }
    }
    missing.sort_unstable();
    required_missing.sort_unstable();
    Ok(Some(Selection {
        engine: VERSION,
        selected: best,
        missing,
        required_missing,
        attempts: policy.attempts,
        work: work.used(),
        rejections,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy() -> Policy {
        Policy {
            seed: 1,
            attempts: 8,
            successful_candidates: 8,
            target_count: 0,
            minimum_same: 500.,
            minimum_other: 50.,
            same_mode: DistanceMode::Max,
            other_mode: DistanceMode::Any,
            maximum_work: 100_000,
        }
    }
    fn shape() -> BoxShape {
        BoxShape {
            center: [0.; 3],
            extent: [10.; 3],
        }
    }
    fn candidates() -> Vec<Candidate> {
        [-1500., -750., 0., 750., 1500.]
            .into_iter()
            .enumerate()
            .map(|(i, x)| Candidate {
                road: 0,
                start_index: i,
                end_index: i + 1,
                sample_distance: i as f32 * 5.,
                direction: 1,
                position: [x, 100., 0.],
                yaw: 0.,
            })
            .collect()
    }
    fn requests(n: usize) -> Vec<Request> {
        (1..=n)
            .map(|id| Request {
                id: id as u64,
                required: true,
                excluded: false,
                preference: None,
            })
            .collect()
    }
    fn eligible(_: u64, _: u64, _: usize, c: &Candidate, w: &mut Work) -> Result<Check, Failure> {
        w.spend(1)?;
        Ok(Check::Accepted {
            height: c.position[1],
        })
    }
    #[test]
    fn counts_source_family_spacing_and_seed_replay_are_preserved() {
        let c = candidates();
        let r = requests(3);
        let g = Group {
            key: 10,
            priority: 2,
            shape: shape(),
            requests: &r,
            candidates: &c,
        };
        let existing = [BoxPose {
            shape: shape(),
            position: [0., 100., 0.],
            yaw: 0.,
        }];
        let run = || {
            solve(&[Group { ..g }], &existing, policy(), eligible, || false)
                .unwrap()
                .unwrap()
        };
        let a = run();
        let b = run();
        assert_eq!(a.selected, b.selected);
        assert_eq!(a.work, b.work);
        assert_eq!(a.selected.len(), 3);
        assert!(a.required_missing.is_empty());
        for (i, s) in a.selected.iter().enumerate() {
            let pose = BoxPose {
                shape: shape(),
                position: s.position,
                yaw: s.yaw,
            };
            assert!(pose.squared_distance(existing[0]).unwrap() >= 50_f64.powi(2));
            for t in &a.selected[..i] {
                assert!(
                    pose.squared_distance(BoxPose {
                        shape: shape(),
                        position: t.position,
                        yaw: t.yaw
                    })
                    .unwrap()
                        >= 500_f64.powi(2)
                );
            }
        }
    }
    #[test]
    fn spacing_is_between_all_same_stage_prefabs_not_just_identical_prefab_names() {
        let a = vec![candidates()[2].clone()];
        let mut b = a.clone();
        b[0].position[0] = 400.;
        let r = requests(2);
        let groups = [
            Group {
                key: 10,
                priority: 2,
                shape: shape(),
                requests: &r[..1],
                candidates: &a,
            },
            Group {
                key: 20,
                priority: 2,
                shape: shape(),
                requests: &r[1..],
                candidates: &b,
            },
        ];
        let s = solve(&groups, &[], policy(), eligible, || false)
            .unwrap()
            .unwrap();
        assert_eq!(s.selected.len(), 1);
        assert_eq!(s.required_missing.len(), 1);
    }
    #[test]
    fn soft_preferences_remain_best_effort_and_exact_requests_require_road_geometry() {
        let c = candidates();
        let mut r = requests(1);
        r[0].preference = Some(Preference {
            position: [700., 0.],
            radius: 0.,
            max_displacement_m: None,
            weight: 1_000_000.,
            exact: false,
            yaw: Some(0.),
        });
        let run = |r: &[Request]| {
            solve(
                &[Group {
                    key: 10,
                    priority: 2,
                    shape: shape(),
                    requests: r,
                    candidates: &c,
                }],
                &[],
                policy(),
                eligible,
                || false,
            )
            .unwrap()
            .unwrap()
        };
        let s = run(&r);
        assert_eq!(s.selected.len(), 1);
        assert_eq!(s.selected[0].position[0], 750.);
        assert!(s.selected[0].score < 0.);
        r[0].preference.as_mut().unwrap().max_displacement_m = Some(49);
        let capped = run(&r);
        assert_eq!(capped.missing, vec![1]);
        assert!(capped.rejections.contains_key("Roadside site is beyond the monument move limit"));
        r[0].preference.as_mut().unwrap().max_displacement_m = Some(50);
        assert_eq!(run(&r).selected[0].position[0], 750.);
        r[0].preference.as_mut().unwrap().max_displacement_m = None;
        r[0].preference.as_mut().unwrap().exact = true;
        assert_eq!(run(&r).missing, vec![1]);
        r[0].preference.as_mut().unwrap().position[0] = 750.;
        assert_eq!(run(&r).selected[0].position[0], 750.);
    }
    #[test]
    fn fitted_height_must_still_meet_spacing_and_invalid_callback_data_is_fatal() {
        let mut c = vec![candidates()[2].clone()];
        c[0].position[1] = 1000.;
        let r = requests(1);
        let groups = [Group {
            key: 10,
            priority: 2,
            shape: shape(),
            requests: &r,
            candidates: &c,
        }];
        let existing = [BoxPose {
            shape: shape(),
            position: [0., 100., 0.],
            yaw: 0.,
        }];
        let s = solve(
            &groups,
            &existing,
            policy(),
            |_, _, _, _, _| Ok(Check::Accepted { height: 100. }),
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(s.selected.is_empty());
        assert!(s.rejections.contains_key("Fitted roadside spacing failed"));
        assert!(matches!(
            solve(
                &groups,
                &existing,
                policy(),
                |_, _, _, _, _| Ok(Check::Accepted { height: f32::NAN }),
                || false
            ),
            Err(Failure::Invalid(_))
        ));
    }
    #[test]
    fn target_caps_exclusions_group_order_and_work_failures_are_explicit() {
        let c = candidates();
        let mut r = requests(3);
        r[2].excluded = true;
        let groups = [
            Group {
                key: 20,
                priority: 2,
                shape: shape(),
                requests: &r[..1],
                candidates: &c,
            },
            Group {
                key: 10,
                priority: 2,
                shape: shape(),
                requests: &r[1..],
                candidates: &c,
            },
        ];
        let a = solve(&groups, &[], policy(), eligible, || false)
            .unwrap()
            .unwrap();
        let reversed = [Group { ..groups[1] }, Group { ..groups[0] }];
        let b = solve(&reversed, &[], policy(), eligible, || false)
            .unwrap()
            .unwrap();
        assert_eq!(a.selected, b.selected);
        let capped = solve(
            &groups,
            &[],
            Policy {
                target_count: 1,
                ..policy()
            },
            eligible,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(capped.selected.len(), 1);
        assert_eq!(capped.required_missing.len(), 1);
        assert!(!capped.missing.contains(&3));
        assert!(matches!(
            solve(
                &groups,
                &[],
                Policy {
                    maximum_work: 1,
                    ..policy()
                },
                eligible,
                || false
            ),
            Err(Failure::Budget)
        ));
        let calls = std::cell::Cell::new(0);
        assert!(
            solve(&groups, &[], policy(), eligible, || {
                calls.set(calls.get() + 1);
                calls.get() > 10
            })
            .unwrap()
            .is_none()
        );
        assert!(
            solve(
                &groups,
                &[],
                policy(),
                |_, _, _, _, _| Ok(Check::Cancelled),
                || false
            )
            .unwrap()
            .is_none()
        );
        let mut work = Work {
            used: usize::MAX,
            limit: usize::MAX,
        };
        assert_eq!(work.spend(1), Err(Failure::Budget));
    }
}
