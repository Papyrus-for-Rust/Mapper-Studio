//! Native source discovery under the asset importer's existing source leases.
use sha2::{Digest, Sha256};
use std::path::PathBuf;
#[cfg(windows)]
use std::process::Command;
#[cfg(any(windows, test))]
use std::{fs, io::Read, path::Path};

pub(crate) fn fingerprint() -> String {
    let mut digest = Sha256::new();
    for (name, bytes) in [
        (
            "bin/mapper-texture-sources.rs",
            include_bytes!("../../../tools/Mapper.AssetProbe/src/bin/mapper-texture-sources.rs")
                .as_slice(),
        ),
        (
            "texture_sources.rs",
            include_bytes!("../../../tools/Mapper.AssetProbe/src/texture_sources.rs").as_slice(),
        ),
        (
            "pages.rs",
            include_bytes!("../../../tools/Mapper.AssetProbe/src/pages.rs").as_slice(),
        ),
        (
            "Cargo.toml",
            include_bytes!("../../../tools/Mapper.AssetProbe/Cargo.toml").as_slice(),
        ),
        (
            "Cargo.lock",
            include_bytes!("../../../tools/Mapper.AssetProbe/Cargo.lock").as_slice(),
        ),
    ] {
        digest.update(name.as_bytes());
        digest.update([0]);
        digest.update(bytes);
    }
    format!("{:x}", digest.finalize())
}

fn reader() -> Option<PathBuf> {
    let path = crate::native_catalog::reader()?.with_file_name(if cfg!(windows) {
        "mapper-texture-sources.exe"
    } else {
        "mapper-texture-sources"
    });
    path.is_file().then_some(path)
}

pub(crate) fn status() -> serde_json::Value {
    serde_json::json!({"extractor_sha256":fingerprint(), "reader_present":reader().is_some()})
}

#[cfg(windows)]
pub(crate) enum IdentityRead {
    PrefabIds,
    ManifestFields,
}

#[cfg(windows)]
pub(crate) fn read_identities(
    kind: IdentityRead,
    root: &Path,
    output: &Path,
    cancelled: &impl Fn() -> bool,
) -> Result<bool, String> {
    let Some(reader) = reader() else {
        return Ok(false);
    };
    let (argument, label) = match kind {
        IdentityRead::PrefabIds => ("--prefab-identities", "Reading prefab identities"),
        IdentityRead::ManifestFields => ("--identity-fields", "Reading prefab references"),
    };
    let progress = crate::loading_progress::begin(label, None);
    let mut command = Command::new(reader);
    command
        .arg(argument)
        .arg(root)
        .arg(output)
        .arg(fingerprint());
    match crate::metadata_runtime::execute(command, progress.reporter(), cancelled) {
        Ok(finished) => Ok(finished),
        // The caller gives the compatibility extractor its own output path.
        Err(_) if !cancelled() => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
pub(crate) fn prepare(
    root: &Path,
    catalog: &Path,
    catalog_sha256: &str,
    plan: &Path,
    cancelled: &impl Fn() -> bool,
) -> Result<bool, String> {
    let Some(reader) = reader() else {
        return Ok(false);
    };
    if plan.try_exists().map_err(|e| e.to_string())? {
        return Err("Texture source plan must be new".into());
    }
    let progress = crate::loading_progress::begin("Locating monument textures", None);
    let mut command = Command::new(reader);
    command.arg(root).arg(catalog).arg(plan).arg(fingerprint());
    let result = match crate::metadata_runtime::execute(command, progress.reporter(), cancelled) {
        Ok(true) => validate(plan, catalog_sha256),
        Ok(false) => return Err("Asset preparation cancelled".into()),
        Err(error) => Err(error),
    };
    if cancelled() {
        return Err("Asset preparation cancelled".into());
    }
    match result {
        Ok(()) => Ok(true),
        Err(error) => {
            // This is the fresh private output of this invocation. The existing
            // importer needs that path available when handling a new layout.
            if plan.try_exists().map_err(|e| e.to_string())? {
                fs::remove_file(plan).map_err(|e| e.to_string())?;
            }
            eprintln!("Native texture-source compatibility fallback: {error}");
            Ok(false)
        }
    }
}

#[cfg(any(windows, test))]
fn validate(path: &Path, catalog_sha256: &str) -> Result<(), String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(crate::prefab_textures::SOURCE_PLAN_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    crate::prefab_textures::source_plan(&bytes, catalog_sha256).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_plans_require_the_exact_catalog_and_bounded_existing_contract() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("plan.json");
        let hash = "a".repeat(64);
        let mut plan = serde_json::json!({"format":"mapper-texture-sources", "schema":1,
            "catalog_sha256":hash,"bundles":["Bundles/shared/content.bundle","Bundles/shared/monuments.bundle"]});
        fs::write(&path, serde_json::to_vec(&plan).unwrap()).unwrap();
        validate(&path, &hash).unwrap();
        assert!(validate(&path, &"b".repeat(64)).is_err());
        plan["bundles"][1] = "../other.bundle".into();
        fs::write(&path, serde_json::to_vec(&plan).unwrap()).unwrap();
        assert!(validate(&path, &hash).is_err());
        fs::write(&path, vec![b' '; 4097]).unwrap();
        assert!(validate(&path, &hash).is_err());
    }
}
