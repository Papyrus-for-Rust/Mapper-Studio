//! Installation-owned inputs for socket-connected coastal cliff placement.
use crate::decor::{Kind, Stage};
use serde::{Deserialize, Serialize};
mod placement;
pub use placement::{Join, PlacementReport, Side, VERSION, emit};

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Parameters {
    #[serde(alias = "CutoffSlopeInitial")]
    pub cutoff_slope_initial: i32,
    #[serde(alias = "CutoffSlopeRepeat")]
    pub cutoff_slope_repeat: i32,
    #[serde(alias = "MinTerrainHeight")]
    pub minimum_terrain_height: i32,
    #[serde(alias = "MaxTerrainHeight")]
    pub maximum_terrain_height: i32,
    #[serde(alias = "MinCliffHeight")]
    pub minimum_cliff_height: i32,
    #[serde(alias = "MaxCliffHeight")]
    pub maximum_cliff_height: i32,
    #[serde(alias = "MinCliffScale")]
    pub minimum_scale: f32,
    #[serde(alias = "MaxCliffScale")]
    pub maximum_scale: f32,
}
impl Parameters {
    pub fn validate(&self) -> Result<(), String> {
        if [self.cutoff_slope_initial, self.cutoff_slope_repeat]
            .iter()
            .any(|s| !(0..=90).contains(s))
            || [
                self.minimum_terrain_height,
                self.maximum_terrain_height,
                self.minimum_cliff_height,
                self.maximum_cliff_height,
            ]
            .iter()
            .any(|h| !(-500..=500).contains(h))
            || self.minimum_terrain_height > self.maximum_terrain_height
            || self.minimum_cliff_height > self.maximum_cliff_height
            || !self.minimum_scale.is_finite()
            || !self.maximum_scale.is_finite()
            || self.minimum_scale <= 0.
            || self.minimum_scale > self.maximum_scale
            || self.maximum_scale > 10000.
        {
            return Err("Invalid coastal cliff placement parameters".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Program {
    pub stage: Stage,
    pub parameters: Parameters,
    pub retry_multiplier: u32,
    pub target_count: u32,
    pub target_length: u32,
    pub repeat_anchor_mode: i32,
    pub repeat_filter_mode: i32,
}
impl Program {
    pub fn validate(&self) -> Result<(), String> {
        self.stage.validate()?;
        self.parameters.validate()?;
        if self.stage.kind != Kind::WhiteNoise
            || self.stage.distance != 0.
            || self.stage.dithering != 0.
            || self.stage.density != 0.
            || self.stage.cluster.is_some()
            || !(0..=2).contains(&self.stage.anchor_mode)
            || !(-1..=7).contains(&self.stage.filter_mode)
            || !(0..=2).contains(&self.repeat_anchor_mode)
            || !(-1..=7).contains(&self.repeat_filter_mode)
            || self.retry_multiplier > 1000
            || self.target_count > 1024
            || self.target_length > 100000
        {
            return Err("Invalid coastal cliff program".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Prefab {
    pub male: Option<[f32; 3]>,
    pub female: Option<[f32; 3]>,
    pub parameters: Option<Parameters>,
}
impl Prefab {
    pub fn validate(&self) -> Result<(), String> {
        if self
            .male
            .iter()
            .chain(self.female.iter())
            .flatten()
            .any(|v| !v.is_finite() || v.abs() > 10000.)
        {
            return Err("Invalid coastal cliff socket position".into());
        }
        if let Some(parameters) = &self.parameters {
            parameters.validate()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parameters() -> Parameters {
        Parameters {
            cutoff_slope_initial: 45,
            cutoff_slope_repeat: 10,
            minimum_terrain_height: 0,
            maximum_terrain_height: 20,
            minimum_cliff_height: 0,
            maximum_cliff_height: 500,
            minimum_scale: 0.85,
            maximum_scale: 1.,
        }
    }
    #[test]
    fn source_parameters_reject_unknown_fields_bad_ranges_and_nonfinite_sockets() {
        let p = parameters();
        p.validate().unwrap();
        let mut value = serde_json::to_value(&p).unwrap();
        value["future_rule"] = 1.into();
        assert!(serde_json::from_value::<Parameters>(value).is_err());
        for bad in 0..6 {
            let mut p = parameters();
            match bad {
                0 => p.cutoff_slope_initial = 91,
                1 => p.minimum_terrain_height = 21,
                2 => p.maximum_cliff_height = -1,
                3 => p.minimum_scale = 0.,
                4 => p.maximum_scale = f32::NAN,
                _ => p.maximum_scale = 0.8,
            }
            assert!(p.validate().is_err());
        }
        for v in [f32::NAN, f32::INFINITY, 10001.] {
            assert!(
                Prefab {
                    male: Some([v, 0., 0.]),
                    ..Default::default()
                }
                .validate()
                .is_err()
            );
        }
    }
    #[test]
    fn source_program_validates_initial_and_repeat_policies_independently() {
        let stage = serde_json::from_value(serde_json::json!({
            "id":"604","order":0,"name":"Coastal Cliffs","kind":"white_noise",
            "folder":"decor/cliff_coastal","enabled":true,"mode":-1,
            "filter":{"SplatType":-1,"BiomeType":-1,"TopologyAny":2,"TopologyAll":0,"TopologyNot":0},
            "filter_mode":1,"anchor_mode":2,"distance":0.,"dithering":0.,"density":0.,
            "cluster":null,"prefabs":[]
        })).unwrap();
        let program = Program {
            stage,
            parameters: parameters(),
            retry_multiplier: 100,
            target_count: 8,
            target_length: 0,
            repeat_anchor_mode: 1,
            repeat_filter_mode: 1,
        };
        program.validate().unwrap();
        for bad in 0..5 {
            let mut p = program.clone();
            match bad {
                0 => p.repeat_filter_mode = 8,
                1 => p.repeat_anchor_mode = 3,
                2 => p.retry_multiplier = 1001,
                3 => p.stage.density = 1.,
                _ => p.stage.kind = Kind::Uniform,
            }
            assert!(p.validate().is_err());
        }
    }
}
