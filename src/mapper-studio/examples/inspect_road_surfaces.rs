//! Offline inspection of generated road geometry using the workstation renderer.
//! Does not open a window or modify a project. Inputs are local verification artifacts.
#[allow(dead_code)]
#[path = "../src/terrain_3d.rs"]
mod terrain_3d;
use eframe::egui::{self, Color32, ColorImage, Rect, Vec2};
use mapper_studio::{Project, image_export, monument_source::Model};
use std::{collections::HashMap, path::Path, time::Instant};

fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err("inspect_road_surfaces PROJECT GENERATED_LAYERS NEW_OUTPUT_DIRECTORY".into());
    }
    let project = Project::load(Path::new(&args[0]))?;
    let input = Path::new(&args[1]);
    let output = Path::new(&args[2]);
    std::fs::create_dir(output).map_err(|e| e.to_string())?;
    let (hr, sr) = papyrus_terrain::native_layers::dimensions(project.size)?;
    let raw = std::fs::read(input.join("height.bin")).map_err(|e| e.to_string())?;
    if raw.len() != hr * hr * 2 {
        return Err("Invalid height buffer".into());
    }
    let layers = papyrus_terrain::native_layers::Layers {
        size: project.size,
        terrain_seed: project.seeds[0].value,
        biome_seed: project.seeds[1].value,
        height_resolution: hr,
        surface_resolution: sr,
        height: raw
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect(),
        biome: vec![],
        splat: vec![],
        topology: vec![],
        normalized_biome_cells: 0,
        normalized_splat_cells: 0,
    };
    let heights = layers.sample_heights(257)?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(
        std::fs::File::open(input.join("overview.png")).map_err(|e| e.to_string())?,
    ));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut pixels = vec![0; reader.output_buffer_size().ok_or("PNG too large")?];
    let info = reader.next_frame(&mut pixels).map_err(|e| e.to_string())?;
    if info.color_type != png::ColorType::Rgba {
        return Err("Expected RGBA overview".into());
    }
    let image = ColorImage::from_rgba_unmultiplied(
        [info.width as usize, info.height as usize],
        &pixels[..info.buffer_size()],
    );
    let mut models = Vec::new();
    let mut bounds = Vec::new();
    let mut paths: Vec<_> = std::fs::read_dir(input)
        .map_err(|e| e.to_string())?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    paths.sort();
    for path in paths {
        let name = path.file_name().unwrap().to_string_lossy();
        if !name.starts_with("road-surface-") || !name.contains("-lod-2-") {
            continue;
        }
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let origin: [f32; 3] = serde_json::from_value(value["geometry"]["origin"].clone())
            .map_err(|e| e.to_string())?;
        let mesh: papyrus_terrain::path_mesh::Template =
            serde_json::from_value(value["geometry"]["mesh"].clone()).map_err(|e| e.to_string())?;
        mesh.validate()?;
        bounds.push([
            origin[0] + mesh.center[0] - mesh.extents[0],
            origin[2] + mesh.center[2] - mesh.extents[2],
            origin[0] + mesh.center[0] + mesh.extents[0],
            origin[2] + mesh.center[2] + mesh.extents[2],
        ]);
        models.push((
            Model {
                vertices: mesh.vertices,
                triangles: mesh.triangles,
                issues: vec![],
                complete: true,
                simplified: false,
            },
            origin,
        ));
    }
    if models.is_empty() {
        return Err("No LOD2 road chunks".into());
    }
    let instances: Vec<_> = models
        .iter()
        .map(|(model, origin)| {
            let mut instance = terrain_3d::ModelInstance::yaw(model, *origin, 0.);
            instance.tint = Some([64., 67., 70.]);
            instance
        })
        .collect();
    let mut measurements = Vec::new();
    for detailed in [false, true] {
        let ctx = egui::Context::default();
        let texture = ctx.load_texture("overview", image.clone(), egui::TextureOptions::LINEAR);
        let mut camera = terrain_3d::Camera::default();
        let mut textures = HashMap::new();
        let mut timings = Vec::new();
        let mut last = None;
        for _ in 0..41 {
            let start = Instant::now();
            let frame = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(1200., 900.),
                    )),
                    ..Default::default()
                },
                |ui| {
                    let (rect, response) =
                        ui.allocate_exact_size(Vec2::new(1180., 880.), egui::Sense::drag());
                    camera.draw_with_ground(
                        ui,
                        rect,
                        &response,
                        texture.id(),
                        &image,
                        &heights,
                        257,
                        project.size,
                        &instances,
                        detailed.then_some(terrain_3d::NativeGround {
                            revision: texture.id(),
                            layers: &layers,
                            surfaces: &bounds,
                        }),
                    );
                },
            );
            timings.push(start.elapsed().as_secs_f64() * 1000.);
            for (id, deltas) in &frame.textures_delta.set {
                for delta in deltas {
                    if delta.pos.is_none() {
                        let egui::ImageData::Color(image) = &delta.image;
                        textures.insert(*id, image.clone());
                    }
                }
            }
            last = Some(frame);
        }
        let first_ms = timings.remove(0);
        timings.sort_by(f64::total_cmp);
        let frame = last.unwrap();
        let primitives = ctx.tessellate(frame.shapes, frame.pixels_per_point);
        let (render, triangles) = raster(&primitives, &textures);
        let name = if detailed {
            "native-road-detail"
        } else {
            "coarse-baseline"
        };
        image_export::write_png(&output.join(format!("{name}.png")), &render, || false)?;
        measurements.push(serde_json::json!({"variant":name,"first_frame_ms":first_ms,"median_frame_ms":timings[20],"p95_frame_ms":timings[38],"triangles":triangles,"frames":40}));
    }
    let report = serde_json::json!({"road_lod2_chunks":models.len(),"viewport":[1200,900],"measurements":measurements,"scope":"CPU egui geometry preparation; excludes GPU and software screenshot rasterization"});
    std::fs::write(
        output.join("measurements.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    println!("{report}");
    Ok(())
}

fn raster(
    primitives: &[egui::ClippedPrimitive],
    textures: &HashMap<egui::TextureId, std::sync::Arc<ColorImage>>,
) -> (ColorImage, usize) {
    let mut output = ColorImage::new([1200, 900], vec![Color32::from_rgb(24, 66, 95); 1200 * 900]);
    let mut count = 0;
    for primitive in primitives {
        let egui::epaint::Primitive::Mesh(mesh) = &primitive.primitive else {
            continue;
        };
        let Some(texture) = textures.get(&mesh.texture_id) else {
            continue;
        };
        for indices in mesh.indices.chunks_exact(3) {
            let v = indices
                .iter()
                .map(|i| mesh.vertices[*i as usize])
                .collect::<Vec<_>>();
            let edge = |a: egui::Pos2, b: egui::Pos2, p: egui::Pos2| {
                (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x)
            };
            let area = edge(v[0].pos, v[1].pos, v[2].pos);
            if !area.is_finite() || area.abs() < 1e-6 {
                continue;
            }
            count += 1;
            let clip = primitive.clip_rect.intersect(Rect::from_min_max(
                egui::Pos2::ZERO,
                egui::pos2(1200., 900.),
            ));
            let x0 = v
                .iter()
                .map(|p| p.pos.x)
                .fold(f32::INFINITY, f32::min)
                .floor()
                .max(clip.min.x) as usize;
            let x1 = v
                .iter()
                .map(|p| p.pos.x)
                .fold(f32::NEG_INFINITY, f32::max)
                .ceil()
                .min(clip.max.x) as usize;
            let y0 = v
                .iter()
                .map(|p| p.pos.y)
                .fold(f32::INFINITY, f32::min)
                .floor()
                .max(clip.min.y) as usize;
            let y1 = v
                .iter()
                .map(|p| p.pos.y)
                .fold(f32::NEG_INFINITY, f32::max)
                .ceil()
                .min(clip.max.y) as usize;
            for y in y0.min(900)..y1.min(900) {
                for x in x0.min(1200)..x1.min(1200) {
                    let p = egui::pos2(x as f32 + 0.5, y as f32 + 0.5);
                    let w = [
                        edge(v[1].pos, v[2].pos, p) / area,
                        edge(v[2].pos, v[0].pos, p) / area,
                        edge(v[0].pos, v[1].pos, p) / area,
                    ];
                    if w.iter().any(|v| *v < -1e-5) {
                        continue;
                    }
                    let uv = (0..3).fold(egui::Vec2::ZERO, |uv, i| uv + v[i].uv.to_vec2() * w[i]);
                    let tx = (uv.x * texture.size[0] as f32).clamp(0., (texture.size[0] - 1) as f32)
                        as usize;
                    let ty = (uv.y * texture.size[1] as f32).clamp(0., (texture.size[1] - 1) as f32)
                        as usize;
                    let tex = texture.pixels[ty * texture.size[0] + tx].to_array();
                    let color: [u8; 4] = std::array::from_fn(|c| {
                        (tex[c] as f32 * (0..3).map(|i| v[i].color[c] as f32 * w[i]).sum::<f32>()
                            / 255.)
                            .clamp(0., 255.) as u8
                    });
                    if color[3] > 0 {
                        output.pixels[y * 1200 + x] = Color32::from_rgba_premultiplied(
                            color[0], color[1], color[2], color[3],
                        );
                    }
                }
            }
        }
    }
    (output, count)
}
