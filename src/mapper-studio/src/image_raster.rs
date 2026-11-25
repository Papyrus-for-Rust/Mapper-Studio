//! Bounded CPU composition of egui overlay meshes into export pixels.
use eframe::egui::{self, Color32, ColorImage, Pos2, TextureId};
use std::{collections::HashMap, sync::Arc};

pub fn paint(
    image: &mut ColorImage,
    context: &egui::Context,
    mut frame: egui::FullOutput,
    scale: f32,
    cancelled: impl Fn() -> bool,
) -> Result<bool, String> {
    let mut textures = HashMap::<TextureId, Arc<ColorImage>>::new();
    let mut partial = false;
    for (id, changes) in &frame.textures_delta.set {
        for change in changes {
            if let Some([x, y]) = change.pos {
                let Some(target) = textures.get_mut(id) else {
                    partial = true;
                    continue;
                };
                let egui::ImageData::Color(pixels) = &change.image;
                let target = Arc::make_mut(target);
                if x + pixels.size[0] > target.size[0] || y + pixels.size[1] > target.size[1] {
                    partial = true;
                    continue;
                }
                for row in 0..pixels.size[1] {
                    let offset = (y + row) * target.size[0] + x;
                    target.pixels[offset..offset + pixels.size[0]].copy_from_slice(
                        &pixels.pixels[row * pixels.size[0]..(row + 1) * pixels.size[0]],
                    );
                }
                continue;
            }
            let egui::ImageData::Color(pixels) = &change.image;
            textures.insert(*id, pixels.clone());
        }
    }
    frame.textures_delta.clear();
    if partial {
        return Err("Export requires complete overlay textures".into());
    }
    let primitives = context.tessellate(frame.shapes, frame.pixels_per_point);
    for primitive in primitives {
        if cancelled() {
            return Ok(false);
        }
        let egui::epaint::Primitive::Mesh(mesh) = primitive.primitive else {
            return Err("Unsupported image overlay callback".into());
        };
        let texture = textures
            .get(&mesh.texture_id)
            .ok_or("Missing export overlay texture")?;
        let clip = egui::Rect::from_min_max(
            primitive.clip_rect.min * scale,
            primitive.clip_rect.max * scale,
        )
        .intersect(egui::Rect::from_min_size(
            Pos2::ZERO,
            egui::vec2(image.size[0] as f32, image.size[1] as f32),
        ));
        for triangle in mesh.indices.chunks_exact(3) {
            let mut vertices: [egui::epaint::Vertex; 3] =
                std::array::from_fn(|i| mesh.vertices[triangle[i] as usize]);
            for vertex in &mut vertices {
                vertex.pos *= scale;
            }
            let mut area = edge(vertices[0].pos, vertices[1].pos, vertices[2].pos);
            if area.abs() < 1e-6 {
                continue;
            }
            if area < 0. {
                vertices.swap(1, 2);
                area = -area;
            }
            let points = vertices.map(|v| v.pos);
            let bounds = egui::Rect::from_points(&points).intersect(clip);
            if !bounds.is_positive() {
                continue;
            }
            let left = bounds.left().floor().max(0.) as usize;
            let right = (bounds.right().ceil() as usize).min(image.size[0]);
            let top = bounds.top().floor().max(0.) as usize;
            let bottom = (bounds.bottom().ceil() as usize).min(image.size[1]);
            let inclusive = [
                top_left(points[1], points[2]),
                top_left(points[2], points[0]),
                top_left(points[0], points[1]),
            ];
            for y in top..bottom {
                if y % 8 == 0 && cancelled() {
                    return Ok(false);
                }
                for x in left..right {
                    let point = egui::pos2(x as f32 + 0.5, y as f32 + 0.5);
                    if !clip.contains(point) {
                        continue;
                    }
                    let edges = [
                        edge(points[1], points[2], point),
                        edge(points[2], points[0], point),
                        edge(points[0], points[1], point),
                    ];
                    if edges
                        .iter()
                        .zip(inclusive)
                        .any(|(&value, inclusive)| value < 0. || (value == 0. && !inclusive))
                    {
                        continue;
                    }
                    let weights = edges.map(|e| e / area);
                    let uv = Pos2::new(
                        (0..3).map(|i| vertices[i].uv.x * weights[i]).sum(),
                        (0..3).map(|i| vertices[i].uv.y * weights[i]).sum(),
                    );
                    let texel = sample(texture, uv);
                    let color: [f32; 4] = std::array::from_fn(|c| {
                        texel[c]
                            * (0..3)
                                .map(|i| vertices[i].color[c] as f32 * weights[i])
                                .sum::<f32>()
                            / 255.
                    });
                    let old = image.pixels[y * image.size[0] + x];
                    let blended: [u8; 4] = std::array::from_fn(|c| {
                        (color[c] + old[c] as f32 * (1. - color[3] / 255.))
                            .round()
                            .clamp(0., 255.) as u8
                    });
                    image.pixels[y * image.size[0] + x] = Color32::from_rgba_premultiplied(
                        blended[0], blended[1], blended[2], blended[3],
                    );
                }
            }
        }
    }
    Ok(true)
}

fn edge(a: Pos2, b: Pos2, p: Pos2) -> f32 {
    (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x)
}
fn top_left(a: Pos2, b: Pos2) -> bool {
    b.y < a.y || (b.y == a.y && b.x > a.x)
}
fn sample(texture: &ColorImage, uv: Pos2) -> [f32; 4] {
    let x = (uv.x * texture.size[0] as f32 - 0.5).clamp(0., (texture.size[0] - 1) as f32);
    let y = (uv.y * texture.size[1] as f32 - 0.5).clamp(0., (texture.size[1] - 1) as f32);
    let x0 = x as usize;
    let y0 = y as usize;
    let x1 = (x0 + 1).min(texture.size[0] - 1);
    let y1 = (y0 + 1).min(texture.size[1] - 1);
    let dx = x - x0 as f32;
    let dy = y - y0 as f32;
    let pixels = [
        texture.pixels[y0 * texture.size[0] + x0],
        texture.pixels[y0 * texture.size[0] + x1],
        texture.pixels[y1 * texture.size[0] + x0],
        texture.pixels[y1 * texture.size[0] + x1],
    ];
    let weights = [
        (1. - dx) * (1. - dy),
        dx * (1. - dy),
        (1. - dx) * dy,
        dx * dy,
    ];
    std::array::from_fn(|c| {
        pixels
            .iter()
            .zip(weights)
            .map(|(p, w)| p[c] as f32 * w)
            .sum()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_edges_blend_once_and_clip_to_pixel_centers() {
        let ctx = egui::Context::default();
        let rect = egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(32., 32.));
        let frame = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(rect),
                ..Default::default()
            },
            |ui| {
                ui.painter().with_clip_rect(rect).rect_filled(
                    rect,
                    0.,
                    Color32::from_white_alpha(128),
                );
            },
        );
        let mut image = ColorImage::filled([32, 32], Color32::TRANSPARENT);
        assert!(paint(&mut image, &ctx, frame, 1., || false).unwrap());
        for p in image.pixels {
            assert_eq!(p.a(), 128);
        }
    }
}
