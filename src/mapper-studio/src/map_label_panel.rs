use super::*;
use mapper_studio::map_labels::{Label, Mode, Prepared};
use std::{collections::HashMap, sync::Arc};

#[derive(Clone, Copy, Default)]
pub(super) struct Text<'a> {
    pub name: &'a str,
    pub source: Option<&'a Arc<Prepared>>,
}

enum Layout {
    Client(Arc<Label>),
    Fallback(Arc<egui::Galley>),
}
impl Layout {
    fn fallback_ink(galley: &egui::Galley) -> Rect {
        if galley.mesh_bounds.is_positive() {
            galley.mesh_bounds
        } else {
            Rect::from_min_size(Pos2::ZERO, galley.size())
        }
    }

    fn bounds(&self) -> Rect {
        match self {
            Self::Client(label) => label.bounds,
            Self::Fallback(galley) => {
                let ink = Self::fallback_ink(galley);
                ink.translate(-ink.center().to_vec2())
            }
        }
    }
}
#[derive(Default)]
pub(super) struct Panel {
    current: Option<Arc<Prepared>>,
    textures: HashMap<String, egui::TextureHandle>,
    layouts: HashMap<(String, String), Layout>,
    pub error: String,
}
impl Panel {
    pub fn accept(&mut self, source: Option<Arc<Prepared>>, error: String, ctx: &egui::Context) {
        if let Some(source) = &source {
            self.texture(source, ctx);
        }
        self.current = source;
        self.error = error;
    }
    pub fn source(
        &self,
        expected: Option<&mapper_studio::installation::Installation>,
    ) -> Option<Arc<Prepared>> {
        self.current
            .as_ref()
            .filter(|source| Some(&source.installation) == expected)
            .cloned()
    }
    fn texture(&mut self, source: &Prepared, ctx: &egui::Context) -> egui::TextureId {
        self.textures
            .entry(source.image_sha256.clone())
            .or_insert_with(|| {
                ctx.load_texture(
                    format!("map-label-font-{}", source.image_sha256),
                    egui::ImageData::Color(source.image.clone()),
                    egui::TextureOptions::LINEAR,
                )
            })
            .id()
    }
    fn key(source: Option<&Arc<Prepared>>, name: &str) -> (String, String) {
        (
            source.map_or_else(String::new, |source| source.sha256.clone()),
            name.into(),
        )
    }
    pub fn prepare(&mut self, ctx: &egui::Context, source: Option<&Arc<Prepared>>, name: &str) {
        let key = Self::key(source, name);
        if self.layouts.contains_key(&key) {
            return;
        }
        if self.layouts.len() >= 2048 {
            self.layouts.clear();
        }
        let layout = source
            .and_then(|source| source.style.centered_label(name, &source.alpha))
            .map(|label| Layout::Client(Arc::new(label)))
            .unwrap_or_else(|| {
                Layout::Fallback(ctx.fonts_mut(|fonts| {
                    fonts.layout_no_wrap(
                        name.split_whitespace().collect::<Vec<_>>().join(" "),
                        FontId::proportional(13.),
                        Color32::from_gray(43),
                    )
                }))
            });
        self.layouts.insert(key, layout);
    }
    pub fn hit(
        &self,
        point: Pos2,
        center: Pos2,
        source: Option<&Arc<Prepared>>,
        name: &str,
    ) -> bool {
        self.layouts
            .get(&Self::key(source, name))
            .is_some_and(|layout| {
                layout
                    .bounds()
                    .translate(center.to_vec2())
                    .expand(2.)
                    .contains(point)
            })
    }
    pub fn paint(
        &mut self,
        painter: &egui::Painter,
        center: Pos2,
        source: Option<&Arc<Prepared>>,
        name: &str,
    ) {
        self.prepare(painter.ctx(), source, name);
        let texture = source.map(|source| self.texture(source, painter.ctx()));
        match &self.layouts[&Self::key(source, name)] {
            Layout::Client(label) => {
                let source = source.unwrap();
                for quad in &label.quads {
                    painter.image(
                        texture.unwrap(),
                        quad.rect.translate(center.to_vec2()),
                        quad.uv,
                        source.style.color,
                    );
                }
            }
            Layout::Fallback(galley) => {
                painter.galley_with_override_text_color(
                    center - Layout::fallback_ink(galley).center().to_vec2(),
                    galley.clone(),
                    source.map_or(Color32::from_gray(43), |source| source.style.color),
                );
            }
        }
    }
    pub fn bounds(
        &mut self,
        ctx: &egui::Context,
        source: Option<&Arc<Prepared>>,
        name: &str,
    ) -> Rect {
        self.prepare(ctx, source, name);
        self.layouts[&Self::key(source, name)].bounds()
    }
}

pub(super) fn controls(ui: &mut egui::Ui, mode: &mut Mode) -> bool {
    let before = *mode;
    ui.menu_button("Markers", |ui| {
        for choice in Mode::ALL {
            if ui.selectable_value(mode, choice, choice.label()).clicked() {
                ui.close();
            }
        }
    })
    .response
    .on_hover_text("Monument markers: none, icons, or the Rust client's map text.");
    before != *mode
}

#[cfg(test)]
#[path = "../examples/support/egui_snapshot.rs"]
mod snapshot;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "Requires local client typography capture and reproduction project"]
    fn client_map_label_snapshot() {
        let folder = std::path::PathBuf::from(
            std::env::var_os("MAPPER_TEST_MAP_LABELS").expect("MAPPER_TEST_MAP_LABELS"),
        );
        let project = Project::load(&std::path::PathBuf::from(
            std::env::var_os("MAPPER_TEST_MAP_PROJECT").expect("MAPPER_TEST_MAP_PROJECT"),
        ))
        .unwrap();
        let metadata =
            serde_json::from_slice(&std::fs::read(folder.join("labels.json")).unwrap()).unwrap();
        let style = mapper_studio::map_labels::Style::decode(&metadata).unwrap();
        assert_eq!(style.font_name, "PermanentMarker SDF");
        let image = style
            .image(&std::fs::read(folder.join("map-font.atlas")).unwrap())
            .unwrap();
        let source = Arc::new(Prepared {
            metadata:Arc::new(metadata),
            installation: project.installation.unwrap(),
            sha256: "test-source".into(),
            style,
            image: Arc::new(image),
            image_sha256: "test-pixels".into(),
            alpha: std::fs::read(folder.join("map-font.atlas")).unwrap().into(),
            grid: std::env::var_os("MAPPER_TEST_PAPER_MAP_GRID").map(|path| {
                Arc::new(mapper_studio::map_grid::Appearance::read_capture(std::path::Path::new(&path)).unwrap())
            }),
        });
        let ctx = egui::Context::default();
        let mut panel = Panel::default();
        let mut textures = snapshot::Textures::default();
        let size = [900, 480];
        let mut mode = Mode::Text;
        let mut last = None;
        for _ in 0..2 {
            let mut frame = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(900., 480.))),
                    ..Default::default()
                },
                |ui| {
                    controls(ui, &mut mode);
                    let (rect, _) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
                    ui.painter()
                        .rect_filled(rect, 0., Color32::from_rgb(110, 122, 83));
                    if let Some(grid) = source.grid.as_deref() {
                        for (index, color) in [Color32::from_rgb(24, 59, 78), Color32::from_rgb(180, 171, 141)].into_iter().enumerate() {
                            let map = Rect::from_min_size(rect.min + egui::vec2(14. + 440. * index as f32, 0.),
                                egui::Vec2::splat(430.));
                            ui.painter().rect_filled(map, 0., color);
                            super::super::restriction_panel::grid(ui.painter(), map, rect, 1800, Some(grid));
                        }
                    }
                    for (index, name) in [
                        "OUTPOST",
                        "WATER TREATMENT PLANT",
                        "OXUM'S GAS STATION",
                        "ABANDONED SUPERMARKET",
                        "LARGE OIL RIG",
                        "TRAIN YARD",
                    ]
                    .iter()
                    .enumerate()
                    {
                        let center = rect.min
                            + egui::vec2(
                                155. + (index % 3) as f32 * 285.,
                                100. + (index / 3) as f32 * 180.,
                            );
                        panel.paint(ui.painter(), center, Some(&source), name);
                        assert!(panel.hit(
                            center,
                            center,
                            Some(&source),
                            name
                        ));
                        assert!(panel.bounds(ui.ctx(), Some(&source), name).center().distance(Pos2::ZERO) < 0.001);
                    }
                },
            );
            textures.update(&frame.textures_delta);
            frame.textures_delta.clear();
            last = Some(frame);
        }
        let image = textures.raster(&ctx, last.unwrap(), size);
        if let Some(path) = std::env::var_os("MAPPER_UI_SNAPSHOT_DIR") {
            std::fs::create_dir_all(&path).unwrap();
            let path = std::path::PathBuf::from(path).join("client-map-labels.png");
            let file = std::fs::File::create(path).unwrap();
            let mut encoder = png::Encoder::new(file, size[0] as u32, size[1] as u32);
            encoder.set_color(png::ColorType::Rgba);
            let mut writer = encoder.write_header().unwrap();
            writer
                .write_image_data(
                    &image
                        .pixels
                        .iter()
                        .flat_map(|p| p.to_array())
                        .collect::<Vec<_>>(),
                )
                .unwrap();
        }
    }
}
