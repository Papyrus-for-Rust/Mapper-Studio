//! Bounded, cancellable north-up terrain patch from the committed stamp kernel.
//! Run on a worker: connected-ocean analysis still visits the native sea grid.
use crate::{map_image, terrain_palette::Palette};
use eframe::egui::{Color32, ColorImage};
use papyrus_terrain::{monument_composition::projection::Projected, ocean};

#[derive(Clone, Copy)]
pub struct Settings<'a> {
    pub maximum_dimension: usize,
    pub palette: Option<&'a Palette>,
    /// Use the water-aware composition revision's boundary-connected ocean.
    /// False retains the legacy overview's simple height-based sea plane.
    pub connected_ocean: bool,
    /// Explicitly registered existing water, never inferred from low ground.
    pub local_water: Option<&'a [i16]>,
}

pub struct Patch {
    pub image: ColorImage,
    /// [minimum X, minimum Z, maximum X, maximum Z]; first image row is +Z.
    pub world_bounds: [f32; 4],
    pub approximate_colors: bool,
}
fn weighted(weights: &[f32], colors: &[[f32; 3]]) -> [f32; 3] {
    let sum = weights.iter().sum::<f32>();
    if sum <= 0. {
        return [114., 129., 79.];
    }
    std::array::from_fn(|c| {
        weights
            .iter()
            .zip(colors)
            .map(|(w, rgb)| w * rgb[c])
            .sum::<f32>()
            / sum
    })
}
fn water_color(depth: f32) -> [f32; 3] {
    let d = (depth / 40.).clamp(0., 1.);
    let t = d * (2. - d);
    [
        65. + (24. - 65.) * t,
        132. + (66. - 132.) * t,
        151. + (95. - 151.) * t,
    ]
}
pub fn render(
    projected: &Projected<'_, '_, '_>,
    settings: Settings<'_>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Patch>, String> {
    render_impl(projected, settings, false, cancelled)
}
/// Complete north-up viewport. Includes remote sea changes outside stamp bounds.
/// Background workers only; capped at the workstation's 2048-pixel overview.
pub fn render_viewport(
    projected: &Projected<'_, '_, '_>,
    settings: Settings<'_>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Patch>, String> {
    render_impl(projected, settings, true, cancelled)
}
fn render_impl(
    projected: &Projected<'_, '_, '_>,
    settings: Settings<'_>,
    viewport: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Patch>, String> {
    let limit = if viewport { 2048 } else { 256 };
    if !(2..=limit).contains(&settings.maximum_dimension) {
        return Err("Invalid projected stamp image dimension".into());
    }
    let hr = projected.height_resolution();
    let n = projected.surface_resolution();
    if let Some(water) = settings.local_water {
        if water.len() != hr * hr {
            return Err("Invalid projected local-water dimensions".into());
        }
        for chunk in water.chunks(4096) {
            if cancelled() {
                return Ok(None);
            }
            if chunk.iter().any(|h| !(0..=32766).contains(h)) {
                return Err("Invalid projected local-water height".into());
            }
        }
    }
    if cancelled() {
        return Ok(None);
    }
    let Some(footprint) = projected.world_bounds() else {
        return Ok(None);
    };
    let bounds = if viewport {
        let half = projected.size() as f32 * 0.5;
        [-half, -half, half, half]
    } else {
        footprint
    };
    let palette = if let Some(palette) = settings.palette {
        let Some(supported) = projected.supports_biomes(palette.biome_count(), &cancelled) else {
            return Ok(None);
        };
        supported.then_some(palette)
    } else {
        None
    };
    let mask = if settings.connected_ocean {
        let Some(mask) = projected.ocean(&cancelled)? else {
            return Ok(None);
        };
        Some(mask)
    } else {
        None
    };
    let dx = bounds[2] - bounds[0];
    let dz = bounds[3] - bounds[1];
    let extent = dx.max(dz);
    let dimension = |meters: f32| {
        ((settings.maximum_dimension as f32 * meters / extent).round() as usize)
            .clamp(2, settings.maximum_dimension)
    };
    let [w, h] = [dimension(dx), dimension(dz)];
    // The projection is immutable and samples encoded vertices through the
    // same stamp kernel as committed composition. Build on this worker, never
    // during UI pointer handling, and avoid allocating another full height map.
    let surface = if let Some(profile) = palette.and_then(Palette::surface_profile) {
        let surface = crate::generation_resources::with_cpu_pool(&cancelled, |pool, limits| {
            let bytes = (limits.image_memory_mib * 1024 * 1024)
                .checked_sub(w * h * 4)
                .ok_or("Projected image exceeds its working memory budget")?;
            let Some(mut surface) = crate::map_surface_frame::generate_sampled(
                |x, z| projected.encoded_height(x, z),
                crate::map_surface_frame::settings_dimensions(projected.size(), hr, bytes),
                profile,
                Some(pool),
                &cancelled,
            )?
            else {
                return Ok(None);
            };
            if profile == crate::map_surface_frame::Profile::Current
                && attach_shore(
                    &mut surface,
                    projected,
                    mask.as_ref(),
                    settings.local_water,
                    bytes,
                    &cancelled,
                )?
                .is_none()
            {
                return Ok(None);
            }
            Ok(Some(surface))
        })?;
        let Some(surface) = surface else {
            return Ok(None);
        };
        Some(surface.for_footprint((dx / w as f32).max(dz / h as f32) / projected.size() as f32))
    } else {
        None
    };
    let mut pixels = Vec::with_capacity(w * h);
    let size = projected.size() as f32;
    for row in 0..h {
        if cancelled() {
            return Ok(None);
        }
        let z = bounds[3] - (row as f32 + 0.5) / h as f32 * dz;
        let v = if viewport {
            1. - (row as f32 + 0.5) / h as f32
        } else {
            (z / size + 0.5).clamp(0., 1.)
        };
        for col in 0..w {
            let x = bounds[0] + (col as f32 + 0.5) / w as f32 * dx;
            let u = if viewport {
                (col as f32 + 0.5) / w as f32
            } else {
                (x / size + 0.5).clamp(0., 1.)
            };
            let sample = projected
                .sample(u, v)
                .ok_or("Invalid projected stamp sample")?;
            let render_height = palette
                .and_then(|p| p.cartography())
                .map_or(sample.height, |m| {
                    m.surface_height(surface.as_ref(), [u, v], || sample.height)
                });
            let depth = map_image::water_depth(
                [u, v],
                hr,
                n,
                render_height,
                mask.as_ref().map(|m| m.classification.as_slice()),
                settings.local_water,
                surface.as_ref(),
                projected.size(),
            );
            let slope = palette
                .and_then(|p| p.cartography())
                .map_or(0., |m| m.surface_slope(surface.as_ref(), [u, v]));
            let rgb = if let Some(depth) = depth {
                palette.and_then(|p| p.cartography()).map_or_else(
                    || water_color(depth),
                    |m| m.water_with_slope(sample.splat, depth, slope),
                )
            } else if palette.and_then(|p| p.cartography()).is_none() && sample.alpha < 128 {
                [24., 29., 29.]
            } else {
                let light = map_image::illumination_sampled(projected.size(), hr, u, v, |u, v| {
                    projected.height(u, v).unwrap()
                });
                if let Some(palette) = palette {
                    let rgb = palette.evaluate(sample.splat, sample.biome).unwrap_or([
                        114. / 255.,
                        129. / 255.,
                        79. / 255.,
                        1.,
                    ]);
                    if let Some(map) = palette.cartography() {
                        let normal = surface
                            .as_ref()
                            .and_then(|s| s.normal())
                            .and_then(|n| n.sample([u, v]))
                            .unwrap_or_else(|| {
                                crate::map_colors::normal_sampled(
                                    projected.size(),
                                    hr,
                                    u,
                                    v,
                                    |u, v| projected.height(u, v).unwrap(),
                                )
                            });
                        map.land_with_slope(rgb, normal, slope)
                    } else {
                        std::array::from_fn(|c| rgb[c] * 255. * light)
                    }
                } else {
                    let ground = weighted(&sample.splat, &crate::native_view::GROUND_PALETTE);
                    let biome = weighted(&sample.biome, &map_image::BIOMES);
                    std::array::from_fn(|c| (ground[c] * 0.8 + biome[c] * 0.2) * light)
                }
            };
            pixels.push(Color32::from_rgb(
                rgb[0].round().clamp(0., 255.) as u8,
                rgb[1].round().clamp(0., 255.) as u8,
                rgb[2].round().clamp(0., 255.) as u8,
            ));
        }
    }
    Ok((!cancelled()).then_some(Patch {
        image: ColorImage::new([w, h], pixels),
        world_bounds: bounds,
        approximate_colors: palette.is_none(),
    }))
}

fn attach_shore(
    surface: &mut crate::map_surface::Grid,
    projected: &Projected<'_, '_, '_>,
    mask: Option<&ocean::Mask>,
    water: Option<&[i16]>,
    maximum_bytes: usize,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<()>, String> {
    let base = projected.source_layers();
    let n = base.surface_resolution;
    let topology_bytes = n * n * std::mem::size_of::<i32>();
    let remaining = maximum_bytes
        .checked_sub(surface.retained_bytes())
        .and_then(|b| b.checked_sub(topology_bytes))
        .ok_or("Projected shoreline exceeds its image allowance")?;
    let water = water.map(|data| papyrus_terrain::water_map_query::Grid {
        resolution: base.height_resolution,
        data,
        origin_y: -500.,
        vertical_span: 1000.,
    });
    let input = papyrus_terrain::shore_mainland::Input {
        terrain: base.view(),
        water,
        origin_y: -500.,
        vertical_span: 1000.,
        ocean_level: 0.,
    };
    let peak = usize::try_from(papyrus_terrain::shore_mainland::bounds(&input)?.0)
        .map_err(|_| "Projected shoreline exceeds address space")?;
    if peak > remaining {
        return Err("Projected shoreline exceeds its image allowance".into());
    }
    let mut topology = Vec::with_capacity(n * n);
    for z in 0..n {
        if cancelled() {
            return Ok(None);
        }
        for x in 0..n {
            let mut value = projected
                .cell(x, z)
                .ok_or("Invalid projected shoreline topology")?
                .topology;
            if let Some(mask) = mask {
                value &= !ocean::OCEAN_BIT;
                if mask.classification[z * n + x] == ocean::OCEAN {
                    value |= ocean::OCEAN_BIT;
                }
            }
            topology.push(value);
        }
    }
    let mut input = input;
    input.terrain.topology = &topology;
    let Some(field) = papyrus_terrain::shore_mainland::generate_sampled(
        input,
        |position| projected.source_height(position, -500., 1000.).ok(),
        papyrus_terrain::shore_field::Limits {
            maximum_bytes: remaining as u64,
            maximum_work: 2_000_000_000 - n as u64 * n as u64 * 32,
        },
        cancelled,
    )?
    else {
        return Ok(None);
    };
    crate::map_surface_frame::upload_shore(
        surface,
        field,
        peak + topology_bytes,
        remaining,
        cancelled,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use papyrus_terrain::{
        monument_composition::{self, Image, Stamp, projection::Terrain},
        native_layers::Layers,
    };
    fn encode(height: f32) -> i16 {
        ((height + 500.) / 1000. * 32766.).round().clamp(0., 32766.) as i16
    }
    fn layers() -> Layers {
        let (hr, sr) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let cells = sr * sr;
        let mut layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![0; hr * hr],
            splat: vec![0; cells * 8],
            biome: vec![0; cells * 5],
            topology: vec![0; cells],
            normalized_splat_cells: 0,
            normalized_biome_cells: 0,
        };
        for z in 0..hr {
            for x in 0..hr {
                let height = if x < hr / 3 || (x > hr * 3 / 4 && z.abs_diff(hr / 2) < hr / 12) {
                    -20.
                } else {
                    10. + z as f32 / hr as f32 * 3.
                };
                layers.height[z * hr + x] = encode(height);
            }
        }
        layers.splat[4 * cells..5 * cells].fill(255);
        layers.biome[cells..2 * cells].fill(255);
        layers
    }
    fn image(rgba: &'static [u8], width: usize) -> Image<'static> {
        Image {
            width,
            height: rgba.len() / 4 / width,
            rgba,
        }
    }
    fn stamp() -> Stamp<'static> {
        Stamp {
            source_cliff_generation: false,
            source_quad_raster: false,
            source_height_blending: false,
            source_texture_sampling: false,
            position: [-150., 30., 0.],
            yaw_degrees: 37.,
            size: [280., 600., 180.],
            offset: [13., 0., -17.],
            radius: 2000.,
            fade: 1200.,
            height: Some(image(&[0; 4], 1)),
            blend: Some(image(&[0, 0, 0, 255], 1)),
            splat: Some([image(&[64, 64, 0, 0], 1), image(&[0, 0, 64, 0], 1)]),
            splat_mask: 255,
            biome: Some(image(&[0, 64, 32, 0], 1)),
            biome_mask: 31,
            alpha: Some(image(
                &[0, 0, 0, 0, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 0],
                2,
            )),
            topology: None,
            topology_mask: 0,
            replace_topology: false,
            auto_cliff_splat: true,
            auto_cliff_topology: true,
        }
    }
    fn palette(count: usize) -> Palette {
        let (installation, mut doc) = crate::terrain_palette::tests::fixture(count);
        for i in 0..8 {
            for j in 0..count {
                doc["materials"][i]["biome_colors"][j] = serde_json::json!([
                    0.1 + i as f32 * 0.02,
                    0.2 + j as f32 * 0.06,
                    0.1 + (i + j) as f32 * 0.03,
                    1.
                ]);
            }
        }
        Palette::decode(&serde_json::to_vec(&doc).unwrap(), &installation).unwrap()
    }
    #[test]
    fn viewport_render_covers_the_world_instead_of_only_the_local_stamp_patch() {
        let layers = layers();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let terrain = Terrain::new(&layers, &alpha, || false).unwrap().unwrap();
        let mut stamp = stamp();
        stamp.radius = 100.;
        stamp.fade = 20.;
        stamp.position[1] = -20.;
        let projected = terrain.projected(&stamp).unwrap();
        let palette = palette(5);
        let settings = Settings {
            maximum_dimension: 64,
            palette: Some(&palette),
            connected_ocean: true,
            local_water: None,
        };
        let patch = render(&projected, settings, || false).unwrap().unwrap();
        assert_ne!(patch.world_bounds, [-500., -500., 500., 500.]);
        let viewport = render_viewport(&projected, settings, || false)
            .unwrap()
            .unwrap();
        assert_eq!(viewport.world_bounds, [-500., -500., 500., 500.]);
        assert_eq!(viewport.image.size, [64, 64]);
        let full = monument_composition::compose_with_water(
            &layers,
            &alpha,
            std::slice::from_ref(&stamp),
            || false,
        )
        .unwrap()
        .unwrap();
        let expected = map_image::render_with_palette(
            &full.layers,
            Some(&full.alpha),
            full.ocean.as_ref(),
            64,
            Some(&palette),
            || false,
        )
        .unwrap()
        .unwrap();
        for (a, b) in viewport.image.pixels.iter().zip(expected.pixels) {
            assert!(
                a.to_array()
                    .into_iter()
                    .zip(b.to_array())
                    .all(|(a, b)| a.abs_diff(b) <= 1)
            );
        }
        assert!(
            render_viewport(
                &projected,
                Settings {
                    maximum_dimension: 2049,
                    ..settings
                },
                || false
            )
            .is_err()
        );
        assert!(
            render_viewport(&projected, settings, || true)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn projected_images_match_full_composed_overviews_with_sea_freshwater_holes_and_palette_fallback()
     {
        let layers = layers();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let terrain = Terrain::new(&layers, &alpha, || false).unwrap().unwrap();
        let five = palette(5);
        let four = palette(4);
        let (installation, mut doc) = crate::terrain_palette::tests::fixture(5);
        doc["cartography"] = serde_json::to_value(crate::map_colors::tests::fixture()).unwrap();
        let cartographic =
            Palette::decode(&serde_json::to_vec(&doc).unwrap(), &installation).unwrap();
        let local = vec![encode(15.); layers.height_resolution.pow(2)];
        let mut holes = 0;
        for (connected, fresh, count, y) in [
            (true, false, 5, -20.),
            (true, true, 5, 30.),
            (false, false, 0, -20.),
            (true, false, 0, 30.),
            (true, false, 4, 30.),
            (true, false, 6, -20.),
            (true, true, 6, 30.),
        ] {
            let mut stamp = stamp();
            stamp.position[1] = y;
            let projected = terrain.projected(&stamp).unwrap();
            let palette = match count {
                6 => Some(&cartographic),
                5 => Some(&five),
                4 => Some(&four),
                _ => None,
            };
            let water = fresh.then_some(local.as_slice());
            let patch = render(
                &projected,
                Settings {
                    maximum_dimension: 64,
                    palette,
                    connected_ocean: connected,
                    local_water: water,
                },
                || false,
            )
            .unwrap()
            .unwrap();
            assert_eq!(patch.world_bounds, [-500., -500., 500., 500.]);
            assert_eq!(patch.image.size, [64, 64]);
            assert_eq!(patch.approximate_colors, !matches!(count, 5 | 6));
            let compose = if connected {
                monument_composition::compose_with_water
            } else {
                monument_composition::compose
            };
            let committed = compose(&layers, &alpha, std::slice::from_ref(&stamp), || false)
                .unwrap()
                .unwrap();
            let expected = map_image::render_with_water(
                &committed.layers,
                Some(&committed.alpha),
                committed.ocean.as_ref(),
                water,
                64,
                palette,
                || false,
            )
            .unwrap()
            .unwrap();
            for (i, (actual, expected)) in
                patch.image.pixels.iter().zip(&expected.pixels).enumerate()
            {
                for (a, b) in actual.to_array().into_iter().zip(expected.to_array()) {
                    assert!(
                        a.abs_diff(b) <= 1,
                        "projected pixel {i} differs: {actual:?} {expected:?}, connected={connected} fresh={fresh} palette={count} y={y}"
                    );
                }
                holes += usize::from(*actual == Color32::from_rgb(24, 29, 29));
            }
        }
        assert!(
            holes > 50,
            "The fixture must exercise visible terrain holes"
        );
    }
    #[test]
    fn water_at_ground_level_is_not_a_different_surface_in_the_projection() {
        let mut layers = layers();
        layers.height.fill(17000);
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let water = vec![17000; layers.height.len()];
        let mut stamp = stamp();
        stamp.height = None;
        stamp.blend = None;
        stamp.alpha = None;
        stamp.biome = None;
        stamp.splat = None;
        stamp.auto_cliff_splat = false;
        let terrain = Terrain::new(&layers, &alpha, || false).unwrap().unwrap();
        let projected = terrain.projected(&stamp).unwrap();
        for output in [64, 65, 127] {
            let actual = render_viewport(
                &projected,
                Settings {
                    maximum_dimension: output,
                    palette: None,
                    connected_ocean: false,
                    local_water: Some(&water),
                },
                || false,
            )
            .unwrap()
            .unwrap();
            let expected = map_image::render_with_water(
                &layers,
                Some(&alpha),
                None,
                Some(&water),
                output,
                None,
                || false,
            )
            .unwrap()
            .unwrap();
            let maximum = actual
                .image
                .pixels
                .iter()
                .zip(&expected.pixels)
                .flat_map(|(a, b)| {
                    a.to_array()
                        .into_iter()
                        .zip(b.to_array())
                        .map(|(a, b)| a.abs_diff(b))
                })
                .max()
                .unwrap();
            assert!(
                maximum <= 1,
                "Equal water/ground differs by {maximum} at {output}px"
            );
        }
    }
    #[test]
    fn renderer_rejects_invalid_water_and_cancels_before_publishing_a_patch() {
        let layers = layers();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let stamp = stamp();
        let terrain = Terrain::new(&layers, &alpha, || false).unwrap().unwrap();
        let projected = terrain.projected(&stamp).unwrap();
        let settings = Settings {
            maximum_dimension: 64,
            palette: None,
            connected_ocean: false,
            local_water: None,
        };
        for dimension in [0, 1, 257, usize::MAX] {
            assert!(
                render(
                    &projected,
                    Settings {
                        maximum_dimension: dimension,
                        ..settings
                    },
                    || false
                )
                .is_err()
            );
        }
        assert!(
            render(
                &projected,
                Settings {
                    local_water: Some(&[0; 4]),
                    ..settings
                },
                || false
            )
            .is_err()
        );
        let bad = vec![-1; layers.height_resolution.pow(2)];
        assert!(
            render(
                &projected,
                Settings {
                    local_water: Some(&bad),
                    ..settings
                },
                || false
            )
            .is_err()
        );
        assert!(render(&projected, settings, || true).unwrap().is_none());
        let calls = std::cell::Cell::new(0);
        assert!(
            render(&projected, settings, || {
                calls.set(calls.get() + 1);
                calls.get() > 8
            })
            .unwrap()
            .is_none()
        );
        let calls = std::cell::Cell::new(0);
        assert!(
            render(
                &projected,
                Settings {
                    connected_ocean: true,
                    ..settings
                },
                || {
                    calls.set(calls.get() + 1);
                    calls.get() > 8
                }
            )
            .unwrap()
            .is_none()
        );
    }
    #[test]
    fn bound_filtered_projection_matches_commit_and_changes_land_and_shallow_water() {
        let mut layers = layers();
        let hr = layers.height_resolution;
        for z in 0..hr {
            for x in 0..hr {
                // Alternating steep land and water strips exercise nonzero slope.
                let elevation = if x < hr / 2 {
                    // Include moderate underwater slopes. The client's normal
                    // producer flattens near-vertical strip edges back to up.
                    (if (x / 8) % 2 == 0 { -1. } else { 89. }) + (z as f32 / 12.).sin() * 24.
                } else {
                    20. + (x % 16) as f32 * 12.
                };
                layers.height[z * hr + x] = encode(elevation);
            }
        }
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let terrain = Terrain::new(&layers, &alpha, || false).unwrap().unwrap();
        let mut stamp = stamp();
        stamp.height = None;
        stamp.blend = None;
        stamp.auto_cliff_splat = false;
        stamp.auto_cliff_topology = false;
        let projected = terrain.projected(&stamp).unwrap();
        let (installation, mut doc) = crate::terrain_palette::tests::fixture(5);
        let mut material = crate::map_colors::tests::fixture();
        material.shader_blob_sha256 =
            Some("9ff936729f90baad098b7a69f0a3244868345f99e3a2542e00063cc004821e07".into());
        // Synthetic texture input shared by both verified shader programs.
        for shader in [
            "9ff936729f90baad098b7a69f0a3244868345f99e3a2542e00063cc004821e07",
            "60b55c7b11da98c2a67f2162a29f1e1aba3ce66d698f8d017f1fe5e5f9a6b8b1",
        ] {
            material.shader_blob_sha256 = Some(shader.into());
            doc["cartography"] = serde_json::to_value(&material).unwrap();
            let legacy =
                Palette::decode(&serde_json::to_vec(&doc).unwrap(), &installation).unwrap();
            let mut bound = legacy.clone();
            if shader.starts_with("60b55c7b") {
                bound.test_current_surface_profile();
            } else {
                bound.test_surface_profile();
            }
            let settings = Settings {
                maximum_dimension: 128,
                palette: Some(&bound),
                connected_ocean: false,
                local_water: None,
            };
            let actual = render_viewport(&projected, settings, || false)
                .unwrap()
                .unwrap();
            let before = render_viewport(
                &projected,
                Settings {
                    palette: Some(&legacy),
                    ..settings
                },
                || false,
            )
            .unwrap()
            .unwrap();
            let committed = monument_composition::compose(
                &layers,
                &alpha,
                std::slice::from_ref(&stamp),
                || false,
            )
            .unwrap()
            .unwrap();
            let expected = map_image::render_with_palette(
                &committed.layers,
                Some(&committed.alpha),
                None,
                128,
                Some(&bound),
                || false,
            )
            .unwrap()
            .unwrap();
            let mut changed = [0, 0];
            for (i, ((a, b), old)) in actual
                .image
                .pixels
                .iter()
                .zip(expected.pixels)
                .zip(before.image.pixels)
                .enumerate()
            {
                assert!(
                    a.to_array()
                        .into_iter()
                        .zip(b.to_array())
                        .all(|(a, b)| a.abs_diff(b) <= 1),
                    "pixel {i}: {a:?} {b:?}"
                );
                if *a != old {
                    let u = (i % 128) as f32 / 128. + 0.5 / 128.;
                    let v = 1. - (i / 128) as f32 / 128. - 0.5 / 128.;
                    changed[usize::from(projected.height(u, v).unwrap() > 0.)] += 1;
                }
            }
            assert!(
                changed[0] > 0 && changed[1] > 0,
                "slope response missing in water/land: {changed:?}"
            );
            assert!(
                render_viewport(&projected, settings, || true)
                    .unwrap()
                    .is_none()
            );
        }
    }
}
/// Last valid source paint isolated for a cheap provisional drag image.
/// This is view data only; it never becomes terrain, a recipe, or an export.
pub struct MovingPatch {
    pub image: ColorImage,
    pub world_bounds: [f32; 4],
}
pub fn moving_patch(
    image: &ColorImage,
    hit: &[u8],
    bounds: [f32; 4],
    size: u32,
    cancelled: impl Fn() -> bool,
) -> Result<Option<MovingPatch>, String> {
    let [w, h] = image.size;
    if !(2..=2048).contains(&w)
        || w != h
        || image.pixels.len() != w * w
        || hit.len() != w * w
        || size == 0
        || bounds.iter().any(|v| !v.is_finite())
        || bounds[0] >= bounds[2]
        || bounds[1] >= bounds[3]
    {
        return Err("Invalid moving stamp image".into());
    }
    if cancelled() {
        return Ok(None);
    }
    let side = size as f32;
    let index = |v: f32| ((v / side + 0.5) * w as f32).clamp(0., w as f32);
    let x0 = index(bounds[0]).floor() as usize;
    let x1 = index(bounds[2]).ceil() as usize;
    let y0 = index(-bounds[3]).floor() as usize;
    let y1 = index(-bounds[1]).ceil() as usize;
    if x0 >= x1 || y0 >= y1 {
        return Ok(None);
    }
    let mut pixels = Vec::with_capacity((x1 - x0) * (y1 - y0));
    let mut visible = false;
    for y in y0..y1 {
        if cancelled() {
            return Ok(None);
        }
        for x in x0..x1 {
            let i = y * w + x;
            let paint = hit[i] != 0;
            visible |= paint;
            pixels.push(if paint {
                image.pixels[i]
            } else {
                Color32::TRANSPARENT
            });
        }
    }
    if !visible {
        return Ok(None);
    }
    let cropped = ColorImage::new([x1 - x0, y1 - y0], pixels);
    let image = crate::splat_preview::contextual_base(&cropped);
    let wx = |x: usize| (x as f32 / w as f32 - 0.5) * side;
    let wz = |y: usize| (0.5 - y as f32 / w as f32) * side;
    Ok((!cancelled()).then_some(MovingPatch {
        image,
        world_bounds: [wx(x0), wz(y1), wx(x1), wz(y0)],
    }))
}
#[cfg(test)]
mod moving_tests {
    use super::*;
    #[test]
    fn moving_patch_keeps_only_visible_source_paint_and_preserves_coverage_when_reduced() {
        let image = ColorImage::filled([512, 512], Color32::WHITE);
        let mut hit = vec![0; 512 * 512];
        for y in (0..512).step_by(2) {
            for x in (0..512).step_by(2) {
                hit[y * 512 + x] = 1;
            }
        }
        let patch = moving_patch(&image, &hit, [-500., -500., 500., 500.], 1000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(patch.image.size, [256, 256]);
        assert_eq!(patch.world_bounds, [-500., -500., 500., 500.]);
        assert!(
            patch
                .image
                .pixels
                .iter()
                .all(|p| p.to_array() == [64, 64, 64, 64])
        );
        hit.fill(0);
        assert!(
            moving_patch(&image, &hit, [-500., -500., 500., 500.], 1000, || false)
                .unwrap()
                .is_none()
        );
        assert!(
            moving_patch(&image, &hit, [-500., -500., 500., 500.], 1000, || true)
                .unwrap()
                .is_none()
        );
        assert!(
            moving_patch(&image, &hit[..4], [-500., -500., 500., 500.], 1000, || {
                false
            })
            .is_err()
        );
    }
}
