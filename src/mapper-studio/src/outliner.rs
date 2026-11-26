use super::*;
use egui::AtomExt as _;
use mapper_studio::prefab_assets::Catalog;
use std::collections::HashMap;
use std::sync::{Arc, Weak};

#[derive(Default)]
pub(super) struct TemplatePicker {
    binding: Option<TemplateBinding>,
    entries: Vec<TemplateEntry>,
}

struct TemplateBinding {
    native: bool,
    catalogue: Option<Weak<Catalog>>,
    names: Option<Weak<Catalog>>,
}

struct TemplateEntry {
    name: String,
    path: String,
    label: String,
    placeable: bool,
}

fn same_catalogue(previous: &Option<Weak<Catalog>>, current: Option<&Arc<Catalog>>) -> bool {
    match (previous, current) {
        (Some(previous), Some(current)) => std::ptr::eq(previous.as_ptr(), Arc::as_ptr(current)),
        (None, None) => true,
        _ => false,
    }
}

impl TemplatePicker {
    fn refresh(
        &mut self,
        project: &Project,
        catalogue: Option<&Arc<Catalog>>,
        icons: &monument_icon_panel::Panel,
    ) {
        let native = mapper_studio::native_preview::is_native(&project.preview_engine);
        let catalogue = catalogue.filter(|_| native);
        let names = icons.name_source();
        if self.binding.as_ref().is_some_and(|binding| {
            binding.native == native
                && same_catalogue(&binding.catalogue, catalogue)
                && same_catalogue(&binding.names, names)
        }) {
            return;
        }
        let mut entries: Vec<_> =
            mapper_studio::placement_preview::project_templates(project, catalogue.map(Arc::as_ref))
                .into_iter()
                .map(|template| TemplateEntry {
                    label: icons.name(Some(&template.path), &template.name),
                    name: template.name,
                    path: template.path,
                    placeable: template.entry.land,
                })
                .collect();
        if let Some(catalogue) = catalogue {
            for (path, asset) in &catalogue.entries {
                if asset.bounds.is_none() && (path.contains("/monument/") || path.contains("/unique_environment/")) {
                    let name = path.rsplit('/').next().unwrap_or(path)
                        .trim_end_matches(".prefab").replace('_', " ");
                    entries.push(TemplateEntry {
                        label: icons.name(Some(path), &name),
                        name,
                        path: path.clone(),
                        placeable: false,
                    });
                }
            }
        }
        entries.sort_by_cached_key(|entry| (entry.label.to_lowercase(), entry.path.clone()));
        let variants = qualifiers(entries.iter().map(|item| (item.label.as_str(), Some(item.path.as_str()))));
        for (item, variant) in entries.iter_mut().zip(variants) {
            if let Some(variant) = variant {
                item.label = format!("{variant} · {}", item.label);
            }
        }
        self.entries = entries;
        self.binding = Some(TemplateBinding {
            native,
            catalogue: catalogue.map(Arc::downgrade),
            names: names.map(Arc::downgrade),
        });
    }
}

struct Row {
    target_index: Option<usize>,
    generated_index: Option<usize>,
    name: String,
    instance_id: u64,
    presence: Presence,
    identifier: String,
    prefab_path: Option<String>,
}

fn presence_buttons(ui: &mut egui::Ui, presence: Presence, blocker: Option<&str>) -> Option<Presence> {
    let mut changed = None;
    for (choice, help) in [
        (Presence::Required, "Require this monument"),
        (Presence::Excluded, "Exclude this monument"),
        (Presence::Allowed, "Let generation decide"),
    ] {
        let button = egui::Button::new(if choice == Presence::Allowed { "?" } else { "" })
            .selected(choice == presence);
        let response = ui.add_enabled_ui(blocker.is_none(), |ui| ui.add_sized([20., 22.], button))
            .inner.on_hover_text(help).on_disabled_hover_text(blocker.unwrap_or_default());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, blocker.is_none(), help));
        let center = response.rect.center();
        let stroke = egui::Stroke::new(1.4, ui.style().interact(&response).text_color());
        match choice {
            Presence::Required => {
                ui.painter().add(egui::Shape::line(
                    vec![center + Vec2::new(-4., 0.), center + Vec2::new(-1., 3.), center + Vec2::new(5., -3.)],
                    stroke,
                ));
            }
            Presence::Excluded => {
                let d = Vec2::splat(3.5);
                ui.painter().line_segment([center - d, center + d], stroke);
                let d = Vec2::new(-3.5, 3.5);
                ui.painter().line_segment([center - d, center + d], stroke);
            }
            Presence::Allowed => {}
        };
        if response.clicked() && presence != choice {
            changed = Some(choice);
        }
    }
    changed
}

fn prefab_variant(path: &str) -> Option<&str> {
    let stem = path.rsplit('/').next()?.strip_suffix(".prefab")?;
    let (_, suffix) = stem.rsplit_once('_')?;
    ((suffix.len() == 1 && suffix.as_bytes()[0].is_ascii_alphabetic())
        || ((1..=2).contains(&suffix.len()) && suffix.bytes().all(|byte| byte.is_ascii_digit())))
    .then_some(suffix)
}

fn qualifiers<'a>(
    names: impl Iterator<Item = (&'a str, Option<&'a str>)> + Clone,
) -> Vec<Option<String>> {
    let mut counts = HashMap::<_, usize>::new();
    let mut variants = HashMap::<_, usize>::new();
    for (name, path) in names.clone() {
        *counts.entry(name).or_default() += 1;
        if let Some(variant) = path.and_then(prefab_variant) {
            *variants.entry((name, variant)).or_default() += 1;
        }
    }
    let mut seen = HashMap::<_, usize>::new();
    names
        .map(|(name, path)| {
            if counts[name] == 1 {
                return None;
            }
            let occurrence = seen.entry(name).or_default();
            *occurrence += 1;
            Some(match path.and_then(prefab_variant) {
                Some(variant) if variants[&(name, variant)] == 1 => variant.to_ascii_uppercase(),
                _ => format!("#{occurrence}"),
            })
        })
        .collect()
}

fn matches_query(row: &Row, qualifier: Option<&str>, query: &str) -> bool {
    query.is_empty()
        || row.name.to_lowercase().contains(query)
        || row.identifier.to_lowercase().contains(query)
        || row
            .prefab_path
            .as_ref()
            .is_some_and(|path| path.to_lowercase().contains(query))
        || qualifier.is_some_and(|qualifier| {
            format!("{} {qualifier}", row.name)
                .to_lowercase()
                .contains(query)
        })
}

impl Studio {
    pub(super) fn choose_monument_replacement(&mut self, ctx: &egui::Context, id: u64, index: usize) {
        let view_id = egui::Id::new(("monument-list-view", Some(id)));
        ctx.data_mut(|data| data.insert_temp(view_id.with("replacement"), index));
        self.show_tab(Tab::Outliner);
    }

    pub(super) fn outliner(&mut self, ui: &mut egui::Ui, generated_id: Option<u64>) {
        let view_id = egui::Id::new(("monument-list-view", generated_id));
        let replacement_id = view_id.with("replacement");
        let replacement = ui.ctx().data(|data| data.get_temp::<usize>(replacement_id));
        let mut settings_request = None;
        let selected_template = if generated_id.is_none() {
            self.selected.and_then(|index| self.project.targets.get(index)).and_then(|target| {
                let path = target.prefab_path.as_ref()?;
                Some((path.clone(), self.monument_icons.name(Some(path), &target.name)))
            })
        } else {
            None
        };
        let search = ui.horizontal(|ui| {
            let import_width = ui.painter().layout_no_wrap(
                "Import…".into(), egui::TextStyle::Button.resolve(ui.style()), ui.visuals().text_color(),
            ).size().x + 2. * ui.spacing().button_padding.x;
            let gear_width = if generated_id.is_none() {
                workbench_style::SETTINGS_BUTTON_WIDTH + ui.spacing().item_spacing.x
            } else { 0. };
            let search_width = (ui.available_width() - import_width - gear_width
                - ui.spacing().item_spacing.x - 8.).max(12.);
            let search = ui.add(
                egui::TextEdit::singleline(&mut self.monument_filter)
                    .hint_text("Search monuments")
                    .desired_width(search_width),
            );
            if ui.button("Import…").on_hover_text("Import a standalone custom monument").clicked() {
                self.custom_monuments.open(ui.ctx());
            }
            if generated_id.is_none()
                && ui.add_enabled_ui(selected_template.is_some(), |ui| {
                    workbench_style::settings_button(ui, "Monument settings")
                        .on_disabled_hover_text("Select a monument with a prefab to edit its settings.")
                }).inner.clicked()
            {
                settings_request = selected_template;
            }
            search
        }).inner;
        if generated_id.is_none() {
            search.on_hover_text(format!(
                "{} candidates. Placement quotas and terrain checks determine which are used.",
                self.project.targets.len()
            ));
        }
        let query = self.monument_filter.trim().to_lowercase();
        if !self.document_recipe().custom_monuments.is_empty() {
            self.with_recipe_draft(|studio| studio.custom_placement_rows(ui, &query));
        }
        let mut rows = Vec::new();
        if generated_id.is_none() {
            let catalogue = self.prefabs.catalog(&self.project);
            self.template_picker.refresh(&self.project, catalogue.as_ref(), &self.monument_icons);
            rows.extend(
                self.project
                    .targets
                    .iter()
                    .enumerate()
                    .map(|(index, target)| Row {
                        target_index: Some(index),
                        generated_index: None,
                        name: self
                            .monument_icons
                            .name(target.prefab_path.as_deref(), &target.name),
                        instance_id: target.instance_id,
                        presence: self.project.monument_presence(target),
                        identifier: target.name.clone(),
                        prefab_path: target.prefab_path.clone(),
                    }),
            );
            for (index, template) in self.template_picker.entries.iter().enumerate() {
                if !rows.iter().any(|row| row.prefab_path.as_deref() == Some(template.path.as_str()))
                {
                    rows.push(Row {
                        target_index: None,
                        generated_index: None,
                        name: self.monument_icons.name(Some(&template.path), &template.name),
                        instance_id: u64::MAX - index as u64,
                        presence: if self.project.monument_blocker(Some(&template.path)).is_some() {
                            Presence::Excluded
                        } else {
                            Presence::Allowed
                        },
                        identifier: template.name.clone(),
                        prefab_path: Some(template.path.clone()),
                    });
                }
            }
        }
        let monuments = match generated_id {
            Some(id) => self.generation.result_monuments(id).unwrap_or_default(),
            None if self.generation.busy() => &[],
            None => self
                .retained_composition()
                .map(|view| view.generated_monuments.as_ref())
                .unwrap_or_default(),
        };
        for (index, monument) in monuments.iter().enumerate() {
            rows.push(Row {
                target_index: None,
                generated_index: generated_id.map(|_| index),
                name: self
                    .monument_icons
                    .name(monument.prefab.as_deref(), &monument.name),
                instance_id: monument.id,
                presence: generated_id.and_then(|id| self.generation.editable_monument_presence(id, index))
                    .unwrap_or(Presence::Allowed),
                identifier: monument.name.clone(),
                prefab_path: monument.prefab.clone(),
            });
        }
        let variants = qualifiers(
            rows.iter()
                .map(|row| (row.name.as_str(), row.prefab_path.as_deref())),
        );
        let rows: Vec<_> = rows
            .iter()
            .zip(variants)
            .filter(|(row, variant)| matches_query(row, variant.as_deref(), &query))
            .collect();
        if rows.is_empty() && generated_id.is_none() && !query.is_empty() {
            ui.weak(if query.is_empty() {
                "No monuments"
            } else {
                "No matching monuments"
            });
        }
        let mut selected = None;
        let mut selected_instance = None;
        let current_instance = generated_id.and_then(|id| self.generation.selected_monument(id));
        let mut policy_change = None;
        let row_height = if generated_id.is_some() {
            (ui.available_height() * 0.32).max(24.)
        } else {
            ui.available_height().max(32.)
        };
        if generated_id.is_some() && !rows.is_empty() {
            ui.add_space(5.);
            ui.separator();
            ui.strong("Placed").on_hover_text("Monument instances on the current map.");
        }
        egui::ScrollArea::vertical()
            .id_salt(("monument-outliner", generated_id))
            .max_height(row_height)
            .show_rows(ui, 22., rows.len(), |ui, range| {
                for (row, variant) in &rows[range] {
                    let label = &row.name;
                    let qualified_name = variant
                        .as_ref()
                        .map_or_else(|| label.clone(), |variant| format!("{label} {variant}"));
                    let identifier = row.prefab_path.as_deref().unwrap_or(&row.identifier);
                    let tooltip = format!(
                        "{qualified_name}\n{identifier}\nInstance {}",
                        row.instance_id
                    );
                    let selected_row = if generated_id.is_some() {
                        current_instance.is_some() && current_instance == row.generated_index
                    } else {
                        row.target_index.is_some() && self.selected == row.target_index
                    };
                    let text = egui::RichText::new(label);
                    let text = if selected_row {
                        text.color(Color32::WHITE)
                    } else if row.presence == Presence::Excluded {
                        text.weak().strikethrough()
                    } else {
                        text
                    };
                    let icon = self.monument_icons.thumbnail(
                        ui.ctx(),
                        row.prefab_path.as_deref(),
                        &self.preferences,
                        self.settings_path.as_deref(),
                    );
                    let marker = icon.map_or_else(
                        || {
                            egui::Atom::custom(
                                egui::Id::new(("empty-monument-icon", row.instance_id)),
                                Vec2::splat(16.),
                            )
                        },
                        |icon| egui::Atom::from(egui::Image::new((icon, Vec2::splat(16.)))),
                    );
                    let mut atoms =
                        egui::Atoms::new((marker, text.atom_shrink(true), egui::Atom::grow()));
                    if let Some(variant) = variant {
                        atoms.push_right(egui::RichText::new(variant).small().weak());
                    }
                    let editable = row.target_index.is_some();
                    let blocker = generated_id.is_none().then(|| {
                        self.project.monument_blocker(row.prefab_path.as_deref())
                    }).flatten();
                    let generated = row.generated_index.zip(generated_id)
                        .filter(|&(index, id)| self.generation.editable_monument_presence(id, index).is_some());
                    let edits_visible = editable || generated.is_some();
                    let can_select = editable || row.generated_index.is_some();
                    let mut button = egui::Button::new(atoms).gap(4.).sense(if can_select {
                        Sense::click()
                    } else {
                        Sense::hover()
                    });
                    let response = ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.;
                        let name_width = (ui.available_width() - if edits_visible || blocker.is_some() { 66. } else { 0. }).max(1.);
                        if selected_row {
                            button = button.fill(workbench_style::SELECTED_MONUMENT);
                        }
                        let response = ui
                        .add_sized(
                            [name_width, 22.],
                            button
                                .frame(selected_row)
                                .truncate(),
                        )
                        .on_hover_text(blocker.unwrap_or(&tooltip));
                        if (edits_visible || blocker.is_some()) && let Some(presence) = presence_buttons(ui, row.presence, blocker) {
                            policy_change = Some((row.target_index, generated, presence));
                        }
                        response
                    }).inner;
                    if let Some((index, _)) = generated {
                        response.context_menu(|ui| {
                            if ui.button("Replace with…").clicked() {
                                ui.ctx().data_mut(|data| {
                                    data.insert_temp(replacement_id, index);
                                });
                                ui.close();
                            }
                        });
                    } else if generated_id.is_none() && let Some(path) = &row.prefab_path {
                        response.context_menu(|ui| {
                            if ui.button("Monument settings…").clicked() {
                                settings_request = Some((path.clone(), row.name.clone()));
                                ui.close();
                            }
                        });
                    }
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            if can_select {
                                egui::WidgetType::Button
                            } else {
                                egui::WidgetType::Label
                            },
                            true,
                            &qualified_name,
                        )
                    });
                    if response.clicked() {
                        selected = row.target_index;
                        selected_instance = row.generated_index;
                    }
                }
            });
        if let Some((target, generated, presence)) = policy_change {
            if let Some(index) = target {
                self.project.targets[index].presence = presence;
            } else if let Some((index, id)) = generated
                && let Err(error) = self.generation.set_monument_presence(id, index, presence)
            {
                self.status = error;
            }
        }
        if let Some(index) = selected {
            self.selected = Some(index);
            self.show_tab(Tab::Inspector);
        }
        if let Some(id) = generated_id {
            if let Some(index) = selected_instance {
                self.generation.select_monument(id, index);
                self.show_tab(Tab::Inspector);
            }
            ui.add_space(5.);
            ui.separator();
            ui.strong("Catalog").on_hover_text("Drag a prefab onto the map to add or replace a monument.");
            if self.monument_catalog(ui, id, &query, replacement) {
                ui.ctx().data_mut(|data| data.remove::<usize>(replacement_id));
            }
            return;
        }
        if let Some((path, name)) = settings_request {
            self.content.open_monument_settings(&self.project, self.recipe_tabs.active(), path, name);
        }
    }

    fn custom_placement_rows(&mut self, ui: &mut egui::Ui, query: &str) {
        if self.project.custom_monuments.is_empty() {
            return;
        }
        let blocker = (!self.project.performance.monuments).then_some("Enable Monuments in Recipe.");
        egui::CollapsingHeader::new("Custom monuments")
            .id_salt("custom-monument-placements")
            .default_open(true)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("custom-placement-scroll")
                    .max_height(110.)
                    .show(ui, |ui| {
                        for (index, item) in self.project.custom_monuments.iter_mut().enumerate() {
                            let name = if item.name.is_empty() {
                                let version = item.version.get(..8).unwrap_or(&item.version);
                                format!("Custom monument · {version}")
                            } else {
                                item.name.clone()
                            };
                            if !query.is_empty() && !name.to_lowercase().contains(query) {
                                continue;
                            }
                            ui.push_id(index, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 2.;
                                    let name_width = (ui.available_width() - 66.).max(1.);
                                    let [x, y, z] = item.position;
                                    let presence = if blocker.is_some() {
                                        Presence::Excluded
                                    } else {
                                        item.presence
                                    };
                                    let mut label = egui::RichText::new(&name);
                                    if presence == Presence::Excluded {
                                        label = label.weak().strikethrough();
                                    }
                                    ui.add_sized([name_width, 22.], egui::Label::new(label).truncate())
                                        .on_hover_text(format!(
                                            "{name}\nX {x:.1} m · Y {y:.1} m · Z {z:.1} m · {:.1}°",
                                            item.yaw
                                        ));
                                    if let Some(choice) = presence_buttons(ui, presence, blocker) {
                                        item.presence = choice;
                                    }
                                });
                            });
                        }
                    });
            });
    }

    fn monument_catalog(&mut self, ui: &mut egui::Ui, id: u64, query: &str, replacement: Option<usize>) -> bool {
        let result = self.generation.result_recipe(id);
        let recipe = result.unwrap_or(&self.project);
        let metadata = self.prefabs.catalog(recipe);
        self.template_picker.refresh(recipe, metadata.as_ref(), &self.monument_icons);
        let entries: Vec<_> = self.template_picker.entries.iter().filter(|item| {
            query.is_empty() || item.label.to_lowercase().contains(query) || item.path.contains(query)
        }).collect();
        if replacement.is_some() && ui.button("Cancel replacement").clicked() {
            return true;
        }
        if entries.is_empty() {
            ui.weak(if metadata.is_none() { "Prepare this map’s Rust installation" } else { "No matching monuments" });
        }
        let mut chosen = None;
        egui::ScrollArea::vertical().id_salt(("monument-catalog", id)).show_rows(ui, 22., entries.len(), |ui, range| {
            for item in &entries[range] {
                let blocker = super::generation_ui::MonumentTemplate::blocker(recipe, &item.path)
                    .or_else(|| (!item.placeable).then_some("This monument is managed by its generation stage"))
                    .or_else(|| result.is_none().then_some("Finish generating the map to add monuments"));
                let recipe_blocker = recipe.monument_blocker(Some(&item.path));
                let row = ui.horizontal(|ui| {
                    let width = ui.available_width() - if recipe_blocker.is_some() { 66. } else { 0. };
                    let icon = self.monument_icons.thumbnail(
                        ui.ctx(), Some(&item.path), &self.preferences, self.settings_path.as_deref(),
                    );
                    let marker = icon.map_or_else(
                        || egui::Atom::custom(ui.id().with(("catalog-icon", &item.path)), Vec2::splat(16.)),
                        |icon| egui::Atom::from(egui::Image::new((icon, Vec2::splat(16.)))),
                    );
                    let atoms = egui::Atoms::new((
                        marker, egui::RichText::new(&item.label).atom_shrink(true), egui::Atom::grow(),
                    ));
                    let row = ui.add_enabled_ui(blocker.is_none(), |ui| {
                    ui.add_sized(
                        [width.max(1.), 22.],
                        egui::Button::new(atoms).gap(4.).frame(false).truncate().sense(Sense::click_and_drag()),
                    )
                    }).inner.on_hover_ui(|ui| {
                        ui.label(if replacement.is_some() {
                            "Select to replace the monument."
                        } else {
                            "Drag onto the map to place."
                        });
                        ui.weak(&item.path);
                    }).on_disabled_hover_text(blocker.unwrap_or_default());
                    if row.dragged() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                    } else if row.enabled() && row.hovered() && replacement.is_none() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                    }
                    if recipe_blocker.is_some() {
                        ui.spacing_mut().item_spacing.x = 2.;
                        presence_buttons(ui, Presence::Excluded, recipe_blocker);
                    }
                    row
                }).inner;
                if row.drag_started() || (replacement.is_some() && row.clicked()) {
                    let template = super::generation_ui::MonumentTemplate {
                        map: id, name: item.label.clone(), prefab: item.path.clone(),
                    };
                    if row.drag_started() {
                        row.dnd_set_drag_payload(template);
                    } else {
                        chosen = Some(template);
                    }
                }
            }
        });
        if let Some((index, template)) = replacement.zip(chosen) {
            match self.generation.replace_monument(id, index, &template) {
                Ok(()) => return true,
                Err(error) => self.status = error,
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a configured Rust installation and a prepared prefab catalog"]
    fn capture_recipe_and_monument_catalog() {
        mod snapshot { include!("../examples/support/egui_snapshot.rs"); }
        let directory = PathBuf::from(std::env::var_os("MAPPER_DESIGN_CAPTURE_DIR").unwrap());
        let root = PathBuf::from(std::env::var_os("MAPPER_DESIGN_RUST_ROOT").unwrap());
        let cache = PathBuf::from(std::env::var_os("MAPPER_DESIGN_CATALOG").unwrap());
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        workbench_style::install(&ctx);
        ctx.all_styles_mut(|style| style.animation_time = 0.);
        let mut studio = crate::tests::fixture(&ctx);
        if let Some(recipe) = std::env::var_os("MAPPER_DESIGN_RECIPE") {
            studio.project = mapper_studio::profile::Profile::load_snapshot(&PathBuf::from(recipe))
                .unwrap().recipe;
        }
        let installation = mapper_studio::installation::inspect(&root, || false).unwrap();
        let catalog = Arc::new(mapper_studio::prefab_assets::load(&root, &cache, &installation, || false).unwrap());
        studio.project.installation = Some(installation);
        studio.project.preview_engine = mapper_studio::native_preview::VERSION.into();
        studio.prefabs.seed_catalog(catalog.clone());
        studio.monument_icons.seed_names(catalog.clone());
        if let Some(cache) = std::env::var_os("MAPPER_DESIGN_ROAD_CACHE") {
            for entry in std::fs::read_dir(cache).unwrap().flatten() {
                let path = entry.path().join("infrastructure.json");
                if let Ok(source) = mapper_studio::infrastructure::Source::load(&path, &catalog, || false) {
                    studio.roads.seed_source(source);
                    break;
                }
            }
        }
        studio.template_picker.refresh(&studio.project, Some(&catalog), &studio.monument_icons);
        let mut roster = String::from("# Mapper monument catalog\n\n");
        for entry in &studio.template_picker.entries {
            roster.push_str(&format!("- **{}** — `{}`\n", entry.label, entry.path));
        }
        std::fs::write(directory.join("Mapper Monument Catalog.md"), roster).unwrap();
        let mut textures = snapshot::Textures::default();
        let mut render = |studio: &mut Studio, size: [usize; 2], events, recipe| {
            let mut output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(size[0] as f32, size[1] as f32))),
                focused: true, events, ..Default::default()
            }, |ui| {
                if recipe {
                    ui.heading("Recipe");
                    studio.recipe(ui);
                } else {
                    ui.heading("Monuments");
                    studio.outliner(ui, None);
                }
            });
            textures.update(&output.textures_delta);
            output.textures_delta.clear();
            output
        };
        let recipe_size = [460, 2250];
        render(&mut studio, recipe_size, vec![], true);
        for section in ["Stage seeds", "Terrain", "Custom tiers", "Biomes", "Custom mix", "Monuments", "Terrain composition",
            "Water & rivers", "Custom river settings", "Roads & trails", "Advanced routing", "Rails",
            "Rail settings", "Scenery & objects", "Landmarks", "Content rules", "Prefab blocklist", "Output"] {
            let frame = render(&mut studio, recipe_size, vec![], true);
            let pos = frame.shapes.iter().rev().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == section => {
                    let center = text.pos + text.galley.size() * 0.5;
                    shape.clip_rect.contains(center).then_some(center)
                }
                _ => None,
            });
            let Some(pos) = pos else { continue };
            for pressed in [true, false] {
                render(&mut studio, recipe_size, vec![egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton { pos, button: egui::PointerButton::Primary,
                        pressed, modifiers: egui::Modifiers::NONE }], true);
            }
        }
        let recipe = render(&mut studio, recipe_size, vec![egui::Event::PointerGone], true);
        let last_row = recipe.shapes.iter().filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if shape.clip_rect.contains(text.pos) => {
                Some(text.pos.y + text.galley.size().y)
            }
            _ => None,
        }).fold(0., f32::max);
        let recipe_size = [recipe_size[0], (last_row.ceil() as usize + 16).min(recipe_size[1])];
        studio.project.targets.clear();
        let catalog_size = [460, 140 + studio.template_picker.entries.len() * 26];
        render(&mut studio, catalog_size, vec![], false);
        let outliner = render(&mut studio, catalog_size, vec![], false);
        for (file, size, output) in [
            ("Mapper Expanded Recipe.png", recipe_size, recipe),
            ("Mapper Monument Catalog.png", catalog_size, outliner),
        ] {
            let image = textures.raster(&ctx, output, size);
            mapper_studio::image_export::write_png(&directory.join(file), &image, || false).unwrap();
        }
    }

    #[test]
    fn custom_outliner_switches_all_three_generation_choices() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.);
        let mut studio = crate::tests::fixture(&ctx);
        studio.project.custom_monuments.push(mapper_studio::custom_monuments::Placement {
            version: "a".repeat(64), name: "Harbor Workshop".into(), position: [10., 20., 30.],
            yaw: 45., presence: Presence::Required, baked: false,
        });
        let render = |studio: &mut Studio, events| {
            let mut frame = ctx.run_ui(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(180., 250.))),
                events, focused: true, ..Default::default()
            }, |ui| studio.outliner(ui, None));
            frame.textures_delta.clear();
            frame
        };
        render(&mut studio, vec![]);
        let frame = render(&mut studio, vec![]);
        let text = |label: &str| frame.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == label => Some(text.pos + text.galley.size() * 0.5),
            _ => None,
        });
        assert!(text("Custom monuments").is_some());
        let question = text("?").expect("Custom row has a generation preference control");
        for (offset, expected) in [(0., Presence::Allowed), (-22., Presence::Excluded), (-44., Presence::Required)] {
            let point = question + egui::vec2(offset, 0.);
            for pressed in [true, false] {
                render(&mut studio, vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
                    },
                ]);
            }
            assert_eq!(studio.project.custom_monuments[0].presence, expected);
        }
    }
    use mapper_studio::prefab_assets::Bounds;

    #[test]
    fn catalog_reuses_rows_and_refreshes_when_sources_change() {
        let root = tempfile::tempdir().unwrap();
        let (catalogue, _) = crate::prepared_assets::load(root.path());
        let mut catalogue = Arc::try_unwrap(catalogue).ok().unwrap();
        let sample = catalogue.entries.remove("assets/test.prefab").unwrap();
        let asset = |display_name| mapper_studio::prefab_assets::Asset {
            bounds: Some(Bounds {
                center: [0.; 3],
                extent: [10.; 3],
                minimum_world_size: 1000,
            }),
            display_name: Some(String::from(display_name)),
            ..sample.clone()
        };
        catalogue.entries.insert("assets/site_1.prefab".into(), asset("Site"));
        catalogue.entries.insert("assets/site_2.prefab".into(), asset("Site"));
        let mut project = Project {
            preview_engine: mapper_studio::native_preview::VERSION.into(),
            installation: Some(catalogue.installation.clone()),
            ..Project::default()
        };
        let catalogue = Arc::new(catalogue);
        let mut icons = monument_icon_panel::Panel::default();
        let mut picker = TemplatePicker::default();
        picker.refresh(&project, Some(&catalogue), &icons);
        assert_eq!(picker.entries[1].label, "site 2");
        let allocation = picker.entries.as_ptr();
        project.size += 100;
        picker.refresh(&project, Some(&catalogue), &icons);
        assert_eq!(picker.entries.as_ptr(), allocation);
        icons.seed_names(catalogue.clone());
        picker.refresh(&project, Some(&catalogue), &icons);
        assert_eq!(picker.entries[1].label, "2 · Site");

        let mut next = Catalog {
            sha256: catalogue.sha256.clone(),
            installation: catalogue.installation.clone(),
            entries: catalogue.entries.clone(),
            sources: catalogue.sources.clone(),
        };
        next.entries.insert("assets/earlier.prefab".into(), asset("Earlier"));
        let next = Arc::new(next);
        picker.refresh(&project, Some(&next), &icons);
        assert_eq!(picker.entries[2].path, "assets/site_2.prefab");
        assert_eq!(Arc::strong_count(&next), 1);

        picker.refresh(&project, None, &icons);
        assert!(picker.entries.is_empty());
    }
}
