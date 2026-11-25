//! Yaw-oriented, pivot-offset bounds for the top-down placement policy.
//! Projected bounds are conservative in Y; this is not a 3D collider test.
#[derive(Clone, Copy, Debug)]
pub struct Footprint {
    pub center: [f32; 2],
    pub extent: [f32; 2],
}
impl Footprint {
    pub fn valid(&self) -> bool {
        self.center
            .iter()
            .chain(&self.extent)
            .all(|v| v.is_finite() && v.abs() <= 10000.)
            && self.extent.iter().all(|v| *v >= 0.)
    }
    pub fn at(&self, position: [f32; 2], yaw: f32) -> Bounds {
        let (s, c) = f64::from(yaw).to_radians().sin_cos();
        Bounds {
            center: [
                f64::from(position[0]) + c * f64::from(self.center[0])
                    - s * f64::from(self.center[1]),
                f64::from(position[1])
                    + s * f64::from(self.center[0])
                    + c * f64::from(self.center[1]),
            ],
            axes: [[c, s], [-s, c]],
            extent: self.extent.map(f64::from),
        }
    }
}
#[derive(Clone, Copy)]
pub struct Bounds {
    center: [f64; 2],
    axes: [[f64; 2]; 2],
    extent: [f64; 2],
}
fn dot(a: [f64; 2], b: [f64; 2]) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}
impl Bounds {
    pub(crate) fn axis_aligned(self) -> [[f64; 2]; 2] {
        std::array::from_fn(|i| {
            let radius =
                self.axes[0][i].abs() * self.extent[0] + self.axes[1][i].abs() * self.extent[1];
            [self.center[i] - radius, self.center[i] + radius]
        })
    }

    /// Exact planar rectangle distance, including touching and degenerate boxes.
    /// Used with a vertical interval gap for upright source OBB spacing.
    pub fn squared_distance(&self, other: &Self) -> f64 {
        let delta = [
            other.center[0] - self.center[0],
            other.center[1] - self.center[1],
        ];
        let overlaps = self.axes.iter().chain(&other.axes).all(|&axis| {
            let radius = |b: &Self| {
                dot(b.axes[0], axis).abs() * b.extent[0] + dot(b.axes[1], axis).abs() * b.extent[1]
            };
            dot(delta, axis).abs() <= radius(self) + radius(other)
        });
        if overlaps {
            return 0.;
        }
        let corners = |b: &Self| {
            std::array::from_fn::<_, 4, _>(|i| {
                let x = if i & 1 == 0 { -1. } else { 1. };
                let z = if i & 2 == 0 { -1. } else { 1. };
                [
                    b.center[0] + x * b.axes[0][0] * b.extent[0] + z * b.axes[1][0] * b.extent[1],
                    b.center[1] + x * b.axes[0][1] * b.extent[0] + z * b.axes[1][1] * b.extent[1],
                ]
            })
        };
        let to_box = |p: [f64; 2], b: &Self| {
            let v = [p[0] - b.center[0], p[1] - b.center[1]];
            let x = (dot(v, b.axes[0]).abs() - b.extent[0]).max(0.);
            let z = (dot(v, b.axes[1]).abs() - b.extent[1]).max(0.);
            x * x + z * z
        };
        corners(self)
            .into_iter()
            .map(|p| to_box(p, other))
            .chain(corners(other).into_iter().map(|p| to_box(p, self)))
            .fold(f64::INFINITY, f64::min)
    }
    pub fn inside_world(&self, size: f32) -> bool {
        (0..2).all(|i| {
            self.center[i].abs()
                + self.axes[0][i].abs() * self.extent[0]
                + self.axes[1][i].abs() * self.extent[1]
                <= f64::from(size) * 0.5
        })
    }
    pub fn intersects(&self, other: &Self) -> bool {
        let delta = [
            other.center[0] - self.center[0],
            other.center[1] - self.center[1],
        ];
        self.axes.iter().chain(&other.axes).all(|&axis| {
            let radius = |b: &Self| {
                dot(b.axes[0], axis).abs() * b.extent[0] + dot(b.axes[1], axis).abs() * b.extent[1]
            };
            dot(delta, axis).abs() < radius(self) + radius(other) - 1e-8
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn elongated_bounds_allow_sites_rejected_by_circles_and_respect_rotation() {
        let f = Footprint {
            center: [0., 0.],
            extent: [100., 10.],
        };
        assert!(!f.at([0., 0.], 0.).intersects(&f.at([0., 30.], 0.)));
        assert!(f.at([0., 0.], 0.).intersects(&f.at([0., 30.], 90.)));
        for yaw in [0., 17., 90., 137., 270.] {
            let a = f.at([-25., 35.], yaw);
            let b = f.at([-25., 35.], yaw + 180.);
            assert!(a.intersects(&b));
            assert!(b.intersects(&a));
        }
    }
    #[test]
    fn authored_center_rotates_about_pivot_and_world_edges_are_checked() {
        let f = Footprint {
            center: [100., 0.],
            extent: [20., 10.],
        };
        assert!(f.at([400., 0.], 90.).inside_world(1000.));
        assert!(!f.at([400., 0.], 0.).inside_world(1000.));
        let near = Footprint {
            center: [0., 0.],
            extent: [5., 5.],
        };
        assert!(f.at([0., 0.], 90.).intersects(&near.at([0., 100.], 0.)));
        assert!(!f.at([0., 0.], 90.).intersects(&near.at([100., 0.], 0.)));
        assert!(
            !Footprint {
                center: [f32::NAN, 0.],
                extent: [1., 1.]
            }
            .valid()
        );
        assert!(
            !Footprint {
                center: [0., 0.],
                extent: [-1., 1.]
            }
            .valid()
        );
    }
}
