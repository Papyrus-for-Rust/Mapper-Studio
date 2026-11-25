//! Transactional source-configured rail-bed candidates. Uses explicit world-grid
//! vertex/texel conventions, not Unity's quantized swept-quad rasterizer.
use crate::{
    erosion::{decode, encode},
    native_layers::{self, Layers},
    path_geometry::Path,
};
use serde::{Deserialize, Serialize};
pub const VERSION: &str = "papyrus-rail-bed-1";
pub const TERRAIN_VERSION: &str = "papyrus-rail-bed-terrain-1";
#[cfg(feature = "road-diagnostics")]
mod capture;
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub engine: String,
    pub loops: usize,
    pub vertical_iterations: u16,
    pub sample_spacing: f32,
    pub transition_points: usize,
    pub minimum_fade: f32,
    pub river_smoothing: f32,
    pub bank_smoothing: f32,
    pub river_fade: f32,
    pub bank_fade: f32,
    pub water_clearance: f32,
    pub topology_threshold: f32,
    pub railside_radius: usize,
    pub cliff_slope: f32,
}
impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        if self.engine != VERSION
            || !(1..=16).contains(&self.loops)
            || self.vertical_iterations > 64
            || !self.sample_spacing.is_finite()
            || !(0.25..=4.).contains(&self.sample_spacing)
            || !(1..=64).contains(&self.transition_points)
            || self.railside_radius > 32
            || [
                self.minimum_fade,
                self.river_smoothing,
                self.bank_smoothing,
                self.river_fade,
                self.bank_fade,
                self.topology_threshold,
            ]
            .iter()
            .any(|v| !v.is_finite() || !(0. ..=1.).contains(v))
            || !self.water_clearance.is_finite()
            || !(0. ..=20.).contains(&self.water_clearance)
            || !self.cliff_slope.is_finite()
            || !(0. ..=90.).contains(&self.cliff_slope)
        {
            return Err("Unsupported rail bed source policy".into());
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Shape {
    pub width: f32,
    pub inner_padding: f32,
    pub outer_padding: f32,
    pub inner_fade: f32,
    pub outer_fade: f32,
    pub terrain_offset: f32,
    pub splat_channel: usize,
    pub rail: u32,
    pub railside: u32,
    pub cliff: u32,
    pub eligible_railside: u32,
    pub river: u32,
    pub bank: u32,
    pub preserve_topology: u32,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub maximum_work: usize,
    pub maximum_cut_fill: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade_fit: Option<crate::rail_grade_fit::Settings>,
}
impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(fit) = self.grade_fit {
            fit.validate()?;
        }
        if !(1..=512_000_000).contains(&self.maximum_work)
            || !self.maximum_cut_fill.is_finite()
            || !(0. ..=64.).contains(&self.maximum_cut_fill)
        {
            return Err("Invalid rail bed work/earthwork limits".into());
        }
        Ok(())
    }
}
pub struct Track<'a> {
    pub id: u64,
    pub path: &'a Path,
    pub preserve_start: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct FittedTrack {
    pub id: u64,
    pub preserve_start: bool,
    pub path: Path,
    pub maximum_discrete_grade: f32,
}
#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub work: usize,
    pub height_writes: usize,
    pub changed_height_samples: usize,
    pub changed_splat_cells: usize,
    pub changed_topology_cells: usize,
    pub protected_samples: usize,
    pub protected_core_samples: usize,
    pub alpha_fade_samples: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub limited_shoulder_samples: usize,
    pub railside_cells: usize,
    pub maximum_raise: f32,
    pub maximum_lower: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grade_fits: Option<Vec<crate::rail_bed_fit::Report>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface_paint_deferred: Option<bool>,
}
pub struct Output {
    pub layers: Layers,
    pub tracks: Vec<FittedTrack>,
    pub report: Report,
}
fn is_zero(value: &usize) -> bool {
    *value == 0
}

fn smooth(v: f32) -> f32 {
    let v = v.clamp(0., 1.);
    v * v * (3. - 2. * v)
}
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0., 1.)
}
fn inverse(a: f32, b: f32, v: f32) -> f32 {
    if a == b {
        0.
    } else {
        ((v - a) / (b - a)).clamp(0., 1.)
    }
}
fn index(n: usize, v: f32) -> usize {
    (v * n as f32).clamp(0., (n - 1) as f32) as usize
}
fn uv(size: u32, p: [f32; 3]) -> Option<[f32; 2]> {
    let uv = [p[0] / size as f32 + 0.5, p[2] / size as f32 + 0.5];
    uv.iter().all(|v| (0. ..=1.).contains(v)).then_some(uv)
}
fn visit(report: &mut Report, settings: Settings) -> Result<(), String> {
    if report.work >= settings.maximum_work {
        return Err("Rail bed work allowance reached".into());
    }
    report.work += 1;
    Ok(())
}

fn has_empty_cells(planes: &[u8], cells: usize, start: usize, present: &mut [u8]) -> bool {
    let end = start + present.len();
    let mut channels = planes.chunks_exact(cells);
    present.copy_from_slice(&channels.next().expect("validated surface channels")[start..end]);
    for channel in channels {
        for (value, &weight) in present.iter_mut().zip(&channel[start..end]) {
            *value |= weight;
        }
    }
    present.contains(&0)
}

fn validate_surface_rows(
    base: &Layers,
    report: &mut Report,
    settings: Settings,
    cancelled: &impl Fn() -> bool,
) -> Result<bool, String> {
    let n = base.surface_resolution;
    let cells = n * n;
    let mut present = vec![0; n];
    for start in (0..cells).step_by(n) {
        if cancelled() {
            return Ok(false);
        }
        let count = n.min(settings.maximum_work - report.work);
        // Only check cells the original per-cell walk could charge. An invalid
        // cell in this row's prefix still wins over a later exhausted budget.
        if count != 0
            && (has_empty_cells(&base.splat, cells, start, &mut present[..count])
                || has_empty_cells(&base.biome, cells, start, &mut present[..count]))
        {
            return Err("Rail bed requires nonempty input biome/splat cells".into());
        }
        report.work += count;
        if count != n {
            return Err("Rail bed work allowance reached".into());
        }
    }
    Ok(true)
}

fn flags(layers: &Layers, p: [f32; 3]) -> Result<(usize, u32), String> {
    let uv = uv(layers.size, p).ok_or("Rail bed geometry leaves the world")?;
    let n = layers.surface_resolution;
    let i = index(n, uv[1]) * n + index(n, uv[0]);
    Ok((i, layers.topology[i] as u32))
}
fn height(layers: &Layers, p: [f32; 3]) -> Result<f32, String> {
    let uv = uv(layers.size, p).ok_or("Rail bed geometry leaves the world")?;
    Ok(crate::spawn::height(layers, uv[0], uv[1]))
}
#[allow(clippy::too_many_arguments)]
fn fit(
    layers: &Layers,
    water: Option<&[f32]>,
    track: &FittedTrack,
    p: &Policy,
    shape: Shape,
    iterations: u16,
    report: &mut Report,
    settings: Settings,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Path>, String> {
    let mut heights = Vec::with_capacity(track.path.points().len());
    let mut weights = Vec::with_capacity(heights.capacity());
    for (i, &point) in track.path.points().iter().enumerate() {
        if cancelled() {
            return Ok(None);
        }
        visit(report, settings)?;
        let (cell, mask) = flags(layers, point)?;
        let mut y = height(layers, point)?;
        if iterations > 0 && mask & (shape.river | shape.bank) != 0 {
            let level = water
                .and_then(|v| v.get(cell))
                .copied()
                .filter(|v| v.is_finite() && *v > -500. && *v <= 500.)
                .ok_or("Rail water crossing has no verified water surface height")?;
            y = y.max(level + p.water_clearance);
        }
        let start = if track.preserve_start {
            smooth(i as f32 / p.transition_points as f32)
        } else {
            1.
        };
        let weight = start
            * if mask & shape.river != 0 {
                p.river_smoothing
            } else if mask & shape.bank != 0 {
                p.bank_smoothing
            } else {
                1.
            };
        heights.push(if track.preserve_start {
            lerp(point[1], y, smooth(weight))
        } else {
            y
        });
        weights.push(weight);
    }
    let budget = settings
        .maximum_work
        .saturating_sub(report.work)
        .min(32000000);
    if budget == 0 {
        return Err("Rail bed geometry allowance reached".into());
    }
    let result = track
        .path
        .refit_heights(&heights, iterations, &weights, budget, cancelled)
        .map_err(|e| format!("Rail vertical fitting failed: {e:?}"))?;
    if let Some(path) = &result {
        report.work += path.work();
    }
    Ok(result)
}
// Bounded Hermite sampling followed by segment-distance rasterization. Height
// indices are actual mesh vertices; material/topology indices are texel centers.
// Nearest coverage is accumulated once per path to avoid repeated paint rounding.
struct RasterAxis {
    world: Vec<f32>,
    scale: f32,
    shift: f32,
}

impl RasterAxis {
    fn new(size: u32, resolution: usize, centers: bool) -> Self {
        let (scale, shift) = if centers {
            (resolution as f32, 0.5)
        } else {
            ((resolution - 1) as f32, 0.)
        };
        let world = (0..resolution)
            .map(|i| ((i as f32 + shift) / scale - 0.5) * size as f32)
            .collect();
        Self {
            world,
            scale,
            shift,
        }
    }
}

// Corridor and endpoint callers bound both axes by the authored radius.
// On x64 MSVC, the CRT uses this same double-precision calculation for
// finite inputs. Keeping it inline avoids a CRT call for each raster cell.
#[inline]
fn bounded_distance(x: f32, z: f32) -> f32 {
    #[cfg(all(target_arch = "x86_64", target_env = "msvc"))]
    {
        let (x, z) = (f64::from(x), f64::from(z));
        (x * x + z * z).sqrt() as f32
    }
    #[cfg(not(all(target_arch = "x86_64", target_env = "msvc")))]
    {
        x.hypot(z)
    }
}

fn endpoint_opacity(start: [f32; 3], end: [f32; 3], wx: f32, wz: f32, radius: f32) -> f32 {
    let clipped_distance = |p: [f32; 3]| {
        let dx = wx - p[0];
        let dz = wz - p[2];
        // Outside either axis the radial fade is already saturated.
        if dx.abs() >= radius || dz.abs() >= radius {
            radius
        } else {
            bounded_distance(dx, dz).min(radius)
        }
    };
    inverse(
        0.,
        radius,
        clipped_distance(start).min(clipped_distance(end)),
    )
}

#[allow(clippy::too_many_arguments)]
fn corridor(
    size: u32,
    path: &Path,
    axis: &RasterAxis,
    radius: f32,
    spacing: f32,
    report: &mut Report,
    settings: Settings,
    cancelled: &impl Fn() -> bool,
    mut action: impl FnMut(usize, f32, f32, [usize; 2]) -> Result<(), String>,
) -> Result<bool, String> {
    if path.length() <= 0. {
        return Err("Zero-length rail bed path".into());
    }
    let steps = (path.length() / spacing).ceil() as usize;
    if steps > 100000 {
        return Err("Rail bed path exceeds sample count bound".into());
    }
    let n = axis.world.len();
    let grid_index = |v: f32| {
        ((v / size as f32 + 0.5) * axis.scale - axis.shift)
            .floor()
            .clamp(0., (n - 1) as f32) as usize
    };
    let mut a = path
        .point(0.)
        .map_err(|e| format!("Rail sampling failed: {e:?}"))?;
    for step in 0..steps {
        if cancelled() {
            return Ok(false);
        }
        visit(report, settings)?;
        let b = path
            .point(((step + 1) as f32 * spacing).min(path.length()))
            .map_err(|e| format!("Rail sampling failed: {e:?}"))?;
        if uv(size, a).is_none() || uv(size, b).is_none() {
            return Err("Rail curve leaves world bounds".into());
        }
        let dx = b[0] - a[0];
        let dz = b[2] - a[2];
        let d2 = dx * dx + dz * dz;
        if d2 < 0.000001 {
            a = b;
            continue;
        }
        let minx = grid_index(a[0].min(b[0]) - radius);
        let maxx = (grid_index(a[0].max(b[0]) + radius) + 1).min(n - 1);
        let minz = grid_index(a[2].min(b[2]) - radius);
        let maxz = (grid_index(a[2].max(b[2]) + radius) + 1).min(n - 1);
        for z in minz..=maxz {
            let pz = axis.world[z] - a[2];
            for x in minx..=maxx {
                visit(report, settings)?;
                let px = axis.world[x] - a[0];
                let t = ((px * dx + pz * dz) / d2).clamp(0., 1.);
                let ex = px - t * dx;
                let ez = pz - t * dz;
                if ex.abs() > radius || ez.abs() > radius {
                    continue;
                }
                let distance = bounded_distance(ex, ez);
                if distance > radius {
                    continue;
                }
                action(z * n + x, distance, lerp(a[1], b[1], t), [x, z])?;
            }
        }
        a = b;
    }
    Ok(true)
}
fn normalize(mut weights: [f32; 8]) -> [u8; 8] {
    let sum = weights.iter().sum::<f32>();
    if sum <= 0. {
        return [255, 0, 0, 0, 0, 0, 0, 0];
    }
    for v in &mut weights {
        *v = *v / sum * 255.;
    }
    let mut out = weights.map(|v| v.floor() as u8);
    let mut order: [usize; 8] = std::array::from_fn(|i| i);
    order.sort_by(|&a, &b| {
        (weights[b] - out[b] as f32)
            .total_cmp(&(weights[a] - out[a] as f32))
            .then(a.cmp(&b))
    });
    let remainder = 255 - out.iter().map(|&v| v as usize).sum::<usize>();
    for &i in order.iter().take(remainder) {
        out[i] += 1;
    }
    out
}
fn grade(path: &Path) -> f32 {
    path.points()
        .windows(2)
        .map(|w| {
            let distance = (w[1][0] - w[0][0]).hypot(w[1][2] - w[0][2]);
            if distance < 0.00001 {
                if (w[1][1] - w[0][1]).abs() < 0.00001 {
                    0.
                } else {
                    f32::INFINITY
                }
            } else {
                (w[1][1] - w[0][1]).abs() / distance
            }
        })
        .fold(0., f32::max)
}
/// All writes target a private revision. Any failure/cancellation drops that
/// revision; alpha and biome arrays are immutable. Protection is conservative
/// and does not establish collision-free use of a monument's existing tracks.
#[allow(clippy::too_many_arguments)]
pub fn build(
    base: &Layers,
    alpha: &[u8],
    water: Option<&[f32]>,
    tracks: &[Track<'_>],
    p: &Policy,
    shape: Shape,
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    build_with_joins(
        base,
        alpha,
        water,
        tracks,
        &[],
        p,
        shape,
        settings,
        cancelled,
    )
}
#[allow(clippy::too_many_arguments)]
pub fn build_with_joins(
    base: &Layers,
    alpha: &[u8],
    water: Option<&[f32]>,
    tracks: &[Track<'_>],
    joins: &[crate::rail_bed_fit::Join],
    p: &Policy,
    shape: Shape,
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    build_inner(
        base, alpha, water, tracks, joins, p, shape, settings, false, cancelled,
    )
}
/// Earthwork and fitted curves only. Preserve authored splat/topology so the
/// source-ordered coordinator can apply late surface stages exactly once.
#[allow(clippy::too_many_arguments)]
pub fn build_terrain_with_joins(
    base: &Layers,
    alpha: &[u8],
    water: Option<&[f32]>,
    tracks: &[Track<'_>],
    joins: &[crate::rail_bed_fit::Join],
    p: &Policy,
    shape: Shape,
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    build_inner(
        base, alpha, water, tracks, joins, p, shape, settings, true, cancelled,
    )
}
#[allow(clippy::too_many_arguments)]
fn build_inner(
    base: &Layers,
    alpha: &[u8],
    water: Option<&[f32]>,
    tracks: &[Track<'_>],
    joins: &[crate::rail_bed_fit::Join],
    p: &Policy,
    shape: Shape,
    settings: Settings,
    defer_surfaces: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    p.validate()?;
    settings.validate()?;
    let (hr, n) = native_layers::dimensions(base.size)?;
    let cells = n * n;
    if base.height_resolution != hr
        || base.surface_resolution != n
        || base.height.len() != hr * hr
        || base.splat.len() != cells * 8
        || base.topology.len() != cells
        || base.biome.len() != cells * 5
        || alpha.len() != cells
        || base.height.iter().any(|v| !(0..=32766).contains(v))
        || water.is_some_and(|w| w.len() != cells)
        || !(1..=256).contains(&tracks.len())
        || tracks.iter().map(|t| t.path.points().len()).sum::<usize>() > 131072
        || !shape.width.is_finite()
        || !(0.1..=32.).contains(&shape.width)
        || [
            shape.inner_padding,
            shape.outer_padding,
            shape.inner_fade,
            shape.outer_fade,
        ]
        .iter()
        .any(|v| !v.is_finite() || !(0. ..=128.).contains(v))
        || shape.inner_padding > shape.width * 0.5
        || !shape.terrain_offset.is_finite()
        || shape.terrain_offset.abs() > 8.
        || shape.splat_channel >= 8
        || [
            shape.rail,
            shape.railside,
            shape.cliff,
            shape.river,
            shape.bank,
        ]
        .iter()
        .any(|v| v.count_ones() != 1)
        || shape.eligible_railside == 0
        || shape.preserve_topology == 0
    {
        return Err("Invalid rail bed layers, geometry or source shape".into());
    }
    let ids = tracks
        .iter()
        .map(|t| t.id)
        .collect::<std::collections::BTreeSet<_>>();
    if ids.len() != tracks.len() || ids.contains(&0) {
        return Err("Invalid rail bed path identity".into());
    }
    if cancelled() {
        return Ok(None);
    }
    let mut report = Report {
        surface_paint_deferred: defer_surfaces.then_some(true),
        ..Default::default()
    };
    if !validate_surface_rows(base, &mut report, settings, &cancelled)? {
        return Ok(None);
    }
    let mut layers = base.clone();
    let mut fitted = Vec::new();
    for track in tracks {
        let heights = track.path.points().iter().map(|p| p[1]).collect::<Vec<_>>();
        let weights = vec![1.; heights.len()];
        let budget = settings
            .maximum_work
            .saturating_sub(report.work)
            .min(32000000);
        if budget == 0 {
            return Err("Rail bed setup allowance reached".into());
        }
        let Some(path) = track
            .path
            .refit_heights(&heights, 0, &weights, budget, &cancelled)
            .map_err(|e| format!("Invalid bed path: {e:?}"))?
        else {
            return Ok(None);
        };
        report.work += path.work();
        fitted.push(FittedTrack {
            id: track.id,
            preserve_start: track.preserve_start,
            path,
            maximum_discrete_grade: 0.,
        });
    }
    let mut nearest_distance = vec![f32::INFINITY; hr * hr];
    let mut nearest_height = vec![0f32; hr * hr];
    let mut touched = Vec::new();
    let height_axis = RasterAxis::new(base.size, hr, false);
    let surface_from_height: Vec<_> = height_axis
        .world
        .iter()
        .map(|&v| index(n, v / base.size as f32 + 0.5))
        .collect();
    for pass in 0..p.loops {
        for track in fitted.iter_mut().rev() {
            let Some(path) = fit(
                &layers,
                water,
                track,
                p,
                shape,
                p.vertical_iterations,
                &mut report,
                settings,
                &cancelled,
            )?
            else {
                return Ok(None);
            };
            track.path = path;
        }
        if let Some(fit) = settings.grade_fit {
            let result = crate::rail_bed_fit::apply(
                base,
                alpha,
                water,
                tracks,
                &mut fitted,
                joins,
                p,
                shape,
                settings.maximum_cut_fill,
                fit,
                &cancelled,
            );
            #[cfg(feature = "road-diagnostics")]
            if result.is_err() {
                capture::if_requested(
                    base,
                    alpha,
                    water,
                    tracks,
                    joins,
                    p,
                    shape,
                    settings,
                    defer_surfaces,
                    &cancelled,
                )?;
            }
            let Some(result) = result? else {
                return Ok(None);
            };
            report.work += result.work;
            if report.work > settings.maximum_work {
                return Err("Connected rail fitting exhausted the bed work allowance".into());
            }
            report.grade_fits.get_or_insert_with(Vec::new).push(result);
        }
        for track in fitted.iter().rev() {
            if cancelled() {
                return Ok(None);
            }
            let source = &layers;
            let maximum_fade = 1. - pass as f32 / p.loops as f32;
            let radius = shape.width * 0.5
                + shape.outer_padding
                + shape.outer_fade * maximum_fade.max(p.minimum_fade);
            let mut protected = 0;
            let mut protected_core = 0;
            let mut holes = 0;
            let mut writes = 0;
            let complete = corridor(
                base.size,
                &track.path,
                &height_axis,
                radius,
                p.sample_spacing,
                &mut report,
                settings,
                &cancelled,
                |i, d, y, [x, z]| {
                    let cell = surface_from_height[z] * n + surface_from_height[x];
                    if alpha[cell] < 128 {
                        if d <= shape.width * 0.5 {
                            return Err("Rail core intersects an alpha hole".into());
                        }
                        holes += 1;
                        return Ok(());
                    }
                    let flags = source.topology[cell] as u32;
                    if flags & shape.preserve_topology != 0 {
                        protected += 1;
                        if d <= shape.width * 0.5 {
                            protected_core += 1;
                        }
                        return Ok(());
                    }
                    if d < nearest_distance[i] {
                        if !nearest_distance[i].is_finite() {
                            touched.push(i);
                        }
                        nearest_distance[i] = d;
                        nearest_height[i] = y;
                    }
                    Ok(())
                },
            )?;
            if !complete {
                return Ok(None);
            }
            let start = track.path.points()[0];
            let end = *track.path.points().last().unwrap();
            for &i in &touched {
                if cancelled() {
                    return Ok(None);
                }
                visit(&mut report, settings)?;
                let d = nearest_distance[i];
                let y = nearest_height[i];
                let (x, z) = (i % hr, i / hr);
                let (wx, wz) = (height_axis.world[x], height_axis.world[z]);
                let cell = surface_from_height[z] * n + surface_from_height[x];
                let flags = layers.topology[cell] as u32;
                let opacity = if track.path.is_circular() {
                    1.
                } else {
                    endpoint_opacity(start, end, wx, wz, shape.width * 0.5)
                };
                let fade = if flags & shape.river != 0 {
                    p.river_fade
                } else if flags & shape.bank != 0 {
                    p.bank_fade
                } else {
                    1.
                };
                let outer = lerp(p.minimum_fade, maximum_fade, fade);
                let weight = opacity
                    * inverse(
                        shape.width * 0.5 + shape.outer_padding + shape.outer_fade * outer,
                        shape.width * 0.5 + shape.outer_padding,
                        d,
                    );
                if weight <= 0. {
                    nearest_distance[i] = f32::INFINITY;
                    continue;
                }
                let target = y + shape.terrain_offset
                    * smooth(inverse(
                        shape.width * 0.5 - shape.inner_padding,
                        shape.width * 0.5 - shape.inner_padding - shape.inner_fade,
                        d,
                    ));
                let mut h = lerp(decode(layers.height[i]), target, smooth(weight));
                if !(-500. ..=500.).contains(&h) {
                    return Err("Rail bed would clamp encoded terrain height".into());
                }
                let original = decode(base.height[i]);
                let allowance = settings.maximum_cut_fill;
                let can_limit_shoulder =
                    settings.grade_fit.is_some() && d > shape.width * 0.5 + shape.outer_padding;
                if can_limit_shoulder && (h - original).abs() > allowance {
                    // Successive passes can accumulate on a feathered
                    // shoulder. Keep the limit relative to the original bed.
                    h = h.clamp(original - allowance, original + allowance);
                    report.limited_shoulder_samples += 1;
                } else if (h - original).abs() > allowance {
                    #[cfg(feature = "road-diagnostics")]
                    capture::if_requested(
                        base,
                        alpha,
                        water,
                        tracks,
                        joins,
                        p,
                        shape,
                        settings,
                        defer_surfaces,
                        &cancelled,
                    )?;
                    let witness = serde_json::json!({
                        "track": track.id,
                        "pass": pass,
                        "sample": i,
                        "world": [wx, wz],
                        "original_height": original,
                        "previous_height": decode(layers.height[i]),
                        "proposed_height": h,
                        "path_height": y,
                        "distance": d,
                        "weight": weight,
                        "maximum_cut_fill": settings.maximum_cut_fill,
                        "topology": flags,
                    });
                    return Err(format!(
                        "Rail bed exceeds cumulative cut/fill allowance: {witness}"
                    ));
                }
                let mut value = encode(h);
                if (decode(value) - original).abs() > allowance {
                    // Quantize inward when rounding would cross the limit.
                    value -= (value - base.height[i]).signum();
                }
                writes += usize::from(layers.height[i] != value);
                layers.height[i] = value;
                nearest_distance[i] = f32::INFINITY;
            }
            touched.clear();
            report.height_writes += writes;
            report.protected_samples += protected;
            report.protected_core_samples += protected_core;
            report.alpha_fade_samples += holes;
        }
    }
    for track in &mut fitted {
        if settings.grade_fit.is_some() {
            // Re-sampling heights from the partially modified raster here would
            // discard the graph's fixed sockets, shared joins and grade bound.
            track.maximum_discrete_grade = grade(&track.path);
            continue;
        }
        let Some(path) = fit(
            &layers,
            water,
            track,
            p,
            shape,
            0,
            &mut report,
            settings,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        track.path = path;
        track.maximum_discrete_grade = grade(&track.path);
        if !track.maximum_discrete_grade.is_finite() {
            return Err("Vertical degenerate rail segment after terrain fitting".into());
        }
    }
    if !defer_surfaces {
        let surface_axis = RasterAxis::new(base.size, n, true);
        for track in fitted.iter().rev() {
            let mut coverage = vec![0f32; cells];
            let radius = shape.width * 0.5 + shape.outer_padding;
            let start = track.path.points()[0];
            let end = *track.path.points().last().unwrap();
            let complete = corridor(
                base.size,
                &track.path,
                &surface_axis,
                radius,
                p.sample_spacing,
                &mut report,
                settings,
                &cancelled,
                |i, d, _, [x, z]| {
                    if alpha[i] < 128 {
                        if d <= shape.width * 0.5 {
                            return Err("Rail core intersects a surface alpha hole".into());
                        }
                        return Ok(());
                    }
                    if layers.topology[i] as u32 & shape.preserve_topology != 0 {
                        return Ok(());
                    }
                    let opacity = if track.path.is_circular() {
                        1.
                    } else {
                        endpoint_opacity(
                            start,
                            end,
                            surface_axis.world[x],
                            surface_axis.world[z],
                            shape.width * 0.5,
                        )
                    };
                    coverage[i] = coverage[i]
                        .max(opacity * inverse(radius, shape.width * 0.5 - shape.inner_padding, d));
                    Ok(())
                },
            )?;
            if !complete {
                return Ok(None);
            }
            for (i, &paint) in coverage.iter().enumerate() {
                if i % n == 0 && cancelled() {
                    return Ok(None);
                }
                visit(&mut report, settings)?;
                if paint <= 0. {
                    continue;
                }
                let values: [f32; 8] =
                    std::array::from_fn(|c| layers.splat[c * cells + i] as f32 / 255.);
                let normalized = normalize(std::array::from_fn(|c| {
                    values[c] * (1. - paint) + if c == shape.splat_channel { paint } else { 0. }
                }));
                for (c, v) in normalized.into_iter().enumerate() {
                    layers.splat[c * cells + i] = v;
                }
                if paint > p.topology_threshold {
                    layers.topology[i] |= shape.rail as i32;
                }
            }
        }
        // Two axis scans: the second sees the first scan's admitted railside flags,
        // matching the source's separable, eligibility-limited propagation schedule.
        for vertical in [false, true] {
            let snapshot = layers.topology.clone();
            for a in 0..n {
                if cancelled() {
                    return Ok(None);
                }
                let at = |b: usize| if vertical { b * n + a } else { a * n + b };
                let mut prefix = vec![0u16; n + 1];
                for b in 0..n {
                    visit(&mut report, settings)?;
                    prefix[b + 1] = prefix[b]
                        + u16::from(snapshot[at(b)] as u32 & (shape.rail | shape.railside) != 0);
                }
                for b in 0..n {
                    visit(&mut report, settings)?;
                    if prefix[(b + p.railside_radius + 1).min(n)]
                        == prefix[b.saturating_sub(p.railside_radius)]
                    {
                        continue;
                    }
                    let i = at(b);
                    if alpha[i] < 128 || layers.topology[i] as u32 & shape.preserve_topology != 0 {
                        continue;
                    }
                    if snapshot[i] as u32 & shape.eligible_railside != 0 {
                        report.railside_cells +=
                            usize::from(layers.topology[i] as u32 & shape.railside == 0);
                        layers.topology[i] |= shape.railside as i32;
                    }
                    let x = (i % n) as f32 / n as f32 + 0.5 / n as f32;
                    let z = (i / n) as f32 / n as f32 + 0.5 / n as f32;
                    if crate::rail_costmap::sample_height_slope(
                        base.size as f32,
                        hr,
                        &layers.height,
                        [x, z],
                    )
                    .1 > p.cliff_slope
                    {
                        layers.topology[i] |= shape.cliff as i32;
                    }
                }
            }
        }
    }
    for (i, (&before, &after)) in base.height.iter().zip(&layers.height).enumerate() {
        if i % hr == 0 && cancelled() {
            return Ok(None);
        }
        visit(&mut report, settings)?;
        if before != after {
            report.changed_height_samples += 1;
            let delta = decode(after) - decode(before);
            report.maximum_raise = report.maximum_raise.max(delta);
            report.maximum_lower = report.maximum_lower.max(-delta);
        }
    }
    for i in 0..cells {
        if i % n == 0 && cancelled() {
            return Ok(None);
        }
        visit(&mut report, settings)?;
        report.changed_splat_cells +=
            usize::from((0..8).any(|c| base.splat[c * cells + i] != layers.splat[c * cells + i]));
        report.changed_topology_cells += usize::from(base.topology[i] != layers.topology[i]);
    }
    Ok(if cancelled() {
        None
    } else {
        Some(Output {
            layers,
            tracks: fitted,
            report,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_opacity_preserves_the_source_fade_at_cell_boundaries() {
        let start = [291.75, 50., -71.125];
        let end = [304.125, 51., -75.5];
        for radius in [0.05_f32, 2., 16.] {
            for dx in [
                -radius.next_up(),
                -radius,
                0.,
                radius.next_down(),
                radius,
                radius.next_up(),
            ] {
                for dz in [
                    -radius.next_up(),
                    -radius,
                    0.25 * radius,
                    radius,
                    radius.next_up(),
                ] {
                    for point in [start, end] {
                        let wx = point[0] + dx;
                        let wz = point[2] + dz;
                        let distance = (wx - start[0])
                            .hypot(wz - start[2])
                            .min((wx - end[0]).hypot(wz - end[2]));
                        assert_eq!(
                            endpoint_opacity(start, end, wx, wz, radius).to_bits(),
                            inverse(0., radius, distance).to_bits()
                        );
                    }
                }
            }
        }
    }
    use crate::path_geometry;
    use std::cell::Cell;
    fn policy() -> Policy {
        Policy {
            engine: VERSION.into(),
            loops: 8,
            vertical_iterations: 8,
            sample_spacing: 1.,
            transition_points: 8,
            minimum_fade: 0.125,
            river_smoothing: 0.1,
            bank_smoothing: 0.3,
            river_fade: 0.,
            bank_fade: 0.5,
            water_clearance: 2.,
            topology_threshold: 0.3,
            railside_radius: 6,
            cliff_slope: 40.,
        }
    }
    fn shape() -> Shape {
        Shape {
            width: 4.,
            inner_padding: 1.,
            outer_padding: 1.,
            inner_fade: 1.,
            outer_fade: 32.,
            terrain_offset: -0.125,
            splat_channel: 7,
            rail: 524288,
            railside: 1048576,
            cliff: 2,
            eligible_railside: 49,
            river: 16384,
            bank: 32768,
            preserve_topology: 2098176,
        }
    }
    fn settings() -> Settings {
        Settings {
            maximum_work: 128000000,
            maximum_cut_fill: 24.,
            grade_fit: None,
        }
    }
    fn fixture() -> (Layers, Vec<u8>, Path) {
        let (h, n) = native_layers::dimensions(1000).unwrap();
        let cells = n * n;
        let mut layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 1,
            height_resolution: h,
            surface_resolution: n,
            height: vec![encode(50.); h * h],
            biome: vec![0; cells * 5],
            splat: vec![0; cells * 8],
            topology: vec![1; cells],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        layers.biome[..cells].fill(255);
        layers.splat[..cells].fill(255);
        let path = path_geometry::build(
            &[[-100., 50., 0.], [100., 50., 0.]],
            path_geometry::Settings {
                horizontal_iterations: 0,
                vertical_iterations: 0,
                resample_distance: 7.5,
                maximum_work: 1000000,
            },
            None,
            || false,
        )
        .unwrap()
        .unwrap();
        (layers, vec![255; cells], path)
    }
    fn run(
        base: &Layers,
        alpha: &[u8],
        path: &Path,
        s: Settings,
    ) -> Result<Option<Output>, String> {
        build(
            base,
            alpha,
            None,
            &[Track {
                id: 1,
                path,
                preserve_start: true,
            }],
            &policy(),
            shape(),
            s,
            || false,
        )
    }
    #[test]
    fn authored_non_normalized_weights_survive_outside_paint_and_empty_cells_still_fail() {
        let (mut base, alpha, path) = fixture();
        let n = base.surface_resolution;
        let cells = n * n;
        base.splat[0] = 243;
        base.splat[cells + cells - 1] = 3; // Last cell totals 258.
        base.biome[0] = 250;
        let before = base.clone();
        let out = run(&base, &alpha, &path, settings()).unwrap().unwrap();
        assert_eq!(out.layers.splat[0], 243);
        assert_eq!(
            (0..8)
                .map(|c| u32::from(out.layers.splat[c * cells + cells - 1]))
                .sum::<u32>(),
            258
        );
        assert_eq!(out.layers.biome, base.biome);
        let center = (n / 2) * n + n / 2;
        assert_eq!(
            (0..8)
                .map(|c| u32::from(out.layers.splat[c * cells + center]))
                .sum::<u32>(),
            255
        );
        assert_eq!(base.splat, before.splat);
        for biome in [false, true] {
            let mut empty = base.clone();
            if biome {
                for c in 0..5 {
                    empty.biome[c * cells] = 0;
                }
            } else {
                for c in 0..8 {
                    empty.splat[c * cells] = 0;
                }
            }
            assert!(run(&empty, &alpha, &path, settings()).is_err());
        }
    }
    #[test]
    fn surface_validation_keeps_budget_and_cancellation_boundaries() {
        let (mut base, _, _) = fixture();
        let n = base.surface_resolution;
        let cells = n * n;
        let missing = n + 7;
        for plane in base.biome.chunks_exact_mut(cells) {
            plane[missing] = 0;
        }
        let validate = |maximum_work, cancelled: &dyn Fn() -> bool| {
            validate_surface_rows(
                &base,
                &mut Report::default(),
                Settings {
                    maximum_work,
                    ..settings()
                },
                &cancelled,
            )
        };
        assert_eq!(
            validate(missing, &|| false).unwrap_err(),
            "Rail bed work allowance reached"
        );
        assert_eq!(
            validate(missing + 1, &|| false).unwrap_err(),
            "Rail bed requires nonempty input biome/splat cells"
        );
        let calls = Cell::new(0);
        assert!(
            !validate(n, &|| {
                calls.set(calls.get() + 1);
                calls.get() == 2
            })
            .unwrap()
        );
    }
    #[test]
    fn grade_fit_accounts_for_full_weight_bed_vertices_beside_its_controls() {
        let (mut base, alpha, _) = fixture();
        base.height.fill(encode(82.65));
        let resolution = base.height_resolution;
        let raised_row = resolution / 2 + 1;
        let raised_column = resolution / 2 + 8;
        base.height[raised_row * resolution + raised_column - 1
            ..=raised_row * resolution + raised_column + 1]
            .fill(encode(83.));
        let path = Path::from_controls(
            &[
                [-16., 58.81961, 0.],
                [0., 58.81961, 0.],
                [16., 58.81961, 0.],
            ],
            10000,
            || false,
        )
        .unwrap()
        .unwrap();
        let tracks = [Track {
            id: 1,
            path: &path,
            preserve_start: false,
        }];
        let fitted = || {
            vec![FittedTrack {
                id: 1,
                path: path.clone(),
                preserve_start: false,
                maximum_discrete_grade: 0.,
            }]
        };
        let settings = crate::rail_grade_fit::Settings {
            maximum_grade: 0.15,
            maximum_work: 1000000,
            maximum_attempts: 8,
        };
        let mut result = fitted();
        crate::rail_bed_fit::apply(
            &base,
            &alpha,
            None,
            &tracks,
            &mut result,
            &[],
            &policy(),
            shape(),
            24.,
            settings,
            &|| false,
        )
        .unwrap()
        .unwrap();
        let allowance = 24. - shape().terrain_offset.abs() - 0.031;
        let center_height = crate::spawn::height(&base, 0.5, 0.5);
        assert!(
            58.81961 >= center_height - allowance,
            "The centre-only constraint permits the failing curve"
        );
        let minimum = decode(encode(83.)) - allowance;
        for point in &result[0].path.points()[1..] {
            assert!(point[1] >= minimum - 0.00001);
            assert!((point[1] + shape().terrain_offset - decode(encode(83.))).abs() <= 24.);
        }
        let between = result[0]
            .path
            .point(result[0].path.length() * 0.75)
            .unwrap();
        assert!((between[1] + shape().terrain_offset - decode(encode(83.))).abs() <= 24.);
        let mut exhausted = fitted();
        assert!(
            crate::rail_bed_fit::apply(
                &base,
                &alpha,
                None,
                &tracks,
                &mut exhausted,
                &[],
                &policy(),
                shape(),
                24.,
                crate::rail_grade_fit::Settings {
                    maximum_work: 1,
                    ..settings
                },
                &|| false,
            )
            .is_err()
        );
        assert_eq!(exhausted[0].path.points(), path.points());
        let calls = Cell::new(0);
        let mut cancelled = fitted();
        assert!(
            crate::rail_bed_fit::apply(
                &base,
                &alpha,
                None,
                &tracks,
                &mut cancelled,
                &[],
                &policy(),
                shape(),
                24.,
                settings,
                &|| {
                    calls.set(calls.get() + 1);
                    calls.get() > 3
                },
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(cancelled[0].path.points(), path.points());
    }

    #[test]
    fn fixed_rail_grades_can_use_the_requested_continuous_limit() {
        let (mut base, alpha, _) = fixture();
        base.topology.fill(shape().preserve_topology as i32);
        let path = Path::from_controls(
            &[[-100., 36., 0.], [0., 50., 0.], [100., 64., 0.]],
            1000000,
            || false,
        )
        .unwrap()
        .unwrap();
        let tracks = [Track {
            id: 1,
            path: &path,
            preserve_start: false,
        }];
        let mut fitted = [FittedTrack {
            id: 1,
            preserve_start: false,
            path: path.clone(),
            maximum_discrete_grade: 0.,
        }];
        let report = crate::rail_bed_fit::apply(
            &base,
            &alpha,
            None,
            &tracks,
            &mut fitted,
            &[],
            &policy(),
            shape(),
            24.,
            crate::rail_grade_fit::Settings {
                maximum_grade: 0.15,
                maximum_work: 1000000,
                maximum_attempts: 8,
            },
            &|| false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(report.attempts, 1);
        assert_eq!(fitted[0].path.points(), path.points());
        assert!(
            report.continuous_grades[0]
                .maximum_grade_upper_bound
                .unwrap()
                <= 0.15
        );
        assert!(report.effective_edge_grade > 0.14);
    }

    #[test]
    fn constrained_bed_applies_grade_correction_and_rejects_impossible_earthwork() {
        let (mut base, alpha, path) = fixture();
        let h = base.height_resolution;
        for z in 0..h {
            for x in 0..h {
                let wx = (x as f32 / (h - 1) as f32 - 0.5) * base.size as f32;
                base.height[z * h + x] = encode(50. + 10. * (wx * 0.03).sin());
            }
        }
        let before = base.height.clone();
        let mut s = settings();
        s.grade_fit = Some(crate::rail_grade_fit::Settings {
            maximum_grade: 0.1,
            maximum_work: 1000000,
            maximum_attempts: 8,
        });
        let inputs = [Track {
            id: 1,
            path: &path,
            preserve_start: false,
        }];
        let result = build_with_joins(
            &base,
            &alpha,
            None,
            &inputs,
            &[],
            &policy(),
            shape(),
            s,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            result.report.grade_fits.as_ref().unwrap().len(),
            policy().loops
        );
        let audit = crate::rail_grade::analyze(
            &result.tracks[0].path,
            crate::rail_grade::Settings {
                absolute_tolerance: 0.00001,
                maximum_work: 1000000,
                maximum_depth: 24,
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(audit.maximum_grade_upper_bound.unwrap() <= 0.1);
        assert!(result.report.changed_height_samples > 0);
        assert!(result.report.maximum_raise <= 24. && result.report.maximum_lower <= 24.);
        assert_eq!(base.height, before);
        let disconnected = [
            Track {
                id: 1,
                path: &path,
                preserve_start: false,
            },
            Track {
                id: 2,
                path: &path,
                preserve_start: false,
            },
        ];
        assert!(
            build_with_joins(
                &base,
                &alpha,
                None,
                &disconnected,
                &[],
                &policy(),
                shape(),
                s,
                || false
            )
            .err()
            .unwrap()
            .contains("disconnected")
        );
        s.maximum_cut_fill = 0.;
        assert!(
            build_with_joins(
                &base,
                &alpha,
                None,
                &inputs,
                &[],
                &policy(),
                shape(),
                s,
                || false
            )
            .is_err()
        );
        assert_eq!(base.height, before);
    }
    #[test]
    fn deferred_surfaces_keep_identical_earthwork_and_preserve_authored_channels() {
        let (mut base, alpha, path) = fixture();
        let cells = base.surface_resolution * base.surface_resolution;
        base.topology[0] |= shape().rail as i32;
        base.topology[cells - 1] |= shape().railside as i32;
        base.splat[0] = 243;
        base.splat[cells + cells - 1] = 3;
        let before = base.clone();
        for constrained in [false, true] {
            let inputs = [Track {
                id: 1,
                path: &path,
                preserve_start: !constrained,
            }];
            let mut s = settings();
            if constrained {
                s.grade_fit = Some(crate::rail_grade_fit::Settings {
                    maximum_grade: 0.1,
                    maximum_work: 1_000_000,
                    maximum_attempts: 8,
                });
            }
            let old = build_with_joins(
                &base,
                &alpha,
                None,
                &inputs,
                &[],
                &policy(),
                shape(),
                s,
                || false,
            )
            .unwrap()
            .unwrap();
            let new = build_terrain_with_joins(
                &base,
                &alpha,
                None,
                &inputs,
                &[],
                &policy(),
                shape(),
                s,
                || false,
            )
            .unwrap()
            .unwrap();
            assert_eq!(new.layers.height, old.layers.height);
            assert_eq!(
                serde_json::to_value(&new.tracks).unwrap(),
                serde_json::to_value(&old.tracks).unwrap()
            );
            assert_eq!(new.layers.topology, base.topology);
            assert_eq!(new.layers.splat, base.splat);
            assert_eq!(new.layers.biome, base.biome);
            assert_ne!(new.layers.topology, old.layers.topology);
            assert_ne!(new.layers.splat, old.layers.splat);
            assert_eq!(new.report.changed_topology_cells, 0);
            assert_eq!(new.report.changed_splat_cells, 0);
            assert_eq!(new.report.railside_cells, 0);
            assert_eq!(
                new.report.changed_height_samples,
                old.report.changed_height_samples
            );
            assert!(new.report.work < old.report.work);
            assert_eq!(new.report.surface_paint_deferred, Some(true));
            assert!(
                serde_json::to_value(&old.report)
                    .unwrap()
                    .get("surface_paint_deferred")
                    .is_none()
            );
        }
        assert_eq!(base.height, before.height);
        assert_eq!(base.topology, before.topology);
        assert_eq!(base.splat, before.splat);
    }
    #[test]
    fn deferred_earthwork_keeps_failure_and_cancellation_atomic() {
        let (base, alpha, path) = fixture();
        let before = base.clone();
        let inputs = [Track {
            id: 1,
            path: &path,
            preserve_start: true,
        }];
        let calls = Cell::new(0);
        assert!(
            build_terrain_with_joins(
                &base,
                &alpha,
                None,
                &inputs,
                &[],
                &policy(),
                shape(),
                settings(),
                || {
                    calls.set(calls.get() + 1);
                    calls.get() > 600
                }
            )
            .unwrap()
            .is_none()
        );
        let mut limited = settings();
        limited.maximum_work = 1;
        assert!(
            build_terrain_with_joins(
                &base,
                &alpha,
                None,
                &inputs,
                &[],
                &policy(),
                shape(),
                limited,
                || false
            )
            .is_err()
        );
        assert_eq!(base.height, before.height);
        assert_eq!(base.splat, before.splat);
        assert_eq!(base.topology, before.topology);
    }
    #[test]
    fn bed_changes_height_material_and_topology_but_preserves_inputs_and_biomes() {
        let (base, alpha, path) = fixture();
        let before = base.clone();
        let out = run(&base, &alpha, &path, settings()).unwrap().unwrap();
        assert!(
            out.report.changed_height_samples > 0
                && out.report.changed_splat_cells > 0
                && out.report.changed_topology_cells > 0
        );
        assert!(out.report.railside_cells > 0);
        assert!(out.report.maximum_lower > 0.);
        let n = base.surface_resolution;
        let cells = n * n;
        assert!(
            out.layers
                .topology
                .iter()
                .any(|&v| v as u32 & shape().rail != 0)
        );
        for i in 0..cells {
            assert_eq!(
                (0..8)
                    .map(|c| u32::from(out.layers.splat[c * cells + i]))
                    .sum::<u32>(),
                255
            );
        }
        assert_eq!(out.layers.biome, base.biome);
        assert_eq!(base.height, before.height);
        assert_eq!(base.topology, before.topology);
        assert_eq!(base.splat, before.splat);
        assert_eq!(out.tracks[0].path.points()[0], path.points()[0]);
    }
    #[test]
    fn monument_cells_and_fade_holes_remain_unchanged_and_core_holes_reject() {
        let (mut base, mut alpha, path) = fixture();
        let n = base.surface_resolution;
        for z in n / 2 - 8..=n / 2 + 8 {
            for x in n / 2 - 8..=n / 2 + 8 {
                base.topology[z * n + x] |= 1024;
            }
        }
        let before = base.clone();
        let out = run(&base, &alpha, &path, settings()).unwrap().unwrap();
        assert!(out.report.protected_core_samples > 0);
        for (i, &v) in base.topology.iter().enumerate() {
            if v & 1024 != 0 {
                assert_eq!(out.layers.topology[i], v);
                for c in 0..8 {
                    assert_eq!(out.layers.splat[c * n * n + i], base.splat[c * n * n + i]);
                }
            }
        }
        let h = base.height_resolution;
        assert_eq!(
            out.layers.height[h / 2 * h + h / 2],
            before.height[h / 2 * h + h / 2]
        );
        alpha[n / 2 * n + n / 2] = 0;
        assert!(run(&base, &alpha, &path, settings()).is_err());
        alpha.fill(255);
        alpha[(n / 2 + 10) * n + n / 2] = 0;
        let out = run(&base, &alpha, &path, settings()).unwrap().unwrap();
        assert!(out.report.alpha_fade_samples > 0);
    }
    #[test]
    fn missing_crossing_water_is_not_guessed_and_known_water_constrains_fitting() {
        let (mut base, alpha, path) = fixture();
        base.topology.fill(16384 | 1);
        assert!(
            run(&base, &alpha, &path, settings())
                .err()
                .unwrap()
                .contains("water surface")
        );
        let water = vec![60.; base.surface_resolution.pow(2)];
        let out = build(
            &base,
            &alpha,
            Some(&water),
            &[Track {
                id: 1,
                path: &path,
                preserve_start: false,
            }],
            &policy(),
            shape(),
            settings(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(out.report.maximum_raise > 8.);
    }
    #[test]
    fn cancellation_budget_and_cut_fill_failures_publish_no_partial_revision() {
        let (base, alpha, path) = fixture();
        let before = base.clone();
        let calls = Cell::new(0);
        assert!(
            build(
                &base,
                &alpha,
                None,
                &[Track {
                    id: 1,
                    path: &path,
                    preserve_start: true
                }],
                &policy(),
                shape(),
                settings(),
                || {
                    calls.set(calls.get() + 1);
                    calls.get() > 1000
                }
            )
            .unwrap()
            .is_none()
        );
        let mut s = settings();
        s.maximum_work = 1;
        assert!(run(&base, &alpha, &path, s).is_err());
        s = settings();
        s.maximum_cut_fill = 0.;
        assert!(run(&base, &alpha, &path, s).is_err());
        assert_eq!(base.height, before.height);
        assert_eq!(base.splat, before.splat);
        assert_eq!(base.topology, before.topology);
    }
}
