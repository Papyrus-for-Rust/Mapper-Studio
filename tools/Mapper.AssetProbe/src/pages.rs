//! Bounded read-through page for fine-grained Unity metadata reads on Windows.
use std::{
    collections::HashMap,
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use unity_rs_core::source::Source;
const PAGE: usize = 64 * 1024;
const MAX_PAGES: usize = 128;
struct Page {
    start: u64,
    valid: usize,
    bytes: Vec<u8>,
    last_used: u64,
}
struct State {
    file: File,
    current: Page,
    history: HashMap<u64, Page>,
    clock: u64,
}
pub struct PagedFile {
    length: u64,
    state: Mutex<State>,
    pub physical_reads: AtomicU64,
    pub physical_bytes: AtomicU64,
    pub requests: AtomicU64,
}
impl PagedFile {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let length = file.metadata()?.len();
        Ok(Self {
            length,
            state: Mutex::new(State {
                file,
                current: Page {
                    start: 0,
                    valid: 0,
                    bytes: Vec::new(),
                    last_used: 0,
                },
                history: HashMap::new(),
                clock: 0,
            }),
            physical_reads: AtomicU64::new(0),
            physical_bytes: AtomicU64::new(0),
            requests: AtomicU64::new(0),
        })
    }
}
impl Source for PagedFile {
    fn len(&self) -> u64 {
        self.length
    }
    fn read_exact_at(&self, offset: u64, output: &mut [u8]) -> io::Result<()> {
        if offset
            .checked_add(output.len() as u64)
            .is_none_or(|end| end > self.length)
        {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Read outside source",
            ));
        }
        self.requests.fetch_add(1, Ordering::Relaxed);
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("Source page lock poisoned"))?;
        let mut position = offset;
        let mut written = 0;
        while written < output.len() {
            if position < state.current.start
                || position >= state.current.start + state.current.valid as u64
            {
                let start = position / PAGE as u64 * PAGE as u64;
                let mut next = if let Some(page) = state.history.remove(&start) {
                    page
                } else {
                    let mut page = if state.history.len() >= MAX_PAGES - 1 {
                        let oldest = *state
                            .history
                            .iter()
                            .min_by_key(|(_, p)| p.last_used)
                            .unwrap()
                            .0;
                        state.history.remove(&oldest).unwrap()
                    } else {
                        Page {
                            start,
                            valid: 0,
                            bytes: vec![0; PAGE],
                            last_used: 0,
                        }
                    };
                    let count = (self.length - start).min(PAGE as u64) as usize;
                    state.file.seek(SeekFrom::Start(start))?;
                    state.file.read_exact(&mut page.bytes[..count])?;
                    page.start = start;
                    page.valid = count;
                    self.physical_reads.fetch_add(1, Ordering::Relaxed);
                    self.physical_bytes
                        .fetch_add(count as u64, Ordering::Relaxed);
                    page
                };
                state.clock = state
                    .clock
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("Page-use counter exhausted"))?;
                next.last_used = state.clock;
                let mut previous = std::mem::replace(&mut state.current, next);
                if previous.valid != 0 {
                    previous.last_used = state.clock;
                    state.history.insert(previous.start, previous);
                }
            }
            let within = (position - state.current.start) as usize;
            let count = (output.len() - written).min(state.current.valid - within);
            output[written..written + count]
                .copy_from_slice(&state.current.bytes[within..within + count]);
            position += count as u64;
            written += count;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, sync::Arc};
    #[test]
    fn cross_page_random_and_concurrent_reads_match_source_bytes() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let bytes: Vec<_> = (0..PAGE * 3 + 73).map(|i| (i % 251) as u8).collect();
        file.write_all(&bytes).unwrap();
        let source = Arc::new(PagedFile::open(file.path()).unwrap());
        std::thread::scope(|scope| {
            for worker in 0..4 {
                let source = source.clone();
                let bytes = &bytes;
                scope.spawn(move || {
                    for i in 0..100 {
                        let offset = (i * 1409 + worker * 997) % (bytes.len() - 4096);
                        let mut out = vec![0; 4096];
                        source.read_exact_at(offset as u64, &mut out).unwrap();
                        assert_eq!(out, bytes[offset..offset + 4096]);
                    }
                });
            }
        });
        let mut out = vec![0; PAGE + 10];
        source.read_exact_at((PAGE - 5) as u64, &mut out).unwrap();
        assert_eq!(out, bytes[PAGE - 5..PAGE * 2 + 5]);
        assert!(source.read_exact_at(u64::MAX, &mut [0]).is_err());
        assert!(source.read_exact_at(bytes.len() as u64, &mut [0]).is_err());
        source.read_exact_at(bytes.len() as u64, &mut []).unwrap();
    }
    #[test]
    fn random_page_cache_is_bounded_and_recent_pages_do_not_reread() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&vec![9; PAGE * (MAX_PAGES + 3)]).unwrap();
        let source = PagedFile::open(file.path()).unwrap();
        for index in 0..MAX_PAGES + 3 {
            source
                .read_exact_at((index * PAGE) as u64, &mut [0])
                .unwrap();
        }
        let before = source.physical_reads.load(Ordering::Relaxed);
        source.read_exact_at((PAGE * 5) as u64, &mut [0]).unwrap();
        assert_eq!(source.physical_reads.load(Ordering::Relaxed), before);
        source.read_exact_at(0, &mut [0]).unwrap();
        assert_eq!(source.physical_reads.load(Ordering::Relaxed), before + 1);
        let state = source.state.lock().unwrap();
        assert_eq!(state.history.len() + 1, MAX_PAGES);
        assert_eq!(
            state.history.values().map(|p| p.bytes.len()).sum::<usize>()
                + state.current.bytes.len(),
            MAX_PAGES * PAGE
        );
    }
    #[test]
    fn neighboring_metadata_reads_share_one_physical_read() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&vec![7; PAGE]).unwrap();
        let source = PagedFile::open(file.path()).unwrap();
        for offset in 0..1000 {
            let mut out = [0; 4];
            source.read_exact_at(offset, &mut out).unwrap();
            assert_eq!(out, [7; 4]);
        }
        assert_eq!(source.requests.load(Ordering::Relaxed), 1000);
        assert_eq!(source.physical_reads.load(Ordering::Relaxed), 1);
        assert_eq!(source.physical_bytes.load(Ordering::Relaxed), PAGE as u64);
    }
}
