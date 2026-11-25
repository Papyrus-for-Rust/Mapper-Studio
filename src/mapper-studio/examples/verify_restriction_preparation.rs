//! Exercise normal restriction capture and warm reuse with an owned installation.
use std::{io::Write, path::Path, time::Instant};

fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        return Err("verify_restriction_preparation RUST_ROOT PROJECT NEW_REPORT".into());
    }
    let root = Path::new(&args[0]);
    let project = mapper_studio::Project::load(Path::new(&args[1]))?;
    let source = project
        .installation
        .as_ref()
        .ok_or("Missing installation")?;
    if Path::new(&args[2]).exists() {
        return Err("Restriction verification report must be new".into());
    }
    let started = Instant::now();
    let catalog = mapper_studio::no_build::prepare(root, source, || false)?;
    let capture_seconds = started.elapsed().as_secs_f64();
    let started = Instant::now();
    let reused = mapper_studio::no_build::prepare(root, source, || false)?;
    if reused.sha256 != catalog.sha256 {
        return Err("Warm restriction reuse changed the catalog".into());
    }
    let warm_seconds = started.elapsed().as_secs_f64();
    let mut colliders = 0;
    let mut reports = 0;
    let mut incomplete = 0;
    for report in catalog
        .entries
        .values()
        .filter_map(|a| a.no_build_geometry.as_ref())
    {
        report.validate()?;
        reports += 1;
        colliders += report.volumes.len();
        incomplete += usize::from(!report.complete);
    }
    if reports == 0 || colliders == 0 {
        return Err("Restriction capture has no collider geometry".into());
    }
    let cancelled = mapper_studio::no_build::prepare(root, source, || true).is_err();
    if !cancelled {
        return Err("Cancelled restriction capture was accepted".into());
    }
    let report = serde_json::json!({
        "format":"mapper-restriction-preparation-verification", "schema":1,
        "catalog_sha256":catalog.sha256, "sources":catalog.sources,
        "prefabs":catalog.entries.len(), "restriction_reports":reports,
        "colliders":colliders, "incomplete_reports":incomplete,
        "capture_seconds":capture_seconds, "warm_seconds":warm_seconds,
        "warm_catalog_identical":true, "cancellation_rejected":cancelled,
    });
    std::fs::File::create_new(Path::new(&args[2]))
        .and_then(|mut file| file.write_all(&serde_json::to_vec_pretty(&report).unwrap()))
        .map_err(|e| e.to_string())?;
    println!("{report}");
    Ok(())
}
