//! Bounded source-bound noise batches in the disposable native worker. Game
//! libraries are never loaded into the workstation process.
use crate::{installation::Installation, native_profile::Profile};
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    fs::File,
    io::Read,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub const MAX_SAMPLES: usize = 1_000_000;
const MAX_REQUEST: usize = 16 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(120);

struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Reuses exact coordinate prefixes within one pinned installation and profile.
#[derive(Default)]
pub(crate) struct PrefixCache {
    profile: Option<Profile>,
    coordinates: Vec<f32>,
    values: Vec<f32>,
}

impl PrefixCache {
    /// Leave room for the modest grid extensions between road paint passes.
    pub(crate) fn road_grid<'a>(
        &self,
        coordinates: &'a [f32],
        spacing: f32,
        frequency: f32,
    ) -> Cow<'a, [f32]> {
        // Keep extra requests well below the worker's JSON size limit.
        const MAX_PREFETCH: usize = 64 * 1024;
        let len = coordinates.len();
        if len < 2 || len >= MAX_PREFETCH || len <= self.coordinates.len() {
            return Cow::Borrowed(coordinates);
        }
        let coordinate = |index: usize| index as f32 * spacing * frequency;
        if coordinates
            .iter()
            .enumerate()
            .any(|(i, value)| coordinate(i).to_bits() != value.to_bits())
        {
            return Cow::Borrowed(coordinates);
        }
        let limit = (len + 256).next_power_of_two().min(MAX_PREFETCH);
        Cow::Owned((0..limit).map(coordinate).collect())
    }

    pub(crate) fn sample(
        &mut self,
        root: &Path,
        worker: &Path,
        source: &Installation,
        profile: &Profile,
        coordinates: &[f32],
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Vec<f32>>, String> {
        if cancelled() {
            return Ok(None);
        }
        profile.matches(source)?;
        let _verification = crate::installation::VerificationScope::default();
        let verified = crate::installation::hash_file(
            root,
            &source.native.relative_path,
            64 * 1024 * 1024,
            &cancelled,
        );
        if cancelled() {
            return Ok(None);
        }
        if verified? != source.native {
            return Err("RustNative changed since noise source selection".into());
        }
        if self.profile.as_ref() != Some(profile) {
            self.coordinates.clear();
            self.values.clear();
            self.profile = Some(profile.clone());
        }
        self.fill(coordinates, &cancelled, |missing| {
            sample(root, worker, source, profile, missing, &cancelled)
        })
    }

    fn fill(
        &mut self,
        coordinates: &[f32],
        cancelled: &impl Fn() -> bool,
        fetch: impl FnOnce(&[f32]) -> Result<Option<Vec<f32>>, String>,
    ) -> Result<Option<Vec<f32>>, String> {
        if !(1..=MAX_SAMPLES).contains(&coordinates.len()) {
            return Err("Invalid or excessive native noise coordinates".into());
        }
        let shared = self.coordinates.len().min(coordinates.len());
        let prefix_matches = self.coordinates[..shared]
            .iter()
            .zip(&coordinates[..shared])
            .all(|(old, next)| old.to_bits() == next.to_bits());
        let keep = if prefix_matches { shared } else { 0 };
        if keep < coordinates.len() {
            let Some(values) = fetch(&coordinates[keep..])? else {
                return Ok(None);
            };
            if cancelled() {
                return Ok(None);
            }
            self.coordinates.truncate(keep);
            self.values.truncate(keep);
            self.coordinates.extend_from_slice(&coordinates[keep..]);
            self.values.extend(values);
        }
        Ok((!cancelled()).then(|| self.values[..coordinates.len()].to_vec()))
    }
}

fn hash_bytes(value: &str) -> Result<[u8; 32], String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("Invalid native noise fingerprint".into());
    }
    let mut bytes = [0; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[i * 2..i * 2 + 2], 16).map_err(|e| e.to_string())?;
    }
    Ok(bytes)
}
fn decode(
    bytes: &[u8],
    count: usize,
    request: &[u8; 32],
    native: &[u8; 32],
    profile: &[u8; 32],
) -> Result<Vec<f32>, String> {
    if !(1..=MAX_SAMPLES).contains(&count)
        || bytes.len() != 136 + count * 4
        || &bytes[..4] != b"PNN1"
        || bytes[4..8] != (count as u32).to_le_bytes()
        || &bytes[8..40] != request
        || &bytes[40..72] != native
        || &bytes[72..104] != profile
        || bytes[bytes.len() - 32..] != Sha256::digest(&bytes[..bytes.len() - 32])[..]
    {
        return Err(
            "Native noise result has a different request, source, profile or checksum".into(),
        );
    }
    let values: Vec<_> = bytes[104..bytes.len() - 32]
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().expect("fixed noise word")))
        .collect();
    if values.iter().any(|v| !v.is_finite()) {
        return Err("Native noise returned non-finite samples".into());
    }
    Ok(values)
}
/// Coordinates are passed exactly as f32 values; no approximated noise function,
/// interpolation, clamping or hidden seed substitution is applied here.
pub fn sample(
    root: &Path,
    worker: &Path,
    source: &Installation,
    profile: &Profile,
    coordinates: &[f32],
    cancelled: impl Fn() -> bool,
) -> Result<Option<Vec<f32>>, String> {
    if cancelled() {
        return Ok(None);
    }
    profile.matches(source)?;
    if !(1..=MAX_SAMPLES).contains(&coordinates.len())
        || coordinates
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 1_000_000.)
    {
        return Err("Invalid or excessive native noise coordinates".into());
    }
    let request = serde_json::to_vec(&serde_json::json!({"format":"papyrus-native-noise1d-query","schema":1,"coordinates":coordinates})).map_err(|e| e.to_string())?;
    if request.len() > MAX_REQUEST {
        return Err("Native noise request exceeds 16 MiB".into());
    }
    let profile_bytes = profile.bytes()?;
    let profile_hash = profile.sha256()?;
    let temporary = tempfile::tempdir().map_err(|e| e.to_string())?;
    let query = temporary.path().join("noise.json");
    let settings = temporary.path().join("profile.json");
    let output = temporary.path().join("noise.bin");
    let errors = temporary.path().join("stderr.txt");
    std::fs::write(&query, &request).map_err(|e| e.to_string())?;
    std::fs::write(&settings, profile_bytes).map_err(|e| e.to_string())?;
    let mut command = Command::new(worker);
    command
        .arg("--noise1d")
        .arg(root.join(&source.native.relative_path))
        .arg(&source.native.sha256)
        .arg(settings)
        .arg(&profile_hash)
        .arg(query)
        .arg(&output)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(
            File::create(&errors).map_err(|e| e.to_string())?,
        ));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = Running(
        command
            .spawn()
            .map_err(|e| format!("Cannot start native noise worker: {e}"))?,
    );
    let started = Instant::now();
    loop {
        if cancelled() {
            return Ok(None);
        }
        if let Some(exit) = child.0.try_wait().map_err(|e| e.to_string())? {
            if !exit.success() {
                let mut message = String::new();
                let _ = File::open(errors).and_then(|f| f.take(4096).read_to_string(&mut message));
                return Err(format!(
                    "Native noise worker failed ({exit}): {}",
                    message.trim()
                ));
            }
            break;
        }
        if started.elapsed() > TIMEOUT {
            return Err("Native noise worker timed out".into());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let mut bytes = Vec::new();
    File::open(output)
        .map_err(|e| e.to_string())?
        .take((137 + coordinates.len() * 4) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let values = decode(
        &bytes,
        coordinates.len(),
        &Sha256::digest(&request).into(),
        &hash_bytes(&source.native.sha256)?,
        &hash_bytes(&profile_hash)?,
    )?;
    Ok((!cancelled()).then_some(values))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_noise_extends_only_missing_coordinates_and_compares_float_bits() {
        let mut cache = PrefixCache::default();
        let first = cache
            .fill(&[-0., 0.1], &|| false, |request| {
                assert_eq!(request.len(), 2);
                Ok(Some(vec![0.3, 0.4]))
            })
            .unwrap();
        assert_eq!(first, Some(vec![0.3, 0.4]));
        let extended = cache
            .fill(&[-0., 0.1, 0.2], &|| false, |request| {
                assert_eq!(request, &[0.2]);
                Ok(Some(vec![0.5]))
            })
            .unwrap();
        assert_eq!(extended, Some(vec![0.3, 0.4, 0.5]));
        let short = cache
            .fill(&[-0.], &|| false, |_| panic!("Prefix should be cached"))
            .unwrap();
        assert_eq!(short, Some(vec![0.3]));
        let cancelled = cache
            .fill(&[-0., 0.1, 0.2, 0.3], &|| true, |_| Ok(Some(vec![0.6])))
            .unwrap();
        assert!(cancelled.is_none());
        assert_eq!(cache.values.len(), 3);
        let different = cache
            .fill(&[0., 0.1], &|| false, |request| {
                assert_eq!(request[0].to_bits(), 0_f32.to_bits());
                assert_eq!(request.len(), 2);
                Ok(Some(vec![0.6, 0.4]))
            })
            .unwrap();
        assert_eq!(different, Some(vec![0.6, 0.4]));
    }

    #[test]
    fn road_grid_extends_exact_prefix_and_covers_small_follow_up_batches() {
        let grid = |count| {
            (0..count)
                .map(|i| i as f32 * 0.5 * 0.005)
                .collect::<Vec<_>>()
        };
        let first = grid(255);
        let mut cache = PrefixCache::default();
        let padded = cache.road_grid(&first, 0.5, 0.005);
        assert_eq!(padded.as_ref(), grid(512));
        cache
            .fill(&padded, &|| false, |batch| Ok(Some(batch.to_vec())))
            .unwrap();
        let next = grid(263);
        let padded_next = cache.road_grid(&next, 0.5, 0.005);
        // A request inside the retained padding should not grow it again.
        assert_eq!(padded_next.len(), next.len());
        assert_eq!(
            cache
                .fill(&padded_next, &|| false, |_| panic!(
                    "road prefix should be cached"
                ))
                .unwrap(),
            Some(next)
        );
        let empty = PrefixCache::default();
        assert!(matches!(
            empty.road_grid(&[-0., 0.0025], 0.5, 0.005),
            Cow::Borrowed(_)
        ));
        assert!(matches!(
            empty.road_grid(&grid(64 * 1024), 0.5, 0.005),
            Cow::Borrowed(_)
        ));
    }
    fn response(values: &[f32]) -> Vec<u8> {
        let mut bytes = b"PNN1".to_vec();
        bytes.extend((values.len() as u32).to_le_bytes());
        bytes.extend([1; 32]);
        bytes.extend([2; 32]);
        bytes.extend([3; 32]);
        for value in values {
            bytes.extend(value.to_le_bytes());
        }
        bytes.extend(Sha256::digest(&bytes));
        bytes
    }
    #[test]
    fn replies_are_bound_to_ordered_request_and_source_and_profile() {
        let original = response(&[-0., 0.125, -0.75]);
        let read = |bytes: &[u8]| decode(bytes, 3, &[1; 32], &[2; 32], &[3; 32]);
        let values = read(&original).unwrap();
        assert_eq!(values[0].to_bits(), (-0_f32).to_bits());
        assert_eq!(&values[1..], &[0.125, -0.75]);
        for i in [0, 4, 8, 40, 72, 104, original.len() - 1] {
            let mut corrupt = original.clone();
            corrupt[i] ^= 1;
            assert!(read(&corrupt).is_err());
        }
        assert!(decode(&original, 2, &[1; 32], &[2; 32], &[3; 32]).is_err());
        assert!(decode(&original, 3, &[4; 32], &[2; 32], &[3; 32]).is_err());
        assert!(decode(&original, 3, &[1; 32], &[4; 32], &[3; 32]).is_err());
        assert!(decode(&original, 3, &[1; 32], &[2; 32], &[4; 32]).is_err());
        assert!(read(&original[..100]).is_err());
        let mut extra = original;
        extra.push(0);
        assert!(read(&extra).is_err());
        assert!(decode(&response(&[f32::NAN]), 1, &[1; 32], &[2; 32], &[3; 32]).is_err());
        assert!(decode(&response(&[f32::INFINITY]), 1, &[1; 32], &[2; 32], &[3; 32]).is_err());
    }
}
