use super::*;
use mapper_studio::erosion_compute::{self, Identity};
use mapper_studio::preferences::MapSizeSnap;
use std::sync::mpsc::{self, Receiver, TryRecvError};

#[derive(Default)]
pub(super) enum ErosionDevice {
    #[default]
    Unchecked,
    Checking(Receiver<Result<Identity, String>>),
    Ready(Identity),
    Unavailable(String),
}

impl ErosionDevice {
    fn check(&mut self, ctx: &egui::Context) {
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        *self = match std::thread::Builder::new().name("mapper-gpu-discovery".into()).spawn(move || {
            let _ = sender.send(erosion_compute::detect());
            ctx.request_repaint();
        }) {
            Ok(_) => Self::Checking(receiver),
            Err(error) => Self::Unavailable(error.to_string()),
        };
    }

    fn poll(&mut self) {
        if let Self::Checking(receiver) = self {
            match receiver.try_recv() {
                Ok(Ok(device)) => *self = Self::Ready(device),
                Ok(Err(error)) => *self = Self::Unavailable(error),
                Err(TryRecvError::Disconnected) => {
                    *self = Self::Unavailable("GPU detection stopped".into());
                }
                Err(TryRecvError::Empty) => {}
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(super) enum RecipeTab {
    #[default]
    Map,
    Seeds,
    Terrain,
    Biomes,
    Monuments,
    Water,
    Roads,
    Rails,
    Scenery,
    Landmarks,
    Content,
    Output,
}

impl RecipeTab {
    const ALL: [Self; 12] = [
        Self::Map, Self::Seeds, Self::Terrain, Self::Biomes, Self::Monuments, Self::Water,
        Self::Roads, Self::Rails, Self::Scenery, Self::Landmarks, Self::Content, Self::Output,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Map => "Map",
            Self::Seeds => "Stage seeds",
            Self::Terrain => "Terrain",
            Self::Biomes => "Biomes",
            Self::Monuments => "Monuments",
            Self::Water => "Water & rivers",
            Self::Roads => "Roads & trails",
            Self::Rails => "Rails",
            Self::Scenery => "Scenery & objects",
            Self::Landmarks => "Landmarks",
            Self::Content => "Content rules",
            Self::Output => "Output",
        }
    }
}

const RECIPE_TAB_CONTENT: &str = "mapper-recipe-tab-content";
const RECIPE_TITLE_TEMPLATE_DRAFT: &str = "mapper-recipe-title-template-draft";

fn in_recipe_tab(ui: &egui::Ui) -> bool {
    ui.stack().iter().any(|entry| entry.tags().contains(RECIPE_TAB_CONTENT))
}

impl Studio {
    pub(super) fn recipe(&mut self, ui: &mut egui::Ui) {
        self.with_recipe_draft(|studio| studio.recipe_controls(ui));
    }

    fn recipe_controls(&mut self, ui: &mut egui::Ui) {
        let recovery = self.generation.recipe_recovery.take();
        let jump_button = ui.horizontal(|ui| {
            let width = (ui.available_width() - workbench_style::SETTINGS_BUTTON_WIDTH
                - ui.spacing().item_spacing.x).max(1.);
            let jump = ui.add_sized([width, 30.], egui::Button::new(
            egui::Atoms::new(("Jump to", egui::Atom::grow(), egui::Atom::custom(
                ui.id().with("recipe-jump-chevron"), Vec2::splat(12.),
            ))),
            ).corner_radius(8).stroke(ui.visuals().window_stroke));
            if workbench_style::settings_button(ui, "Recipe settings")
                .on_hover_text("Manage the recipe by topic.").clicked()
            {
                self.recipe_settings_open = true;
            }
            jump
        }).inner;
        let center = jump_button.rect.right_center() - egui::vec2(13., 0.);
        ui.painter().add(egui::Shape::line(
            vec![center + egui::vec2(-3., -1.), center + egui::vec2(0., 2.),
                center + egui::vec2(3., -1.)],
            ui.style().interact(&jump_button).fg_stroke,
        ));
        let jump = egui::Popup::menu(&jump_button).width(jump_button.rect.width())
            .show(|ui| {
                let height = (ui.ctx().content_rect().height() - 48.).clamp(64., 320.);
                egui::ScrollArea::vertical().max_height(height).show(ui, |ui| {
                    let mut target = None;
                    for topic in RecipeTab::ALL.into_iter().skip(1) {
                        let name = topic.label();
                        if ui.button(name).clicked() {
                            target = Some(name);
                            ui.close();
                        }
                    }
                    target
                }).inner
            }).and_then(|popup| popup.inner);
        ui.add_space(7.);
        let scroll = egui::ScrollArea::vertical().animated(ui.style().animation_time > 0.);
        scroll.show(ui, |ui| {
            let jump_id = recipe_jump_id(ui);
            if let Some(name) = jump {
                ui.data_mut(|data| data.insert_temp(jump_id, name));
            }
            for topic in RecipeTab::ALL {
                self.recipe_topic(ui, topic, recovery);
                if topic == RecipeTab::Map {
                    ui.add_space(7.);
                }
            }
        });
    }

    pub(super) fn recipe_settings_window(&mut self, ctx: &egui::Context) {
        if !self.recipe_settings_open || self.settings_open || self.source.has_pending_source() {
            return;
        }
        use super::settings_dialog::{self, Icon};
        let icons = [
            Icon::Globe, Icon::Seed, Icon::Terrain, Icon::Biomes, Icon::Monument, Icon::Water,
            Icon::Road, Icon::Rail, Icon::Objects, Icon::Pin, Icon::Rules, Icon::Output,
        ];
        let pages = RecipeTab::ALL.into_iter().zip(icons)
            .map(|(tab, icon)| (tab, tab.label(), icon));
        let (tab, closed) = settings_dialog::show(
            ctx, "mapper-recipe-settings", "Recipe settings", pages, self.recipe_settings_tab,
            |ui, topic| {
                let info = egui::UiStackInfo::default().with_tag(RECIPE_TAB_CONTENT);
                ui.scope_builder(egui::UiBuilder::new().ui_stack_info(info), |ui| {
                    if topic == RecipeTab::Map {
                        ui.heading(topic.label());
                        ui.add_space(4.);
                    }
                    self.with_recipe_draft(|studio| studio.recipe_topic(ui, topic, None));
                });
            },
        );
        self.recipe_settings_tab = tab;
        self.recipe_settings_open = !closed;
        if closed {
            ctx.data_mut(|data| data.remove::<String>(egui::Id::new(RECIPE_TITLE_TEMPLATE_DRAFT)));
        }
    }

    fn recipe_topic(&mut self, ui: &mut egui::Ui, topic: RecipeTab, recovery: Option<diagnostic_ui::RecipeSection>) {
        use diagnostic_ui::RecipeSection;
        match topic {
            RecipeTab::Map => self.recipe_basics(ui),
            RecipeTab::Seeds => {
                recipe_disclosure(ui, topic.label(), "Link each stage to the master seed or enter a fixed seed.", |ui| {
                    let missing_seed = self.project.master_seed == 0;
                    stage_seeds(ui, &mut self.project, missing_seed);
                });
            }
            RecipeTab::Terrain | RecipeTab::Biomes => self.terrain_section(ui, topic),
            RecipeTab::Monuments => self.monument_recipe(ui),
            RecipeTab::Water => {
                let root = self.source.bound_root(&self.project);
                self.water.show(ui, &mut self.project, root);
            }
            RecipeTab::Roads => {
                let pins = self.recipe_asset_pins();
                self.roads.show(ui, &mut self.project, pins, recovery == Some(RecipeSection::Roads));
            }
            RecipeTab::Rails => self.rails.show(ui, &mut self.project, recovery == Some(RecipeSection::Rails)),
            RecipeTab::Scenery => {
                let pins = self.recipe_asset_pins();
                self.decoration_panel.show(ui, &mut self.project, pins);
            }
            RecipeTab::Landmarks | RecipeTab::Content | RecipeTab::Output => self.additional_recipe_options(ui, topic),
        }
    }

    fn recipe_asset_pins(&self) -> Option<mapper_studio::AssetIdentity> {
        let catalog = self.prefabs.catalog(&self.project)?;
        let bindings = self.prefabs.bindings(&self.project)?;
        Some(mapper_studio::AssetIdentity {
            catalog_sha256: catalog.sha256.clone(),
            bindings_sha256: bindings.sha256.clone(),
        })
    }

    fn monument_recipe(&mut self, ui: &mut egui::Ui) {
        let pins = self.recipe_asset_pins();
        let (monuments, changed) = recipe_section(
            ui, "Monuments", self.project.performance.monuments, true, false,
            "Generate monuments using the installation roster and recipe preferences.",
        );
        if let Some(enabled) = changed {
            self.project.performance.monuments = enabled;
        }
        monuments.body(|ui| {
            ui.add_enabled_ui(self.project.performance.monuments, |ui| {
                let options = &mut self.project.options;
                generation_rule(ui, "Relax tier rules", &mut options.relax_monument_rules,
                    "Allow land monuments with fewer corners in the requested tier.",
                    "Require the standard monument tier checks.",
                    "Use standard monument tier checks.");
                generation_rule(ui, "Center Outpost", &mut options.center_outpost,
                    "Require Outpost and favor the map center. An explicit Outpost pin takes precedence.",
                    "Place Outpost without a center preference.",
                    "Use the standard Outpost placement rules.");
            }).response.on_disabled_hover_text("Enable Monuments to use these placement settings.");
            if self.project.performance.monuments {
                if pins.is_some() {
                    self.monuments.workflow(ui, &mut self.project);
                }
                if self.project.composition_engine.is_some() {
                    ui.collapsing("Terrain composition", |ui| self.composition.show(ui, &self.project));
                }
            }
        });
    }

    fn map_size_magnet(&mut self, ui: &mut egui::Ui, label_y: f32) {
        let snap = self.preferences.map_size_snap;
        let (rect, response) = ui.allocate_exact_size(Vec2::new(18., 18.), Sense::click());
        let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
        let color = if snap.enabled() || response.hovered() || response.has_focus() {
            ui.visuals().text_color()
        } else {
            ui.visuals().weak_text_color()
        };
        let stroke = Stroke::new(1.05, color);
        let scale = 0.72;
        // Align the visible mark with the text baseline rather than the hit box.
        let center = Pos2::new(rect.center().x, label_y + 1. - 0.5 * scale);
        let magnet = [
            (-7., -6.), (-7., 1.), (-6., 4.), (-3.5, 6.), (0., 7.), (3.5, 6.), (6., 4.),
            (7., 1.), (7., -6.), (3., -6.), (3., 1.), (2., 2.5), (0., 3.), (-2., 2.5),
            (-3., 1.), (-3., -6.), (-7., -6.),
        ];
        ui.painter().add(egui::Shape::line(
            magnet.map(|(x, y)| center + egui::vec2(x, y) * scale).to_vec(), stroke,
        ));
        for [left, right] in [[-7., -3.], [3., 7.]] {
            ui.painter().hline(
                center.x + left * scale..=center.x + right * scale, center.y - 2. * scale, stroke,
            );
        }
        response.widget_info(|| egui::WidgetInfo::selected(
            egui::WidgetType::Button, response.enabled(), snap.enabled(), "Map size snapping",
        ));
        let mut next = if response.clicked() { snap.toggled() } else { snap };
        let popup_state = response.id.with("snap-distance-hover");
        let previous = ui.ctx().data(|data| data.get_temp::<egui::Rect>(popup_state));
        let over_popup = previous.is_some_and(|rect| {
            ui.ctx().pointer_hover_pos().is_some_and(|pos| rect.expand(2.).contains(pos))
        });
        if response.hovered() || response.gained_focus() || previous.is_some() {
            let mut distance = next.distance();
            let panel = egui::Popup::from_response(&response)
                .gap(1.)
                .close_behavior(egui::PopupCloseBehavior::IgnoreClicks)
                .show(|ui| {
                    egui::Grid::new("map-size-controls").show(ui, |ui| {
                        let size_label = ui.label("Map size");
                        let size = ui.add(Self::map_size_value(&mut self.project.size).suffix(" m"))
                            .labelled_by(size_label.id);
                        ui.end_row();

                        let snap_label = ui.label(if next.enabled() { "Snap distance" } else { "Snap distance (off)" });
                        let step = ui.add(egui::DragValue::new(&mut distance)
                            .range(1..=MapSizeSnap::MAX_DISTANCE)
                            .speed(1.)
                            .suffix(" m")
                            .update_while_editing(false))
                            .labelled_by(snap_label.id);
                        ui.end_row();
                        (size, step)
                    }).inner
                });
            if let Some(panel) = panel {
                let (size, step) = panel.inner;
                if step.changed() && distance != next.distance() {
                    next = next.with_distance(distance);
                }
                // Draw once after the pointer leaves so a click outside can commit a typed value.
                let keep_open = response.hovered() || response.gained_focus() || over_popup
                    || size.has_focus() || size.dragged() || step.has_focus() || step.dragged();
                ui.ctx().data_mut(|data| {
                    if keep_open {
                        data.insert_temp(popup_state, panel.response.rect);
                    } else {
                        data.remove::<egui::Rect>(popup_state);
                    }
                });
                if !keep_open {
                    ui.ctx().request_repaint();
                }
            }
        }
        if next != snap {
            self.preferences.map_size_snap = next;
            self.save_preferences();
        }
    }

    fn map_size_value(size: &mut u32) -> egui::DragValue<'_> {
        egui::DragValue::new(size).range(1000..=6000).speed(50.)
            .update_while_editing(false)
    }

    fn recipe_basics(&mut self, ui: &mut egui::Ui) {
        let old_size = self.project.size;
        let track_width = ui.available_width().max(1.);
        let size_label = ui.horizontal(|ui| {
            let start = ui.next_widget_position().x;
            let label = ui.label("Size (m)");
            self.map_size_magnet(ui, label.rect.center().y);
            let fraction = self.project.size.saturating_sub(1000) as f32 / 5000.;
            let value_x = start + (10. + (track_width - 20.) * fraction - 26.)
                .clamp(0., (track_width - 52.).max(0.));
            ui.add_space((value_x - ui.next_widget_position().x).max(0.));
            ui.add_sized([52., 24.], Self::map_size_value(&mut self.project.size))
                .labelled_by(label.id)
                .on_hover_text("Drag left or right to resize. Click to type an exact size.");
            label
        }).inner;
        let snap = self.preferences.map_size_snap;
        ui.horizontal(|ui| {
            ui.spacing_mut().slider_width = track_width;
            let selection = ui.visuals().selection.bg_fill;
            ui.visuals_mut().selection.bg_fill = ui.visuals().hyperlink_color;
            ui.add(egui::Slider::new(&mut self.project.size, 1000..=6000)
                .clamping(egui::SliderClamping::Edits).step_by(snap.step()).smart_aim(false).show_value(false)
                .trailing_fill(true).handle_shape(egui::style::HandleShape::Circle))
                .labelled_by(size_label.id);
            ui.visuals_mut().selection.bg_fill = selection;
        });
        if self.project.size != old_size {
            self.project.size = self.project.size.clamp(1000, 6000);
            let half = self.project.size as f32 / 2.;
            for target in &mut self.project.targets {
                target.x = target.x.clamp(-half, half);
                target.z = target.z.clamp(-half, half);
                target.radius = target.radius.min(self.project.size as f32);
                target.max_displacement_m = target.max_displacement_m.map(|limit| limit.min(self.project.size));
            }
        }
        ui.add_space(5.);
        const SEED_CONTROL_SIZE: f32 = 28.;
        const SEED_INSET: i8 = 7;
        let font = egui::TextStyle::Body.resolve(ui.style());
        let text_width = |text: String| ui.fonts_mut(|fonts| {
            fonts.layout_no_wrap(text, font.clone(), Color32::PLACEHOLDER).size().x
        });
        let inline_width = text_width("Master seed".into()) + text_width(MAX_SEED.to_string())
            + f32::from(SEED_INSET) * 2. + SEED_CONTROL_SIZE + ui.spacing().item_spacing.x * 2.;
        let label_above = (ui.available_width() < inline_width).then(|| ui.label("Master seed"));
        ui.horizontal(|ui| {
            ui.set_height(SEED_CONTROL_SIZE);
            let label = label_above.unwrap_or_else(|| ui.label("Master seed"));
            let field_width = (ui.available_width() - SEED_CONTROL_SIZE - ui.spacing().item_spacing.x).max(1.);
            let mut seed_text = if self.project.master_seed == 0 {
                String::new()
            } else {
                self.project.master_seed.to_string()
            };
            let accent = ui.visuals().hyperlink_color;
            let field = ui.scope(|ui| {
                let visuals = ui.visuals_mut();
                let border = visuals.window_stroke;
                visuals.selection.stroke = Stroke::new(1., accent);
                for widget in [&mut visuals.widgets.inactive, &mut visuals.widgets.hovered,
                    &mut visuals.widgets.active] {
                    widget.corner_radius = egui::CornerRadius::same(6);
                    widget.bg_stroke = border;
                }
                let id = if in_recipe_tab(ui) { "recipe-settings-master-seed" } else { "recipe-master-seed" };
                ui.add(egui::TextEdit::singleline(&mut seed_text)
                    .id(egui::Id::new(id))
                    .desired_width(field_width)
                    .min_size(Vec2::new(0., SEED_CONTROL_SIZE))
                    .margin(egui::Margin::symmetric(SEED_INSET, 3))
                    .font(egui::TextStyle::Body)
                    .horizontal_align(egui::Align::Center)
                    .vertical_align(egui::Align::Center)
                    .char_limit(10)).labelled_by(label.id)
            }).inner;
            if field.changed()
                && let Some(seed) = if seed_text.trim().is_empty() { Some(0) }
                    else { seed_text.trim().parse::<u32>().ok().filter(|seed| *seed <= MAX_SEED) }
            {
                self.project.master_seed = seed;
                self.project.refresh_inherited_seeds();
            }
            let (rect, response) = ui.allocate_exact_size(Vec2::splat(SEED_CONTROL_SIZE), Sense::click());
            let response = response.on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text("Randomize master seed");
            if response.clicked() {
                self.randomize_master_seed();
            }
            response.widget_info(|| egui::WidgetInfo::labeled(
                egui::WidgetType::Button, ui.is_enabled(), "Randomize master seed",
            ));
            let tint = if !response.enabled() {
                ui.visuals().weak_text_color()
            } else if response.hovered() || response.has_focus() {
                accent
            } else {
                ui.visuals().text_color()
            };
            settings_dialog::Icon::Refresh.paint(ui, rect.center(), 17., tint);
        });
        let missing = egui::Id::new("missing-master-seed");
        if self.project.master_seed != 0 {
            ui.data_mut(|data| data.remove::<bool>(missing));
        } else if ui.data(|data| data.get_temp::<bool>(missing)).unwrap_or(false) {
            ui.colored_label(ui.visuals().warn_fg_color, mapper_studio::MASTER_SEED_REQUIRED);
        }
    }

    fn additional_recipe_options(&mut self, ui: &mut egui::Ui, topic: RecipeTab) {
        use mapper_studio::recipe_options::Landmark;
        let (size, seed, stages) = (self.project.size, self.project.master_seed, self.project.seeds);
        let options = &mut self.project.options;
        if topic == RecipeTab::Landmarks {
            recipe_disclosure(ui, "Landmarks", "Set generation preferences for oasis, lake, and canyon variants.", |ui| {
                for family in Landmark::ALL {
                    generation_rule(ui, family.label(), options.landmark(family),
                        "Allow this landmark family and its variants to generate.",
                        "Skip every variant in this landmark family.",
                        "Use the normal landmark selection for this installation and terrain.");
                }
            });
        }
        if topic == RecipeTab::Content {
            recipe_disclosure(ui, "Content rules", "Control tunnels, roadside objects, building restrictions, and beach spawns.", |ui| {
                generation_rule(ui, "Rail tunnels", &mut options.underground_tunnels,
                    "Generate underground tunnels when the installation supports them.",
                    "Skip underground tunnels.",
                    "Generate tunnels on maps with compatible monument and tunnel data.");
                for (name, exclusion, yes, no, default) in [
                    ("Car wrecks", &mut options.block_car_wrecks,
                        "Allow car wreck placements.", "Exclude car wreck prefabs from the map.",
                        "Use standard car wreck placement."),
                    ("Road powerlines", &mut options.remove_road_powerlines,
                        "Allow roadside powerlines and substations.", "Exclude roadside powerlines and substations.",
                        "Use standard roadside powerline placement."),
                    ("Large powerlines", &mut options.remove_large_powerlines,
                        "Allow the large powerline network.", "Exclude the large powerline network.",
                        "Use standard large powerline placement."),
                ] {
                    if let Some(include) = generation_override(ui, name, exclusion.map(|value| !value), yes, no, default) {
                        *exclusion = include.map(|value| !value);
                    }
                }
                generation_rule(ui, "Build on roads", &mut options.build_on_roads,
                    "Remove Road topology on export. Rail and monument restrictions stay active. The companion plugin also handles road collider checks.",
                    "Keep Road topology and its construction restrictions.",
                    "Keep standard road construction restrictions.");
                generation_rule(ui, "Jungle spawns", &mut options.jungle_beach_spawns,
                    "Blend a spawn-compatible biome into eligible jungle beach cells.",
                    "Skip the jungle beach spawn adjustment.",
                    "Use standard beach spawn rules.");
                ui.collapsing("Prefab blocklist", |ui| {
                    let id = ui.id().with("prefab-block-entry");
                    let mut entry = ui.data(|data| data.get_temp::<String>(id)).unwrap_or_default();
                    let mut error = ui.data(|data| data.get_temp::<String>(id.with("error")))
                        .filter(|message| !message.is_empty());
                    let apply = ui.horizontal(|ui| {
                        let width = (ui.available_width() - 46.).max(32.);
                        let field = ui.add(egui::TextEdit::singleline(&mut entry)
                            .desired_width(width).hint_text("Prefab name or path"));
                        let enter = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                        ui.add_enabled(!entry.trim().is_empty(), egui::Button::new("Add")).clicked() || enter
                    }).inner;
                    if apply {
                        error = options.add_prefab(&entry).err();
                        if error.is_none() {
                            entry.clear();
                        }
                    }
                    if let Some(ref message) = error {
                        ui.colored_label(ui.visuals().error_fg_color, message);
                    }
                    ui.data_mut(|data| {
                        data.insert_temp(id, entry);
                        data.insert_temp(id.with("error"), error.unwrap_or_default());
                    });
                    let mut remove = None;
                    for (index, rule) in options.prefab_blocklist.iter().enumerate() {
                        ui.push_id(index, |ui| {
                            ui.horizontal(|ui| {
                                let width = (ui.available_width() - 58.).max(32.);
                                ui.add_sized([width, 22.], egui::Label::new(rule).truncate())
                                    .on_hover_text(rule);
                                if ui.small_button("Remove").clicked() {
                                    remove = Some(index);
                                }
                            });
                        });
                    }
                    if let Some(index) = remove {
                        options.prefab_blocklist.remove(index);
                    }
                });
            });
        }
        if topic == RecipeTab::Output {
            recipe_disclosure(ui, "Output", "Configure the exported map filename, cargo route, and webhook.", |ui| {
                generation_rule(ui, "Bake cargo route", &mut options.cargo_path,
                    "Save an offshore route for the Papyrus Cargo Route or CargoPath server plugin.",
                    "Export without a baked cargo route.",
                    "Let the server generate the cargo ship route.");
                filename_template_editor(ui, &mut options.filename_template);
                if in_recipe_tab(ui) {
                    let id = egui::Id::new(RECIPE_TITLE_TEMPLATE_DRAFT);
                    let mut use_title = options.filename_template.trim().is_empty();
                    if ui.checkbox(&mut use_title, "Use map title").changed() {
                        if use_title {
                            let previous = std::mem::take(&mut options.filename_template);
                            ui.data_mut(|data| data.insert_temp(id, previous));
                        } else {
                            options.filename_template = ui.data(|data| data.get_temp::<String>(id))
                                .unwrap_or_else(|| "{Type}{_}{Size}{_}{Seed}".into());
                        }
                    }
                }
                if options.filename_template.contains("{Custom}") {
                    let label = ui.label("Custom text");
                    ui.add(egui::TextEdit::singleline(&mut options.filename_text)
                        .desired_width(f32::INFINITY).char_limit(30)).labelled_by(label.id);
                }
                if !options.filename_template.trim().is_empty() {
                    let preview = format!("{}.map", options.filename(size, seed, &stages, "Map"));
                    ui.add(egui::Label::new(egui::RichText::new(&preview).weak()).truncate())
                        .on_hover_text(preview);
                }
                ui.checkbox(&mut options.webhook_enabled, "Webhook on export")
                    .on_hover_text("Send the filename, map size and seed to this HTTPS endpoint after an explicit .map export.");
                if options.webhook_enabled {
                    let label = ui.label("HTTPS endpoint");
                    ui.add(egui::TextEdit::singleline(&mut options.webhook_url)
                        .desired_width(f32::INFINITY).password(true).char_limit(512)
                        .hint_text("https://…"))
                        .labelled_by(label.id);
                }
            });
        }
    }

    fn terrain_section(&mut self, ui: &mut egui::Ui, topic: RecipeTab) {
        use mapper_studio::native_profile::{self, BiomeOverrides, TerrainOverrides};

        let project = &mut self.project;
        let profile = project.installation.as_ref()
            .filter(|_| mapper_studio::native_preview::is_native(&project.preview_engine))
            .and_then(|source| native_profile::for_source(source, project.native_profile.as_ref()).ok());
        let settings = profile.as_ref().map(|profile| &profile.settings);
        if topic == RecipeTab::Terrain {
            let supports_islands = project.supports_terrain_graph();
            let resources = self.preferences.generation_resources;
            self.erosion_comparison.sync(project.size, resources);
            if !project.supports_gpu_erosion() {
                self.erosion_comparison.cancel();
            }
            let mut open_performance = false;
            let (terrain, changed) = recipe_section_with_toggle(
                ui, "Terrain", "Custom tiers", project.terrain_overrides.is_some(),
                Some(settings.is_some() || project.terrain_overrides.is_some()), false,
                if settings.is_some() || project.terrain_overrides.is_some() {
                    "Customize tier percentages and rotation. Clear to use the terrain source’s defaults."
                } else {
                    "Tier overrides require terrain from a supported Rust installation."
                },
            );
            if let Some(enabled) = changed {
                project.terrain_overrides = settings.filter(|_| enabled).map(TerrainOverrides::from_settings);
            }
            terrain.body(|ui| {
                ui.add_enabled_ui(supports_islands, |ui| {
                    let mut custom = project.options.island_intensity.is_some();
                    if ui.checkbox(&mut custom, "Custom island intensity").changed() {
                        project.options.island_intensity = custom.then_some(3);
                    }
                    if let Some(intensity) = &mut project.options.island_intensity {
                        ui.add(egui::Slider::new(intensity, 0..=10).text("Intensity"))
                            .on_hover_text("Higher values let Mapper add more offshore islands. Zero preserves the source coastline.");
                    }
                }).response.on_disabled_hover_text("Requires the selected Rust installation’s terrain composition stages.");
                if let Some(overrides) = &mut project.terrain_overrides {
                    recipe_angle(ui, "Tier rotation", &mut overrides.tier_angle);
                    recipe_weights(ui, ["Tier 0", "Tier 1", "Tier 2"], &mut overrides.tier_weights);
                }
                open_performance = erosion_control(
                    ui, project, &mut self.erosion_device, resources,
                );
            });
            if open_performance {
                self.settings_page = settings::Page::Performance;
                self.settings_open = true;
            }
            return;
        }

        let (biomes, changed) = recipe_section_with_toggle(
            ui, "Biomes", "Custom mix", project.biome_overrides.is_some(),
            Some(settings.is_some() || project.biome_overrides.is_some()), false,
            if settings.is_some() || project.biome_overrides.is_some() {
                "Customize biome percentages and rotation. Clear to use the terrain source’s defaults."
            } else {
                "Biome overrides require terrain from a supported Rust installation."
            },
        );
        if let Some(enabled) = changed {
            project.biome_overrides = settings.filter(|_| enabled).map(BiomeOverrides::from_settings);
        }
        biomes.body(|ui| {
            if let Some(overrides) = &mut project.biome_overrides {
                recipe_angle(ui, "Biome rotation", &mut overrides.angle);
                recipe_weights(ui, ["Arid", "Temperate", "Tundra", "Arctic"], &mut overrides.weights);
                ui.horizontal(|ui| recipe_weight(ui, "Jungle", &mut overrides.jungle_weight));
            } else {
                ui.weak(if settings.is_some() {
                    "Rust defaults"
                } else {
                    "Requires a compatible Rust installation"
                });
            }
        });
    }

    pub(super) fn inspector(&mut self, ui: &mut egui::Ui) {
        let drafting = self.generation.busy();
        egui::ScrollArea::vertical().show(ui, |ui| {
            let native_roster = self.project.monument_rules.as_ref()
                .and_then(|rules| rules.stage_rules.as_ref())
                .is_some_and(|binding| binding.population.is_some());
            let size = self.project.size as f32;
            let half = size / 2.;
            let value_width = (ui.available_width() - 85.).max(60.);
            ui.spacing_mut().slider_width = (value_width - 65.).max(24.);
            let mut remove = false;
            if let Some(target) = self.selected.and_then(|index| self.project.targets.get_mut(index)) {
                let display = self.monument_icons.name(target.prefab_path.as_deref(), &target.name);
                ui.add(egui::Label::new(egui::RichText::new(&display).strong()).truncate())
                    .on_hover_text(format!(
                        "{display}\nInstance {}. Placement prefers this location within the terrain and spacing rules.",
                        target.instance_id,
                    ));
                if display != target.name {
                    ui.add(egui::Label::new(egui::RichText::new(&target.name).small().weak()).truncate())
                        .on_hover_text(&target.name);
                }
                egui::Grid::new(("monument-properties", target.instance_id))
                    .num_columns(2)
                    .min_col_width(0.)
                    .show(ui, |ui| {
                        let label = ui.label("Presence");
                        egui::ComboBox::from_id_salt("monument-presence")
                            .width(value_width)
                            .truncate()
                            .selected_text(format!("{:?}", target.presence))
                            .show_ui(ui, |ui| {
                                for presence in [Presence::Allowed, Presence::Required, Presence::Excluded] {
                                    ui.selectable_value(&mut target.presence, presence, format!("{presence:?}"));
                                }
                            }).response.labelled_by(label.id);
                        ui.end_row();
                        let label = ui.label("Preferred X");
                        ui.add_sized([value_width, ui.spacing().interact_size.y],
                            egui::DragValue::new(&mut target.x).range(-half..=half).suffix(" m")).labelled_by(label.id);
                        ui.end_row();
                        let label = ui.label("Preferred Z");
                        ui.add_sized([value_width, ui.spacing().interact_size.y],
                            egui::DragValue::new(&mut target.z).range(-half..=half).suffix(" m")).labelled_by(label.id);
                        ui.end_row();
                        let label = ui.label("Radius (m)")
                            .on_hover_text("Favor sites near the preferred location. Set Max move for a hard distance limit.");
                        ui.add(egui::Slider::new(&mut target.radius, 1. ..=size).logarithmic(true))
                            .labelled_by(label.id);
                        ui.end_row();
                        let label = ui.label("Max move").on_hover_text(
                            "Hard distance limit from the preferred location. Reports a placement issue if no valid site fits.",
                        );
                        ui.horizontal(|ui| {
                            let mut limited = target.max_displacement_m.is_some();
                            if ui.checkbox(&mut limited, "").labelled_by(label.id).changed() {
                                target.max_displacement_m = limited.then_some(
                                    mapper_studio::DEFAULT_MONUMENT_MOVE_LIMIT_M.min(size as u32),
                                );
                            }
                            if let Some(limit) = &mut target.max_displacement_m {
                                ui.add(egui::DragValue::new(limit).range(1..=size as u32).suffix(" m"))
                                    .labelled_by(label.id);
                            } else {
                                ui.weak("Off");
                            }
                        });
                        ui.end_row();
                        let label = ui.label("Weight").on_hover_text("Higher values favor this location.");
                        ui.add(egui::Slider::new(&mut target.strength, 0.1..=10.)).labelled_by(label.id);
                        ui.end_row();
                        let mut fixed = target.yaw.is_some();
                        let orientation = ui.checkbox(&mut fixed, "Rotation")
                            .on_hover_text("Set a fixed rotation. Clear to let placement choose the orientation.");
                        if orientation.changed() {
                            target.yaw = fixed.then_some(0.);
                            if fixed && !mapper_studio::placement_preview::is_earthwork(&self.project.placement_engine) {
                                self.project.placement_engine = mapper_studio::placement_preview::ROTATION_VERSION.into();
                            }
                        }
                        if let Some(yaw) = &mut target.yaw {
                            ui.add_sized([value_width, ui.spacing().interact_size.y],
                                egui::DragValue::new(yaw).range(0. ..=360.0_f32.next_down()).suffix("°"))
                                .labelled_by(orientation.id);
                        } else {
                            ui.weak("Automatic");
                        }
                        ui.end_row();
                    });
            } else if self.project.targets.is_empty() {
                ui.weak("Monuments load with the Rust installation.");
            } else {
                ui.weak("Select a monument to inspect it.");
            }
            if self.selected.is_some() {
                ui.separator();
                if drafting {
                    ui.weak("Next map")
                        .on_hover_text("These preferences apply when you queue a new map.");
                } else if let Some(layout) = self.current_layout() {
                    let selected_id = self.selected.and_then(|index| self.project.targets.get(index))
                        .map(|target| target.instance_id);
                    if let Some(outcome) = layout.solution.outcomes.iter().find(|outcome| Some(outcome.id) == selected_id) {
                        ui.add(egui::Label::new(&outcome.reason).truncate()).on_hover_text(&outcome.reason);
                        if let Some(distance) = outcome.distance {
                            ui.small(format!("{distance:.1} m from preference"));
                        }
                    }
                } else if !self.layouts.is_empty() {
                    ui.colored_label(ui.visuals().warn_fg_color, "Regeneration needed")
                        .on_hover_text("Preferences changed. Regenerate to update placements.");
                }
            }
            let ready = self.preview_current() && self.placement_task.is_none() && self.task.is_none();
            if !drafting && self.preview.is_some() && ui.add_enabled(ready, egui::Button::new("Regenerate monuments").wrap()).clicked() {
                self.start_placement(ui.ctx());
            }
            if !drafting && let Some(task) = &self.placement_task {
                ui.horizontal(|ui| { ui.spinner(); ui.label("Searching placements…"); });
                if ui.button("Cancel placement search").clicked() {
                    task.cancel.store(true, Ordering::Relaxed);
                }
            }
            if let Some(target) = self.selected.and_then(|index| self.project.targets.get_mut(index)) {
                ui.collapsing("Advanced constraints", |ui| {
                    ui.checkbox(&mut target.exact, "Require exact position")
                        .on_hover_text("Reject a site if it cannot satisfy this position. Leave off for weighted placement preferences.");
                });
                remove = ui.add_enabled(!native_roster || target.presence != Presence::Excluded,
                    egui::Button::new(if native_roster { "Exclude monument" } else { "Remove monument" })).clicked();
            }
            if remove && let Some(index) = self.selected.take() {
                if native_roster {
                    self.project.targets[index].presence = Presence::Excluded;
                } else {
                    self.project.targets.remove(index);
                }
            }
            if let Some(stamp) = self.selected.and_then(|index| self.project.targets.get(index))
                .and_then(|target| target.prefab_path.as_deref())
                .and_then(|path| self.prefabs.stamp(&self.project, path)) {
                ui.small(format!("Stamp {:.0} × {:.0} m", stamp.geometry.size[0], stamp.geometry.size[1]))
                    .on_hover_text("Full ground-paint bounds, including transparent margins. Height modifiers can have different bounds.");
            }
            if self.selected.is_some() && !drafting {
                let selected_path = self.selected.and_then(|index| self.project.targets.get(index))
                    .and_then(|target| target.prefab_path.clone());
                self.prefabs.details(ui, &mut self.project, selected_path.as_deref());
                ui.collapsing("Placement diagnostics", |ui| self.monuments.show(ui, &mut self.project));
                let metadata = self.prefabs.catalog(&self.project);
                let selected = self.selected.and_then(|index| self.project.targets.get(index));
                let placed = self.current_layout().and_then(|layout| layout.solution.outcomes.iter()
                    .find(|outcome| Some(outcome.id) == selected.map(|target| target.instance_id))
                    .and_then(|outcome| outcome.position.clone()));
                ui.collapsing("Terrain assessment", |ui| self.preparation.show(ui, &self.project,
                    self.preview.as_ref(), selected, placed.as_ref(),
                    self.task.is_none() && self.placement_task.is_none(), metadata.as_deref()));
            }
        });
    }

}

pub(super) struct RecipeHeader<'a> {
    ui: &'a mut egui::Ui,
    state: Option<egui::collapsing_header::CollapsingState>,
    response: egui::Response,
}

impl RecipeHeader<'_> {
    pub(super) fn set_open(&mut self, open: bool) {
        if let Some(state) = &mut self.state {
            state.set_open(open);
        }
    }

    pub(super) fn body<R>(mut self, draw: impl FnOnce(&mut egui::Ui) -> R)
        -> (egui::Response, Option<egui::InnerResponse<R>>) {
        let body = if let Some(state) = &mut self.state {
            state.show_body_indented(&self.response, self.ui, draw)
        } else {
            Some(self.ui.scope(draw))
        };
        (self.response, body)
    }
}

pub(super) fn recipe_section<'a>(
    ui: &'a mut egui::Ui,
    name: &str,
    enabled: bool,
    can_toggle: bool,
    open: bool,
    help: &str,
) -> (RecipeHeader<'a>, Option<bool>) {
    recipe_section_with_toggle(ui, name, "", enabled, Some(can_toggle), open, help)
}

fn recipe_section_with_toggle<'a>(
    ui: &'a mut egui::Ui,
    name: &str,
    toggle_label: &str,
    mut enabled: bool,
    can_toggle: Option<bool>,
    open: bool,
    help: &str,
) -> (RecipeHeader<'a>, Option<bool>) {
    if in_recipe_tab(ui) {
        let response = ui.horizontal(|ui| {
            ui.heading(name).on_hover_text(help);
            can_toggle.map(|can_toggle| {
                let label = if toggle_label.is_empty() { "Enabled" } else { toggle_label };
                ui.add_enabled(can_toggle, egui::Checkbox::new(&mut enabled, label))
                    .on_disabled_hover_text(help).changed()
            }).unwrap_or(false)
        });
        ui.add_space(8.);
        return (RecipeHeader { ui, state: None, response: response.response },
            response.inner.then_some(enabled));
    }
    let id = recipe_section_id(ui, name);
    let mut changed = false;
    let start = ui.next_widget_position();
    let right = ui.available_rect_before_wrap().right();
    let background = ui.painter().add(egui::Shape::Noop);
    let painter = ui.painter().clone();
    let reveal = recipe_jump_matches(ui, name);
    let jump_id = recipe_jump_id(ui);
    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, open);
    if reveal {
        state.set_open(true);
    }
    let scroll_to_header = reveal && state.openness(ui.ctx()) >= 1.;
    let expanded = state.is_open();
    let header = ui.horizontal(|ui| {
            let height = if ui.available_width() >= 216. { 30. } else { 28. };
            let switch_width = 32.;
            let toggle_width = if can_toggle.is_none() {
                0.
            } else if toggle_label.is_empty() {
                switch_width
            } else {
                let text_width = ui.painter().layout_no_wrap(
                    toggle_label.into(), egui::TextStyle::Body.resolve(ui.style()),
                    ui.visuals().text_color(),
                ).size().x;
                switch_width + ui.spacing().item_spacing.x + text_width.ceil() + 2.
            };
            let spacing = ui.spacing().item_spacing.x;
            let controls_width = height + spacing
                + can_toggle.map_or(0., |_| toggle_width + spacing);
            let label_width = (ui.available_width() - controls_width).max(0.);
            let label_size = egui::vec2(label_width, height);
            let label = ui.allocate_ui_with_layout(
                label_size, egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.set_min_size(label_size);
                    ui.add_space(22.);
                    let text = egui::RichText::new(name).strong()
                        .size(if ui.available_width() > 125. { 14. } else { 13. });
                    ui.add(egui::Label::new(text).truncate().selectable(false))
                },
            );
            let controls = egui::vec2(controls_width - spacing, height);
            ui.allocate_ui_with_layout(
                controls, egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.set_min_size(controls);
                    let button = ui.add_sized([height, height], egui::Button::new("?").frame(false));
                    button.widget_info(|| egui::WidgetInfo::labeled(
                        egui::WidgetType::Button, ui.is_enabled(), format!("{name} help"),
                    ));
                    let width = recipe_help_width(ui.ctx());
                    let popup = egui::Popup::menu(&button)
                        .align(egui::emath::RectAlign::BOTTOM_END)
                        .width(width)
                        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                        .show(|ui| {
                            ui.add(egui::Label::new(help).wrap());
                        });
                    if popup.is_none() {
                        recipe_help(button, false, help);
                    }
                    if let Some(can_toggle) = can_toggle {
                        let response = ui.add_enabled_ui(can_toggle, |ui| {
                            let (rect, mut response) = ui.allocate_exact_size(
                                egui::vec2(toggle_width, height), egui::Sense::click(),
                            );
                            if response.clicked() {
                                enabled = !enabled;
                                response.mark_changed();
                            }
                            response.widget_info(|| egui::WidgetInfo::selected(
                                egui::WidgetType::Checkbox, can_toggle, enabled,
                                if toggle_label.is_empty() { name } else { toggle_label },
                            ));
                            let center = rect.right_center() - egui::vec2(switch_width * 0.5, 0.);
                            if !toggle_label.is_empty() {
                                let track = Rect::from_center_size(center, egui::vec2(30., 16.));
                                let font = egui::TextStyle::Body.resolve(ui.style());
                                let galley = ui.painter().layout_no_wrap(toggle_label.into(), font, ui.visuals().text_color());
                                let right = track.left() - spacing;
                                let text_rect = Rect::from_min_size(
                                    egui::pos2(right - galley.size().x, track.center().y - galley.size().y * 0.5),
                                    galley.size(),
                                );
                                ui.painter().rect_filled(text_rect.expand2(egui::vec2(3., 1.)), 4,
                                    ui.visuals().widgets.hovered.bg_fill.gamma_multiply(0.72));
                                ui.painter().text(
                                    track.left_center() - egui::vec2(spacing, 0.),
                                    egui::Align2::RIGHT_CENTER, toggle_label,
                                    egui::TextStyle::Body.resolve(ui.style()), ui.visuals().text_color(),
                                );
                            }
                            workbench_style::paint_switch(ui, &response, center, enabled);
                            response
                        }).inner;
                        changed = recipe_help(response, false, help).changed();
                    }
                },
            );
            let disclosure = recipe_help(label.response.interact(egui::Sense::click()), false, help);
            if scroll_to_header {
                disclosure.scroll_to_me(Some(egui::Align::Min));
                ui.data_mut(|data| data.remove::<&'static str>(jump_id));
            }
            disclosure.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), name)
            });
            if disclosure.clicked() {
                state.toggle(ui);
            } else if changed && enabled {
                state.set_open(true);
            }
            let openness = state.openness(ui.ctx()) * std::f32::consts::FRAC_PI_2;
            let rotation = egui::emath::Rot2::from_angle(openness);
            let center = egui::pos2(start.x + 11., disclosure.rect.center().y);
            let points = [egui::vec2(-2., -4.), egui::vec2(2., 0.), egui::vec2(-2., 4.)]
                .map(|point| center + rotation * point);
            painter.add(egui::Shape::line(points.to_vec(),
                egui::Stroke::new(1.4, ui.style().interact(&disclosure).text_color())));
            let row = Rect::from_min_max(start, Pos2::new(right, disclosure.rect.bottom()));
            let corners = egui::CornerRadius::same(7);
            let fill = if expanded || disclosure.hovered() {
                ui.visuals().widgets.hovered.bg_fill
            } else {
                ui.visuals().faint_bg_color
            };
            let shadow = egui::epaint::Shadow {
                offset: [0, 2], blur: 4, spread: 0,
                color: Color32::from_black_alpha(if ui.visuals().dark_mode { 57 } else { 24 }),
            };
            painter.set(background, egui::Shape::Vec(vec![
                shadow.as_shape(row, corners).into(),
                egui::epaint::RectShape::new(row, corners, fill,
                    ui.visuals().window_stroke, egui::StrokeKind::Inside).into(),
            ]));
            disclosure
        }).inner;
    (RecipeHeader { ui, state: Some(state), response: header }, changed.then_some(enabled))
}

fn recipe_help_width(ctx: &egui::Context) -> f32 {
    (ctx.content_rect().width() - 20.).clamp(32., 260.)
}

fn recipe_help(response: egui::Response, is_default: bool, description: &str) -> egui::Response {
    let width = recipe_help_width(&response.ctx);
    let tooltip = if response.enabled() {
        egui::Tooltip::for_enabled(&response)
    } else {
        egui::Tooltip::for_disabled(&response)
    };
    tooltip.width(width).show(|ui| {
        ui.set_max_width(width);
        if is_default {
            ui.strong("Generator default");
        }
        ui.add(egui::Label::new(description).wrap());
    });
    response
}

const FILENAME_FIELDS: [(&str, &str); 8] = [
    ("Map type", "{Type}"),
    ("Map size", "{Size}"),
    ("Master seed", "{Seed}"),
    ("Short map ID", "{ShortId}"),
    ("Map ID", "{Id}"),
    ("Date (UTC)", "{Date}"),
    ("Underscore", "{_}"),
    ("Custom text", "{Custom}"),
];

#[derive(Clone, Copy)]
struct FilenameField(&'static str);

fn filename_template_editor(ui: &mut egui::Ui, template: &mut String) {
    const CHAR_LIMIT: usize = 100;
    const BYTE_LIMIT: usize = 160;

    let id = ui.make_persistent_id("recipe-filename-template");
    let selection_id = id.with("insertion-selection");
    let label = ui.label("Map filename");
    let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, _: f32| {
        let font = egui::TextStyle::Body.resolve(ui.style());
        let plain = egui::TextFormat::simple(font, ui.visuals().text_color());
        let token = egui::TextFormat {
            background: ui.visuals().selection.bg_fill,
            color: ui.visuals().selection.stroke.color,
            ..plain.clone()
        };
        let mut job = egui::text::LayoutJob::default();
        for part in text.as_str().split_inclusive('}') {
            if let Some(start) = part.rfind('{')
                && FILENAME_FIELDS.iter().any(|(_, value)| *value == &part[start..])
            {
                job.append(&part[..start], 0., plain.clone());
                job.append(&part[start..], 2., token.clone());
            } else {
                job.append(part, 0., plain.clone());
            }
        }
        ui.fonts_mut(|fonts| fonts.layout_job(job))
    };
    let mut field = egui::TextEdit::singleline(template)
        .id(id)
        .desired_width(f32::INFINITY)
        .char_limit(CHAR_LIMIT)
        .hint_text("Enter a filename template")
        .layouter(&mut layouter)
        .show(ui);
    let response = field.response.response.labelled_by(label.id);
    let chars = template.chars().count();
    // Clicking a palette button can take focus before the stored selection is read.
    let selection = if response.has_focus() {
        field.cursor_range.map(|range| range.as_sorted_char_range())
            .map(|range| range.start.0..range.end.0)
    } else {
        ui.data(|data| data.get_temp::<(String, std::ops::Range<usize>)>(selection_id))
            .filter(|(text, _)| text == template)
            .map(|(_, range)| range)
    }.unwrap_or(chars..chars);
    let bytes = |range: std::ops::Range<usize>| {
        let offset = |index| template.char_indices().nth(index).map_or(template.len(), |(start, _)| start);
        offset(range.start)..offset(range.end)
    };
    let fits = |token: &str, range: &std::ops::Range<usize>| {
        chars - range.len() + token.len() <= CHAR_LIMIT
            && template.len() - bytes(range.clone()).len() + token.len() <= BYTE_LIMIT
    };

    let drop = response.dnd_hover_payload::<FilenameField>().and_then(|field_token| {
        let pointer = ui.ctx().pointer_latest_pos()?;
        let index = field.galley.cursor_from_pos(pointer - field.galley_pos).index;
        let index = filename_drop_index(template, index.into());
        fits(field_token.0, &(index..index)).then_some(index)
    });
    if let Some(index) = drop {
        let cursor = field.galley.pos_from_cursor(egui::text::CCursor::new(index))
            .translate(field.galley_pos.to_vec2());
        let x = cursor.left().clamp(field.text_clip_rect.left(), field.text_clip_rect.right());
        let color = ui.visuals().hyperlink_color;
        ui.painter().rect_stroke(response.rect, 5, Stroke::new(1., color), egui::StrokeKind::Inside);
        ui.painter().vline(x, cursor.y_range(), Stroke::new(2., color));
    }
    let dropped = response.dnd_release_payload::<FilenameField>()
        .zip(drop)
        .map(|(token, index)| (token.0, index..index));

    ui.small("Drag or click a field to insert it.");
    let mut clicked = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6., 5.);
        for (description, token) in FILENAME_FIELDS {
            let button = ui.add_enabled(
                fits(token, &selection),
                egui::Button::new(token).sense(egui::Sense::click_and_drag()),
            )
                .on_hover_text(description)
                .on_disabled_hover_text("The filename template is full.");
            button.dnd_set_drag_payload(FilenameField(token));
            if button.clicked() {
                clicked = Some((token, selection.clone()));
            }
        }
    });

    if let Some((token, range)) = dropped.or(clicked) {
        let span = bytes(range.clone());
        template.replace_range(span, token);
        let caret = range.start + token.len();
        field.state.cursor.set_char_range(Some(egui::text::CCursorRange::one(
            egui::text::CCursor::new(caret),
        )));
        field.state.store(ui.ctx(), id);
        response.request_focus();
        ui.data_mut(|data| data.insert_temp(selection_id, (template.clone(), caret..caret)));
    } else if response.has_focus() {
        ui.data_mut(|data| data.insert_temp(selection_id, (template.clone(), selection)));
    }
}

/// Dropping inside an existing field inserts before or after the field.
fn filename_drop_index(template: &str, index: usize) -> usize {
    for (_, token) in FILENAME_FIELDS {
        for (offset, _) in template.match_indices(token) {
            let start = template[..offset].chars().count();
            let end = start + token.len();
            if start < index && index < end {
                return if index - start < end - index { start } else { end };
            }
        }
    }
    index.min(template.chars().count())
}

fn generation_rule(ui: &mut egui::Ui, name: &str, value: &mut Option<bool>, yes: &str, no: &str, default: &str) {
    if let Some(choice) = generation_override(ui, name, *value, yes, no, default) {
        *value = choice;
    }
}

/// Edit a generation override. An unset value inherits the generator default.
pub(super) fn generation_override(
    ui: &mut egui::Ui,
    name: &str,
    value: Option<bool>,
    yes_help: &str,
    no_help: &str,
    default_help: &str,
) -> Option<Option<bool>> {
    ui.push_id(name, |ui| {
        ui.horizontal(|ui| {
            let in_settings = in_recipe_tab(ui);
            let choice_width = if in_settings { 41. } else { 25. };
            let choice_gap = if in_settings { 1. } else { 2. };
            let default_gap = if in_settings { 7. } else { 0. };
            let height = ui.spacing().interact_size.y;
            let label_width = (ui.available_width()
                - ui.spacing().item_spacing.x - 3. * (choice_width + choice_gap) - default_gap)
                .max(0.);
            let label = ui.allocate_ui_with_layout(
                egui::vec2(label_width, height),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_min_width(label_width);
                    ui.add(egui::Label::new(name).truncate())
                },
            ).inner;
            let label = recipe_help(label, false, yes_help);
            let mut change = None;
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.x = choice_gap;
                ui.spacing_mut().button_padding.x = 3.;
                for (title, option, description) in [
                    ("Yes", Some(true), yes_help),
                    ("No", Some(false), no_help),
                    ("?", None, default_help),
                ] {
                    let mut button = egui::Button::selectable(value == option, title);
                    if in_settings {
                        let corners = match option {
                            Some(true) => egui::CornerRadius { nw: 7, sw: 7, ne: 0, se: 0 },
                            Some(false) => egui::CornerRadius { nw: 0, sw: 0, ne: 7, se: 7 },
                            None => egui::CornerRadius::same(7),
                        };
                        if option.is_none() { ui.add_space(default_gap); }
                        button = button.frame(true).frame_when_inactive(true).corner_radius(corners);
                    } else {
                        button = button.small();
                    }
                    let response = recipe_help(
                        ui.add_sized(
                            [choice_width, height], button,
                        ).labelled_by(label.id),
                        option.is_none(),
                        description,
                    );
                    if response.clicked() && value != option {
                        change = Some(option);
                    }
                }
            });
            change
        }).inner
    }).inner
}

fn recipe_section_id(ui: &egui::Ui, name: &str) -> egui::Id {
    ui.make_persistent_id(("recipe-section", name))
}

fn recipe_jump_id(ui: &egui::Ui) -> egui::Id {
    ui.id().with("recipe-jump-target")
}

fn recipe_jump_matches(ui: &egui::Ui, name: &str) -> bool {
    ui.data(|data| data.get_temp::<&'static str>(recipe_jump_id(ui))) == Some(name)
}

fn recipe_disclosure(ui: &mut egui::Ui, name: &str, help: &str, body: impl FnOnce(&mut egui::Ui)) {
    let (section, _) = recipe_section_with_toggle(ui, name, "", false, None, false, help);
    section.body(body);
}

#[cfg(test)]
pub(super) fn open_recipe_section(ui: &egui::Ui, name: &str) {
    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
        ui.ctx(), recipe_section_id(ui, name), true,
    );
    state.set_open(true);
    state.store(ui.ctx());
}

#[cfg(test)]
pub(super) fn recipe_checkbox_position(output: &egui::FullOutput, name: &str) -> Pos2 {
    let label = output.shapes.iter().find_map(|shape| match &shape.shape {
        egui::Shape::Text(text) if text.galley.job.text == name => {
            Some(Rect::from_min_size(text.pos, text.galley.size()))
        }
        _ => None,
    }).unwrap_or_else(|| panic!("Missing recipe category {name}"));
    let help_x = output.shapes.iter().filter_map(|shape| match &shape.shape {
        egui::Shape::Text(text) if text.galley.job.text == "?"
            && label.y_range().contains(text.pos.y + 0.5 * text.galley.size().y) =>
                Some(text.pos.x),
        _ => None,
    }).min_by(f32::total_cmp).unwrap_or(f32::INFINITY);
    output.shapes.iter().filter_map(|shape| match &shape.shape {
        egui::Shape::Rect(rect) if rect.rect.left() > label.right()
            && rect.rect.center().x < help_x
            && label.y_range().contains(rect.rect.center().y)
            && (rect.rect.width() - 30.).abs() < 0.5
            && (rect.rect.height() - 16.).abs() < 0.5 => Some(rect.rect.center()),
        _ => None,
    }).max_by(|a, b| a.x.total_cmp(&b.x))
        .unwrap_or_else(|| panic!("Missing recipe checkbox for {name}"))
}

fn erosion_control(
    ui: &mut egui::Ui,
    project: &mut Project,
    device: &mut ErosionDevice,
    resources: mapper_studio::generation_resources::Limits,
) -> bool {
    let supported = project.supports_gpu_erosion();
    if !supported && project.gpu_erosion.is_none() {
        return false;
    }
    if supported && matches!(device, ErosionDevice::Unchecked) {
        device.check(ui.ctx());
    }
    device.poll();

    let mut retry = false;
    let mut open_performance = false;
    ui.horizontal_wrapped(|ui| {
        let label = ui.label("Erosion");
        let response = egui::ComboBox::from_id_salt("recipe-erosion")
            .width(72.)
            .selected_text(if project.gpu_erosion.is_some() { "GPU" } else { "CPU" })
            .show_ui(ui, |ui| {
                if ui.selectable_label(project.gpu_erosion.is_none(), "CPU").clicked() {
                    project.gpu_erosion = None;
                }
                if !supported {
                    return;
                }
                match device {
                    ErosionDevice::Ready(local) => {
                        let bound = project.gpu_erosion.as_ref() == Some(local);
                        let label = if project.gpu_erosion.is_some() && !bound { "Use this GPU" } else { "GPU" };
                        if ui.selectable_label(bound, label)
                            .on_hover_text(format!("{} · {}", local.name, local.backend))
                            .clicked()
                        {
                            project.gpu_erosion = Some(local.clone());
                        }
                    }
                    ErosionDevice::Checking(_) | ErosionDevice::Unchecked => {
                        ui.weak("Checking GPU…");
                    }
                    ErosionDevice::Unavailable(error) => {
                        ui.weak("GPU unavailable").on_hover_text(error.as_str());
                        retry = ui.button("Check again").clicked();
                    }
                }
                ui.separator();
                if ui.link("Compare CPU and GPU…").clicked() {
                    open_performance = true;
                    ui.close();
                }
            }).response.labelled_by(label.id);
        if let Some(bound) = &project.gpu_erosion {
            response.on_hover_text(format!("{} · {}\nSaved with the recipe.", bound.name, bound.backend));
        }
    });
    if retry {
        device.check(ui.ctx());
    }
    let required = (|| {
        project.gpu_erosion.as_ref()?;
        let (resolution, _) = papyrus_terrain::native_layers::dimensions(project.size).ok()?;
        erosion_compute::required_memory_mib(resolution)
    })();
    if let Some(required) = required.filter(|required| *required > resources.gpu_memory_mib as u64) {
        open_performance |= ui.link(format!("GPU needs {required} MiB"))
            .on_hover_text("Open Performance settings to raise the GPU buffer limit.")
            .clicked();
    }
    open_performance
}

fn recipe_angle(ui: &mut egui::Ui, name: &str, angle: &mut u32) {
    ui.horizontal(|ui| {
        let label = ui.label(name);
        let mut degrees = f64::from(*angle) / 1000.;
        if ui.add(egui::DragValue::new(&mut degrees).range(0. ..=360.).suffix("°"))
            .labelled_by(label.id).changed()
        {
            *angle = (degrees * 1000.).round() as u32;
        }
    });
}

fn recipe_weight(ui: &mut egui::Ui, name: &str, weight: &mut u32) -> bool {
    let label = ui.label(name);
    let mut percent = f64::from(*weight) / 100.;
    let changed = ui.add(egui::DragValue::new(&mut percent).range(0. ..=100.).suffix("%"))
        .labelled_by(label.id).changed();
    if changed {
        *weight = (percent * 100.).round() as u32;
    }
    changed
}

fn recipe_weights<const N: usize>(ui: &mut egui::Ui, names: [&str; N], weights: &mut [u32; N]) {
    egui::Grid::new(names[0]).num_columns(2).show(ui, |ui| {
        for (index, name) in names.into_iter().enumerate() {
            let mut weight = weights[index];
            if recipe_weight(ui, name, &mut weight) {
                redistribute(weights, index, weight);
            }
            ui.end_row();
        }
    });
}

// Scale the other weights together and distribute rounding through the running total.
fn redistribute(weights: &mut [u32], index: usize, weight: u32) {
    let old_total = 10_000 - weights[index];
    let remaining = 10_000 - weight;
    let fallback = (index + 1) % weights.len();
    weights[index] = weight;
    let mut cumulative = 0;
    let mut assigned = 0;
    for (peer, other) in weights.iter_mut().enumerate().filter(|(peer, _)| *peer != index) {
        cumulative += *other;
        let share = if let Some(total) = (cumulative * remaining).checked_div(old_total) {
            let share = total - assigned;
            assigned = total;
            share
        } else if peer == fallback {
            remaining
        } else {
            0
        };
        *other = share;
    }
}

fn stage_seeds(ui: &mut egui::Ui, project: &mut Project, missing_master: bool) {
    egui::Grid::new("recipe-stage-seeds")
        .num_columns(3)
        .min_col_width(0.)
        .spacing(egui::vec2(4., 4.))
        .show(ui, |ui| {
            ui.weak("Stage");
            ui.weak("Seed");
            let fixed_label = ui.weak("Fixed")
                .on_hover_text("Use a seed independent of the master seed.");
            ui.end_row();
            for (seed, name) in project.seeds.iter_mut().zip(STAGES) {
                let label = ui.label(name);
                let field_size = [80., ui.spacing().interact_size.y];
                if missing_master && seed.inherited {
                    ui.add_sized(field_size, egui::Label::new("From master").truncate())
                        .on_hover_text("Enter a master seed to derive this stage’s seed.");
                } else {
                    ui.add_enabled_ui(!seed.inherited, |ui| {
                        ui.add_sized(field_size, egui::DragValue::new(&mut seed.value).range(1..=MAX_SEED))
                            .labelled_by(label.id);
                    });
                }
                let mut custom = !seed.inherited;
                if ui.checkbox(&mut custom, "")
                    .labelled_by(fixed_label.id)
                    .labelled_by(label.id)
                    .on_hover_text("Set this stage’s seed independently.")
                    .changed()
                {
                    seed.inherited = !custom;
                    if seed.inherited {
                        seed.value = mapper_studio::derive_seed(project.master_seed, name);
                    }
                }
                ui.end_row();
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipe_text(output: &egui::FullOutput, label: &str) -> Option<Pos2> {
        output.shapes.iter().rev().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == label => {
                let center = text.pos + text.galley.size() * 0.5;
                shape.clip_rect.contains(center).then_some(center)
            }
            _ => None,
        })
    }

    #[test]
    fn recipe_settings_tabs_edit_the_same_recipe_and_keep_app_preferences_separate() {
        mod snapshot { include!("../examples/support/egui_snapshot.rs"); }
        for (dark, size) in [(true, [1200, 896]), (false, [640, 600])] {
            let ctx = egui::Context::default();
            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
            workbench_style::install(&ctx);
            let mut app = super::super::tests::fixture(&ctx);
            app.recipe_settings_open = true;
            let recipe = app.project.clone();
            let preferences = app.preferences.clone();
            let mut textures = snapshot::Textures::default();
            let mut render = |app: &mut Studio, events| {
                let mut output = ctx.run_ui(egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(size[0] as f32, size[1] as f32))),
                    focused: true, events, ..Default::default()
                }, |_| app.recipe_settings_window(&ctx));
                textures.update(&output.textures_delta);
                output.textures_delta.clear();
                output
            };
            let mut click = |app: &mut Studio, pos| {
                for pressed in [true, false] {
                    render(app, vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
                        pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
                    }]);
                }
                render(app, vec![])
            };
            let mut frame = click(&mut app, Pos2::new(1., 1.));
            assert!(recipe_text(&frame, "Master seed").is_some(), "Map controls missing at {size:?}: {:?}",
                frame.shapes.iter().filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) => Some((&text.galley.job.text, text.pos, shape.clip_rect)),
                    _ => None,
                }).collect::<Vec<_>>());
            for (tab, control) in [
                (RecipeTab::Seeds, "Fixed"), (RecipeTab::Terrain, "Custom island intensity"),
                (RecipeTab::Biomes, "Custom mix"), (RecipeTab::Monuments, "Center Outpost"),
                (RecipeTab::Water, "Enabled"), (RecipeTab::Roads, "Roads require native terrain"),
                (RecipeTab::Rails, "Rails require native terrain"),
                (RecipeTab::Scenery, "Scenery requires native terrain"),
                (RecipeTab::Landmarks, "Oasis"), (RecipeTab::Content, "Prefab blocklist"),
                (RecipeTab::Output, "Map filename"),
            ] {
                let position = frame.shapes.iter().filter_map(|shape| {
                    if let egui::Shape::Text(text) = &shape.shape
                        && text.galley.job.text == tab.label()
                    {
                        let center = text.pos + text.galley.size() * 0.5;
                        return shape.clip_rect.contains(center).then_some(center);
                    }
                    None
                }).min_by(|a, b| a.x.total_cmp(&b.x)).unwrap_or_else(|| panic!("Missing tab {tab:?}"));
                frame = click(&mut app, position);
                assert_eq!(app.recipe_settings_tab, tab);
                assert!(recipe_text(&frame, control).is_some(), "Missing {control} on {tab:?}");
                assert_eq!(app.project, recipe);
            }
            frame = click(&mut app, recipe_text(&frame, "Webhook on export").unwrap());
            assert!(app.project.options.webhook_enabled);
            assert!(recipe_text(&frame, "HTTPS endpoint").is_some());
            frame = click(&mut app, recipe_text(&frame, "Use map title").unwrap());
            assert_eq!(app.project.options.filename_template, "{Type}{_}{Size}{_}{Seed}");
            for (_, token) in FILENAME_FIELDS {
                assert!(recipe_text(&frame, token).is_some(), "Missing visible filename field {token}");
            }
            frame = click(&mut app, recipe_text(&frame, "{ShortId}").unwrap());
            let template = app.project.options.filename_template.clone();
            assert!(template.ends_with("{ShortId}"));
            frame = click(&mut app, recipe_text(&frame, "Use map title").unwrap());
            assert!(app.project.options.filename_template.is_empty());
            frame = click(&mut app, recipe_text(&frame, "Use map title").unwrap());
            assert_eq!(app.project.options.filename_template, template);
            assert_eq!(app.preferences, preferences);
            assert!(!app.settings_open);
            click(&mut app, recipe_text(&frame, "Done").unwrap());
            assert!(!app.recipe_settings_open);
            assert!(ctx.data(|data| data.get_temp::<String>(egui::Id::new(RECIPE_TITLE_TEMPLATE_DRAFT))).is_none());
            if let Some(directory) = std::env::var_os("MAPPER_RECIPE_SETTINGS_SNAPSHOTS") {
                let image = textures.raster(&ctx, frame, size);
                let name = if dark { "recipe-settings-dark.png" } else { "recipe-settings-light.png" };
                mapper_studio::image_export::write_png(&PathBuf::from(directory).join(name), &image, || false).unwrap();
            }
        }
    }

    #[test]
    fn master_seed_stays_centered_and_readable_in_narrow_recipes() {
        mod snapshot { include!("../examples/support/egui_snapshot.rs"); }
        for dark in [true, false] {
            for width in [168, 218] {
                let ctx = egui::Context::default();
                ctx.set_theme(if dark { egui::Theme::Dark } else { egui::Theme::Light });
                workbench_style::install(&ctx);
                let mut app = super::super::tests::fixture(&ctx);
                app.project.master_seed = MAX_SEED;
                let dimensions = [width, 160];
                let textures = std::cell::RefCell::new(snapshot::Textures::default());
                let render = |app: &mut Studio, events| {
                    let mut output = ctx.run_ui(egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO,
                            Vec2::new(dimensions[0] as f32, dimensions[1] as f32))),
                        focused: true, events, ..Default::default()
                    }, |ui| app.recipe_basics(ui));
                    textures.borrow_mut().update(&output.textures_delta);
                    output.textures_delta.clear();
                    output
                };
                render(&mut app, vec![]);
                let output = render(&mut app, vec![]);
                let field = ctx.read_response(egui::Id::new("recipe-master-seed")).unwrap().rect;
                let value = MAX_SEED.to_string();
                let text = output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.job.text == value => {
                        let bounds = text.galley.rect.translate(text.pos.to_vec2());
                        assert!(shape.clip_rect.contains_rect(bounds), "Seed digits were clipped at {width}");
                        Some(bounds)
                    }
                    _ => None,
                }).unwrap();
                let label = recipe_text(&output, "Master seed").unwrap();
                let randomize = output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Mesh(mesh) if mesh.calc_bounds().size() == Vec2::splat(17.) => {
                        Some(mesh.calc_bounds().center())
                    }
                    _ => None,
                }).unwrap();
                assert!(text.center().distance(field.center()) < 2., "Seed text sits off center");
                assert!((randomize.y - field.center().y).abs() < 1., "Randomize is off the input row");
                if width == 218 {
                    assert!((label.y - field.center().y).abs() < 2., "Label is off the input row");
                } else {
                    assert!(label.y < field.top(), "The narrow layout needs room for all ten digits");
                }

                if let Some(directory) = std::env::var_os("MAPPER_SEED_FIELD_SNAPSHOTS") {
                    let image = textures.borrow().raster(&ctx, output, dimensions);
                    let theme = if dark { "dark" } else { "light" };
                    let name = format!("seed-{theme}-{width}.png");
                    mapper_studio::image_export::write_png(&PathBuf::from(directory).join(name), &image, || false).unwrap();
                }
                for pressed in [true, false] {
                    render(&mut app, vec![egui::Event::PointerMoved(randomize), egui::Event::PointerButton {
                        pos: randomize, pressed, button: egui::PointerButton::Primary,
                        modifiers: egui::Modifiers::NONE,
                    }]);
                }
                assert_ne!(app.project.master_seed, MAX_SEED);
            }
        }
    }

    #[test]
    fn map_size_snap_hover_edits_both_size_and_distance() {
        let ctx = egui::Context::default();
        workbench_style::install(&ctx);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut app = super::super::tests::fixture(&ctx);
        app.settings_path = Some(path.clone());
        app.project.size = 4003;
        app.preferences.map_size_snap = MapSizeSnap::Meters50;
        let render = |app: &mut Studio, events| {
            let mut output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(218., 360.))),
                focused: true, events, ..Default::default()
            }, |ui| app.recipe_basics(ui));
            output.textures_delta.clear();
            output
        };
        let button = |pos, pressed| egui::Event::PointerButton {
            pos, pressed, button: egui::PointerButton::Primary, modifiers: egui::Modifiers::NONE,
        };
        let click = |app: &mut Studio, point| {
            render(app, vec![egui::Event::PointerMoved(point), button(point, true)]);
            render(app, vec![button(point, false)]);
        };
        let initial = render(&mut app, vec![]);
        let magnet = initial.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Path(path) if path.points.len() == 17 => {
                Some(path.points[4] - egui::vec2(0., 6.5 * 0.72))
            }
            _ => None,
        }).unwrap();
        let label = recipe_text(&initial, "Size (m)").unwrap();
        assert!((magnet.y - label.y - 1.).abs() < 1.);
        assert!((10. ..55.).contains(&(magnet.x - label.x)));
        assert!(recipe_text(&initial, "Snap distance").is_none());

        let field = |app: &mut Studio, value: u32| {
            render(app, vec![egui::Event::PointerMoved(magnet)]);
            let open = render(app, vec![]);
            assert!(recipe_text(&open, "Snap distance").is_some());
            recipe_text(&open, &value.to_string()).unwrap_or_else(|| panic!("Missing snap distance {value} m"))
        };
        let typed = |app: &mut Studio, current, updated: &str| {
            let point = field(app, current);
            click(app, point);
            render(app, vec![]);
            render(app, vec![egui::Event::Text(updated.to_owned())]);
        };
        typed(&mut app, 50, "37");
        assert_eq!(app.preferences.map_size_snap.distance(), 50);
        render(&mut app, vec![egui::Event::Key {
            key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert_eq!(app.preferences.map_size_snap, MapSizeSnap::Custom(37));
        typed(&mut app, 37, "65");
        click(&mut app, Pos2::new(180., 110.));
        assert_eq!(app.preferences.map_size_snap, MapSizeSnap::Custom(65));

        let from = field(&mut app, 65);
        let to = from + egui::vec2(20., 0.);
        render(&mut app, vec![egui::Event::PointerMoved(from), button(from, true)]);
        render(&mut app, vec![egui::Event::PointerMoved(to)]);
        render(&mut app, vec![button(to, false)]);
        let distance = app.preferences.map_size_snap.distance();
        assert!(distance > 65, "Dragging to the right should increase the distance");
        assert_eq!(app.project.size, 4003, "The snap preference must preserve the current recipe");
        assert_eq!(mapper_studio::preferences::Preferences::load(&path).unwrap().map_size_snap,
            MapSizeSnap::Custom(distance));

        let probe = |x| Pos2::new(x, magnet.y + 24.);
        click(&mut app, probe(40.));
        assert_ne!(app.project.size, 4003);
        assert_eq!((app.project.size - 1000) % distance, 0);
        let snapped = app.project.size;
        click(&mut app, magnet);
        assert_eq!(app.project.size, snapped);
        assert_eq!(mapper_studio::preferences::Preferences::load(&path).unwrap().map_size_snap,
            MapSizeSnap::Disabled(distance));
        assert!([35., 40., 45.].into_iter().any(|x| {
            click(&mut app, probe(x));
            (app.project.size - 1000) % distance != 0
        }), "With snapping off the slider must reach values between the snap points");
        click(&mut app, magnet);
        assert_eq!(app.preferences.map_size_snap, MapSizeSnap::Custom(distance));

        let size_field = |app: &mut Studio| {
            let value = app.project.size.to_string();
            render(app, vec![egui::Event::PointerMoved(magnet)]);
            let open = render(app, vec![]);
            let caption = recipe_text(&open, "Map size").unwrap();
            let field = recipe_text(&open, &value).unwrap();
            assert!((caption.y - field.y).abs() < 2.);
            field
        };
        let point = size_field(&mut app);
        let dragged = point + egui::vec2(8., 0.);
        let before_drag = app.project.size;
        render(&mut app, vec![egui::Event::PointerMoved(point), button(point, true)]);
        render(&mut app, vec![egui::Event::PointerMoved(dragged)]);
        render(&mut app, vec![button(dragged, false)]);
        assert!(app.project.size > before_drag, "Dragging the size should resize the map");

        let point = size_field(&mut app);
        click(&mut app, point);
        render(&mut app, vec![]);
        render(&mut app, vec![egui::Event::Text("4123".into())]);
        click(&mut app, Pos2::new(180., 110.));
        assert_eq!(app.project.size, 4123, "Clicking away should apply the exact typed size");
        assert_eq!(app.preferences.map_size_snap, MapSizeSnap::Custom(distance));
    }

    fn rule_choice(output: &egui::FullOutput, label: &str, choice: &str) -> Pos2 {
        let row = recipe_text(output, label).unwrap();
        output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == choice => {
                let center = text.pos + text.galley.size() * 0.5;
                ((center.y - row.y).abs() < 2. && shape.clip_rect.contains(center)).then_some(center)
            }
            _ => None,
        }).unwrap_or_else(|| panic!("Missing {choice} in recipe row {label}"))
    }

    #[test]
    fn recipe_jump_opens_and_scrolls_to_sections_without_editing_the_recipe() {
        mod snapshot { include!("../examples/support/egui_snapshot.rs"); }
        let ctx = egui::Context::default();
        workbench_style::install(&ctx);
        ctx.all_styles_mut(|style| style.animation_time = 0.);
        let mut app = super::super::tests::fixture(&ctx);
        app.project.size = 4003;
        let original = app.project.clone();
        let size = [210, 340];
        let mut textures = snapshot::Textures::default();
        let mut render = |app: &mut Studio, events| {
            let mut output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(size[0] as f32, size[1] as f32))),
                focused: true,
                events,
                ..Default::default()
            }, |ui| app.recipe(ui));
            textures.update(&output.textures_delta);
            output.textures_delta.clear();
            output
        };
        let click = |point| [true, false].map(|pressed| vec![
            egui::Event::PointerMoved(point),
            egui::Event::PointerButton { pos: point, button: egui::PointerButton::Primary,
                pressed, modifiers: egui::Modifiers::NONE },
        ]);
        render(&mut app, vec![]);
        let start = render(&mut app, vec![]);
        assert!(recipe_text(&start, "Map filename").is_none());
        let heading = recipe_text(&start, "Stage seeds").unwrap();
        assert!(start.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Path(path) if path.points.len() == 3
                && path.points[1].x > path.points[0].x
                && (path.points[1].y - heading.y).abs() < 1.
                && path.points[1].x < heading.x
        )), "The collapsed chevron should point right and align with its label");
        let mut output_snapshot = None;
        let mut menu_snapshot = None;
        for (section, field) in [("Output", "Map filename"), ("Roads & trails", "Roads require native terrain")] {
            let output = render(&mut app, vec![]);
            for events in click(recipe_text(&output, "Jump to").unwrap()) {
                render(&mut app, events);
            }
            let menu = render(&mut app, vec![]);
            for events in click(recipe_text(&menu, section).unwrap()) {
                render(&mut app, events);
            }
            let settled = render(&mut app, vec![egui::Event::PointerGone]);
            assert!(recipe_text(&settled, "Jump to").is_some());
            assert!(recipe_text(&settled, section).is_some());
            assert!(recipe_text(&settled, field).is_some(), "Missing {field} after opening {section}");
            if section == "Output" {
                output_snapshot = Some(settled);
                menu_snapshot = Some(menu);
            }
        }
        assert_eq!(app.project, original);
        let gear = start.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Circle(circle) if circle.radius == 2.25 => Some(circle.center),
            _ => None,
        }).expect("Recipe settings gear");
        for events in click(gear) {
            render(&mut app, events);
        }
        assert!(app.recipe_settings_open);
        assert!(!app.settings_open);
        if let Some(directory) = std::env::var_os("MAPPER_RECIPE_JUMP_SNAPSHOTS") {
            for (name, frame) in [("recipe-jump-menu.png", menu_snapshot), ("recipe-jump-output.png", output_snapshot)] {
                let image = textures.raster(&ctx, frame.unwrap(), size);
                let path = PathBuf::from(&directory).join(name);
                mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
            }
        }
    }

    #[test]
    fn recipe_percentages_stay_normalized_when_one_weight_changes() {
        let mut tiers = [3000, 3000, 4000];
        redistribute(&mut tiers, 1, 6500);
        assert_eq!(tiers, [1500, 6500, 2000]);
        redistribute(&mut tiers, 2, 10_000);
        assert_eq!(tiers, [0, 0, 10_000]);
        redistribute(&mut tiers, 2, 3550);
        assert_eq!(tiers, [6450, 0, 3550]);
    }

    #[test]
    fn recipe_monument_switch_preserves_placement_preferences_and_supports_undo() {
        let ctx = egui::Context::default();
        workbench_style::install(&ctx);
        ctx.all_styles_mut(|style| style.animation_time = 0.);
        let mut app = super::super::tests::fixture(&ctx);
        let original = app.project.clone();
        let render = |app: &mut Studio, events| {
            let before = app.history.snapshot(&app.project);
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(280., 580.))),
                    focused: true,
                    events,
                    ..Default::default()
                },
                |ui| app.recipe(ui),
            );
            app.history.finish_snapshot(before, &app.project, true);
            output.textures_delta.clear();
            output
        };
        let text_position = |frame: &egui::FullOutput, name: &str| {
            frame.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == name => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            })
        };
        let click = |app: &mut Studio, position: Pos2| {
            for pressed in [true, false] {
                let events = vec![egui::Event::PointerMoved(position), egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }];
                let _ = render(app, events);
            }
        };
        let frame = render(&mut app, vec![]);
        assert!(text_position(&frame, "Center Outpost").is_none());
        click(&mut app, text_position(&frame, "Monuments").unwrap());
        let frame = render(&mut app, vec![]);
        assert!(text_position(&frame, "Relax tier rules").is_some());
        let outpost = rule_choice(&frame, "Center Outpost", "Yes");
        click(&mut app, outpost);
        assert_eq!(app.project.options.center_outpost, Some(true));
        click(&mut app, recipe_checkbox_position(&frame, "Monuments"));
        assert!(!app.project.performance.monuments);
        let frame = render(&mut app, vec![]);
        click(&mut app, rule_choice(&frame, "Center Outpost", "No"));
        assert_eq!(app.project.options.center_outpost, Some(true));
        assert_eq!(app.project.targets, original.targets);
        let mut with_outpost = original.clone();
        with_outpost.options.center_outpost = Some(true);
        assert_eq!(app.project.terrain_key(), with_outpost.terrain_key());
        app.history.undo(&mut app.project);
        assert_eq!(app.project, with_outpost);
        app.history.undo(&mut app.project);
        assert_eq!(app.project, original);
    }

    #[test]
    fn landmark_rule_fits_narrow_docks_and_restores_automatic_placement() {
        mod snapshot { include!("../examples/support/egui_snapshot.rs"); }
        const OASIS: &str = "assets/bundled/prefabs/autospawn/unique_environment/oasis/oasis_a.prefab";
        for dark in [false, true] {
            let ctx = egui::Context::default();
            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
            workbench_style::install(&ctx);
            ctx.all_styles_mut(|style| {
                style.animation_time = 0.;
                style.interaction.tooltip_delay = 0.;
                style.interaction.show_tooltips_only_when_still = false;
            });
            let mut app = super::super::tests::fixture(&ctx);
            let screen = Rect::from_min_size(Pos2::ZERO, egui::vec2(210., 460.));
            let mut textures = snapshot::Textures::default();
            let mut render = |app: &mut Studio, events| {
                let mut output = ctx.run_ui(
                    egui::RawInput { screen_rect: Some(screen), events, focused: true, ..Default::default() },
                    |ui| {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            for topic in [RecipeTab::Landmarks, RecipeTab::Content, RecipeTab::Output] {
                                app.additional_recipe_options(ui, topic);
                            }
                        });
                    },
                );
                textures.update(&output.textures_delta);
                output.textures_delta.clear();
                output
            };
            let click = |app: &mut Studio, render: &mut dyn FnMut(&mut Studio, Vec<egui::Event>) -> egui::FullOutput, pos| {
                for pressed in [true, false] {
                    render(app, vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
                        pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
                    }]);
                }
            };
            for name in ["Landmarks", "Content rules", "Output"] {
                let output = render(&mut app, vec![]);
                click(&mut app, &mut render, recipe_text(&output, name).unwrap());
            }
            for (caption, expected) in [("Yes", Some(true)), ("No", Some(false)), ("?", None)] {
                let output = render(&mut app, vec![]);
                click(&mut app, &mut render, rule_choice(&output, "Oasis", caption));
                assert_eq!(app.project.options.oasis, expected);
                assert_eq!(app.project.options.landmark_blocker(OASIS).is_some(), expected == Some(false));
            }
            for (caption, excluded) in [("No", Some(true)), ("Yes", Some(false)), ("?", None)] {
                for name in ["Car wrecks", "Road powerlines", "Large powerlines"] {
                    let output = render(&mut app, vec![]);
                    click(&mut app, &mut render, rule_choice(&output, name, caption));
                }
                let options = &app.project.options;
                assert_eq!(options.block_car_wrecks, excluded);
                assert_eq!(options.remove_road_powerlines, excluded);
                assert_eq!(options.remove_large_powerlines, excluded);
            }
            let output = render(&mut app, vec![]);
            for name in ["Rail tunnels", "Road powerlines", "Large powerlines", "Jungle spawns", "Bake cargo route"] {
                assert!(recipe_text(&output, name).is_some(), "Missing rule {name}");
            }
            for shape in &output.shapes {
                if let egui::Shape::Text(text) = &shape.shape {
                    let rect = text.galley.rect.translate(text.pos.to_vec2());
                    // Expanded recipe sections can scroll vertically in the dock.
                    assert!(screen.x_range().contains(rect.left()) && screen.x_range().contains(rect.right()),
                        "Recipe rule too wide: {:?}", text.galley.job.text);
                }
            }
            let options = app.project.options.clone();
            // Egui suppresses tooltips over pointers moved immediately after a click.
            for _ in 0..8 {
                render(&mut app, vec![]);
            }
            for (row, choice, description) in [
                ("Content rules", "?", "Control tunnels, roadside objects, building restrictions, and beach spawns."),
                ("Car wrecks", "No", "Exclude car wreck prefabs from the map."),
                ("Car wrecks", "?", "Use standard car wreck placement."),
            ] {
                let point = rule_choice(&output, row, choice);
                render(&mut app, vec![egui::Event::PointerGone]);
                render(&mut app, vec![egui::Event::PointerMoved(point)]);
                // Give the tooltip area time to size and then anchor its wrapped text.
                let mut hover = render(&mut app, vec![]);
                for _ in 0..2 {
                    hover = render(&mut app, vec![]);
                }
                let bounds = hover.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.job.text == description => {
                        Some(text.galley.rect.translate(text.pos.to_vec2()))
                    }
                    _ => None,
                }).unwrap_or_else(|| panic!("Missing hover help for {row} {choice}"));
                assert!(screen.contains_rect(bounds), "Recipe help clipped for {row} {choice}: {bounds:?}");
                if row == "Car wrecks" && choice == "?" {
                    assert!(recipe_text(&hover, "Generator default").is_some());
                }
            }
            let help = rule_choice(&output, "Content rules", "?");
            click(&mut app, &mut render, help);
            render(&mut app, vec![egui::Event::PointerGone]);
            let description = "Control tunnels, roadside objects, building restrictions, and beach spawns.";
            let pinned = render(&mut app, vec![]);
            assert!(recipe_text(&pinned, description).is_some());
            render(&mut app, vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }]);
            assert!(recipe_text(&render(&mut app, vec![]), description).is_none());
            assert!(recipe_text(&render(&mut app, vec![]), "Car wrecks").is_some());
            assert_eq!(app.project.options, options);
            if let Some(directory) = std::env::var_os("MAPPER_RECIPE_RULE_SNAPSHOTS") {
                let image = textures.raster(&ctx, output, [210, 460]);
                let path = PathBuf::from(directory).join(if dark { "rules-dark.png" } else { "rules-light.png" });
                mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
            }
        }
    }

    #[test]
    fn filename_fields_support_clicking_dragging_and_typing() {
        mod snapshot { include!("../examples/support/egui_snapshot.rs"); }
        let ctx = egui::Context::default();
        workbench_style::install(&ctx);
        ctx.all_styles_mut(|style| style.animation_time = 0.);
        let mut app = super::super::tests::fixture(&ctx);
        app.project.options.filename_template = "Café_{Size}".into();
        let size = [230, 330];
        let mut textures = snapshot::Textures::default();
        let mut render = |app: &mut Studio, events| {
            let mut output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(size[0] as f32, size[1] as f32))),
                focused: true, events, ..Default::default()
            }, |ui| {
                open_recipe_section(ui, "Output");
                for topic in [RecipeTab::Landmarks, RecipeTab::Content, RecipeTab::Output] {
                    app.additional_recipe_options(ui, topic);
                }
            });
            textures.update(&output.textures_delta);
            output.textures_delta.clear();
            output
        };
        let click = |pos| [true, false].map(|pressed| vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton { pos, button: egui::PointerButton::Primary,
                pressed, modifiers: egui::Modifiers::NONE },
        ]);
        render(&mut app, vec![]);
        let initial = render(&mut app, vec![]);
        let field = recipe_text(&initial, "Café_{Size}").unwrap();
        for event in click(field) { render(&mut app, event); }
        let id = ctx.memory(|memory| memory.focused()).expect("Filename field focused");
        let mut state = egui::TextEdit::load_state(&ctx, id).unwrap();
        state.cursor.set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(0), egui::text::CCursor::new(4),
        )));
        state.store(&ctx, id);
        render(&mut app, vec![]);
        let palette = render(&mut app, vec![]);
        let seed = recipe_text(&palette, "{Seed}").unwrap();
        for event in click(seed) { render(&mut app, event); }
        assert_eq!(app.project.options.filename_template, "{Seed}_{Size}");
        let output = render(&mut app, vec![]);
        let filename = format!("{}_{}.map", app.project.master_seed, app.project.size);
        assert!(recipe_text(&output, &filename).is_some());

        app.project.options.filename_template = "Café{Size}Tail".into();
        let output = render(&mut app, vec![]);
        let source = recipe_text(&output, "{Date}").unwrap();
        let target = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == "Café{Size}Tail" => {
                let caret = text.galley.pos_from_cursor(egui::text::CCursor::new(6));
                Some(text.pos + caret.center().to_vec2())
            }
            _ => None,
        }).expect("Editable filename");
        render(&mut app, click(source)[0].clone());
        render(&mut app, vec![egui::Event::PointerMoved(target)]);
        render(&mut app, vec![
            egui::Event::PointerButton { pos: target, button: egui::PointerButton::Primary,
                pressed: false, modifiers: egui::Modifiers::NONE },
        ]);
        assert_eq!(app.project.options.filename_template, "Café{Date}{Size}Tail");
        render(&mut app, vec![egui::Event::Text("_".into())]);
        assert_eq!(app.project.options.filename_template, "Café{Date}_{Size}Tail");

        if let Some(directory) = std::env::var_os("MAPPER_FILENAME_FIELD_SNAPSHOTS") {
            let image = textures.raster(&ctx, palette, size);
            mapper_studio::image_export::write_png(
                &PathBuf::from(directory).join("filename-fields.png"), &image, || false,
            ).unwrap();
        }
    }

    #[test]
    fn terrain_and_biomes_can_be_enabled_and_cleared_independently() {
        let ctx = egui::Context::default();
        workbench_style::install(&ctx);
        ctx.all_styles_mut(|style| style.animation_time = 0.);
        let mut app = crate::tests::fixture(&ctx);
        let profile: mapper_studio::native_profile::Profile = serde_json::from_str(include_str!(
            "../../../compatibility/native/client-0f703d43.json"
        )).unwrap();
        app.project.preview_engine = mapper_studio::native_preview::VERSION.into();
        app.project.installation = Some(mapper_studio::installation::Installation {
            kind: mapper_studio::installation::InstallationKind::Client,
            native: mapper_studio::installation::SourceFile {
                relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                sha256: profile.native_sha256,
                bytes: 1,
            },
            scene_manifest: None,
        });
        let initial = app.project.clone();
        let initial_key = initial.terrain_key();
        let frame = |app: &mut Studio, events| {
            let mut output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(210., 500.))),
                events,
                ..Default::default()
            }, |ui| {
                egui::CentralPanel::default().show(ui, |ui| app.recipe(ui));
            });
            output.textures_delta.clear();
            output
        };
        let click = |app: &mut Studio, label: &str, checkbox: bool| {
            let output = frame(app, vec![]);
            let point = if checkbox {
                recipe_checkbox_position(&output, label)
            } else {
                output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.job.text == label => {
                        Some(text.pos + text.galley.size() * 0.5)
                    }
                    _ => None,
                }).unwrap()
            };
            for pressed in [true, false] {
                frame(app, vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ]);
            }
        };
        let default = frame(&mut app, vec![]);
        for label in ["Custom tiers", "Custom mix"] {
            let visible = default.shapes.iter().any(|shape| matches!(&shape.shape,
                egui::Shape::Text(text) if text.galley.job.text == label
                    && text.pos.x + text.galley.size().x <= shape.clip_rect.right()
            ));
            assert!(visible, "Clipped or missing {label}");
        }
        click(&mut app, "Terrain", false);
        assert!(app.project.terrain_overrides.is_none());
        click(&mut app, "Terrain", true);
        assert!(app.project.terrain_overrides.is_some());
        assert!(app.project.biome_overrides.is_none());
        click(&mut app, "Biomes", true);
        assert!(app.project.biome_overrides.is_some());
        assert_eq!(app.project.terrain_key(), initial_key);
        assert!(app.placement_recipe_matches(&initial));
        let biomes = app.project.biome_overrides.as_mut().unwrap();
        biomes.angle = (biomes.angle + 90_000) % 360_000;
        assert_ne!(app.project.terrain_key(), initial_key);
        assert!(!app.placement_recipe_matches(&initial));
        click(&mut app, "Terrain", true);
        assert!(app.project.terrain_overrides.is_none());
        assert!(app.project.biome_overrides.is_some());
        click(&mut app, "Biomes", true);
        assert_eq!(app.project.terrain_key(), initial_key);
    }

    #[test]
    fn erosion_selector_binds_the_recipe_and_opens_gpu_buffer_settings() {
        mod snapshot { include!("../examples/support/egui_snapshot.rs"); }
        let ctx = egui::Context::default();
        workbench_style::install(&ctx);
        ctx.all_styles_mut(|style| style.animation_time = 0.);
        let mut app = crate::tests::fixture(&ctx);
        let profile: mapper_studio::native_profile::Profile = serde_json::from_str(include_str!(
            "../../../compatibility/native/client-0f703d43.json"
        )).unwrap();
        app.project.size = 4000;
        app.project.preview_engine = mapper_studio::native_preview::VERSION.into();
        app.project.composition_engine = Some(papyrus_terrain::monument_composition::SOURCE_HEIGHT_VERSION.into());
        app.project.installation = Some(mapper_studio::installation::Installation {
            kind: mapper_studio::installation::InstallationKind::Client,
            native: mapper_studio::installation::SourceFile {
                relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                sha256: profile.native_sha256,
                bytes: 1,
            },
            scene_manifest: None,
        });
        app.project.monument_rules = Some(serde_json::from_value(serde_json::json!({
            "engine": mapper_studio::monument_source::VERSION,
            "source_sha256": "a".repeat(64),
            "surface_modifiers": mapper_studio::monument_source::SOURCE_PRIORITY_VERSION,
        })).unwrap());
        let gpu = Identity {
            engine: erosion_compute::VERSION.into(),
            backend: "Vulkan".into(),
            vendor_id: 1,
            device_id: 2,
            name: "Test GPU".into(),
            driver: "Test driver".into(),
            driver_info: "1".into(),
        };
        app.erosion_device = ErosionDevice::Ready(gpu.clone());
        app.preferences.generation_resources.gpu_memory_mib = 256;
        let textures = std::cell::RefCell::new(snapshot::Textures::default());
        let settings_frame = std::cell::Cell::new(false);
        let render = |app: &mut Studio, events| {
            let size = if settings_frame.get() { [820., 640.] } else { [210., 520.] };
            let mut output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size.into())),
                focused: true,
                events,
                ..Default::default()
            }, |ui| {
                if settings_frame.get() {
                    app.settings_window(ui.ctx());
                } else {
                    app.recipe(ui);
                }
            });
            textures.borrow_mut().update(&output.textures_delta);
            output.textures_delta.clear();
            output
        };
        let click = |app: &mut Studio, label: &str| {
            let frame = render(app, vec![]);
            let point = frame.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == label => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            }).unwrap_or_else(|| panic!("Missing {label}"));
            let width = if settings_frame.get() { 820. } else { 210. };
            assert!((0. ..width).contains(&point.x));
            for pressed in [true, false] {
                render(app, vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ]);
            }
        };
        click(&mut app, "Terrain");
        click(&mut app, "CPU");
        click(&mut app, "GPU");
        assert_eq!(app.project.gpu_erosion, Some(gpu.clone()));
        assert_eq!(app.preferences.generation_resources.gpu_memory_mib, 256);
        let required = erosion_compute::required_memory_mib(4097).unwrap();
        click(&mut app, &format!("GPU needs {required} MiB"));
        assert!(app.settings_open);
        assert!(app.settings_page == settings::Page::Performance);
        click(&mut app, "GPU");
        click(&mut app, "CPU");
        assert!(app.project.gpu_erosion.is_none());
        let mut limits = app.preferences.generation_resources;
        limits.gpu_memory_mib = 1024;
        app.preferences.generation_resources = limits;
        app.settings_open = false;
        click(&mut app, "CPU");
        click(&mut app, "Compare CPU and GPU…");
        assert!(app.settings_open);
        assert!(app.settings_page == settings::Page::Performance);
        let recipe = render(&mut app, vec![]);
        assert!(!recipe.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Text(text) if text.galley.job.text == "Run benchmark")));
        if let Some(dir) = std::env::var_os("MAPPER_UI_SNAPSHOT_DIR") {
            let image = textures.borrow().raster(&ctx, recipe, [210, 520]);
            mapper_studio::image_export::write_png(&std::path::PathBuf::from(dir).join("recipe-erosion.png"), &image, || false).unwrap();
        }
        settings_frame.set(true);
        for _ in 0..3 { render(&mut app, vec![]); }
        app.erosion_comparison.complete_test(gpu.clone(), limits);
        let recommended = render(&mut app, vec![]);
        assert!(app.project.gpu_erosion.is_none());
        if let Some(dir) = std::env::var_os("MAPPER_UI_SNAPSHOT_DIR") {
            let image = textures.borrow().raster(&ctx, recommended, [820, 640]);
            mapper_studio::image_export::write_png(&std::path::PathBuf::from(dir).join("settings-erosion.png"), &image, || false).unwrap();
        }
        click(&mut app, "Use recommended GPU");
        assert_eq!(app.project.gpu_erosion, Some(gpu));
        app.preferences.generation_resources.gpu_memory_mib = 256;
        let stale = render(&mut app, vec![]);
        assert!(!stale.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Text(text) if text.galley.job.text == "GPU recommended")));
        assert!(app.project.gpu_erosion.is_some());
    }

    fn selection_fixture(ctx: &egui::Context) -> Studio {
        let mut app = crate::tests::fixture(ctx);
        let path = app.project.targets[0].prefab_path.clone().unwrap();
        app.prefabs.seed_test_stamp(ctx, &mut app.project, &path);
        app.project.targets[0].name = "launch_site_internal_identifier_with_a_long_variant_name".into();
        let source = app.prefabs.catalog(&app.project).unwrap();
        let asset = mapper_studio::prefab_assets::Asset {
            priority: None,
            bounds: None,
            yaw: None,
            modifier_size: None,
            modifier_offset: None,
            texture_guids: Default::default(),
            splat_mask: None,
            modifier: None,
            modifier_absent: false,
            water_geometry: None,
            no_build_geometry: None,
            display_name: Some("Launch Site".into()),
        };
        app.monument_icons.seed_names(std::sync::Arc::new(mapper_studio::prefab_assets::Catalog {
            sha256: source.sha256.clone(),
            installation: source.installation.clone(),
            entries: std::collections::HashMap::from([(path, asset)]),
            sources: source.sources.clone(),
        }));
        app
    }

    #[test]
    fn seed_fields_fit_narrow_docks_and_link_only_the_selected_domain() {
        mod snapshot { include!("../examples/support/egui_snapshot.rs"); }
        for dark in [false, true] {
            for width in [210, 290] {
                let ctx = egui::Context::default();
                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                workbench_style::install(&ctx);
                let mut project = Project::default();
                for seed in &mut project.seeds {
                    seed.inherited = false;
                    seed.value = MAX_SEED;
                }
                let before = project.clone();
                let screen = Rect::from_min_size(Pos2::ZERO, egui::vec2(width as f32, 200.));
                let render = |project: &mut Project, random_master, events| ctx.run_ui(
                    egui::RawInput { screen_rect: Some(screen), events, focused: true, ..Default::default() },
                    |ui| {
                        egui::CentralPanel::default().show(ui, |ui| stage_seeds(ui, project, random_master));
                    },
                );
                let mut textures = snapshot::Textures::default();
                let mut first = render(&mut project, false, vec![]);
                textures.update(&first.textures_delta);
                first.textures_delta.clear();
                let mut output = render(&mut project, false, vec![]);
                textures.update(&output.textures_delta);
                output.textures_delta.clear();
                assert_eq!(project, before);
                let mut fixed_x = None;
                let mut monuments_y = None;
                let mut values = 0;
                for shape in &output.shapes {
                    if let egui::Shape::Text(text) = &shape.shape {
                        let rect = text.galley.rect.translate(text.pos.to_vec2());
                        assert!(screen.contains_rect(rect) && shape.clip_rect.contains_rect(rect),
                            "Seed field clipped at {width}px: {:?} {rect:?}", text.galley.job.text);
                        if text.galley.job.text == "Fixed" { fixed_x = Some(rect.left() + 7.); }
                        if text.galley.job.text == "Monuments" { monuments_y = Some(rect.center().y); }
                        if text.galley.job.text == MAX_SEED.to_string() { values += 1; }
                    }
                }
                assert_eq!(values, STAGES.len());
                if let Some(directory) = std::env::var_os("MAPPER_RECIPE_SNAPSHOTS") {
                    let directory = PathBuf::from(directory);
                    std::fs::create_dir_all(&directory).unwrap();
                    let image = textures.raster(&ctx, output, [width, 200]);
                    let path = directory.join(format!("seeds-{}-{width}.png", if dark { "dark" } else { "light" }));
                    mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
                }
                let point = egui::pos2(fixed_x.unwrap(), monuments_y.unwrap());
                let button = |pressed| egui::Event::PointerButton {
                    pos: point, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
                };
                let mut pressed = render(&mut project, false, vec![egui::Event::PointerMoved(point), button(true)]);
                pressed.textures_delta.clear();
                let mut released = render(&mut project, false, vec![button(false)]);
                released.textures_delta.clear();
                let mut expected = before;
                expected.seeds[4].inherited = true;
                expected.seeds[4].value = mapper_studio::derive_seed(expected.master_seed, STAGES[4]);
                assert_eq!(project, expected);
                let mut random_view = render(&mut project, true, vec![]);
                textures.update(&random_view.textures_delta);
                random_view.textures_delta.clear();
                let labels: Vec<_> = random_view.shapes.iter().filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) => Some(text.galley.job.text.as_str()),
                    _ => None,
                }).collect();
                assert_eq!(labels.iter().filter(|&&label| label == "From master").count(), 1);
                assert_eq!(labels.iter().filter(|&&label| label == MAX_SEED.to_string()).count(), STAGES.len() - 1);
                assert_eq!(project, expected);
                if let Some(directory) = std::env::var_os("MAPPER_RECIPE_SNAPSHOTS") {
                    let image = textures.raster(&ctx, random_view, [width, 200]);
                    let path = PathBuf::from(directory).join(format!(
                        "random-seeds-{}-{width}.png", if dark { "dark" } else { "light" },
                    ));
                    mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
                }
                let mut pressed = render(&mut project, true, vec![button(true)]);
                pressed.textures_delta.clear();
                let mut released = render(&mut project, true, vec![button(false)]);
                released.textures_delta.clear();
                expected.seeds[4].inherited = false;
                assert_eq!(project, expected);
            }
        }
    }

    #[test]
    fn compact_monument_properties_fit_narrow_docks_and_preserve_rotation_behavior() {
        mod snapshot { include!("../examples/support/egui_snapshot.rs"); }
        for dark in [false, true] {
            for width in [190, 210, 290] {
                for fixed in [false, true] {
                    let ctx = egui::Context::default();
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    workbench_style::install(&ctx);
                    let mut app = selection_fixture(&ctx);
                    let half = app.project.size as f32 / 2.;
                    app.project.targets[0].x = -half;
                    app.project.targets[0].z = half;
                    app.project.targets[0].radius = app.project.size as f32;
                    app.project.targets[0].strength = 9.5;
                    app.project.targets[0].yaw = fixed.then_some(360.0_f32.next_down());
                    app.project.placement_engine = if fixed {
                        mapper_studio::placement_preview::ROTATION_VERSION
                    } else {
                        papyrus_terrain::placement::VERSION
                    }.into();
                    app.project.validate().unwrap();
                    let original = app.project.clone();
                    let screen = Rect::from_min_size(Pos2::ZERO, egui::vec2(width as f32, 480.));
                    let render = |app: &mut Studio, events| ctx.run_ui(
                        egui::RawInput { screen_rect: Some(screen), events, focused: true, ..Default::default() },
                        |ui| { egui::CentralPanel::default().show(ui, |ui| app.inspector(ui)); },
                    );
                    let mut textures = snapshot::Textures::default();
                    let mut first = render(&mut app, vec![]);
                    textures.update(&first.textures_delta);
                    first.textures_delta.clear();
                    let mut output = render(&mut app, vec![]);
                    textures.update(&output.textures_delta);
                    output.textures_delta.clear();
                    assert_eq!(app.project, original, "Displaying properties must preserve the recipe");
                    let mut rotation = None;
                    for shape in &output.shapes {
                        if let egui::Shape::Text(text) = &shape.shape {
                            let rect = text.galley.rect.translate(text.pos.to_vec2());
                            assert!(screen.contains_rect(rect) && shape.clip_rect.contains_rect(rect),
                                "Property clipped at {width}px: {:?} {rect:?}", text.galley.job.text);
                            if text.galley.job.text == "Rotation" { rotation = Some(rect.center()); }
                            assert_ne!(text.galley.job.text, "Preferred location");
                        }
                    }
                    if let Some(directory) = std::env::var_os("MAPPER_INSPECTOR_SNAPSHOTS") {
                        let directory = PathBuf::from(directory);
                        std::fs::create_dir_all(&directory).unwrap();
                        let image = textures.raster(&ctx, output, [width, 480]);
                        let name = format!("selection-{}-{width}-{}.png", if dark { "dark" } else { "light" },
                            if fixed { "fixed" } else { "automatic" });
                        mapper_studio::image_export::write_png(&directory.join(name), &image, || false).unwrap();
                    }
                    let point = rotation.expect("Rotation remains visible");
                    let button = |pressed| egui::Event::PointerButton {
                        pos: point, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
                    };
                    let mut pressed = render(&mut app, vec![egui::Event::PointerMoved(point), button(true)]);
                    pressed.textures_delta.clear();
                    let mut released = render(&mut app, vec![button(false)]);
                    released.textures_delta.clear();
                    let mut expected = original;
                    expected.targets[0].yaw = (!fixed).then_some(0.);
                    if !fixed { expected.placement_engine = mapper_studio::placement_preview::ROTATION_VERSION.into(); }
                    assert_eq!(app.project, expected, "Rotation must update only its request and required engine");
                }
            }
        }
    }

    #[test]
    fn compact_selection_remains_available_in_the_complete_workspace() {
        mod snapshot { include!("../examples/support/egui_snapshot.rs"); }
        for dark in [false, true] {
            for [width, height] in [[1050, 720], [1440, 940]] {
                let ctx = egui::Context::default();
                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                workbench_style::install(&ctx);
                let mut app = selection_fixture(&ctx);
                let preview_path = app.dock.find_tab(&Tab::Map).unwrap().node_path();
                app.dock.leaf_mut(preview_path).unwrap().tabs.push(Tab::Nodes);
                if let Some(path) = std::env::var_os("MAPPER_WORKSPACE_OVERVIEW") {
                    let file = std::io::BufReader::new(std::fs::File::open(path).unwrap());
                    let mut reader = png::Decoder::new(file).read_info().unwrap();
                    assert!(reader.info().width <= 4096 && reader.info().height <= 4096);
                    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
                    let info = reader.next_frame(&mut pixels).unwrap();
                    let size = [info.width as usize, info.height as usize];
                    assert_eq!(info.bit_depth, png::BitDepth::Eight);
                    let image = match info.color_type {
                        png::ColorType::Rgb => egui::ColorImage::from_rgb(size, &pixels[..info.buffer_size()]),
                        png::ColorType::Rgba => egui::ColorImage::from_rgba_unmultiplied(size, &pixels[..info.buffer_size()]),
                        _ => panic!("Workspace overview must be RGB or RGBA"),
                    };
                    let image = std::sync::Arc::new(image);
                    app.texture.as_mut().unwrap().set(image.clone(), egui::TextureOptions::LINEAR);
                    app.preview.as_mut().unwrap().image = image;
                }
                let before = app.project.clone();
                let mut textures = snapshot::Textures::default();
                let mut last = None;
                for _ in 0..3 {
                    let mut output = ctx.run_ui(
                        egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO,
                            egui::vec2(width as f32, height as f32))), ..Default::default() },
                        |ui| {
                            let mut dock = std::mem::replace(&mut app.dock, DockState::new(vec![]));
                            egui::Panel::top("toolbar").show(ui, |ui| app.toolbar(ui));
                            app.generation_status(ui);
                            egui::CentralPanel::default().frame(workbench_style::dock_frame(ui)).show(ui, |ui| {
                                let mut tabs = Tabs::new(&mut app, None, &dock);
                                DockArea::new(&mut dock).style(workbench_style::dock(ui))
                                    .show_leaf_close_all_buttons(false).show_leaf_collapse_buttons(false)
                                    .show_inside(ui, &mut tabs);
                                tabs.finish(&mut dock);
                            });
                            app.dock = dock;
                        },
                    );
                    textures.update(&output.textures_delta);
                    output.textures_delta.clear();
                    last = Some(output);
                }
                assert_eq!(app.project, before);
                let output = last.unwrap();
                let recipe_path = app.dock.find_tab(&Tab::Recipe).unwrap();
                let recipe_bounds = app.dock.leaf(recipe_path.node_path()).unwrap().rect;
                for (panel, name) in [
                    (Tab::Recipe, "Recipe"), (Tab::Map, "Recipe preview"),
                    (Tab::Outliner, "Monuments"), (Tab::Inspector, "Selection"), (Tab::Queue, "Queue"),
                ] {
                    let path = app.dock.find_tab(&panel).unwrap().node_path();
                    let leaf = app.dock.leaf(path).unwrap();
                    assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                        egui::Shape::Rect(rect) if rect.corner_radius.sw == 8
                            && rect.corner_radius.nw == 0 && !rect.stroke.is_empty()
                            && rect.fill != Color32::TRANSPARENT && rect.rect.center().distance(leaf.viewport.center()) < 2.
                    )), "{name} must draw its fill and rounded outline together");
                }
                let button = output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect) if rect.brush.is_some()
                        && recipe_bounds.contains_rect(rect.rect) => Some(rect.rect),
                    _ => None,
                }).expect("The Generate and Batch control belongs to Recipe");
                assert!((button.center().x - recipe_bounds.center().x).abs() <= 1.);
                assert!(recipe_bounds.bottom() - button.bottom() <= 16.);
                if let Some(directory) = std::env::var_os("MAPPER_INSPECTOR_SNAPSHOTS") {
                    let directory = PathBuf::from(directory);
                    std::fs::create_dir_all(&directory).unwrap();
                    let image = textures.raster(&ctx, output.clone(), [width, height]);
                    let name = format!("workspace-{}-{width}x{height}.png", if dark { "dark" } else { "light" });
                    mapper_studio::image_export::write_png(&directory.join(name), &image, || false).unwrap();
                }
                let row = |label: &str| {
                    output.shapes.iter().find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.job.text == label => Some(text.pos.y),
                        _ => None,
                    })
                };
                assert!((row("Recipe preview").unwrap() - row("Nodes").unwrap()).abs() < 2.);
                assert!(row("Master seed").unwrap() < row("Stage seeds").unwrap());
                assert!(row("Stage seeds").unwrap() < row("Terrain").unwrap());
                assert!(row("Terrain").unwrap() < row("Biomes").unwrap());
                let mut left_edge = None;
                let mut help_column = None;
                for category in ["Terrain", "Biomes", "Monuments", "Water & rivers",
                    "Roads & trails", "Rails", "Scenery & objects", "Stage seeds", "Landmarks", "Content rules", "Output"] {
                    if !["Stage seeds", "Landmarks", "Content rules", "Output"].contains(&category) {
                        recipe_checkbox_position(&output, category);
                    }
                    let label = output.shapes.iter().filter_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.job.text == category =>
                            Some(text.pos + egui::vec2(0., 0.5 * text.galley.size().y)),
                        _ => None,
                    }).min_by(|a, b| a.x.total_cmp(&b.x)).unwrap();
                    let first = left_edge.get_or_insert(label.x);
                    assert!((label.x - *first).abs() < 1., "Misaligned recipe category {category}");
                    let help = output.shapes.iter().filter_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.job.text == "?" => {
                            let center = text.pos + text.galley.size() * 0.5;
                            ((center.y - label.y).abs() < 2. && center.x > label.x
                                && shape.clip_rect.contains(center)).then_some(center.x)
                        }
                        _ => None,
                    }).min_by(f32::total_cmp)
                        .unwrap_or_else(|| panic!("Missing Recipe help for {category}"));
                    let first = help_column.get_or_insert(help);
                    assert!((help - *first).abs() < 1., "Misaligned recipe help for {category}");
                }
                assert!(row("Custom monuments").is_none());
                for shape in &output.shapes {
                    if let egui::Shape::Text(text) = &shape.shape
                        && ["Preferred X", "Preferred Z", "Rotation", "Regenerate monuments"].contains(&text.galley.job.text.as_str()) {
                        let rect = text.galley.rect.translate(text.pos.to_vec2());
                        assert!(shape.clip_rect.contains_rect(rect), "Workspace control clipped at {width}×{height}");
                    }
                }
            }
        }
    }

    #[test]
    fn compact_outliner_distinguishes_duplicate_names_by_row_identity() {
        let ctx = egui::Context::default();
        workbench_style::install(&ctx);
        let mut app = crate::tests::fixture(&ctx);
        app.project.targets.truncate(1);
        app.project.targets[0].name = "Duplicate monument".into();
        app.project.targets[0].prefab_path = None;
        let mut duplicate = app.project.targets[0].clone();
        duplicate.instance_id += 1000;
        app.project.targets.push(duplicate);
        let original = app.project.clone();
        let screen = Rect::from_min_size(Pos2::ZERO, egui::vec2(240., 260.));
        let render = |app: &mut Studio, events| {
            let mut output = ctx.run_ui(
                egui::RawInput { screen_rect: Some(screen), events, focused: true, ..Default::default() },
                |ui| { egui::CentralPanel::default().show(ui, |ui| app.outliner(ui, None)); },
            );
            output.textures_delta.clear();
            output
        };
        let _ = render(&mut app, vec![]);
        let output = render(&mut app, vec![]);
        let rows: Vec<_> = output.shapes.iter().filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == "Duplicate monument" =>
                Some(text.galley.rect.translate(text.pos.to_vec2()).center()),
            _ => None,
        }).collect();
        assert_eq!(rows.len(), 2);
        for qualifier in ["#1", "#2"] {
            assert!(output.shapes.iter().any(|shape| matches!(
                &shape.shape, egui::Shape::Text(text) if text.galley.job.text == qualifier
            )));
        }
        for index in [1, 0] {
            let point = rows[index];
            let button = |pressed| egui::Event::PointerButton {
                pos: point, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
            };
            let _ = render(&mut app, vec![egui::Event::PointerMoved(point), button(true)]);
            let _ = render(&mut app, vec![button(false)]);
            assert_eq!(app.selected, Some(index));
            assert_eq!(app.project, original);
        }
    }

    #[test]
    fn outliner_variants_fit_narrow_docks_and_keep_their_suffix_during_search() {
        mod snapshot { include!("../examples/support/egui_snapshot.rs"); }
        for dark in [false, true] {
            let ctx = egui::Context::default();
            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
            workbench_style::install(&ctx);
            let mut app = crate::tests::fixture(&ctx);
            app.project.targets.truncate(2);
            for (index, target) in app.project.targets.iter_mut().enumerate() {
                let suffix = ['a', 'b'][index];
                target.name = "Abandoned Military Base".into();
                target.prefab_path = Some(format!(
                    "assets/bundled/prefabs/autospawn/monument/military_bases/desert_military_base_{suffix}.prefab"
                ));
            }
            let size = [176, 225];
            let screen = Rect::from_min_size(Pos2::ZERO, egui::vec2(size[0] as f32, size[1] as f32));
            let mut textures = snapshot::Textures::default();
            let mut render = |app: &mut Studio| {
                let mut output = ctx.run_ui(egui::RawInput {
                    screen_rect: Some(screen), ..Default::default()
                }, |ui| {
                    egui::CentralPanel::default().show(ui, |ui| app.outliner(ui, None));
                });
                textures.update(&output.textures_delta);
                output.textures_delta.clear();
                output
            };
            render(&mut app);
            let full = render(&mut app);
            let visible = |output: &egui::FullOutput, label| output.shapes.iter().any(|shape| {
                if let egui::Shape::Text(text) = &shape.shape {
                    let rect = text.galley.rect.translate(text.pos.to_vec2());
                    text.galley.job.text == label && shape.clip_rect.contains_rect(rect)
                } else {
                    false
                }
            });
            assert!(visible(&full, "A") && visible(&full, "B"));
            assert!(visible(&full, "Import…"));
            app.monument_filter = "military base b".into();
            let filtered = render(&mut app);
            assert!(!visible(&filtered, "A") && visible(&filtered, "B"));
            if let Some(directory) = std::env::var_os("MAPPER_OUTLINER_SNAPSHOTS") {
                let theme = if dark { "dark" } else { "light" };
                let image = textures.raster(&ctx, full, size);
                let path = PathBuf::from(directory).join(format!("outliner-{theme}.png"));
                mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
            }
        }
    }

    #[test]
    fn narrow_monument_catalog_keeps_custom_import_and_settings_available() {
        let ctx=egui::Context::default();
        workbench_style::install(&ctx);
        let mut app=crate::tests::fixture(&ctx);
        app.project.preview_engine=mapper_studio::LEGACY_PREVIEW_ENGINE.into();
        app.preview=None;
        app.selected=app.project.targets.iter().position(|target| target.prefab_path.is_some());
        let screen=Rect::from_min_size(Pos2::ZERO,egui::vec2(150.,720.));
        let render=|app:&mut Studio,events|{let mut out=ctx.run_ui(egui::RawInput{screen_rect:Some(screen),events,focused:true,..Default::default()},|ui|app.outliner(ui,None));out.textures_delta.clear();out};
        let _=render(&mut app,vec![]);
        let out=render(&mut app,vec![]);
        let (clip,rect)=out.shapes.iter().find_map(|shape|match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text=="Import…"=>Some((shape.clip_rect,text.galley.rect.translate(text.pos.to_vec2()))),
            _=>None,
        }).expect("Import action is drawn");
        assert!(screen.contains_rect(rect) && clip.contains_rect(rect),"Import must fit inside the visible inspector: {rect:?}, clip {clip:?}");
        assert!(!out.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Text(text) if text.galley.job.text == "Add"
        )));
        let point=rect.center();
        let count=app.project.targets.len();
        let button=|pressed|egui::Event::PointerButton{pos:point,button:egui::PointerButton::Primary,pressed,modifiers:egui::Modifiers::NONE};
        let _=render(&mut app,vec![egui::Event::PointerMoved(point),button(true)]);
        let _=render(&mut app,vec![button(false)]);
        assert!(app.custom_monuments.is_open());
        assert_eq!(app.project.targets.len(),count);

        let out = render(&mut app, vec![]);
        let point = out.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Circle(circle) if circle.radius == 2.25 => Some(circle.center),
            _ => None,
        }).expect("Selected monument has a settings gear");
        assert!(screen.contains(point) && point.y < 40.);
        for pressed in [true, false] {
            render(&mut app, vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
                },
            ]);
        }
        assert!(app.content.monument_settings_open());
        assert_eq!(app.project.targets.len(), count);
        assert!(app.project.monument_addons.is_empty());
    }
}
