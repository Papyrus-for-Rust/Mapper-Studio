//! Selected-client final clutter topology, after placement and before user paint.
use crate::{Project, installation::SourceFile, prefab_assets::Catalog, prefab_textures::Bindings};
use papyrus_terrain::{clutter_topology, monument_composition::Composed};
use serde::Deserialize;

const PROFILES: [&str; 3] = [
    include_str!("../../../compatibility/topology/client-0f703d43-clutter.json"),
    include_str!("../../../compatibility/topology/client-8dd4e235-clutter.json"),
    include_str!("../../../compatibility/topology/client-58f3263f-clutter.json"),
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
    callback_sha256: String,
    policy: clutter_topology::Policy,
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
        if profile.format != "papyrus-clutter-topology-profile"
            || profile.schema != 1
            || !hash(&profile.native_sha256)
            || !hash(&profile.process_sha256)
            || !hash(&profile.callback_sha256)
            || profile.code_sources.len() != 2
            || profile.code_sources[0].relative_path != "GameAssembly.dll"
            || profile.code_sources[1].relative_path
                != "RustClient_Data/il2cpp_data/Metadata/global-metadata.dat"
            || profile.map_scene.relative_path != "Bundles/maps/maps.bundle"
        {
            return Err("Invalid bundled clutter topology profile".into());
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
    if maps.clutter_topology.is_some() {
        return Ok(true);
    }
    if catalog.installation.kind != crate::installation::InstallationKind::Client {
        return Ok(true);
    }
    let Some(profile) = selected(&catalog.installation.native.sha256)? else {
        return Ok(true);
    };
    let root = bindings.installation_root();
    for receipt in profile
        .code_sources
        .iter()
        .chain(std::iter::once(&profile.map_scene))
    {
        if cancelled() {
            return Ok(false);
        }
        if crate::installation::hash_file(
            root,
            &receipt.relative_path,
            1024 * 1024 * 1024,
            &cancelled,
        )? != *receipt
        {
            return Err(
                "Clutter topology profile differs from the selected Rust installation".into(),
            );
        }
    }
    let infrastructure = if let Some(recipe) = &project.roads {
        crate::infrastructure::Source::cached_shared(catalog, recipe, &cancelled)?
    } else {
        std::sync::Arc::new(crate::infrastructure::Source::discover(catalog, None, &cancelled)?)
    };
    let source = crate::object_extract::prepare_roads(
        root,
        &infrastructure,
        project
            .objects
            .as_ref()
            .map(|r| r.road_source_sha256.as_str()),
        &cancelled,
    )?;
    let stages = source.procedural_order()?;
    let mut matches = stages
        .iter()
        .filter(|s| s.kind == "GenerateClutterTopology");
    let stage = matches
        .next()
        .ok_or("Missing clutter topology stage in the selected client")?;
    if matches.next().is_some() {
        return Err("Ambiguous clutter topology stage".into());
    }
    let enabled = active(stage);
    let mut topology = None;
    let report = if enabled {
        let Some((cells, report)) = clutter_topology::apply(
            &maps.layers.topology,
            maps.layers.surface_resolution,
            profile.policy,
            64_000_000,
            &cancelled,
        )?
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
    let provenance = serde_json::json!({"engine":clutter_topology::VERSION,"source_sha256":source.sha256,
        "object_id":stage.object_id,"scene_order":stage.order,"executed":enabled,
        "process_sha256":profile.process_sha256,"callback_sha256":profile.callback_sha256,"report":report});
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
    maps.clutter_topology = Some(provenance);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
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
            kind: "GenerateClutterTopology".into(),
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
