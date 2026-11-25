#[cfg(test)]
#[path = "../examples/support/prepared_assets.rs"]
mod prepared_assets;
mod generation_ui;
mod tool_icons;
mod path_editor;
mod map_label_panel;
mod map_archive;
mod project_io;
mod diagnostic_ui;
mod river_panel;
mod loading_ui;
mod composition_panel;
mod color_panel;
mod content_panel;
mod custom_monument_panel;
mod update_panel;
mod decor_model_panel;
mod decor_panel;
mod export_panel;
mod export_render;
mod infrastructure_panel;
mod menus;
mod monument_panel;
mod panels;
mod node_panel;
mod erosion_panel;
mod plugin_panel;
mod prefab_panel;
mod preparation_panel;
mod rail_panel;
mod preview;
mod profile_menu;
mod projection_panel;
mod settings;
mod settings_dialog;
mod source_panel;
mod terrain_3d;
mod topology_tool;
mod map_view_tabs;
mod map_monuments;
mod restriction_panel;
mod monument_icon_panel;
mod installation_panel;
mod workbench_style;
mod outliner;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use egui_dock::{DockArea, DockState, NodeIndex, TabViewer};
use egui_dock::tab_viewer::OnCloseResponse;
use preview::{Layout, LayoutTask, Preview, Task};
use mapper_studio::{History, MAX_SEED, Presence, Project, STAGES};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::Ordering, mpsc::TryRecvError},
};
const MAP_BACKGROUND: Color32 = Color32::from_rgb(24, 66, 95);
fn ocean_background(palette:Option<&mapper_studio::terrain_palette::Palette>,colors:&mapper_studio::map_colors::Preferences)->Color32 {
    palette.and_then(|p|p.defaults()).map(|material| {
        let rgb=material.effective(colors).water([0.;8],50.).map(|v|v.round().clamp(0.,255.) as u8);
        Color32::from_rgb(rgb[0],rgb[1],rgb[2])
    }).unwrap_or(MAP_BACKGROUND)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Map,
    Generated(u64),
    Recipe,
    Outliner,
    Inspector,
    Queue,
    Content,
    Plugins,
    Nodes,
}
fn layout() -> DockState<Tab> {
    let mut dock = DockState::new(vec![Tab::Map]);
    let [workspace, _recipe] = dock.main_surface_mut().split_left(
        NodeIndex::root(),
        0.22,
        vec![Tab::Recipe],
    );
    let [center, outliner] = dock.main_surface_mut().split_right(
        workspace,
        0.78,
        vec![Tab::Outliner],
    );
    dock.main_surface_mut().split_below(outliner, 0.5, vec![Tab::Inspector]);
    dock.main_surface_mut()
        .split_below(center, 0.85, vec![Tab::Queue]);
    dock
}
#[derive(Clone, Copy)]
enum Action {
    New,
    Open,
    AcceptOpened,
    Close,
}
#[derive(Clone)]
struct PlacementRequest {
    recipe: Project,
    id: u64,
}
fn marker_label(painter: &egui::Painter, outer: Rect, p: Pos2, text: &str) {
    let galley = painter.layout_no_wrap(text.into(), FontId::proportional(13.), Color32::WHITE);
    let label = egui::pos2(
        (p.x + 12.).clamp(
            outer.left() + 3.,
            (outer.right() - galley.size().x - 6.).max(outer.left() + 3.),
        ),
        (p.y - 20.).clamp(
            outer.top() + 3.,
            (outer.bottom() - 20.).max(outer.top() + 3.),
        ),
    );
    painter.rect_filled(
        Rect::from_min_size(
            label - egui::vec2(3., 2.),
            galley.size() + egui::vec2(6., 4.),
        ),
        3.,
        Color32::from_black_alpha(210),
    );
    painter.galley(label, galley, Color32::WHITE);
}
struct Studio {
    project_io:project_io::ProjectIo,
    map_archive: map_archive::Panel,
    project: Project,
    saved: Project,
    path: Option<PathBuf>,
    history: History,
    dock: DockState<Tab>,
    requested_tab: Option<Tab>,
    selected: Option<usize>,
    monument_filter: String,
    texture: Option<egui::TextureHandle>,
    stage_image: Option<egui::ColorImage>,
    stage_texture: Option<egui::TextureHandle>,
    layer_texture: Option<egui::TextureHandle>,
    native_view: mapper_studio::native_view::View,
    topology_bit: u8,
    topology_tool: topology_tool::Tool,
    generation_progress: Option<&'static str>,
    live_generation: bool,
    preview: Option<Preview>,
    task: Option<Task>,
    placement_task: Option<LayoutTask>,
    layouts: Vec<Layout>,
    active_layout: Option<usize>,
    template_picker: outliner::TemplatePicker,
    content: content_panel::ContentPanel,
    custom_monuments: custom_monument_panel::Panel,
    updates: update_panel::Panel,
    preparation: preparation_panel::PreparationPanel,
    prefabs: prefab_panel::PrefabPanel,
    monuments: monument_panel::MonumentPanel,
    roads: infrastructure_panel::RoadPanel,
    rails: rail_panel::RailPanel,
    decoration_panel: decor_panel::DecorPanel,
    decoration_models: decor_model_panel::DecorModelsPanel,
    composition: composition_panel::CompositionPanel,
    terrain_projection: projection_panel::Panel,
    erosion_device: panels::ErosionDevice,
    erosion_comparison: erosion_panel::Comparison,
    nodes: node_panel::Panel,
    map_colors: color_panel::Panel,
    source: source_panel::SourcePanel,
    generation: generation_ui::Generation,
    water: river_panel::Panel,
    image_export: export_panel::ImageExport,
    export_options_open: bool,
    image_export_target: Option<u64>,
    plugins: plugin_panel::PluginPanel,
    status: String,
    zoom: f32,
    pan: Vec2,
    grid: bool,
    show_no_build: bool,
    monument_icons: monument_icon_panel::Panel,
    installation_library: installation_panel::Panel,
    restrictions: restriction_panel::Panel,
    show_monuments: bool,
    show_decoration: bool,
    show_preferences: bool,
    drag_before: Option<Project>,
    drag_origin: Option<[f32; 2]>,
    drag_press: Option<Pos2>,
    drag_press_can_edit: bool,
    placement_request: Option<PlacementRequest>,
    camera: terrain_3d::Camera,
    drag_splat: Option<egui::TextureHandle>,
    skip_history: bool,
    pending: Option<Action>,
    allow_close: bool,
    reset_layout: bool,
    preferences: mapper_studio::preferences::Preferences,
    settings_path: Option<PathBuf>,
    settings_message: String,
    settings_open: bool,
    settings_page: settings::Page,
    recipe_settings_open: bool,
    recipe_settings_tab: panels::RecipeTab,
    installation_setup_open: bool,
    installation_roster_pending: bool,
    installation_placement_pending: bool,
    restore_source: bool,
}
impl Studio {
    fn viewport_background(&self)->Color32 {
        let palette=self.retained_composition().and_then(|v|v.palette.as_deref())
            .or_else(||self.preview.as_ref().and_then(|p|p.palette.as_deref()));
        ocean_background(palette,&self.preferences.map_colors)
    }
    fn new_workbench_project() -> Project {
        let mut project = Project::default();
        // The library keeps its versioned independent preview fixture. Desktop
        // projects start with the selected installation's scene roster instead.
        project.targets.clear();
        project.master_seed = 0;
        project.refresh_inherited_seeds();
        project
    }
    fn new(cc: &eframe::CreationContext<'_>, path: Option<PathBuf>) -> Self {
        if let Some(state) = &cc.wgpu_render_state {
            mapper_studio::map_compute::register_renderer(state);
        }
        workbench_style::install(&cc.egui_ctx);
        let p = Self::new_workbench_project();
        let mut app = Self {
            project_io:Default::default(),
            saved: p.clone(),
            project: p,
            path: None,
            history: History::default(),
            dock: layout(),
            map_archive: Default::default(),
            requested_tab: None,
            selected: None,
            monument_filter: String::new(),
            texture: None,
            stage_image: None,
            stage_texture: None,
            layer_texture: None,
            native_view: mapper_studio::native_view::View::default(),
            topology_bit: 7,
            topology_tool: Default::default(),
            generation_progress: None,
            live_generation: true,
            preview: None,
            task: None,
            placement_task: None,
            layouts: Vec::new(),
            active_layout: None,
            template_picker: Default::default(),
            content: content_panel::ContentPanel::default(),
            custom_monuments: Default::default(),
            updates: update_panel::Panel::new(),
            preparation: preparation_panel::PreparationPanel::default(),
            prefabs: prefab_panel::PrefabPanel::default(),
            monuments: Default::default(),
            roads: Default::default(),
            rails: Default::default(),
            decoration_panel: Default::default(),
            decoration_models: Default::default(),
            source: source_panel::SourcePanel::default(),
            generation: generation_ui::Generation::default(),
            water: river_panel::Panel::default(),
            image_export: Default::default(),
            export_options_open: false,
            image_export_target: None,
            status: "Configure a recipe to begin.".into(),
            plugins: plugin_panel::PluginPanel::default(),
            composition: composition_panel::CompositionPanel::default(),
            terrain_projection: Default::default(),
            erosion_device: Default::default(),
            erosion_comparison: Default::default(),
            nodes: Default::default(),
            map_colors: Default::default(),
            zoom: 1.,
            pan: Vec2::ZERO,
            grid: false,
            show_no_build: false,
            monument_icons: Default::default(),
            installation_library: Default::default(),
            restrictions: Default::default(),
            show_monuments: true,
            show_decoration: true,
            show_preferences: false,
            drag_before: None,
            drag_origin: None,
            drag_press: None,
            drag_press_can_edit: false,
            placement_request: None,
            camera: terrain_3d::Camera::default(),
            drag_splat: None,
            skip_history: false,
            pending: None,
            allow_close: false,
            reset_layout: false,
            preferences: Default::default(),
            settings_path: None,
            settings_message: String::new(),
            settings_open: false,
            settings_page: Default::default(),
            recipe_settings_open: false,
            recipe_settings_tab: Default::default(),
            installation_setup_open: false,
            installation_roster_pending: path.is_none(),
            installation_placement_pending: false,
            restore_source: false,
        };
        app.load_preferences(&cc.egui_ctx);
        if app.preferences.check_for_updates {
            app.updates.check(&cc.egui_ctx, false);
        }
        if let Some(path)=path {app.open_project_from(path,&cc.egui_ctx);}
        else {app.prepare_installation_startup();}
        app
    }
    fn exportable_map(&self) -> Option<&egui::ColorImage> {
        self.exportable_overview().map(|(image, _)| image.as_ref())
    }

    // Accepted overview textures are replaced on render changes. A generated
    // tab can keep the exact texture without following later working edits.
    fn exportable_overview(&self) -> Option<(&Arc<egui::ColorImage>, Option<&egui::TextureHandle>)> {
        if !self.preview_current()
            || self.task.is_some()
            || self.placement_task.is_some()
            || self.drag_before.is_some()
        {
            return None;
        }
        if self.retained_composition().is_some() { return None; }
        if let Some(view) = self.composition.view(&self.project) {
            return Some(view.palette.as_ref().and_then(|palette|self.map_colors.view_for(
                &color_panel::Base::Composed(view.maps.clone()),palette,&self.preferences.map_colors))
                .map_or(
                    (&view.image, Some(&view.texture)),
                    |colors| (&colors.image, Some(&colors.texture)),
                ));
        }
        // Do not export a clean base while its accepted terrain edits are still composing.
        if self.project.composition_engine.is_some()
            && self.current_layout().is_some()
            && self
                .preview
                .as_ref()
                .is_some_and(|p| p.native_layers.is_some())
        {
            return None;
        }
        if let Some((base, palette)) = self.map_color_source()
            && let Some(view) = self.map_colors.view_for(&base, &palette, &self.preferences.map_colors) {
            return Some((&view.image, Some(&view.texture)));
        }
        self.preview
            .as_ref()
            .map(|p| (p.display_image.as_ref().unwrap_or(&p.image), self.texture.as_ref()))
    }
    fn export_source(&self) -> Option<export_panel::Source> {
        self.exportable_map()?;
        if let Some(view) = self.composition.view(&self.project) {
            return Some(export_panel::Source::Composed {maps: view.maps.clone(),palette: view.palette.as_ref().map(|p|std::sync::Arc::new(p.with_display_colors(&self.preferences.map_colors)))});
        }
        let preview = self.preview.as_ref()?;
        if let Some(layers) = &preview.native_layers {
            return Some(export_panel::Source::Native {
                palette: preview.palette.as_ref().map(|p|std::sync::Arc::new(p.with_display_colors(&self.preferences.map_colors))),
                layers: layers.clone(),
                ocean: preview.ocean.clone(),
            });
        }
        if let Some(land) = &preview.landscape {
            return Some(export_panel::Source::Independent {
                land: land.clone(),
                size: preview.key.size,
                seed: preview.key.biome_seed.unwrap_or(preview.key.seed),
            });
        }
        Some(export_panel::Source::Legacy(preview.image.clone()))
    }
    fn export_map_image(&mut self, ctx: &egui::Context) {
        if self.image_export.busy() || !self.image_export_ready() {
            return;
        }
        let recipe=self.image_export_recipe().unwrap();
        let name = format!("map-{}-{}.png", recipe.size, recipe.master_seed);
        let directory = export_panel::dialog_directory(
            self.preferences.export_folder.as_deref(),
            self.path.as_deref(),
        )
            .or_else(|| {
                std::env::var_os("USERPROFILE")
                    .map(PathBuf::from)
                    .map(|p| p.join("Pictures"))
                    .filter(|p| p.is_dir())
            })
            .or_else(|| std::env::current_dir().ok());
        let mut dialog = rfd::FileDialog::new();
        if let Some(directory) = directory {
            dialog = dialog.set_directory(directory);
        }
        let Some(mut path) = dialog
            .set_title("Export map image")
            .add_filter("PNG image", &["png"])
            .set_file_name(name)
            .save_file()
        else {
            return;
        };
        if path.extension().is_none() {
            path.set_extension("png");
        }
        if !path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("png"))
        {
            self.status = "Map images export as PNG. Choose a .png filename.".into();
            return;
        }
        if let Some((source,scene)) = self.image_export_input() {
            let resolution=self.image_export_legacy_size().map_or(self.preferences.export_resolution.pixels(),|size|size[0]);
            self.image_export.start_render(path,source,resolution,Some(scene),self.preferences.image_render.clone(),ctx);
        }
    }
    fn dirty(&self) -> bool {
        self.document_recipe() != &self.saved || self.generation.revision!=self.project_io.saved_generation
    }
    fn pristine_new_project(&self) -> bool {
        self.path.is_none() && !self.history.can_undo() && !self.history.can_redo() && !self.dirty()
    }
    fn accept_new_project_defaults(&mut self, pristine: bool) {
        if pristine
            && self.path.is_none()
            && self.generation.revision == self.project_io.saved_generation
        {
            if self.saved != self.project {
                self.saved.clone_from(&self.project);
            }
            self.history = History::default();
        }
    }
    fn apply_prepared_recipe_defaults(&mut self) {
        if self.generation.busy() {
            return;
        }
        let catalog = self.prefabs.catalog(&self.project);
        if let (Some(root), Some(catalog)) = (
            self.source.bound_root_ref(&self.project),
            catalog.as_deref(),
        ) {
            self.decoration_panel.bind_cliffs(&mut self.project, root, &catalog.sha256);
        }
        mapper_studio::generation_defaults::apply(&mut self.project);
        let needs_terrain_defaults = self.project.monument_rules.is_none();
        self.monuments.apply_defaults(&mut self.project);
        if needs_terrain_defaults {
            self.monuments.apply_terrain_defaults(&mut self.project, catalog.as_deref());
        }
        if let Err(error) = self.roads.refresh_defaults(&mut self.project) {
            self.status = error;
        }
    }
    fn preview_current(&self) -> bool {
        self.preview
            .as_ref()
            .is_some_and(|p| p.matches_project(&self.project))
    }
    fn preview_presentation(&self) -> preview::Presentation {
        let draft = self.document_recipe();
        // A queued terrain variant won't be retained as the recipe preview.
        // If that preview is hidden, its existing thumbnail is enough until
        // composition creates the detailed image for the finished map tab.
        let hidden_variant = self.generation.busy()
            && self.project.composition_engine.is_some()
            && mapper_studio::native_preview::is_native(&self.project.preview_engine)
            && !self.working_map_visible()
            && (draft.terrain_key() != self.project.terrain_key()
                || mapper_studio::native_processing::rivers_follow_terrain(draft)
                    != mapper_studio::native_processing::rivers_follow_terrain(&self.project));
        preview::Presentation {
            live: self.live_generation,
            detailed: !hidden_variant,
        }
    }

    fn request_preview(&mut self, ctx: &egui::Context, reuse_render: bool) {
        if !self.generation.busy() {
            let catalog = self.prefabs.catalog(&self.project);
            if let Err(error) = self.monuments.refresh_saved_roster(&mut self.project, catalog.as_deref()) {
                self.status = error;
                return;
            }
        }
        if self.task.is_none() {
            let root = self.source.bound_root(&self.project);
            let palette = self.prefabs.prepared_palette(&self.project, root.as_deref());
            self.task = Some(Task::start(
                &self.project,
                self.preview_presentation(),
                root,
                self.preview.as_ref(),
                palette,
                reuse_render,
                ctx.clone(),
            ));
            self.clear_stage_preview();
            self.generation_progress = None;
        }
    }
    fn poll(&mut self, ctx: &egui::Context) {
        let Some(task) = &self.task else {
            return;
        };
        let key = self.project.terrain_key();
        let stale = task.key != key
            || task.ordered_terrain
                != mapper_studio::native_processing::rivers_follow_terrain(&self.project)
            || task.cancel.load(Ordering::Relaxed);
        if stale {
            task.cancel.store(true, Ordering::Relaxed);
            self.stage_image = None;
            self.stage_texture = None;
            self.generation_progress = None;
        }
        if let Some(update) = task.progress.lock().ok().and_then(|mut slot| slot.take())
            && update.key == key
            && !stale
            && !task.cancel.load(Ordering::Relaxed)
        {
            self.generation_progress = Some(update.label);
            if self.live_generation
                && let Some(image) = update.image
            {
                self.stage_image = Some(image);
            }
        }
        let result = match task.receiver.try_recv() {
            Ok(r) => Some(r),
            Err(TryRecvError::Disconnected) => Some(Err("Worker ended unexpectedly.".into())),
            Err(TryRecvError::Empty) => None,
        };
        if let Some(result) = result {
            let cancelled = task.cancel.load(Ordering::Relaxed);
            self.task = None;
            self.clear_stage_preview();
            self.generation_progress = None;
            match result {
                Ok(Some(p)) if !cancelled && p.matches_project(&self.project) => {
                    self.layer_texture = None;
                    self.drag_splat = None;
                    if p.ocean.is_none()
                        && matches!(
                            self.native_view,
                            mapper_studio::native_view::View::Water
                                | mapper_studio::native_view::View::ProcessedTopology
                        )
                    {
                        self.native_view = mapper_studio::native_view::View::Overview;
                    }
                    if self.texture.is_none() || self.preview.as_ref().is_none_or(|previous| {
                        !Arc::ptr_eq(previous.viewport_image(), p.viewport_image())
                    }) {
                        self.texture = Some(ctx.load_texture(
                            "terrain-preview",
                            p.viewport_image().clone(),
                            egui::TextureOptions::LINEAR,
                        ));
                    }
                    self.preview = Some(p);
                    self.generation_terrain_ready();
                    self.status =
                        "Terrain preview ready. Searching for monument placements…".into();
                    self.start_placement(ctx);
                }
                Ok(_) => {
                    self.status =
                        "Preview discarded: cancelled or recipe changed. Existing preview retained."
                            .into()
                }
                Err(e) if !cancelled => {
                    self.report_generation_worker_failure(format!("Preview failed: {e}"), ctx);
                }
                Err(_) => {}
            }
        }
    }
    fn start_placement(&mut self, ctx: &egui::Context) {
        if self.task.is_some() || self.placement_task.is_some() || !self.preview_current() {
            return;
        }
        if self.deferred_generation_placement() {
            self.status = "Terrain ready. Building map…".into();
            return;
        }
        if mapper_studio::placement_preview::is_earthwork(&self.project.placement_engine)
            && self.prefabs.bindings(&self.project).is_none()
        {
            self.status="Terrain ready. Waiting for automatic monument asset verification before placement…".into();
            return;
        }
        if let Some(preview) = &self.preview {
            self.placement_task = Some(LayoutTask::start(
                self.project.clone(),
                preview,
                ctx.clone(),
                self.prefabs
                    .catalog(&self.project)
                    .zip(self.prefabs.bindings(&self.project)),
                self.monuments.prepared.clone(),
            ));
            self.status =
                if mapper_studio::placement_preview::is_earthwork(&self.project.placement_engine) {
                    "Searching source-authored monument terrain and bounds…"
                } else {
                    "Searching preferred monument locations using catalog footprint estimates…"
                }
                .into();
        }
    }
    fn requested_footprint(&self) -> bool {
        let Some(request) = self
            .placement_request
            .as_ref()
            .filter(|r| r.recipe == self.project)
        else {
            return false;
        };
        let Some(target) = self
            .selected
            .and_then(|i| self.project.targets.get(i))
            .filter(|t| t.instance_id == request.id && t.presence != Presence::Excluded)
        else {
            return false;
        };
        let position = self
            .current_layout()
            .and_then(|l| l.solution.outcomes.iter().find(|o| o.id == request.id))
            .and_then(|o| o.position.as_ref());
        position.is_none_or(|p| {
            (p.x - target.x).abs() > 0.01
                || (p.z - target.z).abs() > 0.01
                || target
                    .yaw
                    .is_some_and(|y| ((p.yaw - y + 180.).rem_euclid(360.) - 180.).abs() > 0.01)
        })
    }
    /// Material views already contain accepted paint. Reapplying a translucent
    /// selection preview there would strengthen it merely by selecting a target.
    fn selected_stamp_is_composed(&self) -> bool {
        if self.drag_before.is_some()
            || self.stage_texture.is_some()
            || self.composition.view(&self.project).is_none()
            || !matches!(self.native_view,
                mapper_studio::native_view::View::Overview | mapper_studio::native_view::View::Ground)
        {
            return false;
        }
        let Some(target) = self.selected.and_then(|i| self.project.targets.get(i)) else {
            return false;
        };
        let Some(stamp) = target.prefab_path.as_deref().and_then(|p| self.prefabs.stamp(&self.project, p)) else {
            return false;
        };
        self.current_layout()
            .and_then(|l| l.solution.outcomes.iter().find(|o| o.id == target.instance_id))
            .and_then(|o| o.position.as_ref())
            .is_some_and(|p| {
                (p.x - target.x).abs() <= 0.01
                    && (p.z - target.z).abs() <= 0.01
                    && ((p.yaw - target.yaw.unwrap_or(stamp.default_yaw) + 180.).rem_euclid(360.) - 180.).abs() <= 0.01
            })
    }
    /// Shared by drawing and picking: speculative paint is only present for
    /// the selected target when the requested preview is actually shown.
    fn selected_stamp_overlay_visible(&self) -> bool {
        self.preview_current() && !self.selected_stamp_is_composed()
            && (self.drag_before.is_some() || self.prefabs.show_stamp
                || (self.show_monuments && self.requested_footprint()))
    }
    fn source_stamp_sampling(&self)->bool {
        papyrus_terrain::monument_composition::source_textures(self.project.composition_engine.as_deref())
    }
    fn legacy_stamp_overlays_visible(&self) -> bool {
        self.show_monuments
            && !self.camera.enabled
            && matches!(
                self.native_view,
                mapper_studio::native_view::View::Overview | mapper_studio::native_view::View::Ground
            )
            && !self.source_stamp_sampling()
            && !self.terrain_projection.ready()
            && self.preview_current()
            && self.composition.view(&self.project).is_none()
    }

    fn working_map_visible(&self) -> bool {
        self.dock.find_tab(&Tab::Map).is_some_and(|path| {
            self.dock.leaf(path.node_path()).is_ok_and(|leaf| leaf.active == path.tab)
        })
    }

    fn accepted_stamp_visible(&self, index: usize) -> bool {
        if !self.preview_current() || !matches!(self.native_view,
            mapper_studio::native_view::View::Overview | mapper_studio::native_view::View::Ground) {
            return false;
        }
        if self.retained_composition().is_some() { return true; }
        if self.terrain_projection.ready() {return self.selected!=Some(index);}
        if self.composition.view(&self.project).is_some() {
            // Accepted paint is baked into the composed material image, unless
            // a generation stage or clean drag background is replacing it.
            return self.stage_texture.is_none() && self.drag_before.is_none();
        }
        if self.source_stamp_sampling() { return false; }
        self.show_monuments && !(self.selected == Some(index)
            && (self.drag_before.is_some() || self.prefabs.show_stamp)
            && (!self.requested_footprint() || self.drag_before.is_some()))
    }
    fn displayed_placements(&self) -> Vec<(u64,papyrus_terrain::placement::Position)> {
        if let Some(view) = self.retained_composition() {
            return view.solution.outcomes.iter().filter_map(|o|
                o.position.as_ref().map(|p|(o.id,p.clone()))).collect();
        }
        let display_layout = self
            .preview_current()
            .then(|| {
                self.current_layout().or_else(|| {
                    self.layouts.iter().rev().find(|l| {
                        l.recipe.terrain_key() == self.project.terrain_key()
                            && self.layout_assets_match(l)
                    })
                })
            })
            .flatten();
        display_layout
            .map(|layout| {
                layout
                    .solution
                    .outcomes
                    .iter()
                    .filter_map(|o| {
                        let old = layout
                            .recipe
                            .targets
                            .iter()
                            .find(|t| t.instance_id == o.id)?;
                        let current = self
                            .project
                            .targets
                            .iter()
                            .find(|t| t.instance_id == o.id)?;
                        let unchanged = old == current;
                        let dragging = self.drag_before.as_ref().is_some_and(|before| {
                            before.targets.iter().find(|t| t.instance_id == o.id) == Some(old)
                        });
                        (unchanged || dragging)
                            .then(|| o.position.as_ref().map(|p| (o.id, p.clone())))
                            .flatten()
                    })
                    .collect()
            })
            .unwrap_or_default()

    }
    fn retained_composition(&self) -> Option<&composition_panel::View> {
        if !self.preview_current() || self.stage_texture.is_some() || self.task.is_some()
            || self.camera.enabled || self.topology_tool.enabled
            || self.native_view != mapper_studio::native_view::View::Overview
            || (self.drag_before.is_none() && self.composition.view(&self.project).is_some()) {
            return None;
        }
        let catalog = self.prefabs.catalog(&self.project)?;
        let bindings = self.prefabs.bindings(&self.project)?;
        let palette=self.preview.as_ref()?.palette.as_ref().map(|p|p.source_render_key());
        self.composition.preference_view(&self.project,&catalog.sha256,&bindings.sha256,palette.as_deref())
    }
    fn map_background_texture(&self) -> Option<&egui::TextureHandle> {
        if let Some(view) = self.retained_composition() {
            return Some(view.palette.as_ref().and_then(|palette|self.map_colors.view_for(
                &color_panel::Base::Composed(view.maps.clone()),palette,&self.preferences.map_colors))
                .map_or(&view.texture,|colors|&colors.texture));
        }
        self.terrain_projection.texture().or(self.drag_splat.as_ref().filter(|_| self.drag_before.is_some()))
            .or(self.topology_tool.enabled.then(||self.topology_tool.pending_texture(&self.project)).flatten())
            .or(self.stage_texture.as_ref())
            .or(self.layer_texture.as_ref())
            .or(self.map_colors.view().map(|v|&v.texture))
            .or(self.composition.view(&self.project).map(|v|&v.texture))
            .or(self.texture.as_ref())
    }
    fn map_palette(&self)->Option<&std::sync::Arc<mapper_studio::terrain_palette::Palette>> {
        if self.stage_texture.is_some() {return None;}
        if let Some(view)=self.retained_composition() {
            return view.palette.as_ref().map(|palette|self.map_colors.view_for(
                &color_panel::Base::Composed(view.maps.clone()),palette,&self.preferences.map_colors)
                .map_or(palette,|colors|&colors.palette));
        }
        if let Some(view)=self.map_colors.view(){return Some(&view.palette);}
        if self.drag_before.is_none() && let Some(view)=self.composition.view(&self.project) {
            return view.palette.as_ref();
        }
        self.preview.as_ref().and_then(|p|p.palette.as_ref())
    }
    fn displayed_composition(&self)->Option<&composition_panel::View> {
        self.retained_composition().or_else(||(self.drag_before.is_none() && self.stage_texture.is_none()).then(||self.composition.view(&self.project)).flatten())
    }
    fn map_stamp_context(&self)->prefab_panel::StampContext<'_> {
        let view=self.displayed_composition();
        prefab_panel::StampContext {
            base:view.map(|v|&v.base_layers).or_else(||self.preview.as_ref().and_then(|p|p.native_layers.as_ref())),
            composed:view.map(|v|&v.maps),
            palette:self.map_palette(),
            base_surface:view.and_then(|v|v.base_surface.as_ref()).or_else(||self.preview.as_ref().and_then(|p|p.surface.as_ref())),
            composed_surface:view.and_then(|v|v.palette.as_ref().and_then(|palette|self.map_colors.view_for(
                &color_panel::Base::Composed(v.maps.clone()),palette,&self.preferences.map_colors)).and_then(|c|c.surface.as_ref()).or(v.surface.as_ref())),
        }
    }
    fn projection_request(&self)->Option<projection_panel::Request> {
        if self.camera.enabled || self.topology_tool.enabled || self.stage_texture.is_some()
            || self.task.is_some() || !self.selected_stamp_overlay_visible()
            || (self.drag_before.is_none() && (self.composition.view(&self.project).is_some()
                || self.native_view!=mapper_studio::native_view::View::Overview))
            || !self.project.composition_engine.as_deref().is_some_and(papyrus_terrain::monument_composition::supported)
            || !self.project.performance.monuments {return None;}
        let preview=self.preview.as_ref()?;let layers=preview.native_layers.clone()?;
        let target=self.selected.and_then(|i|self.project.targets.get(i))?;
        if target.presence==Presence::Excluded {return None;}
        let path=target.prefab_path.as_deref()?;let stamp=self.prefabs.stamp(&self.project,path)?;
        let others=self.displayed_placements().into_iter().filter(|(id,_)|*id!=target.instance_id).filter_map(|(id,position)| {
            let other=self.project.targets.iter().find(|t|t.instance_id==id)?;
            Some(projection_panel::Other{id,path:other.prefab_path.clone()?,position})
        }).collect();
        Some(projection_panel::Request {project:self.project.clone(),layers,ocean:preview.ocean.clone(),palette:self.map_palette().cloned(),
            catalog:self.prefabs.catalog(&self.project)?,bindings:self.prefabs.bindings(&self.project)?,
            id:target.instance_id,path:path.into(),pose:[target.x,target.z,target.yaw.unwrap_or(stamp.default_yaw)],others})
    }
    fn requested_footprint_status(&self) -> Option<String> {
        if !self.show_monuments || !self.requested_footprint() {
            return None;
        }
        let target = self.selected.and_then(|i| self.project.targets.get(i))?;
        if self.placement_task.is_some() {
            return Some("Requested footprint · searching for a feasible placement…".into());
        }
        let outcome = self.current_layout().and_then(|l| {
            l.solution
                .outcomes
                .iter()
                .find(|o| o.id == target.instance_id)
        });
        Some(match outcome {
            Some(o) if o.position.is_none() => {
                format!("Requested footprint · not placed: {}", o.reason)
            }
            Some(_) => "Requested footprint · accepted placement is at the blue marker".into(),
            None => "Requested footprint · not applied; regenerate monuments to evaluate".into(),
        })
    }
    fn layout_assets_match(&self, layout: &Layout) -> bool {
        if !mapper_studio::placement_preview::is_earthwork(&layout.recipe.placement_engine) {
            return layout.assets.is_none();
        }
        self.prefabs
            .catalog(&self.project)
            .zip(self.prefabs.bindings(&self.project))
            .is_some_and(|(c, b)| layout.assets.as_ref().is_some_and(|[catalog, bindings]| {
                catalog == &c.sha256 && bindings == &b.sha256
            }))
    }
    fn placement_recipe_matches(&self, recipe: &Project) -> bool {
        // Enumerate the entire recipe so a new field requires an explicit
        // dependency decision. Network, scenery and brush edits reuse placement.
        let Project {
            schema, installation, native_profile, terrain_overrides, biome_overrides, gpu_erosion, terrain_graph,
            seed_derivation, preview_engine,
            placement_engine, master_seed, size, seeds, targets, performance, options,
            composition_engine, monument_assets, objects, monument_rules, rivers,
            roads: _, rails: _, decoration: _, topology_edits: _, monument_addons: _,
            monument_overlay_choices: _, path_edits: _, custom_monuments: _,
        } = recipe;
        let current = &self.project;
        current.schema == *schema
            && &current.installation == installation
            && &current.native_profile == native_profile
            && &current.gpu_erosion == gpu_erosion
            && &current.terrain_graph == terrain_graph
            && ((&current.terrain_overrides == terrain_overrides
                && &current.biome_overrides == biome_overrides)
                || current.effective_native_profile() == recipe.effective_native_profile())
            && current.seed_derivation == *seed_derivation
            && &current.preview_engine == preview_engine
            && &current.placement_engine == placement_engine
            && current.master_seed == *master_seed
            && current.size == *size
            && current.seeds[..3] == seeds[..3]
            && current.seeds[4] == seeds[4]
            && &current.targets == targets
            && &current.performance == performance
            && current.options.early_placement_key() == options.early_placement_key()
            && current.options.river_limit == options.river_limit
            && &current.composition_engine == composition_engine
            && &current.monument_assets == monument_assets
            && &current.objects == objects
            && &current.monument_rules == monument_rules
            && &current.rivers == rivers
    }
    fn current_layout(&self) -> Option<&Layout> {
        self.current_layout_index().map(|index| &self.layouts[index])
    }
    fn current_layout_index(&self) -> Option<usize> {
        let eligible = |layout: &Layout| {
            self.placement_recipe_matches(&layout.recipe)
                && self.layout_assets_match(layout)
                && self.preview_current()
                && self.task.is_none()
        };
        self.active_layout
            .filter(|&index| self.layouts.get(index).is_some_and(eligible))
            .or_else(|| self.layouts.iter().rposition(eligible))
    }
    fn layout_status(layout: &Layout) -> String {
        let placed = layout
            .solution
            .outcomes
            .iter()
            .filter(|o| o.position.is_some())
            .count();
        let budget = if layout
            .solution
            .outcomes
            .iter()
            .any(|o| o.reason.contains("Search budget exhausted"))
        {
            " Search budget reached; unassessed monuments remain unresolved."
        } else {
            ""
        };
        format!(
            "Layout seed {}: {} placed; {} required missing. {}; game validation pending.{budget}",
            layout.recipe.seeds[4].value,
            placed,
            layout.solution.required_missing,
            if mapper_studio::placement_preview::is_earthwork(&layout.recipe.placement_engine) {
                "Source-authored terrain checks"
            } else {
                "Estimated footprints"
            }
        )
    }
    fn refresh_layout_status(&mut self) {
        self.status = self
            .current_layout()
            .map(Self::layout_status)
            .unwrap_or_else(|| {
                "Recipe restored. Rebuild stale terrain or regenerate placements to apply it."
                    .into()
            });
    }
    fn poll_placement(&mut self, ctx: &egui::Context) {
        let Some(task) = &self.placement_task else {
            return;
        };
        let result = match task.receiver.try_recv() {
            Ok(r) => Some(r),
            Err(TryRecvError::Disconnected) => Some(Err("Worker ended unexpectedly.".into())),
            Err(TryRecvError::Empty) => None,
        };
        if let Some(result) = result {
            let cancelled = task.cancel.load(Ordering::Relaxed);
            self.placement_task = None;
            match result {
                Ok(Some(layout)) if !cancelled && layout.recipe==self.project && self.layout_assets_match(&layout) && self.preview_current()=>{
                    if !self.generation.busy() {self.generation.revision+=1;}
                    self.status=Self::layout_status(&layout);
                    self.store_layout(layout);
                },
                Ok(_)=>self.status="Placement result discarded: cancelled or recipe changed. Earlier candidates retained.".into(),
                Err(e) if !cancelled => {
                    self.report_generation_worker_failure(format!("Placement search failed: {e}"), ctx);
                }
                Err(_) => {}
            }
        }
    }
    fn store_layout(&mut self, layout: Layout) {
        if let Some(index) = self.layouts.iter().position(|old| old.recipe == layout.recipe) {
            self.layouts[index] = layout;
            self.active_layout = Some(index);
        } else {
            self.layouts.push(layout);
            if self.layouts.len() > 6 {
                self.layouts.remove(0);
            }
            self.active_layout = Some(self.layouts.len() - 1);
        }
    }
    fn save(&mut self, save_as: bool, ctx: &egui::Context) {
        if self.project_io.busy() {
            return;
        }
        let path = if save_as || self.path.is_none() {
            rfd::FileDialog::new()
                .add_filter("Mapper project", &["mapper"])
                .set_file_name("world.mapper")
                .save_file()
        } else {
            self.path.clone()
        };
        let Some(path) = path else {
            return;
        };
        if let Err(error) = self.save_project_to(path, ctx) {
            self.project_io.report_issue(error.clone());
            self.status = format!("Save failed. {error}");
        }
    }
    fn request(&mut self, action: Action, ctx: &egui::Context) {
        if self.defer_project_action(action) {
            return;
        }
        // A file must open successfully before we ask to replace the current project.
        if !matches!(action, Action::Open) && self.dirty() {
            self.pending = Some(action)
        } else {
            self.perform(action, ctx)
        }
    }
    fn perform(&mut self, action: Action, ctx: &egui::Context) {
        match action {
            Action::Close => {
                self.plugins.cancel();
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Action::New => self.replace_project(Self::new_workbench_project(), None),
            Action::AcceptOpened => self.accept_opened_project(),
            Action::Open => {
                let Some(path) = rfd::FileDialog::new()
                    .add_filter("Mapper project", &["mapper"])
                    .pick_file()
                else {
                    return;
                };
                self.open_project_from(path, ctx);
            }
        };
    }
    fn replace_project(&mut self, p: Project, path: Option<PathBuf>) {
        self.nodes = Default::default();
        self.source.cancel_inspection();
        self.project_io.discard_opened();
        self.project_io.clear_issue();
        self.plugins.cancel();
        self.cancel_generation();
        self.generation = Default::default();
        self.project_io.saved_generation = 0;
        self.dock = layout();
        self.requested_tab = None;
        if let Some(task) = &self.task {
            task.cancel.store(true, Ordering::Relaxed);
        }
        if let Some(task) = &self.placement_task {
            task.cancel.store(true, Ordering::Relaxed);
        }
        self.layouts.clear();
        self.composition = Default::default();
        self.terrain_projection = Default::default();
        self.active_layout = None;
        self.project = p.clone();
        self.saved = p;
        self.restore_source = true;
        self.installation_roster_pending = path.is_none();
        self.installation_placement_pending = false;
        self.path = path;
        self.history = History::default();
        self.selected = (!self.project.targets.is_empty()).then_some(0);
        self.drag_before = None;
        self.drag_origin = None;
        self.drag_press = None;
        self.placement_request = None;
        self.preview = None;
        self.texture = None;
        self.clear_stage_preview();
        self.generation_progress = None;
        self.pan = Vec2::ZERO;
        self.drag_splat = None;
        self.camera.reset();
        self.zoom = 1.;
        self.skip_history = true;
        self.status = "Project opened. Build its terrain preview to begin.".into();
    }
    fn inspection_ocean(&self) -> Option<&papyrus_terrain::ocean::Ocean> {
        if let Some(composed) = self.composition.view(&self.project) {
            composed.maps.ocean.as_ref()
        } else {
            self.preview
                .as_ref()
                .filter(|_| self.preview_current())
                .and_then(|p| p.ocean.as_deref())
        }
    }
    fn publish_composed_layout(&mut self) {
        if let Some((input,published))=self.composition.take_published_layout(&self.project) {
            let Some(input) = input else {
                if self.deferred_generation_placement() && self.preview_current()
                    && self.layout_assets_match(&published)
                {
                    self.store_layout(published);
                } else {
                    self.composition.discard_context();
                }
                return;
            };
                let matches=|l:&Layout|self.placement_recipe_matches(&l.recipe) && self.layout_assets_match(l)
                    && mapper_studio::composition::layout_fingerprint(&l.solution)==input;
                let index=self.active_layout.filter(|&i|self.layouts.get(i).is_some_and(&matches))
                    .or_else(||self.layouts.iter().rposition(matches));
                if let Some(index)=index {
                    self.layouts[index].solution=published.solution;self.active_layout=Some(index);
                } else {self.composition.discard_context();}
            }
    }
    fn map_color_source(&self) -> Option<(color_panel::Base, std::sync::Arc<mapper_studio::terrain_palette::Palette>)> {
        if self.stage_texture.is_some() || !self.preview_current() {return None;}
        if let Some(view)=self.retained_composition() {
            return view.palette.as_ref().map(|p|(color_panel::Base::Composed(view.maps.clone()),p.clone()));
        }
        if self.drag_before.is_none() && let Some(view)=self.composition.view(&self.project) {
            view.palette.as_ref().map(|p|(color_panel::Base::Composed(view.maps.clone()),p.clone()))
        } else {
            self.preview.as_ref().and_then(|p|p.native_layers.as_ref().zip(p.palette.as_ref())
                .map(|(l,palette)|(color_panel::Base::Native(l.clone(),p.ocean.clone()),palette.clone())))
        }
    }
    fn refresh_map_colors(&mut self, ctx:&egui::Context) {
        let clean=self.preview.as_ref().and_then(|p|p.native_layers.as_ref().zip(p.palette.as_ref()).map(|(l,palette)|(color_panel::Base::Native(l.clone(),p.ocean.clone()),palette.clone())));
        self.map_colors.tick_with_clean(self.map_color_source(),clean,&self.preferences.map_colors,ctx);
    }
    fn clear_stage_preview(&mut self) {
        self.stage_image = None;
        self.stage_texture = None;
    }

    fn upload_stage_preview(&mut self, ctx: &egui::Context) {
        if let Some(image) = self.stage_image.take() {
            let options = egui::TextureOptions::LINEAR;
            if let Some(texture) = &mut self.stage_texture {
                texture.set(image, options);
            } else {
                self.stage_texture = Some(ctx.load_texture("terrain-stage", image, options));
            }
        }
    }

    fn map(&mut self, ui: &mut egui::Ui) {
        self.upload_stage_preview(ui.ctx());
        self.refresh_map_colors(ui.ctx());
        self.terrain_projection.tick(self.projection_request(),ui.ctx());
        let spawn_available = self
            .composition
            .view(&self.project)
            .is_some_and(|view| view.maps.spawn_audit.is_some());
        let paint_ready = spawn_available
            && !self.generation.busy()
            && self.preview_current()
            && self.task.is_none()
            && self.placement_task.is_none()
            && self.drag_before.is_none();
        if self.native_view == mapper_studio::native_view::View::SpawnCandidates && !spawn_available {
            self.native_view = mapper_studio::native_view::View::Overview;
            self.layer_texture = None;
        }
        if !self.camera.enabled && self.preview.as_ref().is_some_and(|p| p.native_layers.is_some()) {
            let availability = map_view_tabs::Availability {
                water: self.inspection_ocean().is_some(),
                spawns: spawn_available,
                painting: self.topology_tool.enabled,
                locked: self.drag_before.is_some(),
            };
            if map_view_tabs::reserve(ui, &mut self.native_view, availability) {
                self.layer_texture = None;
                self.drag_splat = None;
            }
        }
        let canvas_area = ui.available_rect_before_wrap();
        ui.scope_builder(
            egui::UiBuilder::new().id_salt("map-content").max_rect(canvas_area),
            |ui| {
                ui.shrink_clip_rect(canvas_area);
                self.map_content(ui, paint_ready);
            },
        );
    }

    fn map_content(&mut self, ui: &mut egui::Ui, paint_ready: bool) {
        let audit = self.topology_tool.enabled.then(|| {
            self.composition.view(&self.project).and_then(|view| view.maps.spawn_audit.as_ref())
        }).flatten();
        let paint_source = audit.filter(|_| paint_ready).map(|audit| (
            audit.source_sha256.clone(),
            audit.flags.iter().fold(0, |mask, flag| mask | flag.mask),
        ));
        let topology_flags: Vec<_> = audit.map(|audit| {
            audit.flags.iter().map(|flag| (flag.name.clone(), flag.mask)).collect()
        }).unwrap_or_default();
        if (!self.camera.enabled && self.preview.is_some()) && self.topology_tool.controls_with_layers(ui, &mut self.project, paint_ready, self.topology_bit,
            &topology_flags, &mut self.preferences.topology_groups) {
            self.save_preferences();
        }
        if self.topology_tool.take_overlay_change() { self.layer_texture=None; }
        if self.topology_tool.enabled {
            self.native_view = mapper_studio::native_view::View::Topology;
            self.camera.enabled = false;
        }
        let spawn_audit = self
            .composition
            .view(&self.project)
            .and_then(|c| c.maps.spawn_audit.as_ref());
        if !self.camera.enabled
            && let Some(layers) = self
                .composition
                .view(&self.project)
                .map(|v| &v.maps.layers)
                .or_else(|| {
                    self.preview
                        .as_ref()
                        .and_then(|p| p.native_layers.as_deref())
                })
        {
            let before = (self.native_view, self.topology_bit);
            if self.native_view != mapper_studio::native_view::View::Overview {
            ui.horizontal_wrapped(|ui| {
                ui.add_enabled_ui(!self.topology_tool.enabled || paint_ready, |ui| {
                    if matches!(
                        self.native_view,
                        mapper_studio::native_view::View::Topology
                            | mapper_studio::native_view::View::ProcessedTopology
                    ) {
                        ui.label("Bit");
                        ui.add(egui::DragValue::new(&mut self.topology_bit).range(0..=31));
                        ui.monospace(format!("0x{:08X}", 1_u32 << self.topology_bit));
                        if let Some(audit) = spawn_audit {
                            egui::ComboBox::from_id_salt("topology-source-name")
                                .selected_text(
                                    audit
                                        .flags
                                        .iter()
                                        .find(|f| f.mask == 1_u32 << self.topology_bit)
                                        .map_or("Unlabelled bit", |f| f.name.as_str()),
                                )
                                .show_ui(ui, |ui| {
                                    for flag in &audit.flags {
                                        ui.selectable_value(
                                            &mut self.topology_bit,
                                            flag.mask.trailing_zeros() as u8,
                                            &flag.name,
                                        );
                                    }
                                });
                        }
                    }
                    ui.small(if self.topology_tool.enabled {"White: all selected · Amber: some · Dark: none"}else{self.native_view.legend()});
                });
            });
            }
            if self.native_view == mapper_studio::native_view::View::Overview {
                self.layer_texture = None;
            } else if self.drag_before.is_none() && (before != (self.native_view, self.topology_bit)
                || self.layer_texture.is_none())
            {
                let rendered = if self.topology_tool.enabled {
                    mapper_studio::native_view::render_topology_selection(layers,self.topology_tool.selection_mask(self.topology_bit),mapper_studio::map_image::DISPLAY_RESOLUTION)
                } else if self.native_view == mapper_studio::native_view::View::SpawnCandidates
                {
                    mapper_studio::native_view::render_spawn_candidates(
                        spawn_audit.unwrap(),
                        mapper_studio::map_image::DISPLAY_RESOLUTION,
                    )
                } else {
                    mapper_studio::native_view::render_sized(
                        layers,
                        self.native_view,
                        self.topology_bit,
                        self.inspection_ocean(),
                        mapper_studio::map_image::DISPLAY_RESOLUTION,
                    )
                };
                match rendered {
                    Ok(image) => {
                        self.layer_texture = Some(ui.ctx().load_texture(
                            "native-layer",
                            image,
                            if matches!(
                                self.native_view,
                                mapper_studio::native_view::View::Topology
                                    | mapper_studio::native_view::View::ProcessedTopology
                                    | mapper_studio::native_view::View::Water
                                    | mapper_studio::native_view::View::SpawnCandidates
                            ) {
                                egui::TextureOptions::NEAREST
                            } else {
                                egui::TextureOptions::LINEAR
                            },
                        ))
                    }
                    Err(error) => {
                        self.layer_texture = None;
                        self.status = error;
                    }
                }
            }
        } else {
            self.layer_texture = None;
            self.drag_splat = None;
        }
        let map_font=self.monument_icons.labels.source(self.project.installation.as_ref());
        let viewport = ui.available_rect_before_wrap().intersect(ui.clip_rect());
        let compact = viewport.width() < 560.;
        let pending_move = self.drag_before.is_none() && self.placement_request.as_ref()
            .is_some_and(|request| request.recipe == self.project)
            && self.current_layout().is_none() && self.preview_current();
        egui::Area::new(ui.id().with("preview-toolbar"))
            .pivot(Align2::CENTER_TOP)
            .fixed_pos(egui::pos2(viewport.center().x, viewport.top() + 8.))
            .default_size(Vec2::ZERO)
            .constrain_to(viewport)
            .fade_in(false)
            .show(ui.ctx(), |ui| {
                workbench_style::map_glass_controls(ui);
                workbench_style::map_glass().show(ui, |ui| ui.horizontal(|ui| {
                    ui.add_enabled_ui(self.drag_before.is_none(), |ui| {
                        egui::Frame::new()
                            .fill(Color32::from_black_alpha(77))
                            .stroke(Stroke::new(1., Color32::from_white_alpha(40)))
                            .corner_radius(8)
                            .inner_margin(egui::Margin::same(2))
                            .show(ui, |ui| ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.;
                                for (enabled, label) in [(false, "2D"), (true, "3D")] {
                                    let selected = self.camera.enabled == enabled;
                                    let button = egui::Button::selectable(selected, label)
                                        .frame_when_inactive(selected).corner_radius(6);
                                    let button = if selected {
                                        button.fill(workbench_style::MAP_VIEW_SELECTED)
                                    } else {
                                        button
                                    };
                                    if ui.add_sized([34., 20.], button).clicked() {
                                        self.camera.enabled = enabled;
                                        ui.ctx().request_repaint();
                                    }
                                }
                            }));
                    });
                    ui.separator();
                    if ui.button("Fit").on_hover_text("Fit the whole map in the viewport.").clicked() {
                        self.zoom = 1.;
                        self.pan = Vec2::ZERO;
                        self.camera.reset();
                    }
                    if !self.camera.enabled && !compact {
                        ui.toggle_value(&mut self.grid, "Grid");
                        ui.toggle_value(&mut self.show_no_build, "No-build").on_hover_text(if self.restrictions.message.is_empty() {"Show building restrictions from the Rust installation."} else {&self.restrictions.message});
                    }
                    ui.menu_button("View", |ui| {
                        if !self.camera.enabled {
                            if compact {
                                ui.checkbox(&mut self.grid, "Grid");
                                ui.checkbox(&mut self.show_no_build, "No-build zones");
                            }
                            if map_label_panel::controls(ui, &mut self.preferences.monument_markers) { self.save_preferences(); }
                            ui.checkbox(&mut self.show_monuments, "Monument splats");
                            let mut paint = self.topology_tool.enabled;
                            if ui.add_enabled((paint_ready || paint) && self.drag_before.is_none(),
                                egui::Checkbox::new(&mut paint, "Paint topology")).changed()
                            {
                                self.topology_tool.set_enabled(paint);
                            }
                            ui.separator();
                        }
                        ui.menu_button("Controls", |ui| {
                            if self.camera.enabled {
                                ui.label("Hold right mouse to fly");
                                ui.label("W A S D to move · Q / E down / up · Shift to speed up");
                                ui.label("Scroll to adjust flight speed or move forward and back");
                                ui.label("Drag to look around · Esc releases the cursor");
                                if let Some(message) = self.monuments.model_status() { ui.small(message); }
                            } else if self.topology_tool.enabled {
                                ui.label("Drag to paint");
                                ui.label("Scroll to zoom");
                                ui.label("Escape to cancel a stroke");
                            } else {
                                ui.label("Drag the map to pan");
                                ui.label("Drag a monument to move");
                                ui.label("Scroll while dragging a monument to rotate it");
                            }
                            ui.label("Middle drag to pan");
                            ui.label("Fit to reset the view");
                        });
                    });
                    if self.preview.is_some() && !self.preview_current() {
                        ui.add(egui::Label::new(egui::RichText::new("Recipe changed").color(ui.visuals().warn_fg_color)).truncate())
                            .on_hover_text("Generate to update this map.");
                    } else if pending_move {
                        let ready = self.placement_task.is_none() && !self.generation.busy();
                        if ui.add_enabled(ready, egui::Button::new("Apply changes"))
                            .on_hover_text("Rebuild with the staged monument preferences.")
                            .clicked()
                        {
                            self.start_placement(ui.ctx());
                        }
                    } else if !self.camera.enabled && !compact {
                        ui.label(format!("{:.0}%", self.zoom * 100.));
                    }
                }));
            });
        if self.camera.enabled && self.topology_tool.enabled {
            self.topology_tool.enabled = false;
            self.topology_tool.cancel_stroke();
        }
        let placement_note = if !self.camera.enabled && (self.retained_composition().is_some() || self.terrain_projection.ready()
            || !self.terrain_projection.message().is_empty() || self.drag_before.is_some()) {
            let stamp_ready = self
                .selected
                .and_then(|i| self.project.targets.get(i))
                .and_then(|t| t.prefab_path.as_deref())
                .is_some_and(|path| self.prefabs.stamp(&self.project, path).is_some());
            let legend = if self.retained_composition().is_some() {
                "Previous map · Unapplied placement changes".into()
            } else if self.terrain_projection.ready() {
                if self.terrain_projection.message().is_empty() {
                    "Requested monument terrain preview · Roads and scenery update after placement".into()
                } else {
                    "Requested terrain preview · Placement needs adjustment · Regeneration searches nearby".into()
                }
            } else if !self.terrain_projection.message().is_empty() {
                "Requested material preview · Terrain fitting is unavailable at this preference".into()
            } else if let Some(message) = self.requested_footprint_status() {
                message
            } else if self.current_layout().is_none()
                && !self.layouts.is_empty()
                && self.preview_current()
            {
                "Previous unchanged footprints · Amber outlines · Regenerating invalidates acceptance; terrain unchanged".into()
            } else {
                (if stamp_ready && !self.selected_stamp_is_composed() && (self.drag_before.is_some() || self.prefabs.show_stamp) {
                "Selected monument splat preview · Terrain unchanged · Requested footprint"
            } else if self.drag_before.is_some() && self.drag_splat.is_some() {
                "Existing splat map · Installation assets are being prepared · Terrain unchanged"
            } else if self.composition.view(&self.project).is_some() { "Composed monument terrain · Dark patches: terrain holes · Footprints and game access need validation" }
            else if self.show_monuments && self.prefabs.bindings(&self.project).is_some() { "Monument ground stamps in 2D · Static model previews are available in 3D" } else { "Markers are placement preferences · Installation assets load automatically; see Settings" }).to_owned()
            };
            let legend=if self.preview.as_ref().is_some_and(|p|p.native_layers.is_some())
                && (self.native_view==mapper_studio::native_view::View::Overview || self.drag_before.is_some()) {
                format!("{legend} · {}",if self.map_colors.view().is_some(){"Custom map colors"}else if self.map_palette().is_some_and(|p|p.cartography().is_some()) {"Installation map colors"}else if self.terrain_projection.approximate().map_or(self.map_palette().is_some(),|a|!a) {"Installation terrain colors"}else{"Approximate terrain colors"})
            }else{legend};
            let caption = if self.retained_composition().is_some() { "Placement changes pending" }
                else if !self.terrain_projection.message().is_empty() { "Placement needs adjustment" }
                else if self.terrain_projection.ready() || self.drag_before.is_some() { "Placement preview" }
                else { "" };
            Some((caption,legend))
        } else {None};
        let (display_size, seed) = if self.stage_texture.is_some() {
            (self.project.size, Some(self.project.seeds[0].value))
        } else {
            self.preview.as_ref().map_or(
                (self.project.size, (self.project.master_seed != 0).then_some(self.project.seeds[0].value)),
                |preview| (preview.key.size, Some(preview.key.seed)),
            )
        };
        let badge_position = viewport.left_top()
            + egui::vec2(8., if viewport.width() < 710. { 50. } else { 8. });
        egui::Area::new(ui.id().with("preview-identity"))
            .fixed_pos(badge_position)
            .default_size(Vec2::ZERO)
            .constrain_to(viewport)
            .fade_in(false)
            .show(ui.ctx(), |ui| {
                workbench_style::map_glass_controls(ui);
                workbench_style::map_glass().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.strong(format!("{display_size} m"));
                        if let Some(seed) = seed {
                            ui.separator();
                            ui.label(format!("Seed {seed}"));
                        }
                    });
                    if let Some((caption, legend)) = &placement_note && !caption.is_empty() {
                        ui.small(*caption).on_hover_text(legend);
                    }
                }).response.on_hover_text(placement_note.as_ref()
                    .map_or("Map size and terrain seed", |(_, legend)| legend));
            });
        let (outer, _) =
            ui.allocate_exact_size(ui.available_size().max(Vec2::splat(20.)), Sense::hover());
        let response = ui.interact(outer, ui.id().with("map-canvas"), Sense::click_and_drag());
        let painter = ui.painter().with_clip_rect(outer);
        painter.rect_filled(outer, 0., self.viewport_background());
        if self.camera.enabled {
            self.terrain_projection.tick(None,ui.ctx());
            if let (Some(preview), Some(texture)) = (&self.preview, &self.texture) {
                let composed = self.composition.view(&self.project);
                let model_source = self.monuments.model_source().cloned();
                let mut models = Vec::new();
                if self.show_monuments
                    && let Some(source) = &model_source
                    && let Some(layout) = self.current_layout()
                {
                    for outcome in &layout.solution.outcomes {
                        let Some(position) = &outcome.position else {
                            continue;
                        };
                        let Some(path) = self
                            .project
                            .targets
                            .iter()
                            .find(|t| t.instance_id == outcome.id)
                            .and_then(|t| t.prefab_path.as_deref())
                        else {
                            continue;
                        };
                        if let Some(prefab) = source.prefabs.get(path)
                            && !prefab.model.triangles.is_empty()
                        {
                            models.push(terrain_3d::ModelInstance::yaw(
                                &prefab.model,
                                [position.x, position.y, position.z],
                                position.yaw,
                            ));
                        }
                    }
                }
                if let Some(view) = composed {
                    if self.show_monuments
                        && let Some(source) = &model_source
                        && let Some(offshore) = &view.maps.offshore
                    {
                        for instance in &offshore.instances {
                            if let Some(prefab) = source.prefabs.get(&instance.prefab)
                                && !prefab.model.triangles.is_empty()
                            {
                                models.push(terrain_3d::ModelInstance {
                                    model:terrain_3d::Geometry::Static(&prefab.model),
                                    position:instance.pose.position,rotation:instance.pose.rotation,
                                    scale:instance.pose.scale,tint:None,
                                });
                            }
                        }
                    }
                    models.extend(terrain_3d::surface_instances(&view.maps));
                }
                let scenery_map = composed.filter(|_| self.show_decoration).map(|view| &view.maps);
                models.extend(self.decoration_models.instances(scenery_map));
                let cables = if self.show_decoration {
                    composed.and_then(|view| match &view.cables {
                        Ok(Some(scene)) => Some(terrain_3d::Cables::Ready(&scene.report)),
                        Err(error) => Some(terrain_3d::Cables::Unavailable(error)),
                        Ok(None) => None,
                    })
                } else {
                    None
                };
                let refining = self.camera.draw_live(
                    ui,
                    outer,
                    &response,
                    self.map_colors.view().map_or_else(||composed.map_or(texture.id(), |v| v.texture.id()),|v|v.texture.id()),
                    self.map_colors.view().map_or_else(||composed.map_or(&preview.image, |v| &v.image),|v|&v.image),
                    composed.map_or(preview.heights.as_slice(), |v| v.heights.as_slice()),
                    preview.resolution,
                    preview.key.size,
                    &models,
                    composed.map(|v| (v.texture.id(), &v.maps, v.surface_bounds.as_slice())),
                    cables,
                );
                let status = refining.then_some("Refining terrain…").or_else(|| self.monuments.model_status()
                    .filter(|message| message.starts_with("Preparing"))
                    .or_else(|| self.decoration_models.loading_status()));
                let hint = if outer.width() < 390. {
                    "Hold right mouse to fly"
                } else {
                    "Hold right mouse to fly · W A S D · Q / E · Shift"
                };
                let mut y = outer.bottom() - 10.;
                for caption in std::iter::once(hint).chain(status) {
                    let text = painter.layout_no_wrap(caption.to_owned(), FontId::proportional(12.), Color32::WHITE);
                    let bounds = Rect::from_min_size(egui::pos2(outer.left() + 9., y - text.size().y - 6.),
                        text.size() + egui::vec2(12., 6.));
                    painter.rect_filled(bounds, 5, Color32::from_black_alpha(190));
                    painter.galley(bounds.min + egui::vec2(6., 3.), text, Color32::WHITE);
                    y = bounds.top() - 4.;
                }
            } else {
                painter.text(
                    outer.center(),
                    Align2::CENTER_CENTER,
                    "Generate a map to inspect it in 3D",
                    FontId::proportional(16.),
                    Color32::LIGHT_GRAY,
                );
            }
            return;
        }
        if self.topology_tool.enabled
            && self.composition.view(&self.project).is_some()
            && let Some(texture) = &self.layer_texture
        {
            self.topology_tool.remember(&self.project, texture);
        }
        let Some(texture_id) = self.map_background_texture().map(|texture|texture.id())
        else {
            painter.text(
                outer.center(),
                Align2::CENTER_CENTER,
                "Generate a map to begin",
                FontId::proportional(16.),
                Color32::LIGHT_GRAY,
            );
            return;
        };
        if response.hovered()
            && self.drag_before.is_none()
            && !response.dragged_by(egui::PointerButton::Primary)
            // A press/move/release may arrive together before egui promotes
            // the gesture to a drag. Keep its screen-to-world transform fixed;
            // the monument handler below consumes its wheel input as rotation.
            && !ui.input(|i| {
                i.pointer.primary_down()
                    || i.pointer.button_pressed(egui::PointerButton::Primary)
                    || i.pointer.button_released(egui::PointerButton::Primary)
            })
        {
            let scroll = terrain_3d::wheel_delta(ui);
            if scroll != 0. {
                let old = self.zoom;
                self.zoom = (self.zoom * (scroll * 0.002).exp()).clamp(0.5, 8.);
                if let Some(p) = response.hover_pos() {
                    self.pan = (self.pan + outer.center().to_vec2() - p.to_vec2())
                        * (self.zoom / old)
                        + p.to_vec2()
                        - outer.center().to_vec2();
                }
            }
        }
        if response.dragged_by(egui::PointerButton::Middle) {
            self.pan += ui.input(|input| input.pointer.delta());
        }
        let available=outer.width().min(outer.height());
        let fit=self.displayed_composition().map_or(1.,|view|map_monuments::fit_scale(self.project.size,&view.generated_monuments,available));
        let side = available * self.zoom * fit;
        let map = Rect::from_center_size(outer.center() + self.pan, Vec2::splat(side));
        let background_shape = painter.image(
            texture_id,
            map,
            Rect::from_min_max(Pos2::ZERO, egui::pos2(1., 1.)),
            Color32::WHITE,
        );
        let size = if self.stage_texture.is_some() {
            self.project.size
        } else {
            self.preview
                .as_ref()
                .map_or(self.project.size, |p| p.key.size)
        } as f32;
        let screen = |x: f32, z: f32| {
            egui::pos2(
                map.left() + (x / size + 0.5) * side,
                map.top() + (0.5 - z / size) * side,
            )
        };
        self.topology_tool.paint(
            ui,
            &response,
            map,
            &mut self.project,
            self.topology_bit,
            paint_source,
        );
        // egui may never promote a short gesture to a drag if movement and
        // release arrive in one frame. Retain only presses owned by this map.
        let editing_ready = self.preview_current() && self.task.is_none() && !self.generation.busy();
        if !editing_ready {self.drag_press_can_edit=false;}
        let (presses, released_at, pointer_gone, focused, primary_down) = ui.input(|input| {
            let mut presses = Vec::new();
            let mut release = None;
            let mut pointer_gone = false;
            for event in &input.raw.events {
                match event {
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        ..
                    } => {
                        if *pressed {
                            if response.rect.contains(*pos) {
                                presses.push(*pos);
                            }
                        } else {
                            release = Some(*pos);
                        }
                    }
                    egui::Event::PointerGone => pointer_gone = true,
                    _ => {}
                }
            }
            (presses, release, pointer_gone, input.focused, input.pointer.primary_down())
        });
        if self.drag_before.is_none()
            && !self.topology_tool.enabled
            && let Some(pos) = presses.into_iter().rev()
                .find(|pos| ui.ctx().layer_id_at(*pos) == Some(ui.layer_id()))
        {
            self.drag_press = Some(pos);
            self.drag_press_can_edit = editing_ready;
        }
        let pointer_lost = pointer_gone || !focused || (released_at.is_none() && !primary_down);
        if pointer_lost {
            if let Some(before) = self.drag_before.take() {
                self.project = before;
                self.skip_history = true;
                self.status = "Target move cancelled: pointer capture lost.".into();
            }
            self.drag_press = None;
            self.drag_origin = None;
            self.drag_splat = None;
        }
        let quick_drag = !pointer_lost
            && self
                .drag_press
                .zip(released_at)
                .is_some_and(|(start, end)| {
                    start.distance(end) > ui.ctx().options(|o| o.input_options.max_click_dist)
                });
        // Selection remains available while terrain is stale or rebuilding.
        // Only an editable, owned press can promote to a map mutation.
        let can_start_drag = editing_ready && self.drag_press.is_some() && self.drag_press_can_edit;
        let picking = !self.topology_tool.enabled
            && self.drag_before.is_none()
            && (response.clicked()
                || (can_start_drag && (response.drag_started_by(egui::PointerButton::Primary)
                || quick_drag)));
        let placements = if picking || self.drag_before.is_some() {
            self.displayed_placements()
        } else {
            Vec::new()
        };
        if picking
            && let Some(pos) = self.drag_press.or_else(|| {
                ui.input(|i| i.pointer.press_origin())
                    .or(response.interact_pointer_pos())
            })
        {
            // Accepted center handles are drawn only for a current layout.
            // Retained, visible paint remains pickable independently of handles.
            let accepted_markers_visible=self.current_layout().is_some();
            let hidden_map_text = |path| self.preferences.monument_markers == mapper_studio::map_labels::Mode::Text
                && !self.restrictions.shows_map_text(path);
            // A first click must use the same artwork/fallback as this frame's
            // paint, even if the icon has not been drawn previously.
            for t in &self.project.targets {
                if t.presence!=Presence::Excluded && !hidden_map_text(t.prefab_path.as_deref()) {
                    self.monument_icons.prepare_hit(ui.ctx(),t.prefab_path.as_deref(),&self.preferences,self.settings_path.as_deref(),map_label_panel::Text{name:&self.monument_icons.name(t.prefab_path.as_deref(),&t.name),source:map_font.as_ref()});
                }
            }
            // A retained complete world also contains baked paint. Its picker
            // must sample that displayed world after preferences invalidate the
            // current composition, without requiring an overlay texture cache.
            let stamp_context = self.map_stamp_context();
            let nearest = self
                .project
                .targets
                .iter()
                .enumerate()
                .filter(|(_, t)| t.presence != Presence::Excluded)
                .map(|(i, t)| {
                    let accepted_order = placements
                        .iter()
                        .position(|(id, _)| *id == t.instance_id);
                    let accepted = accepted_order.map(|order| &placements[order].1);
                    let requested_marker_visible=self.selected==Some(i) || self.show_preferences;
                    let marker_hit=|center: Pos2,requested| if hidden_map_text(t.prefab_path.as_deref()) {
                        requested && pos.distance(center) <= monument_icon_panel::REQUEST_RADIUS + 2.
                    } else {
                        self.monument_icons.marker_hit(pos,center,t.prefab_path.as_deref(),&self.preferences,requested,map_label_panel::Text{name:&self.monument_icons.name(t.prefab_path.as_deref(),&t.name),source:map_font.as_ref()})
                    };
                    let desired=if requested_marker_visible && marker_hit(screen(t.x,t.z),true) {screen(t.x,t.z).distance(pos)}else{f32::INFINITY};
                    let generated = accepted.filter(|p|accepted_markers_visible && marker_hit(screen(p.x,p.z),false)).map_or(f32::INFINITY, |p| screen(p.x,p.z).distance(pos));
                    let stamp = t
                        .prefab_path
                        .as_deref()
                        .and_then(|path| self.prefabs.stamp(&self.project, path));
                    let hit = |pose:[f32;3],requested:bool| self.preview_current() && map.intersect(outer).contains(pos) && stamp.is_some_and(|stamp| {
                            stamp.hit_test_displayed(
                                [
                                    (pos.x - map.left()) / side * size - size / 2.,
                                    size / 2. - (pos.y - map.top()) / side * size,
                                ],
                                pose,
                                self.native_view == mapper_studio::native_view::View::Overview
                                    && self
                                        .preview
                                        .as_ref()
                                        .is_some_and(|p| p.native_layers.is_some()),
                                stamp_context,(t.instance_id,requested),!requested && stamp_context.composed.is_some(),
                            )
                        });
                    let requested_visible=self.selected==Some(i) && self.selected_stamp_overlay_visible()
                        && (!self.source_stamp_sampling() || self.terrain_projection.has_paint());
                    let requested_hit=requested_visible && map.intersect(outer).contains(pos)
                        && self.terrain_projection.hit_test(t.instance_id,[(pos.x-map.left())/side*size-size/2.,size/2.-(pos.y-map.top())/side*size])
                        .unwrap_or_else(||hit(self.terrain_projection.light_pose(t.instance_id,[t.x,t.z,t.yaw.unwrap_or_else(||stamp.map_or(0.,|s|s.default_yaw))]),true));
                    let generated_hit=self.accepted_stamp_visible(i) && accepted.is_some_and(|p|hit([p.x,p.z,p.yaw],false));
                    let desired_rank=if requested_hit {desired.min(21.)}else{desired};
                    let generated_rank=if generated_hit {generated.min(21.)}else{generated};
                    // A pending request marker can coincide with an accepted
                    // origin while carrying a different requested rotation.
                    let request_marker_hit=self.selected==Some(i) && self.requested_footprint()
                        && (desired.is_finite() || requested_hit) && desired_rank<=generated_rank;
                    if let Some(p)=accepted.filter(|_|!request_marker_hit && generated_rank<=desired_rank) {
                        (
                            i,
                            generated_rank,
                            [p.x, p.z],
                            Some(p.yaw),
                            requested_hit,
                            accepted_order.filter(|_| generated_hit),
                        )
                    } else {
                        (i, desired_rank, [t.x, t.z], t.yaw, requested_hit, None)
                    }
                })
                // The requested preview is painted last. Keep it selected where
                // opaque stamp hits tie, while preserving nearer visible handles.
                // Accepted paint follows the displayed placement order, which
                // can differ from the recipe's target order after regeneration.
                .min_by(|a, b| {
                    a.1.total_cmp(&b.1)
                        .then_with(|| b.4.cmp(&a.4))
                        .then_with(|| b.5.cmp(&a.5))
                });
            if let Some((i, d, origin, yaw, _, _)) = nearest
                && d <= 22.
            {
                self.selected = Some(i);
                if can_start_drag && (response.drag_started_by(egui::PointerButton::Primary) || quick_drag) {
                    self.drag_before = Some(self.project.clone());
                    self.drag_origin = Some(origin);
                    self.drag_press = Some(pos);
                    self.project.targets[i].x = origin[0];
                    self.project.targets[i].z = origin[1];
                    if let Some(yaw) = yaw {
                        self.project.targets[i].yaw = Some(yaw);
                    }
                    let heading = self.project.targets[i]
                        .prefab_path
                        .as_deref()
                        .and_then(|p| self.prefabs.stamp(&self.project, p))
                        .map(|s| s.default_yaw);
                    if self.project.targets[i].yaw.is_none()
                        && let Some(yaw) = heading
                    {
                        self.project.targets[i].yaw = Some(yaw.rem_euclid(360.));
                        if !mapper_studio::placement_preview::is_earthwork(
                            &self.project.placement_engine,
                        ) {
                            self.project.placement_engine =
                                mapper_studio::placement_preview::ROTATION_VERSION.into();
                        }
                    }
                    // Show the clean cached base while moving. No old monument paint or
                    // synchronous 2048-square layer render belongs in the pointer path.
                    self.drag_splat = self.preview.as_ref().and_then(|p|
                        p.native_layers.as_ref().zip(p.palette.as_ref()).and_then(|(layers,palette)|
                            self.map_colors.clean_view(layers,&p.ocean,palette,&self.preferences.map_colors)
                        )).map(|v|v.texture.clone()).or_else(||self.texture.clone());
                    // This diagnostic texture may contain the old monument.
                    // Rebuild it only after the gesture, from the current source.
                    self.layer_texture = None;
                }
            } else if response.clicked() {
                self.selected = None;
            }
        }
        if self.drag_before.is_some()
            && (response.dragged_by(egui::PointerButton::Primary)
                || response.drag_stopped_by(egui::PointerButton::Primary)
                || released_at.is_some())
            && let Some(pos) = released_at.or(response.interact_pointer_pos())
        {
            let half = self.project.size as f32 / 2.;
            if let Some(t) = self.selected.and_then(|i| self.project.targets.get_mut(i)) {
                let press = self.drag_press.unwrap_or(pos);
                let origin = self.drag_origin.unwrap_or([t.x, t.z]);
                t.x = (origin[0] + (pos.x - press.x) / side * size).clamp(-half, half);
                t.z = (origin[1] - (pos.y - press.y) / side * size).clamp(-half, half);
                let scroll: f32 = ui.input(|i| {
                    i.raw
                        .events
                        .iter()
                        .filter_map(|event| {
                            if let egui::Event::MouseWheel { delta, .. } = event {
                                Some(delta.y)
                            } else {
                                None
                            }
                        })
                        .sum()
                });
                if scroll != 0. {
                    let initial = self
                        .drag_before
                        .as_ref()
                        .and_then(|p| self.selected.and_then(|i| p.targets.get(i)))
                        .and_then(|t| t.yaw)
                        .or_else(|| {
                            placements
                                .iter()
                                .find(|(id, _)| *id == t.instance_id)
                                .map(|(_, p)| p.yaw)
                        })
                        .unwrap_or(0.);
                    let step = ui.input(|i| if i.modifiers.shift { 1. } else { 15. });
                    t.yaw =
                        Some((t.yaw.unwrap_or(initial) + scroll.signum() * step).rem_euclid(360.));
                    if !mapper_studio::placement_preview::is_earthwork(&self.project.placement_engine)
                    {
                        self.project.placement_engine =
                            mapper_studio::placement_preview::ROTATION_VERSION.into();
                    }
                }
            }
        }
        if (response.drag_stopped_by(egui::PointerButton::Primary) || released_at.is_some())
            && let Some(before) = self.drag_before.take()
        {
            if let Some(target) = self.selected.and_then(|i| self.project.targets.get_mut(i)) {
                target.max_displacement_m.get_or_insert(
                    mapper_studio::DEFAULT_MONUMENT_MOVE_LIMIT_M.min(self.project.size),
                );
            }
            self.history.record(before, &self.project);
            self.skip_history = true;
            self.drag_origin = None;
            self.drag_press = None;
            self.drag_splat = None;
            self.status = "Monument move staged".into();
            if let Some(target) = self.selected.and_then(|i| self.project.targets.get(i)) {
                self.placement_request = Some(PlacementRequest {
                    recipe: self.project.clone(),
                    id: target.instance_id,
                });
            }
        }
        if !self.topology_tool.enabled && self.drag_before.is_none()
            && response.dragged_by(egui::PointerButton::Primary) && released_at.is_none()
        {
            self.pan += ui.input(|input| input.pointer.delta());
            ui.ctx().request_repaint();
        }
        if released_at.is_some() {
            self.drag_press = None;
            self.drag_press_can_edit = false;
        }
        // Input can change the authoritative background and accepted poses in
        // this frame. Replace the original image command in-place to preserve
        // grid/topology paint order without a synchronous terrain render.
        self.refresh_map_colors(ui.ctx());
        self.terrain_projection.tick(self.projection_request(),ui.ctx());
        if let Some(texture) = self.map_background_texture() {
            painter.set(background_shape,egui::Shape::image(texture.id(),map,
                Rect::from_min_max(Pos2::ZERO,egui::pos2(1.,1.)),Color32::WHITE));
        }
        let placements = self.displayed_placements();
        // A requested footprint stays speculative until the solver accepts its pose.
        let requested = self.requested_footprint();
        // Baseline accepted paint goes first, matching the asynchronous preview's
        // baseline-plus-selected composition when requested footprints overlap.
        if self.legacy_stamp_overlays_visible() {
            for (id, position) in &placements {
                let Some((index,target)) = self.project.targets.iter().enumerate().find(|(_,t)| t.instance_id == *id)
                else { continue; };
                if !self.accepted_stamp_visible(index) { continue; }
                let Some(stamp) = target
                    .prefab_path
                    .as_deref()
                    .and_then(|p| self.prefabs.stamp(&self.project, p))
                else {
                    continue;
                };
                let points = stamp.geometry.quad(position.x, position.z, position.yaw);
                let mut mesh = egui::Mesh::with_texture(stamp.texture_at_background(
                    ui.ctx(),
                    self.map_stamp_context(),
                    (target.instance_id, false),
                    [position.x, position.z, position.yaw],
                    self.drag_before.is_some()
                        || self.native_view == mapper_studio::native_view::View::Overview,
                ));
                for (point, uv) in points
                    .into_iter()
                    .zip([[0., 0.], [1., 0.], [1., 1.], [0., 1.]])
                {
                    mesh.vertices.push(egui::epaint::Vertex {
                        pos: screen(point[0], point[1]),
                        uv: egui::pos2(uv[0], uv[1]),
                        color: Color32::WHITE,
                    });
                }
                mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
                painter
                    .with_clip_rect(map.intersect(outer))
                    .add(egui::Shape::mesh(mesh));
                // Retained paint stays visible; amber identifies selection.
                if self.show_preferences && self.current_layout().is_none() && self.selected == Some(index) {
                    painter
                        .with_clip_rect(map.intersect(outer))
                        .add(egui::Shape::closed_line(
                            points.into_iter().map(|p| screen(p[0], p[1])).collect(),
                            Stroke::new(1., Color32::from_rgb(255, 213, 138)),
                        ));
                }
            }
        }
        // Recheck after a drag: an old result must not appear current for edited targets.
        if self.selected_stamp_overlay_visible() && !self.terrain_projection.ready() && !self.source_stamp_sampling()
            && let Some(target) = self
                .selected
                .and_then(|i| self.project.targets.get(i))
                .filter(|t| t.presence != Presence::Excluded)
            && let Some(stamp) = target
                .prefab_path
                .as_deref()
                .and_then(|path| self.prefabs.stamp(&self.project, path))
        {
            let pose=self.terrain_projection.light_pose(target.instance_id,[target.x,target.z,target.yaw.unwrap_or(stamp.default_yaw)]);
            let points=stamp.geometry.quad(pose[0],pose[1],pose[2]);
            let mut mesh = egui::Mesh::with_texture(stamp.texture_at_background(
                ui.ctx(),
                self.map_stamp_context(),
                (target.instance_id, true),
                pose,
                self.drag_before.is_some()
                    || self.native_view == mapper_studio::native_view::View::Overview,
            ));
            for (point, uv) in points
                .into_iter()
                .zip([[0., 0.], [1., 0.], [1., 1.], [0., 1.]])
            {
                mesh.vertices.push(egui::epaint::Vertex {
                    pos: screen(point[0], point[1]),
                    uv: egui::pos2(uv[0], uv[1]),
                    color: Color32::WHITE,
                });
            }
            mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
            painter
                .with_clip_rect(map.intersect(outer))
                .add(egui::Shape::mesh(mesh));
            if self.show_preferences && requested && self.drag_before.is_none() {
                painter
                    .with_clip_rect(map.intersect(outer))
                    .add(egui::Shape::closed_line(
                        points.into_iter().map(|p| screen(p[0], p[1])).collect(),
                        Stroke::new(1., Color32::from_rgb(255, 213, 138)),
                    ));
            }
        }
        if self.selected_stamp_overlay_visible()
            && let Some((texture,points))=if self.retained_composition().is_some() {
                self.terrain_projection.proposal_quad()
            } else { self.terrain_projection.moving_quad() } {
            let mut mesh=egui::Mesh::with_texture(texture);
            for (p,uv) in points.into_iter().zip([[0.,0.],[1.,0.],[1.,1.],[0.,1.]]) {mesh.vertices.push(egui::epaint::Vertex{pos:screen(p[0],p[1]),uv:egui::pos2(uv[0],uv[1]),color:Color32::WHITE});}
            mesh.indices.extend_from_slice(&[0,1,2,0,2,3]);painter.with_clip_rect(map.intersect(outer)).add(egui::Shape::mesh(mesh));
        }
        if self.show_preferences && self.terrain_projection.ready() && requested && self.drag_before.is_none()
            && let Some(pose)=self.terrain_projection.resolved_pose()
            && let Some(target)=self.selected.and_then(|i|self.project.targets.get(i))
            && let Some(stamp)=target.prefab_path.as_deref().and_then(|p|self.prefabs.stamp(&self.project,p))
        {
            let points=stamp.geometry.quad(pose[0],pose[2],pose[3]);
            painter.with_clip_rect(map.intersect(outer)).add(egui::Shape::closed_line(
                points.into_iter().map(|p|screen(p[0],p[1])).collect(),Stroke::new(1.,Color32::from_rgb(255,213,138))));
        }
        if self.show_no_build {
            let roadside=self.displayed_composition().map(|v|v.generated_monuments.clone()).unwrap_or_else(||std::sync::Arc::from([]));
            let pending=self.selected.filter(|_|self.requested_footprint()).and_then(|i|self.project.targets.get(i)).filter(|t|t.presence!=Presence::Excluded).map(|t| {
                let fallback=t.yaw.or_else(||placements.iter().find(|p|p.0==t.instance_id).map(|p|p.1.yaw)).unwrap_or(0.);
                let pose=self.terrain_projection.ready().then(||self.terrain_projection.resolved_pose()).flatten().map(|p|[p[0],p[2],p[3]])
                    .unwrap_or_else(||self.terrain_projection.light_pose(t.instance_id,[t.x,t.z,fallback]));
                (t.instance_id,pose)
            });
            let (_,mut partial)=self.restrictions.paint(&painter,restriction_panel::Paint{map,viewport:outer,size,project:&self.project,placements:&placements,requested:pending});
            partial+=self.restrictions.paint_monuments(&painter,map,outer,size,&roadside).1;
            let note=if !self.restrictions.message.is_empty(){Some(self.restrictions.message.as_str())}else if partial>0{Some("No-build zones: incomplete restriction data")}else{None};
            if let Some(note)=note {painter.with_clip_rect(outer).text(egui::pos2(outer.left()+8.,outer.bottom()-8.),Align2::LEFT_BOTTOM,note,FontId::proportional(12.),Color32::from_rgb(255,213,138));}
        }
        if self.grid {
            let grid_style = map_font.as_ref().and_then(|font| font.grid.as_deref());
            restriction_panel::grid(&painter,map,outer,size as u32,grid_style);
            if let Some(pos)=response.hover_pos()
                && let Some(cell)=restriction_panel::grid_coordinate(map,pos,size as u32,grid_style) {
                response.clone().on_hover_text(cell);
            }
        }
        if self.current_layout().is_some() {
            for (id, position) in &placements {
                let p = screen(position.x, position.z);
                let color = Color32::from_rgb(118, 207, 245);
                if self.show_preferences
                    && let Some(target) = self.selected.and_then(|i| self.project.targets.get(i))
                    && target.instance_id == *id
                {
                    painter.line_segment([screen(target.x, target.z), p], Stroke::new(1., color));
                }
                let target=self.project.targets.iter().find(|t|t.instance_id==*id);
                let path=target.and_then(|t|t.prefab_path.as_deref());
                if self.preferences.monument_markers == mapper_studio::map_labels::Mode::Text
                    && !self.restrictions.shows_map_text(path) { continue; }
                let name=self.monument_icons.name(path,target.map_or("",|t|t.name.as_str()));
                if !self.monument_icons.paint(&painter,p,path,&self.preferences,self.settings_path.as_deref(),map_label_panel::Text{name:&name,source:map_font.as_ref()}) {
                    painter.circle_filled(p, monument_icon_panel::PLACED_RADIUS, color);
                }
            }
        }
        if let Some(view)=self.displayed_composition() {
            let roadside=view.generated_monuments.clone();
            for monument in roadside.iter() {
                let point=screen(monument.position[0],monument.position[2]);
                if !outer.expand(monument_icon_panel::ICON_SIDE).contains(point) {continue;}
                if self.preferences.monument_markers == mapper_studio::map_labels::Mode::Text
                    && !self.restrictions.shows_map_text(monument.prefab.as_deref()) {continue;}
                if !self.monument_icons.paint(&painter,point,monument.prefab.as_deref(),&self.preferences,self.settings_path.as_deref(),map_label_panel::Text{name:&self.monument_icons.name(monument.prefab.as_deref(),&monument.name),source:map_font.as_ref()}) {
                    painter.circle_filled(point, monument_icon_panel::PLACED_RADIUS, Color32::from_rgb(118,207,245));
                }
            }
            if let Some(pointer)=response.hover_pos().filter(|_|self.preferences.monument_markers==mapper_studio::map_labels::Mode::Icons)
                && let Some(monument)=roadside.iter().filter(|m|screen(m.position[0],m.position[2]).distance(pointer)<=18.)
                    .min_by(|a,b|screen(a.position[0],a.position[2]).distance(pointer).total_cmp(&screen(b.position[0],b.position[2]).distance(pointer))) {
                marker_label(&painter,outer,screen(monument.position[0],monument.position[2]),&self.monument_icons.name(monument.prefab.as_deref(),&monument.name));
            }
        }
        for (i, t) in self.project.targets.iter().enumerate() {
            if t.presence == Presence::Excluded {
                continue;
            }
            let p = screen(t.x, t.z);
            let selected = self.selected == Some(i);
            if !selected && !self.show_preferences {
                continue;
            }
            let color = if selected {
                Color32::from_rgb(255, 213, 138)
            } else {
                Color32::from_rgb(223, 239, 245)
            };
            if self.show_preferences && selected {
                painter.circle_stroke(
                    p,
                    (t.radius / size * side).max(2.),
                    Stroke::new(1., color.gamma_multiply(0.8)),
                );
            }
            painter.circle_filled(p, monument_icon_panel::REQUEST_RADIUS, color);
            if self.show_preferences && (self.preferences.monument_markers != mapper_studio::map_labels::Mode::Text
                || self.restrictions.shows_map_text(t.prefab_path.as_deref())) {
                self.monument_icons.paint(&painter,p,t.prefab_path.as_deref(),&self.preferences,self.settings_path.as_deref(),map_label_panel::Text{name:&self.monument_icons.name(t.prefab_path.as_deref(),&t.name),source:map_font.as_ref()});
            }
        }
        if let Some(pointer) = response.hover_pos().filter(|_|self.preferences.monument_markers==mapper_studio::map_labels::Mode::Icons) {
            let placed = placements
                .iter()
                .filter(|_| self.current_layout().is_some())
                .filter_map(|(id, p)| {
                    let t = self.project.targets.iter().find(|t| t.instance_id == *id)?;
                    let center = screen(p.x, p.z);
                    Some((
                        pointer.distance(center),
                        center,
                        format!("{} · placed", self.monument_icons.name(t.prefab_path.as_deref(),&t.name)),
                    ))
                });
            let requested = self
                .project
                .targets
                .iter()
                .enumerate()
                .filter(|(i, t)| {
                    t.presence != Presence::Excluded
                        && (self.selected == Some(*i) || self.show_preferences)
                })
                .map(|(_, t)| {
                    let center = screen(t.x, t.z);
                    (
                        pointer.distance(center),
                        center,
                        format!("{} · requested", self.monument_icons.name(t.prefab_path.as_deref(),&t.name)),
                    )
                });
            if let Some((distance, center, label)) =
                placed.chain(requested).min_by(|a, b| a.0.total_cmp(&b.0))
                && distance <= 18.
            {
                marker_label(&painter, outer, center, &label);
            }
        }
    }
}
struct Tabs<'a> {
    studio: &'a mut Studio,
    active_map: Option<u64>,
    map_count: usize,
    closing: Vec<Tab>,
    active_tabs: Vec<(Tab, Rect)>,
}
impl<'a> Tabs<'a> {
    fn new(studio: &'a mut Studio, active: Option<u64>, dock: &DockState<Tab>) -> Self {
        let map_tabs = dock.iter_all_tabs()
            .filter(|(_, tab)| matches!(tab, Tab::Map | Tab::Generated(_)))
            .count();
        let active_tabs = dock.iter_all_nodes()
            .filter_map(|(_, node)| node.get_leaf())
            .filter(|leaf| !leaf.collapsed && !leaf.tab_bar_hidden && leaf.rect.is_positive())
            .filter_map(|leaf| Some((*leaf.tabs.get(leaf.active.0)?, leaf.rect)))
            .collect();
        Self { studio, active_map: active, map_count: map_tabs, closing: Vec::new(), active_tabs }
    }

    fn finish(mut self, dock: &mut DockState<Tab>) {
        for tab in self.closing {
            let Some(path) = dock.find_tab(&tab) else { continue; };
            if self.map_count > 1 {
                dock.remove_tab(path);
                self.map_count -= 1;
            } else if let Ok(leaf) = dock.leaf_mut(path.node_path()) {
                leaf.tabs[path.tab.0] = Tab::Map;
            }
        }
    }
}
impl TabViewer for Tabs<'_> {
    type Tab = Tab;
    fn id(&mut self, tab: &mut Tab) -> egui::Id {
        egui::Id::new(format!("{tab:?}"))
    }
    fn on_tab_button(&mut self, tab: &mut Tab, response: &egui::Response) {
        workbench_style::finish_dock_title(response);
        if let Some((_, leaf)) = self.active_tabs.iter().find(|(active, _)| active == tab) {
            workbench_style::join_dock_tab(response, *leaf);
        }
    }
    fn title(&mut self, t: &mut Tab) -> egui::WidgetText {
        if let Tab::Generated(id) = *t { return self.studio.generated_title(id).into(); }
        let title = match t {
            Tab::Map => "Recipe preview",
            Tab::Generated(_) => unreachable!(),
            Tab::Recipe => "Recipe",
            Tab::Outliner => "Monuments",
            Tab::Inspector => "Selection",
            Tab::Queue => "Queue",
            Tab::Content => "Content",
            Tab::Plugins => "Plugins",
            Tab::Nodes => "Nodes",
        };
        if *t == Tab::Recipe {
            egui::RichText::new(title).size(16.).strong().into()
        } else if matches!(t, Tab::Outliner | Tab::Map) {
            egui::RichText::new(title).strong().into()
        } else {
            title.into()
        }
    }
    fn ui(&mut self, ui: &mut egui::Ui, t: &mut Tab) {
        workbench_style::finish_dock_body(ui);
        match t {
            Tab::Map => self.studio.map(ui),
            Tab::Generated(id) => self.studio.generated_map(ui, *id),
            Tab::Recipe => {
                self.studio.recipe_generate(ui);
                self.studio.recipe(ui);
            }
            Tab::Outliner => {
                if let Some(id) = self.active_map { self.studio.outliner(ui, Some(id)); }
                else { self.studio.with_recipe_draft(|studio| studio.outliner(ui, None)); }
            },
            Tab::Inspector => {
                if let Some(id) = self.active_map { self.studio.generated_summary(ui, id); }
                else { self.studio.with_recipe_draft(|studio| studio.inspector(ui)); }
            },
            Tab::Queue => self.studio.queue(ui),
            Tab::Nodes => self.studio.with_recipe_draft(|studio| studio.nodes.show(ui, &mut studio.project)),
            Tab::Content => {
                let editable = (self.studio.generation.busy()
                    || (self.studio.task.is_none() && self.studio.placement_task.is_none()))
                    && self.studio.drag_before.is_none();
                ui.add_enabled_ui(editable, |ui| {
                    self.studio.with_recipe_draft(|studio| studio.content.ui(ui, &mut studio.project));
                });
            }
            Tab::Plugins => {
                let idle = !self.studio.generation.busy()
                    && self.studio.task.is_none()
                    && self.studio.placement_task.is_none()
                    && self.studio.drag_before.is_none();
                self.studio.plugins.show(ui, &self.studio.project, idle, self.studio.settings_path.as_deref());
            }
        }
    }
    fn is_closeable(&self, tab: &Tab) -> bool {
        matches!(tab, Tab::Generated(_) | Tab::Nodes) || (*tab == Tab::Map && self.map_count > 1)
    }
    fn scroll_bars(&self, tab: &Tab) -> [bool; 2] {
        [!matches!(tab, Tab::Nodes | Tab::Recipe); 2]
    }
    fn on_close(&mut self, tab: &mut Tab) -> OnCloseResponse {
        if *tab == Tab::Nodes {
            return OnCloseResponse::Close;
        }
        if let Tab::Generated(id) = *tab {
            self.studio.close_generation_tab(id);
        }
        if matches!(tab, Tab::Map | Tab::Generated(_)) {
            self.closing.push(*tab);
        }
        OnCloseResponse::Ignore
    }
}
impl eframe::App for Studio {
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.extreme_bg_color.to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_project_io(&ctx);
        let pristine = self.pristine_new_project();
        self.tick_preferences(&ctx);
        self.poll(&ctx);
        self.poll_placement(&ctx);
        self.content.poll();
        self.preparation.tick();
        self.poll_exports();
        let path = self
            .selected
            .and_then(|i| self.project.targets.get(i))
            .filter(|t| t.presence != Presence::Excluded)
            .and_then(|t| t.prefab_path.as_deref());
        let working_map_visible = self.working_map_visible();
        // Native composition reads the prepared stamp data directly. The
        // preview worker makes raster overlays only for the older 2D view.
        let legacy_placements = (working_map_visible && self.legacy_stamp_overlays_visible())
            .then(|| self.displayed_placements());
        let stamp_paths = legacy_placements.iter().flatten().filter_map(|(id, _)| {
            self.project.targets.iter().find(|target| {
                target.instance_id == *id && target.presence != Presence::Excluded
            }).and_then(|target| target.prefab_path.as_deref())
        });
        let edited_stamp_paths = self.visible_generated_tab().into_iter()
            .flat_map(|id| self.generation.edited_stamp_paths(id));
        let assets_were_ready = self.source.bound_root_ref(&self.project)
            .is_some_and(|root| self.prefabs.generation_assets_ready(&self.project, root));
        self.prefabs.tick_all(
            &self.project,
            self.source.bound_root(&self.project),
            path,
            stamp_paths.chain(edited_stamp_paths),
            &ctx,
        );
        if let Some(prepared) = self.prefabs.take_prepared_generation() {
            self.decoration_panel.accept_cliffs(prepared.uniform_cliffs);
            let text_error=prepared.steps.iter().find(|step|step.name=="Preparing map text").and_then(|step|step.error.clone()).unwrap_or_default();
            if let Some(font) = &prepared.map_labels {
                self.generation.sync_map_font(font);
            }
            self.monument_icons.labels.accept(prepared.map_labels,text_error,&ctx);
            self.restrictions.set_tunnel_markers(&self.project, prepared.tunnel_markers);
            self.rails.accept_prepared(prepared.rails);
            if let (Some(root), Some(catalog), Some(bindings)) = (self.source.bound_root(&self.project), self.prefabs.catalog(&self.project), self.prefabs.bindings(&self.project)) {
                if let Some(monuments) = prepared.monuments {
                    self.monuments.accept_prepared(&self.project, &catalog, &bindings, monuments);
                }
                if let Some(roads) = prepared.roads { self.roads.accept_prepared(&self.project, root.clone(), &catalog, roads); }
                if let Some(source) = prepared.decoration { self.decoration_panel.accept_prepared(&self.project, root, &catalog, source); }
            }
            if let (Some(root), Some(source)) = (self.source.bound_root(&self.project), self.project.installation.clone()) {
                self.water.accept_prepared(root, source, prepared.rivers);
            }
        }
        let restriction_catalog = self.prefabs.catalog(&self.project);
        if self.restrictions.tick(
            restriction_catalog.is_some(),
            &self.project,
            self.source.bound_root(&self.project),
            restriction_catalog,
            &ctx,
        ) {
            self.generation.sync_restrictions(&self.restrictions);
        }
        let model_view_visible = self.camera.enabled
            && self.preview.is_some()
            && self.texture.is_some()
            && !self.generation.busy()
            && working_map_visible;
        self.monuments.tick(
            self.project.monument_rules.as_ref(),
            self.prefabs.catalog(&self.project),
            self.prefabs.bindings(&self.project),
            self.source.bound_root(&self.project).map(|p| p.to_owned()),
            model_view_visible && self.show_monuments,
            &ctx,
        );
        if !self.generation.busy()
            && self.installation_roster_pending
            && self.monuments.source.as_ref().is_some_and(|s| s.stage_source.is_some())
            && let Some(catalog) = self.prefabs.catalog(&self.project)
        {
            self.installation_roster_pending = false;
            // Only the empty new-project roster can be populated
            // automatically. Loaded projects and user edits remain explicit.
            if self.project.targets.is_empty() {
                let before = self.project.clone();
                let mut next = before.clone();
                next.targets.clear();
                match self.monuments.populate_installation(&mut next, &catalog) {
                    Ok(count) => {
                        self.project = next;
                        self.history.record(before, &self.project);
                        self.selected = None;
                        self.installation_placement_pending = true;
                        if let Some(task) = &self.placement_task { task.cancel.store(true, Ordering::Relaxed); }
                        self.status = format!("Loaded {count} monument candidates.");
                    }
                    Err(error) => self.status = format!("Installation roster unavailable: {error}"),
                }
            }
        }
        if !self.generation.busy() && self.installation_placement_pending && self.preview_current()
            && self.task.is_none() && self.placement_task.is_none()
        {
            self.installation_placement_pending = false;
            if self.current_layout().is_none() { self.start_placement(&ctx); }
        }
        if !self.generation.busy()
            && !assets_were_ready
            && self.source.bound_root_ref(&self.project)
                .is_some_and(|root| self.prefabs.generation_assets_ready(&self.project, root))
            && self.preview_current()
            && self.task.is_none()
            && self.placement_task.is_none()
            && self.current_layout().is_none()
        {
            self.start_placement(&ctx);
        }
        self.roads
            .tick(&self.project, self.prefabs.catalog(&self.project),self.source.bound_root(&self.project).map(|p|p.to_owned()), &ctx);
        self.apply_prepared_recipe_defaults();
        self.rails.tick(&self.project,self.source.bound_root(&self.project).map(|p|p.to_owned()),self.prefabs.catalog(&self.project),self.prefabs.bindings(&self.project),&ctx);
        self.decoration_panel.tick(
            &self.project,
            self.source.bound_root(&self.project).map(|p| p.to_owned()),
            self.prefabs.catalog(&self.project),
            self.prefabs.bindings(&self.project),
            &ctx,
        );
        // Installation defaults belong to a fresh document's initial recipe.
        // Finish before applying user-requested live jobs, plugins or UI edits.
        self.accept_new_project_defaults(pristine);
        if !self.generation.busy()
            && let Some(before) = self.decoration_panel.poll_live(&mut self.project, &ctx)
        {
            self.history.record(before, &self.project);
        }
        self.decoration_models.tick(
            &self.project,
            self.prefabs.catalog(&self.project),
            self.prefabs.bindings(&self.project),
            model_view_visible && self.show_decoration && self.composition.view(&self.project).is_some(),
            &ctx,
        );
        let composition_layout = self.current_layout_index();
        let composition_ready = composition_layout.is_some() || self.deferred_generation_placement();
        self.composition.set_live_preview(self.live_job_tab_visible());
        let composition_inputs = composition_panel::Inputs {
            source: self
                .preview
                .as_ref()
                .filter(|p| composition_ready && self.preview_current() && p.native_layers.is_some()),
            solution: composition_layout.map(|index| &self.layouts[index].solution),
            catalog: self.prefabs.catalog(&self.project),
            bindings: self.prefabs.bindings(&self.project),
            monuments: self.monuments.prepared.clone(),
            identities: self.source.bound_root_ref(&self.project)
                .and_then(|root| self.prefabs.prepared_identities(&self.project, root)),
            road_layout: self.generation.active_edit_road_layout(),
        };
        if self
            .composition
            .tick(&self.project, composition_inputs, &ctx)
        {
            self.layer_texture = None;
            self.publish_composed_layout();
            if !self.generation.busy() && self.composition.view(&self.project).is_some() {self.generation.revision+=1;}
            if self.inspection_ocean().is_none()
                && matches!(
                    self.native_view,
                    mapper_studio::native_view::View::Water
                        | mapper_studio::native_view::View::ProcessedTopology
                )
            {
                self.native_view = mapper_studio::native_view::View::Overview;
            }
        }
        self.tick_generation(&ctx);
        self.skip_history = false;
        if ctx.input(|i| i.key_pressed(egui::Key::Escape))
            && let Some(before) = self.drag_before.take()
        {
            self.project = before;
            self.drag_origin = None;
            self.drag_press = None;
            self.drag_splat = None;
            self.status = "Target move cancelled.".into();
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.request(Action::Close, &ctx);
        }
        if !self.generation.busy() && self.drag_before.is_none()
            && let Some(next) = self.plugins.poll(&self.project)
            && next != self.project
        {
            let previous = std::mem::replace(&mut self.project, next);
            self.history.record(previous, &self.project);
            if let Some(task) = &self.task {
                task.cancel.store(true, Ordering::Relaxed);
            }
            if let Some(task) = &self.placement_task {
                task.cancel.store(true, Ordering::Relaxed);
            }
            self.layer_texture = None;
            self.drag_splat = None;
            self.selected = self.selected.filter(|&i| i < self.project.targets.len());
            self.status="Plugin recipe applied. Rebuild terrain or regenerate monuments; Undo restores the previous recipe.".into();
        }
        let before = self.history.snapshot(&self.project);
        egui::Panel::top("toolbar")
            .frame(egui::Frame::NONE.fill(ui.visuals().extreme_bg_color).inner_margin(egui::Margin::symmetric(6, 3)))
            .show(ui, |ui| self.toolbar(ui));
        if self.plugins.focus_requested {
            self.show_tab(Tab::Plugins);
            self.plugins.focus_requested = false;
        }
        if self.content.focus_requested {
            self.show_tab(Tab::Content);
            self.content.focus_requested = false;
        }
        self.apply_tab_request();
        self.terrain_projection.begin_frame();
        let selected_before_dock = self.selected;
        let snapshot = self.visible_generated_tab();
        let mut dock = std::mem::replace(&mut self.dock, DockState::new(vec![]));
        self.generation_status(ui);
        if self.content.monument_settings_open() {
            self.with_recipe_draft(|studio| {
                let identities = studio.source.bound_root_ref(&studio.project)
                    .and_then(|root| studio.prefabs.prepared_identities(&studio.project, root));
                studio.content.show_monument_settings(&ctx, &mut studio.project, identities);
            });
        }
        egui::CentralPanel::default().frame(workbench_style::dock_frame(ui)).show(ui, |ui| {
            let mut tabs = Tabs::new(self, snapshot, &dock);
            DockArea::new(&mut dock).style(workbench_style::dock(ui))
                .show_leaf_close_all_buttons(false).show_leaf_collapse_buttons(false)
                .show_inside(ui, &mut tabs);
            tabs.finish(&mut dock);
        });
        if self.custom_monuments.is_open() {
            self.with_recipe_draft(|studio| {
                let editable = (studio.generation.busy()
                    || (studio.task.is_none() && studio.placement_task.is_none()))
                    && studio.drag_before.is_none();
                studio.custom_monuments.show(&ctx, &mut studio.project, editable);
            });
        }
        self.terrain_projection.end_frame(&ctx);
        self.camera.end_frame(&ctx);
        self.dock = if self.reset_layout {
            self.reset_layout = false;
            layout()
        } else {
            dock
        };
        let selection_changed = self.selected != selected_before_dock && self.selected.is_some();
        let candidate_opened = self.visible_generated_tab() != snapshot && self.visible_generated_tab().is_some();
        if (selection_changed || candidate_opened) && self.requested_tab.is_none() {
            self.show_tab(Tab::Inspector);
        }
        self.apply_tab_request();
        self.generation.finish_preview_frame(&self.dock);
        self.installation_setup_window(&ctx);
        self.recipe_settings_window(&ctx);
        self.settings_window(&ctx);
        let can_restart = self.updates.awaiting_restart() && !self.dirty() && !self.generation_in_progress()
            && !self.project_io.busy() && self.pending.is_none();
        self.updates.show(&ctx, self.path.as_deref(), can_restart);
        self.export_options(&ctx);
        self.persist_preferences();
        self.history.finish_snapshot(
            before,
            &self.project,
            !self.skip_history && self.drag_before.is_none(),
        );
        if self.pending.is_some() {
            egui::Modal::new(egui::Id::new("unsaved-project")).show(&ctx, |ui| {
                ui.heading("Unsaved project changes");
                ui.label("Save this project before continuing?");
                if let Some(error)=self.project_io.issue() {diagnostic_ui::message(ui,error,"project-io-error");}
                ui.horizontal(|ui| {
                    if ui.button("Save and continue").clicked() {self.save(false,&ctx);}
                    if ui.button("Discard changes").clicked()
                        && let Some(action) = self.pending.take()
                    {
                        self.perform(action, &ctx);
                    }
                    if ui.button("Keep editing").clicked() {
                        self.project_io.discard_opened();
                        self.pending = None;
                    }
                });
            });
        }
    }
}
fn main() -> eframe::Result {
    let path = std::env::args_os().nth(1).map(PathBuf::from);
    if let Some(channel) = mapper_studio::updates::Channel::installed() {
        match channel.redirect(path.as_deref()) {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) => eprintln!("Could not open the selected Mapper update. {error}"),
        }
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Mapper for Rust")
            .with_inner_size([1440., 940.])
            .with_min_inner_size([1050., 720.]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "Mapper for Rust",
        options,
        Box::new(move |cc| Ok(Box::new(Studio::new(cc, path)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preview_processing_order_changes_invalidate_the_same_terrain_key() {
        let ctx = egui::Context::default();
        let mut studio = fixture(&ctx);
        studio.project.preview_engine = mapper_studio::native_preview::VERSION.into();
        studio.project.composition_engine =
            Some(papyrus_terrain::monument_composition::SOURCE_HEIGHT_VERSION.into());
        studio.project.monument_rules = Some(
            serde_json::from_value(serde_json::json!({
                "engine": mapper_studio::monument_source::VERSION,
                "source_sha256": "a".repeat(64),
                "surface_modifiers": mapper_studio::monument_source::SOURCE_PRIORITY_VERSION,
            }))
            .unwrap(),
        );
        studio.preview.as_mut().unwrap().key = studio.project.terrain_key();
        assert!(!studio.preview_current());
        studio.preview.as_mut().unwrap().ordered_terrain = true;
        assert!(studio.preview_current());
        let key = studio.project.terrain_key();
        studio
            .project
            .monument_rules
            .as_mut()
            .unwrap()
            .surface_modifiers = None;
        assert_eq!(key, studio.project.terrain_key());
        assert!(!studio.preview_current());
        studio.preview.as_mut().unwrap().ordered_terrain = false;
        assert!(studio.preview_current());

        let (_sender, receiver) = std::sync::mpsc::channel();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        studio.task = Some(Task {
            ordered_terrain: false,
            key: studio.project.terrain_key(),
            receiver,
            cancel: cancel.clone(),
            progress: Default::default(),
        });
        studio.project.monument_rules.as_mut().unwrap().surface_modifiers =
            Some(mapper_studio::monument_source::SOURCE_PRIORITY_VERSION.into());
        studio.poll(&ctx);
        assert!(cancel.load(Ordering::Relaxed));
        assert!(!studio.preview_current());
    }

    #[test]
    fn unchanged_history_snapshots_reuse_allocations_and_record_each_edit() {
        let mut history = History::default();
        let mut project = Project::default();
        let original = project.clone();

        let before = history.snapshot(&project);
        let targets = before.targets.as_ptr();
        history.finish_snapshot(before, &project, true);
        assert!(!history.can_undo());

        let before = history.snapshot(&project);
        assert_eq!(before.targets.as_ptr(), targets);
        project.size += 100;
        history.finish_snapshot(before, &project, true);
        let edited = project.clone();
        history.undo(&mut project);
        assert_eq!(project, original);

        let before = history.snapshot(&project);
        history.redo(&mut project);
        assert_eq!(project, edited);
        history.finish_snapshot(before, &project, false);
        history.undo(&mut project);
        assert_eq!(project, original);
        assert!(!history.can_undo());
    }

    pub(super) fn fixture(ctx: &egui::Context) -> Studio {
        let project = Project::default();
        let image = Arc::new(egui::ColorImage::filled([2, 2], Color32::GREEN));
        Studio {
            project_io:Default::default(),
            map_archive: Default::default(),
            saved: project.clone(),
            preview: Some(Preview {
                ordered_terrain: false,
                raw_source: None,
                rivers: None,
                key: project.terrain_key(),
                image: image.clone(),
                display_image: None,
                landscape: None,
                heights: std::sync::Arc::new(vec![17000; 65 * 65]),
                resolution: 65,
                metrics: preview::Metrics::default(),
                palette: None,
            native_layers: None, surface:None,
                ocean: None,
            }),
            project,
            path: None,
            history: History::default(),
            dock: layout(),
            requested_tab: None,
            selected: Some(0),
            monument_filter: String::new(),
            texture: Some(ctx.load_texture("test-terrain", image, egui::TextureOptions::LINEAR)),
            stage_image: None,
            stage_texture: None,
            layer_texture: None,
            native_view: mapper_studio::native_view::View::default(),
            topology_bit: 7,
            topology_tool: Default::default(),
            generation_progress: None,
            live_generation: true,
            task: None,
            placement_task: None,
            layouts: Vec::new(),
            active_layout: None,
            template_picker: Default::default(),
            content: content_panel::ContentPanel::default(),
            custom_monuments: Default::default(),
            updates: Default::default(),
            preparation: preparation_panel::PreparationPanel::default(),
            prefabs: prefab_panel::PrefabPanel::default(),
            monuments: Default::default(),
            roads: Default::default(),
            rails: Default::default(),
            decoration_panel: Default::default(),
            decoration_models: Default::default(),
            source: source_panel::SourcePanel::default(),
            generation: generation_ui::Generation::default(),
            water: river_panel::Panel::default(),
            image_export: Default::default(),
            export_options_open: false,
            image_export_target: None,
            status: String::new(),
            plugins: plugin_panel::PluginPanel::default(),
            composition: composition_panel::CompositionPanel::default(),
            zoom: 1.,
            terrain_projection: Default::default(),
            erosion_device: Default::default(),
            erosion_comparison: Default::default(),
            nodes: Default::default(),
            map_colors: Default::default(),
            pan: Vec2::ZERO,
            grid: false,
            show_no_build: false,
            monument_icons: Default::default(),
            installation_library: Default::default(),
            restrictions: Default::default(),
            show_monuments: true,
            show_decoration: true,
            show_preferences: false,
            drag_before: None,
            drag_origin: None,
            drag_press: None,
            placement_request: None,
            drag_press_can_edit: false,
            camera: terrain_3d::Camera::default(),
            drag_splat: None,
            skip_history: false,
            pending: None,
            allow_close: false,
            reset_layout: false,
            preferences: Default::default(),
            settings_path: None,
            settings_message: String::new(),
            settings_open: false,
            settings_page: Default::default(),
            recipe_settings_open: false,
            recipe_settings_tab: Default::default(),
            installation_setup_open: false,
            installation_roster_pending: false,
            installation_placement_pending: false,
            restore_source: false,
        }
    }
    #[test]
    fn legacy_stamp_previews_are_only_needed_for_visible_uncomposed_2d_terrain() {
        use mapper_studio::native_view::View;
        use papyrus_terrain::monument_composition::{SOURCE_HEIGHT_VERSION, WATER_VERSION};

        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        app.project.size = 1000;
        app.project.composition_engine = Some(WATER_VERSION.into());
        app.preview.as_mut().unwrap().key = app.project.terrain_key();
        assert!(app.working_map_visible());
        assert!(app.legacy_stamp_overlays_visible());

        app.camera.enabled = true;
        assert!(!app.legacy_stamp_overlays_visible());
        app.camera.enabled = false;
        app.native_view = View::Water;
        assert!(!app.legacy_stamp_overlays_visible());
        app.native_view = View::Ground;
        assert!(app.legacy_stamp_overlays_visible());

        app.composition.seed_test_view(&ctx, &app.project);
        assert!(!app.legacy_stamp_overlays_visible());
        app.composition.discard_context();
        app.project.composition_engine = Some(SOURCE_HEIGHT_VERSION.into());
        app.preview.as_mut().unwrap().key = app.project.terrain_key();
        assert!(!app.legacy_stamp_overlays_visible());
        app.project.composition_engine = None;
        app.preview = None;
        assert!(!app.legacy_stamp_overlays_visible());
    }

    #[test]
    fn cables_share_the_render_batch_and_keep_compact_theme_safe_status() {
        use super::terrain_3d::{Camera, Cables, ModelInstance};
        use egui::TextureId;
        mod snapshot {
            include!("../examples/support/egui_snapshot.rs");
        }
        let report = terrain_3d::tests::cable_report();
        let original = serde_json::to_vec(&report).unwrap();
        let pole = papyrus_terrain::path_mesh::Template {
            vertices: vec![[-3., 0., 0.], [3., 0., 0.], [-3., 140., 0.], [3., 140., 0.]],
            normals: vec![[0., 0., 1.]; 4],
            tangents: vec![[1., 0., 0., 1.]; 4],
            uv: vec![[0.; 2]; 4],
            triangles: vec![[0, 1, 2], [2, 1, 3]],
            center: [0., 70., 0.],
            extents: [3., 70., 1.],
        };
        for dark in [true, false] {
            for size in [[1050, 720], [1440, 940]] {
                let ctx = egui::Context::default();
                super::workbench_style::install(&ctx);
                ctx.set_theme(if dark {
                    egui::Theme::Dark
                } else {
                    egui::Theme::Light
                });
                ctx.all_styles_mut(|style| style.animation_time = 0.);
                let models: Vec<_> = [-300., 0., 300.]
                    .into_iter()
                    .map(|x| {
                        ModelInstance::surface(
                            &pole,
                            [x, papyrus_terrain::erosion::decode(18000), 0.],
                            [165., 165., 160.],
                        )
                    })
                    .collect();
                let mut camera = Camera::default();
                let mut textures = snapshot::Textures::default();
                let mut last = None;
                for _ in 0..2 {
                    let mut frame = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(size[0] as f32, size[1] as f32),
                            )),
                            ..Default::default()
                        },
                        |ui| {
                            let (rect, response) =
                                ui.allocate_exact_size(ui.available_size(), egui::Sense::drag());
                            camera.draw_with_scene(
                                ui,
                                rect,
                                &response,
                                TextureId::default(),
                                &egui::ColorImage::filled([2, 2], Color32::from_rgb(75, 94, 61)),
                                &vec![18000; 65 * 65],
                                65,
                                1000,
                                &models,
                                None,
                                Some(Cables::Ready(&report)),
                            );
                        },
                    );
                    textures.update(&frame.textures_delta);
                    frame.textures_delta.clear();
                    last = Some(frame);
                }
                let frame = last.unwrap();
                let mesh = frame
                    .shapes
                    .iter()
                    .find_map(|s| match &s.shape {
                        egui::Shape::Mesh(mesh) => Some(mesh),
                        _ => None,
                    })
                    .unwrap();
                assert_eq!(
                    mesh.vertices
                        .iter()
                        .filter(|v| v.color == Color32::from_rgb(40, 42, 38))
                        .count(),
                    8
                );
                assert!(mesh.indices.len() <= 64 * 64 * 6 + 48_000 * 3);
                let status = frame
                    .shapes
                    .iter()
                    .find_map(|s| match &s.shape {
                        egui::Shape::Text(text) if text.galley.job.text.contains("cable spans") => {
                            Some(text)
                        }
                        _ => None,
                    })
                    .unwrap();
                assert!(status.galley.job.text.contains("2/2 cable spans"));
                assert!(status.pos.x + status.galley.size().x <= size[0] as f32);
                let image = textures.raster(&ctx, frame, size);
                let visible_cable_pixels = image.pixels.iter().filter(|pixel| {
                    (38..=41).contains(&pixel.r())
                        && (40..=43).contains(&pixel.g())
                        && (36..=39).contains(&pixel.b())
                }).count();
                assert!(visible_cable_pixels > 100, "Elevated cable spans must remain visible above terrain");
                if let Some(dir) = std::env::var_os("MAPPER_UI_SNAPSHOT_DIR") {
                    let dir = std::path::PathBuf::from(dir);
                    std::fs::create_dir_all(&dir).unwrap();
                    mapper_studio::image_export::write_png(
                        &dir.join(format!(
                            "cables-{}-{}x{}.png",
                            if dark { "dark" } else { "light" },
                            size[0],
                            size[1]
                        )),
                        &image,
                        || false,
                    )
                    .unwrap();
                }
            }
        }
        assert_eq!(original, serde_json::to_vec(&report).unwrap());
    }
    fn frame(ctx: &egui::Context, app: &mut Studio, events: Vec<egui::Event>) -> egui::FullOutput {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(900., 700.))),
                events,
                focused: true,
                ..Default::default()
            },
            |ui| app.map(ui),
        );
        // Headless tests inspect paint shapes without a GPU texture consumer.
        output.textures_delta.clear();
        output
    }
    #[test]
    fn working_header_controls_fit_without_wrapping_across_views_and_stale_recipes() {
        for dark in [false, true] {
            for width in [280., 360., 520., 600., 1200.] {
                let ctx = egui::Context::default();
                ctx.set_theme(if dark { egui::Theme::Dark } else { egui::Theme::Light });
                workbench_style::install(&ctx);
                let mut app = fixture(&ctx);
                app.project.master_seed += 1;
                app.project.refresh_inherited_seeds();
                for three_d in [false, true] {
                    app.camera.enabled = three_d;
                    let render = |app: &mut Studio, events| {
                        let mut output = ctx.run_ui(egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(width, 700.))),
                            events, focused: true,
                            ..Default::default()
                        }, |ui| app.map(ui));
                        output.textures_delta.clear();
                        output
                    };
                    let _ = render(&mut app, vec![]);
                    let output = render(&mut app, vec![]);
                    let mut centers = vec![];
                    for name in ["2D", "3D", "Fit", "View"] {
                        let rect = output.shapes.iter().find_map(|shape| match &shape.shape {
                            egui::Shape::Text(text) if text.galley.job.text == name => {
                                let rect = text.galley.rect.translate(text.pos.to_vec2());
                                assert!(shape.clip_rect.contains_rect(rect), "Clipped {name} at {width}");
                                Some(rect)
                            }
                            _ => None,
                        }).unwrap_or_else(|| panic!("Missing {name} at {width}"));
                        centers.push(rect.center());
                    }
                    assert!(centers.iter().all(|pos| (pos.y - centers[0].y).abs() < 2.));
                    assert!(centers[0].y < 60., "The map header must occupy one compact row");
                    assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                        egui::Shape::Rect(rect) if rect.fill == workbench_style::MAP_VIEW_SELECTED
                            && rect.rect.contains(centers[usize::from(three_d)])
                    )), "The active view must have a visible selection at {width}");
                    let expected_seed = format!("Seed {}", app.preview.as_ref().unwrap().key.seed);
                    assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                        egui::Shape::Text(text) if text.galley.job.text == expected_seed
                    )));
                    if width == 600. {
                        let pos = centers[usize::from(!three_d)];
                        let recipe = app.project.clone();
                        let pan = app.pan;
                        render(&mut app, vec![egui::Event::PointerMoved(pos), button(pos, true)]);
                        render(&mut app, vec![button(pos, false)]);
                        assert_eq!(app.camera.enabled, !three_d);
                        assert_eq!(app.project, recipe);
                        assert_eq!(app.pan, pan);
                    }
                }
            }
        }
    }

    fn button(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }
    fn first_target(output: egui::FullOutput) -> Pos2 {
        output
            .shapes
            .into_iter()
            .find_map(|s| match s.shape {
                egui::Shape::Circle(c) if c.radius == monument_icon_panel::REQUEST_RADIUS
                    && c.fill == Color32::from_rgb(255, 213, 138) => Some(c.center),
                _ => None,
            })
            .expect("Selected monument marker should render")
    }
    fn placed_marker_point(app: &Studio, output: &egui::FullOutput, index: usize) -> Pos2 {
        let texture = app.map_background_texture().unwrap().id();
        let bounds = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Mesh(mesh) if mesh.texture_id == texture => Some(mesh.calc_bounds()),
            _ => None,
        }).expect("Map background should render");
        let target = &app.project.targets[index];
        let position = app.current_layout().unwrap().solution.outcomes.iter()
            .find(|outcome| outcome.id == target.instance_id).unwrap().position.as_ref().unwrap();
        let size = app.project.size as f32;
        let center = egui::pos2(bounds.left() + (position.x / size + 0.5) * bounds.width(),
            bounds.top() + (0.5 - position.z / size) * bounds.height());
        // Locate visible artwork instead of assuming every marker is a square.
        (-14..=14).flat_map(|y| (-14..=14).map(move |x| center + egui::vec2(x as f32, y as f32)))
            .filter(|point| app.monument_icons.marker_hit(*point, center, target.prefab_path.as_deref(), &app.preferences, false,Default::default()))
            .min_by(|a, b| a.distance_sq(center).total_cmp(&b.distance_sq(center)))
            .expect("Placed marker must have a selectable visible point")
    }
    fn two_stamp_interaction_fixture(ctx:&egui::Context)->(Studio,[egui::TextureId;2]) {
        let mut app=fixture(ctx);app.project.size=1000;app.project.targets.truncate(2);
        for (i,target) in app.project.targets.iter_mut().enumerate() {target.x=if i==0 {-250.}else{250.};target.z=0.;target.radius=40.;target.yaw=Some(0.);}
        app.project.placement_engine=mapper_studio::placement_preview::ROTATION_VERSION.into();
        let paths=app.project.targets.iter().map(|t|t.prefab_path.clone().unwrap()).collect::<Vec<_>>();
        let textures=[app.prefabs.seed_test_stamp(ctx,&mut app.project,&paths[0]),app.prefabs.seed_test_stamp(ctx,&mut app.project,&paths[1])];
        app.preview.as_mut().unwrap().key=app.project.terrain_key();
        let recipe=app.project.clone();deliver_layout(&mut app,recipe,false);
        let active=app.active_layout.unwrap();
        for t in &app.project.targets {app.layouts[active].solution.outcomes.iter_mut().find(|o|o.id==t.instance_id).unwrap().position=Some(papyrus_terrain::placement::Position{x:t.x,y:20.,z:t.z,yaw:0.,clearance:40.});}
        assert!(app.current_layout().is_some());(app,textures)
    }
    #[test]
    fn composed_layout_publication_updates_markers_and_rejects_orphaned_results() {
        for cancelled in [false,true] {
            let ctx=egui::Context::default();let mut app=fixture(&ctx);
            app.project.size=1000;app.project.targets.truncate(1);app.project.targets[0].x=0.;app.project.targets[0].z=0.;
            app.preview.as_mut().unwrap().key=app.project.terrain_key();
            let recipe=app.project.clone();deliver_layout(&mut app,recipe.clone(),false);
            let layout=&mut app.layouts[app.active_layout.unwrap()];
            layout.solution.outcomes[0].position=Some(papyrus_terrain::placement::Position{x:0.,y:20.,z:0.,yaw:0.,clearance:40.});
            layout.solution.required_missing=0;
            let input=app.current_layout().unwrap().solution.clone();let mut resolved=input.clone();
            resolved.outcomes[0].position.as_mut().unwrap().x+=25.;
            app.composition.seed_test_completed(&ctx,&app.project,Some(&input),resolved.clone(),cancelled);
            if !cancelled {app.active_layout=None;}
            app.publish_composed_layout();
            assert_eq!(app.project,recipe);
            assert_eq!(&app.current_layout().unwrap().solution,if cancelled {&input}else{&resolved});
            assert_eq!(app.composition.view(&app.project).is_some(),!cancelled);
            assert_eq!(app.displayed_placements()[0].1.x,if cancelled {0.}else{25.});
            if !cancelled {
                let mut orphan=input.clone();orphan.outcomes[0].position.as_mut().unwrap().x+=50.;
                app.composition.seed_test_completed(&ctx,&app.project,Some(&input),orphan,false);
                app.publish_composed_layout();
                assert!(app.composition.view(&app.project).is_none(),"An unmatched source layout published a world");
                assert_eq!(app.current_layout().unwrap().solution,resolved);
            }
        }
    }
    #[test]
    fn full_world_stays_visible_through_first_stamp_drag_ready_proposal_deselection_and_undo() {
        let ctx=egui::Context::default();let mut app=fixture(&ctx);
        let _directory=app.prefabs.seed_test_verified_stamp(&ctx,&mut app.project);
        app.project.targets.truncate(1);app.project.size=1000;
        app.project.targets[0].x=0.;app.project.targets[0].z=0.;app.project.targets[0].yaw=Some(0.);
        app.project.placement_engine=mapper_studio::placement_preview::ROTATION_VERSION.into();
        app.project.composition_engine=Some(papyrus_terrain::monument_composition::SOURCE_HEIGHT_VERSION.into());
        let layers=std::sync::Arc::new(papyrus_terrain::native_layers::Layers{size:1000,terrain_seed:1,biome_seed:2,height_resolution:2,surface_resolution:2,height:vec![18000;4],splat:vec![0;32],biome:vec![0;20],topology:vec![0;4],normalized_biome_cells:0,normalized_splat_cells:0});
        let preview=app.preview.as_mut().unwrap();preview.native_layers=Some(layers);preview.key=app.project.terrain_key();
        let recipe=app.project.clone();deliver_layout(&mut app,recipe.clone(),false);
        let layout=&mut app.layouts[app.active_layout.unwrap()];
        layout.solution.outcomes[0].position=Some(papyrus_terrain::placement::Position{x:0.,y:20.,z:0.,yaw:0.,clearance:40.});
        let solution=layout.solution.clone();let original_positions=solution.outcomes.clone();
        let catalog=app.prefabs.catalog(&app.project).unwrap();let bindings=app.prefabs.bindings(&app.project).unwrap();
        let complete=app.composition.seed_test_world(&ctx,&app.project,solution,catalog.sha256.clone(),bindings.sha256.clone());
        app.selected=None;
        let output=frame(&ctx,&mut app,vec![]);
        let quad=output.shapes.iter().find_map(|s|match &s.shape {egui::Shape::Mesh(m) if m.texture_id==complete=>Some(m),_=>None}).unwrap();
        let start=quad.vertices.iter().map(|v|v.pos.to_vec2()).fold(Vec2::ZERO,|a,b|a+b)/4.;let start=Pos2::new(start.x,start.y);
        frame(&ctx,&mut app,vec![egui::Event::PointerMoved(start),button(start,true)]);
        let end=start+egui::vec2(35.,20.);
        let moved=frame(&ctx,&mut app,vec![egui::Event::PointerMoved(end)]);
        assert!(app.drag_before.is_some());assert!(!app.terrain_projection.has_paint());
        assert_eq!(app.map_background_texture().unwrap().id(),complete);
        assert!(has_stamp_mesh(&moved,complete));assert!(app.exportable_map().is_none());
        assert_eq!(app.displayed_placements()[0].1,original_positions[0].position.clone().unwrap());
        let released=frame(&ctx,&mut app,vec![button(end,false)]);
        assert!(has_stamp_mesh(&released,complete));assert!(app.composition.view(&app.project).is_none());
        let request=app.projection_request().unwrap();let proposed=app.terrain_projection.seed_test_projection(request,&ctx);
        assert!(app.terrain_projection.ready());assert!(app.terrain_projection.moving_quad().is_none());
        assert!(app.terrain_projection.proposal_quad().is_some());
        let ready=frame(&ctx,&mut app,vec![]);
        assert_eq!(app.map_background_texture().unwrap().id(),complete);
        assert!(has_stamp_mesh(&ready,complete));assert!(has_stamp_mesh(&ready,proposed));
        assert!(app.exportable_map().is_none());
        let palette=prefab_panel::palette_fixture(app.project.installation.as_ref().unwrap());
        app.composition.seed_test_palette(palette.clone());
        let mut color_layers=app.retained_composition().map(|v|v.maps.layers.clone())
            .unwrap_or_else(||app.composition.preference_view(&app.project,&catalog.sha256,&bindings.sha256,Some(&palette.source_render_key())).unwrap().maps.layers.clone());
        color_layers.terrain_seed=app.project.seeds[0].value;color_layers.biome_seed=app.project.seeds[1].value;
        let preview=app.preview.as_mut().unwrap();preview.palette=Some(palette.clone());preview.native_layers=Some(std::sync::Arc::new(color_layers));
        app.preferences.map_colors.enabled=true;app.preferences.map_colors.overrides[4]=Some([255,0,0]);
        // Parallel editor tests share the serialized image-generation pool.
        let deadline=std::time::Instant::now()+std::time::Duration::from_secs(90);
        loop {
            app.refresh_map_colors(&ctx);
            if app.map_colors.view().is_some() {break;}
            assert!(std::time::Instant::now()<deadline,"Retained world recoloring did not finish");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let recolored=app.map_colors.view().unwrap().texture.id();assert_ne!(recolored,complete);
        assert_eq!(app.map_background_texture().unwrap().id(),recolored);
        assert_eq!(app.map_palette().unwrap().cartography().unwrap().colors[4],[1.,0.,0.,1.]);
        assert!(app.exportable_map().is_none());
        let blank=egui::pos2(850.,620.);frame(&ctx,&mut app,vec![egui::Event::PointerMoved(blank),button(blank,true)]);
        let deselected=frame(&ctx,&mut app,vec![button(blank,false)]);
        assert!(app.selected.is_none());assert!(has_stamp_mesh(&deselected,recolored));assert!(!has_stamp_mesh(&deselected,proposed));
        assert!(app.exportable_map().is_none());
        app.history.undo(&mut app.project);assert_eq!(app.project,recipe);
        let restored=frame(&ctx,&mut app,vec![]);assert!(has_stamp_mesh(&restored,recolored));
        assert!(app.retained_composition().is_none());assert!(app.composition.view(&app.project).is_some());
    }
    #[test]
    fn retained_cartographic_stamp_can_be_reselected_after_a_preference_edit() {
        for visible in [true, false] {
            let ctx = egui::Context::default();
            let mut app = fixture(&ctx);
            let _directory = app.prefabs.seed_test_verified_stamp(&ctx, &mut app.project);
            app.project.size = 1000;
            app.project.targets.truncate(1);
            app.project.targets[0].x = 0.;
            app.project.targets[0].z = 0.;
            app.project.targets[0].yaw = Some(0.);
            app.project.placement_engine = mapper_studio::placement_preview::ROTATION_VERSION.into();
            let palette = app.prefabs.seed_test_cartographic_weights(&app.project, Some(0));
            app.project.composition_engine = Some(papyrus_terrain::monument_composition::SOURCE_HEIGHT_VERSION.into());
            let (hr, sr) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
            let cells = sr * sr;
            let mut base = papyrus_terrain::native_layers::Layers {
                size: 1000, terrain_seed: 1, biome_seed: 2,
                height_resolution: hr, surface_resolution: sr,
                height: vec![18000; hr * hr], splat: vec![0; cells * 8],
                biome: vec![0; cells * 5],
                topology: vec![0; cells], normalized_biome_cells: 0, normalized_splat_cells: 0,
            };
            base.biome[cells..cells * 2].fill(255);
            base.splat[cells * 4..cells * 5].fill(255);
            let mut painted = base.clone();
            if visible {
                painted.splat.fill(0);
                painted.splat[..cells].fill(255);
            }
            let preview = app.preview.as_mut().unwrap();
            preview.key = app.project.terrain_key();
            preview.native_layers = Some(std::sync::Arc::new(base.clone()));
            preview.palette = Some(palette.clone());
            let recipe = app.project.clone();
            deliver_layout(&mut app, recipe.clone(), false);
            let layout = &mut app.layouts[app.active_layout.unwrap()];
            layout.solution.outcomes[0].position = Some(papyrus_terrain::placement::Position {
                x: 0., y: 30., z: 0., yaw: 0., clearance: 80.,
            });
            let solution = layout.solution.clone();
            let catalog = app.prefabs.catalog(&app.project).unwrap().sha256.clone();
            let bindings = app.prefabs.bindings(&app.project).unwrap().sha256.clone();
            let background = app.composition.seed_test_world(&ctx, &app.project, solution, catalog, bindings);
            app.composition.seed_test_palette(palette);
            app.composition.seed_test_world_layers(base, painted);
            // Keep the complete generated world visible after editing a preference.
            app.project.targets[0].x = 250.;
            app.selected = None;
            app.show_preferences = false;
            assert!(app.composition.view(&app.project).is_none());
            assert!(app.retained_composition().is_some());
            let before = app.project.clone();
            let output = frame(&ctx, &mut app, vec![]);
            let mesh = output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Mesh(mesh) if mesh.texture_id == background => Some(mesh),
                _ => None,
            }).unwrap();
            let bounds = Rect::from_points(&mesh.vertices.iter().map(|v| v.pos).collect::<Vec<_>>());
            let center = bounds.center();
            let point = center + egui::vec2(50., 0.);
            let world = [(point.x - center.x) / bounds.width() * 1000., 0.];
            assert!(app.accepted_stamp_visible(0));
            assert_eq!(app.displayed_placements()[0].0, app.project.targets[0].instance_id);
            let stamp = app.prefabs.stamp(
                &app.project, app.project.targets[0].prefab_path.as_deref().unwrap(),
            ).unwrap();
            let context = app.map_stamp_context();
            assert_eq!(
                stamp.hit_test_displayed(
                    world, [0., 0., 0.], true, context,
                    (app.project.targets[0].instance_id, false), true,
                ),
                visible,
            );
            frame(&ctx, &mut app, vec![egui::Event::PointerMoved(point), button(point, true)]);
            frame(&ctx, &mut app, vec![button(point, false)]);
            assert_eq!(app.selected, visible.then_some(0));
            assert_eq!(app.project, before);
            if visible {
                frame(&ctx, &mut app, vec![button(point, true)]);
                let end = point + egui::vec2(30., 15.);
                frame(&ctx, &mut app, vec![egui::Event::PointerMoved(end)]);
                assert_eq!(app.selected, Some(0));
                assert_eq!(app.drag_origin, Some([0., 0.]));
                assert!(app.drag_before.is_some());
                assert_ne!(app.project.targets[0].x, before.targets[0].x);
                frame(&ctx, &mut app, vec![button(end, false)]);
                app.history.undo(&mut app.project);
                assert_eq!(app.project, before);
            }
            let blank = center + egui::vec2(-210., -130.);
            frame(&ctx, &mut app, vec![egui::Event::PointerMoved(blank), button(blank, true)]);
            let deselected = frame(&ctx, &mut app, vec![button(blank, false)]);
            assert_eq!(app.selected, None);
            assert_eq!(app.project, before);
            assert!(has_stamp_mesh(&deselected, background));
        }
    }
    #[test]
    fn source_stamp_remains_visible_through_drag_rotation_and_transparent_blank_deselection() {
        mod snapshot {include!("../examples/support/egui_snapshot.rs");}
        for dark in [false,true]{for (width,height,name) in [(900,700,"wide"),(430,650,"narrow")] {
            let ctx=egui::Context::default();ctx.set_visuals(if dark{egui::Visuals::dark()}else{egui::Visuals::light()});let mut app=fixture(&ctx);let _directory=app.prefabs.seed_test_verified_stamp(&ctx,&mut app.project);app.project.targets.truncate(1);app.project.size=1000;
            app.project.targets[0].x=0.;app.project.targets[0].z=0.;app.project.targets[0].yaw=Some(0.);
            app.project.placement_engine=mapper_studio::placement_preview::ROTATION_VERSION.into();app.project.composition_engine=Some(papyrus_terrain::monument_composition::SOURCE_HEIGHT_VERSION.into());app.prefabs.show_stamp=true;
            let layers=std::sync::Arc::new(papyrus_terrain::native_layers::Layers{size:1000,terrain_seed:1,biome_seed:2,height_resolution:2,surface_resolution:2,height:vec![18000;4],splat:vec![0;32],biome:vec![0;20],topology:vec![0;4],normalized_biome_cells:0,normalized_splat_cells:0});
            let preview=app.preview.as_mut().unwrap();preview.native_layers=Some(layers);preview.key=app.project.terrain_key();
            let request=app.projection_request().unwrap();let moving_id=app.terrain_projection.seed_test_projection(request,&ctx);assert!(app.terrain_projection.ready());
            let mut textures=snapshot::Textures::default();
            let render=|textures:&mut snapshot::Textures,app:&mut Studio,events:Vec<egui::Event>|{let mut out=ctx.run_ui(egui::RawInput{screen_rect:Some(Rect::from_min_size(Pos2::ZERO,egui::vec2(width as f32,height as f32))),events,focused:true,..Default::default()},|ui|app.map(ui));textures.update(&out.textures_delta);out.textures_delta.clear();out};
            render(&mut textures,&mut app,vec![]);let out=render(&mut textures,&mut app,vec![]);
            let map=out.shapes.iter().find_map(|s|match &s.shape{egui::Shape::Rect(r) if r.fill==Color32::from_rgb(24,66,95)=>Some(r.rect),_=>None}).unwrap();
            // The map image is fitted into the viewport; read its actual quad.
            let image=out.shapes.iter().find_map(|s|match &s.shape{egui::Shape::Mesh(m) if m.vertices.len()==4&&m.vertices[0].uv==Pos2::ZERO=>Some(m),_=>None}).unwrap();
            let left=image.vertices.iter().map(|v|v.pos.x).fold(f32::INFINITY,f32::min);let right=image.vertices.iter().map(|v|v.pos.x).fold(f32::NEG_INFINITY,f32::max);let top=image.vertices.iter().map(|v|v.pos.y).fold(f32::INFINITY,f32::min);let side=right-left;
            let screen=|p:[f32;2]|egui::pos2(left+(p[0]/1000.+0.5)*side,top+(0.5-p[1]/1000.)*side);
            let start=screen([-125.,-125.]);assert!(map.contains(start));render(&mut textures,&mut app,vec![egui::Event::PointerMoved(start),button(start,true)]);
            let end=start+egui::vec2(25.,15.);let moved=render(&mut textures,&mut app,vec![egui::Event::PointerMoved(end)]);
            assert!(app.drag_before.is_some());assert!(!app.terrain_projection.ready());assert!(has_stamp_mesh(&moved,moving_id));assert!(app.terrain_projection.texture().is_some());
            let moved_points=app.terrain_projection.moving_quad().unwrap().1;
            render(&mut textures,&mut app,vec![egui::Event::MouseWheel{unit:egui::MouseWheelUnit::Point,phase:egui::TouchPhase::Move,delta:egui::vec2(0.,20.),modifiers:egui::Modifiers::NONE}]);
            assert_ne!(app.terrain_projection.moving_quad().unwrap().1,moved_points);
            let released=render(&mut textures,&mut app,vec![button(end,false)]);assert!(app.drag_before.is_none());assert!(has_stamp_mesh(&released,moving_id));assert!(!app.terrain_projection.ready());
            let pose=app.project.targets[0].clone();let yaw=pose.yaw.unwrap().to_radians();let(s,c)=yaw.sin_cos();let clear=[pose.x+c*125.-s*125.,pose.z+s*125.+c*125.];
            assert_eq!(app.terrain_projection.hit_test(pose.instance_id,clear),Some(false));let blank=screen(clear);let before=app.project.clone();
            if let Some(dir)=std::env::var_os("MAPPER_UI_SNAPSHOT_DIR"){let dir=PathBuf::from(dir);std::fs::create_dir_all(&dir).unwrap();let image=textures.raster(&ctx,released,[width,height]);mapper_studio::image_export::write_png(&dir.join(format!("moving-source-{}-{name}.png",if dark{"dark"}else{"light"})),&image,||false).unwrap();}
            render(&mut textures,&mut app,vec![egui::Event::PointerMoved(blank),button(blank,true)]);let deselected=render(&mut textures,&mut app,vec![button(blank,false)]);
            assert!(app.selected.is_none());assert_eq!(app.project,before);assert!(!has_stamp_mesh(&deselected,moving_id));assert!(!app.terrain_projection.has_paint());
        }}
    }
    #[test]
    fn source_stamp_revision_does_not_draw_or_pick_legacy_fallback_paint_while_projection_is_pending() {
        for revision in [papyrus_terrain::monument_composition::SOURCE_TEXTURE_VERSION,
            papyrus_terrain::monument_composition::SOURCE_CLIFF_VERSION,
            papyrus_terrain::monument_composition::SOURCE_RASTER_VERSION,
            papyrus_terrain::monument_composition::SOURCE_HEIGHT_VERSION] {
        let ctx=egui::Context::default();let(mut app,textures)=two_stamp_interaction_fixture(&ctx);
        app.selected=Some(1);app.prefabs.show_stamp=true;
        let output=frame(&ctx,&mut app,vec![]);
        let mesh=output.shapes.iter().find_map(|s|match &s.shape {
            egui::Shape::Mesh(m) if m.texture_id==textures[1]=>Some(m),_=>None
        }).unwrap();
        let point=mesh.vertices[0].pos.lerp(mesh.vertices[1].pos,0.9)
            .lerp(mesh.vertices[3].pos.lerp(mesh.vertices[2].pos,0.9),0.5);
        app.project.composition_engine=Some(revision.into());
        let before=app.project.clone();let pending=frame(&ctx,&mut app,vec![]);
        assert!(!app.terrain_projection.ready());
        for texture in textures {assert!(!has_stamp_mesh(&pending,texture));}
        frame(&ctx,&mut app,vec![egui::Event::PointerMoved(point),button(point,true)]);
        frame(&ctx,&mut app,vec![button(point,false)]);
        assert_eq!(app.selected,None);assert_eq!(app.project,before);
        }
    }
    #[test]
    fn retained_stamp_blank_click_removes_selection_outline_while_stale_or_busy() {
        for busy in [false,true] {
            let ctx=egui::Context::default();let(mut app,textures)=two_stamp_interaction_fixture(&ctx);
            app.selected=None;app.prefabs.show_stamp=false;
            let (_sender,receiver)=std::sync::mpsc::channel();
            let cancel=std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            if busy {app.task=Some(Task {ordered_terrain:false,key:app.project.terrain_key(),receiver,cancel:cancel.clone(),progress:std::sync::Arc::new(std::sync::Mutex::new(None))});}
            else {app.project.targets[0].radius+=1.;}
            assert!(app.current_layout().is_none());let before=app.project.clone();
            let output=frame(&ctx,&mut app,vec![]);
            let mesh=output.shapes.iter().find_map(|s|match &s.shape {
                egui::Shape::Mesh(m) if m.texture_id==textures[1]=>Some(m),_=>None,
            }).unwrap();
            let corners=mesh.vertices.iter().map(|v|v.pos).collect::<Vec<_>>();
            let point=corners[0].lerp(corners[1],0.85).lerp(corners[3].lerp(corners[2],0.85),0.5);
            let outlined=|output:&egui::FullOutput|output.shapes.iter().any(|s|matches!(&s.shape,
                egui::Shape::Path(p) if p.closed && p.points==corners));
            frame(&ctx,&mut app,vec![egui::Event::PointerMoved(point),button(point,true)]);
            let selected=frame(&ctx,&mut app,vec![button(point,false)]);
            assert_eq!(app.selected,Some(1));assert!(!outlined(&selected));
            app.show_preferences=true;
            let guides=frame(&ctx,&mut app,vec![]);
            assert!(outlined(&guides));
            let blank=egui::pos2(850.,620.);
            frame(&ctx,&mut app,vec![egui::Event::PointerMoved(blank),button(blank,true)]);
            let deselected=frame(&ctx,&mut app,vec![button(blank,false)]);
            assert_eq!(app.selected,None);assert_eq!(app.project,before);
            assert!(has_stamp_mesh(&deselected,textures[1]),"Deselecting must retain the stamp paint");
            assert!(!outlined(&deselected),"Deselected retained stamp still looks selected; busy={busy}");
            assert!(!cancel.load(std::sync::atomic::Ordering::Relaxed));
        }
    }
    #[test]
    fn stamp_interaction_regression_hidden_accepted_handles_cannot_capture_blank_clicks() {
        for busy in [false,true] {
            let ctx=egui::Context::default();let(mut app,textures)=two_stamp_interaction_fixture(&ctx);
            app.show_monuments=false;app.prefabs.show_stamp=false;
            let output=frame(&ctx,&mut app,vec![]);
            let point=placed_marker_point(&app,&output,1);
            // The same center remains selectable while its actual marker is drawn.
            frame(&ctx,&mut app,vec![egui::Event::PointerMoved(point),button(point,true)]);frame(&ctx,&mut app,vec![button(point,false)]);
            assert_eq!(app.selected,Some(1));app.selected=Some(0);
            let (_sender,receiver)=std::sync::mpsc::channel();
            if busy {app.task=Some(Task{ordered_terrain:false,key:app.project.terrain_key(),receiver,cancel:std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),progress:std::sync::Arc::new(std::sync::Mutex::new(None))});}
            else {app.project.targets[0].radius+=1.;}
            assert!(app.current_layout().is_none());let before=app.project.clone();
            let output=frame(&ctx,&mut app,vec![]);
            assert!(!has_stamp_mesh(&output,textures[0]) && !has_stamp_mesh(&output,textures[1]));
            assert!(!output.shapes.iter().any(|s|matches!(&s.shape,egui::Shape::Circle(c) if c.fill==Color32::from_rgb(118,207,245))));
            frame(&ctx,&mut app,vec![egui::Event::PointerMoved(point),button(point,true)]);frame(&ctx,&mut app,vec![button(point,false)]);
            assert_eq!(app.selected,None,"Hidden accepted center captured a blank click; busy={busy}");assert_eq!(app.project,before);
            frame(&ctx,&mut app,vec![egui::Event::PointerMoved(point),button(point,true)]);
            frame(&ctx,&mut app,vec![egui::Event::PointerMoved(point+egui::vec2(35.,20.))]);
            assert!(app.drag_before.is_none());assert_eq!(app.project,before);
            frame(&ctx,&mut app,vec![button(point+egui::vec2(35.,20.),false)]);
            // Retained, visible paint stays selectable even with its center hidden.
            app.show_monuments=true;let output=frame(&ctx,&mut app,vec![]);assert!(has_stamp_mesh(&output,textures[1]));
            frame(&ctx,&mut app,vec![egui::Event::PointerMoved(point),button(point,true)]);frame(&ctx,&mut app,vec![button(point,false)]);
            assert_eq!(app.selected,Some(1));assert_eq!(app.project,before);
            if let Some(task)=&app.task {assert!(!task.cancel.load(std::sync::atomic::Ordering::Relaxed));}
        }
    }
    #[test]
    fn stamp_interaction_regression_requested_paint_is_above_unchanged_accepted_paint() {
        for selected in [0,1] {for view in [mapper_studio::native_view::View::Overview,mapper_studio::native_view::View::Ground] {
            let ctx=egui::Context::default();let(mut app,textures)=two_stamp_interaction_fixture(&ctx);
            app.selected=Some(selected);app.native_view=view;
            app.project.targets[selected].x=app.project.targets[1-selected].x;
            app.placement_request=Some(PlacementRequest{recipe:app.project.clone(),id:app.project.targets[selected].instance_id});
            assert!(app.requested_footprint() && app.selected_stamp_overlay_visible());
            let output=frame(&ctx,&mut app,vec![]);assert!(!app.terrain_projection.ready());
            let mesh=|id|output.shapes.iter().enumerate().find_map(|(i,s)|match &s.shape {egui::Shape::Mesh(m) if m.texture_id==id=>Some((i,m)),_=>None}).unwrap();
            let (front,requested)=mesh(textures[selected]);let (back,accepted)=mesh(textures[1-selected]);
            assert_eq!(requested.vertices.iter().map(|v|v.pos).collect::<Vec<_>>(),accepted.vertices.iter().map(|v|v.pos).collect::<Vec<_>>());
            assert!(front>back,"Requested paint was covered by an unchanged stamp; selected={selected},view={view:?}");
        }}
    }
    #[test]
    fn overlapping_accepted_stamps_pick_visible_paint_in_display_order() {
        for reversed in [false, true] {
            for transparent in [false, true] {
                for view in [
                    mapper_studio::native_view::View::Overview,
                    mapper_studio::native_view::View::Ground,
                ] {
                    let ctx = egui::Context::default();
                    let (mut app, mut textures) = two_stamp_interaction_fixture(&ctx);
                    app.selected = None;
                    app.prefabs.show_stamp = false;
                    app.native_view = view;
                    app.project.targets[1].x = app.project.targets[0].x;
                    let recipe = app.project.clone();
                    let layout = &mut app.layouts[app.active_layout.unwrap()];
                    layout.recipe = recipe.clone();
                    for outcome in &mut layout.solution.outcomes {
                        outcome.position.as_mut().unwrap().x = recipe.targets[0].x;
                    }
                    if reversed {
                        layout.solution.outcomes.reverse();
                    }
                    let front = usize::from(!reversed);
                    if transparent {
                        let path = app.project.targets[front].prefab_path.clone().unwrap();
                        app.prefabs.seed_test_stamp_image(
                            &ctx,
                            &mut app.project,
                            &path,
                            egui::ColorImage::new(
                                [4, 2],
                                vec![
                                    Color32::WHITE, Color32::WHITE,
                                    Color32::TRANSPARENT, Color32::TRANSPARENT,
                                    Color32::WHITE, Color32::WHITE,
                                    Color32::TRANSPARENT, Color32::TRANSPARENT,
                                ],
                            ),
                        );
                        textures[front] = app.prefabs.stamp(&app.project, &path).unwrap().texture.id();
                    }
                    let output = frame(&ctx, &mut app, vec![]);
                    let meshes = output.shapes.iter()
                        .filter_map(|shape| match &shape.shape {
                            egui::Shape::Mesh(mesh) if textures.contains(&mesh.texture_id) => Some(mesh),
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(meshes.len(), 2);
                    assert_eq!(meshes[1].texture_id, textures[front]);
                    let mesh = meshes[1];
                    let center = mesh.vertices[0].pos.lerp(mesh.vertices[2].pos, 0.5);
                    let left = mesh.vertices[0].pos.lerp(mesh.vertices[3].pos, 0.5);
                    let right = mesh.vertices[1].pos.lerp(mesh.vertices[2].pos, 0.5);
                    let point = left.lerp(right, 0.95);
                    assert!(point.distance(center) > 22.);
                    frame(&ctx, &mut app, vec![
                        egui::Event::PointerMoved(point), button(point, true),
                    ]);
                    frame(&ctx, &mut app, vec![button(point, false)]);
                    let expected = if transparent { 1 - front } else { front };
                    assert_eq!(app.selected, Some(expected),
                        "reversed={reversed}, transparent={transparent}, view={view:?}");
                    assert_eq!(app.project, recipe);
                    frame(&ctx, &mut app, vec![
                        egui::Event::PointerMoved(point), button(point, true),
                    ]);
                    let end = point + egui::vec2(24., 8.);
                    frame(&ctx, &mut app, vec![egui::Event::PointerMoved(end)]);
                    assert_eq!(app.selected, Some(expected));
                    assert!(app.drag_before.is_some());
                    assert_ne!(app.project.targets[expected].x, recipe.targets[expected].x);
                    assert_eq!(app.project.targets[1 - expected], recipe.targets[1 - expected]);
                    frame(&ctx, &mut app, vec![button(end, false)]);
                    app.history.undo(&mut app.project);
                    assert_eq!(app.project, recipe);
                    let blank = right + egui::vec2(45., 0.);
                    frame(&ctx, &mut app, vec![
                        egui::Event::PointerMoved(blank), button(blank, true),
                    ]);
                    frame(&ctx, &mut app, vec![button(blank, false)]);
                    assert_eq!(app.selected, None);
                    assert_eq!(app.project, recipe);
                }
            }
        }
    }
    #[test]
    fn stamp_interaction_regression_overlapping_requested_paint_keeps_selection() {
        let ctx=egui::Context::default();let(mut app,textures)=two_stamp_interaction_fixture(&ctx);
        app.selected=Some(1);app.project.targets[1].x=app.project.targets[0].x;
        app.placement_request=Some(PlacementRequest{recipe:app.project.clone(),id:app.project.targets[1].instance_id});
        assert!(app.requested_footprint() && app.selected_stamp_overlay_visible());
        let output=frame(&ctx,&mut app,vec![]);let center=first_target(output.clone());
        let mesh=output.shapes.iter().find_map(|s|match &s.shape {egui::Shape::Mesh(m) if m.texture_id==textures[1]=>Some(m),_=>None}).unwrap();
        let left=mesh.vertices[0].pos.lerp(mesh.vertices[3].pos,0.5);let right=mesh.vertices[1].pos.lerp(mesh.vertices[2].pos,0.5);let point=left.lerp(right,0.9);
        assert!(point.distance(center)>22.);let before=app.project.clone();
        frame(&ctx,&mut app,vec![egui::Event::PointerMoved(point),button(point,true)]);frame(&ctx,&mut app,vec![button(point,false)]);
        assert_eq!(app.selected,Some(1),"Overlapping accepted paint stole the selected requested footprint");assert_eq!(app.project,before);
        frame(&ctx,&mut app,vec![egui::Event::PointerMoved(point),button(point,true)]);
        let end=point+egui::vec2(35.,20.);frame(&ctx,&mut app,vec![egui::Event::PointerMoved(end)]);
        assert_eq!(app.selected,Some(1));assert!(app.drag_before.is_some());assert_eq!(app.project.targets[0],before.targets[0]);
        assert_ne!(app.project.targets[1].x,before.targets[1].x);
        frame(&ctx,&mut app,vec![button(end,false)]);assert!(app.drag_before.is_none());
        app.history.undo(&mut app.project);assert_eq!(app.project,before);
    }

    #[test]
    fn transparent_requested_paint_does_not_steal_selection_from_visible_underlying_paint() {
        let ctx=egui::Context::default();let(mut app,_)=two_stamp_interaction_fixture(&ctx);
        let path=app.project.targets[1].prefab_path.clone().unwrap();
        app.prefabs.seed_test_stamp_image(&ctx,&mut app.project,&path,egui::ColorImage::new([4,2],vec![Color32::WHITE,Color32::WHITE,Color32::TRANSPARENT,Color32::TRANSPARENT,Color32::WHITE,Color32::WHITE,Color32::TRANSPARENT,Color32::TRANSPARENT]));
        app.selected=Some(1);app.project.targets[1].x=app.project.targets[0].x;
        app.placement_request=Some(PlacementRequest{recipe:app.project.clone(),id:app.project.targets[1].instance_id});
        let output=frame(&ctx,&mut app,vec![]);let center=first_target(output.clone());
        let stamp=app.prefabs.stamp(&app.project,&path).unwrap();
        let texture=stamp.texture_at_background(&ctx,app.map_stamp_context(),(app.project.targets[1].instance_id,true),[app.project.targets[1].x,0.,0.],true);
        let mesh=output.shapes.iter().find_map(|s|match &s.shape {egui::Shape::Mesh(m) if m.texture_id==texture=>Some(m),_=>None}).unwrap();
        let left=mesh.vertices[0].pos.lerp(mesh.vertices[3].pos,0.5);let right=mesh.vertices[1].pos.lerp(mesh.vertices[2].pos,0.5);let point=left.lerp(right,0.95);
        assert!(point.distance(center)>22.);assert!(!stamp.hit_test([app.project.targets[1].x+91.,20.],[app.project.targets[1].x,0.,0.],false));
        let before=app.project.clone();frame(&ctx,&mut app,vec![egui::Event::PointerMoved(point),button(point,true)]);frame(&ctx,&mut app,vec![button(point,false)]);
        assert_eq!(app.selected,Some(0));assert_eq!(app.project,before);
    }

    #[test]
    fn blank_space_near_a_stamp_selected_marker_deselects_without_moving_the_map() {
        mod snapshot {include!("../examples/support/egui_snapshot.rs");}
        for dark in [false,true] {for (width,height,name) in [(900,700,"wide"),(430,650,"narrow")] {for zoom in [0.5,1.,2.] {
            if width==430 && zoom==2. {continue;}
            let mut textures=snapshot::Textures::default();
            let ctx=egui::Context::default();let(mut app,_)=two_stamp_interaction_fixture(&ctx);
            ctx.set_visuals(if dark{egui::Visuals::dark()}else{egui::Visuals::light()});
            let render=|textures:&mut snapshot::Textures,app:&mut Studio,events:Vec<egui::Event>| {
                let mut out=ctx.run_ui(egui::RawInput{screen_rect:Some(Rect::from_min_size(Pos2::ZERO,egui::vec2(width as f32,height as f32))),events,focused:true,..Default::default()},|ui|app.map(ui));
                textures.update(&out.textures_delta);out.textures_delta.clear();out
            };
            let path=app.project.targets[0].prefab_path.clone().unwrap();
            app.prefabs.seed_test_stamp_image(&ctx,&mut app.project,&path,egui::ColorImage::new([4,2],vec![
                Color32::WHITE,Color32::TRANSPARENT,Color32::TRANSPARENT,Color32::TRANSPARENT,
                Color32::WHITE,Color32::TRANSPARENT,Color32::TRANSPARENT,Color32::TRANSPARENT]));
            app.selected=None;app.zoom=zoom;
            if zoom > 1. {
                // Keep the left edge of this stamp inside the wider dock's canvas.
                app.pan.x = 100.;
            }
            let texture=app.prefabs.stamp(&app.project,&path).unwrap().texture.id();
            let output=render(&mut textures,&mut app,vec![]);
            let (mesh, clip)=output.shapes.iter().find_map(|s|match &s.shape {
                egui::Shape::Mesh(m) if m.texture_id==texture=>Some((m,s.clip_rect)),_=>None }).unwrap();
            let left=mesh.vertices[0].pos.lerp(mesh.vertices[3].pos,0.5);
            let right=mesh.vertices[1].pos.lerp(mesh.vertices[2].pos,0.5);
            let paint=left.lerp(right,0.08);let center=left.lerp(right,0.5);
            assert!(clip.contains(paint), "Stamp sample is outside the canvas at zoom {zoom}");
            let before=app.project.clone();
            render(&mut textures,&mut app,vec![egui::Event::PointerMoved(paint),button(paint,true)]);
            let selected=render(&mut textures,&mut app,vec![button(paint,false)]);
            assert_eq!(app.selected,Some(0),"Visible paint should select at zoom {zoom}");
            // Outside the painted cross/dot/square and on transparent stamp pixels,
            // but within the old invisible 22-point circular marker hit region.
            let blank=center+egui::vec2(15.,10.);
            render(&mut textures,&mut app,vec![egui::Event::PointerMoved(blank),button(blank,true)]);
            let deselected=render(&mut textures,&mut app,vec![button(blank,false)]);
            assert_eq!(app.selected,None,"Blank marker halo captured the click at zoom {zoom}");
            assert_eq!(app.project,before);assert!(app.drag_before.is_none());
            render(&mut textures,&mut app,vec![egui::Event::PointerMoved(blank),button(blank,true)]);
            let end=blank+egui::vec2(35.,20.);
            render(&mut textures,&mut app,vec![egui::Event::PointerMoved(end)]);
            assert!(app.drag_before.is_none());assert_eq!(app.project,before);
            render(&mut textures,&mut app,vec![button(end,false)]);
            if zoom==1. && let Some(dir)=std::env::var_os("MAPPER_UI_SNAPSHOT_DIR") {
                let dir=PathBuf::from(dir);std::fs::create_dir_all(&dir).unwrap();
                for (state,out) in [("selected",selected),("deselected",deselected)] {
                    let image=textures.raster(&ctx,out,[width,height]);
                    mapper_studio::image_export::write_png(&dir.join(format!("marker-halo-{}-{name}-{state}.png",if dark{"dark"}else{"light"})),&image,||false).unwrap();
                }
            }
        }}}
    }

    #[test]
    fn hidden_stamp_footprints_do_not_capture_blank_clicks_or_start_drags() {
        for mode in [0,1,2] {
            let ctx=egui::Context::default();let mut app=fixture(&ctx);
            app.project.size=1000;app.project.targets.truncate(1);
            app.project.targets[0].x=0.;app.project.targets[0].z=0.;app.project.targets[0].yaw=Some(0.);
            app.project.targets[0].radius=100.;
            app.project.placement_engine=mapper_studio::placement_preview::ROTATION_VERSION.into();
            let path=app.project.targets[0].prefab_path.clone().unwrap();
            let stamp=app.prefabs.seed_test_stamp(&ctx,&mut app.project,&path);
            app.preview.as_mut().unwrap().key=app.project.terrain_key();
            app.prefabs.show_stamp=true;
            let output=frame(&ctx,&mut app,vec![]);
            let center=first_target(output.clone());
            let mesh=output.shapes.iter().find_map(|s|match &s.shape {
                egui::Shape::Mesh(m) if m.texture_id==stamp=>Some(m),_=>None }).unwrap();
            let point=mesh.vertices[0].pos.lerp(mesh.vertices[1].pos,0.85)
                .lerp(mesh.vertices[3].pos.lerp(mesh.vertices[2].pos,0.85),0.5);
            assert!(point.distance(center)>22.);
            app.prefabs.show_stamp=false;
            if mode==1 || mode==2 {
                let recipe=app.project.clone();deliver_layout(&mut app,recipe,false);
                let active=app.active_layout.unwrap();
                app.layouts[active].solution.outcomes[0].position=Some(papyrus_terrain::placement::Position {
                    x:0.,y:20.,z:0.,yaw:0.,clearance:20.,
                });
            }
            if mode==1 {app.show_monuments=false;}
            if mode==2 {app.native_view=mapper_studio::native_view::View::Heights;}
            let before=app.project.clone();
            let output=frame(&ctx,&mut app,vec![]);
            assert!(!has_stamp_mesh(&output,stamp));
            frame(&ctx,&mut app,vec![egui::Event::PointerMoved(point),button(point,true)]);
            frame(&ctx,&mut app,vec![button(point,false)]);
            assert_eq!(app.selected,None,"Invisible footprint captured a blank click in mode {mode}");
            assert_eq!(app.project,before);
            frame(&ctx,&mut app,vec![egui::Event::PointerMoved(point),button(point,true)]);
            frame(&ctx,&mut app,vec![egui::Event::PointerMoved(point+egui::vec2(40.,20.))]);
            assert!(app.drag_before.is_none(),"Invisible footprint started a drag in mode {mode}");
            assert_eq!(app.project,before);
            frame(&ctx,&mut app,vec![button(point+egui::vec2(40.,20.),false)]);
        }
    }

    #[test]
    fn clipped_stamp_cannot_select_from_letterboxed_map_background() {
        let ctx=egui::Context::default();let mut app=fixture(&ctx);
        app.project.size=1000;app.project.targets.truncate(1);
        let t=&mut app.project.targets[0];t.x=500.;t.z=0.;t.radius=100.;t.yaw=Some(0.);
        app.project.placement_engine=mapper_studio::placement_preview::ROTATION_VERSION.into();
        let path=app.project.targets[0].prefab_path.clone().unwrap();
        let stamp=app.prefabs.seed_test_stamp(&ctx,&mut app.project,&path);
        app.preview.as_mut().unwrap().key=app.project.terrain_key();
        app.prefabs.show_stamp=true;app.zoom=0.5;
        let before=app.project.clone();
        let output=frame(&ctx,&mut app,vec![]);
        let center=first_target(output.clone());
        let(shape,mesh)=output.shapes.iter().find_map(|s|match &s.shape {
            egui::Shape::Mesh(m) if m.texture_id==stamp=>Some((s,m)),_=>None }).unwrap();
        let point=mesh.vertices[0].pos.lerp(mesh.vertices[1].pos,0.95)
            .lerp(mesh.vertices[3].pos.lerp(mesh.vertices[2].pos,0.95),0.5);
        assert!(!shape.clip_rect.contains(point));assert!(point.distance(center)>22.);
        frame(&ctx,&mut app,vec![egui::Event::PointerMoved(point),button(point,true)]);
        frame(&ctx,&mut app,vec![button(point,false)]);
        assert_eq!(app.selected,None,"Clipped texture is not a clickable footprint");
        assert_eq!(app.project,before);assert!(app.drag_before.is_none());
    }

    #[test]
    fn cartographic_invisible_paint_deselects_while_visible_paint_drags_and_undoes() {
        for channel in [None,Some(4),Some(0)] {
            let ctx=egui::Context::default();let mut app=fixture(&ctx);app.project.size=1000;app.project.targets.truncate(1);
            app.project.targets[0].x=0.;app.project.targets[0].z=0.;app.project.targets[0].yaw=Some(0.);
            let path=app.project.targets[0].prefab_path.clone().unwrap();
            let palette=app.prefabs.seed_test_cartographic_stamp(&ctx,&mut app.project,&path,channel);app.prefabs.show_stamp=true;
            let mut splat=vec![0;32];splat[16..20].fill(255);let mut biome=vec![0;20];biome[4..8].fill(255);
            let preview=app.preview.as_mut().unwrap();preview.key=app.project.terrain_key();preview.palette=Some(palette);
            preview.native_layers=Some(std::sync::Arc::new(papyrus_terrain::native_layers::Layers{size:1000,terrain_seed:1,biome_seed:2,height_resolution:2,surface_resolution:2,height:vec![18000;4],biome,splat,topology:vec![0;4],normalized_biome_cells:0,normalized_splat_cells:0}));
            let center=first_target(frame(&ctx,&mut app,vec![]));let point=center+egui::vec2(50.,0.);let before=app.project.clone();
            frame(&ctx,&mut app,vec![egui::Event::PointerMoved(point),button(point,true)]);frame(&ctx,&mut app,vec![button(point,false)]);
            if channel==Some(0) {
                assert_eq!(app.selected,Some(0));
                frame(&ctx,&mut app,vec![button(point,true)]);let end=point+egui::vec2(35.,20.);frame(&ctx,&mut app,vec![egui::Event::PointerMoved(end)]);frame(&ctx,&mut app,vec![button(end,false)]);
                assert_ne!(app.project,before);app.history.undo(&mut app.project);assert_eq!(app.project,before);
            } else {
                assert_eq!(app.selected,None,"Invisible cartographic paint captured blank click: {channel:?}");assert_eq!(app.project,before);
                frame(&ctx,&mut app,vec![button(point,true)]);frame(&ctx,&mut app,vec![egui::Event::PointerMoved(point+egui::vec2(35.,20.))]);frame(&ctx,&mut app,vec![button(point,false)]);
                assert!(app.drag_before.is_none());assert_eq!(app.project,before);
            }
        }
    }
    #[test]
    fn biome_only_footprint_picks_and_blank_click_deselects_without_edits() {
        let ctx=egui::Context::default();let mut app=fixture(&ctx);
        app.project.size=1000;app.project.targets.truncate(1);
        app.project.targets[0].x=0.;app.project.targets[0].z=0.;app.project.targets[0].yaw=Some(0.);
        let path=app.project.targets[0].prefab_path.clone().unwrap();
        app.prefabs.seed_test_biome_stamp(&ctx,&mut app.project,&path);
        app.prefabs.show_stamp=true;
        let preview=app.preview.as_mut().unwrap();preview.key=app.project.terrain_key();
        preview.native_layers=Some(std::sync::Arc::new(papyrus_terrain::native_layers::Layers {
            size:1000,terrain_seed:1,biome_seed:2,height_resolution:2,surface_resolution:2,
            height:vec![18000;4],biome:vec![255;20],splat:vec![255;32],topology:vec![0;4],
            normalized_biome_cells:0,normalized_splat_cells:0,
        }));
        let stamp=app.prefabs.stamp(&app.project,&path).unwrap();
        let texture=stamp.texture_at(&ctx,preview.native_layers.as_ref(),(app.project.targets[0].instance_id,true),[0.,0.,0.],true);
        let output=frame(&ctx,&mut app,vec![]);let center=first_target(output.clone());
        let mesh=output.shapes.iter().find_map(|s|match &s.shape {egui::Shape::Mesh(m) if m.texture_id==texture=>Some(m),_=>None}).unwrap();
        let point=mesh.vertices[1].pos.lerp(mesh.vertices[2].pos,0.5).lerp(mesh.vertices[0].pos.lerp(mesh.vertices[3].pos,0.5),0.1);
        assert!(point.distance(center)>22.);
        let before=app.project.clone();
        frame(&ctx,&mut app,vec![egui::Event::PointerMoved(point),button(point,true)]);
        frame(&ctx,&mut app,vec![button(point,false)]);
        assert_eq!(app.selected,Some(0));assert_eq!(app.project,before);
        let blank=egui::pos2(850.,620.);
        frame(&ctx,&mut app,vec![egui::Event::PointerMoved(blank),button(blank,true)]);
        frame(&ctx,&mut app,vec![button(blank,false)]);
        assert_eq!(app.selected,None);assert_eq!(app.project,before);assert!(app.drag_before.is_none());
    }
    #[test]
    fn blank_map_click_deselects_stamp_and_hides_its_requested_overlay_without_recipe_changes() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        let path = app.project.targets[0].prefab_path.clone().unwrap();
        let texture = app.prefabs.seed_test_stamp(&ctx, &mut app.project, &path);
        app.prefabs.show_stamp = true;
        let before = app.project.clone();
        let output = frame(&ctx, &mut app, vec![]);
        let start = first_target(output.clone());
        assert!(
            output
                .shapes
                .iter()
                .any(|s| matches!(&s.shape,egui::Shape::Mesh(m) if m.texture_id==texture))
        );
        let blank = egui::pos2(850., 620.);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(blank), button(blank, true)],
        );
        let output = frame(&ctx, &mut app, vec![button(blank, false)]);
        assert_eq!(app.selected, None);
        assert_eq!(app.project, before);
        assert!(app.drag_before.is_none());
        assert!(
            !output
                .shapes
                .iter()
                .any(|s| matches!(&s.shape,egui::Shape::Mesh(m) if m.texture_id==texture))
        );
        // Hidden request handles cannot capture clicks. Showing preferences
        // makes the target marker visible and selectable again.
        app.show_preferences=true;
        frame(&ctx,&mut app,vec![]);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(start), button(start, true)],
        );
        frame(&ctx, &mut app, vec![button(start, false)]);
        assert_eq!(app.selected, Some(0));
        assert_eq!(app.project, before);
    }
    #[test]
    fn read_only_map_click_deselects_without_mutating_or_cancelling_generation() {
        for busy in [false,true] {
            let ctx=egui::Context::default();let mut app=fixture(&ctx);
            let path=app.project.targets[0].prefab_path.clone().unwrap();app.prefabs.seed_test_stamp(&ctx,&mut app.project,&path);app.prefabs.show_stamp=true;
            let (_sender,receiver)=std::sync::mpsc::channel();
            if busy {app.task=Some(Task{ordered_terrain:false,key:app.project.terrain_key(),receiver,cancel:std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),progress:std::sync::Arc::new(std::sync::Mutex::new(None))});}
            else {app.project.size=2000;assert!(!app.preview_current());}
            let before=app.project.clone();let blank=egui::pos2(850.,620.);frame(&ctx,&mut app,vec![]);
            frame(&ctx,&mut app,vec![egui::Event::PointerMoved(blank),button(blank,true)]);
            frame(&ctx,&mut app,vec![button(blank,false)]);
            assert_eq!(app.selected,None);assert_eq!(app.project,before);assert!(app.drag_before.is_none());
            if let Some(task)=&app.task {assert!(!task.cancel.load(Ordering::Relaxed));}
        }
    }
    #[test]
    fn press_started_during_generation_cannot_turn_into_edit_after_completion() {
        for busy_before_press in [true,false] {
            let ctx=egui::Context::default();let mut app=fixture(&ctx);
            let start=first_target(frame(&ctx,&mut app,vec![]));let before=app.project.clone();
            let pending=|app:&mut Studio| {
                let (sender,receiver)=std::sync::mpsc::channel();
                app.task=Some(Task{ordered_terrain:false,key:app.project.terrain_key(),receiver,cancel:std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),progress:std::sync::Arc::new(std::sync::Mutex::new(None))});
                sender
            };
            let mut _sender=None;
            if busy_before_press {_sender=Some(pending(&mut app));}
            frame(&ctx,&mut app,vec![egui::Event::PointerMoved(start),button(start,true)]);
            if !busy_before_press {
                _sender=Some(pending(&mut app));
                frame(&ctx,&mut app,vec![egui::Event::PointerMoved(start+egui::vec2(1.,1.))]);
            }
            app.task=None;
            let end=start+egui::vec2(100.,50.);frame(&ctx,&mut app,vec![egui::Event::PointerMoved(end)]);frame(&ctx,&mut app,vec![button(end,false)]);
            assert_eq!(app.project,before);assert!(app.drag_before.is_none());assert!(app.drag_splat.is_none());
        }
    }
    #[test]
    fn wheel_during_quick_monument_drag_rotates_without_zooming_or_changing_translation() {
        for one_frame in [false, true] {
            let run = |with_wheel| {
                let ctx = egui::Context::default();
                let mut app = fixture(&ctx);
                let before = app.project.clone();
                let start = first_target(frame(&ctx, &mut app, vec![]));
                let end = start + egui::vec2(80., 40.);
                let press = vec![egui::Event::PointerMoved(start), button(start, true)];
                let mut events = if one_frame { press.clone() } else {
                    frame(&ctx, &mut app, press);
                    Vec::new()
                };
                events.push(egui::Event::PointerMoved(end));
                if with_wheel {
                    events.push(egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        phase: egui::TouchPhase::Move,
                        delta: egui::vec2(0., 60.),
                        modifiers: egui::Modifiers::NONE,
                    });
                }
                events.push(button(end, false));
                frame(&ctx, &mut app, events);
                assert!(app.drag_before.is_none());
                let result = (app.project.targets[0].clone(), app.zoom, app.pan);
                let edited = app.project.clone();
                app.history.undo(&mut app.project);
                assert_eq!(app.project, before);
                app.history.redo(&mut app.project);
                assert_eq!(app.project, edited);
                // Once released, an ordinary wheel gesture still zooms.
                frame(&ctx, &mut app, vec![egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    phase: egui::TouchPhase::Move,
                    delta: egui::vec2(0., 60.),
                    modifiers: egui::Modifiers::NONE,
                }]);
                assert!(app.zoom > result.1);
                result
            };
            let (plain, zoom, pan) = run(false);
            let (rotated, wheel_zoom, wheel_pan) = run(true);
            assert_eq!(wheel_zoom, zoom, "Wheel zoomed a primary-button gesture");
            assert_eq!(wheel_pan, pan);
            assert_eq!([rotated.x, rotated.z], [plain.x, plain.z]);
            assert_eq!(rotated.yaw, Some((plain.yaw.unwrap_or(0.) + 15.).rem_euclid(360.)));
        }
    }
    #[test]
    fn fast_pointer_drag_selects_by_press_origin_and_undoes_as_one_step() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        let before = app.project.clone();
        let start = first_target(frame(&ctx, &mut app, vec![]));
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(start), button(start, true)],
        );
        let end = start + egui::vec2(100., 50.);
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(end)]);
        frame(&ctx, &mut app, vec![button(end, false)]);
        assert_ne!(app.project.targets[0].x, before.targets[0].x);
        assert_ne!(app.project.targets[0].z, before.targets[0].z);
        assert!(app.drag_before.is_none());
        let edited = app.project.clone();
        app.history.undo(&mut app.project);
        assert_eq!(app.project, before);
        app.history.redo(&mut app.project);
        assert_eq!(app.project, edited);
    }
    #[test]
    fn monument_drag_applies_final_release_position_before_recording_undo() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        let before = app.project.clone();
        let start = first_target(frame(&ctx, &mut app, vec![]));
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(start), button(start, true)],
        );
        let middle = start + egui::vec2(40., 20.);
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(middle)]);
        let intermediate = app.project.targets[0].clone();
        let end = start + egui::vec2(80., 40.);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(end), button(end, false)],
        );
        let final_target = &app.project.targets[0];
        assert!((final_target.x - (2. * intermediate.x - before.targets[0].x)).abs() < 0.01);
        assert!((final_target.z - (2. * intermediate.z - before.targets[0].z)).abs() < 0.01);
        assert!(app.drag_before.is_none());
        let edited = app.project.clone();
        app.history.undo(&mut app.project);
        assert_eq!(app.project, before);
        app.history.redo(&mut app.project);
        assert_eq!(app.project, edited);
    }
    #[test]
    fn quick_monument_drag_uses_owned_press_even_when_release_arrives_with_movement() {
        for one_frame in [false, true] {
            let ctx = egui::Context::default();
            let mut app = fixture(&ctx);
            let before = app.project.clone();
            let start = first_target(frame(&ctx, &mut app, vec![]));
            let end = start + egui::vec2(80., 40.);
            let press = vec![egui::Event::PointerMoved(start), button(start, true)];
            if one_frame {
                let mut events = press;
                events.extend([egui::Event::PointerMoved(end), button(end, false)]);
                frame(&ctx, &mut app, events);
            } else {
                frame(&ctx, &mut app, press);
                frame(
                    &ctx,
                    &mut app,
                    vec![egui::Event::PointerMoved(end), button(end, false)],
                );
            }
            assert!(app.project.targets[0].x > before.targets[0].x);
            assert!(app.project.targets[0].z < before.targets[0].z);
            assert!(app.drag_before.is_none());
            assert!(app.drag_press.is_none());
            let edited = app.project.clone();
            app.history.undo(&mut app.project);
            assert_eq!(app.project, before);
            app.history.redo(&mut app.project);
            assert_eq!(app.project, edited);
        }
    }
    #[test]
    fn outside_presses_cannot_capture_and_lost_pointer_cancels_monument_move() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        let before = app.project.clone();
        let start = first_target(frame(&ctx, &mut app, vec![]));
        let outside = egui::pos2(-30., -30.);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(outside), button(outside, true)],
        );
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(start), button(start, false)],
        );
        assert_eq!(app.project, before);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(start), button(start, true)],
        );
        let end = start + egui::vec2(80., 40.);
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(end)]);
        assert_ne!(app.project, before);
        frame(&ctx, &mut app, vec![egui::Event::PointerGone]);
        assert_eq!(app.project, before);
        assert!(app.drag_before.is_none());
        assert!(app.drag_origin.is_none());
        assert!(app.drag_press.is_none());
        frame(&ctx, &mut app, vec![button(end, false)]);
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(end)]);
        assert_eq!(app.project, before);
    }
    #[test]
    fn captured_monument_release_outside_viewport_uses_final_clamped_position() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        let before = app.project.clone();
        let start = first_target(frame(&ctx, &mut app, vec![]));
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(start), button(start, true)],
        );
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(start + egui::vec2(30., 20.))],
        );
        let outside = egui::pos2(3000., -3000.);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(outside), button(outside, false)],
        );
        let half = app.project.size as f32 * 0.5;
        assert_eq!(app.project.targets[0].x, half);
        assert_eq!(app.project.targets[0].z, half);
        assert!(app.drag_before.is_none());
        app.history.undo(&mut app.project);
        assert_eq!(app.project, before);
    }
    #[test]
    fn dragging_wheel_rotates_without_zoom_and_undoes_position_with_orientation() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        let before = app.project.clone();
        let start = first_target(frame(&ctx, &mut app, vec![]));
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(start), button(start, true)],
        );
        let end = start + egui::vec2(80., 40.);
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(end)]);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                phase: egui::TouchPhase::Move,
                delta: egui::vec2(0., 1.),
                modifiers: egui::Modifiers::NONE,
            }],
        );
        frame(&ctx, &mut app, vec![button(end, false)]);
        assert_eq!(app.project.targets[0].yaw, Some(15.));
        for _ in 0..8 {
            frame(&ctx, &mut app, vec![]);
        }
        assert_eq!(app.zoom, 1.);
        app.project.validate().unwrap();
        app.history.undo(&mut app.project);
        assert_eq!(app.project, before);
    }
    #[test]
    fn dragging_verified_splat_reuses_texture_and_undoes_the_pose_together() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        let path = app.project.targets[0].prefab_path.clone().unwrap();
        let id = app.prefabs.seed_test_stamp(&ctx, &mut app.project, &path);
        let before = app.project.clone();
        let start = first_target(frame(&ctx, &mut app, vec![]));
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(start), button(start, true)],
        );
        let end = start + egui::vec2(70., 40.);
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(end)]);
        let mesh = |output: egui::FullOutput| {
            output
                .shapes
                .into_iter()
                .find_map(|s| {
                    if let egui::Shape::Mesh(m) = s.shape {
                        (m.texture_id == id).then_some(m)
                    } else {
                        None
                    }
                })
                .unwrap()
        };
        let first = mesh(frame(&ctx, &mut app, vec![]));
        assert_eq!(first.vertices.len(), 4);
        let next = end + egui::vec2(30., -20.);
        let rotated = mesh(frame(
            &ctx,
            &mut app,
            vec![
                egui::Event::PointerMoved(next),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    phase: egui::TouchPhase::Move,
                    delta: egui::vec2(0., 1.),
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        ));
        assert_ne!(first.vertices[0].pos, rotated.vertices[0].pos);
        assert_eq!(first.texture_id, rotated.texture_id);
        assert_eq!(first.vertices[0].uv, rotated.vertices[0].uv);
        assert_eq!(app.project.targets[0].yaw, Some(52.));
        assert_eq!(app.zoom, 1.);
        frame(&ctx, &mut app, vec![button(next, false)]);
        assert!(app.requested_footprint());
        let retained = mesh(frame(&ctx, &mut app, vec![]));
        assert_eq!(
            retained.texture_id, id,
            "Dropping must retain the requested splat"
        );
        assert!(
            app.requested_footprint_status()
                .unwrap()
                .contains("not applied")
        );
        app.project.validate().unwrap();
        app.history.undo(&mut app.project);
        assert_eq!(app.project, before);
    }
    #[test]
    fn relocated_monument_request_is_picked_and_dragged_from_its_visible_pose() {
        let ctx=egui::Context::default();let mut app=fixture(&ctx);
        let path=app.project.targets[0].prefab_path.clone().unwrap();let texture=app.prefabs.seed_test_stamp(&ctx,&mut app.project,&path);
        app.preview.as_mut().unwrap().key=app.project.terrain_key();let before=app.project.clone();let target=before.targets[0].clone();
        deliver_layout(&mut app,before.clone(),false);let active=app.active_layout.unwrap();
        app.layouts[active].solution.outcomes.iter_mut().find(|o|o.id==target.instance_id).unwrap().position=Some(papyrus_terrain::placement::Position{x:target.x+300.,y:30.,z:target.z+200.,yaw:123.,clearance:80.});
        app.placement_request=Some(PlacementRequest{recipe:before.clone(),id:target.instance_id});
        let output=frame(&ctx,&mut app,vec![]);
        let center=placed_marker_point(&app,&output,0);
        // Requested paint is deliberately last; the accepted pose shares this texture.
        let mesh=output.shapes.iter().rev().find_map(|s|match &s.shape{egui::Shape::Mesh(m) if m.texture_id==texture=>Some(m),_=>None}).unwrap();
        let start=mesh.vertices[1].pos.lerp(mesh.vertices[2].pos,0.5).lerp(mesh.vertices[0].pos.lerp(mesh.vertices[3].pos,0.5),0.1);
        assert!(start.distance(center)>22.);
        frame(&ctx,&mut app,vec![egui::Event::PointerMoved(start),button(start,true)]);frame(&ctx,&mut app,vec![button(start,false)]);
        assert_eq!(app.selected,Some(0));assert_eq!(app.project,before);
        frame(&ctx,&mut app,vec![egui::Event::PointerMoved(start),button(start,true)]);
        let end=start+egui::vec2(40.,25.);frame(&ctx,&mut app,vec![egui::Event::PointerMoved(end)]);
        assert_eq!(app.drag_origin,Some([target.x,target.z]));assert_eq!(app.project.targets[0].yaw,Some(37.));
        frame(&ctx,&mut app,vec![button(end,false)]);assert!(app.requested_footprint());
        app.history.undo(&mut app.project);assert_eq!(app.project,before);
    }
    #[test]
    fn hidden_request_marker_cannot_steal_a_drag_from_the_accepted_stamp() {
        let ctx=egui::Context::default();let mut app=fixture(&ctx);
        let path=app.project.targets[0].prefab_path.clone().unwrap();app.prefabs.seed_test_stamp(&ctx,&mut app.project,&path);
        app.preview.as_mut().unwrap().key=app.project.terrain_key();let before=app.project.clone();let target=before.targets[0].clone();
        deliver_layout(&mut app,before.clone(),false);let active=app.active_layout.unwrap();
        let actual=[target.x+40.,target.z,123.];
        app.layouts[active].solution.outcomes.iter_mut().find(|o|o.id==target.instance_id).unwrap().position=Some(papyrus_terrain::placement::Position{x:actual[0],y:30.,z:actual[1],yaw:actual[2],clearance:80.});
        let start=first_target(frame(&ctx,&mut app,vec![]));
        assert!(app.prefabs.stamp(&app.project,&path).unwrap().hit_test([target.x,target.z],actual,false));
        app.selected=None;app.show_preferences=false;frame(&ctx,&mut app,vec![]);
        frame(&ctx,&mut app,vec![egui::Event::PointerMoved(start),button(start,true)]);
        let end=start+egui::vec2(40.,25.);frame(&ctx,&mut app,vec![egui::Event::PointerMoved(end)]);
        assert_eq!(app.selected,Some(0));assert_eq!(app.drag_origin,Some([actual[0],actual[1]]));assert_eq!(app.project.targets[0].yaw,Some(actual[2]));
        frame(&ctx,&mut app,vec![button(end,false)]);app.history.undo(&mut app.project);assert_eq!(app.project,before);
    }
    fn composed_stamp_fixture(ctx: &egui::Context, view: mapper_studio::native_view::View) -> (Studio, egui::TextureId) {
        let mut app=fixture(ctx);app.project.targets.truncate(1);
        let path=app.project.targets[0].prefab_path.clone().unwrap();
        let stamp=app.prefabs.seed_test_stamp(ctx,&mut app.project,&path);
        app.prefabs.show_stamp=true;app.project.targets[0].yaw=Some(37.);
        app.project.placement_engine=mapper_studio::placement_preview::ROTATION_VERSION.into();
        app.project.composition_engine=Some(papyrus_terrain::monument_composition::VERSION.into());
        app.preview.as_mut().unwrap().key=app.project.terrain_key();
        let recipe=app.project.clone();deliver_layout(&mut app,recipe,false);
        let target=&app.project.targets[0];let active=app.active_layout.unwrap();
        let position=app.layouts[active].solution.outcomes[0].position.as_mut().unwrap();
        position.x=target.x;position.z=target.z;position.yaw=37.;
        app.composition.seed_test_view(ctx,&app.project);app.native_view=view;
        (app,stamp)
    }
    fn has_stamp_mesh(output: &egui::FullOutput, texture: egui::TextureId) -> bool {
        output.shapes.iter().any(|s|matches!(&s.shape,egui::Shape::Mesh(m) if m.texture_id==texture))
    }
    #[test]
    fn composed_drag_changes_background_in_the_movement_frame_and_drops_one_stamp() {
        let ctx=egui::Context::default();let(mut app,stamp)=composed_stamp_fixture(&ctx,mapper_studio::native_view::View::Overview);
        let clean=app.texture.as_ref().unwrap().id();
        let baked=app.composition.view(&app.project).unwrap().texture.id();
        let start=first_target(frame(&ctx,&mut app,vec![]));
        frame(&ctx,&mut app,vec![egui::Event::PointerMoved(start),button(start,true)]);
        let end=start+egui::vec2(65.,35.);
        let moved=frame(&ctx,&mut app,vec![egui::Event::PointerMoved(end)]);
        assert!(has_stamp_mesh(&moved,clean),"The first movement frame must already use pristine terrain");
        assert!(!has_stamp_mesh(&moved,baked),"Old composed paint must not ghost behind the moved stamp");
        let dropped=frame(&ctx,&mut app,vec![button(end,false)]);
        assert_eq!(dropped.shapes.iter().filter(|s|matches!(&s.shape,egui::Shape::Mesh(m) if m.texture_id==stamp)).count(),1,
            "Release must retain only the requested stamp, without a one-frame old accepted copy");
        assert!(app.requested_footprint());
    }

    #[test]
    fn quick_ground_drag_does_not_reuse_the_previous_composed_layer() {
        let ctx=egui::Context::default();let(mut app,_)=composed_stamp_fixture(&ctx,mapper_studio::native_view::View::Ground);
        let clean=app.texture.as_ref().unwrap().id();
        let start=first_target(frame(&ctx,&mut app,vec![]));
        let old_layer=app.layer_texture.as_ref().unwrap().id();
        let end=start+egui::vec2(65.,35.);
        let dropped=frame(&ctx,&mut app,vec![egui::Event::PointerMoved(start),button(start,true),
            egui::Event::PointerMoved(end),button(end,false)]);
        assert!(app.drag_before.is_none());assert!(app.requested_footprint());
        assert!(!has_stamp_mesh(&dropped,old_layer),"Fast release must not restore old composed diagnostic paint");
        assert!(has_stamp_mesh(&dropped,clean));
    }

    #[test]
    fn speculative_stamp_shades_against_the_displayed_composed_terrain() {
        let ctx=egui::Context::default();
        let(mut app,_)=composed_stamp_fixture(&ctx,mapper_studio::native_view::View::Overview);
        let active=app.active_layout.unwrap();
        app.layouts[active].solution.outcomes[0].position.as_mut().unwrap().x+=100.;
        app.preview.as_mut().unwrap().native_layers=Some(std::sync::Arc::new(papyrus_terrain::native_layers::Layers {
            size:app.project.size,terrain_seed:1,biome_seed:2,height_resolution:2,surface_resolution:2,
            height:vec![18000;4],biome:[vec![255;4],vec![0;16]].concat(),
            splat:[vec![255;4],vec![0;28]].concat(),topology:vec![0;4],
            normalized_biome_cells:0,normalized_splat_cells:0,
        }));
        let view=app.composition.view(&app.project).unwrap();
        let baked=view.texture.id();
        let target=&app.project.targets[0];
        let expected=mapper_studio::splat_preview::contextual_image(
            &egui::ColorImage::filled([2,2],Color32::WHITE),
            mapper_studio::splat_preview::Geometry{size:[180.,120.],offset:[10.,20.]},
            &view.maps.layers,[target.x,target.z,37.]);
        let mut output=ctx.run_ui(egui::RawInput{
            screen_rect:Some(Rect::from_min_size(Pos2::ZERO,egui::vec2(900.,700.))),focused:true,
            ..Default::default()
        },|ui|app.map(ui));
        let stamp=output.shapes.iter().find_map(|s|match &s.shape {
            egui::Shape::Mesh(m) if m.vertices.len()==4 && m.texture_id!=baked=>Some(m.texture_id),
            _=>None,
        }).expect("A distinct requested pose must draw a speculative stamp");
        let image=output.textures_delta.set.iter().find_map(|(id,deltas)| {
            if *id!=stamp {return None;} let egui::ImageData::Color(image)=&deltas.last()?.image;Some(image)
        }).expect("The initial stamp image must be uploaded");
        let actual=image.pixels.clone();output.textures_delta.clear();
        assert_eq!(actual,expected.pixels,"Stamp lighting and tint must use the same terrain as its composed map background");
    }
    #[test]
    fn selected_stamp_does_not_repaint_its_current_composed_pose() {
        for view in [mapper_studio::native_view::View::Overview, mapper_studio::native_view::View::Ground] {
            let ctx=egui::Context::default();let (mut app,stamp)=composed_stamp_fixture(&ctx,view);
            let before=app.project.clone();
            assert!(!has_stamp_mesh(&frame(&ctx,&mut app,vec![]),stamp),
                "Selected preview must not paint a second layer over its accepted composition: {view:?}");
            assert_eq!(app.project,before);assert_eq!(app.selected,Some(0));
            let blank=egui::pos2(850.,620.);
            frame(&ctx,&mut app,vec![egui::Event::PointerMoved(blank),button(blank,true)]);
            let output=frame(&ctx,&mut app,vec![button(blank,false)]);
            assert_eq!(app.selected,None);assert_eq!(app.project,before);assert!(!has_stamp_mesh(&output,stamp));
        }
    }
    #[test]
    fn composed_stamp_preview_stays_visible_for_distinct_requests_stages_and_dragging() {
        for state in ["relocated","rotated","stage","inspection","drag"] {
            let ctx=egui::Context::default();let (mut app,_)=composed_stamp_fixture(&ctx,mapper_studio::native_view::View::Overview);
            let before=app.project.clone();
            match state {
                "relocated"|"rotated"=> {
                    let active=app.active_layout.unwrap();let p=app.layouts[active].solution.outcomes[0].position.as_mut().unwrap();
                    if state=="relocated" {p.x+=100.;}else{p.yaw+=60.;}
                    app.placement_request=Some(PlacementRequest{recipe:before.clone(),id:before.targets[0].instance_id});
                    assert!(app.requested_footprint());
                },
                "stage"=>app.stage_texture=Some(ctx.load_texture("test-generation-stage",egui::ColorImage::filled([2,2],Color32::BLUE),egui::TextureOptions::LINEAR)),
                "inspection"=>app.native_view=mapper_studio::native_view::View::Heights,
                "drag"=>{},
                _=>unreachable!(),
            }
            let output=if state=="drag" {
                let start=first_target(frame(&ctx,&mut app,vec![]));
                frame(&ctx,&mut app,vec![egui::Event::PointerMoved(start),button(start,true)]);
                let end=start+egui::vec2(40.,25.);
                let output=frame(&ctx,&mut app,vec![egui::Event::PointerMoved(end)]);
                assert!(app.drag_before.is_some());assert_eq!(app.project.targets[0].yaw,Some(37.));output
            }else{frame(&ctx,&mut app,vec![])};
            let target=&app.project.targets[0];
            let displayed_stamp=app.prefabs.stamp(&app.project,target.prefab_path.as_deref().unwrap()).unwrap()
                .texture_at_background(&ctx,app.map_stamp_context(),(target.instance_id,true),
                    [target.x,target.z,target.yaw.unwrap_or(37.)],
                    app.drag_before.is_some() || app.native_view==mapper_studio::native_view::View::Overview);
            assert!(has_stamp_mesh(&output,displayed_stamp),"An uncommitted {state} preview must remain visible");
            if state!="drag" {assert_eq!(app.project,before);}
        }
    }
    #[test]
    fn composed_stamp_pose_matches_wrapped_and_authored_default_yaw() {
        for yaw in [None,Some(37.),Some(397.),Some(-323.)] {
            let ctx=egui::Context::default();let (mut app,stamp)=composed_stamp_fixture(&ctx,mapper_studio::native_view::View::Overview);
            app.project.targets[0].yaw=yaw;
            let active=app.active_layout.unwrap();app.layouts[active].recipe=app.project.clone();
            app.composition.seed_test_view(&ctx,&app.project);
            assert!(!has_stamp_mesh(&frame(&ctx,&mut app,vec![]),stamp));
        }
    }
    #[test]
    fn pending_rotation_marker_keeps_requested_heading_at_the_same_origin() {
        let ctx=egui::Context::default();let mut app=fixture(&ctx);
        let path=app.project.targets[0].prefab_path.clone().unwrap();app.prefabs.seed_test_stamp(&ctx,&mut app.project,&path);
        app.project.targets[0].yaw=Some(37.);app.project.placement_engine=mapper_studio::placement_preview::ROTATION_VERSION.into();
        app.preview.as_mut().unwrap().key=app.project.terrain_key();let before=app.project.clone();let id=before.targets[0].instance_id;
        deliver_layout(&mut app,before.clone(),false);let active=app.active_layout.unwrap();
        let accepted=app.layouts[active].solution.outcomes.iter_mut().find(|o|o.id==id).unwrap().position.as_mut().unwrap();
        accepted.x=before.targets[0].x;accepted.z=before.targets[0].z;accepted.yaw=123.;
        app.placement_request=Some(PlacementRequest{recipe:before.clone(),id});assert!(app.requested_footprint());
        let start=first_target(frame(&ctx,&mut app,vec![]));frame(&ctx,&mut app,vec![egui::Event::PointerMoved(start),button(start,true)]);
        let end=start+egui::vec2(40.,25.);frame(&ctx,&mut app,vec![egui::Event::PointerMoved(end)]);
        assert_eq!(app.project.targets[0].yaw,Some(37.));frame(&ctx,&mut app,vec![button(end,false)]);
        app.history.undo(&mut app.project);assert_eq!(app.project,before);
    }
    #[test]
    fn moving_generated_stamp_preserves_grab_offset_yaw_and_cached_terrain() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        let path = app.project.targets[0].prefab_path.clone().unwrap();
        app.prefabs.seed_test_stamp(&ctx, &mut app.project, &path);
        app.preview.as_mut().unwrap().key = app.project.terrain_key();
        let before = app.project.clone();
        deliver_layout(&mut app, before.clone(), false);
        let active = app.active_layout.unwrap();
        let target = &app.project.targets[0];
        let id = target.instance_id;
        let actual = [target.x + 240., target.z + 160., 123.];
        app.layouts[active]
            .solution
            .outcomes
            .iter_mut()
            .find(|o| o.id == id)
            .unwrap()
            .position = Some(papyrus_terrain::placement::Position {
            x: actual[0],
            y: 30.,
            z: actual[1],
            yaw: actual[2],
            clearance: 80.,
        });
        // Grab the authored offset center rather than the monument origin;
        // movement must retain that offset and the accepted orientation.
        let output = frame(&ctx, &mut app, vec![]);
        let mesh = output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Mesh(m)
                    if m.texture_id
                        == app.prefabs.stamp(&app.project, &path).unwrap().texture.id() =>
                {
                    Some(m)
                }
                _ => None,
            })
            .unwrap();
        let center = (mesh.vertices[0].pos.to_vec2() + mesh.vertices[2].pos.to_vec2()) * 0.5;
        let start = egui::pos2(center.x, center.y);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(start), button(start, true)],
        );
        let end = start + egui::vec2(40., 25.);
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(end)]);
        assert_eq!(app.selected, Some(0));
        assert_eq!(app.drag_origin, Some([actual[0], actual[1]]));
        assert_eq!(app.project.targets[0].yaw, Some(123.));
        assert!(app.project.targets[0].x > actual[0]);
        assert!(app.project.targets[0].z < actual[1]);
        assert_eq!(app.project.terrain_key(), before.terrain_key());
        frame(&ctx, &mut app, vec![button(end, false)]);
        assert!(app.drag_origin.is_none());
        app.history.undo(&mut app.project);
        assert_eq!(app.project, before);
    }
    #[test]
    fn custom_colors_survive_composed_stamp_drag_deselection_and_undo() {
        let ctx=egui::Context::default();let mut app=fixture(&ctx);
        app.project.size=1000;app.project.targets.truncate(1);
        app.project.placement_engine=mapper_studio::placement_preview::ROTATION_VERSION.into();
        app.project.targets[0].x=0.;app.project.targets[0].z=0.;app.project.targets[0].yaw=Some(0.);
        let path=app.project.targets[0].prefab_path.clone().unwrap();
        let palette=app.prefabs.seed_test_cartographic_stamp(&ctx,&mut app.project,&path,Some(0));
        app.preview.as_mut().unwrap().key=app.project.terrain_key();
        let recipe=app.project.clone();deliver_layout(&mut app,recipe.clone(),false);
        app.layouts[app.active_layout.unwrap()].solution.outcomes[0].position=Some(papyrus_terrain::placement::Position{x:0.,y:30.,z:0.,yaw:0.,clearance:80.});
        app.composition.seed_test_view(&ctx,&app.project);
        app.composition.seed_test_palette(palette.clone());
        let mut layers=app.composition.view(&app.project).unwrap().maps.layers.clone();
        layers.terrain_seed=app.project.seeds[0].value;layers.biome_seed=app.project.seeds[1].value;
        let layers=std::sync::Arc::new(layers);
        let preview=app.preview.as_mut().unwrap();preview.palette=Some(palette.clone());preview.native_layers=Some(layers.clone());
        app.preferences.map_colors.enabled=true;app.preferences.map_colors.overrides[4]=Some([255,0,0]);
        let deadline=std::time::Instant::now()+std::time::Duration::from_secs(90);
        loop {
            app.refresh_map_colors(&ctx);
            if app.map_colors.view().is_some(){break;}
            assert!(std::time::Instant::now()<deadline,"Color worker did not complete: {}",app.map_colors.message);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let composed=app.map_colors.view().unwrap().texture.id();
        let clean=app.map_colors.clean_view(&layers,&None,&palette,&app.preferences.map_colors).unwrap().texture.id();
        assert_ne!(clean,composed);
        let start=first_target(frame(&ctx,&mut app,vec![]));
        frame(&ctx,&mut app,vec![egui::Event::PointerMoved(start),button(start,true)]);
        let end=start+egui::vec2(50.,30.);
        let output=frame(&ctx,&mut app,vec![egui::Event::PointerMoved(end)]);
        assert!(app.drag_before.is_some());
        assert_eq!(app.drag_splat.as_ref().unwrap().id(),clean);
        assert_eq!(app.map_background_texture().unwrap().id(),clean);
        assert!(has_stamp_mesh(&output,clean));
        frame(&ctx,&mut app,vec![button(end,false)]);
        let moved=app.project.clone();let blank=egui::pos2(850.,620.);
        frame(&ctx,&mut app,vec![egui::Event::PointerMoved(blank),button(blank,true)]);
        frame(&ctx,&mut app,vec![button(blank,false)]);
        assert_eq!(app.selected,None);assert_eq!(app.project,moved);
        assert_eq!(app.map_background_texture().unwrap().id(),clean);
        app.history.undo(&mut app.project);
        assert_eq!(app.project,recipe);
        frame(&ctx,&mut app,vec![]);
        assert_eq!(app.map_background_texture().unwrap().id(),composed);
    }
    #[test]
    fn native_viewport_keeps_layer_tabs_outside_the_full_canvas() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        app.preview.as_mut().unwrap().native_layers =
            Some(std::sync::Arc::new(papyrus_terrain::native_layers::Layers {
                size: app.project.size,
                terrain_seed: 1,
                biome_seed: 2,
                height_resolution: 2,
                surface_resolution: 2,
                height: vec![18000; 4],
                splat: vec![0; 32],
                biome: vec![0; 20],
                topology: vec![0; 4],
                normalized_biome_cells: 0,
                normalized_splat_cells: 0,
            }));
        for width in [430., 900.] {
            let mut bounds = Rect::NOTHING;
            let mut frame = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(width, 650.))),
                    ..Default::default()
                },
                |ui| {
                    bounds = ui.available_rect_before_wrap();
                    app.map(ui);
                },
            );
            frame.textures_delta.clear();
            let canvas = frame
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect) if rect.fill == MAP_BACKGROUND => Some(rect.rect),
                    _ => None,
                })
                .expect("map canvas");
            assert!(
                (canvas.left() - bounds.left()).abs() <= 1.
                    && (bounds.right() - canvas.right() - 46.).abs() <= 1.,
                "Canvas {canvas:?}, viewport {bounds:?}"
            );
        }
    }

    #[test]
    fn native_drag_matches_clean_overview_shading_then_restores_selected_inspection_layer() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        app.project.size = 1000;
        let path = app.project.targets[0].prefab_path.clone().unwrap();
        let raw_stamp = app.prefabs.seed_test_stamp(&ctx, &mut app.project, &path);
        app.native_view = mapper_studio::native_view::View::Ground;
        let preview = app.preview.as_mut().unwrap();
        preview.key = app.project.terrain_key();
        let (hr, sr) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let mut splat = vec![0; sr * sr * 8];
        splat[..sr * sr].fill(255);
        preview.native_layers = Some(std::sync::Arc::new(papyrus_terrain::native_layers::Layers {
            size: 1000,
            terrain_seed: app.project.seeds[0].value,
            biome_seed: app.project.seeds[1].value,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![18000; hr * hr],
            biome: vec![0; sr * sr * 5],
            splat,
            topology: vec![0; sr * sr],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        }));
        let start = first_target(frame(&ctx, &mut app, vec![]));
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(start), button(start, true)],
        );
        let end = start + egui::vec2(40., 30.);
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(end)]);
        let id = app.drag_splat.as_ref().unwrap().id();
        let output = frame(&ctx, &mut app, vec![]);
        assert!(
            output
                .shapes
                .iter()
                .any(|s| matches!(&s.shape, egui::Shape::Mesh(m) if m.texture_id == id))
        );
        let t = &app.project.targets[0];
        let stamp = app.prefabs.stamp(&app.project, &path).unwrap();
        let contextual = stamp.texture_at(
            &ctx,
            app.preview.as_ref().unwrap().native_layers.as_ref(),
            (t.instance_id, true),
            [t.x, t.z, t.yaw.unwrap_or(stamp.default_yaw)],
            true,
        );
        assert_ne!(raw_stamp, contextual);
        assert!(
            output
                .shapes
                .iter()
                .any(|s| matches!(&s.shape,egui::Shape::Mesh(m) if m.texture_id==contextual))
        );
        frame(&ctx, &mut app, vec![button(end, false)]);
        assert!(app.drag_splat.is_none());
        assert_eq!(app.native_view, mapper_studio::native_view::View::Ground);
    }
    #[test]
    fn stale_preview_blocks_map_mutation_and_dirty_actions_require_a_decision() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        let start = first_target(frame(&ctx, &mut app, vec![]));
        app.project.shuffle_monuments();
        assert!(app.preview_current()); // Monument seeds do not invalidate base terrain.
        app.project.size = 2000;
        assert!(!app.preview_current());
        let before = app.project.clone();
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(start), button(start, true)],
        );
        let end = start + egui::vec2(100., 50.);
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(end)]);
        frame(&ctx, &mut app, vec![button(end, false)]);
        assert_eq!(app.project, before);
        app.request(Action::New, &ctx);
        assert!(app.pending.is_some());
        assert_eq!(app.project, before);
    }
    #[test]
    fn reused_terrain_preview_keeps_its_uploaded_texture() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        let original = app.texture.as_ref().unwrap().id();
        let preview = app.preview.as_ref().unwrap().clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        app.task = Some(Task {
            ordered_terrain: preview.ordered_terrain,
            key: preview.key.clone(),
            receiver,
            cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            progress: Default::default(),
        });
        sender.send(Ok(Some(preview))).unwrap();
        app.poll(&ctx);
        assert!(app.task.is_none());
        assert_eq!(app.texture.as_ref().unwrap().id(), original);
        assert!(app.placement_task.is_some());
    }
    #[test]
    fn stale_background_result_is_not_published() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        let old = app.preview.as_ref().unwrap().key.seed;
        let (sender, receiver) = std::sync::mpsc::channel();
        app.task = Some(Task {
            ordered_terrain: false,
            key: app.project.terrain_key(),
            receiver,
            cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            progress: std::sync::Arc::new(std::sync::Mutex::new(None)),
        });
        sender
            .send(Ok(Some(Preview {
                ordered_terrain: false,
                raw_source: None,
                rivers: None,
                key: mapper_studio::TerrainKey {
                    seed: old % MAX_SEED + 1,
                    ..app.project.terrain_key()
                },
                image: Arc::new(egui::ColorImage::filled([2, 2], Color32::RED)),
                display_image: None,
                landscape: None,
                heights: std::sync::Arc::new(vec![17000; 65 * 65]),
                resolution: 65,
                metrics: preview::Metrics::default(),
                palette: None,
            native_layers: None, surface:None,
                ocean: None,
            })))
            .unwrap();
        app.poll(&ctx);
        assert!(app.task.is_none());
        assert_eq!(app.preview.as_ref().unwrap().key.seed, old);
        assert!(app.status.contains("discarded"));
    }
    pub(super) fn deliver_layout(app: &mut Studio, recipe: Project, cancelled: bool) {
        let (solution_sender, receiver) = std::sync::mpsc::channel();
        let h = vec![17000; 65 * 65];
        let solution = mapper_studio::placement_preview::solve(&recipe, &h, 65, || false)
            .unwrap()
            .unwrap();
        app.placement_task = Some(LayoutTask {
            receiver,
            cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(cancelled)),
        });
        solution_sender
            .send(Ok(Some(Layout {
                recipe,
                solution,
                assets: None,
            })))
            .unwrap();
        app.poll_placement(&egui::Context::default());
    }
    #[test]
    fn export_uses_full_map_pixels_and_blocks_stale_or_unfinished_edits() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        let detailed = egui::ColorImage::filled([8, 8], Color32::BLUE);
        app.preview.as_mut().unwrap().display_image = Some(Arc::new(detailed.clone()));
        app.zoom = 3.;
        app.pan = egui::vec2(70., 90.);
        app.grid = true;
        app.show_preferences = true;
        app.camera.enabled = true;
        assert_eq!(app.exportable_map(), Some(&detailed));
        let original = app.project.clone();
        app.drag_before = Some(original.clone());
        assert!(app.exportable_map().is_none());
        app.drag_before = None;
        app.project.size = 2000;
        assert!(app.exportable_map().is_none());
        app.project = original;
        assert_eq!(app.exportable_map(), Some(&detailed));
        let recipe = app.project.clone();
        deliver_layout(&mut app, recipe, false);
        // Mark the preview as native: enabled accepted composition must finish before export.
        let (hr, sr) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        app.preview.as_mut().unwrap().native_layers =
            Some(std::sync::Arc::new(papyrus_terrain::native_layers::Layers {
                size: 1000,
                terrain_seed: 1,
                biome_seed: 2,
                height_resolution: hr,
                surface_resolution: sr,
                height: vec![0; hr * hr],
                biome: vec![0; sr * sr * 5],
                splat: vec![0; sr * sr * 8],
                topology: vec![0; sr * sr],
                normalized_biome_cells: 0,
                normalized_splat_cells: 0,
            }));
        assert!(app.exportable_map().is_none());
    }
    #[test]
    fn terrain_can_wait_for_automatic_assets_instead_of_failing_placement() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        app.project.placement_engine = mapper_studio::placement_preview::EARTHWORK_VERSION.into();
        let recipe = app.project.clone();
        app.start_placement(&ctx);
        assert!(app.placement_task.is_none());
        assert!(app.status.contains("Waiting for automatic"));
        assert_eq!(app.project, recipe);
    }
    #[test]
    fn requested_stamp_survives_rejection_then_hands_off_to_accepted_placement() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        let path = app.project.targets[0].prefab_path.clone().unwrap();
        let id = app.prefabs.seed_test_stamp(&ctx, &mut app.project, &path);
        app.preview.as_mut().unwrap().key = app.project.terrain_key();
        let recipe = app.project.clone();
        app.placement_request = Some(PlacementRequest {
            recipe: recipe.clone(),
            id: recipe.targets[0].instance_id,
        });
        deliver_layout(&mut app, recipe, false);
        let active = app.active_layout.unwrap();
        let target = app.project.targets[0].clone();
        let outcome = app.layouts[active]
            .solution
            .outcomes
            .iter_mut()
            .find(|o| o.id == target.instance_id)
            .unwrap();
        outcome.position = None;
        outcome.reason = "Test site is submerged".into();
        let has_stamp = |output: egui::FullOutput| {
            output
                .shapes
                .iter()
                .any(|s| matches!(&s.shape,egui::Shape::Mesh(m) if m.texture_id==id))
        };
        assert!(
            app.requested_footprint_status()
                .unwrap()
                .contains("not placed: Test site is submerged")
        );
        assert!(has_stamp(frame(&ctx, &mut app, vec![])));
        app.layouts[active]
            .solution
            .outcomes
            .iter_mut()
            .find(|o| o.id == target.instance_id)
            .unwrap()
            .position = Some(papyrus_terrain::placement::Position {
            x: target.x,
            y: 20.,
            z: target.z,
            yaw: target.yaw.unwrap_or(0.),
            clearance: 100.,
        });
        assert!(!app.requested_footprint());
        assert!(
            has_stamp(frame(&ctx, &mut app, vec![])),
            "Accepted placement replaces the draft without losing its stamp"
        );
        app.project.targets[0].x += 3.;
        assert!(!app.requested_footprint());
        assert!(
            !has_stamp(frame(&ctx, &mut app, vec![])),
            "A stale request must not masquerade as a new edit"
        );
    }
    #[test]
    fn monument_names_only_render_when_hovering_requested_or_placed_centers() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        let text = |output: &egui::FullOutput| {
            output
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) => Some(t.galley.job.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let idle = frame(&ctx, &mut app, vec![egui::Event::PointerGone]);
        assert!(
            !text(&idle)
                .iter()
                .any(|s| s.ends_with("· requested") || s.ends_with("· placed"))
        );
        let center = first_target(idle);
        let hover = frame(&ctx, &mut app, vec![egui::Event::PointerMoved(center)]);
        assert!(text(&hover).iter().any(|s| s == "Launch Site · requested"));
        let recipe = app.project.clone();
        deliver_layout(&mut app, recipe, false);
        let placed = frame(&ctx, &mut app, vec![egui::Event::PointerGone]);
        let center = placed_marker_point(&app, &placed, 1);
        let hover = frame(&ctx, &mut app, vec![egui::Event::PointerMoved(center)]);
        assert!(text(&hover).iter().any(|s| s == "Satellite Dish · placed"));
        let idle = frame(&ctx, &mut app, vec![egui::Event::PointerGone]);
        assert!(
            !text(&idle)
                .iter()
                .any(|s| s.ends_with("· requested") || s.ends_with("· placed"))
        );
    }
    #[test]
    fn automatic_new_project_defaults_become_the_clean_initial_recipe() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        app.replace_project(Studio::new_workbench_project(), None);
        for stage in 0..2 {
            let pristine = app.pristine_new_project();
            assert!(pristine);
            let before = app.project.clone();
            if stage == 0 {
                app.project.preview_engine = mapper_studio::native_preview::VERSION.into();
            } else {
                app.project.targets = Project::default().targets;
            }
            app.history.record(before, &app.project);
            assert!(app.dirty() && app.history.can_undo());
            app.accept_new_project_defaults(pristine);
            assert!(!app.dirty());
            assert_eq!(app.saved, app.project);
            assert!(!app.history.can_undo() && !app.history.can_redo());
        }
        app.request(Action::New, &ctx);
        assert!(app.pending.is_none());
        assert!(!app.dirty());
    }

    #[test]
    fn new_project_initialization_preserves_user_edits_redo_and_generated_work() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        app.replace_project(Studio::new_workbench_project(), None);
        let initial = app.project.clone();
        app.project.size = 3000;
        app.history.record(initial.clone(), &app.project);
        assert!(!app.pristine_new_project());
        app.accept_new_project_defaults(app.pristine_new_project());
        assert_eq!(app.saved, initial);
        assert!(app.dirty() && app.history.can_undo());
        app.request(Action::New, &ctx);
        assert!(matches!(app.pending, Some(Action::New)));
        app.pending = None;
        app.history.undo(&mut app.project);
        assert!(!app.dirty() && app.history.can_redo());
        assert!(!app.pristine_new_project());
        app.accept_new_project_defaults(app.pristine_new_project());
        assert!(app.history.can_redo());
        app.history.redo(&mut app.project);
        assert_eq!(app.project.size, 3000);

        app.replace_project(Studio::new_workbench_project(), None);
        let pristine = app.pristine_new_project();
        app.generation.revision += 1;
        app.accept_new_project_defaults(pristine);
        assert!(app.dirty());
        app.request(Action::New, &ctx);
        assert!(matches!(app.pending, Some(Action::New)));

        app.replace_project(
            Studio::new_workbench_project(),
            Some(PathBuf::from("saved.mapper")),
        );
        assert!(!app.pristine_new_project());
        let saved = app.saved.clone();
        app.project.preview_engine = mapper_studio::native_preview::VERSION.into();
        app.accept_new_project_defaults(app.pristine_new_project());
        assert_eq!(app.saved, saved);
        assert!(app.dirty());
    }

    #[test]
    fn first_launch_prompts_for_installation_and_saved_folder_skips_setup() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        let project = app.project.clone();
        app.preferences.rust_folder = None;
        app.prepare_installation_startup();
        assert!(app.installation_setup_open);
        assert!(app.restore_source);
        assert!(app.task.is_none());
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| app.installation_setup_window(ui.ctx()));
        output.textures_delta.clear();
        assert_eq!(app.project, project);
        app.preferences.rust_folder = Some(PathBuf::from("saved-rust-installation"));
        app.prepare_installation_startup();
        assert!(!app.installation_setup_open);
        assert!(app.restore_source);
        assert!(app.task.is_none());
        assert_eq!(app.project, project);
    }
    #[test]
    fn application_preferences_persist_without_changing_the_recipe() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        app.settings_path = Some(path.clone());
        let recipe = app.project.clone();
        app.live_generation = false;
        app.grid = true;
        app.show_monuments = false;
        app.show_no_build = true;
        app.preferences.map_colors.enabled=true;
        app.preferences.map_colors.overrides[4]=Some([37,82,41]);
        app.persist_preferences();
        let restored = mapper_studio::preferences::Preferences::load(&path).unwrap();
        assert!(!restored.live_generation);
        assert!(restored.map_grid);
        assert!(restored.map_no_build);
        assert!(!restored.monument_splats);
        assert_eq!(restored.map_colors,app.preferences.map_colors);
        assert_eq!(app.project, recipe);
        assert!(!app.dirty());
        assert!(!app.settings_open);
    }
    #[test]
    fn named_installation_settings_fit_themes_and_switch_without_editing_recipe() {
        mod snapshot {include!("../examples/support/egui_snapshot.rs");}
        for dark in [false,true] {for (width,height,name) in [(900,650,"wide"),(430,650,"narrow")] {
            let ctx=egui::Context::default();ctx.set_visuals(if dark{egui::Visuals::dark()}else{egui::Visuals::light()});
            let mut app=fixture(&ctx);let recipe=app.project.clone();
            app.preferences.remember_installation("C:/Steam/steamapps/common/Rust".into(),true).unwrap();
            let staging=app.preferences.remember_installation("D:/SteamLibrary/steamapps/common/Rust - Staging".into(),false).unwrap();
            app.preferences.installations[0].name="Regular Rust".into();app.preferences.installations[1].name="Staging".into();
            let mut textures=snapshot::Textures::default();let mut last=None;
            for _ in 0..2 {let mut out=ctx.run_ui(egui::RawInput{screen_rect:Some(Rect::from_min_size(Pos2::ZERO,egui::vec2(width as f32,height as f32))),focused:true,..Default::default()},|ui|{app.installation_library.controls(ui,&mut app.preferences,false);});textures.update(&out.textures_delta);out.textures_delta.clear();last=Some(out);}
            let out=last.unwrap();
            let find=|label:&str|out.shapes.iter().find_map(|s|if let egui::Shape::Text(t)=&s.shape {if t.galley.job.text==label {Some(t.pos+t.galley.rect.center().to_vec2())}else{None}}else{None});
            assert!(find("Regular Rust").is_some());assert!(find("Staging").is_some());assert!(find("Reset cache").is_some());
            let use_button=find("Use").unwrap();assert!(use_button.x<width as f32);assert_eq!(app.project,recipe);
            if let Some(dir)=std::env::var_os("MAPPER_UI_SNAPSHOT_DIR") {let dir=PathBuf::from(dir);std::fs::create_dir_all(&dir).unwrap();let image=textures.raster(&ctx,out,[width,height]);mapper_studio::image_export::write_png(&dir.join(format!("installations-{}-{name}.png",if dark{"dark"}else{"light"})),&image,||false).unwrap();}
            for pressed in [true,false] {let mut out=ctx.run_ui(egui::RawInput{screen_rect:Some(Rect::from_min_size(Pos2::ZERO,egui::vec2(width as f32,height as f32))),events:vec![egui::Event::PointerMoved(use_button),button(use_button,pressed)],focused:true,..Default::default()},|ui|{
                let (_,action)=app.installation_library.controls(ui,&mut app.preferences,false);
                if !pressed {assert!(matches!(action,Some(installation_panel::Action::Select(_))));}
            });textures.update(&out.textures_delta);out.textures_delta.clear();}
            assert_eq!(app.preferences.selected_installation,Some(staging));assert_eq!(app.project,recipe);assert!(!app.history.can_undo());
        }}
    }
    #[test]
    fn terrain_memory_settings_fit_both_themes_and_compact_windows() {
        mod snapshot {
            include!("../examples/support/egui_snapshot.rs");
        }
        for dark in [false, true] {
            for (width, height, name) in [(960, 720, "wide"), (640, 600, "compact")] {
                let ctx = egui::Context::default();
                ctx.set_visuals(if dark {
                    egui::Visuals::dark()
                } else {
                    egui::Visuals::light()
                });
                workbench_style::install(&ctx);
                let mut app = fixture(&ctx);
                app.settings_open = true;
                app.settings_page = settings::Page::Performance;
                let recipe = app.project.clone();
                let mut textures = snapshot::Textures::default();
                let mut last = None;
                for frame in 0..3 {
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(
                                Pos2::ZERO,
                                egui::vec2(width as f32, height as f32),
                            )),
                            time: Some(frame as f64),
                            focused: true,
                            ..Default::default()
                        },
                        |ui| {
                            ui.label("Mapper test workspace");
                            app.settings_window(&ctx);
                        },
                    );
                    textures.update(&output.textures_delta);
                    output.textures_delta.clear();
                    last = Some(output);
                }
                let output = last.unwrap();
                for label in [
                    "CPU workers",
                    "Terrain memory (MiB)",
                    "Image memory (MiB)",
                    "Done",
                ] {
                    let text = output
                        .shapes
                        .iter()
                        .find_map(|shape| match &shape.shape {
                            egui::Shape::Text(text) if text.galley.job.text == label => {
                                Some((text, shape.clip_rect))
                            }
                            _ => None,
                        })
                        .unwrap_or_else(|| panic!("Missing {label} at {width}x{height}"));
                    let center = text.0.pos + text.0.galley.rect.center().to_vec2();
                    assert!(text.1.contains(center), "Clipped {label}");
                    assert!(
                        Rect::from_min_size(Pos2::ZERO, egui::vec2(width as f32, height as f32))
                            .contains(center)
                    );
                }
                assert_eq!(app.project, recipe);
                if let Some(directory) = std::env::var_os("MAPPER_UI_SNAPSHOT_DIR") {
                    let directory = PathBuf::from(directory);
                    std::fs::create_dir_all(&directory).unwrap();
                    let image = textures.raster(&ctx, output, [width, height]);
                    mapper_studio::image_export::write_png(
                        &directory.join(format!(
                            "terrain-memory-{}-{name}.png",
                            if dark { "dark" } else { "light" }
                        )),
                        &image,
                        || false,
                    )
                    .unwrap();
                }
            }
        }
    }

    #[test]
    fn settings_categories_keep_controls_visible_without_changing_the_recipe() {
        mod snapshot { include!("../examples/support/egui_snapshot.rs"); }
        for screen in [egui::vec2(960., 720.), egui::vec2(640., 600.)] {
            let ctx = egui::Context::default();
            ctx.set_theme(if screen.x > 640. { egui::Theme::Dark } else { egui::Theme::Light });
            workbench_style::install(&ctx);
            ctx.all_styles_mut(|style| style.animation_time = 0.);
            let mut textures = snapshot::Textures::default();
            let mut app = fixture(&ctx);
            app.settings_open = true;
            let recipe = app.project.clone();
            let preferences = app.preferences.clone();
            let mut outside = None;
            for (page, control, error) in [
                (settings::Page::Installations, "Add Rust folder…", false),
                (settings::Page::Performance, "CPU workers", false),
                (settings::Page::Appearance, "Use custom map colors", false),
                (settings::Page::Viewport, "Map grid", false),
                (settings::Page::Icons, "Restore all", false),
                (settings::Page::Updates, "Check for updates on launch", false),
                (settings::Page::Performance, "CPU workers", true),
            ] {
                app.settings_page = page;
                let message = if error {
                    "Could not save settings. Access denied. ".repeat(300)
                } else {
                    String::new()
                };
                app.settings_message = message.clone();
                let mut output = None;
                for _ in 0..2 {
                    let mut frame = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, screen)),
                            focused: true,
                            ..Default::default()
                        },
                        |_| app.settings_window(&ctx),
                    );
                    textures.update(&frame.textures_delta);
                    frame.textures_delta.clear();
                    output = Some(frame);
                }
                let output = output.unwrap();
                let mut expected = vec![control, "Done"];
                if error {
                    expected.push("Settings error");
                }
                for expected in expected {
                    let text = output.shapes.iter().find_map(|shape| {
                        if let egui::Shape::Text(text) = &shape.shape
                            && text.galley.job.text == expected {
                            Some(text)
                        } else { None }
                    }).unwrap_or_else(|| panic!("Missing settings control {expected} on {control} at {screen:?}. Painted text: {:?}",
                        output.shapes.iter().filter_map(|shape| if let egui::Shape::Text(text) = &shape.shape {
                            Some((&text.galley.job.text, text.pos, shape.clip_rect))
                        } else { None }).collect::<Vec<_>>()));
                    assert!(
                        Rect::from_min_size(Pos2::ZERO, screen)
                            .contains(text.pos + text.galley.rect.center().to_vec2()),
                        "Settings control {expected} left the window"
                    );
                }
                assert_eq!(app.project, recipe);
                assert_eq!(app.preferences, preferences);
                assert_eq!(app.settings_message, message);
                if !error && let Some(directory) = std::env::var_os("MAPPER_APP_SETTINGS_SNAPSHOTS") {
                    let size = [screen.x as usize, screen.y as usize];
                    let path = PathBuf::from(directory).join(format!("settings-{page:?}-{}.png", size[0]));
                    let image = textures.raster(&ctx, output, size);
                    mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
                }
            }
            let mut output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, screen)),
                focused: true,
                events: vec![egui::Event::Key {
                    key: egui::Key::Tab, physical_key: None, pressed: true,
                    repeat: false, modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            }, |ui| {
                outside = Some(ui.button("Behind settings").id);
                app.settings_window(&ctx);
            });
            output.textures_delta.clear();
            let focused = ctx.memory(|memory| memory.focused());
            assert!(focused.is_some(), "Settings must have a keyboard focus target");
            assert_ne!(focused, outside, "Tab escaped to the workbench behind Settings");
            let mut output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, screen)),
                focused: true,
                events: vec![egui::Event::ModifiersChanged(egui::Modifiers::COMMAND), egui::Event::Key {
                    key: egui::Key::N, physical_key: None, pressed: true,
                    repeat: false, modifiers: egui::Modifiers::COMMAND,
                }],
                ..Default::default()
            }, |ui| {
                app.toolbar(ui);
                app.settings_window(&ctx);
            });
            output.textures_delta.clear();
            assert!(app.pending.is_none(), "Project shortcuts must stay inactive in Settings");
            assert_eq!(app.project, recipe);
        }
    }
    #[test]
    fn custom_monument_icons_and_client_names_are_view_only_and_restore_cleanly() {
        mod snapshot {include!("../examples/support/egui_snapshot.rs");}
        for dark in [false,true] {for (width,height,name) in [(900,650,"wide"),(430,650,"narrow")] {
            let ctx=egui::Context::default();ctx.set_visuals(if dark{egui::Visuals::dark()}else{egui::Visuals::light()});
            let mut app=fixture(&ctx);app.project.targets.truncate(1);app.project.targets[0].name="compound".into();app.project.targets[0].prefab_path=Some("assets/bundled/prefabs/autospawn/monument/medium/compound.prefab".into());
            let path=app.project.targets[0].prefab_path.clone().unwrap();app.prefabs.seed_test_stamp(&ctx,&mut app.project,&path);
            let source=app.prefabs.catalog(&app.project).unwrap();let mut entries=source.entries.clone();
            let asset=entries.values().next().cloned().unwrap_or(mapper_studio::prefab_assets::Asset{priority:None,bounds:None,yaw:None,modifier_size:None,modifier_offset:None,texture_guids:Default::default(),splat_mask:None,modifier:None,modifier_absent:false,water_geometry:None,no_build_geometry:None,display_name:None});
            entries.insert(path.clone(),mapper_studio::prefab_assets::Asset{display_name:Some("Outpost".into()),..asset});
            let catalog=std::sync::Arc::new(mapper_studio::prefab_assets::Catalog{sha256:source.sha256.clone(),installation:source.installation.clone(),entries,sources:source.sources.clone()});
            app.monument_icons.seed_names(catalog.clone());
            let temp=tempfile::tempdir().unwrap();app.settings_path=Some(temp.path().join("settings.json"));
            let icon=temp.path().join("outpost.png");let mut image=egui::ColorImage::new([32,32],vec![Color32::TRANSPARENT;1024]);
            for y in 8..24 {for x in 6..26 {image.pixels[y*32+x]=Color32::from_rgb(85,180,230);}}
            mapper_studio::image_export::write_png(&icon,&image,||false).unwrap();
            let folder=mapper_studio::monument_icons::directory(app.settings_path.as_ref().unwrap()).unwrap();
            let hash=mapper_studio::monument_icons::import(&folder,&icon).unwrap();app.preferences.monument_icons.insert(path.clone(),hash.clone());app.save_preferences();
            let mut textures=snapshot::Textures::default();
            let mut painted=false;let mut painted_frame=ctx.run_ui(egui::RawInput::default(),|ui|{painted=app.monument_icons.paint(ui.painter(),egui::pos2(100.,100.),Some(&path),&app.preferences,app.settings_path.as_deref(),Default::default());});assert!(painted);textures.update(&painted_frame.textures_delta);painted_frame.textures_delta.clear();
            let recipe=app.project.clone();let mut last=None;
            for _ in 0..2 {let mut out=ctx.run_ui(egui::RawInput{screen_rect:Some(Rect::from_min_size(Pos2::ZERO,egui::vec2(width as f32,height as f32))),focused:true,..Default::default()},|ui|{
                app.monument_icons.settings(ui,&app.project,Some(&catalog),&mut app.preferences,app.settings_path.as_deref());
            });textures.update(&out.textures_delta);out.textures_delta.clear();last=Some(out);}
            let out=last.unwrap();let labels:Vec<_>=out.shapes.iter().filter_map(|s|if let egui::Shape::Text(t)=&s.shape{Some(t.galley.job.text.clone())}else{None}).collect();
            for label in ["Outpost", "Search monuments", "Restore", "Restore all"] {
                assert!(labels.iter().any(|text| text == label), "Missing icon library control {label}");
            }
            let restore=out.shapes.iter().find_map(|s|if let egui::Shape::Text(t)=&s.shape {if t.galley.job.text=="Restore" {Some(t.pos+t.galley.rect.center().to_vec2())}else{None}}else{None}).unwrap();
            assert_eq!(app.monument_icons.name(Some(&path),"compound"),"Outpost");assert_eq!(app.monument_icons.name(Some("unknown"),"Unknown"),"Unknown");
            assert_eq!(app.project,recipe);assert!(!app.history.can_undo());
            if let Some(dir)=std::env::var_os("MAPPER_UI_SNAPSHOT_DIR") {let dir=PathBuf::from(dir);std::fs::create_dir_all(&dir).unwrap();let image=textures.raster(&ctx,out,[width,height]);mapper_studio::image_export::write_png(&dir.join(format!("monument-icons-{}-{name}.png",if dark{"dark"}else{"light"})),&image,||false).unwrap();}
            for pressed in [true,false] {let mut click_frame=ctx.run_ui(egui::RawInput{screen_rect:Some(Rect::from_min_size(Pos2::ZERO,egui::vec2(width as f32,height as f32))),focused:true,
                events:vec![egui::Event::PointerMoved(restore),button(restore,pressed)],..Default::default()},|ui|{
                if app.monument_icons.settings(ui,&app.project,Some(&catalog),&mut app.preferences,app.settings_path.as_deref()){app.save_preferences();}
            });textures.update(&click_frame.textures_delta);click_frame.textures_delta.clear();}
            let mut fallback_frame=ctx.run_ui(egui::RawInput::default(),|ui|{painted=app.monument_icons.paint(ui.painter(),egui::pos2(100.,100.),Some(&path),&app.preferences,app.settings_path.as_deref(),Default::default());});textures.update(&fallback_frame.textures_delta);fallback_frame.textures_delta.clear();assert!(painted,"Restore must show the bundled Outpost artwork");
            let icon_texture=|frame:&egui::FullOutput|frame.shapes.iter().find_map(|s|match &s.shape {egui::Shape::Mesh(mesh)=>Some(mesh.texture_id),_=>None}).unwrap();
            assert_ne!(icon_texture(&fallback_frame),icon_texture(&painted_frame));
            assert!(mapper_studio::preferences::Preferences::load(app.settings_path.as_ref().unwrap()).unwrap().monument_icons.is_empty());
            assert_eq!(app.project,recipe);
        }}
    }
    #[test]
    fn no_build_overlay_and_rust_grid_are_view_only_and_do_not_capture_blank_clicks() {
        mod snapshot {include!("../examples/support/egui_snapshot.rs");}
        for dark in [false,true] {for (width,height,name) in [(900,700,"wide"),(360,520,"narrow")] {
            let ctx=egui::Context::default();ctx.set_visuals(if dark{egui::Visuals::dark()}else{egui::Visuals::light()});
            let mut app=fixture(&ctx);app.project.size=1000;app.project.targets.truncate(1);app.project.targets[0].x=0.;app.project.targets[0].z=0.;
            let path=app.project.targets[0].prefab_path.clone().unwrap();app.prefabs.seed_test_stamp(&ctx,&mut app.project,&path);
            app.preview.as_mut().unwrap().key=app.project.terrain_key();let recipe=app.project.clone();deliver_layout(&mut app,recipe.clone(),false);
            let layout=&mut app.layouts[app.active_layout.unwrap()];layout.solution.outcomes[0].position=Some(papyrus_terrain::placement::Position{x:0.,y:20.,z:0.,yaw:37.,clearance:40.});layout.solution.required_missing=0;
            let source=app.prefabs.catalog(&app.project).unwrap();let mut entries=source.entries.clone();
            let report:mapper_studio::no_build::Report=serde_json::from_value(serde_json::json!({"version":1,"space":"prefab-root-local-unity","layerIndex":29,"nodesVisited":3,"volumes":[
                {"objectId":1,"active":true,"geometry":{"shape":"box","center":[-85,0,0],"halfAxes":[[8,0,0],[0,10,0],[0,0,8]],"flatOrthogonal":true}},
                {"objectId":2,"active":true,"geometry":{"shape":"sphere","center":[-100,0,0],"radius":65}},
                {"objectId":3,"active":true,"geometry":{"shape":"capsule","start":[-50,-200,120],"end":[50,-200,120],"radius":75}}],"issues":[],"complete":true})).unwrap();
            entries.insert(path.clone(),mapper_studio::prefab_assets::Asset{priority:None,bounds:None,yaw:None,modifier_size:None,modifier_offset:None,texture_guids:Default::default(),splat_mask:None,modifier:None,modifier_absent:false,water_geometry:None,no_build_geometry:Some(report),display_name:None});
            app.restrictions.seed(std::sync::Arc::new(mapper_studio::prefab_assets::Catalog{sha256:source.sha256.clone(),installation:source.installation.clone(),entries,sources:source.sources.clone()}));
            app.grid=true;app.show_no_build=true;app.selected=Some(0);app.prefabs.show_stamp=false;
            let mut textures=snapshot::Textures::default();let mut last=None;
            for _ in 0..2 {let mut out=ctx.run_ui(egui::RawInput{screen_rect:Some(Rect::from_min_size(Pos2::ZERO,egui::vec2(width as f32,height as f32))),focused:true,..Default::default()},|ui|app.map(ui));textures.update(&out.textures_delta);out.textures_delta.clear();last=Some(out);}
            let out = last.unwrap();
            let fills = out.shapes.iter().filter(|shape| matches!(&shape.shape,
                egui::Shape::Path(path) if path.fill == Color32::from_rgba_unmultiplied(240, 124, 100, 22))).count();
            assert_eq!(fills, 1, "Only the large surface perimeter should be drawn");
            assert!(out.shapes.iter().any(|s|matches!(&s.shape,egui::Shape::Text(t) if t.galley.job.text=="A0")));
            assert_eq!(app.project,recipe);assert!(!app.history.can_undo());
            if let Some(dir)=std::env::var_os("MAPPER_UI_SNAPSHOT_DIR") {let dir=PathBuf::from(dir);std::fs::create_dir_all(&dir).unwrap();let image=textures.raster(&ctx,out,[width,height]);mapper_studio::image_export::write_png(&dir.join(format!("zones-grid-{}-{name}.png",if dark{"dark"}else{"light"})),&image,||false).unwrap();}
            if width==900 {let blank=egui::pos2(200.,620.);frame(&ctx,&mut app,vec![egui::Event::PointerMoved(blank),button(blank,true)]);frame(&ctx,&mut app,vec![button(blank,false)]);assert!(app.selected.is_none());assert_eq!(app.project,recipe);}
        }}
    }
    #[test]
    fn placed_splats_retain_only_unchanged_previews_during_regeneration() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        let paths: Vec<_> = app
            .project
            .targets
            .iter()
            .take(2)
            .map(|t| t.prefab_path.clone().unwrap())
            .collect();
        let ids: Vec<_> = paths
            .iter()
            .map(|path| app.prefabs.seed_test_stamp(&ctx, &mut app.project, path))
            .collect();
        app.preview.as_mut().unwrap().key = app.project.terrain_key();
        let recipe = app.project.clone();
        deliver_layout(&mut app, recipe, false);
        let meshes = |output: egui::FullOutput| {
            output
                .shapes
                .into_iter()
                .filter_map(|s| match s.shape {
                    egui::Shape::Mesh(m) if ids.contains(&m.texture_id) => Some(m.texture_id),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let drawn = meshes(frame(&ctx, &mut app, vec![]));
        assert!(
            ids.iter().all(|id| drawn.contains(id)),
            "Both placed splats must be visible"
        );
        app.show_monuments = false;
        assert!(meshes(frame(&ctx, &mut app, vec![])).is_empty());
        app.show_monuments = true;
        app.project.targets[0].x += 10.;
        let drawn = meshes(frame(&ctx, &mut app, vec![]));
        assert!(
            !drawn.contains(&ids[0]),
            "Moved stamp's previous position must disappear"
        );
        assert!(
            drawn.contains(&ids[1]),
            "Unchanged stamp remains as a preview"
        );
        assert!(app.current_layout().is_none());
        assert!(app.composition.view(&app.project).is_none());
        app.project.seeds[0].value += 1;
        assert!(
            meshes(frame(&ctx, &mut app, vec![])).is_empty(),
            "Different terrain cannot retain old footprints"
        );
    }
    #[test]
    fn placement_lifecycle_discards_cancelled_stale_and_duplicate_results() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        let recipe = app.project.clone();
        deliver_layout(&mut app, recipe.clone(), true);
        assert!(app.layouts.is_empty());
        app.project.targets[0].x += 50.;
        deliver_layout(&mut app, recipe.clone(), false);
        assert!(app.layouts.is_empty());
        app.project = recipe.clone();
        deliver_layout(&mut app, recipe.clone(), false);
        assert_eq!(app.layouts.len(), 1);
        deliver_layout(&mut app, recipe, false);
        assert_eq!(app.layouts.len(), 1);
        assert!(app.current_layout().is_some());
    }
    #[test]
    fn road_seed_and_settings_reuse_accepted_monuments_but_keep_composition_stale() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        let path = app.project.targets[0].prefab_path.clone().unwrap();
        app.prefabs.seed_test_stamp(&ctx, &mut app.project, &path);
        app.project.preview_engine = mapper_studio::native_preview::VERSION.into();
        app.project.monument_assets = Some(mapper_studio::AssetIdentity {
            catalog_sha256: "b".repeat(64),
            bindings_sha256: "c".repeat(64),
        });
        app.project.roads = Some(mapper_studio::infrastructure::Recipe::new("d".repeat(64)));
        app.project.monument_rules=Some(mapper_studio::monument_source::Recipe {engine:mapper_studio::monument_source::VERSION.into(),source_sha256:"f".repeat(64),coastal_profile:None,tier_search:None,earthwork:None,coastal_normals:None,coastal_navigation:None,coastal_search:None,canonical_yaw:None,land_alignment:None,surface_modifiers:None,stage_rules:None,dungeon_source:None,offshore_source:None});
        app.project.validate().unwrap();
        app.preview.as_mut().unwrap().key = app.project.terrain_key();
        let recipe = app.project.clone();
        let terrain_key = recipe.terrain_key();
        deliver_layout(&mut app, recipe, false);
        app.project.seeds[3].inherited = false;
        app.project.seeds[3].value = 123;
        app.project.roads.as_mut().unwrap().maximum_cut_fill = 4.;
        app.project.rails=Some(mapper_studio::rail_generation::Recipe {engine:mapper_studio::rail_generation::VERSION.into(),source_sha256:"a".repeat(64),mesh_sha256:"b".repeat(64),settings:mapper_studio::rail_generation::initial_settings(123)});
        app.project.validate().unwrap();
        assert!(app.current_layout().is_some());
        app.project.rails.as_mut().unwrap().settings["ring"]["seed"]=42.into();
        assert!(app.current_layout().is_some());
        assert!(app.composition.view(&app.project).is_none());
        assert_eq!(terrain_key,app.project.terrain_key());
        app.project.rails=None;
        app.project.decoration = Some(mapper_studio::decor_source::Recipe::new("e".repeat(64)));
        app.project.seeds[5].inherited = false;
        app.project.seeds[5].value = 456;
        app.project.validate().unwrap();
        assert!(app.current_layout().is_some());
        assert!(app.composition.view(&app.project).is_none());
        assert_eq!(terrain_key, app.project.terrain_key());
        app.project.shuffle_monuments();
        assert!(app.current_layout().is_none());
    }
    #[test]
    fn undo_seed_restores_matching_layout_but_changed_targets_hide_results() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        let first = app.project.clone();
        deliver_layout(&mut app, first.clone(), false);
        app.project.shuffle_monuments();
        let second = app.project.clone();
        app.history.record(first.clone(), &second);
        deliver_layout(&mut app, second.clone(), false);
        assert_eq!(app.layouts.len(), 2);
        app.history.undo(&mut app.project);
        app.refresh_layout_status();
        assert!(app.status.contains(&first.seeds[4].value.to_string()));
        assert_eq!(app.current_layout().unwrap().recipe, first);
        app.history.redo(&mut app.project);
        assert_eq!(app.current_layout().unwrap().recipe, second);
        app.project.targets[0].x += 10.;
        assert!(app.current_layout().is_none());
    }
    #[test]
    fn intermediate_images_wait_for_the_viewport_and_reuse_its_texture() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        let key = app.project.terrain_key();
        let original = std::sync::Arc::clone(&app.preview.as_ref().unwrap().heights);
        let (_sender, receiver) = std::sync::mpsc::channel();
        app.show_tab(Tab::Generated(1));
        app.apply_tab_request();
        assert!(!app.working_map_visible());
        let progress = |label, color| preview::Progress {
            key: key.clone(),
            label,
            image: Some(egui::ColorImage::filled([129, 129], color)),
        };
        let updates = Arc::new(std::sync::Mutex::new(None));
        app.task = Some(Task {
            ordered_terrain: false,
            key: key.clone(),
            receiver,
            cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            progress: updates.clone(),
        });
        *updates.lock().unwrap() = Some(progress("Coast", Color32::BLUE));
        app.poll(&ctx);
        *updates.lock().unwrap() = Some(progress("Refinement", Color32::RED));
        app.poll(&ctx);
        assert!(app.stage_texture.is_none());
        assert_eq!(app.generation_progress, Some("Refinement"));
        assert_eq!(app.stage_image.as_ref().unwrap().pixels[0], Color32::RED);
        app.show_tab(Tab::Map);
        app.apply_tab_request();
        assert!(app.working_map_visible());
        let _ = frame(&ctx, &mut app, Vec::new());
        let texture = app.stage_texture.as_ref().unwrap().id();
        assert!(app.stage_image.is_none());
        *updates.lock().unwrap() = Some(progress("Materials", Color32::WHITE));
        app.poll(&ctx);
        let _ = frame(&ctx, &mut app, Vec::new());
        assert_eq!(app.stage_texture.as_ref().unwrap().id(), texture);
        assert_eq!(app.preview.as_ref().unwrap().heights, original);
        app.start_placement(&ctx);
        assert!(app.placement_task.is_none());
        *updates.lock().unwrap() = Some(progress("Finalizing", Color32::BLACK));
        app.poll(&ctx);
        assert!(app.stage_image.is_some());
        app.project.seeds[1].value = 222;
        app.poll(&ctx);
        assert!(app.stage_image.is_none());
        assert!(app.stage_texture.is_none());
        assert!(app.generation_progress.is_none());
        assert_eq!(app.preview.as_ref().unwrap().heights, original);
    }
}
