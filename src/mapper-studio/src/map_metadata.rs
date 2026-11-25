//! Optional metadata sidecar. Never changes game map bytes or assumes unknown-field tolerance.
use crate::{Project, profile};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Monument {
    pub instance_id: u64,
    pub prefab: String,
    pub position: [f32; 3],
    pub yaw: f32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub format: String,
    pub schema: u32,
    pub map_sha256: String,
    pub map_bytes: u64,
    pub game_build: Option<String>,
    pub protocol: Option<u32>,
    pub recipe: Project,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<crate::provenance::Provenance>,
    /// Actual exported placements, never merely requested target locations.
    pub monuments: Vec<Monument>,
}
fn digest(mut map: impl Read) -> Result<(String, u64), String> {
    let mut sha = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 65536];
    loop {
        let n = map.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > 512 * 1024 * 1024 {
            return Err("Map exceeds 512 MiB metadata hashing limit".into());
        }
        sha.update(&buffer[..n]);
    }
    Ok((format!("{:x}", sha.finalize()), total))
}
impl Metadata {
    pub fn for_map(
        map: impl Read,
        recipe: Project,
        monuments: Vec<Monument>,
    ) -> Result<Self, String> {
        let (map_sha256, map_bytes) = digest(map)?;
        let value = Self {
            format: "papyrus-map-metadata".into(),
            schema: 2,
            map_sha256,
            map_bytes,
            game_build: None,
            protocol: None,
            provenance: Some(crate::provenance::Provenance::for_recipe(&recipe, &[])),
            recipe,
            monuments,
        };
        value.validate()?;
        Ok(value)
    }
    pub fn declare_edits(
        &mut self,
        edited: &[crate::provenance::EditedStage],
    ) -> Result<(), String> {
        self.schema = 2;
        self.provenance = Some(crate::provenance::Provenance::for_recipe(
            &self.recipe,
            edited,
        ));
        self.validate()
    }
    pub fn validate(&self) -> Result<(), String> {
        self.recipe.validate()?;
        if let Some(provenance) = &self.provenance {
            provenance.validate(&self.recipe)?;
        }
        let mut ids = std::collections::HashSet::new();
        if self.format != "papyrus-map-metadata"
            || ![1, 2].contains(&self.schema)
            || (self.schema == 2 && self.provenance.is_none())
            || (self.schema == 1 && self.provenance.is_some())
            || self.map_sha256.len() != 64
            || !self
                .map_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || self.map_bytes > 512 * 1024 * 1024
            || self
                .game_build
                .as_ref()
                .is_some_and(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
            || self.monuments.len() > 65536
            || self.monuments.iter().any(|m| {
                m.instance_id == 0
                    || !ids.insert(m.instance_id)
                    || m.prefab.is_empty()
                    || m.prefab.len() > 512
                    || m.prefab.chars().any(char::is_control)
                    || m.position
                        .iter()
                        .any(|v| !v.is_finite() || v.abs() > 1_000_000.)
                    || !m.yaw.is_finite()
                    || !(0. ..360.).contains(&m.yaw)
            })
        {
            return Err("Invalid map metadata".into());
        }
        Ok(())
    }
    pub fn verify_map(&self, map: impl Read) -> Result<(), String> {
        self.validate()?;
        let (sha, bytes) = digest(map)?;
        if sha != self.map_sha256 || bytes != self.map_bytes {
            return Err("Metadata belongs to a different map".into());
        }
        Ok(())
    }
    pub fn save(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        profile::write_json(path, self)
    }
    pub fn load(path: &Path) -> Result<Self, String> {
        let value: Self = profile::read_json(path)?;
        value.validate()?;
        Ok(value)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_sidecars_load_and_explicit_edits_keep_original_seeds() {
        let map = b"map bytes";
        let mut metadata = Metadata::for_map(map.as_slice(), Project::default(), vec![]).unwrap();
        metadata.schema = 1;
        metadata.provenance = None;
        metadata.validate().unwrap();
        let raw = serde_json::to_vec(&metadata).unwrap();
        let old: Metadata = serde_json::from_slice(&raw).unwrap();
        old.validate().unwrap();
        metadata
            .declare_edits(&[crate::provenance::EditedStage::Monuments])
            .unwrap();
        assert_eq!(metadata.provenance.as_ref().unwrap().seeds[4].seed, 0);
        assert_eq!(
            metadata.provenance.as_ref().unwrap().seeds[4].generation_seed,
            Some(metadata.recipe.seeds[4].value)
        );
        metadata.verify_map(map.as_slice()).unwrap();
    }
    #[test]
    fn sidecar_roundtrip_detects_changed_map_and_keeps_version_unknown() {
        let map = b"map bytes";
        let metadata = Metadata::for_map(map.as_slice(), Project::default(), vec![]).unwrap();
        assert_eq!(metadata.game_build, None);
        assert_eq!(metadata.protocol, None);
        metadata.verify_map(map.as_slice()).unwrap();
        assert!(metadata.verify_map(b"wrong map".as_slice()).is_err());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("world.map.mapper.json");
        metadata.save(&path).unwrap();
        assert_eq!(metadata, Metadata::load(&path).unwrap());
    }
}
