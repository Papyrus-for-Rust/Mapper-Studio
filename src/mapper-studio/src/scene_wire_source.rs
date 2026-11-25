//! Installation-owned cable nodes, prepared once and reused through AppData.
use crate::{
    installation::{self, SourceFile},
    metadata_runtime,
    powerline_source::{self, RuntimeComponent, WireTemplate},
    prefab_assets::Catalog,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
};

const LIMIT: u64 = 64 * 1024 * 1024;
const CACHE: &str = "papyrus-scene-wire-cache";
const MANIFEST: &str = "Bundles/AssetSceneManifest.json";
const ASSET_SCENES: &str = "Bundles/shared/assetscenes.bundle";
const CONTENT: &str = "Bundles/shared/content.bundle";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    format: String,
    schema: u32,
    inspection_only: bool,
    game_code_executed: bool,
    physics_executed: bool,
    sources: Vec<SourceFile>,
    extra_sources: Vec<SourceFile>,
    extractor_sha256: String,
    prefabs: BTreeMap<String, Vec<RuntimeComponent>>,
    wire_resources: BTreeMap<String, String>,
    wire_prefabs: BTreeMap<String, Vec<RuntimeComponent>>,
}

pub struct Source {
    document: Document,
    templates: BTreeMap<String, Vec<WireTemplate>>,
    sha256: String,
}

fn valid_path(path: &str) -> bool {
    path.len() <= 512
        && path.starts_with("assets/")
        && path.ends_with(".prefab")
        && path == path.to_lowercase()
        && !path.contains(['\\', '\0', ':'])
        && !path.split('/').any(|part| matches!(part, "" | "." | ".."))
}

fn cancelled(stop: &impl Fn() -> bool) -> Result<(), String> {
    if stop() {
        Err("Cable preparation cancelled".into())
    } else {
        Ok(())
    }
}

impl Source {
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Empty entries explicitly record that a requested prefab has no active nodes.
    pub fn templates(&self) -> &BTreeMap<String, Vec<WireTemplate>> {
        &self.templates
    }

    pub fn wire_resources(&self) -> &BTreeMap<String, String> {
        &self.document.wire_resources
    }

    fn read(
        bytes: &[u8],
        required: &BTreeSet<String>,
        stop: &impl Fn() -> bool,
    ) -> Result<Self, String> {
        cancelled(stop)?;
        if bytes.len() as u64 > LIMIT || required.is_empty() || required.len() > 8192 {
            return Err("Invalid cable source size or prefab roster".into());
        }
        let document: Document = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if document.format != "mapper-scene-wire-source-reference"
            || document.schema != 1
            || !document.inspection_only
            || document.game_code_executed
            || document.physics_executed
            || !metadata_runtime::scene_wire_extractor_matches(&document.extractor_sha256)
            || document.sources.len() != 3
            || document.prefabs.len() > 8192
            || !required
                .iter()
                .all(|path| document.prefabs.contains_key(path))
            || document.prefabs.keys().any(|path| !valid_path(path))
            || document.wire_resources.len() > 256
            || document.wire_prefabs.len() > 256
        {
            return Err("Cable source identity or prefab coverage differs".into());
        }
        installation::validate_source_receipts(&document.sources, &document.extra_sources)?;
        let source_paths: BTreeSet<_> = document
            .sources
            .iter()
            .map(|s| s.relative_path.as_str())
            .collect();
        if source_paths.len() != 3
            || !source_paths.contains(MANIFEST)
            || !source_paths.contains(ASSET_SCENES)
            || !source_paths.iter().any(|path| {
                matches!(
                    *path,
                    "RustClient_Data/Plugins/x86_64/RustNative.dll"
                        | "RustDedicated_Data/Plugins/x86_64/RustNative.dll"
                )
            })
            || document.extra_sources.len() != usize::from(!document.wire_resources.is_empty())
            || document
                .extra_sources
                .iter()
                .any(|s| s.relative_path != CONTENT)
        {
            return Err("Cable source file coverage differs".into());
        }
        for (guid, path) in &document.wire_resources {
            if guid.len() != 32
                || !guid
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                || !valid_path(path)
            {
                return Err("Invalid cable resource identity".into());
            }
        }
        if document.wire_resources.values().collect::<BTreeSet<_>>()
            != document.wire_prefabs.keys().collect()
        {
            return Err("Cable resource prefab coverage differs".into());
        }
        let mut templates = BTreeMap::new();
        let mut resources = BTreeSet::new();
        let mut rows = 0usize;
        for (path, runtime) in &document.prefabs {
            cancelled(stop)?;
            rows += runtime.len();
            if rows > 200_000
                || runtime.iter().any(|row| {
                    let Some(identity) = row.component.as_array() else {
                        return true;
                    };
                    identity.len() != 3
                        || identity[0] != "Assembly-CSharp"
                        || identity[1] != ""
                        || !matches!(
                            identity[2].as_str(),
                            Some("PowerlineNode" | "PowerLineWireConnectionHelper")
                        )
                })
            {
                return Err("Invalid cable runtime component coverage".into());
            }
            let compiled =
                powerline_source::wire_templates_from_runtime(runtime, &document.wire_resources)?;
            resources.extend(compiled.iter().map(|node| node.wire_resource().to_owned()));
            templates.insert(path.clone(), compiled);
        }
        if resources != document.wire_resources.keys().cloned().collect() {
            return Err("Cable resources differ from active node references".into());
        }
        for runtime in document.wire_prefabs.values() {
            cancelled(stop)?;
            powerline_source::validate_runtime(runtime)?;
        }
        cancelled(stop)?;
        Ok(Self {
            document,
            templates,
            sha256: format!("{:x}", Sha256::digest(bytes)),
        })
    }

    pub fn load(
        root: &Path,
        path: &Path,
        catalog: &Catalog,
        required: &BTreeSet<String>,
        stop: impl Fn() -> bool,
    ) -> Result<Self, String> {
        let _verification = installation::VerificationScope::default();
        cancelled(&stop)?;
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        let source = Self::read(&bytes, required, &stop)?;
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
                .filter(|file| file.relative_path == ASSET_SCENES)
                .any(|file| source.document.sources.contains(file))
        {
            return Err("Cable source differs from the selected catalog".into());
        }
        for file in source
            .document
            .sources
            .iter()
            .chain(&source.document.extra_sources)
        {
            if installation::hash_file(root, &file.relative_path, 8 * 1024 * 1024 * 1024, &stop)?
                != *file
            {
                return Err("Cable source files differ from the selected installation".into());
            }
        }
        cancelled(&stop)?;
        Ok(source)
    }
}

#[derive(Deserialize)]
struct Manifest {
    #[serde(rename = "Scenes")]
    scenes: Vec<Scene>,
}
#[derive(Deserialize)]
struct Scene {
    #[serde(rename = "IncludedAssets")]
    assets: Vec<String>,
}

fn roster(
    root: &Path,
    catalog: &Catalog,
    stop: &impl Fn() -> bool,
) -> Result<BTreeSet<String>, String> {
    cancelled(stop)?;
    let receipt = installation::hash_file(root, MANIFEST, 32 * 1024 * 1024, stop)?;
    if catalog.installation.scene_manifest.as_ref() != Some(&receipt) {
        return Err("Cable prefab manifest differs from the selected catalog".into());
    }
    let mut raw = Vec::new();
    std::fs::File::open(root.join(MANIFEST))
        .map_err(|e| e.to_string())?
        .take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() as u64 != receipt.bytes || format!("{:x}", Sha256::digest(&raw)) != receipt.sha256
    {
        return Err("Cable prefab manifest changed during loading".into());
    }
    let manifest: Manifest = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    let mut requested: BTreeSet<_> = catalog.entries.keys().cloned().collect();
    for scene in manifest.scenes {
        cancelled(stop)?;
        for path in scene.assets {
            let path = path.to_lowercase();
            if path.starts_with("assets/bundled/prefabs/autospawn/") && path.ends_with(".prefab") {
                requested.insert(path);
            }
        }
    }
    if requested.is_empty()
        || requested.len() > 8192
        || requested.iter().any(|path| !valid_path(path))
    {
        return Err("Invalid installation cable prefab roster".into());
    }
    Ok(requested)
}

fn find_cached(
    root: &Path,
    catalog: &Catalog,
    required: &BTreeSet<String>,
    local: &Path,
    stop: &impl Fn() -> bool,
) -> Result<Option<Source>, String> {
    for path in crate::object_extract::documents(&local.join(CACHE), "source.json")? {
        cancelled(stop)?;
        if let Ok(source) = Source::load(root, &path, catalog, required, stop)
            && path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                == Some(source.sha256())
        {
            return Ok(Some(source));
        }
    }
    cancelled(stop)?;
    Ok(None)
}

/// Cache-only access. Missing preparation returns an actionable error.
pub fn available(
    root: &Path,
    catalog: &Catalog,
    stop: impl Fn() -> bool,
) -> Result<Source, String> {
    let _verification = installation::VerificationScope::default();
    let required = roster(root, catalog, &stop)?;
    find_cached(
        root,
        catalog,
        &required,
        &crate::storage::local_data()?,
        &stop,
    )?
    .ok_or_else(|| "Prepared cables are unavailable. Reload the selected Rust installation.".into())
}

pub fn prepare(root: &Path, catalog: &Catalog, stop: impl Fn() -> bool) -> Result<Source, String> {
    prepare_in(root, catalog, &crate::storage::local_data()?, stop)
}

fn prepare_in(
    root: &Path,
    catalog: &Catalog,
    local: &Path,
    stop: impl Fn() -> bool,
) -> Result<Source, String> {
    let _verification = installation::VerificationScope::default();
    let required = roster(root, catalog, &stop)?;
    if let Some(source) = find_cached(root, catalog, &required, local, &stop)? {
        return Ok(source);
    }
    cancelled(&stop)?;
    std::fs::create_dir_all(local).map_err(|e| e.to_string())?;
    let temporary = tempfile::Builder::new()
        .prefix(".cable-prepare-")
        .tempdir_in(local)
        .map_err(|e| e.to_string())?;
    let requested = temporary.path().join("prefabs.json");
    std::fs::write(
        &requested,
        serde_json::to_vec(&required).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let output = temporary.path().join("capture");
    if !metadata_runtime::run(
        metadata_runtime::Script::SceneWires,
        &[
            "--rust-root".into(),
            root.as_os_str().to_owned(),
            "--prefabs".into(),
            requested.into_os_string(),
            "--output".into(),
            output.as_os_str().to_owned(),
        ],
        &stop,
    )? {
        return Err("Cable preparation cancelled".into());
    }
    let path = output.join("source.json");
    let source = Source::load(root, &path, catalog, &required, &stop)?;
    cancelled(&stop)?;
    metadata_runtime::publish_unkeyed(&path, &local.join(CACHE), source.sha256(), LIMIT)?;
    Ok(source)
}

#[cfg(test)]
mod tests;
