use super::*;
impl Studio {
    pub(super) fn profiles_menu(&mut self, ui: &mut egui::Ui) {
        if ui.button("Export recipe…").clicked() {
            ui.close();
            let name = self.recipe_tabs.name(self.recipe_tabs.active()).to_owned();
            let stem: String = name.chars().map(|c| if r#"<>:"/\|?*"#.contains(c) { '_' } else { c }).collect();
            let stem = stem.trim_end_matches([' ', '.']);
            let stem = if stem.is_empty() { "Recipe" } else { stem };
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Mapper Recipe", &["rec"])
                .set_file_name(format!("{stem}.rec"))
                .save_file()
            {
                self.status = match mapper_studio::profile::Profile::new(name, self.document_recipe().clone())
                    .save(&path)
                {
                    Ok(()) => format!("Recipe exported: {}", path.display()),
                    Err(e) => format!("Recipe export failed: {e}"),
                };
            }
        }
        if ui
            .add_enabled(
                self.drag_before.is_none() && self.recipe_tabs.can_add(),
                egui::Button::new("Import recipe…"),
            )
            .clicked()
        {
            ui.close();
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Mapper Recipe", &["rec"])
                .pick_file()
            {
                match mapper_studio::profile::Profile::load(&path) {
                    Ok(profile) => {
                        if self.import_named_recipe(&profile.name, profile.recipe) {
                            self.status = format!("Imported recipe '{}'.", profile.name);
                        }
                    }
                    Err(e) => self.status = format!("Recipe import failed: {e}"),
                }
            }
        }
        if ui.add_enabled(self.drag_before.is_none(), egui::Button::new("Import RustMaps profile…")).clicked() {
            ui.close();
            if let Some(path) = rfd::FileDialog::new().add_filter("RustMaps profile", &["json"]).pick_file() {
                self.with_recipe_draft(|studio| studio.import_rustmaps_profile(&path));
            }
        }
    }

    fn import_rustmaps_profile(&mut self, path: &std::path::Path) {
        let catalog = self.prefabs.catalog(&self.project);
        let root = self.source.bound_root(&self.project);
        let result = mapper_studio::rustmaps_profile::load_with_bindings(
            path, &self.project, catalog.as_deref(),
            self.rails.ready_for_import(&self.project),
            self.water.ready_for_import(&self.project, root.as_deref()),
        );
        match result {
            Ok(import) => {
                self.project = import.recipe;
                self.status = "RustMaps profile applied. Undo restores the previous recipe.".into();
                if !import.skipped.is_empty() {
                    rfd::MessageDialog::new()
                        .set_title("RustMaps profile imported")
                        .set_description(format!("Review these settings in Mapper:\n\n{}", import.skipped.join("\n")))
                        .show();
                }
            }
            Err(error) => self.status = format!("RustMaps import failed. {error}"),
        }
    }
}
