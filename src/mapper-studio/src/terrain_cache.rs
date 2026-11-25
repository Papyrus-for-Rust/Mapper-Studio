//! Versioned local native terrain snapshots. No game binaries or prefab textures.
pub(crate) mod erosion;

use crate::{TerrainKey, native_preview, native_processing::Source};
use papyrus_terrain::native_layers;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

const MAGIC: &[u8; 8] = b"PTCACHE1";
const TERRAIN_ONLY_MAGIC: &[u8; 8] = b"PTCACHE2";
const PREVIEW_BYTES: usize = 8 + native_preview::RESOLUTION * native_preview::RESOLUTION * 6;
const KEY_LIMIT: usize = 16 * 1024;
const TRANSFER_BYTES: usize = 64 * 1024;
const DISK_BUDGET: u64 = 4 * 1024 * 1024 * 1024;

pub fn directory() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .and_then(|p| crate::storage::prepare(&p).ok())
        .map(|p| p.join("papyrus-terrain-cache/v1"))
        .or_else(|| {
            std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .map(|p| p.join("papyrus/terrain/v1"))
        })
}

fn identity(key: &TerrainKey) -> Result<Vec<u8>, String> {
    let source = key
        .source
        .as_ref()
        .ok_or("Terrain cache requires a source identity")?;
    source.validate()?;
    if !native_preview::is_native(&key.engine)
        || !(1..=crate::MAX_SEED).contains(&key.seed)
        || !key
            .biome_seed
            .is_some_and(|s| (1..=crate::MAX_SEED).contains(&s))
    {
        return Err("Unsupported terrain cache recipe".into());
    }
    native_layers::dimensions(key.size)?;
    crate::native_profile::resolve(key)?;
    serde_json::to_vec(&native_preview::source_key(key)).map_err(|e| e.to_string())
}

fn path(root: &Path, key: &[u8]) -> PathBuf {
    root.join(format!("{:x}.ptc", Sha256::digest(key)))
}

fn layer_bytes(hr: usize, sr: usize) -> u64 {
    (24 + hr * hr * 2 + sr * sr * 17) as u64
}

fn stopped(cancel: &impl Fn() -> bool) -> Result<(), String> {
    if cancel() {
        Err("Terrain cache cancelled".into())
    } else {
        Ok(())
    }
}

/// Missing entries return None; rejected entries are reported to the caller, which
/// may regenerate. A terrain-only snapshot returns an empty overview.
pub fn load(
    root: &Path,
    key: &TerrainKey,
    cancel: impl Fn() -> bool,
) -> Result<Option<Source>, String> {
    stopped(&cancel)?;
    let expected = identity(key)?;
    let file = match File::open(path(root, &expected)) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let (hr, sr) = native_layers::dimensions(key.size)?;
    let layer_len = layer_bytes(hr, sr);
    let terrain_len = 28 + expected.len() as u64 + layer_len + 32;
    let has_preview = match file.metadata().map_err(|e| e.to_string())?.len() {
        n if n == terrain_len => false,
        n if n == terrain_len + PREVIEW_BYTES as u64 => true,
        _ => return Err("Terrain cache length mismatch".into()),
    };
    let mut input = Checksummed {
        inner: BufReader::new(file),
        hash: Sha256::new(),
    };
    let mut header = [0; 28];
    input.read_exact(&mut header).map_err(|e| e.to_string())?;
    let key_len = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
    let magic = if has_preview { MAGIC } else { TERRAIN_ONLY_MAGIC };
    if &header[..8] != magic || key_len > KEY_LIMIT || key_len != expected.len() {
        return Err("Unsupported terrain cache header".into());
    }
    let cells = (sr * sr) as u64;
    let biome = u64::from_le_bytes(header[12..20].try_into().unwrap());
    let splat = u64::from_le_bytes(header[20..28].try_into().unwrap());
    if biome > cells || splat > cells {
        return Err("Invalid terrain cache normalization counts".into());
    }
    let mut saved_key = vec![0; key_len];
    input
        .read_exact(&mut saved_key)
        .map_err(|e| e.to_string())?;
    if saved_key != expected {
        return Err("Terrain cache recipe/source mismatch".into());
    }
    stopped(&cancel)?;
    let mut preview = vec![0; if has_preview { PREVIEW_BYTES } else { 0 }];
    input.read_exact(&mut preview).map_err(|e| e.to_string())?;
    // Hash the same stream the decoder consumes. No decoded data is returned
    // until the verified payload ends exactly at the recorded checksum.
    let mut layers = native_layers::read_normalized((&mut input).take(layer_len), &cancel)?;
    let mut checksum = [0; 32];
    input
        .inner
        .read_exact(&mut checksum)
        .map_err(|e| e.to_string())?;
    if input.hash.finalize().as_slice() != checksum {
        return Err("Terrain cache checksum mismatch".into());
    }
    if input.inner.read(&mut [0]).map_err(|e| e.to_string())? != 0 {
        return Err("Trailing terrain cache data".into());
    }
    let sampled = layers.sample_heights(native_preview::RESOLUTION)?;
    if layers.size != key.size
        || layers.terrain_seed != key.seed
        || Some(layers.biome_seed) != key.biome_seed
        || sampled.iter().all(|&height| height == sampled[0])
    {
        return Err("Terrain cache layers disagree with recipe or preview".into());
    }
    let image = if has_preview {
        let (heights, image) = native_preview::decode(&preview)?;
        if heights != sampled {
            return Err("Terrain cache layers disagree with recipe or preview".into());
        }
        image
    } else {
        eframe::egui::ColorImage::new([0, 0], Vec::new())
    };
    layers.normalized_biome_cells = biome as usize;
    layers.normalized_splat_cells = splat as usize;
    stopped(&cancel)?;
    Ok(Some(Source {
        terrain: None,
        native_base: None,
        rivers: None,
        palette: None,
        key: native_preview::source_key(key),
        image,
        heights: Arc::new(sampled),
        layers: Arc::new(layers),
    }))
}

struct Checksummed<T> {
    inner: T,
    hash: Sha256,
}
impl<R: Read> Read for Checksummed<R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(bytes)?;
        self.hash.update(&bytes[..n]);
        Ok(n)
    }
}
impl<W: Write> Write for Checksummed<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(bytes)?;
        self.hash.update(&bytes[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Publish atomically in the same directory. Interrupted writes leave no accepted
/// entry. Cache write failures must not prevent use of the generated terrain.
/// The checksummed snapshot can be regenerated after a system crash, so publishing
/// it only waits for the buffered writer to hand its bytes to the operating system.
pub fn save(root: &Path, source: &Source, cancel: impl Fn() -> bool) -> Result<(), String> {
    save_with_budget(root, source, cancel, DISK_BUDGET)
}

fn save_with_budget(
    root: &Path,
    source: &Source,
    cancel: impl Fn() -> bool,
    budget: u64,
) -> Result<(), String> {
    stopped(&cancel)?;
    if source.rivers.is_some() || source.key.rivers.is_some() || source.native_base.is_some() || source.terrain.is_some() {
        return Err("Derived terrain cannot replace an immutable native cache entry".into());
    }
    let key = identity(&source.key)?;
    let layers = &source.layers;
    let (hr, sr) = native_layers::dimensions(source.key.size)?;
    let cells = sr * sr;
    let has_preview = source.image.size != [0, 0] || !source.image.pixels.is_empty();
    if key.len() > KEY_LIMIT
        || layers.height_resolution != hr
        || layers.surface_resolution != sr
        || layers.size != source.key.size
        || layers.terrain_seed != source.key.seed
        || Some(layers.biome_seed) != source.key.biome_seed
        || layers.height.len() != hr * hr
        || layers.biome.len() != cells * 5
        || layers.splat.len() != cells * 8
        || layers.topology.len() != cells
        || layers.normalized_biome_cells > cells
        || layers.normalized_splat_cells > cells
        || has_preview && (source.image.size != [native_preview::RESOLUTION; 2]
            || source.image.pixels.len() != native_preview::RESOLUTION * native_preview::RESOLUTION)
        || layers.sample_heights(native_preview::RESOLUTION)? != *source.heights
    {
        return Err("Invalid terrain cache source".into());
    }
    fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let destination = path(root, &key);
    let mut used = 0_u64;
    let mut entries = 0;
    for entry in fs::read_dir(root).map_err(|e| e.to_string())? {
        stopped(&cancel)?;
        entries += 1;
        if entries > 4096 {
            return Err("Terrain cache entry budget exceeded".into());
        }
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.path() != destination && entry.path().extension().is_some_and(|e| e == "ptc") {
            used = used.saturating_add(entry.metadata().map_err(|e| e.to_string())?.len());
        }
    }
    let required = 28 + key.len() as u64 + layer_bytes(hr, sr) + 32
        + if has_preview { PREVIEW_BYTES as u64 } else { 0 };
    if used.saturating_add(required) > budget {
        return Err(
            "Terrain cache is full (4 GiB); generated terrain remains available in memory".into(),
        );
    }
    let mut temp = tempfile::NamedTempFile::new_in(root).map_err(|e| e.to_string())?;
    {
        let mut writer = Checksummed {
            inner: BufWriter::with_capacity(TRANSFER_BYTES, temp.as_file_mut()),
            hash: Sha256::new(),
        };
        let mut write = || -> Result<(), String> {
            let mut row_bytes = Vec::with_capacity(hr * 4);
            let magic = if has_preview { MAGIC } else { TERRAIN_ONLY_MAGIC };
            writer.write_all(magic).map_err(|e| e.to_string())?;
            writer
                .write_all(&(key.len() as u32).to_le_bytes())
                .map_err(|e| e.to_string())?;
            writer
                .write_all(&(layers.normalized_biome_cells as u64).to_le_bytes())
                .map_err(|e| e.to_string())?;
            writer
                .write_all(&(layers.normalized_splat_cells as u64).to_le_bytes())
                .map_err(|e| e.to_string())?;
            writer.write_all(&key).map_err(|e| e.to_string())?;
            if has_preview {
                writer.write_all(b"PNP1").map_err(|e| e.to_string())?;
                writer
                    .write_all(&(native_preview::RESOLUTION as u32).to_le_bytes())
                    .map_err(|e| e.to_string())?;
                for row in source.heights.chunks(native_preview::RESOLUTION) {
                    stopped(&cancel)?;
                    row_bytes.clear();
                    row_bytes.extend(row.iter().flat_map(|h| h.to_le_bytes()));
                    writer.write_all(&row_bytes).map_err(|e| e.to_string())?;
                }
                for row in source.image.pixels.chunks(native_preview::RESOLUTION) {
                    stopped(&cancel)?;
                    row_bytes.clear();
                    for pixel in row {
                        let [r, g, b, a] = pixel.to_array();
                        if a != 255 {
                            return Err("Invalid terrain cache overview alpha".into());
                        }
                        row_bytes.extend([b, g, r, a]);
                    }
                    writer.write_all(&row_bytes).map_err(|e| e.to_string())?;
                }
            }
            writer.write_all(b"PNF1").map_err(|e| e.to_string())?;
            for word in [
                layers.size,
                layers.terrain_seed,
                layers.biome_seed,
                hr as u32,
                sr as u32,
            ] {
                writer
                    .write_all(&word.to_le_bytes())
                    .map_err(|e| e.to_string())?;
            }
            for chunk in layers.height.chunks(TRANSFER_BYTES / std::mem::size_of::<i16>()) {
                stopped(&cancel)?;
                if chunk.iter().any(|h| !(0..=32766).contains(h)) {
                    return Err("Invalid cached terrain height".into());
                }
                #[cfg(target_endian = "little")]
                writer
                    .write_all(bytemuck::cast_slice(chunk))
                    .map_err(|e| e.to_string())?;
                #[cfg(target_endian = "big")]
                {
                    row_bytes.clear();
                    row_bytes.extend(chunk.iter().flat_map(|h| h.to_le_bytes()));
                    writer.write_all(&row_bytes).map_err(|e| e.to_string())?;
                }
            }
            for bytes in [&layers.biome, &layers.splat] {
                for chunk in bytes.chunks(TRANSFER_BYTES) {
                    stopped(&cancel)?;
                    writer.write_all(chunk).map_err(|e| e.to_string())?;
                }
            }
            for chunk in layers.topology.chunks(TRANSFER_BYTES / std::mem::size_of::<i32>()) {
                stopped(&cancel)?;
                #[cfg(target_endian = "little")]
                writer
                    .write_all(bytemuck::cast_slice(chunk))
                    .map_err(|e| e.to_string())?;
                #[cfg(target_endian = "big")]
                {
                    row_bytes.clear();
                    row_bytes.extend(chunk.iter().flat_map(|t| t.to_le_bytes()));
                    writer.write_all(&row_bytes).map_err(|e| e.to_string())?;
                }
            }
            Ok(())
        };
        write()?;
        let checksum = writer.hash.finalize();
        writer
            .inner
            .write_all(&checksum)
            .map_err(|e| e.to_string())?;
        writer.inner.flush().map_err(|e| e.to_string())?;
    }
    stopped(&cancel)?;
    temp.persist(destination).map_err(|e| e.error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Project,
        installation::{Installation, InstallationKind, SourceFile},
    };
    use eframe::egui::{Color32, ColorImage};
    fn fixture() -> Source {
        let project = Project {
            size: 1000,
            preview_engine: native_preview::VERSION.into(),
            installation: Some(Installation {
                kind: InstallationKind::Client,
                native: SourceFile {
                    relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                    sha256: "0f703d43b57672b8bb3dfd29bab0d484f8622fbac66dee7f1f058893ef22a99c"
                        .into(),
                    bytes: 1,
                },
                scene_manifest: None,
            }),
            ..Default::default()
        };
        let (hr, sr) = native_layers::dimensions(project.size).unwrap();
        let cells = sr * sr;
        let mut biome = vec![0; cells * 5];
        biome[..cells].fill(255);
        let mut splat = vec![0; cells * 8];
        splat[..cells].fill(255);
        let layers = native_layers::Layers {
            size: project.size,
            terrain_seed: project.seeds[0].value,
            biome_seed: project.seeds[1].value,
            height_resolution: hr,
            surface_resolution: sr,
            height: (0..hr * hr).map(|i| (i % 32767) as i16).collect(),
            biome,
            splat,
            topology: vec![0x80000180_u32 as i32; cells],
            normalized_biome_cells: 23,
            normalized_splat_cells: 42,
        };
        Source {
            terrain: None,
            native_base: None,
            rivers: None,
            palette: None,
            key: project.terrain_key(),
            heights: Arc::new(layers.sample_heights(native_preview::RESOLUTION).unwrap()),
            image: ColorImage::filled(
                [native_preview::RESOLUTION; 2],
                Color32::from_rgb(20, 40, 80),
            ),
            layers: Arc::new(layers),
        }
    }
    #[test]
    fn derived_river_keys_reuse_raw_cache_but_cannot_overwrite_it() {
        let root = tempfile::tempdir().unwrap();
        let source = fixture();
        save(root.path(), &source, || false).unwrap();
        let mut key = source.key.clone();
        key.rivers = Some(crate::river_generation::Key {
            recipe: crate::river_generation::Recipe {
                engine: papyrus_terrain::river_pipeline::VERSION.into(),
                source_sha256: "a".repeat(64),
                source_document: None,
                objects: None,
            },
            seed: 7,
            limit: None,
        });
        let read = load(root.path(), &key, || false).unwrap().unwrap();
        assert_eq!(read.key, source.key);
        assert!(read.rivers.is_none());
        assert_eq!(read.layers.height, source.layers.height);
        let mut derived = source.clone();
        derived.key = key;
        assert!(save(root.path(), &derived, || false).is_err());
        let replay = load(root.path(), &source.key, || false).unwrap().unwrap();
        assert_eq!(replay.layers.height, source.layers.height);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
    #[test]
    fn derived_terrain_retains_its_native_source_and_cannot_poison_the_raw_cache() {
        let root = tempfile::tempdir().unwrap();
        let mut derived = fixture();
        let raw = derived.layers.clone();
        assert!(std::ptr::eq(
            derived.native_base_layers().unwrap(),
            raw.as_ref()
        ));
        derived.native_base = Some(raw.clone());
        Arc::make_mut(&mut derived.layers).height[0] += 1;
        assert!(std::ptr::eq(
            derived.native_base_layers().unwrap(),
            raw.as_ref()
        ));
        assert_ne!(
            derived.layers.height[0],
            derived.native_base_layers().unwrap().height[0]
        );
        assert!(
            save(root.path(), &derived, || false)
                .unwrap_err()
                .contains("Derived terrain")
        );
        assert!(load(root.path(), &derived.key, || false).unwrap().is_none());
        Arc::make_mut(derived.native_base.as_mut().unwrap()).terrain_seed += 1;
        assert!(derived.native_base_layers().is_err());
    }

    #[test]
    fn disk_roundtrip_restores_all_layers_and_shared_raw_identity() {
        let root = tempfile::tempdir().unwrap();
        let source = fixture();
        assert!(load(root.path(), &source.key, || false).unwrap().is_none());
        save(root.path(), &source, || false).unwrap();
        let mut processed = source.key.clone();
        processed.engine = native_preview::OCEAN_VERSION.into();
        let read = load(root.path(), &processed, || false).unwrap().unwrap();
        assert_eq!(source.key, read.key);
        assert!(read.native_base.is_none());
        assert_eq!(source.heights, read.heights);
        assert_eq!(source.image, read.image);
        assert_eq!(source.layers.height, read.layers.height);
        assert_eq!(source.layers.splat, read.layers.splat);
        assert_eq!(source.layers.biome, read.layers.biome);
        assert_eq!(source.layers.topology, read.layers.topology);
        assert_eq!(read.layers.normalized_biome_cells, 23);
        assert_eq!(read.layers.normalized_splat_cells, 42);
        let mut changed = source.key.clone();
        changed.seed += 1;
        assert!(load(root.path(), &changed, || false).unwrap().is_none());
        changed = source.key.clone();
        changed.source.as_mut().unwrap().native.bytes += 1;
        assert!(load(root.path(), &changed, || false).unwrap().is_none());
    }

    #[test]
    fn terrain_only_snapshot_uses_the_same_key_and_verifies_its_payload() {
        let root = tempfile::tempdir().unwrap();
        let mut source = fixture();
        let destination = path(root.path(), &identity(&source.key).unwrap());
        save(root.path(), &source, || false).unwrap();
        let preview_bytes = destination.metadata().unwrap().len();
        source.image = ColorImage::new([0, 0], Vec::new());
        save(root.path(), &source, || false).unwrap();
        let loaded = load(root.path(), &source.key, || false).unwrap().unwrap();
        assert_eq!(loaded.image.size, [0, 0]);
        assert_eq!(loaded.heights, source.heights);
        assert_eq!(loaded.layers, source.layers);
        let mut bytes = fs::read(&destination).unwrap();
        assert_eq!(&bytes[..8], TERRAIN_ONLY_MAGIC);
        assert_eq!(bytes.len() as u64 + PREVIEW_BYTES as u64, preview_bytes);
        *bytes.last_mut().unwrap() ^= 1;
        fs::write(destination, bytes).unwrap();
        assert!(load(root.path(), &source.key, || false).is_err());
    }
    #[test]
    fn profile_variants_cannot_reuse_legacy_or_changed_parameter_snapshots() {
        let root = tempfile::tempdir().unwrap();
        let mut source = fixture();
        let legacy_key = source.key.clone();
        save(root.path(), &source, || false).unwrap();
        source.key.native_profile = Some(crate::native_profile::resolve(&legacy_key).unwrap());
        assert!(load(root.path(), &source.key, || false).unwrap().is_none());
        save(root.path(), &source, || false).unwrap();
        assert!(load(root.path(), &source.key, || false).unwrap().is_some());
        source
            .key
            .native_profile
            .as_mut()
            .unwrap()
            .settings
            .biome_angle_millidegrees = 180_000;
        assert!(load(root.path(), &source.key, || false).unwrap().is_none());
        assert!(load(root.path(), &legacy_key, || false).unwrap().is_some());
    }
    #[test]
    fn corruption_truncation_and_copied_wrong_identity_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let source = fixture();
        save(root.path(), &source, || false).unwrap();
        let location = path(root.path(), &identity(&source.key).unwrap());
        let mut bytes = fs::read(&location).unwrap();
        let biome_count: [u8; 8] = bytes[12..20].try_into().unwrap();
        bytes[12..20].copy_from_slice(&u64::MAX.to_le_bytes());
        fs::write(&location, &bytes).unwrap();
        assert_eq!(
            load(root.path(), &source.key, || false).err().as_deref(),
            Some("Invalid terrain cache normalization counts"),
        );
        bytes[12..20].copy_from_slice(&biome_count);
        // Changing the last topology word leaves the transport structurally
        // valid. Its exact decoded bytes still have to match the saved digest.
        let last_payload_byte = bytes.len() - 33;
        bytes[last_payload_byte] ^= 1;
        fs::write(&location, &bytes).unwrap();
        assert!(
            load(root.path(), &source.key, || false)
                .err()
                .unwrap()
                .contains("checksum")
        );
        save(root.path(), &source, || false).unwrap(); // Atomic replacement repairs rejection.
        let mut wrong = source.key.clone();
        wrong.seed += 1;
        fs::copy(&location, path(root.path(), &identity(&wrong).unwrap())).unwrap();
        assert!(load(root.path(), &wrong, || false).is_err());
        fs::write(&location, &bytes[..100]).unwrap();
        assert!(load(root.path(), &source.key, || false).is_err());
    }
    #[test]
    fn cancelled_replacement_preserves_accepted_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let source = fixture();
        save(root.path(), &source, || false).unwrap();
        let calls = std::cell::Cell::new(0);
        assert!(
            save(root.path(), &source, || {
                calls.set(calls.get() + 1);
                calls.get() > 100
            })
            .is_err()
        );
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        assert!(load(root.path(), &source.key, || false).unwrap().is_some());
        let calls = std::cell::Cell::new(0);
        assert!(
            load(root.path(), &source.key, || {
                calls.set(calls.get() + 1);
                calls.get() > 5
            })
            .is_err()
        );
    }
    #[test]
    fn full_cache_refuses_new_writes_without_evicting_existing_data() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("occupied.ptc"), b"previous").unwrap();
        assert!(
            save_with_budget(root.path(), &fixture(), || false, 1)
                .unwrap_err()
                .contains("full")
        );
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }
}
