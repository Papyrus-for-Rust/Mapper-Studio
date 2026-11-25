use mapper_studio::{
    installation,
    prefab_assets::{Asset, Catalog},
    prefab_textures::Bindings,
};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path, sync::Arc};

pub fn load(root: &Path) -> (Arc<Catalog>, Arc<Bindings>) {
    let mut sources = Vec::new();
    for path in [
        "RustClient_Data/Plugins/x86_64/RustNative.dll",
        "Bundles/AssetSceneManifest.json",
        "Bundles/shared/assetscenes.bundle",
        "Bundles/shared/content.bundle",
        "Bundles/shared/monuments.bundle",
    ] {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, path.as_bytes()).unwrap();
        sources.push(installation::SourceFile {
            relative_path: path.into(),
            sha256: format!("{:x}", Sha256::digest(path.as_bytes())),
            bytes: path.len() as u64,
        });
    }
    let installation = installation::Installation {
        kind: installation::InstallationKind::Client,
        native: sources[0].clone(),
        scene_manifest: Some(sources[1].clone()),
    };
    let guid = "a".repeat(32);
    let catalog = Arc::new(Catalog {
        sha256: "b".repeat(64),
        installation: installation.clone(),
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
    });
    let pixels = [1_u8, 2, 3, 255];
    std::fs::create_dir(root.join("textures")).unwrap();
    std::fs::write(root.join(format!("textures/{guid}.rgba")), pixels).unwrap();
    let document = serde_json::json!({
        "format":"papyrus-prefab-textures", "schema":1,
        "adapter":"papyrus-guid-rgba-1", "catalog_sha256":catalog.sha256,
        "sources":sources, "pixel_format":"RGBA8-top-left", "decoded_bytes":4,
        "textures":{guid.clone():{
            "asset_path":"assets/test.png",
            "bundle_relative_path":"Bundles/shared/monuments.bundle",
            "rgba_path":format!("textures/{guid}.rgba"),
            "width":1, "height":1, "bytes":4,
            "sha256":format!("{:x}", Sha256::digest(pixels))
        }}
    });
    let receipt = root.join("bindings.json");
    std::fs::write(&receipt, serde_json::to_vec(&document).unwrap()).unwrap();
    let bindings = Arc::new(Bindings::load(root, &receipt, &catalog, || false).unwrap());
    (catalog, bindings)
}
