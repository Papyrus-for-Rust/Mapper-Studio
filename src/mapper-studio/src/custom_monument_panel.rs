use eframe::egui;
use mapper_studio::rustedit_prefab::{self, LibraryEntry, PREVIEW_SIDE, Source, SplatPreview};
use mapper_studio::{
    Project,
    custom_monuments::{self, Placement},
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
};

#[derive(Default)]
pub(super) struct Panel {
    open: bool,
    task: Option<Task>,
    draft: Option<Draft>,
    notice: Option<Result<(), String>>,
    library: Vec<LibraryEntry>,
    position: [f32; 3],
    yaw: f32,
}

struct Draft {
    name: String,
    source: Arc<Source>,
    footprint: egui::TextureHandle,
    splat: Option<egui::TextureHandle>,
    show_splat: bool,
    saved: Option<LibraryEntry>,
    placement_issue: Option<String>,
}

struct Preview {
    name: String,
    source: Arc<Source>,
    footprint: egui::ColorImage,
    splat: Option<egui::ColorImage>,
    saved: Option<LibraryEntry>,
}

enum Finished {
    Imported(Preview),
    Library(Vec<LibraryEntry>),
    Saved(LibraryEntry),
}

enum Selected {
    File(PathBuf),
    Library(LibraryEntry),
}

struct Task {
    receiver: Receiver<Result<Finished, String>>,
    stop: Arc<AtomicBool>,
    activity: &'static str,
}

impl Drop for Task {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Panel {
    pub(super) fn open(&mut self, ctx: &egui::Context) {
        if !self.open {
            self.open = true;
            self.start(ctx, "Reading library…", |stop| {
                rustedit_prefab::library_entries(|| stop.load(Ordering::Relaxed))
                    .map(Finished::Library)
            });
        }
    }

    fn start(
        &mut self,
        ctx: &egui::Context,
        activity: &'static str,
        work: impl FnOnce(&AtomicBool) -> Result<Finished, String> + Send + 'static,
    ) {
        self.notice = None;
        let ctx = ctx.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (sender, receiver) = mpsc::channel();
        match std::thread::Builder::new()
            .name("mapper-custom-monument".into())
            .spawn(move || {
                let _ = sender.send(work(&worker_stop));
                ctx.request_repaint();
            }) {
            Ok(_) => {
                self.task = Some(Task {
                    receiver,
                    stop,
                    activity,
                })
            }
            Err(error) => self.notice = Some(Err(error.to_string())),
        }
    }

    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    pub(super) fn show(&mut self, ctx: &egui::Context, recipe: &mut Project, editable: bool) {
        if !self.open {
            return;
        }
        if let Some(task) = &self.task {
            let result = match task.receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err("Monument import worker stopped.".into()))
                }
            };
            if let Some(result) = result {
                self.task = None;
                match result {
                    Ok(Finished::Imported(preview)) => {
                        let texture = |name, image| {
                            ctx.load_texture(name, image, egui::TextureOptions::NEAREST)
                        };
                        let show_splat = preview.splat.is_some();
                        let placement_issue =
                            custom_monuments::check_objects_only(&preview.source).err();
                        self.draft = Some(Draft {
                            name: preview.name,
                            source: preview.source,
                            footprint: texture("custom-monument-footprint", preview.footprint),
                            splat: preview
                                .splat
                                .map(|image| texture("custom-monument-splat", image)),
                            show_splat,
                            saved: preview.saved,
                            placement_issue,
                        });
                    }
                    Ok(Finished::Library(entries)) => self.library = entries,
                    Ok(Finished::Saved(entry)) => {
                        if let Some(draft) = &mut self.draft {
                            draft.saved = Some(entry.clone());
                        }
                        if let Err(index) = self.library.binary_search(&entry) {
                            self.library.insert(index, entry);
                        }
                        self.notice = Some(Ok(()));
                    }
                    Err(error) => self.notice = Some(Err(error)),
                }
            }
        }
        let mut open = self.open;
        egui::Window::new("Custom monuments")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .vscroll(true)
            .default_width(420.)
            .default_height((ctx.content_rect().height() - 70.).max(240.))
            .frame(super::settings_dialog::workbench_dialog_frame(ctx, 14.))
            .show(ctx, |ui| {
                let mut selected = None;
                ui.add_enabled_ui(self.task.is_none(), |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("Choose prefab…")
                            .on_hover_text("RustEdit 1 .prefab. Keep the terrain files next to it.")
                            .clicked()
                            && let Some(path) = rfd::FileDialog::new().add_filter("RustEdit 1 prefab", &["prefab"]).pick_file()
                        {
                            selected = Some(Selected::File(path));
                        }
                        if !self.library.is_empty() {
                            egui::ComboBox::from_id_salt("custom-monument-library")
                                .selected_text("Library")
                                .width(88.)
                                .show_ui(ui, |ui| {
                                    for (index, entry) in self.library.iter().enumerate() {
                                        let duplicate = (index > 0 && self.library[index - 1].name == entry.name)
                                            || self.library.get(index + 1).is_some_and(|next| next.name == entry.name);
                                        let label = if duplicate {
                                            format!("{} · {}", entry.name, &entry.version.as_str()[..8])
                                        } else {
                                            entry.name.clone()
                                        };
                                        if ui.selectable_label(false, label).clicked() {
                                            selected = Some(Selected::Library(entry.clone()));
                                        }
                                    }
                                });
                        }
                    });
                });
                if let Some(selection) = selected {
                    self.start(ctx, "Preparing stamp preview…", move |stop| {
                        let cancelled = || stop.load(Ordering::Relaxed);
                        let preview = match selection {
                            Selected::File(path) => Preview::read(path, cancelled)?,
                            Selected::Library(entry) => {
                                let (source, splats) = entry.read_for_preview(cancelled)?;
                                let mut preview = Preview::new(entry.name.clone(), source, splats);
                                preview.saved = Some(entry);
                                preview
                            }
                        };
                        Ok(Finished::Imported(preview))
                    });
                }

                let mut save = None;
                if let Some(draft) = &mut self.draft {
                    ui.add_space(4.);
                    ui.horizontal(|ui| {
                        let label = ui.label("Name");
                        ui.add_enabled(self.task.is_none(),
                            egui::TextEdit::singleline(&mut draft.name).desired_width(208.)
                        ).labelled_by(label.id);
                    });
                    let parts = draft.source.prefab.prefabs.len();
                    let images = draft.source.terrain.len();
                    let mut summary = if parts == 1 { "1 prefab".to_owned() } else { format!("{parts} prefabs") };
                    if images > 0 { summary.push_str(&format!(" · {images} terrain {}", if images == 1 { "image" } else { "images" })); }
                    ui.weak(summary);
                    if let Some(entry) = draft.saved.as_ref().filter(|entry| entry.name == draft.name.trim()) {
                        if let Some(message) = &draft.placement_issue {
                            ui.weak("Preview only").on_hover_text(message);
                        } else {
                            let blocked = if !editable { Some("Finish the current edit first.") }
                                else if !recipe.performance.monuments { Some("Enable Monuments in Recipe.") }
                                else if recipe.installation.is_none() || !mapper_studio::native_preview::is_native(&recipe.preview_engine)
                                    { Some("Choose a Rust installation first.") }
                                else { None };
                            ui.add_enabled_ui(blocked.is_none() && self.task.is_none(), |ui| {
                                placement_fields(ui, recipe.size, &mut self.position, &mut self.yaw);
                                if ui.add(egui::Button::new(egui::RichText::new("Add to recipe").color(egui::Color32::WHITE))
                                    .fill(super::workbench_style::PRIMARY_ACTION)).clicked() {
                                    let placement = Placement {
                                        version: entry.version.as_str().to_owned(),
                                        position: self.position,
                                        yaw: self.yaw,
                                        name: entry.name.clone(),
                                        presence: mapper_studio::Presence::Required,
                                        baked: false,
                                    };
                                    self.notice = custom_monuments::add_placement(recipe, placement).err().map(Err);
                                }
                            }).response.on_disabled_hover_text(blocked.unwrap_or("Wait for the current operation."));
                        }
                    } else if ui.add_enabled(self.task.is_none() && !draft.name.trim().is_empty(),
                        egui::Button::new(egui::RichText::new("Save to library").color(egui::Color32::WHITE))
                            .fill(super::workbench_style::PRIMARY_ACTION)).clicked() {
                            save = Some((draft.source.clone(), draft.name.clone()));
                    }
                } else if self.task.is_none() {
                    ui.weak("Select a RustEdit 1 .prefab to preview its footprint and splat mask.");
                }
                if let Some((source, name)) = save {
                    self.start(ctx, "Saving monument…", move |stop| {
                        source.save_to_library(&name, || stop.load(Ordering::Relaxed)).map(Finished::Saved)
                    });
                }
                if !recipe.custom_monuments.is_empty() {
                    ui.collapsing(format!("In recipe ({})", recipe.custom_monuments.len()), |ui| {
                        egui::ScrollArea::vertical().max_height(110.).show(ui, |ui| {
                            let mut remove = None;
                            for (index, placement) in recipe.custom_monuments.iter().enumerate() {
                                let name = self.library.iter().find(|entry| entry.version.as_str() == placement.version)
                                    .map_or_else(|| placement.version.get(..8).unwrap_or(&placement.version), |entry| entry.name.as_str());
                                ui.push_id(index, |ui| {
                                    ui.horizontal(|ui| {
                                        let title = format!("{name} · {:.0}, {:.0}, {:.0}", placement.position[0], placement.position[1], placement.position[2]);
                                        if ui.add_enabled(editable, egui::Button::new("Remove")).clicked() { remove = Some(index); }
                                        ui.add(egui::Label::new(&title).truncate()).on_hover_text(format!("{title} m · {:.1}°", placement.yaw));
                                    });
                                });
                            }
                            if let Some(index) = remove {
                                recipe.custom_monuments.remove(index);
                            }
                        });
                    });
                }
                if let Some(draft) = &mut self.draft {
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut draft.show_splat, false, "Prefab footprint")
                            .on_hover_text("Prefab origins viewed from above. The cross marks the source origin.");
                        if draft.splat.is_some() {
                            ui.selectable_value(&mut draft.show_splat, true, "Splat mask")
                                .on_hover_text("Material weights shown in preview colors.");
                        }
                    });
                    let texture = if draft.show_splat {
                        draft.splat.as_ref().unwrap_or(&draft.footprint)
                    } else {
                        &draft.footprint
                    };
                    let size = texture.size_vec2();
                    let controls_height = if draft.saved.is_some() { 365. } else { 265. };
                    let preview_height = (ctx.content_rect().height() - controls_height).clamp(90., 320.);
                    let scale = ((ui.available_width() - 16.).clamp(1., 420.) / size.x).min(preview_height / size.y);
                    egui::Frame::new().fill(ui.visuals().extreme_bg_color).corner_radius(8)
                        .inner_margin(egui::Margin::same(8)).show(ui, |ui| {
                            ui.vertical_centered(|ui| {
                                ui.add(egui::Image::new((texture.id(), size * scale)));
                            });
                        });
                }
                if let Some(task) = &self.task {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(task.activity);
                    });
                }
                match &self.notice {
                    Some(Ok(())) => { ui.weak("Saved to the custom monument library."); }
                    Some(Err(error)) => { ui.colored_label(ui.visuals().error_fg_color, error); }
                    None => {}
                }
            });
        if !open {
            *self = Self::default();
        }
    }
}

impl Preview {
    fn read(path: PathBuf, cancelled: impl Fn() -> bool) -> Result<Self, String> {
        if !path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("prefab"))
        {
            return Err(
                "Choose a RustEdit 1 .prefab. RustEdit 2 .asset files are not supported yet."
                    .into(),
            );
        }
        let (source, splats) = Source::read_for_preview(&path, &cancelled)?;
        let name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        Ok(Self::new(name, source, splats))
    }

    fn new(name: String, source: Source, splats: SplatPreview) -> Self {
        let footprint = footprint(&source);
        Self {
            name,
            source: Arc::new(source),
            footprint,
            splat: splat_preview(splats),
            saved: None,
        }
    }
}

fn placement_fields(ui: &mut egui::Ui, size: u32, position: &mut [f32; 3], yaw: &mut f32) {
    let edge = size as f32 * 0.5;
    let [x, y, z] = position;
    egui::Grid::new("custom-monument-transform").show(ui, |ui| {
        let x_label = ui.label("X");
        ui.add(egui::DragValue::new(x).suffix(" m").range(-edge..=edge))
            .labelled_by(x_label.id);
        let z_label = ui.label("Z");
        ui.add(egui::DragValue::new(z).suffix(" m").range(-edge..=edge))
            .labelled_by(z_label.id);
        ui.end_row();
        let y_label = ui
            .label("Y")
            .on_hover_text("Elevation of the source origin.");
        ui.add(
            egui::DragValue::new(y)
                .suffix(" m")
                .range(-10000. ..=10000.),
        )
        .labelled_by(y_label.id);
        let yaw_label = ui.label("Yaw");
        ui.add(
            egui::DragValue::new(yaw)
                .suffix("°")
                .range(0. ..=360.0_f32.next_down()),
        )
        .labelled_by(yaw_label.id);
        ui.end_row();
    });
}

fn footprint(source: &Source) -> egui::ColorImage {
    let mut image = egui::ColorImage::filled([PREVIEW_SIDE; 2], egui::Color32::from_gray(30));
    let points = source
        .prefab
        .prefabs
        .iter()
        .map(|prefab| [prefab.position.x, prefab.position.z]);
    let (mut min, mut max) = ([0.0_f32; 2], [0.0_f32; 2]);
    for [x, z] in points.clone() {
        min = [min[0].min(x), min[1].min(z)];
        max = [max[0].max(x), max[1].max(z)];
    }
    let span = (max[0] - min[0]).max(max[1] - min[1]).max(1.);
    let mid = [(max[0] + min[0]) * 0.5, (max[1] + min[1]) * 0.5];
    let scale = (PREVIEW_SIDE - 24) as f32 / span;
    let pixel = |[x, z]: [f32; 2]| {
        let center = PREVIEW_SIDE as f32 / 2.;
        [
            (center + (x - mid[0]) * scale) as usize,
            (center - (z - mid[1]) * scale) as usize,
        ]
    };
    for [x, y] in points.map(pixel) {
        for row in y - 1..=y + 1 {
            image.pixels[row * PREVIEW_SIDE + x - 1..=row * PREVIEW_SIDE + x + 1]
                .fill(egui::Color32::from_rgb(151, 190, 222));
        }
    }
    let [x, y] = pixel([0.; 2]);
    for offset in -4..=4 {
        image.pixels[y * PREVIEW_SIDE + x.wrapping_add_signed(offset)] =
            egui::Color32::from_rgb(255, 213, 138);
        image.pixels[y.wrapping_add_signed(offset) * PREVIEW_SIDE + x] =
            egui::Color32::from_rgb(255, 213, 138);
    }
    image
}

fn splat_preview(layers: SplatPreview) -> Option<egui::ColorImage> {
    if layers.iter().all(Option::is_none) {
        return None;
    }
    let mut colors = vec![[0_u32; 4]; PREVIEW_SIDE * PREVIEW_SIDE];
    let palettes = [
        [
            [155, 111, 85],
            [220, 229, 235],
            [211, 181, 123],
            [139, 141, 150],
        ],
        [
            [112, 157, 83],
            [65, 111, 74],
            [154, 156, 166],
            [182, 164, 148],
        ],
    ];
    for (layer, palette) in layers.into_iter().zip(palettes) {
        let Some(layer) = layer else { continue };
        for (pixel, weights) in colors.iter_mut().zip(layer) {
            for (weight, color) in weights.into_iter().zip(palette) {
                let weight = u32::from(weight);
                for channel in 0..3 {
                    pixel[channel] += color[channel] * weight;
                }
                pixel[3] += weight;
            }
        }
    }
    let pixels = colors
        .into_iter()
        .map(|[r, g, b, weight]| {
            let denominator = weight.max(255);
            let background = 30 * 255_u32.saturating_sub(weight);
            egui::Color32::from_rgb(
                ((r + background) / denominator) as u8,
                ((g + background) / denominator) as u8,
                ((b + background) / denominator) as u8,
            )
        })
        .collect();
    Some(egui::ColorImage::new([PREVIEW_SIDE; 2], pixels))
}

#[cfg(test)]
mod tests {
    use super::*;
    mod snapshot {
        include!("../examples/support/egui_snapshot.rs");
    }

    #[test]
    fn custom_monument_window_renders_a_splat_stamp_and_library_action() {
        let temp = tempfile::tempdir().unwrap();
        let fixture = std::env::var_os("MAPPER_CUSTOM_MONUMENT_FIXTURE").map_or_else(
            || {
                let path = temp.path().join("Monument.prefab");
                std::fs::write(&path, [1, 0, 0, 0, 0, 8, 10, 2, 8, 80, 26, 2, 16, 42]).unwrap();
                let file =
                    std::fs::File::create(temp.path().join("Monument.prefab.splat0")).unwrap();
                let mut encoder = png::Encoder::new(file, 2, 2);
                encoder.set_color(png::ColorType::Rgba);
                encoder
                    .write_header()
                    .unwrap()
                    .write_image_data(&[255, 0, 0, 0, 0, 255, 0, 0, 0, 0, 255, 0, 0, 0, 0, 255])
                    .unwrap();
                path
            },
            PathBuf::from,
        );
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            let ctx = egui::Context::default();
            ctx.set_theme(theme);
            crate::workbench_style::install(&ctx);
            ctx.all_styles_mut(|style| style.animation_time = 0.);
            let preview = Preview::read(fixture.clone(), || false).unwrap();
            assert!(
                preview
                    .splat
                    .as_ref()
                    .unwrap()
                    .pixels
                    .iter()
                    .any(|pixel| *pixel != egui::Color32::from_gray(30))
            );
            let (sender, receiver) = mpsc::channel();
            sender.send(Ok(Finished::Imported(preview))).unwrap();
            let mut panel = Panel {
                open: true,
                task: Some(Task {
                    receiver,
                    stop: Arc::new(AtomicBool::new(false)),
                    activity: "Preparing stamp preview…",
                }),
                ..Default::default()
            };
            let size = [640, 520];
            let mut recipe = Project::default();
            let mut textures = snapshot::Textures::default();
            let mut render = || {
                let mut frame = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(size[0] as f32, size[1] as f32),
                        )),
                        ..Default::default()
                    },
                    |_| panel.show(&ctx, &mut recipe, true),
                );
                textures.update(&frame.textures_delta);
                frame.textures_delta.clear();
                frame
            };
            render();
            let frame = render();
            for label in ["Custom monuments", "Splat mask", "Save to library"] {
                assert!(
                    frame.shapes.iter().any(|shape| match &shape.shape {
                        egui::Shape::Text(text) =>
                            text.galley.job.text == label
                                && shape
                                    .clip_rect
                                    .contains_rect(text.galley.rect.translate(text.pos.to_vec2())),
                        _ => false,
                    }),
                    "{label} must fit"
                );
            }
            if let Some(path) = std::env::var_os("MAPPER_CUSTOM_STAMP_SNAPSHOTS") {
                let image = textures.raster(&ctx, frame, size);
                let path = PathBuf::from(path).join(format!("custom-monument-{theme:?}.png"));
                mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
            }
        }
    }

    #[test]
    fn custom_monument_window_adds_a_saved_pose_to_the_recipe() {
        use mapper_studio::installation::{Installation, InstallationKind, SourceFile};
        let ctx = egui::Context::default();
        crate::workbench_style::install(&ctx);
        ctx.all_styles_mut(|style| style.animation_time = 0.);
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("Assembly.prefab");
        std::fs::write(&input, [1, 0, 0, 0, 0, 6, 10, 0, 26, 2, 16, 42]).unwrap();
        let version = rustedit_prefab::Version::parse(&"a".repeat(64)).unwrap();
        let mut preview = Preview::read(input, || false).unwrap();
        let entry = LibraryEntry {
            name: preview.name.clone(),
            version: version.clone(),
        };
        preview.saved = Some(entry.clone());
        let (sender, receiver) = mpsc::channel();
        sender.send(Ok(Finished::Imported(preview))).unwrap();
        let position = [123., 45., -67.];
        let yaw = 91.;
        let mut panel = Panel {
            open: true,
            position,
            yaw,
            library: vec![entry],
            task: Some(Task {
                receiver,
                stop: Arc::new(AtomicBool::new(false)),
                activity: "Opening",
            }),
            ..Default::default()
        };
        let mut project = Project {
            preview_engine: mapper_studio::native_preview::VERSION.into(),
            installation: Some(Installation {
                kind: InstallationKind::Client,
                native: SourceFile {
                    relative_path: "RustNative.dll".into(),
                    bytes: 1,
                    sha256: "b".repeat(64),
                },
                scene_manifest: None,
            }),
            ..Project::default()
        };
        let size = [640, 640];
        let mut textures = snapshot::Textures::default();
        let mut render = |panel: &mut Panel, project: &mut Project, events| {
            let mut frame = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(640., 640.),
                    )),
                    events,
                    focused: true,
                    ..Default::default()
                },
                |_| panel.show(&ctx, project, true),
            );
            textures.update(&frame.textures_delta);
            frame.textures_delta.clear();
            frame
        };
        render(&mut panel, &mut project, vec![]);
        let mut click = |panel: &mut Panel, project: &mut Project, label: &str| {
            let frame = render(panel, project, vec![]);
            let point = frame
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.job.text == label => {
                        Some(text.pos + text.galley.size() * 0.5)
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("Missing {label} action"));
            for pressed in [true, false] {
                render(
                    panel,
                    project,
                    vec![
                        egui::Event::PointerMoved(point),
                        egui::Event::PointerButton {
                            pos: point,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                );
            }
            render(panel, project, vec![])
        };
        let frame = click(&mut panel, &mut project, "Add to recipe");
        assert_eq!(
            project.custom_monuments,
            [Placement {
                version: version.as_str().into(),
                position,
                yaw,
                name: "Assembly".into(),
                presence: mapper_studio::Presence::Required,
                baked: false,
            }]
        );
        click(&mut panel, &mut project, "In recipe (1)");
        click(&mut panel, &mut project, "Remove");
        assert!(project.custom_monuments.is_empty());
        if let Some(path) = std::env::var_os("MAPPER_CUSTOM_STAMP_SNAPSHOTS") {
            let image = textures.raster(&ctx, frame, size);
            mapper_studio::image_export::write_png(
                &PathBuf::from(path).join("custom-monument-placement.png"),
                &image,
                || false,
            )
            .unwrap();
        }
    }
}
