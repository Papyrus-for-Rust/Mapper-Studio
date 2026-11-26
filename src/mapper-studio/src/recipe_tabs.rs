use super::*;
use serde::{Deserialize, Serialize};

pub(super) const FIRST_RECIPE: u64 = 1;
const MAX_RECIPES: usize = 32;
const MAX_NAME_CHARS: usize = 64;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecipeBook {
    active: u64,
    entries: Vec<RecipeEntry>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecipeEntry {
    id: u64,
    name: String,
    // The active recipe lives in the existing top-level project field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recipe: Option<Project>,
}

impl Default for RecipeBook {
    fn default() -> Self {
        Self {
            active: FIRST_RECIPE,
            entries: vec![RecipeEntry {
                id: FIRST_RECIPE,
                name: "Recipe".into(),
                recipe: None,
            }],
        }
    }
}

impl RecipeBook {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.entries.is_empty() || self.entries.len() > MAX_RECIPES {
            return Err("Invalid number of saved recipes".into());
        }
        let mut ids = std::collections::HashSet::new();
        for entry in &self.entries {
            if entry.id == 0
                || !ids.insert(entry.id)
                || !valid_name(&entry.name)
                || entry.recipe.is_none() != (entry.id == self.active)
            {
                return Err("Invalid saved recipe tab".into());
            }
            if let Some(recipe) = &entry.recipe {
                recipe.validate()?;
            }
        }
        if !ids.contains(&self.active) {
            return Err("The active recipe is missing".into());
        }
        Ok(())
    }

    pub(super) fn is_legacy_default(&self) -> bool {
        self.entries.len() == 1 && self.active == FIRST_RECIPE && self.entries[0].name == "Recipe"
    }

    fn entry_mut(&mut self, id: u64) -> Option<&mut RecipeEntry> {
        self.entries.iter_mut().find(|entry| entry.id == id)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RecipeOrigin {
    pub(super) id: u64,
    pub(super) name: String,
}

impl RecipeOrigin {
    pub(super) fn valid(&self) -> bool {
        self.id != 0 && valid_name(&self.name)
    }
}

fn valid_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.chars().count() <= MAX_NAME_CHARS
        && !name.chars().any(char::is_control)
}

#[derive(Default)]
pub(super) struct RecipeTabs {
    pub(super) book: RecipeBook,
    pub(super) revision: u64,
    histories: std::collections::HashMap<u64, History>,
    work: std::collections::HashMap<u64, RecipeWork>,
    editing: Option<u64>,
    rename: Option<(u64, String)>,
    pub(super) settings_owner: Option<u64>,
    pub(super) pending_new: Option<(u64, bool)>,
}

#[derive(Default)]
struct RecipeWork {
    water: river_panel::Panel,
    scenery: decor_panel::RecipeWork,
    erosion: erosion_panel::Comparison,
}

impl RecipeTabs {
    pub(super) fn parked_readiness(&self, id: u64, project: &Project) -> Result<(), String> {
        if let Some(work) = self.work.get(&id) {
            work.water.readiness(project)?;
            work.scenery.readiness()?;
        }
        Ok(())
    }

    pub(super) fn active(&self) -> u64 {
        self.book.active
    }

    pub(super) fn editor(&self) -> u64 {
        self.editing.unwrap_or(self.book.active)
    }

    pub(super) fn name(&self, id: u64) -> &str {
        self.book
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .map_or("Recipe", |entry| entry.name.as_str())
    }

    pub(super) fn parked(&self, id: u64) -> Option<&Project> {
        self.book
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .and_then(|entry| entry.recipe.as_ref())
    }

    pub(super) fn multiple(&self) -> bool {
        self.book.entries.len() > 1
    }

    pub(super) fn can_add(&self) -> bool {
        self.book.entries.len() < MAX_RECIPES
            && self.book.entries.iter().all(|entry| entry.id < u64::MAX)
    }

    pub(super) fn origin(&self, id: u64) -> RecipeOrigin {
        RecipeOrigin {
            id,
            name: self.name(id).to_owned(),
        }
    }

    pub(super) fn rebuild_tabs(&self, dock: &mut DockState<Tab>) {
        let path = dock
            .find_tab(&Tab::Recipe(FIRST_RECIPE))
            .expect("the default dock contains a recipe");
        let leaf = dock
            .leaf_mut(path.node_path())
            .expect("the recipe belongs to a dock leaf");
        leaf.tabs = self
            .book
            .entries
            .iter()
            .map(|entry| Tab::Recipe(entry.id))
            .collect();
        leaf.active = egui_dock::TabIndex(
            self.book
                .entries
                .iter()
                .position(|entry| entry.id == self.book.active)
                .expect("the active recipe exists"),
        );
    }

    pub(super) fn renaming(&self) -> bool {
        self.rename.is_some()
    }

    pub(super) fn begin_rename(&mut self, id: u64) {
        self.rename = Some((id, self.name(id).to_owned()));
    }

    pub(super) fn rename_window(&mut self, ctx: &egui::Context) {
        let Some((id, mut name)) = self.rename.take() else {
            return;
        };
        let mut confirm = false;
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::new("rename-recipe").with(id)).show(ctx, |ui| {
            ui.heading("Rename recipe");
            let field = ui.add(
                egui::TextEdit::singleline(&mut name)
                    .char_limit(MAX_NAME_CHARS)
                    .desired_width(260.),
            );
            let focus = field.id.with("initial-focus");
            if !ui.data(|data| data.get_temp::<bool>(focus).unwrap_or(false)) {
                field.request_focus();
                ui.data_mut(|data| data.insert_temp(focus, true));
            }
            let valid = valid_name(name.trim());
            ui.horizontal(|ui| {
                confirm = ui.add_enabled(valid, egui::Button::new("Rename")).clicked()
                    || (valid
                        && field.lost_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter)));
                cancel = ui.button("Cancel").clicked();
            });
            focus
        });
        if confirm {
            let entry = self.book.entry_mut(id).expect("the renamed recipe exists");
            if entry.name != name.trim() {
                entry.name = name.trim().into();
                self.revision += 1;
            }
        } else if !cancel && !modal.should_close() {
            self.rename = Some((id, name));
            return;
        }
        ctx.data_mut(|data| data.remove::<bool>(modal.inner));
    }
}

pub(super) fn seed_id(label: &str, recipe: u64) -> egui::Id {
    let id = egui::Id::new(label);
    if recipe == FIRST_RECIPE {
        id
    } else {
        id.with(recipe)
    }
}

impl Studio {
    fn swap_recipe_work(&mut self, work: &mut RecipeWork) {
        work.water.share_prepared_from(&self.water);
        std::mem::swap(&mut self.water, &mut work.water);
        self.decoration_panel.swap_recipe_work(&mut work.scenery);
        std::mem::swap(&mut self.erosion_comparison, &mut work.erosion);
    }

    fn tick_recipe_work(&mut self, ctx: &egui::Context) {
        let root = self.source.bound_root(&self.project);
        let catalog = self.prefabs.catalog(&self.project);
        let bindings = self.prefabs.bindings(&self.project);
        self.water.tick(&mut self.project, root.clone(), ctx);
        self.decoration_panel.tick_recipe_work(
            &self.project,
            root.as_deref(),
            catalog,
            bindings,
            ctx,
        );
        self.decoration_panel.poll_live(&mut self.project, ctx);
    }

    pub(super) fn recipe_by_id(&self, id: u64) -> &Project {
        self.recipe_tabs
            .parked(id)
            .unwrap_or_else(|| self.document_recipe())
    }

    pub(super) fn activate_recipe(&mut self, id: u64) {
        let previous = self.recipe_tabs.active();
        if previous == id || self.drag_before.is_some() {
            return;
        }
        let Some(next) = self
            .recipe_tabs
            .book
            .entry_mut(id)
            .and_then(|entry| entry.recipe.take())
        else {
            return;
        };
        let old = self.replace_recipe_draft(next);
        self.recipe_tabs
            .book
            .entry_mut(previous)
            .expect("the old recipe exists")
            .recipe = Some(old);
        let history = self.recipe_tabs.histories.remove(&id).unwrap_or_default();
        self.recipe_tabs
            .histories
            .insert(previous, std::mem::replace(&mut self.history, history));
        let mut work = self.recipe_tabs.work.remove(&id).unwrap_or_default();
        self.swap_recipe_work(&mut work);
        self.recipe_tabs.work.insert(previous, work);
        self.recipe_tabs.book.active = id;
        self.recipe_tabs.revision += 1;
        self.skip_history = true;
    }

    fn add_named_recipe(&mut self, name: String, recipe: Project) -> Option<u64> {
        if !self.recipe_tabs.can_add() || self.drag_before.is_some() {
            return None;
        }
        let id = self
            .recipe_tabs
            .book
            .entries
            .iter()
            .map(|entry| entry.id)
            .max()?
            + 1;
        self.recipe_tabs.book.entries.push(RecipeEntry {
            id,
            name,
            recipe: Some(recipe),
        });
        self.activate_recipe(id);
        Some(id)
    }

    pub(super) fn import_named_recipe(&mut self, name: &str, recipe: Project) -> bool {
        let name = name
            .trim()
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_NAME_CHARS)
            .collect::<String>();
        let name = if name.is_empty() {
            "Imported recipe".into()
        } else {
            name
        };
        let Some(id) = self.add_named_recipe(name, recipe) else {
            return false;
        };
        self.show_tab(Tab::Recipe(id));
        true
    }

    pub(super) fn add_recipe_tab(
        &mut self,
        anchor: u64,
        duplicate: bool,
        dock: &mut DockState<Tab>,
    ) {
        let source = self.recipe_by_id(anchor);
        let (name, recipe) = if duplicate {
            let name: String = self
                .recipe_tabs
                .name(anchor)
                .chars()
                .take(MAX_NAME_CHARS - 5)
                .collect();
            (format!("{name} copy"), source.clone())
        } else {
            let mut recipe = Self::new_workbench_project();
            recipe.installation.clone_from(&source.installation);
            recipe.native_profile.clone_from(&source.native_profile);
            recipe.preview_engine.clone_from(&source.preview_engine);
            recipe.placement_engine.clone_from(&source.placement_engine);
            mapper_studio::generation_defaults::apply(&mut recipe);
            (
                format!("Recipe {}", self.recipe_tabs.book.entries.len() + 1),
                recipe,
            )
        };
        if let Some(id) = self.add_named_recipe(name, recipe) {
            let path = dock.find_tab(&Tab::Recipe(anchor));
            if let Some(path) = path
                && let Ok(leaf) = dock.leaf_mut(path.node_path())
            {
                leaf.append_tab(Tab::Recipe(id));
            } else {
                dock.push_to_focused_leaf(Tab::Recipe(id));
            }
        }
    }

    pub(super) fn with_recipe_editor(
        &mut self,
        id: u64,
        ctx: &egui::Context,
        draw: impl FnOnce(&mut Self),
    ) {
        if id == self.recipe_tabs.active() {
            let busy = self.generation.busy();
            self.with_recipe_draft(|studio| {
                if busy {
                    studio.tick_recipe_work(ctx);
                }
                draw(studio);
            });
            return;
        }
        let Some(mut parked) = self
            .recipe_tabs
            .book
            .entry_mut(id)
            .and_then(|entry| entry.recipe.take())
        else {
            return;
        };
        let mut history = self.recipe_tabs.histories.remove(&id).unwrap_or_default();
        let before = history.snapshot(&parked);
        std::mem::swap(&mut self.project, &mut parked);
        let mut work = self.recipe_tabs.work.remove(&id).unwrap_or_default();
        self.swap_recipe_work(&mut work);
        self.recipe_tabs.editing = Some(id);
        self.tick_recipe_work(ctx);
        draw(self);
        self.recipe_tabs.editing = None;
        self.swap_recipe_work(&mut work);
        self.recipe_tabs.work.insert(id, work);
        std::mem::swap(&mut self.project, &mut parked);
        if before != parked {
            self.recipe_tabs.revision += 1;
        }
        history.finish_snapshot(before, &parked, true);
        self.recipe_tabs.histories.insert(id, history);
        self.recipe_tabs
            .book
            .entry_mut(id)
            .expect("the edited recipe exists")
            .recipe = Some(parked);
    }
}
