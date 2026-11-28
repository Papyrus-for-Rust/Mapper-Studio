//! Safe access to the Windows system memory counters.
#![cfg(windows)]

/// Returns total physical RAM, available physical RAM, and available commit, in bytes.
pub fn physical_memory() -> Option<(u64, u64, u64)> {
    #[repr(C)]
    #[derive(Default)]
    struct MemoryStatus {
        length: u32,
        load: u32,
        total_physical: u64,
        available_physical: u64,
        total_commit: u64,
        available_commit: u64,
        total_virtual: u64,
        available_virtual: u64,
        extended_virtual: u64,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalMemoryStatusEx(status: *mut MemoryStatus) -> i32;
    }
    let mut status = MemoryStatus {
        length: std::mem::size_of::<MemoryStatus>() as u32,
        ..MemoryStatus::default()
    };
    // The initialized buffer has the layout and size expected by GlobalMemoryStatusEx.
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 || status.total_physical == 0 {
        return None;
    }
    Some((status.total_physical, status.available_physical, status.available_commit))
}
