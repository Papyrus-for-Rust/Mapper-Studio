use super::project_io::TaskSignal;
use eframe::egui;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{self, Receiver, TryRecvError},
    },
};
pub(super) enum Source {
    Unavailable,
    Native {
        palette: Option<Arc<mapper_studio::terrain_palette::Palette>>,
        layers: Arc<papyrus_terrain::native_layers::Layers>,
        ocean: Option<Arc<papyrus_terrain::ocean::Ocean>>,
    },
    Composed {
        maps: Arc<papyrus_terrain::monument_composition::Composed>,
        palette: Option<Arc<mapper_studio::terrain_palette::Palette>>,
    },
    Independent {
        land: Arc<papyrus_terrain::landscape::Landscape>,
        size: u32,
        seed: u32,
    },
    Legacy(Arc<egui::ColorImage>),
    Saved {snapshot:Arc<mapper_studio::image_snapshot::Snapshot>,palette:Option<Arc<mapper_studio::terrain_palette::Palette>>},
}
impl Source {
    pub fn render(
        &self,
        resolution: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<egui::ColorImage>, String> {
        match self {
            Self::Unavailable=>Err("Terrain render data is unavailable for this map".into()),
            Self::Native { layers, ocean, palette } => {
                mapper_studio::map_image::render_native_owned(layers, None, ocean.as_deref(), resolution, palette.as_deref(), cancelled).map(|r|r.map(|r|r.image))
            }
            Self::Composed {maps,palette} => mapper_studio::map_image::render_composed_owned(
                maps,
                resolution,
                palette.as_deref(),
                cancelled,
            ).map(|r|r.map(|r|r.image)),
            Self::Independent { land, size, seed } => mapper_studio::terrain_view::render_cancellable(
                land, *size, *seed, resolution, cancelled,
            ),
            Self::Legacy(image) => Ok((!cancelled()).then(|| image.as_ref().clone())),
            Self::Saved {snapshot,palette}=>snapshot.render(resolution,palette.as_deref(),cancelled),
        }
    }
}
struct Task {
    receiver: Receiver<Result<bool, String>>,
    signal: Arc<TaskSignal>,
    path: PathBuf,
}
impl Drop for Task {
    fn drop(&mut self) {
        self.signal.cancel();
    }
}

pub(super) fn dialog_directory(saved: Option<&Path>, project: Option<&Path>) -> Option<PathBuf> {
    saved
        .into_iter()
        .chain(project.and_then(Path::parent))
        .filter(|path| path.is_dir())
        .find_map(|path| path.canonicalize().ok())
}

pub(super) struct ExportedFile {
    path: PathBuf,
    folder_error: String,
}

impl ExportedFile {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            folder_error: String::new(),
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("Open folder").clicked() {
                self.folder_error = open_export_folder(&self.path).err().unwrap_or_default();
            }
            let name = self
                .path
                .file_name()
                .unwrap_or(self.path.as_os_str())
                .to_string_lossy();
            ui.add(egui::Label::new(name).truncate())
                .on_hover_text(self.path.to_string_lossy());
        });
        if !self.folder_error.is_empty() {
            ui.colored_label(ui.visuals().error_fg_color, &self.folder_error);
            crate::diagnostic_ui::copy_error(ui, &self.folder_error, None);
        }
    }
}

fn open_export_folder(path: &Path) -> Result<(), String> {
    let folder = path
        .parent()
        .filter(|folder| folder.is_dir())
        .ok_or("The export folder is unavailable.")?;
    let folder = std::path::absolute(folder).map_err(|_| "The export folder is unavailable.")?;
    let program = if cfg!(windows) {
        std::env::var_os("WINDIR")
            .map(|root| PathBuf::from(root).join("explorer.exe"))
            .unwrap_or_else(|| "explorer.exe".into())
    } else if cfg!(target_os = "macos") {
        "/usr/bin/open".into()
    } else {
        "xdg-open".into()
    };
    let child = std::process::Command::new(program)
        .arg(folder)
        .spawn()
        .map_err(|_| "Could not open the export folder.")?;
    #[cfg(not(windows))]
    std::thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    #[cfg(windows)]
    drop(child);
    Ok(())
}

#[derive(Clone, PartialEq, Eq)]
struct PreviewKey {
    target: Option<u64>,
    options: mapper_studio::image_render::Settings,
    missing_zones: usize,
    waiting_zones: bool,
    has_font: bool,
}

struct PreviewTask {
    key: PreviewKey,
    receiver: Receiver<Result<Option<egui::ColorImage>, String>>,
    signal: Arc<TaskSignal>,
}

impl Drop for PreviewTask {
    fn drop(&mut self) {
        self.signal.cancel();
    }
}

#[derive(Default)]
struct RenderPreview {
    task: Option<PreviewTask>,
    frame: Option<(PreviewKey, Result<egui::TextureHandle, String>)>,
}

impl RenderPreview {
    fn needs_input(&mut self, key: &PreviewKey, ctx: &egui::Context) -> bool {
        if let Some(task) = &self.task {
            if task.key != *key {
                task.signal.cancel();
            }
            let result = match task.receiver.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => Err("Image preview worker stopped.".into()),
            };
            let task = self.task.take().unwrap();
            if task.key == *key && !task.signal.cancelled() {
                let texture = result.and_then(|image| image.ok_or_else(|| "Preview unavailable.".into()))
                    .map(|image| ctx.load_texture("Image export preview", image, egui::TextureOptions::LINEAR));
                self.frame = Some((task.key.clone(), texture));
            }
        }
        self.frame.as_ref().is_none_or(|(presented, _)| presented != key)
    }

    fn start(&mut self, key: PreviewKey, source: Source, scene: super::export_render::Scene, ctx: &egui::Context) {
        const PREVIEW_SIDE: usize = 384;
        if let Source::Legacy(image) = source {
            let scale = (PREVIEW_SIDE as f32 / image.size[0].max(image.size[1]) as f32).min(1.);
            let size = image.size.map(|length| (length as f32 * scale).round().max(1.) as usize);
            let pixels = (0..size[1]).flat_map(|y| {
                let image = &image;
                (0..size[0]).map(move |x| {
                    image.pixels[(y * image.size[1] / size[1]) * image.size[0] + x * image.size[0] / size[0]]
                })
            }).collect();
            let image = egui::ColorImage::new(size, pixels);
            self.frame = Some((key, Ok(ctx.load_texture("Image export preview", image, egui::TextureOptions::LINEAR))));
            return;
        }
        let signal = Arc::new(TaskSignal::default());
        let cancellation = signal.clone();
        let (sender, receiver) = mpsc::channel();
        let repaint = ctx.clone();
        let mut options = key.options.clone();
        options.monument_data = false;
        let worker = std::thread::Builder::new().name("mapper-export-preview".into()).spawn(move || {
            let result = scene.draw(source, PREVIEW_SIDE, options, || cancellation.cancelled())
                .map(|rendered| rendered.map(|(image, _)| image));
            let _ = sender.send(result);
            repaint.request_repaint();
        });
        match worker {
            Ok(_) => self.task = Some(PreviewTask { key, receiver, signal }),
            Err(error) => self.frame = Some((key, Err(error.to_string()))),
        }
    }

    fn show(&self, ui: &mut egui::Ui, key: &PreviewKey, ready: bool, side: f32, unavailable_hint: &str) {
        let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(side), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 7., ui.visuals().extreme_bg_color);
        match self.frame.as_ref().filter(|(presented, _)| presented == key).map(|(_, frame)| frame) {
            Some(Ok(texture)) if ready => {
                let size = texture.size_vec2();
                let fit = size * (side / size.x.max(size.y));
                painter.image(texture.id(), egui::Rect::from_center_size(rect.center(), fit),
                    egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1., 1.)), egui::Color32::WHITE);
            }
            frame => {
                let message = if !ready { "Terrain render data unavailable" }
                    else if frame.is_some() { "Preview unavailable" } else { "Updating preview…" };
                let response = ui.interact(rect, ui.id().with("image-preview-status"), egui::Sense::hover());
                if !ready {
                    response.on_hover_text(unavailable_hint);
                } else if let Some(Err(error)) = frame {
                    response.on_hover_text(error);
                }
                painter.text(rect.center(), egui::Align2::CENTER_CENTER, message,
                    egui::TextStyle::Body.resolve(ui.style()), ui.visuals().weak_text_color());
            }
        }
    }
}

#[derive(Default)]
pub(super) struct ImageExport {
    preview: RenderPreview,
    task: Option<Task>,
    message: String,
    message_is_error: bool,
    exported: Option<ExportedFile>,
}
impl ImageExport {
    pub fn busy(&self) -> bool {
        self.task.is_some()
    }
    #[cfg(test)]
    pub fn start(&mut self, path: PathBuf, source: Source, resolution: usize, ctx: &egui::Context) {
        self.start_render(path,source,resolution,None,Default::default(),ctx);
    }
    pub fn start_render(&mut self, path: PathBuf, source: Source, resolution: usize, scene:Option<super::export_render::Scene>, settings:mapper_studio::image_render::Settings, ctx: &egui::Context) {
        if self.task.is_some() {
            return;
        }
        let (width, height) = match &source {
            Source::Legacy(image) => (image.size[0], image.size[1]),
            _ => (resolution, resolution),
        };
        let destination = path.clone();
        let signal = Arc::new(TaskSignal::default());
        let worker = signal.clone();
        let (sender, receiver) = mpsc::channel();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let result = if let Some(scene)=scene {
                scene.draw(source,resolution,settings,||worker.cancelled())
            }else{source.render(resolution,||worker.cancelled()).map(|r|r.map(|image|(image,None)))}
                .and_then(|image| match image {
                    Some((image,metadata)) => {
                        mapper_studio::image_export::write_png_with_publish_gate(
                            &destination, &image, metadata.as_ref(), || worker.cancelled(), || {
                                let publishing = worker.begin_publish();
                                repaint.request_repaint();
                                publishing
                            },
                        )
                    }
                    None => Ok(false),
                });
            let result=if worker.cancelled() && result.is_err() {Ok(false)}else{result};
            let _ = sender.send(result);
            repaint.request_repaint();
        });
        self.task = Some(Task {
            receiver,
            signal,
            path,
        });
        self.exported = None;
        self.message = format!("Exporting {width} × {height} map image…");
        self.message_is_error = false;
        ctx.request_repaint();
    }
    pub fn poll(&mut self) -> Option<PathBuf> {
        let Some(task) = &self.task else {
            return None;
        };
        let result = match task.receiver.try_recv() {
            Ok(r) => Some(r),
            Err(TryRecvError::Empty) => None,
            Err(_) => Some(Err("Image export worker stopped".into())),
        };
        if let Some(result) = result {
            let task = self.task.take().unwrap();
            let completed = matches!(result, Ok(true)).then(|| task.path.clone());
            self.exported = completed.clone().map(ExportedFile::new);
            self.message_is_error = result.is_err();
            self.message = match result {
                Ok(true) => "Map image exported".into(),
                Ok(false) => "Image export cancelled".into(),
                Err(e) => format!("Image export failed: {e}"),
            };
            return completed;
        }
        None
    }
    pub fn show(&mut self, ui: &mut egui::Ui) {
        let cancellable = self.task.as_ref().filter(|task| task.signal.can_cancel());
        let message = match (&self.task, cancellable) {
            (Some(task), None) if task.signal.cancelled() => "Cancelling image export…",
            (Some(_), None) => "Finishing image export…",
            _ => &self.message,
        };
        if !message.is_empty() {
            ui.add(egui::Label::new(egui::RichText::new(message).small()).truncate())
                .on_hover_text(message);
        }
        if self.message_is_error {
            crate::diagnostic_ui::copy_error(ui, &self.message, None);
        }
        if let Some(exported) = &mut self.exported {
            exported.show(ui);
        }
        if let Some(task) = cancellable
            && ui.button("Cancel image export").clicked()
        {
            task.signal.cancel();
        }
    }
}

impl super::Studio {
    pub(super) fn poll_exports(&mut self) {
        let map = self.map_archive.poll_export();
        let image = self.image_export.poll();
        if let Some(path) = image.or(map) {
            self.preferences.export_folder = path.parent().map(|parent| parent.to_owned());
            self.save_preferences();
        }
    }

    pub(super) fn export_controls(&mut self, ui: &mut egui::Ui) {
        let map_notice = !self.map_archive.export_message().is_empty();
        let image_notice = !self.image_export.message.is_empty();
        if !map_notice && !image_notice {
            return;
        }
        let busy = self.map_archive.exporting() || self.image_export.busy();
        let label = if busy { "Exporting…" } else { "Export status" };
        egui::Popup::menu(&ui.button(label))
            .align(egui::emath::RectAlign::BOTTOM_END)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_min_width(230.);
                ui.set_max_width(320.);
                self.map_archive.show_export(ui);
                if map_notice && image_notice {
                    ui.separator();
                }
                self.image_export.show(ui);
                if !busy && ui.button("Dismiss").clicked() {
                    self.map_archive.dismiss_export_message();
                    self.image_export.message.clear();
                    self.image_export.exported = None;
                    ui.close();
                }
            });
    }

    pub(super) fn image_export_recipe(&self) -> Option<&mapper_studio::Project> {
        if let Some(id) = self.image_export_target {
            self.generated_image_recipe(id)
        } else {
            Some(&self.project)
        }
    }
    pub(super) fn image_export_ready(&self) -> bool {
        if let Some(id) = self.image_export_target {
            self.generated_image_recipe(id).is_some()
                && (!self.preferences.image_render.terrain || self.generated_image_ready(id))
        } else {
            self.exportable_map().is_some()
        }
    }
    pub(super) fn image_export_legacy_size(&self) -> Option<[usize; 2]> {
        if self.image_export_target.is_none()
            && self
                .preview
                .as_ref()
                .is_some_and(|p| p.native_layers.is_none() && p.landscape.is_none())
        {
            self.exportable_map().map(|image| image.size)
        } else {
            None
        }
    }
    fn image_export_warnings(&self) -> (usize, bool) {
        if let Some(id) = self.image_export_target {
            return self.generated_image_warnings(id);
        }
        let mut monuments = self
            .current_layout()
            .map(|layout| super::map_monuments::initial(&self.project, &layout.solution))
            .unwrap_or_default();
        if let Some(view) = self.composition.view(&self.project) {
            monuments.extend(view.generated_monuments.iter().cloned());
        }
        (
            self.restrictions
                .incomplete_count(&self.project, &monuments),
            self.monument_icons
                .labels
                .source(self.project.installation.as_ref())
                .is_some(),
        )
    }
    pub(super) fn image_export_input(&self) -> Option<(Source, super::export_render::Scene)> {
        if let Some(id) = self.image_export_target {
            return self.generated_image_input(id);
        }
        let source = self.export_source()?;
        let mut monuments = self
            .current_layout()
            .map(|layout| super::map_monuments::initial(&self.project, &layout.solution))
            .unwrap_or_default();
        if let Some(view) = self.composition.view(&self.project) {
            monuments.extend(view.generated_monuments.iter().cloned());
        }
        for monument in &mut monuments {
            monument.name = self
                .monument_icons
                .name(monument.prefab.as_deref(), &monument.name);
        }
        Some((
            source,
            super::export_render::Scene {
                project: self.project.clone(),
                monuments,
                restrictions: self.restrictions.for_export(),
                icons: self.monument_icons.for_export(),
                font: self
                    .monument_icons
                    .labels
                    .source(self.project.installation.as_ref()),
                preferences: self.preferences.clone(),
                settings_path: self.settings_path.clone(),
                background: self.viewport_background(),
            },
        ))
    }
    pub(super) fn export_options(&mut self, ctx: &egui::Context) {
        if !self.export_options_open {
            self.image_export.preview = RenderPreview::default();
            return;
        }
        let mut save = false;
        let before = self.preferences.clone();
        let legacy = self.image_export_legacy_size();
        let ready = self.image_export_ready();
        let busy = self.image_export.busy();
        let applied = self.image_export_target.is_some_and(|id| self.map_has_pending_edits(id));
        let (partial, font) = self.image_export_warnings();
        let zones_pending = partial > 0
            && self.image_export_recipe().is_some_and(|recipe| self.restrictions.pending_for(recipe));
        let recipe = self.image_export_recipe().map(|recipe| (recipe.size, recipe.master_seed));
        let width = (ctx.content_rect().width() - 52.).clamp(280., 782.);
        let compact = width < 580.;
        let title = if applied { "Render applied map image" } else { "Render map image" };
        let modal = super::settings_dialog::workbench_modal(ctx, egui::Id::new("map-image-export-options"), 18.)
            .show(ctx, |ui| {
                ui.set_width(width);
                ui.heading(title);
                if let Some((size, seed)) = recipe {
                    ui.weak(format!("{size} m · Seed {seed}"));
                }
                ui.separator();
                let max_body_height = (ctx.content_rect().height() - 164.).max(120.);
                egui::ScrollArea::vertical().max_height(max_body_height)
                    .auto_shrink([false, true]).show(ui, |ui| {
                        let controls = |ui: &mut egui::Ui, preferences: &mut mapper_studio::preferences::Preferences| {
                            ui.strong("Image settings");
                            ui.add_enabled_ui(!busy, |ui| render_controls(ui, preferences, legacy));
                            if applied {
                                ui.small("Apply your changes on the map tab to include them in the export.");
                            }
                            if preferences.image_render.no_build && zones_pending {
                                ui.small("Loading no-build zones…");
                            } else if preferences.image_render.no_build && partial > 0 {
                                ui.small("Some no-build zones are unavailable")
                                    .on_hover_text(format!("{partial} monuments have incomplete source volumes. Known volumes will be included."));
                            }
                            if preferences.image_render.markers == mapper_studio::image_render::Markers::Text && !font {
                                ui.small("Client font unavailable").on_hover_text("This export uses Mapper’s fallback font.");
                            }
                        };
                        let mut options_width = width;
                        if compact {
                            controls(ui, &mut self.preferences);
                        } else {
                            options_width = (width * 0.43).max(240.);
                        }
                        let preview_key = |this: &Self| PreviewKey {
                            target: this.image_export_target,
                            options: this.preferences.image_render.clone(),
                            missing_zones: partial,
                            waiting_zones: zones_pending,
                            has_font: font,
                        };
                        if compact {
                            let key = preview_key(self);
                            self.render_export_preview(ui, &key, ready, width.min(220.));
                        } else {
                            ui.horizontal_top(|ui| {
                                let control_rect = egui::vec2(options_width, 0.);
                                ui.allocate_ui_with_layout(control_rect, egui::Layout::top_down(egui::Align::Min),
                                    |ui| controls(ui, &mut self.preferences));
                                ui.add_space(10.);
                                let key = preview_key(self);
                                let side = ui.available_width().min(398.).max(1.);
                                self.render_export_preview(ui, &key, ready, side);
                            });
                        }
                    });
                ui.separator();
                self.image_export.show(ui);
                let waiting = self.preferences.image_render.no_build && zones_pending;
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    save = ui.add_enabled(!busy && ready && !waiting, egui::Button::new(
                        egui::RichText::new("Save PNG…").color(egui::Color32::WHITE),
                    ).fill(super::workbench_style::PRIMARY_ACTION)).clicked();
                    ui.button("Close").clicked()
                }).inner
            });
        if before != self.preferences {
            self.save_preferences();
        }
        if modal.inner || modal.should_close() {
            self.export_options_open = false;
            self.image_export.preview = RenderPreview::default();
        }
        if save {
            self.export_map_image(ctx);
        }
    }

    fn render_export_preview(&mut self, ui: &mut egui::Ui, key: &PreviewKey, ready: bool, side: f32) {
        ui.vertical(|ui| {
            ui.strong("Preview");
            if ready && !self.image_export.busy() && self.image_export.preview.needs_input(key, ui.ctx())
                && let Some((source, scene)) = self.image_export_input()
            {
                self.image_export.preview.start(key.clone(), source, scene, ui.ctx());
            }
            let unavailable_hint = self.image_export_target.map_or(
                "Finish working-map generation and composition before exporting.",
                |id| self.generated_image_issue(id),
            );
            self.image_export.preview.show(ui, key, ready, side, unavailable_hint);
        });
    }
}
fn render_controls(
    ui: &mut egui::Ui,
    preferences: &mut mapper_studio::preferences::Preferences,
    legacy: Option<[usize; 2]>,
) {
    egui::Grid::new("image-render-fields")
        .num_columns(2)
        .spacing([18., 8.])
        .show(ui, |ui| {
            ui.label("Resolution");
            if let Some([width, height]) = legacy {
                ui.label(format!("{width} × {height} px"));
            } else {
                egui::ComboBox::from_id_salt("export-image-resolution")
                    .selected_text(preferences.export_resolution.label())
                    .show_ui(ui, |ui| {
                        for resolution in mapper_studio::preferences::ImageResolution::ALL {
                            ui.selectable_value(
                                &mut preferences.export_resolution,
                                resolution,
                                resolution.label(),
                            );
                        }
                    })
                    .response
                    .on_hover_text("Higher resolutions use more image memory.");
            }
            ui.end_row();
            ui.label("Monuments");
            egui::ComboBox::from_id_salt("export-monument-markers")
                .selected_text(preferences.image_render.markers.label())
                .show_ui(ui, |ui| {
                    for mode in mapper_studio::image_render::Markers::ALL {
                        ui.selectable_value(
                            &mut preferences.image_render.markers,
                            mode,
                            mode.label(),
                        );
                    }
                });
            ui.end_row();
        });
    ui.add_space(8.);
    ui.checkbox(&mut preferences.image_render.terrain, "Terrain")
        .on_hover_text("Turn off to render overlays on a transparent image.");
    ui.checkbox(&mut preferences.image_render.no_build, "No-build zones")
        .on_hover_text("Draw known restriction volumes from the map's Rust installation.");
    ui.checkbox(&mut preferences.image_render.grid, "Map grid");
    ui.add_space(4.);
    ui.collapsing("Image data",|ui|{
        ui.checkbox(&mut preferences.image_render.monument_data,"Embed monument positions").on_hover_text("Store monument IDs, names, positions, rotations, seeds and pixel coordinates inside the PNG for future tools. Contains no local folder paths.");
    });
}
#[cfg(test)]
mod snapshot { include!("../examples/support/egui_snapshot.rs"); }
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn render_options_are_compact_and_terrain_is_a_presentation_preference() {
        let ctx=egui::Context::default();super::super::workbench_style::install(&ctx);
        ctx.set_theme(egui::Theme::Dark);
        ctx.all_styles_mut(|style|style.animation_time=0.);
        let mut app=super::super::tests::fixture(&ctx);app.export_options_open=true;
        app.preview.as_mut().unwrap().landscape=Some(Arc::new(papyrus_terrain::landscape::Landscape {resolution:33,heights:vec![20.;33*33],biomes:vec![[0,255,0,0];33*33]}));
        app.preferences.image_render.markers=mapper_studio::image_render::Markers::Centers;
        let recipe=app.project.clone();let mut textures=snapshot::Textures::default();
        let render=|app:&mut super::super::Studio,events|ctx.run_ui(egui::RawInput {
            screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1024.,700.))),events,focused:true,..Default::default()
        },|ui|{ui.painter().rect_filled(ui.max_rect(),0.,ui.visuals().extreme_bg_color);app.export_options(&ctx);});
        let mut first=render(&mut app,Vec::new());textures.update(&first.textures_delta);first.textures_delta.clear();
        let mut frame=render(&mut app,Vec::new());textures.update(&frame.textures_delta);frame.textures_delta.clear();
        assert!(frame.shapes.iter().any(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == "Save PNG…" =>
                shape.clip_rect.contains_rect(text.galley.rect.translate(text.pos.to_vec2())),
            _ => false,
        }), "The export action must remain visible below the preview");
        let point=frame.shapes.iter().find_map(|s|match &s.shape {
            egui::Shape::Text(t) if t.galley.job.text=="Terrain"=>Some(t.pos+t.galley.rect.center().to_vec2()),_=>None,
        }).unwrap();
        let button=|pressed|egui::Event::PointerButton {pos:point,button:egui::PointerButton::Primary,pressed,modifiers:Default::default()};
        let mut down=render(&mut app,vec![egui::Event::PointerMoved(point),button(true)]);textures.update(&down.textures_delta);down.textures_delta.clear();
        let mut up=render(&mut app,vec![button(false)]);textures.update(&up.textures_delta);up.textures_delta.clear();
        assert!(!app.preferences.image_render.terrain);assert_eq!(app.project,recipe);
        assert_eq!(app.preferences.monument_markers,mapper_studio::map_labels::Mode::Icons);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !matches!(&app.image_export.preview.frame, Some((key, Ok(_))) if !key.options.terrain) {
            assert!(std::time::Instant::now() < deadline, "No preview for the updated terrain setting");
            std::thread::sleep(std::time::Duration::from_millis(10));
            up = render(&mut app, Vec::new());
            textures.update(&up.textures_delta);
            up.textures_delta.clear();
        }
        if let Some(folder)=std::env::var_os("MAPPER_UI_SNAPSHOT_DIR") {
            let folder=std::path::PathBuf::from(folder);std::fs::create_dir_all(&folder).unwrap();
            let image=textures.raster(&ctx,up,[1024,700]);
            mapper_studio::image_export::write_png(&folder.join("render-options.png"),&image,||false).unwrap();
        }
    }
    #[test]
    fn render_preferences_roundtrip_independently_of_the_viewport() {
        let prefs=mapper_studio::preferences::Preferences {
            image_render:mapper_studio::image_render::Settings{terrain:false,markers:mapper_studio::image_render::Markers::Centers,no_build:true,grid:true,monument_data:false},
            ..Default::default()
        };
        let encoded=serde_json::to_vec(&prefs).unwrap();
        let restored:mapper_studio::preferences::Preferences=serde_json::from_slice(&encoded).unwrap();
        assert_eq!(restored.image_render,prefs.image_render);
        assert_eq!(restored.monument_markers,mapper_studio::map_labels::Mode::Icons);
    }
    #[test]
    fn background_export_remembers_only_success_and_preserves_output_on_cancel() {
        let ctx = egui::Context::default();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.png");
        let mut export = ImageExport::default();
        export.start(
            path.clone(),
            Source::Legacy(Arc::new(egui::ColorImage::filled([2, 2], egui::Color32::BLUE))),
            2048,
            &ctx,
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let completed = loop {
            if let Some(path) = export.poll() {
                break path;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(completed, path);
        assert_eq!(export.exported.as_ref().unwrap().path, path);
        let original = std::fs::read(&path).unwrap();
        let land = papyrus_terrain::landscape::Landscape {
            resolution: 33,
            heights: vec![20.; 33 * 33],
            biomes: vec![[0, 255, 0, 0]; 33 * 33],
        };
        export.start(
            path.clone(),
            Source::Independent {
                land: Arc::new(land),
                size: 2000,
                seed: 1,
            },
            4096,
            &ctx,
        );
        export
            .task
            .as_ref()
            .unwrap()
            .signal
            .cancel();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while export.busy() {
            assert!(export.poll().is_none());
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(export.message.contains("cancelled"));
        assert!(export.exported.is_none());
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
    #[test]
    fn export_dialog_prefers_the_shared_folder_and_falls_back_to_the_project() {
        let saved = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let file = project.path().join("workspace.mapper");
        assert_eq!(
            dialog_directory(Some(saved.path()), Some(&file)),
            Some(saved.path().canonicalize().unwrap())
        );
        assert_eq!(
            dialog_directory(Some(&saved.path().join("missing")), Some(&file)),
            Some(project.path().canonicalize().unwrap())
        );
    }
    #[test]
    fn export_rerenders_generated_data_at_selected_resolution_and_cancels_before_encoding() {
        let land = papyrus_terrain::landscape::Landscape {
            resolution: 33,
            heights: vec![20.; 33 * 33],
            biomes: vec![[0, 255, 0, 0]; 33 * 33],
        };
        let source = Source::Independent {
            land: Arc::new(land),
            size: 2000,
            seed: 123,
        };
        let image = source.render(1024, || false).unwrap().unwrap();
        assert_eq!(image.size, [1024, 1024]);
        assert!(source.render(4096, || true).unwrap().is_none());
        // Legacy rendering retains its source pixels rather than presenting upscaling as detail.
        let old = egui::ColorImage::filled([7, 7], egui::Color32::GREEN);
        let source = Source::Legacy(Arc::new(old.clone()));
        assert_eq!(source.render(4096, || false).unwrap(), Some(old));
    }
}
