//! Installation-bound roadside stage policy. Metadata preparation alone does not
//! enable placement; the staged generation/selection adapter is a separate step.
use crate::{installation::SourceFile, metadata_runtime, prefab_assets::Catalog};
use papyrus_terrain::{
    monument_stage::Context,
    roadside_monuments::{RoadMode, Settings},
    spawn::Filter,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
};
const LIMIT: u64 = 4 * 1024 * 1024;
const ORDER: &str = "relative hierarchy MonoBehaviour order; not an official seed ordinal";
const SOCKET_ORDER: &str = "root-first-depth-first-component-order";
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    format: String,
    schema: u32,
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stage_layout: Option<String>,
    pub sources: Vec<SourceFile>,
    proposal_engine: String,
    pub road_infrastructure_type: i32,
    pub trail_infrastructure_type: i32,
    pub sample_spacing: f32,
    pub end_padding: f32,
    pub group_attempts: usize,
    pub individual_candidates: usize,
}
impl Profile {
    fn for_catalog(catalog: &Catalog) -> Result<Option<(&'static str, Self)>, String> {
        for raw in [
            metadata_runtime::ROADSIDE_PROFILE,
            metadata_runtime::CURRENT_ROADSIDE_PROFILE,
            include_str!("../../../compatibility/monuments/client-58f3263f-roadside.json"),
        ] {
            let profile: Self = serde_json::from_str(raw).map_err(|e| e.to_string())?;
            if catalog.sources.as_slice() == &profile.sources[..3]
                && catalog.installation.kind == crate::installation::InstallationKind::Client
                && catalog.installation.native == profile.sources[0]
            {
                return Ok(Some((raw, profile)));
            }
        }
        Ok(None)
    }
    pub fn settings(&self, maximum_work: usize, maximum_candidates: usize) -> Settings {
        Settings {
            sample_spacing: self.sample_spacing,
            end_padding: self.end_padding,
            maximum_work,
            maximum_candidates,
        }
    }
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stage {
    pub object_id: i64,
    pub order: usize,
    pub mode: i32,
    #[serde(deserialize_with = "Option::deserialize")]
    pub disabled: Option<bool>,
    component_enabled: bool,
    hierarchy_active: bool,
    pub folders: Vec<String>,
    pub filter: Filter,
    pub minimum_world_size: u32,
    pub target_count: usize,
    pub minimum_distance_same_type: u32,
    pub minimum_distance_different_type: u32,
    pub distance_same_type: u8,
    pub distance_different_type: u8,
    road_mode: u8,
}
impl Stage {
    pub fn road_mode(&self) -> Result<RoadMode, String> {
        Ok(match self.road_mode {
            0 => RoadMode::SideRoadOrRingRoad,
            1 => RoadMode::SideRoad,
            2 => RoadMode::RingRoad,
            3 => RoadMode::SideRoadOrDesireTrail,
            4 => RoadMode::DesireTrail,
            _ => return Err("Invalid captured roadside mode".into()),
        })
    }
    /// Source ShouldRun gates. Stored Unity enabled/active flags are provenance.
    /// This stage has no land-style probabilistic root filter query.
    pub fn runs(&self, size: u32, context: Context) -> Result<bool, String> {
        if !(1..=3).contains(&context.realm_mask) || !(1000..=6000).contains(&size) {
            return Err("Invalid roadside execution context".into());
        }
        Ok(!self.disabled.unwrap_or(!self.hierarchy_active)
            && !context.cached
            && self.mode & context.realm_mask != 0
            && size >= self.minimum_world_size)
    }
}
#[derive(Debug, PartialEq, Eq, Deserialize)]
enum Kind {
    GenerateRoadRing,
    GenerateRoadLayout,
    PlaceMonumentsRoadside,
    GenerateRoadTopology,
    GenerateRoadTerrain,
    GenerateRoadTexture,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Sequence {
    kind: Kind,
    object_id: i64,
    order: usize,
    mode: i32,
    #[serde(deserialize_with = "Option::deserialize")]
    disabled: Option<bool>,
    component_enabled: bool,
    hierarchy_active: bool,
    #[serde(default)]
    infrastructure_type: Option<i32>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Connector {
    pub object_id: i64,
    pub game_object_id: i64,
    pub infrastructure_type: i32,
    pub position: [f32; 3],
    pub forward: [f32; 3],
    pub hierarchy_active_without_root: bool,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Sockets {
    version: u32,
    space: String,
    complete: bool,
    connectors: Vec<Connector>,
    issues: Vec<serde_json::Value>,
    nodes_visited: usize,
    connector_order: String,
    #[serde(default)]
    dungeon_components: Option<DungeonComponents>,
    #[serde(default)]
    water_map_enabled: Option<bool>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DungeonComponents {
    pub links: Vec<i64>,
    pub entrances: Vec<i64>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Parameters {
    pub priority: i32,
    pub count: usize,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prefab {
    pub parameters: Parameters,
    sockets: Sockets,
}
impl Prefab {
    pub fn water_map_enabled(&self) -> Option<bool> {
        self.sockets.water_map_enabled
    }
    pub fn dungeon_components(&self) -> Option<&DungeonComponents> {
        self.sockets.dungeon_components.as_ref()
    }
    pub fn road_connectors(&self, profile: &Profile) -> impl Iterator<Item = &Connector> {
        let road = profile.road_infrastructure_type;
        self.sockets
            .connectors
            .iter()
            .filter(move |c| c.infrastructure_type == road)
    }
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    format: String,
    schema: u32,
    adapter: String,
    catalog_sha256: String,
    sources: Vec<SourceFile>,
    profile: Profile,
    profile_sha256: String,
    extractor_sha256: String,
    order_scope: String,
    stages: Vec<Stage>,
    road_sequence: Vec<Sequence>,
    prefabs: BTreeMap<String, Prefab>,
    full_stage_execution_validated: bool,
}
pub struct Source {
    pub sha256: String,
    doc: Document,
}
#[derive(Clone, Copy)]
pub enum PaintStage {
    Topology,
    Terrain,
    Texture,
}
impl Source {
    pub fn profile_sha256(&self) -> &str {
        &self.doc.profile_sha256
    }
    /// Fresh-map stage gates. Cached-world restoration is a separate pipeline;
    /// it is not Mapper's immutable native terrain-variant cache.
    pub fn paint_stage_runs(&self, stage: PaintStage, context: Context) -> Result<bool, String> {
        if !(1..=3).contains(&context.realm_mask) || context.cached {
            return Err("Invalid fresh-map painting context".into());
        }
        let index = match stage {
            PaintStage::Topology => 4,
            PaintStage::Terrain => 5,
            PaintStage::Texture => 6,
        };
        let s = &self.doc.road_sequence[index];
        Ok(!s.disabled.unwrap_or(!s.hierarchy_active) && s.mode & context.realm_mask != 0)
    }
    pub fn catalog_sha256(&self) -> &str {
        &self.doc.catalog_sha256
    }
    pub fn profile(&self) -> &Profile {
        &self.doc.profile
    }
    pub fn stage(&self) -> &Stage {
        &self.doc.stages[0]
    }
    pub fn prefabs(&self) -> &BTreeMap<String, Prefab> {
        &self.doc.prefabs
    }
    /// Validates numeric metadata and identities. load/cached additionally verify
    /// gameplay and map receipts against the selected installation.
    pub fn decode(raw: &[u8], catalog: &Catalog) -> Result<Self, String> {
        if raw.len() as u64 > LIMIT {
            return Err("Roadside source exceeds 4 MiB".into());
        }
        let doc: Document =
            serde_json::from_slice(raw).map_err(|e| format!("Invalid roadside metadata: {e}"))?;
        let (profile_raw, known) =
            Profile::for_catalog(catalog)?.ok_or("Unsupported roadside installation")?;
        let current = known.stage_layout.as_deref() == Some("mode-description");
        if doc.format != "papyrus-roadside-monument-source"
            || (doc.schema, doc.adapter.as_str())
                != if current {
                    (2, "papyrus-roadside-programs-2")
                } else {
                    (1, "papyrus-roadside-programs-1")
                }
            || doc.profile != known
            || !crate::source_profile::matches(profile_raw, &doc.profile_sha256)
            || doc.sources != known.sources
            || catalog.sources.as_slice() != &known.sources[..3]
            || catalog.installation.kind != crate::installation::InstallationKind::Client
            || catalog.installation.native != known.sources[0]
            || doc.catalog_sha256 != catalog.sha256
            || !hash(&doc.extractor_sha256)
            || doc.order_scope != ORDER
            || doc.full_stage_execution_validated
            || doc.stages.len() != 1
            || doc.road_sequence.len() != 7
            || !(1..=128).contains(&doc.prefabs.len())
        {
            return Err("Roadside metadata does not match its catalog or supported adapter".into());
        }
        let stage = &doc.stages[0];
        stage.filter.validate()?;
        stage.road_mode()?;
        let prefix = "assets/bundled/prefabs/autospawn/";
        if stage.object_id <= 0
            || stage.disabled.is_none() != current
            || stage.order > 65535
            || !(-1..=3).contains(&stage.mode)
            || stage.minimum_world_size > 6000
            || stage.target_count > 4096
            || stage.minimum_distance_same_type > 10000
            || stage.minimum_distance_different_type > 10000
            || stage.distance_same_type > 2
            || stage.distance_different_type > 2
            || !(1..=32).contains(&stage.folders.len())
            || stage.folders.iter().any(|f| {
                !f.starts_with(prefix)
                    || !f.ends_with('/')
                    || f.len() <= prefix.len() + 1
                    || f.len() > 512
                    || f.bytes().any(|b| {
                        !b.is_ascii_lowercase() && !b.is_ascii_digit() && !b"_-/".contains(&b)
                    })
                    || f[prefix.len()..f.len() - 1]
                        .split('/')
                        .any(|s| s.is_empty() || s == "." || s == "..")
            })
            || stage.folders.iter().collect::<BTreeSet<_>>().len() != stage.folders.len()
        {
            return Err("Invalid roadside stage policy".into());
        }
        let expected_kinds = [
            Kind::GenerateRoadRing,
            Kind::GenerateRoadLayout,
            Kind::GenerateRoadLayout,
            Kind::PlaceMonumentsRoadside,
            Kind::GenerateRoadTopology,
            Kind::GenerateRoadTerrain,
            Kind::GenerateRoadTexture,
        ];
        let mut ids = BTreeSet::new();
        let mut last = None;
        for (index, (s, kind)) in doc.road_sequence.iter().zip(expected_kinds).enumerate() {
            let infrastructure = match index {
                1 => Some(known.road_infrastructure_type),
                2 => Some(known.trail_infrastructure_type),
                _ => None,
            };
            if s.kind != kind
                || s.disabled.is_none() != current
                || s.object_id <= 0
                || !ids.insert(s.object_id)
                || s.order > 65535
                || last.is_some_and(|n| s.order <= n)
                || !(-1..=3).contains(&s.mode)
                || s.infrastructure_type != infrastructure
            {
                return Err("Unsupported roadside generation sequence".into());
            }
            last = Some(s.order);
        }
        let s = &doc.road_sequence[3];
        if (
            s.object_id,
            s.order,
            s.mode,
            s.disabled,
            s.component_enabled,
            s.hierarchy_active,
        ) != (
            stage.object_id,
            stage.order,
            stage.mode,
            stage.disabled,
            stage.component_enabled,
            stage.hierarchy_active,
        ) {
            return Err("Roadside sequence contradicts its placement stage".into());
        }
        let expected_paths: BTreeSet<_> = catalog
            .entries
            .keys()
            .filter(|p| stage.folders.iter().any(|f| p.starts_with(f)))
            .collect();
        if expected_paths != doc.prefabs.keys().collect() {
            return Err("Roadside selectors do not match catalog paths".into());
        }
        for (path, p) in &doc.prefabs {
            let sockets = &p.sockets;
            if !(0..=4).contains(&p.parameters.priority)
                || p.parameters.count > 4096
                || catalog.entries[path].priority != Some(p.parameters.priority)
                || sockets.version != 3
                || sockets.space != "prefab-root-local-unity"
                || !sockets.complete
                || !sockets.issues.is_empty()
                || !(1..=65536).contains(&sockets.nodes_visited)
                || sockets.connector_order != SOCKET_ORDER
                || !(1..=256).contains(&sockets.connectors.len())
            {
                return Err("Invalid roadside prefab parameters or ordered sockets".into());
            }
            let mut connections = BTreeSet::new();
            let mut owners = BTreeSet::new();
            for c in &sockets.connectors {
                let length = c.forward.iter().map(|v| v * v).sum::<f32>().sqrt();
                if c.object_id <= 0
                    || c.game_object_id <= 0
                    || !connections.insert(c.object_id)
                    || !(0..=64).contains(&c.infrastructure_type)
                    || c.position
                        .iter()
                        .chain(&c.forward)
                        .any(|v| !v.is_finite() || v.abs() > 10000.)
                    || !(0.99..=1.01).contains(&length)
                    || (c.infrastructure_type == known.road_infrastructure_type
                        && !owners.insert(c.game_object_id))
                {
                    return Err(
                        "Roadside sockets require a pose or runtime-ordering adapter".into(),
                    );
                }
            }
            if owners.is_empty() {
                return Err("Roadside prefab has no captured road socket".into());
            }
            if let Some(d) = &sockets.dungeon_components
                && (d.links.len() + d.entrances.len() > 4096
                    || d.links
                        .iter()
                        .chain(&d.entrances)
                        .any(|&id| id <= 0 || !connections.insert(id)))
            {
                return Err("Invalid roadside dungeon component inventory".into());
            }
        }
        Ok(Self {
            sha256: digest(raw),
            doc,
        })
    }
    fn load_inner(
        path: &Path,
        root: &Path,
        catalog: &Catalog,
        pin: Option<&str>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        if cancelled() {
            return Err("Roadside preparation cancelled".into());
        }
        let mut raw = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(LIMIT + 1)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
        if pin.is_some_and(|p| digest(&raw) != p) {
            return Err("Pinned roadside source bytes changed".into());
        }
        let source = Self::decode(&raw, catalog)?;
        for receipt in source.doc.sources.iter().skip(3) {
            if crate::installation::hash_file(
                root,
                &receipt.relative_path,
                1024 * 1024 * 1024,
                &cancelled,
            )? != *receipt
            {
                return Err("Roadside gameplay or map source changed".into());
            }
        }
        if cancelled() {
            return Err("Roadside preparation cancelled".into());
        }
        Ok(source)
    }
    pub fn load(
        path: &Path,
        root: &Path,
        catalog: &Catalog,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        Self::load_inner(path, root, catalog, None, cancelled)
    }
    pub fn cached(
        sha: &str,
        root: &Path,
        catalog: &Catalog,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        if !hash(sha) {
            return Err("Invalid roadside source pin".into());
        }
        let path = crate::client_cache::pinned_document(
            &crate::storage::local_data()?,
            "papyrus-roadside-stage-cache",
            sha,
            "roadside.json",
        )?;
        Self::load_inner(&path, root, catalog, Some(sha), cancelled)
    }
}

/// Prepare verified metadata without activating generation or changing a recipe.
pub fn prepare(
    root: &Path,
    catalog: &Catalog,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Source>, String> {
    if cancelled() {
        return Ok(None);
    }
    if crate::coastal_placement::Profile::available(root, catalog, &cancelled)?.is_none() {
        return Ok(None);
    }
    let Some((profile_raw, _)) = Profile::for_catalog(catalog)? else {
        return Ok(None);
    };
    let local = crate::storage::local_data()?;
    let cache = local.join("papyrus-roadside-stage-cache");
    let extractor = metadata_runtime::roadside_extractor_sha256();
    if cache.is_dir() {
        for (index, entry) in std::fs::read_dir(&cache)
            .map_err(|e| e.to_string())?
            .enumerate()
        {
            if index >= 256 {
                return Err("Too many roadside source snapshots".into());
            }
            if cancelled() {
                return Ok(None);
            }
            let path = entry
                .map_err(|e| e.to_string())?
                .path()
                .join("roadside.json");
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
                && source.doc.extractor_sha256 == extractor
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
    let tmp = tempfile::Builder::new()
        .prefix(".roadside-stage-")
        .tempdir_in(&cache)
        .map_err(|e| e.to_string())?;
    let profile = tmp.path().join("profile.json");
    std::fs::write(&profile, profile_raw).map_err(|e| e.to_string())?;
    let output = tmp.path().join("output");
    if !metadata_runtime::run(
        metadata_runtime::Script::RoadsideStages,
        &[
            "--rust-root".into(),
            root.as_os_str().into(),
            "--catalog".into(),
            catalog_path.into_os_string(),
            "--profile".into(),
            profile.into_os_string(),
            "--cache-root".into(),
            output.as_os_str().into(),
        ],
        &cancelled,
    )? {
        return Ok(None);
    }
    let paths = std::fs::read_dir(&output)
        .map_err(|e| e.to_string())?
        .map(|e| e.map(|e| e.path().join("roadside.json")))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    if paths.len() != 1 {
        return Err("Roadside extractor did not publish exactly one source".into());
    }
    let source = Source::load(&paths[0], root, catalog, &cancelled)?;
    if cancelled() {
        return Ok(None);
    }
    let destination = cache.join(&source.sha256);
    if destination.exists() {
        let existing = Source::load(
            &destination.join("roadside.json"),
            root,
            catalog,
            &cancelled,
        )?;
        if existing.sha256 != source.sha256 {
            return Err("Existing roadside cache changed".into());
        }
    } else {
        std::fs::rename(
            paths[0].parent().ok_or("Missing roadside output parent")?,
            &destination,
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(Some(source))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        installation::{Installation, InstallationKind},
        prefab_assets::Asset,
    };
    use serde_json::{Value, json};
    fn fixture() -> (Catalog, Value) {
        let doc: Value = serde_json::from_slice(include_bytes!(
            "../tests/fixtures/roadside-stage-source.json"
        ))
        .unwrap();
        let receipts: Vec<SourceFile> = serde_json::from_value(doc["sources"].clone()).unwrap();
        let entries = doc["prefabs"]
            .as_object()
            .unwrap()
            .keys()
            .map(|p| {
                (
                    p.clone(),
                    Asset {
                        priority: Some(2),
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
            .collect();
        (
            Catalog {
                sha256: "a".repeat(64),
                installation: Installation {
                    kind: InstallationKind::Client,
                    native: receipts[0].clone(),
                    scene_manifest: Some(receipts[1].clone()),
                },
                sources: receipts[..3].to_vec(),
                entries,
            },
            doc,
        )
    }
    fn parse(doc: &Value, catalog: &Catalog) -> Result<Source, String> {
        Source::decode(&serde_json::to_vec(doc).unwrap(), catalog)
    }
    #[test]
    fn saved_roadside_preference_preserves_the_source_roster_and_rejects_stale_occurrences() {
        use crate::{
            road_generation::RoadsidePreference,
            roadside_placement::{captured_requests, captured_requests_with_preferences},
        };
        use papyrus_terrain::placement::Solution;

        let (catalog, document) = fixture();
        let source = parse(&document, &catalog).unwrap();
        let initial = Solution { outcomes: vec![], required_missing: 0, attempts: 0 };
        let before = captured_requests(&source, &initial).unwrap();
        let group = before.iter().find(|group| !group.requests.is_empty()).unwrap();
        let preference = RoadsidePreference {
            request: group.requests[0].id,
            prefab: group.path.clone(),
            position: [334., 950.],
            yaw: 225.,
            max_displacement_m: None,
            presence: None,
        };
        assert!(crate::roadside_placement::is_captured_request_id(&preference.prefab, preference.request));
        let mut policy = crate::road_generation::Policy::for_new_recipe(source.sha256.clone(), None, false).unwrap();
        policy.roadside_preferences.push(preference.clone());
        let raw = serde_json::to_vec(&policy).unwrap();
        let roundtrip: crate::road_generation::Policy = serde_json::from_slice(&raw).unwrap();
        roundtrip.validate().unwrap();
        assert_eq!(roundtrip, policy);
        let after = captured_requests_with_preferences(&source, &initial, &roundtrip.roadside_preferences).unwrap();
        for (before, after) in before.iter().zip(&after) {
            assert_eq!(before.path, after.path);
            assert_eq!(before.requests.len(), after.requests.len());
            for (before, after) in before.requests.iter().zip(&after.requests) {
                assert_eq!(before.id, after.id);
                assert_eq!(before.required, after.required);
                assert_eq!(before.excluded, after.excluded);
                if after.id == preference.request {
                    let moved = after.preference.unwrap();
                    assert_eq!(moved.position, preference.position);
                    assert_eq!(moved.yaw, Some(preference.yaw));
                    assert!(!moved.exact);
                } else {
                    assert!(after.preference.is_none());
                }
            }
        }
        policy.roadside_preferences.push(preference.clone());
        assert!(policy.validate().is_err());
        assert!(captured_requests_with_preferences(&source, &initial, &policy.roadside_preferences).is_err());
        let mut stale = preference;
        stale.request = 1;
        assert!(captured_requests_with_preferences(&source, &initial, &[stale]).is_err());
    }

    #[test]
    fn roadside_edits_exclude_one_occurrence_and_preserve_siblings() {
        use crate::{Presence, road_generation::RoadsidePreference, roadside_placement::*};
        let (catalog, mut document) = fixture();
        let path = document["prefabs"].as_object().unwrap().keys().next().unwrap().clone();
        document["prefabs"][&path]["parameters"]["count"] = 3.into();
        let source = parse(&document, &catalog).unwrap();
        let initial = papyrus_terrain::placement::Solution { outcomes: vec![], required_missing: 0, attempts: 0 };
        let before = captured_requests(&source, &initial).unwrap();
        let group = before.iter().find(|group| group.path == path).unwrap();
        let deleted = group.requests[1].id;
        let added = added_request_id(&path, |_| false).unwrap();
        let edit = |request, presence| RoadsidePreference {
            request, prefab: path.clone(), position: [100., -300.], yaw: 45., max_displacement_m: None, presence: Some(presence),
        };
        let after = captured_requests_with_preferences(&source, &initial, &[
            edit(deleted, Presence::Excluded), edit(added, Presence::Required),
        ]).unwrap();
        let after = &after.iter().find(|group| group.path == path).unwrap().requests;
        assert_eq!(after.iter().map(|request| request.id).collect::<Vec<_>>(),
            group.requests.iter().map(|request| request.id).chain([added]).collect::<Vec<_>>());
        assert_eq!(after.iter().filter(|request| request.excluded).map(|request| request.id).collect::<Vec<_>>(), [deleted]);
        assert!(after[3].required);
        assert_eq!(after[3].preference.unwrap().position, [100., -300.]);
    }

    #[test]
    fn current_source_requires_matching_profile_version_and_explicit_null_flags() {
        let (mut catalog, mut doc) = fixture();
        let profile: Profile =
            serde_json::from_str(metadata_runtime::CURRENT_ROADSIDE_PROFILE).unwrap();
        catalog.sources = profile.sources[..3].to_vec();
        catalog.installation.native = profile.sources[0].clone();
        catalog.installation.scene_manifest = Some(profile.sources[1].clone());
        doc["schema"] = 2.into();
        doc["adapter"] = "papyrus-roadside-programs-2".into();
        doc["profile"] = serde_json::to_value(&profile).unwrap();
        doc["sources"] = serde_json::to_value(&profile.sources).unwrap();
        doc["profile_sha256"] =
            digest(metadata_runtime::CURRENT_ROADSIDE_PROFILE.as_bytes()).into();
        doc["stages"][0]["disabled"] = Value::Null;
        for stage in doc["road_sequence"].as_array_mut().unwrap() {
            stage["disabled"] = Value::Null;
        }
        let context = Context {
            realm_mask: 2,
            cached: false,
            nexus: false,
        };
        let source = parse(&doc, &catalog).unwrap();
        assert!(source.stage().disabled.is_none());
        // The fixture's inactive legacy hierarchy now controls the current gate.
        assert!(!source.stage().runs(4000, context).unwrap());
        doc["stages"][0]["hierarchy_active"] = true.into();
        doc["road_sequence"][3]["hierarchy_active"] = true.into();
        assert!(
            parse(&doc, &catalog)
                .unwrap()
                .stage()
                .runs(4000, context)
                .unwrap()
        );
        for index in 4..=6 {
            doc["road_sequence"][index]["hierarchy_active"] = true.into();
        }
        let source = parse(&doc, &catalog).unwrap();
        for stage in [
            PaintStage::Topology,
            PaintStage::Terrain,
            PaintStage::Texture,
        ] {
            assert!(source.paint_stage_runs(stage, context).unwrap());
        }
        let paint_sha = crate::road_paint_source::Profile::for_source(&source)
            .unwrap().unwrap().sha256().unwrap();
        for hash in crate::source_profile::hashes(metadata_runtime::CURRENT_ROADSIDE_PROFILE) {
            doc["profile_sha256"] = hash.into();
            let variant = parse(&doc, &catalog).unwrap();
            assert_eq!(
                crate::road_paint_source::Profile::for_source(&variant)
                    .unwrap().unwrap().sha256().unwrap(),
                paint_sha
            );
        }
        for pointer in ["/schema", "/profile/stage_layout", "/sources/3/sha256"] {
            let mut bad = doc.clone();
            *bad.pointer_mut(pointer).unwrap() = json!("unknown");
            assert!(parse(&bad, &catalog).is_err(), "{pointer}");
        }
        for index in 0..7 {
            let mut bad = doc.clone();
            bad["road_sequence"][index]["disabled"] = false.into();
            assert!(parse(&bad, &catalog).is_err());
            bad["road_sequence"][index]
                .as_object_mut()
                .unwrap()
                .remove("disabled");
            assert!(parse(&bad, &catalog).is_err());
        }
        let mut bad = doc.clone();
        bad["stages"][0].as_object_mut().unwrap().remove("disabled");
        assert!(parse(&bad, &catalog).is_err());
        bad = doc.clone();
        bad["schema"] = 1.into();
        bad["adapter"] = "papyrus-roadside-programs-1".into();
        assert!(parse(&bad, &catalog).is_err());
    }

    #[test]
    #[ignore = "requires an owned installation and its verified catalog"]
    fn owned_preparation_reopens_source_bound_roadside_programs() {
        let path = |name| std::path::PathBuf::from(std::env::var_os(name).expect(name));
        let root = path("MAPPER_ROADSIDE_TEST_ROOT");
        let _leases = crate::source_lease::Scope::default();
        let installation = crate::installation::inspect(&root, || false).unwrap();
        let catalog = crate::prefab_assets::load(
            &root,
            &path("MAPPER_ROADSIDE_TEST_CATALOG"),
            &installation,
            || false,
        )
        .unwrap();
        let source = prepare(&root, &catalog, || false).unwrap().unwrap();
        let reopened = Source::cached(&source.sha256, &root, &catalog, || false).unwrap();
        assert_eq!(source.sha256, reopened.sha256);
        assert_eq!(source.doc.stages.len(), 1);
        assert_eq!(source.doc.road_sequence.len(), 7);
        let paint = crate::road_paint_source::Profile::for_source(&source)
            .unwrap()
            .unwrap();
        let terrain = crate::road_terrain_source::Profile::for_paint(&paint)
            .unwrap()
            .unwrap();
        let connectors: usize = source
            .prefabs()
            .values()
            .map(|p| p.sockets.connectors.len())
            .sum();
        let report = json!({"source_sha256":source.sha256,"profile":source.profile().id,"prefabs":source.prefabs().len(),"connectors":connectors,"reopened":true,"paint_profile":paint.id,"paint_sha256":paint.sha256().unwrap(),"terrain_profile":terrain.id,"terrain_sha256":terrain.sha256().unwrap()});
        std::fs::write(
            path("MAPPER_ROADSIDE_TEST_REPORT"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
    }
    #[test]
    fn source_rules_counts_order_and_actual_should_run_gates_are_retained() {
        let (catalog, doc) = fixture();
        let source = parse(&doc, &catalog).unwrap();
        let context = Context {
            realm_mask: 2,
            cached: false,
            nexus: false,
        };
        assert!(source.stage().runs(4000, context).unwrap());
        assert!(!source.stage().component_enabled && !source.stage().hierarchy_active);
        assert!(
            !source
                .stage()
                .runs(
                    4000,
                    Context {
                        cached: true,
                        ..context
                    }
                )
                .unwrap()
        );
        assert_eq!(
            source.stage().road_mode().unwrap(),
            RoadMode::SideRoadOrRingRoad
        );
        assert_eq!(source.stage().minimum_distance_same_type, 500);
        assert_eq!(source.stage().minimum_distance_different_type, 50);
        let p = source.prefabs().values().next().unwrap();
        assert_eq!(p.parameters.count, 3);
        assert_eq!(
            p.road_connectors(source.profile())
                .map(|c| c.object_id)
                .collect::<Vec<_>>(),
            vec![2, 1]
        );
        let settings = source.profile().settings(1000, 100);
        assert_eq!(settings.sample_spacing, 5.);
        assert_eq!(settings.end_padding, 5.);
        let mut changed = doc.clone();
        changed["stages"][0]["disabled"] = true.into();
        changed["road_sequence"][3]["disabled"] = true.into();
        assert!(
            !parse(&changed, &catalog)
                .unwrap()
                .stage()
                .runs(4000, context)
                .unwrap()
        );
        let mut changed = doc.clone();
        changed["stages"][0]["minimum_world_size"] = 5000.into();
        assert!(
            !parse(&changed, &catalog)
                .unwrap()
                .stage()
                .runs(4000, context)
                .unwrap()
        );
        assert!(
            source
                .stage()
                .runs(
                    4000,
                    Context {
                        realm_mask: 0,
                        ..context
                    }
                )
                .is_err()
        );
    }
    #[test]
    fn painting_gates_keep_captured_modes_disabled_flags_and_fresh_world_scope() {
        let (catalog, doc) = fixture();
        let source = parse(&doc, &catalog).unwrap();
        let context = Context {
            realm_mask: 2,
            cached: false,
            nexus: false,
        };
        for stage in [
            PaintStage::Topology,
            PaintStage::Terrain,
            PaintStage::Texture,
        ] {
            assert!(source.paint_stage_runs(stage, context).unwrap());
            assert!(
                source
                    .paint_stage_runs(
                        stage,
                        Context {
                            cached: true,
                            ..context
                        }
                    )
                    .is_err()
            );
        }
        for (index, stage) in [
            (4, PaintStage::Topology),
            (5, PaintStage::Terrain),
            (6, PaintStage::Texture),
        ] {
            let mut changed = doc.clone();
            changed["road_sequence"][index]["disabled"] = true.into();
            assert!(
                !parse(&changed, &catalog)
                    .unwrap()
                    .paint_stage_runs(stage, context)
                    .unwrap()
            );
            changed["road_sequence"][index]["disabled"] = false.into();
            changed["road_sequence"][index]["mode"] = 1.into();
            assert!(
                !parse(&changed, &catalog)
                    .unwrap()
                    .paint_stage_runs(stage, context)
                    .unwrap()
            );
        }
    }
    #[test]
    fn parent_adapter_and_sequence_changes_are_rejected_without_fallbacks() {
        let (catalog, doc) = fixture();
        for pointer in [
            "/profile/sample_spacing",
            "/profile/road_infrastructure_type",
            "/profile/group_attempts",
        ] {
            let mut d = doc.clone();
            *d.pointer_mut(pointer).unwrap() = 99.into();
            assert!(parse(&d, &catalog).is_err());
        }
        for (pointer, value) in [
            ("/catalog_sha256", json!("c".repeat(64))),
            ("/profile_sha256", json!("c".repeat(64))),
            ("/sources/3/sha256", json!("c".repeat(64))),
            ("/road_sequence/2/infrastructure_type", json!(0)),
            ("/road_sequence/3/order", json!(0)),
            ("/stages/0/road_mode", json!(5)),
            (
                "/stages/0/folders",
                json!(["assets/bundled/prefabs/autospawn/"]),
            ),
            ("/full_stage_execution_validated", json!(true)),
        ] {
            let mut d = doc.clone();
            *d.pointer_mut(pointer).unwrap() = value;
            assert!(parse(&d, &catalog).is_err(), "{pointer}");
        }
        let mut d = doc.clone();
        d["unexpected"] = true.into();
        assert!(parse(&d, &catalog).is_err());
        let mut d = doc.clone();
        d["stages"][0]["unexpected"] = true.into();
        assert!(parse(&d, &catalog).is_err());
        let mut d = doc.clone();
        d["road_sequence"][3]["mode"] = 1.into();
        assert!(parse(&d, &catalog).is_err());
    }
    #[test]
    fn ordered_socket_owners_prefab_priority_and_catalog_roster_are_verified() {
        let (mut catalog, doc) = fixture();
        let path = doc["prefabs"].as_object().unwrap().keys().next().unwrap();
        for (field, value) in [
            ("gameObjectId", json!(12)),
            ("objectId", json!(2)),
            ("forward", json!([0., 0., 0.])),
            ("position", json!([20000., 0., 0.])),
        ] {
            let mut d = doc.clone();
            d["prefabs"][path]["sockets"]["connectors"][1][field] = value;
            assert!(parse(&d, &catalog).is_err(), "{field}");
        }
        let mut d = doc.clone();
        d["prefabs"][path]["sockets"]["version"] = 2.into();
        assert!(parse(&d, &catalog).is_err());
        let mut d = doc.clone();
        d["prefabs"][path]["parameters"]["priority"] = 3.into();
        assert!(parse(&d, &catalog).is_err());
        let mut d = doc.clone();
        d["prefabs"][path]["parameters"]["count"] = 4097.into();
        assert!(parse(&d, &catalog).is_err());
        let mut d = doc.clone();
        let p = d["prefabs"].as_object_mut().unwrap().remove(path).unwrap();
        d["prefabs"]["assets/bundled/prefabs/autospawn/monument/roadside/missing.prefab"] = p;
        assert!(parse(&d, &catalog).is_err());
        catalog.entries.get_mut(path).unwrap().priority = None;
        assert!(parse(&doc, &catalog).is_err());
    }
    #[test]
    fn legacy_missing_dungeon_inventory_stays_unknown_and_new_inventory_is_validated() {
        let (catalog, doc) = fixture();
        assert!(
            parse(&doc, &catalog)
                .unwrap()
                .prefabs()
                .values()
                .next()
                .unwrap()
                .dungeon_components()
                .is_none()
        );
        let path = doc["prefabs"].as_object().unwrap().keys().next().unwrap();
        let mut d = doc.clone();
        d["prefabs"][path]["sockets"]["dungeonComponents"] = json!({"links":[],"entrances":[]});
        assert!(
            parse(&d, &catalog)
                .unwrap()
                .prefabs()
                .values()
                .next()
                .unwrap()
                .dungeon_components()
                .unwrap()
                .links
                .is_empty()
        );
        d["prefabs"][path]["sockets"]["dungeonComponents"]["links"] = json!([900]);
        assert_eq!(
            parse(&d, &catalog)
                .unwrap()
                .prefabs()
                .values()
                .next()
                .unwrap()
                .dungeon_components()
                .unwrap()
                .links,
            vec![900]
        );
        d["prefabs"][path]["sockets"]["dungeonComponents"]["entrances"] = json!([900]);
        assert!(parse(&d, &catalog).is_err());
        d["prefabs"][path]["sockets"]["dungeonComponents"] = json!({"links":[2],"entrances":[]});
        assert!(parse(&d, &catalog).is_err());
    }
    #[test]
    fn cancellation_and_tampered_pins_stop_before_gameplay_file_reads() {
        let (catalog, doc) = fixture();
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("roadside.json");
        std::fs::write(&file, serde_json::to_vec(&doc).unwrap()).unwrap();
        let nonexistent = directory.path().join("missing-installation");
        let error =
            Source::load_inner(&file, &nonexistent, &catalog, Some(&"c".repeat(64)), || {
                false
            })
            .err()
            .unwrap();
        assert!(error.contains("Pinned roadside source bytes changed"));
        assert!(
            Source::load(&file, &nonexistent, &catalog, || true)
                .err()
                .unwrap()
                .contains("cancelled")
        );
        assert!(prepare(&nonexistent, &catalog, || true).unwrap().is_none());
        assert!(Source::cached("../outside", &nonexistent, &catalog, || false).is_err());
    }
}
