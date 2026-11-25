//! Explicit, versioned orchestration of source-ordered ring/paved/dirt inputs.
//! Plans remain subject to the normal binding, terrain and mesh acceptance gates.
use crate::{
    Project, infrastructure, prefab_assets::Catalog, prefab_textures::Bindings,
    road_planning::Plan, source_road_handoff as handoff, source_road_inputs,
};
use papyrus_terrain::{
    monument_composition::Composed, path_geometry, path_terrain, placement::Solution,
    rail_reservations, road_costmap::CellGrid, source_road_layout as layout,
    source_road_ring as ring, source_road_route,
};
use serde::{Deserialize, Serialize};
use std::{cell::RefCell, collections::{BTreeMap, BTreeSet}, sync::Arc};
mod local_edits;
pub const LEGACY_VERSION: &str = "papyrus-ordered-road-planning-1";
pub const VERSION: &str = "papyrus-ordered-road-planning-2";
pub const RING_CLEARANCE_LEGACY_VERSION: &str = "papyrus-source-ring-clearance-1";
pub const RING_CLEARANCE_VERSION: &str = "papyrus-source-ring-clearance-2";
const RING_GEOMETRY_VERSION: &str = "papyrus-source-ring-geometry-1";

/// Road layout already checked by a completed composition. Keep the raw
/// junction targets and shaped controls together for the ring and its branches.
pub struct AcceptedRoadLayout {
    project: Project,
    monuments: [u8; 32],
    solution: Solution,
    catalog: String,
    bindings: String,
    layout: Arc<layout::AcceptedNetwork>,
}

impl AcceptedRoadLayout {
    pub fn capture(
        project: &Project,
        monuments: &Solution,
        catalog: &str,
        bindings: &str,
        maps: &Composed,
    ) -> Option<Self> {
        let layout = maps.roads.as_ref()?.source_route_layout.as_ref()?.clone();
        if !project.roads.as_ref().and_then(|roads| roads.entrance_launch).is_some_and(|launch| {
                launch.engine == papyrus_terrain::road_network::LaunchEngine::GlobalEarthwork
            })
        {
            return None;
        }
        Some(Self {
            project: project.clone(),
            monuments: crate::composition::layout_fingerprint(monuments),
            solution: monuments.clone(),
            catalog: catalog.into(),
            bindings: bindings.into(),
            layout,
        })
    }

    fn matches(&self, project: &Project, solution: &Solution, catalog: &str, bindings: &str) -> bool {
        let Some(added) = project.path_edits.strip_prefix(self.project.path_edits.as_slice()) else {
            return false;
        };
        if self.catalog != catalog || self.bindings != bindings || added.is_empty() {
            return false;
        }
        let mut recipe = project.clone();
        recipe.path_edits.truncate(self.project.path_edits.len());
        recipe == self.project && crate::composition::layout_fingerprint(solution) == self.monuments
    }

    fn matches_monument_move(&self, project: &Project, catalog: &str, bindings: &str) -> bool {
        if self.catalog != catalog || self.bindings != bindings
            || !project.path_edits.is_empty() || project.targets.len() != self.project.targets.len()
        {
            return false;
        }
        let mut recipe = project.clone();
        let mut moved = false;
        for (target, original) in recipe.targets.iter_mut().zip(&self.project.targets) {
            if target != original && target.max_displacement_m.is_some() {
                moved |= (target.x, target.z, target.yaw) != (original.x, original.z, original.yaw);
                target.x = original.x;
                target.z = original.z;
                target.yaw = original.yaw;
                target.max_displacement_m = original.max_displacement_m;
            }
        }
        moved && recipe == self.project
    }
}

struct RetainedRoads {
    accepted: Arc<AcceptedRoadLayout>,
    obstructions: Option<Vec<local_edits::Obstruction>>,
}

thread_local! {
    static ACCEPTED_ROAD_LAYOUT: RefCell<Option<Arc<AcceptedRoadLayout>>> = const { RefCell::new(None) };
}

/// Give one edit composition attempt the road layout from the last accepted map.
pub fn with_accepted_road_layout<T>(layout: Option<Arc<AcceptedRoadLayout>>, build: impl FnOnce() -> T) -> T {
    struct Restore(Option<Arc<AcceptedRoadLayout>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            ACCEPTED_ROAD_LAYOUT.with(|slot| slot.replace(self.0.take()));
        }
    }
    let _restore = Restore(ACCEPTED_ROAD_LAYOUT.with(|slot| slot.replace(layout)));
    build()
}

pub(crate) fn has_retained_layout() -> bool {
    ACCEPTED_ROAD_LAYOUT.with(|slot| slot.borrow().is_some())
}

fn route_with_budget_recovery(
    policy: layout::Policy,
    initial_work: usize,
    mut generate: impl FnMut(layout::Policy, usize) -> Result<layout::Outcome, String>,
    cancelled: &impl Fn() -> bool,
) -> Result<layout::Outcome, String> {
    if !(1..=128_000_000).contains(&initial_work) {
        return Err("Invalid initial road layout allowance".into());
    }
    let previous = match generate(policy, initial_work)? {
        layout::Outcome::Budget(audit) => audit,
        outcome => return Ok(outcome),
    };
    if cancelled() {
        return Ok(layout::Outcome::Cancelled(previous));
    }
    let queries = previous.entrance_queries + previous.layout_queries;
    if previous.work > initial_work || queries > policy.maximum_queries {
        return Err("Road layout exceeded its initial allowance".into());
    }
    if queries == policy.maximum_queries {
        return Ok(layout::Outcome::Budget(previous));
    }
    // Each source layout already reserves two billion work units globally.
    // Two road families can each spend at most 512 million, including failure.
    // Keep the ordinary first pass exact, then retry only an exhausted family.
    let maximum_work = initial_work + 384_000_000;
    let mut outcome = generate(
        layout::Policy {
            maximum_queries: policy.maximum_queries - queries,
            ..policy
        },
        maximum_work - previous.work,
    )?;
    let audit = match &mut outcome {
        layout::Outcome::Found(layout) => &mut layout.audit,
        layout::Outcome::Budget(audit) | layout::Outcome::Cancelled(audit) => audit,
    };
    audit.add_previous(&previous)?;
    if audit.work > maximum_work
        || audit.entrance_queries + audit.layout_queries > policy.maximum_queries
    {
        return Err("Road layout recovery exceeded its shared allowance".into());
    }
    audit.maximum_work = maximum_work;
    audit.policy = policy;
    Ok(outcome)
}

/// Keep the installed shaping program when its full-width strip is sound.
/// Otherwise retry that same source smoother before binding any child roads.
/// Raw routing nodes, costs, seeds and final terrain/mesh gates are unchanged.
fn shape_source_ring(
    settings: path_geometry::Settings,
    radius: f32,
    build: impl FnMut(path_geometry::Settings) -> Result<Option<ring::Shaped>, String>,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<(ring::Shaped, serde_json::Value)>, String> {
    shape_source_path(settings, radius, true, build, cancelled)
}
fn shape_source_path(
    settings: path_geometry::Settings,
    radius: f32,
    circular: bool,
    build: impl FnMut(path_geometry::Settings) -> Result<Option<ring::Shaped>, String>,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<(ring::Shaped, serde_json::Value)>, String> {
    match try_shape_source_path(settings, radius, circular, false, build, cancelled)? {
        ShapeOutcome::Clear(path, receipt) => Ok(Some((path, receipt))),
        ShapeOutcome::Cancelled => Ok(None),
        ShapeOutcome::Folded { diagnostic, .. } => Err(diagnostic),
    }
}

enum ShapeOutcome {
    Clear(ring::Shaped, serde_json::Value),
    Cancelled,
    Folded { work: usize, diagnostic: String },
}

fn try_shape_source_path(
    settings: path_geometry::Settings,
    radius: f32,
    circular: bool,
    prefer_level_strip: bool,
    mut build: impl FnMut(path_geometry::Settings) -> Result<Option<ring::Shaped>, String>,
    cancelled: &impl Fn() -> bool,
) -> Result<ShapeOutcome, String> {
    if !radius.is_finite()
        || !(0.05..=64.).contains(&radius)
        || settings.horizontal_iterations > 256
        || !(1..=32_000_000).contains(&settings.maximum_work)
    {
        return Err("Invalid source road geometry preflight".into());
    }
    let mut work = 0usize;
    let mut attempts = Vec::new();
    let mut previous = None;
    let mut fallback = None;
    for extra in [0, 1, 2, 4, 8, 16, 32] {
        if cancelled() {
            return Ok(ShapeOutcome::Cancelled);
        }
        let horizontal = settings
            .horizontal_iterations
            .saturating_add(extra)
            .min(256);
        if previous == Some(horizontal) {
            continue;
        }
        previous = Some(horizontal);
        let remaining = settings
            .maximum_work
            .checked_sub(work)
            .filter(|v| *v > 0)
            .ok_or("Source road geometry preflight exhausted its shared work allowance")?;
        let shaped = match build(path_geometry::Settings {
            horizontal_iterations: horizontal,
            maximum_work: remaining,
            ..settings
        }) {
            Ok(Some(path)) => path,
            Ok(None) => return Ok(ShapeOutcome::Cancelled),
            Err(_) if fallback.is_some() => {
                work = settings.maximum_work;
                break;
            }
            Err(error) => return Err(error),
        };
        if shaped.path.is_circular() != circular {
            return Err("Source road shaping changed its open/closed geometry".into());
        }
        let shape_work = shaped.total_work;
        work = work
            .checked_add(shape_work)
            .filter(|v| *v <= settings.maximum_work)
            .ok_or("Source road shaping exceeded its shared work allowance")?;
        let controls = shaped.path.points().len();
        if controls > settings.maximum_work - work {
            if fallback.is_some() {
                work = settings.maximum_work;
                break;
            }
            return Err("Source road strip preparation exhausted its work allowance".into());
        }
        work += controls;
        let before = work;
        let mut witness = None;
        let clear = match papyrus_terrain::road_strip::check_with_witness(
            &shaped.path,
            &vec![true; controls],
            radius,
            |amount| {
                work = work
                    .checked_add(amount)
                    .filter(|v| *v <= settings.maximum_work)
                    .ok_or("Source road strip check exhausted its work allowance")?;
                Ok::<_, String>(())
            },
            cancelled,
            |interval, point| {
                witness = Some(serde_json::json!({
                    "interval":interval,"point":point,
                    "controls":&shaped.path.points()[interval.saturating_sub(1)..(interval+3).min(controls)]
                }));
            },
        ) {
            Ok(value) => value,
            Err(_) if fallback.is_some() => {
                work = settings.maximum_work;
                break;
            }
            Err(error) => return Err(error),
        };
        let Some(clear) = clear else {
            return Ok(ShapeOutcome::Cancelled);
        };
        let level_clear = if clear && prefer_level_strip {
            match check_level_strip(&shaped.path, radius, &mut work, settings.maximum_work, cancelled)? {
                Some(clear) => Some(clear),
                None => return Ok(ShapeOutcome::Cancelled),
            }
        } else {
            None
        };
        attempts.push(
            serde_json::json!({"horizontal_iterations":horizontal,"shape_work":shape_work,
            "strip_work":work-before,"controls":controls,"strip_clear":clear,"fold":witness,
            "level_strip_clear":level_clear}),
        );
        if cancelled() {
            return Ok(ShapeOutcome::Cancelled);
        }
        if clear && (!prefer_level_strip || level_clear == Some(true)) {
            fallback = Some((shaped, horizontal));
            break;
        }
        if clear && fallback.is_none() {
            fallback = Some((shaped, horizontal));
        }
        // Retain an otherwise valid source candidate if the optional
        // projection exhausts the allowance. Final checks still decide.
        if fallback.is_some() && work == settings.maximum_work {
            break;
        }
    }
    if let Some((mut shaped, horizontal)) = fallback {
        shaped.total_work = work;
        return Ok(ShapeOutcome::Clear(
            shaped,
            serde_json::json!({"engine":if circular { RING_GEOMETRY_VERSION } else { "papyrus-source-road-geometry-1" },
            "requested_horizontal_iterations":settings.horizontal_iterations,
            "selected_horizontal_iterations":horizontal,"recovered":horizontal!=settings.horizontal_iterations,
            "radius":radius,"attempts":attempts,"work":work,
            "raw_nodes_preserved":true,"final_route_and_mesh_checks_required":true}),
        ));
    }
    Ok(ShapeOutcome::Folded {
        work,
        diagnostic: format!(
            "Source road retains folded surface strips after bounded source smoothing. {}",
            serde_json::json!({"radius":radius,"circular":circular,"first":attempts.first(),"last":attempts.last()})
        ),
    })
}

/// Source interpolation uses 3D tangents. Terrain fitting can flatten a
/// grade and expose a sharp horizontal turn. This probe only ranks candidates.
fn check_level_strip(
    path: &path_geometry::Path,
    radius: f32,
    work: &mut usize,
    maximum_work: usize,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<bool>, String> {
    let controls = path.points().len();
    if maximum_work - *work < controls * 3 {
        return Ok(Some(false));
    }
    *work += controls * 2; // Level controls and affected flags.
    let points: Vec<_> = path.points().iter().map(|point| [point[0], 0., point[2]]).collect();
    let Some(level) = path_geometry::Path::from_controls(
        &points, maximum_work - *work, cancelled,
    ).map_err(|error| format!("Level road strip preparation: {error:?}"))? else {
        return Ok(None);
    };
    *work += level.work();
    let result = papyrus_terrain::road_strip::check(
        &level, &vec![true; controls], radius,
        |amount| {
            if amount > maximum_work - *work { return Err(()); }
            *work += amount;
            Ok(())
        },
        cancelled,
    );
    Ok(result.unwrap_or(Some(false)))
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RingClearance {
    pub engine: String,
    pub radius_cells: usize,
}
impl RingClearance {
    fn validate(&self) -> Result<(), String> {
        if !matches!(
            self.engine.as_str(),
            RING_CLEARANCE_VERSION | RING_CLEARANCE_LEGACY_VERSION
        ) || !(1..=2).contains(&self.radius_cells)
        {
            return Err("Unsupported source ring clearance policy".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub engine: String,
    pub assets_sha256: String,
    pub roles_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ring_clearance: Option<RingClearance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terrain_entrances: Option<String>,
}
impl Policy {
    pub fn bind(inputs: &crate::road_extract::OrderedInputs) -> Self {
        Self {
            engine: VERSION.into(),
            assets_sha256: inputs.assets.sha256.clone(),
            roles_sha256: inputs.roles.reference_sha256().into(),
            ring_clearance: None,
            terrain_entrances: None,
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.engine.as_str(), VERSION | LEGACY_VERSION)
            || [&self.assets_sha256, &self.roles_sha256]
                .iter()
                .any(|s| !crate::source_road_assets::valid_digest(s))
        {
            return Err("Unsupported ordered road policy or input fingerprint".into());
        }
        if let Some(p) = &self.ring_clearance {
            p.validate()?;
        }
        if self.terrain_entrances.as_deref().is_some_and(|v| {
            !matches!(
                v,
                source_road_route::TERRAIN_ENTRANCE_VERSION
                    | source_road_route::TERRAIN_ENTRANCE_ROUTED_VERSION
                    | source_road_route::TERRAIN_ENTRANCE_BOOTSTRAP_VERSION
                    | source_road_route::TERRAIN_ENTRANCE_PINNED_VERSION
            )
        }) {
            return Err("Unsupported terrain entrance policy".into());
        }
        Ok(())
    }
}
/// Reattach authored X/Z after source resampling. The fitted terrain Y and all
/// interior controls survive. Only the explicit pinned policy calls this.
fn pin_authored_endpoints(
    segment: &layout::Segment,
    endpoints: &[source_road_inputs::Endpoint],
    path: &path_geometry::Path,
    maximum_shift: f32,
    maximum_work: usize,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<path_geometry::Path>, String> {
    if !maximum_shift.is_finite() || !(0.1..=100.).contains(&maximum_shift) {
        return Err("Invalid source endpoint correction distance".into());
    }
    if cancelled() {
        return Ok(None);
    }
    let mut terminals = [None, None];
    for (side, connector) in [segment.origin, segment.target].into_iter().enumerate() {
        let Some(connector) = connector else {
            continue;
        };
        let endpoint = endpoints
            .get(connector)
            .ok_or("Missing authored source endpoint")?;
        let expected_cell = segment
            .lattice
            .cell([endpoint.world[0], endpoint.world[2]])?;
        let node = if side == 0 {
            segment.nodes.first()
        } else {
            segment.nodes.last()
        };
        if node.is_none_or(|node| node.cell != expected_cell) {
            return Err("Authored source endpoint differs from its raw terminal cell".into());
        }
        let index = if side == 0 {
            0
        } else {
            path.points().len() - 1
        };
        let mut point = path.points()[index];
        let size = segment.lattice.size();
        let [x, z] = [endpoint.world[0], endpoint.world[2]]
            .map(|value| -size * 0.5 + ((value + size * 0.5) * (1. / size)) * size);
        if (point[0] - x).hypot(point[2] - z) > maximum_shift {
            return Err("Source endpoint correction exceeds its resampling interval".into());
        }
        point[0] = x;
        point[2] = z;
        terminals[side] = Some(point);
    }
    path.reattach_endpoints(terminals, maximum_work, cancelled)
        .map_err(|error| format!("Source endpoint correction failed: {error:?}"))
}

/// A recovered entrance and its connecting walk can share lattice cells.
/// Collapse exact revisits only when every other road can still attach to
/// the same terminal cell and both ends of this open segment survive.
fn erase_open_road_retraces(
    segment: &layout::Segment,
    terminals: &BTreeSet<[usize; 2]>,
    cancelled: &impl Fn() -> bool,
) -> Option<layout::Segment> {
    let mut indices = BTreeMap::new();
    let mut nodes: Vec<ring::RawNode> = Vec::with_capacity(segment.nodes.len());
    for node in &segment.nodes {
        if cancelled() {
            return None;
        }
        if let Some(&index) = indices.get(&node.cell) {
            for removed in nodes.drain(index + 1..) {
                indices.remove(&removed.cell);
            }
        } else {
            indices.insert(node.cell, nodes.len());
            nodes.push(*node);
        }
    }
    if nodes.len() < 2
        || nodes.len() == segment.nodes.len()
        || segment
            .nodes
            .iter()
            .any(|node| terminals.contains(&node.cell) && !indices.contains_key(&node.cell))
    {
        return None;
    }
    Some(layout::Segment {
        nodes,
        lattice: segment.lattice,
        origin: segment.origin,
        target: segment.target,
    })
}

fn shape_open_road(
    segment: &mut layout::Segment,
    terminals: &BTreeSet<[usize; 2]>,
    settings: path_geometry::Settings,
    radius: f32,
    prefer_level_strip: bool,
    build: impl Fn(&layout::Segment, path_geometry::Settings) -> Result<Option<ring::Shaped>, String>,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<(ring::Shaped, serde_json::Value)>, String> {
    let (work, diagnostic) =
        match try_shape_source_path(settings, radius, false, prefer_level_strip, |s| build(segment, s), cancelled)? {
            ShapeOutcome::Clear(path, receipt) => return Ok(Some((path, receipt))),
            ShapeOutcome::Cancelled => return Ok(None),
            ShapeOutcome::Folded { work, diagnostic } => (work, diagnostic),
        };
    let count = segment.nodes.len();
    let index_depth = count.max(terminals.len()).max(1).ilog2() as usize + 1;
    let previous_work = work + count * (6 * index_depth);
    let Some(remaining) = settings
        .maximum_work
        .checked_sub(previous_work)
        .filter(|&v| v > 0)
    else {
        return Err(diagnostic);
    };
    let simplified = erase_open_road_retraces(segment, terminals, cancelled);
    if cancelled() {
        return Ok(None);
    }
    let Some(simplified) = simplified else {
        return Err(diagnostic);
    };
    let result = try_shape_source_path(
        path_geometry::Settings {
            maximum_work: remaining,
            ..settings
        },
        radius,
        false,
        prefer_level_strip,
        |s| build(&simplified, s),
        cancelled,
    )?;
    let (mut shape, mut receipt) = match result {
        ShapeOutcome::Clear(path, receipt) => (path, receipt),
        ShapeOutcome::Cancelled => return Ok(None),
        ShapeOutcome::Folded { diagnostic, .. } => return Err(diagnostic),
    };
    shape.total_work += previous_work;
    receipt["work"] = shape.total_work.into();
    receipt["recovered"] = true.into();
    receipt["raw_nodes_preserved"] = false.into();
    receipt["retraced_nodes_removed"] = (count - simplified.nodes.len()).into();
    *segment = simplified;
    Ok(Some((shape, receipt)))
}

pub(crate) struct Prepared {
    pub plan: Plan,
    pub ring_costs: Option<CellGrid>,
    pub ring_clearance: Option<serde_json::Value>,
    pub routing_preferences: Vec<serde_json::Value>,
    pub terrain_entrances: Vec<serde_json::Value>,
    pub family_costs: Vec<FamilyCosts>,
    pub family_geometry: Vec<serde_json::Value>,
}
pub(crate) struct FamilyCosts {
    pub hierarchy: u32,
    pub grid: CellGrid,
}
/// Entrance recovery can traverse original source cost zeros. Preserve newly
/// excluded retry cells separately so it cannot bypass a rejected footprint.
fn entrance_retry_mask(
    existing: &[u8],
    retry: &[u8],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Vec<u8>>, String> {
    if cancelled() {
        return Ok(None);
    }
    if existing.is_empty()
        || existing.len() > 1_048_576
        || retry.len() != existing.len()
        || existing.len() > maximum_work
    {
        return Err("Invalid entrance retry-mask dimensions or work allowance".into());
    }
    let mut mask = Vec::with_capacity(existing.len());
    for (i, (&excluded, &additional)) in existing.iter().zip(retry).enumerate() {
        if i % 4096 == 0 && cancelled() {
            return Ok(None);
        }
        if excluded > 1 || additional > 1 {
            return Err("Invalid entrance retry-mask value".into());
        }
        mask.push(excluded | additional);
    }
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(mask))
}
fn coastal_retry_mask(
    topology: &[i32],
    resolution: usize,
    lattice: papyrus_terrain::road_costmap::CellLattice,
    ocean: i32,
    radius: f32,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<(Vec<u8>, usize)>, String> {
    if !(2..=2048).contains(&resolution)
        || topology.len() != resolution * resolution
        || ocean <= 0
        || !(ocean as u32).is_power_of_two()
        || !radius.is_finite()
        || !(0.1..=64.).contains(&radius)
    {
        return Err("Invalid coastal entrance-clearance inputs or work allowance".into());
    }
    let n = lattice.resolution();
    let stride = resolution + 1;
    let mut work = stride * resolution + n * n;
    if work > maximum_work {
        return Err("Coastal entrance-clearance work exhausted".into());
    }
    // Row prefixes query the circular footprint without expanding every ocean
    // pixel into a square or repeatedly painting large overlapping regions.
    let mut prefix = vec![0u32; stride * resolution];
    for z in 0..resolution {
        if cancelled() {
            return Ok(None);
        }
        for x in 0..resolution {
            work += 1;
            if work > maximum_work {
                return Err("Coastal entrance-clearance work exhausted".into());
            }
            let i = z * stride + x;
            prefix[i + 1] = prefix[i] + u32::from(topology[z * resolution + x] & ocean != 0);
        }
    }
    let mut mask = vec![0; n * n];
    let radius = f64::from(radius) / f64::from(lattice.size()) * resolution as f64;
    for (i, excluded) in mask.iter_mut().enumerate() {
        if i % 1024 == 0 && cancelled() {
            return Ok(None);
        }
        work += 1;
        if work > maximum_work {
            return Err("Coastal entrance-clearance work exhausted".into());
        }
        let [px, pz] = lattice
            .point([i % n, i / n])
            .ok_or("Invalid coastal entrance lattice")?
            .map(|v| (f64::from(v) / f64::from(lattice.size()) + 0.5) * resolution as f64);
        let low = (pz - radius - 1.).ceil().max(0.) as usize;
        let high = (pz + radius).floor().min((resolution - 1) as f64) as usize;
        for z in low..=high {
            work += 3;
            if work > maximum_work {
                return Err("Coastal entrance-clearance work exhausted".into());
            }
            let dz = (z as f64 - pz).max(pz - (z + 1) as f64).max(0.);
            let remaining = radius * radius - dz * dz;
            if remaining < 0. {
                continue;
            }
            let reach = remaining.sqrt();
            let x0 = (px - reach - 1.).ceil().max(0.) as usize;
            let x1 = (px + reach).floor().min((resolution - 1) as f64) as usize;
            if x0 <= x1 && prefix[z * stride + x1 + 1] != prefix[z * stride + x0] {
                *excluded = 1;
                break;
            }
        }
    }
    if cancelled() {
        return Ok(None);
    }
    Ok(Some((mask, work)))
}
/// Expand protected or low terrain near each confirmed collision. A
/// point exclusion alone lets later curves clip other parts of the same edge.
fn terrain_retry_mask(
    layers: &papyrus_terrain::native_layers::Layers,
    lattice: papyrus_terrain::road_costmap::CellLattice,
    markers: &[crate::road_replanning::Marker],
    hierarchy: u32,
    low_ground: Option<&[i32]>,
    maximum_work: usize,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<(Vec<u8>, usize)>, String> {
    let resolution = layers.surface_resolution;
    let cells = resolution.saturating_mul(resolution);
    if !(2..=2048).contains(&resolution)
        || layers.topology.len() != cells
        || low_ground.is_some_and(|mask| mask.len() != cells)
        || cells > maximum_work
        || layers.size as f32 != lattice.size()
    {
        return Err("Invalid terrain retry raster or allowance".into());
    }
    if cancelled() {
        return Ok(None);
    }
    let mut local = vec![0; cells];
    let mut work = cells;
    let mut radius = 0.1_f32;
    let scale = resolution as f64 / f64::from(layers.size);
    for marker in markers.iter().filter(|m| {
        m.hierarchy == hierarchy
            && (m.protected_topology != 0 || (m.low_ground && low_ground.is_some()))
    }) {
        let low = low_ground.filter(|_| marker.low_ground);
        let extent = (f64::from(marker.radius) * 8.).clamp(32., 128.);
        radius = radius.max(marker.radius);
        let bound = |value: f64| ((value + f64::from(layers.size) * 0.5) * scale)
            .floor().clamp(0., (resolution - 1) as f64) as usize;
        let [x, z] = marker.witness.position;
        let [x0, x1] = [bound(x - extent), bound(x + extent)];
        let [z0, z1] = [bound(z - extent), bound(z + extent)];
        work = work.checked_add((x1 - x0 + 1) * (z1 - z0 + 1))
            .filter(|&work| work <= maximum_work)
            .ok_or("Terrain retry neighborhood exhausted its work allowance")?;
        for z in z0..=z1 {
            if cancelled() {
                return Ok(None);
            }
            for x in x0..=x1 {
                let i = z * resolution + x;
                local[i] |= i32::from(layers.topology[i] & marker.protected_topology != 0
                    || low.is_some_and(|mask| mask[i] != 0));
            }
        }
    }
    let Some((mask, dilation_work)) = coastal_retry_mask(
        &local,
        resolution,
        lattice,
        1,
        (radius + lattice.spacing() * 0.5).min(64.),
        maximum_work - work,
        cancelled,
    )?
    else {
        return Ok(None);
    };
    Ok(Some((mask, work + dilation_work)))
}

// A private low-ground raster. Native height vertices mark every surface cell
// they touch, enclosing the bilinear terrain without changing source topology.
#[cfg(test)]
fn coastal_ground_guidance(
    layers: &papyrus_terrain::native_layers::Layers,
    ocean: i32,
    minimum_height: f32,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<(Vec<i32>, usize)>, String> {
    Ok(coastal_ground_masks(layers, ocean, minimum_height, None, maximum_work, &cancelled)?
        .map(|(low, _, work)| (low, work)))
}

fn coastal_ground_masks(
    layers: &papyrus_terrain::native_layers::Layers,
    ocean: i32,
    minimum_height: f32,
    deep_height: Option<f32>,
    maximum_work: usize,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<(Vec<i32>, Option<Vec<i32>>, usize)>, String> {
    let (hr, sr) = papyrus_terrain::native_layers::dimensions(layers.size)?;
    if layers.height_resolution != hr
        || layers.surface_resolution != sr
        || layers.height.len() != hr * hr
        || layers.topology.len() != sr * sr
        || ocean <= 0
        || !(ocean as u32).is_power_of_two()
        || !minimum_height.is_finite()
        || minimum_height.abs() > 500.
        || deep_height.is_some_and(|deep| !(-500. ..=minimum_height).contains(&deep))
    {
        return Err("Invalid coastal ground guidance inputs".into());
    }
    if cancelled() {
        return Ok(None);
    }
    let mut work = layers.topology.len() * (1 + usize::from(deep_height.is_some()));
    if work > maximum_work {
        return Err("Coastal ground guidance work exhausted".into());
    }
    let mut topology = vec![0; layers.topology.len()];
    let mut deep = deep_height.map(|height| (height, vec![0; layers.topology.len()]));
    for z in 0..sr {
        if cancelled() {
            return Ok(None);
        }
        for x in 0..sr {
            let mut lowest = i16::MAX;
            for hz in z * (hr - 1) / sr..=(z + 1) * (hr - 1) / sr {
                for hx in x * (hr - 1) / sr..=(x + 1) * (hr - 1) / sr {
                    if work == maximum_work {
                        return Err("Coastal ground guidance work exhausted".into());
                    }
                    work += 1;
                    lowest = lowest.min(layers.height[hz * hr + hx]);
                }
            }
            let lowest = papyrus_terrain::erosion::decode(lowest);
            let i = z * sr + x;
            if lowest <= minimum_height {
                topology[i] = ocean;
            }
            if let Some((threshold, cells)) = &mut deep
                && lowest <= *threshold
            {
                cells[i] = ocean;
            }
        }
    }
    Ok((!cancelled()).then_some((topology, deep.map(|(_, cells)| cells), work)))
}
fn entrance_hole_mask(
    alpha: &[u8],
    resolution: usize,
    lattice: papyrus_terrain::road_costmap::CellLattice,
    radius: f32,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<(Vec<u8>, usize)>, String> {
    if !(2..=2048).contains(&resolution)
        || alpha.len() != resolution * resolution
        || !radius.is_finite()
        || !(0.1..=64.).contains(&radius)
    {
        return Err("Invalid entrance hole-clearance inputs".into());
    }
    let n = lattice.resolution();
    let mut work = alpha.len() + n * n;
    if work > maximum_work {
        return Err("Entrance hole-clearance work exhausted".into());
    }
    if cancelled() {
        return Ok(None);
    }
    let mut mask = vec![0; n * n];
    let expansion = f64::from(radius) / f64::from(lattice.spacing());
    let scale = n as f64 / resolution as f64;
    // Alpha samples cover cells of the native surface grid. Include the road
    // half-width on every side before mapping them to source cell centres.
    for (i, &value) in alpha.iter().enumerate() {
        if i % 4096 == 0 && cancelled() {
            return Ok(None);
        }
        if value >= 128 {
            continue;
        }
        let ranges = [i % resolution, i / resolution].map(|cell| {
            let low = (cell as f64 * scale - expansion - 0.5).ceil().max(0.);
            let high = ((cell + 1) as f64 * scale + expansion - 0.5)
                .floor()
                .min((n - 1) as f64);
            (low <= high).then_some([low as usize, high as usize])
        });
        let [Some([x0, x1]), Some([z0, z1])] = ranges else {
            continue;
        };
        for z in z0..=z1 {
            for x in x0..=x1 {
                if work == maximum_work {
                    return Err("Entrance hole-clearance work exhausted".into());
                }
                work += 1;
                mask[z * n + x] = 1;
            }
        }
    }
    Ok(Some((mask, work)))
}
/// Inputs captured before any attempt reserves a road or applies retry feedback.
pub(crate) struct RetryInputs<'m> {
    ordered: crate::road_extract::OrderedInputs,
    placement: Vec<u8>,
    ring_costs: Option<CellGrid>,
    terrain: papyrus_terrain::road_costmap::CellTerrain<'m>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare<'m>(
    project: &Project,
    solution: &Solution,
    catalog: &Catalog,
    bindings: &Bindings,
    maps: &'m Composed,
    source: &infrastructure::Source,
    rails: Option<&crate::retained_rail_scene::Receipt>,
    policy: &Policy,
    retained: &mut Option<RetryInputs<'m>>,
    preferences: &[crate::road_replanning::Marker],
    cancel: &impl Fn() -> bool,
) -> Result<Option<Prepared>, crate::road_planning::Failure> {
    policy.validate()?;
    if cancel() {
        return Ok(None);
    }
    let recipe = project.roads.as_ref().ok_or("Missing road recipe")?;
    let costs = recipe
        .access_costmap
        .as_ref()
        .ok_or("Ordered roads require bound access costs")?;
    costs.check(source)?;
    if let Some(ring) = &recipe.ring_costmap {
        ring.check(source)?;
        if ring != costs {
            return Err("Ordered road families require the same bound source cost rules".into());
        }
    }
    let inputs = if let Some(inputs) = retained.as_mut() {
        inputs
    } else {
        let Some(ordered) = crate::road_extract::prepare_ordered(
            bindings.installation_root(),
            catalog,
            source,
            Some(&policy.assets_sha256),
            cancel,
        )? else {
            return if cancel() {
                Ok(None)
            } else {
                Err("Selected Rust code has no verified ordered road adapter".into())
            };
        };
        if ordered.roles.reference_sha256() != policy.roles_sha256 {
            return Err("Ordered road enum roles differ from the saved recipe".into());
        }
        let Some(blockers) = costs.prepare_blockers(project, solution, catalog, bindings, source, cancel)? else {
            return Ok(None);
        };
        let Some(placement) = crate::road_cost_source::with_retained_placement(
            project, maps, rails, blockers, cancel,
        )? else {
            return Ok(None);
        };
        let spacing = costs
            .joins
            .as_ref()
            .ok_or("Missing source cell lattice policy")?
            .source_grid_spacing;
        let lattice = papyrus_terrain::road_costmap::CellLattice::new(project.size as f32, spacing)?;
        let terrain = papyrus_terrain::road_costmap::CellTerrain::new(
            &maps.layers, costs.policy.clone(), lattice,
        )?;
        retained.insert(RetryInputs {
            ordered,
            placement,
            ring_costs: None,
            terrain,
        })
    };
    let retained_layout = ACCEPTED_ROAD_LAYOUT.with(|slot| {
        let mut slot = slot.borrow_mut();
        if !preferences.is_empty() {
            return None;
        }
        let accepted = slot.as_ref()?;
        let obstructions = if accepted.matches(project, solution, &catalog.sha256, &bindings.sha256) {
            None
        } else if accepted.matches_monument_move(project, &catalog.sha256, &bindings.sha256) {
            let moved = local_edits::moved_footprints(&accepted.solution, solution);
            if moved.len() > 16 {
                return None;
            }
            Some(moved)
        } else {
            return None;
        };
        slot.take().map(|accepted| RetainedRoads { accepted, obstructions })
    });
    let uses_retained_layout = retained_layout.is_some();
    route_with_preferences(
        project,
        solution,
        maps,
        source,
        &inputs.ordered,
        inputs.placement.clone(),
        &mut inputs.ring_costs,
        &mut inputs.terrain,
        preferences,
        retained_layout,
        cancel,
    ).map_err(|mut error| {
        error.rejected_retained_layout = uses_retained_layout;
        error
    })
}
/// Pure data orchestration shared with headless verification. No scratch terrain
/// writes, cache lookups or source identity substitution occur here.
#[allow(clippy::too_many_arguments)]
fn route_with_preferences(
    project: &Project,
    solution: &Solution,
    maps: &Composed,
    source: &infrastructure::Source,
    inputs: &crate::road_extract::OrderedInputs,
    mut placement: Vec<u8>,
    pristine_ring: &mut Option<CellGrid>,
    terrain_costs: &mut papyrus_terrain::road_costmap::CellTerrain<'_>,
    preferences: &[crate::road_replanning::Marker],
    retained_layout: Option<RetainedRoads>,
    cancel: &impl Fn() -> bool,
) -> Result<Option<Prepared>, crate::road_planning::Failure> {
    let progress = crate::loading_progress::begin("Planning source roads", None);
    macro_rules! ready {
        ($value:expr) => {
            match $value? {
                Some(v) => v,
                None => return Ok(None),
            }
        };
    }
    let recipe = project
        .roads
        .as_ref()
        .ok_or("Missing ordered road recipe")?;
    let costs = recipe
        .access_costmap
        .as_ref()
        .ok_or("Missing source road costs")?;
    costs.check(source)?;
    let joins = costs
        .joins
        .as_ref()
        .ok_or("Missing source cell lattice policy")?;
    let reserves = costs
        .path_reservations
        .as_ref()
        .ok_or("Missing source road placement policy")?;
    reserves.validate()?;
    let ring_policy = source.profile.ring.ok_or("Missing source ring policy")?;
    let shaping = source
        .profile
        .shaping
        .ok_or("Missing source shaping policy")?;
    shaping.validate()?;
    let shape = path_geometry::Settings {
        horizontal_iterations: shaping.horizontal_iterations,
        vertical_iterations: shaping.vertical_iterations,
        resample_distance: shaping.resample_distance,
        maximum_work: shaping.maximum_work,
    };
    let height = path_terrain::Grid {
        size: project.size as f32,
        resolution: maps.layers.height_resolution,
        origin_y: reserves.terrain_origin_y,
        vertical_span: reserves.terrain_height,
        height: &maps.layers.height,
    };
    // These are versioned Mapper search limits, not claims about native method literals.
    let maximum_work = 128_000_000;
    let search = source_road_route::Settings {
        maximum_expansions: 100_000,
        maximum_work,
        directional_penalty: joins.directional_penalty,
        directional_radius: joins.directional_radius_cells,
    };
    let layout_policy = layout::Policy {
        network_sample_stride: ring_policy.network_sample_stride,
        push_multiplier: 50_000,
        maximum_queries: 8192,
        maximum_node_storage: 262144,
    };
    let reserve_policy = |main| rail_reservations::Policy {
        engine: rail_reservations::VERSION.into(),
        width: if main {
            reserves.main_road_width
        } else {
            reserves.width
        },
        sample_spacing: reserves.sample_spacing,
        terrain_origin_y: reserves.terrain_origin_y,
        terrain_height: reserves.terrain_height,
        spline: reserves.spline,
    };
    let mut ring_layout = None;
    let mut ring_costs = None;
    let mut ring_shaping_work = 0usize;
    let mut coastal_ring_work = 0usize;
    let mut clearance_receipt = None;
    let mut routing_preferences = Vec::new();
    let mut terrain_entrances = Vec::new();
    let terrain_engine = recipe
        .staged
        .as_ref()
        .and_then(|s| s.layout.as_ref())
        .and_then(|p| p.terrain_entrances.as_deref());
    let terrain_guidance = terrain_engine.is_some();
    let pinned_endpoints =
        terrain_engine == Some(source_road_route::TERRAIN_ENTRANCE_PINNED_VERSION);
    let bootstrap = pinned_endpoints
        || terrain_engine == Some(source_road_route::TERRAIN_ENTRANCE_BOOTSTRAP_VERSION);
    let guided_routes =
        bootstrap || terrain_engine == Some(source_road_route::TERRAIN_ENTRANCE_ROUTED_VERSION);
    let clearance = recipe
        .staged
        .as_ref()
        .and_then(|p| p.layout.as_ref())
        .and_then(|p| p.ring_clearance.as_ref());
    let source_ring = recipe
        .staged
        .as_ref()
        .is_some_and(|p| crate::road_generation::uses_source_cost_guidance(&p.engine));
    let early_coastal = costs.coastal_entrances.as_deref()
        == Some(crate::road_cost_source::GROUND_GUIDED_COASTAL_ENTRANCES_VERSION);
    let coastal_retry = if source_ring && (early_coastal || !preferences.is_empty()) {
        let ocean = source
            .profile
            .topology_flags
            .as_ref()
            .and_then(|flags| flags.get("Ocean"))
            .copied()
            .and_then(|flag| i32::try_from(flag).ok())
            .filter(|&flag| flag > 0 && (flag as u32).is_power_of_two())
            .ok_or("Missing installation Ocean topology role")?;
        let ownership = source
            .profile
            .entrances
            .ok_or("Missing source entrance ownership")?;
        let sr = maps.layers.surface_resolution;
        if !(2..=2048).contains(&sr) || maps.layers.topology.len() != sr * sr {
            return Err("Invalid coastal retry topology dimensions".into());
        }
        let encountered = preferences.iter().any(|marker| {
            let p = marker.witness.position;
            if !marker.exclude || marker.obstacle.is_some() || p.iter().any(|v| !v.is_finite()) {
                return false;
            }
            let cell = p.map(|v| {
                ((v / f64::from(project.size) + 0.5) * sr as f64).clamp(0., (sr - 1) as f64)
                    as usize
            });
            maps.layers.topology[cell[1] * sr + cell[0]] & ocean != 0
        });
        ((early_coastal || encountered)
            && source.profile.protected_topology & ocean != 0
            && ownership.owned_topology & ocean == 0)
            .then_some(ocean)
    } else {
        None
    };
    let mut coastal_ground_work = 0;
    let mut deep_ground = None;
    let coastal_ground = if early_coastal && let Some(ocean) = coastal_retry {
        let minimum = source
            .profile
            .entrances
            .ok_or("Missing source entrance height")?
            .minimum_height;
        let wet_retry = preferences.iter().any(|marker| marker.exclude && marker.low_ground);
        let (topology, deep, work) = ready!(coastal_ground_masks(
            &maps.layers,
            ocean,
            minimum,
            wet_retry.then_some(minimum - recipe.maximum_cut_fill),
            maximum_work,
            cancel
        ));
        coastal_ground_work = work;
        deep_ground = deep;
        Some(topology)
    } else {
        None
    };
    let coastal_topology = coastal_ground.as_deref().unwrap_or(&maps.layers.topology);
    let ring_grid = if recipe.main_ring.unwrap_or(true) {
        let grid = if let Some(grid) = pristine_ring.as_ref() {
            grid.clone()
        } else {
            let grid = ready!(terrain_costs.build(
                &placement,
                project.seeds[3].value,
                false,
                costs.maximum_work,
                cancel
            ));
            *pristine_ring = Some(grid.clone());
            grid
        };
        Some(grid)
    } else {
        None
    };
    if let Some(grid) = ring_grid.as_ref().filter(|_| retained_layout.is_none()) {
        progress.reporter().stage("Routing main road ring");
        let guarded = if source_ring {
            // Source routing already prices blocked terrain. Expanding every
            // blocked cell is an additional Mapper constraint and can collapse
            // a full-world ring into a small closed subwalk.
            clearance_receipt = Some(serde_json::json!({
                "engine":"papyrus-source-ring-routing-1","source_grid_unexpanded":true,
                "extra_blocked_cells":0,"mask_work":0,"source_cost_guidance_only":true,
                "horizontal_iterations":shape.horizontal_iterations
            }));
            None
        } else if let Some(policy) = clearance {
            policy.validate()?;
            let (guarded, added, work) = ready!(papyrus_terrain::road_costmap::guard_ring(
                grid,
                policy.radius_cells,
                maximum_work,
                cancel
            ));
            clearance_receipt = Some(
                serde_json::json!({"engine":policy.engine,"radius_cells":policy.radius_cells,
                "extra_blocked_cells":added,"mask_work":work,"original_source_costs_retained_for_acceptance":true}),
            );
            Some(guarded)
        } else {
            None
        };
        let mut routing = guarded.unwrap_or_else(|| grid.clone());
        if early_coastal && let Some(ocean) = coastal_retry {
            let (mask, work) = ready!(coastal_retry_mask(
                coastal_topology,
                maps.layers.surface_resolution,
                routing.lattice,
                ocean,
                ring_policy.width * 0.5 + source.profile.shoulder,
                maximum_work,
                cancel
            ));
            let mut added = 0;
            for (i, (cost, &excluded)) in routing.grid.costs.iter_mut().zip(&mask).enumerate() {
                if i % 4096 == 0 && cancel() {
                    return Ok(None);
                }
                if excluded != 0 {
                    added += usize::from(*cost != 0);
                    *cost = 0;
                }
            }
            coastal_ring_work = work
                .checked_add(mask.len())
                .filter(|&w| w <= maximum_work)
                .ok_or("Coastal ring guidance work exhausted")?;
            if let Some(receipt) = &mut clearance_receipt {
                receipt["source_grid_unexpanded"] = false.into();
                receipt["extra_blocked_cells"] = added.into();
                receipt["mask_work"] = (work + mask.len()).into();
                receipt["coastal_width_guidance"] = true.into();
                receipt["coastal_ground_guidance_work"] = coastal_ground_work.into();
                receipt["original_source_costs_retained_for_acceptance"] = true.into();
            }
        }
        if !preferences.is_empty() {
            let applied = ready!(crate::road_replanning::apply_cell_for(
                &mut routing,
                preferences,
                5000,
                maximum_work,
                0,
                cancel
            ));
            routing_preferences.push(applied.report);
        }
        match ring::assemble(
            &routing,
            ring_policy,
            joins,
            source_road_route::Settings {
                maximum_expansions: 250_000,
                ..search
            },
            maximum_work,
            cancel,
        )? {
            ring::Outcome::Found(mut raw) => {
                #[cfg(feature = "road-diagnostics")]
                capture_ring_if_requested(&raw, grid, &routing)?;
                if clearance.is_some_and(|p| p.engine == RING_CLEARANCE_VERSION) {
                    let remaining = maximum_work.saturating_sub(raw.audit.work);
                    let selected = ready!(papyrus_terrain::source_road_ring_cycle::select(
                        &raw.nodes,
                        raw.lattice,
                        remaining,
                        cancel
                    ));
                    raw.audit.work += selected.audit.work;
                    if let Some(receipt) = &mut clearance_receipt {
                        receipt["cycle"] =
                            serde_json::to_value(&selected.audit).map_err(|e| e.to_string())?;
                    }
                    raw.nodes = selected.nodes;
                }
                let shaped = if !source_ring && clearance.is_some() {
                    let mut accepted = None;
                    let mut tried = Vec::new();
                    for horizontal in [
                        shape.horizontal_iterations,
                        shape.horizontal_iterations / 2,
                        1,
                        0,
                    ] {
                        if horizontal > shape.horizontal_iterations || tried.contains(&horizontal) {
                            continue;
                        }
                        tried.push(horizontal);
                        let mut shaped = ready!(raw.shape(
                            path_geometry::Settings {
                                horizontal_iterations: horizontal,
                                ..shape
                            },
                            |uv| path_terrain::sample_height_uv(height, uv),
                            cancel
                        ));
                        // The source resampler can leave a tiny duplicate-seam
                        // drift. Native Y smoothing copies the entire first
                        // Vector3 onto that duplicate. Normalize before any
                        // indexed child junction is bound, then check clearance.
                        let points = shaped.path.points();
                        let seam = points[0]
                            .iter()
                            .zip(points.last().unwrap())
                            .map(|(a, b)| (f64::from(*a) - f64::from(*b)).powi(2))
                            .sum::<f64>()
                            .sqrt();
                        let heights = points.iter().map(|p| p[1]).collect::<Vec<_>>();
                        let allowance = shape
                            .maximum_work
                            .checked_sub(shaped.total_work)
                            .filter(|&v| v > 0)
                            .ok_or("Ring seam preparation work exhausted")?;
                        let sealed = ready!(
                            shaped
                                .path
                                .refit_heights(
                                    &heights,
                                    0,
                                    &vec![1.; heights.len()],
                                    allowance,
                                    cancel
                                )
                                .map_err(|e| format!("Ring seam preparation: {e:?}"))
                        );
                        shaped.total_work += sealed.work();
                        shaped.path = sealed;
                        let audit = ready!(
                            papyrus_terrain::road_smoothing::curve_cell_cost_check(
                                &shaped.path,
                                &grid.grid.costs,
                                grid.lattice,
                                1.,
                                2_000_000,
                                cancel
                            )
                            .map_err(|e| format!("Guarded ring clearance: {e:?}"))
                        );
                        if audit.clear {
                            if let Some(receipt) = &mut clearance_receipt {
                                receipt["horizontal_iterations"] = horizontal.into();
                                receipt["clearance_work"] = audit.work.into();
                                receipt["source_curve_clear"] = true.into();
                                receipt["loop_seam_adjustment_metres"] = seam.into();
                                receipt["loop_seam_normalized_before_junction_binding"] =
                                    true.into();
                            }
                            accepted = Some(shaped);
                            break;
                        }
                    }
                    accepted.ok_or(
                        "Source ring cannot retain continuous clearance after guarded routing",
                    )?
                } else if source_ring {
                    let (shaped, geometry) = ready!(shape_source_ring(
                        shape,
                        ring_policy.width * 0.5,
                        |settings| raw.shape(
                            settings,
                            |uv| path_terrain::sample_height_uv(height, uv),
                            cancel
                        ),
                        cancel
                    ));
                    if let Some(receipt) = &mut clearance_receipt {
                        receipt["geometry_preflight"] = geometry;
                    }
                    shaped
                } else {
                    ready!(raw.shape(
                        shape,
                        |uv| path_terrain::sample_height_uv(height, uv),
                        cancel
                    ))
                };
                ring_shaping_work = shaped.total_work;
                ring_layout = Some(Arc::new(ring::Layout {
                    lattice: raw.lattice,
                    nodes: raw.nodes,
                    path: shaped.path,
                }));
            }
            ring::Outcome::Skipped(_) => {
                if let Some(receipt) = &mut clearance_receipt {
                    receipt["skipped_below_source_world_size"] =
                        ring_policy.minimum_world_size.into();
                }
            }
            ring::Outcome::Cancelled(_) => return Ok(None),
            ring::Outcome::Budget(_) => {
                return Err("Ordered road ring search budget exhausted".into());
            }
            ring::Outcome::Unresolved(_) => {
                return Err("Ordered road ring could not be connected".into());
            }
        }
    }
    let obstructions = retained_layout.as_ref().and_then(|retained| retained.obstructions.as_deref());
    let mut local_routed_cells = 0;
    let mut local_sections = 0;
    let mut rebuilt_families = 0;
    let protected: BTreeSet<_> = if obstructions.is_some() {
        retained_layout.iter()
            .flat_map(|retained| &retained.accepted.layout.families)
            .flat_map(|family| &family.raw.segments)
            .flat_map(|segment| [segment.nodes.first(), segment.nodes.last()])
            .flatten()
            .map(|node| node.cell)
            .collect()
    } else {
        BTreeSet::new()
    };
    if let Some(retained) = &retained_layout {
        clearance_receipt = Some(serde_json::json!({"retained_road_layout":true}));
        if let Some(saved) = &retained.accepted.layout.ring {
            let grid = ring_grid.as_ref().filter(|grid| saved.lattice == grid.lattice)
                .ok_or("Retained road ring uses a different source lattice")?;
            let edited = if let Some(obstructions) = obstructions {
                local_edits::detour(
                    grid, &saved.nodes, &saved.path, &protected, obstructions,
                    ring_policy.width * 0.5 + source.profile.shoulder,
                    |uv| path_terrain::sample_height_uv(height, uv), cancel,
                )?
            } else {
                None
            };
            ring_layout = Some(if let Some(edited) = edited {
                local_sections += 1;
                local_routed_cells += edited.cells;
                Arc::new(ring::Layout { lattice: grid.lattice, nodes: edited.nodes, path: edited.path })
            } else {
                saved.clone()
            });
        }
        if cancel() {
            return Ok(None);
        }
    }
    if let Some(layout) = &ring_layout {
        let reserved = ready!(rail_reservations::generate(
            project.size as f32,
            maps.layers.surface_resolution,
            &placement,
            &[rail_reservations::Track { id: 1, path: &layout.path }],
            &reserve_policy(true),
            reserves.maximum_work,
            cancel,
        ).map_err(|error| format!("Main road placement sweep failed: {error}")));
        placement = reserved.raster;
        ring_costs = ring_grid;
    }
    let mut shaping_work = ring_shaping_work
        .checked_add(coastal_ring_work)
        .and_then(|w| w.checked_add(coastal_ground_work))
        .filter(|&w| w <= maximum_work)
        .ok_or("Coastal ring guidance work exhausted")?;
    let connected = recipe
        .staged
        .as_ref()
        .is_some_and(|s| crate::road_generation::is_connected_source_engine(&s.engine));
    let mut family_costs = Vec::new();
    let mut family_geometry = Vec::new();
    let mut families: Vec<(source_road_inputs::Prepared, Arc<layout::AcceptedFamily>)> = Vec::new();
    let reserve_family = |paths: &[path_geometry::Path], placement: &[u8], trail| {
        let tracks: Vec<_> = paths.iter().enumerate().map(|(i, path)| rail_reservations::Track {
            id: i as u64 + 1, path,
        }).collect();
        rail_reservations::generate(
            project.size as f32,
            maps.layers.surface_resolution,
            placement,
            &tracks,
            &reserve_policy(false),
            reserves.maximum_work,
            cancel,
        ).map_err(|error| format!("Ordered {} road placement sweep failed: {error}", if trail { "dirt" } else { "paved" }))
    };
    for trail in [false, true] {
        if trail && recipe.include_trails == Some(false) {
            continue;
        }
        if cancel() {
            return Ok(None);
        }
        progress.reporter().stage(if trail {
            "Routing dirt roads"
        } else {
            "Routing paved roads"
        });
        let seed = if trail {
            infrastructure::trail_seed(
                project.seeds[3].value,
                &source
                    .profile
                    .trails
                    .as_ref()
                    .ok_or("Missing source dirt seed domain")?
                    .seed_domain,
            )
        } else {
            project.seeds[3].value
        };
        let mut grid = ready!(terrain_costs.build(&placement, seed, trail, costs.maximum_work, cancel));
        // Keep the actual source grid, before retry preferences or entrance
        // guidance. Final validation must not invent a 257-vertex substitute.
        shaping_work = shaping_work
            .checked_add(grid.grid.costs.len())
            .filter(|&n| n <= maximum_work)
            .ok_or("Source route receipt work exhausted")?;
        family_costs.push(FamilyCosts {
            hierarchy: if trail { 2 } else { 1 },
            grid: grid.clone(),
        });
        let width = if trail {
            source
                .profile
                .trails
                .as_ref()
                .ok_or("Missing source trail width")?
                .width
        } else {
            source.profile.width
        };
        if let Some(retained) = &retained_layout && rebuilt_families == 0 {
            let saved = retained.accepted.layout.families.get(usize::from(trail))
                .filter(|family| family.raw.lattice == grid.lattice)
                .ok_or("Retained road family uses a different source lattice")?;
            let prepared = ready!(source_road_inputs::prepare(
                project, solution, &inputs.assets, &inputs.roles, grid.lattice, trail, cancel,
            ));
            let entrances_match = if obstructions.is_some() {
                let previous = ready!(source_road_inputs::prepare(
                    &retained.accepted.project, &retained.accepted.solution, &inputs.assets,
                    &inputs.roles, grid.lattice, trail, cancel,
                ));
                previous.connectors == prepared.connectors && previous.endpoints == prepared.endpoints
            } else {
                true
            };
            if entrances_match {
                let mut changed = None;
                let shoulder = if trail {
                    source.profile.trails.as_ref().ok_or("Missing source trail shoulders")?.shoulder
                } else {
                    source.profile.shoulder
                };
                if let Some(obstructions) = obstructions {
                    for (index, (segment, path)) in saved.raw.segments.iter().zip(&saved.shaped).enumerate() {
                        if let Some(edited) = local_edits::detour(
                            &grid, &segment.nodes, path, &protected, obstructions, width * 0.5 + shoulder,
                            |uv| path_terrain::sample_height_uv(height, uv), cancel,
                        )? {
                            let family = changed.get_or_insert_with(|| layout::AcceptedFamily {
                                raw: saved.raw.clone(), shaped: saved.shaped.clone(),
                            });
                            family.raw.segments[index].nodes = edited.nodes;
                            family.shaped[index] = edited.path;
                            local_sections += 1;
                            local_routed_cells += edited.cells;
                            if local_sections > 32 {
                                return Err("Monument moves touch too many retained road sections".into());
                            }
                        }
                        if cancel() {
                            return Ok(None);
                        }
                    }
                }
                let family = changed.map(Arc::new).unwrap_or_else(|| saved.clone());
                placement = ready!(reserve_family(&family.shaped, &placement, trail)).raster;
                families.push((prepared, family));
                continue;
            }
        }
        if obstructions.is_some() {
            // A changed entrance needs fresh family routing. Later families
            // can attach to this one, so their old junctions cannot be reused.
            rebuilt_families += 1;
        }
        let (mut hole_exclusions, hole_work) = if guided_routes {
            let (mask, work) = ready!(entrance_hole_mask(
                &maps.alpha,
                maps.layers.surface_resolution,
                grid.lattice,
                width * 0.5,
                maximum_work.saturating_sub(shaping_work),
                cancel,
            ));
            let work = work
                .checked_add(mask.len())
                .filter(|&w| w <= maximum_work.saturating_sub(shaping_work))
                .ok_or("Entrance hole-clearance application work exhausted")?;
            shaping_work += work;
            for (i, (cost, &excluded)) in grid.grid.costs.iter_mut().zip(&mask).enumerate() {
                if i % 4096 == 0 && cancel() {
                    return Ok(None);
                }
                if excluded != 0 {
                    *cost = 0;
                }
            }
            (Some(mask), work)
        } else {
            (None, 0)
        };
        #[cfg(feature = "road-diagnostics")]
        let hole_mask = hole_exclusions.clone();
        #[cfg(feature = "road-diagnostics")]
        let mut coastal_mask = None;
        let mut retry_exclusion_work = 0;
        let mut coastal_clearance_work = 0;
        let mut coastal_penalties = None;
        let hierarchy = if trail { 2 } else { 1 };
        if let Some(ocean) = coastal_retry
            && let Some(mask) = &hole_exclusions
        {
            let shoulder = if trail {
                source
                    .profile
                    .trails
                    .as_ref()
                    .ok_or("Missing source trail shoulders")?
                    .shoulder
            } else {
                source.profile.shoulder
            };
            let spacing = recipe
                .entrance_launch
                .ok_or("Missing source entrance launch policy")?
                .sample_spacing;
            let (coast, work) = ready!(coastal_retry_mask(
                coastal_topology,
                maps.layers.surface_resolution,
                grid.lattice,
                ocean,
                width * 0.5 + shoulder + if early_coastal { 0. } else { spacing },
                maximum_work.saturating_sub(shaping_work),
                cancel,
            ));
            shaping_work += work;
            #[cfg(feature = "road-diagnostics")]
            {
                coastal_mask = Some(coast.clone());
            }
            let merged = ready!(entrance_retry_mask(
                mask,
                &coast,
                maximum_work.saturating_sub(shaping_work + mask.len()),
                cancel
            ));
            let application_work = merged.len() * 2;
            coastal_clearance_work = work + application_work;
            shaping_work = shaping_work
                .checked_add(application_work)
                .filter(|&w| w <= maximum_work)
                .ok_or("Coastal route guidance work exhausted")?;
            for (i, (cost, &excluded)) in grid.grid.costs.iter_mut().zip(&merged).enumerate() {
                if i % 4096 == 0 && cancel() {
                    return Ok(None);
                }
                if excluded != 0 {
                    *cost = 0;
                }
            }
            if !early_coastal {
                hole_exclusions = Some(merged);
            } else {
                coastal_penalties = Some(coast);
            }
            if let Some(deep) = &deep_ground
                && preferences.iter().any(|m| m.hierarchy == hierarchy && m.exclude && m.low_ground)
            {
                // Terrain deeper than the recipe's fill allowance cannot
                // become a dry approach, even after coastal construction.
                let (deep, work) = ready!(coastal_retry_mask(
                    deep, maps.layers.surface_resolution, grid.lattice, ocean,
                    width * 0.5 + shoulder + spacing,
                    maximum_work.saturating_sub(shaping_work), cancel
                ));
                let merged = ready!(entrance_retry_mask(
                    hole_exclusions.as_deref().ok_or("Missing coastal entrance mask")?,
                    &deep,
                    maximum_work.saturating_sub(shaping_work + work), cancel
                ));
                shaping_work += work + merged.len();
                hole_exclusions = Some(merged);
            }
        }
        if let Some(mask) = &hole_exclusions
            && preferences.iter().any(|m| m.hierarchy == hierarchy
                && (m.protected_topology != 0 || (m.low_ground && coastal_ground.is_some())))
        {
            let (blocked_terrain, work) = ready!(terrain_retry_mask(
                &maps.layers, grid.lattice, preferences, hierarchy,
                coastal_ground.as_deref(),
                maximum_work.saturating_sub(shaping_work), cancel
            ));
            let merged = ready!(entrance_retry_mask(
                mask, &blocked_terrain,
                maximum_work.saturating_sub(shaping_work + work + mask.len()), cancel
            ));
            shaping_work += work + merged.len() * 2;
            for (i, (cost, &blocked)) in grid.grid.costs.iter_mut().zip(&blocked_terrain).enumerate() {
                if i % 4096 == 0 && cancel() {
                    return Ok(None);
                }
                if blocked != 0 {
                    *cost = 0;
                }
            }
            hole_exclusions = Some(merged);
        }
        let mut entrance_footprints = None;
        if !preferences.is_empty() {
            let entrance_margin = if guided_routes {
                let spacing = recipe
                    .entrance_launch
                    .ok_or("Missing source entrance launch policy")?
                    .sample_spacing;
                Some(width * 0.5 + spacing + grid.lattice.spacing() * 0.5)
            } else {
                None
            };
            let applied = ready!(crate::road_replanning::apply_cell_with_entrances(
                &mut grid,
                preferences,
                5000,
                maximum_work,
                hierarchy,
                entrance_margin,
                cancel
            ));
            entrance_footprints = applied.entrance_footprints;
            if let Some(mask) = &hole_exclusions {
                let merged = ready!(entrance_retry_mask(
                    mask,
                    &applied.entrance_exclusions,
                    maximum_work.saturating_sub(shaping_work),
                    cancel,
                ));
                shaping_work += merged.len();
                retry_exclusion_work = merged.len();
                hole_exclusions = Some(merged);
            }
            routing_preferences.push(applied.report);
        }
        let prepared = ready!(source_road_inputs::prepare(
            project,
            solution,
            &inputs.assets,
            &inputs.roles,
            grid.lattice,
            trail,
            cancel
        ));
        let mut existing = Vec::new();
        if let Some(r) = &ring_layout {
            existing.push(layout::ExistingPath {
                lattice: r.lattice,
                nodes: &r.nodes,
            });
        }
        for (_, previous) in &families {
            for s in &previous.raw.segments {
                existing.push(layout::ExistingPath {
                    lattice: s.lattice,
                    nodes: &s.nodes,
                });
            }
        }
        let mut entrance_heights = Vec::new();
        let entrance_owners: Vec<_> = if entrance_footprints.is_some() {
            prepared.endpoints.iter().map(|endpoint| endpoint.monument).collect()
        } else {
            Vec::new()
        };
        let recovery = if terrain_guidance {
            progress
                .reporter()
                .stage("Checking monument entrance slopes");
            let n = grid.lattice.resolution();
            entrance_heights.reserve(n * n);
            for i in 0..n * n {
                if i % 256 == 0 && cancel() {
                    return Ok(None);
                }
                let [x, z] = grid
                    .lattice
                    .point([i % n, i / n])
                    .ok_or("Invalid entrance lattice")?;
                entrance_heights.push(path_terrain::sample_height(height, [x, 0., z])?);
            }
            let mut recovery = source_road_route::TerrainRecovery::new(
                grid.lattice,
                &entrance_heights,
                recipe.maximum_grade,
                costs.policy.expensive_cost,
                joins.bridge_radius_cells,
            )?;
            if let Some(mask) = &hole_exclusions {
                recovery = recovery.with_entrance_exclusions(mask)?;
            }
            if let Some(mask) = &coastal_penalties {
                recovery = recovery.with_entrance_penalties(mask)?;
            }
            if let Some(footprints) = &entrance_footprints {
                recovery = recovery.with_foreign_footprints(footprints, &entrance_owners)?;
            }
            Some(recovery)
        } else {
            None
        };
        let outcome = if let Some(recovery) = recovery {
            let generate = if bootstrap {
                layout::generate_connected_with_terrain_bootstrap
            } else if guided_routes {
                layout::generate_connected_with_terrain_routed
            } else {
                layout::generate_connected_with_terrain
            };
            let initial_work = maximum_work.saturating_sub(
                entrance_heights.len() + hole_work + retry_exclusion_work + coastal_clearance_work,
            );
            route_with_budget_recovery(
                layout_policy,
                initial_work,
                |policy, allowance| {
                    generate(
                        &grid,
                        &prepared.connectors,
                        &existing,
                        policy,
                        search,
                        allowance,
                        recovery,
                        cancel,
                    )
                },
                cancel,
            )?
        } else {
            layout::generate_connected(
                &grid,
                &prepared.connectors,
                &existing,
                layout_policy,
                search,
                maximum_work,
                cancel,
            )?
        };
        let mut raw = match outcome {
            layout::Outcome::Found(v) => v,
            layout::Outcome::Cancelled(_) => return Ok(None),
            layout::Outcome::Budget(audit) => {
                let last = audit
                    .terrain_route_traces
                    .last()
                    .map(|t| {
                        format!(
                            "{}: {} expansions, {} labels, {}/{} goals, {} goal comparisons, indexed {}, sparse {}",
                            t.stop, t.expansions, t.labels, t.walkable_goals, t.goal_samples,
                            t.direct_comparisons, t.indexed, t.sparse_index
                        )
                    })
                    .unwrap_or_default();
                return Err(format!(
                    "Ordered {} road connector search budget exhausted (work {}/{}, terrain routes {}, source fallback {}; {})",
                    if trail { "dirt" } else { "paved" },
                    audit.work,
                    audit.maximum_work,
                    audit.terrain_route_queries,
                    audit.terrain_route_fallback,
                    last
                ).into());
            }
        };
        if terrain_guidance {
            terrain_entrances.push(serde_json::json!({"engine":terrain_engine,
                "trail":trail,"lattice":grid.lattice,"maximum_grade":recipe.maximum_grade,
                "slope_penalty":costs.policy.expensive_cost,"extra_radius_cells":joins.bridge_radius_cells,
                "height_sampling_work":entrance_heights.len(),"audit":raw.audit,
                "hole_clearance":hole_exclusions.is_some(),"hole_clearance_work":hole_work,
                "retry_entrance_exclusion_work":retry_exclusion_work,
                "coastal_entrance_clearance_work":coastal_clearance_work,
                "soft_guidance_only":hole_exclusions.is_none(),"terrain_and_mesh_validated":false}));
            if early_coastal {
                terrain_entrances.last_mut().unwrap()["coastal_guidance_keeps_source_entrance_recovery"] =
                    true.into();
                terrain_entrances.last_mut().unwrap()["coastal_entrance_penalty"] =
                    costs.policy.expensive_cost.into();
            }
        }
        if !raw.unattached_connectors.is_empty() || !raw.unrecovered_connectors.is_empty() {
            #[cfg(feature = "road-diagnostics")]
            capture_layout_failure_if_requested(
                &raw,
                &family_costs.last().unwrap().grid,
                &grid,
                &serde_json::json!({
                    "trail": trail,
                    "hole_mask": hole_mask,
                    "coastal_mask": coastal_mask,
                    "combined_mask": hole_exclusions,
                    "preferences": preferences,
                    "endpoints": prepared.endpoints,
                }),
            )?;
            let message = format!(
                "Ordered {} roads left {} connectors unrecovered and {} unconnected. {}",
                if trail { "dirt" } else { "paved" },
                raw.unrecovered_connectors.len(),
                raw.unattached_connectors.len(),
                serde_json::json!({
                    "unrecovered":raw.unrecovered_connectors.iter().filter_map(|&id|prepared.endpoints.get(id)).collect::<Vec<_>>(),
                    "unconnected":raw.unattached_connectors.iter().filter_map(|&id|prepared.endpoints.get(id)).collect::<Vec<_>>(),
                    "last_family_search":raw.audit.terrain_route_traces.last(),
                    "failed_connections":raw.audit.failed_connections,
                    "depth_rejections":raw.audit.depth_rejections,
                    "entrance_diagnostics":raw.unrecovered_connectors.iter().chain(&raw.unattached_connectors).filter_map(|&id| {
                        let connector = prepared.connectors.get(id)?;
                        let endpoint = prepared.endpoints.get(id)?;
                        let index = connector.cell[1] * grid.lattice.resolution() + connector.cell[0];
                        let recovery = raw.recovered_entrances.iter().find(|entrance|entrance.connector == id);
                        Some(serde_json::json!({"monument":endpoint.monument,"connector":endpoint.connector,
                            "cell":connector.cell,"radius":connector.radius,
                            "source_cost":family_costs.last()?.grid.grid.costs.get(index),
                            "routing_cost":grid.grid.costs.get(index),
                            "entrance_excluded":hole_exclusions.as_ref().map(|mask|mask[index] != 0),
                            "height":entrance_heights.get(index),"recovery":recovery}))
                    }).collect::<Vec<_>>()
                })
            );
            let entrances = if matches!(
                costs.coastal_entrances.as_deref(),
                Some(
                    crate::road_cost_source::LATTICE_COASTAL_ENTRANCES_VERSION
                        | crate::road_cost_source::GROUND_GUIDED_COASTAL_ENTRANCES_VERSION
                )
            ) {
                raw.unrecovered_connectors
                    .iter()
                    .chain(&raw.unattached_connectors)
                    .filter_map(|&id| prepared.endpoints.get(id))
                    .map(|endpoint| papyrus_terrain::road_network::socket_recovery::Key {
                        monument: endpoint.monument,
                        connector: endpoint.connector,
                    })
                    .collect()
            } else {
                Vec::new()
            };
            return Err(crate::road_planning::Failure { message, entrances, rejected_retained_layout: false });
        }
        let terminals = if connected {
            let maximum_terminal_nodes = raw.segments.len() * 2;
            let index_depth = maximum_terminal_nodes.max(1).ilog2() as usize + 1;
            shaping_work = shaping_work
                .checked_add(maximum_terminal_nodes * index_depth)
                .filter(|&work| work <= maximum_work)
                .ok_or("Ordered road terminal indexing exhausted its work allowance")?;
            raw.segments
                .iter()
                .flat_map(|segment| [segment.nodes.first(), segment.nodes.last()])
                .flatten()
                .map(|node| node.cell)
                .collect()
        } else {
            BTreeSet::new()
        };
        let mut shaped = Vec::new();
        for (segment_index, segment) in raw.segments.iter_mut().enumerate() {
            let build = |segment: &layout::Segment, settings: path_geometry::Settings| -> Result<Option<ring::Shaped>, String> {
                let mut built = ready!(segment.shape(
                    settings,
                    |id| {
                        prepared
                            .endpoints
                            .get(id)
                            .map(|e| [e.world[0], e.world[2]])
                            .ok_or("Invalid source endpoint identity".into())
                    },
                    |uv| path_terrain::sample_height_uv(height, uv),
                    cancel,
                ));
                if pinned_endpoints {
                    let pinned = ready!(pin_authored_endpoints(
                        segment,
                        &prepared.endpoints,
                        &built.path,
                        settings.resample_distance,
                        settings.maximum_work.saturating_sub(built.total_work),
                        cancel,
                    ));
                    built.total_work = built
                        .total_work
                        .checked_add(pinned.work())
                        .filter(|&work| work <= settings.maximum_work)
                        .ok_or("Ordered road endpoint shaping budget exhausted")?;
                    built.path = pinned;
                }
                Ok(Some(ring::Shaped {
                    path: built.path,
                    total_work: built.total_work,
                }))
            };
            let built = if connected {
                let (built, geometry) = ready!(shape_open_road(
                    segment,
                    &terminals,
                    path_geometry::Settings {
                        maximum_work: shape
                            .maximum_work
                            .min(maximum_work.saturating_sub(shaping_work)),
                        ..shape
                    },
                    width * 0.5,
                    trail && !preferences.is_empty(),
                    build,
                    cancel,
                ).map_err(|error| {
                    format!("{error}. {}", serde_json::json!({
                        "family":if trail { "dirt" } else { "paved" },
                        "segment":segment_index,"raw_nodes":segment.nodes.len(),
                        "origin":segment.origin,"target":segment.target
                    }))
                }));
                family_geometry.push(serde_json::json!({
                    "hierarchy":if trail { 2 } else { 1 }, "segment":segment_index,
                    "geometry":geometry,
                }));
                built
            } else {
                ready!(build(segment, shape))
            };
            shaping_work = shaping_work
                .checked_add(built.total_work)
                .filter(|&work| work <= maximum_work)
                .ok_or("Ordered road family shaping budget exhausted")?;
            shaped.push(built.path);
        }
        placement = ready!(reserve_family(&shaped, &placement, trail)).raster;
        families.push((prepared, Arc::new(layout::AcceptedFamily { raw, shaped })));
    }
    let ring = ring_layout.as_ref().map(|layout| handoff::Ring {
        lattice: layout.lattice,
        nodes: &layout.nodes,
        path: &layout.path,
    });
    let borrowed = families
        .iter()
        .map(|(inputs, family)| handoff::Family {
            inputs,
            layout: &family.raw,
            shaped: &family.shaped,
        })
        .collect::<Vec<_>>();
    let junction_settings = papyrus_terrain::source_road_junctions::Settings {
        maximum_snap_distance: 20.,
        maximum_work: 4_000_000,
    };
    let network = ready!(if recipe
        .staged
        .as_ref()
        .is_some_and(|s| crate::road_generation::is_raw_source_engine(&s.engine))
    {
        handoff::prepare_raw(ring, &borrowed, junction_settings, cancel)
    } else if connected
        || recipe
            .staged
            .as_ref()
            .is_some_and(|s| s.engine == crate::road_generation::SOURCE_VERSION)
    {
        let widths = [
            source
                .profile
                .ring
                .as_ref()
                .map_or(source.profile.width, |r| r.width),
            source.profile.width,
            source
                .profile
                .trails
                .as_ref()
                .map_or(source.profile.width, |t| t.width),
        ];
        if connected {
            handoff::prepare_junction_checked(ring, &borrowed, widths, junction_settings, cancel)
        } else {
            handoff::prepare_strip_checked(ring, &borrowed, widths, junction_settings, cancel)
        }
    } else {
        handoff::prepare(ring, &borrowed, junction_settings, cancel)
    });
    let mut plan = ready!(network.into_retained_plan(source, project.seeds[3].value, cancel));
    if let Some(receipt) = &mut clearance_receipt && obstructions.is_some() {
        receipt["local_road_sections"] = local_sections.into();
        receipt["local_routed_cells"] = local_routed_cells.into();
        receipt["rebuilt_road_families"] = rebuilt_families.into();
    }
    if ring_layout.is_some() || !families.is_empty() {
        plan.retain_source_layout(Some(Arc::new(layout::AcceptedNetwork {
            ring: ring_layout,
            families: families.into_iter().map(|(_, family)| family).collect(),
        })));
    }
    Ok(Some(Prepared {
        plan,
        ring_costs,
        ring_clearance: clearance_receipt,
        routing_preferences,
        terrain_entrances,
        family_costs,
        family_geometry,
    }))
}

/// A failed family retains its exact recovered roots and immutable cost grids
/// for offline reachability analysis. Compiled only for developer builds.
#[cfg(feature = "road-diagnostics")]
fn capture_layout_failure_if_requested(
    raw: &layout::Layout,
    original: &CellGrid,
    routing: &CellGrid,
    entrance_inputs: &serde_json::Value,
) -> Result<(), String> {
    use std::{
        fs,
        io::Write,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let Some(directory) = std::env::var_os("MAPPER_LAYOUT_FAILURE_CAPTURE") else {
        return Ok(());
    };
    let index = NEXT.fetch_add(1, Ordering::Relaxed);
    let directory = std::path::Path::new(&directory);
    if index == 0 {
        fs::create_dir(directory).map_err(|e| format!("New layout capture directory: {e}"))?;
    }
    let bytes = serde_json::to_vec(&serde_json::json!({
        "format":"mapper-private-layout-failure-1", "ordinal":index,
        "layout":raw,"original":original,"routing":routing,
        "entrance_inputs":entrance_inputs
    }))
    .map_err(|e| e.to_string())?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err("Layout capture exceeds allowance".into());
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join(format!("layout-{index:04}.json")))
        .map_err(|e| e.to_string())?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())
}

/// Data-only opt-in capture; ordinary generation performs no capture IO.
#[cfg(feature = "road-diagnostics")]
fn capture_ring_if_requested(
    raw: &ring::Ring,
    original: &CellGrid,
    routing: &CellGrid,
) -> Result<(), String> {
    use std::{
        fs,
        io::Write,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let Some(directory) = std::env::var_os("MAPPER_RING_RAW_CAPTURE") else {
        return Ok(());
    };
    let index = NEXT.fetch_add(1, Ordering::Relaxed);
    let directory = std::path::Path::new(&directory);
    if index == 0 {
        fs::create_dir(directory).map_err(|e| format!("New ring capture directory: {e}"))?;
    }
    let bytes = serde_json::to_vec(
        &serde_json::json!({"format":"mapper-private-ring-inputs-1","ordinal":index,
        "ring":raw,"original":original,"routing":routing}),
    )
    .map_err(|e| e.to_string())?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("Ring capture exceeds allowance".into());
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join(format!("ring-{index:04}.json")))
        .map_err(|e| e.to_string())?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_road_layout_matches_only_appended_path_edits_on_the_same_world() {
        use crate::path_edit::{Change, Key, Kind, Operation};
        let mut project = Project::default();
        let rail = Change {
            route: Key { kind: Kind::Rail, index: 0 },
            control: 1,
            expected: [0., 0.],
            operation: Operation::Move([1., 0.]),
        };
        project.path_edits.push(rail);
        let solution = Solution { outcomes: vec![], attempts: 0, required_missing: 0 };
        let path = path_geometry::Path::from_controls(
            &[[0., 3., 0.], [3., 3., 3.], [0., 3., 0.]], 100, || false,
        ).unwrap().unwrap();
        let accepted = AcceptedRoadLayout {
            project: project.clone(),
            monuments: crate::composition::layout_fingerprint(&solution),
            solution: solution.clone(),
            catalog: "catalog".into(), bindings: "bindings".into(),
            layout: Arc::new(layout::AcceptedNetwork {
                ring: Some(Arc::new(ring::Layout {
                    lattice: papyrus_terrain::road_costmap::CellLattice::new(1000., 8.).unwrap(),
                    nodes: Vec::new(), path,
                })),
                families: Vec::new(),
            }),
        };
        let fits = |recipe: &Project| accepted.matches(recipe, &solution, "catalog", "bindings");
        assert!(!fits(&project));
        let next = Change { expected: [1., 0.], operation: Operation::Move([2., 0.]), ..rail };
        project.path_edits.push(next);
        assert!(fits(&project));
        assert!(!accepted.matches(&project, &solution, "catalog", "changed bindings"));
        assert!(!accepted.matches(&project, &Solution { attempts: 1, ..solution.clone() }, "catalog", "bindings"));
        project.path_edits[0] = next;
        assert!(!fits(&project));
        project.path_edits[0] = rail;
        project.path_edits[1].route.kind = Kind::Road;
        assert!(fits(&project));
        project.path_edits[1] = next;
        project.seeds[3].value += 1;
        assert!(!fits(&project));
    }
    fn empty_layout() -> layout::Layout {
        let grid = CellGrid {
            lattice: papyrus_terrain::road_costmap::CellLattice::new(2., 1.).unwrap(),
            grid: papyrus_terrain::road_costmap::Grid {
                engine: papyrus_terrain::road_costmap::CELL_VERSION,
                resolution: 2,
                seed: 1,
                trail: false,
                counts: Default::default(),
                work: 0,
                costs: vec![100; 4],
            },
        };
        let layout::Outcome::Found(layout) = layout::generate(
            &grid,
            &[],
            &[],
            layout::Policy {
                network_sample_stride: 8,
                push_multiplier: 50000,
                maximum_queries: 8192,
                maximum_node_storage: 262144,
            },
            source_road_route::Settings {
                maximum_expansions: 100000,
                maximum_work: 128_000_000,
                directional_penalty: 10000,
                directional_radius: 1,
            },
            512_000_000,
            || false,
        )
        .unwrap() else {
            panic!("Empty layout failed");
        };
        layout
    }
    #[test]
    fn successful_layout_keeps_its_exact_receipt_without_budget_recovery() {
        let expected = serde_json::to_value(empty_layout()).unwrap();
        let policy = empty_layout().audit.policy;
        let mut calls = 0;
        let outcome = route_with_budget_recovery(
            policy,
            100,
            |_, allowance| {
                calls += 1;
                assert_eq!(allowance, 100);
                Ok(layout::Outcome::Found(empty_layout()))
            },
            &|| false,
        )
        .unwrap();
        let layout::Outcome::Found(actual) = outcome else {
            panic!();
        };
        assert_eq!(calls, 1);
        assert_eq!(serde_json::to_value(actual).unwrap(), expected);
    }
    #[test]
    fn exhausted_layout_retains_work_queries_and_traces_in_one_recovery() {
        let policy = empty_layout().audit.policy;
        for succeed in [false, true] {
            let mut calls = 0;
            let outcome = route_with_budget_recovery(
                policy,
                100,
                |next, allowance| {
                    calls += 1;
                    let mut result = empty_layout();
                    result.audit.entrance_queries = 1;
                    result.audit.layout_queries = 2;
                    if calls == 1 {
                        assert_eq!(allowance, 100);
                        result.audit.work = 100;
                        result.audit.terrain_route_traces.push(
                            source_road_route::AccessDiagnostics {
                                stop: "initial".into(),
                                ..Default::default()
                            },
                        );
                        return Ok(layout::Outcome::Budget(result.audit));
                    }
                    assert_eq!(calls, 2);
                    assert_eq!(next.maximum_queries, policy.maximum_queries - 3);
                    assert_eq!(allowance, 384_000_000);
                    result.audit.work = allowance;
                    Ok(if succeed {
                        layout::Outcome::Found(result)
                    } else {
                        layout::Outcome::Budget(result.audit)
                    })
                },
                &|| false,
            )
            .unwrap();
            let audit = match outcome {
                layout::Outcome::Found(result) => {
                    assert!(succeed);
                    result.audit
                }
                layout::Outcome::Budget(audit) => {
                    assert!(!succeed);
                    audit
                }
                layout::Outcome::Cancelled(_) => panic!(),
            };
            assert_eq!(calls, 2);
            assert_eq!(audit.work, 384_000_100);
            assert_eq!(audit.maximum_work, audit.work);
            assert_eq!(audit.entrance_queries, 2);
            assert_eq!(audit.layout_queries, 4);
            assert_eq!(audit.policy.maximum_queries, policy.maximum_queries);
            assert_eq!(audit.terrain_route_traces[0].stop, "initial");
        }
    }
    #[test]
    fn layout_recovery_preserves_cancellation_and_exhausted_query_limits() {
        let policy = empty_layout().audit.policy;
        for cancel in [false, true] {
            let mut calls = 0;
            let outcome = route_with_budget_recovery(
                policy,
                100,
                |_, _| {
                    calls += 1;
                    let mut audit = empty_layout().audit;
                    audit.work = 100;
                    audit.entrance_queries = policy.maximum_queries;
                    Ok(layout::Outcome::Budget(audit))
                },
                &|| cancel,
            )
            .unwrap();
            match outcome {
                layout::Outcome::Cancelled(audit) => {
                    assert!(cancel);
                    assert_eq!(audit.work, 100);
                }
                layout::Outcome::Budget(audit) => {
                    assert!(!cancel);
                    assert_eq!(audit.work, 100);
                }
                layout::Outcome::Found(_) => panic!(),
            }
            assert_eq!(calls, 1);
        }
        for allowance in [0, 128_000_001] {
            assert!(
                route_with_budget_recovery(
                    policy,
                    allowance,
                    |_, _| panic!("Invalid grant ran"),
                    &|| false
                )
                .is_err()
            );
        }
    }
    #[test]
    fn pinned_source_endpoints_repair_resampling_drift_without_moving_interior_or_height() {
        let lattice = papyrus_terrain::road_costmap::CellLattice::new(5000., 7.5).unwrap();
        let world = [[-350., 10., -1790.], [-329.8833, 12., -1774.8328]];
        let endpoints = world
            .iter()
            .enumerate()
            .map(|(index, &world)| source_road_inputs::Endpoint {
                monument: index as u64 + 1,
                connector: index as i64 + 1,
                prefab: "test".into(),
                world,
                hierarchy_active_without_root: true,
            })
            .collect::<Vec<_>>();
        let mut segment = layout::Segment {
            lattice,
            origin: Some(0),
            target: Some(1),
            nodes: world
                .iter()
                .map(|point| ring::RawNode {
                    cell: lattice.cell([point[0], point[2]]).unwrap(),
                    cost: 1,
                })
                .collect(),
        };
        let controls = [
            world[0],
            [-340., 11., -1785.],
            [-329.88382, 12., -1774.8318],
        ];
        let original = path_geometry::Path::from_controls(&controls, 1000, || false)
            .unwrap()
            .unwrap();
        let corrected =
            pin_authored_endpoints(&segment, &endpoints, &original, 7.5, 1000, &|| false)
                .unwrap()
                .unwrap();
        assert!(
            (original.points()[2][0] - world[1][0]).hypot(original.points()[2][2] - world[1][2])
                > 0.001
        );
        for (index, world) in [(0, world[0]), (2, world[1])] {
            let canonical =
                [world[0], world[2]].map(|v| -2500. + ((v + 2500.) * (1. / 5000.)) * 5000.);
            assert_eq!(
                [corrected.points()[index][0], corrected.points()[index][2]],
                canonical
            );
            assert_eq!(corrected.points()[index][1], controls[index][1]);
        }
        assert_eq!(corrected.points()[1], controls[1]);
        assert_eq!(original.points(), controls);
        assert!(corrected.work() > 0);
        assert!(
            pin_authored_endpoints(&segment, &endpoints, &original, 7.5, 1, &|| false).is_err()
        );
        assert!(
            pin_authored_endpoints(&segment, &endpoints, &original, 7.5, 1000, &|| true)
                .unwrap()
                .is_none()
        );
        let calls = std::cell::Cell::new(0);
        assert!(
            pin_authored_endpoints(&segment, &endpoints, &original, 7.5, 1000, &|| {
                calls.set(calls.get() + 1);
                calls.get() > 2
            })
            .unwrap()
            .is_none()
        );
        let mut moved = controls;
        moved[2][0] += 20.;
        let distant = path_geometry::Path::from_controls(&moved, 1000, || false)
            .unwrap()
            .unwrap();
        assert!(
            pin_authored_endpoints(&segment, &endpoints, &distant, 7.5, 1000, &|| false).is_err()
        );
        segment.nodes[1].cell[0] += 1;
        assert!(
            pin_authored_endpoints(&segment, &endpoints, &original, 7.5, 1000, &|| false).is_err()
        );
        segment.target = Some(99);
        assert!(
            pin_authored_endpoints(&segment, &endpoints, &original, 7.5, 1000, &|| false).is_err()
        );
    }
    #[test]
    fn low_ground_guidance_encloses_bilinear_cells_and_preserves_source_maps() {
        let (hr, sr) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let encode = |height: f32| ((height + 500.) / 1000. * 32766.).round() as i16;
        let ocean = 1 << 20;
        let mut layers = papyrus_terrain::native_layers::Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![encode(20.); hr * hr],
            topology: vec![32; sr * sr],
            splat: vec![],
            biome: vec![],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        layers.height[0] = encode(0.9);
        layers.height[(hr / 2) * hr + hr / 2] = encode(0.9);
        layers.topology[3] |= ocean;
        let before_height = layers.height.clone();
        let before_topology = layers.topology.clone();
        let (guided, work) = coastal_ground_guidance(&layers, ocean, 1., 10_000_000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(work, sr * sr * 10);
        for z in 0..sr {
            for x in 0..sr {
                let touches_low = (x == 0 && z == 0)
                    || ([sr / 2 - 1, sr / 2].contains(&x) && [sr / 2 - 1, sr / 2].contains(&z));
                assert_eq!(guided[z * sr + x], if touches_low { ocean } else { 0 });
            }
        }
        let terrain = papyrus_terrain::placement::Terrain {
            heights: &layers.height,
            resolution: hr,
            size: 1000,
        };
        for [x, z] in [[0., 0.], [-0.001, 0.001], [0.001, -0.001], [-500., -500.]] {
            assert!(terrain.height_at(x, z) <= 1.);
            let cell = |v: f32| {
                ((v / 1000. + 0.5) * sr as f32)
                    .floor()
                    .clamp(0., (sr - 1) as f32) as usize
            };
            assert_ne!(guided[cell(z) * sr + cell(x)] & ocean, 0);
        }
        assert_eq!(
            coastal_ground_guidance(&layers, ocean, 0., work, || false)
                .unwrap()
                .unwrap()
                .0,
            vec![0; sr * sr]
        );
        assert!(coastal_ground_guidance(&layers, ocean, 1., work - 1, || false).is_err());
        assert!(
            coastal_ground_guidance(&layers, ocean, 1., work, || true)
                .unwrap()
                .is_none()
        );
        assert!(coastal_ground_guidance(&layers, 3, 1., work, || false).is_err());
        assert_eq!(layers.height, before_height);
        assert_eq!(layers.topology, before_topology);

        layers.height[0] = encode(-12.);
        let shared_budget = work + sr * sr;
        let (shallow, deep, shared_work) = coastal_ground_masks(
            &layers, ocean, 1., Some(-7.), shared_budget, &|| false,
        ).unwrap().unwrap();
        assert_eq!(shallow, guided);
        let deep = deep.unwrap();
        assert_eq!(deep[0], ocean);
        assert_eq!(deep.iter().filter(|&&cell| cell == ocean).count(), 1);
        assert_eq!(shared_work, shared_budget);
    }
    #[test]
    fn coastal_routing_padding_preserves_blocked_cell_entrance_recovery() {
        use papyrus_terrain::road_costmap::{self, CellLattice, Grid};
        let lattice = CellLattice::new(80., 10.).unwrap();
        let mut topology = vec![0; 16 * 16];
        topology[8 * 16] = 128;
        let (mask, _) = coastal_retry_mask(&topology, 16, lattice, 128, 14., 10000, || false)
            .unwrap()
            .unwrap();
        let mut grid = CellGrid {
            lattice,
            grid: Grid {
                engine: road_costmap::CELL_VERSION,
                resolution: 8,
                seed: 1,
                trail: true,
                counts: Default::default(),
                work: 0,
                costs: vec![101; 64],
            },
        };
        grid.grid.costs[63] = 0;
        let original = grid.clone();
        for (cost, &excluded) in grid.grid.costs.iter_mut().zip(&mask) {
            if excluded != 0 {
                *cost = 0;
            }
        }
        assert_eq!(grid.grid.costs[63], 0);
        assert_eq!(original.grid.costs[4 * 8 + 1], 101);
        assert_eq!(grid.grid.costs[4 * 8 + 1], 0);
        let heights = vec![20.; 64];
        let holes = vec![0; 64];
        let recovery = source_road_route::TerrainRecovery::new(lattice, &heights, 1., 5000, 15)
            .unwrap()
            .with_entrance_exclusions(&holes)
            .unwrap()
            .with_entrance_penalties(&mask)
            .unwrap();
        let settings = source_road_route::Settings {
            maximum_expansions: 1000,
            maximum_work: 100000,
            directional_penalty: 10000,
            directional_radius: 1,
        };
        let outcome = recovery
            .find_closest(&grid, [1, 4], None, 2, settings, || false)
            .unwrap();
        let source_road_route::Outcome::Found(route) = outcome else {
            panic!("Routing padding prevented source entrance recovery");
        };
        assert_eq!(route.cells.last(), Some(&[1, 4]));
        let goal = route.cells[0];
        assert_ne!(grid.grid.costs[goal[1] * 8 + goal[0]], 0);
        let forbidden = recovery
            .with_entrance_exclusions(&mask)
            .unwrap()
            .find_closest(&grid, [1, 4], None, 2, settings, || false)
            .unwrap();
        assert!(matches!(
            forbidden,
            source_road_route::Outcome::Unreachable { .. }
        ));
    }
    #[test]
    fn protected_retry_blocks_the_nearby_edge_with_shoulder_clearance() {
        let resolution = 512;
        let mut layers = papyrus_terrain::native_layers::Layers {
            size: 1000,
            terrain_seed: 0,
            biome_seed: 0,
            height_resolution: 0,
            surface_resolution: resolution,
            height: Vec::new(),
            biome: Vec::new(),
            splat: Vec::new(),
            topology: vec![0; resolution * resolution],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        let middle = resolution / 2;
        for z in middle..middle + 44 {
            layers.topology[z * resolution + middle] = 65536;
        }
        layers.topology[(middle + 130) * resolution + middle] = 65536;
        layers.topology[(middle + 30) * resolution + middle + 22] = 1024;
        let lattice = papyrus_terrain::road_costmap::CellLattice::new(1000., 7.5).unwrap();
        let marker = crate::road_replanning::Marker {
            witness: crate::road_refinement::AlignmentWitness {
                kind: crate::road_refinement::AlignmentKind::Clearance,
                road: 0,
                position: [1., 1.],
            },
            radius: 11.,
            exclude: true,
            hierarchy: 2,
            terminal: None,
            obstacle: None,
            protected_topology: 65536,
            low_ground: false,
        };
        let run = |budget| {
            terrain_retry_mask(&layers, lattice, std::slice::from_ref(&marker), 2, None, budget, &|| false)
        };
        let (mask, work) = run(2_000_000).unwrap().unwrap();
        let excluded = |world| {
            let [x, z] = lattice.cell(world).unwrap();
            mask[z * lattice.resolution() + x] != 0
        };
        assert!(excluded([8., 65.]));
        assert!(!excluded([30., 65.]));
        assert!(!excluded([43., 60.]));
        assert!(!excluded([1., 255.]));
        assert!(run(work - 1).is_err());
        assert!(terrain_retry_mask(&layers, lattice, std::slice::from_ref(&marker), 2, None, 2_000_000, &|| true)
            .unwrap().is_none());

        let mut low = vec![0; resolution * resolution];
        low[(middle + 30) * resolution + middle + 22] = 128;
        low[(middle + 130) * resolution + middle] = 128;
        let wet = crate::road_replanning::Marker {
            low_ground: true,
            protected_topology: 0,
            ..marker
        };
        let (wet_mask, _) = terrain_retry_mask(
            &layers, lattice, &[wet], 2, Some(&low), 2_000_000, &|| false,
        ).unwrap().unwrap();
        let cell = |world| {
            let [x, z] = lattice.cell(world).unwrap();
            z * lattice.resolution() + x
        };
        assert_eq!(wet_mask[cell([43., 60.])], 1);
        assert_eq!(wet_mask[cell([8., 65.])], 0);
        assert_eq!(wet_mask[cell([1., 255.])], 0);
    }

    #[test]
    fn coastal_retry_clearance_uses_the_bound_role_and_entire_road_footprint() {
        let lattice = papyrus_terrain::road_costmap::CellLattice::new(80., 10.).unwrap();
        let ocean = 1 << 20;
        let mut topology = vec![0; 16 * 16];
        topology[7 * 16 + 6] = ocean;
        topology[2 * 16 + 2] = 1024;
        let before = topology.clone();
        let (mask, work) = coastal_retry_mask(&topology, 16, lattice, ocean, 14., 10000, || false)
            .unwrap()
            .unwrap();
        for z in 0..8 {
            for x in 0..8 {
                let p = lattice.point([x, z]).unwrap();
                let dx = (-10. - p[0]).max(p[0] + 5.).max(0.);
                let dz = (-5. - p[1]).max(p[1]).max(0.);
                let covered = dx * dx + dz * dz <= 14. * 14.;
                assert_eq!(mask[z * 8 + x] != 0, covered);
            }
        }
        assert_eq!(
            mask[2 * 8 + 4],
            0,
            "A clear circular corner must stay recoverable"
        );
        assert_eq!(
            mask[8 + 1],
            0,
            "Monument topology is separate from the bound Ocean role"
        );
        assert_eq!(topology, before);
        assert_eq!(
            coastal_retry_mask(&topology, 16, lattice, ocean, 14., work, || false)
                .unwrap()
                .unwrap()
                .0,
            mask
        );
        assert!(
            coastal_retry_mask(&topology, 16, lattice, ocean, 14., work - 1, || false).is_err()
        );
        assert!(coastal_retry_mask(&topology, 16, lattice, 0, 14., 10000, || false).is_err());
        assert!(coastal_retry_mask(&topology, 16, lattice, 3, 14., 10000, || false).is_err());
        assert!(
            coastal_retry_mask(&topology[..255], 16, lattice, ocean, 14., 10000, || false).is_err()
        );
        assert!(
            coastal_retry_mask(&topology, 16, lattice, ocean, 14., 10000, || true)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn circular_coast_masks_match_brute_force_on_uneven_lattices_and_edges() {
        let lattice = papyrus_terrain::road_costmap::CellLattice::new(1000., 37.).unwrap();
        let n = lattice.resolution();
        let ocean = 1 << 17;
        for resolution in [7, 16] {
            let topology = (0..resolution * resolution)
                .map(|i| if i * 7 % 11 == 0 { ocean } else { 1024 })
                .collect::<Vec<_>>();
            for radius in [0.1, 14., 64.] {
                let (mask, _) = coastal_retry_mask(
                    &topology,
                    resolution,
                    lattice,
                    ocean,
                    radius,
                    1_000_000,
                    || false,
                )
                .unwrap()
                .unwrap();
                for (i, &excluded) in mask.iter().enumerate() {
                    let p = lattice.point([i % n, i / n]).unwrap().map(f64::from);
                    let expected = topology.iter().enumerate().any(|(j, &flags)| {
                        if flags & ocean == 0 {
                            return false;
                        }
                        let low = [j % resolution, j / resolution]
                            .map(|v| v as f64 / resolution as f64 * 1000. - 500.);
                        let high = low.map(|v| v + 1000. / resolution as f64);
                        let dx = (low[0] - p[0]).max(p[0] - high[0]).max(0.);
                        let dz = (low[1] - p[1]).max(p[1] - high[1]).max(0.);
                        dx * dx + dz * dz <= f64::from(radius) * f64::from(radius)
                    });
                    assert_eq!(
                        excluded != 0,
                        expected,
                        "resolution {resolution}, radius {radius}, cell {i}"
                    );
                }
            }
        }
    }
    #[test]
    fn entrance_recovery_merges_hole_and_retry_masks_atomically() {
        let existing = [0, 0, 1, 0, 0];
        let retry = [0, 1, 0, 0, 0];
        assert_eq!(
            entrance_retry_mask(&existing, &retry, 5, || false)
                .unwrap()
                .unwrap(),
            [0, 1, 1, 0, 0],
            "Preserve both terrain holes and hard retry regions"
        );
        assert!(entrance_retry_mask(&existing, &retry, 4, || false).is_err());
        assert!(entrance_retry_mask(&existing[..4], &retry, 5, || false).is_err());
        assert!(entrance_retry_mask(&[2; 5], &retry, 5, || false).is_err());
        assert!(entrance_retry_mask(&existing, &[2; 5], 5, || false).is_err());
        let calls = std::cell::Cell::new(0);
        assert!(
            entrance_retry_mask(&existing, &retry, 5, || {
                calls.set(calls.get() + 1);
                calls.get() >= 3
            })
            .unwrap()
            .is_none()
        );
        assert_eq!(existing, [0, 0, 1, 0, 0]);
        assert_eq!(retry, [0, 1, 0, 0, 0]);
    }
    #[test]
    fn hole_clearance_matches_native_alpha_cells_and_counts_all_work() {
        let lattice = papyrus_terrain::road_costmap::CellLattice::new(80., 10.).unwrap();
        let n = 16;
        let mut alpha = vec![255; n * n];
        alpha[7 * n + 6] = 0;
        alpha[8 * n + 6] = 127;
        alpha[9 * n + 6] = 128;
        let before = alpha.clone();
        let (mask, work) = entrance_hole_mask(&alpha, n, lattice, 2.5, 10000, || false)
            .unwrap()
            .unwrap();
        for z in 0..8 {
            for x in 0..8 {
                let p = lattice.point([x, z]).unwrap();
                let blocked = alpha.iter().enumerate().any(|(i, &a)| {
                    let low = [i % n, i / n].map(|v| v as f32 / 16. * 80. - 40.);
                    a < 128 && (0..2).all(|c| p[c] >= low[c] - 2.5 && p[c] <= low[c] + 5. + 2.5)
                });
                assert_eq!(mask[z * 8 + x] != 0, blocked);
            }
        }
        assert_eq!(alpha, before);
        assert_eq!(
            entrance_hole_mask(&alpha, n, lattice, 2.5, work, || false)
                .unwrap()
                .unwrap()
                .0,
            mask
        );
        assert!(entrance_hole_mask(&alpha, n, lattice, 2.5, work - 1, || false).is_err());
        assert!(
            entrance_hole_mask(&alpha, n, lattice, 2.5, work, || true)
                .unwrap()
                .is_none()
        );
        assert!(entrance_hole_mask(&alpha, n, lattice, f32::NAN, work, || false).is_err());
        assert!(entrance_hole_mask(&alpha[..10], n, lattice, 2.5, work, || false).is_err());
        let mut edge_alpha = vec![255; 32 * 32];
        edge_alpha[0] = 0;
        let (edge, _) = entrance_hole_mask(&edge_alpha, 32, lattice, 0.1, 10000, || false)
            .unwrap()
            .unwrap();
        assert!(
            edge.iter().all(|&v| v == 0),
            "An edge hole far from any source cell must not block its nearest cell"
        );
    }
    fn sampled_loop(corners: &[[f32; 2]]) -> Vec<[f32; 3]> {
        let mut points = Vec::new();
        for pair in corners.windows(2) {
            let count =
                ((pair[1][0] - pair[0][0]).hypot(pair[1][1] - pair[0][1]) / 7.5).ceil() as usize;
            for i in 0..count {
                let t = i as f32 / count as f32;
                points.push([
                    pair[0][0] + (pair[1][0] - pair[0][0]) * t,
                    5.,
                    pair[0][1] + (pair[1][1] - pair[0][1]) * t,
                ]);
            }
        }
        points.push(points[0]);
        points
    }
    fn shape_settings() -> path_geometry::Settings {
        path_geometry::Settings {
            horizontal_iterations: 4,
            vertical_iterations: 16,
            resample_distance: 7.5,
            maximum_work: 2_000_000,
        }
    }
    fn build_test_ring(
        points: &[[f32; 3]],
        settings: path_geometry::Settings,
    ) -> Result<Option<ring::Shaped>, String> {
        path_geometry::build(points, settings, None, || false)
            .map(|p| {
                p.map(|path| ring::Shaped {
                    total_work: path.work(),
                    path,
                })
            })
            .map_err(|e| format!("{e:?}"))
    }

    #[test]
    fn level_strip_preference_detects_a_fold_hidden_by_the_original_grades() {
        let bend = [
            [-220.10294, 16.53446, -416.4685],
            [-214.42088, 17.252834, -421.34512],
            [-208.27246, 17.521906, -425.60294],
            [-206.50555, 14.347281, -420.79645],
            [-199.86375, 12.593048, -420.5007],
            [-192.88535, 12.293511, -423.26501],
        ];
        let before = (1..=25).rev().map(|i| {
            let [x, y, z] = bend[0];
            [x - 5.7 * i as f32, y, z + 4.88 * i as f32]
        });
        let after = (1..=25).map(|i| {
            let [x, y, z] = bend[5];
            [x + 6.98 * i as f32, y, z - 2.77 * i as f32]
        });
        let points: Vec<_> = before.chain(bend).chain(after).collect();
        let settings = shape_settings();
        let build = |settings: path_geometry::Settings| {
            let mut controls = points.clone();
            if settings.horizontal_iterations > 4 {
                for axis in [0, 2] {
                    controls[27][axis] = (controls[26][axis] + controls[28][axis]) * 0.5;
                }
            }
            let path = path_geometry::Path::from_controls(
                &controls, settings.maximum_work, || false,
            ).map_err(|error| format!("{error:?}"))?.unwrap();
            Ok(Some(ring::Shaped { total_work: path.work(), path }))
        };
        let (native, _) = shape_source_path(settings, 2., false, build, &|| false)
            .unwrap().unwrap();
        assert_eq!(native.path.points(), points);
        let ShapeOutcome::Clear(guided, receipt) =
            try_shape_source_path(settings, 2., false, true, build, &|| false).unwrap()
        else {
            panic!("Expected a candidate with a level strip");
        };
        assert_eq!(receipt["attempts"][0]["strip_clear"], true);
        assert_eq!(receipt["attempts"][0]["level_strip_clear"], false);
        assert_eq!(receipt["selected_horizontal_iterations"], 5);
        assert_eq!(receipt["attempts"][1]["level_strip_clear"], true);
        assert_eq!(guided.path.points()[27][1], points[27][1]);
        assert!(guided.total_work <= settings.maximum_work);

        let keep_native = |s: path_geometry::Settings| {
            build(path_geometry::Settings { horizontal_iterations: 4, ..s })
        };
        let ShapeOutcome::Clear(retained, receipt) =
            try_shape_source_path(settings, 2., false, true, keep_native, &|| false).unwrap()
        else {
            panic!("Expected the unmodified fallback to reach final validation");
        };
        assert_eq!(retained.path.points(), points);
        assert_eq!(receipt["selected_horizontal_iterations"], 4);
        assert!(retained.total_work <= settings.maximum_work);
    }

    #[test]
    fn an_open_road_can_remove_a_folded_retrace_without_losing_other_terminals() {
        let lattice = papyrus_terrain::road_costmap::CellLattice::new(1000., 7.5).unwrap();
        let cells = (0..=16)
            .chain((6..16).rev())
            .map(|x| [x, 4])
            .chain((1..=18).map(|i| [6 + i, 4 + i]));
        let nodes: Vec<_> = cells.map(|cell| ring::RawNode { cell, cost: 1 }).collect();
        let mut segment = layout::Segment {
            lattice,
            nodes: nodes.clone(),
            origin: Some(3),
            target: Some(9),
        };
        let settings = shape_settings();
        let build = |segment: &layout::Segment, settings| {
            let controls = segment.nodes.iter().map(|node| {
                [node.cell[0] as f32 * 7.5, 20., node.cell[1] as f32 * 7.5]
            }).collect::<Vec<_>>();
            build_test_ring(&controls, settings)
        };
        let protected = BTreeSet::from([[16, 4]]);
        assert!(shape_open_road(&mut segment, &protected, settings, 3., false, build, &|| false).is_err());
        assert_eq!(segment.nodes, nodes);
        let (path, receipt) =
            shape_open_road(&mut segment, &BTreeSet::new(), settings, 3., false, build, &|| false)
                .unwrap().unwrap();
        assert_eq!(receipt["retraced_nodes_removed"], 20);
        assert_eq!(segment.origin, Some(3));
        assert_eq!(segment.target, Some(9));
        assert_eq!(segment.nodes.first(), nodes.first());
        assert_eq!(segment.nodes.last(), nodes.last());
        assert!(path.total_work <= settings.maximum_work);
    }

    #[test]
    fn source_open_road_geometry_repairs_folds_before_binding_junctions() {
        let points = [
            [0., 20., 0.],
            [7.5, 20., 0.],
            [15., 20., 0.],
            [7.5, 20., 7.5],
            [15., 20., 15.],
            [22.5, 20., 15.],
            [30., 20., 15.],
        ];
        let settings = path_geometry::Settings {
            horizontal_iterations: 0,
            vertical_iterations: 0,
            resample_distance: 7.5,
            maximum_work: 1_000_000,
        };
        let original = build_test_ring(&points, settings).unwrap().unwrap();
        let (repaired, receipt) = shape_source_path(
            settings,
            5.,
            false,
            |s| build_test_ring(&points, s),
            &|| false,
        )
        .unwrap()
        .unwrap();
        assert!(!repaired.path.is_circular());
        assert_eq!(
            repaired.path.points().first(),
            original.path.points().first()
        );
        assert!(receipt["selected_horizontal_iterations"].as_u64().unwrap() > 0);
        assert_eq!(receipt["engine"], "papyrus-source-road-geometry-1");
        let clear = papyrus_terrain::road_strip::check(
            &repaired.path,
            &vec![true; repaired.path.points().len()],
            5.,
            |_| Ok::<_, String>(()),
            &|| false,
        )
        .unwrap();
        assert_eq!(clear, Some(true));
        assert!(repaired.total_work <= settings.maximum_work);
    }
    #[test]
    fn source_ring_geometry_keeps_valid_shaping_and_repairs_a_narrow_hairpin() {
        let clear = sampled_loop(&[[0., 0.], [600., 0.], [600., 600.], [0., 600.], [0., 0.]]);
        let before = clear.clone();
        let original = build_test_ring(&clear, shape_settings()).unwrap().unwrap();
        let (same, receipt) = shape_source_ring(
            shape_settings(),
            6.,
            |s| build_test_ring(&clear, s),
            &|| false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(same.path.points(), original.path.points());
        assert_eq!(same.path.tangents(), original.path.tangents());
        assert_eq!(receipt["recovered"], false);
        assert_eq!(receipt["attempts"].as_array().unwrap().len(), 1);
        assert_eq!(clear, before);
        let needle = sampled_loop(&[
            [0., 0.],
            [600., 0.],
            [600., 600.],
            [307.5, 600.],
            [307.5, 630.],
            [300., 630.],
            [300., 600.],
            [0., 600.],
            [0., 0.],
        ]);
        let before = needle.clone();
        let original = build_test_ring(&needle, shape_settings()).unwrap().unwrap();
        assert_eq!(
            papyrus_terrain::road_strip::check(
                &original.path,
                &vec![true; original.path.points().len()],
                6.,
                |_| Ok::<_, String>(()),
                &|| false
            )
            .unwrap(),
            Some(false)
        );
        let (repaired, receipt) = shape_source_ring(
            shape_settings(),
            6.,
            |s| build_test_ring(&needle, s),
            &|| false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(receipt["recovered"], true);
        assert!(repaired.path.is_circular());
        assert_eq!(
            papyrus_terrain::road_strip::check(
                &repaired.path,
                &vec![true; repaired.path.points().len()],
                6.,
                |_| Ok::<_, String>(()),
                &|| false
            )
            .unwrap(),
            Some(true)
        );
        let (again, same_receipt) = shape_source_ring(
            shape_settings(),
            6.,
            |s| build_test_ring(&needle, s),
            &|| false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(again.path.points(), repaired.path.points());
        assert_eq!(same_receipt, receipt);
        assert_eq!(needle, before);
        assert!(repaired.total_work <= shape_settings().maximum_work);
        // Each attempt fits on its own, but a second attempt must not reset
        // the shared allowance after spending the entire first attempt's work.
        let first = &receipt["attempts"][0];
        let first_work = ["shape_work", "strip_work", "controls"]
            .iter()
            .map(|key| first[*key].as_u64().unwrap() as usize)
            .sum();
        let calls = std::cell::Cell::new(0);
        let limited = path_geometry::Settings {
            maximum_work: first_work,
            ..shape_settings()
        };
        assert!(
            shape_source_ring(
                limited,
                6.,
                |s| {
                    calls.set(calls.get() + 1);
                    build_test_ring(&needle, s)
                },
                &|| false
            )
            .is_err()
        );
        assert_eq!(calls.get(), 1);
    }
    #[test]
    fn source_ring_geometry_cancellation_and_shared_budget_publish_no_candidate() {
        let points = sampled_loop(&[[0., 0.], [600., 0.], [600., 600.], [0., 600.], [0., 0.]]);
        assert!(
            shape_source_ring(
                shape_settings(),
                6.,
                |_| panic!("Cancelled build ran"),
                &|| true
            )
            .unwrap()
            .is_none()
        );
        let settings = path_geometry::Settings {
            maximum_work: 1,
            ..shape_settings()
        };
        assert!(
            shape_source_ring(settings, 6., |s| build_test_ring(&points, s), &|| false).is_err()
        );
        assert!(
            shape_source_ring(
                shape_settings(),
                f32::NAN,
                |_| panic!("Invalid radius reached builder"),
                &|| false
            )
            .is_err()
        );
        // Stop during strip sampling, after a valid first shape has been made.
        let calls = std::cell::Cell::new(0);
        let cancelled = || {
            calls.set(calls.get() + 1);
            calls.get() > 4
        };
        assert!(
            shape_source_ring(
                shape_settings(),
                6.,
                |s| build_test_ring(&points, s),
                &cancelled
            )
            .unwrap()
            .is_none()
        );
    }
    #[test]
    fn policy_rejects_unknown_engine_and_unpinned_inputs() {
        let mut p = Policy {
            engine: VERSION.into(),
            assets_sha256: "a".repeat(64),
            roles_sha256: "b".repeat(64),
            ring_clearance: None,
            terrain_entrances: None,
        };
        assert!(p.validate().is_ok());
        p.engine = LEGACY_VERSION.into();
        assert!(p.validate().is_ok());
        let legacy = serde_json::to_vec(&p).unwrap();
        let restored: Policy = serde_json::from_slice(&legacy).unwrap();
        assert_eq!(serde_json::to_vec(&restored).unwrap(), legacy);
        p.engine = VERSION.into();
        assert!(
            serde_json::to_value(&p)
                .unwrap()
                .get("terrain_entrances")
                .is_none()
        );
        p.terrain_entrances = Some(source_road_route::TERRAIN_ENTRANCE_VERSION.into());
        let restored: Policy = serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap();
        assert_eq!(restored, p);
        assert!(restored.validate().is_ok());
        p.terrain_entrances = Some(source_road_route::TERRAIN_ENTRANCE_ROUTED_VERSION.into());
        let routed: Policy = serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap();
        assert!(routed.validate().is_ok());
        assert_eq!(routed, p);
        p.terrain_entrances = Some(source_road_route::TERRAIN_ENTRANCE_BOOTSTRAP_VERSION.into());
        let bootstrap: Policy = serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap();
        assert!(bootstrap.validate().is_ok());
        assert_eq!(bootstrap, p);
        p.terrain_entrances = Some(source_road_route::TERRAIN_ENTRANCE_PINNED_VERSION.into());
        let pinned: Policy = serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap();
        assert!(pinned.validate().is_ok());
        assert_eq!(pinned, p);
        p.terrain_entrances = Some("future".into());
        assert!(p.validate().is_err());
        p.terrain_entrances = None;
        assert!(
            serde_json::to_value(&p)
                .unwrap()
                .get("ring_clearance")
                .is_none()
        );
        p.ring_clearance = Some(RingClearance {
            engine: RING_CLEARANCE_VERSION.into(),
            radius_cells: 1,
        });
        assert!(p.validate().is_ok());
        p.ring_clearance.as_mut().unwrap().engine = RING_CLEARANCE_LEGACY_VERSION.into();
        let legacy: Policy = serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap();
        assert_eq!(legacy, p);
        assert!(legacy.validate().is_ok());
        p.ring_clearance.as_mut().unwrap().engine = "future".into();
        assert!(p.validate().is_err());
        p.ring_clearance.as_mut().unwrap().engine = RING_CLEARANCE_VERSION.into();
        p.ring_clearance.as_mut().unwrap().radius_cells = 0;
        assert!(p.validate().is_err());
        p.ring_clearance = None;
        let copy: Policy = serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap();
        assert_eq!(copy, p);
        p.engine = "future".into();
        assert!(p.validate().is_err());
        p.engine = VERSION.into();
        p.assets_sha256 = "A".repeat(64);
        assert!(p.validate().is_err());
        p.assets_sha256 = "a".repeat(64);
        p.roles_sha256.clear();
        assert!(p.validate().is_err());
    }
}
