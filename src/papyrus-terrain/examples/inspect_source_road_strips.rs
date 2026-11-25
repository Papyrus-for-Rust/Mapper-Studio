//! SOURCE_CAPTURE_REPORT. Compare the generic strip preflight at bounded widths.
use papyrus_terrain::{path_geometry::Path, road_strip};
use serde_json::{Value, json};
use std::{fs::File, io::Read};
fn main() -> Result<(), String> {
    let file = std::env::args_os()
        .nth(1)
        .ok_or("Expected source capture report")?;
    let mut raw = Vec::new();
    File::open(file)
        .map_err(|e| e.to_string())?
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() > 64 * 1024 * 1024 {
        return Err("Capture exceeds limit".into());
    }
    let doc: Value = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    let roads = doc["roads"]["roads"]
        .as_array()
        .filter(|r| r.len() <= 256)
        .ok_or("Missing bounded roads")?;
    let families = doc["roads"]["families"]
        .as_array()
        .ok_or("Missing road families")?;
    let mut output = Vec::new();
    for (i, r) in roads.iter().enumerate() {
        let p: Vec<[f32; 3]> =
            serde_json::from_value(r["points"].clone()).map_err(|e| e.to_string())?;
        let family = families
            .iter()
            .find(|f| {
                f["first_road"].as_u64().is_some_and(|first| {
                    f["road_count"]
                        .as_u64()
                        .and_then(|n| first.checked_add(n))
                        .is_some_and(|end| (first..end).contains(&(i as u64)))
                })
            })
            .ok_or("Missing road family")?;
        let width = family["width"].as_f64().ok_or("Missing width")? as f32;
        let path = Path::from_controls(&p, 1_000_000, || false)
            .map_err(|e| format!("{e:?}"))?
            .ok_or("Cancelled")?;
        let mut rows = Vec::new();
        for factor in [0.5, 0.75, 0.875, 1.] {
            let mut work = 0usize;
            let result = road_strip::check(
                &path,
                &vec![true; p.len()],
                width * 0.5 * factor,
                |n| {
                    work += n;
                    if work > 2_000_000 {
                        Err("Strip work exceeded")
                    } else {
                        Ok(())
                    }
                },
                &|| false,
            )?;
            rows.push(json!({"radius_factor":factor,"passed":result,"work":work}));
        }
        if rows.iter().any(|r| r["passed"] != true) {
            output.push(json!({"road":i,"hierarchy":family["hierarchy"],"width":width,"controls":p.len(),"strips":rows}));
        }
    }
    println!(
        "{}",
        json!({"format":"mapper-source-strip-comparison-1","roads":roads.len(),"failures":output,
        "actual_mesh_geometry_validated":false,"runtime_physics_validated":false})
    );
    Ok(())
}
