//! Portable data for an explicitly declared native ABI. Profiles contain no executable code.
use crate::installation::Installation;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path, sync::OnceLock};

pub const ABI: &str = "rust-native-five-biomes-1";
pub const MAX_BYTES: u64 = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub loot_angle_millidegrees: u32,
    pub biome_angle_millidegrees: u32,
    /// Integer parts per 10,000 keep portable identities exact, without float equality.
    pub terrain_weights: [u32; 3],
    pub biome_weights: [u32; 4],
    pub jungle_weight: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerrainOverrides {
    pub tier_angle: u32,
    pub tier_weights: [u32; 3],
}

impl TerrainOverrides {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            tier_angle: settings.loot_angle_millidegrees,
            tier_weights: settings.terrain_weights,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.tier_angle > 360_000 || !normalized(&self.tier_weights) {
            return Err("Terrain tier rotation or weights are outside the supported range".into());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BiomeOverrides {
    pub angle: u32,
    pub weights: [u32; 4],
    pub jungle_weight: u32,
}

impl BiomeOverrides {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            angle: settings.biome_angle_millidegrees,
            weights: settings.biome_weights,
            jungle_weight: settings.jungle_weight,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.angle > 360_000 || !normalized(&self.weights) || self.jungle_weight > 10_000 {
            return Err("Biome rotation or weights are outside the supported range".into());
        }
        Ok(())
    }
}

fn normalized(weights: &[u32]) -> bool {
    weights.iter().all(|weight| *weight <= 10_000) && weights.iter().sum::<u32>() == 10_000
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub format: String,
    pub schema: u32,
    pub id: String,
    pub abi: String,
    pub native_sha256: String,
    pub settings: Settings,
}
impl Profile {
    pub fn validate(&self) -> Result<(), String> {
        let s = &self.settings;
        if self.format != "papyrus-native-profile" || self.schema != 1 || self.abi != ABI {
            return Err("Unsupported native compatibility profile schema or ABI family".into());
        }
        if self.id.is_empty()
            || self.id.len() > 64
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || self.native_sha256.len() != 64
            || !self
                .native_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(
                "Invalid native compatibility profile identifier or DLL fingerprint".into(),
            );
        }
        if s.loot_angle_millidegrees > 360_000
            || s.biome_angle_millidegrees > 360_000
            || s.jungle_weight > 10_000
            || !normalized(&s.terrain_weights)
            || !normalized(&s.biome_weights)
        {
            return Err("Native profile angles or normalized weights are outside the supported ABI contract".into());
        }
        Ok(())
    }
    pub fn matches(&self, source: &Installation) -> Result<(), String> {
        self.validate()?;
        source.validate()?;
        if self.native_sha256 != source.native.sha256 {
            return Err(
                "Native compatibility profile belongs to a different RustNative.dll".into(),
            );
        }
        Ok(())
    }
    pub fn bytes(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        serde_json::to_vec(self).map_err(|e| e.to_string())
    }
    pub fn sha256(&self) -> Result<String, String> {
        Ok(format!("{:x}", Sha256::digest(self.bytes()?)))
    }
    pub fn load(path: &Path) -> Result<Self, String> {
        let mut bytes = Vec::new();
        File::open(path)
            .map_err(|e| e.to_string())?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("Native profile exceeds 16 KiB".into());
        }
        let profile: Self = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        profile.validate()?;
        Ok(profile)
    }
}
/// Immutable legacy defaults, retained only for recipes without an explicit profile.
pub fn legacy(source: &Installation) -> Option<Profile> {
    legacy_profile(source).cloned()
}

pub(crate) fn legacy_profile(source: &Installation) -> Option<&'static Profile> {
    static PROFILES: OnceLock<Vec<Profile>> = OnceLock::new();
    PROFILES
        .get_or_init(|| {
            [
                include_str!("../../../compatibility/native/client-0f703d43.json"),
                include_str!("../../../compatibility/native/client-1dc98ce9.json"),
                include_str!("../../../compatibility/native/client-8dd4e235.json"),
                include_str!("../../../compatibility/native/client-58f3263f.json"),
            ]
            .into_iter()
            .filter_map(|text| serde_json::from_str(text).ok())
            .collect()
        })
        .iter()
        .find(|profile| {
            profile.native_sha256 == source.native.sha256 && profile.matches(source).is_ok()
        })
}
pub fn resolve(key: &crate::TerrainKey) -> Result<Profile, String> {
    let source = key
        .source
        .as_ref()
        .ok_or("Missing native source identity")?;
    for_source(source, key.native_profile.as_ref())
}
pub fn for_source(source: &Installation, selected: Option<&Profile>) -> Result<Profile, String> {
    if let Some(profile) = selected {
        profile.matches(source)?;
        Ok(profile.clone())
    } else {
        legacy(source).ok_or_else(|| "No matching native compatibility profile. Import a tested profile for this DLL and a supported ABI family.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn current_client_terrain_profile_requires_its_exact_native_library() {
        for (native_hash, id) in [
            (
                "8dd4e23570729ddbd72844bdb8a5e04bfa671612b3e7d34841d17d2353f7015c",
                "client-8dd4e235-preview-1",
            ),
            (
                "58f3263fd5cc1ab31942e14b902e52ff952b65fa08896226d15181a01b486750",
                "client-58f3263f-preview-1",
            ),
        ] {
            let (mut source, _) = fixture();
            source.native.sha256 = native_hash.into();
            let profile = for_source(&source, None).unwrap();
            assert_eq!(profile.id, id);
            profile.validate().unwrap();
            source.native.sha256 = "c".repeat(64);
            assert!(for_source(&source, None).is_err());
            assert!(for_source(&source, Some(&profile)).is_err());
        }
    }
    use crate::{
        Project,
        installation::{InstallationKind, SourceFile},
        native_preview,
    };
    fn fixture() -> (Installation, Profile) {
        let profile: Profile = serde_json::from_str(include_str!(
            "../../../compatibility/native/client-0f703d43.json"
        ))
        .unwrap();
        let source = Installation {
            kind: InstallationKind::Client,
            native: SourceFile {
                relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                sha256: profile.native_sha256.clone(),
                bytes: 1,
            },
            scene_manifest: None,
        };
        (source, profile)
    }
    #[test]
    fn recipe_overrides_reach_native_terrain_and_restore_the_original_cache_key() {
        let (source, baseline) = fixture();
        let mut project = Project {
            installation: Some(source),
            preview_engine: native_preview::VERSION.into(),
            ..Default::default()
        };
        let defaults = project.terrain_key();
        project.terrain_overrides = Some(TerrainOverrides::from_settings(&baseline.settings));
        project.biome_overrides = Some(BiomeOverrides::from_settings(&baseline.settings));
        assert_eq!(project.terrain_key(), defaults);
        assert!(serde_json::to_value(&project).unwrap().get("biome_overrides").is_some());

        project.terrain_overrides = Some(TerrainOverrides {
            tier_angle: 90_000,
            tier_weights: [1000, 4000, 5000],
        });
        project.biome_overrides = Some(BiomeOverrides {
            angle: 180_000,
            weights: [2500, 2500, 500, 4500],
            jungle_weight: 3000,
        });
        project.validate().unwrap();
        let serialized = serde_json::to_vec(&project).unwrap();
        let mut restored: Project = serde_json::from_slice(&serialized).unwrap();
        assert_eq!(restored, project);
        let key = restored.terrain_key();
        assert_ne!(key, defaults);
        let settings = resolve(&key).unwrap().settings;
        assert_eq!(TerrainOverrides::from_settings(&settings), project.terrain_overrides.unwrap());
        assert_eq!(BiomeOverrides::from_settings(&settings), project.biome_overrides.unwrap());
        restored.terrain_overrides = None;
        assert_eq!(resolve(&restored.terrain_key()).unwrap().settings.terrain_weights, baseline.settings.terrain_weights);
        restored.biome_overrides = None;
        assert_eq!(restored.terrain_key(), defaults);
        project.biome_overrides.as_mut().unwrap().weights = [u32::MAX; 4];
        assert!(project.validate().is_err());
    }
    #[test]
    fn portable_profile_pins_identity_and_preserves_legacy_key_serialization() {
        let (source, profile) = fixture();
        assert_eq!(legacy(&source).unwrap(), profile);
        let mut project = Project {
            installation: Some(source.clone()),
            preview_engine: native_preview::VERSION.into(),
            ..Default::default()
        };
        let old = project.clone();
        let old_key = project.terrain_key();
        assert!(
            serde_json::to_value(&old_key)
                .unwrap()
                .get("native_profile")
                .is_none()
        );
        assert!(
            serde_json::to_value(&project)
                .unwrap()
                .get("native_profile")
                .is_none()
        );
        project.native_profile = Some(profile.clone());
        project.validate().unwrap();
        assert_ne!(project.terrain_key(), old_key);
        let pinned = project.terrain_key();
        project
            .native_profile
            .as_mut()
            .unwrap()
            .settings
            .biome_angle_millidegrees = 180_000;
        assert_ne!(project.terrain_key(), pinned);
        assert_ne!(
            project.native_profile.as_ref().unwrap().sha256().unwrap(),
            profile.sha256().unwrap()
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("portable.rec");
        crate::profile::Profile::new("Portable native settings".into(), project.clone())
            .save(&path)
            .unwrap();
        assert_eq!(crate::profile::Profile::load(&path).unwrap().recipe, {
            let mut current = project.clone();
            crate::generation_defaults::apply(&mut current);
            current
        });
        let mut history = crate::History::default();
        history.record(old.clone(), &project);
        let changed = project.clone();
        history.undo(&mut project);
        assert_eq!(project, old);
        history.redo(&mut project);
        assert_eq!(project, changed);
        let native_key = project.terrain_key();
        project.preview_engine = papyrus_terrain::landscape::VERSION.into();
        assert!(project.terrain_key().native_profile.is_none());
        assert!(project.native_profile.is_some());
        assert!(resolve(&native_key).is_ok());
        project.installation.as_mut().unwrap().native.sha256 = "a".repeat(64);
        assert!(project.validate().is_err());
        let mut unknown = source;
        unknown.native.sha256 = "b".repeat(64);
        assert!(legacy(&unknown).is_none());
        assert!(for_source(&unknown, Some(&profile)).is_err());
    }
    #[test]
    fn malformed_profiles_and_unknown_abis_fail_without_normalizing_or_guessing() {
        let (_, original) = fixture();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&original).unwrap()).unwrap();
        assert_eq!(
            Profile::load(&path).unwrap().sha256().unwrap(),
            original.sha256().unwrap()
        );
        for edit in 0..8 {
            let mut p = original.clone();
            match edit {
                0 => p.abi = "future-signature".into(),
                1 => p.schema = 2,
                2 => p.settings.terrain_weights = [u32::MAX; 3],
                3 => p.settings.biome_weights = [0; 4],
                4 => p.settings.jungle_weight = 10_001,
                5 => p.settings.loot_angle_millidegrees = 360_001,
                6 => p.native_sha256 = "Z".repeat(64),
                _ => p.id = "../profile".into(),
            }
            assert!(p.validate().is_err());
        }
        let mut value = serde_json::to_value(&original).unwrap();
        value["settings"]
            .as_object_mut()
            .unwrap()
            .remove("jungle_weight");
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(Profile::load(&path).is_err());
        value = serde_json::to_value(&original).unwrap();
        value["executable"] = "no.exe".into();
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(Profile::load(&path).is_err());
        std::fs::write(&path, vec![b' '; MAX_BYTES as usize + 1]).unwrap();
        assert!(Profile::load(&path).is_err());
    }
    #[test]
    fn legacy_default_profiles_are_frozen_for_existing_cache_and_recipe_versions() {
        for (data, expected) in [
            (
                include_str!("../../../compatibility/native/client-0f703d43.json"),
                "1c6558c477b203d4772101d429a66567783069bb4d50dce237695e73d5013dc7",
            ),
            (
                include_str!("../../../compatibility/native/client-1dc98ce9.json"),
                "acd20239e43eb9ba90795ec79fef67a29d5debdc52f2a0953c7390638bdb90e1",
            ),
        ] {
            let profile: Profile = serde_json::from_str(data).unwrap();
            assert_eq!(profile.sha256().unwrap(), expected);
        }
    }
}
