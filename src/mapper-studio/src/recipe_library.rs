//! Local recipe library and the Home workspace.
use super::*;
use mapper_studio::profile::{Profile, write_document};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::Read, path::Path};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Item {
    name: String,
    size: u32,
    seed: u32,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Index {
    recipes: BTreeMap<u64, Item>,
    recent: Vec<PathBuf>,
    selected: Option<u64>,
    project_recipes: BTreeMap<PathBuf, BTreeMap<u64, u64>>,
}

#[derive(Default)]
pub(super) struct Library {
    directory: Option<PathBuf>,
    index: Index,
    saved: BTreeMap<u64, Profile>,
    dirty: bool,
    message: String,
    save_due: Option<std::time::Instant>,
}

impl Library {
    pub(super) fn can_autosave(&self) -> bool {
        self.directory.is_some() && self.message.is_empty()
    }

    fn profile_path(directory: &Path, key: u64) -> PathBuf {
        directory.join(format!("{key}.rec"))
    }

    fn load(directory: PathBuf) -> Result<Self, String> {
        let path = directory.join("index.json");
        let index: Index = match std::fs::File::open(path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
                if bytes.len() > 4 * 1024 * 1024 {
                    return Err("Recipe library index exceeds 4 MiB".into());
                }
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Index::default(),
            Err(error) => return Err(error.to_string()),
        };
        Ok(Self { directory: Some(directory), index, ..Self::default() })
    }

    fn lock_for_write(&mut self) -> Result<std::fs::File, String> {
        let directory = self.directory.as_ref().ok_or("Recipe library is unavailable")?;
        std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
        let lock = std::fs::OpenOptions::new().read(true).write(true)
            .create(true).truncate(false).open(directory.join("library.lock")).map_err(|e| e.to_string())?;
        lock.try_lock().map_err(|e| e.to_string())?;
        let mut index = Self::load(directory.clone())?.index;
        if self.dirty {
            let mut recent = std::mem::take(&mut self.index.recent);
            for path in index.recent {
                if !recent.contains(&path) {
                    recent.push(path);
                }
            }
            recent.truncate(20);
            index.recent = recent;
        }
        self.index = index;
        Ok(lock)
    }

    pub(super) fn clear_open_cache(&mut self) {
        self.saved.clear();
    }

    fn read(&mut self, key: u64) -> Result<Profile, String> {
        if let Some(profile) = self.saved.get(&key) {
            return Ok(profile.clone());
        }
        let directory = self.directory.as_deref().ok_or("Recipe library is unavailable")?;
        Profile::load_snapshot(&Self::profile_path(directory, key))
    }

    fn save(&mut self, key: Option<u64>, name: &str, recipe: &Project) -> Result<u64, String> {
        if let Some(key) = key
            && self.index.recipes.contains_key(&key)
            && self.saved.get(&key).is_some_and(|saved| saved.name == name && saved.recipe == *recipe)
        {
            return Ok(key);
        }
        let directory = self.directory.as_deref().ok_or("Recipe library is unavailable")?;
        let key = match key {
            Some(key) => key,
            None => {
                let mut key = self.index.recipes.last_key_value().map_or(0, |(&key, _)| key);
                loop {
                    key = key.checked_add(1).ok_or("Recipe library has run out of IDs")?;
                    if !Self::profile_path(directory, key).exists() {
                        break key;
                    }
                }
            }
        };
        let profile = Profile::new(name.to_owned(), recipe.clone());
        profile.validate()?;
        std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
        write_document(&Self::profile_path(directory, key), &profile, true)?;
        let item = Item { name: name.to_owned(), size: recipe.size, seed: recipe.master_seed };
        if self.index.recipes.get(&key) != Some(&item) {
            self.index.recipes.insert(key, item);
            self.dirty = true;
        }
        self.saved.insert(key, profile);
        Ok(key)
    }

    fn publish(&mut self) -> Result<(), String> {
        if !self.dirty {
            return Ok(());
        }
        let directory = self.directory.as_deref().ok_or("Recipe library is unavailable")?;
        std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
        write_document(&directory.join("index.json"), &self.index, true)?;
        self.dirty = false;
        Ok(())
    }
}

impl Studio {
    pub(super) fn load_recipe_library(&mut self, restore: bool) {
        let Some(settings) = &self.settings_path else { return; };
        let Some(directory) = settings.parent().map(|p| p.join("recipes")) else { return; };
        match Library::load(directory) {
            Ok(library) => self.recipe_library = library,
            Err(error) => {
                self.recipe_library.message = format!("Could not open saved recipes. {error}");
                return;
            }
        }
        if !restore { return; }
        if let Some(key) = self.recipe_library.index.selected {
            match self.recipe_library.read(key) {
                Ok(profile) => {
                    self.recipe_tabs.restore_first(profile.name, key);
                    self.project = profile.recipe;
                    self.saved = self.project.clone();
                }
                Err(error) => self.recipe_library.message = format!("Could not reopen the last recipe. {error}"),
            }
        } else if !self.recipe_library.index.recipes.is_empty() {
            self.recipe_tabs.close(FIRST_RECIPE);
            self.recipe_tabs.rebuild_tabs(&mut self.dock);
        }
    }

    fn skip_default_recipe(&self, id: u64, name: &str) -> bool {
        id == FIRST_RECIPE && name == "Recipe" && self.path.is_none()
            && !self.recipe_tabs.library_keys.contains_key(&id)
            && self.recipe_by_id(id).master_seed == 0
            && self.recipe_by_id(id) == &self.saved
    }

    pub(super) fn tick_recipe_library(&mut self, ctx: &egui::Context) {
        let library = &self.recipe_library;
        if library.directory.is_none() { return; }
        let active = self.recipe_tabs.active();
        let selected = (!self.recipe_tabs.closed.contains(&active))
            .then(|| self.recipe_tabs.library_keys.get(&active).copied()).flatten();
        let changed = library.dirty || library.index.selected != selected
            || self.recipe_tabs.entries().into_iter().any(|(id, name)| {
                !self.skip_default_recipe(id, &name)
                    && self.recipe_tabs.library_keys.get(&id).and_then(|key| library.saved.get(key))
                        .is_none_or(|saved| saved.name != name || &saved.recipe != self.recipe_by_id(id))
            });
        if !changed && self.recipe_library.save_due.is_none() { return; }
        let now = std::time::Instant::now();
        let due = *self.recipe_library.save_due.get_or_insert(now + std::time::Duration::from_millis(450));
        if now >= due {
            self.persist_recipe_library();
        } else {
            ctx.request_repaint_after(due - now);
        }
    }

    pub(super) fn restore_project_recipe_links(&mut self, path: &Path) {
        let Some(known) = self.recipe_library.index.project_recipes.get(path).cloned() else { return; };
        for (id, name) in self.recipe_tabs.entries() {
            if let Some(&key) = known.get(&id)
                && let Ok(saved) = self.recipe_library.read(key)
                && saved.name == name && &saved.recipe == self.recipe_by_id(id)
            {
                self.recipe_tabs.library_keys.insert(id, key);
                self.recipe_library.saved.insert(key, saved);
            }
        }
    }

    pub(super) fn persist_recipe_library(&mut self) {
        if self.recipe_library.directory.is_none() {
            return;
        }
        let active = self.recipe_tabs.active();
        let mut library = std::mem::take(&mut self.recipe_library);
        library.save_due = None;
        let result = (|| {
            let _lock = library.lock_for_write()?;
            for (id, name) in self.recipe_tabs.entries() {
                if self.skip_default_recipe(id, &name) { continue; }
                let recipe = self.recipe_by_id(id);
                let key = library.save(self.recipe_tabs.library_keys.get(&id).copied(), &name, recipe)?;
                self.recipe_tabs.library_keys.insert(id, key);
            }
            let selected = (!self.recipe_tabs.closed.contains(&active))
                .then(|| self.recipe_tabs.library_keys.get(&active).copied()).flatten();
            if library.index.selected != selected {
                library.index.selected = selected;
                library.dirty = true;
            }
            if let Some(path) = &self.path
                && !self.recipe_tabs.library_keys.is_empty()
                && library.index.project_recipes.get(path) != Some(&self.recipe_tabs.library_keys)
            {
                library.index.project_recipes.insert(path.clone(), self.recipe_tabs.library_keys.clone());
                library.dirty = true;
            }
            library.publish()
        })();
        library.message = result.err().map_or_else(String::new, |error| format!("Could not save recipes. {error}"));
        self.recipe_library = library;
    }

    pub(super) fn remember_recent_project(&mut self, path: PathBuf) {
        let library = &mut self.recipe_library;
        if library.directory.is_none() || library.index.recent.first() == Some(&path) {
            return;
        }
        library.index.recent.retain(|other| other != &path);
        library.index.recent.insert(0, path);
        library.index.recent.truncate(20);
        library.dirty = true;
    }

    fn open_library_recipe(&mut self, key: u64) {
        if let Some(id) = self.recipe_tabs.library_keys.iter().find_map(|(&id, &saved)| (saved == key).then_some(id)) {
            self.activate_recipe(id);
            self.show_tab(Tab::Recipe(id));
            return;
        }
        match self.recipe_library.read(key) {
            Ok(profile) => {
                if let Some(id) = self.add_named_recipe(profile.name, profile.recipe) {
                    self.recipe_tabs.library_keys.insert(id, key);
                    self.show_tab(Tab::Recipe(id));
                } else {
                    self.recipe_library.message = "This project has reached its recipe limit.".into();
                }
            }
            Err(error) => self.recipe_library.message = format!("Could not open this recipe. {error}"),
        }
    }

    pub(super) fn home(&mut self, ui: &mut egui::Ui) {
        let search_id = ui.id().with("home-search");
        let mut query = ui.data(|data| data.get_temp::<String>(search_id)).unwrap_or_default();
        let mut recipe = None;
        let mut project = None;
        egui::Frame::NONE.inner_margin(egui::Margin::same(20)).show(ui, |ui| {
            ui.add(egui::Label::new(egui::RichText::new("Home").size(24.).strong()));
            ui.add_space(3.);
            ui.horizontal_wrapped(|ui| {
                if ui.button("Open project…").clicked() {
                    self.request(Action::Open, ui.ctx());
                }
                if ui.add_enabled(self.recipe_tabs.can_add(), egui::Button::new("New recipe")).clicked() {
                    self.recipe_tabs.pending_new = Some((self.recipe_tabs.active(), false));
                }
            });
            ui.add_space(10.);
            ui.add(egui::TextEdit::singleline(&mut query).hint_text("Search recipes and projects")
                .desired_width(ui.available_width().min(330.)));
            let query = query.trim().to_lowercase();
            ui.add_space(14.);
            ui.label(egui::RichText::new("Recipes").size(16.).strong());
            for (&key, item) in self.recipe_library.index.recipes.iter().rev() {
                if !item.name.to_lowercase().contains(&query) { continue; }
                ui.horizontal(|ui| {
                    let width = (ui.available_width() - 115.).max(48.);
                    if ui.add_sized([width, 30.], egui::Button::new(&item.name).truncate()).clicked() {
                        recipe = Some(key);
                    }
                    ui.weak(if item.seed == 0 { format!("{} m", item.size) }
                        else { format!("{} m · {}", item.size, item.seed) });
                });
            }
            if self.recipe_library.index.recipes.is_empty() {
                ui.weak("Recipes you edit in Mapper appear here.");
            }
            ui.add_space(16.);
            ui.label(egui::RichText::new("Recent projects").size(16.).strong());
            for path in &self.recipe_library.index.recent {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                if !name.to_lowercase().contains(&query) { continue; }
                if ui.add(egui::Button::new(name.as_ref()).truncate()).on_hover_text(path.display().to_string()).clicked() {
                    project = Some(path.clone());
                }
            }
            if self.recipe_library.index.recent.is_empty() {
                ui.weak("Open or save a project to add it here.");
            }
            if !self.recipe_library.message.is_empty() {
                ui.colored_label(ui.visuals().warn_fg_color, &self.recipe_library.message);
            }
        });
        ui.data_mut(|data| data.insert_temp(search_id, query));
        if let Some(key) = recipe {
            self.open_library_recipe(key);
        }
        if let Some(path) = project {
            self.open_project_from(path, ui.ctx());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recipe_library_reopens_the_same_record_and_keeps_recent_projects() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("recipes");
        let mut lib = Library::load(folder.clone()).unwrap();
        let mut recipe = Project::default();
        recipe.master_seed = 24680;
        recipe.refresh_inherited_seeds();
        let key = lib.save(None, "Islands", &recipe).unwrap();
        lib.index.recent.push(root.path().join("Wipe.mapper"));
        lib.index.selected = Some(key);
        lib.publish().unwrap();

        let mut lib = Library::load(folder).unwrap();
        assert_eq!(lib.index.selected, Some(key));
        let saved = lib.read(key).unwrap();
        assert_eq!(saved.recipe, recipe);
        assert_eq!(saved.name, "Islands");
        assert_eq!(lib.index.recent[0].file_name().unwrap(), "Wipe.mapper");
        assert_eq!(lib.save(Some(key), "Archipelago", &recipe).unwrap(), key);
        assert_eq!(lib.index.recipes.len(), 1);
    }
}

#[cfg(test)]
#[test]
fn a_stale_window_adds_recipes_without_replacing_another_windows_record() {
    let root = tempfile::tempdir().unwrap();
    let mut first = Library::load(root.path().to_owned()).unwrap();
    let mut second = Library::load(root.path().to_owned()).unwrap();
    {
        let _lock = first.lock_for_write().unwrap();
        first.save(None, "First", &Project::default()).unwrap();
        first.publish().unwrap();
    }
    {
        let _lock = second.lock_for_write().unwrap();
        second.save(None, "Second", &Project::default()).unwrap();
        second.publish().unwrap();
    }
    let merged = Library::load(root.path().to_owned()).unwrap();
    assert_eq!(merged.index.recipes.len(), 2);
}
