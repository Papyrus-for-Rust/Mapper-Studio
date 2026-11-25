//! Portable content envelope. Raw source is retained, never rewritten during inspection.
use papyrus_terrain::{
    content::{self, Composition, MapContent, Vector},
    content_stamp,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};
const MAGIC: &[u8; 8] = b"MAPA\x01\0\0\0";
const MAX_MANIFEST: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerrainRegion {
    pub center: [f32; 2],
    pub size: [f32; 2],
    pub fade: f32,
    pub height: bool,
    pub splat: bool,
    pub topology: bool,
    pub biome: bool,
    pub alpha: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", deny_unknown_fields)]
pub enum Binding {
    AddOn {
        anchor_index: usize,
    },
    Standalone {
        pivot: [f32; 3],
        authoring_yaw: f32,
        terrain: Option<TerrainRegion>,
    },
}
impl Binding {
    pub fn composition(&self) -> Composition {
        match self {
            Self::AddOn { anchor_index } => Composition::AddOn {
                anchor: *anchor_index,
            },
            Self::Standalone {
                pivot,
                authoring_yaw,
                ..
            } => Composition::Standalone {
                pivot: Vector {
                    x: pivot[0],
                    y: pivot[1],
                    z: pivot[2],
                },
                authoring_yaw: *authoring_yaw,
            },
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    pub name: String,
    pub source_sha256: String,
    pub binding: Binding,
}
pub struct Imported {
    pub source: Vec<u8>,
    pub map: MapContent,
    pub manifest: Manifest,
    source_origin: Option<PathBuf>,
}
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

impl Imported {
    pub(crate) fn package_digest(&self, manifest: &Manifest) -> Result<String, String> {
        let json = manifest_bytes(manifest)?;
        let mut hash = Sha256::new();
        hash.update(MAGIC);
        hash.update((json.len() as u32).to_le_bytes());
        hash.update(&json);
        hash.update(&self.source);
        Ok(format!("{:x}", hash.finalize()))
    }

    pub fn terrain_stamp(
        &self,
        binding: &Binding,
    ) -> Result<Option<content_stamp::Stamp<'_>>, String> {
        let Binding::Standalone {
            pivot,
            authoring_yaw,
            terrain: Some(region),
        } = binding
        else {
            return Ok(None);
        };
        let region = content_stamp::Region {
            center: region.center,
            size: region.size,
            fade: region.fade,
            height: region.height,
            splat: region.splat,
            biome: region.biome,
            topology_mask: if region.topology { u32::MAX } else { 0 },
            alpha: region.alpha,
        };
        content_stamp::Stamp::new(
            &self.map,
            region,
            content_stamp::Pose {
                position: *pivot,
                yaw: *authoring_yaw,
            },
        )
        .map(Some)
    }

    pub fn read_file(path: &Path, cancelled: impl Fn() -> bool) -> Result<Self, String> {
        let mut bytes = Vec::new();
        File::open(path)
            .map_err(|e| e.to_string())?
            .take((content::MAX_SOURCE + MAX_MANIFEST + 12 + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > content::MAX_SOURCE + MAX_MANIFEST + 12 {
            return Err("Content package exceeds its import budget".into());
        }
        let mut imported = Self::read_bytes(
            &bytes,
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Custom content"),
            cancelled,
        )?;
        if !bytes.starts_with(MAGIC) {
            imported.source_origin = Some(path.canonicalize().map_err(|e| e.to_string())?);
        }
        Ok(imported)
    }
    pub fn read_bytes(
        bytes: &[u8],
        name: &str,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        if cancelled() {
            return Err("Content import cancelled".into());
        }
        let (source, manifest) = if bytes.starts_with(MAGIC) {
            let size = u32::from_le_bytes(
                bytes
                    .get(8..12)
                    .ok_or("Truncated package header")?
                    .try_into()
                    .map_err(|_| "Invalid package header")?,
            ) as usize;
            if size > MAX_MANIFEST {
                return Err("Package manifest exceeds 64 KiB".into());
            }
            let metadata = bytes
                .get(12..12 + size)
                .ok_or("Truncated package manifest")?;
            let manifest: Manifest = serde_json::from_slice(metadata)
                .map_err(|e| format!("Invalid content manifest: {e}"))?;
            (
                bytes
                    .get(12 + size..)
                    .ok_or("Missing source body")?
                    .to_vec(),
                Some(manifest),
            )
        } else {
            (bytes.to_vec(), None)
        };
        if source.len() > content::MAX_SOURCE {
            return Err("Content source exceeds 64 MiB".into());
        }
        let map = content::read_map(&source, &cancelled)?;
        let source_sha256 = digest(&source);
        let manifest = manifest.unwrap_or(Manifest {
            schema: 1,
            name: name.into(),
            source_sha256,
            binding: Binding::Standalone {
                pivot: [0.; 3],
                authoring_yaw: 0.,
                terrain: None,
            },
        });
        let item = Self {
            source,
            map,
            manifest,
            source_origin: None,
        };
        item.validate(&item.manifest)?;
        Ok(item)
    }
    pub fn validate(&self, manifest: &Manifest) -> Result<(), String> {
        if manifest.schema != 1
            || manifest.name.trim().is_empty()
            || manifest.name.len() > 128
            || manifest.name.chars().any(char::is_control)
        {
            return Err("Unsupported content schema or invalid name".into());
        }
        if manifest.source_sha256 != digest(&self.source) {
            return Err("Content source checksum mismatch".into());
        }
        content::compose(
            &self.map,
            manifest.binding.composition(),
            Vector::default(),
            0.,
        )?;
        self.terrain_stamp(&manifest.binding)?;
        Ok(())
    }
    pub fn save(&self, manifest: &Manifest, path: &Path) -> Result<(), String> {
        self.validate(manifest)?;
        if let Some(origin) = &self.source_origin
            && let Ok(destination) = path.canonicalize()
            && (destination == *origin
                || cfg!(windows)
                    && destination
                        .to_string_lossy()
                        .eq_ignore_ascii_case(&origin.to_string_lossy()))
        {
            return Err(
                "Choose a package destination different from the original source map".into(),
            );
        }
        let json = manifest_bytes(manifest)?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
        file.write_all(MAGIC)
            .and_then(|_| file.write_all(&(json.len() as u32).to_le_bytes()))
            .and_then(|_| file.write_all(&json))
            .and_then(|_| file.write_all(&self.source))
            .and_then(|_| file.as_file().sync_all())
            .map_err(|e| e.to_string())?;
        file.persist(path).map_err(|e| e.error.to_string())?;
        Ok(())
    }
}

fn manifest_bytes(manifest: &Manifest) -> Result<Vec<u8>, String> {
    let json = serde_json::to_vec_pretty(manifest).map_err(|e| e.to_string())?;
    if json.len() > MAX_MANIFEST {
        return Err("Manifest exceeds 64 KiB".into());
    }
    Ok(json)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source() -> Vec<u8> {
        // Minimal valid raw legacy block: size=1000, no objects.
        let mut map = 10_u32.to_le_bytes().to_vec();
        map.extend(0_i64.to_le_bytes());
        map.extend([0, 3, 8, 232, 7]);
        map
    }
    #[test]
    fn bundle_roundtrip_retains_original_source_and_checksums() {
        let bytes = source();
        let item = Imported::read_bytes(&bytes, "Standalone", || false).unwrap();
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join("sample.mapperasset");
        item.save(&item.manifest, &file).unwrap();
        let read = Imported::read_file(&file, || false).unwrap();
        assert_eq!(read.source, bytes);
        assert_eq!(read.manifest, item.manifest);
        let old = std::fs::read(&file).unwrap();
        let mut bad = item.manifest.clone();
        bad.source_sha256 = "wrong".into();
        assert!(item.save(&bad, &file).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), old);
    }
    #[test]
    fn no_flat_canvas_stamp_or_anchor_is_assumed() {
        let item = Imported::read_bytes(&source(), "Empty", || false).unwrap();
        assert!(matches!(
            item.manifest.binding,
            Binding::Standalone { terrain: None, .. }
        ));
        let mut manifest = item.manifest.clone();
        manifest.binding = Binding::AddOn { anchor_index: 0 };
        assert!(item.validate(&manifest).is_err());
        manifest.binding = Binding::Standalone {
            pivot: [0.; 3],
            authoring_yaw: 0.,
            terrain: Some(TerrainRegion {
                center: [0.; 2],
                size: [2000.; 2],
                fade: 0.,
                height: true,
                splat: false,
                topology: false,
                biome: false,
                alpha: false,
            }),
        };
        assert!(item.validate(&manifest).is_err());
    }
    #[test]
    fn package_save_rejects_missing_selected_alpha_before_publication() {
        let item = Imported::read_bytes(&source(), "Cutout", || false).unwrap();
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("cutout.mapperasset");
        item.save(&item.manifest, &path).unwrap();
        let original = std::fs::read(&path).unwrap();
        let mut manifest = item.manifest.clone();
        manifest.binding = Binding::Standalone {
            pivot: [0.; 3],
            authoring_yaw: 37.,
            terrain: Some(TerrainRegion {
                center: [0.; 2],
                size: [100.; 2],
                fade: 5.,
                height: false,
                splat: false,
                topology: false,
                biome: false,
                alpha: true,
            }),
        };
        assert!(
            item.save(&manifest, &path)
                .unwrap_err()
                .contains("missing its alpha layer")
        );
        assert_eq!(std::fs::read(path).unwrap(), original);
    }
    #[test]
    fn saving_a_package_cannot_overwrite_the_imported_map() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("authored.map");
        let bytes = source();
        std::fs::write(&path, &bytes).unwrap();
        let item = Imported::read_file(&path, || false).unwrap();
        assert!(item.save(&item.manifest, &path).is_err());
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
}
