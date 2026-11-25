//! Prepare installation-bound rail stages and template LODs with packaged parsers.
use crate::{
    infrastructure,
    metadata_runtime::{self, Script},
    prefab_assets::Catalog,
    rail_generation::Recipe,
    rail_mesh_source, rail_source,
};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

const LEGACY_PROFILE: &str =
    include_str!("../../../compatibility/rails/client-0f703d43-reservations.json");
const PROFILES: [&str; 3] = [
    LEGACY_PROFILE,
    include_str!("../../../compatibility/rails/client-8dd4e235.json"),
    include_str!("../../../compatibility/rails/client-58f3263f.json"),
];

fn selected_profile(catalog: &Catalog) -> Result<Option<&'static str>, String> {
    for raw in PROFILES {
        let profile: serde_json::Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
        if profile["native_sha256"].as_str() == Some(&catalog.installation.native.sha256)
            && catalog.sources.iter().any(|source| {
                source.relative_path == "Bundles/shared/assetscenes.bundle"
                    && profile["scene_sha256"] == source.sha256
            })
        {
            return Ok(Some(raw));
        }
    }
    Ok(None)
}

pub struct Prepared {
    pub source: rail_source::Source,
    pub meshes: rail_mesh_source::Source,
    pub reused: bool,
}

/// Reuse exact saved pins, or discover/prepare complete supported rail metadata.
pub fn prepare(
    root: &Path,
    catalog: &Catalog,
    catalog_path: &Path,
    infrastructure: &infrastructure::Source,
    pinned: Option<&Recipe>,
    cancelled: impl Fn() -> bool,
) -> Result<Prepared, String> {
    check_cancelled(&cancelled)?;
    let local = crate::storage::local_data()?;
    prepare_from_cache(
        root,
        catalog,
        infrastructure,
        pinned,
        &local,
        || Ok(catalog_path.to_owned()),
        cancelled,
    )
}

pub(crate) fn prepare_cached(
    root: &Path,
    catalog: &Catalog,
    infrastructure: &infrastructure::Source,
    pinned: Option<&Recipe>,
    cancelled: impl Fn() -> bool,
) -> Result<Prepared, String> {
    let local = crate::storage::local_data()?;
    prepare_from_cache(
        root,
        catalog,
        infrastructure,
        pinned,
        &local,
        || crate::asset_discovery::catalog_path(&local.join("papyrus-prefab-cache"), &catalog.sha256),
        cancelled,
    )
}

fn prepare_from_cache(
    root: &Path,
    catalog: &Catalog,
    infrastructure: &infrastructure::Source,
    pinned: Option<&Recipe>,
    local: &Path,
    catalog_path: impl FnOnce() -> Result<PathBuf, String>,
    cancelled: impl Fn() -> bool,
) -> Result<Prepared, String> {
    prepare_with_parents(
        root,
        catalog,
        infrastructure,
        pinned,
        local,
        || {
            let parent = crate::client_cache::pinned_document(
                local,
                "papyrus-infrastructure-cache",
                &infrastructure.sha256,
                "infrastructure.json",
            )?;
            Ok((catalog_path()?, parent))
        },
        cancelled,
    )
}

#[allow(clippy::too_many_arguments)] // Explicit verified parents and independent cache root.
pub fn prepare_in(
    root: &Path,
    catalog: &Catalog,
    catalog_path: &Path,
    infrastructure: &infrastructure::Source,
    infrastructure_path: &Path,
    pinned: Option<&Recipe>,
    local: &Path,
    cancelled: impl Fn() -> bool,
) -> Result<Prepared, String> {
    prepare_with_parents(
        root,
        catalog,
        infrastructure,
        pinned,
        local,
        || Ok((catalog_path.to_owned(), infrastructure_path.to_owned())),
        cancelled,
    )
}

fn prepare_with_parents(
    root: &Path,
    catalog: &Catalog,
    infrastructure: &infrastructure::Source,
    pinned: Option<&Recipe>,
    local: &Path,
    parents: impl FnOnce() -> Result<(PathBuf, PathBuf), String>,
    cancelled: impl Fn() -> bool,
) -> Result<Prepared, String> {
    check_cancelled(&cancelled)?;
    if infrastructure.catalog_sha256 != catalog.sha256 {
        return Err("Rail preparation catalog differs from road infrastructure".into());
    }
    let source_cache = local.join("papyrus-rail-cache");
    let mesh_cache = local.join("papyrus-rail-mesh-cache");
    if let Some(recipe) = pinned {
        recipe.validate()?;
        // A missing saved identity is never replaced by an equivalent new capture.
        infrastructure.verify_installation(root, &cancelled)?;
        let source = rail_source::Source::load(
            root,
            &crate::client_cache::pinned_document(
                local,
                "papyrus-rail-cache",
                &recipe.source_sha256,
                "rails.json",
            )?,
            catalog,
            infrastructure,
            &cancelled,
        )?;
        if source.sha256 != recipe.source_sha256 {
            return Err("Saved rail source digest changed".into());
        }
        let meshes = rail_mesh_source::Source::load(
            root,
            &crate::client_cache::pinned_document(
                local,
                "papyrus-rail-mesh-cache",
                &recipe.mesh_sha256,
                "meshes.json",
            )?,
            &source,
            &cancelled,
        )?;
        if meshes.sha256 != recipe.mesh_sha256 {
            return Err("Saved rail mesh digest changed".into());
        }
        return Ok(Prepared {
            source,
            meshes,
            reused: true,
        });
    }
    let profile = selected_profile(catalog)?
        .ok_or("No matching rail preparation adapter for this installation")?;
    infrastructure.verify_installation(root, &cancelled)?;

    let expected_profile: crate::rail_source::Profile =
        serde_json::from_str(profile).map_err(|e| e.to_string())?;
    crate::rail_generation::check_profile(&expected_profile)?;
    let expected_profile = serde_json::to_value(expected_profile).map_err(|e| e.to_string())?;
    let mut found = None;
    for path in documents(&source_cache, "rails.json")? {
        check_cancelled(&cancelled)?;
        if let Ok(source) =
            rail_source::Source::load(root, &path, catalog, infrastructure, &cancelled)
            && path
                .parent()
                .and_then(Path::file_name)
                .and_then(|s| s.to_str())
                == Some(source.sha256.as_str())
            && serde_json::to_value(&source.profile).map_err(|e| e.to_string())? == expected_profile
        {
            found = Some((source, path));
            break;
        }
    }
    if let Some((source, _)) = &found {
        for path in documents(&mesh_cache, "meshes.json")? {
            check_cancelled(&cancelled)?;
            if let Ok(meshes) = rail_mesh_source::Source::load(root, &path, source, &cancelled)
                && path
                    .parent()
                    .and_then(Path::file_name)
                    .and_then(|s| s.to_str())
                    == Some(meshes.sha256.as_str())
            {
                return Ok(Prepared {
                    source: found.take().unwrap().0,
                    meshes,
                    reused: true,
                });
            }
        }
    }
    check_cancelled(&cancelled)?;
    let (catalog_path, infrastructure_path) = parents()?;
    verify_document(&catalog_path, &catalog.sha256, 16 * 1024 * 1024)?;
    verify_document(&infrastructure_path, &infrastructure.sha256, 16 * 1024 * 1024)?;
    std::fs::create_dir_all(local).map_err(|e| e.to_string())?;
    let temporary = tempfile::Builder::new()
        .prefix(".rail-prepare-")
        .tempdir_in(local)
        .map_err(|e| e.to_string())?;
    let captured = found.is_none();
    let (source, source_path) = if let Some(found) = found {
        found
    } else {
        let profile_path = temporary.path().join("profile.json");
        std::fs::write(&profile_path, profile).map_err(|e| e.to_string())?;
        let output = temporary.path().join("rails");
        if !metadata_runtime::run(
            Script::Rails,
            &[
                "--rust-root".into(),
                root.as_os_str().to_owned(),
                "--catalog".into(),
                catalog_path.as_os_str().to_owned(),
                "--infrastructure".into(),
                infrastructure_path.as_os_str().to_owned(),
                "--profile".into(),
                profile_path.into_os_string(),
                "--cache-root".into(),
                output.as_os_str().to_owned(),
            ],
            &cancelled,
        )? {
            return Err("Rail preparation cancelled".into());
        }
        let document = metadata_runtime::document(&output, "rails.json")?;
        let source =
            rail_source::Source::load(root, &document, catalog, infrastructure, &cancelled)?;
        (source, document)
    };
    let output = temporary.path().join("meshes");
    if !metadata_runtime::run(
        Script::RailMeshes,
        &[
            "--rust-root".into(),
            root.as_os_str().to_owned(),
            "--rail-source".into(),
            source_path.as_os_str().to_owned(),
            "--cache-root".into(),
            output.as_os_str().to_owned(),
        ],
        &cancelled,
    )? {
        return Err("Rail mesh preparation cancelled".into());
    }
    crate::rail_generation::check_source(&source, 6000)?;
    let document = metadata_runtime::document(&output, "meshes.json")?;
    let meshes = rail_mesh_source::Source::load(root, &document, &source, &cancelled)?;
    // Recheck both parent documents before publishing a newly captured pair.
    verify_document(&catalog_path, &catalog.sha256, 16 * 1024 * 1024)?;
    verify_document(
        &infrastructure_path,
        &infrastructure.sha256,
        16 * 1024 * 1024,
    )?;
    check_cancelled(&cancelled)?;
    if captured {
        metadata_runtime::publish(&source_path, &source_cache, &source.sha256, 4 * 1024 * 1024)?;
    }
    metadata_runtime::publish(&document, &mesh_cache, &meshes.sha256, 32 * 1024 * 1024)?;
    Ok(Prepared {
        source,
        meshes,
        reused: false,
    })
}
fn check_cancelled(cancelled: &impl Fn() -> bool) -> Result<(), String> {
    if cancelled() {
        Err("Rail preparation cancelled".into())
    } else {
        Ok(())
    }
}
fn verify_document(path: &Path, digest: &str, limit: u64) -> Result<(), String> {
    let mut raw = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() as u64 > limit || format!("{:x}", Sha256::digest(&raw)) != digest {
        return Err("Rail preparation parent document changed".into());
    }
    Ok(())
}
fn documents(root: &Path, name: &str) -> Result<Vec<PathBuf>, String> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    for (i, entry) in std::fs::read_dir(root)
        .map_err(|e| e.to_string())?
        .take(257)
        .enumerate()
    {
        if i == 256 {
            return Err("Too many cached rail snapshots".into());
        }
        let entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        let id = entry.file_name();
        let id = id.to_string_lossy();
        if kind.is_dir()
            && !kind.is_symlink()
            && id.len() == 64
            && id
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            let file = entry.path().join(name);
            if std::fs::symlink_metadata(&file).is_ok_and(|m| m.file_type().is_file()) {
                found.push(file);
            }
        }
    }
    found.sort();
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bundled_profiles_select_the_exact_native_and_scene_pair() {
        let (mut catalog, _) = crate::infrastructure::tests::fixture();
        for raw in PROFILES {
            let profile: serde_json::Value = serde_json::from_str(raw).unwrap();
            let typed: rail_source::Profile = serde_json::from_str(raw).unwrap();
            crate::rail_generation::check_profile(&typed).unwrap();
            catalog.installation.native.sha256 = profile["native_sha256"].as_str().unwrap().into();
            catalog.sources[2].sha256 = profile["scene_sha256"].as_str().unwrap().into();
            assert_eq!(selected_profile(&catalog).unwrap(), Some(raw));
            catalog.sources[2].sha256 = "f".repeat(64);
            assert!(selected_profile(&catalog).unwrap().is_none());
        }
        catalog.installation.native.sha256 = "f".repeat(64);
        assert!(selected_profile(&catalog).unwrap().is_none());
    }
    #[test]
    #[ignore = "requires an owned client and packaged metadata runtime"]
    fn owned_current_rail_preparation_publishes_and_reopens_exact_saved_sources() {
        let path = |name| PathBuf::from(std::env::var_os(name).expect(name));
        let root = path("MAPPER_RAIL_PREP_TEST_ROOT");
        let _verification = crate::installation::VerificationScope::default();
        let installation = crate::installation::inspect(&root, || false).unwrap();
        let catalog_path = path("MAPPER_RAIL_PREP_TEST_CATALOG");
        let catalog =
            crate::prefab_assets::load(&root, &catalog_path, &installation, || false).unwrap();
        let parent_path = path("MAPPER_RAIL_PREP_TEST_INFRASTRUCTURE");
        let parent = infrastructure::Source::load(&parent_path, &catalog, || false).unwrap();
        let local = crate::storage::local_data().unwrap();
        let prepared = prepare_in(
            &root,
            &catalog,
            &catalog_path,
            &parent,
            &parent_path,
            None,
            &local,
            || false,
        )
        .unwrap();
        assert_eq!(prepared.source.stages.len(), 9);
        assert!(
            prepared
                .source
                .stages
                .iter()
                .all(|stage| stage.disabled.is_none())
        );
        assert_eq!(prepared.meshes.meshes.len(), 3);
        let recipe = Recipe {
            engine: crate::rail_generation::BEFORE_ROADS_VERSION.into(),
            source_sha256: prepared.source.sha256.clone(),
            mesh_sha256: prepared.meshes.sha256.clone(),
            settings: crate::rail_generation::initial_settings(7),
        };
        let reopened = prepare_in(
            &root,
            &catalog,
            &catalog_path,
            &parent,
            &parent_path,
            Some(&recipe),
            &local,
            || false,
        )
        .unwrap();
        assert!(reopened.reused);
        assert_eq!(reopened.source.sha256, prepared.source.sha256);
        assert_eq!(reopened.meshes.sha256, prepared.meshes.sha256);
        std::fs::write(
            path("MAPPER_RAIL_PREP_TEST_REPORT"),
            serde_json::to_vec_pretty(
                &serde_json::json!({"format":"mapper-owned-current-rail-preparation","schema":1,
                "source_sha256":prepared.source.sha256,"mesh_sha256":prepared.meshes.sha256,
                "stages":prepared.source.stages.len(),"lods":prepared.meshes.meshes.len(),
                "pinned_reopen_reused":reopened.reused,"generation_executed":false,
                "game_runtime_validated":false}),
            )
            .unwrap(),
        )
        .unwrap();
    }
    #[test]
    fn bundled_adapter_covers_saved_composition_and_rejects_mesh_only_profile() {
        let profile: rail_source::Profile = serde_json::from_str(LEGACY_PROFILE).unwrap();
        crate::rail_generation::check_profile(&profile).unwrap();
        let old: rail_source::Profile = serde_json::from_str(include_str!(
            "../../../compatibility/rails/client-0f703d43-meshes.json"
        ))
        .unwrap();
        assert!(crate::rail_generation::check_profile(&old).is_err());
        for field in [
            "ring",
            "approaches",
            "bed",
            "mesh",
            "branching",
            "siding",
            "reservations",
        ] {
            let mut value = serde_json::to_value(&profile).unwrap();
            value[field] = serde_json::Value::Null;
            let incomplete: rail_source::Profile = serde_json::from_value(value).unwrap();
            assert!(
                crate::rail_generation::check_profile(&incomplete).is_err(),
                "{field}"
            );
        }
    }
    #[test]
    fn preparation_cancellation_and_source_mismatch_leave_no_output() {
        let (catalog, raw) = crate::infrastructure::tests::fixture();
        let source: infrastructure::Source = serde_json::from_value(raw).unwrap();
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing");
        let output = root.path().join("output");
        assert!(
            prepare_in(
                root.path(),
                &catalog,
                &missing,
                &source,
                &missing,
                None,
                &output,
                || true
            )
            .err()
            .unwrap()
            .contains("cancelled")
        );
        assert!(
            prepare_in(
                root.path(),
                &catalog,
                &missing,
                &source,
                &missing,
                None,
                &output,
                || false
            )
            .err()
            .unwrap()
            .contains("No matching rail preparation adapter")
        );
        assert!(!output.exists());
    }
    #[test]
    fn parent_and_candidate_documents_are_bounded_and_immutable() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("parent.json");
        std::fs::write(&path, b"{}tail").unwrap();
        assert!(verify_document(&path, &format!("{:x}", Sha256::digest(b"{}tail")), 2).is_err());
        assert!(verify_document(&path, &format!("{:x}", Sha256::digest(b"{}")), 20).is_err());
        assert!(verify_document(&path, &format!("{:x}", Sha256::digest(b"{}tail")), 20).is_ok());
        for id in ["a".repeat(64), "invalid".into()] {
            std::fs::create_dir(root.path().join(&id)).unwrap();
            std::fs::write(root.path().join(&id).join("rails.json"), b"{}").unwrap();
        }
        assert_eq!(documents(root.path(), "rails.json").unwrap().len(), 1);
    }
}
