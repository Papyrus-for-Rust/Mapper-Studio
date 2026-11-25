//! Installation-bound road-attached object contracts. Inspection never places objects.
use crate::{infrastructure, installation::SourceFile};
use papyrus_terrain::spawn::Filter;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
};
const LIMIT: u64 = 16 * 1024 * 1024;
pub const VERSION: &str = "papyrus-road-objects-1";
pub const HIERARCHY_VERSION: &str = "papyrus-road-objects-2";
pub const PATH_VERSION: &str = "papyrus-path-objects-1";
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathFamily {
    #[default]
    Road,
    River,
    Powerline,
}
impl PathFamily {
    fn is_road(&self) -> bool {
        *self == Self::Road
    }
    fn class(self) -> &'static str {
        match self {
            Self::Road => "PlaceRoadObjects",
            Self::River => "PlaceRiverObjects",
            Self::Powerline => "PlacePowerlineObjects",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Basic {
    pub folder: String,
    pub filter: Filter,
    pub prefabs: Vec<String>,
    pub placement: u32,
    pub align_to_normal: bool,
    pub height_to_terrain: bool,
    pub offset: f32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Side {
    pub folder: String,
    pub filter: Filter,
    pub prefabs: Vec<String>,
    pub alignment: u32,
    pub density: f32,
    pub distance: f32,
    pub side: u32,
    pub offset: f32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Along {
    pub folder: String,
    pub filter: Filter,
    pub prefabs: Vec<String>,
    pub alignment: u32,
    pub density: f32,
    pub distance: f32,
    pub dithering: f32,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stage {
    pub object_id: i64,
    pub order: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_order: Option<usize>,
    pub name: String,
    pub mode: i32,
    #[serde(deserialize_with = "Option::deserialize")]
    pub disabled: Option<bool>,
    pub component_enabled: bool,
    pub hierarchy_active: bool,
    pub start: Vec<Basic>,
    pub end: Vec<Basic>,
    pub side: Vec<Side>,
    pub path: Vec<Along>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProceduralStage {
    pub object_id: i64,
    pub order: usize,
    pub kind: String,
    pub description: String,
    pub mode: i32,
    #[serde(deserialize_with = "Option::deserialize")]
    pub disabled: Option<bool>,
    pub component_enabled: bool,
    pub hierarchy_active: bool,
}
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    format: String,
    schema: u32,
    pub stages: Vec<ProceduralStage>,
}
pub(crate) fn validate_attachment_resources(
    start: &[Basic], end: &[Basic], side: &[Side], path: &[Along],
    requested: &mut BTreeSet<String>,
) -> Result<(), String> {
    for r in start.iter().chain(end) {
        roster(&r.folder, &r.filter, &r.prefabs, requested)?;
        if r.placement > 1 || !range(r.offset, -128., 128.) {
            return Err("Invalid road endpoint rule".into());
        }
    }
    for r in side {
        roster(&r.folder, &r.filter, &r.prefabs, requested)?;
        if r.alignment > 3
            || r.side > 3
            || !range(r.density, 0., 1.)
            || !range(r.distance, 0.1, 1000.)
            || !range(r.offset, -128., 128.)
        {
            return Err("Invalid roadside rule".into());
        }
    }
    for r in path {
        roster(&r.folder, &r.filter, &r.prefabs, requested)?;
        if r.alignment > 3
            || !range(r.density, 0., 1.)
            || !range(r.distance, 0.1, 1000.)
            || !range(r.dithering, 0., 128.)
        {
            return Err("Invalid along-road rule".into());
        }
    }
    Ok(())
}
impl Stage {
    pub fn enabled(&self) -> bool {
        !self.disabled.unwrap_or(false) && self.component_enabled && self.hierarchy_active
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prefab {
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
    pub attributes: Vec<serde_json::Value>,
    pub components: BTreeMap<String, usize>,
    pub issues: Vec<String>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    format: String,
    schema: u32,
    adapter: String,
    #[serde(default, skip_serializing_if = "PathFamily::is_road")]
    pub path_family: PathFamily,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environments: Option<BTreeMap<String, crate::monument_attributes::Environment>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stage_layout: Option<String>,
    pub infrastructure_sha256: String,
    sources: Vec<SourceFile>,
    #[serde(default)]
    extra_sources: Vec<SourceFile>,
    #[serde(default)]
    resources: BTreeMap<String, String>,
    extractor_sha256: String,
    #[serde(default)]
    pub path_policy: Option<crate::road_object_placement::Policy>,
    #[serde(default)]
    placement_profile_sha256: Option<String>,
    pub stages: Vec<Stage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<Workflow>,
    pub prefabs: BTreeMap<String, Prefab>,
    pub issues: Vec<String>,
    #[serde(skip)]
    pub sha256: String,
    #[serde(skip)]
    canonical_sha256: String,
    #[serde(skip)]
    loaded_sha256: String,
}
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn folder(s: &str) -> bool {
    s.len() <= 256
        && s == s.trim()
        && s == s.to_lowercase()
        && !s.contains(['\\', '\0'])
        && (s.is_empty() || s.split('/').all(|p| !matches!(p, "" | "." | "..")))
}
fn prefab_path(s: &str) -> bool {
    s.len() <= 512
        && s.starts_with("assets/bundled/prefabs/autospawn/")
        && s.ends_with(".prefab")
        && s == s.to_lowercase()
        && !s.contains(['\\', '\0'])
        && s.split('/').all(|p| !matches!(p, "" | "." | ".."))
}
fn roster(
    folder_name: &str,
    filter: &Filter,
    paths: &[String],
    requested: &mut BTreeSet<String>,
) -> Result<(), String> {
    filter.validate()?;
    if !folder(folder_name) || paths.len() > 4096 || folder_name.is_empty() && !paths.is_empty() {
        return Err("Invalid road object resource folder".into());
    }
    let prefix = format!("assets/bundled/prefabs/autospawn/{folder_name}/");
    if paths.windows(2).any(|p| p[0] >= p[1])
        || paths
            .iter()
            .any(|p| !prefab_path(p) || !p.starts_with(&prefix))
    {
        return Err("Road object roster differs from authored folder".into());
    }
    requested.extend(paths.iter().cloned());
    Ok(())
}
fn range(v: f32, lo: f32, hi: f32) -> bool {
    v.is_finite() && (lo..=hi).contains(&v)
}
impl Source {
    pub(crate) fn stage_layout(&self) -> Option<&str> {
        self.stage_layout.as_deref()
    }
    pub(crate) fn preparation_matches(&self, profile: &str) -> bool {
        self.path_family == PathFamily::Road
            && self.path_policy.is_some()
            && self.workflow.is_some()
            && self.placement_profile_sha256.as_deref() == Some(profile)
    }
    pub(crate) fn river_preparation_matches(&self, profile: &str, extractor: &str) -> bool {
        self.path_family == PathFamily::River
            && self.path_policy.is_some()
            && self.workflow.is_some()
            && self.environments.is_some()
            && self.placement_profile_sha256.as_deref() == Some(profile)
            && self.extractor_sha256 == extractor
    }
    pub(crate) fn verify_common_receipts(
        &self,
        required: &[SourceFile],
        additional: &[SourceFile],
    ) -> Result<(), String> {
        if required.iter().any(|r| !self.sources.contains(r))
            || additional.iter().any(|r| {
                self.sources
                    .iter()
                    .chain(&self.extra_sources)
                    .any(|other| other.relative_path == r.relative_path && other != r)
            })
        {
            return Err("Object sources refer to different installation receipts".into());
        }
        Ok(())
    }
    fn validate_workflow(&self) -> Result<(), String> {
        if self.schema == 1 {
            if self.workflow.is_some() || self.stages.iter().any(|s| s.scene_order.is_some()) {
                return Err("Legacy road-object source cannot claim procedural order".into());
            }
            return Ok(());
        }
        let workflow = self
            .workflow
            .as_ref()
            .ok_or("Road-object source is missing procedural order")?;
        if workflow.format != "papyrus-procedural-order"
            || workflow.schema != if self.stage_layout.is_some() { 2 } else { 1 }
            || workflow.stages.is_empty()
            || workflow.stages.len() > 512
        {
            return Err("Invalid procedural workflow schema or stage count".into());
        }
        let mut ids = BTreeSet::new();
        for (i, s) in workflow.stages.iter().enumerate() {
            if s.order != i
                || s.object_id == 0
                || !ids.insert(s.object_id)
                || s.kind.is_empty()
                || s.kind.len() > 256
                || s.kind.contains('\0')
                || s.description.len() > 1024
                || s.description.contains('\0')
                || s.disabled.is_none() != self.stage_layout.is_some()
            {
                return Err("Invalid procedural stage identity or ordering".into());
            }
        }
        let roads: Vec<_> = workflow
            .stages
            .iter()
            .filter(|s| s.kind == self.path_family.class())
            .collect();
        if roads.len() != self.stages.len() {
            return Err("Procedural workflow has a different road-object roster".into());
        }
        for (s, global) in self.stages.iter().zip(roads) {
            if s.scene_order != Some(global.order)
                || s.object_id != global.object_id
                || s.name != global.description
                || s.mode != global.mode
                || s.disabled != global.disabled
                || s.component_enabled != global.component_enabled
                || s.hierarchy_active != global.hierarchy_active
            {
                return Err("Road-object stage differs from its procedural workflow entry".into());
            }
        }
        Ok(())
    }
    pub fn procedural_order(&self) -> Result<&[ProceduralStage], String> {
        self.validate_for_fitting()?;
        self.workflow.as_ref().map(|w|w.stages.as_slice())
            .ok_or_else(||"This source predates global procedural order; re-extract it before binding full generation".into())
    }
    pub(crate) fn contains_source_receipts(&self, receipts: &[SourceFile]) -> bool {
        receipts.iter().all(|receipt| {
            self.sources.iter().chain(&self.extra_sources).any(|source| source == receipt)
        })
    }
    pub(crate) fn validate_for_fitting(&self) -> Result<(), String> {
        if !hash(&self.sha256) || self.stages.len() > 32 || self.prefabs.len() > 4096 {
            return Err("Road fitting requires a loaded, bounded source snapshot".into());
        }
        let raw = serde_json::to_vec(self).map_err(|e| e.to_string())?;
        if self.sha256 != self.loaded_sha256
            || format!("{:x}", Sha256::digest(&raw)) != self.canonical_sha256
        {
            return Err("Road object source changed after verification".into());
        }
        Self::parse(&raw, &self.infrastructure_sha256, &self.sources, || false).map(|_| ())
    }
    /// Find a prepared source for this exact infrastructure/installation. A saved
    /// object recipe always uses `cached` instead; discovery never upgrades it.
    pub fn discover(
        root: &Path,
        parent: &infrastructure::Source,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        let cache = crate::storage::local_data()?.join("papyrus-road-object-cache");
        if !cache.is_dir() {
            return Ok(None);
        }
        let mut candidates = Vec::new();
        for (index, entry) in std::fs::read_dir(&cache)
            .map_err(|e| e.to_string())?
            .enumerate()
        {
            if cancelled() {
                return Ok(None);
            }
            if index >= 256 {
                return Err("Too many road object snapshots; discovery is limited to 256".into());
            }
            let directory = entry.map_err(|e| e.to_string())?.path();
            let Some(digest) = directory
                .file_name()
                .and_then(|n| n.to_str())
                .filter(|n| hash(n))
            else {
                continue;
            };
            let path = directory.join("objects.json");
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if !meta.is_file() || meta.len() > LIMIT {
                continue;
            }
            candidates.push((meta.modified().ok(), digest.to_owned(), path));
        }
        candidates.sort();
        let receipts = parent.road_object_receipts()?;
        for (_, digest, path) in candidates.into_iter().rev() {
            if cancelled() {
                return Ok(None);
            }
            let mut raw = Vec::new();
            let Ok(file) = std::fs::File::open(&path) else {
                continue;
            };
            if file.take(LIMIT + 1).read_to_end(&mut raw).is_err() || raw.len() as u64 > LIMIT {
                continue;
            }
            if format!("{:x}", Sha256::digest(&raw)) != digest {
                continue;
            }
            let Ok(candidate) = Self::parse(&raw, &parent.sha256, &receipts, &cancelled) else {
                continue;
            };
            // Legacy snapshots cannot invent the scene order or source sampling policy.
            if candidate.path_family != PathFamily::Road
                || candidate.path_policy.is_none()
                || candidate.procedural_order().is_err()
            {
                continue;
            }
            if let Ok(source) = Self::cached(root, &digest, parent, &cancelled) {
                return Ok(Some(source));
            }
        }
        Ok(None)
    }
    pub fn cached(
        root: &Path,
        digest: &str,
        parent: &infrastructure::Source,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        if !hash(digest) {
            return Err("Invalid pinned road object fingerprint".into());
        }
        let path = crate::client_cache::require_pinned_document(
            &crate::storage::local_data()?,
            "papyrus-road-object-cache",
            digest,
            "objects.json",
            "road object",
        )?;
        let source = Self::load(root, &path, parent, cancelled)?;
        if source.sha256 != digest {
            return Err("Pinned road object source changed".into());
        }
        Ok(source)
    }
    pub fn load(
        root: &Path,
        path: &Path,
        parent: &infrastructure::Source,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        if cancelled() {
            return Err("Road object verification cancelled".into());
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        Self::load_document(root, &bytes, parent, cancelled)
    }
    pub(crate) fn load_document(
        root: &Path,
        bytes: &[u8],
        parent: &infrastructure::Source,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        let source = Self::parse(
            bytes,
            &parent.sha256,
            &parent.road_object_receipts()?,
            &cancelled,
        )?;
        if source.stage_layout.as_deref() != parent.stage_layout() {
            return Err("Road object stage layout differs from its infrastructure source".into());
        }
        parent.verify_installation(root, &cancelled)?;
        for receipt in &source.extra_sources {
            if crate::installation::hash_file(
                root,
                &receipt.relative_path,
                8 * 1024 * 1024 * 1024,
                &cancelled,
            )? != *receipt
            {
                return Err(
                    "Road object resource manifest differs from selected installation".into(),
                );
            }
        }
        if cancelled() {
            return Err("Road object verification cancelled".into());
        }
        Ok(source)
    }
    pub(crate) fn parse(
        bytes: &[u8],
        parent: &str,
        receipts: &[SourceFile],
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        if bytes.len() as u64 > LIMIT {
            return Err("Road object source exceeds 16 MiB".into());
        }
        let mut doc: Self = serde_json::from_slice(bytes)
            .map_err(|e| format!("Invalid road object source: {e}"))?;
        let supported_header = matches!(
            (
                doc.schema,
                doc.adapter.as_str(),
                doc.stage_layout.as_deref()
            ),
            (1 | 2, VERSION, None)
                | (3, HIERARCHY_VERSION, Some("mode-description"))
                | (4, PATH_VERSION, None)
                | (5, PATH_VERSION, Some("mode-description"))
                | (6, PATH_VERSION, None)
                | (7, PATH_VERSION, Some("mode-description"))
        );
        if doc.format != "papyrus-road-object-source"
            || !supported_header
            || !matches!((doc.schema, doc.path_family),
                (1..=3, PathFamily::Road) | (4..=5, PathFamily::River) | (6..=7, PathFamily::Powerline))
            || (doc.path_family == PathFamily::River) != doc.environments.is_some()
            || !hash(&doc.infrastructure_sha256)
            || doc.infrastructure_sha256 != parent
            || doc.sources != receipts
            || !hash(&doc.extractor_sha256)
            || doc.stages.is_empty()
            || doc.stages.len() > 32
            || (doc.prefabs.is_empty() && doc.path_family != PathFamily::Powerline)
            || doc.prefabs.len() > 4096
            || doc.issues.len() > 4096
        {
            return Err("Road object source schema or installation mismatch".into());
        }
        doc.validate_workflow()?;
        if let Some(environments) = &doc.environments {
            if environments.keys().ne(doc.prefabs.keys()) {
                return Err("Path object environment roster differs from captured prefabs".into());
            }
            for environment in environments.values() {
                crate::monument_attributes::Attributes {
                    version: 3,
                    root_decor: vec![],
                    terrain_checks: vec![],
                    water_checks: vec![],
                    path_connectors: vec![],
                    placement_blockers: Some(vec![]),
                    environment: Some(environment.clone()),
                    issues: vec![],
                }
                .validate()?;
            }
        }
        let mut ids = BTreeSet::new();
        match (&doc.path_policy, &doc.placement_profile_sha256) {
            (Some(policy), Some(profile)) if hash(profile) => policy.validate()?,
            (None, None) => {}
            _ => return Err("Road object fitting requires a bound placement profile".into()),
        }
        if doc.extra_sources.len() > 1
            || doc.extra_sources.iter().any(|s| {
                s.relative_path != "Bundles/shared/content.bundle"
                    || !hash(&s.sha256)
                    || s.bytes == 0
                    || s.bytes > 8 * 1024 * 1024 * 1024
            })
            || doc.resources.len() > 4096
            || (!doc.resources.is_empty() && doc.extra_sources.is_empty())
            || doc.resources.iter().any(|(guid, path)| {
                guid.len() != 32
                    || !guid
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    || path.len() > 1024
                    || !path.starts_with("assets/")
                    || path != &path.to_lowercase()
                    || path.contains(['\\', '\0'])
                    || path.split('/').any(|p| matches!(p, "" | "." | ".."))
            })
        {
            return Err("Invalid road object resource receipts or GUID paths".into());
        }
        let mut requested = BTreeSet::new();
        for (i, s) in doc.stages.iter().enumerate() {
            if cancelled() {
                return Err("Road object verification cancelled".into());
            }
            if s.object_id == 0
                || !ids.insert(s.object_id)
                || s.order != i
                || s.name.len() > 256
                || !(-1..=3).contains(&s.mode)
                || s.disabled.is_none() != doc.stage_layout.is_some()
                || [s.start.len(), s.end.len(), s.side.len(), s.path.len()]
                    .iter()
                    .any(|n| *n > 128)
            {
                return Err("Invalid road object stage order or activation".into());
            }
            validate_attachment_resources(&s.start, &s.end, &s.side, &s.path, &mut requested)?;
        }
        for (path, p) in &doc.prefabs {
            if cancelled() {
                return Err("Road object verification cancelled".into());
            }
            if !requested.contains(path)
                || !prefab_path(path)
                || p.attributes.len() > 4096
                || p.components.len() > 1024
                || p.issues.len() > 1024
                || p.rotation.iter().any(|v| !v.is_finite())
                || (p.rotation.iter().map(|v| v * v).sum::<f32>() - 1.).abs() > 0.001
                || p.scale.iter().any(|v| !range(*v, 0.0001, 10000.))
                || p.issues.iter().any(|s| s.len() > 1024)
                || p.components
                    .iter()
                    .any(|(k, v)| k.is_empty() || k.len() > 256 || *v == 0 || *v > 65536)
            {
                return Err("Invalid road prefab metadata or transform".into());
            }
            for attr in &p.attributes {
                if let Some(references) = attr.get("references") {
                    let refs = references
                        .as_array()
                        .filter(|r| r.len() <= 64)
                        .ok_or("Invalid road prefab reference list")?;
                    for reference in refs {
                        let object = reference
                            .as_object()
                            .filter(|r| r.len() == 2)
                            .ok_or("Invalid road prefab reference")?;
                        let field = object
                            .get("field")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty() && s.len() <= 128 && !s.contains('\0'))
                            .ok_or("Invalid road prefab reference field")?;
                        let guid = object
                            .get("guid")
                            .and_then(|v| v.as_str())
                            .filter(|s| {
                                s.len() == 32
                                    && s.bytes()
                                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                            })
                            .ok_or("Invalid road prefab reference GUID")?;
                        if !doc.resources.contains_key(guid)
                            && !p
                                .issues
                                .iter()
                                .any(|s| s.contains(field) && s.contains("GUID absent"))
                        {
                            return Err(
                                "Road prefab GUID is missing without a coverage issue".into()
                            );
                        }
                    }
                }
            }
        }
        if doc.issues.iter().any(|s| s.len() > 1024) {
            return Err("Invalid road object coverage issues".into());
        }
        for path in requested.difference(&doc.prefabs.keys().cloned().collect()) {
            if !doc
                .issues
                .contains(&format!("Missing road prefab root: {path}"))
            {
                return Err("Missing road prefab without a coverage issue".into());
            }
        }
        doc.sha256 = format!("{:x}", Sha256::digest(bytes));
        doc.loaded_sha256 = doc.sha256.clone();
        doc.canonical_sha256 = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&doc).map_err(|e| e.to_string())?)
        );
        Ok(doc)
    }
    pub fn inspect(&self, details: bool) -> serde_json::Value {
        let mut out = serde_json::json!({"engine":self.adapter,"source_sha256":self.sha256,"infrastructure_sha256":self.infrastructure_sha256,"stages":self.stages.iter().map(|s|serde_json::json!({"object_id":s.object_id,"name":s.name,"order":s.order,"enabled":s.enabled(),"mode":s.mode,"start_rules":s.start.len(),"end_rules":s.end.len(),"side_rules":s.side.len(),"path_rules":s.path.len()})).collect::<Vec<_>>(),"prefab_count":self.prefabs.len(),"attribute_count":self.prefabs.values().map(|p|p.attributes.len()).sum::<usize>(),"prefab_issues":self.prefabs.iter().filter(|(_,p)|!p.issues.is_empty()).map(|(path,p)|serde_json::json!({"prefab":path,"issues":p.issues})).collect::<Vec<_>>(),"source_issues":self.issues,"placed_instances":0,"pending_checks":["Path sampling, endpoint trim and source sequence replacement","Authored decor transforms, anchors, filters and terrain effects","Prefab assembly, model rendering and runtime validation"]});
        out["resources"] = serde_json::json!(self.resources);
        out["path_policy"] = serde_json::json!(self.path_policy);
        if let Some(workflow) = &self.workflow {
            out["procedural_stage_count"] = serde_json::json!(workflow.stages.len());
            out["road_object_scene_order"] = serde_json::json!(
                self.stages
                    .iter()
                    .map(|s| s.scene_order)
                    .collect::<Vec<_>>()
            );
            if details {
                out["procedural_workflow"] = serde_json::json!(workflow);
            }
        }
        if details {
            out["rules"] =
                serde_json::to_value(&self.stages).expect("validated road object fields");
            out["prefabs"] = serde_json::to_value(&self.prefabs).expect("validated authored JSON");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (serde_json::Value, Vec<SourceFile>) {
        let path = "assets/bundled/prefabs/autospawn/decor/roadside/test.prefab";
        let receipts = vec![SourceFile {
            relative_path: "Bundles/maps/maps.bundle".into(),
            sha256: "c".repeat(64),
            bytes: 1,
        }];
        let doc = serde_json::json!({"format":"papyrus-road-object-source","schema":1,"adapter":VERSION,"infrastructure_sha256":"a".repeat(64),"sources":receipts,"extractor_sha256":"b".repeat(64),"stages":[{"object_id":340,"order":0,"name":"Road objects","mode":-1,"disabled":false,"component_enabled":true,"hierarchy_active":false,"start":[],"end":[],"path":[],"side":[{"folder":"decor/roadside","filter":{"SplatType":-1,"BiomeType":-1,"TopologyAny":-1,"TopologyAll":0,"TopologyNot":128},"prefabs":[path],"alignment":3,"density":0.5,"distance":25.,"side":3,"offset":0.}]}],"prefabs":{path:{"rotation":[0.,0.,0.,1.],"scale":[1.,1.,1.],"attributes":[{"kind":"TerrainAnchor","fields":{"Extent":2.}}],"components":{"TerrainAnchor":1},"issues":["Sequence.Prefab: unresolved field"]}},"issues":[]});
        (doc, receipts)
    }
    #[test]
    fn current_headers_require_explicit_absent_flags_and_matching_workflow_layout() {
        let (mut doc, receipts) = fixture();
        doc["schema"] = 3.into();
        doc["adapter"] = HIERARCHY_VERSION.into();
        doc["stage_layout"] = "mode-description".into();
        doc["stages"][0]["scene_order"] = 0.into();
        doc["stages"][0]["disabled"] = serde_json::Value::Null;
        doc["workflow"] = serde_json::json!({
            "format": "papyrus-procedural-order", "schema": 2,
            "stages": [{"object_id":340,"order":0,"kind":"PlaceRoadObjects",
                "description":"Road objects","mode":-1,"disabled":null,
                "component_enabled":true,"hierarchy_active":false}]
        });
        let parse = |value: &serde_json::Value| {
            Source::parse(
                &serde_json::to_vec(value).unwrap(),
                &"a".repeat(64),
                &receipts,
                || false,
            )
        };
        let source = parse(&doc).unwrap();
        assert!(source.stages[0].disabled.is_none());
        assert_eq!(source.inspect(false)["engine"], HIERARCHY_VERSION);
        assert!(!source.stages[0].enabled());
        assert_eq!(source.procedural_order().unwrap().len(), 1);
        source.validate_for_fitting().unwrap();
        for (pointer, value) in [
            ("/schema", 2.into()),
            ("/adapter", VERSION.into()),
            ("/stage_layout", "future".into()),
            ("/workflow/schema", 1.into()),
            ("/stages/0/disabled", false.into()),
            ("/workflow/stages/0/disabled", false.into()),
        ] {
            let mut mixed = doc.clone();
            *mixed.pointer_mut(pointer).unwrap() = value;
            assert!(parse(&mixed).is_err(), "{pointer}");
        }
        for pointer in ["/stages/0", "/workflow/stages/0"] {
            let mut missing = doc.clone();
            missing
                .pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove("disabled");
            assert!(parse(&missing).is_err(), "{pointer}");
        }
    }
    #[test]
    #[ignore = "requires an owned installation and its captured road object scene"]
    fn owned_road_objects_load_with_their_exact_parent_and_procedural_order() {
        let path = |name| std::path::PathBuf::from(std::env::var_os(name).expect(name));
        let root = path("MAPPER_ROAD_OBJECT_TEST_ROOT");
        let _verification = crate::installation::VerificationScope::default();
        let installation = crate::installation::inspect(&root, || false).unwrap();
        let catalog = crate::prefab_assets::load(
            &root,
            &path("MAPPER_ROAD_OBJECT_TEST_CATALOG"),
            &installation,
            || false,
        )
        .unwrap();
        let parent =
            infrastructure::Source::load(&path("MAPPER_ROAD_OBJECT_TEST_PARENT"), &catalog, || {
                false
            })
            .unwrap();
        let source = Source::load(
            &root,
            &path("MAPPER_ROAD_OBJECT_TEST_SOURCE"),
            &parent,
            || false,
        )
        .unwrap();
        let current = parent.stage_layout() == Some("mode-description");
        assert_eq!(source.schema, if current { 3 } else { 2 });
        assert_eq!(source.stage_layout.as_deref(), parent.stage_layout());
        assert!(
            source
                .stages
                .iter()
                .all(|stage| stage.disabled.is_none() == current)
        );
        let workflow = source.procedural_order().unwrap();
        assert!(workflow.len() > source.stages.len());
        assert!(
            workflow
                .iter()
                .all(|stage| stage.disabled.is_none() == current)
        );
        assert_eq!(
            workflow
                .iter()
                .filter(|stage| stage.kind == "PlaceRoadObjects")
                .count(),
            source.stages.len()
        );
        let mut report = source.inspect(true);
        report["source_layout_verified"] = true.into();
        report["runtime_validated"] = false.into();
        std::fs::write(
            path("MAPPER_ROAD_OBJECT_TEST_REPORT"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
    }
    #[test]
    fn global_workflow_is_bound_to_the_exact_road_roster_and_legacy_sources_cannot_invent_it() {
        let (mut doc, receipts) = fixture();
        let parse = |v: &serde_json::Value| {
            Source::parse(
                &serde_json::to_vec(v).unwrap(),
                &"a".repeat(64),
                &receipts,
                || false,
            )
        };
        let legacy = parse(&doc).unwrap();
        assert!(legacy.procedural_order().is_err());
        assert!(legacy.inspect(true).get("procedural_workflow").is_none());
        doc["schema"] = 2.into();
        assert!(parse(&doc).is_err());
        doc["stages"][0]["scene_order"] = 1.into();
        doc["workflow"] = serde_json::json!({"format":"papyrus-procedural-order","schema":1,"stages":[
            {"object_id":100,"order":0,"kind":"GenerateRoadTerrain","description":"Road terrain","mode":-1,"disabled":false,"component_enabled":true,"hierarchy_active":true},
            {"object_id":340,"order":1,"kind":"PlaceRoadObjects","description":"Road objects","mode":-1,"disabled":false,"component_enabled":true,"hierarchy_active":false},
            {"object_id":500,"order":2,"kind":"FutureStage","description":"Source-provided future stage","mode":16,"disabled":false,"component_enabled":true,"hierarchy_active":true}
        ]});
        let source = parse(&doc).unwrap();
        assert_eq!(source.procedural_order().unwrap()[2].mode, 16);
        assert_eq!(
            source.inspect(false)["road_object_scene_order"],
            serde_json::json!([1])
        );
        for (pointer, value) in [
            ("/schema", serde_json::json!(1)),
            ("/workflow/schema", serde_json::json!(2)),
            ("/workflow/stages/1/order", serde_json::json!(2)),
            ("/workflow/stages/1/object_id", serde_json::json!(100)),
            (
                "/workflow/stages/1/description",
                serde_json::json!("Wrong stage"),
            ),
            (
                "/workflow/stages/1/hierarchy_active",
                serde_json::json!(true),
            ),
            (
                "/workflow/stages/2/kind",
                serde_json::json!("PlaceRoadObjects"),
            ),
            ("/stages/0/scene_order", serde_json::json!(2)),
        ] {
            let mut bad = doc.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(parse(&bad).is_err(), "{pointer}");
        }
        let mut source = parse(&doc).unwrap();
        source.workflow.as_mut().unwrap().stages[2].description =
            "Changed after verification".into();
        assert!(source.procedural_order().is_err());
    }
    #[test]
    fn source_rules_and_pending_attributes_remain_inspection_only() {
        let (doc, receipts) = fixture();
        let bytes = serde_json::to_vec(&doc).unwrap();
        let source = Source::parse(&bytes, &"a".repeat(64), &receipts, || false).unwrap();
        let serialized = serde_json::to_value(&source).unwrap();
        assert!(serialized.get("stage_layout").is_none());
        assert_eq!(serialized["stages"][0]["disabled"], false);
        assert!(!source.stages[0].enabled());
        assert_eq!(source.stages[0].side[0].offset, 0.);
        let report = source.inspect(true);
        assert_eq!(report["placed_instances"], 0);
        assert_eq!(report["prefab_count"], 1);
        assert_eq!(report["attribute_count"], 1);
        assert_eq!(report["prefab_issues"].as_array().unwrap().len(), 1);
        assert_eq!(report["rules"][0]["side"][0]["filter"]["TopologyNot"], 128);
        assert!(source.inspect(false).get("rules").is_none());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            doc
        );
    }
    #[test]
    fn source_identity_order_roster_and_transform_mismatches_fail() {
        let (doc, receipts) = fixture();
        let check = |doc: &serde_json::Value| {
            Source::parse(
                &serde_json::to_vec(doc).unwrap(),
                &"a".repeat(64),
                &receipts,
                || false,
            )
        };
        for (pointer, value) in [
            ("/infrastructure_sha256", serde_json::json!("d".repeat(64))),
            ("/stages/0/order", serde_json::json!(1)),
            ("/stages/0/side/0/side", serde_json::json!(4)),
            ("/stages/0/side/0/distance", serde_json::json!(0)),
            ("/stages/0/side/0/folder", serde_json::json!("../roadside")),
            (
                "/stages/0/side/0/prefabs/0",
                serde_json::json!("assets/bundled/prefabs/autospawn/decor/other.prefab"),
            ),
            ("/stages/0/side/0/filter/TopologyAny", serde_json::json!(0)),
            ("/sources/0/bytes", serde_json::json!(2)),
        ] {
            let mut bad = doc.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(check(&bad).is_err(), "{pointer}");
        }
        let path = "assets/bundled/prefabs/autospawn/decor/roadside/test.prefab";
        let mut bad = doc.clone();
        bad["prefabs"][path]["rotation"] = serde_json::json!([0., 0., 0., 0.]);
        assert!(check(&bad).is_err());
        let mut bad = doc.clone();
        bad["prefabs"].as_object_mut().unwrap().remove(path);
        assert!(check(&bad).is_err());
        let mut bad = doc.clone();
        bad["placed_instances"] = 1.into();
        assert!(check(&bad).is_err());
    }
    #[test]
    fn cancellation_and_bounded_reads_never_return_a_partial_source() {
        let (doc, receipts) = fixture();
        let bytes = serde_json::to_vec(&doc).unwrap();
        assert!(Source::parse(&bytes, &"a".repeat(64), &receipts, || true).is_err());
        assert!(
            Source::parse(
                &vec![b' '; LIMIT as usize + 1],
                &"a".repeat(64),
                &receipts,
                || false
            )
            .is_err()
        );
    }
    #[test]
    fn resource_references_require_bound_manifest_and_safe_exact_guid_mapping() {
        let (mut doc, receipts) = fixture();
        let path = "assets/bundled/prefabs/autospawn/decor/roadside/test.prefab";
        doc["prefabs"][path]["attributes"][0]["references"] =
            serde_json::json!([{"field":"WirePrefab","guid":"d".repeat(32)}]);
        doc["resources"] = serde_json::json!({"d".repeat(32):"assets/prefabs/wire.prefab"});
        doc["extra_sources"] = serde_json::json!([{"relative_path":"Bundles/shared/content.bundle","sha256":"e".repeat(64),"bytes":128}]);
        let check = |doc: &serde_json::Value| {
            Source::parse(
                &serde_json::to_vec(doc).unwrap(),
                &"a".repeat(64),
                &receipts,
                || false,
            )
        };
        let source = check(&doc).unwrap();
        assert_eq!(
            source.inspect(false)["resources"]["d".repeat(32)],
            "assets/prefabs/wire.prefab"
        );
        let mut bad = doc.clone();
        bad["extra_sources"] = serde_json::json!([]);
        assert!(check(&bad).is_err());
        let mut bad = doc.clone();
        bad["extra_sources"][0]["relative_path"] = "../content.bundle".into();
        assert!(check(&bad).is_err());
        let mut bad = doc.clone();
        bad["resources"]["d".repeat(32)] = "assets/../wire.prefab".into();
        assert!(check(&bad).is_err());
        let mut bad = doc.clone();
        bad["resources"] = serde_json::json!({});
        assert!(check(&bad).is_err());
        let mut bad = doc.clone();
        bad["prefabs"][path]["attributes"][0]["references"][0]["guid"] = "invalid".into();
        assert!(check(&bad).is_err());
    }
    #[test]
    fn fitting_rejects_mutated_rules_and_spoofed_source_identity() {
        let (doc, receipts) = fixture();
        let bytes = serde_json::to_vec(&doc).unwrap();
        let mut source = Source::parse(&bytes, &"a".repeat(64), &receipts, || false).unwrap();
        source.validate_for_fitting().unwrap();
        source.stages[0].side[0].density = 0.25;
        assert!(source.validate_for_fitting().is_err());
        let mut source = Source::parse(&bytes, &"a".repeat(64), &receipts, || false).unwrap();
        source.sha256 = "f".repeat(64);
        assert!(source.validate_for_fitting().is_err());
    }
    #[test]
    fn river_sources_bind_their_workflow_family_and_complete_environment_roster() {
        let (mut doc, receipts) = fixture();
        doc["schema"] = 4.into();
        doc["adapter"] = PATH_VERSION.into();
        doc["path_family"] = "river".into();
        doc["stages"][0]["scene_order"] = 0.into();
        doc["workflow"] = serde_json::json!({"format":"papyrus-procedural-order","schema":1,"stages":[{
            "object_id":340,"order":0,"kind":"PlaceRiverObjects","description":"Road objects",
            "mode":-1,"disabled":false,"component_enabled":true,"hierarchy_active":false}]});
        let environment = serde_json::json!({"root_transform":{"position":[0,0,0],"rotation":[0,0,0,1],"scale":[1,1,1]},
            "checks":[],"volumes":[],"issues":[],"collider_meshes":[]});
        let path = doc["prefabs"]
            .as_object()
            .unwrap()
            .keys()
            .next()
            .unwrap()
            .clone();
        doc["environments"] = serde_json::json!({path.clone():environment});
        let parse = |doc: &serde_json::Value| {
            Source::parse(
                &serde_json::to_vec(doc).unwrap(),
                &"a".repeat(64),
                &receipts,
                || false,
            )
        };
        let source = parse(&doc).unwrap();
        assert_eq!(source.path_family, PathFamily::River);
        source.validate_for_fitting().unwrap();
        assert!(crate::object_workflow::plan(&source, &[]).is_err());
        for (pointer, value) in [
            ("/schema", 2.into()),
            ("/path_family", "road".into()),
            ("/workflow/stages/0/kind", "PlaceRoadObjects".into()),
            ("/environments", serde_json::json!({})),
        ] {
            let mut invalid = doc.clone();
            *invalid.pointer_mut(pointer).unwrap() = value;
            assert!(parse(&invalid).is_err(), "{pointer}");
        }
        let mut current = doc.clone();
        current["schema"] = 5.into();
        current["stage_layout"] = "mode-description".into();
        current["stages"][0]["disabled"] = serde_json::Value::Null;
        current["workflow"]["schema"] = 2.into();
        current["workflow"]["stages"][0]["disabled"] = serde_json::Value::Null;
        assert!(parse(&current).is_ok());
    }
    #[test]
    fn legacy_sources_keep_their_serialized_field_set() {
        let (doc, receipts) = fixture();
        let source = Source::parse(
            &serde_json::to_vec(&doc).unwrap(),
            &"a".repeat(64),
            &receipts,
            || false,
        )
        .unwrap();
        assert_eq!(source.path_family, PathFamily::Road);
        let serialized = serde_json::to_value(source).unwrap();
        assert!(serialized.get("path_family").is_none());
        assert!(serialized.get("environments").is_none());
    }
}
