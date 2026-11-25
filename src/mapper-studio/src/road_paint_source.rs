//! Data-only road paint policy for a verified roadside installation profile.
//! This does not activate final painting or implement the terrain-height pass.
use crate::roadside_source::{PaintStage, Source};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
const CURRENT_PROFILE: &str =
    include_str!("../../../compatibility/infrastructure/client-8dd4e235-road-paint.json");
const PROFILE: &str =
    include_str!("../../../compatibility/infrastructure/client-0f703d43-road-paint.json");
const PROFILES: [(&str, &str, &str); 3] = [
    (
        PROFILE,
        "client-0f703d43-road-path-paint-1",
        include_str!("../../../compatibility/monuments/client-0f703d43-roadside.json"),
    ),
    (
        CURRENT_PROFILE,
        "client-8dd4e235-road-path-paint-1",
        include_str!("../../../compatibility/monuments/client-8dd4e235-roadside.json"),
    ),
    (
        include_str!("../../../compatibility/infrastructure/client-58f3263f-road-paint.json"),
        "client-58f3263f-road-path-paint-1",
        include_str!("../../../compatibility/monuments/client-58f3263f-roadside.json"),
    ),
];
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    format: String,
    schema: u32,
    pub id: String,
    roadside_profile_sha256: String,
    reference_server_assembly_sha256: String,
    pub height_origin: f32,
    pub vertical_span: f32,
    pub policy: papyrus_terrain::road_paint::Policy,
    pub post_topology: papyrus_terrain::road_topology::Policy,
}
impl Profile {
    /// Recover only the exact supported policy already receipted by a snapshot.
    /// This does not prepare source assets or authorize generation on its own.
    pub(crate) fn from_receipt(sha256: &str) -> Result<Self, String> {
        for profile in Self::known()? {
            if profile.sha256()? == sha256 {
                return Ok(profile);
            }
        }
        Err("Unsupported saved road paint policy receipt".into())
    }
    fn known() -> Result<Vec<Self>, String> {
        let mut profiles = Vec::new();
        for (raw, id, parent) in PROFILES {
            let profile: Self = serde_json::from_str(raw).map_err(|e| e.to_string())?;
            if profile.format != "papyrus-road-path-paint-profile"
                || profile.schema != 1
                || profile.id != id
                || !crate::source_profile::matches(parent, &profile.roadside_profile_sha256)
                || profile.reference_server_assembly_sha256.len() != 64
                || !profile
                    .reference_server_assembly_sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                || !profile.height_origin.is_finite()
                || profile.height_origin.abs() > 10000.
                || !profile.vertical_span.is_finite()
                || !(1. ..=20000.).contains(&profile.vertical_span)
            {
                return Err("Invalid bundled road paint compatibility profile".into());
            }
            profile.policy.validate()?;
            profile.post_topology.validate()?;
            profiles.push(profile);
        }
        Ok(profiles)
    }
    pub fn for_source(source: &Source) -> Result<Option<Self>, String> {
        let Some(profile) = Self::known()?
            .into_iter()
            .zip(PROFILES)
            .find(|(_, (_, _, parent))| crate::source_profile::matches(parent, source.profile_sha256()))
            .map(|(profile, _)| profile)
        else {
            return Ok(None);
        };
        let context = papyrus_terrain::monument_stage::Context {
            realm_mask: 2,
            cached: false,
            nexus: false,
        };
        for stage in [
            PaintStage::Topology,
            PaintStage::Terrain,
            PaintStage::Texture,
        ] {
            if !source.paint_stage_runs(stage, context)? {
                return Err(
                    "Captured road painting sequence is disabled for the server realm".into(),
                );
            }
        }
        Ok(Some(profile))
    }
    pub fn sha256(&self) -> Result<String, String> {
        self.policy.validate()?;
        self.post_topology.validate()?;
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(self).map_err(|e| e.to_string())?)
        ))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profiles_keep_versioned_normals_and_bind_their_own_terrain_receipts() {
        let profiles = Profile::known().unwrap();
        assert_eq!(profiles.len(), 3);
        for mut profile in profiles {
            let current = profile.id != "client-0f703d43-road-path-paint-1";
            assert_eq!(profile.post_topology.normal_sampling.is_some(), current);
            let expected = match profile.id.as_str() {
                "client-0f703d43-road-path-paint-1" => "fe2abc8739e73e0dd4ede67176983baa5a674e02cf25772827d5de376720b66a",
                "client-8dd4e235-road-path-paint-1" => "dfe2cb140cd2e711dae5e238ef094620b510c2e427f77c23226dbd54c35f8385",
                "client-58f3263f-road-path-paint-1" => "7e593c2df0761ddbd78eecc78e322fc5a43301279cdd20c719e8384ab62af3c7",
                other => panic!("Unexpected paint profile {other}"),
            };
            assert_eq!(profile.sha256().unwrap(), expected);
            assert_eq!(Profile::from_receipt(expected).unwrap().id, profile.id);
            let terrain = crate::road_terrain_source::Profile::for_paint(&profile)
                .unwrap()
                .unwrap();
            assert_eq!(terrain.families.len(), profile.policy.families.len());
            assert_eq!(terrain.policy.road_fades, [1., 0.5]);
            assert!(terrain.id.starts_with(profile.id.split("-road-").next().unwrap()));
            profile.policy.families[0].width += 1.;
            assert!(
                crate::road_terrain_source::Profile::for_paint(&profile)
                    .unwrap()
                    .is_none()
            );
        }
        assert!(Profile::from_receipt(&"f".repeat(64)).is_err());
    }
    #[test]
    fn profile_keeps_distinct_captured_paved_and_dirt_shapes_and_parent_binding() {
        let p = Profile::known().unwrap().remove(0);
        assert_eq!(
            p.policy
                .families
                .iter()
                .map(|f| (f.hierarchy, f.width, f.padding[0], f.splat_channel))
                .collect::<Vec<_>>(),
            vec![
                (0, 12., 1., Some(7)),
                (1, 10., 1., Some(7)),
                (2, 4., 0.4, Some(0))
            ]
        );
        assert_eq!(p.policy.noise_unsigned_bias, 1.);
        assert_eq!(p.policy.noise_frequency, 0.005);
        assert_eq!(p.post_topology.radius_cells, 6);
        assert_eq!(p.sha256().unwrap().len(), 64);
        let mut changed = Profile::known().unwrap().remove(0);
        changed.policy.families[0].width += 1.;
        assert_ne!(changed.sha256().unwrap(), p.sha256().unwrap());
        let mut value: serde_json::Value = serde_json::from_str(PROFILE).unwrap();
        value["unexpected"] = true.into();
        assert!(serde_json::from_value::<Profile>(value).is_err());
    }
}
