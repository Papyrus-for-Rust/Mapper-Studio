//! Requested-output checks shared by desktop and command-line map export.
use crate::{Presence, Project};
use papyrus_terrain::{monument_composition::Composed, placement::Solution};
use std::collections::{BTreeMap, BTreeSet};

pub fn required_monuments_issue(project: &Project, solution: &Solution) -> Option<String> {
    let unplaced: BTreeSet<_> = solution
        .outcomes
        .iter()
        .filter(|outcome| outcome.position.is_none())
        .map(|outcome| outcome.id)
        .collect();
    let required: Vec<_> = project
        .targets
        .iter()
        .filter(|target| {
            project.performance.monuments
                && (target.presence == Presence::Required
                    || (target.max_displacement_m.is_some() && project.requires_monument(target)))
                && unplaced.contains(&target.instance_id)
        })
        .collect();
    let missing = required.len().max(solution.required_missing);
    if let [target] = required.as_slice()
        && missing == 1
        && let Some(limit) = target.max_displacement_m
    {
        return Some(format!("{} could not be placed within {limit} m of its preferred location", target.name));
    }
    let mut message = match missing {
        0 => return None,
        1 => "1 required monument could not be placed".into(),
        _ => format!("{missing} required monuments could not be placed"),
    };
    for target in required {
        message.push('\n');
        message.push_str(&target.name);
        if let Some(limit) = target.max_displacement_m {
            message.push_str(&format!(" (no site within {limit} m of its preferred location)"));
        }
    }
    Some(message)
}

pub fn validate(project: &Project, maps: &Composed, solution: &Solution) -> Result<(), String> {
    project.validate()?;
    if maps.layers.size != project.size
        || maps.layers.terrain_seed != project.seeds[0].value
        || maps.layers.biome_seed != project.seeds[1].value
    {
        return Err("Generated terrain belongs to a different recipe".into());
    }
    if solution.outcomes.len() != project.targets.len() {
        return Err("Generated monument roster differs from the recipe".into());
    }
    let mut outcomes = BTreeMap::new();
    for outcome in &solution.outcomes {
        if outcomes.insert(outcome.id, outcome).is_some() {
            return Err("Duplicate monument outcome".into());
        }
    }
    let mut required_unplaced = false;
    for target in &project.targets {
        let outcome = outcomes
            .get(&target.instance_id)
            .ok_or("Missing monument outcome")?;
        if project.performance.monuments
            && (target.presence == Presence::Required
                || (target.max_displacement_m.is_some() && project.requires_monument(target)))
            && outcome.position.is_none()
        {
            required_unplaced = true;
        }
        if let (Some(limit), Some(position)) = (target.max_displacement_m, &outcome.position)
            && (position.x - target.x).hypot(position.z - target.z) > limit as f32
        {
            return Err(format!("{} is outside its {limit} m move limit", target.name));
        }
        if (!project.performance.monuments || target.presence == Presence::Excluded)
            && outcome.position.is_some()
        {
            return Err(format!(
                "Disabled monument remains in the generated map. {}",
                target.name
            ));
        }
    }
    if (required_unplaced || solution.required_missing != 0)
        && let Some(issue) = required_monuments_issue(project, solution)
    {
        return Err(issue);
    }
    let topology = project
        .topology_edits
        .as_ref()
        .filter(|edits| edits.enabled && !edits.strokes.is_empty());
    for (requested, present, name) in [
        (project.roads.is_some(), maps.roads.is_some(), "roads"),
        (project.rivers.is_some(), maps.rivers.is_some(), "rivers"),
        (
            project.rails.is_some(),
            maps.rails.is_some(),
            "surface rails",
        ),
        (
            project.objects.is_some(),
            maps.objects.is_some(),
            "shared objects",
        ),
        (
            project.decoration.is_some() && project.objects.is_none(),
            maps.decoration.is_some(),
            "scenery",
        ),
        (
            topology.is_some(),
            maps.topology_overrides.is_some(),
            "topology edits",
        ),
    ] {
        if requested != present {
            return Err(if requested {
                format!("Requested {name} output is missing")
            } else {
                format!("Generated {name} output is absent from the recipe")
            });
        }
    }
    if let (Some(recipe), Some(rivers)) = (&project.rivers, &maps.rivers) {
        recipe.verify_world(rivers, project.seeds[2].value)?;
    }
    if let (Some(recipe), Some(roads)) = (&project.roads, &maps.roads) {
        recipe.verify_report(roads, project.seeds[3].value)?;
    }
    if let Some(scene) = &maps.objects
        && (scene.road_seed != project.seeds[3].value
            || scene.decoration_seed != project.seeds[5].value
            || project.objects.as_ref().is_none_or(|recipe| {
                recipe.engine != scene.engine
                    || recipe.road_source_sha256 != scene.road_source_sha256
                    || recipe.uniform_cliffs_source_sha256 != scene.uniform_cliffs_source_sha256
                    || recipe.coastal_cliffs_source_sha256 != scene.coastal_cliffs_source_sha256
                    || recipe.jungle_vines_source_sha256 != scene.jungle_vines_source_sha256
            })
            || project
                .decoration
                .as_ref()
                .is_none_or(|recipe| recipe.source_sha256 != scene.decoration_source_sha256)
            || project
                .monument_rules
                .as_ref()
                .is_none_or(|recipe| recipe.source_sha256 != scene.monument_source_sha256))
    {
        return Err("Shared object sources or seeds differ from the recipe".into());
    }
    if let Some(surfaces) = maps.roads.as_ref().and_then(|roads| roads.surfaces.as_ref()) {
        crate::water_treatment_stage::validate_committed(&surfaces.report["staged_pipeline"]["pipes"], project)?;
    }
    if let Some(scene) = &maps.objects
        && scene.uniform_cliffs_source_sha256.is_some()
        && (scene
            .external_phases
            .iter()
            .any(|phase| phase.class == "PlaceCliffsUniform")
            || !scene
                .decoration
                .iter()
                .any(|report| report.engine == papyrus_terrain::uniform_cliffs::VERSION))
    {
        return Err("Requested hill-cliff placement is incomplete".into());
    }
    if let Some(scene) = &maps.objects
        && scene.coastal_cliffs_source_sha256.is_some()
        && (scene.external_phases.iter().any(|p| p.class == "PlaceCliffs")
            || !scene.decoration.iter().any(|r| r.engine == papyrus_terrain::coastal_cliffs::VERSION)) {
        return Err("Requested coastal cliff placement is incomplete".into());
    }
    if let Some(scene) = &maps.objects
        && scene.jungle_vines_source_sha256.is_some()
        && scene.external_phases.iter().any(|phase| phase.class == "PlaceJungleVines")
    {
        return Err("Requested vine-tree placement is incomplete".into());
    }
    if let Some(scene) = &maps.decoration
        && (project
            .decoration
            .as_ref()
            .is_none_or(|recipe| recipe.source_sha256 != scene.source_sha256)
            || scene.seed != project.seeds[5].value)
    {
        return Err("Scenery source or seed differs from the recipe".into());
    }
    if let Some(report) = &maps.topology_overrides
        && topology.is_none_or(|edits| {
            edits.source_sha256 != report.source_sha256
                || edits.engine != report.engine
                || edits.strokes.len() != report.strokes
        })
    {
        return Err("Topology edits differ from the accepted recipe".into());
    }
    if let (Some(recipe), Some(rails)) = (&project.rails, &maps.rails) {
        recipe.verify_report(&rails.report)?;
        let meshes = &rails.report["meshes"];
        if rails.tracks.is_empty()
            || rails.meshes.is_empty()
            || meshes["chunks"].as_u64() != Some(rails.meshes.len() as u64)
            || meshes["tracks"].as_array().is_none_or(|tracks| tracks.len() != rails.tracks.len())
        {
            return Err("Surface rails are missing their completed track meshes".into());
        }
        if maps.gameplay_height.as_ref().is_none_or(|height| height.height.len() != maps.layers.height.len())
            || rails.report["gameplay_height"]["scope"].as_str() != Some("rail collision triangles")
            || rails.report["gameplay_height"]["changes"]["engine"].as_str()
                != Some(papyrus_terrain::gameplay_height::VERSION)
        {
            return Err("Surface rails are missing their completed gameplay height".into());
        }
    }
    if papyrus_terrain::monument_composition::water_aware(project.composition_engine.as_deref())
        && maps.ocean.is_none()
    {
        return Err("Water-aware composition is missing its completed water layers".into());
    }
    validate_monument_assemblies(project, maps, solution)
}

fn validate_monument_assemblies(
    project: &Project,
    maps: &Composed,
    solution: &Solution,
) -> Result<(), String> {
    let rules = project
        .monument_rules
        .as_ref()
        .filter(|_| project.performance.monuments);
    let dungeons = rules.and_then(|rules| rules.dungeon_source.as_ref());
    let offshore = rules.and_then(|rules| rules.offshore_source.as_ref());
    if dungeons.map(|binding| binding.source_sha256.as_str())
        != maps
            .dungeons
            .as_ref()
            .map(|scene| scene.source_sha256.as_str())
    {
        return Err("Map export requires the matching completed dungeon assembly".into());
    }
    if offshore.map(|binding| binding.source_sha256.as_str())
        != maps
            .offshore
            .as_ref()
            .map(|scene| scene.source_sha256.as_str())
    {
        return Err("Map export requires the matching completed offshore stage".into());
    }
    let Some(rules) = rules else {
        if maps.tunnels.is_some() {
            return Err("Underground assembly does not belong to this recipe".into());
        }
        return Ok(());
    };
    if maps.tunnels.is_none() && maps.dungeons.is_none() && maps.offshore.is_none() {
        return Ok(());
    }

    let layout = crate::composition::layout_fingerprint(solution);
    let seed = project.seeds[4].value;
    if let Some(scene) = &maps.tunnels
        && (scene.engine != papyrus_terrain::tunnel_scene::VERSION
            || scene.monument_sha256 != rules.source_sha256
            || scene.seed != crate::derive_seed(seed, "Underground rails")
            || scene.layout_fingerprint != layout
            || scene.instances.len() > 65536)
    {
        return Err("Underground assembly differs from the accepted source or map layout".into());
    }
    if let Some(scene) = &maps.dungeons
        && (dungeons.is_none_or(|binding| binding.engine != scene.engine)
            || scene.monument_sha256 != rules.source_sha256
            || scene.seed != seed
            || scene.layout_fingerprint != layout)
    {
        return Err("Dungeon assembly differs from the accepted map layout".into());
    }
    if let Some(scene) = &maps.offshore
        && (offshore.is_none_or(|binding| binding.engine != scene.engine)
            || scene.monument_sha256 != rules.source_sha256
            || scene.seed != seed
            || scene.layout_fingerprint != layout)
    {
        return Err("Offshore assembly differs from the accepted map layout".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
