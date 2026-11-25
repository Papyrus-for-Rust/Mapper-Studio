//! Read-only installed runtime-profile verification, without loading game code.
//! ROOT NEW_REPORT. Does not verify a prefab snapshot, instantiate or run physics.
use mapper_studio::{environment_runtime::Verified, installation};
use std::{fs::OpenOptions, io::Write, path::Path};
fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        return Err("ROOT NEW_REPORT".into());
    }
    let root = Path::new(&args[0]);
    let output = Path::new(&args[1]);
    if output.exists() {
        return Err("Output must be new".into());
    }
    let _scope = installation::VerificationScope::default();
    let installation = installation::inspect(root, || false)?;
    let verified = Verified::available(root, &installation, || false)?
        .ok_or("No matching environment runtime profile")?;
    let report = serde_json::json!({"format":"papyrus-environment-runtime-verification-1","profile":verified.id(),"profile_sha256":verified.fingerprint()?,"installation_kind":installation.kind,"root":verified.root(),"native":verified.native(),"game_code_loaded":false,"physics_executed":false,"prefab_snapshot_verified":false,"generation_enabled":false});
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    println!("Verified environment runtime code receipts without loading the game");
    Ok(())
}
