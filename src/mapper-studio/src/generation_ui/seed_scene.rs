//! Lazy, per-map 3D terrain for generated seed tabs.
use super::*;

struct Terrain {
    resolution: usize,
    heights: Vec<i16>,
    image: Arc<egui::ColorImage>,
    texture: Option<egui::TextureHandle>,
}

enum State {
    Loading(std::sync::mpsc::Receiver<Result<Option<Terrain>, String>>),
    Ready(Terrain),
    Failed(String),
}

#[derive(Default)]
pub(crate) struct Scene {
    pub(crate) camera: terrain_3d::Camera,
    snapshot: Option<Arc<mapper_studio::image_snapshot::Snapshot>>,
    state: Option<State>,
    cancel: Option<Arc<AtomicBool>>,
}

impl Drop for Scene {
    fn drop(&mut self) {
        self.cancel_loading();
    }
}

impl Scene {
    fn cancel_loading(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    pub(super) fn draw(
        &mut self, ui: &mut egui::Ui, size: u32,
        snapshot: Arc<mapper_studio::image_snapshot::Snapshot>,
        overview: Arc<tempfile::TempPath>,
        models: &[terrain_3d::ModelInstance<'_>],
    ) {
        if self.snapshot.as_ref().is_none_or(|previous| !Arc::ptr_eq(previous, &snapshot)) {
            self.cancel_loading();
            self.snapshot = Some(snapshot.clone());
            let cancel = Arc::new(AtomicBool::new(false));
            let signal = cancel.clone();
            let worker = preview::start_worker("mapper-seed-3d", ui.ctx().clone(), move |_| {
                let cancelled = || signal.load(Ordering::Relaxed);
                let Some((resolution, heights)) = snapshot.coarse_height(size, &cancelled)? else {
                    return Ok(None);
                };
                let Some(image) = persistence::read_overview(&overview, &cancelled)? else {
                    return Ok(None);
                };
                let [width, height] = image.size;
                let image = if width.max(height) > 1024 {
                    let scale = width.max(height).div_ceil(1024);
                    let size = [width.div_ceil(scale), height.div_ceil(scale)];
                    let pixels = image.pixels.chunks_exact(width).step_by(scale)
                        .flat_map(|row| row.iter().step_by(scale).copied()).collect();
                    egui::ColorImage::new(size, pixels)
                } else {
                    image
                };
                Ok(Some(Terrain {
                    resolution, heights, image: Arc::new(image), texture: None,
                }))
            });
            self.cancel = Some(cancel);
            self.state = Some(State::Loading(worker));
        }
        if let Some(State::Loading(worker)) = self.state.as_mut() {
            match worker.try_recv() {
                Ok(Ok(Some(terrain))) => self.state = Some(State::Ready(terrain)),
                Ok(Err(error)) => self.state = Some(State::Failed(error)),
                Ok(Ok(None)) | Err(std::sync::mpsc::TryRecvError::Disconnected) =>
                    self.state = Some(State::Failed("Terrain viewer stopped.".into())),
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        let (rect, response) = ui.allocate_exact_size(ui.available_size().max(Vec2::splat(1.)), Sense::click_and_drag());
        match self.state.as_mut() {
            Some(State::Ready(terrain)) => {
                let texture = terrain.texture.get_or_insert_with(|| ui.ctx().load_texture(
                    "seed-3d-overview", terrain.image.as_ref().clone(), egui::TextureOptions::LINEAR,
                ));
                self.camera.draw_live(ui, rect, &response, texture.id(), &terrain.image,
                    &terrain.heights, terrain.resolution, size, models, None, None);
                ui.painter().text(rect.left_bottom() + egui::vec2(10., -10.),
                    Align2::LEFT_BOTTOM, "Hold right mouse to fly · WASD · Q / E · Shift",
                    FontId::proportional(12.), Color32::WHITE);
            }
            Some(State::Failed(error)) => {
                ui.painter().text(rect.center(), Align2::CENTER_CENTER,
                    "3D terrain could not load", FontId::proportional(14.), ui.visuals().warn_fg_color);
                response.on_hover_text(error.as_str());
            }
            _ => {
                ui.painter().text(rect.center(), Align2::CENTER_CENTER,
                    "Loading 3D terrain…", FontId::proportional(14.), ui.visuals().text_color());
            }
        }
    }
}

pub(super) fn view_toggle(ui: &mut egui::Ui, enabled: &mut bool, available: bool) {
    egui::Frame::new()
        .fill(Color32::from_black_alpha(77))
        .stroke(egui::Stroke::new(1., Color32::from_white_alpha(40)))
        .corner_radius(8)
        .inner_margin(egui::Margin::same(2))
        .show(ui, |ui| ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.;
            for (value, label) in [(false, "2D"), (true, "3D")] {
                let selected = *enabled == value;
                let mut button = egui::Button::selectable(selected, label)
                    .frame_when_inactive(selected).corner_radius(6).min_size(egui::vec2(30., 20.));
                if selected {
                    button = button.fill(workbench_style::MAP_VIEW_SELECTED);
                }
                if ui.add_enabled(!value || available, button)
                    .on_disabled_hover_text("3D opens when this map has retained terrain.")
                    .clicked()
                {
                    *enabled = value;
                }
            }
        }));
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    #[test]
    fn seed_3d_reads_a_bounded_height_grid_from_the_accepted_snapshot() {
        let size = 1000;
        let (height, surface) = papyrus_terrain::native_layers::dimensions(size).unwrap();
        let length = (24 + height * height * 2 + surface * surface * 17) as u64;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        {
            let mut gzip = flate2::write::GzEncoder::new(file.as_file_mut(), flate2::Compression::fast());
            gzip.write_all(b"MIMG0001").unwrap();
            gzip.write_all(&length.to_le_bytes()).unwrap();
            gzip.write_all(&(24 + length).to_le_bytes()).unwrap();
            gzip.write_all(b"PNF1").unwrap();
            for value in [size, 1, 2, height as u32, surface as u32] {
                gzip.write_all(&value.to_le_bytes()).unwrap();
            }
            for z in 0..height {
                let row: Vec<_> = (0..height).flat_map(|x| ((z * 2 + x) as i16).to_le_bytes()).collect();
                gzip.write_all(&row).unwrap();
            }
            gzip.finish().unwrap();
        }
        let snapshot = mapper_studio::image_snapshot::Snapshot::from_retained_file(
            std::sync::Arc::new(file.into_temp_path()), size,
        ).unwrap();
        let (side, samples) = snapshot.coarse_height(size, &|| false).unwrap().unwrap();
        assert_eq!(side, 257);
        assert_eq!(samples.len(), side * side);
        assert_eq!([samples[0], samples[side - 1], samples[samples.len() - 1]],
            [0, (height - 1) as i16, ((height - 1) * 3) as i16]);
        assert!(snapshot.coarse_height(size, &|| true).unwrap().is_none());
    }
}
