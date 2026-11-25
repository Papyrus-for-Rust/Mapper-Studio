//! Private fit/rebuild transaction shared by native GUI and CLI workers.
//! Source receipt/recipe binding belongs to the caller; no map is mutated here.
use crate::road_surface_stage::{self, ClearanceAudit, TracedOutput};
use papyrus_terrain::{
    path_mesh::Template,
    path_terrain::{self, Grid, Layout},
    road_bed::{self, ClearanceProbe},
    road_clearance,
    road_grade::Junction,
    road_mesh_audit,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
pub const VERSION: &str = "papyrus-road-refinement-2";
// A late-stage work-selection heuristic, not an acceptance limit. Broad
// terrain shaping uses the original projection before integer recovery.
const QUANTIZED_RECOVERY_MAX_GRADE_FACES: usize = 128;
pub const QUANTIZED_VERSION: &str = "papyrus-road-refinement-quantized-6";
pub const LOCAL_VERSION: &str = "papyrus-road-refinement-local-2";
pub struct Input<'a> {
    pub retained_height: Option<&'a crate::retained_rail_height::RetainedHeight>,
    pub layout: Layout,
    pub initial_height: &'a [i16],
    pub ground: &'a [i16],
    pub source_painted: &'a [i16],
    pub source_rounding: Option<&'a papyrus_terrain::road_terrain::SourceCutRounding>,
    pub topology: &'a [i32],
    pub surface_resolution: usize,
    pub protected_topology: i32,
    pub allowed_topology: i32,
    pub controls: &'a [Vec<[f32; 3]>],
    pub junctions: &'a [Junction],
    pub template: &'a Template,
}
#[derive(Clone, Copy)]
pub struct Settings {
    pub fit: road_bed::Settings,
    pub clearance: road_clearance::Settings,
    pub maximum_rounds: usize,
    pub maximum_total_work: u64,
    pub maximum_reprojection_distance: f64,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct AlignmentWitness {
    pub kind: AlignmentKind,
    pub road: usize,
    pub position: [f64; 2],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlignmentKind {
    Grade,
    Clearance,
    Curvature,
}

fn alignment_witness(
    error: &road_bed::Failure,
    collisions: &[road_bed::Collision<'_>],
) -> Option<AlignmentWitness> {
    let (road, chunk, triangle, exact_position) = match error {
        road_bed::Failure::Infeasible {
            road,
            chunk,
            triangle,
            ..
        }
        | road_bed::Failure::HorizontalGeometry {
            road,
            chunk,
            triangle,
            ..
        } => (road, chunk, triangle, None),
        road_bed::Failure::ClearanceInfeasible {
            road,
            chunk,
            triangle,
            position,
            ..
        } => (road, chunk, triangle, Some(*position)),
        _ => return None,
    };
    let geometry = collisions
        .iter()
        .find(|c| c.road == *road && c.chunk_index == *chunk)?
        .geometry;
    let face = geometry.mesh.triangles.get(*triangle)?;
    if let Some(position) = exact_position {
        return position
            .iter()
            .all(|v| v.is_finite())
            .then_some(AlignmentWitness {
                kind: AlignmentKind::Clearance,
                road: *road,
                position,
            });
    }
    let mut position = [0.; 2];
    for &index in face {
        let point = geometry.mesh.vertices.get(index as usize)?;
        for (component, axis) in [0, 2].into_iter().enumerate() {
            position[component] += (f64::from(point[axis]) + f64::from(geometry.origin[axis])) / 3.;
        }
    }
    position
        .iter()
        .all(|v| v.is_finite())
        .then_some(AlignmentWitness {
            kind: crate::road_refinement::AlignmentKind::Grade,
            road: *road,
            position,
        })
}
#[derive(Debug)]
pub enum Failure {
    Invalid(&'static str),
    Budget,
    BudgetAtRebuild {
        rounds: Vec<Round>,
    },
    Rebuild(String),
    InitialControlProposal {
        conflict: papyrus_terrain::road_grade::ControlConflict,
        initial_work_limit: usize,
    },
    Fit(road_bed::Failure),
    FittingRound {
        round: usize,
        work_before_fit: u64,
        fit_work_limit: usize,
        progress: Option<Box<road_bed::Progress>>,
        witness: Option<Box<AlignmentWitness>>,
        ranking_revision: Option<Box<crate::road_portal_revision::Revision>>,
        error: Box<road_bed::Failure>,
    },
    NoPavedGeometry,
    Cycle,
    CycleAtRebuild {
        work: u64,
        rounds: Vec<Round>,
        last_fit: Box<road_bed::Report>,
    },
    RoundLimit {
        work: u64,
        rounds: Vec<Round>,
    },
}
impl Failure {
    /// Charge an unsuccessful attempt against its original total grant. Early
    /// clearance rejection has an exact meter; other fit failures conservatively
    /// consume their entire fit grant. Unmetered failures consume the attempt.
    pub fn charged_work(&self, attempt_grant: u64) -> u64 {
        match self {
            Self::InitialControlProposal {
                initial_work_limit, ..
            } => *initial_work_limit as u64,
            Self::FittingRound {
                work_before_fit,
                fit_work_limit,
                error,
                ..
            } => {
                let fit_work = match error.as_ref() {
                    road_bed::Failure::ClearanceInfeasible { work, .. } => *work,
                    _ => *fit_work_limit,
                };
                work_before_fit.saturating_add(fit_work as u64)
            }
            Self::RoundLimit { work, .. } | Self::CycleAtRebuild { work, .. } => *work,
            _ => attempt_grant,
        }
        .min(attempt_grant)
    }

    fn with_initial_work(mut self, work: u64) -> Self {
        match &mut self {
            Self::FittingRound {
                work_before_fit, ..
            } => {
                *work_before_fit = work_before_fit.saturating_add(work);
            }
            Self::RoundLimit { work: total, .. } | Self::CycleAtRebuild { work: total, .. } => {
                *total = total.saturating_add(work)
            }
            _ => {}
        }
        self
    }
    fn bind_ranking_original(mut self, roads: &papyrus_terrain::road_network::Report) -> Self {
        if let Self::FittingRound {
            ranking_revision: Some(revision),
            ..
        } = &mut self
        {
            revision.bind_original(roads);
        }
        self
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Round {
    pub index: usize,
    pub grade_violations: usize,
    pub maximum_mesh_grade: f64,
    pub clearance_intersections: usize,
    pub source_intersections: usize,
    pub maximum_control_grade: f64,
    pub witnesses: usize,
    pub dropped_history: usize,
}
#[derive(Serialize)]
pub struct Report {
    pub engine: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retained_sample_entries: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_bed: Option<papyrus_terrain::road_grade_terrain::Report>,
    pub rounds: Vec<Round>,
    pub fits: Vec<road_bed::Report>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub local_attempts: Vec<LocalAttempt>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub clearance_headroom_retries: Vec<ClearanceHeadroomRetry>,
    pub work: u64,
    #[serde(skip_serializing_if = "is_false")]
    pub quantized_headroom_recovery: bool,
    pub numerical_model_passed: bool,
    pub paved_clearance_applicable: bool,
    pub runtime_physics_validated: bool,
    pub water_clearance_validated: bool,
}
#[derive(Debug, Serialize)]
pub struct LocalAttempt {
    pub round: usize,
    pub samples: usize,
    pub work: usize,
    pub adopted: bool,
    pub reason: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct ClearanceHeadroomRetry {
    pub round: usize,
    pub rejected_limit: f64,
    pub model_minimum: f64,
    pub provisional_limit: f64,
    pub rejected_work: usize,
    pub work_before_retry: u64,
}
pub struct Output {
    pub height: Vec<i16>,
    pub controls: Vec<Vec<[f32; 3]>>,
    pub geometry: TracedOutput,
    pub clearance: ClearanceAudit,
    pub report: Report,
}
/// Snapshots captured by the installation-bound road-paint stage. Binding them
/// to the active installation/recipe remains the caller's responsibility.
pub struct Snapshots<'a> {
    pub retained_height: Option<&'a crate::retained_rail_height::RetainedHeight>,
    pub geometry: road_surface_stage::HeightGeometry,
    pub ground: &'a [i16],
    pub source_painted: &'a [i16],
    pub source_rounding: Option<&'a papyrus_terrain::road_terrain::SourceCutRounding>,
    pub topology: &'a [i32],
    pub surface_resolution: usize,
    pub protected_topology: i32,
    pub allowed_topology: i32,
}
/// Raw output from the source road-height pass, before bounded bed correction.
/// The caller binds this snapshot and the stamped Context layers to its plan.
pub struct PaintedSnapshot<'a> {
    pub retained_height: Option<&'a crate::retained_rail_height::RetainedHeight>,
    pub height: &'a [i16],
    pub source_rounding: Option<&'a papyrus_terrain::road_terrain::SourceCutRounding>,
    pub topology: &'a [i32],
    pub surface_resolution: usize,
    pub paths: &'a [papyrus_terrain::path_geometry::Path],
    pub paint: &'a crate::road_paint_source::Profile,
    pub terrain: &'a crate::road_terrain_source::Profile,
    pub allowed_topology: i32,
    pub maximum_initial_work: usize,
}
fn junctions(
    roads: &papyrus_terrain::road_network::Report,
    counts: &[usize],
) -> Result<Vec<Junction>, Failure> {
    if roads.roads.len() != counts.len() || counts.iter().any(|&n| n < 2) {
        return Err(Failure::Invalid("road coverage"));
    }
    let mut joins = Vec::new();
    for (i, road) in roads.roads.iter().enumerate() {
        for (side, dest) in [(0, road.from), (1, road.to)] {
            if matches!(dest, papyrus_terrain::road_network::Destination::RawNetwork(_)) {
                return Err(Failure::Invalid(
                    "strict bed fitting cannot reinterpret raw source joins",
                ));
            }
            if let papyrus_terrain::road_network::Destination::Network { road, point } = dest {
                if counts.get(road).is_none_or(|&n| point >= n) {
                    return Err(Failure::Invalid("network attachment"));
                }
                joins.push(Junction {
                    a: (i, if side == 0 { 0 } else { counts[i] - 1 }),
                    b: (road, point),
                });
            }
        }
    }
    Ok(joins)
}
fn protection(snapshot: &PaintedSnapshot<'_>) -> i32 {
    snapshot
        .terrain
        .policy
        .smoothing_weights
        .iter()
        .filter(|w| w.value == 0.)
        .fold(snapshot.terrain.policy.water_topology, |mask, w| {
            mask | w.topology_mask
        })
}
/// Initial bound repair shared by GUI/CLI callers. Uses source family radii and
/// shoulders; raw painting is not accepted as a bounded road bed by itself.
pub fn prepare_painted_bed(
    layers: &papyrus_terrain::native_layers::Layers,
    roads: &papyrus_terrain::road_network::Report,
    snapshot: &PaintedSnapshot<'_>,
    settings: road_bed::Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<papyrus_terrain::road_grade_terrain::Output>, Failure> {
    prepare_painted_bed_with_junction_mode(
        layers,
        roads,
        snapshot,
        settings,
        papyrus_terrain::road_grade::JunctionMode::Pinned,
        cancelled,
    )
}
pub fn prepare_painted_bed_with_junction_mode(
    layers: &papyrus_terrain::native_layers::Layers,
    roads: &papyrus_terrain::road_network::Report,
    snapshot: &PaintedSnapshot<'_>,
    settings: road_bed::Settings,
    mode: papyrus_terrain::road_grade::JunctionMode,
    cancelled: impl Fn() -> bool,
) -> Result<Option<papyrus_terrain::road_grade_terrain::Output>, Failure> {
    if cancelled() {
        return Ok(None);
    }
    settings.validate().map_err(Failure::Fit)?;
    let expected = crate::road_terrain_source::Profile::for_paint(snapshot.paint)
        .map_err(Failure::Rebuild)?
        .ok_or(Failure::Invalid("missing bound terrain profile"))?;
    if expected.sha256().map_err(Failure::Rebuild)?
        != snapshot.terrain.sha256().map_err(Failure::Rebuild)?
        || snapshot.surface_resolution != layers.surface_resolution
        || snapshot.paths.len() != roads.roads.len()
        || snapshot.allowed_topology <= 0
        || snapshot.allowed_topology & !protection(snapshot) != 0
        || roads.families.len() > 16
    {
        return Err(Failure::Invalid("painted road binding"));
    }
    let counts = snapshot
        .paths
        .iter()
        .map(|p| p.points().len())
        .collect::<Vec<_>>();
    let joins = junctions(roads, &counts)?;
    let mut shaped = Vec::new();
    for (i, path) in snapshot.paths.iter().enumerate() {
        let mut families = roads
            .families
            .iter()
            .filter(|f| i >= f.first_road && i < f.first_road.saturating_add(f.road_count));
        let family = families
            .next()
            .ok_or(Failure::Invalid("missing road family"))?;
        if families.next().is_some() {
            return Err(Failure::Invalid("overlapping road families"));
        }
        let shape = snapshot
            .paint
            .policy
            .families
            .iter()
            .find(|s| u32::from(s.hierarchy) == family.hierarchy)
            .ok_or(Failure::Invalid("missing road paint shape"))?;
        let modifier = snapshot
            .terrain
            .families
            .iter()
            .find(|s| u32::from(s.hierarchy) == family.hierarchy)
            .ok_or(Failure::Invalid("missing road terrain shape"))?;
        if family.width.to_bits() != shape.width.to_bits() {
            return Err(Failure::Invalid("road family width differs"));
        }
        shaped.push(papyrus_terrain::road_grade_terrain::Road {
            path,
            radius: shape.width * 0.5 * shape.random_scale,
            shoulder: shape.padding[1] + modifier.fade[1],
        });
    }
    let input = papyrus_terrain::road_grade_terrain::Input {
        grid: Grid {
            size: layers.size as f32,
            resolution: layers.height_resolution,
            origin_y: snapshot.paint.height_origin,
            vertical_span: snapshot.paint.vertical_span,
            height: snapshot.height,
        },
        ground: &layers.height,
        topology: snapshot.topology,
        surface_resolution: snapshot.surface_resolution,
        protected_topology: protection(snapshot),
        roads: &shaped,
        junctions: &joins,
    };
    #[cfg(feature = "road-diagnostics")]
    if !crate::road_fit_diagnostics::capture_initial_grade_if_requested(
        input,
        settings,
        snapshot.maximum_initial_work,
        mode == papyrus_terrain::road_grade::JunctionMode::CoupledNetwork,
        snapshot.retained_height.map(|guard| guard.samples()),
        &cancelled,
    )
    .map_err(Failure::Rebuild)?
    {
        return Ok(None);
    }
    if let Some(retained) = snapshot.retained_height {
        return match mode {
            papyrus_terrain::road_grade::JunctionMode::Pinned => retained.fit_pinned_initial_bed(
                input,
                settings.maximum_grade,
                settings.maximum_cut_fill,
                snapshot.maximum_initial_work,
                &cancelled,
            ),
            papyrus_terrain::road_grade::JunctionMode::CoupledNetwork => retained.fit_initial_bed(
                input,
                settings.maximum_grade,
                settings.maximum_cut_fill,
                snapshot.maximum_initial_work,
                &cancelled,
            ),
        }
        .map_err(Failure::Rebuild);
    }
    let result = match mode {
        papyrus_terrain::road_grade::JunctionMode::Pinned => papyrus_terrain::road_grade_terrain::correct(
            input,
            settings.maximum_grade,
            settings.maximum_cut_fill,
            snapshot.maximum_initial_work,
            &cancelled,
        ),
        papyrus_terrain::road_grade::JunctionMode::CoupledNetwork => {
            papyrus_terrain::road_grade_terrain::correct_coupled_junctions(
                input,
                settings.maximum_grade,
                settings.maximum_cut_fill,
                snapshot.maximum_initial_work,
                &cancelled,
            )
        }
    };
    match result {
        Ok(output) => Ok((!cancelled()).then_some(output)),
        Err(papyrus_terrain::road_grade_terrain::Failure::Cancelled) => Ok(None),
        Err(papyrus_terrain::road_grade_terrain::Failure::ControlProposal(conflict)) => {
            Err(Failure::InitialControlProposal {
                conflict,
                initial_work_limit: snapshot.maximum_initial_work,
            })
        }
        Err(error) => Err(Failure::Rebuild(format!("Initial road bed: {error:?}"))),
    }
}
/// End-to-end numerical bed transaction from source-painted buffers. The
/// stamped layers, raw paint and original report remain immutable on failure.
pub fn refine_painted_with_source(
    context: road_surface_stage::Context<'_>,
    roads: &papyrus_terrain::road_network::Report,
    source: &crate::road_surface_source::Source,
    mut snapshot: PaintedSnapshot<'_>,
    settings: Settings,
    progress: impl FnMut(&Round),
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, Failure> {
    if cancelled() {
        return Ok(None);
    }
    if !(1..=64_000_000_000).contains(&settings.maximum_total_work) {
        return Err(Failure::Invalid("transaction work budget"));
    }
    if context.project.rails.is_some() && snapshot.retained_height.is_none() {
        return Err(Failure::Invalid(
            "painted road adapter needs retained rail-height inputs",
        ));
    }
    #[cfg(feature = "road-diagnostics")]
    if snapshot.retained_height.is_some()
        && [
            "MAPPER_ROAD_INITIAL_GRADE_CAPTURE",
            "MAPPER_ROAD_FIT_CAPTURE",
            "MAPPER_ROAD_TRANSACTION_CAPTURES",
        ]
        .iter()
        .any(|name| std::env::var_os(name).is_some())
    {
        return Err(Failure::Invalid(
            "painted road capture cannot omit retained height constraints",
        ));
    }
    let recipe = context
        .project
        .roads
        .as_ref()
        .ok_or(Failure::Invalid("missing road recipe"))?;
    recipe.validate().map_err(Failure::Rebuild)?;
    if settings.fit.maximum_grade != recipe.maximum_grade
        || settings.fit.maximum_cut_fill != recipe.maximum_cut_fill
    {
        return Err(Failure::Invalid("saved road bounds differ"));
    }
    snapshot.maximum_initial_work = snapshot
        .maximum_initial_work
        .min(settings.maximum_total_work.min(usize::MAX as u64) as usize);
    let mode = if recipe
        .entrance_launch
        .is_some_and(|p| p.engine == papyrus_terrain::road_network::LaunchEngine::GlobalEarthwork)
    {
        papyrus_terrain::road_grade::JunctionMode::CoupledNetwork
    } else {
        papyrus_terrain::road_grade::JunctionMode::Pinned
    };
    let Some(initial) = prepare_painted_bed_with_junction_mode(
        context.layers,
        roads,
        &snapshot,
        settings.fit,
        mode,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let initial_work = initial.report.work as u64;
    let remaining = settings
        .maximum_total_work
        .checked_sub(initial_work)
        .filter(|&v| v > 0)
        .ok_or(Failure::Budget)?;
    let candidate = papyrus_terrain::road_network::Report {
        roads: roads
            .roads
            .iter()
            .zip(&initial.roads)
            .map(|(r, p)| papyrus_terrain::road_network::Road {
                from: r.from,
                to: r.to,
                points: p.points().to_vec(),
            })
            .collect(),
        families: roads.families.clone(),
        ..Default::default()
    };
    let layers = papyrus_terrain::native_layers::Layers {
        size: context.layers.size,
        terrain_seed: context.layers.terrain_seed,
        biome_seed: context.layers.biome_seed,
        height_resolution: context.layers.height_resolution,
        surface_resolution: context.layers.surface_resolution,
        height: initial.height,
        biome: Vec::new(),
        splat: Vec::new(),
        topology: Vec::new(),
        normalized_biome_cells: 0,
        normalized_splat_cells: 0,
    };
    let Some(mut output) = refine_with_source(
        road_surface_stage::Context {
            project: context.project,
            root: context.root,
            worker: context.worker,
            layers: &layers,
        },
        &candidate,
        source,
        Snapshots {
            retained_height: snapshot.retained_height,
            geometry: road_surface_stage::HeightGeometry {
                origin_y: snapshot.paint.height_origin,
                vertical_span: snapshot.paint.vertical_span,
            },
            ground: &context.layers.height,
            source_painted: snapshot.height,
            source_rounding: snapshot.source_rounding,
            topology: snapshot.topology,
            surface_resolution: snapshot.surface_resolution,
            protected_topology: protection(&snapshot),
            allowed_topology: snapshot.allowed_topology,
        },
        Settings {
            maximum_total_work: remaining,
            ..settings
        },
        progress,
        &cancelled,
    )
    .map_err(|error| {
        error
            .with_initial_work(initial_work)
            .bind_ranking_original(roads)
    })?
    else {
        return Ok(None);
    };
    output.report.work += initial_work;
    output.geometry.surfaces.report["refinement"]["work"] = serde_json::json!(output.report.work);
    output.report.initial_bed = Some(initial.report);
    #[cfg(feature = "road-diagnostics")]
    crate::road_transaction_diagnostics::capture_if_requested(
        context.project,
        &candidate,
        context.layers,
        snapshot.height,
        &layers.height,
        snapshot.topology,
        protection(&snapshot),
        snapshot.allowed_topology,
        snapshot.paint.height_origin,
        snapshot.paint.vertical_span,
        &output,
        &cancelled,
    )
    .map_err(Failure::Rebuild)?;
    Ok((!cancelled()).then_some(output))
}
/// Source-backed bridge used by both application workers and CLI callers. It
/// reads the original Layers throughout and rebuilds over borrowed candidates.
pub fn refine_with_source(
    context: road_surface_stage::Context<'_>,
    roads: &papyrus_terrain::road_network::Report,
    source: &crate::road_surface_source::Source,
    snapshots: Snapshots<'_>,
    settings: Settings,
    progress: impl FnMut(&Round),
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, Failure> {
    let strategy = if context
        .project
        .roads
        .as_ref()
        .and_then(|r| r.entrance_launch.as_ref())
        .is_some_and(|r| r.engine == papyrus_terrain::road_network::LaunchEngine::GlobalEarthwork)
    {
        Strategy::QuantizedLocal
    } else {
        Strategy::Local
    };
    refine_source_strategy(
        context, roads, source, snapshots, settings, strategy, progress, cancelled,
    )
}
/// Experimental integer-polished bridge with the same fresh-source acceptance gates.
pub fn refine_quantized_with_source(
    context: road_surface_stage::Context<'_>,
    roads: &papyrus_terrain::road_network::Report,
    source: &crate::road_surface_source::Source,
    snapshots: Snapshots<'_>,
    settings: Settings,
    progress: impl FnMut(&Round),
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, Failure> {
    refine_source_strategy(
        context,
        roads,
        source,
        snapshots,
        settings,
        Strategy::QuantizedLocal,
        progress,
        cancelled,
    )
}
#[allow(clippy::too_many_arguments)]
fn refine_source_strategy(
    context: road_surface_stage::Context<'_>,
    roads: &papyrus_terrain::road_network::Report,
    source: &crate::road_surface_source::Source,
    snapshots: Snapshots<'_>,
    settings: Settings,
    strategy: Strategy,
    mut progress: impl FnMut(&Round),
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, Failure> {
    let recipe = context
        .project
        .roads
        .as_ref()
        .ok_or(Failure::Invalid("missing road recipe"))?;
    recipe.validate().map_err(Failure::Rebuild)?;
    if settings.fit.maximum_grade != recipe.maximum_grade
        || settings.fit.maximum_cut_fill != recipe.maximum_cut_fill
    {
        return Err(Failure::Invalid("saved road bounds differ"));
    }
    let controls: Vec<_> = roads.roads.iter().map(|r| r.points.clone()).collect();
    let mut junctions = Vec::new();
    for (index, road) in roads.roads.iter().enumerate() {
        for (side, dest) in [(0, road.from), (1, road.to)] {
            if matches!(dest, papyrus_terrain::road_network::Destination::RawNetwork(_)) {
                return Err(Failure::Invalid(
                    "strict bed fitting cannot reinterpret raw source joins",
                ));
            }
            if let papyrus_terrain::road_network::Destination::Network { road, point } = dest {
                let own = if side == 0 {
                    0
                } else {
                    controls[index]
                        .len()
                        .checked_sub(1)
                        .ok_or(Failure::Invalid("empty road"))?
                };
                junctions.push(Junction {
                    a: (index, own),
                    b: (road, point),
                });
            }
        }
    }
    let expected_meshes =
        road_surface_stage::mesh_roads(roads, source.policy().maximum_hierarchy_exclusive)
            .map_err(Failure::Rebuild)?;
    let mut observed = Vec::new();
    let result = refine_impl_strategy(
        Input {
            retained_height: snapshots.retained_height,
            layout: Layout {
                size: context.layers.size as f32,
                resolution: context.layers.height_resolution,
                origin_y: snapshots.geometry.origin_y,
                vertical_span: snapshots.geometry.vertical_span,
            },
            initial_height: &context.layers.height,
            ground: snapshots.ground,
            source_painted: snapshots.source_painted,
            source_rounding: snapshots.source_rounding,
            topology: snapshots.topology,
            surface_resolution: snapshots.surface_resolution,
            protected_topology: snapshots.protected_topology,
            allowed_topology: snapshots.allowed_topology,
            controls: &controls,
            junctions: &junctions,
            template: &source
                .meshes
                .first()
                .ok_or(Failure::Invalid("missing source template"))?
                .buffer,
        },
        settings,
        Some(&expected_meshes),
        strategy,
        |height, points, cancel| {
            let candidate = papyrus_terrain::road_network::Report {
                roads: roads
                    .roads
                    .iter()
                    .zip(points)
                    .map(|(road, points)| papyrus_terrain::road_network::Road {
                        from: road.from,
                        to: road.to,
                        points: points.clone(),
                    })
                    .collect(),
                families: roads.families.clone(),
                ..Default::default()
            };
            road_surface_stage::generate_traced_on_height(
                road_surface_stage::Context {
                    project: context.project,
                    root: context.root,
                    worker: context.worker,
                    layers: context.layers,
                },
                &candidate,
                source,
                snapshots.geometry,
                height,
                cancel,
            )
        },
        |round| {
            observed.push(round.clone());
            progress(round);
        },
        cancelled,
    );
    match result {
        Err(Failure::Budget) => Err(Failure::BudgetAtRebuild { rounds: observed }),
        other => other,
    }
}
struct Meter<'a, F: Fn() -> bool> {
    work: u64,
    limit: u64,
    cancel: &'a F,
}
impl<F: Fn() -> bool> Meter<'_, F> {
    fn spend(&mut self, n: u64) -> Result<bool, Failure> {
        if (self.cancel)() {
            return Ok(false);
        }
        self.work = self
            .work
            .checked_add(n)
            .filter(|v| *v <= self.limit)
            .ok_or(Failure::Budget)?;
        Ok(true)
    }
    fn allowance(&self, cap: usize) -> Result<usize, Failure> {
        let available = self.limit.saturating_sub(self.work).min(cap as u64) as usize;
        if available == 0 {
            Err(Failure::Budget)
        } else {
            Ok(available)
        }
    }
}
fn grid<'a>(layout: Layout, height: &'a [i16]) -> Grid<'a> {
    Grid {
        size: layout.size,
        resolution: layout.resolution,
        origin_y: layout.origin_y,
        vertical_span: layout.vertical_span,
        height,
    }
}
fn digest(height: &[i16]) -> [u8; 32] {
    let mut hash = Sha256::new();
    for v in height {
        hash.update(v.to_le_bytes());
    }
    hash.finalize().into()
}
fn is_false(value: &bool) -> bool {
    !*value
}
/// Coupled source meshes depend on indexed controls as well as terrain. Keep
/// the legacy grid-only key for older strategies; recovery phase is also part
/// of the quantized state because its constraint selection changes at handoff.
fn state_digest(
    height: &[i16],
    controls: &[Vec<[f32; 3]>],
    quantized: bool,
    recovery: bool,
    headroom_recovery: bool,
) -> [u8; 32] {
    let terrain = digest(height);
    if !quantized {
        return terrain;
    }
    let mut hash = Sha256::new();
    hash.update(b"papyrus-coupled-road-refinement-state-2");
    hash.update(terrain);
    hash.update([u8::from(recovery), u8::from(headroom_recovery)]);
    hash.update((controls.len() as u64).to_le_bytes());
    for road in controls {
        hash.update((road.len() as u64).to_le_bytes());
        for point in road {
            for value in point {
                hash.update(value.to_bits().to_le_bytes());
            }
        }
    }
    hash.finalize().into()
}
fn protected(input: &Input<'_>, i: usize) -> i32 {
    let n = input.layout.resolution;
    let sn = input.surface_resolution;
    let size = input.layout.size;
    let [x, z] = [i % n, i / n].map(|v| {
        let p = v as f32 / (n - 1) as f32 * size - size * 0.5;
        (((p + size * 0.5) * (1. / size)) * sn as f32).clamp(0., (sn - 1) as f32) as usize
    });
    input.topology[z * sn + x] & input.protected_topology
}
fn validate_state<F: Fn() -> bool>(
    input: &Input<'_>,
    height: &[i16],
    controls: &[Vec<[f32; 3]>],
    settings: Settings,
    meter: &mut Meter<'_, F>,
) -> Result<Option<f64>, Failure> {
    if let Some(retained) = input.retained_height {
        if !meter.spend(retained.samples().len() as u64)? {
            return Ok(None);
        }
        if !retained
            .validate_retained_grid(grid(input.layout, height), meter.cancel)
            .map_err(Failure::Rebuild)?
        {
            return Ok(None);
        }
    }
    let quantum = path_terrain::encoded_height_quantum(input.layout)
        .map_err(|_| Failure::Invalid("height layout"))?;
    let allowance = (f64::from(settings.fit.maximum_cut_fill) / f64::from(quantum)).floor() as i32;
    for (i, &value) in height.iter().enumerate() {
        if i % 4096 == 0 && !meter.spend((height.len() - i).min(4096) as u64)? {
            return Ok(None);
        }
        let (value, ground, painted) = (
            i32::from(value),
            i32::from(input.ground[i]),
            i32::from(input.source_painted[i]),
        );
        if !(0..=32766).contains(&value)
            || !(0..=32766).contains(&ground)
            || !(0..=32766).contains(&painted)
            || (value - ground).abs() > allowance
        {
            return Err(Failure::Invalid("encoded terrain or earthwork"));
        }
        let flags = protected(input, i);
        if flags != 0 {
            let envelope = road_bed::PaintEnvelope {
                source_painted: input.source_painted,
                source_rounding: input.source_rounding,
                allowed_topology: input.allowed_topology,
            };
            if let Some((minimum, maximum)) = envelope.protected_bounds(input.ground[i], flags, i) {
                if !(minimum..=maximum).contains(&value) {
                    return Err(Failure::Invalid("protected paint envelope"));
                }
            } else if value != i32::from(input.initial_height[i]) {
                return Err(Failure::Invalid("fixed protected sample"));
            }
        }
    }
    let mut maximum = 0_f64;
    for (old, road) in input.controls.iter().zip(controls) {
        if old.len() != road.len() {
            return Err(Failure::Invalid("control count"));
        }
        for (old, &p) in old.iter().zip(road) {
            if !meter.spend(8)? {
                return Ok(None);
            }
            if [old[0].to_bits(), old[2].to_bits()] != [p[0].to_bits(), p[2].to_bits()]
                || path_terrain::sample_height(grid(input.layout, height), p)
                    .map_err(|_| Failure::Invalid("control sample"))?
                    .to_bits()
                    != p[1].to_bits()
            {
                return Err(Failure::Invalid("control position or sampled height"));
            }
        }
        for pair in road.windows(2) {
            let run = f64::from(pair[1][0] - pair[0][0]).hypot(f64::from(pair[1][2] - pair[0][2]));
            let grade = f64::from((pair[1][1] - pair[0][1]).abs()) / run;
            if !grade.is_finite() {
                return Err(Failure::Invalid("control horizontal run"));
            }
            maximum = maximum.max(grade);
        }
    }
    for join in input.junctions {
        if !meter.spend(1)? {
            return Ok(None);
        }
        let get = |(r, p): (usize, usize)| controls.get(r).and_then(|r| r.get(p));
        let (Some(a), Some(b)) = (get(join.a), get(join.b)) else {
            return Err(Failure::Invalid("junction address"));
        };
        if a.iter().zip(b).any(|(a, b)| a.to_bits() != b.to_bits()) {
            return Err(Failure::Invalid("junction mismatch"));
        }
    }
    Ok(Some(maximum))
}
fn project(p: [f64; 2], triangle: [[f64; 2]; 3], maximum: f64) -> Option<[f64; 2]> {
    let [a, b, c] = triangle;
    let ab = [b[0] - a[0], b[1] - a[1]];
    let ac = [c[0] - a[0], c[1] - a[1]];
    let det = ab[0] * ac[1] - ab[1] * ac[0];
    if det.abs() < 1e-10 || p.iter().any(|v| !v.is_finite()) {
        return None;
    }
    let q = [p[0] - a[0], p[1] - a[1]];
    let wb = (q[0] * ac[1] - q[1] * ac[0]) / det;
    let wc = (ab[0] * q[1] - ab[1] * q[0]) / det;
    if (1. - wb - wc).min(wb).min(wc) >= 0. {
        return Some(p);
    }
    let mut best = (f64::INFINITY, [0.; 2]);
    for (a, b) in [(a, b), (b, c), (c, a)] {
        let edge = [b[0] - a[0], b[1] - a[1]];
        let length = edge[0] * edge[0] + edge[1] * edge[1];
        let t = if length > 0. {
            (((p[0] - a[0]) * edge[0] + (p[1] - a[1]) * edge[1]) / length).clamp(0., 1.)
        } else {
            0.
        };
        let q = [a[0] + t * edge[0], a[1] + t * edge[1]];
        let distance = (q[0] - p[0]).hypot(q[1] - p[1]);
        if distance < best.0 {
            best = (distance, q)
        }
    }
    (best.0 <= maximum).then_some(best.1)
}
fn witnesses<F: Fn() -> bool>(
    current: Vec<ClearanceProbe>,
    history: &[ClearanceProbe],
    geometry: &TracedOutput,
    settings: Settings,
    retire_history_at_capacity: bool,
    meter: &mut Meter<'_, F>,
) -> Result<Option<(Vec<ClearanceProbe>, usize)>, Failure> {
    let chunks: BTreeMap<_, _> = geometry
        .surfaces
        .chunks
        .iter()
        .filter(|c| c.lod == 0)
        .map(|c| ((c.road, c.chunk_index), &c.geometry))
        .collect();
    let mut result = BTreeMap::new();
    let mut dropped = 0;
    for (probe, old) in current
        .into_iter()
        .map(|p| (p, false))
        .chain(history.iter().cloned().map(|p| (p, true)))
    {
        if !meter.spend(64)? {
            return Ok(None);
        }
        let mut probe = probe;
        if old {
            let Some(chunk) = chunks.get(&(probe.road, probe.chunk)) else {
                dropped += 1;
                continue;
            };
            let Some(face) = chunk.mesh.triangles.get(probe.triangle) else {
                dropped += 1;
                continue;
            };
            let triangle = face.map(|i| {
                [0, 2].map(|j| f64::from(chunk.mesh.vertices[i as usize][j] + chunk.origin[j]))
            });
            let Some(position) = project(
                probe.position,
                triangle,
                settings.maximum_reprojection_distance,
            ) else {
                dropped += 1;
                continue;
            };
            probe.position = position;
        }
        let key = (
            probe.road,
            probe.chunk,
            probe.triangle,
            (probe.position[0] * 1_000_000.).round() as i64,
            (probe.position[1] * 1_000_000.).round() as i64,
        );
        if let Some(previous) = result.get_mut(&key) {
            let previous: &mut ClearanceProbe = previous;
            if probe.maximum_terrain_excess < previous.maximum_terrain_excess {
                *previous = probe
            }
        } else {
            if result.len() == settings.clearance.maximum_probes {
                // Current continuous/source audit witnesses are mandatory. Old
                // linearization points only guide the next proposal; retiring
                // them cannot accept a road, which requires a fresh full audit.
                // Keep the legacy strategy's strict history behavior unchanged.
                if old && retire_history_at_capacity {
                    dropped += 1;
                    continue;
                }
                return Err(Failure::Budget);
            }
            result.insert(key, probe);
        }
    }
    Ok(Some((result.into_values().collect(), dropped)))
}
/// No input is mutated. The callback must rebuild deterministically from the
/// supplied candidate and cooperate with cancellation; source generation does.
pub fn refine(
    input: Input<'_>,
    settings: Settings,
    rebuild: impl FnMut(
        &[i16],
        &[Vec<[f32; 3]>],
        &dyn Fn() -> bool,
    ) -> Result<Option<TracedOutput>, String>,
    progress: impl FnMut(&Round),
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, Failure> {
    refine_impl(input, settings, None, rebuild, progress, cancelled)
}
fn refine_impl(
    input: Input<'_>,
    settings: Settings,
    expected_meshes: Option<&[bool]>,
    rebuild: impl FnMut(
        &[i16],
        &[Vec<[f32; 3]>],
        &dyn Fn() -> bool,
    ) -> Result<Option<TracedOutput>, String>,
    progress: impl FnMut(&Round),
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, Failure> {
    refine_impl_strategy(
        input,
        settings,
        expected_meshes,
        Strategy::Standard,
        rebuild,
        progress,
        cancelled,
    )
}
/// Experimental local-first strategy; every proposal still requires a fresh
/// rebuild and the same final acceptance gates as the full-model transaction.
pub fn refine_local(
    input: Input<'_>,
    settings: Settings,
    rebuild: impl FnMut(
        &[i16],
        &[Vec<[f32; 3]>],
        &dyn Fn() -> bool,
    ) -> Result<Option<TracedOutput>, String>,
    progress: impl FnMut(&Round),
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, Failure> {
    refine_impl_strategy(
        input,
        settings,
        None,
        Strategy::Local,
        rebuild,
        progress,
        cancelled,
    )
}
#[derive(Clone, Copy)]
enum Strategy {
    Standard,
    Local,
    QuantizedLocal,
}
fn fit_candidate(
    retained: Option<&crate::retained_rail_height::RetainedHeight>,
    input: road_bed::Input<'_>,
    settings: road_bed::Settings,
    probes: &[ClearanceProbe],
    pass: road_bed::RebuildPass<'_>,
    progress: impl FnMut(road_bed::Progress),
    cancelled: impl Fn() -> bool,
) -> Result<road_bed::RebuildCandidate, road_bed::Failure> {
    if let Some(retained) = retained {
        return road_bed::fit_retaining_for_rebuild(
            input,
            settings,
            probes,
            pass,
            retained.samples(),
            progress,
            cancelled,
        );
    }
    match pass.mode {
        road_bed::RebuildMode::Feedback => {
            road_bed::fit_for_rebuild(input, settings, probes, pass.envelope, progress, cancelled)
        }
        road_bed::RebuildMode::Quantized => road_bed::fit_quantized_for_rebuild(
            input,
            settings,
            probes,
            pass.envelope,
            progress,
            cancelled,
        ),
        road_bed::RebuildMode::FrozenControls => road_bed::fit_clearance_for_rebuild(
            input,
            settings,
            probes,
            pass.envelope,
            progress,
            cancelled,
        ),
        road_bed::RebuildMode::Local(samples) => road_bed::fit_local_for_rebuild(
            input,
            settings,
            probes,
            pass.envelope,
            samples,
            progress,
            cancelled,
        ),
    }
}
fn refine_impl_strategy(
    mut input: Input<'_>,
    settings: Settings,
    expected_meshes: Option<&[bool]>,
    strategy: Strategy,
    mut rebuild: impl FnMut(
        &[i16],
        &[Vec<[f32; 3]>],
        &dyn Fn() -> bool,
    ) -> Result<Option<TracedOutput>, String>,
    mut progress: impl FnMut(&Round),
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, Failure> {
    let prefer_local = !matches!(strategy, Strategy::Standard);
    let quantized = matches!(strategy, Strategy::QuantizedLocal);
    if !quantized {
        input.source_rounding = None;
    }
    if cancelled() {
        return Ok(None);
    }
    #[cfg(feature = "road-diagnostics")]
    if input.retained_height.is_some() && std::env::var_os("MAPPER_ROAD_FIT_CAPTURE").is_some() {
        return Err(Failure::Invalid(
            "road fit capture cannot omit retained height constraints",
        ));
    }
    settings.fit.validate().map_err(Failure::Fit)?;
    settings
        .clearance
        .validate()
        .map_err(|_| Failure::Invalid("clearance settings"))?;
    path_terrain::encoded_height_quantum(input.layout)
        .map_err(|_| Failure::Invalid("height layout"))?;
    let n = input.layout.resolution;
    let sn = input.surface_resolution;
    if input.initial_height.len() != n * n
        || input.ground.len() != n * n
        || input.source_painted.len() != n * n
        || input.source_rounding.is_some_and(|r| r.cells() != n * n)
        || !(2..=2048).contains(&sn)
        || input.topology.len() != sn * sn
        || input.allowed_topology <= 0
        || input.allowed_topology & !input.protected_topology != 0
        || expected_meshes.is_some_and(|v| v.len() != input.controls.len())
        || (input.controls.is_empty() && expected_meshes.is_none())
        || input.controls.len() > 256
        || input
            .controls
            .iter()
            .any(|r| !(2..=65536).contains(&r.len()))
        || input.controls.iter().map(Vec::len).sum::<usize>() > 131072
        || input.junctions.len() > 131072
        || !(1..=32).contains(&settings.maximum_rounds)
        || !(1..=64_000_000_000).contains(&settings.maximum_total_work)
        || !settings.maximum_reprojection_distance.is_finite()
        || !(0. ..=1.).contains(&settings.maximum_reprojection_distance)
        || settings.fit.minimum_up_normal != settings.clearance.minimum_up_normal
    {
        return Err(Failure::Invalid("transaction inputs"));
    }
    if input.controls.is_empty()
        && (input.initial_height != input.ground
            || input.source_painted != input.ground
            || !input.junctions.is_empty())
    {
        return Err(Failure::Invalid("empty road terrain changed"));
    }
    input
        .template
        .validate()
        .map_err(|_| Failure::Invalid("template"))?;
    let mut meter = Meter {
        work: 0,
        limit: settings.maximum_total_work,
        cancel: &cancelled,
    };
    let mut height = input.initial_height.to_vec();
    let mut controls = input.controls.to_vec();
    let mut history = Vec::new();
    let mut quantized_recovery = false;
    let mut headroom_recovery = false;
    let mut seen = BTreeSet::new();
    seen.insert(state_digest(
        &height,
        &controls,
        quantized,
        quantized_recovery,
        headroom_recovery,
    ));
    let mut rounds = Vec::new();
    let mut fits = Vec::new();
    let mut local_attempts = Vec::new();
    let mut clearance_headroom_retries = Vec::new();
    let engine = if quantized {
        QUANTIZED_VERSION
    } else if prefer_local {
        LOCAL_VERSION
    } else {
        VERSION
    };
    for index in 0..=settings.maximum_rounds {
        let Some(control_grade) = validate_state(&input, &height, &controls, settings, &mut meter)?
        else {
            return Ok(None);
        };
        let Some(geometry) = rebuild(&height, &controls, &cancelled).map_err(Failure::Rebuild)?
        else {
            return Ok(None);
        };
        let has_meshes = if let Some(expected) = expected_meshes {
            road_surface_stage::collision_coverage(expected, &geometry.surfaces)
                .map_err(Failure::Rebuild)?
        } else {
            true
        };
        let rebuild_work = geometry.surfaces.report["work"]
            .as_u64()
            .filter(|v| *v <= 32_000_000)
            .ok_or(Failure::Invalid("rebuild work counter"))?;
        if !meter.spend(rebuild_work)? {
            return Ok(None);
        }
        let grid = grid(input.layout, &height);
        let Some(clearance) = road_surface_stage::audit_clearance(
            grid,
            &geometry.surfaces,
            input.template,
            road_clearance::Settings {
                maximum_work: meter.allowance(settings.clearance.maximum_work)?,
                ..settings.clearance
            },
            &cancelled,
        )
        .map_err(Failure::Rebuild)?
        else {
            return Ok(None);
        };
        if has_meshes && clearance.report["passed"].is_null() {
            return Err(Failure::NoPavedGeometry);
        }
        if !meter.spend(
            clearance.report["work"]
                .as_u64()
                .ok_or(Failure::Invalid("audit work"))?,
        )? {
            return Ok(None);
        }
        let traces: BTreeMap<_, _> = geometry
            .traces
            .iter()
            .map(|t| ((t.road, t.chunk_index), t))
            .collect();
        let count = geometry
            .surfaces
            .chunks
            .iter()
            .filter(|c| c.lod == 0)
            .count();
        if traces.len() != geometry.traces.len() || traces.len() != count {
            return Err(Failure::Invalid("trace coverage"));
        }
        let mut violations = 0;
        let mut maximum_mesh_grade = 0f64;
        let mut collisions = Vec::new();
        for chunk in geometry.surfaces.chunks.iter().filter(|c| c.lod == 0) {
            if chunk.road >= controls.len() {
                return Err(Failure::Invalid("collision road"));
            }
            let trace = traces
                .get(&(chunk.road, chunk.chunk_index))
                .ok_or(Failure::Invalid("trace identity"))?;
            if trace.heights.len() != chunk.geometry.mesh.vertices.len() {
                return Err(Failure::Invalid("trace vertices"));
            }
            for (p, binding) in chunk.geometry.mesh.vertices.iter().zip(&trace.heights) {
                if !meter.spend(64)? {
                    return Ok(None);
                }
                let value = binding
                    .resolve(|x, z| path_terrain::sample_height(grid, [x, 0., z]).ok())
                    .ok_or(Failure::Invalid("trace sample"))?;
                if (value - chunk.geometry.origin[1]).to_bits() != p[1].to_bits() {
                    return Err(Failure::Invalid("stale trace geometry"));
                }
            }
            let grade = match road_mesh_audit::audit(
                &chunk.geometry,
                input.template,
                road_mesh_audit::Settings {
                    maximum_grade: settings.fit.maximum_grade,
                    minimum_up_normal: settings.fit.minimum_up_normal,
                    maximum_work: meter.allowance(32_000_000)?,
                },
                &cancelled,
            ) {
                Ok(v) => v,
                Err(road_mesh_audit::Failure::Cancelled) => return Ok(None),
                Err(e) => return Err(Failure::Rebuild(format!("Grade audit: {e:?}"))),
            };
            if !meter.spend(grade.work as u64)? {
                return Ok(None);
            }
            maximum_mesh_grade = maximum_mesh_grade.max(grade.maximum_longitudinal_grade);
            violations += grade.longitudinal_violations
                + grade.degenerate_world_triangles
                + grade.inverted_world_triangles;
            collisions.push(road_bed::Collision {
                road: chunk.road,
                chunk_index: chunk.chunk_index,
                geometry: &chunk.geometry,
                template: input.template,
                heights: &trace.heights,
            });
        }
        let round = Round {
            index,
            grade_violations: violations,
            maximum_mesh_grade,
            clearance_intersections: clearance.report["continuous_intersections"]
                .as_u64()
                .ok_or(Failure::Invalid("clearance summary"))?
                as usize,
            source_intersections: clearance.report["source_sampler_confirmed_intersections"]
                .as_u64()
                .ok_or(Failure::Invalid("source summary"))?
                as usize,
            maximum_control_grade: control_grade,
            witnesses: clearance.probes.len(),
            dropped_history: 0,
        };
        if violations == 0
            && road_surface_stage::clearance_accepted(has_meshes, &clearance)
            && control_grade <= f64::from(settings.fit.maximum_grade)
        {
            // Capture actual accepted geometry only for the opt-in numeric
            // diagnostic. Failed-round captures below retain merged history.
            #[cfg(feature = "road-diagnostics")]
            crate::road_fit_diagnostics::capture_with_retained_if_requested(
                index,
                road_bed::Input {
                    grid,
                    ground: input.ground,
                    topology: input.topology,
                    surface_resolution: sn,
                    protected_topology: input.protected_topology,
                    controls: &controls,
                    junctions: input.junctions,
                    collisions: &collisions,
                },
                settings.fit,
                &clearance.probes,
                road_bed::PaintEnvelope {
                    source_painted: input.source_painted,
                    source_rounding: input.source_rounding,
                    allowed_topology: input.allowed_topology,
                },
                false,
                input.retained_height,
                &cancelled,
            )
            .map_err(Failure::Rebuild)?;
            progress(&round);
            rounds.push(round);
            if cancelled() {
                return Ok(None);
            }
            let mut geometry = geometry;
            geometry.surfaces.report["clearance_audit"] = clearance.report.clone();
            geometry.surfaces.report["refinement"] = serde_json::json!({"engine":engine,"rounds":rounds.len(),"work":meter.work,"numerical_model_passed":true,"paved_clearance_applicable":has_meshes,"runtime_physics_validated":false,"water_clearance_validated":false});
            if let Some(retained) = input.retained_height {
                geometry.surfaces.report["refinement"]["retained_sample_entries"] =
                    serde_json::json!(retained.samples().len());
            }
            return Ok(Some(Output {
                height,
                controls,
                geometry,
                clearance,
                report: Report {
                    engine,
                    retained_sample_entries: input.retained_height.map(|r| r.samples().len()),
                    initial_bed: None,
                    rounds,
                    fits,
                    local_attempts,
                    clearance_headroom_retries,
                    work: meter.work,
                    quantized_headroom_recovery: headroom_recovery,
                    numerical_model_passed: true,
                    paved_clearance_applicable: has_meshes,
                    runtime_physics_validated: false,
                    water_clearance_validated: false,
                },
            }));
        }
        if index == settings.maximum_rounds {
            progress(&round);
            rounds.push(round);
            return Err(Failure::RoundLimit {
                work: meter.work,
                rounds,
            });
        }
        let mut retired_for_recovery = 0;
        if quantized && !quantized_recovery && violations <= QUANTIZED_RECOVERY_MAX_GRADE_FACES {
            quantized_recovery = true;
            // Retain every fresh continuous/source witness at the handoff.
            // Prior linearization points are optional guidance, not acceptance.
            retired_for_recovery = history.len();
            history.clear();
        }
        let Some((mut probes, dropped)) = witnesses(
            clearance.probes,
            &history,
            &geometry,
            settings,
            prefer_local,
            &mut meter,
        )?
        else {
            return Ok(None);
        };
        let mut round = round;
        round.witnesses = probes.len();
        round.dropped_history = dropped + retired_for_recovery;
        progress(&round);
        rounds.push(round);
        // Inner linearization may be provisional. Only the fresh geometry,
        // control-grade and clearance gates above can publish a final result.
        let mut fit_work_limit = meter.allowance(settings.fit.maximum_work)?;
        let mut fit_progress = None;
        #[cfg(feature = "road-diagnostics")]
        crate::road_fit_diagnostics::capture_with_retained_if_requested(
            index,
            road_bed::Input {
                grid,
                ground: input.ground,
                topology: input.topology,
                surface_resolution: sn,
                protected_topology: input.protected_topology,
                controls: &controls,
                junctions: input.junctions,
                collisions: &collisions,
            },
            road_bed::Settings {
                maximum_work: fit_work_limit,
                quantization_reserve: if quantized_recovery && !headroom_recovery {
                    settings.fit.quantization_reserve.min(0.125)
                } else {
                    settings.fit.quantization_reserve
                },
                reserve_roundoff: (!quantized_recovery || headroom_recovery)
                    && settings.fit.reserve_roundoff,
                ..settings.fit
            },
            &probes,
            road_bed::PaintEnvelope {
                source_painted: input.source_painted,
                source_rounding: input.source_rounding,
                allowed_topology: input.allowed_topology,
            },
            false,
            input.retained_height,
            &cancelled,
        )
        .map_err(Failure::Rebuild)?;
        let bed_input = road_bed::Input {
            grid,
            ground: input.ground,
            topology: input.topology,
            surface_resolution: sn,
            protected_topology: input.protected_topology,
            controls: &controls,
            junctions: input.junctions,
            collisions: &collisions,
        };
        let envelope = road_bed::PaintEnvelope {
            source_painted: input.source_painted,
            source_rounding: input.source_rounding,
            allowed_topology: input.allowed_topology,
        };
        let mut local_candidate = None;
        if prefer_local && violations == 0 && control_grade <= f64::from(settings.fit.maximum_grade)
        {
            let selection = match road_bed::select_clearance_samples(
                bed_input,
                &probes,
                fit_work_limit.min(128_000_000),
                &cancelled,
            ) {
                Ok(v) => v,
                Err(road_bed::Failure::Cancelled) => return Ok(None),
                Err(e) => return Err(Failure::Fit(e)),
            };
            if !meter.spend(selection.work as u64)? {
                return Ok(None);
            }
            fit_work_limit = fit_work_limit
                .checked_sub(selection.work)
                .filter(|&v| v > 0)
                .ok_or(Failure::Budget)?;
            if !selection.samples.is_empty() && fit_work_limit >= 4 {
                let allowance = fit_work_limit / 4;
                let result = fit_candidate(
                    input.retained_height,
                    bed_input,
                    road_bed::Settings {
                        maximum_work: allowance,
                        ..settings.fit
                    },
                    &probes,
                    road_bed::RebuildPass {
                        mode: road_bed::RebuildMode::Local(&selection.samples),
                        envelope,
                    },
                    |p| fit_progress = Some(p),
                    &cancelled,
                );
                let (charged, reason) = match result {
                    Ok(candidate) => {
                        let work = candidate.report.work;
                        let improved = candidate.report.clearance_probe_violations_after
                            < candidate.report.clearance_probe_violations_before
                            || (candidate.report.clearance_probe_violations_after
                                == candidate.report.clearance_probe_violations_before
                                && candidate.report.maximum_probe_terrain_excess_after
                                    < candidate.report.maximum_probe_terrain_excess_before);
                        if candidate.report.violations_after == 0
                            && improved
                            && !seen.contains(&state_digest(
                                &candidate.height,
                                &candidate.controls,
                                quantized,
                                quantized_recovery,
                                headroom_recovery,
                            ))
                        {
                            local_candidate = Some(candidate);
                            (work, None)
                        } else {
                            (work,Some("local proposal did not improve clearance without grade failures or a repeated grid".into()))
                        }
                    }
                    Err(road_bed::Failure::Cancelled) => return Ok(None),
                    // Failed fits cannot report exact work, so conservatively
                    // debit their entire allowance before any full-model retry.
                    Err(error) => (allowance, Some(format!("{error:?}"))),
                };
                local_attempts.push(LocalAttempt {
                    round: index,
                    samples: selection.samples.len(),
                    work: selection.work + charged,
                    adopted: local_candidate.is_some(),
                    reason,
                });
                if local_candidate.is_none() {
                    if !meter.spend(charged as u64)? {
                        return Ok(None);
                    }
                    fit_work_limit = fit_work_limit
                        .checked_sub(charged)
                        .filter(|&v| v > 0)
                        .ok_or(Failure::Budget)?;
                    fit_progress = None;
                }
            }
        }
        let mut result = if let Some(candidate) = local_candidate {
            Ok(candidate)
        } else if quantized_recovery {
            fit_candidate(
                input.retained_height,
                bed_input,
                road_bed::Settings {
                    maximum_work: fit_work_limit,
                    quantization_reserve: if headroom_recovery {
                        settings.fit.quantization_reserve
                    } else {
                        settings.fit.quantization_reserve.min(0.125)
                    },
                    reserve_roundoff: headroom_recovery && settings.fit.reserve_roundoff,
                    ..settings.fit
                },
                &probes,
                road_bed::RebuildPass {
                    mode: road_bed::RebuildMode::Quantized,
                    envelope,
                },
                |p| fit_progress = Some(p),
                &cancelled,
            )
        } else {
            fit_candidate(
                input.retained_height,
                road_bed::Input {
                    grid,
                    ground: input.ground,
                    topology: input.topology,
                    surface_resolution: sn,
                    protected_topology: input.protected_topology,
                    controls: &controls,
                    junctions: input.junctions,
                    collisions: &collisions,
                },
                road_bed::Settings {
                    maximum_work: fit_work_limit,
                    ..settings.fit
                },
                &probes,
                road_bed::RebuildPass {
                    mode: road_bed::RebuildMode::Feedback,
                    envelope: road_bed::PaintEnvelope {
                        source_painted: input.source_painted,
                        source_rounding: input.source_rounding,
                        allowed_topology: input.allowed_topology,
                    },
                },
                |p| fit_progress = Some(p),
                &cancelled,
            )
        };
        // Half-tolerance witnesses are convergence guidance, not the final
        // clearance contract. A source-limited encoded grid can have a minimum
        // between that target and the actual tolerance. Retry provisionally at
        // the latter before rejecting it, without granting another work budget.
        // Fresh geometry/grade/continuous/source-sampler gates above still own
        // acceptance. Keep the legacy standard/local strategies unchanged.
        if quantized
            && let Err(road_bed::Failure::ClearanceInfeasible {
                work,
                minimum,
                limit,
                free_variables,
                ..
            }) = &result
            && *free_variables > 0
            && *limit < settings.clearance.tolerance
            && *minimum <= settings.clearance.tolerance
        {
            let (rejected_work, model_minimum, rejected_limit) = (*work, *minimum, *limit);
            if rejected_work > fit_work_limit {
                return Err(Failure::Invalid("clearance retry work counter"));
            }
            if !meter.spend(rejected_work as u64)? {
                return Ok(None);
            }
            fit_work_limit = fit_work_limit
                .checked_sub(rejected_work)
                .filter(|&v| v > 0)
                .ok_or(Failure::Budget)?;
            for probe in &mut probes {
                probe.maximum_terrain_excess = settings.clearance.tolerance as f32;
            }
            clearance_headroom_retries.push(ClearanceHeadroomRetry {
                round: index,
                rejected_limit,
                model_minimum,
                provisional_limit: settings.clearance.tolerance,
                rejected_work,
                work_before_retry: meter.work,
            });
            fit_progress = None;
            result = fit_candidate(
                input.retained_height,
                bed_input,
                road_bed::Settings {
                    maximum_work: fit_work_limit,
                    quantization_reserve: if quantized_recovery && !headroom_recovery {
                        settings.fit.quantization_reserve.min(0.125)
                    } else {
                        settings.fit.quantization_reserve
                    },
                    reserve_roundoff: (!quantized_recovery || headroom_recovery)
                        && settings.fit.reserve_roundoff,
                    ..settings.fit
                },
                &probes,
                road_bed::RebuildPass {
                    mode: if quantized_recovery {
                        road_bed::RebuildMode::Quantized
                    } else {
                        road_bed::RebuildMode::Feedback
                    },
                    envelope,
                },
                |p| fit_progress = Some(p),
                &cancelled,
            );
        }
        let fitted = match result {
            Ok(v) => v,
            Err(road_bed::Failure::Cancelled) => return Ok(None),
            Err(error) => {
                let witness = alignment_witness(&error, &collisions);
                let metadata_work = witness
                    .and_then(|w| controls.get(w.road))
                    .map_or(0, |points| points.len() as u64 * 2 + 1);
                let captured_work = match &error {
                    road_bed::Failure::ClearanceInfeasible { work, .. } => Some(*work as u64),
                    _ => None,
                };
                let ranking_revision = if metadata_work > 0
                    && let Some(witness) = witness.filter(|w| w.kind == AlignmentKind::Clearance)
                    && captured_work.is_some_and(|fit| {
                        metadata_work <= meter.limit.saturating_sub(meter.work).saturating_sub(fit)
                    }) {
                    let original = input.controls[witness.road].clone();
                    let road_controls = std::mem::take(&mut controls[witness.road]);
                    Some(Box::new(crate::road_portal_revision::Revision::new(
                        witness.road,
                        index,
                        input.layout,
                        height,
                        road_controls,
                        original,
                    )))
                } else {
                    None
                };
                return Err(Failure::FittingRound {
                    round: index,
                    work_before_fit: meter.work
                        + if ranking_revision.is_some() {
                            metadata_work
                        } else {
                            0
                        },
                    fit_work_limit,
                    progress: fit_progress.map(Box::new),
                    witness: witness.map(Box::new),
                    ranking_revision,
                    error: Box::new(error),
                });
            }
        };
        if !meter.spend(fitted.report.work as u64)? {
            return Ok(None);
        }
        if !seen.insert(state_digest(
            &fitted.height,
            &fitted.controls,
            quantized,
            quantized_recovery,
            headroom_recovery,
        )) {
            if quantized {
                // A global projection can stall above the grade-count handoff
                // threshold. Give the existing integer-polished strategy its
                // one bounded recovery opportunity before declaring a cycle.
                // The next iteration still rebuilds and checks fresh meshes.
                if !quantized_recovery {
                    quantized_recovery = true;
                    history.clear();
                    height = fitted.height;
                    controls = fitted.controls;
                    fits.push(fitted.report);
                    continue;
                }
                if !headroom_recovery
                    && (settings.fit.quantization_reserve > 0.125 || settings.fit.reserve_roundoff)
                {
                    // Retry the original rounding headroom once. Integer
                    // polishing can stall in a weaker-margin linearization;
                    // fresh source geometry and every physical gate still run.
                    headroom_recovery = true;
                    history.clear();
                    height = fitted.height;
                    controls = fitted.controls;
                    fits.push(fitted.report);
                    continue;
                }
                return Err(Failure::CycleAtRebuild {
                    work: meter.work,
                    rounds,
                    last_fit: Box::new(fitted.report),
                });
            }
            return Err(Failure::Cycle);
        }
        height = fitted.height;
        controls = fitted.controls;
        fits.push(fitted.report);
        history = probes;
    }
    Err(Failure::RoundLimit {
        work: meter.work,
        rounds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coupled_cycle_identity_distinguishes_controls_partitioning_and_recovery_phase() {
        let terrain = [0_i16, 1, 2, 3];
        let controls = vec![vec![[0., 1., 0.], [10., 2., 0.]]];
        let key = state_digest(&terrain, &controls, true, false, false);
        let mut changed = controls.clone();
        changed[0][1][1] = 2.01;
        assert_ne!(key, state_digest(&terrain, &changed, true, false, false));
        assert_ne!(key, state_digest(&terrain, &controls, true, true, false));
        assert_ne!(
            state_digest(&terrain, &controls, true, true, false),
            state_digest(&terrain, &controls, true, true, true)
        );
        let partitioned = vec![vec![controls[0][0]], vec![controls[0][1]]];
        assert_ne!(
            key,
            state_digest(&terrain, &partitioned, true, false, false)
        );
        assert_eq!(key, state_digest(&terrain, &controls, true, false, false));
        assert_eq!(
            state_digest(&terrain, &controls, false, false, false),
            digest(&terrain)
        );
        assert_eq!(
            state_digest(&terrain, &changed, false, true, false),
            digest(&terrain)
        );
        assert_eq!(
            state_digest(&terrain, &changed, false, true, true),
            digest(&terrain)
        );
    }
    #[test]
    fn retry_failure_charges_include_initial_bed_and_conserve_the_shared_grant() {
        let early = Failure::FittingRound {
            ranking_revision: None,
            round: 0,
            work_before_fit: 100,
            fit_work_limit: 2000,
            progress: None,
            witness: None,
            error: Box::new(road_bed::Failure::ClearanceInfeasible {
                work: 20,
                road: 0,
                chunk: 0,
                triangle: 0,
                position: [0.; 2],
                topology: 0,
                free_variables: 0,
                minimum: 0.03,
                limit: 0.02,
            }),
        }
        .with_initial_work(30);
        assert_eq!(early.charged_work(3000), 150);
        let remaining = 3000 - early.charged_work(3000);
        let unknown = Failure::FittingRound {
            ranking_revision: None,
            round: 1,
            work_before_fit: 200,
            fit_work_limit: 2000,
            progress: None,
            witness: None,
            error: Box::new(road_bed::Failure::Budget),
        }
        .with_initial_work(30);
        assert_eq!(unknown.charged_work(remaining), 2230);
        assert_eq!(remaining - unknown.charged_work(remaining), 620);
        assert_eq!(unknown.charged_work(10), 10);
        assert_eq!(Failure::Budget.charged_work(620), 620);
        let initial = Failure::InitialControlProposal {
            conflict: papyrus_terrain::road_grade::ControlConflict {
                kind: papyrus_terrain::road_grade::ConflictKind::IntervalEnvelope,
                road: 0,
                point: 1,
                position: [0.; 3],
                required: 10.,
                available: 2.,
                envelope: None,
            },
            initial_work_limit: 512,
        };
        assert_eq!(initial.charged_work(3000), 512);
        assert_eq!(initial.charged_work(10), 10);
        assert_eq!(Failure::Rebuild("failed".into()).charged_work(620), 620);
        assert_eq!(
            Failure::RoundLimit {
                work: 200,
                rounds: vec![]
            }
            .with_initial_work(30)
            .charged_work(3000),
            230
        );
        assert_eq!(
            Failure::RoundLimit {
                work: u64::MAX,
                rounds: vec![]
            }
            .with_initial_work(30)
            .charged_work(3000),
            3000
        );
    }
    #[test]
    fn raw_source_bed_preparation_checks_families_bounds_cancellation_and_attachment_indices() {
        use papyrus_terrain::{
            native_layers::Layers,
            path_geometry::Path,
            road_network::{self, Destination},
        };
        let paint: crate::road_paint_source::Profile = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-road-paint.json"
        ))
        .unwrap();
        let terrain = crate::road_terrain_source::Profile::for_paint(&paint)
            .unwrap()
            .unwrap();
        let layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: 3,
            surface_resolution: 8,
            height: vec![17039; 9],
            topology: vec![0; 64],
            biome: vec![],
            splat: vec![],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        let y = path_terrain::sample_height(
            Grid {
                size: 1000.,
                resolution: 3,
                origin_y: paint.height_origin,
                vertical_span: paint.vertical_span,
                height: &layers.height,
            },
            [0.; 3],
        )
        .unwrap();
        let points = vec![[-400., y, 0.], [400., y, 0.]];
        let paths = vec![
            Path::from_controls(&points, 10000, || false)
                .unwrap()
                .unwrap(),
        ];
        let mut roads = road_network::Report {
            roads: vec![road_network::Road {
                from: Destination::Monument {
                    monument: 0,
                    connector: 0,
                },
                to: Destination::Monument {
                    monument: 1,
                    connector: 0,
                },
                points,
            }],
            families: vec![road_network::Family {
                infrastructure_type: 0,
                hierarchy: 1,
                seed: 1,
                first_road: 0,
                road_count: 1,
                socket_count: 2,
                unresolved: vec![],
                width: 10.,
                splat_channel: 7,
            }],
            ..Default::default()
        };
        let snapshot = PaintedSnapshot {
            source_rounding: None,
            retained_height: None,
            height: &layers.height,
            topology: &layers.topology,
            surface_resolution: 8,
            paths: &paths,
            paint: &paint,
            terrain: &terrain,
            allowed_topology: 1024,
            maximum_initial_work: 100000,
        };
        let settings = road_bed::Settings {
            maximum_grade: 0.2,
            maximum_cut_fill: 8.,
            minimum_up_normal: 0.5,
            maximum_iterations: 32,
            maximum_work: 100000,
            quantization_reserve: 0.51,
            reserve_roundoff: true,
        };
        let retained = crate::retained_rail_height::RetainedHeight::test_capture(
            Grid {
                size: 1000.,
                resolution: 3,
                origin_y: paint.height_origin,
                vertical_span: paint.vertical_span,
                height: &layers.height,
            },
            &[4],
        );
        let held_snapshot = PaintedSnapshot {
            source_rounding: None,
            retained_height: Some(&retained),
            ..snapshot
        };
        for mode in [
            papyrus_terrain::road_grade::JunctionMode::Pinned,
            papyrus_terrain::road_grade::JunctionMode::CoupledNetwork,
        ] {
            let output = prepare_painted_bed_with_junction_mode(
                &layers,
                &roads,
                &held_snapshot,
                settings,
                mode,
                || false,
            )
            .unwrap()
            .unwrap();
            assert_eq!(output.height, layers.height);
            assert_eq!(output.report.retained_sample_entries, Some(1));
            assert_eq!(
                output.report.moved_junctions.is_some(),
                mode == papyrus_terrain::road_grade::JunctionMode::CoupledNetwork
            );
        }
        let result = prepare_painted_bed(&layers, &roads, &snapshot, settings, || false)
            .unwrap()
            .unwrap();
        assert_eq!(result.height, layers.height);
        assert!(
            result.report.control_grades_validated && result.report.full_grid_earthwork_validated
        );
        assert!(!result.report.runtime_access_validated);
        assert!(
            prepare_painted_bed(&layers, &roads, &snapshot, settings, || true)
                .unwrap()
                .is_none()
        );
        roads.families[0].width = 11.;
        assert!(matches!(
            prepare_painted_bed(&layers, &roads, &snapshot, settings, || false),
            Err(Failure::Invalid("road family width differs"))
        ));
        roads.families[0].width = 10.;
        roads.families.push(roads.families[0].clone());
        assert!(matches!(
            prepare_painted_bed(&layers, &roads, &snapshot, settings, || false),
            Err(Failure::Invalid("overlapping road families"))
        ));
        roads.families.pop();
        roads.roads[0].to = Destination::Network { road: 1, point: 0 };
        assert!(matches!(
            prepare_painted_bed(&layers, &roads, &snapshot, settings, || false),
            Err(Failure::Invalid("network attachment"))
        ));
        assert_eq!(layers.height, vec![17039; 9]);
    }
    use crate::road_surface_stage::HeightTrace;
    use papyrus_terrain::{
        path_mesh::{Chunk, HeightBinding},
        road_network::{SurfaceChunk, SurfaceOutput},
    };
    struct Fixture {
        height: Vec<i16>,
        ground: Vec<i16>,
        topology: Vec<i32>,
        controls: Vec<Vec<[f32; 3]>>,
        joins: Vec<Junction>,
        template: Template,
        n: usize,
        span: f32,
    }
    impl Fixture {
        fn new(n: usize) -> Self {
            let ground = vec![10000; n * n];
            let mut height = ground.clone();
            height[(n / 2) * n + n / 2] += 164;
            let template = Template {
                vertices: vec![[-500., 0., -500.], [500., 0., -500.], [0., 0., 250.]],
                normals: vec![[0., 1., 0.]; 3],
                tangents: vec![[1., 0., 0., 1.]; 3],
                uv: vec![[0.; 2]; 3],
                triangles: vec![[0, 1, 2]],
                center: [0., 0., -125.],
                extents: [500., 0., 375.],
            };
            let mut fixture = Self {
                height,
                ground,
                topology: vec![1024; n * n],
                controls: vec![
                    vec![[-500., 0., -500.], [0., 0., 0.]],
                    vec![[0., 0., 0.], [0., 0., 250.]],
                ],
                joins: vec![Junction {
                    a: (0, 1),
                    b: (1, 0),
                }],
                template,
                n,
                span: 1000.,
            };
            fixture.resample();
            fixture
        }
        fn layout(&self) -> Layout {
            Layout {
                size: 1000.,
                resolution: self.n,
                origin_y: -500.,
                vertical_span: self.span,
            }
        }
        fn resample(&mut self) {
            let layout = self.layout();
            for point in self.controls.iter_mut().flatten() {
                point[1] = path_terrain::sample_height(grid(layout, &self.height), *point).unwrap();
            }
        }
        fn input(&self) -> Input<'_> {
            Input {
                retained_height: None,
                layout: self.layout(),
                initial_height: &self.height,
                ground: &self.ground,
                source_painted: &self.height,
                source_rounding: None,
                topology: &self.topology,
                surface_resolution: self.n,
                protected_topology: 1024,
                allowed_topology: 1024,
                controls: &self.controls,
                junctions: &self.joins,
                template: &self.template,
            }
        }
        fn rebuild(&self, height: &[i16], horizontal: Option<f32>) -> TracedOutput {
            let mut mesh = self.template.clone();
            if let Some(offset) = horizontal {
                for p in &mut mesh.vertices {
                    p[0] = p[0] * 0.5 + offset;
                }
                mesh.center[0] = offset;
                mesh.extents[0] *= 0.5;
            }
            for p in &mut mesh.vertices {
                p[1] = path_terrain::sample_height(grid(self.layout(), height), *p).unwrap();
            }
            let lo = mesh
                .vertices
                .iter()
                .map(|p| p[1])
                .fold(f32::INFINITY, f32::min);
            let hi = mesh
                .vertices
                .iter()
                .map(|p| p[1])
                .fold(f32::NEG_INFINITY, f32::max);
            mesh.center[1] = (lo + hi) * 0.5;
            mesh.extents[1] = (hi - lo) * 0.5;
            let heights = mesh
                .vertices
                .iter()
                .map(|p| HeightBinding::Endpoint {
                    position: [p[0], p[2]],
                })
                .collect();
            TracedOutput {
                surfaces: SurfaceOutput {
                    report: serde_json::json!({"work":0,"collision_grades_passed":true,"clearance_audit":{"passed":true}}),
                    chunks: vec![SurfaceChunk {
                        road: 0,
                        lod: 0,
                        chunk_index: 0,
                        geometry: Chunk {
                            first_repetition: 0,
                            repetitions: 1,
                            origin: [0.; 3],
                            mesh,
                            work: 0,
                        },
                    }],
                },
                traces: vec![HeightTrace {
                    road: 0,
                    chunk_index: 0,
                    heights,
                }],
            }
        }
    }
    #[test]
    fn grade_failure_witness_uses_identified_face_in_world_space() {
        let fixture = Fixture::new(3);
        let mut rebuilt = fixture.rebuild(&fixture.height, None);
        rebuilt.surfaces.chunks[0].geometry.origin = [1234.5, 50., -1000.25];
        let chunk = &rebuilt.surfaces.chunks[0].geometry;
        let collisions = [road_bed::Collision {
            road: 7,
            chunk_index: 9,
            geometry: chunk,
            template: &fixture.template,
            heights: &rebuilt.traces[0].heights,
        }];
        let error = road_bed::Failure::Infeasible {
            road: 7,
            chunk: 9,
            triangle: 0,
            minimum: 1.,
            maximum: 1.,
            limit: 0.2,
        };
        let witness = alignment_witness(&error, &collisions).unwrap();
        assert_eq!(witness.kind, AlignmentKind::Grade);
        assert_eq!(witness.road, 7);
        assert!((witness.position[0] - 1234.5).abs() < 1e-10);
        assert!((witness.position[1] + 1250.25).abs() < 1e-10);
        assert!(alignment_witness(&road_bed::Failure::Budget, &collisions).is_none());
        assert!(alignment_witness(&error, &[]).is_none());
        let folded = road_bed::Failure::HorizontalGeometry {
            road: 7,
            chunk: 9,
            triangle: 0,
            inverted: 13,
            degenerate: 0,
        };
        let folded_witness = alignment_witness(&folded, &collisions).unwrap();
        assert_eq!(folded_witness.kind, AlignmentKind::Grade);
        assert_eq!(folded_witness.road, witness.road);
        assert_eq!(folded_witness.position, witness.position);
        assert!(alignment_witness(&folded, &[]).is_none());
        // Continuous-clearance extrema can be between vertices. Preserve the
        // exact f64 location rather than substituting a face centroid or f32.
        let exact = [1234.5000000000002, -1250.2500000000002];
        let clearance = |position| road_bed::Failure::ClearanceInfeasible {
            work: 10,
            road: 7,
            chunk: 9,
            triangle: 0,
            position,
            topology: 0,
            free_variables: 8,
            minimum: 0.025,
            limit: 0.01,
        };
        assert_eq!(
            alignment_witness(&clearance(exact), &collisions)
                .unwrap()
                .position,
            exact
        );
        assert_eq!(
            alignment_witness(&clearance(exact), &collisions)
                .unwrap()
                .kind,
            AlignmentKind::Clearance
        );
        assert!(alignment_witness(&clearance([f64::NAN, 0.]), &collisions).is_none());
        assert!(alignment_witness(&clearance(exact), &[]).is_none());
    }
    fn settings() -> Settings {
        Settings {
            fit: road_bed::Settings {
                maximum_grade: 0.2,
                maximum_cut_fill: 8.,
                minimum_up_normal: 0.5,
                maximum_iterations: 512,
                maximum_work: 2_000_000,
                quantization_reserve: 0.51,
                reserve_roundoff: true,
            },
            clearance: road_clearance::Settings {
                tolerance: 0.02,
                minimum_up_normal: 0.5,
                maximum_work: 10000,
                maximum_probes: 1000,
            },
            maximum_rounds: 4,
            maximum_total_work: 10_000_000,
            maximum_reprojection_distance: 0.25,
        }
    }
    #[test]
    fn bounded_history_keeps_fresh_witnesses_tighter_duplicates_and_budget_accounting() {
        let fixture = Fixture::new(3);
        let geometry = fixture.rebuild(&fixture.height, None);
        let chunk = &geometry.surfaces.chunks[0].geometry;
        let face = chunk.mesh.triangles[0];
        let triangle = face.map(|i| {
            [0, 2].map(|k| f64::from(chunk.mesh.vertices[i as usize][k] + chunk.origin[k]))
        });
        let probe = |weights: [f64; 3], tolerance| ClearanceProbe {
            road: 0,
            chunk: 0,
            triangle: 0,
            position: std::array::from_fn(|k| (0..3).map(|i| weights[i] * triangle[i][k]).sum()),
            maximum_terrain_excess: tolerance,
        };
        let fresh = probe([0.3, 0.3, 0.4], 0.02);
        let older = probe([0.1, 0.5, 0.4], 0.02);
        let overflow = probe([0.6, 0.2, 0.2], 0.02);
        let tighter = ClearanceProbe {
            maximum_terrain_excess: 0.01,
            ..fresh.clone()
        };
        let history = [older.clone(), overflow.clone(), tighter];
        let mut limited = settings();
        limited.clearance.maximum_probes = 2;
        let cancel = || false;
        let mut meter = Meter {
            work: 0,
            limit: 10000,
            cancel: &cancel,
        };
        let (kept, dropped) = witnesses(
            vec![fresh.clone()],
            &history,
            &geometry,
            limited,
            true,
            &mut meter,
        )
        .unwrap()
        .unwrap();
        assert_eq!(kept.len(), 2);
        assert_eq!(dropped, 1);
        assert!(
            kept.iter()
                .any(|p| p.position == fresh.position && p.maximum_terrain_excess == 0.01)
        );
        assert_eq!(meter.work, 4 * 64);
        assert!(matches!(
            witnesses(
                vec![fresh.clone()],
                &history,
                &geometry,
                limited,
                false,
                &mut meter
            ),
            Err(Failure::Budget)
        ));
        // A fresh audit overflowing its own cap must still fail, even when
        // historical retirement is enabled. There is no partial audit success.
        assert!(matches!(
            witnesses(
                vec![fresh, older, overflow],
                &[],
                &geometry,
                limited,
                true,
                &mut meter
            ),
            Err(Failure::Budget)
        ));
        let cancel = || true;
        let mut meter = Meter {
            work: 0,
            limit: 10000,
            cancel: &cancel,
        };
        assert!(
            witnesses(history.to_vec(), &[], &geometry, limited, true, &mut meter)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn empty_meshes_require_explicit_source_applicability_and_never_claim_paved_clearance() {
        let fixture = Fixture::new(3);
        let empty = || TracedOutput {
            surfaces: SurfaceOutput {
                chunks: vec![],
                report: serde_json::json!({"work":0,"passed":true}),
            },
            traces: vec![],
        };
        assert!(matches!(
            refine(
                fixture.input(),
                settings(),
                |_, _, _| Ok(Some(empty())),
                |_| {},
                || false
            ),
            Err(Failure::NoPavedGeometry)
        ));
        let out = refine_impl(
            fixture.input(),
            settings(),
            Some(&[false, false]),
            |_, _, _| Ok(Some(empty())),
            |_| {},
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(!out.report.paved_clearance_applicable);
        assert!(out.clearance.report["passed"].is_null());
        assert_eq!(out.height, fixture.height);
        assert!(matches!(
            refine_impl(
                fixture.input(),
                settings(),
                Some(&[true, false]),
                |_, _, _| Ok(Some(empty())),
                |_| {},
                || false
            ),
            Err(Failure::Rebuild(_))
        ));
    }
    #[test]
    fn source_empty_network_rejects_changed_snapshots_and_dangling_controls_before_rebuild() {
        let fixture = Fixture::new(3);
        let input = || Input {
            controls: &[],
            junctions: &[],
            ground: &fixture.height,
            ..fixture.input()
        };
        let empty = || TracedOutput {
            surfaces: SurfaceOutput {
                chunks: vec![],
                report: serde_json::json!({"work":0}),
            },
            traces: vec![],
        };
        let output = refine_impl(
            input(),
            settings(),
            Some(&[]),
            |_, _, _| Ok(Some(empty())),
            |_| {},
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(output.height, fixture.height);
        assert!(output.controls.is_empty());
        assert!(!output.report.paved_clearance_applicable);
        let mut changed = input();
        changed.source_painted = &fixture.ground;
        assert!(matches!(
            refine_impl(
                changed,
                settings(),
                Some(&[]),
                |_, _, _| panic!(),
                |_| {},
                || false
            ),
            Err(Failure::Invalid("empty road terrain changed"))
        ));
        let joins = [Junction {
            a: (0, 0),
            b: (0, 0),
        }];
        let mut dangling = input();
        dangling.junctions = &joins;
        assert!(matches!(
            refine_impl(
                dangling,
                settings(),
                Some(&[]),
                |_, _, _| panic!(),
                |_| {},
                || false
            ),
            Err(Failure::Invalid("empty road terrain changed"))
        ));
    }
    #[test]
    fn quantized_strategy_requires_fresh_meshes_and_keeps_original_inputs_private() {
        let fixture = Fixture::new(3);
        let original = fixture.height.clone();
        let mut settings = settings();
        settings.fit.maximum_grade = 0.001;
        let mut calls = 0;
        let out = refine_impl_strategy(
            fixture.input(),
            settings,
            None,
            Strategy::QuantizedLocal,
            |height, _, _| {
                calls += 1;
                Ok(Some(fixture.rebuild(height, None)))
            },
            |_| {},
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(calls >= 2);
        assert_eq!(out.report.engine, QUANTIZED_VERSION);
        assert!(out.report.numerical_model_passed);
        assert!(
            out.report
                .fits
                .iter()
                .any(|f| f.engine == road_bed::QUANTIZED_REBUILD_VERSION)
        );
        assert_eq!(out.report.rounds.last().unwrap().grade_violations, 0);
        assert_eq!(out.clearance.report["passed"], true);
        assert!(!out.report.runtime_physics_validated && !out.report.water_clearance_validated);
        assert_eq!(fixture.height, original);
        // A claimed pass from the callback cannot bypass a bad fresh surface.
        let mut calls = 0;
        let mut limited = settings;
        limited.maximum_rounds = 1;
        let result = refine_impl_strategy(
            fixture.input(),
            limited,
            None,
            Strategy::QuantizedLocal,
            |height, _, _| {
                calls += 1;
                Ok(Some(fixture.rebuild(height, Some(500.))))
            },
            |_| {},
            || false,
        );
        assert!(result.is_err() || matches!(result, Ok(None)));
        assert!(calls >= 1);
        assert_eq!(fixture.height, original);
    }
    #[test]
    fn quantized_headroom_retry_can_fit_a_source_limited_grid_within_final_clearance() {
        let mut fixture = Fixture::new(5);
        fixture.span = 500.; // One encoded unit is about 15.26 mm.
        fixture.ground[12] = 10001;
        fixture.height[12] = 10002;
        fixture.resample();
        let original = fixture.height.clone();
        let configured = settings();
        // The source-modified center may fall only to 10001. Its 15 mm floor
        // cannot meet 10 mm guidance, but can meet the actual 20 mm contract.
        let mut calls = 0;
        let output = refine_impl_strategy(
            fixture.input(),
            configured,
            None,
            Strategy::QuantizedLocal,
            |height, _, _| {
                calls += 1;
                Ok(Some(fixture.rebuild(height, None)))
            },
            |_| {},
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(calls >= 2);
        assert_eq!(output.height[12], 10001);
        assert_eq!(output.clearance.report["passed"], true);
        assert_eq!(
            output.clearance.report["tolerance"],
            configured.clearance.tolerance
        );
        assert_eq!(output.report.clearance_headroom_retries.len(), 1);
        let retry = &output.report.clearance_headroom_retries[0];
        assert!(retry.model_minimum > 0.01 && retry.model_minimum < 0.02);
        assert_eq!(retry.provisional_limit, configured.clearance.tolerance);
        assert!(retry.rejected_work > 0);
        assert!(output.report.work <= configured.maximum_total_work);
        assert_eq!(fixture.height, original);
        assert!(!output.report.runtime_physics_validated);
        // A provisional fit must not accept a newly rebuilt surface that has
        // become lower than the proposal assumed. Callback pass flags do not
        // replace the actual continuous/source height audit.
        let mut limited = configured;
        limited.maximum_rounds = 1;
        let mut rebuilds = 0;
        let result = refine_impl_strategy(
            fixture.input(),
            limited,
            None,
            Strategy::QuantizedLocal,
            |height, _, _| {
                rebuilds += 1;
                let mut geometry = fixture.rebuild(height, None);
                if rebuilds > 1 {
                    for chunk in &mut geometry.surfaces.chunks {
                        for p in &mut chunk.geometry.mesh.vertices {
                            p[1] -= 0.03;
                        }
                        chunk.geometry.mesh.center[1] -= 0.03;
                    }
                    for trace in &mut geometry.traces {
                        for binding in &mut trace.heights {
                            let HeightBinding::Endpoint { position } = *binding else {
                                panic!("Fixture endpoint trace expected")
                            };
                            *binding = HeightBinding::Span {
                                left: position,
                                right: position,
                                across: 0.,
                                side_offset: 0.,
                                template_offset: -0.03,
                            };
                        }
                    }
                }
                Ok(Some(geometry))
            },
            |_| {},
            || false,
        );
        assert_eq!(rebuilds, 2);
        assert!(
            matches!(result,Err(Failure::RoundLimit{rounds,..}) if rounds.last().unwrap().clearance_intersections>0)
        );
        // The legacy algorithm remains unchanged.
        assert!(matches!(refine_impl_strategy(
            fixture.input(), configured, None, Strategy::Standard,
            |height,_,_| Ok(Some(fixture.rebuild(height,None))), |_|{}, ||false,
        ),Err(Failure::FittingRound{error,..}) if matches!(*error,road_bed::Failure::ClearanceInfeasible{minimum,..} if minimum>0.01 && minimum<0.02)));
        // An actual floor above 20 mm is still rejected, not reclassified as a
        // passing map merely because the guidance target was conservative.
        fixture.span = 1000.;
        fixture.resample();
        assert!(matches!(refine_impl_strategy(
            fixture.input(), configured, None, Strategy::QuantizedLocal,
            |height,_,_| Ok(Some(fixture.rebuild(height,None))), |_|{}, ||false,
        ),Err(Failure::FittingRound{error,..}) if matches!(*error,road_bed::Failure::ClearanceInfeasible{minimum,..} if minimum>0.02)));
    }
    #[test]
    fn rebuilds_and_audits_real_buffers_before_returning_a_private_candidate() {
        let fixture = Fixture::new(3);
        let original = fixture.height.clone();
        let mut calls = 0;
        let mut reported = 0;
        let out = refine(
            fixture.input(),
            settings(),
            |height, _, _| {
                calls += 1;
                Ok(Some(fixture.rebuild(height, None)))
            },
            |_| reported += 1,
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(calls >= 2 && !out.report.fits.is_empty());
        assert_eq!(reported, out.report.rounds.len());
        assert!(
            out.report.numerical_model_passed
                && !out.report.runtime_physics_validated
                && !out.report.water_clearance_validated
        );
        assert_eq!(out.clearance.report["passed"], true);
        assert_eq!(out.controls[0][1], out.controls[1][0]);
        assert!(
            out.report
                .fits
                .iter()
                .any(|f| f.control_heights_resampled > 0)
        );
        assert_eq!(fixture.height, original);
        assert_ne!(out.height, original);
        assert_eq!(
            out.geometry.surfaces.report["clearance_audit"]["passed"],
            true
        );
    }
    #[test]
    fn cancellation_and_rebuild_failure_after_fitting_do_not_publish_height() {
        let fixture = Fixture::new(3);
        let original = fixture.height.clone();
        let cancel = std::cell::Cell::new(false);
        let mut calls = 0;
        let out = refine(
            fixture.input(),
            settings(),
            |height, _, _| {
                calls += 1;
                if calls == 2 {
                    cancel.set(true);
                }
                Ok(Some(fixture.rebuild(height, None)))
            },
            |_| {},
            || cancel.get(),
        )
        .unwrap();
        assert!(out.is_none() && calls == 2);
        assert_eq!(fixture.height, original);
        let mut calls = 0;
        let result = refine(
            fixture.input(),
            settings(),
            |height, _, _| {
                calls += 1;
                if calls == 2 {
                    return Err("worker failed".into());
                }
                Ok(Some(fixture.rebuild(height, None)))
            },
            |_| {},
            || false,
        );
        assert!(matches!(result, Err(Failure::Rebuild(_))));
        assert_eq!(fixture.height, original);
    }
    #[test]
    fn rejects_stale_geometry_and_snapshot_errors_without_using_claimed_pass_flags() {
        let fixture = Fixture::new(3);
        let result = refine(
            fixture.input(),
            settings(),
            |_, _, _| Ok(Some(fixture.rebuild(&fixture.height, None))),
            |_| {},
            || false,
        );
        assert!(matches!(
            result,
            Err(Failure::Invalid("stale trace geometry"))
        ));
        let mut stale = Fixture::new(3);
        stale.controls[0][0][1] += 1.;
        let mut called = false;
        let result = refine(
            stale.input(),
            settings(),
            |_, _, _| {
                called = true;
                unreachable!()
            },
            |_| {},
            || false,
        );
        assert!(matches!(
            result,
            Err(Failure::Invalid("control position or sampled height"))
        ));
        assert!(!called);
        let result = refine(
            fixture.input(),
            Settings {
                maximum_total_work: 1,
                ..settings()
            },
            |_, _, _| unreachable!(),
            |_| {},
            || false,
        );
        assert!(matches!(result, Err(Failure::Budget)));
    }
    #[test]
    fn larger_rebuild_allowance_keeps_the_same_early_acceptance_and_work_guards() {
        let fixture = Fixture::new(3);
        let out = refine(
            fixture.input(),
            Settings {
                maximum_rounds: 32,
                ..settings()
            },
            |height, _, _| Ok(Some(fixture.rebuild(height, None))),
            |_| {},
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(out.report.rounds.len() < 32);
        assert!(out.report.work <= settings().maximum_total_work);
        assert!(out.report.numerical_model_passed);
        assert!(matches!(
            refine(
                fixture.input(),
                Settings {
                    maximum_rounds: 33,
                    ..settings()
                },
                |_, _, _| panic!("Invalid limits must reject before rebuilding"),
                |_| {},
                || false
            ),
            Err(Failure::Invalid("transaction inputs"))
        ));
    }
    #[test]
    fn round_limit_preserves_inputs_when_rebuilding_exposes_another_intersection() {
        let mut fixture = Fixture::new(5);
        fixture.height = fixture.ground.clone();
        fixture.height[11] += 164;
        fixture.height[13] += 164;
        fixture.resample();
        let original = fixture.height.clone();
        let result = refine(
            fixture.input(),
            Settings {
                maximum_rounds: 1,
                ..settings()
            },
            |height, _, _| {
                Ok(Some(fixture.rebuild(
                    height,
                    Some(if height[11] == original[11] {
                        -250.
                    } else {
                        250.
                    }),
                )))
            },
            |_| {},
            || false,
        );
        let Err(Failure::RoundLimit { work, rounds }) = result else {
            panic!("Expected a private round-limit failure")
        };
        assert!(work <= settings().maximum_total_work);
        assert_eq!(rounds.len(), 2);
        assert_eq!(rounds[0].index, 0);
        let last = rounds.last().unwrap();
        assert_eq!(last.index, 1);
        assert!(last.maximum_mesh_grade.is_finite());
        assert!(
            last.grade_violations > 0
                || last.clearance_intersections > 0
                || last.source_intersections > 0
                || last.maximum_control_grade > f64::from(settings().fit.maximum_grade)
        );
        assert_eq!(fixture.height, original);
    }

    #[test]
    #[cfg(feature = "road-diagnostics")]
    fn retained_capture_requests_reject_before_rebuild() {
        if std::env::var_os("MAPPER_RETAINED_CAPTURE_TEST_CHILD").is_some() {
            let fixture = Fixture::new(3);
            let retained = crate::retained_rail_height::RetainedHeight::test_capture(
                grid(fixture.layout(), &fixture.height),
                &[0],
            );
            let result = refine(
                Input {
                    retained_height: Some(&retained),
                    ..fixture.input()
                },
                settings(),
                |_, _, _| panic!("Unconstrained capture must reject before rebuilding"),
                |_| {},
                || false,
            );
            assert!(matches!(
                result,
                Err(Failure::Invalid(
                    "road fit capture cannot omit retained height constraints"
                ))
            ));
            assert!(
                !std::path::PathBuf::from(std::env::var_os("MAPPER_ROAD_FIT_CAPTURE").unwrap())
                    .exists()
            );
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "road_refinement::tests::retained_capture_requests_reject_before_rebuild",
            ])
            .env("MAPPER_RETAINED_CAPTURE_TEST_CHILD", "1")
            .env("MAPPER_ROAD_FIT_CAPTURE", directory.path().join("capture"));
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!directory.path().join("capture").exists());
    }
    #[test]
    fn retained_height_survives_multiple_rebuilds_and_cannot_be_rebound_on_repair() {
        let mut fixture = Fixture::new(9);
        fixture.height = fixture.ground.clone();
        let left = 4 * 9 + 2;
        let right = 4 * 9 + 6;
        let held = 7 * 9 + 4;
        for i in [left, right, held] {
            fixture.height[i] += 164;
        }
        fixture.resample();
        let original = fixture.height.clone();
        let retained = crate::retained_rail_height::RetainedHeight::test_capture(
            grid(fixture.layout(), &fixture.height),
            &[held],
        );
        for strategy in [
            Strategy::Standard,
            Strategy::Local,
            Strategy::QuantizedLocal,
        ] {
            let output = refine_impl_strategy(
                Input {
                    retained_height: Some(&retained),
                    ..fixture.input()
                },
                settings(),
                None,
                strategy,
                |height, _, _| {
                    Ok(Some(fixture.rebuild(
                        height,
                        Some(if height[left] == original[left] {
                            -250.
                        } else {
                            250.
                        }),
                    )))
                },
                |_| {},
                || false,
            )
            .unwrap()
            .unwrap();
            assert!(output.report.fits.len() >= 2);
            assert_eq!(output.height[held], original[held]);
            assert!(
                output
                    .report
                    .fits
                    .iter()
                    .all(|f| f.retained_sample_entries == Some(1))
            );
            assert_eq!(output.report.retained_sample_entries, Some(1));
            assert_eq!(
                output.geometry.surfaces.report["refinement"]["retained_sample_entries"],
                1
            );
            assert!(output.report.work <= settings().maximum_total_work);
            assert_eq!(fixture.height, original);
        }
        let blocked = crate::retained_rail_height::RetainedHeight::test_capture(
            grid(fixture.layout(), &fixture.height),
            &[left],
        );
        let result = refine_local(
            Input {
                retained_height: Some(&blocked),
                ..fixture.input()
            },
            settings(),
            |height, _, _| Ok(Some(fixture.rebuild(height, Some(-250.)))),
            |_| {},
            || false,
        );
        assert!(matches!(
            result,
            Err(Failure::FittingRound {error,..}) if matches!(error.as_ref(),road_bed::Failure::ClearanceInfeasible{..})
        ));
        let mut changed = fixture.height.clone();
        changed[held] += 1;
        let result = refine(
            Input {
                retained_height: Some(&retained),
                initial_height: &changed,
                ..fixture.input()
            },
            settings(),
            |_, _, _| panic!("Drift must reject before rebuilding"),
            |_| {},
            || false,
        );
        assert!(matches!(result, Err(Failure::Rebuild(_))));
        assert_eq!(fixture.height, original);
    }
    #[test]
    fn clearance_repair_handles_an_intersection_exposed_by_rebuilding() {
        let mut fixture = Fixture::new(9);
        fixture.height = fixture.ground.clone();
        // Neither bump contributes to the sampled controls. The second becomes
        // visible only after the first repair, simulating a changed footprint.
        let left = 4 * 9 + 2;
        let right = 4 * 9 + 6;
        fixture.height[left] += 164;
        fixture.height[right] += 164;
        fixture.resample();
        let original = fixture.height.clone();
        let out = refine(
            fixture.input(),
            settings(),
            |height, _, _| {
                Ok(Some(fixture.rebuild(
                    height,
                    Some(if height[left] == original[left] {
                        -250.
                    } else {
                        250.
                    }),
                )))
            },
            |_| {},
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(out.report.fits.len() >= 2);
        assert!(!out.report.fits[0].control_supports_frozen);
        assert!(out.report.fits.iter().all(|f| !f.control_supports_frozen));
        assert_eq!(out.controls, fixture.controls);
        assert!(out.report.numerical_model_passed);
        assert_eq!(out.report.rounds.last().unwrap().clearance_intersections, 0);
        assert_eq!(fixture.height, original);
    }
    #[test]
    fn local_first_strategy_rebuilds_and_accounts_for_selection_and_fitting() {
        let fixture = Fixture::new(9);
        let original = fixture.height.clone();
        let output = refine_local(
            fixture.input(),
            settings(),
            |height, _, _| Ok(Some(fixture.rebuild(height, None))),
            |_| {},
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(output.report.engine, LOCAL_VERSION);
        assert!(output.report.numerical_model_passed);
        assert!(output.report.local_attempts.iter().any(|a| a.adopted));
        assert!(
            output
                .report
                .fits
                .iter()
                .any(|f| f.repair_sample_limit.is_some())
        );
        assert!(output.report.work <= settings().maximum_total_work);
        let local_work: usize = output.report.local_attempts.iter().map(|a| a.work).sum();
        assert!(output.report.work >= local_work as u64);
        assert_eq!(output.clearance.report["passed"], true);
        assert_eq!(fixture.height, original);
        let cancelled = std::cell::Cell::new(false);
        let output = refine_local(
            fixture.input(),
            settings(),
            |height, _, _| Ok(Some(fixture.rebuild(height, None))),
            |_| cancelled.set(true),
            || cancelled.get(),
        )
        .unwrap();
        assert!(output.is_none());
        assert_eq!(fixture.height, original);
    }
}
