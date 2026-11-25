//! Verified source snapshots of topology labels and procedural population filters.
use crate::{Project, installation::SourceFile, prefab_assets::Catalog, prefab_textures::Bindings};
use papyrus_terrain::{monument_composition::Composed, placement::Solution, spawn};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Read, path::Path};
const LIMIT: u64 = 4 * 1024 * 1024;
const CACHE: &str = "papyrus-spawn-cache";
const DOCUMENT: &str = "spawn.json";
const PROFILES: [&str; 3] = [
    include_str!("../../../compatibility/topology/client-0f703d43.json"),
    include_str!("../../../compatibility/topology/client-8dd4e235.json"),
    include_str!("../../../compatibility/topology/client-58f3263f.json"),
];
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    format: String,
    schema: u32,
    id: String,
    revision: u32,
    world_kind: String,
    native_sha256: String,
    scene_sha256: String,
    handler_prefab: String,
    reference_assembly_sha256: String,
    reference_gameplay_sha256: String,
    monument_clearance: f32,
    flags: BTreeMap<String, u32>,
    // Shared compatibility profiles also carry decoration query policy. Spawn
    // coverage does not perform collider queries; older snapshots omit this.
    #[serde(default, rename = "collider_sampling")]
    _collider_sampling: Option<ColliderSampling>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ColliderSampling {
    #[serde(rename = "backfaces")]
    _backfaces: bool,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Population {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    object_id: i64,
    terrain_filter_extracted: bool,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Runtime {
    placement_check_height: f32,
    radius_check_distance: f32,
    monument_clearance: f32,
    physics_validated: bool,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    format: String,
    schema: u32,
    adapter: String,
    sources: Vec<SourceFile>,
    extractor_sha256: String,
    profile: Profile,
    filters: Vec<spawn::Rule>,
    populations: Vec<Population>,
    issues: Vec<String>,
    runtime_checks: Runtime,
    #[serde(skip)]
    pub sha256: String,
}
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn profile_for(catalog: &Catalog) -> Result<Option<&'static str>, String> {
    let scene = catalog
        .sources
        .iter()
        .find(|source| source.relative_path == "Bundles/shared/assetscenes.bundle");
    for raw in PROFILES {
        let profile: Profile = serde_json::from_str(raw).map_err(|error| error.to_string())?;
        if catalog.installation.native.sha256 == profile.native_sha256
            && scene.is_some_and(|source| source.sha256 == profile.scene_sha256)
        {
            return Ok(Some(raw));
        }
    }
    Ok(None)
}

impl Source {
    pub fn topology_mask(&self) -> u32 {
        self.profile.flags.values().fold(0, |a, b| a | b)
    }
    /// Catalog and binding receipts must have passed their normal file verification.
    pub fn load(
        path: &Path,
        catalog: &Catalog,
        bindings: &Bindings,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        if cancelled() {
            return Err("Spawn source verification cancelled".into());
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > LIMIT {
            return Err("Spawn source exceeds 4 MiB".into());
        }
        let mut doc: Self =
            serde_json::from_slice(&bytes).map_err(|e| format!("Invalid spawn source: {e}"))?;
        doc.validate(catalog, bindings.sources())?;
        if cancelled() {
            return Err("Spawn source verification cancelled".into());
        }
        doc.sha256 = format!("{:x}", Sha256::digest(bytes));
        Ok(doc)
    }
    fn validate(&self, catalog: &Catalog, verified: &[SourceFile]) -> Result<(), String> {
        let p = &self.profile;
        let r = &self.runtime_checks;
        let mut expected = catalog.sources.clone();
        expected.push(
            verified
                .iter()
                .find(|f| f.relative_path == "Bundles/shared/content.bundle")
                .ok_or("Missing verified population source bundle")?
                .clone(),
        );
        if self.format != "papyrus-spawn-source"
            || self.schema != 1
            || self.adapter != "papyrus-spawn-filters-1"
            || self.sources != expected
            || p.format != "papyrus-topology-profile"
            || p.schema != 1
            || p.world_kind != "procedural"
            || !(1..=1024).contains(&p.revision)
            || p.id.is_empty()
            || p.id.len() > 128
            || p.native_sha256 != catalog.installation.native.sha256
            || self
                .sources
                .iter()
                .find(|f| f.relative_path == "Bundles/shared/assetscenes.bundle")
                .is_none_or(|f| f.sha256 != p.scene_sha256)
            || !hash(&p.reference_assembly_sha256)
            || !hash(&p.reference_gameplay_sha256)
            || !hash(&self.extractor_sha256)
            || !p.handler_prefab.starts_with("assets/")
            || !p.handler_prefab.ends_with(".prefab")
            || p.handler_prefab.len() > 512
            || p.handler_prefab.contains(['\\', '\0'])
            || p.handler_prefab
                .split('/')
                .any(|s| s.is_empty() || s == "..")
            || p.flags.is_empty()
            || p.flags.len() > 32
            || p.flags
                .iter()
                .any(|(name, v)| name.is_empty() || name.len() > 64 || v.count_ones() != 1)
            || p.flags
                .values()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != p.flags.len()
            || self.filters.is_empty()
            || self.filters.len() > 128
            || self
                .filters
                .iter()
                .filter(|f| f.kind == spawn::Kind::Character)
                .count()
                != 1
            || self.populations.len() > 256
            || self.populations.iter().any(|v| {
                v.name.is_empty()
                    || v.name.len() > 256
                    || v.kind.is_empty()
                    || v.kind.len() > 128
                    || v.object_id == 0
            })
            || self
                .populations
                .iter()
                .filter(|p| p.terrain_filter_extracted)
                .count()
                + 1
                != self.filters.len()
            || self.issues.len() > 256
            || self.issues.iter().any(|s| s.len() > 1024)
            || !p.monument_clearance.is_finite()
            || !(0. ..=500.).contains(&p.monument_clearance)
            || p.monument_clearance != r.monument_clearance
            || !r.placement_check_height.is_finite()
            || !(0. ..=1000.).contains(&r.placement_check_height)
            || !r.radius_check_distance.is_finite()
            || !(0. ..=1000.).contains(&r.radius_check_distance)
            || r.physics_validated
        {
            return Err(
                "Spawn source schema, receipts, topology profile or coverage mismatch".into(),
            );
        }
        for rule in &self.filters {
            rule.validate()?;
        }
        Ok(())
    }
    pub fn cached(
        catalog: &Catalog,
        bindings: &Bindings,
        sha: &str,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        if !hash(sha) {
            return Err("Invalid pinned topology source hash".into());
        }
        let path = crate::client_cache::pinned_document(
            &crate::storage::local_data()?, CACHE, sha, DOCUMENT,
        )?;
        Self::load_exact(&path, catalog, bindings, sha, cancelled)
    }

    fn load_exact(
        path: &Path,
        catalog: &Catalog,
        bindings: &Bindings,
        sha: &str,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        let source = Self::load(path, catalog, bindings, cancelled)?;
        if source.sha256 != sha {
            return Err("Pinned topology source changed".into());
        }
        Ok(source)
    }
    pub fn discover(
        catalog: &Catalog,
        bindings: &Bindings,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        Self::discover_in(&crate::storage::local_data()?, catalog, bindings, cancelled)
    }

    fn discover_in(
        local: &Path,
        catalog: &Catalog,
        bindings: &Bindings,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        let root = local.join(CACHE);
        if !root.is_dir() {
            return Ok(None);
        }
        let mut best: Option<Self> = None;
        for (i, entry) in std::fs::read_dir(root)
            .map_err(|e| e.to_string())?
            .enumerate()
        {
            if i >= 256 {
                return Err("Too many spawn source snapshots".into());
            }
            if cancelled() {
                return Ok(None);
            }
            let directory = entry.map_err(|e| e.to_string())?.path();
            if let Ok(doc) =
                Self::load(&directory.join(DOCUMENT), catalog, bindings, &cancelled)
                && directory.file_name().and_then(|s| s.to_str()) == Some(&doc.sha256)
                && best.as_ref().is_none_or(|old| {
                    (doc.profile.revision, &doc.sha256) > (old.profile.revision, &old.sha256)
                })
            {
                best = Some(doc);
            }
        }
        Ok(best)
    }

    pub(crate) fn prepare(
        root: &Path,
        catalog: &Catalog,
        bindings: &Bindings,
        pin: Option<&str>,
        cancelled: impl Fn() -> bool,
    ) -> Result<(), String> {
        use crate::metadata_runtime::{self, Script};

        let local = crate::storage::local_data()?;
        if cancelled() {
            return Err("Topology preparation cancelled".into());
        }
        if let Some(sha) = pin {
            let path = crate::client_cache::pinned_document(&local, CACHE, sha, DOCUMENT)?;
            if path.try_exists().map_err(|error| error.to_string())? {
                Self::load_exact(&path, catalog, bindings, sha, &cancelled)?;
                return Ok(());
            }
        } else if Self::discover_in(&local, catalog, bindings, &cancelled)?.is_some() {
            return Ok(());
        }
        let missing_pin = || {
            "Matching topology layers are missing. Restore the original Mapper cache for this recipe.".to_string()
        };
        let Some(profile) = profile_for(catalog)? else {
            return Err(if pin.is_some() {
                missing_pin()
            } else {
                "Topology metadata is unavailable for this Rust installation".into()
            });
        };
        if cancelled() {
            return Err("Topology preparation cancelled".into());
        }
        std::fs::create_dir_all(&local).map_err(|error| error.to_string())?;
        let staging = tempfile::tempdir_in(&local).map_err(|error| error.to_string())?;
        let profile_path = staging.path().join("topology.json");
        let cache = staging.path().join("capture");
        std::fs::write(&profile_path, profile).map_err(|error| error.to_string())?;
        let arguments = [
            "--rust-root".into(),
            root.as_os_str().to_owned(),
            "--profile".into(),
            profile_path.into_os_string(),
            "--cache-root".into(),
            cache.as_os_str().to_owned(),
        ];
        if !metadata_runtime::run(Script::Spawn, &arguments, &cancelled)? {
            return Err("Topology preparation cancelled".into());
        }
        let document = metadata_runtime::document(&cache, DOCUMENT)?;
        let source = Self::load(&document, catalog, bindings, &cancelled)?;
        if pin.is_some_and(|sha| sha != source.sha256) {
            return Err(missing_pin());
        }
        if cancelled() {
            return Err("Topology preparation cancelled".into());
        }
        metadata_runtime::publish(&document, &local.join(CACHE), &source.sha256, LIMIT)?;
        Ok(())
    }

    pub(crate) fn enable_jungle_beaches(
        &self,
        layers: &mut papyrus_terrain::native_layers::Layers,
        cancelled: &impl Fn() -> bool,
    ) -> Result<bool, String> {
        let rule = self.filters.iter().find(|rule| rule.kind == spawn::Kind::Character)
            .ok_or("Missing character spawn filter")?;
        if rule.filter.biome_type & 16 != 0 {
            return Ok(!cancelled());
        }
        let compatible = (0..4).find(|&channel| rule.filter.biome_type & (1 << channel) != 0)
            .ok_or("No compatible native biome for jungle beach spawns")?;
        let beach = *self.profile.flags.get("Beach").ok_or("Missing Beach topology role")?;
        let cells = layers.topology.len();
        if layers.biome.len() != 5 * cells {
            return Err("Jungle beach spawns require five native biome channels".into());
        }
        let transferred = ((rule.cutoff * 255.).floor() as u8).saturating_add(1).max(16);
        for (index, &topology) in layers.topology.iter().enumerate() {
            if index % 16384 == 0 && cancelled() {
                return Ok(false);
            }
            let topology = topology as u32;
            if topology & beach == 0 || !rule.filter.topology_matches(topology) {
                continue;
            }
            let jungle = 4 * cells + index;
            let compatible = compatible * cells + index;
            if layers.biome[jungle] < 128 || layers.biome[compatible] >= transferred {
                continue;
            }
            let amount = (transferred - layers.biome[compatible]).min(layers.biome[jungle]);
            layers.biome[jungle] -= amount;
            layers.biome[compatible] += amount;
        }
        Ok(!cancelled())
    }

    pub fn audit(
        &self,
        project: &Project,
        solution: &Solution,
        catalog: &Catalog,
        maps: &Composed,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<spawn::Audit>, String> {
        if project.installation.as_ref() != Some(&catalog.installation)
            || maps.layers.size != project.size
            || maps.layers.terrain_seed != project.seeds[0].value
            || maps.layers.biome_seed != project.seeds[1].value
        {
            return Err("Spawn audit terrain/source mismatch".into());
        }
        let mut obstacles = Vec::new();
        for outcome in &solution.outcomes {
            let Some(p) = &outcome.position else {
                continue;
            };
            let target = project
                .targets
                .iter()
                .find(|t| t.instance_id == outcome.id)
                .ok_or("Unknown spawn-clearance target")?;
            let path = target
                .prefab_path
                .as_deref()
                .ok_or("Missing spawn-clearance prefab")?;
            let bounds = catalog
                .entries
                .get(path)
                .and_then(|a| a.bounds.as_ref())
                .ok_or("Missing authored spawn-clearance bounds")?;
            obstacles.push(spawn::Obstacle {
                center: [p.x, p.z],
                extent: bounds.horizontal_extent(),
                yaw: p.yaw,
            });
        }
        let flags: Vec<_> = self
            .profile
            .flags
            .iter()
            .map(|(name, mask)| (name.clone(), *mask))
            .collect();
        let result = spawn::audit(
            &maps.layers,
            &maps.alpha,
            &obstacles,
            spawn::Definition {
                source_sha256: &self.sha256,
                flags: &flags,
                rules: &self.filters,
                clearance: self.runtime_checks.monument_clearance,
            },
            &cancelled,
        )?;
        let Some(mut report) = result else {
            return Ok(None);
        };
        if let Some(blocking) = maps
            .decoration
            .as_ref()
            .and_then(|d| d.terrain_placement.as_ref())
            .and_then(|p| p.blocking.as_ref())
            && !report.exclude_decoration(&blocking.raster, &cancelled)?
        {
            return Ok(None);
        }
        report.source_issues = self.issues.clone();
        report.fixed_point_populations = self
            .populations
            .iter()
            .filter(|p| !p.terrain_filter_extracted)
            .map(|p| p.name.clone())
            .collect();
        Ok(Some(report))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::installation::{Installation, InstallationKind};
    fn fixture() -> (Source, Catalog, Vec<SourceFile>) {
        let file = |path: &str, c: char| SourceFile {
            relative_path: path.into(),
            sha256: c.to_string().repeat(64),
            bytes: 1,
        };
        let sources = vec![
            file("RustClient_Data/Plugins/x86_64/RustNative.dll", 'a'),
            file("Bundles/AssetSceneManifest.json", 'b'),
            file("Bundles/shared/assetscenes.bundle", 'c'),
        ];
        let catalog = Catalog {
            sha256: "d".repeat(64),
            installation: Installation {
                kind: InstallationKind::Client,
                native: sources[0].clone(),
                scene_manifest: Some(sources[1].clone()),
            },
            sources: sources.clone(),
            entries: Default::default(),
        };
        let mut verified = sources;
        verified.push(file("Bundles/shared/content.bundle", 'e'));
        let mut profile: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/topology/client-0f703d43.json"
        ))
        .unwrap();
        profile["native_sha256"] = "a".repeat(64).into();
        profile["scene_sha256"] = "c".repeat(64).into();
        let doc = serde_json::json!({"format":"papyrus-spawn-source","schema":1,"adapter":"papyrus-spawn-filters-1","sources":verified,"extractor_sha256":"f".repeat(64),"profile":profile,
            "filters":[{"name":"CharacterSpawn","kind":"character","filter":{"SplatType":-1,"BiomeType":-1,"TopologyAny":-1,"TopologyAll":0,"TopologyNot":0},"cutoff":0.,"radius":0.}],
            "populations":[],"issues":[],"runtime_checks":{"placement_check_height":50.,"radius_check_distance":0.5,"monument_clearance":50.,"physics_validated":false}});
        (serde_json::from_value(doc).unwrap(), catalog, verified)
    }
    #[test]
    fn preparation_profiles_bind_to_both_the_native_library_and_scene_bundle() {
        let (_, mut catalog, _) = fixture();
        assert!(profile_for(&catalog).unwrap().is_none());
        for raw in PROFILES {
            let profile: Profile = serde_json::from_str(raw).unwrap();
            catalog.installation.native.sha256 = profile.native_sha256;
            assert!(profile_for(&catalog).unwrap().is_none());
            catalog.sources[2].sha256 = profile.scene_sha256;
            assert_eq!(profile_for(&catalog).unwrap(), Some(raw));
            catalog.sources[2].sha256 = "0".repeat(64);
        }
    }
    #[test]
    fn source_requires_exact_build_receipts_and_honest_filter_coverage() {
        let (mut src, catalog, verified) = fixture();
        src.validate(&catalog, &verified).unwrap();
        src.sources[3].sha256 = "0".repeat(64);
        assert!(src.validate(&catalog, &verified).is_err());
        let (mut src, _, _) = fixture();
        src.profile.native_sha256 = "0".repeat(64);
        assert!(src.validate(&catalog, &verified).is_err());
        let (mut src, _, _) = fixture();
        src.runtime_checks.physics_validated = true;
        assert!(src.validate(&catalog, &verified).is_err());
        let (mut src, _, _) = fixture();
        src.populations.push(Population {
            name: "Trees".into(),
            kind: "DensitySpawnPopulation".into(),
            object_id: 1,
            terrain_filter_extracted: true,
        });
        assert!(src.validate(&catalog, &verified).is_err());
        let (mut src, _, _) = fixture();
        src.profile.flags.insert("Conflicting label".into(), 1);
        assert!(src.validate(&catalog, &verified).is_err());
    }
}
