//! Prepared tagged-volume clearance for the reviewed roadside pipe policy.
//! The caller supplies resolved world-space tag boxes and registry coverage.
use crate::{
    decor_stream::Budget,
    environment::{Assessment, Box3, Coverage, Key, Transform, Verdict, rotate},
};
use std::collections::BTreeSet;

pub const VERSION: &str = "papyrus-water-treatment-clearance-1";

/// Sphere tags use an axis-aligned box and the largest absolute lossy scale.
/// The caller supplies the collider center after Unity-style TransformPoint,
/// including any shear in the full local-to-world matrix.
pub fn sphere_bounds(
    center_world: [f32; 3],
    radius: f32,
    lossy_scale: [f32; 3],
) -> Result<Box3, String> {
    if !radius.is_finite()
        || !(0. ..=10000.).contains(&radius)
        || lossy_scale
            .iter()
            .any(|value| !value.is_finite() || value.abs() > 10000.)
    {
        return Err("Invalid tagged sphere dimensions".into());
    }
    let maximum_scale = lossy_scale
        .iter()
        .map(|value| value.abs())
        .fold(0., f32::max);
    let extent = ((radius + radius) * maximum_scale) * 0.5;
    let result = Box3 {
        center: center_world,
        rotation: [0., 0., 0., 1.],
        extents: [extent; 3],
    };
    result.validate()?;
    Ok(result)
}

/// Capsule tags use the collider's oriented bounding box. Height is clamped
/// to the diameter before applying the transform's per-axis lossy scale.
pub fn capsule_bounds(
    transform: Transform,
    center: [f32; 3],
    radius: f32,
    height: f32,
    direction: u8,
) -> Result<Box3, String> {
    transform.validate()?;
    if !radius.is_finite()
        || !(0. ..=10000.).contains(&radius)
        || !height.is_finite()
        || !(0. ..=20000.).contains(&height)
        || direction > 2
        || center
            .iter()
            .any(|value| !value.is_finite() || value.abs() > 1_000_000.)
    {
        return Err("Invalid tagged capsule dimensions".into());
    }
    let diameter = radius + radius;
    let mut size = [diameter; 3];
    size[usize::from(direction)] = height.max(diameter);
    let offset = rotate(
        transform.rotation,
        std::array::from_fn(|i| center[i] * transform.scale[i]),
    );
    let result = Box3 {
        center: std::array::from_fn(|i| transform.position[i] + offset[i]),
        rotation: transform.rotation,
        extents: std::array::from_fn(|i| (size[i] * 0.5) * transform.scale[i]),
    };
    result.validate()?;
    Ok(result)
}

struct PreparedBox {
    key: Key,
    center: [f32; 3],
    extents: [f32; 3],
    axes: [[f32; 3]; 3],
    rejection_squared: f32,
}

/// Immutable preparation is reusable across every candidate in one world.
pub struct Clearance {
    boxes: Vec<PreparedBox>,
    padding: f32,
    coverage: Coverage,
}

fn squared_length(p: [f32; 3]) -> f32 {
    (p[0] * p[0] + p[1] * p[1]) + p[2] * p[2]
}

impl Clearance {
    /// Keep registry order so the first blocking tag remains reproducible.
    /// Coverage describes all relevant active tags in the generated world.
    pub fn new(boxes: &[(Key, Box3)], padding: f32, coverage: Coverage) -> Result<Self, String> {
        if boxes.len() > 131072 || !padding.is_finite() || !(0. ..=10000.).contains(&padding) {
            return Err("Invalid water-treatment clearance inputs".into());
        }
        let mut keys = BTreeSet::new();
        let mut prepared = Vec::with_capacity(boxes.len());
        for &(key, shape) in boxes {
            shape.validate()?;
            if !keys.insert(key) {
                return Err("Duplicate water-treatment clearance tag".into());
            }
            // OBB.reject stores squared extents length in the reviewed client.
            // Its pipe broad phase adds raw padding, then squares that sum.
            let rejection = squared_length(shape.extents) + padding;
            prepared.push(PreparedBox {
                key,
                center: shape.center,
                extents: shape.extents,
                axes: [
                    rotate(shape.rotation, [1., 0., 0.]),
                    rotate(shape.rotation, [0., 1., 0.]),
                    rotate(shape.rotation, [0., 0., 1.]),
                ],
                rejection_squared: rejection * rejection,
            });
        }
        Ok(Self {
            boxes: prepared,
            padding,
            coverage,
        })
    }

    /// Query the fitted origin with shared work accounting and no allocation.
    /// Incomplete tag coverage cannot establish a clear placement.
    pub fn check(
        &self,
        position: [f32; 3],
        budget: &mut Budget,
        cancelled: impl Fn() -> bool,
    ) -> Result<Assessment, String> {
        if position
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 1_000_000.)
        {
            return Err("Invalid water-treatment clearance position".into());
        }
        let initial_work = budget.used();
        let mut assessment = Assessment {
            verdict: Verdict::Clear,
            work: 0,
            candidates: 0,
        };
        if cancelled() {
            assessment.verdict = Verdict::Cancelled;
            return Ok(assessment);
        }
        if self.coverage != Coverage::Complete {
            assessment.verdict = Verdict::Incomplete;
            return Ok(assessment);
        }
        for shape in &self.boxes {
            if cancelled() {
                assessment.verdict = Verdict::Cancelled;
                break;
            }
            if budget.take().is_err() {
                assessment.verdict = Verdict::Budget;
                break;
            }
            let delta = std::array::from_fn(|i| position[i] - shape.center[i]);
            if squared_length(delta) > shape.rejection_squared {
                continue;
            }
            assessment.candidates += 1;
            let closest = shape.closest_point(position, delta);
            let distance_squared =
                squared_length(std::array::from_fn(|i| position[i] - closest[i]));
            // The inspected pipe method compares squared distance to raw padding.
            if distance_squared < self.padding {
                assessment.verdict = Verdict::Blocked(shape.key);
                break;
            }
        }
        assessment.work = budget.used() - initial_work;
        Ok(assessment)
    }
}

impl PreparedBox {
    fn closest_point(&self, position: [f32; 3], delta: [f32; 3]) -> [f32; 3] {
        let mut closest = self.center;
        let mut inside = true;
        for (axis, extent) in self.axes.iter().zip(self.extents) {
            let projection = (delta[0] * axis[0] + delta[1] * axis[1]) + delta[2] * axis[2];
            inside &= projection >= -extent && projection <= extent;
            let clamped = projection.clamp(-extent, extent);
            for i in 0..3 {
                closest[i] += clamped * axis[i];
            }
        }
        // OBB.ClosestPoint returns the original point verbatim when inside.
        if inside { position } else { closest }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::multiply;

    fn key(component: i64) -> Key {
        Key {
            instance: 1,
            component,
        }
    }
    fn shape() -> Box3 {
        Box3 {
            center: [0.; 3],
            rotation: [0., 0., 0., 1.],
            extents: [10., 3., 4.],
        }
    }
    fn verdict(clearance: &Clearance, position: [f32; 3]) -> Verdict {
        clearance
            .check(position, &mut Budget::new(100).unwrap(), || false)
            .unwrap()
            .verdict
    }

    #[test]
    fn sphere_tag_bounds_use_world_center_absolute_maximum_scale_and_identity_rotation() {
        let center = [14., -8., 23.];
        let bounds = sphere_bounds(center, 2., [-3., 1., 2.]).unwrap();
        assert_eq!(bounds.center, center);
        assert_eq!(bounds.extents, [6.; 3]);
        assert_eq!(bounds.rotation, [0., 0., 0., 1.]);
        assert_eq!(sphere_bounds(center, 2., [0., -3., 0.]).unwrap(), bounds);
        let clearance = Clearance::new(&[(key(1), bounds)], 4., Coverage::Complete).unwrap();
        assert_eq!(
            verdict(&clearance, [21., -8., 23.]),
            Verdict::Blocked(key(1))
        );
        assert_eq!(verdict(&clearance, [22., -8., 23.]), Verdict::Clear);
    }

    #[test]
    fn capsule_tag_bounds_keep_each_direction_nonuniform_scale_and_authored_center() {
        let transform = Transform {
            position: [50., 12., -30.],
            rotation: [0., (0.3_f32).sin(), 0., (0.3_f32).cos()],
            scale: [2., 3., 4.],
        };
        let center = [1., -2., 3.];
        let offset = rotate(transform.rotation, [2., -6., 12.]);
        for direction in 0..3 {
            let bounds = capsule_bounds(transform, center, 2., 10., direction).unwrap();
            assert_eq!(
                bounds.center,
                std::array::from_fn(|i| transform.position[i] + offset[i])
            );
            assert_eq!(bounds.rotation, transform.rotation);
            let mut expected = [4., 6., 8.];
            expected[usize::from(direction)] = 5. * transform.scale[usize::from(direction)];
            assert_eq!(bounds.extents, expected);
            let short = capsule_bounds(transform, center, 2., 1., direction).unwrap();
            assert_eq!(short.extents, [4., 6., 8.]);
        }
    }

    #[test]
    fn primitive_tag_bounds_reject_nonfinite_degenerate_and_unbounded_inputs() {
        let transform = Transform {
            position: [0.; 3],
            rotation: [0., 0., 0., 1.],
            scale: [1.; 3],
        };
        assert_eq!(
            capsule_bounds(transform, [0.; 3], 10000., 20000., 1)
                .unwrap()
                .extents,
            [10000.; 3]
        );
        assert_eq!(
            sphere_bounds([0.; 3], 10000., [1.; 3]).unwrap().extents,
            [10000.; 3]
        );
        for radius in [-1., 0., f32::NAN, f32::INFINITY, 10001.] {
            assert!(sphere_bounds([0.; 3], radius, [1.; 3]).is_err());
            assert!(capsule_bounds(transform, [0.; 3], radius, 10., 1).is_err());
        }
        for scale in [[f32::NAN; 3], [f32::INFINITY; 3], [10001.; 3], [0.; 3]] {
            assert!(sphere_bounds([0.; 3], 1., scale).is_err());
        }
        for height in [-1., f32::NAN, f32::INFINITY, 20001.] {
            assert!(capsule_bounds(transform, [0.; 3], 1., height, 1).is_err());
        }
        assert!(capsule_bounds(transform, [0.; 3], 1., 10., 3).is_err());
        assert!(
            capsule_bounds(
                Transform {
                    scale: [-1., 1., 1.],
                    ..transform
                },
                [0.; 3],
                1.,
                10.,
                1
            )
            .is_err()
        );
        assert!(sphere_bounds([f32::NAN; 3], 1., [1.; 3]).is_err());
        assert!(capsule_bounds(transform, [f32::NAN; 3], 1., 10., 1).is_err());
    }

    #[test]
    fn padding_uses_strict_raw_threshold_for_squared_three_dimensional_distance() {
        let c = Clearance::new(&[(key(1), shape())], 4., Coverage::Complete).unwrap();
        for point in [
            [0.; 3],
            [10., 0., 0.],
            [11.99, 0., 0.],
            [0., 4.99, 0.],
            [0., 0., 5.99],
        ] {
            assert_eq!(verdict(&c, point), Verdict::Blocked(key(1)));
        }
        for point in [[12., 0., 0.], [0., 5., 0.], [0., 0., 6.], [11.5, 4.5, 0.]] {
            assert_eq!(verdict(&c, point), Verdict::Clear);
        }
        let zero = Clearance::new(&[(key(1), shape())], 0., Coverage::Complete).unwrap();
        assert_eq!(verdict(&zero, [0.; 3]), Verdict::Clear);
    }

    #[test]
    fn arbitrary_rotation_and_translation_keep_vertical_clearance() {
        let yaw = [0., (0.37_f32).sin(), 0., (0.37_f32).cos()];
        let pitch = [(0.22_f32).sin(), 0., 0., (0.22_f32).cos()];
        let s = Box3 {
            center: [123., 17., -98.],
            rotation: multiply(yaw, pitch),
            ..shape()
        };
        let c = Clearance::new(&[(key(1), s)], 4., Coverage::Complete).unwrap();
        for (local, blocked) in [
            ([0.; 3], true),
            ([11., 0., 0.], true),
            ([13., 0., 0.], false),
            ([0., 6., 0.], false),
            ([0., 0., 7.], false),
        ] {
            let offset = rotate(s.rotation, local);
            let world = std::array::from_fn(|i| s.center[i] + offset[i]);
            assert_eq!(
                verdict(&c, world),
                if blocked {
                    Verdict::Blocked(key(1))
                } else {
                    Verdict::Clear
                }
            );
        }
    }

    #[test]
    fn broad_phase_preserves_squared_extent_rejection_and_equality() {
        // reject = 0.1^2 * 3, so tiny boxes expose the exact source broad phase.
        let s = Box3 {
            extents: [0.1; 3],
            ..shape()
        };
        let c = Clearance::new(&[(key(1), s)], 0., Coverage::Complete).unwrap();
        let threshold = squared_length(s.extents);
        let mut budget = Budget::new(10).unwrap();
        let at = c.check([threshold, 0., 0.], &mut budget, || false).unwrap();
        let beyond = c
            .check([threshold + 0.0001, 0., 0.], &mut budget, || false)
            .unwrap();
        assert_eq!(at.candidates, 1);
        assert_eq!(beyond.candidates, 0);
        assert_eq!(at.work, 1);
        assert_eq!(beyond.work, 1);
    }

    #[test]
    fn first_blocker_and_work_follow_registry_order_and_share_the_attempt_allowance() {
        let far = Box3 {
            center: [900000., 0., 0.],
            ..shape()
        };
        let c = Clearance::new(
            &[(key(2), far), (key(7), shape()), (key(1), shape())],
            7.,
            Coverage::Complete,
        )
        .unwrap();
        let mut budget = Budget::new(3).unwrap();
        let hit = c.check([0.; 3], &mut budget, || false).unwrap();
        assert_eq!(hit.verdict, Verdict::Blocked(key(7)));
        assert_eq!((hit.work, hit.candidates, budget.used()), (2, 1, 2));
        let exhausted = c.check([0.; 3], &mut budget, || false).unwrap();
        assert_eq!(exhausted.verdict, Verdict::Budget);
        assert_eq!(
            (exhausted.work, exhausted.candidates, budget.used()),
            (1, 0, 3)
        );
        assert_eq!(
            c.check([0.; 3], &mut budget, || false).unwrap().verdict,
            Verdict::Budget
        );
    }

    #[test]
    fn incomplete_coverage_and_cancellation_cannot_report_clear() {
        let c = Clearance::new(&[], 7., Coverage::Incomplete).unwrap();
        let mut budget = Budget::new(10).unwrap();
        assert_eq!(
            c.check([0.; 3], &mut budget, || false).unwrap().verdict,
            Verdict::Incomplete
        );
        assert_eq!(
            c.check([0.; 3], &mut budget, || true).unwrap().verdict,
            Verdict::Cancelled
        );
        assert_eq!(budget.used(), 0);
        let complete = Clearance::new(&[], 7., Coverage::Complete).unwrap();
        assert_eq!(verdict(&complete, [0.; 3]), Verdict::Clear);
        let far = Box3 {
            center: [900000., 0., 0.],
            ..shape()
        };
        let many = Clearance::new(&[(key(1), far), (key(2), far)], 7., Coverage::Complete).unwrap();
        let calls = std::cell::Cell::new(0);
        let result = many
            .check([0.; 3], &mut budget, || {
                calls.set(calls.get() + 1);
                calls.get() == 3
            })
            .unwrap();
        assert_eq!(result.verdict, Verdict::Cancelled);
        assert_eq!((result.work, budget.used()), (1, 1));
    }

    #[test]
    fn invalid_geometry_keys_padding_and_queries_are_rejected() {
        for padding in [f32::NAN, f32::INFINITY, -1., 10001.] {
            assert!(Clearance::new(&[], padding, Coverage::Complete).is_err());
        }
        assert!(
            Clearance::new(
                &[(key(1), shape()), (key(1), shape())],
                7.,
                Coverage::Complete
            )
            .is_err()
        );
        for bad in [
            Box3 {
                extents: [-1., 2., 3.],
                ..shape()
            },
            Box3 {
                rotation: [0.; 4],
                ..shape()
            },
            Box3 {
                center: [f32::NAN; 3],
                ..shape()
            },
        ] {
            assert!(Clearance::new(&[(key(1), bad)], 7., Coverage::Complete).is_err());
        }
        let c = Clearance::new(&[], 7., Coverage::Complete).unwrap();
        let mut budget = Budget::new(10).unwrap();
        for position in [[f32::NAN; 3], [f32::INFINITY; 3], [1_000_001.; 3]] {
            assert!(c.check(position, &mut budget, || false).is_err());
        }
        assert_eq!(budget.used(), 0);
    }
}
