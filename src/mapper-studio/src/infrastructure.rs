//! Source-bound infrastructure metadata and the shared GUI/CLI road stage.
use crate::{Project, installation::SourceFile, prefab_assets::Catalog};
use papyrus_terrain::{monument_composition::Composed, placement::Solution, road_network};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    io::Read,
    path::Path,
    rc::{Rc, Weak},
    sync::Arc,
};
const LIMIT: u64 = 16 * 1024 * 1024;
/// Stable independent trail seed; shared by legacy and ordered source layouts.
pub(crate) fn trail_seed(road_seed: u32, domain: &str) -> u32 {
    let mut seed = Sha256::new();
    seed.update(b"papyrus-road-family-seed-1\0");
    seed.update(road_seed.to_le_bytes());
    seed.update(domain.as_bytes());
    u32::from_le_bytes(seed.finalize()[..4].try_into().unwrap())
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staged: Option<crate::road_generation::Policy>,
    pub engine: String,
    pub source_sha256: String,
    pub maximum_grade: f32,
    pub maximum_cut_fill: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_ring: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_trails: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ring_search: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ring_costmap: Option<crate::road_cost_source::Profile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_costmap: Option<crate::road_cost_source::Profile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_smoothing: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_river_water: Option<papyrus_terrain::road_water::Policy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_search: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrance_launch: Option<road_network::LaunchPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_topology: Option<String>,
}
impl Recipe {
    pub(crate) fn result_receipt(&self, seed: u32) -> Result<road_network::RecipeReceipt, String> {
        let mut recipe = serde_json::to_value(self).map_err(|e| e.to_string())?;
        recipe.sort_all_objects();
        let bytes = serde_json::to_vec(&recipe).map_err(|e| e.to_string())?;
        Ok(road_network::RecipeReceipt {
            source_sha256: self.source_sha256.clone(),
            recipe_sha256: format!("{:x}", Sha256::digest(bytes)),
            seed,
        })
    }

    pub(crate) fn verify_report(&self, report: &road_network::Report, seed: u32) -> Result<(), String> {
        if report.recipe.as_ref() != Some(&self.result_receipt(seed)?) {
            return Err("Generated road source, seed or settings differ from the recipe".into());
        }
        Ok(())
    }

    /// Rebind derived metadata only after both source snapshots and replacement
    /// dependencies have been verified against the same installed game data.
    pub fn rebind_source(
        &self,
        previous: &Source,
        current: &Source,
        surface: Option<String>,
        roadside: Option<String>,
        layout: Option<crate::source_road_planning::Policy>,
    ) -> Result<Self, String> {
        if self.source_sha256 != previous.sha256
            || previous.source_receipts().get(..2) != current.source_receipts().get(..2)
            || previous.source_receipts().len() < 2
            || current.profile.revision <= previous.profile.revision
        {
            return Err("Cannot migrate road metadata across Rust installations".into());
        }
        for old in previous
            .source_receipts()
            .iter()
            .chain(previous.gameplay_receipts())
        {
            if current
                .source_receipts()
                .iter()
                .chain(current.gameplay_receipts())
                .any(|new| new.relative_path == old.relative_path && new != old)
            {
                return Err("Rust sources changed during road metadata migration".into());
            }
        }
        if self.surface_sha256.is_some() && surface.is_none() {
            return Err("Matching replacement road surfaces are unavailable".into());
        }
        let native_sampling = crate::road_cost_source::Profile::bundled_joined(current)?
            .is_some_and(|profile| profile.policy.sampling.is_some());
        let mut next = if native_sampling {
            Self::for_prepared_source(
                current,
                surface
                    .clone()
                    .ok_or("Missing replacement road surface templates")?,
                roadside
                    .clone()
                    .ok_or("Missing replacement roadside data")?,
                layout
                    .clone()
                    .ok_or("Missing replacement ordered road inputs")?,
                false,
            )?
        } else {
            Self::for_source(current)
        };
        next.maximum_grade = self.maximum_grade;
        next.maximum_cut_fill = self.maximum_cut_fill;
        next.main_ring = self.main_ring;
        next.include_trails = self.include_trails;
        if next.main_ring == Some(false) {
            next.ring_costmap = None;
        }
        next.apply_source_defaults(current)?;
        next.surface_sha256 = surface;
        for (old, new) in [
            (&self.access_costmap, &mut next.access_costmap),
            (&self.ring_costmap, &mut next.ring_costmap),
        ] {
            if let Some(old) = old {
                let new = new
                    .as_mut()
                    .ok_or("Replacement source routing data is unavailable")?;
                new.maximum_work = new.maximum_work.min(old.maximum_work);
                new.policy.sampling = old.policy.sampling.clone();
            }
        }
        if let Some(saved) = self.entrance_launch {
            let launch = next
                .entrance_launch
                .as_mut()
                .ok_or("Replacement entrance checks are unavailable")?;
            launch.minimum_run_widths = saved.minimum_run_widths;
            launch.sample_spacing = saved.sample_spacing;
            launch.maximum_work = saved.maximum_work;
        }
        if let Some(saved) = &self.staged {
            let mut layout = layout;
            if saved.layout.is_some() && layout.is_none() {
                return Err("Replacement ordered road data is unavailable".into());
            }
            if let (Some(old), Some(new)) = (&saved.layout, &mut layout) {
                new.ring_clearance = old.ring_clearance.clone();
                new.terrain_entrances = old.terrain_entrances.clone();
            }
            next.staged = Some(crate::road_generation::Policy {
                engine: saved.engine.clone(),
                roadside_source_sha256: roadside
                    .ok_or("Replacement roadside data is unavailable")?,
                layout,
                roadside_preferences: saved.roadside_preferences.clone(),
            });
            if saved.engine != crate::road_generation::VERSION
                || self
                    .entrance_launch
                    .is_some_and(|p| p.engine == road_network::LaunchEngine::GlobalEarthwork)
            {
                next.set_global_earthwork(true)?;
            }
        }
        next.apply_correctness_defaults();
        next.validate()?;
        Ok(next)
    }
    pub fn apply_correctness_defaults(&mut self) {
        if let Some(staged) = &mut self.staged {
            match staged.engine.as_str() {
                crate::road_generation::RAW_SOURCE_VERSION => {
                    staged.engine = crate::road_generation::CONNECTED_SOURCE_VERSION.into();
                }
                crate::road_generation::SOURCE_COUPLED_VERSION => {
                    staged.engine = crate::road_generation::CONNECTED_COUPLED_VERSION.into();
                }
                _ => (),
            }
        }
        if let Some(layout) = self.staged.as_mut().and_then(|s| s.layout.as_mut()) {
            if !matches!(
                layout.terrain_entrances.as_deref(),
                Some(
                    papyrus_terrain::source_road_route::TERRAIN_ENTRANCE_BOOTSTRAP_VERSION
                    | papyrus_terrain::source_road_route::TERRAIN_ENTRANCE_PINNED_VERSION
                    | papyrus_terrain::source_road_route::TERRAIN_ENTRANCE_ROUTED_VERSION
                )
            ) {
                layout.terrain_entrances = Some(
                    papyrus_terrain::source_road_route::TERRAIN_ENTRANCE_ROUTED_VERSION.into(),
                );
            }
            let clearance =
                layout
                    .ring_clearance
                    .get_or_insert_with(|| crate::source_road_planning::RingClearance {
                        engine: crate::source_road_planning::RING_CLEARANCE_VERSION.into(),
                        radius_cells: 1,
                    });
            if clearance.engine != crate::source_road_planning::RING_CLEARANCE_VERSION {
                clearance.engine = crate::source_road_planning::RING_CLEARANCE_VERSION.into();
            }
        }
        if self.engine == road_network::COST_VERSION && self.access_costmap.is_some() {
            crate::generation_defaults::set_version(
                &mut self.access_smoothing,
                papyrus_terrain::road_smoothing::VERSION,
            );
            crate::generation_defaults::set_version(
                &mut self.access_search,
                road_network::COMPONENT_VERSION,
            );
            let launch = self
                .entrance_launch
                .get_or_insert(road_network::LaunchPolicy {
                    engine: road_network::LaunchEngine::SurfaceChecked,
                    minimum_run_widths: 1.,
                    sample_spacing: 1.,
                    maximum_work: 2_000_000,
                });
            if launch.engine != road_network::LaunchEngine::GlobalEarthwork {
                launch.engine = road_network::LaunchEngine::SurfaceChecked;
            }
        }
    }
    pub fn apply_source_defaults(&mut self, source: &Source) -> Result<(), String> {
        self.validate()?;
        if self.source_sha256 != source.sha256 {
            return Err("Road settings do not match the selected installation".into());
        }
        let mut next = self.clone();
        let defaults = Self::for_source(source);
        next.engine = defaults.engine;
        next.ring_search = defaults.ring_search;
        next.access_search = defaults.access_search;
        next.post_topology = defaults.post_topology;
        if let Some(profile) = crate::road_cost_source::Profile::bundled_joined(source)? {
            profile.check(source)?;
            // Bound recipe policies are replay inputs. Fill missing profiles
            // from the saved family before consulting newer bundled defaults.
            let profile = next
                .access_costmap
                .clone()
                .or_else(|| next.ring_costmap.clone())
                .unwrap_or(profile);
            profile.check(source)?;
            let access = next.access_costmap.get_or_insert_with(|| profile.clone());
            access.check(source)?;
            next.access_river_water = access.joins.as_ref().and_then(|j| j.river_water.clone());
            if next.main_ring != Some(false) {
                next.ring_costmap.get_or_insert(profile).check(source)?;
            } else {
                next.ring_costmap = None;
            }
        }
        next.apply_correctness_defaults();
        next.validate()?;
        *self = next;
        Ok(())
    }
    /// Explicitly select the complete shared-ground road workflow. Existing
    /// recipes are never upgraded merely by loading or drawing their controls.
    pub fn set_global_earthwork(&mut self, enabled: bool) -> Result<(), String> {
        let mut next = self.clone();
        if enabled {
            if next.staged.is_none()
                || next.surface_sha256.is_none()
                || next.engine != road_network::COST_VERSION
            {
                return Err("Shared road terrain fitting requires roadside terrain fitting and road surfaces".into());
            }
            next.access_search = Some(road_network::COMPONENT_VERSION.into());
            let launch = next
                .entrance_launch
                .get_or_insert(road_network::LaunchPolicy {
                    engine: road_network::LaunchEngine::GlobalEarthwork,
                    minimum_run_widths: 1.,
                    sample_spacing: 1.,
                    maximum_work: 2_000_000,
                });
            launch.engine = road_network::LaunchEngine::GlobalEarthwork;
        } else if let Some(launch) = &mut next.entrance_launch
            && launch.engine == road_network::LaunchEngine::GlobalEarthwork
        {
            launch.engine = road_network::LaunchEngine::SurfaceChecked;
        }
        next.validate()?;
        *self = next;
        Ok(())
    }
    pub fn new(source_sha256: String) -> Self {
        Self {
            staged: None,
            engine: road_network::VERSION.into(),
            source_sha256,
            maximum_grade: 0.2,
            maximum_cut_fill: 8.,
            surface_sha256: None,
            main_ring: None,
            include_trails: None,
            ring_search: None,
            ring_costmap: None,
            access_costmap: None,
            access_smoothing: None,
            access_river_water: None,
            access_search: None,
            entrance_launch: None,
            post_topology: None,
        }
    }
    pub fn for_source(source: &Source) -> Self {
        let mut recipe = Self::new(source.sha256.clone());
        if source.profile.access_routing.is_some() {
            recipe.engine = road_network::COST_VERSION.into();
            recipe.access_search = Some(road_network::COMPONENT_VERSION.into());
            recipe.ring_search = Some(papyrus_terrain::road_ring::RECOVERY_VERSION.into());
        } else if source.profile.ring.is_some() {
            recipe.engine = road_network::RING_VERSION.into();
        } else if source.profile.shaping.is_some() {
            recipe.engine = road_network::SHAPED_VERSION.into();
        } else if source.profile.entrances.is_some() {
            recipe.engine = road_network::ACCESS_VERSION.into();
        } else if source.profile.trails.is_some() {
            recipe.engine = road_network::FAMILY_VERSION.into();
        }
        if source.profile.post_topology.is_some() {
            recipe.post_topology = Some(papyrus_terrain::road_topology::VERSION.into());
        }
        recipe
    }
    /// Bind the mutually dependent source-lattice inputs together before
    /// validation. A newly enabled native-sampling recipe needs all of them.
    pub fn for_prepared_source(
        source: &Source,
        surface: String,
        roadside: String,
        layout: crate::source_road_planning::Policy,
        has_late_rails: bool,
    ) -> Result<Self, String> {
        let mut recipe = Self::for_source(source);
        let costs = crate::road_cost_source::Profile::bundled_joined(source)?
            .ok_or("Matching source road routing data is unavailable")?;
        costs.check(source)?;
        recipe.surface_sha256 = Some(surface);
        recipe.access_costmap = Some(costs.clone());
        recipe.ring_costmap = Some(costs);
        recipe.staged = Some(crate::road_generation::Policy::for_new_recipe(
            roadside,
            Some(layout),
            has_late_rails,
        )?);
        recipe.apply_source_defaults(source)?;
        Ok(recipe)
    }
    pub fn validate(&self) -> Result<(), String> {
        if self
            .access_costmap
            .iter()
            .chain(&self.ring_costmap)
            .any(|p| p.policy.sampling.is_some())
            && !self.staged.as_ref().is_some_and(|p| p.layout.is_some())
        {
            return Err("Native road sampling requires an ordered road layout".into());
        }
        if let Some(policy) = &self.staged {
            policy.validate()?;
            if policy.layout.is_some()
                && (self.access_costmap.is_none()
                    || (self.main_ring != Some(false)
                        && self.ring_costmap.as_ref() != self.access_costmap.as_ref()))
            {
                return Err(
                    "Ordered roads require bound access costs and matching active ring costs"
                        .into(),
                );
            }
            if self.engine != road_network::COST_VERSION || self.surface_sha256.is_none() {
                return Err(
                    "Staged roads require source-cost routing and bound surface templates".into(),
                );
            }
        }
        if let Some(launch) = self.entrance_launch {
            launch.validate()?;
            if self.engine != road_network::COST_VERSION
                || self.access_search.as_deref() != Some(road_network::COMPONENT_VERSION)
            {
                return Err(
                    "Checked road launches require connected source-profiled access".into(),
                );
            }
        }
        if let Some(water) = &self.access_river_water {
            water.validate()?;
            if self
                .access_costmap
                .as_ref()
                .is_none_or(|p| p.policy.water_topology != water.topology as u32)
            {
                return Err("River-aware access requires matching source water costs".into());
            }
        }
        if self.access_smoothing.as_ref().is_some_and(|v| {
            v != papyrus_terrain::road_smoothing::VERSION || self.access_costmap.is_none()
        }) {
            return Err("Constrained road shaping requires matching source access costs".into());
        }
        if let Some(profile) = &self.access_costmap {
            profile.validate()?;
            if self.engine != road_network::COST_VERSION
                || self.access_search.as_deref() != Some(road_network::COMPONENT_VERSION)
                || profile.infrastructure_sha256 != self.source_sha256
            {
                return Err(
                    "Source access costs require matching connected road-exit rules".into(),
                );
            }
        }
        if let Some(costmap) = &self.ring_costmap {
            costmap.validate()?;
            if self.engine != road_network::COST_VERSION
                || self.ring_search.is_none()
                || self.main_ring == Some(false)
                || costmap.infrastructure_sha256 != self.source_sha256
            {
                return Err("Source ring costs require the matching active recovery recipe".into());
            }
        }
        if self.post_topology.as_ref().is_some_and(|p| {
            self.engine != road_network::COST_VERSION || p != papyrus_terrain::road_topology::VERSION
        }) {
            return Err("Unsupported post-road topology policy".into());
        }
        if self.access_search.as_ref().is_some_and(|p| {
            self.engine != road_network::COST_VERSION || p != road_network::COMPONENT_VERSION
        }) {
            return Err("Unsupported road entrance search policy".into());
        }

        if self.ring_search.as_ref().is_some_and(|s| {
            self.engine != road_network::COST_VERSION
                || s != papyrus_terrain::road_ring::RECOVERY_VERSION
        }) {
            return Err("Unsupported main ring search policy".into());
        }
        if (self.main_ring.is_some() || self.include_trails.is_some())
            && self.engine != road_network::COST_VERSION
        {
            return Err(
                "Independent road-family switches require a source-cost road recipe".into(),
            );
        }
        if let Some(digest) = &self.surface_sha256
            && (![
                road_network::SHAPED_VERSION,
                road_network::RING_VERSION,
                road_network::COST_VERSION,
            ]
            .contains(&self.engine.as_str())
                || !hash(digest))
        {
            return Err("Road surfaces require a shaped road recipe and source fingerprint".into());
        }
        if ![
            road_network::VERSION,
            road_network::FAMILY_VERSION,
            road_network::ACCESS_VERSION,
            road_network::SHAPED_VERSION,
            road_network::RING_VERSION,
            road_network::COST_VERSION,
        ]
        .contains(&self.engine.as_str())
            || !hash(&self.source_sha256)
            || !self.maximum_grade.is_finite()
            || !(0.01..=0.5).contains(&self.maximum_grade)
            || !self.maximum_cut_fill.is_finite()
            || !(0. ..=24.).contains(&self.maximum_cut_fill)
        {
            return Err("Unsupported road recipe or source fingerprint".into());
        }
        Ok(())
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    format: String,
    schema: u32,
    pub id: String,
    #[serde(default = "first_revision")]
    pub revision: u32,
    #[serde(default)]
    pub topology_flags: Option<BTreeMap<String, u32>>,
    native_sha256: String,
    scene_sha256: String,
    pub road_type: i32,
    pub road_topology: i32,
    pub roadside_topology: i32,
    pub protected_topology: i32,
    pub splat_channel: usize,
    pub width: f32,
    pub shoulder: f32,
    pub terrain_offset: f32,
    #[serde(default)]
    pub road_hierarchy: Option<u32>,
    #[serde(default)]
    pub trails: Option<TrailPolicy>,
    #[serde(default)]
    pub entrances: Option<road_network::EntrancePolicy>,
    #[serde(default)]
    pub shaping: Option<road_network::ShapingPolicy>,
    #[serde(default)]
    pub ring: Option<papyrus_terrain::road_ring::Policy>,
    #[serde(default)]
    pub access_routing: Option<road_network::TraversalPolicy>,
    #[serde(default)]
    pub post_topology: Option<papyrus_terrain::road_topology::Policy>,
    #[serde(default)]
    code_sources: Vec<SourceFile>,
    #[serde(default)]
    map_scene: Option<SourceFile>,
    #[serde(default)]
    layout_order: Vec<i32>,
    #[serde(default)]
    layout_mode: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stage_layout: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrailPolicy {
    pub infrastructure_type: i32,
    pub hierarchy: u32,
    pub width: f32,
    pub shoulder: f32,
    pub terrain_offset: f32,
    pub splat_channel: usize,
    pub network_sample_stride: usize,
    pub seed_domain: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LayoutStage {
    object_id: i64,
    order: usize,
    infrastructure_type: i32,
    mode: i32,
    #[serde(deserialize_with = "Option::deserialize")]
    disabled: Option<bool>,
    component_enabled: bool,
    hierarchy_active: bool,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RingStage {
    object_id: i64,
    order: usize,
    mode: i32,
    #[serde(deserialize_with = "Option::deserialize")]
    disabled: Option<bool>,
    component_enabled: bool,
    hierarchy_active: bool,
    minimum_world_size: u32,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TopologyStage {
    object_id: i64,
    order: usize,
    mode: i32,
    #[serde(deserialize_with = "Option::deserialize")]
    disabled: Option<bool>,
    component_enabled: bool,
    hierarchy_active: bool,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Connector {
    pub object_id: i64,
    pub infrastructure_type: i32,
    pub position: [f32; 3],
    pub forward: [f32; 3],
    pub hierarchy_active_without_root: bool,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Issue {
    object_id: i64,
    reason: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Prefab {
    version: u32,
    space: String,
    pub complete: bool,
    pub connectors: Vec<Connector>,
    issues: Vec<Issue>,
    nodes_visited: usize,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    format: String,
    schema: u32,
    adapter: String,
    pub catalog_sha256: String,
    sources: Vec<SourceFile>,
    extractor_sha256: String,
    pub profile: Profile,
    pub prefabs: BTreeMap<String, Prefab>,
    #[serde(default)]
    layout_stages: Vec<LayoutStage>,
    #[serde(default)]
    ring_stage: Option<RingStage>,
    #[serde(default)]
    topology_stage: Option<TopologyStage>,
    #[serde(skip)]
    pub sha256: String,
}

type Retained = RefCell<Option<Arc<Source>>>;
thread_local! {
    static CURRENT: RefCell<Weak<Retained>> = const { RefCell::new(Weak::new()) };
}

/// One verified infrastructure snapshot per composition worker. Inner
/// composition stages share the outer scope, including layout retries.
pub(crate) struct Scope {
    _retained: Rc<Retained>,
}

impl Scope {
    pub(crate) fn new() -> Self {
        CURRENT.with(|current| {
            let previous = current.borrow().upgrade();
            let retained = previous.unwrap_or_else(|| {
                let retained = Rc::new(RefCell::new(None));
                *current.borrow_mut() = Rc::downgrade(&retained);
                retained
            });
            Self { _retained: retained }
        })
    }
}

fn first_revision() -> u32 {
    1
}
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn one_bit(value: i32) -> bool {
    (value as u32).count_ones() == 1
}
impl Source {
    pub(crate) fn stage_layout(&self) -> Option<&str> {
        self.profile.stage_layout.as_deref()
    }
    pub fn semantic_sha256(&self) -> Result<String, String> {
        let mut value = serde_json::to_value(self).map_err(|e| e.to_string())?;
        let fields = value
            .as_object_mut()
            .ok_or("Invalid infrastructure metadata")?;
        fields.remove("catalog_sha256");
        fields.remove("extractor_sha256");
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&value).map_err(|e| e.to_string())?)
        ))
    }
    pub(crate) fn road_object_receipts(&self) -> Result<Vec<SourceFile>, String> {
        if self.profile.code_sources.is_empty() {
            return Err("Road objects require bound gameplay receipts".into());
        }
        let mut receipts = self.sources.clone();
        receipts.extend(self.profile.code_sources.clone());
        receipts.push(
            self.profile
                .map_scene
                .clone()
                .ok_or("Missing bound map scene")?,
        );
        Ok(receipts)
    }
    pub(crate) fn source_receipts(&self) -> &[SourceFile] {
        &self.sources
    }
    pub(crate) fn gameplay_receipts(&self) -> &[SourceFile] {
        &self.profile.code_sources
    }
    pub(crate) fn map_receipt(&self) -> Option<&SourceFile> {
        self.profile.map_scene.as_ref()
    }
    /// Catalog must already have passed installation verification. The source list
    /// is compared exactly; metadata never substitutes for an unverified catalog.
    pub fn load(
        path: &Path,
        catalog: &Catalog,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        if cancelled() {
            return Err("Infrastructure verification cancelled".into());
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        Self::load_document(&bytes, catalog, cancelled)
    }
    pub(crate) fn load_document(
        bytes: &[u8],
        catalog: &Catalog,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        if bytes.len() as u64 > LIMIT {
            return Err("Infrastructure metadata exceeds 16 MiB".into());
        }
        let mut doc: Self = serde_json::from_slice(bytes)
            .map_err(|e| format!("Invalid infrastructure metadata: {e}"))?;
        let p = &doc.profile;
        let hierarchy = match p.stage_layout.as_deref() {
            None => false,
            Some("mode-description") => true,
            _ => return Err("Unsupported road stage activation layout".into()),
        };
        if doc.format != "papyrus-infrastructure-source"
            || doc.schema != if hierarchy { 2 } else { 1 }
            || doc.adapter
                != if hierarchy {
                    "papyrus-path-connectors-2"
                } else {
                    "papyrus-path-connectors-1"
                }
            || doc.catalog_sha256 != catalog.sha256
            || doc.sources != catalog.sources
            || !hash(&doc.extractor_sha256)
            || p.format != "papyrus-infrastructure-profile"
            || p.schema != 1
            || p.id.is_empty()
            || p.id.len() > 128
            || p.native_sha256 != catalog.installation.native.sha256
            || doc
                .sources
                .iter()
                .find(|s| s.relative_path == "Bundles/shared/assetscenes.bundle")
                .is_none_or(|s| s.sha256 != p.scene_sha256)
            || p.splat_channel >= 8
            || !one_bit(p.road_topology)
            || !one_bit(p.roadside_topology)
            || p.road_topology == p.roadside_topology
            || p.road_type < 0
            || !p.width.is_finite()
            || !(1. ..=32.).contains(&p.width)
            || !p.shoulder.is_finite()
            || !(1. ..=32.).contains(&p.shoulder)
            || !p.terrain_offset.is_finite()
            || p.terrain_offset.abs() > 2.
            || doc.prefabs.is_empty()
            || doc.prefabs.len() > 4096
            || doc.prefabs.len() != catalog.entries.len()
        {
            return Err("Infrastructure schema, source, catalog or layer profile mismatch".into());
        }
        if !(1..=1024).contains(&p.revision) || hierarchy && (p.revision < 9 || p.trails.is_none())
        {
            return Err("Invalid infrastructure profile revision".into());
        }
        if let Some(flags) = &p.topology_flags {
            if flags.is_empty()
                || flags.len() > 32
                || flags.values().any(|v| v.count_ones() != 1)
                || flags
                    .values()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != flags.len()
            {
                return Err("Invalid source topology dictionary".into());
            }
            let mut required = 0_u32;
            for name in ["Cliff", "Ocean", "Monument", "River", "Lake", "Building"] {
                required |= *flags.get(name).ok_or("Missing protected topology role")?;
            }
            if flags.get("Road").copied() != Some(p.road_topology as u32)
                || flags.get("Roadside").copied() != Some(p.roadside_topology as u32)
                || p.protected_topology as u32 & required != required
            {
                return Err("Road profile masks contradict source topology roles".into());
            }
        }
        if let Some(t) = &p.trails {
            let code_path = |p: &str| {
                matches!(
                    p,
                    "GameAssembly.dll"
                        | "RustClient_Data/il2cpp_data/Metadata/global-metadata.dat"
                        | "RustClient_Data/Managed/Assembly-CSharp.dll"
                        | "RustDedicated_Data/Managed/Assembly-CSharp.dll"
                )
            };
            let mut paths = std::collections::BTreeSet::new();
            if p.revision < 3
                || p.road_hierarchy.is_none_or(|h| !(1..=16).contains(&h))
                || !(1..=16).contains(&t.hierarchy)
                || !(0..=64).contains(&t.infrastructure_type)
                || t.infrastructure_type == p.road_type
                || !t.width.is_finite()
                || !(1. ..=32.).contains(&t.width)
                || !t.shoulder.is_finite()
                || !(1. ..=32.).contains(&t.shoulder)
                || !t.terrain_offset.is_finite()
                || t.terrain_offset.abs() > 2.
                || t.splat_channel >= 8
                || !(1..=1024).contains(&t.network_sample_stride)
                || t.seed_domain.is_empty()
                || t.seed_domain.len() > 128
                || !t
                    .seed_domain
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
                || p.topology_flags.is_none()
                || p.code_sources.is_empty()
                || p.code_sources.len() > 2
                || p.code_sources.iter().any(|r| {
                    !code_path(&r.relative_path)
                        || !hash(&r.sha256)
                        || r.bytes == 0
                        || r.bytes > 1024 * 1024 * 1024
                        || !paths.insert(&r.relative_path)
                })
                || p.map_scene.as_ref().is_none_or(|r| {
                    r.relative_path != "Bundles/maps/maps.bundle"
                        || !hash(&r.sha256)
                        || r.bytes == 0
                        || r.bytes > 512 * 1024 * 1024
                })
                || p.layout_order != [p.road_type, t.infrastructure_type]
                || p.layout_mode != Some(-1)
                || doc.layout_stages.len() != 2
                || doc.layout_stages.iter().enumerate().any(|(i, stage)| {
                    stage.object_id == 0
                        || stage.order != i
                        || stage.infrastructure_type != p.layout_order[i]
                        || Some(stage.mode) != p.layout_mode
                        || stage.disabled != if hierarchy { None } else { Some(false) }
                        || !stage.component_enabled
                        || !stage.hierarchy_active
                })
                || doc.layout_stages[0].object_id == doc.layout_stages[1].object_id
            {
                return Err("Unsupported trail policy, scene order or gameplay receipts".into());
            }
        } else if !doc.layout_stages.is_empty()
            || !p.code_sources.is_empty()
            || p.map_scene.is_some()
            || !p.layout_order.is_empty()
            || p.layout_mode.is_some()
            || p.road_hierarchy.is_some()
        {
            return Err("Road stage metadata requires an explicit trail policy".into());
        }
        if let Some(policy) = p.entrances {
            policy.validate()?;
            if p.revision < 4
                || p.trails.is_none()
                || p.topology_flags
                    .as_ref()
                    .and_then(|f| Some(*f.get("Monument")? | *f.get("Building")?))
                    != Some(policy.owned_topology as u32)
                || p.protected_topology & policy.owned_topology != policy.owned_topology
            {
                return Err("Road entrance policy contradicts source topology or revision".into());
            }
        }
        if let Some(policy) = p.shaping {
            policy.validate()?;
            if p.revision < 5 || p.entrances.is_none() {
                return Err("Road shaping requires a bound entrance profile revision".into());
            }
        }
        if let Some(policy) = p.ring {
            policy.validate()?;
            let stage = doc
                .ring_stage
                .as_ref()
                .ok_or("Missing captured ring stage")?;
            if p.revision < 6
                || p.shaping.is_none()
                || stage.object_id == 0
                || stage.order != 0
                || stage.mode != -1
                || stage.disabled != if hierarchy { None } else { Some(false) }
                || !stage.component_enabled
                || !stage.hierarchy_active
                || stage.minimum_world_size != policy.minimum_world_size
                || doc
                    .layout_stages
                    .iter()
                    .any(|s| s.object_id == stage.object_id)
            {
                return Err("Main road ring differs from source stage or profile revision".into());
            }
        } else if doc.ring_stage.is_some() {
            return Err("Captured road ring requires an explicit profile".into());
        }
        if let Some(policy) = p.access_routing {
            policy.validate()?;
            if p.revision < 7
                || p.shaping.is_none()
                || p.ring.is_none()
                || p.topology_flags
                    .as_ref()
                    .and_then(|f| f.get("Cliff").copied())
                    != Some(policy.traversable_topology as u32)
                || p.protected_topology & policy.traversable_topology != policy.traversable_topology
            {
                return Err(
                    "Road traversal costs contradict captured source topology or profile revision"
                        .into(),
                );
            }
        }
        if let Some(policy) = p.post_topology {
            policy.validate()?;
            let stage = doc
                .topology_stage
                .as_ref()
                .ok_or("Missing captured road topology stage")?;
            let flags = p.topology_flags.as_ref().ok_or("Missing topology roles")?;
            let role = |name: &str| {
                flags
                    .get(name)
                    .copied()
                    .ok_or("Missing post-road topology role")
            };
            if p.revision < 8
                || p.access_routing.is_none()
                || policy.source_mask as u32 != role("Road")? | role("Roadside")?
                || policy.eligible_mask as u32 != role("Field")? | role("Beach")? | role("Forest")?
                || policy.roadside_flag != p.roadside_topology
                || policy.cliff_flag as u32 != role("Cliff")?
                || stage.object_id == 0
                || stage.order != 3
                || stage.mode != -1
                || stage.disabled != if hierarchy { None } else { Some(false) }
                || !stage.component_enabled
                || !stage.hierarchy_active
                || doc
                    .layout_stages
                    .iter()
                    .any(|s| s.object_id == stage.object_id)
                || doc
                    .ring_stage
                    .as_ref()
                    .is_some_and(|s| s.object_id == stage.object_id)
            {
                return Err("Post-road topology differs from source roles or scene order".into());
            }
        } else if doc.topology_stage.is_some() {
            return Err("Captured road topology stage requires a bound policy".into());
        }
        for (path, prefab) in &doc.prefabs {
            if cancelled() {
                return Err("Infrastructure verification cancelled".into());
            }
            if !catalog.entries.contains_key(path)
                || prefab.version != 1
                || prefab.space != "prefab-root-local-unity"
                || prefab.nodes_visited > 65536
                || prefab.connectors.len() > 256
                || prefab.issues.len() > 128
                || (prefab.complete && !prefab.issues.is_empty())
                || (!prefab.complete && prefab.issues.is_empty())
                || prefab.issues.iter().any(|i| {
                    i.reason.is_empty() || i.reason.len() > 1024 || i.object_id == i64::MIN
                })
            {
                return Err("Invalid infrastructure prefab coverage report".into());
            }
            let mut ids = std::collections::BTreeSet::new();
            for c in &prefab.connectors {
                let length = c.forward.iter().map(|v| v * v).sum::<f32>().sqrt();
                if c.object_id == 0
                    || !ids.insert(c.object_id)
                    || !length.is_finite()
                    || (length - 1.).abs() > 0.001
                    || c.position
                        .iter()
                        .any(|v| !v.is_finite() || v.abs() > 10000.)
                    || c.infrastructure_type < 0
                {
                    return Err("Invalid authored connector pose or identity".into());
                }
            }
        }
        doc.sha256 = format!("{:x}", Sha256::digest(bytes));
        Ok(doc)
    }
    pub fn cache(
        path: &Path,
        catalog: &Catalog,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        let root = crate::storage::local_data()?.join("papyrus-infrastructure-cache");
        Self::cache_in(path, catalog, &root, cancelled)
    }
    fn cache_in(
        path: &Path,
        catalog: &Catalog,
        root: &Path,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        let doc = Self::load(path, catalog, &cancelled)?;
        std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
        let destination = root.join(&doc.sha256);
        if destination.exists() {
            let cached = Self::load(
                &destination.join("infrastructure.json"),
                catalog,
                &cancelled,
            )?;
            if cached.sha256 != doc.sha256 {
                return Err("Existing infrastructure cache has changed".into());
            }
        } else {
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .map_err(|e| e.to_string())?
                .take(LIMIT + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if format!("{:x}", Sha256::digest(&bytes)) != doc.sha256 {
                return Err("Infrastructure input changed during caching".into());
            }
            let stage = tempfile::Builder::new()
                .prefix(".infrastructure-")
                .tempdir_in(root)
                .map_err(|e| e.to_string())?;
            let mut file = std::fs::File::create(stage.path().join("infrastructure.json"))
                .map_err(|e| e.to_string())?;
            use std::io::Write;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
            // Windows requires descendant handles to close before this directory moves.
            drop(file);
            if cancelled() {
                return Err("Infrastructure caching cancelled".into());
            }
            if let Err(error) = std::fs::rename(stage.path(), &destination) {
                if !destination.is_dir() {
                    return Err(format!("Cannot publish infrastructure metadata: {error}"));
                }
                let cached = Self::load(
                    &destination.join("infrastructure.json"),
                    catalog,
                    &cancelled,
                )?;
                if cached.sha256 != doc.sha256 {
                    return Err("Concurrent infrastructure cache content differs".into());
                }
            }
        }
        Ok(doc)
    }
    pub fn discover(
        catalog: &Catalog,
        pinned: Option<&str>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        if let Some(pin) = pinned {
            return Self::cached(catalog, &Recipe::new(pin.into()), cancelled);
        }
        let root = crate::storage::local_data()?.join("papyrus-infrastructure-cache");
        let mut paths = Vec::new();
        if root.is_dir() {
            for (i, entry) in std::fs::read_dir(root)
                .map_err(|e| e.to_string())?
                .enumerate()
            {
                if i >= 256 {
                    return Err("Too many infrastructure snapshots".into());
                }
                let path = entry
                    .map_err(|e| e.to_string())?
                    .path()
                    .join("infrastructure.json");
                if path.is_file() {
                    paths.push(path);
                }
            }
        }
        paths.sort();
        let mut best: Option<Self> = None;
        for path in paths {
            if cancelled() {
                return Err("Infrastructure discovery cancelled".into());
            }
            if let Ok(doc) = Self::load(&path, catalog, &cancelled)
                && path
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str())
                    == Some(&doc.sha256)
                && best.as_ref().is_none_or(|old| {
                    (doc.profile.revision, &doc.sha256) > (old.profile.revision, &old.sha256)
                })
            {
                best = Some(doc);
            }
        }
        if let Some(doc) = best {
            return Ok(doc);
        }
        Err(
            "No matching extracted infrastructure source. Prepare it from this Rust installation."
                .into(),
        )
    }
    pub fn verify_installation(
        &self,
        root: &Path,
        cancelled: impl Fn() -> bool,
    ) -> Result<(), String> {
        for receipt in self
            .profile
            .code_sources
            .iter()
            .chain(self.profile.map_scene.iter())
        {
            let limit = if receipt.relative_path == "Bundles/maps/maps.bundle" {
                512 * 1024 * 1024
            } else {
                1024 * 1024 * 1024
            };
            if crate::installation::hash_file(root, &receipt.relative_path, limit, &cancelled)?
                != *receipt
            {
                return Err(
                    "Road gameplay code or map scene differs from selected installation".into(),
                );
            }
        }
        if cancelled() {
            return Err("Road source verification cancelled".into());
        }
        Ok(())
    }
    pub fn cached(
        catalog: &Catalog,
        recipe: &Recipe,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        recipe.validate()?;
        let root = crate::storage::local_data()?;
        let path = crate::client_cache::require_pinned_document(
            &root,
            "papyrus-infrastructure-cache",
            &recipe.source_sha256,
            "infrastructure.json",
            "road connector",
        )?;
        let doc = Self::load(&path, catalog, cancelled)?;
        if doc.sha256 != recipe.source_sha256 {
            return Err("Road source snapshot differs from saved fingerprint".into());
        }
        Ok(doc)
    }

    pub(crate) fn cached_shared(
        catalog: &Catalog,
        recipe: &Recipe,
        cancelled: impl Fn() -> bool,
    ) -> Result<Arc<Self>, String> {
        Self::share_cached(catalog, recipe, &cancelled, || {
            Self::cached(catalog, recipe, &cancelled)
        })
    }

    fn share_cached(
        catalog: &Catalog,
        recipe: &Recipe,
        cancelled: &impl Fn() -> bool,
        load: impl FnOnce() -> Result<Self, String>,
    ) -> Result<Arc<Self>, String> {
        if cancelled() {
            return Err("Infrastructure verification cancelled".into());
        }
        let scope = CURRENT.with(|current| current.borrow().upgrade());
        let retained = scope.as_ref().and_then(|scope| {
            scope.borrow().as_ref().filter(|source| {
                source.sha256 == recipe.source_sha256
                    && source.catalog_sha256 == catalog.sha256
                    && source.sources == catalog.sources
                    && source.profile.native_sha256 == catalog.installation.native.sha256
            }).cloned()
        });
        let source = if let Some(source) = retained {
            recipe.validate()?;
            source
        } else {
            Arc::new(load()?)
        };
        if cancelled() {
            return Err("Infrastructure verification cancelled".into());
        }
        if let Some(scope) = scope {
            *scope.borrow_mut() = Some(source.clone());
        }
        Ok(source)
    }
}
#[allow(clippy::too_many_arguments)] // Explicit optional revision policies preserve legacy results.
fn source_access(
    layers: &mut papyrus_terrain::native_layers::Layers,
    alpha: &[u8],
    sockets: &[road_network::Socket],
    obstacles: &[road_network::Obstacle],
    access: road_network::Access<'_>,
    shaping: Option<road_network::ShapingPolicy>,
    traversal: Option<road_network::TraversalPolicy>,
    components: bool,
    costs: Option<&papyrus_terrain::road_costmap::Grid>,
    constrained: bool,
    water: Option<papyrus_terrain::road_water::Guide<'_>>,
    launch: Option<road_network::LaunchPolicy>,
    ground: Option<&[i16]>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<road_network::Report>, String> {
    if let Some(launch) = launch {
        if !components {
            return Err("Checked launch requires connected entrance search".into());
        }
        return road_network::build_launched_access(
            layers,
            alpha,
            sockets,
            obstacles,
            road_network::LaunchedAccess {
                profile: road_network::ProfiledAccess {
                    access,
                    shaping: shaping.ok_or("Checked launch requires shaping")?,
                    traversal: traversal.ok_or("Checked launch requires source traversal")?,
                },
                policy: launch,
                costs: costs.map(|c| c.costs.as_slice()),
                constrained,
                water,
                ground,
            },
            cancelled,
        );
    }
    if let Some(traversal) = traversal {
        if let Some(costs) = costs {
            if costs.resolution != 257 {
                return Err("Invalid access source-cost resolution".into());
            }
            if let Some(water) = water {
                return road_network::build_river_source_access(
                    layers,
                    alpha,
                    sockets,
                    obstacles,
                    road_network::ProfiledAccess {
                        access,
                        shaping: shaping.ok_or("Road costs require shaping")?,
                        traversal,
                    },
                    &costs.costs,
                    constrained,
                    water,
                    cancelled,
                );
            }
            let build = if constrained {
                road_network::build_constrained_source_access
            } else {
                road_network::build_source_cost_access
            };
            return build(
                layers,
                alpha,
                sockets,
                obstacles,
                road_network::ProfiledAccess {
                    access,
                    shaping: shaping.ok_or("Road costs require shaping")?,
                    traversal,
                },
                &costs.costs,
                cancelled,
            );
        }
        let build = if components {
            road_network::build_component_access
        } else {
            road_network::build_profiled_access
        };
        return build(
            layers,
            alpha,
            sockets,
            obstacles,
            road_network::ProfiledAccess {
                access,
                shaping: shaping.ok_or("Road costs require shaping")?,
                traversal,
            },
            cancelled,
        );
    }
    match shaping {
        Some(policy) => road_network::build_shaped_access(
            layers, alpha, sockets, obstacles, access, policy, cancelled,
        ),
        None => road_network::build_access(layers, alpha, sockets, obstacles, access, cancelled),
    }
}
/// Preserve exact source endpoints while removing numerically coincident controls
/// anywhere in a resampled/grid/network join. Run before later families sample
/// road indices; legacy recipes retain their original controls.
fn normalize_coincident_controls(roads: &mut [road_network::Road], tolerance: f32) {
    for road in roads {
        if road.points.len() < 3 {
            continue;
        }
        let same = |a: [f32; 3], b: [f32; 3]| {
            (a[0] - b[0]).hypot(a[2] - b[2]) <= tolerance && (a[1] - b[1]).abs() <= 1e-5
        };
        let mut clean = Vec::with_capacity(road.points.len());
        clean.push(road.points[0]);
        for (i, &point) in road.points.iter().enumerate().skip(1) {
            if same(*clean.last().unwrap(), point) {
                if i + 1 == road.points.len() {
                    *clean.last_mut().unwrap() = point;
                }
            } else {
                clean.push(point);
            }
        }
        if clean.len() >= 2 {
            road.points = clean;
        }
    }
}
pub fn generate(
    project: &Project,
    solution: &Solution,
    catalog: &Catalog,
    source: &Source,
    root: &Path,
    maps: &mut Composed,
    cancelled: impl Fn() -> bool,
) -> Result<Option<road_network::Report>, String> {
    generate_with_ring_costs(
        project, solution, catalog, source, root, maps, None, cancelled,
    )
}
#[allow(clippy::too_many_arguments)]
pub fn generate_with_ring_costs(
    project: &Project,
    solution: &Solution,
    catalog: &Catalog,
    source: &Source,
    root: &Path,
    maps: &mut Composed,
    ring_costs: Option<&papyrus_terrain::road_costmap::Grid>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<road_network::Report>, String> {
    generate_with_source_costs(
        project, solution, catalog, source, root, maps, ring_costs, None, cancelled,
    )
}
#[allow(clippy::too_many_arguments)]
pub fn generate_with_source_costs(
    project: &Project,
    solution: &Solution,
    catalog: &Catalog,
    source: &Source,
    root: &Path,
    maps: &mut Composed,
    ring_costs: Option<&papyrus_terrain::road_costmap::Grid>,
    access_placement: Option<&[u8]>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<road_network::Report>, String> {
    generate_with_preferences(
        project,
        solution,
        catalog,
        source,
        root,
        maps,
        ring_costs,
        access_placement,
        &[],
        cancelled,
    )
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn generate_with_preferences(
    project: &Project,
    solution: &Solution,
    catalog: &Catalog,
    source: &Source,
    root: &Path,
    maps: &mut Composed,
    ring_costs: Option<&papyrus_terrain::road_costmap::Grid>,
    access_placement: Option<&[u8]>,
    preferences: &[crate::road_replanning::Marker],
    cancelled: impl Fn() -> bool,
) -> Result<Option<road_network::Report>, String> {
    project.validate()?;
    let recipe = project
        .roads
        .as_ref()
        .ok_or("Road generation is not enabled in this recipe")?;
    let mut preferred_ring = if preferences.is_empty() {
        None
    } else {
        ring_costs.cloned()
    };
    let ring_preferences = if let Some(grid) = preferred_ring.as_mut() {
        let policy = recipe
            .ring_costmap
            .as_ref()
            .ok_or("Replanning requires source ring costs")?;
        let Some(applied) = crate::road_replanning::apply_for(
            grid,
            project.size,
            preferences,
            policy.policy.expensive_cost,
            8_000_000,
            Some(0),
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        Some(applied)
    } else {
        None
    };
    let ring_costs = preferred_ring.as_ref().or(ring_costs);
    if recipe.ring_costmap.is_some() != ring_costs.is_some() {
        return Err("Requested source ring cost map is missing or unexpected".into());
    }
    if recipe.access_costmap.is_some() != access_placement.is_some() {
        return Err("Requested access source placement raster is missing or unexpected".into());
    }
    if let Some(profile) = &recipe.access_costmap {
        profile.check(source)?;
    }
    if source.sha256 != recipe.source_sha256
        || source.catalog_sha256 != catalog.sha256
        || project.installation.as_ref() != Some(&catalog.installation)
        || maps.layers.size != project.size
        || maps.layers.terrain_seed != project.seeds[0].value
        || maps.layers.biome_seed != project.seeds[1].value
    {
        return Err("Road generation source or terrain mismatch".into());
    }
    let use_costs = recipe.engine == road_network::COST_VERSION;
    if recipe.post_topology.is_some() && source.profile.post_topology.is_none() {
        return Err("Post-road topology requires a bound source stage".into());
    }
    let include_trails = !use_costs || recipe.include_trails.unwrap_or(true);
    let traversal = if use_costs {
        Some(
            source
                .profile
                .access_routing
                .ok_or("Missing bound road traversal costs")?,
        )
    } else {
        None
    };
    let use_ring = recipe.engine == road_network::RING_VERSION
        || (use_costs && recipe.main_ring.unwrap_or(true));
    if use_ring && source.profile.ring.is_none() {
        return Err("Main road ring requires a captured ring policy".into());
    }
    let shaped = recipe.engine == road_network::SHAPED_VERSION || use_ring || use_costs;
    if shaped && source.profile.shaping.is_none() {
        return Err("Shaped road recipe requires a bound shaping policy".into());
    }
    let shaping = shaped.then_some(source.profile.shaping).flatten();
    let access = recipe.engine == road_network::ACCESS_VERSION || shaped;
    if access && source.profile.entrances.is_none() {
        return Err("Road entrance recipe requires a bound entrance policy".into());
    }
    let multi = recipe.engine == road_network::FAMILY_VERSION || access;
    if multi {
        if source.profile.trails.is_none() {
            return Err("Trail recipe requires a bound trail source".into());
        }
        source.verify_installation(root, &cancelled)?;
    }
    let mut sockets = Vec::new();
    let mut trail_sockets = Vec::new();
    let mut obstacles = Vec::new();
    let mut owners = Vec::new();
    let mut missing_metadata = Vec::new();
    for outcome in &solution.outcomes {
        if cancelled() {
            return Ok(None);
        }
        let Some(position) = &outcome.position else {
            continue;
        };
        let target = project
            .targets
            .iter()
            .find(|t| t.instance_id == outcome.id)
            .ok_or("Unknown road layout target")?;
        let path = target
            .prefab_path
            .as_ref()
            .ok_or("Road target lacks prefab identity")?;
        let asset = catalog
            .entries
            .get(path)
            .ok_or("Road target lacks verified metadata")?;
        if let Some(b) = &asset.bounds {
            obstacles.push(road_network::Obstacle {
                monument: outcome.id,
                center: [position.x, position.z],
                extent: b.horizontal_extent(),
                yaw: position.yaw,
            });
        }
        if access {
            if let Ok(([_, bounds], _)) =
                crate::placement_preview::bounds_for_recipe(project, asset)
            {
                let (s, c) = position.yaw.to_radians().sin_cos();
                owners.push(road_network::Obstacle {
                    monument: outcome.id,
                    center: [
                        position.x + c * bounds.center[0] - s * bounds.center[1],
                        position.z + s * bounds.center[0] + c * bounds.center[1],
                    ],
                    extent: bounds.extent,
                    yaw: position.yaw,
                });
            } else if asset.bounds.is_some() {
                missing_metadata.push(outcome.id);
            }
        }
        let Some(prefab) = source.prefabs.get(path).filter(|p| p.complete) else {
            missing_metadata.push(outcome.id);
            continue;
        };
        let (s, c) = position.yaw.to_radians().sin_cos();
        for socket in prefab.connectors.iter().filter(|s| {
            (s.infrastructure_type == source.profile.road_type
                || (multi
                    && include_trails
                    && source
                        .profile
                        .trails
                        .as_ref()
                        .is_some_and(|t| t.infrastructure_type == s.infrastructure_type)))
                && (multi || s.hierarchy_active_without_root)
        }) {
            let [x, y, z] = socket.position;
            let collection = if socket.infrastructure_type == source.profile.road_type {
                &mut sockets
            } else {
                &mut trail_sockets
            };
            collection.push(road_network::Socket {
                monument: outcome.id,
                connector: socket.object_id,
                position: [
                    position.x + c * x - s * z,
                    position.y + y,
                    position.z + s * x + c * z,
                ],
            });
        }
    }
    let p = &source.profile;
    let settings = road_network::Settings {
        seed: project.seeds[3].value,
        width: p.width,
        shoulder: p.shoulder,
        maximum_grade: recipe.maximum_grade,
        maximum_cut_fill: recipe.maximum_cut_fill,
        terrain_offset: p.terrain_offset,
        road_topology: p.road_topology,
        roadside_topology: p.roadside_topology,
        protected_topology: p.protected_topology,
        splat_channel: p.splat_channel,
    };
    let water_policy = recipe
        .ring_costmap
        .as_ref()
        .and_then(|p| p.joins.as_ref())
        .and_then(|j| j.river_water.as_ref());
    let water_grid = if water_policy.is_some() || recipe.access_river_water.is_some() {
        if let Some(rivers) = &maps.rivers {
            let Some(raw) = rivers.collider_heights(
                project.size,
                maps.layers.surface_resolution,
                &cancelled,
            )?
            else {
                return Ok(None);
            };
            Some(raw.heights)
        } else {
            if project.rivers.is_some() {
                return Err("Road river policy lost its generated water world".into());
            }
            None
        }
    } else {
        None
    };
    let water = water_policy
        .zip(water_grid.as_deref())
        .map(|(policy, heights)| papyrus_terrain::road_water::Guide { policy, heights });
    let access_water = recipe
        .access_river_water
        .as_ref()
        .zip(water_grid.as_deref())
        .map(|(policy, heights)| papyrus_terrain::road_water::Guide { policy, heights });
    let original_road_ground = recipe
        .entrance_launch
        .filter(|p| p.engine == road_network::LaunchEngine::GlobalEarthwork)
        .map(|_| maps.layers.height.clone());
    let mut private = multi.then(|| maps.layers.clone());
    let layers = private.as_mut().unwrap_or(&mut maps.layers);
    let ring = if use_ring {
        let build_ring = |layers: &mut papyrus_terrain::native_layers::Layers| {
            let traversal = if recipe.ring_search.is_some() {
                Some((
                    p.access_routing.ok_or("Missing ring cost policy")?,
                    p.entrances
                        .ok_or("Missing ownership policy")?
                        .owned_topology,
                ))
            } else {
                None
            };
            if let Some(costs) = ring_costs {
                recipe
                    .ring_costmap
                    .as_ref()
                    .ok_or("Missing cost profile")?
                    .check(source)?;
                if costs.engine != papyrus_terrain::road_costmap::VERSION
                    || costs.resolution != 257
                    || costs.seed != settings.seed
                    || costs.trail
                {
                    return Err("Source ring cost request differs from the road recipe".into());
                }
            }
            if original_road_ground.is_some() {
                road_network::build_ring_with_global_earthwork(
                    layers,
                    &maps.alpha,
                    &obstacles,
                    settings,
                    p.ring.ok_or("Missing ring policy")?,
                    p.shaping.ok_or("Missing ring shaping")?,
                    traversal,
                    ring_costs.map(|costs| costs.costs.as_slice()),
                    recipe.ring_costmap.as_ref().and_then(|p| p.joins.as_ref()),
                    water,
                    recipe
                        .entrance_launch
                        .ok_or("Missing global launch policy")?,
                    &cancelled,
                )
            } else if let Some(costs) = ring_costs {
                road_network::build_ring_with_river_costs(
                    layers,
                    &maps.alpha,
                    &obstacles,
                    settings,
                    p.ring.ok_or("Missing ring policy")?,
                    p.shaping.ok_or("Missing ring shaping")?,
                    traversal.ok_or("Missing source traversal")?,
                    &costs.costs,
                    recipe.ring_costmap.as_ref().and_then(|p| p.joins.as_ref()),
                    water,
                    &cancelled,
                )
            } else {
                road_network::build_ring_with_recovery(
                    layers,
                    &maps.alpha,
                    &obstacles,
                    settings,
                    p.ring.ok_or("Missing ring policy")?,
                    p.shaping.ok_or("Missing ring shaping")?,
                    traversal,
                    &cancelled,
                )
            }
        };
        let Some(ring) = build_ring(layers)? else {
            return Ok(None);
        };
        Some(ring)
    } else {
        None
    };
    let ring_count = ring.as_ref().map_or(0, |r| r.roads.len());
    let access_reservations = if let Some(policy) = recipe
        .access_costmap
        .as_ref()
        .and_then(|p| p.path_reservations.as_ref())
    {
        let Some(output) = papyrus_terrain::road_reservations::generate(
            layers.size as f32,
            layers.surface_resolution,
            access_placement.unwrap(),
            (
                ring.as_ref().map_or(&[][..], |r| r.roads.as_slice()),
                ring_count,
            ),
            policy,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        Some(output)
    } else {
        None
    };
    let mut access_costs = if let Some(profile) = &recipe.access_costmap {
        let Some(grid) = profile.build_family(
            layers,
            access_reservations
                .as_ref()
                .map_or_else(|| access_placement.unwrap(), |r| r.raster.as_slice()),
            project.seeds[3].value,
            false,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        Some(grid)
    } else {
        None
    };
    let access_preferences = if !preferences.is_empty()
        && let Some(grid) = access_costs.as_mut()
    {
        let profile = recipe.access_costmap.as_ref().unwrap();
        let Some(applied) = crate::road_replanning::apply_for(
            grid,
            project.size,
            preferences,
            profile.policy.expensive_cost,
            8_000_000,
            Some(1),
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        Some(applied)
    } else {
        None
    };
    let Some(mut report) = (if access {
        source_access(
            layers,
            &maps.alpha,
            &sockets,
            &obstacles,
            road_network::Access {
                settings,
                policy: p.entrances.ok_or("Missing entrance policy")?,
                owner_footprints: &owners,
                network: ring
                    .as_ref()
                    .filter(|r| !r.roads.is_empty())
                    .map(|r| (&r.roads[..], p.ring.unwrap().network_sample_stride)),
            },
            shaping,
            traversal,
            recipe.access_search.is_some(),
            access_costs.as_ref(),
            recipe.access_costmap.is_some(),
            access_water,
            recipe.entrance_launch,
            original_road_ground.as_deref(),
            &cancelled,
        )?
    } else {
        road_network::build(
            layers,
            &maps.alpha,
            &sockets,
            &obstacles,
            settings,
            &cancelled,
        )?
    }) else {
        return Ok(None);
    };
    if use_ring || use_costs {
        normalize_coincident_controls(
            &mut report.roads,
            if recipe.access_costmap.is_some() || recipe.entrance_launch.is_some() {
                1e-4
            } else {
                0.
            },
        );
    }
    if use_costs {
        report.trails_enabled = Some(include_trails);
    }
    if let Some(mut ring) = ring {
        ring.roads.append(&mut report.roads);
        report.roads = ring.roads;
        if let Some(audit) = ring.shaping.constrained {
            report.shaping.work += audit.work;
            report
                .shaping
                .constrained
                .get_or_insert_with(Default::default)
                .merge(&audit);
        }
        report.changed_heights += ring.changed_heights;
        report.attempts += ring.attempts;
        report.budget_exhausted |= ring.budget_exhausted;
        report.river_water = ring.river_water;
        report.ring = ring.ring;
        if let Some(costs) = ring_costs {
            report.ring_costmap = Some(
                serde_json::json!({"grid":costs,"profile":recipe.ring_costmap,
                "sampling":"endpoint routing grid; source cost rules; independent noise and router",
                "placement_scope":"verified monument blockers; earlier server path reservations incomplete",
                "complete_source_costmap_parity":false,"applies_to":"main ring only"}),
            );
        }
    }
    if multi {
        let t = source
            .profile
            .trails
            .as_ref()
            .ok_or("Missing trail policy")?;
        let trail_seed = trail_seed(project.seeds[3].value, &t.seed_domain);
        let trail_settings = road_network::Settings {
            seed: trail_seed,
            width: t.width,
            shoulder: t.shoulder,
            terrain_offset: t.terrain_offset,
            splat_channel: t.splat_channel,
            ..settings
        };
        let trail_reservations = if include_trails
            && let Some(policy) = recipe
                .access_costmap
                .as_ref()
                .and_then(|p| p.path_reservations.as_ref())
        {
            let Some(output) = papyrus_terrain::road_reservations::generate(
                layers.size as f32,
                layers.surface_resolution,
                access_placement.unwrap(),
                (&report.roads, ring_count),
                policy,
                &cancelled,
            )?
            else {
                return Ok(None);
            };
            Some(output)
        } else {
            None
        };
        let mut trail_costs = if include_trails && let Some(profile) = &recipe.access_costmap {
            let Some(grid) = profile.build_family(
                layers,
                trail_reservations
                    .as_ref()
                    .map_or_else(|| access_placement.unwrap(), |r| r.raster.as_slice()),
                trail_seed,
                true,
                &cancelled,
            )?
            else {
                return Ok(None);
            };
            Some(grid)
        } else {
            None
        };
        let trail_preferences = if !preferences.is_empty()
            && let Some(grid) = trail_costs.as_mut()
        {
            let profile = recipe.access_costmap.as_ref().unwrap();
            let Some(applied) = crate::road_replanning::apply_for(
                grid,
                project.size,
                preferences,
                profile.policy.expensive_cost,
                8_000_000,
                Some(2),
                &cancelled,
            )?
            else {
                return Ok(None);
            };
            Some(applied)
        } else {
            None
        };
        let trail = if !include_trails {
            Some(road_network::Report::default())
        } else if access {
            source_access(
                layers,
                &maps.alpha,
                &trail_sockets,
                &obstacles,
                road_network::Access {
                    settings: trail_settings,
                    policy: p.entrances.ok_or("Missing entrance policy")?,
                    owner_footprints: &owners,
                    network: (!report.roads.is_empty())
                        .then_some((&report.roads[..], t.network_sample_stride)),
                },
                shaping,
                traversal,
                recipe.access_search.is_some(),
                trail_costs.as_ref(),
                recipe.access_costmap.is_some(),
                None, // Source dirt-trail costs explicitly forbid river crossings.
                recipe.entrance_launch,
                original_road_ground.as_deref(),
                &cancelled,
            )?
        } else if report.roads.is_empty() {
            road_network::build(
                layers,
                &maps.alpha,
                &trail_sockets,
                &obstacles,
                trail_settings,
                &cancelled,
            )?
        } else {
            road_network::build_with_network(
                layers,
                &maps.alpha,
                &trail_sockets,
                &obstacles,
                trail_settings,
                (&report.roads, t.network_sample_stride),
                &cancelled,
            )?
        };
        let Some(mut trail) = trail else {
            return Ok(None);
        };
        if use_ring || use_costs {
            normalize_coincident_controls(
                &mut trail.roads,
                if recipe.access_costmap.is_some() || recipe.entrance_launch.is_some() {
                    1e-4
                } else {
                    0.
                },
            );
        }
        if report.roads.is_empty() && trail.roads.is_empty() {
            trail.unresolved = trail_sockets
                .iter()
                .map(|s| s.monument)
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
        }
        let family = |kind,
                      hierarchy,
                      settings: road_network::Settings,
                      first_road,
                      r: &road_network::Report| road_network::Family {
            infrastructure_type: kind,
            hierarchy,
            seed: settings.seed,
            first_road,
            road_count: r.roads.len(),
            socket_count: r.socket_count,
            unresolved: r.unresolved.clone(),
            width: settings.width,
            splat_channel: settings.splat_channel,
        };
        report.families = vec![
            family(
                p.road_type,
                p.road_hierarchy.ok_or("Missing road hierarchy")?,
                settings,
                ring_count,
                &report,
            ),
            family(
                t.infrastructure_type,
                t.hierarchy,
                trail_settings,
                report.roads.len(),
                &trail,
            ),
        ];
        report.families[0].road_count -= ring_count;
        if use_ring {
            let policy = p.ring.unwrap();
            report.families.insert(
                0,
                road_network::Family {
                    infrastructure_type: p.road_type,
                    hierarchy: policy.hierarchy,
                    seed: settings.seed,
                    first_road: 0,
                    road_count: ring_count,
                    socket_count: 0,
                    unresolved: vec![],
                    width: policy.width,
                    splat_channel: settings.splat_channel,
                },
            );
        }
        // Attachment references are into the paved prefix, which stays in place.
        report.roads.append(&mut trail.roads);
        report.shaping.attempted += trail.shaping.attempted;
        report.shaping.accepted += trail.shaping.accepted;
        report.shaping.fallbacks += trail.shaping.fallbacks;
        report.shaping.budget_fallbacks += trail.shaping.budget_fallbacks;
        report.shaping.work += trail.shaping.work;
        if let Some(audit) = trail.shaping.entrance_recovery {
            let combined = report
                .shaping
                .entrance_recovery
                .get_or_insert_with(Default::default);
            combined.attempts += audit.attempts;
            combined.proposals += audit.proposals;
            combined.accepted += audit.accepted;
            if let Some(count) = audit.saved_grade_proposals {
                *combined.saved_grade_proposals.get_or_insert(0) += count;
            }
        }
        if let Some(audit) = trail.shaping.constrained {
            report
                .shaping
                .constrained
                .get_or_insert_with(Default::default)
                .merge(&audit);
        }
        for (reason, count) in trail.shaping.fallback_reasons {
            *report.shaping.fallback_reasons.entry(reason).or_default() += count;
        }
        report.entrance_joins.append(&mut trail.entrance_joins);
        report
            .rejection_samples
            .append(&mut trail.rejection_samples);
        for (reason, count) in trail.rejection_reasons {
            *report.rejection_reasons.entry(reason).or_default() += count;
        }
        report.attempts += trail.attempts;
        report.rejected += trail.rejected;
        report.changed_heights += trail.changed_heights;
        report.budget_exhausted |= trail.budget_exhausted;
        report.unresolved.append(&mut trail.unresolved);
        report.unresolved.sort_unstable();
        report.unresolved.dedup();
        if let Some(costs) = trail_costs {
            let mut family = serde_json::json!({"family":"dirt","grid":costs});
            crate::road_replanning::attach(&mut family, trail_preferences);
            if let Some(reservations) = &trail_reservations {
                family["path_reservations"] = serde_json::json!({"report":reservations.report,"raster_sha256":format!("{:x}",Sha256::digest(&reservations.raster)),"scope":"initial monument blockers plus all preceding paved paths"});
            }
            report.source_access_costmaps.push(family);
        }
    }
    if let Some(costs) = access_costs {
        let mut family = serde_json::json!({"family":"paved","grid":costs,"profile":recipe.access_costmap,
        "sampling":"endpoint routing grid; source costs; independent noise and routing","placement_scope":"verified initial monument blockers; server path reservations incomplete","complete_server_parity":false});
        crate::road_replanning::attach(&mut family, access_preferences);
        if let Some(reservations) = &access_reservations {
            family["path_reservations"] = serde_json::json!({"report":reservations.report,"raster_sha256":format!("{:x}",Sha256::digest(&reservations.raster)),"scope":"initial monument blockers plus preceding main-road paths"});
            family["placement_scope"] =
                "verified initial monument blockers and preceding main-road reservations".into();
        }
        report.source_access_costmaps.insert(0, family);
    }
    if let Some(ring) = report.ring_costmap.as_mut() {
        crate::road_replanning::attach(ring, ring_preferences);
    }
    if recipe.post_topology.is_some() {
        let policy = source
            .profile
            .post_topology
            .ok_or("Missing post-road topology policy")?;
        let Some((topology, summary)) =
            papyrus_terrain::road_topology::apply(layers, policy, &cancelled)?
        else {
            return Ok(None);
        };
        layers.topology = topology;
        report.post_topology = Some(summary);
    }
    let rebuilt_ocean = if maps.ocean.is_some() {
        let Some(ocean) = papyrus_terrain::ocean::reconstruct(
            &layers.height,
            layers.height_resolution,
            &layers.topology,
            layers.surface_resolution,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        Some(ocean)
    } else {
        None
    };
    if cancelled() {
        return Ok(None);
    }
    if let Some(ocean) = &rebuilt_ocean {
        layers.topology.clone_from(&ocean.topology);
    }
    if (use_ring || use_costs)
        && let Some(next) = &private
        && maps.layers.height.iter().zip(&next.height).any(|(&a, &b)| {
            (papyrus_terrain::erosion::decode(a) - papyrus_terrain::erosion::decode(b)).abs()
                > recipe.maximum_cut_fill + 1000. / 32766.
        })
    {
        return Err(
            "Combined main road and access/trail earthwork exceeds the saved cut/fill allowance"
                .into(),
        );
    }
    if let Some(private) = private {
        maps.layers = private;
    }
    if let Some(ocean) = rebuilt_ocean {
        maps.ocean = Some(ocean);
    }
    report.missing_metadata = missing_metadata;
    report.socket_count = sockets.len() + trail_sockets.len();
    Ok(Some(report))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    #[test]
    fn global_earthwork_selection_is_atomic_preserves_limits_and_keeps_legacy_explicit() {
        let mut recipe = Recipe::new("a".repeat(64));
        let legacy = recipe.clone();
        assert!(recipe.set_global_earthwork(true).is_err());
        assert_eq!(recipe, legacy);
        recipe.engine = road_network::COST_VERSION.into();
        recipe.surface_sha256 = Some("b".repeat(64));
        recipe.staged = Some(crate::road_generation::Policy {
            layout: None,
            engine: crate::road_generation::VERSION.into(),
            roadside_source_sha256: "c".repeat(64),
            roadside_preferences: Vec::new(),
        });
        recipe.maximum_grade = 0.17;
        recipe.maximum_cut_fill = 3.25;
        recipe.include_trails = Some(false);
        recipe.set_global_earthwork(true).unwrap();
        assert_eq!(
            recipe.entrance_launch.unwrap().engine,
            road_network::LaunchEngine::GlobalEarthwork
        );
        assert_eq!(
            recipe.access_search.as_deref(),
            Some(road_network::COMPONENT_VERSION)
        );
        assert!(recipe.access_costmap.is_none() && recipe.access_smoothing.is_none());
        let launch = recipe.entrance_launch.as_mut().unwrap();
        launch.minimum_run_widths = 1.5;
        launch.sample_spacing = 0.5;
        launch.maximum_work = 3_000_000;
        let original = recipe.clone();
        recipe.set_global_earthwork(false).unwrap();
        assert_eq!(
            recipe.entrance_launch.unwrap(),
            road_network::LaunchPolicy {
                engine: road_network::LaunchEngine::SurfaceChecked,
                ..original.entrance_launch.unwrap()
            }
        );
        recipe.set_global_earthwork(true).unwrap();
        assert_eq!(recipe, original);
        let restored: Recipe =
            serde_json::from_value(serde_json::to_value(&recipe).unwrap()).unwrap();
        assert_eq!(restored, recipe);
        recipe.staged = None;
        let invalid = recipe.clone();
        assert!(recipe.set_global_earthwork(true).is_err());
        assert_eq!(recipe, invalid);
    }
    #[test]
    fn checked_launch_recipe_is_explicit_versioned_and_roundtrips_without_changing_legacy_recipes()
    {
        let legacy = Recipe::new("a".repeat(64));
        assert!(
            serde_json::to_value(&legacy)
                .unwrap()
                .get("entrance_launch")
                .is_none()
        );
        let mut checked = legacy.clone();
        checked.engine = road_network::COST_VERSION.into();
        checked.access_search = Some(road_network::COMPONENT_VERSION.into());
        checked.entrance_launch = Some(road_network::LaunchPolicy {
            engine: road_network::LaunchEngine::Checked,
            minimum_run_widths: 1.,
            sample_spacing: 1.,
            maximum_work: 2_000_000,
        });
        checked.validate().unwrap();
        let encoded = serde_json::to_value(&checked).unwrap();
        assert_eq!(
            serde_json::from_value::<Recipe>(encoded.clone()).unwrap(),
            checked
        );
        let mut unknown = encoded;
        unknown["entrance_launch"]["engine"] = "unknown".into();
        assert!(serde_json::from_value::<Recipe>(unknown).is_err());
        checked.access_search = None;
        assert!(checked.validate().is_err());
        assert!(
            serde_json::to_value(&legacy)
                .unwrap()
                .get("entrance_launch")
                .is_none()
        );
    }
    #[test]
    fn near_coincident_resampling_controls_keep_exact_socket_and_network_poses() {
        let first = [1., 20., 2.];
        let end = [100., 21., 90.];
        let mut roads = vec![road_network::Road {
            from: road_network::Destination::Monument {
                monument: 1,
                connector: 2,
            },
            to: road_network::Destination::Network { road: 0, point: 4 },
            points: vec![
                first,
                [1., 20.000002, 2.],
                [40., 20.5, 50.],
                [40., 20.500002, 50.],
                [100., 21.000002, 90.],
                end,
            ],
        }];
        normalize_coincident_controls(&mut roads, 0.);
        assert_eq!(roads[0].points, vec![first, [40., 20.5, 50.], end]);
        assert_eq!(
            roads[0].to,
            road_network::Destination::Network { road: 0, point: 4 }
        );
        let mut vertical = vec![road_network::Road {
            from: road_network::Destination::Loop,
            to: road_network::Destination::Loop,
            points: vec![first, [1., 21., 2.], end],
        }];
        normalize_coincident_controls(&mut vertical, 0.);
        assert_eq!(vertical[0].points.len(), 3);
        let b: [f32; 3] = [46.875, 14.252625, 578.125];
        let a = [f32::from_bits(b[0].to_bits() + 2), b[1], b[2]];
        let mut tiny = vec![road_network::Road {
            from: road_network::Destination::Loop,
            to: road_network::Destination::Loop,
            points: vec![first, a, b, end],
        }];
        normalize_coincident_controls(&mut tiny, 0.);
        assert_eq!(tiny[0].points.len(), 4);
        normalize_coincident_controls(&mut tiny, 1e-4);
        assert_eq!(tiny[0].points, vec![first, a, end]);
        tiny[0].points = vec![first, [end[0] - 0.00002, end[1], end[2]], end];
        normalize_coincident_controls(&mut tiny, 1e-4);
        assert_eq!(tiny[0].points, vec![first, end]);
    }
    #[test]
    fn river_access_is_explicit_source_bound_and_preserves_legacy_recipe_bytes() {
        let mut recipe = Recipe::new("a".repeat(64));
        assert!(
            serde_json::to_value(&recipe)
                .unwrap()
                .get("access_river_water")
                .is_none()
        );
        let profile: crate::road_cost_source::Profile = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-road-rivers.json"
        ))
        .unwrap();
        let water = profile.joins.as_ref().unwrap().river_water.clone().unwrap();
        recipe.access_river_water = Some(water.clone());
        assert!(recipe.validate().is_err());
        recipe.source_sha256 = profile.infrastructure_sha256.clone();
        recipe.engine = road_network::COST_VERSION.into();
        recipe.access_search = Some(road_network::COMPONENT_VERSION.into());
        recipe.access_costmap = Some(profile);
        recipe.validate().unwrap();
        assert_eq!(
            serde_json::from_slice::<Recipe>(&serde_json::to_vec(&recipe).unwrap()).unwrap(),
            recipe
        );
        recipe.access_river_water.as_mut().unwrap().clearance = 3.;
        assert!(recipe.validate().is_err());
        recipe.access_river_water = Some(water);
        recipe
            .access_costmap
            .as_mut()
            .unwrap()
            .policy
            .water_topology = 16;
        assert!(recipe.validate().is_err());
    }
    #[test]
    fn constrained_shaping_requires_source_costs_and_preserves_legacy_serialization() {
        let mut recipe = Recipe::new("a".repeat(64));
        assert!(
            serde_json::to_value(&recipe)
                .unwrap()
                .get("access_smoothing")
                .is_none()
        );
        recipe.access_smoothing = Some(papyrus_terrain::road_smoothing::VERSION.into());
        assert!(recipe.validate().is_err());
        let profile: crate::road_cost_source::Profile = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-road-joins.json"
        ))
        .unwrap();
        recipe.source_sha256 = profile.infrastructure_sha256.clone();
        recipe.engine = road_network::COST_VERSION.into();
        recipe.access_search = Some(road_network::COMPONENT_VERSION.into());
        recipe.access_costmap = Some(profile);
        recipe.validate().unwrap();
        assert_eq!(
            serde_json::from_value::<Recipe>(serde_json::to_value(&recipe).unwrap()).unwrap(),
            recipe
        );
        recipe.access_smoothing = Some("unknown-policy".into());
        assert!(recipe.validate().is_err());
        recipe.access_smoothing = Some(papyrus_terrain::road_smoothing::VERSION.into());
        recipe.access_search = None;
        assert!(recipe.validate().is_err());
    }
    #[test]
    fn road_surfaces_round_trip_without_changing_terrain_or_legacy_recipes() {
        let mut project = Project::default();
        let terrain = project.terrain_key();
        let mut recipe = Recipe::new("a".repeat(64));
        let legacy = serde_json::to_value(&recipe).unwrap();
        assert!(legacy.get("surface_sha256").is_none());
        assert_eq!(serde_json::from_value::<Recipe>(legacy).unwrap(), recipe);
        recipe.surface_sha256 = Some("b".repeat(64));
        assert!(recipe.validate().is_err());
        recipe.engine = road_network::SHAPED_VERSION.into();
        recipe.validate().unwrap();
        project.roads = Some(recipe.clone());
        let loaded: Project =
            serde_json::from_slice(&serde_json::to_vec(&project).unwrap()).unwrap();
        assert_eq!(loaded.roads, Some(recipe));
        assert_eq!(loaded.terrain_key(), terrain);
        let mut bad = loaded.roads.unwrap();
        bad.surface_sha256 = Some("../source".into());
        assert!(bad.validate().is_err());
    }
    use crate::{
        installation::{Installation, InstallationKind},
        prefab_assets::Asset,
    };
    pub(crate) fn fixture() -> (Catalog, serde_json::Value) {
        let source = |path: &str, c: char| SourceFile {
            relative_path: path.into(),
            sha256: c.to_string().repeat(64),
            bytes: 1,
        };
        let sources = vec![
            source("RustClient_Data/Plugins/x86_64/RustNative.dll", 'a'),
            source("Bundles/AssetSceneManifest.json", 'b'),
            source("Bundles/shared/assetscenes.bundle", 'c'),
        ];
        let path = "assets/bundled/prefabs/autospawn/monument/test.prefab";
        let catalog = Catalog {
            sha256: "d".repeat(64),
            installation: Installation {
                kind: InstallationKind::Client,
                native: sources[0].clone(),
                scene_manifest: Some(sources[1].clone()),
            },
            sources: sources.clone(),
            entries: std::collections::HashMap::from([(
                path.into(),
                Asset {
                    priority: None,
                    bounds: None,
                    yaw: None,
                    modifier_size: None,
                    modifier_offset: None,
                    texture_guids: Default::default(),
                    splat_mask: None,
                    modifier: None,
                    modifier_absent: false,
                    water_geometry: None,
                    no_build_geometry: None,
                    display_name: None,
                },
            )]),
        };
        let doc = serde_json::json!({"format":"papyrus-infrastructure-source","schema":1,"adapter":"papyrus-path-connectors-1","catalog_sha256":catalog.sha256,"sources":sources,"extractor_sha256":"e".repeat(64),
            "profile":{"format":"papyrus-infrastructure-profile","schema":1,"id":"synthetic","native_sha256":"a".repeat(64),"scene_sha256":"c".repeat(64),"road_type":0,"road_topology":2048,"roadside_topology":4096,"protected_topology":1024,"splat_channel":7,"width":10.,"shoulder":8.,"terrain_offset":-0.125},
            "prefabs":{path:{"version":1,"space":"prefab-root-local-unity","complete":true,"connectors":[{"objectId":12,"infrastructureType":0,"position":[1.,2.,3.],"forward":[0.,0.,1.],"hierarchyActiveWithoutRoot":true}],"nodesVisited":2,"issues":[]}}});
        (catalog, doc)
    }
    #[test]
    fn composition_scope_shares_only_its_verified_infrastructure_snapshot() {
        let (mut catalog, doc) = fixture();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("infrastructure.json");
        let raw = serde_json::to_vec(&doc).unwrap();
        std::fs::write(&path, &raw).unwrap();
        let recipe = Recipe::new(format!("{:x}", Sha256::digest(&raw)));
        let scope = Scope::new();
        let first = Source::share_cached(&catalog, &recipe, &|| false, || {
            Source::load(&path, &catalog, || false)
        }).unwrap();
        std::fs::write(&path, b"changed").unwrap();
        {
            let _nested = Scope::new();
            let next = Source::share_cached(&catalog, &recipe, &|| false, || {
                panic!("A second composition stage reopened the snapshot")
            }).unwrap();
            assert!(Arc::ptr_eq(&first, &next));
        }
        let mut invalid = recipe.clone();
        invalid.maximum_grade = -1.;
        assert!(Source::share_cached(&catalog, &invalid, &|| false, || {
            panic!("Recipe validation must still run on a hit")
        }).is_err());
        assert!(Source::share_cached(&catalog, &recipe, &|| true, || {
            panic!("Cancellation must be checked before loading")
        }).is_err());

        let another_source = Recipe::new("a".repeat(64));
        assert!(Source::share_cached(&catalog, &another_source, &|| false, || {
            Source::load(&path, &catalog, || false)
        }).is_err());
        catalog.sha256 = "f".repeat(64);
        assert!(Source::share_cached(&catalog, &recipe, &|| false, || {
            Source::load(&path, &catalog, || false)
        }).is_err());
        catalog.sha256 = doc["catalog_sha256"].as_str().unwrap().into();

        drop(scope);
        let _next_operation = Scope::new();
        assert!(Source::share_cached(&catalog, &recipe, &|| false, || {
            Source::load(&path, &catalog, || false)
        }).is_err());
    }

    #[test]
    fn infrastructure_cache_publishes_closed_files_and_reuses_only_matching_content() {
        let root = tempfile::tempdir().unwrap();
        let (catalog, doc) = fixture();
        let path = root.path().join("input.json");
        let raw = serde_json::to_vec(&doc).unwrap();
        std::fs::write(&path, &raw).unwrap();
        let cache = root.path().join("cache");
        let first = Source::cache_in(&path, &catalog, &cache, || false).unwrap();
        let published = cache.join(&first.sha256).join("infrastructure.json");
        assert_eq!(std::fs::read(&published).unwrap(), raw);
        assert_eq!(
            Source::cache_in(&path, &catalog, &cache, || false)
                .unwrap()
                .sha256,
            first.sha256
        );
        std::fs::write(&published, b"changed").unwrap();
        assert!(Source::cache_in(&path, &catalog, &cache, || false).is_err());
        assert_eq!(std::fs::read(&published).unwrap(), b"changed");
        let cancelled = root.path().join("cancelled");
        let ticks = std::cell::Cell::new(0);
        assert!(
            Source::cache_in(&path, &catalog, &cancelled, || {
                ticks.set(ticks.get() + 1);
                ticks.get() == 3
            })
            .is_err()
        );
        assert!(!cancelled.join(&first.sha256).exists());
        assert_eq!(std::fs::read_dir(&cancelled).unwrap().count(), 0);
    }
    #[test]
    fn simultaneous_infrastructure_imports_share_one_verified_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let (catalog, doc) = fixture();
        let path = root.path().join("input.json");
        let raw = serde_json::to_vec(&doc).unwrap();
        std::fs::write(&path, &raw).unwrap();
        let cache = root.path().join("cache");
        let barrier = std::sync::Barrier::new(2);
        let run = || {
            let ticks = std::cell::Cell::new(0);
            Source::cache_in(&path, &catalog, &cache, || {
                ticks.set(ticks.get() + 1);
                if ticks.get() == 3 {
                    // Both imports have flushed and closed their staged file.
                    barrier.wait();
                }
                false
            })
            .unwrap()
            .sha256
        };
        let ids = std::thread::scope(|scope| {
            let a = scope.spawn(run);
            let b = scope.spawn(run);
            [a.join().unwrap(), b.join().unwrap()]
        });
        assert_eq!(ids[0], ids[1]);
        assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 1);
        assert_eq!(
            std::fs::read(cache.join(&ids[0]).join("infrastructure.json")).unwrap(),
            raw
        );
    }
    fn trail_fixture(root: &Path) -> (Catalog, serde_json::Value) {
        let (mut catalog, mut doc) = fixture();
        let prefab = catalog.entries.keys().next().unwrap().clone();
        let trail_path = "assets/bundled/prefabs/autospawn/monument/trail.prefab";
        catalog
            .entries
            .insert(trail_path.into(), catalog.entries[&prefab].clone());
        let mut trail = doc["prefabs"][&prefab].clone();
        trail["connectors"][0]["infrastructureType"] = 2.into();
        trail["connectors"][0]["position"] = serde_json::json!([0., 0., 0.]);
        trail["connectors"][0]["hierarchyActiveWithoutRoot"] = false.into();
        doc["prefabs"][trail_path] = trail;
        doc["prefabs"][&prefab]["connectors"][0]["position"] = serde_json::json!([0., 0., 0.]);
        let mut profile: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-trails.json"
        ))
        .unwrap();
        profile["native_sha256"] = "a".repeat(64).into();
        profile["scene_sha256"] = "c".repeat(64).into();
        std::fs::create_dir_all(root.join("Bundles/maps")).unwrap();
        std::fs::write(root.join("GameAssembly.dll"), b"code").unwrap();
        std::fs::write(root.join("Bundles/maps/maps.bundle"), b"scene").unwrap();
        profile["code_sources"] = serde_json::json!([crate::installation::hash_file(
            root,
            "GameAssembly.dll",
            1024,
            &|| false
        )
        .unwrap()]);
        profile["map_scene"] = serde_json::to_value(
            crate::installation::hash_file(root, "Bundles/maps/maps.bundle", 1024, &|| false)
                .unwrap(),
        )
        .unwrap();
        doc["profile"] = profile;
        doc["layout_stages"] = serde_json::json!([
            {"object_id":1,"order":0,"infrastructure_type":0,"mode":-1,"disabled":false,"component_enabled":true,"hierarchy_active":true},
            {"object_id":2,"order":1,"infrastructure_type":2,"mode":-1,"disabled":false,"component_enabled":true,"hierarchy_active":true}]);
        (catalog, doc)
    }
    #[test]
    fn current_road_stage_activation_requires_versioned_explicit_absence() {
        let root = tempfile::tempdir().unwrap();
        let (catalog, mut legacy) = trail_fixture(root.path());
        let profile: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-road-topology.json"
        ))
        .unwrap();
        for name in [
            "ring",
            "shaping",
            "entrances",
            "access_routing",
            "post_topology",
        ] {
            legacy["profile"][name] = profile[name].clone();
        }
        legacy["profile"]["revision"] = 8.into();
        legacy["ring_stage"] = serde_json::json!({"object_id":408,"order":0,"mode":-1,"disabled":false,"component_enabled":true,"hierarchy_active":true,"minimum_world_size":4000});
        legacy["topology_stage"] = serde_json::json!({"object_id":509,"order":3,"mode":-1,"disabled":false,"component_enabled":true,"hierarchy_active":true});
        let path = root.path().join("source.json");
        let check = |doc: &serde_json::Value| {
            std::fs::write(&path, serde_json::to_vec(doc).unwrap()).unwrap();
            Source::load(&path, &catalog, || false)
        };
        let original = check(&legacy).unwrap();
        assert!(
            serde_json::to_value(&original).unwrap()["profile"]
                .get("stage_layout")
                .is_none()
        );
        let mut current = legacy.clone();
        current["schema"] = 2.into();
        current["adapter"] = "papyrus-path-connectors-2".into();
        current["profile"]["revision"] = 9.into();
        current["profile"]["stage_layout"] = "mode-description".into();
        for pointer in [
            "/ring_stage/disabled",
            "/topology_stage/disabled",
            "/layout_stages/0/disabled",
            "/layout_stages/1/disabled",
        ] {
            *current.pointer_mut(pointer).unwrap() = serde_json::Value::Null;
        }
        let source = check(&current).unwrap();
        assert_ne!(
            source.semantic_sha256().unwrap(),
            original.semantic_sha256().unwrap()
        );
        for pointer in [
            "/ring_stage/disabled",
            "/topology_stage/disabled",
            "/layout_stages/0/disabled",
            "/layout_stages/1/disabled",
        ] {
            for value in [serde_json::json!(false), serde_json::json!(true)] {
                let mut bad = current.clone();
                *bad.pointer_mut(pointer).unwrap() = value;
                assert!(check(&bad).is_err(), "{pointer}");
            }
            let mut bad = legacy.clone();
            *bad.pointer_mut(pointer).unwrap() = serde_json::Value::Null;
            assert!(check(&bad).is_err(), "{pointer}");
        }
        for pointer in ["/schema", "/profile/revision"] {
            let mut bad = current.clone();
            *bad.pointer_mut(pointer).unwrap() = serde_json::json!(1);
            assert!(check(&bad).is_err(), "{pointer}");
        }
        for name in ["ring_stage", "topology_stage"] {
            for field in ["disabled", "component_enabled", "hierarchy_active"] {
                let mut bad = current.clone();
                bad[name].as_object_mut().unwrap().remove(field);
                assert!(check(&bad).is_err(), "{name}/{field}");
            }
        }
        current["profile"]["stage_layout"] = "future".into();
        assert!(check(&current).is_err());
    }
    #[test]
    fn main_ring_requires_its_captured_scene_policy_and_explicit_recipe_revision() {
        let root = tempfile::tempdir().unwrap();
        let (catalog, mut doc) = trail_fixture(root.path());
        let profile: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-ring.json"
        ))
        .unwrap();
        for name in ["ring", "shaping", "entrances"] {
            doc["profile"][name] = profile[name].clone();
        }
        doc["profile"]["revision"] = 6.into();
        doc["ring_stage"] = serde_json::json!({"object_id":408,"order":0,"mode":-1,"disabled":false,"component_enabled":true,"hierarchy_active":true,"minimum_world_size":4000});
        let path = root.path().join("ring.json");
        let check = |doc: &serde_json::Value| {
            std::fs::write(&path, serde_json::to_vec(doc).unwrap()).unwrap();
            Source::load(&path, &catalog, || false)
        };
        let source = check(&doc).unwrap();
        let mut recipe = Recipe::for_source(&source);
        assert_eq!(recipe.engine, road_network::RING_VERSION);
        assert_eq!(
            Recipe::new(source.sha256.clone()).engine,
            road_network::VERSION
        );
        recipe.surface_sha256 = Some("b".repeat(64));
        recipe.validate().unwrap();
        let mut costs = doc.clone();
        costs["profile"]["revision"] = 7.into();
        costs["profile"]["access_routing"] =
            serde_json::json!({"traversable_topology":2,"normal_cost":101,"traversable_cost":5000});
        let cost_source = check(&costs).unwrap();
        let mut cost_recipe = Recipe::for_source(&cost_source);
        assert_eq!(cost_recipe.engine, road_network::COST_VERSION);
        cost_recipe.main_ring = Some(false);
        cost_recipe.include_trails = Some(false);
        cost_recipe.ring_search = Some(papyrus_terrain::road_ring::RECOVERY_VERSION.into());
        cost_recipe.surface_sha256 = Some("b".repeat(64));
        cost_recipe.validate().unwrap();
        let mut bound: crate::road_cost_source::Profile = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-road-costmap.json"
        ))
        .unwrap();
        bound.infrastructure_sha256 = cost_source.sha256.clone();
        bound.sources = cost_source.road_object_receipts().unwrap();
        bound.check(&cost_source).unwrap();
        let mut with_costmap = cost_recipe.clone();
        with_costmap.main_ring = Some(true);
        with_costmap.ring_costmap = Some(bound.clone());
        with_costmap.validate().unwrap();
        let join_profile: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-road-joins.json"
        ))
        .unwrap();
        with_costmap.ring_costmap.as_mut().unwrap().joins =
            Some(serde_json::from_value(join_profile["joins"].clone()).unwrap());
        with_costmap.validate().unwrap();
        assert_eq!(
            with_costmap,
            serde_json::from_slice(&serde_json::to_vec(&with_costmap).unwrap()).unwrap()
        );
        with_costmap.main_ring = Some(false);
        assert!(with_costmap.validate().is_err());
        with_costmap.main_ring = Some(true);
        with_costmap.ring_search = None;
        assert!(with_costmap.validate().is_err());
        bound.sources[0].sha256 = "f".repeat(64);
        assert!(bound.check(&cost_source).is_err());
        let mut invalid_search = cost_recipe.clone();
        invalid_search.ring_search = Some("unknown".into());
        assert!(invalid_search.validate().is_err());
        invalid_search.ring_search = cost_recipe.ring_search.clone();
        invalid_search.engine = road_network::RING_VERSION.into();
        assert!(invalid_search.validate().is_err());
        assert_eq!(
            cost_recipe,
            serde_json::from_value(serde_json::to_value(&cost_recipe).unwrap()).unwrap()
        );
        for (pointer, value) in [
            ("/profile/revision", serde_json::json!(6)),
            (
                "/profile/access_routing/traversable_topology",
                serde_json::json!(128),
            ),
            ("/profile/access_routing/normal_cost", serde_json::json!(0)),
            (
                "/profile/access_routing/traversable_cost",
                serde_json::json!(100),
            ),
        ] {
            let mut bad = costs.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(check(&bad).is_err(), "{pointer}");
        }
        for (pointer, value) in [
            ("/profile/revision", serde_json::json!(5)),
            ("/profile/ring/width", serde_json::json!(0)),
            ("/profile/ring/hierarchy", serde_json::json!(2)),
            ("/profile/ring/maximum_attempts", serde_json::json!(129)),
            ("/ring_stage/hierarchy_active", serde_json::json!(false)),
            ("/ring_stage/order", serde_json::json!(1)),
            ("/ring_stage/minimum_world_size", serde_json::json!(3999)),
            ("/ring_stage", serde_json::Value::Null),
        ] {
            let mut bad = doc.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(check(&bad).is_err(), "{pointer}");
        }
        let mut bad = doc;
        bad["profile"]["ring"] = serde_json::Value::Null;
        assert!(check(&bad).is_err());
    }
    #[test]
    fn trail_policy_requires_bound_code_scene_and_order_without_upgrading_legacy_recipes() {
        let root = tempfile::tempdir().unwrap();
        let (catalog, doc) = trail_fixture(root.path());
        let path = root.path().join("source.json");
        let check = |doc: &serde_json::Value| {
            std::fs::write(&path, serde_json::to_vec(doc).unwrap()).unwrap();
            Source::load(&path, &catalog, || false)
        };
        let source = check(&doc).unwrap();
        source.verify_installation(root.path(), || false).unwrap();
        assert_eq!(
            Recipe::new(source.sha256.clone()).engine,
            road_network::VERSION
        );
        assert_eq!(
            Recipe::for_source(&source).engine,
            road_network::FAMILY_VERSION
        );
        for (field, value) in [
            ("width", serde_json::json!(0.)),
            ("splat_channel", serde_json::json!(8)),
            ("network_sample_stride", serde_json::json!(0)),
            ("seed_domain", serde_json::json!("../bad")),
        ] {
            let mut bad = doc.clone();
            bad["profile"]["trails"][field] = value;
            assert!(check(&bad).is_err());
        }
        let mut bad = doc.clone();
        bad["layout_stages"][0]["infrastructure_type"] = 2.into();
        assert!(check(&bad).is_err());
        let mut bad = doc.clone();
        bad["layout_stages"][1]["hierarchy_active"] = false.into();
        assert!(check(&bad).is_err());
        let mut bad = doc.clone();
        bad["profile"]["code_sources"] = serde_json::json!([]);
        assert!(check(&bad).is_err());
        let mut bad = doc.clone();
        bad["profile"]["map_scene"]["relative_path"] = "../other".into();
        assert!(check(&bad).is_err());
        std::fs::write(root.path().join("GameAssembly.dll"), b"wrong").unwrap();
        assert!(source.verify_installation(root.path(), || false).is_err());
        assert!(source.verify_installation(root.path(), || true).is_err());
    }
    #[test]
    fn source_defaults_preserve_saved_routing_profiles_and_bind_new_coastal_defaults() {
        let (_, doc) = fixture();
        let mut source: Source = serde_json::from_value(doc).unwrap();
        let bundled: crate::road_cost_source::Profile = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-8dd4e235-road-reservations.json"
        ))
        .unwrap();
        source.profile = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-8dd4e235-road-topology.json"
        ))
        .unwrap();
        source.sources = bundled.sources[..3].to_vec();
        source.sha256 = bundled.infrastructure_sha256.clone();
        assert_eq!(source.road_object_receipts().unwrap(), bundled.sources);
        let mut current = Recipe::for_prepared_source(
            &source,
            "a".repeat(64),
            "b".repeat(64),
            crate::source_road_planning::Policy {
                engine: crate::source_road_planning::VERSION.into(),
                assets_sha256: "c".repeat(64),
                roles_sha256: "d".repeat(64),
                ring_clearance: None,
                terrain_entrances: None,
            },
            false,
        )
        .unwrap();
        current.apply_source_defaults(&source).unwrap();
        assert_eq!(
            current
                .access_costmap
                .as_ref()
                .unwrap()
                .coastal_entrances
                .as_deref(),
            Some(crate::road_cost_source::GROUND_GUIDED_COASTAL_ENTRANCES_VERSION)
        );
        let mut saved = current;
        for profile in [&mut saved.access_costmap, &mut saved.ring_costmap]
            .into_iter()
            .flatten()
        {
            profile.id = "saved-coastal-policy".into();
            profile.coastal_entrances =
                Some(crate::road_cost_source::COASTAL_ENTRANCES_VERSION.into());
            profile.maximum_work /= 2;
            profile.policy.sampling = None;
        }
        let original = saved.clone();
        saved.apply_source_defaults(&source).unwrap();
        assert_eq!(saved, original);
        let mut foreign = original;
        foreign.access_costmap.as_mut().unwrap().sources[0].sha256 = "a".repeat(64);
        let before = foreign.clone();
        assert!(foreign.apply_source_defaults(&source).is_err());
        assert_eq!(foreign, before);
    }
    #[test]
    fn entrance_policy_is_source_bound_and_does_not_upgrade_saved_engines() {
        let root = tempfile::tempdir().unwrap();
        let (catalog, mut doc) = trail_fixture(root.path());
        let profile: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-entrances.json"
        ))
        .unwrap();
        doc["profile"]["revision"] = 4.into();
        doc["profile"]["entrances"] = profile["entrances"].clone();
        let path = root.path().join("entrances.json");
        let check = |doc: &serde_json::Value| {
            std::fs::write(&path, serde_json::to_vec(doc).unwrap()).unwrap();
            Source::load(&path, &catalog, || false)
        };
        let source = check(&doc).unwrap();
        assert_eq!(
            Recipe::for_source(&source).engine,
            road_network::ACCESS_VERSION
        );
        assert_eq!(
            Recipe::new(source.sha256.clone()).engine,
            road_network::VERSION
        );
        let recipe = Recipe::for_source(&source);
        let restored: Recipe =
            serde_json::from_slice(&serde_json::to_vec(&recipe).unwrap()).unwrap();
        assert_eq!(restored, recipe);
        for (field, value) in [
            ("minimum_height", serde_json::json!(0.)),
            ("owned_topology", serde_json::json!(2048)),
            ("maximum_exit_distance", serde_json::json!(513.)),
            ("maximum_exit_nodes", serde_json::json!(8193)),
        ] {
            let mut bad = doc.clone();
            bad["profile"]["entrances"][field] = value;
            assert!(check(&bad).is_err(), "{field}");
        }
        doc["profile"]["revision"] = 3.into();
        assert!(check(&doc).is_err());
    }
    #[test]
    fn shaping_profile_is_bound_optional_and_preserves_old_recipe_selection() {
        let root = tempfile::tempdir().unwrap();
        let (catalog, mut doc) = trail_fixture(root.path());
        let profile: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-shaped.json"
        ))
        .unwrap();
        doc["profile"]["revision"] = 5.into();
        doc["profile"]["entrances"] = profile["entrances"].clone();
        doc["profile"]["shaping"] = profile["shaping"].clone();
        let path = root.path().join("shape.json");
        let check = |doc: &serde_json::Value| {
            std::fs::write(&path, serde_json::to_vec(doc).unwrap()).unwrap();
            Source::load(&path, &catalog, || false)
        };
        let source = check(&doc).unwrap();
        let recipe = Recipe::for_source(&source);
        assert_eq!(recipe.engine, road_network::SHAPED_VERSION);
        recipe.validate().unwrap();
        let mut saved = recipe.clone();
        saved.engine = road_network::ACCESS_VERSION.into();
        saved.validate().unwrap();
        let restored: Recipe =
            serde_json::from_slice(&serde_json::to_vec(&saved).unwrap()).unwrap();
        assert_eq!(restored, saved);
        for (field, value) in [
            ("horizontal_iterations", serde_json::json!(65)),
            ("vertical_iterations", serde_json::json!(65)),
            ("resample_distance", serde_json::json!(0.)),
            ("maximum_work", serde_json::json!(2000001)),
        ] {
            let mut bad = doc.clone();
            bad["profile"]["shaping"][field] = value;
            assert!(check(&bad).is_err(), "{field}");
        }
        let mut bad = doc.clone();
        bad["profile"]["entrances"] = serde_json::Value::Null;
        assert!(check(&bad).is_err());
        doc["profile"]["revision"] = 4.into();
        assert!(check(&doc).is_err());
    }
    #[test]
    fn shared_stage_generates_both_families_replays_seeds_and_rolls_back_cancellation() {
        let root = tempfile::tempdir().unwrap();
        let (catalog, doc) = trail_fixture(root.path());
        let path = root.path().join("source.json");
        std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
        let source = Source::load(&path, &catalog, || false).unwrap();
        let mut project = Project {
            size: 1000,
            installation: Some(catalog.installation.clone()),
            preview_engine: crate::native_preview::VERSION.into(),
            monument_assets: Some(crate::AssetIdentity {
                catalog_sha256: catalog.sha256.clone(),
                bindings_sha256: "f".repeat(64),
            }),
            ..Project::default()
        };
        project.roads = Some(Recipe::for_source(&source));
        let road_path = "assets/bundled/prefabs/autospawn/monument/test.prefab";
        for (i, t) in project.targets.iter_mut().enumerate() {
            t.prefab_path = Some(
                if i < 2 {
                    road_path
                } else {
                    "assets/bundled/prefabs/autospawn/monument/trail.prefab"
                }
                .into(),
            );
            t.x = [-300., 300., 0.][i];
            t.z = [0., 0., 200.][i];
            t.radius = 20.;
        }
        let (hr, sr) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let mut splat = vec![0; sr * sr * 8];
        splat[4 * sr * sr..5 * sr * sr].fill(255);
        let layers = papyrus_terrain::native_layers::Layers {
            size: 1000,
            terrain_seed: project.seeds[0].value,
            biome_seed: project.seeds[1].value,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![17039; hr * hr],
            biome: vec![0; sr * sr * 5],
            splat,
            topology: vec![1; sr * sr],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        let alpha = vec![255; sr * sr];
        let fresh = || {
            papyrus_terrain::monument_composition::compose(&layers, &alpha, &[], || false)
                .unwrap()
                .unwrap()
        };
        let solution = Solution {
            outcomes: project
                .targets
                .iter()
                .map(|t| papyrus_terrain::placement::Outcome {
                    id: t.instance_id,
                    position: Some(papyrus_terrain::placement::Position {
                        x: t.x,
                        y: papyrus_terrain::erosion::decode(17039),
                        z: t.z,
                        yaw: 0.,
                        clearance: 1.,
                    }),
                    distance: Some(0.),
                    within_radius: true,
                    reason: String::new(),
                })
                .collect(),
            required_missing: 0,
            attempts: 3,
        };
        let key = project.terrain_key();
        let mut maps = fresh();
        let report = generate(
            &project,
            &solution,
            &catalog,
            &source,
            root.path(),
            &mut maps,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(report.families.len(), 2);
        assert_eq!(report.families[0].road_count, 1);
        assert_eq!(report.families[1].road_count, 1);
        assert_eq!(report.families[1].socket_count, 1); // includeInactive source behavior
        assert!(report.unresolved.is_empty());
        assert_eq!(report.json()["engine"], road_network::FAMILY_VERSION);
        assert!(maps.layers.splat[..sr * sr].iter().any(|v| *v > 0));
        let mut replay = fresh();
        let again = generate(
            &project,
            &solution,
            &catalog,
            &source,
            root.path(),
            &mut replay,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(report.json(), again.json());
        assert_eq!(maps.layers.splat, replay.layers.splat);
        assert_eq!(maps.layers.height, replay.layers.height);
        assert_eq!(project.terrain_key(), key);
        let mut isolated = solution.clone();
        isolated.outcomes[0].position = None;
        isolated.outcomes[1].position = None;
        let only_trail = generate(
            &project,
            &isolated,
            &catalog,
            &source,
            root.path(),
            &mut fresh(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(only_trail.roads.is_empty());
        assert_eq!(only_trail.unresolved, vec![3]);
        let saved = serde_json::to_vec(&project).unwrap();
        let restored: Project = serde_json::from_slice(&saved).unwrap();
        assert_eq!(restored, project);
        let mut old = project.clone();
        old.roads = Some(Recipe::new(source.sha256.clone()));
        let ticks = std::cell::Cell::new(0);
        let mut old_maps = fresh();
        let legacy = generate(
            &old,
            &solution,
            &catalog,
            &source,
            root.path(),
            &mut old_maps,
            || {
                ticks.set(ticks.get() + 1);
                false
            },
        )
        .unwrap()
        .unwrap();
        assert!(legacy.families.is_empty());
        assert_eq!(legacy.socket_count, 2);
        assert_eq!(legacy.roads.len(), 1);
        let limit = ticks.get() + 10;
        let ticks = std::cell::Cell::new(0);
        let mut cancelled = fresh();
        assert!(
            generate(
                &project,
                &solution,
                &catalog,
                &source,
                root.path(),
                &mut cancelled,
                || {
                    let n = ticks.get();
                    ticks.set(n + 1);
                    n >= limit
                }
            )
            .unwrap()
            .is_none()
        );
        assert!(ticks.get() > limit);
        assert_eq!(cancelled.layers.height, layers.height);
        assert_eq!(cancelled.layers.splat, layers.splat);
        assert_eq!(cancelled.layers.topology, layers.topology);
        project.seeds[3].inherited = false;
        project.seeds[3].value = project.seeds[3].value.wrapping_add(1);
        let changed = generate(
            &project,
            &solution,
            &catalog,
            &source,
            root.path(),
            &mut fresh(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_ne!(report.families[1].seed, changed.families[1].seed);
        assert_eq!(project.terrain_key(), key);
        let mut shaped_doc = doc;
        let profile: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-shaped.json"
        ))
        .unwrap();
        shaped_doc["profile"]["revision"] = 5.into();
        shaped_doc["profile"]["entrances"] = profile["entrances"].clone();
        shaped_doc["profile"]["shaping"] = profile["shaping"].clone();
        let path = root.path().join("shaped-source.json");
        std::fs::write(&path, serde_json::to_vec(&shaped_doc).unwrap()).unwrap();
        let shaped_source = Source::load(&path, &catalog, || false).unwrap();
        project.roads = Some(Recipe::for_source(&shaped_source));
        let shaped = generate(
            &project,
            &solution,
            &catalog,
            &shaped_source,
            root.path(),
            &mut fresh(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(shaped.json()["engine"], road_network::SHAPED_VERSION);
        assert_eq!(shaped.families.len(), 2);
        assert!(shaped.families.iter().all(|f| f.road_count > 0));
        assert!(shaped.shaping.accepted >= 2);
        assert!(shaped.unresolved.is_empty());
        assert_eq!(project.terrain_key(), key);
        let mut cost_doc = shaped_doc;
        let profile: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-access-costs.json"
        ))
        .unwrap();
        for name in ["ring", "access_routing"] {
            cost_doc["profile"][name] = profile[name].clone();
        }
        cost_doc["profile"]["revision"] = 7.into();
        cost_doc["ring_stage"] = serde_json::json!({"object_id":408,"order":0,"mode":-1,"disabled":false,"component_enabled":true,"hierarchy_active":true,"minimum_world_size":4000});
        let path = root.path().join("cost-source.json");
        std::fs::write(&path, serde_json::to_vec(&cost_doc).unwrap()).unwrap();
        let cost_source = Source::load(&path, &catalog, || false).unwrap();
        project.roads = Some(Recipe::for_source(&cost_source));
        project.roads.as_mut().unwrap().main_ring = Some(false);
        let cost = generate(
            &project,
            &solution,
            &catalog,
            &cost_source,
            root.path(),
            &mut fresh(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(cost.json()["engine"], road_network::COST_VERSION);
        assert!(cost.ring.is_none());
        assert_eq!(cost.trails_enabled, Some(true));
        assert_eq!(project.roads.as_ref().unwrap().include_trails, None);
        assert_eq!(
            crate::provenance::Provenance::for_recipe(&project, &[])
                .seeds
                .iter()
                .find(|s| s.stage == "trails")
                .unwrap()
                .status,
            crate::provenance::SeedStatus::Derived
        );
        assert!(cost.families[1].road_count > 0);
        project.roads.as_mut().unwrap().include_trails = Some(false);
        let no_trails = generate(
            &project,
            &solution,
            &catalog,
            &cost_source,
            root.path(),
            &mut fresh(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(no_trails.trails_enabled, Some(false));
        assert_eq!(
            crate::provenance::Provenance::for_recipe(&project, &[])
                .seeds
                .iter()
                .find(|s| s.stage == "trails")
                .unwrap()
                .status,
            crate::provenance::SeedStatus::Disabled
        );
        assert!(no_trails.ring.is_none());
        assert_eq!(no_trails.families[1].road_count, 0);
        assert_eq!(
            no_trails.families[0].road_count,
            cost.families[0].road_count
        );
        assert_eq!(no_trails.access_routing, cost.access_routing);
        assert_eq!(project.terrain_key(), key);

        // Reservations must reach the later family, not merely appear in metadata.
        let mut reserved_project = project.clone();
        let mut routing: crate::road_cost_source::Profile = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-road-reservations.json"
        ))
        .unwrap();
        routing.infrastructure_sha256 = cost_source.sha256.clone();
        routing.sources = cost_source.road_object_receipts().unwrap();
        let sweep = routing.path_reservations.take().unwrap();
        let recipe = reserved_project.roads.as_mut().unwrap();
        recipe.include_trails = Some(true);
        recipe.access_costmap = Some(routing);
        let initial = vec![0; fresh().layers.surface_resolution.pow(2)];
        let run = |p: &Project, maps: &mut Composed| {
            generate_with_source_costs(
                p,
                &solution,
                &catalog,
                &cost_source,
                root.path(),
                maps,
                None,
                Some(&initial),
                || false,
            )
        };
        let legacy = run(&reserved_project, &mut fresh()).unwrap().unwrap();
        reserved_project
            .roads
            .as_mut()
            .unwrap()
            .access_costmap
            .as_mut()
            .unwrap()
            .path_reservations = Some(sweep);
        let with_reservations = run(&reserved_project, &mut fresh()).unwrap().unwrap();
        assert!(
            with_reservations.families[0].road_count > 0
                && with_reservations.families[1].road_count > 0
        );
        let paved = &with_reservations.source_access_costmaps[0];
        let dirt = &with_reservations.source_access_costmaps[1];
        assert_eq!(paved["path_reservations"]["report"]["roads"], 0);
        assert!(
            dirt["path_reservations"]["report"]["roads"]
                .as_u64()
                .unwrap()
                > 0
        );
        assert!(
            dirt["path_reservations"]["report"]["sweep"]["newly_blocked"]
                .as_u64()
                .unwrap()
                > 0
        );
        assert!(
            dirt["grid"]["counts"]["placement"].as_u64().unwrap()
                > legacy.source_access_costmaps[1]["grid"]["counts"]["placement"]
                    .as_u64()
                    .unwrap()
        );
        assert_eq!(paved["grid"], legacy.source_access_costmaps[0]["grid"]);
        assert_eq!(dirt["grid"]["counts"]["blocked_trail_water"], 0);
        let mut limited = reserved_project.clone();
        limited
            .roads
            .as_mut()
            .unwrap()
            .access_costmap
            .as_mut()
            .unwrap()
            .path_reservations
            .as_mut()
            .unwrap()
            .maximum_work = 100;
        let mut rejected = fresh();
        let before = rejected.layers.clone();
        assert!(run(&limited, &mut rejected).is_err());
        assert_eq!(rejected.layers.height, before.height);
        assert_eq!(rejected.layers.splat, before.splat);
        assert_eq!(rejected.layers.topology, before.topology);

        let topology_profile: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-road-topology.json"
        ))
        .unwrap();
        cost_doc["profile"]["post_topology"] = topology_profile["post_topology"].clone();
        cost_doc["profile"]["revision"] = 8.into();
        cost_doc["topology_stage"] = serde_json::json!({"object_id":509,"order":3,"mode":-1,"disabled":false,"component_enabled":true,"hierarchy_active":true});
        let path = root.path().join("topology-source.json");
        std::fs::write(&path, serde_json::to_vec(&cost_doc).unwrap()).unwrap();
        let topology_source = Source::load(&path, &catalog, || false).unwrap();
        let mut next = Recipe::for_source(&topology_source);
        assert!(next.post_topology.is_some());
        next.main_ring = Some(false);
        project.roads = Some(next);
        let saved = serde_json::to_string(&project).unwrap();
        let reopened: Project = serde_json::from_str(&saved).unwrap();
        assert_eq!(project.roads, reopened.roads);
        let mut new_maps = fresh();
        let calls = std::cell::Cell::new(0);
        let next = generate(
            &project,
            &solution,
            &catalog,
            &topology_source,
            root.path(),
            &mut new_maps,
            || {
                calls.set(calls.get() + 1);
                false
            },
        )
        .unwrap()
        .unwrap();
        assert!(next.post_topology.as_ref().unwrap().roadside_added > 0);
        assert_eq!(
            next.json()["post_topology"]["engine"],
            papyrus_terrain::road_topology::VERSION
        );
        let cancel_at = calls.get() - 50;
        let ticks = std::cell::Cell::new(0);
        let mut cancelled = fresh();
        assert!(
            generate(
                &project,
                &solution,
                &catalog,
                &topology_source,
                root.path(),
                &mut cancelled,
                || {
                    ticks.set(ticks.get() + 1);
                    ticks.get() >= cancel_at
                }
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(cancelled.layers.topology, layers.topology);
        assert_eq!(cancelled.layers.height, layers.height);
        assert_eq!(cancelled.layers.splat, layers.splat);
        project.roads.as_mut().unwrap().post_topology = None;
        let mut prior_maps = fresh();
        let prior = generate(
            &project,
            &solution,
            &catalog,
            &topology_source,
            root.path(),
            &mut prior_maps,
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(prior.json().get("post_topology").is_none());
        assert_eq!(prior.json()["roads"], next.json()["roads"]);
        assert_eq!(prior_maps.layers.height, new_maps.layers.height);
        assert_eq!(prior_maps.layers.splat, new_maps.layers.splat);
        assert_eq!(prior_maps.layers.biome, new_maps.layers.biome);
        assert_eq!(prior_maps.alpha, new_maps.alpha);
        assert_ne!(prior_maps.layers.topology, new_maps.layers.topology);
        assert_eq!(project.terrain_key(), key);
        for pointer in [
            "/topology_stage/order",
            "/topology_stage/mode",
            "/profile/post_topology/eligible_mask",
        ] {
            let mut bad = cost_doc.clone();
            *bad.pointer_mut(pointer).unwrap() = 999.into();
            std::fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
            assert!(Source::load(&path, &catalog, || false).is_err());
        }
    }
    #[test]
    fn source_binding_rejects_wrong_builds_malformed_poses_and_incomplete_claims() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("source.json");
        let (catalog, doc) = fixture();
        let check = |value: &serde_json::Value| {
            std::fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
            Source::load(&path, &catalog, || false)
        };
        let source = check(&doc).unwrap();
        assert!(hash(&source.sha256));
        assert_eq!(
            source.prefabs.values().next().unwrap().connectors[0].position,
            [1., 2., 3.]
        );
        let mut bad = doc.clone();
        bad["profile"]["native_sha256"] = "f".repeat(64).into();
        assert!(check(&bad).is_err());
        let mut bad = doc.clone();
        bad["sources"][2]["sha256"] = "f".repeat(64).into();
        assert!(check(&bad).is_err());
        let name = catalog.entries.keys().next().unwrap();
        let mut bad = doc.clone();
        bad["prefabs"][name]["connectors"][0]["position"][0] = 1e9.into();
        assert!(check(&bad).is_err());
        let mut bad = doc.clone();
        let connector = bad["prefabs"][name]["connectors"][0].clone();
        bad["prefabs"][name]["connectors"]
            .as_array_mut()
            .unwrap()
            .push(connector);
        assert!(check(&bad).is_err());
        let mut bad = doc.clone();
        bad["prefabs"][name]["complete"] = false.into();
        assert!(check(&bad).is_err());
        assert!(Source::load(&path, &catalog, || true).is_err());
    }
    #[test]
    fn audited_profile_requires_declared_protection_roles_and_keeps_legacy_sources_loadable() {
        let (catalog, mut doc) = fixture();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("source.json");
        let check = |doc: &serde_json::Value| {
            std::fs::write(&path, serde_json::to_vec(doc).unwrap()).unwrap();
            Source::load(&path, &catalog, || false)
        };
        assert_eq!(check(&doc).unwrap().profile.revision, 1);
        let profile: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-v2.json"
        ))
        .unwrap();
        doc["profile"]["topology_flags"] = profile["topology_flags"].clone();
        doc["profile"]["revision"] = 2.into();
        doc["profile"]["protected_topology"] = profile["protected_topology"].clone();
        assert_eq!(check(&doc).unwrap().profile.revision, 2);
        let mask = doc["profile"]["protected_topology"].as_u64().unwrap();
        doc["profile"]["protected_topology"] = (mask & !2097152).into();
        assert!(check(&doc).is_err());
    }
    #[test]
    fn road_recipe_persists_through_profiles_without_changing_the_terrain_key() {
        let (catalog, _) = fixture();
        let mut project = Project {
            installation: Some(catalog.installation),
            preview_engine: crate::native_preview::VERSION.into(),
            monument_assets: Some(crate::AssetIdentity {
                catalog_sha256: catalog.sha256,
                bindings_sha256: "f".repeat(64),
            }),
            ..Project::default()
        };
        let key = project.terrain_key();
        project.roads = Some(Recipe::new("e".repeat(64)));
        project.validate().unwrap();
        assert_eq!(project.terrain_key(), key);
        let encoded = serde_json::to_vec(&project).unwrap();
        let restored: Project = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(project, restored);
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("roads.rec");
        crate::profile::Profile::new("Road fixture".into(), restored.clone())
            .save(&path)
            .unwrap();
        assert_eq!(crate::profile::Profile::load(&path).unwrap().recipe, {
            let mut current = restored.clone();
            crate::generation_defaults::apply(&mut current);
            current
        });
        let result = crate::headless::generate(
            &project,
            &crate::headless::Options {
                skip_composition: true,
                ..Default::default()
            },
            &std::sync::atomic::AtomicBool::new(false),
            |_, _| {},
        );
        assert!(matches!(result,Err(e) if e.contains("skip requested roads")));
        let mut invalid = restored;
        invalid.roads.as_mut().unwrap().source_sha256 = "../escape".into();
        assert!(invalid.validate().is_err());
    }
}
