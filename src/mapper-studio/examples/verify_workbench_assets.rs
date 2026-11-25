//! Explicit local end-to-end check for cache discovery, catalog targets and visible splats.
use mapper_studio::{
    Project, asset_discovery, installation, native_preview, native_processing, placement_preview,
    splat_preview,
};
use std::{path::Path, sync::atomic::AtomicBool};
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 4 {
        return Err(
            "verify_workbench_assets <Rust root> <worker> <project output> <report>".into(),
        );
    }
    let root = Path::new(&args[0]);
    let source = installation::inspect(root, || false)?;
    let (catalog, bindings) = asset_discovery::load_cached(root, &source, None, || false)?;
    let mut project = Project {
        installation: Some(source),
        preview_engine: native_preview::VERSION.into(),
        ..Default::default()
    };
    let original = project.clone();
    let added = placement_preview::populate_targets(&mut project, &catalog)?;
    if added == 0
        || project.targets[..3] != original.targets
        || project.terrain_key() != original.terrain_key()
    {
        return Err("Roster population did not preserve starter edits/base terrain".into());
    }
    project.monument_assets = Some(mapper_studio::AssetIdentity {
        catalog_sha256: catalog.sha256.clone(),
        bindings_sha256: bindings.sha256.clone(),
    });
    project.save(Path::new(&args[2]))?;
    let cancel = AtomicBool::new(false);
    let raw = native_processing::build(
        root,
        Path::new(&args[1]),
        &project.terrain_key(),
        None,
        &cancel,
        |_, _| {},
    )?
    .ok_or("Cancelled")?;
    let result =
        placement_preview::solve_native(&project, &raw.source.layers, &catalog, &bindings, || {
            false
        })?
        .ok_or("Cancelled")?;
    let mut visible = 0;
    let mut failures = Vec::new();
    for outcome in &result.outcomes {
        if outcome.position.is_none() {
            continue;
        }
        let target = project
            .targets
            .iter()
            .find(|t| t.instance_id == outcome.id)
            .unwrap();
        let asset = &catalog.entries[target.prefab_path.as_ref().unwrap()];
        match splat_preview::load(&bindings, asset, || false) {
            Ok(Some(_)) => visible += 1,
            Ok(None) => return Err("Cancelled".into()),
            Err(e) => failures.push(e),
        }
    }
    let placed = result
        .outcomes
        .iter()
        .filter(|o| o.position.is_some())
        .count();
    let maps = mapper_studio::composition::compose_layout(
        &raw.source,
        &project,
        &result,
        &catalog,
        &bindings,
        || false,
    )?
    .ok_or("Cancelled")?;
    let changed = maps
        .layers
        .height
        .iter()
        .zip(&raw.source.layers.height)
        .filter(|(a, b)| a != b)
        .count();
    let image = mapper_studio::composition::render_overview(&raw.source, &maps)?;
    let png = Path::new(&args[3]).with_extension("png");
    let mut encoder = png::Encoder::new(
        std::fs::File::create(png).map_err(|e| e.to_string())?,
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
                .flat_map(|p| p.to_array())
                .collect::<Vec<_>>(),
        )
        .map_err(|e| e.to_string())?;
    let report = serde_json::json!({"terrain_composed":true,"changed_height_samples":changed,"catalog_prefabs":catalog.entries.len(),"added_targets":added,"total_targets":project.targets.len(),"placed":placed,"visible_splats":visible,"splat_failures":failures,"required_missing":result.required_missing,"cache":raw.cache_status,"outcomes":result.outcomes.iter().map(|o|serde_json::json!({"id":o.id,"placed":o.position.is_some(),"reason":o.reason})).collect::<Vec<_>>(),"playable":false});
    std::fs::write(
        &args[3],
        serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    println!("{report}");
    if placed <= 3 || visible <= 3 {
        return Err("This fixture still has no fuller visible monument layout".into());
    }
    Ok(())
}
