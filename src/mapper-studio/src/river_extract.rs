//! Prepare river metadata from the selected installation, with exact saved pins.
use crate::{
    installation::Installation,
    metadata_runtime::{self, Script},
    river_generation::{self, Recipe, Source},
};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub struct Prepared {
    pub source: Source,
    pub reused: bool,
}
fn read(path: &Path) -> Result<Vec<u8>, String> {
    let mut raw = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take((river_generation::LIMIT + 1) as u64)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() > river_generation::LIMIT {
        return Err("River source exceeds 20 MiB".into());
    }
    Ok(raw)
}
fn documents(root: &Path, name: &str) -> Result<Vec<PathBuf>, String> {
    if !root.exists() {
        return Ok(vec![]);
    }
    let mut found = Vec::new();
    for (i, entry) in std::fs::read_dir(root)
        .map_err(|e| e.to_string())?
        .enumerate()
    {
        if i >= 128 {
            return Err("Too many river source cache entries".into());
        }
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_type().map_err(|e| e.to_string())?.is_dir()
            && entry.path().join(name).is_file()
        {
            found.push(entry.path().join(name));
        }
    }
    found.sort();
    Ok(found)
}
pub fn prepare(
    root: &Path,
    installation: &Installation,
    pinned: Option<&Recipe>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Prepared>, String> {
    prepare_in(
        root,
        installation,
        pinned,
        &crate::storage::local_data()?,
        cancelled,
    )
}
pub fn prepare_in(
    root: &Path,
    installation: &Installation,
    pinned: Option<&Recipe>,
    local: &Path,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Prepared>, String> {
    if cancelled() {
        return Ok(None);
    }
    let cache = local.join("papyrus-river-cache");
    if let Some(recipe) = pinned {
        recipe.validate()?;
        let path = crate::client_cache::pinned_document(
            local,
            "papyrus-river-cache",
            &recipe.source_sha256,
            "rivers.json",
        )?;
        let Some(source) =
            Source::load_recipe(root, Some(&path), installation, recipe, &cancelled)?
        else {
            return Ok(None);
        };
        if source.sha256 != recipe.source_sha256 {
            return Err("Saved river source digest changed".into());
        }
        return Ok(Some(Prepared {
            source,
            reused: true,
        }));
    }
    for path in documents(&cache, "rivers.json")? {
        if cancelled() {
            return Ok(None);
        }
        if let Ok(Some(source)) = Source::load(root, &path, installation, &cancelled)
            && source.mesh.current_extractor()
            && path
                .parent()
                .and_then(Path::file_name)
                .and_then(|v| v.to_str())
                == Some(source.sha256.as_str())
        {
            return Ok(Some(Prepared {
                source,
                reused: true,
            }));
        }
    }
    let profile = river_generation::profile_for(installation)?;
    std::fs::create_dir_all(local).map_err(|e| e.to_string())?;
    let private = tempfile::Builder::new()
        .prefix(".river-prepare-")
        .tempdir_in(local)
        .map_err(|e| e.to_string())?;
    let profile_path = private.path().join("profile.json");
    std::fs::write(
        &profile_path,
        serde_json::to_vec(&profile["mesh_profile"]).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let captured = private.path().join("meshes");
    if !metadata_runtime::run(
        Script::RiverMeshes,
        &[
            "--rust-root".into(),
            root.as_os_str().to_owned(),
            "--profile".into(),
            profile_path.as_os_str().to_owned(),
            "--cache-root".into(),
            captured.as_os_str().to_owned(),
        ],
        &cancelled,
    )? {
        return Ok(None);
    }
    let files = documents(&captured, "meshes.json")?;
    if files.len() != 1 {
        return Err("River preparation did not produce one complete snapshot".into());
    }
    let mesh: serde_json::Value =
        serde_json::from_slice(&read(&files[0])?).map_err(|e| e.to_string())?;
    let raw=serde_json::to_vec(&serde_json::json!({"format":"papyrus-river-generation-source","schema":1,"profile":profile,"mesh_source":mesh})).map_err(|e|e.to_string())?;
    let source = Source::decode(&raw, installation)?;
    let verified = source.mesh.verify(root, &cancelled);
    if cancelled() {
        return Ok(None);
    }
    if !verified? {
        return Ok(None);
    }
    let publish = private.path().join("generation").join(&source.sha256);
    std::fs::create_dir_all(&publish).map_err(|e| e.to_string())?;
    let file = publish.join("rivers.json");
    let mut writer = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file)
        .map_err(|e| e.to_string())?;
    writer
        .write_all(&raw)
        .and_then(|_| writer.sync_all())
        .map_err(|e| e.to_string())?;
    drop(writer);
    if cancelled() {
        return Ok(None);
    }
    metadata_runtime::publish(
        &file,
        &cache,
        &source.sha256,
        river_generation::LIMIT as u64,
    )?;
    Ok(Some(Prepared {
        source,
        reused: false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires an owned client and packaged metadata runtime"]
    fn owned_river_preparation_reuses_warm_and_saved_sources() {
        let path = |name| PathBuf::from(std::env::var_os(name).expect(name));
        let root = path("MAPPER_RIVER_PREP_TEST_ROOT");
        let _verification = crate::installation::VerificationScope::default();
        let installation = crate::installation::inspect(&root, || false).unwrap();
        let local = tempfile::tempdir().unwrap();
        assert!(prepare_in(&root, &installation, None, local.path(), || true)
            .unwrap().is_none());
        let first = prepare_in(&root, &installation, None, local.path(), || false)
            .unwrap().unwrap();
        assert!(!first.reused);
        let recipe = first.source.recipe();
        recipe.validate().unwrap();
        let file = local.path().join("papyrus-river-cache")
            .join(&recipe.source_sha256).join("rivers.json");
        let original = std::fs::read(&file).unwrap();
        for pin in [None, Some(&recipe)] {
            let cached = prepare_in(&root, &installation, pin, local.path(), || false)
                .unwrap().unwrap();
            assert!(cached.reused);
            assert_eq!(cached.source.sha256, first.source.sha256);
            assert_eq!(std::fs::read(&file).unwrap(), original);
        }
        let mut missing = recipe.clone();
        missing.source_sha256 = "f".repeat(64);
        assert!(prepare_in(&root, &installation, Some(&missing), local.path(), || false).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), original);
        std::fs::write(
            path("MAPPER_RIVER_PREP_TEST_REPORT"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "source_sha256": first.source.sha256,
                "warm_reused": true,
                "saved_pin_reused": true,
                "missing_pin_rejected": true,
                "generation_executed": false,
                "game_runtime_validated": false
            })).unwrap(),
        ).unwrap();
    }

    #[test]
    fn cancelled_or_missing_saved_identity_cannot_start_recapture() {
        let directory = tempfile::tempdir().unwrap();
        let (installation, _) = crate::river_mesh_source::tests::fixture(directory.path());
        let local = directory.path().join("new-cache");
        let recipe = Recipe {
            engine: papyrus_terrain::river_pipeline::VERSION.into(),
            source_sha256: "a".repeat(64),
            source_document: None,
            objects: None,
        };
        assert!(
            prepare_in(
                directory.path(),
                &installation,
                Some(&recipe),
                &local,
                || true
            )
            .unwrap()
            .is_none()
        );
        assert!(!local.exists());
        assert!(
            prepare_in(
                directory.path(),
                &installation,
                Some(&recipe),
                &local,
                || false
            )
            .is_err()
        );
        assert!(!local.exists());
        assert!(prepare_in(directory.path(), &installation, None, &local, || false).is_err());
        assert!(!local.exists());
    }
}
