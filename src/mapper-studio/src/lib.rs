//! Project recipes and deterministic preview support for the native workstation.
use serde::{Deserialize, Serialize};
pub mod asset_discovery;
mod cargo_route;
mod native_catalog;
mod native_texture_sources;
pub mod authored_tracks;
pub mod coastal_candidates;
pub mod coastal_placement;
mod coastal_road_entrances;
mod clutter_topology_stage;
mod cliff_topology_stage;
mod uniform_cliff_source;
mod jungle_vine_source;
mod coastal_cliff_source;
pub mod composition;
pub mod content_package;
pub mod custom_monuments;
pub mod updates;
pub mod monument_addons;
pub mod rustmaps_profile;
pub mod rustedit_prefab;
pub mod project_bundle;
pub mod decor_models;
pub mod decor_source;
mod entrance_grid;
pub mod environment_checks;
pub mod environment_provider;
pub mod dungeon_source;
pub mod tunnel_source;
pub mod tunnel_generation;
pub mod offshore_generation;
pub mod environment_runtime;
pub mod erosion_source;
pub mod erosion_compute;
pub mod generation_defaults;
pub mod generation_plan;
pub mod generation_resources;
pub mod generation_validation;
pub mod headless;
pub mod image_export;
pub mod image_render;
pub mod image_raster;
pub mod image_snapshot;
pub mod infrastructure;
pub mod installation;
pub mod loading_progress;
pub mod map_colors;
pub mod map_compute;
pub mod map_grid;
pub mod map_image;
pub mod map_labels;
pub mod map_metadata;
pub mod map_surface;
pub mod map_normal;
pub mod map_surface_frame;
mod metadata_runtime;
pub mod monument_alignment;
pub mod monument_attributes;
pub mod monument_tag_geometry;
pub mod monument_tag_source;
pub mod water_treatment_source;
pub mod powerline_source;
pub mod scene_wire_source;
pub mod scene_wires;
pub mod powerline_generation;
pub mod water_treatment_stage;
pub mod monument_blockers;
pub mod monument_extract;
pub mod monument_source;
pub mod monument_stages;
mod monument_water_hook;
pub mod native_noise;
pub mod native_preview;
pub mod native_processing;
pub mod native_profile;
pub mod native_view;
pub mod no_build;
pub mod object_extract;
pub mod object_generation;
pub mod object_preparation;
pub mod object_workflow;
pub mod offshore_source;
pub mod offshore_decor;
pub mod palette_extract;
pub mod path_edit;
pub mod path_export;
pub mod placement_preview;
pub mod placement_water;
pub mod plugins;
pub mod prefab_assets;
mod prefab_priority;
pub mod prefab_export;
pub mod prefab_identities;
pub mod prefab_textures;
pub mod preferences;
pub mod profile;
pub mod recipe_options;
pub mod provenance;
pub mod rail_bed_preview;
pub mod rail_extract;
pub mod rail_generation;
pub mod rail_mesh_source;
pub mod rail_mesh_stage;
pub mod rail_preview;
pub mod rail_source;
pub mod river_extract;
pub mod river_generation;
pub mod river_mesh_source;
pub(crate) mod road_alignment;
pub mod road_cost_source;
pub mod road_extract;
pub mod road_generation;
pub mod road_object_connectors;
pub mod road_object_effects;
pub mod road_object_placement;
pub mod road_object_source;
mod road_portal_heading;
pub mod road_portal_mesh;
mod road_portal_revision;
pub mod road_refinement;
pub(crate) mod road_replanning;
pub mod road_surface_source;
pub mod road_surface_stage;
mod source_lease;
mod slope_compute;
mod source_profile;
pub mod source_road_assets;
pub mod source_road_cache;
pub mod source_road_handoff;
pub mod source_road_inputs;
mod source_road_materialization;
pub mod source_road_planning;
pub mod spawn_source;
pub mod splat_preview;
pub mod stamp_projection;
pub mod terrain_cache;
pub mod terrain_palette;
pub mod terrain_placement;
pub mod terrain_stages;
pub mod terrain_view;
pub mod water_collision;
pub mod river_objects;
pub mod water_geometry;
pub mod world_document;
pub mod topology_presets;
pub mod world_recovery;
use std::{
    collections::HashSet,
    io::Write,
    path::Path,
    sync::Arc,
};

pub const MAX_SEED: u32 = i32::MAX as u32;
pub const MASTER_SEED_REQUIRED: &str = "Enter a master seed or use Randomize.";
pub const STAGES: [&str; 6] = [
    "Terrain",
    "Biomes",
    "Rivers",
    "Roads",
    "Monuments",
    "Decoration",
];
pub const MAX_PROJECT_BYTES: u64 = 4 * 1024 * 1024;
pub const LEGACY_PREVIEW_ENGINE: &str = "mapper-field-preview-1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerrainKey {
    pub seed: u32,
    pub biome_seed: Option<u32>,
    pub size: u32,
    pub engine: String,
    pub source: Option<installation::Installation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_profile: Option<native_profile::Profile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rivers: Option<river_generation::Key>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageSeed {
    pub value: u32,
    pub inherited: bool,
}

/// Version-one name-based FNV derivation. Never use Rust's unspecified DefaultHasher.
pub fn derive_seed(master: u32, stage: &str) -> u32 {
    // Stable seed-domain offset basis, preserved across application renames.
    let mut hash = 2_952_533_033_u32;
    for byte in master.to_le_bytes().into_iter().chain(stage.bytes()) {
        hash = (hash ^ u32::from(byte)).wrapping_mul(16_777_619);
    }
    hash % MAX_SEED + 1
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Presence {
    Allowed,
    Required,
    Excluded,
}

/// Explicit recipe policy; defaults preserve recipes written before these controls.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Performance {
    pub monuments: bool,
}
impl Default for Performance {
    fn default() -> Self {
        Self { monuments: true }
    }
}
impl Performance {
    fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

pub const DEFAULT_MONUMENT_MOVE_LIMIT_M: u32 = 150;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonumentTarget {
    pub instance_id: u64,
    pub name: String,
    #[serde(default)]
    pub prefab_path: Option<String>,
    pub x: f32,
    pub z: f32,
    pub radius: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_displacement_m: Option<u32>,
    pub strength: f32,
    pub exact: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yaw: Option<f32>,
    pub presence: Presence,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetIdentity {
    pub catalog_sha256: String,
    pub bindings_sha256: String,
}
impl AssetIdentity {
    pub fn validate(&self) -> Result<(), String> {
        if [&self.catalog_sha256, &self.bindings_sha256]
            .iter()
            .any(|s| {
                s.len() != 64
                    || !s
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        {
            return Err("Invalid saved monument asset fingerprint".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub schema: u32,
    #[serde(default)]
    pub installation: Option<installation::Installation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_profile: Option<native_profile::Profile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terrain_overrides: Option<native_profile::TerrainOverrides>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub biome_overrides: Option<native_profile::BiomeOverrides>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_erosion: Option<erosion_compute::Identity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terrain_graph: Option<Arc<papyrus_terrain::terrain_graph::Graph>>,
    pub seed_derivation: u32,
    pub preview_engine: String,
    #[serde(default)]
    pub placement_engine: String,
    pub master_seed: u32,
    pub size: u32,
    pub seeds: [StageSeed; 6],
    pub targets: Vec<MonumentTarget>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub monument_addons: Vec<monument_addons::AddOn>,
    #[serde(default, skip_serializing_if = "monument_addons::Choices::is_empty")]
    pub monument_overlay_choices: monument_addons::Choices,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom_monuments: Vec<custom_monuments::Placement>,
    #[serde(default, skip_serializing_if = "Performance::is_default")]
    pub performance: Performance,
    #[serde(default, skip_serializing_if = "recipe_options::Options::is_default")]
    pub options: recipe_options::Options,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composition_engine: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monument_assets: Option<AssetIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roads: Option<infrastructure::Recipe>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rails: Option<rail_generation::Recipe>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decoration: Option<decor_source::Recipe>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objects: Option<object_generation::Recipe>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topology_edits: Option<Arc<papyrus_terrain::topology_paint::Edits>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path_edits: Vec<path_edit::Change>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monument_rules: Option<monument_source::Recipe>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rivers: Option<river_generation::Recipe>,
}

impl Default for Project {
    fn default() -> Self {
        let master_seed = 12345;
        Self {
            schema: 3,
            installation: None,
            native_profile: None,
            terrain_overrides: None,
            biome_overrides: None,
            gpu_erosion: None,
            terrain_graph: None,
            seed_derivation: 1,
            preview_engine: papyrus_terrain::landscape::VERSION.into(),
            placement_engine: papyrus_terrain::placement::VERSION.into(),
            master_seed,
            size: 4000,
            performance: Performance::default(),
            options: recipe_options::Options::default(),
            composition_engine: Some(papyrus_terrain::monument_composition::WATER_VERSION.into()),
            monument_assets: None,
            roads: None,
            rails: None,
            decoration: None,
            objects: None,
            topology_edits: None,
            path_edits: Vec::new(),
            monument_rules: None,
            rivers: None,
            monument_addons: Vec::new(),
            monument_overlay_choices: Default::default(),
            custom_monuments: Vec::new(),
            seeds: std::array::from_fn(|i| StageSeed {
                value: derive_seed(master_seed, STAGES[i]),
                inherited: true,
            }),
            targets: ["Launch Site", "Satellite Dish", "Airfield"]
                .into_iter()
                .enumerate()
                .map(|(i, name)| MonumentTarget {
                    instance_id: i as u64 + 1,
                    name: name.into(),
                    prefab_path: placement_preview::path_for_name(name),
                    x: [-180., 20., 140.][i],
                    z: [100., 0., -140.][i],
                    radius: 120.,
                    max_displacement_m: None,
                    strength: 1.,
                    exact: false,
                    yaw: None,
                    presence: Presence::Required,
                })
                .collect(),
        }
    }
}

impl Project {
    pub fn monument_blocker(&self, prefab: Option<&str>) -> Option<&'static str> {
        if !self.performance.monuments {
            Some("Enable Monuments in the recipe to change this setting.")
        } else {
            prefab.and_then(|path| self.options.prefab_blocker(path))
        }
    }

    pub fn requires_monument(&self, target: &MonumentTarget) -> bool {
        match self.monument_presence(target) {
            Presence::Required => true,
            Presence::Allowed => target.max_displacement_m.is_some(),
            Presence::Excluded => false,
        }
    }

    pub fn monument_presence(&self, target: &MonumentTarget) -> Presence {
        if self.monument_blocker(target.prefab_path.as_deref()).is_some() {
            Presence::Excluded
        } else if target.presence != Presence::Excluded && self.options.prefers_center(target.prefab_path.as_deref()) {
            Presence::Required
        } else {
            target.presence
        }
    }

    pub fn add_monument_addon(&mut self, addon: monument_addons::AddOn) -> Result<(), String> {
        self.monument_addons.push(addon);
        if let Err(error) = self.validate() {
            self.monument_addons.pop();
            return Err(error);
        }
        Ok(())
    }

    pub fn has_monument_addons(&self) -> bool {
        !self.monument_addons.is_empty() || !self.monument_overlay_choices.is_empty()
    }

    /// A recipe can choose one library version or the stock monument.
    pub fn choose_monument_overlay(&mut self, anchor: u32, version: Option<String>) -> Result<(), String> {
        let mut next = self.clone();
        next.monument_addons.retain(|addon| addon.anchor_id != anchor);
        next.monument_overlay_choices.remove(&anchor);
        if let Some(version) = version {
            next.monument_overlay_choices.insert(anchor, version);
        }
        next.validate()?;
        self.monument_overlay_choices = next.monument_overlay_choices;
        self.monument_addons = next.monument_addons;
        Ok(())
    }

    pub fn check_monument_assets(&self, catalog: &str, bindings: &str) -> Result<(), String> {
        if self.monument_assets.as_ref().is_some_and(|saved| {
            saved.catalog_sha256 != catalog || saved.bindings_sha256 != bindings
        }) {
            return Err("Monument assets differ from this project's saved fingerprints. Load matching assets or explicitly replace the saved fingerprints.".into());
        }
        Ok(())
    }
    pub fn terrain_key(&self) -> TerrainKey {
        TerrainKey {
            rivers: self.rivers.as_ref().map(|recipe| river_generation::Key {
                recipe: recipe.clone(),
                seed: self.seeds[2].value,
                limit: self.options.river_limit,
            }),
            seed: self.seeds[0].value,
            size: self.size,
            engine: self.preview_engine.clone(),
            native_profile: self.effective_native_profile(),
            source: if native_preview::is_native(&self.preview_engine) {
                self.installation.clone()
            } else {
                None
            },
            biome_seed: if self.preview_engine != LEGACY_PREVIEW_ENGINE {
                Some(self.seeds[1].value)
            } else {
                None
            },
        }
    }
    pub fn effective_native_profile(&self) -> Option<native_profile::Profile> {
        if !native_preview::is_native(&self.preview_engine) {
            return None;
        }
        if self.terrain_overrides.is_none() && self.biome_overrides.is_none() {
            return self.native_profile.clone();
        }
        let base = self.native_profile.as_ref().or_else(|| {
            self.installation.as_ref().and_then(native_profile::legacy_profile)
        })?;
        let mut settings = base.settings.clone();
        if let Some(terrain) = self.terrain_overrides {
            settings.loot_angle_millidegrees = terrain.tier_angle;
            settings.terrain_weights = terrain.tier_weights;
        }
        if let Some(biomes) = self.biome_overrides {
            settings.biome_angle_millidegrees = biomes.angle;
            settings.biome_weights = biomes.weights;
            settings.jungle_weight = biomes.jungle_weight;
        }
        // Checking a section initially copies the defaults. Keep the original
        // key for those same inputs, including legacy keys without a profile.
        if settings == base.settings {
            return self.native_profile.clone();
        }
        Some(native_profile::Profile { settings, ..base.clone() })
    }
    pub fn add_target(&mut self, name: String, path: String) -> Result<(), String> {
        if self.targets.len() >= papyrus_terrain::placement::MAX_TARGETS {
            return Err("Target limit reached (128).".into());
        }
        let instance_id = self
            .targets
            .iter()
            .map(|t| t.instance_id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("Target identity space is exhausted.")?;
        self.targets.push(MonumentTarget {
            instance_id,
            name,
            prefab_path: Some(path),
            x: 0.,
            z: 0.,
            radius: 250.,
            max_displacement_m: None,
            strength: 1.,
            exact: false,
            yaw: None,
            presence: Presence::Required,
        });
        Ok(())
    }
    pub fn refresh_inherited_seeds(&mut self) {
        for (seed, name) in self.seeds.iter_mut().zip(STAGES) {
            if seed.inherited {
                seed.value = derive_seed(self.master_seed, name);
            }
        }
    }

    pub fn require_master_seed(&self) -> Result<(), String> {
        if self.master_seed == 0 {
            return Err(MASTER_SEED_REQUIRED.into());
        }
        Ok(())
    }

    pub fn shuffle_monuments(&mut self) {
        let seed = &mut self.seeds[4];
        seed.inherited = false;
        seed.value = seed.value % MAX_SEED + 1;
    }

    pub fn supports_gpu_erosion(&self) -> bool {
        self.supports_terrain_graph()
    }

    pub fn supports_terrain_graph(&self) -> bool {
        terrain_stages::enabled(self)
            && self.installation.is_some()
            && self.composition_engine.is_some()
    }

    pub fn validate(&self) -> Result<(), String> {
        self.options.validate()?;
        if self.options.island_intensity.is_some_and(|intensity| intensity > 0)
            && !self.supports_terrain_graph()
        {
            return Err("Custom islands require installation-backed terrain composition".into());
        }
        monument_addons::validate(&self.monument_addons)?;
        monument_addons::validate_choices(&self.monument_overlay_choices, &self.monument_addons)?;
        custom_monuments::validate(self)?;
        if let Some(graph) = &self.terrain_graph {
            if !self.supports_terrain_graph() {
                return Err("Terrain graphs require an installation-backed source composition recipe".into());
            }
            graph.validate(self.size)?;
        }
        if let Some(gpu) = &self.gpu_erosion {
            gpu.validate()?;
            if !self.supports_gpu_erosion() {
                return Err("GPU erosion requires an installation-backed source composition recipe".into());
            }
        }
        if self.has_monument_addons()
            && (!native_preview::is_native(&self.preview_engine)
                || self.installation.is_none()
                || self.composition_engine.is_none())
        {
            return Err("Monument add-ons require installation-backed generation".into());
        }
        if let Some(rivers) = &self.rivers {
            rivers.validate()?;
            if !native_preview::is_native(&self.preview_engine) || self.installation.is_none() {
                return Err("River generation requires installed native terrain".into());
            }
            if self
                .composition_engine
                .as_deref()
                .is_some_and(|e| !papyrus_terrain::monument_composition::water_aware(Some(e)))
            {
                return Err("River composition requires water-aware terrain composition".into());
            }
        }
        if let Some(rails) = &self.rails {
            rails.validate()?;
            if rail_generation::before_roads(self)
                && !self.roads.as_ref().is_some_and(|r| {
                    r.staged.is_some()
                        && r.entrance_launch.is_some_and(|p| {
                            p.engine == papyrus_terrain::road_network::LaunchEngine::GlobalEarthwork
                        })
                })
            {
                return Err(
                    "Source-ordered rails require staged roads with global earthwork".into(),
                );
            }
            if (self.decoration.is_some() && self.objects.is_none())
                || self.roads.is_none()
                || self.monument_rules.is_none()
                || self.monument_assets.is_none()
                || !native_preview::is_native(&self.preview_engine)
                || self.composition_engine.is_none()
            {
                return Err(
                    "Rail composition requires native composition, pinned infrastructure/monuments and shared live objects when decoration is enabled".into(),
                );
            }
        }
        if let Some(objects) = &self.objects {
            objects.validate()?;
            if self.roads.is_none() || self.decoration.is_none() || self.monument_rules.is_none() {
                return Err(
                    "Live objects require pinned roads, decoration and monument sources".into(),
                );
            }
        }
        if let Some(decor) = &self.decoration {
            decor.validate()?;
            if !native_preview::is_native(&self.preview_engine)
                || self.composition_engine.is_none()
                || self.monument_assets.is_none()
            {
                return Err(
                    "Static decoration requires native composition and pinned monument assets"
                        .into(),
                );
            }
        }
        if let Some(roads) = &self.roads {
            roads.validate()?;
            if roads.ring_costmap.is_some() && self.monument_rules.is_none() {
                return Err("Source ring costs require pinned monument blocker metadata".into());
            }
            if !native_preview::is_native(&self.preview_engine)
                || self.composition_engine.is_none()
                || self.monument_assets.is_none()
            {
                return Err(
                    "Road generation requires native composition and pinned monument assets".into(),
                );
            }
        }
        if let Some(profile) = &self.native_profile {
            profile.matches(
                self.installation
                    .as_ref()
                    .ok_or("Native profile requires a saved Rust installation identity")?,
            )?;
        }
        if let Some(terrain) = &self.terrain_overrides {
            terrain.validate()?;
        }
        if let Some(biomes) = &self.biome_overrides {
            biomes.validate()?;
        }
        if self
            .composition_engine
            .as_deref()
            .is_some_and(|v| !papyrus_terrain::monument_composition::supported(v))
        {
            return Err("Unsupported monument composition engine version".into());
        }
        if let Some(edits) = &self.topology_edits {
            edits.validate(self.size)?;
            if !native_preview::is_native(&self.preview_engine) || self.composition_engine.is_none()
            {
                return Err("Topology overrides require native terrain composition".into());
            }
        }
        path_edit::validate(&self.path_edits, self.size)?;
        for edit in &self.path_edits {
            let enabled = match edit.route.kind {
                path_edit::Kind::Rail => self.rails.is_some(),
                path_edit::Kind::Road | path_edit::Kind::Trail => self.roads.as_ref()
                    .and_then(|roads| roads.staged.as_ref())
                    .is_some_and(|staged| road_generation::is_source_engine(&staged.engine)),
            };
            if !enabled {
                return Err("Spline changes require their original road or rail generation settings".into());
            }
        }
        if let Some(rules) = &self.monument_rules {
            rules.validate()?;
            if monument_source::supported(&self.placement_engine)
                && rules.engine != self.placement_engine
            {
                return Err("Placement engine and monument source recipe disagree".into());
            }
        }
        if monument_source::supported(&self.placement_engine) && self.monument_rules.is_none() {
            return Err(
                "Source monument checks require a pinned installation rule snapshot".into(),
            );
        }
        if self.schema != 3
            || self.seed_derivation != 1
            || ![
                LEGACY_PREVIEW_ENGINE,
                papyrus_terrain::landscape::VERSION,
                native_preview::VERSION,
                native_preview::OCEAN_VERSION,
            ]
            .contains(&self.preview_engine.as_str())
            || ![
                papyrus_terrain::placement::VERSION,
                placement_preview::ROTATION_VERSION,
                placement_preview::EARTHWORK_VERSION,
                placement_preview::BALANCED_EARTHWORK_VERSION,
                placement_preview::BOUNDS_EARTHWORK_VERSION,
                monument_source::VERSION,
                coastal_placement::VERSION,
                coastal_placement::BUDGET_VERSION,
                placement_preview::LEGACY_EARTHWORK_VERSION,
            ]
            .contains(&self.placement_engine.as_str())
        {
            return Err("Unsupported project or preview engine version. Open with the matching studio version.".into());
        }
        if placement_preview::is_earthwork(&self.placement_engine)
            && (!native_preview::is_native(&self.preview_engine)
                || self.composition_engine.is_none())
        {
            return Err(
                "Earthwork placement requires native terrain with monument composition enabled"
                    .into(),
            );
        }
        if let Some(assets) = &self.monument_assets {
            assets.validate()?;
            if self.installation.is_none() {
                return Err("Saved monument assets require a Rust installation identity".into());
            }
        }
        if let Some(source) = &self.installation {
            source.validate()?;
        }
        if native_preview::is_native(&self.preview_engine) && self.installation.is_none() {
            return Err("Native terrain recipes require a saved Rust installation identity".into());
        }
        if !(1000..=6000).contains(&self.size) || self.master_seed > MAX_SEED {
            return Err("World size or master seed is outside the supported range.".into());
        }
        for (seed, name) in self.seeds.iter().zip(STAGES) {
            if !(1..=MAX_SEED).contains(&seed.value)
                || (seed.inherited && seed.value != derive_seed(self.master_seed, name))
            {
                return Err(format!("Invalid {name} seed or inherited seed mismatch."));
            }
        }
        if self.targets.len() > papyrus_terrain::placement::MAX_TARGETS {
            return Err("Too many monument targets.".into());
        }
        let mut ids = HashSet::new();
        for t in &self.targets {
            let half = self.size as f32 / 2.;
            if t.instance_id == 0
                || !ids.insert(t.instance_id)
                || t.name.is_empty()
                || t.name.len() > 256
                || t.prefab_path
                    .as_ref()
                    .is_some_and(|p| p.is_empty() || p.len() > 512 || p.contains('\0'))
                || !t.x.is_finite()
                || !t.z.is_finite()
                || t.x.abs() > half
                || t.z.abs() > half
                || !t.radius.is_finite()
                || !(1. ..=self.size as f32).contains(&t.radius)
                || t.max_displacement_m.is_some_and(|limit| !(1..=self.size).contains(&limit))
                || !t.strength.is_finite()
                || !(0.1..=10.).contains(&t.strength)
                || t.yaw
                    .is_some_and(|y| !y.is_finite() || !(0. ..360.).contains(&y))
                || (t.yaw.is_some() && self.placement_engine == papyrus_terrain::placement::VERSION)
            {
                return Err(
                    "Invalid or duplicate monument target. Coordinates must stay inside the world."
                        .into(),
                );
            }
        }
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        project_bundle::load_recipe(path)
    }

    fn decode_json(data: &[u8]) -> Result<Self, String> {
        let mut project: Self = serde_json::from_slice(data).map_err(|e| e.to_string())?;
        if project.schema == 1 && project.placement_engine.is_empty() {
            project.schema = 2;
            project.placement_engine = papyrus_terrain::placement::VERSION.into();
            for target in &mut project.targets {
                if target.prefab_path.is_none() {
                    target.prefab_path = placement_preview::path_for_name(&target.name);
                }
            }
        }
        if project.schema == 2 {
            if project.installation.is_some() {
                return Err("Legacy projects cannot declare an installation identity".into());
            }
            project.schema = 3;
        }
        project.validate()?;
        generation_defaults::apply(&mut project);
        project.validate()?;
        Ok(project)
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        if path.exists() && project_bundle::rewrite_recipe(path,path,self.clone(),true,||false)? {
            return Ok(());
        }
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_PROJECT_BYTES {
            return Err("Project exceeds the 4 MiB limit.".into());
        }
        let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
        temp.write_all(&bytes).map_err(|e| e.to_string())?;
        temp.as_file().sync_all().map_err(|e| e.to_string())?;
        temp.persist(path).map_err(|e| e.error.to_string())?;
        Ok(())
    }
}

#[derive(Default)]
pub struct History {
    undo: Vec<Project>,
    redo: Vec<Project>,
    snapshot: Option<Project>,
}
impl History {
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn record(&mut self, before: Project, after: &Project) {
        if &before != after {
            self.push_undo(before);
        }
    }
    pub fn snapshot(&mut self, current: &Project) -> Project {
        self.snapshot
            .take()
            .filter(|saved| saved == current)
            .unwrap_or_else(|| current.clone())
    }
    pub fn finish_snapshot(&mut self, before: Project, after: &Project, record: bool) {
        if &before == after {
            self.snapshot = Some(before);
        } else if record {
            self.push_undo(before);
        }
    }
    fn push_undo(&mut self, before: Project) {
        self.undo.push(before);
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }
    pub fn undo(&mut self, current: &mut Project) {
        if let Some(previous) = self.undo.pop() {
            self.redo.push(std::mem::replace(current, previous));
        }
    }
    pub fn redo(&mut self, current: &mut Project) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(current, next));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_stamp_revision_preserves_base_identity_and_survives_profile_and_undo() {
        for revision in [
            papyrus_terrain::monument_composition::SOURCE_TEXTURE_VERSION,
            papyrus_terrain::monument_composition::SOURCE_CLIFF_VERSION,
            papyrus_terrain::monument_composition::SOURCE_RASTER_VERSION,
            papyrus_terrain::monument_composition::SOURCE_HEIGHT_VERSION,
        ] {
            let mut project = Project::default();
            let old = project.clone();
            let key = project.terrain_key();
            project.composition_engine = Some(revision.into());
            project.validate().unwrap();
            assert_eq!(project.terrain_key(), key);
            let profile = profile::Profile::new("Source stamps".into(), project.clone());
            let restored: profile::Profile =
                serde_json::from_slice(&serde_json::to_vec(&profile).unwrap()).unwrap();
            restored.validate().unwrap();
            assert_eq!(restored.recipe, project);
            let mut history = History::default();
            history.record(old.clone(), &project);
            history.undo(&mut project);
            assert_eq!(project, old);
            history.redo(&mut project);
            assert_eq!(project, restored.recipe);
        }
    }
    #[test]
    fn live_object_recipe_preserves_profiles_undo_and_terrain_identity() {
        let mut project = Project {
            preview_engine: native_preview::VERSION.into(),
            installation: Some(installation::Installation {
                kind: installation::InstallationKind::Client,
                native: installation::SourceFile {
                    relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                    sha256: "a".repeat(64),
                    bytes: 1,
                },
                scene_manifest: None,
            }),
            monument_assets: Some(AssetIdentity {
                catalog_sha256: "b".repeat(64),
                bindings_sha256: "c".repeat(64),
            }),
            roads: Some(infrastructure::Recipe::new("d".repeat(64))),
            decoration: Some(decor_source::Recipe::new("e".repeat(64))),
            monument_rules: Some(monument_source::Recipe {
                engine: monument_source::VERSION.into(),
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
                dungeon_source: None, offshore_source: None,
            }),
            ..Default::default()
        };
        project.validate().unwrap();
        let before = project.clone();
        assert!(
            serde_json::to_value(&before)
                .unwrap()
                .get("objects")
                .is_none()
        );
        project.objects = Some(object_generation::Recipe::new("1".repeat(64)));
        project.validate().unwrap();
        assert_eq!(before.terrain_key(), project.terrain_key());
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("objects.rec");
        profile::Profile::new("Objects".into(), project.clone())
            .save(&path)
            .unwrap();
        let mut current = project.clone();
        generation_defaults::apply(&mut current);
        assert_eq!(profile::Profile::load(&path).unwrap().recipe, current);
        let mut history = History::default();
        history.record(before.clone(), &project);
        history.undo(&mut project);
        assert_eq!(project, before);
        history.redo(&mut project);
        assert!(project.objects.is_some());
        project.objects.as_mut().unwrap().maximum_terrain_work = 0;
        assert!(project.validate().is_err());
        project.objects = Some(object_generation::Recipe::new("../bad".into()));
        assert!(project.validate().is_err());
        project.objects = Some(object_generation::Recipe::new("1".repeat(64)));
        for missing in [0, 1, 2] {
            let mut bad = project.clone();
            match missing {
                0 => bad.roads = None,
                1 => bad.decoration = None,
                _ => bad.monument_rules = None,
            }
            assert!(bad.validate().is_err());
        }
    }
    #[test]
    fn monument_source_upgrade_roundtrips_profiles_and_undo_without_new_terrain() {
        let mut project = Project {
            preview_engine: native_preview::VERSION.into(),
            placement_engine: placement_preview::BOUNDS_EARTHWORK_VERSION.into(),
            installation: Some(installation::Installation {
                kind: installation::InstallationKind::Client,
                native: installation::SourceFile {
                    relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                    sha256: "a".repeat(64),
                    bytes: 1,
                },
                scene_manifest: None,
            }),
            ..Default::default()
        };
        project.validate().unwrap();
        let before = project.clone();
        project.placement_engine = monument_source::VERSION.into();
        assert!(project.validate().is_err());
        project.monument_rules = Some(monument_source::Recipe {
            engine: monument_source::VERSION.into(),
            source_sha256: "b".repeat(64),
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
            dungeon_source: None, offshore_source: None,
        });
        project.validate().unwrap();
        assert_eq!(project.terrain_key(), before.terrain_key());
        let dir = tempfile::tempdir().unwrap();
        let profile_path = dir.path().join("checked.rec");
        profile::Profile::new("Source checks".into(), project.clone())
            .save(&profile_path)
            .unwrap();
        assert_eq!(profile::Profile::load(&profile_path).unwrap().recipe, {
            let mut current = project.clone();
            generation_defaults::apply(&mut current);
            current
        });
        let mut history = History::default();
        history.record(before.clone(), &project);
        history.undo(&mut project);
        assert_eq!(project, before);
        history.redo(&mut project);
        assert_eq!(project.placement_engine, monument_source::VERSION);
        assert_eq!(project.terrain_key(), before.terrain_key());
    }
    #[test]
    fn asset_fingerprints_roundtrip_and_mismatches_require_explicit_replacement() {
        let mut project = Project {
            installation: Some(installation::Installation {
                kind: installation::InstallationKind::Client,
                native: installation::SourceFile {
                    relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                    sha256: "a".repeat(64),
                    bytes: 1,
                },
                scene_manifest: None,
            }),
            ..Default::default()
        };
        let key = project.terrain_key();
        project.monument_assets = Some(AssetIdentity {
            catalog_sha256: "b".repeat(64),
            bindings_sha256: "c".repeat(64),
        });
        project.validate().unwrap();
        assert_eq!(project.terrain_key(), key);
        assert!(
            project
                .check_monument_assets(&"b".repeat(64), &"c".repeat(64))
                .is_ok()
        );
        assert!(
            project
                .check_monument_assets(&"d".repeat(64), &"c".repeat(64))
                .is_err()
        );
        assert_eq!(
            serde_json::from_str::<Project>(&serde_json::to_string(&project).unwrap()).unwrap(),
            project
        );
        project.monument_assets.as_mut().unwrap().bindings_sha256 = "invalid".into();
        assert!(project.validate().is_err());
    }

    #[test]
    fn composition_version_is_explicit_and_legacy_recipes_stay_uncomposed() {
        let mut old = serde_json::to_value(Project::default()).unwrap();
        old.as_object_mut().unwrap().remove("composition_engine");
        let loaded: Project = serde_json::from_value(old).unwrap();
        loaded.validate().unwrap();
        assert!(loaded.composition_engine.is_none());
        let mut enabled = loaded.clone();
        enabled.composition_engine = Some(papyrus_terrain::monument_composition::VERSION.into());
        assert_eq!(loaded.terrain_key(), enabled.terrain_key());
        enabled.composition_engine = Some("unknown-future-composer".into());
        assert!(enabled.validate().is_err());
    }

    #[test]
    fn water_revision_is_explicit_undoable_and_keeps_the_same_base_variant() {
        let mut project = Project::default();
        assert_eq!(
            project.composition_engine.as_deref(),
            Some(papyrus_terrain::monument_composition::WATER_VERSION)
        );
        project.composition_engine = Some(papyrus_terrain::monument_composition::VERSION.into());
        let legacy = project.clone();
        let key = project.terrain_key();
        project.composition_engine = Some(papyrus_terrain::monument_composition::WATER_VERSION.into());
        project.validate().unwrap();
        assert_eq!(project.terrain_key(), key);
        let mut history = History::default();
        history.record(legacy.clone(), &project);
        history.undo(&mut project);
        assert_eq!(project, legacy);
        history.redo(&mut project);
        assert_eq!(
            project.composition_engine.as_deref(),
            Some(papyrus_terrain::monument_composition::WATER_VERSION)
        );
        let profile = crate::profile::Profile::new("Water-aware".into(), project.clone());
        let encoded = serde_json::to_string(&profile).unwrap();
        let restored: crate::profile::Profile = serde_json::from_str(&encoded).unwrap();
        restored.validate().unwrap();
        assert_eq!(restored.recipe, project);
    }
    #[test]
    fn performance_policy_roundtrips_and_undo_preserves_base_identity() {
        let mut project = Project::default();
        let original = project.clone();
        let legacy = serde_json::to_value(&project).unwrap();
        assert!(legacy.get("performance").is_none());
        assert_eq!(serde_json::from_value::<Project>(legacy).unwrap(), original);
        project.performance.monuments = false;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("performance.mapper");
        project.save(&path).unwrap();
        assert_eq!(Project::load(&path).unwrap(), project);
        let mut history = History::default();
        history.record(original.clone(), &project);
        history.undo(&mut project);
        assert_eq!(project, original);
        history.redo(&mut project);
        assert!(!project.performance.monuments);
        assert_eq!(project.terrain_key(), original.terrain_key());
        let mut unknown = serde_json::to_value(&project).unwrap();
        unknown["performance"]["future_content"] = true.into();
        assert!(serde_json::from_value::<Project>(unknown).is_err());
    }
    #[test]
    fn recipe_roundtrip_and_replacement() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("world.mapper");
        let mut p = Project::default();
        p.save(&path).unwrap();
        p.targets[0].x = 55.;
        p.shuffle_monuments();
        p.save(&path).unwrap();
        assert_eq!(
            {
                let mut current = p.clone();
                crate::generation_defaults::apply(&mut current);
                current
            },
            Project::load(&path).unwrap()
        );
        let bytes = std::fs::read(&path).unwrap();
        p.targets[0].x = f32::NAN;
        assert!(p.save(&path).is_err());
        assert_eq!(bytes, std::fs::read(path).unwrap());
    }
    #[test]
    fn inherited_seeds_and_monument_shuffle_are_independent() {
        // Freeze the version-one derivation so future refactors cannot silently reshuffle projects.
        assert_eq!(derive_seed(12345, "Terrain"), 1_301_890_610);
        assert_eq!(derive_seed(12345, "Monuments"), 1_013_556_988);
        let mut p = Project::default();
        let terrain = p.seeds[0];
        let targets = p.targets.clone();
        p.shuffle_monuments();
        assert_eq!(p.seeds[0], terrain);
        assert_eq!(p.targets, targets);
        let monument = p.seeds[4];
        p.master_seed = 987;
        p.refresh_inherited_seeds();
        assert_eq!(p.seeds[4], monument);
        assert_ne!(p.seeds[0], terrain);
        p.validate().unwrap();
        assert_ne!(
            derive_seed(12345, "Terrain"),
            derive_seed(12345, "Monuments")
        );
    }
    #[test]
    fn rejects_invalid_identity_version_and_input() {
        let mut p = Project::default();
        p.targets[1].instance_id = p.targets[0].instance_id;
        assert!(p.validate().is_err());
        p = Project::default();
        p.seeds[0].value = 1;
        assert!(p.validate().is_err());
        p = Project::default();
        p.schema = 4;
        assert!(p.validate().is_err());
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(f.path(), b"{\"schema\":1}").unwrap();
        assert!(Project::load(f.path()).is_err());
    }
    #[test]
    fn history_restores_recipe_and_targets_as_one_transaction() {
        let mut h = History::default();
        let mut p = Project::default();
        let old = p.clone();
        p.targets[0].x = 77.;
        p.shuffle_monuments();
        h.record(old.clone(), &p);
        let edited = p.clone();
        h.undo(&mut p);
        assert_eq!(p, old);
        h.redo(&mut p);
        assert_eq!(p, edited);
        h.undo(&mut p);
        let before = p.clone();
        p.targets[0].z = 70.;
        h.record(before, &p);
        let branch = p.clone();
        h.redo(&mut p);
        assert_eq!(p, branch);
    }
    #[test]
    fn old_projects_migrate_in_memory_without_changing_original() {
        let mut legacy = serde_json::to_value(Project::default()).unwrap();
        legacy["schema"] = 1.into();
        legacy["preview_engine"] = LEGACY_PREVIEW_ENGINE.into();
        legacy.as_object_mut().unwrap().remove("placement_engine");
        for target in legacy["targets"].as_array_mut().unwrap() {
            target.as_object_mut().unwrap().remove("prefab_path");
        }
        let file = tempfile::NamedTempFile::new().unwrap();
        let bytes = serde_json::to_vec(&legacy).unwrap();
        std::fs::write(file.path(), &bytes).unwrap();
        let loaded = Project::load(file.path()).unwrap();
        assert_eq!(loaded.schema, 3);
        assert!(loaded.targets[0].prefab_path.is_some());
        assert_eq!(std::fs::read(file.path()).unwrap(), bytes);
        let mut edited = loaded.clone();
        edited
            .add_target(
                "Dome".into(),
                placement_preview::path_for_name("Dome").unwrap(),
            )
            .unwrap();
        assert_eq!(edited.targets.last().unwrap().instance_id, 4);
        let mut history = History::default();
        history.record(loaded.clone(), &edited);
        history.undo(&mut edited);
        assert_eq!(edited, loaded);
    }
    #[test]
    fn biome_changes_invalidate_only_new_preview_keys() {
        let mut p = Project::default();
        let new = p.terrain_key();
        p.seeds[1].inherited = false;
        p.seeds[1].value = 4444;
        assert_ne!(new, p.terrain_key());
        assert_eq!(new.seed, p.terrain_key().seed);
        p.preview_engine = LEGACY_PREVIEW_ENGINE.into();
        let legacy = p.terrain_key();
        p.seeds[1].value = 5555;
        assert_eq!(legacy, p.terrain_key());
        p.validate().unwrap();
    }

    #[test]
    fn source_identity_roundtrips_and_legacy_schema_two_migrates() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("source.mapper");
        let p = Project {
            installation: Some(installation::Installation {
                kind: installation::InstallationKind::Client,
                native: installation::SourceFile {
                    relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                    sha256: "a".repeat(64),
                    bytes: 128,
                },
                scene_manifest: None,
            }),
            ..Default::default()
        };
        p.save(&path).unwrap();
        assert_eq!(
            {
                let mut current = p.clone();
                crate::generation_defaults::apply(&mut current);
                current
            },
            Project::load(&path).unwrap()
        );
        let mut legacy = serde_json::to_value(Project::default()).unwrap();
        legacy["schema"] = 2.into();
        legacy.as_object_mut().unwrap().remove("installation");
        let bytes = serde_json::to_vec(&legacy).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let migrated = Project::load(&path).unwrap();
        assert_eq!(migrated.schema, 3);
        assert!(migrated.installation.is_none());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        legacy["installation"] = serde_json::to_value(p.installation).unwrap();
        std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
        assert!(Project::load(&path).is_err());
    }

    #[test]
    fn native_recipe_keys_include_source_and_preserve_backend_on_reopen() {
        let source = installation::Installation {
            kind: installation::InstallationKind::Client,
            native: installation::SourceFile {
                relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                sha256: "a".repeat(64),
                bytes: 128,
            },
            scene_manifest: None,
        };
        let mut p = Project::default();
        let independent = p.terrain_key();
        p.installation = Some(source);
        assert_eq!(independent, p.terrain_key());
        p.preview_engine = native_preview::VERSION.into();
        p.validate().unwrap();
        let native = p.terrain_key();
        assert_ne!(independent, native);
        p.installation.as_mut().unwrap().native.sha256 = "b".repeat(64);
        assert_ne!(native, p.terrain_key());
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("native.mapper");
        p.save(&path).unwrap();
        assert_eq!(
            {
                let mut current = p.clone();
                crate::generation_defaults::apply(&mut current);
                current
            },
            Project::load(&path).unwrap()
        );
        p.installation = None;
        assert!(p.validate().is_err());
    }

    #[test]
    fn ocean_recipe_is_explicit_versioned_and_keeps_legacy_native_recipes_raw() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("ocean.mapper");
        let mut p = Project {
            preview_engine: native_preview::VERSION.into(),
            installation: Some(installation::Installation {
                kind: installation::InstallationKind::Client,
                native: installation::SourceFile {
                    relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                    sha256: "a".repeat(64),
                    bytes: 128,
                },
                scene_manifest: None,
            }),
            ..Default::default()
        };
        p.save(&path).unwrap();
        assert_eq!(
            Project::load(&path).unwrap().preview_engine,
            native_preview::VERSION
        );
        let raw = p.clone();
        let mut history = History::default();
        p.preview_engine = native_preview::OCEAN_VERSION.into();
        p.validate().unwrap();
        history.record(raw.clone(), &p);
        assert_ne!(p.terrain_key(), raw.terrain_key());
        assert_eq!(
            native_preview::source_key(&p.terrain_key()),
            raw.terrain_key()
        );
        p.save(&path).unwrap();
        assert_eq!(Project::load(&path).unwrap(), {
            let mut current = p.clone();
            crate::generation_defaults::apply(&mut current);
            current
        });
        history.undo(&mut p);
        assert_eq!(p, raw);
        history.redo(&mut p);
        assert_eq!(p.preview_engine, native_preview::OCEAN_VERSION);
        assert_eq!(p.seeds, raw.seeds);
        assert_eq!(p.targets, raw.targets);
    }
    #[test]
    fn topology_strokes_roundtrip_profiles_and_undo_without_changing_terrain_identity() {
        let mut p = Project {
            preview_engine: native_preview::VERSION.into(),
            installation: Some(installation::Installation {
                kind: installation::InstallationKind::Client,
                native: installation::SourceFile {
                    relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                    sha256: "a".repeat(64),
                    bytes: 1,
                },
                scene_manifest: None,
            }),
            ..Default::default()
        };
        let key = p.terrain_key();
        let before = p.clone();
        p.topology_edits = Some(Arc::new(papyrus_terrain::topology_paint::Edits {
            engine: papyrus_terrain::topology_paint::VERSION.into(),
            source_sha256: "b".repeat(64),
            size: p.size,
            enabled: true,
            strokes: vec![papyrus_terrain::topology_paint::Stroke {
                shape: Default::default(),
                operation: papyrus_terrain::topology_paint::Operation::Add,
                mask: 2,
                radius: 25.,
                points: vec![[0., 0.], [50., 50.]],
            }],
        }));
        p.validate().unwrap();
        assert_eq!(p.terrain_key(), key);
        let snapshot = p.clone();
        assert!(Arc::ptr_eq(
            snapshot.topology_edits.as_ref().unwrap(),
            p.topology_edits.as_ref().unwrap()
        ));
        let mut disabled = snapshot.clone();
        Arc::make_mut(disabled.topology_edits.as_mut().unwrap()).enabled = false;
        assert_ne!(disabled, snapshot);
        assert!(snapshot.topology_edits.as_ref().unwrap().enabled);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("paint.mapper");
        p.save(&file).unwrap();
        assert_eq!(Project::load(&file).unwrap(), {
            let mut current = p.clone();
            crate::generation_defaults::apply(&mut current);
            current
        });
        let profile = profile::Profile::new("Painted".into(), p.clone());
        let file = dir.path().join("paint.rec");
        profile.save(&file).unwrap();
        assert_eq!(profile::Profile::load(&file).unwrap().recipe, {
            let mut current = p.clone();
            crate::generation_defaults::apply(&mut current);
            current
        });
        let mut h = History::default();
        h.record(before.clone(), &p);
        h.undo(&mut p);
        assert_eq!(p, before);
        h.redo(&mut p);
        assert!(p.topology_edits.is_some());
        assert_eq!(p.terrain_key(), key);
    }
}

#[cfg(feature = "road-diagnostics")]
pub mod road_fit_diagnostics;
#[cfg(feature = "road-diagnostics")]
mod road_height_diagnostics;
pub mod road_paint_source;
pub mod road_paint_stage;
pub mod road_planning;
#[cfg(feature = "road-diagnostics")]
pub mod road_route_diagnostics;
#[cfg(feature = "road-diagnostics")]
mod road_source_diagnostics;
pub mod road_terrain_source;
#[cfg(feature = "road-diagnostics")]
pub mod road_transaction_diagnostics;
pub mod roadside_placement;
mod roadside_rail_check;
mod roadside_road_check;
pub mod roadside_source;
pub mod roadside_stamps;
pub mod storage;
pub mod support_snapshot;

pub mod rail_texture_stage;
pub mod rail_topology_stage;
pub mod retained_rail_height;
pub mod retained_rail_scene;
pub mod road_socket_recovery;

pub mod monument_icons;

pub mod installations;

pub mod client_cache;
pub mod client_preparation;
pub mod map_export;
