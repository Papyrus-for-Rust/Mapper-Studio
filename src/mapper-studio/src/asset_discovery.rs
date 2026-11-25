//! Finds already-extracted matching caches, then runs the normal source verification.
use crate::{
    AssetIdentity,
    installation::Installation,
    prefab_assets::{self, Catalog},
    prefab_textures::{Bindings, DAMAGED_PAYLOAD, PayloadError},
};
use serde::{Deserialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

/// Read-only packaging/support check; it does not inspect a game installation.
pub fn catalog_reader_status() -> serde_json::Value {
    crate::native_catalog::status()
}

fn documents(root: &Path, name: &str) -> Result<Vec<PathBuf>, String> {
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for (index, entry) in fs::read_dir(root)
        .map_err(|e| e.to_string())?
        .take(257)
        .enumerate()
    {
        let entry = entry.map_err(|e| e.to_string())?;
        if index >= 256 {
            return Err(
                "Too many cached asset snapshots; choose files in Advanced asset files".into(),
            );
        }
        let p = entry.path().join(name);
        if p.is_file() {
            paths.push(p);
        }
    }
    paths.sort();
    Ok(paths)
}
// Discovery only needs these fields. Serde validates and skips the potentially
// large prefab/texture bodies without allocating a second copy of their trees.
#[derive(Deserialize)]
struct CatalogHeader {
    format: String,
    adapter: Option<String>,
    extractor_sha256: Option<String>,
    sources: Vec<crate::installation::SourceFile>,
}
#[derive(Deserialize)]
struct TextureHeader {
    format: String,
    catalog_sha256: String,
}
enum AssetLookup {
    Missing,
    Ready(Catalog, Bindings),
    Damaged {
        catalog: Catalog,
        catalog_path: PathBuf,
        bindings: Bindings,
        cache_key: OsString,
    },
}
const MISSING_ASSETS: &str = "No matching extracted monument assets found. Advanced asset files accepts Mapper catalog.json and bindings.json, not Rust's AssetSceneManifest.json. Extraction tools are in tools/Mapper.Diagnostics.";
fn owned_texture_key<'a>(cache: &Path, receipt: &'a Path) -> Option<&'a OsStr> {
    let directory = receipt.parent()?;
    let key = directory.file_name()?;
    let hex = key.to_str()?;
    // The writer keys the folder by the catalog and source set. The receipt
    // digest used by saved projects identifies a separate object.
    if !valid_digest(hex)
        || crate::client_cache::ordinary(cache) != Ok(true)
        || crate::client_cache::ordinary(directory) != Ok(true)
        || directory.canonicalize().ok()? != cache.canonicalize().ok()?.join(key)
    {
        return None;
    }
    Some(key)
}

fn valid_digest(hex: &str) -> bool {
    hex.len() == 64
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn read_document(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("Cached asset document exceeds its size limit".into());
    }
    Ok(bytes)
}

fn read<T: DeserializeOwned>(path: &Path) -> Result<(T, String), String> {
    let bytes = read_document(path, 16 * 1024 * 1024)?;
    let v = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    Ok((v, format!("{:x}", Sha256::digest(bytes))))
}
/// Locate an already verified catalog by its exact document hash.
pub fn catalog_path(cache: &Path, sha: &str) -> Result<PathBuf, String> {
    if valid_digest(sha) {
        let path = cache.join(sha).join("catalog.json");
        if read::<serde::de::IgnoredAny>(&path).is_ok_and(|(_, actual)| actual == sha) {
            return Ok(path);
        }
    }
    for path in documents(cache, "catalog.json")? {
        if read::<serde::de::IgnoredAny>(&path).is_ok_and(|(_, actual)| actual == sha) {
            return Ok(path);
        }
    }
    if cache
        .file_name()
        .is_some_and(|n| n == "papyrus-prefab-cache")
        && let Some(local) = cache.parent()
    {
        for archive in crate::client_cache::archived_roots(local) {
            for path in documents(&archive.join("papyrus-prefab-cache"), "catalog.json")? {
                if read::<serde::de::IgnoredAny>(&path).is_ok_and(|(_, actual)| actual == sha) {
                    return Ok(path);
                }
            }
        }
    }
    Err("Verified catalog cache file is unavailable for extraction".into())
}
pub fn load_cached(
    root: &Path,
    source: &Installation,
    pinned: Option<&AssetIdentity>,
    cancelled: impl Fn() -> bool,
) -> Result<(Catalog, Bindings), String> {
    let local = crate::storage::local_data()?;
    match lookup_cached_in(root, source, pinned, &local, &cancelled)? {
        AssetLookup::Missing if pinned.is_some() => Err(
            "The recipe's saved assets are unavailable. Use the matching Rust installation and cache, or supply matching --catalog and --bindings."
                .into(),
        ),
        AssetLookup::Missing => Err(
            "No matching prepared assets. Run mapper installation prepare RUST_ROOT, or supply --catalog and --bindings."
                .into(),
        ),
        found => ready_assets(found),
    }
}

fn lookup_cached_in(
    root: &Path,
    source: &Installation,
    pinned: Option<&AssetIdentity>,
    local: &Path,
    cancelled: &impl Fn() -> bool,
) -> Result<AssetLookup, String> {
    let found = lookup(
        root,
        source,
        pinned,
        &local.join("papyrus-prefab-cache"),
        &local.join("papyrus-texture-cache"),
        cancelled,
    )?;
    if pinned.is_some() && matches!(found, AssetLookup::Missing) {
        for archive in crate::client_cache::archived_roots(local) {
            if cancelled() {
                return Err("Asset preparation cancelled".into());
            }
            if let Ok(pair @ AssetLookup::Ready(..)) = lookup(
                root,
                source,
                pinned,
                &archive.join("papyrus-prefab-cache"),
                &archive.join("papyrus-texture-cache"),
                cancelled,
            ) {
                return Ok(pair);
            }
        }
    }
    Ok(found)
}
/// Prepare missing assets in an owned temporary cache, verify the complete pair,
/// then publish without rebinding or weakening any saved asset fingerprints.
pub fn prepare(
    root: &Path,
    source: &Installation,
    pinned: Option<&AssetIdentity>,
    cancelled: impl Fn() -> bool,
) -> Result<(Catalog, Bindings), String> {
    let local = crate::storage::local_data()?;
    prepare_in(root, source, pinned, &local, &cancelled)
}
#[cfg(windows)]
fn texture_search_leases(root: &Path) -> Result<(Vec<File>, Vec<PathBuf>), String> {
    let _verification = crate::installation::VerificationScope::default();
    let mut leases = Vec::new();
    let mut missing = Vec::new();
    for relative in &crate::prefab_textures::SOURCE_BUNDLES[2..] {
        let path = root.join(relative);
        if path.try_exists().map_err(|e| e.to_string())? {
            leases.push(crate::source_lease::open(&path).map_err(|e| e.to_string())?);
        } else {
            missing.push(path);
        }
    }
    Ok((leases, missing))
}


#[cfg(windows)]
fn verify_missing_texture_bundles(missing: &[PathBuf]) -> Result<(), String> {
    for path in missing {
        if path.try_exists().map_err(|e| e.to_string())? {
            return Err("Rust files changed during texture loading. Try again.".into());
        }
    }
    Ok(())
}

pub fn prepare_in(
    root: &Path,
    source: &Installation,
    pinned: Option<&AssetIdentity>,
    local: &Path,
    cancelled: impl Fn() -> bool,
) -> Result<(Catalog, Bindings), String> {
    let _verification = crate::installation::VerificationScope::default();
    source.validate()?;
    let progress = crate::loading_progress::begin("Checking saved map assets", None);
    let reporter = progress.reporter();
    let catalogs = local.join("papyrus-prefab-cache");
    let textures = local.join("papyrus-texture-cache");
    let repair = match lookup_cached_in(root, source, pinned, local, &cancelled)? {
        AssetLookup::Ready(catalog, bindings) => return Ok((catalog, bindings)),
        AssetLookup::Damaged {
            catalog,
            catalog_path,
            bindings,
            cache_key,
        } => {
            let _progress = crate::loading_progress::begin("Repairing texture cache", None);
            for archive in crate::client_cache::archived_roots(local) {
                if cancelled() {
                    return Err("Texture repair cancelled".into());
                }
                let receipt = archive
                    .join("papyrus-texture-cache")
                    .join(&cache_key)
                    .join("bindings.json");
                if let Ok(donor) = Bindings::load_manifest(root, &receipt, &catalog, &cancelled)
                    && donor.sha256 == bindings.sha256
                    && bindings.repair_from_available(&donor, &cancelled)?
                {
                    return Ok((catalog, bindings));
                }
            }
            Some((catalog, catalog_path, bindings))
        }
        AssetLookup::Missing => None,
    };
    if cancelled() {
        return Err("Asset preparation cancelled".into());
    }
    let actual = crate::installation::inspect(root, &cancelled)?;
    if actual.kind != source.kind
        || actual.native != source.native
        || source
            .scene_manifest
            .as_ref()
            .is_some_and(|v| actual.scene_manifest.as_ref() != Some(v))
    {
        return Err("Selected installation differs from the requested asset source".into());
    }
    fs::create_dir_all(local).map_err(|e| e.to_string())?;
    let temporary = tempfile::Builder::new()
        .prefix(".asset-prepare-")
        .tempdir_in(local)
        .map_err(|e| e.to_string())?;
    let new_catalogs = temporary.path().join("catalogs");
    let new_textures = temporary.path().join("textures");
    use crate::metadata_runtime::{self, Script};
    reporter.stage("Reading prefab catalog");
    let staged = if repair.is_none() {
        stage_cached_catalog(root, source, pinned, &catalogs, &new_catalogs, &cancelled)?
    } else {
        None
    };
    let (catalog, catalog_path, damaged) = match (repair, staged) {
        (Some((catalog, path, damaged)), _) => (catalog, path, Some(damaged)),
        (None, Some((catalog, path))) => (catalog, path, None),
        (None, None) => {
            let native_catalog = if pinned.is_none() {
                crate::native_catalog::prepare(root, source, &new_catalogs, &cancelled)?
            } else {
                None
            };
            let (catalog, path) = match native_catalog {
                Some(prepared) => prepared,
                None => {
                    if !metadata_runtime::run(
                        Script::Catalog,
                        &[
                            "--rust-root".into(),
                            root.as_os_str().to_owned(),
                            "--cache-root".into(),
                            new_catalogs.as_os_str().to_owned(),
                        ],
                        &cancelled,
                    )? {
                        return Err("Asset preparation cancelled".into());
                    }
                    let path = metadata_runtime::document(&new_catalogs, "catalog.json")?;
                    (prefab_assets::load(root, &path, source, &cancelled)?, path)
                }
            };
            (catalog, path, None)
        }
    };
    if pinned.is_some_and(|p| p.catalog_sha256 != catalog.sha256) {
        return Err(
            "Prepared catalog differs from the saved asset pin; the recipe was retained".into(),
        );
    }
    let plan_path = temporary.path().join("texture-sources.json");
    reporter.stage("Finding terrain textures");
    // Keep every searched index stable between discovery and decoding, including
    // bundles that discovery excludes from the selected texture sources.
    #[cfg(windows)]
    let (_texture_search_leases, missing_texture_bundles) = texture_search_leases(root)?;
    #[cfg(windows)]
    {
        // Locate additional sources from exact GUIDs before the decoding child
        // starts. Its before/after checks can then reuse our retained leases.
        for relative in [
            "Bundles/shared/content.bundle",
            "Bundles/shared/monuments.bundle",
        ] {
            crate::installation::hash_file(root, relative, 8 * 1024_u64.pow(3), &cancelled)?;
        }
        if !crate::native_texture_sources::prepare(
            root, &catalog_path, &catalog.sha256, &plan_path, &cancelled,
        )? && !metadata_runtime::run(
            Script::Textures,
            &[
                "--rust-root".into(),
                root.as_os_str().to_owned(),
                "--catalog".into(),
                catalog_path.as_os_str().to_owned(),
                "--discover-sources".into(),
                plan_path.as_os_str().to_owned(),
            ],
            &cancelled,
        )? {
            return Err("Asset preparation cancelled".into());
        }
        let mut plan = Vec::new();
        File::open(&plan_path)
            .map_err(|e| e.to_string())?
            .take(crate::prefab_textures::SOURCE_PLAN_LIMIT + 1)
            .read_to_end(&mut plan)
            .map_err(|e| e.to_string())?;
        let read_ahead = crate::generation_resources::current().effective_workers() >= 2;
        for relative in crate::prefab_textures::source_plan(&plan, &catalog.sha256)? {
            crate::installation::hash_file_with_read_ahead(
                root,
                &relative,
                8 * 1024_u64.pow(3),
                read_ahead,
                &cancelled,
            )?;
        }
    }
    let mut texture_arguments = vec![
        "--rust-root".into(),
        root.as_os_str().to_owned(),
        "--catalog".into(),
        catalog_path.as_os_str().to_owned(),
        "--reuse-cache".into(),
        textures.as_os_str().to_owned(),
        "--cache-root".into(),
        new_textures.as_os_str().to_owned(),
    ];
    if cfg!(windows) {
        texture_arguments.extend(["--source-plan".into(), plan_path.as_os_str().to_owned()]);
    }
    if !metadata_runtime::run(Script::Textures, &texture_arguments, &cancelled)? {
        return Err("Asset preparation cancelled".into());
    }
    #[cfg(windows)]
    verify_missing_texture_bundles(&missing_texture_bundles)?;
    reporter.stage("Verifying terrain texture cache");
    let receipt = metadata_runtime::document(&new_textures, "bindings.json")?;
    let mut bindings = Bindings::load_manifest(root, &receipt, &catalog, &cancelled)?;
    let relocatable = bindings.verify_staged_payloads(&cancelled)?;
    if pinned.is_some_and(|p| p.bindings_sha256 != bindings.sha256) {
        return Err(
            "Prepared textures differ from the saved asset pin; the recipe was retained".into(),
        );
    }
    if cancelled() {
        return Err("Asset preparation cancelled".into());
    }
    if let Some(damaged) = damaged {
        damaged.repair_from(&bindings, &cancelled)?;
        return Ok((catalog, damaged));
    }
    metadata_runtime::publish(&catalog_path, &catalogs, &catalog.sha256, 16 * 1024 * 1024)?;
    let published = metadata_runtime::publish_with_status(
        &receipt, &textures, &bindings.sha256, 4 * 1024 * 1024,
    )?;
    if cancelled() {
        return Err("Asset preparation cancelled".into());
    }
    if relocatable && published.moved && bindings.use_published_root(&published.path)? {
        return Ok((catalog, bindings));
    }
    // A retained directory has only had its manifest checked by publication.
    load_from(root, source, pinned, &catalogs, &textures, cancelled)
}
// Only installation changes are cache misses. Corruption and access errors remain errors.
fn stale_source(error: &str) -> bool {
    error.starts_with("Prefab source changed:") || error.starts_with("Texture source changed:")
}
pub fn load_from(
    root: &Path,
    source: &Installation,
    pinned: Option<&AssetIdentity>,
    catalogs: &Path,
    textures: &Path,
    cancelled: impl Fn() -> bool,
) -> Result<(Catalog, Bindings), String> {
    ready_assets(lookup(root, source, pinned, catalogs, textures, cancelled)?)
}

fn ready_assets(found: AssetLookup) -> Result<(Catalog, Bindings), String> {
    match found {
        AssetLookup::Missing => Err(MISSING_ASSETS.into()),
        AssetLookup::Ready(catalog, bindings) => Ok((catalog, bindings)),
        AssetLookup::Damaged { .. } => Err(DAMAGED_PAYLOAD.into()),
    }
}

fn catalog_candidate(
    path: &Path,
    source: &Installation,
    pinned: Option<&AssetIdentity>,
    extractor: Option<&str>,
) -> Option<(String, Vec<u8>)> {
    let bytes = read_document(path, 16 * 1024 * 1024).ok()?;
    let doc = serde_json::from_slice::<CatalogHeader>(&bytes).ok()?;
    if doc.format != "papyrus-prefab-catalog"
        || (pinned.is_none()
            && doc.adapter.as_deref() != Some(prefab_assets::DISCOVERY_ADAPTER)
            && doc.adapter.as_deref() != Some(prefab_assets::BUILDING_ADAPTER))
        || extractor.is_some_and(|hash| doc.extractor_sha256.as_deref() != Some(hash))
        || !doc.sources.contains(&source.native)
        || source
            .scene_manifest
            .as_ref()
            .is_some_and(|manifest| !doc.sources.contains(manifest))
    {
        return None;
    }
    let hash = format!("{:x}", Sha256::digest(&bytes));
    pinned
        .is_none_or(|pin| pin.catalog_sha256 == hash)
        .then_some((hash, bytes))
}

fn stage_cached_catalog(
    root: &Path,
    source: &Installation,
    pinned: Option<&AssetIdentity>,
    cache: &Path,
    staging: &Path,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<(Catalog, PathBuf)>, String> {
    if cancelled() {
        return Err("Asset preparation cancelled".into());
    }
    let extractor = pinned.is_none().then(crate::native_catalog::fingerprint);
    for path in documents(cache, "catalog.json")? {
        if cancelled() {
            return Err("Asset preparation cancelled".into());
        }
        let Some((hash, bytes)) = catalog_candidate(&path, source, pinned, extractor.as_deref())
        else {
            continue;
        };
        if path.parent().and_then(Path::file_name) != Some(OsStr::new(&hash)) {
            continue;
        }
        let catalog = match prefab_assets::load_document(root, &bytes, source, cancelled) {
            Ok(catalog) => catalog,
            Err(error) if stale_source(&error) && pinned.is_none() => continue,
            Err(error) => return Err(error),
        };
        // The texture importer snapshots these sources in its three-file
        // generation layout. Restriction catalogs use additional mesh sources.
        if catalog.sources.len() != 3 {
            continue;
        }
        // Give the importer a private, immutable copy. The original cache path
        // can be rotated or removed while installation preparation runs.
        let directory = staging.join(hash);
        fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        let document = directory.join("catalog.json");
        fs::write(&document, bytes).map_err(|e| e.to_string())?;
        return Ok(Some((catalog, document)));
    }
    Ok(None)
}

fn matching_catalogs(
    catalogs: &Path,
    source: &Installation,
    pinned: Option<&AssetIdentity>,
    cancelled: &impl Fn() -> bool,
) -> Result<Vec<(String, PathBuf)>, String> {
    if cancelled() {
        return Err("Asset discovery cancelled".into());
    }
    // The cache directory uses the catalog document's SHA-256 as its key.
    // Verify the contents before relying on it. Older cache layouts can
    // still contain the same pinned document under a different key.
    if let Some(pin) = pinned.filter(|pin| valid_digest(&pin.catalog_sha256)) {
        let path = catalogs.join(&pin.catalog_sha256).join("catalog.json");
        if let Some((hash, _)) = catalog_candidate(&path, source, pinned, None) {
            return Ok(vec![(hash, path)]);
        }
    }
    let mut candidates = Vec::new();
    for path in documents(catalogs, "catalog.json")? {
        if cancelled() {
            return Err("Asset discovery cancelled".into());
        }
        if let Some((hash, _)) = catalog_candidate(&path, source, pinned, None) {
            candidates.push((hash, path));
        }
    }
    Ok(candidates)
}

fn lookup(
    root: &Path,
    source: &Installation,
    pinned: Option<&AssetIdentity>,
    catalogs: &Path,
    textures: &Path,
    cancelled: impl Fn() -> bool,
) -> Result<AssetLookup, String> {
    if cancelled() {
        return Err("Asset discovery cancelled".into());
    }
    let receipts = match documents(textures, "bindings.json") {
        Ok(receipts) => receipts,
        Err(_) if matching_catalogs(catalogs, source, pinned, &cancelled)?.is_empty() => {
            return Ok(AssetLookup::Missing);
        }
        Err(error) => return Err(error),
    };
    let mut candidates = None;
    for receipt in receipts {
        if cancelled() {
            return Err("Asset discovery cancelled".into());
        }
        let Ok(raw) = read_document(&receipt, 4 * 1024 * 1024) else {
            continue;
        };
        let Ok(doc) = serde_json::from_slice::<TextureHeader>(&raw) else {
            continue;
        };
        if doc.format != "papyrus-prefab-textures"
            || !valid_digest(&doc.catalog_sha256)
            || pinned.is_some_and(|p| p.catalog_sha256 != doc.catalog_sha256)
            || pinned.is_some_and(|p| p.bindings_sha256 != format!("{:x}", Sha256::digest(&raw)))
        {
            continue;
        }
        // A texture receipt names the catalog it uses. The current cache layout
        // can open that exact document directly, even in a new unpinned project.
        let direct = catalogs.join(&doc.catalog_sha256).join("catalog.json");
        let (path, loaded) = if let Some((_, bytes)) = catalog_candidate(&direct, source, pinned, None)
            .filter(|(hash, _)| *hash == doc.catalog_sha256)
        {
            (
                direct,
                prefab_assets::load_document(root, &bytes, source, &cancelled),
            )
        } else {
            if candidates.is_none() {
                candidates = Some(matching_catalogs(catalogs, source, pinned, &cancelled)?);
            }
            let Some((_, path)) = candidates
                .as_deref()
                .unwrap_or_default()
                .iter()
                .find(|(hash, _)| doc.catalog_sha256 == *hash)
            else {
                continue;
            };
            (path.clone(), prefab_assets::load(root, path, source, &cancelled))
        };
        let result = loaded.and_then(|catalog| {
            if catalog.sha256 != doc.catalog_sha256 {
                return Err("Cached prefab catalog changed during discovery. Retry asset preparation.".into());
            }
            let bindings =
                Bindings::load_manifest_document(root, &receipt, &raw, &catalog, &cancelled)?;
            match bindings.verify_payloads(&cancelled) {
                Ok(()) => Ok(AssetLookup::Ready(catalog, bindings)),
                Err(PayloadError::Damaged) => {
                    let cache_key = owned_texture_key(textures, &receipt)
                        .ok_or(DAMAGED_PAYLOAD)?
                        .to_owned();
                    Ok(AssetLookup::Damaged {
                        catalog,
                        catalog_path: path,
                        bindings,
                        cache_key,
                    })
                }
                Err(error) => Err(error.into()),
            }
        });
        match result {
            Ok(found) => return Ok(found),
            Err(error) if stale_source(&error) && pinned.is_none() => continue,
            Err(error) if stale_source(&error) => {
                return Err(format!(
                    "Rust files changed. This project's saved assets need their matching Rust installation; select that installation or start a new project to use updated assets. Older cache snapshots were retained. {error}"
                ));
            }
            Err(error) => return Err(error),
        }
    }
    if cancelled() {
        return Err("Asset discovery cancelled".into());
    }
    Ok(AssetLookup::Missing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::installation::{InstallationKind, SourceFile};

    #[test]
    fn catalog_path_verifies_content_even_in_the_digest_named_directory() {
        let cache = tempfile::tempdir().unwrap();
        let original = br#"{"catalog":"original"}"#;
        let sha = format!("{:x}", Sha256::digest(original));
        let direct = cache.path().join(&sha);
        let fallback = cache.path().join("old-layout");
        fs::create_dir(&direct).unwrap();
        fs::create_dir(&fallback).unwrap();
        let direct = direct.join("catalog.json");
        let fallback = fallback.join("catalog.json");
        fs::write(&direct, original).unwrap();
        fs::write(&fallback, original).unwrap();
        assert_eq!(catalog_path(cache.path(), &sha).unwrap(), direct);
        fs::write(&direct, br#"{"catalog":"replaced"}"#).unwrap();
        assert_eq!(catalog_path(cache.path(), &sha).unwrap(), fallback);
    }
    #[test]
    fn repair_requires_a_direct_owned_texture_cache_entry() {
        let local = tempfile::tempdir().unwrap();
        let cache = local.path().join("papyrus-texture-cache");
        let key = "a".repeat(64);
        let directory = cache.join(&key);
        fs::create_dir_all(&directory).unwrap();
        let receipt = directory.join("bindings.json");
        fs::write(&receipt, b"unrelated receipt hash").unwrap();
        assert_eq!(owned_texture_key(&cache, &receipt), Some(OsStr::new(&key)));
        assert_eq!(owned_texture_key(local.path(), &receipt), None);
    }
    #[cfg(windows)]
    #[test]
    fn texture_search_indices_stay_read_only_through_decoding() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("Bundles/shared/textures.0.bundle");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"index").unwrap();
        let (leases, missing) = texture_search_leases(root.path()).unwrap();
        assert_eq!(leases.len(), 1);
        assert_eq!(missing.len(), 5);
        assert!(missing.contains(&root.path().join("Bundles/shared/textures.4.bundle")));
        verify_missing_texture_bundles(&missing).unwrap();
        let added = &missing[0];
        fs::create_dir_all(added.parent().unwrap()).unwrap();
        fs::write(added, b"new index").unwrap();
        assert!(verify_missing_texture_bundles(&missing).unwrap_err().contains("changed"));
        assert!(fs::write(&path, b"changed").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"index");
        drop(leases);
        fs::write(&path, b"changed").unwrap();
    }

    #[test]
    fn discovery_headers_preserve_hashes_and_validate_skipped_json() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.json");
        let raw = br#"{"format":"papyrus-prefab-catalog","sources":[],"metadata":{"nested":[{"value":"escaped \\"},null,1.25]}}"#;
        fs::write(&path, raw).unwrap();
        let (header, hash) = read::<CatalogHeader>(&path).unwrap();
        assert_eq!(header.format, "papyrus-prefab-catalog");
        assert!(header.adapter.is_none());
        assert!(header.sources.is_empty());
        assert_eq!(hash, format!("{:x}", Sha256::digest(raw)));
        assert_eq!(read::<serde::de::IgnoredAny>(&path).unwrap().1, hash);
        fs::write(
            &path,
            br#"{"format":"papyrus-prefab-catalog","sources":[],"metadata":[1,]}"#,
        )
        .unwrap();
        assert!(read::<CatalogHeader>(&path).is_err());
        assert!(read::<serde::de::IgnoredAny>(&path).is_err());
        fs::write(
            &path,
            br#"{"format":"papyrus-prefab-textures","catalog_sha256":42}"#,
        )
        .unwrap();
        assert!(read::<TextureHeader>(&path).is_err());
        fs::write(&path, vec![b' '; 16 * 1024 * 1024 + 1]).unwrap();
        assert!(
            read::<CatalogHeader>(&path)
                .err()
                .unwrap()
                .contains("exceeds")
        );
    }
    #[test]
    #[ignore = "Requires MAPPER_DISCOVERY_CACHE pointing at an owned installation cache"]
    fn installed_discovery_headers_match_full_documents() {
        use std::{hint::black_box, time::Instant};
        let cache = PathBuf::from(std::env::var_os("MAPPER_DISCOVERY_CACHE").unwrap());
        let mut rows = Vec::new();
        for (folder, name) in [
            ("papyrus-prefab-cache", "catalog.json"),
            ("papyrus-texture-cache", "bindings.json"),
        ] {
            for path in documents(&cache.join(folder), name).unwrap() {
                let (full, hash) = read::<serde_json::Value>(&path).unwrap();
                if name == "catalog.json" {
                    let (header, actual) = read::<CatalogHeader>(&path).unwrap();
                    assert_eq!(hash, actual);
                    assert_eq!(full["format"], header.format);
                    assert_eq!(full["adapter"].as_str(), header.adapter.as_deref());
                    assert_eq!(
                        full["sources"],
                        serde_json::to_value(header.sources).unwrap()
                    );
                } else {
                    let (header, actual) = read::<TextureHeader>(&path).unwrap();
                    assert_eq!(hash, actual);
                    assert_eq!(full["format"], header.format);
                    assert_eq!(full["catalog_sha256"], header.catalog_sha256);
                }
                rows.push((path, name));
            }
        }
        assert!(!rows.is_empty());
        // Alternate order to reduce ordering bias. Disk reads, JSON validation
        // and full-document hashes are included in both measurements.
        let mut full = std::time::Duration::ZERO;
        let mut headers = std::time::Duration::ZERO;
        for iteration in 0..12 {
            for pass in 0..2 {
                let projected = (iteration + pass) % 2 == 0;
                let start = Instant::now();
                for (path, name) in &rows {
                    if !projected {
                        black_box(read::<serde_json::Value>(path).unwrap());
                    } else if *name == "catalog.json" {
                        black_box(read::<CatalogHeader>(path).unwrap());
                    } else {
                        black_box(read::<TextureHeader>(path).unwrap());
                    }
                }
                if projected {
                    headers += start.elapsed();
                } else {
                    full += start.elapsed();
                }
            }
        }
        println!(
            "{}",
            serde_json::json!({"format":"mapper-discovery-header-comparison","documents":rows.len(),"iterations":12,"full_seconds":full.as_secs_f64(),"header_seconds":headers.as_secs_f64(),"summaries_and_hashes_match":true})
        );
    }
    #[test]
    fn bundle_only_updates_rebuild_unpinned_data_but_preserve_pinned_projects() {
        use sha2::{Digest, Sha256};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut sources = Vec::new();
        for path in [
            "RustClient_Data/Plugins/x86_64/RustNative.dll",
            "Bundles/AssetSceneManifest.json",
            "Bundles/shared/assetscenes.bundle",
            "Bundles/shared/content.bundle",
            "Bundles/shared/monuments.bundle",
        ] {
            fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
            fs::write(root.join(path), path).unwrap();
            sources.push(crate::installation::hash_file(root, path, 1024, &|| false).unwrap());
        }
        let source = Installation {
            kind: InstallationKind::Client,
            native: sources[0].clone(),
            scene_manifest: Some(sources[1].clone()),
        };
        let local = root.join("mapper-cache");
        let archive = local.join("client-cache-archive/snapshot");
        let catalogs = archive.join("papyrus-prefab-cache");
        let textures = archive.join("papyrus-texture-cache");
        fs::create_dir_all(catalogs.join("old")).unwrap();
        fs::create_dir_all(textures.join("old/textures")).unwrap();
        let guid = "a".repeat(32);
        let prefab = "assets/bundled/prefabs/autospawn/monument/test.prefab";
        let catalog = serde_json::json!({
            "format":"papyrus-prefab-catalog", "schema":1,
            "adapter":prefab_assets::DISCOVERY_ADAPTER, "extractor_sha256":"a".repeat(64),
            "sources":&sources[..3], "metadata":{"format":5,"modifiers":{
                (prefab):{"textureGuids":{"blend":guid}},
            },"special":{}}
        });
        let raw = serde_json::to_vec(&catalog).unwrap();
        let catalog_sha = format!("{:x}", Sha256::digest(&raw));
        fs::write(catalogs.join("old/catalog.json"), raw).unwrap();
        let relative = format!("textures/{guid}.rgba");
        let pixels = [1, 2, 3, 255];
        fs::write(textures.join("old").join(&relative), pixels).unwrap();
        let bindings = serde_json::to_vec(&serde_json::json!({
            "format":"papyrus-prefab-textures", "schema":1, "adapter":"papyrus-guid-rgba-1",
            "pixel_format":"RGBA8-top-left", "catalog_sha256":catalog_sha, "sources":sources,
            "decoded_bytes":4, "textures":{(guid.clone()):{
                "asset_path":"assets/monument/blend.png",
                "bundle_relative_path":"Bundles/shared/monuments.bundle",
                "rgba_path":relative, "width":1, "height":1, "bytes":4,
                "sha256":format!("{:x}", Sha256::digest(pixels)),
            }}
        })).unwrap();
        let pins = AssetIdentity {
            catalog_sha256: catalog_sha,
            bindings_sha256: format!("{:x}", Sha256::digest(&bindings)),
        };
        let legacy = catalogs.join("old/catalog.json");
        let keyed = catalogs.join(&pins.catalog_sha256).join("catalog.json");
        fs::create_dir(keyed.parent().unwrap()).unwrap();
        fs::copy(&legacy, &keyed).unwrap();
        fs::write(textures.join("old/bindings.json"), bindings).unwrap();
        for pin in [None, Some(&pins)] {
            let (catalog, textures) =
                load_from(root, &source, pin, &catalogs, &textures, || false).unwrap();
            assert_eq!(catalog.sha256, pins.catalog_sha256);
            assert_eq!(textures.sha256, pins.bindings_sha256);
            assert_eq!(textures.read_rgba(&guid).unwrap(), pixels);
        }
        assert_eq!(
            matching_catalogs(&catalogs, &source, Some(&pins), &|| false).unwrap(),
            vec![(pins.catalog_sha256.clone(), keyed.clone())]
        );
        let mut replaced = fs::read(&legacy).unwrap();
        replaced.push(b' ');
        fs::write(keyed, replaced).unwrap();
        assert_eq!(
            matching_catalogs(&catalogs, &source, Some(&pins), &|| false).unwrap(),
            vec![(pins.catalog_sha256.clone(), legacy)]
        );
        assert!(matching_catalogs(&catalogs, &source, Some(&pins), &|| true).is_err());
        let (retained_catalog, retained_bindings) =
            prepare_in(root, &source, Some(&pins), &local, || false).unwrap();
        assert_eq!(retained_catalog.sha256, pins.catalog_sha256);
        assert_eq!(retained_bindings.sha256, pins.bindings_sha256);
        assert_eq!(retained_bindings.read_rgba(&guid).unwrap(), pixels);
        assert!(!local.join("papyrus-prefab-cache").exists());
        fs::write(
            root.join("Bundles/shared/assetscenes.bundle"),
            b"new Rust update",
        )
        .unwrap();
        let error = load_from(root, &source, None, &catalogs, &textures, || false)
            .err()
            .unwrap();
        assert!(
            error.starts_with("No matching extracted monument assets found"),
            "{error}"
        );
        let error = load_from(root, &source, Some(&pins), &catalogs, &textures, || false)
            .err()
            .unwrap();
        assert!(
            error.contains("saved assets need their matching Rust installation"),
            "{error}"
        );
        assert!(catalogs.join("old/catalog.json").exists());
        assert!(textures.join("old/bindings.json").exists());
        assert!(!stale_source("Invalid texture source set"));
        assert!(!stale_source("Cannot read bundle: Access is denied"));
    }
    #[test]
    fn cancelled_or_changed_installation_cannot_start_extraction_or_publish_cache() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("cache");
        let game = dir.path().join("game");
        let source = Installation {
            kind: InstallationKind::Client,
            native: SourceFile {
                relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                sha256: "a".repeat(64),
                bytes: 1,
            },
            scene_manifest: None,
        };
        assert!(prepare_in(&game, &source, None, &local, || true).is_err());
        assert!(!local.exists());
        let dll = game.join(&source.native.relative_path);
        fs::create_dir_all(dll.parent().unwrap()).unwrap();
        fs::write(dll, [1]).unwrap();
        assert!(prepare_in(&game, &source, None, &local, || false).is_err());
        assert!(!local.exists());
    }
    #[test]
    fn discovery_is_bounded_cancellable_and_never_uses_raw_game_manifests() {
        let dir = tempfile::tempdir().unwrap();
        let catalogs = dir.path().join("catalogs");
        let textures = dir.path().join("textures");
        fs::create_dir_all(catalogs.join("wrong")).unwrap();
        fs::create_dir_all(textures.join("wrong")).unwrap();
        fs::write(catalogs.join("wrong/catalog.json"), br#"{"Scenes":[]}"#).unwrap();
        let source = Installation {
            kind: InstallationKind::Client,
            native: SourceFile {
                relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                sha256: "a".repeat(64),
                bytes: 1,
            },
            scene_manifest: None,
        };
        assert!(
            load_from(dir.path(), &source, None, &catalogs, &textures, || false)
                .err()
                .unwrap()
                .contains("No matching")
        );
        assert!(
            load_from(dir.path(), &source, None, &catalogs, &textures, || true)
                .err()
                .unwrap()
                .contains("cancelled")
        );
        for n in 0..257 {
            fs::create_dir_all(catalogs.join(format!("cache-{n}"))).unwrap();
        }
        assert!(
            documents(&catalogs, "catalog.json")
                .unwrap_err()
                .contains("Too many")
        );
    }
}
