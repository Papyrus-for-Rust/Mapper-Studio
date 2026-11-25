//! Final-terrain source-template deformation. No PhysX cooking or runtime objects.
use crate::{Project, native_noise, native_profile, road_surface_source::Source};
use papyrus_terrain::{
    native_layers::Layers,
    path_geometry::Path,
    path_mesh,
    road_network::{Report, SurfaceChunk, SurfaceOutput},
};
use serde_json::json;
use std::{cell::Cell, collections::HashMap, path::Path as FilePath};
const MAX_WORK: usize = 32_000_000;
const MAX_CHUNKS: usize = 8192;
const MAX_BYTES: u64 = 256 * 1024 * 1024;
pub struct Context<'a> {
    pub project: &'a Project,
    pub root: &'a FilePath,
    pub worker: &'a FilePath,
    pub layers: &'a Layers,
}
/// World-height layout supplied by the selected installation's height policy.
#[derive(Clone, Copy, serde::Serialize)]
pub struct HeightGeometry {
    pub origin_y: f32,
    pub vertical_span: f32,
}
#[derive(serde::Serialize)]
pub struct HeightTrace {
    pub road: usize,
    pub chunk_index: usize,
    pub heights: Vec<path_mesh::HeightBinding>,
}
pub struct TracedOutput {
    pub surfaces: SurfaceOutput,
    pub traces: Vec<HeightTrace>,
}

/// Encoded height cells read during deformation, including lower detail meshes.
/// Only retained for the duration of one composition.
pub(crate) struct MeshHeightSupport {
    length: usize,
    samples: Vec<(u32, i16)>,
}

impl MeshHeightSupport {
    fn capture(mask: &[u64], height: &[i16]) -> Option<Self> {
        let count: usize = mask.iter().map(|word| word.count_ones() as usize).sum();
        if count > 262_144 {
            return None;
        }
        let mut samples = Vec::with_capacity(count);
        for (word, &bits) in mask.iter().enumerate() {
            let mut bits = bits;
            while bits != 0 {
                let index = word * 64 + bits.trailing_zeros() as usize;
                samples.push((index as u32, height[index]));
                bits &= bits - 1;
            }
        }
        Some(Self { length: height.len(), samples })
    }

    pub(crate) fn matches(&self, height: &[i16], cancelled: impl Fn() -> bool) -> bool {
        height.len() == self.length
            && self.samples.chunks(256).all(|group| {
                !cancelled() && group.iter().all(|&(index, value)| height[index as usize] == value)
            })
    }
}
pub struct ClearanceAudit {
    pub report: serde_json::Value,
    pub probes: Vec<papyrus_terrain::road_bed::ClearanceProbe>,
}
/// Mesh applicability comes from the installation's hierarchy policy, not from
/// whether a rebuild happened to return any triangles.
pub(crate) fn mesh_roads(
    roads: &Report,
    maximum_hierarchy_exclusive: u32,
) -> Result<Vec<bool>, String> {
    let mut expected = vec![None; roads.roads.len()];
    for family in &roads.families {
        let end = family
            .first_road
            .checked_add(family.road_count)
            .filter(|&end| end <= expected.len())
            .ok_or("Invalid road surface family range")?;
        for slot in &mut expected[family.first_road..end] {
            if slot
                .replace(family.hierarchy < maximum_hierarchy_exclusive)
                .is_some()
            {
                return Err("Road surface family coverage is ambiguous".into());
            }
        }
    }
    expected
        .into_iter()
        .map(|v| v.ok_or_else(|| "Missing road surface family".into()))
        .collect()
}
pub(crate) fn collision_coverage(
    expected: &[bool],
    surfaces: &SurfaceOutput,
) -> Result<bool, String> {
    let mut seen = vec![false; expected.len()];
    for chunk in &surfaces.chunks {
        if expected.get(chunk.road) != Some(&true) {
            return Err("Unexpected road surface geometry".into());
        }
        if chunk.lod == 0 {
            seen[chunk.road] = true;
        }
    }
    if expected != seen {
        return Err("Missing paved road collision geometry".into());
    }
    Ok(expected.iter().any(|&v| v))
}
pub(crate) fn clearance_accepted(has_meshes: bool, audit: &ClearanceAudit) -> bool {
    if has_meshes {
        audit.report["passed"] == true
    } else {
        audit.report["passed"].is_null()
            && audit.report["collision_chunks"] == 0
            && audit.report["driving_triangles"] == 0
            && audit.probes.is_empty()
    }
}
/// Shared GUI/CLI worker primitive. Reads private generated buffers and keeps
/// the complete audit/probe proposal private on cancellation or budget failure.
pub fn audit_clearance(
    grid: papyrus_terrain::path_terrain::Grid<'_>,
    surfaces: &SurfaceOutput,
    template: &path_mesh::Template,
    settings: papyrus_terrain::road_clearance::Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<ClearanceAudit>, String> {
    settings
        .validate()
        .map_err(|e| format!("Invalid road clearance settings: {e:?}"))?;
    let mut seen = std::collections::BTreeSet::new();
    let mut audits = Vec::new();
    let mut probes = Vec::new();
    let mut work = 0usize;
    let (mut triangles, mut continuous, mut source_only, mut confirmed) =
        (0usize, 0usize, 0usize, 0usize);
    let mut maximum = f64::NEG_INFINITY;
    if surfaces.chunks.len() > MAX_CHUNKS {
        return Err("Too many road clearance chunks".into());
    }
    for chunk in surfaces.chunks.iter().filter(|c| c.lod == 0) {
        if !seen.insert((chunk.road, chunk.chunk_index)) {
            return Err("Duplicate road clearance collision chunk".into());
        }
        let result = papyrus_terrain::road_clearance::audit(
            papyrus_terrain::road_clearance::Input {
                grid,
                chunk: &chunk.geometry,
                template,
                road: chunk.road,
                chunk_index: chunk.chunk_index,
            },
            papyrus_terrain::road_clearance::Settings {
                maximum_work: settings
                    .maximum_work
                    .checked_sub(work)
                    .filter(|v| *v > 0)
                    .ok_or("Road clearance work budget exceeded")?,
                maximum_probes: settings
                    .maximum_probes
                    .checked_sub(probes.len())
                    .filter(|v| *v > 0)
                    .ok_or("Road clearance probe budget exceeded")?,
                ..settings
            },
            &cancelled,
        );
        let result = match result {
            Ok(v) => v,
            Err(papyrus_terrain::road_clearance::Failure::Cancelled) => return Ok(None),
            Err(e) => return Err(format!("Road clearance audit failed: {e:?}")),
        };
        work += result.report.work;
        triangles += result.report.driving_triangles;
        continuous += result.report.continuous_intersections;
        source_only += result.report.source_only_intersections;
        confirmed += result.report.source_sampler_confirmed_intersections;
        maximum = maximum.max(result.report.maximum_continuous_terrain_excess);
        probes.extend(result.probes);
        audits
            .push(json!({"road":chunk.road,"chunk_index":chunk.chunk_index,"audit":result.report}));
    }
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(ClearanceAudit {
        report: json!({"engine":papyrus_terrain::road_clearance::VERSION,
        "collision_chunks":audits.len(),"driving_triangles":triangles,"continuous_intersections":continuous,
        "source_only_intersections":source_only,"source_sampler_confirmed_intersections":confirmed,
        "maximum_continuous_terrain_excess":if maximum.is_finite(){Some(maximum)}else{None},
        "tolerance":settings.tolerance,"work":work,"probes":probes.len(),"chunks":audits,
        "passed":if triangles>0{Some(continuous==0 && confirmed==0)}else{None},
        "runtime_physics_validated":false,"water_clearance_validated":false,"playable":false}),
        probes,
    }))
}

/// Generates source geometry and clearance witnesses in memory for the bounded
/// bed transaction. A failing audit is returned for correction, not acceptance.
pub fn generate_audited_with_height_geometry(
    context: Context<'_>,
    roads: &Report,
    source: &Source,
    geometry: HeightGeometry,
    settings: papyrus_terrain::road_clearance::Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<(TracedOutput, ClearanceAudit)>, String> {
    let grid = papyrus_terrain::path_terrain::Grid {
        size: context.layers.size as f32,
        resolution: context.layers.height_resolution,
        origin_y: geometry.origin_y,
        vertical_span: geometry.vertical_span,
        height: &context.layers.height,
    };
    let Some(mut output) =
        generate_traced_with_height_geometry(context, roads, source, geometry, &cancelled)?
    else {
        return Ok(None);
    };
    let Some(audit) = audit_clearance(
        grid,
        &output.surfaces,
        &source.meshes[0].buffer,
        settings,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    output.surfaces.report["clearance_audit"] = audit.report.clone();
    Ok(Some((output, audit)))
}
fn validate_geometry(geometry: HeightGeometry) -> Result<(), String> {
    if !geometry.origin_y.is_finite()
        || geometry.origin_y.abs() > 10000.
        || !geometry.vertical_span.is_finite()
        || !(1. ..=20000.).contains(&geometry.vertical_span)
    {
        return Err("Invalid bound road surface height geometry".into());
    }
    Ok(())
}

pub fn generate(
    context: Context<'_>,
    roads: &Report,
    source: &Source,
    cancelled: impl Fn() -> bool,
) -> Result<Option<SurfaceOutput>, String> {
    generate_impl(context, roads, source, None, None, (None, None), cancelled)
}
pub fn generate_with_height_geometry(
    context: Context<'_>,
    roads: &Report,
    source: &Source,
    geometry: HeightGeometry,
    cancelled: impl Fn() -> bool,
) -> Result<Option<SurfaceOutput>, String> {
    validate_geometry(geometry)?;
    generate_impl(
        context,
        roads,
        source,
        Some(geometry),
        None,
        (None, None),
        cancelled,
    )
}
/// Collision-LOD provenance is explicit and budgeted. A cancelled/failed run
/// discards the private geometry and traces together.
pub fn generate_traced_with_height_geometry(
    context: Context<'_>,
    roads: &Report,
    source: &Source,
    geometry: HeightGeometry,
    cancelled: impl Fn() -> bool,
) -> Result<Option<TracedOutput>, String> {
    validate_geometry(geometry)?;
    let mut traces = Vec::new();
    let surfaces = generate_impl(
        context,
        roads,
        source,
        Some(geometry),
        Some(&mut traces),
        (None, None),
        cancelled,
    )?;
    Ok(surfaces.map(|surfaces| TracedOutput { surfaces, traces }))
}
/// Rebuild over a private height candidate without copying unrelated channels
/// or replacing the source Layers held by the generation worker.
pub fn generate_traced_on_height(
    context: Context<'_>,
    roads: &Report,
    source: &Source,
    geometry: HeightGeometry,
    height: &[i16],
    cancelled: impl Fn() -> bool,
) -> Result<Option<TracedOutput>, String> {
    validate_geometry(geometry)?;
    let mut traces = Vec::new();
    let surfaces = generate_impl(
        context,
        roads,
        source,
        Some(geometry),
        Some(&mut traces),
        (Some(height), None),
        cancelled,
    )?;
    Ok(surfaces.map(|surfaces| TracedOutput { surfaces, traces }))
}

pub(crate) fn generate_traced_with_support(
    context: Context<'_>,
    roads: &Report,
    source: &Source,
    geometry: HeightGeometry,
    cancelled: impl Fn() -> bool,
) -> Result<Option<(TracedOutput, Option<MeshHeightSupport>)>, String> {
    validate_geometry(geometry)?;
    let height = &context.layers.height;
    let mut mask = vec![0; height.len().div_ceil(64)];
    let mut traces = Vec::new();
    let surfaces = generate_impl(
        context,
        roads,
        source,
        Some(geometry),
        Some(&mut traces),
        (None, Some(Cell::from_mut(mask.as_mut_slice()).as_slice_of_cells())),
        &cancelled,
    )?;
    let Some(surfaces) = surfaces else { return Ok(None) };
    let support = MeshHeightSupport::capture(&mask, height);
    Ok((!cancelled()).then_some((TracedOutput { surfaces, traces }, support)))
}

fn generate_impl(
    context: Context<'_>,
    roads: &Report,
    source: &Source,
    height_geometry: Option<HeightGeometry>,
    mut traces: Option<&mut Vec<HeightTrace>>,
    (height_override, height_reads): (Option<&[i16]>, Option<&[Cell<u64>]>),
    cancelled: impl Fn() -> bool,
) -> Result<Option<SurfaceOutput>, String> {
    if cancelled() {
        return Ok(None);
    }
    let height_values = height_override.unwrap_or(&context.layers.height);
    let recipe = context
        .project
        .roads
        .as_ref()
        .ok_or("Missing road recipe")?;
    recipe.validate()?;
    if recipe.surface_sha256.as_deref() != Some(&source.sha256)
        || recipe.source_sha256 != source.parent_digest()
        || ![
            papyrus_terrain::road_network::SHAPED_VERSION,
            papyrus_terrain::road_network::RING_VERSION,
            papyrus_terrain::road_network::COST_VERSION,
        ]
        .contains(&recipe.engine.as_str())
        || context.layers.size != context.project.size
        || context.layers.terrain_seed != context.project.seeds[0].value
        || context.layers.biome_seed != context.project.seeds[1].value
    {
        return Err("Road surface recipe, source or terrain mismatch".into());
    }
    let policy = source.policy();
    policy.validate()?;
    if traces.is_some() && !policy.snap_to_terrain {
        return Err("Terrain height tracing requires terrain-snapped road surfaces".into());
    }
    let templates: Vec<_> = source.meshes.iter().map(|m| &m.buffer).collect();
    let last = templates.last().ok_or("Missing road mesh templates")?;
    let (hr, _) = papyrus_terrain::native_layers::dimensions(context.layers.size)?;
    if context.layers.height_resolution != hr
        || height_values.len() != hr * hr
        || roads.roads.len() > 256
        || roads.families.len() > 16
        || roads.families.iter().any(|f| {
            f.first_road
                .checked_add(f.road_count)
                .is_none_or(|end| end > roads.roads.len())
        })
    {
        return Err("Invalid final road surface inputs".into());
    }
    let mut schedules = Vec::new();
    let (mut work, mut chunks, mut bytes) = (0usize, 0usize, 0u64);
    let (mut trace_vertices, mut trace_bytes) = (0usize, 0u64);
    let mut coordinates = Vec::new();
    for (index, road) in roads.roads.iter().enumerate() {
        if cancelled() {
            return Ok(None);
        }
        let families: Vec<_> = roads
            .families
            .iter()
            .filter(|f| index >= f.first_road && index < f.first_road + f.road_count)
            .collect();
        if families.len() != 1 {
            return Err("Road surface family coverage is ambiguous".into());
        }
        let family = families[0];
        if family.hierarchy >= policy.maximum_hierarchy_exclusive {
            continue;
        }
        let Some(path) = Path::from_controls(&road.points, MAX_WORK, &cancelled)
            .map_err(|e| format!("Road surface controls: {e:?}"))?
        else {
            return Ok(None);
        };
        let settings = path_mesh::Settings {
            width: family.width,
            chunk_repetitions: policy.chunk_repetitions,
            maximum_work: MAX_WORK,
            snap_start: policy.snap_open_endpoints && !path.is_circular(),
            snap_end: policy.snap_open_endpoints && !path.is_circular(),
            mesh_offset: policy.mesh_offset,
            top_aligned: policy.top_aligned,
        };
        let plan = path_mesh::Plan::new(&path, last, settings)
            .map_err(|e| format!("Road surface planning failed: {e:?}"))?;
        let Some(queries) = path_mesh::surface_queries(&path, &templates, settings, &cancelled)
            .map_err(|e| format!("Road radius query budget: {e:?}"))?
        else {
            return Ok(None);
        };
        let start = coordinates.len();
        if start + queries.distances.len() > native_noise::MAX_SAMPLES {
            return Err("Road surface noise allowance exceeded".into());
        }
        coordinates.extend(
            queries
                .distances
                .iter()
                .map(|d| *d * policy.noise_frequency),
        );
        work = work
            .checked_add(queries.work)
            .ok_or("Road surface work overflow")?;
        for template in &templates {
            let vertices = plan
                .repetitions
                .checked_mul(template.vertices.len())
                .ok_or("Road vertex overflow")?;
            let faces = plan
                .repetitions
                .checked_mul(template.triangles.len())
                .ok_or("Road face overflow")?;
            work = work
                .checked_add(vertices)
                .and_then(|n| n.checked_add(faces))
                .ok_or("Road work overflow")?;
            chunks = chunks
                .checked_add(plan.chunks)
                .ok_or("Road chunk overflow")?;
            bytes = bytes
                .checked_add(vertices as u64 * 48 + faces as u64 * 12)
                .ok_or("Road buffer overflow")?;
        }
        if traces.is_some() {
            let count = plan
                .repetitions
                .checked_mul(templates[0].vertices.len())
                .ok_or("Road trace vertex overflow")?;
            trace_vertices = trace_vertices
                .checked_add(count)
                .ok_or("Road trace count overflow")?;
            trace_bytes = trace_bytes
                .checked_add(count as u64 * std::mem::size_of::<path_mesh::HeightBinding>() as u64)
                .ok_or("Road trace buffer overflow")?;
            work = work.checked_add(count).ok_or("Road trace work overflow")?;
        }
        if work > MAX_WORK
            || chunks > MAX_CHUNKS
            || bytes.checked_add(trace_bytes).is_none_or(|n| n > MAX_BYTES)
        {
            return Err("Road surface work/chunk/memory allowance exceeded".into());
        }
        schedules.push((index, path, settings, plan, queries.distances, start));
    }
    let noise = if coordinates.is_empty() {
        Vec::new()
    } else {
        let profile = native_profile::resolve(&context.project.terrain_key())?;
        let Some(values) = native_noise::sample(
            context.root,
            context.worker,
            context
                .project
                .installation
                .as_ref()
                .ok_or("Missing road surface installation")?,
            &profile,
            &coordinates,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        values
    };
    let mut geometry = Vec::with_capacity(chunks);
    let mut entries = Vec::new();
    for (index, path, settings, plan, distances, start) in schedules {
        let radii: HashMap<_, _> = distances
            .iter()
            .enumerate()
            .map(|(i, d)| {
                policy
                    .radius(settings.width, noise[start + i])
                    .map(|r| (d.to_bits(), r))
                    .ok_or("Invalid source-derived road radius")
            })
            .collect::<Result<_, _>>()?;
        let radius = |d: f32| radii.get(&d.to_bits()).copied();
        let height = |x: f32, z: f32| {
            if let Some(geometry) = height_geometry {
                if let Some(reads) = height_reads {
                    return papyrus_terrain::path_terrain::sample_height_with(
                        papyrus_terrain::path_terrain::Layout {
                            size: context.layers.size as f32,
                            resolution: context.layers.height_resolution,
                            origin_y: geometry.origin_y,
                            vertical_span: geometry.vertical_span,
                        },
                        [x, 0., z],
                        |i| height_values.get(i).copied().inspect(|_| {
                            let word = &reads[i / 64];
                            word.set(word.get() | (1 << (i % 64)));
                        }),
                    ).ok();
                }
                return papyrus_terrain::path_terrain::sample_height(
                    papyrus_terrain::path_terrain::Grid {
                        size: context.layers.size as f32,
                        resolution: context.layers.height_resolution,
                        origin_y: geometry.origin_y,
                        vertical_span: geometry.vertical_span,
                        height: height_values,
                    },
                    [x, 0., z],
                )
                .ok();
            }
            papyrus_terrain::spawn::sample_height(
                context.layers,
                x / context.layers.size as f32 + 0.5,
                z / context.layers.size as f32 + 0.5,
            )
            .ok()
        };
        entries.push(json!({"road":index,"width":settings.width,"path_length":path.length(),"plan":plan,"noise_samples":distances.len()}));
        for lod in 0..templates.len() {
            for chunk_index in 0..plan.chunks {
                let surface = path_mesh::Surface {
                    settings,
                    snap_to_terrain: policy.snap_to_terrain,
                    radius: &radius,
                };
                let chunk = if lod == 0 && traces.is_some() {
                    let Some(traced) = path_mesh::deform_traced_surface_chunk(
                        &path,
                        &templates,
                        lod,
                        surface,
                        chunk_index,
                        height,
                        &cancelled,
                    )
                    .map_err(|e| format!("Traced road mesh deformation failed: {e:?}"))?
                    else {
                        return Ok(None);
                    };
                    traces.as_deref_mut().unwrap().push(HeightTrace {
                        road: index,
                        chunk_index,
                        heights: traced.heights,
                    });
                    traced.geometry
                } else {
                    let Some(chunk) = path_mesh::deform_surface_chunk(
                        &path,
                        &templates,
                        lod,
                        surface,
                        chunk_index,
                        height,
                        &cancelled,
                    )
                    .map_err(|e| format!("Road mesh deformation failed: {e:?}"))?
                    else {
                        return Ok(None);
                    };
                    chunk
                };
                geometry.push(SurfaceChunk {
                    road: index,
                    lod,
                    chunk_index,
                    geometry: chunk,
                });
            }
        }
    }
    if cancelled() {
        return Ok(None);
    }
    let files=geometry.iter().map(|g|json!({"file":g.filename(),"road":g.road,"lod":g.lod,"chunk_index":g.chunk_index,"vertices":g.geometry.mesh.vertices.len(),"triangles":g.geometry.mesh.triangles.len()})).collect::<Vec<_>>();
    let mut output = SurfaceOutput {
        report: json!({"engine":policy.engine,"source_sha256":source.sha256,"infrastructure_sha256":recipe.source_sha256,"policy":policy,"roads":entries,"files":files,"lods":templates.len(),"deformation_bounds_lod":templates.len()-1,"collision_lod":0,"work":work,"geometry_buffer_bytes":bytes,"noise_samples":coordinates.len(),"physics_cooking_validated":false,"runtime_assembly_complete":false,"materials_rendered":false}),
        chunks: geometry,
    };
    if traces.is_some() {
        output.report["height_trace_vertices"] = json!(trace_vertices);
        output.report["height_trace_bytes"] = json!(trace_bytes);
    }
    if let Some(geometry) = height_geometry {
        output.report["height_sampling"] =
            json!({"engine":"papyrus-owned-height-map-sampler-1","geometry":geometry});
        let mut audits = Vec::new();
        let mut passed = true;
        for chunk in output.chunks.iter().filter(|c| c.lod == 0) {
            if cancelled() {
                return Ok(None);
            }
            let audit = match papyrus_terrain::road_mesh_audit::audit(
                &chunk.geometry,
                &source.meshes[0].buffer,
                papyrus_terrain::road_mesh_audit::Settings {
                    maximum_grade: recipe.maximum_grade,
                    minimum_up_normal: 0.5,
                    maximum_work: MAX_WORK
                        .checked_sub(work)
                        .filter(|v| *v > 0)
                        .ok_or("Road collision audit work budget exceeded")?,
                },
                &cancelled,
            ) {
                Ok(audit) => audit,
                Err(papyrus_terrain::road_mesh_audit::Failure::Cancelled) => return Ok(None),
                Err(e) => return Err(format!("Road collision grade audit failed: {e:?}")),
            };
            work = work
                .checked_add(audit.work)
                .filter(|v| *v <= MAX_WORK)
                .ok_or("Road collision audit work budget exceeded")?;
            passed &= audit.passed;
            audits.push(json!({"road":chunk.road,"chunk_index":chunk.chunk_index,"audit":audit}));
        }
        // Absence of paved geometry is not a successful physical road check.
        output.report["collision_grades_passed"] = if audits.is_empty() {
            serde_json::Value::Null
        } else {
            passed.into()
        };
        output.report["collision_grade_audits"] = json!(audits);
        output.report["work"] = work.into();
    }
    Ok(Some(output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use papyrus_terrain::road_network::{Destination, Family, Road};

    #[test]
    fn retained_mesh_support_matches_only_its_exact_encoded_height_taps() {
        let mut height = vec![123; 129];
        let support = MeshHeightSupport::capture(&[1 << 63, 0, 1], &height).unwrap();
        height[64] += 1;
        assert!(support.matches(&height, || false));
        assert!(!support.matches(&height[..128], || false));
        assert!(!support.matches(&height, || true));
        for index in [63, 128] {
            height[index] += 1;
            assert!(!support.matches(&height, || false));
            height[index] -= 1;
        }
    }

    #[test]
    fn clearance_adapter_shares_budgets_cancels_atomically_and_never_accepts_absent_geometry() {
        let template = path_mesh::Template {
            vertices: vec![[-500., 1., -500.], [500., 1., -500.], [0., 1., 500.]],
            normals: vec![[0., 1., 0.]; 3],
            tangents: vec![[1., 0., 0., 1.]; 3],
            uv: vec![[0.; 2]; 3],
            triangles: vec![[0, 1, 2]],
            center: [0., 1., 0.],
            extents: [500., 0., 500.],
        };
        let make = |index| SurfaceChunk {
            road: 0,
            lod: 0,
            chunk_index: index,
            geometry: path_mesh::Chunk {
                first_repetition: 0,
                repetitions: 1,
                origin: [0.; 3],
                mesh: template.clone(),
                work: 0,
            },
        };
        let mut surfaces = SurfaceOutput {
            report: json!({"unchanged":true}),
            chunks: vec![make(0), make(1)],
        };
        let height = vec![0; 9];
        let grid = papyrus_terrain::path_terrain::Grid {
            size: 1000.,
            resolution: 3,
            origin_y: 0.,
            vertical_span: 1000.,
            height: &height,
        };
        let settings = papyrus_terrain::road_clearance::Settings {
            tolerance: 0.02,
            minimum_up_normal: 0.5,
            maximum_work: 100,
            maximum_probes: 100,
        };
        let audit = audit_clearance(grid, &surfaces, &template, settings, || false)
            .unwrap()
            .unwrap();
        assert_eq!(audit.report["passed"], true);
        assert_eq!(audit.report["driving_triangles"], 2);
        assert!(audit.probes.is_empty());
        assert!(
            audit_clearance(
                grid,
                &surfaces,
                &template,
                papyrus_terrain::road_clearance::Settings {
                    maximum_work: 5,
                    ..settings
                },
                || false
            )
            .is_err()
        );
        let calls = std::cell::Cell::new(0);
        assert!(
            audit_clearance(grid, &surfaces, &template, settings, || {
                let n = calls.get();
                calls.set(n + 1);
                n > 7
            })
            .unwrap()
            .is_none()
        );
        assert_eq!(surfaces.report, json!({"unchanged":true}));
        surfaces.chunks[1].chunk_index = 0;
        assert!(audit_clearance(grid, &surfaces, &template, settings, || false).is_err());
        surfaces.chunks.clear();
        assert!(
            audit_clearance(grid, &surfaces, &template, settings, || false)
                .unwrap()
                .unwrap()
                .report["passed"]
                .is_null()
        );
        assert!(
            audit_clearance(
                grid,
                &surfaces,
                &template,
                papyrus_terrain::road_clearance::Settings {
                    tolerance: f64::NAN,
                    ..settings
                },
                || false
            )
            .is_err()
        );
    }

    #[test]
    fn dirt_paths_skip_meshes_and_invalid_families_fail_before_native_execution() {
        let (parent, doc) = crate::road_surface_source::tests::fixture();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("meshes.json");
        std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
        let source = Source::read(&path, &parent, &|| false).unwrap();
        let project = Project {
            size: 1000,
            roads: Some(crate::infrastructure::Recipe {
                engine: papyrus_terrain::road_network::SHAPED_VERSION.into(),
                surface_sha256: Some(source.sha256.clone()),
                ..crate::infrastructure::Recipe::new(parent.sha256.clone())
            }),
            ..Project::default()
        };
        let (hr, sr) = papyrus_terrain::native_layers::dimensions(project.size).unwrap();
        let layers = Layers {
            size: project.size,
            terrain_seed: project.seeds[0].value,
            biome_seed: project.seeds[1].value,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![0; hr * hr],
            biome: vec![],
            splat: vec![],
            topology: vec![],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        let run = |roads: &Report, cancel| {
            generate(
                Context {
                    project: &project,
                    root: directory.path(),
                    worker: &directory.path().join("worker-must-not-start.exe"),
                    layers: &layers,
                },
                roads,
                &source,
                || cancel,
            )
        };
        let mut roads = Report {
            roads: vec![Road {
                from: Destination::Monument {
                    monument: 1,
                    connector: 2,
                },
                to: Destination::Network { road: 0, point: 0 },
                points: vec![[0., 0., 0.], [0., 0., 30.]],
            }],
            families: vec![Family {
                infrastructure_type: 2,
                hierarchy: 2,
                seed: 1,
                first_road: 0,
                road_count: 1,
                socket_count: 1,
                unresolved: vec![],
                width: 4.,
                splat_channel: 0,
            }],
            ..Report::default()
        };
        let output = run(&roads, false).unwrap().unwrap();
        assert!(output.chunks.is_empty());
        assert_eq!(output.report["noise_samples"], 0);
        assert!(output.report.get("height_sampling").is_none());
        let bound = |geometry| {
            generate_with_height_geometry(
                Context {
                    project: &project,
                    root: directory.path(),
                    worker: &directory.path().join("worker-must-not-start.exe"),
                    layers: &layers,
                },
                &roads,
                &source,
                geometry,
                || false,
            )
        };
        let geometry = HeightGeometry {
            origin_y: -500.,
            vertical_span: 1000.,
        };
        let exact = bound(geometry).unwrap().unwrap();
        assert!(exact.chunks.is_empty());
        assert!(exact.report["collision_grades_passed"].is_null());
        assert_eq!(exact.report["collision_grade_audits"], json!([]));
        assert_eq!(
            exact.report["height_sampling"]["geometry"]["origin_y"],
            -500.
        );
        assert!(
            bound(HeightGeometry {
                vertical_span: f32::NAN,
                ..geometry
            })
            .unwrap_err()
            .contains("Invalid bound")
        );
        assert!(run(&roads, true).unwrap().is_none());
        // Exercise the source-bound refinement bridge, not just mesh skipping.
        // A nonexistent Worker ensures terrain-only trails need no mesh noise.
        let mut dirt = Report {
            roads: roads
                .roads
                .iter()
                .map(|r| Road {
                    from: r.from,
                    to: r.to,
                    points: r.points.clone(),
                })
                .collect(),
            families: roads.families.clone(),
            ..Default::default()
        };
        dirt.roads[0].to = Destination::Monument {
            monument: 3,
            connector: 4,
        };
        for point in dirt.roads.iter_mut().flat_map(|r| &mut r.points) {
            point[1] = -500.;
        }
        let topology = vec![0; sr * sr];
        let refined = crate::road_refinement::refine_with_source(
            Context {
                project: &project,
                root: directory.path(),
                worker: &directory.path().join("worker-must-not-start.exe"),
                layers: &layers,
            },
            &dirt,
            &source,
            crate::road_refinement::Snapshots {
                retained_height: None,
                geometry,
                ground: &layers.height,
                source_painted: &layers.height,
                source_rounding: None,
                topology: &topology,
                surface_resolution: sr,
                protected_topology: 1024,
                allowed_topology: 1024,
            },
            crate::road_refinement::Settings {
                fit: papyrus_terrain::road_bed::Settings {
                    maximum_grade: 0.2,
                    maximum_cut_fill: 8.,
                    minimum_up_normal: 0.5,
                    maximum_iterations: 512,
                    maximum_work: 2_000_000,
                    quantization_reserve: 0.51,
                    reserve_roundoff: true,
                },
                clearance: papyrus_terrain::road_clearance::Settings {
                    tolerance: 0.02,
                    minimum_up_normal: 0.5,
                    maximum_work: 10000,
                    maximum_probes: 1000,
                },
                maximum_rounds: 4,
                maximum_total_work: 10_000_000,
                maximum_reprojection_distance: 0.25,
            },
            |_| {},
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(refined.height, layers.height);
        assert!(refined.geometry.surfaces.chunks.is_empty());
        assert!(refined.clearance.report["passed"].is_null());
        assert!(refined.report.numerical_model_passed);
        assert!(!refined.report.runtime_physics_validated);
        let expected = mesh_roads(&dirt, source.policy().maximum_hierarchy_exclusive).unwrap();
        assert!(!collision_coverage(&expected, &refined.geometry.surfaces).unwrap());
        assert!(clearance_accepted(false, &refined.clearance));
        // Empty geometry never excuses missing paved roads or bad families.
        dirt.families[0].hierarchy = 1;
        let expected = mesh_roads(&dirt, source.policy().maximum_hierarchy_exclusive).unwrap();
        assert!(collision_coverage(&expected, &refined.geometry.surfaces).is_err());
        assert!(!clearance_accepted(true, &refined.clearance));
        let mut malformed = SurfaceOutput {
            report: json!({"passed": true}),
            chunks: vec![SurfaceChunk {
                road: 0,
                lod: 1,
                chunk_index: 0,
                geometry: path_mesh::Chunk {
                    first_repetition: 0,
                    repetitions: 1,
                    origin: [0.; 3],
                    mesh: source.meshes[0].buffer.clone(),
                    work: 0,
                },
            }],
        };
        assert!(
            collision_coverage(&expected, &malformed).is_err(),
            "Render LOD cannot replace collision LOD"
        );
        malformed.chunks[0].lod = 0;
        assert!(collision_coverage(&expected, &malformed).unwrap());
        assert!(
            collision_coverage(&[true, true], &malformed).is_err(),
            "Every paved road needs geometry"
        );
        assert!(
            collision_coverage(&[false], &malformed).is_err(),
            "Dirt cannot claim paved geometry"
        );
        malformed.chunks[0].road = 1;
        assert!(collision_coverage(&expected, &malformed).is_err());
        dirt.families.push(dirt.families[0].clone());
        assert!(mesh_roads(&dirt, 2).is_err());
        dirt.families.clear();
        assert!(mesh_roads(&dirt, 2).is_err());
        roads.families[0].road_count = usize::MAX;
        assert!(run(&roads, false).unwrap_err().contains("Invalid final"));
        roads.families[0].road_count = 0;
        assert!(run(&roads, false).unwrap_err().contains("ambiguous"));
        assert_eq!(layers.height, vec![0; hr * hr]);
    }
}
