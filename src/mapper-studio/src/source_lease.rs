//! Strong source receipts with read-only operation leases and journal validation.
#[cfg(windows)]
mod persistent;
#[cfg(windows)]
pub(crate) use persistent::batch as batch_persistent_writes;
#[cfg(windows)]
mod windows {
    use crate::installation::SourceFile;
    use std::{
        cell::RefCell,
        collections::BTreeMap,
        fs::{File, OpenOptions},
        os::windows::fs::OpenOptionsExt,
        path::{Path, PathBuf},
        rc::{Rc, Weak},
    };
    struct Entry {
        _file: File,
        id: papyrus_file_id::Identity,
        receipt: SourceFile,
    }
    type Entries = RefCell<BTreeMap<PathBuf, Entry>>;
    struct VerifiedDirectory {
        path: PathBuf,
        watch: papyrus_file_id::DirectoryWatch,
        receipts: BTreeMap<papyrus_file_id::Identity, (papyrus_file_id::Revision, SourceFile)>,
    }
    impl VerifiedDirectory {
        fn lookup(
            &mut self,
            revision: papyrus_file_id::Revision,
            limit: u64,
        ) -> Option<SourceFile> {
            if !self.watch.unchanged().ok()? {
                self.receipts.clear();
                return None;
            }
            let (before, source) = self.receipts.get(&revision.identity)?;
            (before.can_reuse(&revision) && source.bytes == revision.bytes && source.bytes <= limit)
                .then(|| source.clone())
        }
    }
    static VERIFIED: std::sync::OnceLock<std::sync::Mutex<Vec<VerifiedDirectory>>> =
        std::sync::OnceLock::new();

    fn session_lookup(path: &Path, file: &File, limit: u64) -> Option<SourceFile> {
        let revision = papyrus_file_id::revision(file).ok()?;
        if revision.bytes > limit {
            return None;
        }
        let parent = path.parent()?.canonicalize().ok()?;
        if let Some(receipt) = VERIFIED
            .get_or_init(Default::default)
            .lock()
            .ok()?
            .iter_mut()
            .find(|d| d.path == parent)
            .and_then(|directory| directory.lookup(revision, limit))
        {
            return Some(receipt);
        }
        let receipt = super::persistent::lookup(revision, limit)?;
        remember_session(parent, revision, &receipt);
        Some(receipt)
    }
    pub fn clear_session_cache() {
        if let Ok(mut directories) = VERIFIED.get_or_init(Default::default).lock() {
            directories.clear();
        }
        super::persistent::clear();
    }
    pub(crate) fn release_directory_watches(root: &Path) {
        if let Some(directories) = VERIFIED.get()
            && let Ok(mut directories) = directories.lock()
        {
            directories.retain(|directory| !directory.path.starts_with(root));
        }
    }
    fn remember_session(
        parent: PathBuf,
        revision: papyrus_file_id::Revision,
        receipt: &SourceFile,
    ) {
        if revision.bytes != receipt.bytes {
            return;
        }
        let Ok(mut directories) = VERIFIED.get_or_init(Default::default).lock() else {
            return;
        };
        if directories.iter().map(|d| d.receipts.len()).sum::<usize>() >= 64
            || directories.len() >= 16
        {
            directories.clear();
        }
        let mut position = directories.iter().position(|d| d.path == parent);
        if let Some(index) = position {
            match directories[index].watch.unchanged() {
                Ok(true) => {}
                Ok(false) => directories[index].receipts.clear(),
                Err(_) => {
                    directories.remove(index);
                    position = None;
                }
            }
        }
        let position = match position {
            Some(index) => index,
            None => {
                let Ok(watch) = papyrus_file_id::DirectoryWatch::new(&parent) else {
                    return;
                };
                directories.push(VerifiedDirectory {
                    path: parent,
                    watch,
                    receipts: BTreeMap::new(),
                });
                directories.len() - 1
            }
        };
        directories[position]
            .receipts
            .insert(revision.identity, (revision, receipt.clone()));
    }
    thread_local! { static CURRENT: RefCell<Weak<Entries>> = const { RefCell::new(Weak::new()) }; }
    /// Nested scopes share a bounded cache; the last drop releases all leases.
    pub struct Scope {
        _entries: Rc<Entries>,
    }
    impl Default for Scope {
        fn default() -> Self {
            let entries = CURRENT.with(|c| {
                if let Some(entries) = c.borrow().upgrade() {
                    return entries;
                }
                let entries = Rc::new(Entries::default());
                *c.borrow_mut() = Rc::downgrade(&entries);
                entries
            });
            Self { _entries: entries }
        }
    }
    /// Owned duplicate handles can move to a new worker. The original read-only
    /// leases remain in force throughout the transfer and worker execution.
    pub(crate) struct WorkerLeases(BTreeMap<PathBuf, Entry>);
    pub(crate) fn fork() -> Result<WorkerLeases, String> {
        CURRENT.with(|current| {
            let mut copied = BTreeMap::new();
            if let Some(entries) = current.borrow().upgrade() {
                for (path, entry) in entries.borrow().iter() {
                    copied.insert(
                        path.clone(),
                        Entry {
                            _file: entry
                                ._file
                                .try_clone()
                                .map_err(|e| format!("Cannot retain verified Rust source: {e}"))?,
                            id: entry.id,
                            receipt: entry.receipt.clone(),
                        },
                    );
                }
            }
            Ok(WorkerLeases(copied))
        })
    }
    impl WorkerLeases {
        pub(crate) fn enter(self) -> Scope {
            let scope = Scope::default();
            let mut entries = scope._entries.borrow_mut();
            for (path, entry) in self.0 {
                if entries.len() < 64 {
                    entries.entry(path).or_insert(entry);
                }
            }
            drop(entries);
            scope
        }
    }
    pub fn open(path: &Path) -> std::io::Result<File> {
        let scoped = CURRENT.with(|c| c.borrow().upgrade().is_some());
        if scoped {
            // FILE_SHARE_READ only: no writers, replacement or deletion while
            // the verified handle is retained. Existing writers also conflict.
            const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x08000000;
            OpenOptions::new()
                .read(true)
                .share_mode(1)
                .custom_flags(FILE_FLAG_SEQUENTIAL_SCAN)
                .open(path)
        } else {
            File::open(path)
        }
    }
    /// Reuse a receipt and take ownership of the open deny-write handle if it
    /// has not already been leased in this operation.
    pub fn reuse(path: &Path, file: File, limit: u64) -> Result<SourceFile, File> {
        CURRENT.with(|c| {
            let Some(entries) = c.borrow().upgrade() else {
                return Err(file);
            };
            let Ok(id) = papyrus_file_id::identity(&file) else {
                return Err(file);
            };
            let mut entries = entries.borrow_mut();
            // Relative/absolute paths, case aliases and hard links can identify
            // the same leased file. Its retained handle still denies writes and
            // replacement, so the handle identity can reuse that exact digest.
            // The bounded fallback avoids hashing large bundles again when a
            // later stage canonicalizes the installation root.
            let entry = entries
                .get(path)
                .filter(|entry| entry.id == id)
                .or_else(|| entries.values().find(|entry| entry.id == id));
            if let Some(entry) = entry {
                return if entry.receipt.bytes <= limit {
                    Ok(entry.receipt.clone())
                } else {
                    Err(file)
                };
            }
            if entries.len() >= 64 {
                return Err(file);
            }
            let Some(receipt) = session_lookup(path, &file, limit) else {
                return Err(file);
            };
            entries.insert(
                path.to_owned(),
                Entry {
                    _file: file,
                    id,
                    receipt: receipt.clone(),
                },
            );
            Ok(receipt)
        })
    }
    pub fn retain(path: PathBuf, file: File, receipt: SourceFile) {
        let Ok(id) = papyrus_file_id::identity(&file) else {
            return;
        };
        CURRENT.with(|c| {
            if let Some(entries) = c.borrow().upgrade() {
                if entries.borrow().values().any(|entry| entry.id == id) {
                    return;
                }
                if let Ok(revision) = papyrus_file_id::revision(&file) {
                    if let Some(parent) = path.parent().and_then(|p| p.canonicalize().ok()) {
                        remember_session(parent, revision, &receipt);
                    }
                    super::persistent::remember(revision, &receipt);
                }
                let mut entries = entries.borrow_mut();
                if entries.len() < 64 {
                    entries.insert(
                        path,
                        Entry {
                            _file: file,
                            id,
                            receipt,
                        },
                    );
                }
            }
        });
    }
    /// Export only live, deny-write leases to a synchronous metadata child.
    pub fn receipts() -> Vec<serde_json::Value> {
        CURRENT.with(|current| {
            let Some(entries) = current.borrow().upgrade() else {
                return Vec::new();
            };
            entries
                .borrow()
                .values()
                .map(|entry| {
                    let (volume, id) = entry.id.parts();
                    let file_id: String = id.iter().map(|b| format!("{b:02x}")).collect();
                    serde_json::json!({"volume":volume,"file_id":file_id,
                    "bytes":entry.receipt.bytes,"sha256":entry.receipt.sha256})
                })
                .collect()
        })
    }
    #[cfg(test)]
    mod session_tests {
        use super::*;
        #[test]
        fn session_receipts_require_the_same_revision_watch_identity_and_budget() {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("source");
            std::fs::write(&path, b"old").unwrap();
            let source =
                crate::installation::hash_file(root.path(), "source", 32, &|| false).unwrap();
            let file = File::open(&path).unwrap();
            let Ok(revision) = papyrus_file_id::revision(&file) else {
                assert!(session_lookup(&path, &file, 32).is_none());
                return;
            };
            let modified = file.metadata().unwrap().modified().unwrap();
            let mut directory = VerifiedDirectory {
                path: root.path().to_owned(),
                watch: papyrus_file_id::DirectoryWatch::new(root.path()).unwrap(),
                receipts: BTreeMap::from([(revision.identity, (revision, source.clone()))]),
            };
            assert_eq!(directory.lookup(revision, 32), Some(source.clone()));
            assert!(directory.lookup(revision, 2).is_none());
            drop(file);
            let reopened = File::open(&path).unwrap();
            assert_eq!(
                directory.lookup(papyrus_file_id::revision(&reopened).unwrap(), 32),
                Some(source)
            );
            drop(reopened);
            std::fs::write(&path, b"new").unwrap();
            std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(modified))
                .unwrap();
            let changed = File::open(&path).unwrap();
            assert!(
                directory
                    .lookup(papyrus_file_id::revision(&changed).unwrap(), 32)
                    .is_none()
            );
            drop(changed);
            std::fs::remove_file(&path).unwrap();
            std::fs::write(&path, b"old").unwrap();
            let replacement = File::open(&path).unwrap();
            assert!(
                directory
                    .lookup(papyrus_file_id::revision(&replacement).unwrap(), 32)
                    .is_none()
            );
        }

        #[test]
        fn reuse_from_previous_scope_retains_the_open_file() {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("source.bundle");
            std::fs::write(&path, b"source").unwrap();
            if papyrus_file_id::revision(&File::open(&path).unwrap()).is_err() {
                return;
            }
            let receipt =
                crate::installation::hash_file(root.path(), "source.bundle", 32, &|| false)
                    .unwrap();
            assert!(receipts().is_empty());
            let scope = Scope::default();
            assert_eq!(reuse(&path, open(&path).unwrap(), 32).unwrap(), receipt);
            assert_eq!(receipts().len(), 1);
            assert!(std::fs::write(&path, b"change").is_err());
            drop(scope);
            std::fs::write(&path, b"change").unwrap();
        }
    }
}
#[cfg(windows)]
pub use windows::*;
#[cfg(not(windows))]
#[derive(Default)]
pub struct Scope {
    _private: (),
}
#[cfg(not(windows))]
pub fn open(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}
#[cfg(not(windows))]
pub fn reuse(
    _: &std::path::Path,
    file: std::fs::File,
    _: u64,
) -> Result<crate::installation::SourceFile, std::fs::File> {
    Err(file)
}
#[cfg(not(windows))]
pub fn retain(_: std::path::PathBuf, _: std::fs::File, _: crate::installation::SourceFile) {}
#[cfg(not(windows))]
pub fn receipts() -> Vec<serde_json::Value> {
    Vec::new()
}
#[cfg(not(windows))]
pub fn clear_session_cache() {}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::installation::hash_file;
    #[test]
    fn scoped_hash_reuse_denies_changes_and_rechecks_after_release() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.bundle");
        std::fs::write(&path, b"old").unwrap();
        let before;
        assert!(receipts().is_empty());
        {
            let _scope = Scope::default();
            before = hash_file(dir.path(), "source.bundle", 32, &|| false).unwrap();
            let exported = receipts();
            assert_eq!(exported.len(), 1);
            assert_eq!(exported[0]["sha256"], before.sha256);
            assert_eq!(exported[0]["bytes"], before.bytes);
            assert_eq!(exported[0]["file_id"].as_str().unwrap().len(), 32);
            let probe = open(&path).unwrap();
            assert_eq!(reuse(&path, probe, 32).unwrap(), before);
            assert!(std::fs::write(&path, b"new").is_err());
            assert!(std::fs::remove_file(&path).is_err());
            assert_eq!(
                hash_file(dir.path(), "source.bundle", 32, &|| false).unwrap(),
                before
            );
            assert!(hash_file(dir.path(), "source.bundle", 2, &|| false).is_err());
            assert!(hash_file(dir.path(), "source.bundle", 32, &|| true).is_err());
        }
        assert!(receipts().is_empty());
        assert!(reuse(&path, open(&path).unwrap(), 32).is_err());
        // Equal byte counts still require a new strong hash in the next scope.
        std::fs::write(&path, b"new").unwrap();
        let _scope = Scope::default();
        let after = hash_file(dir.path(), "source.bundle", 32, &|| false).unwrap();
        assert_ne!(before.sha256, after.sha256);
    }
    #[test]
    fn aliases_reuse_the_same_leased_file_without_reusing_another_file() {
        let working = std::env::current_dir().unwrap();
        let dir = tempfile::tempdir_in(&working).unwrap();
        let path = dir.path().join("source.bundle");
        let hard_link = dir.path().join("linked.bundle");
        std::fs::write(&path, b"source").unwrap();
        std::fs::hard_link(&path, &hard_link).unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        let alternate = dir.path().join("nested/../source.bundle");
        let relative = path.strip_prefix(&working).unwrap().to_path_buf();
        let receipt;
        {
            let _scope = Scope::default();
            receipt = hash_file(dir.path(), "source.bundle", 32, &|| false).unwrap();
            for alias in [
                relative,
                alternate,
                path.canonicalize().unwrap(),
                hard_link.clone(),
            ] {
                assert_eq!(reuse(&alias, open(&alias).unwrap(), 32).unwrap(), receipt);
                assert!(reuse(&alias, open(&alias).unwrap(), 2).is_err());
                assert!(std::fs::write(&alias, b"change").is_err());
            }
            let linked = hash_file(dir.path(), "linked.bundle", 32, &|| false).unwrap();
            assert_eq!(linked.relative_path, "linked.bundle");
            assert_eq!(linked.sha256, receipt.sha256);
            let other = dir.path().join("other.bundle");
            std::fs::write(&other, b"source").unwrap();
            assert!(reuse(&other, open(&other).unwrap(), 32).is_err());
        }
        std::fs::write(&hard_link, b"change").unwrap();
        let _scope = Scope::default();
        assert!(reuse(&path, open(&path).unwrap(), 32).is_err());
        let changed = hash_file(dir.path(), "source.bundle", 32, &|| false).unwrap();
        assert_ne!(changed.sha256, receipt.sha256);
    }
    #[test]
    fn workers_inherit_exact_receipts_and_keep_source_handles_alive() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source.bundle");
        std::fs::write(&path, b"source").unwrap();
        let owner = Scope::default();
        let receipt =
            crate::installation::hash_file(root.path(), "source.bundle", 32, &|| false).unwrap();
        let inherited = fork().unwrap();
        let before = receipts();
        drop(owner);
        // Duplicated handles deny writes even before the worker enters its scope.
        assert!(std::fs::write(&path, b"change").is_err());
        let source_path = path.clone();
        std::thread::spawn(move || {
            let scope = inherited.enter();
            let nested = Scope::default();
            drop(scope);
            assert_eq!(receipts(), before);
            assert_eq!(
                reuse(&source_path, open(&source_path).unwrap(), 32).unwrap(),
                receipt
            );
            assert!(reuse(&source_path, open(&source_path).unwrap(), 2).is_err());
            assert!(std::fs::write(&source_path, b"change").is_err());
            drop(nested);
            assert!(receipts().is_empty());
        })
        .join()
        .unwrap();
        std::fs::write(&path, b"change").unwrap();
    }
    #[test]
    fn nested_scopes_keep_the_lease_until_the_last_owner_and_conflict_with_existing_writers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.bundle");
        std::fs::write(&path, b"source").unwrap();
        let outer = Scope::default();
        let inner = Scope::default();
        hash_file(dir.path(), "source.bundle", 32, &|| false).unwrap();
        drop(outer);
        assert!(std::fs::write(&path, b"change").is_err());
        drop(inner);
        let writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        let _scope = Scope::default();
        assert!(hash_file(dir.path(), "source.bundle", 32, &|| false).is_err());
        drop(writer);
        hash_file(dir.path(), "source.bundle", 32, &|| false).unwrap();
    }
    #[test]
    fn source_handles_are_bounded_and_do_not_cross_roots() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        std::fs::write(b.path().join("0"), b"other").unwrap();
        let scope = Scope::default();
        for i in 0..65 {
            let name = i.to_string();
            std::fs::write(a.path().join(&name), b"source").unwrap();
            hash_file(a.path(), &name, 32, &|| false).unwrap();
        }
        assert!(reuse(&a.path().join("0"), open(&a.path().join("0")).unwrap(), 32).is_ok());
        assert!(
            reuse(
                &a.path().join("64"),
                open(&a.path().join("64")).unwrap(),
                32
            )
            .is_err()
        );
        assert!(reuse(&b.path().join("0"), open(&b.path().join("0")).unwrap(), 32).is_err());
        std::fs::write(a.path().join("64"), b"change").unwrap();
        drop(scope);
        std::fs::write(a.path().join("0"), b"change").unwrap();
    }
}
