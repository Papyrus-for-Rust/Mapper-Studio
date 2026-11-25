//! Source glyph metrics and kerning, in the marker prefab's coordinate system.
use eframe::egui::{Color32, ColorImage, Rect, pos2, vec2};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone)]
pub struct Quad {
    pub rect: Rect,
    pub uv: Rect,
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Pos2;

    fn style() -> Style {
        let glyph = Glyph {
            index: 1,
            rect: [2., 2., 4., 6.],
            metrics: [4., 6., 0., 6., 5.],
            scale: 1.,
        };
        Style {
            font_name: "Synthetic test font".into(),
            font_size: 10.,
            color: Color32::BLACK,
            width: 16,
            height: 16,
            padding: 1.,
            point_size: 10.,
            gradient: 2.,
            units: [1. / 28.; 2],
            text_rect: Rect::from_min_size(Pos2::ZERO, vec2(12. / 28., 24. / 28.)),
            ascent: 8.,
            line_height: 10.,
            horizontal: 1,
            vertical: 256,
            kerning: true,
            spacing: 0.,
            word_spacing: 0.,
            glyphs: [
                ('A', glyph.clone()),
                (
                    'B',
                    Glyph {
                        index: 2,
                        ..glyph.clone()
                    },
                ),
                (
                    ' ',
                    Glyph {
                        index: 3,
                        metrics: [0., 0., 0., 0., 3.],
                        ..glyph
                    },
                ),
            ]
            .into(),
            pairs: BTreeMap::new(),
        }
    }

    #[test]
    fn glyph_geometry_uses_source_size_and_baseline() {
        let label = style().layout("AB").unwrap();
        assert_eq!(label.bounds, Rect::from_min_max(pos2(0., 2.), pos2(9., 8.)));
        assert_eq!(label.quads.len(), 2);
        assert_eq!(
            label.quads[0].rect,
            Rect::from_min_max(pos2(-1., 1.), pos2(5., 9.))
        );
    }

    #[test]
    fn wide_labels_keep_source_kerning_on_one_line() {
        let mut style = style();
        style.pairs.insert(
            (1, 2),
            Pair {
                first: Adjustment {
                    advance: -2.,
                    ..Default::default()
                },
                second: Default::default(),
            },
        );
        assert_eq!(style.layout("AB").unwrap().bounds.right(), 7.);
        let label = style.layout("AB\nAB").unwrap();
        assert_eq!(label.quads.len(), 4);
        assert_eq!(label.quads[2].rect.top(), label.quads[0].rect.top());
        assert!(label.bounds.width() > 12.);
        assert!(style.layout("unknown").is_none());
        assert!(style.layout(&"A".repeat(257)).is_none());
    }

    #[test]
    fn atlas_rows_are_flipped_and_dimensions_checked() {
        let style = style();
        let mut alpha = vec![0; 256];
        alpha[0] = 255;
        let image = style.image(&alpha).unwrap();
        assert_eq!(image.pixels[15 * 16], Color32::WHITE);
        assert_eq!(image.pixels[0], Color32::TRANSPARENT);
        let small = style.image_scaled(&alpha, 0.25).unwrap();
        assert_eq!(small.pixels.iter().filter(|p| p.a() != 0).count(), 1);
        assert!(style.image(&alpha[..255]).is_err());
        assert!(Style::decode(&serde_json::json!({})).is_err());
    }
}
pub struct Label {
    pub quads: Vec<Quad>,
    pub bounds: Rect,
}
#[derive(Clone)]
struct Glyph {
    index: u32,
    rect: [f32; 4],
    metrics: [f32; 5],
    scale: f32,
}
#[derive(Clone, Copy, Default, PartialEq)]
struct Adjustment {
    x: f32,
    y: f32,
    advance: f32,
    vertical: f32,
}
#[derive(Clone, Copy)]
struct Pair {
    first: Adjustment,
    second: Adjustment,
}
#[derive(Clone)]
pub struct Style {
    pub font_name: String,
    pub font_size: f32,
    pub color: Color32,
    width: usize,
    height: usize,
    padding: f32,
    point_size: f32,
    gradient: f32,
    units: [f32; 2],
    text_rect: Rect,
    ascent: f32,
    line_height: f32,
    horizontal: i64,
    vertical: i64,
    kerning: bool,
    spacing: f32,
    word_spacing: f32,
    glyphs: BTreeMap<char, Glyph>,
    pairs: BTreeMap<(u32, u32), Pair>,
}
fn number(value: &Value) -> Result<f32, String> {
    value
        .as_f64()
        .filter(|v| v.is_finite() && v.abs() <= 32768.)
        .map(|v| v as f32)
        .ok_or("Invalid client map text number".into())
}
fn positive(value: &Value) -> Result<f32, String> {
    number(value).and_then(|v| {
        if v > 0. {
            Ok(v)
        } else {
            Err("Client map text dimensions must be positive".into())
        }
    })
}
fn pair(value: &Value) -> Result<[f32; 2], String> {
    Ok([number(&value["x"])?, number(&value["y"])?])
}
fn material(value: &Value, key: &str) -> Result<f32, String> {
    let entries = value["m_SavedProperties"]["m_Floats"]
        .as_array()
        .filter(|v| v.len() <= 256)
        .ok_or("Missing client font material properties")?;
    let values = entries
        .iter()
        .filter(|entry| entry["key"] == key)
        .collect::<Vec<_>>();
    if values.len() != 1 {
        return Err(format!("Missing or repeated font material property {key}"));
    }
    number(&values[0]["value"])
}
fn adjustment(value: &Value) -> Result<Adjustment, String> {
    Ok(Adjustment {
        x: number(&value["m_XPlacement"])?,
        y: number(&value["m_YPlacement"])?,
        advance: number(&value["m_XAdvance"])?,
        vertical: number(&value["m_YAdvance"])?,
    })
}
impl Style {
    /// Center the visible SDF contours independently of the prefab's text box.
    pub fn centered_label(&self, text: &str, alpha: &[u8]) -> Option<Label> {
        if alpha.len() != self.width * self.height {
            return None;
        }
        let mut label = self.layout(text)?;
        let size = vec2(self.width as f32, self.height as f32);
        let mut ink = Rect::NOTHING;
        let mut remaining = 1_000_000_usize;
        for quad in &label.quads {
            let min = quad.uv.min.to_vec2() * size;
            let max = quad.uv.max.to_vec2() * size;
            let columns = min.x.ceil() as usize..(max.x.floor() as usize).min(self.width);
            let rows = min.y.ceil() as usize..(max.y.floor() as usize).min(self.height);
            let Some(budget) = remaining.checked_sub(rows.len() * columns.len()) else {
                ink = Rect::NOTHING;
                break;
            };
            remaining = budget;
            let mut glyph_ink = Rect::NOTHING;
            for y in rows {
                let row = (self.height - 1 - y) * self.width;
                for x in columns.clone() {
                    if alpha[row + x] >= 128 {
                        glyph_ink.extend_with(pos2(x as f32, y as f32));
                        glyph_ink.extend_with(pos2(x as f32 + 1., y as f32 + 1.));
                    }
                }
            }
            if glyph_ink.is_positive() {
                let project = |point| quad.rect.lerp_inside((point - min.to_pos2()) / (max - min));
                ink = ink.union(Rect::from_min_max(project(glyph_ink.min), project(glyph_ink.max)));
            }
        }
        if ink.is_positive() {
            label.bounds = ink;
        }
        let offset = -label.bounds.center().to_vec2();
        label.bounds = label.bounds.translate(offset);
        for quad in &mut label.quads {
            quad.rect = quad.rect.translate(offset);
        }
        Some(label)
    }

    pub fn decode(value: &Value) -> Result<Self, String> {
        let font = &value["font"];
        let text = &value["text"];
        let atlas = &value["atlas"];
        let material_properties = &value["material"];
        let dimensions = ["m_Width", "m_Height"].map(|key| {
            atlas[key]
                .as_u64()
                .filter(|&v| (16..=4096).contains(&v))
                .map(|v| v as usize)
        });
        let [Some(width), Some(height)] = dimensions else {
            return Err("Invalid client font atlas dimensions".into());
        };
        if atlas["m_TextureFormat"] != 1
            || atlas["m_MipCount"] != 1
            || font["m_AtlasTextures"]
                .as_array()
                .is_none_or(|v| v.len() != 1)
            || font["m_AtlasWidth"].as_u64() != Some(width as u64)
            || font["m_AtlasHeight"].as_u64() != Some(height as u64)
            || text["m_fontStyle"] != 0
            || text["m_fontWeight"] != 400
            || text["m_enableAutoSizing"] != 0
            || [
                "_FaceDilate",
                "_OutlineWidth",
                "_WeightNormal",
                "_Sharpness",
            ]
            .iter()
            .any(|key| material(material_properties, key) != Ok(0.))
        {
            return Err("Client map font texture or styling requires another adapter".into());
        }
        let font_name = font["m_Name"]
            .as_str()
            .filter(|v| !v.is_empty() && v.len() <= 256)
            .ok_or("Missing client map font name")?
            .to_owned();
        let point_size = positive(&font["m_FaceInfo"]["m_PointSize"])?;
        let face_scale = positive(&font["m_FaceInfo"]["m_Scale"])?;
        let font_size = positive(&text["m_fontSize"])?;
        let padding = number(&font["m_AtlasPadding"])?;
        let gradient = material(material_properties, "_GradientScale")?;
        if !(0. ..=128.).contains(&padding)
            || !(0. ..=256.).contains(&gradient)
            || gradient == 0.
            || font_size > 512.
        {
            return Err("Client map font scale exceeds its bounds".into());
        }
        let root = &value["root_transform"];
        let transform = &value["text_transform"];
        let root_size = pair(&root["m_SizeDelta"])?;
        let root_scale = pair(&root["m_LocalScale"])?;
        let text_size = pair(&transform["m_SizeDelta"])?;
        let text_scale = pair(&transform["m_LocalScale"])?;
        if root_size
            .iter()
            .chain(&root_scale)
            .chain(&text_size)
            .chain(&text_scale)
            .any(|&v| v <= 0.)
            || transform["m_AnchorMin"] != transform["m_AnchorMax"]
            || [root, transform].iter().any(|v| {
                ["x", "y", "z"]
                    .iter()
                    .any(|axis| number(&v["m_LocalRotation"][axis]) != Ok(0.))
                    || number(&v["m_LocalRotation"]["w"]) != Ok(1.)
            })
        {
            return Err("Client map label transform requires another adapter".into());
        }
        let anchor = pair(&transform["m_AnchorMin"])?;
        let pivot = pair(&transform["m_Pivot"])?;
        let offset = pair(&transform["m_AnchoredPosition"])?;
        let units = [
            1. / root_size[0],
            root_scale[1] / root_scale[0] / root_size[0],
        ];
        let left = ((anchor[0] - 0.5) * root_size[0] + offset[0]
            - pivot[0] * text_size[0] * text_scale[0])
            * units[0];
        let top = -((anchor[1] - 0.5) * root_size[1]
            + offset[1]
            + (1. - pivot[1]) * text_size[1] * text_scale[1])
            * units[1];
        let text_rect = Rect::from_min_size(
            pos2(left, top),
            vec2(
                text_size[0] * text_scale[0] * units[0],
                text_size[1] * text_scale[1] * units[1],
            ),
        );
        let horizontal = text["m_HorizontalAlignment"]
            .as_i64()
            .filter(|v| [1, 2, 4].contains(v))
            .ok_or("Unsupported client text horizontal alignment")?;
        let vertical = text["m_VerticalAlignment"]
            .as_i64()
            .filter(|v| [256, 512, 1024, 2048].contains(v))
            .ok_or("Unsupported client text vertical alignment")?;
        let kerning = text["m_enableKerning"]
            .as_i64()
            .filter(|v| [0, 1].contains(v))
            .ok_or("Unsupported client text kerning")?
            == 1;
        let glyph_entries = font["m_GlyphTable"]
            .as_array()
            .filter(|v| !v.is_empty() && v.len() <= 16384)
            .ok_or("Invalid client font glyph table")?;
        let mut glyphs_by_index = BTreeMap::new();
        for entry in glyph_entries {
            let index = entry["m_Index"]
                .as_u64()
                .filter(|&v| v <= u32::MAX as u64)
                .ok_or("Invalid client glyph identity")? as u32;
            let rect = &entry["m_GlyphRect"];
            let rect = [
                number(&rect["m_X"])?,
                number(&rect["m_Y"])?,
                number(&rect["m_Width"])?,
                number(&rect["m_Height"])?,
            ];
            if entry["m_AtlasIndex"] != 0
                || rect.iter().any(|v| *v < 0.)
                || (rect[2] > 0.
                    && rect[3] > 0.
                    && (rect[0] < padding
                        || rect[1] < padding
                        || rect[0] + rect[2] + padding > width as f32
                        || rect[1] + rect[3] + padding > height as f32))
            {
                return Err("Client glyph is outside its atlas".into());
            }
            let metrics = &entry["m_Metrics"];
            let glyph = Glyph {
                index,
                rect,
                metrics: [
                    number(&metrics["m_Width"])?,
                    number(&metrics["m_Height"])?,
                    number(&metrics["m_HorizontalBearingX"])?,
                    number(&metrics["m_HorizontalBearingY"])?,
                    number(&metrics["m_HorizontalAdvance"])?,
                ],
                scale: positive(&entry["m_Scale"])?,
            };
            if glyphs_by_index.insert(index, glyph).is_some() {
                return Err("Repeated client font glyph identity".into());
            }
        }
        let characters = font["m_CharacterTable"]
            .as_array()
            .filter(|v| !v.is_empty() && v.len() <= 65536)
            .ok_or("Invalid client font character table")?;
        let mut glyphs = BTreeMap::new();
        for entry in characters {
            let character = entry["m_Unicode"]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .and_then(char::from_u32)
                .ok_or("Invalid client font character")?;
            let index = entry["m_GlyphIndex"]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .ok_or("Missing client character glyph")?;
            let mut glyph = glyphs_by_index
                .get(&index)
                .ok_or("Client character references an absent glyph")?
                .clone();
            glyph.scale *= positive(&entry["m_Scale"])?;
            if glyphs.insert(character, glyph).is_some() {
                return Err("Repeated client font character".into());
            }
        }
        let features = &font["m_FontFeatureTable"];
        for key in [
            "m_LigatureSubstitutionRecords",
            "m_MarkToBaseAdjustmentRecords",
            "m_MarkToMarkAdjustmentRecords",
            "m_MultipleSubstitutionRecords",
        ] {
            if features[key].as_array().is_some_and(|v| !v.is_empty()) {
                return Err("Client font substitution features require another adapter".into());
            }
        }
        let records = features["m_GlyphPairAdjustmentRecords"]
            .as_array()
            .filter(|v| v.len() <= 65536)
            .ok_or("Invalid client font kerning table")?;
        let mut pairs = BTreeMap::new();
        for entry in records {
            let first = &entry["m_FirstAdjustmentRecord"];
            let second = &entry["m_SecondAdjustmentRecord"];
            let first_adjustment = adjustment(&first["m_GlyphValueRecord"])?;
            let second_adjustment = adjustment(&second["m_GlyphValueRecord"])?;
            if first_adjustment == Adjustment::default()
                && second_adjustment == Adjustment::default()
            {
                continue;
            }
            let first = first["m_GlyphIndex"]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .ok_or("Invalid first kerning glyph")?;
            let second = second["m_GlyphIndex"]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .ok_or("Invalid second kerning glyph")?;
            if entry["m_FeatureLookupFlags"] != 0
                || !glyphs_by_index.contains_key(&first)
                || !glyphs_by_index.contains_key(&second)
                || pairs
                    .insert(
                        (first, second),
                        Pair {
                            first: first_adjustment,
                            second: second_adjustment,
                        },
                    )
                    .is_some()
            {
                return Err("Unsupported or repeated client kerning pair".into());
            }
        }
        let color = &value["marker"]["dayColor"];
        let rgba = ["r", "g", "b", "a"].map(|key| number(&color[key]));
        let mut bytes = [0; 4];
        for (byte, value) in bytes.iter_mut().zip(rgba) {
            let value = value?;
            if !(0. ..=1.).contains(&value) {
                return Err("Invalid client marker color".into());
            }
            *byte = (value * 255.).round() as u8;
        }
        Ok(Self {
            font_name,
            font_size,
            color: Color32::from_rgba_unmultiplied(bytes[0], bytes[1], bytes[2], bytes[3]),
            width,
            height,
            padding,
            point_size,
            gradient,
            units: [
                units[0]
                    * text_scale[0]
                    * face_scale
                    * positive(&text["m_characterHorizontalScale"])?,
                units[1] * text_scale[1] * face_scale,
            ],
            text_rect,
            ascent: number(&font["m_FaceInfo"]["m_AscentLine"])?,
            line_height: positive(&font["m_FaceInfo"]["m_LineHeight"])?
                + number(&text["m_lineSpacing"])?,
            horizontal,
            vertical,
            kerning,
            spacing: number(&text["m_characterSpacing"])?,
            word_spacing: number(&text["m_wordSpacing"])?,
            glyphs,
            pairs,
        })
    }
    pub fn image(&self, alpha: &[u8]) -> Result<ColorImage, String> {
        self.image_scaled(alpha, 1.)
    }
    pub fn image_scaled(&self, alpha: &[u8], pixels_per_point: f32) -> Result<ColorImage, String> {
        if !pixels_per_point.is_finite() || !(0.25..=8.).contains(&pixels_per_point) {return Err("Invalid map font image scale".into());}
        if alpha.len() != self.width * self.height {
            return Err("Client font atlas byte count differs from its dimensions".into());
        }
        // At small export sizes a wider transition would give even the
        // atlas's empty texels opacity, exposing the padded glyph rectangles.
        let scale = (self.gradient * self.font_size / self.point_size * self.units[1]
            * super::MARKER_SIDE * pixels_per_point).max(1.);
        let mut image = ColorImage::filled([self.width, self.height], Color32::TRANSPARENT);
        for y in 0..self.height {
            for x in 0..self.width {
                let distance = f32::from(alpha[y * self.width + x]) / 255.;
                let coverage = ((distance - 0.5) * scale + 0.5).clamp(0., 1.);
                image.pixels[(self.height - 1 - y) * self.width + x] =
                    Color32::from_white_alpha((coverage * 255.).round() as u8);
            }
        }
        Ok(image)
    }
    fn adjusted(&self, characters: &[char], index: usize) -> Option<(&Glyph, Adjustment)> {
        let glyph = self.glyphs.get(&characters[index])?;
        let mut adjustment = Adjustment::default();
        if self.kerning {
            if index > 0
                && let Some(previous) = self.glyphs.get(&characters[index - 1])
                && let Some(pair) = self.pairs.get(&(previous.index, glyph.index))
            {
                adjustment = pair.second;
            }
            if let Some(next) = characters.get(index + 1).and_then(|c| self.glyphs.get(c))
                && let Some(pair) = self.pairs.get(&(glyph.index, next.index))
            {
                adjustment.x += pair.first.x;
                adjustment.y += pair.first.y;
                adjustment.advance += pair.first.advance;
                adjustment.vertical += pair.first.vertical;
            }
        }
        Some((glyph, adjustment))
    }
    fn advance(&self, characters: &[char]) -> Option<f32> {
        let mut width = 0.;
        for index in 0..characters.len() {
            let (glyph, adjustment) = self.adjusted(characters, index)?;
            width += glyph.metrics[4] * glyph.scale
                + adjustment.advance
                + self.spacing
                + if characters[index].is_whitespace() {
                    self.word_spacing
                } else {
                    0.
                };
        }
        Some(width)
    }
    pub fn layout(&self, text: &str) -> Option<Label> {
        if text.is_empty() || text.chars().count() > 256 {
            return None;
        }
        let sx = self.font_size / self.point_size * self.units[0] * super::MARKER_SIDE;
        let sy = self.font_size / self.point_size * self.units[1] * super::MARKER_SIDE;
        let rect = Rect::from_min_max(
            self.text_rect.min * super::MARKER_SIDE,
            self.text_rect.max * super::MARKER_SIDE,
        );
        let mut characters = Vec::new();
        for word in text.split_whitespace() {
            if !characters.is_empty() {
                characters.push(' ');
            }
            characters.extend(word.chars());
        }
        if characters.is_empty() {
            return None;
        }
        let height = self.line_height * sy;
        let mut y = match self.vertical {
            256 => rect.top() + self.ascent * sy,
            512 => rect.center().y - height / 2. + self.ascent * sy,
            1024 => rect.bottom() - height + self.ascent * sy,
            2048 => rect.center().y,
            _ => return None,
        };
        let mut quads = Vec::new();
        let mut bounds = Rect::NOTHING;
        let width = self.advance(&characters)? * sx;
        let mut x = match self.horizontal {
            1 => rect.left(),
            2 => rect.center().x - width / 2.,
            4 => rect.right() - width,
            _ => return None,
        };
        for index in 0..characters.len() {
            let (glyph, adjustment) = self.adjusted(&characters, index)?;
            let [width, height, bearing_x, bearing_y, advance] = glyph.metrics;
            let gx = sx * glyph.scale;
            let gy = sy * glyph.scale;
            if width > 0. && height > 0. {
                let ink = Rect::from_min_size(
                    pos2(
                        x + bearing_x * gx + adjustment.x * sx,
                        y - bearing_y * gy - adjustment.y * sy,
                    ),
                    vec2(width * gx, height * gy),
                );
                bounds = bounds.union(ink);
                let padded = Rect::from_min_max(
                    ink.min - vec2(self.padding * gx, self.padding * gy),
                    ink.max + vec2(self.padding * gx, self.padding * gy),
                );
                let [rx, ry, rw, rh] = glyph.rect;
                let uv = Rect::from_min_max(
                    pos2(
                        (rx - self.padding) / self.width as f32,
                        1. - (ry + rh + self.padding) / self.height as f32,
                    ),
                    pos2(
                        (rx + rw + self.padding) / self.width as f32,
                        1. - (ry - self.padding) / self.height as f32,
                    ),
                );
                quads.push(Quad { rect: padded, uv });
            }
            x += (advance * glyph.scale
                + adjustment.advance
                + self.spacing
                + if characters[index].is_whitespace() {
                    self.word_spacing
                } else {
                    0.
                })
                * sx;
            y -= adjustment.vertical * sy;
        }
        bounds.is_positive().then_some(Label { quads, bounds })
    }
}
