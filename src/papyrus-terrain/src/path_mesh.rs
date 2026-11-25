//! Exact path templates and bounded chunk deformation. PhysX cooking and
//! runtime object assembly remain separate from generated triangle geometry.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Template {
    pub vertices: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub tangents: Vec<[f32; 4]>,
    pub uv: Vec<[f32; 2]>,
    pub triangles: Vec<[u32; 3]>,
    pub center: [f32; 3],
    pub extents: [f32; 3],
}
impl Template {
    pub fn validate(&self) -> Result<(), String> {
        let n = self.vertices.len();
        let finite =
            |values: &[f32], bound: f32| values.iter().all(|v| v.is_finite() && v.abs() <= bound);
        if !(3..=65536).contains(&n)
            || !(1..=131072).contains(&self.triangles.len())
            || self.normals.len() != n
            || self.tangents.len() != n
            || self.uv.len() != n
            || !finite(&self.center, 10000.)
            || !finite(&self.extents, 10000.)
            || self.extents[0] <= 0.
            || self.extents[2] <= 0.
            || self.extents[1] < 0.
            || self.vertices.iter().any(|p| {
                !finite(p, 10000.)
                    || (0..3).any(|i| (p[i] - self.center[i]).abs() > self.extents[i] + 0.0001)
            })
            || self.normals.iter().any(|p| !finite(p, 2.))
            || self.tangents.iter().any(|p| !finite(p, 2.))
            || self.uv.iter().any(|p| !finite(p, 10000.))
            || self.triangles.iter().flatten().any(|&i| i as usize >= n)
        {
            return Err("Invalid or excessive path mesh template buffers".into());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub width: f32,
    pub chunk_repetitions: usize,
    pub maximum_work: usize,
    pub snap_start: bool,
    pub snap_end: bool,
    pub mesh_offset: f32,
    pub top_aligned: bool,
}
#[derive(Debug, PartialEq)]
pub enum Failure {
    Invalid,
    Budget,
    MissingHeight,
    DegenerateDirection,
    ShortPath,
    MissingRadius,
}
/// Source-bound road surfaces can supply exact radius samples and snap both
/// side rails to terrain before interpolating across the authored template.
pub struct Surface<'a> {
    pub settings: Settings,
    pub snap_to_terrain: bool,
    pub radius: &'a dyn Fn(f32) -> Option<f32>,
}
/// Exact height-query provenance for a terrain-snapped vertex, with the same
/// f32 evaluation order as deformation. This is not a quantization-free linear
/// model: a terrain repair must regenerate and audit the resulting mesh.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum HeightBinding {
    Span {
        left: [f32; 2],
        right: [f32; 2],
        across: f32,
        side_offset: f32,
        template_offset: f32,
    },
    Endpoint {
        position: [f32; 2],
    },
}
impl HeightBinding {
    pub fn resolve(&self, height: impl Fn(f32, f32) -> Option<f32>) -> Option<f32> {
        let read = |p: [f32; 2]| {
            if p.iter().any(|v| !v.is_finite() || v.abs() > 10000.) {
                return None;
            }
            height(p[0], p[1]).filter(|v| v.is_finite() && v.abs() <= 10000.)
        };
        let value = match *self {
            Self::Span {
                left,
                right,
                across,
                side_offset,
                template_offset,
            } => {
                if !(0. ..=1.).contains(&across)
                    || !side_offset.is_finite()
                    || !template_offset.is_finite()
                {
                    return None;
                }
                let a = read(left)? + side_offset;
                let b = read(right)? + side_offset;
                a + (b - a) * across + template_offset
            }
            Self::Endpoint { position } => read(position)?,
        };
        value.is_finite().then_some(value)
    }
}
pub struct TracedChunk {
    pub geometry: Chunk,
    pub heights: Vec<HeightBinding>,
}
pub struct SurfaceQueries {
    pub distances: Vec<f32>,
    pub work: usize,
}
/// Explicit opt-in finishing. Existing road/rail revisions keep their original
/// unrounded vertices and normals through the default wrappers.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Finishing {
    pub normal_smoothing: f32,
    pub round_vertices: u32,
}
pub struct FinishedSurface<'a> {
    pub surface: Surface<'a>,
    pub finishing: Finishing,
}
impl Finishing {
    fn validate(self) -> Result<(), Failure> {
        if !self.normal_smoothing.is_finite()
            || !(0. ..=1.).contains(&self.normal_smoothing)
            || self.round_vertices > 6
        {
            Err(Failure::Invalid)
        } else {
            Ok(())
        }
    }
}
fn finish_normal(value: [f32; 3], amount: f32) -> Result<[f32; 3], Failure> {
    if amount == 0. {
        return Ok(value);
    }
    let (a, length) = normalized(value)?;
    let cosine = a[1].clamp(-1., 1.);
    if cosine < -0.9995 {
        return Err(Failure::DegenerateDirection);
    }
    let b = [0., 1., 0.];
    let direction = if cosine > 0.9995 {
        normalized(std::array::from_fn(|i| a[i] + (b[i] - a[i]) * amount))?.0
    } else {
        let angle = cosine.acos();
        let denominator = angle.sin();
        std::array::from_fn(|i| {
            (a[i] * ((1. - amount) * angle).sin() + b[i] * (amount * angle).sin()) / denominator
        })
    };
    let length = length + (1. - length) * amount;
    Ok(direction.map(|v| v * length))
}
fn round_vertex(value: f32, digits: u32) -> f32 {
    if digits == 0 {
        return value;
    }
    let factor = 10_f64.powi(digits as i32);
    ((f64::from(value) * factor).round_ties_even() / factor) as f32
}
fn vertex_distance(start: f32, z: f32, min: f32, size: f32, step: f32) -> f32 {
    start + (z - min) / size * step
}
/// Enumerate the exact f32 distances used by all template LODs. A caller can
/// batch selected-installation noise once, then index its results by float bits.
pub fn surface_queries(
    path: &crate::path_geometry::Path,
    templates: &[&Template],
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<SurfaceQueries>, Failure> {
    if cancelled() {
        return Ok(None);
    }
    if !(1..=16).contains(&templates.len()) {
        return Err(Failure::Invalid);
    }
    for template in templates {
        template.validate().map_err(|_| Failure::Invalid)?;
    }
    let last = templates[templates.len() - 1];
    let plan = Plan::new(path, last, settings)?;
    let required = templates
        .iter()
        .map(|t| t.vertices.len())
        .sum::<usize>()
        .checked_mul(plan.repetitions)
        .ok_or(Failure::Budget)?;
    if required > settings.maximum_work {
        return Err(Failure::Budget);
    }
    let min = last.center[2] - last.extents[2];
    let size = last.extents[2] * 2.;
    let mut bits = std::collections::HashSet::new();
    let mut work = 0;
    for repetition in 0..plan.repetitions {
        let start = repetition as f32 * plan.step_size;
        for template in templates {
            for vertex in &template.vertices {
                if work % 256 == 0 && cancelled() {
                    return Ok(None);
                }
                work += 1;
                let distance = vertex_distance(start, vertex[2], min, size, plan.step_size);
                if !distance.is_finite() {
                    return Err(Failure::Invalid);
                }
                if !bits.contains(&distance.to_bits()) {
                    if bits.len() >= 1_000_000 {
                        return Err(Failure::Budget);
                    }
                    bits.insert(distance.to_bits());
                }
            }
        }
    }
    let mut distances: Vec<_> = bits.into_iter().map(f32::from_bits).collect();
    distances.sort_unstable_by(f32::total_cmp);
    Ok((!cancelled()).then_some(SurfaceQueries { distances, work }))
}
#[derive(Debug, Serialize)]
pub struct Plan {
    pub repetitions: usize,
    pub chunks: usize,
    pub step_size: f32,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Chunk {
    pub first_repetition: usize,
    pub repetitions: usize,
    pub origin: [f32; 3],
    pub mesh: Template,
    pub work: usize,
}
impl Plan {
    pub fn new(
        path: &crate::path_geometry::Path,
        bounds: &Template,
        settings: Settings,
    ) -> Result<Self, Failure> {
        bounds.validate().map_err(|_| Failure::Invalid)?;
        if !settings.width.is_finite()
            || !(0.1..=128.).contains(&settings.width)
            || !(1..=32).contains(&settings.chunk_repetitions)
            || !(1..=32000000).contains(&settings.maximum_work)
            || !settings.mesh_offset.is_finite()
            || settings.mesh_offset.abs() > 32.
            || path.range() != (0, path.points().len() - 1)
            || !path.length().is_finite()
        {
            return Err(Failure::Invalid);
        }
        // Preserve source operation order: width / bounds.size.x, then * size.z.
        let stride = (settings.width / (bounds.extents[0] * 2.)) * (bounds.extents[2] * 2.);
        let repeats = (path.length() / stride).floor();
        if !repeats.is_finite() || repeats > 65536. {
            return Err(Failure::Budget);
        }
        if repeats < 1. {
            return Err(Failure::ShortPath);
        }
        let repetitions = repeats as usize;
        Ok(Self {
            repetitions,
            chunks: repetitions.div_ceil(settings.chunk_repetitions),
            step_size: path.length() / repetitions as f32,
        })
    }
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn normalized(p: [f32; 3]) -> Result<([f32; 3], f32), Failure> {
    let length = dot(p, p).sqrt();
    if !length.is_finite() || length <= 0.00001 {
        return Err(Failure::DegenerateDirection);
    }
    Ok((p.map(|v| v / length), length))
}
/// Source GetTangent uses spherical interpolation of control tangents, not
/// the derivative of the Hermite position curve. Reject ambiguous reversals.
fn tangent(path: &crate::path_geometry::Path, distance: f32) -> Result<[f32; 3], Failure> {
    if !distance.is_finite() || path.length() <= 0. {
        return Err(Failure::Invalid);
    }
    let at = distance / path.length() * (path.points().len() - 1) as f32;
    let (min, max) = path.range();
    if at <= min as f32 {
        return Ok(path.tangents()[min]);
    }
    if at >= max as f32 {
        return Ok(path.tangents()[max]);
    }
    let i = at as usize;
    let t = at - i as f32;
    let (a, la) = normalized(path.tangents()[i])?;
    let (b, lb) = normalized(path.tangents()[i + 1])?;
    let cosine = dot(a, b).clamp(-1., 1.);
    if cosine < -0.9999 {
        return Err(Failure::DegenerateDirection);
    }
    let direction = if cosine > 0.9995 {
        normalized(std::array::from_fn(|k| a[k] + (b[k] - a[k]) * t))?.0
    } else {
        let angle = cosine.acos();
        let divisor = angle.sin();
        std::array::from_fn(|k| {
            (a[k] * ((1. - t) * angle).sin() + b[k] * (t * angle).sin()) / divisor
        })
    };
    Ok(direction.map(|v| v * (la + (lb - la) * t)))
}
/// Generate one immutable local-space chunk. Cancellation or failure returns no
/// partial mesh. All LODs use the LAST template's authored bounds, independently
/// of the chosen rendering/collision LOD. Terrain height is world-space metres.
pub fn deform_chunk(
    path: &crate::path_geometry::Path,
    templates: &[&Template],
    lod: usize,
    settings: Settings,
    chunk_index: usize,
    height: impl Fn(f32, f32) -> Option<f32>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Chunk>, Failure> {
    deform_impl(
        path,
        templates,
        lod,
        (settings, None, Finishing::default(), None),
        chunk_index,
        height,
        cancelled,
    )
}
pub fn deform_surface_chunk(
    path: &crate::path_geometry::Path,
    templates: &[&Template],
    lod: usize,
    surface: Surface<'_>,
    chunk_index: usize,
    height: impl Fn(f32, f32) -> Option<f32>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Chunk>, Failure> {
    deform_impl(
        path,
        templates,
        lod,
        (surface.settings, Some(surface), Finishing::default(), None),
        chunk_index,
        height,
        cancelled,
    )
}
/// Opt-in terrain-height provenance. Trace allocation is limited by the same
/// template, repetition and operation bounds as the chunk itself. Non-snapped
/// surfaces have no terrain-only binding and are explicitly rejected here.
pub fn deform_traced_surface_chunk(
    path: &crate::path_geometry::Path,
    templates: &[&Template],
    lod: usize,
    surface: Surface<'_>,
    chunk_index: usize,
    height: impl Fn(f32, f32) -> Option<f32>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<TracedChunk>, Failure> {
    if !surface.snap_to_terrain {
        return Err(Failure::Invalid);
    }
    let mut heights = Vec::new();
    let geometry = deform_impl(
        path,
        templates,
        lod,
        (
            surface.settings,
            Some(surface),
            Finishing::default(),
            Some(&mut heights),
        ),
        chunk_index,
        height,
        cancelled,
    )?;
    Ok(geometry.map(|geometry| TracedChunk { geometry, heights }))
}
pub fn deform_finished_surface_chunk(
    path: &crate::path_geometry::Path,
    templates: &[&Template],
    lod: usize,
    finished: FinishedSurface<'_>,
    chunk_index: usize,
    height: impl Fn(f32, f32) -> Option<f32>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Chunk>, Failure> {
    deform_impl(
        path,
        templates,
        lod,
        (
            finished.surface.settings,
            Some(finished.surface),
            finished.finishing,
            None,
        ),
        chunk_index,
        height,
        cancelled,
    )
}
fn deform_impl(
    path: &crate::path_geometry::Path,
    templates: &[&Template],
    lod: usize,
    config: (
        Settings,
        Option<Surface<'_>>,
        Finishing,
        Option<&mut Vec<HeightBinding>>,
    ),
    chunk_index: usize,
    height: impl Fn(f32, f32) -> Option<f32>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Chunk>, Failure> {
    let (settings, surface, finishing, mut height_trace) = config;
    if cancelled() {
        return Ok(None);
    }
    finishing.validate()?;
    if !(1..=16).contains(&templates.len()) || lod >= templates.len() {
        return Err(Failure::Invalid);
    }
    for template in templates {
        template.validate().map_err(|_| Failure::Invalid)?;
    }
    let bounds = templates[templates.len() - 1];
    let source = templates[lod];
    let plan = Plan::new(path, bounds, settings)?;
    if chunk_index >= plan.chunks {
        return Err(Failure::Invalid);
    }
    let first = chunk_index * settings.chunk_repetitions;
    let count = settings.chunk_repetitions.min(plan.repetitions - first);
    let required = count * (source.vertices.len() + source.triangles.len());
    if required > settings.maximum_work {
        return Err(Failure::Budget);
    }
    if height_trace.is_some()
        && required
            .checked_add(count * source.vertices.len())
            .is_none_or(|n| n > settings.maximum_work)
    {
        return Err(Failure::Budget);
    }
    // Source origin uses the full chunk width even for a short final chunk.
    let origin = path
        .point(
            first as f32 * plan.step_size
                + 0.5 * settings.chunk_repetitions as f32 * plan.step_size,
        )
        .map_err(|_| Failure::Invalid)?;
    let min: [f32; 3] = std::array::from_fn(|i| bounds.center[i] - bounds.extents[i]);
    let size = bounds.extents.map(|v| v * 2.);
    let mut mesh = Template {
        vertices: Vec::with_capacity(count * source.vertices.len()),
        normals: Vec::with_capacity(count * source.vertices.len()),
        tangents: Vec::with_capacity(count * source.vertices.len()),
        uv: Vec::with_capacity(count * source.vertices.len()),
        triangles: Vec::with_capacity(count * source.triangles.len()),
        center: [0.; 3],
        extents: [0.; 3],
    };
    let mut lower = [f32::INFINITY; 3];
    let mut upper = [f32::NEG_INFINITY; 3];
    let mut work = 0;
    for repetition in 0..count {
        let base = mesh.vertices.len() as u32;
        let start = (first + repetition) as f32 * plan.step_size;
        for (i, p) in source.vertices.iter().enumerate() {
            if cancelled() {
                return Ok(None);
            }
            work += 1;
            let across = ((p[0] - min[0]) / size[0]).clamp(0., 1.);
            let distance = vertex_distance(start, p[2], min[2], size[2], plan.step_size);
            let center = path.point(distance).map_err(|_| Failure::Invalid)?;
            let tan = tangent(path, distance)?;
            let forward = normalized([tan[0], 0., tan[2]])?.0;
            let side = [forward[2], 0., -forward[0]];
            let up = [
                tan[1] * side[2],
                tan[2] * side[0] - tan[0] * side[2],
                -tan[1] * side[0],
            ];
            let radius = if let Some(surface) = &surface {
                let radius = (surface.radius)(distance).ok_or(Failure::MissingRadius)?;
                if !radius.is_finite() || radius <= 0. || radius > 128. {
                    return Err(Failure::Invalid);
                }
                radius
            } else {
                settings.width * 0.5
            };
            let mut left: [f32; 3] = std::array::from_fn(|k| center[k] - side[k] * radius);
            let mut right: [f32; 3] = std::array::from_fn(|k| center[k] + side[k] * radius);
            let trace_span = height_trace
                .as_ref()
                .map(|_| ([left[0], left[2]], [right[0], right[2]]));
            if surface.as_ref().is_some_and(|s| s.snap_to_terrain) {
                for point in [&mut left, &mut right] {
                    point[1] = height(point[0], point[2])
                        .filter(|v| v.is_finite() && v.abs() <= 10000.)
                        .ok_or(Failure::MissingHeight)?;
                }
            }
            for k in 0..3 {
                left[k] += up[k] * settings.mesh_offset;
                right[k] += up[k] * settings.mesh_offset;
            }
            let mut point: [f32; 3] =
                std::array::from_fn(|k| left[k] + (right[k] - left[k]) * across);
            let snap_endpoint = (settings.snap_start && distance < 0.1)
                || (settings.snap_end && distance > path.length() - 0.1);
            let template_offset = p[1] - min[1] - if settings.top_aligned { size[1] } else { 0. };
            if snap_endpoint {
                point[1] = height(point[0], point[2])
                    .filter(|v| v.is_finite() && v.abs() <= 10000.)
                    .ok_or(Failure::MissingHeight)?;
            } else {
                point[1] += template_offset;
            }
            if let Some(trace) = &mut height_trace {
                let (left, right) = trace_span.ok_or(Failure::Invalid)?;
                trace.push(if snap_endpoint {
                    HeightBinding::Endpoint {
                        position: [point[0], point[2]],
                    }
                } else {
                    HeightBinding::Span {
                        left,
                        right,
                        across,
                        side_offset: up[1] * settings.mesh_offset,
                        template_offset,
                    }
                });
            }
            let local: [f32; 3] = std::array::from_fn(|k| {
                round_vertex(point[k] - origin[k], finishing.round_vertices)
            });
            if local.iter().any(|v| !v.is_finite() || v.abs() > 10000.) {
                return Err(Failure::Invalid);
            }
            for k in 0..3 {
                lower[k] = lower[k].min(local[k]);
                upper[k] = upper[k].max(local[k]);
            }
            // LookRotation(horizontal forward, cross(tangent, side)) has a
            // vertical re-orthogonalized up axis: this is yaw, not slope pitch.
            let rotate = |v: [f32; 3]| {
                [
                    side[0] * v[0] + forward[0] * v[2],
                    v[1],
                    side[2] * v[0] + forward[2] * v[2],
                ]
            };
            mesh.vertices.push(local);
            mesh.normals.push(finish_normal(
                rotate(source.normals[i]),
                finishing.normal_smoothing,
            )?);
            let old = source.tangents[i];
            let rotated = rotate([old[0], old[1], old[2]]);
            mesh.tangents
                .push([rotated[0], rotated[1], rotated[2], old[3]]);
            mesh.uv.push(source.uv[i]);
        }
        for face in &source.triangles {
            if cancelled() {
                return Ok(None);
            }
            work += 1;
            mesh.triangles.push(face.map(|i| i + base));
        }
    }
    mesh.center = std::array::from_fn(|i| (lower[i] + upper[i]) * 0.5);
    mesh.extents = std::array::from_fn(|i| (upper[i] - lower[i]) * 0.5);
    Ok(Some(Chunk {
        first_repetition: first,
        repetitions: count,
        origin,
        mesh,
        work,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finishing_rounds_like_the_managed_source_and_preserves_legacy_defaults() {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/mesh-rounding.json")).unwrap();
        for case in v["cases"].as_array().unwrap() {
            let value = case["value"].as_f64().unwrap() as f32;
            let expected = case["expected"].as_f64().unwrap() as f32;
            assert_eq!(
                round_vertex(value, case["digits"].as_u64().unwrap() as u32).to_bits(),
                expected.to_bits()
            );
            assert_eq!(round_vertex(value, 0).to_bits(), value.to_bits());
        }
        assert_eq!(finish_normal([0., 1., 0.], 0.1).unwrap(), [0., 1., 0.]);
        let n = [0.6, 0.8, 0.];
        let got = finish_normal(n, 0.1).unwrap();
        assert!(got[1] > n[1]);
        assert!((dot(got, got) - 1.).abs() < 0.00001);
        assert_eq!(finish_normal(n, 0.).unwrap(), n);
        assert!(finish_normal([0., -1., 0.], 0.1).is_err());
    }
    fn fixture() -> Template {
        Template {
            vertices: vec![[0., 0., 0.], [1., 0., 0.], [0., 0., 2.]],
            normals: vec![[0., 1., 0.]; 3],
            tangents: vec![[1., 0., 0., -1.]; 3],
            uv: vec![[0., 0.], [1., 0.], [0., 1.]],
            triangles: vec![[0, 1, 2]],
            center: [0.5, 0., 1.],
            extents: [0.5, 0., 1.],
        }
    }
    #[test]
    fn exact_template_buffers_validate_without_reordering_or_simplification() {
        let template = fixture();
        let before = serde_json::to_vec(&template).unwrap();
        template.validate().unwrap();
        assert_eq!(serde_json::to_vec(&template).unwrap(), before);
    }
    #[test]
    fn direct_calls_reject_nonfinite_mismatched_degenerate_and_outside_buffers() {
        for invalid in 0..10 {
            let mut t = fixture();
            match invalid {
                0 => t.vertices[0][0] = f32::NAN,
                1 => t.normals[0][0] = f32::INFINITY,
                2 => t.tangents[0][3] = 3.,
                3 => {
                    t.uv.pop();
                }
                4 => t.triangles[0][2] = 3,
                5 => t.extents[0] = 0.,
                6 => t.extents[1] = -1.,
                7 => t.center[0] = 50.,
                8 => t.vertices[0][0] = 10001.,
                _ => t.normals.clear(),
            }
            assert!(t.validate().is_err(), "case {invalid}");
        }
    }
    fn settings() -> Settings {
        Settings {
            width: 1.,
            chunk_repetitions: 5,
            maximum_work: 10000,
            snap_start: false,
            snap_end: false,
            mesh_offset: 0.,
            top_aligned: false,
        }
    }
    fn straight() -> crate::path_geometry::Path {
        crate::path_geometry::Path::from_controls(&[[0., 10., 0.], [0., 10., 13.]], 1000, || false)
            .unwrap()
            .unwrap()
    }
    #[test]
    fn traced_vertices_reproduce_geometry_and_endpoint_override_exactly() {
        let path = crate::path_geometry::Path::from_controls(
            &[[0., 10., 0.], [3., 13., 6.], [6., 17., 13.]],
            1000,
            || false,
        )
        .unwrap()
        .unwrap();
        let mut template = fixture();
        template.vertices[2][1] = 0.25;
        template.center[1] = 0.125;
        template.extents[1] = 0.125;
        let radius = |d: f32| Some(1. + d * 0.01);
        let height = |x: f32, z: f32| Some(x * x * 0.1 + z * 0.2 + 12.);
        for (snap, top_aligned) in [(false, false), (true, false), (false, true), (true, true)] {
            let cfg = Settings {
                width: 4.,
                mesh_offset: 0.7,
                snap_start: snap,
                snap_end: snap,
                top_aligned,
                ..settings()
            };
            let plan = Plan::new(&path, &template, cfg).unwrap();
            for chunk in 0..plan.chunks {
                let surface = || Surface {
                    settings: cfg,
                    snap_to_terrain: true,
                    radius: &radius,
                };
                let plain =
                    deform_surface_chunk(&path, &[&template], 0, surface(), chunk, height, || {
                        false
                    })
                    .unwrap()
                    .unwrap();
                let traced = deform_traced_surface_chunk(
                    &path,
                    &[&template],
                    0,
                    surface(),
                    chunk,
                    height,
                    || false,
                )
                .unwrap()
                .unwrap();
                assert_eq!(
                    serde_json::to_vec(&plain).unwrap(),
                    serde_json::to_vec(&traced.geometry).unwrap()
                );
                assert_eq!(traced.heights.len(), plain.mesh.vertices.len());
                for (binding, p) in traced.heights.iter().zip(&plain.mesh.vertices) {
                    let local = binding.resolve(height).unwrap() - plain.origin[1];
                    assert_eq!(local.to_bits(), p[1].to_bits());
                }
                if snap && chunk == 0 {
                    assert!(matches!(traced.heights[0], HeightBinding::Endpoint { .. }));
                }
            }
        }
        let surface = || Surface {
            settings: settings(),
            snap_to_terrain: true,
            radius: &radius,
        };
        assert!(
            deform_traced_surface_chunk(&path, &[&template], 0, surface(), 0, height, || true)
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            deform_traced_surface_chunk(
                &path,
                &[&template],
                0,
                Surface {
                    settings: Settings {
                        maximum_work: 30,
                        ..settings()
                    },
                    snap_to_terrain: true,
                    radius: &radius
                },
                0,
                height,
                || false
            ),
            Err(Failure::Budget)
        ));
        assert_eq!(
            HeightBinding::Endpoint {
                position: [f32::NAN, 0.]
            }
            .resolve(|_, _| Some(1.)),
            None
        );
    }
    #[test]
    fn surface_queries_cover_exact_vertex_distances_across_all_lods_and_chunks() {
        let path = straight();
        let last = fixture();
        let mut detailed = fixture();
        detailed.vertices[2][2] = 1.;
        let templates = [&detailed, &last];
        let cfg = settings();
        let query = surface_queries(&path, &templates, cfg, || false)
            .unwrap()
            .unwrap();
        let expected: std::collections::HashSet<_> =
            query.distances.iter().map(|v| v.to_bits()).collect();
        let seen = std::cell::RefCell::new(std::collections::HashSet::new());
        let radius = |d: f32| {
            assert!(expected.contains(&d.to_bits()));
            seen.borrow_mut().insert(d.to_bits());
            Some(0.5)
        };
        let plan = Plan::new(&path, &last, cfg).unwrap();
        for lod in 0..2 {
            for chunk in 0..plan.chunks {
                assert!(
                    deform_surface_chunk(
                        &path,
                        &templates,
                        lod,
                        Surface {
                            settings: cfg,
                            snap_to_terrain: false,
                            radius: &radius
                        },
                        chunk,
                        |_, _| None,
                        || false
                    )
                    .unwrap()
                    .is_some()
                );
            }
        }
        assert_eq!(*seen.borrow(), expected);
        assert_eq!(query.work, plan.repetitions * 6);
        assert!(query.distances.windows(2).all(|p| p[0] < p[1]));
    }
    #[test]
    fn variable_width_surfaces_snap_sides_before_offset_and_endpoint_override() {
        let path = straight();
        let mut template = fixture();
        template.vertices.push([0.5, 0., 0.]);
        template.normals.push([0., 1., 0.]);
        template.tangents.push([1., 0., 0., -1.]);
        template.uv.push([0.5, 0.]);
        template.triangles = vec![[0, 3, 2], [3, 1, 2]];
        let cfg = Settings {
            width: 4.,
            mesh_offset: 1.,
            ..settings()
        };
        let radius = |d: f32| Some(if d == 0. { 2. } else { 3. });
        let run = |snap_start| {
            deform_surface_chunk(
                &path,
                &[&template],
                0,
                Surface {
                    settings: Settings { snap_start, ..cfg },
                    snap_to_terrain: true,
                    radius: &radius,
                },
                0,
                |x, z| Some(x * x + z),
                || false,
            )
            .unwrap()
            .unwrap()
        };
        let chunk = run(false);
        let world = |i: usize| {
            std::array::from_fn::<_, 3, _>(|a| chunk.mesh.vertices[i][a] + chunk.origin[a])
        };
        assert_eq!(world(0), [-2., 5., 0.]);
        assert_eq!(world(1), [2., 5., 0.]);
        assert_eq!(world(2), [-3., 23., 13.]);
        // The center is interpolated between terrain-snapped side rails. It is
        // deliberately not independently snapped to the terrain at X=0.
        assert_eq!(world(3), [0., 5., 0.]);
        let snapped = run(true);
        assert_eq!(snapped.mesh.vertices[3][1] + snapped.origin[1], 0.);
        assert_eq!(chunk.mesh.uv, template.uv);
        assert_eq!(chunk.mesh.triangles, template.triangles);
    }
    #[test]
    fn missing_surface_samples_and_bounded_queries_never_publish_partial_geometry() {
        let path = straight();
        let template = fixture();
        let missing = |_| None;
        assert_eq!(
            deform_surface_chunk(
                &path,
                &[&template],
                0,
                Surface {
                    settings: settings(),
                    snap_to_terrain: true,
                    radius: &missing
                },
                0,
                |_, _| Some(0.),
                || false
            )
            .unwrap_err(),
            Failure::MissingRadius
        );
        for value in [0., -1., 129., f32::NAN, f32::INFINITY] {
            let radius = |_| Some(value);
            assert_eq!(
                deform_surface_chunk(
                    &path,
                    &[&template],
                    0,
                    Surface {
                        settings: settings(),
                        snap_to_terrain: true,
                        radius: &radius
                    },
                    0,
                    |_, _| Some(0.),
                    || false
                )
                .unwrap_err(),
                Failure::Invalid
            );
        }
        let valid = |_| Some(0.5);
        assert_eq!(
            deform_surface_chunk(
                &path,
                &[&template],
                0,
                Surface {
                    settings: settings(),
                    snap_to_terrain: true,
                    radius: &valid
                },
                0,
                |_, _| None,
                || false
            )
            .unwrap_err(),
            Failure::MissingHeight
        );
        assert_eq!(
            surface_queries(
                &path,
                &[&template],
                Settings {
                    maximum_work: 1,
                    ..settings()
                },
                || false
            )
            .err(),
            Some(Failure::Budget)
        );
        assert!(
            surface_queries(&path, &[&template], settings(), || true)
                .unwrap()
                .is_none()
        );
        assert!(
            deform_surface_chunk(
                &path,
                &[&template],
                0,
                Surface {
                    settings: settings(),
                    snap_to_terrain: true,
                    radius: &valid
                },
                0,
                |_, _| None,
                || true
            )
            .unwrap()
            .is_none()
        );
    }
    #[test]
    fn chunks_follow_shared_last_lod_bounds_and_preserve_vertex_streams() {
        let path = straight();
        let last = fixture();
        let mut detailed = fixture();
        detailed.vertices[1][0] = 2.;
        detailed.center[0] = 1.;
        detailed.extents[0] = 1.;
        let templates = [&detailed, &last];
        let cfg = settings();
        let plan = Plan::new(&path, &last, cfg).unwrap();
        assert_eq!((plan.repetitions, plan.chunks), (6, 2));
        let chunk = deform_chunk(&path, &templates, 0, cfg, 0, |_, _| None, || false)
            .unwrap()
            .unwrap();
        assert_eq!(chunk.mesh.vertices.len(), 15);
        assert_eq!(chunk.mesh.triangles[1], [3, 4, 5]);
        // LOD0's wider vertex clamps to the last LOD's width domain.
        assert!((chunk.mesh.vertices[1][0] - 0.5).abs() < 0.00001);
        assert_eq!(chunk.mesh.normals[0], detailed.normals[0]);
        assert_eq!(chunk.mesh.tangents[0], detailed.tangents[0]);
        assert_eq!(chunk.mesh.uv[4], detailed.uv[1]);
        let final_chunk = deform_chunk(&path, &templates, 0, cfg, 1, |_, _| None, || false)
            .unwrap()
            .unwrap();
        assert_eq!(
            (final_chunk.first_repetition, final_chunk.repetitions),
            (5, 1)
        );
        assert_eq!(final_chunk.origin, [0., 10., 13.]);
        assert!((final_chunk.mesh.vertices[2][2]).abs() < 0.00001);
    }
    #[test]
    fn endpoint_snapping_replaces_height_and_failures_never_publish_partial_meshes() {
        let path = straight();
        let template = fixture();
        let templates = [&template];
        let mut cfg = settings();
        cfg.snap_start = true;
        cfg.snap_end = true;
        assert_eq!(
            deform_chunk(&path, &templates, 0, cfg, 0, |_, _| None, || false).unwrap_err(),
            Failure::MissingHeight
        );
        let chunk = deform_chunk(&path, &templates, 0, cfg, 0, |_, _| Some(50.), || false)
            .unwrap()
            .unwrap();
        assert_eq!(chunk.mesh.vertices[0][1] + chunk.origin[1], 50.);
        cfg.maximum_work = 1;
        assert_eq!(
            deform_chunk(&path, &templates, 0, cfg, 0, |_, _| Some(50.), || false).unwrap_err(),
            Failure::Budget
        );
        assert!(
            deform_chunk(&path, &templates, 0, cfg, 0, |_, _| None, || true)
                .unwrap()
                .is_none()
        );
        let short =
            crate::path_geometry::Path::from_controls(&[[0., 0., 0.], [0., 0., 0.5]], 1000, || {
                false
            })
            .unwrap()
            .unwrap();
        assert_eq!(
            Plan::new(&short, &template, settings()).unwrap_err(),
            Failure::ShortPath
        );
    }
    #[test]
    fn normals_rotate_with_horizontal_heading_and_reject_vertical_or_reversed_directions() {
        let template = fixture();
        let templates = [&template];
        let path = crate::path_geometry::Path::from_controls(
            &[[0., 10., 0.], [13., 12., 0.]],
            1000,
            || false,
        )
        .unwrap()
        .unwrap();
        let chunk = deform_chunk(&path, &templates, 0, settings(), 0, |_, _| None, || false)
            .unwrap()
            .unwrap();
        assert_eq!(chunk.mesh.normals[0], [0., 1., 0.]);
        assert_eq!(chunk.mesh.tangents[0], [0., 0., -1., -1.]);
        let vertical =
            crate::path_geometry::Path::from_controls(&[[0., 0., 0.], [0., 20., 0.]], 1000, || {
                false
            })
            .unwrap()
            .unwrap();
        assert_eq!(
            deform_chunk(
                &vertical,
                &templates,
                0,
                settings(),
                0,
                |_, _| None,
                || false
            )
            .unwrap_err(),
            Failure::DegenerateDirection
        );
    }
}
