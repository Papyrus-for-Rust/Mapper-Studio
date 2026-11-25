//! CAPTURE NEW_REPORT [captured|frozen|resample|local|mesh-local|feedback|quantized] [ITERATIONS]; no game/GUI/worker.
#[cfg(feature = "road-diagnostics")]
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !(2..=4).contains(&args.len()) {
        return Err("Expected CAPTURE NEW_REPORT [captured|frozen|resample|local|mesh-local|feedback|quantized] [ITERATIONS]".into());
    }
    let mode = match args.get(2).and_then(|v| v.to_str()).unwrap_or("captured") {
        "captured" => None,
        "frozen" => Some(true),
        "resample" => Some(false),
        "local" => Some(false),
        "mesh-local" => Some(false),
        "feedback" => Some(false),
        "quantized" => Some(false),
        _ => return Err("Unknown control mode".into()),
    };
    let iterations = args
        .get(3)
        .map(|v| {
            v.to_str()
                .ok_or("Invalid iterations")?
                .parse::<usize>()
                .map_err(|_| "Invalid iterations")
        })
        .transpose()?;
    let report = if args.get(2).is_some_and(|v| v == "feedback") {
        mapper_studio::road_fit_diagnostics::replay_feedback(
            std::path::Path::new(&args[0]),
            iterations,
        )?
    } else if args.get(2).is_some_and(|v| v == "quantized") {
        mapper_studio::road_fit_diagnostics::replay_quantized(
            std::path::Path::new(&args[0]),
            iterations,
        )?
    } else if args.get(2).is_some_and(|v| v == "mesh-local") {
        mapper_studio::road_fit_diagnostics::replay_mesh_local(
            std::path::Path::new(&args[0]),
            iterations,
        )?
    } else if args.get(2).is_some_and(|v| v == "local") {
        mapper_studio::road_fit_diagnostics::replay_local(std::path::Path::new(&args[0]), iterations)?
    } else {
        mapper_studio::road_fit_diagnostics::replay(std::path::Path::new(&args[0]), mode, iterations)?
    };
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args[1])
        .map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    println!(
        "{}",
        serde_json::json!({"report":args[1].to_string_lossy(),"mesh_fit_passed":report["mesh_fit_passed"],"error":report["error"]})
    );
    Ok(())
}
#[cfg(not(feature = "road-diagnostics"))]
fn main() {
    eprintln!("Build with --features road-diagnostics");
    std::process::exit(2);
}
