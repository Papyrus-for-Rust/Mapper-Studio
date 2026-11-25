//! Read-only diagnostic of preferred monument pads on the selected native source.
use mapper_studio::{Project, installation, native_preview, native_processing, placement_preview};
use papyrus_terrain::preparation::{self, Footprint};
use std::{path::Path, sync::atomic::AtomicBool};
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !(3..=4).contains(&args.len()) {
        return Err(
            "assess_native_preparation <Rust root> <worker> <report json> [verified catalog]"
                .into(),
        );
    }
    let root = Path::new(&args[0]);
    let project = Project {
        preview_engine: native_preview::VERSION.into(),
        installation: Some(installation::inspect(root, || false)?),
        ..Default::default()
    };
    let assets = args
        .get(3)
        .map(|path| {
            mapper_studio::prefab_assets::load(
                root,
                Path::new(path),
                project.installation.as_ref().unwrap(),
                || false,
            )
        })
        .transpose()?;
    let cancel = AtomicBool::new(false);
    let source = native_processing::build(
        root,
        Path::new(&args[1]),
        &project.terrain_key(),
        None,
        &cancel,
        |_, _| {},
    )?
    .ok_or("Unexpected cancellation")?
    .source;
    let catalog = placement_preview::templates();
    let mut reports = Vec::new();
    for target in &project.targets {
        let entry = &catalog
            .iter()
            .find(|e| Some(&e.path) == target.prefab_path.as_ref())
            .ok_or("Missing catalog estimate")?
            .entry;
        let footprint = Footprint {
            x: target.x,
            z: target.z,
            yaw: entry.minimum_yaw,
            extent_x: entry.extent_x,
            extent_z: entry.extent_z,
            fade: 24.,
            maximum_change: 20.,
        };
        let bounds = assets
            .as_ref()
            .and_then(|a| {
                target
                    .prefab_path
                    .as_ref()
                    .and_then(|path| a.entries.get(path))
            })
            .and_then(|a| a.bounds.as_ref());
        let footprint = if let Some(bounds) = bounds {
            if project.size < bounds.minimum_world_size {
                return Err("World smaller than authored monument minimum".into());
            }
            let extent = bounds.horizontal_extent();
            Footprint {
                extent_x: extent[0],
                extent_z: extent[1],
                ..footprint
            }
        } else {
            footprint
        };
        let basis = if bounds.is_some() {
            "source-verified authored bounds; terrain stamps not applied"
        } else {
            "independent catalog estimate; real prefab stamps not loaded"
        };
        let a = preparation::assess(
            &source.layers.height,
            source.layers.height_resolution,
            source.layers.size as usize,
            footprint,
            || false,
        )?
        .ok_or("Unexpected cancellation")?;
        reports.push(serde_json::json!({"id":target.instance_id,"name":target.name,"location_basis":"preferred location","footprint_source":basis,"position":[target.x,target.z],"yaw":footprint.yaw,"extent":[footprint.extent_x,footprint.extent_z],"fade":footprint.fade,"maximum_change":footprint.maximum_change,"plane_height":a.plane_height,"minimum_height":a.minimum_height,"maximum_height":a.maximum_height,"maximum_cut":a.maximum_cut,"maximum_fill":a.maximum_fill,"cut_volume_estimate":a.cut_volume,"fill_volume_estimate":a.fill_volume,"sampled_cells":a.sampled_cells,"low_core_samples":a.low_core_samples,"submerged_blend_samples":a.submerged_blend_samples,"within_budget":a.within_budget}));
    }
    let report = serde_json::json!({"assessment_engine":preparation::VERSION,"source":project.terrain_key().engine,"terrain_seed":project.seeds[0].value,"catalog_sha256":assets.as_ref().map(|a| &a.sha256),"read_only":true,"assessments":reports});
    std::fs::write(
        &args[2],
        serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    println!("{report}");
    Ok(())
}
