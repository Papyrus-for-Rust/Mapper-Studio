use super::*;
use mapper_studio::preferences::{Appearance, DEFAULT_UI_SCALE, Preferences, UI_SCALE_PRESETS};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(super) enum Page {
    #[default]
    Installations,
    Performance,
    Appearance,
    Viewport,
    Icons,
    Updates,
    Support,
}
impl Studio {
    pub(super) fn open_installation_settings(&mut self) {
        self.settings_page = Page::Installations;
        self.settings_open = true;
    }

    pub(super) fn prepare_installation_startup(&mut self) {
        self.restore_source = true;
        self.installation_setup_open = self.preferences.rust_folder.is_none();
        self.status = if self.installation_setup_open {
            "Choose a Rust installation to load its terrain backend and assets."
        } else {
            "Checking the saved Rust installation. Assets load automatically."
        }
        .into();
    }
    pub(super) fn installation_setup_window(&mut self, ctx: &egui::Context) {
        if self.source.bound_root(&self.project).is_some() || self.source.has_pending_source() {
            self.installation_setup_open = false;
        }
        if !self.installation_setup_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Choose your Rust installation")
            .id(egui::Id::new("mapper-installation-setup"))
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .default_width(440.)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("Maps use this installation’s terrain and assets.");
                ui.add_space(12.);
                if self.installation_library.resetting() {
                    ui.label("Resetting asset cache…");
                } else {
                    self.source.folder_picker(ui, &mut self.project);
                }
                let message = self.source.setup_message();
                if !message.is_empty() { ui.label(message); }
                self.source.copy_inspection_error(ui);
                ui.add_space(12.);
                if ui.button("Set up later").clicked() {
                    self.installation_setup_open = false;
                }
            });
        self.installation_setup_open &= open;
    }
    fn apply_appearance(&self, ctx: &egui::Context) {
        ctx.set_theme(match self.preferences.appearance {
            Appearance::System => egui::ThemePreference::System,
            Appearance::Dark => egui::ThemePreference::Dark,
            Appearance::Light => egui::ThemePreference::Light,
        });
        workbench_style::set_reduced_motion(ctx, self.preferences.reduce_motion);
        ctx.set_zoom_factor(f32::from(self.preferences.ui_scale_percent) / 100.);
    }

    fn set_ui_scale(&mut self, ctx: &egui::Context, percent: u16) {
        if self.preferences.ui_scale_percent != percent {
            self.preferences.ui_scale_percent = percent;
            ctx.set_zoom_factor(f32::from(percent) / 100.);
            self.save_preferences();
        }
    }

    pub(super) fn handle_ui_scale_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::gui_zoom::kb_shortcuts::{ZOOM_IN, ZOOM_IN_SECONDARY, ZOOM_OUT, ZOOM_RESET};

        let current = self.preferences.ui_scale_percent;
        let next = ctx.input_mut(|input| {
            if input.consume_shortcut(&ZOOM_RESET) {
                Some(DEFAULT_UI_SCALE)
            } else if input.consume_shortcut(&ZOOM_IN) || input.consume_shortcut(&ZOOM_IN_SECONDARY) {
                UI_SCALE_PRESETS.iter().copied().find(|percent| *percent > current)
            } else if input.consume_shortcut(&ZOOM_OUT) {
                UI_SCALE_PRESETS.iter().rev().copied().find(|percent| *percent < current)
            } else {
                None
            }
        });
        if let Some(percent) = next {
            self.set_ui_scale(ctx, percent);
        }
    }
    pub(super) fn load_preferences(&mut self, ctx: &egui::Context) {
        match Preferences::path()
            .and_then(|path| Preferences::load(&path).map(|prefs| (path, prefs)))
        {
            Ok((path, prefs)) => {
                self.settings_path = Some(path);
                self.preferences = prefs;
                if self.preferences.migrate_installations().unwrap_or(false) {self.save_preferences();}
            }
            Err(e) => {
                self.settings_message = format!(
                    "Settings could not be loaded: {e}. The existing file has been retained."
                );
            }
        }
        self.live_generation = self.preferences.live_generation;
        self.grid = self.preferences.map_grid;
        self.show_no_build = self.preferences.map_no_build;
        self.show_monuments = self.preferences.monument_splats;
        self.show_decoration = self.preferences.decoration_models;
        let _ = mapper_studio::generation_resources::configure(self.preferences.generation_resources);
        ctx.options_mut(|options| options.zoom_with_keyboard = false);
        self.apply_appearance(ctx);
    }
    pub(super) fn tick_preferences(&mut self, ctx: &egui::Context) {
        if self.restore_source && !self.installation_library.resetting() {
            self.restore_source = false;
            if let Some(root) = self.preferences.rust_folder.clone() {
                self.source.start(root, &self.project, ctx);
            }
        }
        self.monument_icons.tick(&self.project,self.source.bound_root(&self.project),self.prefabs.catalog(&self.project),ctx);
        let was_bound = self.source.bound_root(&self.project).is_some();
        let before = self.history.snapshot(&self.project);
        self.source.tick(&mut self.project, ctx);
        if !self.generation.busy() {
            let river_root = self.source.bound_root(&self.project);
            self.water.tick(&mut self.project, river_root, ctx);
        }
        if self.source.take_source_changed() {
            // Cancelling owned tasks also prevents late results being attached to the new installation.
            self.cancel_generation();
            self.water = Default::default();
            self.task = None;
            self.placement_task = None;
            self.prefabs = Default::default();
            self.monuments = Default::default();
            self.monument_icons = Default::default();
            self.restrictions = Default::default();
            self.roads = Default::default();
            self.rails = Default::default();
            self.decoration_panel = Default::default();
            self.decoration_models = Default::default();
            self.composition = Default::default();
            self.map_colors = Default::default();
            self.preparation = Default::default();
            self.terrain_projection = Default::default();
            self.preview = None;
            self.texture = None;
            self.layer_texture = None;
            self.clear_stage_preview();
            self.generation_progress = None;
            self.layouts.clear();
            self.active_layout = None;
            if let Some(before_drag) = self.drag_before.take() {
                self.project = before_drag;
            }
            self.drag_origin = None;
            self.drag_press = None;
            self.drag_splat = None;
            self.placement_request = None;
            self.status = if self.installation_library.resetting() {
                "Resetting asset cache…"
            } else {
                "Rust files changed. Reloading installation data."
            }.into();
        }
        if self.installation_library.poll_reset(ctx) {
            self.status.clone_from(&self.installation_library.message);
            self.restore_source = false;
            if let Some(path) = self.source.selected_root().or_else(|| self.preferences.rust_folder.clone()) {
                self.source.start(path, &self.project, ctx);
            }
        }
        if let Some((root, prepared)) = self.source.take_prepared_recipe() {
            self.prefabs.accept_prepared(root, prepared.catalog, prepared.bindings, prepared.report);
            self.status = "Rust installation switched.".into();
        } else if !was_bound && self.source.bound_root(&self.project).is_some() {
            self.status=if self.project.installation.as_ref().is_some_and(|source| {
                mapper_studio::native_profile::for_source(source,self.project.native_profile.as_ref()).is_ok()
            }) {
                "Rust terrain adapter ready. Checking monument assets."
            } else {
                "Rust installation selected. This version needs a native generation adapter."
            }.into();
        }
        self.history.finish_snapshot(before, &self.project, true);
    }
    pub(super) fn persist_preferences(&mut self) {
        let mut current = self.preferences.clone();
        if let Some(root) = self.source.selected_root()
            && let Err(error)=current.remember_installation(root,true){self.settings_message=error;}
        current.live_generation = self.live_generation;
        current.map_grid = self.grid;
        current.map_no_build = self.show_no_build;
        current.monument_splats = self.show_monuments;
        current.decoration_models = self.show_decoration;
        if current != self.preferences {
            self.preferences = current;
            self.save_preferences();
        }
    }
    pub(super) fn save_preferences(&mut self) {
        if let Some(path) = &self.settings_path {
            self.settings_message = match self.preferences.save(path) {
                Ok(()) => String::new(),
                Err(e) => format!("Could not save settings: {e}"),
            };
        }
    }
    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        if self.source.has_pending_source() && !self.settings_open {
            self.open_installation_settings();
        }
        if !self.settings_open {
            return;
        }
        use super::settings_dialog::{self, Icon};
        let pages = [
            (Page::Installations, "Rust installations", Icon::Folder),
            (Page::Performance, "Performance", Icon::Performance),
            (Page::Appearance, "Appearance", Icon::Appearance),
            (Page::Viewport, "Viewport", Icon::Viewport),
            (Page::Icons, "Monument icons", Icon::Monument),
            (Page::Updates, "Updates", Icon::Updates),
            (Page::Support, "Support", Icon::Rules),
        ];
        let (page, closed) = settings_dialog::show(
            ctx, "mapper-settings", "Settings", pages, self.settings_page, false, |ui, page| {
                if let Some((_, label, _)) = pages.iter().find(|(key, _, _)| *key == page) {
                    ui.heading(*label);
                    ui.add_space(4.);
                }
                if !self.settings_message.is_empty() {
                    let label = egui::RichText::new("Settings error")
                        .color(ui.visuals().error_fg_color);
                    ui.menu_button(label, |ui| {
                        diagnostic_ui::copy_error(ui, &self.settings_message, None);
                        diagnostic_ui::details(ui, &self.settings_message);
                    });
                }
                match page {
                    Page::Installations => self.installation_settings(ui),
                    Page::Performance => self.performance_settings(ui),
                    Page::Appearance => self.appearance_settings(ui, ctx),
                    Page::Viewport => self.viewport_settings(ui),
                    Page::Icons => {
                        let catalog = self.prefabs.catalog(&self.project);
                        if self.monument_icons.settings(
                            ui, &self.project, catalog.as_deref(), &mut self.preferences,
                            self.settings_path.as_deref(),
                        ) {
                            self.save_preferences();
                        }
                    }
                    Page::Support => {
                        let snapshot = self.support_environment().snapshot(None);
                        match (serde_json::to_string_pretty(&snapshot), snapshot.encode()) {
                            (Ok(preview), Ok(code)) => diagnostic_ui::support_controls(ui, &preview, &code),
                            _ => { ui.label("Could not create the support snapshot."); }
                        }
                    }
                    Page::Updates => {
                        preference_section(ui, "Startup");
                        if preference_switch(ui, "Check for updates on launch", &mut self.preferences.check_for_updates,
                            "Check GitHub for a newer Mapper release at startup.").changed()
                        {
                            self.save_preferences();
                        }
                        ui.weak("Mapper asks before installing an available update.");
                    }
                }
            },
        );
        self.settings_page = page;
        self.settings_open = !closed;
    }

    fn installation_settings(&mut self, ui: &mut egui::Ui) {
        let busy = self.generation_in_progress()
            || self.source.inspecting()
            || !mapper_studio::loading_progress::active().is_empty()
            || self.installation_library.resetting();
        preference_section(ui, "Installation library");
        let (changed, action) = self
            .installation_library
            .controls(ui, &mut self.preferences, busy);
        if changed {
            self.save_preferences();
        }
        match action {
            Some(installation_panel::Action::Select(path)) => {
                self.source.start(path, &self.project, ui.ctx())
            }
            Some(installation_panel::Action::Forget) => self.source.forget(false),
            Some(installation_panel::Action::Reset) => {
                match mapper_studio::storage::local_data() {
                    Ok(root) => {
                        self.source.forget(true);
                        self.installation_library.request_reset(root, ui.ctx());
                    }
                    Err(error) => self.installation_library.message = error,
                }
            }
            None => {}
        }
        preference_section(ui, "Preparation");
        ui.add_enabled_ui(!self.installation_library.resetting(), |ui| {
            self.prefabs
                .readiness(ui, &self.project, self.source.bound_root(&self.project));
            egui::CollapsingHeader::new("Version details")
                .open(self.source.has_pending_source().then_some(true))
                .show(ui, |ui| self.source.settings(ui, &mut self.project, !busy));
            ui.collapsing("Static model preparation", |ui| {
                self.decoration_models.show(ui)
            });
        });
        if let Some(path) = &self.settings_path {
            ui.collapsing("Settings file", |ui| {
                ui.small(path.display().to_string());
            });
        }
    }

    fn performance_settings(&mut self, ui: &mut egui::Ui) {
        use mapper_studio::generation_resources::{self, ImageCompute};

        preference_section(ui, "CPU & memory");
        let before = self.preferences.generation_resources;
        let limits = &mut self.preferences.generation_resources;
        limit_row(ui, "CPU workers", &mut limits.cpu_workers, 1..=generation_resources::available_workers(),
            "Maximum workers for terrain and map images. Native libraries manage their own threads. Applies to the next job.");
        limit_row(ui, "Terrain memory (MiB)", &mut limits.terrain_memory_mib, 256..=16384,
            "Maximum terrain-processing buffers for the next generation job.");
        limit_row(ui, "Image memory (MiB)", &mut limits.image_memory_mib, 16..=4096,
            "Maximum image-job buffers. Native library allocations are managed separately.");

        preference_section(ui, "Graphics");
        preference_row(ui, "Map image compute", |ui, label| {
            choice_group(ui, label, &mut limits.image_compute, [
                (ImageCompute::Auto, "Auto", "Use GPU compute for compatible large images within the buffer limit."),
                (ImageCompute::Cpu, "CPU", "Render map images on the CPU."),
                (ImageCompute::Gpu, "GPU", "Accelerate PaperMap images when supported. Falls back to CPU when the buffer limit is exceeded."),
            ]);
        });
        limit_row(ui, "GPU buffers (MiB)", &mut limits.gpu_memory_mib, 32..=2048,
            "Buffer limit for GPU map images and GPU-bound erosion recipes.");
        if before != *limits {
            self.erosion_comparison.cancel();
            let _ = mapper_studio::generation_resources::configure(*limits);
            self.save_preferences();
        }
        let limits = self.preferences.generation_resources;
        let busy = self.generation.busy();
        if self.document_recipe().supports_gpu_erosion() {
            preference_section(ui, "Erosion benchmark");
            self.with_recipe_draft(|studio| {
                let comparison = &mut studio.erosion_comparison;
                comparison.sync(studio.project.size, limits);
                comparison.action(ui, busy);
                comparison.result(ui, &mut studio.project);
                if let Some(required) = comparison.required_gpu_memory(limits) {
                    ui.colored_label(ui.visuals().warn_fg_color, format!("GPU needs {required} MiB"))
                        .on_hover_text("Raise GPU buffers to run the GPU comparison.");
                }
            });
        }
        let status = mapper_studio::generation_resources::status();
        if !status.is_empty() {
            ui.collapsing("Compute details", |ui| {
                ui.small(status);
            });
        }
    }

    fn appearance_settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        preference_section(ui, "Interface");
        let before = self.preferences.appearance;
        preference_row(ui, "Theme", |ui, label| {
            choice_group(ui, label, &mut self.preferences.appearance, [
                (Appearance::System, "System", "Follow the operating system appearance."),
                (Appearance::Dark, "Dark", "Use dark appearance."),
                (Appearance::Light, "Light", "Use light appearance."),
            ]);
        });
        let mut scale = self.preferences.ui_scale_percent;
        preference_row(ui, "UI scale", |ui, label| {
            egui::ComboBox::from_id_salt("ui-scale")
                .width(90.)
                .selected_text(format!("{scale}%"))
                .show_ui(ui, |ui| {
                    for percent in UI_SCALE_PRESETS {
                        ui.selectable_value(&mut scale, percent, format!("{percent}%"));
                    }
                })
                .response
                .labelled_by(label)
                .on_hover_text("Change the size of Mapper’s interface. Ctrl+0 resets to 100%.");
        });
        self.set_ui_scale(ctx, scale);
        let motion = preference_switch(ui, "Reduce interface motion", &mut self.preferences.reduce_motion,
            "Make panel transitions, queue scrolling, and progress updates immediate.").changed();
        if before != self.preferences.appearance || motion {
            self.apply_appearance(ctx);
            self.save_preferences();
        }
        preference_section(ui, "Map colors");
        let palette = self
            .preview
            .as_ref()
            .and_then(|preview| preview.palette.clone());
        if color_panel::Panel::controls(
            ui,
            &mut self.preferences.map_colors,
            palette.as_ref().and_then(|palette| palette.defaults()),
        ) {
            self.save_preferences();
        }
        if !self.map_colors.message.is_empty() {
            diagnostic_ui::message(ui, &self.map_colors.message, "map-colors-details");
        }
    }

    fn viewport_settings(&mut self, ui: &mut egui::Ui) {
        use mapper_studio::map_labels::Mode;

        preference_section(ui, "Map overlays");
        let before = self.preferences.monument_markers;
        preference_row(ui, "Monument markers", |ui, label| {
            choice_group(ui, label, &mut self.preferences.monument_markers,
                Mode::ALL.map(|choice| (choice, choice.label(), "Choose how monuments appear on the map.")));
        });
        if before != self.preferences.monument_markers {
            self.save_preferences();
        }
        preference_switch(ui, "Map grid", &mut self.grid, "Show Rust grid coordinates over the map.");
        preference_switch(ui, "No-build zones", &mut self.show_no_build,
            "Show the footprints of building-restriction colliders from the selected installation.");
        preference_switch(ui, "Monument footprints", &mut self.show_monuments,
            "Show monument terrain stamps in the viewport.");

        preference_section(ui, "Preview");
        if preference_switch(ui, "Live generation previews", &mut self.live_generation,
            "Show small previews after completed stages while generation continues.").changed()
            && !self.live_generation
        {
            self.clear_stage_preview();
        }
        preference_switch(ui, "Static decoration in 3D", &mut self.show_decoration,
            "Draw supported scenery models when inspecting the map in 3D.");
    }
}

fn preference_section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(6.);
    ui.separator();
    ui.strong(title);
}

fn preference_row(ui: &mut egui::Ui, title: &str, control: impl FnOnce(&mut egui::Ui, egui::Id)) {
    ui.horizontal(|ui| {
        let width = ui.available_width();
        let control_width = (width * 0.43).clamp(145., 250.).min(width * 0.55);
        let label_width = (width - control_width - ui.spacing().item_spacing.x).max(1.);
        let label = ui.allocate_ui_with_layout(
            egui::vec2(label_width, 32.), egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_min_width(label_width);
                ui.add(egui::Label::new(title).wrap())
            },
        ).inner;
        ui.allocate_ui_with_layout(
            egui::vec2(control_width, 32.), egui::Layout::right_to_left(egui::Align::Center),
            |ui| control(ui, label.id),
        );
    });
}

fn choice_group<T: Copy + PartialEq>(
    ui: &mut egui::Ui, label: egui::Id, value: &mut T, choices: [(T, &str, &str); 3],
) {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let padding = ui.spacing().button_padding.x * 2.;
    let width = 2. + choices.iter().map(|(_, text, _)| {
        ui.painter().layout_no_wrap((*text).into(), font.clone(), ui.visuals().text_color())
            .size().x.ceil() + padding
    }).sum::<f32>();
    ui.allocate_ui_with_layout(
        egui::vec2(width, ui.spacing().interact_size.y), egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = 1.;
            for (index, (choice, text, help)) in choices.into_iter().enumerate() {
                let corners = egui::CornerRadius {
                    nw: if index == 0 { 6 } else { 0 }, sw: if index == 0 { 6 } else { 0 },
                    ne: if index == 2 { 6 } else { 0 }, se: if index == 2 { 6 } else { 0 },
                };
                if ui.add(egui::Button::selectable(*value == choice, text)
                    .frame(true).frame_when_inactive(true).corner_radius(corners))
                    .labelled_by(label).on_hover_text(help).clicked()
                {
                    *value = choice;
                }
            }
        }
    );
}

fn limit_row(
    ui: &mut egui::Ui, title: &str, value: &mut usize, range: std::ops::RangeInclusive<usize>, help: &str,
) {
    preference_row(ui, title, |ui, label| {
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.spacing_mut().slider_width = (ui.available_width() - 57.).max(35.);
            ui.add(egui::Slider::new(value, range)).labelled_by(label).on_hover_text(help);
        });
    });
}

fn preference_switch(ui: &mut egui::Ui, title: &str, value: &mut bool, help: &str) -> egui::Response {
    let (rect, mut response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 32.), egui::Sense::click(),
    );
    if response.clicked() {
        *value = !*value;
        response.mark_changed();
    }
    response.widget_info(|| egui::WidgetInfo::selected(
        egui::WidgetType::Checkbox, response.enabled(), *value, title,
    ));
    if response.hovered() || response.has_focus() {
        ui.painter().rect_filled(rect, 5, ui.visuals().faint_bg_color);
    }
    let center = rect.right_center() - egui::vec2(16., 0.);
    workbench_style::paint_switch(ui, &response, center, *value);
    let label_rect = egui::Rect::from_min_max(rect.min + egui::vec2(0., 2.),
        egui::pos2(center.x - 24., rect.bottom() - 2.));
    let painter = ui.painter_at(label_rect);
    painter.text(label_rect.left_center(), egui::Align2::LEFT_CENTER, title,
        egui::TextStyle::Body.resolve(ui.style()), ui.visuals().text_color());
    response.on_hover_text(help)
}

#[cfg(test)]
mod tests {
    use super::*;
    mod snapshot {
        include!("../examples/support/egui_snapshot.rs");
    }

    #[test]
    fn appearance_scale_applies_and_persists() {
        let ctx = egui::Context::default();
        workbench_style::install(&ctx);
        ctx.options_mut(|options| options.zoom_with_keyboard = false);
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("settings.json");
        let mut app = super::super::tests::fixture(&ctx);
        app.settings_open = true;
        app.settings_page = Page::Appearance;
        app.settings_path = Some(path.clone());
        let project = app.project.clone();

        let render = |app: &mut Studio, events| {
            let mut output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO, egui::vec2(1050., 720.) / ctx.zoom_factor(),
                )),
                focused: true,
                events,
                ..Default::default()
            }, |_| {
                app.settings_window(&ctx);
                app.handle_ui_scale_shortcuts(&ctx);
            });
            output.textures_delta.clear();
            output
        };
        let position = |output: &egui::FullOutput, label| {
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == label => {
                    let center = text.pos + text.galley.rect.center().to_vec2();
                    shape.clip_rect.contains(center).then_some(center)
                }
                _ => None,
            }).unwrap_or_else(|| panic!("{label} should be visible"))
        };
        let click = |app: &mut Studio, point| {
            for pressed in [true, false] {
                render(app, vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point, button: egui::PointerButton::Primary,
                        pressed, modifiers: egui::Modifiers::NONE,
                    },
                ]);
            }
        };

        render(&mut app, vec![]);
        let initial = render(&mut app, vec![]);
        click(&mut app, position(&initial, "100%"));
        render(&mut app, vec![]);
        let open = render(&mut app, vec![]);
        click(&mut app, position(&open, "150%"));
        render(&mut app, vec![]);
        let scaled = render(&mut app, vec![]);
        assert_eq!(ctx.zoom_factor(), 1.5);
        position(&scaled, "UI scale");
        position(&scaled, "Done");
        assert_eq!(Preferences::load(&path).unwrap().ui_scale_percent, 150);

        render(&mut app, vec![egui::Event::Key {
            key: egui::Key::Num0, physical_key: None, pressed: true,
            repeat: false, modifiers: egui::Modifiers::COMMAND,
        }]);
        render(&mut app, vec![]);
        assert_eq!(ctx.zoom_factor(), 1.);
        assert_eq!(Preferences::load(&path).unwrap().ui_scale_percent, 100);
        assert_eq!(app.project, project);
    }

    #[test]
    fn appearance_motion_setting_applies_to_both_themes_and_persists() {
        let ctx = egui::Context::default();
        workbench_style::install(&ctx);
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("settings.json");
        let mut app = super::super::tests::fixture(&ctx);
        app.settings_open = true;
        app.settings_page = Page::Appearance;
        app.settings_path = Some(path.clone());
        let recipe = app.project.clone();
        let size = [640, 600];
        let mut textures = snapshot::Textures::default();
        let mut render = |app: &mut Studio, events| {
            let mut output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO, egui::vec2(size[0] as f32, size[1] as f32),
                )),
                focused: true,
                events,
                ..Default::default()
            }, |_| app.settings_window(&ctx));
            textures.update(&output.textures_delta);
            output.textures_delta.clear();
            output
        };
        let request = |output: &egui::FullOutput| {
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == "Reduce interface motion" => {
                    let center = text.pos + text.galley.rect.center().to_vec2();
                    shape.clip_rect.contains(center).then_some(center)
                }
                _ => None,
            }).expect("Reduce interface motion is visible")
        };
        render(&mut app, vec![]);
        for reduce in [true, false] {
            let point = request(&render(&mut app, vec![]));
            for pressed in [true, false] {
                render(&mut app, vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point, button: egui::PointerButton::Primary,
                        pressed, modifiers: egui::Modifiers::NONE,
                    },
                ]);
            }
            assert_eq!(app.preferences.reduce_motion, reduce);
            assert_eq!(Preferences::load(&path).unwrap().reduce_motion, reduce);
            for theme in [egui::Theme::Dark, egui::Theme::Light] {
                let style = ctx.style_of(theme);
                assert_eq!(style.animation_time == 0., reduce);
                assert_eq!(style.scroll_animation.duration.max == 0., reduce);
            }
            assert_eq!(app.project, recipe);
        }
        let output = render(&mut app, vec![egui::Event::PointerGone]);
        if let Some(dir) = std::env::var_os("MAPPER_MOTION_SNAPSHOT_DIR") {
            let image = textures.raster(&ctx, output, size);
            mapper_studio::image_export::write_png(
                &std::path::PathBuf::from(dir).join("appearance-motion.png"), &image, || false,
            ).unwrap();
        }
    }
}
