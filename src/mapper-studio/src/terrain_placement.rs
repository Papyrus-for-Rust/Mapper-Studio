//! Source-ordered natural terrain prefabs, before hydraulic erosion and rivers.
use crate::{
    Project, composition, erosion_source, monument_source, placement_preview,
    prefab_assets::Catalog, prefab_textures::Bindings,
};
use papyrus_terrain::{
    monument_composition,
    native_layers::Layers,
    placement::{Outcome, Solution},
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Serialize)]
pub struct StageReport {
    pub component_id: i64,
    pub order: usize,
    pub active: bool,
    pub seed: u32,
    pub targets: usize,
    pub placed: usize,
    pub clamped_height_samples: usize,
}

pub struct Output {
    pub recipe: Project,
    pub layers: Layers,
    pub alpha: Vec<u8>,
    pub solution: Solution,
    pub stages: Vec<StageReport>,
    pub proposals: BTreeMap<u64, [f32; 3]>,
}

fn phase_recipe(project: &Project, ids: &BTreeSet<u64>, prior: &Solution) -> Project {
    let mut phase = project.clone();
    phase.seeds[4] = crate::StageSeed {
        value: project.seeds[0].value,
        inherited: false,
    };
    phase.targets.retain(|t| {
        ids.contains(&t.instance_id) || prior.outcomes.iter().any(|o| o.id == t.instance_id)
    });
    phase
}

/// Place and stamp each early source phase before assessing the next phase.
/// The caller supplies the immutable terrain entering those source components.
/// Cancelled or rejected work remains private to this operation.
pub fn generate(
    project: &Project,
    base: &Layers,
    catalog: &Catalog,
    bindings: &Bindings,
    erosion: &erosion_source::Source,
    cancelled: impl Fn() -> bool + Sync,
    mut progress: impl FnMut(usize, usize),
) -> Result<Option<Output>, String> {
    project.validate()?;
    project.check_monument_assets(&catalog.sha256, &bindings.sha256)?;
    if base.size != project.size
        || base.terrain_seed != project.seeds[0].value
        || base.biome_seed != project.seeds[1].value
    {
        return Err("Early terrain source differs from the generation recipe".into());
    }
    if cancelled() {
        return Ok(None);
    }
    let source = monument_source::Source::cached_shared(
        project
            .monument_rules
            .as_ref()
            .ok_or("Missing early terrain placement rules")?,
        catalog,
        bindings,
        &cancelled,
    )?;
    let stages = source
        .stage_source
        .as_ref()
        .ok_or("Missing early terrain source programs")?;
    let groups = erosion.early_groups(stages)?;
    let mut effective = project.clone();
    effective.monument_rules.as_mut().unwrap().surface_modifiers = source
        .recipe_with_source_defaults(catalog)?
        .surface_modifiers;
    let mut layers = base.clone();
    if let Some(graph) = &project.terrain_graph {
        let mut timing = crate::composition::StageTimer::start("terrain_graph");
        let progress = crate::loading_progress::begin("Evaluating terrain graph", None);
        let Some(height) = crate::generation_resources::with_cpu_pool(&cancelled, |pool, limits| {
            let bytes = limits.terrain_memory_mib.saturating_mul(1024 * 1024)
                .saturating_sub(layers.bytes());
            graph.render(base, bytes, pool, &cancelled)
        })? else {
            return Ok(None);
        };
        if height != layers.height {
            layers.height = height;
            let Some(topology) = papyrus_terrain::ocean::reconstruct_topology(
                &layers.height, layers.height_resolution, &layers.topology,
                layers.surface_resolution, &cancelled,
            )? else {
                return Ok(None);
            };
            layers.topology = topology;
        }
        crate::composition::record_timing(&mut timing, "evaluated");
        progress.reporter().stage("Placing terrain monuments");
    }
    if let Some(intensity) = project.options.island_intensity.filter(|value| *value > 0) {
        let reporter = crate::loading_progress::begin("Shaping offshore islands", None);
        if !add_islands(&mut layers, intensity, &cancelled)? {
            return Ok(None);
        }
        drop(reporter);
    }
    let mut alpha = vec![255; base.surface_resolution.pow(2)];
    let mut prior = placement_preview::CanonicalPlacement {
        solution: Solution {
            outcomes: Vec::new(),
            required_missing: 0,
            attempts: 0,
        },
        proposals: BTreeMap::new(),
    };
    let mut reports = Vec::new();
    for (index, (stage, paths)) in groups.iter().enumerate() {
        if cancelled() {
            return Ok(None);
        }
        progress(index, groups.len());
        let paths = paths.iter().collect::<BTreeSet<_>>();
        let ids = effective
            .targets
            .iter()
            .filter(|t| t.prefab_path.as_ref().is_some_and(|p| paths.contains(p)))
            .map(|t| t.instance_id)
            .collect::<BTreeSet<_>>();
        let mut report = StageReport {
            component_id: stage.object_id,
            order: stage.order,
            active: stage.active,
            seed: effective.seeds[0].value,
            targets: ids.len(),
            placed: 0,
            clamped_height_samples: 0,
        };
        if ids.is_empty() {
            reports.push(report);
            continue;
        }
        if !stage.active {
            prior
                .solution
                .outcomes
                .extend(ids.iter().map(|&id| Outcome {
                    id,
                    position: None,
                    distance: None,
                    within_radius: false,
                    reason:
                        "Terrain placement phase is inactive in the selected Rust workflow".into(),
                }));
        } else {
            let phase = phase_recipe(&effective, &ids, &prior.solution);
            let Some(selected) = placement_preview::solve_native_phase(
                &phase, &layers, catalog, bindings, &prior, &cancelled,
            )?
            else {
                return Ok(None);
            };
            let placed = selected
                .solution
                .outcomes
                .iter()
                .filter(|o| ids.contains(&o.id) && o.position.is_some())
                .collect::<Vec<_>>();
            report.placed = placed.len();
            if !placed.is_empty() {
                let Some(composed) =
                    monument_composition::compose_owned_streamed(layers, alpha, &cancelled, |apply| {
                        for outcome in placed {
                            if cancelled() {
                                return Ok(false);
                            }
                            let position = outcome.position.as_ref().unwrap();
                            let target = phase
                                .targets
                                .iter()
                                .find(|t| t.instance_id == outcome.id)
                                .ok_or("Missing early terrain target")?;
                            let asset = target
                                .prefab_path
                                .as_ref()
                                .and_then(|p| catalog.entries.get(p))
                                .ok_or("Missing early terrain prefab")?;
                            if composition::omit_terrain_modifier(&phase, asset, position)? {
                                continue;
                            }
                            let Some(prepared) =
                                composition::prepare(asset, bindings, phase.size, &cancelled)?
                            else {
                                return Ok(false);
                            };
                            let stamp = prepared.stamp_for_recipe(
                                [position.x, position.y, position.z].map(f64::from),
                                f64::from(position.yaw),
                                phase.composition_engine.as_deref(),
                            );
                            if !apply(&stamp)? {
                                return Ok(false);
                            }
                        }
                        Ok(true)
                    })?
                else {
                    return Ok(None);
                };
                layers = composed.layers;
                alpha = composed.alpha;
                report.clamped_height_samples = composed.clamped_height_samples;
            }
            prior = selected;
        }
        reports.push(report);
    }
    if cancelled() {
        return Ok(None);
    }
    prior.solution.outcomes.sort_by_key(|o| o.id);
    prior.solution.required_missing = effective
        .targets
        .iter()
        .filter(|t| {
            effective.monument_presence(t) == crate::Presence::Required
                && prior
                    .solution
                    .outcomes
                    .iter()
                    .any(|o| o.id == t.instance_id && o.position.is_none())
        })
        .count();
    progress(groups.len(), groups.len());
    Ok(Some(Output {
        recipe: effective,
        layers,
        alpha,
        solution: prior.solution,
        stages: reports,
        proposals: prior
            .proposals
            .into_iter()
            .map(|(id, p)| (id, [p.x, p.z, p.yaw]))
            .collect(),
    }))
}

/// Islands only occupy existing shallow ocean. Use a deterministic candidate
/// order so raising intensity tries additional sites with the same terrain seed.
fn add_islands(layers: &mut Layers, intensity: u8, cancelled: &impl Fn() -> bool) -> Result<bool, String> {
    let n = layers.height_resolution;
    let s = layers.surface_resolution;
    let stride = n - 1;
    let density = stride as f32 / layers.size as f32;
    let mut placed = 0;
    for candidate in 0..384 {
        if cancelled() {
            return Ok(false);
        }
        let x = crate::derive_seed(layers.terrain_seed, &format!("island-x-{candidate}")) as usize % n;
        let z = crate::derive_seed(layers.terrain_seed, &format!("island-z-{candidate}")) as usize % n;
        let radius = ((50. + (candidate % 5) as f32 * 8.) * density).round() as usize;
        let margin = radius * 2;
        if x < margin || z < margin || x + margin >= n || z + margin >= n {
            continue;
        }
        let center = layers.height[z * n + x];
        if !(15_400..=16_245).contains(&center)
            || [(x - margin, z), (x + margin, z), (x, z - margin), (x, z + margin)]
                .iter()
                .any(|&(x, z)| layers.height[z * n + x] >= 16_300)
        {
            continue;
        }
        let mut clear = true;
        for row in z - radius..=z + radius {
            if layers.height[row * n + x - radius..=row * n + x + radius]
                .iter().any(|&height| height > 16_300)
            {
                clear = false;
                break;
            }
        }
        if !clear {
            continue;
        }
        let radius_sq = (radius * radius) as f32;
        let lift = f32::from(16_930 - center);
        for row in z - radius..=z + radius {
            for col in x - radius..=x + radius {
                let distance = (row.abs_diff(z).pow(2) + col.abs_diff(x).pow(2)) as f32 / radius_sq;
                if distance < 1. {
                    let profile = 1. - distance;
                    layers.height[row * n + col] += (profile * profile * lift).round() as i16;
                }
            }
        }
        placed += 1;
        if placed == intensity {
            break;
        }
    }
    if placed > 0 {
        let Some(topology) = papyrus_terrain::ocean::reconstruct_topology(
            &layers.height, n, &layers.topology, s, cancelled,
        )? else { return Ok(false); };
        layers.topology = topology;
    }
    Ok(!cancelled())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn natural_terrain_uses_the_terrain_seed_and_preserves_the_saved_recipe() {
        let project = Project::default();
        let mut alternate = project.clone();
        alternate.seeds[4] = crate::StageSeed {
            value: 7654321,
            inherited: false,
        };
        let ids = [project.targets[1].instance_id].into_iter().collect();
        let prior = Solution {
            outcomes: vec![Outcome {
                id: project.targets[0].instance_id,
                position: None,
                distance: None,
                within_radius: false,
                reason: "Earlier phase".into(),
            }],
            required_missing: 0,
            attempts: 8,
        };
        let phase = phase_recipe(&project, &ids, &prior);
        assert_eq!(phase, phase_recipe(&alternate, &ids, &prior));
        assert_eq!(phase.seeds[4].value, project.seeds[0].value);
        assert!(!phase.seeds[4].inherited);
        assert_eq!(phase.terrain_key(), project.terrain_key());
        assert_eq!(
            phase
                .targets
                .iter()
                .map(|t| t.instance_id)
                .collect::<Vec<_>>(),
            [
                project.targets[0].instance_id,
                project.targets[1].instance_id
            ]
        );
        phase.validate().unwrap();
        assert_eq!(project, Project::default());
        assert_eq!(alternate.seeds[4].value, 7654321);
    }
}
