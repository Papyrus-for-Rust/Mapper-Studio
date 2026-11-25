//! Developer-only source sequence capture. Never returns an accepted world.
use crate::{
    road_generation::Context, road_paint_source, road_paint_stage, road_surface_source,
    road_surface_stage,
};
use papyrus_terrain::{monument_composition::Composed, path_terrain, road_network};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path};

pub(crate) struct Input<'a> {
    pub context: Context<'a>,
    pub base: &'a Composed,
    pub roads: &'a road_network::Report,
    pub before_height: &'a [papyrus_terrain::road_paint::Road<'a>],
    pub painted: &'a road_paint_stage::Preliminary,
    pub mesh: &'a road_surface_source::Source,
    pub paint: &'a road_paint_source::Profile,
    pub endpoint_replay: Option<Value>,
}

pub(crate) struct EndpointReplay {
    pub paths: Vec<papyrus_terrain::path_geometry::Path>,
    pub receipt: Value,
}

/// A local counterfactual on the same roadside-stamped baseline. Only undo XZ
/// snaps whose recorded post-snap endpoint still matches. Roadside selection,
/// straightening and stamps are NOT replayed, so this is not a vanilla oracle.
pub(crate) fn restore_endpoints(
    report: &road_network::Report,
    roads: &[papyrus_terrain::road_paint::Road<'_>],
    cancelled: impl Fn() -> bool,
) -> Result<Option<EndpointReplay>, String> {
    use papyrus_terrain::path_geometry::Path as Curve;
    if cancelled() {
        return Ok(None);
    }
    if roads.len() != report.roads.len() || roads.len() > 256 {
        return Err("Endpoint replay road count differs".into());
    }
    let attachments = report
        .source_layout
        .as_ref()
        .and_then(|v| v["attachments"].as_array())
        .filter(|a| a.len() <= 512)
        .ok_or("Missing bounded endpoint replay receipt")?;
    let mut endpoints = vec![[None; 2]; roads.len()];
    let mut seen = std::collections::BTreeSet::new();
    let mut rows = Vec::new();
    for a in attachments {
        if cancelled() {
            return Ok(None);
        }
        let index = |key: &str| {
            a[key]
                .as_u64()
                .and_then(|v| usize::try_from(v).ok())
                .ok_or("Invalid endpoint replay index")
        };
        let road = index("road")?;
        let side = index("endpoint")?;
        if road >= roads.len() || side > 1 || !seen.insert((road, side)) {
            return Err("Duplicate or invalid endpoint replay attachment".into());
        }
        let expected = road_network::Destination::Network {
            road: index("parent_road")?,
            point: index("parent_point")?,
        };
        let original = &report.roads[road];
        if [original.from, original.to][side] != expected {
            return Err("Endpoint replay destination differs from receipt".into());
        }
        let point = |key: &str| -> Result<[f32; 3], String> {
            let p: [f32; 3] = serde_json::from_value(a[key].clone())
                .map_err(|e| format!("Invalid endpoint replay point: {e}"))?;
            if p.iter().any(|v| !v.is_finite() || v.abs() > 10000.) {
                return Err("Endpoint replay coordinate out of bounds".into());
            }
            Ok(p)
        };
        let before = point("before")?;
        let after = point("after")?;
        let current = if side == 0 {
            roads[road].path.points().first()
        } else {
            roads[road].path.points().last()
        }
        .copied()
        .ok_or("Empty endpoint replay path")?;
        let matches = [0, 2]
            .iter()
            .all(|&k| current[k].to_bits() == after[k].to_bits());
        if matches {
            endpoints[road][side] = Some([before[0], current[1], before[2]]);
        }
        rows.push(json!({"road":road,"endpoint":side,"restored":matches,
            "before":current,"after":endpoints[road][side],
            "skip_reason":(!matches).then_some("Endpoint changed after handoff; preserved") }));
    }
    let mut paths = Vec::<Curve>::with_capacity(roads.len());
    let mut remaining = 32_000_000usize;
    for (r, ends) in roads.iter().zip(endpoints) {
        if cancelled() {
            return Ok(None);
        }
        let path = if ends.iter().any(Option::is_some) {
            match r.path.reattach_endpoints(ends, remaining, &cancelled) {
                Ok(Some(path)) => {
                    remaining = remaining
                        .checked_sub(path.work())
                        .ok_or("Endpoint replay work exhausted")?;
                    path
                }
                Ok(None) => return Ok(None),
                Err(error) => return Err(format!("Endpoint replay geometry: {error:?}")),
            }
        } else {
            remaining = remaining
                .checked_sub(r.path.points().len().saturating_mul(2))
                .ok_or("Endpoint replay work exhausted")?;
            r.path.clone()
        };
        paths.push(path);
    }
    Ok(Some(EndpointReplay {
        paths,
        receipt: json!({
            "mode":"restore-matching-handoff-endpoint-xz",
            "scope":"Same roadside-stamped baseline; no roadside placement/stamp replay. Recorded endpoints changed since handoff are preserved. Network destinations are provenance only; exact joins are not asserted.",
            "accepted":false,"rows":rows,"work":32000000-remaining
        }),
    }))
}

fn height_hash(values: &[i16]) -> String {
    let mut digest = Sha256::new();
    for value in values {
        digest.update(value.to_le_bytes());
    }
    format!("{:x}", digest.finalize())
}

fn source_controls(
    original: &road_network::Report,
    before_height: &[papyrus_terrain::road_paint::Road<'_>],
    after_height: &[papyrus_terrain::path_geometry::Path],
) -> Result<road_network::Report, String> {
    if original.roads.len() != after_height.len()
        || original.roads.len() != before_height.len()
        || original.roads.len() > 256
    {
        return Err("Source capture road count differs".into());
    }
    // The real smoothing pass can close a circular sub-0.1m seam by copying
    // point zero to the final point. Share the production control-change gate
    // instead of rejecting that source operation before diagnostics can run.
    crate::road_paint_stage::painted_control_changes(before_height, after_height, true)?;
    let mut roads = Vec::new();
    for (road, path) in original.roads.iter().zip(after_height) {
        roads.push(road_network::Road {
            from: road.from,
            to: road.to,
            points: path.points().to_vec(),
        });
    }
    Ok(road_network::Report {
        roads,
        families: original.families.clone(),
        ..Default::default()
    })
}

/// Each chunk keeps the existing primitive limits; this separate diagnostic has
/// an aggregate 512M work ceiling. Probe buffers are released between chunks.
fn audit_network(
    input: &Input<'_>,
    surfaces: &road_network::SurfaceOutput,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Value>, String> {
    let mut work = 0usize;
    let mut chunks = Vec::new();
    let mut maximum = f64::NEG_INFINITY;
    let mut continuous = 0usize;
    let mut confirmed = 0usize;
    let mut probes = 0usize;
    for chunk in surfaces.chunks.iter().filter(|c| c.lod == 0) {
        if cancelled() {
            return Ok(None);
        }
        let allowance = (512_000_000usize.saturating_sub(work)).min(10_000_000);
        if allowance == 0 {
            return Err("Source diagnostic clearance work exhausted".into());
        }
        let result = match papyrus_terrain::road_clearance::audit(
            papyrus_terrain::road_clearance::Input {
                grid: path_terrain::Grid {
                    size: input.context.project.size as f32,
                    resolution: input.base.layers.height_resolution,
                    origin_y: input.paint.height_origin,
                    vertical_span: input.paint.vertical_span,
                    height: &input.painted.height,
                },
                chunk: &chunk.geometry,
                template: &input.mesh.meshes[0].buffer,
                road: chunk.road,
                chunk_index: chunk.chunk_index,
            },
            papyrus_terrain::road_clearance::Settings {
                tolerance: 0.02,
                minimum_up_normal: 0.5,
                maximum_work: allowance,
                maximum_probes: 131072,
            },
            cancelled,
        ) {
            Ok(result) => result,
            Err(papyrus_terrain::road_clearance::Failure::Cancelled) => return Ok(None),
            Err(error) => return Err(format!("Source diagnostic clearance: {error:?}")),
        };
        work += result.report.work;
        continuous += result.report.continuous_intersections;
        confirmed += result.report.source_sampler_confirmed_intersections;
        probes += result.probes.len();
        maximum = maximum.max(result.report.maximum_continuous_terrain_excess);
        // Retain each violating triangle's maximum, not the much larger set of
        // boundary guard probes. These are samples for investigation only;
        // coverage of them cannot certify the continuous junction surface.
        let witnesses = result
            .probes
            .iter()
            .filter(|p| p.maximum_terrain_excess < 0.02)
            .collect::<Vec<_>>();
        chunks.push(json!({"road":chunk.road,"chunk":chunk.chunk_index,"audit":result.report,"maximum_witnesses":witnesses}));
    }
    let applicable = !chunks.is_empty();
    Ok(Some(
        json!({"completed":true,"applicable":applicable,"chunks":chunks,"work":work,"maximum_work":512000000,"continuous_intersections":continuous,"source_sampler_confirmed_intersections":confirmed,"probe_count":probes,"maximum_terrain_excess":maximum.is_finite().then_some(maximum),"strict_clearance_passed":applicable.then_some(continuous==0&&confirmed==0),"tolerance":0.02,"runtime_physics_validated":false}),
    ))
}

pub(crate) fn capture(
    input: Input<'_>,
    destination: &Path,
    cancelled: impl Fn() -> bool,
) -> Result<Option<()>, String> {
    if destination.exists() {
        return Err("Source diagnostic destination already exists".into());
    }
    if input.context.project.rails.is_some() || input.base.rails.is_some() {
        return Err("Source diagnostic requires a road-only source revision".into());
    }
    if cancelled() {
        return Ok(None);
    }
    let roads = source_controls(input.roads, input.before_height, &input.painted.roads)?;
    let context = road_surface_stage::Context {
        project: input.context.project,
        root: input.context.bindings.installation_root(),
        worker: input.context.worker,
        layers: &input.base.layers,
    };
    let Some(generated) = road_surface_stage::generate_traced_on_height(
        context,
        &roads,
        input.mesh,
        road_surface_stage::HeightGeometry {
            origin_y: input.paint.height_origin,
            vertical_span: input.paint.vertical_span,
        },
        &input.painted.height,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let audit = match audit_network(&input, &generated.surfaces, &cancelled) {
        Ok(Some(audit)) => audit,
        Ok(None) => return Ok(None),
        Err(error) => json!({"completed":false,"error":error,"strict_clearance_passed":null}),
    };
    let source_roads = input
        .painted
        .roads
        .iter()
        .zip(&input.roads.roads)
        .enumerate()
        .map(|(i, (path, _))| {
            let family = input
                .roads
                .families
                .iter()
                .find(|f| i >= f.first_road && i < f.first_road + f.road_count)
                .ok_or("Missing diagnostic road family")?;
            Ok(papyrus_terrain::road_paint::Road {
                path,
                hierarchy: u8::try_from(family.hierarchy).map_err(|e| e.to_string())?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let native = crate::native_profile::resolve(&input.context.project.terrain_key())?;
    let installation = input
        .context
        .project
        .installation
        .as_ref()
        .ok_or("Missing source diagnostic installation")?;
    let Some(final_surfaces) = road_paint_stage::finish_surfaces(
        input.base,
        &input.painted.height,
        &source_roads,
        input.paint,
        256_000_000,
        |coordinates| {
            crate::native_noise::sample(
                input.context.bindings.installation_root(),
                input.context.worker,
                installation,
                &native,
                coordinates,
                &cancelled,
            )
        },
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let layers = papyrus_terrain::native_layers::Layers {
        size: input.base.layers.size,
        terrain_seed: input.base.layers.terrain_seed,
        biome_seed: input.base.layers.biome_seed,
        height_resolution: input.base.layers.height_resolution,
        surface_resolution: input.base.layers.surface_resolution,
        height: input.painted.height.clone(),
        biome: input.base.layers.biome.clone(),
        splat: final_surfaces.splat,
        topology: final_surfaces.topology,
        normalized_splat_cells: 0,
        normalized_biome_cells: 0,
    };
    let palette = crate::terrain_palette::Palette::discover(
        input.context.bindings.installation_root(),
        installation,
        &cancelled,
    )?;
    let Some(image) = crate::map_image::render_with_palette(
        &layers,
        Some(&input.base.alpha),
        final_surfaces.ocean.as_ref(),
        crate::map_image::DISPLAY_RESOLUTION,
        palette.as_ref(),
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let mut families = BTreeMap::new();
    for family in &roads.families {
        *families.entry(family.hierarchy).or_insert(0usize) += family.road_count;
    }
    let layout = crate::composition::layout_fingerprint(input.context.solution)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let report = json!({
        "format":"mapper-private-source-road-stages-1", "accepted":false,
        "full_map_validated":false, "playable":false, "strict_fitting_run":false,
        "scope":"Source roadside stamps, terrain shaping, source meshes and final road paint; intentionally stopped before Mapper fitting. Not an accepted world.",
        "recipe_sha256":format!("{:x}",Sha256::digest(serde_json::to_vec(input.context.project).map_err(|e|e.to_string())?)),
        "layout_sha256":layout, "catalog_sha256":input.context.catalog.sha256,
        "bindings_sha256":input.context.bindings.sha256, "surface_sha256":input.mesh.sha256,
        "ground_height_sha256":height_hash(&input.base.layers.height),
        "source_height_sha256":height_hash(&input.painted.height),
        "roads":roads.json(), "roads_by_hierarchy":families,
        "endpoint_replay":input.endpoint_replay,
        "source_network_handoff":input.roads.source_layout,
        "source_xz_and_control_count_preserved":input.painted.report.circular_endpoint_closures.is_empty(),
        "source_circular_endpoint_closures":input.painted.report.circular_endpoint_closures,
        "paint":input.painted.report,
        "final_surfaces":final_surfaces.report, "source_meshes":generated.surfaces.report,
        "collision_chunks":generated.surfaces.chunks.iter().filter(|c|c.lod==0).count(),
        "collision_geometry_file":"collision-geometry.json",
        "clearance_diagnostic":audit,
        "approximate_colors":!palette.as_ref().is_some_and(|p|p.covers_layers(&layers))
    });
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let stage = tempfile::Builder::new()
        .prefix(".source-road-diagnostic-")
        .tempdir_in(parent)
        .map_err(|e| e.to_string())?;
    crate::image_export::write_png(
        &stage.path().join("source-stage-diagnostic.png"),
        &image,
        &cancelled,
    )?;
    if cancelled() {
        return Ok(None);
    }
    // Keep exact local vertices plus origins and authored top-face normals so
    // junction investigations can reproduce the clearance audit's f32 world
    // transform and driving-face selection without invoking generation again.
    std::fs::write(
        stage.path().join("collision-geometry.json"),
        serde_json::to_vec(&json!({
            "format":"mapper-private-source-road-collision-1",
            "accepted":false,
            "template":input.mesh.meshes[0].buffer,
            "chunks":generated.surfaces.chunks.iter().filter(|c|c.lod==0).collect::<Vec<_>>()
        }))
        .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(
        stage.path().join("report.json"),
        serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if cancelled() {
        return Ok(None);
    }
    std::fs::rename(stage.path(), destination).map_err(|e| e.to_string())?;
    Ok(Some(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoint_replay_restores_only_matching_xz_and_preserves_later_edits() {
        use papyrus_terrain::{path_geometry::Path as Curve, road_network::Destination};
        let parent =
            Curve::from_controls(&[[0., 0., 0.], [2., 0., 0.], [4., 0., 0.]], 10000, || false)
                .unwrap()
                .unwrap();
        let child =
            Curve::from_controls(&[[2., 4., 0.], [3., 4., 1.], [4., 4., 2.]], 10000, || false)
                .unwrap()
                .unwrap();
        let roads = [
            papyrus_terrain::road_paint::Road {
                path: &parent,
                hierarchy: 0,
            },
            papyrus_terrain::road_paint::Road {
                path: &child,
                hierarchy: 1,
            },
        ];
        let mut report = road_network::Report {
            roads: vec![
                road_network::Road {
                    from: Destination::Loop,
                    to: Destination::Loop,
                    points: parent.points().to_vec(),
                },
                road_network::Road {
                    from: Destination::Network { road: 0, point: 1 },
                    to: Destination::Monument {
                        monument: 7,
                        connector: 11,
                    },
                    points: child.points().to_vec(),
                },
            ],
            source_layout: Some(
                json!({"attachments":[{"road":1,"endpoint":0,"parent_road":0,"parent_point":1,"before":[1.,0.,0.],"after":[2.,0.,0.]}]}),
            ),
            ..Default::default()
        };
        let replay = restore_endpoints(&report, &roads, || false)
            .unwrap()
            .unwrap();
        assert_eq!(replay.paths[0].points(), parent.points());
        assert_eq!(
            replay.paths[1].points(),
            &[[1., 4., 0.], [3., 4., 1.], [4., 4., 2.]]
        );
        assert_eq!(child.points()[0], [2., 4., 0.]);
        assert_eq!(replay.receipt["rows"][0]["restored"], true);
        let moved = child
            .reattach_endpoints([Some([2.5, 4., 0.]), None], 10000, || false)
            .unwrap()
            .unwrap();
        let moved_roads = [
            papyrus_terrain::road_paint::Road {
                path: &parent,
                hierarchy: 0,
            },
            papyrus_terrain::road_paint::Road {
                path: &moved,
                hierarchy: 1,
            },
        ];
        let preserved = restore_endpoints(&report, &moved_roads, || false)
            .unwrap()
            .unwrap();
        assert_eq!(preserved.paths[1].points(), moved.points());
        assert_eq!(preserved.receipt["rows"][0]["restored"], false);
        assert!(
            restore_endpoints(&report, &roads, || true)
                .unwrap()
                .is_none()
        );
        let entry = report.source_layout.as_ref().unwrap()["attachments"][0].clone();
        report.source_layout.as_mut().unwrap()["attachments"]
            .as_array_mut()
            .unwrap()
            .push(entry);
        assert!(restore_endpoints(&report, &roads, || false).is_err());
    }

    #[test]
    fn source_height_capture_accepts_only_the_source_circular_endpoint_closure() {
        use papyrus_terrain::{path_geometry::Path as Curve, road_network::Destination};
        let path = Curve::from_controls(
            &[
                [0., 10., 0.],
                [10., 10., 0.],
                [10., 10., 10.],
                [0.05, 10., 0.02],
            ],
            10000,
            || false,
        )
        .unwrap()
        .unwrap();
        let original = road_network::Report {
            roads: vec![road_network::Road {
                from: Destination::Loop,
                to: Destination::Loop,
                points: path.points().to_vec(),
            }],
            ..Default::default()
        };
        let input = [papyrus_terrain::road_paint::Road {
            path: &path,
            hierarchy: 0,
        }];
        let closed = path
            .refit_heights(&[2., 8., 4., 3.], 8, &[0., 1., 1., 0.], 10000, || false)
            .unwrap()
            .unwrap();
        let captured = source_controls(&original, &input, std::slice::from_ref(&closed)).unwrap();
        assert_eq!(captured.roads[0].points, closed.points());
        assert_eq!(
            captured.roads[0].points.last(),
            captured.roads[0].points.first()
        );
        assert_eq!(path.points().last().unwrap()[0], 0.05);
        let mut changed = closed.points().to_vec();
        changed[1][0] += 0.01;
        let moved = Curve::from_controls(&changed, 10000, || false)
            .unwrap()
            .unwrap();
        assert!(source_controls(&original, &input, &[moved]).is_err());
    }

    #[test]
    fn source_height_capture_preserves_straightened_xz_and_connector_identities() {
        use papyrus_terrain::{path_geometry::Path as Curve, road_network::Destination};
        let points = [[0., 0., 0.], [2., 0., 1.], [4., 0., 2.]];
        let before = Curve::from_controls(&points, 10000, || false)
            .unwrap()
            .unwrap();
        let input = [papyrus_terrain::road_paint::Road {
            path: &before,
            hierarchy: 1,
        }];
        let original = road_network::Report {
            roads: vec![road_network::Road {
                from: Destination::Monument {
                    monument: 7,
                    connector: 11,
                },
                to: Destination::Monument {
                    monument: 8,
                    connector: 12,
                },
                // Roadside straightening legitimately changed these earlier points.
                points: vec![[0., 0., 0.], [2., 0., 0.], [4., 0., 2.]],
            }],
            ..Default::default()
        };
        let mut raised = points;
        for p in &mut raised {
            p[1] = 3.;
        }
        let after = Curve::from_controls(&raised, 10000, || false)
            .unwrap()
            .unwrap();
        let result = source_controls(&original, &input, std::slice::from_ref(&after)).unwrap();
        assert_eq!(result.roads[0].from, original.roads[0].from);
        assert_eq!(result.roads[0].to, original.roads[0].to);
        assert_eq!(result.roads[0].points, raised);
        raised[1][0] += 0.01;
        let moved = Curve::from_controls(&raised, 10000, || false)
            .unwrap()
            .unwrap();
        assert!(source_controls(&original, &input, &[moved]).is_err());
        assert!(source_controls(&original, &input, &[]).is_err());
    }
}
