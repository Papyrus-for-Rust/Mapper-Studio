use super::*;

fn shortcut_button(
    ctx: &egui::Context,
    label: &'static str,
    modifiers: egui::Modifiers,
    key: egui::Key,
) -> egui::Button<'static> {
    let shortcut = egui::KeyboardShortcut::new(modifiers, key);
    egui::Button::new(label).shortcut_text(ctx.format_shortcut(&shortcut))
}

impl Studio {
    pub(super) fn map_tab_path(&self) -> Option<egui_dock::TabPath> {
        self.dock.find_tab(&Tab::Map).or_else(|| {
            self.dock.find_tab_from(|tab| matches!(tab, Tab::Generated(_)))
        })
    }

    pub(super) fn show_tab(&mut self, tab: Tab) {
        self.requested_tab = Some(tab);
    }
    pub(super) fn apply_tab_request(&mut self) {
        self.apply_requested_tab();
        self.finish_map_tab_requests();
    }
    fn apply_requested_tab(&mut self) {
        let Some(tab) = self.requested_tab.take() else {
            return;
        };
        if let Some(path) = self.dock.find_tab(&tab) {
            let _ = self.dock.set_active_tab(path);
            if matches!(tab, Tab::Map | Tab::Generated(_) | Tab::Nodes) {
                self.dock.set_focused_node_and_surface(path.node_path());
            }
        } else {
            let anchor = match tab {
                Tab::Map | Tab::Generated(_) | Tab::Nodes => self.map_tab_path(),
                Tab::Content | Tab::Plugins => self.dock.find_tab(&Tab::Inspector),
                other => self.dock.find_tab(&other),
            };
            if let Some(path) = anchor {
                self.dock.set_focused_node_and_surface(path.node_path());
            }
            if matches!(tab, Tab::Generated(_))
                && self.dock.find_tab_from(|tab| matches!(tab, Tab::Generated(_))).is_none()
                && let Some(path) = self.dock.find_tab(&Tab::Map)
                && let Ok(leaf) = self.dock.leaf_mut(path.node_path())
            {
                leaf.tabs[path.tab.0] = tab;
                leaf.active = path.tab;
            } else {
                self.dock.push_to_focused_leaf(tab);
            }
        }
        if matches!(tab, Tab::Generated(_))
            && let Some(path) = self.dock.find_tab(&Tab::Inspector)
        {
            let _ = self.dock.set_active_tab(path);
        }
    }
    pub(super) fn populate_monuments(&mut self, ctx: &egui::Context) {
        let Some(catalog) = self.prefabs.catalog(&self.project) else {
            self.status =
                "Choose Rust in Settings and wait for automatic asset verification.".into();
            return;
        };
        if self.prefabs.bindings(&self.project).is_none() {
            self.status =
                "Installation textures are not ready. Check Settings for verification status."
                    .into();
            return;
        }
        let before = self.project.clone();
        match self
            .monuments
            .populate_installation(&mut self.project, &catalog)
        {
            Ok(n) => {
                if self.project == before {
                    self.status =
                        "Installation stage candidates are already in this project.".into();
                    return;
                }
                self.status = format!("Added {n} monument candidates.");
                if self.preview_current() {
                    self.start_placement(ctx);
                }
            }
            Err(error) => self.status = error,
        }
    }
    fn undo_recipe(&mut self, redo: bool) {
        self.topology_tool.cancel_stroke();
        self.plugins.cancel();
        if !self.undo_generation_draft(redo) {
            if redo {
                self.history.redo(&mut self.project);
            } else {
                self.history.undo(&mut self.project);
            }
            self.refresh_layout_status();
        }
        self.skip_history = true;
    }
    fn can_step_edit(&self, ctx: &egui::Context, redo: bool) -> bool {
        if self.drag_before.is_some() {
            return false;
        }
        if let Some(id) = self.focused_map_edit(ctx) {
            return self.can_step_map_edit(id, redo);
        }
        if redo {
            self.history.can_redo()
        } else {
            self.history.can_undo()
        }
    }
    fn step_edit(&mut self, ctx: &egui::Context, redo: bool) {
        if let Some(id) = self.focused_map_edit(ctx) {
            self.step_map_edit(id, redo);
        } else {
            self.undo_recipe(redo);
        }
    }
    fn fit_map(&mut self) {
        self.zoom = 1.;
        self.pan = Vec2::ZERO;
        self.camera.reset();
    }
    pub(super) fn toolbar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let command = egui::Modifiers::COMMAND;
        let save_as = command | egui::Modifiers::SHIFT;
        if self.pending.is_none() && self.drag_before.is_none() && !self.settings_open {
            if ctx.input_mut(|i| {
                i.consume_shortcut(&egui::KeyboardShortcut::new(command, egui::Key::N))
            }) {
                self.request(Action::New, &ctx);
            }
            if ctx.input_mut(|i| {
                i.consume_shortcut(&egui::KeyboardShortcut::new(command, egui::Key::O))
            }) {
                self.request(Action::Open, &ctx);
            }
            if ctx.input_mut(|i| {
                i.consume_shortcut(&egui::KeyboardShortcut::new(save_as, egui::Key::S))
            }) {
                self.save(true, &ctx);
            } else if ctx.input_mut(|i| {
                i.consume_shortcut(&egui::KeyboardShortcut::new(command, egui::Key::S))
            }) {
                self.save(false, &ctx);
            }
            if !ctx.egui_wants_keyboard_input() || self.focused_map_canvas(&ctx).is_some() {
                if ctx.input_mut(|i| {
                    i.consume_shortcut(&egui::KeyboardShortcut::new(command, egui::Key::Y))
                        || i.consume_shortcut(&egui::KeyboardShortcut::new(save_as, egui::Key::Z))
                }) {
                    self.step_edit(&ctx, true);
                } else if ctx.input_mut(|i| {
                    i.consume_shortcut(&egui::KeyboardShortcut::new(command, egui::Key::Z))
                }) {
                    self.step_edit(&ctx, false);
                }
            }
        }
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui
                    .add(shortcut_button(&ctx, "New project", command, egui::Key::N))
                    .clicked()
                {
                    ui.close();
                    self.request(Action::New, &ctx);
                }
                if ui
                    .add(shortcut_button(
                        &ctx,
                        "Open project…",
                        command,
                        egui::Key::O,
                    ))
                    .clicked()
                {
                    ui.close();
                    self.request(Action::Open, &ctx);
                }
                if ui
                    .add(shortcut_button(&ctx, "Save project", command, egui::Key::S))
                    .clicked()
                {
                    ui.close();
                    self.save(false, &ctx);
                }
                if ui
                    .add(shortcut_button(
                        &ctx,
                        "Save project as…",
                        save_as,
                        egui::Key::S,
                    ))
                    .clicked()
                {
                    ui.close();
                    self.save(true, &ctx);
                }
                ui.separator();
                ui.menu_button("Recipes", |ui| self.profiles_menu(ui));
                if ui.button("Import custom content…").clicked() {
                    ui.close();
                    self.content.choose_file(&ctx);
                }
                ui.separator();
                let active_map = self.visible_generated_tab();
                let applied = active_map.is_some_and(|id| self.map_has_pending_edits(id));
                let image_label = if applied { "Export applied map image…" } else { "Export map image…" };
                let image_ready = match active_map {
                    Some(id) => self.generated_image_recipe(id).is_some(),
                    None => self.exportable_map().is_some(),
                };
                if ui
                    .add_enabled(image_ready, egui::Button::new(image_label))
                    .on_disabled_hover_text("Open a generated map to export its image.")
                    .clicked()
                {
                    ui.close();
                    self.image_export_target = active_map;
                    self.export_options_open = true;
                }
                let archive = self.focused_map_archive();
                let exporting = self.map_archive.exporting();
                let export_label = map_archive::Notification::export_label(
                    archive.as_ref().is_some_and(|(_, _, notification)| notification.is_some()),
                    applied,
                );
                if ui
                    .add_enabled(!exporting && archive.is_some(), egui::Button::new(export_label))
                    .on_disabled_hover_text(if exporting {
                        "A map export is in progress"
                    } else {
                        "Open a completed map to export it as a .map file."
                    })
                    .clicked()
                    && let Some((archive, label, notification)) = archive
                {
                    ui.close();
                    let directory = export_panel::dialog_directory(
                        self.preferences.export_folder.as_deref(),
                        self.path.as_deref(),
                    );
                    self.map_archive.export(archive, &label, notification, directory, &ctx);
                }
                ui.separator();
                if ui.button("Exit").clicked() {
                    ui.close();
                    self.request(Action::Close, &ctx);
                }
            });
            ui.menu_button("Edit", |ui| {
                if ui
                    .add_enabled(
                        self.can_step_edit(&ctx, false),
                        shortcut_button(&ctx, "Undo", command, egui::Key::Z),
                    )
                    .clicked()
                {
                    ui.close();
                    self.step_edit(&ctx, false);
                }
                if ui
                    .add_enabled(
                        self.can_step_edit(&ctx, true),
                        shortcut_button(&ctx, "Redo", command, egui::Key::Y),
                    )
                    .clicked()
                {
                    ui.close();
                    self.step_edit(&ctx, true);
                }
                ui.separator();
                if ui.button("Settings…").clicked() {
                    ui.close();
                    self.settings_open = true;
                }
            });
            ui.menu_button("View", |ui| {
                ui.menu_button("Maps", |ui| self.map_tabs_menu(ui));
                if self.generated_view_controls(ui) {
                    return;
                }
                ui.selectable_value(&mut self.camera.enabled, false, "2D map");
                if ui
                    .selectable_value(&mut self.camera.enabled, true, "3D terrain")
                    .clicked()
                {
                    self.topology_tool.enabled = false;
                    self.topology_tool.cancel_stroke();
                }
                if ui.button("Fit map").clicked() {
                    ui.close();
                    self.fit_map();
                }
                ui.separator();
                ui.checkbox(&mut self.grid, "Grid");
                ui.checkbox(&mut self.show_no_build, "No-build zones");
                ui.checkbox(&mut self.show_monuments, "Monument footprints");
                ui.checkbox(&mut self.show_preferences, "Placement guides")
                    .on_hover_text("Amber shows the requested area. The line leads to where Mapper placed the monument.");
                ui.checkbox(&mut self.show_decoration, "Scenery models (3D)");
            });
            ui.menu_button("Generate", |ui| {
                let idle = !self.generation_in_progress();
                let readiness = self.generation_readiness();
                let mut generate = ui.add_enabled(
                    readiness.is_ok()
                        || matches!(readiness, Err(generation_ui::GenerationBlocker::MissingSeed)),
                    egui::Button::new("Generate recipe"),
                );
                if let Err(error) = readiness {
                    generate = generate.on_disabled_hover_text(error.message());
                }
                if generate.clicked() {
                    ui.close();
                    self.generate_recipe(&ctx);
                }
                if ui
                    .add_enabled(
                        idle && self.prefabs.bindings(&self.project).is_some(),
                        egui::Button::new("Refresh installation roster"),
                    )
                    .clicked()
                {
                    ui.close();
                    self.populate_monuments(&ctx);
                }
                if ui
                    .add_enabled(
                        idle && self.preview_current(),
                        egui::Button::new("Regenerate monuments"),
                    )
                    .clicked()
                {
                    ui.close();
                    self.start_placement(&ctx);
                }
                if ui
                    .add_enabled(!idle, egui::Button::new("Cancel generation"))
                    .clicked()
                {
                    self.cancel_generation();
                    ui.close();
                }
            });
            ui.menu_button("Window", |ui| {
                for (tab, label) in [
                    (Tab::Recipe, "Recipe"),
                    (Tab::Outliner, "Monuments"),
                    (Tab::Inspector, "Selection"),
                    (Tab::Queue, "Queue"),
                    (Tab::Content, "Custom content"),
                    (Tab::Plugins, "Plugins"),
                    (Tab::Nodes, "Nodes"),
                ] {
                    if ui.button(label).clicked() {
                        ui.close();
                        self.show_tab(tab);
                    }
                }
                ui.separator();
                if ui.button("Reset workspace layout").clicked() {
                    ui.close();
                    self.reset_layout = true;
                }
            });
            ui.menu_button("Help", |ui| {
                ui.label("Mapper for Rust");
                ui.small(format!("Version {}", self.updates.version().unwrap_or(env!("CARGO_PKG_VERSION"))));
                if ui.button("Check for updates…").clicked() {
                    self.updates.check(ui.ctx(), true);
                    ui.close();
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.project_io_controls(ui);
                self.export_controls(ui);
                let dirty = self.dirty();
                let name = self
                    .path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .and_then(|p| p.to_str())
                    .unwrap_or("Untitled project");
                egui::Frame::new()
                    .fill(ui.visuals().faint_bg_color)
                    .stroke(ui.visuals().window_stroke)
                    .corner_radius(12)
                    .inner_margin(egui::Margin::symmetric(9, 2))
                    .show(ui, |ui| ui.add_sized(
                        egui::vec2(140., 18.),
                        egui::Label::new(format!("{name}{}", if dirty { " *" } else { "" })).truncate(),
                    )).response.on_hover_text(format!(
                    "{name}\n{}",
                    if dirty { "Unsaved changes" } else { "Saved" }
                ));
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    mod snapshot {
        include!("../examples/support/egui_snapshot.rs");
    }

    fn label_position(frame: &egui::FullOutput, label: &str) -> Option<Pos2> {
        frame.shapes.iter().find_map(|shape| {
            let egui::Shape::Text(text) = &shape.shape else {
                return None;
            };
            let center = text.pos + text.galley.rect.center().to_vec2();
            (text.galley.job.text == label && shape.clip_rect.contains(center)).then_some(center)
        })
    }

    #[test]
    fn menus_keep_settings_overlays_and_panels_in_reachable_groups() {
        for dark in [true, false] {
            let theme = if dark {
                egui::Theme::Dark
            } else {
                egui::Theme::Light
            };
            let appearance = if dark { "dark" } else { "light" };
            let width = if dark { 720 } else { 1050 };
            for (menu, labels, command) in [
                (
                    "File",
                    &["New project", "Save project as…", "Recipes", "Export .map…"][..],
                    None,
                ),
                (
                    "Edit",
                    &["Undo", "Redo", "Settings…"][..],
                    Some("Settings…"),
                ),
                (
                    "View",
                    &["2D map", "3D terrain", "Grid", "No-build zones"][..],
                    Some("Grid"),
                ),
                (
                    "Window",
                    &["Recipe", "Queue", "Plugins", "Reset workspace layout"][..],
                    Some("Queue"),
                ),
            ] {
                let ctx = egui::Context::default();
                workbench_style::install(&ctx);
                ctx.set_theme(theme);
                let mut app = super::super::tests::fixture(&ctx);
                let recipe = app.project.clone();
                let mut textures = snapshot::Textures::default();
                let mut render = |app: &mut Studio, events| {
                    let mut frame = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(
                                Pos2::ZERO,
                                egui::vec2(width as f32, 320.),
                            )),
                            events,
                            ..Default::default()
                        },
                        |ui| app.toolbar(ui),
                    );
                    textures.update(&frame.textures_delta);
                    frame.textures_delta.clear();
                    frame
                };
                let press = |pos, pressed| egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                };
                render(&mut app, vec![]);
                let closed = render(&mut app, vec![]);
                let button = label_position(&closed, menu).unwrap();
                render(
                    &mut app,
                    vec![egui::Event::PointerMoved(button), press(button, true)],
                );
                render(&mut app, vec![press(button, false)]);
                let frame = (0..14).map(|_| render(&mut app, vec![])).last().unwrap();
                for label in labels {
                    assert!(
                        label_position(&frame, label).is_some(),
                        "{menu} is missing {label}"
                    );
                }
                if menu == "Window" {
                    assert!(label_position(&frame, "Performance").is_none());
                }
                let position = command.map(|label| label_position(&frame, label).unwrap());
                if let Some(position) = position {
                    render(
                        &mut app,
                        vec![egui::Event::PointerMoved(position), press(position, true)],
                    );
                    render(&mut app, vec![press(position, false)]);
                }
                if let Some(directory) = std::env::var_os("MAPPER_MENU_SNAPSHOTS") {
                    let path = PathBuf::from(directory).join(format!("{menu}-{appearance}.png"));
                    let image = textures.raster(&ctx, frame, [width, 320]);
                    mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
                }
                match menu {
                    "Edit" => assert!(app.settings_open),
                    "View" => assert!(app.grid),
                    "Window" => assert_eq!(app.requested_tab, Some(Tab::Queue)),
                    _ => {}
                }
                assert_eq!(app.project, recipe);
            }
        }
    }
}
