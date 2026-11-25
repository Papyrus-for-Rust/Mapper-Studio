//! Experimental base-restored alpha composition; not a game export adapter.
//! Byte 255 is intact terrain, 0 is a hole. All images are top-down row-major.
//! Positive yaw rotates local +X toward world +Z, matching the studio preview.
//! A Unity transform adapter must negate yaw before using this convention.

/// Single-channel texture; alpha and blend can have different resolutions.
#[derive(Clone, Copy)]
pub struct Plane<'a> {
    pub width: usize,
    pub height: usize,
    pub bytes: &'a [u8],
}

impl Plane<'_> {
    fn valid(self) -> bool {
        (1..=4096).contains(&self.width)
            && (1..=4096).contains(&self.height)
            && self.bytes.len() == self.width * self.height
    }

    fn sample(self, u: f64, v: f64) -> f64 {
        let x = u.clamp(0., 1.) * (self.width - 1) as f64;
        let y = v.clamp(0., 1.) * (self.height - 1) as f64;
        let x0 = x.floor() as usize;
        let y0 = y.floor() as usize;
        let x1 = (x0 + 1).min(self.width - 1);
        let y1 = (y0 + 1).min(self.height - 1);
        let at = |x, y| f64::from(self.bytes[y * self.width + x]);
        let top = at(x0, y0) * (1. - x.fract()) + at(x1, y0) * x.fract();
        let bottom = at(x0, y1) * (1. - x.fract()) + at(x1, y1) * x.fract();
        top * (1. - y.fract()) + bottom * y.fract()
    }
}

pub struct Stamp<'a> {
    pub position: [f64; 2],
    pub yaw_degrees: f64,
    /// Full horizontal dimensions in metres, before rotation.
    pub size: [f64; 2],
    /// Local offset rotates with the entire modifier.
    pub offset: [f64; 2],
    pub alpha: Plane<'a>,
    pub blend: Plane<'a>,
}

struct Prepared<'a> {
    stamp: &'a Stamp<'a>,
    center: [f64; 2],
    sin: f64,
    cos: f64,
    bounds: [usize; 4],
}

/// Replay the complete ordered layout onto an immutable base. Moving/deleting a
/// stamp restores its former hole, including when other stamps overlap it.
/// Destination samples are cell centres; source samples span texture endpoints.
/// Cancellation discards the private result and never mutates the base.
pub fn compose(
    base: Plane<'_>,
    world_size: [f64; 2],
    stamps: &[Stamp<'_>],
    cancelled: impl Fn() -> bool,
) -> Result<Option<Vec<u8>>, String> {
    if !base.valid()
        || world_size
            .iter()
            .any(|v| !v.is_finite() || !(1. ..=65536.).contains(v))
        || stamps.len() > 4096
    {
        return Err("Invalid alpha base dimensions or layout count".into());
    }
    if cancelled() {
        return Ok(None);
    }
    let mut prepared = Vec::with_capacity(stamps.len());
    let mut work = 0_u64;
    for stamp in stamps {
        if !stamp.alpha.valid()
            || !stamp.blend.valid()
            || !stamp.yaw_degrees.is_finite()
            || stamp
                .position
                .iter()
                .chain(&stamp.offset)
                .any(|v| !v.is_finite() || v.abs() > 65536.)
            || stamp
                .size
                .iter()
                .any(|v| !v.is_finite() || !(0.1..=8192.).contains(v))
        {
            return Err("Invalid alpha stamp texture or transform".into());
        }
        let (sin, cos) = stamp.yaw_degrees.rem_euclid(360.).to_radians().sin_cos();
        let center = [
            stamp.position[0] + cos * stamp.offset[0] - sin * stamp.offset[1],
            stamp.position[1] + sin * stamp.offset[0] + cos * stamp.offset[1],
        ];
        let half = [
            (cos.abs() * stamp.size[0] + sin.abs() * stamp.size[1]) * 0.5,
            (sin.abs() * stamp.size[0] + cos.abs() * stamp.size[1]) * 0.5,
        ];
        let mut bounds = [0; 4];
        for axis in 0..2 {
            let count = [base.width, base.height][axis];
            // Convert world Z to top-down image rows.
            let c = if axis == 0 {
                center[axis]
            } else {
                -center[axis]
            };
            let grid = |p: f64| (p / world_size[axis] + 0.5) * count as f64 - 0.5;
            // Include exact boundary samples despite trig/coordinate roundoff.
            bounds[axis * 2] =
                (grid(c - half[axis]) - 1e-9).ceil().clamp(0., count as f64) as usize;
            bounds[axis * 2 + 1] =
                ((grid(c + half[axis]) + 1e-9).floor() + 1.).clamp(0., count as f64) as usize;
        }
        work += (bounds[1].saturating_sub(bounds[0]) * bounds[3].saturating_sub(bounds[2])) as u64;
        if work > 128_000_000 {
            return Err("Alpha composition exceeds the per-request sample budget".into());
        }
        prepared.push(Prepared {
            stamp,
            center,
            sin,
            cos,
            bounds,
        });
    }
    let mut output = base.bytes.to_vec();
    for p in prepared {
        for y in p.bounds[2]..p.bounds[3] {
            if cancelled() {
                return Ok(None);
            }
            let z = (0.5 - (y as f64 + 0.5) / base.height as f64) * world_size[1];
            for x in p.bounds[0]..p.bounds[1] {
                let wx = ((x as f64 + 0.5) / base.width as f64 - 0.5) * world_size[0];
                let dx = wx - p.center[0];
                let dz = z - p.center[1];
                let u = (p.cos * dx + p.sin * dz) / p.stamp.size[0] + 0.5;
                let v = 0.5 - (-p.sin * dx + p.cos * dz) / p.stamp.size[1];
                if !(-1e-12..=1. + 1e-12).contains(&u) || !(-1e-12..=1. + 1e-12).contains(&v) {
                    continue;
                }
                let opacity = p.stamp.blend.sample(u, v) / 255.;
                let value = &mut output[y * base.width + x];
                *value = (f64::from(*value) * (1. - opacity) + p.stamp.alpha.sample(u, v) * opacity)
                    .round_ties_even() as u8;
            }
        }
    }
    Ok((!cancelled()).then_some(output))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn plane(bytes: &[u8], width: usize) -> Plane<'_> {
        Plane {
            width,
            height: bytes.len() / width,
            bytes,
        }
    }
    fn stamp(yaw: f64) -> Stamp<'static> {
        Stamp {
            position: [0., 0.],
            yaw_degrees: yaw,
            size: [6., 2.],
            offset: [2., 0.],
            alpha: plane(&[0], 1),
            blend: plane(&[255], 1),
        }
    }
    fn render(stamps: &[Stamp<'_>]) -> Vec<u8> {
        compose(plane(&[255; 441], 21), [21., 21.], stamps, || false)
            .unwrap()
            .unwrap()
    }
    #[test]
    fn offset_and_rectangle_rotate_together() {
        let zero = render(&[stamp(0.)]);
        let ninety = render(&[stamp(90.)]);
        // Local centre (2,0) rotates to (0,2); the long axis becomes vertical.
        assert_eq!(zero[10 * 21 + 14], 0);
        assert_eq!(ninety[6 * 21 + 10], 0);
        assert_eq!(ninety[10 * 21 + 14], 255);
        assert_eq!(render(&[stamp(360.)]), zero);
        assert_eq!(render(&[stamp(-270.)]), ninety);
    }
    #[test]
    fn arbitrary_angles_keep_the_hole_and_restore_on_move_or_delete() {
        for yaw in [15., 37., 45., 89., 135., 217., 333.] {
            let mut s = stamp(yaw);
            s.offset = [0., 0.];
            let first = render(&[s]);
            assert_eq!(first[220], 0, "angle {yaw}");
            let mut moved = stamp(yaw);
            moved.position = [8., 8.];
            moved.offset = [0., 0.];
            assert_eq!(render(&[moved])[220], 255);
        }
        assert_eq!(render(&[]), vec![255; 441]);
    }
    #[test]
    fn asymmetric_texture_has_known_orientation() {
        let mut s = stamp(90.);
        s.size = [4., 4.];
        s.offset = [0., 0.];
        s.alpha = plane(&[0, 255, 255, 255], 2);
        let result = render(&[s]);
        // Local top-left (-2,+2) rotates to world (-2,-2).
        assert_eq!(result[12 * 21 + 8], 0);
        assert_eq!(result[8 * 21 + 12], 255);
    }
    #[test]
    fn oblique_asymmetric_corner_uses_the_full_rotation() {
        let mut s = stamp(0.6_f64.atan2(0.8).to_degrees());
        s.size = [4., 4.];
        s.offset = [0., 0.];
        s.alpha = plane(&[0, 255, 255, 255], 2);
        let base = vec![255; 101 * 101];
        let image = compose(plane(&base, 101), [20.2, 20.2], &[s], || false)
            .unwrap()
            .unwrap();
        // 3-4-5 triangle rotation maps (-2,+2) to (-2.8,+0.4).
        // On this grid the transformed corner is column 36, row 48.
        assert!(image[48 * 101 + 36] <= 1);
        assert_eq!(image[52 * 101 + 64], 255);
    }
    #[test]
    fn rectangular_base_uses_each_world_axis_independently() {
        let mut s = stamp(0.);
        s.position = [-2., 1.];
        s.offset = [0., 0.];
        s.size = [1., 1.];
        let image = compose(plane(&[255; 15], 5), [5., 3.], &[s], || false)
            .unwrap()
            .unwrap();
        assert_eq!(image[0], 0);
        assert_eq!(image.iter().filter(|&&v| v == 0).count(), 1);
    }
    #[test]
    fn independent_dimensions_and_blending_preserve_order() {
        let mut s = stamp(0.);
        s.alpha = plane(&[0; 6], 3);
        s.blend = plane(&[128; 4], 2);
        assert_eq!(render(&[s])[222], 127);
        let mut solid = stamp(0.);
        solid.alpha = plane(&[255], 1);
        assert_eq!(render(&[stamp(0.), solid])[222], 255);
    }
    #[test]
    fn overlap_is_replayed_from_base() {
        let mut moved = stamp(37.);
        moved.position = [8., 8.];
        assert_eq!(render(&[stamp(0.), moved])[222], 0);
        assert_eq!(render(&[])[222], 255);
    }
    #[test]
    fn clipping_does_not_clamp_outside_stamps_onto_border() {
        let mut s = stamp(45.);
        s.position = [100., 100.];
        assert_eq!(render(&[s]), vec![255; 441]);
        let mut edge = stamp(37.);
        edge.position = [10., 10.];
        let result = render(&[edge]);
        assert!(result.contains(&0));
        assert_eq!(result[440], 255);
    }
    #[test]
    fn cancellation_never_changes_base() {
        let base = [255; 441];
        let calls = std::cell::Cell::new(0);
        let result = compose(plane(&base, 21), [21., 21.], &[stamp(37.)], || {
            calls.set(calls.get() + 1);
            calls.get() > 2
        })
        .unwrap();
        assert!(result.is_none());
        assert_eq!(base, [255; 441]);
    }
    #[test]
    fn invalid_and_excessive_work_are_rejected() {
        let mut s = stamp(f64::NAN);
        assert!(compose(plane(&[255], 1), [21., 21.], &[s], || false).is_err());
        s = stamp(0.);
        s.alpha.bytes = &[];
        assert!(compose(plane(&[255], 1), [21., 21.], &[s], || false).is_err());
        let bytes = vec![255; 4096 * 4096];
        let stamps: Vec<_> = (0..8).map(|_| stamp(0.)).collect();
        assert!(compose(plane(&bytes, 4096), [1., 1.], &stamps, || false).is_err());
    }
}
