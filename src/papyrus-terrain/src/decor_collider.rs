//! Raw source-triangle AddToHeightMap adapter. Does not emulate PhysX cooking.
use crate::{
    decor_fit,
    erosion::{decode, encode},
    height_mesh::Mesh,
};
use serde::Serialize;
use std::collections::BTreeMap;
pub const VERSION: &str = "papyrus-decor-colliders-10";
#[derive(Clone, Debug)]
pub struct Contributor {
    pub mesh: Mesh,
    pub local_to_root: [[f32; 4]; 4],
}
#[derive(Default, Debug, Serialize)]
pub struct Report {
    pub instances: usize,
    pub meshes: usize,
    pub changed_samples: usize,
    pub maximum_raise: f32,
}
pub(crate) struct Plan {
    pub writes: Vec<(usize, i16)>,
    pub report: Report,
}
#[derive(Debug, PartialEq)]
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
    pub contributors: &'a [Contributor],
    pub pose: &'a decor_fit::Pose,
}
pub fn validate(contributors: &[Contributor]) -> Result<(), String> {
    if contributors.len() > 4096 {
        return Err("Too many height contributor meshes".into());
    }
    for c in contributors {
        c.mesh.validate()?;
        let m = &c.local_to_root;
        let determinant = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
        if !c.mesh.enabled
            || c.mesh.trigger
            || c.mesh.convex
            || !matches!(c.mesh.cooking_options, 0 | 30)
            || m.iter()
                .flatten()
                .any(|v| !v.is_finite() || v.abs() > 10000.)
            || m[3] != [0., 0., 0., 1.]
            || !determinant.is_finite()
            || determinant <= 1e-9
        {
            return Err("Unsupported height contributor collider flags or transform".into());
        }
    }
    Ok(())
}
fn world(m: &[[f32; 4]; 4], p: &decor_fit::Pose, v: [f32; 3]) -> [f64; 3] {
    let local: [f64; 3] = std::array::from_fn(|i| {
        (m[i][0] as f64 * v[0] as f64
            + m[i][1] as f64 * v[1] as f64
            + m[i][2] as f64 * v[2] as f64
            + m[i][3] as f64)
            * p.scale[i] as f64
    });
    let [x, y, z, w] = p.rotation.map(f64::from);
    let [a, b, c] = local;
    let t = [
        2. * (y * c - z * b),
        2. * (z * a - x * c),
        2. * (x * b - y * a),
    ];
    [
        a + w * t[0] + y * t[2] - z * t[1] + p.position[0] as f64,
        b + w * t[1] + z * t[0] - x * t[2] + p.position[1] as f64,
        c + w * t[2] + x * t[1] - y * t[0] + p.position[2] as f64,
    ]
}
pub(crate) fn prepare(
    input: Input<'_>,
    work: &mut usize,
    limit: usize,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Plan>, Failure> {
    let Input {
        heights,
        resolution: n,
        size,
        initial,
        contributors,
        pose,
    } = input;
    if cancelled() {
        return Ok(None);
    }
    if !(2..=8193).contains(&n)
        || n.checked_mul(n) != Some(heights.len())
        || !size.is_finite()
        || size <= 0.
        || validate(contributors).is_err()
        || initial.len() > 262144
        || initial.windows(2).any(|p| p[0].0 >= p[1].0)
        || initial
            .iter()
            .any(|(i, h)| *i >= heights.len() || !(0..=32766).contains(h))
    {
        return Err(Failure::Unsupported);
    }
    let mut writes: BTreeMap<_, _> = initial.iter().copied().collect();
    let mut report = Report {
        instances: usize::from(!contributors.is_empty()),
        meshes: contributors.len(),
        ..Default::default()
    };
    let mut visit = || {
        if *work >= limit {
            Err(Failure::Budget)
        } else {
            *work += 1;
            Ok(())
        }
    };
    let size = size as f64;
    let index = |x: f64| {
        ((x / size + 0.5) * n as f64)
            .floor()
            .clamp(0., (n - 1) as f64) as usize
    };
    for contributor in contributors {
        if cancelled() {
            return Ok(None);
        }
        let mut vertices = Vec::with_capacity(contributor.mesh.vertices.len());
        for (i, v) in contributor.mesh.vertices.iter().enumerate() {
            if i % 64 == 0 && cancelled() {
                return Ok(None);
            }
            visit()?;
            let v = world(&contributor.local_to_root, pose, *v);
            if v.iter().any(|x| !x.is_finite()) || v[1] < -500. || v[1] > 500. {
                return Err(Failure::Unsupported);
            }
            if v[0].abs() > size / 2. || v[2].abs() > size / 2. {
                return Err(Failure::Outside);
            }
            vertices.push(v);
        }
        // Scan only each triangle's projected cells. Shared edges and stacked
        // surfaces are resolved by maximum height, independent of triangle order.
        for t in &contributor.mesh.triangles {
            if cancelled() {
                return Ok(None);
            }
            visit()?;
            let [a, b, c] = t.map(|i| vertices[i as usize]);
            let dx = b[0] - a[0];
            let dz = b[2] - a[2];
            let ex = c[0] - a[0];
            let ez = c[2] - a[2];
            let det = dx * ez - dz * ex;
            let scale = dx.abs().max(dz.abs()).max(ex.abs()).max(ez.abs());
            if det.abs() <= scale * scale * 1e-12 || (!contributor.mesh.backfaces && det >= 0.) {
                continue;
            }
            for z in index(a[2].min(b[2]).min(c[2]))..=index(a[2].max(b[2]).max(c[2])) {
                if cancelled() {
                    return Ok(None);
                }
                let wz = ((z as f64 + 0.5) / n as f64 - 0.5) * size;
                for x in index(a[0].min(b[0]).min(c[0]))..=index(a[0].max(b[0]).max(c[0])) {
                    visit()?;
                    let wx = ((x as f64 + 0.5) / n as f64 - 0.5) * size;
                    let px = wx - a[0];
                    let pz = wz - a[2];
                    let u = (px * ez - pz * ex) / det;
                    let v = (dx * pz - dz * px) / det;
                    if u < -1e-9 || v < -1e-9 || u + v > 1. + 1e-9 {
                        continue;
                    }
                    let h = (a[1] + u * (b[1] - a[1]) + v * (c[1] - a[1])) as f32;
                    let i = z * n + x;
                    let old = *writes.get(&i).unwrap_or(&heights[i]);
                    let next = encode(h);
                    if next > old {
                        writes.insert(i, next);
                    }
                    if writes.len() > 262144 {
                        return Err(Failure::Unsupported);
                    }
                }
            }
        }
    }
    if cancelled() {
        return Ok(None);
    }
    for (i, h) in &writes {
        let before = initial
            .binary_search_by_key(i, |p| p.0)
            .map_or(heights[*i], |j| initial[j].1);
        if *h > before {
            report.changed_samples += 1;
            report.maximum_raise = report.maximum_raise.max(decode(*h) - decode(before));
        }
    }
    writes.retain(|i, h| *h != heights[*i]);
    Ok(Some(Plan {
        writes: writes.into_iter().collect(),
        report,
    }))
}
impl Report {
    pub(crate) fn commit(&mut self, other: Self) {
        self.instances += other.instances;
        self.meshes += other.meshes;
        self.changed_samples += other.changed_samples;
        self.maximum_raise = self.maximum_raise.max(other.maximum_raise);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn contributor() -> Contributor {
        Contributor {
            mesh: Mesh {
                kind: "raw-triangle-mesh".into(),
                enabled: true,
                trigger: false,
                convex: false,
                cooking_options: 30,
                backfaces: false,
                vertices: vec![[-2., 2., -2.], [-2., 2., 2.], [2., 2., -2.], [2., 2., 2.]],
                triangles: vec![[0, 1, 2], [2, 1, 3]],
            },
            local_to_root: [
                [1., 0., 0., 0.],
                [0., 1., 0., 0.],
                [0., 0., 1., 0.],
                [0., 0., 0., 1.],
            ],
        }
    }
    fn pose() -> decor_fit::Pose {
        decor_fit::Pose {
            candidate: 0,
            position: [0.; 3],
            rotation: [0., 0., 0., 1.],
            scale: [1.; 3],
        }
    }
    fn run(
        c: &[Contributor],
        p: &decor_fit::Pose,
        initial: &[(usize, i16)],
        limit: usize,
    ) -> Result<Option<Plan>, Failure> {
        prepare(
            Input {
                heights: &[encode(-10.); 81],
                resolution: 9,
                size: 9.,
                initial,
                contributors: c,
                pose: p,
            },
            &mut 0,
            limit,
            &|| false,
        )
    }
    #[test]
    fn triangle_intersections_raise_only_preserve_holes_and_initial_height_operations() {
        let mut c = contributor();
        c.mesh.vertices[3][1] = 5.;
        let before = vec![(40, encode(8.))];
        let plan = run(&[c], &pose(), &before, 10000).unwrap().unwrap();
        assert_eq!(
            plan.writes.iter().find(|(i, _)| *i == 40).unwrap().1,
            encode(8.)
        );
        assert!(
            plan.writes
                .iter()
                .all(|(i, h)| *i == 40 || *h <= encode(5.))
        );
        assert!(!plan.writes.iter().any(|(i, _)| *i == 0));
        assert_eq!(plan.report.changed_samples, 24);
        // A missing half of the quad stays empty; no bounding-box fill.
        let mut c = contributor();
        c.mesh.triangles.pop();
        let plan = run(&[c], &pose(), &[], 10000).unwrap().unwrap();
        assert!(!plan.writes.iter().any(|(i, _)| *i == 50));
        assert!(plan.writes.iter().any(|(i, _)| *i == 30));
        let mut c = contributor();
        for v in &mut c.mesh.vertices {
            v[1] = -20.;
        }
        assert!(
            run(&[c], &pose(), &[], 10000)
                .unwrap()
                .unwrap()
                .writes
                .is_empty()
        );
    }
    #[test]
    fn child_scale_rotation_and_translation_apply_in_hierarchy_order() {
        let mut c = contributor();
        for (i, row) in c.local_to_root.iter_mut().enumerate().take(3) {
            row[3] = [4., 5., 6.][i];
        }
        let mut p = pose();
        let q = std::f32::consts::FRAC_1_SQRT_2;
        p.rotation = [q, 0., 0., q];
        p.scale = [2., 3., 4.];
        p.position = [1., 2., 3.];
        let actual = world(&c.local_to_root, &p, [1., 2., 3.]);
        for (value, expected) in actual.into_iter().zip([11., -34., 24.]) {
            assert!((value - expected).abs() < 1e-5);
        }
    }
    #[test]
    fn projected_raster_matches_brute_ray_queries_under_child_affine_and_pose_transforms() {
        for degrees in [0_f32, 13., 47.5, 90., 143., 180., 270., 359.] {
            let mut c = contributor();
            for v in &mut c.mesh.vertices {
                v[1] += 0.3 * v[0] + 0.2 * v[2];
            }
            // Affine child transform, including shear; raw triangle geometry
            // supports this explicitly without claiming PhysX scale decomposition.
            c.local_to_root = [
                [1.1, 0.1, 0., 0.5],
                [0., 0.8, 0.15, 1.],
                [0.1, 0., 1.3, -0.5],
                [0., 0., 0., 1.],
            ];
            let mut p = pose();
            let (s, co) = (degrees.to_radians() * 0.5).sin_cos();
            let (pitch_s, pitch_c) = 10_f32.to_radians().sin_cos();
            p.rotation = [co * pitch_s, s * pitch_c, -s * pitch_s, co * pitch_c];
            p.scale = [0.9, 0.7, 0.8];
            p.position = [0.2, 0.5, 0.3];
            let vertices: Vec<_> = c
                .mesh
                .vertices
                .iter()
                .map(|v| world(&c.local_to_root, &p, *v))
                .collect();
            let surface = crate::water_mesh::Surface::new(&vertices, &c.mesh.triangles).unwrap();
            let plan = run(std::slice::from_ref(&c), &p, &[], 10000)
                .unwrap()
                .unwrap();
            let writes: BTreeMap<_, _> = plan.writes.into_iter().collect();
            for z in 0..9 {
                for x in 0..9 {
                    let hit = surface
                        .highest(x as f64 - 4., z as f64 - 4., false, &mut 0, 1000, || false)
                        .unwrap();
                    assert_eq!(
                        writes.get(&(z * 9 + x)).copied(),
                        hit.filter(|h| *h > -10.).map(|h| encode(h as f32)),
                        "yaw {degrees}, cell {x},{z}"
                    );
                }
            }
        }
    }
    #[test]
    fn highest_surface_face_policy_verticals_and_triangle_order_are_explicit() {
        let mut c = contributor();
        let base = c.mesh.vertices.clone();
        c.mesh
            .vertices
            .extend(base.iter().map(|v| [v[0], v[1] + 3., v[2]]));
        c.mesh.triangles.extend([[4, 5, 6], [6, 5, 7]]);
        let top = run(std::slice::from_ref(&c), &pose(), &[], 10000)
            .unwrap()
            .unwrap();
        assert!(top.writes.iter().all(|(_, h)| *h == encode(5.)));
        c.mesh.triangles.reverse();
        assert_eq!(
            top.writes,
            run(std::slice::from_ref(&c), &pose(), &[], 10000)
                .unwrap()
                .unwrap()
                .writes
        );
        for t in &mut c.mesh.triangles {
            t.swap(0, 1);
        }
        assert!(
            run(std::slice::from_ref(&c), &pose(), &[], 10000)
                .unwrap()
                .unwrap()
                .writes
                .is_empty()
        );
        c.mesh.backfaces = true;
        assert_eq!(
            top.writes,
            run(std::slice::from_ref(&c), &pose(), &[], 10000)
                .unwrap()
                .unwrap()
                .writes
        );
        for v in &mut c.mesh.vertices {
            v[0] = 0.;
        }
        assert!(
            run(&[c], &pose(), &[], 10000)
                .unwrap()
                .unwrap()
                .writes
                .is_empty()
        );
    }
    #[test]
    fn bounds_unknown_flags_reflections_budget_and_cancellation_never_return_partial_edits() {
        let c = contributor();
        assert!(matches!(
            run(std::slice::from_ref(&c), &pose(), &[], 8),
            Err(Failure::Budget)
        ));
        let mut p = pose();
        p.position[0] = 4.;
        assert!(matches!(
            run(std::slice::from_ref(&c), &p, &[], 10000),
            Err(Failure::Outside)
        ));
        for flags in [1, 31, u32::MAX] {
            let mut c = c.clone();
            c.mesh.cooking_options = flags;
            assert!(validate(&[c]).is_err());
        }
        let mut negative = c.clone();
        negative.local_to_root[0][0] = -1.;
        assert!(validate(&[negative]).is_err());
        let mut convex = c.clone();
        convex.mesh.convex = true;
        assert!(validate(&[convex]).is_err());
        let mut trigger = c.clone();
        trigger.mesh.trigger = true;
        assert!(validate(&[trigger]).is_err());
        let heights = vec![encode(-10.); 81];
        let before = heights.clone();
        let calls = std::cell::Cell::new(0);
        let result = prepare(
            Input {
                heights: &heights,
                resolution: 9,
                size: 9.,
                initial: &[],
                contributors: &[c],
                pose: &pose(),
            },
            &mut 0,
            10000,
            &|| {
                calls.set(calls.get() + 1);
                calls.get() > 4
            },
        )
        .unwrap();
        assert!(result.is_none());
        assert_eq!(before, heights);
    }
}
