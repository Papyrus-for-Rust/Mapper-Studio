//! New-only numeric/PNG evidence for the intermediate roadside stamp stage.
use mapper_studio::{native_processing::Source, roadside_stamps::StampedStage};
use papyrus_terrain::monument_composition::Composed;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Write, path::Path};
fn write_chunks(
    directory: &Path,
    name: &str,
    chunks: impl Iterator<Item = Vec<u8>>,
) -> Result<serde_json::Value, String> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join(name))
        .map_err(|e| e.to_string())?;
    let mut writer = std::io::BufWriter::new(file);
    let mut hash = Sha256::new();
    let mut bytes = 0;
    for chunk in chunks {
        writer.write_all(&chunk).map_err(|e| e.to_string())?;
        hash.update(&chunk);
        bytes += chunk.len();
    }
    writer.flush().map_err(|e| e.to_string())?;
    writer.get_ref().sync_all().map_err(|e| e.to_string())?;
    Ok(serde_json::json!({"bytes":bytes,"sha256":format!("{:x}",hash.finalize())}))
}
pub fn publish(
    destination: &Path,
    source: &Source,
    before: &Composed,
    stage: &StampedStage,
    road_paint: &super::road_paint_study::Study,
) -> Result<serde_json::Value, String> {
    if destination.exists() {
        return Err("Stamp artifacts must be new".into());
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temp = tempfile::Builder::new()
        .prefix(".roadside-stamp-study-")
        .tempdir_in(parent)
        .map_err(|e| e.to_string())?;
    let directory = temp.path();
    let mut files = BTreeMap::new();
    for (prefix, maps) in [("before", before), ("after", &stage.maps)] {
        let name = format!("{prefix}-height.i16le");
        files.insert(
            name.clone(),
            write_chunks(
                directory,
                &name,
                maps.layers
                    .height
                    .chunks(4096)
                    .map(|c| c.iter().flat_map(|v| v.to_le_bytes()).collect()),
            )?,
        );
        let name = format!("{prefix}-topology.i32le");
        files.insert(
            name.clone(),
            write_chunks(
                directory,
                &name,
                maps.layers
                    .topology
                    .chunks(4096)
                    .map(|c| c.iter().flat_map(|v| v.to_le_bytes()).collect()),
            )?,
        );
        for (kind, bytes) in [
            ("splat", &maps.layers.splat),
            ("biome", &maps.layers.biome),
            ("alpha", &maps.alpha),
        ] {
            let name = format!("{prefix}-{kind}.u8");
            files.insert(
                name.clone(),
                write_chunks(directory, &name, bytes.chunks(65536).map(<[u8]>::to_vec))?,
            );
        }
        if let Some(ocean) = &maps.ocean {
            let name = format!("{prefix}-local-water.i16le");
            files.insert(
                name.clone(),
                write_chunks(
                    directory,
                    &name,
                    ocean
                        .local_water
                        .chunks(4096)
                        .map(|c| c.iter().flat_map(|v| v.to_le_bytes()).collect()),
                )?,
            );
            let name = format!("{prefix}-ocean.u8");
            files.insert(
                name.clone(),
                write_chunks(
                    directory,
                    &name,
                    ocean.classification.chunks(65536).map(<[u8]>::to_vec),
                )?,
            );
        }
        let image = mapper_studio::map_image::render_with_palette(
            &maps.layers,
            Some(&maps.alpha),
            maps.ocean.as_ref(),
            2048,
            source.palette.as_deref(),
            || false,
        )?
        .ok_or("Study image cancelled")?;
        let name = format!("{prefix}.png");
        if !mapper_studio::image_export::write_png(&directory.join(&name), &image, || false)? {
            return Err("Study PNG cancelled".into());
        }
        let raw = std::fs::read(directory.join(&name)).map_err(|e| e.to_string())?;
        files.insert(
            name,
            serde_json::json!({"bytes":raw.len(),"sha256":format!("{:x}",Sha256::digest(&raw))}),
        );
    }
    let report = serde_json::to_vec_pretty(&stage.report).map_err(|e| e.to_string())?;
    files.insert(
        "stage.json".into(),
        write_chunks(directory, "stage.json", std::iter::once(report))?,
    );
    files.insert(
        "road-study-splat.u8".into(),
        write_chunks(
            directory,
            "road-study-splat.u8",
            road_paint
                .maps
                .layers
                .splat
                .chunks(65536)
                .map(<[u8]>::to_vec),
        )?,
    );
    files.insert(
        "road-study-topology.i32le".into(),
        write_chunks(
            directory,
            "road-study-topology.i32le",
            road_paint
                .maps
                .layers
                .topology
                .chunks(4096)
                .map(|c| c.iter().flat_map(|v| v.to_le_bytes()).collect()),
        )?,
    );
    files.insert(
        "road-study-height.i16le".into(),
        write_chunks(
            directory,
            "road-study-height.i16le",
            road_paint
                .maps
                .layers
                .height
                .chunks(4096)
                .map(|c| c.iter().flat_map(|v| v.to_le_bytes()).collect()),
        )?,
    );
    if let Some(ocean) = &road_paint.maps.ocean {
        files.insert(
            "road-study-ocean.u8".into(),
            write_chunks(
                directory,
                "road-study-ocean.u8",
                ocean.classification.chunks(65536).map(<[u8]>::to_vec),
            )?,
        );
    }
    let report = serde_json::to_vec_pretty(&road_paint.report).map_err(|e| e.to_string())?;
    files.insert(
        "road-study.json".into(),
        write_chunks(directory, "road-study.json", std::iter::once(report))?,
    );
    let image = mapper_studio::map_image::render_with_palette(
        &road_paint.maps.layers,
        Some(&road_paint.maps.alpha),
        road_paint.maps.ocean.as_ref(),
        2048,
        source.palette.as_deref(),
        || false,
    )?
    .ok_or("Road study image cancelled")?;
    if !mapper_studio::image_export::write_png(&directory.join("road-study.png"), &image, || false)? {
        return Err("Road study PNG cancelled".into());
    }
    let raw = std::fs::read(directory.join("road-study.png")).map_err(|e| e.to_string())?;
    files.insert(
        "road-study.png".into(),
        serde_json::json!({"bytes":raw.len(),"sha256":format!("{:x}",Sha256::digest(&raw))}),
    );
    let manifest = serde_json::json!({"format":"mapper-roadside-stamp-study","schema":1,"playable":false,"intermediate":true,
        "size":before.layers.size,"height_resolution":before.layers.height_resolution,"surface_resolution":before.layers.surface_resolution,
        "layout":"planar channel-major bytes; rows advance toward world +Z","files":files});
    let raw = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    let receipt = write_chunks(directory, "manifest.json", std::iter::once(raw))?;
    std::fs::rename(directory, destination).map_err(|e| e.to_string())?;
    Ok(receipt)
}
