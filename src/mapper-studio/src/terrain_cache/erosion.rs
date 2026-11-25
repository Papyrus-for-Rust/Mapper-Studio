//! Native-endian erosion checkpoints scoped to exact terrain and source inputs.
use super::{Checksummed, stopped};
use crate::{erosion_compute, erosion_source};
use papyrus_terrain::{erosion_pipeline, native_layers::Layers};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

const MAGIC: &[u8; 8] = b"PTEROS01";
const DISK_BUDGET: u64 = 2 * 1024 * 1024 * 1024;
const HEADER_BYTES: usize = 56;

fn key(
    layers: &Layers,
    plan: &erosion_source::Plan,
    gpu: Option<&[u8]>,
    cancel: &impl Fn() -> bool,
) -> Option<[u8; 32]> {
    let mut hash = Sha256::new();
    hash.update(MAGIC);
    hash.update([u8::from(cfg!(target_endian = "little"))]);
    hash.update(erosion_pipeline::VERSION.as_bytes());
    hash.update(plan.profile_sha256.as_bytes());
    hash.update(plan.workflow_sha256.as_bytes());
    // Preserve the existing CPU key while separating every pinned GPU policy.
    if let Some(gpu) = gpu {
        hash.update(b"gpu-erosion");
        hash.update((gpu.len() as u64).to_le_bytes());
        hash.update(gpu);
    }
    for word in [
        layers.size,
        layers.terrain_seed,
        layers.biome_seed,
        layers.height_resolution as u32,
        layers.surface_resolution as u32,
    ] {
        hash.update(word.to_le_bytes());
    }
    for bytes in [
        bytemuck::cast_slice::<i16, u8>(&layers.height),
        bytemuck::cast_slice::<i32, u8>(&layers.topology),
    ] {
        hash.update((bytes.len() as u64).to_le_bytes());
        for chunk in bytes.chunks(65536) {
            if cancel() {
                return None;
            }
            hash.update(chunk);
        }
    }
    Some(hash.finalize().into())
}

fn filename(key: &[u8; 32]) -> String {
    format!("{:x}.pec", sha2::digest::Output::<Sha256>::from(*key))
}

struct Snapshot {
    height: Vec<i16>,
    delta: Vec<f32>,
    work: u64,
    peak: usize,
}

fn checkpoint_bytes(samples: usize) -> u64 {
    (HEADER_BYTES + samples * 6 + 32) as u64
}

/// The exact checkpoint key depends on terrain monument stamps. Check only
/// whether a disk hit is possible so an empty cache can fail early.
pub(crate) fn may_have_checkpoint(samples: usize) -> bool {
    super::directory().is_some_and(|root| may_have_checkpoint_in(&root.join("erosion"), samples))
}

fn may_have_checkpoint_in(root: &Path, samples: usize) -> bool {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) => return error.kind() != std::io::ErrorKind::NotFound,
    };
    let expected = checkpoint_bytes(samples);
    for entry in entries {
        let Ok(entry) = entry else { return true; };
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pec"))
        {
            match fs::metadata(&path) {
                Ok(metadata) if metadata.len() == expected => return true,
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => return true,
                _ => {}
            }
        }
    }
    false
}

fn load(
    path: &Path,
    key: &[u8; 32],
    samples: usize,
    limits: erosion_pipeline::Limits,
    cancel: &impl Fn() -> bool,
) -> Result<Option<Snapshot>, String> {
    stopped(cancel)?;
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let length = checkpoint_bytes(samples);
    if file.metadata().map_err(|e| e.to_string())?.len() != length {
        return Err("Saved erosion length mismatch".into());
    }
    let mut reader = BufReader::new(file);
    let mut header = [0; HEADER_BYTES];
    reader.read_exact(&mut header).map_err(|e| e.to_string())?;
    if &header[..8] != MAGIC || &header[8..40] != key {
        return Err("Saved erosion inputs differ".into());
    }
    let work = u64::from_le_bytes(header[40..48].try_into().unwrap());
    let peak = usize::try_from(u64::from_le_bytes(header[48..56].try_into().unwrap()))
        .map_err(|_| "Saved erosion memory receipt exceeds this platform")?;
    if !(samples as u64 * 2..=limits.maximum_work).contains(&work)
        || !(samples * 6..=limits.maximum_bytes).contains(&peak)
    {
        return Err("Saved erosion exceeds its work or memory allowance".into());
    }
    let mut height = vec![0i16; samples];
    let mut delta = vec![0f32; samples];
    let mut hash = Sha256::new();
    hash.update(header);
    for bytes in [
        bytemuck::cast_slice_mut::<i16, u8>(&mut height),
        bytemuck::cast_slice_mut::<f32, u8>(&mut delta),
    ] {
        for chunk in bytes.chunks_mut(65536) {
            stopped(cancel)?;
            reader.read_exact(chunk).map_err(|e| e.to_string())?;
            hash.update(chunk);
        }
    }
    let mut checksum = [0; 32];
    reader
        .read_exact(&mut checksum)
        .map_err(|e| e.to_string())?;
    if hash.finalize().as_slice() != checksum {
        return Err("Saved erosion checksum mismatch".into());
    }
    for chunk in height.chunks(32768) {
        stopped(cancel)?;
        if chunk.iter().any(|h| !(0..=32766).contains(h)) {
            return Err("Invalid saved erosion heights".into());
        }
    }
    Ok(Some(Snapshot {
        height,
        delta,
        work,
        peak,
    }))
}

struct Checkpoint<'a> {
    height: &'a [i16],
    delta: &'a [f32],
    work: u64,
    peak: usize,
}

fn mark_recent(path: &Path) {
    if let Ok(file) = OpenOptions::new().write(true).open(path) {
        let _ = file.set_modified(SystemTime::now());
    }
}

fn make_room(
    root: &Path,
    destination: &Path,
    required: u64,
    budget: u64,
    cancel: &impl Fn() -> bool,
) -> Result<(), String> {
    let mut used = required;
    let mut entries = Vec::new();
    for (index, entry) in fs::read_dir(root).map_err(|e| e.to_string())?.enumerate() {
        stopped(cancel)?;
        if index == 4096 {
            return Err("Saved erosion entry budget exceeded".into());
        }
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path == destination
            || path.extension().is_none_or(|ext| ext != "pec")
            || !path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| {
                    stem.len() == 64
                        && stem
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
            || !entry.file_type().map_err(|e| e.to_string())?.is_file()
        {
            continue;
        }
        let metadata = entry.metadata().map_err(|e| e.to_string())?;
        used = used.saturating_add(metadata.len());
        entries.push((
            metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            path,
            metadata.len(),
        ));
    }
    if used <= budget {
        return Ok(());
    }
    entries.sort_unstable();
    let mut victims = Vec::new();
    for (_, path, len) in entries {
        stopped(cancel)?;
        let mut header = [0u8; 40];
        if File::open(&path)
            .and_then(|mut f| f.read_exact(&mut header))
            .is_err()
            || &header[..8] != MAGIC
            || path.file_name().and_then(|n| n.to_str())
                != Some(filename(header[8..].try_into().unwrap()).as_str())
        {
            continue;
        }
        victims.push(path);
        used -= len;
        if used <= budget {
            break;
        }
    }
    if used > budget {
        return Err("Saved erosion disk budget exceeded".into());
    }
    stopped(cancel)?;
    for path in victims {
        fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn save(
    path: &Path,
    key: &[u8; 32],
    snapshot: Checkpoint<'_>,
    budget: u64,
    cancel: &impl Fn() -> bool,
) -> Result<(), String> {
    let root = path.parent().ok_or("Missing erosion cache location")?;
    let required = (HEADER_BYTES + snapshot.height.len() * 6 + 32) as u64;
    if required > budget {
        return Err("Saved erosion disk budget exceeded".into());
    }
    stopped(cancel)?;
    fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let mut temp = tempfile::NamedTempFile::new_in(root).map_err(|e| e.to_string())?;
    {
        let mut writer = Checksummed {
            inner: BufWriter::new(temp.as_file_mut()),
            hash: Sha256::new(),
        };
        writer.write_all(MAGIC).map_err(|e| e.to_string())?;
        writer.write_all(key).map_err(|e| e.to_string())?;
        writer
            .write_all(&snapshot.work.to_le_bytes())
            .map_err(|e| e.to_string())?;
        writer
            .write_all(&(snapshot.peak as u64).to_le_bytes())
            .map_err(|e| e.to_string())?;
        for bytes in [
            bytemuck::cast_slice::<i16, u8>(snapshot.height),
            bytemuck::cast_slice::<f32, u8>(snapshot.delta),
        ] {
            for chunk in bytes.chunks(65536) {
                stopped(cancel)?;
                writer.write_all(chunk).map_err(|e| e.to_string())?;
            }
        }
        writer
            .inner
            .write_all(&writer.hash.finalize())
            .map_err(|e| e.to_string())?;
        writer.inner.flush().map_err(|e| e.to_string())?;
    }
    // Checkpoints can be rebuilt after a system crash. Finish the buffered write
    // before atomic publication and let the operating system flush it to disk.
    stopped(cancel)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join(".write.lock"))
        .map_err(|e| e.to_string())?;
    lock.try_lock().map_err(|e| e.to_string())?;
    make_room(root, path, required, budget, cancel)?;
    temp.persist(path).map_err(|e| e.error.to_string())?;
    Ok(())
}

pub(crate) struct Eroded {
    pub height: Vec<i16>,
    pub painting: erosion_pipeline::Painting,
    pub checkpoint: Option<PendingCheckpoint>,
}

pub(crate) struct PendingCheckpoint {
    path: PathBuf,
    key: [u8; 32],
    work: u64,
    peak: usize,
}

impl PendingCheckpoint {
    pub fn write(
        self,
        height: &[i16],
        painting: &erosion_pipeline::Painting,
        cancel: &impl Fn() -> bool,
    ) {
        let mut timing = crate::composition::StageTimer::start("erosion_checkpoint");
        let snapshot = Checkpoint {
            height,
            delta: painting.height_delta(),
            work: self.work,
            peak: self.peak,
        };
        if let Err(error) = save(&self.path, &self.key, snapshot, DISK_BUDGET, cancel)
            && std::env::var_os("MAPPER_COMPOSITION_TIMINGS").is_some()
        {
            eprintln!("Saved erosion cache write unavailable: {error}");
        }
        crate::composition::record_timing(&mut timing, "cache_write_finished");
    }
}

pub(crate) fn prepare(
    source: &erosion_source::Source,
    layers: &Layers,
    limits: erosion_pipeline::Limits,
    gpu: Option<&erosion_compute::Identity>,
    cancel: &(impl Fn() -> bool + Sync),
) -> Result<Option<Eroded>, String> {
    let progress = crate::loading_progress::begin("Checking saved erosion", None);
    let reporter = progress.reporter();
    let timings = std::env::var_os("MAPPER_COMPOSITION_TIMINGS").is_some();
    let checkpoint_read = timings.then(std::time::Instant::now);
    let mut destination = None;
    if let Some(root) = super::directory() {
        let identity = gpu.map(serde_json::to_vec).transpose().map_err(|e| e.to_string())?;
        let Some(key) = key(layers, source.plan(), identity.as_deref(), cancel) else {
            return Ok(None);
        };
        let path = root.join("erosion").join(filename(&key));
        let cached = load(&path, &key, layers.height.len(), limits, cancel).and_then(|saved| {
            let Some(saved) = saved else {
                return Ok(None);
            };
            let policy = source
                .policies(layers.size, layers.height_resolution, 0.)?
                .paint;
            let painting = erosion_pipeline::Painting::restore(
                layers,
                saved.delta,
                policy,
                limits,
                saved.work,
                saved.peak,
                cancel,
            )?;
            Ok(painting.map(|painting| Eroded {
                height: saved.height,
                painting,
                checkpoint: None,
            }))
        });
        if cancel() {
            return Ok(None);
        }
        match cached {
            Ok(Some(eroded)) => {
                mark_recent(&path);
                if timings {
                    eprintln!("Saved erosion cache hit");
                }
                return Ok(Some(eroded));
            }
            Ok(None) => {
                if timings {
                    eprintln!("Saved erosion cache miss");
                }
            }
            Err(error) => {
                if timings {
                    eprintln!("Saved erosion rejected: {error}");
                }
            }
        }
        destination = Some((path, key));
    }
    reporter.stage(if gpu.is_some() { "Simulating erosion on GPU" } else { "Simulating erosion" });
    let Some(eroded) = crate::generation_resources::with_cpu_pool(cancel, |pool, resources| {
        let started = timings.then(std::time::Instant::now);
        let progress = |done: usize, total: usize| {
            reporter.update("", done as u64, Some(total as u64), crate::loading_progress::Unit::Items);
        };
        let result = if let Some(gpu) = gpu {
            erosion_compute::prepare_adapter(
                gpu,
                layers.height_resolution,
                resources.gpu_memory_mib as u64 * 1024 * 1024,
            ).and_then(|adapter| {
                erosion_pipeline::erode_with(
                    layers, source.policies(layers.size, layers.height_resolution, 0.)?,
                    limits, cancel, |input, policy, limits| {
                        erosion_compute::run_prepared(adapter, input, policy, limits, cancel, progress)
                            .map(|result| result.map(|run| run.output))
                    },
                )
            })
        } else {
            source.erode(layers, limits, pool, cancel, progress)
        };
        if let Some(started) = started {
            eprintln!("MAPPER_EROSION_TIMING {}", serde_json::json!({
                "simulation_ms": started.elapsed().as_secs_f64() * 1000.,
                "before_simulation_ms": checkpoint_read.map(|read| started.duration_since(read).as_secs_f64() * 1000.),
                "workers": gpu.is_none().then(|| pool.current_num_threads()),
                "gpu": gpu,
                "resolution": layers.height_resolution,
                "complete": matches!(&result, Ok(Some(_))),
            }));
        }
        result
    })?
    else {
        return Ok(None);
    };
    Ok((!cancel()).then_some(Eroded {
        height: eroded.height,
        painting: eroded.painting,
        checkpoint: destination.map(|(path, key)| PendingCheckpoint {
            path,
            key,
            work: eroded.report.work,
            peak: eroded.report.peak_buffer_bytes,
        }),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom};
    use std::time::Duration;

    fn checkpoint() -> Checkpoint<'static> {
        Checkpoint {
            height: &[120, 32766, 0],
            delta: &[0.25, -0.0, -0.375],
            work: 16,
            peak: 64,
        }
    }

    #[test]
    fn early_buffer_check_does_not_rule_out_a_potential_disk_hit() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("erosion");
        let samples = checkpoint().height.len();
        assert!(!may_have_checkpoint_in(&directory, samples));
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("partial.tmp"), vec![0; checkpoint_bytes(samples) as usize]).unwrap();
        fs::write(directory.join("short.pec"), b"truncated").unwrap();
        assert!(!may_have_checkpoint_in(&directory, samples));
        let key = [7; 32];
        let path = directory.join(filename(&key));
        save(&path, &key, checkpoint(), checkpoint_bytes(samples), &|| false).unwrap();
        assert!(may_have_checkpoint_in(&directory, samples));
        assert!(!may_have_checkpoint_in(&directory, samples + 1));
    }

    #[test]
    fn cache_replaces_the_least_recently_used_checkpoint_and_keeps_other_files() {
        let root = tempfile::tempdir().unwrap();
        let [first, second, next] = [1, 2, 3].map(|value| root.path().join(filename(&[value; 32])));
        let bytes = (HEADER_BYTES + checkpoint().height.len() * 6 + 32) as u64;
        let budget = bytes * 2;
        for (index, path) in [&first, &second].into_iter().enumerate() {
            save(
                path,
                &[(index + 1) as u8; 32],
                checkpoint(),
                budget,
                &|| false,
            )
            .unwrap();
            OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(index as u64))
                .unwrap();
        }
        let other = root.path().join("keep.pec");
        fs::write(&other, b"unrelated").unwrap();
        mark_recent(&first);
        assert!(save(&next, &[3; 32], checkpoint(), budget, &|| true).is_err());
        assert!(first.exists() && second.exists());
        save(&next, &[3; 32], checkpoint(), budget, &|| false).unwrap();
        assert!(first.exists() && !second.exists() && next.exists());
        assert_eq!(fs::read(other).unwrap(), b"unrelated");
    }

    #[test]
    fn simultaneous_publication_leaves_the_existing_checkpoint_intact() {
        let root = tempfile::tempdir().unwrap();
        let key = [7; 32];
        let path = root.path().join(filename(&key));
        let next = root.path().join(filename(&[8; 32]));
        let budget = (HEADER_BYTES + checkpoint().height.len() * 6 + 32) as u64;
        save(&path, &key, checkpoint(), budget, &|| false).unwrap();
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.path().join(".write.lock"))
            .unwrap();
        lock.try_lock().unwrap();
        assert!(save(&next, &[8; 32], checkpoint(), budget, &|| false).is_err());
        assert!(path.exists() && !next.exists());
        drop(lock);
        save(&next, &[8; 32], checkpoint(), budget, &|| false).unwrap();
        assert!(!path.exists() && next.exists());
    }

    #[test]
    fn cancellation_during_write_removes_the_unpublished_checkpoint() {
        let root = tempfile::tempdir().unwrap();
        let key = [7; 32];
        let path = root.path().join(filename(&key));
        let result = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let cancelled = std::cell::Cell::new(0);
                    save(&path, &key, checkpoint(), DISK_BUDGET, &|| {
                        cancelled.set(cancelled.get() + 1);
                        cancelled.get() > 2
                    })
                })
                .join()
                .unwrap()
        });
        assert_eq!(result.unwrap_err(), "Terrain cache cancelled");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn checkpoint_round_trips_and_rejects_wrong_key_corruption_and_limits() {
        let root = tempfile::tempdir().unwrap();
        let key = [7; 32];
        let path = root.path().join(filename(&key));
        let limits = erosion_pipeline::Limits {
            maximum_bytes: 128,
            maximum_work: 32,
        };
        assert!(load(&path, &key, 3, limits, &|| false).unwrap().is_none());
        assert!(save(&path, &key, checkpoint(), DISK_BUDGET, &|| true).is_err());
        assert!(!path.exists());
        save(&path, &key, checkpoint(), DISK_BUDGET, &|| false).unwrap();
        let saved = load(&path, &key, 3, limits, &|| false).unwrap().unwrap();
        assert_eq!(saved.height, [120, 32766, 0]);
        assert_eq!(
            saved.delta.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
            [0.25f32, -0.0, -0.375].map(f32::to_bits)
        );
        assert_eq!((saved.work, saved.peak), (16, 64));
        assert!(load(&path, &[8; 32], 3, limits, &|| false).is_err());
        assert!(
            load(
                &path,
                &key,
                3,
                erosion_pipeline::Limits {
                    maximum_work: 15,
                    ..limits
                },
                &|| false
            )
            .is_err()
        );
        let mut file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(SeekFrom::Start(HEADER_BYTES as u64)).unwrap();
        file.write_all(&[0xff]).unwrap();
        drop(file);
        assert!(load(&path, &key, 3, limits, &|| false).is_err());
    }
}
