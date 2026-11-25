//! RUST_ROOT CATALOG BINDINGS NEW_REPORT. Read-only cache/decoded-pixel audit.
use mapper_studio::{installation, prefab_assets, prefab_textures::Bindings};
use serde_json::json;
use std::{io::Write, path::Path, time::Instant};
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 4 {
        return Err("RUST_ROOT CATALOG BINDINGS NEW_REPORT".into());
    }
    if Path::new(&args[3]).exists() {
        return Err("Report must be new".into());
    }
    let started = Instant::now();
    let root = Path::new(&args[0]);
    let installation = installation::inspect(root, || false)?;
    let catalog = prefab_assets::load(root, Path::new(&args[1]), &installation, || false)?;
    let catalog_seconds = started.elapsed().as_secs_f64();
    let started = Instant::now();
    let bindings = Bindings::load(root, Path::new(&args[2]), &catalog, || false)?;
    let bindings_seconds = started.elapsed().as_secs_f64();
    let started = Instant::now();
    let mut decoded_bytes = 0u64;
    let mut stored_bytes = 0u64;
    for (guid, texture) in bindings.textures() {
        // Each read verifies both stored and decoded checksums. Drop its pixels
        // before reading the next texture; do not retain the whole catalog.
        decoded_bytes += bindings.read_rgba(guid)?.len() as u64;
        stored_bytes += texture.storage.as_ref().map_or(texture.bytes, |s| s.bytes);
    }
    let report = json!({"format":"mapper-prefab-cache-inspection-1",
        "catalog_sha256":catalog.sha256,"bindings_sha256":bindings.sha256,
        "texture_count":bindings.textures().len(),"decoded_bytes":decoded_bytes,"stored_bytes":stored_bytes,
        "installation_and_catalog_seconds":catalog_seconds,"bindings_verification_seconds":bindings_seconds,
        "all_pixels_decode_and_verify_seconds":started.elapsed().as_secs_f64(),
        "scope":"Owned-file cache and exact pixel checks only. No extraction, generation, GUI, native game library or server execution."});
    let mut output = std::fs::File::options()
        .create_new(true)
        .write(true)
        .open(&args[3])
        .map_err(|e| e.to_string())?;
    output
        .write_all(&serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    println!(
        "Verified {} textures: {} stored bytes, {} decoded bytes",
        bindings.textures().len(),
        stored_bytes,
        decoded_bytes
    );
    Ok(())
}
