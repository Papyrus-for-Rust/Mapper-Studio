//! Named local installation registrations, separate from portable source receipts.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: u64,
    pub name: String,
    pub path: PathBuf,
}
pub fn key(path: &Path) -> String {
    let value = path
        .to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_owned();
    if cfg!(windows) {
        value.to_lowercase()
    } else {
        value
    }
}
pub fn default_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("Rust")
        .chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect()
}
pub fn validate(entries: &[Entry], selected: Option<u64>) -> Result<(), String> {
    let mut ids = std::collections::BTreeSet::new();
    let mut paths = std::collections::BTreeSet::new();
    if entries.len() > 32
        || entries.iter().any(|e| {
            e.id == 0
                || !ids.insert(e.id)
                || !paths.insert(key(&e.path))
                || e.path.as_os_str().is_empty()
                || e.path.to_string_lossy().len() > 4096
                || e.name.trim().is_empty()
                || e.name.chars().count() > 80
                || e.name.chars().any(char::is_control)
        })
        || selected.is_some_and(|id| !ids.contains(&id))
    {
        return Err("Invalid saved Rust installations".into());
    }
    Ok(())
}
impl crate::preferences::Preferences {
    pub fn migrate_installations(&mut self) -> Result<bool, String> {
        if self.installations.is_empty()
            && let Some(path) = self.rust_folder.clone()
        {
            self.remember_installation(path, true)?;
            return Ok(true);
        }
        Ok(false)
    }
    pub fn remember_installation(&mut self, path: PathBuf, select: bool) -> Result<u64, String> {
        let id = if let Some(entry) = self
            .installations
            .iter()
            .find(|e| key(&e.path) == key(&path))
        {
            entry.id
        } else {
            if self.installations.len() >= 32 {
                return Err("Up to 32 Rust installations can be saved".into());
            }
            let id = self
                .installations
                .iter()
                .map(|e| e.id)
                .max()
                .unwrap_or(0)
                .checked_add(1)
                .ok_or("Installation identifier exhausted")?;
            let entry = Entry {
                id,
                name: default_name(&path),
                path,
            };
            validate(std::slice::from_ref(&entry), None)?;
            self.installations.push(entry);
            id
        };
        if select {
            self.select_installation(id)?;
        }
        Ok(id)
    }
    pub fn select_installation(&mut self, id: u64) -> Result<PathBuf, String> {
        let entry = self
            .installations
            .iter()
            .find(|e| e.id == id)
            .ok_or("Saved installation unavailable")?;
        let path = entry.path.clone();
        self.rust_folder = Some(path.clone());
        self.selected_installation = Some(id);
        Ok(path)
    }
    pub fn forget_installation(&mut self, id: u64) -> bool {
        let active = self.selected_installation == Some(id);
        self.installations.retain(|e| e.id != id);
        if active {
            self.selected_installation = None;
            self.rust_folder = None;
        }
        active
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn legacy_folder_migrates_once_names_and_selection_survive_roundtrip_and_removal() {
        let mut p: crate::preferences::Preferences =
            serde_json::from_str(r#"{"schema":1,"rust_folder":"C:/Steam/Rust"}"#).unwrap();
        assert!(p.migrate_installations().unwrap());
        assert!(!p.migrate_installations().unwrap());
        let regular = p.selected_installation.unwrap();
        let staging = p
            .remember_installation("C:/Steam/Rust Staging".into(), false)
            .unwrap();
        assert_eq!(p.selected_installation, Some(regular));
        p.installations[1].name = "Staging / test build".into();
        p.select_installation(staging).unwrap();
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join("settings.json");
        p.save(&file).unwrap();
        assert_eq!(crate::preferences::Preferences::load(&file).unwrap(), p);
        assert!(!p.forget_installation(regular));
        assert_eq!(p.selected_installation, Some(staging));
        assert!(p.forget_installation(staging));
        assert!(p.rust_folder.is_none());
        assert!(p.installations.is_empty());
    }
    #[test]
    fn duplicate_paths_do_not_create_entries_and_bad_names_cannot_replace_settings() {
        let mut p = crate::preferences::Preferences::default();
        let id = p
            .remember_installation("C:/Steam/Rust/".into(), true)
            .unwrap();
        assert_eq!(
            p.remember_installation("C:/Steam/Rust".into(), false)
                .unwrap(),
            id
        );
        assert_eq!(p.installations.len(), 1);
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join("settings.json");
        p.save(&file).unwrap();
        let old = std::fs::read(&file).unwrap();
        p.installations[0].name = "\n".into();
        assert!(p.save(&file).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), old);
    }
}
