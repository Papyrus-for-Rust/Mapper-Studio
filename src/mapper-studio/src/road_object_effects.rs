//! Atomic road-object supports and terrain modifiers; prefab assembly is separate.
use crate::{
    infrastructure,
    road_object_placement::{self, Group},
    road_object_source::Source,
};
use papyrus_terrain::{decor, decor_fit, erosion, footprints, native_layers::Layers};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::{io::Write, path::Path};
pub const VERSION: &str = footprints::ROAD_OBJECT_VERSION;
#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub source_sha256: String,
    pub input_terrain_sha256: String,
    pub fitting_poses: usize,
    pub accepted_fit_indices: Vec<usize>,
    pub transaction: footprints::Report,
    pub terrain_applied: bool,
    pub assembled_instances: usize,
    pub physics_validated: bool,
    pub connectors: Vec<crate::road_object_connectors::Connector>,
    pub connector_connections_built: bool,
}
pub struct Output {
    pub height: Vec<i16>,
    pub topology: Vec<i32>,
    pub splat: Vec<u8>,
    pub report: Report,
}
pub struct Preview<'a> {
    pub directory: &'a Path,
    pub input_directory: &'a Path,
    pub input_manifest_sha256: &'a str,
    pub base: &'a Layers,
    pub alpha: &'a [u8],
    pub fitting: &'a road_object_placement::Report,
}
/// Publish an explicit terrain-only diagnostic, never a playable map or an
/// apparently complete generation bundle. Rename occurs after every file closes.
pub fn publish(
    output: Output,
    preview: Preview<'_>,
    cancelled: impl Fn() -> bool,
) -> Result<serde_json::Value, String> {
    if preview.directory.exists() {
        return Err("Road-object preview destination already exists".into());
    }
    if output.report.source_sha256 != preview.fitting.source_sha256
        || output.report.input_terrain_sha256 != preview.fitting.terrain_sha256
        || output.report.input_terrain_sha256
            != road_object_placement::terrain_digest(preview.base, preview.alpha)
    {
        return Err("Road-object preview inputs differ from their transaction".into());
    }
    let parent = preview
        .directory
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = parent.canonicalize().map_err(|e| e.to_string())?;
    let name = preview
        .directory
        .file_name()
        .ok_or("Invalid road-object preview destination")?;
    let destination = parent.join(name);
    let input = preview
        .input_directory
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if destination.starts_with(&input) || input.starts_with(&destination) {
        return Err("Road-object preview must be separate from its input bundle".into());
    }
    if cancelled() {
        return Err("Road-object preview cancelled".into());
    }
    let stage = tempfile::Builder::new()
        .prefix(".road-object-preview-")
        .tempdir_in(&parent)
        .map_err(|e| e.to_string())?;
    let base = preview.base;
    let layers = Layers {
        size: base.size,
        terrain_seed: base.terrain_seed,
        biome_seed: base.biome_seed,
        height_resolution: base.height_resolution,
        surface_resolution: base.surface_resolution,
        height: output.height,
        topology: output.topology,
        splat: output.splat,
        biome: base.biome.clone(),
        normalized_biome_cells: base.normalized_biome_cells,
        normalized_splat_cells: base.normalized_splat_cells,
    };
    let mut files = serde_json::Map::new();
    let mut write = |name: &str, bytes: &[u8]| -> Result<(), String> {
        if cancelled() {
            return Err("Road-object preview cancelled".into());
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(stage.path().join(name))
            .map_err(|e| e.to_string())?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        files.insert(
            name.into(),
            serde_json::json!({"bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(bytes))}),
        );
        Ok(())
    };
    write(
        "height.i16le",
        &layers
            .height
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>(),
    )?;
    write(
        "topology.i32le",
        &layers
            .topology
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>(),
    )?;
    write("splat.u8", &layers.splat)?;
    write("biome.u8", &layers.biome)?;
    write("alpha.u8", preview.alpha)?;
    write(
        "effects.json",
        &serde_json::to_vec_pretty(&output.report).map_err(|e| e.to_string())?,
    )?;
    write(
        "fitting.json",
        &serde_json::to_vec(preview.fitting).map_err(|e| e.to_string())?,
    )?;
    let ocean = papyrus_terrain::ocean::reconstruct(
        &layers.height,
        layers.height_resolution,
        &layers.topology,
        layers.surface_resolution,
        &cancelled,
    )?
    .ok_or("Road-object image cancelled")?;
    let image = crate::map_image::render(
        &layers,
        Some(preview.alpha),
        Some(&ocean),
        crate::map_image::DISPLAY_RESOLUTION,
        &cancelled,
    )?
    .ok_or("Road-object image cancelled")?;
    let mut encoded = Vec::new();
    {
        let mut encoder =
            png::Encoder::new(&mut encoded, image.size[0] as u32, image.size[1] as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        let rgba = image
            .pixels
            .iter()
            .flat_map(eframe::egui::Color32::to_srgba_unmultiplied)
            .collect::<Vec<_>>();
        writer.write_image_data(&rgba).map_err(|e| e.to_string())?;
        writer.finish().map_err(|e| e.to_string())?;
    }
    write("overview.png", &encoded)?;
    let manifest = serde_json::json!({"format":"papyrus-road-object-terrain-preview","schema":1,"playable":false,"terrain_only":true,"input_manifest_sha256":preview.input_manifest_sha256,"source_sha256":output.report.source_sha256,"size":layers.size,"height_resolution":layers.height_resolution,"surface_resolution":layers.surface_resolution,"grid_rows":"world -Z to +Z; planar channels; little-endian words","height_encoding":"signed i16 normalized by 32766 over -500..500 metres","ocean_rebuilt_for_image":true,"ocean_topology_reconciliation_applied":false,"files":files});
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(stage.path().join("manifest.json"))
        .map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    drop(file);
    if cancelled() {
        return Err("Road-object preview cancelled".into());
    }
    if destination.exists() {
        return Err("Road-object preview destination appeared during publication".into());
    }
    std::fs::rename(stage.path(), &destination).map_err(|e| e.to_string())?;
    Ok(manifest)
}
pub fn apply(
    layers: &Layers,
    alpha: &[u8],
    source: &Source,
    parent: &infrastructure::Source,
    fitting: &road_object_placement::Report,
    maximum_height_change: f32,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    if cancelled() {
        return Ok(None);
    }
    source.validate_for_fitting()?;
    if source.infrastructure_sha256 != parent.sha256
        || fitting.source_sha256 != source.sha256
        || fitting.engine != road_object_placement::VERSION
        || fitting.placed_instances != 0
        || fitting.terrain_applied
        || fitting.budget_exhausted
        || fitting.provisional_poses.len() > 100_000
        || !maximum_height_change.is_finite()
        || !(0. ..=24.).contains(&maximum_height_change)
        || fitting.terrain_sha256 != road_object_placement::terrain_digest(layers, alpha)
    {
        return Err("Road-object effect source, terrain or fitting revision mismatch".into());
    }
    let flags = parent
        .profile
        .topology_flags
        .as_ref()
        .ok_or("Road-object effects require source topology roles")?;
    let known = flags.values().fold(0, |a, b| a | b);
    let protected = parent.profile.protected_topology as u32;
    let mut prefabs = BTreeMap::new();
    for (path, p) in &source.prefabs {
        if cancelled() {
            return Ok(None);
        }
        prefabs.insert(
            path.clone(),
            crate::decor_source::compile_road_effects(p, flags, protected)?,
        );
    }
    let mut stages = Vec::new();
    let mut stage_ids = BTreeMap::new();
    let mut candidates = Vec::new();
    let mut poses = Vec::new();
    for (i, p) in fitting.provisional_poses.iter().enumerate() {
        if cancelled() {
            return Ok(None);
        }
        let s = source
            .stages
            .get(p.stage)
            .ok_or("Unknown effect source stage")?;
        let (folder, filter, paths) = match p.group {
            Group::Start | Group::End => {
                let r = if p.group == Group::Start {
                    &s.start
                } else {
                    &s.end
                }
                .get(p.rule)
                .ok_or("Unknown endpoint effect rule")?;
                (&r.folder, &r.filter, &r.prefabs)
            }
            Group::Side => {
                let r = s.side.get(p.rule).ok_or("Unknown roadside effect rule")?;
                (&r.folder, &r.filter, &r.prefabs)
            }
            Group::Path => {
                let r = s.path.get(p.rule).ok_or("Unknown path effect rule")?;
                (&r.folder, &r.filter, &r.prefabs)
            }
        };
        if !s.enabled() || s.mode != -1 {
            return Err("Inactive road-object effect stage".into());
        }
        let key = (p.stage, p.group, p.rule);
        let stage = *stage_ids.entry(key).or_insert_with(|| {
            let index = stages.len();
            stages.push(decor::Stage {
                id: format!("{}:{:?}:{}", s.object_id, p.group, p.rule),
                order: index,
                name: s.name.clone(),
                kind: decor::Kind::Uniform,
                folder: folder.clone(),
                enabled: true,
                mode: -1,
                filter: filter.clone(),
                filter_mode: 3,
                anchor_mode: 0,
                distance: 1.,
                dithering: 0.,
                density: 1.,
                cluster: None,
                prefabs: paths.clone(),
            });
            index
        });
        let prefab = paths
            .iter()
            .position(|path| path == &p.prefab)
            .ok_or("Effect prefab differs from source roster")?;
        candidates.push(decor::Candidate {
            stage,
            prefab,
            position: p.transform.position,
        });
        poses.push(decor_fit::Pose {
            candidate: i,
            position: p.transform.position,
            rotation: p.transform.rotation,
            scale: p.transform.scale,
        });
    }
    let result = footprints::apply_road_objects(
        footprints::Input {
            height_reservations: None,
            layers,
            alpha,
            candidates: &candidates,
            stages: &stages,
            poses: &poses,
            prefabs: &prefabs,
            obstacles: &[],
            protected_topology: protected,
        },
        known,
        &cancelled,
    )?;
    let Some(result) = result else {
        return Ok(None);
    };
    finish_output(
        layers,
        source,
        fitting,
        result,
        maximum_height_change,
        VERSION,
        &cancelled,
    )
}
fn finish_output(
    layers: &Layers,
    source: &Source,
    fitting: &road_object_placement::Report,
    result: footprints::Output,
    maximum_height_change: f32,
    engine: &'static str,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    if result.report.budget_exhausted {
        return Err("Road-object terrain transaction exhausted its work allowance".into());
    }
    if result.height.iter().zip(&layers.height).any(|(&a, &b)| {
        (erosion::decode(a) - erosion::decode(b)).abs() > maximum_height_change + 1000. / 32766.
    }) {
        return Err("Road-object terrain transaction exceeds the saved cut/fill allowance".into());
    }
    if cancelled() {
        return Ok(None);
    }
    let accepted_fit_indices: Vec<_> = result.report.applied.iter().map(|p| p.pose).collect();
    let mut connectors = Vec::new();
    for &i in &accepted_fit_indices {
        if cancelled() {
            return Ok(None);
        }
        let pose = &fitting.provisional_poses[i];
        for connector in crate::road_object_connectors::local(&source.prefabs[&pose.prefab])? {
            if connectors.len() == 200_000 {
                return Err("Road-object transaction exceeds the connector allowance".into());
            }
            connectors.push(crate::road_object_connectors::world(
                i,
                &pose.prefab,
                &connector,
                &pose.transform,
            )?);
        }
    }
    Ok(Some(Output {
        height: result.height,
        topology: result.topology.ok_or("Missing committed topology")?,
        splat: result.splat.ok_or("Missing committed material grid")?,
        report: Report {
            engine,
            source_sha256: source.sha256.clone(),
            input_terrain_sha256: fitting.terrain_sha256.clone(),
            fitting_poses: fitting.provisional_poses.len(),
            accepted_fit_indices,
            transaction: result.report,
            terrain_applied: true,
            assembled_instances: 0,
            physics_validated: false,
            connectors,
            connector_connections_built: false,
        },
    }))
}

pub(crate) fn placement_stage(
    source: &Source,
    p: &road_object_placement::Pose,
    order: usize,
) -> Result<decor::Stage, String> {
    placement_stage_for_realm(source, p, order, None)
}
pub(crate) fn placement_stage_for_realm(
    source: &Source,
    p: &road_object_placement::Pose,
    order: usize,
    realm: Option<u32>,
) -> Result<decor::Stage, String> {
    let s = source
        .stages
        .get(p.stage)
        .ok_or("Unknown effect source stage")?;
    let (folder, filter, paths) = match p.group {
        Group::Start | Group::End => {
            let r = if p.group == Group::Start {
                &s.start
            } else {
                &s.end
            }
            .get(p.rule)
            .ok_or("Unknown endpoint effect rule")?;
            (&r.folder, &r.filter, &r.prefabs)
        }
        Group::Side => {
            let r = s.side.get(p.rule).ok_or("Unknown roadside effect rule")?;
            (&r.folder, &r.filter, &r.prefabs)
        }
        Group::Path => {
            let r = s.path.get(p.rule).ok_or("Unknown path effect rule")?;
            (&r.folder, &r.filter, &r.prefabs)
        }
    };
    if !s.enabled() || !realm.map_or(s.mode == -1, |r| r != 0 && s.mode as u32 & r != 0) {
        return Err("Inactive road-object effect stage".into());
    }

    if !paths.iter().any(|path| path == &p.prefab) {
        return Err("Effect prefab differs from source roster".into());
    }
    let stage = decor::Stage {
        id: format!("{}:{:?}:{}", s.object_id, p.group, p.rule),
        order,
        name: s.name.clone(),
        kind: decor::Kind::Uniform,
        folder: folder.clone(),
        enabled: true,
        mode: -1,
        filter: filter.clone(),
        filter_mode: 3,
        anchor_mode: 0,
        distance: 1.,
        dithering: 0.,
        density: 1.,
        cluster: None,
        prefabs: paths.clone(),
    };
    stage.validate_path_attached()?;
    Ok(stage)
}
pub const LIVE_VERSION: &str = "papyrus-road-object-live-effects-3";
pub struct Generated {
    pub fitting: road_object_placement::Report,
    pub terrain: Output,
}
/// Fit and admit each emitted object against the preceding private destination.
/// Rejected admission does not advance immediate source spacing/side state.
#[allow(clippy::too_many_arguments)]
pub fn generate(
    layers: &Layers,
    alpha: &[u8],
    roads: &[road_object_placement::Road],
    source: &Source,
    parent: &infrastructure::Source,
    settings: road_object_placement::Settings,
    maximum_height_change: f32,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Generated>, String> {
    if cancelled() {
        return Ok(None);
    }
    source.validate_for_fitting()?;
    if source.infrastructure_sha256 != parent.sha256
        || !maximum_height_change.is_finite()
        || !(0. ..=24.).contains(&maximum_height_change)
    {
        return Err("Live road-object source or cut/fill policy mismatch".into());
    }
    let flags = parent
        .profile
        .topology_flags
        .as_ref()
        .ok_or("Live road objects require bound topology roles")?;
    let known = flags.values().fold(0, |a, b| a | b);
    let protected = parent.profile.protected_topology as u32;
    let mut prefabs = BTreeMap::new();
    for (path, p) in &source.prefabs {
        if cancelled() {
            return Ok(None);
        }
        prefabs.insert(
            path.clone(),
            crate::decor_source::compile_road_effects(p, flags, protected)?,
        );
    }
    let Some(transaction) =
        footprints::RoadTransaction::new(layers, alpha, prefabs, known, protected, &cancelled)?
    else {
        return Ok(None);
    };
    let Some((fitting, result)) = road_object_placement::fit_incremental(
        layers,
        alpha,
        roads,
        source,
        settings,
        Some(transaction),
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let Some(terrain) = finish_output(
        layers,
        source,
        &fitting,
        result.ok_or("Missing live road-object terrain")?,
        maximum_height_change,
        LIVE_VERSION,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    Ok(Some(Generated { fitting, terrain }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::road_object_placement::tests::{layers, run, source, source_value};
    const A: &str = "assets/bundled/prefabs/autospawn/decor/test/a.prefab";
    #[test]
    fn live_generation_filters_later_samples_using_committed_terrain_and_replays() {
        let mut v = source_value();
        v["stages"][0]["side"][0]["filter"]["TopologyNot"] = 2.into();
        v["prefabs"][A]["attributes"] = serde_json::json!([
            {"kind":"TerrainTopologyAdd","root":true,"position":[0.,0.,0.],"fields":{"Opacity":1.,"Radius":40.,"Fade":0.,"TopologyType":2}},
            {"kind":"TerrainHeightAdd","root":true,"position":[0.,0.,0.],"fields":{"Opacity":1.,"Radius":20.,"Fade":0.,"Delta":1.}}
        ]);
        let src = source(v);
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let settings = road_object_placement::Settings {
            seed: 42,
            maximum_work: 100000,
            maximum_poses: 1000,
        };
        let stale = run(&l, &src);
        assert_eq!(stale.counts.get("filter_miss"), None);
        let parent = parent();
        let generate = || {
            super::generate(
                &l,
                &alpha,
                &[crate::road_object_placement::tests::road()],
                &src,
                &parent,
                settings,
                8.,
                || false,
            )
            .unwrap()
            .unwrap()
        };
        let live = generate();
        let replay = generate();
        assert_eq!(live.fitting.engine, road_object_placement::LIVE_VERSION);
        assert!(live.fitting.terrain_applied);
        assert!(!live.fitting.budget_exhausted);
        assert!(live.fitting.counts["filter_miss"] > 0);
        assert!(live.terrain.report.transaction.counts.accepted > 0);
        assert!(live.terrain.report.transaction.counts.occupied > 0);
        assert_ne!(live.terrain.height, l.height);
        assert_ne!(live.terrain.topology, l.topology);
        assert_eq!(live.terrain.height, replay.terrain.height);
        assert_eq!(
            serde_json::to_value(&live.fitting).unwrap(),
            serde_json::to_value(&replay.fitting).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&live.terrain.report).unwrap(),
            serde_json::to_value(&replay.terrain.report).unwrap()
        );
        assert!(l.height.iter().all(|v| *v == 18000));
        assert!(l.topology.iter().all(|v| *v == 1));
    }
    #[test]
    fn live_generation_cancellation_and_budgets_discard_the_entire_private_destination() {
        let l = layers();
        let src = effects_source();
        let parent = parent();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let settings = road_object_placement::Settings {
            seed: 42,
            maximum_work: 100000,
            maximum_poses: 1000,
        };
        let calls = std::cell::Cell::new(0);
        super::generate(
            &l,
            &alpha,
            &[crate::road_object_placement::tests::road()],
            &src,
            &parent,
            settings,
            8.,
            || {
                calls.set(calls.get() + 1);
                false
            },
        )
        .unwrap()
        .unwrap();
        let stop = calls.get() - 5;
        calls.set(0);
        assert!(
            super::generate(
                &l,
                &alpha,
                &[crate::road_object_placement::tests::road()],
                &src,
                &parent,
                settings,
                8.,
                || {
                    calls.set(calls.get() + 1);
                    calls.get() >= stop
                }
            )
            .unwrap()
            .is_none()
        );
        assert!(
            super::generate(
                &l,
                &alpha,
                &[crate::road_object_placement::tests::road()],
                &src,
                &parent,
                road_object_placement::Settings {
                    maximum_work: 1,
                    ..settings
                },
                8.,
                || false
            )
            .is_err()
        );
        assert!(
            super::generate(
                &l,
                &alpha,
                &[crate::road_object_placement::tests::road()],
                &src,
                &parent,
                settings,
                0.,
                || false
            )
            .is_err()
        );
        assert!(l.height.iter().all(|v| *v == 18000));
    }
    fn parent() -> infrastructure::Source {
        let profile: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-road-topology.json"
        ))
        .unwrap();
        let mut p:infrastructure::Source=serde_json::from_value(serde_json::json!({"format":"papyrus-infrastructure-source","schema":1,"adapter":"papyrus-path-connectors-1","catalog_sha256":"c".repeat(64),"sources":[],"extractor_sha256":"d".repeat(64),"profile":profile,"prefabs":{}})).unwrap();
        p.sha256 = "a".repeat(64);
        p
    }
    fn effects_source() -> Source {
        let mut v = source_value();
        v["prefabs"][A]["attributes"] = serde_json::json!([
            {"kind":"TerrainHeightAdd","root":true,"position":[0.,0.,0.],"fields":{"Opacity":1.,"Radius":4.,"Fade":2.,"Delta":1.}},
            {"kind":"TerrainSplatSet","root":true,"position":[0.,0.,0.],"fields":{"Opacity":1.,"Radius":2.,"Fade":1.,"SplatType":2}},
            {"kind":"TerrainTopologyAdd","root":true,"position":[0.,0.,0.],"fields":{"Opacity":1.,"Radius":2.,"Fade":1.,"TopologyType":512}}
        ]);
        source(v)
    }
    #[test]
    fn mixed_road_effects_commit_atomically_and_conflicting_instances_do_not_accumulate() {
        let l = layers();
        let src = effects_source();
        let f = run(&l, &src);
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let before = road_object_placement::terrain_digest(&l, &alpha);
        let result = apply(&l, &alpha, &src, &parent(), &f, 8., || false)
            .unwrap()
            .unwrap();
        assert_eq!(result.report.transaction.counts.accepted, 7);
        assert!(result.height.iter().zip(&l.height).any(|(a, b)| a > b));
        assert_ne!(result.splat, l.splat);
        assert!(result.topology.iter().any(|v| v & 512 != 0));
        let cells = l.surface_resolution.pow(2);
        assert!((0..cells).all(|i| {
            (0..8)
                .map(|c| result.splat[c * cells + i] as usize)
                .sum::<usize>()
                == 255
        }));
        assert_eq!(before, road_object_placement::terrain_digest(&l, &alpha));
        let mut overlap = run(&l, &src);
        overlap.provisional_poses[1].transform = overlap.provisional_poses[0].transform;
        let result = apply(&l, &alpha, &src, &parent(), &overlap, 8., || false)
            .unwrap()
            .unwrap();
        assert_eq!(result.report.transaction.counts.accepted, 6);
        assert!(result.report.transaction.counts.occupied > 0);
        assert!(
            result
                .height
                .iter()
                .zip(&l.height)
                .all(|(a, b)| (erosion::decode(*a) - erosion::decode(*b)).abs() < 1.04)
        );
    }
    #[test]
    fn no_effect_prefabs_reserve_supports_and_stale_or_over_budget_inputs_are_rejected() {
        let mut l = layers();
        let src = source(source_value());
        let f = run(&l, &src);
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let result = apply(&l, &alpha, &src, &parent(), &f, 8., || false)
            .unwrap()
            .unwrap();
        assert_eq!(result.report.transaction.counts.accepted, 7);
        assert_eq!(result.height, l.height);
        assert_eq!(result.splat, l.splat);
        assert_eq!(result.topology, l.topology);
        l.height[0] += 1;
        assert!(apply(&l, &alpha, &src, &parent(), &f, 8., || false).is_err());
        l.height[0] -= 1;
        let src = effects_source();
        let f = run(&l, &src);
        assert!(apply(&l, &alpha, &src, &parent(), &f, 0., || false).is_err());
        let mut wrong = run(&l, &src);
        wrong.budget_exhausted = true;
        assert!(apply(&l, &alpha, &src, &parent(), &wrong, 8., || false).is_err());
    }
    #[test]
    fn accepted_socket_positions_are_retained_but_rejected_instances_leave_no_connectors() {
        let mut v = source_value();
        v["prefabs"][A]["attributes"] = serde_json::json!([
            {"kind":"TerrainPathConnect","root":false,"position":[2.,0.,0.],"local_to_root":[[1.,0.,0.,2.],[0.,1.,0.,0.],[0.,0.,1.,0.],[0.,0.,0.,1.]],"fields":{"Type":2},"references":[]}
        ]);
        let src = source(v);
        let l = layers();
        let mut f = run(&l, &src);
        assert_eq!(f.provisional_poses.len(), 7);
        f.provisional_poses[1].transform = f.provisional_poses[0].transform;
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let out = apply(&l, &alpha, &src, &parent(), &f, 8., || false)
            .unwrap()
            .unwrap();
        assert_eq!(out.report.transaction.counts.accepted, 6);
        assert_eq!(out.report.connectors.len(), 6);
        assert!(!out.report.connector_connections_built);
        assert_eq!(out.height, l.height);
        assert_eq!(out.topology, l.topology);
        assert_eq!(out.splat, l.splat);
        for socket in out.report.connectors {
            assert_ne!(socket.fit_pose, 1);
            assert!(out.report.accepted_fit_indices.contains(&socket.fit_pose));
            let pose = &f.provisional_poses[socket.fit_pose].transform;
            let offset =
                papyrus_terrain::decor_fit::rotate(pose.rotation, [2. * pose.scale[0], 0., 0.]);
            assert_eq!(
                socket.position,
                std::array::from_fn(|i| pose.position[i] + offset[i])
            );
        }
    }
    #[test]
    fn late_cancellation_rolls_back_private_terrain_and_protected_ground_stays_untouched() {
        let mut l = layers();
        let src = effects_source();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let f = run(&l, &src);
        let before = road_object_placement::terrain_digest(&l, &alpha);
        let calls = std::cell::Cell::new(0);
        apply(&l, &alpha, &src, &parent(), &f, 8., || {
            calls.set(calls.get() + 1);
            false
        })
        .unwrap()
        .unwrap();
        let limit = calls.get() - 5;
        let ticks = std::cell::Cell::new(0);
        assert!(
            apply(&l, &alpha, &src, &parent(), &f, 8., || {
                ticks.set(ticks.get() + 1);
                ticks.get() >= limit
            })
            .unwrap()
            .is_none()
        );
        assert_eq!(before, road_object_placement::terrain_digest(&l, &alpha));
        l.topology.fill(1024);
        let f = run(&l, &src);
        let output = apply(&l, &alpha, &src, &parent(), &f, 8., || false)
            .unwrap()
            .unwrap();
        assert_eq!(output.report.transaction.counts.accepted, 0);
        assert!(output.report.transaction.counts.protected > 0);
        assert_eq!(output.height, l.height);
        assert_eq!(output.topology, l.topology);
    }
    #[test]
    fn preview_publication_is_explicit_atomic_and_never_overwrites_or_changes_input_bundle() {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("input");
        std::fs::create_dir(&input).unwrap();
        std::fs::write(input.join("sentinel"), b"unchanged").unwrap();
        let l = layers();
        let src = effects_source();
        let f = run(&l, &src);
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let parent = parent();
        let output = || {
            apply(&l, &alpha, &src, &parent, &f, 8., || false)
                .unwrap()
                .unwrap()
        };
        let nested = input.join("out");
        assert!(
            publish(
                output(),
                Preview {
                    directory: &nested,
                    input_directory: &input,
                    input_manifest_sha256: "test",
                    base: &l,
                    alpha: &alpha,
                    fitting: &f
                },
                || false
            )
            .is_err()
        );
        assert!(!nested.exists());
        let dest = root.path().join("out");
        assert!(
            publish(
                output(),
                Preview {
                    directory: &dest,
                    input_directory: &input,
                    input_manifest_sha256: "test",
                    base: &l,
                    alpha: &alpha,
                    fitting: &f
                },
                || true
            )
            .is_err()
        );
        assert!(!dest.exists());
        let m = publish(
            output(),
            Preview {
                directory: &dest,
                input_directory: &input,
                input_manifest_sha256: "test",
                base: &l,
                alpha: &alpha,
                fitting: &f,
            },
            || false,
        )
        .unwrap();
        assert_eq!(m["playable"], false);
        assert_eq!(m["terrain_only"], true);
        assert!(
            publish(
                output(),
                Preview {
                    directory: &dest,
                    input_directory: &input,
                    input_manifest_sha256: "test",
                    base: &l,
                    alpha: &alpha,
                    fitting: &f
                },
                || false
            )
            .is_err()
        );
        assert_eq!(std::fs::read(input.join("sentinel")).unwrap(), b"unchanged");
    }
}
