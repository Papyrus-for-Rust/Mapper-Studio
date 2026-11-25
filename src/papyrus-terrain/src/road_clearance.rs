//! Continuous bilinear terrain/affine pavement checking for generation workers.
//! Passing this numerical model is not collider, water or runtime acceptance.
use crate::{
    path_mesh::{Chunk, Template},
    path_terrain::{self, Grid, Layout},
    road_bed::ClearanceProbe,
};
use serde::Serialize;
use std::collections::BTreeMap;
pub const VERSION: &str = "papyrus-road-clearance-1";
#[derive(Clone, Copy)]
pub struct Settings {
    pub tolerance: f64,
    pub minimum_up_normal: f32,
    pub maximum_work: usize,
    pub maximum_probes: usize,
}
impl Settings {
    pub fn validate(self) -> Result<(), Failure> {
        if !self.tolerance.is_finite()
            || !(0.0001..=1.).contains(&self.tolerance)
            || !self.minimum_up_normal.is_finite()
            || !(0. ..=1.).contains(&self.minimum_up_normal)
            || !(1..=10_000_000).contains(&self.maximum_work)
            || !(1..=131072).contains(&self.maximum_probes)
        {
            Err(Failure::Invalid)
        } else {
            Ok(())
        }
    }
}
pub struct Input<'a> {
    pub grid: Grid<'a>,
    pub chunk: &'a Chunk,
    pub template: &'a Template,
    pub road: usize,
    pub chunk_index: usize,
}
#[derive(Debug, PartialEq)]
pub enum Failure {
    Invalid,
    Budget,
    Cancelled,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub driving_triangles: usize,
    pub cell_rectangle_tests: usize,
    pub continuous_intersections: usize,
    pub source_sampler_confirmed_intersections: usize,
    pub source_only_intersections: usize,
    pub maximum_continuous_terrain_excess: f64,
    pub tolerance: f64,
    pub work: usize,
    pub passed: bool,
    pub runtime_physics_validated: bool,
}
pub struct Output {
    pub report: Report,
    pub probes: Vec<ClearanceProbe>,
}
type Point = [f64; 2];
#[derive(Clone, Copy)]
struct Polygon {
    points: [Point; 16],
    len: usize,
}
impl Polygon {
    fn triangle(p: [Point; 3]) -> Self {
        let mut out = Self {
            points: [[0.; 2]; 16],
            len: 3,
        };
        out.points[..3].copy_from_slice(&p);
        out
    }
    fn push(&mut self, p: Point) -> Result<(), Failure> {
        if self.len == 16 {
            return Err(Failure::Invalid);
        }
        self.points[self.len] = p;
        self.len += 1;
        Ok(())
    }
    fn clip(self, axis: usize, bound: f64, greater: bool) -> Result<Self, Failure> {
        let mut out = Self {
            points: [[0.; 2]; 16],
            len: 0,
        };
        if self.len == 0 {
            return Ok(out);
        }
        let inside = |p: Point| {
            if greater {
                p[axis] >= bound
            } else {
                p[axis] <= bound
            }
        };
        let mut previous = self.points[self.len - 1];
        let mut was = inside(previous);
        for &current in &self.points[..self.len] {
            let now = inside(current);
            if now != was {
                let t = (bound - previous[axis]) / (current[axis] - previous[axis]);
                let mut p = std::array::from_fn(|i| previous[i] + t * (current[i] - previous[i]));
                p[axis] = bound;
                out.push(p)?;
            }
            if now {
                out.push(current)?;
            }
            previous = current;
            was = now;
        }
        Ok(out)
    }
    fn maximum(&self, c: [f64; 4], offset: Point) -> (f64, Point) {
        let value = |p: Point| {
            let [x, z] = [p[0] - offset[0], p[1] - offset[1]];
            c[0] + c[1] * x + c[2] * z + c[3] * x * z
        };
        let mut best = (f64::NEG_INFINITY, [0.; 2]);
        for i in 0..self.len {
            let p = self.points[i];
            let q = self.points[(i + 1) % self.len];
            let v = value(p);
            if v > best.0 {
                best = (v, p);
            }
            let [x, z] = [p[0] - offset[0], p[1] - offset[1]];
            let [dx, dz] = [q[0] - p[0], q[1] - p[1]];
            let quadratic = c[3] * dx * dz;
            let linear = c[1] * dx + c[2] * dz + c[3] * (x * dz + z * dx);
            if quadratic < 0. {
                let t = -linear / (2. * quadratic);
                if t > 0. && t < 1. {
                    let point = [p[0] + t * dx, p[1] + t * dz];
                    let v = value(point);
                    if v > best.0 {
                        best = (v, point);
                    }
                }
            }
        }
        best
    }
}
struct Meter<'a, F: Fn() -> bool> {
    work: usize,
    limit: usize,
    cancel: &'a F,
}
impl<F: Fn() -> bool> Meter<'_, F> {
    fn spend(&mut self, n: usize) -> Result<(), Failure> {
        if (self.cancel)() {
            return Err(Failure::Cancelled);
        }
        self.work = self
            .work
            .checked_add(n)
            .filter(|v| *v <= self.limit)
            .ok_or(Failure::Budget)?;
        Ok(())
    }
}
struct Triangle {
    points: [Point; 3],
    base: [f64; 3],
    slope: Point,
    low: [usize; 2],
    high: [usize; 2],
    scale: f64,
    size: f64,
}
impl Triangle {
    fn new(world: [[f64; 3]; 3], grid: Grid<'_>) -> Result<Self, Failure> {
        if world.iter().flatten().any(|v| !v.is_finite()) {
            return Err(Failure::Invalid);
        }
        let size = f64::from(grid.size);
        let scale = (grid.resolution - 1) as f64 / size;
        let points = world.map(|p| [(p[0] + size * 0.5) * scale, (p[2] + size * 0.5) * scale]);
        if points
            .iter()
            .flatten()
            .any(|v| !v.is_finite() || *v < 0. || *v > (grid.resolution - 1) as f64)
        {
            return Err(Failure::Invalid);
        }
        let [p, q, r] = points;
        let ab = [q[0] - p[0], q[1] - p[1]];
        let ac = [r[0] - p[0], r[1] - p[1]];
        let det = ab[0] * ac[1] - ab[1] * ac[0];
        if det.abs() < 1e-10 {
            return Err(Failure::Invalid);
        }
        let dyb = world[1][1] - world[0][1];
        let dyc = world[2][1] - world[0][1];
        let slope = [
            (dyb * ac[1] - dyc * ab[1]) / det,
            (ab[0] * dyc - ac[0] * dyb) / det,
        ];
        let low = std::array::from_fn(|i| {
            points
                .iter()
                .map(|p| p[i])
                .fold(f64::INFINITY, f64::min)
                .floor() as usize
        });
        let high = std::array::from_fn(|i| {
            (points
                .iter()
                .map(|p| p[i])
                .fold(f64::NEG_INFINITY, f64::max)
                .floor() as usize)
                .min(grid.resolution - 2)
        });
        Ok(Self {
            points,
            base: [p[0], world[0][1], p[1]],
            slope,
            low,
            high,
            scale,
            size,
        })
    }
    fn road(&self, p: Point) -> f64 {
        self.base[1] + self.slope[0] * (p[0] - self.base[0]) + self.slope[1] * (p[1] - self.base[2])
    }
    fn world(&self, p: Point) -> Point {
        p.map(|v| v / self.scale - self.size * 0.5)
    }
    fn cells<F: Fn() -> bool>(
        &self,
        grid: Grid<'_>,
        meter: &mut Meter<'_, F>,
        mut visit: impl FnMut(Polygon, [f64; 4], Point) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        for z in self.low[1]..=self.high[1] {
            for x in self.low[0]..=self.high[0] {
                meter.spend(1)?;
                let mut polygon = Polygon::triangle(self.points);
                for (axis, bound, greater) in [
                    (0, x as f64, true),
                    (0, (x + 1) as f64, false),
                    (1, z as f64, true),
                    (1, (z + 1) as f64, false),
                ] {
                    polygon = polygon.clip(axis, bound, greater)?;
                    if polygon.len == 0 {
                        break;
                    }
                }
                if polygon.len == 0 {
                    continue;
                }
                let decode = |x: usize, z: usize| {
                    let value = grid.height[z * grid.resolution + x];
                    if !(0..=32766).contains(&value) {
                        return Err(Failure::Invalid);
                    }
                    Ok(f64::from(
                        grid.origin_y
                            + (value as f32 * path_terrain::SHORT_TO_FLOAT) * grid.vertical_span,
                    ))
                };
                let [a, b, c, d] = [
                    decode(x, z)?,
                    decode(x + 1, z)?,
                    decode(x, z + 1)?,
                    decode(x + 1, z + 1)?,
                ];
                visit(
                    polygon,
                    [
                        a - self.road([x as f64, z as f64]),
                        b - a - self.slope[0],
                        c - a - self.slope[1],
                        d - b - c + a,
                    ],
                    [x as f64, z as f64],
                )?;
            }
        }
        Ok(())
    }
}
/// Inputs remain immutable; failures discard all reports and witness proposals.
/// Reserve the four terrain samples of every grid cell intersecting pavement.
/// Mesh-query/control supports are added separately by the source-bound caller.
pub fn reserve_height_cells(
    input: Input<'_>,
    reservations: &mut crate::height_reservations::HeightReservations,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<usize, Failure> {
    let mut meter = Meter {
        work: 0,
        limit: maximum_work,
        cancel: &cancelled,
    };
    meter.spend(0)?;
    if maximum_work == 0
        || maximum_work > 10_000_000
        || !reservations.layout_matches(input.grid)
        || input.chunk.mesh.validate().is_err()
        || input.template.validate().is_err()
        || !input
            .chunk
            .mesh
            .triangles
            .len()
            .is_multiple_of(input.template.triangles.len())
    {
        return Err(Failure::Invalid);
    }
    for (index, face) in input.chunk.mesh.triangles.iter().enumerate() {
        meter.spend(1)?;
        let authored = input.template.triangles[index % input.template.triangles.len()];
        if authored
            .iter()
            .any(|&i| input.template.normals[i as usize][1] < 0.5)
        {
            continue;
        }
        let [a, b, c] = authored.map(|i| input.template.vertices[i as usize].map(f64::from));
        if ((b[0] - a[0]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[0] - a[0])).abs() < 1e-10 {
            continue;
        }
        let triangle = Triangle::new(
            face.map(|i| {
                std::array::from_fn(|k| {
                    f64::from(input.chunk.mesh.vertices[i as usize][k] + input.chunk.origin[k])
                })
            }),
            input.grid,
        )?;
        triangle.cells(input.grid, &mut meter, |_, _, offset| {
            let i = offset[1] as usize * input.grid.resolution + offset[0] as usize;
            for j in [
                i,
                i + 1,
                i + input.grid.resolution,
                i + input.grid.resolution + 1,
            ] {
                reservations.reserve(j).map_err(|_| Failure::Invalid)?;
            }
            Ok(())
        })?;
    }
    Ok(meter.work)
}
/// Inputs remain immutable; failures discard all reports and witness proposals.
pub fn audit(
    input: Input<'_>,
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Output, Failure> {
    let mut meter = Meter {
        work: 0,
        limit: settings.maximum_work,
        cancel: &cancelled,
    };
    meter.spend(0)?;
    settings.validate()?;
    let grid = input.grid;
    let chunk = input.chunk;
    let template = input.template;
    path_terrain::height_support(
        Layout {
            size: grid.size,
            resolution: grid.resolution,
            origin_y: grid.origin_y,
            vertical_span: grid.vertical_span,
        },
        [0.; 3],
    )
    .map_err(|_| Failure::Invalid)?;
    template.validate().map_err(|_| Failure::Invalid)?;
    chunk.mesh.validate().map_err(|_| Failure::Invalid)?;
    if grid.height.len() != grid.resolution * grid.resolution
        || !settings.tolerance.is_finite()
        || !(0.0001..=1.).contains(&settings.tolerance)
        || !settings.minimum_up_normal.is_finite()
        || !(0. ..=1.).contains(&settings.minimum_up_normal)
        || !(1..=10_000_000).contains(&settings.maximum_work)
        || !(1..=131072).contains(&settings.maximum_probes)
        || !(1..=32).contains(&chunk.repetitions)
        || chunk.origin.iter().any(|v| !v.is_finite())
        || chunk.mesh.vertices.len() != chunk.repetitions * template.vertices.len()
        || chunk.mesh.triangles.len() != chunk.repetitions * template.triangles.len()
    {
        return Err(Failure::Invalid);
    }
    let mut report = Report {
        engine: VERSION,
        driving_triangles: 0,
        cell_rectangle_tests: 0,
        continuous_intersections: 0,
        source_sampler_confirmed_intersections: 0,
        source_only_intersections: 0,
        maximum_continuous_terrain_excess: f64::NEG_INFINITY,
        tolerance: settings.tolerance,
        work: 0,
        passed: false,
        runtime_physics_validated: false,
    };
    let roundoff = 32.
        * f64::from(f32::EPSILON)
        * (f64::from(grid.origin_y).abs() + f64::from(grid.vertical_span).abs() + 1.);
    let mut probes = BTreeMap::<(usize, i64, i64), ClearanceProbe>::new();
    for (index, face) in chunk.mesh.triangles.iter().enumerate() {
        meter.spend(1)?;
        let authored = template.triangles[index % template.triangles.len()];
        let repetition = index / template.triangles.len();
        if face.map(|i| i as usize)
            != authored.map(|i| i as usize + repetition * template.vertices.len())
        {
            return Err(Failure::Invalid);
        }
        let [a, b, c] = authored.map(|i| template.vertices[i as usize].map(f64::from));
        let det = (b[0] - a[0]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[0] - a[0]);
        if det.abs() < 1e-10
            || authored
                .iter()
                .any(|&i| template.normals[i as usize][1] < settings.minimum_up_normal)
        {
            continue;
        }
        report.driving_triangles += 1;
        let triangle = Triangle::new(
            face.map(|i| {
                std::array::from_fn(|j| {
                    f64::from(chunk.mesh.vertices[i as usize][j] + chunk.origin[j])
                })
            }),
            grid,
        )?;
        let mut best = (f64::NEG_INFINITY, [0.; 2]);
        triangle.cells(grid, &mut meter, |polygon, coefficients, offset| {
            let candidate = polygon.maximum(coefficients, offset);
            if candidate.0 > best.0 {
                best = candidate
            }
            Ok(())
        })?;
        report.maximum_continuous_terrain_excess =
            report.maximum_continuous_terrain_excess.max(best.0);
        if best.0 <= settings.tolerance - roundoff {
            continue;
        }
        let position = triangle.world(best.1);
        let actual = f64::from(
            path_terrain::sample_height(grid, [position[0] as f32, 0., position[1] as f32])
                .map_err(|_| Failure::Invalid)?,
        ) - triangle.road(best.1);
        if best.0 <= settings.tolerance && actual <= settings.tolerance {
            continue;
        }
        report.continuous_intersections += usize::from(best.0 > settings.tolerance);
        report.source_sampler_confirmed_intersections += usize::from(actual > settings.tolerance);
        report.source_only_intersections +=
            usize::from(best.0 <= settings.tolerance && actual > settings.tolerance);
        let mut add = |position: Point, maximum: f32| -> Result<(), Failure> {
            let key = (
                index,
                (position[0] * 1_000_000.).round() as i64,
                (position[1] * 1_000_000.).round() as i64,
            );
            if let Some(p) = probes.get_mut(&key) {
                p.maximum_terrain_excess = p.maximum_terrain_excess.min(maximum);
            } else {
                if probes.len() == settings.maximum_probes {
                    return Err(Failure::Budget);
                }
                probes.insert(
                    key,
                    ClearanceProbe {
                        road: input.road,
                        chunk: input.chunk_index,
                        triangle: index,
                        position,
                        maximum_terrain_excess: maximum,
                    },
                );
            }
            Ok(())
        };
        add(position, (settings.tolerance * 0.5) as f32)?;
        triangle.cells(grid, &mut meter, |polygon, _, _| {
            for i in 0..polygon.len {
                let p = polygon.points[i];
                let q = polygon.points[(i + 1) % polygon.len];
                add(triangle.world(p), settings.tolerance as f32)?;
                add(
                    triangle.world([(p[0] + q[0]) * 0.5, (p[1] + q[1]) * 0.5]),
                    settings.tolerance as f32,
                )?;
            }
            Ok(())
        })?;
    }
    meter.spend(0)?;
    if report.driving_triangles == 0 {
        return Err(Failure::Invalid);
    }
    report.cell_rectangle_tests = meter.work - chunk.mesh.triangles.len();
    report.work = meter.work;
    report.passed =
        report.continuous_intersections == 0 && report.source_sampler_confirmed_intersections == 0;
    Ok(Output {
        report,
        probes: probes.into_values().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn settings() -> Settings {
        Settings {
            tolerance: 0.02,
            minimum_up_normal: 0.5,
            maximum_work: 1000,
            maximum_probes: 1000,
        }
    }
    fn mesh(y: f32) -> Template {
        Template {
            vertices: vec![[-500., y, -500.], [500., y, -500.], [0., y, 500.]],
            normals: vec![[0., 1., 0.]; 3],
            tangents: vec![[1., 0., 0., 1.]; 3],
            uv: vec![[0., 0.]; 3],
            triangles: vec![[0, 1, 2]],
            center: [0., y, 0.],
            extents: [500., 0., 500.],
        }
    }
    #[test]
    fn height_dependencies_cover_clipped_cells_without_reserving_the_entire_map() {
        let heights = vec![0; 25];
        let grid = Grid {
            size: 1000.,
            resolution: 5,
            origin_y: 0.,
            vertical_span: 1000.,
            height: &heights,
        };
        let mut template = mesh(0.);
        template.vertices = vec![[-400., 0., -400.], [-100., 0., -400.], [-400., 0., -100.]];
        template.center = [-250., 0., -250.];
        template.extents = [150., 0., 150.];
        let chunk = Chunk {
            first_repetition: 0,
            repetitions: 1,
            origin: [0.; 3],
            mesh: template.clone(),
            work: 0,
        };
        let input = || Input {
            grid,
            chunk: &chunk,
            template: &template,
            road: 0,
            chunk_index: 0,
        };
        let mut mask = crate::height_reservations::HeightReservations::new(grid, || false)
            .unwrap()
            .unwrap();
        let work = reserve_height_cells(input(), &mut mask, 10000, || false).unwrap();
        assert!(work > 0);
        assert!(mask.contains(0) && mask.contains(6) && mask.contains(12));
        assert!(!mask.contains(24));
        assert!(mask.samples() < heights.len());
        let mut short = crate::height_reservations::HeightReservations::new(grid, || false)
            .unwrap()
            .unwrap();
        assert_eq!(
            reserve_height_cells(input(), &mut short, work - 1, || false),
            Err(Failure::Budget)
        );
        assert_eq!(
            reserve_height_cells(input(), &mut short, 10000, || true),
            Err(Failure::Cancelled)
        );
    }
    #[test]
    fn hidden_peak_produces_bounded_unique_guards_and_clear_surface_passes() {
        let mut heights = vec![0; 9];
        heights[4] = 100;
        let template = mesh(0.);
        let chunk = Chunk {
            first_repetition: 0,
            repetitions: 1,
            origin: [0.; 3],
            mesh: template.clone(),
            work: 0,
        };
        let input = || Input {
            grid: Grid {
                size: 1000.,
                resolution: 3,
                origin_y: 0.,
                vertical_span: 1000.,
                height: &heights,
            },
            chunk: &chunk,
            template: &template,
            road: 7,
            chunk_index: 2,
        };
        let out = audit(input(), settings(), || false).unwrap();
        assert_eq!(out.report.continuous_intersections, 1);
        assert_eq!(out.report.source_sampler_confirmed_intersections, 1);
        assert!((out.report.maximum_continuous_terrain_excess - 3.051944).abs() < 0.001);
        assert!(!out.report.passed && !out.report.runtime_physics_validated);
        assert!(
            out.probes
                .iter()
                .any(|p| p.position == [0., 0.] && p.maximum_terrain_excess == 0.01)
        );
        assert!(out.probes.len() > 3 && out.probes.iter().all(|p| p.road == 7 && p.chunk == 2));
        assert_eq!(
            audit(
                input(),
                Settings {
                    maximum_work: 1,
                    ..settings()
                },
                || false
            )
            .err(),
            Some(Failure::Budget)
        );
        assert_eq!(
            audit(
                input(),
                Settings {
                    maximum_probes: 1,
                    ..settings()
                },
                || false
            )
            .err(),
            Some(Failure::Budget)
        );
        assert_eq!(
            audit(input(), settings(), || true).err(),
            Some(Failure::Cancelled)
        );
        let flat = vec![0; 9];
        let raised = Chunk {
            mesh: mesh(1.),
            ..chunk
        };
        let clean = audit(
            Input {
                grid: Grid {
                    height: &flat,
                    ..input().grid
                },
                chunk: &raised,
                ..input()
            },
            settings(),
            || false,
        )
        .unwrap();
        assert!(clean.report.passed && clean.probes.is_empty());
        assert_eq!(clean.report.maximum_continuous_terrain_excess, -1.);
    }
    #[test]
    fn edge_quadratic_maximum_and_exact_contact_clipping() {
        let polygon = Polygon::triangle([[0., 0.], [1., 0.], [0., 1.]]);
        let maximum = polygon.maximum([0., 0., 0., 1.], [0.; 2]);
        assert_eq!(maximum, (0.25, [0.5, 0.5]));
        let contact = polygon.clip(0, 1., true).unwrap();
        assert!(contact.len > 0);
        assert!(contact.points[..contact.len].iter().all(|p| *p == [1., 0.]));
    }
}
