//! Reproduce circular shaping from an exported local road report.
use std::{io::Read, path::Path};
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("inspect_road_ring_shape ROAD_REPORT NEW_JSON".into());
    }
    let mut raw = Vec::new();
    std::fs::File::open(&args[0])
        .map_err(|e| e.to_string())?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() > 16 * 1024 * 1024 {
        return Err("Road report too large".into());
    }
    let doc: serde_json::Value = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    let points: Vec<[f32; 3]> =
        serde_json::from_value(doc["roads"][0]["points"].clone()).map_err(|e| e.to_string())?;
    let p = doc.get("shaping_policy").ok_or("Missing shaping policy")?;
    let path = papyrus_terrain::path_geometry::build(
        &points,
        papyrus_terrain::path_geometry::Settings {
            horizontal_iterations: p["horizontal_iterations"]
                .as_u64()
                .ok_or("Missing horizontal passes")? as u16,
            vertical_iterations: p["vertical_iterations"]
                .as_u64()
                .ok_or("Missing vertical passes")? as u16,
            resample_distance: p["resample_distance"].as_f64().ok_or("Missing spacing")? as f32,
            maximum_work: p["maximum_work"].as_u64().ok_or("Missing work allowance")? as usize,
        },
        None,
        || false,
    )
    .map_err(|e| format!("{e:?}"))?
    .ok_or("Cancelled")?;
    let mut shaped = path.points().to_vec();
    let before_close = shaped.last().copied();
    if shaped.first() != shaped.last() {
        shaped.push(shaped[0]);
    }
    let mut grades = Vec::new();
    for (i, w) in shaped.windows(2).enumerate() {
        let run = (w[0][0] - w[1][0]).hypot(w[0][2] - w[1][2]);
        let rise = (w[0][1] - w[1][1]).abs();
        if run < 1e-4 || rise > 0.2 * run + 1e-5 {
            grades.push(serde_json::json!({"segment":i,"from":w[0],"to":w[1],"run":run,"rise":rise,"grade":(run>0.).then_some(rise/run)}));
        }
    }
    let result = serde_json::json!({"points":shaped,"first":path.points().first(),"last_before_closing":before_close,"failed_segments":grades});
    let output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(Path::new(&args[1]))
        .map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(output, &result).map_err(|e| e.to_string())?;
    println!(
        "{} failed segments; first: {}",
        grades.len(),
        grades.first().unwrap_or(&serde_json::Value::Null)
    );
    Ok(())
}
