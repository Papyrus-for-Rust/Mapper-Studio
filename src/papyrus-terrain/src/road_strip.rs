//! Bounded strip-orientation preflight, not collision-mesh or vehicle acceptance.
use crate::path_geometry::Path;

/// Check affected spline intervals and their neighbours at approximately 1 m.
/// The caller must enforce its work allowance in `spend`; cancellation publishes
/// no partial result. Invalid geometry is rejected without indexing unchecked data.
pub fn check<E>(
    path: &Path,
    affected: &[bool],
    radius: f32,
    spend: impl FnMut(usize) -> Result<(), E>,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<bool>, E> {
    check_with_witness(path, affected, radius, spend, cancelled, |_, _| {})
}

/// Retain the first rejected sample for bounded geometry recovery. The same
/// orientation test and work charges apply to callers that only need a verdict.
pub fn check_with_witness<E>(
    path: &Path,
    affected: &[bool],
    radius: f32,
    mut spend: impl FnMut(usize) -> Result<(), E>,
    cancelled: &impl Fn() -> bool,
    mut rejected: impl FnMut(usize, [f32; 3]),
) -> Result<Option<bool>, E> {
    let points = path.points();
    if points.len() < 2
        || affected.len() != points.len()
        || !radius.is_finite()
        || radius <= 0.
        || !path.length().is_finite()
        || path.length() <= 0.
    {
        return Ok(Some(false));
    }
    let step = path.length() / (points.len() - 1) as f32;
    let orientation = |a: [f32; 2], b: [f32; 2], c: [f32; 2]| {
        (f64::from(b[0]) - f64::from(a[0])) * (f64::from(c[1]) - f64::from(a[1]))
            - (f64::from(b[1]) - f64::from(a[1])) * (f64::from(c[0]) - f64::from(a[0]))
    };
    for i in 0..points.len() - 1 {
        let lo = i.saturating_sub(1);
        let hi = (i + 3).min(affected.len());
        if !affected[lo..hi].iter().any(|v| *v) {
            continue;
        }
        let count = step.ceil().max(2.) as usize;
        let mut previous = None;
        for sample in 0..=count {
            if cancelled() {
                return Ok(None);
            }
            spend(1)?;
            let d = (i as f32 + sample as f32 / count as f32) * step;
            let Ok(p) = path.point(d) else {
                return Ok(Some(false));
            };
            let Ok(tan) = path.tangent(d) else {
                return Ok(Some(false));
            };
            let length = (tan[0] * tan[0] + tan[2] * tan[2]).sqrt();
            if length < 1e-5 {
                rejected(i, p);
                return Ok(Some(false));
            }
            let side = [tan[2] / length, -tan[0] / length];
            let left = [p[0] - side[0] * radius, p[2] - side[1] * radius];
            let right = [p[0] + side[0] * radius, p[2] + side[1] * radius];
            if let Some((a, b)) = previous
                && (orientation(a, b, left) <= 1e-8 || orientation(b, right, left) <= 1e-8)
            {
                rejected(i, p);
                return Ok(Some(false));
            }
            previous = Some((left, right));
        }
    }
    Ok(Some(true))
}

pub struct FixedHeight {
    pub height: f32,
    pub fixed: bool,
    pub changed: bool,
}
/// Necessary feasibility check for adjacent frozen edge samples. Editable
/// sections are left to the bed solver; this does not certify the final mesh.
pub fn check_fixed_grades<E>(
    path: &Path,
    affected: &[bool],
    radii: &[f32],
    maximum_grade: f32,
    mut query: impl FnMut([f32; 3]) -> Result<FixedHeight, E>,
    mut spend: impl FnMut(usize) -> Result<(), E>,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<bool>, E> {
    let points = path.points();
    if points.len() < 2
        || affected.len() != points.len()
        || radii.is_empty()
        || radii.len() > 16
        || radii.iter().any(|r| !r.is_finite() || *r <= 0.)
        || !maximum_grade.is_finite()
        || maximum_grade <= 0.
        || maximum_grade > 1.
        || !path.length().is_finite()
        || path.length() <= 0.
    {
        return Ok(Some(false));
    }
    let step = path.length() / (points.len() - 1) as f32;
    for i in 0..points.len() - 1 {
        if !affected[i.saturating_sub(1)..(i + 3).min(affected.len())]
            .iter()
            .any(|v| *v)
        {
            continue;
        }
        let count = step.ceil().max(2.) as usize;
        for &radius in radii {
            let mut previous: Option<([[f32; 3]; 2], [FixedHeight; 2])> = None;
            for sample in 0..=count {
                if cancelled() {
                    return Ok(None);
                }
                spend(1)?;
                let d = (i as f32 + sample as f32 / count as f32) * step;
                let (Ok(p), Ok(tan)) = (path.point(d), path.tangent(d)) else {
                    return Ok(Some(false));
                };
                let length = (tan[0] * tan[0] + tan[2] * tan[2]).sqrt();
                if length < 1e-5 {
                    return Ok(Some(false));
                }
                let side = [tan[2] / length, -tan[0] / length];
                let positions = [
                    [p[0] - side[0] * radius, 0., p[2] - side[1] * radius],
                    [p[0] + side[0] * radius, 0., p[2] + side[1] * radius],
                ];
                let heights = [query(positions[0])?, query(positions[1])?];
                if heights.iter().any(|h| !h.height.is_finite()) {
                    return Ok(Some(false));
                }
                if let Some((old_positions, old_heights)) = previous {
                    for edge in 0..2 {
                        let a = &old_heights[edge];
                        let b = &heights[edge];
                        let run = (positions[edge][0] - old_positions[edge][0])
                            .hypot(positions[edge][2] - old_positions[edge][2]);
                        if a.fixed
                            && b.fixed
                            && (a.changed || b.changed)
                            && (a.height - b.height).abs() > maximum_grade * run + 1e-5
                        {
                            return Ok(Some(false));
                        }
                    }
                }
                previous = Some((positions, heights));
            }
        }
    }
    Ok(Some(true))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_edge_grades_catch_crossfall_hidden_by_a_flat_centerline() {
        let path = Path::from_controls(&[[0., 0., 0.], [0., 0., 20.], [0., 0., 40.]], 1000, || {
            false
        })
        .unwrap()
        .unwrap();
        let query = |fixed, changed| {
            move |p: [f32; 3]| {
                Ok::<_, &str>(FixedHeight {
                    height: p[0] * p[2] * 0.05,
                    fixed,
                    changed,
                })
            }
        };
        let spend = |_| Ok::<_, &str>(());
        assert_eq!(
            check_fixed_grades(
                &path,
                &[true; 3],
                &[5.],
                0.2,
                query(true, true),
                spend,
                &|| false
            )
            .unwrap(),
            Some(false)
        );
        assert_eq!(
            check_fixed_grades(
                &path,
                &[true; 3],
                &[5.],
                0.2,
                query(false, true),
                spend,
                &|| false
            )
            .unwrap(),
            Some(true)
        );
        assert_eq!(
            check_fixed_grades(
                &path,
                &[true; 3],
                &[5.],
                0.2,
                query(true, false),
                spend,
                &|| false
            )
            .unwrap(),
            Some(true)
        );
        assert_eq!(
            check_fixed_grades(
                &path,
                &[true; 3],
                &[5.],
                0.2,
                query(true, true),
                spend,
                &|| true
            )
            .unwrap(),
            None
        );
        assert_eq!(
            check_fixed_grades(
                &path,
                &[true; 3],
                &[5.],
                0.2,
                query(true, true),
                |_| Err("budget"),
                &|| false
            ),
            Err("budget")
        );
    }
    #[test]
    fn fold_witness_preserves_verdict_work_and_cancellation() {
        let path =
            Path::from_controls(&[[0., 0., 0.], [1., 0., 0.], [1., 0., 10.]], 1000, || false)
                .unwrap()
                .unwrap();
        let mut plain_work = 0;
        let plain = check(
            &path,
            &[true; 3],
            5.,
            |n| {
                plain_work += n;
                Ok::<_, &str>(())
            },
            &|| false,
        )
        .unwrap();
        let mut detailed_work = 0;
        let mut witness = None;
        let detailed = check_with_witness(
            &path,
            &[true; 3],
            5.,
            |n| {
                detailed_work += n;
                Ok::<_, &str>(())
            },
            &|| false,
            |interval, position| witness = Some((interval, position)),
        )
        .unwrap();
        assert_eq!(plain, Some(false));
        assert_eq!(detailed, plain);
        assert_eq!(plain_work, detailed_work);
        let (interval, position) = witness.unwrap();
        assert!(interval < path.points().len() - 1);
        assert!(position.iter().all(|v| v.is_finite()));
        assert_eq!(
            check_with_witness(
                &path,
                &[true; 3],
                5.,
                |_| Ok::<_, &str>(()),
                &|| true,
                |_, _| panic!("Cancelled check emitted a witness")
            )
            .unwrap(),
            None
        );
        assert_eq!(
            check_with_witness(
                &path,
                &[true; 3],
                5.,
                |_| Err("budget"),
                &|| false,
                |_, _| panic!("Exhausted check emitted a witness")
            ),
            Err("budget")
        );
    }
    #[test]
    fn rejects_folds_invalid_masks_and_exhausted_work_without_hiding_cancellation() {
        let straight =
            Path::from_controls(&[[0., 0., 0.], [0., 0., 20.], [0., 0., 40.]], 1000, || {
                false
            })
            .unwrap()
            .unwrap();
        let tight =
            Path::from_controls(&[[0., 0., 0.], [1., 0., 0.], [1., 0., 10.]], 1000, || false)
                .unwrap()
                .unwrap();
        let free = |_| Ok::<(), &str>(());
        assert_eq!(
            check(&straight, &[true; 3], 5., free, &|| false).unwrap(),
            Some(true)
        );
        assert_eq!(
            check(&tight, &[true; 3], 5., free, &|| false).unwrap(),
            Some(false)
        );
        assert_eq!(
            check(&straight, &[true; 2], 5., free, &|| false).unwrap(),
            Some(false)
        );
        assert_eq!(
            check(&straight, &[true; 3], 5., free, &|| true).unwrap(),
            None
        );
        assert_eq!(
            check(&straight, &[true; 3], 5., |_| Err("budget"), &|| false),
            Err("budget")
        );
    }
}
