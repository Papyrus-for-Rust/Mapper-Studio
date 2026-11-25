//! Source road construction is distinct from Mapper's optional strict bed fit.
use crate::{road_refinement, road_surface_source, road_surface_stage};
use papyrus_terrain::{native_layers::Layers, road_network};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub(crate) const VERSION: &str = "papyrus-source-road-materialization-1";
pub(crate) const MAX_WORK: u64 = 64_000_000;
#[derive(Debug)]
pub(crate) enum Failure {
    Invalid(String),
    Geometry {
        road: usize,
        chunk: usize,
        inverted: u64,
        degenerate: u64,
        position: Option<[f64; 3]>,
    },
}
impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self::Invalid(message)
    }
}
impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        Self::Invalid(message.into())
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => formatter.write_str(message),
            Self::Geometry {
                road,
                chunk,
                inverted,
                degenerate,
                ..
            } => write!(
                formatter,
                "Invalid source road collision geometry at road {road}, chunk {chunk} ({inverted} inverted, {degenerate} degenerate triangles)"
            ),
        }
    }
}
pub(crate) struct Output {
    pub height: Vec<i16>,
    pub controls: Vec<Vec<[f32; 3]>>,
    pub surfaces: road_network::SurfaceOutput,
    pub report: Value,
    pub work: u64,
}
impl Output {
    pub fn fitted(output: road_refinement::Output) -> Result<Self, String> {
        Ok(Self {
            height: output.height,
            controls: output.controls,
            surfaces: output.geometry.surfaces,
            report: serde_json::to_value(&output.report).map_err(|e| e.to_string())?,
            work: output.report.work,
        })
    }
}

/// A complete source build still requires every expected collision chunk's
/// structural audit. Grade comparisons remain measurements, not source limits.
pub(crate) fn validate_geometry(
    expected: &[bool],
    output: &road_network::SurfaceOutput,
) -> Result<bool, Failure> {
    let applicable = road_surface_stage::collision_coverage(expected, output)?;
    let mut keys = BTreeSet::new();
    for chunk in output.chunks.iter().filter(|c| c.lod == 0) {
        if !keys.insert((chunk.road, chunk.chunk_index)) {
            return Err("Duplicate source road collision chunk".into());
        }
    }
    let rows = output.report["collision_grade_audits"]
        .as_array()
        .ok_or("Missing source road structural audits")?;
    if rows.len() != keys.len()
        || (applicable && !output.report["collision_grades_passed"].is_boolean())
        || (!applicable && !output.report["collision_grades_passed"].is_null())
    {
        return Err("Incomplete source road structural audits".into());
    }
    for row in rows {
        let road = row["road"]
            .as_u64()
            .and_then(|v| usize::try_from(v).ok())
            .ok_or("Missing source audit road")?;
        let chunk = row["chunk_index"]
            .as_u64()
            .and_then(|v| usize::try_from(v).ok())
            .ok_or("Missing source audit chunk")?;
        let audit = &row["audit"];
        let degenerate = audit["degenerate_world_triangles"]
            .as_u64()
            .ok_or("Missing source audit degenerate triangle count")?;
        let inverted = audit["inverted_world_triangles"]
            .as_u64()
            .ok_or("Missing source audit inverted triangle count")?;
        if !keys.remove(&(road, chunk))
            || audit["driving_triangles"].as_u64().is_none_or(|n| n == 0)
        {
            return Err("Invalid source road collision geometry".into());
        }
        if degenerate > 0 || inverted > 0 {
            let position = if let Some(index) = audit.get("first_invalid_triangle") {
                let index = index
                    .as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                    .ok_or("Invalid source collision triangle witness")?;
                let geometry = &output
                    .chunks
                    .iter()
                    .find(|c| c.lod == 0 && c.road == road && c.chunk_index == chunk)
                    .ok_or("Missing source collision witness chunk")?
                    .geometry;
                let face = geometry
                    .mesh
                    .triangles
                    .get(index)
                    .ok_or("Source collision witness triangle is outside its chunk")?;
                let [Some(a), Some(b), Some(c)] =
                    face.map(|i| geometry.mesh.vertices.get(i as usize))
                else {
                    return Err("Source collision witness vertex is outside its chunk".into());
                };
                let position = std::array::from_fn(|axis| {
                    (f64::from(a[axis]) + f64::from(b[axis]) + f64::from(c[axis])) / 3.
                        + f64::from(geometry.origin[axis])
                });
                if position
                    .iter()
                    .any(|v: &f64| !v.is_finite() || v.abs() > 10000.)
                {
                    return Err("Invalid source collision witness position".into());
                }
                Some(position)
            } else {
                None
            };
            return Err(Failure::Geometry {
                road,
                chunk,
                inverted,
                degenerate,
                position,
            });
        }
    }
    Ok(applicable)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn materialize(
    context: road_surface_stage::Context<'_>,
    roads: &road_network::Report,
    source: &road_surface_source::Source,
    geometry: road_surface_stage::HeightGeometry,
    height: Vec<i16>,
    audited_height_sha256: Option<&str>,
    controls: Vec<Vec<[f32; 3]>>,
    maximum_work: u64,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, Failure> {
    if cancelled() {
        return Ok(None);
    }
    let control_count = controls.iter().map(Vec::len).sum::<usize>();
    let reservation = height.len() as u64 + 32_000_000 + control_count as u64 * 4 + 262_144;
    if controls.len() != roads.roads.len()
        || control_count > 131072
        || reservation > maximum_work.min(MAX_WORK)
    {
        return Err("Source road materialization coverage or work allowance differs".into());
    }
    let report = road_network::Report {
        roads: roads
            .roads
            .iter()
            .zip(controls)
            .map(|(r, points)| road_network::Road {
                from: r.from,
                to: r.to,
                points,
            })
            .collect(),
        families: roads.families.clone(),
        ..Default::default()
    };
    let source_height_sha256 = if let Some(digest) = audited_height_sha256 {
        digest.to_owned()
    } else {
        let mut digest = Sha256::new();
        if crate::road_planning::hash_raster_words(
            &mut digest,
            &height,
            |value| value.to_le_bytes(),
            &cancelled,
        )
        .is_none()
        {
            return Ok(None);
        }
        format!("{:x}", digest.finalize())
    };
    // Surface construction only borrows this source-painted raster. It cannot
    // turn the query-height bitmap or a fitted bed into replacement terrain.
    let layers = Layers {
        height,
        size: context.layers.size,
        terrain_seed: context.layers.terrain_seed,
        biome_seed: context.layers.biome_seed,
        height_resolution: context.layers.height_resolution,
        surface_resolution: context.layers.surface_resolution,
        topology: Vec::new(),
        splat: Vec::new(),
        biome: Vec::new(),
        normalized_biome_cells: 0,
        normalized_splat_cells: 0,
    };
    let Some(surfaces) = road_surface_stage::generate_with_height_geometry(
        road_surface_stage::Context {
            layers: &layers,
            ..context
        },
        &report,
        source,
        geometry,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let expected =
        road_surface_stage::mesh_roads(&report, source.policy().maximum_hierarchy_exclusive)?;
    let applicable = validate_geometry(&expected, &surfaces)?;
    let body = json!({"engine":VERSION,"construction_complete":true,"source_terrain_preserved":true,
        "source_height_sha256":source_height_sha256,"strict_fit_applied":false,
        "numerical_model_passed":null,"paved_geometry_applicable":applicable,
        "collision_grade_comparison_passed":surfaces.report["collision_grades_passed"],
        "strict_clearance_audit":{"performed":false,"scope":"not a source completion rule"},
        "work":reservation,"work_accounting":"conservative construction reservation",
        "runtime_physics_validated":false,"water_clearance_validated":false,"full_world_validated":false});
    Ok((!cancelled()).then_some(Output {
        height: layers.height,
        controls: report.roads.into_iter().map(|r| r.points).collect(),
        surfaces,
        report: body,
        work: reservation,
    }))
}

#[cfg(test)]
pub(crate) fn steep_test_surface() -> road_network::SurfaceOutput {
    use papyrus_terrain::{path_mesh, road_mesh_audit};
    let template = path_mesh::Template {
        vertices: vec![[-1., 0., -1.], [1., 0., -1.], [-1., 0., 1.]],
        normals: vec![[0., 1., 0.]; 3],
        tangents: vec![[1., 0., 0., 1.]; 3],
        uv: vec![[0.; 2]; 3],
        triangles: vec![[0, 1, 2]],
        center: [0.; 3],
        extents: [1., 0., 1.],
    };
    let mut mesh = template.clone();
    mesh.vertices[2][1] = 2.;
    mesh.center[1] = 1.;
    mesh.extents[1] = 1.;
    let chunk = path_mesh::Chunk {
        first_repetition: 0,
        repetitions: 1,
        origin: [0.; 3],
        mesh,
        work: 0,
    };
    let audit = road_mesh_audit::audit(
        &chunk,
        &template,
        road_mesh_audit::Settings {
            maximum_grade: 0.2,
            minimum_up_normal: 0.5,
            maximum_work: 1000,
        },
        || false,
    )
    .unwrap();
    assert!(!audit.passed && audit.longitudinal_violations > 0);
    road_network::SurfaceOutput {
        chunks: vec![road_network::SurfaceChunk {
            road: 0,
            lod: 0,
            chunk_index: 0,
            geometry: chunk,
        }],
        report: json!({
        "collision_grades_passed":audit.passed,
        "collision_grade_audits":[{"road":0,"chunk_index":0,"audit":audit}] }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejected_collision_geometry_keeps_its_actual_triangle_position() {
        let mut surface = steep_test_surface();
        surface.chunks[0].geometry.origin = [100., 4., -200.];
        surface.chunks[0].geometry.mesh.vertices.swap(0, 1);
        let audit = &mut surface.report["collision_grade_audits"][0]["audit"];
        audit["inverted_world_triangles"] = 1.into();
        audit["first_invalid_triangle"] = 0.into();
        let Err(Failure::Geometry {
            road: 0,
            chunk: 0,
            inverted: 1,
            position: Some(p),
            ..
        }) = validate_geometry(&[true], &surface)
        else {
            panic!("Expected a positioned collision rejection");
        };
        assert_eq!(p, [100. - 1. / 3., 4. + 2. / 3., -200. - 1. / 3.]);
        surface.report["collision_grade_audits"][0]["audit"]["first_invalid_triangle"] = 99.into();
        assert!(matches!(
            validate_geometry(&[true], &surface),
            Err(Failure::Invalid(_))
        ));
        surface.report["collision_grade_audits"][0]["audit"]["first_invalid_triangle"] = 0.into();
        surface.chunks[0].geometry.mesh.triangles[0][2] = u32::MAX;
        assert!(matches!(
            validate_geometry(&[true], &surface),
            Err(Failure::Invalid(_))
        ));
    }
    #[test]
    fn source_completion_keeps_real_grade_warnings_but_rejects_missing_or_invalid_geometry() {
        let source = steep_test_surface();
        assert!(validate_geometry(&[true], &source).unwrap());
        assert_eq!(source.report["collision_grades_passed"], false);
        assert!(validate_geometry(&[true, true], &source).is_err());
        assert!(validate_geometry(&[false], &source).is_err());
        for key in ["inverted_world_triangles", "degenerate_world_triangles"] {
            let mut bad = steep_test_surface();
            bad.report["collision_grade_audits"][0]["audit"][key] = 1.into();
            assert!(matches!(
                validate_geometry(&[true], &bad),
                Err(Failure::Geometry { .. })
            ));
            bad.report["collision_grade_audits"][0]["audit"]
                .as_object_mut()
                .unwrap()
                .remove(key);
            assert!(matches!(
                validate_geometry(&[true], &bad),
                Err(Failure::Invalid(_))
            ));
        }
        let mut missing = steep_test_surface();
        missing.report["collision_grade_audits"] = json!([]);
        assert!(validate_geometry(&[true], &missing).is_err());
        let mut foreign = steep_test_surface();
        foreign.report["collision_grade_audits"][0]["chunk_index"] = 4.into();
        assert!(validate_geometry(&[true], &foreign).is_err());
        let dirt = road_network::SurfaceOutput {
            chunks: vec![],
            report: json!({"collision_grade_audits":[],"collision_grades_passed":null}),
        };
        assert!(!validate_geometry(&[false], &dirt).unwrap());
        assert!(validate_geometry(&[true], &dirt).is_err());
    }
}
