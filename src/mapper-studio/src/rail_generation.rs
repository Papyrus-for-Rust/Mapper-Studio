//! Saved opt-in surface-rail composition. Complete world/runtime acceptance is
//! deliberately separate from the implemented terrain/network/mesh stages.
use crate::{
    Project, prefab_assets::Catalog, prefab_textures::Bindings, rail_bed_preview, rail_mesh_source,
    rail_mesh_stage, rail_preview, rail_source,
};
use papyrus_terrain::{
    monument_composition::Composed, placement::Solution, rail_approaches, rail_ring, rail_scene,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{path::Path, sync::Arc};
pub const VERSION: &str = "papyrus-surface-rail-composition-1";
pub const BEFORE_ROADS_VERSION: &str = "papyrus-surface-rail-before-roads-2";
/// Mapper search/earthwork allowances, separate from installation-derived policies.
/// Stage seeds are explicit after binding; changing the master does not rewrite them.
pub fn initial_settings(master: u32) -> Value {
    // Search raw terrain before fitting the track vertically. The final grade
    // constraint belongs to grade_fit and its continuous-curve validation.
    json!({
        "ring":{"seed":crate::derive_seed(master,"Rail ring"),"maximum_grade":1.0,"maximum_total_expansions":2000000,"maximum_attempts":64},
        "approaches":{"seed":crate::derive_seed(master,"Rail approaches"),"maximum_grade":1.0,"maximum_total_expansions":4000000,"maximum_geometry_work":32000000},
        "bed":{"maximum_work":512000000,"maximum_cut_fill":24.,
            "grade_fit":{"maximum_grade":0.15,"maximum_work":8000000,"maximum_attempts":8},
            "corridor":{"maximum_work":32000000,"maximum_depth":20,"maximum_chord":1.,"maximum_deviation":0.01,"maximum_ground_error":0.5},
            "branching":{"seed":crate::derive_seed(master,"Rail branches"),"maximum_grade":1.0,"maximum_expansions":8000000,"maximum_work":128000000,"maximum_branches":64},
            "siding":{"seed":crate::derive_seed(master,"Rail sidings"),"maximum_work":128000000,"maximum_sidings":64},
            "reservation_work":128000000,"meshes":{"maximum_work":32000000,"maximum_chunks":8192,"maximum_geometry_bytes":536870912},"gameplay_height_work":256000000}
    })
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub engine: String,
    pub source_sha256: String,
    pub mesh_sha256: String,
    pub settings: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default, rename = "force_small_maps")]
    _legacy_force_small_maps: bool,
    #[serde(default = "default_main_ring")]
    pub main_ring: bool,
    pub ring: rail_ring::Settings,
    pub approaches: rail_approaches::Settings,
    pub bed: rail_bed_preview::Query,
}
fn default_main_ring() -> bool { true }
fn hash(v: &str) -> bool {
    v.len() == 64
        && v.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
impl Recipe {
    fn settings_sha256(&self) -> Result<String, String> {
        let mut settings = self.settings.clone();
        settings.sort_all_objects();
        let bytes = serde_json::to_vec(&settings).map_err(|e| e.to_string())?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }

    pub(crate) fn verify_report(&self, report: &Value) -> Result<(), String> {
        if report["composition_engine"].as_str() != Some(self.engine.as_str())
            || report["source_sha256"].as_str() != Some(self.source_sha256.as_str())
            || report["mesh_source_sha256"].as_str() != Some(self.mesh_sha256.as_str())
            || report["recipe_settings_sha256"].as_str() != Some(self.settings_sha256()?.as_str())
        {
            return Err("Surface rail sources or settings differ from the recipe".into());
        }
        Ok(())
    }

    pub fn settings(&self) -> Result<Settings, String> {
        let settings: Settings =
            serde_json::from_value(self.settings.clone()).map_err(|e| e.to_string())?;
        settings.ring.validate()?;
        settings.approaches.validate()?;
        settings.bed.validate()?;
        if settings.bed.corridor.is_none()
            || settings.bed.grade_fit.is_none()
            || settings.bed.branching.is_none()
            || settings.bed.siding.is_none()
            || settings.bed.reservation_work.is_none()
            || settings.bed.meshes.is_none()
            || settings.bed.gameplay_height_work.is_none()
            || settings.bed.track_network.is_some()
        {
            return Err("Saved rail composition requires constrained beds, branches, sidings, reservations, meshes and gameplay height; authored graph integration is pending".into());
        }
        Ok(settings)
    }
    pub fn validate(&self) -> Result<(), String> {
        if ![VERSION, BEFORE_ROADS_VERSION].contains(&self.engine.as_str())
            || !hash(&self.source_sha256)
            || !hash(&self.mesh_sha256)
            || serde_json::to_vec(&self.settings)
                .map_err(|e| e.to_string())?
                .len()
                > 16384
        {
            return Err("Invalid saved surface rail recipe".into());
        }
        self.settings()?;
        Ok(())
    }
}
pub struct Pending {
    initial_placement: Arc<Vec<u8>>,
    corridor: papyrus_terrain::rail_corridor::Settings,
    source: Arc<rail_source::Source>,
    infrastructure: Arc<crate::infrastructure::Source>,
    meshes: Arc<rail_mesh_source::Source>,
    mesh_settings: rail_mesh_stage::Settings,
    height_work: usize,
}
pub(crate) enum Rejection {
    Message(String),
    Layout(String),
}
impl Rejection {
    pub(crate) fn message(self) -> String {
        match self {
            Self::Message(message) | Self::Layout(message) => message,
        }
    }
}
impl From<String> for Rejection {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}
impl From<&str> for Rejection {
    fn from(message: &str) -> Self {
        Self::Message(message.into())
    }
}
/// Private snapshot between the installed ring and later rail-layout stages.
/// Keep it on the generation worker inside its installation verification scope.
pub struct RingPlan {
    project: Project,
    sockets: rail_preview::Sockets,
    ring: rail_ring::Outcome,
    curve: papyrus_terrain::path_geometry::Path,
    original_curve: Option<papyrus_terrain::path_geometry::Path>,
    layout: Solution,
    initial_placement: Arc<Vec<u8>>,
    source: Arc<rail_source::Source>,
    infrastructure: Arc<crate::infrastructure::Source>,
    meshes: Arc<rail_mesh_source::Source>,
    settings: Settings,
    mesh_settings: rail_mesh_stage::Settings,
    height_work: usize,
    engine: String,
    deferred: bool,
    monument_sha256: String,
}
mod portals;
pub use portals::Portal;
impl RingPlan {
    /// Source-smoothed sample vertices available before approaches and beds.
    pub fn path(&self) -> &papyrus_terrain::path_geometry::Path {
        &self.curve
    }
    pub fn source(&self) -> &rail_source::Source {
        &self.source
    }
}
impl Pending {
    pub fn source(&self) -> &rail_source::Source {
        &self.source
    }
    pub fn infrastructure(&self) -> &crate::infrastructure::Source {
        &self.infrastructure
    }
}
pub fn before_roads(project: &Project) -> bool {
    project
        .rails
        .as_ref()
        .is_some_and(|r| r.engine == BEFORE_ROADS_VERSION)
}
/// Source gates shared by saved binding and preparation verification.
pub fn check_source(source: &rail_source::Source, size: u32) -> Result<(), String> {
    for kind in [
        rail_source::Kind::GenerateRailRing,
        rail_source::Kind::GenerateRailLayout,
        rail_source::Kind::GenerateRailBranching,
        rail_source::Kind::GenerateRailSiding,
        rail_source::Kind::GenerateRailTerrain,
        rail_source::Kind::GenerateRailTopology,
        rail_source::Kind::GenerateRailTexture,
        rail_source::Kind::GenerateRailMeshes,
    ] {
        let stage = source
            .stages
            .iter()
            .find(|s| s.kind == kind)
            .ok_or("Missing required source rail stage")?;
        // An explicit request for surface rails overrides vanilla's automatic
        // ring-size preference. All other source gates and fit checks still run.
        let gate_size = if kind == rail_source::Kind::GenerateRailRing {
            size.max(stage.minimum_world_size.unwrap_or(size))
        } else {
            size
        };
        if let Some(reason) = source.skip(stage, gate_size, false, true) {
            let size_detail = stage
                .minimum_world_size
                .filter(|minimum| size < *minimum)
                .map(|minimum| format!("; needs at least {minimum} m, current map is {size} m"))
                .unwrap_or_default();
            return Err(format!(
                "Requested rail stage {kind:?} unavailable: {reason}{size_detail}"
            ));
        }
    }
    check_profile(&source.profile)
}
pub(crate) fn check_profile(profile: &rail_source::Profile) -> Result<(), String> {
    if profile.ring.is_none()
        || profile.approaches.is_none()
        || profile.branching.is_none()
        || profile.siding.is_none()
        || profile.reservations.is_none()
        || profile.bed.is_none()
        || profile.mesh.is_none()
    {
        return Err("Prepare a complete supported surface rail source snapshot".into());
    }
    Ok(())
}
fn sources(
    project: &Project,
    catalog: &Catalog,
    bindings: &Bindings,
    recipe: &Recipe,
    cancelled: &impl Fn() -> bool,
) -> Result<
    (
        rail_source::Source,
        rail_mesh_source::Source,
        Arc<crate::infrastructure::Source>,
    ),
    String,
> {
    recipe.validate()?;
    let parent = crate::infrastructure::Source::cached_shared(
        catalog,
        project
            .roads
            .as_ref()
            .ok_or("Rails require bound road infrastructure")?,
        cancelled,
    )?;
    let data = crate::storage::local_data()?;
    let source = rail_source::Source::load(
        bindings.installation_root(),
        &crate::client_cache::pinned_document(
            &data,
            "papyrus-rail-cache",
            &recipe.source_sha256,
            "rails.json",
        )?,
        catalog,
        &parent,
        cancelled,
    )?;
    if source.sha256 != recipe.source_sha256 {
        return Err("Saved rail source digest changed".into());
    }
    check_source(&source, project.size)?;
    let meshes = rail_mesh_source::Source::load(
        bindings.installation_root(),
        &crate::client_cache::pinned_document(
            &data,
            "papyrus-rail-mesh-cache",
            &recipe.mesh_sha256,
            "meshes.json",
        )?,
        &source,
        cancelled,
    )?;
    if meshes.sha256 != recipe.mesh_sha256 {
        return Err("Saved rail mesh digest changed".into());
    }
    Ok((source, meshes, parent))
}
pub fn bind(
    project: &Project,
    catalog: &Catalog,
    bindings: &Bindings,
    source_sha256: String,
    mesh_sha256: String,
    settings: Value,
    cancelled: impl Fn() -> bool,
) -> Result<Recipe, String> {
    let recipe = binding_recipe(
        project,
        catalog,
        bindings,
        source_sha256,
        mesh_sha256,
        settings,
    )?;
    sources(project, catalog, bindings, &recipe, &cancelled)?;
    if cancelled() {
        return Err("Rail binding cancelled".into());
    }
    Ok(recipe)
}
pub(crate) fn bind_prepared(
    project: &Project,
    catalog: &Catalog,
    bindings: &Bindings,
    prepared: &crate::rail_extract::Prepared,
    settings: Value,
) -> Result<Recipe, String> {
    if project.roads.as_ref().map(|r| r.source_sha256.as_str())
        != Some(prepared.source.infrastructure_digest())
        || prepared.meshes.parent_digest() != prepared.source.sha256
    {
        return Err("Prepared rails belong to different infrastructure".into());
    }
    let recipe = binding_recipe(
        project,
        catalog,
        bindings,
        prepared.source.sha256.clone(),
        prepared.meshes.sha256.clone(),
        settings,
    )?;
    check_source(&prepared.source, project.size)?;
    Ok(recipe)
}
fn binding_recipe(
    project: &Project,
    catalog: &Catalog,
    bindings: &Bindings,
    source_sha256: String,
    mesh_sha256: String,
    settings: Value,
) -> Result<Recipe, String> {
    if project.roads.as_ref().is_some_and(|r| {
        r.staged.is_some()
            && !r.entrance_launch.is_some_and(|p| {
                p.engine == papyrus_terrain::road_network::LaunchEngine::GlobalEarthwork
            })
    }) {
        return Err("Enable global road earthwork before binding source-ordered rails".into());
    }
    let recipe = Recipe {
        engine: if project.roads.as_ref().is_some_and(|r| r.staged.is_some()) {
            BEFORE_ROADS_VERSION.into()
        } else {
            VERSION.into()
        },
        source_sha256,
        mesh_sha256,
        settings,
    };
    recipe.validate()?;
    if project.decoration.is_some() && project.objects.is_none() {
        return Err("Enable shared live objects before binding rails so placement reservations can reach decoration".into());
    }
    project.check_monument_assets(&catalog.sha256, &bindings.sha256)?;
    Ok(recipe)
}
/// Recovery preserves the accepted roster and changes one pose. Connector count
/// is therefore invariant across its retries even when their track geometry is
/// different. Count against the same verified sources consumed by preparation.
pub(crate) fn rebuild_approach_limit(
    project: &Project,
    layout: &Solution,
    catalog: &Catalog,
    bindings: &Bindings,
    cancelled: &impl Fn() -> bool,
) -> Result<usize, String> {
    let Some(recipe) = &project.rails else {
        return Ok(0);
    };
    let (source, _, infrastructure) = sources(project, catalog, bindings, recipe, cancelled)?;
    let mut count = 0;
    for outcome in layout.outcomes.iter().filter(|o| o.position.is_some()) {
        if cancelled() {
            return Err("Rail rebuild reservation cancelled".into());
        }
        let path = project
            .targets
            .iter()
            .find(|t| t.instance_id == outcome.id)
            .and_then(|t| t.prefab_path.as_ref())
            .ok_or("Missing recovery rail prefab")?;
        let prefab = infrastructure
            .prefabs
            .get(path)
            .filter(|p| p.complete)
            .ok_or("Incomplete recovery rail connectors")?;
        count += prefab
            .connectors
            .iter()
            .filter(|c| c.infrastructure_type == source.profile.rail_type)
            .count();
        if count > 256 {
            return Err("Recovery rail connector count exceeds its preparation bound".into());
        }
    }
    if let Some(rules) = &project.monument_rules {
        crate::monument_source::Source::cached_shared(rules, catalog, bindings, cancelled)?;
        if let Some(tunnels) = crate::tunnel_source::Source::for_recipe(
            bindings.installation_root(),
            catalog,
            rules,
            cancelled,
        )? {
            let maximum = tunnels
                .selectors
                .get("UpwardsFolder")
                .into_iter()
                .flat_map(|pool| &pool.prefabs)
                .filter_map(|path| tunnels.prefabs.get(path))
                .map(|prefab| {
                    prefab
                        .connectors
                        .iter()
                        .filter(|connector| {
                            connector.infrastructure_type == source.profile.rail_type
                        })
                        .count()
                })
                .max()
                .unwrap_or(0);
            count = count
                .saturating_add(maximum * crate::tunnel_generation::MAX_SURFACE_PORTALS)
                .min(256);
        }
    }
    Ok(count)
}
pub fn prepare(
    project: &Project,
    layout: &Solution,
    catalog: &Catalog,
    bindings: &Bindings,
    maps: &mut Composed,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Pending>, String> {
    let Some((plan, terrain, grid)) =
        prepare_ring_with_terrain(project, layout, catalog, bindings, maps, &cancelled)?
    else {
        return Ok(None);
    };
    finish_ring(&plan, &plan.sockets, terrain, grid, maps, &cancelled).map_err(Rejection::message)
}
/// Build the ring against the incoming terrain without publishing rail surfaces.
pub fn prepare_ring(
    project: &Project,
    layout: &Solution,
    catalog: &Catalog,
    bindings: &Bindings,
    maps: &Composed,
    cancelled: impl Fn() -> bool,
) -> Result<Option<RingPlan>, String> {
    Ok(prepare_ring_with_terrain(project, layout, catalog, bindings, maps, cancelled)?
        .map(|(plan, ..)| plan))
}

/// Leave a gap on the least useful portion of the source-routed alignment.
/// Routing later sees only the retained cells, so joins cannot target the gap.
fn open_main_line(
    cells: &mut Vec<[usize; 2]>,
    points: &mut Vec<[f32; 3]>,
    sockets: &rail_preview::Sockets,
) {
    cells.pop();
    points.pop();
    let n = points.len();
    let gap = (n / 8).max(1);
    let clearance = |start: usize| {
        (0..gap).step_by((gap / 8).max(1)).map(|offset| {
            let [x, _, z] = points[(start + offset) % n];
            sockets.sockets.iter()
                .map(|s| (x - s.position[0]).powi(2) + (z - s.position[2]).powi(2))
                .fold(f32::INFINITY, f32::min)
        }).fold(f32::INFINITY, f32::min)
    };
    let start = (0..n).step_by((n / 16).max(1))
        .max_by(|&a, &b| clearance(a).total_cmp(&clearance(b))).unwrap();
    let first = (start + gap) % n;
    cells.rotate_left(first);
    points.rotate_left(first);
    cells.truncate(n - gap);
    points.truncate(n - gap);
}

fn prepare_ring_with_terrain(
    project: &Project,
    layout: &Solution,
    catalog: &Catalog,
    bindings: &Bindings,
    maps: &Composed,
    cancelled: impl Fn() -> bool,
) -> Result<Option<(RingPlan, rail_preview::Terrain, papyrus_terrain::rail_costmap::Grid)>, String> {
    if cancelled() {
        return Ok(None);
    }
    let recipe = project.rails.as_ref().ok_or("Missing rail recipe")?;
    let (source, meshes, infrastructure) = sources(project, catalog, bindings, recipe, &cancelled)?;
    let mut settings = recipe.settings()?;
    let mesh_settings = settings.bed.meshes.take().unwrap();
    let height_work = settings.bed.gameplay_height_work.take().unwrap();
    let monument_source = crate::monument_source::Source::cached_shared(
        project
            .monument_rules
            .as_ref()
            .ok_or("Rails require verified monument blockers")?,
        catalog,
        bindings,
        &cancelled,
    )?;
    let Some(initial) = crate::monument_blockers::generate(
        project,
        layout,
        &monument_source,
        128000000,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let Some(terrain) =
        rail_preview::Terrain::from_composed(project, maps, layout, &initial.cells, &cancelled)?
    else {
        return Ok(None);
    };
    let Some(grid) = terrain.grid(&source.profile.costmap, &cancelled)? else {
        return Ok(None);
    };
    let sockets = terrain.sockets(
        Path::new(""),
        project,
        &infrastructure,
        source.profile.rail_type,
        &cancelled,
    )?;
    let mut ring = rail_ring::build(
        &grid,
        source.profile.ring.as_ref().unwrap(),
        settings.ring,
        &sockets
            .sockets
            .iter()
            .map(|s| [s.position[0], s.position[2]])
            .collect::<Vec<_>>(),
        &cancelled,
    )?;
    let points = match &mut ring {
        rail_ring::Outcome::Found { cells, points, .. } => {
            if !settings.main_ring {
                open_main_line(cells, points, &sockets);
            }
            points
        }
        rail_ring::Outcome::Cancelled => return Ok(None),
        other => return Err(format!("Required rail ring was not resolved: {other:?}")),
    };
    let curve = match papyrus_terrain::path_geometry::build(
        points,
        source
            .profile
            .path_settings(settings.approaches.maximum_geometry_work.min(32000000)),
        None,
        &cancelled,
    ) {
        Ok(Some(curve)) => curve,
        Ok(None) => return Ok(None),
        Err(error) => return Err(format!("Primary ring curve preparation failed: {error:?}")),
    };
    let original_curve = project.path_edits.iter().any(|edit| {
        edit.route.kind == crate::path_edit::Kind::Rail && edit.route.index == 0
    }).then(|| curve.clone());
    let Some(curve) = crate::path_edit::edit_rail(0, curve, &project.path_edits, &cancelled)?
    else { return Ok(None); };
    let plan = RingPlan {
        project: project.clone(),
        sockets,
        ring,
        curve,
        original_curve,
        layout: layout.clone(),
        initial_placement: Arc::new(initial.cells),
        source: Arc::new(source),
        infrastructure,
        meshes: Arc::new(meshes),
        settings,
        mesh_settings,
        height_work,
        engine: recipe.engine.clone(),
        deferred: before_roads(project),
        monument_sha256: monument_source.sha256.clone(),
    };
    Ok(Some((plan, terrain, grid)))
}
/// Rebuild later rail costs from the current private terrain after an intervening
/// stage. The recipe and original monument layout stay bound to the checkpoint.
pub fn resume(
    plan: RingPlan,
    project: &Project,
    layout: &Solution,
    maps: &mut Composed,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Pending>, String> {
    resume_with_rejection(&plan, None, project, layout, maps, cancelled).map_err(Rejection::message)
}
pub(crate) fn resume_with_rejection(
    plan: &RingPlan,
    sockets: Option<&rail_preview::Sockets>,
    project: &Project,
    layout: &Solution,
    maps: &mut Composed,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Pending>, Rejection> {
    let mut timing = crate::composition::StageTimer::start("rail_resume");
    if cancelled() {
        return Ok(None);
    }
    if *project != plan.project || *layout != plan.layout {
        return Err("Rail ring checkpoint differs from the recipe or monument layout".into());
    }
    let Some(terrain) = rail_preview::Terrain::from_composed(
        project,
        maps,
        layout,
        &plan.initial_placement,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    crate::composition::record_timing(&mut timing, "terrain");
    let Some(grid) = terrain.grid(&plan.source.profile.costmap, &cancelled)? else {
        return Ok(None);
    };
    crate::composition::record_timing(&mut timing, "costmap");
    finish_ring(plan, sockets.unwrap_or(&plan.sockets), terrain, grid, maps, cancelled)
}
fn finish_ring(
    plan: &RingPlan,
    sockets: &rail_preview::Sockets,
    terrain: rail_preview::Terrain,
    grid: papyrus_terrain::rail_costmap::Grid,
    maps: &mut Composed,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Pending>, Rejection> {
    let mut timing = crate::composition::StageTimer::start("rail_finish");
    if cancelled() {
        return Ok(None);
    }
    let RingPlan {
        ring,
        curve,
        original_curve,
        initial_placement,
        source,
        infrastructure,
        meshes,
        settings,
        mesh_settings,
        height_work,
        engine,
        deferred,
        ..
    } = plan;
    let approaches = rail_preview::approach_report_prepared(
        &grid,
        source,
        sockets,
        ring,
        settings.approaches,
        Some(original_curve.as_ref().map_or(
            rail_preview::ApproachCurve::Generated(curve),
            |original| rail_preview::ApproachCurve::Edited { original, revised: curve },
        )),
        &cancelled,
    )?;
    crate::composition::record_timing(&mut timing, "approaches");
    if approaches["result"]["status"] != "proposed" {
        return Err(Rejection::Layout(format!(
            "Rail approach search stopped. {}",
            approaches["result"]
        )));
    }
    if approaches["result"]["unresolved"]
        .as_array()
        .is_some_and(|missing| !missing.is_empty())
    {
        return Err(Rejection::Layout(format!(
            "Rail entrances could not be connected. {}",
            approaches["result"]["unresolved"]
        )));
    }
    let plan = json!({"source_sha256":source.sha256,"terrain_plan":{"manifest_sha256":terrain.manifest_digest(),"approaches":approaches}});
    let final_audit = settings
        .bed
        .corridor
        .ok_or("Missing requested final rail corridor audit")?;
    let Some(candidate) = rail_bed_preview::build_from_ring(
        &terrain,
        &grid,
        source,
        infrastructure,
        &plan,
        settings.bed.clone(),
        meshes,
        *deferred,
        &cancelled,
    )
    .map_err(Rejection::Layout)?
    else {
        return Ok(None);
    };
    drop(grid);
    crate::composition::record_timing(&mut timing, "bed");
    if cancelled() {
        return Ok(None);
    }
    let mut report = candidate.report;
    let recipe = terrain
        .project()
        .rails
        .as_ref()
        .ok_or("Missing rail recipe for the generated scene")?;
    report["composition_engine"] = json!(engine);
    report["execution"] = json!("normal composition preview");
    report["source_sha256"] = json!(source.sha256);
    report["mesh_source_sha256"] = json!(meshes.sha256);
    report["recipe_settings_sha256"] = json!(recipe.settings_sha256()?);
    report["world_generation_complete"] = json!(false);
    report["base_input_kind"] = json!("in-memory composed grids and accepted layout");
    let reservations = candidate
        .reservations
        .ok_or("Rail composition omitted required reservations")?;
    maps.layers = candidate.output.layers;
    maps.alpha = candidate.alpha;
    maps.ocean = Some(candidate.ocean);
    maps.rails = Some(rail_scene::Scene {
        report,
        tracks: candidate.output.tracks,
        meshes: Vec::new(),
        reservations,
    });
    Ok(Some(Pending {
        initial_placement: initial_placement.clone(),
        corridor: final_audit,
        source: source.clone(),
        infrastructure: infrastructure.clone(),
        meshes: meshes.clone(),
        mesh_settings: *mesh_settings,
        height_work: *height_work,
    }))
}
pub fn finish(
    pending: Pending,
    maps: &mut Composed,
    cancelled: impl Fn() -> bool,
) -> Result<bool, String> {
    if cancelled() {
        return Ok(false);
    }
    let scene = maps.rails.as_mut().ok_or("Missing prepared rail scene")?;
    let mut placement = Arc::unwrap_or_clone(pending.initial_placement);
    if let Some(objects) = &maps.objects {
        if objects.terrain.occupancy.len() != placement.len() {
            return Err("Final rail object occupancy dimensions differ".into());
        }
        for (i, (cell, &occupied)) in placement
            .iter_mut()
            .zip(&objects.terrain.occupancy)
            .enumerate()
        {
            if i % 4096 == 0 && cancelled() {
                return Ok(false);
            }
            *cell |= u8::from(occupied == 1);
        }
        if let Some(blocking) = &objects.terrain.blocking {
            if blocking.raster.len() != placement.len() {
                return Err("Final rail blocker dimensions differ".into());
            }
            for (i, cell) in placement.iter_mut().enumerate() {
                if i % 4096 == 0 && cancelled() {
                    return Ok(false);
                }
                *cell |= u8::from(blocking.raster[i] != 0 && scene.reservations.raster[i] == 0);
            }
        }
    }
    let shape = pending.source.profile.bed_shape()?;
    let policy = papyrus_terrain::rail_corridor::Policy {
        half_width: f64::from(shape.width) * 0.5,
        terrain_offset: f64::from(shape.terrain_offset),
        blocked_topology: pending.source.profile.costmap.blocked_topology
            | pending.source.profile.costmap.clearance_topology,
        protected_topology: shape.preserve_topology,
        water_topology: pending.source.profile.costmap.water_topology,
    };
    let corridor = pending.corridor;
    let mut auditor =
        papyrus_terrain::rail_corridor::Auditor::new(&maps.layers, &maps.alpha, Some(&placement))?;
    let mut audits = Vec::new();
    let mut work = 0;
    for track in &scene.tracks {
        let remaining = corridor.maximum_work.saturating_sub(work);
        if remaining == 0 {
            return Err("Final rail corridor allowance reached".into());
        }
        let Some(audit) = auditor.analyze(
            &track.path,
            policy,
            papyrus_terrain::rail_corridor::Settings {
                maximum_work: remaining,
                ..corridor
            },
            &cancelled,
        )?
        else {
            return Ok(false);
        };
        work += audit.work;
        audits.push(json!({"track_id":track.id,"audit":audit}));
    }
    drop(auditor);
    scene.report["corridor_before_objects"] = scene.report["corridor"].take();
    scene.report["corridor"] = json!({"tracks":audits,"work":work,"policy":policy,"settings":corridor,"stage":"after objects and final topology",
        "placement_scope":"initial monuments and committed object footprints; own rail reservations excluded",
        "overlapping_new_blocker_writes_not_separately_identifiable":true,"full_corridor_validated":false,"runtime_placement_map_reconstructed":false});
    let ids = |name: &str| -> Result<std::collections::BTreeSet<u64>, String> {
        scene.report[name]["fitted_joins"]
            .as_array()
            .ok_or("Missing fitted rail child roster")?
            .iter()
            .map(|v| v["track_id"].as_u64().ok_or("Invalid rail child ID".into()))
            .collect()
    };
    let branches = ids("branching")?;
    let sidings = ids("sidings")?;
    let flags = |id: u64| {
        if sidings.contains(&id) {
            let p = pending.source.profile.siding.as_ref().unwrap();
            (p.start_flag, p.end_flag)
        } else if branches.contains(&id) {
            let p = pending.source.profile.branching.as_ref().unwrap();
            (p.start_flag, p.end_flag)
        } else if id == 1 {
            let p = pending.source.profile.mesh.as_ref().unwrap();
            (p.ring_start_flag, p.ring_end_flag)
        } else {
            let p = pending.source.profile.mesh.as_ref().unwrap();
            (p.approach_start_flag, p.approach_end_flag)
        }
    };
    let tracks = scene
        .tracks
        .iter()
        .map(|t| {
            let (source_start_flag, source_end_flag) = flags(t.id);
            rail_mesh_stage::Track {
                id: t.id,
                path: &t.path,
                source_start_flag,
                source_end_flag,
            }
        })
        .collect::<Vec<_>>();
    let Some(geometry) = rail_mesh_stage::build(
        &pending.source,
        &pending.meshes,
        &maps.layers,
        &tracks,
        pending.mesh_settings,
        &cancelled,
    )?
    else {
        return Ok(false);
    };
    let Some(height) =
        rail_mesh_stage::gameplay_height(&maps.layers, &geometry, pending.height_work, &cancelled)?
    else {
        return Ok(false);
    };
    scene.report["meshes"] = geometry.report;
    scene.meshes = geometry.chunks;
    scene.report["gameplay_height"] = json!({"scope":"rail collision triangles","changes":height.report,"physics_cooking_validated":false});
    maps.gameplay_height = Some(if let Some(roads) = maps.gameplay_height.take() {
        let Some(combined) = papyrus_terrain::gameplay_height::combine(
            &maps.layers.height,
            roads,
            height,
            &cancelled,
        )?
        else {
            return Ok(false);
        };
        combined
    } else {
        height
    });
    Ok(!cancelled())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn small_map_override_preserves_source_gates_and_saved_settings() {
        let (_root, catalog, infrastructure, mut document) = crate::rail_source::tests::fixture();
        let mut profile: Value = serde_json::from_str(include_str!(
            "../../../compatibility/rails/client-0f703d43-reservations.json"
        ))
        .unwrap();
        for field in ["native_sha256", "scene_sha256", "code_sources"] {
            profile[field] = document["profile"][field].clone();
        }
        document["profile"] = profile;
        let mut source = rail_source::Source::decode(
            &serde_json::to_vec(&document).unwrap(),
            &catalog,
            &infrastructure,
        )
        .unwrap();
        check_source(&source, 6000).unwrap();
        check_source(&source, 4000).unwrap();
        let ring = source
            .stages
            .iter_mut()
            .find(|s| s.kind == rail_source::Kind::GenerateRailRing)
            .unwrap();
        assert_eq!(ring.minimum_world_size, Some(4250));
        ring.disabled = Some(true);
        assert!(
            check_source(&source, 4000)
                .unwrap_err()
                .contains("disabled in source")
        );
        let ring = source
            .stages
            .iter_mut()
            .find(|s| s.kind == rail_source::Kind::GenerateRailRing)
            .unwrap();
        ring.disabled = Some(false);
        ring.mode = 0;
        assert!(
            check_source(&source, 4000)
                .unwrap_err()
                .contains("not in server realm")
        );
        let mut recipe = self::recipe();
        assert!(recipe.settings().unwrap().main_ring);
        recipe.settings["force_small_maps"] = true.into();
        recipe.validate().unwrap();
        let restored: Recipe =
            serde_json::from_slice(&serde_json::to_vec(&recipe).unwrap()).unwrap();
        assert!(restored.settings().unwrap().main_ring);
        assert_eq!(restored, recipe);
        recipe.settings["force_small_maps"] = "yes".into();
        assert!(recipe.validate().is_err());
        recipe.settings.as_object_mut().unwrap().remove("force_small_maps");
        recipe.settings["main_ring"] = false.into();
        assert!(!recipe.settings().unwrap().main_ring);
    }
    #[test]
    fn workstation_settings_are_complete_deterministic_and_use_distinct_saved_seeds() {
        let settings = initial_settings(123);
        assert_eq!(settings, initial_settings(123));
        assert_ne!(settings, initial_settings(456));
        let recipe = Recipe {
            engine: VERSION.into(),
            source_sha256: "a".repeat(64),
            mesh_sha256: "b".repeat(64),
            settings,
        };
        recipe.validate().unwrap();
        let typed = recipe.settings().unwrap();
        let seeds = [
            typed.ring.seed,
            typed.approaches.seed,
            typed.bed.branching.unwrap().seed,
            typed.bed.siding.unwrap().seed,
        ];
        assert_eq!(
            seeds
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            4
        );
        assert!(seeds.iter().all(|s| (1..=crate::MAX_SEED).contains(s)));
        assert_eq!(typed.ring.maximum_grade, 1.);
        assert_eq!(typed.approaches.maximum_grade, 1.);
        assert_eq!(typed.bed.branching.unwrap().maximum_grade, 1.);
        assert_eq!(typed.bed.grade_fit.unwrap().maximum_grade, 0.15);
        assert_eq!(typed.bed.maximum_cut_fill, 24.);
        assert_eq!(typed.ring.maximum_total_expansions, 2_000_000);
        let historical = self::recipe();
        let saved = historical.clone();
        assert_eq!(historical.settings().unwrap().ring.maximum_grade, 0.15);
        assert_eq!(historical, saved);
        assert_eq!(
            serde_json::from_slice::<Recipe>(&serde_json::to_vec(&recipe).unwrap()).unwrap(),
            recipe
        );
    }
    pub(crate) fn recipe() -> Recipe {
        Recipe {
            engine: VERSION.into(),
            source_sha256: "a".repeat(64),
            mesh_sha256: "b".repeat(64),
            settings: json!({
            "ring":{"seed":1,"maximum_grade":0.15,"maximum_total_expansions":1000000,"maximum_attempts":32},
            "approaches":{"seed":2,"maximum_grade":0.15,"maximum_total_expansions":2000000,"maximum_geometry_work":32000000},
            "bed":{"maximum_work":512000000,"maximum_cut_fill":24,"grade_fit":{"maximum_grade":0.15,"maximum_work":8000000,"maximum_attempts":8},
                "corridor":{"maximum_work":32000000,"maximum_depth":20,"maximum_chord":1.,"maximum_deviation":0.01,"maximum_ground_error":0.5},
                "branching":{"seed":3,"maximum_grade":0.15,"maximum_expansions":8000000,"maximum_work":128000000,"maximum_branches":64},
                "siding":{"seed":4,"maximum_work":128000000,"maximum_sidings":64},"reservation_work":128000000,
                "meshes":{"maximum_work":32000000,"maximum_chunks":8192,"maximum_geometry_bytes":536870912},"gameplay_height_work":256000000}}),
        }
    }
    #[test]
    fn export_rail_report_is_bound_to_sources_seeds_and_bed_limits() {
        let original = recipe();
        let report = json!({
            "composition_engine": original.engine,
            "source_sha256": original.source_sha256,
            "mesh_source_sha256": original.mesh_sha256,
            "recipe_settings_sha256": original.settings_sha256().unwrap(),
        });
        original.verify_report(&report).unwrap();
        let mut reordered = original.clone();
        reordered.settings.sort_all_objects();
        reordered.verify_report(&report).unwrap();

        for field in [
            "composition_engine",
            "source_sha256",
            "mesh_source_sha256",
            "recipe_settings_sha256",
        ] {
            let mut missing = report.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(original.verify_report(&missing).is_err(), "missing {field}");
        }
        for pointer in [
            "/ring/seed",
            "/approaches/seed",
            "/bed/branching/seed",
            "/bed/siding/seed",
            "/bed/maximum_cut_fill",
        ] {
            let mut changed = original.clone();
            *changed.settings.pointer_mut(pointer).unwrap() = json!(7);
            assert!(changed.verify_report(&report).is_err(), "changed {pointer}");
        }
    }

    #[test]
    fn saved_recipe_roundtrip_preserves_terrain_identity_and_rejects_missing_stages() {
        let mut project = Project {
            preview_engine: crate::native_preview::VERSION.into(),
            installation: Some(crate::installation::Installation {
                kind: crate::installation::InstallationKind::Client,
                native: crate::installation::SourceFile {
                    relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                    sha256: "a".repeat(64),
                    bytes: 1,
                },
                scene_manifest: None,
            }),
            roads: Some(crate::infrastructure::Recipe::new("c".repeat(64))),
            monument_assets: Some(crate::AssetIdentity {
                catalog_sha256: "d".repeat(64),
                bindings_sha256: "e".repeat(64),
            }),
            monument_rules: Some(crate::monument_source::Recipe {
                engine: crate::monument_source::VERSION.into(),
                source_sha256: "f".repeat(64),
                coastal_profile: None,
                tier_search: None,
                earthwork: None,
                coastal_normals: None,
                coastal_navigation: None,
                coastal_search: None,
                canonical_yaw: None,
                land_alignment: None,
                surface_modifiers: None,
                stage_rules: None,
                dungeon_source: None,
                offshore_source: None,
            }),
            ..Default::default()
        };
        project.validate().unwrap();
        let terrain = project.terrain_key();
        project.rails = Some(recipe());
        project.validate().unwrap();
        assert_eq!(project.terrain_key(), terrain);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rails.mapper");
        project.save(&path).unwrap();
        let mut current = project.clone();
        crate::generation_defaults::apply(&mut current);
        assert_eq!(Project::load(&path).unwrap(), current);
        let profile = crate::profile::Profile::new("Rails".into(), project.clone());
        profile.validate().unwrap();
        let copy: crate::profile::Profile =
            serde_json::from_slice(&serde_json::to_vec(&profile).unwrap()).unwrap();
        assert_eq!(copy.recipe, project);
        let mut ordered = project.clone();
        ordered.rails.as_mut().unwrap().engine = BEFORE_ROADS_VERSION.into();
        assert!(
            ordered.validate().is_err(),
            "Rail-first recipes need staged global roads"
        );
        assert_eq!(
            ordered.rails.as_ref().unwrap().settings,
            project.rails.as_ref().unwrap().settings
        );
        assert_eq!(ordered.terrain_key(), terrain);
        ordered.rails.as_ref().unwrap().validate().unwrap();
        assert_eq!(
            serde_json::from_value::<Recipe>(
                serde_json::to_value(ordered.rails.as_ref().unwrap()).unwrap()
            )
            .unwrap(),
            ordered.rails.unwrap()
        );
        project.rails.as_mut().unwrap().settings["bed"]["reservation_work"] = Value::Null;
        assert!(project.validate().is_err());
        let mut invalid = recipe();
        invalid.settings["unknown_stage"] = json!(true);
        assert!(invalid.validate().is_err());
        let mut invalid = recipe();
        invalid.source_sha256 = "../source".into();
        assert!(invalid.validate().is_err());
    }
}
