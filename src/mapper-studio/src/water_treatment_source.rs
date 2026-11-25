//! Installation-bound pipe programs and reusable prefab preparation.
//! Loading serialized inputs does not certify native stage execution policy.
use crate::{
    decor_source,
    installation::{self, SourceFile},
    metadata_runtime,
    prefab_assets::Catalog,
    road_object_source,
};
use papyrus_terrain::{
    decor::{Kind, Stage},
    native_layers::View,
    roadside_decor::Settings,
    spawn::Filter,
    terrain_normal,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
};

const LIMIT: u64 = 16 * 1024 * 1024;
const GAMEPLAY_FILES: [&str; 4] = [
    "GameAssembly.dll",
    "RustClient_Data/il2cpp_data/Metadata/global-metadata.dat",
    "RustClient_Data/Managed/Assembly-CSharp.dll",
    "RustDedicated_Data/Managed/Assembly-CSharp.dll",
];
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Program {
    pub object_id: i64,
    pub order: usize,
    pub name: String,
    pub hierarchy_active: bool,
    pub mode: i32,
    #[serde(deserialize_with = "Option::deserialize")]
    pub disabled: Option<bool>,
    pub component_enabled: bool,
    pub folder: String,
    pub filter: Filter,
    pub anchor_mode: i32,
    pub road_type: u8,
    pub placement_attempts: u16,
    pub prefabs: Vec<String>,
    pub distance: f32,
    pub side_offset: f32,
    pub maximum_slope: f32,
    pub dithering: f32,
    pub prevent_building_padding: f32,
}
impl Program {
    pub fn descriptor(&self, index: usize, filter_mode: i32) -> Result<Stage, String> {
        let stage = Stage {
            id: self.object_id.to_string(),
            order: index,
            name: self.name.clone(),
            kind: Kind::Uniform,
            folder: self.folder.clone(),
            enabled: self.hierarchy_active
                && self.component_enabled
                && !self.disabled.unwrap_or(false),
            mode: self.mode,
            filter: self.filter.clone(),
            filter_mode,
            anchor_mode: self.anchor_mode,
            distance: self.distance,
            dithering: self.dithering,
            density: 0.,
            cluster: None,
            prefabs: self.prefabs.clone(),
        };
        stage.validate_path_attached()?;
        Ok(stage)
    }
    /// The owning code-policy adapter supplies the inspected spacing fraction.
    pub fn sampling(&self, spacing_fraction: f32) -> Result<Settings, String> {
        let settings = Settings {
            road_mode: self.road_type,
            distance: self.distance,
            spacing_fraction,
            side_offset: self.side_offset,
            dithering: self.dithering,
            placement_attempts: self.placement_attempts,
        };
        settings
            .validate()
            .map_err(|error| format!("Invalid pipe sampling settings {error:?}"))?;
        Ok(settings)
    }
    /// Ordinary terrain-frame GetSlope. Deep-sea data is outside this adapter.
    pub fn accepts_slope(&self, view: View<'_>, position: [f32; 3]) -> Result<bool, String> {
        if !self.maximum_slope.is_finite()
            || !(0. ..=90.).contains(&self.maximum_slope)
            || position.iter().any(|value| !value.is_finite())
        {
            return Err("Invalid pipe slope query".into());
        }
        let size = view.size as f32;
        let slope = terrain_normal::slope(
            view.size,
            view.height_resolution,
            view.height,
            [position[0] / size + 0.5, position[2] / size + 0.5],
        )?;
        Ok(slope <= self.maximum_slope)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    format: String,
    schema: u32,
    inspection_only: bool,
    game_code_executed: bool,
    scene: String,
    stage_layout: String,
    order_basis: String,
    collider_backfaces: bool,
    sources: Vec<SourceFile>,
    extra_sources: Vec<SourceFile>,
    extractor_sha256: String,
    stages: Vec<Program>,
    procedural_order: road_object_source::Workflow,
    prefabs: BTreeMap<String, Box<serde_json::value::RawValue>>,
}
pub struct Source {
    document: Document,
    prefabs: BTreeMap<String, decor_source::PathPrefab>,
    sha256: String,
}
// The procedural scene reader changed only the standalone monument capture.
// Pipe records from both versions match, including with LF-only scripts.
const VERIFIED_PRODUCERS: [&str; 4] = [
    "94ae60a3637e92c477e1f5bc4e368a2b6a3186cec84aed3dc4b4361975ec2179",
    "14d94082e7819a28ac5a813b3994a59ce7017fa6c9890aeb5a26c05731e9ff3f",
    "67241e867549cfddeb1a8f06ec3cc14b6c6f677fdeeb66b010cb7efb92133a79",
    "7c9e465c794baa8cda83c06ab8de32dbfe19646d159abaf22e5d2069a989b036",
];

fn producer_matches(capture: &str) -> bool {
    let current = metadata_runtime::water_treatment_extractor_sha256();
    capture == current
        || VERIFIED_PRODUCERS.contains(&current.as_str()) && VERIFIED_PRODUCERS.contains(&capture)
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
impl Source {
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    pub fn prefabs(&self) -> &BTreeMap<String, decor_source::PathPrefab> {
        &self.prefabs
    }
    pub fn programs(&self) -> &[Program] {
        &self.document.stages
    }
    pub fn collider_backfaces(&self) -> bool {
        self.document.collider_backfaces
    }
    fn read(bytes: &[u8], flags: &BTreeMap<String, u32>, protected: u32) -> Result<Self, String> {
        if bytes.len() as u64 > LIMIT {
            return Err("Pipe source exceeds 16 MiB".into());
        }
        let document: Document =
            serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        if document.format != "mapper-water-treatment-reference"
            || document.schema != 2
            || !document.inspection_only
            || document.game_code_executed
            || document.scene != "BuildPlayer-Procedural Map"
            || !matches!(
                document.stage_layout.as_str(),
                "legacy" | "mode-description"
            )
            || document.order_basis != "relative hierarchy procedural component order"
            || !producer_matches(&document.extractor_sha256)
            || document.stages.len() > 16
            || document.prefabs.len() > 4096
            || document.sources.len() > 64
            || document.extra_sources.len() > 64
        {
            return Err("Unsupported pipe capture layout or producer".into());
        }
        if !document.sources.iter().any(|source| {
            source.relative_path == "Bundles/maps/maps.bundle" && source.bytes <= 512 * 1024 * 1024
        }) {
            return Err("Pipe capture is missing its procedural map source".into());
        }
        installation::validate_source_receipts(&document.sources, &document.extra_sources)?;
        let mut ids = BTreeSet::new();
        let mut paths = BTreeSet::new();
        let mut previous = None;
        for (index, program) in document.stages.iter().enumerate() {
            program.descriptor(index, 2)?;
            program.sampling(0.25)?;
            if program.object_id <= 0
                || !ids.insert(program.object_id)
                || program.order > 65535
                || previous.is_some_and(|order| program.order <= order)
                || !program.maximum_slope.is_finite()
                || !(0. ..=90.).contains(&program.maximum_slope)
                || !program.prevent_building_padding.is_finite()
                || !(0. ..=10000.).contains(&program.prevent_building_padding)
                || !(0..=2).contains(&program.anchor_mode)
            {
                return Err("Invalid or unordered pipe program".into());
            }
            previous = Some(program.order);
            paths.extend(program.prefabs.iter().cloned());
        }
        if paths != document.prefabs.keys().cloned().collect() {
            return Err("Pipe prefab coverage differs from its program roster".into());
        }
        let mut prefabs = BTreeMap::new();
        for (path, raw) in &document.prefabs {
            prefabs.insert(
                path.clone(),
                decor_source::prepare_path_prefab(raw.get().as_bytes(), flags, protected)?,
            );
        }
        Ok(Self {
            document,
            prefabs,
            sha256: digest(bytes),
        })
    }
    fn verify_gameplay_roster(&self, root: &Path) -> Result<(), String> {
        for path in GAMEPLAY_FILES {
            if root.join(path).is_file()
                != self
                    .document
                    .sources
                    .iter()
                    .any(|source| source.relative_path == path)
            {
                return Err("Pipe capture differs from the installed gameplay file roster".into());
            }
        }
        Ok(())
    }
    pub fn verify_road(&self, road: &road_object_source::Source) -> Result<(), String> {
        let workflow = road.procedural_order()?;
        if road.stage_layout().unwrap_or("legacy") != self.document.stage_layout
            || &self.document.procedural_order
                != road.workflow.as_ref().ok_or("Missing road workflow")?
        {
            return Err("Pipe capture differs from the prepared road workflow".into());
        }
        road.verify_common_receipts(&self.document.sources, &self.document.extra_sources)?;
        let entries = workflow
            .iter()
            .filter(|entry| entry.kind == "PlaceWaterTreatmentPipes");
        if entries.clone().count() != self.document.stages.len() {
            return Err("Pipe stage coverage differs from its source workflow".into());
        }
        for (program, entry) in self.document.stages.iter().zip(entries) {
            if program.object_id != entry.object_id
                || program.order != entry.order
                || program.name != entry.description
                || program.mode != entry.mode
                || program.disabled != entry.disabled
                || program.hierarchy_active != entry.hierarchy_active
                || program.component_enabled != entry.component_enabled
            {
                return Err("Pipe program differs from its procedural entry".into());
            }
        }
        Ok(())
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
            return Err("Pipe preparation cancelled".into());
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        let source = Self::read(&bytes, flags, protected)?;
        source.verify_road(road)?;
        source.verify_gameplay_roster(root)?;
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
            return Err("Pipe capture differs from the selected catalog".into());
        }
        for expected in source
            .document
            .sources
            .iter()
            .chain(&source.document.extra_sources)
        {
            if installation::hash_file(
                root,
                &expected.relative_path,
                8 * 1024 * 1024 * 1024,
                &cancelled,
            )? != *expected
            {
                return Err("Pipe files differ from the selected installation".into());
            }
        }
        if cancelled() {
            return Err("Pipe preparation cancelled".into());
        }
        Ok(source)
    }
}

#[cfg(test)]
mod tests;

/// Reuse startup data without starting an extractor during generation.
pub fn available(
    root: &Path,
    catalog: &Catalog,
    road: &road_object_source::Source,
    flags: &BTreeMap<String, u32>,
    protected: u32,
    cancelled: impl Fn() -> bool,
) -> Result<Source, String> {
    let _verification = installation::VerificationScope::default();
    find_cached(root, catalog, road, flags, protected, cancelled)?
        .ok_or_else(|| "Prepared roadside pipes are unavailable. Reload the selected Rust installation.".into())
}

fn find_cached(
    root: &Path,
    catalog: &Catalog,
    road: &road_object_source::Source,
    flags: &BTreeMap<String, u32>,
    protected: u32,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Source>, String> {
    if cancelled() {
        return Err("Pipe preparation cancelled".into());
    }
    let local = crate::storage::local_data()?;
    let cache = local.join("papyrus-water-treatment-cache");
    for path in crate::object_extract::documents(&cache, "source.json")? {
        if cancelled() {
            return Err("Pipe preparation cancelled".into());
        }
        if let Ok(source) = Source::load(root, &path, catalog, road, flags, protected, &cancelled)
            && !source.collider_backfaces()
            && path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                == Some(source.sha256.as_str())
        {
            return Ok(Some(source));
        }
    }
    Ok(None)
}

/// Populate the persistent source cache. Calling this never enables a stage.
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
    if cancelled() {
        return Err("Pipe preparation cancelled".into());
    }
    let local = crate::storage::local_data()?;
    let cache = local.join("papyrus-water-treatment-cache");
    std::fs::create_dir_all(&local).map_err(|e| e.to_string())?;
    let temporary = tempfile::Builder::new()
        .prefix(".pipe-prepare-")
        .tempdir_in(&local)
        .map_err(|e| e.to_string())?;
    let output = temporary.path().join("capture");
    // Legacy prepared road workflows carry their activation fields directly.
    road.procedural_order()?;
    let layout = road.stage_layout().unwrap_or("legacy");
    if !metadata_runtime::run(
        metadata_runtime::Script::WaterTreatment,
        &[
            "--rust-root".into(),
            root.as_os_str().to_owned(),
            "--stage-layout".into(),
            layout.into(),
            "--output".into(),
            output.as_os_str().to_owned(),
        ],
        &cancelled,
    )? {
        return Err("Pipe preparation cancelled".into());
    }
    let path = output.join("source.json");
    let source = Source::load(root, &path, catalog, road, flags, protected, &cancelled)?;
    if cancelled() {
        return Err("Pipe preparation cancelled".into());
    }
    publish_capture(&path, &cache, &source)?;
    Ok(source)
}

fn publish_capture(path: &Path, cache: &Path, source: &Source) -> Result<(), String> {
    metadata_runtime::publish_unkeyed(path, cache, source.sha256(), LIMIT)?;
    Ok(())
}
