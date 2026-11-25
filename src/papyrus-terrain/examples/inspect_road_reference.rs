//! MAP EXPECTED_SHA256 NEW_DIRECTORY. Read-only numeric reference, never an export certificate.
use papyrus_terrain::{content, native_layers};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io::Write, path::PathBuf};

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn control_grades(path: &content::Path) -> serde_json::Value {
    let mut maximum = 0f64;
    let mut above_comparison_limit = 0usize;
    let mut vertical_segments = 0usize;
    let mut worst = None;
    for (index, pair) in path.nodes.windows(2).enumerate() {
        let run = f64::from(pair[1].x - pair[0].x).hypot(f64::from(pair[1].z - pair[0].z));
        let rise = f64::from((pair[1].y - pair[0].y).abs());
        if run == 0. {
            vertical_segments += usize::from(rise != 0.);
            continue;
        }
        let grade = rise / run;
        above_comparison_limit += usize::from(grade > 0.2);
        if grade > maximum {
            maximum = grade;
            worst = Some(
                json!({"segment":index,"horizontal_run":run,"absolute_rise":rise,
                "from":[pair[0].x,pair[0].y,pair[0].z],"to":[pair[1].x,pair[1].y,pair[1].z]}),
            );
        }
    }
    json!({"maximum":maximum,"comparison_limit":0.2,
        "segments_above_comparison_limit":above_comparison_limit,
        "vertical_segments":vertical_segments,"worst":worst,
        "source_grade_limit_inferred":false,"mesh_grades_validated":false})
}
fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        return Err("Expected MAP EXPECTED_SHA256 NEW_DIRECTORY".into());
    }
    let expected = args[1].to_str().ok_or("Invalid expected hash")?;
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Expected SHA256 must contain 64 hex digits".into());
    }
    let bytes = content::read_source(fs::File::open(&args[0]).map_err(|e| e.to_string())?)?;
    let hash = digest(&bytes);
    if !hash.eq_ignore_ascii_case(expected) {
        return Err("Reference source hash differs".into());
    }
    let map = content::read_map(&bytes, || false)?;
    let (hr, sr) = native_layers::dimensions(map.size)?;
    let layer = |name: &str| {
        map.layers
            .iter()
            .find(|l| l.name == name)
            .map(|l| l.data.as_slice())
            .ok_or_else(|| format!("Missing reference layer {name}"))
    };
    let terrain = layer("terrain")?;
    let height = layer("height")?;
    if terrain.len() != hr * hr * 2
        || height.len() != terrain.len()
        || layer("topology")?.len() != sr * sr * 4
        || layer("alpha")?.len() != sr * sr
    {
        return Err("Unexpected reference layer dimensions".into());
    }
    let mut differences = 0usize;
    let mut maximum_raise = 0i32;
    let mut maximum_lower = 0i32;
    let mut invalid_encodings = 0usize;
    for (a, b) in terrain.chunks_exact(2).zip(height.chunks_exact(2)) {
        let a = i16::from_le_bytes([a[0], a[1]]);
        let b = i16::from_le_bytes([b[0], b[1]]);
        invalid_encodings += usize::from(!(0..=32766).contains(&a) || !(0..=32766).contains(&b));
        let delta = i32::from(b) - i32::from(a);
        differences += usize::from(delta != 0);
        maximum_raise = maximum_raise.max(delta);
        maximum_lower = maximum_lower.max(-delta);
    }
    let mut families = BTreeMap::<(String, i32), (usize, usize)>::new();
    let paths = map
        .paths
        .iter()
        .enumerate()
        .map(|(index, p)| {
            let count = families.entry((p.name.clone(), p.hierarchy)).or_default();
            count.0 += 1;
            count.1 += p.nodes.len();
            json!({"index":index,"name":p.name,"hierarchy":p.hierarchy,
            "width":p.width,"spline":p.spline,"start":p.start,"end":p.end,
            "padding":[p.inner_padding,p.outer_padding],"fade":[p.inner_fade,p.outer_fade],
            "random_scale":p.random_scale,"mesh_offset":p.mesh_offset,
            "terrain_offset":p.terrain_offset,"splat":p.splat,"topology":p.topology,
            "unknown_fields":p.unknown_fields,
            "control_grade_comparison":control_grades(p),
            "nodes":p.nodes.iter().map(|n|[n.x,n.y,n.z]).collect::<Vec<_>>()})
        })
        .collect::<Vec<_>>();
    let destination = PathBuf::from(&args[2]);
    fs::create_dir(&destination).map_err(|e| e.to_string())?;
    let mut files = BTreeMap::new();
    for name in ["terrain", "height", "topology", "alpha"] {
        let data = layer(name)?;
        let filename = match name {
            "topology" => "topology.i32le",
            "alpha" => "alpha.u8",
            "terrain" => "terrain.i16le",
            _ => "height.i16le",
        };
        fs::write(destination.join(filename), data).map_err(|e| e.to_string())?;
        files.insert(filename, json!({"bytes":data.len(),"sha256":digest(data)}));
    }
    let paths = serde_json::to_vec(&paths).map_err(|e| e.to_string())?;
    fs::write(destination.join("paths.json"), &paths).map_err(|e| e.to_string())?;
    files.insert(
        "paths.json",
        json!({"bytes":paths.len(),"sha256":digest(&paths)}),
    );
    let report = json!({"format":"mapper-owned-road-reference-1","completed":true,
        "source_sha256":hash,"source_bytes":bytes.len(),"size":map.size,
        "height_resolution":hr,"surface_resolution":sr,"prefabs":map.prefabs.len(),
        "unknown_world_fields":map.unknown_fields,
        "families":families.into_iter().map(|((name,hierarchy),(count,nodes))|
            json!({"name":name,"hierarchy":hierarchy,"paths":count,"nodes":nodes})).collect::<Vec<_>>(),
        "height_vs_terrain":{"different_samples":differences,"maximum_raise_encoded":maximum_raise,
            "maximum_lower_encoded":maximum_lower,"invalid_encodings":invalid_encodings},
        "layers":map.layers.iter().map(|l|json!({"name":l.name,"bytes":l.data.len(),"sha256":digest(&l.data)})).collect::<Vec<_>>(),
        "files":files,"vanilla_provenance_verified":false,"runtime_physics_validated":false});
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination.join("reference.json"))
        .map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    println!(
        "{}",
        json!({"directory":destination,"completed":true,"different_height_samples":differences})
    );
    Ok(())
}
