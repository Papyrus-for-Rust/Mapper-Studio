//! Export a portable generation profile from an existing project.
use mapper_studio::{Project, profile::Profile};
use std::path::Path;
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("Usage: export_profile <project.mapper> <settings.rec>".into());
    }
    let project = Project::load(Path::new(&args[0]))?;
    let output = Path::new(&args[1]);
    let name = output
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("Invalid profile filename")?
        .to_string();
    let profile = Profile::new(name, project);
    profile.save(output)?;
    if profile != Profile::load(output)? {
        return Err("Profile verification failed".into());
    }
    println!("Exported and verified {}", output.display());
    Ok(())
}
