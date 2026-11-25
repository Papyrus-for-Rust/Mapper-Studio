//! Local source-bound noise regression; no game DLL is loaded by this process.
use mapper_studio::{installation, native_noise, native_profile};
use std::path::Path;
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 4 {
        return Err("verify_native_noise RUST_ROOT PROFILE WORKER NEW_REPORT".into());
    }
    let root = Path::new(&args[0]);
    let source = installation::inspect(root, || false)?;
    let profile = native_profile::Profile::load(Path::new(&args[1]))?;
    let mut coordinates = vec![-0., 0., 0.125, 1., -4., -1_000_000., 1_000_000.];
    coordinates.extend((0..4096).map(|i| i as f32 * 0.03125));
    let values = native_noise::sample(
        root,
        Path::new(&args[2]),
        &source,
        &profile,
        &coordinates,
        || false,
    )?
    .ok_or("Cancelled")?;
    let repeated = native_noise::sample(
        root,
        Path::new(&args[2]),
        &source,
        &profile,
        &coordinates,
        || false,
    )?
    .ok_or("Replay cancelled")?;
    let bits: Vec<_> = values.iter().map(|v| v.to_bits()).collect();
    if bits != repeated.iter().map(|v| v.to_bits()).collect::<Vec<_>>() {
        return Err("Noise replay differs".into());
    }
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args[3])
        .map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(file,&serde_json::json!({"format":"papyrus-noise1d-verification","schema":1,"native":source.native,"profile_sha256":profile.sha256()?,"coordinates":coordinates,"values":values,"bits":bits,"replay_identical":true})).map_err(|e|e.to_string())?;
    println!("{} source-bound samples; exact replay", values.len());
    Ok(())
}
