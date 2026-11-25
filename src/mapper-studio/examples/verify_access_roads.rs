//! Local road-stage check on an already receipted native composition; no texture scan.
use mapper_studio::{Project, infrastructure, prefab_assets, rail_preview::Terrain};
use papyrus_terrain::{
    monument_composition::Composed,
    placement::{Outcome, Position, Solution},
};
use std::path::Path;
fn main() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let legacy_exits = args.last().is_some_and(|s| s == "--legacy-exits");
    if legacy_exits {
        args.pop();
    }
    let recover = args.last().is_some_and(|s| s == "--ring-recovery");
    let legacy = args.last().is_some_and(|s| s == "--no-ring-recovery");
    if recover || legacy {
        args.pop();
    }
    if ![5, 6, 8].contains(&args.len()) {
        return Err(
            "verify_access_roads BUNDLE RUST_ROOT CATALOG INFRASTRUCTURE NEW_REPORT [NEW_LAYERS_DIR [SURFACE_SHA256 WORKER]] [--ring-recovery | --no-ring-recovery]".into(),
        );
    }
    let bundle = Path::new(&args[0]);
    let root = Path::new(&args[1]);
    let mut project = Project::load(&bundle.join("project.mapper"))?;
    let terrain = Terrain::load(bundle, &project, || false)?;
    let (layers, alpha) = terrain.bed_layers(bundle, || false)?;
    let catalog = prefab_assets::load(
        root,
        Path::new(&args[2]),
        project
            .installation
            .as_ref()
            .ok_or("Missing installation")?,
        || false,
    )?;
    let source = infrastructure::Source::load(Path::new(&args[3]), &catalog, || false)?;
    project.roads = Some(infrastructure::Recipe::for_source(&source));
    if legacy_exits {
        project.roads.as_mut().unwrap().access_search = None;
    }
    if legacy {
        project.roads.as_mut().unwrap().ring_search = None;
    }
    if recover {
        project.roads.as_mut().unwrap().ring_search =
            Some(papyrus_terrain::road_ring::RECOVERY_VERSION.into());
    }
    if args.len() == 8 {
        project.roads.as_mut().unwrap().surface_sha256 =
            Some(args[6].to_string_lossy().into_owned());
    }
    let layout: serde_json::Value = serde_json::from_slice(
        &std::fs::read(bundle.join("layout.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let mut outcomes = Vec::new();
    for row in layout["outcomes"].as_array().ok_or("Missing outcomes")? {
        let pos = &row["position"];
        outcomes.push(Outcome {
            id: row["id"].as_u64().ok_or("Invalid id")?,
            position: if pos.is_null() {
                None
            } else {
                Some(Position {
                    x: pos["x"].as_f64().ok_or("Missing x")? as f32,
                    y: pos["y"].as_f64().ok_or("Missing y")? as f32,
                    z: pos["z"].as_f64().ok_or("Missing z")? as f32,
                    yaw: pos["yaw"].as_f64().ok_or("Missing yaw")? as f32,
                    clearance: pos["clearance"].as_f64().ok_or("Missing clearance")? as f32,
                })
            },
            distance: row["distance"].as_f64().map(|v| v as f32),
            within_radius: row["within_radius"].as_bool().unwrap_or(false),
            reason: row["reason"].as_str().unwrap_or("").into(),
        });
    }
    let solution = Solution {
        outcomes,
        required_missing: layout["required_missing"].as_u64().unwrap_or(0) as usize,
        attempts: layout["attempts"].as_u64().unwrap_or(0) as usize,
    };
    let mut maps = Composed {
        custom_baked: Vec::new(),
        rivers: None,
        rails: None,
        gameplay_height: None,
        objects: None,
        dungeons: None, tunnels: None,
        offshore: None,
        clutter_topology: None,
        cliff_topology: None,
        layers,
        alpha,
        clamped_height_samples: 0,
        ocean: None,
        roads: None,
        decoration: None,
        spawn_audit: None,
        topology_before_overrides: None,
        topology_overrides: None,
    };
    let mut report = infrastructure::generate(
        &project,
        &solution,
        &catalog,
        &source,
        root,
        &mut maps,
        || false,
    )?
    .ok_or("Road stage cancelled")?;
    if args.len() == 8 {
        let surface = mapper_studio::road_surface_source::Source::cached(
            root,
            &args[6].to_string_lossy(),
            &source,
            || false,
        )?;
        report.surfaces = Some(
            mapper_studio::road_surface_stage::generate(
                mapper_studio::road_surface_stage::Context {
                    project: &project,
                    root,
                    worker: Path::new(&args[7]),
                    layers: &maps.layers,
                },
                &report,
                &surface,
                || false,
            )?
            .ok_or("Surface stage cancelled")?,
        );
    }
    let output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(Path::new(&args[4]))
        .map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(output, &report.json()).map_err(|e| e.to_string())?;
    if let Some(path) = args.get(5) {
        let path = Path::new(path);
        std::fs::create_dir(path).map_err(|e| e.to_string())?;
        if let Some(surface) = &report.surfaces {
            for chunk in &surface.chunks {
                std::fs::write(
                    path.join(chunk.filename()),
                    serde_json::to_vec(chunk).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
            }
        }
        for (name, bytes) in [
            (
                "height.bin",
                maps.layers
                    .height
                    .iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect::<Vec<_>>(),
            ),
            ("splat.bin", maps.layers.splat.clone()),
            (
                "topology.bin",
                maps.layers
                    .topology
                    .iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect(),
            ),
            ("alpha.bin", maps.alpha.clone()),
        ] {
            std::fs::write(path.join(name), bytes).map_err(|e| e.to_string())?;
        }
        let ocean = papyrus_terrain::ocean::reconstruct(
            &maps.layers.height,
            maps.layers.height_resolution,
            &maps.layers.topology,
            maps.layers.surface_resolution,
            || false,
        )?
        .ok_or("Ocean reconstruction cancelled")?;
        let image = mapper_studio::map_image::render(
            &maps.layers,
            Some(&maps.alpha),
            Some(&ocean),
            mapper_studio::map_image::DISPLAY_RESOLUTION,
            || false,
        )?
        .ok_or("Map rendering cancelled")?;
        mapper_studio::image_export::write_png(&path.join("overview.png"), &image, || false)?;
    }
    println!(
        "{} corridors, {} unresolved groups, {} rejected candidates",
        report.roads.len(),
        report.unresolved.len(),
        report.rejected
    );
    Ok(())
}
