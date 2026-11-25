//! Independent convex polytope model. No PhysX cooking or registration is implied.
use super::*;
use parry3d_f64::{math::Vector, transformation::try_convex_hull};
use std::sync::Arc;

const MAX_POINTS: usize = 256;

#[derive(Debug)]
struct Geometry {
    points: Vec<[f64; 3]>,
    faces: Vec<[u32; 3]>,
    normals: Vec<[f64; 3]>,
    edges: Vec<[f64; 3]>,
    bounds: Bounds,
}

#[derive(Clone, Debug)]
pub struct Convex(Arc<Geometry>);
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}
fn unit(v: [f64; 3]) -> Option<[f64; 3]> {
    let length = dot(v, v).sqrt();
    (length > 1e-12 && length.is_finite()).then(|| v.map(|v| v / length))
}
impl Convex {
    /// Hull construction has a small fixed input cap. Reserve its quadratic
    /// work before entering the dependency; check cancellation again afterward.
    /// Larger, planar and otherwise unsupported point sets fail explicitly.
    pub fn build(
        points: &[[f32; 3]],
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Result<Self, Verdict>, String> {
        if !(4..=MAX_POINTS).contains(&points.len())
            || points.iter().any(|p| !vector(p))
            || !(1..=128_000_000).contains(&maximum_work)
        {
            return Err("Unsupported environment convex hull input or allowance".into());
        }
        let mut meter = Meter {
            used: 0,
            limit: maximum_work,
            cancelled: &cancelled,
        };
        if let Err(v) = meter.spend(points.len() * points.len() * 16) {
            return Ok(Err(v));
        }
        let input = points
            .iter()
            .map(|p| Vector::new(p[0].into(), p[1].into(), p[2].into()))
            .collect::<Vec<_>>();
        let (vertices, faces) = try_convex_hull(&input)
            .map_err(|e| format!("Environment hull construction failed: {e:?}"))?;
        if let Err(v) = meter.spend(1) {
            return Ok(Err(v));
        }
        let hull = Self::assemble(
            vertices.into_iter().map(|p| [p.x, p.y, p.z]).collect(),
            faces,
        )?;
        // Detect a returned hull that excludes input vertices beyond roundoff.
        // The dependency is not allowed to silently truncate this source shape.
        let scale = (0..3)
            .map(|i| hull.0.bounds.maximum[i] - hull.0.bounds.minimum[i])
            .fold(1., f64::max);
        for (face, normal) in hull.0.faces.iter().zip(&hull.0.normals) {
            if let Err(v) = meter.spend(points.len()) {
                return Ok(Err(v));
            }
            let a = hull.0.points[face[0] as usize];
            if points
                .iter()
                .any(|p| dot(sub(p.map(f64::from), a), *normal) > scale * 1e-12)
            {
                return Err("Environment convex hull does not enclose its source vertices".into());
            }
        }
        Ok(Ok(hull))
    }
    fn assemble(points: Vec<[f64; 3]>, faces: Vec<[u32; 3]>) -> Result<Self, String> {
        if !(4..=MAX_POINTS).contains(&points.len())
            || !(4..=1024).contains(&faces.len())
            || points
                .iter()
                .flatten()
                .any(|v| !v.is_finite() || v.abs() > f64::from(LIMIT))
            || faces.iter().flatten().any(|&i| i as usize >= points.len())
        {
            return Err("Invalid environment convex hull geometry".into());
        }
        let center =
            std::array::from_fn(|i| points.iter().map(|p| p[i]).sum::<f64>() / points.len() as f64);
        let bounds = Bounds {
            minimum: std::array::from_fn(|i| {
                points.iter().map(|p| p[i]).fold(f64::INFINITY, f64::min)
            }),
            maximum: std::array::from_fn(|i| {
                points
                    .iter()
                    .map(|p| p[i])
                    .fold(f64::NEG_INFINITY, f64::max)
            }),
        };
        let scale = (0..3)
            .map(|i| bounds.maximum[i] - bounds.minimum[i])
            .fold(0., f64::max);
        let mut normals = Vec::with_capacity(faces.len());
        let mut edge_ids = BTreeSet::new();
        let mut volume = 0.;
        for face in &faces {
            let [a, b, c] = face.map(|i| points[i as usize]);
            let mut normal =
                unit(cross(sub(b, a), sub(c, a))).ok_or("Degenerate environment hull face")?;
            if dot(sub(center, a), normal) > 0. {
                normal = normal.map(|v| -v);
            }
            normals.push(normal);
            volume += dot(sub(a, center), cross(sub(b, center), sub(c, center))).abs() / 6.;
            for [a, b] in [[face[0], face[1]], [face[1], face[2]], [face[2], face[0]]] {
                edge_ids.insert([a.min(b), a.max(b)]);
            }
        }
        if !bounds.valid() || !volume.is_finite() || volume <= scale.powi(3) * 1e-12 {
            return Err("Environment convex hull has no supported enclosed volume".into());
        }
        let edges = edge_ids
            .into_iter()
            .map(|[a, b]| {
                unit(sub(points[b as usize], points[a as usize]))
                    .ok_or_else(|| "Degenerate environment hull edge".to_owned())
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self(Arc::new(Geometry {
            points,
            faces,
            normals,
            edges,
            bounds,
        })))
    }
    /// Apply the actual instance/child affine frame, never the source query's
    /// postmultiplied rotation. Geometry is rebuilt once per world occurrence.
    pub fn transformed(
        &self,
        matrix: [[f32; 4]; 4],
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        if matrix[3] != [0., 0., 0., 1.]
            || matrix
                .iter()
                .flatten()
                .any(|v| !v.is_finite() || v.abs() > LIMIT)
        {
            return Err("Invalid environment convex affine frame".into());
        }
        if cancelled() {
            return Ok(None);
        }
        let mut points = Vec::with_capacity(self.0.points.len());
        for p in &self.0.points {
            if cancelled() {
                return Ok(None);
            }
            points.push(std::array::from_fn(|i| {
                f64::from(matrix[i][3])
                    + (0..3).map(|j| f64::from(matrix[i][j]) * p[j]).sum::<f64>()
            }));
        }
        let result = Self::assemble(points, self.0.faces.clone())?;
        Ok((!cancelled()).then_some(result))
    }
    pub fn counts(&self) -> [usize; 2] {
        [self.0.points.len(), self.0.faces.len()]
    }
    pub fn bounds(&self) -> Bounds {
        self.0.bounds
    }
    pub(super) fn overlaps<F: Fn() -> bool>(
        &self,
        query: Box3,
        tolerance: f64,
        meter: &mut Meter<'_, F>,
    ) -> Result<bool, Verdict> {
        let axes = query.axes();
        let origin = query.center.map(f64::from);
        let separated = |axis, meter: &mut Meter<'_, F>| -> Result<bool, Verdict> {
            meter.spend(self.0.points.len() + 4)?;
            let Some(axis) = unit(axis) else {
                return Ok(false);
            };
            let radius = (0..3)
                .map(|i| f64::from(query.extents[i]) * dot(axes[i], axis).abs())
                .sum::<f64>();
            let mut minimum = f64::INFINITY;
            let mut maximum = f64::NEG_INFINITY;
            for p in &self.0.points {
                let projection = dot(sub(*p, origin), axis);
                minimum = minimum.min(projection);
                maximum = maximum.max(projection);
            }
            let rounding = radius.max(maximum.abs()).max(minimum.abs()).max(1.) * 1e-10;
            Ok(minimum > radius + tolerance + rounding || maximum < -radius - tolerance - rounding)
        };
        for axis in axes.into_iter().chain(self.0.normals.iter().copied()) {
            if separated(axis, meter)? {
                return Ok(false);
            }
        }
        for edge in &self.0.edges {
            for axis in axes {
                if separated(cross(*edge, axis), meter)? {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cube() -> Convex {
        let points = [-1., 1.]
            .into_iter()
            .flat_map(|x| {
                [-1., 1.]
                    .into_iter()
                    .flat_map(move |y| [-1., 1.].map(move |z| [x, y, z]))
            })
            .collect::<Vec<_>>();
        Convex::build(&points, 2_000_000, || false)
            .unwrap()
            .ok()
            .unwrap()
    }
    fn world(hull: Convex) -> World {
        World::build(
            vec![Volume {
                key: Key {
                    instance: 1,
                    component: 1,
                },
                mask: 1,
                shape: Shape::Convex(hull),
            }],
            Coverage::Complete,
            100,
            || false,
        )
        .unwrap()
        .ok()
        .unwrap()
    }
    fn query(center: [f32; 3], extent: f32) -> Box3 {
        Box3 {
            center,
            rotation: [0., 0., 0., 1.],
            extents: [extent; 3],
        }
    }
    #[test]
    fn hull_queries_cover_interior_containment_touching_and_separation() {
        let hull = cube();
        assert_eq!(hull.counts(), [8, 12]);
        let world = world(hull);
        for (center, extent, tolerance, blocked) in [
            ([0.; 3], 0.1, 0., true),
            ([0.; 3], 3., 0., true),
            ([2., 0., 0.], 1., 0., true),
            ([2.1, 0., 0.], 1., 0., false),
            ([2.001, 0., 0.], 1., 0.01, true),
        ] {
            let result = world
                .query(query(center, extent), 1, tolerance, 10000, || false)
                .unwrap();
            assert_eq!(
                matches!(result.verdict, Verdict::Blocked(_)),
                blocked,
                "{center:?}"
            );
        }
        assert_eq!(
            world
                .query(query([0.; 3], 0.1), 2, 0., 10, || false)
                .unwrap()
                .verdict,
            Verdict::Clear
        );
    }
    #[test]
    fn tetrahedron_face_separates_boxes_that_overlap_its_bounds() {
        let hull = Convex::build(
            &[[0., 0., 0.], [1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
            10000,
            || false,
        )
        .unwrap()
        .ok()
        .unwrap();
        let q = query([0.8; 3], 0.1);
        assert!(hull.bounds().overlaps(q.bounds(), 0.));
        let result = world(hull).query(q, 1, 0., 10000, || false).unwrap();
        assert_eq!(result.verdict, Verdict::Clear);
    }
    #[test]
    fn convex_model_fills_a_concavity_in_the_raw_point_cloud() {
        let points = [[0., 0.], [2., 0.], [2., 1.], [1., 1.], [1., 2.], [0., 2.]]
            .into_iter()
            .flat_map(|[x, z]| [-1., 1.].map(move |y| [x, y, z]))
            .collect::<Vec<_>>();
        let hull = Convex::build(&points, 100000, || false)
            .unwrap()
            .ok()
            .unwrap();
        assert!(matches!(
            world(hull)
                .query(query([1.4, 0., 1.4], 0.01), 1, 0., 10000, || false)
                .unwrap()
                .verdict,
            Verdict::Blocked(_)
        ));
    }
    #[test]
    fn rotated_hull_matches_independent_box_sat_away_from_contact() {
        let box_shape = Box3 {
            center: [2., -1., 3.],
            rotation: [0.2, 0.4, 0.4, 0.8],
            extents: [1.; 3],
        };
        let columns = [
            rotate(box_shape.rotation, [1., 0., 0.]),
            rotate(box_shape.rotation, [0., 1., 0.]),
            rotate(box_shape.rotation, [0., 0., 1.]),
        ];
        let matrix = std::array::from_fn(|i| {
            std::array::from_fn(|j| {
                if i == 3 {
                    if j == 3 { 1. } else { 0. }
                } else if j == 3 {
                    box_shape.center[i]
                } else {
                    columns[j][i]
                }
            })
        });
        let hull = cube().transformed(matrix, || false).unwrap().unwrap();
        let world = world(hull);
        for x in -5..=5 {
            for y in -3..=3 {
                for z in -5..=5 {
                    let q = Box3 {
                        center: [
                            2. + x as f32 * 0.473,
                            -1. + y as f32 * 0.517,
                            3. + z as f32 * 0.413,
                        ],
                        rotation: [0., 0.6, 0., 0.8],
                        extents: [0.23, 0.31, 0.17],
                    };
                    let expected = overlap(box_shape, q, 0.);
                    let result = world.query(q, 1, 0., 100000, || false).unwrap();
                    assert_eq!(
                        matches!(result.verdict, Verdict::Blocked(_)),
                        expected,
                        "{x},{y},{z}"
                    );
                }
            }
        }
    }
    #[test]
    fn irregular_hull_sat_matches_parry_support_map_queries() {
        use parry3d_f64::{
            math::{Pose, Rotation},
            query::intersection_test,
            shape::{ConvexPolyhedron, Cuboid},
        };
        let points = [
            [-1., -0.4, -0.7],
            [1.7, -0.3, -0.6],
            [0.2, 1.3, -0.5],
            [-0.4, 0.1, 1.9],
            [0.8, 0.9, 0.7],
        ];
        let hull = Convex::build(&points, 100000, || false)
            .unwrap()
            .ok()
            .unwrap();
        let reference = ConvexPolyhedron::from_convex_hull(
            &points.map(|p| Vector::new(p[0].into(), p[1].into(), p[2].into())),
        )
        .unwrap();
        let world = world(hull);
        for x in -5..=5 {
            for y in -4..=4 {
                for z in -5..=5 {
                    let q = Box3 {
                        center: [x as f32 * 0.411, y as f32 * 0.337, z as f32 * 0.453],
                        rotation: [0.2, 0.4, 0.4, 0.8],
                        extents: [0.23, 0.31, 0.17],
                    };
                    let [xq, yq, zq, wq] = q.rotation.map(f64::from);
                    let pose = Pose::from_parts(
                        Vector::new(q.center[0].into(), q.center[1].into(), q.center[2].into()),
                        Rotation::from_xyzw(xq, yq, zq, wq).normalize(),
                    );
                    let cuboid = Cuboid::new(Vector::new(
                        q.extents[0].into(),
                        q.extents[1].into(),
                        q.extents[2].into(),
                    ));
                    let reference = intersection_test(&Pose::IDENTITY, &reference, &pose, &cuboid)
                        .unwrap()
                        .intersecting;
                    let result = world.query(q, 1, 0., 100000, || false).unwrap();
                    assert_eq!(
                        matches!(result.verdict, Verdict::Blocked(_)),
                        reference,
                        "{x},{y},{z}"
                    );
                }
            }
        }
    }
    #[test]
    fn unsupported_inputs_cancellation_and_budgets_cannot_publish_partial_hulls_or_clearance() {
        let points = [[0., 0., 0.], [1., 0., 0.], [0., 1., 0.], [0., 0., 1.]];
        assert!(matches!(
            Convex::build(&points, 1, || false).unwrap(),
            Err(Verdict::Budget)
        ));
        assert!(matches!(
            Convex::build(&points, 10000, || true).unwrap(),
            Err(Verdict::Cancelled)
        ));
        assert!(Convex::build(&[[0., 0., 0.]; 4], 10000, || false).is_err());
        assert!(Convex::build(&[[0., 0., 0.]; 257], 2_000_000, || false).is_err());
        let hull = cube();
        let identity = [
            [1., 0., 0., 0.],
            [0., 1., 0., 0.],
            [0., 0., 1., 0.],
            [0., 0., 0., 1.],
        ];
        assert!(hull.transformed(identity, || true).unwrap().is_none());
        let mut flat = identity;
        flat[1][1] = 0.;
        assert!(hull.transformed(flat, || false).is_err());
        let world = world(hull);
        assert_eq!(
            world
                .query(query([0.; 3], 0.1), 1, 0., 5, || false)
                .unwrap()
                .verdict,
            Verdict::Budget
        );
        assert_eq!(
            world
                .query(query([0.; 3], 0.1), 1, 0., 10000, || true)
                .unwrap()
                .verdict,
            Verdict::Cancelled
        );
        let polls = std::cell::Cell::new(0);
        let result = world
            .query(query([0.; 3], 0.1), 1, 0., 10000, || {
                polls.set(polls.get() + 1);
                polls.get() > 5
            })
            .unwrap();
        assert_eq!(result.verdict, Verdict::Cancelled);
        assert_eq!(result.candidates, 1);
        let world = World::build(
            vec![Volume {
                key: Key {
                    instance: 1,
                    component: 1,
                },
                mask: 1,
                shape: Shape::Convex(cube()),
            }],
            Coverage::Incomplete,
            100,
            || false,
        )
        .unwrap()
        .ok()
        .unwrap();
        assert_eq!(
            world
                .query(query([10.; 3], 0.1), 1, 0., 10000, || false)
                .unwrap()
                .verdict,
            Verdict::Incomplete
        );
    }
}
