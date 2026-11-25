//! Cancellable installation palette preparation using embedded parsers.
use crate::{
    installation::Installation,
    metadata_runtime::{self, Script},
    terrain_palette::Palette,
};
use std::path::Path;
pub fn prepare(
    root: &Path,
    expected: &Installation,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Palette>, String> {
    let cache = crate::storage::local_data()?.join("papyrus-terrain-palette-cache");
    prepare_in(&cache, root, expected, cancelled)
}
pub fn prepare_in(
    cache: &Path,
    root: &Path,
    expected: &Installation,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Palette>, String> {
    let _verification = crate::installation::VerificationScope::default();
    if let Some(palette) = Palette::discover_in(cache, root, expected, &cancelled)?
        && palette
            .defaults()
            .is_some_and(|m| m.shader_blob_sha256.is_some())
    {
        return Ok(Some(palette));
    }
    if cancelled() {
        return Ok(None);
    }
    std::fs::create_dir_all(cache).map_err(|e| e.to_string())?;
    let temporary = tempfile::Builder::new()
        .prefix(".palette-prepare-")
        .tempdir_in(cache)
        .map_err(|e| e.to_string())?;
    let output = temporary.path().join("output");
    if !metadata_runtime::run(
        Script::Palette,
        &[
            "--rust-root".into(),
            root.as_os_str().to_owned(),
            "--cache-root".into(),
            output.as_os_str().to_owned(),
        ],
        &cancelled,
    )? {
        return Ok(None);
    }
    // Ignore child-provided paths. Verify the owned output directory and actual
    // game receipts before publishing a complete, content-addressed cache entry.
    let palette = Palette::discover_in(&output, root, expected, &cancelled)?
        .ok_or("Terrain color preparation produced no matching palette")?;
    if cancelled() {
        return Ok(None);
    }
    palette.persist_in(cache)?;
    Ok((!cancelled()).then_some(palette))
}
