//! Reusable, installation-owned authored monument geometry and display phrases.
use crate::{
    installation::{self, SourceFile},
    metadata_runtime::{self, Script},
    monument_tag_geometry::Prepared,
    prefab_assets::Catalog,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

const LIMIT: u64 = 32 * 1024 * 1024;
const CACHE: &str = "papyrus-monument-tag-cache";
const MANIFEST: &str = "Bundles/AssetSceneManifest.json";
const SCENES: &str = "Bundles/shared/assetscenes.bundle";
const PREVIOUS_PRODUCER: &str = "8bfcfa2d9433e0ff97fe209aa2ee9a484632828f5782580a00febf8022981987";
const EQUIVALENT_PRODUCER: &str = "e70394ec0969120c2dadcbbde4fba81f399103a92d1f3a95e617e16321bcbcde";
const BASELINE_PRODUCER: &str = "c7441ea6e2d9905f1fff7b1328d080f4c4b57f50ab5dcc2d5933c1fafa791135";
const INDEXED_PRODUCER: &str = "4813fb2486a658027abcb9cb19275c4034670e7be6fbdfc6006c8935da9f8d0e";
const AFFINE_PRODUCER: &str = "ba56abb291cc8636acd7d21cd0676b68450d3818eb890b8709039ecbd1f6c6fb";

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
    catalog_sha256: String,
    prefabs: BTreeMap<String, Box<serde_json::value::RawValue>>,
}
pub struct Source {
    catalog_sha256: String,
    sources: Vec<SourceFile>,
    prefabs: BTreeMap<String, Prepared>,
    sha256: String,
}

struct Retained {
    root: PathBuf,
    cache: PathBuf,
    catalog_sha256: String,
    source: Arc<Source>,
}
static RETAINED: Mutex<Option<Retained>> = Mutex::new(None);

pub(crate) fn clear_session() {
    if let Ok(mut retained) = RETAINED.lock() {
        *retained = None;
    }
}

fn retain(root: &Path, cache: &Path, catalog: &Catalog, source: Source) -> Arc<Source> {
    let source = Arc::new(source);
    if let Ok(mut retained) = RETAINED.lock() {
        *retained = Some(Retained {
            root: root.to_owned(),
            cache: cache.to_owned(),
            catalog_sha256: catalog.sha256.clone(),
            source: source.clone(),
        });
    }
    source
}

fn reopen_retained(
    cache: &Path,
    root: &Path,
    source: Arc<Source>,
    cancelled: &impl Fn() -> bool,
) -> Option<Arc<Source>> {
    let directory = cache.join(source.sha256());
    if crate::client_cache::ordinary(cache).ok() != Some(true)
        || crate::client_cache::ordinary(&directory).ok() != Some(true)
        || !std::fs::symlink_metadata(directory.join("source.json"))
            .is_ok_and(|metadata| metadata.file_type().is_file())
        || !installation::hash_file(&directory, "source.json", LIMIT, cancelled)
            .is_ok_and(|receipt| receipt.sha256 == source.sha256)
        || source.verify_installation(root, cancelled).is_err()
    {
        return None;
    }
    Some(source)
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
impl Source {
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    pub fn captured_catalog_sha256(&self) -> &str {
        &self.catalog_sha256
    }
    pub fn prefabs(&self) -> &BTreeMap<String, Prepared> {
        &self.prefabs
    }
    fn read(bytes: &[u8], catalog: &Catalog, cancelled: impl Fn() -> bool) -> Result<Self, String> {
        if bytes.len() as u64 > LIMIT {
            return Err("Monument tag source exceeds 32 MiB".into());
        }
        let document: Document =
            serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        let producer = metadata_runtime::monument_tag_extractor_sha256();
        // Indexed reads and affine products emit the same geometry.
        // Limit reuse to the reviewed producer pairs.
        let compatible = document.extractor_sha256 == producer
            || (matches!(producer.as_str(), AFFINE_PRODUCER | INDEXED_PRODUCER)
                && [
                    INDEXED_PRODUCER,
                    PREVIOUS_PRODUCER,
                    EQUIVALENT_PRODUCER,
                    BASELINE_PRODUCER,
                ]
                .contains(&document.extractor_sha256.as_str()));
        if document.format != "mapper-monument-tag-reference"
            || document.schema != 1
            || !document.inspection_only
            || document.game_code_executed
            || !compatible
            || document.catalog_sha256.len() != 64
            || !document
                .catalog_sha256
                .bytes()
                .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
            || document.prefabs.is_empty()
            || document.prefabs.len() > 512
            || document.prefabs.len() != catalog.entries.len()
            || document
                .prefabs
                .keys()
                .any(|path| !catalog.entries.contains_key(path))
        {
            return Err("Monument tag source layout, producer or prefab roster differs".into());
        }
        installation::validate_source_receipts(&document.sources, &document.extra_sources)?;
        catalog.installation.validate()?;
        let manifest = catalog
            .installation
            .scene_manifest
            .as_ref()
            .ok_or("Missing selected scene manifest")?;
        let scenes = catalog
            .sources
            .iter()
            .find(|file| file.relative_path == SCENES)
            .ok_or("Missing selected prefab scene source")?;
        if !document.sources.contains(manifest) || !document.sources.contains(scenes) {
            return Err("Monument tag geometry differs from the selected catalog".into());
        }
        let mut prefabs = BTreeMap::new();
        for (path, raw) in document.prefabs {
            if cancelled() {
                return Err("Monument tag preparation cancelled".into());
            }
            prefabs.insert(path, Prepared::read(raw.get().as_bytes())?);
        }
        Ok(Self {
            catalog_sha256: document.catalog_sha256,
            // The terrain and code receipts describe provenance. The manifest,
            // prefab nodes and referenced meshes define the captured geometry.
            sources: document.sources.into_iter()
                .filter(|file| matches!(file.relative_path.as_str(), MANIFEST | SCENES))
                .chain(document.extra_sources)
                .collect(),
            prefabs,
            sha256: hash(bytes),
        })
    }
    pub fn load(
        root: &Path,
        path: &Path,
        catalog: &Catalog,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        let _verification = installation::VerificationScope::default();
        if cancelled() {
            return Err("Monument tag preparation cancelled".into());
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|error| error.to_string())?
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        let source = Self::read(&bytes, catalog, &cancelled)?;
        source.verify_installation(root, &cancelled)?;
        Ok(source)
    }

    fn verify_installation(&self, root: &Path, cancelled: &impl Fn() -> bool) -> Result<(), String> {
        for expected in &self.sources {
            if installation::hash_file(
                root,
                &expected.relative_path,
                8 * 1024 * 1024 * 1024,
                cancelled,
            )? != *expected
            {
                return Err(
                    "Monument tag geometry files differ from the selected installation".into(),
                );
            }
        }
        if cancelled() {
            return Err("Monument tag preparation cancelled".into());
        }
        Ok(())
    }
}

pub fn prepare(
    root: &Path,
    catalog_path: &Path,
    catalog: &Catalog,
    cancelled: impl Fn() -> bool,
) -> Result<Arc<Source>, String> {
    if cancelled() {
        return Err("Monument tag preparation cancelled".into());
    }
    prepare_in(
        root,
        || Ok(catalog_path.to_owned()),
        catalog,
        &crate::storage::local_data()?,
        cancelled,
    )
}

pub(crate) fn prepare_cached(
    root: &Path,
    catalog: &Catalog,
    cancelled: impl Fn() -> bool,
) -> Result<Arc<Source>, String> {
    let local = crate::storage::local_data()?;
    prepare_in(
        root,
        || crate::asset_discovery::catalog_path(&local.join("papyrus-prefab-cache"), &catalog.sha256),
        catalog,
        &local,
        cancelled,
    )
}

/// Reuse startup data without starting an extractor during generation.
pub fn available(
    root: &Path,
    catalog: &Catalog,
    cancelled: impl Fn() -> bool,
) -> Result<Arc<Source>, String> {
    let _verification = installation::VerificationScope::default();
    find_cached(root, catalog, &crate::storage::local_data()?, cancelled)?
        .ok_or_else(|| "Prepared monument volumes are unavailable. Reload the selected Rust installation.".into())
}

fn find_cached(
    root: &Path,
    catalog: &Catalog,
    local: &Path,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Arc<Source>>, String> {
    if cancelled() {
        return Err("Monument tag preparation cancelled".into());
    }
    let cache = local.join(CACHE);
    let retained = RETAINED.lock().ok().and_then(|entry| {
        entry
            .as_ref()
            .filter(|entry| {
                entry.root == root && entry.cache == cache && entry.catalog_sha256 == catalog.sha256
            })
            .map(|entry| entry.source.clone())
    });
    if let Some(source) = retained
        && let Some(verified) = reopen_retained(&cache, root, source, &cancelled)
    {
        return Ok(Some(verified));
    }
    for path in crate::object_extract::documents(&cache, "source.json")? {
        if cancelled() {
            return Err("Monument tag preparation cancelled".into());
        }
        if let Ok(source) = Source::load(root, &path, catalog, &cancelled)
            && path.parent().and_then(Path::file_name).and_then(|name| name.to_str())
                == Some(source.sha256())
        {
            return Ok(Some(retain(root, &cache, catalog, source)));
        }
    }
    if cancelled() {
        return Err("Monument tag preparation cancelled".into());
    }
    Ok(None)
}

fn prepare_in(
    root: &Path,
    catalog_path: impl FnOnce() -> Result<std::path::PathBuf, String>,
    catalog: &Catalog,
    local: &Path,
    cancelled: impl Fn() -> bool,
) -> Result<Arc<Source>, String> {
    let _verification = installation::VerificationScope::default();
    if cancelled() {
        return Err("Monument tag preparation cancelled".into());
    }
    let cache = local.join(CACHE);
    if let Some(source) = find_cached(root, catalog, local, &cancelled)? {
        return Ok(source);
    }
    if cancelled() {
        return Err("Monument tag preparation cancelled".into());
    }
    let catalog_path = catalog_path()?;
    let mut catalog_bytes = Vec::new();
    std::fs::File::open(&catalog_path)
        .map_err(|error| error.to_string())?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut catalog_bytes)
        .map_err(|error| error.to_string())?;
    if catalog_bytes.len() > 16 * 1024 * 1024 || hash(&catalog_bytes) != catalog.sha256 {
        return Err("Selected monument tag catalog document differs".into());
    }
    std::fs::create_dir_all(local).map_err(|error| error.to_string())?;
    let temporary = tempfile::Builder::new()
        .prefix(".monument-tag-prepare-")
        .tempdir_in(local)
        .map_err(|error| error.to_string())?;
    let output = temporary.path().join("capture");
    if !metadata_runtime::run(
        Script::MonumentTags,
        &[
            "--rust-root".into(),
            root.as_os_str().to_owned(),
            "--catalog".into(),
            catalog_path.as_os_str().to_owned(),
            "--output".into(),
            output.as_os_str().to_owned(),
        ],
        &cancelled,
    )? {
        return Err("Monument tag preparation cancelled".into());
    }
    let path = output.join("source.json");
    let source = Source::load(root, &path, catalog, &cancelled)?;
    if cancelled() {
        return Err("Monument tag preparation cancelled".into());
    }
    metadata_runtime::publish_unkeyed(&path, &cache, source.sha256(), LIMIT)?;
    Ok(retain(root, &cache, catalog, source))
}

#[cfg(test)]
mod tests;
