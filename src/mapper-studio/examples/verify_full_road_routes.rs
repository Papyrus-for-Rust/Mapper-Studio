//! CAPTURE_DIRECTORY FINAL_PREVIEW_BUNDLE NEW_REPORT. Numeric data only.
#[cfg(feature = "road-diagnostics")]
fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let planned = args.len() == 4 && args[3] == "--planned";
    if args.len() != 3 && !planned {
        return Err("CAPTURE_DIRECTORY FINAL_PREVIEW_BUNDLE NEW_REPORT [--planned]".into());
    }
    let report = mapper_studio::road_route_diagnostics::verify_with_controls(
        std::path::Path::new(&args[0]),
        std::path::Path::new(&args[1]),
        std::path::Path::new(&args[2]),
        planned,
    )?;
    println!(
        "{}",
        serde_json::json!({"roads":report["roads"],"all_routes_passed":report["all_routes_passed"],"controls":report["controls"],"report":std::path::Path::new(&args[2]),"runtime_validated":false})
    );
    if report["all_routes_passed"] != true {
        return Err(
            "Final route footprint audit rejected at least one family; see new report".into(),
        );
    }
    Ok(())
}
#[cfg(not(feature = "road-diagnostics"))]
fn main() -> Result<(), String> {
    Err("Requires the road-diagnostics Cargo feature".into())
}
