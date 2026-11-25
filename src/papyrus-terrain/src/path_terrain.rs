//! Source-style terrain sweeps with sparse frozen-per-path writes.
use crate::{path_geometry::Path, rail_reservations::QuadCoverage};
pub const VERSION: &str = "papyrus-path-terrain-sparse-1";
pub(crate) const SHORT_TO_FLOAT: f32 = 3.051944E-5;
pub(crate) fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0., 1.)
}
pub(crate) fn inverse(a: f32, b: f32, v: f32) -> f32 {
    if a == b {
        0.
    } else {
        ((v - a) / (b - a)).clamp(0., 1.)
    }
}
pub(crate) fn smooth(a: f32, b: f32, t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    let t = -2. * t * t * t + 3. * t * t;
    b * t + a * (1. - t)
}
pub(crate) fn magnitude(v: [f32; 3]) -> f32 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}
pub(crate) fn distance_xz(a: [f32; 3], b: [f32; 3]) -> f32 {
    let x = a[0] - b[0];
    let z = a[2] - b[2];
    (x * x + z * z).sqrt()
}
/// The inspected Line.ClosestPoint2D uses a 3D unit direction, a horizontal
/// clamp length, then a 3D/horizontal length factor. Standard segment projection
/// gives different answers for slopes. Vertical lines are rejected by the sweep.
#[derive(Clone, Copy)]
pub(crate) struct SweepLine {
    origin: [f32; 3],
    projection: Option<Projection>,
}

#[derive(Clone, Copy)]
struct Projection {
    direction: [f32; 3],
    horizontal: f32,
    length_ratio: f32,
}

impl SweepLine {
    pub(crate) fn new(a: [f32; 3], tangent: [f32; 3], spacing: f32) -> Self {
        let b = std::array::from_fn(|i| a[i] + tangent[i] * spacing);
        Self::between(a, b)
    }

    fn between(a: [f32; 3], b: [f32; 3]) -> Self {
        let v = std::array::from_fn(|i| b[i] - a[i]);
        let length = magnitude(v);
        let projection = if length < 0.001 {
            None
        } else {
            let horizontal = (v[0] * v[0] + v[2] * v[2]).sqrt();
            Some(Projection {
                direction: v.map(|v| v / length),
                horizontal,
                length_ratio: length / horizontal,
            })
        };
        Self { origin: a, projection }
    }

    fn along(self, p: [f32; 3]) -> f32 {
        self.projection.map_or(0., |projection| {
            let a = self.origin;
            let direction = projection.direction;
            let dot = ((p[0] - a[0]) * direction[0] + (p[2] - a[2]) * direction[2])
                .clamp(0., projection.horizontal);
            dot * projection.length_ratio
        })
    }

    fn axis(self, coordinate: usize, along: f32) -> f32 {
        self.projection.map_or(self.origin[coordinate], |projection| {
            self.origin[coordinate] + along * projection.direction[coordinate]
        })
    }

    fn distance(self, p: [f32; 3]) -> (f32, f32) {
        let along = self.along(p);
        let x = p[0] - self.axis(0, along);
        let z = p[2] - self.axis(2, along);
        ((x * x + z * z).sqrt(), along)
    }
}

#[inline]
pub(crate) fn nearest_sweep_height(lines: [SweepLine; 3], p: [f32; 3]) -> (f32, f32) {
    let [(da, a), (db, b), (dc, c)] = lines.map(|line| line.distance(p));
    // Compare rounded distances in source order, including exact ties.
    if db <= da && db <= dc {
        (db, lines[1].axis(1, b))
    } else if da <= dc {
        (da, lines[0].axis(1, a))
    } else {
        (dc, lines[2].axis(1, c))
    }
}

#[cfg(test)]
pub(crate) fn closest([a, b]: [[f32; 3]; 2], p: [f32; 3]) -> [f32; 3] {
    let line = SweepLine::between(a, b);
    let along = line.along(p);
    std::array::from_fn(|i| line.axis(i, along))
}
pub(crate) fn blend(source: i16, target: f32, opacity: f32) -> i16 {
    blend_observed(source, target, opacity).0
}
fn blend_observed(source: i16, target: f32, opacity: f32) -> (i16, f32) {
    let h = smooth(source as f32 * SHORT_TO_FLOAT, target, opacity);
    let encoded = h * 32766.;
    ((encoded + 0.5) as i16, encoded)
}
/// One active source write before the path's pending grid is committed.
/// `raw_encoded` retains the blend before nearest-short quantization.
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct CellWrite {
    pub cell: usize,
    pub opacity: f32,
    pub before: i16,
    pub target_normalized: f32,
    pub raw_encoded: f32,
    pub after: i16,
}
#[derive(Clone, Copy)]
pub struct Grid<'a> {
    pub size: f32,
    pub resolution: usize,
    pub origin_y: f32,
    pub vertical_span: f32,
    pub height: &'a [i16],
}
/// Inspected HeightMapData.GetHeight01 arithmetic followed by world conversion.
/// Source fractional coordinates are formed before integer indices are clamped.
pub fn sample_height(grid: Grid<'_>, position: [f32; 3]) -> Result<f32, String> {
    if grid.height.len() != grid.resolution.saturating_mul(grid.resolution) {
        return Err("Invalid height-sampling grid length".into());
    }
    sample_height_with(
        Layout {
            size: grid.size,
            resolution: grid.resolution,
            origin_y: grid.origin_y,
            vertical_span: grid.vertical_span,
        },
        position,
        |i| grid.height.get(i).copied(),
    )
}
/// Direct normalized sampling used by source procgen cells. Avoid converting a
/// cell UV to world and back, which adds an unnecessary f32 normalization step.
pub fn sample_height_uv(grid: Grid<'_>, uv: [f32; 2]) -> Result<f32, String> {
    if grid.height.len() != grid.resolution.saturating_mul(grid.resolution) {
        return Err("Invalid height-sampling grid length".into());
    }
    let layout = Layout {
        size: grid.size,
        resolution: grid.resolution,
        origin_y: grid.origin_y,
        vertical_span: grid.vertical_span,
    };
    validate_layout(layout)?;
    if uv.iter().any(|v| !v.is_finite() || v.abs() > 10000.) {
        return Err("Invalid normalized height coordinate".into());
    }
    interpolate(
        layout,
        support_at(
            layout.resolution,
            uv.map(|v| v * (layout.resolution - 1) as f32),
        ),
        |i| grid.height.get(i).copied(),
    )
}
#[derive(Clone, Copy)]
pub struct Layout {
    pub size: f32,
    pub resolution: usize,
    pub origin_y: f32,
    pub vertical_span: f32,
}
/// World-space size of one supported encoded height unit.
pub fn encoded_height_quantum(grid: Layout) -> Result<f32, String> {
    height_support(grid, [0.; 3])?;
    Ok(SHORT_TO_FLOAT * grid.vertical_span)
}
#[derive(Clone, Copy, Debug)]
pub struct HeightSupport {
    pub indices: [usize; 4],
    pub fractions: [f32; 2],
}
impl HeightSupport {
    /// Samples that must remain fixed to preserve the exact f32 height query.
    /// A zero interpolation fraction makes the corresponding branch inert.
    /// Keep branches at fraction one: subtraction/addition rounding can still
    /// depend on their nominally zero-weight samples in the source arithmetic.
    pub fn pinned_indices(self) -> impl Iterator<Item = usize> {
        let [x, z] = self.fractions;
        self.indices
            .into_iter()
            .enumerate()
            .filter_map(move |(i, index)| {
                let used = (i % 2 == 0 || x != 0.) && (i < 2 || z != 0.);
                used.then_some(index)
            })
    }
    /// Real-valued coefficients for a provisional linear model. The exact f32
    /// sampler remains authoritative after a proposed grid is quantized.
    pub fn weights(self) -> [f64; 4] {
        let [x, z] = self.fractions.map(f64::from);
        [(1. - x) * (1. - z), x * (1. - z), (1. - x) * z, x * z]
    }
}
/// Same owned height sampler over a lazy/private raster, without copying it.
pub fn sample_height_with(
    grid: Layout,
    position: [f32; 3],
    read: impl FnMut(usize) -> Option<i16>,
) -> Result<f32, String> {
    let support = height_support(grid, position)?;
    interpolate(grid, support, read)
}
fn interpolate(
    grid: Layout,
    support: HeightSupport,
    mut read: impl FnMut(usize) -> Option<i16>,
) -> Result<f32, String> {
    let [tx, tz] = support.fractions;
    let mut samples = [0_i16; 4];
    for (out, index) in samples.iter_mut().zip(support.indices) {
        *out = read(index)
            .filter(|v| (0..=32766).contains(v))
            .ok_or("Invalid sampled terrain encoding")?;
    }
    let at = |i: usize| samples[i] as f32 * SHORT_TO_FLOAT;
    let low = (at(1) - at(0)) * tx + at(0);
    let value = ((at(3) - at(2)) * tx + at(2) - low) * tz + low;
    Ok(grid.origin_y + value * grid.vertical_span)
}
pub fn height_support(grid: Layout, position: [f32; 3]) -> Result<HeightSupport, String> {
    validate_layout(grid)?;
    if position.iter().any(|v| !v.is_finite() || v.abs() > 10000.) {
        return Err("Invalid height-sampling position".into());
    }
    Ok(support_at(
        grid.resolution,
        [position[0], position[2]]
            .map(|v| ((v + grid.size * 0.5) * (1. / grid.size)) * (grid.resolution - 1) as f32),
    ))
}
fn validate_layout(grid: Layout) -> Result<(), String> {
    if !(2..=4097).contains(&grid.resolution)
        || !grid.size.is_finite()
        || !(1. ..=6000.).contains(&grid.size)
        || !grid.origin_y.is_finite()
        || grid.origin_y.abs() > 10000.
        || !grid.vertical_span.is_finite()
        || !(1. ..=20000.).contains(&grid.vertical_span)
    {
        return Err("Invalid height-sampling grid or position".into());
    }
    Ok(())
}
fn support_at(n: usize, [x, z]: [f32; 2]) -> HeightSupport {
    let (xi, zi) = (x as i32, z as i32);
    let (tx, tz) = ((x - xi as f32).clamp(0., 1.), (z - zi as f32).clamp(0., 1.));
    let (a, b) = (
        xi.clamp(0, (n - 1) as i32) as usize,
        zi.clamp(0, (n - 1) as i32) as usize,
    );
    let c = usize::from(x < (n - 1) as f32);
    let d = if z < (n - 1) as f32 { n } else { 0 };
    let i = b * n + a;
    HeightSupport {
        indices: [i, i + c, i + d, i + d + c],
        fractions: [tx, tz],
    }
}
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Shape {
    pub width: f32,
    pub padding: [f32; 2],
    pub fade: [f32; 2],
}
impl Shape {
    pub fn validate(self) -> Result<(), String> {
        if !self.width.is_finite()
            || !(0.0001..=10000.).contains(&self.width)
            || self
                .padding
                .into_iter()
                .chain(self.fade)
                .any(|v| !v.is_finite() || !(0. ..=256.).contains(&v))
        {
            return Err("Invalid terrain sweep shape".into());
        }
        Ok(())
    }
}
#[derive(Clone, Copy)]
pub struct Settings {
    pub sample_spacing: f32,
    pub intensity: f32,
    pub fade_multiplier: f32,
}
pub struct Sample {
    pub radius: f32,
    pub depth: f32,
    pub inner_scale: f32,
}
#[derive(Debug, Default, serde::Serialize)]
pub struct SweepReport {
    pub sweeps: usize,
    pub covered_visits: usize,
    pub height_writes: usize,
    pub committed_samples: usize,
    pub work: usize,
}
#[derive(Debug, serde::Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub path_passes: usize,
    pub sweeps: usize,
    pub height_writes: usize,
    pub committed_samples: usize,
    pub changed_samples: usize,
    pub maximum_raise: f32,
    pub maximum_lower: f32,
    pub full_grid_copies: usize,
    pub work: usize,
}
pub struct Output {
    pub height: Vec<i16>,
    pub report: Report,
}
/// Every sweep reads `current` and writes pending values. Only touched cells are
/// committed at the end of that path, matching Push/Pop without a full copy per
/// path. An errored/cancelled frame is poisoned and cannot produce an output.
pub struct Frame<'a> {
    grid: Grid<'a>,
    cell_centers: Vec<f32>,
    current: Vec<i16>,
    pending: Vec<i16>,
    dirty: Vec<u64>,
    touched: Vec<u32>,
    used: usize,
    limit: usize,
    poisoned: bool,
    passes: usize,
    sweeps: usize,
    writes: usize,
    committed: usize,
}
impl<'a> Frame<'a> {
    pub fn new(
        grid: Grid<'a>,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        let n = grid.resolution;
        if !(2..=4097).contains(&n)
            || grid.height.len() != n * n
            || !grid.size.is_finite()
            || !(1. ..=6000.).contains(&grid.size)
            || !grid.origin_y.is_finite()
            || grid.origin_y.abs() > 10000.
            || !grid.vertical_span.is_finite()
            || !(1. ..=20000.).contains(&grid.vertical_span)
            || !(1..=512_000_000).contains(&maximum_work)
        {
            return Err("Invalid terrain transaction grid or work allowance".into());
        }
        if cancelled() {
            return Ok(None);
        }
        let used = n * n * 3 + (n * n).div_ceil(64);
        if used > maximum_work {
            return Err("Terrain transaction initialization budget exceeded".into());
        }
        for chunk in grid.height.chunks(4096) {
            if cancelled() {
                return Ok(None);
            }
            if chunk.iter().any(|h| !(0..=32766).contains(h)) {
                return Err("Invalid encoded terrain height".into());
            }
        }
        Ok(Some(Self {
            grid,
            cell_centers: (0..n)
                .map(|i| -grid.size * 0.5 + ((i as f32 + 0.5) / n as f32) * grid.size)
                .collect(),
            current: grid.height.to_vec(),
            pending: vec![0; n * n],
            dirty: vec![0; (n * n).div_ceil(64)],
            touched: Vec::new(),
            used,
            limit: maximum_work,
            poisoned: false,
            passes: 0,
            sweeps: 0,
            writes: 0,
            committed: 0,
        }))
    }
    pub fn heights(&self) -> Result<&[i16], String> {
        if self.poisoned {
            return Err("Terrain transaction failed; discard it".into());
        }
        Ok(&self.current)
    }
    pub fn charge_work(&mut self, count: usize) -> Result<(), String> {
        if self.poisoned {
            return Err("Terrain transaction failed; discard it".into());
        }
        if count > self.limit - self.used {
            self.poisoned = true;
            return Err("Terrain transaction work budget exceeded".into());
        }
        self.used += count;
        Ok(())
    }
    pub fn work(&self) -> usize {
        self.used
    }
    /// Radius/depth sampling is provided by the bound family policy and native
    /// noise. The frame never invents noise or scales roads as if they were rivers.
    pub fn sweep(
        &mut self,
        path: &Path,
        shape: Shape,
        settings: Settings,
        sample: impl FnMut(usize, f32, f32) -> Result<Sample, String>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<SweepReport>, String> {
        self.sweep_observed(path, shape, settings, sample, |_| {}, cancelled)
    }
    /// Observes active writes, including those rounded back to their source
    /// value. The observer does not grant permission to change protected data.
    pub fn sweep_observed(
        &mut self,
        path: &Path,
        shape: Shape,
        settings: Settings,
        mut sample: impl FnMut(usize, f32, f32) -> Result<Sample, String>,
        mut observe: impl FnMut(CellWrite),
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<SweepReport>, String> {
        if self.poisoned {
            return Err("Terrain transaction failed; discard it".into());
        }
        self.poisoned = true;
        shape.validate()?;
        if ![0.125, 0.25, 0.5, 1., 2., 4., 8.].contains(&settings.sample_spacing)
            || [settings.intensity, settings.fade_multiplier]
                .iter()
                .any(|v| !v.is_finite() || !(0. ..=1.).contains(v))
            || !path.length().is_finite()
            || path.length() <= 0.
            || path.length() > 40000.
            || path.range() != (0, path.points().len() - 1)
        {
            return Err("Invalid terrain sweep path or settings".into());
        }
        let before = self.used;
        macro_rules! visit {
            ($count:expr) => {
                if cancelled() {
                    return Ok(None);
                }
                let count = $count;
                if count > self.limit - self.used {
                    return Err("Terrain sweep work budget exceeded".into());
                }
                self.used += count;
            };
        }
        let grid = self.grid;
        let n = grid.resolution;
        let index = |p: [f32; 3]| {
            [p[0], p[2]].map(|v| {
                (((v + grid.size * 0.5) * (1. / grid.size)) * n as f32).clamp(0., (n - 1) as f32)
                    as i32
            })
        };
        let edges =
            |point: [f32; 3], tangent: [f32; 3], radius: f32| -> Result<[[f32; 3]; 2], String> {
                let length = (tangent[0] * tangent[0] + tangent[2] * tangent[2]).sqrt();
                if length < 0.00001 {
                    return Err("Vertical terrain sweep tangent is unsupported".into());
                }
                let side = [tangent[2] / length, 0., -tangent[0] / length];
                Ok([
                    std::array::from_fn(|i| point[i] - side[i] * radius),
                    std::array::from_fn(|i| point[i] + side[i] * radius),
                ])
            };
        let start = path.points()[0];
        let end = *path.points().last().expect("validated controls");
        let mut point = start;
        let mut tangent = path.tangents()[0];
        let mut previous_line = SweepLine::new(point, tangent, settings.sample_spacing);
        let mut current_line = previous_line;
        let base = shape.width * 0.5;
        let mut previous_edges = edges(point, tangent, base + shape.padding[1] + shape.fade[1])?;
        let count =
            ((path.length() + settings.sample_spacing) / settings.sample_spacing).ceil() as usize;
        let mut report = SweepReport::default();
        for step in 0..count {
            visit!(1);
            let distance = step as f32 * settings.sample_spacing;
            let (next, next_tangent) = path
                .surface_sample(distance + settings.sample_spacing)
                .map_err(|e| format!("Terrain sweep sample failed: {e:?}"))?;
            edges(next, next_tangent, 0.)?;
            let next_line = SweepLine::new(next, next_tangent, settings.sample_spacing);
            let values = sample(step, distance, path.length())?;
            if !values.radius.is_finite()
                || values.radius <= 0.
                || !values.depth.is_finite()
                || values.depth.abs() > 1_000_000.
                || !values.inner_scale.is_finite()
                || !(0.0001..=10000.).contains(&values.inner_scale)
            {
                return Err("Invalid terrain sweep radius, depth or inner scale".into());
            }
            let opacity = if path.is_circular() {
                1.
            } else {
                inverse(
                    0.,
                    base,
                    distance_xz(start, point).min(distance_xz(end, point)),
                )
            };
            let current_edges = edges(
                point,
                tangent,
                values.radius + shape.padding[1] + shape.fade[1],
            )?;
            let quad = [
                index(previous_edges[0]),
                index(previous_edges[1]),
                index(current_edges[0]),
                index(current_edges[1]),
            ];
            let min_x = quad.iter().map(|p| p[0]).min().unwrap();
            let max_x = quad.iter().map(|p| p[0]).max().unwrap();
            let min_z = quad.iter().map(|p| p[1]).min().unwrap();
            let max_z = quad.iter().map(|p| p[1]).max().unwrap();
            let coverage = QuadCoverage::new(quad);
            report.sweeps += 1;
            for z in min_z..=max_z {
                let row = coverage.row(z);
                let world_z = self.cell_centers[z as usize];
                for x in min_x..=max_x {
                    visit!(1);
                    if !row.contains(x) {
                        continue;
                    }
                    report.covered_visits += 1;
                    let world = [self.cell_centers[x as usize], 0., world_z];
                    let (d, nearest_height) =
                        nearest_sweep_height([previous_line, current_line, next_line], world);
                    let fade = inverse(
                        values.radius + shape.padding[1] + shape.fade[1] * settings.fade_multiplier,
                        values.radius + shape.padding[1],
                        d,
                    );
                    let alpha = settings.intensity * opacity * fade;
                    if alpha <= 0. {
                        continue;
                    }
                    let cut = inverse(
                        values.radius - shape.padding[0] * values.inner_scale,
                        values.radius
                            - shape.padding[0] * values.inner_scale
                            - shape.fade[0] * values.inner_scale,
                        d,
                    );
                    let inv_height = 1. / grid.vertical_span;
                    let target = (nearest_height - grid.origin_y) * inv_height
                        + smooth(0., values.depth * inv_height, cut);
                    if !target.is_finite() || !(0. ..=1.).contains(&target) {
                        return Err("Terrain sweep target exceeds encodable height range".into());
                    }
                    let i = z as usize * n + x as usize;
                    if self.dirty[i / 64] & (1_u64 << (i % 64)) == 0 {
                        self.dirty[i / 64] |= 1_u64 << (i % 64);
                        self.touched.push(i as u32);
                    }
                    let (after, raw_encoded) = blend_observed(self.current[i], target, alpha);
                    self.pending[i] = after;
                    observe(CellWrite {
                        cell: i,
                        opacity: alpha,
                        before: self.current[i],
                        target_normalized: target,
                        raw_encoded,
                        after,
                    });
                    report.height_writes += 1;
                }
            }
            previous_edges = current_edges;
            previous_line = current_line;
            current_line = next_line;
            point = next;
            tangent = next_tangent;
        }
        for k in 0..self.touched.len() {
            visit!(1);
            let i = self.touched[k] as usize;
            self.current[i] = self.pending[i];
            self.dirty[i / 64] &= !(1_u64 << (i % 64));
        }
        report.committed_samples = self.touched.len();
        self.touched.clear();
        report.work = self.used - before;
        self.passes += 1;
        self.sweeps += report.sweeps;
        self.writes += report.height_writes;
        self.committed += report.committed_samples;
        self.poisoned = false;
        Ok(Some(report))
    }
    pub fn finish(mut self, cancelled: impl Fn() -> bool) -> Result<Option<Output>, String> {
        if self.poisoned {
            return Err("Terrain transaction failed; discard it".into());
        }
        let mut report = Report {
            engine: VERSION,
            path_passes: self.passes,
            sweeps: self.sweeps,
            height_writes: self.writes,
            committed_samples: self.committed,
            changed_samples: 0,
            maximum_raise: 0.,
            maximum_lower: 0.,
            full_grid_copies: 1,
            work: 0,
        };
        let mut raise = 0;
        let mut lower = 0;
        for start in (0..self.current.len()).step_by(4096) {
            if cancelled() {
                return Ok(None);
            }
            let end = (start + 4096).min(self.current.len());
            self.charge_work(end - start)?;
            for (after, before) in self.current[start..end]
                .chunks(128)
                .zip(self.grid.height[start..end].chunks(128))
            {
                if after == before {
                    continue;
                }
                for (&after, &before) in after.iter().zip(before) {
                    let delta = i32::from(after) - i32::from(before);
                    report.changed_samples += usize::from(delta != 0);
                    raise = raise.max(delta);
                    lower = lower.max(-delta);
                }
            }
        }
        // Positive height scaling preserves the order of encoded differences.
        report.maximum_raise = raise as f32 * SHORT_TO_FLOAT * self.grid.vertical_span;
        report.maximum_lower = lower as f32 * SHORT_TO_FLOAT * self.grid.vertical_span;
        report.work = self.used;
        Ok((!cancelled()).then_some(Output {
            height: self.current,
            report,
        }))
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn river_distance_near_tie_keeps_the_same_closest_segment() {
        // Captured from the seed-5 river sweep. powi(2) rounds the final
        // distance up in an unoptimized build and changes the winning segment.
        let point = [3297642063, 0, 1149438590].map(f32::from_bits);
        let projections = [
            [3297839158, 1107674827, 1149715174],
            [3297838815, 1107673168, 1149715262],
            [3297832169, 1107629181, 1149719870],
        ]
        .map(|p| p.map(f32::from_bits));
        let distances = projections.map(|p| distance_xz(point, p));
        assert_eq!(
            distances.map(f32::to_bits),
            [1109775648, 1109771577, 1109771576]
        );
        assert!(distances[2] < distances[1]);
        let lines = projections.map(|p| SweepLine::between(p, p));
        let (distance, height) = nearest_sweep_height(lines, point);
        assert_eq!(distance.to_bits(), distances[2].to_bits());
        assert_eq!(height.to_bits(), projections[2][1].to_bits());
        let tied = [[1., 10., 0.], [1., 20., 0.], [1., 30., 0.]]
            .map(|p| SweepLine::between(p, p));
        assert_eq!(nearest_sweep_height(tied, [0.; 3]), (1., 20.));
        assert_eq!(
            projections.map(|p| crate::path_surface::distance_xz(point, p)),
            distances
        );
    }
    #[test]
    fn observations_preserve_source_output_and_include_quantized_away_cuts() {
        let base = vec![18000; 65 * 65];
        let y = -500. + 18000. * SHORT_TO_FLOAT * 1000.;
        let p = path(y, 17.);
        let shape = Shape {
            width: 12.,
            padding: [1., 2.],
            fade: [1., 8.],
        };
        let settings = Settings {
            sample_spacing: 1.,
            intensity: 1.,
            fade_multiplier: 0.5,
        };
        let sample = |_: usize, _: f32, _: f32| {
            Ok(Sample {
                radius: 6.,
                depth: -0.01,
                inner_scale: 1.,
            })
        };
        let mut ordinary = Frame::new(grid(&base, 65), 1_000_000, || false)
            .unwrap()
            .unwrap();
        let mut observed = Frame::new(grid(&base, 65), 1_000_000, || false)
            .unwrap()
            .unwrap();
        let a = ordinary
            .sweep(&p, shape, settings, sample, || false)
            .unwrap()
            .unwrap();
        let mut events = Vec::new();
        let b = observed
            .sweep_observed(&p, shape, settings, sample, |e| events.push(e), || false)
            .unwrap()
            .unwrap();
        assert_eq!(events.len(), b.height_writes);
        assert_eq!(
            serde_json::to_value(a).unwrap(),
            serde_json::to_value(b).unwrap()
        );
        assert!(
            events
                .iter()
                .any(|e| e.opacity > 0. && e.raw_encoded < e.before as f32 && e.after == e.before)
        );
        let a = ordinary.finish(|| false).unwrap().unwrap();
        let b = observed.finish(|| false).unwrap().unwrap();
        assert_eq!(a.height, b.height);
        assert_eq!(
            serde_json::to_value(a.report).unwrap(),
            serde_json::to_value(b.report).unwrap()
        );
    }
    #[test]
    fn closest_points_match_owned_line_arithmetic_at_slopes_and_short_segments() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/server-bd19c4ac-path-arithmetic.json"
        ))
        .unwrap();
        assert_eq!(fixture["format"], "mapper-owned-path-arithmetic-1");
        let mut count = 0;
        for result in fixture["results"].as_array().unwrap() {
            let query = &result["query"];
            if query["kind"] != "closest" {
                continue;
            }
            let point =
                |name: &str| -> [f32; 3] { serde_json::from_value(query[name].clone()).unwrap() };
            let expected: [i32; 3] = serde_json::from_value(result["bits"].clone()).unwrap();
            let actual = closest([point("a"), point("b")], point("p"));
            assert_eq!(actual.map(|v| v.to_bits() as i32), expected, "{query}");
            count += 1;
        }
        assert_eq!(count, 96);
    }
    #[test]
    fn pinned_supports_preserve_exact_source_queries_when_inert_neighbors_change() {
        let grid = Layout {
            size: 2.,
            resolution: 2,
            origin_y: -500.,
            vertical_span: 1000.,
        };
        for (fractions, expected) in [
            ([0., 0.], vec![0]),
            ([0., 0.375], vec![0, 2]),
            ([0.625, 0.], vec![0, 1]),
            ([0.25, 0.75], vec![0, 1, 2, 3]),
            ([1., 1.], vec![0, 1, 2, 3]),
        ] {
            let support = HeightSupport {
                indices: [0, 1, 2, 3],
                fractions,
            };
            assert_eq!(support.pinned_indices().collect::<Vec<_>>(), expected);
            for fixed in [0, 1, 16383, 32765, 32766] {
                let original = [fixed; 4];
                let before = interpolate(grid, support, |i| Some(original[i])).unwrap();
                for replacement in [0, 1, 16383, 32765, 32766] {
                    let mut changed = original;
                    for (i, value) in changed.iter_mut().enumerate() {
                        if !expected.contains(&i) {
                            *value = replacement;
                        }
                    }
                    let after = interpolate(grid, support, |i| Some(changed[i])).unwrap();
                    assert_eq!(before.to_bits(), after.to_bits(), "{fractions:?}");
                }
            }
        }
        // At the outer boundary the indices can alias. Retaining duplicates is
        // harmless; the fitter inserts them into a set.
        let edge = height_support(grid, [1., 0., 1.]).unwrap();
        assert_eq!(edge.pinned_indices().collect::<Vec<_>>(), vec![3]);
    }
    use super::*;
    #[test]
    fn world_height_sampler_matches_owned_get_height_01_at_fractional_and_border_queries() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/terrain-height-bd19c4ac.json"
        ))
        .unwrap();
        assert_eq!(
            reference["assembly_sha256"],
            "bd19c4ac705edd18ff556f5dbb3130e098b380577cc2ab0d73a2ec89ee289cc9"
        );
        let cases = reference["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 81);
        for case in cases {
            let heights: Vec<i16> = serde_json::from_value(case["heights"].clone()).unwrap();
            let position: [f32; 3] = serde_json::from_value(case["position"].clone()).unwrap();
            let uv: [f32; 2] = serde_json::from_value(case["uv"].clone()).unwrap();
            let normalized = sample_height_uv(
                Grid {
                    size: case["size"].as_f64().unwrap() as f32,
                    resolution: case["resolution"].as_u64().unwrap() as usize,
                    origin_y: -500.,
                    vertical_span: 1000.,
                    height: &heights,
                },
                uv,
            )
            .unwrap();
            assert_eq!(
                normalized.to_bits(),
                (case["world"].as_f64().unwrap() as f32).to_bits()
            );
            let actual = sample_height(
                Grid {
                    size: case["size"].as_f64().unwrap() as f32,
                    resolution: case["resolution"].as_u64().unwrap() as usize,
                    origin_y: -500.,
                    vertical_span: 1000.,
                    height: &heights,
                },
                position,
            )
            .unwrap();
            assert_eq!(
                actual.to_bits(),
                (case["world"].as_f64().unwrap() as f32).to_bits()
            );
        }
    }
    fn shape() -> crate::river_layout::Policy {
        let value: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/rivers/server-bd19c4ac-layout.json"
        ))
        .unwrap();
        serde_json::from_value(value["policy"].clone()).unwrap()
    }
    fn policy() -> crate::river_carve::Policy {
        let value: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/rivers/server-bd19c4ac-terrain.json"
        ))
        .unwrap();
        let mut policy: crate::river_carve::Policy =
            serde_json::from_value(value["policy"].clone()).unwrap();
        policy.vertical_iterations = 0;
        policy
    }
    fn path(y: f32, yaw: f32) -> Path {
        let (s, c) = yaw.to_radians().sin_cos();
        Path::from_controls(
            &[-24., -8., 8., 24.].map(|x| [x * c, y, x * s]),
            1000,
            || false,
        )
        .unwrap()
        .unwrap()
    }
    fn grid(height: &[i16], n: usize) -> Grid<'_> {
        Grid {
            size: 128.,
            resolution: n,
            origin_y: -500.,
            vertical_span: 1000.,
            height,
        }
    }
    #[test]
    fn sparse_frozen_paths_match_dense_source_reference_at_overlaps_and_arbitrary_yaw() {
        let shape = shape();
        let policy = policy();
        let paths = [path(10., 17.), path(14., 73.), path(20., -33.)];
        let prepared = crate::river_carve::prepare(&paths, &shape, &policy, 4_000_000, || false)
            .unwrap()
            .unwrap();
        let count = prepared.noise_coordinates().len();
        let noise: Vec<_> = (0..count).map(|i| [-0.8, 0.25, 0.9][i % 3]).collect();
        for n in [65, 129] {
            let base = vec![18000; n * n];
            let before = base.clone();
            let dense = prepared
                .carve(
                    crate::river_carve::Grid {
                        size: 128.,
                        resolution: n,
                        origin_y: -500.,
                        vertical_span: 1000.,
                        height: &base,
                    },
                    &noise,
                    || false,
                )
                .unwrap()
                .unwrap();
            let mut frame = Frame::new(grid(&base, n), 4_000_000, || false)
                .unwrap()
                .unwrap();
            let mut writes = 0;
            for path in prepared.paths().iter().rev() {
                let report = frame
                    .sweep(
                        path,
                        Shape {
                            width: shape.width,
                            padding: shape.padding,
                            fade: shape.fade,
                        },
                        Settings {
                            sample_spacing: policy.sample_spacing,
                            intensity: policy.intensity,
                            fade_multiplier: policy.fade_multiplier,
                        },
                        |step, distance, length| {
                            Ok(Sample {
                                radius: shape.radius(distance, length, noise[step]),
                                depth: policy.depth(distance, noise[step], shape.random_scale),
                                inner_scale: lerp(
                                    policy.inner_scale[0],
                                    policy.inner_scale[1],
                                    distance / policy.inner_scale[2],
                                ),
                            })
                        },
                        || false,
                    )
                    .unwrap()
                    .unwrap();
                writes += report.height_writes;
            }
            let sparse = frame.finish(|| false).unwrap().unwrap();
            assert_eq!(sparse.height, dense.height);
            assert_eq!(writes, dense.report.height_writes);
            assert_eq!(sparse.report.changed_samples, dense.report.changed_samples);
            assert_eq!(sparse.report.maximum_raise, dense.report.maximum_raise);
            assert_eq!(sparse.report.maximum_lower, dense.report.maximum_lower);
            assert_eq!(sparse.report.full_grid_copies, 1);
            assert!(sparse.report.height_writes > sparse.report.committed_samples);
            assert_eq!(base, before);
        }
    }
    #[test]
    fn invalid_or_cancelled_partial_sweeps_poison_the_entire_transaction() {
        let base = vec![18000; 33 * 33];
        let p = path(10., 17.);
        let shape = Shape {
            width: 12.,
            padding: [1., 2.],
            fade: [1., 8.],
        };
        let settings = Settings {
            sample_spacing: 1.,
            intensity: 1.,
            fade_multiplier: 0.5,
        };
        let sample = |_: usize, _: f32, _: f32| {
            Ok(Sample {
                radius: 6.,
                depth: -0.125,
                inner_scale: 1.,
            })
        };
        let mut frame = Frame::new(grid(&base, 33), 1_000_000, || false)
            .unwrap()
            .unwrap();
        assert!(
            frame
                .sweep(
                    &p,
                    shape,
                    settings,
                    |step, _, _| Ok(Sample {
                        radius: 6.,
                        depth: if step > 5 { f32::NAN } else { -0.125 },
                        inner_scale: 1.
                    }),
                    || false
                )
                .is_err()
        );
        assert!(frame.heights().is_err());
        assert!(frame.finish(|| false).is_err());
        let observed = std::cell::Cell::new(0);
        let mut reference = Frame::new(grid(&base, 33), 1_000_000, || false)
            .unwrap()
            .unwrap();
        let report = reference
            .sweep(&p, shape, settings, sample, || {
                observed.set(observed.get() + 1);
                false
            })
            .unwrap()
            .unwrap();
        let threshold = observed.get() - report.committed_samples / 2;
        let calls = std::cell::Cell::new(0);
        let mut cancelled = Frame::new(grid(&base, 33), 1_000_000, || false)
            .unwrap()
            .unwrap();
        assert!(
            cancelled
                .sweep(&p, shape, settings, sample, || {
                    calls.set(calls.get() + 1);
                    calls.get() > threshold
                })
                .unwrap()
                .is_none()
        );
        assert!(cancelled.heights().is_err());
        assert!(cancelled.finish(|| false).is_err());
        let mut limited = Frame::new(grid(&base, 33), 4000, || false)
            .unwrap()
            .unwrap();
        assert!(
            limited
                .sweep(&p, shape, settings, sample, || false)
                .is_err()
        );
        assert!(limited.finish(|| false).is_err());
        assert_eq!(base, vec![18000; 33 * 33]);
    }
    #[test]
    fn invalid_height_encodings_and_bad_frame_shapes_are_rejected_before_output() {
        let invalid = vec![-1; 33 * 33];
        assert!(Frame::new(grid(&invalid, 33), 1_000_000, || false).is_err());
        let base = vec![18000; 33 * 33];
        assert!(Frame::new(grid(&base, 33), 1, || false).is_err());
        assert!(
            Frame::new(grid(&base, 33), 1_000_000, || true)
                .unwrap()
                .is_none()
        );
        let mut bad = grid(&base, 33);
        bad.vertical_span = f32::NAN;
        assert!(Frame::new(bad, 1_000_000, || false).is_err());
    }
}
