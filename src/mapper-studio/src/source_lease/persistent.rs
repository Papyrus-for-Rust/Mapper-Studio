//! Bounded checksum receipts. Every hit requires the current file and journal
//! identity under a read-only lease. Unknown or damaged caches are cache misses.
use crate::installation::SourceFile;
use papyrus_file_id::Revision;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

const LIMIT: usize = 64 * 1024;
const MINIMUM: u64 = 64 * 1024;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    revision: Revision,
    source: SourceFile,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    format: String,
    schema: u32,
    sha256: String,
    records: Vec<Record>,
}
#[derive(Default)]
struct Index {
    records: Vec<Record>,
    pending: Vec<Record>,
    batches: usize,
}
static INDEX: OnceLock<Mutex<Index>> = OnceLock::new();

fn cache_path() -> Option<PathBuf> {
    if cfg!(test) || std::env::var_os("MAPPER_DISABLE_PERSISTENT_VERIFICATION").is_some() {
        return None;
    }
    Some(
        crate::storage::local_data()
            .ok()?
            .join("verified-source-index.json"),
    )
}
fn valid(record: &Record) -> bool {
    record.revision.bytes >= MINIMUM
        && record.revision.bytes <= 16 * 1024 * 1024 * 1024
        && record.revision.bytes == record.source.bytes
        && record.revision.can_reuse(&record.revision)
        && record.source.relative_path.len() <= 1024
        && record.source.sha256.len() == 64
        && record
            .source
            .sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn read(path: &Path) -> Option<Vec<Record>> {
    let mut raw = Vec::new();
    File::open(path)
        .ok()?
        .take((LIMIT + 1) as u64)
        .read_to_end(&mut raw)
        .ok()?;
    if raw.len() > LIMIT {
        return None;
    }
    let document: Envelope = serde_json::from_slice(&raw).ok()?;
    if document.format != "mapper-verified-source-index"
        || document.schema != 1
        || document.records.len() > 64
        || !document.records.iter().all(valid)
    {
        return None;
    }
    let records = serde_json::to_vec(&document.records).ok()?;
    (format!("{:x}", Sha256::digest(&records)) == document.sha256).then_some(document.records)
}
fn write(path: &Path, records: &[Record]) -> Result<(), String> {
    if records.len() > 64 || !records.iter().all(valid) {
        return Err("Invalid source verification records".into());
    }
    let payload = serde_json::to_vec(records).map_err(|e| e.to_string())?;
    let document = Envelope {
        format: "mapper-verified-source-index".into(),
        schema: 1,
        sha256: format!("{:x}", Sha256::digest(&payload)),
        records: records.to_vec(),
    };
    let raw = serde_json::to_vec(&document).map_err(|e| e.to_string())?;
    if raw.len() > LIMIT {
        return Err("Source verification index exceeds its budget".into());
    }
    let parent = path.parent().ok_or("Missing source cache directory")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    temporary.write_all(&raw).map_err(|e| e.to_string())?;
    temporary.as_file().sync_all().map_err(|e| e.to_string())?;
    temporary.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}
pub(super) fn lookup(revision: Revision, limit: u64) -> Option<SourceFile> {
    if revision.bytes < MINIMUM || revision.bytes > limit {
        return None;
    }
    let path = cache_path()?;
    let index = index(&path).lock().ok()?;
    index
        .pending
        .iter()
        .chain(&index.records)
        .find(|record| record.revision.can_reuse(&revision))
        .map(|record| record.source.clone())
}
fn index(path: &Path) -> &'static Mutex<Index> {
    INDEX.get_or_init(|| {
        Mutex::new(Index {
            records: read(path).unwrap_or_default(),
            ..Default::default()
        })
    })
}

/// Delay disk publication until all overlapping verification batches finish.
pub(crate) struct Batch(Option<PathBuf>);

pub(crate) fn batch() -> Batch {
    let path = cache_path().filter(|path| {
        let Ok(mut index) = index(path).lock() else {
            return false;
        };
        index.batches += 1;
        true
    });
    Batch(path)
}

impl Drop for Batch {
    fn drop(&mut self) {
        if let Some(path) = &self.0
            && let Ok(mut index) = index(path).lock()
        {
            index.batches -= 1;
            index.flush(path);
        }
    }
}

impl Index {
    fn remember(&mut self, record: Record) {
        self.records
            .retain(|old| old.revision.identity != record.revision.identity);
        self.pending
            .retain(|old| old.revision.identity != record.revision.identity);
        if self.pending.len() == 64 {
            self.pending.remove(0);
        }
        self.pending.push(record);
    }

    fn flush(&mut self, path: &Path) {
        if self.batches != 0 || self.pending.is_empty() {
            return;
        }
        // Another instance may have prepared a different installation.
        // Prefer its complete receipts to older in-memory receipts.
        let mut merged = read(path).unwrap_or_default();
        for old in &self.records {
            if !merged
                .iter()
                .any(|r| r.revision.identity == old.revision.identity)
            {
                merged.push(old.clone());
            }
        }
        merged.retain(|old| {
            !self
                .pending
                .iter()
                .any(|r| r.revision.identity == old.revision.identity)
        });
        merged.append(&mut self.pending);
        if merged.len() > 64 {
            merged.drain(..merged.len() - 64);
        }
        // Publication failure only loses the disk cache optimization.
        let _ = write(path, &merged);
        self.records = merged;
    }
}

pub(super) fn remember(revision: Revision, source: &SourceFile) {
    if revision.bytes < MINIMUM {
        return;
    }
    let Some(path) = cache_path() else {
        return;
    };
    let record = Record {
        revision,
        source: source.clone(),
    };
    if !valid(&record) {
        return;
    }
    let Ok(mut index) = index(&path).lock() else {
        return;
    };
    index.remember(record);
    index.flush(&path);
}
pub(super) fn clear() {
    let path = cache_path();
    let mut index = INDEX
        .get()
        .or_else(|| path.as_deref().map(index))
        .and_then(|index| index.lock().ok());
    if let Some(index) = index.as_mut() {
        index.records.clear();
        index.pending.clear();
    }
    if let Some(path) = path {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_batches_publish_verified_records_together_and_merge_disk_entries() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("index.json");
        let record = |name: &str, byte: u8| Record {
            revision: serde_json::from_value(serde_json::json!({
                "identity": {"volume": 1, "file": (vec![byte; 16])},
                "bytes": MINIMUM,
                "usn": 100,
                "change_time": 1,
                "write_time": 1,
                "journal_id": 1,
                "observed_next": 101,
                "lowest_valid": 1,
            }))
            .unwrap(),
            source: SourceFile {
                relative_path: name.into(),
                bytes: MINIMUM,
                sha256: format!("{:x}", Sha256::digest([byte])),
            },
        };
        let (first, second, external) = (
            record("first", 1),
            record("second", 2),
            record("external", 3),
        );
        let mut index = Index {
            batches: 2,
            ..Default::default()
        };
        index.remember(first.clone());
        index.flush(&path);
        assert!(!path.exists());
        write(&path, std::slice::from_ref(&external)).unwrap();
        index.batches -= 1;
        index.remember(second.clone());
        index.remember(first.clone());
        index.flush(&path);
        assert_eq!(read(&path).unwrap()[0].source, external.source);
        assert_eq!(index.pending.len(), 2);
        index.batches -= 1;
        index.flush(&path);
        assert!(index.pending.is_empty());
        assert_eq!(
            read(&path)
                .unwrap()
                .into_iter()
                .map(|r| r.source)
                .collect::<Vec<_>>(),
            [external.source, second.source, first.source]
        );
    }

    #[test]
    fn disk_receipts_are_bounded_checked_and_atomically_replaceable() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source");
        let data = vec![17u8; MINIMUM as usize];
        std::fs::write(&path, &data).unwrap();
        let file = File::open(&path).unwrap();
        let Ok(revision) = papyrus_file_id::revision(&file) else {
            return;
        };
        let record = Record {
            revision,
            source: SourceFile {
                relative_path: "Bundles/source.bundle".into(),
                bytes: MINIMUM,
                sha256: format!("{:x}", Sha256::digest(&data)),
            },
        };
        let index = root.path().join("index.json");
        write(&index, std::slice::from_ref(&record)).unwrap();
        let loaded = read(&index).unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(
            loaded[0]
                .revision
                .can_reuse(&papyrus_file_id::revision(&file).unwrap())
        );
        assert_eq!(loaded[0].source, record.source);
        write(&index, &[]).unwrap();
        assert!(read(&index).unwrap().is_empty());
        assert!(write(&index, &vec![record.clone(); 65]).is_err());
        write(&index, &[record]).unwrap();
        let mut document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&index).unwrap()).unwrap();
        document["records"][0]["source"]["sha256"] = "0".repeat(64).into();
        std::fs::write(&index, serde_json::to_vec(&document).unwrap()).unwrap();
        assert!(read(&index).is_none());
        std::fs::write(&index, vec![0; LIMIT + 1]).unwrap();
        assert!(read(&index).is_none());
    }
}
