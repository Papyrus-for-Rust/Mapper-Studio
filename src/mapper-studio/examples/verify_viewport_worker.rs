//! Drive the exact native viewport worker offscreen using an owned installation.
#[path = "../src/projection_panel.rs"]
// Desktop-only test seeding is unused when this worker harness is tested alone.
#[cfg_attr(test, allow(dead_code))]
mod projection_panel;
use eframe::egui;
use projection_panel::{Other, Panel, Request};
use mapper_studio::{
    Project, composition, map_image, native_processing, prefab_assets, prefab_textures,
};
use papyrus_terrain::{monument_composition, placement::Position};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::Path,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

fn number(v: &serde_json::Value, name: &str) -> Result<f32, String> {
    v[name]
        .as_f64()
        .filter(|v| v.is_finite())
        .map(|v| v as f32)
        .ok_or_else(|| format!("Missing finite placement {name}"))
}
fn png(path: &Path, image: &egui::ColorImage) -> Result<(), String> {
    let file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(
        std::io::BufWriter::new(file),
        image.size[0] as u32,
        image.size[1] as u32,
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer
        .write_image_data(
            &image
                .pixels
                .iter()
                .flat_map(egui::Color32::to_srgba_unmultiplied)
                .collect::<Vec<_>>(),
        )
        .map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())
}
fn image(ctx: &egui::Context, id: egui::TextureId) -> Result<Arc<egui::ColorImage>, String> {
    let mut delta = ctx.tex_manager().write().take_delta();
    let image = delta.set.iter().find_map(|(key, images)| {
        if *key != id {
            return None;
        }
        let egui::ImageData::Color(image) = &images.last()?.image;
        Some(image.clone())
    });
    delta.clear();
    image.ok_or("Worker did not upload its complete viewport".into())
}
fn wait(
    panel: &mut Panel,
    ctx: &egui::Context,
    make: &impl Fn([f32; 3]) -> Request,
    pose: [f32; 3],
) -> Result<Duration, String> {
    let start = Instant::now();
    loop {
        panel.begin_frame();
        panel.tick(Some(make(pose)), ctx);
        panel.end_frame(ctx);
        if panel.ready() {
            return Ok(start.elapsed());
        }
        if !panel.message().is_empty() {
            return Err(panel.message().into());
        }
        if start.elapsed() > Duration::from_secs(90) {
            return Err("Viewport worker did not finish within the diagnostic limit".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn reference(request: &Request, p: [f32; 4]) -> Result<egui::ColorImage, String> {
    let n = request.layers.surface_resolution;
    let hr = request.layers.height_resolution;
    let alpha = vec![255; n * n];
    let maps = monument_composition::compose_streamed(
        &request.layers,
        &alpha,
        || false,
        |apply| {
            for other in &request.others {
                let item = composition::prepare(
                    &request.catalog.entries[&other.path],
                    &request.bindings,
                    request.layers.size,
                    || false,
                )?
                .ok_or("Cancelled")?;
                let p = &other.position;
                if !apply(&item.stamp_for_recipe(
                    [p.x as f64, p.y as f64, p.z as f64],
                    p.yaw as f64,
                    request.project.composition_engine.as_deref(),
                ))? {
                    return Ok(false);
                }
            }
            let item = composition::prepare(
                &request.catalog.entries[&request.path],
                &request.bindings,
                request.layers.size,
                || false,
            )?
            .ok_or("Cancelled")?;
            apply(&item.stamp_for_recipe(
                [p[0] as f64, p[1] as f64, p[2] as f64],
                p[3] as f64,
                request.project.composition_engine.as_deref(),
            ))
        },
    )?
    .ok_or("Cancelled")?;
    let mask = if monument_composition::water_aware(request.project.composition_engine.as_deref()) {
        Some(
            papyrus_terrain::ocean::classify_sampled(
                n,
                |x, z| {
                    Ok(
                        maps.layers.height[z * (hr - 1) / (n - 1) * hr + x * (hr - 1) / (n - 1)]
                            <= 16383,
                    )
                },
                || false,
            )?
            .ok_or("Cancelled")?,
        )
    } else {
        None
    };
    map_image::render_with_ocean_mask(
        &maps.layers,
        Some(&maps.alpha),
        mask.as_ref(),
        request.ocean.as_ref().map(|o| o.local_water.as_slice()),
        map_image::DISPLAY_RESOLUTION,
        request.palette.as_deref(),
        || false,
    )?
    .ok_or("Cancelled".into())
}
fn compare(actual: &egui::ColorImage, expected: &egui::ColorImage) -> Result<u8, String> {
    if actual.size != expected.size {
        return Err("Viewport dimensions differ".into());
    }
    let mut maximum = 0;
    for (a, b) in actual.pixels.iter().zip(&expected.pixels) {
        for (a, b) in a.to_array().into_iter().zip(b.to_array()) {
            maximum = maximum.max(a.abs_diff(b));
        }
    }
    if maximum > 1 {
        return Err(format!(
            "Worker viewport differs from full composition by {maximum} channel bytes"
        ));
    }
    Ok(maximum)
}
/// Retain failed numerical evidence without publishing a successful verification.
fn compare_captured(
    actual: &egui::ColorImage,
    expected: &egui::ColorImage,
    stage: &Path,
    output: &Path,
    label: &str,
    size: u32,
) -> Result<u8, String> {
    png(&stage.join(format!("{label}.png")), actual)?;
    png(&stage.join(format!("{label}-reference.png")), expected)?;
    match compare(actual, expected) {
        Ok(error) => Ok(error),
        Err(error) => {
            let mut count = 0usize;
            let mut worst = Vec::new();
            for (i, (a, b)) in actual.pixels.iter().zip(&expected.pixels).enumerate() {
                let maximum = a
                    .to_array()
                    .into_iter()
                    .zip(b.to_array())
                    .map(|(a, b)| a.abs_diff(b))
                    .max()
                    .unwrap();
                if maximum <= 1 {
                    continue;
                }
                count += 1;
                let x = i % actual.size[0];
                let y = i / actual.size[0];
                worst.push((maximum,i,serde_json::json!({"pixel":[x,y],"world":[((x as f32+0.5)/actual.size[0] as f32-0.5)*size as f32,(0.5-(y as f32+0.5)/actual.size[1] as f32)*size as f32],"actual":a.to_array(),"reference":b.to_array(),"maximum_channel_error":maximum})));
                worst.sort_by_key(|(m, i, _)| (std::cmp::Reverse(*m), *i));
                worst.truncate(16);
            }
            let report = serde_json::json!({"format":"mapper-viewport-worker-failure","schema":1,"passed":false,"stage":label,"error":error,"dimensions":actual.size,"differing_pixels":count,"worst_pixels":worst.into_iter().map(|(_,_,v)|v).collect::<Vec<_>>()});
            std::fs::write(
                stage.join("failure.json"),
                serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            std::fs::rename(stage, output).map_err(|e| e.to_string())?;
            Err(error)
        }
    }
}
fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if !(9..=10).contains(&args.len()) {
        return Err("verify_viewport_worker BUNDLE RUST_ROOT CATALOG BINDINGS WORKER INSTANCE_ID MOVE_X MOVE_Z NEW_DIRECTORY [COMPOSITION_VERSION]".into());
    }
    let bundle = Path::new(&args[0]);
    let root = Path::new(&args[1]);
    let output = Path::new(&args[8]);
    if output.exists() {
        return Err("Choose a new diagnostic directory".into());
    }
    let id = args[5]
        .to_str()
        .ok_or("Invalid instance ID")?
        .parse::<u64>()
        .map_err(|e| e.to_string())?;
    let dx = args[6]
        .to_str()
        .ok_or("Invalid move X")?
        .parse::<f32>()
        .map_err(|e| e.to_string())?;
    let dz = args[7]
        .to_str()
        .ok_or("Invalid move Z")?
        .parse::<f32>()
        .map_err(|e| e.to_string())?;
    if !dx.is_finite() || !dz.is_finite() {
        return Err("Move must be finite".into());
    }
    let mut project = Project::load(&bundle.join("project.mapper"))?;
    if let Some(version) = args.get(9) {
        let version = version.to_str().ok_or("Invalid composition version")?;
        if !monument_composition::supported(version) {
            return Err("Unsupported diagnostic composition version".into());
        }
        // Numerical renderer comparison only; retained layout positions are not
        // asserted to be accepted generation under this alternate stamp kernel.
        project.composition_engine = Some(version.into());
    }
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(bundle.join("manifest.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let raw = std::fs::read(bundle.join("layout.json")).map_err(|e| e.to_string())?;
    if manifest["files"]["layout.json"]
        != serde_json::json!({"bytes":raw.len(),"sha256":format!("{:x}",Sha256::digest(&raw))})
    {
        return Err("Layout receipt mismatch".into());
    }
    let layout: serde_json::Value = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    let outcomes = layout["outcomes"]
        .as_array()
        .ok_or("Missing layout outcomes")?;
    let selected = outcomes
        .iter()
        .find(|o| o["id"].as_u64() == Some(id))
        .ok_or("Missing selected outcome")?;
    let p = &selected["position"];
    let pose = [number(p, "x")?, number(p, "z")?, number(p, "yaw")?];
    let target = project
        .targets
        .iter_mut()
        .find(|t| t.instance_id == id)
        .ok_or("Missing selected target")?;
    target.x = pose[0];
    target.z = pose[1];
    target.yaw = Some(pose[2]);
    let path = target
        .prefab_path
        .clone()
        .ok_or("Missing selected prefab")?;
    let installation = project
        .installation
        .as_ref()
        .ok_or("Missing installation")?;
    let catalog = Arc::new(prefab_assets::load(
        root,
        Path::new(&args[2]),
        installation,
        || false,
    )?);
    let bindings = Arc::new(prefab_textures::Bindings::load(
        root,
        Path::new(&args[3]),
        &catalog,
        || false,
    )?);
    let mut others = Vec::new();
    for o in outcomes {
        let other_id = o["id"].as_u64().ok_or("Invalid outcome ID")?;
        if other_id == id || o["position"].is_null() {
            continue;
        }
        let target = project
            .targets
            .iter()
            .find(|t| t.instance_id == other_id)
            .ok_or("Unknown outcome target")?;
        let p = &o["position"];
        others.push(Other {
            id: other_id,
            path: target
                .prefab_path
                .clone()
                .ok_or("Missing unchanged prefab")?,
            position: Position {
                x: number(p, "x")?,
                y: number(p, "y")?,
                z: number(p, "z")?,
                yaw: number(p, "yaw")?,
                clearance: number(p, "clearance")?,
            },
        });
    }
    let processed = native_processing::build(
        root,
        Path::new(&args[4]),
        &project.terrain_key(),
        None,
        &AtomicBool::new(false),
        |label, _| eprintln!("{label}"),
    )?
    .ok_or("Cancelled")?;
    let layers = processed.source.layers.clone();
    let source_count = Arc::strong_count(&layers);
    let make = |pose: [f32; 3]| {
        let mut project = project.clone();
        let t = project
            .targets
            .iter_mut()
            .find(|t| t.instance_id == id)
            .unwrap();
        t.x = pose[0];
        t.z = pose[1];
        t.yaw = Some(pose[2]);
        Request {
            project,
            layers: layers.clone(),
            ocean: processed.ocean.clone(),
            palette: processed.source.palette.clone(),
            catalog: catalog.clone(),
            bindings: bindings.clone(),
            id,
            path: path.clone(),
            pose,
            others: others.clone(),
        }
    };
    let ctx = egui::Context::default();
    let mut panel = Panel::default();
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let stage = tempfile::Builder::new()
        .prefix(".viewport-worker-")
        .tempdir_in(parent)
        .map_err(|e| e.to_string())?;
    let initial_wait = wait(&mut panel, &ctx, &make, pose)
        .map_err(|e| format!("Initial accepted-pose preview failed: {e}"))?;
    eprintln!("Initial accepted-pose viewport ready");
    let initial_warning = panel.message().to_owned();
    let first_id = panel.texture().unwrap().id();
    let initial = image(&ctx, first_id)?;
    let initial_pose = panel.resolved_pose().ok_or("Missing fitted pose")?;
    let initial_error = compare_captured(
        &initial,
        &reference(&make(pose), initial_pose)?,
        stage.path(),
        output,
        "initial",
        project.size,
    )?;
    let intermediate = [pose[0] + dx * 0.5, pose[1] + dz * 0.5, pose[2]];
    let final_pose = [pose[0] + dx, pose[1] + dz, pose[2]];
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(200) {
        panel.begin_frame();
        panel.tick(Some(make(intermediate)), &ctx);
        panel.end_frame(&ctx);
        std::thread::sleep(Duration::from_millis(10));
    }
    panel.tick(Some(make(final_pose)), &ctx);
    if panel.ready() {
        return Err("New pose retained a stale viewport".into());
    }
    if panel.texture().is_none() || !panel.has_paint() || panel.moving_quad().is_none() {
        return Err("Pending source stamp lost its background or moving paint".into());
    }
    let final_wait = wait(&mut panel, &ctx, &make, final_pose)
        .map_err(|e| format!("Moved requested-pose preview failed: {e}"))?;
    eprintln!("Moved requested-pose viewport ready");
    let moved_warning = panel.message().to_owned();
    if panel.texture().unwrap().id() != first_id {
        return Err("Pose update replaced its GPU texture allocation".into());
    }
    let moved = image(&ctx, first_id)?;
    let fitted = panel.resolved_pose().ok_or("Missing moved fitted pose")?;
    let moved_error = compare_captured(
        &moved,
        &reference(&make(final_pose), fitted)?,
        stage.path(),
        output,
        "moved",
        project.size,
    )?;
    if initial.pixels == moved.pixels {
        return Err("Movement did not change viewport pixels".into());
    }
    if panel.approximate() != Some(false) {
        return Err("Native diagnostic unexpectedly used approximate colors".into());
    }
    let light = panel.light_pose(id, final_pose);
    if (light[0] - fitted[0]).abs() > 0.001 || (light[1] - fitted[2]).abs() > 0.001 {
        return Err("Light preview lost its fitted offset".into());
    }
    if panel.hit_test(id, [project.size as f32, 0.]) != Some(false) {
        return Err("Off-map terrain captured stamp selection".into());
    }
    let mut visible = None;
    for z in 0..64 {
        for x in 0..64 {
            let point = [
                fitted[0] + (x as f32 / 63. * 2. - 1.) * 600.,
                fitted[2] + (z as f32 / 63. * 2. - 1.) * 600.,
            ];
            if panel.hit_test(id, point) == Some(true) {
                visible = Some(point);
            }
        }
    }
    if visible.is_none() {
        return Err("No visible stamp changes were selectable".into());
    }
    panel.tick(None, &ctx);
    if panel.ready() || panel.texture().is_some() {
        return Err("Deselection retained its projected viewport".into());
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while Arc::strong_count(&layers) > source_count && Instant::now() < deadline {
        panel.tick(None, &ctx);
        std::thread::sleep(Duration::from_millis(5));
    }
    if Arc::strong_count(&layers) != source_count {
        return Err("Cancelled viewport worker retained its source terrain".into());
    }
    let mut delta = ctx.tex_manager().write().take_delta();
    delta.clear();
    let report = serde_json::json!({"format":"mapper-viewport-worker-verification","schema":1,"selected_id":id,"prefab":path,"other_monuments":others.len(),"initial_requested":pose,"initial_fitted":initial_pose,"moved_requested":final_pose,"moved_fitted":fitted,"viewport_dimensions":moved.size,"initial_maximum_channel_error":initial_error,"moved_maximum_channel_error":moved_error,"initial_wait_ms":initial_wait.as_secs_f64()*1000.,"moved_wait_ms":final_wait.as_secs_f64()*1000.,"texture_handle_reused":true,"stale_pose_hidden":true,"visible_changes_selectable":true,"deselection_clears_projection":true,"cancelled_worker_releases_source":true,"source_cache":processed.cache_status,"desktop_windows_opened":0,"roads_scenery_in_transient_preview":false,"generation_recipe_mutated":false});
    let mut report = report;
    report["initial_placement_warning"] = initial_warning.into();
    report["moved_placement_warning"] = moved_warning.into();
    report["pending_source_paint_retained"] = true.into();
    report["composition_engine"] = project.composition_engine.clone().into();
    report["composition_version_overridden"] = (args.len() == 10).into();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(stage.path().join("report.json"))
        .map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    std::fs::rename(stage.path(), output).map_err(|e| e.to_string())?;
    println!("Verified complete viewport worker: {}", output.display());
    Ok(())
}

#[cfg(test)]
mod capture_tests {
    use super::*;
    #[test]
    fn failed_pixel_comparison_retains_images_and_never_publishes_a_success_report() {
        let directory = tempfile::tempdir().unwrap();
        let stage = tempfile::tempdir_in(directory.path()).unwrap();
        let output = directory.path().join("failure");
        let actual = egui::ColorImage::filled([2, 2], egui::Color32::WHITE);
        let expected = egui::ColorImage::filled([2, 2], egui::Color32::BLACK);
        assert!(
            compare_captured(&actual, &expected, stage.path(), &output, "initial", 1000).is_err()
        );
        assert!(!stage.path().exists());
        assert!(output.join("initial.png").is_file());
        assert!(output.join("initial-reference.png").is_file());
        assert!(!output.join("report.json").exists());
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(output.join("failure.json")).unwrap()).unwrap();
        assert_eq!(report["passed"], false);
        assert_eq!(report["differing_pixels"], 4);
        assert_eq!(report["worst_pixels"].as_array().unwrap().len(), 4);
        assert_eq!(report["worst_pixels"][0]["maximum_channel_error"], 255);
    }
}
