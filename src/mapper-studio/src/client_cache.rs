//! Reversible reset of shared extracted client data. Terrain and user files stay put.
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
pub const DIRECTORIES: &[&str] = &[
    "papyrus-prefab-cache",
    "papyrus-prefab-identity-cache",
    "papyrus-texture-cache",
    "papyrus-monument-cache",
    "papyrus-monument-stage-cache",
    "papyrus-dungeon-cache",
    "papyrus-tunnel-cache",
    "papyrus-offshore-stage-cache",
    "papyrus-roadside-stage-cache",
    "papyrus-infrastructure-cache",
    "papyrus-road-layout-cache",
    "papyrus-road-mesh-cache",
    "papyrus-rail-cache",
    "papyrus-rail-mesh-cache",
    "papyrus-decor-cache",
    "papyrus-cliff-cache",
    "papyrus-vine-cache",
    "papyrus-coastal-cliff-cache",
    "papyrus-water-treatment-cache",
    "papyrus-powerline-cache",
    "papyrus-scene-wire-cache",
    "papyrus-monument-tag-cache",
    "papyrus-decor-model-cache",
    "papyrus-spawn-cache",
    "papyrus-road-object-cache",
    "papyrus-river-cache",
    "papyrus-river-object-cache",
    "papyrus-map-label-cache",
    "papyrus-terrain-palette-cache",
    "papyrus-water-collision-cache",
];
fn reparse(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    metadata.file_type().is_symlink()
}
pub(crate) fn ordinary(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() && !reparse(&m) => Ok(true),
        Ok(_) => Err("Client cache path is not an ordinary directory".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.to_string()),
    }
}
pub struct Reset {
    pub directories: usize,
    pub archive: Option<PathBuf>,
}
fn clear_prepared_sources() {
    crate::tunnel_source::clear_session();
    crate::monument_tag_source::clear_session();
    crate::decor_source::clear_session();
}
pub fn reset_in(local: &Path) -> Result<Reset, String> {
    if !ordinary(local)? {
        clear_prepared_sources();
        return Ok(Reset {
            directories: 0,
            archive: None,
        });
    }
    let local = local.canonicalize().map_err(|e| e.to_string())?;
    let mut paths = Vec::new();
    for name in DIRECTORIES {
        let path = local.join(name);
        if ordinary(&path)? {
            if path.canonicalize().map_err(|e| e.to_string())?.parent() != Some(local.as_path()) {
                return Err("Cache reset escaped its data directory".into());
            }
            paths.push(path);
        }
    }
    if paths.is_empty() {
        clear_prepared_sources();
        return Ok(Reset {
            directories: 0,
            archive: None,
        });
    }
    let archives = local.join("client-cache-archive");
    if !ordinary(&archives)? {
        fs::create_dir(&archives).map_err(|e| e.to_string())?;
    }
    if archives.canonicalize().map_err(|e| e.to_string())?.parent() != Some(local.as_path()) {
        return Err("Invalid reset archive location".into());
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let destination = archives.join(format!("{stamp:032}-{}", std::process::id()));
    fs::create_dir(&destination).map_err(|e| e.to_string())?;
    // Cache verification can leave Windows change notifications active after
    // its file leases end. Release those watches before moving the directories.
    #[cfg(windows)]
    crate::source_lease::release_directory_watches(&local);
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    for path in paths {
        let target = destination.join(path.file_name().ok_or("Missing cache directory name")?);
        if let Err(error) = fs::rename(&path, &target) {
            for (from, to) in moved.iter().rev() {
                if fs::rename(to, from).is_err() {
                    return Err(format!(
                        "Cache reset interrupted; retained data is in {}",
                        destination.display()
                    ));
                }
            }
            return Err(format!("Client cache is in use: {error}"));
        }
        moved.push((path, target));
    }
    clear_prepared_sources();
    Ok(Reset {
        directories: moved.len(),
        archive: Some(destination),
    })
}
pub fn archived_roots(local: &Path) -> Vec<PathBuf> {
    let root = local.join("client-cache-archive");
    if ordinary(&root) != Ok(true) {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut paths: Vec<_> = entries
        .take(128)
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| ordinary(p) == Ok(true))
        .collect();
    paths.sort();
    paths.reverse();
    paths.truncate(16);
    paths
}
/// Resolve an exact saved snapshot after a reversible client-cache reset.
/// Loaders still validate its content digest and installation receipts.
pub(crate) fn pinned_document(
    local: &Path,
    directory: &str,
    digest: &str,
    filename: &str,
) -> Result<PathBuf, String> {
    Ok(find_pinned_document(local, directory, digest, filename)?
        .unwrap_or_else(|| local.join(directory).join(digest).join(filename)))
}

pub(crate) fn require_pinned_document(
    local: &Path,
    directory: &str,
    digest: &str,
    filename: &str,
    label: &str,
) -> Result<PathBuf, String> {
    find_pinned_document(local, directory, digest, filename)?.ok_or_else(|| {
        format!("Missing {label} data for this recipe. Restore its original Mapper cache.\nSnapshot {digest}")
    })
}

fn find_pinned_document(
    local: &Path,
    directory: &str,
    digest: &str,
    filename: &str,
) -> Result<Option<PathBuf>, String> {
    if !DIRECTORIES.contains(&directory)
        || digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || filename.is_empty()
        || filename.len() > 128
        || !filename
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        || matches!(filename, "." | "..")
    {
        return Err("Invalid pinned client-cache document".into());
    }
    let current = local.join(directory).join(digest).join(filename);
    if current.try_exists().map_err(|e| e.to_string())? {
        return Ok(Some(current));
    }
    for archive in archived_roots(local) {
        let path = archive.join(directory).join(digest).join(filename);
        if path.try_exists().map_err(|e| e.to_string())? {
            return Ok(Some(path));
        }
    }
    Ok(None)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reset_moves_only_extracted_caches_and_preserves_terrain_recipes_icons_and_versions() {
        let d = tempfile::tempdir().unwrap();
        for name in [
            "papyrus-prefab-cache",
            "papyrus-texture-cache",
            "papyrus-tunnel-cache",
            "papyrus-terrain-cache",
            "tools",
            "profiles",
            "monument-icons",
            "saved-projects",
        ] {
            fs::create_dir(d.path().join(name)).unwrap();
            fs::write(d.path().join(name).join("keep"), name).unwrap();
        }
        let reset = reset_in(d.path()).unwrap();
        assert_eq!(reset.directories, 3);
        let archive = reset.archive.unwrap();
        for name in [
            "papyrus-prefab-cache",
            "papyrus-texture-cache",
            "papyrus-tunnel-cache",
        ] {
            assert!(!d.path().join(name).exists());
            assert_eq!(
                fs::read_to_string(archive.join(name).join("keep")).unwrap(),
                name
            );
        }
        for name in [
            "papyrus-terrain-cache",
            "tools",
            "profiles",
            "monument-icons",
            "saved-projects",
        ] {
            assert_eq!(
                fs::read_to_string(d.path().join(name).join("keep")).unwrap(),
                name
            );
        }
        assert_eq!(reset_in(d.path()).unwrap().directories, 0);
        assert_eq!(
            archived_roots(d.path())
                .into_iter()
                .map(|p| p.canonicalize().unwrap())
                .collect::<Vec<_>>(),
            vec![archive]
        );
    }
    #[test]
    fn reset_retains_exact_catalog_lookup_for_saved_recipe_pins() {
        use sha2::{Digest, Sha256};
        let d = tempfile::tempdir().unwrap();
        let cache = d.path().join("papyrus-prefab-cache");
        let snapshot = cache.join("snapshot");
        fs::create_dir_all(&snapshot).unwrap();
        let raw = b"{\"format\":\"test\"}";
        fs::write(snapshot.join("catalog.json"), raw).unwrap();
        let pin = format!("{:x}", Sha256::digest(raw));
        assert!(
            crate::asset_discovery::catalog_path(&cache, &pin)
                .unwrap()
                .is_file()
        );
        reset_in(d.path()).unwrap();
        let retained = crate::asset_discovery::catalog_path(&cache, &pin).unwrap();
        assert_eq!(fs::read(retained).unwrap(), raw);
        assert!(crate::asset_discovery::catalog_path(&cache, &"0".repeat(64)).is_err());
    }
    #[test]
    fn pinned_documents_survive_reset_and_prefer_current_exact_snapshots() {
        let local = tempfile::tempdir().unwrap();
        let digest = "a".repeat(64);
        for family in DIRECTORIES {
            let path = local.path().join(family).join(&digest);
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join("source.json"), family).unwrap();
        }
        let reset = reset_in(local.path()).unwrap();
        assert_eq!(reset.directories, DIRECTORIES.len());
        let archive = reset.archive.unwrap();
        for family in DIRECTORIES {
            let path = pinned_document(local.path(), family, &digest, "source.json").unwrap();
            assert!(path.canonicalize().unwrap().starts_with(&archive));
            assert_eq!(
                require_pinned_document(local.path(), family, &digest, "source.json", "test").unwrap(),
                path
            );
            assert_eq!(fs::read_to_string(path).unwrap(), *family);
        }
        let active = local
            .path()
            .join(DIRECTORIES[0])
            .join(&digest)
            .join("source.json");
        fs::create_dir_all(active.parent().unwrap()).unwrap();
        fs::write(&active, "current").unwrap();
        assert_eq!(
            pinned_document(local.path(), DIRECTORIES[0], &digest, "source.json").unwrap(),
            active
        );
        let missing =
            pinned_document(local.path(), DIRECTORIES[0], &"b".repeat(64), "source.json").unwrap();
        assert!(!missing.exists());
        assert!(!missing.starts_with(archive));
        let expected = "b".repeat(64);
        let error = require_pinned_document(
            local.path(), DIRECTORIES[0], &expected, "source.json", "monument placement",
        ).unwrap_err();
        assert!(error.starts_with("Missing monument placement data for this recipe."));
        assert!(error.ends_with(&expected));
        for (family, id, file) in [
            ("../saved-projects", digest.as_str(), "source.json"),
            (DIRECTORIES[0], "../bad", "source.json"),
            (DIRECTORIES[0], digest.as_str(), "../settings.json"),
            (DIRECTORIES[0], digest.as_str(), ".."),
            (DIRECTORIES[0], digest.as_str(), ""),
        ] {
            assert!(pinned_document(local.path(), family, id, file).is_err());
        }
    }
    #[test]
    fn unexpected_cache_entry_is_rejected_before_moving_any_data() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join(DIRECTORIES[0])).unwrap();
        fs::write(d.path().join(DIRECTORIES[1]), "file").unwrap();
        assert!(reset_in(d.path()).is_err());
        assert!(d.path().join(DIRECTORIES[0]).is_dir());
        assert!(!d.path().join("client-cache-archive").exists());
    }
}
