//! Source-style runtime lookup sampling for authored WorldSplineData. The
//! installed server uses its baked LUT, not freshly resampled Hermite controls.
use serde::{Deserialize, Serialize};
type V = [f32; 3];
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Sample {
    pub distance: f32,
    pub position: V,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Curve {
    pub points: Vec<V>,
    pub tangents: Vec<V>,
    pub lut_interval: f32,
    pub length: f32,
    pub samples: Vec<Sample>,
}
#[derive(Debug, Serialize)]
pub struct Pose {
    pub position: V,
    pub tangent: V,
}
impl Curve {
    pub fn validate(&self) -> Result<(), String> {
        let finite = |v: f32| v.is_finite() && v.abs() <= 10000.;
        if !(2..=4096).contains(&self.points.len())
            || self.tangents.len() != self.points.len()
            || !(2..=1000000).contains(&self.samples.len())
            || !self.length.is_finite()
            || !(0. ..=10000.).contains(&self.length)
            || self.length == 0.
            || !self.lut_interval.is_finite()
            || !(0.05..=100.).contains(&self.lut_interval)
            || self
                .points
                .iter()
                .chain(&self.tangents)
                .flatten()
                .any(|&v| !finite(v))
            || self.samples.iter().any(|s| {
                !finite(s.distance) || s.distance < 0. || s.position.iter().any(|&v| !finite(v))
            })
            || self
                .samples
                .windows(2)
                .any(|s| s[0].distance >= s[1].distance)
            || self.samples[0].distance != 0.
            || self.samples.last().unwrap().distance != self.length
            || self.samples[0].position != self.points[0]
            || self.samples.last().unwrap().position != *self.points.last().unwrap()
        {
            return Err("Invalid authored shared spline controls/lookup data".into());
        }
        Ok(())
    }
    /// Call after validating immutable imported data. Endpoint tangents preserve
    /// their authored magnitude. Interior tangents follow the source LUT chord.
    pub fn sample(&self, distance: f32) -> Result<Pose, String> {
        if !distance.is_finite()
            || self.points.len() < 2
            || self.tangents.len() != self.points.len()
            || self.samples.len() < 2
            || !self.length.is_finite()
            || self.length <= 0.
        {
            return Err("Invalid authored spline query".into());
        }
        if distance <= 0. {
            return checked(Pose {
                position: self.points[0],
                tangent: self.tangents[0],
            });
        }
        if distance >= self.length {
            return checked(Pose {
                position: *self.points.last().unwrap(),
                tangent: *self.tangents.last().unwrap(),
            });
        }
        let upper = self.samples.partition_point(|s| s.distance <= distance);
        if upper == 0 || upper >= self.samples.len() {
            return Err("Invalid authored spline lookup range".into());
        }
        // The inspected source's backward lower-bound scan stops before bucket
        // zero. It falls back to the start even if bucket zero has later samples.
        let lower = if self.samples[upper - 1].distance < 1. {
            0
        } else {
            upper - 1
        };
        let a = &self.samples[lower];
        let b = &self.samples[upper];
        if !a.distance.is_finite() || !b.distance.is_finite() || b.distance <= a.distance {
            return Err("Invalid authored spline lookup interval".into());
        }
        let t = ((distance - a.distance) / (b.distance - a.distance)).clamp(0., 1.);
        let delta: V = std::array::from_fn(|i| b.position[i] - a.position[i]);
        let magnitude = (delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2]).sqrt();
        checked(Pose {
            position: std::array::from_fn(|i| a.position[i] + delta[i] * t),
            tangent: if magnitude > 0.00001 {
                delta.map(|v| v / magnitude)
            } else {
                [0.; 3]
            },
        })
    }
}
fn checked(pose: Pose) -> Result<Pose, String> {
    if pose
        .position
        .iter()
        .chain(&pose.tangent)
        .any(|v| !v.is_finite() || v.abs() > 10000.)
    {
        Err("Invalid authored spline pose".into())
    } else {
        Ok(pose)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_lookup_sampling_matches_the_actual_installed_world_spline_data() {
        let doc: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/world-spline-bd19c4ac.json"))
                .unwrap();
        assert_eq!(doc["reference"], "installed-server-WorldSplineData");
        for case in doc["cases"].as_array().unwrap() {
            let i = &case["input"];
            let curve = Curve {
                points: serde_json::from_value(i["Points"].clone()).unwrap(),
                tangents: serde_json::from_value(i["Tangents"].clone()).unwrap(),
                length: serde_json::from_value(i["Length"].clone()).unwrap(),
                lut_interval: 0.25,
                samples: i["Samples"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| Sample {
                        distance: serde_json::from_value(s["Distance"].clone()).unwrap(),
                        position: serde_json::from_value(s["Position"].clone()).unwrap(),
                    })
                    .collect(),
            };
            curve.validate().unwrap();
            for expected in case["expected"].as_array().unwrap() {
                let pose = curve
                    .sample(expected["distance"].as_f64().unwrap() as f32)
                    .unwrap();
                for (actual, key) in [(pose.position, "position"), (pose.tangent, "tangent")] {
                    let values: V = serde_json::from_value(expected[key].clone()).unwrap();
                    for (a, b) in actual.into_iter().zip(values) {
                        assert!((a - b).abs() < 0.00001, "{}: {a} vs {b}", i["Name"]);
                    }
                }
            }
            assert!(curve.sample(f32::NAN).is_err());
        }
    }
}
