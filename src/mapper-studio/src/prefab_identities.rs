//! Exact prefab identities from a verified selected-installation GameManifest.
use crate::{
    installation::{self, SourceFile},
    prefab_assets::Catalog,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
    rc::{Rc, Weak},
    sync::Arc,
};
const LIMIT: u64 = 16 * 1024 * 1024;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    format: String,
    schema: u32,
    adapter: String,
    source: SourceFile,
    extractor_sha256: String,
    prefabs: BTreeMap<String, u32>,
}
pub struct Source {
    pub sha256: String,
    pub source: SourceFile,
    pub prefabs: BTreeMap<String, u32>,
}
struct Retained {
    root: PathBuf,
    catalog: String,
    source: Arc<Source>,
}

type Operation = RefCell<Option<Retained>>;
thread_local! {
    static CURRENT: RefCell<Weak<Operation>> = const { RefCell::new(Weak::new()) };
}

/// Share decoded identities across the stages of one worker operation.
/// Nested scopes use the same slot. Dropping the last scope retires the slot.
pub struct Scope(Rc<Operation>);

impl Default for Scope {
    fn default() -> Self {
        CURRENT.with(|current| {
            if let Some(operation) = current.borrow().upgrade() {
                return Self(operation);
            }
            let operation = Rc::new(RefCell::new(None));
            *current.borrow_mut() = Rc::downgrade(&operation);
            Self(operation)
        })
    }
}

impl Scope {
    pub fn with_prepared(
        root: &Path,
        catalog: &Catalog,
        prepared: Option<&crate::client_preparation::ReadyIdentities>,
    ) -> Self {
        let scope = Self::default();
        if let Some(prepared) = prepared {
            scope.remember(root, catalog, prepared.source.clone());
        }
        scope
    }

    fn remember(&self, root: &Path, catalog: &Catalog, source: Arc<Source>) {
        *self.0.borrow_mut() = Some(Retained {
            root: root.to_owned(),
            catalog: catalog.sha256.clone(),
            source,
        });
    }
}

fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
impl Source {
    pub fn prepare(
        root: &Path,
        catalog: &Catalog,
        cancel: impl Fn() -> bool,
    ) -> Result<Option<Arc<Self>>, String> {
        Self::shared(root, catalog, &cancel, || {
            Self::prepare_in(root, catalog, &crate::storage::local_data()?, &cancel)
        })
    }

    fn shared(
        root: &Path,
        catalog: &Catalog,
        cancel: &impl Fn() -> bool,
        load: impl FnOnce() -> Result<Option<Self>, String>,
    ) -> Result<Option<Arc<Self>>, String> {
        if cancel() {
            return Ok(None);
        }
        let operation = CURRENT.with(|current| current.borrow().upgrade()).map(Scope);
        let retained = operation.as_ref().and_then(|scope| {
            scope.0.borrow().as_ref().filter(|entry| {
                entry.root == root && entry.catalog == catalog.sha256
            }).map(|entry| entry.source.clone())
        });
        if let Some(source) = retained {
            source.verify_installation(root, catalog, cancel)?;
            return Ok((!cancel()).then_some(source));
        }
        let source = load()?.filter(|_| !cancel()).map(Arc::new);
        if let (Some(scope), Some(source)) = (operation, &source) {
            scope.remember(root, catalog, source.clone());
        }
        Ok(source)
    }
    fn prepare_in(
        root: &Path,
        catalog: &Catalog,
        local: &Path,
        cancel: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        if let Some(source) = Self::cached_in(root, catalog, local, &cancel)? {
            return Ok(Some(source));
        }
        if cancel() {
            return Ok(None);
        }
        let cache = local.join("papyrus-prefab-identity-cache");
        std::fs::create_dir_all(local).map_err(|e| e.to_string())?;
        let private = tempfile::Builder::new()
            .prefix(".prefab-identities-")
            .tempdir_in(local)
            .map_err(|e| e.to_string())?;
        #[cfg(windows)]
        let _verification = installation::VerificationScope::default();
        #[cfg(windows)]
        {
            let receipt = installation::hash_file(
                root,
                "Bundles/shared/content.bundle",
                8 * 1024 * 1024 * 1024,
                &cancel,
            )?;
            let payload = private.path().join("prefabs.json");
            if crate::native_texture_sources::read_identities(
                crate::native_texture_sources::IdentityRead::PrefabIds,
                root,
                &payload,
                &cancel,
            )? {
                let mut raw = Vec::new();
                std::fs::File::open(&payload)
                    .map_err(|e| e.to_string())?
                    .take(LIMIT + 1)
                    .read_to_end(&mut raw)
                    .map_err(|e| e.to_string())?;
                if raw.len() as u64 > LIMIT {
                    return Err("Native prefab identities exceed 16 MiB".into());
                }
                let prefabs: BTreeMap<String, u32> = serde_json::from_slice(&raw)
                    .map_err(|e| e.to_string())?;
                let raw = serde_json::to_vec(&serde_json::json!({
                    "format": "papyrus-prefab-identities", "schema": 1,
                    "adapter": "papyrus-game-manifest-pool-1", "source": receipt,
                    "extractor_sha256": crate::native_texture_sources::fingerprint(), "prefabs": prefabs,
                })).map_err(|e| e.to_string())?;
                let source = Self::decode(&raw)?;
                source.verify_installation(root, catalog, &cancel)?;
                if cancel() {
                    return Ok(None);
                }
                let document = private.path().join("identities.json");
                std::fs::write(&document, raw).map_err(|e| e.to_string())?;
                crate::metadata_runtime::publish_unkeyed(&document, &cache, &source.sha256, LIMIT)?;
                return Ok(Some(source));
            }
            if cancel() {
                return Ok(None);
            }
        }
        let output = private.path().join("output");
        if !crate::metadata_runtime::run(
            crate::metadata_runtime::Script::PrefabIdentities,
            &[
                "--rust-root".into(),
                root.as_os_str().to_owned(),
                "--cache-root".into(),
                output.as_os_str().to_owned(),
            ],
            &cancel,
        )? {
            return Ok(None);
        }
        let document = crate::metadata_runtime::document(&output, "identities.json")?;
        let source = Self::load(root, &document, catalog, &cancel)?;
        if cancel() {
            return Ok(None);
        }
        crate::metadata_runtime::publish(&document, &cache, &source.sha256, LIMIT)?;
        Ok(Some(source))
    }
    /// Read startup identities without extracting assets during generation.
    pub fn available(root: &Path, catalog: &Catalog, cancel: impl Fn() -> bool) -> Result<Arc<Self>, String> {
        Self::shared(root, catalog, &cancel, || {
            Self::cached_in(root, catalog, &crate::storage::local_data()?, &cancel)
        })?
            .ok_or_else(|| "Prepared prefab identities are unavailable. Reload the selected Rust installation.".into())
    }
    fn cached_in(
        root: &Path,
        catalog: &Catalog,
        local: &Path,
        cancel: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        let cache = local.join("papyrus-prefab-identity-cache");
        if cancel() {
            return Ok(None);
        }
        if cache.is_dir() {
            for (index, entry) in std::fs::read_dir(&cache)
                .map_err(|e| e.to_string())?
                .enumerate()
            {
                if index >= 256 {
                    return Err("Too many prefab identity snapshots".into());
                }
                if cancel() {
                    return Ok(None);
                }
                let entry = entry.map_err(|e| e.to_string())?;
                let kind = entry.file_type().map_err(|e| e.to_string())?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if !kind.is_dir() || kind.is_symlink() || !hash(&name) {
                    continue;
                }
                let path = entry.path().join("identities.json");
                if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file())
                    && let Ok(source) = Self::load(root, &path, catalog, &cancel)
                    && source.sha256 == name
                {
                    return Ok(Some(source));
                }
            }
        }
        if cancel() {
            return Ok(None);
        }
        Ok(None)
    }
    pub fn load(
        root: &Path,
        path: &Path,
        catalog: &Catalog,
        cancel: impl Fn() -> bool,
    ) -> Result<Self, String> {
        let mut raw = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(LIMIT + 1)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
        let source = Self::decode(&raw)?;
        source.verify_installation(root, catalog, &cancel)?;
        Ok(source)
    }

    /// Verify a retained identity snapshot against the selected files.
    pub(crate) fn verify_installation(
        &self, root: &Path, catalog: &Catalog, cancel: impl Fn() -> bool,
    ) -> Result<(), String> {
        if cancel() {
            return Err("Prefab identity verification cancelled".into());
        }
        let native = &catalog.installation.native;
        if installation::hash_file(root, &native.relative_path, 64 * 1024 * 1024, &cancel)?
            != *native
        {
            return Err("Prefab identity installation differs from the catalog".into());
        }
        if let Some(scene) = &catalog.installation.scene_manifest
            && installation::hash_file(root, &scene.relative_path, 32 * 1024 * 1024, &cancel)?
                != *scene
        {
            return Err("Prefab identity scene manifest differs from the catalog".into());
        }
        if installation::hash_file(
            root,
            &self.source.relative_path,
            8 * 1024 * 1024 * 1024,
            &cancel,
        )? != self.source
        {
            return Err("Prefab identities belong to another GameManifest".into());
        }
        Ok(())
    }
    fn decode(raw: &[u8]) -> Result<Self, String> {
        if raw.len() as u64 > LIMIT {
            return Err("Prefab identity snapshot exceeds 16 MiB".into());
        }
        let d: Document =
            serde_json::from_slice(raw).map_err(|e| format!("Invalid prefab identities: {e}"))?;
        if d.format != "papyrus-prefab-identities"
            || d.schema != 1
            || d.adapter != "papyrus-game-manifest-pool-1"
            || !hash(&d.extractor_sha256)
            || d.source.relative_path != "Bundles/shared/content.bundle"
            || !hash(&d.source.sha256)
            || d.source.bytes == 0
            || d.source.bytes > 8 * 1024 * 1024 * 1024
            || d.prefabs.is_empty()
            || d.prefabs.len() > 131072
        {
            return Err("Invalid prefab identity contract".into());
        }
        let mut ids = BTreeSet::new();
        for (path, id) in &d.prefabs {
            if *id == 0
                || !ids.insert(*id)
                || !path.is_ascii()
                || path.len() > 4096
                || !path.starts_with("assets/")
                || !path.ends_with(".prefab")
                || path.bytes().any(|b| b.is_ascii_uppercase())
                || path.chars().any(char::is_control)
                || path.contains('\\')
                || path
                    .split('/')
                    .any(|p| p.is_empty() || p == "." || p == "..")
            {
                return Err("Invalid or ambiguous prefab identity".into());
            }
        }
        Ok(Self {
            sha256: format!("{:x}", Sha256::digest(raw)),
            source: d.source,
            prefabs: d.prefabs,
        })
    }
    pub fn resolve(&self, path: &str) -> Option<u32> {
        self.prefabs.get(path).copied().or_else(|| {
            path.bytes().any(|b| b.is_ascii_uppercase())
                .then(|| self.prefabs.get(&path.to_ascii_lowercase()).copied())
                .flatten()
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn changed_installation_or_manifest_receipts_and_cancellation_reject_the_snapshot() {
        use crate::installation::{Installation, InstallationKind};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let native_path = "RustClient_Data/Plugins/x86_64/RustNative.dll";
        std::fs::create_dir_all(root.join("RustClient_Data/Plugins/x86_64")).unwrap();
        std::fs::create_dir_all(root.join("Bundles/shared")).unwrap();
        std::fs::write(root.join(native_path), b"native").unwrap();
        std::fs::write(root.join("Bundles/shared/content.bundle"), b"manifest").unwrap();
        let native = installation::hash_file(root, native_path, 100, &|| false).unwrap();
        let manifest =
            installation::hash_file(root, "Bundles/shared/content.bundle", 100, &|| false).unwrap();
        let catalog = Catalog {
            sha256: "c".repeat(64),
            installation: Installation {
                kind: InstallationKind::Client,
                native,
                scene_manifest: None,
            },
            sources: vec![],
            entries: Default::default(),
        };
        let doc = serde_json::json!({"format":"papyrus-prefab-identities","schema":1,"adapter":"papyrus-game-manifest-pool-1","source":manifest,"extractor_sha256":"b".repeat(64),"prefabs":{"assets/a.prefab":7}});
        let path = root.join("identities.json");
        std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
        let retained = Source::load(root, &path, &catalog, || false).unwrap();
        assert_eq!(retained.resolve("assets/a.prefab"), Some(7));
        assert!(Source::load(root, &path, &catalog, || true).is_err());
        let local=root.join("cache");
        let raw=std::fs::read(&path).unwrap();
        let digest=format!("{:x}",Sha256::digest(&raw));
        let folder=local.join("papyrus-prefab-identity-cache").join(digest);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("identities.json"),raw).unwrap();
        assert_eq!(Source::prepare_in(root,&catalog,&local,||false).unwrap().unwrap().resolve("assets/a.prefab"),Some(7));
        assert!(Source::prepare_in(root,&catalog,&local,||true).unwrap().is_none());
        let scoped = Scope::default();
        let first = Source::shared(root, &catalog, &|| false, || {
            Source::prepare_in(root, &catalog, &local, || false)
        }).unwrap().unwrap();
        let nested = Scope::default();
        drop(scoped);
        let next = Source::shared(root, &catalog, &|| false, || panic!("snapshot reread"))
            .unwrap().unwrap();
        assert!(Arc::ptr_eq(&first, &next));
        assert!(Source::shared(&root.join("other"), &catalog, &|| false, || Ok(None)).unwrap().is_none());
        assert!(Source::shared(root, &catalog, &|| true, || panic!("cancelled lookup loaded a snapshot")).unwrap().is_none());
        std::fs::write(
            root.join("Bundles/shared/content.bundle"),
            b"updated manifest",
        )
        .unwrap();
        assert!(Source::load(root, &path, &catalog, || false).is_err());
        assert!(retained.verify_installation(root, &catalog, || false).is_err());
        assert!(Source::shared(root, &catalog, &|| false, || panic!("stale in-flight snapshot reread")).is_err());
        drop(nested);
        assert!(Source::shared(root, &catalog, &|| false, || Ok(None)).unwrap().is_none());
        std::fs::write(root.join("Bundles/shared/content.bundle"), b"manifest").unwrap();
        std::fs::write(root.join(native_path), b"another installation").unwrap();
        assert!(Source::load(root, &path, &catalog, || false).is_err());
    }
    #[test]
    fn unknown_ids_are_not_guessed_and_malformed_tables_fail() {
        let doc = serde_json::json!({"format":"papyrus-prefab-identities","schema":1,"adapter":"papyrus-game-manifest-pool-1","source":{"relative_path":"Bundles/shared/content.bundle","sha256":"a".repeat(64),"bytes":1},"extractor_sha256":"b".repeat(64),"prefabs":{"assets/a.prefab":7,"assets/b.prefab":8}});
        let read = |v: &serde_json::Value| Source::decode(&serde_json::to_vec(v).unwrap());
        let s = read(&doc).unwrap();
        assert_eq!(s.resolve("ASSETS/A.PREFAB"), Some(7));
        assert_eq!(s.resolve("assets/unknown.prefab"), None);
        for (pointer, value) in [
            ("/prefabs/assets~1b.prefab", serde_json::json!(7)),
            ("/schema", serde_json::json!(2)),
            (
                "/source/relative_path",
                serde_json::json!("../content.bundle"),
            ),
        ] {
            let mut bad = doc.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(read(&bad).is_err());
        }
    }
}
