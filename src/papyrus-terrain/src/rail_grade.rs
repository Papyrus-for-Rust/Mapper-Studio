//! Adaptive bounds on grade along the actual source-style cubic Hermite path.
//! No terrain/physics acceptance or game-specific train grade limit is implied.
use crate::path_geometry::Path;
use serde::Serialize;
pub const VERSION: &str = "papyrus-hermite-grade-audit-1";
type V = [f64; 3];
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Settings {
    pub absolute_tolerance: f64,
    pub maximum_work: usize,
    pub maximum_depth: u8,
}
#[derive(Debug, PartialEq)]
pub enum Failure {
    Invalid,
    Budget,
}
#[derive(Debug, Serialize)]
pub struct Audit {
    pub engine: &'static str,
    pub settings: Settings,
    pub maximum_observed_grade: f64,
    pub maximum_grade_upper_bound: Option<f64>,
    pub unresolved_intervals: usize,
    pub segments: usize,
    pub work: usize,
}
fn midpoint(a: V, b: V) -> V {
    std::array::from_fn(|i| (a[i] + b[i]) * 0.5)
}
fn observed(v: V) -> Option<f64> {
    let horizontal = v[0].hypot(v[2]);
    (horizontal > 0.).then(|| v[1].abs() / horizontal)
}
fn upper(d: [V; 3]) -> Option<f64> {
    let bounds: [(f64, f64); 3] = std::array::from_fn(|axis| {
        let lo = d.iter().map(|v| v[axis]).fold(f64::INFINITY, f64::min);
        let hi = d.iter().map(|v| v[axis]).fold(f64::NEG_INFINITY, f64::max);
        // Roundoff allowance for arithmetic in the derivative control hull.
        let pad = 64. * f64::EPSILON * lo.abs().max(hi.abs()).max(1.);
        (lo - pad, hi + pad)
    });
    let nearest = |(lo, hi): (f64, f64)| {
        if lo > 0. {
            lo
        } else if hi < 0. {
            -hi
        } else {
            0.
        }
    };
    let horizontal = nearest(bounds[0]).hypot(nearest(bounds[2]));
    (horizontal > 0.).then(|| bounds[1].0.abs().max(bounds[1].1.abs()) / horizontal)
}
/// Derivative quadratic Bézier hulls bound grade over entire intervals, rather
/// than assuming that control-point slopes or a fixed sample spacing suffice.
/// Singular/unresolved intervals keep the aggregate upper bound unavailable.
pub fn analyze(
    path: &Path,
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Audit>, Failure> {
    analyze_segments(path, settings, |_, _| {}, cancelled)
}

/// Report each segment's certified upper bound to the connected bed fitter.
pub(crate) fn analyze_segments(
    path: &Path,
    settings: Settings,
    mut segment: impl FnMut(usize, Option<f64>),
    cancelled: impl Fn() -> bool,
) -> Result<Option<Audit>, Failure> {
    if !settings.absolute_tolerance.is_finite()
        || !(0.000001..=0.01).contains(&settings.absolute_tolerance)
        || !(1..=4_000_000).contains(&settings.maximum_work)
        || !(1..=32).contains(&settings.maximum_depth)
        || path.range() != (0, path.points().len() - 1)
    {
        return Err(Failure::Invalid);
    }
    let mut audit = Audit {
        engine: VERSION,
        settings,
        maximum_observed_grade: 0.,
        maximum_grade_upper_bound: None,
        unresolved_intervals: 0,
        segments: path.points().len() - 1,
        work: 0,
    };
    let mut maximum_bound: f64 = 0.;
    for i in 0..audit.segments {
        let a = path.points()[i];
        let b = path.points()[i + 1];
        let ta = path.tangents()[i];
        let tb = path.tangents()[i + 1];
        let step = f64::from(path.step_size());
        let derivative: [V; 3] = [
            ta.map(|v| f64::from(v) * step),
            std::array::from_fn(|j| {
                3. * (f64::from(b[j]) - f64::from(a[j]))
                    - step * (f64::from(ta[j]) + f64::from(tb[j]))
            }),
            tb.map(|v| f64::from(v) * step),
        ];
        let mut pending = vec![(derivative, 0u8)];
        let mut segment_bound: f64 = 0.;
        let unresolved_before = audit.unresolved_intervals;
        let mut segment_observed: f64 = 0.;
        while let Some((d, depth)) = pending.pop() {
            if cancelled() {
                return Ok(None);
            }
            if audit.work >= settings.maximum_work {
                return Err(Failure::Budget);
            }
            audit.work += 1;
            let left = midpoint(d[0], d[1]);
            let right = midpoint(d[1], d[2]);
            let middle = midpoint(left, right);
            for sample in [d[0], middle, d[2]] {
                if let Some(grade) = observed(sample) {
                    audit.maximum_observed_grade = audit.maximum_observed_grade.max(grade);
                    segment_observed = segment_observed.max(grade);
                }
            }
            let bound = upper(d);
            if let Some(value) =
                bound.filter(|&v| v <= audit.maximum_observed_grade + settings.absolute_tolerance)
            {
                maximum_bound = maximum_bound.max(value);
                segment_bound = segment_bound.max(value);
            } else if depth >= settings.maximum_depth || d.iter().all(|v| v[0] == 0. && v[2] == 0.)
            {
                audit.unresolved_intervals += 1;
            } else {
                pending.push(([middle, right, d[2]], depth + 1));
                pending.push(([d[0], left, middle], depth + 1));
            }
        }
        segment(
            i,
            (audit.unresolved_intervals == unresolved_before)
                .then_some(segment_bound.max(segment_observed)),
        );
    }
    if audit.unresolved_intervals == 0 {
        audit.maximum_grade_upper_bound = Some(maximum_bound.max(audit.maximum_observed_grade));
    }
    Ok(Some(audit))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn settings() -> Settings {
        Settings {
            absolute_tolerance: 0.0001,
            maximum_work: 100000,
            maximum_depth: 24,
        }
    }
    fn path(points: &[[f32; 3]]) -> Path {
        Path::from_controls(points, 10000, || false)
            .unwrap()
            .unwrap()
    }
    #[test]
    fn linear_grades_are_bounded_and_hermite_overshoot_is_not_hidden() {
        let p = path(&[[0., 0., 0.], [10., 1., 0.], [20., 2., 0.]]);
        let a = analyze(&p, settings(), || false).unwrap().unwrap();
        assert!((a.maximum_observed_grade - 0.1).abs() < 0.000001);
        assert!(a.maximum_grade_upper_bound.unwrap() >= a.maximum_observed_grade);
        assert!(a.maximum_grade_upper_bound.unwrap() < 0.1001);
        let p = path(&[[0., 0., 0.], [1., 1., 0.], [2., 1., 0.]]);
        let a = analyze(&p, settings(), || false).unwrap().unwrap();
        // The control polyline's maximum grade is 1; its Hermite curve exceeds it.
        assert!(a.maximum_observed_grade > 1.1);
        assert!(a.maximum_grade_upper_bound.unwrap() - a.maximum_observed_grade <= 0.0001);
    }
    #[test]
    fn dense_independent_derivatives_stay_below_the_interval_bound() {
        for points in [
            vec![[0., 2., 0.], [10., 4., 6.], [23., 1., 9.], [30., 8., 20.]],
            vec![
                [0., 0., 0.],
                [10., 1., 0.],
                [10., 2., 10.],
                [0., 3., 10.],
                [0., 0., 0.],
            ],
        ] {
            let p = path(&points);
            let a = analyze(&p, settings(), || false).unwrap().unwrap();
            let mut dense: f64 = 0.;
            for i in 0..points.len() - 1 {
                for j in 0..=8192 {
                    let t = j as f64 / 8192.;
                    let t2 = t * t;
                    let derivative: V = std::array::from_fn(|axis| {
                        (6. * t2 - 6. * t) * f64::from(points[i][axis])
                            + (3. * t2 - 4. * t + 1.)
                                * f64::from(p.tangents()[i][axis])
                                * f64::from(p.step_size())
                            + (-6. * t2 + 6. * t) * f64::from(points[i + 1][axis])
                            + (3. * t2 - 2. * t)
                                * f64::from(p.tangents()[i + 1][axis])
                                * f64::from(p.step_size())
                    });
                    dense = dense.max(observed(derivative).unwrap());
                }
            }
            assert!(dense <= a.maximum_grade_upper_bound.unwrap() + 1e-12);
            assert!((dense - a.maximum_observed_grade).abs() < 0.0001);
        }
    }
    #[test]
    fn singularities_cancellation_and_work_exhaustion_never_report_a_pass() {
        for points in [vec![[0., 0., 0.], [0., 10., 0.]], vec![[0.; 3], [0.; 3]]] {
            let p = path(&points);
            let a = analyze(&p, settings(), || false).unwrap().unwrap();
            assert!(a.maximum_grade_upper_bound.is_none());
            assert!(a.unresolved_intervals > 0);
        }
        let p = path(&[[0., 0., 0.], [1., 1., 0.], [2., 1., 0.]]);
        let before = serde_json::to_vec(&p).unwrap();
        assert!(analyze(&p, settings(), || true).unwrap().is_none());
        assert!(matches!(
            analyze(
                &p,
                Settings {
                    maximum_work: 1,
                    ..settings()
                },
                || false
            ),
            Err(Failure::Budget)
        ));
        assert_eq!(before, serde_json::to_vec(&p).unwrap());
    }
}
