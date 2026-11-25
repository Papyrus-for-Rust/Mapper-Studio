//! Safe handle-based identity; file ownership and sharing stay with the caller.
#[cfg(windows)]
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    volume: u64,
    file: [u8; 16],
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revision {
    pub identity: Identity,
    pub bytes: u64,
    usn: i64,
    change_time: i64,
    write_time: i64,
    journal_id: u64,
    observed_next: i64,
    lowest_valid: i64,
}
#[cfg(windows)]
impl Revision {
    /// Continuity and the file's complete revision must both match. Other
    /// files can advance the journal without invalidating this file.
    pub fn can_reuse(&self, current: &Self) -> bool {
        self.identity == current.identity
            && self.bytes == current.bytes
            && self.usn > 0
            && self.usn == current.usn
            && self.change_time == current.change_time
            && self.write_time == current.write_time
            && self.journal_id != 0
            && self.journal_id == current.journal_id
            && self.observed_next > self.usn
            && self.lowest_valid >= 0
            && self.lowest_valid <= self.observed_next
            && current.lowest_valid <= self.observed_next
            && self.observed_next <= current.observed_next
    }
}

/// A complete file revision and journal continuity token for bounded checksum
/// reuse. Callers retain a read-only lease while using it. Hard links fail closed.
#[cfg(windows)]
pub fn revision(file: &std::fs::File) -> std::io::Result<Revision> {
    use std::{
        mem::{MaybeUninit, size_of},
        os::windows::io::AsRawHandle,
    };
    use windows_sys::Win32::{
        Storage::FileSystem::{
            FILE_BASIC_INFO, FILE_STANDARD_INFO, FileBasicInfo, FileStandardInfo,
            GetFileInformationByHandleEx,
        },
        System::{
            IO::DeviceIoControl,
            Ioctl::{FSCTL_READ_FILE_USN_DATA, READ_FILE_USN_DATA},
        },
    };
    let handle = file.as_raw_handle();
    let before_journal = journal(file)?;
    let mut basic = MaybeUninit::<FILE_BASIC_INFO>::uninit();
    let mut standard = MaybeUninit::<FILE_STANDARD_INFO>::uninit();
    // SAFETY: live borrowed handle, correctly sized/aligned writable buffers.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileBasicInfo,
            basic.as_mut_ptr().cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        ) != 0
            && GetFileInformationByHandleEx(
                handle,
                FileStandardInfo,
                standard.as_mut_ptr().cast(),
                size_of::<FILE_STANDARD_INFO>() as u32,
            ) != 0
    };
    if !ok {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: both synchronous queries succeeded and initialized their buffers.
    let (basic, standard) = unsafe { (basic.assume_init(), standard.assume_init()) };
    if standard.NumberOfLinks != 1
        || standard.EndOfFile < 0
        || standard.Directory
        || standard.DeletePending
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Source has no reusable file revision",
        ));
    }
    let input = READ_FILE_USN_DATA {
        MinMajorVersion: 2,
        MaxMajorVersion: 3,
    };
    let mut output = [0u8; 4096];
    let mut returned = 0;
    // SAFETY: fixed initialized request, bounded writable byte buffer and valid
    // result length. The synchronous call keeps all borrowed buffers alive.
    let ok = unsafe {
        DeviceIoControl(
            handle,
            FSCTL_READ_FILE_USN_DATA,
            (&input as *const READ_FILE_USN_DATA).cast(),
            size_of::<READ_FILE_USN_DATA>() as u32,
            output.as_mut_ptr().cast(),
            output.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let raw = output
        .get(..returned as usize)
        .ok_or_else(|| std::io::Error::other("Oversized journal response"))?;
    let usn = parse_usn(raw)?;
    let after_journal = journal(file)?;
    if before_journal.0 != after_journal.0
        || after_journal.1 > before_journal.2
        || after_journal.2 < before_journal.2
        || usn >= after_journal.2
    {
        return Err(std::io::Error::other(
            "Source journal changed during inspection",
        ));
    }
    Ok(Revision {
        identity: identity(file)?,
        bytes: standard.EndOfFile as u64,
        usn,
        change_time: basic.ChangeTime,
        write_time: basic.LastWriteTime,
        journal_id: after_journal.0,
        lowest_valid: after_journal.1,
        observed_next: after_journal.2,
    })
}

#[cfg(windows)]
fn journal(file: &std::fs::File) -> std::io::Result<(u64, i64, i64)> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::{IO::DeviceIoControl, Ioctl::FSCTL_QUERY_USN_JOURNAL};
    let mut output = [0u8; 128];
    let mut returned = 0;
    // SAFETY: borrowed live handle, no input, bounded initialized output and
    // result-length buffers. This read-only query never creates a journal.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_QUERY_USN_JOURNAL,
            std::ptr::null(),
            0,
            output.as_mut_ptr().cast(),
            output.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let bytes = output
        .get(..returned as usize)
        .ok_or_else(|| std::io::Error::other("Oversized source journal response"))?;
    parse_journal(bytes)
}

#[cfg(windows)]
fn parse_journal(output: &[u8]) -> std::io::Result<(u64, i64, i64)> {
    if !(56..=128).contains(&output.len()) {
        return Err(std::io::Error::other("Unsupported source journal response"));
    }
    // USN_JOURNAL_DATA_V0's prefix is shared by the later supported layouts.
    let id = u64::from_le_bytes(output[..8].try_into().unwrap());
    let first = i64::from_le_bytes(output[8..16].try_into().unwrap());
    let next = i64::from_le_bytes(output[16..24].try_into().unwrap());
    let lowest = i64::from_le_bytes(output[24..32].try_into().unwrap());
    if id == 0 || first < 0 || lowest < 0 || next <= 0 || first > next || lowest > next {
        return Err(std::io::Error::other("Invalid source journal continuity"));
    }
    // FirstUsn advances when old records are trimmed. LowestValidUsn records
    // actual discontinuities, which can invalidate a retained file revision.
    Ok((id, lowest, next))
}

#[cfg(windows)]
fn parse_usn(raw: &[u8]) -> std::io::Result<i64> {
    let invalid = || {
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Unsupported file journal record",
        )
    };
    if raw.len() < 8 {
        return Err(invalid());
    }
    let length = u32::from_le_bytes(raw[..4].try_into().unwrap()) as usize;
    let (minimum, at) = match u16::from_le_bytes(raw[4..6].try_into().unwrap()) {
        2 => (60, 24),
        3 => (76, 40),
        _ => return Err(invalid()),
    };
    if length < minimum || length > raw.len() {
        return Err(invalid());
    }
    let usn = i64::from_le_bytes(raw[at..at + 8].try_into().unwrap());
    if usn <= 0 {
        return Err(invalid());
    }
    Ok(usn)
}

/// Coarse, lossless invalidation for one local source directory. Notifications
/// are rearmed before a changed result is returned. Unknown/error states reject
/// cache reuse. Notification handles are session-local.
#[cfg(windows)]
pub struct DirectoryWatch {
    handle: isize,
    path: std::path::PathBuf,
    identity: Identity,
}
#[cfg(windows)]
impl DirectoryWatch {
    pub fn new(path: &std::path::Path) -> std::io::Result<Self> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::{
            Foundation::INVALID_HANDLE_VALUE,
            Storage::FileSystem::{
                FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_DIR_NAME,
                FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
                FILE_NOTIFY_CHANGE_SECURITY, FILE_NOTIFY_CHANGE_SIZE, FindFirstChangeNotificationW,
            },
        };
        let id = directory_identity(path)?;
        let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        if name[..name.len() - 1].contains(&0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Invalid source directory",
            ));
        }
        // SAFETY: NUL-terminated path remains alive through this synchronous call.
        let handle = unsafe {
            FindFirstChangeNotificationW(
                name.as_ptr(),
                0,
                FILE_NOTIFY_CHANGE_FILE_NAME
                    | FILE_NOTIFY_CHANGE_DIR_NAME
                    | FILE_NOTIFY_CHANGE_SIZE
                    | FILE_NOTIFY_CHANGE_LAST_WRITE
                    | FILE_NOTIFY_CHANGE_ATTRIBUTES
                    | FILE_NOTIFY_CHANGE_SECURITY,
            )
        };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        // The notification handle is thread-neutral. Store its opaque integer
        // value, own it exactly once and close with FindCloseChangeNotification.
        let watch = Self {
            handle: handle as isize,
            path: path.to_owned(),
            identity: id,
        };
        if directory_identity(path)? != id {
            return Err(std::io::Error::other(
                "Source directory changed while registering its watch",
            ));
        }
        Ok(watch)
    }
    pub fn unchanged(&mut self) -> std::io::Result<bool> {
        use windows_sys::Win32::{
            Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT},
            Storage::FileSystem::FindNextChangeNotification,
            System::Threading::WaitForSingleObject,
        };
        if directory_identity(&self.path)? != self.identity {
            return Err(std::io::Error::other("Source directory was replaced"));
        }
        // SAFETY: this object uniquely owns a live notification handle. Its
        // caller serializes access and the wait never blocks.
        match unsafe { WaitForSingleObject(self.handle as _, 0) } {
            WAIT_TIMEOUT => Ok(true),
            WAIT_OBJECT_0 => {
                // SAFETY: same live handle, rearmed before receipts are cleared.
                if unsafe { FindNextChangeNotification(self.handle as _) } == 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(false)
            }
            _ => Err(std::io::Error::last_os_error()),
        }
    }
}
#[cfg(windows)]
fn directory_identity(path: &std::path::Path) -> std::io::Result<Identity> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
    identity(
        &std::fs::OpenOptions::new()
            .read(true)
            .share_mode(7)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)?,
    )
}
#[cfg(windows)]
impl Drop for DirectoryWatch {
    fn drop(&mut self) {
        // SAFETY: unique ownership, correct close function, exactly once.
        unsafe {
            windows_sys::Win32::Storage::FileSystem::FindCloseChangeNotification(self.handle as _);
        }
    }
}
#[cfg(windows)]
impl Identity {
    /// Stable handle identity for a child of the process retaining that handle.
    pub fn parts(self) -> (u64, [u8; 16]) {
        (self.volume, self.file)
    }
}

/// Query the full 128-bit ID, including on ReFS. Unsupported filesystems fail
/// closed so callers can hash normally instead of using an identity cache.
#[cfg(windows)]
pub fn identity(file: &std::fs::File) -> std::io::Result<Identity> {
    use std::{
        mem::{MaybeUninit, size_of},
        os::windows::io::AsRawHandle,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
    };
    let mut info = MaybeUninit::<FILE_ID_INFO>::uninit();
    // SAFETY: the borrowed File keeps its handle alive. The aligned buffer has
    // exactly FILE_ID_INFO's size and remains writable for the synchronous call.
    let success = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            info.as_mut_ptr().cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if success == 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: success means Windows initialized this fixed-size integer struct.
    let info = unsafe { info.assume_init() };
    Ok(Identity {
        volume: info.VolumeSerialNumber,
        file: info.FileId.Identifier,
    })
}

/// Available physical bytes and commit headroom for bounded source loading.
#[cfg(windows)]
#[derive(Clone, Copy, Debug)]
pub struct AvailableMemory {
    pub physical: u64,
    pub commit: u64,
}
#[cfg(windows)]
pub fn available_memory() -> std::io::Result<AvailableMemory> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: status points to a writable, initialized structure with the SDK
    // size set. The call is synchronous and retains no pointers.
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(AvailableMemory {
        physical: status.ullAvailPhys,
        commit: status.ullAvailPageFile,
    })
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn journal_trimming_preserves_file_tokens_and_discontinuities_still_invalidate() {
        let mut raw = [0u8; 56];
        raw[..8].copy_from_slice(&400u64.to_le_bytes());
        raw[8..16].copy_from_slice(&800i64.to_le_bytes());
        raw[16..24].copy_from_slice(&900i64.to_le_bytes());
        assert_eq!(super::parse_journal(&raw).unwrap(), (400, 0, 900));
        let file = tempfile::tempfile().unwrap();
        let before = super::Revision {
            identity: super::identity(&file).unwrap(),
            bytes: 42,
            usn: 100,
            change_time: 200,
            write_time: 300,
            journal_id: 400,
            observed_next: 500,
            lowest_valid: 0,
        };
        let (journal_id, lowest_valid, observed_next) = super::parse_journal(&raw).unwrap();
        assert!(before.can_reuse(&super::Revision {
            journal_id,
            lowest_valid,
            observed_next,
            ..before
        }));
        raw[24..32].copy_from_slice(&501i64.to_le_bytes());
        let (journal_id, lowest_valid, observed_next) = super::parse_journal(&raw).unwrap();
        assert!(!before.can_reuse(&super::Revision {
            journal_id,
            lowest_valid,
            observed_next,
            ..before
        }));
        assert!(super::parse_journal(&raw[..55]).is_err());
        raw[8..16].copy_from_slice(&901i64.to_le_bytes());
        assert!(super::parse_journal(&raw).is_err());
    }
    #[test]
    fn receipt_reuse_requires_journal_continuity_and_the_exact_file_revision() {
        let file = tempfile::tempfile().unwrap();
        let before = super::Revision {
            identity: super::identity(&file).unwrap(),
            bytes: 42,
            usn: 100,
            change_time: 200,
            write_time: 300,
            journal_id: 400,
            observed_next: 500,
            lowest_valid: 0,
        };
        assert!(before.can_reuse(&before));
        assert!(before.can_reuse(&super::Revision {
            observed_next: 900,
            ..before
        }));
        for changed in [
            super::Revision {
                journal_id: 401,
                ..before
            },
            super::Revision {
                lowest_valid: 501,
                observed_next: 900,
                ..before
            },
            super::Revision {
                observed_next: 499,
                ..before
            },
            super::Revision { usn: 101, ..before },
            super::Revision {
                change_time: 201,
                ..before
            },
            super::Revision {
                write_time: 301,
                ..before
            },
            super::Revision {
                bytes: 43,
                ..before
            },
            super::Revision {
                identity: super::identity(&tempfile::tempfile().unwrap()).unwrap(),
                ..before
            },
        ] {
            assert!(!before.can_reuse(&changed));
        }
    }
    #[test]
    fn journal_parser_accepts_only_bounded_known_records_with_positive_usns() {
        for (version, minimum, at) in [(2u16, 60usize, 24usize), (3, 76, 40)] {
            let mut raw = vec![0u8; minimum];
            raw[..4].copy_from_slice(&(minimum as u32).to_le_bytes());
            raw[4..6].copy_from_slice(&version.to_le_bytes());
            raw[at..at + 8].copy_from_slice(&71i64.to_le_bytes());
            assert_eq!(super::parse_usn(&raw).unwrap(), 71);
            assert!(super::parse_usn(&raw[..minimum - 1]).is_err());
            raw[at..at + 8].fill(0);
            assert!(super::parse_usn(&raw).is_err());
            raw[4..6].copy_from_slice(&4u16.to_le_bytes());
            assert!(super::parse_usn(&raw).is_err());
        }
        assert!(super::parse_usn(&[]).is_err());
    }
    #[test]
    fn directory_watches_invalidate_on_writes_and_replacement_and_allow_rename() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("watched");
        std::fs::create_dir(&path).unwrap();
        let mut watch = super::DirectoryWatch::new(&path).unwrap();
        assert!(watch.unchanged().unwrap());
        std::fs::write(path.join("source"), b"data").unwrap();
        let mut changed = false;
        for _ in 0..100 {
            if !watch.unchanged().unwrap() {
                changed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(changed);
        std::fs::rename(&path, root.path().join("previous")).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(watch.unchanged().is_err());
    }
    #[test]
    fn journal_revisions_reject_hard_links_and_detect_preserved_write_times() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source");
        std::fs::write(&path, b"old").unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let before = super::revision(&file);
        let modified = file.metadata().unwrap().modified().unwrap();
        drop(file);
        std::fs::write(&path, b"new").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let after = super::revision(&file);
        if let Ok(before) = before {
            assert!(!before.can_reuse(&after.unwrap()));
        }
        std::fs::hard_link(&path, root.path().join("alias")).unwrap();
        assert!(super::revision(&file).is_err());
    }
    #[test]
    fn identities_follow_open_files_and_distinguish_equal_content() {
        let a = tempfile::tempfile().unwrap();
        let b = tempfile::tempfile().unwrap();
        assert_eq!(
            super::identity(&a).unwrap(),
            super::identity(&a.try_clone().unwrap()).unwrap()
        );
        assert_ne!(super::identity(&a).unwrap(), super::identity(&b).unwrap());
    }
}
