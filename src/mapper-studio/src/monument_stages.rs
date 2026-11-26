//! Verified installation program data for the land monument stage gate.
use crate::{installation::SourceFile, metadata_runtime, prefab_assets::Catalog};
use papyrus_terrain::{
    monument_stage::{self, Check, Context, Program},
    native_layers::Layers,
    spawn::Filter,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
};
const LIMIT: u64 = 4 * 1024 * 1024;
const ADAPTER: &str = "papyrus-monument-scene-programs-1";
pub const HIERARCHY_VERSION: &str = "papyrus-monument-stage-filter-2";
struct CodeProfile {
    raw: &'static str,
    topology: &'static str,
    schema: u32,
    adapter: &'static str,
    engine: &'static str,
    realm_mask: i32,
}
static CODE_PROFILES: [CodeProfile; 3] = [
    CodeProfile {
        raw: metadata_runtime::MONUMENT_STAGE_CODE_PROFILE,
        topology: include_str!("../../../compatibility/topology/client-0f703d43.json"),
        schema: 1,
        adapter: ADAPTER,
        engine: monument_stage::VERSION,
        realm_mask: 2,
    },
    CodeProfile {
        raw: metadata_runtime::CURRENT_MONUMENT_STAGE_CODE_PROFILE,
        topology: include_str!("../../../compatibility/topology/client-8dd4e235.json"),
        schema: 2,
        adapter: "papyrus-monument-scene-programs-2",
        engine: HIERARCHY_VERSION,
        realm_mask: 1,
    },
    CodeProfile {
        raw: include_str!("../../../compatibility/monuments/client-58f3263f-stages.json"),
        topology: include_str!("../../../compatibility/topology/client-58f3263f.json"),
        schema: 2,
        adapter: "papyrus-monument-scene-programs-2",
        engine: HIERARCHY_VERSION,
        realm_mask: 1,
    },
];
fn select_profile(
    catalog: &Catalog,
) -> Result<Option<(&'static CodeProfile, serde_json::Value)>, String> {
    if catalog.installation.kind != crate::installation::InstallationKind::Client {
        return Ok(None);
    }
    let scene = catalog
        .sources
        .iter()
        .find(|s| s.relative_path == "Bundles/shared/assetscenes.bundle");
    for spec in &CODE_PROFILES {
        let profile: serde_json::Value =
            serde_json::from_str(spec.raw).map_err(|e| e.to_string())?;
        if profile["native_sha256"].as_str() == Some(catalog.installation.native.sha256.as_str())
            && scene.is_some_and(|s| profile["scene_sha256"].as_str() == Some(s.sha256.as_str()))
        {
            return Ok(Some((spec, profile)));
        }
    }
    Ok(None)
}
/// The current WorldSetup hierarchy gate is shared by all generation stages.
pub(crate) fn hierarchy_code_profile(catalog: &Catalog) -> Result<Option<&'static str>, String> {
    Ok(select_profile(catalog)?
        .filter(|(profile, _)| profile.schema == 2)
        .map(|(profile, _)| profile.raw))
}
pub(crate) fn capture_code_profile(catalog: &Catalog) -> Result<Option<&'static str>, String> {
    Ok(select_profile(catalog)?.map(|(profile, _)| profile.raw))
}
const ORDER: &str = "relative hierarchy MonoBehaviour order; not an official seed ordinal";
const PREFIX: &str = "assets/bundled/prefabs/autospawn/";
pub const SPACING_VERSION: &str = "papyrus-monument-minimum-spacing-1";
pub const POPULATION_VERSION: &str = "papyrus-installation-stage-population-1";
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub engine: String,
    pub source_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spacing: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub population: Option<String>,
}
impl Binding {
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(
            self.engine.as_str(),
            monument_stage::VERSION | HIERARCHY_VERSION
        ) || !hash(&self.source_sha256)
        {
            return Err("Unsupported monument stage binding".into());
        }
        if self
            .spacing
            .as_deref()
            .is_some_and(|s| s != SPACING_VERSION)
        {
            return Err("Unsupported source monument spacing policy".into());
        }
        if self
            .population
            .as_deref()
            .is_some_and(|s| s != POPULATION_VERSION)
        {
            return Err("Unsupported installation monument population policy".into());
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Stage {
    object_id: i64,
    order: usize,
    mode: i32,
    disabled: Option<bool>,
    component_enabled: bool,
    hierarchy_active: bool,
    folders: Vec<String>,
    filter: Filter,
    minimum_world_size: u32,
    nexus_only: bool,
    target_count: u32,
    target_count_curve: serde_json::Value,
    minimum_distance_same_type: u32,
    minimum_distance_different_type: u32,
    distance_same_type: u8,
    distance_different_type: u8,
}
impl Stage {
    fn program(&self) -> Program {
        Program {
            mode: self.mode,
            // Legacy snapshots carry Disabled. The hierarchy adapter records
            // that field's absence and uses the captured GameObject state.
            disabled: self.disabled.unwrap_or(!self.hierarchy_active),
            minimum_world_size: self.minimum_world_size,
            nexus_only: self.nexus_only,
            filter: self.filter.clone(),
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Other {
    object_id: i64,
    order: usize,
    kind: String,
    folders: Vec<String>,
    reason: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    format: String,
    schema: u32,
    adapter: String,
    catalog_sha256: String,
    sources: Vec<SourceFile>,
    extractor_sha256: String,
    code_profile_sha256: String,
    order_scope: String,
    stages: Vec<Stage>,
    unsupported_families: Vec<Other>,
    prefab_stage_matches: BTreeMap<String, Vec<i64>>,
    full_stage_execution_validated: bool,
}
pub struct Source {
    pub sha256: String,
    ocean_topology: u32,
    doc: Document,
    policy: &'static CodeProfile,
}
fn folders_valid(paths: &[String]) -> bool {
    !paths.is_empty()
        && paths.len() <= 32
        && paths.iter().collect::<BTreeSet<_>>().len() == paths.len()
        && paths.iter().all(|p| {
            p.starts_with(PREFIX)
                && p.ends_with('/')
                && p.len() > PREFIX.len() + 1
                && p.len() <= PREFIX.len() + 257
                && p[PREFIX.len()..p.len() - 1].split('/').all(|s| {
                    !s.is_empty()
                        && s.bytes().all(|c| {
                            c.is_ascii_lowercase() || c.is_ascii_digit() || b"_-".contains(&c)
                        })
                })
        })
}
fn curve_valid(v: &serde_json::Value) -> bool {
    let Some(o) = v.as_object() else {
        return false;
    };
    let Some(keys) = o.get("m_Curve").and_then(|v| v.as_array()) else {
        return false;
    };
    if o.len() != 4
        || !(1..=64).contains(&keys.len())
        || ["m_PreInfinity", "m_PostInfinity", "m_RotationOrder"]
            .iter()
            .any(|k| o.get(*k).and_then(|v| v.as_u64()).is_none_or(|n| n > 8))
    {
        return false;
    }
    let mut previous = f64::NEG_INFINITY;
    for key in keys {
        let Some(k) = key.as_object() else {
            return false;
        };
        if k.len() != 7
            || k.get("weightedMode")
                .and_then(|v| v.as_u64())
                .is_none_or(|n| n > 3)
        {
            return false;
        }
        for name in [
            "time",
            "value",
            "inSlope",
            "outSlope",
            "inWeight",
            "outWeight",
        ] {
            let Some(v) = k.get(name) else {
                return false;
            };
            if ["inSlope", "outSlope"].contains(&name)
                && matches!(v.as_str(), Some("positive_infinity" | "negative_infinity"))
            {
                continue;
            }
            if v.as_f64()
                .is_none_or(|v| !v.is_finite() || v.abs() > 1_000_000.)
            {
                return false;
            }
        }
        let time = k["time"].as_f64().unwrap();
        if time <= previous || k["value"].as_f64().unwrap() < 0. {
            return false;
        }
        previous = time;
    }
    true
}
fn curve_value(curve: &serde_json::Value, time: f64) -> Result<f64, String> {
    if !curve_valid(curve) || !time.is_finite() {
        return Err("Invalid source population curve".into());
    }
    let keys = curve["m_Curve"].as_array().unwrap();
    if keys.len() == 1 {
        return Ok(keys[0]["value"].as_f64().unwrap());
    }
    if let Some(key) = keys.iter().find(|k| k["time"].as_f64() == Some(time)) {
        return Ok(key["value"].as_f64().unwrap());
    }
    let pair = keys
        .windows(2)
        .find(|k| k[0]["time"].as_f64().unwrap() < time && time < k[1]["time"].as_f64().unwrap())
        .ok_or("Population curve extrapolation needs a compatibility adapter")?;
    let a = &pair[0];
    let b = &pair[1];
    if a["weightedMode"].as_u64().unwrap() & 2 != 0 || b["weightedMode"].as_u64().unwrap() & 1 != 0
    {
        return Err("Weighted population curves need a compatibility adapter".into());
    }
    let av = a["value"].as_f64().unwrap();
    let bv = b["value"].as_f64().unwrap();
    let (Some(out_slope), Some(in_slope)) = (a["outSlope"].as_f64(), b["inSlope"].as_f64()) else {
        return Ok(av);
    };
    let span = b["time"].as_f64().unwrap() - a["time"].as_f64().unwrap();
    let t = (time - a["time"].as_f64().unwrap()) / span;
    Ok((2. * t.powi(3) - 3. * t * t + 1.) * av
        + (t.powi(3) - 2. * t * t + t) * span * out_slope
        + (-2. * t.powi(3) + 3. * t * t) * bv
        + (t.powi(3) - t * t) * span * in_slope)
}
impl Source {
    pub fn binding(&self) -> Binding {
        Binding {
            engine: self.policy.engine.into(),
            source_sha256: self.sha256.clone(),
            spacing: Some(SPACING_VERSION.into()),
            population: None,
        }
    }
    /// Folder identity follows Prefab.Folder; unsupported placement families
    /// contribute their authored bounds without inventing a stage distance.
    pub fn spacing(
        &self,
        path: &str,
        bounds: &crate::prefab_assets::Bounds,
    ) -> Result<papyrus_terrain::placement::Spacing, String> {
        let family = path
            .rsplit_once('/')
            .map(|(folder, _)| folder)
            .filter(|folder| !folder.is_empty())
            .ok_or("Invalid monument prefab folder")?;
        let stage = self
            .doc
            .prefab_stage_matches
            .get(path)
            .filter(|ids| ids.len() == 1)
            .and_then(|ids| self.doc.stages.iter().find(|s| s.object_id == ids[0]));
        Ok(papyrus_terrain::placement::Spacing {
            shape: papyrus_terrain::monument_spacing::BoxShape {
                center: bounds.center,
                extent: bounds.extent,
            },
            family: family.into(),
            minimum_same: stage.map_or(0., |s| s.minimum_distance_same_type as f32),
            minimum_different: stage.map_or(0., |s| s.minimum_distance_different_type as f32),
            entrance: None,
        })
    }
    pub fn program_count(&self) -> usize {
        self.doc.stages.len()
    }
    pub fn unsupported_family_count(&self) -> usize {
        self.doc.unsupported_families.len()
    }
    /// Join the early terrain phase to the same ordered, installation-bound
    /// monument programs used for placement. Folder names carry no phase rule.
    pub(crate) fn paths_for_prefix(
        &self,
        expected: &[i64],
        receipts: &[SourceFile],
    ) -> Result<Vec<String>, String> {
        if receipts.iter().any(|r| !self.doc.sources.contains(r)) {
            return Err("Early monument programs belong to another Rust installation".into());
        }
        // This snapshot counts hierarchy MonoBehaviours. The procedural workflow
        // counts only procedural components. Their numeric ordinals differ.
        let mut ordered: Vec<_> = self.doc.stages.iter().collect();
        ordered.sort_by_key(|stage| stage.order);
        let actual = ordered.iter().take(expected.len()).map(|stage| stage.object_id).collect::<Vec<_>>();
        if actual != expected {
            return Err("Early monument program order differs from the erosion workflow".into());
        }
        let ids: BTreeSet<_> = expected.iter().copied().collect();
        self.doc.prefab_stage_matches.iter()
            .filter(|(_, stages)| stages.iter().any(|id| ids.contains(id)))
            .map(|(path, stages)| {
                if stages.len() != 1 {
                    Err("Early monument has an ambiguous source placement phase".into())
                } else {
                    Ok(path.clone())
                }
            }).collect()
    }
    pub(crate) fn groups_for_prefix(
        &self,
        expected: &[i64],
        receipts: &[SourceFile],
    ) -> Result<Vec<Vec<String>>, String> {
        let paths = self.paths_for_prefix(expected, receipts)?;
        Ok(expected
            .iter()
            .map(|id| {
                paths
                    .iter()
                    .filter(|path| {
                        self.doc
                            .prefab_stage_matches
                            .get(*path)
                            .is_some_and(|stages| stages == &[*id])
                    })
                    .cloned()
                    .collect()
            })
            .collect())
    }

    /// Smallest map where the selected installation admits a family variant.
    pub fn landmark_minimum(
        &self,
        catalog: &Catalog,
        family: crate::recipe_options::Landmark,
    ) -> Option<u32> {
        if catalog.sha256 != self.doc.catalog_sha256 {
            return None;
        }
        self.doc.prefab_stage_matches.iter().filter_map(|(path, ids)| {
            if crate::recipe_options::Landmark::for_path(path) != Some(family) {
                return None;
            }
            let [id] = ids.as_slice() else { return None };
            let stage = self.doc.stages.iter().find(|stage| stage.object_id == *id)?;
            if stage.program().disabled || stage.mode & self.policy.realm_mask == 0 || stage.nexus_only {
                return None;
            }
            let bounds = catalog.entries.get(path)?.bounds.as_ref()?;
            Some(bounds.minimum_world_size.max(stage.minimum_world_size))
        }).min()
    }

    /// Source stage candidates, before terrain, biome and placement checks.
    /// Zero TargetCount means uncapped, exactly as in PlaceMonuments.Process.
    pub fn population(
        &self,
        catalog: &Catalog,
        size: u32,
    ) -> Result<Vec<(String, i64, Option<usize>)>, String> {
        self.population_with_landmarks(catalog, size, |_| false)
    }

    pub(crate) fn population_with_landmarks(
        &self,
        catalog: &Catalog,
        size: u32,
        force: impl Fn(&str) -> bool,
    ) -> Result<Vec<(String, i64, Option<usize>)>, String> {
        if catalog.sha256 != self.doc.catalog_sha256 {
            return Err("Population catalog differs from installation stages".into());
        }
        let mut result = Vec::new();
        let mut stages: Vec<_> = self.doc.stages.iter().collect();
        stages.sort_by_key(|s| s.order);
        for stage in stages {
            // Same source adapter realm used by the placement evaluator below.
            if stage.program().disabled
                || stage.mode & self.policy.realm_mask == 0
                || stage.nexus_only
            {
                continue;
            }
            let mut paths: Vec<_> = catalog
                .entries
                .iter()
                .filter(|(path, asset)| {
                    self.doc
                        .prefab_stage_matches
                        .get(*path)
                        .is_some_and(|ids| ids == &[stage.object_id])
                        && (size >= stage.minimum_world_size || force(path))
                        && asset
                            .bounds
                            .as_ref()
                            .is_some_and(|b| size >= b.minimum_world_size)
                })
                .map(|(p, _)| p.clone())
                .collect();
            if paths.is_empty() {
                continue;
            }
            let maximum = if stage.target_count == 0 {
                None
            } else {
                let count = (stage.target_count as f64
                    * curve_value(&stage.target_count_curve, size.max(stage.minimum_world_size) as f64)?)
                .round_ties_even();
                if !count.is_finite() || count < 0. {
                    return Err("Invalid source monument target count".into());
                }
                (count > 0. || size < stage.minimum_world_size)
                    .then_some((count as usize).clamp(1, papyrus_terrain::placement::MAX_TARGETS))
            };
            paths.sort();
            result.extend(paths.into_iter().map(|p| (p, stage.object_id, maximum)));
        }
        Ok(result)
    }
    /// Decode numeric metadata and parent identities. Use load/cached to verify
    /// the additional gameplay-code and map-scene files against a Rust folder.
    pub fn decode(raw: &[u8], catalog: &Catalog) -> Result<Self, String> {
        if raw.len() as u64 > LIMIT {
            return Err("Monument stage source exceeds 4 MiB".into());
        }
        let doc: Document = serde_json::from_slice(raw).map_err(|e| e.to_string())?;
        let (policy, profile) = select_profile(catalog)?
            .ok_or("No source-matched monument stage adapter is available")?;
        let topology: serde_json::Value =
            serde_json::from_str(policy.topology).map_err(|e| e.to_string())?;
        if topology["native_sha256"] != profile["native_sha256"]
            || topology["scene_sha256"] != profile["scene_sha256"]
        {
            return Err("Monument topology roles belong to another installation".into());
        }
        let ocean_topology = topology["flags"]["Ocean"]
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .filter(|v| v.is_power_of_two())
            .ok_or("Missing monument Ocean topology role")?;
        let mut expected = catalog.sources.clone();
        expected
            .push(serde_json::from_value(profile["map_scene"].clone()).map_err(|e| e.to_string())?);
        expected.extend(
            serde_json::from_value::<Vec<SourceFile>>(profile["code_sources"].clone())
                .map_err(|e| e.to_string())?,
        );
        if doc.format != "papyrus-monument-stage-source"
            || doc.schema != policy.schema
            || doc.adapter != policy.adapter
            || doc.catalog_sha256 != catalog.sha256
            || doc.sources != expected
            || !crate::source_profile::matches(policy.raw, &doc.code_profile_sha256)
            || !hash(&doc.extractor_sha256)
            || doc.order_scope != ORDER
            || doc.full_stage_execution_validated
            || !(1..=64).contains(&doc.stages.len())
            || doc.unsupported_families.len() > 64
        {
            return Err("Monument stage source differs from its catalog or adapter".into());
        }
        let mut ids = BTreeSet::new();
        let mut orders = BTreeSet::new();
        for stage in &doc.stages {
            if stage.disabled.is_none() != (policy.schema == 2) {
                return Err("Monument activation fields differ from the source adapter".into());
            }
            stage.program().validate()?;
            if stage.object_id <= 0
                || !ids.insert(stage.object_id)
                || stage.order > 65535
                || !orders.insert(stage.order)
                || !folders_valid(&stage.folders)
                || !curve_valid(&stage.target_count_curve)
                || stage.target_count > 4096
                || stage.minimum_distance_same_type > 10000
                || stage.minimum_distance_different_type > 10000
                || stage.distance_same_type > 2
                || stage.distance_different_type > 2
            {
                return Err("Invalid monument stage program".into());
            }
            // Behaviour.enabled is provenance for both adapters. The current
            // adapter gates on hierarchy_active through Stage::program.
            let _ = stage.component_enabled;
        }
        for other in &doc.unsupported_families {
            if other.object_id <= 0
                || !ids.insert(other.object_id)
                || other.order > 65535
                || !orders.insert(other.order)
                || !other.kind.starts_with("PlaceMonuments")
                || other.kind.len() > 128
                || !folders_valid(&other.folders)
                || other.reason.is_empty()
                || other.reason.len() > 512
            {
                return Err("Invalid separate monument family".into());
            }
        }
        let matches = catalog
            .entries
            .keys()
            .map(|p| {
                (
                    p.clone(),
                    doc.stages
                        .iter()
                        .filter(|s| s.folders.iter().any(|f| p.starts_with(f)))
                        .map(|s| s.object_id)
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        if matches != doc.prefab_stage_matches {
            return Err("Monument stage selectors do not match catalog paths".into());
        }
        Ok(Self {
            sha256: format!("{:x}", Sha256::digest(raw)),
            ocean_topology,
            doc,
            policy,
        })
    }
    fn metadata(
        path: &Path,
        catalog: &Catalog,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        let mut raw = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(LIMIT + 1)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
        if cancelled() {
            return Err("Monument stage verification cancelled".into());
        }
        Self::decode(&raw, catalog)
    }
    pub fn load(
        path: &Path,
        root: &Path,
        catalog: &Catalog,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        let source = Self::metadata(path, catalog, &cancelled)?;
        source.verify_installation(root, catalog, &cancelled)?;
        Ok(source)
    }

    pub(crate) fn verify_installation(
        &self,
        root: &Path,
        catalog: &Catalog,
        cancelled: &impl Fn() -> bool,
    ) -> Result<(), String> {
        for receipt in self.doc.sources.iter().skip(catalog.sources.len()) {
            if crate::installation::hash_file(
                root,
                &receipt.relative_path,
                1024 * 1024 * 1024,
                cancelled,
            )? != *receipt
            {
                return Err("Monument program gameplay sources changed".into());
            }
        }
        Ok(())
    }
    /// Bounded recipe-planning read only. Placement must still use `cached`,
    /// which verifies the gameplay and scene files before accepting a layout.
    pub fn cached_metadata(
        binding: &Binding,
        catalog: &Catalog,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        binding.validate()?;
        let path = crate::client_cache::require_pinned_document(
            &crate::storage::local_data()?,
            "papyrus-monument-stage-cache",
            &binding.source_sha256,
            "stages.json",
            "monument stage",
        )?;
        Self::bound_metadata(&path, binding, catalog, cancelled)
    }
    fn bound_metadata(
        path: &Path,
        binding: &Binding,
        catalog: &Catalog,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        binding.validate()?;
        let source = Self::metadata(path, catalog, cancelled)?;
        if source.sha256 != binding.source_sha256 {
            return Err("Pinned monument program bytes changed".into());
        }
        if source.policy.engine != binding.engine {
            return Err("Monument stage binding names another activation policy".into());
        }
        Ok(source)
    }
    pub fn cached(
        binding: &Binding,
        root: &Path,
        catalog: &Catalog,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        binding.validate()?;
        let path = crate::client_cache::require_pinned_document(
            &crate::storage::local_data()?,
            "papyrus-monument-stage-cache",
            &binding.source_sha256,
            "stages.json",
            "monument stage",
        )?;
        let source = Self::load(&path, root, catalog, cancelled)?;
        if source.sha256 != binding.source_sha256 {
            return Err("Pinned monument program bytes changed".into());
        }
        if source.policy.engine != binding.engine {
            return Err("Monument stage binding names another activation policy".into());
        }
        Ok(source)
    }
    pub fn anchor_filter(&self, path: &str) -> Option<&papyrus_terrain::spawn::Filter> {
        let ids = self.doc.prefab_stage_matches.get(path)?;
        if ids.len() != 1 {
            return None;
        }
        self.doc
            .stages
            .iter()
            .find(|s| s.object_id == ids[0])
            .map(|s| &s.filter)
    }
    /// Ocean-only stages place on the seafloor unless their root decorators
    /// move them to the water surface. Keep the authored stage gate in charge.
    pub fn requires_ocean(&self, path: &str) -> bool {
        self.anchor_filter(path).is_some_and(|filter| {
            filter.topology_not as u32 & self.ocean_topology == 0
                && (filter.topology_all as u32 & self.ocean_topology != 0
                    || filter.topology_any as u32 == self.ocean_topology)
        })
    }
    pub fn evaluate(
        &self,
        layers: &Layers,
        path: &str,
        seed: u32,
        id: u64,
        point: [f32; 2],
    ) -> Result<Check, String> {
        self.evaluate_with_landmark_preference(layers, path, seed, id, point, false)
    }

    pub(crate) fn evaluate_with_landmark_preference(
        &self,
        layers: &Layers,
        path: &str,
        seed: u32,
        id: u64,
        point: [f32; 2],
        force: bool,
    ) -> Result<Check, String> {
        let Some(ids) = self.doc.prefab_stage_matches.get(path) else {
            return Ok(Check::Skipped("Prefab has no captured monument stage"));
        };
        if ids.len() != 1 {
            return Ok(Check::Skipped(
                "Prefab needs a separate or ambiguous placement-family adapter",
            ));
        }
        let stage = self
            .doc
            .stages
            .iter()
            .find(|s| s.object_id == ids[0])
            .ok_or("Missing captured monument program")?;
        let mut h = 0x811c9dc5_u32;
        for b in b"papyrus-monument-stage-draw-1"
            .iter()
            .copied()
            .chain(seed.to_le_bytes())
            .chain(id.to_le_bytes())
            .chain(
                point
                    .into_iter()
                    .flat_map(|v| (if v == 0. { 0_f32 } else { v }).to_bits().to_le_bytes()),
            )
        {
            h = (h ^ u32::from(b)).wrapping_mul(16777619);
        }
        // Independent coordinate-keyed draw in (0,1]. Pinning an accepted pose
        // cannot change its draw by changing a trial counter or target ordering.
        let draw = ((h >> 8) + 1) as f32 / 16_777_216.;
        let mut program = stage.program();
        if force {
            program.minimum_world_size = 0;
        }
        monument_stage::evaluate(
            layers,
            &program,
            Context {
                realm_mask: self.policy.realm_mask,
                cached: false,
                nexus: false,
            },
            point,
            draw,
        )
    }
}

pub fn prepare(
    root: &Path,
    catalog: &Catalog,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Source>, String> {
    let Some((policy, _)) = select_profile(catalog)? else {
        return Ok(None);
    };
    let local = crate::storage::local_data()?;
    let cache = local.join("papyrus-monument-stage-cache");
    let current = metadata_runtime::monument_stage_extractor_sha256();
    if cache.is_dir() {
        for (i, e) in std::fs::read_dir(&cache)
            .map_err(|e| e.to_string())?
            .enumerate()
        {
            if i >= 256 {
                return Err("Too many monument program snapshots".into());
            }
            if cancelled() {
                return Ok(None);
            }
            let path = e.map_err(|e| e.to_string())?.path().join("stages.json");
            if !path.is_file() {
                continue;
            }
            let mut raw = Vec::new();
            std::fs::File::open(&path)
                .map_err(|e| e.to_string())?
                .take(LIMIT + 1)
                .read_to_end(&mut raw)
                .map_err(|e| e.to_string())?;
            if let Ok(source) = Source::decode(&raw, catalog)
                && source.doc.extractor_sha256 == current
                && path
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|p| p.to_str())
                    == Some(&source.sha256)
            {
                return Source::load(&path, root, catalog, &cancelled).map(Some);
            }
        }
    }
    let catalog_path =
        crate::asset_discovery::catalog_path(&local.join("papyrus-prefab-cache"), &catalog.sha256)?;
    std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
    let temporary = tempfile::Builder::new()
        .prefix(".monument-stages-")
        .tempdir_in(&cache)
        .map_err(|e| e.to_string())?;
    let profile = temporary.path().join("code-profile.json");
    std::fs::write(&profile, policy.raw).map_err(|e| e.to_string())?;
    let output = temporary.path().join("output");
    if !metadata_runtime::run(
        metadata_runtime::Script::MonumentStages,
        &[
            "--rust-root".into(),
            root.as_os_str().into(),
            "--catalog".into(),
            catalog_path.into_os_string(),
            "--code-profile".into(),
            profile.into_os_string(),
            "--cache-root".into(),
            output.as_os_str().into(),
        ],
        &cancelled,
    )? {
        return Ok(None);
    }
    let mut paths = std::fs::read_dir(&output)
        .map_err(|e| e.to_string())?
        .map(|e| e.map(|e| e.path().join("stages.json")))
        .collect::<Result<Vec<PathBuf>, _>>()
        .map_err(|e| e.to_string())?;
    if paths.len() != 1 {
        return Err("Monument program extractor did not publish one snapshot".into());
    }
    let path = paths.pop().unwrap();
    let source = Source::load(&path, root, catalog, &cancelled)?;
    if cancelled() {
        return Ok(None);
    }
    let destination = cache.join(&source.sha256);
    if destination.exists() {
        let existing = Source::load(&destination.join("stages.json"), root, catalog, &cancelled)?;
        if existing.sha256 != source.sha256 {
            return Err("Existing monument program cache changed".into());
        }
    } else {
        std::fs::rename(
            path.parent().ok_or("Missing stage output directory")?,
            &destination,
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(Some(source))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stage_receipts_accept_only_known_profile_line_endings() {
        for (catalog, mut doc) in [fixture(), current_fixture()] {
            let (profile, _) = select_profile(&catalog).unwrap().unwrap();
            for hash in crate::source_profile::hashes(profile.raw) {
                doc["code_profile_sha256"] = hash.into();
                assert!(Source::decode(&serde_json::to_vec(&doc).unwrap(), &catalog).is_ok());
            }
            doc["code_profile_sha256"] = "f".repeat(64).into();
            assert!(Source::decode(&serde_json::to_vec(&doc).unwrap(), &catalog).is_err());
        }
    }

    #[test]
    fn bundled_stage_profiles_require_the_matching_client_and_scene() {
        let (mut catalog, _) = crate::infrastructure::tests::fixture();
        for spec in &CODE_PROFILES {
            let value: serde_json::Value = serde_json::from_str(spec.raw).unwrap();
            catalog.installation.native.sha256 = value["native_sha256"].as_str().unwrap().into();
            catalog.sources.iter_mut()
                .find(|source| source.relative_path == "Bundles/shared/assetscenes.bundle")
                .unwrap().sha256 = value["scene_sha256"].as_str().unwrap().into();
            assert_eq!(capture_code_profile(&catalog).unwrap(), Some(spec.raw));
            assert_eq!(
                hierarchy_code_profile(&catalog).unwrap(),
                (spec.schema == 2).then_some(spec.raw)
            );
            let topology: serde_json::Value = serde_json::from_str(spec.topology).unwrap();
            assert_eq!(topology["native_sha256"], value["native_sha256"]);
            assert_eq!(topology["scene_sha256"], value["scene_sha256"]);
            catalog.installation.kind = crate::installation::InstallationKind::DedicatedServer;
            assert!(capture_code_profile(&catalog).unwrap().is_none());
            catalog.installation.kind = crate::installation::InstallationKind::Client;
            catalog.installation.native.sha256 = "e".repeat(64);
            assert!(capture_code_profile(&catalog).unwrap().is_none());
            catalog.installation.native.sha256 = value["native_sha256"].as_str().unwrap().into();
            catalog.sources.iter_mut()
                .find(|source| source.relative_path == "Bundles/shared/assetscenes.bundle")
                .unwrap().sha256 = "e".repeat(64);
            assert!(capture_code_profile(&catalog).unwrap().is_none());
        }
    }

    #[test]
    fn early_paths_require_matching_scene_programs_and_unambiguous_membership() {
        for (catalog, doc) in [fixture(), current_fixture()] {
            let mut source = Source::decode(&serde_json::to_vec(&doc).unwrap(), &catalog).unwrap();
            let stage = &source.doc.stages[0];
            let expected = [stage.object_id];
            let receipts = source.doc.sources.clone();
            let paths = source.paths_for_prefix(&expected, &receipts).unwrap();
            assert!(!paths.is_empty());
            assert_eq!(source.groups_for_prefix(&expected, &receipts).unwrap().as_slice(), std::slice::from_ref(&paths));
            assert!(source.groups_for_prefix(&[], &receipts).unwrap().is_empty());
            assert!(source.paths_for_prefix(&[999], &receipts).is_err());
            let mut wrong_receipts = receipts.clone();
            wrong_receipts[0].sha256 = "0".repeat(64);
            assert!(source.paths_for_prefix(&expected, &wrong_receipts).is_err());
            assert!(source.paths_for_prefix(&[], &receipts).unwrap().is_empty());
            source.doc.stages[0].order += 1000;
            assert_eq!(source.paths_for_prefix(&expected, &receipts).unwrap(), paths);
            let mut earlier: Stage = serde_json::from_value(doc["stages"][0].clone()).unwrap();
            earlier.object_id = 999;
            earlier.order = source.doc.stages[0].order - 1;
            source.doc.stages.push(earlier);
            assert!(source.paths_for_prefix(&expected, &receipts).is_err());
            source.doc.stages.pop();
            source.doc.prefab_stage_matches.get_mut(&paths[0]).unwrap().push(999);
            assert!(source.paths_for_prefix(&expected, &receipts).is_err());
        }
    }
    #[test]
    #[ignore = "requires an owned installation, cached catalog and bundled metadata runtime"]
    fn owned_stage_preparation_replays_the_capture_and_reuses_the_cache() {
        let path = |name| PathBuf::from(std::env::var_os(name).expect(name));
        let root = path("MAPPER_STAGE_TEST_ROOT");
        let _leases = crate::source_lease::Scope::default();
        let installation = crate::installation::inspect(&root, || false).unwrap();
        let catalog = crate::prefab_assets::load(
            &root,
            &path("MAPPER_STAGE_TEST_CATALOG"),
            &installation,
            || false,
        )
        .unwrap();
        let expected =
            Source::load(&path("MAPPER_STAGE_TEST_SOURCE"), &root, &catalog, || false).unwrap();
        let prepared = prepare(&root, &catalog, || false).unwrap().unwrap();
        assert_eq!(prepared.sha256, expected.sha256);
        assert_eq!(prepared.binding(), expected.binding());
        let cached = prepare(&root, &catalog, || false).unwrap().unwrap();
        assert_eq!(cached.sha256, expected.sha256);
        assert_eq!(
            cached.population(&catalog, 5000).unwrap(),
            expected.population(&catalog, 5000).unwrap()
        );
    }
    #[test]
    #[ignore = "requires an owned Rust installation and source-bound stage capture"]
    fn owned_stage_capture_loads_and_plans_against_the_selected_installation() {
        let path = |name| PathBuf::from(std::env::var_os(name).expect(name));
        let root = path("MAPPER_STAGE_TEST_ROOT");
        let _leases = crate::source_lease::Scope::default();
        let installation = crate::installation::inspect(&root, || false).unwrap();
        let catalog = crate::prefab_assets::load(
            &root,
            &path("MAPPER_STAGE_TEST_CATALOG"),
            &installation,
            || false,
        )
        .unwrap();
        let source =
            Source::load(&path("MAPPER_STAGE_TEST_SOURCE"), &root, &catalog, || false).unwrap();
        source.binding().validate().unwrap();
        let mut sizes = BTreeMap::new();
        for size in [1000, 2000, 3000, 4000, 5000, 6000] {
            sizes.insert(size, source.population(&catalog, size).unwrap());
        }
        let report = serde_json::json!({
            "format":"mapper-owned-monument-stage-verification", "schema":1,
            "source_sha256":source.sha256, "engine":source.binding().engine,
            "programs":source.program_count(), "separate_families":source.unsupported_family_count(),
            "realm_mask":source.policy.realm_mask, "candidates_by_size":sizes,
            "game_code_executed":false, "full_stage_execution_validated":false,
        });
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path("MAPPER_STAGE_TEST_REPORT"))
            .unwrap();
        serde_json::to_writer_pretty(file, &report).unwrap();
    }
    fn current_fixture() -> (Catalog, serde_json::Value) {
        let (mut catalog, mut doc) = fixture();
        let raw = metadata_runtime::CURRENT_MONUMENT_STAGE_CODE_PROFILE;
        let profile: serde_json::Value = serde_json::from_str(raw).unwrap();
        catalog.sources[0].sha256 = profile["native_sha256"].as_str().unwrap().into();
        catalog.installation.native = catalog.sources[0].clone();
        catalog.sources[2].sha256 = profile["scene_sha256"].as_str().unwrap().into();
        let mut sources = catalog.sources.clone();
        sources.push(serde_json::from_value(profile["map_scene"].clone()).unwrap());
        sources.extend(
            serde_json::from_value::<Vec<SourceFile>>(profile["code_sources"].clone()).unwrap(),
        );
        doc["sources"] = serde_json::to_value(sources).unwrap();
        doc["schema"] = 2.into();
        doc["adapter"] = "papyrus-monument-scene-programs-2".into();
        doc["code_profile_sha256"] = format!("{:x}", Sha256::digest(raw.as_bytes())).into();
        doc["stages"][0]["disabled"] = serde_json::Value::Null;
        doc["stages"][0]["hierarchy_active"] = true.into();
        doc["stages"][0]["component_enabled"] = false.into();
        doc["stages"][0]["mode"] = 1.into();
        for asset in catalog.entries.values_mut() {
            asset.bounds = Some(crate::prefab_assets::Bounds {
                center: [0.; 3],
                extent: [10.; 3],
                minimum_world_size: 0,
            });
        }
        (catalog, doc)
    }
    fn layers() -> Layers {
        layers_for_size(1000)
    }
    fn layers_for_size(size: u32) -> Layers {
        let (hr, n) = papyrus_terrain::native_layers::dimensions(size).unwrap();
        let mut layers = Layers {
            size,
            terrain_seed: 1,
            biome_seed: 1,
            height_resolution: hr,
            surface_resolution: n,
            height: vec![18000; hr * hr],
            biome: vec![0; n * n * 5],
            splat: vec![0; n * n * 8],
            topology: vec![536870912; n * n],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        layers.biome[3 * n * n..4 * n * n].fill(255);
        layers
    }
    #[test]
    fn current_activation_uses_hierarchy_and_client_realm_for_population_and_evaluation() {
        let (catalog, mut doc) = current_fixture();
        let layers = layers();
        let path = "assets/bundled/prefabs/autospawn/monument/arctic_bases/test.prefab";
        for mode in [-1, 0, 1, 2, 3] {
            for active in [false, true] {
                doc["stages"][0]["mode"] = mode.into();
                doc["stages"][0]["hierarchy_active"] = active.into();
                let source = Source::decode(&serde_json::to_vec(&doc).unwrap(), &catalog).unwrap();
                assert_eq!(source.binding().engine, HIERARCHY_VERSION);
                let expected = active && mode & 1 != 0;
                assert_eq!(
                    !source.population(&catalog, 1000).unwrap().is_empty(),
                    expected
                );
                assert_eq!(
                    matches!(
                        source.evaluate(&layers, path, 1, 17, [0.; 2]).unwrap(),
                        Check::Accepted { .. }
                    ),
                    expected
                );
            }
        }
    }
    #[test]
    fn stage_adapter_rejects_cross_version_fields_receipts_and_binding_names() {
        let (catalog, doc) = current_fixture();
        for (pointer, value) in [
            ("/schema", serde_json::json!(1)),
            ("/adapter", serde_json::json!(ADAPTER)),
            ("/stages/0/disabled", serde_json::json!(false)),
            ("/code_profile_sha256", serde_json::json!("c".repeat(64))),
            ("/sources/4/sha256", serde_json::json!("c".repeat(64))),
        ] {
            let mut bad = doc.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(
                Source::decode(&serde_json::to_vec(&bad).unwrap(), &catalog).is_err(),
                "{pointer}"
            );
        }
        let raw = serde_json::to_vec(&doc).unwrap();
        let source = Source::decode(&raw, &catalog).unwrap();
        let mut binding = source.binding();
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("stages.json");
        std::fs::write(&path, raw).unwrap();
        assert!(Source::bound_metadata(&path, &binding, &catalog, || false).is_ok());
        binding.engine = monument_stage::VERSION.into();
        assert!(Source::bound_metadata(&path, &binding, &catalog, || false).is_err());
        let (old_catalog, mut old) = fixture();
        old["stages"][0]["disabled"] = serde_json::Value::Null;
        assert!(Source::decode(&serde_json::to_vec(&old).unwrap(), &old_catalog).is_err());
    }
    #[test]
    fn planning_snapshot_is_content_bound_and_does_not_require_game_file_reads() {
        let (catalog, doc) = fixture();
        let raw = serde_json::to_vec(&doc).unwrap();
        let binding = Source::decode(&raw, &catalog).unwrap().binding();
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("stages.json");
        std::fs::write(&path, &raw).unwrap();
        assert_eq!(
            Source::bound_metadata(&path, &binding, &catalog, || false)
                .unwrap()
                .sha256,
            binding.source_sha256
        );
        let mut changed = doc;
        changed["stages"][0]["target_count"] = serde_json::json!(2);
        std::fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(
            Source::bound_metadata(&path, &binding, &catalog, || false)
                .err()
                .unwrap()
                .contains("bytes changed")
        );
        assert!(Source::bound_metadata(&path, &binding, &catalog, || true).is_err());
    }
    #[test]
    fn population_uses_scene_limits_size_and_activation_without_offshore_fallback() {
        let (mut catalog, mut doc) = fixture();
        for asset in catalog.entries.values_mut() {
            asset.bounds = Some(crate::prefab_assets::Bounds {
                center: [0.; 3],
                extent: [10.; 3],
                minimum_world_size: 4000,
            });
        }
        let decode = |d: &serde_json::Value| {
            Source::decode(&serde_json::to_vec(d).unwrap(), &catalog).unwrap()
        };
        assert!(decode(&doc).population(&catalog, 3999).unwrap().is_empty());
        let pool = decode(&doc).population(&catalog, 4000).unwrap();
        assert_eq!(pool.len(), 1);
        assert_eq!(pool[0].1, 1);
        assert_eq!(pool[0].2, Some(1));
        assert!(!pool[0].0.contains("offshore"));
        doc["stages"][0]["target_count"] = serde_json::json!(0);
        assert_eq!(decode(&doc).population(&catalog, 4000).unwrap()[0].2, None);
        doc["stages"][0]["disabled"] = serde_json::json!(true);
        assert!(decode(&doc).population(&catalog, 4000).unwrap().is_empty());
    }
    #[test]
    fn requested_landmark_can_try_a_3000m_map_without_skipping_terrain_filter() {
        let (mut catalog, mut doc) = fixture();
        let path = "assets/bundled/prefabs/autospawn/unique_environment/oasis/ue_oasis_a.prefab";
        let original = "assets/bundled/prefabs/autospawn/monument/arctic_bases/test.prefab";
        let mut asset = catalog.entries.remove(original).unwrap();
        asset.bounds = Some(crate::prefab_assets::Bounds {
            center: [0.; 3],
            extent: [10.; 3],
            minimum_world_size: 0,
        });
        catalog.entries.insert(path.to_owned(), asset);
        let matches = doc["prefab_stage_matches"].as_object_mut().unwrap();
        let stage_ids = matches.remove(original).unwrap();
        matches.insert(path.into(), stage_ids);
        doc["stages"][0]["folders"] = serde_json::json!([format!("{}/", path.rsplit_once('/').unwrap().0)]);
        doc["stages"][0]["minimum_world_size"] = serde_json::json!(4000);
        let source = Source::decode(&serde_json::to_vec(&doc).unwrap(), &catalog).unwrap();
        assert!(source.population(&catalog, 3000).unwrap().is_empty());

        let mut options = crate::recipe_options::Options { oasis: Some(true), ..Default::default() };
        let population = source.population_with_landmarks(&catalog, 3000, |path| {
            options.prefers_landmark(path)
        }).unwrap();
        assert_eq!(population, [(path.to_owned(), 1, Some(1))]);

        let mut layers = layers_for_size(3000);
        assert!(matches!(source.evaluate(&layers, path, 1, 17, [0.; 2]).unwrap(), Check::Skipped(_)));
        let evaluate = |layers: &Layers| {
            source.evaluate_with_landmark_preference(layers, path, 1, 17, [0.; 2],
                options.prefers_landmark(path)).unwrap()
        };
        assert!(matches!(evaluate(&layers), Check::Accepted { .. }));
        layers.biome.fill(0);
        assert!(matches!(evaluate(&layers), Check::FilterRejected { .. }));
        options.oasis = Some(false);
        assert!(options.landmark_blocker(path).is_some());
        assert!(!options.prefers_landmark(path));
    }

    #[test]
    fn population_curve_preserves_smooth_and_step_transitions() {
        let (_, doc) = fixture();
        let mut curve = doc["stages"][0]["target_count_curve"].clone();
        curve["m_Curve"][0]["value"] = serde_json::json!(0.5);
        curve["m_Curve"][1]["inSlope"] = serde_json::json!(0.);
        assert_eq!(curve_value(&curve, 3500.).unwrap(), 0.75);
        curve["m_Curve"][0]["outSlope"] = serde_json::json!("positive_infinity");
        assert_eq!(curve_value(&curve, 5999.).unwrap(), 0.5);
        assert_eq!(curve_value(&curve, 6000.).unwrap(), 1.);
        assert!(curve_value(&curve, 999.).is_err());
        curve["m_Curve"][0]["weightedMode"] = serde_json::json!(2);
        assert!(curve_value(&curve, 3500.).is_err());
    }
    use crate::{
        installation::{Installation, InstallationKind},
        prefab_assets::Asset,
    };
    fn fixture() -> (Catalog, serde_json::Value) {
        let doc: serde_json::Value = serde_json::from_slice(include_bytes!(
            "../tests/fixtures/monument-stage-source.json"
        ))
        .unwrap();
        let sources: Vec<SourceFile> = serde_json::from_value(doc["sources"].clone()).unwrap();
        let catalog = Catalog {
            sha256: "a".repeat(64),
            installation: Installation {
                kind: InstallationKind::Client,
                native: sources[0].clone(),
                scene_manifest: Some(sources[1].clone()),
            },
            sources: sources[..3].to_vec(),
            entries: doc["prefab_stage_matches"]
                .as_object()
                .unwrap()
                .keys()
                .map(|p| {
                    (
                        p.clone(),
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
                    )
                })
                .collect(),
        };
        (catalog, doc)
    }
    #[test]
    fn submerged_admission_requires_an_unambiguous_ocean_stage_for_each_installation() {
        for (catalog, doc) in [fixture(), current_fixture()] {
            let path = doc["prefab_stage_matches"]
                .as_object()
                .unwrap()
                .keys()
                .next()
                .unwrap();
            for (any, all, excluded, expected) in [
                (-1, 0, 0, false),
                (-1, 128 | 262144, 256, true),
                (128, 0, 0, true),
                (128 | 1, 0, 0, false),
                (-1, 262144, 0, false),
                (-1, 128, 128, false),
            ] {
                let mut changed = doc.clone();
                changed["stages"][0]["filter"]["TopologyAny"] = any.into();
                changed["stages"][0]["filter"]["TopologyAll"] = all.into();
                changed["stages"][0]["filter"]["TopologyNot"] = excluded.into();
                let source =
                    Source::decode(&serde_json::to_vec(&changed).unwrap(), &catalog).unwrap();
                assert_eq!(source.requires_ocean(path), expected);
                assert!(!source.requires_ocean("unknown.prefab"));
            }
            let mut source =
                Source::decode(&serde_json::to_vec(&doc).unwrap(), &catalog).unwrap();
            source.doc.prefab_stage_matches.get_mut(path).unwrap().push(999);
            assert!(!source.requires_ocean(path));
        }
    }
    #[test]
    fn strict_stage_decode_checks_parent_receipts_selectors_and_program_contract() {
        let (catalog, doc) = fixture();
        let decode =
            |d: &serde_json::Value| Source::decode(&serde_json::to_vec(d).unwrap(), &catalog);
        let source = decode(&doc).unwrap();
        assert_eq!(source.program_count(), 1);
        assert_eq!(source.unsupported_family_count(), 1);
        source.binding().validate().unwrap();
        assert_eq!(source.sha256, decode(&doc).unwrap().sha256);
        for (pointer, value) in [
            ("/catalog_sha256", serde_json::json!("c".repeat(64))),
            ("/sources/3/bytes", serde_json::json!(1)),
            ("/code_profile_sha256", serde_json::json!("c".repeat(64))),
            ("/stages/0/filter/BiomeType", serde_json::json!(0)),
            ("/stages/0/mode", serde_json::json!(4)),
            ("/stages/0/folders", serde_json::json!([PREFIX])),
            ("/stages/0/target_count", serde_json::json!(4097)),
            (
                "/stages/0/target_count_curve/m_Curve/1/inSlope",
                serde_json::json!("unknown"),
            ),
            ("/full_stage_execution_validated", serde_json::json!(true)),
        ] {
            let mut bad = doc.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(decode(&bad).is_err(), "{pointer}");
        }
        let mut bad = doc;
        *bad["prefab_stage_matches"]
            .as_object_mut()
            .unwrap()
            .values_mut()
            .next()
            .unwrap() = serde_json::json!([999]);
        assert!(decode(&bad).is_err());
    }
    #[test]
    fn spacing_uses_installed_stage_distances_and_prefab_folders_with_legacy_opt_out() {
        let (catalog, doc) = fixture();
        let source = Source::decode(&serde_json::to_vec(&doc).unwrap(), &catalog).unwrap();
        let bounds = crate::prefab_assets::Bounds {
            center: [15., 20., -30.],
            extent: [60., 40., 10.],
            minimum_world_size: 1000,
        };
        let path = "assets/bundled/prefabs/autospawn/monument/arctic_bases/test.prefab";
        let spacing = source.spacing(path, &bounds).unwrap();
        assert_eq!(spacing.family, path.rsplit_once('/').unwrap().0);
        assert_eq!(spacing.shape.center, bounds.center);
        assert_eq!(spacing.shape.extent, bounds.extent);
        assert_eq!(
            spacing.minimum_same,
            doc["stages"][0]["minimum_distance_same_type"]
                .as_u64()
                .unwrap() as f32
        );
        assert_eq!(
            spacing.minimum_different,
            doc["stages"][0]["minimum_distance_different_type"]
                .as_u64()
                .unwrap() as f32
        );
        let unknown = source
            .spacing(
                "assets/bundled/prefabs/autospawn/monument/offshore/test.prefab",
                &bounds,
            )
            .unwrap();
        assert_eq!([unknown.minimum_same, unknown.minimum_different], [0.; 2]);
        let fresh = source.binding();
        assert_eq!(fresh.spacing.as_deref(), Some(SPACING_VERSION));
        let mut raw = serde_json::to_value(&fresh).unwrap();
        raw.as_object_mut().unwrap().remove("spacing");
        let legacy: Binding = serde_json::from_value(raw).unwrap();
        legacy.validate().unwrap();
        assert!(legacy.spacing.is_none());
        let mut bad = fresh;
        bad.spacing = Some("unknown".into());
        assert!(bad.validate().is_err());
    }
    #[test]
    fn inactive_provenance_does_not_disable_stage_and_coordinate_draw_replays() {
        let (catalog, doc) = fixture();
        let source = Source::decode(&serde_json::to_vec(&doc).unwrap(), &catalog).unwrap();
        let mut layers = layers();
        let path = "assets/bundled/prefabs/autospawn/monument/arctic_bases/test.prefab";
        let check = source.evaluate(&layers, path, 1, 17, [0.; 2]).unwrap();
        assert!(matches!(check, Check::Accepted { .. }));
        assert_eq!(
            check,
            source.evaluate(&layers, path, 1, 17, [-0., 0.]).unwrap()
        );
        assert_eq!(
            check,
            source.evaluate(&layers, path, 1, 17, [0.; 2]).unwrap()
        );
        assert_ne!(
            check,
            source.evaluate(&layers, path, 2, 17, [0.; 2]).unwrap()
        );
        assert!(matches!(
            source
                .evaluate(
                    &layers,
                    "assets/bundled/prefabs/autospawn/monument/offshore/test.prefab",
                    1,
                    17,
                    [0.; 2]
                )
                .unwrap(),
            Check::Skipped(_)
        ));
        layers.biome.fill(0);
        assert!(matches!(
            source.evaluate(&layers, path, 1, 17, [0.; 2]).unwrap(),
            Check::FilterRejected { .. }
        ));
    }
    #[test]
    fn stage_binding_roundtrips_and_legacy_recipes_omit_it() {
        let (catalog, doc) = fixture();
        let source = Source::decode(&serde_json::to_vec(&doc).unwrap(), &catalog).unwrap();
        let recipe = crate::monument_source::Recipe {
            engine: crate::monument_source::VERSION.into(),
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
            stage_rules: Some(source.binding()),
            dungeon_source: None, offshore_source: None,
        };
        recipe.validate().unwrap();
        let mut saved = serde_json::to_value(&recipe).unwrap();
        assert_eq!(recipe, serde_json::from_value(saved.clone()).unwrap());
        saved.as_object_mut().unwrap().remove("stage_rules");
        let old: crate::monument_source::Recipe = serde_json::from_value(saved.clone()).unwrap();
        old.validate().unwrap();
        assert_eq!(serde_json::to_value(old).unwrap(), saved);
        let mut bad = source.binding();
        bad.engine = "unknown".into();
        assert!(bad.validate().is_err());
        bad = source.binding();
        bad.source_sha256 = "../bad".into();
        assert!(bad.validate().is_err());
    }
}
