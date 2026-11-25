//! Read-only installation identity. Detection never loads or executes game binaries.
/// Own this on a generation/preparation worker to reuse hashes of unchanged,
/// read-leased source files. Read handles close with the operation. Bounded
/// checksum receipts can be reused after validating current file revisions.
pub use crate::source_lease::Scope as VerificationScope;
pub use crate::source_lease::clear_session_cache as clear_verification_cache;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceFile {
    pub relative_path: String,
    pub sha256: String,
    pub bytes: u64,
}

/// Shared bounded receipt syntax for captured placement metadata. Each loader
/// chooses and verifies the source files its own data actually depends on.
pub(crate) fn validate_source_receipts(
    required: &[SourceFile],
    additional: &[SourceFile],
) -> Result<(), String> {
    if required.len() > 64 || additional.len() > 64 {
        return Err("Excessive placement metadata source receipts".into());
    }
    let mut receipts = std::collections::BTreeMap::new();
    for receipt in required.iter().chain(additional) {
        if receipt.sha256.len() != 64
            || !receipt
                .sha256
                .bytes()
                .all(|value| value.is_ascii_digit() || (b'a'..=b'f').contains(&value))
            || receipt.bytes == 0
            || receipt.bytes > 8 * 1024 * 1024 * 1024
            || receipt.relative_path.contains(['\\', '\0', ':'])
            || receipt
                .relative_path
                .split('/')
                .any(|part| matches!(part, "" | "." | ".."))
        {
            return Err("Invalid placement metadata source receipt".into());
        }
        if let Some(previous) = receipts.insert(&receipt.relative_path, receipt)
            && previous != receipt
        {
            return Err("Conflicting placement metadata source receipts".into());
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Installation {
    pub kind: InstallationKind,
    pub native: SourceFile,
    pub scene_manifest: Option<SourceFile>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstallationKind {
    Client,
    DedicatedServer,
}

impl Installation {
    pub fn validate(&self) -> Result<(), String> {
        let native_path = match self.kind {
            InstallationKind::Client => "RustClient_Data/Plugins/x86_64/RustNative.dll",
            InstallationKind::DedicatedServer => "RustDedicated_Data/Plugins/x86_64/RustNative.dll",
        };
        fn valid(file: &SourceFile, path: &str, limit: u64) -> bool {
            file.relative_path == path
                && file.bytes > 0
                && file.bytes <= limit
                && file.sha256.len() == 64
                && file
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }
        if !valid(&self.native, native_path, 64 * 1024 * 1024)
            || self
                .scene_manifest
                .as_ref()
                .is_some_and(|f| !valid(f, "Bundles/AssetSceneManifest.json", 32 * 1024 * 1024))
        {
            return Err("Invalid Rust installation identity".into());
        }
        Ok(())
    }
}

pub(crate) fn hash_file(
    root: &Path,
    relative: &str,
    limit: u64,
    cancelled: &impl Fn() -> bool,
) -> Result<SourceFile, String> {
    hash_file_with_read_ahead(root, relative, limit, false, cancelled)
}

pub(crate) fn hash_file_with_read_ahead(
    root: &Path,
    relative: &str,
    limit: u64,
    read_ahead: bool,
    cancelled: &impl Fn() -> bool,
) -> Result<SourceFile, String> {
    if cancelled() {
        return Err("Installation inspection cancelled".into());
    }
    let _verification = VerificationScope::default();
    let path = root.join(relative);
    let file =
        crate::source_lease::open(&path).map_err(|e| format!("Cannot read {relative}: {e}"))?;
    match reuse_open_hash(&path, relative, file, limit) {
        Ok(receipt) => Ok(receipt),
        Err(file) => {
            hash_open_file_with_read_ahead(path, relative, file, limit, read_ahead, cancelled)
        }
    }
}

pub(crate) fn reuse_open_hash(
    path: &Path,
    relative: &str,
    file: File,
    limit: u64,
) -> Result<SourceFile, File> {
    let mut receipt = crate::source_lease::reuse(path, file, limit)?;
    if receipt.relative_path != relative {
        receipt.relative_path = relative.into();
    }
    Ok(receipt)
}

pub(crate) fn hash_open_file(
    path: PathBuf,
    relative: &str,
    file: File,
    limit: u64,
    cancelled: &impl Fn() -> bool,
) -> Result<SourceFile, String> {
    hash_open_file_with_read_ahead(path, relative, file, limit, false, cancelled)
}

fn hash_open_file_with_read_ahead(
    path: PathBuf,
    relative: &str,
    mut file: File,
    limit: u64,
    read_ahead: bool,
    cancelled: &impl Fn() -> bool,
) -> Result<SourceFile, String> {
    if cancelled() {
        return Err("Installation inspection cancelled".into());
    }
    let progress = crate::loading_progress::begin("Verifying Rust files", None);
    let reporter = progress.reporter();
    let before = file.metadata().map_err(|e| e.to_string())?;
    reporter.update(
        relative,
        0,
        Some(before.len()),
        crate::loading_progress::Unit::Bytes,
    );
    if before.len() > limit {
        return Err(format!("{relative} exceeds its inspection budget"));
    }
    if before.len() == 0 {
        return Err(format!("{relative} is empty"));
    }
    const READ_SIZE: u64 = 1024 * 1024;
    const PROGRESS_INTERVAL: u64 = 4 * READ_SIZE;
    let mut bytes = 0_u64;
    let mut hash = Sha256::new();
    let mut consume = |chunk: &[u8]| -> Result<(), String> {
        if cancelled() {
            return Err("Installation inspection cancelled".into());
        }
        bytes += chunk.len() as u64;
        if bytes > limit {
            return Err(format!("{relative} exceeds its inspection budget"));
        }
        hash.update(chunk);
        if bytes / PROGRESS_INTERVAL != (bytes - chunk.len() as u64) / PROGRESS_INTERVAL {
            reporter.update(
                relative,
                bytes,
                Some(before.len()),
                crate::loading_progress::Unit::Bytes,
            );
        }
        Ok(())
    };
    let buffer_size = before.len().min(READ_SIZE) as usize;
    if read_ahead && before.len() >= 256 * READ_SIZE {
        read_ahead_chunks(&mut file, buffer_size, &mut consume)?;
    } else {
        read_chunks(&mut file, buffer_size, &mut consume)?;
    }
    if cancelled() {
        return Err("Installation inspection cancelled".into());
    }
    let after = std::fs::metadata(&path).map_err(|e| e.to_string())?;
    if bytes != before.len()
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
        || before.created().ok() != after.created().ok()
    {
        return Err(format!(
            "Source changed while verifying: {relative}; retry after Rust finishes updating"
        ));
    }
    reporter.update(
        relative,
        bytes,
        Some(bytes),
        crate::loading_progress::Unit::Bytes,
    );
    let receipt = SourceFile {
        relative_path: relative.into(),
        sha256: format!("{:x}", hash.finalize()),
        bytes,
    };
    crate::source_lease::retain(path, file, receipt.clone());
    Ok(receipt)
}

fn read_chunks(
    mut reader: impl Read,
    buffer_size: usize,
    mut consume: impl FnMut(&[u8]) -> Result<(), String>,
) -> Result<(), String> {
    let mut buffer = vec![0; buffer_size];
    loop {
        let n = reader.read(&mut buffer).map_err(|error| error.to_string())?;
        if n == 0 {
            return Ok(());
        }
        consume(&buffer[..n])?;
    }
}

fn read_ahead_chunks(
    mut reader: impl Read + Send,
    buffer_size: usize,
    mut consume: impl FnMut(&[u8]) -> Result<(), String>,
) -> Result<(), String> {
    // Keep one read in flight with one SHA-256 update. The reader owns
    // no shared state. The caller still owns cancellation and progress.
    let result = std::thread::scope(|scope| {
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<(Vec<u8>, usize)>(1);
        let (free_tx, free_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(2);
        // Borrow the reader so thread creation failure can use the serial path.
        let input = &mut reader;
        let worker = std::thread::Builder::new()
            .name("mapper-source-read".into())
            .spawn_scoped(scope, move || {
                while let Ok(mut buffer) = free_rx.recv() {
                    let n = input.read(&mut buffer).map_err(|error| error.to_string())?;
                    if n == 0 || ready_tx.send((buffer, n)).is_err() {
                        break;
                    }
                }
                Ok::<(), String>(())
            });
        let Ok(worker) = worker else {
            return None;
        };
        let _ = free_tx.send(vec![0; buffer_size]);
        let _ = free_tx.send(vec![0; buffer_size]);
        let consumed = (|| {
            while let Ok((buffer, n)) = ready_rx.recv() {
                consume(&buffer[..n])?;
                let _ = free_tx.send(buffer);
            }
            Ok::<(), String>(())
        })();
        drop(ready_rx);
        drop(free_tx);
        let read = worker
            .join()
            .map_err(|_| "Source read worker stopped".to_owned())
            .and_then(|result| result);
        Some(consumed.and(read))
    });
    result.unwrap_or_else(|| read_chunks(reader, buffer_size, consume))
}

pub fn inspect(root: &Path, cancelled: impl Fn() -> bool) -> Result<Installation, String> {
    if cancelled() {
        return Err("Installation inspection cancelled".into());
    }
    let candidates = [
        (
            InstallationKind::Client,
            "RustClient_Data/Plugins/x86_64/RustNative.dll",
        ),
        (
            InstallationKind::DedicatedServer,
            "RustDedicated_Data/Plugins/x86_64/RustNative.dll",
        ),
    ];
    let mut found = candidates
        .into_iter()
        .filter(|(_, path)| root.join(path).is_file());
    let selection_error = || {
        "Choose a Rust root with exactly one client or dedicated-server RustNative.dll installation."
            .to_owned()
    };
    let (kind, relative) = found.next().ok_or_else(selection_error)?;
    if found.next().is_some() {
        return Err(selection_error());
    }
    let _verification = VerificationScope::default();
    let path = root.join(relative);
    let mut file = crate::source_lease::open(&path)
        .map_err(|e| format!("Cannot read {relative}: {e}"))?;
    check_windows_x64(&mut file)?;
    let limit = 64 * 1024 * 1024;
    let native = match reuse_open_hash(&path, relative, file, limit) {
        Ok(receipt) => receipt,
        Err(mut file) => {
            file.rewind().map_err(|e| e.to_string())?;
            hash_open_file(path, relative, file, limit, &cancelled)?
        }
    };
    let scene_manifest = if root
        .join("Bundles/AssetSceneManifest.json")
        .try_exists()
        .map_err(|e| e.to_string())?
    {
        Some(hash_file(
            root,
            "Bundles/AssetSceneManifest.json",
            32 * 1024 * 1024,
            &cancelled,
        )?)
    } else {
        None
    };
    let item = Installation {
        kind,
        native,
        scene_manifest,
    };
    item.validate()?;
    Ok(item)
}

// The PE header identifies the architecture. The fingerprint and native
// profile establish which generation adapter can use it.
fn check_windows_x64(mut file: impl Read + Seek) -> Result<(), String> {
    const HEADER_BUDGET: u64 = 1024 * 1024;
    let mut dos = [0; 64];
    file.read_exact(&mut dos).map_err(|error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            "Truncated native library header".into()
        } else {
            error.to_string()
        }
    })?;
    if &dos[..2] != b"MZ" {
        return Err("Native library is not a Windows PE binary".into());
    }
    let offset = u64::from(u32::from_le_bytes(dos[60..64].try_into().unwrap()));
    let mut pe = [0; 6];
    if offset + pe.len() as u64 > HEADER_BUDGET {
        return Err("Native PE header is outside the inspection budget".into());
    }
    file.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
    file.read_exact(&mut pe).map_err(|error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            "Native PE header is outside the inspection budget".into()
        } else {
            error.to_string()
        }
    })?;
    if &pe[..4] != b"PE\0\0" || pe[4..] != [0x64, 0x86] {
        return Err("Only Windows x64 Rust installations are currently recognized".into());
    }
    Ok(())
}

type FileHint = Option<(
    u64,
    Option<std::time::SystemTime>,
    Option<std::time::SystemTime>,
)>;
/// Cheap update notification only. Cache acceptance still requires full SHA-256 verification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeWatch(Vec<FileHint>);
impl ChangeWatch {
    pub fn capture(root: &Path) -> Self {
        let mut paths = vec![
            "Bundles/AssetSceneManifest.json".to_owned(),
            "GameAssembly.dll".to_owned(),
        ];
        for bundle in [
            "assetscenes",
            "content",
            "monuments",
            "textures.0",
            "textures.1",
            "textures.2",
            "textures.3",
            "textures.4",
        ] {
            paths.push(format!("Bundles/shared/{bundle}.bundle"));
        }
        paths.push("Bundles/maps/maps.bundle".into());
        for data in ["RustClient_Data", "RustDedicated_Data"] {
            for file in [
                "Plugins/x86_64/RustNative.dll",
                "Managed/Assembly-CSharp.dll",
                "il2cpp_data/Metadata/global-metadata.dat",
                "globalgamemanagers",
            ] {
                paths.push(format!("{data}/{file}"));
            }
        }
        Self(
            paths
                .iter()
                .map(|path| {
                    std::fs::metadata(root.join(path))
                        .ok()
                        .map(|m| (m.len(), m.modified().ok(), m.created().ok()))
                })
                .collect(),
        )
    }
    pub fn changed(&self, root: &Path) -> bool {
        *self != Self::capture(root)
    }
}

/// A folder binding is session-local; portable project identity contains no absolute path.
pub struct Binding {
    pub root: PathBuf,
    pub identity: Installation,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn update_watch_detects_bundle_only_changes_additions_and_removal() {
        let root = tempfile::tempdir().unwrap();
        let original = ChangeWatch::capture(root.path());
        assert!(!original.changed(root.path()));
        std::fs::create_dir_all(root.path().join("Bundles/shared")).unwrap();
        let path = root.path().join("Bundles/shared/monuments.bundle");
        std::fs::write(&path, b"old").unwrap();
        assert!(original.changed(root.path()));
        let installed = ChangeWatch::capture(root.path());
        std::fs::write(&path, b"updated").unwrap();
        assert!(installed.changed(root.path()));
        let updated = ChangeWatch::capture(root.path());
        std::fs::remove_file(path).unwrap();
        assert!(updated.changed(root.path()));
    }
    fn fixture(root: &Path, client: bool) {
        let data = if client {
            "RustClient_Data"
        } else {
            "RustDedicated_Data"
        };
        let dir = root.join(data).join("Plugins/x86_64");
        std::fs::create_dir_all(&dir).unwrap();
        let mut pe = vec![0; 128];
        pe[..2].copy_from_slice(b"MZ");
        pe[60..64].copy_from_slice(&64_u32.to_le_bytes());
        pe[64..70].copy_from_slice(b"PE\0\0\x64\x86");
        std::fs::write(dir.join("RustNative.dll"), pe).unwrap();
    }
    #[test]
    fn detects_both_layouts_and_fingerprints_changes_without_paths() {
        for client in [true, false] {
            let root = tempfile::tempdir().unwrap();
            fixture(root.path(), client);
            let first = inspect(root.path(), || false).unwrap();
            assert_eq!(first.kind == InstallationKind::Client, client);
            assert!(first.scene_manifest.is_none());
            let relocated = tempfile::tempdir().unwrap();
            fixture(relocated.path(), client);
            assert_eq!(first, inspect(relocated.path(), || false).unwrap());
            std::fs::create_dir(root.path().join("Bundles")).unwrap();
            std::fs::write(root.path().join("Bundles/AssetSceneManifest.json"), b"{}").unwrap();
            let second = inspect(root.path(), || false).unwrap();
            assert_ne!(first, second);
            std::fs::write(
                root.path().join("Bundles/AssetSceneManifest.json"),
                b"{\"changed\":true}",
            )
            .unwrap();
            assert_ne!(second, inspect(root.path(), || false).unwrap());
            let native_path = root.path().join(&first.native.relative_path);
            let mut bytes = std::fs::read(&native_path).unwrap();
            bytes.push(1);
            std::fs::write(native_path, bytes).unwrap();
            assert_ne!(
                first.native.sha256,
                inspect(root.path(), || false).unwrap().native.sha256
            );
        }
    }
    #[test]
    fn hashing_respects_cancellation_and_includes_a_partial_last_read() {
        let root = tempfile::tempdir().unwrap();
        let data = vec![0x7b; 1024 * 1024 + 123];
        std::fs::write(root.path().join("source.bundle"), &data).unwrap();
        let calls = std::cell::Cell::new(0);
        let cancelled = || {
            calls.set(calls.get() + 1);
            calls.get() >= 4
        };
        let length = data.len() as u64;
        assert_eq!(
            hash_file(root.path(), "source.bundle", length, &cancelled).unwrap_err(),
            "Installation inspection cancelled"
        );
        let receipt = hash_file(root.path(), "source.bundle", length, &|| false).unwrap();
        assert_eq!(receipt.bytes, length);
        assert_eq!(receipt.sha256, format!("{:x}", Sha256::digest(&data)));
        assert!(hash_file(root.path(), "source.bundle", length - 1, &|| false).is_err());
    }

    #[test]
    fn read_ahead_hashes_partial_reads_in_order_and_can_cancel() {
        let data: Vec<u8> = (0..251).cycle().take(1024).collect();
        let mut hash = Sha256::new();
        read_ahead_chunks(data.as_slice(), 127, |chunk| {
            hash.update(chunk);
            Ok(())
        })
        .unwrap();
        assert_eq!(hash.finalize(), Sha256::digest(&data));

        let mut processed = 0;
        assert_eq!(
            read_ahead_chunks(data.as_slice(), 127, |chunk| {
                processed += chunk.len();
                if processed >= 254 {
                    return Err("Cancelled".into());
                }
                Ok(())
            })
            .err()
            .as_deref(),
            Some("Cancelled")
        );
    }

    #[test]
    fn pe_inspection_checks_the_machine_and_bounds_the_header_offset() {
        let limit: u32 = 1024 * 1024;
        let mut image = vec![0; limit as usize + 1];
        image[..2].copy_from_slice(b"MZ");
        for offset in [limit - 6, limit - 5] {
            image[60..64].copy_from_slice(&offset.to_le_bytes());
            let start = offset as usize;
            image[start..start + 6].copy_from_slice(b"PE\0\0\x64\x86");
            assert_eq!(
                check_windows_x64(std::io::Cursor::new(&image)).is_ok(),
                offset == limit - 6,
            );
        }
        image[60..64].copy_from_slice(&64_u32.to_le_bytes());
        image[64..70].copy_from_slice(b"PE\0\0\x4c\x01");
        assert!(check_windows_x64(std::io::Cursor::new(&image)).is_err());
        assert!(check_windows_x64(std::io::Cursor::new(&image[..69])).is_err());
    }

    #[test]
    fn rejects_missing_ambiguous_invalid_and_cancelled_sources() {
        let root = tempfile::tempdir().unwrap();
        assert!(inspect(root.path(), || false).is_err());
        fixture(root.path(), true);
        assert!(inspect(root.path(), || true).is_err());
        let mut item = inspect(root.path(), || false).unwrap();
        item.native.relative_path = "../bad.dll".into();
        assert!(item.validate().is_err());
        std::fs::write(
            root.path()
                .join("RustClient_Data/Plugins/x86_64/RustNative.dll"),
            b"wrong",
        )
        .unwrap();
        assert!(inspect(root.path(), || false).is_err());
        fixture(root.path(), true);
        fixture(root.path(), false);
        assert!(inspect(root.path(), || false).is_err());
    }
}
