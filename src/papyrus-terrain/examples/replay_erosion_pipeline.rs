//! Replay source-bound policies against an owned native terrain transport.
use papyrus_terrain::{erosion_pipeline, native_layers};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom, Write},
    path::Path,
};

fn verified(path: &Path, expected: &str, maximum: u64) -> Result<BufReader<File>, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1);
    }
    let mut input = BufReader::new(options.open(path).map_err(|e| e.to_string())?);
    if input.get_ref().metadata().map_err(|e| e.to_string())?.len() > maximum {
        return Err("Erosion replay input exceeds its file bound".into());
    }
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let read = input.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    if format!("{:x}", hash.finalize()) != expected {
        return Err("Erosion replay input checksum differs".into());
    }
    input.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    Ok(input)
}

fn main() -> Result<(), String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 6 {
        return Err("LAYERS POLICY_REPORT LAYERS_SHA256 REPORT_SHA256 NEW_REPORT THREADS".into());
    }
    let destination = Path::new(&args[4]);
    if destination.try_exists().map_err(|e| e.to_string())? {
        return Err("Erosion replay report must be new".into());
    }
    let workers = args[5].parse::<usize>().map_err(|e| e.to_string())?;
    if !(1..=256).contains(&workers) {
        return Err("Invalid erosion worker count".into());
    }
    let layers = verified(Path::new(&args[0]), &args[2], 1024 * 1024 * 1024)?;
    let mut layers = native_layers::read(layers, || false)?;
    let report = verified(Path::new(&args[1]), &args[3], 2 * 1024 * 1024)?;
    let report: serde_json::Value = serde_json::from_reader(report).map_err(|e| e.to_string())?;
    if report["format"] != "mapper-owned-erosion-source-verification"
        || report["frame"]["size"] != layers.size
        || report["frame"]["height_resolution"] != layers.height_resolution
        || report["plan"]["erosion"]["active"] != true
        || report["plan"]["materials"]["active"] != true
    {
        return Err("Erosion replay source phases or world dimensions differ".into());
    }
    let policy = erosion_pipeline::Policy {
        grid: serde_json::from_value(report["grid"].clone()).map_err(|e| e.to_string())?,
        bounds: serde_json::from_value(report["bounds"].clone()).map_err(|e| e.to_string())?,
        paint: serde_json::from_value(report["paint"].clone()).map_err(|e| e.to_string())?,
    };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .build()
        .map_err(|e| e.to_string())?;
    let result = erosion_pipeline::erode(
        &layers,
        policy,
        erosion_pipeline::Limits {
            maximum_bytes: 2 * 1024 * 1024 * 1024,
            maximum_work: 64_000_000_000,
        },
        &pool,
        || false,
        |iteration, total| {
            if iteration.is_multiple_of(64) || iteration == total {
                eprintln!("Erosion {iteration}/{total}");
            }
        },
    )?
    .ok_or("Erosion replay cancelled")?;
    layers.height = result.height;
    let painted = result
        .painting
        .paint(&layers, &pool, || false)?
        .ok_or("Erosion paint cancelled")?;
    let mut heights = Sha256::new();
    for height in &layers.height {
        heights.update(height.to_le_bytes());
    }
    let mut delta = Sha256::new();
    for value in result.painting.height_delta() {
        delta.update(value.to_le_bytes());
    }
    let output = serde_json::json!({
        "format": "papyrus-owned-erosion-pipeline-replay-1",
        "input_sha256": args[2], "policy_report_sha256": args[3], "workers": workers,
        "early": result.report, "materials": painted.output.report, "slopes": painted.slopes,
        "work": painted.work, "peak_buffer_bytes": painted.peak_buffer_bytes,
        "height_sha256": format!("{:x}", heights.finalize()),
        "delta_sha256": format!("{:x}", delta.finalize()),
        "splat_sha256": format!("{:x}", Sha256::digest(&painted.output.splat)),
        "early_monument_stamps_applied": false, "normal_generate_integrated": false,
    });
    let bytes = serde_json::to_vec_pretty(&output).map_err(|e| e.to_string())?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|e| e.to_string())?;
    file.write_all(&bytes).map_err(|e| e.to_string())?;
    println!("{output}");
    Ok(())
}
