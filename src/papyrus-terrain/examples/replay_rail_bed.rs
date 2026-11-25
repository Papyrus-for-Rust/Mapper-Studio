//! Replay captured numeric rail-bed inputs without running installation generation.
use papyrus_terrain::{
    native_layers::Layers,
    path_geometry::{self, Path},
    rail_bed,
    rail_bed_fit::Join,
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::PathBuf,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Base {
    size: u32,
    terrain_seed: u32,
    biome_seed: u32,
    height_resolution: usize,
    surface_resolution: usize,
    height: Vec<i16>,
    biome: Vec<u8>,
    splat: Vec<u8>,
    topology: Vec<i32>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Geometry {
    engine: String,
    points: Vec<[f32; 3]>,
    tangents: Vec<[f32; 3]>,
    length: f32,
    step_size: f32,
    circular: bool,
    work: usize,
    min_index: usize,
    max_index: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Track {
    id: u64,
    preserve_start: bool,
    path: Geometry,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    format: String,
    base: Base,
    alpha: Vec<u8>,
    water: Option<Vec<f32>>,
    tracks: Vec<Track>,
    joins: Vec<Join>,
    policy: rail_bed::Policy,
    shape: rail_bed::Shape,
    settings: rail_bed::Settings,
    defer_surfaces: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err("CAPTURE EXPECTED_SHA256 NEW_DIRECTORY".into());
    }
    let capture = PathBuf::from(&args[0]);
    if fs::metadata(&capture)?.len() > 2 * 1024 * 1024 * 1024 {
        return Err("Capture exceeds two GiB".into());
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1);
    }
    let mut file = options.open(&capture)?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    let hash = format!("{:x}", digest.finalize());
    if args[1].to_str() != Some(hash.as_str()) {
        return Err("Capture hash differs".into());
    }
    file.seek(SeekFrom::Start(0))?;
    let input: Input = serde_json::from_reader(BufReader::new(file))?;
    if input.format != "mapper-rail-bed-replay-1" {
        return Err("Unsupported rail replay".into());
    }
    let mut paths = Vec::new();
    for track in &input.tracks {
        let saved = &track.path;
        let path = Path::from_controls(&saved.points, 32000000, || false)
            .map_err(|e| format!("Invalid captured path {e:?}"))?
            .ok_or("Replay cancelled")?;
        if saved.engine != path_geometry::VERSION
            || path.points() != saved.points
            || path.tangents() != saved.tangents
            || path.length() != saved.length
            || path.step_size() != saved.step_size
            || path.is_circular() != saved.circular
            || path.range() != (saved.min_index, saved.max_index)
            || saved.work > 32000000
        {
            return Err("Captured path reconstruction differs".into());
        }
        paths.push(path);
    }
    let tracks: Vec<_> = input
        .tracks
        .iter()
        .zip(&paths)
        .map(|(track, path)| rail_bed::Track {
            id: track.id,
            path,
            preserve_start: track.preserve_start,
        })
        .collect();
    let base = Layers {
        size: input.base.size,
        terrain_seed: input.base.terrain_seed,
        biome_seed: input.base.biome_seed,
        height_resolution: input.base.height_resolution,
        surface_resolution: input.base.surface_resolution,
        height: input.base.height,
        biome: input.base.biome,
        splat: input.base.splat,
        topology: input.base.topology,
        normalized_biome_cells: 0,
        normalized_splat_cells: 0,
    };
    let build = if input.defer_surfaces {
        rail_bed::build_terrain_with_joins
    } else {
        rail_bed::build_with_joins
    };
    let result = build(
        &base,
        &input.alpha,
        input.water.as_deref(),
        &tracks,
        &input.joins,
        &input.policy,
        input.shape,
        input.settings,
        || false,
    )?
    .ok_or("Rail replay cancelled")?;
    let output = PathBuf::from(&args[2]);
    fs::create_dir(&output)?;
    let mut heights = BufWriter::new(fs::File::create(output.join("height.i16le"))?);
    for h in &result.layers.height {
        heights.write_all(&h.to_le_bytes())?;
    }
    heights.flush()?;
    let mut track_writer = BufWriter::new(fs::File::create(output.join("tracks.json"))?);
    serde_json::to_writer(&mut track_writer, &result.tracks)?;
    track_writer.flush()?;
    let report = json!({"input_sha256":hash,"report":result.report,"tracks":result.tracks.len()});
    fs::write(
        output.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{report}");
    Ok(())
}
