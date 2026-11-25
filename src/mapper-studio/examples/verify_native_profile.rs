//! Local integration check, using a user-owned installation and a previous terrain report.
use mapper_studio::{Project, installation, native_preview, native_profile};
use sha2::{Digest, Sha256};
use std::{path::Path, process::Command, sync::atomic::AtomicBool};

fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 4 {
        return Err(
            "verify_native_profile <Rust root> <worker> <previous native-stage report> <report>"
                .into(),
        );
    }
    let root = Path::new(&args[0]);
    let worker = Path::new(&args[1]);
    let previous: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&args[2]).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let mut project = Project {
        installation: Some(installation::inspect(root, || false)?),
        preview_engine: native_preview::VERSION.into(),
        ..Default::default()
    };
    let cancel = AtomicBool::new(false);
    let legacy_key = project.terrain_key();
    let a = native_preview::generate(root, worker, &legacy_key, &cancel, |_, _| {})?
        .ok_or("Cancelled")?;
    if a.metrics["fullTerrainSha256"] != previous["first_run"]["fullTerrainSha256"]
        || a.metrics["sourceSha256"] != previous["first_run"]["sourceSha256"]
    {
        return Err("Retained profile defaults changed the previously verified terrain".into());
    }
    let profile = native_profile::resolve(&legacy_key)?;
    project.native_profile = Some(profile.clone());
    project.validate()?;
    let b = native_preview::generate(root, worker, &project.terrain_key(), &cancel, |_, _| {})?
        .ok_or("Cancelled")?;
    if a.metrics["fullTerrainSha256"] != b.metrics["fullTerrainSha256"]
        || legacy_key == project.terrain_key()
    {
        return Err(
            "Explicit unchanged profile did not preserve terrain or separate identity".into(),
        );
    }
    let original_key = project.terrain_key();
    let s = &mut project.native_profile.as_mut().unwrap().settings;
    s.biome_angle_millidegrees = 180_000;
    s.biome_weights = [10000, 0, 0, 0];
    s.jungle_weight = 0;
    let c = native_preview::generate(root, worker, &project.terrain_key(), &cancel, |_, _| {})?
        .ok_or("Cancelled")?;
    let d = native_preview::generate(root, worker, &project.terrain_key(), &cancel, |_, _| {})?
        .ok_or("Cancelled")?;
    if original_key == project.terrain_key()
        || b.metrics["fullTerrainSha256"] == c.metrics["fullTerrainSha256"]
        || c.metrics["fullTerrainSha256"] != d.metrics["fullTerrainSha256"]
    {
        return Err("Changed profile parameters are not independent/repeatable".into());
    }
    // The worker must reject malformed profiles before attempting even a nonexistent DLL.
    let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
    for case in 0..6 {
        let mut value = serde_json::to_value(&profile).map_err(|e| e.to_string())?;
        match case {
            0 => value["abi"] = "unknown-family".into(),
            1 => value["native_sha256"] = "a".repeat(64).into(),
            2 => value["settings"]["terrain_weights"] = serde_json::json!([u32::MAX, 0, 0]),
            3 => {
                value["settings"]
                    .as_object_mut()
                    .unwrap()
                    .remove("jungle_weight");
            }
            4 => value["unknown"] = true.into(),
            _ => {}
        }
        let bytes = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
        let path = temp.path().join("profile.json");
        std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
        let digest = if case == 5 {
            "0".repeat(64)
        } else {
            format!("{:x}", Sha256::digest(&bytes))
        };
        let mut command = Command::new(worker);
        command
            .arg(temp.path().join("not-a-library.dll"))
            .arg(&profile.native_sha256)
            .arg(temp.path())
            .args(["1000", "1", "1"])
            .arg(&path)
            .arg(digest);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let result = command.output().map_err(|e| e.to_string())?;
        let error = String::from_utf8_lossy(&result.stderr);
        if result.status.success()
            || error.contains("not-a-library")
            || !error.to_lowercase().contains(if case == 3 {
                "required"
            } else if case == 4 {
                "mapped"
            } else {
                "profile"
            })
        {
            return Err(format!(
                "Worker profile rejection failed for case {case}: {error}"
            ));
        }
    }
    let report = serde_json::json!({"legacy_matches_previous_layers":true,"explicit_profile_preserves_layers":true,"changed_profile_changes_layers":true,"changed_profile_repeatable":true,"worker_rejections":6,"profile":profile,"profile_sha256":profile.sha256()?,"changed_profile_sha256":project.native_profile.as_ref().unwrap().sha256()?,"legacy_layers_sha256":a.metrics["fullTerrainSha256"],"changed_layers_sha256":c.metrics["fullTerrainSha256"],"playable":false});
    std::fs::write(
        &args[3],
        serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    println!("{report}");
    Ok(())
}
