//! Cartographic colors from the selected installation's PaperMap material.
use serde::{Deserialize, Serialize};
pub const NAMES: [&str; 11] = [
    "Dirt",
    "Snow",
    "Sand",
    "Rock",
    "Grass",
    "Forest",
    "Pebbles",
    "Gravel",
    "Base",
    "Shallow water",
    "Deep water",
];
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Preferences {
    pub enabled: bool,
    pub overrides: [Option<[u8; 3]>; 11],
}
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Material {
    pub material_path: String,
    pub serialized_file: String,
    pub material_object: i64,
    pub shader_object: i64,
    pub shader_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shader_blob_sha256: Option<String>,
    pub colors: [[f32; 4]; 11],
    pub sun_direction: [f32; 3],
    pub sun_power: f32,
    pub brightness: f32,
    pub contrast: f32,
}
impl Material {
    fn verified_client_shader(&self) -> bool {
        matches!(
            self.shader_blob_sha256.as_deref(),
            Some(
                "9ff936729f90baad098b7a69f0a3244868345f99e3a2542e00063cc004821e07"
                    | "60b55c7b11da98c2a67f2162a29f1e1aba3ce66d698f8d017f1fe5e5f9a6b8b1"
            )
        )
    }
    /// Verified PaperMap D3D11 program: post-contrast includes water. The older
    /// managed MapImageRenderer instead applies contrast only to land. Unknown
    /// shaders retain that historical response until their program is verified.
    pub fn water_uses_contrast(&self) -> bool {
        self.verified_client_shader()
    }

    /// The newer PaperMap program reads alpha/green directly for darkening.
    /// Red still participates in lighting, so decoded lighting X cannot be reused.
    pub(crate) fn slope_uses_normal(&self) -> bool {
        self.shader_blob_sha256.as_deref()
            == Some("60b55c7b11da98c2a67f2162a29f1e1aba3ce66d698f8d017f1fe5e5f9a6b8b1")
    }
    pub(crate) fn sampled_slope(&self, coarse: f32, normal: Option<[f32; 4]>) -> f32 {
        if self.slope_uses_normal() {
            normal.map_or(0., |rgba| {
                let x = rgba[3] * 2. - 1.;
                let z = rgba[1] * 2. - 1.;
                x * x + z * z
            })
        } else {
            coarse
        }
    }
    /// Consume only a source-bound texture frame. Missing inputs retain the
    /// zero metric until the installation's texture producer is verified.
    pub(crate) fn surface_slope(
        &self,
        surface: Option<&crate::map_surface::Grid>,
        uv: [f32; 2],
    ) -> f32 {
        if self.slope_uses_normal() {
            self.sampled_slope(
                0.,
                surface
                    .and_then(|s| s.normal())
                    .and_then(|n| n.sample_rgba_lod(uv, surface.map_or(0., |s| s.normal_lod()))),
            )
        } else {
            surface.and_then(|s| s.sample(uv)).map_or(0., |p| p[1])
        }
    }

    pub(crate) fn surface_height(
        &self,
        surface: Option<&crate::map_surface::Grid>,
        uv: [f32; 2],
        fallback: impl FnOnce() -> f32,
    ) -> f32 {
        if self.slope_uses_normal() {
            surface
                .and_then(|s| s.sample(uv))
                .map_or_else(fallback, |p| p[0])
        } else {
            fallback()
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.material_path != "assets/prefabs/tools/map/papermap.mat"
            || self.shader_name != "Custom/Map"
            || self.serialized_file.is_empty()
            || self.serialized_file.len() > 256
            || self.serialized_file.contains(['/', '\\'])
            || self.material_object == 0
            || self.shader_object == 0
            || self.material_object == self.shader_object
            || self.shader_blob_sha256.as_ref().is_some_and(|s| {
                s.len() != 64
                    || !s
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            || self
                .colors
                .iter()
                .flatten()
                .any(|v| !v.is_finite() || !(0. ..=1.).contains(v))
            || self
                .sun_direction
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 100.)
            || self.sun_direction.iter().map(|v| v * v).sum::<f32>() < 1e-8
            || [self.sun_power, self.brightness, self.contrast]
                .iter()
                .any(|v| !v.is_finite() || !(0. ..=4.).contains(v))
        {
            return Err("Unsupported PaperMap color metadata".into());
        }
        Ok(())
    }
    pub fn effective(&self, preferences: &Preferences) -> Self {
        let mut result = self.clone();
        if preferences.enabled {
            for (color, override_color) in result.colors.iter_mut().zip(preferences.overrides) {
                if let Some(rgb) = override_color {
                    for c in 0..3 {
                        color[c] = rgb[c] as f32 / 255.;
                    }
                }
            }
        }
        result
    }
    pub fn ground(&self, weights: [f32; 8]) -> [f32; 4] {
        let mut rgb = self.colors[8];
        // MapImageRenderer's sequential, noncommutative splat lerps. Biomes
        // already affect generated splat weights; there is no extra biome tint.
        for i in [7, 6, 3, 0, 4, 5, 2, 1] {
            let t = (weights[i] * self.colors[i][3]).clamp(0., 1.);
            for (c, value) in rgb.iter_mut().enumerate() {
                *value += t * (self.colors[i][c] - *value);
            }
        }
        rgb
    }
    pub fn land(&self, rgba: [f32; 4], normal: [f32; 3]) -> [f32; 3] {
        self.land_with_slope(rgba, normal, 0.)
    }
    /// `slope` is the version-specific sampled metric from surface_slope().
    /// It is never a degree angle. Missing texture frames supply zero.
    pub fn land_with_slope(&self, rgba: [f32; 4], normal: [f32; 3], slope: f32) -> [f32; 3] {
        let rgba = self.slope_ground(rgba, slope);
        // The client unpacks world X/Z from the normal texture and reconstructs
        // its upward component in Z before dotting the serialized sun vector.
        // The managed server renderer instead dots the world-space X/Y/Z normal.
        let normal = if self.verified_client_shader() {
            [normal[0], normal[2], normal[1]]
        } else {
            normal
        };
        let length = self.sun_direction.iter().map(|v| v * v).sum::<f32>().sqrt();
        let dot = normal
            .iter()
            .zip(self.sun_direction)
            .map(|(n, s)| n * s / length)
            .sum::<f32>()
            .max(0.);
        let light = 1. + (dot - 0.5) * self.sun_power;
        std::array::from_fn(|c| {
            ((rgba[c] * light - 0.5) * self.contrast + 0.5) * self.brightness * 255.
        })
    }
    pub fn water(&self, weights: [f32; 8], depth: f32) -> [f32; 3] {
        self.water_with_slope(weights, depth, 0.)
    }
    /// The client attenuates ground before both land lighting and water blending.
    pub fn water_with_slope(&self, weights: [f32; 8], depth: f32, slope: f32) -> [f32; 3] {
        let ground = self.slope_ground(self.ground(weights), slope);
        let near = (0.5 + depth.max(0.) / 5.).clamp(0., 1.);
        let far = (depth.max(0.) / 50.).clamp(0., 1.);
        std::array::from_fn(|c| {
            let value = ground[c] + near * (self.colors[9][c] - ground[c]);
            let mut value = value + far * (self.colors[10][c] - value);
            if self.water_uses_contrast() {
                value = (value - 0.5) * self.contrast + 0.5;
            }
            value * self.brightness * 255.
        })
    }
    fn slope_ground(&self, mut rgba: [f32; 4], slope: f32) -> [f32; 4] {
        if self.verified_client_shader() && slope >= 0.2 {
            let factor = (1. - ((slope - 0.2) * 1.25).clamp(0., 1.) * 0.75).max(0.5);
            for c in &mut rgba[..3] {
                *c *= factor;
            }
        }
        rgba
    }
}
/// Same diagonal vertex normals used by the retained native height grid, with
/// a sampled height provider so projected stamps use the same light calculation.
pub fn normal_sampled(
    size: u32,
    n: usize,
    u: f32,
    v: f32,
    height: impl Fn(f32, f32) -> f32,
) -> [f32; 3] {
    let [x, z] = [u, v].map(|v| v.clamp(0., 1.) * (n - 1) as f32);
    let vertex = |x: usize, z: usize| {
        let l = x.saturating_sub(1);
        let r = (x + 1).min(n - 1);
        let f = z.saturating_sub(1);
        let b = (z + 1).min(n - 1);
        let at = |x: usize, z: usize| height(x as f32 / (n - 1) as f32, z as f32 / (n - 1) as f32);
        let base = at(l, f);
        let normal = [
            -(at(r, f) - base) * 0.5,
            size as f32 / n as f32,
            -(at(l, b) - base) * 0.5,
        ];
        let len = normal.iter().map(|v| v * v).sum::<f32>().sqrt();
        normal.map(|v| v / len)
    };
    let a = x as usize;
    let b = z as usize;
    let c = (a + 1).min(n - 1);
    let d = (b + 1).min(n - 1);
    let low = papyrus_terrain::rail_costmap::slerp(vertex(a, b), vertex(c, b), x.fract());
    let high = papyrus_terrain::rail_costmap::slerp(vertex(a, d), vertex(c, d), x.fract());
    papyrus_terrain::rail_costmap::slerp(low, high, z.fract())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    #[test]
    fn mip_filtered_lanes_and_base_lighting_match_both_owned_shaders() {
        #[derive(Deserialize)]
        struct Case {
            normal_lod: f32,
            uv_scale: f32,
            water_depth: Option<f32>,
            rgba: [f32; 4],
            legacy_rgba: [f32; 4],
        }
        #[derive(Deserialize)]
        struct Profile {
            normal_mips: Vec<[f32; 4]>,
            rows: Vec<Case>,
        }
        #[derive(Deserialize)]
        struct Inputs {
            snow: [f32; 4],
            water: [f32; 4],
            deep_water: [f32; 4],
            sun_direction: [f32; 3],
            sun_power: f32,
            brightness: f32,
            contrast: f32,
        }
        #[derive(Deserialize)]
        struct Probe {
            format: String,
            schema: u32,
            driver: String,
            normal_base_resolution: usize,
            sampler: String,
            current_shader_blob_sha256: String,
            legacy_shader_blob_sha256: String,
            current_pixel_program_sha256: String,
            legacy_pixel_program_sha256: String,
            material: Inputs,
            profiles: Vec<Profile>,
        }
        let probe: Probe = serde_json::from_str(include_str!(
            "../tests/fixtures/client-papermap-mip-inputs-20261108.json"
        ))
        .unwrap();
        assert_eq!(probe.format, "mapper-owned-shader-mip-probe");
        assert_eq!(probe.schema, 1);
        assert_eq!(probe.driver, "d3d11-warp");
        assert_eq!(probe.normal_base_resolution, 8);
        assert_eq!(probe.sampler, "trilinear-clamp");
        assert_eq!(
            probe.current_pixel_program_sha256,
            "ac12266cb1b50ca7c47cc37a9585a3af5109682ebc96049e838e228c67ce7aff"
        );
        assert_eq!(
            probe.legacy_pixel_program_sha256,
            "b48a03a5b8b079f0fbc660e098a6430ac31ed25f1c4659d96ada88cb94a924d8"
        );
        let mut current = fixture();
        current.shader_blob_sha256 = Some(probe.current_shader_blob_sha256);
        current.colors[1] = probe.material.snow;
        current.colors[9] = probe.material.water;
        current.colors[10] = probe.material.deep_water;
        current.sun_direction = probe.material.sun_direction;
        current.sun_power = probe.material.sun_power;
        current.brightness = probe.material.brightness;
        current.contrast = probe.material.contrast;
        current.validate().unwrap();
        let mut legacy = current.clone();
        legacy.shader_blob_sha256 = Some(probe.legacy_shader_blob_sha256);
        assert!(current.slope_uses_normal());
        assert!(!legacy.slope_uses_normal());
        let weights = [0., 1., 0., 0., 0., 0., 0., 0.];
        assert_eq!(probe.profiles.len(), 3);
        for profile in probe.profiles {
            assert_eq!(profile.normal_mips.len(), 4);
            assert_eq!(profile.rows.len(), 34);
            let base_normal = crate::map_normal::decode(profile.normal_mips[0]).unwrap();
            for case in profile.rows {
                // The shader multiplies input UV by three. The probe draws
                // one pixel across the eight-texel normal source.
                let lod_from_uv = (case.uv_scale * 3. * 8.).log2();
                assert!((lod_from_uv - case.normal_lod).abs() < 1e-6);
                let lod = case.normal_lod.clamp(0., 3.);
                let low = lod as usize;
                let high = (low + 1).min(3);
                let filtered = std::array::from_fn(|c| {
                    let a = profile.normal_mips[low][c];
                    a + (profile.normal_mips[high][c] - a) * lod.fract()
                });
                // Filter encoded lanes before calculating the metric. Lighting
                // continues to decode level zero even when slope uses a mip.
                for (material, expected) in [(&current, case.rgba), (&legacy, case.legacy_rgba)] {
                    let slope = material.sampled_slope(0., Some(filtered));
                    let actual = if let Some(depth) = case.water_depth {
                        material.water_with_slope(weights, depth, slope)
                    } else {
                        material.land_with_slope(material.ground(weights), base_normal, slope)
                    };
                    for (actual, expected) in actual.into_iter().zip(expected) {
                        assert!(
                            (actual - expected * 255.).abs() < 0.0002,
                            "LOD {} water {:?}: {actual} differs from {}",
                            case.normal_lod,
                            case.water_depth,
                            expected * 255.
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn mixed_materials_and_shore_inputs_match_owned_current_pixel_shader() {
        #[derive(Deserialize)]
        struct Row {
            texels: [[f32; 4]; 7],
            rgba: [f32; 4],
        }
        #[derive(Deserialize)]
        struct Probe {
            format: String,
            schema: u32,
            driver: String,
            pixel_program_sha256: String,
            whole_map_pixel_parity: bool,
            material: Material,
            rows: Vec<Row>,
        }
        let probe: Probe = serde_json::from_str(include_str!(
            "../tests/fixtures/client-papermap-surfaces-199a470f.json"
        ))
        .unwrap();
        assert_eq!(probe.format, "mapper-owned-shader-surface-probe");
        assert_eq!(probe.schema, 1);
        assert_eq!(probe.driver, "d3d11-warp");
        assert_eq!(
            probe.pixel_program_sha256,
            "ac12266cb1b50ca7c47cc37a9585a3af5109682ebc96049e838e228c67ce7aff"
        );
        assert!(!probe.whole_map_pixel_parity);
        assert_eq!(probe.rows.len(), 600);
        let m = probe.material;
        m.validate().unwrap();
        assert!(m.slope_uses_normal());
        let mut land_cases = 0;
        let mut water_cases = 0;
        for (i, row) in probe.rows.iter().enumerate() {
            let weights = std::array::from_fn(|c| row.texels[2 + c / 4][c % 4]);
            let height = row.texels[4][0];
            let shore_distance = row.texels[5][2];
            let shore_type = row.texels[5][3];
            // Independent shader branch transcription. This does not establish
            // that Mapper produces the source shoreline texture for a world.
            let mut depth = 0.;
            if shore_distance > 0. {
                depth = -height;
                if depth <= 0. || shore_type != 1. {
                    depth = depth.max(shore_distance * 0.1);
                }
            }
            let slope = m.sampled_slope(0., Some(row.texels[6]));
            let actual = if depth > 0. {
                water_cases += 1;
                m.water_with_slope(weights, depth, slope)
            } else {
                land_cases += 1;
                m.land_with_slope(
                    m.ground(weights),
                    crate::map_normal::decode(row.texels[6]).unwrap(),
                    slope,
                )
            };
            assert_eq!(row.rgba[3], 1.);
            for (actual, expected) in actual.into_iter().zip(row.rgba) {
                assert!(
                    (actual - expected * 255.).abs() < 0.0002,
                    "surface shader case {i}: {actual} differs from {}",
                    expected * 255.
                );
            }
        }
        assert_eq!((land_cases, water_cases), (180, 420));
    }

    #[test]
    fn current_normal_slope_matches_all_owned_pixel_shader_cases() {
        let probe: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/client-papermap-normal-slope-8dd4e235.json"
        ))
        .unwrap();
        assert_eq!(probe["format"], "mapper-owned-current-shader-probe");
        assert_eq!(probe["schema"], 1);
        assert_eq!(probe["driver"], "d3d11-warp");
        assert_eq!(
            probe["pixel_program_sha256"],
            "ac12266cb1b50ca7c47cc37a9585a3af5109682ebc96049e838e228c67ce7aff"
        );
        let mut m = fixture();
        m.shader_blob_sha256 = Some(probe["shader_blob_sha256"].as_str().unwrap().into());
        let inputs = &probe["material"];
        m.colors[1] = serde_json::from_value(inputs["snow"].clone()).unwrap();
        m.colors[9] = serde_json::from_value(inputs["water"].clone()).unwrap();
        m.colors[10] = serde_json::from_value(inputs["deep_water"].clone()).unwrap();
        m.sun_direction = serde_json::from_value(inputs["sun_direction"].clone()).unwrap();
        m.sun_power = inputs["sun_power"].as_f64().unwrap() as f32;
        m.brightness = inputs["brightness"].as_f64().unwrap() as f32;
        m.contrast = inputs["contrast"].as_f64().unwrap() as f32;
        m.validate().unwrap();
        assert!(m.slope_uses_normal());
        assert!(m.water_uses_contrast());
        let rows = probe["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 180);
        let weights = [0., 1., 0., 0., 0., 0., 0., 0.];
        for (i, row) in rows.iter().enumerate() {
            let encoded: [f32; 4] = if row["sampled_rgba"].is_array() {
                serde_json::from_value(row["sampled_rgba"].clone()).unwrap()
            } else {
                let normal: [f32; 3] = serde_json::from_value(row["world_normal"].clone()).unwrap();
                [1., (normal[2] + 1.) * 0.5, 0., (normal[0] + 1.) * 0.5]
            };
            let coarse = row["slope"].as_f64().unwrap_or(0.) as f32;
            let metric = m.sampled_slope(coarse, Some(encoded));
            assert_eq!(metric, m.sampled_slope(99., Some(encoded)));
            let actual = if let Some(depth) = row["water_depth"].as_f64() {
                m.water_with_slope(weights, depth as f32, metric)
            } else {
                m.land_with_slope(
                    m.ground(weights),
                    crate::map_normal::decode(encoded).unwrap(),
                    metric,
                )
            };
            let expected: [f32; 4] = serde_json::from_value(row["rgba"].clone()).unwrap();
            for (actual, expected) in actual.into_iter().zip(expected) {
                assert!(
                    (actual - expected * 255.).abs() < 0.0002,
                    "current shader case {i}: {actual} differs from {}",
                    expected * 255.
                );
            }
        }
        assert_eq!(m.sampled_slope(99., None), 0.);
        assert_eq!(m.surface_slope(None, [0.5, 0.5]), 0.);
        let mut settings = crate::map_surface_frame::settings_dimensions(8, 17, 1024 * 1024);
        settings.blur_pairs = 0;
        let surface = crate::map_surface::generate(&[16383; 17 * 17], settings, || false)
            .unwrap()
            .unwrap();
        let uv = [0.5, 0.5];
        assert_eq!(
            m.surface_height(Some(&surface), uv, || panic!("Coarse height exists")),
            surface.sample(uv).unwrap()[0]
        );
        assert_eq!(m.surface_height(None, uv, || 37.), 37.);
        // Red affects lighting but has no influence on the slope metric.
        assert_eq!(
            m.sampled_slope(0., Some([0.25, 0.8, 0.4, 0.9])),
            m.sampled_slope(0., Some([1., 0.8, 0.4, 0.9]))
        );
        m.shader_blob_sha256 =
            Some("9ff936729f90baad098b7a69f0a3244868345f99e3a2542e00063cc004821e07".into());
        assert!(!m.slope_uses_normal());
        assert_eq!(m.sampled_slope(0.4, Some([1.; 4])), 0.4);
        assert_eq!(m.surface_height(Some(&surface), uv, || 37.), 37.);
    }

    #[test]
    fn filtered_slope_response_matches_owned_shader_on_land_and_under_water() {
        #[derive(Deserialize)]
        struct Case {
            world_normal: [f32; 3],
            slope: f32,
            water_depth: Option<f32>,
            rgba: [f32; 4],
        }
        #[derive(Deserialize)]
        struct Inputs {
            snow: [f32; 4],
            water: [f32; 4],
            deep_water: [f32; 4],
            sun_direction: [f32; 3],
            sun_power: f32,
            brightness: f32,
            contrast: f32,
        }
        #[derive(Deserialize)]
        struct Probe {
            format: String,
            schema: u32,
            shader_blob_sha256: String,
            pixel_program_sha256: String,
            material: Inputs,
            rows: Vec<Case>,
        }
        let probe: Probe = serde_json::from_str(include_str!(
            "../tests/fixtures/client-papermap-slope-0f703d43.json"
        ))
        .unwrap();
        assert_eq!(probe.format, "mapper-owned-shader-slope-probe");
        assert_eq!(probe.schema, 1);
        assert_eq!(probe.rows.len(), 162);
        assert_eq!(
            probe.pixel_program_sha256,
            "b48a03a5b8b079f0fbc660e098a6430ac31ed25f1c4659d96ada88cb94a924d8"
        );
        let mut m = fixture();
        m.shader_blob_sha256 = Some(probe.shader_blob_sha256);
        assert!(m.water_uses_contrast());
        m.colors[1] = probe.material.snow;
        m.colors[9] = probe.material.water;
        m.colors[10] = probe.material.deep_water;
        m.sun_direction = probe.material.sun_direction;
        m.sun_power = probe.material.sun_power;
        m.brightness = probe.material.brightness;
        m.contrast = probe.material.contrast;
        m.validate().unwrap();
        let weights = [0., 1., 0., 0., 0., 0., 0., 0.];
        for case in &probe.rows {
            let actual = if let Some(depth) = case.water_depth {
                m.water_with_slope(weights, depth, case.slope)
            } else {
                m.land_with_slope(m.ground(weights), case.world_normal, case.slope)
            };
            for (actual, expected) in actual.into_iter().zip(case.rgba) {
                assert!(
                    (actual - expected * 255.).abs() < 0.0002,
                    "slope {} water {:?}: {actual} differs from client {}",
                    case.slope,
                    case.water_depth,
                    expected * 255.
                );
            }
        }
        let rgba = [0.7, 0.6, 0.8, 0.3];
        assert_eq!(m.slope_ground(rgba, 0.1999), rgba);
        assert_eq!(m.slope_ground(rgba, 0.2), rgba);
        assert_eq!(m.slope_ground(rgba, 1.), [0.35, 0.3, 0.4, 0.3]);
        for hash in [None, Some("b".repeat(64))] {
            m.shader_blob_sha256 = hash;
            for case in &probe.rows {
                assert_eq!(
                    m.land_with_slope(rgba, case.world_normal, case.slope),
                    m.land(rgba, case.world_normal)
                );
                assert_eq!(
                    m.water_with_slope(weights, 1., case.slope),
                    m.water(weights, 1.)
                );
            }
        }
    }
    #[test]
    fn land_lighting_matches_the_owned_client_pixel_shader_normal_axes() {
        #[derive(Deserialize)]
        struct Case {
            world_normal: [f32; 3],
            rgba: [f32; 4],
        }
        #[derive(Deserialize)]
        struct Probe {
            format: String,
            schema: u32,
            shader_blob_sha256: String,
            pixel_program_sha256: String,
            rows: Vec<Case>,
        }
        // Recorded by the actual extracted D3D11 pixel shader over our synthetic
        // 1x1 textures and constants, not by a second implementation of land().
        let probe: Probe = serde_json::from_str(include_str!(
            "../tests/fixtures/client-papermap-normal-lighting-0f703d43.json"
        ))
        .unwrap();
        assert_eq!(probe.format, "mapper-owned-shader-lighting-probe");
        assert_eq!(probe.schema, 1);
        assert_eq!(probe.rows.len(), 9);
        assert_eq!(
            probe.pixel_program_sha256,
            "b48a03a5b8b079f0fbc660e098a6430ac31ed25f1c4659d96ada88cb94a924d8"
        );
        let mut material = fixture();
        let snow = [0.86274517, 0.9294118, 0.94117653, 1.];
        let legacy: Vec<_> = probe
            .rows
            .iter()
            .map(|c| material.land(snow, c.world_normal))
            .collect();
        material.shader_blob_sha256 = Some("b".repeat(64));
        for (case, expected) in probe.rows.iter().zip(&legacy) {
            assert_eq!(material.land(snow, case.world_normal), *expected);
        }
        material.shader_blob_sha256 = Some(probe.shader_blob_sha256);
        material.validate().unwrap();
        assert!(material.water_uses_contrast());
        for (case, legacy) in probe.rows.iter().zip(legacy) {
            let actual = material.land(snow, case.world_normal);
            for (actual, expected) in actual.into_iter().zip(case.rgba) {
                assert!(
                    (actual - expected * 255.).abs() < 0.0002,
                    "{:?}: {actual} differs from client {}",
                    case.world_normal,
                    expected * 255.
                );
            }
            assert!((legacy[0] - actual[0]).abs() > 0.01);
        }
    }
    pub(crate) fn fixture() -> Material {
        Material {
            material_path: "assets/prefabs/tools/map/papermap.mat".into(),
            serialized_file: "CAB-test".into(),
            material_object: 1,
            shader_object: 2,
            shader_name: "Custom/Map".into(),
            shader_blob_sha256: None,
            colors: std::array::from_fn(|i| [i as f32 / 12., 0.3, 0.5, 1.]),
            sun_direction: [0.95, 2.87, 2.37],
            sun_power: 0.65,
            brightness: 1.05,
            contrast: 0.94,
        }
    }
    #[test]
    fn verified_client_water_post_contrast_preserves_legacy_and_unknown_responses() {
        let mut m = fixture();
        m.colors[8] = [0.8, 0.4, 0.2, 1.];
        m.colors[9] = [0.2, 0.3, 0.4, 1.];
        m.colors[10] = [0.02, 0.04, 0.06, 1.];
        m.contrast = 0.6;
        m.brightness = 1.1;
        let legacy: Vec<_> = [0., 1., 2.5, 25., 50.].map(|d| m.water([0.; 8], d)).into();
        // A different shader with identical material names does not opt in.
        m.shader_blob_sha256 = Some("a".repeat(64));
        m.validate().unwrap();
        assert!(!m.water_uses_contrast());
        for (i, depth) in [0., 1., 2.5, 25., 50.].into_iter().enumerate() {
            assert_eq!(m.water([0.; 8], depth), legacy[i]);
        }
        let land = m.land(m.ground([0.; 8]), [0., 1., 0.]);
        m.shader_blob_sha256 =
            Some("9ff936729f90baad098b7a69f0a3244868345f99e3a2542e00063cc004821e07".into());
        m.validate().unwrap();
        assert!(m.water_uses_contrast());
        // Numeric examples from the verified program's neutral water lighting,
        // contrast about .5 and brightness. Values remain floats until packing.
        for (depth, expected) in [
            (0., [140.25, 115.005, 106.59]),
            (50., [59.466, 62.832, 66.198]),
        ] {
            for (value, expected) in m.water([0.; 8], depth).into_iter().zip(expected) {
                assert!((value - expected).abs() < 0.0001);
            }
        }
        assert_ne!(m.water([0.; 8], 0.), legacy[0]);
        assert_ne!(m.land(m.ground([0.; 8]), [0., 1., 0.]), land);
        let restored: Material = serde_json::from_value(serde_json::to_value(&m).unwrap()).unwrap();
        assert_eq!(restored, m);
        for invalid in ["", "A", &"A".repeat(64), &"a".repeat(63), &"g".repeat(64)] {
            m.shader_blob_sha256 = Some(invalid.into());
            assert!(m.validate().is_err());
        }
    }
    #[test]
    fn ordered_material_blends_and_shading_differ_from_weighted_terrain_colors() {
        let m = fixture();
        m.validate().unwrap();
        let mut w = [0.; 8];
        w[0] = 0.5;
        w[1] = 0.5;
        let actual = m.ground(w);
        let expected = std::array::from_fn::<_, 4, _>(|c| {
            (m.colors[8][c] + (m.colors[0][c] - m.colors[8][c]) * 0.5) * 0.5 + m.colors[1][c] * 0.5
        });
        assert_eq!(actual, expected);
        assert!((actual[0] - (m.colors[0][0] + m.colors[1][0]) * 0.5).abs() > 0.1);
        let n = [0., 1., 0.];
        let shade = m.land(actual, n);
        let length = (0.95_f32.powi(2) + 2.87_f32.powi(2) + 2.37_f32.powi(2)).sqrt();
        for c in 0..3 {
            let expected = ((actual[c] * (1. + (2.87 / length - 0.5) * 0.65) - 0.5) * 0.94 + 0.5)
                * 1.05
                * 255.;
            assert!((shade[c] - expected).abs() < 1e-5);
        }
        for i in 0..8 {
            let mut w = [0.; 8];
            w[i] = 1.;
            let mixed = m.ground(w);
            for (c, value) in mixed.iter().enumerate() {
                assert!((*value - m.colors[i][c]).abs() < 1e-6);
            }
        }
        let far = m.water([0.; 8], 100.);
        for (c, value) in far.iter().enumerate() {
            assert!((*value - m.colors[10][c] * m.brightness * 255.).abs() < 1e-5);
        }
    }
    #[test]
    fn custom_colors_reset_without_rounding_defaults_or_changing_source() {
        let m = fixture();
        let before = m.clone();
        let mut p = Preferences {
            enabled: true,
            ..Default::default()
        };
        p.overrides[4] = Some([255, 0, 16]);
        let customized = m.effective(&p);
        assert_eq!(customized.colors[4], [1., 0., 16. / 255., 1.]);
        assert_eq!(m, before);
        p.enabled = false;
        assert_eq!(m.effective(&p), m);
        assert_eq!(m.effective(&Preferences::default()), m);
        let old: crate::preferences::Preferences = serde_json::from_str("{\"schema\":1}").unwrap();
        assert_eq!(old.map_colors, Preferences::default());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let prefs = crate::preferences::Preferences {
            map_colors: p,
            ..Default::default()
        };
        prefs.save(&path).unwrap();
        assert_eq!(crate::preferences::Preferences::load(&path).unwrap(), prefs);
    }
    #[test]
    fn projected_lighting_normals_match_retained_height_normal_convention() {
        let n = 7;
        let size = 1000;
        let h = (0..n * n)
            .map(|i| 16000 + (i % 7) as i16 * 100 + (i / 7) as i16 * 30)
            .collect::<Vec<_>>();
        for (u, v) in [(0., 0.), (1., 1.), (0.37, 0.61), (0.5, 0.5)] {
            let expected = papyrus_terrain::terrain_normal::sample(size, n, &h, [u, v]).unwrap();
            // Bilinear height at exact vertices is the same decoded i16 height.
            let actual = normal_sampled(size, n, u, v, |x, z| {
                papyrus_terrain::erosion::decode(
                    h[(z * (n - 1) as f32).round() as usize * n
                        + (x * (n - 1) as f32).round() as usize],
                )
            });
            for c in 0..3 {
                assert!(
                    (actual[c] - expected[c]).abs() < 1e-4,
                    "{actual:?} != {expected:?}"
                );
            }
        }
    }
}
