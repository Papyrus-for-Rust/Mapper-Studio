//! Detailed north-up map imagery derived from retained terrain, never editor overlays.
use eframe::egui::{Color32, ColorImage};
use papyrus_terrain::{
    monument_composition::Composed,
    native_layers::{Layers, dimensions},
    ocean::Ocean,
};
use std::sync::Arc;
pub const DISPLAY_RESOLUTION: usize = 2048;
#[derive(Clone, Debug)]
pub struct Rendered {
    pub image: ColorImage,
    pub surface: Option<std::sync::Arc<crate::map_surface_frame::Frame>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Detail {
    Full,
    MemoryLimited,
    Thumbnail,
}

enum ViewportTerrain<'a> {
    Native(&'a Arc<Layers>, Option<&'a Ocean>),
    Composed(&'a Arc<Composed>),
}
struct ViewportInput<'a> {
    layers: &'a Layers,
    height: Option<&'a [i16]>,
    alpha: Option<&'a [u8]>,
    ocean: Option<&'a Ocean>,
}
impl<'a> ViewportTerrain<'a> {
    fn inputs(&self) -> ViewportInput<'a> {
        match self {
            Self::Native(layers, ocean) => ViewportInput {
                layers,
                height: None,
                alpha: None,
                ocean: *ocean,
            },
            Self::Composed(maps) => ViewportInput {
                layers: &maps.layers,
                height: maps.gameplay_height.as_ref().map(|h| h.height.as_slice()),
                alpha: Some(&maps.alpha),
                ocean: maps.ocean.as_ref(),
            },
        }
    }

    fn prepare(
        &self,
        palette: &crate::terrain_palette::Palette,
        output: usize,
        bytes: usize,
        pool: &rayon::ThreadPool,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Option<Arc<crate::map_surface_frame::Frame>>, String> {
        let ViewportInput {
            layers,
            height,
            ocean,
            ..
        } = self.inputs();
        let Some(profile) = palette
            .surface_profile()
            .filter(|_| palette.covers_layers(layers))
        else {
            return Ok(None);
        };
        let water = ocean.map(|o| o.local_water.as_slice());
        if crate::map_surface_frame::working_bytes(
            layers,
            height.unwrap_or(&layers.height),
            water,
            profile,
        )? > bytes
        {
            return Ok(None);
        }
        match self {
            Self::Native(layers, _) => {
                palette.prepare_native_surface_in(layers, water, bytes, output, pool, cancelled)
            }
            Self::Composed(maps) => {
                palette.prepare_composed_surface_in(maps, bytes, output, pool, cancelled)
            }
        }
    }
}

pub fn render_native_viewport(
    layers: &Arc<Layers>,
    ocean: Option<&Ocean>,
    palette: Option<&crate::terrain_palette::Palette>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Rendered>, String> {
    render_viewport(ViewportTerrain::Native(layers, ocean), palette, cancelled)
}

pub fn render_composed_viewport(
    maps: &Arc<Composed>,
    palette: Option<&crate::terrain_palette::Palette>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Rendered>, String> {
    render_viewport(ViewportTerrain::Composed(maps), palette, cancelled)
}

pub fn prepare_native_viewport_surface(
    layers: &Arc<Layers>,
    ocean: Option<&Ocean>,
    output: usize,
    reserved: usize,
    palette: Option<&crate::terrain_palette::Palette>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Arc<crate::map_surface_frame::Frame>>, String> {
    prepare_viewport_surface(
        ViewportTerrain::Native(layers, ocean),
        output,
        reserved,
        palette,
        cancelled,
    )
}

pub fn prepare_composed_viewport_surface(
    maps: &Arc<Composed>,
    output: usize,
    reserved: usize,
    palette: Option<&crate::terrain_palette::Palette>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Arc<crate::map_surface_frame::Frame>>, String> {
    prepare_viewport_surface(
        ViewportTerrain::Composed(maps),
        output,
        reserved,
        palette,
        cancelled,
    )
}

fn prepare_viewport_surface(
    terrain: ViewportTerrain<'_>,
    output: usize,
    reserved: usize,
    palette: Option<&crate::terrain_palette::Palette>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Arc<crate::map_surface_frame::Frame>>, String> {
    let Some(palette) = palette.filter(|p| p.has_surface_profile()) else {
        return Ok(None);
    };
    crate::generation_resources::with_cpu_pool(&cancelled, |pool, limits| {
        let bytes = (limits.image_memory_mib * 1024 * 1024).saturating_sub(reserved);
        terrain.prepare(palette, output, bytes, pool, &cancelled)
    })
}

fn render_viewport(
    terrain: ViewportTerrain<'_>,
    palette: Option<&crate::terrain_palette::Palette>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Rendered>, String> {
    crate::generation_resources::with_cpu_pool(&cancelled, |pool, limits| {
        render_viewport_in(
            terrain,
            DISPLAY_RESOLUTION,
            palette,
            pool,
            limits,
            &cancelled,
        )
    })
}

fn render_viewport_in(
    terrain: ViewportTerrain<'_>,
    requested: usize,
    palette: Option<&crate::terrain_palette::Palette>,
    pool: &rayon::ThreadPool,
    limits: crate::generation_resources::Limits,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Rendered>, String> {
    let ViewportInput {
        layers,
        height,
        alpha,
        ocean,
    } = terrain.inputs();
    let detailed = palette.filter(|p| p.has_surface_profile() && p.covers_layers(layers));
    let budget = limits.image_memory_mib * 1024 * 1024;
    // Geometry scratch is released before the pixel buffer is allocated.
    let mut surface = match detailed {
        Some(palette) => terrain.prepare(palette, requested, budget, pool, cancelled)?,
        None => None,
    };
    if cancelled() {
        return Ok(None);
    }
    let pixel_bytes = std::mem::size_of::<Color32>();
    if surface
        .as_ref()
        .is_some_and(|f| f.retained_bytes() > budget.saturating_sub(4 * pixel_bytes))
    {
        surface = None;
    }
    let simplified = detailed.is_some() && surface.is_none();
    let available = budget - surface.as_ref().map_or(0, |f| f.retained_bytes());
    let mut output = requested.min((available / pixel_bytes).isqrt());
    if simplified {
        output = output.min(DISPLAY_RESOLUTION / 2);
    }
    let surface = surface.zip(detailed).map(|(frame, palette)| {
        if output == requested {
            frame
        } else {
            palette.surface_for_output(frame, output)
        }
    });
    let grid = surface.as_ref().and_then(|frame| match terrain {
        ViewportTerrain::Native(layers, _) => frame.native(layers),
        ViewportTerrain::Composed(maps) => frame.composed(maps),
    });
    let image = render_in(
        layers,
        height,
        alpha,
        ocean.map(|o| (o.resolution, o.classification.as_slice())),
        ocean.map(|o| o.local_water.as_slice()),
        output,
        palette,
        grid,
        if simplified {
            Detail::MemoryLimited
        } else {
            Detail::Full
        },
        pool,
        limits,
        0,
        cancelled,
    )?;
    Ok(image.map(|image| Rendered { image, surface }))
}

/// The small intermediate image is used during generation. Detailed shading is
/// constructed only for the map the user will inspect or export.
pub(crate) fn render_terrain_thumbnail(
    layers: &Layers,
    alpha: Option<&[u8]>,
    local_water: Option<&[i16]>,
    palette: Option<&crate::terrain_palette::Palette>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<ColorImage>, String> {
    crate::generation_resources::with_cpu_pool(&cancelled, |pool, limits| {
        render_thumbnail_in(layers, alpha, local_water, palette, pool, limits, &cancelled)
    })
}

/// A generation thumbnail can be skipped while the shared image pool is busy.
pub fn render_composition_thumbnail(
    maps: &papyrus_terrain::monument_composition::Composed,
    palette: Option<&crate::terrain_palette::Palette>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<ColorImage>, String> {
    if cancelled() {
        return Ok(None);
    }
    let water = maps.ocean.as_ref().map(|water| water.local_water.as_slice());
    crate::generation_resources::try_with_cpu_pool(|pool| {
        render_thumbnail_in(
            &maps.layers, Some(&maps.alpha), water, palette, pool,
            crate::generation_resources::current(), &cancelled,
        )
    }).map(Option::flatten)
}

fn render_thumbnail_in(
    layers: &Layers,
    alpha: Option<&[u8]>,
    local_water: Option<&[i16]>,
    palette: Option<&crate::terrain_palette::Palette>,
    pool: &rayon::ThreadPool,
    limits: crate::generation_resources::Limits,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<ColorImage>, String> {
    render_in(
        layers, None, alpha, None, local_water, crate::native_preview::RESOLUTION,
        palette, None, Detail::Thumbnail, pool, limits, 0, cancelled,
    )
}
/// Prepare geometry on an image worker; accepted views retain the returned
/// frame. Presentation-only color changes share it without rebuilding heights.
pub fn prepare_native_surface(
    layers: &std::sync::Arc<Layers>,
    output: usize,
    palette: Option<&crate::terrain_palette::Palette>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<std::sync::Arc<crate::map_surface_frame::Frame>>, String> {
    prepare_native_surface_with_water(layers, None, output, palette, cancelled)
}
pub fn prepare_native_surface_with_water(
    layers: &std::sync::Arc<Layers>,
    ocean: Option<&Ocean>,
    output: usize,
    palette: Option<&crate::terrain_palette::Palette>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<std::sync::Arc<crate::map_surface_frame::Frame>>, String> {
    let Some(palette) = palette.filter(|p| p.has_surface_profile() && p.covers_layers(layers))
    else {
        return Ok(None);
    };
    crate::generation_resources::with_cpu_pool(&cancelled, |pool, limits| {
        let bytes = surface_budget(output, limits)?;
        palette.prepare_native_surface_in(
            layers,
            ocean.map(|o| o.local_water.as_slice()),
            bytes,
            output,
            pool,
            &cancelled,
        )
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_retained(
    layers: &Layers,
    alpha: &[u8],
    classification: Option<(usize, &[u8])>,
    water: Option<&[i16]>,
    output: usize,
    palette: Option<&crate::terrain_palette::Palette>,
    reserved: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<ColorImage>, String> {
    crate::generation_resources::with_cpu_pool(&cancelled, |pool, limits| {
        render_in(
            layers, None, Some(alpha), classification, water, output, palette, None,
            Detail::Full, pool, limits, reserved, &cancelled,
        )
    })
}
fn surface_budget(
    output: usize,
    limits: crate::generation_resources::Limits,
) -> Result<usize, String> {
    if !(2..=4096).contains(&output) {
        return Err("Invalid detailed image resolution".into());
    }
    (limits.image_memory_mib * 1024 * 1024)
        .checked_sub(output * output * 4)
        .ok_or_else(|| "Map image exceeds the image working memory limit in Settings".into())
}
pub fn render_native_owned(
    layers: &std::sync::Arc<Layers>,
    alpha: Option<&[u8]>,
    ocean: Option<&Ocean>,
    output: usize,
    palette: Option<&crate::terrain_palette::Palette>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Rendered>, String> {
    let surface = prepare_native_surface_with_water(layers, ocean, output, palette, &cancelled)?;
    if cancelled() {
        return Ok(None);
    }
    let image = render_impl(
        layers,
        None,
        alpha,
        ocean.map(|o| (o.resolution, o.classification.as_slice())),
        ocean.map(|o| o.local_water.as_slice()),
        output,
        palette,
        surface.as_ref().and_then(|f| f.native(layers)),
        &cancelled,
    )?;
    Ok(image.map(|image| Rendered { image, surface }))
}
pub fn prepare_composed_surface(
    maps: &std::sync::Arc<papyrus_terrain::monument_composition::Composed>,
    output: usize,
    palette: Option<&crate::terrain_palette::Palette>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<std::sync::Arc<crate::map_surface_frame::Frame>>, String> {
    Ok(
        if let Some(palette) =
            palette.filter(|p| p.has_surface_profile() && p.covers_layers(&maps.layers))
        {
            crate::generation_resources::with_cpu_pool(&cancelled, |pool, limits| {
                palette.prepare_composed_surface_in(
                    maps,
                    surface_budget(output, limits)?,
                    output,
                    pool,
                    &cancelled,
                )
            })?
        } else {
            None
        },
    )
}
pub fn render_composed_owned(
    maps: &std::sync::Arc<papyrus_terrain::monument_composition::Composed>,
    output: usize,
    palette: Option<&crate::terrain_palette::Palette>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Rendered>, String> {
    let surface = prepare_composed_surface(maps, output, palette, &cancelled)?;
    if cancelled() {
        return Ok(None);
    }
    let image = render_impl(
        &maps.layers,
        maps.gameplay_height.as_ref().map(|h| h.height.as_slice()),
        Some(&maps.alpha),
        maps.ocean
            .as_ref()
            .map(|o| (o.resolution, o.classification.as_slice())),
        maps.ocean.as_ref().map(|o| o.local_water.as_slice()),
        output,
        palette,
        surface.as_ref().and_then(|f| f.composed(maps)),
        &cancelled,
    )?;
    Ok(image.map(|image| Rendered { image, surface }))
}
pub fn sample_native_surface(
    layers: &std::sync::Arc<Layers>,
    alpha: Option<&[u8]>,
    ocean: Option<&Ocean>,
    point: [f32; 2],
    palette: &crate::terrain_palette::Palette,
    surface: Option<&crate::map_surface_frame::Frame>,
) -> Option<Color32> {
    sample_cartographic_height(
        layers,
        &layers.height,
        alpha,
        ocean,
        point,
        palette,
        surface.and_then(|f| f.native(layers)),
    )
}
pub fn sample_composed_surface(
    maps: &std::sync::Arc<papyrus_terrain::monument_composition::Composed>,
    point: [f32; 2],
    palette: &crate::terrain_palette::Palette,
    surface: Option<&crate::map_surface_frame::Frame>,
) -> Option<Color32> {
    sample_cartographic_height(
        &maps.layers,
        maps.gameplay_height
            .as_ref()
            .map_or(maps.layers.height.as_slice(), |h| &h.height),
        Some(&maps.alpha),
        maps.ocean.as_ref(),
        point,
        palette,
        surface.and_then(|f| f.composed(maps)),
    )
}
/// Bounded point query for selecting already composed cartographic paint.
/// Uses the same cartographic material, water and lighting rules as the overview.
/// Like the source MapImageRenderer, map colors do not mask terrain-alpha holes.
/// Bound filtered-surface palettes require `sample_native_surface` with the
/// displayed frame; this legacy query returns None rather than guessing slope.
pub fn sample_cartographic(
    layers: &Layers,
    alpha: Option<&[u8]>,
    ocean: Option<&Ocean>,
    point: [f32; 2],
    palette: &crate::terrain_palette::Palette,
) -> Option<Color32> {
    sample_cartographic_height(layers, &layers.height, alpha, ocean, point, palette, None)
}
/// Point picking uses the same accepted surface heights as the composed image.
/// Use `sample_composed_surface` for a bound filtered-surface palette.
pub fn sample_composed_cartographic(
    maps: &papyrus_terrain::monument_composition::Composed,
    point: [f32; 2],
    palette: &crate::terrain_palette::Palette,
) -> Option<Color32> {
    let height = maps
        .gameplay_height
        .as_ref()
        .map_or(maps.layers.height.as_slice(), |h| &h.height);
    sample_cartographic_height(
        &maps.layers,
        height,
        Some(&maps.alpha),
        maps.ocean.as_ref(),
        point,
        palette,
        None,
    )
}
fn sample_cartographic_height(
    layers: &Layers,
    height: &[i16],
    alpha: Option<&[u8]>,
    ocean: Option<&Ocean>,
    point: [f32; 2],
    palette: &crate::terrain_palette::Palette,
    surface: Option<&crate::map_surface::Grid>,
) -> Option<Color32> {
    let map = palette.cartography()?;
    let hr = layers.height_resolution;
    let sr = layers.surface_resolution;
    let cells = sr.checked_mul(sr)?;
    if hr < 2
        || sr < 1
        || hr.checked_mul(hr)? != layers.height.len()
        || height.len() != layers.height.len()
        || cells.checked_mul(8)? != layers.splat.len()
        || cells.checked_mul(5)? != layers.biome.len()
        || alpha.is_some_and(|a| a.len() != cells)
        || ocean.is_some_and(|o| {
            o.resolution != sr || o.classification.len() != cells || o.local_water.len() != hr * hr
        })
    {
        return None;
    }
    let [u, v] = point.map(|x| x / layers.size as f32 + 0.5);
    if !u.is_finite() || !v.is_finite() || !(0. ..=1.).contains(&u) || !(0. ..=1.).contains(&v) {
        return None;
    }
    if palette.has_surface_profile() && surface.and_then(|s| s.normal()).is_none() {
        return None;
    }
    let slope = map.surface_slope(surface, [u, v]);
    let elevation = map.surface_height(surface, [u, v], || Sample::at(u, v, hr).height(height));
    let sample = Sample::surface(u, v, sr);
    let weights = sample.channels(&layers.splat, cells);
    let depth = water_depth(
        [u, v],
        hr,
        sr,
        elevation,
        ocean.map(|o| o.classification.as_slice()),
        ocean.map(|o| o.local_water.as_slice()),
        surface,
        layers.size,
    );
    let rgb = if let Some(depth) = depth {
        map.water_with_slope(weights, depth, slope)
    } else {
        shade_weights_with_height(
            layers,
            height,
            point[0],
            point[1],
            weights,
            || sample.channels(&layers.biome, cells),
            palette,
            slope,
            surface.and_then(|s| s.normal()),
        )
    };
    Some(Color32::from_rgb(
        rgb[0].round().clamp(0., 255.) as u8,
        rgb[1].round().clamp(0., 255.) as u8,
        rgb[2].round().clamp(0., 255.) as u8,
    ))
}
pub(crate) const BIOMES: [[f32; 3]; 5] = [
    [185., 164., 112.],
    [110., 141., 85.],
    [147., 151., 129.],
    [224., 233., 229.],
    [49., 101., 60.],
];

/// Shared by overview, point picking and temporary stamp paint. Inputs must
/// belong to the same terrain revision and have validated grid dimensions.
#[allow(clippy::too_many_arguments)]
pub(crate) fn water_depth(
    uv: [f32; 2],
    height_resolution: usize,
    surface_resolution: usize,
    elevation: f32,
    ocean: Option<&[u8]>,
    local_water: Option<&[i16]>,
    surface: Option<&crate::map_surface::Grid>,
    size: u32,
) -> Option<f32> {
    if let Some(shore) = surface.and_then(|s| s.shore()) {
        let sampled = shore.sample(uv)?;
        let distance = sampled[2] / (shore.resolution() as f32 / size as f32);
        if distance > 0. {
            let mut depth = -elevation;
            if depth <= 0. || sampled[3] != 1. {
                depth = depth.max(0.1 * distance);
            }
            return (depth > 0.).then_some(depth);
        }
        return None;
    }
    let index = |n: usize| {
        let [x, z] = uv.map(|v| (v * n as f32).clamp(0., (n - 1) as f32) as usize);
        z * n + x
    };
    if ocean.map_or(elevation <= 0., |o| {
        o[index(surface_resolution)] == papyrus_terrain::ocean::OCEAN
    }) {
        return Some(-elevation);
    }
    local_water.and_then(|water| {
        let encoded = water[index(height_resolution)];
        let level = papyrus_terrain::erosion::decode(encoded);
        (encoded != 0 && level > elevation).then_some(level - elevation)
    })
}
struct Sample {
    indices: [usize; 4],
    weights: [f32; 4],
}
impl Sample {
    fn channels<const N: usize>(&self, values: &[u8], cells: usize) -> [f32; N] {
        std::array::from_fn(|c| {
            self.indices
                .iter()
                .zip(self.weights)
                .map(|(&i, w)| values[c * cells + i] as f32 * w / 255.)
                .sum()
        })
    }
    fn at(u: f32, v: f32, n: usize) -> Self {
        Self::grid(u * (n - 1) as f32, v * (n - 1) as f32, n)
    }
    fn surface(u: f32, v: f32, n: usize) -> Self {
        Self::grid(
            (u * n as f32 - 0.5).clamp(0., (n - 1) as f32),
            (v * n as f32 - 0.5).clamp(0., (n - 1) as f32),
            n,
        )
    }
    fn grid(x: f32, z: f32, n: usize) -> Self {
        let a = x.floor() as usize;
        let b = z.floor() as usize;
        let tx = x - a as f32;
        let tz = z - b as f32;
        Self {
            indices: [
                b * n + a,
                b * n + (a + 1).min(n - 1),
                (b + 1).min(n - 1) * n + a,
                (b + 1).min(n - 1) * n + (a + 1).min(n - 1),
            ],
            weights: [
                (1. - tx) * (1. - tz),
                tx * (1. - tz),
                (1. - tx) * tz,
                tx * tz,
            ],
        }
    }
    fn height(&self, values: &[i16]) -> f32 {
        self.indices
            .iter()
            .zip(self.weights)
            .map(|(&i, w)| values[i] as f32 * w)
            .sum::<f32>()
            / 32766.
            * 1000.
            - 500.
    }
    fn color(&self, values: &[u8], cells: usize, palette: &[[f32; 3]]) -> [f32; 3] {
        let mut rgb = [0.; 3];
        let mut total = 0.;
        for (channel, color) in palette.iter().enumerate() {
            let value = self
                .indices
                .iter()
                .zip(self.weights)
                .map(|(&i, w)| values[channel * cells + i] as f32 * w)
                .sum::<f32>();
            total += value;
            for c in 0..3 {
                rgb[c] += value * color[c];
            }
        }
        if total > 0. {
            rgb.map(|v| v / total)
        } else {
            [114., 129., 79.]
        }
    }
}
/// Editor paint previews share the overview's biome tint and terrain lighting.
/// Topology is a gameplay mask, not an RGB layer. This is cartographic shading,
/// not a reproduction of Unity's terrain shader.
pub fn shade_ground(layers: &Layers, x: f32, z: f32, ground: [f32; 3]) -> [f32; 3] {
    shade_ground_with_biome(layers, x, z, ground, biome_at(layers, x, z))
}
pub(crate) fn ground_at(layers: &Layers, x: f32, z: f32) -> [f32; 3] {
    sample_at(layers, x, z).color(
        &layers.splat,
        layers.surface_resolution.pow(2),
        &crate::native_view::GROUND_PALETTE,
    )
}
pub(crate) fn biome_at(layers: &Layers, x: f32, z: f32) -> [f32; 3] {
    sample_at(layers, x, z).color(&layers.biome, layers.surface_resolution.pow(2), &BIOMES)
}
pub(crate) fn material_weights_at(layers: &Layers, x: f32, z: f32) -> [f32; 8] {
    sample_at(layers, x, z).channels(&layers.splat, layers.surface_resolution.pow(2))
}
pub(crate) fn biome_weights_at(layers: &Layers, x: f32, z: f32) -> [f32; 5] {
    sample_at(layers, x, z).channels(&layers.biome, layers.surface_resolution.pow(2))
}
#[cfg(test)]
pub(crate) fn shade_weights(
    layers: &Layers,
    x: f32,
    z: f32,
    materials: [f32; 8],
    biomes: [f32; 5],
    palette: &crate::terrain_palette::Palette,
) -> [f32; 3] {
    shade_weights_with_height(
        layers,
        &layers.height,
        x,
        z,
        materials,
        || biomes,
        palette,
        0.,
        None,
    )
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn shade_weights_surface(
    layers: &Layers,
    x: f32,
    z: f32,
    materials: [f32; 8],
    biomes: [f32; 5],
    palette: &crate::terrain_palette::Palette,
    surface: Option<&crate::map_surface::Grid>,
    height: Option<&[i16]>,
    ocean: Option<&Ocean>,
) -> [f32; 3] {
    let uv = [x, z].map(|v| v / layers.size as f32 + 0.5);
    let slope = palette
        .cartography()
        .map_or(0., |m| m.surface_slope(surface, uv));
    let height = height.unwrap_or(&layers.height);
    if let Some(map) = palette.cartography() {
        let h = map.surface_height(surface, uv, || {
            Sample::at(uv[0], uv[1], layers.height_resolution).height(height)
        });
        if let Some(depth) = water_depth(
            uv,
            layers.height_resolution,
            layers.surface_resolution,
            h,
            ocean.map(|o| o.classification.as_slice()),
            ocean.map(|o| o.local_water.as_slice()),
            surface,
            layers.size,
        ) {
            return map.water_with_slope(materials, depth, slope);
        }
    }
    shade_weights_with_height(
        layers,
        height,
        x,
        z,
        materials,
        || biomes,
        palette,
        slope,
        surface.and_then(|s| s.normal()),
    )
}
#[allow(clippy::too_many_arguments)]
fn shade_weights_with_height(
    layers: &Layers,
    height: &[i16],
    x: f32,
    z: f32,
    materials: [f32; 8],
    biomes: impl FnOnce() -> [f32; 5],
    palette: &crate::terrain_palette::Palette,
    slope: f32,
    normal_grid: Option<&crate::map_normal::Grid>,
) -> [f32; 3] {
    let rgba = palette.evaluate_with_biomes(materials, biomes).unwrap_or([
        114. / 255.,
        129. / 255.,
        79. / 255.,
        1.,
    ]);
    if let Some(map) = palette.cartography() {
        let uv = [x, z].map(|v| (v / layers.size as f32 + 0.5).clamp(0., 1.));
        let normal = normal_grid
            .filter(|_| map.water_uses_contrast())
            .and_then(|n| n.sample(uv))
            .unwrap_or_else(|| {
                crate::map_colors::normal_sampled(
                    layers.size,
                    layers.height_resolution,
                    uv[0],
                    uv[1],
                    |u, v| Sample::at(u, v, layers.height_resolution).height(height),
                )
            });
        return map.land_with_slope(rgba, normal, slope);
    }
    let light = illumination_sampled(
        layers.size,
        layers.height_resolution,
        (x / layers.size as f32 + 0.5).clamp(0., 1.),
        (z / layers.size as f32 + 0.5).clamp(0., 1.),
        |u, v| Sample::at(u, v, layers.height_resolution).height(height),
    );
    std::array::from_fn(|c| rgba[c] * 255. * light)
}
fn sample_at(layers: &Layers, x: f32, z: f32) -> Sample {
    Sample::surface(
        (x / layers.size as f32 + 0.5).clamp(0., 1.),
        (z / layers.size as f32 + 0.5).clamp(0., 1.),
        layers.surface_resolution,
    )
}
pub(crate) fn shade_ground_with_biome(
    layers: &Layers,
    x: f32,
    z: f32,
    ground: [f32; 3],
    biome: [f32; 3],
) -> [f32; 3] {
    shade_ground_with_height(layers, &layers.height, x, z, ground, biome)
}
fn shade_ground_with_height(
    layers: &Layers,
    height: &[i16],
    x: f32,
    z: f32,
    ground: [f32; 3],
    biome: [f32; 3],
) -> [f32; 3] {
    let u = (x / layers.size as f32 + 0.5).clamp(0., 1.);
    let v = (z / layers.size as f32 + 0.5).clamp(0., 1.);
    let illumination = illumination_sampled(layers.size, layers.height_resolution, u, v, |u, v| {
        Sample::at(u, v, layers.height_resolution).height(height)
    });
    std::array::from_fn(|c| (ground[c] * 0.8 + biome[c] * 0.2) * illumination)
}
pub(crate) fn illumination_sampled(
    size: u32,
    hr: usize,
    u: f32,
    v: f32,
    height: impl Fn(f32, f32) -> f32,
) -> f32 {
    let step = 1. / (hr - 1) as f32;
    let spacing = size as f32 / (hr - 1) as f32;
    let gx = (height((u + step).min(1.), v) - height((u - step).max(0.), v)) / (2. * spacing);
    let gz = (height(u, (v + step).min(1.)) - height(u, (v - step).max(0.))) / (2. * spacing);
    (0.72 + 0.36 * (0.78 + 0.50 * gx - 0.38 * gz) / (1. + gx * gx + gz * gz).sqrt())
        .clamp(0.58, 1.10)
}
pub fn render(
    layers: &Layers,
    alpha: Option<&[u8]>,
    ocean: Option<&Ocean>,
    output: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<ColorImage>, String> {
    render_with_palette(layers, alpha, ocean, output, None, cancelled)
}
pub fn render_with_palette(
    layers: &Layers,
    alpha: Option<&[u8]>,
    ocean: Option<&Ocean>,
    output: usize,
    palette: Option<&crate::terrain_palette::Palette>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<ColorImage>, String> {
    render_with_water(
        layers,
        alpha,
        ocean,
        ocean.map(|o| o.local_water.as_slice()),
        output,
        palette,
        cancelled,
    )
}
/// Render the final surface height, including accepted road/rail collider
/// contributions. Keep the underlying terrain and registered water unchanged.
pub fn render_composed(
    maps: &papyrus_terrain::monument_composition::Composed,
    output: usize,
    palette: Option<&crate::terrain_palette::Palette>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<ColorImage>, String> {
    let height = maps.gameplay_height.as_ref().map(|h| h.height.as_slice());
    render_impl(
        &maps.layers,
        height,
        Some(&maps.alpha),
        maps.ocean
            .as_ref()
            .map(|o| (o.resolution, o.classification.as_slice())),
        maps.ocean.as_ref().map(|o| o.local_water.as_slice()),
        output,
        palette,
        None,
        cancelled,
    )
}
/// Cartographic freshwater from registered nearest water cells. Dry sentinel
/// values are never interpolated into real water heights at the bank edge.
pub fn render_with_water(
    layers: &Layers,
    alpha: Option<&[u8]>,
    ocean: Option<&Ocean>,
    local_water: Option<&[i16]>,
    output: usize,
    palette: Option<&crate::terrain_palette::Palette>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<ColorImage>, String> {
    render_impl(
        layers,
        None,
        alpha,
        ocean.map(|o| (o.resolution, o.classification.as_slice())),
        local_water,
        output,
        palette,
        None,
        cancelled,
    )
}
/// Render a verified sampled ocean mask without allocating unused topology/water arrays.
pub fn render_with_ocean_mask(
    layers: &Layers,
    alpha: Option<&[u8]>,
    mask: Option<&papyrus_terrain::ocean::Mask>,
    local_water: Option<&[i16]>,
    output: usize,
    palette: Option<&crate::terrain_palette::Palette>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<ColorImage>, String> {
    render_impl(
        layers,
        None,
        alpha,
        mask.map(|m| (m.resolution, m.classification.as_slice())),
        local_water,
        output,
        palette,
        None,
        cancelled,
    )
}
#[allow(clippy::too_many_arguments)]
fn render_impl(
    layers: &Layers,
    surface_height: Option<&[i16]>,
    alpha: Option<&[u8]>,
    ocean: Option<(usize, &[u8])>,
    local_water: Option<&[i16]>,
    output: usize,
    palette: Option<&crate::terrain_palette::Palette>,
    surface: Option<&crate::map_surface::Grid>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<ColorImage>, String> {
    crate::generation_resources::with_cpu_pool(&cancelled, |pool, limits| {
        render_in(
            layers,
            surface_height,
            alpha,
            ocean,
            local_water,
            output,
            palette,
            surface,
            Detail::Full,
            pool,
            limits,
            0,
            &cancelled,
        )
    })
}

#[allow(clippy::too_many_arguments)]
fn render_in(
    layers: &Layers,
    surface_height: Option<&[i16]>,
    alpha: Option<&[u8]>,
    ocean: Option<(usize, &[u8])>,
    local_water: Option<&[i16]>,
    output: usize,
    palette: Option<&crate::terrain_palette::Palette>,
    surface: Option<&crate::map_surface::Grid>,
    detail: Detail,
    pool: &rayon::ThreadPool,
    limits: crate::generation_resources::Limits,
    reserved: usize,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<ColorImage>, String> {
    let (hr, sr) = dimensions(layers.size)?;
    let cells = sr * sr;
    if !(2..=4096).contains(&output)
        || layers.height_resolution != hr
        || layers.surface_resolution != sr
        || layers.height.len() != hr * hr
        || surface_height.is_some_and(|h| h.len() != hr * hr)
        || layers.biome.len() != cells * 5
        || layers.splat.len() != cells * 8
        || alpha.is_some_and(|a| a.len() != cells)
        || ocean.is_some_and(|(resolution, classes)| resolution != sr || classes.len() != cells)
        || local_water.is_some_and(|w| w.len() != hr * hr)
    {
        return Err("Invalid detailed map inputs".into());
    }
    if let Some(height) = surface_height {
        for (base, raised) in layers.height.chunks(4096).zip(height.chunks(4096)) {
            if cancelled() {
                return Ok(None);
            }
            if base
                .iter()
                .zip(raised)
                .any(|(&b, &h)| !(0..=32766).contains(&b) || h < b || h > 32766)
            {
                return Err("Invalid composed map surface heights".into());
            }
        }
    }
    let height = surface_height.unwrap_or(&layers.height);
    if detail != Detail::Full && surface_height.is_none() {
        for row in height.chunks(4096) {
            if cancelled() {
                return Ok(None);
            }
            if row.iter().any(|h| !(0..=32766).contains(h)) {
                return Err("Invalid encoded map surface heights".into());
            }
        }
    }
    if let Some(water) = local_water {
        for chunk in water.chunks(4096) {
            if cancelled() {
                return Ok(None);
            }
            if chunk.iter().any(|h| !(0..=32766).contains(h)) {
                return Err("Invalid local water image heights".into());
            }
        }
    }
    let palette = palette.filter(|p| p.covers_layers(layers));
    let bytes = output * output * std::mem::size_of::<Color32>();
    let budget = (limits.image_memory_mib * 1024 * 1024).saturating_sub(reserved);
    if bytes > budget {
        return Err("Map image exceeds the image working memory limit in Settings".into());
    }
    let generated_surface = if let Some(profile) = palette.and_then(|p| p.surface_profile())
        && surface.is_none()
        && detail == Detail::Full
    {
        let settings = crate::map_surface_frame::settings(
            layers,
            budget - bytes,
        );
        let Some(mut grid) =
            crate::map_surface_frame::generate(height, settings, profile, Some(pool), cancelled)?
        else {
            return Ok(None);
        };
        if profile == crate::map_surface_frame::Profile::Current
            && crate::map_surface_frame::attach_shore(
                &mut grid,
                layers,
                height,
                local_water,
                settings.maximum_bytes,
                cancelled,
            )?
            .is_none()
        {
            return Ok(None);
        }
        Some(grid)
    } else {
        None
    };
    let sampled_surface = surface
        .or(generated_surface.as_ref())
        .map(|s| s.for_footprint(1. / output as f32));
    let surface = sampled_surface.as_ref();
    if surface.is_some_and(|g| {
        g.resolution() != (hr - 1) / 2
            || g.retained_bytes() + bytes > budget
            || (palette.is_some_and(|p| p.has_surface_profile()) && g.normal().is_none())
    }) {
        return Err("Coarse surface exceeds detailed image dimensions or memory budget".into());
    }
    let mut fallback = if detail == Detail::MemoryLimited {
        " · Simplified map preview (image memory limit)".into()
    } else {
        String::new()
    };
    if detail == Detail::Full
        && (limits.image_compute == crate::generation_resources::ImageCompute::Gpu
            || (limits.image_compute == crate::generation_resources::ImageCompute::Auto
                && output >= 1024))
    {
        if let Some(material) = palette.and_then(|p| p.cartography()) {
            match crate::map_compute::render_with_surface(
                layers,
                height,
                ocean.map(|(_, o)| o),
                local_water,
                output,
                material,
                surface,
                limits,
                cancelled,
            ) {
                Ok(image) => return Ok(image),
                Err(e) => fallback = format!(" · GPU fallback: {e}"),
            }
        } else {
            fallback = " · GPU needs a compatible source PaperMap material".into();
        }
    }
    let cartography = palette.and_then(|p| p.cartography());
    let pixel = |row: usize, x: usize| {
        let v = 1. - (row as f32 + 0.5) / output as f32;
        let u = (x as f32 + 0.5) / output as f32;
        let slope = cartography.map_or(0., |m| m.surface_slope(surface, [u, v]));
        let raw_height = || Sample::at(u, v, hr).height(height);
        let h = match cartography {
            Some(map) => map.surface_height(surface, [u, v], raw_height),
            None => raw_height(),
        };
        let sample = Sample::surface(u, v, sr);
        let i = (v * sr as f32).floor().clamp(0., (sr - 1) as f32) as usize * sr
            + (u * sr as f32).floor().clamp(0., (sr - 1) as f32) as usize;
        let depth = water_depth(
            [u, v],
            hr,
            sr,
            h,
            ocean.map(|(_, classes)| classes),
            local_water,
            surface,
            layers.size,
        );
        let rgb = if let Some(depth) = depth {
            if let Some(map) = cartography {
                map.water_with_slope(sample.channels(&layers.splat, cells), depth, slope)
            } else {
                let depth = (depth / 40.).clamp(0., 1.);
                let t = depth * (2. - depth);
                [
                    65. + (24. - 65.) * t,
                    132. + (66. - 132.) * t,
                    151. + (95. - 151.) * t,
                ]
            }
        } else if cartography.is_none() && alpha.is_some_and(|a| a[i] < 128) {
            [24., 29., 29.]
        } else if let Some(palette) = palette {
            shade_weights_with_height(
                layers,
                height,
                (u - 0.5) * layers.size as f32,
                (v - 0.5) * layers.size as f32,
                sample.channels(&layers.splat, cells),
                || sample.channels(&layers.biome, cells),
                palette,
                slope,
                surface.and_then(|s| s.normal()),
            )
        } else {
            let ground = sample.color(&layers.splat, cells, &crate::native_view::GROUND_PALETTE);
            let [x, z] = [u, v].map(|v| (v - 0.5) * layers.size as f32);
            shade_ground_with_height(layers, height, x, z, ground, biome_at(layers, x, z))
        };
        Color32::from_rgb(
            rgb[0].round().clamp(0., 255.) as u8,
            rgb[1].round().clamp(0., 255.) as u8,
            rgb[2].round().clamp(0., 255.) as u8,
        )
    };
    let mut pixels = vec![Color32::TRANSPARENT; output * output];
    use rayon::prelude::*;
    // Bounded batches preserve cancellation without requiring callers'
    // cancellation closures (including Cell-based probes) to be Sync.
    for (batch, rows) in pixels.chunks_mut(output * 32).enumerate() {
        if cancelled() {
            return Ok(None);
        }
        pool.install(|| {
            rows.par_chunks_mut(output)
                .enumerate()
                .for_each(|(row, dst)| {
                    for (x, value) in dst.iter_mut().enumerate() {
                        *value = pixel(batch * 32 + row, x);
                    }
                })
        });
    }
    if cancelled() {
        return Ok(None);
    }
    if detail != Detail::Thumbnail {
        crate::generation_resources::report(format!(
            "CPU image rendering · {} workers{fallback}",
            pool.current_num_threads()
        ));
    }
    Ok(Some(ColorImage::new([output, output], pixels)))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewport_adapts_to_image_memory_without_weakening_exact_rendering() {
        use crate::generation_resources::{ImageCompute, Limits};

        let (hr, sr) = dimensions(1000).unwrap();
        let cells = sr * sr;
        let mut terrain = Arc::new(Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![17_000; hr * hr],
            biome: vec![0; cells * 5],
            splat: vec![0; cells * 8],
            topology: vec![0; cells],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        });
        Arc::get_mut(&mut terrain).unwrap().splat[4 * cells..5 * cells].fill(255);
        let (installation, mut doc) = crate::terrain_palette::tests::fixture(5);
        let material = crate::map_colors::tests::fixture();
        doc["cartography"] = serde_json::to_value(&material).unwrap();
        let base = crate::terrain_palette::Palette::decode(
            &serde_json::to_vec(&doc).unwrap(),
            &installation,
        )
        .unwrap();
        let mut legacy = base.clone();
        legacy.test_surface_profile();
        let mut current = base.clone();
        current.test_current_surface_profile();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let limits = Limits {
            image_memory_mib: 4,
            image_compute: ImageCompute::Cpu,
            ..Limits::default()
        };
        let viewport = |layers, requested, palette, cancelled: &dyn Fn() -> bool| {
            render_viewport_in(
                ViewportTerrain::Native(layers, None),
                requested,
                Some(palette),
                &pool,
                limits,
                &cancelled,
            )
        };
        let requested = 1000;
        let preview = viewport(&terrain, requested, &legacy, &|| false)
            .unwrap()
            .unwrap();
        let grid = preview.surface.as_ref().unwrap().native(&terrain).unwrap();
        assert_eq!(
            grid.working_bytes(),
            crate::map_surface_frame::working_bytes(
                &terrain,
                &terrain.height,
                None,
                crate::map_surface_frame::Profile::Legacy,
            )
            .unwrap()
        );
        let output = preview.image.size[0];
        let budget = limits.image_memory_mib * 1024 * 1024;
        assert_eq!(output, ((budget - grid.retained_bytes()) / 4).isqrt());
        assert!(output < requested);
        let exact = render_in(
            &terrain,
            None,
            None,
            None,
            None,
            output,
            Some(&legacy),
            Some(grid),
            Detail::Full,
            &pool,
            limits,
            0,
            &|| false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(preview.image.pixels, exact.pixels);
        assert!(
            render_in(
                &terrain,
                None,
                None,
                None,
                None,
                requested,
                Some(&legacy),
                Some(grid),
                Detail::Full,
                &pool,
                limits,
                0,
                &|| false
            )
            .is_err()
        );

        let simplified = viewport(&terrain, 96, &current, &|| false)
            .unwrap()
            .unwrap();
        let expected = viewport(&terrain, 96, &base, &|| false).unwrap().unwrap();
        assert!(simplified.surface.is_none());
        assert_eq!(simplified.image.size, [96; 2]);
        assert_eq!(simplified.image.pixels, expected.image.pixels);
        let thumbnail = render_terrain_thumbnail(&terrain, None, None, Some(&current), || false)
            .unwrap()
            .unwrap();
        assert_eq!(
            thumbnail,
            render_terrain_thumbnail(&terrain, None, None, Some(&base), || false)
                .unwrap()
                .unwrap()
        );
        assert!(
            viewport(&terrain, 96, &current, &|| true)
                .unwrap()
                .is_none()
        );

        let mut invalid = terrain.as_ref().clone();
        invalid.height[0] = -1;
        assert!(
            viewport(&Arc::new(invalid), 96, &current, &|| false)
                .unwrap_err()
                .contains("Invalid encoded map surface heights")
        );
    }

    #[test]
    fn shore_upload_sampling_and_depth_match_the_owned_half_texture_shader() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/client-papermap-shore-199a470f.json"
        ))
        .unwrap();
        assert_eq!(reference["format"], "mapper-owned-shader-shore-probe");
        assert_eq!(
            reference["shader_program_sha256"],
            "ac12266cb1b50ca7c47cc37a9585a3af5109682ebc96049e838e228c67ce7aff"
        );
        assert_eq!(reference["nonfinite_land_cases"], 180);
        let material: crate::map_colors::Material =
            serde_json::from_value(reference["material"].clone()).unwrap();
        material.validate().unwrap();
        let mut settings = crate::map_surface_frame::settings_dimensions(8, 17, 1024 * 1024);
        settings.blur_pairs = 0;
        let mut grid = crate::map_surface::generate(&[16383; 17 * 17], settings, || false)
            .unwrap()
            .unwrap();
        let rows = reference["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 360);
        for (i, row) in rows.iter().enumerate() {
            let bits: [[u32; 4]; 7] = serde_json::from_value(row["texel_bits"].clone()).unwrap();
            let texels = bits.map(|v| v.map(f32::from_bits));
            let uv: [u32; 2] = serde_json::from_value(row["uv_bits"].clone()).unwrap();
            let field = papyrus_terrain::shore_field::Field {
                resolution: 8,
                distances: vec![texels[5][2]; 64],
                vectors: vec![[texels[5][0], texels[5][1], 9998., texels[5][3]]; 64],
                boundary_seeds: 0,
            };
            grid.shore = Some(std::sync::Arc::new(
                field.texture(512, || false).unwrap().unwrap(),
            ));
            let depth = water_depth(
                uv.map(f32::from_bits),
                17,
                8,
                texels[4][0],
                None,
                None,
                Some(&grid),
                8,
            );
            if !texels[5][2].is_finite() {
                assert!(depth.is_none());
            }
            let weights = std::array::from_fn(|c| texels[2 + c / 4][c % 4]);
            let actual = depth.map_or_else(
                || material.land(material.ground(weights), [0., 1., 0.]),
                |depth| material.water(weights, depth),
            );
            let rgba: [f32; 4] = serde_json::from_value(row["rgba"].clone()).unwrap();
            for (channel, (&actual, expected)) in actual.iter().zip(rgba).enumerate() {
                assert!(
                    (actual - expected * 255.).abs() < 0.0002,
                    "Shore case {i} channel {channel}: {actual} vs {}",
                    expected * 255.
                );
            }
        }
    }

    #[test]
    fn composed_overview_uses_raised_crossing_without_changing_terrain_or_water() {
        let encode = |h: f32| ((h + 500.) / 1000. * 32766.).round() as i16;
        let (hr, sr) = dimensions(1000).unwrap();
        let cells = sr * sr;
        let mut layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![encode(10.); hr * hr],
            biome: vec![0; cells * 5],
            splat: vec![0; cells * 8],
            topology: vec![0; cells],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        layers.splat[7 * cells..8 * cells].fill(255);
        let mut ocean =
            papyrus_terrain::ocean::reconstruct(&layers.height, hr, &layers.topology, sr, || false)
                .unwrap()
                .unwrap();
        ocean.local_water.fill(encode(15.));
        let terrain = layers.height.clone();
        let water = ocean.local_water.clone();
        let mut raised = terrain.clone();
        for row in raised.chunks_mut(hr) {
            row[hr / 3..2 * hr / 3].fill(encode(20.));
        }
        let mut expected_layers = layers.clone();
        expected_layers.height.clone_from(&raised);
        let mut maps = papyrus_terrain::monument_composition::Composed {
            custom_baked: Vec::new(),
            layers,
            alpha: vec![255; cells],
            ocean: Some(ocean),
            gameplay_height: Some(papyrus_terrain::gameplay_height::Output {
                height: raised,
                report: papyrus_terrain::gameplay_height::Report {
                    engine: papyrus_terrain::gameplay_height::VERSION,
                    contributors: 1,
                    changed_samples: 0,
                    work: 0,
                    physics_cooking_validated: false,
                },
            }),
            rivers: None,
            rails: None,
            objects: None,
            dungeons: None,
            tunnels: None,
            offshore: None,
            clutter_topology: None,
            cliff_topology: None,
            topology_before_overrides: None,
            topology_overrides: None,
            spawn_audit: None,
            decoration: None,
            roads: None,
            clamped_height_samples: 0,
        };
        let (installation, mut doc) = crate::terrain_palette::tests::fixture(5);
        doc["cartography"] = serde_json::to_value(crate::map_colors::tests::fixture()).unwrap();
        let palette = crate::terrain_palette::Palette::decode(
            &serde_json::to_vec(&doc).unwrap(),
            &installation,
        )
        .unwrap();
        for palette in [None, Some(&palette)] {
            let raw = render_with_palette(
                &maps.layers,
                Some(&maps.alpha),
                maps.ocean.as_ref(),
                64,
                palette,
                || false,
            )
            .unwrap()
            .unwrap();
            let image = render_composed(&maps, 64, palette, || false)
                .unwrap()
                .unwrap();
            let expected = render_with_palette(
                &expected_layers,
                Some(&maps.alpha),
                maps.ocean.as_ref(),
                64,
                palette,
                || false,
            )
            .unwrap()
            .unwrap();
            assert_eq!(image.pixels, expected.pixels);
            assert_ne!(image.pixels[32 * 64 + 32], raw.pixels[32 * 64 + 32]);
            assert_eq!(image.pixels[32 * 64 + 4], raw.pixels[32 * 64 + 4]);
            if let Some(p) = palette {
                for y in 0..64 {
                    for x in 0..64 {
                        let point = [
                            ((x as f32 + 0.5) / 64. - 0.5) * 1000.,
                            (0.5 - (y as f32 + 0.5) / 64.) * 1000.,
                        ];
                        let sampled = sample_composed_cartographic(&maps, point, p).unwrap();
                        assert!(
                            sampled
                                .to_array()
                                .into_iter()
                                .zip(image.pixels[y * 64 + x].to_array())
                                .all(|(a, b)| a.abs_diff(b) <= 1)
                        );
                    }
                }
            }
        }
        assert_eq!(maps.layers.height, terrain);
        assert_eq!(maps.ocean.as_ref().unwrap().local_water, water);
        assert!(
            render_composed(&maps, 64, Some(&palette), || true)
                .unwrap()
                .is_none()
        );
        let saved = maps.gameplay_height.as_ref().unwrap().height[0];
        for invalid in [saved - 1, -1, 32767] {
            maps.gameplay_height.as_mut().unwrap().height[0] = invalid;
            assert!(render_composed(&maps, 64, Some(&palette), || false).is_err());
        }
        maps.gameplay_height.as_mut().unwrap().height[0] = saved;
        let last = maps.gameplay_height.as_mut().unwrap().height.pop().unwrap();
        assert!(render_composed(&maps, 64, Some(&palette), || false).is_err());
        maps.gameplay_height.as_mut().unwrap().height.push(last);
        let maps = &mut Arc::new(maps);
        let mut bound = palette.clone();
        bound.test_current_surface_profile();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let limits = crate::generation_resources::Limits {
            image_memory_mib: 1,
            image_compute: crate::generation_resources::ImageCompute::Cpu,
            ..Default::default()
        };
        let simplified = render_viewport_in(
            ViewportTerrain::Composed(maps),
            64,
            Some(&bound),
            &pool,
            limits,
            &|| false,
        )
        .unwrap()
        .unwrap();
        assert!(simplified.surface.is_none());
        assert_eq!(
            simplified.image.pixels,
            render_composed(maps, 64, Some(&palette), || false)
                .unwrap()
                .unwrap()
                .pixels
        );
        let maps = Arc::get_mut(maps).unwrap();
        maps.gameplay_height = None;
        assert_eq!(
            render_composed(maps, 64, Some(&palette), || false)
                .unwrap()
                .unwrap()
                .pixels,
            render_with_palette(
                &maps.layers,
                Some(&maps.alpha),
                maps.ocean.as_ref(),
                64,
                Some(&palette),
                || false
            )
            .unwrap()
            .unwrap()
            .pixels
        );
    }
    #[test]
    fn cartographic_point_queries_match_overview_pixels_for_water_land_and_holes() {
        let encode = |h: f32| ((h + 500.) / 1000. * 32766.).round() as i16;
        let (hr, sr) = dimensions(1000).unwrap();
        let cells = sr * sr;
        let mut layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![0; hr * hr],
            biome: vec![0; cells * 5],
            splat: vec![0; cells * 8],
            topology: vec![0; cells],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        for z in 0..hr {
            for x in 0..hr {
                layers.height[z * hr + x] = encode(if x < hr / 3 {
                    -20.
                } else {
                    10. + z as f32 / hr as f32 * 20.
                });
            }
        }
        layers.biome[cells..2 * cells].fill(255);
        layers.splat[4 * cells..5 * cells].fill(255);
        let mut alpha = vec![255; cells];
        for z in sr / 2..sr {
            alpha[z * sr + sr / 2..(z + 1) * sr].fill(0);
        }
        let mut ocean =
            papyrus_terrain::ocean::reconstruct(&layers.height, hr, &layers.topology, sr, || false)
                .unwrap()
                .unwrap();
        ocean.local_water.fill(encode(15.));
        let (installation, mut doc) = crate::terrain_palette::tests::fixture(5);
        doc["cartography"] = serde_json::to_value(crate::map_colors::tests::fixture()).unwrap();
        let palette = crate::terrain_palette::Palette::decode(
            &serde_json::to_vec(&doc).unwrap(),
            &installation,
        )
        .unwrap();
        for water in [None, Some(&ocean)] {
            let image =
                render_with_palette(&layers, Some(&alpha), water, 32, Some(&palette), || false)
                    .unwrap()
                    .unwrap();
            // The server's map renderer reads height/splat/topology, not alpha.
            // Geometry holes remain in the terrain data and diagnostic layers.
            let solid = render_with_palette(&layers, None, water, 32, Some(&palette), || false)
                .unwrap()
                .unwrap();
            assert_eq!(image.pixels, solid.pixels);
            for y in 0..32 {
                for x in 0..32 {
                    let p = [
                        ((x as f32 + 0.5) / 32. - 0.5) * 1000.,
                        (0.5 - (y as f32 + 0.5) / 32.) * 1000.,
                    ];
                    let sampled =
                        sample_cartographic(&layers, Some(&alpha), water, p, &palette).unwrap();
                    let actual = image.pixels[y * 32 + x];
                    assert!(
                        sampled
                            .to_array()
                            .into_iter()
                            .zip(actual.to_array())
                            .all(|(a, b)| a.abs_diff(b) <= 1),
                        "{p:?}: {sampled:?} != {actual:?}"
                    );
                }
            }
        }
        assert!(sample_cartographic(&layers, Some(&[0]), None, [0., 0.], &palette).is_none());
        assert!(sample_cartographic(&layers, None, None, [f32::NAN, 0.], &palette).is_none());
        let mut material = crate::map_colors::tests::fixture();
        let layers = std::sync::Arc::new(layers);
        // These are synthetic producer inputs for both shader consumers.
        for shader in [
            "9ff936729f90baad098b7a69f0a3244868345f99e3a2542e00063cc004821e07",
            "60b55c7b11da98c2a67f2162a29f1e1aba3ce66d698f8d017f1fe5e5f9a6b8b1",
        ] {
            material.shader_blob_sha256 = Some(shader.into());
            doc["cartography"] = serde_json::to_value(&material).unwrap();
            let legacy = crate::terrain_palette::Palette::decode(
                &serde_json::to_vec(&doc).unwrap(),
                &installation,
            )
            .unwrap();
            let mut bound = legacy.clone();
            if shader.starts_with("60b55c7b") {
                bound.test_current_surface_profile();
            } else {
                bound.test_surface_profile();
            }

            for water in [None, Some(&ocean)] {
                let rendered =
                    render_native_owned(&layers, Some(&alpha), water, 32, Some(&bound), || false)
                        .unwrap()
                        .unwrap();
                let frame = rendered.surface.as_ref().unwrap();
                assert!(frame.native(&layers).unwrap().normal().is_some());
                let borrowed =
                    render_with_palette(&layers, Some(&alpha), water, 32, Some(&bound), || false)
                        .unwrap()
                        .unwrap();
                assert_eq!(borrowed.pixels, rendered.image.pixels);
                let old =
                    render_with_palette(&layers, Some(&alpha), water, 32, Some(&legacy), || false)
                        .unwrap()
                        .unwrap();
                assert_ne!(
                    old.pixels, rendered.image.pixels,
                    "Prepared normals must affect map lighting"
                );
                let repeated =
                    prepare_native_surface_with_water(&layers, water, 32, Some(&bound), || false)
                        .unwrap()
                        .unwrap();
                assert!(std::sync::Arc::ptr_eq(frame, &repeated));
                assert!(
                    sample_native_surface(&layers, Some(&alpha), water, [0., 0.], &bound, None)
                        .is_none()
                );
                for y in 0..32 {
                    for x in 0..32 {
                        let point = [
                            ((x as f32 + 0.5) / 32. - 0.5) * 1000.,
                            (0.5 - (y as f32 + 0.5) / 32.) * 1000.,
                        ];
                        let sampled = sample_native_surface(
                            &layers,
                            Some(&alpha),
                            water,
                            point,
                            &bound,
                            Some(frame),
                        )
                        .unwrap();
                        assert_eq!(
                            sampled,
                            rendered.image.pixels[y * 32 + x],
                            "Bound normal picking differs at {point:?}"
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn freshwater_uses_registered_heights_and_never_fills_dry_or_buried_cells() {
        let (hr, sr) = dimensions(1000).unwrap();
        let cells = sr * sr;
        let mut layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 1,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![0; hr * hr],
            biome: vec![0; 5 * cells],
            splat: vec![0; 8 * cells],
            topology: vec![1; cells],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        // Native encoded ten metres, with a single material and biome.
        layers
            .height
            .fill(((510. / 1000.) * 32766_f32 + 0.5) as i16);
        layers.splat[..cells].fill(255);
        layers.biome[cells..2 * cells].fill(255);
        let land = render(&layers, None, None, 32, || false).unwrap().unwrap();
        for encoded in [0, ((505. / 1000.) * 32766_f32 + 0.5) as i16] {
            let local = vec![encoded; hr * hr];
            let image = render_with_water(&layers, None, None, Some(&local), 32, None, || false)
                .unwrap()
                .unwrap();
            assert_eq!(image.pixels, land.pixels);
        }
        let local = vec![((515. / 1000.) * 32766_f32 + 0.5) as i16; hr * hr];
        let wet = render_with_water(&layers, None, None, Some(&local), 32, None, || false)
            .unwrap()
            .unwrap();
        assert_ne!(wet.pixels, land.pixels);
        assert!(wet.pixels.iter().all(|p| p.b() > p.r()));
        assert!(render_with_water(&layers, None, None, Some(&[-1]), 32, None, || false).is_err());
    }
    #[test]
    fn source_palette_colors_land_and_incomplete_biomes_fall_back_as_a_whole() {
        let (hr, sr) = dimensions(1000).unwrap();
        let cells = sr * sr;
        let mut layers = Layers {
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
        layers.splat[2 * cells..3 * cells].fill(255);
        layers.biome[cells..2 * cells].fill(255);
        let (installation, doc) = crate::terrain_palette::tests::fixture(4);
        let palette = crate::terrain_palette::Palette::decode(
            &serde_json::to_vec(&doc).unwrap(),
            &installation,
        )
        .unwrap();
        let image = render_with_palette(&layers, None, None, 32, Some(&palette), || false)
            .unwrap()
            .unwrap();
        let light = 0.72 + 0.36 * 0.78;
        let expected = Color32::from_rgb(
            (0.25_f32 * 255. * light).round() as u8,
            (0.2_f32 * 255. * light).round() as u8,
            (0.5_f32 * 255. * light).round() as u8,
        );
        assert_eq!(image.pixels[16 * 32 + 16], expected);
        let original = layers.clone();
        assert!(
            render_with_palette(&layers, None, None, 32, Some(&palette), || true)
                .unwrap()
                .is_none()
        );
        assert_eq!(layers.splat, original.splat);
        assert_eq!(layers.biome, original.biome);
        assert_eq!(layers.height, original.height);
        layers.biome[4 * cells] = 255;
        let legacy = render(&layers, None, None, 32, || false).unwrap().unwrap();
        let unsupported = render_with_palette(&layers, None, None, 32, Some(&palette), || false)
            .unwrap()
            .unwrap();
        assert_eq!(legacy.pixels, unsupported.pixels);
    }
    #[test]
    fn detailed_image_retains_subpreview_features_orientation_holes_and_cancellation() {
        let (hr, sr) = dimensions(2000).unwrap();
        let cells = sr * sr;
        let mut l = Layers {
            size: 2000,
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
        l.biome[cells..cells * 2].fill(255);
        l.splat[cells * 4..cells * 5].fill(255);
        // A single source material feature between old 513 px sample columns.
        let x = sr / 3 + 1;
        assert!(
            (0..513).all(|p| p * (sr - 1) / 512 != x),
            "The old preview really must skip this source column"
        );
        let z = sr - 3;
        let i = z * sr + x;
        l.splat[cells * 4 + i] = 0;
        l.splat[cells + i] = 255;
        let mut alpha = vec![255; cells];
        alpha[2 * sr + 2] = 0;
        let image = render(&l, Some(&alpha), None, sr, || false)
            .unwrap()
            .unwrap();
        assert_eq!(image.size, [sr, sr]);
        assert!(
            image.pixels[(sr - 1 - z) * sr + x].r() > image.pixels[(sr - 1 - z) * sr + x + 3].r()
        );
        // At one output pixel per surface cell, a single painted cell must not
        // be blended with its neighbours by an endpoint-grid registration.
        let expected = shade_ground(
            &l,
            ((x as f32 + 0.5) / sr as f32 - 0.5) * l.size as f32,
            ((z as f32 + 0.5) / sr as f32 - 0.5) * l.size as f32,
            crate::native_view::GROUND_PALETTE[1],
        );
        assert_eq!(
            image.pixels[(sr - 1 - z) * sr + x],
            Color32::from_rgb(
                expected[0].round() as u8,
                expected[1].round() as u8,
                expected[2].round() as u8
            )
        );
        assert_eq!(
            image.pixels[(sr - 3) * sr + 2],
            Color32::from_rgb(24, 29, 29)
        );
        assert!(render(&l, None, None, 2048, || true).unwrap().is_none());
        assert!(render(&l, None, None, 4097, || false).is_err());
    }
}
