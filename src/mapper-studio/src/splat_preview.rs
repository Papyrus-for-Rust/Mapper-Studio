//! Small read-only splat image, reused while the pointer changes only a textured quad.
use crate::{native_view::GROUND_PALETTE, prefab_assets::Asset, prefab_textures::Bindings};
use eframe::egui::{Color32, ColorImage};
use std::borrow::Cow;

/// Coverage-preserving material thumbnail. Average premultiplied channels so
/// transparent borders reduce coverage without darkening the remaining paint.
fn paint_lod(image: &ColorImage) -> Cow<'_, ColorImage> {
    let width = image.size[0].min(256);
    let height = image.size[1].min(256);
    if image.size == [width, height] {
        return Cow::Borrowed(image);
    }
    let sx = image.size[0] as f32 / width as f32;
    let sy = image.size[1] as f32 / height as f32;
    let mut pixels = Vec::with_capacity(width * height);
    for row in 0..height {
        let top = row as f32 * sy;
        let bottom = (row + 1) as f32 * sy;
        for col in 0..width {
            let left = col as f32 * sx;
            let right = (col + 1) as f32 * sx;
            let mut sum = [0.; 4];
            let mut coverage = 0.;
            for y in top.floor() as usize..(bottom.ceil() as usize).min(image.size[1]) {
                for x in left.floor() as usize..(right.ceil() as usize).min(image.size[0]) {
                    let weight = (right.min(x as f32 + 1.) - left.max(x as f32)).max(0.)
                        * (bottom.min(y as f32 + 1.) - top.max(y as f32)).max(0.);
                    let rgba = image.pixels[y * image.size[0] + x].to_array();
                    for c in 0..4 {
                        sum[c] += rgba[c] as f32 * weight;
                    }
                    coverage += weight;
                }
            }
            let rgba = sum.map(|v| (v / coverage).round().clamp(0., 255.) as u8);
            pixels.push(Color32::from_rgba_premultiplied(
                rgba[0], rgba[1], rgba[2], rgba[3],
            ));
        }
    }
    Cow::Owned(ColorImage::new([width, height], pixels))
}
/// Prepare once per immutable stamp, then reuse for pose-dependent shading.
pub fn contextual_base(image: &ColorImage) -> ColorImage {
    paint_lod(image).into_owned()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub size: [f32; 2],
    pub offset: [f32; 2],
}
impl Geometry {
    pub fn uv(&self, point: [f32; 2], x: f32, z: f32, yaw: f32) -> Option<[f32; 2]> {
        if !self.size.iter().all(|v| v.is_finite() && *v > 0.)
            || !self.offset.iter().all(|v| v.is_finite())
            || !point
                .iter()
                .chain([x, z, yaw].iter())
                .all(|v| v.is_finite())
        {
            return None;
        }
        let (s, c) = yaw.to_radians().sin_cos();
        let dx = point[0] - x;
        let dz = point[1] - z;
        let u = (c * dx + s * dz - self.offset[0]) / self.size[0] + 0.5;
        let v = 0.5 - (-s * dx + c * dz - self.offset[1]) / self.size[1];
        ((0. ..=1.).contains(&u) && (0. ..=1.).contains(&v)).then_some([u, v])
    }
    pub fn contains(&self, point: [f32; 2], x: f32, z: f32, yaw: f32) -> bool {
        self.uv(point, x, z, yaw).is_some()
    }
    /// Same positive-yaw convention as the versioned independent placement search.
    pub fn quad(&self, x: f32, z: f32, yaw: f32) -> [[f32; 2]; 4] {
        let (s, c) = yaw.to_radians().sin_cos();
        [(-0.5, 0.5), (0.5, 0.5), (0.5, -0.5), (-0.5, -0.5)].map(|(u, v)| {
            let lx = u * self.size[0] + self.offset[0];
            let lz = v * self.size[1] + self.offset[1];
            [x + c * lx - s * lz, z + s * lx + c * lz]
        })
    }
}
/// Match the displayed texture's linear alpha sampling. Faint fade fringes and
/// transparent margins do not capture clicks; this never changes terrain paint.
pub fn hit_test(image: &ColorImage, geometry: Geometry, point: [f32; 2], pose: [f32; 3]) -> bool {
    hit_test_alpha(image.size, image.pixels.len(), geometry, point, pose, |i| {
        image.pixels[i].a()
    })
}
pub fn hit_test_mask(
    size: [usize; 2],
    alpha: &[u8],
    geometry: Geometry,
    point: [f32; 2],
    pose: [f32; 3],
) -> bool {
    hit_test_alpha(size, alpha.len(), geometry, point, pose, |i| alpha[i])
}
fn hit_test_alpha(
    size: [usize; 2],
    length: usize,
    geometry: Geometry,
    point: [f32; 2],
    pose: [f32; 3],
    alpha: impl Fn(usize) -> u8,
) -> bool {
    let Some([u, v]) = geometry.uv(point, pose[0], pose[1], pose[2]) else {
        return false;
    };
    let [w, h] = size;
    if w == 0 || h == 0 || w.checked_mul(h) != Some(length) {
        return false;
    }
    let x = (u * w as f32 - 0.5).clamp(0., (w - 1) as f32);
    let y = (v * h as f32 - 0.5).clamp(0., (h - 1) as f32);
    let a = x.floor() as usize;
    let b = y.floor() as usize;
    let c = (a + 1).min(w - 1);
    let d = (b + 1).min(h - 1);
    let at = |x: usize, y: usize| alpha(y * w + x) as f32;
    let lo = at(a, b) * (1. - x.fract()) + at(c, b) * x.fract();
    let hi = at(a, d) * (1. - x.fract()) + at(c, d) * x.fract();
    lo * (1. - y.fract()) + hi * y.fract() >= 16.
}
fn sample_pixel(image: &ColorImage, u: f32, v: f32) -> Color32 {
    let [w, h] = image.size;
    if w == 0 || h == 0 || w.checked_mul(h) != Some(image.pixels.len()) {
        return Color32::TRANSPARENT;
    }
    let x = (u * w as f32 - 0.5).clamp(0., (w - 1) as f32);
    let y = (v * h as f32 - 0.5).clamp(0., (h - 1) as f32);
    let (a, b) = (x.floor() as usize, y.floor() as usize);
    let (c, d) = ((a + 1).min(w - 1), (b + 1).min(h - 1));
    let rgba: [u8; 4] = std::array::from_fn(|ch| {
        let at = |x: usize, y: usize| image.pixels[y * w + x].to_array()[ch] as f32;
        let lo = at(a, b) * (1. - x.fract()) + at(c, b) * x.fract();
        let hi = at(a, d) * (1. - x.fract()) + at(c, d) * x.fract();
        (lo * (1. - y.fract()) + hi * y.fract())
            .round()
            .clamp(0., 255.) as u8
    });
    Color32::from_rgba_premultiplied(rgba[0], rgba[1], rgba[2], rgba[3])
}
/// Picking uses the same maximum material/biome coverage as the contextual image.
pub fn contextual_coverage(material: &ColorImage, biome: &ColorImage) -> ColorImage {
    let [w, h] = material.size;
    if w == 0 || h == 0 || w.checked_mul(h) != Some(material.pixels.len()) {
        return material.clone();
    }
    ColorImage::new(
        material.size,
        material
            .pixels
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let b = sample_pixel(
                    biome,
                    ((i % w) as f32 + 0.5) / w as f32,
                    ((i / w) as f32 + 0.5) / h as f32,
                );
                Color32::from_white_alpha(p.a().max(b.a()))
            })
            .collect(),
    )
}

/// Tint at the transformed world position, so rotating a stamp doesn't rotate the
/// underlying climate or sunlight. Alpha still comes from authored material paint.
pub fn contextual_image(
    image: &ColorImage,
    geometry: Geometry,
    layers: &papyrus_terrain::native_layers::Layers,
    pose: [f32; 3],
) -> ColorImage {
    contextual_image_with_biome(image, None, geometry, layers, pose)
}
/// Material and biome paint combine independently before shared overview lighting.
pub fn contextual_image_with_biome(
    image: &ColorImage,
    biome: Option<&ColorImage>,
    geometry: Geometry,
    layers: &papyrus_terrain::native_layers::Layers,
    pose: [f32; 3],
) -> ColorImage {
    let [x, z, yaw] = pose;
    let quad = geometry.quad(x, z, yaw);
    // Bound CPU work during pointer movement; accepted composition uses full data.
    let image = paint_lod(image);
    let biome = biome.map(paint_lod);
    let [width, height] = image.size;
    let mut pixels = Vec::with_capacity(width * height);
    for row in 0..height {
        let v = (row as f32 + 0.5) / height as f32;
        for col in 0..width {
            let u = (col as f32 + 0.5) / width as f32;
            let p = image.pixels[row * width + col];
            let b = biome
                .as_ref()
                .map_or(Color32::TRANSPARENT, |b| sample_pixel(b, u, v));
            if p.a() == 0 && b.a() == 0 {
                pixels.push(Color32::TRANSPARENT);
                continue;
            }
            let world: [f32; 2] = std::array::from_fn(|i| {
                quad[0][i] + u * (quad[1][i] - quad[0][i]) + v * (quad[3][i] - quad[0][i])
            });
            let rgba = p.to_srgba_unmultiplied();
            let alpha = p.a().max(b.a());
            let rgb = if b.a() == 0 {
                crate::map_image::shade_ground(
                    layers,
                    world[0],
                    world[1],
                    [rgba[0] as f32, rgba[1] as f32, rgba[2] as f32],
                )
            } else {
                let old_ground = crate::map_image::ground_at(layers, world[0], world[1]);
                let old_biome = crate::map_image::biome_at(layers, world[0], world[1]);
                let bio = b.to_srgba_unmultiplied();
                let ground = std::array::from_fn(|c| {
                    old_ground[c] + (rgba[c] as f32 - old_ground[c]) * p.a() as f32 / alpha as f32
                });
                let climate = std::array::from_fn(|c| {
                    old_biome[c] + (bio[c] as f32 - old_biome[c]) * b.a() as f32 / alpha as f32
                });
                crate::map_image::shade_ground_with_biome(
                    layers, world[0], world[1], ground, climate,
                )
            };
            pixels.push(Color32::from_rgba_unmultiplied(
                rgb[0].round().clamp(0., 255.) as u8,
                rgb[1].round().clamp(0., 255.) as u8,
                rgb[2].round().clamp(0., 255.) as u8,
                alpha,
            ));
        }
    }
    ColorImage::new([width, height], pixels)
}
/// Coverage-preserving channel weights for source palette shading. The retained
/// raster is capped at 256 square, independently of source texture dimensions.
#[derive(Clone)]
pub struct WeightImage<const N: usize> {
    pub size: [usize; 2],
    pub pixels: Vec<[u8; N]>,
}
impl<const N: usize> WeightImage<N> {
    fn from_paint(size: [usize; 2], paint: &[[f32; N]]) -> Self {
        let [sw, sh] = size;
        let w = sw.min(256);
        let h = sh.min(256);
        let sx = sw as f32 / w as f32;
        let sy = sh as f32 / h as f32;
        let mut pixels = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                let left = x as f32 * sx;
                let right = (x + 1) as f32 * sx;
                let top = y as f32 * sy;
                let bottom = (y + 1) as f32 * sy;
                let mut sum = [0.; N];
                let mut area = 0.;
                for row in top.floor() as usize..(bottom.ceil() as usize).min(sh) {
                    for col in left.floor() as usize..(right.ceil() as usize).min(sw) {
                        let a = (right.min(col as f32 + 1.) - left.max(col as f32)).max(0.)
                            * (bottom.min(row as f32 + 1.) - top.max(row as f32)).max(0.);
                        for (c, v) in sum.iter_mut().enumerate() {
                            *v += paint[row * sw + col][c] * a;
                        }
                        area += a;
                    }
                }
                let values = sum.map(|v| (v / area * 255.).clamp(0., 255.));
                let target = (values.iter().sum::<f32>().round() as u32).min(255);
                let mut quantized = values.map(|v| v.floor() as u8);
                let mut remaining =
                    target.saturating_sub(quantized.iter().map(|v| u32::from(*v)).sum());
                let mut order: [usize; N] = std::array::from_fn(|i| i);
                order.sort_by(|&a, &b| {
                    (values[b] - f32::from(quantized[b]))
                        .total_cmp(&(values[a] - f32::from(quantized[a])))
                        .then(a.cmp(&b))
                });
                for c in order {
                    if remaining == 0 {
                        break;
                    }
                    quantized[c] += 1;
                    remaining -= 1;
                }
                pixels.push(quantized);
            }
        }
        Self {
            size: [w, h],
            pixels,
        }
    }
    fn sample(&self, u: f32, v: f32) -> [f32; N] {
        let [w, h] = self.size;
        if w == 0 || h == 0 || w > 256 || h > 256 || w.checked_mul(h) != Some(self.pixels.len()) {
            return [0.; N];
        }
        let x = (u * w as f32 - 0.5).clamp(0., (w - 1) as f32);
        let y = (v * h as f32 - 0.5).clamp(0., (h - 1) as f32);
        let a = x.floor() as usize;
        let b = y.floor() as usize;
        let c = (a + 1).min(w - 1);
        let d = (b + 1).min(h - 1);
        std::array::from_fn(|channel| {
            let at = |x: usize, y: usize| self.pixels[y * w + x][channel] as f32 / 255.;
            let lo = at(a, b) * (1. - x.fract()) + at(c, b) * x.fract();
            let hi = at(a, d) * (1. - x.fract()) + at(c, d) * x.fract();
            lo * (1. - y.fract()) + hi * y.fract()
        })
    }
}
/// Union coverage matches independent material and biome paint. It differs from
/// the legacy RGB preview's maximum-alpha approximation when both are partial.
pub fn palette_coverage(material: &WeightImage<8>, biome: Option<&WeightImage<5>>) -> ColorImage {
    let [w, h] = material.size;
    ColorImage::new(
        material.size,
        (0..w * h)
            .map(|i| {
                let u = ((i % w) as f32 + 0.5) / w as f32;
                let v = ((i / w) as f32 + 0.5) / h as f32;
                let a = material.sample(u, v).iter().sum::<f32>().clamp(0., 1.);
                let b = biome
                    .map_or(0., |b| b.sample(u, v).iter().sum::<f32>())
                    .clamp(0., 1.);
                Color32::from_white_alpha(((a + b - a * b) * 255.).round() as u8)
            })
            .collect(),
    )
}
/// Cartographic color picking ignores biome-only coverage. Sample the retained
/// material LOD directly, without allocating another image in the pointer path.
pub fn hit_test_material_weights(
    material: &WeightImage<8>,
    geometry: Geometry,
    point: [f32; 2],
    pose: [f32; 3],
) -> bool {
    geometry
        .uv(point, pose[0], pose[1], pose[2])
        .is_some_and(|[u, v]| material.sample(u, v).iter().sum::<f32>().clamp(0., 1.) * 255. >= 16.)
}
pub fn contextual_image_with_palette(
    material: &WeightImage<8>,
    biome: Option<&WeightImage<5>>,
    geometry: Geometry,
    layers: &papyrus_terrain::native_layers::Layers,
    pose: [f32; 3],
    palette: &crate::terrain_palette::Palette,
) -> ColorImage {
    // Borrowed compatibility/diagnostic entry point. The viewport uses its
    // retained frame below and never constructs a full surface during dragging.
    let surface = if let Some(profile) = palette.surface_profile() {
        crate::generation_resources::with_cpu_pool(
            || false,
            |pool, limits| {
                let bytes = (limits.image_memory_mib * 1024 * 1024)
                    .checked_sub(material.size[0] * material.size[1] * 4)
                    .ok_or("Stamp preview exceeds its image budget")?;
                crate::map_surface_frame::generate(
                    &layers.height,
                    crate::map_surface_frame::settings(layers, bytes),
                    profile,
                    Some(pool),
                    || false,
                )
            },
        )
        .ok()
        .flatten()
    } else {
        None
    };
    contextual_image_with_surface(
        material,
        biome,
        geometry,
        layers,
        pose,
        palette,
        surface.as_ref(),
        None,
    )
}
#[allow(clippy::too_many_arguments)]
pub fn contextual_image_with_surface(
    material: &WeightImage<8>,
    biome: Option<&WeightImage<5>>,
    geometry: Geometry,
    layers: &papyrus_terrain::native_layers::Layers,
    pose: [f32; 3],
    palette: &crate::terrain_palette::Palette,
    surface: Option<&crate::map_surface::Grid>,
    height: Option<&[i16]>,
) -> ColorImage {
    if palette.has_surface_profile() && surface.and_then(|s| s.normal()).is_none() {
        return ColorImage::filled(material.size, Color32::TRANSPARENT);
    }
    contextual_image_with_water(
        material, biome, geometry, layers, pose, palette, surface, height, None,
    )
}

/// Reuse the displayed map's sea classification and registered local water for
/// temporary paint. Water-hidden splats must also remain absent from hit masks.
#[allow(clippy::too_many_arguments)]
pub fn contextual_image_with_water(
    material: &WeightImage<8>,
    biome: Option<&WeightImage<5>>,
    geometry: Geometry,
    layers: &papyrus_terrain::native_layers::Layers,
    pose: [f32; 3],
    palette: &crate::terrain_palette::Palette,
    surface: Option<&crate::map_surface::Grid>,
    height: Option<&[i16]>,
    ocean: Option<&papyrus_terrain::ocean::Ocean>,
) -> ColorImage {
    let hr = layers.height_resolution;
    let sr = layers.surface_resolution;
    if height.is_some_and(|h| h.len() != hr * hr)
        || ocean.is_some_and(|o| {
            o.resolution != sr
                || o.classification.len() != sr * sr
                || o.local_water.len() != hr * hr
        })
    {
        return ColorImage::filled(material.size, Color32::TRANSPARENT);
    }
    let [w, h] = material.size;
    let quad = geometry.quad(pose[0], pose[1], pose[2]);
    let mut pixels = Vec::with_capacity(w * h);
    for row in 0..h {
        for col in 0..w {
            let u = (col as f32 + 0.5) / w as f32;
            let v = (row as f32 + 0.5) / h as f32;
            let paint = material.sample(u, v);
            let climate = biome.map_or([0.; 5], |b| b.sample(u, v));
            let a = paint.iter().sum::<f32>().clamp(0., 1.);
            let b = climate.iter().sum::<f32>().clamp(0., 1.);
            let alpha = a + b - a * b;
            if alpha <= 0. {
                pixels.push(Color32::TRANSPARENT);
                continue;
            }
            let world: [f32; 2] = std::array::from_fn(|i| {
                quad[0][i] + u * (quad[1][i] - quad[0][i]) + v * (quad[3][i] - quad[0][i])
            });
            let normalize = |values: &mut [f32]| {
                let total = values.iter().sum::<f32>();
                if total > 0. {
                    for v in values {
                        *v /= total;
                    }
                }
            };
            let mut old_material =
                crate::map_image::material_weights_at(layers, world[0], world[1]);
            normalize(&mut old_material);
            let mut old_biome = crate::map_image::biome_weights_at(layers, world[0], world[1]);
            normalize(&mut old_biome);
            let new_material = std::array::from_fn(|c| old_material[c] * (1. - a) + paint[c]);
            let new_biome = std::array::from_fn(|c| old_biome[c] * (1. - b) + climate[c]);
            let base = crate::map_image::shade_weights_surface(
                layers,
                world[0],
                world[1],
                old_material,
                old_biome,
                palette,
                surface,
                height,
                ocean,
            );
            let mixed = crate::map_image::shade_weights_surface(
                layers,
                world[0],
                world[1],
                new_material,
                new_biome,
                palette,
                surface,
                height,
                ocean,
            );
            if palette.cartography().is_some() {
                // Ordered splat lerps are nonlinear in the paint weights. An
                // inverse alpha blend can require negative/out-of-range RGB,
                // so publish the already composed pixel within paint coverage.
                if (0..3).all(|c| {
                    (mixed[c].round().clamp(0., 255.) - base[c].round().clamp(0., 255.)).abs() < 2.
                }) {
                    pixels.push(Color32::TRANSPARENT);
                } else {
                    pixels.push(Color32::from_rgb(
                        mixed[0].round().clamp(0., 255.) as u8,
                        mixed[1].round().clamp(0., 255.) as u8,
                        mixed[2].round().clamp(0., 255.) as u8,
                    ));
                }
                continue;
            }
            let rgb: [u8; 3] = std::array::from_fn(|c| {
                ((mixed[c] - base[c] * (1. - alpha)) / alpha)
                    .round()
                    .clamp(0., 255.) as u8
            });
            pixels.push(Color32::from_rgba_unmultiplied(
                rgb[0],
                rgb[1],
                rgb[2],
                (alpha * 255.).round() as u8,
            ));
        }
    }
    ColorImage::new(material.size, pixels)
}
pub struct Preview {
    pub material_weights: Option<WeightImage<8>>,
    pub biome_weights: Option<WeightImage<5>>,
    pub palette_hit: Option<ColorImage>,
    pub image: ColorImage,
    pub biome: Option<ColorImage>,
    pub geometry: Geometry,
    pub default_yaw: f32,
}
#[derive(Clone, Copy)]
pub struct Rgba<'a> {
    pub width: usize,
    pub height: usize,
    pub pixels: &'a [u8],
}
impl Rgba<'_> {
    fn valid(&self) -> bool {
        (1..=4096).contains(&self.width)
            && (1..=4096).contains(&self.height)
            && self.pixels.len() == self.width * self.height * 4
    }
    fn sample(&self, u: f32, v: f32) -> [f32; 4] {
        let x = u.clamp(0., 1.) * (self.width - 1) as f32;
        let y = v.clamp(0., 1.) * (self.height - 1) as f32;
        let a = x.floor() as usize;
        let b = y.floor() as usize;
        let c = (a + 1).min(self.width - 1);
        let d = (b + 1).min(self.height - 1);
        std::array::from_fn(|channel| {
            let at = |x, y| self.pixels[(y * self.width + x) * 4 + channel] as f32 / 255.;
            let lo = at(a, b) * (1. - x.fract()) + at(c, b) * x.fract();
            let hi = at(a, d) * (1. - x.fract()) + at(c, d) * x.fract();
            lo * (1. - y.fract()) + hi * y.fract()
        })
    }
}
/// Authored surface domain; the height blend texture is not surface opacity.
#[derive(Clone, Copy)]
pub struct Surface {
    pub radius: f32,
    pub fade: f32,
}
fn surface_uv(u: f32, v: f32, geometry: Geometry, surface: Surface) -> (f32, f32, f32) {
    let lx = (u - 0.5) * 2. * surface.radius;
    let lz = (0.5 - v) * 2. * surface.radius;
    let distance = lx.hypot(lz);
    let opacity = if surface.fade == 0. {
        if distance <= surface.radius { 1. } else { 0. }
    } else {
        ((surface.radius - distance) / surface.fade).clamp(0., 1.)
    };
    (
        lx / geometry.size[0] + 0.5,
        0.5 - lz / geometry.size[1],
        opacity,
    )
}

fn biome_preview(
    source: Rgba<'_>,
    mask: u8,
    geometry: Geometry,
    surface: Surface,
    size: [usize; 2],
    cancelled: &impl Fn() -> bool,
) -> Result<Option<ColorImage>, String> {
    if !source.valid() {
        return Err("Invalid biome preview texture".into());
    }
    let [width, height] = size;
    let mut pixels = Vec::with_capacity(width * height);
    for row in 0..height {
        if cancelled() {
            return Ok(None);
        }
        for col in 0..width {
            let (u, v, opacity) = surface_uv(
                (col as f32 + 0.5) / width as f32,
                (row as f32 + 0.5) / height as f32,
                geometry,
                surface,
            );
            let rgba = source.sample(u, v);
            let fifth = (1. - rgba.iter().sum::<f32>()).max(0.);
            let weights: [f32; 5] = std::array::from_fn(|c| {
                if mask & (1 << c) == 0 {
                    0.
                } else if c < 4 {
                    rgba[c]
                } else {
                    fifth
                }
            });
            let sum = weights.iter().sum::<f32>();
            if sum <= 0. || opacity <= 0. {
                pixels.push(Color32::TRANSPARENT);
                continue;
            }
            let rgb: [u8; 3] = std::array::from_fn(|c| {
                (weights
                    .iter()
                    .zip(crate::map_image::BIOMES)
                    .map(|(w, p)| w * p[c])
                    .sum::<f32>()
                    / sum)
                    .round()
                    .clamp(0., 255.) as u8
            });
            let total = 1. - opacity * sum.min(1.) + opacity * sum;
            let alpha = (255. * opacity * sum / total).round().clamp(0., 255.) as u8;
            pixels.push(Color32::from_rgba_unmultiplied(
                rgb[0], rgb[1], rgb[2], alpha,
            ));
        }
    }
    Ok(Some(ColorImage::new(size, pixels)))
}

fn biome_weight_preview(
    source: Rgba<'_>,
    mask: u8,
    geometry: Geometry,
    surface: Surface,
    size: [usize; 2],
    cancelled: &impl Fn() -> bool,
) -> Result<Option<WeightImage<5>>, String> {
    if !source.valid() || size.contains(&0) || size.iter().any(|v| *v > 512) {
        return Err("Invalid biome weight preview".into());
    }
    let [w, h] = size;
    let mut paint = Vec::with_capacity(w * h);
    for row in 0..h {
        if cancelled() {
            return Ok(None);
        }
        for col in 0..w {
            let (u, v, opacity) = surface_uv(
                (col as f32 + 0.5) / w as f32,
                (row as f32 + 0.5) / h as f32,
                geometry,
                surface,
            );
            let rgba = source.sample(u, v);
            let fifth = (1. - rgba.iter().sum::<f32>()).max(0.);
            let values: [f32; 5] = std::array::from_fn(|c| {
                if mask & (1 << c) == 0 {
                    0.
                } else if c < 4 {
                    rgba[c]
                } else {
                    fifth
                }
            });
            let sum = values.iter().sum::<f32>();
            let total = 1. - opacity * sum.min(1.) + opacity * sum;
            paint.push(values.map(|v| v * opacity / total));
        }
    }
    Ok(Some(WeightImage::from_paint(size, &paint)))
}

pub fn render_surface(
    first: Rgba<'_>,
    second: Rgba<'_>,
    mask: u8,
    geometry: Geometry,
    surface: Surface,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Preview>, String> {
    if !surface.radius.is_finite()
        || !(0.1..=8192.).contains(&surface.radius)
        || !surface.fade.is_finite()
        || !(0. ..=surface.radius).contains(&surface.fade)
    {
        return Err("Invalid authored surface radius/fade".into());
    }
    render_impl(
        first,
        second,
        None,
        mask,
        geometry,
        Some(surface),
        cancelled,
    )
}

pub fn render(
    first: Rgba<'_>,
    second: Rgba<'_>,
    blend: Rgba<'_>,
    mask: u8,
    geometry: Geometry,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Preview>, String> {
    render_impl(first, second, Some(blend), mask, geometry, None, cancelled)
}
fn render_impl(
    first: Rgba<'_>,
    second: Rgba<'_>,
    blend: Option<Rgba<'_>>,
    mask: u8,
    geometry: Geometry,
    surface: Option<Surface>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Preview>, String> {
    if !first.valid()
        || !second.valid()
        || blend.as_ref().is_some_and(|b| !b.valid())
        || geometry
            .size
            .iter()
            .any(|v| !v.is_finite() || !(0.1..=8192.).contains(v))
        || geometry
            .offset
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 4096.)
    {
        return Err("Unsupported splat preview dimensions or transform".into());
    }
    let width = if surface.is_some() {
        first.width.clamp(65, 512)
    } else {
        first.width.min(512)
    };
    let height = if surface.is_some() {
        first.height.clamp(65, 512)
    } else {
        first.height.min(512)
    };
    let mut pixels = Vec::with_capacity(width * height);
    let mut paint_weights = Vec::with_capacity(width * height);
    for y in 0..height {
        if cancelled() {
            return Ok(None);
        }
        for x in 0..width {
            let u = if surface.is_some() {
                (x as f32 + 0.5) / width as f32
            } else {
                x as f32 / (width - 1).max(1) as f32
            };
            let v = if surface.is_some() {
                (y as f32 + 0.5) / height as f32
            } else {
                y as f32 / (height - 1).max(1) as f32
            };
            let (tu, tv, opacity) = if let Some(surface) = surface {
                surface_uv(u, v, geometry, surface)
            } else {
                (u, v, blend.as_ref().unwrap().sample(u, v)[3])
            };
            let a = first.sample(tu, tv);
            let b = second.sample(tu, tv);
            let weights: [f32; 8] = std::array::from_fn(|i| {
                if mask & (1 << i) == 0 {
                    0.
                } else {
                    if i < 4 { a[i] } else { b[i - 4] }
                }
            });
            let sum: f32 = weights.iter().sum();
            if sum == 0. || opacity == 0. {
                pixels.push(Color32::TRANSPARENT);
                paint_weights.push([0.; 8]);
                continue;
            }
            let rgb: [u8; 3] = std::array::from_fn(|c| {
                (weights
                    .iter()
                    .zip(GROUND_PALETTE)
                    .map(|(w, p)| w * p[c])
                    .sum::<f32>()
                    / sum)
                    .round()
                    .clamp(0., 255.) as u8
            });
            // Matches normalized weight blending before byte rounding; this is a material-color preview.
            let total = 1. - opacity * sum.min(1.) + opacity * sum;
            paint_weights.push(weights.map(|w| w * opacity / total));
            let alpha = (255. * opacity * sum / total).round().clamp(0., 255.) as u8;
            pixels.push(Color32::from_rgba_unmultiplied(
                rgb[0], rgb[1], rgb[2], alpha,
            ));
        }
    }
    Ok(Some(Preview {
        material_weights: Some(WeightImage::from_paint([width, height], &paint_weights)),
        biome_weights: None,
        palette_hit: None,
        image: ColorImage::new([width, height], pixels),
        biome: None,
        geometry: surface.map_or(geometry, |surface| Geometry {
            size: [surface.radius * 2.; 2],
            offset: geometry.offset,
        }),
        default_yaw: 0.,
    }))
}
pub fn load(
    bindings: &Bindings,
    asset: &Asset,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Preview>, String> {
    let size = asset
        .modifier_size
        .ok_or("Monument has no terrain modifier size")?;
    let offset = asset
        .modifier_offset
        .ok_or("Monument has no terrain modifier offset")?;
    let mask = asset
        .splat_mask
        .ok_or("Monument has no authored splat mask")?;
    let mut channels = Vec::new();
    for name in ["splat0", "splat1"] {
        if mask == 0 {
            channels.push((1, 1, vec![0; 4]));
            continue;
        }
        if cancelled() {
            return Ok(None);
        }
        let guid = asset
            .texture_guids
            .get(name)
            .ok_or("Monument lacks a required splat/blend reference")?;
        let descriptor = bindings
            .textures()
            .get(guid)
            .ok_or("Unresolved splat/blend GUID")?;
        channels.push((
            descriptor.width,
            descriptor.height,
            bindings.read_rgba(guid)?,
        ));
    }
    let channel = |i: usize| Rgba {
        width: channels[i].0,
        height: channels[i].1,
        pixels: &channels[i].2,
    };
    let modifier = asset
        .modifier
        .as_ref()
        .ok_or("Missing authored surface radius/fade")?;
    let surface = Surface {
        radius: if modifier.radius == 0. {
            size[0] / 2.
        } else {
            modifier.radius
        },
        fade: modifier.fade,
    };
    let mut result = render_surface(
        channel(0),
        channel(1),
        mask,
        Geometry {
            size: [size[0], size[2]],
            offset: [offset[0], offset[2]],
        },
        surface,
        &cancelled,
    )?;
    if let Some(preview) = &mut result {
        if modifier.biome_mask & 31 != 0 {
            let guid = asset
                .texture_guids
                .get("biome")
                .ok_or("Missing authored biome reference")?;
            let descriptor = bindings
                .textures()
                .get(guid)
                .ok_or("Unresolved authored biome reference")?;
            let bytes = bindings.read_rgba(guid)?;
            let raw = Rgba {
                width: descriptor.width,
                height: descriptor.height,
                pixels: &bytes,
            };
            preview.biome = biome_preview(
                Rgba {
                    width: descriptor.width,
                    height: descriptor.height,
                    pixels: &bytes,
                },
                (modifier.biome_mask & 31) as u8,
                Geometry {
                    size: [size[0], size[2]],
                    offset: [offset[0], offset[2]],
                },
                surface,
                preview.image.size,
                &cancelled,
            )?;
            if preview.biome.is_none() {
                return Ok(None);
            }
            preview.biome_weights = biome_weight_preview(
                raw,
                (modifier.biome_mask & 31) as u8,
                Geometry {
                    size: [size[0], size[2]],
                    offset: [offset[0], offset[2]],
                },
                surface,
                preview.image.size,
                &cancelled,
            )?;
            if preview.biome_weights.is_none() {
                return Ok(None);
            }
        }
        if preview.image.pixels.iter().all(|p| p.a() == 0)
            && preview
                .biome
                .as_ref()
                .is_none_or(|b| b.pixels.iter().all(|p| p.a() == 0))
        {
            return Err(
                "Monument has no visible material paint in the supported splat channels".into(),
            );
        }
        if let Some(material) = &preview.material_weights {
            preview.palette_hit = Some(palette_coverage(material, preview.biome_weights.as_ref()));
        }
        preview.default_yaw = asset.yaw.map_or(0., |y| (-y[0]).rem_euclid(360.));
    }
    Ok(result)
}

#[cfg(test)]
#[path = "splat_palette_installation_tests.rs"]
mod installation_tests;

#[cfg(test)]
mod tests {
    use super::*;
    fn source_palette() -> crate::terrain_palette::Palette {
        let (installation, mut doc) = crate::terrain_palette::tests::fixture(5);
        for i in 0..8 {
            for j in 0..5 {
                doc["materials"][i]["biome_colors"][j] = serde_json::json!([
                    i as f32 * j as f32 / 32.,
                    (i + j) as f32 / 12.,
                    (8 - i + j) as f32 / 12.,
                    1.
                ]);
            }
        }
        crate::terrain_palette::Palette::decode(&serde_json::to_vec(&doc).unwrap(), &installation)
            .unwrap()
    }
    pub(super) fn palette_layers() -> papyrus_terrain::native_layers::Layers {
        let (hr, sr) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let cells = sr * sr;
        let mut layers = papyrus_terrain::native_layers::Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![18000; hr * hr],
            splat: vec![0; cells * 8],
            biome: vec![0; cells * 5],
            topology: vec![0; cells],
            normalized_splat_cells: 0,
            normalized_biome_cells: 0,
        };
        layers.splat[4 * cells..5 * cells].fill(255);
        layers.biome[cells..2 * cells].fill(255);
        layers
    }
    #[test]
    fn palette_material_biome_union_matches_full_weight_blend_and_picking_coverage() {
        let palette = source_palette();
        let layers = palette_layers();
        let mut m = [0; 8];
        m[2] = 64;
        let material = WeightImage {
            size: [1, 1],
            pixels: vec![m],
        };
        let mut b = [0; 5];
        b[3] = 102;
        let biome = WeightImage {
            size: [1, 1],
            pixels: vec![b],
        };
        let geometry = Geometry {
            size: [100., 100.],
            offset: [0., 0.],
        };
        let overlay = contextual_image_with_palette(
            &material,
            Some(&biome),
            geometry,
            &layers,
            [0., 0., 37.],
            &palette,
        );
        let coverage = palette_coverage(&material, Some(&biome));
        let pixel = overlay.pixels[0];
        assert_eq!(pixel.a(), coverage.pixels[0].a());
        assert!(pixel.a() > 102, "Coverage is the union, not the maximum");
        let a = 64. / 255.;
        let b = 102. / 255.;
        let mut mats = [0.; 8];
        mats[4] = 1. - a;
        mats[2] = a;
        let mut bios = [0.; 5];
        bios[1] = 1. - b;
        bios[3] = b;
        let expected = crate::map_image::shade_weights(&layers, 0., 0., mats, bios, &palette);
        let base = crate::map_image::shade_weights(
            &layers,
            0.,
            0.,
            [0., 0., 0., 0., 1., 0., 0., 0.],
            [0., 1., 0., 0., 0.],
            &palette,
        );
        let rgba = pixel.to_srgba_unmultiplied();
        let alpha = pixel.a() as f32 / 255.;
        for c in 0..3 {
            assert!((base[c] * (1. - alpha) + rgba[c] as f32 * alpha - expected[c]).abs() <= 2.);
        }
        assert!(hit_test(&coverage, geometry, [0., 0.], [0., 0., 37.]));
    }
    #[test]
    fn temporary_cartographic_paint_uses_displayed_water_and_final_height() {
        use papyrus_terrain::ocean;
        let (installation, mut doc) = crate::terrain_palette::tests::fixture(5);
        let mut map = crate::map_colors::tests::fixture();
        map.shader_blob_sha256 =
            Some("60b55c7b11da98c2a67f2162a29f1e1aba3ce66d698f8d017f1fe5e5f9a6b8b1".into());
        doc["cartography"] = serde_json::to_value(&map).unwrap();
        let palette = crate::terrain_palette::Palette::decode(
            &serde_json::to_vec(&doc).unwrap(),
            &installation,
        )
        .unwrap();
        let layers = palette_layers();
        let hr = layers.height_resolution;
        let sr = layers.surface_resolution;
        let encode = |h: f32| ((h + 500.) / 1000. * 32766.).round() as i16;
        let material = WeightImage {
            size: [3, 3],
            pixels: vec![[255, 0, 0, 0, 0, 0, 0, 0]; 9],
        };
        let geometry = Geometry {
            size: [200., 160.],
            offset: [10., -20.],
        };
        let before = layers.height.clone();
        // Explicit classification distinguishes enclosed depressions from sea.
        // Registered local water can be above sea level or buried below ground.
        for (elevation, class, water_level, depth_kind) in [
            (20., ocean::LAND, None, 0),
            (-1., ocean::OCEAN, None, 1),
            (-60., ocean::OCEAN, None, 1),
            (-1., ocean::ENCLOSED, None, 0),
            (10., ocean::LAND, Some(12.), 2),
            (10., ocean::LAND, Some(5.), 0),
        ] {
            let height = vec![encode(elevation); hr * hr];
            let ocean = papyrus_terrain::ocean::Ocean {
                resolution: sr,
                classification: vec![class; sr * sr],
                topology: vec![0; sr * sr],
                local_water: vec![water_level.map_or(0, encode); hr * hr],
                ocean_cells: 0,
                enclosed_cells: 0,
                changed_topology_cells: 0,
            };
            let h = papyrus_terrain::erosion::decode(height[0]);
            let depth = match depth_kind {
                1 => Some(-h),
                2 => Some(papyrus_terrain::erosion::decode(ocean.local_water[0]) - h),
                _ => None,
            };
            let shade = |weights| {
                depth.map_or_else(
                    || map.land(map.ground(weights), [0., 1., 0.]),
                    |d| map.water(weights, d),
                )
            };
            let old = shade([0., 0., 0., 0., 1., 0., 0., 0.]);
            let expected = shade([1., 0., 0., 0., 0., 0., 0., 0.]);
            let visible = (0..3).any(|c| {
                (expected[c].round().clamp(0., 255.) - old[c].round().clamp(0., 255.)).abs() >= 2.
            });
            for yaw in [0., 37., 123., 271.] {
                let pose = [80., -70., yaw];
                let overlay = contextual_image_with_water(
                    &material,
                    None,
                    geometry,
                    &layers,
                    pose,
                    &palette,
                    None,
                    Some(&height),
                    Some(&ocean),
                );
                let pixel = if visible {
                    Color32::from_rgb(
                        expected[0].round().clamp(0., 255.) as u8,
                        expected[1].round().clamp(0., 255.) as u8,
                        expected[2].round().clamp(0., 255.) as u8,
                    )
                } else {
                    Color32::TRANSPARENT
                };
                assert!(
                    overlay.pixels.iter().all(|p| *p == pixel),
                    "Water paint differs for height {elevation}, class {class}, yaw {yaw}"
                );
                let mask = overlay.pixels.iter().map(|p| p.a()).collect::<Vec<_>>();
                let quad = geometry.quad(pose[0], pose[1], pose[2]);
                let center = std::array::from_fn(|c| (quad[0][c] + quad[2][c]) * 0.5);
                assert_eq!(
                    hit_test_mask(overlay.size, &mask, geometry, center, pose),
                    visible
                );
            }
            let bad = contextual_image_with_water(
                &material,
                None,
                geometry,
                &layers,
                [0.; 3],
                &palette,
                None,
                Some(&height[..height.len() - 1]),
                Some(&ocean),
            );
            assert!(bad.pixels.iter().all(|p| p.a() == 0));
        }
        assert_eq!(layers.height, before);
    }

    #[test]
    fn cartographic_faded_stamps_publish_exact_composed_colors_without_inverse_alpha_clipping() {
        let (installation, mut doc) = crate::terrain_palette::tests::fixture(5);
        doc["cartography"] = serde_json::to_value(crate::map_colors::tests::fixture()).unwrap();
        let palette = crate::terrain_palette::Palette::decode(
            &serde_json::to_vec(&doc).unwrap(),
            &installation,
        )
        .unwrap();
        let layers = palette_layers();
        for channel in 0..8 {
            for byte in [1, 32, 128, 255] {
                let mut paint = [0; 8];
                paint[channel] = byte;
                let material = WeightImage {
                    size: [1, 1],
                    pixels: vec![paint],
                };
                let overlay = contextual_image_with_palette(
                    &material,
                    None,
                    Geometry {
                        size: [100., 100.],
                        offset: [0., 0.],
                    },
                    &layers,
                    [0., 0., 37.],
                    &palette,
                );
                let a = byte as f32 / 255.;
                let mut weights = [0.; 8];
                weights[4] = 1. - a;
                weights[channel] += a;
                let expected = crate::map_image::shade_weights(
                    &layers,
                    0.,
                    0.,
                    weights,
                    [0., 1., 0., 0., 0.],
                    &palette,
                );
                let base = crate::map_image::shade_weights(
                    &layers,
                    0.,
                    0.,
                    [0., 0., 0., 0., 1., 0., 0., 0.],
                    [0., 1., 0., 0., 0.],
                    &palette,
                );
                if (0..3).all(|c| {
                    (expected[c].round().clamp(0., 255.) - base[c].round().clamp(0., 255.)).abs()
                        < 2.
                }) {
                    assert_eq!(overlay.pixels[0], Color32::TRANSPARENT);
                    continue;
                }
                assert_eq!(overlay.pixels[0].a(), 255);
                for (actual, value) in overlay.pixels[0].to_array()[..3].iter().zip(expected) {
                    assert_eq!(*actual, value.round().clamp(0., 255.) as u8);
                }
            }
        }
    }
    #[test]
    fn source_palette_drag_matches_composed_material_and_biome_paint_at_arbitrary_yaw() {
        use papyrus_terrain::monument_composition as composition;
        let palette = source_palette();
        let mut layers = palette_layers();
        let sr = layers.surface_resolution;
        let cells = sr * sr;
        for z in 0..sr {
            for x in sr / 2..sr {
                layers.biome[cells + z * sr + x] = 0;
                layers.biome[3 * cells + z * sr + x] = 255;
            }
        }
        let first = [128, 0, 64, 0];
        let second = [0; 4];
        let bio = [64, 128, 0, 0];
        fn rgba(pixels: &[u8; 4]) -> Rgba<'_> {
            Rgba {
                width: 1,
                height: 1,
                pixels,
            }
        }
        fn image(pixels: &[u8; 4]) -> composition::Image<'_> {
            composition::Image {
                width: 1,
                height: 1,
                rgba: pixels,
            }
        }
        let geometry = Geometry {
            size: [200., 200.],
            offset: [10., -20.],
        };
        let surface = Surface {
            radius: 100.,
            fade: 20.,
        };
        let paint = render_surface(rgba(&first), rgba(&second), 255, geometry, surface, || {
            false
        })
        .unwrap()
        .unwrap();
        let weights = paint.material_weights.as_ref().unwrap();
        let biome =
            biome_weight_preview(rgba(&bio), 31, geometry, surface, paint.image.size, &|| {
                false
            })
            .unwrap()
            .unwrap();
        let coverage = palette_coverage(weights, Some(&biome));
        for yaw in [0., 37., 123., 271.] {
            let pose = [0., 0., yaw];
            let overlay = contextual_image_with_palette(
                weights,
                Some(&biome),
                geometry,
                &layers,
                pose,
                &palette,
            );
            let stamp = composition::Stamp {
                source_cliff_generation: false,
                source_quad_raster: false,
                source_height_blending: false,
                source_texture_sampling: false,
                position: [0., 20., 0.],
                yaw_degrees: yaw as f64,
                size: [200., 600., 200.],
                offset: [10., 0., -20.],
                radius: 100.,
                fade: 20.,
                height: None,
                blend: None,
                splat: Some([image(&first), image(&second)]),
                splat_mask: 255,
                biome: Some(image(&bio)),
                biome_mask: 31,
                alpha: None,
                topology: None,
                topology_mask: 0,
                replace_topology: false,
                auto_cliff_splat: false,
                auto_cliff_topology: false,
            };
            let committed = composition::compose(&layers, &vec![255; cells], &[stamp], || false)
                .unwrap()
                .unwrap();
            let [w, h] = overlay.size;
            let quad = geometry.quad(0., 0., yaw);
            let mut partial = 0;
            for (i, pixel) in overlay.pixels.iter().enumerate() {
                assert_eq!(pixel.a(), coverage.pixels[i].a());
                let u = ((i % w) as f32 + 0.5) / w as f32;
                let v = ((i / w) as f32 + 0.5) / h as f32;
                let world: [f32; 2] = std::array::from_fn(|c| {
                    quad[0][c] + u * (quad[1][c] - quad[0][c]) + v * (quad[3][c] - quad[0][c])
                });
                if pixel.a() < 40 || world[0].abs() < 5. {
                    continue;
                }
                if pixel.a() < 230 {
                    partial += 1;
                }
                let base = crate::map_image::shade_weights(
                    &layers,
                    world[0],
                    world[1],
                    crate::map_image::material_weights_at(&layers, world[0], world[1]),
                    crate::map_image::biome_weights_at(&layers, world[0], world[1]),
                    &palette,
                );
                let expected = crate::map_image::shade_weights(
                    &committed.layers,
                    world[0],
                    world[1],
                    crate::map_image::material_weights_at(&committed.layers, world[0], world[1]),
                    crate::map_image::biome_weights_at(&committed.layers, world[0], world[1]),
                    &palette,
                );
                let rgba = pixel.to_srgba_unmultiplied();
                let a = pixel.a() as f32 / 255.;
                for c in 0..3 {
                    assert!(
                        (base[c] * (1. - a) + rgba[c] as f32 * a - expected[c]).abs() <= 5.,
                        "yaw={yaw} pixel={i} channel={c}"
                    );
                }
            }
            assert!(
                partial > 20,
                "Fade must exercise partial material/biome union coverage"
            );
        }
    }
    #[test]
    fn source_weight_reduction_keeps_thin_paint_and_stays_bounded() {
        let mut paint = vec![[0.; 8]; 512 * 2];
        paint[1][2] = 1.;
        paint[513][2] = 1.;
        let image = WeightImage::from_paint([512, 2], &paint);
        assert_eq!(image.size, [256, 2]);
        assert_eq!(image.pixels[0][2], 128);
        assert_eq!(image.pixels[256][2], 128);
        assert!(image.pixels[1].iter().all(|v| *v == 0));
        assert!(
            image
                .pixels
                .iter()
                .all(|p| p.iter().map(|v| u32::from(*v)).sum::<u32>() <= 255)
        );
    }

    #[test]
    fn contextual_biome_and_material_colors_match_committed_surface_weights() {
        use papyrus_terrain::{monument_composition as composition, native_layers};
        let (hr, sr) = native_layers::dimensions(1000).unwrap();
        let cells = sr * sr;
        let mut layers = native_layers::Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![((20_f32 + 500.) * 32766. / 1000.).round() as i16; hr * hr],
            biome: vec![0; 5 * cells],
            splat: vec![0; 8 * cells],
            topology: vec![0; cells],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        layers.biome[3 * cells..4 * cells].fill(255);
        layers.splat[cells..2 * cells].fill(255);
        let geometry = Geometry {
            size: [200., 200.],
            offset: [10., -20.],
        };
        let surface = Surface {
            radius: 100.,
            fade: 20.,
        };
        let first = [128, 0, 64, 0];
        let second = [0; 4];
        fn rgba(pixels: &[u8]) -> Rgba<'_> {
            Rgba {
                width: 1,
                height: 1,
                pixels,
            }
        }
        fn image(pixels: &[u8]) -> composition::Image<'_> {
            composition::Image {
                width: 1,
                height: 1,
                rgba: pixels,
            }
        }
        for (material_mask, biome_mask, biome) in [
            (255, 31, [64, 128, 0, 0]),
            (0, 16, [0; 4]),
            (255, 1, [64, 0, 0, 0]),
        ] {
            let material = render_surface(
                rgba(&first),
                rgba(&second),
                material_mask,
                geometry,
                surface,
                || false,
            )
            .unwrap()
            .unwrap()
            .image;
            let bio = biome_preview(
                rgba(&biome),
                biome_mask,
                geometry,
                surface,
                material.size,
                &|| false,
            )
            .unwrap()
            .unwrap();
            let coverage = contextual_coverage(&material, &bio);
            for yaw in [0_f32, 37., 123., 271.] {
                let pose = [100., -200., yaw];
                let overlay =
                    contextual_image_with_biome(&material, Some(&bio), geometry, &layers, pose);
                let stamp = composition::Stamp {
                    source_cliff_generation: false,
                    source_quad_raster: false,
                    source_height_blending: false,
                    source_texture_sampling: false,
                    position: [100., 20., -200.],
                    yaw_degrees: yaw as f64,
                    size: [200., 600., 200.],
                    offset: [10., 0., -20.],
                    radius: 100.,
                    fade: 20.,
                    height: None,
                    blend: None,
                    splat: Some([image(&first), image(&second)]),
                    splat_mask: material_mask,
                    alpha: None,
                    biome: Some(image(&biome)),
                    biome_mask,
                    topology: None,
                    topology_mask: 0,
                    replace_topology: false,
                    auto_cliff_splat: false,
                    auto_cliff_topology: false,
                };
                let committed =
                    composition::compose(&layers, &vec![255; cells], &[stamp], || false)
                        .unwrap()
                        .unwrap();
                let quad = geometry.quad(pose[0], pose[1], yaw);
                for (x, y) in [(16, 32), (32, 16), (48, 40), (58, 32), (62, 32)] {
                    let u = (x as f32 + 0.5) / 65.;
                    let v = (y as f32 + 0.5) / 65.;
                    let world: [f32; 2] = std::array::from_fn(|c| {
                        quad[0][c] + u * (quad[1][c] - quad[0][c]) + v * (quad[3][c] - quad[0][c])
                    });
                    let base = crate::map_image::shade_ground(
                        &layers,
                        world[0],
                        world[1],
                        crate::map_image::ground_at(&layers, world[0], world[1]),
                    );
                    let expected = crate::map_image::shade_ground(
                        &committed.layers,
                        world[0],
                        world[1],
                        crate::map_image::ground_at(&committed.layers, world[0], world[1]),
                    );
                    let pixel = overlay.pixels[y * 65 + x];
                    let painted = pixel.to_srgba_unmultiplied();
                    let alpha = pixel.a() as f32 / 255.;
                    for c in 0..3 {
                        let mixed = base[c] * (1. - alpha) + painted[c] as f32 * alpha;
                        assert!(
                            (mixed - expected[c]).abs() < 3.,
                            "yaw={yaw} masks={material_mask}/{biome_mask} pixel={x},{y} channel={c}: {mixed} vs {}",
                            expected[c]
                        );
                    }
                    assert_eq!(coverage.pixels[y * 65 + x].a(), pixel.a());
                }
                if material_mask == 0 {
                    assert!(material.pixels.iter().all(|p| p.a() == 0));
                    assert!(hit_test(
                        &coverage,
                        geometry,
                        [
                            quad[0][0] + 0.5 * (quad[2][0] - quad[0][0]),
                            quad[0][1] + 0.5 * (quad[2][1] - quad[0][1])
                        ],
                        pose
                    ));
                }
            }
        }
    }
    #[test]
    fn contextual_reduction_preserves_thin_paint_coverage_without_dark_fringes() {
        let mut image = ColorImage::filled([512, 2], Color32::TRANSPARENT);
        let missing = (0..512)
            .find(|x| (0..256).all(|i| ((i as f32 / 255.) * 511.).round() as usize != *x))
            .unwrap();
        for row in 0..2 {
            image.pixels[row * 512 + missing] = Color32::from_rgb(200, 100, 50);
        }
        let original = image.pixels.clone();
        let reduced = contextual_base(&image);
        assert_eq!(reduced.size, [256, 2]);
        assert_eq!(reduced.pixels[missing / 2].a(), 128);
        let rgba = reduced.pixels[missing / 2].to_srgba_unmultiplied();
        for (actual, expected) in rgba[..3].iter().zip([200_i16, 100, 50]) {
            assert!((i16::from(*actual) - expected).abs() <= 1);
        }
        let mut layers = papyrus_terrain::native_layers::Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: 2,
            surface_resolution: 2,
            height: vec![16711; 4],
            biome: vec![0; 20],
            splat: vec![0; 32],
            topology: vec![0; 4],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        layers.biome[4..8].fill(255);
        let geometry = Geometry {
            size: [200., 100.],
            offset: [0., 0.],
        };
        let preview = contextual_image(&image, geometry, &layers, [0., 0., 37.]);
        assert_eq!(preview.pixels[missing / 2].a(), 128);
        assert_eq!(
            preview,
            contextual_image(&reduced, geometry, &layers, [0., 0., 37.])
        );
        assert_eq!(image.pixels, original);
        let constant = ColorImage::filled(
            [511, 65],
            Color32::from_rgba_unmultiplied(180, 150, 100, 77),
        );
        assert!(
            contextual_base(&constant)
                .pixels
                .iter()
                .all(|p| *p == constant.pixels[0])
        );
    }
    #[test]
    fn paint_picking_ignores_transparent_margins_at_rotated_offset_poses() {
        let mut image = ColorImage::filled([5, 5], Color32::TRANSPARENT);
        image.pixels[12] = Color32::WHITE;
        let geometry = Geometry {
            size: [200., 100.],
            offset: [30., -20.],
        };
        for yaw in [0_f32, 37., 90., 123., 271.] {
            let (s, c) = yaw.to_radians().sin_cos();
            let world = |x: f32, z: f32| {
                [
                    100. + c * (x + 30.) - s * (z - 20.),
                    -200. + s * (x + 30.) + c * (z - 20.),
                ]
            };
            assert!(hit_test(
                &image,
                geometry,
                world(0., 0.),
                [100., -200., yaw]
            ));
            assert!(geometry.contains(world(80., 35.), 100., -200., yaw));
            assert!(!hit_test(
                &image,
                geometry,
                world(80., 35.),
                [100., -200., yaw]
            ));
            assert!(!hit_test(
                &image,
                geometry,
                world(110., 0.),
                [100., -200., yaw]
            ));
        }
        assert!(!hit_test(
            &ColorImage::filled([1, 1], Color32::from_white_alpha(1)),
            geometry,
            [30., -20.],
            [0., 0., 0.]
        ));
        assert!(!hit_test(&image, geometry, [f32::NAN, 0.], [0., 0., 0.]));
    }
    #[test]
    fn surface_preview_uses_radial_fade_instead_of_height_blend_and_texture_bounds() {
        let first = [255, 0, 0, 0];
        let second = [0; 4];
        let image = |pixels| Rgba {
            width: 1,
            height: 1,
            pixels,
        };
        let geometry = Geometry {
            size: [400., 100.],
            offset: [11., -17.],
        };
        let preview = render_surface(
            image(&first),
            image(&second),
            1,
            geometry,
            Surface {
                radius: 100.,
                fade: 100.,
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            preview.geometry,
            Geometry {
                size: [200., 200.],
                offset: geometry.offset
            }
        );
        let at = |x: usize, z: usize| preview.image.pixels[z * 65 + x].a();
        assert_eq!(at(32, 32), 255);
        assert!((128..=131).contains(&at(48, 32)));
        assert!((1..=8).contains(&at(64, 32)));
        assert_eq!(at(0, 0), 0);
        assert!(
            render_surface(
                image(&first),
                image(&second),
                1,
                geometry,
                Surface {
                    radius: 100.,
                    fade: 101.
                },
                || false
            )
            .is_err()
        );
    }
    #[test]
    fn transformed_paint_uses_world_biome_and_retains_authored_opacity() {
        let (hr, sr) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let cells = sr * sr;
        let mut layers = papyrus_terrain::native_layers::Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![18000; hr * hr],
            biome: vec![0; cells * 5],
            splat: vec![0; cells * 8],
            topology: vec![0; cells],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        for z in 0..sr {
            for x in 0..sr {
                layers.biome[if x < sr / 2 {
                    z * sr + x
                } else {
                    3 * cells + z * sr + x
                }] = 255;
            }
        }
        let original = layers.biome.clone();
        let paint = Color32::from_rgba_unmultiplied(132, 135, 121, 180);
        let image = ColorImage::filled([2, 2], paint);
        let geometry = Geometry {
            size: [800., 200.],
            offset: [0., 0.],
        };
        let preview = contextual_image(&image, geometry, &layers, [0., 0., 0.]);
        assert_ne!(preview.pixels[0], preview.pixels[1]);
        assert_eq!(preview.pixels[0].a(), 180);
        let rotated = contextual_image(&image, geometry, &layers, [0., 0., 180.]);
        assert_eq!(preview.pixels[0], rotated.pixels[1]);
        assert_eq!(preview.pixels[1], rotated.pixels[0]);
        assert_eq!(layers.biome, original);
        let transparent = contextual_image(
            &ColorImage::filled([2, 2], Color32::TRANSPARENT),
            geometry,
            &layers,
            [0., 0., 37.],
        );
        assert!(transparent.pixels.iter().all(|p| p.a() == 0));
    }
    #[test]
    fn footprint_picking_respects_yaw_and_offset() {
        let g = Geometry {
            size: [200., 60.],
            offset: [25., -13.],
        };
        for yaw in [0_f32, 37., 90., 123., 271.] {
            let (s, c) = yaw.to_radians().sin_cos();
            let world = |lx: f32, lz: f32| {
                [
                    100. + c * (lx + 25.) - s * (lz - 13.),
                    -200. + s * (lx + 25.) + c * (lz - 13.),
                ]
            };
            assert!(g.contains(world(95., 25.), 100., -200., yaw));
            assert!(!g.contains(world(105., 25.), 100., -200., yaw));
            assert!(!g.contains(world(0., 35.), 100., -200., yaw));
        }
    }
    #[test]
    fn mask_alpha_and_different_channel_dimensions_are_respected() {
        let first = [255, 255, 0, 0, 255, 255, 0, 0];
        let second = [0; 4];
        let blend = [0, 0, 0, 0, 0, 0, 0, 255];
        let geometry = Geometry {
            size: [80., 40.],
            offset: [10., 20.],
        };
        let result = render(
            Rgba {
                width: 2,
                height: 1,
                pixels: &first,
            },
            Rgba {
                width: 1,
                height: 1,
                pixels: &second,
            },
            Rgba {
                width: 2,
                height: 1,
                pixels: &blend,
            },
            1,
            geometry,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.image.pixels[0], Color32::TRANSPARENT);
        assert_eq!(result.image.pixels[1], Color32::from_rgb(120, 108, 78));
        let q = geometry.quad(100., 200., 90.);
        assert!((q[0][0] - 60.).abs() < 0.001 && (q[0][1] - 170.).abs() < 0.001);
    }
    #[test]
    fn downsampling_preserves_authored_metres_and_rotated_side_lengths() {
        let pixels = vec![255; 1024 * 1024 * 4];
        let channel = || Rgba {
            width: 1024,
            height: 1024,
            pixels: &pixels,
        };
        let geometry = Geometry {
            size: [800., 512.],
            offset: [17., -31.],
        };
        let preview = render(channel(), channel(), channel(), 255, geometry, || false)
            .unwrap()
            .unwrap();
        assert_eq!(preview.image.size, [512, 512]);
        for yaw in [0., 37., 90., 123., 271.] {
            let q = preview.geometry.quad(91., -42., yaw);
            let length =
                |a: [f32; 2], b: [f32; 2]| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
            assert!((length(q[0], q[1]) - 800.).abs() < 0.001);
            assert!((length(q[1], q[2]) - 512.).abs() < 0.001);
            // On a 4 km world displayed 1000 px wide, 800 m occupies 200 px.
            assert!((length(q[0], q[1]) / 4000. * 1000. - 200.).abs() < 0.001);
        }
    }
    #[test]
    fn cancelled_or_malformed_preview_never_publishes() {
        let valid = [255; 4];
        let geometry = Geometry {
            size: [20., 20.],
            offset: [0., 0.],
        };
        assert!(
            render(
                Rgba {
                    width: 1,
                    height: 1,
                    pixels: &valid
                },
                Rgba {
                    width: 1,
                    height: 1,
                    pixels: &valid
                },
                Rgba {
                    width: 1,
                    height: 1,
                    pixels: &valid
                },
                255,
                geometry,
                || true
            )
            .unwrap()
            .is_none()
        );
        assert!(
            render(
                Rgba {
                    width: 4096,
                    height: 4096,
                    pixels: &valid
                },
                Rgba {
                    width: 1,
                    height: 1,
                    pixels: &valid
                },
                Rgba {
                    width: 1,
                    height: 1,
                    pixels: &valid
                },
                255,
                geometry,
                || false
            )
            .is_err()
        );
    }
}
