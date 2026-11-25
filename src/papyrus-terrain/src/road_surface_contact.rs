//! Local, actual-triangle contact measurements. Not a continuous road-union or
//! terrain/prefab support certificate; no physics or vehicle simulation occurs.
use crate::{path_mesh::Template, road_mesh_audit, road_network::SurfaceChunk};
use serde::Serialize;
use std::collections::BTreeSet;

pub struct Input<'a> {
    pub template: &'a Template,
    pub chunks: &'a [SurfaceChunk],
    pub roads: [usize; 2],
    /// X minimum, Z minimum, X maximum, Z maximum, in world metres.
    pub bounds: [f64; 4],
    pub maximum_work: usize,
}
#[derive(Debug, PartialEq)]
pub enum Failure {
    Invalid(&'static str),
    Budget,
}
#[derive(Debug, Serialize)]
pub struct Witness {
    pub chunks: [usize; 2],
    pub triangles: [usize; 2],
    pub projected_area: f64,
    pub polygon: Vec<[f64; 2]>,
    /// Second surface minus first surface, affine extrema over the polygon.
    pub signed_vertical_separation: [f64; 2],
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub roads: [usize; 2],
    pub bounds: [f64; 4],
    pub local_driving_triangles: [usize; 2],
    pub minimum_template_up_normal: f32,
    pub minimum_projected_pair_area: f64,
    pub tested_pairs: usize,
    pub projected_overlap_pairs: usize,
    pub edge_or_point_pairs: usize,
    /// Sum of pair areas, not an independently certified polygon-union area.
    pub projected_pair_area_sum: f64,
    pub plane_crossing_pairs: usize,
    pub minimum_absolute_vertical_separation: Option<f64>,
    pub maximum_absolute_vertical_separation: Option<f64>,
    pub witnesses: Vec<Witness>,
    pub work: usize,
    pub continuous_union_certified: bool,
    pub terrain_support_validated: bool,
    pub runtime_physics_validated: bool,
}
struct Triangle {
    points: [[f64; 3]; 3],
    bounds: [f64; 4],
    chunk: usize,
    face: usize,
}
struct Meter<'a, F> {
    work: usize,
    maximum: usize,
    cancel: &'a F,
}
impl<F: Fn() -> bool> Meter<'_, F> {
    fn spend(&mut self, n: usize) -> Result<bool, Failure> {
        if (self.cancel)() {
            return Ok(false);
        }
        self.work = self
            .work
            .checked_add(n)
            .filter(|w| *w <= self.maximum)
            .ok_or(Failure::Budget)?;
        Ok(true)
    }
}
fn orient(a: [f64; 2], b: [f64; 2], p: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])
}
fn overlaps(a: [f64; 4], b: [f64; 4]) -> bool {
    a[0] <= b[2] && b[0] <= a[2] && a[1] <= b[3] && b[1] <= a[3]
}
fn clip(polygon: Vec<[f64; 2]>, distance: impl Fn([f64; 2]) -> f64) -> Vec<[f64; 2]> {
    let Some(&last) = polygon.last() else {
        return polygon;
    };
    let mut out = Vec::with_capacity(polygon.len() + 1);
    let (mut previous, mut pd) = (last, distance(last));
    for p in polygon {
        let d = distance(p);
        if (d >= 0.) != (pd >= 0.) {
            let t = pd / (pd - d);
            out.push(std::array::from_fn(|k| {
                previous[k] + t * (p[k] - previous[k])
            }));
        }
        if d >= 0. {
            out.push(p);
        }
        (previous, pd) = (p, d);
    }
    out
}
fn intersection(a: &Triangle, b: &Triangle, bounds: [f64; 4]) -> Vec<[f64; 2]> {
    let project = |p: [f64; 3]| [p[0], p[2]];
    let q = b.points.map(project);
    let sign = orient(q[0], q[1], q[2]).signum();
    let mut polygon = a.points.map(project).to_vec();
    for i in 0..3 {
        polygon = clip(polygon, |p| sign * orient(q[i], q[(i + 1) % 3], p));
    }
    polygon = clip(polygon, |p| p[0] - bounds[0]);
    polygon = clip(polygon, |p| p[1] - bounds[1]);
    polygon = clip(polygon, |p| bounds[2] - p[0]);
    clip(polygon, |p| bounds[3] - p[1])
}
fn area(p: &[[f64; 2]]) -> f64 {
    if p.len() < 3 {
        return 0.;
    }
    (1..p.len() - 1)
        .map(|i| orient(p[0], p[i], p[i + 1]))
        .sum::<f64>()
        .abs()
        * 0.5
}
fn height(t: &Triangle, p: [f64; 2]) -> f64 {
    let [a, b, c] = t.points;
    let [ab, ac] = [[b[0] - a[0], b[2] - a[2]], [c[0] - a[0], c[2] - a[2]]];
    let d = ab[0] * ac[1] - ab[1] * ac[0];
    let u = ((p[0] - a[0]) * ac[1] - (p[1] - a[2]) * ac[0]) / d;
    let v = (ab[0] * (p[1] - a[2]) - ab[1] * (p[0] - a[0])) / d;
    a[1] + u * (b[1] - a[1]) + v * (c[1] - a[1])
}

pub fn analyze(input: Input<'_>, cancel: impl Fn() -> bool) -> Result<Option<Report>, Failure> {
    let mut meter = Meter {
        work: 0,
        maximum: input.maximum_work,
        cancel: &cancel,
    };
    macro_rules! spend {
        ($n:expr) => {
            if !meter.spend($n)? {
                return Ok(None);
            }
        };
    }
    spend!(0);
    if !(1..=128_000_000).contains(&input.maximum_work)
        || input.roads[0] == input.roads[1]
        || input.chunks.len() > 65536
        || input
            .bounds
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 10000.)
        || input.bounds[0] >= input.bounds[2]
        || input.bounds[1] >= input.bounds[3]
    {
        return Err(Failure::Invalid("contact query"));
    }
    spend!(input.template.vertices.len() + input.template.triangles.len());
    input
        .template
        .validate()
        .map_err(|_| Failure::Invalid("contact template"))?;
    let driving = input
        .template
        .triangles
        .iter()
        .map(|face| {
            let [a, b, c] = face.map(|i| input.template.vertices[i as usize]);
            let det = (f64::from(b[0]) - f64::from(a[0])) * (f64::from(c[2]) - f64::from(a[2]))
                - (f64::from(b[2]) - f64::from(a[2])) * (f64::from(c[0]) - f64::from(a[0]));
            det.abs() >= 1e-10
                && face
                    .iter()
                    .all(|&i| input.template.normals[i as usize][1] >= 0.5)
        })
        .collect::<Vec<_>>();
    let mut triangles: [Vec<Triangle>; 2] = Default::default();
    let mut keys = BTreeSet::new();
    let mut seen = [false; 2];
    for chunk in input.chunks {
        spend!(1);
        let Some(side) = input.roads.iter().position(|r| *r == chunk.road) else {
            continue;
        };
        if chunk.lod != 0 {
            continue;
        }
        if !keys.insert((chunk.road, chunk.chunk_index)) {
            return Err(Failure::Invalid("duplicate collision chunk"));
        }
        seen[side] = true;
        let g = &chunk.geometry;
        spend!(
            g.mesh.vertices.len()
                + g.mesh.triangles.len()
                + input.template.vertices.len()
                + input.template.triangles.len()
        );
        let remaining = meter.maximum.saturating_sub(meter.work).min(32_000_000);
        if remaining == 0 {
            return Err(Failure::Budget);
        }
        let audit = match road_mesh_audit::audit(
            g,
            input.template,
            road_mesh_audit::Settings {
                maximum_grade: 1.,
                minimum_up_normal: 0.5,
                maximum_work: remaining,
            },
            &cancel,
        ) {
            Ok(audit) => audit,
            Err(road_mesh_audit::Failure::Cancelled) => return Ok(None),
            Err(road_mesh_audit::Failure::Budget) => return Err(Failure::Budget),
            Err(_) => return Err(Failure::Invalid("collision chunk")),
        };
        spend!(audit.work);
        if audit.degenerate_world_triangles != 0 || audit.inverted_world_triangles != 0 {
            return Err(Failure::Invalid("invalid driving triangle"));
        }
        for (face, indices) in g.mesh.triangles.iter().enumerate() {
            spend!(1);
            if !driving[face % driving.len()] {
                continue;
            }
            // Source f32 addition occurs before f64 plane/clipping arithmetic.
            let points = indices.map(|i| {
                std::array::from_fn(|k| f64::from(g.origin[k] + g.mesh.vertices[i as usize][k]))
            });
            let bounds = [
                points.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min),
                points.iter().map(|p| p[2]).fold(f64::INFINITY, f64::min),
                points
                    .iter()
                    .map(|p| p[0])
                    .fold(f64::NEG_INFINITY, f64::max),
                points
                    .iter()
                    .map(|p| p[2])
                    .fold(f64::NEG_INFINITY, f64::max),
            ];
            if overlaps(bounds, input.bounds) {
                if triangles.iter().map(Vec::len).sum::<usize>() >= 131072 {
                    return Err(Failure::Invalid("contact triangle allowance"));
                }
                triangles[side].push(Triangle {
                    points,
                    bounds,
                    chunk: chunk.chunk_index,
                    face,
                });
            }
        }
    }
    if !seen.iter().all(|v| *v) {
        return Err(Failure::Invalid("missing collision road"));
    }
    let mut report = Report {
        engine: "papyrus-road-surface-contact-study-1",
        roads: input.roads,
        bounds: input.bounds,
        local_driving_triangles: triangles.each_ref().map(|v| v.len()),
        tested_pairs: 0,
        minimum_template_up_normal: 0.5,
        minimum_projected_pair_area: 1e-8,
        projected_overlap_pairs: 0,
        edge_or_point_pairs: 0,
        projected_pair_area_sum: 0.,
        plane_crossing_pairs: 0,
        minimum_absolute_vertical_separation: None,
        maximum_absolute_vertical_separation: None,
        witnesses: Vec::new(),
        work: 0,
        continuous_union_certified: false,
        terrain_support_validated: false,
        runtime_physics_validated: false,
    };
    for a in &triangles[0] {
        for b in &triangles[1] {
            spend!(1);
            report.tested_pairs += 1;
            if !overlaps(a.bounds, b.bounds) {
                continue;
            }
            spend!(256);
            let polygon = intersection(a, b, input.bounds);
            if polygon.is_empty() {
                continue;
            }
            let area = area(&polygon);
            if area <= 1e-8 {
                report.edge_or_point_pairs += 1;
                continue;
            }
            let mut range = [f64::INFINITY, f64::NEG_INFINITY];
            for &p in &polygon {
                let delta = height(b, p) - height(a, p);
                range[0] = range[0].min(delta);
                range[1] = range[1].max(delta);
            }
            if !range.iter().all(|v| v.is_finite()) {
                return Err(Failure::Invalid("contact plane"));
            }
            let crosses = range[0] <= 0. && range[1] >= 0.;
            let minimum = if crosses {
                0.
            } else {
                range[0].abs().min(range[1].abs())
            };
            let maximum = range[0].abs().max(range[1].abs());
            report.projected_overlap_pairs += 1;
            report.projected_pair_area_sum += area;
            report.plane_crossing_pairs += usize::from(crosses);
            report.minimum_absolute_vertical_separation = Some(
                report
                    .minimum_absolute_vertical_separation
                    .map_or(minimum, |v| v.min(minimum)),
            );
            report.maximum_absolute_vertical_separation = Some(
                report
                    .maximum_absolute_vertical_separation
                    .map_or(maximum, |v| v.max(maximum)),
            );
            report.witnesses.push(Witness {
                chunks: [a.chunk, b.chunk],
                triangles: [a.face, b.face],
                projected_area: area,
                polygon,
                signed_vertical_separation: range,
            });
            report
                .witnesses
                .sort_by(|a, b| b.projected_area.total_cmp(&a.projected_area));
            report.witnesses.truncate(8);
        }
    }
    report.work = meter.work;
    Ok((!cancel()).then_some(report))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn template() -> Template {
        Template {
            vertices: vec![[-1., 0., -1.], [1., 0., -1.], [1., 0., 1.], [-1., 0., 1.]],
            normals: vec![[0., 1., 0.]; 4],
            tangents: vec![[1., 0., 0., 1.]; 4],
            uv: vec![[0.; 2]; 4],
            triangles: vec![[0, 1, 2], [0, 2, 3]],
            center: [0.; 3],
            extents: [1., 0., 1.],
        }
    }
    fn chunk(template: &Template, road: usize, origin: [f32; 3]) -> SurfaceChunk {
        SurfaceChunk {
            road,
            lod: 0,
            chunk_index: 0,
            geometry: crate::path_mesh::Chunk {
                first_repetition: 0,
                repetitions: 1,
                origin,
                mesh: template.clone(),
                work: 0,
            },
        }
    }
    fn query<'a>(template: &'a Template, chunks: &'a [SurfaceChunk]) -> Input<'a> {
        Input {
            template,
            chunks,
            roads: [0, 1],
            bounds: [-4., -4., 4., 4.],
            maximum_work: 100000,
        }
    }
    #[test]
    fn projected_overlap_is_not_a_vertical_connection_certificate() {
        let t = template();
        let mut chunks = vec![chunk(&t, 0, [0.; 3]), chunk(&t, 1, [1., 0., 0.])];
        let r = analyze(query(&t, &chunks), || false).unwrap().unwrap();
        assert!((r.projected_pair_area_sum - 2.).abs() < 1e-9);
        assert_eq!(r.minimum_absolute_vertical_separation, Some(0.));
        assert_eq!(r.maximum_absolute_vertical_separation, Some(0.));
        assert!(r.plane_crossing_pairs > 0);
        assert!(
            !r.continuous_union_certified
                && !r.terrain_support_validated
                && !r.runtime_physics_validated
        );
        chunks[1].geometry.origin[1] = 2.;
        let r = analyze(query(&t, &chunks), || false).unwrap().unwrap();
        assert!((r.projected_pair_area_sum - 2.).abs() < 1e-9);
        assert_eq!(r.minimum_absolute_vertical_separation, Some(2.));
        assert_eq!(r.maximum_absolute_vertical_separation, Some(2.));
        assert_eq!(r.plane_crossing_pairs, 0);
    }
    #[test]
    fn plane_extrema_edge_contacts_and_local_bounds_are_distinguished() {
        let t = template();
        let mut chunks = vec![chunk(&t, 0, [0.; 3]), chunk(&t, 1, [0.; 3])];
        for p in &mut chunks[1].geometry.mesh.vertices {
            p[1] = p[0] * 0.25;
        }
        chunks[1].geometry.mesh.extents[1] = 0.25;
        let r = analyze(query(&t, &chunks), || false).unwrap().unwrap();
        assert!((r.projected_pair_area_sum - 4.).abs() < 1e-9);
        assert_eq!(r.minimum_absolute_vertical_separation, Some(0.));
        assert!((r.maximum_absolute_vertical_separation.unwrap() - 0.25).abs() < 1e-9);
        assert!(r.plane_crossing_pairs > 0);
        chunks[1] = chunk(&t, 1, [2., 0., 0.]);
        let r = analyze(query(&t, &chunks), || false).unwrap().unwrap();
        assert_eq!(r.projected_overlap_pairs, 0);
        assert!(r.edge_or_point_pairs > 0);
        assert!(r.minimum_absolute_vertical_separation.is_none());
        let mut input = query(&t, &chunks);
        input.bounds = [3., 3., 4., 4.];
        let r = analyze(input, || false).unwrap().unwrap();
        assert_eq!(r.projected_overlap_pairs, 0);
        assert_eq!(r.local_driving_triangles, [0, 0]);
    }
    #[test]
    fn missing_duplicate_invalid_geometry_cancel_and_budget_publish_no_result() {
        let t = template();
        let mut chunks = vec![chunk(&t, 0, [0.; 3]), chunk(&t, 1, [0.; 3])];
        assert!(analyze(query(&t, &chunks), || true).unwrap().is_none());
        let mut input = query(&t, &chunks);
        input.maximum_work = 1;
        assert_eq!(analyze(input, || false).unwrap_err(), Failure::Budget);
        assert!(matches!(
            analyze(query(&t, &chunks[..1]), || false),
            Err(Failure::Invalid("missing collision road"))
        ));
        chunks.push(chunk(&t, 1, [0.; 3]));
        assert!(matches!(
            analyze(query(&t, &chunks), || false),
            Err(Failure::Invalid("duplicate collision chunk"))
        ));
        chunks.pop();
        chunks[1].geometry.mesh.triangles[0] = [0, 2, 1];
        assert!(matches!(
            analyze(query(&t, &chunks), || false),
            Err(Failure::Invalid("collision chunk"))
        ));
    }
}
