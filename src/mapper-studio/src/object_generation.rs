//! Shared live execution of source-ordered road and static decoration handlers.
//! Other scene phases remain explicit and are not executed by this object pass.
use crate::{
    decor_source, infrastructure, object_workflow,
    prefab_assets::Catalog,
    road_object_connectors,
    road_object_placement::{self, DecorationStage, Emitter, Progress},
    road_object_source,
};
use papyrus_terrain::{
    decor_fit::{OrientedPose, ValidatedWater},
    decor_live,
    decor_stream::Budget,
    footprints::{self, ObjectPolicy, ProtectionScope, RoadTransaction},
    native_layers::Layers,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
pub const LEGACY_VERSION: &str = "papyrus-object-generation-2";
pub const VERSION: &str = "papyrus-object-generation-3";
/// Opt-in saved policy. Existing recipes continue using their original batch pass.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub engine: String,
    pub road_source_sha256: String,
    pub realm_mask: u32,
    pub maximum_terrain_work: usize,
    pub maximum_sampling_attempts: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uniform_cliffs_source_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coastal_cliffs_source_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jungle_vines_source_sha256: Option<String>,
}
impl Recipe {
    pub fn new(road_source_sha256: String) -> Self {
        Self {
            engine: VERSION.into(),
            road_source_sha256,
            realm_mask: 2,
            maximum_terrain_work: 512_000_000,
            maximum_sampling_attempts: 64_000_000,
            uniform_cliffs_source_sha256: None,
            coastal_cliffs_source_sha256: None,
            jungle_vines_source_sha256: None,
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.engine.as_str(), VERSION | LEGACY_VERSION)
            || self.road_source_sha256.len() != 64
            || !self
                .road_source_sha256
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            || self.realm_mask == 0
            || !(1..=1_024_000_000).contains(&self.maximum_terrain_work)
            || !(1..=64_000_000).contains(&self.maximum_sampling_attempts)
        {
            return Err("Invalid live object recipe or source fingerprint".into());
        }
        for pin in [&self.uniform_cliffs_source_sha256, &self.coastal_cliffs_source_sha256, &self.jungle_vines_source_sha256].into_iter().flatten() {
            if self.engine != VERSION || pin.len() != 64
                || !pin.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) {
                return Err("Invalid cliff recipe source".into());
            }
        }
        Ok(())
    }
}
/// Authored supports and water-stage terrain operations for newly bound recipes.
/// Earlier recipes retain their conservative root and water reservations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SceneryAdmission {
    Legacy,
    Authored,
}
impl SceneryAdmission {
    fn is_legacy(&self) -> bool {
        *self == Self::Legacy
    }
    fn protected_roles(self, flags: &BTreeMap<String, u32>, protected: u32) -> Result<u32, String> {
        if self.is_legacy() {
            return Ok(protected);
        }
        let mut water = 0;
        for role in ["Ocean", "River", "Lake"] {
            water |= *flags
                .get(role)
                .ok_or("Missing authored scenery water role")?;
        }
        Ok(protected & !water)
    }
}
pub struct Sources<'a> {
    pub utilities: Option<&'a crate::powerline_generation::Inputs>,
    pub early_cliff: Option<&'a EarlyCliff>,
    pub uniform_cliffs: Option<&'a decor_source::Source>,
    pub jungle_vines: Option<&'a decor_source::Source>,
    pub coastal_cliffs: Option<(&'a decor_source::Source, &'a [papyrus_terrain::coastal_cliffs::Program])>,
    pub height_reservations: Option<&'a papyrus_terrain::height_reservations::HeightReservations>,
    pub road: &'a road_object_source::Source,
    pub infrastructure: &'a infrastructure::Source,
    pub decoration: &'a decor_source::Source,
    pub catalog: &'a Catalog,
}
/// An installation-verified procedural stage prepared by the composition adapter.
pub struct EarlyCliff {
    pub(crate) object_id: i64,
    pub(crate) source_sha256: String,
    pub(crate) policy: papyrus_terrain::cliff_topology::Policy,
    pub(crate) keep_existing: bool,
    pub(crate) active: bool,
    pub(crate) provenance: serde_json::Value,
}
impl EarlyCliff {
    pub(crate) fn receipt(
        &self,
        report: Option<papyrus_terrain::cliff_topology::Report>,
        index_work: usize,
    ) -> Result<serde_json::Value, String> {
        let mut receipt = self.provenance.clone();
        receipt["executed"] = self.active.into();
        receipt["maximum_work"] = crate::cliff_topology_stage::MAXIMUM_WORK.into();
        receipt["index_work"] = index_work.into();
        receipt["report"] = serde_json::to_value(report).map_err(|e| e.to_string())?;
        Ok(receipt)
    }
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Settings {
    #[serde(skip_serializing_if = "SceneryAdmission::is_legacy")]
    pub scenery_admission: SceneryAdmission,
    pub road: road_object_placement::Settings,
    pub decoration_seed: u32,
    pub realm_mask: u32,
    pub maximum_terrain_work: usize,
    pub maximum_sampling_attempts: usize,
    pub maximum_road_height_change: f32,
}
#[derive(Serialize)]
pub struct Step {
    pub scene_order: usize,
    pub executed: bool,
    pub before: Progress,
    pub after: Progress,
}
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Origin {
    Road { fit_index: usize },
    Utility { fit_index: usize },
    Decoration { stage: usize, instance: usize },
}
#[derive(Serialize)]
pub struct Admission {
    pub id: usize,
    pub origin: Origin,
    pub prefab: String,
    pub transform: OrientedPose,
}
#[derive(Serialize)]
pub struct Connector {
    pub transaction_pose: usize,
    pub socket: road_object_connectors::Connector,
}
struct Resolved {
    admissions: Vec<Admission>,
    connectors: Vec<Connector>,
}
pub struct Generated {
    pub utility: Option<serde_json::Value>,
    pub early_cliff: Option<serde_json::Value>,
    pub engine: &'static str,
    pub plan: object_workflow::Plan,
    pub decoration_source_sha256: String,
    pub initial_blockers_sha256: Option<String>,
    pub settings: Settings,
    pub steps: Vec<Step>,
    pub road: road_object_placement::Report,
    pub decoration: Vec<decor_live::Report>,
    pub admissions: Vec<Admission>,
    pub connectors: Vec<Connector>,
    pub sampling_attempts: usize,
    pub uniform_cliff_sampling_attempts: Option<usize>,
    pub terrain: footprints::Output,
}
/// Verify all independently pinned inputs before enabling the shared composition pass.
pub fn bind(
    project: &crate::Project,
    catalog: &Catalog,
    bindings: &crate::prefab_textures::Bindings,
    road_digest: String,
    cancelled: impl Fn() -> bool,
) -> Result<Recipe, String> {
    let mut recipe = Recipe::new(road_digest);
    recipe.validate()?;
    let mut candidate = project.clone();
    candidate.objects = Some(recipe.clone());
    candidate.validate()?;
    project.check_monument_assets(&catalog.sha256, &bindings.sha256)?;
    let parent = infrastructure::Source::cached_shared(
        catalog,
        project
            .roads
            .as_ref()
            .ok_or("Bind roads before enabling live objects")?,
        &cancelled,
    )?;
    let road = road_object_source::Source::cached(
        bindings.installation_root(),
        &recipe.road_source_sha256,
        &parent,
        &cancelled,
    )?;
    let decoration = decor_source::Source::cached_shared(
        catalog,
        bindings,
        project
            .decoration
            .as_ref()
            .ok_or("Bind decoration before enabling live objects")?,
        &cancelled,
    )?;
    let monuments = crate::monument_source::Source::cached_shared(
        project
            .monument_rules
            .as_ref()
            .ok_or("Bind monuments before enabling live objects")?,
        catalog,
        bindings,
        &cancelled,
    )?;
    recipe.uniform_cliffs_source_sha256 = Some(
        crate::uniform_cliff_source::prepare(
            bindings.installation_root(),
            catalog,
            &road,
            &cancelled,
        )?
        .sha256()
        .to_owned(),
    );
    recipe.coastal_cliffs_source_sha256 = Some(crate::coastal_cliff_source::prepare(
        bindings.installation_root(), catalog, &road, &cancelled)?.sha256().to_owned());
    recipe.jungle_vines_source_sha256 = Some(crate::jungle_vine_source::prepare(
        bindings.installation_root(), catalog, &road, &cancelled)?.sha256().to_owned());
    recipe.validate()?;
    verify_prepared_sources(catalog, &road, &decoration, &monuments)?;
    if cancelled() {
        return Err("Live object binding cancelled".into());
    }
    Ok(recipe)
}
pub(crate) fn verify_prepared_sources(
    catalog: &Catalog,
    road: &road_object_source::Source,
    decoration: &decor_source::Source,
    monuments: &crate::monument_source::Source,
) -> Result<(), String> {
    if monuments.prefabs.values().any(|p| {
        p.placement_attributes
            .as_ref()
            .is_none_or(|a| a.placement_blockers.is_none() || !a.issues.is_empty())
    }) {
        return Err("Re-extract and bind monument attributes with verified blocker coverage before enabling live objects".into());
    }
    decoration.verify_road_binding(road)?;
    decoration.prepare_live(catalog)?;
    object_workflow::plan(road, &decoration.stages)?;
    Ok(())
}
/// Apply the saved live policy to a private, already stamped/road-shaped composition.
/// Cancellation or any source/fitting error leaves the caller's terrain untouched.
pub fn compose(
    project: &crate::Project,
    layout: &papyrus_terrain::placement::Solution,
    catalog: &Catalog,
    bindings: &crate::prefab_textures::Bindings,
    maps: &mut papyrus_terrain::monument_composition::Composed,
    cancelled: impl Fn() -> bool,
) -> Result<bool, String> {
    compose_with_height_reservations(project, layout, catalog, bindings, maps, None, cancelled)
}
#[allow(clippy::too_many_arguments)]
pub fn compose_with_height_reservations(
    project: &crate::Project,
    layout: &papyrus_terrain::placement::Solution,
    catalog: &Catalog,
    bindings: &crate::prefab_textures::Bindings,
    maps: &mut papyrus_terrain::monument_composition::Composed,
    height_reservations: Option<&papyrus_terrain::height_reservations::HeightReservations>,
    cancelled: impl Fn() -> bool,
) -> Result<bool, String> {
    let mut timing = crate::composition::StageTimer::start("object_composition");
    crate::composition::record_timing(&mut timing, "object_sources_begin");
    if cancelled() {
        return Ok(false);
    }
    let recipe = project
        .objects
        .as_ref()
        .ok_or("Missing live object recipe")?;
    recipe.validate()?;
    let road_recipe = project
        .roads
        .as_ref()
        .ok_or("Missing road recipe for live objects")?;
    let infrastructure = infrastructure::Source::cached_shared(catalog, road_recipe, &cancelled)?;
    let root = bindings.installation_root();
    let road = road_object_source::Source::cached(
        root,
        &recipe.road_source_sha256,
        &infrastructure,
        &cancelled,
    )?;
    let decoration = decor_source::Source::cached_shared(
        catalog,
        bindings,
        project
            .decoration
            .as_ref()
            .ok_or("Missing decoration recipe for live objects")?,
        &cancelled,
    )?;
    let monuments = crate::monument_source::Source::cached_shared(
        project
            .monument_rules
            .as_ref()
            .ok_or("Missing monument source for live objects")?,
        catalog,
        bindings,
        &cancelled,
    )?;
    let Some(mut initial) =
        crate::monument_blockers::generate(project, layout, &monuments, 128_000_000, &cancelled)?
    else {
        return Ok(false);
    };
    let rail_reservations = if crate::rail_generation::before_roads(project) {
        if maps
            .rails
            .as_ref()
            .is_none_or(|s| s.report["placement_reset_after_topology"] != true)
        {
            return Err(
                "Source-ordered object placement requires the post-topology placement reset".into(),
            );
        }
        Some(
            serde_json::json!({"pre_road_rail_mask_retired":true,"remaining_initial_scope":"monument collision subset"}),
        )
    } else if let Some(rails) = &maps.rails {
        let mask = &rails.reservations.raster;
        if mask.len() != initial.cells.len() || mask.iter().any(|v| *v > 1) {
            return Err("Rail reservation grid differs from live object terrain".into());
        }
        let monument_sha256 = format!("{:x}", Sha256::digest(&initial.cells));
        for (i, (dst, &rail)) in initial.cells.iter_mut().zip(mask).enumerate() {
            if i % 4096 == 0 && cancelled() {
                return Ok(false);
            }
            *dst |= rail;
        }
        Some(
            serde_json::json!({"rail_source_sha256":project.rails.as_ref().ok_or("Missing rail source recipe")?.source_sha256,
            "mask_sha256":format!("{:x}",Sha256::digest(mask)),"monument_mask_sha256":monument_sha256,
            "combined_initial_cells":initial.cells.iter().filter(|v|**v!=0).count(),"initial_blockers_report_scope":"monuments"}),
        )
    } else {
        None
    };
    let pipe_receipt = maps.roads.as_ref().and_then(|roads| roads.surfaces.as_ref())
        .map(|surfaces| &surfaces.report["staged_pipeline"]["pipes"])
        .unwrap_or(&serde_json::Value::Null);
    let Some(pipes_completed) = crate::water_treatment_stage::reserve_committed(
        pipe_receipt, &mut initial.cells, &cancelled,
    )? else {
        return Ok(false);
    };
    crate::water_treatment_stage::validate_committed(pipe_receipt, project)?;
    let road_report = maps
            .roads
            .as_ref()
            .ok_or("Missing composed roads for live objects")?
            .json();
    let roads = road_object_placement::roads_from_report(
        &road_report,
        infrastructure
            .profile
            .road_hierarchy
            .ok_or("Missing source road hierarchy")?,
        infrastructure.profile.width,
    )?;
    let early_cliff = crate::cliff_topology_stage::prepare_early(catalog, bindings, &road, &cancelled)?;
    let uniform_cliffs = recipe
        .uniform_cliffs_source_sha256
        .as_deref()
        .map(|pin| crate::uniform_cliff_source::cached(root, catalog, &road, pin, &cancelled))
        .transpose()?;
    let coastal_cliffs = recipe.coastal_cliffs_source_sha256.as_deref()
        .map(|pin| crate::coastal_cliff_source::cached(root, catalog, &road, pin, &cancelled)).transpose()?;
    let jungle_vines = recipe.jungle_vines_source_sha256.as_deref()
        .map(|pin| crate::jungle_vine_source::cached(root, catalog, &road, pin, &cancelled)).transpose()?;
    let utilities = if recipe.engine == VERSION {
        crate::powerline_generation::Inputs::prepare(
            crate::powerline_generation::Context {
                project,
                solution: layout,
                root,
                catalog,
                infrastructure: &infrastructure,
                road: &road,
                report: &road_report,
            },
            &cancelled,
        )?
    } else {
        None
    };
    crate::composition::record_timing(&mut timing, "object_sources_prepared");
    let Some(generated) = generate_with_options(
        &maps.layers,
        &maps.alpha,
        &roads,
        Sources {
            utilities: utilities.as_ref(),
            early_cliff: early_cliff.as_ref(),
            uniform_cliffs: uniform_cliffs.as_ref().map(|source| source.metadata()),
            jungle_vines: jungle_vines.as_ref().map(|source| source.metadata()),
            coastal_cliffs: coastal_cliffs.as_ref().map(|source| (source.metadata(), source.programs())),
            height_reservations,
            road: &road,
            infrastructure: &infrastructure,
            decoration: &decoration,
            catalog,
        },
        Settings {
            scenery_admission: if recipe.engine == VERSION {
                SceneryAdmission::Authored
            } else {
                SceneryAdmission::Legacy
            },
            road: road_object_placement::Settings {
                seed: project.seeds[3].value,
                maximum_work: 2_000_000,
                maximum_poses: 100_000,
            },
            decoration_seed: project.seeds[5].value,
            realm_mask: recipe.realm_mask,
            maximum_terrain_work: recipe.maximum_terrain_work,
            maximum_sampling_attempts: recipe.maximum_sampling_attempts,
            maximum_road_height_change: road_recipe.maximum_cut_fill,
        },
        Some(&initial.cells),
        (!project.options.is_default()).then_some(&project.options),
        &cancelled,
    )?
    else {
        return Ok(false);
    };
    if cancelled() {
        return Ok(false);
    }
    let Generated {
        utility,
        early_cliff,
        engine,
        plan,
        decoration_source_sha256,
        initial_blockers_sha256,
        decoration,
        admissions,
        connectors,
        terrain,
        road: road_fitting,
        ..
    } = generated;
    let instances = admissions
        .into_iter()
        .map(|a| {
            Ok(papyrus_terrain::object_scene::Instance {
                id: a.id,
                kind: match a.origin {
                    Origin::Road { .. } => papyrus_terrain::object_scene::Kind::Road,
                    Origin::Decoration { .. } => papyrus_terrain::object_scene::Kind::Decoration,
                    Origin::Utility { .. } => papyrus_terrain::object_scene::Kind::Utility,
                },
                road: match a.origin {
                    Origin::Road { fit_index } => Some(
                        road_fitting
                            .provisional_poses
                            .get(fit_index)
                            .ok_or("Missing road ownership for admitted prefab")?
                            .road,
                    ),
                    Origin::Decoration { .. } => None,
                    Origin::Utility { fit_index } => Some(road_fitting.utilities.as_ref()
                        .and_then(|report| report.provisional_poses.get(fit_index))
                        .ok_or("Missing utility ownership for admitted prefab")?.road),
                },
                prefab: a.prefab,
                pose: papyrus_terrain::decor_fit::Pose {
                    candidate: a.id,
                    position: a.transform.position,
                    rotation: a.transform.rotation,
                    scale: a.transform.scale,
                },
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let sockets = connectors
        .into_iter()
        .map(|c| papyrus_terrain::object_scene::Socket {
            instance: c.transaction_pose,
            prefab: c.socket.prefab,
            attribute: c.socket.attribute,
            infrastructure_type: c.socket.infrastructure_type,
            position: c.socket.position,
        })
        .collect();
    let external_phases = plan
        .steps
        .into_iter()
        .filter(|s| matches!(s.handler, object_workflow::Handler::External)
            && !(pipes_completed && s.class == "PlaceWaterTreatmentPipes"))
        .map(|s| papyrus_terrain::object_scene::PendingPhase {
            scene_order: s.scene_order,
            class: s.class,
            description: s.description,
        })
        .collect();
    let topology = terrain
        .topology
        .ok_or("Live object output is missing topology")?;
    let splat = terrain
        .splat
        .ok_or("Live object output is missing materials")?;
    let scene = papyrus_terrain::object_scene::Scene {
        utility,
        initial_rail_reservations: rail_reservations,
        engine,
        road_seed: project.seeds[3].value,
        decoration_seed: project.seeds[5].value,
        road_source_sha256: road.sha256,
        decoration_source_sha256,
        uniform_cliffs_source_sha256: recipe.uniform_cliffs_source_sha256.clone(),
        coastal_cliffs_source_sha256: recipe.coastal_cliffs_source_sha256.clone(),
        jungle_vines_source_sha256: recipe.jungle_vines_source_sha256.clone(),
        monument_source_sha256: monuments.sha256.clone(),
        initial_blockers_sha256,
        initial_blockers: initial.report,
        instances,
        sockets,
        decoration,
        terrain: terrain.report,
        external_phases,
        assembled_instances: 0,
        physics_validated: false,
    };
    // Publication follows complete fitting/admission and the final cancellation checkpoint.
    maps.layers.height = terrain.height;
    maps.layers.topology = topology;
    maps.layers.splat = splat;
    maps.decoration = None;
    maps.objects = Some(scene);
    maps.cliff_topology = early_cliff.map(|report| serde_json::json!({"early":report}));
    crate::composition::record_timing(&mut timing, "object_world_published");
    Ok(true)
}

pub fn generate(
    layers: &Layers,
    alpha: &[u8],
    roads: &[road_object_placement::Road],
    sources: Sources<'_>,
    settings: Settings,
    initial_blockers: Option<&[u8]>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Generated>, String> {
    generate_with_options(layers, alpha, roads, sources, settings, initial_blockers, None, cancelled)
}

#[allow(clippy::too_many_arguments)]
fn generate_with_options(
    layers: &Layers,
    alpha: &[u8],
    roads: &[road_object_placement::Road],
    sources: Sources<'_>,
    settings: Settings,
    initial_blockers: Option<&[u8]>,
    options: Option<&crate::recipe_options::Options>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Generated>, String> {
    let mut timing = crate::composition::StageTimer::start("object_generation");
    crate::composition::record_timing(&mut timing, "object_generation_begin");
    if cancelled() {
        return Ok(None);
    }
    let mut decoration_stages = sources.decoration.stages.clone();
    if let Some(vines) = sources.jungle_vines {
        vines.verify_road_binding(sources.road)?;
        for mut stage in vines.stages.clone() {
            stage.order = decoration_stages.len();
            decoration_stages.push(stage);
        }
    }
    if let Some(options) = options {
        for stage in &mut decoration_stages {
            stage.prefabs.retain(|path| !options.export_blocked(path));
        }
    }
    let mut plan = object_workflow::plan(sources.road, &decoration_stages)?;
    if let Some(utility) = sources.utilities {
        utility.bind_plan(&mut plan)?;
    }
    if let Some(early) = sources.early_cliff {
        bind_early_cliff(&mut plan, early)?;
    }
    if sources.road.infrastructure_sha256 != sources.infrastructure.sha256
        || sources.infrastructure.catalog_sha256 != sources.catalog.sha256
        || settings.realm_mask == 0
        || !settings.maximum_road_height_change.is_finite()
        || !(0. ..=24.).contains(&settings.maximum_road_height_change)
    {
        return Err("Mismatched live object sources or realm".into());
    }
    sources.decoration.verify_road_binding(sources.road)?;
    let mut prepared = match settings.scenery_admission {
        SceneryAdmission::Legacy => sources.decoration.prepare_live(sources.catalog)?,
        SceneryAdmission::Authored => sources.decoration.prepare_authored_live(sources.catalog)?,
    };
    if let Some(vines) = sources.jungle_vines {
        let vines = vines.prepare_authored_live(sources.catalog)?;
        if vines.topology_flags != prepared.topology_flags
            || vines.fitting.keys().any(|path| prepared.fitting.contains_key(path))
            || vines.terrain.keys().any(|path| prepared.terrain.contains_key(path))
        {
            return Err("Vine-tree and scenery inputs conflict".into());
        }
        prepared.fitting.extend(vines.fitting);
        prepared.terrain.extend(vines.terrain);
    }
    let flags = sources
        .infrastructure
        .profile
        .topology_flags
        .as_ref()
        .ok_or("Missing road topology roles")?;
    if flags != &prepared.topology_flags {
        return Err("Road and decoration topology roles differ".into());
    }
    let mut cliff_prepared = sources
        .uniform_cliffs
        .map(|source| {
            source.verify_road_binding(sources.road)?;
            bind_uniform_cliffs(&mut plan, &source.stages)?;
            source.prepare_authored_live(sources.catalog)
        })
        .transpose()?;
    let cliff_priorities = sources
        .uniform_cliffs
        .map(|source| source.cliff_priorities())
        .transpose()?
        .unwrap_or_default();
    if cliff_prepared
        .as_ref()
        .is_some_and(|p| p.topology_flags != prepared.topology_flags)
    {
        return Err("Uniform cliffs and scenery topology roles differ".into());
    }
    let mut coastal_prepared = sources
        .coastal_cliffs
        .map(|(source, programs)| {
            source.verify_road_binding(sources.road)?;
            bind_coastal_cliffs(&mut plan, programs, &source.stages)?;
            source.prepare_authored_live(sources.catalog)
        })
        .transpose()?;
    let coastal_placements = sources
        .coastal_cliffs
        .map(|(source, _)| source.coastal_prefabs())
        .transpose()?
        .unwrap_or_default();
    if coastal_prepared
        .as_ref()
        .is_some_and(|p| p.topology_flags != prepared.topology_flags)
    {
        return Err("Coastal cliffs and scenery topology roles differ".into());
    }
    let road_protected = sources.infrastructure.profile.protected_topology as u32;
    let road_scope = ProtectionScope {
        namespace: "road".into(),
        protected_topology: road_protected,
        role_exceptions: 0,
        material_role_exceptions: 0,
        maximum_height_change: Some(settings.maximum_road_height_change),
    };
    let scenery_protected = settings
        .scenery_admission
        .protected_roles(&prepared.topology_flags, prepared.protected_topology)?;
    let decor_scope = ProtectionScope {
        namespace: "decoration".into(),
        protected_topology: scenery_protected,
        role_exceptions: prepared.placement_role_exceptions & scenery_protected,
        material_role_exceptions: prepared.placement_material_role_exceptions & scenery_protected,
        maximum_height_change: None,
    };
    let mut specs = BTreeMap::new();
    for (path, prefab) in std::mem::take(&mut prepared.terrain) {
        specs.insert(format!("decoration:{path}"), prefab);
    }
    if let Some(cliffs) = cliff_prepared.as_mut() {
        for (path, prefab) in std::mem::take(&mut cliffs.terrain) {
            specs.insert(format!("cliff:{path}"), prefab);
        }
    }
    if let Some(coastal) = coastal_prepared.as_mut() {
        for (path, prefab) in std::mem::take(&mut coastal.terrain) {
            specs.insert(format!("coastal:{path}"), prefab);
        }
    }
    for (path, prefab) in &sources.road.prefabs {
        if cancelled() {
            return Ok(None);
        }
        specs.insert(
            format!("road:{path}"),
            decor_source::compile_road_effects(prefab, flags, road_protected)?,
        );
    }
    if let Some(utility) = sources.utilities {
        for (path, prefab) in &utility.objects.prefabs {
            specs.insert(
                format!("utility:{path}"),
                decor_source::compile_road_effects(prefab, flags, road_protected)?,
            );
        }
    }
    let mut attempts = Budget::new(settings.maximum_sampling_attempts)?;
    // PlaceCliffsUniform has its own multiscale walk. Keep the existing
    // PlaceDecor allowance available for the ordinary scenery programs.
    let mut cliff_attempts = Budget::new(128_000_000)?;
    let mut coastal_attempts = Budget::new(128_000_000)?;
    let Some(mut transaction) = RoadTransaction::new_objects_with_policy(
        layers,
        alpha,
        specs,
        prepared.known_topology,
        prepared.protected_topology | road_protected,
        ObjectPolicy {
            maximum_work: settings.maximum_terrain_work,
            role_exceptions: 0,
            material_role_exceptions: 0,
        },
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    if let Some(reservations) = sources.height_reservations
        && !transaction.set_height_reservations(reservations, &cancelled)?
    {
        return Ok(None);
    }
    if let Some(blockers) = initial_blockers
        && transaction
            .initialize_blocking(blockers, &cancelled)?
            .is_none()
    {
        return Ok(None);
    }
    let Some(water) = ValidatedWater::global_ocean(layers, prepared.ocean_mask, &cancelled)?
    else {
        return Ok(None);
    };
    let Some(mut emitter) = Emitter::new(
        layers,
        alpha,
        roads,
        sources.road,
        settings.road,
        Some(transaction),
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    emitter = emitter.with_prefab_options(options);
    if settings.scenery_admission == SceneryAdmission::Authored {
        // Hill cliffs search 112 orientations at every eligible multiscale point.
        // Their fitting reads share one bounded allowance with the later scenery.
        let maximum_samples = fitting_allowance(
            layers.size,
            sources.uniform_cliffs.is_some() || sources.coastal_cliffs.is_some(),
        )?;
        emitter = emitter.with_fitting_limit(maximum_samples)?;
    }
    if let Some(utility) = sources.utilities {
        let Some(prepared) = emitter.with_utilities(
            &utility.objects,
            road_object_placement::Settings {
                seed: settings.decoration_seed,
                maximum_work: 2_000_000,
                maximum_poses: 100_000,
            },
        )?
        else {
            return Ok(None);
        };
        emitter = prepared;
    }
    let cliff_count = sources.uniform_cliffs.map_or(0, |s| s.stages.len());
    let coastal_count = sources.coastal_cliffs.map_or(0, |(_, p)| p.len());
    emitter.begin_workflow(
        decoration_stages.len(),
        cliff_count,
        coastal_count,
        settings.realm_mask,
    )?;
    let progress = crate::loading_progress::begin("Placing scenery", None);
    let reporter = progress.reporter();
    let mut decoration = Vec::with_capacity(sources.decoration.stages.len() + cliff_count);
    let mut cliffs = Vec::with_capacity(cliff_count);
    let mut steps = Vec::with_capacity(plan.steps.len());
    let mut early_cliff = None;
    let mut utility_report = None;
    crate::composition::record_timing(&mut timing, "object_generation_prepared");
    for step in &plan.steps {
        if cancelled() {
            return Ok(None);
        }
        let before = emitter.progress();
        let executed = match step.handler {
            object_workflow::Handler::RoadObjects { source_index } => {
                reporter.stage("Placing roadside objects");
                emitter.use_scope(road_scope.clone())?;
                if emitter.emit_stage(source_index)?.is_none() {
                    return Ok(None);
                }
                true
            }
            object_workflow::Handler::Decoration { source_index } => {
                reporter.stage("Placing scenery");
                emitter.use_scope(decor_scope.clone())?;
                let Some(report) = emitter.emit_decoration(DecorationStage {
                    stage: &decoration_stages[source_index],
                    index: source_index,
                    seed: settings.decoration_seed,
                    realm: settings.realm_mask,
                    prefabs: &prepared.fitting,
                    attempts: &mut attempts,
                    water: Some(water),
                })?
                else {
                    return Ok(None);
                };
                decoration.push(report);
                true
            }
            object_workflow::Handler::CoastalCliffs { source_index } => {
                reporter.stage("Placing coastal cliffs");
                let (_, programs) = sources
                    .coastal_cliffs
                    .ok_or("Missing coastal cliff programs")?;
                let prepared = coastal_prepared
                    .as_ref()
                    .ok_or("Missing prepared coastal cliffs")?;
                let program = &programs[source_index];
                let mut scope = decor_scope.clone();
                scope.namespace = "coastal".into();
                emitter.use_scope(scope)?;
                let Some(report) = emitter.emit_coastal(
                    DecorationStage {
                        stage: &program.stage,
                        index: source_index,
                        seed: settings.decoration_seed,
                        realm: settings.realm_mask,
                        prefabs: &prepared.fitting,
                        attempts: &mut coastal_attempts,
                        water: Some(water),
                    },
                    program,
                    &coastal_placements,
                )?
                else {
                    return Ok(None);
                };
                cliffs.push(report);
                true
            }
            object_workflow::Handler::UniformCliffs { source_index } => {
                reporter.stage("Placing cliffs");
                let source = sources
                    .uniform_cliffs
                    .ok_or("Missing bound uniform cliff source")?;
                let prepared = cliff_prepared
                    .as_ref()
                    .ok_or("Missing compiled uniform cliffs")?;
                let mut scope = decor_scope.clone();
                scope.namespace = "cliff".into();
                emitter.use_scope(scope)?;
                let Some(report) = emitter.emit_cliffs(
                    DecorationStage {
                        stage: &source.stages[source_index],
                        index: source_index,
                        seed: settings.decoration_seed,
                        realm: settings.realm_mask,
                        prefabs: &prepared.fitting,
                        attempts: &mut cliff_attempts,
                        water: Some(water),
                    },
                    &cliff_priorities,
                )?
                else {
                    return Ok(None);
                };
                cliffs.push(report);
                true
            }
            object_workflow::Handler::CliffTopology => {
                let early = sources
                    .early_cliff
                    .ok_or("Missing bound early cliff stage")?;
                let report = if early.active {
                    let Some(report) =
                        emitter.apply_cliff_topology(early.policy, early.keep_existing)?
                    else {
                        return Ok(None);
                    };
                    Some(report)
                } else {
                    None
                };
                let index_work = if early.active {
                    layers.surface_resolution.pow(2)
                } else {
                    0
                };
                early_cliff = Some(early.receipt(report, index_work)?);
                early.active
            }
            object_workflow::Handler::External => false,
            object_workflow::Handler::UtilityLayout => {
                let utility=sources.utilities.ok_or("Missing utility layout inputs")?;
                if utility.layout_active {
                    let Some((report,paths))=utility.generate(&emitter,&cancelled)? else {return Ok(None);};
                    if emitter.set_utility_paths(paths)?.is_none() {return Ok(None);}
                    utility_report=Some(serde_json::to_value(report).map_err(|error|error.to_string())?);
                } else {
                    if emitter.set_utility_paths(vec![])?.is_none() {return Ok(None);}
                    utility_report=Some(serde_json::json!({"engine":crate::powerline_generation::VERSION,
                        "objects_source_sha256":utility.objects.sha256,"capture_sha256":utility.capture_sha256,
                        "executed":false,"paths":[]}));
                }
                utility.layout_active
            }
            // The selected source's topology stage currently has an empty body.
            object_workflow::Handler::UtilityTopology => step.component_enabled && step.hierarchy_active
                && !step.disabled.unwrap_or(false) && step.mode as u32 & settings.realm_mask != 0,
            object_workflow::Handler::UtilityObjects {source_index} => {
                let mut scope=road_scope.clone(); scope.namespace="utility".into();
                emitter.use_scope(scope)?;
                if emitter.emit_utility_stage(source_index)?.is_none() {return Ok(None);}
                true
            }
        };
        steps.push(Step {
            scene_order: step.scene_order,
            executed,
            before,
            after: emitter.progress(),
        });
        if timing.is_some() {
            crate::composition::record_timing(
                &mut timing,
                &format!("object_{}_{}", step.scene_order, step.class),
            );
        }
    }
    let Some((road, terrain)) = emitter.finish()? else {
        return Ok(None);
    };
    let terrain = terrain.ok_or("Missing shared object terrain")?;
    crate::composition::record_timing(&mut timing, "object_generation_fitted");
    if timing.is_some() {
        use std::io::Write;
        let mut stderr = std::io::stderr().lock();
        for report in &cliffs {
            if let Some(reservations) = report.cliff_reservations {
                let event = serde_json::json!({
                    "stage_id": report.sampling.id,
                    "stage": report.sampling.name,
                    "fitting_attempts": report.fitting_attempts,
                    "reservations": reservations,
                });
                let _ = writeln!(stderr, "MAPPER_CLIFF_RESERVATIONS {event}");
            }
        }
    }
    decoration.extend(cliffs);
    let Some(Resolved {
        admissions,
        connectors,
    }) = resolve_admissions(
        &road,
        &decoration,
        &terrain.report,
        sources.road,
        sources.utilities.map(|utility| &utility.objects),
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(Generated {
        utility: utility_report,
        early_cliff,
        engine: if settings.scenery_admission.is_legacy() {
            LEGACY_VERSION
        } else {
            VERSION
        },
        plan,
        decoration_source_sha256: sources.decoration.sha256.clone(),
        initial_blockers_sha256: initial_blockers.map(|b| format!("{:x}", Sha256::digest(b))),
        settings,
        steps,
        road,
        decoration,
        admissions,
        connectors,
        sampling_attempts: attempts.used(),
        uniform_cliff_sampling_attempts: sources.uniform_cliffs.map(|_| cliff_attempts.used()),
        terrain,
    }))
}
fn fitting_allowance(size: u32, uniform_cliffs: bool) -> Result<usize, String> {
    papyrus_terrain::native_layers::dimensions(size)?;
    let samples = if uniform_cliffs {
        // Full multiscale searches grow with world area. This accounts for their
        // repeated anchor neighborhoods while remaining bounded at native sizes.
        500 * u64::from(size).pow(2)
    } else {
        256_000_000
    };
    usize::try_from(samples).map_err(|_| "Hill-cliff fitting requires a 64-bit build".into())
}
fn bind_coastal_cliffs(
    plan: &mut object_workflow::Plan,
    programs: &[papyrus_terrain::coastal_cliffs::Program],
    stages: &[papyrus_terrain::decor::Stage],
) -> Result<(), String> {
    let entries: Vec<_> = plan
        .steps
        .iter_mut()
        .filter(|s| s.class == "PlaceCliffs")
        .collect();
    if entries.is_empty() || entries.len() != programs.len() || stages.len() != programs.len() {
        return Err("Coastal cliff roster differs from workflow".into());
    }
    for (index, ((step, program), stage)) in
        entries.into_iter().zip(programs).zip(stages).enumerate()
    {
        program.validate()?;
        if stage.order != index
            || stage.id.parse::<i64>().ok() != Some(step.object_id)
            || stage.name != step.description
            || stage.mode != step.mode
            || stage.enabled != (step.hierarchy_active && !step.disabled.unwrap_or(false))
            || serde_json::to_value(stage).map_err(|e| e.to_string())?
                != serde_json::to_value(&program.stage).map_err(|e| e.to_string())?
            || !matches!(step.handler, object_workflow::Handler::External)
        {
            return Err("Coastal cliff program differs from workflow".into());
        }
        step.handler = object_workflow::Handler::CoastalCliffs {
            source_index: index,
        };
    }
    Ok(())
}
fn bind_uniform_cliffs(
    plan: &mut object_workflow::Plan,
    stages: &[papyrus_terrain::decor::Stage],
) -> Result<(), String> {
    let entries: Vec<_> = plan
        .steps
        .iter_mut()
        .filter(|s| s.class == "PlaceCliffsUniform")
        .collect();
    if entries.is_empty() || entries.len() != stages.len() {
        return Err("Uniform cliff roster differs from the object workflow".into());
    }
    for (index, (step, stage)) in entries.into_iter().zip(stages).enumerate() {
        stage.validate()?;
        if stage.order != index
            || stage.id.parse::<i64>().ok() != Some(step.object_id)
            || stage.kind != papyrus_terrain::decor::Kind::Uniform
            || stage.name != step.description
            || stage.mode != step.mode
            || stage.enabled != (step.hierarchy_active && !step.disabled.unwrap_or(false))
            || !matches!(step.handler, object_workflow::Handler::External)
        {
            return Err("Uniform cliff stage differs from the object workflow".into());
        }
        step.handler = object_workflow::Handler::UniformCliffs {
            source_index: index,
        };
    }
    Ok(())
}
fn bind_early_cliff(plan: &mut object_workflow::Plan, early: &EarlyCliff) -> Result<(), String> {
    if plan.road_source_sha256 != early.source_sha256 {
        return Err("Early cliff stage differs from the live object source".into());
    }
    let step = plan
        .steps
        .iter_mut()
        .find(|s| s.object_id == early.object_id)
        .ok_or("Early cliff stage is absent from the object workflow")?;
    if step.class != "GenerateCliffTopology"
        || !matches!(step.handler, object_workflow::Handler::External)
    {
        return Err("Early cliff stage resolves to a different procedural handler".into());
    }
    step.handler = object_workflow::Handler::CliffTopology;
    Ok(())
}
fn resolve_admissions(
    road: &road_object_placement::Report,
    decoration: &[decor_live::Report],
    terrain: &footprints::Report,
    source: &road_object_source::Source,
    utility_source: Option<&road_object_source::Source>,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Resolved>, String> {
    let ids = road
        .global_admission_ids
        .as_ref()
        .ok_or("Missing global road admission mapping")?;
    let count = road
        .global_admission_count
        .ok_or("Missing global admission count")?;
    if ids.len() != road.provisional_poses.len()
        || ids.iter().any(|id| *id >= count)
        || ids.windows(2).any(|w| w[0] >= w[1])
    {
        return Err("Incomplete global road admission mapping".into());
    }
    let mut by_id = BTreeMap::new();
    for (fit_index, &id) in ids.iter().enumerate() {
        if fit_index % 128 == 0 && cancelled() {
            return Ok(None);
        }
        let p = &road.provisional_poses[fit_index];
        if by_id
            .insert(
                id,
                Admission {
                    id,
                    origin: Origin::Road { fit_index },
                    prefab: p.prefab.clone(),
                    transform: p.transform,
                },
            )
            .is_some()
        {
            return Err("Duplicate global road admission ID".into());
        }
    }
    let mut decor_ids = std::collections::BTreeSet::new();
    if road.utilities.is_some() != utility_source.is_some() {
        return Err("Utility report and source coverage differ".into());
    }
    if let Some(utility) = &road.utilities {
        let ids = utility
            .global_admission_ids
            .as_ref()
            .ok_or("Missing utility admission mapping")?;
        if utility.global_admission_count != Some(count)
            || ids.len() != utility.provisional_poses.len()
            || ids.windows(2).any(|pair| pair[0] >= pair[1])
            || utility_source.is_none_or(|source| source.sha256 != utility.source_sha256)
        {
            return Err("Incomplete utility admission mapping".into());
        }
        for (fit_index, &id) in ids.iter().enumerate() {
            if fit_index % 128 == 0 && cancelled() {
                return Ok(None);
            }
            let pose = &utility.provisional_poses[fit_index];
            if id >= count
                || by_id
                    .insert(
                        id,
                        Admission {
                            id,
                            origin: Origin::Utility { fit_index },
                            prefab: pose.prefab.clone(),
                            transform: pose.transform,
                        },
                    )
                    .is_some()
            {
                return Err("Duplicate utility admission identity".into());
            }
        }
    }
    for (stage, report) in decoration.iter().enumerate() {
        for (instance, p) in report.admitted.iter().enumerate() {
            if instance % 128 == 0 && cancelled() {
                return Ok(None);
            }
            let id = p.transaction_pose;
            if id >= count {
                return Err("Decoration ID exceeds the shared admission count".into());
            }
            if by_id
                .insert(
                    id,
                    Admission {
                        id,
                        origin: Origin::Decoration { stage, instance },
                        prefab: p.prefab.clone(),
                        transform: p.transform,
                    },
                )
                .is_some()
            {
                return Err("Duplicate global decoration admission ID".into());
            }
            decor_ids.insert(id);
        }
    }
    let mut admissions = Vec::with_capacity(terrain.applied.len());
    let mut connectors = Vec::new();
    for applied in &terrain.applied {
        if cancelled() {
            return Ok(None);
        }
        let row = by_id
            .remove(&applied.pose)
            .ok_or("Unmapped or duplicate committed object ID")?;
        match row.origin {
            Origin::Road { fit_index } | Origin::Utility { fit_index } => {
                let source=if matches!(row.origin,Origin::Utility {..}) {
                    utility_source.ok_or("Missing committed utility source")?
                } else {source};
                for connector in road_object_connectors::local(
                    source
                        .prefabs
                        .get(&row.prefab)
                        .ok_or("Unknown committed road prefab")?,
                )? {
                    if connectors.len() == 200000 {
                        return Err("Mixed object connector allowance exhausted".into());
                    }
                    connectors.push(Connector {
                        transaction_pose: row.id,
                        socket: road_object_connectors::world(
                            fit_index,
                            &row.prefab,
                            &connector,
                            &row.transform,
                        )?,
                    });
                }
            }
            Origin::Decoration { .. } => {
                decor_ids.remove(&row.id);
            }
        }
        admissions.push(row);
    }
    if !decor_ids.is_empty()
        || terrain.counts.accepted != admissions.len()
        || road.global_admission_count != Some(terrain.counts.attempted)
        || admissions.windows(2).any(|p| p[0].id >= p[1].id)
    {
        return Err("Mixed object admission accounting mismatch".into());
    }
    Ok(Some(Resolved {
        admissions,
        connectors,
    }))
}
#[cfg(test)]
mod tests {
    #[test]
    fn fitting_allowance_tracks_supported_world_area_and_retains_plain_scenery_limits() {
        for size in [1000, 2000, 4000, 5000, 6000] {
            assert_eq!(fitting_allowance(size, false).unwrap(), 256_000_000);
            if usize::BITS == 64 {
                let allowance = fitting_allowance(size, true).unwrap();
                assert_eq!(allowance as u64, 500 * u64::from(size).pow(2));
                assert!(allowance as u64 <= papyrus_terrain::decor_fit::MAXIMUM_FITTING_SAMPLES);
            }
        }
        assert!(fitting_allowance(999, true).is_err());
        assert!(fitting_allowance(6001, true).is_err());
    }
    #[test]
    fn uniform_cliff_binding_preserves_source_order_and_procedural_activation() {
        let stage = |id: &str, order: usize| {
            serde_json::from_value::<papyrus_terrain::decor::Stage>(serde_json::json!({
            "id":id,"order":order,"name":"Cliffs","kind":"uniform","folder":"decor/cliffs","enabled":true,"mode":-1,
            "filter":{"SplatType":-1,"BiomeType":-1,"TopologyAny":2,"TopologyAll":0,"TopologyNot":0},"filter_mode":1,"anchor_mode":2,
            "distance":3.,"dithering":0.,"density":0.,"cluster":null,"prefabs":[]
        })).unwrap()
        };
        let plan = || object_workflow::Plan {
            engine: object_workflow::VERSION,
            road_source_sha256: "a".repeat(64),
            decoration_stages_sha256: "b".repeat(64),
            executed: false,
            steps: [
                (622, "PlaceCliffsUniform"),
                (10, "PlaceDecorUniform"),
                (629, "PlaceCliffsUniform"),
            ]
            .into_iter()
            .enumerate()
            .map(|(order, (id, class))| object_workflow::Step {
                scene_order: order,
                object_id: id,
                class: class.into(),
                description: "Cliffs".into(),
                mode: -1,
                disabled: Some(false),
                component_enabled: false,
                hierarchy_active: true,
                handler: object_workflow::Handler::External,
            })
            .collect(),
        };
        let stages = vec![stage("622", 0), stage("629", 1)];
        let mut bound = plan();
        bind_uniform_cliffs(&mut bound, &stages).unwrap();
        assert!(matches!(
            bound.steps[0].handler,
            object_workflow::Handler::UniformCliffs { source_index: 0 }
        ));
        assert!(matches!(
            bound.steps[1].handler,
            object_workflow::Handler::External
        ));
        assert!(matches!(
            bound.steps[2].handler,
            object_workflow::Handler::UniformCliffs { source_index: 1 }
        ));
        assert!(bind_uniform_cliffs(&mut bound, &stages).is_err());
        assert!(bind_uniform_cliffs(&mut plan(), &stages[..1]).is_err());
        for change in 0..5 {
            let mut bad = stages.clone();
            match change {
                0 => bad[0].id = "10".into(),
                1 => bad[1].order = 0,
                2 => bad[0].enabled = false,
                3 => bad[0].kind = papyrus_terrain::decor::Kind::WhiteNoise,
                _ => bad.swap(0, 1),
            }
            assert!(bind_uniform_cliffs(&mut plan(), &bad).is_err());
        }
    }
    use super::*;
    #[test]
    fn early_cliffs_bind_one_source_stage_without_reordering_other_handlers() {
        let step = |id, order, class: &str| object_workflow::Step {
            object_id: id,
            scene_order: order,
            class: class.into(),
            description: String::new(),
            mode: -1,
            disabled: None,
            component_enabled: true,
            hierarchy_active: true,
            handler: object_workflow::Handler::External,
        };
        let plan = || object_workflow::Plan {
            engine: object_workflow::VERSION,
            road_source_sha256: "a".repeat(64),
            decoration_stages_sha256: "b".repeat(64),
            steps: vec![
                step(1, 0, "PlaceRoadObjects"),
                step(2, 1, "GenerateCliffTopology"),
                step(3, 2, "PlaceDecorUniform"),
                step(4, 3, "GenerateCliffTopology"),
            ],
            executed: false,
        };
        let mut early = EarlyCliff {
            object_id: 2,
            source_sha256: "a".repeat(64),
            active: true,
            keep_existing: false,
            policy: papyrus_terrain::cliff_topology::Policy {
                cliff_flag: 2,
                cliffside_flag: 4,
                protected_topology: 8,
                rock_channel: 3,
                slope_cutoff: 30.,
                rock_cutoff: 0.4,
                radius_cells: 1,
            },
            provenance: serde_json::json!({"object_id":2}),
        };
        let mut bound = plan();
        bind_early_cliff(&mut bound, &early).unwrap();
        assert!(matches!(
            bound.steps[1].handler,
            object_workflow::Handler::CliffTopology
        ));
        assert!(matches!(
            bound.steps[3].handler,
            object_workflow::Handler::External
        ));
        assert_eq!(
            bound.steps.iter().map(|s| s.object_id).collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );
        assert!(bind_early_cliff(&mut bound, &early).is_err());
        early.object_id = 1;
        assert!(bind_early_cliff(&mut plan(), &early).is_err());
        early.object_id = 99;
        assert!(bind_early_cliff(&mut plan(), &early).is_err());
        early.object_id = 2;
        early.source_sha256 = "c".repeat(64);
        assert!(bind_early_cliff(&mut plan(), &early).is_err());
        early.active = false;
        let receipt = early.receipt(None, 0).unwrap();
        assert_eq!(receipt["executed"], false);
        assert_eq!(receipt["report"], serde_json::Value::Null);
    }
    #[test]
    fn new_recipes_use_authored_water_rules_and_legacy_reports_keep_their_shape() {
        let mut recipe = Recipe::new("a".repeat(64));
        assert_eq!(recipe.engine, VERSION);
        assert_eq!(recipe.maximum_terrain_work, 512_000_000);
        recipe.validate().unwrap();
        recipe.engine = LEGACY_VERSION.into();
        recipe.validate().unwrap();
        recipe.engine = "future".into();
        assert!(recipe.validate().is_err());
        let flags = BTreeMap::from([
            ("Monument".into(), 1),
            ("Road".into(), 2),
            ("Ocean".into(), 4),
            ("River".into(), 8),
            ("Lake".into(), 16),
        ]);
        assert_eq!(
            SceneryAdmission::Legacy
                .protected_roles(&flags, 31)
                .unwrap(),
            31
        );
        assert_eq!(
            SceneryAdmission::Authored
                .protected_roles(&flags, 31)
                .unwrap(),
            3
        );
        assert!(
            SceneryAdmission::Authored
                .protected_roles(&BTreeMap::new(), 31)
                .is_err()
        );
        let mut settings = Settings {
            scenery_admission: SceneryAdmission::Legacy,
            road: road_object_placement::Settings {
                seed: 1,
                maximum_work: 1000,
                maximum_poses: 100,
            },
            decoration_seed: 2,
            realm_mask: 2,
            maximum_terrain_work: 1000,
            maximum_sampling_attempts: 1000,
            maximum_road_height_change: 8.,
        };
        assert!(
            serde_json::to_value(settings)
                .unwrap()
                .get("scenery_admission")
                .is_none()
        );
        settings.scenery_admission = SceneryAdmission::Authored;
        assert_eq!(
            serde_json::to_value(settings).unwrap()["scenery_admission"],
            "authored"
        );
    }
    #[test]
    fn mixed_ledger_keeps_connector_fit_indices_separate_from_global_ids() {
        let (source, road, decoration, terrain) =
            crate::road_object_placement::tests::mixed_fixture();
        let Resolved {
            admissions: ledger,
            connectors,
        } = resolve_admissions(&road, &decoration, &terrain.report, &source, None, &|| false)
            .unwrap()
            .unwrap();
        assert_eq!(ledger.len(), terrain.report.counts.accepted);
        assert!(!connectors.is_empty());
        assert!(
            connectors
                .iter()
                .any(|c| c.transaction_pose != c.socket.fit_pose)
        );
        for c in connectors {
            assert_eq!(
                road.global_admission_ids.as_ref().unwrap()[c.socket.fit_pose],
                c.transaction_pose
            );
            assert!(ledger.iter().any(|p| p.id == c.transaction_pose
                && matches!(p.origin,Origin::Road {fit_index} if fit_index==c.socket.fit_pose)));
        }
    }
    #[test]
    fn broken_global_mappings_and_cancellation_cannot_publish_a_partial_ledger() {
        for bad in 0..4 {
            let (source, mut road, decoration, mut terrain) =
                crate::road_object_placement::tests::mixed_fixture();
            match bad {
                0 => road.global_admission_ids.as_mut().unwrap()[0] = usize::MAX,
                1 => road.global_admission_ids.as_mut().unwrap().clear(),
                2 => terrain.report.applied[0].pose = usize::MAX,
                _ => road.global_admission_count = Some(0),
            }
            assert!(
                resolve_admissions(&road, &decoration, &terrain.report, &source, None, &|| false)
                    .is_err()
            );
        }
        let (source, road, decoration, terrain) =
            crate::road_object_placement::tests::mixed_fixture();
        let polls = std::cell::Cell::new(0);
        assert!(
            resolve_admissions(&road, &decoration, &terrain.report, &source, None, &|| {
                polls.set(polls.get() + 1);
                polls.get() == 1
            })
            .unwrap()
            .is_none()
        );
    }
}
