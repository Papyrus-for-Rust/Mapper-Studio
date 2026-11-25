//! Installation-bound ordered connector preparation. Never changes a recipe.
use crate::{
    infrastructure::Source,
    metadata_runtime::{self, Script},
    prefab_assets::Catalog,
    source_road_assets::{Assets, valid_digest},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
};
const LIMIT: u64 = 16 * 1024 * 1024;
const FILE: &str = "layout-assets.json";
const CACHE: &str = "papyrus-road-layout-cache";
#[derive(Clone, Copy)]
pub struct Context<'a> {
    pub root: &'a Path,
    pub catalog: &'a Catalog,
    pub infrastructure: &'a Source,
}
pub struct MetadataFiles<'a> {
    pub catalog: &'a Path,
    pub infrastructure: &'a Path,
}
fn read(path: &Path) -> Result<Vec<u8>, String> {
    let mut raw = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(LIMIT + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() as u64 > LIMIT {
        return Err("Ordered road cache document exceeds 16 MiB".into());
    }
    Ok(raw)
}
fn hash(raw: &[u8]) -> String {
    format!("{:x}", Sha256::digest(raw))
}
fn check(context: Context<'_>, pin: Option<&str>) -> Result<(), String> {
    if context.infrastructure.catalog_sha256 != context.catalog.sha256
        || !valid_digest(&context.infrastructure.sha256)
        || !valid_digest(&context.catalog.sha256)
        || pin.is_some_and(|p| !valid_digest(p))
    {
        return Err("Ordered road cache context or pin differs from verified metadata".into());
    }
    Ok(())
}
/// Metadata-only scan; root bundles/code are verified once after selection.
fn find(
    context: Context<'_>,
    cache: &Path,
    pin: Option<&str>,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<(PathBuf, Assets)>, String> {
    check(context, pin)?;
    if cancelled() {
        return Ok(None);
    }
    if pin.is_none() && !cache.exists() {
        return Ok(None);
    }
    let mut paths = Vec::new();
    if let Some(pin) = pin {
        let path = crate::client_cache::pinned_document(
            cache.parent().ok_or("Missing ordered road cache parent")?,
            CACHE,
            pin,
            FILE,
        )?;
        if path.is_file() {
            paths.push(path);
        }
    } else {
        for (index, entry) in std::fs::read_dir(cache)
            .map_err(|e| e.to_string())?
            .enumerate()
        {
            if index >= 256 {
                return Err("Too many ordered road cache snapshots".into());
            }
            if cancelled() {
                return Ok(None);
            }
            let entry = entry.map_err(|e| e.to_string())?;
            if !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
                continue;
            }
            let id = entry.file_name();
            if !id.to_str().is_some_and(valid_digest) {
                continue;
            }
            let path = entry.path().join(FILE);
            if path.is_file() {
                paths.push(path);
            }
        }
        paths.sort();
    }
    #[derive(Deserialize)]
    struct Header {
        catalog_sha256: String,
        infrastructure_sha256: String,
    }
    let mut selected: Option<(PathBuf, Assets)> = None;
    let extractor = metadata_runtime::road_layout_extractor_sha256();
    for path in paths {
        if cancelled() {
            return Ok(None);
        }
        let raw = read(&path)?;
        let id = hash(&raw);
        if path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|p| p.to_str())
            != Some(id.as_str())
        {
            return Err("Ordered road cache content changed; existing files retained".into());
        }
        let header: Header =
            serde_json::from_slice(&raw).map_err(|e| format!("Invalid ordered road cache: {e}"))?;
        if header.catalog_sha256 != context.catalog.sha256
            || header.infrastructure_sha256 != context.infrastructure.sha256
        {
            if pin.is_some() {
                return Err(
                    "Pinned ordered road assets belong to different source metadata".into(),
                );
            }
            continue;
        }
        let Some(assets) =
            Assets::decode(&raw, context.catalog, context.infrastructure, cancelled)?
        else {
            return Ok(None);
        };
        if pin.is_none() && assets.extractor_sha256() != extractor {
            continue;
        }
        if let Some((_, previous)) = &selected {
            if !assets.same_prefabs(previous)? {
                return Err(
                    "Conflicting ordered road snapshots for the same selected source".into(),
                );
            }
        } else {
            selected = Some((path, assets));
        }
    }
    Ok(selected)
}
pub fn cached(
    context: Context<'_>,
    pin: &str,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Assets>, String> {
    let local = crate::storage::local_data()?;
    cached_in(context, &local, pin, cancelled)
}
pub fn cached_in(
    context: Context<'_>,
    local: &Path,
    pin: &str,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Assets>, String> {
    let Some((_, assets)) = find(context, &local.join(CACHE), Some(pin), &cancelled)? else {
        if cancelled() {
            return Ok(None);
        }
        return Err("Pinned ordered road snapshot is unavailable".into());
    };
    if assets
        .verify_installation(context.root, &cancelled)?
        .is_none()
    {
        return Ok(None);
    }
    Ok(Some(assets))
}
/// Resolve normal metadata caches automatically; no extra asset-file picker.
pub fn prepare(
    context: Context<'_>,
    pin: Option<&str>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Assets>, String> {
    let local = crate::storage::local_data()?;
    prepare_with(context, &local, pin, &cancelled, |output| {
        let catalog = crate::asset_discovery::catalog_path(
            &local.join("papyrus-prefab-cache"),
            &context.catalog.sha256,
        )?;
        let infrastructure = crate::client_cache::pinned_document(
            &local,
            "papyrus-infrastructure-cache",
            &context.infrastructure.sha256,
            "infrastructure.json",
        )?;
        extract(
            context,
            MetadataFiles {
                catalog: &catalog,
                infrastructure: &infrastructure,
            },
            output,
            &cancelled,
        )
    })
}
pub fn prepare_in(
    context: Context<'_>,
    files: MetadataFiles<'_>,
    local: &Path,
    pin: Option<&str>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Assets>, String> {
    prepare_with(context, local, pin, &cancelled, |output| {
        extract(context, files, output, &cancelled)
    })
}
fn extract(
    context: Context<'_>,
    files: MetadataFiles<'_>,
    output: &Path,
    cancelled: &impl Fn() -> bool,
) -> Result<bool, String> {
    if cancelled() {
        return Ok(false);
    }
    if hash(&read(files.catalog)?) != context.catalog.sha256
        || hash(&read(files.infrastructure)?) != context.infrastructure.sha256
    {
        return Err("Ordered road extraction metadata files changed".into());
    }
    metadata_runtime::run(
        Script::RoadLayout,
        &[
            "--rust-root".into(),
            context.root.as_os_str().into(),
            "--catalog".into(),
            files.catalog.as_os_str().into(),
            "--infrastructure".into(),
            files.infrastructure.as_os_str().into(),
            "--cache-root".into(),
            output.as_os_str().into(),
        ],
        cancelled,
    )
}
fn prepare_with(
    context: Context<'_>,
    local: &Path,
    pin: Option<&str>,
    cancelled: &impl Fn() -> bool,
    extract: impl FnOnce(&Path) -> Result<bool, String>,
) -> Result<Option<Assets>, String> {
    check(context, pin)?;
    if cancelled() {
        return Ok(None);
    }
    let _progress = crate::loading_progress::begin("Preparing ordered road connectors", None);
    let cache = local.join(CACHE);
    if let Some((_, assets)) = find(context, &cache, pin, cancelled)? {
        if assets
            .verify_installation(context.root, cancelled)?
            .is_none()
        {
            return Ok(None);
        }
        return Ok(Some(assets));
    }
    if cancelled() {
        return Ok(None);
    }
    std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
    let temporary = tempfile::Builder::new()
        .prefix(".ordered-road-prepare-")
        .tempdir_in(&cache)
        .map_err(|e| e.to_string())?;
    let output = temporary.path().join("output");
    if !extract(&output)? || cancelled() {
        return Ok(None);
    }
    publish_prepared(context, local, &output, pin, cancelled)
}

pub(crate) fn publish_prepared(
    context: Context<'_>,
    local: &Path,
    output: &Path,
    pin: Option<&str>,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Assets>, String> {
    check(context, pin)?;
    if cancelled() {
        return Ok(None);
    }
    let document = metadata_runtime::document(output, FILE)?;
    let Some(assets) = Assets::load(
        &document,
        context.root,
        context.catalog,
        context.infrastructure,
        cancelled,
    )?
    else {
        return Ok(None);
    };
    if document
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|p| p.to_str())
        != Some(assets.sha256.as_str())
        || assets.extractor_sha256() != metadata_runtime::road_layout_extractor_sha256()
        || pin.is_some_and(|p| p != assets.sha256)
    {
        return Err(
            "Prepared ordered road assets differ from requested pin or extractor; recipe retained"
                .into(),
        );
    }
    if cancelled() {
        return Ok(None);
    }
    metadata_runtime::publish(&document, &local.join(CACHE), &assets.sha256, LIMIT)?;
    Ok(Some(assets))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    fn fixture(root: &Path) -> (Catalog, Source, Value) {
        let (mut catalog, mut source) = crate::infrastructure::tests::fixture();
        let mut receipts = Vec::new();
        let paths = [
            "RustClient_Data/Plugins/x86_64/RustNative.dll",
            "Bundles/AssetSceneManifest.json",
            "Bundles/shared/assetscenes.bundle",
            "GameAssembly.dll",
            "RustClient_Data/il2cpp_data/Metadata/global-metadata.dat",
            "Bundles/maps/maps.bundle",
        ];
        for (i, p) in paths.iter().enumerate() {
            let file = root.join(p);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, [i as u8]).unwrap();
            receipts.push(crate::installation::hash_file(root, p, 1, &|| false).unwrap());
        }
        catalog.sources = receipts[..3].to_vec();
        catalog.installation.native = receipts[0].clone();
        catalog.installation.scene_manifest = Some(receipts[1].clone());
        source["sources"] = serde_json::to_value(&catalog.sources).unwrap();
        source["profile"]["code_sources"] = serde_json::to_value(&receipts[3..5]).unwrap();
        source["profile"]["map_scene"] = serde_json::to_value(&receipts[5]).unwrap();
        let raw = serde_json::to_vec(&source).unwrap();
        let mut source: Source = serde_json::from_slice(&raw).unwrap();
        source.sha256 = hash(&raw);
        let prefab = catalog.entries.keys().next().unwrap();
        let doc = json!({"format":"papyrus-road-layout-assets","schema":1,"adapter":crate::source_road_assets::ADAPTER,
            "catalog_sha256":catalog.sha256,"infrastructure_sha256":source.sha256,"sources":receipts,
            "extractor_sha256":metadata_runtime::road_layout_extractor_sha256(),
            "prefabs":{prefab:{"version":1,"space":"prefab-root-local-unity","connectorOrder":"root-first-depth-first-component-order","complete":true,"nodesVisited":1,"issues":[],"monument":null,"connectors":[]}}});
        (catalog, source, doc)
    }
    fn publish_fake(output: &Path, doc: &Value) -> String {
        let raw = serde_json::to_vec(doc).unwrap();
        let id = hash(&raw);
        let folder = output.join(&id);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(FILE), raw).unwrap();
        id
    }
    #[test]
    fn verified_preparation_reuses_cache_keeps_pins_and_rejects_changed_installations() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("game");
        let local = temp.path().join("local");
        let (catalog, source, doc) = fixture(&root);
        let context = Context {
            root: &root,
            catalog: &catalog,
            infrastructure: &source,
        };
        let calls = std::cell::Cell::new(0);
        let first = prepare_with(context, &local, None, &|| false, |out| {
            calls.set(calls.get() + 1);
            publish_fake(out, &doc);
            Ok(true)
        })
        .unwrap()
        .unwrap();
        let cached = prepare_with(context, &local, Some(&first.sha256), &|| false, |_| {
            panic!("cache re-extracted")
        })
        .unwrap()
        .unwrap();
        assert_eq!(first.sha256, cached.sha256);
        assert_eq!(calls.get(), 1);
        let code_path = root.join("GameAssembly.dll");
        let original_code = std::fs::read(&code_path).unwrap();
        std::fs::write(&code_path, [99]).unwrap();
        let error = prepare_with(context, &local, None, &|| false, |_| {
            panic!("changed installation re-extracted against active old metadata")
        }).unwrap_err();
        assert!(error.contains("source changed"));
        std::fs::write(&code_path, original_code).unwrap();
        let reset = crate::client_cache::reset_in(&local).unwrap();
        let archive = reset.archive.unwrap();
        assert_eq!(reset.directories, 1);
        assert!(!local.join(CACHE).exists());
        assert_eq!(
            cached_in(context, &local, &first.sha256, || false)
                .unwrap()
                .unwrap()
                .sha256,
            first.sha256
        );
        std::fs::write(root.join("GameAssembly.dll"), [99]).unwrap();
        let error = prepare_with(context, &local, Some(&first.sha256), &|| false, |_| {
            panic!("changed installation re-extracted against archived old pins")
        })
        .unwrap_err();
        assert!(error.contains("source changed"));
        assert!(archive.join(CACHE).join(&first.sha256).join(FILE).exists());
        assert!(cached_in(context, &local, "../bad", || false).is_err());
    }
    #[test]
    fn cancellation_or_pin_mismatch_never_publishes_a_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("game");
        let local = temp.path().join("local");
        let (catalog, source, doc) = fixture(&root);
        let context = Context {
            root: &root,
            catalog: &catalog,
            infrastructure: &source,
        };
        assert!(
            prepare_with(context, &local, None, &|| true, |_| panic!(
                "cancelled extractor started"
            ))
            .unwrap()
            .is_none()
        );
        assert!(!local.exists());
        let cancelled = std::cell::Cell::new(false);
        assert!(
            prepare_with(context, &local, None, &|| cancelled.get(), |out| {
                publish_fake(out, &doc);
                cancelled.set(true);
                Ok(true)
            })
            .unwrap()
            .is_none()
        );
        assert_eq!(std::fs::read_dir(local.join(CACHE)).unwrap().count(), 0);
        let error = prepare_with(context, &local, Some(&"a".repeat(64)), &|| false, |out| {
            publish_fake(out, &doc);
            Ok(true)
        })
        .unwrap_err();
        assert!(error.contains("requested pin"));
        assert_eq!(std::fs::read_dir(local.join(CACHE)).unwrap().count(), 0);
    }
    #[test]
    fn stale_extractors_stay_readable_by_pin_and_conflicting_or_corrupt_caches_fail() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("game");
        let local = temp.path().join("local");
        let cache = local.join(CACHE);
        let (catalog, source, doc) = fixture(&root);
        let context = Context {
            root: &root,
            catalog: &catalog,
            infrastructure: &source,
        };
        let mut old = doc.clone();
        old["extractor_sha256"] = "e".repeat(64).into();
        let old_pin = publish_fake(&cache, &old);
        assert_eq!(
            prepare_with(context, &local, Some(&old_pin), &|| false, |_| panic!(
                "old pin replaced"
            ))
            .unwrap()
            .unwrap()
            .sha256,
            old_pin
        );
        let fresh = prepare_with(context, &local, None, &|| false, |out| {
            publish_fake(out, &doc);
            Ok(true)
        })
        .unwrap()
        .unwrap();
        assert_ne!(fresh.sha256, old_pin);
        assert!(cache.join(old_pin).is_dir());
        let mut conflict = doc.clone();
        let prefab = catalog.entries.keys().next().unwrap();
        conflict["prefabs"][prefab]["connectors"] = json!([{"objectId":999,"infrastructureType":0,"position":[0.,0.,0.],"forward":[0.,0.,1.],"hierarchyActiveWithoutRoot":true}]);
        let bad_pin = publish_fake(&cache, &conflict);
        assert!(
            prepare_with(context, &local, None, &|| false, |_| panic!(
                "ambiguous data re-extracted"
            ))
            .unwrap_err()
            .contains("Conflicting")
        );
        assert!(
            cached_in(context, &local, &fresh.sha256, || false)
                .unwrap()
                .is_some()
        );
        std::fs::write(cache.join(bad_pin).join(FILE), b"changed").unwrap();
        assert!(
            prepare_with(context, &local, None, &|| false, |_| panic!(
                "corrupt data overwritten"
            ))
            .unwrap_err()
            .contains("content changed")
        );
    }
}
