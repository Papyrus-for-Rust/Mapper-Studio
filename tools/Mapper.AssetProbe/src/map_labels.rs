//! Data-only map marker typography from the selected installation's UI prefab.
use crate::{bounded, object_fields, pages, values};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    sync::Arc,
};
use unity_rs_core::{
    bundle::{BundleParseLimits, UnityFsBundle},
    scene::{SceneReadLimits, read_game_object},
    serialized::{SerializedFile, SerializedParseLimits},
    source::Region,
    type_tree::TypeValue,
};

const BUNDLES: [&str; 2] = [
    "Bundles/shared/assetscenes.bundle",
    "Bundles/shared/content.bundle",
];
const MAX_FONT: usize = 8 * 1024 * 1024;

struct Resolver<'a> {
    root: &'a Path,
    archives: BTreeMap<String, UnityFsBundle>,
    files: BTreeMap<String, SerializedFile>,
    bundles: Vec<String>,
    used: BTreeSet<String>,
}
impl<'a> Resolver<'a> {
    fn new(root: &'a Path) -> Result<Self, String> {
        let mut directories = vec![root.join("Bundles")];
        let mut bundles = BTreeSet::new();
        let mut visited = 0;
        while let Some(directory) = directories.pop() {
            for entry in fs::read_dir(directory).map_err(|e| e.to_string())? {
                visited += 1;
                if visited > 4096 {
                    return Err("UI bundle discovery exceeds its path budget".into());
                }
                let entry = entry.map_err(|e| e.to_string())?;
                let kind = entry.file_type().map_err(|e| e.to_string())?;
                if kind.is_dir() {
                    directories.push(entry.path());
                }
                if kind.is_file() && entry.path().extension().is_some_and(|v| v == "bundle") {
                    bundles.insert(
                        entry
                            .path()
                            .strip_prefix(root)
                            .map_err(|e| e.to_string())?
                            .to_string_lossy()
                            .replace('\\', "/"),
                    );
                }
            }
        }
        let mut paths = BUNDLES
            .iter()
            .map(|name| name.to_string())
            .collect::<Vec<_>>();
        paths.extend(
            bundles
                .into_iter()
                .filter(|name| !BUNDLES.contains(&name.as_str())),
        );
        Ok(Self {
            root,
            archives: BTreeMap::new(),
            files: BTreeMap::new(),
            bundles: paths,
            used: BTreeSet::new(),
        })
    }
    fn file(&mut self, name: &str) -> Result<&SerializedFile, String> {
        let name = name
            .rsplit('/')
            .next()
            .ok_or("Invalid UI serialized file")?;
        if !self.files.contains_key(name) {
            let mut selected = None;
            for relative in &self.bundles {
                if !self.archives.contains_key(relative) {
                    let path = self.root.join(relative);
                    let length = fs::metadata(&path).map_err(|e| e.to_string())?.len();
                    if length > 16 * 1024 * 1024 * 1024 {
                        return Err("UI bundle exceeds its budget".into());
                    }
                    let source =
                        Arc::new(pages::PagedFile::open(&path).map_err(|e| e.to_string())?);
                    let region = Region::new(source, 0, length).map_err(|e| e.to_string())?;
                    let archive = UnityFsBundle::open_with_limits(
                        &region,
                        BundleParseLimits {
                            max_entries: 256,
                            max_compressed_block_size: 16 * 1024 * 1024 * 1024,
                            max_uncompressed_block_size: 16 * 1024 * 1024 * 1024,
                            ..Default::default()
                        },
                    )
                    .map_err(|e| e.to_string())?;
                    if archive.blocks.iter().any(|block| {
                        block.compression != unity_rs_core::bundle::CompressionType::None
                            && (block.uncompressed_size > 8 * 1024 * 1024
                                || block.compressed_size > 64 * 1024 * 1024)
                    }) {
                        return Err("UI bundle compression exceeds its decoding budget".into());
                    }
                    self.archives.insert(relative.clone(), archive);
                }
                let archive = &self.archives[relative];
                if let Some(index) = archive
                    .entries
                    .iter()
                    .position(|e| e.path.rsplit('/').next() == Some(name))
                {
                    self.used.insert(relative.clone());
                    selected = Some(
                        SerializedFile::open_with_limits(
                            archive.entry_region(index).map_err(|e| e.to_string())?,
                            SerializedParseLimits {
                                maximum_metadata_bytes: 64 * 1024 * 1024,
                                maximum_objects: 2_000_000,
                                ..Default::default()
                            },
                        )
                        .map_err(|e| e.to_string())?,
                    );
                    break;
                }
            }
            self.files.insert(
                name.into(),
                selected.ok_or_else(|| {
                    format!("Map UI dependency {name} is absent from its bundles")
                })?,
            );
        }
        Ok(&self.files[name])
    }
    fn reference(&mut self, origin: &str, value: &Value) -> Result<(String, i64), String> {
        let id = value["m_PathID"]
            .as_i64()
            .filter(|&v| v != 0)
            .ok_or("Missing map UI object reference")?;
        let file_id = value["m_FileID"]
            .as_u64()
            .filter(|&v| v <= 256)
            .ok_or("Invalid map UI file reference")?;
        let name = if file_id == 0 {
            origin.into()
        } else {
            self.file(origin)?
                .externals
                .get(file_id as usize - 1)
                .ok_or("Missing map UI external file")?
                .path
                .clone()
        };
        Ok((
            name.rsplit('/')
                .next()
                .ok_or("Invalid map UI external name")?
                .into(),
            id,
        ))
    }
    fn fields(&mut self, key: &(String, i64)) -> Result<(i32, Value), String> {
        let file = self.file(&key.0)?;
        let index = file
            .objects
            .iter()
            .position(|o| o.path_id == key.1)
            .ok_or("Missing map UI object")?;
        let kind = file.objects[index].class_id;
        let fields = if kind == 28 && file.objects[index].byte_size <= 64 * 1024 * 1024 {
            values::into_value(
                file.read_type_tree_value(index)
                    .map_err(|e| e.to_string())?,
            )?
        } else {
            object_fields(file, index)?
        };
        Ok((kind, fields))
    }
    fn texture_bytes(&mut self, key: &(String, i64), fields: &Value) -> Result<Vec<u8>, String> {
        let inline = &fields["image data"];
        if let Some(size) = inline["Size"]
            .as_u64()
            .filter(|&size| size > 0 && size <= 32 * 1024 * 1024)
        {
            let offset = inline["Offset"]
                .as_u64()
                .ok_or("Missing font atlas data offset")?;
            let file = self.file(&key.0)?;
            return file
                .object_region_by_path_id(key.1)
                .map_err(|e| e.to_string())?
                .subregion(offset, size)
                .map_err(|e| e.to_string())?
                .read_to_vec(32 * 1024 * 1024)
                .map_err(|e| e.to_string());
        }
        let stream = &fields["m_StreamData"];
        let name = stream["path"]
            .as_str()
            .and_then(|path| path.rsplit('/').next())
            .ok_or("Missing font atlas stream")?;
        let offset = stream["offset"]
            .as_u64()
            .ok_or("Missing font atlas stream offset")?;
        let size = stream["size"]
            .as_u64()
            .filter(|&n| n > 0 && n <= 32 * 1024 * 1024)
            .ok_or("Font atlas stream exceeds its budget")?;
        for (relative, archive) in &self.archives {
            if let Some(index) = archive
                .entries
                .iter()
                .position(|entry| entry.path.rsplit('/').next() == Some(name))
            {
                let data = archive
                    .entry_region(index)
                    .map_err(|e| e.to_string())?
                    .subregion(offset, size)
                    .map_err(|e| e.to_string())?
                    .read_to_vec(32 * 1024 * 1024)
                    .map_err(|e| e.to_string())?;
                self.used.insert(relative.clone());
                return Ok(data);
            }
        }
        Err("Font atlas stream is unavailable in its source bundles".into())
    }
}

fn open_type(resolver: &mut Resolver<'_>, key: &(String, i64)) -> Result<(Value, Vec<u8>), String> {
    let file = resolver.file(&key.0)?;
    let index = file
        .objects
        .iter()
        .position(|o| o.path_id == key.1)
        .ok_or("Map font object is unavailable")?;
    if file.objects[index].class_id != 128 || file.objects[index].byte_size > MAX_FONT as u64 {
        return Err("Unsupported map font type or size".into());
    }
    let mut font = file
        .read_type_tree_value(index)
        .map_err(|e| e.to_string())?;
    let TypeValue::Object(fields) = &mut font else {
        return Err("Unsupported map font fields".into());
    };
    let position = fields
        .iter()
        .position(|field| field.name == "m_FontData")
        .ok_or("Map font has no embedded data")?;
    let data = match fields.remove(position).value {
        TypeValue::ByteArray { bytes, .. } => bytes,
        TypeValue::TypelessData { offset, size } if size <= MAX_FONT as u64 => file
            .object_region(index)
            .map_err(|e| e.to_string())?
            .subregion(offset, size)
            .map_err(|e| e.to_string())?
            .read_to_vec(MAX_FONT as u64)
            .map_err(|e| e.to_string())?,
        _ => return Err("Map font data requires a byte-array adapter".into()),
    };
    if data.len() < 12
        || data.len() > MAX_FONT
        || ![&b"\0\x01\0\0"[..], &b"OTTO"[..], &b"ttcf"[..]].contains(&&data[..4])
    {
        return Err("Map font is not a supported embedded OpenType font".into());
    }
    Ok((values::into_value(font)?, data))
}

pub fn extract(root: &Path, output: &Path) -> Result<(), String> {
    if output.exists() {
        return Err("Map label output must be new".into());
    }
    let manifest: Value = serde_json::from_slice(&bounded(
        &root.join("Bundles/AssetSceneManifest.json"),
        32 * 1024 * 1024,
    )?)
    .map_err(|e| e.to_string())?;
    let mut marker = None;
    for scene in manifest["Scenes"]
        .as_array()
        .filter(|v| v.len() <= 8192)
        .ok_or("Invalid UI scene manifest")?
    {
        for asset in scene["IncludedAssets"]
            .as_array()
            .filter(|v| v.len() <= 262144)
            .ok_or("Invalid UI scene asset list")?
        {
            let asset = asset
                .as_str()
                .ok_or("Invalid UI asset identifier")?
                .to_lowercase();
            if asset.ends_with("/ui.map.monumentmarker.prefab") {
                let scene = scene["Name"].as_str().ok_or("Missing UI scene name")?;
                if marker
                    .replace((asset, format!("BuildPlayer-{scene}")))
                    .is_some()
                {
                    return Err("Ambiguous monument marker UI prefab".into());
                }
            }
        }
    }
    let (asset, scene) =
        marker.ok_or("This installation does not expose a monument marker UI prefab")?;
    let mut resolver = Resolver::new(root)?;
    let file = resolver.file(&scene)?;
    let mut root_object = None;
    for (index, object) in file
        .objects
        .iter()
        .enumerate()
        .filter(|(_, o)| o.class_id == 1)
    {
        let object_data =
            read_game_object(file, index, SceneReadLimits::default()).map_err(|e| e.to_string())?;
        if object_data.name.to_lowercase() == asset
            && root_object.replace((object.path_id, object_data)).is_some()
        {
            return Err("Ambiguous map marker root".into());
        }
    }
    let (root_id, root_object) = root_object.ok_or("Map marker prefab root is unavailable")?;
    if root_object.components.len() > 256 {
        return Err("Map marker component count exceeds its budget".into());
    }
    let mut marker_fields = None;
    let mut root_transform = None;
    for component in root_object.components {
        let key = (scene.clone(), component.component.path_id);
        if component.component.file_id != 0 {
            return Err("External map marker component requires an adapter".into());
        }
        let (kind, fields) = resolver.fields(&key)?;
        if kind == 224 {
            root_transform = Some(fields.clone());
        }
        if kind == 114
            && fields.get("text").is_some()
            && fields.get("dayColor").is_some()
            && fields.get("nightColor").is_some()
            && marker_fields.replace(fields).is_some()
        {
            return Err("Ambiguous map marker text component".into());
        }
    }
    let marker_fields = marker_fields.ok_or("Map marker text properties require an adapter")?;
    let text_key = resolver
        .reference(&scene, &marker_fields["text"])
        .map_err(|error| format!("Marker text reference: {error}"))?;
    let (kind, text) = resolver.fields(&text_key)?;
    if kind != 114 {
        return Err("Map marker text is not a serialized UI component".into());
    }
    let text_object = resolver
        .reference(&text_key.0, &text["m_GameObject"])
        .map_err(|error| format!("Text GameObject reference: {error}"))?;
    let (_, game_object) = resolver.fields(&text_object)?;
    let mut text_transform = None;
    for component in game_object["m_Component"]
        .as_array()
        .filter(|v| v.len() <= 256)
        .ok_or("Missing text GameObject components")?
    {
        let key = resolver
            .reference(&text_object.0, &component["component"])
            .map_err(|error| format!("Text component reference: {error}"))?;
        let (kind, fields) = resolver.fields(&key)?;
        if kind == 224 {
            text_transform = Some(fields);
        }
    }
    let font_reference = text
        .get("m_FontData")
        .map(|data| &data["m_Font"])
        .unwrap_or(&text["m_fontAsset"]);
    let font_key = resolver
        .reference(&text_key.0, font_reference)
        .map_err(|error| format!("Text font reference: {error}"))?;
    let (font_kind, font_fields) = resolver.fields(&font_key)?;
    let (kind, payload, font, atlas, material, data) = if font_kind == 114 {
        let atlas_key = resolver.reference(&font_key.0, &font_fields["m_AtlasTextures"][0])?;
        let (kind, atlas) = resolver.fields(&atlas_key)?;
        if kind != 28 {
            return Err("Font atlas is not a Texture2D".into());
        }
        let material_key = resolver.reference(&font_key.0, &font_fields["m_Material"])?;
        let (kind, material) = resolver.fields(&material_key)?;
        if kind != 21 {
            return Err("Font material is unavailable".into());
        }
        let data = resolver.texture_bytes(&atlas_key, &atlas)?;
        ("sdf", "map-font.atlas", font_fields, atlas, material, data)
    } else {
        let (font, data) = open_type(&mut resolver, &font_key)?;
        (
            "opentype",
            "map-font.ttf",
            font,
            Value::Null,
            Value::Null,
            data,
        )
    };
    let report = json!({"format":"mapper-client-map-labels","schema":1,"extractor_sha256":crate::source_fingerprint(),
        "prefab":asset,"scene":scene,"root_object":root_id,"marker":marker_fields,
        "text_object":text_key.1,"text":text,"root_transform":root_transform,"text_transform":text_transform,
        "font_file":font_key.0,"font_object":font_key.1,"font":font,"font_bytes":data.len(),
        "font_kind":kind,"atlas":atlas,"material":material,
        "font_sha256":format!("{:x}",Sha256::digest(&data)),"font_payload":payload,
        "bundle_files":resolver.used});
    fs::create_dir(output).map_err(|e| e.to_string())?;
    fs::write(output.join(payload), data).map_err(|e| e.to_string())?;
    crate::write_new(&output.join("labels.json"), &report)
}
