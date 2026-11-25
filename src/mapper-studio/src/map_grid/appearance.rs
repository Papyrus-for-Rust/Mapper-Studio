use super::Grid;
use eframe::egui::{Color32, ColorImage, Rect, Rgba, pos2, vec2};
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::Path, sync::Arc};

const SOURCE: &str = "Bundles/shared/content.bundle";
const ATLAS: [usize; 2] = [4096, 128];
const DRAW_ATLAS: [usize; 2] = [2048, 256];
const LEGACY_ZOOM: f32 = std::f32::consts::E;
const CAPTURE: &[u8] =
    include_bytes!("../../../../tools/Mapper.Diagnostics/extract_paper_map_grid.py");

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: u32,
    alpha_sha256: String,
    color: [f32; 3],
    line_opacity: f32,
    number_offset: f32,
    number_opacity: f32,
    number_size: f32,
    cells_per_1024: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    zoom: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    line_mips: Option<[Vec<u8>; 10]>,
}

/// A PaperMap glyph rectangle in cell units and its corresponding atlas crop.
pub struct Glyph {
    pub cell_rect: Rect,
    pub uv: Rect,
}

pub struct Appearance {
    receipt: Receipt,
    alpha: Arc<[u8]>,
    pub atlas: Arc<ColorImage>,
    line_id: Option<[u8; 32]>,
}

impl Appearance {
    pub fn read_capture(folder: &Path) -> Result<Self, String> {
        let read = |name, limit| -> Result<Vec<u8>, String> {
            let mut bytes = Vec::new();
            fs::File::open(folder.join(name))
                .map_err(|e| e.to_string())?
                .take(limit)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            Ok(bytes)
        };
        let metadata =
            serde_json::from_slice(&read("grid.json", 16385)?).map_err(|e| e.to_string())?;
        Self::from_snapshot(
            metadata,
            read("grid.alpha", (ATLAS[0] * ATLAS[1] + 1) as u64)?.into(),
        )
    }

    pub fn from_snapshot(metadata: serde_json::Value, alpha: Arc<[u8]>) -> Result<Self, String> {
        let receipt: Receipt = serde_json::from_value(metadata).map_err(|e| e.to_string())?;
        if receipt.schema != 1
            || receipt
                .color
                .iter()
                .chain([&receipt.line_opacity, &receipt.number_opacity])
                .any(|v| !(0. ..=1.).contains(v))
            || !(0. ..=0.15).contains(&receipt.number_offset)
            || !(0.1..=0.5).contains(&receipt.number_size)
            || !(4. ..=10.).contains(&receipt.cells_per_1024)
            || receipt
                .zoom
                .is_some_and(|zoom| !(1. ..=16.).contains(&zoom))
            || receipt.line_mips.as_ref().is_some_and(|mips| {
                mips.iter().enumerate().any(|(level, patch)| {
                    patch.len() != (512_usize >> level).min(9).pow(2)
                })
            })
            || alpha.len() != ATLAS[0] * ATLAS[1]
            || format!("{:x}", Sha256::digest(&alpha)) != receipt.alpha_sha256
        {
            return Err("Invalid PaperMap grid capture".into());
        }
        // Stack the two halves to fit the 2048-pixel offscreen texture limit.
        let mut pixels = Vec::with_capacity(alpha.len());
        for half in 0..2 {
            let offset = half * DRAW_ATLAS[0];
            for row in alpha.chunks_exact(ATLAS[0]) {
                pixels.extend(
                    row[offset..offset + DRAW_ATLAS[0]]
                        .iter()
                        .map(|&a| Color32::from_white_alpha(a)),
                );
            }
        }
        let line_id = receipt.line_mips.as_ref().map(|mips| {
            let mut hash = Sha256::new();
            for mip in mips {
                hash.update(mip);
            }
            hash.finalize().into()
        });
        Ok(Self {
            receipt,
            alpha,
            atlas: Arc::new(ColorImage::new(DRAW_ATLAS, pixels)),
            line_id,
        })
    }

    pub fn snapshot(&self) -> (serde_json::Value, Arc<[u8]>) {
        (
            serde_json::to_value(&self.receipt).expect("validated grid appearance"),
            self.alpha.clone(),
        )
    }

    pub fn grid(&self, size: u32) -> Option<Grid> {
        Grid::with_paper_spread(size, self.receipt.cells_per_1024)
    }

    pub fn atlas_id(&self) -> &str {
        &self.receipt.alpha_sha256
    }

    pub fn adds_line_samples_to(&self, older: &Self) -> bool {
        let a = &self.receipt;
        let b = &older.receipt;
        self.line_id.is_some()
            && older.line_id.is_none()
            && a.alpha_sha256 == b.alpha_sha256
            && a.color == b.color
            && a.line_opacity == b.line_opacity
            && a.number_offset == b.number_offset
            && a.number_opacity == b.number_opacity
            && a.number_size == b.number_size
            && a.cells_per_1024 == b.cells_per_1024
            && a.zoom.unwrap_or(LEGACY_ZOOM) == b.zoom.unwrap_or(LEGACY_ZOOM)
    }

    pub fn line_uv(
        &self,
        ctx: &eframe::egui::Context,
        cell_pixels: f32,
    ) -> Option<(eframe::egui::TextureHandle, Rect, f32)> {
        use eframe::egui::{Id, TextureHandle, TextureOptions};
        let mips = self.receipt.line_mips.as_ref()?;
        let level = (512. / cell_pixels).log2().round().clamp(0., 9.) as usize;
        let key = (self.line_id?, level);
        let id = Id::new("paper-map-grid-lines");
        let texture = ctx
            .data(|data| data.get_temp::<(([u8; 32], usize), TextureHandle)>(id))
            .filter(|(cached, _)| *cached == key)
            .map(|(_, texture)| texture)
            .unwrap_or_else(|| {
                let image = line_image(level, &mips[level]);
                let texture = ctx.load_texture("PaperMap grid lines", image, TextureOptions::LINEAR);
                ctx.data_mut(|data| data.insert_temp(id, (key, texture.clone())));
                texture
            });
        let edge = 1. / texture.size()[0] as f32;
        let side = 512 >> level;
        // Skip the clear center of large tiles, including the half-texel
        // bilinear fringe. This avoids scanning it in the PNG rasterizer.
        let border = if side > 9 && mips[level][4 * 9 + 4] == 0 {
            4.5 / side as f32
        } else {
            0.5
        };
        Some((texture, Rect::from_min_max(pos2(edge, edge), pos2(1. - edge, 1. - edge)), border))
    }

    pub fn glyphs(&self, column: u32, row: u32) -> impl Iterator<Item = Glyph> {
        let size = self.receipt.number_size / self.receipt.zoom.unwrap_or(LEGACY_ZOOM);
        glyphs(column, row, self.receipt.number_offset, size)
    }

    pub fn color(&self, labels: bool) -> Color32 {
        let [r, g, b] = self.receipt.color;
        let opacity = if labels {
            self.receipt.number_opacity
        } else {
            self.receipt.line_opacity
        };
        Color32::from(Rgba::from_rgba_unmultiplied(r, g, b, opacity))
    }
}

fn line_image(level: usize, patch: &[u8]) -> ColorImage {
    let side = 512 >> level;
    let stored = side.min(9);
    let index = |pixel| {
        // A gutter duplicates the opposite edge for bilinear repeat sampling.
        let source = (pixel + side - 1) % side;
        if side <= 9 || source < 4 {
            source
        } else if source >= side - 4 {
            source - (side - 9)
        } else {
            4
        }
    };
    let side_with_gutter = side + 2;
    let mut pixels = Vec::with_capacity(side_with_gutter * side_with_gutter);
    for y in 0..side_with_gutter {
        for x in 0..side_with_gutter {
            // The captured texture blocks use Unity's bottom-up UV order.
            // Egui image rows run down from the north edge of the map.
            pixels.push(Color32::from_white_alpha(patch[index(side + 1 - y) * stored + index(x)]));
        }
    }
    ColorImage::new([side_with_gutter; 2], pixels)
}

fn glyphs(column: u32, row: u32, inset: f32, size: f32) -> impl Iterator<Item = Glyph> {
    // PaperMap samples one of 64 entries across the original atlas. In the
    // repacked atlas each half occupies two rows (letters, then numbers).
    let glyph = move |index: u32, number: bool, x: f32| {
        let u = (index % 32) as f32 / 32.;
        let v = (2 * (index / 32) + u32::from(number)) as f32 / 4.;
        let rect = Rect::from_min_size(pos2(inset + x * size, inset), vec2(size, size));
        let cell_rect = rect.intersect(Rect::from_min_max(pos2(0., 0.), pos2(1., 1.)));
        let uv = |point| pos2(u, v) + (point - rect.min) / rect.size() * vec2(1. / 32., 0.25);
        cell_rect.is_positive().then(|| Glyph {
            cell_rect,
            uv: Rect::from_min_max(uv(cell_rect.min), uv(cell_rect.max)),
        })
    };
    let two_letters = column >= 26;
    let x = if two_letters { 0.45 } else { -0.2 };
    [
        two_letters.then(|| glyph(column / 26 - 1, false, -0.2)).flatten(),
        glyph(column % 26, false, x),
        glyph(row, true, x + 0.85),
    ]
    .into_iter()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_texture_preserves_corner_samples_and_repeats_at_edges() {
        let mut patch = [0; 81];
        patch[0] = 206;
        patch[8] = 198;
        patch[4] = 148;
        let image = line_image(2, &patch);
        assert_eq!(image.size, [130, 130]);
        assert_eq!(image[(1, 128)].a(), 206);
        assert_eq!(image[(129, 128)].a(), 206);
        assert_eq!(image[(0, 128)].a(), 198);
        assert_eq!(image[(65, 128)].a(), 148);
        assert_eq!(image[(1, 0)].a(), 206);
        assert_eq!(image[(65, 65)].a(), 0);
    }

    #[test]
    fn paper_map_samples_letter_and_number_slots_with_shader_offsets() {
        let single: Vec<_> = glyphs(25, 31, 0.05, 0.1).collect();
        assert_eq!(single.len(), 2);
        assert_eq!(single[0].uv.min, pos2(25. / 32., 0.));
        assert_eq!(single[1].uv.min, pos2(31. / 32., 0.25));
        assert!((single[0].cell_rect.left() - 0.03).abs() < 1e-6);
        assert!((single[1].cell_rect.left() - 0.115).abs() < 1e-6);
        let double: Vec<_> = glyphs(26, 32, 0.05, 0.1).collect();
        assert_eq!(double.len(), 3);
        assert_eq!(double[0].uv.min, pos2(0., 0.));
        assert_eq!(double[1].uv.min, pos2(0., 0.));
        assert_eq!(double[2].uv.min, pos2(0., 0.75));
        assert!((double[0].cell_rect.left() - 0.03).abs() < 1e-6);
        assert!((double[0].cell_rect.top() - 0.05).abs() < 1e-6);
        assert!((double[1].cell_rect.left() - 0.095).abs() < 1e-6);
        assert!((double[2].cell_rect.left() - 0.18).abs() < 1e-6);

        let flush: Vec<_> = glyphs(0, 0, 0., 0.25).collect();
        assert_eq!(flush[0].cell_rect.left(), 0.);
        assert!((flush[0].uv.left() - 0.2 / 32.).abs() < 1e-6);
    }
}

pub(crate) fn prepare(
    cache: &Path,
    root: &Path,
    cancelled: &impl Fn() -> bool,
) -> Option<Appearance> {
    if cancelled() {
        return None;
    }
    let _lease = crate::source_lease::open(&root.join(SOURCE)).ok()?;
    let source =
        crate::installation::hash_file(root, SOURCE, 8 * 1024_u64.pow(3), cancelled).ok()?;
    let version = format!("{:x}", Sha256::digest(CAPTURE));
    let base = cache.join("paper-grid");
    let key = format!("{}-{version}", source.sha256);
    let destination = base.join(key);
    if let Ok(value) = Appearance::read_capture(&destination) {
        return Some(value);
    }
    fs::create_dir_all(&base).ok()?;
    let temporary = tempfile::Builder::new()
        .prefix(".paper-grid-")
        .tempdir_in(&base)
        .ok()?;
    let output = temporary.path().join("grid");
    if !crate::metadata_runtime::run(
        crate::metadata_runtime::Script::PaperMapGrid,
        &[root.as_os_str().into(), output.as_os_str().into()],
        cancelled,
    )
    .ok()?
        || cancelled()
    {
        return None;
    }
    let value = Appearance::read_capture(&output).ok()?;
    if source
        != crate::installation::hash_file(root, SOURCE, 8 * 1024_u64.pow(3), cancelled).ok()?
    {
        return None;
    }
    if fs::rename(&output, &destination).is_ok() {
        return Some(value);
    }
    Appearance::read_capture(&destination).ok()
}
