//! Selected-client cliff passes before scenery and after late collision height.
use crate::{Project, installation::SourceFile, prefab_assets::Catalog, prefab_textures::Bindings};
use papyrus_terrain::{cliff_topology, monument_composition::Composed};
use serde::Deserialize;

pub(crate) const MAXIMUM_WORK: usize = 192_000_000;

const PROFILES: [&str; 3] = [
    include_str!("../../../compatibility/topology/client-0f703d43-cliff.json"),
    include_str!("../../../compatibility/topology/client-8dd4e235-cliff.json"),
    include_str!("../../../compatibility/topology/client-58f3263f-cliff.json"),
];
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    format: String,
    schema: u32,
    native_sha256: String,
    code_sources: Vec<SourceFile>,
    map_scene: SourceFile,
    process_sha256: String,
    classification_sha256: String,
    dilation_sha256: String,
    stage_ids: [i64; 2],
    keep_existing: bool,
    policy: cliff_topology::Policy,
}

fn selected(native: &str) -> Result<Option<Profile>, String> {
    for raw in PROFILES {
        let profile: Profile = serde_json::from_str(raw).map_err(|e| e.to_string())?;
        profile.policy.validate()?;
        let hash = |s: &str| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        if profile.format != "papyrus-final-cliff-topology-profile"
            || profile.schema != 1
            || !hash(&profile.native_sha256)
            || !hash(&profile.process_sha256)
            || !hash(&profile.classification_sha256)
            || !hash(&profile.dilation_sha256)
            || profile.stage_ids.iter().any(|id| *id <= 0)
            || profile.stage_ids[0] == profile.stage_ids[1]
            || profile.code_sources.len() != 2
            || profile.code_sources[0].relative_path != "GameAssembly.dll"
            || profile.code_sources[1].relative_path
                != "RustClient_Data/il2cpp_data/Metadata/global-metadata.dat"
            || profile.map_scene.relative_path != "Bundles/maps/maps.bundle"
        {
            return Err("Invalid bundled cliff topology profile".into());
        }
        if profile.native_sha256 == native {
            return Ok(Some(profile));
        }
    }
    Ok(None)
}

fn active(stage: &crate::road_object_source::ProceduralStage) -> bool {
    stage.hierarchy_active && !stage.disabled.unwrap_or(false) && stage.mode & 2 != 0
}

fn verified_profile(
    catalog: &Catalog,
    bindings: &Bindings,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Profile>, String> {
    if catalog.installation.kind != crate::installation::InstallationKind::Client {
        return Ok(None);
    }
    let Some(profile) = selected(&catalog.installation.native.sha256)? else {
        return Ok(None);
    };
    let root = bindings.installation_root();
    for receipt in profile
        .code_sources
        .iter()
        .chain(std::iter::once(&profile.map_scene))
    {
        if cancelled() {
            return Ok(None);
        }
        if crate::installation::hash_file(
            root,
            &receipt.relative_path,
            1024 * 1024 * 1024,
            cancelled,
        )? != *receipt
        {
            return Err(
                "Cliff topology profile differs from the selected Rust installation".into(),
            );
        }
    }
    Ok(Some(profile))
}

fn source_stages<'a>(
    profile: &Profile,
    source: &'a crate::road_object_source::Source,
) -> Result<[&'a crate::road_object_source::ProceduralStage; 2], String> {
    let matches = source
        .procedural_order()?
        .iter()
        .filter(|s| s.kind == "GenerateCliffTopology")
        .collect::<Vec<_>>();
    if matches.len() != 2 || matches.iter().map(|s| s.object_id).ne(profile.stage_ids) {
        return Err("Cliff topology stages differ from the selected client profile".into());
    }
    Ok([matches[0], matches[1]])
}

pub(crate) fn prepare_early(
    catalog: &Catalog,
    bindings: &Bindings,
    source: &crate::road_object_source::Source,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<crate::object_generation::EarlyCliff>, String> {
    let Some(profile) = verified_profile(catalog, bindings, cancelled)? else {
        return Ok(None);
    };
    early_stage(&profile, source).map(Some)
}

fn early_stage(
    profile: &Profile,
    source: &crate::road_object_source::Source,
) -> Result<crate::object_generation::EarlyCliff, String> {
    let stage = source_stages(profile, source)?[0];
    Ok(crate::object_generation::EarlyCliff {
        object_id: stage.object_id,
        source_sha256: source.sha256.clone(),
        policy: profile.policy,
        keep_existing: profile.keep_existing,
        active: active(stage),
        provenance: serde_json::json!({
            "engine": cliff_topology::VERSION,
            "source_sha256": source.sha256,
            "object_id": stage.object_id,
            "scene_order": stage.order,
            "height_source": "live_terrain",
            "process_sha256": profile.process_sha256,
            "classification_sha256": profile.classification_sha256,
            "dilation_sha256": profile.dilation_sha256,
        }),
    })
}

fn object_source(
    project: &Project,
    catalog: &Catalog,
    bindings: &Bindings,
    cancelled: &impl Fn() -> bool,
) -> Result<crate::road_object_source::Source, String> {
    let infrastructure = if let Some(recipe) = &project.roads {
        crate::infrastructure::Source::cached_shared(catalog, recipe, cancelled)?
    } else {
        std::sync::Arc::new(crate::infrastructure::Source::discover(catalog, None, cancelled)?)
    };
    crate::object_extract::prepare_roads(
        bindings.installation_root(),
        &infrastructure,
        project
            .objects
            .as_ref()
            .map(|r| r.road_source_sha256.as_str()),
        cancelled,
    )
}

/// Batch scenery and terrain-only recipes have no incremental object emitter.
pub(crate) fn apply_early(
    project: &Project,
    catalog: &Catalog,
    bindings: &Bindings,
    maps: &mut Composed,
    cancelled: &impl Fn() -> bool,
) -> Result<bool, String> {
    if cancelled() {
        return Ok(false);
    }
    if maps
        .cliff_topology
        .as_ref()
        .is_some_and(|r| r.get("early").is_some())
    {
        return Ok(true);
    }
    let Some(profile) = verified_profile(catalog, bindings, cancelled)? else {
        return Ok(!cancelled());
    };
    let source = object_source(project, catalog, bindings, cancelled)?;
    let early = early_stage(&profile, &source)?;
    let mut topology = None;
    let report = if early.active {
        let Some((cells, report)) = cliff_topology::apply(
            maps.layers.view(),
            early.policy,
            early.keep_existing,
            MAXIMUM_WORK,
            cancelled,
        )?
        else {
            return Ok(false);
        };
        topology = Some(cells);
        Some(report)
    } else {
        None
    };
    let receipt = early.receipt(report, 0)?;
    if cancelled() {
        return Ok(false);
    }
    if let Some(cells) = topology {
        maps.layers.topology = cells;
        if let Some(ocean) = &mut maps.ocean {
            ocean.topology.clone_from(&maps.layers.topology);
        }
    }
    maps.cliff_topology = Some(serde_json::json!({"early":receipt}));
    Ok(true)
}

pub(crate) fn apply(
    project: &Project,
    catalog: &Catalog,
    bindings: &Bindings,
    maps: &mut Composed,
    cancelled: impl Fn() -> bool,
) -> Result<bool, String> {
    if cancelled() {
        return Ok(false);
    }
    if maps
        .cliff_topology
        .as_ref()
        .is_some_and(|r| r["engine"].as_str() == Some(cliff_topology::VERSION))
    {
        return Ok(true);
    }
    let Some(profile) = verified_profile(catalog, bindings, &cancelled)? else {
        return Ok(!cancelled());
    };
    let source = object_source(project, catalog, bindings, &cancelled)?;
    let stage = source_stages(&profile, &source)?[1];
    let enabled = active(stage);
    let mut topology = None;
    let report = if enabled {
        let layers = papyrus_terrain::native_layers::View {
            height: maps
                .gameplay_height
                .as_ref()
                .map_or(maps.layers.height.as_slice(), |h| h.height.as_slice()),
            ..maps.layers.view()
        };
        let Some((cells, report)) =
            apply_final(layers, profile.policy, profile.keep_existing, &cancelled)?
        else {
            return Ok(false);
        };
        topology = Some(cells);
        Some(report)
    } else {
        None
    };
    if cancelled() {
        return Ok(false);
    }
    let provenance = serde_json::json!({
        "engine": cliff_topology::VERSION,
        "source_sha256": source.sha256,
        "object_id": stage.object_id,
        "scene_order": stage.order,
        "executed": enabled,
        "process_sha256": profile.process_sha256,
        "classification_sha256": profile.classification_sha256,
        "dilation_sha256": profile.dilation_sha256,
        "height_source": if maps.gameplay_height.is_some() { "gameplay_height" } else { "terrain" },
        "report": report,
        "early": maps.cliff_topology.as_ref().and_then(|r| r.get("early")),
    });
    if let Some(cells) = topology {
        maps.layers.topology = cells;
        if let Some(ocean) = &mut maps.ocean {
            ocean.topology.clone_from(&maps.layers.topology);
        }
        if let Some(objects) = &mut maps.objects {
            objects
                .external_phases
                .retain(|s| s.scene_order != stage.order || s.class != stage.kind);
        }
    }
    maps.cliff_topology = Some(provenance);
    Ok(true)
}

fn apply_final(
    layers: papyrus_terrain::native_layers::View<'_>,
    policy: cliff_topology::Policy,
    keep_existing: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Option<(Vec<i32>, cliff_topology::Report)>, String> {
    use crate::slope_compute::{self, Threshold};
    use papyrus_terrain::terrain_slope;

    let n = layers.height_resolution;
    let surface = layers.surface_resolution;
    if (2..=2048).contains(&surface)
        && surface.is_power_of_two()
        && let Ok(threshold) = Threshold::new(layers.size, n, policy.slope_cutoff)
        && (n - 1).is_multiple_of(2 * surface)
        && layers.height.len() == n * n
        && let Some(result) = crate::generation_resources::try_with_cpu_pool(|pool| {
            if cancelled() {
                return Ok(None);
            }
            if layers.height.iter().any(|h| !(0..=32766).contains(h)) {
                return Err("Invalid cliff topology height encoding".into());
            }
            let input = terrain_slope::Input {
                size: layers.size,
                resolution: n,
                heights: layers.height,
            };
            let started =
                std::env::var_os("MAPPER_COMPOSITION_TIMINGS").map(|_| std::time::Instant::now());
            let mut decisions = slope_compute::cpu(&input, &threshold, pool);
            if cancelled() {
                return Ok(None);
            }
            slope_compute::resolve_with(&mut decisions, pool, |cell| {
                terrain_slope::interior_cliff_angle(&input, cell)
                    .is_ok_and(|angle| angle > threshold.degrees)
            });
            let result = cliff_topology::apply_with_vertex_mask(
                layers,
                policy,
                keep_existing,
                MAXIMUM_WORK,
                &decisions,
                &cancelled,
            );
            if let Some(started) = started {
                eprintln!(
                    "MAPPER_CLIFF_THRESHOLD_TIMING {}",
                    serde_json::json!({
                        "workers": pool.current_num_threads(),
                        "milliseconds": started.elapsed().as_secs_f64() * 1000.,
                        "complete": matches!(&result, Ok(Some(_))),
                    })
                );
            }
            result
        })?
    {
        return Ok(result);
    }
    cliff_topology::apply(layers, policy, keep_existing, MAXIMUM_WORK, cancelled)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_profiles_bind_both_ordered_stages_and_reject_changed_rosters() {
        use crate::road_object_placement::tests::{source, source_value};
        for raw in PROFILES {
            let profile: Profile = serde_json::from_str(raw).unwrap();
            let mut value = source_value();
            value["schema"] = 2.into();
            value["stages"][0]["scene_order"] = 0.into();
            let row = |id, order, kind: &str, name: &str| {
                serde_json::json!({
                    "object_id":id,"order":order,"kind":kind,"description":name,
                    "mode":-1,"disabled":false,"component_enabled":true,"hierarchy_active":true,
                })
            };
            value["workflow"] = serde_json::json!({
                "format":"papyrus-procedural-order","schema":1,
                "stages":[row(1,0,"PlaceRoadObjects","Objects"),
                    row(profile.stage_ids[0],1,"GenerateCliffTopology","Cliff Topology"),
                    row(profile.stage_ids[1],2,"GenerateCliffTopology","Cliff Topology")],
            });
            let snapshot = source(value.clone());
            let early = early_stage(&profile, &snapshot).unwrap();
            assert_eq!(early.object_id, profile.stage_ids[0]);
            assert_eq!(early.policy, profile.policy);
            assert!(early.active);
            assert!(!early.keep_existing);
            value["workflow"]["stages"][2]["object_id"] = 99999.into();
            assert!(source_stages(&profile, &source(value)).is_err());
        }
    }
    #[test]
    fn profiles_select_exact_builds_and_activation_uses_the_procedural_gate() {
        for raw in PROFILES {
            let p: Profile = serde_json::from_str(raw).unwrap();
            assert_eq!(
                selected(&p.native_sha256).unwrap().unwrap().policy,
                p.policy
            );
        }
        assert!(selected(&"0".repeat(64)).unwrap().is_none());
        let mut stage = crate::road_object_source::ProceduralStage {
            object_id: 1,
            order: 88,
            kind: "GenerateCliffTopology".into(),
            description: String::new(),
            mode: -1,
            disabled: None,
            component_enabled: false,
            hierarchy_active: true,
        };
        assert!(active(&stage));
        stage.mode = 1;
        assert!(!active(&stage));
        stage.mode = 2;
        stage.disabled = Some(true);
        assert!(!active(&stage));
        stage.disabled = Some(false);
        stage.hierarchy_active = false;
        assert!(!active(&stage));
    }
}
