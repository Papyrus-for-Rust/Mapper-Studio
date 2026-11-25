use super::*;
use crate::prefab_assets::Asset;
use serde_json::{Value, json};
use std::collections::HashMap;

const PREFAB: &str = "assets/bundled/prefabs/autospawn/monument/test/a.prefab";

fn geometry_receipt(root: &Path, path: &str, bytes: &[u8]) -> SourceFile {
    let destination = root.join(path);
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    std::fs::write(destination, bytes).unwrap();
    SourceFile {
        relative_path: path.into(),
        bytes: bytes.len() as u64,
        sha256: hash(bytes),
    }
}
fn fixture() -> (tempfile::TempDir, Catalog, Value) {
    let root = tempfile::tempdir().unwrap();
    let native = SourceFile {
        relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
        sha256: "a".repeat(64),
        bytes: 10,
    };
    let manifest = geometry_receipt(root.path(), MANIFEST, b"manifest");
    let scenes = geometry_receipt(root.path(), SCENES, b"scenes");
    let mesh = geometry_receipt(root.path(), "Bundles/shared/content.bundle", b"mesh");
    let catalog = Catalog {
        sha256: "a".repeat(64),
        installation: installation::Installation {
            kind: installation::InstallationKind::Client,
            native: native.clone(),
            scene_manifest: Some(manifest.clone()),
        },
        sources: vec![native.clone(), manifest.clone(), scenes.clone()],
        entries: HashMap::from([(
            PREFAB.into(),
            Asset {
                priority: None,
                bounds: None,
                yaw: None,
                modifier_size: None,
                modifier_offset: None,
                texture_guids: BTreeMap::new(),
                splat_mask: None,
                modifier: None,
                modifier_absent: true,
                water_geometry: None,
                no_build_geometry: None,
                display_name: None,
            },
        )]),
    };
    let value = json!({"format":"mapper-monument-tag-reference","schema":1,"inspection_only":true,
        "game_code_executed":false,"sources":[native,manifest,scenes],"extra_sources":[mesh],
        "extractor_sha256":metadata_runtime::monument_tag_extractor_sha256(),"catalog_sha256":catalog.sha256,
        "prefabs":{PREFAB:{"space":"prefab-root-local-unity","monuments":[],"tags":[],
            "issues":[],"complete":true,"root_rotation":[0.,0.,0.,1.],"root_scale":[1.,1.,1.],"nodes_visited":1}}});
    (root, catalog, value)
}

#[test]
fn tag_data_producers_and_reset_registration_match_existing_captures() {
    assert_eq!(
        metadata_runtime::monument_tag_extractor_sha256(),
        AFFINE_PRODUCER
    );
    assert_eq!(
        metadata_runtime::water_treatment_extractor_sha256(),
        "14d94082e7819a28ac5a813b3994a59ce7017fa6c9890aeb5a26c05731e9ff3f"
    );
    assert!(crate::client_cache::DIRECTORIES.contains(&CACHE));
}

#[test]
fn strict_headers_rosters_receipts_and_prefab_layouts_are_required() {
    let (_root, catalog, value) = fixture();
    let raw = serde_json::to_vec(&value).unwrap();
    let source = Source::read(&raw, &catalog, || false).unwrap();
    assert_eq!(source.sha256(), hash(&raw));
    assert_eq!(source.prefabs().len(), 1);
    for producer in [
        INDEXED_PRODUCER,
        PREVIOUS_PRODUCER,
        EQUIVALENT_PRODUCER,
        BASELINE_PRODUCER,
    ] {
        let mut compatible = value.clone();
        compatible["extractor_sha256"] = producer.into();
        assert!(Source::read(&serde_json::to_vec(&compatible).unwrap(), &catalog, || false).is_ok());
    }
    for (pointer, bad) in [
        ("/schema", json!(2)),
        ("/inspection_only", json!(false)),
        ("/game_code_executed", json!(true)),
        ("/extractor_sha256", json!("b".repeat(64))),
        ("/catalog_sha256", json!("invalid")),
        ("/prefabs", json!({})),
        ("/sources/1/sha256", json!("b".repeat(64))),
        ("/sources/2/bytes", json!(0)),
        ("/extra_sources/0/relative_path", json!("../content.bundle")),
    ] {
        let mut invalid = value.clone();
        *invalid.pointer_mut(pointer).unwrap() = bad;
        assert!(
            Source::read(&serde_json::to_vec(&invalid).unwrap(), &catalog, || false).is_err(),
            "{pointer}"
        );
    }
    let mut invalid = value.clone();
    invalid["prefabs"][PREFAB]["complete"] = false.into();
    assert!(Source::read(&serde_json::to_vec(&invalid).unwrap(), &catalog, || false).is_err());
    assert!(Source::read(&vec![b' '; LIMIT as usize + 1], &catalog, || false).is_err());
}

#[test]
fn cache_reuses_geometry_across_catalog_code_and_installation_location_changes() {
    let (root, mut catalog, mut value) = fixture();
    value["extractor_sha256"] = PREVIOUS_PRODUCER.into();
    let local = root.path().join("local");
    let raw = serde_json::to_vec(&value).unwrap();
    let directory = local.join(CACHE).join(hash(&raw));
    std::fs::create_dir_all(&directory).unwrap();
    let document = directory.join("source.json");
    std::fs::write(&document, &raw).unwrap();
    let skipped_catalog = || panic!("A verified cache hit should not read the catalog");
    let source = prepare_in(root.path(), skipped_catalog, &catalog, &local, || false).unwrap();
    catalog.sha256 = "b".repeat(64);
    catalog.installation.native.sha256 = "c".repeat(64);
    catalog.sources[0].sha256 = "c".repeat(64);
    let refreshed = prepare_in(root.path(), skipped_catalog, &catalog, &local, || false).unwrap();
    assert_eq!(source.sha256(), refreshed.sha256());
    assert_eq!(refreshed.captured_catalog_sha256(), "a".repeat(64));
    let relocated = tempfile::tempdir().unwrap();
    for path in [MANIFEST, SCENES, "Bundles/shared/content.bundle"] {
        geometry_receipt(
            relocated.path(),
            path,
            &std::fs::read(root.path().join(path)).unwrap(),
        );
    }
    assert_eq!(
        prepare_in(relocated.path(), skipped_catalog, &catalog, &local, || false)
            .unwrap()
            .sha256(),
        source.sha256()
    );
    assert_eq!(std::fs::read(&document).unwrap(), raw);
    assert_eq!(std::fs::read_dir(local.join(CACHE)).unwrap().count(), 1);
}

#[test]
fn changed_geometry_missing_data_and_wrong_cache_id_cannot_reuse_a_snapshot() {
    let (root, mut catalog, value) = fixture();
    let raw = serde_json::to_vec(&value).unwrap();
    let local = root.path().join("local");
    let directory = local.join(CACHE).join("b".repeat(64));
    std::fs::create_dir_all(&directory).unwrap();
    let document = directory.join("source.json");
    std::fs::write(&document, &raw).unwrap();
    assert!(
        prepare_in(
            root.path(),
            || Ok(root.path().join("missing.json")),
            &catalog,
            &local,
            || false
        )
        .is_err()
    );
    Source::load(root.path(), &document, &catalog, || false).unwrap();
    std::fs::write(
        root.path().join("Bundles/shared/content.bundle"),
        b"changed mesh",
    )
    .unwrap();
    assert!(Source::load(root.path(), &document, &catalog, || false).is_err());
    let asset = catalog.entries[PREFAB].clone();
    catalog.entries.insert("assets/other.prefab".into(), asset);
    assert!(Source::read(&raw, &catalog, || false).is_err());
    assert_eq!(std::fs::read(document).unwrap(), raw);
}

#[test]
fn reopening_a_retained_capture_checks_the_exact_file_and_live_geometry() {
    let (root, catalog, value) = fixture();
    let raw = serde_json::to_vec(&value).unwrap();
    let source = Arc::new(Source::read(&raw, &catalog, || false).unwrap());
    let local = tempfile::tempdir().unwrap();
    let cache = local.path().join(CACHE);
    let directory = cache.join(source.sha256());
    std::fs::create_dir_all(&directory).unwrap();
    let document = directory.join("source.json");
    std::fs::write(&document, &raw).unwrap();
    let reopened = reopen_retained(&cache, root.path(), source.clone(), &|| false).unwrap();
    assert!(Arc::ptr_eq(&source, &reopened));

    assert!(reopen_retained(&cache, root.path(), source.clone(), &|| true).is_none());
    assert!(reopen_retained(local.path(), root.path(), source.clone(), &|| false).is_none());
    std::fs::write(&document, b"changed cache file").unwrap();
    assert!(reopen_retained(&cache, root.path(), source.clone(), &|| false).is_none());
    std::fs::write(&document, &raw).unwrap();
    geometry_receipt(root.path(), "Bundles/shared/content.bundle", b"changed mesh");
    assert!(reopen_retained(&cache, root.path(), source, &|| false).is_none());
}

#[test]
fn cancellation_prevents_load_or_preparation_before_accessing_missing_files() {
    let (root, catalog, _value) = fixture();
    let missing = root.path().join("missing");
    for error in [
        Source::load(&missing, &missing, &catalog, || true).err(),
        prepare_in(&missing, || panic!("Cancelled preparation requested the catalog"), &catalog, &missing, || true).err(),
    ] {
        assert_eq!(error.as_deref(), Some("Monument tag preparation cancelled"));
    }
    assert!(!missing.exists());
}
