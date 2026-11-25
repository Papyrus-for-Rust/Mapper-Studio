//! Cancellable source-policy road painting, before surface refinement and objects.
//! Input rasters stay immutable; no positive access/physics certificate is issued.
use crate::{road_paint_source, road_terrain_source};
use papyrus_terrain::{
    native_layers::Layers, path_geometry::Path, path_surface, path_terrain, road_grade::Junction,
    road_paint, road_terrain, road_topology,
};
use serde::Serialize;
mod roadside;
pub use roadside::{RoadsideChanges, RoadsideInput};

pub const VERSION: &str = "papyrus-road-paint-stage-1";
pub const RAIL_STAGES_VERSION: &str = "papyrus-road-rail-paint-stage-1";
pub struct Input<'a> {
    pub layers: &'a Layers,
    pub roads: &'a [road_paint::Road<'a>],
    pub junctions: &'a [Junction],
    pub paint: &'a road_paint_source::Profile,
    pub terrain: &'a road_terrain_source::Profile,
}

#[derive(Serialize)]
pub struct Report<Texture = road_paint::Report> {
    pub engine: &'static str,
    pub paint_profile_sha256: String,
    pub terrain_profile_sha256: String,
    pub topology: road_paint::Report,
    pub marking: road_topology::Report,
    pub height: road_terrain::Report,
    pub texture: Texture,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roadside_height_work: Option<usize>,
    pub exact_joins: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub circular_endpoint_closures: Vec<EndpointClosure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retained_rails: Option<crate::retained_rail_height::Receipt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rail_topology: Option<crate::rail_topology_stage::Report>,
    pub runtime_access_validated: bool,
}
#[derive(Debug, Serialize)]
pub struct EndpointClosure {
    pub road: usize,
    pub before: [f32; 2],
    pub after: [f32; 2],
}
pub(crate) fn painted_control_changes(
    before: &[road_paint::Road<'_>],
    after: &[Path],
    smoothing: bool,
) -> Result<Vec<EndpointClosure>, String> {
    let same_xz = |a: &[f32; 3], b: &[f32; 3]| {
        a[0].to_bits() == b[0].to_bits() && a[2].to_bits() == b[2].to_bits()
    };
    let mut closures = Vec::new();
    let mut invalid = before.len() != after.len();
    for (road, (a, b)) in after.iter().zip(before).enumerate() {
        let original = b.path;
        if a.points().len() != original.points().len()
            || a.is_circular() != original.is_circular()
            || a.range() != original.range()
        {
            invalid = true;
            break;
        }
        let last = original.points().len() - 1;
        for (i, (new, old)) in a.points().iter().zip(original.points()).enumerate() {
            if same_xz(new, old) {
                continue;
            }
            // PathInterpolator.SmoothenIndex(0) copies its full first point to
            // the final endpoint, including a sub-0.1m closure gap. No other
            // horizontal change belongs to source terrain smoothing.
            if smoothing
                && original.is_circular()
                && i == last
                && new
                    .iter()
                    .zip(a.points()[0])
                    .all(|(x, y)| x.to_bits() == y.to_bits())
            {
                closures.push(EndpointClosure {
                    road,
                    before: [old[0], old[2]],
                    after: [new[0], new[2]],
                });
            } else {
                invalid = true;
                break;
            }
        }
        if invalid {
            break;
        }
    }
    if !invalid {
        return Ok(closures);
    }
    let witness=after.iter().zip(before).enumerate().find_map(|(road,(a,b))| {
        let changed=a.points().iter().zip(b.path.points()).enumerate().find(|(_, (a,b))|!same_xz(a,b));
        (a.points().len()!=b.path.points().len() || a.is_circular()!=b.path.is_circular()
            || a.range()!=b.path.range() || changed.is_some()).then(||serde_json::json!({
            "road":road,"hierarchy":b.hierarchy,"before_count":b.path.points().len(),"after_count":a.points().len(),
            "circular":b.path.is_circular(),"after_circular":a.is_circular(),"before_first":b.path.points().first(),"before_last":b.path.points().last(),
            "after_first":a.points().first(),"after_last":a.points().last(),
            "first_changed":changed.map(|(i,(after,before))|serde_json::json!({"control":i,"before":before,"after":after}))
        }))
    });
    Err(format!(
        "Road painting changed horizontal controls: {}",
        serde_json::json!({
        "before_roads":before.len(),"after_roads":after.len(),"witness":witness})
    ))
}
pub struct Output<Splat = Vec<u8>, Texture = road_paint::Report> {
    pub height: Vec<i16>,
    pub source_rounding: road_terrain::SourceCutRounding,
    pub topology: Vec<i32>,
    pub splat: Splat,
    pub roads: Vec<Path>,
    pub report: Report<Texture>,
}
pub(crate) type Preliminary = Output<Option<Vec<u8>>, Option<road_paint::Report>>;

fn require_texture(painted: Preliminary) -> Result<Output, String> {
    let Output { height, source_rounding, topology, splat, roads, report } = painted;
    let Report {
        engine, paint_profile_sha256, terrain_profile_sha256, topology: topology_report,
        marking, height: height_report, texture, roadside_height_work, exact_joins,
        circular_endpoint_closures, retained_rails, rail_topology, runtime_access_validated,
    } = report;
    Ok(Output {
        height,
        source_rounding,
        topology,
        splat: splat.ok_or("Missing road material result")?,
        roads,
        report: Report {
            engine,
            paint_profile_sha256,
            terrain_profile_sha256,
            topology: topology_report,
            marking,
            height: height_report,
            texture: texture.ok_or("Missing road material report")?,
            roadside_height_work,
            exact_joins,
            circular_endpoint_closures,
            retained_rails,
            rail_topology,
            runtime_access_validated,
        },
    })
}
#[derive(Serialize)]
pub struct FinalReport {
    pub engine: &'static str,
    pub paint_profile_sha256: String,
    pub topology: road_paint::Report,
    pub marking: road_topology::Report,
    pub texture: road_paint::Report,
    pub controls_checked_against_final_height: usize,
    pub ocean_reclassified: bool,
    pub local_water_preserved: bool,
    pub runtime_access_validated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retained_rails: Option<crate::retained_rail_scene::Receipt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retained_height_checks: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retained_curve_checks: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rail_topology: Option<crate::rail_topology_stage::Report>,
}
pub struct FinalSurfaces {
    /// Owned only when a final roadside stage changes the road-earthwork grid.
    pub height: Option<Vec<i16>>,
    pub topology: Vec<i32>,
    pub splat: Vec<u8>,
    pub ocean: Option<papyrus_terrain::ocean::Ocean>,
    pub report: FinalReport,
}
#[derive(Clone, Copy)]
pub struct RetainedSurfaceContext<'a> {
    pub recipe: &'a crate::rail_generation::Recipe,
    pub scene_receipt: &'a crate::retained_rail_scene::Receipt,
    pub height: &'a crate::retained_rail_height::RetainedHeight,
    pub rail_stages: Option<&'a crate::rail_topology_stage::Prepared<'a>>,
}
pub struct FinalSurfaceInput<'a> {
    pub base: &'a papyrus_terrain::monument_composition::Composed,
    pub height: &'a [i16],
    pub roads: &'a [road_paint::Road<'a>],
    pub profile: &'a road_paint_source::Profile,
    pub retained: Option<RetainedSurfaceContext<'a>>,
}
/// Repaint from the pre-road stamped rasters over the final corrected paths.
/// Reusing previously painted splat would leave a ghost footprint where the
/// source's distance-dependent width noise changes after height correction.
pub fn finish_surfaces(
    base: &papyrus_terrain::monument_composition::Composed,
    height: &[i16],
    roads: &[road_paint::Road<'_>],
    profile: &road_paint_source::Profile,
    maximum_work: usize,
    noise: impl FnMut(&[f32]) -> Result<Option<Vec<f32>>, String>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<FinalSurfaces>, String> {
    finish_surfaces_with_context(
        FinalSurfaceInput {
            base,
            height,
            roads,
            profile,
            retained: None,
        },
        maximum_work,
        noise,
        cancelled,
    )
}
pub fn finish_surfaces_with_context(
    input: FinalSurfaceInput<'_>,
    maximum_work: usize,
    noise: impl FnMut(&[f32]) -> Result<Option<Vec<f32>>, String>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<FinalSurfaces>, String> {
    finish_surfaces_with_roadside(input, maximum_work, noise, |_| Ok(None), cancelled)
}

/// Run final roadside admission after road/rail marking and corrected earthwork,
/// before the authoritative texture pass. Its private changes publish together.
pub fn finish_surfaces_with_roadside(
    input: FinalSurfaceInput<'_>,
    maximum_work: usize,
    noise: impl FnMut(&[f32]) -> Result<Option<Vec<f32>>, String>,
    roadside: impl FnOnce(RoadsideInput<'_, '_>) -> Result<Option<RoadsideChanges>, String>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<FinalSurfaces>, String> {
    finish_surfaces_with_ocean_mask(input, None, maximum_work, noise, roadside, cancelled)
}

/// The supplied mask must classify the exact height grid in the input.
pub(crate) fn finish_surfaces_with_ocean_mask(
    input: FinalSurfaceInput<'_>,
    audited_ocean: Option<papyrus_terrain::ocean::Mask>,
    maximum_work: usize,
    mut noise: impl FnMut(&[f32]) -> Result<Option<Vec<f32>>, String>,
    roadside: impl FnOnce(RoadsideInput<'_, '_>) -> Result<Option<RoadsideChanges>, String>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<FinalSurfaces>, String> {
    let FinalSurfaceInput {
        base,
        height,
        roads,
        profile,
        retained,
    } = input;
    if cancelled() {
        return Ok(None);
    }
    let layers = &base.layers;
    let (hr, n) = papyrus_terrain::native_layers::dimensions(layers.size)?;
    if layers.height_resolution != hr
        || layers.surface_resolution != n
        || height.len() != hr * hr
        || layers.topology.len() != n * n
        || layers.splat.len() != n * n * 8
        || base.alpha.len() != n * n
        || base.roads.is_some()
        || (base.rails.is_some() && retained.is_none())
        || base.objects.is_some()
        || base.decoration.is_some()
        || base.gameplay_height.is_some()
        || base.spawn_audit.is_some()
        || base.topology_before_overrides.is_some()
        || base.topology_overrides.is_some()
    {
        return Err("Final road surfaces require the pristine pre-road stamped revision".into());
    }
    if let Some(ocean) = &base.ocean
        && (ocean.resolution != n
            || ocean.local_water.len() != height.len()
            || ocean.topology.len() != n * n
            || ocean.classification.len() != n * n)
    {
        return Err("Pre-road water dimensions differ".into());
    }
    let profile_sha256 = profile.sha256()?;
    let mut curve_checks = None;
    let mut retained_checks = if let Some(context) = retained {
        let scene = base
            .rails
            .as_ref()
            .ok_or("Final road surfaces require the retained rail scene")?;
        if context
            .scene_receipt
            .check(scene, context.recipe, n, &cancelled)?
            .is_none()
        {
            return Ok(None);
        }
        let height_checks = context.height.samples().len();
        let curve_budget = maximum_work
            .checked_sub(height_checks)
            .filter(|&v| v > 0)
            .ok_or("Final retained height validation exhausted surface work")?;
        if let Some(rails) = context.rail_stages {
            let Some(work) = rails.check_scene(scene, context.recipe, curve_budget, &cancelled)?
            else {
                return Ok(None);
            };
            curve_checks = Some(work);
        }
        if !context.height.validate_retained_grid(
            path_terrain::Grid {
                size: layers.size as f32, resolution: hr,
                origin_y: profile.height_origin, vertical_span: profile.vertical_span, height,
            }, &cancelled,
        )? {
            return Ok(None);
        }
        Some(height_checks)
    } else {
        None
    };
    let maximum_work = if let Some(checks) = retained_checks {
        maximum_work
            .checked_sub(checks)
            .and_then(|v| v.checked_sub(curve_checks.unwrap_or(0)))
            .filter(|&v| v > 0)
            .ok_or("Final retained height validation exhausted surface work")?
    } else {
        maximum_work
    };
    let Some(prepared) = road_paint::prepare(roads, &profile.policy, maximum_work, &cancelled)?
    else {
        return Ok(None);
    };
    let grid = path_terrain::Grid {
        size: layers.size as f32,
        resolution: hr,
        origin_y: profile.height_origin,
        vertical_span: profile.vertical_span,
        height,
    };
    let check_controls = |grid| -> Result<Option<usize>, String> {
        let mut controls = 0;
        for road in roads {
            for &point in road.path.points() {
                if cancelled() { return Ok(None); }
                if path_terrain::sample_height(grid, point)?.to_bits() != point[1].to_bits() {
                    return Err("Final road material paths do not match corrected terrain".into());
                }
                controls += 1;
            }
        }
        Ok(Some(controls))
    };
    let Some(controls) = check_controls(grid)? else { return Ok(None); };
    let coordinates = prepared.noise_coordinates();
    let values = if coordinates.is_empty() {
        Vec::new()
    } else {
        let Some(values) = noise(&coordinates)? else {
            return Ok(None);
        };
        values
    };
    let geometry = path_surface::Grid {
        size: grid.size,
        resolution: n,
        origin_y: grid.origin_y,
        vertical_span: grid.vertical_span,
    };
    let Some(topology) = prepared.apply(
        road_paint::Grid {
            geometry,
            splat: &layers.splat,
            topology: &layers.topology,
        },
        &values,
        road_paint::Pass::Topology,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let proposed = topology.topology.ok_or("Missing final road topology")?;
    let Some((mut marked, marking)) = road_topology::apply_with_height(
        layers,
        height,
        &proposed,
        profile.post_topology,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    // Repaint from the pristine base, never from an already dilated rail result.
    // Rail marking retains the pre-road height snapshot used by its source stage.
    let rail_topology = if let Some(rails) = retained.and_then(|c| c.rail_stages) {
        let Some(output) = rails.apply(
            layers,
            &marked,
            crate::road_surface_stage::HeightGeometry {
                origin_y: geometry.origin_y,
                vertical_span: geometry.vertical_span,
            },
            maximum_work,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        marked = output.topology;
        Some(output.report)
    } else {
        None
    };
    let view = papyrus_terrain::native_layers::View {
        height,
        topology: &marked,
        ..layers.view()
    };
    let changes = roadside(RoadsideInput { layers: view, roads });
    if cancelled() {
        return Ok(None);
    }
    let mut changed_height = None;
    let mut splat = std::borrow::Cow::Borrowed(layers.splat.as_slice());
    if let Some(changes) = changes? {
        if !changes.validate(view, maximum_work, &cancelled)? {
            return Ok(None);
        }
        changed_height = changes.height;
        if let Some(topology) = changes.topology {
            marked = topology;
        }
        if let Some(changed) = changes.splat {
            splat = std::borrow::Cow::Owned(changed);
        }
    }
    let height = changed_height.as_deref().unwrap_or(height);
    let grid = path_terrain::Grid { height, ..grid };
    if changed_height.is_some() {
        if let Some(context) = retained {
            if !context.height.validate_retained_grid(grid, &cancelled)? { return Ok(None); }
            retained_checks = retained_checks.map(|checks| checks * 2);
        }
        if check_controls(grid)?.is_none() { return Ok(None); }
    }
    let Some(texture) = prepared.apply(
        road_paint::Grid {
            geometry,
            splat: &splat,
            topology: &marked,
        },
        &values,
        road_paint::Pass::Texture,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let ocean = if let Some(previous) = &base.ocean {
        let ocean = match audited_ocean {
            Some(mask) if changed_height.is_none() && mask.resolution == n => {
                papyrus_terrain::ocean::reconstruct_from_mask(
                    mask, &marked, hr, &previous.local_water, &cancelled,
                )?
            }
            _ => papyrus_terrain::ocean::reconstruct_with_water(
                height, hr, &marked, n, Some(&previous.local_water), &cancelled,
            )?,
        };
        let Some(ocean) = ocean else {
            return Ok(None);
        };
        marked.clone_from(&ocean.topology);
        Some(ocean)
    } else {
        None
    };
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(FinalSurfaces {
        height: changed_height,
        topology: marked,
        splat: texture.splat.ok_or("Missing final road splat")?,
        ocean,
        report: FinalReport {
            engine: if rail_topology.is_some() {
                "papyrus-final-road-rail-surfaces-1"
            } else {
                "papyrus-final-road-surfaces-1"
            },
            paint_profile_sha256: profile_sha256,
            topology: topology.report,
            marking,
            texture: texture.report,
            controls_checked_against_final_height: controls,
            ocean_reclassified: base.ocean.is_some(),
            local_water_preserved: true,
            runtime_access_validated: false,
            retained_rails: retained.map(|r| r.scene_receipt.clone()),
            retained_height_checks: retained_checks,
            retained_curve_checks: curve_checks,
            rail_topology,
        },
    }))
}
/// Each primitive shares this per-operation ceiling. Native provider and water
/// queries must enforce their own limits and honor the supplied cancellation.
/// No rail paths are silently omitted: this adapter accepts roads only.
pub fn paint(
    input: Input<'_>,
    maximum_work: usize,
    noise: impl FnMut(&[f32]) -> Result<Option<Vec<f32>>, String>,
    water_height: impl FnMut([f32; 3]) -> Result<Option<f32>, String>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    paint_with_retained_rails(input, None, maximum_work, noise, water_height, cancelled)
}
/// Source GenerateRoadTerrain sweeps roads in reverse, then retained rails in
/// reverse at the captured quarter fade. Road texture/topology passes remain
/// road-only. Combined generation still needs final rail/road fit validation.
pub fn paint_with_retained_rails(
    input: Input<'_>,
    retained_rails: Option<&crate::retained_rail_height::RetainedRails<'_>>,
    maximum_work: usize,
    noise: impl FnMut(&[f32]) -> Result<Option<Vec<f32>>, String>,
    water_height: impl FnMut([f32; 3]) -> Result<Option<f32>, String>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    paint_inner(
        input,
        retained_rails,
        None,
        maximum_work,
        noise,
        water_height,
        |_| Ok(None),
        true,
        cancelled,
    )
    .and_then(|painted| painted.map(require_texture).transpose())
}
/// Source-ordered road topology, rail topology, road terrain (including rails),
/// then road texture. Late rail texture remains a separate post-refinement pass.
pub fn paint_with_rail_stages(
    input: Input<'_>,
    rails: &crate::rail_topology_stage::Prepared<'_>,
    maximum_work: usize,
    noise: impl FnMut(&[f32]) -> Result<Option<Vec<f32>>, String>,
    water_height: impl FnMut([f32; 3]) -> Result<Option<f32>, String>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    paint_inner(
        input,
        Some(rails.retained()),
        Some(rails),
        maximum_work,
        noise,
        water_height,
        |_| Ok(None),
        true,
        cancelled,
    )
    .and_then(|painted| painted.map(require_texture).transpose())
}
/// Run a source-bound roadside pass after road/rail earthwork and before road
/// texture. The callback owns its fitting/admission budgets and stage receipt.
/// All raster changes remain private until this complete paint operation succeeds.
pub fn paint_with_roadside(
    input: Input<'_>,
    rails: Option<&crate::rail_topology_stage::Prepared<'_>>,
    maximum_work: usize,
    noise: impl FnMut(&[f32]) -> Result<Option<Vec<f32>>, String>,
    water_height: impl FnMut([f32; 3]) -> Result<Option<f32>, String>,
    roadside: impl FnOnce(RoadsideInput<'_, '_>) -> Result<Option<RoadsideChanges>, String>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    paint_inner(
        input,
        rails.map(|source| source.retained()),
        rails,
        maximum_work,
        noise,
        water_height,
        roadside,
        true,
        cancelled,
    )
    .and_then(|painted| painted.map(require_texture).transpose())
}

/// The final source-guided pass repaints material from the stamped revision.
/// Early fitting can retain just heights, topology and controls in that case.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_for_composition(
    input: Input<'_>,
    rails: Option<&crate::rail_topology_stage::Prepared<'_>>,
    maximum_work: usize,
    noise: impl FnMut(&[f32]) -> Result<Option<Vec<f32>>, String>,
    water_height: impl FnMut([f32; 3]) -> Result<Option<f32>, String>,
    roadside: impl FnOnce(RoadsideInput<'_, '_>) -> Result<Option<RoadsideChanges>, String>,
    preliminary_texture: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Preliminary>, String> {
    paint_inner(
        input, rails.map(|rails| rails.retained()), rails, maximum_work, noise,
        water_height, roadside, preliminary_texture, cancelled,
    )
}
#[allow(clippy::too_many_arguments)] // Retained rail providers and the ordered roadside boundary are explicit.
fn paint_inner(
    input: Input<'_>,
    retained_rails: Option<&crate::retained_rail_height::RetainedRails<'_>>,
    rail_stages: Option<&crate::rail_topology_stage::Prepared<'_>>,
    maximum_work: usize,
    mut noise: impl FnMut(&[f32]) -> Result<Option<Vec<f32>>, String>,
    water_height: impl FnMut([f32; 3]) -> Result<Option<f32>, String>,
    roadside: impl FnOnce(RoadsideInput<'_, '_>) -> Result<Option<RoadsideChanges>, String>,
    preliminary_texture: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Preliminary>, String> {
    let mut timing = crate::composition::StageTimer::start("road_paint");
    macro_rules! ready {
        ($value:expr) => {
            match $value? {
                Some(value) => value,
                None => return Ok(None),
            }
        };
    }
    if cancelled() {
        return Ok(None);
    }
    if !(1..=512_000_000).contains(&maximum_work) || input.junctions.len() > 131072 {
        return Err("Invalid road painting limits".into());
    }
    let paint_hash = input.paint.sha256()?;
    let expected = road_terrain_source::Profile::for_paint(input.paint)?
        .ok_or("No matching road terrain policy")?;
    let terrain_hash = input.terrain.sha256()?;
    if terrain_hash != expected.sha256()? {
        return Err("Road terrain policy differs from bound paint profile".into());
    }
    let layers = input.layers;
    let n = layers.surface_resolution;
    let hr = layers.height_resolution;
    let expected_dimensions = papyrus_terrain::native_layers::dimensions(layers.size)?;
    if (hr, n) != expected_dimensions
        || layers.height.len() != hr * hr
        || layers.topology.len() != n * n
        || layers.splat.len() != n * n * 8
    {
        return Err("Invalid road paint rasters".into());
    }
    let geometry = path_surface::Grid {
        size: layers.size as f32,
        resolution: n,
        origin_y: input.paint.height_origin,
        vertical_span: input.paint.vertical_span,
    };
    let check_joins = |paths: &[&Path]| -> Result<(), String> {
        for join in input.junctions {
            let point = |(r, i): (usize, usize)| paths.get(r).and_then(|p| p.points().get(i));
            let a = point(join.a).ok_or("Invalid road attachment")?;
            let b = point(join.b).ok_or("Invalid road attachment")?;
            if a.iter().zip(b).any(|(a, b)| a.to_bits() != b.to_bits()) {
                return Err("Road painting separated a network attachment".into());
            }
        }
        Ok(())
    };
    check_joins(&input.roads.iter().map(|r| r.path).collect::<Vec<_>>())?;
    let prepared = ready!(road_paint::prepare(
        input.roads,
        &input.paint.policy,
        maximum_work,
        &cancelled
    ));
    let coordinates = prepared.noise_coordinates();
    let values = if coordinates.is_empty() {
        Vec::new()
    } else {
        ready!(noise(&coordinates))
    };
    let topology = ready!(prepared.apply(
        road_paint::Grid {
            geometry,
            splat: &layers.splat,
            topology: &layers.topology,
        },
        &values,
        road_paint::Pass::Topology,
        &cancelled
    ));
    let proposed_topology = topology.topology.ok_or("Missing road topology result")?;
    let (marked, marking) = ready!(road_topology::apply_with_topology(
        layers,
        &proposed_topology,
        input.paint.post_topology,
        &cancelled
    ));
    let (mut marked, rail_topology) = if let Some(rails) = rail_stages {
        let painted = ready!(rails.apply(
            layers,
            &marked,
            crate::road_surface_stage::HeightGeometry {
                origin_y: geometry.origin_y,
                vertical_span: geometry.vertical_span,
            },
            maximum_work,
            &cancelled
        ));
        (painted.topology, Some(painted.report))
    } else {
        (marked, None)
    };
    crate::composition::record_timing(&mut timing, "path_topology");
    let height_paths = input
        .roads
        .iter()
        .map(|road| {
            let shape = input
                .paint
                .policy
                .families
                .iter()
                .find(|f| f.hierarchy == road.hierarchy)
                .ok_or("Missing road paint shape")?;
            let family = input
                .terrain
                .families
                .iter()
                .find(|f| f.hierarchy == road.hierarchy)
                .ok_or("Missing road terrain shape")?;
            Ok(road_terrain::InputPath {
                path: road.path,
                shape: path_terrain::Shape {
                    width: shape.width,
                    padding: shape.padding,
                    fade: family.fade,
                },
                terrain_offset: family.terrain_offset,
                random_scale: shape.random_scale,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    #[cfg(feature = "road-diagnostics")]
    let mut height_trace = crate::road_height_diagnostics::Trace::begin(path_terrain::Grid {
        size: layers.size as f32,
        resolution: hr,
        origin_y: geometry.origin_y,
        vertical_span: geometry.vertical_span,
        height: &layers.height,
    })?;
    let mut fitted = ready!(road_terrain::run_observed(
        road_terrain::Input {
            grid: path_terrain::Grid {
                size: layers.size as f32,
                resolution: hr,
                origin_y: geometry.origin_y,
                vertical_span: geometry.vertical_span,
                height: &layers.height
            },
            topology: &marked,
            surface_resolution: n,
            roads: &height_paths,
            rails: retained_rails.map_or(&[], |r| r.paths()),
        },
        &input.terrain.policy,
        maximum_work,
        &mut noise,
        water_height,
        |_event| {
            #[cfg(feature = "road-diagnostics")]
            if let Some(trace) = &mut height_trace {
                trace.observe(_event);
            }
        },
        &cancelled
    ));
    #[cfg(feature = "road-diagnostics")]
    if let Some(trace) = height_trace {
        trace.finish(&fitted.height)?;
    }
    crate::composition::record_timing(&mut timing, "path_height");
    check_joins(&fitted.roads.iter().collect::<Vec<_>>())?;
    let circular_endpoint_closures = painted_control_changes(
        input.roads,
        &fitted.roads,
        input.terrain.policy.vertical_iterations > 0 && !input.terrain.policy.road_fades.is_empty(),
    )?;
    let texture_paths = fitted
        .roads
        .iter()
        .zip(input.roads)
        .map(|(path, r)| road_paint::Road {
            path,
            hierarchy: r.hierarchy,
        })
        .collect::<Vec<_>>();
    if cancelled() {
        return Ok(None);
    }
    let view = papyrus_terrain::native_layers::View {
        height: &fitted.height,
        topology: &marked,
        ..layers.view()
    };
    let changes = roadside(RoadsideInput {
        layers: view,
        roads: &texture_paths,
    });
    crate::composition::record_timing(&mut timing, "roadside_callback");
    if cancelled() {
        return Ok(None);
    }
    let changes = changes?;
    let mut splat = std::borrow::Cow::Borrowed(layers.splat.as_slice());
    let mut height_changed = false;
    if let Some(changes) = changes {
        if !changes.validate(view, maximum_work, &cancelled)? {
            return Ok(None);
        }
        if let Some(height) = changes.height {
            fitted.height = height;
            height_changed = true;
        }
        if let Some(topology) = changes.topology {
            marked = topology;
        }
        if let Some(changed) = changes.splat {
            splat = std::borrow::Cow::Owned(changed);
        }
    }
    drop(texture_paths);
    let roadside_height_work = if height_changed {
        let grid = path_terrain::Grid {
            size: layers.size as f32, resolution: hr,
            origin_y: input.paint.height_origin, vertical_span: input.paint.vertical_span,
            height: &fitted.height,
        };
        let mut work = 0usize;
        for path in &mut fitted.roads {
            if cancelled() { return Ok(None); }
            work = work.checked_add(path.points().len()).filter(|&work| work <= maximum_work)
                .ok_or("Roadside road-height sampling exhausted its work allowance")?;
            let heights = path.points().iter().map(|&point| path_terrain::sample_height(grid, point))
                .collect::<Result<Vec<_>, _>>()?;
            if path.points().iter().zip(&heights).all(|(point, height)| point[1].to_bits() == height.to_bits()) {
                continue;
            }
            let refitted = ready!(path.refit_heights(
                &heights, 0, &vec![1.; heights.len()], maximum_work.saturating_sub(work).min(32_000_000), &cancelled,
            ).map_err(|error| format!("Roadside road-height update failed {error:?}")));
            work = work.checked_add(refitted.work()).filter(|&work| work <= maximum_work)
                .ok_or("Roadside road-height update exhausted its work allowance")?;
            *path = refitted;
        }
        check_joins(&fitted.roads.iter().collect::<Vec<_>>())?;
        Some(work)
    } else { None };
    let (splat, texture) = if preliminary_texture {
        let texture_paths = fitted.roads.iter().zip(input.roads).map(|(path, road)| road_paint::Road {
            path, hierarchy: road.hierarchy,
        }).collect::<Vec<_>>();
        let texture_prepared = ready!(road_paint::prepare(
            &texture_paths, &input.paint.policy, maximum_work, &cancelled,
        ));
        let coordinates = texture_prepared.noise_coordinates();
        let values = if coordinates.is_empty() {
            Vec::new()
        } else {
            ready!(noise(&coordinates))
        };
        let texture = ready!(texture_prepared.apply(
            road_paint::Grid { geometry, splat: &splat, topology: &marked },
            &values, road_paint::Pass::Texture, &cancelled,
        ));
        (Some(texture.splat.ok_or("Missing road material result")?), Some(texture.report))
    } else {
        (None, None)
    };
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(Output {
        height: fitted.height,
        source_rounding: fitted.source_rounding,
        topology: marked,
        splat,
        roads: fitted.roads,
        report: Report {
            engine: if rail_stages.is_some() {
                RAIL_STAGES_VERSION
            } else {
                VERSION
            },
            paint_profile_sha256: paint_hash,
            terrain_profile_sha256: terrain_hash,
            topology: topology.report,
            marking,
            height: fitted.report,
            texture,
            roadside_height_work,
            exact_joins: input.junctions.len(),
            circular_endpoint_closures,
            retained_rails: retained_rails.map(|r| r.receipt().clone()),
            rail_topology,
            runtime_access_validated: false,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    #[test]
    fn source_circular_closure_is_recorded_without_allowing_other_horizontal_changes() {
        let make = |points: &[[f32; 3]]| {
            Path::from_controls(points, 10000, || false)
                .unwrap()
                .unwrap()
        };
        let original = make(&[
            [0., 10., 0.],
            [10., 10., 0.],
            [10., 10., 10.],
            [0.05, 10., 0.02],
        ]);
        assert!(original.is_circular());
        let roads = [road_paint::Road {
            path: &original,
            hierarchy: 0,
        }];
        let closed = original
            .refit_heights(&[2., 8., 4., 3.], 8, &[0., 1., 1., 0.], 10000, || false)
            .unwrap()
            .unwrap();
        let audit = painted_control_changes(&roads, std::slice::from_ref(&closed), true).unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].road, 0);
        assert_eq!(audit[0].before, [0.05, 0.02]);
        assert_eq!(audit[0].after, [0., 0.]);
        assert!(painted_control_changes(&roads, std::slice::from_ref(&closed), false).is_err());
        let unsmoothed = original
            .refit_heights(&[2., 8., 4., 3.], 0, &[0., 1., 1., 0.], 10000, || false)
            .unwrap()
            .unwrap();
        assert!(
            painted_control_changes(&roads, &[unsmoothed], false)
                .unwrap()
                .is_empty()
        );
        for kind in 0..4 {
            let mut points = closed.points().to_vec();
            match kind {
                0 => points[1][0] += 0.0001,
                1 => points[0][0] += 0.001,
                2 => points[3][0] = 0.001,
                _ => points.push(points[0]),
            }
            assert!(
                painted_control_changes(&roads, &[make(&points)], true).is_err(),
                "Invalid edit {kind} accepted"
            );
        }
        let open = make(&[
            [0., 10., 0.],
            [10., 10., 0.],
            [10., 10., 10.],
            [0.12, 10., 0.02],
        ]);
        assert!(!open.is_circular());
        assert!(
            painted_control_changes(
                &[road_paint::Road {
                    path: &open,
                    hierarchy: 1
                }],
                std::slice::from_ref(&closed),
                true
            )
            .is_err()
        );
        assert!(painted_control_changes(&roads, &[], true).is_err());
        assert!(painted_control_changes(&roads, &[closed.clone(), closed], true).is_err());
    }
    pub(super) fn profiles() -> (road_paint_source::Profile, road_terrain_source::Profile) {
        let paint = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-road-paint.json"
        ))
        .unwrap();
        let terrain = road_terrain_source::Profile::for_paint(&paint)
            .unwrap()
            .unwrap();
        (paint, terrain)
    }
    pub(super) fn layers() -> Layers {
        let (hr, n) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let mut splat = vec![0; n * n * 8];
        splat[n * n..n * n * 2].fill(255);
        Layers {
            size: 1000,
            terrain_seed: 7,
            biome_seed: 8,
            height_resolution: hr,
            surface_resolution: n,
            height: vec![17039; hr * hr],
            topology: vec![1; n * n],
            splat,
            biome: vec![0; n * n * 5],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        }
    }
    pub(super) fn paths(layers: &Layers, paint: &road_paint_source::Profile) -> Vec<Path> {
        let y = path_terrain::sample_height(
            path_terrain::Grid {
                size: layers.size as f32,
                resolution: layers.height_resolution,
                origin_y: paint.height_origin,
                vertical_span: paint.vertical_span,
                height: &layers.height,
            },
            [0.; 3],
        )
        .unwrap();
        [
            vec![[-100., y, 0.], [0., y, 0.], [100., y, 0.]],
            vec![[0., y, 0.], [0., y, 75.], [0., y, 150.]],
        ]
        .iter()
        .map(|p| Path::from_controls(p, 10000, || false).unwrap().unwrap())
        .collect()
    }
    #[test]
    fn current_and_legacy_sequences_paint_paved_and_dirt_and_keep_joins_and_inputs() {
        for raw in [
            include_str!("../../../compatibility/infrastructure/client-0f703d43-road-paint.json"),
            include_str!("../../../compatibility/infrastructure/client-8dd4e235-road-paint.json"),
        ] {
            let layers = layers();
            let paint_profile: road_paint_source::Profile = serde_json::from_str(raw).unwrap();
            let terrain = road_terrain_source::Profile::for_paint(&paint_profile)
                .unwrap()
                .unwrap();
            let paths = paths(&layers, &paint_profile);
            let roads = [
                road_paint::Road {
                    path: &paths[0],
                    hierarchy: 1,
                },
                road_paint::Road {
                    path: &paths[1],
                    hierarchy: 2,
                },
            ];
            let joins = [Junction {
                a: (0, 1),
                b: (1, 0),
            }];
            let before = (
                &layers.height.clone(),
                &layers.topology.clone(),
                &layers.splat.clone(),
            );
            let output = paint(
                Input {
                    layers: &layers,
                    roads: &roads,
                    junctions: &joins,
                    paint: &paint_profile,
                    terrain: &terrain,
                },
                32_000_000,
                |p| Ok(Some(vec![0.; p.len()])),
                |_| Ok(Some(terrain.water_no_hit_level)),
                || false,
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                output.report.marking.engine,
                if paint_profile.post_topology.normal_sampling.is_some() {
                    road_topology::NATIVE_VERSION
                } else {
                    road_topology::VERSION
                }
            );
            assert_eq!(output.report.exact_joins, 1);
            assert!(
                output.report.topology.changed_cells > 0 && output.report.texture.changed_cells > 0
            );
            assert!(
                output
                    .report
                    .texture
                    .families
                    .iter()
                    .any(|f| f.hierarchy == 2 && f.paths == 1 && f.callbacks > 0)
            );
            let n = layers.surface_resolution;
            let sample = |x: f32, z: f32| {
                ((z + 500.) / 1000. * n as f32) as usize * n
                    + ((x + 500.) / 1000. * n as f32) as usize
            };
            assert_eq!(output.splat[7 * n * n + sample(60., 0.)], 255);
            assert_eq!(output.splat[sample(0., 100.)], 255);
            assert_eq!(output.roads[0].points()[1], output.roads[1].points()[0]);
            for (a, b) in paths.iter().zip(&output.roads) {
                for (a, b) in a.points().iter().zip(b.points()) {
                    assert_eq!(
                        [a[0].to_bits(), a[2].to_bits()],
                        [b[0].to_bits(), b[2].to_bits()]
                    );
                }
            }
            assert!(!output.report.runtime_access_validated);
            assert_eq!((&layers.height, &layers.topology, &layers.splat), before);
        }
    }
    #[test]
    fn dirt_only_network_survives_paint_bed_refinement_and_final_repaint_without_paved_meshes() {
        terrain_only_pipeline(false, false);
    }
    #[test]
    fn empty_network_runs_source_topology_without_native_noise_or_invented_terrain() {
        terrain_only_pipeline(true, false);
    }
    #[test]
    fn retained_rail_height_crosses_source_paint_initial_fit_refinement_and_late_texture() {
        terrain_only_pipeline(false, true);
    }
    fn terrain_only_pipeline(empty: bool, retaining: bool) {
        use papyrus_terrain::road_network::{Destination, Family, Report, Road};
        let mut layers = layers();
        if empty {
            // Authored road topology can exist even without generated paths.
            let n = layers.surface_resolution;
            layers.topology[(n / 2) * n + n / 2] |= 2048;
        }
        let original_topology = layers.topology.clone();
        let (profile, terrain) = profiles();
        let paths = paths(&layers, &profile);
        let roads = [road_paint::Road {
            path: &paths[1],
            hierarchy: 2,
        }]
        .into_iter()
        .take(usize::from(!empty))
        .collect::<Vec<_>>();
        let (_rail_root, rail_parent, rail_source) =
            crate::retained_rail_height::tests::source_fixture();
        let rail_paths = [Path::from_controls(
            &[[-200., 21., 300.], [0., 21., 300.], [200., 21., 300.]],
            10000,
            || false,
        )
        .unwrap()
        .unwrap()];
        let retained_paths = crate::retained_rail_height::RetainedRails::prepare(
            &rail_source,
            &rail_parent,
            &rail_paths,
            10000,
            || false,
        )
        .unwrap()
        .unwrap();
        let painted = paint_with_retained_rails(
            Input {
                layers: &layers,
                roads: &roads,
                junctions: &[],
                paint: &profile,
                terrain: &terrain,
            },
            retaining.then_some(&retained_paths),
            32_000_000,
            |p| {
                assert!(!empty, "Empty network requested native noise");
                Ok(Some(vec![0.; p.len()]))
            },
            |_| {
                assert!(!empty, "Empty network queried water");
                Ok(Some(terrain.water_no_hit_level))
            },
            || false,
        )
        .unwrap()
        .unwrap();
        let painted_layers = Layers {
            height: painted.height.clone(),
            ..layers.clone()
        };
        if retaining {
            assert_ne!(
                painted.height, layers.height,
                "Retained pass must actually alter the source height baseline"
            );
        }
        let retained_height = if retaining {
            Some(
                retained_paths
                    .height_support(
                        &painted_layers,
                        crate::road_surface_stage::HeightGeometry {
                            origin_y: profile.height_origin,
                            vertical_span: profile.vertical_span,
                        },
                        32_000_000,
                        || false,
                    )
                    .unwrap()
                    .unwrap(),
            )
        } else {
            None
        };
        let (parent, doc) = crate::road_surface_source::tests::fixture();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("meshes.json");
        std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
        let source = crate::road_surface_source::Source::read(&path, &parent, &|| false).unwrap();
        let project = crate::Project {
            size: layers.size,
            roads: Some(crate::infrastructure::Recipe {
                engine: papyrus_terrain::road_network::SHAPED_VERSION.into(),
                surface_sha256: Some(source.sha256.clone()),
                ..crate::infrastructure::Recipe::new(parent.sha256.clone())
            }),
            ..crate::Project::default()
        };
        // Use the fixture's seed identities for source validation.
        let mut project = project;
        project.seeds[0].value = layers.terrain_seed;
        project.seeds[1].value = layers.biome_seed;
        let mut network = Report {
            roads: vec![Road {
                from: Destination::Network { road: 0, point: 0 },
                to: Destination::Network {
                    road: 0,
                    point: paths[1].points().len() - 1,
                },
                points: paths[1].points().to_vec(),
            }],
            families: vec![Family {
                infrastructure_type: 2,
                hierarchy: 2,
                seed: 1,
                first_road: 0,
                road_count: 1,
                socket_count: 0,
                unresolved: vec![],
                width: 4.,
                splat_channel: 0,
            }],
            ..Default::default()
        };
        if empty {
            network.roads.clear();
            network.families.clear();
            assert_eq!(painted.height, layers.height);
            assert_eq!(painted.splat, layers.splat);
            assert!(painted.report.marking.roadside_added > 0);
        }
        let refined = crate::road_refinement::refine_painted_with_source(
            crate::road_surface_stage::Context {
                project: &project,
                root: directory.path(),
                worker: &directory.path().join("must-not-start.exe"),
                layers: &layers,
            },
            &network,
            &source,
            crate::road_refinement::PaintedSnapshot {
                source_rounding: None,
                retained_height: retained_height.as_ref(),
                height: &painted.height,
                topology: &painted.topology,
                surface_resolution: layers.surface_resolution,
                paths: &painted.roads,
                paint: &profile,
                terrain: &terrain,
                allowed_topology: terrain.policy.water_topology,
                maximum_initial_work: 32_000_000,
            },
            crate::road_refinement::Settings {
                fit: papyrus_terrain::road_bed::Settings {
                    maximum_grade: 0.2,
                    maximum_cut_fill: 8.,
                    minimum_up_normal: 0.5,
                    maximum_iterations: 512,
                    maximum_work: 32_000_000,
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
                maximum_total_work: 128_000_000,
                maximum_reprojection_distance: 0.25,
            },
            |_| {},
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(!refined.report.paved_clearance_applicable);
        assert!(refined.report.initial_bed.is_some());
        if let Some(retained) = &retained_height {
            assert_eq!(
                refined
                    .report
                    .initial_bed
                    .as_ref()
                    .unwrap()
                    .retained_sample_entries,
                Some(retained.samples().len())
            );
            assert_eq!(
                refined.report.retained_sample_entries,
                Some(retained.samples().len())
            );
            for &i in retained.samples() {
                assert_eq!(refined.height[i], painted.height[i]);
            }
        }

        assert!(refined.clearance.report["passed"].is_null());
        assert!(refined.geometry.surfaces.chunks.is_empty());
        let corrected = refined
            .controls
            .iter()
            .map(|p| Path::from_controls(p, 10000, || false).unwrap().unwrap())
            .collect::<Vec<_>>();
        let final_roads = corrected
            .iter()
            .map(|path| road_paint::Road { path, hierarchy: 2 })
            .collect::<Vec<_>>();
        let base = papyrus_terrain::monument_composition::compose_with_water(
            &layers,
            &vec![255; layers.surface_resolution * layers.surface_resolution],
            &[],
            || false,
        )
        .unwrap()
        .unwrap();
        let finished = finish_surfaces(
            &base,
            &refined.height,
            &final_roads,
            &profile,
            32_000_000,
            |p| {
                assert!(!empty, "Empty final repaint requested noise");
                Ok(Some(vec![0.; p.len()]))
            },
            || false,
        )
        .unwrap()
        .unwrap();
        let n = layers.surface_resolution;
        let i = (600 * n / 1000) * n + n / 2;
        if empty {
            assert_eq!(refined.height, layers.height);
            assert_eq!(finished.splat, layers.splat);
            assert_eq!(finished.topology, painted.topology);
            assert!(finished.report.marking.roadside_added > 0);
            assert_eq!(
                refined.report.initial_bed.as_ref().unwrap().changed_samples,
                0
            );
        } else {
            assert_eq!(
                finished.splat[i], 255,
                "Dirt must remain visible after final repaint"
            );
            assert_ne!(finished.topology[i] & 2048, 0);
        }
        if retaining {
            let final_layers = Layers {
                height: refined.height.clone(),
                splat: finished.splat.clone(),
                topology: finished.topology.clone(),
                ..layers.clone()
            };
            let late = crate::rail_texture_stage::paint(
                &rail_source,
                &rail_parent,
                &final_layers,
                &rail_paths,
                crate::road_surface_stage::HeightGeometry {
                    origin_y: profile.height_origin,
                    vertical_span: profile.vertical_span,
                },
                32_000_000,
                || false,
            )
            .unwrap()
            .unwrap();
            assert!(late.report.texture.changed_cells > 0);
            assert_eq!(final_layers.height, refined.height);
            assert_eq!(
                late.splat[i], 255,
                "Dirt away from rail strips must survive late rail texture"
            );
        }
        assert!(layers.height.iter().all(|&h| h == 17039));
        assert_eq!(layers.topology, original_topology);
    }
    #[test]
    fn later_provider_failure_cancellation_and_invalid_joins_publish_no_layers() {
        let layers = layers();
        let (paint_profile, terrain) = profiles();
        let paths = paths(&layers, &paint_profile);
        let roads = [road_paint::Road {
            path: &paths[0],
            hierarchy: 1,
        }];
        let input = || Input {
            layers: &layers,
            roads: &roads,
            junctions: &[],
            paint: &paint_profile,
            terrain: &terrain,
        };
        let calls = Cell::new(0);
        let stop = Cell::new(false);
        let result = paint(
            input(),
            32_000_000,
            |p| {
                calls.set(calls.get() + 1);
                if calls.get() == 2 {
                    stop.set(true);
                }
                Ok(Some(vec![0.; p.len()]))
            },
            |_| Ok(Some(terrain.water_no_hit_level)),
            || stop.get(),
        )
        .unwrap();
        assert!(result.is_none() && calls.get() == 2);
        calls.set(0);
        let failed = paint(
            input(),
            32_000_000,
            |p| {
                calls.set(calls.get() + 1);
                if calls.get() == 2 {
                    Err("native provider unavailable".into())
                } else {
                    Ok(Some(vec![0.; p.len()]))
                }
            },
            |_| Ok(Some(terrain.water_no_hit_level)),
            || false,
        );
        assert!(matches!(failed,Err(e) if e=="native provider unavailable"));
        let mut bad = input();
        let joins = [Junction {
            a: (0, 0),
            b: (0, 99),
        }];
        bad.junctions = &joins;
        assert!(
            paint(
                bad,
                32_000_000,
                |_| panic!("invalid joins must fail before native work"),
                |_| panic!(),
                || false
            )
            .is_err()
        );
        assert!(
            paint(
                input(),
                1,
                |_| panic!("budget must fail before native work"),
                |_| panic!(),
                || false
            )
            .is_err()
        );
        assert!(layers.height.iter().all(|&h| h == 17039));
        assert!(layers.topology.iter().all(|&t| t == 1));
        assert_eq!(layers.splat, self::layers().splat);
    }
    #[test]
    fn retained_rail_sweeps_change_height_without_road_paint_or_control_mutation() {
        let layers = layers();
        let (paint_profile, terrain) = profiles();
        let (_root, parent, rail_source) = crate::retained_rail_height::tests::source_fixture();
        let rails = [Path::from_controls(
            &[[200., 30., 200.], [275., 30., 200.], [350., 30., 200.]],
            10000,
            || false,
        )
        .unwrap()
        .unwrap()];
        let retained = crate::retained_rail_height::RetainedRails::prepare(
            &rail_source,
            &parent,
            &rails,
            10000,
            || false,
        )
        .unwrap()
        .unwrap();
        let original = serde_json::to_vec(&rails).unwrap();
        let run = |retained| {
            paint_with_retained_rails(
                Input {
                    layers: &layers,
                    roads: &[],
                    junctions: &[],
                    paint: &paint_profile,
                    terrain: &terrain,
                },
                retained,
                32_000_000,
                |p| Ok(Some(vec![0.; p.len()])),
                |_| Ok(Some(terrain.water_no_hit_level)),
                || false,
            )
            .unwrap()
            .unwrap()
        };
        let without = run(None);
        let with = run(Some(&retained));
        assert_ne!(with.height, without.height);
        assert_eq!(with.topology, without.topology);
        assert_eq!(with.splat, without.splat);
        assert!(with.roads.is_empty());
        assert_eq!(serde_json::to_vec(&rails).unwrap(), original);
        for (pass, expected_fade) in with.report.height.loops.iter().zip([0.25, 0.125]) {
            assert_eq!(pass.rail_fade, expected_fade);
            assert!(pass.road_passes.is_empty());
            assert_eq!(pass.rail_passes.len(), 1);
            assert!(pass.rail_passes[0].sweep.covered_visits > 0);
        }
        assert_eq!(with.report.retained_rails.as_ref().unwrap().tracks, 1);
        assert_eq!(
            with.report.retained_rails.as_ref().unwrap().controls_sha256,
            retained.receipt().controls_sha256
        );
        assert!(
            serde_json::to_value(&without.report)
                .unwrap()
                .get("retained_rails")
                .is_none()
        );
        assert!(!with.report.runtime_access_validated);
        assert!(
            paint_with_retained_rails(
                Input {
                    layers: &layers,
                    roads: &[],
                    junctions: &[],
                    paint: &paint_profile,
                    terrain: &terrain
                },
                Some(&retained),
                1,
                |p| Ok(Some(vec![0.; p.len()])),
                |_| Ok(Some(0.)),
                || false
            )
            .is_err()
        );
        assert!(
            paint_with_retained_rails(
                Input {
                    layers: &layers,
                    roads: &[],
                    junctions: &[],
                    paint: &paint_profile,
                    terrain: &terrain
                },
                Some(&retained),
                32_000_000,
                |_| Ok(None),
                |_| Ok(Some(0.)),
                || false
            )
            .unwrap()
            .is_none()
        );
    }
    #[test]
    fn source_ordered_rail_topology_precedes_height_for_paved_and_dirt_roads() {
        let mut layers = layers();
        let (paint, terrain) = profiles();
        let hr = layers.height_resolution;
        for z in 0..hr {
            for x in 0..hr {
                let world = (x as f32 / (hr - 1) as f32 - 0.5) * 1000.;
                layers.height[z * hr + x] +=
                    ((30. - world.abs()).max(0.) * 32766. / 1000.).round() as i16;
            }
        }
        let original_height = layers.height.clone();
        let original_topology = layers.topology.clone();
        let original_splat = layers.splat.clone();
        let make = |z, y| {
            Path::from_controls(
                &[[-100., 20., z], [0., y, z], [100., 20., z]],
                10000,
                || false,
            )
            .unwrap()
            .unwrap()
        };
        let paths = [make(0., 50.), make(80., 50.)];
        let roads = [
            road_paint::Road {
                path: &paths[0],
                hierarchy: 1,
            },
            road_paint::Road {
                path: &paths[1],
                hierarchy: 2,
            },
        ];
        let rails = [Path::from_controls(
            &[[0., 50., -150.], [0., 50., 0.], [0., 50., 150.]],
            10000,
            || false,
        )
        .unwrap()
        .unwrap()];
        let original_rails = serde_json::to_value(&rails).unwrap();
        let (_root, parent, source) = crate::retained_rail_height::tests::source_fixture();
        let prepared = crate::rail_topology_stage::prepare(
            &source,
            &parent,
            &rails,
            layers.size,
            32_000_000,
            || false,
        )
        .unwrap()
        .unwrap();
        let input = || Input {
            layers: &layers,
            roads: &roads,
            junctions: &[],
            paint: &paint,
            terrain: &terrain,
        };
        let legacy = paint_with_retained_rails(
            input(),
            Some(prepared.retained()),
            32_000_000,
            |p| Ok(Some(vec![0.; p.len()])),
            |_| Ok(Some(terrain.water_no_hit_level)),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(legacy.report.engine, VERSION);
        assert!(
            serde_json::to_value(&legacy.report)
                .unwrap()
                .get("rail_topology")
                .is_none()
        );
        let expected_topology = prepared
            .apply(
                &layers,
                &legacy.topology,
                crate::road_surface_stage::HeightGeometry {
                    origin_y: paint.height_origin,
                    vertical_span: paint.vertical_span,
                },
                32_000_000,
                || false,
            )
            .unwrap()
            .unwrap();
        let height_paths = roads
            .iter()
            .map(|r| {
                let shape = paint
                    .policy
                    .families
                    .iter()
                    .find(|f| f.hierarchy == r.hierarchy)
                    .unwrap();
                let family = terrain
                    .families
                    .iter()
                    .find(|f| f.hierarchy == r.hierarchy)
                    .unwrap();
                road_terrain::InputPath {
                    path: r.path,
                    shape: path_terrain::Shape {
                        width: shape.width,
                        padding: shape.padding,
                        fade: family.fade,
                    },
                    terrain_offset: family.terrain_offset,
                    random_scale: shape.random_scale,
                }
            })
            .collect::<Vec<_>>();
        let expected_height = road_terrain::run(
            road_terrain::Input {
                grid: path_terrain::Grid {
                    size: layers.size as f32,
                    resolution: hr,
                    origin_y: paint.height_origin,
                    vertical_span: paint.vertical_span,
                    height: &layers.height,
                },
                topology: &expected_topology.topology,
                surface_resolution: layers.surface_resolution,
                roads: &height_paths,
                rails: prepared.retained().paths(),
            },
            &terrain.policy,
            32_000_000,
            |p| Ok(Some(vec![0.; p.len()])),
            |_| Ok(Some(terrain.water_no_hit_level)),
            || false,
        )
        .unwrap()
        .unwrap();
        let ordered = paint_with_rail_stages(
            input(),
            &prepared,
            32_000_000,
            |p| Ok(Some(vec![0.; p.len()])),
            |_| Ok(Some(terrain.water_no_hit_level)),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(ordered.report.engine, RAIL_STAGES_VERSION);
        assert_eq!(ordered.topology, expected_topology.topology);
        assert_eq!(ordered.height, expected_height.height);
        assert_eq!(
            serde_json::to_value(&ordered.roads).unwrap(),
            serde_json::to_value(&expected_height.roads).unwrap()
        );
        assert_ne!(
            ordered.height, legacy.height,
            "Fixture must distinguish topology before versus after earthwork"
        );
        assert_eq!(
            serde_json::to_value(ordered.report.rail_topology.as_ref().unwrap()).unwrap(),
            serde_json::to_value(&expected_topology.report).unwrap()
        );
        let geometry = path_surface::Grid {
            size: layers.size as f32,
            resolution: layers.surface_resolution,
            origin_y: paint.height_origin,
            vertical_span: paint.vertical_span,
        };
        let texture_paths = expected_height
            .roads
            .iter()
            .zip(&roads)
            .map(|(path, r)| road_paint::Road {
                path,
                hierarchy: r.hierarchy,
            })
            .collect::<Vec<_>>();
        let texture = road_paint::prepare(&texture_paths, &paint.policy, 32_000_000, || false)
            .unwrap()
            .unwrap();
        let noise = vec![0.; texture.noise_coordinates().len()];
        let texture = texture
            .apply(
                road_paint::Grid {
                    geometry,
                    splat: &layers.splat,
                    topology: &expected_topology.topology,
                },
                &noise,
                road_paint::Pass::Texture,
                || false,
            )
            .unwrap()
            .unwrap();
        assert_eq!(ordered.splat, texture.splat.unwrap());
        assert!(
            paint_with_rail_stages(
                input(),
                &prepared,
                1,
                |p| Ok(Some(vec![0.; p.len()])),
                |_| Ok(Some(terrain.water_no_hit_level)),
                || false
            )
            .is_err()
        );
        assert!(
            paint_with_rail_stages(
                input(),
                &prepared,
                32_000_000,
                |_| panic!("Cancelled pass requested noise"),
                |_| panic!("Cancelled pass queried water"),
                || true
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(layers.height, original_height);
        assert_eq!(layers.topology, original_topology);
        assert_eq!(layers.splat, original_splat);
        assert_eq!(serde_json::to_value(&rails).unwrap(), original_rails);
        assert!(!ordered.report.runtime_access_validated);
    }
    #[test]
    fn retained_rails_feed_final_road_heights_without_moving_rail_controls() {
        let layers = layers();
        let (profile, terrain) = profiles();
        let paths = paths(&layers, &profile);
        let roads = [road_paint::Road {
            path: &paths[1],
            hierarchy: 2,
        }];
        let (_root, parent, source) = crate::retained_rail_height::tests::source_fixture();
        let rails = [Path::from_controls(
            &[[-100., 30., 75.], [0., 30., 75.], [100., 30., 75.]],
            10000,
            || false,
        )
        .unwrap()
        .unwrap()];
        let original = serde_json::to_vec(&rails).unwrap();
        let retained = crate::retained_rail_height::RetainedRails::prepare(
            &source,
            &parent,
            &rails,
            10000,
            || false,
        )
        .unwrap()
        .unwrap();
        let run = |rails| {
            paint_with_retained_rails(
                Input {
                    layers: &layers,
                    roads: &roads,
                    junctions: &[],
                    paint: &profile,
                    terrain: &terrain,
                },
                rails,
                32_000_000,
                |p| Ok(Some(vec![0.; p.len()])),
                |_| Ok(Some(terrain.water_no_hit_level)),
                || false,
            )
            .unwrap()
            .unwrap()
        };
        let without = run(None);
        let with = run(Some(&retained));
        assert_ne!(with.height, without.height);
        assert_ne!(with.roads[0].points(), without.roads[0].points());
        let grid = path_terrain::Grid {
            size: layers.size as f32,
            resolution: layers.height_resolution,
            origin_y: profile.height_origin,
            vertical_span: profile.vertical_span,
            height: &with.height,
        };
        for (actual, original) in with.roads[0].points().iter().zip(paths[1].points()) {
            assert_eq!(
                [actual[0].to_bits(), actual[2].to_bits()],
                [original[0].to_bits(), original[2].to_bits()]
            );
            assert_eq!(
                actual[1].to_bits(),
                path_terrain::sample_height(grid, *actual)
                    .unwrap()
                    .to_bits()
            );
        }
        assert_eq!(serde_json::to_vec(&rails).unwrap(), original);
        for pass in &with.report.height.loops {
            assert_eq!(pass.road_passes.len(), 1);
            assert_eq!(pass.rail_passes.len(), 1);
        }
        assert_eq!(layers.height, self::layers().height);
        assert!(!with.report.runtime_access_validated);
    }
    #[test]
    fn final_surfaces_preserve_retained_rails_and_reject_revision_or_height_drift_before_painting()
    {
        use papyrus_terrain::monument_composition::Composed;
        let layers = layers();
        let (profile, _) = profiles();
        let paths = paths(&layers, &profile);
        let roads = [
            road_paint::Road {
                path: &paths[0],
                hierarchy: 1,
            },
            road_paint::Road {
                path: &paths[1],
                hierarchy: 2,
            },
        ];
        let n = layers.surface_resolution;
        let mut base = papyrus_terrain::monument_composition::compose_with_water(
            &layers,
            &vec![255; n * n],
            &[],
            || false,
        )
        .unwrap()
        .unwrap();
        let noise_calls = Cell::new(0);
        let run = |base: &Composed,
                   height: &[i16],
                   retained: Option<RetainedSurfaceContext<'_>>,
                   budget| {
            finish_surfaces_with_context(
                FinalSurfaceInput {
                    base,
                    height,
                    roads: &roads,
                    profile: &profile,
                    retained,
                },
                budget,
                |p| {
                    noise_calls.set(noise_calls.get() + 1);
                    Ok(Some(vec![0.; p.len()]))
                },
                || false,
            )
        };
        let legacy = run(&base, &layers.height, None, 32_000_000)
            .unwrap()
            .unwrap();
        let legacy_report = serde_json::to_value(&legacy.report).unwrap();
        assert!(legacy_report.get("retained_rails").is_none());
        assert!(legacy_report.get("retained_height_checks").is_none());
        let (recipe, scene) = crate::retained_rail_scene::tests::fixture(n);
        let receipt = crate::retained_rail_scene::Receipt::capture(&scene, &recipe, n, || false)
            .unwrap()
            .unwrap();
        base.rails = Some(scene);
        let held = crate::retained_rail_height::RetainedHeight::test_capture(
            path_terrain::Grid {
                size: layers.size as f32,
                resolution: layers.height_resolution,
                origin_y: profile.height_origin,
                vertical_span: profile.vertical_span,
                height: &layers.height,
            },
            &[0],
        );
        let context = RetainedSurfaceContext {
            recipe: &recipe,
            scene_receipt: &receipt,
            height: &held,
            rail_stages: None,
        };
        let output = run(&base, &layers.height, Some(context), 32_000_000)
            .unwrap()
            .unwrap();
        assert_eq!(output.topology, legacy.topology);
        assert_eq!(output.splat, legacy.splat);
        assert_eq!(output.report.retained_rails.as_ref(), Some(&receipt));
        assert_eq!(output.report.retained_height_checks, Some(1));
        assert_eq!(
            crate::retained_rail_scene::Receipt::capture(
                base.rails.as_ref().unwrap(),
                &recipe,
                n,
                || false,
            )
            .unwrap()
            .unwrap(),
            receipt
        );
        noise_calls.set(0);
        assert!(run(&base, &layers.height, None, 32_000_000).is_err());
        let mut changed_height = layers.height.clone();
        changed_height[0] += 1;
        assert!(run(&base, &changed_height, Some(context), 32_000_000).is_err());
        assert!(run(&base, &layers.height, Some(context), 1).is_err());
        let mut changed_recipe = recipe.clone();
        changed_recipe.settings["ring"]["seed"] = 42.into();
        assert!(
            run(
                &base,
                &layers.height,
                Some(RetainedSurfaceContext {
                    recipe: &changed_recipe,
                    ..context
                }),
                32_000_000
            )
            .is_err()
        );
        base.rails.as_mut().unwrap().reservations.raster[0] = 1;
        assert!(run(&base, &layers.height, Some(context), 32_000_000).is_err());
        base.rails = None;
        assert!(run(&base, &layers.height, Some(context), 32_000_000).is_err());
        assert_eq!(noise_calls.get(), 0);
        assert!(
            finish_surfaces_with_context(
                FinalSurfaceInput {
                    base: &base,
                    height: &layers.height,
                    roads: &roads,
                    profile: &profile,
                    retained: Some(context),
                },
                32_000_000,
                |_| panic!("Cancelled pass requested noise"),
                || true
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(base.layers.splat, layers.splat);
        assert_eq!(base.layers.height, layers.height);
    }
    #[test]
    fn final_rail_surface_repaint_starts_from_pristine_maps_and_checks_curve_identity() {
        let layers = layers();
        let (profile, _) = profiles();
        let paths = paths(&layers, &profile);
        let roads = [
            road_paint::Road {
                path: &paths[0],
                hierarchy: 1,
            },
            road_paint::Road {
                path: &paths[1],
                hierarchy: 2,
            },
        ];
        let n = layers.surface_resolution;
        let (_root, parent, source) = crate::retained_rail_height::tests::source_fixture();
        let (mut recipe, mut scene) = crate::retained_rail_scene::tests::fixture(n);
        recipe.source_sha256 = source.sha256.clone();
        scene.report["source_sha256"] = source.sha256.clone().into();
        let rails = scene
            .tracks
            .iter()
            .map(|t| t.path.clone())
            .collect::<Vec<_>>();
        let prepared = crate::rail_topology_stage::prepare(
            &source,
            &parent,
            &rails,
            layers.size,
            32_000_000,
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(
            prepared
                .check_scene(&scene, &recipe, 32_000_000, || false)
                .is_err()
        );
        scene.report["changes"]["surface_paint_deferred"] = true.into();
        let receipt = crate::retained_rail_scene::Receipt::capture(&scene, &recipe, n, || false)
            .unwrap()
            .unwrap();
        let mut base =
            papyrus_terrain::monument_composition::compose(&layers, &vec![255; n * n], &[], || false)
                .unwrap()
                .unwrap();
        base.rails = Some(scene);
        let held = crate::retained_rail_height::RetainedHeight::test_capture(
            path_terrain::Grid {
                size: layers.size as f32,
                resolution: layers.height_resolution,
                origin_y: profile.height_origin,
                vertical_span: profile.vertical_span,
                height: &layers.height,
            },
            &[0],
        );
        let context = RetainedSurfaceContext {
            recipe: &recipe,
            scene_receipt: &receipt,
            height: &held,
            rail_stages: None,
        };
        let run = |context| {
            finish_surfaces_with_context(
                FinalSurfaceInput {
                    base: &base,
                    height: &layers.height,
                    roads: &roads,
                    profile: &profile,
                    retained: Some(context),
                },
                32_000_000,
                |p| Ok(Some(vec![0.; p.len()])),
                || false,
            )
            .unwrap()
            .unwrap()
        };
        let road_only = run(context);
        let context = RetainedSurfaceContext {
            rail_stages: Some(&prepared),
            ..context
        };
        let output = run(context);
        let repeated = run(context);
        let expected = prepared
            .apply(
                &layers,
                &road_only.topology,
                crate::road_surface_stage::HeightGeometry {
                    origin_y: profile.height_origin,
                    vertical_span: profile.vertical_span,
                },
                32_000_000,
                || false,
            )
            .unwrap()
            .unwrap();
        assert_eq!(output.topology, expected.topology);
        assert_ne!(output.topology, road_only.topology);
        assert_eq!(output.topology, repeated.topology);
        assert_eq!(output.splat, road_only.splat);
        assert_eq!(output.splat, repeated.splat);
        assert_eq!(output.report.engine, "papyrus-final-road-rail-surfaces-1");
        assert_eq!(
            output.report.retained_curve_checks,
            Some(1 + 6 * rails[0].points().len())
        );
        assert_eq!(
            serde_json::to_value(output.report.rail_topology.as_ref().unwrap()).unwrap(),
            serde_json::to_value(&expected.report).unwrap()
        );
        let other = [Path::from_controls(
            &[[-100., 20., 10.], [0., 20., 10.], [100., 20., 10.]],
            10000,
            || false,
        )
        .unwrap()
        .unwrap()];
        let mismatched = crate::rail_topology_stage::prepare(
            &source,
            &parent,
            &other,
            layers.size,
            32_000_000,
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(
            finish_surfaces_with_context(
                FinalSurfaceInput {
                    base: &base,
                    height: &layers.height,
                    roads: &roads,
                    profile: &profile,
                    retained: Some(RetainedSurfaceContext {
                        rail_stages: Some(&mismatched),
                        ..context
                    }),
                },
                32_000_000,
                |_| panic!("Mismatched rail curves requested noise"),
                || false
            )
            .is_err()
        );
        assert!(
            finish_surfaces_with_context(
                FinalSurfaceInput {
                    base: &base,
                    height: &layers.height,
                    roads: &roads,
                    profile: &profile,
                    retained: Some(context),
                },
                output.report.retained_curve_checks.unwrap(),
                |_| panic!("Exhausted binding grant requested noise"),
                || false
            )
            .is_err()
        );
        assert_eq!(base.layers.topology, layers.topology);
        assert_eq!(base.layers.splat, layers.splat);
        assert_eq!(base.layers.height, layers.height);
        assert_eq!(
            crate::retained_rail_scene::Receipt::capture(
                base.rails.as_ref().unwrap(),
                &recipe,
                n,
                || false
            )
            .unwrap()
            .unwrap(),
            receipt
        );
    }
    #[test]
    fn final_surfaces_repaint_both_families_keep_local_water_and_reject_stale_paths_atomically() {
        let layers = layers();
        let (profile, _) = profiles();
        let paths = paths(&layers, &profile);
        let roads = [
            road_paint::Road {
                path: &paths[0],
                hierarchy: 1,
            },
            road_paint::Road {
                path: &paths[1],
                hierarchy: 2,
            },
        ];
        let n = layers.surface_resolution;
        let mut base = papyrus_terrain::monument_composition::compose_with_water(
            &layers,
            &vec![255; n * n],
            &[],
            || false,
        )
        .unwrap()
        .unwrap();
        base.ocean.as_mut().unwrap().local_water[0] = 18000;
        let original = base.layers.splat.clone();
        let output = finish_surfaces(
            &base,
            &layers.height,
            &roads,
            &profile,
            32_000_000,
            |p| Ok(Some(vec![0.; p.len()])),
            || false,
        )
        .unwrap()
        .unwrap();
        let sample = |x: f32, z: f32| {
            ((z + 500.) / 1000. * n as f32) as usize * n + ((x + 500.) / 1000. * n as f32) as usize
        };
        assert_eq!(output.splat[7 * n * n + sample(60., 0.)], 255);
        assert_eq!(output.splat[sample(0., 100.)], 255);
        assert_eq!(output.ocean.as_ref().unwrap().local_water[0], 18000);
        assert_eq!(output.report.controls_checked_against_final_height, 6);
        assert!(output.report.ocean_reclassified && output.report.local_water_preserved);
        assert!(!output.report.runtime_access_validated);
        assert_eq!(base.layers.splat, original);
        assert!(base.roads.is_none());
        let mut stale = layers.height.clone();
        let hr = layers.height_resolution;
        stale[(hr / 2) * hr + hr / 2] += 100;
        assert!(
            finish_surfaces(
                &base,
                &stale,
                &roads,
                &profile,
                32_000_000,
                |_| panic!("stale controls must fail before noise"),
                || false
            )
            .is_err()
        );
        let stopped = Cell::new(false);
        assert!(
            finish_surfaces(
                &base,
                &layers.height,
                &roads,
                &profile,
                32_000_000,
                |p| {
                    stopped.set(true);
                    Ok(Some(vec![0.; p.len()]))
                },
                || stopped.get()
            )
            .unwrap()
            .is_none()
        );
        assert!(
            finish_surfaces(
                &base,
                &layers.height,
                &roads,
                &profile,
                32_000_000,
                |_| Err("noise failure".into()),
                || false
            )
            .is_err()
        );
        assert_eq!(base.layers.splat, original);
        assert_eq!(base.ocean.as_ref().unwrap().local_water[0], 18000);
        base.spawn_audit = None;
        base.topology_before_overrides = Some(base.layers.topology.clone());
        assert!(
            finish_surfaces(
                &base,
                &layers.height,
                &roads,
                &profile,
                32_000_000,
                |_| panic!(),
                || false
            )
            .is_err()
        );
    }
}
