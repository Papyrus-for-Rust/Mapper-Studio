//! Fast isolated catalog parsing under the normal installation source leases.
//! Missing/unsupported readers fall back to the existing compatibility parser.
use crate::installation::{self, Installation, SourceFile};
use serde_json::Value;
#[cfg(test)]
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
};

// The fully compared payload uses the existing layout contract. Parser identity
// is tracked separately by extractor_sha256, so downstream caches need no new schema.
pub(crate) const ADAPTER: &str = crate::prefab_assets::DISCOVERY_ADAPTER;
const LIMIT: u64 = 16 * 1024 * 1024;

pub(crate) fn fingerprint() -> String {
    let mut digest = Sha256::new();
    for (name, source) in [
        (
            "main.rs",
            include_str!("../../../tools/Mapper.AssetProbe/src/main.rs"),
        ),
        (
            "geometry.rs",
            include_str!("../../../tools/Mapper.AssetProbe/src/geometry.rs"),
        ),
        (
            "pages.rs",
            include_str!("../../../tools/Mapper.AssetProbe/src/pages.rs"),
        ),
        (
            "values.rs",
            include_str!("../../../tools/Mapper.AssetProbe/src/values.rs"),
        ),
        (
            "map_labels.rs",
            include_str!("../../../tools/Mapper.AssetProbe/src/map_labels.rs"),
        ),
        (
            "Cargo.toml",
            include_str!("../../../tools/Mapper.AssetProbe/Cargo.toml"),
        ),
        (
            "Cargo.lock",
            include_str!("../../../tools/Mapper.AssetProbe/Cargo.lock"),
        ),
    ] {
        digest.update(name.as_bytes());
        digest.update([0]);
        digest.update(source.as_bytes());
    }
    format!("{:x}", digest.finalize())
}
fn packaged(directory: &Path) -> PathBuf {
    directory.join("tools").join(if cfg!(windows) {
        "mapper-asset-reader.exe"
    } else {
        "mapper-asset-reader"
    })
}
pub(crate) fn reader() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let path = packaged(exe.parent()?);
    if path.is_file() {
        return Some(path);
    }
    #[cfg(debug_assertions)]
    {
        let name = if cfg!(windows) {
            "mapper-asset-reader.exe"
        } else {
            "mapper-asset-reader"
        };
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tools/Mapper.AssetProbe/target/release")
            .join(name);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}
pub(crate) fn status() -> Value {
    serde_json::json!({"format":"mapper-native-catalog-reader-status-1","schema":1,
        "extractor_sha256":fingerprint(),"reader_present":reader().is_some(),"payload_adapter":ADAPTER,
        "texture_source_reader":crate::native_texture_sources::status()})
}
fn metadata<'a>(
    bytes: &'a [u8],
    sources: &[SourceFile],
) -> Result<&'a serde_json::value::RawValue, String> {
    use serde::de::IgnoredAny;
    use std::collections::HashMap;

    #[derive(serde::Deserialize)]
    struct Report<'a> {
        format: &'a str,
        extractor_sha256: &'a str,
        backend: &'a str,
        manifest_sha256: &'a str,
        input_bytes: u64,
        issues: Vec<IgnoredAny>,
        roots_read: usize,
        #[serde(borrow)]
        catalog_metadata: &'a serde_json::value::RawValue,
    }
    #[derive(serde::Deserialize)]
    struct Coverage {
        format: u32,
        modifiers: HashMap<String, IgnoredAny>,
        special: HashMap<String, IgnoredAny>,
    }
    let report: Report<'_> = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    if report.format != "mapper-native-catalog-metadata-1"
        || report.extractor_sha256 != fingerprint()
        || report.backend != "unity-rs-core-0.6.2"
        || report.manifest_sha256 != sources[1].sha256
        || report.input_bytes != sources[2].bytes
        || !report.issues.is_empty()
        || !(1..=4096).contains(&report.roots_read)
    {
        return Err("Native catalog identity or coverage differs".into());
    }
    let counts: Coverage =
        serde_json::from_str(report.catalog_metadata.get()).map_err(|error| error.to_string())?;
    if counts.format != 5 || counts.modifiers.len() + counts.special.len() != report.roots_read {
        return Err("Native catalog identity or coverage differs".into());
    }
    Ok(report.catalog_metadata)
}
fn encode(
    sources: &[SourceFile],
    metadata: &serde_json::value::RawValue,
) -> Result<Vec<u8>, String> {
    #[derive(serde::Serialize)]
    struct Document<'a> {
        format: &'static str,
        schema: u32,
        adapter: &'static str,
        extractor_sha256: String,
        sources: &'a [SourceFile],
        metadata: &'a serde_json::value::RawValue,
    }
    serde_json::to_vec(&Document {
        format: "papyrus-prefab-catalog",
        schema: 1,
        adapter: ADAPTER,
        extractor_sha256: fingerprint(),
        sources,
        metadata,
    })
    .map_err(|e| e.to_string())
}

/// Returns no candidate only when the compatible Python reader should be tried.
/// Source mismatch, cancellation and publication errors remain hard failures.
pub(crate) fn prepare(
    root: &Path,
    source: &Installation,
    cache: &Path,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<(crate::prefab_assets::Catalog, PathBuf)>, String> {
    if cancelled() {
        return Err("Asset preparation cancelled".into());
    }
    let Some(reader) = reader() else {
        return Ok(None);
    };
    let _verification = installation::VerificationScope::default();
    source.validate()?;
    let manifest = source
        .scene_manifest
        .as_ref()
        .ok_or("Missing source manifest")?;
    let required = [
        (source.native.relative_path.as_str(), 64 * 1024 * 1024),
        (manifest.relative_path.as_str(), 32 * 1024 * 1024),
        ("Bundles/shared/assetscenes.bundle", 2 * 1024 * 1024 * 1024),
    ];
    let sources = required
        .iter()
        .map(|(path, limit)| installation::hash_file(root, path, *limit, cancelled))
        .collect::<Result<Vec<_>, _>>()?;
    if sources[0] != source.native || sources[1] != *manifest {
        return Err("Selected installation changed before native catalog preparation".into());
    }
    if cancelled() {
        return Err("Asset preparation cancelled".into());
    }
    fs::create_dir_all(cache).map_err(|e| e.to_string())?;
    let temporary = tempfile::Builder::new()
        .prefix(".native-catalog-")
        .tempdir_in(cache)
        .map_err(|e| e.to_string())?;
    let report = temporary.path().join("metadata.json");
    let progress = crate::loading_progress::begin("Reading monument metadata", None);
    let mut command = Command::new(reader);
    command.arg("--catalog-metadata").arg(root).arg(&report);
    match crate::metadata_runtime::execute(command, progress.reporter(), cancelled) {
        Ok(true) => {}
        Ok(false) => return Err("Asset preparation cancelled".into()),
        Err(error) => {
            if cancelled() {
                return Err("Asset preparation cancelled".into());
            }
            eprintln!("Native catalog compatibility fallback: {error}");
            return Ok(None);
        }
    }
    // Recheck the exact files while the operation's deny-write leases remain held.
    for ((path, limit), expected) in required.iter().zip(&sources) {
        if installation::hash_file(root, path, *limit, cancelled)? != *expected {
            return Err("Rust source changed during native catalog preparation".into());
        }
    }
    let candidate = (|| {
        let mut bytes = Vec::new();
        fs::File::open(&report)
            .map_err(|e| e.to_string())?
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > LIMIT {
            return Err("Native metadata exceeds allowance".into());
        }
        // Keep the exact numeric tokens from the reader's payload.
        encode(&sources, metadata(&bytes, &sources)?)
    })();
    let bytes = match candidate {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("Native catalog compatibility fallback: {error}");
            return Ok(None);
        }
    };
    if cancelled() {
        return Err("Asset preparation cancelled".into());
    }
    if bytes.len() as u64 > LIMIT {
        return Err("Native catalog exceeds allowance".into());
    }
    let catalog = match crate::prefab_assets::load_document(root, &bytes, source, cancelled) {
        Ok(catalog) => catalog,
        Err(error) if cancelled() || error.starts_with("Prefab source changed:") => {
            return Err(error);
        }
        Err(error) => {
            eprintln!("Native catalog compatibility fallback: {error}");
            return Ok(None);
        }
    };
    let hash = &catalog.sha256;
    let folder = temporary.path().join(hash);
    fs::create_dir(&folder).map_err(|e| e.to_string())?;
    let path = folder.join("catalog.json");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| e.to_string())?;
    drop(file);
    if cancelled() {
        return Err("Asset preparation cancelled".into());
    }
    crate::metadata_runtime::publish(&path, cache, hash, LIMIT)
        .map(|path| Some((catalog, path)))
        .map_err(|e| format!("Publish native monument catalog: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancelled_import_does_not_read_sources_or_create_cache_output() {
        let dir = tempfile::tempdir().unwrap();
        let source = Installation {
            kind: installation::InstallationKind::Client,
            native: SourceFile {
                relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                sha256: "a".repeat(64),
                bytes: 42,
            },
            scene_manifest: Some(SourceFile {
                relative_path: "Bundles/AssetSceneManifest.json".into(),
                sha256: "a".repeat(64),
                bytes: 42,
            }),
        };
        let output = dir.path().join("cache");
        assert!(
            prepare(
                &dir.path().join("missing-installation"),
                &source,
                &output,
                &|| true
            )
            .is_err_and(|error| error.contains("cancelled"))
        );
        assert!(!output.exists());
    }
    #[test]
    fn publication_keeps_numeric_metadata_verbatim() {
        let raw = serde_json::value::RawValue::from_string(
            r#"{"format":5,"f64":1.2345678901234567,"mask":16777217,"enabled":true}"#.into(),
        )
        .unwrap();
        let bytes = encode(&sources(), &raw).unwrap();
        #[derive(serde::Deserialize)]
        struct Envelope {
            metadata: Box<serde_json::value::RawValue>,
        }
        let envelope: Envelope = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(envelope.metadata.get(), raw.get());
    }
    fn sources() -> Vec<SourceFile> {
        ["native", "manifest", "scenes"]
            .map(|name| SourceFile {
                relative_path: name.into(),
                sha256: "a".repeat(64),
                bytes: 42,
            })
            .to_vec()
    }
    fn report() -> Value {
        json!({"format":"mapper-native-catalog-metadata-1","extractor_sha256":fingerprint(),"backend":"unity-rs-core-0.6.2",
            "manifest_sha256":"a".repeat(64),"input_bytes":42,"issues":[],"roots_read":1,
            "catalog_metadata":{"format":5,"modifiers":{"assets/example.prefab":{}},"special":{}}})
    }
    #[test]
    fn native_metadata_requires_current_code_complete_roster_and_exact_sources() {
        let sources = sources();
        let check = |report| serde_json::to_vec(&report).unwrap();
        assert!(metadata(&check(report()), &sources).is_ok());
        for (key, value) in [
            ("extractor_sha256", json!("b".repeat(64))),
            ("manifest_sha256", json!("b".repeat(64))),
            ("input_bytes", json!(41)),
            ("issues", json!([{"error":"unsupported root"}])),
            ("roots_read", json!(2)),
            ("backend", json!("unknown")),
        ] {
            let mut changed = report();
            changed[key] = value;
            assert!(metadata(&check(changed), &sources).is_err(), "{key}");
        }
        let mut changed = report();
        changed["catalog_metadata"]["special"] = Value::Null;
        assert!(metadata(&check(changed), &sources).is_err());
    }
    #[test]
    #[ignore = "Requires an owned Rust installation and compiled native asset reader"]
    fn owned_native_catalog_validates_and_publishes_without_reference_input() {
        let root =
            PathBuf::from(std::env::var_os("MAPPER_NATIVE_CATALOG_ROOT").expect("Rust root"));
        let output = PathBuf::from(
            std::env::var_os("MAPPER_NATIVE_CATALOG_OUTPUT").expect("New output directory"),
        );
        assert!(!output.exists());
        let _verification = installation::VerificationScope::default();
        let source = installation::inspect(&root, || false).unwrap();
        // Both readers receive the same held, verified source files. Initial
        // hashing is excluded from this catalog-only comparison.
        installation::hash_file(
            &root,
            "Bundles/shared/assetscenes.bundle",
            2 * 1024 * 1024 * 1024,
            &|| false,
        )
        .unwrap();
        let start = std::time::Instant::now();
        let (catalog, path) = prepare(&root, &source, &output, &|| false)
            .unwrap()
            .expect("Native reader used");
        let native_seconds = start.elapsed().as_secs_f64();
        assert!(!catalog.entries.is_empty());
        let reference = output.join("reference");
        let start = std::time::Instant::now();
        assert!(
            crate::metadata_runtime::run(
                crate::metadata_runtime::Script::Catalog,
                &[
                    "--rust-root".into(),
                    root.as_os_str().to_owned(),
                    "--cache-root".into(),
                    reference.as_os_str().to_owned()
                ],
                || false
            )
            .unwrap()
        );
        let reference_path = crate::metadata_runtime::document(&reference, "catalog.json").unwrap();
        let compatible =
            crate::prefab_assets::load(&root, &reference_path, &source, || false).unwrap();
        let compatibility_seconds = start.elapsed().as_secs_f64();
        let native_json: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let reference_json: Value =
            serde_json::from_slice(&fs::read(&reference_path).unwrap()).unwrap();
        assert_eq!(native_json["metadata"], reference_json["metadata"]);
        assert_eq!(catalog.sources, compatible.sources);
        fs::write(output.join("comparison.json"),serde_json::to_vec_pretty(&json!({
            "format":"mapper-catalog-import-comparison-1","native_catalog":path,"reference_catalog":reference_path,
            "native_seconds":native_seconds,"compatibility_seconds":compatibility_seconds,"entries":catalog.entries.len(),
            "initial_source_hashing_included":false,"os_cache_flushed":false,"metadata_matches":true,"sources_match":true,
            "scope":"Catalog metadata import, validation and private cache publication; excludes textures, generation stage preparation and complete startup"
        })).unwrap()).unwrap();
        eprintln!(
            "Native catalog: {} entries, {:.3}s; compatibility {:.3}s; {}",
            catalog.entries.len(),
            native_seconds,
            compatibility_seconds,
            path.display()
        );
    }
}
