//! Original RustEdit 1 custom prefab and its adjacent terrain images.
use papyrus_terrain::content::{self, RustEditPrefab};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
};

pub struct TerrainImage {
    pub suffix: &'static str,
    pub dimensions: [u32; 2],
    pub bytes: Vec<u8>,
}

pub struct Source {
    pub bytes: Vec<u8>,
    pub prefab: RustEditPrefab,
    pub terrain: Vec<TerrainImage>,
}

pub const PREVIEW_SIDE: usize = 224;
pub type SplatPreview = [Option<Vec<[u8; 4]>>; 2];

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(String);

impl Version {
    pub fn parse(key: &str) -> Result<Self, String> {
        if key.len() != 64
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("Invalid custom monument version".into());
        }
        Ok(Self(key.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LibraryEntry {
    pub name: String,
    pub version: Version,
}

impl LibraryEntry {
    pub fn read_for_preview(
        &self,
        cancelled: impl Fn() -> bool,
    ) -> Result<(Source, SplatPreview), String> {
        let (name, source, splats) = self
            .version
            .read_saved(&library_path()?, true, &cancelled)?;
        if name != self.name {
            return Err(
                "Saved custom monument name changed. Reopen the library to refresh it.".into(),
            );
        }
        Ok((source, splats))
    }
}

impl Version {
    fn read_saved(
        &self,
        library: &Path,
        preview: bool,
        cancelled: &impl Fn() -> bool,
    ) -> Result<(String, Source, SplatPreview), String> {
        if cancelled() {
            return Err("Content import cancelled".into());
        }
        let directory = library.join(self.as_str());
        let result = (|| {
            let name = read_name(&directory)?;
            let mut splats = [None, None];
            let source = Source::read_inner(
                &directory.join("monument.prefab"),
                preview.then_some(&mut splats),
                cancelled,
            )?;
            if source.version(&name, cancelled)? != *self {
                return Err(
                    "Prefab, terrain images or name no longer match the saved version".into(),
                );
            }
            Ok((name, source, splats))
        })();
        result.map_err(|error: String| {
            if cancelled() {
                error
            } else {
                format!("Custom monument {} is unavailable. {error}. Import the original files under a new name.", &self.0[..8])
            }
        })
    }
}

pub fn read_version(version: &Version, cancelled: impl Fn() -> bool) -> Result<Source, String> {
    version
        .read_saved(&library_path()?, false, &cancelled)
        .map(|(_, source, _)| source)
}

fn library_path() -> Result<PathBuf, String> {
    let settings = crate::preferences::Preferences::path()?;
    let folder = settings
        .parent()
        .ok_or("Mapper settings folder is unavailable")?;
    Ok(folder.join("custom-monuments"))
}

pub fn library_entries(cancelled: impl Fn() -> bool) -> Result<Vec<LibraryEntry>, String> {
    let entries = match fs::read_dir(library_path()?) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let mut result = Vec::new();
    for entry in entries {
        if cancelled() {
            return Err("Monument import cancelled".into());
        }
        let entry = entry.map_err(|error| error.to_string())?;
        let file_name = entry.file_name();
        let Some(version) = file_name.to_str().and_then(|key| Version::parse(key).ok()) else {
            continue;
        };
        let directory = entry.path();
        if let Ok(name) = read_name(&directory) {
            result.push(LibraryEntry { name, version });
        }
    }
    result.sort_unstable();
    Ok(result)
}

fn valid_name(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
}

fn read_name(directory: &Path) -> Result<String, String> {
    let mut name = String::new();
    File::open(directory.join("name.txt"))
        .and_then(|file| file.take(129).read_to_string(&mut name))
        .map_err(|error| error.to_string())?;
    if !valid_name(&name) {
        return Err("Invalid saved custom monument name".into());
    }
    Ok(name)
}

impl Source {
    pub fn save_to_library(
        &self,
        name: &str,
        cancelled: impl Fn() -> bool,
    ) -> Result<LibraryEntry, String> {
        self.store(&library_path()?, name, cancelled)
    }

    fn store(
        &self,
        library: &Path,
        name: &str,
        cancelled: impl Fn() -> bool,
    ) -> Result<LibraryEntry, String> {
        let name = name.trim();
        if !valid_name(name) {
            return Err("Enter a monument name up to 128 bytes.".into());
        }
        let version = self.version(name, &cancelled)?;
        let destination = library.join(version.as_str());
        let entry = LibraryEntry {
            name: name.to_owned(),
            version,
        };
        if cancelled() {
            return Err("Monument import cancelled".into());
        }
        if destination.exists() {
            entry.version.read_saved(library, false, &cancelled)?;
            return Ok(entry);
        }
        fs::create_dir_all(library).map_err(|error| error.to_string())?;
        let stage = tempfile::tempdir_in(library).map_err(|error| error.to_string())?;
        let write = |relative: &str, bytes: &[u8]| -> Result<(), String> {
            if cancelled() {
                return Err("Monument import cancelled".into());
            }
            let mut file =
                File::create_new(stage.path().join(relative)).map_err(|error| error.to_string())?;
            file.write_all(bytes)
                .and_then(|()| file.sync_all())
                .map_err(|error| error.to_string())
        };
        write("name.txt", name.as_bytes())?;
        write("monument.prefab", &self.bytes)?;
        for image in &self.terrain {
            write(&format!("monument.prefab.{}", image.suffix), &image.bytes)?;
        }
        if cancelled() {
            return Err("Monument import cancelled".into());
        }
        match fs::rename(stage.path(), &destination) {
            Ok(()) => Ok(entry),
            Err(_) if destination.exists() => {
                entry.version.read_saved(library, false, &cancelled)?;
                Ok(entry)
            }
            Err(error) => Err(error.to_string()),
        }
    }

    fn version(&self, name: &str, cancelled: &impl Fn() -> bool) -> Result<Version, String> {
        let mut hash = Sha256::new();
        let mut add = |bytes: &[u8]| {
            hash.update((bytes.len() as u64).to_le_bytes());
            for chunk in bytes.chunks(1024 * 1024) {
                if cancelled() {
                    return Err("Content import cancelled".to_owned());
                }
                hash.update(chunk);
            }
            Ok(())
        };
        add(name.as_bytes())?;
        add(&self.bytes)?;
        for image in &self.terrain {
            add(image.suffix.as_bytes())?;
            add(&image.bytes)?;
        }
        Ok(Version(format!("{:x}", hash.finalize())))
    }

    pub fn read(path: &Path, cancelled: impl Fn() -> bool) -> Result<Self, String> {
        Self::read_inner(path, None, &cancelled)
    }

    pub fn read_for_preview(
        path: &Path,
        cancelled: impl Fn() -> bool,
    ) -> Result<(Self, SplatPreview), String> {
        let mut splats = [None, None];
        let source = Self::read_inner(path, Some(&mut splats), &cancelled)?;
        Ok((source, splats))
    }

    fn read_inner(
        path: &Path,
        mut preview: Option<&mut SplatPreview>,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Self, String> {
        if cancelled() {
            return Err("Content import cancelled".into());
        }
        let bytes = content::read_source(File::open(path).map_err(|error| error.to_string())?)?;
        let prefab = content::read_rustedit_prefab(&bytes, cancelled)?;
        let mut remaining = content::MAX_SOURCE - bytes.len();
        let mut terrain = Vec::new();
        for suffix in [
            "heights", "splat0", "splat1", "biome", "biomes", "topology", "alpha",
        ] {
            if cancelled() {
                return Err("Content import cancelled".into());
            }
            let mut name = path.as_os_str().to_os_string();
            name.push(".");
            name.push(suffix);
            let file = match File::open(&name) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("Cannot read {suffix} terrain image. {error}")),
            };
            let mut image = Vec::new();
            file.take(remaining as u64 + 1)
                .read_to_end(&mut image)
                .map_err(|error| format!("Cannot read {suffix} terrain image. {error}"))?;
            if image.len() > remaining {
                return Err("RustEdit prefab and its terrain images exceed 64 MiB".into());
            }
            let splat = match suffix {
                "splat0" => preview.as_mut().map(|layers| &mut layers[0]),
                "splat1" => preview.as_mut().map(|layers| &mut layers[1]),
                _ => None,
            };
            let dimensions = inspect_png(&image, splat, cancelled)
                .map_err(|error| format!("Invalid RustEdit {suffix} terrain image. {error}"))?;
            remaining -= image.len();
            terrain.push(TerrainImage {
                suffix,
                dimensions,
                bytes: image,
            });
        }
        Ok(Self {
            bytes,
            prefab,
            terrain,
        })
    }
}

fn inspect_png(
    bytes: &[u8],
    preview: Option<&mut Option<Vec<[u8; 4]>>>,
    cancelled: &impl Fn() -> bool,
) -> Result<[u32; 2], String> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    if preview.is_some() {
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    }
    decoder.set_limits(png::Limits {
        bytes: 72 * 1024 * 1024,
    });
    let mut reader = decoder.read_info().map_err(|error| error.to_string())?;
    let dimensions = [reader.info().width, reader.info().height];
    if dimensions.iter().any(|&side| !(1..=4097).contains(&side))
        || reader.info().animation_control.is_some()
    {
        return Err("Unsupported terrain image dimensions or animation".into());
    }
    if let Some(preview) = preview {
        *preview = Some(sample_splat(&mut reader, dimensions, cancelled)?);
        reader.finish().map_err(|error| error.to_string())?;
        return Ok(dimensions);
    }
    while !cancelled() {
        if reader
            .next_interlaced_row()
            .map_err(|error| error.to_string())?
            .is_none()
        {
            reader.finish().map_err(|error| error.to_string())?;
            return Ok(dimensions);
        }
    }
    Err("Content import cancelled".into())
}

fn sample_splat(
    reader: &mut png::Reader<Cursor<&[u8]>>,
    dimensions: [u32; 2],
    cancelled: &impl Fn() -> bool,
) -> Result<Vec<[u8; 4]>, String> {
    let [width, height] = dimensions.map(|side| side as usize);
    let samples = reader.output_color_type().0.samples();
    let mut pixels = vec![[0; 4]; PREVIEW_SIDE * PREVIEW_SIDE];
    let columns: [usize; PREVIEW_SIDE] =
        std::array::from_fn(|x| x * width / PREVIEW_SIDE * samples);
    let copy_row = |target: &mut [[u8; 4]], row: &[u8]| {
        for (pixel, at) in target.iter_mut().zip(columns) {
            pixel[..samples].copy_from_slice(&row[at..at + samples]);
        }
    };
    if reader.info().interlaced {
        // Adam7 rows are sparse. Let png reconstruct the frame for this
        // uncommon layout, with the same allocation bound as the import.
        let size = reader
            .output_buffer_size()
            .filter(|&size| size <= 4097 * 4097 * 4)
            .ok_or("Splat image exceeds its preview budget")?;
        let mut decoded = vec![0; size];
        let info = reader
            .next_frame(&mut decoded)
            .map_err(|error| error.to_string())?;
        for (y, target) in pixels.chunks_exact_mut(PREVIEW_SIDE).enumerate() {
            if cancelled() {
                return Err("Content import cancelled".into());
            }
            let start = y * height / PREVIEW_SIDE * info.line_size;
            copy_row(target, &decoded[start..start + info.line_size]);
        }
    } else {
        let mut target_y = 0;
        for y in 0..height {
            if cancelled() {
                return Err("Content import cancelled".into());
            }
            let row = reader
                .next_row()
                .map_err(|error| error.to_string())?
                .ok_or("Missing splat image row")?;
            while target_y < PREVIEW_SIDE && target_y * height / PREVIEW_SIDE == y {
                let start = target_y * PREVIEW_SIDE;
                copy_row(&mut pixels[start..start + PREVIEW_SIDE], row.data());
                target_y += 1;
            }
        }
    }
    Ok(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_prefab_with_its_images_and_rejects_truncated_images() {
        let temp = tempfile::tempdir().unwrap();
        let prefab = temp.path().join("example.prefab");
        // Version, raw block framing, terrain descriptor, and one prefab record.
        let bytes = [1, 0, 0, 0, 0, 8, 10, 2, 8, 80, 26, 2, 16, 42];
        std::fs::write(&prefab, bytes).unwrap();
        let mut png = Vec::new();
        png::Encoder::new(&mut png, 2, 2)
            .write_header()
            .unwrap()
            .write_image_data(&[1, 2, 3, 4])
            .unwrap();
        let alpha = temp.path().join("example.prefab.alpha");
        std::fs::write(&alpha, &png).unwrap();
        std::fs::write(temp.path().join("other.prefab.heights"), b"unrelated").unwrap();
        let source = Source::read(&prefab, || false).unwrap();
        assert_eq!(source.bytes, bytes);
        assert_eq!(source.prefab.prefabs[0].id, 42);
        assert_eq!(source.prefab.terrain_descriptor, [8, 80]);
        assert_eq!(source.terrain.len(), 1);
        assert_eq!(source.terrain[0].suffix, "alpha");
        assert_eq!(source.terrain[0].dimensions, [2, 2]);
        assert_eq!(source.terrain[0].bytes, png);
        let library = temp.path().join("library");
        let entry = source.store(&library, "Bunker", || false).unwrap();
        assert_eq!(entry.name, "Bunker");
        assert_eq!(
            source.store(&library, "Bunker", || false).unwrap().version,
            entry.version
        );
        assert!(source.store(&library, "Cancelled", || true).is_err());
        assert_eq!(fs::read_dir(&library).unwrap().count(), 1);
        let (_, restored, _) = entry
            .version
            .read_saved(&library, false, &|| false)
            .unwrap();
        assert_eq!(restored.bytes, bytes);
        assert_eq!(restored.terrain[0].bytes, png);
        let saved = library
            .join(entry.version.as_str())
            .join("monument.prefab.alpha");
        fs::remove_file(&saved).unwrap();
        assert!(
            entry
                .version
                .read_saved(&library, true, &|| false)
                .err()
                .unwrap()
                .contains("no longer match")
        );
        assert!(source.store(&library, "Bunker", || false).is_err());
        assert!(Version::parse("../another").is_err());
        std::fs::write(&alpha, &png[..png.len() - 16]).unwrap();
        assert!(
            Source::read(&prefab, || false)
                .err()
                .unwrap()
                .contains("alpha")
        );
    }
}
