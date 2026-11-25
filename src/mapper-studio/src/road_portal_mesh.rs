//! Installation-template entrance-mesh measurement for geometry proposals.
//! Only the first/last collision chunk is measured. This never certifies a
//! complete road, a fitted terrain revision, connectivity or a playable map.
use crate::road_surface_source::Policy;
use papyrus_terrain::{path_geometry::Path, path_mesh, path_terrain, road_clearance, road_mesh_audit};
use serde::Serialize;
use std::collections::HashMap;

pub const VERSION: &str = "papyrus-road-portal-mesh-1";
#[derive(Clone, Copy, Debug, Serialize)]
pub enum Endpoint {
    Start,
    End,
}
pub struct Source<'a> {
    pub policy: &'a Policy,
    /// All LODs, in source order: the LAST LOD supplies deformation bounds.
    pub templates: &'a [&'a path_mesh::Template],
}
#[derive(Clone, Copy)]
pub struct Settings {
    pub endpoint: Endpoint,
    pub width: f32,
    pub maximum_grade: f32,
    pub clearance: road_clearance::Settings,
    pub maximum_work: usize,
}
#[derive(Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub scope: &'static str,
    pub endpoint: Endpoint,
    pub chunk_index: usize,
    pub plan: path_mesh::Plan,
    pub noise_samples: usize,
    pub work: usize,
    pub mesh: road_mesh_audit::Report,
    pub clearance: road_clearance::Report,
    pub complete_road_validated: bool,
    pub full_map_validated: bool,
}
pub struct Output {
    pub geometry: path_mesh::Chunk,
    pub heights: Option<Vec<path_mesh::HeightBinding>>,
    pub probes: Vec<papyrus_terrain::road_bed::ClearanceProbe>,
    pub report: Report,
}
#[derive(Debug, PartialEq)]
pub enum Failure {
    Budget,
    Other(String),
}
impl From<String> for Failure {
    fn from(value: String) -> Self {
        Self::Other(value)
    }
}
impl From<&str> for Failure {
    fn from(value: &str) -> Self {
        Self::Other(value.into())
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Budget => f.write_str("Portal mesh work exhausted"),
            Self::Other(v) => f.write_str(v),
        }
    }
}
impl std::error::Error for Failure {}
fn mesh_failure(stage: &str, error: path_mesh::Failure) -> Failure {
    match error {
        path_mesh::Failure::Budget => Failure::Budget,
        e => Failure::Other(format!("{stage}: {e:?}")),
    }
}
struct Meter {
    work: usize,
    limit: usize,
}
impl Meter {
    fn spend(&mut self, amount: usize) -> Result<(), Failure> {
        self.work = self
            .work
            .checked_add(amount)
            .filter(|v| *v <= self.limit)
            .ok_or(Failure::Budget)?;
        Ok(())
    }
    fn remaining(&self) -> Result<usize, Failure> {
        self.limit
            .checked_sub(self.work)
            .filter(|v| *v > 0)
            .ok_or(Failure::Budget)
    }
}

/// `noise` must supply exact selected-installation snoise1_32 values for the
/// supplied f32 coordinates, in order. The production adapter is responsible
/// for native/profile provenance; a synthetic provider is useful in tests.
/// Input buffers are immutable, and cancellation publishes no partial output.
pub fn measure(
    controls: &[[f32; 3]],
    grid: path_terrain::Grid<'_>,
    source: Source<'_>,
    settings: Settings,
    noise: impl FnMut(&[f32]) -> Result<Option<Vec<f32>>, String>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, Failure> {
    measure_inner(controls, grid, source, settings, false, noise, cancelled)
}
/// Diagnostic companion retaining exact terrain-height dependencies. It uses
/// the same deformation and audits; tracing consumes the same work allowance.
pub fn measure_traced(
    controls: &[[f32; 3]],
    grid: path_terrain::Grid<'_>,
    source: Source<'_>,
    settings: Settings,
    noise: impl FnMut(&[f32]) -> Result<Option<Vec<f32>>, String>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, Failure> {
    measure_inner(controls, grid, source, settings, true, noise, cancelled)
}
fn measure_inner(
    controls: &[[f32; 3]],
    grid: path_terrain::Grid<'_>,
    source: Source<'_>,
    settings: Settings,
    trace: bool,
    mut noise: impl FnMut(&[f32]) -> Result<Option<Vec<f32>>, String>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, Failure> {
    if cancelled() {
        return Ok(None);
    }
    if !(1..=1_000_000).contains(&settings.maximum_work)
        || !(1..=16).contains(&source.templates.len())
        || !(2..=65536).contains(&controls.len())
        || !settings.width.is_finite()
        || !(0.1..=128.).contains(&settings.width)
        || !settings.maximum_grade.is_finite()
        || !(0.001..=1.).contains(&settings.maximum_grade)
    {
        return Err("Invalid portal mesh inputs".into());
    }
    source.policy.validate()?;
    // Validate layout before multiplying dimensions or calling the provider.
    path_terrain::height_support(
        path_terrain::Layout {
            size: grid.size,
            resolution: grid.resolution,
            origin_y: grid.origin_y,
            vertical_span: grid.vertical_span,
        },
        [0.; 3],
    )?;
    if grid.height.len() != grid.resolution * grid.resolution {
        return Err("Portal height buffer differs from its layout".into());
    }
    settings
        .clearance
        .validate()
        .map_err(|e| format!("Portal clearance settings: {e:?}"))?;
    let mut meter = Meter {
        work: 0,
        limit: settings.maximum_work,
    };
    // Charge validation before traversing caller-owned template buffers.
    for template in source.templates {
        let validation = template
            .vertices
            .len()
            .checked_mul(128)
            .and_then(|v| v.checked_add(template.triangles.len().checked_mul(32)?))
            .ok_or("Portal template work overflow")?;
        meter.spend(validation)?;
        template.validate()?;
    }
    meter.spend(controls.len())?;
    let Some(path) =
        Path::from_controls(controls, meter.remaining()?, &cancelled).map_err(|e| match e {
            papyrus_terrain::path_geometry::Failure::Budget => Failure::Budget,
            e => Failure::Other(format!("Portal controls: {e:?}")),
        })?
    else {
        return Ok(None);
    };
    meter.spend(path.work())?;
    if path.is_circular() {
        return Err("A portal requires an open road".into());
    }
    let policy = source.policy;
    let mesh_settings = path_mesh::Settings {
        width: settings.width,
        chunk_repetitions: policy.chunk_repetitions,
        maximum_work: meter.remaining()?,
        snap_start: policy.snap_open_endpoints,
        snap_end: policy.snap_open_endpoints,
        mesh_offset: policy.mesh_offset,
        top_aligned: policy.top_aligned,
    };
    let bounds = source.templates.last().ok_or("Missing portal bounds")?;
    let plan = path_mesh::Plan::new(&path, bounds, mesh_settings)
        .map_err(|e| mesh_failure("Portal schedule", e))?;
    let Some(queries) =
        path_mesh::surface_queries(&path, source.templates, mesh_settings, &cancelled)
            .map_err(|e| mesh_failure("Portal radius queries", e))?
    else {
        return Ok(None);
    };
    meter.spend(queries.work)?;
    meter.spend(
        queries
            .distances
            .len()
            .checked_mul(9)
            .ok_or("Portal noise work overflow")?,
    )?;
    let coordinates: Vec<_> = queries
        .distances
        .iter()
        .map(|d| *d * policy.noise_frequency)
        .collect();
    let Some(values) = noise(&coordinates)? else {
        return Ok(None);
    };
    if cancelled() {
        return Ok(None);
    }
    if values.len() != coordinates.len() || values.iter().any(|v| !v.is_finite()) {
        return Err("Portal noise count/values differ from the request".into());
    }
    let radii = queries
        .distances
        .iter()
        .zip(values)
        .map(|(d, value)| {
            policy
                .radius(settings.width, value)
                .map(|r| (d.to_bits(), r))
                .ok_or("Invalid source-derived portal radius")
        })
        .collect::<Result<HashMap<_, _>, _>>()?;
    let radius = |distance: f32| radii.get(&distance.to_bits()).copied();
    let chunk_index = match settings.endpoint {
        Endpoint::Start => 0,
        Endpoint::End => plan.chunks - 1,
    };
    let repetitions =
        (plan.repetitions - chunk_index * policy.chunk_repetitions).min(policy.chunk_repetitions);
    let vertices = source.templates[0]
        .vertices
        .len()
        .checked_mul(repetitions)
        .ok_or("Portal vertex overflow")?;
    // Reserve three bilinear queries, assembly and repeated generated-buffer
    // validation before deformation, including cancelled/error paths.
    meter.spend(
        vertices
            .checked_mul(80)
            .ok_or("Portal deformation work overflow")?,
    )?;
    meter.spend(
        source.templates[0]
            .triangles
            .len()
            .checked_mul(repetitions)
            .and_then(|v| v.checked_mul(8))
            .ok_or("Portal face validation work overflow")?,
    )?;
    let surface = path_mesh::Surface {
        settings: path_mesh::Settings {
            maximum_work: meter.remaining()?,
            ..mesh_settings
        },
        snap_to_terrain: policy.snap_to_terrain,
        radius: &radius,
    };
    if trace {
        meter.spend(
            vertices
                .checked_mul(8)
                .ok_or("Portal trace work overflow")?,
        )?;
    }
    let (geometry, heights) = if trace {
        let Some(traced) = path_mesh::deform_traced_surface_chunk(
            &path,
            source.templates,
            0,
            path_mesh::Surface {
                settings: path_mesh::Settings {
                    maximum_work: meter.remaining()?,
                    ..surface.settings
                },
                ..surface
            },
            chunk_index,
            |x, z| path_terrain::sample_height(grid, [x, 0., z]).ok(),
            &cancelled,
        )
        .map_err(|e| mesh_failure("Portal traced deformation", e))?
        else {
            return Ok(None);
        };
        (traced.geometry, Some(traced.heights))
    } else {
        let Some(geometry) = path_mesh::deform_surface_chunk(
            &path,
            source.templates,
            0,
            path_mesh::Surface {
                settings: path_mesh::Settings {
                    maximum_work: meter.remaining()?,
                    ..mesh_settings
                },
                snap_to_terrain: policy.snap_to_terrain,
                radius: &radius,
            },
            chunk_index,
            |x, z| path_terrain::sample_height(grid, [x, 0., z]).ok(),
            &cancelled,
        )
        .map_err(|e| mesh_failure("Portal deformation", e))?
        else {
            return Ok(None);
        };
        (geometry, None)
    };
    meter.spend(geometry.work)?;
    let mesh = road_mesh_audit::audit(
        &geometry,
        source.templates[0],
        road_mesh_audit::Settings {
            maximum_grade: settings.maximum_grade,
            minimum_up_normal: settings.clearance.minimum_up_normal,
            maximum_work: meter.remaining()?,
        },
        &cancelled,
    );
    let mesh = match mesh {
        Ok(v) => v,
        Err(road_mesh_audit::Failure::Cancelled) => return Ok(None),
        Err(road_mesh_audit::Failure::Budget) => return Err(Failure::Budget),
        Err(e) => return Err(format!("Portal mesh audit: {e:?}").into()),
    };
    meter.spend(mesh.work)?;
    let clearance = road_clearance::audit(
        road_clearance::Input {
            grid,
            chunk: &geometry,
            template: source.templates[0],
            road: 0,
            chunk_index,
        },
        road_clearance::Settings {
            maximum_work: settings.clearance.maximum_work.min(meter.remaining()?),
            ..settings.clearance
        },
        &cancelled,
    );
    let clearance = match clearance {
        Ok(v) => v,
        Err(road_clearance::Failure::Cancelled) => return Ok(None),
        Err(road_clearance::Failure::Budget) => return Err(Failure::Budget),
        Err(e) => return Err(format!("Portal clearance audit: {e:?}").into()),
    };
    meter.spend(clearance.report.work)?;
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(Output {
        geometry,
        heights,
        probes: clearance.probes,
        report: Report {
            engine: VERSION,
            scope: "one_native_collision_chunk_only",
            endpoint: settings.endpoint,
            chunk_index,
            plan,
            noise_samples: coordinates.len(),
            work: meter.work,
            mesh,
            clearance: clearance.report,
            complete_road_validated: false,
            full_map_validated: false,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn template() -> path_mesh::Template {
        path_mesh::Template {
            vertices: vec![[-1., 0., 0.], [1., 0., 0.], [-1., 0., 4.], [1., 0., 4.]],
            normals: vec![[0., 1., 0.]; 4],
            tangents: vec![[1., 0., 0., 1.]; 4],
            uv: vec![[0.; 2]; 4],
            triangles: vec![[0, 2, 1], [1, 2, 3]],
            center: [0., 0., 2.],
            extents: [1., 0., 2.],
        }
    }
    fn policy() -> Policy {
        Policy {
            engine: crate::road_surface_source::ENGINE.into(),
            chunk_repetitions: 2,
            maximum_hierarchy_exclusive: 2,
            spline: true,
            snap_to_terrain: true,
            snap_open_endpoints: true,
            normal_smoothing: 0.,
            random_scale: 0.75,
            mesh_offset: 0.,
            noise_export: "snoise1_32".into(),
            noise_frequency: 0.01,
            noise_unsigned_scale: 0.5,
            noise_unsigned_bias: 0.5,
            scale_width_with_length: false,
            top_aligned: false,
            round_vertices: 0,
        }
    }
    fn settings(endpoint: Endpoint) -> Settings {
        Settings {
            endpoint,
            width: 2.,
            maximum_grade: 0.2,
            maximum_work: 1_000_000,
            clearance: road_clearance::Settings {
                tolerance: 0.02,
                minimum_up_normal: 0.5,
                maximum_work: 1_000_000,
                maximum_probes: 131072,
            },
        }
    }
    fn grid(height: &[i16]) -> path_terrain::Grid<'_> {
        path_terrain::Grid {
            size: 100.,
            resolution: 101,
            origin_y: -500.,
            vertical_span: 1000.,
            height,
        }
    }
    #[test]
    fn first_and_last_chunks_keep_source_schedule_and_reject_partial_map_claims() {
        let height = vec![16383; 101 * 101];
        let t = template();
        let p = policy();
        let ts = [&t];
        let controls = [[0., 0., -20.], [0., 0., 20.]];
        for (endpoint, index) in [(Endpoint::Start, 0), (Endpoint::End, 4)] {
            let out = measure(
                &controls,
                grid(&height),
                Source {
                    policy: &p,
                    templates: &ts,
                },
                settings(endpoint),
                |coords| Ok(Some(vec![0.; coords.len()])),
                || false,
            )
            .unwrap()
            .unwrap();
            assert_eq!(out.report.chunk_index, index);
            assert_eq!(out.report.plan.repetitions, 10);
            assert_eq!(out.geometry.first_repetition, index * 2);
            assert!(out.report.mesh.passed && out.report.clearance.passed);
            assert!(!out.report.complete_road_validated && !out.report.full_map_validated);
            assert!(out.heights.is_none());
            let traced = measure_traced(
                &controls,
                grid(&height),
                Source {
                    policy: &p,
                    templates: &ts,
                },
                settings(endpoint),
                |coords| Ok(Some(vec![0.; coords.len()])),
                || false,
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                serde_json::to_value(&traced.geometry).unwrap(),
                serde_json::to_value(&out.geometry).unwrap()
            );
            assert_eq!(
                serde_json::to_value(&traced.probes).unwrap(),
                serde_json::to_value(&out.probes).unwrap()
            );
            let bindings = traced.heights.as_ref().unwrap();
            assert_eq!(bindings.len(), traced.geometry.mesh.vertices.len());
            for (binding, vertex) in bindings.iter().zip(&traced.geometry.mesh.vertices) {
                let y = binding
                    .resolve(|x, z| path_terrain::sample_height(grid(&height), [x, 0., z]).ok())
                    .unwrap();
                assert_eq!(
                    (y - traced.geometry.origin[1]).to_bits(),
                    vertex[1].to_bits()
                );
            }
            assert_eq!(traced.report.work, out.report.work + bindings.len() * 8);
            assert!(matches!(
                measure_traced(
                    &controls,
                    grid(&height),
                    Source {
                        policy: &p,
                        templates: &ts
                    },
                    Settings {
                        maximum_work: traced.report.work - 1,
                        ..settings(endpoint)
                    },
                    |coords| Ok(Some(vec![0.; coords.len()])),
                    || false,
                ),
                Err(Failure::Budget)
            ));
            let exact = out.report.work;
            let cfg = Settings {
                maximum_work: exact,
                ..settings(endpoint)
            };
            assert!(
                measure(
                    &controls,
                    grid(&height),
                    Source {
                        policy: &p,
                        templates: &ts
                    },
                    cfg,
                    |coords| Ok(Some(vec![0.; coords.len()])),
                    || false
                )
                .unwrap()
                .is_some()
            );
            assert!(
                measure(
                    &controls,
                    grid(&height),
                    Source {
                        policy: &p,
                        templates: &ts
                    },
                    Settings {
                        maximum_work: exact - 1,
                        ..cfg
                    },
                    |coords| Ok(Some(vec![0.; coords.len()])),
                    || false
                )
                .is_err()
            );
        }
    }
    #[test]
    fn conforming_vertices_do_not_hide_a_continuous_terrain_collision() {
        let height = (0usize..101 * 101)
            .map(|i| {
                let x = (i % 101) as i32 - 50;
                (16383 - (x * x)) as i16
            })
            .collect::<Vec<_>>();
        let original = height.clone();
        let t = template();
        let p = policy();
        let ts = [&t];
        let out = measure(
            &[[0., 0., -20.], [0., 0., 20.]],
            grid(&height),
            Source {
                policy: &p,
                templates: &ts,
            },
            settings(Endpoint::End),
            |coords| Ok(Some(vec![0.; coords.len()])),
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(out.report.mesh.passed);
        assert!(!out.report.clearance.passed);
        assert!(out.report.clearance.maximum_continuous_terrain_excess > 0.02);
        for v in &out.geometry.mesh.vertices {
            let world = std::array::from_fn(|k| v[k] + out.geometry.origin[k]);
            assert!(
                (world[1] - path_terrain::sample_height(grid(&height), world).unwrap()).abs()
                    < 0.0001
            );
        }
        assert_eq!(height, original);
    }
    #[test]
    fn bad_noise_and_midflight_cancellation_never_publish_geometry() {
        let height = vec![16383; 101 * 101];
        let t = template();
        let p = policy();
        let ts = [&t];
        let controls = [[0., 0., -20.], [0., 0., 20.]];
        for invalid in [
            path_terrain::Grid {
                resolution: usize::MAX,
                ..grid(&height)
            },
            path_terrain::Grid {
                height: &height[..10],
                ..grid(&height)
            },
        ] {
            assert!(
                measure(
                    &controls,
                    invalid,
                    Source {
                        policy: &p,
                        templates: &ts
                    },
                    settings(Endpoint::End),
                    |_| panic!("Invalid layout reached the native provider"),
                    || false
                )
                .is_err()
            );
        }
        assert!(
            measure(
                &controls,
                grid(&height),
                Source {
                    policy: &p,
                    templates: &ts
                },
                settings(Endpoint::End),
                |_| Ok(Some(vec![])),
                || false
            )
            .is_err()
        );
        assert!(
            measure(
                &controls,
                grid(&height),
                Source {
                    policy: &p,
                    templates: &ts
                },
                settings(Endpoint::End),
                |coords| Ok(Some(vec![f32::NAN; coords.len()])),
                || false
            )
            .is_err()
        );
        let stop = std::cell::Cell::new(false);
        assert!(
            measure(
                &controls,
                grid(&height),
                Source {
                    policy: &p,
                    templates: &ts
                },
                settings(Endpoint::End),
                |coords| {
                    stop.set(true);
                    Ok(Some(vec![0.; coords.len()]))
                },
                || stop.get()
            )
            .unwrap()
            .is_none()
        );
    }
}
