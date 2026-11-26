//! Shared settings shell for stock monuments and standalone imports.
use super::{
    Studio,
    settings_dialog::{self, Icon},
};
use eframe::egui;
use mapper_studio::{MonumentTarget, rustedit_prefab::LibraryEntry};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
pub(super) enum Page {
    #[default]
    Overview,
    Generation,
    Appearance,
    Layouts,
    Source,
}

#[derive(Default)]
pub(super) struct State {
    pub page: Page,
    custom_recipe: u64,
    search: String,
    target: Option<MonumentTarget>,
    message: String,
    display_name: Option<String>,
}

enum Selection {
    Stock(String, String),
    Saved(LibraryEntry),
    New,
}

impl Studio {
    pub(super) fn open_stock_monument_dialog(&mut self, recipe: u64, path: String, name: String) {
        self.custom_monuments.close();
        self.monument_dialog.page = Page::Layouts;
        self.monument_dialog.target = None;
        self.monument_dialog.display_name = None;
        self.monument_dialog.message.clear();
        self.content
            .open_monument_settings(&self.project, recipe, path, name);
    }

    pub(super) fn open_custom_monument_dialog(&mut self, ctx: &egui::Context, recipe: u64) {
        self.content.close_monument_settings();
        self.custom_monuments.close();
        self.custom_monuments.open(ctx);
        self.monument_dialog.page = Page::Source;
        self.monument_dialog.custom_recipe = recipe;
        self.monument_dialog.target = None;
        self.monument_dialog.display_name = None;
        self.monument_dialog.message.clear();
    }

    pub(super) fn show_monument_dialog(&mut self, ctx: &egui::Context) {
        let recipe = self.content.monument_settings_recipe().or_else(|| {
            self.custom_monuments
                .is_open()
                .then_some(self.monument_dialog.custom_recipe)
        });
        if let Some(recipe) = recipe {
            self.with_recipe_editor(recipe, ctx, |studio| {
                studio.draw_monument_dialog(ctx, recipe)
            });
        }
    }

    fn draw_monument_dialog(&mut self, ctx: &egui::Context, recipe: u64) {
        self.custom_monuments.poll(ctx);
        self.custom_monuments.refresh_library(ctx);
        let stock = self
            .content
            .stock_monument()
            .map(|(path, name)| (path.to_owned(), name.to_owned()));
        let catalog = self.prefabs.catalog(&self.project);
        self.template_picker
            .prepare_dialog(&self.project, catalog.as_ref(), &self.monument_icons);
        let editable = (self.generation.busy()
            || (self.task.is_none() && self.placement_task.is_none()))
            && self.drag_before.is_none();
        let recipe_name = self.recipe_tabs.name(recipe).to_owned();
        let title = format!("Monument Settings · {recipe_name}");
        let last = if stock.is_some() {
            (Page::Layouts, "Layouts", Icon::Objects)
        } else {
            (Page::Source, "Source", Icon::Folder)
        };
        let mut page = self.monument_dialog.page;
        if matches!(page, Page::Source | Page::Layouts) {
            page = last.0;
        }
        let mut selection = None;
        let (page, closed) = settings_dialog::show(
            ctx,
            "monument-settings-workbench",
            &title,
            [
                (Page::Overview, "Overview", Icon::Monument),
                (Page::Generation, "Generation", Icon::Pin),
                (Page::Appearance, "Appearance", Icon::Appearance),
                last,
            ],
            page,
            true,
            |ui, page| {
                selection = self.monument_picker(ui, stock.as_ref());
                ui.separator();
                match (page, stock.as_ref()) {
                    (Page::Overview, Some((path, name))) => {
                        ui.heading(name);
                        ui.weak("Stock monument");
                        if let Some(stamp) = self.prefabs.stamp(&self.project, path) {
                            let size = stamp.texture.size_vec2();
                            let scale = (ui.available_width() / size.x).min(260. / size.y);
                            ui.add(egui::Image::new((stamp.texture.id(), size * scale)));
                        } else if let Some(icon) = self.monument_icons.thumbnail(
                            ctx,
                            Some(path),
                            &self.preferences,
                            self.settings_path.as_deref(),
                        ) {
                            ui.add(egui::Image::new((icon, egui::vec2(64., 64.))));
                        }
                        if let Some(bounds) = catalog
                            .as_ref()
                            .and_then(|catalog| catalog.entries.get(path))
                            .and_then(|asset| asset.bounds.as_ref())
                        {
                            ui.label(format!(
                                "Footprint {:.0} × {:.0} m",
                                bounds.extent[0] * 2.,
                                bounds.extent[2] * 2.
                            ));
                        }
                        if !path.is_empty() {
                            ui.label(path);
                        }
                    }
                    (Page::Overview, None) => {
                        ui.heading(self.custom_monuments.name());
                        self.custom_monuments.overview(ui);
                    }
                    (Page::Generation, Some((path, name))) => {
                        self.stock_generation(ui, path, name, editable)
                    }
                    (Page::Generation, None) => {
                        self.custom_monuments
                            .generation(ui, &mut self.project, editable)
                    }
                    (Page::Appearance, stock) => {
                        ui.heading("Appearance");
                        let mut changed = false;
                        if let Some((path, name)) = stock {
                            changed |= self.monument_display_controls(ui, path, name);
                            changed |= self.monument_icons.monument_appearance(
                                ui,
                                path,
                                &mut self.preferences,
                                self.settings_path.as_deref(),
                            );
                        } else {
                            ui.label(self.custom_monuments.name());
                            ui.weak("Edit the custom monument’s name in Source.");
                        }
                        ui.separator();
                        ui.label("Viewport markers");
                        ui.weak("Applies to all monuments in the viewport.");
                        changed |= super::map_label_panel::controls(
                            ui,
                            &mut self.preferences.monument_markers,
                        );
                        if changed {
                            self.save_preferences();
                        }
                    }
                    (Page::Layouts, Some(_)) => {
                        let identities = self
                            .source
                            .bound_root_ref(&self.project)
                            .and_then(|root| self.prefabs.prepared_identities(&self.project, root));
                        ui.heading("Layouts");
                        self.content
                            .monument_layout(ui, &mut self.project, identities, editable);
                    }
                    (Page::Source, None) => {
                        ui.heading("Source");
                        self.custom_monuments
                            .source(ui, &mut self.project, editable);
                    }
                    _ => {}
                }
            },
        );
        self.monument_dialog.page = page;
        if closed {
            self.content.close_monument_settings();
            self.custom_monuments.close();
            self.monument_dialog = State::default();
            return;
        }
        match selection {
            Some(Selection::Stock(path, name)) => {
                self.open_stock_monument_dialog(recipe, path, name)
            }
            Some(Selection::New) => self.open_custom_monument_dialog(ctx, recipe),
            Some(Selection::Saved(entry)) => {
                self.content.close_monument_settings();
                self.custom_monuments.select(ctx, entry);
                self.monument_dialog.custom_recipe = recipe;
                self.monument_dialog.page = Page::Source;
            }
            None => {}
        }
    }

    fn monument_picker(
        &mut self,
        ui: &mut egui::Ui,
        stock: Option<&(String, String)>,
    ) -> Option<Selection> {
        let name = stock.map_or_else(|| self.custom_monuments.name(), |(_, name)| name);
        let mut selected = None;
        let response = egui::ComboBox::from_id_salt("settings-monument")
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .selected_text(name)
            .truncate()
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.monument_dialog.search)
                        .hint_text("Search monuments"),
                );
                if ui.button("New custom monument…").clicked() {
                    selected = Some(Selection::New);
                    ui.close();
                }
                let search = self.monument_dialog.search.to_lowercase();
                let mut stock_choices = BTreeMap::new();
                for (path, name, _) in self.template_picker.dialog_entries() {
                    stock_choices.insert(path, name.to_owned());
                }
                for target in &self.project.targets {
                    if let Some(path) = target.prefab_path.as_deref() {
                        stock_choices
                            .entry(path)
                            .or_insert_with(|| self.monument_icons.name(Some(path), &target.name));
                    }
                }
                let mut stock_choices: Vec<_> = stock_choices
                    .into_iter()
                    .filter(|(path, name)| {
                        path.contains(&search) || name.to_lowercase().contains(&search)
                    })
                    .collect();
                stock_choices
                    .sort_unstable_by(|left, right| left.1.cmp(&right.1).then(left.0.cmp(right.0)));
                egui::ScrollArea::vertical()
                    .max_height(300.)
                    .show(ui, |ui| {
                        for entry in self
                            .custom_monuments
                            .entries()
                            .iter()
                            .filter(|entry| entry.name.to_lowercase().contains(&search))
                        {
                            if ui
                                .button(&entry.name)
                                .on_hover_text(format!(
                                    "Custom monument · {}",
                                    &entry.version.as_str()[..8]
                                ))
                                .clicked()
                            {
                                selected = Some(Selection::Saved(entry.clone()));
                                ui.close();
                            }
                        }
                        for (path, name) in stock_choices {
                            if ui
                                .selectable_label(
                                    stock.is_some_and(|(current, _)| current == path),
                                    &name,
                                )
                                .on_hover_text(path)
                                .clicked()
                            {
                                selected = Some(Selection::Stock(path.to_owned(), name));
                                ui.close();
                            }
                        }
                    });
            })
            .response;
        response.on_hover_text("Switch monument");
        if selected.is_some() {
            self.monument_dialog.search.clear();
        }
        selected
    }

    fn monument_display_controls(&mut self, ui: &mut egui::Ui, path: &str, name: &str) -> bool {
        let mut changed = false;
        let draft = self.monument_dialog.display_name.get_or_insert_with(|| {
            self.preferences
                .monument_display_names
                .get(path)
                .cloned()
                .unwrap_or_default()
        });
        ui.horizontal(|ui| {
            let label = ui.label("Map label");
            let input = ui
                .add(
                    egui::TextEdit::singleline(draft)
                        .hint_text(name)
                        .char_limit(128),
                )
                .labelled_by(label.id);
            let value = draft.trim();
            let valid = value.len() <= 128
                && !value.chars().any(char::is_control)
                && (value.is_empty()
                    || self.preferences.monument_display_names.contains_key(path)
                    || self.preferences.monument_display_names.len() < 128);
            let current = self
                .preferences
                .monument_display_names
                .get(path)
                .map_or("", String::as_str);
            let submitted =
                input.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            let save = ui
                .add_enabled(valid && value != current, egui::Button::new("Save label"))
                .on_hover_text("Leave blank to restore the installation label.");
            if valid && value != current && (submitted || save.clicked()) {
                if value.is_empty() {
                    self.preferences.monument_display_names.remove(path);
                } else {
                    self.preferences
                        .monument_display_names
                        .insert(path.to_owned(), value.to_owned());
                }
                changed = true;
            }
        });
        let mut visible = !self.preferences.hidden_monument_markers.contains(path);
        let editable = !visible || self.preferences.hidden_monument_markers.len() < 128;
        if ui
            .add_enabled(
                editable,
                egui::Checkbox::new(&mut visible, "Show marker in viewport"),
            )
            .changed()
        {
            if visible {
                self.preferences.hidden_monument_markers.remove(path);
            } else {
                self.preferences.hidden_monument_markers.insert(path.into());
            }
            changed = true;
        }
        changed
    }

    fn stock_generation(&mut self, ui: &mut egui::Ui, path: &str, name: &str, editable: bool) {
        ui.heading("Generation");
        let mut targets = self
            .project
            .targets
            .iter()
            .filter(|target| target.prefab_path.as_deref() == Some(path));
        let first = targets.next();
        let selected_id = self
            .monument_dialog
            .target
            .as_ref()
            .map(|target| target.instance_id);
        let target = first
            .into_iter()
            .chain(targets)
            .find(|target| Some(target.instance_id) == selected_id)
            .or(first);
        if let Some(blocker) = self.project.monument_blocker(Some(path)) {
            ui.weak(blocker);
            return;
        }
        let Some(target) = target else {
            ui.weak("Placement follows the installation’s monument rules.");
            let placeable = self
                .template_picker
                .dialog_entries()
                .any(|(candidate, _, placeable)| candidate == path && placeable);
            if ui
                .add_enabled(
                    editable && placeable,
                    egui::Button::new("Add placement preference"),
                )
                .clicked()
            {
                self.monument_dialog.message = self
                    .project
                    .add_target(name.into(), path.into())
                    .err()
                    .unwrap_or_default();
            }
            if !self.monument_dialog.message.is_empty() {
                ui.label(&self.monument_dialog.message);
            }
            return;
        };
        let mut instance = target.instance_id;
        egui::ComboBox::from_id_salt("monument-preference-instance")
            .selected_text(format!("Placement #{instance}"))
            .show_ui(ui, |ui| {
                for candidate in self
                    .project
                    .targets
                    .iter()
                    .filter(|item| item.prefab_path.as_deref() == Some(path))
                {
                    ui.selectable_value(
                        &mut instance,
                        candidate.instance_id,
                        format!("Placement #{}", candidate.instance_id),
                    );
                }
            });
        let Some(target) = self
            .project
            .targets
            .iter_mut()
            .find(|target| target.instance_id == instance)
        else {
            return;
        };
        let draft = self
            .monument_dialog
            .target
            .get_or_insert_with(|| target.clone());
        if draft.instance_id != instance || draft.prefab_path != target.prefab_path {
            *draft = target.clone();
        }
        let size = self.project.size as f32;
        let half = size * 0.5;
        ui.add_enabled_ui(editable, |ui| {
            ui.horizontal(|ui| {
                ui.label("Presence");
                if let Some(choice) = super::outliner::presence_buttons(ui, draft.presence, None) {
                    draft.presence = choice;
                }
            });
            egui::Grid::new("monument-generation-preference")
                .num_columns(2)
                .show(ui, |ui| {
                    let label = ui.label("Preferred X");
                    ui.add(
                        egui::DragValue::new(&mut draft.x)
                            .range(-half..=half)
                            .suffix(" m"),
                    )
                    .labelled_by(label.id);
                    ui.end_row();
                    let label = ui.label("Preferred Z");
                    ui.add(
                        egui::DragValue::new(&mut draft.z)
                            .range(-half..=half)
                            .suffix(" m"),
                    )
                    .labelled_by(label.id);
                    ui.end_row();
                    let label = ui.label("Radius");
                    ui.add(
                        egui::DragValue::new(&mut draft.radius)
                            .range(1. ..=size)
                            .suffix(" m"),
                    )
                    .labelled_by(label.id);
                    ui.end_row();
                    let label = ui.label("Weight");
                    ui.add(
                        egui::DragValue::new(&mut draft.strength)
                            .range(0.1..=10.)
                            .speed(0.05),
                    )
                    .labelled_by(label.id);
                    ui.end_row();
                    let mut limited = draft.max_displacement_m.is_some();
                    let response = ui.checkbox(&mut limited, "Max move");
                    if response.changed() {
                        draft.max_displacement_m = limited.then_some(
                            mapper_studio::DEFAULT_MONUMENT_MOVE_LIMIT_M.min(size as u32),
                        );
                    }
                    if let Some(limit) = &mut draft.max_displacement_m {
                        ui.add(
                            egui::DragValue::new(limit)
                                .range(1..=size as u32)
                                .suffix(" m"),
                        )
                        .labelled_by(response.id);
                    }
                    ui.end_row();
                    let mut fixed = draft.yaw.is_some();
                    let response = ui.checkbox(&mut fixed, "Fixed rotation");
                    if response.changed() {
                        draft.yaw = fixed.then_some(0.);
                    }
                    if let Some(yaw) = &mut draft.yaw {
                        ui.add(
                            egui::DragValue::new(yaw)
                                .range(0. ..=360.0_f32.next_down())
                                .suffix("°"),
                        )
                        .labelled_by(response.id);
                    }
                    ui.end_row();
                    ui.checkbox(&mut draft.exact, "Require exact position");
                    ui.end_row();
                });
            if ui
                .add_enabled(draft != target, egui::Button::new("Apply to recipe"))
                .clicked()
            {
                if draft.yaw.is_some()
                    && !mapper_studio::placement_preview::is_earthwork(
                        &self.project.placement_engine,
                    )
                {
                    self.project.placement_engine =
                        mapper_studio::placement_preview::ROTATION_VERSION.into();
                }
                *target = draft.clone();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monument_settings_entry_points_share_tabs_and_keep_the_recipe_binding() {
        let ctx = egui::Context::default();
        super::super::workbench_style::install(&ctx);
        let mut studio = crate::tests::fixture(&ctx);
        let recipe = studio.recipe_tabs.active();
        let (path, name) = studio
            .project
            .targets
            .iter()
            .find_map(|target| Some((target.prefab_path.clone()?, target.name.clone())))
            .unwrap();
        let render = |studio: &mut Studio| {
            let input = || egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1100., 760.),
                )),
                ..Default::default()
            };
            let mut first = ctx.run_ui(input(), |_| studio.draw_monument_dialog(&ctx, recipe));
            first.textures_delta.clear();
            let mut frame = ctx.run_ui(input(), |_| studio.draw_monument_dialog(&ctx, recipe));
            frame.textures_delta.clear();
            frame
        };
        let has = |frame: &egui::FullOutput, label: &str| {
            frame.shapes.iter().any(|shape| {
            matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text == label)
        })
        };

        studio.open_stock_monument_dialog(recipe, path, name);
        let layout = render(&mut studio);
        assert_eq!(studio.content.monument_settings_recipe(), Some(recipe));
        assert!(has(&layout, "Layouts"));
        assert!(has(&layout, "Import layout…"));
        assert!(has(&layout, "Appearance"));

        studio.open_custom_monument_dialog(&ctx, recipe);
        let custom = render(&mut studio);
        assert!(studio.content.monument_settings_recipe().is_none());
        assert_eq!(studio.monument_dialog.custom_recipe, recipe);
        assert!(studio.custom_monuments.is_open());
        assert!(has(&custom, "Source"));
        assert!(has(&custom, "Choose prefab…"));
        assert!(!has(&custom, "Import layout…"));
    }
}
