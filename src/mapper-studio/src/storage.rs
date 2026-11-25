//! Canonical Mapper data paths and one-time cache-layout migration.
use std::{
    fs,
    path::{Path, PathBuf},
};

pub fn under(local: &Path) -> PathBuf {
    local.join("Mapper")
}

/// Identify earlier data by its cache layout, independent of its folder name.
/// A sibling rename retains content-addressed files without copying their data.
pub fn prepare(local: &Path) -> Result<PathBuf, String> {
    let destination = under(local);
    if crate::client_cache::ordinary(&destination)? || !local.exists() {
        return Ok(destination);
    }
    let parent = local.canonicalize().map_err(|e| e.to_string())?;
    let mut previous = None;
    for (count, entry) in fs::read_dir(&parent)
        .map_err(|e| e.to_string())?
        .enumerate()
    {
        if count >= 4096 {
            return Err("Mapper data migration exceeded its folder search limit".into());
        }
        let path = entry.map_err(|e| e.to_string())?.path();
        if !crate::client_cache::ordinary(&path).unwrap_or(false)
            || ![
                "papyrus-prefab-cache",
                "papyrus-texture-cache",
                "papyrus-terrain-cache",
            ]
            .iter()
            .all(|name| crate::client_cache::ordinary(&path.join(name)).unwrap_or(false))
        {
            continue;
        }
        if previous.replace(path).is_some() {
            return Err("Several previous client-data folders were found; migration requires a single source".into());
        }
    }
    let Some(previous) = previous else {
        return Ok(destination);
    };
    let previous = previous.canonicalize().map_err(|e| e.to_string())?;
    let destination = parent.join("Mapper");
    if previous.parent() != Some(parent.as_path()) || destination.parent() != Some(parent.as_path())
    {
        return Err("Mapper data migration escaped its application-data directory".into());
    }
    if let Err(error) = fs::rename(previous, &destination)
        && !crate::client_cache::ordinary(&destination)?
    {
        return Err(format!(
            "Could not move existing client data into Mapper: {error}"
        ));
    }
    Ok(under(local))
}

pub fn local_data() -> Result<PathBuf, String> {
    let local =
        std::env::var_os("LOCALAPPDATA").ok_or("Local Mapper data directory unavailable")?;
    prepare(Path::new(&local))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cache(root: &Path) {
        for name in [
            "papyrus-prefab-cache",
            "papyrus-texture-cache",
            "papyrus-terrain-cache",
        ] {
            fs::create_dir_all(root.join(name)).unwrap();
        }
        fs::write(
            root.join("papyrus-prefab-cache/receipt.json"),
            b"verified source",
        )
        .unwrap();
    }
    #[test]
    fn data_moves_to_mapper_without_rebuilding_or_overwriting_existing_data() {
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("previous-installation");
        cache(&old);
        let destination = prepare(temp.path()).unwrap();
        assert_eq!(destination, under(temp.path()));
        assert!(!old.exists());
        assert_eq!(
            fs::read(destination.join("papyrus-prefab-cache/receipt.json")).unwrap(),
            b"verified source"
        );
        cache(&old);
        assert_eq!(prepare(temp.path()).unwrap(), destination);
        assert!(old.is_dir());
        assert_eq!(
            fs::read(destination.join("papyrus-prefab-cache/receipt.json")).unwrap(),
            b"verified source"
        );
    }
    #[test]
    fn migration_requires_a_unique_complete_cache_layout() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("unrelated")).unwrap();
        assert_eq!(prepare(temp.path()).unwrap(), under(temp.path()));
        assert!(!under(temp.path()).exists());
        cache(&temp.path().join("first"));
        cache(&temp.path().join("second"));
        assert!(prepare(temp.path()).is_err());
        assert!(temp.path().join("first").is_dir());
        assert!(temp.path().join("second").is_dir());
        assert!(!under(temp.path()).exists());
    }
}
