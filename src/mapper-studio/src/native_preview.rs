//! Crash-isolated adapter to a user-owned native library. No native code loads in this process.
use crate::{TerrainKey, composition, installation};
use eframe::egui::{Color32, ColorImage};
use papyrus_terrain::native_layers::{self, Layers};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[cfg(windows)]
mod terrain_pipe;

pub const VERSION: &str = "papyrus-native-preview-1";
pub const OCEAN_VERSION: &str = "papyrus-native-ocean-preview-1";
pub fn is_native(engine: &str) -> bool {
    matches!(engine, VERSION | OCEAN_VERSION)
}
pub fn source_key(key: &TerrainKey) -> TerrainKey {
    let mut raw = processing_key(key);
    raw.rivers = None;
    raw
}
/// Derived terrain identity retains the river recipe and independent seed.
pub fn processing_key(key: &TerrainKey) -> TerrainKey {
    let mut raw = key.clone();
    if is_native(&raw.engine) {
        raw.engine = VERSION.into();
    }
    raw
}
pub const RESOLUTION: usize = 513;
pub fn supported(source: &installation::Installation) -> bool {
    crate::native_profile::legacy(source).is_some()
}
pub fn worker_path() -> Result<PathBuf, String> {
    Ok(std::env::current_exe()
        .map_err(|e| e.to_string())?
        .parent()
        .ok_or("Missing executable directory")?
        .join("native/Papyrus.NativeWorker.exe"))
}
pub struct NativePreview {
    pub heights: Vec<i16>,
    pub image: ColorImage,
    pub metrics: serde_json::Value,
    pub layers: Arc<papyrus_terrain::native_layers::Layers>,
}
struct HashReader<R> {
    inner: R,
    hash: Sha256,
}
impl<R: Read> Read for HashReader<R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let count = self.inner.read(bytes)?;
        self.hash.update(&bytes[..count]);
        Ok(count)
    }
}
type TerrainPayload = (Layers, String);

fn read_terrain(
    source: impl Read,
    expected_bytes: usize,
    cancel: &AtomicBool,
) -> Result<TerrainPayload, String> {
    let mut reader = HashReader {
        inner: source.take((expected_bytes + 1) as u64),
        hash: Sha256::new(),
    };
    let layers = native_layers::read(&mut reader, || cancel.load(Ordering::Relaxed))?;
    Ok((layers, format!("{:x}", reader.hash.finalize())))
}

struct Running {
    process: Child,
    #[cfg(windows)]
    pipe: Option<terrain_pipe::Pipe>,
}
impl Running {
    fn read_layers(
        &mut self,
        folder: &Path,
        expected_bytes: usize,
        cancel: &AtomicBool,
    ) -> Result<TerrainPayload, String> {
        #[cfg(windows)]
        if let Some(pipe) = self.pipe.as_mut()
            && let Some(payload) = pipe.finish()?
        {
            return Ok(payload);
        }
        let file = File::open(folder.join("terrain.bin")).map_err(|e| e.to_string())?;
        if file.metadata().map_err(|e| e.to_string())?.len() != expected_bytes as u64 {
            return Err("Native full-layer size does not match recipe".into());
        }
        read_terrain(file, expected_bytes, cancel)
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        // Close the writer before dropping the pipe reader, even on cancellation.
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

pub fn decode(bytes: &[u8]) -> Result<(Vec<i16>, ColorImage), String> {
    let cells = RESOLUTION * RESOLUTION;
    if bytes.len() != 8 + cells * 6
        || &bytes[..4] != b"PNP1"
        || bytes[4..8] != (RESOLUTION as u32).to_le_bytes()
    {
        return Err("Invalid native preview transport".into());
    }
    let heights: Vec<_> = bytes[8..8 + cells * 2]
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    if heights.iter().any(|h| !(0..=32766).contains(h)) || heights.iter().all(|h| *h == heights[0])
    {
        return Err("Invalid native preview heights".into());
    }
    let bgra = &bytes[8 + cells * 2..];
    if bgra.chunks_exact(4).any(|b| b[3] != 255) {
        return Err("Invalid native preview alpha".into());
    }
    let pixels = bgra
        .chunks_exact(4)
        .map(|b| Color32::from_rgb(b[2], b[1], b[0]))
        .collect();
    Ok((heights, ColorImage::new([RESOLUTION, RESOLUTION], pixels)))
}

pub fn generate(
    root: &Path,
    worker: &Path,
    key: &TerrainKey,
    cancel: &AtomicBool,
    mut progress: impl FnMut(&'static str, f32),
) -> Result<Option<NativePreview>, String> {
    generate_with_snapshots(root, worker, key, cancel, false, |label, fraction, _| {
        progress(label, fraction)
    })
}

pub const SNAPSHOT_RESOLUTION: usize = 129;
/// Bounded display-only transport. Never accepted as terrain or saved in the raw cache.
pub fn decode_snapshot(bytes: &[u8], stage: u32) -> Result<ColorImage, String> {
    let n = SNAPSHOT_RESOLUTION;
    if !(1..=4).contains(&stage)
        || bytes.len() != 12 + n * n * 4
        || &bytes[..4] != b"PNS1"
        || bytes[4..8] != stage.to_le_bytes()
        || bytes[8..12] != (n as u32).to_le_bytes()
        || bytes[12..].chunks_exact(4).any(|p| p[3] != 255)
    {
        return Err("Invalid native stage snapshot".into());
    }
    Ok(ColorImage::new(
        [n, n],
        bytes[12..]
            .chunks_exact(4)
            .map(|p| Color32::from_rgb(p[2], p[1], p[0]))
            .collect(),
    ))
}

pub fn generate_with_snapshots(
    root: &Path,
    worker: &Path,
    key: &TerrainKey,
    cancel: &AtomicBool,
    live: bool,
    progress: impl FnMut(&'static str, f32, Option<ColorImage>),
) -> Result<Option<NativePreview>, String> {
    generate_inner(root, worker, key, cancel, true, live, progress)
}

pub(crate) fn generate_map_layers(
    root: &Path,
    worker: &Path,
    key: &TerrainKey,
    cancel: &AtomicBool,
    progress: impl FnMut(&'static str, f32, Option<ColorImage>),
) -> Result<Option<NativePreview>, String> {
    generate_inner(root, worker, key, cancel, false, false, progress)
}

fn generate_inner(
    root: &Path,
    worker: &Path,
    key: &TerrainKey,
    cancel: &AtomicBool,
    overview: bool,
    live: bool,
    mut progress: impl FnMut(&'static str, f32, Option<ColorImage>),
) -> Result<Option<NativePreview>, String> {
    if cancel.load(Ordering::Relaxed) {
        return Ok(None);
    }
    let mut timing = composition::StageTimer::start("native_terrain");
    let expected = key
        .source
        .as_ref()
        .ok_or("Bind this project's saved Rust installation before generation")?;
    if !is_native(&key.engine)
        || !(1000..=6000).contains(&key.size)
        || !(1..=crate::MAX_SEED).contains(&key.seed)
        || !key
            .biome_seed
            .is_some_and(|s| (1..=crate::MAX_SEED).contains(&s))
    {
        return Err("Invalid native preview recipe".into());
    }
    let profile = crate::native_profile::resolve(key)?;
    let profile_sha256 = profile.sha256()?;
    progress("Checking Rust installation", 0.02, None);
    let actual = match installation::inspect(root, || cancel.load(Ordering::Relaxed)) {
        Ok(actual) => actual,
        Err(_) if cancel.load(Ordering::Relaxed) => return Ok(None),
        Err(e) => return Err(e),
    };
    if &actual != expected {
        return Err("Rust installation changed. Recheck and explicitly accept the new source before generating.".into());
    }
    if !worker.is_file() {
        return Err("Native terrain worker is missing. Build/install the companion worker before using RustNative.".into());
    }
    composition::record_timing(&mut timing, "source_verified");
    let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let profile_path = temp.path().join("native-profile.json");
    std::fs::write(&profile_path, profile.bytes()?).map_err(|e| e.to_string())?;
    let error_path = temp.path().join("stderr.txt");
    let error_file = File::create(&error_path).map_err(|e| e.to_string())?;
    let mut command = Command::new(worker);
    command
        .arg(root.join(&actual.native.relative_path))
        .arg(&actual.native.sha256)
        .arg(temp.path())
        .arg(key.size.to_string())
        .arg(key.seed.to_string())
        .arg(key.biome_seed.unwrap().to_string())
        .arg(&profile_path)
        .arg(&profile_sha256)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(error_file));
    // Older custom workers can ignore this hint and still provide valid layers.
    if overview {
        command.env_remove("PAPYRUS_NATIVE_TERRAIN_ONLY");
    } else {
        command.env("PAPYRUS_NATIVE_TERRAIN_ONLY", "1");
    }
    if live {
        command.arg("--stage-previews");
    }
    command.env_remove("PAPYRUS_NATIVE_TERRAIN_PIPE");
    let (hr, sr) = native_layers::dimensions(key.size)?;
    let expected_bytes = 24 + hr * hr * 2 + sr * sr * 17;
    #[cfg(windows)]
    let pipe = if crate::generation_resources::current().effective_workers() > 1 {
        terrain_pipe::Pipe::configure(&mut command, temp.path())
    } else {
        None
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = Running {
        process: command
            .spawn()
            .map_err(|e| format!("Cannot start native worker: {e}"))?,
        #[cfg(windows)]
        pipe,
    };
    let started = Instant::now();
    let mut stage = 0;
    let mut snapshot_stage = 0;
    let exit = loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        if started.elapsed() > Duration::from_secs(120) {
            return Err("Native terrain worker exceeded the two-minute limit".into());
        }
        let exit = child.process.try_wait().map_err(|e| e.to_string())?;
        if live {
            // Completed files are immutable, atomically published by the worker.
            // Prefer the newest ready snapshot; never queue full-resolution buffers.
            for next in ((snapshot_stage + 1)..=4).rev() {
                if let Ok(file) = File::open(temp.path().join(format!("stage-{next}.bin"))) {
                    let mut bytes = Vec::new();
                    file.take((13 + SNAPSHOT_RESOLUTION * SNAPSHOT_RESOLUTION * 4) as u64)
                        .read_to_end(&mut bytes)
                        .map_err(|e| e.to_string())?;
                    let image = decode_snapshot(&bytes, next)?;
                    snapshot_stage = next;
                    let labels = [
                        "",
                        "Terrain ready",
                        "Biomes ready",
                        "Topology ready",
                        "Ground textures ready",
                    ];
                    progress(labels[next as usize], next as f32 * 0.17, Some(image));
                    break;
                }
            }
        }
        if let Some(exit) = exit {
            break exit;
        }
        if let Ok(value) = std::fs::read_to_string(temp.path().join("progress.txt"))
            && let Ok(next) = value.trim().parse::<usize>()
            && next > stage
            && next <= 5
        {
            stage = next;
            let labels = [
                "",
                "Native terrain",
                "Native biomes",
                "Native topology",
                "Native ground textures",
                "Writing native terrain",
            ];
            progress(labels[stage], stage as f32 * 0.17, None);
        }
        #[cfg(windows)]
        if stage == 5
            && let Some(pipe) = &mut child.pipe
            && pipe.try_attach(expected_bytes)?
        {
            composition::record_timing(&mut timing, "terrain_pipe_connected");
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    if !exit.success() {
        let mut message = String::new();
        let _ = File::open(error_path).and_then(|f| f.take(4096).read_to_string(&mut message));
        return Err(format!(
            "Native terrain worker failed ({exit}): {}",
            message.trim()
        ));
    }
    composition::record_timing(&mut timing, "worker_finished");
    let preview = if overview {
        let mut bytes = Vec::new();
        File::open(temp.path().join("preview.bin"))
            .map_err(|e| e.to_string())?
            .take((9 + RESOLUTION * RESOLUTION * 6) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        Some(decode(&bytes)?)
    } else {
        None
    };
    let mut metadata = Vec::new();
    File::open(temp.path().join("metrics.json"))
        .map_err(|e| e.to_string())?
        .take(16385)
        .read_to_end(&mut metadata)
        .map_err(|e| e.to_string())?;
    if metadata.len() > 16384 {
        return Err("Native metrics exceed limit".into());
    }
    let metrics: serde_json::Value =
        serde_json::from_slice(&metadata).map_err(|e| e.to_string())?;
    if metrics["adapter"] != VERSION
        || metrics["nativeProfileSha256"] != profile_sha256
        || metrics["sourceSha256"] != expected.native.sha256
        || metrics["size"] != key.size
        || metrics["terrainSeed"] != key.seed
        || metrics["biomeSeed"] != key.biome_seed.unwrap()
    {
        return Err("Native worker response does not match recipe/source".into());
    }
    if cancel.load(Ordering::Relaxed) {
        return Ok(None);
    }
    progress("Validating full-resolution terrain", 0.94, None);
    let (layers, payload_sha256) =
        match child.read_layers(temp.path(), expected_bytes, cancel) {
            Ok(layers) => layers,
            Err(_) if cancel.load(Ordering::Relaxed) => return Ok(None),
            Err(error) => return Err(error),
        };
    let sampled = layers.sample_heights(RESOLUTION)?;
    if metrics["fullTerrainSha256"] != payload_sha256
        || layers.size != key.size
        || layers.terrain_seed != key.seed
        || Some(layers.biome_seed) != key.biome_seed
        || sampled.iter().all(|&height| height == sampled[0])
        || preview.as_ref().is_some_and(|(heights, _)| *heights != sampled)
    {
        return Err(
            "Native full-resolution layers do not match preview, checksum or recipe".into(),
        );
    }
    if cancel.load(Ordering::Relaxed) {
        return Ok(None);
    }
    composition::record_timing(&mut timing, "layers_validated");
    let (heights, image) = preview.unwrap_or_else(|| (sampled, ColorImage::new([0, 0], Vec::new())));
    Ok(Some(NativePreview {
        heights,
        image,
        metrics,
        layers: Arc::new(layers),
    }))
}

/// Reprocessing cached arrays still verifies the currently bound installation.
pub fn verify_source(root: &Path, key: &TerrainKey, cancel: &AtomicBool) -> Result<bool, String> {
    if cancel.load(Ordering::Relaxed) {
        return Ok(false);
    }
    let expected = key
        .source
        .as_ref()
        .ok_or("Missing saved native source identity")?;
    expected.validate()?;
    if !is_native(&key.engine) {
        return Err("Unsupported native source or processing recipe".into());
    }
    crate::native_profile::resolve(key)?;
    let actual = match installation::inspect(root, || cancel.load(Ordering::Relaxed)) {
        Ok(actual) => actual,
        Err(_) if cancel.load(Ordering::Relaxed) => return Ok(false),
        Err(error) => return Err(error),
    };
    if &actual != expected {
        return Err("Rust installation changed. Recheck before processing cached terrain.".into());
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stage_snapshot_rejects_wrong_stage_size_alpha_and_trailing_bytes() {
        let mut bytes = b"PNS1".to_vec();
        bytes.extend(2_u32.to_le_bytes());
        bytes.extend((SNAPSHOT_RESOLUTION as u32).to_le_bytes());
        for _ in 0..SNAPSHOT_RESOLUTION * SNAPSHOT_RESOLUTION {
            bytes.extend([1, 2, 3, 255]);
        }
        let image = decode_snapshot(&bytes, 2).unwrap();
        assert_eq!(image.pixels[0], Color32::from_rgb(3, 2, 1));
        assert!(decode_snapshot(&bytes, 1).is_err());
        assert!(decode_snapshot(&bytes, 5).is_err());
        assert!(decode_snapshot(&bytes[..11], 2).is_err());
        bytes.push(0);
        assert!(decode_snapshot(&bytes, 2).is_err());
        bytes.pop();
        *bytes.last_mut().unwrap() = 0;
        assert!(decode_snapshot(&bytes, 2).is_err());
        bytes[8] = 0;
        assert!(decode_snapshot(&bytes, 2).is_err());
    }
    #[test]
    fn bounded_transport_rejects_corruption_and_preserves_bgra_orientation() {
        let cells = RESOLUTION * RESOLUTION;
        let mut bytes = b"PNP1".to_vec();
        bytes.extend((RESOLUTION as u32).to_le_bytes());
        for i in 0..cells {
            bytes.extend(((i % 32767) as i16).to_le_bytes());
        }
        for _ in 0..cells {
            bytes.extend([12, 34, 56, 255]);
        }
        let (heights, image) = decode(&bytes).unwrap();
        assert_eq!(heights[1], 1);
        assert_eq!(image.pixels[0], Color32::from_rgb(56, 34, 12));
        assert!(decode(&bytes[..bytes.len() - 1]).is_err());
        bytes.push(0);
        assert!(decode(&bytes).is_err());
        bytes.pop();
        bytes[8] = 255;
        bytes[9] = 255;
        assert!(decode(&bytes).is_err());
        bytes[8] = 0;
        bytes[9] = 0;
        *bytes.last_mut().unwrap() = 0;
        assert!(decode(&bytes).is_err());
    }
}
