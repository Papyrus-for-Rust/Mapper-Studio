use super::{TerrainPayload, read_terrain};
use std::{
    fs::OpenOptions,
    io::ErrorKind,
    os::windows::fs::OpenOptionsExt,
    path::Path,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{Builder, JoinHandle},
};

pub(super) struct Pipe {
    name: String,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<Result<TerrainPayload, String>>>,
}

impl Pipe {
    pub(super) fn configure(command: &mut Command, temp: &Path) -> Option<Self> {
        let suffix = temp.file_name()?.to_str()?;
        let name = format!("papyrus-terrain-{}-{suffix}", std::process::id());
        command.env("PAPYRUS_NATIVE_TERRAIN_PIPE", &name);
        Some(Self {
            name,
            stop: Arc::new(AtomicBool::new(false)),
            reader: None,
        })
    }

    pub(super) fn try_attach(&mut self, expected_bytes: usize) -> Result<bool, String> {
        if self.reader.is_some() {
            return Ok(false);
        }
        let source = match OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(format!(r"\\.\pipe\{}", self.name))
        {
            Ok(source) => source,
            // The writer may still be creating the pipe, or this may be an
            // older worker that only produces terrain.bin.
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(format!("Cannot open native terrain pipe: {error}")),
        };
        let stop = self.stop.clone();
        self.reader = Some(
            Builder::new()
                .name("mapper-native-terrain".into())
                .spawn(move || read_terrain(source, expected_bytes, &stop))
                .map_err(|error| format!("Cannot start native terrain reader: {error}"))?,
        );
        Ok(true)
    }

    pub(super) fn finish(&mut self) -> Result<Option<TerrainPayload>, String> {
        self.reader
            .take()
            .map(|reader| {
                reader
                    .join()
                    .map_err(|_| "Native terrain reader stopped unexpectedly")?
            })
            .transpose()
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.finish();
    }
}
