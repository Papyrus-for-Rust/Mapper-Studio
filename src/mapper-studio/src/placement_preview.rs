//! Adapts project preferences to the independent terrain engine's placement search.
use crate::{Presence, Project};
pub const LEGACY_EARTHWORK_VERSION: &str = "papyrus-placement-earthwork-1";
pub const EARTHWORK_VERSION: &str = "papyrus-placement-earthwork-2";
pub const BALANCED_EARTHWORK_VERSION: &str = "papyrus-placement-earthwork-3";
pub const BOUNDS_EARTHWORK_VERSION: &str = "papyrus-placement-earthwork-4";
pub fn has_source_bounds(version: &str) -> bool {
    version == BOUNDS_EARTHWORK_VERSION || crate::monument_source::supported(version)
}
pub fn is_balanced(version: &str) -> bool {
    crate::monument_source::supported(version)
        || matches!(
            version,
            BALANCED_EARTHWORK_VERSION | BOUNDS_EARTHWORK_VERSION
        )
}
pub fn is_earthwork(version: &str) -> bool {
    crate::monument_source::supported(version)
        || matches!(
            version,
            LEGACY_EARTHWORK_VERSION
                | EARTHWORK_VERSION
                | BALANCED_EARTHWORK_VERSION
                | BOUNDS_EARTHWORK_VERSION
        )
}
pub const ROTATION_VERSION: &str = "papyrus-placement-orientation-1";
use papyrus_terrain::{
    monuments::Entry,
    placement::{self, Outcome, Solution, Target, Terrain},
};
use std::collections::HashMap;

pub struct Template {
    pub name: String,
    pub path: String,
    pub entry: Entry,
}
/// Installation-backed targets use authored bounds and yaw, never the legacy roster.
/// These are candidates for Mapper's land search, not recovered game eligibility rules.
pub fn installation_templates(catalog: &crate::prefab_assets::Catalog) -> Vec<Template> {
    let mut paths: Vec<_> = catalog.entries.iter().collect();
    paths.sort_by_key(|(path, _)| *path);
    paths
        .into_iter()
        .filter_map(|(path, asset)| {
            let bounds = asset.bounds.as_ref()?;
            let [extent_x, extent_z] = bounds.horizontal_extent();
            let [min, max] = asset.yaw.unwrap_or([0., 360.]);
            Some(Template {
                name: path
                    .rsplit('/')
                    .next()
                    .unwrap_or(path)
                    .trim_end_matches(".prefab")
                    .replace('_', " "),
                path: path.clone(),
                entry: Entry {
                    group: 0,
                    resolved: 0,
                    land: true,
                    minimum_size: bounds.minimum_world_size as i32,
                    extent_x,
                    extent_z,
                    priority: 0,
                    minimum_yaw: -max,
                    maximum_yaw: -min,
                },
            })
        })
        .collect()
}
pub fn project_templates(
    project: &Project,
    catalog: Option<&crate::prefab_assets::Catalog>,
) -> Vec<Template> {
    if crate::native_preview::is_native(&project.preview_engine) {
        catalog
            .filter(|c| project.installation.as_ref() == Some(&c.installation))
            .map(installation_templates)
            .unwrap_or_default()
    } else {
        templates()
    }
}
/// Adds one optional preference per supported height-stamp asset, preserving existing edits.
/// This is a requested roster, not a promise of runtime eligibility or feasible placement.
pub fn populate_targets(
    project: &mut Project,
    catalog: &crate::prefab_assets::Catalog,
) -> Result<usize, String> {
    if !crate::native_preview::is_native(&project.preview_engine)
        || project.installation.as_ref() != Some(&catalog.installation)
    {
        return Err("Select the matching installed terrain backend first".into());
    }
    if project
        .monument_assets
        .as_ref()
        .is_some_and(|pins| pins.catalog_sha256 != catalog.sha256)
    {
        return Err(
            "Installation roster does not match the project's pinned monument catalog".into(),
        );
    }
    let templates = installation_templates(catalog);
    let mut next = project.clone();
    let mut added = 0;
    for template in templates {
        if next.targets.len() >= papyrus_terrain::placement::MAX_TARGETS {
            break;
        }
        let asset = &catalog.entries[&template.path];
        if next
            .targets
            .iter()
            .any(|t| t.prefab_path.as_ref() == Some(&template.path))
            || template.entry.minimum_size > next.size as i32
            || asset.modifier.as_ref().is_none_or(|m| !m.height_map)
            || asset.texture_guids.contains_key("water")
            || asset.modifier_size.is_none()
            || asset.modifier_offset.is_none()
            || asset.splat_mask.is_none()
            || !["height", "splat0", "splat1", "blend"]
                .iter()
                .all(|key| asset.texture_guids.contains_key(*key))
        {
            continue;
        }
        next.add_target(template.name, template.path)?;
        let n = next.targets.len() as f32;
        let angle = n * 2.3999631 + (next.seeds[4].value % 360) as f32;
        let radius = next.size as f32 * (0.12 + 0.25 * ((n * 0.618034).fract()));
        let t = next.targets.last_mut().unwrap();
        t.x = angle.cos() * radius;
        t.z = angle.sin() * radius;
        t.radius = next.size as f32;
        t.presence = Presence::Allowed;
        added += 1;
    }
    // Roster editing must retain the user's installed anchors, coastal fitting,
    // tier/earthwork choices and source version. Only an unbound legacy roster
    // needs the authored-bounds search selected by this explicit action.
    if !crate::monument_source::supported(&next.placement_engine) {
        next.placement_engine = BOUNDS_EARTHWORK_VERSION.into();
    }
    next.composition_engine
        .get_or_insert_with(|| papyrus_terrain::monument_composition::WATER_VERSION.into());
    next.validate()?;
    *project = next;
    Ok(added)
}
/// Add the scene's eligible land-stage candidate pool. The solver applies shared
/// stage quotas after fitting, so an unsuitable candidate cannot consume a slot.
pub fn populate_stage_targets(
    project: &mut Project,
    catalog: &crate::prefab_assets::Catalog,
    stages: &crate::monument_stages::Source,
) -> Result<usize, String> {
    if !crate::native_preview::is_native(&project.preview_engine)
        || project.installation.as_ref() != Some(&catalog.installation)
    {
        return Err("Select the matching installed terrain backend first".into());
    }
    if project
        .monument_assets
        .as_ref()
        .is_none_or(|pins| pins.catalog_sha256 != catalog.sha256)
    {
        return Err("Population catalog differs from the project's monument asset pins".into());
    }
    if project
        .monument_rules
        .as_ref()
        .and_then(|r| r.stage_rules.as_ref())
        .is_none_or(|binding| binding.source_sha256 != stages.sha256)
    {
        return Err("Population source differs from the project's pinned stage rules".into());
    }
    let population = stages.population(catalog, project.size)?;
    let mut next = project.clone();
    let mut added = 0;
    for (path, _, _) in population {
        if next
            .targets
            .iter()
            .any(|t| t.prefab_path.as_ref() == Some(&path))
        {
            continue;
        }
        let name = path
            .rsplit('/')
            .next()
            .unwrap_or(&path)
            .trim_end_matches(".prefab")
            .replace('_', " ");
        next.add_target(name, path)?;
        let target = next.targets.last_mut().unwrap();
        target.x = 0.;
        target.z = 0.;
        target.radius = next.size as f32;
        target.presence = Presence::Allowed;
        added += 1;
    }
    next.monument_rules
        .as_mut()
        .and_then(|r| r.stage_rules.as_mut())
        .ok_or("Bind installation monument rules before building its roster")?
        .population = Some(crate::monument_stages::POPULATION_VERSION.into());
    next.validate()?;
    *project = next;
    Ok(added)
}
pub fn templates() -> Vec<Template> {
    papyrus_terrain::registry::catalog_estimates()
        .into_iter()
        .map(|(path, entry)| {
            let stem = path
                .rsplit('/')
                .next()
                .unwrap_or("")
                .trim_end_matches(".prefab");
            let name = match stem {
                "launch_site_1" => "Launch Site",
                "airfield_1" => "Airfield",
                "satellite_dish" => "Satellite Dish",
                "sphere_tank" => "Dome",
                "powerplant_1" => "Power Plant",
                "trainyard_1" => "Train Yard",
                "mining_quarry_a" => "Mining Quarry A",
                "mining_quarry_b" => "Mining Quarry B",
                "mining_quarry_c" => "Mining Quarry C",
                "junkyard_1" => "Junkyard",
                "stables_a" => "Stables A",
                "stables_b" => "Stables B",
                "gas_station_1" => "Gas Station",
                "supermarket_1" => "Supermarket",
                "warehouse" => "Warehouse",
                "radtown_1" => "Radtown",
                "radtown_small_3" => "Small Radtown",
                "apartments_complex_1" => "Apartments",
                "water_well_a" => "Water Well A",
                "water_well_b" => "Water Well B",
                "water_well_c" => "Water Well C",
                _ => stem,
            }
            .to_string();
            Template { name, path, entry }
        })
        .collect()
}
pub fn path_for_name(name: &str) -> Option<String> {
    templates()
        .into_iter()
        .find(|t| t.name == name)
        .map(|t| t.path)
}

/// Revision 7 divides work by conservative raster scan area, rather than target count.
/// The required-target multiplier affects search effort, not physical acceptance rules.
fn sized_earthwork_allowances(
    project: &Project,
    catalog: &crate::prefab_assets::Catalog,
) -> std::collections::BTreeMap<u64, u64> {
    let weights = project.targets.iter().filter_map(|t| {
        if project.monument_presence(t) == Presence::Excluded {
            return None;
        }
        let asset = catalog.entries.get(t.prefab_path.as_ref()?)?;
        if asset.bounds.as_ref()?.minimum_world_size > project.size {
            return None;
        }
        let size = asset.modifier_size.unwrap_or([0.; 3]);
        let radius = asset.modifier.as_ref().map_or(0., |m| m.radius);
        let radius = if radius == 0. { size[0] / 2. } else { radius };
        let hx = f64::from((size[0] / 2.).max(radius));
        let hz = f64::from((size[2] / 2.).max(radius));
        // Maximum yaw-rotated bounding rectangle, including a cell on either edge.
        let density = papyrus_terrain::native_layers::dimensions(project.size)
            .map_or(1., |(hr, _)| hr as f64 / project.size as f64);
        let side = (hx + hz) * std::f64::consts::SQRT_2 * density + 2.;
        let weight = side.powi(2).ceil().clamp(1., 4097_f64.powi(2)) as u64;
        Some((
            t.instance_id,
            weight
                * if project.requires_monument(t) {
                    2
                } else {
                    1
                },
        ))
    });
    allocate_earthwork(weights)
}

fn allocate_earthwork(
    weights: impl Iterator<Item = (u64, u64)>,
) -> std::collections::BTreeMap<u64, u64> {
    let weights: std::collections::BTreeMap<_, _> = weights.collect();
    let total: u64 = weights.values().sum();
    // A failed bounded scan counts its first over-budget visit. Reserve one per target.
    let available = papyrus_terrain::monument_composition::MAX_EARTHWORK_SAMPLES
        .saturating_sub(2 * weights.len() as u64);
    weights
        .into_iter()
        .map(|(id, weight)| (id, 1 + available * weight / total.max(1)))
        .collect()
}

pub(crate) fn authored_earthwork_limit(
    rules: &papyrus_terrain::monument_eligibility::Rules,
    ceiling: f32,
) -> f64 {
    // Anchor intervals bound relief relative to the authored anchor height.
    // Keep the historical floor; never let a large source value escape the
    // installation-bound execution profile's ceiling. Full raster checks follow.
    rules
        .anchors
        .iter()
        .map(|a| f64::from(a.extents) + f64::from(a.offset.abs()))
        .fold(24_f64, f64::max)
        .min(f64::from(ceiling).max(24.))
}

/// Source-verified, full-resolution earthwork search; never falls back to raw slope checks.
pub fn solve_native(
    project: &Project,
    layers: &papyrus_terrain::native_layers::Layers,
    catalog: &crate::prefab_assets::Catalog,
    bindings: &crate::prefab_textures::Bindings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Solution>, String> {
    solve_native_observed(project, layers, catalog, bindings, cancelled, |_, _| {})
}

/// Observes completed assessments without changing proposals, budgets or packing.
/// Cancellation and errors never turn a partially assessed candidate into acceptance.
pub fn solve_native_observed(
    project: &Project,
    layers: &papyrus_terrain::native_layers::Layers,
    catalog: &crate::prefab_assets::Catalog,
    bindings: &crate::prefab_textures::Bindings,
    cancelled: impl Fn() -> bool,
    observe: impl FnMut(u64, &placement::CandidateCheck),
) -> Result<Option<Solution>, String> {
    solve_native_inner(
        project, layers, catalog, bindings, cancelled, observe, None, None,
    )
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RawProposal {
    pub x: f32,
    pub z: f32,
    pub yaw: f32,
}
type CapturedProposal = (u64, placement::Position, RawProposal);
pub(crate) struct CanonicalPlacement {
    pub solution: Solution,
    pub proposals: std::collections::BTreeMap<u64, RawProposal>,
}
/// Retain the proposal BEFORE source decorators, for the exact accepted poses
/// chosen by packing. Replay must not apply decorators to their own output.
pub(crate) fn solve_native_canonical(
    project: &Project,
    layers: &papyrus_terrain::native_layers::Layers,
    catalog: &crate::prefab_assets::Catalog,
    bindings: &crate::prefab_textures::Bindings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<CanonicalPlacement>, String> {
    if !is_balanced(&project.placement_engine) {
        return Err("Proposal capture requires bounded balanced native placement".into());
    }
    let mut captured = Vec::new();
    let Some(solution) = solve_native_inner(
        project,
        layers,
        catalog,
        bindings,
        cancelled,
        |_, _| {},
        None,
        Some(&mut captured),
    )?
    else {
        return Ok(None);
    };
    let proposals = select_proposals(&solution, &captured)?;
    Ok(Some(CanonicalPlacement {
        solution,
        proposals,
    }))
}
fn select_proposals(
    solution: &Solution,
    captured: &[CapturedProposal],
) -> Result<std::collections::BTreeMap<u64, RawProposal>, String> {
    let bits = |p: &placement::Position| [p.x, p.y, p.z, p.yaw, p.clearance].map(f32::to_bits);
    solution
        .outcomes
        .iter()
        .filter_map(|o| o.position.as_ref().map(|p| (o.id, p)))
        .map(|(id, p)| {
            captured
                .iter()
                .find(|(key, pose, _)| *key == id && bits(pose) == bits(p))
                .map(|(_, _, raw)| (id, *raw))
                .ok_or_else(|| "Accepted native pose has no original proposal".into())
        })
        .collect()
}

/// Internal recovery boundary. Other canonical poses reserve their authored geometry.
/// only the moved target runs fresh source eligibility/earthwork/coastal checks.
/// The caller must supply the same-source, freshly solved canonical layout and
/// reserve the complete retry cost before entering this function.
pub(crate) fn solve_native_replacement(
    project: &Project,
    layers: &papyrus_terrain::native_layers::Layers,
    catalog: &crate::prefab_assets::Catalog,
    bindings: &crate::prefab_textures::Bindings,
    frozen: &Solution,
    selected: u64,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Solution>, String> {
    solve_native_inner(
        project,
        layers,
        catalog,
        bindings,
        cancelled,
        |_, _| {},
        Some(Frozen {
            solution: frozen,
            selected: Some(selected),
        }),
        None,
    )
}

#[derive(Clone, Copy)]
struct Frozen<'a> {
    solution: &'a Solution,
    selected: Option<u64>,
}
impl Frozen<'_> {
    fn retains(self, id: u64) -> bool {
        self.selected != Some(id) && self.solution.outcomes.iter().any(|o| o.id == id)
    }
}

/// Assess one generation phase while retaining completed preceding phases.
pub(crate) fn solve_native_phase(
    project: &Project,
    layers: &papyrus_terrain::native_layers::Layers,
    catalog: &crate::prefab_assets::Catalog,
    bindings: &crate::prefab_textures::Bindings,
    prior: &CanonicalPlacement,
    cancelled: impl Fn() -> bool,
) -> Result<Option<CanonicalPlacement>, String> {
    if prior.solution.outcomes.iter().any(|o| {
        !project.targets.iter().any(|t| t.instance_id == o.id)
            || prior
                .solution
                .outcomes
                .iter()
                .filter(|p| p.id == o.id)
                .count()
                != 1
    }) {
        return Err("Earlier generation phase has a different target roster".into());
    }
    let mut captured = Vec::new();
    let Some(mut solution) = solve_native_inner(
        project,
        layers,
        catalog,
        bindings,
        cancelled,
        |_, _| {},
        Some(Frozen {
            solution: &prior.solution,
            selected: None,
        }),
        Some(&mut captured),
    )?
    else {
        return Ok(None);
    };
    let mut active = solution.clone();
    active
        .outcomes
        .retain(|o| !prior.solution.outcomes.iter().any(|p| p.id == o.id));
    let mut proposals = select_proposals(&active, &captured)?;
    proposals.extend(
        prior
            .proposals
            .iter()
            .map(|(id, proposal)| (*id, *proposal)),
    );
    solution.attempts = solution.attempts.saturating_add(prior.solution.attempts);
    Ok(Some(CanonicalPlacement {
        solution,
        proposals,
    }))
}

#[allow(clippy::too_many_arguments)]
fn solve_native_inner(
    project: &Project,
    layers: &papyrus_terrain::native_layers::Layers,
    catalog: &crate::prefab_assets::Catalog,
    bindings: &crate::prefab_textures::Bindings,
    cancelled: impl Fn() -> bool,
    mut observe: impl FnMut(u64, &placement::CandidateCheck),
    frozen: Option<Frozen<'_>>,
    mut captured: Option<&mut Vec<CapturedProposal>>,
) -> Result<Option<Solution>, String> {
    let mut timing = crate::composition::StageTimer::start("native_monument_placement");
    project.validate()?;
    project.check_monument_assets(&catalog.sha256, &bindings.sha256)?;
    if let Some(Frozen { solution: canonical, selected: Some(selected) }) = frozen
        && (canonical.outcomes.len() != project.targets.len()
            || project.targets.iter().any(|t| !t.exact)
            || !canonical
                .outcomes
                .iter()
                .any(|o| o.id == selected && o.position.is_some()))
    {
        return Err("Replacement assessment requires an exact same-roster canonical layout".into());
    }
    if !is_earthwork(&project.placement_engine)
        || !crate::native_preview::is_native(&project.preview_engine)
        || !project
            .composition_engine
            .as_deref()
            .is_some_and(papyrus_terrain::monument_composition::supported)
        || project.installation.as_ref() != Some(&catalog.installation)
        || bindings.catalog_sha256() != catalog.sha256
        || layers.size != project.size
        || layers.terrain_seed != project.seeds[0].value
        || layers.biome_seed != project.seeds[1].value
    {
        return Err("Earthwork placement recipe/source/assets mismatch".into());
    }
    let terrain = papyrus_terrain::monument_composition::HeightTerrain::new(
        &layers.height,
        layers.height_resolution,
        layers.size,
    )?;
    let omit_stamp = |asset: &crate::prefab_assets::Asset| project
        .monument_rules.as_ref().is_some_and(|r|r.omits_terrain_stamp(asset));
    let mut prepared = std::collections::HashMap::new();
    let balanced = is_balanced(&project.placement_engine);
    let mut unavailable = std::collections::HashMap::new();
    let mut bytes = 0;
    let mut ordered: Vec<_> = project.targets.iter().collect();
    ordered.sort_by_key(|t| t.instance_id);
    for target in ordered {
        if cancelled() {
            return Ok(None);
        }
        if project.monument_presence(target) == Presence::Excluded {
            continue;
        }
        let Some(path) = target.prefab_path.as_deref() else {
            continue;
        };
        if prepared.contains_key(path) || unavailable.contains_key(path) {
            continue;
        }
        let Some(asset) = catalog.entries.get(path) else {
            unavailable.insert(
                path.to_owned(),
                "Missing verified authored monument".to_string(),
            );
            continue;
        };
        if asset.bounds.is_none() {
            unavailable.insert(
                path.to_owned(),
                "Missing verified authored monument bounds".into(),
            );
            continue;
        }
        if balanced || omit_stamp(asset) {
            continue;
        }
        match crate::composition::prepare_with_budget(
            asset,
            bindings,
            project.size,
            &cancelled,
            256 * 1024 * 1024 - bytes,
        ) {
            Ok(Some(item)) => {
                bytes += item.bytes();
                prepared.insert(path.to_owned(), item);
            }
            Ok(None) => return Ok(None),
            Err(error) => {
                unavailable.insert(path.to_owned(), error);
            }
        }
    }
    let mut work = 0_u64;
    let source_rules = if crate::monument_source::supported(&project.placement_engine) {
        Some(crate::monument_source::Source::cached_shared(
            project
                .monument_rules
                .as_ref()
                .ok_or("Missing pinned monument rules")?,
            catalog,
            bindings,
            &cancelled,
        )?)
    } else {
        None
    };
    let coastal_profile = source_rules
        .as_ref()
        .and_then(|s| s.coastal_profile.as_ref());
    let tunnels = match project.monument_rules.as_ref() {
        Some(rules) if source_rules.is_some() && project.performance.monuments => crate::tunnel_source::Source::for_recipe(
            bindings.installation_root(),
            catalog,
            rules,
            &cancelled,
        )?,
        _ => None,
    };
    let source_navigation = project.monument_rules.as_ref().is_some_and(|r| {
        r.coastal_navigation.as_deref() == Some(papyrus_terrain::boat_navigation::SOURCE_VERSION)
    });
    let navigation = if let Some(profile) = coastal_profile {
        let navigation = if source_navigation {
            profile.navigation_source(layers, &cancelled)
        } else {
            profile.navigation(layers, &cancelled)
        };
        match navigation? {
            Some(n) => Some(n),
            None => return Ok(None),
        }
    } else {
        None
    };
    let road_entrances =
        crate::coastal_road_entrances::Prepared::prepare(project, catalog, &cancelled)?;
    let land_access = if project.performance.monuments
        && project.roads.as_ref().and_then(|r| r.staged.as_ref())
        .is_some_and(|s| crate::road_generation::is_connected_source_engine(&s.engine))
        && let Some(entrances) = &road_entrances
    {
        match entrances.land_access(layers, &cancelled)? {
            Some(access) => Some(access),
            None if cancelled() => return Ok(None),
            None => None,
        }
    } else { None };
    crate::composition::record_timing(&mut timing, "base_navigation_and_assets");
    let eligibility_allowance = 64_000_000_usize.saturating_sub(land_access.as_ref().map_or(0, |a| a.work));
    let mut eligibility_work = std::collections::HashMap::<u64, usize>::new();
    let eligible = project
        .targets
        .iter()
        .filter(|t| {
            project.performance.monuments
                && project.monument_presence(t) != Presence::Excluded
                && t.prefab_path.as_ref().is_some_and(|p| {
                    catalog.entries.get(p).is_some_and(|asset| {
                        asset
                            .bounds
                            .as_ref()
                            .is_some_and(|b| b.minimum_world_size <= project.size)
                    })
                })
        })
        .count()
        .max(1) as u64;
    let allowance = papyrus_terrain::monument_composition::MAX_EARTHWORK_SAMPLES / eligible;
    let sized_allowances = (project.placement_engine == crate::coastal_placement::BUDGET_VERSION)
        .then(|| sized_earthwork_allowances(project, catalog));
    let mut target_work = std::collections::HashMap::<u64, u64>::new();
    let retry_tiers = project
        .monument_rules
        .as_ref()
        .is_some_and(|r| r.tier_search.is_some());
    let samples = coastal_profile.map_or(160, |p| p.candidate_samples);
    let mut proposals = std::collections::HashMap::<u64, usize>::new();
    let coastal_proposals = if let Some(policy) = project
        .monument_rules
        .as_ref()
        .and_then(|r| r.coastal_search.as_ref())
    {
        let Some(prepared) = crate::coastal_candidates::prepare(
            project,
            layers,
            catalog,
            source_rules
                .as_ref()
                .ok_or("Missing source coastal candidates")?,
            policy,
            Some((bindings, &terrain)),
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        Some(prepared)
    } else {
        None
    };
    crate::composition::record_timing(&mut timing, "coastal_proposals");
    let coastal_count = (coastal_proposals.as_ref().map_or(0, |p| p.targets.len())
        + project
            .targets
            .iter()
            .filter(|t| t.exact && project.monument_presence(t) != Presence::Excluded)
            .filter(|t| {
                if t.prefab_path.as_deref().is_none_or(|p| {
                    coastal_profile.is_none_or(|profile| profile.filter(p).is_none())
                        || catalog.entries.get(p).is_none_or(|asset| {
                            asset
                                .bounds
                                .as_ref()
                                .is_none_or(|b| b.minimum_world_size > project.size)
                                || bounds_for_recipe(project, asset).is_err()
                        })
                }) {
                    return false;
                }
                t.prefab_path
                    .as_ref()
                    .and_then(|p| source_rules.as_ref()?.prefabs.get(p))
                    .and_then(|p| p.placement_attributes.as_ref())
                    .is_some_and(crate::coastal_placement::swims)
            })
            .count())
    .max(1);
    let mut target_navigation_samples = std::collections::HashMap::<u64, usize>::new();
    let mut projected_navigation_samples = 0_usize;
    let spacing_source = if project
        .monument_rules
        .as_ref()
        .and_then(|r| r.stage_rules.as_ref())
        .is_some_and(|s| s.spacing.is_some() || s.population.is_some())
    {
        Some(
            source_rules
                .as_ref()
                .and_then(|s| s.stage_source.as_ref())
                .ok_or("Missing bound monument spacing source")?,
        )
    } else {
        None
    };
    let mut result = solve_adapted(
        project,
        &layers.height,
        layers.height_resolution,
        Some(catalog),
        coastal_proposals.as_ref().map(|p| &p.targets),
        spacing_source,
        tunnels.as_deref(),
        frozen,
        &cancelled,
        |raw, target, x, z, yaw| {
            let proposal = proposals.entry(target.id).or_default();
            let trial = *proposal;
            *proposal += 1;
            let check = (|| {
                use placement::CandidateCheck;
                let recipe_target = project
                    .targets
                    .iter()
                    .find(|t| t.instance_id == target.id)
                    .unwrap();
                let path = recipe_target.prefab_path.as_deref().unwrap_or("");
                let ready_stamp = |prepared: &mut HashMap<String, crate::composition::Prepared>,
                                   unavailable: &mut HashMap<String, String>| {
                    if prepared.contains_key(path) {
                        return None;
                    }
                    let asset = catalog.entries.get(path);
                    if asset.is_some_and(omit_stamp) {
                        return None;
                    }
                    if balanced
                        && !unavailable.contains_key(path)
                        && let Some(asset) = asset
                    {
                        // Drop the previous monument before decoding this one's textures.
                        prepared.clear();
                        match crate::composition::prepare_with_budget(
                            asset,
                            bindings,
                            project.size,
                            &cancelled,
                            256 * 1024 * 1024,
                        ) {
                            Ok(Some(item)) => {
                                prepared.insert(path.to_owned(), item);
                            }
                            Ok(None) => return Some(CandidateCheck::Cancelled),
                            Err(error) => {
                                unavailable.insert(path.to_owned(), error);
                            }
                        }
                    }
                    (!prepared.contains_key(path)).then(|| {
                        CandidateCheck::Rejected(
                            unavailable
                                .get(path)
                                .cloned()
                                .unwrap_or_else(|| "Missing authored height modifier".into()),
                        )
                    })
                };
                // Authored footprints can reject a candidate without sampling its
                // stamp. Legacy circular bounds still need stamp clearance up front.
                let defer_stamp =
                    balanced && target.bounds.is_some() && !unavailable.contains_key(path);
                if !defer_stamp
                    && let Some(check) = ready_stamp(&mut prepared, &mut unavailable)
                {
                    return Ok(check);
                }
                if let Some(stages) = source_rules.as_ref().and_then(|s| s.stage_source.as_ref()) {
                    match stages.evaluate(
                        layers,
                        path,
                        project.seeds[4].value,
                        target.id,
                        [x, z],
                    )? {
                        papyrus_terrain::monument_stage::Check::Accepted { .. } => {}
                        papyrus_terrain::monument_stage::Check::FilterRejected { .. } => {
                            return Ok(CandidateCheck::Rejected(
                                "Authored monument stage filter failed".into(),
                            ));
                        }
                        papyrus_terrain::monument_stage::Check::Skipped(reason) => {
                            return Ok(CandidateCheck::Rejected(reason.into()));
                        }
                    }
                }
                let mut x = x;
                let mut z = z;
                let angle = |yaw| {
                    project
                        .monument_rules
                        .as_ref()
                        .map_or(yaw, |r| r.placement_yaw(yaw))
                };
                let mut yaw = angle(yaw);
                let mut y = raw.height_at(x, z);
                let mut relaxation = 0_u8;
                let prefab = source_rules
                    .as_ref()
                    .and_then(|source| source.prefabs.get(path));
                let aligned = prefab.and_then(|prefab| {
                    crate::monument_alignment::program(project.monument_rules.as_ref(), prefab)
                });
                let coastal = prefab
                    .and_then(|p| p.placement_attributes.as_ref())
                    .filter(|a| crate::coastal_placement::swims(a))
                    .zip(coastal_profile);
                let allows_submerged = coastal.is_some()
                    || source_rules
                        .as_ref()
                        .and_then(|source| source.stage_source.as_ref())
                        .is_some_and(|stages| stages.requires_ocean(path));
                if let Some((attributes, profile)) = coastal {
                    if profile.filter(path).is_none() {
                        return Ok(CandidateCheck::Rejected(
                            "Coastal prefab requires a separate placement family".into(),
                        ));
                    }
                    let stage_filter =
                        profile.filter(path).ok_or("Missing coastal stage filter")?;
                    if papyrus_terrain::spawn::sample_factor(
                        layers,
                        stage_filter,
                        x / layers.size as f32 + 0.5,
                        z / layers.size as f32 + 0.5,
                    )
                    .unwrap_or(0.)
                        <= 0.
                    {
                        return Ok(CandidateCheck::Rejected(
                            "Authored coastal stage filter failed".into(),
                        ));
                    }
                    let pose = if project.monument_rules.as_ref().is_some_and(|r| {
                        r.coastal_normals.as_deref() == Some(papyrus_terrain::terrain_normal::VERSION)
                    }) {
                        crate::coastal_placement::pose_source_normal
                    } else {
                        crate::coastal_placement::pose
                    };
                    match pose(layers, attributes, [x, y, z], yaw, profile.sea_level) {
                        Ok((position, rotation)) => {
                            [x, y, z] = position;
                            yaw = angle(rotation);
                        }
                        Err(error) => return Ok(CandidateCheck::Rejected(error)),
                    }
                }
                if aligned.is_some() {
                    yaw = angle(crate::monument_alignment::yaw(layers, x, z)?);
                }
                let extent = [target.entry.extent_x, target.entry.extent_z];
                let clearance = |prepared: &HashMap<String, crate::composition::Prepared>| {
                    prepared
                        .get(path)
                        .map_or_else(
                            || extent[0].hypot(extent[1]),
                            |item| item.placement_clearance(extent),
                        )
                        .max(1.)
                };
                if if let Some(bounds) = &target.bounds {
                    bounds
                        .iter()
                        .any(|b| !b.at([x, z], yaw).inside_world(project.size as f32))
                } else {
                    let clearance = clearance(&prepared);
                    x.abs() + clearance > project.size as f32 / 2.
                        || z.abs() + clearance > project.size as f32 / 2.
                } {
                    return Ok(CandidateCheck::Rejected(
                        "Authored footprint reaches outside world bounds".into(),
                    ));
                }
                if !allows_submerged && y <= 4. {
                    return Ok(CandidateCheck::Rejected(
                        "Monument center is low or submerged".into(),
                    ));
                }
                if let Some(source) = &source_rules {
                    let Some(prefab) = prefab else {
                        return Ok(CandidateCheck::Rejected(
                            "Missing source monument eligibility data".into(),
                        ));
                    };
                    let bounds = target
                        .bounds
                        .as_ref()
                        .ok_or("Source checks require authored bounds")?[0];
                    use papyrus_terrain::monument_eligibility::{self, Check};
                    let mut adapted_rules;
                    let relax = project.options.relax_monument_rules.unwrap_or_default()
                        && crate::recipe_options::Landmark::for_path(path).is_none();
                    let rules = if coastal.is_some() || aligned.is_some() {
                        adapted_rules = prefab.rules.clone();
                        adapted_rules
                            .unsupported
                            .retain(|s| s != crate::monument_alignment::REQUIREMENT);
                        if let Some((_, profile)) = coastal {
                            adapted_rules.min_corners = profile.minimum_tier_corners;
                        }
                        &adapted_rules
                    } else if relax {
                        adapted_rules = prefab.rules.clone();
                        adapted_rules.min_corners = 1;
                        &adapted_rules
                    } else if retry_tiers {
                        adapted_rules = prefab.rules.clone();
                        relaxation = ((trial * prefab.rules.min_corners / samples)
                            .min(prefab.rules.min_corners - 1))
                            as u8;
                        adapted_rules.min_corners -= usize::from(relaxation);
                        &adapted_rules
                    } else {
                        &prefab.rules
                    };
                    let Some(mut evaluator) =
                        monument_eligibility::Evaluator::new(layers, rules, bounds, &cancelled)?
                    else {
                        return Ok(CandidateCheck::Cancelled);
                    };
                    let filter = source
                        .stage_source
                        .as_ref()
                        .and_then(|s| s.anchor_filter(path));
                    if let Some(filter) = filter {
                        evaluator = evaluator.with_stage_filter(filter)?;
                    }
                    match evaluator.evaluate(
                        [x, y, z],
                        yaw,
                        eligibility_work.entry(target.id).or_default(),
                        eligibility_allowance / eligible as usize,
                        &cancelled,
                    )? {
                        None => return Ok(CandidateCheck::Cancelled),
                        Some(Check::Budget) => {
                            return Ok(CandidateCheck::TargetExhausted(
                                "Monument eligibility sampling allowance reached".into(),
                            ));
                        }
                        Some(Check::Rejected(reason)) => {
                            return Ok(CandidateCheck::Rejected(reason.into()));
                        }
                        Some(Check::Accepted(position)) => y = position[1],
                    }
                    if let Some(attributes) = aligned {
                        let filter = filter.ok_or("Aligned monument requires an authored stage filter")?;
                        match crate::monument_alignment::check_terrain(
                            layers,
                            attributes,
                            filter,
                            [x, y, z],
                            yaw,
                            eligibility_work.entry(target.id).or_default(),
                            eligibility_allowance / eligible as usize,
                            &cancelled,
                        )? {
                            None => return Ok(CandidateCheck::Cancelled),
                            Some(Check::Budget) => return Ok(CandidateCheck::TargetExhausted(
                                "Monument eligibility sampling allowance reached".into(),
                            )),
                            Some(Check::Rejected(reason)) => {
                                return Ok(CandidateCheck::Rejected(reason.into()));
                            }
                            Some(Check::Accepted(_)) => {}
                        }
                    }
                }
                if let Some(tunnels) = &tunnels {
                    let Some(position) =
                        tunnels.snap_entrance(path, [x, y, z], yaw, project.size as f32)?
                    else {
                        return Ok(CandidateCheck::Rejected(
                            "Tunnel entrance overlaps its station-grid exclusion".into(),
                        ));
                    };
                    // Replacement trials pin raw proposals internally. Explicit
                    // user pins retain their requested horizontal position.
                    if target.exact
                        && frozen.is_none_or(|f| f.selected.is_none())
                        && (position[0] - x).hypot(position[2] - z) > 0.001
                    {
                        return Ok(CandidateCheck::Rejected(
                            "Exact monument position conflicts with tunnel entrance snapping".into(),
                        ));
                    }
                    [x, y, z] = position;
                    if let Some(bounds) = &target.bounds
                        && bounds
                            .iter()
                            .any(|b| !b.at([x, z], yaw).inside_world(project.size as f32))
                    {
                        return Ok(CandidateCheck::Rejected(
                            "Snapped monument footprint reaches outside world bounds".into(),
                        ));
                    }
                }
                if let Some((attributes, profile)) = coastal
                    && let Err(error) = crate::coastal_placement::checks(
                        layers,
                        attributes,
                        [x, y, z],
                        yaw,
                        profile,
                        profile.filter(path).ok_or("Missing coastal stage filter")?,
                        navigation.as_ref().ok_or("Missing boat navigation")?,
                    )
                {
                    return Ok(CandidateCheck::Rejected(error));
                }
                if coastal.is_none()
                    && let Some(access) = &land_access
                    && let Some(attributes) = source_rules.as_ref()
                        .and_then(|source| source.prefabs.get(path))
                        .and_then(|prefab| prefab.placement_attributes.as_ref())
                    && road_entrances.as_ref().is_some_and(|roads| roads.links_land(attributes))
                    && !access.admits([x, z])
                {
                    return Ok(CandidateCheck::Rejected("Road-linked monument is outside the main land network".into()));
                }
                if let Some((attributes, _)) = coastal
                    && let Some(entrances) = &road_entrances
                {
                    use papyrus_terrain::monument_eligibility::Check;
                    if let Some(access) = &land_access {
                        match entrances.check_land_access(
                            access,
                            attributes,
                            [x, y, z],
                            yaw,
                            eligibility_work.entry(target.id).or_default(),
                            eligibility_allowance / eligible as usize,
                        ) {
                            Check::Budget => {
                                return Ok(CandidateCheck::TargetExhausted(
                                    "Coastal land-access sampling allowance reached".into(),
                                ));
                            }
                            Check::Rejected(reason) => {
                                return Ok(CandidateCheck::Rejected(reason.into()));
                            }
                            Check::Accepted(_) => {}
                        }
                    }
                    let asset = catalog
                        .entries
                        .get(path)
                        .ok_or("Missing coastal road entrance prefab")?;
                    match entrances.check(
                        layers,
                        attributes,
                        asset.modifier.as_ref().is_some_and(|m| m.height_map),
                        [x, y, z],
                        yaw,
                        eligibility_work.entry(target.id).or_default(),
                        eligibility_allowance / eligible as usize,
                        &cancelled,
                    )? {
                        None => return Ok(CandidateCheck::Cancelled),
                        Some(Check::Budget) => {
                            return Ok(CandidateCheck::TargetExhausted(
                                "Coastal road entrance sampling allowance reached".into(),
                            ));
                        }
                        Some(Check::Rejected(reason)) => {
                            return Ok(CandidateCheck::Rejected(reason.into()));
                        }
                        Some(Check::Accepted(_)) => {}
                    }
                }
                let maximum_change = coastal.map_or_else(
                    || {
                        if project
                            .monument_rules
                            .as_ref()
                            .is_some_and(|r| r.earthwork.is_some())
                        {
                            let rules = &source_rules.as_ref().unwrap().prefabs[path].rules;
                            authored_earthwork_limit(
                                rules,
                                coastal_profile.unwrap().maximum_cut_fill,
                            )
                        } else {
                            24.
                        }
                    },
                    |(_, p)| p.maximum_cut_fill as f64,
                );
                if defer_stamp
                    && let Some(check) = ready_stamp(&mut prepared, &mut unavailable)
                {
                    return Ok(check);
                }
                let stamp = prepared.get(path).map(|item| {
                    item.stamp_for_recipe(
                        [x as f64, y as f64, z as f64],
                        yaw as f64,
                        project.composition_engine.as_deref(),
                    )
                });
                if stamp.as_ref().is_some_and(|stamp| stamp.height.is_none())
                    && !project
                        .monument_rules
                        .as_ref()
                        .is_some_and(|rules| rules.surface_modifiers.is_some())
                {
                    return Ok(CandidateCheck::Rejected(
                        "Earthwork placement requires an authored height modifier".into(),
                    ));
                }
                if let Some(stamp) = stamp.as_ref().filter(|s| s.height.is_some())
                    && let Some((attributes, _)) = coastal
                    && let Some(entrances) = &road_entrances
                {
                    use papyrus_terrain::monument_eligibility::Check;
                    let projection = terrain.projected(stamp)?;
                    match entrances.check_projected(
                        layers,
                        attributes,
                        &projection,
                        [x, y, z],
                        yaw,
                        eligibility_work.entry(target.id).or_default(),
                        eligibility_allowance / eligible as usize,
                        &cancelled,
                    )? {
                        None => return Ok(CandidateCheck::Cancelled),
                        Some(Check::Budget) => {
                            return Ok(CandidateCheck::TargetExhausted(
                                "Coastal projected road entrance sampling allowance reached".into(),
                            ));
                        }
                        Some(Check::Rejected(reason)) => {
                            return Ok(CandidateCheck::Rejected(reason.into()));
                        }
                        Some(Check::Accepted(_)) => {}
                    }
                }
                let assessment = if let Some(stamp) = stamp.as_ref() {
                    if balanced {
                        let spent = target_work.entry(target.id).or_default();
                        let allowance = sized_allowances
                            .as_ref()
                            .and_then(|a| a.get(&target.id).copied())
                            .unwrap_or(allowance);
                        let assessed = terrain.check_placement(
                            stamp, spent, maximum_change, allowance, allows_submerged, &cancelled,
                        );
                        match assessed {
                            Err(error) if *spent > allowance => {
                                return Ok(CandidateCheck::TargetExhausted(error));
                            }
                            result => result,
                        }
                    } else {
                        terrain.check_placement(
                            stamp, &mut work, maximum_change,
                            papyrus_terrain::monument_composition::MAX_EARTHWORK_SAMPLES,
                            false, &cancelled,
                        )
                    }
                } else {
                    Ok(Some(
                        papyrus_terrain::monument_composition::PlacementAssessment::default(),
                    ))
                };
                let assessment = match assessment {
                    Ok(Some(report)) => report,
                    Ok(None) => return Ok(CandidateCheck::Cancelled),
                    Err(error)
                        if work > papyrus_terrain::monument_composition::MAX_EARTHWORK_SAMPLES =>
                    {
                        return Ok(CandidateCheck::Exhausted(error));
                    }
                    Err(error) => return Ok(CandidateCheck::Rejected(error)),
                };
                if (stamp.as_ref().is_some_and(|stamp| stamp.height.is_some())
                    && assessment.samples == 0)
                    || assessment.clamped_samples > 0
                    || (!allows_submerged && assessment.submerged_samples > 0)
                {
                    return Ok(CandidateCheck::Rejected(
                        "Authored height stamp is empty, clamped or intersects water".into(),
                    ));
                }
                if assessment.maximum_change > maximum_change {
                    return Ok(CandidateCheck::Rejected(format!(
                        "Authored stamp exceeds the {maximum_change} m cut/fill limit"
                    )));
                }
                if let Some(stamp) = stamp.as_ref()
                    && let Some((attributes, profile)) = coastal
                    && attributes
                        .path_connectors
                        .iter()
                        .any(|c| c.infrastructure_type == profile.boat_type)
                {
                    let n = navigation
                        .as_ref()
                        .ok_or("Missing navigation grid")?
                        .resolution();
                    let cost = n * n * 4;
                    let used = if coastal_proposals.is_some() {
                        target_navigation_samples.entry(target.id).or_default()
                    } else {
                        &mut projected_navigation_samples
                    };
                    let limit = if coastal_proposals.is_some() {
                        64_000_000 / coastal_count
                    } else {
                        64_000_000
                    };
                    if *used + cost > limit {
                        return Ok(CandidateCheck::TargetExhausted(
                            "Coastal projected navigation work budget reached".into(),
                        ));
                    }
                    *used += cost;
                    let projection = terrain.projected(stamp)?;
                    let navigation = if source_navigation {
                        papyrus_terrain::boat_navigation::Navigation::build_source_sampled(
                            layers.size,
                            profile.boat_cell_size,
                            profile.minimum_boat_depth,
                            |p| projection.sample_source(p, -500., 1000.).ok(),
                            |_, _| Some(profile.sea_level),
                            &cancelled,
                        )
                    } else {
                        papyrus_terrain::boat_navigation::Navigation::build_sampled(
                            layers.size,
                            profile.boat_cell_size,
                            profile.minimum_boat_depth,
                            |u, v| projection.sample(u, v),
                            |_, _| Some(profile.sea_level),
                            &cancelled,
                        )
                    };
                    let Some(nav) = navigation? else {
                        return Ok(CandidateCheck::Cancelled);
                    };
                    if let Err(error) = crate::coastal_placement::boat_access(
                        attributes,
                        [x, y, z],
                        yaw,
                        profile,
                        &nav,
                    ) {
                        return Ok(CandidateCheck::Rejected(error));
                    }
                }
                let position = placement::Position {
                    x,
                    y,
                    z,
                    yaw: angle(yaw.rem_euclid(360.)),
                    clearance: clearance(&prepared),
                };
                let grade = (assessment.maximum_change / maximum_change * 0.12) as f32;
                Ok(if relaxation == 0 {
                    CandidateCheck::Accepted(position, grade)
                } else {
                    CandidateCheck::AcceptedRelaxed(position, grade, relaxation)
                })
            })();
            if let Ok(ref assessment) = check {
                if let Some(captured) = captured.as_deref_mut()
                    && let placement::CandidateCheck::Accepted(position, _)
                    | placement::CandidateCheck::AcceptedRelaxed(position, _, _) = assessment
                {
                    if captured.len() >= placement::MAX_TARGETS * 32 {
                        return Err("Native proposal capture exceeds bounded accepted pools".into());
                    }
                    captured.push((target.id, position.clone(), RawProposal { x, z, yaw }));
                }
                observe(target.id, assessment);
            }
            check
        },
    )?;
    crate::composition::record_timing(&mut timing, "placement_search");
    if let Some(result) = &mut result {
        for outcome in &mut result.outcomes {
            if outcome.position.is_some()
                && frozen.is_none_or(|f| !f.retains(outcome.id))
            {
                outcome.reason.push_str(
                    if crate::coastal_placement::supported(&project.placement_engine) {
                        "; full-resolution earthwork and applicable ocean-coastal checks passed"
                    } else {
                        "; full-resolution authored earthwork checks passed (24 m limit)"
                    },
                );
            }
        }
    }
    Ok(result)
}
pub fn solve(
    project: &Project,
    heights: &[i16],
    resolution: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Solution>, String> {
    if is_earthwork(&project.placement_engine) {
        return Err(
            "Earthwork placement requires full native terrain and verified monument assets".into(),
        );
    }
    solve_adapted(
        project,
        heights,
        resolution,
        None,
        None,
        None,
        None,
        None,
        cancelled,
        |terrain, target, x, z, yaw| {
            // Legacy candidate generation remains byte-for-byte versioned in the core.
            terrain.check_candidate(target, x, z, yaw)
        },
    )
}

/// Modifier-free prefabs retain their authored object bounds, including point
/// bounds. No synthetic terrain footprint is introduced.
pub fn bounds_for_recipe(
    project: &Project,
    asset: &crate::prefab_assets::Asset,
) -> Result<([papyrus_terrain::monument_bounds::Footprint; 2], i32), String> {
    let fallback = if asset.priority.is_none() {
        crate::prefab_priority::fallback(project)?
    } else {
        None
    };
    if project
            .monument_rules
            .as_ref()
            .is_some_and(|r| r.omits_terrain_stamp(asset))
    {
        let bounds = asset
            .bounds
            .as_ref()
            .ok_or("Missing authored monument bounds")?;
        let footprint = papyrus_terrain::monument_bounds::Footprint {
            center: [bounds.center[0], bounds.center[2]],
            extent: [bounds.extent[0], bounds.extent[2]],
        };
        if !footprint.valid() || bounds.minimum_world_size > project.size {
            return Err("Invalid or oversized source placement bounds".into());
        }
        return Ok((
            [footprint; 2],
            asset.priority.or(fallback).ok_or("Missing source prefab priority")?,
        ));
    }
    source_bounds_with_priority(asset, fallback)
}
pub fn source_bounds(
    asset: &crate::prefab_assets::Asset,
) -> Result<([papyrus_terrain::monument_bounds::Footprint; 2], i32), String> {
    source_bounds_with_priority(asset, None)
}
fn source_bounds_with_priority(
    asset: &crate::prefab_assets::Asset,
    fallback: Option<i32>,
) -> Result<([papyrus_terrain::monument_bounds::Footprint; 2], i32), String> {
    use papyrus_terrain::monument_bounds::Footprint;
    let b = asset
        .bounds
        .as_ref()
        .ok_or("Missing authored monument bounds")?;
    let size = asset
        .modifier_size
        .ok_or("Missing authored modifier size")?;
    let offset = asset
        .modifier_offset
        .ok_or("Missing authored modifier offset")?;
    let modifier = asset.modifier.as_ref().ok_or("Missing authored modifier")?;
    let priority = asset.priority.or(fallback).ok_or("Missing source prefab priority")?;
    let radius = if modifier.radius == 0. {
        size[0] / 2.
    } else {
        modifier.radius
    };
    let half = if asset.texture_guids.contains_key("blend") {
        [size[0] / 2., size[2] / 2.]
    } else {
        [radius; 2]
    };
    let bounds = [
        Footprint {
            center: [b.center[0], b.center[2]],
            extent: [b.extent[0], b.extent[2]],
        },
        Footprint {
            center: [offset[0], offset[2]],
            extent: [half[0].max(radius), half[1].max(radius)],
        },
    ];
    if bounds.iter().any(|b| !b.valid()) {
        return Err("Invalid source placement bounds".into());
    }
    Ok((bounds, priority))
}

#[allow(clippy::too_many_arguments)] // Optional prepared hints retain all legacy entry points.
fn solve_adapted(
    project: &Project,
    heights: &[i16],
    resolution: usize,
    authored: Option<&crate::prefab_assets::Catalog>,
    proposals: Option<&std::collections::BTreeMap<u64, placement::ProposalSet>>,
    spacing_source: Option<&crate::monument_stages::Source>,
    tunnels: Option<&crate::tunnel_source::Source>,
    frozen: Option<Frozen<'_>>,
    cancelled: impl Fn() -> bool,
    evaluate: impl FnMut(
        &Terrain<'_>,
        &Target,
        f32,
        f32,
        f32,
    ) -> Result<placement::CandidateCheck, String>,
) -> Result<Option<Solution>, String> {
    project.validate()?;
    let catalog = if is_earthwork(&project.placement_engine)
        && project.placement_engine != LEGACY_EARTHWORK_VERSION
    {
        authored.map(installation_templates).unwrap_or_default()
    } else {
        templates()
    };
    let mut targets = Vec::new();
    let mut missing = Vec::new();
    let mut missing_required = 0;
    for t in &project.targets {
        if project.monument_presence(t) == Presence::Excluded {
            missing.push(Outcome {
                id: t.instance_id,
                position: None,
                distance: None,
                within_radius: false,
                reason: project
                    .monument_blocker(t.prefab_path.as_deref())
                    .unwrap_or("Excluded by presence policy")
                    .into(),
            });
            continue;
        }
        let found = catalog.iter().find(|e| {
            t.prefab_path
                .as_ref()
                .map_or(e.name == t.name, |p| p == &e.path)
        });
        if let Some(template) = found {
            let mut entry = template.entry.clone();
            if let Some(asset) = authored.and_then(|c| c.entries.get(&template.path)) {
                if let Some(bounds) = &asset.bounds {
                    [entry.extent_x, entry.extent_z] = bounds.horizontal_extent();
                    entry.minimum_size = bounds.minimum_world_size as i32;
                }
                if let Some([min, max]) = asset.yaw {
                    // Authored Unity yaw rotates in the opposite direction to the studio.
                    entry.minimum_yaw = -max;
                    entry.maximum_yaw = -min;
                }
            }
            if let Some(yaw) = t.yaw {
                entry.minimum_yaw = yaw;
                entry.maximum_yaw = yaw;
            }
            let (bounds, priority) = if has_source_bounds(&project.placement_engine) {
                let rules = authored
                    .and_then(|c| c.entries.get(&template.path))
                    .ok_or_else(|| "Verified prefab missing".to_string())
                    .and_then(|asset| bounds_for_recipe(project, asset));
                match rules {
                    Ok((bounds, priority)) => (Some(bounds), priority),
                    Err(error) => {
                        missing.push(Outcome {
                            id: t.instance_id,
                            position: None,
                            distance: None,
                            within_radius: false,
                            reason: format!("Unsupported source placement rules: {error}"),
                        });
                        missing_required += usize::from(project.requires_monument(t));
                        continue;
                    }
                }
            } else {
                (None, 0)
            };
            let mut spacing = spacing_source
                .filter(|_| {
                    project
                        .monument_rules
                        .as_ref()
                        .and_then(|r| r.stage_rules.as_ref())
                        .is_some_and(|b| b.spacing.is_some())
                })
                .map(|s| {
                    let shape = authored
                        .and_then(|c| c.entries.get(&template.path))
                        .and_then(|a| a.bounds.as_ref())
                        .ok_or("Missing authored spacing bounds")?;
                    s.spacing(&template.path, shape)
                })
                .transpose()?;
            if let Some(entrance) =
                tunnels.and_then(|source| source.entrance_spacing(&template.path))
            {
                if spacing.is_none() {
                    let shape = authored
                        .and_then(|c| c.entries.get(&template.path))
                        .and_then(|a| a.bounds.as_ref())
                        .ok_or("Missing tunnel-owner bounds")?;
                    spacing = Some(placement::Spacing {
                        shape: papyrus_terrain::monument_spacing::BoxShape {
                            center: shape.center,
                            extent: shape.extent,
                        },
                        family: template.path.clone(),
                        minimum_same: 0.,
                        minimum_different: 0.,
                        entrance: None,
                    });
                }
                spacing.as_mut().unwrap().entrance = Some(entrance);
            }
            let center = !t.exact && t.max_displacement_m.is_none() && project.options.prefers_center(t.prefab_path.as_deref());
            targets.push(Target {
                spacing,
                bounds,
                priority,
                id: t.instance_id,
                entry,
                x: if center { 0. } else { t.x },
                z: if center { 0. } else { t.z },
                radius: if center { project.size as f32 * 0.12 } else { t.radius },
                max_displacement_m: t.max_displacement_m,
                weight: if center { 10. } else { t.strength },
                exact: t.exact,
                required: project.requires_monument(t),
                excluded: false,
            });
        } else {
            missing.push(Outcome {
                id: t.instance_id,
                position: None,
                distance: None,
                within_radius: false,
                reason: "No supported catalog entry for this monument; select a template".into(),
            });
            missing_required += usize::from(project.requires_monument(t));
        }
    }
    let samples = project
        .monument_rules
        .as_ref()
        .and_then(|r| r.coastal_profile.as_ref())
        .filter(|_| crate::coastal_placement::supported(&project.placement_engine))
        .map_or(160, |p| p.candidate_samples);
    let limits = if project
        .monument_rules
        .as_ref()
        .and_then(|r| r.stage_rules.as_ref())
        .is_some_and(|b| b.population.is_some())
    {
        let source = spacing_source.ok_or("Missing bound installation population source")?;
        let population =
            source.population(authored.ok_or("Missing population catalog")?, project.size)?;
        let paths: std::collections::BTreeMap<_, _> = population
            .into_iter()
            .map(|(p, group, maximum)| (p, (group, maximum)))
            .collect();
        Some(
            project
                .targets
                .iter()
                .filter(|t| targets.iter().any(|active| active.id == t.instance_id))
                .filter_map(|t| {
                    let (group, maximum) = paths.get(t.prefab_path.as_ref()?)?;
                    Some((
                        t.instance_id,
                        placement::SelectionLimit {
                            group: *group,
                            maximum: (*maximum)?,
                        },
                    ))
                })
                .collect::<std::collections::BTreeMap<_, _>>(),
        )
    } else {
        None
    };
    let mut reserved_targets = Vec::new();
    let mut retained = Vec::new();
    if let Some(frozen) = frozen {
        for outcome in frozen
            .solution
            .outcomes
            .iter()
            .filter(|o| frozen.retains(o.id))
        {
            if let Some(position) = &outcome.position {
                let target = targets
                    .iter()
                    .find(|t| t.id == outcome.id)
                    .ok_or("Earlier placement is missing its authored reservation geometry")?;
                reserved_targets.push((target.clone(), position));
            }
            retained.push(outcome.clone());
        }
        targets.retain(|t| !frozen.retains(t.id));
        missing.retain(|o| !frozen.retains(o.id));
    }
    let reservations = reserved_targets
        .iter()
        .map(|(target, position)| placement::Reservation { target, position })
        .collect::<Vec<_>>();
    let limits = limits.map(|mut limits| {
        limits.retain(|id, _| {
            targets.iter().any(|t| t.id == *id) || reservations.iter().any(|r| r.target.id == *id)
        });
        limits
    });
    // Keep proposal streams for the active phase. Earlier outcomes reserve
    // geometry and quotas without entering candidate assessment again.
    let active_proposals = frozen.and(proposals).map(|proposals| {
        proposals
            .iter()
            .filter(|(id, _)| targets.iter().any(|t| t.id == **id))
            .map(|(id, set)| {
                (
                    *id,
                    placement::ProposalSet {
                        points: set.points.clone(),
                        maximum_accepted: set.maximum_accepted,
                    },
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    });
    let proposals = if frozen.is_some() {
        active_proposals.as_ref()
    } else {
        proposals
    };
    let Some(mut result) = placement::solve_with_reservations(
        Terrain {
            heights,
            resolution,
            size: project.size as usize,
        },
        &targets,
        project.seeds[4].value,
        authored.is_some(),
        is_balanced(&project.placement_engine),
        samples,
        project
            .monument_rules
            .as_ref()
            .is_some_and(|r| r.tier_search.is_some()),
        proposals,
        limits.as_ref(),
        &reservations,
        cancelled,
        evaluate,
    )?
    else {
        return Ok(None);
    };
    result.outcomes.extend(missing);
    result.outcomes.extend(retained);
    result.outcomes.sort_by_key(|o| o.id);
    result.required_missing += missing_required;
    if frozen.is_some() {
        result.required_missing = project
            .targets
            .iter()
            .filter(|t| {
                project.requires_monument(t)
                    && result
                        .outcomes
                        .iter()
                        .any(|o| o.id == t.instance_id && o.position.is_none())
            })
            .count();
    }
    Ok(Some(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replacement_search_keeps_prior_outcomes_and_only_assesses_the_moved_target() {
        let mut project = Project::default();
        for target in &mut project.targets {
            target.exact = true;
            target.presence = Presence::Required;
        }
        let selected = project.targets[0].instance_id;
        let pose = placement::Position {
            x: 0.,
            y: 20.,
            z: 0.,
            yaw: 15.,
            clearance: 200.,
        };
        let canonical = Solution {
            outcomes: project
                .targets
                .iter()
                .enumerate()
                .map(|(i, t)| Outcome {
                    id: t.instance_id,
                    position: (i != 2).then(|| pose.clone()),
                    distance: (i != 2).then_some(42.),
                    within_radius: i != 2,
                    reason: format!("Retained source outcome {i}"),
                })
                .collect(),
            required_missing: 1,
            attempts: 8,
        };
        let heights = vec![18000; 65 * 65];
        for x in [0., 1000.] {
            let mut assessments = 0;
            let result = solve_adapted(
                &project,
                &heights,
                65,
                None,
                None,
                None,
                None,
                Some(Frozen { solution: &canonical, selected: Some(selected) }),
                || false,
                |_, target, _, _, _| {
                    assert_eq!(target.id, selected);
                    assessments += 1;
                    Ok(placement::CandidateCheck::Accepted(
                        placement::Position { x, ..pose.clone() },
                        0.,
                    ))
                },
            )
            .unwrap()
            .unwrap();
            assert_eq!(assessments, 1);
            assert_eq!(result.outcomes.len(), canonical.outcomes.len());
            for prior in canonical.outcomes.iter().filter(|o| o.id != selected) {
                assert_eq!(
                    result.outcomes.iter().find(|o| o.id == prior.id).unwrap(),
                    prior
                );
            }
            assert_eq!(result.required_missing, if x == 0. { 2 } else { 1 });
            assert_eq!(
                result
                    .outcomes
                    .iter()
                    .find(|o| o.id == selected)
                    .unwrap()
                    .position
                    .is_some(),
                x != 0.
            );
        }
    }

    #[test]
    fn later_phase_keeps_prior_outcomes_and_samples_only_active_proposals() {
        let project = Project::default();
        let selected = project.targets[0].instance_id;
        let pose = placement::Position {
            x: 0.,
            y: 20.,
            z: 0.,
            yaw: 15.,
            clearance: 200.,
        };
        let prior = Solution {
            outcomes: project
                .targets
                .iter()
                .skip(1)
                .enumerate()
                .map(|(index, t)| Outcome {
                    id: t.instance_id,
                    position: (index == 0).then(|| pose.clone()),
                    distance: None,
                    within_radius: false,
                    reason: format!("Earlier phase {index}"),
                })
                .collect(),
            required_missing: 0,
            attempts: 8,
        };
        let proposals = project
            .targets
            .iter()
            .map(|t| {
                (
                    t.instance_id,
                    placement::ProposalSet {
                        points: vec![[20., 10.]],
                        maximum_accepted: 8,
                    },
                )
            })
            .collect();
        let mut observed = Vec::new();
        let result = solve_adapted(
            &project,
            &vec![18000; 65 * 65],
            65,
            None,
            Some(&proposals),
            None,
            None,
            Some(Frozen {
                solution: &prior,
                selected: None,
            }),
            || false,
            |_, target, x, z, _| {
                assert_eq!(target.id, selected);
                observed.push([x, z]);
                Ok(placement::CandidateCheck::Accepted(
                    placement::Position {
                        x: 1000.,
                        ..pose.clone()
                    },
                    0.,
                ))
            },
        )
        .unwrap()
        .unwrap();
        assert!(observed.len() > 1);
        assert_eq!(observed[1], [20., 10.]);
        assert!(
            result
                .outcomes
                .iter()
                .find(|o| o.id == selected)
                .unwrap()
                .position
                .is_some()
        );
        for outcome in &prior.outcomes {
            assert_eq!(
                result.outcomes.iter().find(|o| o.id == outcome.id).unwrap(),
                outcome
            );
        }
    }

    #[test]
    fn canonical_proposals_follow_the_selected_exact_pose_after_packing() {
        let pose = placement::Position {
            x: 100.,
            y: 10.,
            z: 200.,
            yaw: 135.,
            clearance: 10.,
        };
        let outcome = Outcome {
            id: 1,
            position: Some(pose.clone()),
            distance: Some(0.),
            within_radius: true,
            reason: "Accepted".into(),
        };
        let solution = Solution {
            outcomes: vec![outcome],
            required_missing: 0,
            attempts: 8,
        };
        let wrong = RawProposal {
            x: 1.,
            z: 2.,
            yaw: 3.,
        };
        let raw = RawProposal {
            x: 20.,
            z: 30.,
            yaw: 45.,
        };
        let mut different = pose.clone();
        different.yaw = 136.;
        let captured = vec![
            (1, different, wrong),
            (2, pose.clone(), wrong),
            (1, pose.clone(), raw),
        ];
        assert_eq!(select_proposals(&solution, &captured).unwrap()[&1], raw);
        assert!(select_proposals(&solution, &captured[..2]).is_err());
        let mut changed = solution.clone();
        changed.outcomes[0].position.as_mut().unwrap().y += 0.01;
        assert!(select_proposals(&changed, &captured).is_err());
    }
    #[test]
    fn sized_allowances_cover_large_stamps_without_exceeding_stage_work() {
        let weights = [(9, 100), (1, 10_000), (4, 200)];
        let a = allocate_earthwork(weights.into_iter());
        assert!(a[&1] > 90 * a[&9]);
        assert!(a[&4] > a[&9]);
        assert!(
            a.values().sum::<u64>() + a.len() as u64
                <= papyrus_terrain::monument_composition::MAX_EARTHWORK_SAMPLES
        );
        assert_eq!(a, allocate_earthwork(weights.into_iter().rev()));
        assert!(allocate_earthwork(std::iter::empty()).is_empty());
        let extremes =
            allocate_earthwork((0..128).map(|id| (id, if id == 0 { 33_570_818 } else { 1 })));
        assert!(extremes.values().all(|&v| v > 0));
        assert!(
            extremes.values().sum::<u64>() + 128
                <= papyrus_terrain::monument_composition::MAX_EARTHWORK_SAMPLES
        );
    }

    #[test]
    fn performance_policy_disables_required_targets_without_losing_preferences() {
        let mut project = Project::default();
        project.targets.retain(|t| t.name == "Satellite Dish");
        project.targets[0].exact = true;
        let heights = vec![18000; 65 * 65];
        let original = project.clone();
        let baseline = solve(&project, &heights, 65, || false).unwrap().unwrap();
        assert!(baseline.outcomes[0].position.is_some());
        project.performance.monuments = false;
        let disabled = solve(&project, &heights, 65, || false).unwrap().unwrap();
        assert!(disabled.outcomes.iter().all(|o| o.position.is_none()));
        assert!(disabled.outcomes[0].reason.contains("Performance"));
        assert_eq!(disabled.required_missing, 0);
        assert_eq!(project.targets, original.targets);
        assert_eq!(project.terrain_key(), original.terrain_key());
        project.performance.monuments = true;
        assert_eq!(
            solve(&project, &heights, 65, || false).unwrap().unwrap(),
            baseline
        );
    }
    #[test]
    fn performance_policy_keeps_individual_exclusions() {
        let mut project = Project::default();
        project.targets[0].presence = Presence::Excluded;
        project.performance.monuments = false;
        let heights = vec![18000; 65 * 65];
        let result = solve(&project, &heights, 65, || false).unwrap().unwrap();
        assert_eq!(result.outcomes[0].reason, "Excluded by presence policy");
        project.performance.monuments = true;
        let result = solve(&project, &heights, 65, || false).unwrap().unwrap();
        assert!(result.outcomes[0].position.is_none());
        assert_eq!(result.outcomes[0].reason, "Excluded by presence policy");
    }
    #[test]
    fn explicit_orientation_is_versioned_and_reaches_solver() {
        let mut project = Project::default();
        project.targets.retain(|t| t.name == "Satellite Dish");
        project.targets[0].yaw = Some(37.);
        project.targets[0].exact = true;
        assert!(project.validate().is_err());
        project.placement_engine = ROTATION_VERSION.into();
        let result = solve(&project, &vec![18000; 65 * 65], 65, || false)
            .unwrap()
            .unwrap();
        assert_eq!(result.outcomes[0].position.as_ref().unwrap().yaw, 37.);
        project.targets[0].yaw = Some(f32::NAN);
        assert!(project.validate().is_err());
    }
    #[test]
    fn mapping_and_unsupported_entries_are_reported() {
        let mut p = Project::default();
        p.targets[1].prefab_path = None;
        p.targets[1].name = "Unknown custom monument".into();
        let h = vec![17000; 65 * 65];
        let result = solve(&p, &h, 65, || false).unwrap().unwrap();
        assert_eq!(result.outcomes.len(), 3);
        assert_eq!(result.required_missing, 1);
        assert!(result.outcomes[1].reason.contains("catalog"));
    }
    #[test]
    fn changing_layout_seed_preserves_terrain_recipe_and_targets() {
        let mut p = Project::default();
        let original = p.clone();
        let h = vec![17000; 65 * 65];
        let a = solve(&p, &h, 65, || false).unwrap().unwrap();
        p.shuffle_monuments();
        let b = solve(&p, &h, 65, || false).unwrap().unwrap();
        assert_eq!(p.targets, original.targets);
        assert_eq!(p.seeds[0], original.seeds[0]);
        assert_ne!(a, b);
        assert_eq!(h, vec![17000; 65 * 65]);
    }
}
