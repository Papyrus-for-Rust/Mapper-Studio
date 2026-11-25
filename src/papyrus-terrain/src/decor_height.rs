//! Ordered, transactional source TerrainHeightSet operations.
use crate::{
    decor_fit,
    erosion::{decode, encode},
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
pub const VERSION: &str = "papyrus-decor-height-9";
#[derive(Clone, Copy, Debug)]
pub enum Mode {
    Set,
    Raise,
    Lower,
    Add { delta: f32 },
}
#[derive(Clone, Debug)]
pub struct Modifier {
    pub position: [f32; 3],
    pub radius: f32,
    pub fade: f32,
    pub opacity: f32,
    pub mode: Mode,
}
#[derive(Debug, Serialize, Default)]
pub struct Report {
    pub instances: usize,
    pub operations: usize,
    pub changed_samples: usize,
    pub raised_samples: usize,
    pub lowered_samples: usize,
    pub maximum_raise: f32,
    pub maximum_lower: f32,
}
pub(crate) struct Plan {
    pub writes: Vec<(usize, i16)>,
    pub report: Report,
}
#[derive(Debug)]
pub(crate) enum Failure {
    Outside,
    Budget,
    Unsupported,
}
pub(crate) struct Input<'a> {
    pub heights: &'a [i16],
    pub resolution: usize,
    pub size: f32,
    pub initial: &'a [(usize, i16)],
    pub modifiers: &'a [Modifier],
    pub pose: &'a decor_fit::Pose,
}
pub fn validate(modifiers: &[Modifier]) -> Result<(), String> {
    if modifiers.len() > 4096
        || modifiers.iter().any(|m| {
            !m.position
                .iter()
                .all(|v| v.is_finite() && v.abs() <= 10000.)
                || !m.radius.is_finite()
                || !(0. ..=256.).contains(&m.radius)
                || !m.fade.is_finite()
                || !(0. ..=256.).contains(&m.fade)
                || !m.opacity.is_finite()
                || !(0. ..=1.).contains(&m.opacity)
                || matches!(m.mode,Mode::Add {delta} if !delta.is_finite() || delta.abs()>1000.)
        })
    {
        return Err("Invalid source height modifier".into());
    }
    Ok(())
}
pub(crate) fn prepare(
    input: Input<'_>,
    visits: &mut usize,
    limit: usize,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Plan>, Failure> {
    prepare_ordered(input, visits, limit, cancelled, false)
}
/// Road-object modifiers run without TerrainMap.Push: source and destination
/// alias, so a later Set/Raise/Lower observes an earlier Add in the same prefab.
pub(crate) fn prepare_ordered(
    input: Input<'_>,
    visits: &mut usize,
    limit: usize,
    cancelled: &impl Fn() -> bool,
    source_is_destination: bool,
) -> Result<Option<Plan>, Failure> {
    let Input {
        heights,
        resolution: n,
        size,
        initial,
        modifiers,
        pose,
    } = input;
    if n < 2
        || heights.len() != n * n
        || initial.len() > 262144
        || initial.windows(2).any(|p| p[0].0 >= p[1].0)
        || initial
            .iter()
            .any(|(i, h)| *i >= heights.len() || !(0..=32766).contains(h))
    {
        return Err(Failure::Unsupported);
    }
    let mut writes: BTreeMap<_, _> = initial.iter().copied().collect();
    let mut touched = BTreeSet::new();
    let index = |w: f32| ((w / size + 0.5) * n as f32).clamp(0., (n - 1) as f32) as usize;
    let mut visit = || {
        if *visits >= limit {
            Err(Failure::Budget)
        } else {
            *visits += 1;
            Ok(())
        }
    };
    for m in modifiers {
        if cancelled() {
            return Ok(None);
        }
        let local = decor_fit::rotate(
            pose.rotation,
            std::array::from_fn(|i| m.position[i] * pose.scale[i]),
        );
        let p: [f32; 3] = std::array::from_fn(|i| pose.position[i] + local[i]);
        let radius = m.radius * pose.scale[1];
        let fade = m.fade * pose.scale[1];
        if !p.iter().all(|v| v.is_finite())
            || !radius.is_finite()
            || radius > 512.
            || !fade.is_finite()
            || fade > 512.
            || !(-500. ..=500.).contains(&p[1])
        {
            return Err(Failure::Unsupported);
        }
        if p[0].abs() + radius > size * 0.5 || p[2].abs() + radius > size * 0.5 {
            return Err(Failure::Outside);
        }
        let cx = (p[0] / size + 0.5) * n as f32;
        let cz = (p[2] / size + 0.5) * n as f32;
        let r = radius / size * n as f32;
        let f = fade / size * n as f32;
        for z in index(p[2] - radius)..=index(p[2] + radius) {
            if cancelled() {
                return Ok(None);
            }
            for x in index(p[0] - radius)..=index(p[0] + radius) {
                visit()?;
                let d = (x as f32 + 0.5 - cx).hypot(z as f32 + 0.5 - cz);
                let weight = if f == 0. {
                    if d < r { 1. } else { 0. }
                } else {
                    ((r - d) / f).clamp(0., 1.)
                };
                let t = weight * m.opacity;
                if t <= 0. {
                    continue;
                }
                let i = z * n + x;
                let original = heights[i];
                let old = *writes.get(&i).unwrap_or(&original);
                // Source smoothing reads the pre-transaction source grid. Raise/Lower
                // compare that target with the current destination, preserving order.
                let source = if source_is_destination { old } else { original };
                let target =
                    encode(decode(source) + (p[1] - decode(source)) * (t * t * (3. - 2. * t)));
                let next = match m.mode {
                    Mode::Set => target,
                    Mode::Raise => old.max(target),
                    Mode::Lower => old.min(target),
                    Mode::Add { delta } => encode((decode(old) + delta * t).clamp(-500., 500.)),
                };
                if next != old {
                    writes.insert(i, next);
                    touched.insert(i);
                }
                if writes.len() > 262144 || touched.len() > 262144 {
                    return Err(Failure::Unsupported);
                }
            }
        }
    }
    let mut report = Report {
        instances: usize::from(!modifiers.is_empty()),
        operations: modifiers.len(),
        ..Default::default()
    };
    for i in touched {
        let before = initial
            .binary_search_by_key(&i, |p| p.0)
            .map_or(heights[i], |j| initial[j].1);
        let after = *writes.get(&i).unwrap_or(&heights[i]);
        if after == before {
            continue;
        }
        report.changed_samples += 1;
        let delta = decode(after) - decode(before);
        if delta > 0. {
            report.raised_samples += 1;
            report.maximum_raise = report.maximum_raise.max(delta);
        } else {
            report.lowered_samples += 1;
            report.maximum_lower = report.maximum_lower.max(-delta);
        }
    }
    writes.retain(|i, v| *v != heights[*i]);
    Ok(Some(Plan {
        writes: writes.into_iter().collect(),
        report,
    }))
}
impl Report {
    pub(crate) fn commit(&mut self, other: Report) {
        self.instances += other.instances;
        self.operations += other.operations;
        self.changed_samples += other.changed_samples;
        self.raised_samples += other.raised_samples;
        self.lowered_samples += other.lowered_samples;
        self.maximum_raise = self.maximum_raise.max(other.maximum_raise);
        self.maximum_lower = self.maximum_lower.max(other.maximum_lower);
    }
}
/// Cell-order execution for a live destination with no initial polygon writes.
/// Complete every modifier affecting a cell before checking its final change:
/// intermediate writes can cancel, and must not cause a false rejection.
#[derive(Debug)]
pub(crate) enum CheckedFailure<E> {
    Height(Failure),
    Cell(E),
}
impl<E> From<Failure> for CheckedFailure<E> {
    fn from(value: Failure) -> Self {
        Self::Height(value)
    }
}
pub(crate) fn prepare_checked<E>(
    input: Input<'_>,
    visits: &mut usize,
    limit: usize,
    cancelled: &impl Fn() -> bool,
    mut check: impl FnMut(usize) -> Result<(), E>,
) -> Result<Option<Plan>, CheckedFailure<E>> {
    let Input {
        heights,
        resolution: n,
        size,
        initial,
        modifiers,
        pose,
    } = input;
    if cancelled() {
        return Ok(None);
    }
    if !(2..=8193).contains(&n)
        || heights.len() != n * n
        || !initial.is_empty()
        || !size.is_finite()
        || size <= 0.
        || validate(modifiers).is_err()
    {
        return Err(Failure::Unsupported.into());
    }
    struct Circle<'a> {
        modifier: &'a Modifier,
        height: f32,
        center: [f32; 2],
        radius: f32,
        fade: f32,
        bounds: [usize; 4],
    }
    let index = |w: f32| ((w / size + 0.5) * n as f32).clamp(0., (n - 1) as f32) as usize;
    let mut circles = Vec::with_capacity(modifiers.len());
    for m in modifiers {
        if cancelled() {
            return Ok(None);
        }
        let local = decor_fit::rotate(
            pose.rotation,
            std::array::from_fn(|i| m.position[i] * pose.scale[i]),
        );
        let p: [f32; 3] = std::array::from_fn(|i| pose.position[i] + local[i]);
        let radius = m.radius * pose.scale[1];
        let fade = m.fade * pose.scale[1];
        if !p.iter().all(|v| v.is_finite())
            || !radius.is_finite()
            || radius > 512.
            || !fade.is_finite()
            || fade > 512.
            || !(-500. ..=500.).contains(&p[1])
        {
            return Err(Failure::Unsupported.into());
        }
        if p[0].abs() + radius > size * 0.5 || p[2].abs() + radius > size * 0.5 {
            return Err(Failure::Outside.into());
        }
        circles.push(Circle {
            modifier: m,
            height: p[1],
            center: [
                (p[0] / size + 0.5) * n as f32,
                (p[2] / size + 0.5) * n as f32,
            ],
            radius: radius / size * n as f32,
            fade: fade / size * n as f32,
            bounds: [
                index(p[0] - radius),
                index(p[0] + radius),
                index(p[2] - radius),
                index(p[2] + radius),
            ],
        });
    }
    let mut report = Report {
        instances: usize::from(!modifiers.is_empty()),
        operations: modifiers.len(),
        ..Default::default()
    };
    let mut writes = Vec::new();
    let Some(z0) = circles.iter().map(|c| c.bounds[2]).min() else {
        return Ok(Some(Plan { writes, report }));
    };
    let z1 = circles.iter().map(|c| c.bounds[3]).max().unwrap();
    let mut events = Vec::with_capacity(circles.len() * 2);
    let mut active = BTreeSet::new();
    for z in z0..=z1 {
        if cancelled() {
            return Ok(None);
        }
        // Charge the row's interval indexing as well as the actual circle tests.
        if circles.len() > limit.saturating_sub(*visits) {
            return Err(Failure::Budget.into());
        }
        *visits += circles.len();
        events.clear();
        active.clear();
        for (j, c) in circles.iter().enumerate() {
            if (c.bounds[2]..=c.bounds[3]).contains(&z) {
                events.push((c.bounds[0], j, true));
                events.push((c.bounds[1] + 1, j, false));
            }
        }
        events.sort_unstable();
        let mut e = 0;
        while e < events.len() {
            let start = events[e].0;
            while e < events.len() && events[e].0 == start {
                let (_, j, enter) = events[e];
                if enter {
                    active.insert(j);
                } else {
                    active.remove(&j);
                }
                e += 1;
            }
            let Some(&(end, _, _)) = events.get(e) else {
                break;
            };
            if active.is_empty() {
                continue;
            }
            for x in start..end {
                if x % 64 == 0 && cancelled() {
                    return Ok(None);
                }
                let i = z * n + x;
                let original = heights[i];
                let mut old = original;
                for &j in &active {
                    if *visits >= limit {
                        return Err(Failure::Budget.into());
                    }
                    *visits += 1;
                    let c = &circles[j];
                    let m = c.modifier;
                    let d = (x as f32 + 0.5 - c.center[0]).hypot(z as f32 + 0.5 - c.center[1]);
                    let weight = if c.fade == 0. {
                        if d < c.radius { 1. } else { 0. }
                    } else {
                        ((c.radius - d) / c.fade).clamp(0., 1.)
                    };
                    let t = weight * m.opacity;
                    if t <= 0. {
                        continue;
                    }
                    let target =
                        encode(decode(old) + (c.height - decode(old)) * (t * t * (3. - 2. * t)));
                    old = match m.mode {
                        Mode::Set => target,
                        Mode::Raise => old.max(target),
                        Mode::Lower => old.min(target),
                        Mode::Add { delta } => encode((decode(old) + delta * t).clamp(-500., 500.)),
                    };
                }
                if old == original {
                    continue;
                }
                check(i).map_err(CheckedFailure::Cell)?;
                if writes.len() == 262144 {
                    return Err(Failure::Unsupported.into());
                }
                writes.push((i, old));
                report.changed_samples += 1;
                let delta = decode(old) - decode(original);
                if delta > 0. {
                    report.raised_samples += 1;
                    report.maximum_raise = report.maximum_raise.max(delta);
                } else {
                    report.lowered_samples += 1;
                    report.maximum_lower = report.maximum_lower.max(-delta);
                }
            }
        }
    }
    Ok(Some(Plan { writes, report }))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checked_cell_order_matches_modifier_order_for_overlaps_transforms_and_all_modes() {
        let base: Vec<_> = (0..64 * 64)
            .map(|i| encode(15. + (i % 17) as f32 * 0.3))
            .collect();
        for iteration in 0..64 {
            let angle = iteration as f32 * 0.21;
            let p = decor_fit::Pose {
                candidate: 0,
                position: [2., 3., -1.],
                rotation: [0., (angle * 0.5).sin(), 0., (angle * 0.5).cos()],
                scale: [1.2, 0.8, 1.3],
            };
            let mods: Vec<_> = (0..12)
                .map(|j| Modifier {
                    position: [
                        (j % 4) as f32 * 4. - 6.,
                        ((iteration + j) % 9) as f32 + 12.,
                        (j / 4) as f32 * 5. - 5.,
                    ],
                    radius: 3. + (j % 5) as f32,
                    fade: if j % 3 == 0 { 0. } else { 2. },
                    opacity: if j % 4 == 0 { 0. } else { 0.7 },
                    mode: match (j + iteration) % 4 {
                        0 => Mode::Set,
                        1 => Mode::Raise,
                        2 => Mode::Lower,
                        _ => Mode::Add {
                            delta: if j % 2 == 0 { 3. } else { -2. },
                        },
                    },
                })
                .collect();
            let input = || Input {
                heights: &base,
                resolution: 64,
                size: 64.,
                initial: &[],
                modifiers: &mods,
                pose: &p,
            };
            let expected = prepare_ordered(input(), &mut 0, 1_000_000, &|| false, true)
                .unwrap()
                .unwrap();
            let mut checked = Vec::new();
            let got = prepare_checked(input(), &mut 0, 1_000_000, &|| false, |i| {
                checked.push(i);
                Ok::<_, ()>(())
            })
            .unwrap()
            .unwrap();
            assert_eq!(got.writes, expected.writes, "case {iteration}");
            assert_eq!(checked, got.writes.iter().map(|p| p.0).collect::<Vec<_>>());
            assert_eq!(
                serde_json::to_value(got.report).unwrap(),
                serde_json::to_value(expected.report).unwrap()
            );
        }
    }
    #[test]
    fn checked_rejection_is_early_but_only_after_the_cells_final_modifier() {
        let base = vec![encode(20.); 64 * 64];
        let p = pose();
        let mut m = modifier(Mode::Set, 40.);
        m.radius = 20.;
        m.opacity = 1.;
        let mods = vec![m.clone(); 32];
        let input = |mods| Input {
            heights: &base,
            resolution: 64,
            size: 64.,
            initial: &[],
            modifiers: mods,
            pose: &p,
        };
        let mut old_work = 0;
        let expected = prepare_ordered(input(&mods), &mut old_work, 1_000_000, &|| false, true)
            .unwrap()
            .unwrap();
        let mut work = 0;
        let mut checked = 0;
        let result = prepare_checked(input(&mods), &mut work, 1_000_000, &|| false, |i| {
            checked += 1;
            assert_eq!(i, expected.writes[0].0);
            Err("protected")
        });
        assert!(matches!(result, Err(CheckedFailure::Cell("protected"))));
        assert_eq!(checked, 1);
        assert!(work * 10 < old_work);
        // A subsequent Set restoring the original must erase the intermediate change.
        m.position[1] = 20.;
        let restored: Vec<_> = mods.iter().cloned().chain(std::iter::once(m)).collect();
        let out = prepare_checked(input(&restored), &mut 0, 1_000_000, &|| false, |_| {
            Err("must not check reverted cells")
        })
        .unwrap()
        .unwrap();
        assert!(out.writes.is_empty());
        assert_eq!(out.report.changed_samples, 0);
        assert!(matches!(
            prepare_checked(input(&mods), &mut 0, 1, &|| false, |_| Ok::<_, ()>(())),
            Err(CheckedFailure::Height(Failure::Budget))
        ));
        assert!(
            prepare_checked(input(&mods), &mut 0, 1_000_000, &|| true, |_| Ok::<_, ()>(
                ()
            ))
            .unwrap()
            .is_none()
        );
    }
    fn pose() -> decor_fit::Pose {
        decor_fit::Pose {
            candidate: 0,
            position: [0.; 3],
            rotation: [0., 0., 0., 1.],
            scale: [1.; 3],
        }
    }
    fn modifier(mode: Mode, height: f32) -> Modifier {
        Modifier {
            position: [0., height, 0.],
            radius: 4.,
            fade: 0.,
            opacity: 0.5,
            mode,
        }
    }
    fn run(base: &[i16], initial: &[(usize, i16)], mods: &[Modifier], p: &decor_fit::Pose) -> Plan {
        prepare(
            Input {
                heights: base,
                resolution: 32,
                size: 32.,
                initial,
                modifiers: mods,
                pose: p,
            },
            &mut 0,
            100000,
            &|| false,
        )
        .unwrap()
        .unwrap()
    }
    #[test]
    fn mixed_modes_preserve_source_destination_semantics_and_order() {
        let base = vec![encode(20.); 32 * 32];
        let p = pose();
        let i = 16 * 32 + 16;
        let up = modifier(Mode::Raise, 40.);
        let down = modifier(Mode::Lower, 0.);
        let first = run(&base, &[], &[up.clone(), down.clone()], &p);
        let reverse = run(&base, &[], &[down, up], &p);
        assert!((decode(first.writes.iter().find(|p| p.0 == i).unwrap().1) - 10.).abs() < 0.04);
        assert!((decode(reverse.writes.iter().find(|p| p.0 == i).unwrap().1) - 30.).abs() < 0.04);
        let initial = [(i, encode(35.))];
        let set = run(&base, &initial, &[modifier(Mode::Set, 30.)], &p);
        assert!((decode(set.writes.iter().find(|p| p.0 == i).unwrap().1) - 25.).abs() < 0.04);
        assert!(set.report.lowered_samples > 0);
        assert_eq!(set.report.operations, 1);
    }
    #[test]
    fn road_aliasing_observes_prior_add_and_prior_placement_before_all_set_modes() {
        let base = vec![encode(20.); 32 * 32];
        let i = 16 * 32 + 16;
        let pose = pose();
        for (mode, expected) in [(Mode::Set, 36.), (Mode::Raise, 42.), (Mode::Lower, 36.)] {
            let add = modifier(Mode::Add { delta: 4. }, 0.);
            let set = modifier(mode, 30.);
            let modifiers = [add, set];
            let initial = [(i, encode(40.))];
            let out = prepare_ordered(
                Input {
                    heights: &base,
                    resolution: 32,
                    size: 32.,
                    initial: &initial,
                    modifiers: &modifiers,
                    pose: &pose,
                },
                &mut 0,
                100000,
                &|| false,
                true,
            )
            .unwrap()
            .unwrap();
            let got = decode(out.writes.iter().find(|v| v.0 == i).unwrap().1);
            assert!((got - expected).abs() < 0.04, "{mode:?}: {got}");
        }
    }
    #[test]
    fn additive_height_reads_destination_uses_linear_fade_and_does_not_scale_delta() {
        let base = vec![encode(20.); 32 * 32];
        let i = 16 * 32 + 16;
        let mut p = pose();
        p.scale[1] = 2.;
        let mut add = modifier(Mode::Add { delta: 4. }, -100.);
        add.radius = 4.;
        add.fade = 2.;
        let out = run(&base, &[(i, encode(35.))], std::slice::from_ref(&add), &p);
        let center = decode(out.writes.iter().find(|v| v.0 == i).unwrap().1);
        assert!((center - 37.).abs() < 0.04);
        let edge = 16 * 32 + 22;
        let d = 6.5_f32.hypot(0.5);
        let expected = 20. + 4. * 0.5 * ((8. - d) / 4.).clamp(0., 1.);
        assert!(
            (decode(out.writes.iter().find(|v| v.0 == edge).unwrap().1) - expected).abs() < 0.04
        );
        let mut plain = pose();
        plain.position[1] = 200.;
        let a = run(&base, &[], std::slice::from_ref(&add), &plain);
        plain.position[1] = -200.;
        let b = run(&base, &[], &[add], &plain);
        assert_eq!(a.writes, b.writes);
    }
    #[test]
    fn add_set_order_clamping_validation_and_cancellation_are_atomic() {
        let base = vec![encode(20.); 32 * 32];
        let i = 16 * 32 + 16;
        let add = modifier(Mode::Add { delta: 4. }, 0.);
        let set = modifier(Mode::Set, 40.);
        let a = run(&base, &[], &[add.clone(), set.clone()], &pose());
        let b = run(&base, &[], &[set, add.clone()], &pose());
        assert!((decode(a.writes.iter().find(|v| v.0 == i).unwrap().1) - 30.).abs() < 0.04);
        assert!((decode(b.writes.iter().find(|v| v.0 == i).unwrap().1) - 32.).abs() < 0.04);
        let high = vec![encode(499.); 32 * 32];
        let mut large = add.clone();
        large.mode = Mode::Add { delta: 100. };
        assert_eq!(
            run(&high, &[], &[large], &pose())
                .writes
                .iter()
                .find(|v| v.0 == i)
                .unwrap()
                .1,
            32766
        );
        let mut bad = add.clone();
        bad.mode = Mode::Add { delta: f32::NAN };
        assert!(validate(&[bad]).is_err());
        let calls = std::cell::Cell::new(0);
        let p = pose();
        let result = prepare(
            Input {
                heights: &base,
                resolution: 32,
                size: 32.,
                initial: &[],
                modifiers: &[add],
                pose: &p,
            },
            &mut 0,
            100000,
            &|| {
                calls.set(calls.get() + 1);
                calls.get() > 3
            },
        )
        .unwrap();
        assert!(result.is_none());
        assert!(base.iter().all(|v| *v == encode(20.)));
    }
    #[test]
    fn standalone_height_modes_have_no_arbitrary_four_metre_cap_and_no_synthetic_footprint() {
        let base = vec![encode(20.); 32 * 32];
        let mut m = modifier(Mode::Raise, 40.);
        m.opacity = 1.;
        let result = run(&base, &[], &[m], &pose());
        assert!(result.report.maximum_raise > 19.9);
        assert_eq!(result.report.instances, 1);
        for (i, h) in result.writes {
            let x = (i % 32) as f32 + 0.5 - 16.;
            let z = (i / 32) as f32 + 0.5 - 16.;
            assert!(x.hypot(z) < 4.);
            assert!((decode(h) - 40.).abs() < 0.04);
        }
        assert!(base.iter().all(|v| *v == encode(20.)));
    }
    #[test]
    fn height_offsets_rotate_with_pose_while_radius_and_fade_use_y_scale() {
        let base = vec![encode(20.); 32 * 32];
        let a = 37_f32.to_radians();
        let mut p = pose();
        p.rotation = [0., (a / 2.).sin(), 0., (a / 2.).cos()];
        p.scale = [2., 1.5, 0.5];
        p.position[1] = 20.;
        let m = Modifier {
            position: [3., 4., 1.],
            radius: 2.,
            fade: 1.,
            opacity: 0.55,
            mode: Mode::Set,
        };
        let result = run(&base, &[], &[m], &p);
        let cx = 6. * a.cos() + 0.5 * a.sin();
        let cz = -6. * a.sin() + 0.5 * a.cos();
        assert!(!result.writes.is_empty());
        for (i, h) in result.writes {
            let d = ((i % 32) as f32 + 0.5 - 16. - cx).hypot((i / 32) as f32 + 0.5 - 16. - cz);
            assert!(d < 3.);
            let t = ((3. - d) / 1.5).clamp(0., 1.) * 0.55;
            let expected = 20. + 6. * (t * t * (3. - 2. * t));
            assert!((decode(h) - expected).abs() < 0.05);
        }
    }
    #[test]
    fn failed_or_cancelled_height_plans_never_modify_inputs_or_return_partial_writes() {
        let base = vec![encode(20.); 32 * 32];
        let original = base.clone();
        let p = pose();
        let mut m = modifier(Mode::Set, 40.);
        m.radius = 8.;
        let mods = [m.clone()];
        let input = || Input {
            heights: &base,
            resolution: 32,
            size: 32.,
            initial: &[],
            modifiers: &mods,
            pose: &p,
        };
        assert!(matches!(
            prepare(input(), &mut 0, 100, &|| false),
            Err(Failure::Budget)
        ));
        let calls = std::cell::Cell::new(0);
        assert!(
            prepare(input(), &mut 0, 100000, &|| {
                calls.set(calls.get() + 1);
                calls.get() > 3
            })
            .unwrap()
            .is_none()
        );
        assert_eq!(base, original);
        m.position[1] = 501.;
        assert!(matches!(
            prepare(
                Input {
                    modifiers: &[m.clone()],
                    ..input()
                },
                &mut 0,
                100000,
                &|| false
            ),
            Err(Failure::Unsupported)
        ));
        m.position = [15., 0., 0.];
        assert!(matches!(
            prepare(
                Input {
                    modifiers: &[m],
                    ..input()
                },
                &mut 0,
                100000,
                &|| false
            ),
            Err(Failure::Outside)
        ));
    }
}
