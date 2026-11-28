//! Machine-local preferences; portable recipes retain source fingerprints only.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DEFAULT_UI_SCALE: u16 = 100;
pub const UI_SCALE_PRESETS: [u16; 6] = [75, 90, DEFAULT_UI_SCALE, 110, 125, 150];

fn is_default_ui_scale(percent: &u16) -> bool {
    *percent == DEFAULT_UI_SCALE
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    System,
    #[default]
    Dark,
    Light,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageResolution {
    Px1024,
    #[default]
    Px2048,
    Px4096,
}
impl ImageResolution {
    pub const ALL: [Self; 3] = [Self::Px1024, Self::Px2048, Self::Px4096];
    pub fn pixels(self) -> usize {
        match self {
            Self::Px1024 => 1024,
            Self::Px2048 => 2048,
            Self::Px4096 => 4096,
        }
    }
    pub fn label(self) -> String {
        format!("{} × {} px", self.pixels(), self.pixels())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MapSizeSnap {
    Off,
    Meters25,
    #[default]
    Meters50,
    Meters100,
    Meters250,
    Custom(u32),
    Disabled(u32),
}

impl MapSizeSnap {
    pub const MAX_DISTANCE: u32 = 5000;

    pub fn enabled(self) -> bool {
        !matches!(self, Self::Off | Self::Disabled(_))
    }

    pub fn distance(self) -> u32 {
        match self {
            Self::Meters25 => 25,
            Self::Off | Self::Meters50 => 50,
            Self::Meters100 => 100,
            Self::Meters250 => 250,
            Self::Custom(distance) | Self::Disabled(distance) => distance.clamp(1, Self::MAX_DISTANCE),
        }
    }

    pub fn with_distance(self, distance: u32) -> Self {
        let distance = distance.clamp(1, Self::MAX_DISTANCE);
        if self.enabled() {
            match distance {
                25 => Self::Meters25,
                50 => Self::Meters50,
                100 => Self::Meters100,
                250 => Self::Meters250,
                _ => Self::Custom(distance),
            }
        } else if distance == 50 {
            Self::Off
        } else {
            Self::Disabled(distance)
        }
    }

    pub fn toggled(self) -> Self {
        let other = if self.enabled() { Self::Off } else { Self::default() };
        other.with_distance(self.distance())
    }

    pub fn step(self) -> f64 {
        if self.enabled() { f64::from(self.distance()) } else { 0. }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Preferences {
    pub schema: u32,
    pub rust_folder: Option<PathBuf>,
    pub installations: Vec<crate::installations::Entry>,
    pub selected_installation: Option<u64>,
    pub appearance: Appearance,
    #[serde(skip_serializing_if = "is_default_ui_scale")]
    pub ui_scale_percent: u16,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub reduce_motion: bool,
    pub live_generation: bool,
    pub check_for_updates: bool,
    pub map_size_snap: MapSizeSnap,
    #[serde(rename = "random_master_seed", skip_serializing)]
    #[doc(hidden)]
    pub _random_master_seed: bool,
    #[serde(rename = "floating_tools", skip_serializing)]
    #[doc(hidden)]
    pub _floating_tools: bool,
    #[serde(rename = "tools_docked_right", skip_serializing)]
    #[doc(hidden)]
    pub _tools_docked_right: bool,
    #[serde(rename = "tools_single_column", skip_serializing)]
    #[doc(hidden)]
    pub _tools_single_column: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub map_grid: bool,
    pub map_no_build: bool,
    pub monument_splats: bool,
    pub monument_icons: crate::monument_icons::Overrides,
    pub monument_markers: crate::map_labels::Mode,
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub monument_display_names: std::collections::BTreeMap<String, String>,
    #[serde(skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub hidden_monument_markers: std::collections::BTreeSet<String>,
    pub decoration_models: bool,
    pub export_resolution: ImageResolution,
    pub image_render: crate::image_render::Settings,
    pub export_folder: Option<PathBuf>,
    pub map_colors: crate::map_colors::Preferences,
    pub generation_resources: crate::generation_resources::Limits,
    pub terrain_memory_mode: crate::generation_resources::TerrainMemoryMode,
    pub terrain_ram_target_percent: usize,
    pub topology_groups: Vec<crate::topology_presets::Preset>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            schema: 1,
            rust_folder: None,
            installations: Vec::new(),
            selected_installation: None,
            appearance: Appearance::Dark,
            ui_scale_percent: DEFAULT_UI_SCALE,
            reduce_motion: false,
            live_generation: true,
            check_for_updates: true,
            map_size_snap: MapSizeSnap::default(),
            _random_master_seed: false,
            _floating_tools: false,
            _tools_docked_right: false,
            _tools_single_column: false,
            map_grid: false,
            map_no_build: false,
            monument_splats: true,
            monument_icons: Default::default(),
            monument_markers: Default::default(),
            monument_display_names: Default::default(),
            hidden_monument_markers: Default::default(),
            decoration_models: true,
            export_resolution: Default::default(),
            image_render: Default::default(),
            export_folder: None,
            map_colors: Default::default(),
            generation_resources: Default::default(),
            terrain_memory_mode: Default::default(),
            terrain_ram_target_percent: crate::generation_resources::DEFAULT_RAM_TARGET_PERCENT,
            topology_groups: Vec::new(),
        }
    }
}
impl Preferences {
    pub fn monument_label<'a>(&'a self, path: Option<&str>, fallback: &'a str) -> &'a str {
        path.and_then(|path| self.monument_display_names.get(path))
            .map_or(fallback, String::as_str)
    }

    pub fn monument_marker_hidden(&self, path: Option<&str>) -> bool {
        path.is_some_and(|path| self.hidden_monument_markers.contains(path))
    }

    fn validate_monument_display(&self) -> Result<(), String> {
        if self.monument_display_names.len() > 128 || self.hidden_monument_markers.len() > 128
            || self.monument_display_names.iter().any(|(path, name)| {
                !crate::monument_icons::valid_path(path) || name.trim().is_empty()
                    || name.len() > 128 || name.chars().any(char::is_control)
            })
            || self.hidden_monument_markers.iter().any(|path| !crate::monument_icons::valid_path(path))
        {
            return Err("Invalid monument display preferences".into());
        }
        Ok(())
    }

    /// Auto is sampled at admission. The fallback retains the saved fixed budget.
    pub fn auto_terrain_headroom(&self) -> Option<usize> {
        use crate::generation_resources::{TerrainMemoryMode, terrain_ram_headroom_mib};
        if self.terrain_memory_mode == TerrainMemoryMode::Auto {
            terrain_ram_headroom_mib(self.terrain_ram_target_percent)
        } else {
            None
        }
    }

    fn validate_resource_preferences(&self) -> Result<(), String> {
        self.generation_resources.validate()?;
        if !crate::generation_resources::RAM_TARGET_RANGE.contains(&self.terrain_ram_target_percent) {
            return Err("Invalid automatic terrain RAM target".into());
        }
        Ok(())
    }

    pub fn path() -> Result<PathBuf, String> {
        let root = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .ok_or("Local application data folder is unavailable")?;
        Ok(root.join("Papyrus/Mapper/settings.json"))
    }
    pub fn load(path: &Path) -> Result<Self, String> {
        use std::io::Read;
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.to_string()),
        };
        let mut bytes = Vec::new();
        file.take(65537)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 65536 {
            return Err("Settings exceed 64 KiB".into());
        }
        let document: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let legacy = document.get("terrain_memory_mode").is_none();
        let mut prefs: Self = serde_json::from_value(document).map_err(|e| e.to_string())?;
        if legacy && prefs.generation_resources.terrain_memory_mib
            != crate::generation_resources::Limits::default().terrain_memory_mib
        {
            prefs.terrain_memory_mode = crate::generation_resources::TerrainMemoryMode::Manual;
        }
        if !UI_SCALE_PRESETS.contains(&prefs.ui_scale_percent) {
            prefs.ui_scale_percent = DEFAULT_UI_SCALE;
        }
        if prefs.schema != 1 {
            return Err("Unsupported settings version; existing file retained".into());
        }
        crate::installations::validate(&prefs.installations, prefs.selected_installation)?;
        prefs.validate_resource_preferences()?;
        crate::monument_icons::validate(&prefs.monument_icons)?;
        prefs.validate_monument_display()?;
        crate::topology_presets::validate(&prefs.topology_groups)?;
        Ok(prefs)
    }
    pub fn save(&self, path: &Path) -> Result<(), String> {
        crate::installations::validate(&self.installations, self.selected_installation)?;
        self.validate_resource_preferences()?;
        crate::monument_icons::validate(&self.monument_icons)?;
        self.validate_monument_display()?;
        crate::topology_presets::validate(&self.topology_groups)?;
        if serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > 65536 {
            return Err("Settings exceed 64 KiB".into());
        }
        if self.schema != 1 {
            return Err("Unsupported settings version".into());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        crate::profile::write_document(path, self, true)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_terrain_budget_migrates_to_auto_and_custom_limits_stay_manual() {
        use crate::generation_resources::TerrainMemoryMode;
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        assert_eq!(Preferences::load(&settings).unwrap().terrain_ram_target_percent, 80);
        for (budget, mode) in [(2048, TerrainMemoryMode::Auto), (4096, TerrainMemoryMode::Manual)] {
            std::fs::write(&settings, format!(r#"{{"generation_resources":{{"terrain_memory_mib":{budget}}}}}"#)).unwrap();
            let mut prefs = Preferences::load(&settings).unwrap();
            assert_eq!(prefs.terrain_memory_mode, mode);
            assert_eq!(prefs.generation_resources.terrain_memory_mib, budget);
            prefs.terrain_memory_mode = TerrainMemoryMode::Auto;
            prefs.terrain_ram_target_percent = 75;
            prefs.save(&settings).unwrap();
            assert_eq!(Preferences::load(&settings).unwrap(), prefs);
        }
    }

    #[test]
    fn image_export_preferences_upgrade_old_settings_and_reject_unknown_resolutions() {
        let old: Preferences = serde_json::from_str(r#"{"schema":1,"rust_folder":null}"#).unwrap();
        assert_eq!(old.export_resolution, ImageResolution::Px2048);
        assert_eq!(old.image_render, crate::image_render::Settings::default());
        assert!(old.export_folder.is_none());
        assert!(old.decoration_models);
        assert!(!old.reduce_motion);
        assert_eq!(old.ui_scale_percent, DEFAULT_UI_SCALE);
        assert_eq!(old.map_size_snap, MapSizeSnap::Meters50);
        let settings = Preferences {
            export_resolution: ImageResolution::Px4096,
            export_folder: Some(PathBuf::from("C:/Maps/exports")),
            ..Default::default()
        };
        let bytes = serde_json::to_vec(&settings).unwrap();
        assert_eq!(
            serde_json::from_slice::<Preferences>(&bytes).unwrap(),
            settings
        );
        assert!(
            serde_json::from_str::<Preferences>(r#"{"schema":1,"export_resolution":"px8192"}"#)
                .is_err()
        );
    }
    #[test]
    fn image_render_settings_survive_restart_and_viewport_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        for markers in crate::image_render::Markers::ALL {
            let mut prefs = Preferences {
                image_render: crate::image_render::Settings {
                    terrain: false,
                    markers,
                    no_build: true,
                    grid: true,
                    monument_data: false,
                },
                export_resolution: ImageResolution::Px4096,
                export_folder: Some(dir.path().join("exports")),
                ..Default::default()
            };
            prefs.save(&path).unwrap();
            let reopened = Preferences::load(&path).unwrap();
            assert_eq!(reopened, prefs);

            prefs = reopened;
            prefs.map_grid = true;
            prefs.map_no_build = true;
            prefs.monument_markers = crate::map_labels::Mode::Text;
            prefs.save(&path).unwrap();
            let reopened = Preferences::load(&path).unwrap();
            assert_eq!(reopened.image_render, prefs.image_render);
            assert_eq!(reopened.export_resolution, ImageResolution::Px4096);
            assert_eq!(reopened.export_folder, prefs.export_folder);
        }
    }
    #[test]
    fn preferences_roundtrip_preserves_folder_and_rejects_future_or_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/settings.json");
        assert_eq!(Preferences::load(&path).unwrap(), Preferences::default());
        let p = Preferences {
            rust_folder: Some(PathBuf::from("C:/Games/Rust old")),
            appearance: Appearance::Light,
            ui_scale_percent: 125,
            reduce_motion: true,
            live_generation: false,
            map_grid: true,
            monument_splats: false,
            decoration_models: false,
            ..Default::default()
        };
        p.save(&path).unwrap();
        assert_eq!(Preferences::load(&path).unwrap(), p);
        let bytes = std::fs::read(&path).unwrap();
        let mut invalid = p;
        invalid.schema = 2;
        assert!(invalid.save(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::write(&path, br#"{"schema":2}"#).unwrap();
        assert!(Preferences::load(&path).is_err());
        std::fs::write(&path, vec![b' '; 65537]).unwrap();
        assert!(Preferences::load(&path).is_err());
    }

    #[test]
    fn topology_groups_persist_and_invalid_updates_preserve_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut prefs = Preferences::default();
        prefs.topology_groups.push(crate::topology_presets::Preset {
            name: "Spawn area".into(),
            layers: vec!["Field".into(), "PlayerSpawn".into()],
        });
        prefs.save(&path).unwrap();
        assert_eq!(Preferences::load(&path).unwrap(), prefs);
        let before = std::fs::read(&path).unwrap();
        prefs.topology_groups[0].layers.clear();
        assert!(prefs.save(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let legacy: Preferences = serde_json::from_str("{\"schema\":1}").unwrap();
        assert!(legacy.topology_groups.is_empty());
    }
}
