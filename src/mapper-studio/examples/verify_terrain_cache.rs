//! Run write and read as separate processes to verify a real native disk snapshot.
use mapper_studio::{Project, installation, native_preview, native_processing, terrain_cache};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{Arc, atomic::AtomicBool},
    time::Instant,
};

fn fingerprint(source: &native_processing::Source) -> String {
    let mut hash = Sha256::new();
    for row in source.layers.height.chunks(4096) {
        hash.update(row.iter().flat_map(|h| h.to_le_bytes()).collect::<Vec<_>>());
    }
    hash.update(&source.layers.biome);
    hash.update(&source.layers.splat);
    for row in source.layers.topology.chunks(4096) {
        hash.update(row.iter().flat_map(|t| t.to_le_bytes()).collect::<Vec<_>>());
    }
    for row in source.image.pixels.chunks(513) {
        hash.update(row.iter().flat_map(|p| p.to_array()).collect::<Vec<_>>());
    }
    format!("{:x}", hash.finalize())
}

fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 4 {
        return Err("verify_terrain_cache <write|read> <Rust root> <worker> <report json>".into());
    }
    let root = Path::new(&args[1]);
    let project = Project {
        preview_engine: native_preview::VERSION.into(),
        installation: Some(installation::inspect(root, || false)?),
        ..Default::default()
    };
    let key = project.terrain_key();
    let cancel = AtomicBool::new(false);
    let report_path = Path::new(&args[3]);
    let started = Instant::now();
    if args[0] == "write" {
        let native = native_preview::generate(root, Path::new(&args[2]), &key, &cancel, |_, _| {})?
            .ok_or("Cancelled")?;
        let source = native_processing::Source {
            terrain: None,
            native_base: None,
            rivers: None,
            palette: None,
            key: key.clone(),
            image: native.image,
            heights: Arc::new(native.heights),
            layers: native.layers,
        };
        let snapshot = Instant::now();
        terrain_cache::save(
            &terrain_cache::directory().ok_or("No cache directory")?,
            &source,
            || false,
        )?;
        let save_ms = snapshot.elapsed().as_secs_f64() * 1000.;
        let report = serde_json::json!({"key":key,"grid_bytes":source.layers.bytes(),"content_sha256":fingerprint(&source),"save_ms":save_ms,"write_process_ms":started.elapsed().as_secs_f64()*1000.});
        std::fs::write(report_path, serde_json::to_vec_pretty(&report).unwrap())
            .map_err(|e| e.to_string())?;
        println!("Saved real native terrain snapshot ({save_ms:.1} ms)");
    } else if args[0] == "read" {
        let result = native_processing::build(
            root,
            Path::new("worker-intentionally-absent.exe"),
            &key,
            None,
            &cancel,
            |_, _| {},
        )?
        .ok_or("Cancelled")?;
        let restore_ms = started.elapsed().as_secs_f64() * 1000.;
        let mut report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(report_path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if !result.reused_source
            || result.cache_status != "Loaded verified terrain variant from disk"
            || serde_json::to_value(&key).unwrap() != report["key"]
            || fingerprint(&result.source) != report["content_sha256"].as_str().unwrap_or("")
        {
            return Err("Cross-process disk snapshot content mismatch".into());
        }
        report["fresh_process_reuse_without_worker"] = true.into();
        report["all_layer_and_overview_bytes_match"] = true.into();
        report["restore_ms"] = restore_ms.into();
        report["cache_status"] = result.cache_status.into();
        std::fs::write(report_path, serde_json::to_vec_pretty(&report).unwrap())
            .map_err(|e| e.to_string())?;
        println!("Restored matching terrain in fresh process without worker ({restore_ms:.1} ms)");
    } else {
        return Err("Expected write or read".into());
    }
    Ok(())
}
