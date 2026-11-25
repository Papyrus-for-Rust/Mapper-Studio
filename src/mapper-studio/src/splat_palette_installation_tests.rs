//! Opt-in, offscreen measurements using an owner's verified installation.
use super::*;
use papyrus_terrain::monument_composition;
use std::{fs::OpenOptions, io::BufWriter, path::PathBuf};

fn required(name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("Set {name}"))
}

fn png(path: &std::path::Path, image: &ColorImage) {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap();
    let mut encoder = png::Encoder::new(
        BufWriter::new(file),
        image.size[0] as u32,
        image.size[1] as u32,
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    let bytes = image
        .pixels
        .iter()
        .flat_map(Color32::to_srgba_unmultiplied)
        .collect::<Vec<_>>();
    writer.write_image_data(&bytes).unwrap();
    writer.finish().unwrap();
}

#[test]
#[ignore = "Requires owned Rust assets; writes a new offscreen diagnostic directory"]
fn installation_palette_drag_vs_committed_paint() {
    let root = required("MAPPER_TEST_RUST_ROOT");
    let output = required("MAPPER_TEST_SPLAT_OUTPUT");
    assert!(!output.exists(), "Choose a new diagnostic output directory");
    let installation = crate::installation::inspect(&root, || false).unwrap();
    let catalog = crate::prefab_assets::load(
        &root,
        &required("MAPPER_TEST_CATALOG"),
        &installation,
        || false,
    )
    .unwrap();
    let bindings = crate::prefab_textures::Bindings::load(
        &root,
        &required("MAPPER_TEST_BINDINGS"),
        &catalog,
        || false,
    )
    .unwrap();
    let palette = crate::terrain_palette::Palette::load(
        &root,
        &required("MAPPER_TEST_PALETTE"),
        &installation,
        || false,
    )
    .unwrap()
    .unwrap();
    let stage = tempfile::Builder::new()
        .prefix(".stamp-palette-")
        .tempdir_in(output.parent().unwrap())
        .unwrap();
    let mut results = Vec::new();
    for name in ["harbor_1", "harbor_2", "ferry_terminal_1", "airfield_1"] {
        let suffix = format!("/{name}.prefab");
        let (path, asset) = catalog
            .entries
            .iter()
            .find(|(path, _)| path.ends_with(&suffix))
            .unwrap();
        let preview = load(&bindings, asset, || false).unwrap().unwrap();
        let material = preview.material_weights.as_ref().unwrap();
        let prepared = crate::composition::prepare(asset, &bindings, 1000, || false)
            .unwrap()
            .unwrap();
        for biome in 0..5 {
            let mut layers = super::tests::palette_layers();
            let cells = layers.surface_resolution.pow(2);
            layers.biome.fill(0);
            layers.biome[biome * cells..(biome + 1) * cells].fill(255);
            for yaw in [0., 37., 123., 271.] {
                let overlay = contextual_image_with_palette(
                    material,
                    preview.biome_weights.as_ref(),
                    preview.geometry,
                    &layers,
                    [0., 0., yaw],
                    &palette,
                );
                let mut stamp = prepared.stamp([0., 20., 0.], f64::from(yaw));
                // Isolate material/biome color fidelity. Height, holes, topology,
                // cliff repaint and water are separate operations, not this oracle.
                stamp.height = None;
                stamp.blend = None;
                stamp.alpha = None;
                stamp.topology = None;
                stamp.auto_cliff_splat = false;
                stamp.auto_cliff_topology = false;
                let committed =
                    monument_composition::compose(&layers, &vec![255; cells], &[stamp], || false)
                        .unwrap()
                        .unwrap();
                assert_eq!(
                    layers.height, committed.layers.height,
                    "Material-only oracle changed height"
                );
                let surface = if palette.has_surface_profile() {
                    Some(
                        crate::map_surface_frame::generate(
                            &layers.height,
                            crate::map_surface_frame::settings(&layers, 128 * 1024 * 1024),
                            palette.surface_profile().unwrap(),
                            None,
                            || false,
                        )
                        .unwrap()
                        .unwrap(),
                    )
                } else {
                    None
                };
                let [w, h] = overlay.size;
                let quad = preview.geometry.quad(0., 0., yaw);
                let mut picture = ColorImage::filled([w * 3, h], Color32::from_rgb(24, 66, 95));
                let mut errors = Vec::new();
                let mut partial = 0;
                let mut partial_coverage = 0;
                for (i, pixel) in overlay.pixels.iter().enumerate() {
                    let u = ((i % w) as f32 + 0.5) / w as f32;
                    let v = ((i / w) as f32 + 0.5) / h as f32;
                    let world: [f32; 2] = std::array::from_fn(|c| {
                        quad[0][c] + u * (quad[1][c] - quad[0][c]) + v * (quad[3][c] - quad[0][c])
                    });
                    if world.iter().any(|v| v.abs() >= 499.) {
                        continue;
                    }
                    let shade = |maps: &papyrus_terrain::native_layers::Layers| {
                        crate::map_image::shade_weights_surface(
                            maps,
                            world[0],
                            world[1],
                            crate::map_image::material_weights_at(maps, world[0], world[1]),
                            crate::map_image::biome_weights_at(maps, world[0], world[1]),
                            &palette,
                            surface.as_ref(),
                            None,
                            None,
                        )
                    };
                    let base = shade(&layers);
                    let expected = shade(&committed.layers);
                    let rgba = pixel.to_srgba_unmultiplied();
                    let a = f32::from(pixel.a()) / 255.;
                    let actual: [f32; 3] =
                        std::array::from_fn(|c| base[c] * (1. - a) + f32::from(rgba[c]) * a);
                    let error = (0..3)
                        .map(|c| (actual[c] - expected[c]).abs())
                        .fold(0., f32::max);
                    // Include transparent pixels: filtering must not silently
                    // drop real paint. Exclude only pixels outside the map.
                    errors.push(error);
                    partial += usize::from(pixel.a() > 0 && pixel.a() < 255);
                    // Cartographic RGB is already composited and can be opaque
                    // even where the source paint is fractional. Measure that
                    // input separately rather than requiring inverse RGB/alpha
                    // reconstruction for the nonlinear material lerps.
                    let paint = material.sample(u, v).iter().sum::<f32>().clamp(0., 1.);
                    let climate = preview
                        .biome_weights
                        .as_ref()
                        .map_or(0., |b| b.sample(u, v).iter().sum::<f32>().clamp(0., 1.));
                    let coverage = ((paint + climate - paint * climate) * 255.).round();
                    partial_coverage += usize::from(coverage > 0. && coverage < 255.);
                    let rgb = |c: [f32; 3]| {
                        Color32::from_rgb(
                            c[0].round().clamp(0., 255.) as u8,
                            c[1].round().clamp(0., 255.) as u8,
                            c[2].round().clamp(0., 255.) as u8,
                        )
                    };
                    let row = i / w * w * 3;
                    let col = i % w;
                    picture.pixels[row + col] = rgb(actual);
                    picture.pixels[row + col + w] = rgb(expected);
                    picture.pixels[row + col + w * 2] = rgb([error * 8., 0., 0.]);
                }
                assert!(!errors.is_empty());
                assert!(
                    partial_coverage > 0,
                    "Real texture must exercise partial input paint coverage"
                );
                errors.sort_by(f32::total_cmp);
                let mean = errors.iter().map(|e| f64::from(*e)).sum::<f64>() / errors.len() as f64;
                let image = format!("{name}-biome-{biome}-yaw-{yaw}.png");
                png(&stage.path().join(&image), &picture);
                results.push(serde_json::json!({"prefab":path,"biome":biome,"yaw":yaw,"samples":errors.len(),"partial_alpha_samples":partial,"partial_input_coverage_samples":partial_coverage,"mean_max_channel_error":mean,"p95_max_channel_error":errors[errors.len()*95/100],"maximum_channel_error":errors.last(),"image":image}));
            }
        }
    }
    assert_eq!(results.len(), 80);
    let report = serde_json::json!({"format":"mapper-stamp-palette-diagnostic","schema":2,"catalog_sha256":catalog.sha256,"bindings_sha256":bindings.sha256,"palette_sha256":palette.sha256,"filtered_surface_profile":palette.has_surface_profile(),"background":"synthetic flat grass material; one of five installation biomes per case","scope":"material and biome paint at fixed height; texture filtering measured, no pixel-equivalence assertion","image_columns":["drag composited over terrain","committed material and biome paint","maximum RGB error magnified 8 times in red"],"desktop_windows_opened":0,"game_rendering_validated":false,"results":results});
    std::fs::write(
        stage.path().join("report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    std::fs::rename(stage.path(), &output).unwrap();
    println!("80 installation-backed comparisons: {}", output.display());
}
