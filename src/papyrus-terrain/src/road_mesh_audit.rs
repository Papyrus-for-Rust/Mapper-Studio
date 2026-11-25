//! Grades of actual deformed triangles in the authored road template frame.
//! This measures collision geometry, not PhysX cooking or vehicle simulation.
use crate::path_mesh::{Chunk, Template};
use serde::Serialize;
#[derive(Clone, Copy)]
pub struct Settings {
    pub maximum_grade: f32,
    /// Classify driving faces from the original template's vertex normals.
    pub minimum_up_normal: f32,
    pub maximum_work: usize,
}
#[derive(Debug, PartialEq)]
pub enum Failure {
    Invalid,
    Budget,
    Cancelled,
}
#[derive(Debug, Serialize)]
pub struct Worst {
    pub repetition: usize,
    pub triangle: usize,
    pub position: [f64; 3],
    pub longitudinal_grade: f64,
    pub cross_grade: f64,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub triangles: usize,
    pub driving_triangles: usize,
    pub excluded_template_triangles: usize,
    pub degenerate_world_triangles: usize,
    pub inverted_world_triangles: usize,
    /// Index into the complete chunk triangle array, including repetitions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_invalid_triangle: Option<usize>,
    pub maximum_longitudinal_grade: f64,
    pub maximum_cross_grade: f64,
    pub longitudinal_violations: usize,
    pub worst: Option<Worst>,
    pub maximum_grade: f32,
    pub minimum_up_normal: f32,
    pub passed: bool,
    pub work: usize,
    pub physics_cooking_validated: bool,
    pub vehicle_access_validated: bool,
}
fn difference(a: [f32; 3], b: [f32; 3]) -> [f64; 3] {
    std::array::from_fn(|i| f64::from(a[i]) - f64::from(b[i]))
}
/// Each triangle's Jacobian maps authored X/Z to the deformed surface. Its Z
/// derivative measures longitudinal grade even on yawed or banked road chunks.
/// Vertical sidewalls are excluded by their original normals/XZ degeneracy;
/// folded/vertical driving faces fail instead of disappearing from the audit.
pub fn audit(
    chunk: &Chunk,
    template: &Template,
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Report, Failure> {
    if cancelled() {
        return Err(Failure::Cancelled);
    }
    template.validate().map_err(|_| Failure::Invalid)?;
    chunk.mesh.validate().map_err(|_| Failure::Invalid)?;
    if !(1..=32_000_000).contains(&settings.maximum_work)
        || !settings.maximum_grade.is_finite()
        || !(0.001..=1.).contains(&settings.maximum_grade)
        || !settings.minimum_up_normal.is_finite()
        || !(0. ..=1.).contains(&settings.minimum_up_normal)
        || chunk.repetitions == 0
        || chunk.repetitions > 32
        || chunk.origin.iter().any(|v| !v.is_finite())
        || chunk.mesh.vertices.len() != chunk.repetitions * template.vertices.len()
        || chunk.mesh.triangles.len() != chunk.repetitions * template.triangles.len()
    {
        return Err(Failure::Invalid);
    }
    let mut report = Report {
        engine: "papyrus-road-mesh-grade-audit-1",
        triangles: chunk.mesh.triangles.len(),
        driving_triangles: 0,
        excluded_template_triangles: 0,
        degenerate_world_triangles: 0,
        inverted_world_triangles: 0,
        first_invalid_triangle: None,
        maximum_longitudinal_grade: 0.,
        maximum_cross_grade: 0.,
        longitudinal_violations: 0,
        worst: None,
        maximum_grade: settings.maximum_grade,
        minimum_up_normal: settings.minimum_up_normal,
        passed: false,
        work: 0,
        physics_cooking_validated: false,
        vehicle_access_validated: false,
    };
    for repetition in 0..chunk.repetitions {
        let offset = repetition * template.vertices.len();
        for (triangle, face) in template.triangles.iter().enumerate() {
            if cancelled() {
                return Err(Failure::Cancelled);
            }
            if report.work >= settings.maximum_work {
                return Err(Failure::Budget);
            }
            report.work += 1;
            let indices = face.map(|i| i as usize);
            let expected = face.map(|i| i + offset as u32);
            if chunk.mesh.triangles[repetition * template.triangles.len() + triangle] != expected {
                return Err(Failure::Invalid);
            }
            let [a, b, c] = indices.map(|i| template.vertices[i]);
            let [ab, ac] = [difference(b, a), difference(c, a)];
            let determinant = ab[0] * ac[2] - ac[0] * ab[2];
            if determinant.abs() < 1e-10
                || indices
                    .iter()
                    .any(|&i| template.normals[i][1] < settings.minimum_up_normal)
            {
                report.excluded_template_triangles += 1;
                continue;
            }
            report.driving_triangles += 1;
            let [p, q, r] = indices.map(|i| chunk.mesh.vertices[offset + i]);
            let [pq, pr] = [difference(q, p), difference(r, p)];
            let along: [f64; 3] =
                std::array::from_fn(|i| (pr[i] * ab[0] - pq[i] * ac[0]) / determinant);
            let across: [f64; 3] =
                std::array::from_fn(|i| (pq[i] * ac[2] - pr[i] * ab[2]) / determinant);
            let along_run = along[0].hypot(along[2]);
            let across_run = across[0].hypot(across[2]);
            let horizontal_area = pq[0] * pr[2] - pq[2] * pr[0];
            if along_run < 1e-8 || across_run < 1e-8 || horizontal_area.abs() < 1e-10 {
                report.degenerate_world_triangles += 1;
                report
                    .first_invalid_triangle
                    .get_or_insert(repetition * template.triangles.len() + triangle);
                continue;
            }
            if horizontal_area / determinant < 0. {
                report.inverted_world_triangles += 1;
                report
                    .first_invalid_triangle
                    .get_or_insert(repetition * template.triangles.len() + triangle);
            }
            let grade = along[1].abs() / along_run;
            let cross = across[1].abs() / across_run;
            if !grade.is_finite() || !cross.is_finite() {
                return Err(Failure::Invalid);
            }
            report.longitudinal_violations +=
                usize::from(grade > f64::from(settings.maximum_grade));
            report.maximum_cross_grade = report.maximum_cross_grade.max(cross);
            if report.worst.is_none() || grade > report.maximum_longitudinal_grade {
                report.maximum_longitudinal_grade = grade;
                report.worst = Some(Worst {
                    repetition: chunk.first_repetition + repetition,
                    triangle,
                    position: std::array::from_fn(|i| {
                        (f64::from(p[i]) + f64::from(q[i]) + f64::from(r[i])) / 3.
                            + f64::from(chunk.origin[i])
                    }),
                    longitudinal_grade: grade,
                    cross_grade: cross,
                });
            }
        }
    }
    report.passed = report.driving_triangles > 0
        && report.degenerate_world_triangles == 0
        && report.inverted_world_triangles == 0
        && report.longitudinal_violations == 0;
    Ok(report)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn template() -> Template {
        Template {
            vertices: vec![[-1., 0., 0.], [1., 0., 0.], [-1., 0., 10.], [1., 0., 10.]],
            normals: vec![[0., 1., 0.]; 4],
            tangents: vec![[1., 0., 0., 1.]; 4],
            uv: vec![[0., 0.]; 4],
            triangles: vec![[0, 2, 1], [1, 2, 3]],
            center: [0., 0., 5.],
            extents: [1., 0., 5.],
        }
    }
    fn chunk(grade: f32, cross: f32, yaw: f32) -> Chunk {
        let mut mesh = template();
        let (s, c) = yaw.to_radians().sin_cos();
        for p in &mut mesh.vertices {
            let x = p[0];
            let z = p[2];
            *p = [c * x + s * z, grade * z + cross * x, -s * x + c * z];
        }
        let low: [f32; 3] = std::array::from_fn(|i| {
            mesh.vertices
                .iter()
                .map(|p| p[i])
                .fold(f32::INFINITY, f32::min)
        });
        let high: [f32; 3] = std::array::from_fn(|i| {
            mesh.vertices
                .iter()
                .map(|p| p[i])
                .fold(f32::NEG_INFINITY, f32::max)
        });
        mesh.center = std::array::from_fn(|i| (low[i] + high[i]) * 0.5);
        mesh.extents = std::array::from_fn(|i| (high[i] - low[i]) * 0.5);
        Chunk {
            first_repetition: 0,
            repetitions: 1,
            origin: [100., 20., -100.],
            mesh,
            work: 0,
        }
    }
    fn settings() -> Settings {
        Settings {
            maximum_grade: 0.2,
            minimum_up_normal: 0.5,
            maximum_work: 100,
        }
    }
    #[test]
    fn actual_triangle_grades_survive_yaw_and_distinguish_crossfall() {
        for yaw in [0., 37., 123., 270.] {
            let report = audit(&chunk(0.25, 0.1, yaw), &template(), settings(), || false).unwrap();
            assert!((report.maximum_longitudinal_grade - 0.25).abs() < 1e-6);
            assert!((report.maximum_cross_grade - 0.1).abs() < 1e-6);
            assert_eq!(report.longitudinal_violations, 2);
            assert!(!report.passed && !report.vehicle_access_validated);
            assert!(
                audit(&chunk(0.1, 0.4, yaw), &template(), settings(), || false)
                    .unwrap()
                    .passed
            );
        }
    }
    #[test]
    fn folded_face_identity_includes_its_chunk_repetition() {
        let mut c = chunk(0., 0., 0.);
        let vertices = c.mesh.vertices.clone();
        c.mesh
            .vertices
            .extend(vertices.iter().map(|p| [-p[0], p[1], p[2]]));
        c.mesh.normals.extend_from_within(..);
        c.mesh.tangents.extend_from_within(..);
        c.mesh.uv.extend_from_within(..);
        let faces = c.mesh.triangles.clone();
        c.mesh
            .triangles
            .extend(faces.iter().map(|f| f.map(|i| i + 4)));
        c.repetitions = 2;
        let r = audit(&c, &template(), settings(), || false).unwrap();
        assert_eq!(r.driving_triangles, 4);
        assert_eq!(r.inverted_world_triangles, 2);
        assert_eq!(r.first_invalid_triangle, Some(2));
        assert_eq!(r.work, 4);
        assert!(!r.passed);
    }
    #[test]
    fn vertical_driving_faces_fail_and_template_sidewalls_are_explicitly_excluded() {
        let mut c = chunk(0.25, 0., 0.);
        c.mesh.vertices[2][2] = 0.;
        let r = audit(&c, &template(), settings(), || false).unwrap();
        assert_eq!(r.degenerate_world_triangles, 1);
        assert_eq!(r.first_invalid_triangle, Some(0));
        assert!(!r.passed);
        let mut mirrored = chunk(0., 0., 0.);
        for p in &mut mirrored.mesh.vertices {
            p[0] = -p[0];
        }
        let r = audit(&mirrored, &template(), settings(), || false).unwrap();
        assert_eq!(r.inverted_world_triangles, 2);
        assert_eq!(r.first_invalid_triangle, Some(0));
        assert!(!r.passed);
        let mut source = template();
        source.normals = vec![[1., 0., 0.]; 4];
        let r = audit(&chunk(0., 0., 0.), &source, settings(), || false).unwrap();
        assert_eq!(r.excluded_template_triangles, 2);
        assert_eq!(r.first_invalid_triangle, None);
        assert!(!r.passed);
    }
    #[test]
    fn wrong_indices_budget_and_cancellation_cannot_publish_a_pass() {
        let mut c = chunk(0., 0., 0.);
        c.mesh.triangles[0] = [0, 1, 2];
        assert!(matches!(
            audit(&c, &template(), settings(), || false),
            Err(Failure::Invalid)
        ));
        assert!(matches!(
            audit(
                &chunk(0., 0., 0.),
                &template(),
                Settings {
                    maximum_work: 1,
                    ..settings()
                },
                || false
            ),
            Err(Failure::Budget)
        ));
        assert!(matches!(
            audit(&chunk(0., 0., 0.), &template(), settings(), || true),
            Err(Failure::Cancelled)
        ));
    }
}
