//! CAPTURE_DIRECTORY PINNED_INFRASTRUCTURE NEW_REPORT. No native worker/game.
#[cfg(feature = "road-diagnostics")]
fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        return Err("CAPTURE_DIRECTORY PINNED_INFRASTRUCTURE NEW_REPORT".into());
    }
    let report = mapper_studio::road_route_diagnostics::replay_main_ring(
        std::path::Path::new(&args[0]),
        std::path::Path::new(&args[1]),
        std::path::Path::new(&args[2]),
    )?;
    println!(
        "{}",
        serde_json::json!({"roads":report["roads"]["roads"].as_array().map_or(0,Vec::len),"ring":report["roads"]["ring"],"constrained":report["roads"]["shaping"]["constrained"],"whole_ring_footprint":report["whole_ring_footprint"],"scope":report["scope"]})
    );
    Ok(())
}
#[cfg(not(feature = "road-diagnostics"))]
fn main() -> Result<(), String> {
    Err("Requires road-diagnostics".into())
}
