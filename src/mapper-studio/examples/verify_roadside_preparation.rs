use std::path::Path;
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !(2..=3).contains(&args.len()) {
        return Err("verify_roadside_preparation RUST_ROOT CATALOG [SOURCE]".into());
    }
    let root = Path::new(&args[0]);
    let installation = mapper_studio::installation::inspect(root, || false)?;
    let catalog =
        mapper_studio::prefab_assets::load(root, Path::new(&args[1]), &installation, || false)?;
    let source = if let Some(path) = args.get(2) {
        mapper_studio::roadside_source::Source::load(Path::new(path), root, &catalog, || false)?
    } else {
        mapper_studio::roadside_source::prepare(root, &catalog, || false)?
            .ok_or("No matched roadside adapter")?
    };
    let context = papyrus_terrain::monument_stage::Context {
        realm_mask: 2,
        cached: false,
        nexus: false,
    };
    let stage = source.stage();
    println!(
        "{}",
        serde_json::json!({"format":"mapper-roadside-preparation-verification","schema":1,
        "source_sha256":source.sha256,"prefabs":source.prefabs().iter().map(|(path,p)|serde_json::json!({
            "path":path,"priority":p.parameters.priority,"count":p.parameters.count,
            "ordered_road_sockets":p.road_connectors(source.profile()).map(|c|serde_json::json!({"object_id":c.object_id,"owner":c.game_object_id})).collect::<Vec<_>>()
        })).collect::<Vec<_>>(),"mode":stage.road_mode()?,"runs_on_server":stage.runs(4000,context)?,
        "runs_on_world_cache":stage.runs(4000,papyrus_terrain::monument_stage::Context {cached:true,..context})?,
        "target_count":stage.target_count,"same_type_spacing":stage.minimum_distance_same_type,
        "different_type_spacing":stage.minimum_distance_different_type,"filter":stage.filter,
        "sample_spacing":source.profile().sample_spacing,"end_padding":source.profile().end_padding,
        "group_attempts":source.profile().group_attempts,"individual_candidates":source.profile().individual_candidates,
        "placement_enabled":false,"desktop_windows_opened":0})
    );
    Ok(())
}
