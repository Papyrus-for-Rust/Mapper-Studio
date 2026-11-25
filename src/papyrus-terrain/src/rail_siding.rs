//! Source-configured parallel sidings. Independent seeded selection; no runtime
//! spline assembly or placement-map reservation is claimed by these proposals.
use crate::path_geometry::{self, Path};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
pub const VERSION: &str = "papyrus-rail-siding-candidates-1";
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub engine: String,
    pub span_min: usize,
    pub span_max_exclusive: usize,
    pub skip_min: usize,
    pub skip_max_exclusive: usize,
    pub endpoint_margin: usize,
    pub join_padding: usize,
    pub tangent_probe: usize,
    pub fade_points: usize,
    pub nearby_track_distance: f32,
    pub maximum_parent_angle: f32,
    pub maximum_join_angle: f32,
    pub lateral_offset: f32,
    pub resample_distance: f32,
    pub hierarchy: u32,
    pub start_flag: bool,
    pub end_flag: bool,
}
impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        if self.engine != VERSION
            || !(2..=32).contains(&self.join_padding)
            || self.endpoint_margin < self.join_padding
            || self.endpoint_margin > 64
            || !(1..=64).contains(&self.tangent_probe)
            || self.span_min <= self.tangent_probe
            || self.span_max_exclusive <= self.span_min
            || self.span_max_exclusive > 256
            || self.skip_min == 0
            || self.skip_max_exclusive <= self.skip_min
            || self.skip_max_exclusive > 512
            || !(1..=32).contains(&self.fade_points)
            || !self.nearby_track_distance.is_finite()
            || !(0.1..=128.).contains(&self.nearby_track_distance)
            || [self.maximum_parent_angle, self.maximum_join_angle]
                .iter()
                .any(|v| !v.is_finite() || !(0.1..=90.).contains(v))
            || !self.lateral_offset.is_finite()
            || !(0.1..=32.).contains(&self.lateral_offset)
            || !self.resample_distance.is_finite()
            || !(0.1..=100.).contains(&self.resample_distance)
            || self.hierarchy != 2
            || self.start_flag
            || self.end_flag
        {
            return Err("Unsupported rail siding source policy".into());
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub seed: u32,
    pub maximum_work: usize,
    pub maximum_sidings: usize,
}
impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=128_000_000).contains(&self.maximum_work)
            || !(1..=256).contains(&self.maximum_sidings)
        {
            return Err("Invalid rail siding work limits".into());
        }
        Ok(())
    }
}
pub struct Track<'a> {
    pub id: u64,
    pub path: &'a Path,
}
#[derive(Debug, Serialize)]
pub struct Siding {
    pub parent: u64,
    pub sample_start: usize,
    pub sample_end: usize,
    pub start_control: usize,
    pub end_control: usize,
    pub side: i32,
    pub path: Path,
}
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Proposed {
        sidings: Vec<Siding>,
        work: usize,
        rejections: BTreeMap<&'static str, usize>,
    },
    BudgetExceeded {
        work: usize,
    },
    Cancelled,
}
fn delta(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}
fn len(a: [f32; 3]) -> f32 {
    a.iter().map(|v| v * v).sum::<f32>().sqrt()
}
fn unit(a: [f32; 3]) -> [f32; 3] {
    let n = len(a);
    if n > 0.00001 {
        a.map(|v| v / n)
    } else {
        [0.; 3]
    }
}
fn angle(a: [f32; 3], b: [f32; 3]) -> f32 {
    let denominator = len(a) * len(b);
    if denominator < 1e-15 {
        return 0.;
    }
    let cosine = a.iter().zip(b).map(|(a, b)| a * b).sum::<f32>() / denominator;
    cosine.clamp(-1., 1.).acos().to_degrees()
}
fn signed(a: [f32; 3], b: [f32; 3]) -> f32 {
    angle(a, b)
        * if a[2] * b[0] - a[0] * b[2] >= 0. {
            1.
        } else {
            -1.
        }
}
fn smooth(t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    t * t * (3. - 2. * t)
}
fn random(state: &mut u32, min: usize, max: usize) -> usize {
    *state = state.wrapping_add(0x9e3779b9);
    let mut v = *state;
    v = (v ^ (v >> 16)).wrapping_mul(0x85ebca6b);
    v = (v ^ (v >> 13)).wrapping_mul(0xc2b2ae35);
    min + ((v ^ (v >> 16)) as usize % (max - min))
}
/// Source branch order is significant when the two fade regions overlap.
fn offset(
    point: [f32; 3],
    tangent: [f32; 3],
    index: usize,
    start: usize,
    end: usize,
    side: i32,
    p: &Policy,
) -> [f32; 3] {
    let factor = if index < start + p.fade_points {
        smooth((index - (start - p.join_padding)) as f32 / p.join_padding as f32)
    } else if index > end.saturating_sub(p.fade_points) {
        smooth((end + p.join_padding - index) as f32 / p.join_padding as f32)
    } else {
        1.
    };
    // A rotation about Y retains the tangent's Y component on either side.
    let lateral = [
        tangent[2] * side as f32,
        tangent[1],
        -tangent[0] * side as f32,
    ];
    std::array::from_fn(|i| point[i] + lateral[i] * (factor * p.lateral_offset))
}
pub fn generate(
    size: f32,
    tracks: &[Track<'_>],
    p: &Policy,
    s: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Outcome, String> {
    p.validate()?;
    s.validate()?;
    if cancelled() {
        return Ok(Outcome::Cancelled);
    }
    let mut ids = BTreeSet::new();
    if !size.is_finite()
        || !(1. ..=10000.).contains(&size)
        || tracks.is_empty()
        || tracks.len() > 257
        || tracks.iter().map(|t| t.path.points().len()).sum::<usize>() > 65536
        || tracks.iter().any(|t| {
            t.id == 0
                || !ids.insert(t.id)
                || t.path
                    .points()
                    .iter()
                    .any(|v| v[0].abs() > size / 2. || v[2].abs() > size / 2.)
        })
    {
        return Err("Invalid rail siding parent roster".into());
    }
    let mut work = 0;
    macro_rules! visit {
        () => {
            if cancelled() {
                return Ok(Outcome::Cancelled);
            }
            if work >= s.maximum_work {
                return Ok(Outcome::BudgetExceeded { work });
            }
            work += 1;
        };
    }
    let mut nearby = Vec::new();
    for track in tracks {
        let mut flags = vec![false; track.path.points().len()];
        for (i, &point) in track.path.points().iter().enumerate() {
            'other: for other in tracks.iter().filter(|t| t.id != track.id) {
                for &v in other.path.points() {
                    visit!();
                    if len(delta(point, v)) < p.nearby_track_distance {
                        flags[i] = true;
                        break 'other;
                    }
                }
            }
        }
        nearby.push(flags);
    }
    let mut state = s.seed;
    let mut sidings = Vec::new();
    let mut rejections = BTreeMap::new();
    macro_rules! reject {
        ($reason:expr) => {
            *rejections.entry($reason).or_default() += 1;
        };
    }
    for (track, nearby) in tracks.iter().zip(&nearby) {
        let points = track.path.points();
        let tangents = track.path.tangents();
        let Some(last) = points.len().checked_sub(p.endpoint_margin + 1) else {
            continue;
        };
        let mut i = p.endpoint_margin;
        while i <= last {
            visit!();
            let start = i;
            i += 1;
            let span = random(&mut state, p.span_min, p.span_max_exclusive);
            let skip = random(&mut state, p.skip_min, p.skip_max_exclusive);
            let end = start + span;
            if end >= last {
                reject!("short_parent");
                continue;
            }
            if angle(tangents[start], tangents[end]) > p.maximum_parent_angle {
                reject!("parent_angle");
                continue;
            }
            let a = unit(delta(points[start + p.tangent_probe], points[start]));
            let b = unit(delta(points[end], points[end - p.tangent_probe]));
            let aa = signed(a, tangents[start]);
            let bb = -signed(b, tangents[end]);
            if (aa >= 0.) != (bb >= 0.)
                || aa.abs() > p.maximum_join_angle
                || bb.abs() > p.maximum_join_angle
            {
                reject!("join_angle");
                continue;
            }
            let first = start - p.join_padding;
            let final_control = end + p.join_padding;
            let side = if aa > 0. { 1 } else { -1 };
            let mut controls = Vec::with_capacity(final_control - first + 1);
            let mut conflict = false;
            for index in first..=final_control {
                visit!();
                if nearby[index] {
                    conflict = true;
                    break;
                }
                controls.push(offset(
                    points[index],
                    tangents[index],
                    index,
                    start,
                    end,
                    side,
                    p,
                ));
            }
            if conflict {
                reject!("near_other_track");
                i += skip;
                continue;
            }
            if controls
                .iter()
                .any(|v| v[0].abs() > size / 2. || v[2].abs() > size / 2.)
            {
                reject!("outside_world");
                i += skip;
                continue;
            }
            let budget = s.maximum_work.saturating_sub(work).min(32000000);
            if budget == 0 {
                return Ok(Outcome::BudgetExceeded { work });
            }
            let path = match path_geometry::build(
                &controls,
                path_geometry::Settings {
                    horizontal_iterations: 0,
                    vertical_iterations: 0,
                    resample_distance: p.resample_distance,
                    maximum_work: budget,
                },
                None,
                &cancelled,
            ) {
                Ok(Some(v)) => v,
                Ok(None) => return Ok(Outcome::Cancelled),
                Err(path_geometry::Failure::Budget) => {
                    return Ok(Outcome::BudgetExceeded {
                        work: s.maximum_work,
                    });
                }
                Err(e) => return Err(format!("Siding resample failed: {e:?}")),
            };
            work += path.work();
            let mut fitted = path.points().to_vec();
            fitted[0] = points[first];
            let last = fitted.len() - 1;
            fitted[last] = points[final_control];
            let budget = s.maximum_work.saturating_sub(work).min(32000000);
            if budget == 0 {
                return Ok(Outcome::BudgetExceeded { work });
            }
            let path = match Path::from_controls(&fitted, budget, &cancelled) {
                Ok(Some(v)) => v,
                Ok(None) => return Ok(Outcome::Cancelled),
                Err(path_geometry::Failure::Budget) => {
                    return Ok(Outcome::BudgetExceeded {
                        work: s.maximum_work,
                    });
                }
                Err(e) => return Err(format!("Siding joins failed: {e:?}")),
            };
            work += path.work();
            if sidings.len() >= s.maximum_sidings {
                return Ok(Outcome::BudgetExceeded { work });
            }
            sidings.push(Siding {
                parent: track.id,
                sample_start: start,
                sample_end: end,
                start_control: first,
                end_control: final_control,
                side,
                path,
            });
            i += span + skip;
        }
    }
    if cancelled() {
        Ok(Outcome::Cancelled)
    } else {
        Ok(Outcome::Proposed {
            sidings,
            work,
            rejections,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn policy() -> Policy {
        Policy {
            engine: VERSION.into(),
            span_min: 13,
            span_max_exclusive: 20,
            skip_min: 40,
            skip_max_exclusive: 53,
            endpoint_margin: 17,
            join_padding: 8,
            tangent_probe: 8,
            fade_points: 8,
            nearby_track_distance: 16.,
            maximum_parent_angle: 30.,
            maximum_join_angle: 60.,
            lateral_offset: 5.,
            resample_distance: 7.5,
            hierarchy: 2,
            start_flag: false,
            end_flag: false,
        }
    }
    fn settings() -> Settings {
        Settings {
            seed: 17,
            maximum_work: 8000000,
            maximum_sidings: 64,
        }
    }
    fn path() -> Path {
        Path::from_controls(
            &(0..201)
                .map(|i| [0., 100., i as f32 * 4. - 400.])
                .collect::<Vec<_>>(),
            100000,
            || false,
        )
        .unwrap()
        .unwrap()
    }
    #[test]
    fn deterministic_sidings_rejoin_parents_and_preserve_source_overlap_priority() {
        let path = path();
        let before = path.points().to_vec();
        let tracks = [Track { id: 1, path: &path }];
        let a = generate(1000., &tracks, &policy(), settings(), || false).unwrap();
        let b = generate(1000., &tracks, &policy(), settings(), || false).unwrap();
        assert_eq!(
            serde_json::to_vec(&a).unwrap(),
            serde_json::to_vec(&b).unwrap()
        );
        let Outcome::Proposed { sidings, .. } = a else {
            panic!("Expected sidings");
        };
        assert!(sidings.len() > 1);
        for siding in sidings {
            assert_eq!(siding.path.points()[0], path.points()[siding.start_control]);
            assert_eq!(
                *siding.path.points().last().unwrap(),
                path.points()[siding.end_control]
            );
            assert_eq!(siding.side, -1);
        }
        let p = policy();
        // With span 13 the fade regions overlap; the first branch takes priority.
        assert_eq!(
            offset([0.; 3], [0., 0., 1.], 27, 20, 33, 1, &p),
            [5., 0., 0.]
        );
        assert_eq!(
            offset([0.; 3], [0., 0.6, 0.8], 20, 20, 33, -1, &p),
            [-4., 3., 0.]
        );
        assert_eq!(path.points(), before);
    }
    #[test]
    fn nearby_tracks_budget_and_cancel_never_return_partial_sidings() {
        let path = path();
        let tracks = [Track { id: 1, path: &path }, Track { id: 2, path: &path }];
        assert!(
            matches!(generate(1000.,&tracks,&policy(),settings(),||false).unwrap(),Outcome::Proposed{sidings,..}if sidings.is_empty())
        );
        let mut s = settings();
        s.maximum_sidings = 1;
        assert!(matches!(
            generate(1000., &tracks[..1], &policy(), s, || false).unwrap(),
            Outcome::BudgetExceeded { .. }
        ));
        let mut s = settings();
        s.maximum_work = 1;
        assert!(matches!(
            generate(1000., &tracks, &policy(), s, || false).unwrap(),
            Outcome::BudgetExceeded { .. }
        ));
        let calls = std::cell::Cell::new(0);
        assert!(matches!(
            generate(1000., &tracks, &policy(), settings(), || {
                calls.set(calls.get() + 1);
                calls.get() == 40
            })
            .unwrap(),
            Outcome::Cancelled
        ));
    }
}
