//! Replay explicit, source-reviewed hydraulic inputs without loading game code.
use papyrus_terrain::{
    erosion_bounds,
    erosion_grid::{self, Input, Limits, Policy},
    erosion_splat, terrain_slope,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::PathBuf,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Capture {
    format: String,
    resolution: usize,
    heights: Vec<f32>,
    minimum_heights: Option<Vec<f32>>,
    #[serde(default)]
    bounds: Option<Bounds>,
    #[serde(default)]
    materials: Option<Materials>,
    policy: Policy,
    limits: Limits,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Bounds {
    topology_resolution: usize,
    topology: Vec<i32>,
    terrain_one_over_size: f32,
    policy: erosion_bounds::Policy,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Materials {
    topology_resolution: usize,
    topology: Vec<i32>,
    splat_resolution: usize,
    splat: Vec<u8>,
    slopes_degrees: Option<Vec<f32>>,
    #[serde(default)]
    current_height: Option<CurrentHeight>,
    terrain_one_over_size: f32,
    policy: erosion_splat::Policy,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CurrentHeight {
    size: u32,
    heights: Vec<i16>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 4 {
        return Err("INPUT_JSON EXPECTED_SHA256 NEW_DIRECTORY THREADS".into());
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1);
    }
    let mut input = options.open(&args[0])?;
    if input.metadata()?.len() > 512 * 1024 * 1024 {
        return Err("Erosion input exceeds 512 MiB".into());
    }
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    let hash = format!("{:x}", digest.finalize());
    if args[1].to_str() != Some(hash.as_str()) {
        return Err("Erosion input hash differs".into());
    }
    input.seek(SeekFrom::Start(0))?;
    let capture: Capture = serde_json::from_reader(BufReader::new(input))?;
    if capture.format != "papyrus-erosion-grid-replay-1" {
        return Err("Unsupported erosion capture".into());
    }
    let threads: usize = args[3].to_str().ok_or("Invalid thread count")?.parse()?;
    if !(1..=256).contains(&threads) {
        return Err("Thread count must be between 1 and 256".into());
    }
    let output = PathBuf::from(&args[2]);
    if output.try_exists()? {
        return Err("Output directory already exists".into());
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()?;
    if capture.minimum_heights.is_some() == capture.bounds.is_some() {
        return Err("Supply exactly one minimum-height grid or topology-bounds input".into());
    }
    let computed = capture
        .bounds
        .as_ref()
        .map(|bounds| {
            erosion_bounds::build(
                erosion_bounds::Input {
                    height_resolution: capture.resolution,
                    heights: &capture.heights,
                    topology_resolution: bounds.topology_resolution,
                    topology: &bounds.topology,
                    terrain_one_over_size: bounds.terrain_one_over_size,
                    ocean_level: capture.policy.ocean_level,
                },
                bounds.policy,
                erosion_bounds::Limits {
                    maximum_bytes: capture.limits.maximum_bytes,
                    maximum_work: usize::try_from(capture.limits.maximum_work)
                        .unwrap_or(usize::MAX),
                },
                || false,
            )
        })
        .transpose()?
        .flatten();
    let minimum = computed
        .as_ref()
        .map(|output| output.minimum_heights.as_slice())
        .or(capture.minimum_heights.as_deref())
        .ok_or("Erosion floors unavailable")?;
    let mut limits = capture.limits;
    if let Some(bounds) = &computed {
        limits.maximum_bytes = limits
            .maximum_bytes
            .checked_sub(bounds.report.retained_buffer_bytes)
            .ok_or("Erosion floors exceed the combined buffer allowance")?;
        limits.maximum_work = limits
            .maximum_work
            .checked_sub(bounds.report.work as u64)
            .ok_or("Erosion floors exceed the combined work allowance")?;
    }
    let result = erosion_grid::run(
        Input {
            resolution: capture.resolution,
            heights: &capture.heights,
            minimum_heights: minimum,
        },
        capture.policy,
        limits,
        &pool,
        || false,
        |_, _| {},
    )?
    .ok_or("Erosion replay cancelled")?;
    let work = result.report.work
        + computed
            .as_ref()
            .map_or(0, |bounds| bounds.report.work as u64);
    let retained_bytes = std::mem::size_of_val(result.heights.as_slice())
        + std::mem::size_of_val(result.height_delta.as_slice())
        + computed
            .as_ref()
            .map_or(0, |bounds| bounds.report.retained_buffer_bytes);
    let slopes = capture
        .materials
        .as_ref()
        .map(|materials| {
            if materials.slopes_degrees.is_some() == materials.current_height.is_some() {
                return Err("Supply exactly one slope grid or current encoded terrain".to_string());
            }
            materials
                .current_height
                .as_ref()
                .map(|height| {
                    terrain_slope::build(
                        terrain_slope::Input {
                            size: height.size,
                            resolution: capture.resolution,
                            heights: &height.heights,
                        },
                        terrain_slope::Limits {
                            maximum_bytes: capture
                                .limits
                                .maximum_bytes
                                .checked_sub(retained_bytes)
                                .ok_or(
                                    "Retained erosion grids exceed the combined buffer allowance",
                                )?,
                            maximum_work: usize::try_from(
                                capture
                                    .limits
                                    .maximum_work
                                    .checked_sub(work)
                                    .ok_or("Erosion exceeded the combined work allowance")?,
                            )
                            .unwrap_or(usize::MAX),
                        },
                        &pool,
                        || false,
                    )?
                    .ok_or("Current terrain slopes unavailable".to_string())
                })
                .transpose()
        })
        .transpose()?
        .flatten();
    let work = work
        + slopes
            .as_ref()
            .map_or(0, |slopes| slopes.report.work as u64);
    let paint_retained_bytes = retained_bytes
        + slopes
            .as_ref()
            .map_or(0, |slopes| slopes.report.buffer_bytes);
    let painted = capture
        .materials
        .as_ref()
        .map(
            |materials| -> Result<Option<erosion_splat::Output>, String> {
                let remaining_work = capture
                    .limits
                    .maximum_work
                    .checked_sub(work)
                    .ok_or("Erosion exceeded the combined work allowance")?;
                let remaining_bytes = capture
                    .limits
                    .maximum_bytes
                    .checked_sub(paint_retained_bytes)
                    .ok_or("Retained erosion grids exceed the combined buffer allowance")?;
                erosion_splat::paint(
                    erosion_splat::Input {
                        height_resolution: capture.resolution,
                        height_delta: &result.height_delta,
                        slopes_degrees: slopes
                            .as_ref()
                            .map(|slopes| slopes.slopes_degrees.as_slice())
                            .or(materials.slopes_degrees.as_deref())
                            .ok_or("Current terrain slopes unavailable")?,
                        topology_resolution: materials.topology_resolution,
                        topology: &materials.topology,
                        splat_resolution: materials.splat_resolution,
                        splat: &materials.splat,
                        terrain_one_over_size: materials.terrain_one_over_size,
                    },
                    materials.policy,
                    erosion_splat::Limits {
                        maximum_bytes: remaining_bytes,
                        maximum_work: usize::try_from(remaining_work).unwrap_or(usize::MAX),
                    },
                    || false,
                )
            },
        )
        .transpose()?
        .flatten();
    fs::create_dir(&output)?;
    for (name, values) in [
        ("height.f32le", result.heights.as_slice()),
        ("height-delta.f32le", result.height_delta.as_slice()),
    ]
    .into_iter()
    .chain(
        computed
            .iter()
            .map(|bounds| ("minimum-height.f32le", bounds.minimum_heights.as_slice())),
    )
    .chain(
        slopes
            .iter()
            .map(|slopes| ("slopes-degrees.f32le", slopes.slopes_degrees.as_slice())),
    ) {
        let mut file = BufWriter::new(File::create(output.join(name))?);
        for value in values {
            file.write_all(&value.to_le_bytes())?;
        }
        file.flush()?;
    }
    if let Some(materials) = &painted {
        fs::write(output.join("splat.u8"), &materials.splat)?;
    }
    let work = work
        + painted
            .as_ref()
            .map_or(0, |materials| materials.report.work as u64);
    let mut peak_buffer_bytes = computed
        .as_ref()
        .map_or(result.report.buffer_bytes, |bounds| {
            bounds
                .report
                .peak_buffer_bytes
                .max(bounds.report.retained_buffer_bytes + result.report.buffer_bytes)
        });
    peak_buffer_bytes = peak_buffer_bytes.max(paint_retained_bytes);
    if let Some(materials) = &painted {
        peak_buffer_bytes =
            peak_buffer_bytes.max(paint_retained_bytes + materials.report.peak_buffer_bytes);
    }
    let report = serde_json::json!({"input_sha256": hash, "policy": capture.policy,
        "work": work, "peak_buffer_bytes": peak_buffer_bytes, "limits": capture.limits,
        "bounds": computed.map(|output| output.report),
        "slopes": slopes.map(|output| output.report),
        "materials": painted.map(|output| output.report),
        "report": result.report, "runtime_compatibility_validated": false});
    fs::write(
        output.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{report}");
    Ok(())
}
