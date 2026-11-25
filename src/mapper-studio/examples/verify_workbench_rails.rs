//! Exercise the desktop's initial rail settings through normal headless composition.
#[allow(dead_code)] // Reuse the production native renderer in this headless verification tool.
#[path = "../src/terrain_3d.rs"]
mod terrain_3d;
use std::{path::Path, sync::atomic::AtomicBool};
fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 6 {
        return Err(
            "verify_workbench_rails PROJECT RUST_ROOT CATALOG BINDINGS WORKER NEW_BUNDLE".into(),
        );
    }
    let destination = Path::new(&args[5]);
    if destination.exists() {
        return Err("Verification output must not exist".into());
    }
    let mut project = mapper_studio::Project::load(Path::new(&args[0]))?;
    let settings = mapper_studio::rail_generation::initial_settings(project.master_seed);
    project
        .rails
        .as_mut()
        .ok_or("Use an already bound rail project")?
        .settings = settings;
    project.validate()?;
    let cancel = AtomicBool::new(false);
    let generated = mapper_studio::headless::generate(
        &project,
        &mapper_studio::headless::Options {
            rust_root: Some(args[1].clone().into()),
            catalog: Some(args[2].clone().into()),
            bindings: Some(args[3].clone().into()),
            worker: Some(args[4].clone().into()),
            skip_composition: false,
        },
        &cancel,
        |label, progress| eprintln!("{:.0}% {label}", progress * 100.),
    )?
    .ok_or("Rail verification cancelled")?;
    let scene = generated
        .composed
        .as_ref()
        .and_then(|m| m.rails.as_ref())
        .ok_or("Normal composition omitted enabled rails")?;
    if scene.tracks.is_empty() || scene.meshes.is_empty() {
        return Err("Normal rail composition returned no tracks or mesh chunks".into());
    }
    let tracks = scene.tracks.len();
    let chunks = scene.meshes.len();
    let maps = generated.composed.as_ref().unwrap();
    let models = terrain_3d::surface_instances(maps).collect::<Vec<_>>();
    let bounds = terrain_3d::surface_bounds(terrain_3d::surface_instances(maps));
    let coarsest = scene
        .meshes
        .iter()
        .map(|c| c.lod)
        .max()
        .ok_or("No source LODs")?;
    if models.iter().any(|instance| match instance.model {
        terrain_3d::Geometry::Surface(mesh) => {
            !scene
                .meshes
                .iter()
                .any(|c| c.lod == coarsest && std::ptr::eq(mesh, &c.geometry.mesh))
                && !maps
                    .roads
                    .as_ref()
                    .and_then(|r| r.surfaces.as_ref())
                    .is_some_and(|s| {
                        s.chunks
                            .iter()
                            .any(|c| std::ptr::eq(mesh, &c.geometry.mesh))
                    })
        }
        terrain_3d::Geometry::Static(_) => true,
    }) {
        return Err("Viewport copied or selected an unexpected template".into());
    }
    let ctx = eframe::egui::Context::default();
    let mut camera = terrain_3d::Camera::default();
    camera.enabled = true;
    let mut output = ctx.run_ui(
        eframe::egui::RawInput {
            screen_rect: Some(eframe::egui::Rect::from_min_size(
                eframe::egui::Pos2::ZERO,
                eframe::egui::vec2(900., 700.),
            )),
            ..Default::default()
        },
        |ui| {
            let (rect, response) =
                ui.allocate_exact_size(eframe::egui::vec2(900., 700.), eframe::egui::Sense::drag());
            camera.draw_with_ground(
                ui,
                rect,
                &response,
                eframe::egui::TextureId::default(),
                &generated.image,
                &generated.heights,
                generated.resolution,
                project.size,
                &models,
                Some(terrain_3d::NativeGround {
                    revision: eframe::egui::TextureId::default(),
                    layers: &maps.layers,
                    surfaces: &bounds,
                }),
            );
        },
    );
    let render_result = (|| -> Result<serde_json::Value, String> {
        let mesh = output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                eframe::egui::Shape::Mesh(m) => Some(m),
                _ => None,
            })
            .ok_or("Viewport returned no mesh")?;
        let colored = mesh
            .vertices
            .iter()
            .filter(|v| v.color != eframe::egui::Color32::WHITE)
            .count()
            / 3;
        if colored == 0
            || colored > 48000
            || mesh
                .vertices
                .iter()
                .any(|v| !v.pos.is_finite() || !v.uv.is_finite())
            || mesh
                .indices
                .iter()
                .any(|i| *i as usize >= mesh.vertices.len())
        {
            return Err("Viewport frame violates geometry/render limits".into());
        }
        let delta = output
            .textures_delta
            .set
            .get(&mesh.texture_id)
            .and_then(|d| d.iter().find(|d| d.pos.is_none()))
            .ok_or("Viewport atlas missing")?;
        let eframe::egui::ImageData::Color(image) = &delta.image;
        mapper_studio::image_export::write_png(
            &destination.with_extension("atlas.png"),
            image.as_ref(),
            || false,
        )?;
        let vertices = mesh
            .vertices
            .iter()
            .map(|v| {
                let c = v.color.to_array();
                serde_json::json!([v.pos.x, v.pos.y, v.uv.x, v.uv.y, c[0], c[1], c[2], c[3]])
            })
            .collect::<Vec<_>>();
        std::fs::write(
            destination.with_extension("frame.json"),
            serde_json::to_vec(
                &serde_json::json!({"size":[900,700],"vertices":vertices,"indices":mesh.indices}),
            )
            .unwrap(),
        )
        .map_err(|e| e.to_string())?;
        Ok(
            serde_json::json!({"borrowed_surface_instances":models.len(),"surface_bounds":bounds.len(),"source_coarsest_lod":coarsest,"frame_model_triangles":colored,"frame_vertices":mesh.vertices.len(),"frame_triangles":mesh.indices.len()/3,"finite_frame":true,"source_buffers_borrowed":true}),
        )
    })();
    output.textures_delta.clear();
    let render_report = render_result?;
    let manifest = mapper_studio::headless::save_bundle(&project, &generated, destination, &cancel)?;
    println!(
        "{}",
        serde_json::json!({"format":"mapper-workbench-rail-generation","tracks":tracks,"mesh_chunks":chunks,"viewport":render_report,"manifest":manifest})
    );
    Ok(())
}
