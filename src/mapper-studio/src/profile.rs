//! Portable Mapper Recipes, applied as one undoable project edit.
use crate::{MAX_PROJECT_BYTES, Project};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub format: String,
    pub schema: u32,
    pub name: String,
    pub recipe: Project,
}
impl Profile {
    pub fn new(name: String, recipe: Project) -> Self {
        Self {
            format: "mapper-recipe".into(),
            schema: 1,
            name,
            recipe,
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(
            self.format.as_str(),
            "mapper-recipe" | "papyrus-generation-profile"
        ) || self.schema != 1
        {
            return Err("Unsupported Mapper Recipe version".into());
        }
        if self.name.trim().is_empty()
            || self.name.len() > 256
            || self.name.chars().any(char::is_control)
        {
            return Err("Recipe name must contain 1–256 bytes without control characters".into());
        }
        self.recipe.validate()
    }
    pub fn save(&self, path: &Path) -> Result<(), String> {
        write_json(path, &self.prepare_export()?)
    }
    pub fn prepare_export(&self) -> Result<Self, String> {
        self.validate()?;
        let mut value = self.clone();
        value.format = "mapper-recipe".into();
        if let Some(rivers) = &mut value.recipe.rivers {
            let installation = value
                .recipe
                .installation
                .as_ref()
                .ok_or("River export requires a saved Rust installation identity")?;
            *rivers = rivers.prepare_export(installation)?;
        }
        Ok(value)
    }
    pub fn load(path: &Path) -> Result<Self, String> {
        let mut value = Self::load_snapshot(path)?;
        value.format = "mapper-recipe".into();
        crate::generation_defaults::apply(&mut value.recipe);
        value.validate()?;
        Ok(value)
    }
    /// Read the saved inputs without applying generation defaults from this build.
    pub fn load_snapshot(path: &Path) -> Result<Self, String> {
        let value: Self = read_json(path)?;
        value.validate()?;
        Ok(value)
    }
}
pub(crate) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(MAX_PROJECT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_PROJECT_BYTES {
        return Err("Document exceeds 4 MiB".into());
    }
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}
pub(crate) fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    write_document(path, value, true)
}
/// Atomic document publication, with no-clobber behavior for unattended callers.
pub fn write_document(path: &Path, value: &impl Serialize, overwrite: bool) -> Result<(), String> {
    write_document_checked(path, value, overwrite, || Ok(()))
}

pub(crate) fn write_document_checked(
    path: &Path,
    value: &impl Serialize,
    overwrite: bool,
    check: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_PROJECT_BYTES {
        return Err("Document exceeds 4 MiB".into());
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    temp.write_all(&bytes).map_err(|e| e.to_string())?;
    temp.as_file().sync_all().map_err(|e| e.to_string())?;
    check()?;
    if overwrite {
        temp.persist(path).map_err(|e| e.error.to_string())?;
    } else {
        temp.persist_noclobber(path)
            .map_err(|e| e.error.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn earlier_recipe_documents_import_and_export_with_mapper_recipe_format() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("earlier.json");
        let mut earlier = Profile::new("Saved settings".into(), Project::default());
        earlier.format = "papyrus-generation-profile".into();
        write_json(&input, &earlier).unwrap();
        let loaded = Profile::load(&input).unwrap();
        assert_eq!(loaded.format, "mapper-recipe");
        assert_eq!(loaded.recipe, earlier.recipe);
        let output = directory.path().join("settings.rec");
        earlier.save(&output).unwrap();
        assert_eq!(
            read_json::<Profile>(&output).unwrap().format,
            "mapper-recipe"
        );
        assert_eq!(Profile::load(&output).unwrap(), loaded);
    }
    #[test]
    fn invalid_embedded_river_data_cannot_replace_an_export() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing.rec");
        std::fs::write(&path, b"existing export").unwrap();
        let mut value = Profile::new("Rivers".into(), Project::default());
        value.recipe.preview_engine = crate::native_preview::VERSION.into();
        value.recipe.installation = Some(crate::installation::Installation {
            kind: crate::installation::InstallationKind::Client,
            native: crate::installation::SourceFile {
                relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                sha256: "a".repeat(64),
                bytes: 1,
            },
            scene_manifest: None,
        });
        value.recipe.rivers = Some(crate::river_generation::Recipe {
            engine: papyrus_terrain::river_pipeline::VERSION.into(),
            source_sha256: "b".repeat(64),
            source_document: Some("{}".into()),
            objects: None,
        });
        value.validate().unwrap();
        assert!(value.save(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"existing export");
    }
    #[test]
    fn portable_profile_roundtrip_and_rejection_preserves_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("islands.rec");
        let mut profile = Profile::new("Islands".into(), Project::default());
        profile.recipe.shuffle_monuments();
        profile.recipe.performance.monuments = false;
        profile.recipe.placement_engine = crate::placement_preview::ROTATION_VERSION.into();
        profile.recipe.targets[0].yaw = Some(45.);
        profile.save(&path).unwrap();
        assert_eq!(Profile::load(&path).unwrap(), profile);
        let original = std::fs::read(&path).unwrap();
        profile.schema = 99;
        assert!(profile.save(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let mut value =
            serde_json::to_value(Profile::new("Test".into(), Project::default())).unwrap();
        value["node_graph"] = serde_json::json!({"unimplemented": true});
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(Profile::load(&path).is_err()); // Never silently discard a future graph.
    }
}
