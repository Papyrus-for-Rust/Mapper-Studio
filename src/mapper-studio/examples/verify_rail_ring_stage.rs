//! Replay the real pre-rail composition through combined and separated entry points.
use mapper_studio::{Project, asset_discovery, composition, native_processing, rail_generation};
use papyrus_terrain::{
    monument_composition::Composed,
    placement::{Outcome, Position, Solution},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{io::Write, path::Path, sync::atomic::AtomicBool};

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn state(maps: &Composed) -> Value {
    let height = maps
        .layers
        .height
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect::<Vec<_>>();
    let topology = maps
        .layers
        .topology
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect::<Vec<_>>();
    let rails = maps.rails.as_ref().map(|scene| {
        json!({"report":scene.report,"tracks":scene.tracks,
        "reservations_sha256":hash(&scene.reservations.raster)})
    });
    json!({"height_sha256":hash(&height),"topology_sha256":hash(&topology),
        "biome_sha256":hash(&maps.layers.biome),"splat_sha256":hash(&maps.layers.splat),
        "alpha_sha256":hash(&maps.alpha),"rails":rails})
}
fn layout(bundle: &Path) -> Result<Solution, String> {
    let path = bundle.join("layout.json");
    if std::fs::metadata(&path).map_err(|e| e.to_string())?.len() > 4 * 1024 * 1024 {
        return Err("Rail fixture layout exceeds its bound".into());
    }
    let raw = std::fs::read(path).map_err(|e| e.to_string())?;
    let value: Value = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    let outcomes = value["outcomes"]
        .as_array()
        .filter(|rows| rows.len() <= 128)
        .ok_or("Invalid layout outcomes")?
        .iter()
        .map(|row| {
            let p = &row["position"];
            let number = |name: &str| {
                p[name]
                    .as_f64()
                    .map(|v| v as f32)
                    .filter(|v| v.is_finite())
                    .ok_or_else(|| format!("Invalid layout {name}"))
            };
            Ok(Outcome {
                id: row["id"].as_u64().ok_or("Missing layout identity")?,
                position: if p.is_null() {
                    None
                } else {
                    Some(Position {
                        x: number("x")?,
                        y: number("y")?,
                        z: number("z")?,
                        yaw: number("yaw")?,
                        clearance: number("clearance")?,
                    })
                },
                distance: row["distance"].as_f64().map(|v| v as f32),
                within_radius: row["within_radius"]
                    .as_bool()
                    .ok_or("Missing radius result")?,
                reason: row["reason"]
                    .as_str()
                    .ok_or("Missing layout reason")?
                    .into(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Solution {
        outcomes,
        required_missing: value["required_missing"]
            .as_u64()
            .ok_or("Missing required count")? as usize,
        attempts: value["attempts"].as_u64().ok_or("Missing attempt count")? as usize,
    })
}
fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 4 {
        return Err("Expected RUST_ROOT EXPORTED_BUNDLE WORKER NEW_REPORT".into());
    }
    let root = Path::new(&args[0]);
    let bundle = Path::new(&args[1]);
    let output = Path::new(&args[3]);
    if output.exists() {
        return Err("Rail stage report must be new".into());
    }
    let _verification = mapper_studio::installation::VerificationScope::default();
    let project = Project::load(&bundle.join("project.mapper"))?;
    let layout = layout(bundle)?;
    let (catalog, bindings) = asset_discovery::prepare(
        root,
        project
            .installation
            .as_ref()
            .ok_or("Missing installation")?,
        project.monument_assets.as_ref(),
        || false,
    )?;
    let processed = native_processing::build(
        root,
        Path::new(&args[2]),
        &project.terrain_key(),
        None,
        &AtomicBool::new(false),
        |_, _| {},
    )?
    .ok_or("Native fixture preparation cancelled")?;
    let compose = || {
        composition::compose_monuments(
            &processed.source,
            &project,
            &layout,
            &catalog,
            &bindings,
            || false,
        )?
        .ok_or_else(|| "Monument fixture composition cancelled".to_string())
    };
    let mut split = compose()?;
    let initial = state(&split);
    let plan =
        rail_generation::prepare_ring(&project, &layout, &catalog, &bindings, &split, || false)?
            .ok_or("Ring checkpoint cancelled")?;
    let ring_sha256 = hash(&serde_json::to_vec(plan.path()).map_err(|e| e.to_string())?);
    let ring_samples = plan.path().points().len();
    if state(&split) != initial || split.rails.is_some() {
        return Err("Ring checkpoint published terrain or rails".into());
    }
    if split.layers.height[0] >= 16383 {
        return Err("Fixture corner requires submerged terrain".into());
    }
    split.layers.height[0] += 1;
    let retained_corner = split.layers.height[0];
    let _split_pending = rail_generation::resume(plan, &project, &layout, &mut split, || false)?
        .ok_or("Resumed rail stage cancelled")?;
    if split.layers.height[0] != retained_corner {
        return Err("Resumed rails discarded intervening terrain edit".into());
    }
    let mut combined = compose()?;
    combined.layers.height[0] += 1;
    let _combined_pending = rail_generation::prepare(
        &project,
        &layout,
        &catalog,
        &bindings,
        &mut combined,
        || false,
    )?
    .ok_or("Combined rail stage cancelled")?;
    let final_state = state(&split);
    if final_state != state(&combined) || split.ocean != combined.ocean {
        return Err("Separated and combined rail outputs differ".into());
    }
    drop(combined);
    let mut portal_maps = compose()?;
    let mut portal_plan = rail_generation::prepare_ring(
        &project,
        &layout,
        &catalog,
        &bindings,
        &portal_maps,
        || false,
    )?
    .ok_or("Portal rail checkpoint cancelled")?;
    let monuments = mapper_studio::monument_source::Source::cached(
        project
            .monument_rules
            .as_ref()
            .ok_or("Missing monument rules")?,
        &catalog,
        &bindings,
        || false,
    )?;
    let tunnels =
        mapper_studio::tunnel_source::Source::prepare(root, &catalog, &monuments, &|| false)?
            .ok_or("Missing tunnel source")?;
    let kind = portal_plan.source().profile.rail_type;
    let path = tunnels.selectors["UpwardsFolder"]
        .prefabs
        .iter()
        .find(|path| {
            let prefab = &tunnels.prefabs[*path];
            prefab.surface_offset.is_some()
                && prefab
                    .connectors
                    .iter()
                    .any(|c| c.infrastructure_type == kind)
        })
        .ok_or("No authored portal rail connectors")?;
    let connector = tunnels.prefabs[path]
        .connectors
        .iter()
        .find(|c| c.infrastructure_type == kind)
        .ok_or("Missing portal connector")?;
    let points = portal_plan.path().points();
    let index = points.len() / 3;
    let tangent = [
        points[index + 1][0] - points[index][0],
        points[index + 1][2] - points[index][2],
    ];
    let angle = tangent[0].atan2(tangent[1]) - connector.forward[0].atan2(connector.forward[2]);
    let rotation = [0., (angle * 0.5).sin(), 0., (angle * 0.5).cos()];
    let offset = papyrus_terrain::environment::rotate(rotation, connector.position);
    let direction = papyrus_terrain::environment::rotate(rotation, connector.forward);
    let radius = points[index][0].hypot(points[index][2]);
    let inward = [points[index][0] / radius, 0., points[index][2] / radius];
    let portal = rail_generation::Portal {
        instance: layout.outcomes.iter().map(|o| o.id).max().unwrap_or(0) + 1,
        prefab: path.clone(),
        pose: papyrus_terrain::dungeon_base::Pose {
            position: std::array::from_fn(|i| {
                points[index][i] - offset[i] - direction[i] * 60. - inward[i] * 300.
            }),
            rotation,
        },
    };
    let portal_count = tunnels.prefabs[path]
        .connectors
        .iter()
        .filter(|c| c.infrastructure_type == kind)
        .count();
    let before_portals = state(&portal_maps);
    if portal_plan.add_tunnel_portals(&tunnels, std::slice::from_ref(&portal), || true)?
        || before_portals != state(&portal_maps)
        || !portal_plan.add_tunnel_portals(&tunnels, std::slice::from_ref(&portal), || false)?
        || portal_plan
            .add_tunnel_portals(&tunnels, &[], || false)
            .is_ok()
        || hash(&serde_json::to_vec(portal_plan.path()).map_err(|e| e.to_string())?) != ring_sha256
    {
        return Err("Portal handoff changed terrain/ring or accepted a second handoff".into());
    }
    let _portal_pending =
        rail_generation::resume(portal_plan, &project, &layout, &mut portal_maps, || false)?
            .ok_or("Portal rail resume cancelled")?;
    let scene = portal_maps
        .rails
        .as_ref()
        .ok_or("Missing portal rail scene")?;
    let handoff = &scene.report["connector_source"];
    let actual = handoff["sockets"]
        .as_array()
        .ok_or("Missing final connector handoff")?
        .iter()
        .filter(|s| s["monument"] == portal.instance)
        .count();
    if actual != portal_count
        || handoff["generated"]["source_sha256"] != tunnels.sha256
        || scene.report["branching"]["input_connector_count"]
            != handoff["sockets"].as_array().unwrap().len()
    {
        return Err("Generated portal connectors were omitted from final rail output".into());
    }
    let portal_report = json!({"synthetic_placement":true,"portal":portal,"connectors":portal_count,
        "tracks":scene.tracks.len(),"report":scene.report,
        "fixed_ring_preserved":true,"cancelled_handoff_retains_input":true,"duplicate_handoff_rejected":true});
    drop(portal_maps);
    let mut retained = compose()?;
    let before = state(&retained);
    let plan =
        rail_generation::prepare_ring(&project, &layout, &catalog, &bindings, &retained, || false)?
            .ok_or("Stale-recipe ring checkpoint cancelled")?;
    let mut changed = project.clone();
    changed.seeds[0].value = changed.seeds[0].value.wrapping_add(1);
    if rail_generation::resume(plan, &changed, &layout, &mut retained, || false).is_ok()
        || before != state(&retained)
    {
        return Err("Stale rail checkpoint changed the private terrain".into());
    }
    let plan =
        rail_generation::prepare_ring(&project, &layout, &catalog, &bindings, &retained, || false)?
            .ok_or("Stale-layout ring checkpoint cancelled")?;
    let mut changed_layout = layout.clone();
    changed_layout.attempts = changed_layout.attempts.saturating_add(1);
    if rail_generation::resume(plan, &project, &changed_layout, &mut retained, || false).is_ok()
        || before != state(&retained)
    {
        return Err("Stale monument layout changed the private terrain".into());
    }
    let plan =
        rail_generation::prepare_ring(&project, &layout, &catalog, &bindings, &retained, || false)?
            .ok_or("Cancellation ring checkpoint cancelled")?;
    if rail_generation::resume(plan, &project, &layout, &mut retained, || true)?.is_some()
        || before != state(&retained)
    {
        return Err("Cancelled rail checkpoint published outputs".into());
    }
    let report = json!({"format":"mapper-rail-ring-stage-verification","schema":1,
        "ring_sha256":ring_sha256,"ring_samples":ring_samples,"prepared_state":final_state,
        "combined_and_split_equal":true,"intervening_height_edit_retained":true,"stale_recipe_rejected":true,"stale_layout_rejected":true,
        "cancelled_resume_retains_input":true,"normal_entry_uses_checkpoint":true,
        "portal_handoff":portal_report,
        "underground_integration_complete":false,"runtime_game_loading_tested":false});
    let mut file = std::fs::File::create_new(output).map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    println!(
        "Verified {ring_samples} ring samples, equivalent rail output and rejected stale/cancelled resumes"
    );
    Ok(())
}
