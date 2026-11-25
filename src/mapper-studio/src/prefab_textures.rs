//! Exact GUID/path texture bindings. Verify sources once; revalidate pixels on each load.
use crate::{
    installation::{self, SourceFile},
    prefab_assets::Catalog,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Texture {
    #[serde(default)]
    pub payload_sha256: Option<String>,
    pub asset_path: String,
    pub bundle_relative_path: String,
    pub rgba_path: String,
    pub width: usize,
    pub height: usize,
    pub bytes: u64,
    pub sha256: String,
    #[serde(default)]
    pub storage: Option<Storage>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Storage {
    pub format: String,
    pub bytes: u64,
    pub sha256: String,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{
        installation::{Installation, InstallationKind},
        prefab_assets::Asset,
    };
    #[test]
    fn source_plans_accept_only_bounded_exact_catalog_bundle_paths() {
        let catalog = "a".repeat(64);
        let plan = serde_json::json!({
            "format":"mapper-texture-sources", "schema":1, "catalog_sha256":catalog,
            "bundles":["Bundles/shared/content.bundle", "Bundles/shared/monuments.bundle",
                       "Bundles/shared/textures.4.bundle"]
        });
        let parse =
            |value: &serde_json::Value| source_plan(&serde_json::to_vec(value).unwrap(), &catalog);
        assert_eq!(parse(&plan).unwrap().len(), 3);
        for (field, value) in [
            ("format", serde_json::json!("other")),
            ("schema", serde_json::json!(2)),
            ("catalog_sha256", serde_json::json!("b".repeat(64))),
            ("bundles", serde_json::json!([])),
            (
                "bundles",
                serde_json::json!(["Bundles/shared/content.bundle"]),
            ),
            (
                "bundles",
                serde_json::json!([
                    "Bundles/shared/content.bundle",
                    "Bundles/shared/monuments.bundle",
                    "../outside"
                ]),
            ),
            (
                "bundles",
                serde_json::json!([
                    "Bundles/shared/content.bundle",
                    "Bundles/shared/monuments.bundle",
                    "Bundles/shared/monuments.bundle"
                ]),
            ),
            (
                "bundles",
                serde_json::json!([
                    "Bundles/shared/monuments.bundle",
                    "Bundles/shared/content.bundle"
                ]),
            ),
            ("extra", serde_json::json!(true)),
        ] {
            let mut invalid = plan.clone();
            invalid[field] = value;
            assert!(parse(&invalid).is_err(), "accepted {invalid}");
        }
        assert!(source_plan(&vec![b' '; 4097], &catalog).is_err());

        let mut native = plan;
        native["schema"] = 2.into();
        let guid = "a".repeat(32);
        native["paths"] = serde_json::json!({(guid.clone()): "assets/modifiers/blend.png"});
        native["content_has_textures"] = false.into();
        assert_eq!(parse(&native).unwrap().len(), 3);
        native["paths"][guid] = "assets/../elsewhere".into();
        assert!(parse(&native).is_err());
    }
    pub(crate) fn fixture(root: &Path) -> (Catalog, PathBuf, serde_json::Value, String) {
        let paths = [
            "RustClient_Data/Plugins/x86_64/RustNative.dll",
            "Bundles/AssetSceneManifest.json",
            "Bundles/shared/assetscenes.bundle",
            "Bundles/shared/content.bundle",
            "Bundles/shared/monuments.bundle",
            "Bundles/maps/maps.bundle",
        ];
        let mut sources = Vec::new();
        for path in paths {
            std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
            std::fs::write(root.join(path), path.as_bytes()).unwrap();
            sources.push(installation::hash_file(root, path, 1024, &|| false).unwrap());
        }
        let guid = "a".repeat(32);
        let catalog = Catalog {
            sha256: "b".repeat(64),
            installation: Installation {
                kind: InstallationKind::Client,
                native: sources[0].clone(),
                scene_manifest: Some(sources[1].clone()),
            },
            sources: sources[..3].to_vec(),
            entries: std::collections::HashMap::from([(
                "assets/test.prefab".into(),
                Asset {
                    priority: None,
                    bounds: None,
                    yaw: None,
                    modifier_size: None,
                    modifier_offset: None,
                    texture_guids: BTreeMap::from([("blend".into(), guid.clone())]),
                    splat_mask: Some(255),
                    modifier: None,
                    modifier_absent: false,
                    water_geometry: None,
                    no_build_geometry: None,
                    display_name: None,
                },
            )]),
        };
        let pixels = [1, 2, 3, 255];
        std::fs::create_dir(root.join("textures")).unwrap();
        std::fs::write(root.join(format!("textures/{guid}.rgba")), pixels).unwrap();
        let mut doc = serde_json::json!({"format":"papyrus-prefab-textures","schema":1,"adapter":"papyrus-guid-rgba-1","catalog_sha256":catalog.sha256,"sources":sources,
            "pixel_format":"RGBA8-top-left","textures":{},"decoded_bytes":4});
        doc["textures"][&guid] = serde_json::json!({"asset_path":"assets/monument/blend.png","bundle_relative_path":"Bundles/shared/monuments.bundle","rgba_path":format!("textures/{guid}.rgba"),"width":1,"height":1,"bytes":4,"sha256":format!("{:x}",Sha256::digest(pixels))});
        let receipt = root.join("bindings.json");
        std::fs::write(&receipt, serde_json::to_vec(&doc).unwrap()).unwrap();
        (catalog, receipt, doc, guid)
    }

    #[test]
    fn cli_recipe_pins_are_checked_before_texture_pixels_or_terrain() {
        use crate::{AssetIdentity, Project, headless, native_preview};
        use std::sync::atomic::AtomicBool;

        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let (catalog, receipt, mut bindings, guid) = fixture(root);
        let path = root.join("catalog.json");
        let prefab = "assets/bundled/prefabs/autospawn/monument/test.prefab";
        let document = serde_json::json!({
            "format": "papyrus-prefab-catalog",
            "schema": 1,
            "adapter": crate::prefab_assets::DISCOVERY_ADAPTER,
            "extractor_sha256": "a".repeat(64),
            "sources": catalog.sources,
            "metadata": {
                "format": 5,
                "modifiers": { (prefab): { "textureGuids": { "blend": guid } } },
                "special": {},
            },
        });
        let document = serde_json::to_vec(&document).unwrap();
        let catalog_sha256 = format!("{:x}", Sha256::digest(&document));
        std::fs::write(&path, document).unwrap();
        bindings["catalog_sha256"] = serde_json::json!(&catalog_sha256);
        let bindings = serde_json::to_vec(&bindings).unwrap();
        let bindings_sha256 = format!("{:x}", Sha256::digest(&bindings));
        std::fs::write(&receipt, bindings).unwrap();
        std::fs::remove_file(root.join(format!("textures/{guid}.rgba"))).unwrap();

        let mut recipe = Project {
            preview_engine: native_preview::VERSION.into(),
            installation: Some(catalog.installation),
            monument_assets: Some(AssetIdentity {
                catalog_sha256,
                bindings_sha256: "f".repeat(64),
            }),
            ..Project::default()
        };
        let options = headless::Options {
            rust_root: Some(root.to_owned()),
            catalog: Some(path),
            bindings: Some(receipt),
            ..headless::Options::default()
        };
        let error = |recipe: &Project| {
            let mut stages = Vec::new();
            let result = headless::generate_map_world(
                recipe,
                &options,
                &AtomicBool::new(false),
                |label, _| stages.push(label.to_owned()),
            );
            assert_eq!(stages, ["Verifying monument assets"]);
            result.err().expect("Invalid recipe or texture payload")
        };
        assert!(error(&recipe).contains("saved fingerprints"));
        recipe.monument_assets.as_mut().unwrap().bindings_sha256 = bindings_sha256;
        assert_eq!(error(&recipe), DAMAGED_PAYLOAD);
    }

    #[test]
    fn disabled_water_reference_is_ignored_only_with_explicit_captured_flag() {
        let directory = tempfile::tempdir().unwrap();
        let (mut catalog, receipt, _, guid) = fixture(directory.path());
        let bindings = Bindings::load(directory.path(), &receipt, &catalog, || false).unwrap();
        let asset = catalog.entries.get_mut("assets/test.prefab").unwrap();
        asset.texture_guids = BTreeMap::from([("water".into(), guid)]);
        asset.modifier_size = Some([200., 600., 200.]);
        asset.modifier_offset = Some([0.; 3]);
        asset.splat_mask = Some(0);
        asset.modifier = Some(crate::prefab_assets::Modifier {
            height_map: false,
            water_map: None,
            alpha_map: false,
            biome_mask: 0,
            topology_mask: 0,
            radius: 100.,
            fade: 0.,
            auto_cliff_splat: false,
            auto_cliff_topology: false,
            remove_existing_topology: false,
        });
        for (flag, allowed) in [(None, false), (Some(true), false), (Some(false), true)] {
            asset.modifier.as_mut().unwrap().water_map = flag;
            let result =
                crate::composition::prepare_with_budget(asset, &bindings, 1000, || false, 1024);
            assert_eq!(result.is_ok(), allowed, "{flag:?}");
        }
        assert!(asset.texture_guids.contains_key("water"));
    }
    #[test]
    fn biome_only_preview_does_not_require_disabled_material_textures() {
        let dir = tempfile::tempdir().unwrap();
        let (mut catalog, receipt, _, guid) = fixture(dir.path());
        let asset = catalog.entries.get_mut("assets/test.prefab").unwrap();
        asset.texture_guids = BTreeMap::from([("biome".into(), guid)]);
        asset.splat_mask = Some(0);
        asset.modifier_size = Some([200., 600., 200.]);
        asset.modifier_offset = Some([0.; 3]);
        asset.modifier = Some(crate::prefab_assets::Modifier {
            height_map: false,
            water_map: None,
            alpha_map: false,
            biome_mask: 8,
            topology_mask: 0,
            radius: 100.,
            fade: 20.,
            auto_cliff_splat: false,
            auto_cliff_topology: false,
            remove_existing_topology: false,
        });
        let bindings = Bindings::load(dir.path(), &receipt, &catalog, || false).unwrap();
        let preview =
            crate::splat_preview::load(&bindings, &catalog.entries["assets/test.prefab"], || false)
                .unwrap()
                .unwrap();
        assert!(preview.image.pixels.iter().all(|p| p.a() == 0));
        let biome = preview.biome.unwrap();
        assert!(biome.pixels.iter().any(|p| p.a() > 128));
        assert!(
            crate::splat_preview::load(&bindings, &catalog.entries["assets/test.prefab"], || true)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn compressed_pixels_are_lossless_lazy_bounded_and_checked_again_on_each_read() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let (catalog, receipt, mut doc, guid) = fixture(dir.path());
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&[1, 2, 3, 255]).unwrap();
        let encoded = encoder.finish().unwrap();
        let path = format!("textures/{guid}.rgba.zlib");
        std::fs::write(dir.path().join(&path), &encoded).unwrap();
        doc["adapter"] = "papyrus-guid-rgba-zlib-2".into();
        doc["textures"][&guid]["rgba_path"] = path.clone().into();
        doc["textures"][&guid]["storage"] = serde_json::json!({"format":"zlib","bytes":encoded.len(),"sha256":format!("{:x}",Sha256::digest(&encoded))});
        std::fs::write(&receipt, serde_json::to_vec(&doc).unwrap()).unwrap();
        let bindings = Bindings::load(dir.path(), &receipt, &catalog, || false).unwrap();
        assert_eq!(bindings.read_rgba(&guid).unwrap(), [1, 2, 3, 255]);
        std::fs::write(dir.path().join(&path), vec![0; encoded.len()]).unwrap();
        assert!(bindings.read_rgba(&guid).is_err());
        assert!(Bindings::load(dir.path(), &receipt, &catalog, || false).is_err());
        // A valid stored checksum never substitutes for the decoded checksum.
        std::fs::write(dir.path().join(&path), &encoded).unwrap();
        doc["textures"][&guid]["sha256"] = "f".repeat(64).into();
        std::fs::write(&receipt, serde_json::to_vec(&doc).unwrap()).unwrap();
        let lazy = Bindings::load(dir.path(), &receipt, &catalog, || false).unwrap();
        assert!(lazy.read_rgba(&guid).is_err());
        // Match the decoded receipt so the length and stream checks are tested
        // independently of an unrelated checksum failure.
        doc["textures"][&guid]["sha256"] = format!("{:x}", Sha256::digest([1, 2, 3, 255])).into();
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&vec![1; 1024 * 1024]).unwrap();
        let oversized = encoder.finish().unwrap();
        let mut trailing = encoded.clone();
        trailing.extend_from_slice(b"trailing");
        let truncated = &encoded[..encoded.len() - 1];
        for invalid in [oversized.as_slice(), trailing.as_slice(), truncated] {
            std::fs::write(dir.path().join(&path), invalid).unwrap();
            doc["textures"][&guid]["storage"] = serde_json::json!({"format":"zlib","bytes":invalid.len(),"sha256":format!("{:x}",Sha256::digest(invalid))});
            std::fs::write(&receipt, serde_json::to_vec(&doc).unwrap()).unwrap();
            let lazy = Bindings::load(dir.path(), &receipt, &catalog, || false).unwrap();
            assert!(lazy.read_rgba(&guid).is_err());
        }
    }
    #[test]
    fn verified_private_payloads_stay_readable_after_publication() {
        let game = tempfile::tempdir().unwrap();
        let (catalog, receipt, _, guid) = fixture(game.path());
        let staging = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let folder = staging.path().join("a".repeat(64));
        let relative = format!("textures/{guid}.rgba");
        std::fs::create_dir_all(folder.join("textures")).unwrap();
        std::fs::copy(game.path().join(&relative), folder.join(&relative)).unwrap();
        let staged_receipt = folder.join("bindings.json");
        std::fs::copy(receipt, &staged_receipt).unwrap();

        let mut bindings = Bindings::load_manifest(game.path(), &staged_receipt, &catalog, || false)
            .unwrap();
        assert!(bindings.verify_staged_payloads(|| false).ok().unwrap());
        let published = crate::metadata_runtime::publish_with_status(
            &staged_receipt,
            cache.path(),
            &bindings.sha256,
            4 * 1024 * 1024,
        )
        .unwrap();
        assert!(published.moved);
        assert!(!staged_receipt.exists());
        assert!(bindings.use_published_root(&published.path).unwrap());
        assert_eq!(bindings.read_rgba(&guid).unwrap(), [1, 2, 3, 255]);
        std::fs::write(published.path.parent().unwrap().join(relative), [9, 2, 3, 255]).unwrap();
        assert!(bindings.read_rgba(&guid).is_err());
    }

    #[test]
    fn verifies_exact_pixels_and_rejects_changed_cache_or_game_source() {
        let dir = tempfile::tempdir().unwrap();
        let (catalog, receipt, _, guid) = fixture(dir.path());
        let bindings = Bindings::load(dir.path(), &receipt, &catalog, || false).unwrap();
        assert_eq!(bindings.read_rgba(&guid).unwrap(), [1, 2, 3, 255]);
        std::fs::write(
            dir.path().join(format!("textures/{guid}.rgba")),
            [9, 2, 3, 255],
        )
        .unwrap();
        assert!(bindings.read_rgba(&guid).is_err());
        assert!(Bindings::load(dir.path(), &receipt, &catalog, || false).is_err());
        std::fs::write(
            dir.path().join(format!("textures/{guid}.rgba")),
            [1, 2, 3, 255],
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Bundles/shared/monuments.bundle"),
            "changed",
        )
        .unwrap();
        assert!(
            Bindings::load(dir.path(), &receipt, &catalog, || false)
                .err()
                .unwrap()
                .contains("source changed")
        );
    }

    #[test]
    fn repairs_missing_and_corrupt_payloads_from_the_exact_saved_binding() {
        let game = tempfile::tempdir().unwrap();
        let donor = tempfile::tempdir().unwrap();
        let (catalog, receipt, _, guid) = fixture(game.path());
        let relative = format!("textures/{guid}.rgba");
        let destination = game.path().join(&relative);
        let donor_path = donor.path().join(&relative);
        std::fs::create_dir(donor_path.parent().unwrap()).unwrap();
        std::fs::copy(&destination, &donor_path).unwrap();
        let donor_receipt = donor.path().join("bindings.json");
        std::fs::copy(&receipt, &donor_receipt).unwrap();
        let binding_hash = std::fs::read(&receipt).unwrap();
        let cached = Bindings::load_manifest(game.path(), &receipt, &catalog, || false).unwrap();
        let mut recovered = Bindings::load(game.path(), &donor_receipt, &catalog, || false).unwrap();
        std::fs::write(&destination, [9, 2, 3, 255]).unwrap();
        assert!(matches!(
            cached.verify_payloads(|| false),
            Err(PayloadError::Damaged)
        ));
        assert!(cached.repair_from(&recovered, || true).is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), [9, 2, 3, 255]);
        recovered.sha256 = "f".repeat(64);
        assert!(cached.repair_from(&recovered, || false).is_err());
        recovered.sha256.clone_from(&cached.sha256);
        cached.repair_from(&recovered, || false).unwrap();
        assert_eq!(cached.read_rgba(&guid).unwrap(), [1, 2, 3, 255]);
        std::fs::remove_file(&destination).unwrap();
        cached.repair_from(&recovered, || false).unwrap();
        assert_eq!(cached.read_rgba(&guid).unwrap(), [1, 2, 3, 255]);
        assert_eq!(std::fs::read(&receipt).unwrap(), binding_hash);
    }

    #[test]
    fn parallel_texture_verification_matches_serial_errors_and_polls_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let (catalog, receipt, _, guid) = fixture(dir.path());
        let mut bindings = Bindings::load_manifest(dir.path(), &receipt, &catalog, || false).unwrap();
        let first = bindings.textures[&guid].clone();
        for name in ["second", "third", "fourth"] {
            let mut texture = first.clone();
            texture.rgba_path = format!("textures/{name}.rgba");
            std::fs::copy(
                dir.path().join(&first.rgba_path),
                dir.path().join(&texture.rgba_path),
            )
            .unwrap();
            bindings.textures.insert(name.into(), texture);
        }
        let second_path = dir.path().join(&bindings.textures["second"].rgba_path);
        assert!(matches!(
            bindings.verify_payloads_in_batches(4, 16, &|| false, true),
            Ok(true)
        ));
        std::fs::write(&second_path, [9, 2, 3, 255]).unwrap();
        assert!(matches!(
            bindings.verify_payloads_in_batches(4, 16, &|| false, false),
            Err(PayloadError::Damaged)
        ));
        std::fs::write(&second_path, [1, 2, 3, 255, 0]).unwrap();
        assert!(matches!(
            bindings.verify_payloads_in_batches(3, 16, &|| false, false),
            Err(PayloadError::Damaged)
        ));
        let calls = std::cell::Cell::new(0);
        let cancelled = || {
            calls.set(calls.get() + 1);
            calls.get() >= 2
        };
        assert!(matches!(
            bindings.verify_payloads_in_batches(4, 16, &cancelled, false),
            Err(PayloadError::Other(message)) if message == "Texture verification cancelled"
        ));
    }

    #[test]
    fn complementary_archives_restore_only_needed_texture_payloads() {
        let game = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let (mut catalog, receipt, mut doc, guid) = fixture(game.path());
        let other_guid = "d".repeat(32);
        let asset = catalog.entries.get_mut("assets/test.prefab").unwrap();
        asset.texture_guids.insert("biome".into(), other_guid.clone());
        let first_path = format!("textures/{guid}.rgba");
        let second_path = format!("textures/{other_guid}.rgba");
        let mut descriptor = doc["textures"][&guid].clone();
        descriptor["rgba_path"] = second_path.clone().into();
        doc["textures"][&other_guid] = descriptor;
        doc["decoded_bytes"] = 8.into();
        let raw = serde_json::to_vec(&doc).unwrap();
        for root in [first.path(), second.path()] {
            std::fs::create_dir(root.join("textures")).unwrap();
            std::fs::write(root.join("bindings.json"), &raw).unwrap();
        }
        std::fs::write(&receipt, raw).unwrap();
        std::fs::remove_file(game.path().join(&first_path)).unwrap();
        std::fs::write(game.path().join(&second_path), [0; 4]).unwrap();
        std::fs::write(first.path().join(&first_path), [1, 2, 3, 255]).unwrap();
        std::fs::write(second.path().join(&second_path), [1, 2, 3, 255]).unwrap();

        let load = |receipt: &Path| {
            Bindings::load_manifest(game.path(), receipt, &catalog, || false).unwrap()
        };
        let cached = load(&receipt);
        let first = load(&first.path().join("bindings.json"));
        let second = load(&second.path().join("bindings.json"));
        assert!(!cached.repair_from_available(&first, || false).unwrap());
        assert_eq!(cached.read_rgba(&guid).unwrap(), [1, 2, 3, 255]);
        assert!(cached.read_rgba(&other_guid).is_err());
        assert!(cached.repair_from_available(&second, || false).unwrap());
        assert!(cached.verify_payloads(|| false).is_ok());
        assert_eq!(cached.read_rgba(&other_guid).unwrap(), [1, 2, 3, 255]);
    }

    #[test]
    fn retained_manifest_uses_the_selected_bytes_when_the_file_changes() {
        let game = tempfile::tempdir().unwrap();
        let (catalog, receipt, _, guid) = fixture(game.path());
        let raw = std::fs::read(&receipt).unwrap();
        std::fs::write(&receipt, b"{}").unwrap();
        let bindings =
            Bindings::load_manifest_document(game.path(), &receipt, &raw, &catalog, || false)
                .unwrap();
        assert_eq!(bindings.sha256, format!("{:x}", Sha256::digest(&raw)));
        assert_eq!(bindings.read_rgba(&guid).unwrap(), [1, 2, 3, 255]);
        assert!(Bindings::load(game.path(), &receipt, &catalog, || false).is_err());
    }
    #[test]
    fn rejects_missing_bindings_traversal_cancellation_and_catalog_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let (catalog, receipt, mut doc, guid) = fixture(dir.path());
        assert!(Bindings::load(dir.path(), &receipt, &catalog, || true).is_err());
        let original = doc.clone();
        doc["textures"][&guid]["rgba_path"] = "../outside.rgba".into();
        std::fs::write(&receipt, serde_json::to_vec(&doc).unwrap()).unwrap();
        assert!(Bindings::load(dir.path(), &receipt, &catalog, || false).is_err());
        doc = original.clone();
        doc["textures"] = serde_json::json!({});
        std::fs::write(&receipt, serde_json::to_vec(&doc).unwrap()).unwrap();
        assert!(Bindings::load(dir.path(), &receipt, &catalog, || false).is_err());
        doc = original;
        doc["catalog_sha256"] = "c".repeat(64).into();
        std::fs::write(&receipt, serde_json::to_vec(&doc).unwrap()).unwrap();
        assert!(Bindings::load(dir.path(), &receipt, &catalog, || false).is_err());
    }

    #[test]
    fn authored_search_accepts_preparable_slopes_and_rejects_changed_assets() {
        use crate::{
            Project, native_preview, placement_preview,
            prefab_assets::{Bounds, Modifier},
        };
        use papyrus_terrain::native_layers::{Layers, dimensions};
        let encode = |h: f32| ((h + 500.) / 1000. * 32766.).round().clamp(0., 32766.) as i16;
        let dir = tempfile::tempdir().unwrap();
        let (mut catalog, receipt, mut doc, guid) = fixture(dir.path());
        let mut project = Project {
            size: 1000,
            installation: Some(catalog.installation.clone()),
            preview_engine: native_preview::VERSION.into(),
            placement_engine: placement_preview::ROTATION_VERSION.into(),
            ..Default::default()
        };
        project.targets.retain(|t| t.name == "Satellite Dish");
        project.targets[0].x = 0.;
        project.targets[0].z = 0.;
        project.targets[0].yaw = Some(37.);
        project.targets[0].exact = true;
        let path = project.targets[0].prefab_path.clone().unwrap();
        let mut asset = catalog.entries.remove("assets/test.prefab").unwrap();
        asset.bounds = Some(Bounds {
            center: [0.; 3],
            extent: [20., 20., 20.],
            minimum_world_size: 1000,
        });
        asset.modifier_size = Some([100., 100., 100.]);
        asset.modifier_offset = Some([0.; 3]);
        asset.splat_mask = Some(0);
        asset.texture_guids.insert("height".into(), guid.clone());
        asset.modifier = Some(Modifier {
            height_map: true,
            water_map: None,
            alpha_map: false,
            biome_mask: 0,
            topology_mask: 0,
            radius: 50.,
            fade: 10.,
            auto_cliff_splat: false,
            auto_cliff_topology: false,
            remove_existing_topology: false,
        });
        catalog.entries.insert(path, asset);
        let pixels = [0, 0, 0, 255];
        let pixel_path = dir.path().join(format!("textures/{guid}.rgba"));
        std::fs::write(&pixel_path, pixels).unwrap();
        doc["textures"][&guid]["sha256"] = format!("{:x}", Sha256::digest(pixels)).into();
        std::fs::write(&receipt, serde_json::to_vec(&doc).unwrap()).unwrap();
        let bindings = Bindings::load(dir.path(), &receipt, &catalog, || false).unwrap();
        let (hr, sr) = dimensions(project.size).unwrap();
        let heights: Vec<_> = (0..hr * hr)
            .map(|i| encode(100. + ((i % hr) as f32 / (hr - 1) as f32 * 1000. - 500.) * 0.3))
            .collect();
        let legacy = placement_preview::solve(&project, &heights, hr, || false)
            .unwrap()
            .unwrap();
        assert_eq!(legacy.required_missing, 1);
        project.placement_engine = placement_preview::EARTHWORK_VERSION.into();
        let layers = Layers {
            size: project.size,
            terrain_seed: project.seeds[0].value,
            biome_seed: project.seeds[1].value,
            height_resolution: hr,
            surface_resolution: sr,
            height: heights,
            biome: vec![],
            splat: vec![],
            topology: vec![],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        let original = layers.height.clone();
        let result =
            placement_preview::solve_native(&project, &layers, &catalog, &bindings, || false)
                .unwrap()
                .unwrap();
        assert_eq!(result.required_missing, 0);
        let pose = result.outcomes[0].position.as_ref().unwrap();
        assert_eq!([pose.x, pose.z, pose.yaw], [0., 0., 37.]);
        assert_eq!(
            result,
            placement_preview::solve_native(&project, &layers, &catalog, &bindings, || false)
                .unwrap()
                .unwrap()
        );
        assert_eq!(layers.height, original);
        // A new search policy must still reproduce an explicit accepted pose.
        project.placement_engine = placement_preview::BALANCED_EARTHWORK_VERSION.into();
        assert_eq!(
            result,
            placement_preview::solve_native(&project, &layers, &catalog, &bindings, || false)
                .unwrap()
                .unwrap()
        );
        let profile_path = dir.path().join("balanced-search.rec");
        crate::profile::Profile::new("Balanced search".into(), project.clone())
            .save(&profile_path)
            .unwrap();
        assert_eq!(
            crate::profile::Profile::load(&profile_path).unwrap().recipe,
            {
                let mut current = project.clone();
                crate::generation_defaults::apply(&mut current);
                current
            }
        );
        catalog
            .entries
            .values_mut()
            .for_each(|asset| asset.priority = Some(4));
        project.placement_engine = placement_preview::BOUNDS_EARTHWORK_VERSION.into();
        assert_eq!(
            result,
            placement_preview::solve_native(&project, &layers, &catalog, &bindings, || false)
                .unwrap()
                .unwrap()
        );
        crate::profile::Profile::new("Source bounds".into(), project.clone())
            .save(&profile_path)
            .unwrap();
        assert_eq!(
            crate::profile::Profile::load(&profile_path).unwrap().recipe,
            {
                let mut current = project.clone();
                crate::generation_defaults::apply(&mut current);
                current
            }
        );
        // An installed monument unknown to the compiled roster is selectable and searchable.
        // Retained v1 recipes must keep their old roster rather than silently changing output.
        project.placement_engine = placement_preview::LEGACY_EARTHWORK_VERSION.into();
        assert_eq!(
            result,
            placement_preview::solve_native(&project, &layers, &catalog, &bindings, || false)
                .unwrap()
                .unwrap()
        );
        let old_path = project.targets[0].prefab_path.take().unwrap();
        let new_path =
            "assets/bundled/prefabs/autospawn/monument/future/new_installation_monument.prefab"
                .to_string();
        let asset = catalog.entries.remove(&old_path).unwrap();
        catalog.entries.insert(new_path.clone(), asset);
        project.targets[0].prefab_path = Some(new_path.clone());
        let legacy =
            placement_preview::solve_native(&project, &layers, &catalog, &bindings, || false)
                .unwrap()
                .unwrap();
        assert_eq!(legacy.required_missing, 1);
        project.placement_engine = placement_preview::EARTHWORK_VERSION.into();
        assert_eq!(
            placement_preview::project_templates(&project, Some(&catalog))[0].path,
            new_path
        );
        assert!(placement_preview::project_templates(&project, None).is_empty());
        let source = project.installation.clone();
        project.installation.as_mut().unwrap().native.sha256 = "f".repeat(64);
        assert!(placement_preview::project_templates(&project, Some(&catalog)).is_empty());
        project.installation = source;
        let installed =
            placement_preview::solve_native(&project, &layers, &catalog, &bindings, || false)
                .unwrap()
                .unwrap();
        assert_eq!(installed, result);
        let bounds = catalog.entries.get_mut(&new_path).unwrap().bounds.take();
        assert!(placement_preview::project_templates(&project, Some(&catalog)).is_empty());
        catalog.entries.get_mut(&new_path).unwrap().bounds = bounds;
        let previous = project.clone();
        let mut extra = catalog.entries[&new_path].clone();
        for name in ["height", "splat0", "splat1", "blend"] {
            extra.texture_guids.insert(name.into(), guid.clone());
        }
        let extra_path = "assets/bundled/prefabs/autospawn/monument/future/second.prefab";
        catalog.entries.insert(extra_path.into(), extra);
        assert_eq!(
            placement_preview::populate_targets(&mut project, &catalog).unwrap(),
            1
        );
        assert_eq!(project.targets[0], previous.targets[0]);
        assert_eq!(project.terrain_key(), previous.terrain_key());
        assert_eq!(project.targets[1].presence, crate::Presence::Allowed);
        assert_eq!(
            placement_preview::populate_targets(&mut project, &catalog).unwrap(),
            0
        );
        // Adding catalog targets must not silently replace installed coastal
        // checks with the older land-only bounds search, including a no-op add.
        for engine in [
            crate::monument_source::VERSION,
            crate::coastal_placement::VERSION,
            crate::coastal_placement::BUDGET_VERSION,
        ] {
            let mut checked = project.clone();
            checked.placement_engine = engine.into();
            checked.monument_assets = Some(crate::AssetIdentity {
                catalog_sha256: catalog.sha256.clone(),
                bindings_sha256: bindings.sha256.clone(),
            });
            let coastal = crate::coastal_placement::supported(engine);
            let sized = engine == crate::coastal_placement::BUDGET_VERSION;
            checked.monument_rules = Some(crate::monument_source::Recipe {
                engine: engine.into(),
                source_sha256: "f".repeat(64),
                coastal_profile: coastal.then(|| {
                    serde_json::from_str(include_str!(
                        "../../../compatibility/monuments/client-0f703d43-coastal.json"
                    ))
                    .unwrap()
                }),
                tier_search: sized.then(|| crate::monument_source::TIER_RETRY_VERSION.into()),
                earthwork: sized.then(|| crate::monument_source::AUTHORED_EARTHWORK_VERSION.into()),
                coastal_normals: sized.then(|| papyrus_terrain::terrain_normal::VERSION.into()),
                coastal_navigation: None,
                coastal_search: None,
                canonical_yaw: None,
                land_alignment: None,
                surface_modifiers: None,
                stage_rules: None,
                dungeon_source: None, offshore_source: None,
            });
            checked.validate().unwrap();
            let before = checked.clone();
            assert_eq!(
                placement_preview::populate_targets(&mut checked, &catalog).unwrap(),
                0
            );
            assert_eq!(checked, before, "No-op roster changed {engine}");
            checked
                .targets
                .retain(|t| t.prefab_path.as_deref() != Some(extra_path));
            let existing = checked.targets.clone();
            assert_eq!(
                placement_preview::populate_targets(&mut checked, &catalog).unwrap(),
                1
            );
            assert_eq!(checked.placement_engine, before.placement_engine);
            assert_eq!(checked.monument_rules, before.monument_rules);
            assert_eq!(checked.monument_assets, before.monument_assets);
            assert_eq!(checked.terrain_key(), before.terrain_key());
            assert_eq!(&checked.targets[..existing.len()], &existing);
            checked.monument_assets.as_mut().unwrap().catalog_sha256 = "e".repeat(64);
            let mismatch = checked.clone();
            assert!(placement_preview::populate_targets(&mut checked, &catalog).is_err());
            assert_eq!(
                checked, mismatch,
                "Mismatched catalog partially changed the roster"
            );
        }
        project = previous;
        catalog.entries.remove(extra_path);
        assert!(
            placement_preview::solve_native(&project, &layers, &catalog, &bindings, || true)
                .unwrap()
                .is_none()
        );
        assert!(placement_preview::solve(&project, &layers.height, hr, || false).is_err());
        std::fs::write(pixel_path, [9, 9, 9, 9]).unwrap();
        let rejected =
            placement_preview::solve_native(&project, &layers, &catalog, &bindings, || false)
                .unwrap()
                .unwrap();
        assert_eq!(rejected.required_missing, 1);
        assert!(rejected.outcomes[0].reason.contains("changed"));
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    format: String,
    schema: u32,
    adapter: String,
    catalog_sha256: String,
    sources: Vec<SourceFile>,
    pixel_format: String,
    textures: BTreeMap<String, Texture>,
    decoded_bytes: u64,
}
pub struct Bindings {
    installation_root: PathBuf,
    pub sha256: String,
    textures: BTreeMap<String, Texture>,
    root: PathBuf,
    catalog_sha256: String,
    sources: Vec<SourceFile>,
}
pub(crate) const DAMAGED_PAYLOAD: &str = "Texture cache pixels changed; rebuild bindings";

pub(crate) enum PayloadError {
    Damaged,
    Other(String),
}
impl From<PayloadError> for String {
    fn from(error: PayloadError) -> Self {
        match error {
            PayloadError::Damaged => DAMAGED_PAYLOAD.into(),
            PayloadError::Other(message) => message,
        }
    }
}
impl From<std::io::Error> for PayloadError {
    fn from(error: std::io::Error) -> Self {
        if error.kind() == std::io::ErrorKind::NotFound {
            Self::Damaged
        } else {
            Self::Other(error.to_string())
        }
    }
}
fn hash(s: &str, n: usize) -> bool {
    s.len() == n
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(crate) const SOURCE_BUNDLES: [&str; 8] = [
    "Bundles/shared/content.bundle",
    "Bundles/shared/monuments.bundle",
    "Bundles/maps/maps.bundle",
    "Bundles/shared/textures.0.bundle",
    "Bundles/shared/textures.1.bundle",
    "Bundles/shared/textures.2.bundle",
    "Bundles/shared/textures.3.bundle",
    "Bundles/shared/textures.4.bundle",
];
fn supported_bundle(path: &str) -> bool {
    SOURCE_BUNDLES.contains(&path)
}

#[cfg(any(windows, test))]
pub(crate) const SOURCE_PLAN_LIMIT: u64 = 8 * 1024 * 1024;

#[cfg(any(windows, test))]
pub(crate) fn source_plan(raw: &[u8], catalog_sha256: &str) -> Result<Vec<String>, String> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Plan {
        format: String,
        schema: u32,
        catalog_sha256: String,
        bundles: Vec<String>,
        paths: Option<BTreeMap<String, String>>,
        content_has_textures: Option<bool>,
    }
    if raw.len() as u64 > SOURCE_PLAN_LIMIT {
        return Err("Texture source plan exceeds its budget".into());
    }
    let plan: Plan = serde_json::from_slice(raw).map_err(|e| e.to_string())?;
    let schema_valid = match (plan.schema, plan.paths, plan.content_has_textures) {
        (1, None, None) => raw.len() <= 4096,
        (2, Some(paths), Some(_)) => {
            (1..=4096).contains(&paths.len())
                && paths.iter().all(|(guid, path)| {
                    guid.len() == 32
                        && guid
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                        && path.len() <= 1024
                        && path.starts_with("assets/")
                        && !path.contains(['\\', '\0'])
                        && !path.split('/').any(|part| matches!(part, "" | "." | ".."))
                })
        }
        _ => false,
    };
    if plan.format != "mapper-texture-sources"
        || !schema_valid
        || plan.catalog_sha256 != catalog_sha256
        || !(2..=8).contains(&plan.bundles.len())
        || plan.bundles[0] != "Bundles/shared/content.bundle"
        || plan.bundles[1] != "Bundles/shared/monuments.bundle"
        || plan.bundles.iter().any(|path| !supported_bundle(path))
        || plan.bundles.iter().collect::<BTreeSet<_>>().len() != plan.bundles.len()
    {
        return Err("Invalid texture source plan".into());
    }
    Ok(plan.bundles)
}
fn read(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut result = Vec::new();
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut result)
        .map_err(|e| e.to_string())?;
    if result.len() as u64 > limit {
        return Err("Texture cache file exceeds inspection budget".into());
    }
    Ok(result)
}
impl Bindings {
    pub fn source_receipts(&self) -> &[SourceFile] {
        &self.sources
    }
    pub fn installation_root(&self) -> &Path {
        &self.installation_root
    }
    pub fn catalog_sha256(&self) -> &str {
        &self.catalog_sha256
    }
    pub fn sources(&self) -> &[SourceFile] {
        &self.sources
    }
    pub fn textures(&self) -> &BTreeMap<String, Texture> {
        &self.textures
    }
    /// This does not keep every decoded texture in memory. Individual reads check SHA again.
    pub fn load(
        root: &Path,
        receipt: &Path,
        catalog: &Catalog,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        let result = Self::load_manifest(root, receipt, catalog, &cancelled)?;
        result.verify_payloads(cancelled)?;
        Ok(result)
    }

    pub(crate) fn load_manifest(
        root: &Path,
        receipt: &Path,
        catalog: &Catalog,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        let raw = read(receipt, 4 * 1024 * 1024)?;
        Self::load_manifest_document(root, receipt, &raw, catalog, cancelled)
    }

    pub(crate) fn load_manifest_document(
        root: &Path,
        receipt: &Path,
        raw: &[u8],
        catalog: &Catalog,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        catalog.installation.validate()?;
        if raw.len() > 4 * 1024 * 1024 {
            return Err("Texture bindings exceed 4 MiB".into());
        }
        let document: Document = serde_json::from_slice(raw).map_err(|e| e.to_string())?;
        if document.format != "papyrus-prefab-textures"
            || document.schema != 1
            || !matches!(
                document.adapter.as_str(),
                "papyrus-guid-rgba-1" | "papyrus-guid-rgba-zlib-2"
            )
            || document.pixel_format != "RGBA8-top-left"
            || document.catalog_sha256 != catalog.sha256
            || !(5..=11).contains(&document.sources.len())
            || catalog.sources.len() != 3
            || document.textures.len() > 4096
        {
            return Err("Unsupported texture bindings or catalog mismatch".into());
        }
        let mut expected = catalog
            .sources
            .iter()
            .map(|s| (s.relative_path.as_str(), Some(s)))
            .collect::<BTreeMap<_, _>>();
        expected.insert("Bundles/shared/content.bundle", None);
        expected.insert("Bundles/shared/monuments.bundle", None);
        for source in &document.sources {
            if supported_bundle(&source.relative_path) {
                expected
                    .entry(source.relative_path.as_str())
                    .or_insert(None);
            }
        }
        if expected.len() != document.sources.len() {
            return Err("Invalid catalog source set".into());
        }
        for (path, original) in expected {
            let matches: Vec<_> = document
                .sources
                .iter()
                .filter(|s| s.relative_path == path)
                .collect();
            if matches.len() != 1 || !hash(&matches[0].sha256, 64) || matches[0].bytes == 0 {
                return Err("Invalid texture source set".into());
            }
            let source = matches[0];
            if original.is_some_and(|s| s != source) {
                return Err("Texture source differs from prefab catalog".into());
            }
            // Only fixed installation paths and the three explicitly supported texture bundles.
            let limit = if path == catalog.installation.native.relative_path {
                64 * 1024 * 1024
            } else if path == "Bundles/AssetSceneManifest.json" {
                32 * 1024 * 1024
            } else if path == "Bundles/shared/assetscenes.bundle" {
                2 * 1024 * 1024 * 1024
            } else if supported_bundle(path) {
                8 * 1024 * 1024 * 1024
            } else {
                return Err("Unexpected texture source path".into());
            };
            if installation::hash_file(root, path, limit, &cancelled)? != *source {
                return Err(format!("Texture source changed: {path}"));
            }
        }
        let required: BTreeSet<_> = catalog
            .entries
            .values()
            .flat_map(|a| a.texture_guids.values().cloned())
            .collect();
        if required.is_empty()
            || document.textures.keys().cloned().collect::<BTreeSet<_>>() != required
        {
            return Err("Texture bindings must resolve every catalog GUID exactly once".into());
        }
        let mut total = 0_u64;
        let compressed = document.adapter == "papyrus-guid-rgba-zlib-2";
        let mut stored_total = 0_u64;
        for (guid, t) in &document.textures {
            if t.payload_sha256.as_ref().is_some_and(|s| !hash(s, 64))
                || !hash(guid, 32)
                || !hash(&t.sha256, 64)
                || !(1..=4096).contains(&t.width)
                || !(1..=4096).contains(&t.height)
                || t.bytes != (t.width * t.height * 4) as u64
                || t.rgba_path
                    != if compressed {
                        format!("textures/{guid}.rgba.zlib")
                    } else {
                        format!("textures/{guid}.rgba")
                    }
                || compressed != t.storage.is_some()
                || !supported_bundle(&t.bundle_relative_path)
                || !document
                    .sources
                    .iter()
                    .any(|s| s.relative_path == t.bundle_relative_path)
                || t.asset_path.len() > 1024
                || !t.asset_path.starts_with("assets/")
                || t.asset_path.contains(['\\', '\0'])
                || t.asset_path
                    .split('/')
                    .any(|p| p.is_empty() || p == "." || p == "..")
            {
                return Err("Invalid GUID texture descriptor".into());
            }
            total += t.bytes;
            if let Some(storage) = &t.storage {
                if storage.format != "zlib"
                    || storage.bytes == 0
                    || storage.bytes > t.bytes + 65536
                    || !hash(&storage.sha256, 64)
                {
                    return Err("Invalid compressed texture storage".into());
                }
                stored_total += storage.bytes;
            } else {
                stored_total += t.bytes;
            }
            if total
                > if compressed {
                    4 * 1024 * 1024 * 1024
                } else {
                    1024 * 1024 * 1024
                }
                || stored_total > 1024 * 1024 * 1024
            {
                return Err("Texture cache exceeds its decoded or stored budget".into());
            }
        }
        if total != document.decoded_bytes {
            return Err("Texture cache byte count mismatch".into());
        }
        Ok(Self {
            catalog_sha256: catalog.sha256.clone(),
            sha256: format!("{:x}", Sha256::digest(raw)),
            textures: document.textures,
            sources: document.sources,
            installation_root: root.to_owned(),
            root: receipt
                .parent()
                .ok_or("Invalid receipt path")?
                .canonicalize()
                .map_err(|e| e.to_string())?,
        })
    }

    pub(crate) fn verify_payloads(&self, cancelled: impl Fn() -> bool) -> Result<(), PayloadError> {
        self.verify_stored_payloads(cancelled, false).map(|_| ())
    }

    /// A directory move preserves payload paths only if they did not resolve
    /// through aliases. Still verify aliased payloads for the normal load path.
    pub(crate) fn verify_staged_payloads(
        &self,
        cancelled: impl Fn() -> bool,
    ) -> Result<bool, PayloadError> {
        self.verify_stored_payloads(cancelled, true)
    }

    fn verify_stored_payloads(
        &self,
        cancelled: impl Fn() -> bool,
        check_paths: bool,
    ) -> Result<bool, PayloadError> {
        let total: u64 = self
            .textures
            .values()
            .map(|texture| texture.stored_receipt().0)
            .sum();
        let workers = crate::generation_resources::current()
            .effective_workers()
            .min(4)
            .min((total / (16 * 1024 * 1024)) as usize)
            .min(self.textures.len());
        if workers < 2 {
            return self.verify_payload_range(self.textures.values(), &cancelled, check_paths);
        }
        self.verify_payloads_in_batches(workers, total, &cancelled, check_paths)
    }

    fn verify_payloads_in_batches(
        &self,
        workers: usize,
        total: u64,
        cancelled: &impl Fn() -> bool,
        check_paths: bool,
    ) -> Result<bool, PayloadError> {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;

        // Keep receipt order within contiguous batches so multiple errors
        // still report the first failing file in the same order as serial use.
        let mut batches = Vec::with_capacity(workers);
        let mut start = 0;
        let mut remaining_bytes = total;
        for groups in (2..=workers).rev() {
            let target = remaining_bytes.div_ceil(groups as u64);
            let mut bytes = 0;
            let count = self
                .textures
                .values()
                .skip(start)
                .take(self.textures.len() - start - (groups - 1))
                .take_while(|texture| {
                    if bytes >= target {
                        return false;
                    }
                    bytes += texture.stored_receipt().0;
                    true
                })
                .count();
            batches.push(start..start + count);
            start += count;
            remaining_bytes -= bytes;
        }
        batches.push(start..self.textures.len());
        let textures = |range: std::ops::Range<usize>| {
            self.textures.values().skip(range.start).take(range.len())
        };
        let stop = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let stop = &stop;
            let mut pending = Vec::with_capacity(workers - 1);
            let mut batches = batches.into_iter();
            let first = batches.next().expect("at least one verification batch");
            for range in batches {
                let batch = textures(range.clone());
                let worker = std::thread::Builder::new()
                    .name("mapper-texture-verification".into())
                    .spawn_scoped(scope, move || {
                        self.verify_payload_range(
                            batch,
                            &|| stop.load(Ordering::Relaxed),
                            check_paths,
                        )
                    })
                    .ok();
                pending.push((range, worker));
            }
            let mut result = self.verify_payload_range(textures(first), cancelled, check_paths);
            for (range, worker) in pending {
                if result.is_err() {
                    stop.store(true, Ordering::Relaxed);
                }
                let next = if let Some(worker) = worker {
                    while !worker.is_finished() {
                        if cancelled() {
                            stop.store(true, Ordering::Relaxed);
                        }
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    match worker.join() {
                        Ok(verified) => verified,
                        Err(_) => Err(PayloadError::Other(
                            "Texture verification worker stopped".into(),
                        )),
                    }
                } else if result.is_ok() {
                    self.verify_payload_range(textures(range), cancelled, check_paths)
                } else {
                    continue;
                };
                result = result.and_then(|relocatable| {
                    if cancelled() {
                        return Err(PayloadError::Other("Texture verification cancelled".into()));
                    }
                    let verified = next?;
                    Ok(relocatable && verified)
                });
            }
            result
        })
    }

    fn verify_payload_range<'a>(
        &self,
        textures: impl Iterator<Item = &'a Texture>,
        cancelled: &impl Fn() -> bool,
        check_paths: bool,
    ) -> Result<bool, PayloadError> {
        let mut buffer = vec![0; 64 * 1024];
        let mut relocatable = true;
        for texture in textures {
            if cancelled() {
                return Err(PayloadError::Other("Texture verification cancelled".into()));
            }
            // Compressed payloads are checked on disk here, then expanded and
            // pixel-verified only when a stamp actually needs them.
            let path = self.stored_path(texture)?;
            let (size, sha) = texture.stored_receipt();
            let mut file = File::open(&path)?.take(size + 1);
            let mut digest = Sha256::new();
            loop {
                if cancelled() {
                    return Err(PayloadError::Other("Texture verification cancelled".into()));
                }
                let count = match file.read(&mut buffer) {
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    result => result?,
                };
                if count == 0 {
                    break;
                }
                digest.update(&buffer[..count]);
            }
            if file.limit() != 1 || format!("{:x}", digest.finalize()) != sha {
                return Err(PayloadError::Damaged);
            }
            if check_paths {
                relocatable &= path == self.root.join(&texture.rgba_path);
            }
        }
        Ok(relocatable)
    }

    pub(crate) fn use_published_root(&mut self, receipt: &Path) -> Result<bool, String> {
        let root = receipt.parent().ok_or("Missing texture cache directory")?;
        let direct_root = root
            .parent()
            .ok_or("Missing texture cache parent")?
            .canonicalize()
            .map_err(|error| error.to_string())?
            .join(root.file_name().ok_or("Invalid texture cache directory")?);
        let canonical = root.canonicalize().map_err(|error| error.to_string())?;
        if canonical != direct_root {
            return Ok(false);
        }
        self.root = canonical;
        Ok(true)
    }

    /// Restore only payloads that fail their saved storage receipts.
    pub(crate) fn repair_from(
        &self,
        replacement: &Self,
        cancelled: impl Fn() -> bool,
    ) -> Result<(), String> {
        if self.repair_from_available(replacement, cancelled)? {
            Ok(())
        } else {
            Err(DAMAGED_PAYLOAD.into())
        }
    }

    /// Accept any exact payload the donor has, and report if anything still
    /// needs to be restored. The caller may try another archived snapshot.
    pub(crate) fn repair_from_available(
        &self,
        replacement: &Self,
        cancelled: impl Fn() -> bool,
    ) -> Result<bool, String> {
        use std::io::Write;
        if self.sha256 != replacement.sha256 || self.catalog_sha256 != replacement.catalog_sha256 {
            return Err("Rebuilt texture bindings differ from the saved cache".into());
        }
        let directory = self.root.join("textures");
        if !crate::client_cache::ordinary(&directory)? {
            std::fs::create_dir(&directory).or_else(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    Ok(())
                } else {
                    Err(error)
                }
            }).map_err(|error| error.to_string())?;
        }
        if directory
            .canonicalize()
            .map_err(|error| error.to_string())?
            != directory
        {
            return Err("Texture repair path escapes the cache directory".into());
        }
        let mut bytes = Vec::new();
        let mut complete = true;
        for texture in self.textures.values() {
            if cancelled() {
                return Err("Texture repair cancelled".into());
            }
            match self.read_stored_into(texture, &mut bytes) {
                Ok(_) => continue,
                Err(PayloadError::Damaged) => {}
                Err(error) => return Err(error.into()),
            }
            if replacement.read_stored_into(texture, &mut bytes).is_err() {
                complete = false;
                continue;
            }
            let mut temporary =
                tempfile::NamedTempFile::new_in(&directory).map_err(|error| error.to_string())?;
            temporary
                .write_all(&bytes)
                .map_err(|error| error.to_string())?;
            temporary
                .as_file()
                .sync_all()
                .map_err(|error| error.to_string())?;
            if cancelled() {
                return Err("Texture repair cancelled".into());
            }
            temporary
                .persist(self.root.join(&texture.rgba_path))
                .map_err(|error| error.to_string())?;
            self.read_stored_into(texture, &mut bytes)?;
        }
        Ok(complete)
    }
    pub fn read_rgba(&self, guid: &str) -> Result<Vec<u8>, String> {
        let texture = self
            .textures
            .get(guid)
            .ok_or("Unresolved terrain texture GUID")?;
        let mut encoded = Vec::new();
        self.read_stored_into(texture, &mut encoded)?;
        if texture.storage.is_none() {
            // The stored receipt is the decoded receipt for raw RGBA.
            return Ok(encoded);
        }
        let mut decoded = Vec::new();
        decoded.try_reserve_exact(texture.bytes as usize + 1).map_err(|e| e.to_string())?;
        let mut decoder = flate2::Decompress::new(true);
        let status = decoder
            .decompress_vec(&encoded, &mut decoded, flate2::FlushDecompress::Finish)
            .map_err(|e| format!("Invalid compressed texture: {e}"))?;
        if status != flate2::Status::StreamEnd {
            return Err(DAMAGED_PAYLOAD.into());
        }
        if decoder.total_in() != encoded.len() as u64 {
            return Err("Compressed texture has trailing data".into());
        }
        if decoded.len() as u64 != texture.bytes
            || format!("{:x}", Sha256::digest(&decoded)) != texture.sha256
        {
            return Err(DAMAGED_PAYLOAD.into());
        }
        Ok(decoded)
    }

    pub(crate) fn verify_stored_rgba(&self, texture: &Texture) -> Result<(), String> {
        self.read_stored_into(texture, &mut Vec::new())?;
        Ok(())
    }

    fn read_stored_into(
        &self,
        texture: &Texture,
        bytes: &mut Vec<u8>,
    ) -> Result<PathBuf, PayloadError> {
        let path = self.stored_path(texture)?;
        let (size, sha) = texture.stored_receipt();
        bytes.clear();
        File::open(&path)?.take(size + 1).read_to_end(bytes)?;
        if bytes.len() as u64 != size || format!("{:x}", Sha256::digest(bytes)) != sha {
            return Err(PayloadError::Damaged);
        }
        Ok(path)
    }

    fn stored_path(&self, texture: &Texture) -> Result<PathBuf, PayloadError> {
        let path = self.root.join(&texture.rgba_path).canonicalize()?;
        if !path.starts_with(&self.root) {
            return Err(PayloadError::Other(
                "Texture path escapes verified cache".into(),
            ));
        }
        Ok(path)
    }
}

impl Texture {
    pub(crate) fn has_same_compressed_pixels(&self, other: &Self) -> bool {
        self.bytes == other.bytes
            && self.sha256 == other.sha256
            && self.storage.as_ref().zip(other.storage.as_ref()).is_some_and(|(a, b)| {
                a.bytes == b.bytes && a.sha256 == b.sha256 && a.format == b.format
            })
    }

    fn stored_receipt(&self) -> (u64, &str) {
        self.storage
            .as_ref()
            .map_or((self.bytes, self.sha256.as_str()), |storage| {
                (storage.bytes, storage.sha256.as_str())
            })
    }
}
