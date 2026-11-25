//! Receipt-checked offshore scene data. Capture compatibility is not proof of
//! complete gameplay-code execution, environment validation or map acceptance.
use crate::{
    installation::{self, SourceFile},
    prefab_assets::Catalog,
};
use papyrus_terrain::offshore_search::{Policy, Prefab};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, io::Read, path::Path};

const LIMIT: u64 = 1024 * 1024;
const PREFIX: &str = "assets/bundled/prefabs/autospawn/";
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Stage {
    pub object_id: i64,
    pub order: usize,
    pub mode: i32,
    pub disabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hierarchy_active: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component_enabled: Option<bool>,
    pub folders: Vec<String>,
    pub target_count: usize,
    pub minimum_world_size: u32,
    pub minimum_distance: u32,
    pub maximum_distance: u32,
    pub spacing: u32,
}
impl Stage {
    /// Pass/attempt counts belong to an explicit independent execution profile.
    /// The installation supplies every geometric distance and stage count.
    pub fn policy(&self, passes: usize, attempts_per_prefab: usize) -> Result<Policy, String> {
        let policy = Policy {
            minimum_world_size: self.minimum_world_size,
            target_count: self.target_count,
            minimum_distance: self.minimum_distance as f32,
            maximum_distance: self.maximum_distance as f32,
            spacing: self.spacing as f32,
            passes,
            attempts_per_prefab,
        };
        policy.validate()?;
        Ok(policy)
    }
    pub fn runs(&self, size: u32, realm_mask: i32) -> Result<bool, String> {
        if !(1000..=6000).contains(&size) || !(1..=3).contains(&realm_mask) {
            return Err("Invalid offshore stage realm or world size".into());
        }
        let active = match (self.disabled, self.hierarchy_active, self.component_enabled) {
            (Some(disabled), None, None) => !disabled,
            (None, Some(active), Some(_)) => active,
            _ => return Err("Offshore activation fields require a matching adapter".into()),
        };
        Ok(active && self.mode & realm_mask != 0 && size >= self.minimum_world_size)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    format: String,
    schema: u32,
    extractor_sha256: String,
    sources: Vec<SourceFile>,
    stages: Vec<Stage>,
    full_stage_execution_validated: bool,
    #[serde(default)]
    activation: Option<String>,
    #[serde(default)]
    code_profile_sha256: Option<String>,
    #[serde(default)]
    prefabs: Option<std::collections::BTreeMap<String,Details>>,
}
#[derive(Debug,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Details {
    pub rotation:[f32;4],
    pub scale:[f32;3],
    pub root_decor:Vec<crate::monument_attributes::RootDecor>,
    pub water_types:Vec<i32>,
    nodes_visited:usize,
}
pub struct Source {
    pub sha256: String,
    pub stages: Vec<Stage>,
    pub prefabs: Option<std::collections::BTreeMap<String,Details>>,
    sources: Vec<SourceFile>,
    extractor_sha256: String,
}
pub struct Entry {
    pub path: String,
    pub prefab: Prefab,
}
fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn folder(value: &str) -> bool {
    value.starts_with(PREFIX)
        && value.ends_with('/')
        && value.len() <= 512
        && value.len() > PREFIX.len() + 1
        && value.split('/').all(|s| s != ".." && s != ".")
        && value[..value.len() - 1].split('/').all(|s| {
            !s.is_empty()
                && s.bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_-".contains(&c))
        })
}
impl Source {
    /// Inspect the selected installation with the bundled data parser and publish
    /// a content-addressed numeric snapshot. Never executes the game's procgen.
    pub fn prepare(
        root: &Path,
        catalog: &Catalog,
        cancelled: impl Fn() -> bool,
    ) -> Result<(Self, std::path::PathBuf), String> {
        if cancelled() {
            return Err("Offshore source preparation cancelled".into());
        }
        let cache = crate::storage::local_data()?.join("papyrus-offshore-stage-cache");
        std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
        if let Some(cached) = Self::cached(&cache, root, catalog, &cancelled)? {
            return Ok(cached);
        }
        let temporary = tempfile::Builder::new()
            .prefix(".prepare-")
            .tempdir_in(&cache)
            .map_err(|e| e.to_string())?;
        let output = temporary.path().join("stages.json");
        let mut arguments = vec![
            "--rust-root".into(),
            root.as_os_str().into(),
            "--output".into(),
            output.as_os_str().into(),
        ];
        if let Some(raw) = crate::monument_stages::hierarchy_code_profile(catalog)? {
            let profile = temporary.path().join("code-profile.json");
            std::fs::write(&profile, raw).map_err(|e| e.to_string())?;
            arguments.extend(["--code-profile".into(), profile.into_os_string()]);
        }
        if !crate::metadata_runtime::run(
            crate::metadata_runtime::Script::OffshoreStages,
            &arguments,
            &cancelled,
        )? {
            return Err("Offshore source preparation cancelled".into());
        }
        let source = Self::load(&output, root, catalog, &cancelled)?;
        let path = source.publish(&output, &cache, &cancelled)?;
        Ok((source, path))
    }
    /// Atomic replacement also repairs a damaged content-addressed snapshot.
    /// Concurrent producers for this identity publish identical verified bytes.
    fn publish(
        &self,
        document: &Path,
        cache: &Path,
        cancelled: impl Fn() -> bool,
    ) -> Result<std::path::PathBuf, String> {
        let mut raw = Vec::new();
        std::fs::File::open(document)
            .map_err(|e| e.to_string())?
            .take(LIMIT + 1)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
        if raw.len() as u64 > LIMIT || format!("{:x}", Sha256::digest(&raw)) != self.sha256 {
            return Err("Offshore source changed before publication".into());
        }
        let group = cache.join(&self.sha256);
        std::fs::create_dir_all(&group).map_err(|e| e.to_string())?;
        let destination = group.join("stages.json");
        let mut file = tempfile::NamedTempFile::new_in(&group).map_err(|e| e.to_string())?;
        std::io::Write::write_all(&mut file, &raw).map_err(|e| e.to_string())?;
        file.as_file().sync_all().map_err(|e| e.to_string())?;
        if cancelled() {
            return Err("Offshore source preparation cancelled".into());
        }
        file.persist(&destination).map_err(|e| e.to_string())?;
        Ok(destination)
    }
    fn cached(
        cache: &Path,
        root: &Path,
        catalog: &Catalog,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<(Self, std::path::PathBuf)>, String> {
        let extractor = crate::metadata_runtime::offshore_extractor_sha256();
        for entry in std::fs::read_dir(cache)
            .map_err(|e| e.to_string())?
            .take(256)
        {
            if cancelled() {
                return Err("Offshore source preparation cancelled".into());
            }
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name();
            let Some(name) = name.to_str().filter(|name| hash(name)) else {
                continue;
            };
            let path = entry.path().join("stages.json");
            let Ok(file) = std::fs::File::open(&path) else {
                continue;
            };
            let mut raw = Vec::new();
            if file.take(LIMIT + 1).read_to_end(&mut raw).is_err() {
                continue;
            }
            if let Ok(source) = Self::decode(&raw, catalog)
                && source.sha256 == name
                && source.extractor_sha256 == extractor
                && let Ok(source) = Self::load(&path, root, catalog, &cancelled)
            {
                return Ok(Some((source, path)));
            }
        }
        if cancelled() {
            return Err("Offshore source preparation cancelled".into());
        }
        Ok(None)
    }
    pub fn decode(raw: &[u8], catalog: &Catalog) -> Result<Self, String> {
        if raw.len() as u64 > LIMIT {
            return Err("Offshore capture exceeds one MiB".into());
        }
        catalog.installation.validate()?;
        let doc: Document = serde_json::from_slice(raw).map_err(|e| e.to_string())?;
        let hierarchy = doc.schema == 2;
        let code_profile = crate::monument_stages::hierarchy_code_profile(catalog)?;
        if code_profile.is_some() && !hierarchy {
            return Err("Current offshore installation requires hierarchy activation".into());
        }
        if hierarchy {
            let raw =
                code_profile.ok_or("No source-matched offshore hierarchy adapter is available")?;
            let profile: serde_json::Value =
                serde_json::from_str(raw).map_err(|e| e.to_string())?;
            let mut expected = catalog.sources.clone();
            expected.push(
                serde_json::from_value(profile["map_scene"].clone()).map_err(|e| e.to_string())?,
            );
            expected.extend(
                serde_json::from_value::<Vec<SourceFile>>(profile["code_sources"].clone())
                    .map_err(|e| e.to_string())?,
            );
            if doc.sources != expected
                || doc.activation.as_deref() != profile["activation"].as_str()
                || doc.code_profile_sha256.as_deref()
                    .is_none_or(|hash| !crate::source_profile::matches(raw, hash))
            {
                return Err("Offshore capture differs from its activation adapter".into());
            }
        } else if doc.schema != 1
            || doc.sources.len() != 4
            || doc.activation.is_some()
            || doc.code_profile_sha256.is_some()
        {
            return Err("Unsupported offshore capture schema or activation".into());
        }
        if doc.format != "papyrus-offshore-stage-capture"
            || !hash(&doc.extractor_sha256)
            || doc.full_stage_execution_validated
            || doc.sources.len() < 4
            || catalog.sources.len() != 3
            || doc.sources[..3] != catalog.sources
            || doc.sources[0] != catalog.installation.native
            || catalog.installation.scene_manifest.as_ref() != Some(&doc.sources[1])
            || doc.sources[2].relative_path != "Bundles/shared/assetscenes.bundle"
            || doc.sources[3].relative_path != "Bundles/maps/maps.bundle"
            || doc.sources.iter().any(|s| !hash(&s.sha256) || s.bytes == 0)
            || doc.sources[2].bytes > 2 * 1024 * 1024 * 1024
            || doc.sources[3].bytes > 512 * 1024 * 1024
            || !(1..=64).contains(&doc.stages.len())
        {
            return Err("Offshore capture differs from installation or supported schema".into());
        }
        let mut ids = BTreeSet::new();
        let mut previous = None;
        for stage in &doc.stages {
            stage.policy(1, 1)?;
            if stage.disabled.is_none() != hierarchy
                || stage.hierarchy_active.is_some() != hierarchy
                || stage.component_enabled.is_some() != hierarchy
            {
                return Err("Offshore stage activation differs from its source schema".into());
            }
            if stage.object_id <= 0
                || !ids.insert(stage.object_id)
                || stage.order > 65535
                || previous.is_some_and(|v| stage.order <= v)
                || !(-1..=3).contains(&stage.mode)
                || !(1..=32).contains(&stage.folders.len())
                || stage.folders.iter().any(|p| !folder(p))
                || stage.folders.iter().collect::<BTreeSet<_>>().len() != stage.folders.len()
                || stage.folders.iter().enumerate().any(|(i, a)| {
                    stage
                        .folders
                        .iter()
                        .enumerate()
                        .any(|(j, b)| i != j && a.starts_with(b))
                })
            {
                return Err("Invalid offshore stage program or selector".into());
            }
            previous = Some(stage.order);
        }
        if let Some(prefabs)=&doc.prefabs {
            if prefabs.is_empty() || prefabs.len()>128 {return Err("Invalid offshore detail roster".into());}
            for (path,details) in prefabs {
                if !path.starts_with(PREFIX) || !path.ends_with(".prefab") || path.len()>512 || path.split('/').any(|p|p.is_empty() || p=="..")
                    || details.water_types.len()>1024 || !(1..=65536).contains(&details.nodes_visited)
                {return Err("Invalid offshore prefab details".into());}
                crate::offshore_decor::Program::from_parts(details.rotation,details.scale,&details.root_decor)?;
            }
        }
        Ok(Self {
            sha256: format!("{:x}", Sha256::digest(raw)),
            stages: doc.stages,
            prefabs:doc.prefabs,
            sources: doc.sources,
            extractor_sha256: doc.extractor_sha256,
        })
    }
    pub fn load(
        path: &Path,
        root: &Path,
        catalog: &Catalog,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        let mut raw = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(LIMIT + 1)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
        let source = Self::decode(&raw, catalog)?;
        for (index, receipt) in source.sources.iter().enumerate() {
            let limit = match index {
                0 => 64 * 1024 * 1024,
                1 => 32 * 1024 * 1024,
                2 => 2 * 1024 * 1024 * 1024,
                3 => 512 * 1024 * 1024,
                _ => 1024 * 1024 * 1024,
            };
            if installation::hash_file(root, &receipt.relative_path, limit, &cancelled)? != *receipt
            {
                return Err("Installation changed since offshore capture".into());
            }
        }
        if cancelled() {
            return Err("Offshore source verification cancelled".into());
        }
        Ok(source)
    }
    /// Input ordering is an independent stable priority order. No official seed
    /// shuffle parity is claimed. Missing priority metadata has no guessed fallback.
    pub fn roster(&self, id: i64, catalog: &Catalog) -> Result<Vec<Entry>, String> {
        if self.sources[..3] != catalog.sources {
            return Err("Offshore roster catalog changed".into());
        }
        let stage = self
            .stages
            .iter()
            .find(|s| s.object_id == id)
            .ok_or("Unknown offshore stage identity")?;
        let mut entries: Vec<_> = catalog
            .entries
            .iter()
            .filter(|(p, _)| stage.folders.iter().any(|f| p.starts_with(f)))
            .collect();
        entries.sort_by_key(|(p, _)| *p);
        if entries.len() > 128 {
            return Err("Offshore roster exceeds its allowance".into());
        }
        let mut roster = Vec::new();
        for (i, (path, asset)) in entries.into_iter().enumerate() {
            let priority = asset
                .priority
                .and_then(|p| p.checked_add(1))
                .filter(|p| (1..=4096).contains(p))
                .ok_or("Offshore prefab priority needs a compatible adapter")?;
            roster.push(Entry {
                path: path.clone(),
                prefab: Prefab {
                    id: i as u64 + 1,
                    score: (priority as u64).pow(4),
                },
            });
        }
        roster.sort_by_key(|entry| std::cmp::Reverse(entry.prefab.score));
        Ok(roster)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires an owned installed-client capture and catalog"]
    fn owned_client_capture_verifies_sources_and_reuses_only_current_cache() {
        let path = |name| std::path::PathBuf::from(std::env::var_os(name).expect(name));
        let root = path("MAPPER_OFFSHORE_TEST_ROOT");
        let installation = installation::inspect(&root, || false).unwrap();
        let catalog = crate::prefab_assets::load(
            &root,
            &path("MAPPER_OFFSHORE_TEST_CATALOG"),
            &installation,
            || false,
        )
        .unwrap();
        let input = path("MAPPER_OFFSHORE_TEST_CAPTURE");
        let source = Source::load(&input, &root, &catalog, || false).unwrap();
        assert_eq!(
            source.extractor_sha256,
            crate::metadata_runtime::offshore_extractor_sha256()
        );
        let roster = source.roster(source.stages[0].object_id, &catalog).unwrap();
        assert!(!roster.is_empty());
        let cache = tempfile::tempdir().unwrap();
        let group = cache.path().join(&source.sha256);
        std::fs::create_dir(&group).unwrap();
        let output = group.join("stages.json");
        std::fs::copy(&input, &output).unwrap();
        let (cached, cached_path) = Source::cached(cache.path(), &root, &catalog, || false)
            .unwrap()
            .expect("Verified capture must be reused");
        assert_eq!(source.sha256, cached.sha256);
        assert_eq!(cached_path, output);
        let mut doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&input).unwrap()).unwrap();
        doc["extractor_sha256"] = "c".repeat(64).into();
        let raw = serde_json::to_vec(&doc).unwrap();
        std::fs::write(output, raw).unwrap();
        assert!(
            Source::cached(cache.path(), &root, &catalog, || false)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn hierarchy_capture_requires_matched_code_and_retains_activation_provenance() {
        let (mut catalog, mut doc) = fixture();
        let raw = crate::metadata_runtime::CURRENT_MONUMENT_STAGE_CODE_PROFILE;
        let profile: serde_json::Value = serde_json::from_str(raw).unwrap();
        catalog.sources[0].sha256 = profile["native_sha256"].as_str().unwrap().into();
        catalog.installation.native = catalog.sources[0].clone();
        catalog.sources[2].sha256 = profile["scene_sha256"].as_str().unwrap().into();
        let mut sources = catalog.sources.clone();
        sources.push(serde_json::from_value(profile["map_scene"].clone()).unwrap());
        sources.extend(
            serde_json::from_value::<Vec<SourceFile>>(profile["code_sources"].clone()).unwrap(),
        );
        doc["sources"] = serde_json::to_value(sources).unwrap();
        doc["schema"] = 2.into();
        doc["activation"] = profile["activation"].clone();
        doc["code_profile_sha256"] = format!("{:x}", Sha256::digest(raw.as_bytes())).into();
        doc["stages"][0]["disabled"] = serde_json::Value::Null;
        doc["stages"][0]["hierarchy_active"] = true.into();
        doc["stages"][0]["component_enabled"] = false.into();
        doc["stages"][0]["mode"] = 1.into();
        let decode =
            |doc: &serde_json::Value| Source::decode(&serde_json::to_vec(doc).unwrap(), &catalog);
        let source = decode(&doc).unwrap();
        assert!(source.stages[0].runs(1000, 1).unwrap());
        assert!(!source.stages[0].runs(1000, 2).unwrap());
        for (pointer, value) in [
            ("/schema", 1.into()),
            ("/activation", "unknown".into()),
            ("/code_profile_sha256", "c".repeat(64).into()),
            ("/sources/4/sha256", "c".repeat(64).into()),
            ("/stages/0/disabled", false.into()),
            ("/stages/0/component_enabled", serde_json::Value::Null),
            ("/stages/0/hierarchy_active", serde_json::Value::Null),
        ] {
            let mut changed = doc.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(decode(&changed).is_err(), "{pointer}");
        }
        doc["stages"][0]["hierarchy_active"] = false.into();
        doc["stages"][0]["component_enabled"] = true.into();
        assert!(!decode(&doc).unwrap().stages[0].runs(1000, 1).unwrap());
    }

    #[test]
    fn cache_skips_corruption_stale_tools_and_changed_installations_and_cancels() {
        let (catalog, mut doc) = fixture();
        let cache = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let publish = |raw: &[u8]| {
            let path = cache.path().join(format!("{:x}", Sha256::digest(raw)));
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("stages.json"), raw).unwrap();
        };
        publish(b"corrupt");
        publish(&serde_json::to_vec(&doc).unwrap());
        doc["extractor_sha256"] = crate::metadata_runtime::offshore_extractor_sha256().into();
        publish(&serde_json::to_vec(&doc).unwrap());
        assert!(
            Source::cached(cache.path(), root.path(), &catalog, || false)
                .unwrap()
                .is_none()
        );
        assert!(Source::cached(cache.path(), root.path(), &catalog, || true).is_err());
    }

    #[test]
    fn publication_repairs_corrupt_snapshots_and_preserves_them_on_cancellation() {
        let (catalog, doc) = fixture();
        let raw = serde_json::to_vec(&doc).unwrap();
        let source = Source::decode(&raw, &catalog).unwrap();
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("cache");
        let input = root.path().join("input.json");
        std::fs::write(&input, &raw).unwrap();
        let output = source.publish(&input, &cache, || false).unwrap();
        std::fs::write(&output, b"damaged").unwrap();
        assert!(source.publish(&input, &cache, || true).is_err());
        assert_eq!(std::fs::read(&output).unwrap(), b"damaged");
        assert_eq!(source.publish(&input, &cache, || false).unwrap(), output);
        assert_eq!(std::fs::read(&output).unwrap(), raw);
        std::fs::write(&input, b"changed after validation").unwrap();
        assert!(source.publish(&input, &cache, || false).is_err());
        assert_eq!(std::fs::read(&output).unwrap(), raw);
    }

    #[test]
    fn roster_uses_installed_selectors_and_priority_without_requiring_land_bounds() {
        let (mut catalog, doc) = fixture();
        let asset = |priority| crate::prefab_assets::Asset {
            priority,
            bounds: None,
            yaw: None,
            modifier_size: None,
            modifier_offset: None,
            texture_guids: Default::default(),
            splat_mask: None,
            modifier: None,
            modifier_absent: false,
            water_geometry: None,
            no_build_geometry: None,
            display_name: None,
        };
        catalog.entries.insert(
            "assets/bundled/prefabs/autospawn/future_offshore/a.prefab".into(),
            asset(Some(3)),
        );
        catalog.entries.insert(
            "assets/bundled/prefabs/autospawn/future_offshore/z.prefab".into(),
            asset(Some(4)),
        );
        catalog.entries.insert(
            "assets/bundled/prefabs/autospawn/unrelated/other.prefab".into(),
            asset(Some(5)),
        );
        let source = Source::decode(&serde_json::to_vec(&doc).unwrap(), &catalog).unwrap();
        let roster = source.roster(405, &catalog).unwrap();
        assert_eq!(roster.len(), 2);
        assert!(roster[0].path.ends_with("/z.prefab"));
        assert_eq!((roster[0].prefab.id, roster[0].prefab.score), (2, 625));
        catalog.entries.get_mut(&roster[0].path).unwrap().priority = None;
        assert!(source.roster(405, &catalog).is_err());
    }
    fn fixture() -> (Catalog, serde_json::Value) {
        let stages: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/monument-stage-source.json"))
                .unwrap();
        let sources: Vec<SourceFile> = serde_json::from_value(stages["sources"].clone()).unwrap();
        let catalog = Catalog {
            sha256: "a".repeat(64),
            installation: crate::installation::Installation {
                kind: crate::installation::InstallationKind::Client,
                native: sources[0].clone(),
                scene_manifest: Some(sources[1].clone()),
            },
            sources: sources[..3].to_vec(),
            entries: Default::default(),
        };
        let doc = serde_json::json!({"format":"papyrus-offshore-stage-capture","schema":1,"extractor_sha256":"b".repeat(64),
            "sources":sources[..4],"full_stage_execution_validated":false,"stages":[{
                "object_id":405,"order":74,"mode":3,"disabled":false,"folders":["assets/bundled/prefabs/autospawn/future_offshore/"],
                "minimum_world_size":0,"target_count":0,"minimum_distance":300,"maximum_distance":350,"spacing":1000}]});
        (catalog, doc)
    }
    #[test]
    fn source_data_controls_bands_counts_and_stage_gates() {
        let (catalog, mut doc) = fixture();
        let decode =
            |d: &serde_json::Value| Source::decode(&serde_json::to_vec(d).unwrap(), &catalog);
        let source = decode(&doc).unwrap();
        assert!(Source::prepare(Path::new("missing-game-root"), &catalog, || true).is_err());
        let stage = &source.stages[0];
        let policy = stage.policy(10, 10000).unwrap();
        assert_eq!(
            [
                policy.minimum_distance,
                policy.maximum_distance,
                policy.spacing
            ],
            [300., 350., 1000.]
        );
        assert_eq!(policy.target_count, 0);
        assert!(stage.runs(1000, 2).unwrap());
        for (pointer, value) in [
            ("/full_stage_execution_validated", serde_json::json!(true)),
            ("/sources/0/sha256", serde_json::json!("c".repeat(64))),
            (
                "/stages/0/folders",
                serde_json::json!(["assets/bundled/prefabs/autospawn/../escape/"]),
            ),
            ("/stages/0/maximum_distance", serde_json::json!(299)),
            ("/stages/0/unknown_rule", serde_json::json!(1)),
        ] {
            let mut changed = doc.clone();
            if pointer.ends_with("unknown_rule") {
                changed["stages"][0]["unknown_rule"] = value;
            } else {
                *changed.pointer_mut(pointer).unwrap() = value;
            }
            assert!(decode(&changed).is_err(), "{pointer}");
        }
        doc["stages"][0]["disabled"] = true.into();
        assert!(!decode(&doc).unwrap().stages[0].runs(1000, 2).unwrap());
    }
}
