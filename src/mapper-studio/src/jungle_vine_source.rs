//! Installation-bound metadata for the separate uniform jungle-vine program.
use crate::{
    decor_source,
    installation::{self, SourceFile},
    metadata_runtime::{self, Script},
    prefab_assets::Catalog,
    road_object_source,
};
use papyrus_terrain::{decor::Kind, decor_material::Removal};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path};

const LIMIT: u64 = 16 * 1024 * 1024;
const PROFILES: [&str; 3] = [
    include_str!("../../../compatibility/vines/client-0f703d43.json"),
    include_str!("../../../compatibility/vines/client-8dd4e235.json"),
    include_str!("../../../compatibility/vines/client-58f3263f.json"),
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    format: String,
    schema: u32,
    id: String,
    native_sha256: String,
    code_sources: Vec<SourceFile>,
    process_sha256: String,
    uniform_process_sha256: String,
    initializer_sha256: String,
    default_enabled: bool,
    topology_flags: std::collections::BTreeMap<String, u32>,
    collider_backfaces: bool,
    material_removal: Removal,
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
impl Profile {
    fn validate(&self) -> Result<(), String> {
        self.material_removal.validate()?;
        if self.format != "papyrus-jungle-vine-profile"
            || self.schema != 1
            || !self.default_enabled
            || self.id.is_empty()
            || self.id.len() > 128
            || [
                &self.native_sha256,
                &self.process_sha256,
                &self.uniform_process_sha256,
                &self.initializer_sha256,
            ]
            .iter()
            .any(|value| !hash(value))
            || self.code_sources.len() != 2
            || self.code_sources[0].relative_path != "GameAssembly.dll"
            || self.code_sources[1].relative_path
                != "RustClient_Data/il2cpp_data/Metadata/global-metadata.dat"
            || self
                .code_sources
                .iter()
                .any(|r| !hash(&r.sha256) || r.bytes == 0 || r.bytes > 1024 * 1024 * 1024)
            || self.topology_flags.len() > 32
            || self
                .topology_flags
                .values()
                .any(|flag| flag.count_ones() != 1)
            || self
                .topology_flags
                .values()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.topology_flags.len()
            || !["Building", "Mountain"].iter().any(|role|
                self.topology_flags.get(*role) == Some(&self.material_removal.excluded_topology))
        {
            return Err("Invalid jungle vine compatibility profile".into());
        }
        Ok(())
    }
}
fn selected(native: &str) -> Result<(Profile, String), String> {
    for raw in PROFILES {
        let profile: Profile = serde_json::from_str(raw).map_err(|e| e.to_string())?;
        profile.validate()?;
        if profile.native_sha256 == native {
            return Ok((profile, digest(raw.as_bytes())));
        }
    }
    Err("Jungle vines need a compatible selected Rust installation".into())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    format: String,
    schema: u32,
    profile_sha256: String,
    metadata: Box<serde_json::value::RawValue>,
}
pub(crate) struct Source {
    metadata: decor_source::Source,
    raw_metadata: Box<serde_json::value::RawValue>,
    sha256: String,
}

fn verify_stages(
    metadata: &decor_source::Source,
    road: &road_object_source::Source,
) -> Result<(), String> {
    metadata.verify_road_binding(road)?;
    let entries: Vec<_> = road
        .procedural_order()?
        .iter()
        .filter(|entry| entry.kind == "PlaceJungleVines")
        .collect();
    if entries.len() != metadata.stages.len() || entries.is_empty() || entries.len() > 16 {
        return Err("Jungle vine roster differs from the procedural workflow".into());
    }
    for (stage, entry) in metadata.stages.iter().zip(entries) {
        if stage.kind != Kind::Uniform
            || stage.id.parse::<i64>().ok() != Some(entry.object_id)
            || stage.name != entry.description
            || stage.mode != entry.mode
            || stage.enabled != (entry.hierarchy_active && !entry.disabled.unwrap_or(false))
        {
            return Err("Jungle vine stage differs from its source workflow entry".into());
        }
    }
    Ok(())
}

impl Source {
    pub(crate) fn metadata_bytes(&self) -> &[u8] {
        self.raw_metadata.get().as_bytes()
    }
    pub(crate) fn metadata(&self) -> &decor_source::Source {
        &self.metadata
    }
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }
    #[allow(clippy::too_many_arguments)]
    fn load(
        root: &Path,
        path: &Path,
        catalog: &Catalog,
        road: &road_object_source::Source,
        profile: &Profile,
        profile_sha: &str,
        topology_sha: &str,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Self, String> {
        if cancelled() {
            return Err("Jungle vine preparation cancelled".into());
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > LIMIT {
            return Err("Jungle vine source exceeds 16 MiB".into());
        }
        let doc: Document = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if doc.format != "papyrus-jungle-vine-source"
            || doc.schema != 1
            || doc.profile_sha256 != profile_sha
        {
            return Err("Jungle vine source differs from its compatibility profile".into());
        }
        let metadata = decor_source::Source::from_bytes_bound(
            root,
            doc.metadata.get().as_bytes(),
            catalog,
            None,
            cancelled,
        )?;
        if !metadata.preparation_matches(topology_sha, &profile.code_sources)
            || metadata.material_removal() != Some(profile.material_removal)
            || metadata.producer_sha256() != metadata_runtime::vine_extractor_sha256()
        {
            return Err(
                "Jungle vine metadata has a different producer or material policy".into(),
            );
        }
        verify_stages(&metadata, road)?;
        Ok(Self {
            metadata,
            raw_metadata: doc.metadata,
            sha256: digest(&bytes),
        })
    }
    fn verify_ready(&self, catalog: &Catalog) -> Result<(), String> {
        let prepared = self.metadata.prepare_authored_live(catalog)?;
        for (path, fitting) in &prepared.fitting {
            if !fitting.metadata().unsupported.is_empty() {
                return Err(format!("Unsupported vine-tree fitting metadata for {path}"));
            }
        }
        for (path, terrain) in &prepared.terrain {
            if !terrain.unsupported.is_empty() {
                return Err(format!(
                    "Unsupported vine-tree terrain effects for {path}: {}",
                    terrain.unsupported.join(", ")
                ));
            }
        }
        Ok(())
    }
}

pub(crate) fn prepare(
    root: &Path,
    catalog: &Catalog,
    road: &road_object_source::Source,
    cancelled: impl Fn() -> bool,
) -> Result<Source, String> {
    resolve(root, catalog, road, None, true, cancelled)
}
pub(crate) fn available(
    root: &Path,
    catalog: &Catalog,
    road: &road_object_source::Source,
    cancelled: impl Fn() -> bool,
) -> Result<Source, String> {
    resolve(root, catalog, road, None, false, cancelled)
}
pub(crate) fn cached(
    root: &Path,
    catalog: &Catalog,
    road: &road_object_source::Source,
    pin: &str,
    cancelled: impl Fn() -> bool,
) -> Result<Source, String> {
    if !hash(pin) {
        return Err("Invalid jungle vine source fingerprint".into());
    }
    resolve(root, catalog, road, Some(pin), false, cancelled)
}
fn resolve(
    root: &Path,
    catalog: &Catalog,
    road: &road_object_source::Source,
    pin: Option<&str>,
    capture: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Source, String> {
    let _verification = installation::VerificationScope::default();
    if cancelled() {
        return Err("Jungle vine preparation cancelled".into());
    }
    let (profile, profile_sha) = selected(&catalog.installation.native.sha256)?;
    for source in &profile.code_sources {
        if installation::hash_file(root, &source.relative_path, 1024 * 1024 * 1024, &cancelled)?
            != *source
        {
            return Err(
                "Jungle vine gameplay code differs from the selected installation".into(),
            );
        }
    }
    let scene = catalog
        .sources
        .iter()
        .find(|r| r.relative_path == "Bundles/shared/assetscenes.bundle")
        .ok_or("Missing jungle vine scene source")?;
    let mut topology = serde_json::json!({
        "format":"papyrus-topology-profile", "schema":1, "id":profile.id,
        "native_sha256":profile.native_sha256, "scene_sha256":scene.sha256,
        "code_sources":profile.code_sources, "flags":profile.topology_flags,
        "collider_sampling":{"backfaces":profile.collider_backfaces},
        "material_removal":profile.material_removal, "vine_profile_sha256":profile_sha,
    });
    if let Some(layout) = road.stage_layout() {
        topology["stage_layout"] = layout.into();
    }
    let topology = serde_json::to_vec(&topology).map_err(|e| e.to_string())?;
    let topology_sha = digest(&topology);
    let local = crate::storage::local_data()?;
    let cache = local.join("papyrus-vine-cache");
    for path in crate::object_extract::documents(&cache, "vines.json")? {
        if cancelled() {
            return Err("Jungle vine preparation cancelled".into());
        }
        if pin.is_some_and(|pin| {
            path.parent()
                .and_then(Path::file_name)
                .and_then(|n| n.to_str())
                != Some(pin)
        }) {
            continue;
        }
        if let Ok(source) = Source::load(
            root,
            &path,
            catalog,
            road,
            &profile,
            &profile_sha,
            &topology_sha,
            &cancelled,
        ) && path
            .parent()
            .and_then(Path::file_name)
            .and_then(|p| p.to_str())
            == Some(source.sha256.as_str())
        {
            source.verify_ready(catalog)?;
            return Ok(source);
        }
    }
    if !capture {
        return Err("Prepared vine-tree data is unavailable. Prepare the selected Rust installation again.".into());
    }
    std::fs::create_dir_all(&local).map_err(|e| e.to_string())?;
    let temporary = tempfile::Builder::new()
        .prefix(".vine-prepare-")
        .tempdir_in(&local)
        .map_err(|e| e.to_string())?;
    let profile_path = temporary.path().join("profile.json");
    std::fs::write(&profile_path, topology).map_err(|e| e.to_string())?;
    let output = temporary.path().join("output");
    if !metadata_runtime::run(
        Script::JungleVines,
        &[
            "--rust-root".into(),
            root.as_os_str().to_owned(),
            "--topology-profile".into(),
            profile_path.into_os_string(),
            "--cache-root".into(),
            output.as_os_str().to_owned(),
        ],
        &cancelled,
    )? {
        return Err("Jungle vine preparation cancelled".into());
    }
    let document = metadata_runtime::document(&output, "vines.json")?;
    let source = Source::load(
        root,
        &document,
        catalog,
        road,
        &profile,
        &profile_sha,
        &topology_sha,
        &cancelled,
    )?;
    source.verify_ready(catalog)?;
    if cancelled() {
        return Err("Jungle vine preparation cancelled".into());
    }
    metadata_runtime::publish(&document, &cache, &source.sha256, LIMIT)?;
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profiles_bind_uniform_policy_and_enabled_default_to_both_clients() {
        for raw in PROFILES {
            let profile: Profile = serde_json::from_str(raw).unwrap();
            let (_, receipt) = selected(&profile.native_sha256).unwrap();
            let role = if profile.id.starts_with("client-58f3263f") { "Mountain" } else { "Building" };
            assert_eq!(profile.material_removal.excluded_topology, profile.topology_flags[role]);
            assert_eq!(receipt, digest(raw.as_bytes()));
            let original: serde_json::Value = serde_json::from_str(raw).unwrap();
            for field in ["process_sha256", "uniform_process_sha256", "initializer_sha256"] {
                let mut changed = original.clone();
                changed[field] = "invalid".into();
                assert!(serde_json::from_value::<Profile>(changed).unwrap().validate().is_err());
            }
            let mut changed = original;
            changed["default_enabled"] = false.into();
            assert!(serde_json::from_value::<Profile>(changed).unwrap().validate().is_err());
        }
        assert!(selected(&"0".repeat(64)).is_err());
    }
}
