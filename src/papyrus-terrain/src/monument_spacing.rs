//! Upright, unit-scale OBB spacing for independently generated placements.
//! This is analytic box distance, not the game's Burst GJK implementation.
use crate::monument_bounds::{Bounds, Footprint};
#[derive(Clone, Copy, Debug)]
pub struct BoxShape {
    pub center: [f32; 3],
    pub extent: [f32; 3],
}
#[derive(Clone, Copy, Debug)]
pub struct BoxPose {
    pub shape: BoxShape,
    pub position: [f32; 3],
    pub yaw: f32,
}
impl BoxShape {
    pub fn valid(&self) -> bool {
        self.center
            .iter()
            .chain(&self.extent)
            .all(|v| v.is_finite() && v.abs() <= 10000.)
            && self.extent.iter().all(|v| *v >= 0.)
    }
}
impl BoxPose {
    pub fn valid(&self) -> bool {
        self.shape.valid()
            && self
                .position
                .iter()
                .all(|v| v.is_finite() && v.abs() <= 20000.)
            && self.yaw.is_finite()
    }
    pub fn squared_distance(self, other: Self) -> Result<f64, String> {
        if !self.valid() || !other.valid() {
            return Err("Invalid monument spacing bounds".into());
        }
        let vertical = vertical_gap_squared(
            self.vertical_center(),
            f64::from(self.shape.extent[1]),
            other.vertical_center(),
            f64::from(other.shape.extent[1]),
        );
        Ok(self.horizontal().squared_distance(&other.horizontal()) + vertical)
    }

    fn horizontal(self) -> Bounds {
        Footprint {
            center: [self.shape.center[0], self.shape.center[2]],
            extent: [self.shape.extent[0], self.shape.extent[2]],
        }
        .at([self.position[0], self.position[2]], self.yaw)
    }

    fn vertical_center(self) -> f64 {
        f64::from(self.position[1]) + f64::from(self.shape.center[1])
    }

    pub(crate) fn prepare(self) -> PreparedBox {
        let horizontal = self.horizontal();
        PreparedBox {
            horizontal,
            axis_aligned: horizontal.axis_aligned(),
            y: self.vertical_center(),
            extent_y: f64::from(self.shape.extent[1]),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct PreparedBox {
    horizontal: Bounds,
    axis_aligned: [[f64; 2]; 2],
    y: f64,
    extent_y: f64,
}

impl PreparedBox {
    pub(crate) fn with_height(mut self, position_y: f32, offset_y: f32) -> Self {
        self.y = f64::from(position_y) + f64::from(offset_y);
        self
    }

    pub(crate) fn squared_distance(&self, other: &Self, best: Option<f64>) -> Option<f64> {
        let vertical_squared = vertical_gap_squared(self.y, self.extent_y, other.y, other.extent_y);
        if let Some(best) = best {
            let gap = |i: usize| {
                let [a, b] = self.axis_aligned[i];
                let [c, d] = other.axis_aligned[i];
                // Allow for rounding in rotated bounds and point projections.
                ((a - d).max(c - b) - 1e-7).max(0.)
            };
            let (x, z) = (gap(0), gap(1));
            if x * x + z * z + vertical_squared >= best {
                return None;
            }
        }
        Some(self.horizontal.squared_distance(&other.horizontal) + vertical_squared)
    }
}

fn vertical_gap_squared(a: f64, a_extent: f64, b: f64, b_extent: f64) -> f64 {
    let gap = ((a - b).abs() - a_extent - b_extent).max(0.);
    gap * gap
}
#[cfg(test)]
mod tests {
    use super::*;
    fn pose(p: [f32; 3], e: [f32; 3], yaw: f32) -> BoxPose {
        BoxPose {
            shape: BoxShape {
                center: [0.; 3],
                extent: e,
            },
            position: p,
            yaw,
        }
    }

    #[test]
    fn prepared_spacing_rejects_only_boxes_that_cannot_improve_the_result() {
        let mut boxes: Vec<_> = (0..256)
            .map(|i| {
                let mut b = pose(
                    [
                        (i * 911 % 4219) as f32 - 2109.,
                        (i * 39 % 213) as f32,
                        (i * 713 % 3643) as f32 - 1821.,
                    ],
                    [(i % 123) as f32, (i % 37) as f32, (i % 63) as f32],
                    (i as f32 * 35.71).rem_euclid(360.),
                );
                b.shape.center = [16., -2., -9.];
                b
            })
            .collect();
        boxes.extend([
            pose([10000., 0., -20000.], [0.; 3], 70.),
            pose([10000.01, 0., -20000.], [0.; 3], 139.),
        ]);
        let prepared: Vec<_> = boxes.iter().map(|b| b.prepare()).collect();
        let mut skipped = 0;
        for source in &boxes {
            // Repeat with a fitted height, as selection does after anchoring.
            let y = source.position[1] + 0.037;
            let query = source.prepare().with_height(y, source.shape.center[1]);
            let mut fitted = *source;
            fitted.position[1] = y;
            let mut best = None;
            for (b, cached) in boxes.iter().zip(&prepared) {
                let exact = fitted.squared_distance(*b).unwrap();
                if let Some(distance) = query.squared_distance(cached, best) {
                    assert_eq!(distance, exact);
                    best = Some(best.map_or(distance, |old: f64| old.min(distance)));
                } else {
                    skipped += 1;
                    assert!(best.unwrap() <= exact);
                }
            }
        }
        assert!(skipped > boxes.len() * boxes.len() / 2);
    }
    #[test]
    fn bounds_gap_uses_edges_height_offsets_and_rotation_instead_of_pivots() {
        let a = pose([0.; 3], [50., 10., 10.], 0.);
        assert_eq!(
            a.squared_distance(pose([150., 0., 0.], [50., 10., 10.], 0.))
                .unwrap(),
            2500.
        );
        assert_eq!(
            a.squared_distance(pose([150., 50., 0.], [50., 10., 10.], 0.))
                .unwrap(),
            3400.
        );
        assert!(
            a.squared_distance(pose([0., 0., 30.], [50., 10., 10.], 90.))
                .unwrap()
                < 1e-12
        );
        let mut offset = a;
        offset.shape.center = [100., 20., 0.];
        assert!(
            offset
                .squared_distance(pose([0., 20., 100.], [10.; 3], 0.))
                .unwrap()
                > 0.
        );
        offset.yaw = 90.;
        assert!(
            offset
                .squared_distance(pose([0., 20., 100.], [10.; 3], 0.))
                .unwrap()
                < 1e-12
        );
    }
    #[test]
    fn touching_zero_width_crossings_symmetry_and_invalid_values_are_handled() {
        let a = pose([0.; 3], [100., 0., 0.], 0.);
        let b = pose([0.; 3], [100., 0., 0.], 90.);
        assert_eq!(a.squared_distance(b).unwrap(), 0.);
        let a = pose([0.; 3], [20., 30., 5.], 37.);
        let b = pose([200., 70., 120.], [7., 10., 30.], 137.);
        assert!((a.squared_distance(b).unwrap() - b.squared_distance(a).unwrap()).abs() < 1e-10);
        let mut bad = b;
        bad.shape.extent[1] = -1.;
        assert!(a.squared_distance(bad).is_err());
    }
}
