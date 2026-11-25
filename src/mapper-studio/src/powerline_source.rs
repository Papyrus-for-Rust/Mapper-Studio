//! Persistent, installation-bound utility inputs. Loading never enables execution.
use crate::{
    decor_source,
    installation::{self, SourceFile},
    metadata_runtime,
    prefab_assets::Catalog,
    road_object_source::{self, Along, Basic, Side, Workflow},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
};

const LIMIT: u64 = 32 * 1024 * 1024;
const CACHE: &str = "papyrus-powerline-cache";
mod binding;
pub use binding::ResourceRules;
mod wires;
pub(crate) use wires::templates as wire_templates_from_runtime;
pub use wires::{PlacedWireNode, WireTemplate};
const KINDS: [&str; 4] = [
    "GeneratePowerlineLayout",
    "GeneratePowerlineTopology",
    "PlacePowerlineObjects",
    "GenerateWireMeshes",
];
const GAMEPLAY_FILES: [&str; 4] = [
    "GameAssembly.dll",
    "RustClient_Data/il2cpp_data/Metadata/global-metadata.dat",
    "RustClient_Data/Managed/Assembly-CSharp.dll",
    "RustDedicated_Data/Managed/Assembly-CSharp.dll",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Program {
    pub object_id: i64,
    pub order: usize,
    pub kind: String,
    pub description: String,
    pub hierarchy_active: bool,
    pub mode: i32,
    #[serde(deserialize_with = "Option::deserialize")]
    pub disabled: Option<bool>,
    pub component_enabled: bool,
    start: Option<Vec<Basic>>,
    end: Option<Vec<Basic>>,
    side: Option<Vec<Side>>,
    path: Option<Vec<Along>>,
}
impl Program {
    fn validate_resources(&self, requested: &mut BTreeSet<String>) -> Result<(), String> {
        let groups = [
            self.start.as_ref().map(Vec::len),
            self.end.as_ref().map(Vec::len),
            self.side.as_ref().map(Vec::len),
            self.path.as_ref().map(Vec::len),
        ];
        let attached = self.kind == "PlacePowerlineObjects";
        if groups
            .iter()
            .any(|group| group.is_some() != attached || group.is_some_and(|n| n > 128))
        {
            return Err("Utility attachment groups differ from their stage kind".into());
        }
        if attached {
            road_object_source::validate_attachment_resources(
                self.start.as_deref().unwrap_or_default(),
                self.end.as_deref().unwrap_or_default(),
                self.side.as_deref().unwrap_or_default(),
                self.path.as_deref().unwrap_or_default(),
                requested,
            )?;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeComponent {
    pub game_object: i64,
    pub name: String,
    pub object_id: i64,
    pub hierarchy_active_without_root: bool,
    pub layer: u8,
    pub local_to_root: [[f32; 4]; 4],
    pub component: serde_json::Value,
    pub fields: Option<Box<serde_json::value::RawValue>>,
}
pub(crate) fn validate_runtime(rows: &[RuntimeComponent]) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    if rows.len() > 8192 {
        return Err("Utility runtime component roster exceeds its bound".into());
    }
    for row in rows {
        let valid_identity = match &row.component {
            serde_json::Value::String(name) => {
                !name.is_empty() && name.len() <= 256 && !name.contains('\0')
            }
            serde_json::Value::Array(parts) => {
                parts.len() == 3
                    && parts.iter().all(|part| {
                        part.as_str()
                            .is_some_and(|s| s.len() <= 256 && !s.contains('\0'))
                    })
            }
            _ => false,
        };
        if row.game_object == 0
            || row.object_id == 0
            || !ids.insert(row.object_id)
            || row.name.len() > 1024
            || row.name.contains('\0')
            || row.layer > 31
            || row
                .local_to_root
                .iter()
                .flatten()
                .any(|value| !value.is_finite())
            || !valid_identity
            || row
                .fields
                .as_ref()
                .is_some_and(|fields| fields.get().len() > 1024 * 1024)
        {
            return Err("Invalid utility runtime component identity or transform".into());
        }
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Prefab {
    placement: Box<serde_json::value::RawValue>,
    runtime: Vec<RuntimeComponent>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    format: String,
    schema: u32,
    inspection_only: bool,
    game_code_executed: bool,
    sources: Vec<SourceFile>,
    extra_sources: Vec<SourceFile>,
    extractor_sha256: String,
    stage_layout: String,
    stages: Vec<Program>,
    procedural_order: Workflow,
    prefabs: BTreeMap<String, Prefab>,
    wire_resources: BTreeMap<String, String>,
    wire_prefabs: BTreeMap<String, Vec<RuntimeComponent>>,
}
pub struct Source {
    document: Document,
    prefabs: BTreeMap<String, Attachment>,
    sha256: String,
}
pub struct Attachment {
    pub fitting: papyrus_terrain::decor_fit::PreparedPrefab,
    pub terrain: papyrus_terrain::footprints::Prefab,
}
impl Source {
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    pub fn programs(&self) -> &[Program] {
        &self.document.stages
    }
    pub fn prefabs(&self) -> &BTreeMap<String, Attachment> {
        &self.prefabs
    }
    pub fn runtime(&self, path: &str) -> Option<&[RuntimeComponent]> {
        self.document
            .prefabs
            .get(path)
            .map(|prefab| prefab.runtime.as_slice())
    }
    pub fn wire_resources(&self) -> &BTreeMap<String, String> {
        &self.document.wire_resources
    }
    fn read(
        bytes: &[u8],
        road: &road_object_source::Source,
        flags: &BTreeMap<String, u32>,
        protected: u32,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Self, String> {
        if cancelled() {
            return Err("Utility preparation cancelled".into());
        }
        if bytes.len() as u64 > LIMIT {
            return Err("Utility source exceeds 32 MiB".into());
        }
        let document: Document = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if document.format != "mapper-powerline-source-reference"
            || document.schema != 1
            || !document.inspection_only
            || document.game_code_executed
            || !metadata_runtime::powerline_extractor_matches(&document.extractor_sha256)
            || !matches!(
                document.stage_layout.as_str(),
                "legacy" | "mode-description"
            )
            || document.stage_layout != road.stage_layout().unwrap_or("legacy")
            || &document.procedural_order
                != road.workflow.as_ref().ok_or("Missing road workflow")?
            || document.sources.len() > 64
            || document.extra_sources.len() > 64
            || document.prefabs.len() > 1024
            || document.stages.len() > 32
            || document.wire_resources.len() > 4096
            || document.wire_prefabs.len() > 4096
        {
            return Err("Utility capture differs from its source workflow or producer".into());
        }
        installation::validate_source_receipts(&document.sources, &document.extra_sources)?;
        if !document.sources.iter().any(|file| {
            file.relative_path == "Bundles/maps/maps.bundle" && file.bytes <= 512 * 1024 * 1024
        }) {
            return Err("Utility capture lacks its procedural scene".into());
        }
        road.verify_common_receipts(&document.sources, &document.extra_sources)?;
        let entries = road
            .procedural_order()?
            .iter()
            .filter(|entry| KINDS.contains(&entry.kind.as_str()));
        if entries.clone().count() != document.stages.len()
            || document
                .stages
                .iter()
                .map(|stage| stage.kind.as_str())
                .collect::<BTreeSet<_>>()
                != KINDS.into_iter().collect()
        {
            return Err("Utility stage coverage differs from its source workflow".into());
        }
        let mut paths = BTreeSet::new();
        for (program, entry) in document.stages.iter().zip(entries) {
            if cancelled() {
                return Err("Utility preparation cancelled".into());
            }
            if program.object_id != entry.object_id
                || program.order != entry.order
                || program.kind != entry.kind
                || program.description != entry.description
                || program.mode != entry.mode
                || program.disabled != entry.disabled
                || program.hierarchy_active != entry.hierarchy_active
                || program.component_enabled != entry.component_enabled
            {
                return Err("Utility program differs from its procedural entry".into());
            }
            program.validate_resources(&mut paths)?;
        }
        if paths != document.prefabs.keys().cloned().collect() {
            return Err("Utility prefab coverage differs from its attachment rules".into());
        }
        let mut prefabs = BTreeMap::new();
        let mut wire_guids = BTreeSet::new();
        for (path, prefab) in &document.prefabs {
            if cancelled() {
                return Err("Utility preparation cancelled".into());
            }
            validate_runtime(&prefab.runtime)?;
            for component in &prefab.runtime {
                let node = component.component.as_array().is_some_and(|owner| {
                    owner.len() == 3
                        && owner[0] == "Assembly-CSharp"
                        && owner[1] == ""
                        && owner[2] == "PowerlineNode"
                });
                if node {
                    let fields: serde_json::Value = serde_json::from_str(
                        component
                            .fields
                            .as_ref()
                            .ok_or("Missing utility node fields")?
                            .get(),
                    )
                    .map_err(|e| e.to_string())?;
                    let reference = fields
                        .get("WirePrefab")
                        .and_then(serde_json::Value::as_object)
                        .filter(|reference| reference.len() == 1)
                        .ok_or("Invalid utility wire reference")?;
                    let guid = reference
                        .get("guid")
                        .and_then(serde_json::Value::as_str)
                        .ok_or("Invalid utility wire GUID")?;
                    if !document.wire_resources.contains_key(guid) {
                        return Err(
                            "Utility wire reference is absent from its resource roster".into()
                        );
                    }
                    wire_guids.insert(guid.to_owned());
                }
            }
            let placement: road_object_source::Prefab =
                serde_json::from_str(prefab.placement.get()).map_err(|e| e.to_string())?;
            if placement.attributes.len() > 4096
                || placement.components.len() > 256
                || placement.issues.len() > 4096
            {
                return Err("Utility placement metadata exceeds its record bounds".into());
            }
            let compiled = decor_source::compile_road_prefab(&placement)?;
            if !compiled.unsupported.is_empty() {
                return Err(format!(
                    "Unsupported utility fitting: {}",
                    compiled.unsupported.join(", ")
                ));
            }
            let fitting = papyrus_terrain::decor_fit::PreparedPrefab::new(compiled)?;
            let terrain = decor_source::compile_road_effects(&placement, flags, protected)?;
            if !terrain.unsupported.is_empty() {
                return Err(format!(
                    "Unsupported utility terrain effects: {}",
                    terrain.unsupported.join(", ")
                ));
            }
            prefabs.insert(path.clone(), Attachment { fitting, terrain });
        }
        if wire_guids != document.wire_resources.keys().cloned().collect() {
            return Err("Utility wire resources differ from their node references".into());
        }
        for (guid, path) in &document.wire_resources {
            if guid.len() != 32
                || !guid
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                || !path.starts_with("assets/")
                || !path.ends_with(".prefab")
                || path.len() > 512
                || path.contains(['\\', '\0', ':'])
                || path != &path.to_lowercase()
                || path.split('/').any(|part| matches!(part, "" | "." | ".."))
            {
                return Err("Invalid utility wire resource identity".into());
            }
        }
        if document.wire_resources.values().collect::<BTreeSet<_>>()
            != document.wire_prefabs.keys().collect()
        {
            return Err("Utility wire prefab coverage differs from its resource references".into());
        }
        for rows in document.wire_prefabs.values() {
            validate_runtime(rows)?;
        }
        if cancelled() {
            return Err("Utility preparation cancelled".into());
        }
        Ok(Self {
            document,
            prefabs,
            sha256: format!("{:x}", Sha256::digest(bytes)),
        })
    }
    pub fn load(
        root: &Path,
        path: &Path,
        catalog: &Catalog,
        road: &road_object_source::Source,
        flags: &BTreeMap<String, u32>,
        protected: u32,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        let _verification = installation::VerificationScope::default();
        if cancelled() {
            return Err("Utility preparation cancelled".into());
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        let source = Self::read(&bytes, road, flags, protected, &cancelled)?;
        catalog.installation.validate()?;
        if !source
            .document
            .sources
            .contains(&catalog.installation.native)
            || catalog
                .installation
                .scene_manifest
                .as_ref()
                .is_none_or(|manifest| !source.document.sources.contains(manifest))
            || !catalog
                .sources
                .iter()
                .filter(|file| file.relative_path == "Bundles/shared/assetscenes.bundle")
                .any(|file| source.document.sources.contains(file))
        {
            return Err("Utility capture differs from the selected catalog".into());
        }
        for path in GAMEPLAY_FILES {
            if root.join(path).is_file()
                != source
                    .document
                    .sources
                    .iter()
                    .any(|file| file.relative_path == path)
            {
                return Err("Utility capture differs from the gameplay file roster".into());
            }
        }
        for file in source
            .document
            .sources
            .iter()
            .chain(&source.document.extra_sources)
        {
            if installation::hash_file(
                root,
                &file.relative_path,
                8 * 1024 * 1024 * 1024,
                &cancelled,
            )? != *file
            {
                return Err("Utility files differ from the selected installation".into());
            }
        }
        if cancelled() {
            return Err("Utility preparation cancelled".into());
        }
        Ok(source)
    }
}

fn find_cached(
    root: &Path,
    catalog: &Catalog,
    road: &road_object_source::Source,
    flags: &BTreeMap<String, u32>,
    protected: u32,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Source>, String> {
    let cache = crate::storage::local_data()?.join(CACHE);
    for path in crate::object_extract::documents(&cache, "source.json")? {
        if cancelled() {
            return Err("Utility preparation cancelled".into());
        }
        if let Ok(source) = Source::load(root, &path, catalog, road, flags, protected, cancelled)
            && path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                == Some(source.sha256())
        {
            return Ok(Some(source));
        }
    }
    if cancelled() {
        return Err("Utility preparation cancelled".into());
    }
    Ok(None)
}
/// Cache-only access for generation. Extraction belongs to installation preparation.
pub fn available(
    root: &Path,
    catalog: &Catalog,
    road: &road_object_source::Source,
    flags: &BTreeMap<String, u32>,
    protected: u32,
    cancelled: impl Fn() -> bool,
) -> Result<Source, String> {
    let _verification = installation::VerificationScope::default();
    find_cached(root, catalog, road, flags, protected, &cancelled)?.ok_or_else(|| {
        "Prepared utilities are unavailable. Reload the selected Rust installation.".into()
    })
}
pub fn prepare(
    root: &Path,
    catalog: &Catalog,
    road: &road_object_source::Source,
    flags: &BTreeMap<String, u32>,
    protected: u32,
    cancelled: impl Fn() -> bool,
) -> Result<Source, String> {
    let _verification = installation::VerificationScope::default();
    if let Some(source) = find_cached(root, catalog, road, flags, protected, &cancelled)? {
        return Ok(source);
    }
    road.procedural_order()?;
    if cancelled() {
        return Err("Utility preparation cancelled".into());
    }
    let local = crate::storage::local_data()?;
    std::fs::create_dir_all(&local).map_err(|e| e.to_string())?;
    let temporary = tempfile::Builder::new()
        .prefix(".utility-prepare-")
        .tempdir_in(&local)
        .map_err(|e| e.to_string())?;
    let output = temporary.path().join("capture");
    if !metadata_runtime::run(
        metadata_runtime::Script::Powerlines,
        &[
            "--rust-root".into(),
            root.as_os_str().to_owned(),
            "--stage-layout".into(),
            road.stage_layout().unwrap_or("legacy").into(),
            "--output".into(),
            output.as_os_str().to_owned(),
        ],
        &cancelled,
    )? {
        return Err("Utility preparation cancelled".into());
    }
    let path = output.join("source.json");
    let source = Source::load(root, &path, catalog, road, flags, protected, &cancelled)?;
    if cancelled() {
        return Err("Utility preparation cancelled".into());
    }
    metadata_runtime::publish_unkeyed(&path, &local.join(CACHE), source.sha256(), LIMIT)?;
    Ok(source)
}

#[cfg(test)]
mod tests;
