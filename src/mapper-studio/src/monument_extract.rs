//! Cancellable, packaged extraction of source monument checks and preview models.
use crate::{
    metadata_runtime::{self, Script},
    monument_source::Source,
    prefab_assets::Catalog,
    prefab_textures::Bindings,
};
use std::{fs::File, io::Read, path::Path};
#[derive(Clone, Copy)]
struct Capture {
    models: bool,
    environment_meshes: bool,
}
impl Capture {
    fn preview(models: bool) -> Self {
        Self {
            models,
            environment_meshes: false,
        }
    }
}
pub fn prepare(
    root: &Path,
    catalog: &Catalog,
    bindings: &Bindings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Source>, String> {
    prepare_local(root, catalog, bindings, Capture::preview(true), cancelled)
}
/// Match optional model capture to the collision coverage of the accepted rules.
pub fn prepare_models_for(
    root: &Path,
    catalog: &Catalog,
    bindings: &Bindings,
    rules: &Source,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Source>, String> {
    prepare_local(
        root,
        catalog,
        bindings,
        Capture {
            models: true,
            environment_meshes: has_environment_meshes(rules),
        },
        cancelled,
    )
}
/// Prepare placement checks without decoding optional preview meshes.
pub fn prepare_rules(
    root: &Path,
    catalog: &Catalog,
    bindings: &Bindings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Source>, String> {
    prepare_local(root, catalog, bindings, Capture::preview(false), cancelled)
}
/// Prepare raw environment collision buffers only when the world collision
/// adapter needs them. Capturing buffers is not physics cooking or registration.
pub fn prepare_environment(
    root: &Path,
    catalog: &Catalog,
    bindings: &Bindings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Source>, String> {
    use sha2::{Digest, Sha256};
    let profile = selected_profile(catalog)?
        .ok_or("No environment capture adapter for this Rust installation")?;
    let producer = metadata_runtime::monument_extractor_sha256();
    let profile_hashes = crate::source_profile::hashes(profile);
    let cache = crate::storage::local_data()?.join("papyrus-monument-cache");
    let mut primary_lookup = None;
    for (candidate, profile_sha256) in metadata_runtime::monument_cache_extractors(producer)
        .into_iter()
        .flat_map(|candidate| profile_hashes.iter().map(move |profile| (candidate.clone(), profile)))
    {
        let key = serde_json::to_vec(&(
            "mapper-monument-environment-cache-1",
            &catalog.sha256,
            &bindings.sha256,
            &candidate,
            &profile_sha256,
        ))
        .map_err(|error| error.to_string())?;
        let lookup = cache.join(format!("environment-{:x}.json", Sha256::digest(key)));
        primary_lookup.get_or_insert_with(|| lookup.clone());
        if !lookup.try_exists().map_err(|error| error.to_string())? {
            continue;
        }
        let mut bytes = Vec::new();
        File::open(&lookup)
            .map_err(|error| error.to_string())?
            .take(129)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        let sha: String = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        if sha.len() != 64
            || !sha
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("Invalid monument environment cache receipt".into());
        }
        let document = cache.join(&sha).join("monuments.json");
        if document.try_exists().map_err(|error| error.to_string())? {
            let source = Source::load(&document, catalog, bindings, &cancelled)?;
            if source.sha256 != sha
                || source.extractor_sha256 != candidate
                || source.profile_sha256 != *profile_sha256
                || !has_environment_meshes(&source)
            {
                return Err("Monument environment cache receipt changed".into());
            }
            return Ok((!cancelled()).then_some(source));
        }
    }
    let Some(source) = prepare_local(
        root,
        catalog,
        bindings,
        Capture {
            models: false,
            environment_meshes: true,
        },
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    if !has_environment_meshes(&source) {
        return Err("Monument environment capture has incomplete mesh coverage".into());
    }
    if cancelled() {
        return Ok(None);
    }
    crate::profile::write_document(
        &primary_lookup.expect("The compiled producer is always present"),
        &source.sha256,
        true,
    )?;
    Ok((!cancelled()).then_some(source))
}
pub(crate) fn has_environment_meshes(source: &Source) -> bool {
    !source.prefabs.is_empty()
        && source.prefabs.values().all(|prefab| {
            prefab
                .placement_attributes
                .as_ref()
                .and_then(|attributes| attributes.environment.as_ref())
                .is_some_and(|environment| environment.collider_meshes.is_some())
        })
}
fn prepare_local(
    root: &Path,
    catalog: &Catalog,
    bindings: &Bindings,
    capture: Capture,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Source>, String> {
    let local = crate::storage::local_data()?;
    let catalog_path =
        crate::asset_discovery::catalog_path(&local.join("papyrus-prefab-cache"), &catalog.sha256)?;
    prepare_request(
        root,
        catalog,
        bindings,
        &catalog_path,
        &local.join("papyrus-monument-cache"),
        capture,
        cancelled,
    )
}
pub fn prepare_in(
    root: &Path,
    catalog: &Catalog,
    bindings: &Bindings,
    catalog_path: &Path,
    cache: &Path,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Source>, String> {
    prepare_request(
        root,
        catalog,
        bindings,
        catalog_path,
        cache,
        Capture::preview(true),
        cancelled,
    )
}
fn prepare_request(
    root: &Path,
    catalog: &Catalog,
    bindings: &Bindings,
    catalog_path: &Path,
    cache: &Path,
    capture: Capture,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Source>, String> {
    if cancelled() {
        return Ok(None);
    }
    let profile = selected_profile(catalog)?
        .ok_or("No source-matched monument adapter is available for this Rust installation")?;
    use sha2::{Digest, Sha256};
    let mut raw = Vec::new();
    File::open(catalog_path)
        .map_err(|e| e.to_string())?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() > 16 * 1024 * 1024 || format!("{:x}", Sha256::digest(&raw)) != catalog.sha256 {
        return Err("Verified catalog changed before monument preparation".into());
    }
    std::fs::create_dir_all(cache).map_err(|e| e.to_string())?;
    let temporary = tempfile::Builder::new()
        .prefix(".monument-prepare-")
        .tempdir_in(cache)
        .map_err(|e| e.to_string())?;
    let profile_path = temporary.path().join("profile.json");
    std::fs::write(&profile_path, profile).map_err(|e| e.to_string())?;
    let output = temporary.path().join("output");
    let mut arguments = vec![
        "--rust-root".into(),
        root.as_os_str().to_owned(),
        "--catalog".into(),
        catalog_path.as_os_str().to_owned(),
        "--profile".into(),
        profile_path.as_os_str().to_owned(),
        "--cache-root".into(),
        output.as_os_str().to_owned(),
    ];
    if capture.models {
        arguments.push("--models".into());
    }
    if capture.environment_meshes {
        arguments.push("--environment-meshes".into());
    }
    if !metadata_runtime::run(Script::Monuments, &arguments, &cancelled)? {
        return Ok(None);
    }
    let document = metadata_runtime::document(&output, "monuments.json")?;
    let source = Source::load(&document, catalog, bindings, &cancelled)?;
    if cancelled() {
        return Ok(None);
    }
    metadata_runtime::publish(&document, cache, &source.sha256, 32 * 1024 * 1024)?;
    Ok((!cancelled()).then_some(source))
}
/// Preparation profiles bind numeric capture to the selected installation.
/// Coastal execution remains selected and verified by its separate adapter.
pub(crate) fn selected_profile(catalog: &Catalog) -> Result<Option<&'static str>, String> {
    for raw in metadata_runtime::MONUMENT_PROFILES {
        let value = serde_json::from_str(raw).map_err(|error| error.to_string())?;
        if profile_matches(&value, catalog) {
            return Ok(Some(raw));
        }
    }
    Ok(None)
}
pub(crate) fn profile_matches(value: &serde_json::Value, catalog: &Catalog) -> bool {
    value["format"] == "papyrus-monument-profile"
        && matches!(value["schema"].as_u64(), Some(1 | 2))
        && value["native_sha256"].as_str() == Some(catalog.installation.native.sha256.as_str())
        && value["scene_sha256"].as_str()
            == catalog
                .sources
                .iter()
                .find(|s| s.relative_path == "Bundles/shared/assetscenes.bundle")
                .map(|s| s.sha256.as_str())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bundled_profiles_select_the_matching_native_and_scene_pair() {
        let (mut catalog, _) = crate::infrastructure::tests::fixture();
        assert!(selected_profile(&catalog).unwrap().is_none());
        for raw in metadata_runtime::MONUMENT_PROFILES {
            let value: serde_json::Value = serde_json::from_str(raw).unwrap();
            catalog.installation.native.sha256 = value["native_sha256"].as_str().unwrap().into();
            let scene = catalog
                .sources
                .iter_mut()
                .find(|source| source.relative_path == "Bundles/shared/assetscenes.bundle")
                .unwrap();
            scene.sha256 = value["scene_sha256"].as_str().unwrap().into();
            assert_eq!(selected_profile(&catalog).unwrap(), Some(raw));
            assert_eq!(value["schema"], 2);
            assert_eq!(value["code_sources"].as_array().unwrap().len(), 2);
            assert_eq!(value["tier_to_topology"]["1"], 1_u32 << 26);
            assert_eq!(value["tier_to_topology"]["2"], 1_u32 << 27);
            assert_eq!(value["tier_to_topology"]["4"], 1_u32 << 28);
            assert_eq!(value["min_corners"], 3);
            catalog.installation.native.sha256 = "e".repeat(64);
            assert!(selected_profile(&catalog).unwrap().is_none());
            catalog.installation.native.sha256 = value["native_sha256"].as_str().unwrap().into();
            catalog
                .sources
                .iter_mut()
                .find(|source| source.relative_path == "Bundles/shared/assetscenes.bundle")
                .unwrap()
                .sha256 = "e".repeat(64);
            assert!(selected_profile(&catalog).unwrap().is_none());
        }
    }
    #[test]
    #[ignore = "requires owned installation, verified catalog, texture bindings and reference capture"]
    fn owned_profile_prepares_matching_numeric_rules_and_optional_models() {
        let path = |name| std::path::PathBuf::from(std::env::var_os(name).expect(name));
        let root = path("MAPPER_MONUMENT_TEST_ROOT");
        let _leases = crate::source_lease::Scope::default();
        let installation = crate::installation::inspect(&root, || false).unwrap();
        let catalog_path = path("MAPPER_MONUMENT_TEST_CATALOG");
        let catalog =
            crate::prefab_assets::load(&root, &catalog_path, &installation, || false).unwrap();
        let bindings = Bindings::load(
            &root,
            &path("MAPPER_MONUMENT_TEST_BINDINGS"),
            &catalog,
            || false,
        )
        .unwrap();
        assert!(selected_profile(&catalog).unwrap().is_some());
        let reference = Source::load(
            &path("MAPPER_MONUMENT_TEST_REFERENCE"),
            &catalog,
            &bindings,
            || false,
        )
        .unwrap();
        let models = std::env::var("MAPPER_MONUMENT_TEST_MODELS").as_deref() == Ok("1");
        let cache = tempfile::tempdir().unwrap();
        let capture = Capture {
            models,
            environment_meshes: has_environment_meshes(&reference),
        };
        assert!(
            prepare_request(
                &root,
                &catalog,
                &bindings,
                &catalog_path,
                cache.path(),
                capture,
                || true,
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 0);
        let mut source = prepare_request(
            &root,
            &catalog,
            &bindings,
            &catalog_path,
            cache.path(),
            capture,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(source.models_requested, models);
        assert!(source.matches_placement_data(&reference));
        assert_eq!(
            source.extractor_sha256,
            metadata_runtime::monument_extractor_sha256()
        );
        assert!(source.coastal_profile.is_none());
        assert_eq!(source.recipe().engine, crate::monument_source::VERSION);
        let published = cache.path().join(&source.sha256).join("monuments.json");
        let loaded = Source::load(&published, &catalog, &bindings, || false).unwrap();
        assert_eq!(loaded.sha256, source.sha256);
        if models {
            assert!(
                source
                    .prefabs
                    .values()
                    .any(|prefab| !prefab.model.triangles.is_empty())
            );
        }
        source.prepare_coastal(&catalog, &root, || false).unwrap();
        source.prepare_stages(&catalog, &root, || false).unwrap();
        let coastal = source.coastal_profile.as_ref().unwrap();
        let stages = source.stage_source.as_ref().unwrap();
        let mut matched = 0;
        for path in source.prefabs.keys() {
            if let Some(filter) = coastal.filter(path) {
                assert_eq!(Some(filter), stages.anchor_filter(path), "{path}");
                matched += 1;
            }
        }
        assert!(matched >= 3);
        assert_eq!(
            source.recipe().engine,
            crate::coastal_placement::BUDGET_VERSION
        );
        println!(
            "prepared {} prefabs, models {}, snapshot {}",
            source.prefabs.len(),
            models,
            source.sha256
        );
    }
    #[test]
    fn source_preparation_does_not_select_the_coastal_execution_profile() {
        let (catalog, _) = crate::infrastructure::tests::fixture();
        let mut value = serde_json::json!({"format":"papyrus-monument-profile","schema":1,"native_sha256":"a".repeat(64),"scene_sha256":"c".repeat(64)});
        assert!(profile_matches(&value, &catalog));
        value["format"] = "papyrus-coastal-placement-profile".into();
        assert!(!profile_matches(&value, &catalog));
        value["format"] = "papyrus-monument-profile".into();
        value["schema"] = 2.into();
        assert!(profile_matches(&value, &catalog));
        value["native_sha256"] = "d".repeat(64).into();
        assert!(!profile_matches(&value, &catalog));
        value["native_sha256"] = "a".repeat(64).into();
        value["scene_sha256"] = "d".repeat(64).into();
        assert!(!profile_matches(&value, &catalog));
        value["scene_sha256"] = "c".repeat(64).into();
        value["schema"] = 3.into();
        assert!(!profile_matches(&value, &catalog));
    }
}
