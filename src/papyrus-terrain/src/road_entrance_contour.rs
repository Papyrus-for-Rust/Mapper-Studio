//! Bounded native-height contour guidance for a fixed monument entrance.
//! A proposal is not a road certificate: source paint, bed fitting, meshes,
//! retained terrain, authored sockets and complete route audits still apply.
use crate::{
    path_geometry::{self, Path},
    path_terrain::{self, Grid},
};
use serde::Serialize;
use std::{cmp::Ordering, collections::BinaryHeap};
type V = [f32; 3];
pub const VERSION: &str = "papyrus-entrance-contour-guidance-1";
#[derive(Clone, Copy)]
pub struct Settings {
    pub maximum_grade: f64,
    /// Conservative grid guidance; the shaped curve uses maximum_grade.
    pub search_grade: f64,
    pub radius_cells: usize,
    pub maximum_expansions: usize,
    pub maximum_work: usize,
    /// Read from the selected source adapter, rather than a user smoothing toggle.
    pub shaping: path_geometry::Settings,
}
pub struct Input<'a> {
    pub controls: &'a [V],
    pub pinned: &'a [bool],
    /// Inclusive start of the replacement tail; the prefix and both ends stay exact.
    pub tail_start: usize,
    pub grid: Grid<'a>,
    pub topology: &'a [i32],
    pub surface_resolution: usize,
    pub protected_topology: i32,
}
#[derive(Debug, PartialEq)]
pub enum Failure {
    Invalid,
    Budget,
    Geometry,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub work: usize,
    pub expansions: usize,
    pub height_queries: usize,
    pub raw_controls: usize,
    pub shaped_controls: usize,
    pub candidates: usize,
    pub maximum_protected_ground_grade: f64,
}
pub struct Output {
    pub controls: Vec<V>,
    pub raw: Vec<V>,
    pub report: Report,
}
pub struct Search {
    pub proposal: Option<Output>,
    pub report: Report,
}
#[derive(Default)]
struct Meter {
    work: usize,
    queries: usize,
    maximum: usize,
}
impl Meter {
    fn spend(&mut self, n: usize) -> Result<(), Failure> {
        self.work = self
            .work
            .checked_add(n)
            .filter(|n| *n <= self.maximum)
            .ok_or(Failure::Budget)?;
        Ok(())
    }
    fn height(&mut self, grid: Grid<'_>, p: V) -> Result<f32, Failure> {
        self.spend(8)?;
        self.queries += 1;
        path_terrain::sample_height(grid, p).map_err(|_| Failure::Invalid)
    }
}
#[derive(Clone, Copy)]
struct Queue {
    estimate: f64,
    cost: f64,
    node: usize,
}
impl PartialEq for Queue {
    fn eq(&self, o: &Self) -> bool {
        self.estimate.to_bits() == o.estimate.to_bits()
            && self.cost.to_bits() == o.cost.to_bits()
            && self.node == o.node
    }
}
impl Eq for Queue {}
impl PartialOrd for Queue {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Queue {
    fn cmp(&self, o: &Self) -> Ordering {
        o.estimate
            .total_cmp(&self.estimate)
            .then_with(|| o.cost.total_cmp(&self.cost))
            .then_with(|| o.node.cmp(&self.node))
    }
}
fn run(a: V, b: V) -> f64 {
    f64::from(a[0] - b[0]).hypot(f64::from(a[2] - b[2]))
}
fn edge(grid: Grid<'_>, a: V, b: V, m: &mut Meter) -> Result<(f64, f64), Failure> {
    let length = run(a, b);
    let steps = (length / 0.25).ceil().max(1.) as usize;
    if steps > 256 {
        return Err(Failure::Invalid);
    }
    let mut previous = a;
    let mut peak = 0_f64;
    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        let mut p = std::array::from_fn(|k| a[k] + (b[k] - a[k]) * t);
        p[1] = m.height(grid, p)?;
        let span = run(previous, p);
        if span > 1e-6 {
            peak = peak.max(f64::from(p[1] - previous[1]).abs() / span);
        }
        previous = p;
    }
    Ok((length, peak))
}
struct Lattice {
    x0: usize,
    z0: usize,
    width: usize,
    height: usize,
    heights: Vec<Option<f32>>,
}
impl Lattice {
    fn index(&self, x: usize, z: usize) -> Option<usize> {
        if x < self.x0 || z < self.z0 || x >= self.x0 + self.width || z >= self.z0 + self.height {
            None
        } else {
            Some((z - self.z0) * self.width + x - self.x0)
        }
    }
    fn point(&mut self, i: usize, grid: Grid<'_>, m: &mut Meter) -> Result<V, Failure> {
        let x = self.x0 + i % self.width;
        let z = self.z0 + i / self.width;
        let mut p = [
            x as f32 / (grid.resolution - 1) as f32 * grid.size - grid.size * 0.5,
            0.,
            z as f32 / (grid.resolution - 1) as f32 * grid.size - grid.size * 0.5,
        ];
        p[1] = if let Some(h) = self.heights[i] {
            h
        } else {
            let h = m.height(grid, p)?;
            self.heights[i] = Some(h);
            h
        };
        Ok(p)
    }
}
fn protected(input: &Input<'_>, p: V) -> bool {
    let n = input.surface_resolution;
    let index = |v: f32| {
        (((v + input.grid.size * 0.5) * (1. / input.grid.size)) * n as f32)
            .clamp(0., (n - 1) as f32) as usize
    };
    input.topology[index(p[2]) * n + index(p[0])] & input.protected_topology != 0
}
fn score(
    input: &Input<'_>,
    controls: &[V],
    m: &mut Meter,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<f64>, Failure> {
    m.spend(controls.len() * 2)?;
    let mut points = controls.to_vec();
    for p in &mut points {
        p[1] = m.height(input.grid, *p)?;
    }
    let Some(path) = Path::from_controls(&points, m.maximum - m.work, cancelled)
        .map_err(|_| Failure::Geometry)?
    else {
        return Ok(None);
    };
    m.spend(path.work())?;
    let steps = (path.length() / 0.25).ceil().max(1.) as usize;
    if steps > 65536 {
        return Err(Failure::Budget);
    }
    let mut previous = None;
    let mut peak = 0_f64;
    for i in 0..=steps {
        if i % 64 == 0 && cancelled() {
            return Ok(None);
        }
        m.spend(32)?;
        let mut p = path
            .point(path.length() * i as f32 / steps as f32)
            .map_err(|_| Failure::Geometry)?;
        p[1] = m.height(input.grid, p)?;
        let fixed = protected(input, p);
        if let Some((q, previous_fixed)) = previous {
            let span = run(q, p);
            if (fixed || previous_fixed) && span > 1e-6 {
                peak = peak.max(f64::from(p[1] - q[1]).abs() / span);
            }
        }
        previous = Some((p, fixed));
    }
    Ok(Some(peak))
}
/// Fine-grid edges are checked on unchanged ground, then source shaping is
/// applied once. At most three interior shaped controls may be removed to keep
/// all existing indices. Every subset is scored as a whole Hermite road, not as
/// a linear grid route. No unsafe code, terrain writes, or attachment renumbering.
pub fn propose(
    input: Input<'_>,
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Search>, Failure> {
    if cancelled() {
        return Ok(None);
    }
    let len = input.controls.len();
    let n = input.grid.resolution;
    let start = input.tail_start;
    if !(4..=512).contains(&len)
        || start == 0
        || start + 3 >= len
        || len - start > 16
        || input.pinned.len() != len
        || input
            .controls
            .iter()
            .flatten()
            .any(|v| !v.is_finite() || v.abs() > 10000.)
        || input
            .controls
            .iter()
            .any(|p| p[0].abs() > input.grid.size * 0.5 || p[2].abs() > input.grid.size * 0.5)
        || !(2..=4097).contains(&n)
        || input.grid.height.len() != n * n
        || !input.grid.size.is_finite()
        || input.grid.size <= 0.
        || input.grid.size / (n - 1) as f32 > 4.
        || !input.grid.origin_y.is_finite()
        || !input.grid.vertical_span.is_finite()
        || input.grid.vertical_span <= 0.
        || !(2..=4096).contains(&input.surface_resolution)
        || input.topology.len() != input.surface_resolution * input.surface_resolution
        || input.protected_topology == 0
        || !settings.maximum_grade.is_finite()
        || !(0.001..=1.).contains(&settings.maximum_grade)
        || !settings.search_grade.is_finite()
        || settings.search_grade <= 0.
        || settings.search_grade > settings.maximum_grade
        || !(1..=64).contains(&settings.radius_cells)
        || !(1..=16384).contains(&settings.maximum_expansions)
        || !(1..=32000000).contains(&settings.maximum_work)
        || input.pinned[start + 1..len - 1].iter().any(|v| *v)
    {
        return Err(Failure::Invalid);
    }
    let mut m = Meter {
        maximum: settings.maximum_work,
        ..Default::default()
    };
    m.spend(len * 3)?;
    let mut report = Report {
        engine: VERSION,
        work: 0,
        expansions: 0,
        height_queries: 0,
        raw_controls: 0,
        shaped_controls: 0,
        candidates: 0,
        maximum_protected_ground_grade: 0.,
    };
    let finish = |mut report: Report, m: &Meter, proposal| {
        report.work = m.work;
        report.height_queries = m.queries;
        Search { proposal, report }
    };
    let cell = |p: V| {
        [
            (((p[0] + input.grid.size * 0.5) / input.grid.size) * (n - 1) as f32)
                .round()
                .clamp(0., (n - 1) as f32) as usize,
            (((p[2] + input.grid.size * 0.5) / input.grid.size) * (n - 1) as f32)
                .round()
                .clamp(0., (n - 1) as f32) as usize,
        ]
    };
    let a = input.controls[start];
    let b = input.controls[len - 1];
    let ca = cell(a);
    let cb = cell(b);
    let r = settings.radius_cells;
    let x0 = ca[0].min(cb[0]).saturating_sub(r);
    let z0 = ca[1].min(cb[1]).saturating_sub(r);
    let width = (ca[0].max(cb[0]) + r).min(n - 1) - x0 + 1;
    let height = (ca[1].max(cb[1]) + r).min(n - 1) - z0 + 1;
    let count = width
        .checked_mul(height)
        .filter(|v| *v <= 65536)
        .ok_or(Failure::Budget)?;
    m.spend(count * 3)?;
    let mut lattice = Lattice {
        x0,
        z0,
        width,
        height,
        heights: vec![None; count],
    };
    let mut distance = vec![f64::INFINITY; count];
    let mut parent = vec![usize::MAX; count];
    let mut goals = vec![false; count];
    let mut queue = BinaryHeap::new();
    let mut a = a;
    let mut b = b;
    a[1] = m.height(input.grid, a)?;
    b[1] = m.height(input.grid, b)?;
    for (center, endpoint, is_start) in [(ca, a, true), (cb, b, false)] {
        for dz in -3_isize..=3 {
            for dx in -3_isize..=3 {
                let Some(i) = center[0]
                    .checked_add_signed(dx)
                    .zip(center[1].checked_add_signed(dz))
                    .and_then(|(x, z)| lattice.index(x, z))
                else {
                    continue;
                };
                let p = lattice.point(i, input.grid, &mut m)?;
                let (length, grade) = edge(input.grid, endpoint, p, &mut m)?;
                if grade > settings.search_grade {
                    continue;
                }
                if is_start {
                    distance[i] = length;
                    queue.push(Queue {
                        estimate: length + run(p, b),
                        cost: length,
                        node: i,
                    });
                } else {
                    goals[i] = true;
                }
            }
        }
    }
    let mut found = None;
    while let Some(q) = queue.pop() {
        if cancelled() {
            return Ok(None);
        }
        m.spend(24)?;
        if q.cost != distance[q.node] {
            continue;
        }
        if report.expansions >= settings.maximum_expansions {
            break;
        }
        report.expansions += 1;
        if goals[q.node] {
            found = Some(q.node);
            break;
        }
        let p = lattice.point(q.node, input.grid, &mut m)?;
        let x = x0 + q.node % width;
        let z = z0 + q.node / width;
        for dz in -1_isize..=1 {
            for dx in -1_isize..=1 {
                if dx == 0 && dz == 0 {
                    continue;
                }
                m.spend(4)?;
                let Some(i) = x
                    .checked_add_signed(dx)
                    .zip(z.checked_add_signed(dz))
                    .and_then(|(x, z)| lattice.index(x, z))
                else {
                    continue;
                };
                let next = lattice.point(i, input.grid, &mut m)?;
                let (length, grade) = edge(input.grid, p, next, &mut m)?;
                if grade > settings.search_grade {
                    continue;
                }
                let cost = q.cost + length * (1. + 2. * (grade / settings.maximum_grade).powi(4));
                if cost >= distance[i] {
                    continue;
                }
                distance[i] = cost;
                parent[i] = q.node;
                queue.push(Queue {
                    estimate: cost + run(next, b),
                    cost,
                    node: i,
                });
            }
        }
    }
    let Some(mut i) = found else {
        return Ok(Some(finish(report, &m, None)));
    };
    let mut raw = vec![b];
    loop {
        if cancelled() {
            return Ok(None);
        }
        m.spend(4)?;
        raw.push(lattice.point(i, input.grid, &mut m)?);
        i = parent[i];
        if i == usize::MAX {
            break;
        }
        if raw.len() > 4096 {
            return Err(Failure::Budget);
        }
    }
    raw.push(a);
    raw.reverse();
    report.raw_controls = raw.len();
    let mut shaping = settings.shaping;
    shaping.maximum_work = (m.maximum - m.work).min(shaping.maximum_work);
    let Some(shaped) =
        path_geometry::build(&raw, shaping, None, &cancelled).map_err(|_| Failure::Geometry)?
    else {
        return Ok(None);
    };
    m.spend(shaped.work())?;
    let mut tail = shaped.points().to_vec();
    let target = len - start;
    report.shaped_controls = tail.len();
    if tail.len() < target || tail.len() > target + 3 || tail.len() > 18 {
        return Ok(Some(finish(report, &m, None)));
    }
    tail[0] = input.controls[start];
    let last = tail.len() - 1;
    tail[last] = input.controls[len - 1];
    let remove = tail.len() - target;
    let interior = tail.len() - 2;
    for mask in 0_u32..1_u32 << interior {
        if cancelled() {
            return Ok(None);
        }
        m.spend(1)?;
        if mask.count_ones() as usize != remove {
            continue;
        }
        let mut controls = input.controls[..start].to_vec();
        controls.push(tail[0]);
        for (i, &p) in tail[1..last].iter().enumerate() {
            if mask & (1 << i) == 0 {
                controls.push(p);
            }
        }
        controls.push(tail[last]);
        report.candidates += 1;
        let Some(peak) = score(&input, &controls, &mut m, &cancelled)? else {
            return Ok(None);
        };
        report.maximum_protected_ground_grade = peak;
        if peak > settings.maximum_grade * 0.95 {
            continue;
        }
        if controls
            .iter()
            .zip(input.controls)
            .zip(input.pinned)
            .any(|((a, b), fixed)| {
                *fixed && a.iter().zip(b).any(|(a, b)| a.to_bits() != b.to_bits())
            })
        {
            return Err(Failure::Invalid);
        }
        report.work = m.work;
        report.height_queries = m.queries;
        let output = Output {
            controls,
            raw,
            report: Report { ..report },
        };
        return Ok(Some(finish(report, &m, Some(output))));
    }
    Ok(Some(finish(report, &m, None)))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn settings() -> Settings {
        Settings {
            maximum_grade: 0.2,
            search_grade: 0.1,
            radius_cells: 8,
            maximum_expansions: 4096,
            maximum_work: 2_000_000,
            shaping: path_geometry::Settings {
                horizontal_iterations: 4,
                vertical_iterations: 16,
                resample_distance: 7.5,
                maximum_work: 100_000,
            },
        }
    }
    fn controls() -> Vec<V> {
        (0..8).map(|i| [-40. + i as f32 * 9., 0., 0.]).collect()
    }
    #[test]
    fn exact_prefix_and_authored_endpoints_survive_deterministic_shaped_guidance() {
        let h = vec![0; 129 * 129];
        let topo = vec![1024; 128 * 128];
        let points = controls();
        let pins = [true, false, false, true, false, false, false, true];
        let make = || Input {
            controls: &points,
            pinned: &pins,
            tail_start: 3,
            grid: Grid {
                size: 128.,
                resolution: 129,
                origin_y: 0.,
                vertical_span: 100.,
                height: &h,
            },
            topology: &topo,
            surface_resolution: 128,
            protected_topology: 1024,
        };
        let a = propose(make(), settings(), || false).unwrap().unwrap();
        let b = propose(make(), settings(), || false).unwrap().unwrap();
        let a = a.proposal.unwrap();
        let b = b.proposal.unwrap();
        assert_eq!(a.controls, b.controls);
        assert_eq!(a.controls.len(), points.len());
        assert_eq!(a.controls[..=3], points[..=3]);
        assert_eq!(a.controls.last(), points.last());
        assert_eq!(a.report.maximum_protected_ground_grade, 0.);
        assert!(a.report.work <= settings().maximum_work);
        assert!(a.report.height_queries > 0);
        assert!(a.report.expansions > 0);
    }
    #[test]
    fn cancellation_work_exhaustion_and_interior_attachments_publish_no_proposal() {
        use std::cell::Cell;
        let h = vec![0; 129 * 129];
        let topo = vec![1024; 128 * 128];
        let points = controls();
        let pins = [true, false, false, true, false, false, false, true];
        let make = |pins| Input {
            controls: &points,
            pinned: pins,
            tail_start: 3,
            grid: Grid {
                size: 128.,
                resolution: 129,
                origin_y: 0.,
                vertical_span: 100.,
                height: &h,
            },
            topology: &topo,
            surface_resolution: 128,
            protected_topology: 1024,
        };
        let polls = Cell::new(0);
        assert!(
            propose(make(&pins), settings(), || {
                polls.set(polls.get() + 1);
                polls.get() > 6
            })
            .unwrap()
            .is_none()
        );
        let mut small = settings();
        small.maximum_work = 128;
        assert!(matches!(
            propose(make(&pins), small, || false),
            Err(Failure::Budget)
        ));
        let mut attached = pins;
        attached[5] = true;
        assert!(matches!(
            propose(make(&attached), settings(), || false),
            Err(Failure::Invalid)
        ));
        assert_eq!(points, controls());
    }
    #[test]
    fn a_linear_grade_pass_does_not_certify_the_final_hermite_curve() {
        let mut h = vec![0; 65 * 65];
        for z in 0..65 {
            for x in 0..65 {
                let y = (x as f32 - 32.) * 0.25;
                h[z * 65 + x] = ((y + 50.) / 100. * 32766.).round() as i16;
            }
        }
        let topo = vec![1024; 64 * 64];
        let points = [[-10., 0., -10.], [0., 0., 10.], [10., 0., -10.]];
        let grid = Grid {
            size: 64.,
            resolution: 65,
            origin_y: -50.,
            vertical_span: 100.,
            height: &h,
        };
        let mut m = Meter {
            maximum: 1_000_000,
            ..Default::default()
        };
        for pair in points.windows(2) {
            let mut a = pair[0];
            let mut b = pair[1];
            a[1] = m.height(grid, a).unwrap();
            b[1] = m.height(grid, b).unwrap();
            assert!(edge(grid, a, b, &mut m).unwrap().1 < 0.2);
        }
        let input = Input {
            controls: &points,
            pinned: &[true, false, true],
            tail_start: 1,
            grid,
            topology: &topo,
            surface_resolution: 64,
            protected_topology: 1024,
        };
        assert!(score(&input, &points, &mut m, &|| false).unwrap().unwrap() > 0.24);
    }
    #[test]
    fn steep_barrier_and_bounded_expansion_do_not_turn_into_accepted_routes() {
        let mut h = vec![0; 129 * 129];
        for z in 0..129 {
            for x in 65..129 {
                h[z * 129 + x] = 16000;
            }
        }
        let topo = vec![1024; 128 * 128];
        let points = controls();
        let pins = [true, false, false, true, false, false, false, true];
        let input = Input {
            controls: &points,
            pinned: &pins,
            tail_start: 3,
            grid: Grid {
                size: 128.,
                resolution: 129,
                origin_y: 0.,
                vertical_span: 100.,
                height: &h,
            },
            topology: &topo,
            surface_resolution: 128,
            protected_topology: 1024,
        };
        let mut small = settings();
        small.maximum_expansions = 100;
        let result = propose(input, small, || false).unwrap().unwrap();
        assert!(result.proposal.is_none());
        assert!(result.report.expansions <= 100);
        assert!(result.report.work <= small.maximum_work);
    }
}
