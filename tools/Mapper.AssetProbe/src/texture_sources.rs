//! Exact GUID/container discovery matching the current texture importer's source order.
use crate::pages::PagedFile;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::Path,
    sync::Arc,
};
use unity_rs_core::{
    bundle::{BundleParseLimits, CompressionType, UnityFsBundle},
    serialized::{
        ContainerMetadataReadLimits, ExternalFile, ObjectReference, SerializedFile,
        SerializedParseLimits,
    },
    source::Region,
    type_tree::TypeValue,
};

const PRIMARY: [&str; 2] = [
    "Bundles/shared/content.bundle",
    "Bundles/shared/monuments.bundle",
];
const EXTRA: [&str; 6] = [
    "Bundles/maps/maps.bundle",
    "Bundles/shared/textures.0.bundle",
    "Bundles/shared/textures.1.bundle",
    "Bundles/shared/textures.2.bundle",
    "Bundles/shared/textures.3.bundle",
    "Bundles/shared/textures.4.bundle",
];

fn bounded(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("Texture catalog exceeds its byte budget".into());
    }
    Ok(bytes)
}

fn required_guids(catalog: &Value) -> Result<BTreeSet<String>, String> {
    if catalog["format"] != "papyrus-prefab-catalog" || catalog["schema"] != 1 {
        return Err("Unsupported texture catalog".into());
    }
    let modifiers = catalog["metadata"]["modifiers"]
        .as_object()
        .ok_or("Missing catalog modifiers")?;
    let mut required = BTreeSet::new();
    for modifier in modifiers.values() {
        let Some(guids) = modifier.get("textureGuids") else {
            continue;
        };
        for value in guids
            .as_object()
            .ok_or("Invalid catalog texture GUIDs")?
            .values()
        {
            if value.is_null() || value.as_str() == Some("") {
                continue;
            }
            let guid = value.as_str().ok_or("Invalid texture GUID")?;
            if guid.len() != 32
                || !guid
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            {
                return Err("Invalid required texture GUID".into());
            }
            required.insert(guid.to_owned());
            if required.len() > 4096 {
                return Err("Texture GUID count exceeds its budget".into());
            }
        }
    }
    if required.is_empty() {
        return Err("Catalog has no texture GUIDs".into());
    }
    Ok(required)
}

fn metadata_prefix(region: &Region) -> Result<Option<u64>, String> {
    if region.len() < 20 {
        return Ok(None);
    }
    let mut bytes = [0u8; 48];
    let count = region.len().min(48) as usize;
    region
        .read_exact_at(0, &mut bytes[..count])
        .map_err(|e| e.to_string())?;
    let u32_at = |at| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
    let version = u32_at(8);
    if !(9..=22).contains(&version) || bytes[16] > 1 {
        return Ok(None);
    }
    let (metadata, size, offset) = if version >= 22 {
        if count < 48 {
            return Ok(None);
        }
        (
            u64::from(u32_at(20)),
            u64::from_be_bytes(bytes[24..32].try_into().unwrap()),
            u64::from_be_bytes(bytes[32..40].try_into().unwrap()),
        )
    } else {
        (
            u64::from(u32_at(0)),
            u64::from(u32_at(4)),
            u64::from(u32_at(12)),
        )
    };
    Ok((size == region.len()
        && metadata > 0
        && metadata <= offset
        && offset >= 20
        && offset <= region.len())
    .then_some(offset))
}

struct Index {
    files: BTreeMap<String, SerializedFile>,
    entries: BTreeMap<String, Vec<(String, ObjectReference)>>,
}
impl Index {
    fn open(root: &Path, relative: &str, stage: &str) -> Result<Self, String> {
        println!(
            "MAPPER_PROGRESS {}",
            json!({"schema":1,"stage":stage,
            "item":relative,"completed":0,"total":null,"unit":"items"})
        );
        let source = Arc::new(PagedFile::open(&root.join(relative)).map_err(|e| e.to_string())?);
        let region = Region::new(
            source,
            0,
            fs::metadata(root.join(relative))
                .map_err(|e| e.to_string())?
                .len(),
        )
        .map_err(|e| e.to_string())?;
        if region.len() > 16 * 1024 * 1024 * 1024 {
            return Err("Texture bundle exceeds its byte budget".into());
        }
        let archive = UnityFsBundle::open_with_limits(
            &region,
            BundleParseLimits {
                max_entries: 8192,
                max_compressed_block_size: 16 * 1024 * 1024 * 1024,
                max_uncompressed_block_size: 16 * 1024 * 1024 * 1024,
                ..Default::default()
            },
        )
        .map_err(|e| e.to_string())?;
        if archive.blocks.iter().any(|b| {
            b.compression != CompressionType::None
                && (b.uncompressed_size > 8 * 1024 * 1024 || b.compressed_size > 64 * 1024 * 1024)
        }) {
            return Err("Texture bundle compression exceeds its budget".into());
        }
        let mut files = BTreeMap::new();
        let mut remaining = 64_u64 * 1024 * 1024;
        let mut objects = 0usize;
        for (i, entry) in archive.entries.iter().enumerate() {
            let region = archive.entry_region(i).map_err(|e| e.to_string())?;
            let Some(prefix) = metadata_prefix(&region)? else {
                continue;
            };
            remaining = remaining
                .checked_sub(prefix)
                .ok_or("Texture metadata exceeds its budget")?;
            let file = SerializedFile::open_with_limits(
                region,
                SerializedParseLimits {
                    maximum_metadata_bytes: 64 * 1024 * 1024,
                    maximum_objects: 2_000_000,
                    ..Default::default()
                },
            )
            .map_err(|e| e.to_string())?;
            objects = objects
                .checked_add(file.objects.len())
                .filter(|&n| n <= 2_000_000)
                .ok_or("Texture object index exceeds its budget")?;
            let name = entry
                .path
                .rsplit('/')
                .next()
                .ok_or("Invalid serialized node")?
                .to_lowercase();
            if files.insert(name, file).is_some() {
                return Err("Ambiguous texture serialized node".into());
            }
        }
        let mut entries: BTreeMap<String, Vec<(String, ObjectReference)>> = BTreeMap::new();
        for (name, file) in &files {
            // UnityPy's container is the first AssetBundle object in each file.
            let Some(index) = file.objects.iter().position(|o| o.class_id == 142) else {
                continue;
            };
            let container = file
                .read_asset_bundle_metadata(
                    index,
                    ContainerMetadataReadLimits {
                        maximum_preload_references: 4_000_000,
                        ..Default::default()
                    },
                )
                .map_err(|e| e.to_string())?;
            for item in container.container {
                let key = item.key.replace('\\', "/").to_lowercase();
                entries
                    .entry(key)
                    .or_default()
                    .push((name.clone(), item.asset));
            }
        }
        Ok(Self { files, entries })
    }
    fn reference(
        name: &str,
        externals: &[ExternalFile],
        reference: ObjectReference,
    ) -> Result<(String, i64), String> {
        if reference.file_id < 0 || reference.path_id == 0 {
            return Err("Invalid texture container reference".into());
        }
        let target = if reference.file_id == 0 {
            name.to_owned()
        } else {
            externals
                .get(reference.file_id as usize - 1)
                .ok_or("Missing texture external file")?
                .path
                .replace('\\', "/")
                .rsplit('/')
                .next()
                .ok_or("Invalid texture external name")?
                .to_lowercase()
        };
        Ok((target, reference.path_id))
    }
    fn asset(&self, path: &str) -> Result<Option<(&SerializedFile, usize)>, String> {
        let Some(entries) = self.entries.get(path) else {
            return Ok(None);
        };
        let mut targets = BTreeSet::new();
        for (owner, reference) in entries {
            let file = self.files.get(owner).ok_or("Missing container owner")?;
            targets.insert(Self::reference(owner, &file.externals, *reference)?);
        }
        if targets.len() != 1 {
            return Err(format!("Ambiguous requested texture asset {path}"));
        }
        let (name, id) = targets.first().unwrap();
        let file = self
            .files
            .get(name)
            .ok_or("Missing texture dependency node")?;
        let index = file
            .objects
            .iter()
            .position(|o| o.path_id == *id)
            .ok_or("Missing texture object")?;
        Ok(Some((file, index)))
    }
    fn textures(
        &self,
        paths: &BTreeMap<String, String>,
        wanted: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, String> {
        let mut found = BTreeSet::new();
        for guid in wanted {
            if let Some(path) = paths.get(guid)
                && let Some((file, index)) = self.asset(path)?
            {
                if file.objects[index].class_id != 28 {
                    return Err(format!("Terrain GUID is not a texture {path}"));
                }
                found.insert(guid.clone());
            }
        }
        Ok(found)
    }
}

fn field<'a>(value: &'a TypeValue, name: &str) -> Result<&'a TypeValue, String> {
    let TypeValue::Object(fields) = value else {
        return Err("Invalid GameManifest record".into());
    };
    let mut matches = fields.iter().filter(|f| f.name == name);
    let found = matches
        .next()
        .ok_or_else(|| format!("Missing GameManifest field {name}"))?;
    if matches.next().is_some() {
        return Err("Ambiguous GameManifest field".into());
    }
    Ok(&found.value)
}
fn string(value: &TypeValue) -> Result<&str, String> {
    match value {
        TypeValue::String(s) => Ok(s),
        _ => Err("Invalid GameManifest string".into()),
    }
}
fn game_manifest(index: &Index) -> Result<TypeValue, String> {
    let (file, object) = index
        .asset("assets/manifest.asset")?
        .ok_or("GameManifest is absent")?;
    if file.objects[object].byte_size > 64 * 1024 * 1024 {
        return Err("GameManifest exceeds its byte budget".into());
    }
    file.read_type_tree_value(object).map_err(|e| e.to_string())
}

fn prefab_ids(value: &TypeValue) -> Result<BTreeMap<String, u32>, String> {
    let (TypeValue::Array(strings), TypeValue::Array(properties)) = (
        field(value, "pooledStrings")?,
        field(value, "prefabProperties")?,
    ) else {
        return Err("Unsupported GameManifest identity tables".into());
    };
    if !(1..=262144).contains(&strings.len()) || !(1..=131072).contains(&properties.len()) {
        return Err("Invalid GameManifest identity table size".into());
    }
    let id = |row| match field(row, "hash")? {
        TypeValue::Unsigned(n) => u32::try_from(*n).map_err(|e| e.to_string()),
        TypeValue::Signed(n) => u32::try_from(*n).map_err(|e| e.to_string()),
        _ => Err("Invalid pooled string ID".into()),
    };
    let mut ids = BTreeSet::new();
    let mut pool = BTreeMap::new();
    for row in strings {
        let name = string(field(row, "str")?)?;
        let identity = id(row)?;
        if name.len() > 4096 || name.contains('\0') {
            return Err("Invalid pooled string".into());
        }
        if ids.insert(identity) && pool.insert(name.to_lowercase(), identity).is_some() {
            return Err("Ambiguous case-insensitive pooled string".into());
        }
    }
    let mut prefabs = BTreeMap::new();
    for row in properties {
        let path = string(field(row, "name")?)?.to_ascii_lowercase();
        let identity = id(row)?;
        if !path.is_ascii()
            || !path.starts_with("assets/")
            || !path.ends_with(".prefab")
            || path.contains('\\')
            || path.chars().any(char::is_control)
            || path.split('/').any(|p| matches!(p, "" | "." | ".."))
            || identity == 0
            || pool.get(&path) != Some(&identity)
        {
            return Err(
                "Prefab property is not resolvable through the selected string pool".into(),
            );
        }
        prefabs.insert(path, identity);
    }
    Ok(prefabs)
}

pub fn write_prefab_ids(root: &Path, output: &Path) -> Result<(), String> {
    let index = Index::open(root, PRIMARY[0], "Reading prefab identities")?;
    let prefabs = prefab_ids(&game_manifest(&index)?)?;
    write_new(
        output,
        &serde_json::to_vec(&prefabs).map_err(|e| e.to_string())?,
    )
}

fn identity_fields(manifest: &TypeValue) -> Result<Value, String> {
    let mut result = serde_json::Map::new();
    for (name, columns) in [
        ("pooledStrings", &["str", "hash"][..]),
        ("prefabProperties", &["name", "hash", "guid"][..]),
        ("guidPaths", &["name", "guid"][..]),
    ] {
        let TypeValue::Array(rows) = field(manifest, name)? else {
            return Err("Unsupported GameManifest identity tables".into());
        };
        if rows.len() > 262144 {
            return Err("GameManifest identity table exceeds its budget".into());
        }
        let mut projected = Vec::with_capacity(rows.len());
        for row in rows {
            let mut record = serde_json::Map::new();
            for &column in columns {
                let value = match field(row, column)? {
                    TypeValue::String(s) => Value::String(s.clone()),
                    TypeValue::Unsigned(n) => Value::from(*n),
                    TypeValue::Signed(n) => Value::from(*n),
                    _ => return Err("Unknown GameManifest identity field type".into()),
                };
                record.insert(column.into(), value);
            }
            projected.push(Value::Object(record));
        }
        result.insert(name.into(), Value::Array(projected));
    }
    Ok(Value::Object(result))
}

pub fn write_identity_fields(root: &Path, output: &Path) -> Result<(), String> {
    let index = Index::open(root, PRIMARY[0], "Reading prefab references")?;
    let fields = identity_fields(&game_manifest(&index)?)?;
    write_new(
        output,
        &serde_json::to_vec(&fields).map_err(|e| e.to_string())?,
    )
}

fn guid_paths(
    value: &TypeValue,
    required: &BTreeSet<String>,
) -> Result<BTreeMap<String, String>, String> {
    let mut paths = BTreeMap::new();
    for group in ["prefabProperties", "guidPaths"] {
        let TypeValue::Array(records) = field(value, group)? else {
            return Err("Unsupported GameManifest GUID table".into());
        };
        if records.len() > 262144 {
            return Err("GameManifest GUID table exceeds its budget".into());
        }
        for row in records {
            let guid = string(field(row, "guid")?)?;
            if !required.contains(guid) {
                continue;
            }
            let name = string(field(row, "name")?)?;
            if name.len() > 1024
                || !name.to_lowercase().starts_with("assets/")
                || name.contains(['\\', '\0'])
                || name
                    .split('/')
                    .any(|p| p.is_empty() || p == "." || p == "..")
            {
                return Err("Invalid GameManifest asset identifier".into());
            }
            let name = name.to_lowercase();
            if paths
                .insert(guid.to_owned(), name.clone())
                .is_some_and(|old| old != name)
            {
                return Err("Ambiguous GameManifest GUID".into());
            }
        }
    }
    Ok(paths)
}

pub fn write_plan(root: &Path, catalog: &Path, output: &Path) -> Result<(), String> {
    const STAGE: &str = "Locating monument textures";
    if output.try_exists().map_err(|e| e.to_string())? {
        return Err("Texture source plan must be new".into());
    }
    let raw = bounded(catalog, 16 * 1024 * 1024)?;
    let document: Value = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    let required = required_guids(&document)?;
    let mut paths = BTreeMap::new();
    let mut found = BTreeSet::new();
    let mut content_has_textures = false;
    for relative in PRIMARY {
        let index = Index::open(root, relative, STAGE)?;
        if relative == PRIMARY[0] {
            paths = guid_paths(&game_manifest(&index)?, &required)?;
        }
        let missing = required.difference(&found).cloned().collect();
        let textures = index.textures(&paths, &missing)?;
        if relative == PRIMARY[0] {
            content_has_textures = !textures.is_empty();
        }
        found.extend(textures);
    }
    let missing = required
        .difference(&found)
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut locations = BTreeMap::new();
    if !missing.is_empty() {
        for relative in EXTRA {
            if !root
                .join(relative)
                .try_exists()
                .map_err(|e| e.to_string())?
            {
                continue;
            }
            let index = Index::open(root, relative, STAGE)?;
            for guid in index.textures(&paths, &missing)? {
                if locations.insert(guid, relative).is_some() {
                    return Err("Texture occurs in multiple candidate bundles".into());
                }
            }
        }
        if locations.keys().cloned().collect::<BTreeSet<_>>() != missing {
            return Err("Unresolved texture GUIDs".into());
        }
    }
    let mut bundles = PRIMARY.to_vec();
    bundles.extend(locations.values().copied().collect::<BTreeSet<_>>());
    let plan = json!({"format":"mapper-texture-sources","schema":2,
        "catalog_sha256":format!("{:x}",Sha256::digest(&raw)),"bundles":bundles,
        "paths":paths,"content_has_textures":content_has_textures});
    let bytes = serde_json::to_vec(&plan).map_err(|e| e.to_string())?;
    write_new(output, &bytes)
}

fn write_new(output: &Path, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("Bundle reader output exceeds its byte budget".into());
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| e.to_string())?;
    file.write_all(bytes).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(fields: Vec<(&str, TypeValue)>) -> TypeValue {
        TypeValue::Object(
            fields
                .into_iter()
                .map(|(name, value)| unity_rs_core::type_tree::TypeField {
                    name: name.into(),
                    value,
                })
                .collect(),
        )
    }
    fn mapping(guid: &str, name: &str) -> TypeValue {
        object(vec![
            ("guid", TypeValue::String(guid.into())),
            ("name", TypeValue::String(name.into())),
        ])
    }
    fn manifest(rows: Vec<TypeValue>) -> TypeValue {
        object(vec![
            ("prefabProperties", TypeValue::Array(Vec::new())),
            ("guidPaths", TypeValue::Array(rows)),
        ])
    }

    #[test]
    fn prefab_ids_use_first_pooled_name_and_reject_conflicting_names() {
        let row = |key, path: &str, id| {
            object(vec![
                (key, TypeValue::String(path.into())),
                ("hash", TypeValue::Unsigned(id)),
            ])
        };
        let path = "assets/test.prefab";
        let properties = || TypeValue::Array(vec![row("name", path, 42)]);
        let manifest = |pool| {
            object(vec![
                ("pooledStrings", TypeValue::Array(pool)),
                ("prefabProperties", properties()),
            ])
        };
        let good = manifest(vec![
            row("str", "Assets/Test.prefab", 42),
            row("str", "alias", 42),
        ]);
        assert_eq!(
            prefab_ids(&good).unwrap(),
            BTreeMap::from([(path.to_owned(), 42)])
        );
        let conflict = manifest(vec![
            row("str", path, 42),
            row("str", "Assets/Test.prefab", 21),
        ]);
        assert!(prefab_ids(&conflict).is_err());
        let mismatched = manifest(vec![row("str", path, 21)]);
        assert!(prefab_ids(&mismatched).is_err());
    }

    #[test]
    fn required_guids_are_exact_deduplicated_and_bounded() {
        let guid = "0123456789abcdef0123456789abcdef";
        let mut catalog = json!({"format":"papyrus-prefab-catalog", "schema":1,
            "metadata":{"modifiers":{"one":{"textureGuids":{"height":guid,"mask":guid,"empty":null}}}}});
        assert_eq!(
            required_guids(&catalog).unwrap(),
            BTreeSet::from([guid.to_owned()])
        );
        for invalid in [
            json!("ABCDEF0123456789abcdef0123456789"),
            json!("bad"),
            json!(12),
        ] {
            catalog["metadata"]["modifiers"]["one"]["textureGuids"]["height"] = invalid;
            assert!(required_guids(&catalog).is_err());
        }
        let too_many: serde_json::Map<_, _> = (0..4097)
            .map(|i| (i.to_string(), json!(format!("{i:032x}"))))
            .collect();
        catalog["metadata"]["modifiers"]["one"]["textureGuids"] = too_many.into();
        assert!(required_guids(&catalog).is_err());
    }

    #[test]
    fn guid_mapping_retains_case_aliases_and_rejects_ambiguous_requested_names() {
        let guid = "0123456789abcdef0123456789abcdef";
        let required = BTreeSet::from([guid.to_owned()]);
        let paths = guid_paths(
            &manifest(vec![
                mapping(guid, "Assets/Test.asset"),
                mapping(guid, "assets/test.asset"),
            ]),
            &required,
        )
        .unwrap();
        assert_eq!(paths[guid], "assets/test.asset");
        assert!(
            guid_paths(
                &manifest(vec![
                    mapping(guid, "assets/one"),
                    mapping(guid, "assets/two")
                ]),
                &required
            )
            .is_err()
        );
        for name in [
            "assets//test",
            "assets/../test",
            "assets/./test",
            "assets/te\\st",
            "assets/te\0st",
            "other/test",
        ] {
            assert!(guid_paths(&manifest(vec![mapping(guid, name)]), &required).is_err());
        }
        let unrelated = manifest(vec![mapping("unrequested", "invalid")]);
        assert!(guid_paths(&unrelated, &required).unwrap().is_empty());
    }

    #[test]
    fn serialized_header_probe_accepts_bounded_prefixes_and_skips_raw_resources() {
        let mut bytes = vec![0u8; 64];
        bytes[8..12].copy_from_slice(&22_u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&16_u32.to_be_bytes());
        bytes[24..32].copy_from_slice(&64_u64.to_be_bytes());
        bytes[32..40].copy_from_slice(&48_u64.to_be_bytes());
        assert_eq!(
            metadata_prefix(&Region::from_bytes(bytes.clone())).unwrap(),
            Some(48)
        );
        bytes[24..32].copy_from_slice(&65_u64.to_be_bytes());
        assert_eq!(metadata_prefix(&Region::from_bytes(bytes)).unwrap(), None);
        assert_eq!(
            metadata_prefix(&Region::from_bytes(vec![0u8; 64])).unwrap(),
            None
        );
    }

    #[test]
    fn references_use_exact_local_or_declared_external_objects() {
        let reference = ObjectReference {
            file_id: 0,
            path_id: 7,
        };
        assert_eq!(
            Index::reference("cab-one", &[], reference).unwrap(),
            ("cab-one".into(), 7)
        );
        let externals = [ExternalFile {
            prefix: None,
            guid: None,
            kind: None,
            path: "archive:/CAB-TWO/CAB-TWO".into(),
        }];
        assert_eq!(
            Index::reference(
                "cab-one",
                &externals,
                ObjectReference {
                    file_id: 1,
                    ..reference
                }
            )
            .unwrap(),
            ("cab-two".into(), 7)
        );
        for invalid in [
            ObjectReference {
                file_id: -1,
                ..reference
            },
            ObjectReference {
                file_id: 2,
                ..reference
            },
            ObjectReference {
                path_id: 0,
                ..reference
            },
        ] {
            assert!(Index::reference("cab-one", &externals, invalid).is_err());
        }
    }

    #[test]
    fn malformed_catalog_does_not_publish_or_replace_a_plan() {
        let root = tempfile::tempdir().unwrap();
        let catalog = root.path().join("catalog.json");
        let plan = root.path().join("plan.json");
        fs::write(&catalog, b"{}").unwrap();
        assert!(write_plan(root.path(), &catalog, &plan).is_err());
        assert!(!plan.exists());
        fs::write(&plan, b"existing").unwrap();
        assert!(write_plan(root.path(), &catalog, &plan).is_err());
        assert_eq!(fs::read(plan).unwrap(), b"existing");
    }
}
