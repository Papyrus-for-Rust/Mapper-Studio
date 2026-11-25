//! Source-style road-object fitting, with optional incremental terrain admission.
//! Read-only recipes retain their original path and random-stream behavior.
#[cfg(test)]
mod utility_tests;
use crate::{
    decor_source,
    road_object_source::{self as source, Source},
};
use papyrus_terrain::{
    decor_fit::{
        self, OrientedBudget, OrientedOutcome, OrientedPose, OrientedRequest, PreparedPrefab,
    },
    native_layers::Layers,
    path_resources,
    path_geometry::Path,
    powerline_sequence,
    spawn,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc};
type V = [f32; 3];
type Q = [f32; 4];
pub const VERSION: &str = "papyrus-road-object-fit-1";
pub const LIVE_VERSION: &str = "papyrus-road-object-live-fit-2";
pub const WORKFLOW_VERSION: &str = "papyrus-road-object-workflow-fit-2";
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub engine: String,
    pub maximum_hierarchy_exclusive: u32,
    pub sample_step_fraction: f32,
    pub endpoint_inset_fraction: f32,
    pub trim_fraction: f32,
    pub endpoint_offsets: [f32; 3],
    pub anchor_mode: i32,
    pub check_anchor_filter: bool,
    pub seed_domain: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub powerline_sequence: Option<powerline_sequence::Policy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_weights: Option<path_resources::Policy>,
}
impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(sequence) = &self.powerline_sequence {
            sequence.validate()?;
        }
        if let Some(resources) = &self.resource_weights {
            resources.validate()?;
        }
        if self.engine != VERSION
            || !(1..=16).contains(&self.maximum_hierarchy_exclusive)
            || !self.sample_step_fraction.is_finite()
            || !(0.01..=1.).contains(&self.sample_step_fraction)
            || !self.endpoint_inset_fraction.is_finite()
            || !(0. ..=1.).contains(&self.endpoint_inset_fraction)
            || !self.trim_fraction.is_finite()
            || !(0. ..=0.5).contains(&self.trim_fraction)
            || self.endpoint_offsets[0] != 0.
            || self
                .endpoint_offsets
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 4.)
            || self.endpoint_offsets[1] == self.endpoint_offsets[2]
            || self.endpoint_offsets[1] == 0.
            || self.endpoint_offsets[2] == 0.
            || !(0..=2).contains(&self.anchor_mode)
            || self.seed_domain.is_empty()
            || self.seed_domain.len() > 128
            || !self
                .seed_domain
                .bytes()
                .all(|v| v.is_ascii_alphanumeric() || v == b'-')
        {
            return Err("Invalid source road-object path policy".into());
        }
        Ok(())
    }
}
pub struct Road {
    pub hierarchy: u32,
    pub width: f32,
    pub start: bool,
    pub end: bool,
    pub points: Vec<V>,
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Settings {
    pub seed: u32,
    pub maximum_work: usize,
    pub maximum_poses: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Group {
    Start,
    End,
    Side,
    Path,
}
#[derive(Debug, Serialize)]
pub struct Pose {
    pub stage: usize,
    pub road: usize,
    pub group: Group,
    pub rule: usize,
    pub prefab: String,
    pub transform: OrientedPose,
}
#[derive(Debug, Serialize)]
pub struct Trim {
    pub stage: usize,
    pub road: usize,
    pub min_index: usize,
    pub max_index: usize,
}
#[derive(Debug, Serialize)]
pub struct Sequence {
    pub stage: usize,
    pub road: usize,
    pub group: Group,
    pub rule: usize,
    pub requested: usize,
    pub poses: Vec<usize>,
}
#[derive(Debug, Serialize)]
pub struct Readiness {
    pub unsupported: Vec<String>,
    pub pending: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub source_sha256: String,
    pub terrain_sha256: String,
    pub seed: u32,
    pub policy: Policy,
    pub provisional_poses: Vec<Pose>,
    pub trims: Vec<Trim>,
    pub neighbor_sequences: Vec<Sequence>,
    pub prefabs: BTreeMap<String, Readiness>,
    pub counts: BTreeMap<String, usize>,
    pub work: usize,
    pub surface_samples: usize,
    pub height_samples: usize,
    pub path_height_samples: usize,
    pub filter_checks: usize,
    pub budget_exhausted: bool,
    pub placed_instances: usize,
    pub terrain_applied: bool,
    pub physics_validated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global_admission_ids: Option<Vec<usize>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global_admission_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub utilities: Option<Box<Report>>,
}
pub(crate) fn terrain_digest(layers: &Layers, alpha: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"papyrus-road-object-terrain-1");
    for value in [
        layers.size,
        layers.terrain_seed,
        layers.biome_seed,
        layers.height_resolution as u32,
        layers.surface_resolution as u32,
    ] {
        hash.update(value.to_le_bytes());
    }
    for chunk in layers.height.chunks(8192) {
        hash.update(
            chunk
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<_>>(),
        );
    }
    for chunk in layers.topology.chunks(4096) {
        hash.update(
            chunk
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<_>>(),
        );
    }
    hash.update(&layers.biome);
    hash.update(&layers.splat);
    hash.update(alpha);
    format!("{:x}", hash.finalize())
}
fn sub(a: V, b: V) -> V {
    std::array::from_fn(|i| a[i] - b[i])
}
fn norm(v: V) -> V {
    let n = v.iter().map(|v| v * v).sum::<f32>().sqrt();
    if n > 1e-6 { v.map(|v| v / n) } else { [0.; 3] }
}
fn distance(a: V, b: V) -> f32 {
    sub(a, b).iter().map(|v| v * v).sum::<f32>().sqrt()
}
fn quarter(v: V) -> V {
    [v[2], v[1], -v[0]]
}
fn qmul(a: Q, b: Q) -> Q {
    [
        a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1],
        a[3] * b[1] - a[0] * b[2] + a[1] * b[3] + a[2] * b[0],
        a[3] * b[2] + a[0] * b[1] - a[1] * b[0] + a[2] * b[3],
        a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2],
    ]
}
fn unit(state: &mut u64) -> f32 {
    *state = state.wrapping_add(0x9e3779b97f4a7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    ((z ^ (z >> 31)) >> 40) as f32 / 16777216.
}
struct PathState<'a> {
    policy: &'a Policy,
    settings: Settings,
    prepared: BTreeMap<String, PreparedPrefab>,
    sequences: BTreeMap<String, SequenceKind>,
    resources: ResourcePools,
    source: &'a Source,
    out: Report,
    admission_ids: Vec<usize>,
}
struct Runner<'a, F: Fn() -> bool> {
    layers: &'a Layers,
    alpha: &'a [u8],
    path: PathState<'a>,
    fitter: OrientedBudget<'a>,
    transaction: Option<papyrus_terrain::footprints::RoadTransaction<'a>>,
    cancelled_seen: bool,
    cancelled: F,
    mixed: bool,
    realm_mask: u32,
    next_admission: usize,
    prefab_options: Option<&'a crate::recipe_options::Options>,
}
#[derive(Clone, Copy)]
struct Location {
    stage: usize,
    road: usize,
    group: Group,
    rule: usize,
}
#[derive(Clone, Copy)]
enum SequenceKind {
    Plain,
    Unsupported,
    Powerline(powerline_sequence::Rule),
}
struct NeighborSequence {
    pool: Option<powerline_sequence::Pool>,
    history: powerline_sequence::History,
    length: usize,
    index: usize,
}
struct WeightedResources {
    paths: Vec<String>,
    pool: path_resources::Pool,
}
type ResourcePools = BTreeMap<(usize, Group, usize), Arc<WeightedResources>>;
enum Resources<'a> {
    Uniform(&'a [String]),
    Weighted(Arc<WeightedResources>),
}
impl Resources<'_> {
    fn paths(&self) -> &[String] {
        match self {
            Self::Uniform(paths) => paths,
            Self::Weighted(resources) => &resources.paths,
        }
    }
    fn select(&self, unit: f32) -> Result<usize, String> {
        match self {
            Self::Uniform(paths) => Ok(((unit * paths.len() as f32) as usize).min(paths.len() - 1)),
            Self::Weighted(resources) => resources.pool.select(unit)?
                .ok_or_else(|| "Cannot sample an empty path resource pool".into()),
        }
    }
}
fn resource_count(prefab: &source::Prefab) -> Result<usize, String> {
    let attributes: Vec<_> = prefab.attributes.iter()
        .filter(|attribute| attribute["kind"] == "PrefabParameters").collect();
    if attributes.len() != prefab.components.get("PrefabParameters").copied().unwrap_or(0)
        || attributes.iter().any(|attribute| !attribute["root"].is_boolean())
    {
        return Err("Path prefab parameter coverage differs from its component roster".into());
    }
    // GameObject.GetComponent selects the first component on the prefab root.
    let Some(attribute) = attributes.iter().find(|attribute| attribute["root"] == true) else {
        return Ok(1);
    };
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Fields {
        #[serde(rename = "Count")]
        count: f32,
        #[serde(rename = "Priority")]
        _priority: i32,
    }
    let fields: Fields = serde_json::from_value(attribute["fields"].clone())
        .map_err(|error| format!("Invalid path prefab parameters: {error}"))?;
    path_resources::count_tickets(fields.count)
}
fn prepare_resources(
    source: &Source,
    counts: &BTreeMap<String, usize>,
    policy: Option<&path_resources::Policy>,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<ResourcePools>, String> {
    let mut prepared = BTreeMap::new();
    let Some(policy) = policy else { return Ok(Some(prepared)); };
    for (stage_index, stage) in source.stages.iter().enumerate() {
        let rules = stage.start.iter().enumerate().map(|(i, r)| (Group::Start, i, &r.prefabs))
            .chain(stage.end.iter().enumerate().map(|(i, r)| (Group::End, i, &r.prefabs)))
            .chain(stage.side.iter().enumerate().map(|(i, r)| (Group::Side, i, &r.prefabs)))
            .chain(stage.path.iter().enumerate().map(|(i, r)| (Group::Path, i, &r.prefabs)));
        for (group, index, paths) in rules {
            if cancelled() { return Ok(None); }
            let mut eligible = Vec::new();
            let mut weights = Vec::new();
            for path in paths {
                if cancelled() { return Ok(None); }
                let weight = *counts.get(path).ok_or("Missing path prefab resource count")?;
                if weight > 0 {
                    eligible.push(path.clone());
                    weights.push(weight);
                }
            }
            let pool = path_resources::Pool::new(&weights, policy)?;
            prepared.insert((stage_index, group, index), Arc::new(WeightedResources { paths: eligible, pool }));
        }
    }
    Ok(Some(prepared))
}
impl SequenceKind {
    fn rule(self) -> Option<powerline_sequence::Rule> {
        match self {
            Self::Powerline(rule) => Some(rule),
            _ => None,
        }
    }
}
fn sequence_kind(prefab: &source::Prefab) -> Result<SequenceKind, String> {
    if prefab.components.keys().any(|name| {
        name.starts_with("PathSequence") && name != "PathSequencePowerline"
    }) {
        return Ok(SequenceKind::Unsupported);
    }
    let Some(&count) = prefab.components.get("PathSequencePowerline") else {
        return Ok(SequenceKind::Plain);
    };
    let attributes: Vec<_> = prefab.attributes.iter()
        .filter(|attribute| attribute["kind"] == "PathSequencePowerline")
        .collect();
    if count != 1 || attributes.len() != 1 {
        return Err("Powerline sequence component coverage differs from its attributes".into());
    }
    if attributes[0]["root"] != true {
        return Ok(SequenceKind::Unsupported);
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Fields {
        #[serde(rename = "Rule")]
        rule: u8,
    }
    let fields: Fields = serde_json::from_value(attributes[0]["fields"].clone())
        .map_err(|error| format!("Invalid powerline sequence fields: {error}"))?;
    use powerline_sequence::Rule;
    let rule = match fields.rule {
        0 => Rule::Platform,
        1 => Rule::Regular,
        _ => return Err("Unknown powerline sequence rule".into()),
    };
    Ok(SequenceKind::Powerline(rule))
}
impl<F: Fn() -> bool> Runner<'_, F> {
    fn resources<'p>(&self, paths: &'p [String], location: Location) -> Result<Resources<'p>, String> {
        if self.path.policy.resource_weights.is_none() {
            return Ok(Resources::Uniform(paths));
        }
        self.path.resources.get(&(location.stage, location.group, location.rule)).cloned()
            .map(Resources::Weighted).ok_or_else(|| "Missing prepared path resource pool".into())
    }
    fn is_cancelled(&mut self) -> bool {
        self.cancelled_seen |= (self.cancelled)();
        self.cancelled_seen
    }
    fn view(&self) -> papyrus_terrain::native_layers::View<'_> {
        self.transaction
            .as_ref()
            .map_or_else(|| self.layers.view(), |t| t.view())
    }
    fn count(&mut self, name: &str) {
        *self.path.out.counts.entry(name.into()).or_default() += 1;
    }
    fn tick(&mut self) -> bool {
        if self.is_cancelled() {
            return false;
        }
        if self.path.out.work >= self.path.settings.maximum_work
            || self.path.out.provisional_poses.len() >= self.path.settings.maximum_poses
        {
            self.path.out.budget_exhausted = true;
            false
        } else {
            self.path.out.work += 1;
            true
        }
    }
    fn filter(&mut self, filter: &spawn::Filter, p: V) -> Result<f32, String> {
        self.path.out.filter_checks += 1;
        let u = p[0] / self.layers.size as f32 + 0.5;
        let v = p[2] / self.layers.size as f32 + 0.5;
        if !(0. ..=1.).contains(&u) || !(0. ..=1.).contains(&v) {
            return Ok(0.);
        }
        let n = self.layers.surface_resolution;
        let x = (u * n as f32).min((n - 1) as f32) as usize;
        let z = (v * n as f32).min((n - 1) as f32) as usize;
        if self.alpha[z * n + x] < 128 {
            return Ok(0.);
        }
        spawn::sample_factor_view(self.view(), filter, u, v)
    }
    fn height(&mut self, p: V) -> Result<f32, String> {
        self.path.out.path_height_samples += 1;
        spawn::sample_height_view(
            self.view(),
            p[0] / self.layers.size as f32 + 0.5,
            p[2] / self.layers.size as f32 + 0.5,
        )
    }
    fn basis_fit(
        &mut self,
        path: &str,
        p: V,
        q: Q,
        filter: &spawn::Filter,
        seed: u64,
        decor: bool,
    ) -> Result<Option<OrientedPose>, String> {
        if !self.tick() {
            return Ok(None);
        }
        if self.prefab_options.is_some_and(|options| {
            options.road_object_blocked(path, self.path.source.path_family == source::PathFamily::Road)
        }) {
            self.count("blocked_by_recipe");
            return Ok(None);
        }
        let Some(prefab) = self.path.prepared.get(path) else {
            self.count("missing_prefab");
            return Ok(None);
        };
        let view = self
            .transaction
            .as_ref()
            .map_or_else(|| self.layers.view(), |t| t.view());
        match self.fitter.try_fit(
            view,
            OrientedRequest {
                position: p,
                rotation: q,
                filter,
                prefab,
                seed,
                apply_decor: decor,
                check_anchor_filter: self.path.policy.check_anchor_filter,
                anchor_mode: self.path.policy.anchor_mode,
            },
        )? {
            OrientedOutcome::Fitted(p) => Ok(Some(p)),
            OrientedOutcome::Rejected(reason) => {
                self.count(&format!("fit_{reason:?}").to_lowercase());
                if reason == decor_fit::OrientedRejection::Budget {
                    self.path.out.budget_exhausted = true;
                }
                Ok(None)
            }
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn endpoint(
        &mut self,
        rule: &source::Basic,
        resources: &Resources<'_>,
        road: &Road,
        path: &Path,
        end: bool,
        check: bool,
        location: Location,
        rng: &mut u64,
    ) -> Result<bool, String> {
        let paths = resources.paths();
        if rule.folder.is_empty() || paths.is_empty() {
            return Ok(false);
        }
        let (lo, hi) = path.range();
        let i = if end { hi } else { lo };
        let p = path.points()[i];
        let mut dir = path.tangents()[i];
        if end {
            dir = dir.map(|v| -v);
        }
        if !rule.align_to_normal {
            dir = norm([dir[0], 0., dir[2]]);
        }
        let right = quarter(dir);
        let offset = road.width * 0.5 + rule.offset;
        for (index, side) in self.path.policy.endpoint_offsets.into_iter().enumerate() {
            if rule.placement == 0 && index != 0 || rule.placement == 1 && index == 0 {
                continue;
            }
            let mut point = std::array::from_fn(|i| p[i] + right[i] * offset * side);
            if self.filter(&rule.filter, point)? <= 0.5 {
                continue;
            }
            if rule.height_to_terrain {
                point[1] = self.height(point)?;
            }
            let forward = if index == 2 {
                [-dir[0], dir[1], -dir[2]]
            } else {
                dir
            };
            let Some(q) = decor_fit::path_rotation(forward) else {
                continue;
            };
            if check {
                let mut valid = true;
                for prefab in paths {
                    if self
                        .basis_fit(prefab, point, q, &rule.filter, 0, false)?
                        .is_none()
                    {
                        valid = false;
                        break;
                    }
                }
                if valid {
                    return Ok(true);
                }
            } else {
                let chosen = resources.select(unit(rng))?;
                let seed = rng.wrapping_add(0x9e3779b97f4a7c15);
                unit(rng);
                if let Some(pose) =
                    self.basis_fit(&paths[chosen], point, q, &rule.filter, seed, true)?
                    && self.push(location, &paths[chosen], pose)?.is_some()
                {
                    return Ok(true);
                }
            }
            if self.path.out.budget_exhausted {
                break;
            }
        }
        Ok(false)
    }
    fn push(
        &mut self,
        l: Location,
        prefab: &str,
        transform: OrientedPose,
    ) -> Result<Option<usize>, String> {
        let index = self.path.out.provisional_poses.len();
        self.path.out.provisional_poses.push(Pose {
            stage: l.stage,
            road: l.road,
            group: l.group,
            rule: l.rule,
            prefab: prefab.into(),
            transform,
        });
        if let Some(transaction) = &mut self.transaction {
            let id = if self.mixed {
                let id = self.next_admission;
                self.next_admission = id
                    .checked_add(1)
                    .ok_or("Mixed object admission ID overflow")?;
                self.path.admission_ids.push(id);
                id
            } else {
                index
            };
            let placement = if self.mixed {
                crate::road_object_effects::placement_stage_for_realm(
                    self.path.source,
                    &self.path.out.provisional_poses[index],
                    0,
                    Some(self.realm_mask),
                )
            } else {
                crate::road_object_effects::placement_stage(
                    self.path.source,
                    &self.path.out.provisional_poses[index],
                    0,
                )
            }?;
            let pose = &self.path.out.provisional_poses[index].transform;
            let result = transaction.try_apply(
                id,
                &placement,
                prefab,
                &decor_fit::Pose {
                    candidate: id,
                    position: pose.position,
                    rotation: pose.rotation,
                    scale: pose.scale,
                },
                &self.cancelled,
            )?;
            match result {
                Some(true) => {}
                Some(false) => {
                    self.count("effect_rejected");
                    return Ok(None);
                }
                None => {
                    self.cancelled_seen = true;
                    return Ok(None);
                }
            }
        }
        Ok(Some(index))
    }
    #[allow(clippy::too_many_arguments)]
    fn emit(
        &mut self,
        resources: &Resources<'_>,
        p: V,
        q: Q,
        filter: &spawn::Filter,
        l: Location,
        rng: &mut u64,
        sequence: Option<NeighborSequence>,
    ) -> Result<Option<usize>, String> {
        let paths = resources.paths();
        if paths.is_empty() {
            return Ok(None);
        }
        let mut selected = resources.select(unit(rng))?;
        if let Some(sequence) = sequence {
            let kind = self.path.sequences[&paths[selected]];
            if let (SequenceKind::Powerline(rule), Some(pool)) = (kind, sequence.pool)
                && let Some(replacement) = pool.replacement(
                    Some(rule), sequence.history, sequence.length, sequence.index,
                )?
            {
                selected = replacement;
            }
            let kind = self.path.sequences[&paths[selected]];
            if matches!(kind, SequenceKind::Unsupported)
                || matches!(kind, SequenceKind::Powerline(_)) && sequence.pool.is_none()
            {
                self.count("unsupported_path_sequence");
                return Ok(None);
            }
        }
        let path = &paths[selected];
        let seed = rng.wrapping_add(0x9e3779b97f4a7c15);
        unit(rng);
        let Some(pose) = self.basis_fit(path, p, q, filter, seed, true)? else {
            return Ok(None);
        };
        self.push(l, path, pose)
    }
    fn neighbors(
        &mut self,
        points: &[V],
        resources: &Resources<'_>,
        filter: &spawn::Filter,
        l: Location,
        rng: &mut u64,
    ) -> Result<(), String> {
        let mut ids = Vec::new();
        let paths = resources.paths();
        let sequence_pool = self.path.policy.powerline_sequence.as_ref()
            .filter(|_| !paths.is_empty())
            .map(|policy| {
                let rules: Vec<_> = paths.iter().map(|path| self.path.sequences[path].rule()).collect();
                powerline_sequence::Pool::new(&rules, policy)
            }).transpose()?;
        let mut history = powerline_sequence::History::default();
        if points.len() >= 2 {
            for (i, &p) in points.iter().enumerate() {
                if self.is_cancelled() || self.path.out.budget_exhausted {
                    break;
                }
                let d = sub(
                    points[(i + 1).min(points.len() - 1)],
                    points[i.saturating_sub(1)],
                );
                let Some(q) = decor_fit::path_rotation([d[0], 0., d[2]]) else {
                    continue;
                };
                let sequence = NeighborSequence {
                    pool: sequence_pool, history, length: points.len(), index: i,
                };
                if let Some(index) = self.emit(resources, p, q, filter, l, rng, Some(sequence))? {
                    let kind = self.path.sequences[&self.path.out.provisional_poses[index].prefab];
                    history.accept(kind.rule())?;
                    ids.push(index);
                }
            }
        }
        self.path.out.neighbor_sequences.push(Sequence {
            stage: l.stage,
            road: l.road,
            group: l.group,
            rule: l.rule,
            requested: points.len(),
            poses: ids,
        });
        Ok(())
    }
    fn side(
        &mut self,
        r: &source::Side,
        road: &Road,
        path: &Path,
        l: Location,
        rng: &mut u64,
    ) -> Result<(), String> {
        if r.folder.is_empty() || r.prefabs.is_empty() {
            return Ok(());
        }
        let resources = self.resources(&r.prefabs, l)?;
        if resources.paths().is_empty() {
            return Ok(());
        }
        let (start, end) = path.offsets();
        let mut d = start + r.distance * self.path.policy.endpoint_inset_fraction;
        let last = path.length() - end - r.distance * self.path.policy.endpoint_inset_fraction;
        let mut previous = path.points()[path.range().0];
        let mut cursor = 0;
        let mut neighbors = Vec::new();
        while d <= last {
            if self.is_cancelled() || !self.tick() {
                break;
            }
            let at = d;
            let next = d + r.distance * self.path.policy.sample_step_fraction;
            if next <= d {
                return Err("Road sampling distance cannot advance".into());
            }
            d = next;
            let center = path
                .point(at)
                .map_err(|e| format!("Road object curve: {e:?}"))?;
            if distance(center, previous) < r.distance {
                continue;
            }
            let tangent = path
                .tangent(at)
                .map_err(|e| format!("Road object tangent: {e:?}"))?;
            let right = quarter(tangent);
            for side in 0..2 {
                let index = (cursor + side) % 2;
                if r.side == 1 && index != 0 || r.side == 2 && index != 1 {
                    continue;
                }
                let offset = (road.width * 0.5 + r.offset) * if index == 0 { -1. } else { 1. };
                let mut p = center;
                p[0] += right[0] * offset;
                p[2] += right[2] * offset;
                if self.filter(&r.filter, p)? < unit(rng) {
                    self.count("filter_miss");
                    continue;
                }
                if r.density >= unit(rng) {
                    p[1] = self.height(p)?;
                    if r.alignment == 1 {
                        neighbors.push(p);
                    } else {
                        let forward = if r.alignment == 0 {
                            [0.; 3]
                        } else {
                            tangent.map(|v| v * offset)
                        };
                        let Some(mut q) = decor_fit::path_rotation(forward) else {
                            continue;
                        };
                        if r.alignment == 3 {
                            let h = std::f32::consts::FRAC_1_SQRT_2;
                            q = qmul(q, [0., -h, 0., h]);
                        }
                        if self
                            .emit(&resources, p, q, &r.filter, l, rng, None)?
                            .is_none()
                        {
                            continue;
                        }
                    }
                } else {
                    self.count("density_miss");
                }
                // Density misses advance source spacing/alternation too; failed
                // immediate anchor fits and filter misses do not.
                cursor = index;
                previous = center;
                if r.side == 3 {
                    break;
                }
            }
            if self.path.out.budget_exhausted {
                break;
            }
        }
        if r.alignment == 1 && !self.path.out.budget_exhausted {
            self.neighbors(&neighbors, &resources, &r.filter, l, rng)?;
        }
        Ok(())
    }
    fn along(
        &mut self,
        r: &source::Along,
        path: &Path,
        l: Location,
        rng: &mut u64,
    ) -> Result<(), String> {
        if r.folder.is_empty() || r.prefabs.is_empty() {
            return Ok(());
        }
        let resources = self.resources(&r.prefabs, l)?;
        if resources.paths().is_empty() {
            return Ok(());
        }
        let (start, end) = path.offsets();
        let mut d = start + r.distance * self.path.policy.endpoint_inset_fraction;
        let last = path.length() - end - r.distance * self.path.policy.endpoint_inset_fraction;
        let mut previous = path.points()[path.range().0];
        let mut neighbors = Vec::new();
        while d <= last {
            if self.is_cancelled() || !self.tick() {
                break;
            }
            let at = d;
            let next = d + r.distance * self.path.policy.sample_step_fraction;
            if next <= d {
                return Err("Road sampling distance cannot advance".into());
            }
            d = next;
            let center = path
                .point(at)
                .map_err(|e| format!("Road object curve: {e:?}"))?;
            if distance(center, previous) < r.distance {
                continue;
            }
            let tangent = path
                .tangent(at)
                .map_err(|e| format!("Road object tangent: {e:?}"))?;
            let mut p = center;
            p[0] += (unit(rng) * 2. - 1.) * r.dithering;
            p[2] += (unit(rng) * 2. - 1.) * r.dithering;
            if self.filter(&r.filter, p)? < unit(rng) {
                self.count("filter_miss");
                continue;
            }
            if r.density >= unit(rng) {
                p[1] = self.height(p)?;
                if r.alignment == 1 {
                    neighbors.push(p);
                } else {
                    let forward = match r.alignment {
                        0 => [0.; 3],
                        2 => tangent,
                        _ => quarter(tangent),
                    };
                    let Some(q) = decor_fit::path_rotation(forward) else {
                        continue;
                    };
                    if self
                        .emit(&resources, p, q, &r.filter, l, rng, None)?
                        .is_none()
                    {
                        continue;
                    }
                }
            } else {
                self.count("density_miss");
            }
            previous = center;
            if self.path.out.budget_exhausted {
                break;
            }
        }
        if r.alignment == 1 && !self.path.out.budget_exhausted {
            self.neighbors(&neighbors, &resources, &r.filter, l, rng)?;
        }
        Ok(())
    }
}
pub fn fit(
    layers: &Layers,
    alpha: &[u8],
    roads: &[Road],
    source: &Source,
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    Ok(
        fit_incremental(layers, alpha, roads, source, settings, None, cancelled)?
            .map(|(report, _)| report),
    )
}
pub(crate) fn fit_incremental<'a>(
    layers: &'a Layers,
    alpha: &'a [u8],
    roads: &'a [Road],
    source: &'a Source,
    settings: Settings,
    transaction: Option<papyrus_terrain::footprints::RoadTransaction<'a>>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<(Report, Option<papyrus_terrain::footprints::Output>)>, String> {
    let Some(mut emitter) = Emitter::new(
        layers,
        alpha,
        roads,
        source,
        settings,
        transaction,
        cancelled,
    )?
    else {
        return Ok(None);
    };
    for si in 0..source.stages.len() {
        if emitter.run.path.out.budget_exhausted
            && source.stages[si].enabled()
            && source.stages[si].mode == -1
        {
            break;
        }
        if emitter.emit_stage(si)?.is_none() {
            return Ok(None);
        }
    }
    emitter.finish()
}
/// A single private road producer, resumable only at whole source-stage
/// boundaries. Prepared attributes, curves, terrain locks and both work budgets
/// stay owned here; an out-of-order call permanently prevents publication.
pub(crate) struct Emitter<'a, F: Fn() -> bool> {
    run: Runner<'a, F>,
    roads: &'a [Road],
    curves: Vec<Path>,
    next_stage: usize,
    failed: bool,
    expected_decor: Option<usize>,
    next_decor: usize,
    expected_cliffs: usize,
    next_cliff: usize,
    expected_coastal: usize,
    next_coastal: usize,
    utility: Option<UtilityPaths<'a>>,
    last_path_order: Option<usize>,
}
struct UtilityPaths<'a> {
    state: PathState<'a>,
    roads: Vec<Road>,
    curves: Vec<Path>,
    layout_ready: bool,
    next_stage: usize,
}
pub(crate) struct DecorationStage<'s, 'terrain> {
    pub stage: &'s papyrus_terrain::decor::Stage,
    pub index: usize,
    pub seed: u32,
    pub realm: u32,
    pub prefabs: &'s BTreeMap<String, PreparedPrefab>,
    pub attempts: &'s mut papyrus_terrain::decor_stream::Budget,
    pub water: Option<papyrus_terrain::decor_fit::ValidatedWater<'terrain>>,
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Progress {
    pub admissions: usize,
    pub surface_samples: usize,
    pub height_samples: usize,
    pub terrain_work: usize,
}
impl<'a> PathState<'a> {
    fn prepare(
        source: &'a Source,
        settings: Settings,
        terrain_sha256: String,
        live: bool,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        source.validate_for_fitting()?;
        let policy = source
            .path_policy
            .as_ref()
            .ok_or("Road fitting requires a source-bound placement profile")?;
        policy.validate()?;
        if !(1..=2_000_000).contains(&settings.maximum_work)
            || !(1..=100_000).contains(&settings.maximum_poses)
        {
            return Err("Invalid road object fitting bounds".into());
        }
        let mut prepared = BTreeMap::new();
        let mut readiness = BTreeMap::new();
        let mut sequences = BTreeMap::new();
        let mut resource_counts = BTreeMap::new();
        for (path, p) in &source.prefabs {
            if cancelled() {
                return Ok(None);
            }
            if policy.resource_weights.is_some() {
                let count = resource_count(p)?;
                resource_counts.insert(path.clone(), count);
                if count == 0 {
                    readiness.insert(
                        path.clone(),
                        Readiness {
                            unsupported: vec![],
                            pending: vec!["Source count disables this prefab".into()],
                        },
                    );
                    continue;
                }
            }
            let kind = sequence_kind(p)?;
            let mut compiled = decor_source::compile_road_prefab(p)?;
            if policy.powerline_sequence.is_some() && matches!(kind, SequenceKind::Powerline(_)) {
                compiled
                    .pending
                    .retain(|issue| issue != "PathSequencePowerline");
            }
            readiness.insert(
                path.clone(),
                Readiness {
                    unsupported: compiled.unsupported.clone(),
                    pending: compiled.pending.clone(),
                },
            );
            prepared.insert(path.clone(), PreparedPrefab::new(compiled)?);
            sequences.insert(path.clone(), kind);
        }
        let Some(resources) = prepare_resources(
            source,
            &resource_counts,
            policy.resource_weights.as_ref(),
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        let engine = if live { LIVE_VERSION } else { VERSION };
        Ok(Some(Self {
            policy,
            settings,
            prepared,
            sequences,
            resources,
            source,
            admission_ids: vec![],
            out: Report {
                engine,
                source_sha256: source.sha256.clone(),
                terrain_sha256,
                seed: settings.seed,
                policy: policy.clone(),
                provisional_poses: vec![],
                trims: vec![],
                neighbor_sequences: vec![],
                prefabs: readiness,
                counts: BTreeMap::new(),
                work: 0,
                surface_samples: 0,
                height_samples: 0,
                path_height_samples: 0,
                filter_checks: 0,
                budget_exhausted: false,
                placed_instances: 0,
                terrain_applied: false,
                physics_validated: false,
                global_admission_ids: None,
                global_admission_count: None,
                utilities: None,
            },
        }))
    }
}
fn prepare_curves(
    roads: &[Road],
    family: source::PathFamily,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Vec<Path>>, String> {
    let minimum_width = if family == source::PathFamily::Powerline {
        0.
    } else {
        1.
    };
    if roads.len() > 4096
        || roads.iter().map(|r| r.points.len()).sum::<usize>() > 1_000_000
        || roads
            .iter()
            .any(|r| !r.width.is_finite() || !(minimum_width..=32.).contains(&r.width))
    {
        return Err("Invalid road object fitting bounds".into());
    }
    let mut curves = Vec::with_capacity(roads.len());
    for road in roads {
        if cancelled() {
            return Ok(None);
        }
        let path = Path::from_controls(&road.points, 2_000_000, cancelled)
            .map_err(|e| format!("Road object path: {e:?}"))?;
        let Some(path) = path else {
            return Ok(None);
        };
        if path.length() <= 0. || path.length() > 1_000_000. {
            return Err("Unsupported road object path length".into());
        }
        curves.push(path);
    }
    Ok(Some(curves))
}
impl<'a, F: Fn() -> bool> Emitter<'a, F> {
    pub(crate) fn view(&self) -> papyrus_terrain::native_layers::View<'_> {
        self.run.view()
    }
    pub(crate) fn live_road_connectors(
        &self,
        power: i32,
    ) -> Result<Vec<(usize, crate::road_object_connectors::Connector)>, String> {
        let transaction = self
            .run
            .transaction
            .as_ref()
            .ok_or("Missing live object terrain")?;
        let applied: std::collections::BTreeSet<_> =
            transaction.applied().iter().map(|row| row.pose).collect();
        let mut output = Vec::new();
        for (index, (pose, &id)) in self
            .run
            .path
            .out
            .provisional_poses
            .iter()
            .zip(&self.run.path.admission_ids)
            .enumerate()
        {
            if !applied.contains(&id) {
                continue;
            }
            let prefab = self
                .run
                .path
                .source
                .prefabs
                .get(&pose.prefab)
                .ok_or("Missing live road prefab")?;
            if prefab.components.get("MonumentInfo").copied().unwrap_or(0) == 0 {
                continue;
            }
            for socket in crate::road_object_connectors::local(prefab)? {
                if socket.infrastructure_type == power {
                    if output.len() >= 4096 {
                        return Err("Utility road connector allowance exhausted".into());
                    }
                    output.push((
                        id,
                        crate::road_object_connectors::world(
                            index,
                            &pose.prefab,
                            &socket,
                            &pose.transform,
                        )?,
                    ));
                }
            }
        }
        Ok(output)
    }
    pub(crate) fn progress(&self) -> Progress {
        Progress {
            admissions: self.run.next_admission,
            surface_samples: self.run.fitter.samples(),
            height_samples: self.run.fitter.height_samples(),
            terrain_work: self
                .run
                .transaction
                .as_ref()
                .map_or(0, |t| t.sample_visits()),
        }
    }
    pub(crate) fn new(
        layers: &'a Layers,
        alpha: &'a [u8],
        roads: &'a [Road],
        source: &'a Source,
        settings: Settings,
        transaction: Option<papyrus_terrain::footprints::RoadTransaction<'a>>,
        cancelled: F,
    ) -> Result<Option<Self>, String> {
        if cancelled() {
            return Ok(None);
        }
        let Some(path) = PathState::prepare(source, settings, terrain_digest(layers, alpha),
            transaction.is_some(), &cancelled)? else { return Ok(None); };
        let Some(curves) = prepare_curves(roads, source.path_family, &cancelled)? else { return Ok(None); };
        let run = Runner {
            layers, alpha, path, fitter: OrientedBudget::new(layers, alpha)?,
            cancelled_seen: false, transaction, cancelled, mixed: false,
            realm_mask: 0, next_admission: 0,
            prefab_options: None,
        };
        Ok(Some(Self {
            run,
            roads,
            curves,
            next_stage: 0,
            failed: false,
            expected_decor: None,
            next_decor: 0,
            expected_cliffs: 0,
            next_cliff: 0,
            expected_coastal: 0,
            next_coastal: 0,
            utility: None,
            last_path_order: None,
        }))
    }
    pub(crate) fn with_fitting_limit(mut self, maximum_samples: usize) -> Result<Self, String> {
        if self.failed || self.next_stage != 0 || self.run.mixed {
            return Err("Fitting allowance must precede the object workflow".into());
        }
        self.run.fitter = self.run.fitter.with_sample_limit(maximum_samples)?;
        Ok(self)
    }
    pub(crate) fn with_prefab_options(mut self, options: Option<&'a crate::recipe_options::Options>) -> Self {
        self.run.prefab_options = options;
        self
    }
    pub(crate) fn with_utilities(
        mut self,
        source: &'a Source,
        settings: Settings,
    ) -> Result<Option<Self>, String> {
        if self.failed
            || self.run.mixed
            || self.next_stage != 0
            || self.utility.is_some()
            || self.run.transaction.is_none()
            || self.run.path.source.path_family != source::PathFamily::Road
            || source.path_family != source::PathFamily::Powerline
            || source.infrastructure_sha256 != self.run.path.source.infrastructure_sha256
            || source.workflow != self.run.path.source.workflow
        {
            return Err("Invalid utility workflow binding".into());
        }
        let Some(state) = PathState::prepare(
            source,
            settings,
            self.run.path.out.terrain_sha256.clone(),
            true,
            &self.run.cancelled,
        )?
        else {
            return Ok(None);
        };
        self.utility = Some(UtilityPaths {
            state,
            roads: vec![],
            curves: vec![],
            layout_ready: false,
            next_stage: 0,
        });
        Ok(Some(self))
    }
    pub(crate) fn set_utility_paths(&mut self, roads: Vec<Road>) -> Result<Option<()>, String> {
        if self.failed {
            return Err("Object emitter cannot continue after failure".into());
        }
        if self.run.is_cancelled() {
            return Ok(None);
        }
        if !self.run.mixed
            || self
                .utility
                .as_ref()
                .is_none_or(|utility| utility.layout_ready || utility.next_stage != 0)
        {
            self.failed = true;
            return Err("Utility layout is missing its workflow or was already supplied".into());
        }
        let curves =
            match prepare_curves(&roads, source::PathFamily::Powerline, &self.run.cancelled) {
                Ok(Some(curves)) => curves,
                Ok(None) => {
                    self.run.cancelled_seen = true;
                    return Ok(None);
                }
                Err(error) => {
                    self.failed = true;
                    return Err(error);
                }
            };
        let utility = self.utility.as_mut().ok_or("Missing utility workflow")?;
        utility.roads = roads;
        utility.curves = curves;
        utility.layout_ready = true;
        Ok(Some(()))
    }
    pub(crate) fn emit_utility_stage(&mut self, index: usize) -> Result<Option<()>, String> {
        if self.failed {
            return Err("Object emitter cannot continue after failure".into());
        }
        if self.run.is_cancelled() {
            return Ok(None);
        }
        let valid = self.utility.as_ref().is_some_and(|utility| {
            self.run.mixed
                && utility.layout_ready
                && index == utility.next_stage
                && index < utility.state.source.stages.len()
        });
        if !valid {
            self.failed = true;
            return Err("Utility stage is out of order or lacks its generated layout".into());
        }
        let mut utility = self.utility.take().ok_or("Missing utility workflow")?;
        let stage = &utility.state.source.stages[index];
        let order = stage.scene_order;
        if order.is_none()
            || self
                .last_path_order
                .zip(order)
                .is_some_and(|(last, current)| current <= last)
            || (utility.state.out.budget_exhausted
                && stage.enabled()
                && stage.mode as u32 & self.run.realm_mask != 0)
        {
            self.failed = true;
            self.utility = Some(utility);
            return Err("Utility stage violates source order or its fitting allowance".into());
        }
        std::mem::swap(&mut self.run.path, &mut utility.state);
        let result = Self::process_stage(&mut self.run, index, &utility.roads, &utility.curves);
        std::mem::swap(&mut self.run.path, &mut utility.state);
        match &result {
            Ok(Some(())) => {
                utility.next_stage += 1;
                self.last_path_order = order;
            }
            Ok(None) => self.run.cancelled_seen = true,
            Err(_) => self.failed = true,
        }
        self.utility = Some(utility);
        result
    }
    pub(crate) fn begin_workflow(
        &mut self,
        decoration_stages: usize,
        cliff_stages: usize,
        coastal_stages: usize,
        realm_mask: u32,
    ) -> Result<(), String> {
        if self.failed
            || self.run.transaction.is_none()
            || self.next_stage != 0
            || !self.run.path.out.provisional_poses.is_empty()
            || self.run.mixed
            || !(1..=128).contains(&decoration_stages)
            || cliff_stages > 16
            || coastal_stages > 16
            || realm_mask == 0
        {
            self.failed = true;
            return Err("Invalid mixed object workflow initialization".into());
        }
        self.expected_decor = Some(decoration_stages);
        self.expected_cliffs = cliff_stages;
        self.expected_coastal = coastal_stages;
        self.run.mixed = true;
        self.run.realm_mask = realm_mask;
        self.run.path.out.engine = WORKFLOW_VERSION;
        if let Some(utility) = &mut self.utility {
            utility.state.out.engine = WORKFLOW_VERSION;
        }
        Ok(())
    }
    pub(crate) fn use_scope(
        &mut self,
        scope: papyrus_terrain::footprints::ProtectionScope,
    ) -> Result<(), String> {
        if self.failed || !self.run.mixed {
            self.failed = true;
            return Err("Mixed object workflow is not ready".into());
        }
        let result = self
            .run
            .transaction
            .as_mut()
            .ok_or("Missing shared object destination")?
            .set_scope(scope);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    pub(crate) fn apply_cliff_topology(
        &mut self,
        policy: papyrus_terrain::cliff_topology::Policy,
        keep_existing: bool,
    ) -> Result<Option<papyrus_terrain::cliff_topology::Report>, String> {
        if self.failed || !self.run.mixed {
            self.failed = true;
            return Err("Mixed object workflow is not ready for cliff topology".into());
        }
        let result = self
            .run
            .transaction
            .as_mut()
            .ok_or("Missing shared object destination")?
            .apply_cliff_topology(
                policy,
                keep_existing,
                crate::cliff_topology_stage::MAXIMUM_WORK,
                &self.run.cancelled,
            );
        if !matches!(result, Ok(Some(_))) {
            self.failed = true;
        }
        result
    }
    pub(crate) fn emit_decoration(
        &mut self,
        request: DecorationStage<'_, 'a>,
    ) -> Result<Option<papyrus_terrain::decor_live::Report>, String> {
        if self.failed
            || !self.run.mixed
            || request.index != self.next_decor
            || self.expected_decor.is_none_or(|n| request.index >= n)
        {
            self.failed = true;
            return Err("Invalid or reordered mixed decoration stage".into());
        }
        if self.run.is_cancelled() {
            self.run.cancelled_seen = true;
            return Ok(None);
        }
        let result = papyrus_terrain::decor_live::emit(
            request.stage,
            request.index,
            request.seed,
            request.realm,
            papyrus_terrain::decor_live::Context {
                transaction: self
                    .run
                    .transaction
                    .as_mut()
                    .ok_or("Missing shared object destination")?,
                alpha: self.run.alpha,
                fitting: &mut self.run.fitter,
                prefabs: request.prefabs,
                attempts: request.attempts,
                next_pose: &mut self.run.next_admission,
                water: request.water,
            },
            &self.run.cancelled,
        );
        match &result {
            Ok(Some(_)) => self.next_decor += 1,
            Ok(None) => self.run.cancelled_seen = true,
            Err(_) => self.failed = true,
        }
        result
    }
    pub(crate) fn emit_cliffs(
        &mut self,
        request: DecorationStage<'_, 'a>,
        priorities: &BTreeMap<String, i32>,
    ) -> Result<Option<papyrus_terrain::decor_live::Report>, String> {
        if self.failed
            || !self.run.mixed
            || request.index != self.next_cliff
            || request.index >= self.expected_cliffs
        {
            self.failed = true;
            return Err("Invalid or reordered mixed hill-cliff stage".into());
        }
        if self.run.is_cancelled() {
            self.run.cancelled_seen = true;
            return Ok(None);
        }
        let mut context = Some(papyrus_terrain::decor_live::Context {
                transaction: self
                    .run
                    .transaction
                    .as_mut()
                    .ok_or("Missing shared object destination")?,
                alpha: self.run.alpha,
                fitting: &mut self.run.fitter,
                prefabs: request.prefabs,
                attempts: request.attempts,
                next_pose: &mut self.run.next_admission,
                water: request.water,
        });
        let cancelled = &self.run.cancelled;
        let mut run = |pool: Option<&rayon::ThreadPool>| {
            papyrus_terrain::uniform_cliffs::emit_with_cpu_pool(
                request.stage,
                priorities,
                request.seed,
                request.realm,
                context.take().expect("One cliff stage per compute job"),
                pool,
                cancelled,
            )
        };
        let result = match crate::generation_resources::try_with_cpu_pool(|pool| run(Some(pool))) {
            Ok(Some(result)) => Ok(result),
            Ok(None) => run(None),
            Err(error) => Err(error),
        };
        match &result {
            Ok(Some(_)) => self.next_cliff += 1,
            Ok(None) => self.run.cancelled_seen = true,
            Err(_) => self.failed = true,
        }
        result
    }
    pub(crate) fn emit_coastal(
        &mut self,
        request: DecorationStage<'_, 'a>,
        program: &papyrus_terrain::coastal_cliffs::Program,
        placements: &BTreeMap<String, papyrus_terrain::coastal_cliffs::Prefab>,
    ) -> Result<Option<papyrus_terrain::decor_live::Report>, String> {
        if self.failed
            || !self.run.mixed
            || request.index != self.next_coastal
            || request.index >= self.expected_coastal
        {
            self.failed = true;
            return Err("Invalid or reordered mixed coastal-cliff stage".into());
        }
        if self.run.is_cancelled() {
            self.run.cancelled_seen = true;
            return Ok(None);
        }
        let result = papyrus_terrain::coastal_cliffs::emit(
            program,
            placements,
            request.seed,
            request.realm,
            papyrus_terrain::decor_live::Context {
                transaction: self
                    .run
                    .transaction
                    .as_mut()
                    .ok_or("Missing shared object destination")?,
                alpha: self.run.alpha,
                fitting: &mut self.run.fitter,
                prefabs: request.prefabs,
                attempts: request.attempts,
                next_pose: &mut self.run.next_admission,
                water: request.water,
            },
            &self.run.cancelled,
        );
        match &result {
            Ok(Some(_)) => self.next_coastal += 1,
            Ok(None) => self.run.cancelled_seen = true,
            Err(_) => self.failed = true,
        }
        result
    }
    pub(crate) fn emit_stage(&mut self, index: usize) -> Result<Option<()>, String> {
        if self.failed {
            return Err("Road emitter cannot continue after failure".into());
        }
        if self.run.is_cancelled() {
            self.run.cancelled_seen = true;
            return Ok(None);
        }
        if index != self.next_stage || index >= self.run.path.source.stages.len() {
            self.failed = true;
            return Err("Road emitter stage is out of order or its budget is exhausted".into());
        }
        let stage = &self.run.path.source.stages[index];
        let order = stage.scene_order;
        if self.last_path_order.zip(order).is_some_and(|(last, current)| current <= last) {
            self.failed = true;
            return Err("Path-object stages violate their shared source order".into());
        }
        if self.run.path.out.budget_exhausted
            && stage.enabled()
            && (if self.run.mixed {
                stage.mode as u32 & self.run.realm_mask != 0
            } else {
                stage.mode == -1
            })
        {
            self.failed = true;
            return Err("Road emitter stage budget is exhausted".into());
        }
        let result = Self::process_stage(&mut self.run, index, self.roads, &self.curves);
        if self.run.cancelled_seen {
            return Ok(None);
        }
        match &result {
            Ok(Some(())) => {
                self.next_stage += 1;
                self.last_path_order = order.or(self.last_path_order);
            }
            Ok(None) => self.run.cancelled_seen = true,
            Err(_) => self.failed = true,
        }
        result
    }
    fn process_stage(run: &mut Runner<'a, F>, si: usize, roads: &[Road], curves: &[Path]) -> Result<Option<()>, String> {
        let stage = &run.path.source.stages[si];
        let policy = run.path.policy;
        let settings = run.path.settings;
        if !stage.enabled()
            || !(if run.mixed {
                stage.mode as u32 & run.realm_mask != 0
            } else {
                stage.mode == -1
            })
        {
            run.count("skipped_stage");
            return Ok(Some(()));
        }
        for (ri, road) in roads.iter().enumerate() {
            if run.is_cancelled() {
                return Ok(None);
            }
            if run.path.out.budget_exhausted {
                return Ok(Some(()));
            }
            if !run.tick() {
                return Ok(Some(()));
            }
            if run.path.source.path_family == source::PathFamily::Road
                && road.hierarchy >= policy.maximum_hierarchy_exclusive
            {
                run.count("skipped_road_hierarchy");
                continue;
            }
            let mut path = curves[ri].clone();
            let mut rng = policy
                .seed_domain
                .bytes()
                .chain(settings.seed.to_le_bytes())
                .chain(stage.object_id.to_le_bytes())
                .chain((ri as u64).to_le_bytes())
                .fold(0xcbf29ce484222325_u64, |h, b| {
                    (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
                });
            for (end, enabled, rules) in [
                (false, road.start, &stage.start),
                (true, road.end, &stage.end),
            ] {
                if !enabled {
                    continue;
                }
                for (i, rule) in rules.iter().enumerate() {
                    if rule.folder.is_empty() || rule.prefabs.is_empty() {
                        continue;
                    }
                    let l = Location {
                        stage: si, road: ri,
                        group: if end { Group::End } else { Group::Start }, rule: i,
                    };
                    let resources = run.resources(&rule.prefabs, l)?;
                    if resources.paths().is_empty() {
                        continue;
                    }
                    let (lo, hi) = path.range();
                    let count =
                        (path.points().len() as f32 * policy.trim_fraction).floor() as usize;
                    for offset in 0..count {
                        if run.is_cancelled() {
                            return Ok(None);
                        }
                        if !run.tick() {
                            break;
                        }
                        let target = if end {
                            hi.checked_sub(offset).filter(|i| *i > lo)
                        } else {
                            lo.checked_add(offset).filter(|i| *i < hi)
                        };
                        let Some(target) = target else {
                            break;
                        };
                        let range = if end { (lo, target) } else { (target, hi) };
                        path.set_range(range.0, range.1)
                            .map_err(|e| format!("Road trim: {e:?}"))?;
                        if run.endpoint(rule, &resources, road, &path, end, true, l, &mut rng)? {
                            break;
                        }
                        path.set_range(lo, hi)
                            .map_err(|e| format!("Road trim reset: {e:?}"))?;
                        if run.path.out.budget_exhausted {
                            break;
                        }
                    }
                }
            }
            run.path.out.trims.push(Trim {
                stage: si,
                road: ri,
                min_index: path.range().0,
                max_index: path.range().1,
            });
            for (group, enabled, rules, end) in [
                (Group::Start, road.start, &stage.start, false),
                (Group::End, road.end, &stage.end, true),
            ] {
                if enabled {
                    for (i, rule) in rules.iter().enumerate() {
                        let l = Location {
                            stage: si,
                            road: ri,
                            group,
                            rule: i,
                        };
                        let resources = run.resources(&rule.prefabs, l)?;
                        run.endpoint(rule, &resources, road, &path, end, false, l, &mut rng)?;
                        if run.path.out.budget_exhausted {
                            break;
                        }
                    }
                }
            }
            for (i, rule) in stage.path.iter().enumerate() {
                if run.path.out.budget_exhausted {
                    break;
                }
                run.along(
                    rule,
                    &path,
                    Location {
                        stage: si,
                        road: ri,
                        group: Group::Path,
                        rule: i,
                    },
                    &mut rng,
                )?;
            }
            for (i, rule) in stage.side.iter().enumerate() {
                if run.path.out.budget_exhausted {
                    break;
                }
                run.side(
                    rule,
                    road,
                    &path,
                    Location {
                        stage: si,
                        road: ri,
                        group: Group::Side,
                        rule: i,
                    },
                    &mut rng,
                )?;
            }
        }
        Ok(Some(()))
    }
    pub(crate) fn finish(
        self,
    ) -> Result<Option<(Report, Option<papyrus_terrain::footprints::Output>)>, String> {
        if self.failed {
            return Err("Road emitter cannot publish after failure".into());
        }
        if self.utility.as_ref().is_some_and(|utility| !utility.layout_ready
            || utility.next_stage != utility.state.source.stages.len()) {
            return Err("Object emitter cannot publish incomplete utility stages".into());
        }
        if self.utility.as_ref().is_some_and(|utility| utility.state.out.budget_exhausted) {
            return Err("Live utility-object fitting exhausted its work allowance".into());
        }
        let mut utility = self.utility;
        let mut run = self.run;
        if run.is_cancelled() {
            return Ok(None);
        }
        if self.expected_decor.is_some_and(|n| self.next_decor != n)
            || self.next_cliff != self.expected_cliffs
            || self.next_coastal != self.expected_coastal
        {
            return Err("Mixed object emitter cannot publish incomplete decoration".into());
        }
        if self.next_stage != run.path.source.stages.len()
            && (!run.path.out.budget_exhausted || run.transaction.is_some())
        {
            return Err("Road emitter cannot publish before every source stage completes".into());
        }
        run.path.out.surface_samples = run.fitter.samples();
        run.path.out.height_samples = run.fitter.height_samples();
        if run.mixed {
            run.path.out.global_admission_ids = Some(run.path.admission_ids);
            run.path.out.global_admission_count = Some(run.next_admission);
        }
        let terrain = if let Some(transaction) = run.transaction {
            if run.path.out.budget_exhausted {
                return Err("Live road-object fitting exhausted its work allowance".into());
            }
            let Some(output) = transaction.finish(&run.cancelled)? else {
                return Ok(None);
            };
            Some(output)
        } else {
            None
        };
        run.path.out.terrain_applied = terrain.is_some();
        if let Some(utility) = &mut utility {
            utility.state.out.surface_samples = run.fitter.samples();
            utility.state.out.height_samples = run.fitter.height_samples();
            utility.state.out.global_admission_count = Some(run.next_admission);
            utility.state.out.terrain_applied = terrain.is_some();
        }
        if let Some(mut utility) = utility {
            utility.state.out.global_admission_ids = Some(utility.state.admission_ids);
            run.path.out.utilities = Some(Box::new(utility.state.out));
        }
        Ok(Some((run.path.out, terrain)))
    }
}

/// Decode the exact receipted road report. End markers follow authored monument
/// connections; network joins and loops do not acquire fabricated endpoint flags.
pub fn roads_from_report(
    value: &serde_json::Value,
    default_hierarchy: u32,
    default_width: f32,
) -> Result<Vec<Road>, String> {
    if default_hierarchy > 16 || !default_width.is_finite() || !(1. ..=32.).contains(&default_width)
    {
        return Err("Invalid source road defaults".into());
    }
    let rows = value["roads"]
        .as_array()
        .filter(|v| v.len() <= 4096)
        .ok_or("Missing bounded road list")?;
    let mut out = Vec::with_capacity(rows.len());
    for (road_index, r) in rows.iter().enumerate() {
        let points: Vec<V> = serde_json::from_value(r["points"].clone())
            .map_err(|e| format!("Invalid road controls: {e}"))?;
        let endpoint = |v: &serde_json::Value, side: usize| -> Result<bool, String> {
            if crate::source_road_handoff::raw_json_endpoint(value, road_index, side, v)? {
                return Ok(false);
            }
            let object = v.as_object().ok_or("Invalid road destination")?;
            if object.len() == 1 && v["loop"] == true {
                return Ok(false);
            }
            if object.len() == 2
                && v["monument"].as_u64().is_some_and(|id| id > 0)
                && v["connector"].as_i64().is_some_and(|id| id != 0)
            {
                return Ok(true);
            }
            if object.len() == 2
                && let Some(road) = v["network_road"].as_u64()
                && let Some(point) = v["network_point"].as_u64()
                && rows
                    .get(road as usize)
                    .and_then(|r| r["points"].as_array())
                    .is_some_and(|p| point < p.len() as u64)
            {
                return Ok(false);
            }
            Err("Unsupported or dangling road destination".into())
        };
        out.push(Road {
            hierarchy: default_hierarchy,
            width: default_width,
            start: endpoint(&r["from"], 0)?,
            end: endpoint(&r["to"], 1)?,
            points,
        });
    }
    if let Some(families) = value.get("families") {
        let families = families
            .as_array()
            .filter(|v| v.len() <= 64)
            .ok_or("Invalid road families")?;
        let mut covered = vec![false; out.len()];
        for f in families {
            let first = f["first_road"]
                .as_u64()
                .filter(|v| *v <= out.len() as u64)
                .ok_or("Invalid first road")? as usize;
            let count = f["road_count"]
                .as_u64()
                .filter(|v| *v <= (out.len() - first) as u64)
                .ok_or("Invalid road family range")? as usize;
            let hierarchy = f["hierarchy"]
                .as_u64()
                .filter(|v| *v <= 16)
                .ok_or("Invalid road hierarchy")? as u32;
            let width = f["width"]
                .as_f64()
                .filter(|v| v.is_finite() && (1. ..=32.).contains(v))
                .ok_or("Invalid road width")? as f32;
            for i in first..first + count {
                if covered[i] {
                    return Err("Overlapping road families".into());
                }
                covered[i] = true;
                out[i].hierarchy = hierarchy;
                out[i].width = width;
            }
        }
        if covered.iter().any(|v| !*v) {
            return Err("Road families do not cover every road".into());
        }
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    #[test]
    fn mixed_completion_barrier_realm_and_cancel_latches_are_enforced() {
        let layers = layers();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let roads = [road()];
        let mut data = source_value();
        data["stages"][0]["mode"] = 2.into();
        let source = source(data);
        let flags = BTreeMap::from([
            ("Field".into(), 1),
            ("Decor".into(), 2),
            ("Monument".into(), 8),
        ]);
        let spec = decor_source::compile_road_effects(&source.prefabs[A], &flags, 8).unwrap();
        let tx = || {
            papyrus_terrain::footprints::RoadTransaction::new_objects(
                &layers,
                &alpha,
                BTreeMap::from([(A.into(), spec.clone())]),
                11,
                8,
                || false,
            )
            .unwrap()
            .unwrap()
        };
        let new = || {
            Emitter::new(
                &layers,
                &alpha,
                &roads,
                &source,
                settings(),
                Some(tx()),
                || false,
            )
            .unwrap()
            .unwrap()
        };
        let mut incomplete = new();
        incomplete.begin_workflow(1, 0, 0, 2).unwrap();
        incomplete.emit_stage(0).unwrap().unwrap();
        assert!(!incomplete.run.path.out.provisional_poses.is_empty());
        assert!(incomplete.finish().is_err());
        let stage=serde_json::from_value(serde_json::json!({"id":"20","order":0,"name":"Disabled","kind":"uniform","folder":"decor/test","enabled":false,"mode":-1,"filter":filter(),"filter_mode":1,"anchor_mode":0,"distance":250.,"dithering":0.,"density":0.,"cluster":null,"prefabs":[]})).unwrap();
        let prefabs = BTreeMap::new();
        let mut attempts = papyrus_terrain::decor_stream::Budget::new(1000).unwrap();
        macro_rules! request {
            ($index:expr) => {
                DecorationStage {
                    stage: &stage,
                    index: $index,
                    seed: 42,
                    realm: 2,
                    prefabs: &prefabs,
                    attempts: &mut attempts,
                    water: None,
                }
            };
        }
        let mut complete = new();
        complete.begin_workflow(1, 0, 0, 2).unwrap();
        complete.emit_stage(0).unwrap().unwrap();
        complete.emit_decoration(request!(0)).unwrap().unwrap();
        assert!(complete.finish().unwrap().is_some());
        let mut bad = new();
        bad.begin_workflow(1, 0, 0, 2).unwrap();
        assert!(bad.emit_decoration(request!(1)).is_err());
        assert!(bad.finish().is_err());
        let cancel = std::cell::Cell::new(false);
        let mut cancelled = Emitter::new(
            &layers,
            &alpha,
            &roads,
            &source,
            settings(),
            Some(tx()),
            || cancel.get(),
        )
        .unwrap()
        .unwrap();
        cancelled.begin_workflow(1, 0, 0, 2).unwrap();
        cancelled.emit_stage(0).unwrap().unwrap();
        cancel.set(true);
        assert!(cancelled.emit_decoration(request!(0)).unwrap().is_none());
        cancel.set(false);
        assert!(cancelled.finish().unwrap().is_none());
        let active = std::cell::Cell::new(false);
        let polls = std::cell::Cell::new(0);
        let mut pulsed = Emitter::new(
            &layers,
            &alpha,
            &roads,
            &source,
            settings(),
            Some(tx()),
            || {
                if !active.get() {
                    return false;
                }
                polls.set(polls.get() + 1);
                polls.get() == 7
            },
        )
        .unwrap()
        .unwrap();
        pulsed.begin_workflow(1, 0, 0, 2).unwrap();
        active.set(true);
        assert!(pulsed.emit_stage(0).unwrap().is_none());
        active.set(false);
        assert!(pulsed.finish().unwrap().is_none());
    }
    pub(crate) fn mixed_fixture() -> (
        Source,
        Report,
        Vec<papyrus_terrain::decor_live::Report>,
        papyrus_terrain::footprints::Output,
    ) {
        use papyrus_terrain::{
            decor_fit::Prefab,
            decor_topology,
            footprints::{ProtectionScope, RoadTransaction},
        };
        let layers = layers();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let roads = [road()];
        let mut data = source_value();
        let mut second = data["stages"][0].clone();
        second["order"] = 1.into();
        second["object_id"] = 2.into();
        second["side"][0]["offset"] = 20.into();
        data["stages"].as_array_mut().unwrap().push(second);
        data["prefabs"][A]["attributes"] = serde_json::json!([
            {"kind":"TerrainHeightAdd","root":true,"position":[0.,0.,0.],"fields":{"Opacity":1.,"Radius":3.,"Fade":0.,"Delta":2.}},
            {"kind":"TerrainPathConnect","root":true,"position":[2.,0.,0.],"fields":{"Type":1}}
        ]);
        let source = source(data);
        let flags = BTreeMap::from([
            ("Field".into(), 1),
            ("Decor".into(), 2),
            ("Monument".into(), 8),
        ]);
        let road_spec = decor_source::compile_road_effects(&source.prefabs[A], &flags, 8).unwrap();
        let mut decor_spec = road_spec.clone();
        decor_spec.height_modifiers = Some(vec![]);
        decor_spec
            .topology
            .as_mut()
            .unwrap()
            .modifiers
            .push(decor_topology::Modifier {
                position: [0.; 3],
                radius: 4.,
                fade: 0.,
                mask: 2,
                operation: decor_topology::Operation::Add,
            });
        let tx = RoadTransaction::new_objects(
            &layers,
            &alpha,
            BTreeMap::from([
                (format!("road:{A}"), road_spec),
                (format!("decoration:{A}"), decor_spec),
            ]),
            11,
            8,
            || false,
        )
        .unwrap()
        .unwrap();
        let mut emitter = Emitter::new(
            &layers,
            &alpha,
            &roads,
            &source,
            settings(),
            Some(tx),
            || false,
        )
        .unwrap()
        .unwrap();
        emitter.begin_workflow(1, 0, 0, 2).unwrap();
        let scope = |namespace: &str| ProtectionScope {
            namespace: namespace.into(),
            protected_topology: 8,
            role_exceptions: 0,
            material_role_exceptions: 0,
            maximum_height_change: None,
        };
        emitter.use_scope(scope("road")).unwrap();
        emitter.emit_stage(0).unwrap().unwrap();
        let before = emitter.progress();
        let address = emitter.run.view().height.as_ptr();
        let stage=serde_json::from_value(serde_json::json!({"id":"20","order":0,"name":"Rocks","kind":"uniform","folder":"decor/test","enabled":true,"mode":-1,"filter":filter(),"filter_mode":1,"anchor_mode":0,"distance":250.,"dithering":0.,"density":0.,"cluster":null,"prefabs":[A]})).unwrap();
        let prefabs = BTreeMap::from([(
            A.into(),
            PreparedPrefab::new(Prefab {
                rotation: [0., 0., 0., 1.],
                scale: [1.; 3],
                transforms: vec![],
                anchors: vec![],
                filters: vec![],
                unsupported: vec![],
                pending: vec![],
            })
            .unwrap(),
        )]);
        let mut attempts = papyrus_terrain::decor_stream::Budget::new(1000).unwrap();
        emitter.use_scope(scope("decoration")).unwrap();
        let report = emitter
            .emit_decoration(DecorationStage {
                stage: &stage,
                index: 0,
                seed: 42,
                realm: 2,
                prefabs: &prefabs,
                attempts: &mut attempts,
                water: None,
            })
            .unwrap()
            .unwrap();
        assert!(!report.admitted.is_empty());
        assert!(emitter.progress().admissions > before.admissions);
        assert!(emitter.progress().terrain_work > before.terrain_work);
        assert_eq!(emitter.run.view().height.as_ptr(), address);
        emitter.use_scope(scope("road")).unwrap();
        emitter.emit_stage(1).unwrap().unwrap();
        let (fitting, terrain) = emitter.finish().unwrap().unwrap();
        let terrain = terrain.unwrap();
        let ids = fitting.global_admission_ids.as_ref().unwrap();
        assert!(ids.windows(2).any(|p| p[1] > p[0] + 1));
        assert!(
            report
                .admitted
                .iter()
                .all(|p| !ids.contains(&p.transaction_pose))
        );
        assert_eq!(
            fitting.global_admission_count,
            Some(terrain.report.counts.attempted)
        );
        assert!(layers.height.iter().all(|v| *v == 18000));
        (source, fitting, vec![report], terrain)
    }
    #[test]
    fn mixed_emitter_interleaves_with_shared_ids_views_budgets_and_repeatable_effects() {
        let (_, a, d, terrain) = mixed_fixture();
        let (_, b, e, replay) = mixed_fixture();
        assert_eq!(
            serde_json::to_value(a).unwrap(),
            serde_json::to_value(b).unwrap()
        );
        assert_eq!(
            serde_json::to_value(d).unwrap(),
            serde_json::to_value(e).unwrap()
        );
        assert_eq!(terrain.height, replay.height);
        assert_eq!(terrain.topology, replay.topology);
        assert_eq!(terrain.splat, replay.splat);
    }
    #[test]
    fn readonly_budget_reports_still_account_for_following_disabled_source_stages() {
        let mut v = source_value();
        let mut disabled = v["stages"][0].clone();
        disabled["order"] = 1.into();
        disabled["object_id"] = 2.into();
        disabled["disabled"] = true.into();
        v["stages"].as_array_mut().unwrap().push(disabled);
        let source = source(v);
        let layers = layers();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let report = fit(
            &layers,
            &alpha,
            &[road()],
            &source,
            Settings {
                seed: 42,
                maximum_work: 1,
                maximum_poses: 1000,
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(report.budget_exhausted);
        assert_eq!(report.counts["skipped_stage"], 1);
    }
    fn staged_source() -> Source {
        let mut v = source_value();
        let mut second = v["stages"][0].clone();
        second["order"] = 1.into();
        second["object_id"] = 2.into();
        second["name"] = "Second road stage".into();
        v["stages"].as_array_mut().unwrap().push(second);
        v["prefabs"][A]["attributes"] = serde_json::json!([
            {"kind":"TerrainAnchor","root":true,"position":[0.,0.,0.],"fields":{"Extents":2.,"Offset":0.,"Radius":0.,"SlopeScale":0.}},
            {"kind":"TerrainHeightAdd","root":true,"position":[0.,0.,0.],"fields":{"Opacity":1.,"Radius":3.,"Fade":0.,"Delta":2.}}
        ]);
        source(v)
    }
    fn transaction<'a>(
        layers: &'a Layers,
        alpha: &'a [u8],
        source: &Source,
    ) -> papyrus_terrain::footprints::RoadTransaction<'a> {
        let flags = BTreeMap::from([
            ("Field".into(), 1),
            ("Building".into(), 2),
            ("Monument".into(), 8),
        ]);
        let prefabs = source
            .prefabs
            .iter()
            .map(|(path, p)| {
                (
                    path.clone(),
                    decor_source::compile_road_effects(p, &flags, 8).unwrap(),
                )
            })
            .collect();
        papyrus_terrain::footprints::RoadTransaction::new(layers, alpha, prefabs, 11, 8, || false)
            .unwrap()
            .unwrap()
    }
    #[test]
    fn paused_emitter_preserves_private_terrain_occupancy_and_budgets_and_matches_continuous_output()
     {
        let layers = layers();
        let source = staged_source();
        let roads = [road()];
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let settings = Settings {
            seed: 42,
            maximum_work: 100000,
            maximum_poses: 1000,
        };
        let mut emitter = Emitter::new(
            &layers,
            &alpha,
            &roads,
            &source,
            settings,
            Some(transaction(&layers, &alpha, &source)),
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(emitter.emit_stage(0).unwrap().is_some());
        let work = emitter.run.path.out.work;
        let samples = emitter.run.fitter.samples();
        assert!(work > 0 && samples > 0);
        let view = emitter.run.view();
        assert_ne!(view.height, &layers.height);
        let buffer = view.height.as_ptr();
        let first = emitter.run.path.out.provisional_poses[0].transform.position;
        let n = layers.height_resolution;
        let index =
            |p: f32| ((p / layers.size as f32 + 0.5) * n as f32).min((n - 1) as f32) as usize;
        let cell = index(first[2]) * n + index(first[0]);
        let saved = view.height[cell];
        assert!(emitter.emit_stage(1).unwrap().is_some());
        assert_eq!(buffer, emitter.run.view().height.as_ptr());
        assert_eq!(saved, emitter.run.view().height[cell]);
        assert!(emitter.run.path.out.work > work && emitter.run.fitter.samples() > samples);
        let (paused, terrain) = emitter.finish().unwrap().unwrap();
        let terrain = terrain.unwrap();
        assert!(terrain.report.counts.occupied > 0);
        let (continuous, expected) = fit_incremental(
            &layers,
            &alpha,
            &roads,
            &source,
            settings,
            Some(transaction(&layers, &alpha, &source)),
            || false,
        )
        .unwrap()
        .unwrap();
        let expected = expected.unwrap();
        assert_eq!(
            serde_json::to_value(paused).unwrap(),
            serde_json::to_value(continuous).unwrap()
        );
        assert_eq!(terrain.height, expected.height);
        assert_eq!(
            serde_json::to_value(terrain.report).unwrap(),
            serde_json::to_value(expected.report).unwrap()
        );
        assert!(layers.height.iter().all(|v| *v == 18000));
    }
    #[test]
    fn paused_emitter_cannot_publish_incomplete_reordered_cancelled_or_budget_limited_work() {
        let layers = layers();
        let source = staged_source();
        let roads = [road()];
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let settings = Settings {
            seed: 42,
            maximum_work: 100000,
            maximum_poses: 1000,
        };
        let new = || {
            Emitter::new(
                &layers,
                &alpha,
                &roads,
                &source,
                settings,
                Some(transaction(&layers, &alpha, &source)),
                || false,
            )
            .unwrap()
            .unwrap()
        };
        let mut emitter = new();
        emitter.emit_stage(0).unwrap();
        assert!(emitter.finish().is_err());
        let mut emitter = new();
        assert!(emitter.emit_stage(1).is_err());
        assert!(emitter.finish().is_err());
        let mut emitter = new();
        emitter.emit_stage(0).unwrap();
        assert!(emitter.emit_stage(0).is_err());
        assert!(emitter.finish().is_err());
        let cancel = std::cell::Cell::new(false);
        let mut emitter = Emitter::new(
            &layers,
            &alpha,
            &roads,
            &source,
            settings,
            Some(transaction(&layers, &alpha, &source)),
            || cancel.get(),
        )
        .unwrap()
        .unwrap();
        emitter.emit_stage(0).unwrap();
        cancel.set(true);
        assert!(emitter.emit_stage(1).unwrap().is_none());
        cancel.set(false);
        assert!(emitter.finish().unwrap().is_none());
        let mut emitter = new();
        emitter.emit_stage(0).unwrap();
        emitter.run.path.out.work = settings.maximum_work;
        emitter.emit_stage(1).unwrap();
        assert!(emitter.finish().is_err());
        assert!(layers.height.iter().all(|v| *v == 18000));
    }
    const A: &str = "assets/bundled/prefabs/autospawn/decor/test/a.prefab";
    const B: &str = "assets/bundled/prefabs/autospawn/decor/test/b.prefab";
    fn filter() -> serde_json::Value {
        serde_json::json!({"SplatType":-1,"BiomeType":-1,"TopologyAny":-1,"TopologyAll":0,"TopologyNot":0})
    }
    fn side() -> serde_json::Value {
        serde_json::json!({"folder":"decor/test","filter":filter(),"prefabs":[A],"alignment":3,"density":1.,"distance":25.,"side":3,"offset":0.})
    }
    fn basic() -> serde_json::Value {
        serde_json::json!({"folder":"decor/test","filter":filter(),"prefabs":[A],"placement":0,"align_to_normal":false,"height_to_terrain":true,"offset":0.})
    }
    fn stage() -> serde_json::Value {
        serde_json::json!({"object_id":1,"order":0,"name":"Objects","mode":-1,"disabled":false,"component_enabled":true,"hierarchy_active":true,"start":[],"end":[],"side":[side()],"path":[]})
    }
    fn prefab() -> serde_json::Value {
        serde_json::json!({"rotation":[0.,0.70710677,0.,0.70710677],"scale":[1.,1.,1.],"attributes":[],"components":{},"issues":[]})
    }
    fn anchor(offset: f32) -> serde_json::Value {
        serde_json::json!({"kind":"TerrainAnchor","root":true,"position":[0.,0.,0.],"fields":{"Extents":1.,"Offset":offset,"Radius":0.,"SlopeScale":0.}})
    }
    pub(crate) fn source_value() -> serde_json::Value {
        let profile: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/road-objects/client-0f703d43.json"
        ))
        .unwrap();
        serde_json::json!({"format":"papyrus-road-object-source","schema":1,"adapter":source::VERSION,"infrastructure_sha256":"a".repeat(64),"sources":[],"extractor_sha256":"b".repeat(64),"stages":[stage()],"prefabs":{A:prefab()},"issues":[],"path_policy":profile["policy"],"placement_profile_sha256":"c".repeat(64)})
    }
    pub(crate) fn source(doc: serde_json::Value) -> Source {
        Source::parse(
            &serde_json::to_vec(&doc).unwrap(),
            &"a".repeat(64),
            &[],
            || false,
        )
        .unwrap()
    }
    pub(crate) fn layers() -> Layers {
        let (hr, n) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let mut splat = vec![0; n * n * 8];
        splat[..n * n].fill(255);
        let mut biome = vec![0; n * n * 5];
        biome[..n * n].fill(255);
        Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: n,
            height: vec![18000; hr * hr],
            biome,
            splat,
            topology: vec![1; n * n],
            normalized_biome_cells: n * n,
            normalized_splat_cells: n * n,
        }
    }
    pub(crate) fn road() -> Road {
        Road {
            hierarchy: 1,
            width: 10.,
            start: true,
            end: true,
            points: (0..=100)
                .map(|i| {
                    [
                        -100. + i as f32 * 2.,
                        papyrus_terrain::erosion::decode(18000),
                        0.,
                    ]
                })
                .collect(),
        }
    }
    fn settings() -> Settings {
        Settings {
            seed: 42,
            maximum_work: 100_000,
            maximum_poses: 10000,
        }
    }
    pub(crate) fn run(l: &Layers, source: &Source) -> Report {
        fit(
            l,
            &vec![255; l.surface_resolution.pow(2)],
            &[road()],
            source,
            settings(),
            || false,
        )
        .unwrap()
        .unwrap()
    }
    #[test]
    fn side_modes_keep_inward_rotation_and_any_keeps_its_last_successful_side() {
        let l = layers();
        for (mode, count) in [(0, 14), (1, 7), (2, 7), (3, 7)] {
            let mut value = source_value();
            value["stages"][0]["side"][0]["side"] = mode.into();
            let r = run(&l, &source(value));
            assert_eq!(r.provisional_poses.len(), count);
            for pose in &r.provisional_poses {
                let p = pose.transform.position;
                assert!((p[2].abs() - 5.).abs() < 1e-4);
                if mode == 1 || mode == 3 {
                    assert!(p[2] > 0.);
                }
                if mode == 2 {
                    assert!(p[2] < 0.);
                }
                let f = decor_fit::rotate(pose.transform.rotation, [0., 0., 1.]);
                assert!(f[2] * -p[2] > 4.99);
                assert!(f[0].abs() < 1e-5);
            }
            assert!(!r.terrain_applied);
            assert_eq!(r.placed_instances, 0);
        }
    }
    #[test]
    fn density_misses_advance_spacing_and_filter_failures_can_switch_any_side() {
        let mut l = layers();
        let mut value = source_value();
        value["stages"][0]["side"][0]["density"] = 0.into();
        let r = run(&l, &source(value));
        assert!(r.provisional_poses.is_empty());
        assert_eq!(r.counts["density_miss"], 7);
        let n = l.surface_resolution;
        for z in 0..n {
            for x in 0..n {
                let wx = (x as f32 + 0.5) / n as f32 * 1000. - 500.;
                let wz = (z as f32 + 0.5) / n as f32 * 1000. - 500.;
                if wx > 0. && wx < 40. && wz > 0. {
                    l.topology[z * n + x] = 128;
                }
            }
        }
        let mut value = source_value();
        value["stages"][0]["side"][0]["filter"]["TopologyNot"] = 128.into();
        let r = run(&l, &source(value));
        assert_eq!(r.provisional_poses.len(), 7);
        let sides = r
            .provisional_poses
            .iter()
            .map(|p| p.transform.position[2] > 0.)
            .collect::<Vec<_>>();
        assert!(sides[0]);
        assert!(!sides[sides.len() - 1]);
        assert_eq!(sides.windows(2).filter(|v| v[0] != v[1]).count(), 1);
    }
    #[test]
    fn endpoint_trims_reset_between_stages_and_all_roster_anchors_are_checked() {
        let mut l = layers();
        let n = l.surface_resolution;
        for z in 0..n {
            for x in 0..n {
                let wx = (x as f32 + 0.5) / n as f32 * 1000. - 500.;
                if wx.abs() > 80. {
                    l.topology[z * n + x] = 128;
                }
            }
        }
        let mut value = source_value();
        let mut endpoints = stage();
        endpoints["side"] = serde_json::json!([]);
        let mut rule = basic();
        rule["filter"]["TopologyNot"] = 128.into();
        endpoints["start"] = serde_json::json!([rule.clone()]);
        endpoints["end"] = serde_json::json!([rule]);
        let mut second = stage();
        second["object_id"] = 2.into();
        second["order"] = 1.into();
        value["stages"] = serde_json::json!([endpoints, second]);
        let r = run(&l, &source(value.clone()));
        assert_eq!((r.trims[0].min_index, r.trims[0].max_index), (10, 90));
        assert_eq!((r.trims[1].min_index, r.trims[1].max_index), (0, 100));
        assert_eq!(
            r.provisional_poses.iter().filter(|p| p.stage == 0).count(),
            2
        );
        assert!(
            (r.provisional_poses
                .iter()
                .find(|p| p.stage == 1)
                .unwrap()
                .transform
                .position[0]
                + 75.)
                .abs()
                < 1e-4
        );
        value["prefabs"][B] = prefab();
        value["prefabs"][B]["attributes"] = serde_json::json!([anchor(0.), anchor(10.)]);
        value["prefabs"][B]["components"] = serde_json::json!({"TerrainAnchor":2});
        for group in ["start", "end"] {
            value["stages"][0][group][0]["prefabs"] = serde_json::json!([A, B]);
        }
        let r = run(&l, &source(value));
        assert_eq!((r.trims[0].min_index, r.trims[0].max_index), (0, 100));
        assert!(r.counts["fit_anchor"] > 0);
    }
    #[test]
    fn neighbor_alignment_defers_fitting_and_trails_budgets_and_cancellation_are_explicit() {
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let before = l.topology.clone();
        let mut value = source_value();
        value["stages"][0]["side"][0]["alignment"] = 1.into();
        let source = source(value);
        let r = run(&l, &source);
        assert_eq!(r.provisional_poses.len(), 7);
        assert_eq!(r.neighbor_sequences[0].requested, 7);
        assert_eq!(r.neighbor_sequences[0].poses.len(), 7);
        for p in &r.provisional_poses {
            let f = decor_fit::rotate(p.transform.rotation, [0., 0., 1.]);
            assert!(f[0] > 0.9999);
        }
        let mut trail = road();
        trail.hierarchy = 2;
        let r = fit(&l, &alpha, &[trail], &source, settings(), || false)
            .unwrap()
            .unwrap();
        assert!(r.provisional_poses.is_empty());
        assert_eq!(r.counts["skipped_road_hierarchy"], 1);
        let small = Settings {
            maximum_work: 3,
            ..settings()
        };
        let r = fit(&l, &alpha, &[road()], &source, small, || false)
            .unwrap()
            .unwrap();
        assert!(r.budget_exhausted);
        assert!(r.work <= 3);
        let one = Settings {
            maximum_poses: 1,
            ..settings()
        };
        let r = fit(&l, &alpha, &[road()], &source, one, || false)
            .unwrap()
            .unwrap();
        assert_eq!(r.provisional_poses.len(), 1);
        assert!(r.budget_exhausted);
        let calls = std::cell::Cell::new(0);
        assert!(
            fit(&l, &alpha, &[road()], &source, settings(), || {
                calls.set(calls.get() + 1);
                calls.get() > 15
            })
            .unwrap()
            .is_none()
        );
        assert_eq!(l.topology, before);
    }
    #[test]
    fn malformed_source_policy_and_road_references_do_not_get_silent_defaults() {
        let l = layers();
        let mut source = source(source_value());
        source.path_policy = None;
        assert!(
            fit(
                &l,
                &vec![255; l.surface_resolution.pow(2)],
                &[road()],
                &source,
                settings(),
                || false
            )
            .is_err()
        );
        let valid = serde_json::json!({"roads":[{"from":{"monument":1,"connector":1},"to":{"loop":true},"points":[[0.,1.,0.],[1.,1.,0.]]}],"families":[{"first_road":0,"road_count":1,"hierarchy":2,"width":4.}]});
        let r = roads_from_report(&valid, 1, 10.).unwrap();
        assert!(r[0].start);
        assert!(!r[0].end);
        assert_eq!(r[0].hierarchy, 2);
        assert_eq!(r[0].width, 4.);
        let mut bad = valid.clone();
        bad["roads"][0]["to"] = serde_json::json!({"network_road":2,"network_point":0});
        assert!(roads_from_report(&bad, 1, 10.).is_err());
        let mut bad = valid.clone();
        bad["families"][0]["road_count"] = 0.into();
        assert!(roads_from_report(&bad, 1, 10.).is_err());
        let mut bad = valid;
        let duplicate = bad["families"][0].clone();
        bad["families"].as_array_mut().unwrap().push(duplicate);
        assert!(roads_from_report(&bad, 1, 10.).is_err());
    }
    #[test]
    fn along_path_dithering_replays_and_neighbor_rotations_use_accepted_positions() {
        let l = layers();
        let mut value = source_value();
        value["stages"][0]["side"] = serde_json::json!([]);
        value["stages"][0]["path"] = serde_json::json!([{"folder":"decor/test","filter":filter(),"prefabs":[A],"alignment":2,"density":1.,"distance":25.,"dithering":3.}]);
        let src = source(value.clone());
        let a = run(&l, &src);
        let b = run(&l, &src);
        assert_eq!(
            serde_json::to_value(&a).unwrap(),
            serde_json::to_value(b).unwrap()
        );
        assert_eq!(a.provisional_poses.len(), 7);
        for (i, p) in a.provisional_poses.iter().enumerate() {
            let expected = -75. + 25. * i as f32;
            assert!((p.transform.position[0] - expected).abs() <= 3.);
            assert!(p.transform.position[2].abs() <= 3.);
            assert!(decor_fit::rotate(p.transform.rotation, [0., 0., 1.])[0] > 0.9999);
        }
        let changed = fit(
            &l,
            &vec![255; l.surface_resolution.pow(2)],
            &[road()],
            &src,
            Settings {
                seed: 43,
                ..settings()
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert_ne!(
            a.provisional_poses[0].transform.position,
            changed.provisional_poses[0].transform.position
        );
        value["stages"][0]["path"][0]["alignment"] = 1.into();
        let neighbor = run(&l, &source(value));
        assert_eq!(neighbor.neighbor_sequences[0].requested, 7);
        for (i, p) in neighbor.provisional_poses.iter().enumerate() {
            let before = neighbor.provisional_poses[i.saturating_sub(1)]
                .transform
                .position;
            let after = neighbor.provisional_poses
                [(i + 1).min(neighbor.provisional_poses.len() - 1)]
            .transform
            .position;
            let direction = sub(after, before);
            let expected = norm([direction[0], 0., direction[2]]);
            let actual = decor_fit::rotate(p.transform.rotation, [0., 0., 1.]);
            assert!(distance(actual, expected) < 1e-5);
        }
    }
    #[test]
    fn unimplemented_sequence_replacement_never_becomes_a_fabricated_fit() {
        let l = layers();
        let mut value = source_value();
        value["stages"][0]["side"][0]["alignment"] = 1.into();
        value["prefabs"][A]["components"] = serde_json::json!({"PathSequence":1});
        value["prefabs"][A]["attributes"] = serde_json::json!([{"kind":"PathSequence","root":true,"position":[0.,0.,0.],"fields":{}}]);
        let r = run(&l, &source(value));
        assert!(r.provisional_poses.is_empty());
        assert_eq!(r.counts["unsupported_path_sequence"], 7);
        assert_eq!(r.neighbor_sequences[0].requested, 7);
        assert!(r.neighbor_sequences[0].poses.is_empty());
    }
    fn powerline_source_value() -> serde_json::Value {
        let mut value = source_value();
        value["path_policy"]["powerline_sequence"] = serde_json::json!({
            "engine":powerline_sequence::VERSION,"regular_spacing":2
        });
        value["stages"][0]["side"][0]["alignment"] = 1.into();
        value["stages"][0]["side"][0]["prefabs"] = serde_json::json!([A,B]);
        for (path, rule) in [(A, 1), (B, 0)] {
            value["prefabs"][path] = prefab();
            value["prefabs"][path]["components"] = serde_json::json!({"PathSequencePowerline":1});
            value["prefabs"][path]["attributes"] = serde_json::json!([
                {"kind":"PathSequencePowerline","root":true,"position":[0.,0.,0.],"fields":{"Rule":rule}}
            ]);
        }
        value
    }
    fn set_resource_count(value: &mut serde_json::Value, path: &str, count: f32) {
        value["prefabs"][path]["components"]["PrefabParameters"] = 1.into();
        let attributes = value["prefabs"][path]["attributes"].as_array_mut().unwrap();
        attributes.retain(|a| a["kind"] != "PrefabParameters");
        attributes.push(serde_json::json!({"kind":"PrefabParameters","root":true,
            "position":[0.,0.,0.],"fields":{"Priority":2,"Count":count}}));
    }
    fn weighted_source_value() -> serde_json::Value {
        let mut value = source_value();
        value["path_policy"]["resource_weights"] = serde_json::json!({
            "engine":path_resources::VERSION,"maximum_tickets":1000
        });
        value["prefabs"][B] = prefab();
        value["stages"][0]["side"][0]["prefabs"] = serde_json::json!([A,B]);
        set_resource_count(&mut value, A, 0.);
        set_resource_count(&mut value, B, 3.);
        value
    }
    #[test]
    fn resource_counts_filter_all_path_groups_before_fitting_and_endpoint_trimming() {
        let layers = layers();
        let mut value = weighted_source_value();
        let mut invalid = anchor(0.);
        invalid["position"] = serde_json::json!([0.,0.]);
        value["prefabs"][A]["attributes"].as_array_mut().unwrap().push(invalid);
        value["prefabs"][A]["components"]["TerrainAnchor"] = 1.into();
        let mut endpoint = basic();
        endpoint["prefabs"] = serde_json::json!([A,B]);
        value["stages"][0]["start"] = serde_json::json!([endpoint.clone()]);
        value["stages"][0]["end"] = serde_json::json!([endpoint]);
        value["stages"][0]["path"] = serde_json::json!([{
            "folder":"decor/test","filter":filter(),"prefabs":[A,B],"alignment":2,
            "density":1.,"distance":25.,"dithering":0.
        }]);
        let report = run(&layers, &source(value));
        assert!(!report.provisional_poses.is_empty());
        assert!(report.provisional_poses.iter().all(|pose| pose.prefab == B));
        for group in [Group::Start, Group::End, Group::Side, Group::Path] {
            assert!(report.provisional_poses.iter().any(|pose| pose.group == group));
        }
        assert_eq!(report.trims[0].min_index, 0);
        assert_eq!(report.trims[0].max_index, 100);
    }
    #[test]
    fn disabled_resources_are_excluded_from_sequence_fallbacks() {
        let layers = layers();
        let mut value = powerline_source_value();
        value["path_policy"]["resource_weights"] = weighted_source_value()["path_policy"]["resource_weights"].clone();
        set_resource_count(&mut value, A, 2.);
        set_resource_count(&mut value, B, 0.);
        let report = run(&layers, &source(value));
        assert_eq!(report.neighbor_sequences[0].poses.len(), 7);
        assert!(report.provisional_poses.iter().all(|pose| pose.prefab == A));
    }
    #[test]
    fn empty_resource_rules_preserve_later_random_choices_and_path_extents() {
        let layers = layers();
        let mut value = weighted_source_value();
        let mut active = side();
        active["prefabs"] = serde_json::json!([B]);
        value["stages"][0]["side"] = serde_json::json!([side(), active.clone()]);
        value["stages"][0]["start"] = serde_json::json!([basic()]);
        value["stages"][0]["end"] = serde_json::json!([basic()]);
        let with_disabled = run(&layers, &source(value.clone()));
        value["prefabs"].as_object_mut().unwrap().remove(A);
        value["stages"][0]["side"] = serde_json::json!([active]);
        value["stages"][0]["start"] = serde_json::json!([]);
        value["stages"][0]["end"] = serde_json::json!([]);
        let baseline = run(&layers, &source(value));
        let poses = |report: &Report| report.provisional_poses.iter()
            .map(|pose| serde_json::to_value(pose.transform).unwrap()).collect::<Vec<_>>();
        assert_eq!(poses(&with_disabled), poses(&baseline));
        assert_eq!(with_disabled.work, baseline.work);
        assert_eq!(with_disabled.trims[0].min_index, baseline.trims[0].min_index);
        assert_eq!(with_disabled.trims[0].max_index, baseline.trims[0].max_index);
    }
    #[test]
    fn resource_parameters_use_the_first_root_component_and_reject_missing_coverage() {
        let mut value = weighted_source_value();
        let mut metadata: source::Prefab = serde_json::from_value(value["prefabs"][B].clone()).unwrap();
        metadata.attributes[0]["root"] = false.into();
        assert_eq!(resource_count(&metadata).unwrap(), 1);
        let mut first = metadata.attributes[0].clone();
        first["root"] = true.into();
        first["fields"]["Count"] = 2.1.into();
        metadata.attributes.push(first);
        metadata.components.insert("PrefabParameters".into(), 2);
        assert_eq!(resource_count(&metadata).unwrap(), 3);
        metadata.components.insert("PrefabParameters".into(), 3);
        assert!(resource_count(&metadata).is_err());
        value["path_policy"]["resource_weights"]["maximum_tickets"] = 2.into();
        let source = source(value);
        let alpha = vec![255; layers().surface_resolution.pow(2)];
        assert!(fit(&layers(), &alpha, &[road()], &source, settings(), || false).is_err());
        let legacy = run(&layers(), &self::source(source_value()));
        assert!(serde_json::to_value(legacy.policy).unwrap().get("resource_weights").is_none());
    }
    #[test]
    fn bound_powerline_sequences_replace_before_fitting_and_legacy_profiles_stay_explicit() {
        let layers = layers();
        let mut value = powerline_source_value();
        let report = run(&layers, &source(value.clone()));
        let sequence = &report.neighbor_sequences[0];
        assert_eq!(sequence.requested, 7);
        let paths: Vec<_> = sequence.poses.iter()
            .map(|&id| report.provisional_poses[id].prefab.as_str()).collect();
        assert_eq!(paths, [B,A,A,B,A,A,B]);
        assert!(report.prefabs.values().all(|ready| !ready.pending.iter().any(|s| s == "PathSequencePowerline")));
        value["path_policy"].as_object_mut().unwrap().remove("powerline_sequence");
        let report = run(&layers, &source(value));
        assert!(report.provisional_poses.is_empty());
        assert_eq!(report.counts["unsupported_path_sequence"], 7);
        assert!(serde_json::to_value(report.policy).unwrap().get("powerline_sequence").is_none());
    }
    #[test]
    fn rejected_platform_fits_do_not_advance_the_accepted_sequence() {
        let layers = layers();
        let mut value = powerline_source_value();
        let mut outside = anchor(0.);
        outside["position"] = serde_json::json!([0.,0.,2000.]);
        value["prefabs"][B]["attributes"].as_array_mut().unwrap().push(outside);
        value["prefabs"][B]["components"]["TerrainAnchor"] = 1.into();
        let report = run(&layers, &source(value));
        assert!(report.provisional_poses.is_empty());
        assert!(report.neighbor_sequences[0].poses.is_empty());
        assert!(layers.height.iter().all(|v| *v == 18000));
    }
    #[test]
    fn incomplete_or_unknown_powerline_sequence_metadata_fails_preparation() {
        let layers = layers();
        for change in ["missing", "count", "rule", "extra"] {
            let mut value = powerline_source_value();
            match change {
                "missing" => value["prefabs"][A]["attributes"] = serde_json::json!([]),
                "count" => value["prefabs"][A]["components"]["PathSequencePowerline"] = 2.into(),
                "rule" => value["prefabs"][A]["attributes"][0]["fields"]["Rule"] = 3.into(),
                _ => value["prefabs"][A]["attributes"][0]["fields"]["Future"] = true.into(),
            }
            let source = source(value);
            assert!(fit(&layers, &vec![255; layers.surface_resolution.pow(2)], &[road()], &source,
                settings(), || false).is_err(), "{change}");
        }
    }
    #[test]
    fn river_paths_do_not_inherit_the_road_only_hierarchy_gate() {
        let layers = layers();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let mut path = road();
        path.hierarchy = 17;
        let mut value = source_value();
        let road_source = source(value.clone());
        let road_report = fit(
            &layers,
            &alpha,
            std::slice::from_ref(&path),
            &road_source,
            settings(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(road_report.provisional_poses.is_empty());
        value["schema"] = 4.into();
        value["adapter"] = source::PATH_VERSION.into();
        value["path_family"] = "river".into();
        value["stages"][0]["scene_order"] = 0.into();
        let stage = &value["stages"][0];
        value["workflow"] = serde_json::json!({"format":"papyrus-procedural-order","schema":1,"stages":[{
            "object_id":stage["object_id"],"order":0,"kind":"PlaceRiverObjects","description":stage["name"],
            "mode":stage["mode"],"disabled":stage["disabled"],"component_enabled":stage["component_enabled"],"hierarchy_active":stage["hierarchy_active"]}]});
        value["environments"] = serde_json::json!({A:{"root_transform":{"position":[0,0,0],"rotation":[0,0,0,1],"scale":[1,1,1]},
            "checks":[],"volumes":[],"issues":[],"collider_meshes":[]}});
        let river_source = source(value);
        let report = fit(&layers, &alpha, &[path], &river_source, settings(), || {
            false
        })
        .unwrap()
        .unwrap();
        assert!(!report.provisional_poses.is_empty());
        assert!(!report.counts.contains_key("skipped_road_hierarchy"));
    }
}
