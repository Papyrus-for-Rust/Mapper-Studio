//! Headless control-height proposal from audited numeric study inputs.
use papyrus_terrain::{
    path_geometry::Path,
    road_grade::{self, Junction, Road},
};
use serde::Deserialize;
use std::{fs, fs::OpenOptions};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    roads: Vec<Controls>,
    junctions: Vec<[(usize, usize); 2]>,
    maximum_grade: f32,
    maximum_cut_fill: f32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Controls {
    points: Vec<[f32; 3]>,
    ground: Vec<f32>,
    fixed: Vec<bool>,
}
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("Expected AUDITED_INPUT NEW_REPORT".into());
    }
    if fs::metadata(&args[0]).map_err(|e| e.to_string())?.len() > 32 * 1024 * 1024 {
        return Err("Input exceeds diagnostic budget".into());
    }
    let bytes = fs::read(&args[0]).map_err(|e| e.to_string())?;
    let input: Input = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let paths = input
        .roads
        .iter()
        .map(|r| {
            Path::from_controls(&r.points, 32_000_000, || false)
                .map_err(|e| format!("Invalid stored controls: {e:?}"))?
                .ok_or("Cancelled".into())
        })
        .collect::<Result<Vec<_>, String>>()?;
    let roads: Vec<_> = paths
        .iter()
        .zip(&input.roads)
        .map(|(path, r)| Road {
            path,
            ground: &r.ground,
            fixed: &r.fixed,
        })
        .collect();
    let junctions: Vec<_> = input
        .junctions
        .iter()
        .map(|j| Junction { a: j[0], b: j[1] })
        .collect();
    let result = road_grade::propose(
        &roads,
        &junctions,
        input.maximum_grade,
        input.maximum_cut_fill,
        32_000_000,
        || false,
    );
    let output = match result {
        Ok(p) => {
            serde_json::json!({"format":"mapper-road-grade-proposal-study","feasible":true,"report":p.report,"roads":p.roads})
        }
        Err(e) => {
            serde_json::json!({"format":"mapper-road-grade-proposal-study","feasible":false,"failure":format!("{e:?}"),"terrain_applied":false})
        }
    };
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&args[1])
        .map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(file, &output).map_err(|e| e.to_string())?;
    println!("Road control proposal feasible: {}", output["feasible"]);
    Ok(())
}
