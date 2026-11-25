//! Presentation settings and portable coordinates for rendered map images.
use eframe::egui::{Rect, pos2};
use serde::{Deserialize, Serialize};

pub const REFERENCE_SIDE: f32 = 1024.;
pub const METADATA_KEY: &str = "Mapper map data";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Markers {
    #[default]
    None,
    Icons,
    Text,
    Centers,
}
impl Markers {
    pub const ALL: [Self; 4] = [Self::None, Self::Icons, Self::Text, Self::Centers];
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Icons => "Icons",
            Self::Text => "Text",
            Self::Centers => "Centers",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub terrain: bool,
    pub markers: Markers,
    pub no_build: bool,
    pub grid: bool,
    pub monument_data: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            terrain: true,
            markers: Markers::None,
            no_build: false,
            grid: false,
            monument_data: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Monument {
    pub id: String,
    pub name: String,
    pub prefab: Option<String>,
    pub world_position: [f32; 3],
    pub yaw_degrees: f32,
    pub pixel_center: [f32; 2],
    pub inside_image: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_pixel_bounds: Option<[f32; 4]>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub format: String,
    pub schema: u32,
    pub image_pixels: [usize; 2],
    pub map_size_metres: u32,
    /// Pixel-edge coordinates. Origin is the image's top-left, +Y points down.
    pub terrain_pixel_bounds: [f32; 4],
    pub north_up: bool,
    pub master_seed: u32,
    pub stage_seeds: std::collections::BTreeMap<String, u32>,
    pub installation: Option<crate::installation::Installation>,
    pub render: Settings,
    pub monuments: Vec<Monument>,
    pub no_build_complete: Option<bool>,
}
impl Metadata {
    pub fn new(project: &crate::Project, size: [usize; 2], map: Rect, settings: Settings) -> Self {
        Self {
            format: "mapper-image-metadata".into(),
            schema: 1,
            image_pixels: size,
            map_size_metres: project.size,
            terrain_pixel_bounds: [map.left(), map.top(), map.right(), map.bottom()],
            north_up: true,
            master_seed: project.master_seed,
            stage_seeds: crate::STAGES
                .iter()
                .zip(&project.seeds)
                .map(|(name, seed)| ((*name).into(), seed.value))
                .collect(),
            installation: project.installation.clone(),
            render: settings,
            monuments: Vec::new(),
            no_build_complete: None,
        }
    }
    pub fn add(
        &mut self,
        id: u64,
        name: String,
        prefab: Option<String>,
        position: [f32; 3],
        yaw: f32,
    ) -> Result<(), String> {
        if !(1000..=6000).contains(&self.map_size_metres)
            || self.monuments.len() >= 4096
            || name.len() > 512
            || prefab.as_ref().is_some_and(|p| p.len() > 1024)
            || position.iter().any(|v| !v.is_finite() || v.abs() > 10000.)
            || !yaw.is_finite()
        {
            return Err("Invalid image monument data".into());
        }
        let [left, top, right, bottom] = self.terrain_pixel_bounds;
        let point = pos2(
            left + (position[0] / self.map_size_metres as f32 + 0.5) * (right - left),
            top + (0.5 - position[2] / self.map_size_metres as f32) * (bottom - top),
        );
        self.monuments.push(Monument {
            id: id.to_string(),
            name,
            prefab,
            world_position: position,
            yaw_degrees: yaw,
            pixel_center: [point.x, point.y],
            inside_image: point.x >= 0.
                && point.y >= 0.
                && point.x < self.image_pixels[0] as f32
                && point.y < self.image_pixels[1] as f32,
            label_pixel_bounds: None,
        });
        Ok(())
    }
    pub fn json(&self) -> Result<String, String> {
        if self.format != "mapper-image-metadata"
            || self.schema != 1
            || !self.north_up
            || self.image_pixels.iter().any(|v| !(1..=4096).contains(v))
            || !(1000..=6000).contains(&self.map_size_metres)
            || self.terrain_pixel_bounds.iter().any(|v| !v.is_finite())
            || self.terrain_pixel_bounds[0] >= self.terrain_pixel_bounds[2]
            || self.terrain_pixel_bounds[1] >= self.terrain_pixel_bounds[3]
            || self.monuments.len() > 4096
            || self.monuments.iter().any(|m| {
                m.id.parse::<u64>().is_err()
                    || m.name.len() > 512
                    || m.prefab.as_ref().is_some_and(|p| p.len() > 1024)
                    || m.world_position
                        .iter()
                        .chain(&m.pixel_center)
                        .any(|v| !v.is_finite())
                    || !m.yaw_degrees.is_finite()
            })
        {
            return Err("Invalid image monument metadata".into());
        }
        if let Some(installation) = &self.installation {
            installation.validate()?;
        }
        let text = serde_json::to_string(self).map_err(|e| e.to_string())?;
        if text.len() > 4 * 1024 * 1024 {
            return Err("Image monument data exceeds its size limit".into());
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn north_up_coordinates_include_offshore_and_keep_large_ids_exact() {
        let project = crate::Project::default();
        let mut data = Metadata::new(
            &project,
            [1024; 2],
            Rect::from_min_max(pos2(100., 100.), pos2(924., 924.)),
            Settings::default(),
        );
        data.add(
            1 << 62,
            "Offshore".into(),
            None,
            [0., 2., project.size as f32],
            90.,
        )
        .unwrap();
        assert_eq!(data.monuments[0].pixel_center[0], 512.);
        assert!(!data.monuments[0].inside_image);
        let decoded: Metadata = serde_json::from_str(&data.json().unwrap()).unwrap();
        assert_eq!(decoded.monuments[0].id, "4611686018427387904");
        assert!(
            data.add(2, "Invalid".into(), None, [f32::NAN, 0., 0.], 0.)
                .is_err()
        );
    }
}
