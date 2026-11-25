//! Installation-bound map typography, cached independently of generation recipes.
mod layout;
use crate::installation::{self, Installation, InstallationKind, SourceFile};
use eframe::egui::ColorImage;
pub use layout::{Label, Quad, Style};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Component, Path},
    process::Command,
    sync::Arc,
};

pub const MARKER_SIDE: f32 = 28.;
const DIRECTORY: &str = "papyrus-map-label-cache";
const VERSION: &str = "mapper-client-map-labels-1";
const METADATA_LIMIT: u64 = 4 * 1024 * 1024;
const PAYLOAD_LIMIT: u64 = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    None,
    #[default]
    Icons,
    Text,
}
impl Mode {
    pub const ALL: [Self; 3] = [Self::None, Self::Icons, Self::Text];
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Icons => "Icons",
            Self::Text => "Text",
        }
    }
}

pub struct Prepared {
    pub metadata: Arc<Value>,
    pub installation: Installation,
    pub sha256: String,
    pub style: Style,
    pub image: Arc<ColorImage>,
    pub image_sha256: String,
    pub alpha: Arc<[u8]>,
    pub grid: Option<Arc<crate::map_grid::Appearance>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_modes_are_stable_and_default_to_icons() {
        assert_eq!(Mode::default(), Mode::Icons);
        for mode in Mode::ALL {
            let bytes = serde_json::to_vec(&mode).unwrap();
            assert_eq!(serde_json::from_slice::<Mode>(&bytes).unwrap(), mode);
        }
        let older: crate::preferences::Preferences =
            serde_json::from_str("{\"schema\":1}").unwrap();
        assert_eq!(older.monument_markers, Mode::Icons);
    }

    #[test]
    fn cancellation_precedes_cache_or_source_access() {
        let expected = Installation {
            kind: InstallationKind::Client,
            native: SourceFile {
                relative_path: String::new(),
                sha256: String::new(),
                bytes: 0,
            },
            scene_manifest: None,
        };
        assert!(
            prepare_in(
                Path::new("missing-cache"),
                Path::new("missing-source"),
                &expected,
                || true
            )
            .unwrap()
            .is_none()
        );
        assert!(serde_json::from_str::<Mode>("\"unknown\"").is_err());
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    format: String,
    schema: u32,
    adapter: String,
    installation: Installation,
    sources: Vec<SourceFile>,
    metadata_sha256: String,
    payload_sha256: String,
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn read(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("Map label data exceeds its byte budget".into());
    }
    Ok(bytes)
}
fn bundle_path(value: &str) -> bool {
    value.starts_with("Bundles/")
        && value.ends_with(".bundle")
        && Path::new(value)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}
fn receipt_sources(expected: &Installation, metadata: &Value) -> Result<Vec<String>, String> {
    let manifest = expected
        .scene_manifest
        .as_ref()
        .ok_or("Map labels require a scene manifest")?;
    let bundles = metadata["bundle_files"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 16)
        .ok_or("Invalid map label bundle receipts")?;
    let mut paths = vec![
        expected.native.relative_path.clone(),
        manifest.relative_path.clone(),
    ];
    for value in bundles {
        let path = value
            .as_str()
            .filter(|v| bundle_path(v))
            .ok_or("Invalid map label bundle path")?;
        if paths.iter().any(|existing| existing == path) {
            return Err("Duplicate map label bundle receipt".into());
        }
        paths.push(path.into());
    }
    Ok(paths)
}
fn decode(folder: &Path, expected: &Installation) -> Result<(Prepared, Receipt), String> {
    let receipt_bytes = read(&folder.join("receipt.json"), 65536)?;
    let receipt: Receipt = serde_json::from_slice(&receipt_bytes).map_err(|e| e.to_string())?;
    let bytes = read(&folder.join("labels.json"), METADATA_LIMIT)?;
    let metadata: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let paths = receipt_sources(expected, &metadata)?;
    if receipt.format != "mapper-map-label-cache"
        || receipt.schema != 1
        || receipt.adapter != VERSION
        || receipt.installation != *expected
        || receipt.sources.len() != paths.len()
        || receipt.sources[0] != expected.native
        || expected.scene_manifest.as_ref() != receipt.sources.get(1)
        || receipt
            .sources
            .iter()
            .zip(paths)
            .any(|(source, path)| source.relative_path != path)
        || digest(&bytes) != receipt.metadata_sha256
        || metadata["format"] != "mapper-client-map-labels"
        || metadata["schema"] != 1
        || metadata["font_kind"] != "sdf"
        || metadata["font_payload"] != "map-font.atlas"
    {
        return Err("Map label cache identity or format differs".into());
    }
    let payload = read(&folder.join("map-font.atlas"), PAYLOAD_LIMIT)?;
    if digest(&payload) != receipt.payload_sha256
        || metadata["font_sha256"] != receipt.payload_sha256
        || metadata["font_bytes"].as_u64() != Some(payload.len() as u64)
    {
        return Err("Map font atlas differs from its receipt".into());
    }
    let style = Style::decode(&metadata)?;
    let image = style.image(&payload)?;
    let image_sha256 = digest(bytemuck::cast_slice(&image.pixels));
    Ok((
        Prepared {
            metadata: Arc::new(metadata),
            installation: expected.clone(),
            sha256: digest(&receipt_bytes),
            style,
            image: Arc::new(image),
            image_sha256,
            alpha: payload.into(),
            grid: None,
        },
        receipt,
    ))
}
fn cached(
    base: &Path,
    root: &Path,
    expected: &Installation,
    stop: &impl Fn() -> bool,
) -> Result<Option<Prepared>, String> {
    let entries = match fs::read_dir(base) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    for (index, entry) in entries.enumerate() {
        if stop() {
            return Ok(None);
        }
        if index >= 128 {
            return Err("Map label snapshot count exceeds its budget".into());
        }
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_dir()
            || !entry.file_name().to_str().is_some_and(|name| {
                name.len() == 64
                    && name
                        .bytes()
                        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            })
        {
            continue;
        }
        let Ok((prepared, receipt)) = decode(&entry.path(), expected) else {
            continue;
        };
        let mut current = true;
        for source in &receipt.sources {
            if stop() {
                return Ok(None);
            }
            if installation::hash_file(root, &source.relative_path, 16 * 1024 * 1024 * 1024, stop)?
                != *source
            {
                current = false;
                break;
            }
        }
        if current {
            return Ok(Some(prepared));
        }
    }
    Ok(None)
}
/// Keep the bundle inventory read-only until the captured dependencies have
/// their hashes. Discovery itself reads directory entries, without hashing every bundle.
fn lease_bundles(root: &Path) -> Result<Vec<fs::File>, String> {
    let mut directories = vec![root.join("Bundles")];
    let mut leases = Vec::new();
    let mut visited = 0;
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory).map_err(|e| e.to_string())? {
            visited += 1;
            if visited > 4096 {
                return Err("Map label bundle inventory exceeds its path budget".into());
            }
            let entry = entry.map_err(|e| e.to_string())?;
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_dir() {
                directories.push(entry.path());
            }
            if kind.is_file() && entry.path().extension().is_some_and(|v| v == "bundle") {
                leases.push(crate::source_lease::open(&entry.path()).map_err(|e| e.to_string())?);
            }
        }
    }
    Ok(leases)
}
pub fn prepare(
    root: &Path,
    expected: &Installation,
    stop: impl Fn() -> bool,
) -> Result<Option<Prepared>, String> {
    if stop() {
        return Ok(None);
    }
    let cache = crate::storage::local_data()?.join(DIRECTORY);
    let _verification = installation::VerificationScope::default();
    let mut font = prepare_in(&cache, root, expected, &stop)?;
    if let Some(font) = &mut font {
        font.grid = crate::map_grid::prepare_appearance(&cache, root, &stop).map(Arc::new);
    }
    Ok(font)
}
pub fn prepare_in(
    cache: &Path,
    root: &Path,
    expected: &Installation,
    stop: impl Fn() -> bool,
) -> Result<Option<Prepared>, String> {
    if stop() {
        return Ok(None);
    }
    expected.validate()?;
    if expected.kind != InstallationKind::Client {
        return Ok(None);
    }
    let _verification = installation::VerificationScope::default();
    let base = cache.join(&expected.native.sha256);
    if let Some(value) = cached(&base, root, expected, &stop)? {
        return Ok(Some(value));
    }
    let reader = crate::native_catalog::reader()
        .ok_or("The native asset reader is required for client map text")?;
    let _leases = lease_bundles(root)?;
    if stop() {
        return Ok(None);
    }
    fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    let temporary = tempfile::Builder::new()
        .prefix(".map-labels-")
        .tempdir_in(&base)
        .map_err(|e| e.to_string())?;
    let output = temporary.path().join("output");
    let progress = crate::loading_progress::begin("Reading client map text", None);
    let mut command = Command::new(reader);
    command.arg("--map-labels").arg(root).arg(&output);
    if !crate::metadata_runtime::execute(command, progress.reporter(), &stop)? {
        return Ok(None);
    }
    let metadata_bytes = read(&output.join("labels.json"), METADATA_LIMIT)?;
    let metadata: Value = serde_json::from_slice(&metadata_bytes).map_err(|e| e.to_string())?;
    if metadata["extractor_sha256"] != crate::native_catalog::fingerprint() {
        return Err("Map label reader differs from this Mapper build".into());
    }
    let paths = receipt_sources(expected, &metadata)?;
    let sources = paths
        .iter()
        .map(|path| installation::hash_file(root, path, 16 * 1024 * 1024 * 1024, &stop))
        .collect::<Result<Vec<_>, _>>()?;
    let receipt = Receipt {
        format: "mapper-map-label-cache".into(),
        schema: 1,
        adapter: VERSION.into(),
        installation: expected.clone(),
        sources,
        metadata_sha256: digest(&metadata_bytes),
        payload_sha256: metadata["font_sha256"]
            .as_str()
            .ok_or("Missing map font digest")?
            .into(),
    };
    crate::profile::write_document(&output.join("receipt.json"), &receipt, false)?;
    let (prepared, _) = decode(&output, expected)?;
    if stop() {
        return Ok(None);
    }
    let destination = base.join(&prepared.sha256);
    let parent = base.canonicalize().map_err(|e| e.to_string())?;
    if !crate::client_cache::ordinary(&output)?
        || !output
            .canonicalize()
            .map_err(|e| e.to_string())?
            .starts_with(&parent)
        || destination
            .parent()
            .ok_or("Missing map label cache parent")?
            .canonicalize()
            .map_err(|e| e.to_string())?
            != parent
    {
        return Err("Map label publication escaped its cache directory".into());
    }
    if destination.exists() {
        let (existing, _) = decode(&destination, expected)?;
        return Ok(Some(existing));
    }
    if let Err(error) = fs::rename(&output, &destination) {
        if destination.is_dir() {
            let (existing, _) = decode(&destination, expected)?;
            return Ok(Some(existing));
        }
        return Err(error.to_string());
    }
    Ok(Some(prepared))
}
