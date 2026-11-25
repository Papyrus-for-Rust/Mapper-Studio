//! Integer coverage used by the inspected terrain-map quad callbacks.
//! World transforms and world-to-index conversion are separate operations.

pub const VERSION: &str = "papyrus-source-stamp-raster-1";
pub const MAX_RESOLUTION: usize = 16_385;

/// The inspected TerrainMap.ApplyFilter brush, clipped through its normalized
/// index bounds while retaining the original centre and radial weights.
#[derive(Clone, Copy, Debug)]
pub struct Circle {
    center: [f32; 2],
    inner: f32,
    outer: f32,
    bounds: [usize; 4],
}
impl Circle {
    pub fn from_world(
        resolution: usize,
        size: f32,
        position: [f32; 2],
        radius: f32,
        fade: f32,
    ) -> Result<Self, String> {
        if !(1..=MAX_RESOLUTION).contains(&resolution)
            || !size.is_finite()
            || size <= 0.
            || position.iter().any(|v| !v.is_finite())
            || !radius.is_finite()
            || radius < 0.
            || !fade.is_finite()
            || fade < 0.
        {
            return Err("Invalid source circle brush".into());
        }
        let inverse = 1. / size;
        let normalized = position.map(|v| (v + size * 0.5) * inverse);
        let r = inverse * radius;
        let f = inverse * fade;
        let n = resolution as f32;
        let index = |v: f32| -> Result<usize, String> {
            let scaled = v * n;
            if !scaled.is_finite() || !(-2147483648_f32..2147483648_f32).contains(&scaled) {
                return Err("Source circle index exceeds supported arithmetic".into());
            }
            Ok((scaled as i32).clamp(0, resolution as i32 - 1) as usize)
        };
        let center = normalized.map(|v| v * n);
        let inner = n * (r - f);
        let outer = n * r;
        if center.iter().any(|v| !v.is_finite()) || !inner.is_finite() || !outer.is_finite() {
            return Err("Source circle extent exceeds supported arithmetic".into());
        }
        Ok(Self {
            center,
            inner,
            outer,
            bounds: [
                index(normalized[0] - r)?,
                index(normalized[0] + r)? + 1,
                index(normalized[1] - r)?,
                index(normalized[1] + r)? + 1,
            ],
        })
    }
    pub fn bounds(self) -> [usize; 4] {
        self.bounds
    }
    pub fn weight(self, x: usize, z: usize) -> f32 {
        let [x0, x1, z0, z1] = self.bounds;
        if x < x0 || x >= x1 || z < z0 || z >= z1 {
            return 0.;
        }
        let dx = x as f32 + 0.5 - self.center[0];
        let dz = z as f32 + 0.5 - self.center[1];
        let distance = (dx * dx + dz * dz).sqrt();
        if self.inner == self.outer {
            if distance < self.outer { 1. } else { 0. }
        } else {
            crate::path_terrain::inverse(self.outer, self.inner, distance)
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Quad {
    vertices: [[i32; 2]; 4],
    bounds: [usize; 4],
}

#[derive(Clone, Copy)]
pub(crate) struct QuadRow {
    spans: [(usize, usize); 2],
}

impl QuadRow {
    pub(crate) fn full(start: usize, end: usize) -> Self {
        Self {
            spans: [(start, end), (0, 0)],
        }
    }

    pub(crate) fn indices(self) -> impl Iterator<Item = usize> {
        let [(a, b), (c, d)] = self.spans;
        (a..b).chain(c..d)
    }
}
impl Quad {
    /// Normalize and truncate transformed world corners using the inspected
    /// terrain-map arithmetic, then clamp each vertex before rasterization.
    pub fn from_world(
        resolution: usize,
        origin: [f32; 2],
        inverse_size: [f32; 2],
        world: [[f32; 2]; 4],
    ) -> Result<Self, String> {
        if !(1..=MAX_RESOLUTION).contains(&resolution)
            || origin.iter().any(|v| !v.is_finite())
            || inverse_size.iter().any(|v| !v.is_finite() || *v <= 0.)
        {
            return Err("Invalid source stamp world grid".into());
        }
        let mut vertices = [[0; 2]; 4];
        for (point, vertex) in world.into_iter().zip(&mut vertices) {
            for axis in 0..2 {
                let normalized = (point[axis] - origin[axis]) * inverse_size[axis];
                let scaled = normalized * resolution as f32;
                if !scaled.is_finite() || !(-2147483648_f32..2147483648_f32).contains(&scaled) {
                    return Err("Source stamp world index exceeds supported arithmetic".into());
                }
                vertex[axis] = (scaled as i32).clamp(0, resolution as i32 - 1);
            }
        }
        Self::new(resolution, vertices)
    }
    pub fn vertices(&self) -> [[i32; 2]; 4] {
        self.vertices
    }
    /// Vertices use the game's two-row order: [bottom-left, bottom-right,
    /// top-left, top-right], transformed without reordering. Winding matters.
    /// Inputs must already have passed the terrain map's index clamping.
    pub fn new(resolution: usize, vertices: [[i32; 2]; 4]) -> Result<Self, String> {
        if !(1..=MAX_RESOLUTION).contains(&resolution)
            || vertices
                .iter()
                .flatten()
                .any(|&v| v < 0 || v as usize >= resolution)
        {
            return Err("Invalid source stamp raster coordinates".into());
        }
        let mut bounds = [0; 4];
        for axis in 0..2 {
            bounds[axis * 2] = vertices.iter().map(|p| p[axis] as usize).min().unwrap();
            bounds[axis * 2 + 1] = vertices.iter().map(|p| p[axis] as usize).max().unwrap() + 1;
        }
        Ok(Self { vertices, bounds })
    }
    /// Half-open [x_min, x_max, z_min, z_max]; includes the source's last row.
    pub fn bounds(&self) -> [usize; 4] {
        self.bounds
    }

    /// Integer lattice point coverage, including shared edges. The union means
    /// the shared diagonal is visited once. Do not substitute cell centers or
    /// normalize winding: both change source coverage, including degeneracies.
    pub fn contains(&self, x: usize, z: usize) -> bool {
        let [a, b, c, d] = self.bounds;
        if x < a || x >= b || z < c || z >= d {
            return false;
        }
        let [v0, v1, v2, v3] = self.vertices;
        let p = [x as i32, z as i32];
        triangle([v0, v1, v2], p) || triangle([v2, v1, v3], p)
    }

    /// Intersect the same six integer half-planes as point coverage with one
    /// scanline. Keep both spans when the two triangles are disjoint on this row.
    pub(crate) fn row(&self, z: usize) -> QuadRow {
        let [lo, hi, min_z, max_z] = self.bounds;
        if !(min_z..max_z).contains(&z) {
            return QuadRow::full(0, 0);
        }
        let [v0, v1, v2, v3] = self.vertices;
        let span = |vertices: [[i32; 2]; 3]| {
            let (mut first, mut end) = (lo as i32, hi as i32);
            let [a, b, c] = vertices;
            for (a, b) in [(a, b), (b, c), (c, a)] {
                let dz = b[1] - a[1];
                let edge_at_zero = (b[0] - a[0]) * (z as i32 - a[1]) + dz * a[0];
                // With the validated coordinate range the edge arithmetic fits i32.
                match dz.cmp(&0) {
                    std::cmp::Ordering::Less => first = first.max(-edge_at_zero.div_euclid(-dz)),
                    std::cmp::Ordering::Greater => end = end.min(edge_at_zero.div_euclid(dz) + 1),
                    std::cmp::Ordering::Equal if edge_at_zero < 0 => return (0, 0),
                    std::cmp::Ordering::Equal => {}
                }
            }
            if first < end {
                (first as usize, end as usize)
            } else {
                (0, 0)
            }
        };
        let mut spans = [span([v0, v1, v2]), span([v2, v1, v3])];
        if spans[0].0 > spans[1].0 {
            spans.swap(0, 1);
        }
        if spans[1].0 <= spans[0].1 {
            return QuadRow::full(spans[0].0, spans[0].1.max(spans[1].1));
        }
        QuadRow { spans }
    }
}
fn triangle(vertices: [[i32; 2]; 3], p: [i32; 2]) -> bool {
    (0..3).all(|i| {
        let a = vertices[i];
        let b = vertices[(i + 1) % 3];
        // Wider intermediates avoid overflow. Validated coordinates also stay
        // inside the inspected implementation's signed 32-bit arithmetic.
        (i64::from(b[0]) - i64::from(a[0])) * (i64::from(p[1]) - i64::from(a[1]))
            - (i64::from(b[1]) - i64::from(a[1])) * (i64::from(p[0]) - i64::from(a[0]))
            >= 0
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scanlines_match_point_coverage_for_every_small_quad() {
        for mut corners in 0..9_usize.pow(4) {
            let vertices = std::array::from_fn(|_| {
                let point = corners % 9;
                corners /= 9;
                [(point % 3) as i32, (point / 3) as i32]
            });
            let quad = Quad::new(3, vertices).unwrap();
            for z in 0..4 {
                let expected = (0..4).filter(|&x| quad.contains(x, z));
                let row = quad.row(z);
                assert!(row.indices().eq(expected), "{vertices:?} at {z}");
            }
        }
    }

    #[test]
    fn circle_clips_source_indices_and_keeps_strict_edge_and_fade_weights() {
        let circle = Circle::from_world(16, 16., [0.5, 0.5], 1., 0.).unwrap();
        assert_eq!(circle.weight(8, 8), 1.);
        assert_eq!(circle.weight(9, 8), 0.);
        assert_eq!(circle.weight(usize::MAX, 8), 0.);
        let faded = Circle::from_world(16, 16., [0.5, 0.5], 1., 2.).unwrap();
        assert_eq!(faded.weight(8, 8), 0.5);
        let edge = Circle::from_world(100, 1000., [510., 0.], 20., 0.).unwrap();
        assert_eq!(edge.bounds()[1], 100);
        assert!(edge.weight(99, 50) > 0.);
        let point = f32::from_bits((-250_f32).to_bits() + 1);
        assert_eq!(
            Circle::from_world(128, 1000., [point, 0.], 0., 0.)
                .unwrap()
                .bounds(),
            [32, 33, 64, 65]
        );
        assert!(Circle::from_world(0, 1000., [0.; 2], 1., 0.).is_err());
        assert!(Circle::from_world(16, 0., [0.; 2], 1., 0.).is_err());
        assert!(Circle::from_world(16, 1000., [f32::INFINITY, 0.], 1., 0.).is_err());
    }
    #[test]
    fn world_corners_clamp_before_coverage_and_reject_undefined_indices() {
        let world = [[-600., -600.], [600., -600.], [-600., 600.], [600., 600.]];
        let quad = Quad::from_world(5, [-500.; 2], [0.001; 2], world).unwrap();
        assert_eq!(quad.vertices(), [[0, 0], [4, 0], [0, 4], [4, 4]]);
        for (origin, inverse, points) in [
            ([f32::NAN, 0.], [0.001; 2], world),
            ([-500.; 2], [0.; 2], world),
            ([-500.; 2], [0.001; 2], [[f32::INFINITY; 2]; 4]),
            ([-500.; 2], [1.; 2], [[2147483648.; 2]; 4]),
        ] {
            assert!(Quad::from_world(5, origin, inverse, points).is_err());
        }
    }
    #[test]
    fn includes_last_row_and_visits_shared_diagonal_once() {
        let q = Quad::new(5, [[0, 0], [4, 0], [0, 4], [4, 4]]).unwrap();
        assert_eq!(q.bounds(), [0, 5, 0, 5]);
        assert_eq!(
            (0..5)
                .flat_map(|z| (0..5).map(move |x| (x, z)))
                .filter(|&(x, z)| q.contains(x, z))
                .count(),
            25
        );
        assert!(!q.contains(5, 4));
        assert!(!q.contains(usize::MAX, usize::MAX));
    }
    #[test]
    fn rotation_winding_and_collapsed_edges_are_not_reinterpreted() {
        let q = Quad::new(5, [[2, 0], [4, 2], [0, 2], [2, 4]]).unwrap();
        assert!(q.contains(2, 0));
        assert!(q.contains(0, 2));
        assert!(q.contains(2, 2));
        assert!(!q.contains(0, 0));
        let reversed = Quad::new(5, [[4, 0], [0, 0], [4, 4], [0, 4]]).unwrap();
        assert!(!reversed.contains(2, 2));
        let point = Quad::new(5, [[2, 2]; 4]).unwrap();
        assert!(point.contains(2, 2));
        assert!(!point.contains(2, 3));
        let line = Quad::new(5, [[0, 0], [4, 4], [0, 0], [4, 4]]).unwrap();
        assert!(line.contains(1, 1));
        assert!(!line.contains(1, 2));
    }
    #[test]
    fn rejects_unclamped_vertices_and_unbounded_grids() {
        for (n, v) in [
            (0, [[0, 0]; 4]),
            (5, [[-1, 0]; 4]),
            (5, [[5, 0]; 4]),
            (MAX_RESOLUTION + 1, [[0, 0]; 4]),
        ] {
            assert!(Quad::new(n, v).is_err());
        }
        let n = MAX_RESOLUTION as i32 - 1;
        let q = Quad::new(MAX_RESOLUTION, [[0, 0], [n, 0], [0, n], [n, n]]).unwrap();
        assert!(q.contains(n as usize, n as usize));
        assert!(q.contains(8192, 8192));
    }
}
