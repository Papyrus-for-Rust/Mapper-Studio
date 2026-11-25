//! Selected-installation road pipeline shared by normal GUI/CLI composition.
use crate::{
    Project, infrastructure, native_noise, native_profile, prefab_assets::Catalog,
    prefab_textures::Bindings, road_paint_source, road_paint_stage, road_planning, road_refinement,
    road_surface_source, road_surface_stage, road_terrain_source, roadside_placement,
    roadside_source, roadside_stamps,
};
use papyrus_terrain::{
    monument_composition::Composed, placement::Solution, road_bed, road_clearance,
    road_grade::Junction,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, path::Path};
pub const VERSION: &str = "papyrus-staged-road-generation-1";
/// Source painting/construction, with strict grade/clearance comparisons kept
/// separate. Historical staged recipes retain their original fitting contract.
pub const SOURCE_VERSION: &str = "papyrus-source-road-generation-1";
pub const RAW_SOURCE_VERSION: &str = "papyrus-source-road-generation-2";
pub const SOURCE_COUPLED_VERSION: &str = "papyrus-source-road-placement-3";
pub const CONNECTED_SOURCE_VERSION: &str = "papyrus-source-road-generation-4";
pub const CONNECTED_COUPLED_VERSION: &str = "papyrus-source-road-placement-4";
pub fn is_raw_source_engine(engine: &str) -> bool {
    matches!(engine, RAW_SOURCE_VERSION | SOURCE_COUPLED_VERSION)
}
pub fn is_source_engine(engine: &str) -> bool {
    engine == SOURCE_VERSION || uses_source_cost_guidance(engine)
}
pub fn is_connected_source_engine(engine: &str) -> bool {
    matches!(engine, CONNECTED_SOURCE_VERSION | CONNECTED_COUPLED_VERSION)
}
pub fn is_source_coupled_engine(engine: &str) -> bool {
    matches!(engine, SOURCE_COUPLED_VERSION | CONNECTED_COUPLED_VERSION)
}
pub fn uses_source_cost_guidance(engine: &str) -> bool {
    is_raw_source_engine(engine) || is_connected_source_engine(engine)
}
pub const LEGACY_COUPLED_VERSION: &str = "papyrus-coupled-road-placement-1";
pub const PROPOSAL_COUPLED_VERSION: &str = "papyrus-coupled-road-placement-2";
pub const GRID_COUPLED_VERSION: &str = "papyrus-coupled-road-placement-3";
pub const COUPLED_VERSION: &str = "papyrus-coupled-road-placement-4";
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoadsidePreference {
    pub request: u64,
    pub prefab: String,
    pub position: [f32; 2],
    pub yaw: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_displacement_m: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<crate::Presence>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub engine: String,
    pub roadside_source_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<crate::source_road_planning::Policy>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roadside_preferences: Vec<RoadsidePreference>,
}
impl Policy {
    /// Defaults for a newly enabled workflow after matching paint, terrain and
    /// surface profiles have been prepared.
    /// Rails generated after roads retain their historical completion contract.
    pub fn for_new_recipe(
        roadside_source_sha256: String,
        mut layout: Option<crate::source_road_planning::Policy>,
        has_late_rails: bool,
    ) -> Result<Self, String> {
        if !has_late_rails && let Some(layout) = &mut layout {
            layout.terrain_entrances.get_or_insert_with(|| {
                papyrus_terrain::source_road_route::TERRAIN_ENTRANCE_PINNED_VERSION.into()
            });
        }
        let policy = Self {
            engine: if layout.is_some() && !has_late_rails {
                CONNECTED_SOURCE_VERSION
            } else {
                VERSION
            }
            .into(),
            roadside_source_sha256,
            layout,
            roadside_preferences: Vec::new(),
        };
        policy.validate()?;
        Ok(policy)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !matches!(
            self.engine.as_str(),
            VERSION
                | SOURCE_VERSION
                | RAW_SOURCE_VERSION
                | SOURCE_COUPLED_VERSION
                | CONNECTED_SOURCE_VERSION
                | CONNECTED_COUPLED_VERSION
                | LEGACY_COUPLED_VERSION
                | PROPOSAL_COUPLED_VERSION
                | GRID_COUPLED_VERSION
                | COUPLED_VERSION
        ) || self.roadside_source_sha256.len() != 64
            || !self
                .roadside_source_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("Unsupported staged road policy or roadside source fingerprint".into());
        }
        let mut requests = BTreeSet::new();
        if self.roadside_preferences.len() > papyrus_terrain::placement::MAX_TARGETS
            || self.roadside_preferences.iter().any(|preference| {
                !requests.insert(preference.request)
                    || preference.prefab.len() > 512
                    || !preference.prefab.starts_with("assets/")
                    || !preference.prefab.ends_with(".prefab")
                    || !roadside_placement::is_editable_request_id(&preference.prefab, preference.request)
                    || preference.max_displacement_m.is_some_and(|limit| !(1..=10000).contains(&limit))
                    || preference.position.iter().any(|value| !value.is_finite() || value.abs() > 10000.)
                    || !preference.yaw.is_finite()
            })
        {
            return Err("Invalid roadside monument placement preferences".into());
        }
        if let Some(layout) = &self.layout {
            layout.validate()?;
        }
        if is_source_engine(&self.engine) && self.layout.is_none() {
            return Err("Source road completion requires ordered installation inputs".into());
        }
        Ok(())
    }
}
#[derive(Clone, Copy)]
pub struct Context<'a> {
    pub project: &'a Project,
    pub solution: &'a Solution,
    pub catalog: &'a Catalog,
    pub bindings: &'a Bindings,
    pub worker: &'a Path,
}
/// All intermediate revisions are private. A failed stage never alters `base`.
pub fn compose(
    context: Context<'_>,
    base: &Composed,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Composed>, String> {
    compose_with_feedback(context, base, None, cancelled).map_err(|error| error.message)
}
pub fn compose_retaining_rails(
    context: Context<'_>,
    base: &Composed,
    rails: &crate::rail_source::Source,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Composed>, String> {
    compose_with_feedback(context, base, Some(rails), cancelled).map_err(|error| error.message)
}
/// A rejected private world revision. Feedback refers to that revision's
/// monument/connector identities; callers must revalidate any new placement.
#[derive(Debug)]
pub struct Rejection {
    pub message: String,
    pub feedback: Vec<crate::road_replanning::Marker>,
    pub entrances: Vec<papyrus_terrain::road_network::socket_recovery::Key>,
}
impl Rejection {
    pub(crate) fn movable_monument(&self, project: &Project) -> Option<u64> {
        self.feedback
            .iter()
            .filter_map(|m| m.terminal)
            .chain(self.entrances.iter().copied())
            .map(|key| key.monument)
            .find(|id| {
                project
                    .targets
                    .iter()
                    .any(|target| target.instance_id == *id && !target.exact)
            })
    }
}
impl From<String> for Rejection {
    fn from(message: String) -> Self {
        Self {
            message,
            feedback: Vec::new(),
            entrances: Vec::new(),
        }
    }
}
impl From<&str> for Rejection {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}
enum LayoutFailure {
    Fatal(String),
    Entrances(crate::road_planning::Failure),
    RetainedRoadLayout(String),
    Retry {
        message: String,
        markers: Vec<crate::road_replanning::Marker>,
    },
}
impl From<crate::road_planning::Failure> for LayoutFailure {
    fn from(error: crate::road_planning::Failure) -> Self {
        if error.rejected_retained_layout {
            Self::RetainedRoadLayout(error.message)
        } else if error.entrances.is_empty() {
            Self::Fatal(error.message)
        } else {
            Self::Entrances(error)
        }
    }
}
impl From<String> for LayoutFailure {
    fn from(value: String) -> Self {
        Self::Fatal(value)
    }
}
impl From<&str> for LayoutFailure {
    fn from(value: &str) -> Self {
        Self::Fatal(value.into())
    }
}
const MAX_LAYOUTS: usize = 3;
// Conservative reservation for the individually bounded source blocker/cost,
// ring/access/trail routing, reservation and control preparation primitives.
const LAYOUT_PREPARATION_WORK: usize = 2_000_000_000;
/// Preserve structured feedback for coupled world generation without parsing
/// human-readable errors. Separate calls create separate sessions; callers
/// rebuilding a world must retain `Session` instead.
pub fn compose_with_feedback(
    context: Context<'_>,
    base: &Composed,
    rails: Option<&crate::rail_source::Source>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Composed>, Rejection> {
    if crate::world_recovery::enabled(context.project) {
        return Err("Coupled placement requires the resolved-layout composition API".into());
    }
    let mut session = Session::new(context, rails.is_some());
    let mut world = road_planning::RetryWorld::new(base);
    loop {
        match session.resume_prepared(context, &mut world, rails, &cancelled) {
            Err(_) if session.retry_routing => continue,
            Err(_) if session.last_retry => return Err(session.exhausted()),
            result => return result,
        }
    }
}

/// One bounded generation search, including any future placement/stamp rework.
/// A session cannot be reset, cloned, or resumed with a different recipe,
/// installation binding, worker, or rail mode. Solutions may change only after
/// the caller has performed fresh source placement and world validation.
pub struct Session {
    project: Project,
    solution: Solution,
    assets: [String; 2],
    worker: std::path::PathBuf,
    retained_rails: bool,
    replanning: bool,
    budget: RetryBudget,
    paint_noise: std::cell::RefCell<native_noise::PrefixCache>,
    preferences: Vec<crate::road_replanning::Marker>,
    failed_entrances: Vec<papyrus_terrain::road_network::socket_recovery::Key>,
    attempts: Vec<serde_json::Value>,
    required_connections: Option<crate::road_replanning::Connections>,
    geometry_hints: Vec<crate::road_alignment::Hint>,
    layouts: usize,
    finished: bool,
    retry_routing: bool,
    last_retry: bool,
    rebuild_reserved: bool,
    entrance_recovery: FinalEntranceRecovery,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FinalEntranceRecovery {
    Unavailable,
    PendingPlacement,
    Ready,
}

impl Session {
    pub fn new(context: Context<'_>, retained_rails: bool) -> Self {
        let shared = context
            .project
            .roads
            .as_ref()
            .and_then(|r| r.entrance_launch)
            .is_some_and(|p| p.engine == papyrus_terrain::road_network::LaunchEngine::GlobalEarthwork);
        let source_replanning = context
            .project
            .roads
            .as_ref()
            .and_then(|r| r.staged.as_ref())
            .and_then(|s| s.layout.as_ref())
            .is_some_and(|l| l.ring_clearance.is_some() || l.terrain_entrances.is_some());
        let replanning = shared
            && (source_replanning
                || (retained_rails && crate::rail_generation::before_roads(context.project)));
        Self {
            project: context.project.clone(),
            solution: context.solution.clone(),
            assets: [
                context.catalog.sha256.clone(),
                context.bindings.sha256.clone(),
            ],
            worker: context.worker.to_owned(),
            retained_rails,
            replanning,
            budget: RetryBudget {
                shared_generation: shared,
                rail_topology: retained_rails.then_some(0),
                layout_planning: replanning.then_some(0),
                ..Default::default()
            },
            paint_noise: Default::default(),
            preferences: Vec::new(),
            failed_entrances: Vec::new(),
            attempts: Vec::new(),
            required_connections: None,
            geometry_hints: Vec::new(),
            layouts: 0,
            finished: false,
            retry_routing: false,
            last_retry: false,
            rebuild_reserved: false,
            entrance_recovery: FinalEntranceRecovery::Unavailable,
        }
    }
    pub fn can_retry(&self) -> bool {
        !self.finished
            && (self.layouts < if self.replanning { MAX_LAYOUTS } else { 1 }
                || self.entrance_recovery != FinalEntranceRecovery::Unavailable)
    }
    fn allow_final_entrance_recovery(&mut self) {
        if self.layouts == MAX_LAYOUTS
            && self.project.roads.as_ref().and_then(|r| r.staged.as_ref())
                .is_some_and(|policy| is_source_coupled_engine(&policy.engine))
            && self.failed_entrances.iter().any(|key| {
                self.project.targets.iter().any(|target| {
                    target.instance_id == key.monument && !target.exact
                        && target.presence != crate::Presence::Excluded
                })
            })
        {
            self.entrance_recovery = FinalEntranceRecovery::PendingPlacement;
        }
    }
    /// Source routing can consume another layout grant over the same immutable
    /// monument/rail revision. A movable entrance must return to world recovery
    /// so its source trial and full restamp are reserved before any pose changes.
    pub(crate) fn retry_same_world(&self, error: &Rejection) -> bool {
        self.project
            .roads
            .as_ref()
            .and_then(|recipe| recipe.staged.as_ref())
            .is_some_and(|policy| is_source_coupled_engine(&policy.engine))
            && self.can_retry()
            && self.layouts < MAX_LAYOUTS
            && self.retry_routing
            && error.movable_monument(&self.project).is_none()
    }
    /// Reserve a caller-proved upper bound before placement/stamp work starts.
    /// The caller must meter or conservatively bound the complete operation;
    /// this reservation does not certify an estimate or validate a placement.
    pub fn reserve_world_rebuild(&mut self, work: u64) -> Result<bool, String> {
        if !self.can_retry() || !self.replanning {
            return Err("World rebuilding requires an unfinished shared road session".into());
        }
        let reserved = self.budget.reserve_world_rebuild(work)?;
        self.rebuild_reserved |= reserved;
        Ok(reserved)
    }
    /// Remove routing preferences at an old monument pose only after a freshly
    /// validated replacement solution exists. Preserve all required connectors.
    /// No road or fitted terrain acceptance is carried into the next attempt.
    pub fn placement_changed(&mut self, monument: u64, solution: &Solution) -> Result<(), String> {
        if !self.can_retry() || !self.last_retry {
            return Err("No rejected layout is available for placement recovery".into());
        }
        let target = self
            .project
            .targets
            .iter()
            .find(|t| t.instance_id == monument)
            .ok_or("Placement recovery monument is absent from the recipe")?;
        if target.exact {
            return Err("Placement recovery cannot move an explicit monument pin".into());
        }
        if !self.rebuild_reserved {
            return Err("Placement recovery requires a world rebuild reservation".into());
        }
        if !self
            .preferences
            .iter()
            .any(|m| m.terminal.is_some_and(|k| k.monument == monument))
            && !self.failed_entrances.iter().any(|k| k.monument == monument)
        {
            return Err("Placement recovery requires a rejected monument entrance".into());
        }
        if self.entrance_recovery == FinalEntranceRecovery::PendingPlacement
            && !self.failed_entrances.iter().any(|key| key.monument == monument)
        {
            return Err("Final entrance recovery requires a currently unresolved entrance".into());
        }
        if self.solution.outcomes.len() != solution.outcomes.len()
            || solution.required_missing > self.solution.required_missing
            || self
                .solution
                .outcomes
                .iter()
                .zip(&solution.outcomes)
                .any(|(old, new)| {
                    old.id != new.id || (old.id != monument && old.position != new.position)
                })
        {
            return Err(
                "Placement recovery changed unrelated monuments or lost required sites".into(),
            );
        }
        let previous = self
            .solution
            .outcomes
            .iter()
            .find(|o| o.id == monument)
            .and_then(|o| o.position.as_ref())
            .ok_or("Missing previous monument pose")?;
        let replacement = solution
            .outcomes
            .iter()
            .find(|o| o.id == monument)
            .and_then(|o| o.position.as_ref())
            .ok_or("Missing replacement monument pose")?;
        if [
            replacement.x,
            replacement.y,
            replacement.z,
            replacement.yaw,
            replacement.clearance,
        ]
        .iter()
        .any(|v| !v.is_finite())
            || replacement == previous
            || (target.yaw.is_some() && replacement.yaw != previous.yaw)
        {
            return Err("Invalid replacement monument pose or changed explicit orientation".into());
        }
        self.preferences
            .retain(|m| !m.terminal.is_some_and(|k| k.monument == monument));
        self.failed_entrances.retain(|k| k.monument != monument);
        self.geometry_hints
            .retain(|hint| !hint.references_monument(monument));
        self.solution = solution.clone();
        self.rebuild_reserved = false;
        self.retry_routing = false;
        if self.entrance_recovery == FinalEntranceRecovery::PendingPlacement {
            self.entrance_recovery = FinalEntranceRecovery::Ready;
        }
        Ok(())
    }
    fn exhausted(&self) -> Rejection {
        Rejection {
            message: format!(
                "Bounded complete road layouts exhausted: {}",
                serde_json::json!({"attempts":self.attempts,"preferences":self.preferences,"work":self.budget})
            ),
            feedback: self.preferences.clone(),
            entrances: self.failed_entrances.clone(),
        }
    }
    fn matches_context(
        &self,
        project: &Project,
        solution: &Solution,
        assets: [&str; 2],
        worker: &Path,
        rails: bool,
    ) -> bool {
        &self.project == project
            && crate::composition::layout_fingerprint(&self.solution)
                == crate::composition::layout_fingerprint(solution)
            && self.assets[0] == assets[0]
            && self.assets[1] == assets[1]
            && self.worker == worker
            && self.retained_rails == rails
    }
    /// Attempt exactly one complete road layout. A retry rejection retains this
    /// session's counters; cancellation or success permanently closes it.
    pub fn resume(
        &mut self,
        context: Context<'_>,
        base: &Composed,
        rails: Option<&crate::rail_source::Source>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Composed>, Rejection> {
        self.resume_prepared(context, &mut road_planning::RetryWorld::new(base), rails, cancelled)
    }

    pub(crate) fn resume_prepared(
        &mut self,
        context: Context<'_>,
        world: &mut road_planning::RetryWorld<'_>,
        rails: Option<&crate::rail_source::Source>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Composed>, Rejection> {
        if !self.matches_context(
            context.project,
            context.solution,
            [&context.catalog.sha256, &context.bindings.sha256],
            context.worker,
            rails.is_some(),
        ) {
            return Err(
                "Road session recipe, solution, assets, worker or rail mode changed".into(),
            );
        }
        self.retry_routing = false;
        self.last_retry = false;
        if !self.can_retry() {
            return Err(self.exhausted());
        }
        let final_entrance_retry = self.entrance_recovery == FinalEntranceRecovery::Ready;
        if self.entrance_recovery == FinalEntranceRecovery::PendingPlacement {
            self.finished = true;
            return Err(self.exhausted());
        }
        if cancelled() {
            self.finished = true;
            return Ok(None);
        }
        // Reuse gets one independently bounded attempt. On rejection the
        // original routing search keeps its full set of recovery attempts.
        let fallback_budget = (self.layouts == 0 && crate::source_road_planning::has_retained_layout())
            .then(|| self.budget.clone());
        if self.replanning && !self.budget.reserve_layout_preparation(final_entrance_retry)? {
            self.finished = true;
            return Err(self.exhausted());
        }
        let layout = self.layouts;
        self.layouts += 1;
        self.entrance_recovery = FinalEntranceRecovery::Unavailable;
        self.rebuild_reserved = false;
        self.failed_entrances.clear();
        let mut local_preferences = self.preferences.clone();
        if layout + 1 >= MAX_LAYOUTS {
            for marker in &mut local_preferences {
                marker.harden();
            }
        }
        match compose_single(
            context,
            world,
            rails,
            layout,
            &local_preferences,
            &mut self.budget,
            &self.paint_noise,
            if self.replanning {
                4
            } else {
                MAX_ALIGNMENT_BRANCHES
            },
            &mut self.required_connections,
            &mut self.geometry_hints,
            &cancelled,
        ) {
            Ok(Some(mut maps)) => {
                self.finished = true;
                if self.replanning {
                    maps.roads
                        .as_mut()
                        .ok_or("Replanned map lost its roads")?
                        .surfaces
                        .as_mut()
                        .ok_or("Replanned map lost its surfaces")?
                        .report["staged_pipeline"]["layout_replanning"] = serde_json::json!({"engine":crate::road_replanning::VERSION,"layout_index":layout,"maximum_layouts":MAX_LAYOUTS+usize::from(final_entrance_retry),"road_seed":context.project.seeds[3].value,"terrain_and_initial_monuments_reused":self.budget.world_rebuilding.is_none(),"source_costs_preserved":true,"baseline_connections_preserved":true,"required_connections":self.required_connections,"preferences":local_preferences,"rejected_layouts":self.attempts,"work":self.budget,"runtime_access_validated":false});
                }
                Ok(Some(maps))
            }
            Ok(None) => {
                self.finished = true;
                Ok(None)
            }
            Err(LayoutFailure::Fatal(message)) => {
                self.finished = true;
                Err(message.into())
            }
            Err(LayoutFailure::Entrances(error)) => {
                self.last_retry = self.replanning;
                self.failed_entrances = error.entrances;
                self.allow_final_entrance_recovery();
                if self.replanning {
                    self.attempts.push(serde_json::json!({"layout_index":layout,
                        "error":error.message,"unconnected_entrances":self.failed_entrances}));
                } else {
                    self.finished = true;
                }
                #[cfg(feature = "road-diagnostics")]
                if std::env::var_os("MAPPER_LAYOUT_FAILURE_CAPTURE").is_some() {
                    eprintln!(
                        "Road entrance recovery state {}",
                        serde_json::json!({
                            "layout_index":layout,"replanning":self.replanning,
                            "can_retry":self.can_retry(),"finished":self.finished,
                            "work":self.budget,"attempts":self.attempts,
                        })
                    );
                }
                Err(Rejection {
                    message: error.message,
                    feedback: Vec::new(),
                    entrances: self.failed_entrances.clone(),
                })
            }
            Err(LayoutFailure::RetainedRoadLayout(message)) => {
                if let Some(budget) = fallback_budget {
                    self.attempts.push(serde_json::json!({"retained_road_layout_rejected": message,"work": self.budget}));
                    self.budget = budget;
                    self.layouts -= 1;
                }
                self.last_retry = self.replanning;
                self.retry_routing = self.can_retry();
                Err(message.into())
            }
            Err(LayoutFailure::Retry { message, markers }) => {
                self.last_retry = self.replanning;
                let before = self.preferences.clone();
                for marker in &markers {
                    crate::road_replanning::collect(&mut self.preferences, marker.clone());
                }
                if self.replanning {
                    self.attempts.push(serde_json::json!({"layout_index":layout,"error":message,"preference_count":self.preferences.len(),"planning_exclusions":local_preferences.iter().any(|m|m.exclude)}));
                    self.retry_routing = self.can_retry()
                        && (self.preferences != before
                            || (!self.preferences.is_empty() && layout + 2 == MAX_LAYOUTS));
                } else {
                    self.finished = true;
                }
                Err(Rejection {
                    message,
                    feedback: markers,
                    entrances: Vec::new(),
                })
            }
        }
    }
}
#[allow(clippy::too_many_arguments)]
fn compose_single(
    context: Context<'_>,
    world: &mut road_planning::RetryWorld<'_>,
    rails: Option<&crate::rail_source::Source>,
    _layout_index: usize,
    preferences: &[crate::road_replanning::Marker],
    budget: &mut RetryBudget,
    paint_noise: &std::cell::RefCell<native_noise::PrefixCache>,
    local_branches: usize,
    required_connections: &mut Option<crate::road_replanning::Connections>,
    geometry_hints: &mut Vec<crate::road_alignment::Hint>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Composed>, LayoutFailure> {
    let mut timing = crate::composition::StageTimer::start("road_layout");
    macro_rules! ready {
        ($e:expr) => {
            match $e? {
                Some(v) => v,
                None => return Ok(None),
            }
        };
    }
    if cancelled() {
        return Ok(None);
    }
    let base = world.maps();
    let Context {
        project,
        solution,
        catalog,
        bindings,
        worker,
    } = context;
    let recipe = project.roads.as_ref().ok_or("Missing staged road recipe")?;
    recipe.validate()?;
    let policy = recipe.staged.as_ref().ok_or("Missing staged road policy")?;
    if project.rails.is_some() && rails.is_none() {
        return Err(
            "Staged roads require retained rail-height support before enabling rails".into(),
        );
    }
    let root = bindings.installation_root();
    let roadside =
        roadside_source::Source::cached(&policy.roadside_source_sha256, root, catalog, &cancelled)?;
    let parent = infrastructure::Source::cached_shared(catalog, recipe, &cancelled)?;
    let pipes = if is_source_engine(&policy.engine)
        && project.objects.as_ref().is_some_and(|recipe| recipe.engine == crate::object_generation::VERSION)
    {
        Some(crate::water_treatment_stage::PreparedSources::available(
            project, catalog, bindings, &parent, &cancelled,
        )?)
    } else {
        None
    };
    if rails.is_some() {
        if !crate::rail_generation::before_roads(project) {
            return Err("Retained composition requires the source-ordered rail recipe".into());
        }
        let scene = base.rails.as_ref().ok_or("Missing pre-road rail scene")?;
        if scene.report["changes"]["surface_paint_deferred"] != true {
            return Err("Pre-road rail surfaces were not deferred".into());
        }
    }
    let rail_paths = base
        .rails
        .as_ref()
        .map(|s| s.tracks.iter().map(|t| t.path.clone()).collect::<Vec<_>>())
        .unwrap_or_default();
    let rail_stages = if let Some(source) = rails {
        Some(ready!(crate::rail_topology_stage::prepare(
            source,
            &parent,
            &rail_paths,
            project.size,
            32_000_000,
            &cancelled
        )))
    } else {
        None
    };
    let mesh = road_surface_source::Source::cached(
        root,
        recipe
            .surface_sha256
            .as_ref()
            .ok_or("Missing road surface pin")?,
        &parent,
        &cancelled,
    )?;
    let paint = road_paint_source::Profile::for_source(&roadside)?
        .ok_or("No matching source road paint policy")?;
    let terrain = road_terrain_source::Profile::for_paint(&paint)?
        .ok_or("No matching source road terrain policy")?;
    let monument_role = parent
        .profile
        .topology_flags
        .as_ref()
        .and_then(|f| f.get("Monument"))
        .copied()
        .and_then(|v| i32::try_from(v).ok())
        .filter(|v| *v > 0)
        .ok_or("Missing installation Monument topology role")?;
    // SmoothenFilter masks apply to control smoothing, not AdjustTerrainHeight.
    // The captured height pass also paints river/riverbank/rail-tagged cells.
    // Corrections may use only the interval it already authored, never a wider
    // topology-based permission to change protected terrain.
    let allowed = terrain
        .policy
        .smoothing_weights
        .iter()
        .filter(|w| w.value == 0.)
        .fold(terrain.policy.water_topology, |mask, w| {
            mask | w.topology_mask
        });
    if allowed & monument_role != monument_role {
        return Err("Road height policy and installation Monument role differ".into());
    }
    let (mut plan, checked_base) = ready!(road_planning::prepare_replanned(
        project,
        solution,
        catalog,
        bindings,
        world,
        preferences,
        &cancelled
    ));
    let rail_receipt = checked_base.rail_receipt();
    crate::composition::record_timing(&mut timing, "routing");
    if budget.layout_planning.is_some() {
        if let Some(required) = required_connections {
            if let Some(diagnostic) = required.rejection(plan.planning_report())? {
                return Err(LayoutFailure::Retry {
                    message: format!(
                        "Replanned layout lost required connections: {}",
                        serde_json::to_string(&diagnostic).map_err(|e| e.to_string())?
                    ),
                    markers: vec![],
                });
            }
        } else {
            *required_connections = Some(crate::road_replanning::Connections::capture(
                plan.planning_report(),
            )?);
        }
    }
    let rail_guard_work = if let Some(rails) = rail_stages.as_ref() {
        let guard = ready!(rails.retained().height_support(
            &base.layers,
            road_surface_stage::HeightGeometry {
                origin_y: paint.height_origin,
                vertical_span: paint.vertical_span
            },
            budget.stage_remaining(128_000_000),
            &cancelled
        ));
        let work = guard.preparation_work;
        plan.protect_rail_height(guard);
        work
    } else {
        0
    };
    let route_environment = if recipe
        .entrance_launch
        .is_some_and(|p| p.engine == papyrus_terrain::road_network::LaunchEngine::GlobalEarthwork)
    {
        Some(ready!(crate::road_socket_recovery::prepare_for_plan(
            context,
            base,
            rail_receipt,
            &plan,
            &cancelled
        )))
    } else {
        None
    };
    #[cfg(feature = "road-diagnostics")]
    if !crate::road_route_diagnostics::capture_if_requested(
        Context {
            project,
            solution,
            catalog,
            bindings,
            worker,
        },
        base,
        plan.planning_report(),
        _layout_index,
        route_environment.as_ref(),
        &cancelled,
    )? {
        return Ok(None);
    }
    let prepared = AttemptContext {
        context: Context {
            project,
            solution,
            catalog,
            bindings,
            worker,
        },
        shaping: parent.profile.shaping,
        roadside: &roadside,
        roadside_earthwork: &world.roadside_earthwork,
        roadside_terrain: &world.roadside_terrain,
        mesh: &mesh,
        paint: &paint,
        paint_noise,
        terrain: &terrain,
        allowed,
        route_environment: route_environment.as_ref(),
        rails: rail_stages.as_ref(),
        rail_receipt,
        checked_base: &checked_base,
        pipes: pipes.as_ref(),
    };
    if rail_guard_work > budget.stage_remaining(128_000_000) {
        return Err("Rail preparation work exhausted".into());
    }
    budget.validation = budget
        .validation
        .checked_add(rail_guard_work)
        .ok_or("Rail preparation work overflow")?;
    let mut seeded_geometry = vec![];
    let mut alignment_base = None;
    for hint in geometry_hints.iter() {
        let grant = budget.proposal_allowance(2_000_000);
        let preparation = plan.alignment_preparation_work()?;
        let validation = if alignment_base.is_none() {
            road_planning::ALIGNMENT_BASE_VALIDATION_WORK
        } else {
            0
        };
        let overhead = preparation
            .checked_add(validation)
            .ok_or("Alignment overhead overflow")?;
        if grant <= overhead {
            break;
        }
        let replay = match hint.replay(plan.planning_report(), grant - overhead, &cancelled) {
            Ok(Some(replay)) => replay,
            Ok(None) => return Ok(None),
            Err(error) => {
                budget.charge_proposal(grant, grant)?;
                seeded_geometry
                    .push(serde_json::json!({"applied":false,"error":error.to_string()}));
                continue;
            }
        };
        let applied = replay.proposal.is_some();
        budget.charge_proposal(replay.work + if applied { overhead } else { 0 }, grant)?;
        seeded_geometry
            .push(serde_json::json!({"applied":applied,"reason":replay.reason,"work":replay.work,"base_validation_work":if applied {validation} else {0},"preparation_work_bound":if applied {preparation} else {0}}));
        if let Some(proposal) = replay.proposal {
            if alignment_base.is_none() {
                alignment_base = Some(checked_base.clone());
            }
            let Some(verified) = alignment_base.as_ref() else {
                return Ok(None);
            };
            let Some(trial) =
                plan.alignment_trial_checked(proposal, verified, preparation, &cancelled)?
            else {
                return Ok(None);
            };
            if !trial.commit(&cancelled) {
                return Ok(None);
            }
        }
    }
    let edited_roads = project.path_edits.iter().any(|e| e.route.kind != crate::path_edit::Kind::Rail);
    // The accepted map may have used a later routing layout. Keep the
    // provisional layout eligible for its ordinary retry diagnostics.
    let draft_mismatch = if edited_roads {
        match plan.apply_path_edits(&project.path_edits, &cancelled) {
            Ok(true) => None,
            Ok(false) => return Ok(None),
            Err(message) => Some(message),
        }
    } else {
        None
    };
    let preserved_roads = plan.planning_report().source_layout.as_ref().is_some_and(|receipt| {
        receipt["ring_clearance"]["retained_road_layout"] == true
    });
    if preserved_roads && let Some(message) = draft_mismatch.as_ref() {
        *required_connections = None;
        return Err(LayoutFailure::RetainedRoadLayout(message.clone()));
    }
    let initial_error = match run_attempt(&prepared, &plan, base, budget, &cancelled) {
        Ok(Some(mut attempt)) => {
            if let Some(message) = draft_mismatch { return Err(message.into()); }
            attempt.surfaces.report["staged_pipeline"]["seeded_geometry"] =
                serde_json::json!(&seeded_geometry);
            attempt.surfaces.report["staged_pipeline"]["generation_work"] =
                serde_json::json!(&budget);
            return Ok(Some(attempt.finish(plan)));
        }
        Ok(None) => return Ok(None),
        Err(error) => error,
    };
    crate::composition::record_timing(&mut timing, "rejected");
    if timing.is_some() {
        eprintln!("MAPPER_ROAD_LAYOUT_REJECTION {}", serde_json::json!({
            "layout_index": _layout_index,
            "reason": initial_error.to_string(),
        }));
    }
    if preserved_roads {
        *required_connections = None;
        return Err(LayoutFailure::RetainedRoadLayout(initial_error.to_string()));
    }
    // A source curve can cross a foreign footprint after source smoothing.
    // Rebuild its whole dependency graph from the pristine base; moving a
    // parent control locally would invalidate raw-cell branch attachments.
    if budget.shared_generation
        && budget.layout_planning.is_some()
        && let Some(marker) =
            source_route_retry_marker(&policy.engine, plan.planning_report(), &initial_error)?
    {
        let Some(markers) =
            source_route_feedback(&prepared, &plan, marker, &initial_error, budget, &cancelled)?
        else {
            return Ok(None);
        };
        return Err(LayoutFailure::Retry {
            message: initial_error.to_string(),
            markers,
        });
    }
    if (prepared.rails.is_some() || budget.layout_planning.is_some())
        && budget.shared_generation
        && !preferences.is_empty()
        && failed_alignment(&initial_error).is_none()
        && matches!(
            &initial_error,
            AttemptFailure::Fitting(_)
                | AttemptFailure::Routes(_)
                | AttemptFailure::SelectionBudget { .. }
                | AttemptFailure::GeometryRejected { .. }
        )
    {
        return Err(LayoutFailure::Retry {
            message: initial_error.to_string(),
            markers: vec![],
        });
    }
    if (prepared.rails.is_some() || budget.layout_planning.is_some())
        && budget.shared_generation
        && let Some(witness) = failed_alignment(&initial_error)
    {
        let mut search = AlignmentSearch {
            attempts: vec![],
            branches: 0,
            path: vec![witness],
            markers: vec![crate::road_replanning::marker(
                plan.planning_report(),
                witness,
            )?],
            branch_limit: local_branches,
            origins: vec![],
            retained: vec![],
            curvature: false,
        };
        if let Some(mut candidate) = retry_alignment(
            &prepared,
            &mut plan,
            base,
            &mut alignment_base,
            budget,
            witness,
            failed_control(&initial_error),
            failed_revision(&initial_error),
            1,
            &mut search,
            &cancelled,
        )? {
            if let Some(message) = draft_mismatch { return Err(message.into()); }
            candidate.surfaces.report["staged_pipeline"]["local_alignment_retry"] = serde_json::json!({
                "engine":"papyrus-road-local-alignment-16","seeded_geometry":seeded_geometry,"initial_witness":witness,"attempts":search.attempts,
                "branches":search.branches,"maximum_branches":search.branch_limit,"maximum_depth":MAX_ALIGNMENT_DEPTH,
                "candidate_from_pristine_base":true,"new_roadside_selection":true,
                "all_final_source_routes_checked":true,"runtime_access_validated":false
            });
            candidate.surfaces.report["staged_pipeline"]["generation_work"] =
                serde_json::json!(&budget);
            return Ok(Some(candidate.finish(plan)));
        }
        if cancelled() {
            return Ok(None);
        }
        for hint in search.retained {
            crate::road_alignment::Hint::collect(geometry_hints, hint);
        }
        if is_source_coupled_engine(&policy.engine)
            && let Some(family) = plan
                .planning_report()
                .families
                .iter()
                .find(|f| f.hierarchy == 2)
        {
            let (lattice, _) = plan
                .source_cost_grid(2, family.seed)?
                .ok_or("Missing source dirt entrance lattice")?;
            crate::road_replanning::attribute_curvature_terminals(
                plan.planning_report(),
                lattice,
                &mut search.markers,
            );
        }
        return Err(LayoutFailure::Retry {
            message: format!(
                "Bounded local alignment retries exhausted: {}",
                serde_json::json!({"original":initial_error.to_string(),"seeded_geometry":seeded_geometry,"attempts":search.attempts,"branches":search.branches,"work":budget})
            ),
            markers: search.markers,
        });
    }
    let Some(key) = failed_socket(plan.planning_report(), &initial_error) else {
        return Err(initial_error.to_string().into());
    };
    let environment = route_environment
        .as_ref()
        .ok_or("Missing pristine route environment")?;
    let mut search = SocketRetrySearch {
        attempts: vec![serde_json::json!({"original":true,"error":initial_error.to_string()})],
        path: vec![key],
        branches: 0,
    };
    let result = retry_socket_paths(
        &prepared,
        &mut plan,
        base,
        environment,
        budget,
        &mut search,
        &cancelled,
    )?;
    if cancelled() {
        return Ok(None);
    }
    if let Some(mut candidate) = result {
        if let Some(message) = draft_mismatch { return Err(message.into()); }
        candidate.surfaces.report["staged_pipeline"]["generation_work"] =
            serde_json::json!(&budget);
        candidate.surfaces.report["staged_pipeline"]["socket_geometry_retry"] = serde_json::json!({
            "engine":"papyrus-road-socket-retry-2","attempts":search.attempts,"work":budget,
            "maximum_depth":MAX_SOCKET_RETRY_DEPTH,"maximum_branches":MAX_SOCKET_RETRY_BRANCHES,
            "branches":search.branches,"candidate_from_pristine_base":true,"new_roadside_selection":true,
            "runtime_access_validated":false,
        });
        return Ok(Some(candidate.finish(plan)));
    }
    Err(format!(
        "Bounded socket retries exhausted: {}",
        serde_json::json!({"attempts":search.attempts,"work":budget,"branches":search.branches})
    )
    .into())
}
struct AlignmentSearch {
    attempts: Vec<serde_json::Value>,
    branches: usize,
    path: Vec<road_refinement::AlignmentWitness>,
    markers: Vec<crate::road_replanning::Marker>,
    branch_limit: usize,
    origins: Vec<crate::road_alignment::Hint>,
    retained: Vec<crate::road_alignment::Hint>,
    curvature: bool,
}
impl AlignmentSearch {
    fn start_curvature_fallback(
        &mut self,
        witness: road_refinement::AlignmentWitness,
        depth: usize,
    ) -> bool {
        if depth != 1
            || self.curvature
            || self.branch_limit >= MAX_ALIGNMENT_BRANCHES
            || witness.kind != road_refinement::AlignmentKind::Curvature
        {
            return false;
        }
        self.curvature = true;
        self.branch_limit = MAX_ALIGNMENT_BRANCHES;
        true
    }
}
fn fresh_alignment(
    report: &papyrus_terrain::road_network::Report,
    path: &[road_refinement::AlignmentWitness],
    next: road_refinement::AlignmentWitness,
) -> bool {
    path.iter()
        .all(|&previous| distinct_alignment(report, previous, next))
}
const MAX_ALIGNMENT_DEPTH: usize = 4;
const MAX_ALIGNMENT_BRANCHES: usize = 12;
/// Repeated faces in the same local patch need an alternative offset, rather
/// than consuming every branch in deeper versions of the same failed trial.
fn distinct_alignment(
    report: &papyrus_terrain::road_network::Report,
    current: road_refinement::AlignmentWitness,
    next: road_refinement::AlignmentWitness,
) -> bool {
    if current.road >= report.roads.len() || next.road >= report.roads.len() {
        return false;
    }
    if current
        .position
        .iter()
        .chain(&next.position)
        .any(|v| !v.is_finite())
    {
        return false;
    }
    if current.road != next.road {
        return next.road < report.roads.len();
    }
    if current.kind != next.kind {
        return true;
    }
    let Some(family) = report.families.iter().find(|f| {
        current.road >= f.first_road && current.road < f.first_road.saturating_add(f.road_count)
    }) else {
        return false;
    };
    let distance =
        (current.position[0] - next.position[0]).hypot(current.position[1] - next.position[1]);
    family.width.is_finite() && family.width > 0. && distance > f64::from(family.width) * 2.
}
fn source_route_retry_marker(
    engine: &str,
    report: &papyrus_terrain::road_network::Report,
    failure: &AttemptFailure,
) -> Result<Option<crate::road_replanning::Marker>, String> {
    if !uses_source_cost_guidance(engine) || failed_socket(report, failure).is_some() {
        return Ok(None);
    }
    let AttemptFailure::Routes(failure) = failure else {
        return Ok(None);
    };
    let Some(detail) = failure.detail.as_ref() else {
        return Ok(None);
    };
    blocked_ground_marker(report, &detail.error)
}
fn blocked_ground_marker(
    report: &papyrus_terrain::road_network::Report,
    error: &papyrus_terrain::road_network::socket_recovery::Failure,
) -> Result<Option<crate::road_replanning::Marker>, String> {
    let papyrus_terrain::road_network::socket_recovery::Failure::GroundBlocked {
        road,
        position,
        radius,
        ground,
        obstacle,
        ..
    } = error
    else {
        return Ok(None);
    };
    let mut marker = crate::road_replanning::marker(
        report,
        road_refinement::AlignmentWitness {
            kind: road_refinement::AlignmentKind::Clearance,
            road: *road,
            position: [f64::from(position[0]), f64::from(position[2])],
        },
    )?;
    marker.obstacle = obstacle.as_deref().cloned();
    marker.exclude = marker.obstacle.is_some();
    if marker.obstacle.is_none()
        && let Some(blocked) = ground.as_ref().filter(|g| {
            matches!(
                g.reason,
                "terrain hole" | "water or low ground" | "unowned protected ground"
            )
        })
    {
        if blocked.position.iter().any(|v| !v.is_finite())
            || !radius.is_finite()
            || !(0. ..=256.).contains(radius)
            || *radius == 0.
        {
            return Err("Invalid source road blocked-ground witness".into());
        }
        // The failed ground sample can lie beside the curve centre. Route
        // around that rejected terrain using at least the checked footprint radius.
        marker.witness.position = [
            f64::from(blocked.position[0]),
            f64::from(blocked.position[2]),
        ];
        marker.radius = marker.radius.max(*radius);
        marker.exclude = true;
        marker.low_ground = blocked.reason == "water or low ground";
    }
    Ok(Some(marker))
}
fn source_route_feedback(
    prepared: &AttemptContext<'_>,
    plan: &road_planning::Plan,
    mut marker: crate::road_replanning::Marker,
    failure: &AttemptFailure,
    budget: &mut RetryBudget,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Vec<crate::road_replanning::Marker>>, String> {
    let policy = prepared
        .context
        .project
        .roads
        .as_ref()
        .and_then(|recipe| recipe.staged.as_ref())
        .ok_or("Missing source road feedback policy")?;
    let detail = match failure {
        AttemptFailure::Routes(failure) => failure.detail.as_deref(),
        _ => None,
    };
    let annotate = |marker: &mut crate::road_replanning::Marker,
                    failure: &papyrus_terrain::road_network::socket_recovery::Failure| {
        if let Some(environment) = prepared.route_environment
            && let papyrus_terrain::road_network::socket_recovery::Failure::GroundBlocked {
                ground: Some(ground), ..
            } = failure
            && ground.reason == "unowned protected ground"
        {
            marker.protected_topology =
                ground.topology & environment.unconditionally_protected_topology(marker.hierarchy);
        }
    };
    if let Some(detail) = detail {
        annotate(&mut marker, &detail.error);
    }
    let mut markers = vec![marker.clone()];
    if let Some(environment) = prepared.route_environment {
        let roads = plan.roads();
        let collect = if policy.layout.as_ref().is_some_and(|layout| {
            matches!(
                layout.terrain_entrances.as_deref(),
                Some(
                    papyrus_terrain::source_road_route::TERRAIN_ENTRANCE_BOOTSTRAP_VERSION
                        | papyrus_terrain::source_road_route::TERRAIN_ENTRANCE_PINNED_VERSION
                )
            )
        }) {
            crate::road_replanning::network_obstacles_all_families
        } else {
            crate::road_replanning::network_obstacles
        };
        let Some((batch, work)) = collect(
            plan.planning_report(),
            &roads,
            marker,
            environment.input().obstacles,
            budget.stage_remaining(64_000_000),
            cancelled,
        )?
        else {
            return Ok(None);
        };
        budget.validation = budget
            .validation
            .checked_add(work)
            .ok_or("Ring feedback work overflow")?;
        markers = batch;
    }
    if let Some(detail) = detail {
        for blocked in &detail.additional_blocked_ground {
            if let Some(mut marker) = blocked_ground_marker(plan.planning_report(), blocked)? {
                annotate(&mut marker, blocked);
                crate::road_replanning::collect(&mut markers, marker);
            }
        }
    }
    Ok(Some(markers))
}
fn failed_alignment(failure: &AttemptFailure) -> Option<road_refinement::AlignmentWitness> {
    match failure {
        AttemptFailure::GeometryRejected { witness, .. } => *witness,
        AttemptFailure::Routes(failure) => {
            let papyrus_terrain::road_network::socket_recovery::Failure::StripFolded {
                road,
                position,
                ..
            } = &failure.detail.as_ref()?.error
            else {
                return None;
            };
            position
                .iter()
                .all(|v| v.is_finite())
                .then_some(road_refinement::AlignmentWitness {
                    kind: road_refinement::AlignmentKind::Curvature,
                    road: *road,
                    position: [f64::from(position[0]), f64::from(position[2])],
                })
        }
        AttemptFailure::Fitting(road_refinement::Failure::FittingRound {
            witness: Some(witness),
            ..
        }) => Some(**witness),
        AttemptFailure::Fitting(road_refinement::Failure::InitialControlProposal {
            conflict,
            ..
        }) => Some(road_refinement::AlignmentWitness {
            kind: road_refinement::AlignmentKind::Grade,
            road: conflict.road,
            position: [
                f64::from(conflict.position[0]),
                f64::from(conflict.position[2]),
            ],
        }),
        _ => None,
    }
}
fn source_ground_comparison(
    error: &crate::road_socket_recovery::AllRoutesFailure,
    base: &papyrus_terrain::native_layers::Layers,
    constructed: &[i16],
) -> Option<serde_json::Value> {
    let papyrus_terrain::road_network::socket_recovery::Failure::GroundBlocked {
        road,
        interval,
        ground: Some(ground),
        ..
    } = &error.detail.as_ref()?.error
    else {
        return None;
    };
    let n = base.height_resolution;
    if !(2..=4097).contains(&n)
        || base.size == 0
        || base.height.len() != n * n
        || constructed.len() != n * n
        || ground.position.iter().any(|v| !v.is_finite())
    {
        return None;
    }
    let cell = [ground.position[0], ground.position[2]]
        .map(|v| ((f64::from(v) / f64::from(base.size) + 0.5) * (n - 1) as f64).round());
    if cell.iter().any(|&v| v < 0. || v >= n as f64) {
        return None;
    }
    let [x, z] = cell.map(|v| v as usize);
    let world = [x, z].map(|i| (i as f32 / (n - 1) as f32 - 0.5) * base.size as f32);
    if (world[0] - ground.position[0]).abs() > 0.001
        || (world[1] - ground.position[2]).abs() > 0.001
    {
        return None;
    }
    let i = z * n + x;
    let before = base.height[i];
    let after = constructed[i];
    if !(0..=32766).contains(&before) || !(0..=32766).contains(&after) {
        return None;
    }
    let before_y = papyrus_terrain::erosion::decode(before);
    let after_y = papyrus_terrain::erosion::decode(after);
    if (before_y - ground.position[1]).abs() > 0.001
        && (after_y - ground.position[1]).abs() > 0.001
    {
        return None;
    }
    Some(
        serde_json::json!({"engine":"mapper-source-ground-comparison-1","accepted":false,
        "road":road,"interval":interval,"cell":[x,z],"position":ground.position,
        "topology":ground.topology,"reason":ground.reason,"before_units":before,"constructed_units":after,
        "before_height":before_y,"constructed_height":after_y,"delta":after_y-before_y,
        "before_low_ground":before_y<=1.,"constructed_low_ground":after_y<=1.,
        "scope":"One rejected terrain sample"}),
    )
}
fn failed_control(failure: &AttemptFailure) -> Option<papyrus_terrain::road_grade::ControlConflict> {
    match failure {
        AttemptFailure::Fitting(road_refinement::Failure::InitialControlProposal {
            conflict,
            ..
        }) => Some(*conflict),
        _ => None,
    }
}
fn failed_revision(failure: &AttemptFailure) -> Option<&crate::road_portal_revision::Revision> {
    match failure {
        AttemptFailure::Fitting(road_refinement::Failure::FittingRound {
            ranking_revision: Some(revision),
            ..
        }) => Some(revision),
        _ => None,
    }
}
fn rejects_exact_portal(failure: &AttemptFailure, road: usize) -> bool {
    match failure {
        AttemptFailure::Fitting(road_refinement::Failure::FittingRound { error, .. }) => {
            match error.as_ref() {
                road_bed::Failure::ClearanceInfeasible { road: failed, .. }
                | road_bed::Failure::HorizontalGeometry { road: failed, .. } => *failed == road,
                _ => false,
            }
        }
        _ => false,
    }
}
/// A solver-wide envelope prefix can establish progress across witnesses.
/// Without that evidence only the same indexed control may be compared.
/// Called solely for parent/child trials preserving the same plan/control order.
/// This is search progress only; it never changes a fitter acceptance tolerance.
fn improving_control(
    previous: Option<papyrus_terrain::road_grade::ControlConflict>,
    next: Option<papyrus_terrain::road_grade::ControlConflict>,
) -> bool {
    use papyrus_terrain::road_grade::ConflictKind;
    let Some((a, b)) = previous.zip(next) else {
        return false;
    };
    if a.kind != ConflictKind::IntervalEnvelope
        || b.kind != a.kind
        || a.position.iter().chain(&b.position).any(|v| !v.is_finite())
        || [a.required, a.available, b.required, b.available]
            .iter()
            .any(|v| !v.is_finite())
    {
        return false;
    }
    match (a.envelope, b.envelope) {
        (Some(x), Some(y)) => {
            let valid = |p: papyrus_terrain::rail_grade_fit::EnvelopePrefix| {
                p.nodes > 0
                    && p.first_node < p.nodes
                    && p.required.is_finite()
                    && p.available.is_finite()
                    && p.required > p.available
            };
            if !valid(x)
                || !valid(y)
                || x.nodes != y.nodes
                || x.edges != y.edges
                || y.first_node < x.first_node
            {
                return false;
            }
            if y.first_node > x.first_node {
                return true;
            }
            let scale = [x.required, x.available, y.required, y.available]
                .into_iter()
                .map(f64::abs)
                .fold(1., f64::max);
            let precision = 8. * f64::from(f32::EPSILON) * scale;
            let improvement = (x.required - x.available) - (y.required - y.available);
            if improvement > precision {
                return true;
            }
            if improvement < -precision {
                return false;
            }
        }
        (None, None) => {}
        _ => return false,
    }
    if a.road != b.road || a.point != b.point {
        return false;
    }
    let before = a.required - a.available;
    let after = b.required - b.available;
    let scale = [a.required, a.available, b.required, b.available]
        .into_iter()
        .map(f64::abs)
        .fold(1., f64::max);
    let precision = 8. * f64::from(f32::EPSILON) * scale;
    before > 0. && after > 0. && before - after > precision
}
#[allow(clippy::too_many_arguments)]
fn retry_alignment<'m>(
    prepared: &AttemptContext<'_>,
    plan: &mut road_planning::Plan,
    base: &'m Composed,
    alignment_base: &mut Option<road_planning::AlignmentBase<'m>>,
    budget: &mut RetryBudget,
    witness: road_refinement::AlignmentWitness,
    previous_control: Option<papyrus_terrain::road_grade::ControlConflict>,
    previous_revision: Option<&crate::road_portal_revision::Revision>,
    depth: usize,
    search: &mut AlignmentSearch,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Attempt>, String> {
    let result = retry_alignment_attempts(
        prepared,
        plan,
        base,
        alignment_base,
        budget,
        witness,
        previous_control,
        previous_revision,
        depth,
        search,
        cancelled,
    )?;
    if result.is_some() || cancelled() || !search.start_curvature_fallback(witness, depth) {
        return Ok(result);
    }
    // Release the original attempt frame before starting fallback recovery.
    // A fallback changes strategy at this depth and must not grow the stack.
    retry_alignment_attempts(
        prepared,
        plan,
        base,
        alignment_base,
        budget,
        witness,
        previous_control,
        previous_revision,
        depth,
        search,
        cancelled,
    )
}
#[allow(clippy::too_many_arguments)]
fn retry_alignment_attempts<'m>(
    prepared: &AttemptContext<'_>,
    plan: &mut road_planning::Plan,
    base: &'m Composed,
    alignment_base: &mut Option<road_planning::AlignmentBase<'m>>,
    budget: &mut RetryBudget,
    witness: road_refinement::AlignmentWitness,
    previous_control: Option<papyrus_terrain::road_grade::ControlConflict>,
    previous_revision: Option<&crate::road_portal_revision::Revision>,
    depth: usize,
    search: &mut AlignmentSearch,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Attempt>, String> {
    // Exact rejected poses only, scoped to this immutable plan/base/revision.
    // No acceptance or native measurement is cached across calls/layouts.
    let mut rejected: Vec<Vec<[f32; 3]>> = Vec::new();
    if cancelled() {
        return Ok(None);
    }
    if alignment_base.is_none() {
        let validation = road_planning::ALIGNMENT_BASE_VALIDATION_WORK;
        if budget.proposal_allowance(validation) < validation {
            return Ok(None);
        };
        budget.charge_proposal(validation, validation)?;
        *alignment_base = plan.alignment_base(base, cancelled)?;
        if alignment_base.is_none() {
            return Ok(None);
        };
    }
    for offset in crate::road_alignment::OFFSETS {
        if cancelled() {
            return Ok(None);
        }
        let grant = budget.proposal_allowance(2_000_000);
        let preparation = plan.alignment_preparation_work()?;
        if grant <= preparation
            || budget.paint_remaining() == 0
            || budget.selection_remaining() == 0
            || search.branches >= search.branch_limit
        {
            break;
        }
        search.branches += 1;
        let family = plan
            .planning_report()
            .families
            .iter()
            .find(|f| witness.road >= f.first_road && witness.road < f.first_road + f.road_count)
            .ok_or("Missing local-alignment family")?;
        let native_family = family.hierarchy < prepared.mesh.policy().maximum_hierarchy_exclusive;
        let width = family.width;
        let grid = papyrus_terrain::path_terrain::Grid {
            size: prepared.context.project.size as f32,
            resolution: base.layers.height_resolution,
            origin_y: prepared.paint.height_origin,
            vertical_span: prepared.paint.vertical_span,
            height: &base.layers.height,
        };
        let original_controls = &plan.planning_report().roads[witness.road].points;
        let revision =
            previous_revision.filter(|r| r.matches(witness.road, grid, original_controls));
        let native_enabled = native_family && revision.is_some();
        let mut native_profile = None;
        let templates = prepared
            .mesh
            .meshes
            .iter()
            .map(|m| &m.buffer)
            .collect::<Vec<_>>();
        let contour_enabled = offset == crate::road_alignment::OFFSETS[0]
            && !native_family
            && prepared
                .context
                .project
                .roads
                .as_ref()
                .and_then(|r| r.staged.as_ref())
                .and_then(|p| p.layout.as_ref())
                .is_some_and(|p| p.engine == crate::source_road_planning::VERSION);
        let mut contour_work = 0;
        let mut contour_report = None;
        let mut contour_proposal = None;
        if contour_enabled && let Some(shaping) = prepared.shaping {
            let protected = prepared
                .terrain
                .policy
                .smoothing_weights
                .iter()
                .filter(|w| w.value == 0.)
                .fold(prepared.terrain.policy.water_topology, |mask, w| {
                    mask | w.topology_mask
                });
            let maximum_grade = prepared
                .context
                .project
                .roads
                .as_ref()
                .ok_or("Missing contour road recipe")?
                .maximum_grade;
            match crate::road_alignment::propose_contour(
                plan.planning_report(),
                witness,
                grid,
                &base.layers.topology,
                base.layers.surface_resolution,
                protected,
                f64::from(maximum_grade),
                shaping,
                grant - preparation,
                cancelled,
            ) {
                Ok(Some(attempt)) => {
                    contour_work = attempt.work;
                    contour_report = Some(attempt.report);
                    contour_proposal = attempt.proposal;
                }
                Ok(None) => {}
                Err(error) => {
                    budget.charge_proposal(grant, grant)?;
                    search.attempts.push(serde_json::json!({"witness":witness,"depth":depth,"contour_preflight":false,"error":error.to_string()}));
                    continue;
                }
            }
        }
        let extra_contour_work = if contour_proposal.is_some() {
            0
        } else {
            contour_work
        };
        let mut proposal_witness = witness;
        if !search.curvature && proposal_witness.kind == road_refinement::AlignmentKind::Curvature {
            proposal_witness.kind = road_refinement::AlignmentKind::Clearance;
        }
        let proposed = if search.curvature
            && witness.kind == road_refinement::AlignmentKind::Curvature
        {
            crate::road_alignment::propose_curvature(
                plan.planning_report(),
                witness,
                grid,
                offset,
                grant.saturating_sub(preparation),
                cancelled,
            )
        } else if let Some(proposal) = contour_proposal {
            Ok(Some(proposal))
        } else {
            crate::road_alignment::propose_with_native(
            plan.planning_report(),
            proposal_witness,
            grid,
            offset,
            grant.saturating_sub(preparation+contour_work),
            plan.rail_height_guard().map(|guard| guard.samples()),
            native_enabled,
            &rejected,
            |controls, endpoint, maximum_work| {
                let revision = revision.ok_or("Missing failed-fit portal revision")?;
                let Some(projection) =
                    revision.project(original_controls, controls, maximum_work, cancelled)?
                else {
                    return Ok(None);
                };
                let remaining = maximum_work
                    .checked_sub(projection.work)
                    .filter(|v| *v > 0)
                    .ok_or(crate::road_portal_mesh::Failure::Budget)?;
                let output = crate::road_portal_mesh::measure(
                    &projection.controls,
                    revision.grid(),
                    crate::road_portal_mesh::Source {
                        policy: prepared.mesh.policy(),
                        templates: &templates,
                    },
                    crate::road_portal_mesh::Settings {
                        endpoint: if endpoint == 0 {
                            crate::road_portal_mesh::Endpoint::Start
                        } else {
                            crate::road_portal_mesh::Endpoint::End
                        },
                        width,
                        maximum_work: remaining,
                        maximum_grade: prepared
                            .context
                            .project
                            .roads
                            .as_ref()
                            .ok_or("Missing native portal recipe")?
                            .maximum_grade,
                        clearance: road_clearance::Settings {
                            tolerance: 0.02,
                            minimum_up_normal: 0.5,
                            maximum_work: 1_000_000,
                            maximum_probes: 131072,
                        },
                    },
                    |coordinates| {
                        if native_profile.is_none() {
                            native_profile = Some(native_profile::resolve(
                                &prepared.context.project.terrain_key(),
                            )?);
                        }
                        native_noise::sample(
                            prepared.context.bindings.installation_root(),
                            prepared.context.worker,
                            prepared
                                .context
                                .project
                                .installation
                                .as_ref()
                                .ok_or("Missing native portal installation")?,
                            native_profile.as_ref().ok_or("Missing native profile")?,
                            coordinates,
                            cancelled,
                        )
                    },
                    cancelled,
                )?;
                let Some(output) = output else {
                    return Ok(None);
                };
                let valid = output.report.mesh.driving_triangles > 0
                    && output.report.mesh.inverted_world_triangles == 0
                    && output.report.mesh.degenerate_world_triangles == 0;
                Ok(Some(crate::road_alignment::PortalEvaluation {
                    excess: valid
                        .then_some(output.report.clearance.maximum_continuous_terrain_excess),
                    clearance_passed: valid && output.report.clearance.passed,
                    work: output.report.work + projection.work,
                    detail: serde_json::json!({"revision_scope":"failed_fit_painted_grid_guidance",
                        "failed_round":revision.round(),"projection_work":projection.work,
                        "changed_controls":projection.changed_controls,"measurement":output.report}),
                }))
            },
            cancelled,
        ).map(|p|p.map(|mut p|{p.work+=contour_work;p}))
        };
        let proposal = match proposed {
            Ok(Some(mut proposal)) => {
                proposal.work = proposal
                    .work
                    .checked_add(extra_contour_work)
                    .ok_or("Contour fallback work overflow")?;
                budget.charge_proposal(proposal.work + preparation, grant)?;
                proposal
            }
            Ok(None) => return Ok(None),
            Err(error) => {
                budget.charge_proposal(grant, grant)?;
                let pinned = matches!(error, crate::road_alignment::Failure::PinnedAttachment);
                search.attempts.push(serde_json::json!({"witness":witness,"depth":depth,"offset_widths":offset,"preflight":false,"offset_independent_rejection":pinned,"error":error.to_string()}));
                if pinned {
                    break;
                }
                continue;
            }
        };
        if !search.origins.iter().any(|h| h.road == witness.road)
            && let Some(hint) =
                crate::road_alignment::Hint::capture(plan.planning_report(), witness.road)
        {
            search.origins.push(hint);
        }
        let native_ranked = proposal.portal_mesh.is_some();
        let cache_allowance = grant.saturating_sub(proposal.work + preparation);
        let geometry = serde_json::json!({"circular":proposal.circular,"retained_contact_controls":proposal.contact_controls,"endpoint_tangent":proposal.endpoint_tangent,"portal_heading":proposal.portal_heading,"portal_mesh":proposal.portal_mesh,"contour":contour_report,"curvature_fallback":search.curvature,"preparation_work_bound":preparation,"immutable_base_validation_reused":true});
        let verified = alignment_base
            .as_ref()
            .ok_or("Missing verified alignment base")?;
        let mut trial =
            match plan.alignment_trial_checked(proposal, verified, preparation, cancelled)? {
                Some(trial) => trial,
                None => return Ok(None),
            };
        let candidate = match run_attempt(prepared, &trial, base, budget, cancelled) {
            Ok(Some(candidate)) => candidate,
            Ok(None) => return Ok(None),
            Err(
                error @ (AttemptFailure::Fitting(_)
                | AttemptFailure::Routes(_)
                | AttemptFailure::GeometryRejected { .. }
                | AttemptFailure::SelectionBudget { .. }),
            ) => {
                if native_ranked && rejects_exact_portal(&error, witness.road) {
                    let pose = &trial.planning_report().roads[witness.road].points;
                    let copy_work = pose
                        .len()
                        .checked_mul(3)
                        .ok_or("Portal history work overflow")?;
                    if rejected.len() < MAX_ALIGNMENT_BRANCHES
                        && rejected.iter().map(Vec::len).sum::<usize>() + pose.len() <= 131072
                        && copy_work <= cache_allowance
                        && copy_work <= budget.proposal_allowance(copy_work)
                    {
                        budget.charge_proposal(copy_work, copy_work)?;
                        rejected.push(pose.clone());
                    }
                }
                let next_control = failed_control(&error);
                let improving = improving_control(previous_control, next_control);
                search.attempts.push(serde_json::json!({"witness":witness,"depth":depth,"offset_widths":offset,"geometry":geometry,"preflight":true,"improving_control":improving,"envelope_prefix":next_control.and_then(|c|c.envelope),"error":error.to_string()}));
                // A geometry repair can expose a later route rejection. Keep
                // that feedback when rebuilding the complete layout, just as
                // for a rejection from the original attempt.
                if budget.shared_generation
                    && budget.layout_planning.is_some()
                    && let Some(policy) = prepared
                        .context
                        .project
                        .roads
                        .as_ref()
                        .and_then(|recipe| recipe.staged.as_ref())
                    && let Some(marker) =
                        source_route_retry_marker(&policy.engine, trial.planning_report(), &error)?
                {
                    let Some(markers) =
                        source_route_feedback(prepared, &trial, marker, &error, budget, cancelled)?
                    else {
                        return Ok(None);
                    };
                    for marker in markers {
                        crate::road_replanning::collect(&mut search.markers, marker);
                    }
                }
                if let Some(next) = failed_alignment(&error) {
                    if next.road != witness.road
                        && next_control.is_none()
                        && let Some(origin) = search.origins.iter().find(|h| h.road == witness.road)
                        && let Some(hint) = origin.candidate(trial.planning_report())
                    {
                        crate::road_alignment::Hint::collect(&mut search.retained, hint);
                    }
                    crate::road_replanning::collect(
                        &mut search.markers,
                        crate::road_replanning::marker(trial.planning_report(), next)?,
                    );
                }
                if depth < MAX_ALIGNMENT_DEPTH
                    && let Some(next) = failed_alignment(&error)
                    && (fresh_alignment(trial.planning_report(), &search.path, next) || improving)
                {
                    search.path.push(next);
                    let nested = retry_alignment(
                        prepared,
                        &mut trial,
                        base,
                        alignment_base,
                        budget,
                        next,
                        next_control,
                        failed_revision(&error),
                        depth + 1,
                        search,
                        cancelled,
                    );
                    search.path.pop();
                    if let Some(candidate) = nested? {
                        if !trial.commit(cancelled) {
                            return Ok(None);
                        }
                        search.attempts.push(serde_json::json!({"witness":witness,"depth":depth,"offset_widths":offset,"accepted_parent":true}));
                        return Ok(Some(candidate));
                    }
                }
                continue;
            }
            Err(error) => return Err(error.to_string()),
        };
        if candidate.validated_routes != Some(candidate.controls.len()) {
            return Err("Alignment candidate lost complete source-route validation".into());
        }
        if !trial.commit(cancelled) {
            return Ok(None);
        }
        search.attempts.push(serde_json::json!({"witness":witness,"depth":depth,"offset_widths":offset,"accepted":true}));
        return Ok(Some(candidate));
    }
    Ok(None)
}
const MAX_SOCKET_RETRY_DEPTH: usize = 4;
const MAX_SOCKET_RETRY_BRANCHES: usize = 36;
struct SocketRetrySearch {
    attempts: Vec<serde_json::Value>,
    path: Vec<papyrus_terrain::road_network::socket_recovery::Key>,
    branches: usize,
}
/// Each recursive level owns a rollback guard. A child is tried only when the
/// fitter identifies a different socket, never by loosening terrain constraints.
fn retry_socket_paths(
    prepared: &AttemptContext<'_>,
    plan: &mut road_planning::Plan,
    base: &Composed,
    environment: &crate::road_socket_recovery::Environment<'_>,
    budget: &mut RetryBudget,
    search: &mut SocketRetrySearch,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Attempt>, String> {
    use papyrus_terrain::road_network::socket_recovery;
    let key = *search.path.last().ok_or("Empty socket retry path")?;
    let angles = match socket_recovery::angles(plan.planning_report(), key) {
        Ok(angles) => angles,
        Err(error @ socket_recovery::Failure::Rejected(_)) => {
            search.attempts.push(serde_json::json!({"socket":key,"depth":search.path.len(),"fan_available":false,"error":error}));
            return Ok(None);
        }
        Err(error) => return Err(format!("Socket retry headings: {error:?}")),
    };
    for degrees in angles {
        if cancelled() {
            return Ok(None);
        }
        if search.branches >= MAX_SOCKET_RETRY_BRANCHES
            || budget.fitting_remaining() == 0
            || budget.paint_remaining() == 0
            || budget.selection_remaining() == 0
            || budget.straightening_remaining() == 0
        {
            break;
        }
        let allowance = budget.proposal_allowance(environment.input().launch.maximum_work);
        if allowance == 0 {
            break;
        }
        search.branches += 1;
        let proposal = match environment.propose(plan, key, degrees, allowance, cancelled) {
            Ok(Some(proposal)) => {
                budget.charge_proposal(proposal.audit().work, allowance)?;
                proposal
            }
            Ok(None) => return Ok(None),
            Err(error) => {
                // Rejections do not carry an exact meter. Charge the full grant.
                budget.charge_proposal(allowance, allowance)?;
                search.attempts.push(serde_json::json!({"socket":key,"depth":search.path.len(),"degrees":degrees,"preflight":false,"error":error}));
                continue;
            }
        };
        let mut trial = match plan.socket_trial(proposal, base, cancelled)? {
            Some(trial) => trial,
            None => return Ok(None),
        };
        let candidate = match run_attempt(prepared, &trial, base, budget, cancelled) {
            Ok(Some(candidate)) => candidate,
            Ok(None) => return Ok(None),
            Err(error @ (AttemptFailure::Fitting(_) | AttemptFailure::Routes(_))) => {
                search.attempts.push(serde_json::json!({"socket":key,"depth":search.path.len(),"degrees":degrees,"preflight":true,"error":error.to_string()}));
                if let Some(next) = failed_socket(trial.planning_report(), &error)
                    && search.path.len() < MAX_SOCKET_RETRY_DEPTH
                    && !search.path.contains(&next)
                {
                    search.path.push(next);
                    let nested = retry_socket_paths(
                        prepared,
                        &mut trial,
                        base,
                        environment,
                        budget,
                        search,
                        cancelled,
                    );
                    search.path.pop();
                    if let Some(candidate) = nested? {
                        if trial.commit(cancelled).is_none() {
                            return Ok(None);
                        }
                        return Ok(Some(candidate));
                    }
                    if cancelled() {
                        return Ok(None);
                    }
                }
                continue;
            }
            Err(error) => return Err(error.to_string()),
        };
        // The normal attempt already checked every source family using its
        // own costs, shape and water policy. Reapplying the older socket-only
        // policy here would reject legitimate checked native exit buffers.
        if candidate.validated_routes != Some(candidate.controls.len()) {
            return Err("Retry candidate lost complete source-route validation".into());
        }
        search.attempts.push(serde_json::json!({"socket":key,"degrees":degrees,"depth":search.path.len(),"adjusted_sockets":search.path,"all_final_source_routes_checked":true}));
        if trial.commit(cancelled).is_none() {
            return Ok(None);
        }
        return Ok(Some(candidate));
    }
    Ok(None)
}

fn failed_socket(
    report: &papyrus_terrain::road_network::Report,
    failure: &AttemptFailure,
) -> Option<papyrus_terrain::road_network::socket_recovery::Key> {
    let (index, position) = match failure {
        AttemptFailure::Fitting(road_refinement::Failure::FittingRound { error, .. }) => {
            match error.as_ref() {
                road_bed::Failure::ClearanceInfeasible { road, position, .. } => (*road, *position),
                _ => return None,
            }
        }
        AttemptFailure::Routes(error) => match &error.detail.as_ref()?.error {
            papyrus_terrain::road_network::socket_recovery::Failure::GroundBlocked {
                road,
                position,
                ..
            }
            | papyrus_terrain::road_network::socket_recovery::Failure::CostBlocked {
                road,
                position,
                ..
            }
            | papyrus_terrain::road_network::socket_recovery::Failure::StripFolded {
                road,
                position,
                ..
            } => (*road, [f64::from(position[0]), f64::from(position[2])]),
            _ => return None,
        },
        _ => return None,
    };
    crate::road_replanning::terminal(report, index, position)
}
#[derive(Clone, Copy)]
struct AttemptContext<'a> {
    shaping: Option<papyrus_terrain::road_network::ShapingPolicy>,
    context: Context<'a>,
    roadside: &'a roadside_source::Source,
    roadside_earthwork: &'a std::cell::RefCell<roadside_placement::EarthworkCache>,
    roadside_terrain: &'a roadside_stamps::TerrainCache,
    mesh: &'a road_surface_source::Source,
    paint: &'a road_paint_source::Profile,
    paint_noise: &'a std::cell::RefCell<native_noise::PrefixCache>,
    terrain: &'a road_terrain_source::Profile,
    allowed: i32,
    route_environment: Option<&'a crate::road_socket_recovery::Environment<'a>>,
    rails: Option<&'a crate::rail_topology_stage::Prepared<'a>>,
    rail_receipt: Option<&'a crate::retained_rail_scene::Receipt>,
    checked_base: &'a road_planning::AlignmentBase<'a>,
    pipes: Option<&'a crate::water_treatment_stage::PreparedSources>,
}
#[derive(Clone, serde::Serialize)]
struct RetryBudget {
    shared_generation: bool,
    selection: usize,
    straightening: usize,
    topology: usize,
    height: usize,
    texture: usize,
    fitting: u64,
    water: usize,
    proposals: usize,
    validation: usize,
    final_surfaces: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    rail_topology: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    layout_planning: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    world_rebuilding: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pipes: Option<u64>,
}
impl Default for RetryBudget {
    fn default() -> Self {
        Self {
            shared_generation: true,
            selection: 0,
            straightening: 0,
            topology: 0,
            height: 0,
            texture: 0,
            fitting: 0,
            water: 0,
            proposals: 0,
            validation: 0,
            final_surfaces: 0,
            rail_topology: None,
            layout_planning: None,
            world_rebuilding: None,
            pipes: None,
        }
    }
}
impl RetryBudget {
    fn reserve_world_rebuild(&mut self, work: u64) -> Result<bool, String> {
        if !self.shared_generation || work == 0 {
            return Err("Invalid world rebuild reservation".into());
        }
        if work > self.fitting_remaining() {
            return Ok(false);
        }
        self.world_rebuilding = Some(
            self.world_rebuilding
                .unwrap_or(0)
                .checked_add(work)
                .ok_or("World rebuild work overflow")?,
        );
        Ok(true)
    }
    fn reserve_layout_preparation(&mut self, final_entrance_retry: bool) -> Result<bool, String> {
        let previous = self
            .layout_planning
            .ok_or("Road layout preparation is not enabled")?;
        if previous >= LAYOUT_PREPARATION_WORK * (MAX_LAYOUTS + usize::from(final_entrance_retry))
            || self.fitting_remaining() < LAYOUT_PREPARATION_WORK as u64
        {
            return Ok(false);
        }
        self.layout_planning = Some(
            previous
                .checked_add(LAYOUT_PREPARATION_WORK)
                .ok_or("Road layout preparation work overflow")?,
        );
        Ok(true)
    }
    fn total(&self) -> u64 {
        [
            self.selection,
            self.straightening,
            self.topology,
            self.height,
            self.texture,
            self.water,
            self.proposals,
            self.validation,
            self.final_surfaces,
            self.rail_topology.unwrap_or(0),
            self.layout_planning.unwrap_or(0),
        ]
        .into_iter()
        .fold(
            self.fitting
                .saturating_add(self.world_rebuilding.unwrap_or(0))
                .saturating_add(self.pipes.unwrap_or(0)),
            |total, work| total.saturating_add(work as u64),
        )
    }
    fn fitting_remaining(&self) -> u64 {
        32_000_000_000_u64.saturating_sub(if self.shared_generation {
            self.total()
        } else {
            self.fitting
        })
    }
    fn stage_remaining(&self, cap: usize) -> usize {
        if self.shared_generation {
            cap.min(self.fitting_remaining() as usize)
        } else {
            cap
        }
    }
    fn selection_remaining(&self) -> usize {
        self.stage_remaining(128_000_000)
    }
    fn straightening_remaining(&self) -> usize {
        self.stage_remaining(1_000_000)
    }
    fn paint_remaining(&self) -> usize {
        // Three independently metered primitives share this call's grant.
        if self.shared_generation {
            (self.fitting_remaining() / if self.rail_topology.is_some() { 4 } else { 3 })
                .min(512_000_000) as usize
        } else {
            512_000_000
        }
    }
    fn final_surfaces_remaining(&self) -> usize {
        if self.shared_generation {
            (self.fitting_remaining() / if self.rail_topology.is_some() { 3 } else { 2 })
                .min(512_000_000) as usize
        } else {
            512_000_000
        }
    }
    fn proposal_allowance(&self, cap: usize) -> usize {
        2_000_000
            .min(cap)
            .min(12_000_000_usize.saturating_sub(self.proposals))
            .min(self.fitting_remaining() as usize)
    }
    fn charge_proposal(&mut self, work: usize, grant: usize) -> Result<(), String> {
        if grant > self.proposal_allowance(grant) || work > grant {
            return Err("Socket proposal budget exceeded".into());
        }
        self.proposals = self
            .proposals
            .checked_add(work)
            .ok_or("Socket proposal work overflow")?;
        Ok(())
    }
    fn charge_selection(&mut self, work: usize) -> Result<(), String> {
        if work > self.selection_remaining() {
            return Err("Cumulative roadside selection budget exceeded".into());
        }
        self.selection = self
            .selection
            .checked_add(work)
            .ok_or("Selection work overflow")?;
        Ok(())
    }
    fn charge_straightening(&mut self, work: usize) -> Result<(), String> {
        if work > self.straightening_remaining() {
            return Err("Cumulative road straightening budget exceeded".into());
        }
        self.straightening = self
            .straightening
            .checked_add(work)
            .ok_or("Straightening work overflow")?;
        Ok(())
    }
    fn charge_paint(
        &mut self,
        report: &road_paint_stage::Report<Option<papyrus_terrain::road_paint::Report>>,
    ) -> Result<(), String> {
        let work = [
            report.topology.work,
            report.height.height.work.checked_add(report.roadside_height_work.unwrap_or(0))
                .ok_or("Roadside road-height work overflow")?,
            report.texture.as_ref().map_or(0, |texture| texture.work),
        ];
        if work.iter().any(|&n| n > self.paint_remaining()) {
            return Err("Cumulative road paint budget exceeded".into());
        }
        if let Some(rails) = &report.rail_topology {
            if rails.work > self.paint_remaining() || self.rail_topology.is_none() {
                return Err("Unbudgeted rail topology work".into());
            }
            self.rail_topology = Some(
                self.rail_topology
                    .unwrap()
                    .checked_add(rails.work)
                    .ok_or("Rail topology work overflow")?,
            );
        }
        for (total, work) in [&mut self.topology, &mut self.height, &mut self.texture]
            .into_iter()
            .zip(work)
        {
            *total = total.checked_add(work).ok_or("Paint work overflow")?;
        }
        Ok(())
    }
    fn charge_fitting(&mut self, work: u64) {
        self.fitting = self.fitting.saturating_add(work);
    }
}

struct Attempt {
    validated_routes: Option<usize>,
    maps: Composed,
    controls: Vec<Vec<[f32; 3]>>,
    surfaces: papyrus_terrain::road_network::SurfaceOutput,
}
impl Attempt {
    fn finish(self, plan: road_planning::Plan) -> Composed {
        let mut report = plan.into_report();
        for (road, points) in report.roads.iter_mut().zip(self.controls) {
            road.points = points;
        }
        report.surfaces = Some(self.surfaces);
        let mut maps = self.maps;
        maps.roads = Some(report);
        maps
    }
}
#[derive(Debug)]
enum AttemptFailure {
    Other(String),
    GeometryRejected {
        message: String,
        witness: Option<road_refinement::AlignmentWitness>,
    },
    SelectionBudget {
        allowance: usize,
        work_before: u64,
        detail: String,
    },
    Fitting(road_refinement::Failure),
    Routes(crate::road_socket_recovery::AllRoutesFailure),
}
impl From<String> for AttemptFailure {
    fn from(value: String) -> Self {
        Self::Other(value)
    }
}
impl From<&str> for AttemptFailure {
    fn from(value: &str) -> Self {
        Self::Other(value.into())
    }
}
impl std::fmt::Display for AttemptFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Other(message) => f.write_str(message),
            Self::GeometryRejected { message, .. } => f.write_str(message),
            Self::SelectionBudget {
                allowance,
                work_before,
                detail,
            } => write!(
                f,
                "Roadside trial selection exhausted its {allowance} work allowance; work before selection: {work_before}; {detail}"
            ),
            Self::Fitting(error) => write!(f, "Staged road bed rejected: {error:?}"),
            Self::Routes(error) => write!(f, "Final source road routes rejected: {error:?}"),
        }
    }
}
fn audit_complete_routes(
    environment: &crate::road_socket_recovery::Environment<'_>,
    plan: &road_planning::Plan,
    controls: &[Vec<[f32; 3]>],
    height: &[i16],
    budget: &mut RetryBudget,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<crate::road_socket_recovery::AllRoutesAudit>, AttemptFailure> {
    let allowance = 384_000_000.min(budget.fitting_remaining() as usize);
    if allowance == 0 {
        return Err("No complete source-route validation allowance".into());
    }
    let result =
        environment.validate_all_routes_from_plan(plan, controls, height, allowance, cancelled);
    let work = match &result {
        Ok(Some(audit)) => audit.work,
        Ok(None) => 0,
        Err(error) => error.work,
    };
    budget.validation = budget
        .validation
        .checked_add(work)
        .ok_or("Complete route work overflow")?;
    result.map_err(|error| {
        if (cfg!(feature = "road-diagnostics") || std::env::var_os("MAPPER_COMPOSITION_TIMINGS").is_some())
            && let Some(comparison) = source_ground_comparison(&error, environment.input().layers, height)
        {
            eprintln!("Source road rejected ground comparison: {comparison}");
        }
        AttemptFailure::Routes(error)
    })
}
fn run_attempt(
    prepared: &AttemptContext<'_>,
    plan: &road_planning::Plan,
    base: &Composed,
    budget: &mut RetryBudget,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Attempt>, AttemptFailure> {
    let result = run_attempt_inner(prepared, plan, base, budget, cancelled);
    // Retaining terrain into the next selection is optional. Keep that work
    // bounded independently of the raster needed by the current trial.
    let maximum_bytes = crate::generation_resources::current().terrain_memory_mib * 1024 * 1024 / 8;
    prepared.roadside_terrain.trim(maximum_bytes);
    result
}

fn run_attempt_inner(
    prepared: &AttemptContext<'_>,
    plan: &road_planning::Plan,
    base: &Composed,
    budget: &mut RetryBudget,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Attempt>, AttemptFailure> {
    let mut timing = crate::composition::StageTimer::start("road_composition");
    macro_rules! ready {
        ($e:expr) => {
            match $e? {
                Some(v) => v,
                None => return Ok(None),
            }
        };
    }
    let AttemptContext {
        context:
            Context {
                project,
                solution,
                catalog,
                bindings,
                worker,
            },
        roadside,
        roadside_earthwork,
        roadside_terrain,
        mesh,
        paint,
        paint_noise,
        terrain,
        allowed,
        route_environment,
        rails,
        rail_receipt,
        checked_base,
        pipes,
        shaping: _,
    } = *prepared;
    let root = bindings.installation_root();
    let recipe = project.roads.as_ref().ok_or("Missing staged road recipe")?;
    let preferences = recipe.staged.as_ref()
        .map_or(&[][..], |policy| policy.roadside_preferences.as_slice());
    let mut requests = roadside_placement::captured_requests_with_preferences(roadside, solution, preferences)?;
    requests.retain(|request| project.options.prefab_blocker(&request.path).is_none());
    let placement = roadside_placement::Context {
        project,
        initial: solution,
        catalog,
        bindings,
        maps: base,
        plan,
        source: roadside,
    };
    let selection_grant = budget.selection_remaining();
    let work_before = budget.total();
    let reused_before = timing.as_ref().map(|_| roadside_earthwork.borrow().reused_samples());
    let selection = ready!(match roadside_placement::select_metered(
        placement,
        checked_base,
        &requests,
        roadside_earthwork,
        selection_grant,
        &cancelled
    ) {
        Ok(value) => Ok(value),
        Err(
            error @ (roadside_placement::SelectionFailure::Budget
            | roadside_placement::SelectionFailure::BudgetDetails(_)),
        ) => {
            budget.charge_selection(selection_grant)?;
            Err(AttemptFailure::SelectionBudget {
                allowance: selection_grant,
                work_before,
                detail: error.to_string(),
            })
        }
        Err(error) => Err(AttemptFailure::Other(format!(
            "{error}; work before selection: {work_before}"
        ))),
    });
    budget.charge_selection(selection.work())?;
    crate::composition::record_timing(&mut timing, "roadside_selection");
    if let Some(before) = reused_before {
        eprintln!(
            "MAPPER_ROADSIDE_EARTHWORK {}",
            serde_json::json!({"reused_samples": roadside_earthwork.borrow().reused_samples() - before})
        );
    }
    let stamped = ready!(roadside_stamps::compose_validated(
        selection,
        roadside_terrain,
        budget.straightening_remaining(),
        &cancelled
    ));
    if let Some(guard) = plan.rail_height_guard() {
        let work = guard.samples().len();
        if work > budget.stage_remaining(128_000_000) {
            return Err("Rail stamp validation work exhausted".into());
        }
        if !guard.validate_retained_grid(
            papyrus_terrain::path_terrain::Grid {
                size: project.size as f32,
                resolution: stamped.maps.layers.height_resolution,
                origin_y: paint.height_origin,
                vertical_span: paint.vertical_span,
                height: &stamped.maps.layers.height,
            },
            &cancelled,
        )? {
            return Ok(None);
        }
        budget.validation = budget
            .validation
            .checked_add(work)
            .ok_or("Rail stamp validation work overflow")?;
    }
    budget.charge_straightening(stamped.roads.work)?;
    crate::composition::record_timing(&mut timing, "roadside_stamps");
    let report = plan.planning_report();
    check_connection_contract(
        report,
        recipe
            .staged
            .as_ref()
            .is_some_and(|p| is_raw_source_engine(&p.engine)),
    )?;
    let paths = stamped.roads.roads();
    let roads = paths
        .iter()
        .map(|r| papyrus_terrain::road_paint::Road {
            path: r.path,
            hierarchy: r.hierarchy,
        })
        .collect::<Vec<_>>();
    let mut joins = Vec::new();
    for (i, r) in report.roads.iter().enumerate() {
        for (side, d) in [r.from, r.to].iter().enumerate() {
            if let papyrus_terrain::road_network::Destination::RawNetwork(connection) = *d {
                let endpoint = if side == 0 {
                    0
                } else {
                    paths[i].path.points().len() - 1
                };
                if !connection.matches(i, side, paths.len(), paths[i].path.points()[endpoint]) {
                    return Err("Source endpoint changed during roadside composition".into());
                }
                continue;
            }
            if let papyrus_terrain::road_network::Destination::Network { road, point } = *d {
                joins.push(Junction {
                    a: (
                        i,
                        if side == 0 {
                            0
                        } else {
                            paths[i].path.points().len() - 1
                        },
                    ),
                    b: (road, point),
                });
            }
        }
    }
    let native = native_profile::resolve(&project.terrain_key())?;
    let installation = project
        .installation
        .as_ref()
        .ok_or("Missing staged road installation")?;
    let mut noise = |coordinates: &[f32]| {
        let mut cache = paint_noise.borrow_mut();
        // Only coalesce passes with the exact same distance-to-noise formula.
        let request = if paint.policy.sample_spacing == terrain.policy.sample_spacing
            && paint.policy.noise_frequency == terrain.policy.noise_frequency
        {
            cache.road_grid(
                coordinates,
                paint.policy.sample_spacing,
                paint.policy.noise_frequency,
            )
        } else {
            std::borrow::Cow::Borrowed(coordinates)
        };
        cache
            .sample(root, worker, installation, &native, &request, &cancelled)
            .map(|values| {
                values.map(|mut values| {
                    values.truncate(coordinates.len());
                    values
                })
            })
    };
    // Explicit raw-triangle river collider proxy. No PhysX cooking claim.
    let mut water_work = budget.water;
    let mut world = Vec::new();
    if let Some(rivers) = &stamped.maps.rivers {
        if rivers.water_chunks.len() > 4096 {
            return Err("Too many source road water surfaces".into());
        }
        for chunk in &rivers.water_chunks {
            if cancelled() {
                return Ok(None);
            }
            water_work = water_work
                .checked_add(
                    chunk.geometry.mesh.vertices.len() + chunk.geometry.mesh.triangles.len(),
                )
                .filter(|v| *v <= 4_194_304)
                .ok_or("Road water conversion budget exceeded")?;
            world.push(
                chunk
                    .geometry
                    .mesh
                    .vertices
                    .iter()
                    .map(|p| std::array::from_fn(|i| f64::from(p[i] + chunk.geometry.origin[i])))
                    .collect::<Vec<_>>(),
            );
        }
    }
    let surfaces = if let Some(rivers) = &stamped.maps.rivers {
        rivers
            .water_chunks
            .iter()
            .zip(&world)
            .map(|(chunk, vertices)| {
                papyrus_terrain::water_mesh::Surface::new(vertices, &chunk.geometry.mesh.triangles)
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };
    let mut water_height = |p: [f32; 3]| {
        let mut level: Option<f64> = None;
        for surface in &surfaces {
            if cancelled() {
                return Ok(None);
            }
            water_work = water_work
                .checked_add(1)
                .filter(|v| *v <= 128_000_000)
                .ok_or("Road water query budget exceeded")?;
            if let Some(value) = surface.highest(
                f64::from(p[0]),
                f64::from(p[2]),
                stamped
                    .maps
                    .rivers
                    .as_ref()
                    .is_some_and(|r| r.water_policy.two_sided),
                &mut water_work,
                128_000_000,
                &cancelled,
            )? {
                level = Some(level.map_or(value, |old| old.max(value)));
            }
        }
        Ok(Some(level.map_or(terrain.water_no_hit_level, |v| v as f32)))
    };
    let paint_input = road_paint_stage::Input {
        layers: &stamped.maps.layers,
        roads: &roads,
        junctions: &joins,
        paint,
        terrain,
    };
    let mut pipe_commit = None;
    let preliminary_texture = cfg!(feature = "road-diagnostics") || recipe.staged.as_ref()
        .is_none_or(|staged| !uses_source_cost_guidance(&staged.engine));
    let maximum_work = budget.paint_remaining();
    let mut painted = ready!(road_paint_stage::paint_for_composition(
        paint_input, rails, maximum_work, &mut noise, &mut water_height,
        |frame| {
            let Some(pipes) = pipes else { return Ok(None); };
            let objects = project.objects.as_ref().ok_or("Missing source pipe recipe")?;
            let settings = crate::water_treatment_stage::Settings {
                seed: project.seeds[5].value,
                realm_mask: objects.realm_mask,
                maximum_terrain_work: objects.maximum_terrain_work,
                maximum_sampling_attempts: objects.maximum_sampling_attempts,
                maximum_fitting_samples: 64_000_000,
                maximum_clearance_work: 64_000_000,
            };
            let grant = (settings.maximum_terrain_work as u64) * 3
                + settings.maximum_sampling_attempts as u64
                + settings.maximum_fitting_samples as u64
                + settings.maximum_clearance_work as u64 + 32_000_000;
            if grant > budget.fitting_remaining() {
                return Err("No complete pipe placement work allowance remains".into());
            }
            let before = budget.pipes.unwrap_or(0);
            budget.pipes = Some(before.checked_add(grant).ok_or("Pipe work overflow")?);
            let Some((changes, committed)) = pipes.place(crate::water_treatment_stage::Boundary {
                project, solution, roadside: &stamped.report.placements,
                original: &stamped.maps.layers, alpha: &stamped.maps.alpha, frame, paint,
            }, settings, &cancelled)? else {
                return Ok(None);
            };
            let validation_work = changes.height.as_ref().map_or(0, Vec::len)
                + changes.splat.as_ref().map_or(0, Vec::len);
            let used = committed.work().saturating_add(validation_work as u64);
            if used > grant { return Err("Pipe placement exceeded its reserved work".into()); }
            budget.pipes = Some(before + used);
            pipe_commit = Some(committed);
            Ok(Some(changes))
        }, preliminary_texture, &cancelled,
    ));
    #[cfg(feature = "road-diagnostics")]
    if let Some(destination) = std::env::var_os("MAPPER_COMBINED_SOURCE_PROBE") {
        let rail_paths = rails.map_or(&[][..], |source| source.retained().paths());
        let sample = std::env::var("MAPPER_COMBINED_SAMPLE")
            .map_err(|e| e.to_string())?
            .parse::<usize>()
            .map_err(|e| e.to_string())?;
        if sample >= painted.height.len() {
            return Err("Combined probe sample is outside the grid".into());
        }
        let inputs = roads
            .iter()
            .map(|r| {
                let shape = paint
                    .policy
                    .families
                    .iter()
                    .find(|f| f.hierarchy == r.hierarchy)
                    .ok_or("Missing probe road shape")?;
                let family = terrain
                    .families
                    .iter()
                    .find(|f| f.hierarchy == r.hierarchy)
                    .ok_or("Missing probe terrain shape")?;
                Ok(papyrus_terrain::road_terrain::InputPath {
                    path: r.path,
                    shape: papyrus_terrain::path_terrain::Shape {
                        width: shape.width,
                        padding: shape.padding,
                        fade: family.fade,
                    },
                    terrain_offset: family.terrain_offset,
                    random_scale: shape.random_scale,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let run = |roads, rails, noise: &mut _, water: &mut _| {
            papyrus_terrain::road_terrain::run(
                papyrus_terrain::road_terrain::Input {
                    grid: papyrus_terrain::path_terrain::Grid {
                        size: project.size as f32,
                        resolution: stamped.maps.layers.height_resolution,
                        origin_y: paint.height_origin,
                        vertical_span: paint.vertical_span,
                        height: &stamped.maps.layers.height,
                    },
                    topology: &painted.topology,
                    surface_resolution: stamped.maps.layers.surface_resolution,
                    roads,
                    rails,
                },
                &terrain.policy,
                512_000_000,
                noise,
                water,
                &cancelled,
            )
        };
        let rail_only = ready!(run(&[], rail_paths, &mut noise, &mut water_height));
        let rail_sample = rail_only.height[sample];
        drop(rail_only);
        let road_only = ready!(run(&inputs, &[], &mut noise, &mut water_height));
        let road_sample = road_only.height[sample];
        drop(road_only);
        let hr = stamped.maps.layers.height_resolution;
        let world = [
            (sample % hr) as f32 / (hr - 1) as f32 * project.size as f32
                - project.size as f32 * 0.5,
            (sample / hr) as f32 / (hr - 1) as f32 * project.size as f32
                - project.size as f32 * 0.5,
        ];
        let report = serde_json::json!({"format":"mapper-combined-source-point-probe-1","sample":sample,"world":world,
            "before_roadside_units":base.layers.height[sample],"after_roadside_units":stamped.maps.layers.height[sample],
            "rail_only_source_units":rail_sample,"road_only_source_units":road_sample,"combined_source_units":painted.height[sample],
            "roadside_placements":stamped.report.placements,"initial_outcomes":solution.outcomes.iter().map(|o|
                serde_json::json!({"id":o.id,"position":o.position.as_ref().map(|p|[p.x,p.y,p.z,p.yaw])})).collect::<Vec<_>>(),
            "rail_source":rails.map(|source|source.retained().receipt()),"rail_path_count":rail_paths.len(),"paint_source_sha256":paint.sha256()?,"terrain_source_sha256":terrain.sha256()?,
            "scope":"Same stamped input and topology, separate source height contributions; not a replay or accepted world"});
        let destination = std::path::PathBuf::from(destination);
        if destination
            .extension()
            .is_none_or(|e| !e.eq_ignore_ascii_case("json"))
        {
            return Err("Combined probe needs a new JSON path".into());
        }
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)
            .map_err(|e| e.to_string())?;
        serde_json::to_writer_pretty(file, &report).map_err(|e| e.to_string())?;
    }
    #[cfg(feature = "road-diagnostics")]
    if let Some(destination) = std::env::var_os("MAPPER_SOURCE_ROAD_CAPTURE") {
        budget.charge_paint(&painted.report)?;
        let replay = if std::env::var_os("MAPPER_SOURCE_ROAD_RESTORE_ENDPOINTS").is_some() {
            if rails.is_some() {
                return Err("Endpoint replay requires a road-only revision".into());
            }
            Some(ready!(crate::road_source_diagnostics::restore_endpoints(
                report, &roads, &cancelled
            )))
        } else {
            None
        };
        let replay_roads = replay.as_ref().map(|r| {
            r.paths
                .iter()
                .zip(&roads)
                .map(|(path, old)| papyrus_terrain::road_paint::Road {
                    path,
                    hierarchy: old.hierarchy,
                })
                .collect::<Vec<_>>()
        });
        let replay_paint = if let Some(roads) = &replay_roads {
            let result = ready!(road_paint_stage::paint_for_composition(
                road_paint_stage::Input {
                    layers: &stamped.maps.layers,
                    roads,
                    junctions: &[],
                    paint,
                    terrain
                },
                None, budget.paint_remaining(),
                &mut noise,
                &mut water_height,
                |_| Ok(None), true,
                &cancelled
            ));
            budget.charge_paint(&result.report)?;
            Some(result)
        } else {
            None
        };
        ready!(crate::road_source_diagnostics::capture(
            crate::road_source_diagnostics::Input {
                context: Context {
                    project,
                    solution,
                    catalog,
                    bindings,
                    worker
                },
                base: &stamped.maps,
                roads: report,
                before_height: replay_roads.as_deref().unwrap_or(&roads),
                painted: replay_paint.as_ref().unwrap_or(&painted),
                mesh,
                paint,
                endpoint_replay: replay.as_ref().map(|r| r.receipt.clone())
            },
            Path::new(&destination),
            &cancelled
        ));
        return Err("Source road stages captured; generation intentionally stopped before Mapper fitting. No map accepted".into());
    }
    budget.water = water_work;
    budget.charge_paint(&painted.report)?;
    crate::composition::record_timing(&mut timing, "paint");
    let staged = recipe.staged.as_ref();
    let source_completion = staged.is_some_and(|policy| is_source_engine(&policy.engine));
    let source_controls = source_completion.then(|| {
        painted
            .roads
            .iter()
            .map(|path| path.points().to_vec())
            .collect::<Vec<_>>()
    });
    // Source construction publishes these controls and this exact height
    // raster. Reject bad footprints before generating any collision meshes.
    let early_route_audit = if let (Some(controls), Some(environment)) =
        (source_controls.as_ref(), route_environment)
        && staged.is_some_and(|policy| uses_source_cost_guidance(&policy.engine))
    {
        Some(ready!(audit_complete_routes(
            environment,
            plan,
            controls,
            &painted.height,
            budget,
            &cancelled
        )))
    } else {
        None
    };
    let retained_height = if let Some(rails) = rails {
        let held = ready!(rails.retained().height_support_on_grid(
            &stamped.maps.layers,
            &painted.height,
            road_surface_stage::HeightGeometry {
                origin_y: paint.height_origin,
                vertical_span: paint.vertical_span
            },
            budget.stage_remaining(128_000_000),
            &cancelled
        ));
        budget.validation = budget
            .validation
            .checked_add(held.preparation_work)
            .ok_or("Retained height work overflow")?;
        Some(held)
    } else {
        None
    };
    let refined = if let Some(controls) = source_controls {
        let allowance = budget
            .fitting_remaining()
            .min(crate::source_road_materialization::MAX_WORK);
        ready!(
            crate::source_road_materialization::materialize(
                road_surface_stage::Context {
                    project,
                    root,
                    worker,
                    layers: &stamped.maps.layers
                },
                report,
                mesh,
                road_surface_stage::HeightGeometry {
                    origin_y: paint.height_origin,
                    vertical_span: paint.vertical_span
                },
                std::mem::take(&mut painted.height),
                early_route_audit
                    .as_ref()
                    .and_then(|audit| audit.constructed_ground_sha256.as_deref()),
                controls,
                allowance,
                &cancelled,
            )
            .map_err(|error| {
                budget.charge_fitting(allowance);
                match error {
                    crate::source_road_materialization::Failure::Geometry {
                        road,
                        position,
                        ..
                    } => AttemptFailure::GeometryRejected {
                        message: error.to_string(),
                        witness: position.map(|p| road_refinement::AlignmentWitness {
                            kind: road_refinement::AlignmentKind::Curvature,
                            road,
                            position: [p[0], p[2]],
                        }),
                    },
                    crate::source_road_materialization::Failure::Invalid(message) => {
                        AttemptFailure::Other(message)
                    }
                }
            })
        )
    } else {
        let settings = road_refinement::Settings {
            fit: road_bed::Settings {
                maximum_grade: recipe.maximum_grade,
                maximum_cut_fill: recipe.maximum_cut_fill,
                minimum_up_normal: 0.5,
                maximum_iterations: 512,
                maximum_work: 2_000_000_000,
                quantization_reserve: 0.51,
                reserve_roundoff: true,
            },
            clearance: road_clearance::Settings {
                tolerance: 0.02,
                minimum_up_normal: 0.5,
                maximum_work: 10_000_000,
                maximum_probes: 131072,
            },
            maximum_rounds: 32,
            maximum_total_work: budget.fitting_remaining(),
            maximum_reprojection_distance: 0.25,
        };
        let refined = ready!(
            road_refinement::refine_painted_with_source(
                road_surface_stage::Context {
                    project,
                    root,
                    worker,
                    layers: &stamped.maps.layers
                },
                report,
                mesh,
                road_refinement::PaintedSnapshot {
                    source_rounding: Some(&painted.source_rounding),
                    retained_height: retained_height.as_ref(),
                    height: &painted.height,
                    topology: &painted.topology,
                    surface_resolution: stamped.maps.layers.surface_resolution,
                    paths: &painted.roads,
                    paint,
                    terrain,
                    allowed_topology: allowed,
                    maximum_initial_work: 512_000_000
                },
                settings,
                |_| {},
                &cancelled
            )
            .map_err(|error| {
                budget.charge_fitting(error.charged_work(settings.maximum_total_work));
                AttemptFailure::Fitting(error)
            })
        );
        crate::source_road_materialization::Output::fitted(refined)?
    };
    budget.charge_fitting(refined.work);
    crate::composition::record_timing(&mut timing, "validation_and_meshes");
    let corrected = refined
        .controls
        .iter()
        .map(|p| {
            papyrus_terrain::path_geometry::Path::from_controls(p, 32_000_000, &cancelled)
                .map_err(|e| format!("Corrected road path: {e:?}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let Some(corrected) = corrected.into_iter().collect::<Option<Vec<_>>>() else {
        return Ok(None);
    };
    let mut source_curve_count = 0;
    // Raw-source completion compares the original routing grid for every
    // family in the final footprint audit. Do not reintroduce a separate hard
    // pre-shaping-grid requirement only for its main ring.
    if recipe
        .staged
        .as_ref()
        .is_none_or(|p| !uses_source_cost_guidance(&p.engine))
        && let Some((launch, _)) = plan.checked_ring_costs()
    {
        for (path, original) in corrected.iter().zip(&paths) {
            if original.hierarchy != 0 {
                continue;
            }
            let allowance = 2_000_000.min(budget.fitting_remaining() as usize);
            if allowance == 0 {
                return Err("No final source-curve validation allowance".into());
            }
            match plan.check_ring_curve(
                path,
                project.size,
                launch.sample_spacing,
                allowance,
                &cancelled,
            ) {
                Ok(Some(audit)) => {
                    budget.validation = budget
                        .validation
                        .checked_add(audit.work)
                        .ok_or("Final source-curve work overflow")?;
                    if !audit.clear {
                        return Err(AttemptFailure::GeometryRejected {
                            message: "Final main-road curve leaves the source routing corridor"
                                .into(),
                            witness: None,
                        });
                    }
                    source_curve_count += 1;
                }
                Ok(None) => return Ok(None),
                Err(error) => {
                    budget.validation = budget
                        .validation
                        .checked_add(allowance)
                        .ok_or("Final source-curve work overflow")?;
                    return Err(format!("Final source-curve check failed: {error:?}").into());
                }
            }
        }
    }
    let mut complete_route_audit = if early_route_audit.is_some() {
        early_route_audit
    } else if let Some(environment) = route_environment {
        Some(ready!(audit_complete_routes(
            environment,
            plan,
            &refined.controls,
            &refined.height,
            budget,
            &cancelled
        )))
    } else {
        None
    };
    let final_roads = corrected
        .iter()
        .zip(&paths)
        .map(|(path, old)| papyrus_terrain::road_paint::Road {
            path,
            hierarchy: old.hierarchy,
        })
        .collect::<Vec<_>>();
    let retained = match (rails, rail_receipt, retained_height.as_ref()) {
        (Some(stage), Some(receipt), Some(height)) => {
            Some(road_paint_stage::RetainedSurfaceContext {
                recipe: project
                    .rails
                    .as_ref()
                    .ok_or("Missing retained rail recipe")?,
                scene_receipt: receipt,
                height,
                rail_stages: Some(stage),
            })
        }
        (None, None, None) => None,
        _ => return Err("Incomplete retained road surface context".into()),
    };
    // Source materialization returns the painted heights unchanged. The fitted
    // branch audits the refined grid directly. Late earthwork invalidates
    // either mask in the final surface stage.
    let audited_ocean = complete_route_audit
        .as_mut()
        .and_then(|audit| audit.constructed_ocean.take());
    let final_surfaces = ready!(road_paint_stage::finish_surfaces_with_ocean_mask(
        road_paint_stage::FinalSurfaceInput {
            base: &stamped.maps,
            height: &refined.height,
            roads: &final_roads,
            profile: paint,
            retained
        },
        audited_ocean,
        budget.final_surfaces_remaining(),
        &mut noise,
        |frame| match &pipe_commit {
            Some(committed) => committed.preserve(frame, &cancelled),
            None => Ok(None),
        },
        &cancelled
    ));
    budget.final_surfaces = budget
        .final_surfaces
        .checked_add(
            final_surfaces
                .report
                .topology
                .work
                .checked_add(final_surfaces.report.texture.work)
                .and_then(|v| {
                    v.checked_add(
                        final_surfaces
                            .report
                            .rail_topology
                            .as_ref()
                            .map_or(0, |r| r.work),
                    )
                })
                .and_then(|v| {
                    v.checked_add(final_surfaces.report.retained_height_checks.unwrap_or(0))
                })
                .and_then(|v| {
                    v.checked_add(final_surfaces.report.retained_curve_checks.unwrap_or(0))
                })
                .ok_or("Final surface work overflow")?,
        )
        .ok_or("Cumulative final surface work overflow")?;
    if budget.shared_generation && budget.total() > 32_000_000_000 {
        return Err("Total road generation work budget exceeded".into());
    }
    crate::composition::record_timing(&mut timing, "final_surfaces");
    drop(surfaces);
    drop(paths);
    drop(roads);
    drop(stamped.maps);
    let mut maps = roadside_terrain.take()?;
    maps.layers.height = final_surfaces.height.unwrap_or(refined.height);
    maps.layers.topology = final_surfaces.topology;
    maps.layers.splat = final_surfaces.splat;
    maps.ocean = final_surfaces.ocean;
    let validated_routes = complete_route_audit.as_ref().map(|audit| audit.roads);
    let mut surfaces = refined.surfaces;
    surfaces.report["staged_pipeline"] = serde_json::json!({"engine":project.roads.as_ref().and_then(|r|r.staged.as_ref()).map_or(VERSION,|p|p.engine.as_str()),"roadside_source_sha256":roadside.sha256,
        "paint_envelope_allowed_topology":allowed,
        "roadside":stamped.report,"paint":painted.report,"bed":refined.report,"final_surfaces":final_surfaces.report,
        "height_origin":paint.height_origin,"vertical_span":paint.vertical_span,
        "post_decoration_validated":false,"water_proxy":"raw river triangles and bound no-hit level; PhysX cooking not validated",
        "main_curves_source_cost_checked":source_curve_count,
        "source_route_footprints":complete_route_audit,
        "runtime_access_validated":false,"playable":false});
    if source_completion {
        surfaces.report["staged_pipeline"]["completion_contract"] =
            recipe.staged.as_ref().unwrap().engine.clone().into();
    }
    if let Some(committed) = pipe_commit {
        surfaces.report["staged_pipeline"]["pipes"] = serde_json::to_value(committed.receipt)
            .map_err(|error| error.to_string())?;
    }
    if let Some(guard) = plan.rail_height_guard() {
        surfaces.report["staged_pipeline"]["rail_stamp_protection"] = serde_json::json!({
            "engine":"papyrus-roadside-retained-rail-height-1","samples":guard.samples().len(),
            "pre_paint_stamps_preserved":true,"scope":"exact encoded rail support before source road/rail terrain loops"});
    }
    Ok((!cancelled()).then_some(Attempt {
        validated_routes,
        maps,
        controls: refined.controls,
        surfaces,
    }))
}
pub(crate) struct VerifiedRoadSurface {
    height: road_surface_stage::MeshHeightSupport,
    report: serde_json::Value,
}

/// Downstream height writers must not silently invalidate the accepted roads.
/// The surface proof belongs to the same composition as the reservations.
pub(crate) fn height_reservations(
    context: Context<'_>,
    maps: &Composed,
    cancelled: impl Fn() -> bool,
) -> Result<Option<(
    papyrus_terrain::height_reservations::HeightReservations,
    Option<VerifiedRoadSurface>,
)>, String> {
    let recipe = context
        .project
        .roads
        .as_ref()
        .ok_or("Missing staged roads")?;
    let roads = maps.roads.as_ref().ok_or("Missing accepted roads")?;
    let accepted = roads
        .surfaces
        .as_ref()
        .ok_or("Missing accepted road surfaces")?;
    let stage = &accepted.report["staged_pipeline"];
    let geometry = road_surface_stage::HeightGeometry {
        origin_y: stage["height_origin"]
            .as_f64()
            .ok_or("Missing road origin")? as f32,
        vertical_span: stage["vertical_span"].as_f64().ok_or("Missing road span")? as f32,
    };
    let root = context.bindings.installation_root();
    let parent = infrastructure::Source::cached_shared(context.catalog, recipe, &cancelled)?;
    let source = road_surface_source::Source::cached(
        root,
        recipe
            .surface_sha256
            .as_ref()
            .ok_or("Missing road surface pin")?,
        &parent,
        &cancelled,
    )?;
    let stage_context = road_surface_stage::Context {
        project: context.project,
        root,
        worker: context.worker,
        layers: &maps.layers,
    };
    let source_completion = recipe.staged.as_ref().is_some_and(|p| is_source_engine(&p.engine));
    let traced = if source_completion {
        road_surface_stage::generate_traced_with_support(
            stage_context, roads, &source, geometry, &cancelled,
        )?
    } else {
        road_surface_stage::generate_traced_with_height_geometry(
            stage_context, roads, &source, geometry, &cancelled,
        )?.map(|output| (output, None))
    };
    let Some((rebuilt, support)) = traced else {
        return Ok(None);
    };
    if !same_surface_chunks(&accepted.chunks, &rebuilt.surfaces.chunks) {
        return Err("Road terrain changed before reservation construction".into());
    }
    let grid = papyrus_terrain::path_terrain::Grid {
        size: maps.layers.size as f32,
        resolution: maps.layers.height_resolution,
        origin_y: geometry.origin_y,
        vertical_span: geometry.vertical_span,
        height: &maps.layers.height,
    };
    let Some(mut reservations) =
        papyrus_terrain::height_reservations::HeightReservations::new(grid, &cancelled)?
    else {
        return Ok(None);
    };
    let mut work = maps
        .layers
        .height
        .len()
        .checked_add(
            rebuilt.surfaces.report["work"]
                .as_u64()
                .ok_or("Missing road guard work")? as usize,
        )
        .ok_or("Road guard work overflow")?;
    let limit = 64_000_000_usize;
    for chunk in rebuilt.surfaces.chunks.iter().filter(|c| c.lod == 0) {
        let remaining = limit
            .checked_sub(work)
            .filter(|&v| v > 0)
            .ok_or("Road guard work exhausted")?
            .min(10_000_000);
        match papyrus_terrain::road_clearance::reserve_height_cells(
            papyrus_terrain::road_clearance::Input {
                grid,
                chunk: &chunk.geometry,
                template: &source.meshes[0].buffer,
                road: chunk.road,
                chunk_index: chunk.chunk_index,
            },
            &mut reservations,
            remaining,
            &cancelled,
        ) {
            Ok(used) => work += used,
            Err(papyrus_terrain::road_clearance::Failure::Cancelled) => return Ok(None),
            Err(error) => return Err(format!("Road guard rasterization: {error:?}")),
        }
    }
    let mut reserve = |position| -> Result<bool, String> {
        if cancelled() {
            return Ok(false);
        }
        work = work
            .checked_add(8)
            .filter(|&v| v <= limit)
            .ok_or("Road guard work exhausted")?;
        reservations.reserve_query(position)?;
        Ok(true)
    };
    for trace in &rebuilt.traces {
        for binding in &trace.heights {
            match *binding {
                papyrus_terrain::path_mesh::HeightBinding::Endpoint { position } => {
                    if !reserve(position)? {
                        return Ok(None);
                    }
                }
                papyrus_terrain::path_mesh::HeightBinding::Span { left, right, .. } => {
                    if !reserve(left)? || !reserve(right)? {
                        return Ok(None);
                    }
                }
            }
        }
    }
    for point in roads.roads.iter().flat_map(|r| &r.points) {
        if !reserve([point[0], point[2]])? {
            return Ok(None);
        }
    }
    if recipe
        .entrance_launch
        .is_some_and(|p| p.engine == papyrus_terrain::road_network::LaunchEngine::GlobalEarthwork)
    {
        let Some(used) = reserve_dirt_corridors(
            &mut reservations,
            roads,
            limit.saturating_sub(work),
            &cancelled,
        )? else { return Ok(None); };
        work += used;
    }
    if crate::water_treatment_stage::reserve_heights(
        &stage["pipes"], &mut reservations, grid, limit.saturating_sub(work), &cancelled,
    )?.is_none() {
        return Ok(None);
    }
    let surface = support.map(|height| VerifiedRoadSurface {
        height,
        report: rebuilt.surfaces.report,
    });
    Ok(Some((reservations, surface)))
}
/// Dirt has no collision mesh to supply the swept terrain dependencies above.
/// Keep its complete source terrain footprint immutable for later height writers.
fn reserve_dirt_corridors(
    reservations: &mut papyrus_terrain::height_reservations::HeightReservations,
    roads: &papyrus_terrain::road_network::Report,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<usize>, String> {
    if roads.roads.len() > 256
        || roads.families.len() > 256
        || maximum_work == 0
        || maximum_work > 64_000_000
    {
        return Err("Invalid dirt corridor reservation inputs".into());
    }
    let stage = &roads
        .surfaces
        .as_ref()
        .ok_or("Missing road corridor provenance")?
        .report["staged_pipeline"]["paint"];
    let paint = road_paint_source::Profile::from_receipt(
        stage["paint_profile_sha256"]
            .as_str()
            .ok_or("Missing road corridor paint receipt")?,
    )?;
    let terrain = road_terrain_source::Profile::for_paint(&paint)?
        .ok_or("Missing road corridor terrain profile")?;
    if stage["terrain_profile_sha256"] != terrain.sha256()? {
        return Err("Road corridor terrain receipt differs".into());
    }
    let mut work = 0usize;
    let mut coverage = vec![false; roads.roads.len()];
    for family in &roads.families {
        if cancelled() {
            return Ok(None);
        }
        let end = family
            .first_road
            .checked_add(family.road_count)
            .filter(|v| *v <= roads.roads.len())
            .ok_or("Invalid road corridor family range")?;
        for covered in &mut coverage[family.first_road..end] {
            if std::mem::replace(covered, true) {
                return Err("Overlapping road corridor families".into());
            }
        }
        if family.hierarchy != 2 {
            continue;
        }
        let shape = paint
            .policy
            .families
            .iter()
            .find(|f| u32::from(f.hierarchy) == family.hierarchy)
            .ok_or("Missing dirt corridor source shape")?;
        let height = terrain
            .families
            .iter()
            .find(|f| f.hierarchy == shape.hierarchy)
            .ok_or("Missing dirt corridor height shape")?;
        if shape.width != family.width || shape.splat_channel != Some(family.splat_channel) {
            return Err("Dirt corridor differs from source family".into());
        }
        // Source GetRadius interpolates between these radii with a clamped
        // noise factor. Outer padding/fade are still terrain dependencies even
        // where no dirt texture or road collision mesh is emitted.
        let radius =
            shape.width * 0.5 * shape.random_scale.max(1.) + shape.padding[1] + height.fade[1];
        for road in &roads.roads[family.first_road..end] {
            let allowance = maximum_work.saturating_sub(work).min(32_000_000);
            let Some(path) = papyrus_terrain::path_geometry::Path::from_controls(
                &road.points,
                allowance,
                &cancelled,
            )
            .map_err(|e| format!("Dirt corridor controls: {e:?}"))?
            else {
                return Ok(None);
            };
            work = work
                .checked_add(path.work())
                .filter(|v| *v <= maximum_work)
                .ok_or("Dirt corridor work exhausted")?;
            let Some(used) = reservations.reserve_path(
                &path,
                radius,
                maximum_work.saturating_sub(work).min(64_000_000),
                &cancelled,
            )?
            else {
                return Ok(None);
            };
            work = work
                .checked_add(used)
                .filter(|v| *v <= maximum_work)
                .ok_or("Dirt corridor work exhausted")?;
        }
    }
    if coverage.iter().any(|v| !v) {
        return Err("Incomplete road corridor family coverage".into());
    }
    Ok(Some(work))
}
/// Downstream height writers must not silently invalidate the accepted roads.
pub(crate) fn validate_final(
    context: Context<'_>,
    maps: &mut Composed,
    verified_surface: Option<VerifiedRoadSurface>,
    cancelled: impl Fn() -> bool,
) -> Result<bool, String> {
    if cancelled() {
        return Ok(false);
    }
    let recipe = context
        .project
        .roads
        .as_ref()
        .ok_or("Missing staged road recipe")?;
    let report = maps.roads.as_ref().ok_or("Missing staged roads")?;
    check_connection_contract(
        report,
        recipe
            .staged
            .as_ref()
            .is_some_and(|p| is_raw_source_engine(&p.engine)),
    )?;
    let accepted = report
        .surfaces
        .as_ref()
        .ok_or("Missing staged road geometry")?;
    let data = &accepted.report["staged_pipeline"];
    let geometry = road_surface_stage::HeightGeometry {
        origin_y: data["height_origin"]
            .as_f64()
            .ok_or("Missing road height origin")? as f32,
        vertical_span: data["vertical_span"]
            .as_f64()
            .ok_or("Missing road height span")? as f32,
    };
    for point in report.roads.iter().flat_map(|r| &r.points) {
        if cancelled() {
            return Ok(false);
        }
        if papyrus_terrain::path_terrain::sample_height(
            papyrus_terrain::path_terrain::Grid {
                size: maps.layers.size as f32,
                resolution: maps.layers.height_resolution,
                origin_y: geometry.origin_y,
                vertical_span: geometry.vertical_span,
                height: &maps.layers.height,
            },
            *point,
        )?
        .to_bits()
            != point[1].to_bits()
        {
            return Err("A downstream terrain stage changed staged road controls; road-aware object placement is required".into());
        }
    }
    let parent = infrastructure::Source::cached_shared(context.catalog, recipe, &cancelled)?;
    let root = context.bindings.installation_root();
    let source = road_surface_source::Source::cached(
        root,
        recipe
            .surface_sha256
            .as_ref()
            .ok_or("Missing road surface pin")?,
        &parent,
        &cancelled,
    )?;
    let source_completion = recipe
        .staged
        .as_ref()
        .is_some_and(|p| is_source_engine(&p.engine));
    if data["engine"].as_str() != recipe.staged.as_ref().map(|p| p.engine.as_str()) {
        return Err("Road completion contract differs from saved recipe".into());
    }
    let expected =
        road_surface_stage::mesh_roads(report, source.policy().maximum_hierarchy_exclusive)?;
    if source_completion
        && let Some(verified) = verified_surface
        && verified.report["source_sha256"].as_str() == Some(source.sha256.as_str())
        && verified.height.matches(&maps.layers.height, &cancelled)
    {
        if cancelled() {
            return Ok(false);
        }
        if verified.report["noise_samples"].as_u64().is_some_and(|n| n > 0) {
            // Reuse the native noise samples from this composition only while
            // the pinned RustNative file still matches the selected installation.
            let native = &context.project.installation.as_ref()
                .ok_or("Missing road surface installation")?.native;
            let current = crate::installation::hash_file(
                root, &native.relative_path, 64 * 1024 * 1024, &cancelled,
            );
            if cancelled() {
                return Ok(false);
            }
            if current? != *native {
                return Err("RustNative changed since road surfaces were built".into());
            }
        }
        validate_source_completion(data)?;
        road_surface_stage::collision_coverage(&expected, accepted)?;
        crate::source_road_materialization::validate_geometry(&expected, accepted)
            .map_err(|error| error.to_string())?;
        let accepted = maps.roads.as_mut().unwrap().surfaces.as_mut().unwrap();
        let final_signature = surface_geometry_sha256(&accepted.chunks)?;
        accepted.report = final_surface_report(
            &accepted.report, verified.report, None, false, final_signature,
        );
        crate::composition::record_timing(
            &mut crate::composition::StageTimer::start("road_surface_validation"),
            "reused_height_support",
        );
        return Ok(!cancelled());
    }
    let stage_context = road_surface_stage::Context {
        project: context.project,
        root,
        worker: context.worker,
        layers: &maps.layers,
    };
    let (rebuilt, audit) = if source_completion {
        validate_source_completion(data)?;
        let Some(rebuilt) = road_surface_stage::generate_traced_with_height_geometry(
            stage_context,
            report,
            &source,
            geometry,
            &cancelled,
        )?
        else {
            return Ok(false);
        };
        (rebuilt, None)
    } else {
        let Some((rebuilt, audit)) = road_surface_stage::generate_audited_with_height_geometry(
            stage_context,
            report,
            &source,
            geometry,
            road_clearance::Settings {
                tolerance: 0.02,
                minimum_up_normal: 0.5,
                maximum_work: 10_000_000,
                maximum_probes: 131072,
            },
            &cancelled,
        )?
        else {
            return Ok(false);
        };
        (rebuilt, Some(audit))
    };
    publish_final_surfaces_for_contract(
        maps.roads.as_mut().unwrap().surfaces.as_mut().unwrap(),
        rebuilt.surfaces,
        audit.as_ref(),
        &expected,
        source_completion,
    )?;
    Ok(!cancelled())
}

pub(crate) fn check_connection_contract(
    report: &papyrus_terrain::road_network::Report,
    raw: bool,
) -> Result<(), String> {
    use papyrus_terrain::road_network::Destination;
    if raw
        && report.source_layout.as_ref().is_none_or(|r| {
            r["engine"] != crate::source_road_handoff::RAW_VERSION
                || r["raw_endpoints_preserved"] != true
        })
    {
        return Err("Raw source roads require their matching handoff receipt".into());
    }
    let mut proofs = Vec::new();
    if report.roads.len() > 4096 {
        return Err("Raw source road allowance exceeded".into());
    }
    for (i, road) in report.roads.iter().enumerate() {
        for (side, destination) in [road.from, road.to].into_iter().enumerate() {
            match destination {
                Destination::RawNetwork(connection) => {
                    let p = if side == 0 {
                        road.points.first()
                    } else {
                        road.points.last()
                    }
                    .ok_or("Empty raw source road")?;
                    if !raw || !connection.matches(i, side, report.roads.len(), *p) {
                        return Err("Raw source connection contract or anchor differs".into());
                    }
                    proofs.push(serde_json::to_value(connection).map_err(|e| e.to_string())?);
                }
                Destination::Network { .. } if raw => {
                    return Err("Raw source contract contains an indexed join".into());
                }
                Destination::Network {
                    road: parent,
                    point,
                } => {
                    let endpoint = if side == 0 {
                        road.points.first()
                    } else {
                        road.points.last()
                    }
                    .ok_or("Empty indexed road")?;
                    let target = report
                        .roads
                        .get(parent)
                        .filter(|_| parent < i)
                        .and_then(|r| r.points.get(point))
                        .ok_or("Indexed road parent is missing or out of dependency order")?;
                    if endpoint
                        .iter()
                        .zip(target)
                        .any(|(a, b)| !a.is_finite() || !b.is_finite() || (*a - *b).abs() > 0.0001)
                    {
                        return Err("Indexed road endpoint differs from its retained parent".into());
                    }
                }
                _ => (),
            }
        }
    }
    if raw {
        let stored = report
            .source_layout
            .as_ref()
            .and_then(|r| r["raw_connections"].as_array());
        if stored.map_or(&[][..], Vec::as_slice) != proofs.as_slice() {
            return Err("Raw source connection receipt coverage or identity differs".into());
        }
    }
    Ok(())
}
fn validate_source_completion(data: &serde_json::Value) -> Result<(), String> {
    let bed = &data["bed"];
    if !data["engine"].as_str().is_some_and(is_source_engine)
        || data["completion_contract"] != data["engine"]
        || bed["engine"] != crate::source_road_materialization::VERSION
        || bed["construction_complete"] != true
        || bed["source_terrain_preserved"] != true
        || bed["strict_fit_applied"] != false
        || bed["full_world_validated"] != false
        || bed["numerical_model_passed"] == true
        || bed["source_height_sha256"]
            .as_str()
            .is_none_or(|s| !crate::source_road_assets::valid_digest(s))
    {
        return Err("Missing or mismatched source road completion receipt".into());
    }
    Ok(())
}
#[cfg(test)]
fn publish_final_surfaces(
    accepted: &mut papyrus_terrain::road_network::SurfaceOutput,
    rebuilt: papyrus_terrain::road_network::SurfaceOutput,
    audit: &road_surface_stage::ClearanceAudit,
    expected: &[bool],
) -> Result<(), String> {
    publish_final_surfaces_for_contract(accepted, rebuilt, Some(audit), expected, false)
}
/// Compare the same fields as the geometry receipt without allocating and
/// serializing two entire meshes. Keep signed zero distinct, as JSON does.
fn same_surface_chunks(
    left: &[papyrus_terrain::road_network::SurfaceChunk],
    right: &[papyrus_terrain::road_network::SurfaceChunk],
) -> bool {
    use papyrus_terrain::{
        path_mesh::{Chunk, Template},
        road_network::SurfaceChunk,
    };
    fn coordinates_match<const N: usize>(left: &[[f32; N]], right: &[[f32; N]]) -> bool {
        left.len() == right.len()
            && left.as_flattened().iter().zip(right.as_flattened())
                .all(|(&a, &b)| a.to_bits() == b.to_bits())
    }
    left.len() == right.len() && left.iter().zip(right).all(|(a, b)| {
        let SurfaceChunk { road, lod, chunk_index, geometry } = a;
        let Chunk { first_repetition, repetitions, origin, mesh, work } = geometry;
        let Template { vertices, normals, tangents, uv, triangles, center, extents } = mesh;
        let other = &b.geometry;
        *road == b.road && *lod == b.lod && *chunk_index == b.chunk_index
            && *first_repetition == other.first_repetition
            && *repetitions == other.repetitions && *work == other.work
            && coordinates_match(std::slice::from_ref(origin), std::slice::from_ref(&other.origin))
            && coordinates_match(std::slice::from_ref(center), std::slice::from_ref(&other.mesh.center))
            && coordinates_match(std::slice::from_ref(extents), std::slice::from_ref(&other.mesh.extents))
            && *triangles == other.mesh.triangles
            && coordinates_match(vertices, &other.mesh.vertices)
            && coordinates_match(normals, &other.mesh.normals)
            && coordinates_match(tangents, &other.mesh.tangents)
            && coordinates_match(uv, &other.mesh.uv)
    })
}

fn surface_geometry_sha256(
    chunks: &[papyrus_terrain::road_network::SurfaceChunk],
) -> Result<String, String> {
    // Buffer the small writes from the JSON encoder while preserving the
    // established receipt format and avoiding a world-sized JSON buffer.
    let mut writer = std::io::BufWriter::with_capacity(64 * 1024, Sha256::new());
    serde_json::to_writer(&mut writer, chunks).map_err(|e| e.to_string())?;
    let digest = writer.into_inner().map_err(|e| e.to_string())?.finalize();
    Ok(format!("{digest:x}"))
}

fn publish_final_surfaces_for_contract(
    accepted: &mut papyrus_terrain::road_network::SurfaceOutput,
    mut rebuilt: papyrus_terrain::road_network::SurfaceOutput,
    audit: Option<&road_surface_stage::ClearanceAudit>,
    expected: &[bool],
    source_completion: bool,
) -> Result<(), String> {
    if !accepted.report["staged_pipeline"].is_object() {
        return Err("Missing staged road provenance".into());
    }
    let has_meshes = road_surface_stage::collision_coverage(expected, &rebuilt)?;
    if source_completion {
        validate_source_completion(&accepted.report["staged_pipeline"])?;
        crate::source_road_materialization::validate_geometry(expected, &rebuilt)
            .map_err(|error| error.to_string())?;
    } else {
        let audit = audit.ok_or("Missing strict road clearance audit")?;
        let grades_passed = if has_meshes {
            rebuilt.report["collision_grades_passed"] == true
        } else {
            rebuilt.report["collision_grades_passed"].is_null()
        };
        let clearance_passed = road_surface_stage::clearance_accepted(has_meshes, audit);
        if !grades_passed || !clearance_passed {
            return Err(format!(
                "Final road surfaces rejected after downstream terrain changes: grades_passed={grades_passed}, continuous_intersections={}, source_intersections={}, maximum_terrain_excess={}",
                audit.report["continuous_intersections"],
                audit.report["source_sampler_confirmed_intersections"],
                audit.report["maximum_continuous_terrain_excess"]
            ));
        }
    }
    let geometry_changed = !same_surface_chunks(&accepted.chunks, &rebuilt.chunks);
    let final_signature = surface_geometry_sha256(&rebuilt.chunks)?;
    rebuilt.report = final_surface_report(
        &accepted.report, rebuilt.report, audit, geometry_changed, final_signature,
    );
    *accepted = rebuilt;
    Ok(())
}

fn final_surface_report(
    accepted: &serde_json::Value,
    mut rebuilt: serde_json::Value,
    audit: Option<&road_surface_stage::ClearanceAudit>,
    geometry_changed: bool,
    final_signature: String,
) -> serde_json::Value {
    // The bed report describes the road earthwork pass. Final mesh audits must
    // describe the actual terrain after objects/decoration, including when that
    // terrain legitimately changes a mesh without changing sampled controls.
    rebuilt["staged_pipeline"] = accepted["staged_pipeline"].clone();
    rebuilt["refinement"] = accepted["refinement"].clone();
    rebuilt["clearance_audit"] = audit.map_or_else(
        ||serde_json::json!({"performed":false,"scope":"not a source completion rule","passed":null}),
        |audit|audit.report.clone(),
    );
    rebuilt["staged_pipeline"]["post_decoration_validated"] = true.into();
    rebuilt["staged_pipeline"]["post_decoration_geometry_updated"] = geometry_changed.into();
    rebuilt["staged_pipeline"]["final_geometry_sha256"] = final_signature.into();
    rebuilt
}

#[cfg(test)]
mod tests {
    #[test]
    fn road_surface_comparison_and_streamed_digest_preserve_receipt_semantics() {
        use super::{same_surface_chunks, surface_geometry_sha256};
        use sha2::{Digest, Sha256};
        let mut baseline = crate::source_road_materialization::steep_test_surface().chunks;
        let mut updated = crate::source_road_materialization::steep_test_surface().chunks;
        baseline[0].geometry.origin[0] = 0.;
        updated[0].geometry.origin[0] = 0.;
        assert!(same_surface_chunks(&baseline, &updated));
        assert_eq!(
            surface_geometry_sha256(&updated).unwrap(),
            format!("{:x}", Sha256::digest(serde_json::to_vec(&updated).unwrap()))
        );
        updated[0].geometry.origin[0] = -0.;
        assert!(!same_surface_chunks(&baseline, &updated));
        updated[0].geometry.origin[0] = 0.;
        updated[0].geometry.mesh.vertices[0][1] += 0.25;
        assert!(!same_surface_chunks(&baseline, &updated));
        updated.clear();
        assert!(!same_surface_chunks(&baseline, &updated));
    }

    #[test]
    fn curvature_fallback_preserves_spent_work_and_the_existing_branch_ceiling() {
        let witness = crate::road_refinement::AlignmentWitness {
            kind: crate::road_refinement::AlignmentKind::Curvature,
            road: 8,
            position: [1905., -304.],
        };
        let mut search = super::AlignmentSearch {
            attempts: vec![],
            branches: 4,
            path: vec![witness],
            markers: vec![],
            branch_limit: 4,
            origins: vec![],
            retained: vec![],
            curvature: false,
        };
        assert!(!search.start_curvature_fallback(witness, 2));
        assert!(!search.start_curvature_fallback(
            crate::road_refinement::AlignmentWitness {
                kind: crate::road_refinement::AlignmentKind::Clearance,
                ..witness
            },
            1
        ));
        assert_eq!(search.branch_limit, 4);
        assert!(search.start_curvature_fallback(witness, 1));
        assert_eq!(search.branches, 4);
        assert_eq!(search.branch_limit, super::MAX_ALIGNMENT_BRANCHES);
        assert!(search.curvature);
        assert!(!search.start_curvature_fallback(witness, 1));
        search.curvature = false;
        assert!(!search.start_curvature_fallback(witness, 1));
    }
    #[test]
    fn source_collision_witness_reaches_local_alignment_without_changing_the_error() {
        let witness = crate::road_refinement::AlignmentWitness {
            kind: crate::road_refinement::AlignmentKind::Curvature,
            road: 12,
            position: [123., -456.],
        };
        let failure = super::AttemptFailure::GeometryRejected {
            message: "Invalid source road collision geometry".into(),
            witness: Some(witness),
        };
        let recovered = super::failed_alignment(&failure).unwrap();
        assert_eq!(recovered.road, witness.road);
        assert_eq!(recovered.position, witness.position);
        assert_eq!(recovered.kind, witness.kind);
        assert_eq!(
            failure.to_string(),
            "Invalid source road collision geometry"
        );
        let legacy = super::AttemptFailure::GeometryRejected {
            message: "Legacy geometry rejection".into(),
            witness: None,
        };
        assert!(super::failed_alignment(&legacy).is_none());
    }
    #[test]
    fn new_recipe_contract_requires_ordered_inputs_and_keeps_late_rail_fallback() {
        let layout = crate::source_road_planning::Policy {
            engine: crate::source_road_planning::VERSION.into(),
            assets_sha256: "a".repeat(64),
            roles_sha256: "b".repeat(64),
            ring_clearance: None,
            terrain_entrances: None,
        };
        let bound =
            super::Policy::for_new_recipe("c".repeat(64), Some(layout.clone()), false).unwrap();
        assert_eq!(bound.engine, super::CONNECTED_SOURCE_VERSION);
        let mut expected = layout.clone();
        expected.terrain_entrances =
            Some(papyrus_terrain::source_road_route::TERRAIN_ENTRANCE_PINNED_VERSION.into());
        assert_eq!(bound.layout, Some(expected));
        let mut explicit = layout.clone();
        explicit.terrain_entrances =
            Some(papyrus_terrain::source_road_route::TERRAIN_ENTRANCE_ROUTED_VERSION.into());
        let preserved =
            super::Policy::for_new_recipe("c".repeat(64), Some(explicit.clone()), false).unwrap();
        assert_eq!(preserved.layout, Some(explicit));
        let fallback = super::Policy::for_new_recipe("c".repeat(64), None, false).unwrap();
        assert_eq!(fallback.engine, super::VERSION);
        let rails =
            super::Policy::for_new_recipe("c".repeat(64), Some(layout.clone()), true).unwrap();
        assert_eq!(rails.engine, super::VERSION);
        assert_eq!(rails.layout, Some(layout.clone()));
        assert!(super::Policy::for_new_recipe("bad".into(), Some(layout.clone()), false).is_err());
        let mut unknown = layout;
        unknown.engine = "future-ordered-roads".into();
        assert!(super::Policy::for_new_recipe("c".repeat(64), Some(unknown), false).is_err());
    }

    use super::*;
    #[test]
    fn source_ground_comparison_uses_the_exact_rejected_grid_sample() {
        use papyrus_terrain::{
            native_layers::Layers,
            road_network::{GroundBlock, socket_recovery},
        };
        let before = 16400;
        let base = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: 3,
            surface_resolution: 2,
            height: vec![before; 9],
            topology: vec![0; 4],
            splat: vec![],
            biome: vec![],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        let mut after = base.height.clone();
        after[4] = 16460;
        let failure = |p| crate::road_socket_recovery::AllRoutesFailure {
            work: 1,
            hierarchy: Some(1),
            reason: "Final source route footprint rejected",
            detail: Some(Box::new(socket_recovery::RouteFailure {
                work: 1,
                measured_work: 1,
                additional_blocked_ground: Vec::new(),
                error: socket_recovery::Failure::GroundBlocked {
                    road: 11,
                    interval: 69,
                    position: [0., 20., 0.],
                    radius: 14.,
                    obstacle: None,
                    ground: Some(GroundBlock {
                        position: p,
                        topology: 16,
                        reason: "water or low ground",
                    }),
                    reason: "native ground footprint",
                },
            })),
        };
        let p = [0., papyrus_terrain::erosion::decode(before), 0.];
        let r = source_ground_comparison(&failure(p), &base, &after).unwrap();
        assert_eq!(r["cell"], serde_json::json!([1, 1]));
        assert_eq!(r["constructed_units"], 16460);
        assert_eq!(r["before_low_ground"], true);
        assert_eq!(r["constructed_low_ground"], false);
        assert_eq!(r["accepted"], false);
        let constructed_witness = [0., papyrus_terrain::erosion::decode(after[4]), 0.];
        assert!(source_ground_comparison(&failure(constructed_witness), &base, &after).is_some());
        assert!(source_ground_comparison(&failure([1., p[1], 0.]), &base, &after).is_none());
        assert!(source_ground_comparison(&failure([0., p[1] + 1., 0.]), &base, &after).is_none());
        assert!(source_ground_comparison(&failure(p), &base, &after[..8]).is_none());
        after[4] = -1;
        assert!(source_ground_comparison(&failure(p), &base, &after).is_none());
    }
    #[test]
    fn distant_source_ground_conflicts_replan_without_moving_authored_sockets() {
        use papyrus_terrain::road_network::{
            Destination, Family, LaunchEngine, LaunchPolicy, Report, Road, socket_recovery,
        };
        let mut report = Report {
            entrance_launch: Some(LaunchPolicy {
                engine: LaunchEngine::GlobalEarthwork,
                minimum_run_widths: 1.,
                sample_spacing: 1.,
                maximum_work: 2_000_000,
            }),
            roads: vec![Road {
                from: Destination::Loop,
                to: Destination::Monument {
                    monument: 42,
                    connector: 7,
                },
                points: vec![[0., 20., 0.], [200., 20., 0.]],
            }],
            families: vec![Family {
                infrastructure_type: 0,
                hierarchy: 1,
                seed: 7,
                first_road: 0,
                road_count: 1,
                socket_count: 1,
                unresolved: vec![],
                width: 10.,
                splat_channel: 7,
            }],
            ..Default::default()
        };
        let blocked = |road, x| {
            AttemptFailure::Routes(crate::road_socket_recovery::AllRoutesFailure {
                work: 20,
                hierarchy: Some(1),
                reason: "Final source route footprint rejected",
                detail: Some(Box::new(socket_recovery::RouteFailure {
                    work: 20,
                    measured_work: 20,
                    additional_blocked_ground: Vec::new(),
                    error: socket_recovery::Failure::GroundBlocked {
                        road,
                        interval: 1,
                        position: [x, 20., 0.],
                        radius: 14.,
                        ground: None,
                        obstacle: None,
                        reason: "foreign monument obstacle",
                    },
                })),
            })
        };
        let distant = blocked(0, 100.);
        let marker = source_route_retry_marker(RAW_SOURCE_VERSION, &report, &distant)
            .unwrap()
            .unwrap();
        assert_eq!(
            source_route_retry_marker(SOURCE_COUPLED_VERSION, &report, &distant)
                .unwrap()
                .unwrap(),
            marker
        );
        assert_eq!(marker.witness.position, [100., 0.]);
        assert_eq!(marker.hierarchy, 1);
        assert_eq!(marker.radius, 20.);
        assert!(marker.terminal.is_none() && !marker.exclude);
        let mut foreign = blocked(0, 100.);
        let witness = socket_recovery::BlockedObstacle {
            monument: 13,
            center: [100., 0.],
            extent: [100., 100.],
            yaw: 99.462326,
        };
        if let AttemptFailure::Routes(route) = &mut foreign
            && let Some(detail) = route.detail.as_mut()
            && let socket_recovery::Failure::GroundBlocked { obstacle, .. } = &mut detail.error
        {
            *obstacle = Some(Box::new(witness.clone()));
        }
        let foreign_marker = source_route_retry_marker(RAW_SOURCE_VERSION, &report, &foreign)
            .unwrap()
            .unwrap();
        assert_eq!(foreign_marker.obstacle, Some(witness));
        assert!(foreign_marker.exclude);
        let mut hole = blocked(0, 100.);
        if let AttemptFailure::Routes(route) = &mut hole
            && let Some(detail) = route.detail.as_mut()
            && let socket_recovery::Failure::GroundBlocked { ground, .. } = &mut detail.error
        {
            *ground = Some(papyrus_terrain::road_network::GroundBlock {
                position: [103., 20., 11.],
                topology: 0,
                reason: "terrain hole",
            });
        }
        let hole_marker = source_route_retry_marker(RAW_SOURCE_VERSION, &report, &hole)
            .unwrap()
            .unwrap();
        assert!(hole_marker.exclude);
        assert_eq!(hole_marker.witness.position, [103., 11.]);
        assert_eq!(hole_marker.radius, 20.);
        assert!(hole_marker.terminal.is_none() && hole_marker.obstacle.is_none());
        let mut wet = blocked(0, 100.);
        if let AttemptFailure::Routes(route) = &mut wet
            && let Some(detail) = route.detail.as_mut()
            && let socket_recovery::Failure::GroundBlocked { ground, .. } = &mut detail.error
        {
            *ground = Some(papyrus_terrain::road_network::GroundBlock {
                position: [112., 0., -7.],
                topology: 134218112,
                reason: "water or low ground",
            });
        }
        let wet_marker = source_route_retry_marker(RAW_SOURCE_VERSION, &report, &wet)
            .unwrap()
            .unwrap();
        assert!(wet_marker.exclude);
        assert!(wet_marker.low_ground);
        assert_eq!(wet_marker.witness.position, [112., -7.]);
        assert_eq!(wet_marker.radius, 20.);
        assert!(wet_marker.terminal.is_none() && wet_marker.obstacle.is_none());
        // Entrance failures retain their existing bounded socket recovery.
        assert!(
            source_route_retry_marker(RAW_SOURCE_VERSION, &report, &blocked(0, 195.))
                .unwrap()
                .is_none()
        );
        for engine in [SOURCE_VERSION, VERSION, "unknown"] {
            assert!(
                source_route_retry_marker(engine, &report, &distant)
                    .unwrap()
                    .is_none()
            );
        }
        assert!(source_route_retry_marker(RAW_SOURCE_VERSION, &report, &blocked(1, 100.)).is_err());
        assert!(
            source_route_retry_marker(RAW_SOURCE_VERSION, &report, &blocked(0, f32::NAN)).is_err()
        );
        assert!(
            source_route_retry_marker(
                RAW_SOURCE_VERSION,
                &report,
                &AttemptFailure::Routes(crate::road_socket_recovery::AllRoutesFailure {
                    work: 0,
                    hierarchy: None,
                    reason: "Invalid inputs",
                    detail: None,
                })
            )
            .unwrap()
            .is_none()
        );
        report.families[0].hierarchy = 2;
        let dirt = source_route_retry_marker(RAW_SOURCE_VERSION, &report, &distant)
            .unwrap()
            .unwrap();
        assert_eq!(dirt.hierarchy, 2);
        assert!(dirt.terminal.is_none());
        report.families[0].width = 4.;
        let trail_hole = source_route_retry_marker(RAW_SOURCE_VERSION, &report, &hole)
            .unwrap()
            .unwrap();
        assert_eq!(
            trail_hole.radius, 14.,
            "Keep the actual checked footprint for narrow trails"
        );
        assert!(trail_hole.exclude && trail_hole.terminal.is_none());
        let trail_wet = source_route_retry_marker(RAW_SOURCE_VERSION, &report, &wet)
            .unwrap()
            .unwrap();
        assert!(trail_wet.exclude && trail_wet.terminal.is_none());
        assert_eq!(trail_wet.radius, 14.);
        let mut protected = blocked(0, 100.);
        if let AttemptFailure::Routes(route) = &mut protected
            && let Some(detail) = route.detail.as_mut()
            && let socket_recovery::Failure::GroundBlocked { ground, radius, .. } =
                &mut detail.error
        {
            *radius = 11.;
            *ground = Some(papyrus_terrain::road_network::GroundBlock {
                position: [108., 0.061035156, -7.],
                topology: 603980160,
                reason: "unowned protected ground",
            });
        }
        let trail_protected = source_route_retry_marker(RAW_SOURCE_VERSION, &report, &protected)
            .unwrap()
            .unwrap();
        assert!(trail_protected.exclude && trail_protected.terminal.is_none());
        assert_eq!(trail_protected.witness.position, [108., -7.]);
        assert_eq!(trail_protected.radius, 11.);
        assert_eq!(trail_protected.hierarchy, 2);
        if let AttemptFailure::Routes(route) = &mut hole
            && let Some(detail) = route.detail.as_mut()
            && let socket_recovery::Failure::GroundBlocked { radius, .. } = &mut detail.error
        {
            *radius = f32::NAN;
        }
        assert!(source_route_retry_marker(RAW_SOURCE_VERSION, &report, &hole).is_err());
    }
    #[test]
    fn raw_completion_tags_cannot_impersonate_a_different_saved_contract() {
        let valid = serde_json::json!({"engine":RAW_SOURCE_VERSION,"completion_contract":RAW_SOURCE_VERSION,
            "bed":{"engine":crate::source_road_materialization::VERSION,"construction_complete":true,
                "source_terrain_preserved":true,"strict_fit_applied":false,"full_world_validated":false,
                "numerical_model_passed":null,"source_height_sha256":"a".repeat(64)}});
        validate_source_completion(&valid).unwrap();
        let mut coupled = valid.clone();
        coupled["engine"] = SOURCE_COUPLED_VERSION.into();
        coupled["completion_contract"] = SOURCE_COUPLED_VERSION.into();
        validate_source_completion(&coupled).unwrap();
        for engine in [CONNECTED_SOURCE_VERSION, CONNECTED_COUPLED_VERSION] {
            let mut connected = valid.clone();
            connected["engine"] = engine.into();
            connected["completion_contract"] = engine.into();
            validate_source_completion(&connected).unwrap();
            assert!(is_connected_source_engine(engine));
            assert!(uses_source_cost_guidance(engine));
            assert!(!is_raw_source_engine(engine));
            let mut policy = Policy {
                engine: engine.into(),
                roadside_source_sha256: "a".repeat(64),
                roadside_preferences: Vec::new(),
                layout: Some(crate::source_road_planning::Policy {
                    engine: crate::source_road_planning::VERSION.into(),
                    assets_sha256: "b".repeat(64),
                    roles_sha256: "c".repeat(64),
                    ring_clearance: None,
                    terrain_entrances: None,
                }),
            };
            policy.validate().unwrap();
            policy.engine = "unknown-connected-engine".into();
            assert!(policy.validate().is_err());
        }
        assert!(is_raw_source_engine(SOURCE_COUPLED_VERSION));
        assert!(is_source_engine(SOURCE_COUPLED_VERSION));
        assert!(!is_raw_source_engine(COUPLED_VERSION));
        let mut wrong = valid.clone();
        wrong["completion_contract"] = SOURCE_VERSION.into();
        assert!(validate_source_completion(&wrong).is_err());
        let mut wrong = valid;
        wrong["engine"] = "unverified-source-version".into();
        assert!(validate_source_completion(&wrong).is_err());
        assert!(check_connection_contract(&Default::default(), true).is_err());
        assert!(check_connection_contract(&Default::default(), false).is_ok());
    }
    #[test]
    fn indexed_completion_rechecks_exact_parent_controls_and_dependency_order() {
        use papyrus_terrain::road_network::{Destination, Report, Road};
        let mut report = Report {
            roads: vec![
                Road {
                    from: Destination::Loop,
                    to: Destination::Loop,
                    points: vec![[0., 5., 0.], [10., 5., 0.], [20., 5., 0.]],
                },
                Road {
                    from: Destination::Monument {
                        monument: 1,
                        connector: 10,
                    },
                    to: Destination::Network { road: 0, point: 1 },
                    points: vec![[10., 5., 20.], [10., 5., 0.]],
                },
            ],
            ..Default::default()
        };
        check_connection_contract(&report, false).unwrap();
        for axis in 0..3 {
            report.roads[1].points[1][axis] += 0.01;
            assert!(check_connection_contract(&report, false).is_err());
            report.roads[1].points[1] = [10., 5., 0.];
        }
        report.roads[1].points[1][1] = f32::NAN;
        assert!(check_connection_contract(&report, false).is_err());
        report.roads[1].points[1] = [10., 5., 0.];
        for destination in [
            Destination::Network { road: 0, point: 3 },
            Destination::Network { road: 1, point: 1 },
            Destination::Network { road: 2, point: 1 },
        ] {
            report.roads[1].to = destination;
            assert!(check_connection_contract(&report, false).is_err());
        }
    }
    #[test]
    fn source_completion_refresh_preserves_comparison_failures_and_strict_contracts() {
        let receipt = serde_json::json!({"engine":SOURCE_VERSION,"completion_contract":SOURCE_VERSION,
            "bed":{"engine":crate::source_road_materialization::VERSION,
                "construction_complete":true,"source_terrain_preserved":true,"strict_fit_applied":false,
                "source_height_sha256":"b".repeat(64),"numerical_model_passed":null,"full_world_validated":false}});
        let mut accepted = crate::source_road_materialization::steep_test_surface();
        accepted.report["staged_pipeline"] = receipt.clone();
        let rebuilt = crate::source_road_materialization::steep_test_surface();
        publish_final_surfaces_for_contract(&mut accepted, rebuilt, None, &[true], true).unwrap();
        assert_eq!(accepted.report["collision_grades_passed"], false);
        assert_eq!(
            accepted.report["clearance_audit"]["passed"],
            serde_json::Value::Null
        );
        assert_eq!(accepted.report["clearance_audit"]["performed"], false);
        assert_eq!(
            accepted.report["staged_pipeline"]["post_decoration_validated"],
            true
        );
        let before = accepted.report.clone();
        assert!(
            publish_final_surfaces_for_contract(
                &mut accepted,
                crate::source_road_materialization::steep_test_surface(),
                None,
                &[true],
                false
            )
            .is_err()
        );
        assert_eq!(accepted.report, before);
        for (field, value) in [
            ("engine", serde_json::json!(VERSION)),
            ("completion_contract", serde_json::json!(VERSION)),
            ("bed", serde_json::json!({"construction_complete":true})),
        ] {
            let mut bad = receipt.clone();
            bad[field] = value;
            accepted.report["staged_pipeline"] = bad;
            let before = accepted.report.clone();
            assert!(
                publish_final_surfaces_for_contract(
                    &mut accepted,
                    crate::source_road_materialization::steep_test_surface(),
                    None,
                    &[true],
                    true
                )
                .is_err()
            );
            assert_eq!(accepted.report, before);
        }
        let mut policy = Policy {
            engine: SOURCE_VERSION.into(),
            roadside_source_sha256: "b".repeat(64),
            roadside_preferences: Vec::new(),
            layout: None,
        };
        assert!(policy.validate().is_err());
        policy.layout = Some(crate::source_road_planning::Policy {
            engine: crate::source_road_planning::VERSION.into(),
            assets_sha256: "a".repeat(64),
            roles_sha256: "b".repeat(64),
            ring_clearance: None,
            terrain_entrances: None,
        });
        policy.validate().unwrap();
        policy.engine = SOURCE_COUPLED_VERSION.into();
        policy.validate().unwrap();
        let restored: Policy =
            serde_json::from_slice(&serde_json::to_vec(&policy).unwrap()).unwrap();
        assert_eq!(restored, policy);
    }
    #[test]
    fn coupled_proposal_frames_are_versioned_without_rejecting_saved_legacy_policies() {
        for engine in [
            VERSION,
            LEGACY_COUPLED_VERSION,
            PROPOSAL_COUPLED_VERSION,
            GRID_COUPLED_VERSION,
            COUPLED_VERSION,
        ] {
            let policy = Policy {
                engine: engine.into(),
                roadside_source_sha256: "a".repeat(64),
                roadside_preferences: Vec::new(),
                layout: None,
            };
            policy.validate().unwrap();
            let restored: Policy =
                serde_json::from_slice(&serde_json::to_vec(&policy).unwrap()).unwrap();
            assert_eq!(restored, policy);
        }
        assert_ne!(COUPLED_VERSION, LEGACY_COUPLED_VERSION);
    }
    fn recovery_session() -> Session {
        use papyrus_terrain::{
            placement::{Outcome, Position},
            road_network::{Destination, Family, Report, Road, socket_recovery::Key},
        };
        let project = Project::default();
        let solution = Solution {
            outcomes: project
                .targets
                .iter()
                .map(|t| Outcome {
                    id: t.instance_id,
                    position: Some(Position {
                        x: t.x,
                        y: 10.,
                        z: t.z,
                        yaw: 20.,
                        clearance: 12.,
                    }),
                    distance: Some(0.),
                    within_radius: true,
                    reason: "Checked source placement".into(),
                })
                .collect(),
            required_missing: 0,
            attempts: 3,
        };
        let mut report = Report::default();
        report.roads.push(Road {
            from: Destination::Monument {
                monument: 1,
                connector: 7,
            },
            to: Destination::Loop,
            points: vec![[0.; 3], [20., 0., 0.]],
        });
        report.roads.push(Road {
            from: Destination::Monument {
                monument: 2,
                connector: 8,
            },
            to: Destination::Loop,
            points: vec![[100.; 3], [120., 100., 100.]],
        });
        report.families.push(Family {
            infrastructure_type: 0,
            hierarchy: 1,
            seed: 1,
            first_road: 0,
            road_count: 2,
            socket_count: 2,
            unresolved: Vec::new(),
            width: 10.,
            splat_channel: 7,
        });
        let marker = crate::road_replanning::Marker {
            witness: road_refinement::AlignmentWitness {
                kind: road_refinement::AlignmentKind::Clearance,
                road: 0,
                position: [0.; 2],
            },
            radius: 20.,
            exclude: false,
            hierarchy: 1,
            terminal: Some(Key {
                monument: 1,
                connector: 7,
            }),
            obstacle: None,
            protected_topology: 0,
            low_ground: false,
        };
        Session {
            project,
            solution,
            assets: ["a".repeat(64), "b".repeat(64)],
            worker: "worker.exe".into(),
            retained_rails: false,
            replanning: true,
            budget: RetryBudget {
                layout_planning: Some(LAYOUT_PREPARATION_WORK),
                fitting: 5_000_000_000,
                proposals: 4_000_000,
                ..Default::default()
            },
            paint_noise: Default::default(),
            preferences: vec![marker],
            failed_entrances: Vec::new(),
            attempts: vec![serde_json::json!({"layout_index":0})],
            required_connections: Some(
                crate::road_replanning::Connections::capture(&report).unwrap(),
            ),
            geometry_hints: (0..2)
                .map(|i| crate::road_alignment::Hint::capture(&report, i).unwrap())
                .collect(),
            layouts: 1,
            finished: false,
            retry_routing: true,
            last_retry: true,
            rebuild_reserved: false,
            entrance_recovery: FinalEntranceRecovery::Unavailable,
        }
    }
    fn replacement(session: &Session) -> Solution {
        let mut result = session.solution.clone();
        result.outcomes[0].position.as_mut().unwrap().x += 0.25;
        result
    }
    #[test]
    fn source_routing_reuses_the_world_only_without_a_movable_entrance() {
        for retained_rails in [false, true] {
            let mut session = recovery_session();
            session.retained_rails = retained_rails;
            let mut recipe = crate::infrastructure::Recipe::new("c".repeat(64));
            recipe.staged = Some(Policy {
                engine: SOURCE_COUPLED_VERSION.into(),
                roadside_source_sha256: "d".repeat(64),
                roadside_preferences: Vec::new(),
                layout: Some(crate::source_road_planning::Policy {
                    engine: crate::source_road_planning::VERSION.into(),
                    assets_sha256: "e".repeat(64),
                    roles_sha256: "f".repeat(64),
                    ring_clearance: None,
                    terrain_entrances: None,
                }),
            });
            session.project.roads = Some(recipe);
            let routing = Rejection::from("Ring needs another route");
            let entrance = Rejection {
                message: "Movable monument needs source assessment".into(),
                feedback: session.preferences.clone(),
                entrances: Vec::new(),
            };
            let work = serde_json::to_value(&session.budget).unwrap();
            let solution = session.solution.clone();
            assert!(session.retry_same_world(&routing));
            assert!(!session.retry_same_world(&entrance));
            assert_eq!(entrance.movable_monument(&session.project), Some(1));
            let structured = Rejection {
                message: "Unrecovered source entrance".into(),
                feedback: Vec::new(),
                entrances: vec![papyrus_terrain::road_network::socket_recovery::Key {
                    monument: 1,
                    connector: 7,
                }],
            };
            assert!(!session.retry_same_world(&structured));
            session.project.targets[0].exact = true;
            assert_eq!(entrance.movable_monument(&session.project), None);
            assert!(session.retry_same_world(&entrance));
            session.retry_routing = false;
            assert!(!session.retry_same_world(&routing));
            session.retry_routing = true;
            session.layouts = MAX_LAYOUTS;
            assert!(!session.retry_same_world(&routing));
            session.layouts = 1;
            session.finished = true;
            assert!(!session.retry_same_world(&routing));
            session.finished = false;
            for engine in [RAW_SOURCE_VERSION, SOURCE_VERSION, COUPLED_VERSION] {
                session
                    .project
                    .roads
                    .as_mut()
                    .unwrap()
                    .staged
                    .as_mut()
                    .unwrap()
                    .engine = engine.into();
                assert!(!session.retry_same_world(&routing));
            }
            assert_eq!(session.solution, solution);
            assert_eq!(serde_json::to_value(&session.budget).unwrap(), work);
            assert_eq!(session.retained_rails, retained_rails);
            assert!(!session.rebuild_reserved);
        }
    }
    #[test]
    fn source_entrance_failures_preserve_identity_and_allow_bounded_placement_recovery() {
        use papyrus_terrain::road_network::socket_recovery::Key;
        let key = Key {
            monument: 1,
            connector: 7,
        };
        let error = LayoutFailure::from(crate::road_planning::Failure {
            message: "Unrecovered dirt-road entrance".into(),
            entrances: vec![key],
            rejected_retained_layout: false,
        });
        let LayoutFailure::Entrances(error) = error else {
            panic!("Source entrance identity became a fatal message");
        };
        assert_eq!(error.entrances, vec![key]);
        assert!(matches!(
            LayoutFailure::from(crate::road_planning::Failure::from("Source data mismatch")),
            LayoutFailure::Fatal(_)
        ));
        let mut session = recovery_session();
        session.preferences.clear();
        session.failed_entrances = vec![
            key,
            Key {
                monument: 2,
                connector: 8,
            },
        ];
        let moved = replacement(&session);
        assert!(session.placement_changed(1, &moved).is_err());
        assert!(session.reserve_world_rebuild(500_000_000).unwrap());
        let total = session.budget.total();
        let connections = serde_json::to_value(&session.required_connections).unwrap();
        session.placement_changed(1, &moved).unwrap();
        assert_eq!(session.solution, moved);
        assert_eq!(
            session.failed_entrances,
            vec![Key {
                monument: 2,
                connector: 8
            }]
        );
        assert_eq!(session.budget.total(), total);
        assert_eq!(
            serde_json::to_value(&session.required_connections).unwrap(),
            connections
        );
        assert!(!session.rebuild_reserved);
    }
    #[test]
    fn last_coupled_entrance_failure_unlocks_one_layout_only_after_a_replacement() {
        use papyrus_terrain::road_network::socket_recovery::Key;
        let mut session = recovery_session();
        let mut roads = crate::infrastructure::Recipe::new("c".repeat(64));
        roads.staged = Some(Policy {
            engine: CONNECTED_COUPLED_VERSION.into(),
            roadside_source_sha256: "d".repeat(64),
            roadside_preferences: Vec::new(),
            layout: None,
        });
        session.project.roads = Some(roads);
        session.layouts = MAX_LAYOUTS;
        session.budget.layout_planning = Some(LAYOUT_PREPARATION_WORK * MAX_LAYOUTS);
        session.failed_entrances = vec![Key { monument: 1, connector: 7 }];
        session.project.targets[0].exact = true;
        session.allow_final_entrance_recovery();
        assert!(!session.can_retry());

        session.project.targets[0].exact = false;
        session.allow_final_entrance_recovery();
        assert!(session.can_retry());
        assert!(session.entrance_recovery == FinalEntranceRecovery::PendingPlacement);
        let updated = replacement(&session);
        assert!(session.placement_changed(1, &updated).is_err());
        assert!(session.reserve_world_rebuild(500_000_000).unwrap());
        session.placement_changed(1, &updated).unwrap();
        assert!(session.entrance_recovery == FinalEntranceRecovery::Ready);
        assert!(!session.budget.reserve_layout_preparation(false).unwrap());
        assert!(session.budget.reserve_layout_preparation(true).unwrap());
        assert!(!session.budget.reserve_layout_preparation(true).unwrap());

        session.layouts += 1;
        session.entrance_recovery = FinalEntranceRecovery::Unavailable;
        session.failed_entrances = vec![Key { monument: 1, connector: 7 }];
        session.allow_final_entrance_recovery();
        assert!(!session.can_retry());
    }
    #[test]
    fn placement_retry_keeps_shared_work_and_connections_but_discards_old_pose_geometry() {
        let mut session = recovery_session();
        let mut second_entrance = session.preferences[0].clone();
        second_entrance.terminal.as_mut().unwrap().connector = 8;
        session.preferences.push(second_entrance);
        let mut grade = session.preferences[0].clone();
        grade.witness.kind = road_refinement::AlignmentKind::Grade;
        grade.terminal = None;
        session.preferences.push(grade);
        let connections = serde_json::to_value(&session.required_connections).unwrap();
        assert!(session.reserve_world_rebuild(500_000_000).unwrap());
        let total = session.budget.total();
        let moved = replacement(&session);
        session.placement_changed(1, &moved).unwrap();
        assert_eq!(session.solution, moved);
        assert_eq!(session.preferences.len(), 1);
        assert!(session.preferences[0].terminal.is_none());
        assert_eq!(session.geometry_hints.len(), 1);
        assert!(session.geometry_hints[0].references_monument(2));
        assert!(!session.geometry_hints[0].references_monument(1));
        assert_eq!(
            serde_json::to_value(&session.required_connections).unwrap(),
            connections
        );
        assert_eq!(session.budget.total(), total);
        assert_eq!(session.budget.proposals, 4_000_000);
        assert_eq!(
            session.budget.layout_planning,
            Some(LAYOUT_PREPARATION_WORK)
        );
        assert_eq!(session.layouts, 1);
        assert!(!session.rebuild_reserved);
        assert!(session.budget.reserve_layout_preparation(false).unwrap());
        assert_eq!(
            session.budget.total(),
            total + LAYOUT_PREPARATION_WORK as u64
        );
    }
    #[test]
    fn placement_retry_rejects_pins_missing_reservations_and_unrelated_or_invalid_changes_atomically()
     {
        for case in 0..8 {
            let mut session = recovery_session();
            if case != 0 {
                assert!(session.reserve_world_rebuild(100).unwrap());
            }
            let mut moved = replacement(&session);
            match case {
                0 => {}
                1 => session.project.targets[0].exact = true,
                2 => {
                    session.project.targets[0].yaw = Some(20.);
                    moved.outcomes[0].position.as_mut().unwrap().yaw = 21.;
                }
                3 => moved.outcomes[1].position.as_mut().unwrap().z += 1.,
                4 => moved.outcomes[0].position.as_mut().unwrap().x = f32::NAN,
                5 => moved.required_missing = 1,
                6 => {
                    moved.outcomes.pop();
                }
                7 => session.preferences.clear(),
                _ => unreachable!(),
            }
            let previous = layout_fingerprint_for_test(&session.solution);
            let preferences = serde_json::to_value(&session.preferences).unwrap();
            let work = session.budget.total();
            assert!(session.placement_changed(1, &moved).is_err(), "case {case}");
            assert_eq!(layout_fingerprint_for_test(&session.solution), previous);
            assert_eq!(
                serde_json::to_value(&session.preferences).unwrap(),
                preferences
            );
            assert_eq!(session.budget.total(), work);
            assert_eq!(session.geometry_hints.len(), 2);
        }
    }
    fn layout_fingerprint_for_test(solution: &Solution) -> [u8; 32] {
        crate::composition::layout_fingerprint(solution)
    }
    #[test]
    fn session_identity_rejects_silent_solution_recipe_assets_worker_and_rail_changes() {
        let session = recovery_session();
        let matches =
            |project: &Project, solution: &Solution, assets: [&str; 2], worker: &Path, rails| {
                session.matches_context(project, solution, assets, worker, rails)
            };
        let assets = [session.assets[0].as_str(), session.assets[1].as_str()];
        assert!(matches(
            &session.project,
            &session.solution,
            assets,
            &session.worker,
            false
        ));
        assert!(!matches(
            &session.project,
            &replacement(&session),
            assets,
            &session.worker,
            false
        ));
        let mut changed = session.project.clone();
        changed.seeds[3].value ^= 1;
        assert!(!matches(
            &changed,
            &session.solution,
            assets,
            &session.worker,
            false
        ));
        assert!(!matches(
            &session.project,
            &session.solution,
            ["other", assets[1]],
            &session.worker,
            false
        ));
        assert!(!matches(
            &session.project,
            &session.solution,
            [assets[0], "other"],
            &session.worker,
            false
        ));
        assert!(!matches(
            &session.project,
            &session.solution,
            assets,
            Path::new("other.exe"),
            false
        ));
        assert!(!matches(
            &session.project,
            &session.solution,
            assets,
            &session.worker,
            true
        ));
    }
    #[test]
    fn rebuild_reservations_share_the_existing_ceiling_and_cannot_reopen_a_finished_search() {
        let mut session = recovery_session();
        let remaining = session.budget.fitting_remaining();
        assert!(session.reserve_world_rebuild(remaining - 1).unwrap());
        assert_eq!(session.budget.fitting_remaining(), 1);
        let total = session.budget.total();
        assert!(!session.reserve_world_rebuild(2).unwrap());
        assert_eq!(session.budget.total(), total);
        assert!(session.reserve_world_rebuild(0).is_err());
        assert!(session.reserve_world_rebuild(1).unwrap());
        assert_eq!(session.budget.fitting_remaining(), 0);
        assert!(!session.budget.reserve_layout_preparation(false).unwrap());
        session.finished = true;
        assert!(!session.can_retry());
        assert!(session.reserve_world_rebuild(1).is_err());
        session.finished = false;
        session.layouts = MAX_LAYOUTS;
        assert!(!session.can_retry());
        assert!(session.reserve_world_rebuild(1).is_err());
        let mut legacy = RetryBudget {
            shared_generation: false,
            ..Default::default()
        };
        assert!(legacy.reserve_world_rebuild(1).is_err());
    }
    #[test]
    fn layout_preparation_and_failed_fits_share_one_generation_budget() {
        let mut budget = RetryBudget {
            layout_planning: Some(0),
            ..Default::default()
        };
        assert!(budget.reserve_layout_preparation(false).unwrap());
        assert!(budget.reserve_layout_preparation(false).unwrap());
        budget.charge_fitting(25_999_999_900);
        assert!(budget.reserve_layout_preparation(false).unwrap());
        assert_eq!(budget.fitting_remaining(), 100);
        budget.charge_selection(100).unwrap();
        assert_eq!(budget.fitting_remaining(), 0);
        assert!(!budget.reserve_layout_preparation(false).unwrap());
        assert_eq!(budget.layout_planning, Some(6_000_000_000));
        assert!(RetryBudget::default().reserve_layout_preparation(false).is_err());
    }
    #[test]
    fn envelope_prefix_progress_crosses_witnesses_but_rejects_regression_or_changed_graphs() {
        use papyrus_terrain::{
            rail_grade_fit::EnvelopePrefix,
            road_grade::{ConflictKind, ControlConflict},
        };
        let prefix = EnvelopePrefix {
            first_node: 20,
            required: 60.,
            available: 59.,
            nodes: 100,
            edges: 101,
        };
        let initial = ControlConflict {
            kind: ConflictKind::IntervalEnvelope,
            road: 26,
            point: 11,
            position: [804., 60., -34.],
            required: 60.,
            available: 59.,
            envelope: Some(prefix),
        };
        let later = ControlConflict {
            point: 12,
            required: 58.,
            available: 57.94,
            envelope: Some(EnvelopePrefix {
                first_node: 21,
                required: 58.,
                available: 57.94,
                ..prefix
            }),
            ..initial
        };
        assert!(improving_control(Some(initial), Some(later)));
        assert!(!improving_control(Some(later), Some(initial)));
        let same_prefix = ControlConflict {
            envelope: Some(EnvelopePrefix {
                required: 59.5,
                ..prefix
            }),
            ..later
        };
        assert!(improving_control(Some(initial), Some(same_prefix)));
        for envelope in [
            None,
            Some(EnvelopePrefix {
                nodes: 101,
                ..prefix
            }),
            Some(EnvelopePrefix {
                edges: 102,
                ..prefix
            }),
            Some(EnvelopePrefix {
                first_node: 19,
                ..prefix
            }),
            Some(EnvelopePrefix {
                first_node: 100,
                ..prefix
            }),
            Some(EnvelopePrefix {
                required: f64::NAN,
                ..prefix
            }),
            Some(EnvelopePrefix {
                available: 58.,
                ..prefix
            }),
            Some(prefix),
        ] {
            let candidate = ControlConflict { envelope, ..later };
            assert!(
                !improving_control(Some(initial), Some(candidate)),
                "{candidate:?}"
            );
        }
        // Local improvement cannot hide a worse earlier envelope.
        let candidate = ControlConflict {
            required: 59.2,
            available: 59.,
            envelope: Some(EnvelopePrefix {
                required: 61.,
                ..prefix
            }),
            ..initial
        };
        assert!(!improving_control(Some(initial), Some(candidate)));
    }
    #[test]
    fn indexed_grade_refinement_requires_materially_decreasing_typed_deficits() {
        use papyrus_terrain::road_grade::{ConflictKind, ControlConflict};
        let initial = ControlConflict {
            kind: ConflictKind::IntervalEnvelope,
            road: 26,
            point: 11,
            position: [804.0491, 60.512817, -34.773422],
            required: 60.5128173828125,
            available: 59.57211696289579,
            envelope: None,
        };
        let better = ControlConflict {
            required: 60.13336181640625,
            available: 59.52772808567962,
            ..initial
        };
        let closer = ControlConflict {
            required: 59.75433349609375,
            available: 59.5354107631209,
            ..initial
        };
        assert!(improving_control(Some(initial), Some(better)));
        assert!(improving_control(Some(better), Some(closer)));
        for candidate in [
            initial,
            ControlConflict {
                required: initial.required + 0.01,
                ..initial
            },
            ControlConflict {
                available: initial.available + 1e-8,
                ..initial
            },
            ControlConflict {
                point: 12,
                ..better
            },
            ControlConflict { road: 25, ..better },
            ControlConflict {
                kind: ConflictKind::FixedEarthwork,
                ..better
            },
            ControlConflict {
                available: f64::NAN,
                ..better
            },
            ControlConflict {
                position: [f32::INFINITY, 0., 0.],
                ..better
            },
            ControlConflict {
                available: better.required + 1.,
                ..better
            },
        ] {
            assert!(
                !improving_control(Some(initial), Some(candidate)),
                "{candidate:?}"
            );
        }
        assert!(!improving_control(None, Some(better)));
        assert!(!improving_control(Some(initial), None));
        assert!(!improving_control(Some(closer), Some(better)));
    }
    #[test]
    fn alignment_history_allows_a_new_constraint_but_rejects_a_grade_clearance_grade_cycle() {
        use crate::road_refinement::{AlignmentKind, AlignmentWitness};
        let report = crate::road_alignment::tests::fixture();
        let grade = AlignmentWitness {
            kind: AlignmentKind::Grade,
            road: 0,
            position: [1143.106, 1314.572],
        };
        let clearance = AlignmentWitness {
            kind: AlignmentKind::Clearance,
            road: 0,
            position: [1136.935, 1319.580],
        };
        assert!(fresh_alignment(&report, &[grade], clearance));
        let recurrent = AlignmentWitness {
            position: [1131.079, 1318.305],
            ..grade
        };
        assert!(!fresh_alignment(&report, &[grade, clearance], recurrent));
        assert!(fresh_alignment(
            &report,
            &[grade, clearance],
            AlignmentWitness {
                position: [1300., 1500.],
                ..recurrent
            }
        ));
    }
    #[test]
    fn alignment_search_distinguishes_new_clearance_sites_without_repeating_one_patch() {
        use crate::road_refinement::AlignmentWitness;
        let mut report = crate::road_alignment::tests::fixture();
        report.roads.push(papyrus_terrain::road_network::Road {
            from: papyrus_terrain::road_network::Destination::Loop,
            to: papyrus_terrain::road_network::Destination::Loop,
            points: vec![[0.; 3], [10., 0., 0.]],
        });
        let current = AlignmentWitness {
            kind: crate::road_refinement::AlignmentKind::Grade,
            road: 0,
            position: [1339.3877, -518.9433],
        };
        let nearby = AlignmentWitness {
            kind: crate::road_refinement::AlignmentKind::Grade,
            road: 0,
            position: [1344.7519, -513.9837],
        };
        let clearance = AlignmentWitness {
            kind: crate::road_refinement::AlignmentKind::Grade,
            road: 0,
            position: [256.34765625, 1214.336],
        };
        assert!(!distinct_alignment(&report, current, nearby));
        assert!(distinct_alignment(&report, current, clearance));
        assert!(distinct_alignment(
            &report,
            current,
            AlignmentWitness {
                kind: crate::road_refinement::AlignmentKind::Grade,
                road: 1,
                ..nearby
            }
        ));
        assert!(!distinct_alignment(
            &report,
            current,
            AlignmentWitness {
                kind: crate::road_refinement::AlignmentKind::Grade,
                road: 2,
                ..nearby
            }
        ));
        assert!(!distinct_alignment(
            &report,
            current,
            AlignmentWitness {
                kind: crate::road_refinement::AlignmentKind::Grade,
                position: [f64::NAN, 0.],
                ..nearby
            }
        ));
        report.families[0].width = f32::NAN;
        assert!(!distinct_alignment(&report, current, clearance));
    }
    #[test]
    fn dirt_corridor_guard_uses_bound_source_fade_and_rejects_bad_provenance() {
        use papyrus_terrain::{
            height_reservations::HeightReservations,
            path_terrain::Grid,
            road_network::{Destination, Family, Report, Road, SurfaceOutput},
        };
        let paint: road_paint_source::Profile = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-road-paint.json"
        ))
        .unwrap();
        let terrain = road_terrain_source::Profile::for_paint(&paint)
            .unwrap()
            .unwrap();
        let height = vec![16383; 257 * 257];
        let grid = Grid {
            size: 1000.,
            resolution: 257,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &height,
        };
        let mut report = Report {
            roads: vec![Road {
                from: Destination::Network { road: 0, point: 0 },
                to: Destination::Network { road: 0, point: 1 },
                points: vec![[-200., 0., 0.], [200., 0., 0.]],
            }],
            families: vec![Family {
                hierarchy: 2,
                width: 4.,
                splat_channel: 0,
                first_road: 0,
                road_count: 1,
                infrastructure_type: 0,
                seed: 1,
                socket_count: 0,
                unresolved: vec![],
            }],
            surfaces: Some(SurfaceOutput {
                chunks: vec![],
                report: serde_json::json!({"staged_pipeline":{"paint":{"paint_profile_sha256":paint.sha256().unwrap(),"terrain_profile_sha256":terrain.sha256().unwrap()}}}),
            }),
            ..Default::default()
        };
        let make = || HeightReservations::new(grid, || false).unwrap().unwrap();
        let mut controls_only = make();
        for p in &report.roads[0].points {
            controls_only.reserve_query([p[0], p[2]]).unwrap();
        }
        let mut mask = make();
        assert!(
            reserve_dirt_corridors(&mut mask, &report, 1_000_000, || false)
                .unwrap()
                .unwrap()
                > 0
        );
        // The dirt paint is narrower than this point, but source terrain fade
        // still reaches it. It must survive later height-writing decoration.
        for p in [[0., 0.], [0., 10.], [-200., 10.], [200., 10.]] {
            let mut probe = make();
            probe.reserve_query(p).unwrap();
            for i in 0..height.len() {
                if probe.contains(i) {
                    assert!(mask.contains(i));
                    assert!(!controls_only.contains(i));
                }
            }
        }
        let mut far = make();
        far.reserve_query([0., 80.]).unwrap();
        assert!(
            (0..height.len())
                .filter(|i| far.contains(*i))
                .all(|i| !mask.contains(i))
        );
        assert!(reserve_dirt_corridors(&mut make(), &report, 1, || false).is_err());
        assert!(
            reserve_dirt_corridors(&mut make(), &report, 1_000_000, || true)
                .unwrap()
                .is_none()
        );
        report.families[0].width = 3.;
        assert!(reserve_dirt_corridors(&mut make(), &report, 1_000_000, || false).is_err());
        report.families[0].width = 4.;
        report.surfaces.as_mut().unwrap().report["staged_pipeline"]["paint"]["terrain_profile_sha256"] =
            "0".repeat(64).into();
        assert!(reserve_dirt_corridors(&mut make(), &report, 1_000_000, || false).is_err());
    }
    #[test]
    fn socket_retries_require_a_nearby_paved_clearance_failure_and_launch_revision_eight() {
        use papyrus_terrain::road_network::{
            Destination, Family, LaunchEngine, LaunchPolicy, Report, Road,
        };
        let mut report = Report {
            entrance_launch: Some(LaunchPolicy {
                engine: LaunchEngine::GlobalEarthwork,
                minimum_run_widths: 1.,
                sample_spacing: 1.,
                maximum_work: 2_000_000,
            }),
            roads: vec![Road {
                from: Destination::Network { road: 0, point: 0 },
                to: Destination::Monument {
                    monument: 42,
                    connector: 7,
                },
                points: vec![[100., 20., 0.], [0., 20., 0.]],
            }],
            families: vec![Family {
                infrastructure_type: 0,
                hierarchy: 1,
                seed: 1,
                first_road: 0,
                road_count: 1,
                socket_count: 1,
                unresolved: vec![],
                width: 10.,
                splat_channel: 7,
            }],
            ..Default::default()
        };
        let failure = |position| {
            AttemptFailure::Fitting(road_refinement::Failure::FittingRound {
                ranking_revision: None,
                round: 0,
                work_before_fit: 12,
                fit_work_limit: 100,
                progress: None,
                witness: None,
                error: Box::new(road_bed::Failure::ClearanceInfeasible {
                    work: 13,
                    road: 0,
                    chunk: 0,
                    triangle: 0,
                    position,
                    topology: 0,
                    free_variables: 0,
                    minimum: 0.03,
                    limit: 0.02,
                }),
            })
        };
        let key = failed_socket(&report, &failure([1., 0.])).unwrap();
        assert_eq!((key.monument, key.connector), (42, 7));
        let route_failure = |x| {
            AttemptFailure::Routes(crate::road_socket_recovery::AllRoutesFailure {
                work: 25,
                hierarchy: Some(1),
                reason: "Final source route footprint rejected",
                detail: Some(Box::new(
                    papyrus_terrain::road_network::socket_recovery::RouteFailure {
                        work: 20,
                        measured_work: 20,
                        additional_blocked_ground: Vec::new(),
                        error: papyrus_terrain::road_network::socket_recovery::Failure::CostBlocked {
                            road: 0,
                            interval: 0,
                            position: [x, 900., 0.],
                            margin: 1.,
                            owner: key,
                        },
                    },
                )),
            })
        };
        assert_eq!(failed_socket(&report, &route_failure(1.)), Some(key));
        assert!(failed_socket(&report, &route_failure(21.)).is_none());
        assert!(failed_socket(&report, &route_failure(f32::NAN)).is_none());
        for (x, expected) in [(1., Some(key)), (21., None), (f32::NAN, None)] {
            let mut failure = route_failure(x);
            let AttemptFailure::Routes(route) = &mut failure else {
                unreachable!()
            };
            route.detail.as_mut().unwrap().error =
                papyrus_terrain::road_network::socket_recovery::Failure::StripFolded {
                    road: 0,
                    interval: 0,
                    position: [x, 900., 0.],
                };
            assert_eq!(failed_socket(&report, &failure), expected);
            let witness = failed_alignment(&failure);
            if x.is_finite() {
                let witness = witness.unwrap();
                assert_eq!(witness.road, 0);
                assert_eq!(witness.kind, road_refinement::AlignmentKind::Curvature);
                assert_eq!(witness.position, [f64::from(x), 0.]);
            } else {
                assert!(witness.is_none());
            }
        }
        assert!(failed_socket(&report, &failure([21., 0.])).is_none());
        assert!(failed_socket(&report, &failure([f64::NAN, 0.])).is_none());
        assert!(
            failed_socket(
                &report,
                &AttemptFailure::Fitting(road_refinement::Failure::Budget)
            )
            .is_none()
        );
        report.families[0].hierarchy = 2;
        assert!(failed_socket(&report, &failure([1., 0.])).is_none());
        report.families[0].hierarchy = 1;
        report.entrance_launch.as_mut().unwrap().engine = LaunchEngine::SurfaceChecked;
        assert!(failed_socket(&report, &failure([1., 0.])).is_none());
    }
    #[test]
    fn proposal_meter_reuses_only_verified_unspent_work_and_keeps_both_caps() {
        let mut budget = RetryBudget::default();
        let grant = budget.proposal_allowance(2_000_000);
        budget.charge_proposal(37_000, grant).unwrap();
        assert_eq!(budget.proposals, 37_000);
        assert_eq!(budget.proposal_allowance(2_000_000), 2_000_000);
        assert!(budget.charge_proposal(grant + 1, grant).is_err());
        assert!(budget.charge_proposal(1, grant + 1).is_err());
        assert_eq!(budget.proposals, 37_000);
        budget.proposals = 11_999_900;
        assert_eq!(budget.proposal_allowance(2_000_000), 100);
        budget.charge_proposal(100, 100).unwrap();
        assert_eq!(budget.proposal_allowance(2_000_000), 0);
        let mut budget = RetryBudget {
            fitting: 31_999_999_960,
            ..Default::default()
        };
        assert_eq!(budget.proposal_allowance(2_000_000), 40);
        budget.charge_proposal(40, 40).unwrap();
        assert_eq!(budget.fitting_remaining(), 0);
        assert!(budget.charge_proposal(1, 1).is_err());
    }
    #[test]
    fn combined_retry_grants_account_for_the_fourth_paint_primitive() {
        let mut combined = RetryBudget {
            fitting: 31_999_999_880,
            rail_topology: Some(0),
            ..Default::default()
        };
        let legacy = RetryBudget {
            fitting: combined.fitting,
            ..Default::default()
        };
        assert_eq!(combined.paint_remaining(), 30);
        assert_eq!(combined.final_surfaces_remaining(), 40);
        assert_eq!(legacy.paint_remaining(), 40);
        assert_eq!(legacy.final_surfaces_remaining(), 60);
        assert!(
            serde_json::to_value(&legacy)
                .unwrap()
                .get("rail_topology")
                .is_none()
        );
        combined.rail_topology = Some(120);
        assert_eq!(combined.paint_remaining(), 0);
        assert_eq!(combined.final_surfaces_remaining(), 0);
    }
    #[test]
    fn repeated_attempts_debit_one_shared_budget_and_reject_stage_overruns() {
        let mut budget = RetryBudget::default();
        budget.charge_selection(80_000_000).unwrap();
        budget.charge_selection(40_000_000).unwrap();
        assert_eq!(budget.selection_remaining(), 128_000_000);
        assert!(budget.charge_selection(128_000_001).is_err());
        assert_eq!(budget.total(), 120_000_000);
        budget.charge_straightening(600_000).unwrap();
        budget.charge_straightening(400_000).unwrap();
        assert_eq!(budget.straightening_remaining(), 1_000_000);
        assert!(budget.charge_straightening(1_000_001).is_err());
        budget.charge_fitting(53_225_082);
        budget.charge_fitting(23_980_383_691);
        assert_eq!(budget.fitting_remaining(), 7_845_391_227);
        budget.charge_fitting(budget.fitting_remaining());
        assert_eq!(budget.fitting_remaining(), 0);
        assert_eq!(budget.selection_remaining(), 0);
        assert!(budget.charge_selection(1).is_err());
        assert_eq!(budget.paint_remaining(), 0);
        budget.charge_fitting(u64::MAX);
        assert_eq!(budget.fitting_remaining(), 0);
        // Existing launch revisions retain their original per-stage grants.
        let legacy = RetryBudget {
            shared_generation: false,
            selection: 120_000_000,
            ..Default::default()
        };
        assert_eq!(legacy.fitting_remaining(), 32_000_000_000);
        assert_eq!(legacy.selection_remaining(), 128_000_000);
    }
    #[test]
    fn final_mesh_refresh_uses_real_grade_and_clearance_checks_and_rejects_atomically() {
        use papyrus_terrain::{
            path_mesh::{Chunk, Template},
            path_terrain::Grid,
            road_network::{SurfaceChunk, SurfaceOutput},
        };
        let template = Template {
            vertices: vec![[-500., 1., -500.], [500., 1., -500.], [0., 1., 500.]],
            normals: vec![[0., 1., 0.]; 3],
            tangents: vec![[1., 0., 0., 1.]; 3],
            uv: vec![[0.; 2]; 3],
            triangles: vec![[0, 1, 2]],
            center: [0., 1., 0.],
            extents: [500., 0., 500.],
        };
        let height = vec![0; 9];
        let grid = Grid {
            size: 1000.,
            resolution: 3,
            origin_y: 0.,
            vertical_span: 1000.,
            height: &height,
        };
        let make = |levels: [f32; 3]| {
            let mut mesh = template.clone();
            for (p, y) in mesh.vertices.iter_mut().zip(levels) {
                p[1] = y;
            }
            let lo = levels.into_iter().fold(f32::INFINITY, f32::min);
            let hi = levels.into_iter().fold(f32::NEG_INFINITY, f32::max);
            mesh.center[1] = (lo + hi) * 0.5;
            mesh.extents[1] = (hi - lo) * 0.5;
            let chunk = Chunk {
                first_repetition: 0,
                repetitions: 1,
                origin: [0.; 3],
                mesh,
                work: 0,
            };
            let grade = papyrus_terrain::road_mesh_audit::audit(
                &chunk,
                &template,
                papyrus_terrain::road_mesh_audit::Settings {
                    maximum_grade: 0.2,
                    minimum_up_normal: 0.5,
                    maximum_work: 10000,
                },
                || false,
            )
            .unwrap();
            let mut surfaces = SurfaceOutput {
                chunks: vec![SurfaceChunk {
                    road: 0,
                    lod: 0,
                    chunk_index: 0,
                    geometry: chunk,
                }],
                report: serde_json::json!({"collision_grades_passed":grade.passed,"collision_grade_audits":[grade],"files":["fresh mesh"],"staged_pipeline":{"bed":{"numerical_model_passed":true}},"refinement":{"engine":"original bed"}}),
            };
            let audit = road_surface_stage::audit_clearance(
                grid,
                &surfaces,
                &template,
                road_clearance::Settings {
                    tolerance: 0.02,
                    minimum_up_normal: 0.5,
                    maximum_work: 10000,
                    maximum_probes: 1000,
                },
                || false,
            )
            .unwrap()
            .unwrap();
            surfaces.report["clearance_audit"] = audit.report.clone();
            (surfaces, audit)
        };
        let (mut accepted, _) = make([0.5; 3]);
        accepted.report["files"] = serde_json::json!(["obsolete mesh"]);
        let (rebuilt, audit) = make([1.; 3]);
        publish_final_surfaces(&mut accepted, rebuilt, &audit, &[true]).unwrap();
        assert_eq!(accepted.chunks[0].geometry.mesh.vertices[0][1], 1.);
        assert_eq!(accepted.report["files"], serde_json::json!(["fresh mesh"]));
        assert_eq!(accepted.report["refinement"]["engine"], "original bed");
        assert_eq!(
            accepted.report["staged_pipeline"]["post_decoration_geometry_updated"],
            true
        );
        assert_eq!(
            accepted.report["staged_pipeline"]["post_decoration_validated"],
            true
        );
        assert_eq!(accepted.report["clearance_audit"]["passed"], true);
        let before = serde_json::to_vec(&accepted.chunks).unwrap();
        let before_report = accepted.report.clone();
        for levels in [[-1.; 3], [0., 600., 0.]] {
            let (rebuilt, audit) = make(levels);
            assert!(publish_final_surfaces(&mut accepted, rebuilt, &audit, &[true]).is_err());
            assert_eq!(serde_json::to_vec(&accepted.chunks).unwrap(), before);
            assert_eq!(accepted.report, before_report);
        }
        let (rebuilt, audit) = make([1.; 3]);
        assert!(publish_final_surfaces(&mut accepted, rebuilt, &audit, &[true, true]).is_err());
        assert_eq!(accepted.report, before_report);
    }
    #[test]
    fn staged_policy_is_saved_source_bound_and_does_not_change_legacy_recipe_encoding() {
        let mut recipe = infrastructure::Recipe::new("a".repeat(64));
        assert!(
            serde_json::to_value(&recipe)
                .unwrap()
                .get("staged")
                .is_none()
        );
        let policy = Policy {
            layout: None,
            engine: VERSION.into(),
            roadside_source_sha256: "b".repeat(64),
            roadside_preferences: Vec::new(),
        };
        recipe.staged = Some(policy.clone());
        assert!(recipe.validate().is_err());
        recipe.engine = papyrus_terrain::road_network::COST_VERSION.into();
        recipe.surface_sha256 = Some("c".repeat(64));
        recipe.validate().unwrap();
        let encoded = serde_json::to_value(&recipe).unwrap();
        assert!(encoded["staged"].get("layout").is_none());
        assert!(encoded["staged"].get("roadside_preferences").is_none());
        let mut ordered = recipe.clone();
        ordered.staged.as_mut().unwrap().layout = Some(crate::source_road_planning::Policy {
            engine: crate::source_road_planning::VERSION.into(),
            assets_sha256: "d".repeat(64),
            roles_sha256: "e".repeat(64),
            ring_clearance: None,
            terrain_entrances: None,
        });
        assert!(ordered.validate().is_err()); // No bound access lattice.
        let restored: infrastructure::Recipe = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(restored, recipe);
        let mut bad = encoded;
        bad["staged"]["engine"] = "unknown".into();
        assert!(
            serde_json::from_value::<infrastructure::Recipe>(bad)
                .unwrap()
                .validate()
                .is_err()
        );
        let mut bad = policy;
        bad.roadside_source_sha256 = "b".repeat(63);
        assert!(bad.validate().is_err());
    }
}
