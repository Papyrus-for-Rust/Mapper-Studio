//! Mainland TerrainWaterMap.WaterMapQueryStructure.GetHeightFast arithmetic.
//! Source: owned server assembly bd19c4ac. This is a map query, not WaterLevel's
//! topology/ocean promotion, collision, waves, volumes or deep-sea provider.
pub const VERSION: &str = "papyrus-water-map-query-bd19c4ac-1";

#[derive(Clone, Copy)]
pub struct Grid<'a> {
    pub resolution: usize,
    pub data: &'a [i16],
    pub origin_y: f32,
    pub vertical_span: f32,
}

impl Grid<'_> {
    /// Four reads, no allocation or mutation. Validate consumed encodings;
    /// whole-grid verification remains the source loader's responsibility.
    /// Fractions are formed before index clamping and are NOT saturated. The
    /// source extrapolates some negative-UV queries differently from height maps.
    pub fn sample(self, uv: [f32; 2]) -> Option<f32> {
        let n = self.resolution;
        if !(2..=4097).contains(&n)
            || self.data.len() != n * n
            || !self.origin_y.is_finite()
            || !self.vertical_span.is_finite()
            || self.vertical_span <= 0.
        {
            return None;
        }
        let [x, z] = uv.map(|v| v * (n - 1) as f32);
        if [x, z]
            .iter()
            .any(|v| !v.is_finite() || *v <= i32::MIN as f32 || *v >= i32::MAX as f32)
        {
            return None;
        }
        let [xi, zi] = [x as i32, z as i32];
        let [tx, tz] = [x - xi as f32, z - zi as f32];
        let a = xi.clamp(0, (n - 1) as i32) as usize;
        let b = zi.clamp(0, (n - 1) as i32) as usize;
        let c = usize::from(x < (n - 1) as f32);
        let d = if z < (n - 1) as f32 { n } else { 0 };
        let i = b * n + a;
        let indices = [i, i + c, i + d, i + d + c];
        let mut samples = [0.; 4];
        for (value, index) in samples.iter_mut().zip(indices) {
            let encoded = *self.data.get(index)?;
            if !(0..=32766).contains(&encoded) {
                return None;
            }
            *value = encoded as f32 * 3.051944E-5;
        }
        let low = (samples[1] - samples[0]) * tx + samples[0];
        let normalized = ((samples[3] - samples[2]) * tx + samples[2] - low) * tz + low;
        let value = self.origin_y + normalized * self.vertical_span;
        value.is_finite().then_some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_owned_managed_water_queries_match_float_bits() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/water-map-query-bd19c4ac.json"
        ))
        .unwrap();
        assert_eq!(
            reference["format"],
            "papyrus-owned-water-map-query-reference"
        );
        assert_eq!(reference["schema"], 1);
        assert_eq!(reference["deep_sea"], false);
        assert_eq!(
            reference["sources"]["Assembly-CSharp.dll"],
            "bd19c4ac705edd18ff556f5dbb3130e098b380577cc2ab0d73a2ec89ee289cc9"
        );
        let mut count = 0;
        for case in reference["cases"].as_array().unwrap() {
            let data = case["data"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_i64().unwrap() as i16)
                .collect::<Vec<_>>();
            let before = data.clone();
            let grid = Grid {
                resolution: case["resolution"].as_u64().unwrap() as usize,
                data: &data,
                origin_y: case["origin_y"].as_f64().unwrap() as f32,
                vertical_span: case["vertical_span"].as_f64().unwrap() as f32,
            };
            for row in case["rows"].as_array().unwrap() {
                let uv = [0, 1].map(|i| row["uv"][i].as_f64().unwrap() as f32);
                assert_eq!(
                    grid.sample(uv).unwrap().to_bits(),
                    row["height_bits"].as_u64().unwrap() as u32,
                    "n={} shape={} uv={uv:?}",
                    grid.resolution,
                    case["shape"]
                );
                count += 1;
            }
            assert_eq!(data, before);
        }
        assert_eq!(count, 405);
    }

    #[test]
    fn invalid_grids_and_queries_do_not_fabricate_water_heights() {
        let data = [0, 32766, 8000, 16383];
        let grid = Grid {
            resolution: 2,
            data: &data,
            origin_y: -500.,
            vertical_span: 1000.,
        };
        assert_eq!(grid.sample([0., 0.]), Some(-500.));
        // Negative fractions extrapolate; the generic terrain sampler saturates.
        assert_eq!(grid.sample([-0.5, 0.]), Some(-1000.));
        for uv in [
            [f32::NAN, 0.],
            [0., f32::INFINITY],
            [f32::MAX, 0.],
            [i32::MIN as f32, 0.],
        ] {
            assert!(grid.sample(uv).is_none());
        }
        for bad in [
            Grid {
                resolution: 1,
                ..grid
            },
            Grid {
                data: &data[..3],
                ..grid
            },
            Grid {
                vertical_span: 0.,
                ..grid
            },
            Grid {
                origin_y: f32::NAN,
                ..grid
            },
            Grid {
                data: &[0, -1, 0, 0],
                ..grid
            },
            Grid {
                data: &[0, 32767, 0, 0],
                ..grid
            },
        ] {
            assert!(bad.sample([0.5, 0.5]).is_none());
        }
    }
}
