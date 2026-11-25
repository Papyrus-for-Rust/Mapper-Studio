//! Deterministic source-filtered proposals; never an acceptance authority.
use crate::{
    Presence, Project, coastal_placement, monument_source::Source, prefab_assets::Catalog,
};
use papyrus_terrain::{
    monument_eligibility::{self, Check},
    native_layers::Layers,
    placement::{ProposalSet, Terrain},
    spawn,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BinaryHeap, HashSet};

pub const LEGACY_VERSION: &str = "papyrus-coastal-filtered-proposals-1";
pub const VERSION: &str = "papyrus-coastal-filtered-proposals-2";
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub engine: String,
    pub resolution: usize,
    pub maximum_work: usize,
    pub maximum_sites: usize,
    pub maximum_accepted: usize,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            engine: VERSION.into(),
            resolution: 513,
            maximum_work: 32_000_000,
            maximum_sites: 1024,
            maximum_accepted: 8,
        }
    }
}
impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        if ![VERSION, LEGACY_VERSION].contains(&self.engine.as_str())
            || !(129..=1025).contains(&self.resolution)
            || !(1..=64_000_000).contains(&self.maximum_work)
            || !(1..=2048).contains(&self.maximum_sites)
            || !(1..=32).contains(&self.maximum_accepted)
        {
            return Err("Unsupported coastal proposal policy or work limits".into());
        }
        Ok(())
    }
}
#[derive(Serialize)]
pub struct Audit {
    pub id: u64,
    pub scanned: usize,
    pub eligible: usize,
    pub retained: usize,
    pub prefilter_rejected: usize,
    pub rejected: BTreeMap<&'static str, usize>,
    pub work: usize,
    pub budget_reached: bool,
}
pub struct Prepared {
    pub targets: BTreeMap<u64, ProposalSet>,
    pub audits: Vec<Audit>,
    pub preparation_work: usize,
}
// A necessary topology condition over every possible yaw. This index can
// reject absence of a required layer, while the exact source filters still run.
struct TopologyCoverage {
    size: f64,
    resolution: usize,
    prefixes: BTreeMap<u32, Vec<u32>>,
    work: usize,
}
impl TopologyCoverage {
    fn build(
        layers: &Layers,
        masks: &[u32],
        maximum_work: usize,
        cancelled: &impl Fn() -> bool,
    ) -> Option<Self> {
        let sr = layers.surface_resolution;
        let n = sr.min(256);
        let stride = n + 1;
        let mut masks = masks.to_vec();
        masks.sort_unstable();
        masks.dedup();
        masks.truncate(16);
        let work = sr * sr + n * n + masks.len() * stride * stride * 8;
        if masks.is_empty() || work > maximum_work / 3 {
            return None;
        }
        let mut cells = vec![0u32; n * n];
        for z in 0..sr {
            if cancelled() {
                return None;
            }
            for x in 0..sr {
                cells[(z * n / sr) * n + x * n / sr] |= layers.topology[z * sr + x] as u32;
            }
        }
        let mut prefixes = BTreeMap::new();
        for mask in masks {
            let mut prefix = vec![0u32; stride * stride];
            for z in 0..n {
                if cancelled() {
                    return None;
                }
                for x in 0..n {
                    let i = (z + 1) * stride + x + 1;
                    prefix[i] = u32::from(cells[z * n + x] & mask != 0)
                        + prefix[i - 1]
                        + prefix[i - stride]
                        - prefix[i - stride - 1];
                }
            }
            prefixes.insert(mask, prefix);
        }
        Some(Self {
            size: f64::from(layers.size),
            resolution: n,
            prefixes,
            work,
        })
    }
    fn covers(&self, position: [f32; 2], mask: u32, radius: f32) -> bool {
        let Some(prefix) = self.prefixes.get(&mask) else {
            return true;
        };
        let n = self.resolution;
        let stride = n + 1;
        let index = |world: f64| ((world / self.size + 0.5) * n as f64).floor();
        // Whole coarse blocks and a registration margin enclose all source
        // bilinear texels, including clamped queries at either map edge.
        let low = position.map(|v| {
            (index(f64::from(v) - f64::from(radius)) - 1.).clamp(0., (n - 1) as f64) as usize
        });
        let high = position.map(|v| {
            (index(f64::from(v) + f64::from(radius)) + 1.).clamp(0., (n - 1) as f64) as usize + 1
        });
        prefix[high[1] * stride + high[0]] + prefix[low[1] * stride + low[0]]
            > prefix[low[1] * stride + high[0]] + prefix[high[1] * stride + low[0]]
    }
}
fn topology_requirements(
    prefab: &crate::monument_source::Prefab,
    size: u32,
    resolution: usize,
) -> BTreeMap<u32, f32> {
    let Some(attributes) = &prefab.placement_attributes else {
        return BTreeMap::new();
    };
    if attributes.validate_coastal_inputs().is_err() {
        return BTreeMap::new();
    }
    let movement: f32 = attributes
        .root_decor
        .iter()
        .filter(|d| d.kind == "DecorTransform")
        .map(|d| d.values[0].hypot(d.values[2]))
        .sum();
    let margin = movement + 3. * size as f32 / resolution as f32;
    let mut requirements = BTreeMap::<u32, f32>::new();
    for filter in &prefab.rules.filters {
        let radius = filter.position[0].hypot(filter.position[2]) + filter.radius + margin;
        for mask in [filter.filter.topology_any, filter.filter.topology_all] {
            if mask != 0 && mask != -1 {
                requirements
                    .entry(mask as u32)
                    .and_modify(|r| *r = r.min(radius))
                    .or_insert(radius);
            }
        }
    }
    requirements
}
fn mix(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}
fn gcd(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}
fn keep(heap: &mut BinaryHeap<(u64, usize)>, item: (u64, usize), maximum: usize) {
    if heap.len() < maximum {
        heap.push(item);
    } else if heap.peek().is_some_and(|v| item < *v) {
        heap.pop();
        heap.push(item);
    }
}
pub fn prepare(
    project: &Project,
    layers: &Layers,
    catalog: &Catalog,
    source: &Source,
    policy: &Policy,
    stamp_inputs: Option<(
        &crate::prefab_textures::Bindings,
        &papyrus_terrain::monument_composition::HeightTerrain<'_>,
    )>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Prepared>, String> {
    policy.validate()?;
    let angle = |yaw| {
        project
            .monument_rules
            .as_ref()
            .map_or(yaw, |r| r.placement_yaw(yaw))
    };
    let (hr, sr) = papyrus_terrain::native_layers::dimensions(layers.size)?;
    if project.targets.len() > papyrus_terrain::placement::MAX_TARGETS
        || project.size != layers.size
        || layers.height_resolution != hr
        || layers.surface_resolution != sr
        || layers.height.len() != hr * hr
        || layers.topology.len() != sr * sr
        || layers.splat.len() != sr * sr * 8
        || layers.biome.len() != sr * sr * 5
    {
        return Err("Coastal proposal terrain dimensions differ".into());
    }
    let profile = source
        .coastal_profile
        .as_ref()
        .ok_or("Missing source coastal proposal profile")?;
    let road_entrances =
        crate::coastal_road_entrances::Prepared::prepare(project, catalog, &cancelled)?;
    let mut targets: Vec<_> = project
        .targets
        .iter()
        .filter(|t| !t.exact && project.monument_presence(t) != Presence::Excluded)
        .filter(|t| {
            t.prefab_path
                .as_ref()
                .and_then(|p| source.prefabs.get(p))
                .and_then(|p| p.placement_attributes.as_ref())
                .is_some_and(coastal_placement::swims)
        })
        .collect();
    targets.sort_by_key(|t| t.instance_id);
    // Charge immutable attribute validation once, including the bounded ID set.
    let metadata_work = targets
        .iter()
        .filter_map(|t| t.prefab_path.as_ref())
        .filter_map(|path| source.prefabs[path].placement_attributes.as_ref())
        .map(|a| {
            128 * (1
                + a.root_decor.len()
                + a.terrain_checks.len()
                + a.water_checks.len()
                + a.path_connectors.len()
                + a.placement_blockers.as_ref().map_or(0, Vec::len))
        })
        .sum::<usize>();
    let cached_roots = policy.engine == VERSION && metadata_work <= policy.maximum_work / 3;
    let coverage = if cached_roots {
        let masks = targets
            .iter()
            .filter_map(|t| t.prefab_path.as_ref())
            .flat_map(|path| {
                topology_requirements(&source.prefabs[path], layers.size, sr).into_keys()
            })
            .collect::<Vec<_>>();
        TopologyCoverage::build(layers, &masks, policy.maximum_work, &cancelled)
    } else {
        None
    };
    if cancelled() {
        return Ok(None);
    }
    let preparation_work =
        coverage.as_ref().map_or(0, |c| c.work) + if cached_roots { metadata_work } else { 0 };
    let allowance = (policy.maximum_work - preparation_work) / targets.len().max(1);
    let sites_limit = policy.maximum_sites.min(65536 / targets.len().max(1));
    let terrain = Terrain {
        heights: &layers.height,
        resolution: hr,
        size: layers.size as usize,
    };
    let n = policy.resolution;
    let total = n * n;
    let point = |cell: usize| {
        [
            (cell % n) as f32 / (n - 1) as f32 * layers.size as f32 - layers.size as f32 * 0.5,
            (cell / n) as f32 / (n - 1) as f32 * layers.size as f32 - layers.size as f32 * 0.5,
        ]
    };
    let mut prepared = Prepared {
        targets: BTreeMap::new(),
        audits: Vec::new(),
        preparation_work,
    };
    for t in targets {
        if cancelled() {
            return Ok(None);
        }
        let path = t.prefab_path.as_ref().unwrap();
        let Some(asset) = catalog.entries.get(path) else {
            continue;
        };
        if asset
            .bounds
            .as_ref()
            .is_none_or(|b| b.minimum_world_size > project.size)
        {
            continue;
        }
        let Ok((bounds, _)) = crate::placement_preview::bounds_for_recipe(project, asset) else {
            continue;
        };
        let Some(filter) = profile.filter(path) else {
            continue;
        };
        let prefab = &source.prefabs[path];
        let attributes = prefab.placement_attributes.as_ref().unwrap();
        let requirements = if coverage.is_some() {
            topology_requirements(prefab, layers.size, sr)
        } else {
            BTreeMap::new()
        };
        let root_pose = if cached_roots {
            Some(coastal_placement::PreparedPose::new(layers, attributes)?)
        } else {
            None
        };
        let projected_stamp = if road_entrances.as_ref().is_some_and(|e| e.projects_height())
            && asset.modifier.as_ref().is_some_and(|m| m.height_map)
        {
            let (bindings, _) =
                stamp_inputs.ok_or("Projected coastal proposals require verified stamp inputs")?;
            let Some(item) = crate::composition::prepare_with_budget(
                asset,
                bindings,
                project.size,
                &cancelled,
                256 * 1024 * 1024,
            )?
            else {
                return Ok(None);
            };
            Some(item)
        } else {
            None
        };
        let mut rules = prefab.rules.clone();
        rules.min_corners = profile.minimum_tier_corners;
        rules
            .unsupported
            .retain(|s| s != "DecorAlign requires terrain-relative rotation/scale");
        if !rules.unsupported.is_empty() {
            continue;
        }
        let Some(evaluator) =
            monument_eligibility::Evaluator::new(layers, &rules, bounds[0], &cancelled)?
        else {
            return Ok(None);
        };
        let evaluator = if let Some(filter) = source
            .stage_source
            .as_ref()
            .and_then(|s| s.anchor_filter(path))
        {
            evaluator.with_stage_filter(filter)?
        } else {
            evaluator
        };
        let yaw = t
            .yaw
            .unwrap_or_else(|| asset.yaw.map_or(0., |[a, b]| -(a + b) * 0.5));
        let yaw = angle(yaw);
        let salt =
            mix(u64::from(project.seeds[4].value) ^ t.instance_id.wrapping_mul(0xd6e8feb86659fd93));
        let mut step = (mix(salt) % total as u64) as usize + 1;
        while gcd(step, total) != 1 {
            step = step % total + 1;
        }
        let mut cell = (salt % total as u64) as usize;
        let mut near = BinaryHeap::new();
        let mut spread = BinaryHeap::new();
        let mut audit = Audit {
            id: t.instance_id,
            scanned: 0,
            eligible: 0,
            retained: 0,
            prefilter_rejected: 0,
            rejected: BTreeMap::new(),
            work: 0,
            budget_reached: false,
        };
        for _ in 0..total {
            if cancelled() {
                return Ok(None);
            }
            if audit.work >= allowance {
                audit.budget_reached = true;
                break;
            }
            audit.work += 1;
            audit.scanned += 1;
            let index = cell;
            cell = (cell + step) % total;
            let [x, z] = point(index);
            if let Some(coverage) = &coverage {
                let work = requirements.len() * 4;
                if work > allowance - audit.work {
                    audit.budget_reached = true;
                    break;
                }
                audit.work += work;
                if requirements
                    .iter()
                    .any(|(&mask, &radius)| !coverage.covers([x, z], mask, radius))
                {
                    audit.prefilter_rejected += 1;
                    continue;
                }
            }
            if spawn::sample_factor(
                layers,
                filter,
                x / layers.size as f32 + 0.5,
                z / layers.size as f32 + 0.5,
            )? <= 0.
            {
                *audit.rejected.entry("Coastal stage filter").or_default() += 1;
                continue;
            }
            if let Some(stages) = &source.stage_source
                && !matches!(
                    stages.evaluate(layers, path, project.seeds[4].value, t.instance_id, [x, z])?,
                    papyrus_terrain::monument_stage::Check::Accepted { .. }
                )
            {
                *audit.rejected.entry("Monument stage filter").or_default() += 1;
                continue;
            }
            // Conservatively charge the bounded normal/root sampling as well as
            // the precise filter/anchor work counted by the eligibility evaluator.
            let root_work = if root_pose.is_some() {
                // Four base-height dependencies and sixteen diagonal normal
                // dependencies per DecorAlign, plus each root decorator visit.
                4 + attributes.root_decor.len()
                    + attributes
                        .root_decor
                        .iter()
                        .filter(|d| d.kind == "DecorAlign")
                        .count()
                        * 16
            } else {
                32 + attributes.root_decor.len()
                    + attributes.terrain_checks.len()
                    + attributes.water_checks.len()
                    + attributes.path_connectors.len()
            };
            if allowance - audit.work < root_work {
                audit.budget_reached = true;
                break;
            }
            audit.work += root_work;
            let raw_position = [x, terrain.height_at(x, z), z];
            let pose = if let Some(root) = &root_pose {
                root.source_normal(raw_position, yaw, profile.sea_level)
            } else {
                coastal_placement::pose_source_normal(
                    layers,
                    attributes,
                    raw_position,
                    yaw,
                    profile.sea_level,
                )
            };
            let Ok((position, rotation)) = pose else {
                *audit.rejected.entry("Coastal root pose").or_default() += 1;
                continue;
            };
            let rotation = angle(rotation);
            if bounds.iter().any(|b| {
                !b.at([position[0], position[2]], rotation)
                    .inside_world(layers.size as f32)
            }) {
                *audit.rejected.entry("Authored world bounds").or_default() += 1;
                continue;
            }
            match evaluator.evaluate(position, rotation, &mut audit.work, allowance, &cancelled)? {
                None => return Ok(None),
                Some(Check::Budget) => {
                    audit.budget_reached = true;
                    break;
                }
                Some(Check::Rejected(reason)) => {
                    *audit.rejected.entry(reason).or_default() += 1;
                    continue;
                }
                Some(Check::Accepted(_)) => {}
            }
            if let Some(entrances) = &road_entrances {
                match entrances.check(
                    layers,
                    attributes,
                    asset.modifier.as_ref().is_some_and(|m| m.height_map),
                    position,
                    rotation,
                    &mut audit.work,
                    allowance,
                    &cancelled,
                )? {
                    None => return Ok(None),
                    Some(Check::Budget) => {
                        audit.budget_reached = true;
                        break;
                    }
                    Some(Check::Rejected(reason)) => {
                        *audit.rejected.entry(reason).or_default() += 1;
                        continue;
                    }
                    Some(Check::Accepted(_)) => {}
                }
            }
            if let Some(item) = &projected_stamp
                && let Some(entrances) = &road_entrances
            {
                let (_, height_terrain) =
                    stamp_inputs.ok_or("Missing coastal proposal height terrain")?;
                let stamp = item.stamp_for_recipe(
                    position.map(f64::from),
                    f64::from(rotation),
                    project.composition_engine.as_deref(),
                );
                let projection = height_terrain.projected(&stamp)?;
                match entrances.check_projected(
                    layers,
                    attributes,
                    &projection,
                    position,
                    rotation,
                    &mut audit.work,
                    allowance,
                    &cancelled,
                )? {
                    None => return Ok(None),
                    Some(Check::Budget) => {
                        audit.budget_reached = true;
                        break;
                    }
                    Some(Check::Rejected(reason)) => {
                        *audit.rejected.entry(reason).or_default() += 1;
                        continue;
                    }
                    Some(Check::Accepted(_)) => {}
                }
            }
            audit.eligible += 1;
            let distance = (x - t.x).hypot(z - t.z);
            keep(
                &mut near,
                (u64::from(distance.to_bits()), index),
                sites_limit,
            );
            keep(&mut spread, (mix(salt ^ index as u64), index), sites_limit);
        }
        let near = near.into_sorted_vec();
        let spread = spread.into_sorted_vec();
        let mut seen = HashSet::new();
        let mut points = Vec::new();
        for (a, b) in near.into_iter().zip(spread) {
            for (_, index) in [a, b] {
                if points.len() < sites_limit && seen.insert(index) {
                    points.push(point(index));
                }
            }
        }
        audit.retained = points.len();
        prepared.targets.insert(
            t.instance_id,
            ProposalSet {
                points,
                maximum_accepted: policy.maximum_accepted,
            },
        );
        prepared.audits.push(audit);
    }
    Ok((!cancelled()).then_some(prepared))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires an owned installation and verified current source caches"]
    fn owned_coastal_search_records_proposals_and_final_rejection_reasons() {
        use std::{collections::BTreeMap, path::PathBuf, sync::atomic::AtomicBool};
        let path = |name| PathBuf::from(std::env::var_os(name).expect(name));
        let _verification = crate::installation::VerificationScope::default();
        let root = path("MAPPER_COASTAL_AUDIT_ROOT");
        let project = Project::load(&path("MAPPER_COASTAL_AUDIT_PROJECT")).unwrap();
        let installation = crate::installation::inspect(&root, || false).unwrap();
        let catalog = crate::prefab_assets::load(
            &root,
            &path("MAPPER_COASTAL_AUDIT_CATALOG"),
            &installation,
            || false,
        )
        .unwrap();
        let bindings = crate::prefab_textures::Bindings::load(
            &root,
            &path("MAPPER_COASTAL_AUDIT_BINDINGS"),
            &catalog,
            || false,
        )
        .unwrap();
        let native = crate::native_processing::build(
            &root,
            &path("MAPPER_COASTAL_AUDIT_WORKER"),
            &project.terrain_key(),
            None,
            &AtomicBool::new(false),
            |_, _| {},
        )
        .unwrap()
        .unwrap();
        let layers = &native.source.layers;
        let source = Source::cached(
            project.monument_rules.as_ref().unwrap(),
            &catalog,
            &bindings,
            || false,
        )
        .unwrap();
        let terrain = papyrus_terrain::monument_composition::HeightTerrain::new(
            &layers.height,
            layers.height_resolution,
            layers.size,
        )
        .unwrap();
        let proposals = prepare(
            &project,
            layers,
            &catalog,
            &source,
            project
                .monument_rules
                .as_ref()
                .unwrap()
                .coastal_search
                .as_ref()
                .unwrap(),
            Some((&bindings, &terrain)),
            || false,
        )
        .unwrap()
        .unwrap();
        let mut assessments = BTreeMap::<u64, BTreeMap<String, usize>>::new();
        let solution = crate::placement_preview::solve_native_observed(
            &project,
            layers,
            &catalog,
            &bindings,
            || false,
            |target, result| {
                let reason = match result {
                    papyrus_terrain::placement::CandidateCheck::Rejected(reason)
                    | papyrus_terrain::placement::CandidateCheck::TargetExhausted(reason)
                    | papyrus_terrain::placement::CandidateCheck::Exhausted(reason) => reason.as_str(),
                    papyrus_terrain::placement::CandidateCheck::Accepted(..)
                    | papyrus_terrain::placement::CandidateCheck::AcceptedRelaxed(..) => "Accepted",
                    papyrus_terrain::placement::CandidateCheck::Cancelled => "Cancelled",
                };
                *assessments
                    .entry(target)
                    .or_default()
                    .entry(reason.into())
                    .or_default() += 1;
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(solution.required_missing, 0);
        let report = serde_json::json!({"format":"mapper-owned-coastal-search-audit","schema":1,
            "terrain_seed":project.seeds[0].value,"preparation_work":proposals.preparation_work,"proposal_audits":proposals.audits,
            "final_assessments":assessments,"outcomes":solution.outcomes.iter().map(|o|serde_json::json!({"id":o.id,"placed":o.position.is_some(),"reason":o.reason})).collect::<Vec<_>>(),"game_runtime_validated":false});
        std::fs::write(
            path("MAPPER_COASTAL_AUDIT_REPORT"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
    }
    use crate::{
        installation::{Installation, InstallationKind, SourceFile},
        monument_source::{Model, Prefab},
        prefab_assets::{Asset, Bounds, Modifier},
    };
    #[test]
    fn topology_prefilter_encloses_rotated_bilinear_queries_and_has_bounded_preparation() {
        let (_, layers, _, _) = fixture();
        let coverage = TopologyCoverage::build(&layers, &[16], 8_000_000, &|| false).unwrap();
        assert!(!coverage.covers([-400., 0.], 16, 20.));
        assert!(coverage.covers([400., 0.], 16, 20.));
        let filter = spawn::Filter {
            splat_type: -1,
            biome_type: -1,
            topology_any: 16,
            topology_all: 0,
            topology_not: 0,
        };
        let mut passing = 0;
        for x in [-500_f32, -400., -100., -1., 0., 1., 100., 400., 500.] {
            for radius in [0_f32, 1., 20., 100.] {
                for turn in 0..32 {
                    let angle = turn as f32 * std::f32::consts::TAU / 32.;
                    let q = [x + radius * angle.cos(), radius * angle.sin()];
                    if q.iter().any(|v| v.abs() > 500.) {
                        continue;
                    }
                    if spawn::sample_factor(
                        &layers,
                        &filter,
                        q[0] / 1000. + 0.5,
                        q[1] / 1000. + 0.5,
                    )
                    .unwrap()
                        > 0.
                    {
                        passing += 1;
                        assert!(coverage.covers([x, 0.], 16, radius));
                    }
                }
            }
        }
        assert!(passing > 100);
        assert!(
            coverage.covers([-400., 0.], 128, 0.),
            "Unindexed roles defer to the exact source checks"
        );
        assert!(coverage.work <= 8_000_000 / 3);
        assert!(TopologyCoverage::build(&layers, &[16], 1, &|| false).is_none());
        assert!(TopologyCoverage::build(&layers, &[16], 8_000_000, &|| true).is_none());
        let legacy = Policy {
            engine: LEGACY_VERSION.into(),
            ..Policy::default()
        };
        legacy.validate().unwrap();
    }
    fn fixture() -> (Project, Layers, Catalog, Source) {
        let path = "assets/bundled/prefabs/autospawn/monument/harbor/harbor_1.prefab";
        let mut project = Project {
            size: 1000,
            ..Default::default()
        };
        project.targets.truncate(1);
        let t = &mut project.targets[0];
        t.prefab_path = Some(path.into());
        t.x = -200.;
        t.z = 0.;
        t.exact = false;
        let (hr, sr) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let mut layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![17000; hr * hr],
            splat: vec![0; sr * sr * 8],
            biome: vec![0; sr * sr * 5],
            topology: vec![0; sr * sr],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        for z in 0..sr {
            layers.topology[z * sr + sr / 2..(z + 1) * sr].fill(16);
        }
        let installation = Installation {
            kind: InstallationKind::Client,
            native: SourceFile {
                relative_path: "RustNative.dll".into(),
                sha256: "a".repeat(64),
                bytes: 1,
            },
            scene_manifest: None,
        };
        let asset = Asset {
            priority: Some(0),
            bounds: Some(Bounds {
                center: [0.; 3],
                extent: [5.; 3],
                minimum_world_size: 1000,
            }),
            yaw: None,
            modifier_size: Some([10.; 3]),
            modifier_offset: Some([0.; 3]),
            texture_guids: Default::default(),
            splat_mask: Some(0),
            modifier_absent: false,
            modifier: Some(Modifier {
                height_map: true,
                water_map: None,
                alpha_map: false,
                biome_mask: 0,
                topology_mask: 0,
                radius: 5.,
                fade: 0.,
                auto_cliff_splat: false,
                auto_cliff_topology: false,
                remove_existing_topology: false,
            }),
            water_geometry: None,
            no_build_geometry: None,
            display_name: None,
        };
        let catalog = Catalog {
            sha256: "b".repeat(64),
            installation,
            entries: [(path.into(), asset)].into(),
            sources: vec![],
        };
        let prefab=Prefab {rules:monument_eligibility::Rules {tier_mask:16,min_corners:1,anchors:vec![],filters:vec![monument_eligibility::Filter {
            position:[0.;3],filter:spawn::Filter {splat_type:-1,biome_type:-1,topology_any:16,topology_all:0,topology_not:0},radius:0.,check_bounds:false,check_placement:true}],unsupported:vec![],pending:vec![]},
            model:Model {vertices:vec![],triangles:vec![],issues:vec![],complete:false,simplified:false},
            placement_attributes:Some(serde_json::from_value(serde_json::json!({"version":1,"root_decor":[{"object_id":1,"active":true,"kind":"DecorSwim","values":[]}],"terrain_checks":[],"water_checks":[],"path_connectors":[],"issues":[]})).unwrap())};
        let source = Source {
            sha256: "c".repeat(64),
            prefabs: [(path.into(), prefab)].into(),
            models_requested: false,
            stage_source: None,
            dungeon_source: None, offshore_source: None,
            code_sources: Vec::new(),
            extractor_sha256: "a".repeat(64),
            profile_sha256: "a".repeat(64),
            coastal_profile: Some(
                serde_json::from_str(include_str!(
                    "../../../compatibility/monuments/client-0f703d43-coastal.json"
                ))
                .unwrap(),
            ),
        };
        (project, layers, catalog, source)
    }
    #[test]
    fn policy_is_optional_versioned_and_requires_its_source_normal_prerequisites() {
        let (_, _, _, source) = fixture();
        let mut recipe = source.recipe();
        assert_eq!(recipe.coastal_search, Some(Policy::default()));
        recipe.validate().unwrap();
        assert_eq!(
            serde_json::from_slice::<crate::monument_source::Recipe>(
                &serde_json::to_vec(&recipe).unwrap()
            )
            .unwrap(),
            recipe
        );
        recipe.coastal_search = None;
        recipe.validate().unwrap();
        assert!(
            serde_json::to_value(&recipe)
                .unwrap()
                .get("coastal_search")
                .is_none()
        );
        recipe.coastal_search = Some(Policy::default());
        recipe.coastal_normals = None;
        assert!(recipe.validate().is_err());
        let unknown = Policy {
            engine: "unknown".into(),
            ..Default::default()
        };
        assert!(unknown.validate().is_err());
        let invalid = Policy {
            maximum_work: 0,
            ..Default::default()
        };
        assert!(invalid.validate().is_err());
    }
    #[test]
    fn proposals_retain_source_filters_repeat_by_seed_and_respect_work_and_cancellation() {
        let (mut project, mut layers, catalog, source) = fixture();
        let policy = Policy {
            resolution: 129,
            maximum_work: 2_000_000,
            maximum_sites: 32,
            ..Default::default()
        };
        let id = project.targets[0].instance_id;
        let a = prepare(&project, &layers, &catalog, &source, &policy, None, || {
            false
        })
        .unwrap()
        .unwrap();
        let b = prepare(&project, &layers, &catalog, &source, &policy, None, || {
            false
        })
        .unwrap()
        .unwrap();
        assert_eq!(a.targets[&id].points, b.targets[&id].points);
        assert_eq!(a.targets[&id].points.len(), 32);
        assert!(a.targets[&id].points.iter().all(|p| p[0] >= 0.));
        assert!(a.audits[0].eligible > 32);
        assert!(a.audits[0].work <= policy.maximum_work);
        project.shuffle_monuments();
        let c = prepare(&project, &layers, &catalog, &source, &policy, None, || {
            false
        })
        .unwrap()
        .unwrap();
        assert_ne!(a.targets[&id].points, c.targets[&id].points);
        let limited = Policy {
            maximum_work: 100,
            ..policy.clone()
        };
        let b = prepare(&project, &layers, &catalog, &source, &limited, None, || {
            false
        })
        .unwrap()
        .unwrap();
        assert!(b.audits[0].budget_reached);
        assert!(b.audits[0].work <= 100);
        assert!(
            prepare(&project, &layers, &catalog, &source, &policy, None, || true)
                .unwrap()
                .is_none()
        );
        layers.topology.fill(0);
        let denied = prepare(&project, &layers, &catalog, &source, &policy, None, || {
            false
        })
        .unwrap()
        .unwrap();
        assert!(denied.targets[&id].points.is_empty());
        project.targets[0].exact = true;
        assert!(
            prepare(&project, &layers, &catalog, &source, &policy, None, || {
                false
            })
            .unwrap()
            .unwrap()
            .targets
            .is_empty()
        );
    }
}
