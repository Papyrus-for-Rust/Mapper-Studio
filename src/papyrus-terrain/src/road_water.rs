//! Source-bound road clearance over an explicitly supplied generated river world.
//! These are sampled raw collider levels, not a guess from encoded water maps.
use crate::native_layers::Layers;
use serde::{Deserialize, Serialize};
pub const VERSION: &str = "papyrus-road-river-clearance-1";
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub engine: String,
    pub topology: i32,
    pub clearance: f32,
}
impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        if self.engine != VERSION || self.topology != 49152 || self.clearance != 2. {
            return Err("Unsupported source road river clearance".into());
        }
        Ok(())
    }
}
#[derive(Clone, Copy)]
pub struct Guide<'a> {
    pub policy: &'a Policy,
    pub heights: &'a [f32],
}
impl Guide<'_> {
    pub fn validate(&self, layers: &Layers, cancelled: &impl Fn() -> bool) -> Result<bool, String> {
        self.policy.validate()?;
        let n = layers.surface_resolution;
        if !(1000..=6000).contains(&layers.size)
            || !(2..=4097).contains(&n)
            || self.heights.len() != n * n
            || layers.topology.len() != n * n
        {
            return Err("Invalid road river collider grid".into());
        }
        for values in self.heights.chunks(4096) {
            if cancelled() {
                return Ok(false);
            }
            if values
                .iter()
                .any(|v| !v.is_finite() || !(-500. ..=498.).contains(v))
            {
                return Err("Invalid road river collider height".into());
            }
        }
        Ok(true)
    }
    pub fn minimum(&self, layers: &Layers, x: f32, z: f32) -> Option<f32> {
        let n = layers.surface_resolution;
        let half = layers.size as f32 * 0.5;
        if !x.is_finite() || !z.is_finite() || x.abs() > half || z.abs() > half {
            return None;
        }
        let cell = |v: f32| {
            ((v / layers.size as f32 + 0.5) * n as f32)
                .floor()
                .clamp(0., (n - 1) as f32) as usize
        };
        let i = cell(z) * n + cell(x);
        (layers.topology[i] & self.policy.topology != 0)
            .then(|| self.heights[i] + self.policy.clearance)
    }
    pub fn desired(&self, layers: &Layers, x: f32, z: f32, ground: f32) -> f32 {
        self.minimum(layers, x, z).map_or(ground, |h| ground.max(h))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guide_requires_explicit_river_flags_and_valid_finite_source_heights() {
        let mut layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: 3,
            surface_resolution: 2,
            height: vec![0; 9],
            splat: vec![0; 32],
            biome: vec![0; 20],
            topology: vec![16384, 32768, 16, 0],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        let policy = Policy {
            engine: VERSION.into(),
            topology: 49152,
            clearance: 2.,
        };
        let levels = [-0.5, 12., 0., 0.];
        let guide = Guide {
            policy: &policy,
            heights: &levels,
        };
        assert!(guide.validate(&layers, &|| false).unwrap());
        assert_eq!(guide.minimum(&layers, -250., -250.), Some(1.5));
        assert_eq!(guide.minimum(&layers, 250., -250.), Some(14.));
        assert_eq!(guide.minimum(&layers, -250., 250.), None);
        assert_eq!(guide.desired(&layers, -250., -250., -0.5), 1.5);
        assert_eq!(guide.desired(&layers, -250., -250., 5.), 5.);
        assert_eq!(guide.minimum(&layers, f32::NAN, 0.), None);
        assert_eq!(guide.minimum(&layers, 501., 0.), None);
        assert!(!guide.validate(&layers, &|| true).unwrap());
        for bad in [f32::NAN, f32::INFINITY, 499., -501.] {
            assert!(
                Guide {
                    policy: &policy,
                    heights: &[bad, 0., 0., 0.]
                }
                .validate(&layers, &|| false)
                .is_err()
            );
        }
        layers.topology.clear();
        assert!(guide.validate(&layers, &|| false).is_err());
        let mut invalid = policy;
        invalid.clearance = 3.;
        assert!(invalid.validate().is_err());
    }
}
