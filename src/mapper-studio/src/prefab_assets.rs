//! Source-bound numeric asset metadata. This does not resolve GUIDs into texture pixels.
use crate::installation::{self, Installation, SourceFile};
use serde::Deserialize;
use serde_json::{Value, value::RawValue};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    io::Read,
    path::Path,
};

pub const FORMAT: &str = "papyrus-prefab-catalog";
pub const ADAPTER: &str = "papyrus-unity-guid-fields-1";
pub const WATER_ADAPTER: &str = "papyrus-unity-guid-fields-water-2";
pub const DISCOVERY_ADAPTER: &str = "papyrus-unity-autospawn-fields-water-3";
pub const BUILDING_ADAPTER: &str = "papyrus-unity-autospawn-fields-building-4";
const MAX_CATALOG: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct Bounds {
    pub center: [f32; 3],
    pub extent: [f32; 3],
    pub minimum_world_size: u32,
}
impl Bounds {
    /// Conservative origin-centered footprint, including authored center displacement.
    pub fn horizontal_extent(&self) -> [f32; 2] {
        [
            self.center[0].abs() + self.extent[0],
            self.center[2].abs() + self.extent[2],
        ]
    }
}
#[derive(Clone, Debug)]
pub struct Asset {
    pub priority: Option<i32>,
    pub bounds: Option<Bounds>,
    pub yaw: Option<[f32; 2]>,
    pub modifier_size: Option<[f32; 3]>,
    pub modifier_offset: Option<[f32; 3]>,
    pub texture_guids: BTreeMap<String, String>,
    pub splat_mask: Option<u8>,
    pub modifier: Option<Modifier>,
    /// Explicit extractor evidence that this prefab has no terrain modifier.
    pub modifier_absent: bool,
    pub water_geometry: Option<crate::water_geometry::Report>,
    pub no_build_geometry: Option<crate::no_build::Report>,
    pub display_name: Option<String>,
}
impl Asset {
    /// Some authored prefabs carry a disabled, zero-sized TerrainModifier.
    /// They contribute their prefab placement without a terrain stamp.
    pub fn has_no_terrain_effects(&self) -> bool {
        self.has_no_modifier()
            || (self.splat_mask == Some(0)
                && self.texture_guids.is_empty()
                && self.modifier.as_ref().is_some_and(|modifier| {
                    !modifier.height_map
                        && modifier.water_map != Some(true)
                        && !modifier.alpha_map
                        && modifier.biome_mask == 0
                        && modifier.topology_mask == 0
                }))
    }
    pub fn has_no_modifier(&self) -> bool {
        self.modifier_absent
            && self.modifier.is_none()
            && self.modifier_size.is_none()
            && self.modifier_offset.is_none()
            && self.texture_guids.is_empty()
            && self.splat_mask.is_none()
    }
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Modifier {
    pub height_map: bool,
    #[serde(default)]
    pub water_map: Option<bool>,
    pub alpha_map: bool,
    pub biome_mask: i32,
    pub topology_mask: i32,
    pub radius: f32,
    pub fade: f32,
    pub auto_cliff_splat: bool,
    pub auto_cliff_topology: bool,
    pub remove_existing_topology: bool,
}
pub struct Catalog {
    pub sha256: String,
    pub installation: Installation,
    pub entries: HashMap<String, Asset>,
    pub sources: Vec<SourceFile>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document<'a> {
    format: String,
    schema: u32,
    adapter: String,
    extractor_sha256: String,
    sources: Vec<SourceFile>,
    #[serde(borrow)]
    metadata: Metadata<'a>,
}

#[derive(Deserialize)]
struct Metadata<'a> {
    format: u64,
    #[serde(borrow)]
    modifiers: BTreeMap<String, &'a RawValue>,
    #[serde(borrow)]
    special: BTreeMap<String, &'a RawValue>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssetFields {
    #[serde(default, deserialize_with = "present")]
    no_build_geometry: Option<Option<crate::no_build::Report>>,
    #[serde(default, deserialize_with = "present")]
    water_geometry: Option<Option<crate::water_geometry::Report>>,
    #[serde(flatten)]
    fields: serde_json::Map<String, Value>,
}

// Legacy adapter rules distinguish an absent field from an explicit null.
fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::deserialize(deserializer).map(Some)
}
fn valid_hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn vector(v: &Value) -> Result<[f32; 3], String> {
    let values = v
        .as_array()
        .filter(|a| a.len() == 3)
        .ok_or("Invalid asset vector")?;
    let mut result = [0.; 3];
    for (out, value) in result.iter_mut().zip(values) {
        *out = value.as_f64().ok_or("Invalid asset number")? as f32;
        if !out.is_finite() || out.abs() > 1_000_000. {
            return Err("Asset vector outside inspection bounds".into());
        }
    }
    Ok(result)
}
fn decode_asset(raw: &str, adapter: &str) -> Result<Asset, String> {
    let AssetFields {
        no_build_geometry,
        water_geometry,
        fields,
    } = serde_json::from_str(raw).map_err(|e| format!("Invalid prefab metadata: {e}"))?;
    if adapter == ADAPTER && water_geometry.is_some() {
        return Err("Legacy catalog cannot claim inspected water geometry".into());
    }
    if adapter != BUILDING_ADAPTER && no_build_geometry.is_some() {
        return Err("Legacy catalog cannot claim inspected building restrictions".into());
    }
    let value = &Value::Object(fields);
    let no_build_geometry = no_build_geometry.flatten();
    let water_geometry = water_geometry.flatten();
    if let Some(report) = &no_build_geometry {
        report.validate()?;
    }
    let bounds = if value["monumentBounds"].is_null() {
        None
    } else {
        let b = &value["monumentBounds"];
        let extent = vector(&b["extent"])?;
        if extent.iter().any(|v| *v < 0.) {
            return Err("Negative monument extent".into());
        }
        let minimum_world_size = b["minWorldSize"]
            .as_u64()
            .filter(|v| *v <= 100_000)
            .ok_or("Invalid minimum world size")? as u32;
        Some(Bounds {
            center: vector(&b["center"])?,
            extent,
            minimum_world_size,
        })
    };
    let yaw = if value["rotation"].is_null() {
        None
    } else {
        let min = vector(&value["rotation"]["min"])?[1];
        let max = vector(&value["rotation"]["max"])?[1];
        if min > max || max - min > 360. {
            return Err("Unsupported authored yaw range".into());
        }
        Some([min, max])
    };
    let mut texture_guids = BTreeMap::new();
    if let Some(guids) = value["textureGuids"].as_object() {
        for (name, guid) in guids {
            if ![
                "height", "alpha", "splat0", "splat1", "blend", "topology", "biome", "water",
            ]
            .contains(&name.as_str())
            {
                return Err("Unsupported texture reference channel".into());
            }
            if guid.is_null() {
                continue;
            }
            let guid = guid
                .as_str()
                .filter(|s| {
                    s.len() == 32
                        && s.bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
                .ok_or("Invalid terrain texture GUID")?;
            texture_guids.insert(name.clone(), guid.into());
        }
    }
    if let Some(report) = &water_geometry {
        report.validate()?;
    }
    let display_name = value
        .get("displayName")
        .filter(|v| !v.is_null())
        .map(|v| {
            v.as_str()
                .filter(|s| {
                    !s.trim().is_empty() && s.len() <= 256 && !s.chars().any(char::is_control)
                })
                .map(str::to_owned)
                .ok_or("Invalid monument display name")
        })
        .transpose()?;
    let modifier_absent = value["reason"] == "No terrain modifier component";
    if modifier_absent
        && [
            "heightMap",
            "waterMap",
            "alphaMap",
            "biomeMask",
            "topologyMask",
            "radius",
            "fade",
            "autoCliffSplat",
            "autoCliffTopology",
            "removeExistingTopology",
            "size",
            "offset",
            "textureGuids",
            "splatMask",
        ]
        .iter()
        .any(|key| value.get(*key).is_some_and(|v| !v.is_null()))
    {
        return Err("Contradictory terrain modifier absence metadata".into());
    }
    Ok(Asset {
        modifier_absent,
        display_name,
        priority: if value["placement"].is_null() {
            None
        } else {
            Some(
                i32::try_from(
                    value["placement"]["priority"]
                        .as_i64()
                        .ok_or("Invalid monument priority")?,
                )
                .map_err(|_| "Invalid monument priority")?,
            )
        },
        water_geometry,
        no_build_geometry,
        modifier: if value.get("heightMap").is_some() {
            let modifier = Modifier::deserialize(value)
                .map_err(|e| format!("Invalid modifier settings: {e}"))?;
            if !modifier.radius.is_finite()
                || !(0. ..=8192.).contains(&modifier.radius)
                || !modifier.fade.is_finite()
                || !(0. ..=8192.).contains(&modifier.fade)
            {
                return Err("Invalid modifier radius or fade".into());
            }
            Some(modifier)
        } else {
            None
        },
        splat_mask: if value["splatMask"].is_null() {
            None
        } else {
            Some(
                i32::try_from(value["splatMask"].as_i64().ok_or("Invalid splat mask")?)
                    .map_err(|_| "Invalid splat mask")? as u8,
            )
        },
        bounds,
        yaw,
        modifier_size: if value["size"].is_null() {
            None
        } else {
            Some(vector(&value["size"])?)
        },
        modifier_offset: if value["offset"].is_null() {
            None
        } else {
            Some(vector(&value["offset"])?)
        },
        texture_guids,
    })
}

pub fn load(
    root: &Path,
    path: &Path,
    expected: &Installation,
    cancelled: impl Fn() -> bool,
) -> Result<Catalog, String> {
    expected.validate()?;
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| format!("Cannot open prefab catalog: {e}"))?
        .take(MAX_CATALOG + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    load_document(root, &bytes, expected, cancelled)
}

pub(crate) fn load_document(
    root: &Path,
    bytes: &[u8],
    expected: &Installation,
    cancelled: impl Fn() -> bool,
) -> Result<Catalog, String> {
    expected.validate()?;
    if bytes.len() as u64 > MAX_CATALOG {
        return Err("Prefab catalog exceeds 16 MiB".into());
    }
    let document: Document<'_> =
        serde_json::from_slice(bytes).map_err(|e| format!("Invalid prefab catalog: {e}"))?;
    if document.format != FORMAT
        || document.schema != 1
        || !((document.adapter == ADAPTER && document.metadata.format == 4)
            || ([WATER_ADAPTER, DISCOVERY_ADAPTER, BUILDING_ADAPTER]
                .contains(&document.adapter.as_str())
                && document.metadata.format == 5))
        || !valid_hash(&document.extractor_sha256)
        || if document.adapter == BUILDING_ADAPTER {
            !(4..=6).contains(&document.sources.len())
        } else {
            document.sources.len() != 3
        }
    {
        return Err(
            "Unsupported or unversioned prefab catalog; rebuild from the selected installation"
                .into(),
        );
    }
    let manifest = expected
        .scene_manifest
        .as_ref()
        .ok_or("Prefab metadata requires a fingerprinted scene manifest")?;
    let manager = format!(
        "{}/globalgamemanagers",
        expected
            .native
            .relative_path
            .split('/')
            .next()
            .ok_or("Missing data directory")?
    );
    let scene_bundle = "Bundles/shared/assetscenes.bundle".to_string();
    let content_bundle = "Bundles/shared/content.bundle".to_string();
    let monument_bundle = "Bundles/shared/monuments.bundle".to_string();
    let mut required = vec![
        (&expected.native.relative_path, 64 * 1024 * 1024),
        (&manifest.relative_path, 32 * 1024 * 1024),
        (&scene_bundle, 2 * 1024 * 1024 * 1024),
    ];
    if document.adapter == BUILDING_ADAPTER {
        required.push((&manager, 32 * 1024 * 1024));
        for path in [&content_bundle, &monument_bundle] {
            if document.sources.iter().any(|s| &s.relative_path == path) {
                required.push((path, 8 * 1024 * 1024 * 1024));
            }
        }
        if required.len() != document.sources.len() {
            return Err("Unexpected restriction mesh source".into());
        }
    }
    for (relative, limit) in required {
        let matches: Vec<_> = document
            .sources
            .iter()
            .filter(|s| &s.relative_path == relative)
            .collect();
        if matches.len() != 1
            || !valid_hash(&matches[0].sha256)
            || matches[0].bytes == 0
            || matches[0].bytes > limit
        {
            return Err("Prefab catalog has invalid or unexpected source paths".into());
        }
        let source = matches[0];
        if (relative == &expected.native.relative_path && source != &expected.native)
            || (relative == &manifest.relative_path && source != manifest)
        {
            return Err("Prefab catalog belongs to a different installation".into());
        }
        let actual = installation::hash_file(root, relative, limit, &cancelled)?;
        if &actual != source {
            return Err(format!(
                "Prefab source changed: {relative}; rebuild the catalog"
            ));
        }
    }
    let mut entries = HashMap::new();
    for values in [document.metadata.modifiers, document.metadata.special] {
        if values.len() > 4096 {
            return Err("Too many prefab metadata entries".into());
        }
        for (path, value) in values {
            if cancelled() {
                return Err("Prefab catalog inspection cancelled".into());
            }
            if !path.starts_with("assets/bundled/prefabs/autospawn/")
                || !path.ends_with(".prefab")
                || path.len() > 512
                || path.contains(['\\', '\0'])
                || path.split('/').any(|s| s == ".." || s.is_empty())
            {
                return Err("Invalid prefab asset identifier".into());
            }
            if entries.insert(path, decode_asset(value.get(), &document.adapter)?).is_some() {
                return Err("Duplicate prefab asset metadata".into());
            }
        }
    }
    Ok(Catalog {
        sha256: format!("{:x}", Sha256::digest(bytes)),
        installation: expected.clone(),
        entries,
        sources: document.sources,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn asset(value: &Value) -> Result<Asset, String> {
        decode_asset(&value.to_string(), BUILDING_ADAPTER)
    }
    #[test]
    fn disabled_zero_sized_modifiers_have_no_terrain_effects() {
        let empty = serde_json::json!({
            "heightMap":false,"waterMap":false,"alphaMap":false,
            "biomeMask":0,"topologyMask":0,"splatMask":0,
            "radius":0.,"fade":10.,"size":[0.,0.,0.],"offset":[0.,0.,0.],
            "autoCliffSplat":true,"autoCliffTopology":true,"removeExistingTopology":false,
            "textureGuids":{}
        });
        let parsed = asset(&empty).unwrap();
        assert!(!parsed.has_no_modifier());
        assert!(parsed.has_no_terrain_effects());
        let mut legacy=empty.clone();
        legacy.as_object_mut().unwrap().remove("waterMap");
        assert!(asset(&legacy).unwrap().has_no_terrain_effects());
        legacy["textureGuids"]=serde_json::json!({"water":"a".repeat(32)});
        assert!(!asset(&legacy).unwrap().has_no_terrain_effects());
        assert!(!asset(&serde_json::json!({})).unwrap().has_no_terrain_effects());
        for (field, value) in [
            ("heightMap", serde_json::json!(true)),
            ("waterMap", serde_json::json!(true)),
            ("alphaMap", serde_json::json!(true)),
            ("biomeMask", serde_json::json!(1)),
            ("topologyMask", serde_json::json!(128)),
            ("splatMask", serde_json::json!(1)),
            ("textureGuids", serde_json::json!({"height":"a".repeat(32)})),
        ] {
            let mut changed = empty.clone();
            changed[field] = value;
            assert!(!asset(&changed).unwrap().has_no_terrain_effects(), "{field}");
        }
    }
    #[test]
    fn modifier_absence_requires_explicit_consistent_extractor_evidence() {
        let explicit = serde_json::json!({"reason":"No terrain modifier component"});
        assert!(asset(&explicit).unwrap().has_no_modifier());
        assert!(!asset(&serde_json::json!({})).unwrap().has_no_modifier());
        assert!(
            !asset(&serde_json::json!({"reason":"Unresolved component"}))
                .unwrap()
                .has_no_modifier()
        );
        for (field, value) in [
            ("heightMap", serde_json::json!(false)),
            ("alphaMap", serde_json::json!(true)),
            ("topologyMask", serde_json::json!(-1)),
            ("size", serde_json::json!([0, 0, 0])),
            ("textureGuids", serde_json::json!({"height":"a".repeat(32)})),
        ] {
            let mut contradictory = explicit.clone();
            contradictory[field] = value;
            assert!(asset(&contradictory).is_err(), "{field}");
        }
    }
    fn fixture(root: &Path) -> (Installation, Value) {
        let paths = [
            "RustClient_Data/Plugins/x86_64/RustNative.dll",
            "Bundles/AssetSceneManifest.json",
            "Bundles/shared/assetscenes.bundle",
        ];
        let mut sources = Vec::new();
        for path in paths {
            std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
            std::fs::write(root.join(path), path.as_bytes()).unwrap();
            sources.push(installation::hash_file(root, path, 1024, &|| false).unwrap());
        }
        let installation = Installation {
            kind: installation::InstallationKind::Client,
            native: sources[0].clone(),
            scene_manifest: Some(sources[1].clone()),
        };
        let document = serde_json::json!({"format":FORMAT,"schema":1,"adapter":ADAPTER,"extractor_sha256":"a".repeat(64),"sources":sources,
            "metadata":{"format":4,"modifiers":{},"special":{}}});
        (installation, document)
    }
    #[test]
    fn restriction_metadata_binds_the_installation_layer_table_and_rejects_stale_layers() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let (identity, mut document) = fixture(root);
        let manager = "RustClient_Data/globalgamemanagers";
        std::fs::write(root.join(manager), "layer table").unwrap();
        let receipt = installation::hash_file(root, manager, 1024, &|| false).unwrap();
        document["adapter"] = BUILDING_ADAPTER.into();
        document["metadata"]["format"] = 5.into();
        document["sources"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::to_value(receipt).unwrap());
        let key = "assets/bundled/prefabs/autospawn/monument/test.prefab";
        document["metadata"]["special"][key] = serde_json::json!({"noBuildGeometry":{"version":1,"space":"prefab-root-local-unity","layerIndex":29,"nodesVisited":1,"volumes":[{"objectId":3,"active":true,"geometry":{"shape":"sphere","center":[10,0,-20],"radius":50}}],"issues":[],"complete":true}});
        let path = root.join("catalog.json");
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        let catalog = load(root, &path, &identity, || false).unwrap();
        assert!(
            catalog.entries[key]
                .no_build_geometry
                .as_ref()
                .unwrap()
                .complete
        );
        std::fs::write(root.join(manager), "updated layers").unwrap();
        assert!(load(root, &path, &identity, || false).is_err());
    }
    #[test]
    fn source_binding_rejects_changed_bundle_unversioned_cache_and_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let (identity, mut document) = fixture(root);
        let path = root.join("catalog.json");
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        assert!(load(root, &path, &identity, || false).is_ok());
        assert!(load(root, &path, &identity, || true).is_err());
        std::fs::write(root.join("Bundles/shared/assetscenes.bundle"), "updated").unwrap();
        assert!(
            load(root, &path, &identity, || false)
                .err()
                .unwrap()
                .contains("source changed")
        );
        document["schema"] = 99.into();
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        assert!(
            load(root, &path, &identity, || false)
                .err()
                .unwrap()
                .contains("Unsupported")
        );
    }
    #[test]
    fn installation_catalog_accepts_new_families_and_rejects_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let (identity, mut document) = fixture(dir.path());
        document["adapter"] = DISCOVERY_ADAPTER.into();
        document["metadata"]["format"] = 5.into();
        let path = dir.path().join("catalog.json");
        for family in [
            "unique_environment/oasis/new",
            "tunnel-entrance/new",
            "future_family/new",
        ] {
            let key = format!("assets/bundled/prefabs/autospawn/{family}.prefab");
            document["metadata"]["special"][&key] =
                serde_json::json!({"reason":"No terrain modifier component"});
        }
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        let catalog = load(dir.path(), &path, &identity, || false).unwrap();
        assert!(
            catalog.entries.contains_key(
                "assets/bundled/prefabs/autospawn/unique_environment/oasis/new.prefab"
            )
        );
        document["metadata"]["special"]["assets/bundled/prefabs/autospawn/../escape.prefab"] =
            serde_json::json!({});
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        assert!(load(dir.path(), &path, &identity, || false).is_err());
    }
    #[test]
    fn duplicate_or_traversing_source_paths_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let (identity, mut document) = fixture(dir.path());
        let path = dir.path().join("catalog.json");
        document["sources"][2]["relative_path"] = "../outside.bundle".into();
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        assert!(load(dir.path(), &path, &identity, || false).is_err());
        document["sources"][2] = document["sources"][0].clone();
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        assert!(load(dir.path(), &path, &identity, || false).is_err());
    }
    #[test]
    fn water_metadata_requires_the_new_adapter_and_validates_before_acceptance() {
        let dir = tempfile::tempdir().unwrap();
        let (identity, mut document) = fixture(dir.path());
        let key = "assets/bundled/prefabs/autospawn/monument/test.prefab";
        document["adapter"] = WATER_ADAPTER.into();
        document["metadata"]["format"] = 5.into();
        document["metadata"]["special"][key] = serde_json::json!({"waterGeometry":{"version":1,"space":"prefab-root-local-unity","nodesVisited":1,"waterBodies":[],"contributors":[],"issues":[],"complete":true}});
        let path = dir.path().join("catalog.json");
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        let catalog = load(dir.path(), &path, &identity, || false).unwrap();
        assert!(
            catalog.entries[key]
                .water_geometry
                .as_ref()
                .unwrap()
                .complete
        );
        document["metadata"]["special"][key]["waterGeometry"]["version"] = 99.into();
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        assert!(load(dir.path(), &path, &identity, || false).is_err());
        document["adapter"] = ADAPTER.into();
        document["metadata"]["format"] = 4.into();
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        assert!(
            load(dir.path(), &path, &identity, || false)
                .err()
                .unwrap()
                .contains("Legacy")
        );
        document["metadata"]["special"][key]["waterGeometry"] = Value::Null;
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        assert!(
            load(dir.path(), &path, &identity, || false)
                .err()
                .unwrap()
                .contains("Legacy")
        );
    }
    #[test]
    #[ignore = "Requires an owned Rust installation and extracted presentation catalog"]
    fn owned_display_names_load_from_verified_client_catalog() {
        let root =
            std::path::PathBuf::from(std::env::var_os("MAPPER_NAMES_ROOT").expect("Rust folder"));
        let path = std::path::PathBuf::from(
            std::env::var_os("MAPPER_NAMES_CATALOG").expect("Names catalog"),
        );
        let _verification = crate::installation::VerificationScope::default();
        let source = crate::installation::inspect(&root, || false).unwrap();
        let catalog = load(&root, &path, &source, || false).unwrap();
        let compound = "assets/bundled/prefabs/autospawn/monument/medium/compound.prefab";
        let bandit = "assets/bundled/prefabs/autospawn/monument/medium/bandit_town.prefab";
        assert_eq!(
            catalog.entries[compound].display_name.as_deref(),
            Some("Outpost")
        );
        assert_eq!(
            catalog.entries[bandit].display_name.as_deref(),
            Some("Bandit Camp")
        );
        eprintln!(
            "{} named monuments / {} source entries",
            catalog
                .entries
                .values()
                .filter(|a| a.display_name.is_some())
                .count(),
            catalog.entries.len()
        );
    }
    #[test]
    fn installation_display_names_are_optional_bounded_and_never_rewrite_asset_identity() {
        assert!(
            asset(&serde_json::json!({}))
                .unwrap()
                .display_name
                .is_none()
        );
        assert_eq!(
            asset(&serde_json::json!({"displayName":"Outpost"}))
                .unwrap()
                .display_name
                .as_deref(),
            Some("Outpost")
        );
        for value in [
            serde_json::json!(""),
            serde_json::json!("Bad\nName"),
            serde_json::json!("x".repeat(257)),
            serde_json::json!(12),
        ] {
            assert!(asset(&serde_json::json!({"displayName":value})).is_err());
        }
    }
    #[test]
    fn bounds_include_center_and_guids_are_not_guessed_from_names() {
        let a = asset(&serde_json::json!({"monumentBounds": {"center": [-10,0,20], "extent": [50,30,60], "minWorldSize": 1000},
            "textureGuids":{"splat0":"5510809b33cbc465c9cf012140cf5713","blend":null}})).unwrap();
        assert_eq!(a.bounds.unwrap().horizontal_extent(), [60., 80.]);
        assert_eq!(a.texture_guids.len(), 1);
        assert!(asset(&serde_json::json!({"textureGuids":{"blend":"not-a-guid"}})).is_err());
        assert_eq!(
            asset(&serde_json::json!({"placement":{"priority":4,"count":1}}))
                .unwrap()
                .priority,
            Some(4)
        );
        assert!(asset(&serde_json::json!({"placement":{"priority":2.5,"count":1}})).is_err());
    }
}
