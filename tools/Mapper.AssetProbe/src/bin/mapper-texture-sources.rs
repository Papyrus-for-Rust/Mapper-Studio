//! Isolated native texture-source discovery. Pixel decoding stays in the existing importer.
#[path = "../pages.rs"]
mod pages;
#[path = "../texture_sources.rs"]
mod texture_sources;

use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::Path;

fn fingerprint() -> String {
    let mut digest = Sha256::new();
    for (name, bytes) in [
        (
            "bin/mapper-texture-sources.rs",
            include_bytes!("mapper-texture-sources.rs").as_slice(),
        ),
        (
            "texture_sources.rs",
            include_bytes!("../texture_sources.rs").as_slice(),
        ),
        ("pages.rs", include_bytes!("../pages.rs").as_slice()),
        ("Cargo.toml", include_bytes!("../../Cargo.toml").as_slice()),
        ("Cargo.lock", include_bytes!("../../Cargo.lock").as_slice()),
    ] {
        digest.update(name.as_bytes());
        digest.update([0]);
        digest.update(bytes);
    }
    format!("{:x}", digest.finalize())
}

fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == "--self-test" {
        println!(
            "{}",
            json!({"format":"mapper-native-texture-sources-self-test", "schema":1,
            "extractor_sha256":fingerprint(), "backend":"unity-rs-core-0.6.2"})
        );
        return Ok(());
    }
    if !(3..=4).contains(&args.len()) {
        return Err("RUST_ROOT CATALOG_JSON NEW_PLAN_JSON [EXPECTED_READER_SHA256]".into());
    }
    if args.len() == 4 && args[3].to_str() != Some(fingerprint().as_str()) {
        return Err("Texture source reader differs from the expected source".into());
    }
    if args[0] == "--prefab-identities" {
        return texture_sources::write_prefab_ids(Path::new(&args[1]), Path::new(&args[2]));
    }
    if args[0] == "--identity-fields" {
        return texture_sources::write_identity_fields(Path::new(&args[1]), Path::new(&args[2]));
    }
    texture_sources::write_plan(
        Path::new(&args[0]),
        Path::new(&args[1]),
        Path::new(&args[2]),
    )
}
