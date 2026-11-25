//! Verify offshore scene data and its prefab roster without starting generation.
use mapper_studio::{installation, offshore_source, prefab_assets};
use std::{io::Write, path::Path};
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 4 {
        return Err(
            "inspect_offshore_source <Rust root> <catalog> <offshore capture> <new report>".into(),
        );
    }
    let root = Path::new(&args[0]);
    let installation = installation::inspect(root, || false)?;
    let catalog = prefab_assets::load(root, Path::new(&args[1]), &installation, || false)?;
    let source = offshore_source::Source::load(Path::new(&args[2]), root, &catalog, || false)?;
    let stages:Vec<_>=source.stages.iter().map(|stage| {
        let roster=source.roster(stage.object_id,&catalog)?;
        Ok(serde_json::json!({"id":stage.object_id,"order":stage.order,"folders":stage.folders,
            "policy":stage.policy(10,10000)?,
            "roster":roster.iter().map(|e|serde_json::json!({"path":e.path,"id":e.prefab.id,"score":e.prefab.score})).collect::<Vec<_>>()}))
    }).collect::<Result<_,String>>()?;
    let report = serde_json::json!({"format":"papyrus-offshore-source-audit","schema":1,"catalog_sha256":catalog.sha256,
        "offshore_sha256":source.sha256,"execution_profile":"independent bounded candidate passes; no official seed parity",
        "full_stage_execution_validated":false,"candidate_search_executed":false,"stages":stages});
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args[3])
        .map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    println!(
        "Verified {} offshore stage(s): {}",
        source.stages.len(),
        Path::new(&args[3]).display()
    );
    Ok(())
}
