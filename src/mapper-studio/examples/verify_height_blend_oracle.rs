//! Compare the production height writer with isolated, owned scalar methods.
use papyrus_terrain::monument_composition::source_height_blend;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};

fn bounded(path: &Path) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("Input exceeds allowance".into());
    }
    Ok(bytes)
}
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 || Path::new(&args[2]).exists() {
        return Err("Expected QUERIES OWNED_RESULTS NEW_REPORT".into());
    }
    let raw = bounded(Path::new(&args[0]))?;
    let input: Value = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    let reference: Value =
        serde_json::from_slice(&bounded(Path::new(&args[1]))?).map_err(|e| e.to_string())?;
    if input["format"] != "mapper-height-blend-queries-1"
        || reference["format"] != "mapper-owned-height-blend-1"
        || reference["input_sha256"] != format!("{:x}", Sha256::digest(&raw))
    {
        return Err("Wrong format or input binding".into());
    }
    let queries = input["queries"].as_array().ok_or("Missing queries")?;
    let expected = reference["results"].as_array().ok_or("Missing results")?;
    if queries.is_empty() || queries.len() > 100000 || queries.len() != expected.len() {
        return Err("Invalid query count".into());
    }
    let mut mismatches = 0;
    let mut legacy_mismatches = 0;
    let mut maximum_legacy_delta = 0;
    for (q, expected) in queries.iter().zip(expected) {
        let old = q["old"]
            .as_i64()
            .filter(|v| (0..=32766).contains(v))
            .ok_or("Invalid old height")? as i16;
        let target: f32 = serde_json::from_value(q["target"].clone()).map_err(|e| e.to_string())?;
        let opacity: f32 =
            serde_json::from_value(q["opacity"].clone()).map_err(|e| e.to_string())?;
        if !target.is_finite()
            || !(0. ..=1.).contains(&target)
            || !opacity.is_finite()
            || !(0. ..=1.).contains(&opacity)
        {
            return Err("Invalid normalized input".into());
        }
        let expected = expected
            .as_i64()
            .filter(|v| (0..=32766).contains(v))
            .ok_or("Invalid expected height")? as i16;
        mismatches += usize::from(source_height_blend(old, target, opacity) != expected);
        let from = old as f32 * (1. / 32766.);
        let t = opacity * opacity * (3. - 2. * opacity);
        let prior = ((from + (target - from) * t) * 32766. + 0.5) as i16;
        legacy_mismatches += usize::from(prior != expected);
        maximum_legacy_delta =
            maximum_legacy_delta.max((i32::from(prior) - i32::from(expected)).abs());
    }
    let report = json!({"format":"mapper-height-blend-comparison-1","queries":queries.len(),
        "owned_reference":reference,"mismatches":mismatches,"legacy_mismatches":legacy_mismatches,
        "maximum_legacy_encoded_delta":maximum_legacy_delta,"passed":mismatches==0,
        "scalar_height_writer_only":true,"full_stamp_transform_validated":false,
        "full_generation_validated":false});
    serde_json::to_writer_pretty(
        File::options()
            .write(true)
            .create_new(true)
            .open(&args[2])
            .map_err(|e| e.to_string())?,
        &report,
    )
    .map_err(|e| e.to_string())?;
    println!(
        "{}",
        json!({"queries":queries.len(),"mismatches":mismatches,"legacy_mismatches":legacy_mismatches})
    );
    if mismatches != 0 {
        return Err("Height writer differs from owned methods".into());
    }
    Ok(())
}
