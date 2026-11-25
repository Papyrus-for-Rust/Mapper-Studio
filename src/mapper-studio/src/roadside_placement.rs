//! Provisional grouped roadside placement on the retained pre-road revision.
//! Environment/physics acceptance and final painting are not completed here.
use crate::{
    Project, composition, monument_source, prefab_assets::Catalog, prefab_textures::Bindings,
    road_planning::Plan, roadside_source,
};
use papyrus_terrain::{
    monument_bounds::Footprint,
    monument_composition::{Composed, HeightTerrain, Stamp},
    monument_eligibility::{Check as TerrainCheck, Evaluator},
    monument_spacing::{BoxPose, BoxShape},
    placement::Solution,
    roadside_monuments,
    roadside_selection::{self as selection, Check, Failure, Request},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    cell::{OnceCell, RefCell},
    collections::{BTreeMap, BTreeSet, HashMap},
};
pub struct RequestedPrefab {
    pub path: String,
    pub requests: Vec<Request>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct QualityFeedback {
    pub engine: String,
    pub binding: String,
    pub excluded: BTreeSet<(u64, usize)>,
    pub baseline_selected: usize,
}
impl QualityFeedback {
    fn validate(&self) -> Result<(), String> {
        if self.engine != "papyrus-road-quality-feedback-1"
            || self.binding.len() != 64
            || !self.binding.bytes().all(|b| b.is_ascii_hexdigit())
            || self.excluded.len() > 128
            || self.baseline_selected > 128
        {
            return Err("Invalid road-quality feedback".into());
        }
        Ok(())
    }
}
#[derive(Clone, Copy)]
pub struct Context<'a> {
    pub project: &'a Project,
    pub initial: &'a Solution,
    pub catalog: &'a Catalog,
    pub bindings: &'a Bindings,
    pub maps: &'a Composed,
    pub plan: &'a Plan,
    pub source: &'a roadside_source::Source,
}
#[derive(Serialize)]
pub struct Provisional {
    pub selection: selection::Selection,
    pub skipped: BTreeMap<String, String>,
    pub pending: BTreeSet<String>,
    pub environment_volumes_validated: bool,
    pub runtime_physics_validated: bool,
    pub accepted_world_published: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality_feedback: Option<QualityFeedback>,
    #[serde(skip)]
    route_binding: String,
    #[serde(skip)]
    receipt_binding: String,
    #[serde(skip)]
    source_sha256: String,
    #[serde(skip)]
    stamps: BTreeMap<u64, (String, composition::Prepared)>,
    #[serde(skip)]
    stamp_inputs: (String, String, u32),
}
fn receipt(
    selection: &selection::Selection,
    skipped: &BTreeMap<String, String>,
    pending: &BTreeSet<String>,
    feedback: Option<&QualityFeedback>,
) -> Result<String, String> {
    let bytes = if let Some(feedback) = feedback {
        serde_json::to_vec(&(selection, skipped, pending, feedback))
    } else {
        serde_json::to_vec(&(selection, skipped, pending))
    }
    .map_err(|e| e.to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
impl Provisional {
    pub fn source_sha256(&self) -> &str {
        &self.source_sha256
    }
    pub(crate) fn check_stamp_inputs(
        &self,
        catalog_sha256: &str,
        bindings_sha256: &str,
        world_size: u32,
    ) -> Result<(), String> {
        let (catalog_id, bindings_id, size) = &self.stamp_inputs;
        if catalog_id != catalog_sha256 || bindings_id != bindings_sha256 || *size != world_size {
            return Err("Roadside stamp inputs changed; re-selection is required".into());
        }
        Ok(())
    }
    pub(crate) fn prepared_stamp(&self, group: u64) -> Option<(&str, &composition::Prepared)> {
        self.stamps.get(&group).map(|(path, stamp)| (path.as_str(), stamp))
    }
    fn check_receipt(&self) -> Result<(), String> {
        if self.environment_volumes_validated
            || self.runtime_physics_validated
            || self.accepted_world_published
            || receipt(
                &self.selection,
                &self.skipped,
                &self.pending,
                self.quality_feedback.as_ref(),
            )? != self.receipt_binding
        {
            return Err("Roadside provisional receipt changed; re-selection is required".into());
        }
        Ok(())
    }
    /// Adjust exact retained routes for this selection without painting terrain.
    /// Edited selections or a different road geometry require fresh selection.
    pub fn straighten<'a>(
        &self,
        plan: &'a Plan,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<crate::road_planning::StraightenedPaths<'a>>, String> {
        self.check_receipt()?;
        let Some(binding) = plan.selection_binding(&self.selection.selected, &cancelled)? else {
            return Ok(None);
        };
        if binding != self.route_binding {
            return Err("Roadside selection or retained road geometry changed".into());
        }
        plan.straighten_selected(&self.selection.selected, maximum_work, cancelled)
    }
}
fn effective_asset(
    asset: &crate::prefab_assets::Asset,
    enabled: Option<bool>,
) -> Result<crate::prefab_assets::Asset, String> {
    let mut result = asset.clone();
    if let Some(enabled) = enabled {
        let modifier = result
            .modifier
            .as_mut()
            .ok_or("Missing roadside terrain modifier")?;
        if modifier.water_map.is_some_and(|value| value != enabled) {
            return Err("Catalog and roadside water policies disagree".into());
        }
        modifier.water_map = Some(enabled);
    }
    Ok(result)
}
fn validate_requests(
    initial: &Solution,
    initial_targets: usize,
    requested: &[RequestedPrefab],
) -> Result<(), String> {
    let count = requested
        .iter()
        .try_fold(initial_targets, |count, r| {
            count.checked_add(r.requests.len())
        })
        .ok_or("Roadside target count overflow")?;
    if count > papyrus_terrain::placement::MAX_TARGETS {
        return Err("Combined land and roadside targets exceed the project allowance".into());
    }
    let mut ids: BTreeSet<_> = initial.outcomes.iter().map(|o| o.id).collect();
    for request in requested.iter().flat_map(|r| &r.requests) {
        if request.id == 0 || !ids.insert(request.id) {
            return Err("Roadside request identity collides with an existing target".into());
        }
    }
    Ok(())
}
const REQUEST_DOMAIN: &[u8] = b"papyrus-roadside-request-1";
const ADDED_REQUEST_DOMAIN: &[u8] = b"papyrus-roadside-added-request-1";

fn hash_bytes(seed: u64, bytes: impl IntoIterator<Item = u8>) -> u64 {
    bytes.into_iter().fold(seed, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}

fn key_prefix(domain: &[u8], path: &str) -> u64 {
    hash_bytes(0xcbf29ce484222325, domain.iter().copied().chain(path.bytes()))
}

fn key(domain: &[u8], path: &str, slot: u64) -> u64 {
    hash_bytes(key_prefix(domain, path), slot.to_le_bytes())
}

/// Recognize source-roster occurrence IDs in existing saved map markers.
pub fn is_captured_request_id(path: &str, id: u64) -> bool {
    is_request_id(REQUEST_DOMAIN, path, id)
}

pub fn is_editable_request_id(path: &str, id: u64) -> bool {
    is_captured_request_id(path, id) || is_request_id(ADDED_REQUEST_DOMAIN, path, id)
}

fn is_request_id(domain: &[u8], path: &str, id: u64) -> bool {
    let prefix = key_prefix(domain, path);
    id != 0 && (0..papyrus_terrain::placement::MAX_TARGETS as u64)
        .any(|slot| hash_bytes(prefix, slot.to_le_bytes()) == id)
}

pub fn added_request_id(path: &str, used: impl Fn(u64) -> bool) -> Option<u64> {
    (0..papyrus_terrain::placement::MAX_TARGETS as u64)
        .map(|slot| key(ADDED_REQUEST_DOMAIN, path, slot))
        .find(|&id| id != 0 && !used(id))
}

fn edited_request(saved: &crate::road_generation::RoadsidePreference) -> Request {
    Request {
        id: saved.request,
        required: saved.presence != Some(crate::Presence::Excluded)
            && (saved.presence == Some(crate::Presence::Required) || saved.max_displacement_m.is_some()),
        excluded: saved.presence == Some(crate::Presence::Excluded),
        preference: Some(selection::Preference {
            position: saved.position,
            radius: 0.,
            max_displacement_m: saved.max_displacement_m,
            weight: if saved.max_displacement_m.is_some() { 1024. } else { 1. },
            yaw: Some(saved.yaw),
            exact: false,
        }),
    }
}

/// Return the source roster's default occurrences.
pub fn captured_requests(
    source: &roadside_source::Source,
    initial: &Solution,
) -> Result<Vec<RequestedPrefab>, String> {
    captured_requests_with_preferences(source, initial, &[])
}

/// Bind saved moves to the source roster's existing occurrence IDs.
pub(crate) fn captured_requests_with_preferences(
    source: &roadside_source::Source,
    initial: &Solution,
    preferences: &[crate::road_generation::RoadsidePreference],
) -> Result<Vec<RequestedPrefab>, String> {
    let mut pending = BTreeMap::new();
    for preference in preferences {
        if pending.insert(preference.request, preference).is_some() {
            return Err("Duplicate roadside monument preference".into());
        }
    }
    let mut used: BTreeSet<_> = initial.outcomes.iter().map(|o| o.id).collect();
    let mut result = Vec::new();
    let mut total = 0;
    for (path, p) in source.prefabs() {
        total += p.parameters.count;
        if total > 128 {
            return Err("Captured roadside roster exceeds target allowance".into());
        }
        let mut requests = Vec::new();
        for slot in 0..p.parameters.count {
            let id = key(REQUEST_DOMAIN, path, slot as u64);
            if id == 0 || !used.insert(id) {
                return Err("Roadside request identity collision".into());
            }
            let request = if let Some(saved) = pending.remove(&id) {
                if saved.prefab != *path {
                    return Err("Roadside monument preference refers to a different prefab".into());
                }
                edited_request(saved)
            } else {
                Request { id, required: false, excluded: false, preference: None }
            };
            requests.push(request);
        }
        for &saved in pending.values().filter(|saved| {
            saved.prefab == *path && is_request_id(ADDED_REQUEST_DOMAIN, path, saved.request)
        }) {
            total += 1;
            if total > papyrus_terrain::placement::MAX_TARGETS || !used.insert(saved.request) {
                return Err("Too many roadside requests or duplicate occurrence identity".into());
            }
            requests.push(edited_request(saved));
        }
        for request in &requests[p.parameters.count..] {
            pending.remove(&request.id);
        }
        result.push(RequestedPrefab {
            path: path.clone(),
            requests,
        });
    }
    if let Some((_, saved)) = pending.first_key_value() {
        return Err(format!("Roadside preference for {} is absent from the selected installation's roster", saved.prefab));
    }
    Ok(result)
}
struct PreparedGroup<'a> {
    path: String,
    key: u64,
    shape: BoxShape,
    priority: u8,
    candidates: Vec<roadside_monuments::Candidate>,
    prepared: composition::Prepared,
    borders: [Footprint; 2],
    evaluator: OnceCell<Evaluator<'a>>,
}
fn fits_world(borders: &[Footprint; 2], position: [f32; 3], yaw: f32, size: u32) -> bool {
    borders.iter().all(|b| {
        b.at([position[0], position[2]], yaw)
            .inside_world(size as f32)
    })
}
fn world(local: [f32; 3], position: [f32; 3], yaw: f32) -> [f32; 3] {
    let (s, c) = yaw.to_radians().sin_cos();
    [
        position[0] + c * local[0] - s * local[2],
        position[1] + local[1],
        position[2] + s * local[0] + c * local[2],
    ]
}
pub fn select(
    context: Context<'_>,
    requested: &[RequestedPrefab],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Provisional>, String> {
    select_with_feedback(context, requested, None, maximum_work, cancelled)
}
#[derive(Debug, PartialEq)]
pub(crate) enum SelectionFailure {
    Budget,
    BudgetDetails(String),
    Other(String),
}
impl From<String> for SelectionFailure {
    fn from(value: String) -> Self {
        Self::Other(value)
    }
}
impl From<&str> for SelectionFailure {
    fn from(value: &str) -> Self {
        Self::Other(value.into())
    }
}
impl std::fmt::Display for SelectionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Budget => f.write_str("Roadside selection work allowance exhausted"),
            Self::BudgetDetails(detail) => {
                write!(f, "Roadside selection work allowance exhausted: {detail}")
            }
            Self::Other(message) => f.write_str(message),
        }
    }
}
fn selection_failure(error: Failure, stage: &str) -> SelectionFailure {
    match error {
        Failure::Budget => SelectionFailure::Budget,
        other => SelectionFailure::Other(format!("{stage}: {other:?}")),
    }
}
#[derive(Debug, Default)]
struct SelectionCosts {
    evaluated_candidates: usize,
    terrain_eligibility: usize,
    earthwork_samples: usize,
    projected_road_checks: usize,
    projected_road_candidates: usize,
    projected_road_accepted: usize,
    projected_rejections: BTreeMap<&'static str, usize>,
}

/// Scoped to the same immutable pre-road terrain, recipe, and asset receipts.
/// Road and rail checks still use the current trial's geometry.
#[derive(Default)]
pub(crate) struct EarthworkCache {
    entries: HashMap<String, HashMap<[u64; 5], (u64, bool)>>,
    len: usize,
    reused_samples: u64,
}

impl EarthworkCache {
    fn check(
        &mut self,
        terrain: &HeightTerrain<'_>,
        path: &str,
        stamp: &Stamp<'_>,
        limit: f64,
        budget: u64,
        cancelled: &impl Fn() -> bool,
    ) -> (u64, Result<Option<bool>, String>) {
        if cancelled() {
            return (0, Ok(None));
        }
        let [x, y, z] = stamp.position;
        let key = [x, y, z, stamp.yaw_degrees, limit].map(f64::to_bits);
        if let Some(&(spent, accepted)) = self.entries.get(path).and_then(|poses| poses.get(&key))
            && spent <= budget
        {
            self.reused_samples += spent;
            return (spent, Ok(Some(accepted)));
        }
        let mut spent = 0;
        let assessment = terrain.check_placement(stamp, &mut spent, limit, budget, false, cancelled);
        (spent, assessment.map(|assessment| assessment.map(|a| {
            let accepted = a.samples != 0
                && a.clamped_samples == 0
                && a.submerged_samples == 0
                && a.maximum_change <= limit;
            if self.len < 8192 {
                let previous = self.entries.entry(path.to_owned()).or_default()
                    .insert(key, (spent, accepted));
                self.len += usize::from(previous.is_none());
            }
            accepted
        })))
    }

    pub(crate) fn reused_samples(&self) -> u64 {
        self.reused_samples
    }
}
fn selection_cost_failure(
    error: Failure,
    stage: &str,
    costs: &std::cell::RefCell<SelectionCosts>,
) -> SelectionFailure {
    if error == Failure::Budget {
        SelectionFailure::BudgetDetails(format!("{stage}; {:?}", *costs.borrow()))
    } else {
        selection_failure(error, stage)
    }
}
/// Keeps the selection and its validated revision borrowed together through
/// stamping, so the base terrain and its sources cannot change between them.
pub(crate) struct ValidatedSelection<'a> {
    context: Context<'a>,
    selection: Provisional,
}
impl<'a> ValidatedSelection<'a> {
    pub(crate) fn work(&self) -> usize {
        self.selection.selection.work
    }
    pub(crate) fn into_parts(self) -> (Context<'a>, Provisional) {
        (self.context, self.selection)
    }
}

pub(crate) fn select_metered<'a>(
    context: Context<'a>,
    prepared: &crate::road_planning::AlignmentBase<'_>,
    requested: &[RequestedPrefab],
    earthwork: &RefCell<EarthworkCache>,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<ValidatedSelection<'a>>, SelectionFailure> {
    Ok(select_inner(context, requested, None, Some(prepared), earthwork, maximum_work, cancelled)?
        .map(|selection| ValidatedSelection { context, selection }))
}
pub fn select_with_feedback(
    context: Context<'_>,
    requested: &[RequestedPrefab],
    feedback: Option<&QualityFeedback>,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Provisional>, String> {
    select_inner(context, requested, feedback, None, &RefCell::default(), maximum_work, cancelled)
        .map_err(|error| error.to_string())
}
fn select_inner(
    context: Context<'_>,
    requested: &[RequestedPrefab],
    feedback: Option<&QualityFeedback>,
    prepared: Option<&crate::road_planning::AlignmentBase<'_>>,
    earthwork: &RefCell<EarthworkCache>,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Provisional>, SelectionFailure> {
    let Context {
        project,
        initial,
        catalog,
        bindings,
        maps,
        plan,
        source,
    } = context;
    if cancelled() {
        return Ok(None);
    }
    project.validate()?;
    let mut quality_feedback = if project
        .roads
        .as_ref()
        .and_then(|r| r.entrance_launch)
        .is_some_and(|p| {
            matches!(
                p.engine,
                papyrus_terrain::road_network::LaunchEngine::BedFeedback
                    | papyrus_terrain::road_network::LaunchEngine::SurfaceChecked
                    | papyrus_terrain::road_network::LaunchEngine::GlobalEarthwork
            )
        }) {
        let Some(binding) = plan.quality_feedback_binding(&source.sha256, &cancelled)? else {
            return Ok(None);
        };
        if let Some(feedback) = feedback {
            feedback.validate()?;
            if feedback.binding != binding {
                return Err("Road-quality feedback belongs to a different plan or terrain".into());
            }
            Some(feedback.clone())
        } else {
            Some(QualityFeedback {
                engine: "papyrus-road-quality-feedback-1".into(),
                binding,
                excluded: BTreeSet::new(),
                baseline_selected: 0,
            })
        }
    } else {
        if feedback.is_some() {
            return Err("Road-quality feedback requires its explicit recipe revision".into());
        }
        None
    };
    validate_requests(initial, project.targets.len(), requested)?;
    if !project.performance.monuments {
        return Err("Roadside placement contradicts the monument content policy".into());
    }
    project.check_monument_assets(&catalog.sha256, &bindings.sha256)?;
    if source.catalog_sha256() != catalog.sha256
        || maps.layers.size != project.size
        || maps.layers.terrain_seed != project.seeds[0].value
        || maps.layers.biome_seed != project.seeds[1].value
    {
        return Err("Roadside source or terrain identity mismatch".into());
    }
    if plan
        .check_inputs_with_base(project, initial, catalog, bindings, maps, prepared, &cancelled)?
        .is_none()
    {
        return Ok(None);
    }
    // The bound pre-mesh scene already validates raster boolean values. Borrow
    // it once; proposal evaluation performs only the authored point queries.
    let placement_map = maps
        .rails
        .as_ref()
        .map(|scene| {
            papyrus_terrain::placement_map::PlacementMap::new(
                maps.layers.size as f32,
                maps.layers.surface_resolution,
                &scene.reservations.raster,
            )
        })
        .transpose()?;
    if !source.stage().runs(
        project.size,
        papyrus_terrain::monument_stage::Context {
            realm_mask: 2,
            cached: false,
            nexus: false,
        },
    )? {
        return Err("Roadside stage does not run for this world".into());
    }
    let recipe = project
        .monument_rules
        .as_ref()
        .ok_or("Roadside placement requires installation monument rules")?;
    let monument_source = monument_source::Source::cached_shared(recipe, catalog, bindings, &cancelled)?;
    let ceiling = recipe
        .coastal_profile
        .as_ref()
        .ok_or("Roadside earthwork requires the installation execution profile")?
        .maximum_cut_fill;
    let mut existing = Vec::new();
    for outcome in &initial.outcomes {
        let Some(position) = &outcome.position else {
            continue;
        };
        let target = project
            .targets
            .iter()
            .find(|t| t.instance_id == outcome.id)
            .ok_or("Unknown initial monument")?;
        let path = target
            .prefab_path
            .as_ref()
            .ok_or("Initial monument has no prefab")?;
        if source.prefabs().contains_key(path) {
            return Err(
                "Initial layout already contains roadside monuments; regenerate the land stage"
                    .into(),
            );
        }
        let b = catalog
            .entries
            .get(path)
            .and_then(|p| p.bounds.as_ref())
            .ok_or("Initial monument has no authored bounds")?;
        existing.push(BoxPose {
            shape: BoxShape {
                center: b.center,
                extent: b.extent,
            },
            position: [position.x, position.y, position.z],
            yaw: position.yaw,
        });
    }
    let roads = plan.roads();
    let mut prepared_groups = Vec::new();
    let mut skipped = BTreeMap::new();
    let mut pending = BTreeSet::new();
    let mut requested_paths = BTreeSet::new();
    let mut texture_bytes = 0;
    for request in requested {
        if cancelled() {
            return Ok(None);
        }
        if !requested_paths.insert(&request.path) {
            return Err("Duplicate requested roadside prefab".into());
        }
        let prefab = source
            .prefabs()
            .get(&request.path)
            .ok_or("Requested prefab is outside the roadside stage")?;
        let asset = catalog
            .entries
            .get(&request.path)
            .ok_or("Missing roadside catalog entry")?;
        let rules = monument_source
            .prefabs
            .get(&request.path)
            .ok_or("Missing roadside eligibility metadata")?;
        let water_supported = !(asset.texture_guids.contains_key("water")
            && prefab.water_map_enabled() != Some(false))
            || crate::monument_water_hook::verified_noop(bindings, &cancelled)?;
        if cancelled() { return Ok(None); }
        let reason = if prefab.dungeon_components().is_none() {
            Some("Prepare current roadside metadata with dungeon inventory")
        } else if prefab
            .dungeon_components()
            .is_some_and(|d| !d.links.is_empty() || !d.entrances.is_empty())
        {
            Some("Dungeon-linked roadside prefabs need a separate adapter")
        } else if !rules.rules.unsupported.is_empty() {
            Some("Unsupported authored roadside rules")
        } else if rules
            .placement_attributes
            .as_ref()
            .is_none_or(|a| !a.issues.is_empty() || !a.water_checks.is_empty())
        {
            Some("Roadside placement attributes need an adapter")
        } else if !water_supported {
            Some("Monument water placement needs a compatible selected Rust installation")
        } else if asset
            .bounds
            .as_ref()
            .is_none_or(|b| b.minimum_world_size > project.size)
        {
            Some("Missing bounds or world below prefab minimum")
        } else {
            None
        };
        if let Some(reason) = reason {
            skipped.insert(request.path.clone(), reason.into());
            continue;
        }
        pending.extend(rules.rules.pending.iter().cloned());
        let effective_asset = effective_asset(asset, prefab.water_map_enabled())?;
        let Some(prepared) = composition::prepare_with_budget(
            &effective_asset,
            bindings,
            project.size,
            &cancelled,
            256 * 1024 * 1024 - texture_bytes,
        )?
        else {
            return Ok(None);
        };
        texture_bytes += prepared.bytes();
        let bounds = asset.bounds.as_ref().unwrap();
        let sockets: Vec<_> = prefab
            .road_connectors(source.profile())
            .map(|c| c.position)
            .collect();
        let Some(mut proposals) = roadside_monuments::generate(
            &sockets,
            &roads,
            source.stage().road_mode()?,
            source.profile().settings(2_000_000, 100_000),
            &cancelled,
        )
        .map_err(|e| format!("Roadside proposals failed: {e:?}"))?
        else {
            return Ok(None);
        };
        for candidate in &mut proposals.candidates {
            let angle = candidate.yaw.rem_euclid(360.);
            candidate.yaw = if angle == 0. { 0. } else { angle };
        }
        prepared_groups.push(PreparedGroup {
            path: request.path.clone(),
            key: key(b"papyrus-roadside-group-1", &request.path, 0),
            shape: BoxShape {
                center: bounds.center,
                extent: bounds.extent,
            },
            priority: prefab.parameters.priority as u8,
            candidates: proposals.candidates,
            prepared,
            borders: crate::placement_preview::bounds_for_recipe(project, asset)?.0,
            evaluator: OnceCell::new(),
        });
    }
    let groups: Vec<_> = prepared_groups
        .iter()
        .map(|g| selection::Group {
            key: g.key,
            priority: g.priority,
            shape: g.shape,
            requests: &requested
                .iter()
                .find(|r| r.path == g.path)
                .unwrap()
                .requests,
            candidates: &g.candidates,
        })
        .collect();
    if let Some(feedback) = &quality_feedback {
        for &(group, index) in &feedback.excluded {
            if !prepared_groups
                .iter()
                .any(|g| g.key == group && index < g.candidates.len())
            {
                return Err("Road-quality feedback references an unknown candidate".into());
            }
        }
    }
    let height = HeightTerrain::new(
        &maps.layers.height,
        maps.layers.height_resolution,
        maps.layers.size,
    )?;
    let road_guard = if project
        .roads
        .as_ref()
        .is_some_and(|r| r.entrance_launch.is_some())
    {
        let paint = crate::road_paint_source::Profile::for_source(source)?
            .ok_or("No projected-road height policy for this installation")?;
        let terrain = crate::road_terrain_source::Profile::for_paint(&paint)?
            .ok_or("No projected-road terrain policy for this installation")?;
        let Some(projection) = papyrus_terrain::monument_composition::projection::Terrain::new(
            &maps.layers,
            &maps.alpha,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        Some((paint, terrain, projection))
    } else {
        None
    };
    let costs = std::cell::RefCell::new(SelectionCosts::default());
    let road_checks = std::cell::RefCell::new(None);
    let mut memo = BTreeMap::<(u64, usize), Result<f32, &'static str>>::new();
    let stage = source.stage();
    let policy = selection::Policy {
        seed: project.seeds[4].value,
        attempts: source.profile().group_attempts,
        successful_candidates: source.profile().individual_candidates,
        target_count: stage.target_count,
        minimum_same: f64::from(stage.minimum_distance_same_type),
        minimum_other: f64::from(stage.minimum_distance_different_type),
        same_mode: stage.distance_same_type.try_into()?,
        other_mode: stage.distance_different_type.try_into()?,
        maximum_work,
    };
    let mut evaluate = |apply_feedback: bool,
                        group: u64,
                        request: u64,
                        index: usize,
                        candidate: &roadside_monuments::Candidate,
                        work: &mut selection::Work| {
        if apply_feedback
            && quality_feedback
                .as_ref()
                .is_some_and(|f| f.excluded.contains(&(group, index)))
        {
            work.spend(1)?;
            return Ok(Check::Rejected(
                "Candidate rejected by a prior road-bed quality attempt",
            ));
        }
        if let Some(value) = memo.get(&(group, index)) {
            return Ok(match value {
                Ok(height) => Check::Accepted { height: *height },
                Err(reason) => Check::Rejected(reason),
            });
        }
        costs.borrow_mut().evaluated_candidates += 1;
        let g = prepared_groups.iter().find(|g| g.key == group).unwrap();
        let p = &monument_source.prefabs[&g.path];
        let mut position = candidate.position;
        let assessed = (|| -> Result<Check, Failure> {
            if !fits_world(&g.borders, position, candidate.yaw, project.size) {
                return Ok(Check::Rejected(
                    "Roadside footprint reaches outside the map",
                ));
            }
            // Other proposals in this group borrow the same terrain and rules.
            let evaluator = if let Some(evaluator) = g.evaluator.get() {
                evaluator
            } else {
                let footprint = Footprint {
                    center: [g.shape.center[0], g.shape.center[2]],
                    extent: [g.shape.extent[0], g.shape.extent[2]],
                };
                let Some(evaluator) = Evaluator::new(&maps.layers, &p.rules, footprint, &cancelled)
                    .map_err(Failure::Evaluation)?
                else {
                    return Ok(Check::Cancelled);
                };
                let evaluator = evaluator
                    .with_stage_filter(&stage.filter)
                    .map_err(Failure::Evaluation)?;
                let evaluator = if let Some(map) = placement_map {
                    evaluator
                        .with_placement_map(map)
                        .map_err(Failure::Evaluation)?
                } else {
                    evaluator
                };
                g.evaluator.get_or_init(|| evaluator)
            };
            let mut spent = 0;
            let check = evaluator
                .evaluate(
                    position,
                    candidate.yaw,
                    &mut spent,
                    work.remaining(),
                    &cancelled,
                )
                .map_err(Failure::Evaluation)?;
            costs.borrow_mut().terrain_eligibility += spent;
            work.spend(spent)?;
            match check {
                None => return Ok(Check::Cancelled),
                Some(TerrainCheck::Budget) => return Err(Failure::Budget),
                Some(TerrainCheck::Rejected(reason)) => return Ok(Check::Rejected(reason)),
                Some(TerrainCheck::Accepted(p)) => position = p,
            }
            let attributes = p.placement_attributes.as_ref().unwrap();
            for check in &attributes.terrain_checks {
                work.spend(1)?;
                let point = world(
                    check.position,
                    position,
                    if check.rotate { candidate.yaw } else { 0. },
                );
                let u = point[0] / project.size as f32 + 0.5;
                let v = point[2] / project.size as f32 + 0.5;
                if !(0. ..=1.).contains(&u) || !(0. ..=1.).contains(&v) {
                    return Ok(Check::Rejected(
                        "Roadside terrain check lies outside the map",
                    ));
                }
                if let Some(map) = placement_map {
                    work.spend(1)?;
                    if map
                        .is_blocked([point[0], point[2]])
                        .map_err(Failure::Evaluation)?
                    {
                        return Ok(Check::Rejected(
                            "Roadside terrain check intersects reserved placement",
                        ));
                    }
                }
                let terrain = papyrus_terrain::spawn::sample_height(&maps.layers, u, v)
                    .map_err(Failure::Evaluation)?;
                if papyrus_terrain::spawn::sample_factor(&maps.layers, &stage.filter, u, v)
                    .map_err(Failure::Evaluation)?
                    <= 0.
                    || terrain < point[1] - check.extents
                    || terrain > point[1] + check.extents
                {
                    return Ok(Check::Rejected("Authored roadside terrain check failed"));
                }
            }
            let limit = crate::placement_preview::authored_earthwork_limit(&p.rules, ceiling);
            if work.remaining() == 0 {
                return Err(Failure::Budget);
            }
            let stamp = g.prepared.stamp_for_recipe(
                position.map(f64::from),
                f64::from(candidate.yaw),
                project.composition_engine.as_deref(),
            );
            let (spent, result) = earthwork.borrow_mut().check(
                &height,
                &g.path,
                &stamp,
                limit,
                work.remaining().min(128_000_000) as u64,
                &cancelled,
            );
            costs.borrow_mut().earthwork_samples += spent as usize;
            work.spend(spent as usize)?;
            match result {
                Ok(None) => Ok(Check::Cancelled),
                Err(_) => Ok(Check::Rejected(
                    "Roadside stamp exceeds its terrain allowance",
                )),
                Ok(Some(accepted)) => {
                    if !accepted {
                        Ok(Check::Rejected(
                            "Roadside stamp is empty, clamped or submerged",
                        ))
                    } else {
                        if let Some((paint, terrain, projection)) = &road_guard {
                            let projected =
                                projection.projected(&stamp).map_err(Failure::Evaluation)?;
                            if let Some(guard) = plan.rail_height_guard() {
                                match crate::roadside_rail_check::assess(
                                    guard,
                                    &maps.layers,
                                    &projected,
                                    |n| work.spend(n),
                                    &cancelled,
                                )? {
                                    Check::Accepted { .. } => {}
                                    other => return Ok(other),
                                }
                            }
                            costs.borrow_mut().projected_road_candidates += 1;
                            let before = work.used();
                            let mut prepared = road_checks.borrow_mut();
                            if prepared.is_none() {
                                let Some(checks) = crate::roadside_road_check::Prepared::new(
                                    plan,
                                    &maps.layers,
                                    paint,
                                    &terrain.policy,
                                    work,
                                    &cancelled,
                                )?
                                else {
                                    return Ok(Check::Cancelled);
                                };
                                *prepared = Some(checks);
                            }
                            let result = prepared.as_ref().unwrap().assess(
                                    selection::Selected {
                                        group,
                                        request,
                                        candidate: index,
                                        road: candidate.road,
                                        start_index: candidate.start_index,
                                        end_index: candidate.end_index,
                                        position,
                                        yaw: candidate.yaw,
                                        score: 0.,
                                    },
                                    &projected,
                                    project.roads.as_ref().unwrap().maximum_grade,
                                    project.roads.as_ref().unwrap().entrance_launch.is_some_and(|p|
                                        matches!(p.engine,papyrus_terrain::road_network::LaunchEngine::ProtectedEdges | papyrus_terrain::road_network::LaunchEngine::BedFeedback | papyrus_terrain::road_network::LaunchEngine::SurfaceChecked | papyrus_terrain::road_network::LaunchEngine::GlobalEarthwork)),
                                    work,
                                    &cancelled,
                                );
                            costs.borrow_mut().projected_road_checks +=
                                work.used().saturating_sub(before);
                            if matches!(result, Ok(Check::Accepted { .. })) {
                                costs.borrow_mut().projected_road_accepted += 1;
                            }
                            if let Ok(Check::Rejected(reason)) = &result {
                                *costs
                                    .borrow_mut()
                                    .projected_rejections
                                    .entry(reason)
                                    .or_default() += 1;
                            }
                            return result;
                        }
                        Ok(Check::Accepted {
                            height: position[1],
                        })
                    }
                }
            }
        })()?;
        match assessed {
            Check::Accepted { height } => {
                memo.insert((group, index), Ok(height));
            }
            Check::Rejected(reason) => {
                memo.insert((group, index), Err(reason));
            }
            Check::Cancelled => {}
        }
        Ok(assessed)
    };
    let baseline = if quality_feedback
        .as_ref()
        .is_some_and(|f| !f.excluded.is_empty())
    {
        let Some(baseline) = selection::solve(
            &groups,
            &existing,
            policy,
            |g, r, i, c, w| evaluate(false, g, r, i, c, w),
            &cancelled,
        )
        .map_err(|e| selection_cost_failure(e, "Quality baseline selection failed", &costs))?
        else {
            return Ok(None);
        };
        Some(baseline)
    } else {
        None
    };
    let remaining = maximum_work
        .checked_sub(baseline.as_ref().map_or(0, |s| s.work))
        .filter(|v| *v > 0)
        .ok_or(SelectionFailure::Budget)?;
    let Some(mut selection) = selection::solve(
        &groups,
        &existing,
        selection::Policy {
            maximum_work: remaining,
            ..policy
        },
        |g, r, i, c, w| evaluate(true, g, r, i, c, w),
        &cancelled,
    )
    .map_err(|e| selection_cost_failure(e, "Roadside grouped selection failed", &costs))?
    else {
        return Ok(None);
    };
    if let Some(feedback) = &mut quality_feedback {
        let minimum = baseline
            .as_ref()
            .map_or(selection.selected.len(), |s| s.selected.len());
        if (baseline.is_some() && feedback.baseline_selected != minimum)
            || selection.selected.len() < minimum
        {
            return Err("Quality feedback changed its baseline or lost roadside monuments".into());
        }
        feedback.baseline_selected = minimum;
    }
    if let Some(baseline) = baseline {
        selection.work += baseline.work;
        selection.attempts += baseline.attempts;
        for (reason, count) in baseline.rejections {
            *selection
                .rejections
                .entry(format!("Quality baseline: {reason}"))
                .or_default() += count;
        }
    }
    for r in requested.iter().filter(|r| skipped.contains_key(&r.path)) {
        for request in &r.requests {
            if !request.excluded {
                selection.missing.push(request.id);
                if request.required {
                    selection.required_missing.push(request.id);
                }
            }
        }
    }
    selection.missing.sort_unstable();
    selection.required_missing.sort_unstable();
    let Some(route_binding) = plan.selection_binding(&selection.selected, &cancelled)? else {
        return Ok(None);
    };
    let receipt_binding = receipt(&selection, &skipped, &pending, quality_feedback.as_ref())?;
    let used_groups: BTreeSet<_> = selection.selected.iter().map(|item| item.group).collect();
    let stamps = prepared_groups
        .into_iter()
        .filter(|group| used_groups.contains(&group.key))
        .map(|group| (group.key, (group.path, group.prepared)))
        .collect();
    Ok(Some(Provisional {
        quality_feedback,
        selection,
        skipped,
        pending,
        environment_volumes_validated: false,
        runtime_physics_validated: false,
        accepted_world_published: false,
        route_binding,
        receipt_binding,
        source_sha256: source.sha256.clone(),
        stamps,
        stamp_inputs: (catalog.sha256.clone(), bindings.sha256.clone(), project.size),
    }))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn earthwork_cache_preserves_charges_and_rechecks_a_smaller_allowance() {
        use papyrus_terrain::{monument_composition::Image, native_layers};

        let size = 1000;
        let (resolution, _) = native_layers::dimensions(size).unwrap();
        let cells = vec![17_202; resolution * resolution]; // 25 m in native encoding.
        let terrain = HeightTerrain::new(&cells, resolution, size).unwrap();
        let mut stamp = Stamp {
            source_cliff_generation: false,
            source_quad_raster: false,
            source_height_blending: false,
            source_texture_sampling: false,
            position: [0., 25., 0.],
            yaw_degrees: 0.,
            size: [40.; 3],
            offset: [0.; 3],
            radius: 20.,
            fade: 4.,
            height: Some(Image { width: 1, height: 1, rgba: &[0; 4] }),
            blend: None,
            splat: None,
            splat_mask: 0,
            alpha: None,
            biome: None,
            biome_mask: 0,
            topology: None,
            topology_mask: 0,
            replace_topology: false,
            auto_cliff_splat: false,
            auto_cliff_topology: false,
        };
        let mut cache = EarthworkCache::default();
        let path = "assets/test.prefab";
        let first = cache.check(&terrain, path, &stamp, 8., 10_000, &|| false);
        let (spent, outcome) = &first;
        assert_eq!(outcome, &Ok(Some(true)));
        assert!(*spent > 1);
        assert_eq!(cache.reused_samples(), 0);
        assert_eq!(cache.check(&terrain, path, &stamp, 8., *spent, &|| false), first);
        assert_eq!(cache.reused_samples(), *spent);

        let tight = cache.check(&terrain, path, &stamp, 8., *spent - 1, &|| false);
        assert_eq!(tight.0, *spent);
        assert!(tight.1.is_err());
        assert_eq!(cache.check(&terrain, path, &stamp, 8., *spent, &|| true), (0, Ok(None)));
        stamp.yaw_degrees = 1.;
        assert_eq!(cache.check(&terrain, path, &stamp, 8., 10_000, &|| false).1, Ok(Some(true)));
        assert_eq!(cache.reused_samples(), *spent);
    }

    #[test]
    fn only_typed_selection_budget_failures_are_retryable() {
        assert_eq!(
            selection_failure(Failure::Budget, "stage"),
            SelectionFailure::Budget
        );
        assert!(matches!(
            selection_failure(Failure::Evaluation("Budget".into()), "stage"),
            SelectionFailure::Other(_)
        ));
        assert!(matches!(
            selection_failure(Failure::Invalid("Budget"), "stage"),
            SelectionFailure::Other(_)
        ));
    }
    #[test]
    fn quality_feedback_is_versioned_bounded_and_roundtrips() {
        let feedback = QualityFeedback {
            engine: "papyrus-road-quality-feedback-1".into(),
            binding: "a".repeat(64),
            excluded: BTreeSet::from([(7, 3)]),
            baseline_selected: 10,
        };
        feedback.validate().unwrap();
        let raw = serde_json::to_vec(&feedback).unwrap();
        assert_eq!(
            serde_json::from_slice::<QualityFeedback>(&raw).unwrap(),
            feedback
        );
        let mut bad = feedback.clone();
        bad.binding = "z".repeat(64);
        assert!(bad.validate().is_err());
        bad = feedback.clone();
        bad.engine = "future".into();
        assert!(bad.validate().is_err());
        bad = feedback;
        bad.excluded = (0..129).map(|i| (7, i)).collect();
        assert!(bad.validate().is_err());
    }
    #[test]
    fn roadside_requests_cannot_duplicate_land_ids_or_exceed_combined_project_capacity() {
        let initial = Solution {
            outcomes: vec![papyrus_terrain::placement::Outcome {
                id: 7,
                position: None,
                distance: None,
                within_radius: false,
                reason: String::new(),
            }],
            required_missing: 0,
            attempts: 1,
        };
        let request = |id| selection::Request {
            id,
            required: false,
            excluded: true,
            preference: None,
        };
        let groups = |requests| {
            vec![RequestedPrefab {
                path: "test".into(),
                requests,
            }]
        };
        assert!(validate_requests(&initial, 1, &groups(vec![request(8)])).is_ok());
        for ids in [vec![7], vec![0], vec![8, 8]] {
            assert!(
                validate_requests(&initial, 1, &groups(ids.into_iter().map(request).collect()))
                    .is_err()
            );
        }
        assert!(
            validate_requests(&initial, 128, &groups(vec![request(8)]))
                .unwrap_err()
                .contains("allowance")
        );
        assert!(validate_requests(&initial, usize::MAX, &groups(vec![request(8)])).is_err());
    }
    #[test]
    fn provisional_scope_and_pending_checks_cannot_be_edited_after_selection() {
        let selection = selection::Selection {
            engine: selection::VERSION,
            selected: vec![],
            missing: vec![7],
            required_missing: vec![7],
            attempts: 1,
            work: 1,
            rejections: BTreeMap::new(),
        };
        let skipped = BTreeMap::new();
        let pending = BTreeSet::from(["EnvironmentVolume".into()]);
        let receipt_binding = receipt(&selection, &skipped, &pending, None).unwrap();
        let mut provisional = Provisional {
            quality_feedback: None,
            selection,
            skipped,
            pending,
            receipt_binding,
            route_binding: String::new(),
            source_sha256: String::new(),
            stamps: BTreeMap::new(),
            stamp_inputs: ("catalog".into(), "bindings".into(), 1000),
            environment_volumes_validated: false,
            runtime_physics_validated: false,
            accepted_world_published: false,
        };
        assert!(provisional.check_receipt().is_ok());
        assert!(provisional.check_stamp_inputs("catalog", "bindings", 1000).is_ok());
        assert!(provisional.check_stamp_inputs("other", "bindings", 1000).is_err());
        assert!(provisional.check_stamp_inputs("catalog", "other", 1000).is_err());
        assert!(provisional.check_stamp_inputs("catalog", "bindings", 2000).is_err());
        provisional.selection.required_missing.clear();
        assert!(provisional.check_receipt().is_err());
        provisional.selection.required_missing.push(7);
        provisional.pending.clear();
        assert!(provisional.check_receipt().is_err());
        provisional.pending.insert("EnvironmentVolume".into());
        provisional.accepted_world_published = true;
        assert!(provisional.check_receipt().is_err());
        provisional.accepted_world_published = false;
        assert!(provisional.check_receipt().is_ok());
        provisional.quality_feedback = Some(QualityFeedback {
            engine: "papyrus-road-quality-feedback-1".into(),
            binding: "a".repeat(64),
            excluded: BTreeSet::new(),
            baseline_selected: 0,
        });
        assert!(provisional.check_receipt().is_err());
    }
    #[test]
    fn modifier_extent_and_offset_are_checked_even_when_monument_objects_fit() {
        let object = Footprint {
            center: [0.; 2],
            extent: [20.; 2],
        };
        let borders = [
            object,
            Footprint {
                center: [80., 0.],
                extent: [120., 90.],
            },
        ];
        assert!(object.at([400., 0.], 0.).inside_world(1000.));
        assert!(!fits_world(&borders, [400., 50., 0.], 0., 1000));
        assert!(fits_world(&borders, [200., 50., 0.], 0., 1000));
        assert!(fits_world(&borders, [400., 50., 0.], 180., 1000));
    }
}
