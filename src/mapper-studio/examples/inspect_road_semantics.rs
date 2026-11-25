use std::path::Path;
fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        return Err("inspect_road_semantics RUST_ROOT CATALOG INFRASTRUCTURE".into());
    }
    let root = Path::new(&args[0]);
    let installation = mapper_studio::installation::inspect(root, || false)?;
    let catalog =
        mapper_studio::prefab_assets::load(root, Path::new(&args[1]), &installation, || false)?;
    let source = mapper_studio::infrastructure::Source::load(Path::new(&args[2]), &catalog, || false)?;
    source.verify_installation(root, || false)?;
    println!(
        "{}",
        serde_json::json!({"format":"mapper-road-semantics-capture","schema":1,"infrastructure_sha256":source.sha256,"semantic_sha256":source.semantic_sha256()?,"revision":source.profile.revision})
    );
    Ok(())
}
