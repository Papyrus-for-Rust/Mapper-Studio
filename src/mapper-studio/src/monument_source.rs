//! Immutable source-bound monument rules and static model preview geometry.
use crate::{installation::SourceFile, prefab_assets::Catalog, prefab_textures::Bindings};
use papyrus_terrain::monument_eligibility::Rules;
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell, collections::BTreeMap, fs::File, io::Read,
    path::{Path, PathBuf}, rc::{Rc, Weak}, sync::Arc,
};
pub const SURFACE_MODIFIER_VERSION: &str = "papyrus-placement-surface-modifiers-2";
pub const SOURCE_PRIORITY_VERSION: &str = "papyrus-placement-surface-priority-3";
pub const LEGACY_SURFACE_MODIFIER_VERSION: &str = "papyrus-placement-surface-modifiers-1";
pub const CANONICAL_YAW_VERSION: &str = "papyrus-canonical-placement-yaw-1";
pub const VERSION: &str = "papyrus-placement-source-checks-5";
pub const TIER_RETRY_VERSION: &str = "papyrus-tier-corner-retry-1";
pub const AUTHORED_EARTHWORK_VERSION: &str = "papyrus-authored-earthwork-1";
pub fn supported(version: &str) -> bool {
    version == VERSION || crate::coastal_placement::supported(version)
}
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        installation::{Installation, InstallationKind},
        prefab_assets::Asset,
    };

    #[test]
    #[ignore = "requires an owned Rust installation and pinned monument recipe"]
    fn owned_generation_scope_reuses_only_the_matching_monument_snapshot() {
        let path = |name| PathBuf::from(std::env::var_os(name).expect(name));
        let root = path("MAPPER_MODEL_TEST_ROOT");
        let mut project = crate::Project::load(&path("MAPPER_MONUMENT_TEST_RECIPE")).unwrap();
        let catalog = crate::prefab_assets::load(
            &root, &path("MAPPER_MODEL_TEST_CATALOG"), project.installation.as_ref().unwrap(), || false,
        ).unwrap();
        let bindings = Bindings::load_manifest(
            &root, &path("MAPPER_MODEL_TEST_BINDINGS"), &catalog, || false,
        ).unwrap();
        let _verification = crate::installation::VerificationScope::default();
        let recipe = project.monument_rules.clone().unwrap();
        let load = |recipe: &Recipe| Source::cached_shared(recipe, &catalog, &bindings, || false).unwrap();
        let first = {
            let _job = Scope::default();
            let first = load(&recipe);
            let nested = Scope::default();
            assert!(Arc::ptr_eq(&first, &load(&recipe)));
            assert!(Source::cached_shared(&recipe, &catalog, &bindings, || true).is_err());
            drop(nested);
            assert!(Arc::ptr_eq(&first, &load(&recipe)));
            first
        };
        let _next_job = Scope::default();
        let next = load(&recipe);
        assert!(!Arc::ptr_eq(&first, &next));
        assert_eq!(first.sha256, next.sha256);

        let mut changed = recipe.clone();
        assert!(changed.dungeon_source.take().is_some());
        let different = load(&changed);
        assert!(different.dungeon_source.is_none());
        assert!(!Arc::ptr_eq(&next, &different));

        let iterations = 12;
        let started = std::time::Instant::now();
        for _ in 0..iterations {
            assert_eq!(Source::cached(&recipe, &catalog, &bindings, || false).unwrap().sha256, first.sha256);
        }
        let uncached = started.elapsed();
        let shared = load(&recipe);
        let started = std::time::Instant::now();
        for _ in 0..iterations {
            assert!(Arc::ptr_eq(&shared, &load(&recipe)));
        }
        eprintln!("{iterations} source reads with existing disk caches: {uncached:?}; shared: {:?}", started.elapsed());
        drop(_next_job);

        let mut prepared = Prepared::verified(
            Source::cached(&recipe, &catalog, &bindings, || false).unwrap(),
            &catalog,
            &bindings,
        );
        let started = std::time::Instant::now();
        for _ in 0..iterations {
            let _job = Scope::with_prepared(Some(&recipe), &catalog, &bindings, Some(&prepared));
            assert!(Arc::ptr_eq(prepared.source(), &load(&recipe)));
        }
        eprintln!("{iterations} new jobs with an installation snapshot: {:?}", started.elapsed());
        {
            let _job = Scope::with_prepared(Some(&changed), &catalog, &bindings, Some(&prepared));
            let result = load(&changed);
            assert!(!Arc::ptr_eq(prepared.source(), &result));
            assert!(result.dungeon_source.is_none());
        }
        prepared.root.push("different-installation");
        let different_root = Scope::with_prepared(Some(&recipe), &catalog, &bindings, Some(&prepared));
        assert!(!Arc::ptr_eq(prepared.source(), &load(&recipe)));
        drop(different_root);

        // Exercise the actual CLI handoff from recipe resolution to composition.
        // Other feature defaults are independent of the monument snapshot.
        project.roads = None;
        project.rails = None;
        project.objects = None;
        let _job = Scope::default();
        let resolved = crate::generation_defaults::resolve(&project, &root, &catalog, &bindings, || false)
            .unwrap();
        let resolved_recipe = resolved.monument_rules.unwrap();
        let retained = CURRENT.with(|current| {
            let operation = current.borrow().upgrade().unwrap();
            let entry = operation.borrow();
            let entry = entry.as_ref().expect("Recipe resolution should retain its verified source");
            assert_eq!(entry.recipe, resolved_recipe);
            entry.source.clone()
        });
        assert!(Arc::ptr_eq(&retained, &load(&resolved_recipe)));
    }
    #[test]
    fn source_priority_defaults_preserve_authored_values_and_both_bound_shapes() {
        for raw in [
            include_str!("../../../compatibility/monuments/client-0f703d43-priority.json"),
            include_str!("../../../compatibility/monuments/client-8dd4e235-priority.json"),
            include_str!("../../../compatibility/monuments/client-58f3263f-priority.json"),
        ] {
            let profile: serde_json::Value = serde_json::from_str(raw).unwrap();
            let (mut catalog, doc) = fixture();
            let mut source =
                Source::decode(&serde_json::to_vec(&doc).unwrap(), &catalog, &[], || false).unwrap();
            source.code_sources = serde_json::from_value(profile["code_sources"].clone()).unwrap();
            catalog.installation.native.sha256 = profile["native_sha256"].as_str().unwrap().into();
            let mut project = crate::Project {
                installation: Some(catalog.installation.clone()),
                monument_rules: Some(source.recipe_with_source_defaults(&catalog).unwrap()),
                ..crate::Project::default()
            };
            assert_eq!(
                project
                    .monument_rules
                    .as_ref()
                    .unwrap()
                    .surface_modifiers
                    .as_deref(),
                Some(SOURCE_PRIORITY_VERSION)
            );
            project.monument_rules.as_ref().unwrap().validate().unwrap();
            let mut asset = catalog.entries["test.prefab"].clone();
            asset.modifier_absent = true;
            asset.bounds = Some(crate::prefab_assets::Bounds {
                center: [10., 0., 20.],
                extent: [30., 0., 40.],
                minimum_world_size: 0,
            });
            let (bounds, priority) =
                crate::placement_preview::bounds_for_recipe(&project, &asset).unwrap();
            assert_eq!(priority, 2);
            assert_eq!(bounds[0].center, [10., 20.]);
            asset.priority = Some(4);
            assert_eq!(
                crate::placement_preview::bounds_for_recipe(&project, &asset)
                    .unwrap()
                    .1,
                4
            );
            asset.priority = None;
            asset.modifier_absent = false;
            asset.modifier_size = Some([100., 100., 100.]);
            asset.modifier_offset = Some([0.; 3]);
            asset.modifier = Some(crate::prefab_assets::Modifier {
                height_map: true,
                water_map: Some(false),
                alpha_map: false,
                biome_mask: 0,
                topology_mask: 0,
                radius: 50.,
                fade: 0.,
                auto_cliff_splat: false,
                auto_cliff_topology: false,
                remove_existing_topology: false,
            });
            let (bounds, priority) =
                crate::placement_preview::bounds_for_recipe(&project, &asset).unwrap();
            assert_eq!(priority, 2);
            assert_eq!(bounds[1].extent, [50.; 2]);
            assert!(crate::placement_preview::source_bounds(&asset).is_err());
            project.monument_rules.as_mut().unwrap().surface_modifiers =
                Some(SURFACE_MODIFIER_VERSION.into());
            assert!(crate::placement_preview::bounds_for_recipe(&project, &asset).is_err());
            project.monument_rules.as_mut().unwrap().surface_modifiers =
                Some(SOURCE_PRIORITY_VERSION.into());
            project.installation.as_mut().unwrap().native.sha256 = "0".repeat(64);
            assert!(crate::placement_preview::bounds_for_recipe(&project, &asset).is_err());
        }
    }
    fn fixture() -> (Catalog, serde_json::Value) {
        let receipt = SourceFile {
            relative_path: "source.bundle".into(),
            sha256: "a".repeat(64),
            bytes: 1,
        };
        let catalog = Catalog {
            sha256: "b".repeat(64),
            installation: Installation {
                kind: InstallationKind::Client,
                native: receipt.clone(),
                scene_manifest: None,
            },
            sources: vec![receipt],
            entries: [(
                "test.prefab".into(),
                Asset {
                    priority: None,
                    bounds: None,
                    yaw: None,
                    modifier_size: None,
                    modifier_offset: None,
                    texture_guids: BTreeMap::new(),
                    splat_mask: None,
                    modifier: None,
                    modifier_absent: false,
                    water_geometry: None,
                    no_build_geometry: None,
                    display_name: None,
                },
            )]
            .into(),
        };
        let doc = serde_json::json!({
            "format":"papyrus-monument-source", "schema":1, "adapter":"papyrus-monument-unit-yaw-1",
            "catalog_sha256":catalog.sha256, "sources":catalog.sources, "extra_sources":[],
            "extractor_sha256":"c".repeat(64), "profile_sha256":"d".repeat(64), "models_requested":true,
            "prefabs":{"test.prefab":{
                "rules":{"tier_mask":1,"min_corners":3,"anchors":[],"filters":[],"unsupported":[],"pending":[]},
                "model":{"vertices":[[0,0,0],[1,0,0],[0,1,0]],"triangles":[[0,1,2]],"issues":[],"complete":true}
            }}
        });
        (catalog, doc)
    }
    #[test]
    fn environment_mesh_coverage_requires_explicit_capture_for_every_prefab() {
        let (catalog, doc) = fixture();
        let mut source =
            Source::decode(&serde_json::to_vec(&doc).unwrap(), &catalog, &[], || false).unwrap();
        assert!(!crate::monument_extract::has_environment_meshes(&source));
        source.prefabs.get_mut("test.prefab").unwrap().placement_attributes = Some(
            serde_json::from_value(serde_json::json!({
                "version":3,"root_decor":[],"terrain_checks":[],"water_checks":[],
                "path_connectors":[],"placement_blockers":[],"issues":[],
                "environment":{"root_transform":{"position":[0,0,0],"rotation":[0,0,0,1],"scale":[1,1,1]},
                    "volumes":[],"checks":[],"issues":[]}
            })).unwrap()
        );
        assert!(!crate::monument_extract::has_environment_meshes(&source));
        source
            .prefabs
            .get_mut("test.prefab")
            .unwrap()
            .placement_attributes
            .as_mut()
            .unwrap()
            .environment
            .as_mut()
            .unwrap()
            .collider_meshes = Some(vec![]);
        assert!(crate::monument_extract::has_environment_meshes(&source));
        source.prefabs.clear();
        assert!(!crate::monument_extract::has_environment_meshes(&source));
    }
    #[test]
    #[ignore = "requires owned installation and matching numeric/collision captures"]
    fn owned_rules_discovery_keeps_numeric_binding_after_collision_capture() {
        let path = |key| std::path::PathBuf::from(std::env::var_os(key).expect(key));
        let root = path("MAPPER_MODEL_TEST_ROOT");
        let _verification = crate::installation::VerificationScope::default();
        let installation = crate::installation::inspect(&root, || false).unwrap();
        let catalog = crate::prefab_assets::load(
            &root,
            &path("MAPPER_MODEL_TEST_CATALOG"),
            &installation,
            || false,
        )
        .unwrap();
        let bindings = Bindings::load(&root, &path("MAPPER_MODEL_TEST_BINDINGS"), &catalog, || {
            false
        })
        .unwrap();
        let cache = tempfile::tempdir().unwrap();
        let numeric = cache.path().join("numeric/monuments.json");
        let collision = cache.path().join("collision/monuments.json");
        for (file, input, seconds) in [
            (&numeric, "MAPPER_MODEL_TEST_RULES", 1000),
            (&collision, "MAPPER_MODEL_TEST_COLLISION", 2000),
        ] {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::copy(path(input), file).unwrap();
            std::fs::OpenOptions::new()
                .write(true)
                .open(file)
                .unwrap()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds))
                .unwrap();
        }
        let rules = Source::load(&numeric, &catalog, &bindings, || false).unwrap();
        let geometry = Source::load(&collision, &catalog, &bindings, || false).unwrap();
        assert!(!rules.models_requested && !geometry.models_requested);
        assert!(!crate::monument_extract::has_environment_meshes(&rules));
        assert!(crate::monument_extract::has_environment_meshes(&geometry));
        let discover = || {
            Source::discover_matching_in(
                cache.path(),
                &catalog,
                &bindings,
                Some(false),
                None,
                None,
                || false,
            )
            .unwrap()
            .unwrap()
        };
        assert_eq!(discover().sha256, rules.sha256);
        std::fs::remove_file(numeric).unwrap();
        assert_eq!(discover().sha256, geometry.sha256);
        assert!(
            Source::discover_matching_in(
                cache.path(),
                &catalog,
                &bindings,
                Some(false),
                None,
                None,
                || true
            )
            .unwrap()
            .is_none()
        );
    }
    #[test]
    fn gameplay_bound_snapshots_require_complete_matching_code_layouts() {
        let (catalog, legacy) = fixture();
        let decode = |doc: &serde_json::Value| {
            Source::decode(&serde_json::to_vec(doc).unwrap(), &catalog, &[], || false)
        };
        assert!(decode(&legacy).unwrap().code_sources.is_empty());
        let mut current = legacy;
        current["schema"] = 2.into();
        current["code_sources"] = serde_json::json!([
            {"relative_path":"GameAssembly.dll", "bytes":4, "sha256":"a".repeat(64)},
            {"relative_path":"RustClient_Data/il2cpp_data/Metadata/global-metadata.dat", "bytes":4, "sha256":"b".repeat(64)}
        ]);
        let source = decode(&current).unwrap();
        assert_eq!(source.code_sources.len(), 2);
        assert!(source.coastal_profile.is_none());
        for (pointer, value) in [
            ("/schema", serde_json::json!(1)),
            ("/schema", serde_json::json!(3)),
            ("/code_sources", serde_json::json!([])),
            (
                "/code_sources/0/relative_path",
                serde_json::json!("../GameAssembly.dll"),
            ),
            (
                "/code_sources/1/relative_path",
                serde_json::json!("GameAssembly.dll"),
            ),
            (
                "/code_sources/1/relative_path",
                serde_json::json!("RustDedicated_Data/Managed/Assembly-CSharp.dll"),
            ),
            ("/code_sources/0/bytes", serde_json::json!(0)),
            (
                "/code_sources/0/bytes",
                serde_json::json!(1024_u64.pow(3) + 1),
            ),
            ("/code_sources/0/sha256", serde_json::json!("A".repeat(64))),
        ] {
            let mut bad = current.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(decode(&bad).is_err(), "{pointer}");
        }
        current["code_sources"].as_array_mut().unwrap().pop();
        assert!(decode(&current).is_err());
        current["code_sources"][0]["relative_path"] =
            "RustClient_Data/Managed/Assembly-CSharp.dll".into();
        let client_mono = decode(&current).unwrap();
        assert!(
            validate_code_sources(
                2,
                &client_mono.code_sources,
                InstallationKind::DedicatedServer
            )
            .is_err()
        );
        current["code_sources"][0]["relative_path"] =
            "RustDedicated_Data/Managed/Assembly-CSharp.dll".into();
        assert!(decode(&current).is_err());
        let server: Vec<SourceFile> =
            serde_json::from_value(current["code_sources"].clone()).unwrap();
        validate_code_sources(2, &server, InstallationKind::DedicatedServer).unwrap();
    }

    #[test]
    fn code_verification_detects_same_length_replacements_missing_files_and_cancellation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("GameAssembly.dll");
        std::fs::write(&path, b"code").unwrap();
        let receipt =
            crate::installation::hash_file(root.path(), "GameAssembly.dll", 1024, &|| false)
                .unwrap();
        let sources = [receipt];
        verify_code_sources(root.path(), &sources, &|| false).unwrap();
        std::fs::write(&path, b"edit").unwrap();
        assert!(verify_code_sources(root.path(), &sources, &|| false).is_err());
        std::fs::write(&path, b"code").unwrap();
        verify_code_sources(root.path(), &sources, &|| false).unwrap();
        assert!(verify_code_sources(root.path(), &sources, &|| true).is_err());
        std::fs::remove_file(path).unwrap();
        assert!(verify_code_sources(root.path(), &sources, &|| false).is_err());
    }
    #[test]
    fn optional_models_match_exact_placement_data_without_sharing_recipe_identity() {
        let (catalog, mut doc) = fixture();
        doc["models_requested"] = false.into();
        doc["prefabs"]["test.prefab"]["model"] =
            serde_json::json!({"vertices":[],"triangles":[],"issues":[],"complete":false});
        doc["prefabs"]["test.prefab"]["placement_attributes"] = serde_json::json!({"version":1,"root_decor":[],"terrain_checks":[],"water_checks":[],"path_connectors":[],"issues":[]});
        let rules =
            Source::decode(&serde_json::to_vec(&doc).unwrap(), &catalog, &[], || false).unwrap();
        doc["models_requested"] = true.into();
        let models =
            Source::decode(&serde_json::to_vec(&doc).unwrap(), &catalog, &[], || false).unwrap();
        assert_ne!(rules.sha256, models.sha256);
        assert!(rules.matches_placement_data(&models));
        doc["prefabs"]["test.prefab"]["rules"]["tier_mask"] = 2.into();
        let changed =
            Source::decode(&serde_json::to_vec(&doc).unwrap(), &catalog, &[], || false).unwrap();
        assert!(!rules.matches_placement_data(&changed));
        doc["prefabs"]["test.prefab"]["rules"]["tier_mask"] = 1.into();
        doc["prefabs"]["test.prefab"]["placement_attributes"]["path_connectors"] = serde_json::json!([{"object_id":42,"active":true,"position":[1.,2.,3.],"infrastructure_type":0}]);
        let changed =
            Source::decode(&serde_json::to_vec(&doc).unwrap(), &catalog, &[], || false).unwrap();
        assert!(!rules.matches_placement_data(&changed));
    }
    #[test]
    #[ignore = "requires owned installation, catalog, bindings and matching rule/model captures"]
    fn owned_model_discovery_skips_newer_incompatible_captures() {
        let path = |name| std::path::PathBuf::from(std::env::var_os(name).expect(name));
        let root = path("MAPPER_MODEL_TEST_ROOT");
        let _leases = crate::source_lease::Scope::default();
        let _verification = crate::installation::VerificationScope::default();
        let installation = crate::installation::inspect(&root, || false).unwrap();
        let catalog = crate::prefab_assets::load(
            &root,
            &path("MAPPER_MODEL_TEST_CATALOG"),
            &installation,
            || false,
        )
        .unwrap();
        let bindings = Bindings::load(&root, &path("MAPPER_MODEL_TEST_BINDINGS"), &catalog, || {
            false
        })
        .unwrap();
        let rules = Source::load(
            &path("MAPPER_MODEL_TEST_RULES"),
            &catalog,
            &bindings,
            || false,
        )
        .unwrap();
        let raw = std::fs::read(path("MAPPER_MODEL_TEST_MODELS")).unwrap();
        let models = Source::decode(&raw, &catalog, bindings.source_receipts(), || false).unwrap();
        assert!(models.models_requested);
        assert!(rules.matches_placement_data(&models));
        let extractor = crate::metadata_runtime::monument_extractor_sha256();
        let approved = crate::metadata_runtime::monument_cache_extractors(extractor.clone());
        assert!(approved.contains(&models.extractor_sha256));
        assert!(approved.contains(&rules.extractor_sha256));
        let cache = tempfile::tempdir().unwrap();
        let matching = cache.path().join("a-matching");
        let incompatible = cache.path().join("z-newer");
        std::fs::create_dir(&matching).unwrap();
        std::fs::create_dir(&incompatible).unwrap();
        std::fs::write(matching.join("monuments.json"), &raw).unwrap();
        let mut altered: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        let prefab = altered["prefabs"]
            .as_object_mut()
            .unwrap()
            .values_mut()
            .next()
            .unwrap();
        let tier = prefab["rules"]["tier_mask"].as_u64().unwrap();
        prefab["rules"]["tier_mask"] = serde_json::json!(tier ^ (1 << 26));
        let altered = serde_json::to_vec(&altered).unwrap();
        std::fs::write(incompatible.join("monuments.json"), &altered).unwrap();
        let unfiltered = Source::discover_matching_in(
            cache.path(),
            &catalog,
            &bindings,
            Some(true),
            None,
            None,
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(!rules.matches_placement_data(&unfiltered));
        let found = Source::discover_matching_in(
            cache.path(),
            &catalog,
            &bindings,
            Some(true),
            Some(&rules),
            None,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(found.sha256, models.sha256);
        std::fs::remove_file(matching.join("monuments.json")).unwrap();
        assert!(
            Source::discover_matching_in(
                cache.path(),
                &catalog,
                &bindings,
                Some(true),
                Some(&rules),
                None,
                || false,
            )
            .unwrap()
            .is_none()
        );
        assert!(
            Source::discover_matching_in(
                cache.path(),
                &catalog,
                &bindings,
                Some(true),
                Some(&rules),
                None,
                || true,
            )
            .unwrap()
            .is_none()
        );
        let numeric = std::fs::read(path("MAPPER_MODEL_TEST_RULES")).unwrap();
        std::fs::write(matching.join("monuments.json"), &numeric).unwrap();
        let mut stale: serde_json::Value = serde_json::from_slice(&numeric).unwrap();
        stale["extractor_sha256"] = serde_json::json!("f".repeat(64));
        std::fs::write(
            incompatible.join("monuments.json"),
            serde_json::to_vec(&stale).unwrap(),
        )
        .unwrap();
        let historical = Source::load(
            &incompatible.join("monuments.json"),
            &catalog,
            &bindings,
            || false,
        )
        .unwrap();
        let saved_binding = historical.recipe();
        let fresh = Source::discover_matching_in(
            cache.path(),
            &catalog,
            &bindings,
            Some(false),
            None,
            Some((&extractor, &rules.profile_sha256)),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(fresh.sha256, rules.sha256);
        std::fs::remove_file(matching.join("monuments.json")).unwrap();
        assert!(
            Source::discover_matching_in(
                cache.path(),
                &catalog,
                &bindings,
                Some(false),
                None,
                Some((&extractor, &rules.profile_sha256)),
                || false,
            )
            .unwrap()
            .is_none()
        );
        let retained = Source::load(
            &incompatible.join("monuments.json"),
            &catalog,
            &bindings,
            || false,
        )
        .unwrap();
        assert_eq!(retained.recipe(), saved_binding);
        let fallback = Source::discover_matching_in(
            cache.path(),
            &catalog,
            &bindings,
            Some(false),
            None,
            None,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(fallback.sha256, retained.sha256);
        stale["extractor_sha256"] = serde_json::json!(extractor);
        stale["profile_sha256"] = serde_json::json!("e".repeat(64));
        std::fs::write(
            incompatible.join("monuments.json"),
            serde_json::to_vec(&stale).unwrap(),
        )
        .unwrap();
        assert!(
            Source::discover_matching_in(
                cache.path(),
                &catalog,
                &bindings,
                Some(false),
                None,
                Some((&extractor, &rules.profile_sha256)),
                || false,
            )
            .unwrap()
            .is_none()
        );
        let previous_profile = Source::load(
            &incompatible.join("monuments.json"),
            &catalog,
            &bindings,
            || false,
        )
        .unwrap();
        assert_eq!(previous_profile.extractor_sha256, extractor);
        assert_ne!(previous_profile.profile_sha256, rules.profile_sha256);
        assert!(previous_profile.matches_placement_data(&rules));
    }
    #[test]
    fn surface_modifier_policy_defaults_on_new_bindings_and_preserves_legacy_omission() {
        let (catalog, doc) = fixture();
        let source =
            Source::decode(&serde_json::to_vec(&doc).unwrap(), &catalog, &[], || false).unwrap();
        let recipe = source.recipe();
        assert_eq!(
            recipe.surface_modifiers.as_deref(),
            Some(SURFACE_MODIFIER_VERSION)
        );
        recipe.validate().unwrap();
        assert!(recipe.permits_prefab_only());
        let mut surface_only = recipe.clone();
        surface_only.surface_modifiers = Some(LEGACY_SURFACE_MODIFIER_VERSION.into());
        surface_only.validate().unwrap();
        assert!(!surface_only.permits_prefab_only());
        let mut legacy = serde_json::to_value(&recipe).unwrap();
        legacy.as_object_mut().unwrap().remove("surface_modifiers");
        let old: Recipe = serde_json::from_value(legacy).unwrap();
        old.validate().unwrap();
        assert!(old.surface_modifiers.is_none());
        assert!(!old.permits_prefab_only());
        assert!(
            serde_json::to_value(&old)
                .unwrap()
                .get("surface_modifiers")
                .is_none()
        );
        let mut unsupported = recipe;
        unsupported.surface_modifiers = Some("future-surface-modifiers".into());
        assert!(unsupported.validate().is_err());
    }
    #[test]
    fn modifier_free_placement_preserves_authored_bounds_and_validates_pose() {
        use crate::{Project, composition, placement_preview, prefab_assets::Bounds};
        use papyrus_terrain::placement::Position;
        let (catalog, doc) = fixture();
        let source =
            Source::decode(&serde_json::to_vec(&doc).unwrap(), &catalog, &[], || false).unwrap();
        let mut project = Project {
            monument_rules: Some(source.recipe()),
            ..Project::default()
        };
        let mut asset = catalog.entries["test.prefab"].clone();
        asset.modifier_absent = true;
        asset.priority = Some(2);
        asset.bounds = Some(Bounds {
            center: [0.; 3],
            extent: [0.; 3],
            minimum_world_size: 0,
        });
        let pose = Position {
            x: 0.,
            y: 0.,
            z: 0.,
            yaw: 37.,
            clearance: 1.,
        };
        let (bounds, priority) = placement_preview::bounds_for_recipe(&project, &asset).unwrap();
        assert_eq!(priority, 2);
        assert_eq!(bounds[0].extent, [0.; 2]);
        assert!(composition::omit_terrain_modifier(&project, &asset, &pose).unwrap());
        for field in 0..5 {
            let mut invalid = pose.clone();
            match field {
                0 => invalid.x = f32::NAN,
                1 => invalid.y = f32::INFINITY,
                2 => invalid.z = project.size as f32,
                3 => invalid.yaw = f32::NAN,
                _ => invalid.clearance = -1.,
            }
            assert!(composition::omit_terrain_modifier(&project, &asset, &invalid).is_err());
        }
        asset.bounds.as_mut().unwrap().center = [10., 0., -20.];
        asset.bounds.as_mut().unwrap().extent = [30., 2., 40.];
        let (bounds, _) = placement_preview::bounds_for_recipe(&project, &asset).unwrap();
        assert_eq!(bounds[0].center, [10., -20.]);
        assert_eq!(bounds[0].extent, [30., 40.]);
        assert_eq!(bounds[0].center, bounds[1].center);
        assert_eq!(bounds[0].extent, bounds[1].extent);
        asset.bounds.as_mut().unwrap().minimum_world_size = project.size + 1;
        assert!(placement_preview::bounds_for_recipe(&project, &asset).is_err());
        asset.bounds.as_mut().unwrap().minimum_world_size = 0;
        asset.priority = None;
        assert!(placement_preview::bounds_for_recipe(&project, &asset).is_err());
        asset.priority = Some(2);
        asset.modifier_absent = false;
        assert!(placement_preview::bounds_for_recipe(&project, &asset).is_err());
        assert!(!composition::omit_terrain_modifier(&project, &asset, &pose).unwrap());
        asset.modifier_absent = true;
        for policy in [None, Some(LEGACY_SURFACE_MODIFIER_VERSION.into())] {
            project.monument_rules.as_mut().unwrap().surface_modifiers = policy;
            assert!(placement_preview::bounds_for_recipe(&project, &asset).is_err());
            assert!(!composition::omit_terrain_modifier(&project, &asset, &pose).unwrap());
        }
    }
    #[test]
    fn angle_policy_is_explicit_serialized_and_idempotent_including_rounding_edges() {
        let (catalog, doc) = fixture();
        let source =
            Source::decode(&serde_json::to_vec(&doc).unwrap(), &catalog, &[], || false).unwrap();
        let current = source.recipe();
        current.validate().unwrap();
        let saved = serde_json::to_value(&current).unwrap();
        assert_eq!(saved["canonical_yaw"], CANONICAL_YAW_VERSION);
        let restored: Recipe = serde_json::from_value(saved.clone()).unwrap();
        assert_eq!(current, restored);
        for yaw in [
            -f32::MAX,
            -1080.,
            -359.99,
            -133.1734,
            -f32::MIN_POSITIVE,
            -0.,
            0.,
            f32::MIN_POSITIVE,
            133.1734,
            360.,
            1080.,
            f32::MAX,
        ] {
            let angle = current.placement_yaw(yaw);
            assert!((0. ..360.).contains(&angle), "{yaw} => {angle}");
            assert_eq!(angle.to_bits(), current.placement_yaw(angle).to_bits());
        }
        let mut old = saved;
        old.as_object_mut().unwrap().remove("canonical_yaw");
        let legacy: Recipe = serde_json::from_value(old.clone()).unwrap();
        legacy.validate().unwrap();
        assert_eq!(legacy.placement_yaw(-133.1734), -133.1734);
        assert_eq!(serde_json::to_value(legacy).unwrap(), old);
        let mut invalid = current;
        invalid.canonical_yaw = Some("unknown-angle-policy".into());
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn canonical_checks_reproduce_saved_rotation_anchor_heights_bit_for_bit() {
        use papyrus_terrain::{
            monument_bounds::Footprint,
            monument_eligibility::{Anchor, Check, Evaluator},
            native_layers::{Layers, dimensions},
        };
        let (catalog, doc) = fixture();
        let source =
            Source::decode(&serde_json::to_vec(&doc).unwrap(), &catalog, &[], || false).unwrap();
        let recipe = source.recipe();
        let mut rules = source.prefabs["test.prefab"].rules.clone();
        rules.anchors.push(Anchor {
            position: [43.17, 7., 13.89],
            extents: 128.,
            offset: 0.,
            radius: 0.,
            slope_scale: 0.,
        });
        let (hr, n) = dimensions(1000).unwrap();
        let layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 1,
            height_resolution: hr,
            surface_resolution: n,
            height: (0..hr * hr)
                .map(|i| (16000 + (i % hr) * 2 + i / hr) as i16)
                .collect(),
            topology: vec![1; n * n],
            splat: vec![255; n * n * 8],
            biome: vec![255; n * n * 5],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        let evaluator = Evaluator::new(
            &layers,
            &rules,
            Footprint {
                center: [0.; 2],
                extent: [20.; 2],
            },
            || false,
        )
        .unwrap()
        .unwrap();
        let run = |yaw| {
            evaluator
                .evaluate([13.2, 60., -32.75], yaw, &mut 0, 100, || false)
                .unwrap()
                .unwrap()
        };
        let mut old_differences = 0;
        for i in 1..360 {
            let raw = -(i as f32 + 0.1734);
            let saved = recipe.placement_yaw(raw);
            let first = run(saved);
            assert!(matches!(first, Check::Accepted(_)));
            assert_eq!(first, run(recipe.placement_yaw(saved)));
            old_differences += usize::from(run(raw) != run(saved));
        }
        assert!(
            old_differences > 0,
            "fixture must expose the original anchor drift"
        );
    }

    #[test]
    fn source_identity_geometry_and_cancellation_are_enforced() {
        let (catalog, doc) = fixture();
        let decode = |d: &serde_json::Value| {
            Source::decode(&serde_json::to_vec(d).unwrap(), &catalog, &[], || false)
        };
        let first = decode(&doc).unwrap();
        assert!(first.models_requested);
        first.recipe().validate().unwrap();
        assert_eq!(first.sha256, decode(&doc).unwrap().sha256);
        assert!(
            Source::decode(&serde_json::to_vec(&doc).unwrap(), &catalog, &[], || true).is_err()
        );
        for (pointer, value) in [
            ("/catalog_sha256", serde_json::json!("e".repeat(64))),
            ("/sources/0/sha256", serde_json::json!("f".repeat(64))),
            (
                "/prefabs/test.prefab/model/triangles/0/2",
                serde_json::json!(3),
            ),
            (
                "/prefabs/test.prefab/model/vertices/0/0",
                serde_json::json!(10001),
            ),
            (
                "/prefabs/test.prefab/rules/min_corners",
                serde_json::json!(5),
            ),
            (
                "/prefabs/test.prefab/model/issues",
                serde_json::json!(["missing mesh"]),
            ),
        ] {
            let mut changed = doc.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(decode(&changed).is_err(), "accepted {pointer}");
        }
        let mut changed = doc.clone();
        changed["extra_sources"] = serde_json::json!([catalog.sources[0]]);
        assert!(decode(&changed).is_err());
        assert!(
            Source::decode(
                &serde_json::to_vec(&changed).unwrap(),
                &catalog,
                &catalog.sources,
                || false
            )
            .is_ok()
        );
        let mut changed = doc;
        changed["prefabs"]["test.prefab"]["model"]["vertices"][0][0] = 0.5.into();
        assert_ne!(first.sha256, decode(&changed).unwrap().sha256);
    }
    #[test]
    fn captured_placement_attributes_are_validated_without_changing_the_execution_engine() {
        let (catalog, mut doc) = fixture();
        doc["prefabs"]["test.prefab"]["placement_attributes"] = serde_json::json!({"version":1,"root_decor":[{"object_id":1,"active":true,"kind":"DecorSwim","values":[]}],"terrain_checks":[],"water_checks":[],"path_connectors":[{"object_id":2,"active":false,"position":[0,0,200],"infrastructure_type":5}],"issues":[]});
        let load = |doc: &serde_json::Value| {
            Source::decode(&serde_json::to_vec(doc).unwrap(), &catalog, &[], || false)
        };
        let source = load(&doc).unwrap();
        assert_eq!(source.recipe().engine, VERSION);
        assert_eq!(source.audit(true)["prefabs_with_placement_attributes"], 1);
        assert_eq!(source.audit(true)["captured_path_connectors"], 1);
        assert_eq!(
            source.audit(true)["entries"][0]["placement_attributes"]["executed"],
            false
        );
        doc["prefabs"]["test.prefab"]["placement_attributes"]["path_connectors"][0]["object_id"] =
            1.into();
        assert!(load(&doc).is_err());
    }
    #[test]
    fn inspection_separates_source_coverage_from_complete_world_support() {
        let (catalog, mut doc) = fixture();
        doc["prefabs"]["test.prefab"]["rules"]["pending"] = serde_json::json!(["occupancy"]);
        let source =
            Source::decode(&serde_json::to_vec(&doc).unwrap(), &catalog, &[], || false).unwrap();
        let summary = source.audit(false);
        assert_eq!(summary["prefabs"], 1);
        assert_eq!(summary["nonempty_model_previews"], 1);
        assert_eq!(summary["complete_static_model_previews"], 1);
        assert_eq!(summary["prefabs_with_pending_checks"], 1);
        assert_eq!(summary["prefab_assembly_complete"], false);
        assert_eq!(summary["physics_validated"], false);
        assert!(summary.get("entries").is_none());
        let detailed = source.audit(true);
        assert_eq!(detailed["entries"][0]["prefab"], "test.prefab");
        assert_eq!(
            detailed["entries"][0]["pending"],
            serde_json::json!(["occupancy"])
        );
        assert_eq!(detailed["entries"][0]["minimum_matching_corners"], 3);
        assert_eq!(detailed["entries"][0]["model"]["triangles"], 1);
        assert_eq!(detailed["entries"][0]["eligibility"]["min_corners"], 3);
        assert_eq!(
            detailed["entries"][0]["model"]["local_bounds"]["min"],
            serde_json::json!([0., 0., 0.])
        );
        assert_eq!(
            detailed["entries"][0]["model"]["local_bounds"]["max"],
            serde_json::json!([1., 1., 0.])
        );
        assert_eq!(detailed, source.audit(true));
        // Unused mesh vertices must not inflate the visible preview bounds.
        let mut with_unused = doc.clone();
        with_unused["prefabs"]["test.prefab"]["model"]["vertices"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!([999., 999., 999.]));
        let source = Source::decode(
            &serde_json::to_vec(&with_unused).unwrap(),
            &catalog,
            &[],
            || false,
        )
        .unwrap();
        assert_eq!(
            source.audit(true)["entries"][0]["model"]["local_bounds"],
            detailed["entries"][0]["model"]["local_bounds"]
        );
        with_unused["prefabs"]["test.prefab"]["model"]["triangles"] = serde_json::json!([]);
        with_unused["prefabs"]["test.prefab"]["model"]["complete"] = false.into();
        let source = Source::decode(
            &serde_json::to_vec(&with_unused).unwrap(),
            &catalog,
            &[],
            || false,
        )
        .unwrap();
        assert!(source.audit(true)["entries"][0]["model"]["local_bounds"].is_null());
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub engine: String,
    pub source_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coastal_profile: Option<crate::coastal_placement::Profile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier_search: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub earthwork: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coastal_normals: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coastal_navigation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coastal_search: Option<crate::coastal_candidates::Policy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_yaw: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub land_alignment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_modifiers: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage_rules: Option<crate::monument_stages::Binding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dungeon_source: Option<crate::dungeon_source::Binding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offshore_source: Option<crate::offshore_generation::Binding>,
}
impl Recipe {
    pub fn permits_prefab_only(&self) -> bool {
        matches!(self.surface_modifiers.as_deref(), Some(SURFACE_MODIFIER_VERSION | SOURCE_PRIORITY_VERSION))
    }
    pub fn omits_terrain_stamp(&self, asset: &crate::prefab_assets::Asset) -> bool {
        asset.has_no_terrain_effects()
            && (asset.modifier.is_some() || self.permits_prefab_only())
    }
    /// Use the serialized angle for every source check and stamp operation.
    /// Older recipes retain their original floating-point sampling convention.
    pub fn placement_yaw(&self, yaw: f32) -> f32 {
        if self.canonical_yaw.is_none() {
            return yaw;
        }
        let yaw = yaw.rem_euclid(360.);
        // rem_euclid may round a tiny negative input up to the modulus.
        if yaw == 0. || yaw >= 360. { 0. } else { yaw }
    }
    pub fn validate(&self) -> Result<(), String> {
        if let Some(binding) = &self.offshore_source { binding.validate()?; }
        if self.dungeon_source.as_ref().is_some_and(|binding| binding.validate().is_err() || self.stage_rules.is_none()) {
            return Err("Dungeon generation requires a verified source and monument stages".into());
        }
        if self.coastal_navigation.as_deref().is_some_and(|p| {
            p != papyrus_terrain::boat_navigation::SOURCE_VERSION
                || self.engine != crate::coastal_placement::BUDGET_VERSION
                || self.coastal_profile.is_none()
        }) {
            return Err("Unsupported coastal navigation policy".into());
        }
        if let Some(binding) = &self.stage_rules {
            binding.validate()?;
        }
        if self
            .canonical_yaw
            .as_deref()
            .is_some_and(|p| p != CANONICAL_YAW_VERSION)
        {
            return Err("Unsupported monument angle policy".into());
        }
        if self.surface_modifiers.as_deref().is_some_and(|policy| {
            !matches!(
                policy,
                SURFACE_MODIFIER_VERSION | LEGACY_SURFACE_MODIFIER_VERSION | SOURCE_PRIORITY_VERSION
            )
        }) {
            return Err("Unsupported monument surface modifier policy".into());
        }
        if self.land_alignment.as_deref().is_some_and(|policy| {
            policy != crate::monument_alignment::VERSION || self.stage_rules.is_none()
        }) {
            return Err("Land alignment requires verified monument stages".into());
        }
        if let Some(policy) = &self.coastal_search {
            policy.validate()?;
            if self.engine != crate::coastal_placement::BUDGET_VERSION
                || self.coastal_profile.is_none()
                || self.coastal_normals.is_none()
            {
                return Err("Coastal proposals require source-normal coastal placement".into());
            }
        }
        if self.coastal_normals.as_ref().is_some_and(|p| {
            p != papyrus_terrain::terrain_normal::VERSION
                || self.engine != crate::coastal_placement::BUDGET_VERSION
                || self.coastal_profile.is_none()
        }) {
            return Err("Unsupported coastal terrain normal policy".into());
        }
        if self.earthwork.as_ref().is_some_and(|p| {
            p != AUTHORED_EARTHWORK_VERSION
                || self.engine != crate::coastal_placement::BUDGET_VERSION
        }) {
            return Err("Unsupported authored earthwork policy".into());
        }
        if self.tier_search.as_ref().is_some_and(|p| {
            p != TIER_RETRY_VERSION || self.engine != crate::coastal_placement::BUDGET_VERSION
        }) {
            return Err("Unsupported monument tier retry policy".into());
        }
        if !supported(&self.engine)
            || !hash(&self.source_sha256)
            || crate::coastal_placement::supported(&self.engine) != self.coastal_profile.is_some()
        {
            return Err("Invalid monument source recipe".into());
        }
        if let Some(profile) = &self.coastal_profile {
            profile.validate()?;
        }
        Ok(())
    }
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub vertices: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
    pub issues: Vec<String>,
    pub complete: bool,
    #[serde(default)]
    pub simplified: bool,
}
impl Model {
    /// Bounds of rendered geometry, excluding vertices unused by any triangle.
    /// These are preview bounds, never authored placement/collision bounds.
    fn local_bounds(&self) -> Option<serde_json::Value> {
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        let mut found = false;
        for i in self.triangles.iter().flatten() {
            let p = self.vertices.get(*i as usize)?;
            found = true;
            for axis in 0..3 {
                min[axis] = min[axis].min(p[axis]);
                max[axis] = max[axis].max(p[axis]);
            }
        }
        found.then(|| serde_json::json!({"min": min, "max": max}))
    }
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prefab {
    pub rules: Rules,
    pub model: Model,
    #[serde(default)]
    pub placement_attributes: Option<crate::monument_attributes::Attributes>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    format: String,
    schema: u32,
    adapter: String,
    catalog_sha256: String,
    sources: Vec<SourceFile>,
    extra_sources: Vec<SourceFile>,
    #[serde(default)]
    code_sources: Vec<SourceFile>,
    extractor_sha256: String,
    profile_sha256: String,
    #[serde(default)]
    models_requested: bool,
    prefabs: BTreeMap<String, Prefab>,
}
pub struct Source {
    pub sha256: String,
    pub extractor_sha256: String,
    pub profile_sha256: String,
    pub prefabs: BTreeMap<String, Prefab>,
    pub models_requested: bool,
    pub coastal_profile: Option<crate::coastal_placement::Profile>,
    pub stage_source: Option<crate::monument_stages::Source>,
    pub dungeon_source: Option<crate::dungeon_source::Binding>,
    pub offshore_source: Option<crate::offshore_generation::Binding>,
    pub code_sources: Vec<SourceFile>,
}

struct Retained {
    root: PathBuf,
    catalog: String,
    bindings: String,
    recipe: Recipe,
    source: Arc<Source>,
}

type Operation = RefCell<Option<Retained>>;
thread_local! {
    static CURRENT: RefCell<Weak<Operation>> = const { RefCell::new(Weak::new()) };
}

/// Owned installation data that has passed the capture and receipt checks.
pub struct Prepared {
    root: PathBuf,
    catalog: String,
    bindings: String,
    source: Arc<Source>,
}

impl Prepared {
    pub(crate) fn verified(source: Source, catalog: &Catalog, bindings: &Bindings) -> Self {
        Self {
            root: bindings.installation_root().to_owned(),
            catalog: catalog.sha256.clone(),
            bindings: bindings.sha256.clone(),
            source: Arc::new(source),
        }
    }

    pub fn source(&self) -> &Arc<Source> {
        &self.source
    }

    fn matches(&self, recipe: &Recipe, catalog: &Catalog, bindings: &Bindings) -> bool {
        let source = &self.source;
        self.root == bindings.installation_root()
            && self.catalog == catalog.sha256
            && self.bindings == bindings.sha256
            && source.sha256 == recipe.source_sha256
            && source.coastal_profile == recipe.coastal_profile
            && source.dungeon_source == recipe.dungeon_source
            && source.offshore_source == recipe.offshore_source
            && match (&source.stage_source, &recipe.stage_rules) {
                (Some(stages), Some(binding)) => {
                    stages.sha256 == binding.source_sha256
                        && stages.binding().engine == binding.engine
                }
                (None, None) => true,
                _ => false,
            }
            && recipe.validate().is_ok()
            && (recipe.surface_modifiers.as_deref() != Some(SOURCE_PRIORITY_VERSION)
                || crate::prefab_priority::supports(catalog, &source.code_sources).unwrap_or(false))
    }
}

/// Share one verified monument snapshot across the stages of a generation job.
pub struct Scope(Rc<Operation>);

impl Default for Scope {
    fn default() -> Self {
        let operation = CURRENT.with(|current| {
            if let Some(operation) = current.borrow().upgrade() {
                return operation;
            }
            let operation = Rc::new(RefCell::new(None));
            *current.borrow_mut() = Rc::downgrade(&operation);
            operation
        });
        Self(operation)
    }
}

impl Scope {
    /// Use the prepared snapshot for this worker when it matches all source pins.
    /// The worker still verifies the live Rust files when the snapshot is used.
    pub fn with_prepared(
        recipe: Option<&Recipe>,
        catalog: &Catalog,
        bindings: &Bindings,
        prepared: Option<&Prepared>,
    ) -> Self {
        let scope = Self::default();
        if let Some((prepared, recipe)) = prepared.zip(recipe)
            && prepared.matches(recipe, catalog, bindings)
        {
            scope.remember(recipe, catalog, bindings, prepared.source.clone());
        }
        scope
    }

    fn remember(&self, recipe: &Recipe, catalog: &Catalog, bindings: &Bindings, source: Arc<Source>) {
        *self.0.borrow_mut() = Some(Retained {
            root: bindings.installation_root().to_owned(),
            catalog: catalog.sha256.clone(),
            bindings: bindings.sha256.clone(),
            recipe: recipe.clone(),
            source,
        });
    }
}

fn validate_code_sources(
    schema: u32,
    sources: &[SourceFile],
    kind: crate::installation::InstallationKind,
) -> Result<(), String> {
    use crate::installation::InstallationKind;
    let paths: std::collections::BTreeSet<_> =
        sources.iter().map(|s| s.relative_path.as_str()).collect();
    let valid_layout = match kind {
        InstallationKind::Client => {
            paths == ["RustClient_Data/Managed/Assembly-CSharp.dll"].into()
                || paths
                    == [
                        "GameAssembly.dll",
                        "RustClient_Data/il2cpp_data/Metadata/global-metadata.dat",
                    ]
                    .into()
        }
        InstallationKind::DedicatedServer => {
            paths == ["RustDedicated_Data/Managed/Assembly-CSharp.dll"].into()
        }
    };
    if !((schema == 1 && sources.is_empty())
        || (schema == 2
            && valid_layout
            && paths.len() == sources.len()
            && sources
                .iter()
                .all(|s| s.bytes > 0 && s.bytes <= 1024 * 1024 * 1024 && hash(&s.sha256))))
    {
        return Err("Invalid monument gameplay receipt layout or revision".into());
    }
    Ok(())
}

fn verify_code_sources(
    root: &Path,
    sources: &[SourceFile],
    cancelled: &impl Fn() -> bool,
) -> Result<(), String> {
    for expected in sources {
        if crate::installation::hash_file(
            root,
            &expected.relative_path,
            1024 * 1024 * 1024,
            cancelled,
        )? != *expected
        {
            return Err("Monument gameplay code differs from selected installation".into());
        }
    }
    if cancelled() {
        return Err("Monument source load cancelled".into());
    }
    Ok(())
}
impl Source {
    /// Preview geometry may arrive independently, but it must describe exactly
    /// the same captured placement data. Attaching it never changes recipe IDs.
    pub fn matches_placement_data(&self, other: &Self) -> bool {
        self.prefabs.len() == other.prefabs.len()
            && self.prefabs.iter().all(|(path, prefab)| {
                other.prefabs.get(path).is_some_and(|other| {
                    serde_json::to_value((&prefab.rules, &prefab.placement_attributes))
                        .ok()
                        .zip(serde_json::to_value((&other.rules, &other.placement_attributes)).ok())
                        .is_some_and(|(a, b)| a == b)
                })
            })
    }
    /// Read-only source coverage. Counts describe extracted data, not accepted
    /// placements, assembled prefabs or physical/game validation.
    pub fn audit(&self, details: bool) -> serde_json::Value {
        use serde_json::json;
        let mut result = json!({
            "source_sha256": self.sha256,
            "extractor_sha256": self.extractor_sha256,
            "gameplay_sources": self.code_sources,
            "placement_engine": self.recipe().engine,
            "prefabs": self.prefabs.len(),
            "anchors": self.prefabs.values().map(|p| p.rules.anchors.len()).sum::<usize>(),
            "filters": self.prefabs.values().map(|p| p.rules.filters.len()).sum::<usize>(),
            "prefabs_with_unsupported_rules": self.prefabs.values().filter(|p| !p.rules.unsupported.is_empty()).count(),
            "prefabs_with_pending_checks": self.prefabs.values().filter(|p| !p.rules.pending.is_empty()).count(),
            "models_requested": self.models_requested,
            "prefabs_with_placement_attributes": self.prefabs.values().filter(|p|p.placement_attributes.is_some()).count(),
            "captured_terrain_checks":self.prefabs.values().filter_map(|p|p.placement_attributes.as_ref()).map(|a|a.terrain_checks.len()).sum::<usize>(),
            "captured_water_checks":self.prefabs.values().filter_map(|p|p.placement_attributes.as_ref()).map(|a|a.water_checks.len()).sum::<usize>(),
            "captured_path_connectors":self.prefabs.values().filter_map(|p|p.placement_attributes.as_ref()).map(|a|a.path_connectors.len()).sum::<usize>(),
            "nonempty_model_previews": self.prefabs.values().filter(|p| !p.model.triangles.is_empty()).count(),
            "complete_static_model_previews": self.prefabs.values().filter(|p| p.model.complete).count(),
            "model_triangles": self.prefabs.values().map(|p| p.model.triangles.len()).sum::<usize>(),
            "physics_validated": false,
            "prefab_assembly_complete": false,
            "scope": "Source-local anchors, filters and tier corners; untextured static inspection LODs. Stage-wide policy, placement-map occupancy and runtime assembly remain incomplete."
        });
        if let Some(stages) = &self.stage_source {
            result["stage_filters"] = json!({"source_sha256":stages.sha256,"programs":stages.program_count(),"separate_families":stages.unsupported_family_count(),"count_and_spacing_execution":false,"available_minimum_spacing_policy":crate::monument_stages::SPACING_VERSION,"minimum_spacing_scope":"symmetric OBB edge minimums; full stage counts, scoring and relaxation incomplete"});
        }
        if details {
            result["entries"] = self.prefabs.iter().map(|(path, p)| json!({
                "prefab": path,
                "tier_mask": p.rules.tier_mask,
                "minimum_matching_corners": p.rules.min_corners,
                "anchors": p.rules.anchors.len(),
                "radius_anchors": p.rules.anchors.iter().filter(|a| a.radius > 0.).count(),
                "filters": p.rules.filters.len(),
                "radius_filters": p.rules.filters.iter().filter(|f| f.radius > 0.).count(),
                "placement_map_checks": p.rules.filters.iter().filter(|f| f.check_placement).count(),
                "unsupported": p.rules.unsupported,
                "pending": p.rules.pending,
                "eligibility": p.rules,
                "placement_attributes":p.placement_attributes.as_ref().map(|a|a.audit()),
                "model": {
                    "vertices": p.model.vertices.len(), "triangles": p.model.triangles.len(),
                    "complete_static_preview": p.model.complete,
                    "simplified": p.model.simplified, "issues": p.model.issues,
                    "local_bounds": p.model.local_bounds()
                }
            })).collect::<Vec<_>>().into();
        }
        result
    }
    pub fn cache(
        path: &Path,
        catalog: &Catalog,
        bindings: &Bindings,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        let source = Self::load(path, catalog, bindings, &cancelled)?;
        let root = crate::storage::local_data()?.join("papyrus-monument-cache");
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let destination = root.join(&source.sha256);
        if !destination.exists() {
            let stage = tempfile::Builder::new()
                .prefix(".monument-import-")
                .tempdir_in(&root)
                .map_err(|e| e.to_string())?;
            std::fs::copy(path, stage.path().join("monuments.json")).map_err(|e| e.to_string())?;
            let checked = Self::load(
                &stage.path().join("monuments.json"),
                catalog,
                bindings,
                &cancelled,
            )?;
            if checked.sha256 != source.sha256 {
                return Err("Monument source changed while importing".into());
            }
            if cancelled() {
                return Err("Monument import cancelled".into());
            }
            std::fs::rename(stage.path(), &destination).map_err(|e| e.to_string())?;
        }
        Self::cached(&source.recipe(), catalog, bindings, cancelled)
    }
    pub fn recipe(&self) -> Recipe {
        Recipe {
            engine: if self.coastal_profile.is_some() {
                crate::coastal_placement::BUDGET_VERSION
            } else {
                VERSION
            }
            .into(),
            source_sha256: self.sha256.clone(),
            canonical_yaw: Some(CANONICAL_YAW_VERSION.into()),
            land_alignment: self.stage_source.as_ref().map(|_| crate::monument_alignment::VERSION.into()),
            surface_modifiers: Some(SURFACE_MODIFIER_VERSION.into()),
            stage_rules: self.stage_source.as_ref().map(|s| s.binding()),
            dungeon_source: self.dungeon_source.clone(),
            offshore_source: self.offshore_source.clone(),
            coastal_search: self
                .coastal_profile
                .as_ref()
                .map(|_| crate::coastal_candidates::Policy::default()),
            coastal_profile: self.coastal_profile.clone(),
            coastal_navigation: self
                .coastal_profile
                .as_ref()
                .map(|_| papyrus_terrain::boat_navigation::SOURCE_VERSION.into()),
            coastal_normals: self
                .coastal_profile
                .as_ref()
                .map(|_| papyrus_terrain::terrain_normal::VERSION.into()),
            tier_search: self
                .coastal_profile
                .as_ref()
                .map(|_| TIER_RETRY_VERSION.into()),
            earthwork: self
                .coastal_profile
                .as_ref()
                .map(|_| AUTHORED_EARTHWORK_VERSION.into()),
        }
    }
    /// Bind missing parameter-component defaults to this verified source revision.
    /// The early terrain coordinator selects this revision with its ordered pass.
    pub fn recipe_with_source_defaults(&self, catalog: &Catalog) -> Result<Recipe, String> {
        let mut recipe = self.recipe();
        if crate::prefab_priority::supports(catalog, &self.code_sources)? {
            recipe.surface_modifiers = Some(SOURCE_PRIORITY_VERSION.into());
        }
        Ok(recipe)
    }
    pub fn load(
        path: &Path,
        catalog: &Catalog,
        bindings: &Bindings,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        if cancelled() {
            return Err("Monument source load cancelled".into());
        }
        let mut raw = Vec::new();
        File::open(path)
            .map_err(|e| e.to_string())?
            .take(32 * 1024 * 1024 + 1)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
        if raw.len() > 32 * 1024 * 1024 {
            return Err("Monument source exceeds 32 MiB".into());
        }
        let source = Self::decode(&raw, catalog, bindings.source_receipts(), &cancelled)?;
        verify_code_sources(
            bindings.installation_root(),
            &source.code_sources,
            &cancelled,
        )?;
        Ok(source)
    }
    fn decode(
        raw: &[u8],
        catalog: &Catalog,
        receipts: &[SourceFile],
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        use sha2::{Digest, Sha256};
        let d: Document =
            serde_json::from_slice(raw).map_err(|e| format!("Invalid monument source: {e}"))?;
        if d.format != "papyrus-monument-source"
            || !matches!(d.schema, 1 | 2)
            || d.adapter != "papyrus-monument-unit-yaw-1"
            || d.catalog_sha256 != catalog.sha256
            || d.sources != catalog.sources
            || !hash(&d.extractor_sha256)
            || !hash(&d.profile_sha256)
            || d.prefabs.is_empty()
            || d.prefabs.len() > 512
            || d.extra_sources.len() > 8
        {
            return Err(
                "Monument source differs from verified installation/catalog or adapter".into(),
            );
        }
        validate_code_sources(d.schema, &d.code_sources, catalog.installation.kind)?;
        let mut seen = std::collections::BTreeSet::new();
        for source in &d.extra_sources {
            if !seen.insert(&source.relative_path) || !receipts.contains(source) {
                return Err("Model source is not in the verified texture bundle receipts".into());
            }
        }
        let mut triangles = 0;
        for (path, p) in &d.prefabs {
            if cancelled() {
                return Err("Monument source load cancelled".into());
            }
            if !catalog.entries.contains_key(path) {
                return Err("Monument rules name an unverified prefab".into());
            }
            p.rules.validate()?;
            if let Some(attributes) = &p.placement_attributes {
                attributes.validate()?;
            }
            let m = &p.model;
            triangles += m.triangles.len();
            if m.vertices.len() > 200000
                || m.triangles.len() > 30000
                || triangles > 300000
                || m.issues.len() > 256
                || m.vertices
                    .iter()
                    .flatten()
                    .any(|v| !v.is_finite() || v.abs() > 10000.)
                || m.triangles
                    .iter()
                    .flatten()
                    .any(|i| *i as usize >= m.vertices.len())
                || m.complete && (m.triangles.is_empty() || !m.issues.is_empty())
            {
                return Err("Invalid or excessive monument model geometry".into());
            }
        }
        Ok(Self {
            sha256: format!("{:x}", Sha256::digest(raw)),
            extractor_sha256: d.extractor_sha256,
            profile_sha256: d.profile_sha256,
            prefabs: d.prefabs,
            models_requested: d.models_requested,
            coastal_profile: None,
            stage_source: None,
            dungeon_source: None, offshore_source: None,
            code_sources: d.code_sources,
        })
    }
    pub fn prepare_coastal(
        &mut self,
        catalog: &Catalog,
        root: &Path,
        cancelled: impl Fn() -> bool,
    ) -> Result<(), String> {
        if self
            .prefabs
            .values()
            .all(|p| p.placement_attributes.is_some())
        {
            self.coastal_profile =
                crate::coastal_placement::Profile::available(root, catalog, cancelled)?;
        }
        Ok(())
    }
    pub fn prepare_stages(
        &mut self,
        catalog: &Catalog,
        root: &Path,
        cancelled: impl Fn() -> bool,
    ) -> Result<(), String> {
        if cancelled() {
            return Err("Monument stage preparation cancelled".into());
        }
        let stages = crate::monument_stages::prepare(root, catalog, &cancelled)?;
        if cancelled() {
            return Err("Monument stage preparation cancelled".into());
        }
        self.stage_source = stages;
        Ok(())
    }
    pub fn cached(
        recipe: &Recipe,
        catalog: &Catalog,
        bindings: &Bindings,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        recipe.validate()?;
        let path = crate::client_cache::require_pinned_document(
            &crate::storage::local_data()?,
            "papyrus-monument-cache",
            &recipe.source_sha256,
            "monuments.json",
            "monument placement",
        )?;
        let mut source = Self::load(&path, catalog, bindings, &cancelled)?;
        if recipe.surface_modifiers.as_deref() == Some(SOURCE_PRIORITY_VERSION)
            && !crate::prefab_priority::supports(catalog, &source.code_sources)?
        {
            return Err("Prefab priority defaults differ from the selected monument source".into());
        }
        if let Some(profile) = &recipe.coastal_profile {
            profile.verify(bindings.installation_root(), catalog, &cancelled)?;
            if source
                .prefabs
                .values()
                .any(|p| p.placement_attributes.is_none())
            {
                return Err("Coastal recipe requires captured placement attributes".into());
            }
            source.coastal_profile = Some(profile.clone());
        }
        if let Some(binding) = &recipe.stage_rules {
            source.stage_source = Some(crate::monument_stages::Source::cached(
                binding,
                bindings.installation_root(),
                catalog,
                &cancelled,
            )?);
        }
        if source.sha256 != recipe.source_sha256 {
            return Err("Pinned monument source changed".into());
        }
        source.dungeon_source = recipe.dungeon_source.clone();
        source.offshore_source = recipe.offshore_source.clone();
        Ok(source)
    }

    pub(crate) fn cached_shared(
        recipe: &Recipe,
        catalog: &Catalog,
        bindings: &Bindings,
        cancelled: impl Fn() -> bool,
    ) -> Result<Arc<Self>, String> {
        if cancelled() {
            return Err("Monument source load cancelled".into());
        }
        let root = bindings.installation_root();
        let operation = CURRENT.with(|current| current.borrow().upgrade());
        let retained = operation.as_ref().and_then(|slot| {
            slot.borrow().as_ref().filter(|entry| {
                entry.root == root && entry.catalog == catalog.sha256
                    && entry.bindings == bindings.sha256 && entry.recipe == *recipe
            }).map(|entry| entry.source.clone())
        });
        if let Some(source) = retained {
            verify_code_sources(root, &source.code_sources, &cancelled)?;
            if let Some(profile) = &source.coastal_profile {
                profile.verify(root, catalog, &cancelled)?;
            }
            if let Some(stages) = &source.stage_source {
                stages.verify_installation(root, catalog, &cancelled)?;
            }
            if cancelled() {
                return Err("Monument source load cancelled".into());
            }
            return Ok(source);
        }
        let source = Self::cached(recipe, catalog, bindings, &cancelled)?;
        if cancelled() {
            return Err("Monument source load cancelled".into());
        }
        Ok(source.retain_verified(recipe, catalog, bindings))
    }

    /// Keep an owned source loaded for this exact recipe in the current job.
    pub(crate) fn retain_verified(
        self,
        recipe: &Recipe,
        catalog: &Catalog,
        bindings: &Bindings,
    ) -> Arc<Self> {
        let source = Arc::new(self);
        if let Some(slot) = CURRENT.with(|current| current.borrow().upgrade()) {
            Scope(slot).remember(recipe, catalog, bindings, source.clone());
        }
        source
    }
    pub fn discover(
        catalog: &Catalog,
        bindings: &Bindings,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        Self::discover_matching(catalog, bindings, None, None, None, cancelled)
    }
    /// Refresh supported installations with the bundled importer. Keep verified
    /// captures usable for installations without a bundled preparation profile.
    pub fn discover_rules(
        catalog: &Catalog,
        bindings: &Bindings,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        let preparations = match crate::monument_extract::selected_profile(catalog)? {
            Some(raw) => crate::source_profile::hashes(raw)
                .into_iter()
                .map(|hash| Some((crate::metadata_runtime::monument_extractor_sha256(), hash)))
                .collect(),
            None => vec![None],
        };
        for models in [false, true] {
            for preparation in &preparations {
                let expected = preparation.as_ref()
                    .map(|(extractor, profile)| (extractor.as_str(), profile.as_str()));
                if let Some(source) = Self::discover_matching(
                    catalog, bindings, Some(models), None, expected, &cancelled,
                )? {
                    return Ok(Some(source));
                }
            }
        }
        Ok(None)
    }
    pub fn discover_models(
        catalog: &Catalog,
        bindings: &Bindings,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        Self::discover_matching(catalog, bindings, Some(true), None, None, cancelled)
    }
    /// Keep searching when a cached model capture belongs to different rules.
    /// A cache miss lets the caller prepare geometry without replacing rules.
    pub fn discover_models_for(
        &self,
        catalog: &Catalog,
        bindings: &Bindings,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        Self::discover_matching(catalog, bindings, Some(true), Some(self), None, cancelled)
    }
    fn discover_matching(
        catalog: &Catalog,
        bindings: &Bindings,
        models: Option<bool>,
        rules: Option<&Self>,
        preparation: Option<(&str, &str)>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        let root = crate::storage::local_data()?.join("papyrus-monument-cache");
        Self::discover_matching_in(
            &root,
            catalog,
            bindings,
            models,
            rules,
            preparation,
            cancelled,
        )
    }
    fn discover_matching_in(
        root: &Path,
        catalog: &Catalog,
        bindings: &Bindings,
        models: Option<bool>,
        rules: Option<&Self>,
        preparation: Option<(&str, &str)>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        if !root.is_dir() {
            return Ok(None);
        }
        let mut paths = Vec::new();
        for (i, e) in std::fs::read_dir(root)
            .map_err(|e| e.to_string())?
            .enumerate()
        {
            if i >= 256 {
                return Err("Too many monument source snapshots".into());
            }
            let path = e.map_err(|e| e.to_string())?.path().join("monuments.json");
            if path.is_file() {
                paths.push((
                    std::fs::metadata(&path)
                        .ok()
                        .and_then(|m| m.modified().ok()),
                    path,
                ));
            }
        }
        paths.sort();
        paths.reverse();
        let mut collision_fallback = None;
        for (_, path) in paths {
            if cancelled() {
                return Ok(None);
            }
            if let Ok(source) = Self::load(&path, catalog, bindings, &cancelled) {
                if models.is_some_and(|want| source.models_requested != want) {
                    continue;
                }
                if preparation.is_some_and(|(extractor, profile)| {
                    !crate::metadata_runtime::monument_cache_extractors(extractor.into())
                        .contains(&source.extractor_sha256)
                        || source.profile_sha256 != profile
                }) {
                    continue;
                }
                if rules.is_some_and(|rules| !rules.matches_placement_data(&source)) {
                    continue;
                }
                // Supplemental collision captures share the rules schema. Keep
                // an existing numeric binding stable so its dependent caches
                // remain reusable after collision data is prepared.
                if models == Some(false) && rules.is_none()
                    && crate::monument_extract::has_environment_meshes(&source)
                {
                    collision_fallback.get_or_insert(source);
                    continue;
                }
                return Ok(Some(source));
            }
        }
        Ok(collision_fallback)
    }
}
