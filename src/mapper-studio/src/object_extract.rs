//! Automatic local decoration and roadside metadata capture with packaged parsers.
use crate::{
    decor_source, infrastructure,
    installation::SourceFile,
    metadata_runtime::{self, Script},
    prefab_assets::Catalog,
    road_object_source,
};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
};
const TOPOLOGY: &str = include_str!("../../../compatibility/topology/client-0f703d43.json");
const CURRENT_TOPOLOGY: &str = include_str!("../../../compatibility/topology/client-8dd4e235.json");
const PLACEMENT: &str = include_str!("../../../compatibility/road-objects/client-0f703d43.json");
const CURRENT_PLACEMENT: &str =
    include_str!("../../../compatibility/road-objects/client-8dd4e235.json");
const PROFILES: [(&str, &str); 3] = [
    (TOPOLOGY, PLACEMENT),
    (CURRENT_TOPOLOGY, CURRENT_PLACEMENT),
    (
        include_str!("../../../compatibility/topology/client-58f3263f.json"),
        include_str!("../../../compatibility/road-objects/client-58f3263f.json"),
    ),
];
fn placement_profile(parent: &infrastructure::Source) -> Result<&'static str, String> {
    let parent_profile = serde_json::to_value(&parent.profile).map_err(|e| e.to_string())?;
    for (_, raw) in PROFILES {
        let profile: serde_json::Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
        if [
            "native_sha256",
            "scene_sha256",
            "code_sources",
            "stage_layout",
        ]
        .into_iter()
        .all(|key| profile[key] == parent_profile[key])
        {
            return Ok(raw);
        }
    }
    Err("No matching road object preparation adapter for this installation".into())
}
fn digest(raw: &[u8]) -> String {
    format!("{:x}", Sha256::digest(raw))
}
fn cancelled(stop: &impl Fn() -> bool) -> Result<(), String> {
    if stop() {
        Err("Object metadata preparation cancelled".into())
    } else {
        Ok(())
    }
}
fn valid_hash(sha: &str) -> bool {
    sha.len() == 64
        && sha
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn profiles(
    topology_raw: &str,
    placement_raw: &str,
) -> Result<(serde_json::Value, Vec<u8>, Vec<SourceFile>), String> {
    let placement: serde_json::Value =
        serde_json::from_str(placement_raw).map_err(|e| e.to_string())?;
    let mut topology: serde_json::Value =
        serde_json::from_str(topology_raw).map_err(|e| e.to_string())?;
    if topology["native_sha256"] != placement["native_sha256"]
        || topology["scene_sha256"] != placement["scene_sha256"]
    {
        return Err("Object preparation profiles refer to different installations".into());
    }
    topology["code_sources"] = placement["code_sources"].clone();
    if let Some(layout) = placement.get("stage_layout") {
        topology["stage_layout"] = layout.clone();
    }
    let code =
        serde_json::from_value(placement["code_sources"].clone()).map_err(|e| e.to_string())?;
    let raw = serde_json::to_vec(&topology).map_err(|e| e.to_string())?;
    Ok((topology, raw, code))
}
fn matches(profile: &serde_json::Value, catalog: &Catalog) -> bool {
    profile["native_sha256"].as_str() == Some(&catalog.installation.native.sha256)
        && catalog.sources.iter().any(|r| {
            r.relative_path == "Bundles/shared/assetscenes.bundle"
                && profile["scene_sha256"] == r.sha256
        })
}
pub(crate) fn documents(cache: &Path, name: &str) -> Result<Vec<PathBuf>, String> {
    if !cache.exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for (i, entry) in std::fs::read_dir(cache)
        .map_err(|e| e.to_string())?
        .take(257)
        .enumerate()
    {
        if i == 256 {
            return Err("Too many object metadata snapshots".into());
        }
        let entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if !kind.is_dir() || kind.is_symlink() || !valid_hash(&entry.file_name().to_string_lossy())
        {
            continue;
        }
        let path = entry.path().join(name);
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file()) {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}
fn in_named_directory(path: &Path, sha: &str) -> bool {
    path.parent()
        .and_then(Path::file_name)
        .and_then(|p| p.to_str())
        == Some(sha)
}
pub fn prepare_decor(
    root: &Path,
    catalog: &Catalog,
    pin: Option<&str>,
    stop: impl Fn() -> bool,
) -> Result<decor_source::Source, String> {
    prepare_decor_in(root, catalog, pin, &crate::storage::local_data()?, stop)
}
pub fn prepare_decor_shared(
    root: &Path,
    catalog: &Catalog,
    pin: Option<&str>,
    stop: impl Fn() -> bool,
) -> Result<std::sync::Arc<decor_source::Source>, String> {
    prepare_decor(root, catalog, pin, stop)
        .map(|source| source.retain_verified(root, catalog))
}
pub fn prepare_decor_in(
    root: &Path,
    catalog: &Catalog,
    pin: Option<&str>,
    local: &Path,
    stop: impl Fn() -> bool,
) -> Result<decor_source::Source, String> {
    cancelled(&stop)?;
    let cache = local.join("papyrus-decor-cache");
    if let Some(pin) = pin {
        if !valid_hash(pin) {
            return Err("Invalid pinned decoration identity".into());
        }
        let source =
            decor_source::Source::load(root, &crate::client_cache::pinned_document(
                local,
                "papyrus-decor-cache",
                pin,
                "decor.json",
            )?, catalog, &stop)?;
        if source.sha256 != pin {
            return Err("Pinned decoration source changed".into());
        }
        return Ok(source);
    }
    let (_, raw, code) = PROFILES
        .into_iter()
        .map(|(topology, placement)| profiles(topology, placement))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .find(|(profile, _, _)| matches(profile, catalog))
        .ok_or("No matching decoration preparation adapter for this installation")?;
    let profile_sha = digest(&raw);
    for path in documents(&cache, "decor.json")? {
        cancelled(&stop)?;
        if let Ok(source) = decor_source::Source::load(root, &path, catalog, &stop)
            && in_named_directory(&path, &source.sha256)
            && source.preparation_matches(&profile_sha, &code)
        {
            return Ok(source);
        }
    }
    cancelled(&stop)?;
    std::fs::create_dir_all(local).map_err(|e| e.to_string())?;
    let temporary = tempfile::Builder::new()
        .prefix(".decor-prepare-")
        .tempdir_in(local)
        .map_err(|e| e.to_string())?;
    let profile_path = temporary.path().join("profile.json");
    std::fs::write(&profile_path, raw).map_err(|e| e.to_string())?;
    let output = temporary.path().join("output");
    if !metadata_runtime::run(
        Script::Decoration,
        &[
            "--rust-root".into(),
            root.as_os_str().to_owned(),
            "--topology-profile".into(),
            profile_path.into_os_string(),
            "--colliders".into(),
            "--cache-root".into(),
            output.as_os_str().to_owned(),
        ],
        &stop,
    )? {
        return Err("Decoration preparation cancelled".into());
    }
    let document = metadata_runtime::document(&output, "decor.json")?;
    let source = decor_source::Source::load(root, &document, catalog, &stop)?;
    if !source.preparation_matches(&profile_sha, &code) {
        return Err(
            "Decoration extraction returned incomplete or mismatched gameplay metadata".into(),
        );
    }
    source.prepare_live(catalog)?;
    cancelled(&stop)?;
    metadata_runtime::publish(&document, &cache, &source.sha256, 16 * 1024 * 1024)?;
    Ok(source)
}
pub fn prepare_roads(
    root: &Path,
    parent: &infrastructure::Source,
    pin: Option<&str>,
    stop: impl Fn() -> bool,
) -> Result<road_object_source::Source, String> {
    prepare_paths(
        root,
        parent,
        pin,
        road_object_source::PathFamily::Road,
        stop,
    )
}
pub fn prepare_rivers(
    root: &Path,
    parent: &infrastructure::Source,
    pin: Option<&str>,
    stop: impl Fn() -> bool,
) -> Result<road_object_source::Source, String> {
    prepare_paths(
        root,
        parent,
        pin,
        road_object_source::PathFamily::River,
        stop,
    )
}
fn prepare_paths(
    root: &Path,
    parent: &infrastructure::Source,
    pin: Option<&str>,
    family: road_object_source::PathFamily,
    stop: impl Fn() -> bool,
) -> Result<road_object_source::Source, String> {
    let local = crate::storage::local_data()?;
    let parent_path = crate::client_cache::pinned_document(
        &local,
        "papyrus-infrastructure-cache",
        &parent.sha256,
        "infrastructure.json",
    )?;
    prepare_paths_in(root, parent, &parent_path, pin, &local, family, stop)
}
pub fn prepare_roads_in(
    root: &Path,
    parent: &infrastructure::Source,
    parent_path: &Path,
    pin: Option<&str>,
    local: &Path,
    stop: impl Fn() -> bool,
) -> Result<road_object_source::Source, String> {
    prepare_paths_in(
        root,
        parent,
        parent_path,
        pin,
        local,
        road_object_source::PathFamily::Road,
        stop,
    )
}
pub fn prepare_rivers_in(
    root: &Path,
    parent: &infrastructure::Source,
    parent_path: &Path,
    pin: Option<&str>,
    local: &Path,
    stop: impl Fn() -> bool,
) -> Result<road_object_source::Source, String> {
    prepare_paths_in(
        root,
        parent,
        parent_path,
        pin,
        local,
        road_object_source::PathFamily::River,
        stop,
    )
}
fn prepare_paths_in(
    root: &Path,
    parent: &infrastructure::Source,
    parent_path: &Path,
    pin: Option<&str>,
    local: &Path,
    family: road_object_source::PathFamily,
    stop: impl Fn() -> bool,
) -> Result<road_object_source::Source, String> {
    cancelled(&stop)?;
    let (folder, script) = match family {
        road_object_source::PathFamily::Road => ("papyrus-road-object-cache", Script::RoadObjects),
        road_object_source::PathFamily::River => {
            ("papyrus-river-object-cache", Script::RiverObjects)
        }
        road_object_source::PathFamily::Powerline => {
            return Err("Utility attachments require their prepared utility source".into());
        }
    };
    let cache = local.join(folder);
    if let Some(pin) = pin {
        if !valid_hash(pin) {
            return Err("Invalid pinned road object identity".into());
        }
        let source = road_object_source::Source::load(
            root,
            &crate::client_cache::pinned_document(
                local,
                folder,
                pin,
                "objects.json",
            )?,
            parent,
            &stop,
        )?;
        if source.sha256 != pin || source.path_family != family {
            return Err("Pinned road object source changed".into());
        }
        return Ok(source);
    }
    let profile = placement_profile(parent)?;
    let profile_sha = digest(profile.as_bytes());
    let extractor = match family {
        road_object_source::PathFamily::Road => String::new(),
        road_object_source::PathFamily::River => {
            crate::metadata_runtime::river_objects_extractor_sha256()
        }
        road_object_source::PathFamily::Powerline => {
            return Err("Utility attachments require their prepared utility source".into());
        }
    };
    let matches = |source: &road_object_source::Source| match family {
        road_object_source::PathFamily::Road => source.preparation_matches(&profile_sha),
        road_object_source::PathFamily::River => {
            source.river_preparation_matches(&profile_sha, &extractor)
        }
        road_object_source::PathFamily::Powerline => false,
    };
    for path in documents(&cache, "objects.json")? {
        cancelled(&stop)?;
        if let Ok(source) = road_object_source::Source::load(root, &path, parent, &stop)
            && in_named_directory(&path, &source.sha256)
            && matches(&source)
        {
            return Ok(source);
        }
    }
    let mut raw = Vec::new();
    std::fs::File::open(parent_path)
        .map_err(|e| e.to_string())?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() > 16 * 1024 * 1024 || digest(&raw) != parent.sha256 {
        return Err("Road infrastructure changed before object preparation".into());
    }
    if family == road_object_source::PathFamily::River {
        parent.verify_installation(root, &stop)?;
    }
    cancelled(&stop)?;
    std::fs::create_dir_all(local).map_err(|e| e.to_string())?;
    let temporary = tempfile::Builder::new()
        .prefix(".road-object-prepare-")
        .tempdir_in(local)
        .map_err(|e| e.to_string())?;
    let profile_path = temporary.path().join("profile.json");
    std::fs::write(&profile_path, profile).map_err(|e| e.to_string())?;
    let output = temporary.path().join("output");
    if !metadata_runtime::run(
        script,
        &[
            "--rust-root".into(),
            root.as_os_str().to_owned(),
            "--infrastructure".into(),
            parent_path.as_os_str().to_owned(),
            "--placement-profile".into(),
            profile_path.into_os_string(),
            "--cache-root".into(),
            output.as_os_str().to_owned(),
        ],
        &stop,
    )? {
        return Err("Road object preparation cancelled".into());
    }
    let document = metadata_runtime::document(&output, "objects.json")?;
    let source = road_object_source::Source::load(root, &document, parent, &stop)?;
    if !matches(&source) {
        return Err("Road object extraction returned incomplete placement metadata".into());
    }
    source.procedural_order()?;
    cancelled(&stop)?;
    metadata_runtime::publish(&document, &cache, &source.sha256, 16 * 1024 * 1024)?;
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn road_object_profiles_require_native_scene_code_and_layout_identity() {
        for (_, raw_profile) in PROFILES {
            let profile: serde_json::Value = serde_json::from_str(raw_profile).unwrap();
            let (_, mut document) = crate::infrastructure::tests::fixture();
            for key in [
                "native_sha256",
                "scene_sha256",
                "code_sources",
                "stage_layout",
            ] {
                document["profile"][key] = profile[key].clone();
            }
            let source: infrastructure::Source = serde_json::from_value(document.clone()).unwrap();
            assert_eq!(placement_profile(&source).unwrap(), raw_profile);
            for (pointer, value) in [
                ("/profile/native_sha256", serde_json::json!("a".repeat(64))),
                ("/profile/scene_sha256", serde_json::json!("b".repeat(64))),
                ("/profile/code_sources/0/bytes", serde_json::json!(1)),
                (
                    "/profile/code_sources/1/sha256",
                    serde_json::json!("c".repeat(64)),
                ),
                ("/profile/stage_layout", serde_json::json!("future")),
            ] {
                let mut changed = document.clone();
                *changed.pointer_mut(pointer).unwrap() = value;
                let changed = serde_json::from_value(changed).unwrap();
                assert!(placement_profile(&changed).is_err(), "{pointer}");
            }
        }
    }
    #[test]
    #[ignore = "requires an owned client, metadata runtime and receipted road bundle"]
    fn owned_current_preparation_fits_objects_against_saved_roads() {
        let path = |name| PathBuf::from(std::env::var_os(name).expect(name));
        let root = path("MAPPER_OBJECT_PREP_TEST_ROOT");
        let _verification = crate::installation::VerificationScope::default();
        let installation = crate::installation::inspect(&root, || false).unwrap();
        let catalog = crate::prefab_assets::load(
            &root,
            &path("MAPPER_OBJECT_PREP_TEST_CATALOG"),
            &installation,
            || false,
        )
        .unwrap();
        let parent_path = path("MAPPER_OBJECT_PREP_TEST_PARENT");
        let parent = infrastructure::Source::load(&parent_path, &catalog, || false).unwrap();
        let source = prepare_roads_in(
            &root,
            &parent,
            &parent_path,
            None,
            &crate::storage::local_data().unwrap(),
            || false,
        )
        .unwrap();
        let reopened = prepare_roads(&root, &parent, Some(&source.sha256), || false).unwrap();
        assert_eq!(source.sha256, reopened.sha256);
        assert!(source.preparation_matches(&digest(placement_profile(&parent).unwrap().as_bytes())));
        let bundle = path("MAPPER_OBJECT_PREP_TEST_BUNDLE");
        let project = crate::Project::load(&bundle.join("project.mapper")).unwrap();
        let terrain = crate::rail_preview::Terrain::load(&bundle, &project, || false).unwrap();
        let (layers, alpha) = terrain.bed_layers(&bundle, || false).unwrap();
        let report = terrain.road_report(&bundle, || false).unwrap();
        let roads = crate::road_object_placement::roads_from_report(
            &report,
            parent.profile.road_hierarchy.unwrap(),
            parent.profile.width,
        )
        .unwrap();
        let fitting = crate::road_object_placement::fit(
            &layers,
            &alpha,
            &roads,
            &source,
            crate::road_object_placement::Settings {
                seed: project.seeds[3].value,
                maximum_work: 2_000_000,
                maximum_poses: 100_000,
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(!fitting.budget_exhausted);
        assert!(!fitting.provisional_poses.is_empty());
        assert!(
            fitting
                .prefabs
                .values()
                .all(|prefab| prefab.unsupported.is_empty())
        );
        assert!(!fitting.terrain_applied && !fitting.physics_validated);
        let mut report = source.inspect(true);
        report["pinned_source_reopened"] = true.into();
        report["roads_checked"] = roads.len().into();
        report["fitting"] = serde_json::to_value(fitting).unwrap();
        std::fs::write(
            path("MAPPER_OBJECT_PREP_TEST_REPORT"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
    }
    #[test]
    #[ignore = "requires an owned client and metadata runtime"]
    fn owned_decoration_preparation_resolves_shared_procedural_order() {
        let path = |name| PathBuf::from(std::env::var_os(name).expect(name));
        let root = path("MAPPER_OBJECT_PREP_TEST_ROOT");
        let _verification = crate::installation::VerificationScope::default();
        let installation = crate::installation::inspect(&root, || false).unwrap();
        let catalog = crate::prefab_assets::load(
            &root,
            &path("MAPPER_OBJECT_PREP_TEST_CATALOG"),
            &installation,
            || false,
        )
        .unwrap();
        let parent =
            infrastructure::Source::load(&path("MAPPER_OBJECT_PREP_TEST_PARENT"), &catalog, || {
                false
            })
            .unwrap();
        let source = prepare_decor(&root, &catalog, None, || false).unwrap();
        let reopened = prepare_decor(&root, &catalog, Some(&source.sha256), || false).unwrap();
        assert_eq!(source.sha256, reopened.sha256);
        let live = reopened.prepare_live(&catalog).unwrap();
        for (name, prefab) in &live.fitting {
            assert!(
                prefab.metadata().unsupported.is_empty(),
                "{name} {:?}",
                prefab.metadata().unsupported
            );
        }
        for (name, prefab) in &live.terrain {
            assert!(
                prefab.unsupported.is_empty(),
                "{name} {:?}",
                prefab.unsupported
            );
        }
        let roads = prepare_roads(&root, &parent, None, || false).unwrap();
        source.verify_road_binding(&roads).unwrap();
        let workflow = crate::object_workflow::plan(&roads, &source.stages).unwrap();
        assert_eq!(
            workflow
                .steps
                .iter()
                .filter(|step| matches!(
                    step.handler,
                    crate::object_workflow::Handler::Decoration { .. }
                ))
                .count(),
            source.stages.len()
        );
        assert_eq!(
            workflow
                .steps
                .iter()
                .filter(|step| matches!(
                    step.handler,
                    crate::object_workflow::Handler::RoadObjects { .. }
                ))
                .count(),
            roads.stages.len()
        );
        assert_eq!(live.topology_flags, parent.profile.topology_flags.unwrap());
        let report = serde_json::json!({
            "format":"mapper-owned-decoration-preparation", "schema":1,
            "source_sha256":source.sha256, "pinned_source_reopened":true,
            "fitting_prefabs":live.fitting.len(), "terrain_prefabs":live.terrain.len(),
            "unsupported_fitting_prefabs":0, "unsupported_terrain_prefabs":0,
            "workflow":workflow, "terrain_applied":false, "game_runtime_validated":false,
        });
        std::fs::write(
            path("MAPPER_DECOR_PREP_TEST_REPORT"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
    }
    #[test]
    fn cancelled_mismatched_and_missing_pinned_inputs_never_publish_a_cache() {
        let (catalog, raw) = crate::infrastructure::tests::fixture();
        let parent: infrastructure::Source = serde_json::from_value(raw).unwrap();
        let root = tempfile::tempdir().unwrap();
        let local = root.path().join("cache");
        let missing = root.path().join("missing.json");
        assert!(
            prepare_decor_in(root.path(), &catalog, None, &local, || true)
                .err()
                .unwrap()
                .contains("cancelled")
        );
        assert!(
            prepare_decor_in(root.path(), &catalog, None, &local, || false)
                .err()
                .unwrap()
                .contains("No matching")
        );
        assert!(
            prepare_decor_in(root.path(), &catalog, Some("../../bad"), &local, || false)
                .err()
                .unwrap()
                .contains("Invalid pinned")
        );
        assert!(
            prepare_decor_in(root.path(), &catalog, Some(&"1".repeat(64)), &local, || {
                false
            })
            .is_err()
        );
        assert!(
            prepare_roads_in(root.path(), &parent, &missing, None, &local, || true)
                .err()
                .unwrap()
                .contains("cancelled")
        );
        assert!(
            prepare_roads_in(root.path(), &parent, &missing, None, &local, || false)
                .err()
                .unwrap()
                .contains("No matching")
        );
        assert!(
            prepare_roads_in(
                root.path(),
                &parent,
                &missing,
                Some("../bad"),
                &local,
                || false
            )
            .err()
            .unwrap()
            .contains("Invalid pinned")
        );
        assert!(!local.exists());
    }
    #[test]
    fn decoration_profile_keeps_source_roles_and_includes_exact_gameplay_receipts() {
        let (profile, raw, code) = profiles(TOPOLOGY, PLACEMENT).unwrap();
        let original: serde_json::Value = serde_json::from_str(TOPOLOGY).unwrap();
        assert_eq!(profile["flags"], original["flags"]);
        assert_eq!(profile["collider_sampling"], original["collider_sampling"]);
        assert_eq!(code.len(), 2);
        assert_eq!(
            profile,
            serde_json::from_slice::<serde_json::Value>(&raw).unwrap()
        );
        assert_eq!(raw, profiles(TOPOLOGY, PLACEMENT).unwrap().1);
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("a".repeat(64))).unwrap();
        std::fs::write(root.path().join("a".repeat(64)).join("decor.json"), b"{}").unwrap();
        std::fs::create_dir(root.path().join("not-a-snapshot")).unwrap();
        assert_eq!(documents(root.path(), "decor.json").unwrap().len(), 1);
    }
}
