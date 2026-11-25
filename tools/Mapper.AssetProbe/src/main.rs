//! Isolated native catalog metadata reader and independent parity probe.
//! Source receipts and cache publication are owned by Mapper, never this process.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::Path,
    time::Instant,
};
mod geometry;
mod map_labels;
mod pages;
mod values;
use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, atomic::Ordering},
};
use unity_rs_core::{
    bundle::{BundleParseLimits, UnityFsBundle},
    scene::{SceneReadLimits, read_game_object},
    serialized::{SerializedFile, SerializedParseLimits},
    source::Region,
};

fn bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > maximum {
        return Err("Input exceeds its byte budget".into());
    }
    Ok(bytes)
}
fn object_fields(file: &SerializedFile, index: usize) -> Result<Value, String> {
    if file.objects[index].byte_size > 1024 * 1024 {
        return Err("Root component exceeds probe budget".into());
    }
    let fields = file
        .read_type_tree_value(index)
        .map_err(|e| e.to_string())?;
    values::into_value(fields)
}
fn vector(fields: &Value) -> Value {
    json!([fields["x"], fields["y"], fields["z"]])
}
fn geometry_record(file: &SerializedFile, index: usize) -> Result<(i32, Value), String> {
    let kind = file.objects[index].class_id;
    let fields = if [1, 4, 224, 114, 64, 65, 135, 136, 154].contains(&kind) {
        if file.objects[index].byte_size > 1024 * 1024 {
            return Err("Geometry component exceeds probe budget".into());
        }
        let mut fields = file
            .read_type_tree_value(index)
            .map_err(|e| e.to_string())?;
        if let unity_rs_core::type_tree::TypeValue::Object(values) = &mut fields {
            values.retain(|field| match kind {
                1 => ["m_Component", "m_IsActive"].contains(&field.name.as_str()),
                4 | 224 => [
                    "m_GameObject",
                    "m_Children",
                    "m_LocalPosition",
                    "m_LocalScale",
                    "m_LocalRotation",
                ]
                .contains(&field.name.as_str()),
                114 => [
                    "m_Enabled",
                    "Type",
                    "Triggers",
                    "IsOcean",
                    "MaterialBlend",
                    "automaticallyAddTerrainTopology",
                ]
                .contains(&field.name.as_str()),
                _ => ["m_Enabled", "m_GameObject", "m_Size", "m_Center"]
                    .contains(&field.name.as_str()),
            });
        }
        values::into_value(fields)?
    } else {
        Value::Null
    };
    Ok((kind, fields))
}
fn modifier_fields(fields: &Value) -> Result<Value, String> {
    let mut result = json!({"size":vector(&fields["size"]),"offset":vector(&fields["offset"])});
    for (name, source) in [
        ("heightMap", "HeightMap"),
        ("waterMap", "WaterMap"),
        ("alphaMap", "AlphaMap"),
        ("splatMask", "SplatMask"),
        ("biomeMask", "BiomeMask"),
        ("topologyMask", "TopologyMask"),
        ("radius", "Radius"),
        ("fade", "Fade"),
        ("autoCliffSplat", "AutoCliffSplat"),
        ("autoCliffTopology", "AutoCliffTopology"),
        ("removeExistingTopology", "RemoveExistingTopology"),
    ] {
        let value = fields[source].clone();
        result[name] = if [
            "heightMap",
            "waterMap",
            "alphaMap",
            "autoCliffSplat",
            "autoCliffTopology",
            "removeExistingTopology",
        ]
        .contains(&name)
        {
            match value {
                Value::Bool(_) => value,
                Value::Null if name == "waterMap" => Value::Null,
                Value::Number(ref n) if n.as_u64() == Some(0) => json!(false),
                Value::Number(ref n) if n.as_u64() == Some(1) => json!(true),
                _ => return Err(format!("Unsupported serialized boolean: {source}")),
            }
        } else {
            value
        };
    }
    let mut guids = serde_json::Map::new();
    for (name, source) in [
        ("height", "heightmap"),
        ("alpha", "alphamap"),
        ("splat0", "splatmap0"),
        ("splat1", "splatmap1"),
        ("blend", "blendmap"),
        ("topology", "topologymap"),
        ("biome", "biomemap"),
        ("water", "watermap"),
    ] {
        let guid = fields[source]["guid"].as_str().filter(|s| !s.is_empty());
        guids.insert(name.into(), guid.map_or(Value::Null, |s| json!(s)));
    }
    result["textureGuids"] = guids.into();
    let mut ids = serde_json::Map::new();
    for (name, source) in [
        ("height", "heightmap"),
        ("alpha", "alphamap"),
        ("splat0", "splatmap0"),
        ("splat1", "splatmap1"),
        ("blend", "blendmap"),
        ("topology", "topologymap"),
        ("biome", "biomemap"),
    ] {
        ids.insert(
            name.into(),
            fields[source]["m_PathID"]
                .as_i64()
                .filter(|id| *id != 0)
                .map_or(Value::Null, |id| json!(id)),
        );
    }
    result["textureObjectIds"] = ids.into();
    Ok(result)
}
fn placement_fields(fields: &Value) -> Result<Value, String> {
    let count = fields["Count"]
        .as_f64()
        .filter(|v| {
            v.is_finite()
                && v.fract() == 0.
                && *v >= f64::from(i32::MIN)
                && *v <= f64::from(i32::MAX)
        })
        .ok_or("Fractional or excessive prefab Count requires a catalog adapter")?;
    Ok(json!({"priority":fields["Priority"],"count":count as i32}))
}
fn display_name(fields: &Value) -> Value {
    let phrase = &fields["displayPhrase"];
    let english = &phrase["english"];
    let truthy = match english {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64() != Some(0.),
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
    };
    let name = if truthy {
        english
    } else {
        &phrase["legacyEnglish"]
    };
    let Some(name) = name.as_str() else {
        return Value::Null;
    };
    let name = name
        .split(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if name.is_empty() || name.len() > 256 {
        Value::Null
    } else {
        name.into()
    }
}
fn catalog_metadata(roots: &BTreeMap<String, Value>) -> Result<Value, String> {
    let mut modifiers = serde_json::Map::new();
    let mut special = serde_json::Map::new();
    for (path, root) in roots {
        let scene = root["scene"]
            .as_str()
            .and_then(|s| s.strip_prefix("BuildPlayer-"))
            .ok_or("Invalid native scene identity")?;
        let mut data = json!({"displayName":root["displayName"],"assetScene":scene,"monumentBounds":root["bounds"],"placement":root["placement"],"rotation":root["rotation"],"waterGeometry":root["waterGeometry"]});
        if let Some(modifier) = root["modifier"].as_object() {
            for (key, value) in modifier {
                data[key] = value.clone();
            }
            modifiers.insert(path.clone(), data);
        } else {
            if root["modifier_issue"].is_null() {
                data["reason"] = "No terrain modifier component".into();
            } else {
                data["reason"] = "Unsupported terrain modifier layout".into();
                data["missingModifierFields"] = root["modifier_missing_fields"].clone();
            }
            special.insert(path.clone(), data);
        }
    }
    Ok(json!({"format":5,"modifiers":modifiers,"special":special}))
}
fn inspect(
    root: &Path,
    unbuffered: bool,
    progress: impl Fn(usize, usize, &str),
) -> Result<Value, String> {
    let manifest_path = root.join("Bundles/AssetSceneManifest.json");
    let manifest_before = fs::metadata(&manifest_path).map_err(|e| e.to_string())?;
    let manifest_bytes = bounded(&manifest_path, 32 * 1024 * 1024)?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes).map_err(|e| e.to_string())?;
    let mut candidates = BTreeMap::new();
    let mut scenes = BTreeSet::new();
    for scene in manifest["Scenes"]
        .as_array()
        .filter(|s| s.len() <= 8192)
        .ok_or("Invalid scene manifest")?
    {
        let name = scene["Name"].as_str().ok_or("Missing scene name")?;
        for asset in scene["IncludedAssets"]
            .as_array()
            .filter(|a| a.len() <= 262144)
            .ok_or("Invalid scene asset list")?
        {
            let path = asset
                .as_str()
                .ok_or("Invalid asset identifier")?
                .to_lowercase();
            if !path.starts_with("assets/bundled/prefabs/autospawn/") || !path.ends_with(".prefab")
            {
                continue;
            }
            let node = format!("BuildPlayer-{name}");
            if candidates
                .insert(path, node.clone())
                .is_some_and(|old| old != node)
            {
                return Err("Ambiguous manifest scene assignment".into());
            }
            scenes.insert(node);
        }
    }
    if candidates.is_empty() || candidates.len() > 8192 || scenes.is_empty() || scenes.len() > 128 {
        return Err("Manifest candidate budget exceeded".into());
    }
    let bundle = root.join("Bundles/shared/assetscenes.bundle");
    let before = fs::metadata(&bundle).map_err(|e| e.to_string())?;
    if before.len() > 2 * 1024 * 1024 * 1024 {
        return Err("Scene bundle exceeds probe budget".into());
    }
    let started = Instant::now();
    let paged = Arc::new(pages::PagedFile::open(&bundle).map_err(|e| e.to_string())?);
    let source = if unbuffered {
        Region::from_file(&bundle)
    } else {
        Region::new(paged.clone(), 0, before.len())
    }
    .map_err(|e| e.to_string())?;
    let archive = UnityFsBundle::open_with_limits(
        &source,
        BundleParseLimits {
            max_entries: 128,
            max_compressed_block_size: 2 * 1024 * 1024 * 1024,
            max_uncompressed_block_size: 2 * 1024 * 1024 * 1024,
            ..Default::default()
        },
    )
    .map_err(|e| e.to_string())?;
    if archive.blocks.iter().any(|b| {
        b.compression != unity_rs_core::bundle::CompressionType::None
            && (b.uncompressed_size > 8 * 1024 * 1024 || b.compressed_size > 64 * 1024 * 1024)
    }) {
        return Err("Compressed block exceeds probe decoding budget".into());
    }
    if scenes
        .iter()
        .any(|scene| !archive.entries.iter().any(|entry| &entry.path == scene))
    {
        return Err("Manifest scene is absent from the bundle".into());
    }
    let mut roots = BTreeMap::new();
    let mut issues = Vec::new();
    let mut components_read = 0;
    let mut objects = 0;
    let mut files = 0;
    for (entry_index, entry) in archive.entries.iter().enumerate() {
        if !scenes.contains(&entry.path) {
            continue;
        }
        let file = SerializedFile::open_with_limits(
            archive
                .entry_region(entry_index)
                .map_err(|e| e.to_string())?,
            SerializedParseLimits {
                maximum_metadata_bytes: 64 * 1024 * 1024,
                maximum_objects: 2_000_000,
                ..Default::default()
            },
        )
        .map_err(|e| format!("{}: {e}", entry.path))?;
        progress(files, scenes.len(), &entry.path);
        files += 1;
        objects += file.objects.len();
        let lookup: HashMap<_, _> = file
            .objects
            .iter()
            .enumerate()
            .map(|(i, o)| (o.path_id, i))
            .collect();
        for (object_index, object) in file
            .objects
            .iter()
            .enumerate()
            .filter(|(_, o)| o.class_id == 1)
        {
            let go = read_game_object(&file, object_index, SceneReadLimits::default())
                .map_err(|e| e.to_string())?;
            let name = go.name.to_lowercase();
            let Some(expected_scene) = candidates.get(&name) else {
                continue;
            };
            if &entry.path != expected_scene {
                continue;
            }
            let result = (|| -> Result<Value, String> {
                let entries = &go.components;
                if entries.len() > 4096 {
                    return Err("Root has too many components".into());
                }
                let mut bounds = Value::Null;
                let mut name = Value::Null;
                let mut placement = Value::Null;
                let mut raw_placement = Value::Null;
                let mut modifier = Value::Null;
                let mut rotation = Value::Null;
                let mut modifier_issue = None;
                let mut modifier_missing_fields: Vec<&str> = Vec::new();
                for entry in entries {
                    let pointer = entry.component;
                    if pointer.file_id != 0 {
                        return Err("External root component requires an adapter".into());
                    }
                    let &index = lookup
                        .get(&pointer.path_id)
                        .ok_or("Missing root component")?;
                    if file.objects[index].class_id != 114 {
                        continue;
                    }
                    components_read += 1;
                    let fields = object_fields(&file, index)?;
                    if fields.get("Bounds").is_some() && fields.get("MinWorldSize").is_some() {
                        name = display_name(&fields);
                        bounds = json!({"center":vector(&fields["Bounds"]["m_Center"]),"extent":vector(&fields["Bounds"]["m_Extent"]),"minWorldSize":fields["MinWorldSize"]});
                    }
                    if fields.get("Priority").is_some() && fields.get("Count").is_some() {
                        raw_placement =
                            json!({"Priority":fields["Priority"],"Count":fields["Count"]});
                    }
                    if fields.get("MinRotation").is_some() && fields.get("MaxRotation").is_some() {
                        rotation = json!({"min":vector(&fields["MinRotation"]),"max":vector(&fields["MaxRotation"])});
                    }
                    if fields.get("size").is_some()
                        && fields.get("splatmap0").is_some()
                        && fields.get("blendmap").is_some()
                    {
                        modifier_missing_fields = [
                            "size",
                            "offset",
                            "HeightMap",
                            "AlphaMap",
                            "SplatMask",
                            "BiomeMask",
                            "TopologyMask",
                            "Radius",
                            "Fade",
                            "AutoCliffSplat",
                            "AutoCliffTopology",
                            "RemoveExistingTopology",
                        ]
                        .into_iter()
                        .filter(|key| fields.get(*key).is_none())
                        .collect();
                        modifier_missing_fields.sort_unstable();
                        if !modifier_missing_fields.is_empty() {
                            modifier_issue = Some("Unsupported terrain modifier layout".into());
                            continue;
                        }
                        match modifier_fields(&fields) {
                            Ok(value) => modifier = value,
                            Err(error) => modifier_issue = Some(error),
                        }
                    }
                }
                // Discovery ignores non-monument resource roots before their
                // placement parameters enter the monument catalog contract.
                if (!bounds.is_null() || !modifier.is_null() || modifier_issue.is_some())
                    && !raw_placement.is_null()
                {
                    placement = placement_fields(&raw_placement)?;
                }
                Ok(
                    json!({"displayName":name,"path_id":object.path_id,"scene":entry.path,"bounds":bounds,"placement":placement,"modifier":modifier,"rotation":rotation,"modifier_issue":modifier_issue,"modifier_missing_fields":modifier_missing_fields}),
                )
            })();
            match result {
                Ok(mut root) => {
                    if root["bounds"].is_null()
                        && root["modifier"].is_null()
                        && root["modifier_issue"].is_null()
                    {
                        continue;
                    }
                    root["waterGeometry"] = geometry::inspect(object.path_id, |id| {
                        let &index = lookup.get(&id).ok_or_else(|| id.to_string())?;
                        geometry_record(&file, index)
                    });
                    if roots.insert(name.clone(), root).is_some() {
                        return Err("Ambiguous root name".into());
                    }
                }
                Err(error) => issues.push(json!({"path":name,"error":error})),
            }
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    let after = fs::metadata(&bundle).map_err(|e| e.to_string())?;
    let manifest_after = fs::metadata(&manifest_path).map_err(|e| e.to_string())?;
    if before.len() != after.len()
        || before.modified().map_err(|e| e.to_string())?
            != after.modified().map_err(|e| e.to_string())?
    {
        return Err("Bundle changed during probe".into());
    }
    if manifest_before.len() != manifest_after.len()
        || manifest_before.modified().map_err(|e| e.to_string())?
            != manifest_after.modified().map_err(|e| e.to_string())?
    {
        return Err("Manifest changed during probe".into());
    }
    progress(files, scenes.len(), "");
    if roots.is_empty() || roots.len() > 4096 {
        return Err("Invalid monument root count".into());
    }
    let metadata = catalog_metadata(&roots)?;
    Ok(
        json!({"format":"mapper-native-asset-reader-probe-1","backend":"unity-rs-core-0.6.2", "catalog_metadata":metadata,
        "input_bytes":before.len(),"manifest_sha256":format!("{:x}",Sha256::digest(&manifest_bytes)),"manifest_candidates":candidates.len(),
        "serialized_files":files,"objects":objects,"roots_read":roots.len(),"components_read":components_read,
        "buffered":!unbuffered,"source_read_requests":paged.requests.load(Ordering::Relaxed),"physical_reads":paged.physical_reads.load(Ordering::Relaxed),"physical_bytes":paged.physical_bytes.load(Ordering::Relaxed),"total_seconds":elapsed,"issues":issues,"roots":roots,
        "full_installation_hashing_included":false}),
    )
}
fn source_fingerprint() -> String {
    let mut digest = Sha256::new();
    for (name, source) in [
        ("main.rs", include_str!("main.rs")),
        ("geometry.rs", include_str!("geometry.rs")),
        ("pages.rs", include_str!("pages.rs")),
        ("values.rs", include_str!("values.rs")),
        ("map_labels.rs", include_str!("map_labels.rs")),
        ("Cargo.toml", include_str!("../Cargo.toml")),
        ("Cargo.lock", include_str!("../Cargo.lock")),
    ] {
        digest.update(name.as_bytes());
        digest.update([0]);
        digest.update(source.as_bytes());
    }
    format!("{:x}", digest.finalize())
}
fn write_new(path: &Path, value: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("Reader output exceeds allowance".into());
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| e.to_string())
}
fn progress(completed: usize, total: usize, item: &str) {
    println!(
        "MAPPER_PROGRESS {}",
        json!({"schema":1,"stage":"Reading monument metadata","item":item,"completed":completed,"total":total,"unit":"items"})
    );
}
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == "--self-test" {
        println!(
            "{}",
            json!({"format":"mapper-native-catalog-reader-self-test","schema":1,"extractor_sha256":source_fingerprint(),"backend":"unity-rs-core-0.6.2"})
        );
        return Ok(());
    }
    if args.len() == 3 && args[0] == "--map-labels" {
        return map_labels::extract(Path::new(&args[1]), Path::new(&args[2]));
    }
    if args.len() == 3 && args[0] == "--catalog-metadata" {
        let output = Path::new(&args[2]);
        if output.exists() {
            return Err("Reader output must be new".into());
        }
        let mut report = inspect(Path::new(&args[1]), false, progress)?;
        if !report["issues"].as_array().is_some_and(|v| v.is_empty()) {
            return Err("Native monument layout requires a compatibility adapter".into());
        }
        report["format"] = "mapper-native-catalog-metadata-1".into();
        report["extractor_sha256"] = source_fingerprint().into();
        report.as_object_mut().unwrap().remove("roots");
        write_new(output, &report)?;
        return Ok(());
    }
    let unbuffered = args.len() == 4 && args[3] == "--unbuffered";
    if args.len() != 3 && !unbuffered {
        return Err("--self-test | --catalog-metadata RUST_ROOT NEW_JSON | RUST_ROOT CATALOG_JSON NEW_REPORT_JSON [--unbuffered]".into());
    }
    let output = Path::new(&args[2]);
    if output.exists() {
        return Err("Report must be new".into());
    }
    let catalog_bytes = bounded(Path::new(&args[1]), 16 * 1024 * 1024)?;
    let catalog: Value = serde_json::from_slice(&catalog_bytes).map_err(|e| e.to_string())?;
    if catalog["format"] != "papyrus-prefab-catalog" || catalog["schema"] != 1 {
        return Err("Unsupported reference catalog".into());
    }
    let mut targets = BTreeSet::new();
    for group in ["modifiers", "special"] {
        for (path, data) in catalog["metadata"][group]
            .as_object()
            .ok_or("Missing reference entries")?
        {
            if data["assetScene"].as_str().is_some() {
                targets.insert(path.clone());
            }
        }
    }
    if targets.is_empty() || targets.len() > 4096 {
        return Err("Invalid reference roster".into());
    }
    let mut report = inspect(Path::new(&args[0]), unbuffered, |_, _, _| {})?;
    let missing: Vec<_> = targets
        .iter()
        .filter(|path| report["roots"].get(path.as_str()).is_none())
        .cloned()
        .collect();
    report["catalog_sha256"] = format!("{:x}", Sha256::digest(catalog_bytes)).into();
    report["target_roots"] = targets.len().into();
    report["missing"] = json!(missing);
    write_new(output, &report)?;
    println!(
        "Native reader: {} roots; {} seconds",
        report["roots_read"], report["total_seconds"]
    );
    if !missing.is_empty() || !report["issues"].as_array().is_some_and(|v| v.is_empty()) {
        return Err("Native root inspection is incomplete; see report".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_follow_installed_phrases_without_invented_aliases() {
        assert_eq!(
            display_name(
                &json!({"displayPhrase":{"english":"  Outpost\n Camp ","legacyEnglish":"Old"}})
            ),
            "Outpost Camp"
        );
        assert_eq!(
            display_name(&json!({"displayPhrase":{"english":"","legacyEnglish":"Legacy name"}})),
            "Legacy name"
        );
        assert_eq!(
            display_name(&json!({"displayPhrase":{"english":1,"legacyEnglish":"Old"}})),
            Value::Null
        );
        assert_eq!(
            display_name(&json!({"displayPhrase":{"english":"\u{001c}  "}})),
            Value::Null
        );
        assert_eq!(
            display_name(&json!({"displayPhrase":{"english":"é".repeat(129)}})),
            Value::Null
        );
    }
    #[test]
    fn integral_source_counts_preserve_the_catalog_without_truncating_new_layouts() {
        assert_eq!(
            placement_fields(&json!({"Priority":4,"Count":2.0})).unwrap(),
            json!({"priority":4,"count":2})
        );
        assert!(placement_fields(&json!({"Priority":4,"Count":0.5})).is_err());
        assert!(placement_fields(&json!({"Priority":4,"Count":1e30})).is_err());
    }
    #[test]
    fn numeric_boolean_fields_normalize_without_guessing_unknown_values() {
        let mut fields = json!({"size":{"x":100.0,"y":200.0,"z":300.0},"offset":{"x":1.0,"y":2.0,"z":3.0},
            "HeightMap":1,"AlphaMap":0,"SplatMask":255,"BiomeMask":31,"TopologyMask":-1,
            "Radius":50.0,"Fade":4.0,"AutoCliffSplat":0,"AutoCliffTopology":true,"RemoveExistingTopology":false,
            "heightmap":{"guid":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}});
        let result = modifier_fields(&fields).unwrap();
        assert_eq!(result["heightMap"], true);
        assert_eq!(result["alphaMap"], false);
        assert_eq!(result["waterMap"], Value::Null);
        assert_eq!(result["topologyMask"], -1);
        assert_eq!(
            result["textureGuids"]["height"],
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        for value in [json!(2), json!(-1), json!("true"), Value::Null] {
            fields["HeightMap"] = value;
            assert!(modifier_fields(&fields).is_err());
        }
    }
}
