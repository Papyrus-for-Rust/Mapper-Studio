//! Road terrain policy bound to an installation's captured road paint policy.
use crate::road_paint_source;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
const CURRENT_DATA: &str =
    include_str!("../../../compatibility/infrastructure/client-8dd4e235-road-terrain.json");
const DATA: &str =
    include_str!("../../../compatibility/infrastructure/client-0f703d43-road-terrain.json");
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Family {
    pub hierarchy: u8,
    pub fade: [f32; 2],
    pub terrain_offset: f32,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    format: String,
    schema: u32,
    pub id: String,
    road_paint_profile_sha256: String,
    reference_server_assembly_sha256: String,
    pub water_no_hit_level: f32,
    pub policy: papyrus_terrain::road_terrain::Policy,
    pub families: Vec<Family>,
}
impl Profile {
    /// GenerateRoadTerrain iterates the whole Road list (ring, paved and dirt).
    /// This policy is bound to that captured pass, not a routing-family option.
    pub(crate) fn water_policy(&self) -> Result<papyrus_terrain::road_water::Policy, String> {
        self.validate()?;
        let policy = papyrus_terrain::road_water::Policy {
            engine: papyrus_terrain::road_water::VERSION.into(),
            topology: self.policy.water_topology,
            clearance: self.policy.water_clearance,
        };
        policy.validate()?;
        Ok(policy)
    }
    fn validate(&self) -> Result<(), String> {
        let mut ids = std::collections::BTreeSet::new();
        if self.format != "papyrus-road-terrain-profile"
            || self.schema != 1
            || !matches!(
                self.id.as_str(),
                "client-0f703d43-road-terrain-1" | "client-8dd4e235-road-terrain-1"
                    | "client-58f3263f-road-terrain-1"
            )
            || [
                self.road_paint_profile_sha256.as_str(),
                self.reference_server_assembly_sha256.as_str(),
            ]
            .iter()
            .any(|s| {
                s.len() != 64
                    || !s
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            || !self.water_no_hit_level.is_finite()
            || self.water_no_hit_level.abs() > 500.
            || self.families.is_empty()
            || self.families.len() > 16
            || self.families.iter().any(|f| {
                !ids.insert(f.hierarchy)
                    || f.hierarchy > 15
                    || f.fade
                        .iter()
                        .any(|v| !v.is_finite() || !(0. ..=256.).contains(v))
                    || !f.terrain_offset.is_finite()
                    || f.terrain_offset.abs() > 64.
            })
        {
            return Err("Invalid road terrain compatibility profile".into());
        }
        self.policy.validate()
    }
    pub fn for_paint(paint: &road_paint_source::Profile) -> Result<Option<Self>, String> {
        let paint_sha256 = paint.sha256()?;
        for raw in [DATA, CURRENT_DATA, include_str!("../../../compatibility/infrastructure/client-58f3263f-road-terrain.json")] {
            let profile: Self = serde_json::from_str(raw).map_err(|e| e.to_string())?;
            profile.validate()?;
            if profile.road_paint_profile_sha256 != paint_sha256 {
                continue;
            }
            if profile.families.len() != paint.policy.families.len()
                || profile.families.iter().any(|f| {
                    !paint
                        .policy
                        .families
                        .iter()
                        .any(|p| p.hierarchy == f.hierarchy)
                })
            {
                return Err("Road height and paint family coverage differs".into());
            }
            return Ok(Some(profile));
        }
        Ok(None)
    }
    pub fn sha256(&self) -> Result<String, String> {
        self.validate()?;
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
    fn captured_loop_fades_filter_priority_and_family_modifiers_are_retained() {
        let profile: Profile = serde_json::from_str(DATA).unwrap();
        profile.validate().unwrap();
        assert_eq!(profile.policy.road_fades, vec![1., 0.5]);
        assert_eq!(profile.policy.rail_fade_multiplier, 0.25);
        let water = profile.water_policy().unwrap();
        assert_eq!(water.topology, 49152);
        assert_eq!(water.clearance, 2.);
        assert_eq!(profile.policy.weight(1024 | 16384), 0.);
        assert_eq!(profile.policy.weight(1048576 | 16384), 0.5);
        assert!(
            profile
                .families
                .iter()
                .all(|f| f.fade == [1., 8.] && f.terrain_offset == -0.125)
        );
        let before = profile.sha256().unwrap();
        let mut changed = profile;
        changed.water_no_hit_level = 2.;
        assert_ne!(before, changed.sha256().unwrap());
        changed.families.push(Family {
            hierarchy: 0,
            fade: [1., 8.],
            terrain_offset: -0.125,
        });
        assert!(changed.validate().is_err());
    }
}
