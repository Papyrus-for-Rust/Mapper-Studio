//! Theme-tinted coverage masks rendered from the supplied tool SVGs.
use eframe::egui;

const SIDE: usize = 48;

#[derive(Clone, Copy, Debug, Hash)]
pub(super) enum Icon {
    Brush,
    Eraser,
}

impl Icon {
    pub fn image(self, ui: &egui::Ui) -> egui::Image<'static> {
        let id = egui::Id::new(("toolbar-icon", self));
        let ctx = ui.ctx();
        let cached = ctx.data(|data| data.get_temp::<egui::TextureHandle>(id));
        let texture = cached.unwrap_or_else(|| {
            let mask: &[u8; SIDE * SIDE] = match self {
                Self::Brush => include_bytes!("../assets/tool-icons/brush.alpha"),
                Self::Eraser => include_bytes!("../assets/tool-icons/eraser.alpha"),
            };
            let pixels = mask.iter().copied().map(egui::Color32::from_white_alpha).collect();
            let texture = ctx.load_texture(
                format!("Mapper tool {self:?}"),
                egui::ColorImage::new([SIDE, SIDE], pixels),
                egui::TextureOptions::LINEAR,
            );
            ctx.data_mut(|data| data.insert_temp(id, texture.clone()));
            texture
        });
        egui::Image::new((texture.id(), egui::vec2(18., 18.)))
            .tint(ui.visuals().text_color())
    }
}
