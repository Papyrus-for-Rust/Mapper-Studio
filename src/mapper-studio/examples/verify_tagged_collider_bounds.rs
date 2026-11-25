//! Replay captured primitive collider dimensions with an explicit synthetic pose.
use papyrus_terrain::{
    decor_stream::Budget,
    environment::{Box3, Coverage, Key, Transform, Verdict, check_box, multiply, rotate},
    water_treatment_clearance::{Clearance, capsule_bounds, sphere_bounds},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, io::Read};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err("TAG_CAPTURE EXPECTED_SHA256 NEW_OUTPUT_JSON".into());
    }
    if std::path::Path::new(&args[2]).exists() {
        return Err("Existing tag-bound verification was retained".into());
    }
    let mut bytes = Vec::new();
    fs::File::open(&args[0])?
        .take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("Tag capture exceeds 32 MiB".into());
    }
    let hash = format!("{:x}", Sha256::digest(&bytes));
    if args[1].to_str() != Some(hash.as_str()) {
        return Err("Tag capture hash differs".into());
    }
    let capture: Value = serde_json::from_slice(&bytes)?;
    if capture["format"] != "mapper-monument-tag-reference" || capture["schema"] != 1 {
        return Err("Unsupported tag capture".into());
    }
    let prefabs = capture["prefabs"]
        .as_object()
        .ok_or("Missing tag prefab roster")?;
    if prefabs.is_empty() || prefabs.len() > 256 {
        return Err("Invalid tag prefab roster".into());
    }
    let fixture = Transform {
        position: [123., 17., -98.],
        rotation: multiply(
            [0., (0.37_f32).sin(), 0., (0.37_f32).cos()],
            [(0.22_f32).sin(), 0., 0., (0.22_f32).cos()],
        ),
        scale: [2., 3., 4.],
    };
    let mut results = Vec::new();
    let mut unresolved = 0;
    let mut ambiguous = 0;
    for (path, prefab) in prefabs {
        if prefab["complete"] != true {
            return Err("Incomplete authored tag capture".into());
        }
        let tags = prefab["tags"].as_array().ok_or("Missing tag roster")?;
        if tags.len() > 4096 {
            return Err("Excessive tag roster".into());
        }
        for tag in tags {
            let colliders = tag["colliders"]
                .as_array()
                .ok_or("Missing collider roster")?;
            if colliders.len() > 64 {
                return Err("Excessive collider roster".into());
            }
            ambiguous += usize::from(colliders.len() > 1);
            // Each collider is an independent numeric fixture. This deliberately
            // leaves runtime GetComponent selection and hierarchy assembly open.
            for collider in colliders {
                let kind = collider["class_id"]
                    .as_u64()
                    .ok_or("Missing collider kind")?;
                let bounds: Box3 = match kind {
                    65 => check_box(
                        [0.; 3],
                        [0., 0., 0., 1.],
                        serde_json::from_value(collider["center"].clone())?,
                        serde_json::from_value(collider["size"].clone())?,
                        fixture,
                    )?,
                    135 => {
                        let center: [f32; 3] = serde_json::from_value(collider["center"].clone())?;
                        let scaled = std::array::from_fn(|i| center[i] * fixture.scale[i]);
                        let offset = rotate(fixture.rotation, scaled);
                        sphere_bounds(
                            std::array::from_fn(|i| fixture.position[i] + offset[i]),
                            serde_json::from_value(collider["radius"].clone())?,
                            fixture.scale,
                        )?
                    }
                    136 => capsule_bounds(
                        fixture,
                        serde_json::from_value(collider["center"].clone())?,
                        serde_json::from_value(collider["radius"].clone())?,
                        serde_json::from_value(collider["height"].clone())?,
                        serde_json::from_value(collider["direction"].clone())?,
                    )?,
                    64 => {
                        unresolved += 1;
                        continue;
                    }
                    _ => return Err("Unknown captured collider kind".into()),
                };
                let id = collider["object_id"]
                    .as_i64()
                    .ok_or("Missing collider ID")?;
                let key = Key {
                    instance: 1,
                    component: id,
                };
                let clearance = Clearance::new(&[(key, bounds)], 7., Coverage::Complete)?;
                let mut work = Budget::new(1)?;
                let assessment = clearance.check(bounds.center, &mut work, || false)?;
                if assessment.verdict != Verdict::Blocked(key) {
                    return Err("Primitive center escaped its clearance volume".into());
                }
                results.push(json!({"prefab":path,"tag":tag["object_id"],"collider":id,"class_id":kind,"bounds":bounds}));
                if results.len() > 131072 {
                    return Err("Excessive primitive fixture results".into());
                }
            }
        }
    }
    let result = json!({"format":"mapper-tagged-primitive-bound-verification","schema":1,
        "source_sha256":hash,"synthetic_transform":fixture,"colliders":results,
        "runtime_mesh_bounds_required":unresolved,"tags_with_multiple_colliders":ambiguous,
        "hierarchy_transforms_applied":false,"runtime_colliders_selected":false,
        "normal_generation_integrated":false,"game_code_executed":false});
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args[2])?;
    serde_json::to_writer(&mut file, &result)?;
    file.sync_all()?;
    println!(
        "{}",
        json!({"primitive_colliders":results.len(),"runtime_mesh_bounds_required":unresolved,"ambiguous_tags":ambiguous})
    );
    Ok(())
}
