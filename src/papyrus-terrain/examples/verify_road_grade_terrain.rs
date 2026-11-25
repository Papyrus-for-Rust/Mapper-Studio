//! Apply constrained road corrections to captured terrain without launching Rust.
use papyrus_terrain::{
    path_geometry::Path,
    path_terrain::Grid,
    road_grade::Junction,
    road_grade_terrain::{self, Road},
};
use serde::Deserialize;
use serde_json::Value;
use std::{
    fs::{self, OpenOptions},
    io::{BufWriter, Write},
    path::Path as FilePath,
};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Numeric {
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
fn read(path: &FilePath) -> Result<Value, String> {
    serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}
fn number(value: &Value) -> Result<f32, String> {
    value
        .as_f64()
        .map(|v| v as f32)
        .ok_or("Missing numeric policy".into())
}
fn index(value: &Value) -> Result<usize, String> {
    value
        .as_u64()
        .and_then(|v| usize::try_from(v).ok())
        .ok_or("Missing numeric index".into())
}
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 4 {
        return Err(
            "Expected STUDY_DIRECTORY AUDITED_NUMERIC_INPUT REFERENCE_ROADS NEW_DIRECTORY".into(),
        );
    }
    let root = FilePath::new(&args[0]);
    let destination = FilePath::new(&args[3]);
    if destination.exists() {
        return Err("Output must be new".into());
    }
    let manifest = read(&root.join("manifest.json"))?;
    let study = read(&root.join("road-study.json"))?;
    let reference = read(FilePath::new(&args[2]))?;
    let numeric: Numeric =
        serde_json::from_value(read(FilePath::new(&args[1]))?).map_err(|e| e.to_string())?;
    let n = index(&manifest["height_resolution"])?;
    let sn = index(&manifest["surface_resolution"])?;
    if !(2..=4097).contains(&n) || !(2..=2048).contains(&sn) {
        return Err("Invalid study grid".into());
    }
    let raw = |name: &str, bytes: usize| -> Result<Vec<u8>, String> {
        let path = root.join(name);
        if fs::metadata(&path).map_err(|e| e.to_string())?.len() != bytes as u64 {
            return Err("Invalid layer size".into());
        }
        fs::read(path).map_err(|e| e.to_string())
    };
    let height: Vec<_> = raw("road-study-height.i16le", n * n * 2)?
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    let ground: Vec<_> = raw("after-height.i16le", n * n * 2)?
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    let topology: Vec<_> = raw("road-study-topology.i32le", sn * sn * 4)?
        .chunks_exact(4)
        .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let paths: Vec<_> = numeric
        .roads
        .iter()
        .map(|r| {
            if r.ground.len() != r.points.len() || r.fixed.len() != r.points.len() {
                return Err("Invalid audited controls".into());
            }
            Path::from_controls(&r.points, 32_000_000, || false)
                .map_err(|e| format!("Invalid path: {e:?}"))?
                .ok_or("Cancelled".into())
        })
        .collect::<Result<_, String>>()?;
    let families = reference["families"]
        .as_array()
        .ok_or("Missing road families")?;
    let shapes = study["profile"]["policy"]["families"]
        .as_array()
        .ok_or("Missing source road shapes")?;
    let modifiers = study["terrain_profile"]["families"]
        .as_array()
        .ok_or("Missing road terrain modifiers")?;
    let roads: Vec<_> = paths
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let families: Vec<_> = families
                .iter()
                .filter(|f| {
                    let start = f["first_road"].as_u64().unwrap_or(u64::MAX);
                    let count = f["road_count"].as_u64().unwrap_or(0);
                    i as u64 >= start && (i as u64) < start.saturating_add(count)
                })
                .collect();
            if families.len() != 1 {
                return Err("Missing or overlapping road family".into());
            }
            let hierarchy = &families[0]["hierarchy"];
            let shape = shapes
                .iter()
                .find(|f| &f["hierarchy"] == hierarchy)
                .ok_or("Missing family shape")?;
            let modifier = modifiers
                .iter()
                .find(|f| &f["hierarchy"] == hierarchy)
                .ok_or("Missing family modifier")?;
            Ok(Road {
                path,
                radius: number(&shape["width"])? * 0.5 * number(&shape["random_scale"])?,
                shoulder: number(&shape["padding"][1])? + number(&modifier["fade"][1])?,
            })
        })
        .collect::<Result<_, String>>()?;
    let junctions: Vec<_> = numeric
        .junctions
        .iter()
        .map(|j| Junction { a: j[0], b: j[1] })
        .collect();
    let policy = &study["terrain_profile"]["policy"];
    let protected = policy["smoothing_weights"][0]["topology_mask"]
        .as_i64()
        .ok_or("Missing protection mask")? as i32
        | policy["water_topology"]
            .as_i64()
            .ok_or("Missing water mask")? as i32;
    let result = road_grade_terrain::correct(
        road_grade_terrain::Input {
            grid: Grid {
                size: number(&manifest["size"])?,
                resolution: n,
                origin_y: number(&study["profile"]["height_origin"])?,
                vertical_span: number(&study["profile"]["vertical_span"])?,
                height: &height,
            },
            ground: &ground,
            topology: &topology,
            surface_resolution: sn,
            protected_topology: protected,
            roads: &roads,
            junctions: &junctions,
        },
        numeric.maximum_grade,
        numeric.maximum_cut_fill,
        128_000_000,
        || false,
    );
    fs::create_dir(destination).map_err(|e| e.to_string())?;
    let output = match result {
        Ok(output) => {
            let file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(destination.join("height.i16le"))
                .map_err(|e| e.to_string())?;
            let mut writer = BufWriter::new(file);
            for chunk in output.height.chunks(4096) {
                let bytes: Vec<_> = chunk.iter().flat_map(|v| v.to_le_bytes()).collect();
                writer.write_all(&bytes).map_err(|e| e.to_string())?;
            }
            writer.flush().map_err(|e| e.to_string())?;
            serde_json::json!({"format":"mapper-road-grade-terrain-study","schema":1,"applied":true,"playable":false,
                "protected_topology":protected,"report":output.report,"roads":output.roads})
        }
        Err(e) => {
            serde_json::json!({"format":"mapper-road-grade-terrain-study","schema":1,"applied":false,"playable":false,"failure":format!("{e:?}")})
        }
    };
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination.join("study.json"))
        .map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(file, &output).map_err(|e| e.to_string())?;
    println!("Applied road terrain correction: {}", output["applied"]);
    Ok(())
}
