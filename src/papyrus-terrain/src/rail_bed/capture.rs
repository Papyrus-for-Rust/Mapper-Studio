//! Explicit diagnostic capture of immutable failed bed inputs.
use super::{Layers, Policy, Settings, Shape, Track};
use crate::{path_geometry::Path, rail_bed_fit::Join};
use serde::Serialize;
use std::{
    fs,
    io::{self, BufWriter, Write},
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

#[derive(Serialize)]
struct Base<'a> {
    size: u32,
    terrain_seed: u32,
    biome_seed: u32,
    height_resolution: usize,
    surface_resolution: usize,
    height: &'a [i16],
    biome: &'a [u8],
    splat: &'a [u8],
    topology: &'a [i32],
}
#[derive(Serialize)]
struct SavedTrack<'a> {
    id: u64,
    preserve_start: bool,
    path: &'a Path,
}
#[derive(Serialize)]
struct Input<'a> {
    format: &'static str,
    base: Base<'a>,
    alpha: &'a [u8],
    water: Option<&'a [f32]>,
    tracks: Vec<SavedTrack<'a>>,
    joins: &'a [Join],
    policy: &'a Policy,
    shape: Shape,
    settings: Settings,
    defer_surfaces: bool,
}
struct Cancellable<W, F> {
    writer: W,
    cancelled: F,
}
impl<W: Write, F: Fn() -> bool> Write for Cancellable<W, F> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if (self.cancelled)() {
            return Err(io::Error::other("Rail capture cancelled"));
        }
        self.writer.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn if_requested(
    base: &Layers,
    alpha: &[u8],
    water: Option<&[f32]>,
    tracks: &[Track<'_>],
    joins: &[Join],
    policy: &Policy,
    shape: Shape,
    settings: Settings,
    defer_surfaces: bool,
    cancelled: &impl Fn() -> bool,
) -> Result<(), String> {
    let Some(root) = std::env::var_os("MAPPER_RAIL_BED_FAILURE_CAPTURES") else {
        return Ok(());
    };
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = PathBuf::from(root);
    if !root.is_dir() || cancelled() {
        return Err("Rail capture destination unavailable or cancelled".into());
    }
    let name = format!(
        "rail-bed-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let partial = root.join(format!("{name}.partial"));
    let destination = root.join(format!("{name}.json"));
    if destination.try_exists().map_err(|e| e.to_string())? {
        return Err("Rail capture already exists".into());
    }
    let input = Input {
        format: "mapper-rail-bed-replay-1",
        base: Base {
            size: base.size,
            terrain_seed: base.terrain_seed,
            biome_seed: base.biome_seed,
            height_resolution: base.height_resolution,
            surface_resolution: base.surface_resolution,
            height: &base.height,
            biome: &base.biome,
            splat: &base.splat,
            topology: &base.topology,
        },
        alpha,
        water,
        tracks: tracks
            .iter()
            .map(|t| SavedTrack {
                id: t.id,
                preserve_start: t.preserve_start,
                path: t.path,
            })
            .collect(),
        joins,
        policy,
        shape,
        settings,
        defer_surfaces,
    };
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)
        .map_err(|e| e.to_string())?;
    let result = (|| -> Result<(), String> {
        let mut writer = BufWriter::new(Cancellable {
            writer: &file,
            cancelled,
        });
        serde_json::to_writer(&mut writer, &input).map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(writer);
        drop(file);
        if cancelled() {
            return Err("Rail capture cancelled".into());
        }
        fs::rename(&partial, &destination).map_err(|e| e.to_string())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&partial);
    }
    result?;
    eprintln!("Captured rail bed inputs at {}", destination.display());
    Ok(())
}
