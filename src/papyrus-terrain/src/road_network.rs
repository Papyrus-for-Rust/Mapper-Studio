//! Independent roads connecting authored access sockets on private composed terrain.
//! No official parity, ring roads, bridges, runtime meshes or rail generation.
use crate::{
    erosion::{decode, encode},
    native_layers::Layers,
    routing,
};
use std::collections::BTreeSet;
pub mod socket_recovery;
pub const VERSION: &str = "papyrus-access-roads-1";
/// Opt-in routing to an existing network. Legacy builds retain VERSION behavior.
pub const ACCESS_VERSION: &str = "papyrus-access-entrances-3";
pub const SHAPED_VERSION: &str = "papyrus-access-shaped-4";
pub const RING_VERSION: &str = "papyrus-road-network-ring-5";
pub const COST_VERSION: &str = "papyrus-road-access-costs-6";
pub const COMPONENT_VERSION: &str = "papyrus-connected-road-exits-1";
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum LaunchEngine {
    #[serde(rename = "papyrus-checked-road-launch-1")]
    Checked,
    #[serde(rename = "papyrus-checked-road-launch-2")]
    Fitted,
    #[serde(rename = "papyrus-checked-road-launch-3")]
    PreserveEntrances,
    #[serde(rename = "papyrus-checked-road-launch-4")]
    AdaptiveEntrances,
    #[serde(rename = "papyrus-checked-road-launch-5")]
    ProtectedEdges,
    #[serde(rename = "papyrus-checked-road-launch-6")]
    BedFeedback,
    /// Preserve the bed-feedback path and reject folded raw fallbacks too.
    #[serde(rename = "papyrus-checked-road-launch-7")]
    SurfaceChecked,
    #[serde(rename = "papyrus-checked-road-launch-8")]
    GlobalEarthwork,
}
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchPolicy {
    pub engine: LaunchEngine,
    pub minimum_run_widths: f32,
    pub sample_spacing: f32,
    pub maximum_work: usize,
}
impl LaunchPolicy {
    pub fn validate(self) -> Result<(), String> {
        if !self.minimum_run_widths.is_finite()
            || !(0.5..=4.).contains(&self.minimum_run_widths)
            || !self.sample_spacing.is_finite()
            || !(0.5..=4.).contains(&self.sample_spacing)
            || !(1024..=8_000_000).contains(&self.maximum_work)
        {
            return Err("Invalid checked road launch policy".into());
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TraversalPolicy {
    pub traversable_topology: i32,
    pub normal_cost: u32,
    pub traversable_cost: u32,
}
impl TraversalPolicy {
    pub fn validate(self) -> Result<(), String> {
        if (self.traversable_topology as u32).count_ones() != 1
            || self.normal_cost == 0
            || self.traversable_cost < self.normal_cost
            || self.traversable_cost > 1_000_000
        {
            return Err("Invalid source road traversal policy".into());
        }
        Ok(())
    }
}
pub const FAMILY_VERSION: &str = "papyrus-access-network-2";
pub const NETWORK_VERSION: &str = "papyrus-access-road-attachments-1";
const MAX_NETWORK_POINTS: usize = 65_536;
const MAX_NETWORK_TARGETS: usize = 1024;
const GRID: usize = 257;
const MAX_ATTEMPTS: usize = 128;
const MAX_WORK: usize = 32_000_000;
const MAX_CORRIDOR_CELLS: usize = 1_000_000;
#[derive(Clone, Debug)]
pub struct Socket {
    pub monument: u64,
    pub connector: i64,
    pub position: [f32; 3],
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Obstacle {
    pub monument: u64,
    pub center: [f32; 2],
    pub extent: [f32; 2],
    pub yaw: f32,
}
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    pub seed: u32,
    pub width: f32,
    pub shoulder: f32,
    pub maximum_grade: f32,
    pub maximum_cut_fill: f32,
    pub terrain_offset: f32,
    pub road_topology: i32,
    pub roadside_topology: i32,
    pub protected_topology: i32,
    pub splat_channel: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destination {
    Loop,
    Monument {
        monument: u64,
        connector: i64,
    },
    /// Indices refer to the immutable input network, never a fabricated monument.
    Network {
        road: usize,
        point: usize,
    },
    /// Verified shared source cell; source endpoints stay at their original XZ.
    /// This proves graph identity, not final physical support.
    RawNetwork(crate::source_road_junctions::RawConnection),
}
impl Destination {
    fn json(self) -> serde_json::Value {
        match self {
            Self::Loop => serde_json::json!({"loop":true}),
            Self::Monument {
                monument,
                connector,
            } => serde_json::json!({"monument":monument,"connector":connector}),
            Self::Network { road, point } => {
                serde_json::json!({"network_road":road,"network_point":point})
            }
            Self::RawNetwork(connection) => serde_json::json!({"source_network":connection}),
        }
    }
}
#[derive(Debug)]
pub struct Road {
    pub from: Destination,
    pub to: Destination,
    pub points: Vec<[f32; 3]>,
}
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntrancePolicy {
    pub minimum_height: f32,
    pub owned_topology: i32,
    pub maximum_exit_distance: f32,
    pub maximum_exit_nodes: usize,
}
impl EntrancePolicy {
    pub fn validate(self) -> Result<(), String> {
        if self.minimum_height != 1.
            || self.owned_topology <= 0
            || !self.maximum_exit_distance.is_finite()
            || !(16. ..=512.).contains(&self.maximum_exit_distance)
            || !(16..=8192).contains(&self.maximum_exit_nodes)
        {
            return Err("Invalid road entrance policy".into());
        }
        Ok(())
    }
}
pub struct Access<'a> {
    pub settings: Settings,
    pub policy: EntrancePolicy,
    pub owner_footprints: &'a [Obstacle],
    pub network: Option<(&'a [Road], usize)>,
}
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShapingPolicy {
    pub horizontal_iterations: u16,
    pub vertical_iterations: u16,
    pub resample_distance: f32,
    pub maximum_work: usize,
}
impl ShapingPolicy {
    pub fn validate(self) -> Result<(), String> {
        if self.horizontal_iterations > 64
            || self.vertical_iterations > 64
            || !self.resample_distance.is_finite()
            || !(1. ..=32.).contains(&self.resample_distance)
            || !(1024..=2_000_000).contains(&self.maximum_work)
        {
            return Err("Invalid road shaping policy".into());
        }
        Ok(())
    }
}
#[derive(Debug, Default, serde::Serialize)]
pub struct ShapingAudit {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constrained: Option<crate::road_smoothing::Audit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entrance_recovery: Option<EntranceRecoveryAudit>,
    pub attempted: usize,
    pub accepted: usize,
    pub fallbacks: usize,
    pub budget_fallbacks: usize,
    pub work: usize,
    pub fallback_reasons: std::collections::BTreeMap<String, usize>,
}
#[derive(Debug, Default, serde::Serialize)]
pub struct EntranceRecoveryAudit {
    pub attempts: usize,
    pub proposals: usize,
    pub accepted: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved_grade_proposals: Option<usize>,
}
impl ShapingAudit {
    fn fallback(&mut self, reason: &str) {
        self.fallbacks += 1;
        *self.fallback_reasons.entry(reason.into()).or_default() += 1;
    }
}
struct ShapedPoints {
    points: Vec<[f32; 3]>,
    work: usize,
    saved_grade_fit: bool,
}
/// Exact segment/closed-cell intersections on the source routing raster.
/// Touching a blocked corner is rejected just as diagonal A* movement is.
fn source_shape_clear(
    points: &[[f32; 3]],
    costs: &[u32],
    size: f32,
    cancelled: &impl Fn() -> bool,
) -> Option<bool> {
    crate::road_smoothing::source_clear(points, costs, GRID, size, cancelled)
}

fn shaped_points(
    points: &[[f32; 3]],
    policy: ShapingPolicy,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<ShapedPoints>, crate::path_geometry::Failure> {
    let Some(path) = crate::path_geometry::build(
        points,
        crate::path_geometry::Settings {
            horizontal_iterations: policy.horizontal_iterations,
            vertical_iterations: policy.vertical_iterations,
            resample_distance: policy.resample_distance,
            maximum_work: policy.maximum_work,
        },
        None,
        cancelled,
    )?
    else {
        return Ok(None);
    };
    let mut output = path.points().to_vec();
    // Source-style resampling may omit a short final remainder. Network joins
    // must retain their exact endpoint even in that case.
    if output.last() != points.last() {
        output.push(*points.last().unwrap());
    }
    Ok(Some(ShapedPoints {
        points: output,
        work: path.work(),
        saved_grade_fit: false,
    }))
}
/// Remove only numerical resampling remainders, at the precision of this world.
/// Preserve both exact attachment poses, including a genuinely short whole path.
fn normalize_fitted_controls(points: &mut Vec<[f32; 3]>, size: u32) {
    if points.len() < 3 {
        return;
    }
    let tolerance = (size as f32 * f32::EPSILON * 4.).max(1e-4);
    let same =
        |a: [f32; 3], b: [f32; 3]| distance(a, b) <= tolerance && (a[1] - b[1]).abs() <= tolerance;
    let last = *points.last().unwrap();
    let mut clean = Vec::with_capacity(points.len());
    clean.push(points[0]);
    for &point in &points[1..points.len() - 1] {
        if !same(*clean.last().unwrap(), point) {
            clean.push(point);
        }
    }
    while clean.len() > 1 && same(*clean.last().unwrap(), last) {
        clean.pop();
    }
    clean.push(last);
    *points = clean;
}
/// Retry only the inland portion. Exact checked entrance chains are immutable;
/// inland heights are fitted before splicing so their slopes do not force a
/// protected entrance to satisfy the stricter proposal grade.
#[allow(clippy::too_many_arguments)]
fn shape_between_entrances(
    layers: &Layers,
    raw: &[[f32; 3]],
    range: (usize, usize),
    settings: Settings,
    policy: ShapingPolicy,
    water: Option<crate::road_water::Guide<'_>>,
    adaptive: bool,
    ground: Option<&[i16]>,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<ShapedPoints>, crate::rail_grade_fit::Failure> {
    use crate::rail_grade_fit::Failure;
    let (first, last) = range;
    if first >= last || last >= raw.len() {
        return Err(Failure::Invalid);
    }
    let Some(mut middle) =
        shaped_points(&raw[first..=last], policy, cancelled).map_err(|e| match e {
            crate::path_geometry::Failure::Budget => Failure::Budget,
            crate::path_geometry::Failure::Invalid => Failure::Invalid,
        })?
    else {
        return Ok(None);
    };
    normalize_fitted_controls(&mut middle.points, layers.size);
    let remaining = policy
        .maximum_work
        .checked_sub(middle.work)
        .filter(|v| *v > 0)
        .ok_or(Failure::Budget)?;
    let first_allowance = if adaptive { remaining / 2 } else { remaining };
    let initial = fit_road_heights_on_ground(
        layers,
        &mut middle.points,
        Settings {
            maximum_grade: settings.maximum_grade * 0.75,
            ..settings
        },
        first_allowance,
        water,
        false,
        true,
        ground,
        cancelled,
    );
    let (used, saved_grade_fit) = match initial {
        Ok((used, _)) => (used, false),
        Err(Failure::Infeasible) if adaptive => {
            let (used, _) = fit_road_heights_on_ground(
                layers,
                &mut middle.points,
                settings,
                remaining - first_allowance,
                water,
                false,
                true,
                ground,
                cancelled,
            )?;
            // Charge the complete reserved allowance for the failed first fit.
            (first_allowance + used, true)
        }
        Err(e) => return Err(e),
    };
    let mut points = raw[..first].to_vec();
    points.extend(middle.points);
    points.extend_from_slice(&raw[last + 1..]);
    Ok(Some(ShapedPoints {
        points,
        work: middle.work + used,
        saved_grade_fit,
    }))
}
fn candidate_strip_clear(
    points: &[[f32; 3]],
    width: f32,
    maximum_work: usize,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<(bool, usize)>, crate::path_geometry::Failure> {
    use crate::path_geometry::{Failure, Path};
    let Some(path) = Path::from_controls(points, maximum_work, cancelled)? else {
        return Ok(None);
    };
    let mut work = path.work();
    let clear = crate::road_strip::check(
        &path,
        &vec![true; points.len()],
        width * 0.5,
        |n| {
            work = work
                .checked_add(n)
                .filter(|v| *v <= maximum_work)
                .ok_or(Failure::Budget)?;
            Ok(())
        },
        cancelled,
    )?;
    Ok(clear.map(|clear| (clear, work)))
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct EntranceJoin {
    pub monument: u64,
    pub connector: i64,
    pub points: Vec<[f32; 3]>,
}
#[derive(Debug, serde::Serialize)]
pub struct GroundRejection {
    pub position: [f32; 3],
    pub topology: i32,
    pub reason: String,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Family {
    pub infrastructure_type: i32,
    pub hierarchy: u32,
    pub seed: u32,
    pub first_road: usize,
    pub road_count: usize,
    pub socket_count: usize,
    pub unresolved: Vec<u64>,
    pub width: f32,
    pub splat_channel: usize,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct RecipeReceipt {
    pub source_sha256: String,
    pub recipe_sha256: String,
    pub seed: u32,
}

#[derive(Debug, Default)]
pub struct Report {
    pub recipe: Option<RecipeReceipt>,
    pub source_layout: Option<serde_json::Value>,
    pub source_route_layout: Option<std::sync::Arc<crate::source_road_layout::AcceptedNetwork>>,
    pub entrance_launch: Option<LaunchPolicy>,
    pub socket_geometry_recovery: Vec<socket_recovery::Audit>,
    pub roads: Vec<Road>,
    pub unresolved: Vec<u64>,
    pub attempts: usize,
    pub rejected: usize,
    pub budget_exhausted: bool,
    pub changed_heights: usize,
    pub missing_metadata: Vec<u64>,
    pub socket_count: usize,
    pub settings: Option<Settings>,
    pub network_attachment_stage: bool,
    pub families: Vec<Family>,
    pub entrance_policy: Option<EntrancePolicy>,
    pub entrance_joins: Vec<EntranceJoin>,
    pub rejection_reasons: std::collections::BTreeMap<String, usize>,
    pub rejection_samples: Vec<GroundRejection>,
    pub shaping_policy: Option<ShapingPolicy>,
    pub shaping: ShapingAudit,
    pub surfaces: Option<SurfaceOutput>,
    pub ring: Option<crate::road_ring::Audit>,
    pub ring_costmap: Option<serde_json::Value>,
    pub river_water: Option<serde_json::Value>,
    pub access_river_water: Option<serde_json::Value>,
    pub source_access_costmaps: Vec<serde_json::Value>,
    pub access_routing: Option<TraversalPolicy>,
    pub trails_enabled: Option<bool>,
    pub access_search: Option<&'static str>,
    pub post_topology: Option<crate::road_topology::Report>,
}
#[derive(Debug, serde::Serialize)]
pub struct SurfaceChunk {
    pub road: usize,
    pub lod: usize,
    pub chunk_index: usize,
    pub geometry: crate::path_mesh::Chunk,
}
impl SurfaceChunk {
    pub fn filename(&self) -> String {
        format!(
            "road-surface-{}-lod-{}-chunk-{}.json",
            self.road, self.lod, self.chunk_index
        )
    }
}
#[derive(Debug)]
pub struct SurfaceOutput {
    pub report: serde_json::Value,
    pub chunks: Vec<SurfaceChunk>,
}
impl Report {
    fn reject(&mut self, reason: &str) {
        self.rejected += 1;
        if self.entrance_policy.is_some() {
            *self.rejection_reasons.entry(reason.into()).or_default() += 1;
        }
    }
    pub fn json(&self) -> serde_json::Value {
        let mut value = serde_json::json!({"settings":self.settings.map(|s|serde_json::json!({"width":s.width,"shoulder":s.shoulder,"terrain_offset":s.terrain_offset,"splat_channel":s.splat_channel,"road_topology":s.road_topology,"roadside_topology":s.roadside_topology})),"engine":if self.entrance_policy.is_some() { ACCESS_VERSION } else if !self.families.is_empty() { FAMILY_VERSION } else if self.network_attachment_stage { NETWORK_VERSION } else { VERSION },"stage":"access-road-corridors","playable":false,"socket_count":self.socket_count,
            "unresolved_monuments":self.unresolved,"missing_metadata":self.missing_metadata,"attempts":self.attempts,"rejected":self.rejected,
            "budget_exhausted":self.budget_exhausted,"changed_height_writes":self.changed_heights,
            "roads":self.roads.iter().map(|r|serde_json::json!({"from":r.from.json(),"to":r.to.json(),"points":r.points})).collect::<Vec<_>>()});
        if let Some(recipe) = &self.recipe {
            value["recipe"] = serde_json::json!(recipe);
        }
        if !self.socket_geometry_recovery.is_empty() {
            value["socket_geometry_recovery"] = serde_json::json!(self.socket_geometry_recovery);
        }
        if let Some(layout) = &self.source_layout {
            value["source_layout"] = layout.clone();
        }
        if let Some(launch) = self.entrance_launch {
            value["entrance_launch"] =
                serde_json::to_value(launch).expect("validated launch policy");
        }
        if let Some(summary) = &self.post_topology {
            value["post_topology"] =
                serde_json::to_value(summary).expect("validated road topology policy");
        }
        if !self.families.is_empty() {
            value["families"] =
                serde_json::to_value(&self.families).expect("validated finite road metadata");
        }
        if let Some(policy) = self.entrance_policy {
            value["entrance_policy"] =
                serde_json::to_value(policy).expect("validated road entrance policy");
            value["entrance_joins"] =
                serde_json::to_value(&self.entrance_joins).expect("validated road joins");
            value["rejection_reasons"] = serde_json::json!(self.rejection_reasons);
            value["rejection_samples"] = serde_json::json!(self.rejection_samples);
        }
        if let Some(policy) = self.shaping_policy {
            value["engine"] = SHAPED_VERSION.into();
            value["shaping_policy"] = serde_json::json!(policy);
            value["shaping"] = serde_json::json!(self.shaping);
            value["corridor_projection"] = "nearest-path-segment".into();
        }
        if let Some(search) = self.access_search {
            value["access_search"] = search.into();
        }
        if let Some(surfaces) = &self.surfaces {
            value["surfaces"] = surfaces.report.clone();
        }
        if let Some(ring) = &self.ring {
            value["engine"] = RING_VERSION.into();
            value["ring"] = serde_json::to_value(ring).expect("finite validated ring audit");
        }
        if let Some(policy) = self.access_routing {
            value["engine"] = COST_VERSION.into();
            value["access_routing"] =
                serde_json::to_value(policy).expect("validated traversal policy");
        }
        if let Some(water) = &self.river_water {
            value["river_water"] = water.clone();
        }
        if let Some(water) = &self.access_river_water {
            value["access_river_water"] = water.clone();
        }
        if let Some(costmap) = &self.ring_costmap {
            value["ring_costmap"] = costmap.clone();
        }
        if !self.source_access_costmaps.is_empty() {
            value["source_access_costmaps"] = serde_json::json!(self.source_access_costmaps);
        }
        if let Some(enabled) = self.trails_enabled {
            value["trails_enabled"] = enabled.into();
        }
        value
    }
}
fn protected(layers: &Layers, alpha: &[u8], s: Settings, x: f32, z: f32) -> bool {
    let n = layers.surface_resolution;
    let cell = |w: f32| {
        ((w / layers.size as f32 + 0.5) * n as f32)
            .floor()
            .clamp(0., (n - 1) as f32) as usize
    };
    let i = cell(z) * n + cell(x);
    alpha[i] < 128 || layers.topology[i] & s.protected_topology != 0
}
fn inside(o: &Obstacle, x: f32, z: f32, padding: f32) -> bool {
    inside_with_rotation(o, x, z, padding, o.yaw.to_radians().sin_cos())
}
fn inside_with_rotation(
    o: &Obstacle,
    x: f32,
    z: f32,
    padding: f32,
    (s, c): (f32, f32),
) -> bool {
    let dx = x - o.center[0];
    let dz = z - o.center[1];
    (c * dx + s * dz).abs() < o.extent[0] + padding
        && (-s * dx + c * dz).abs() < o.extent[1] + padding
}
fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    (a[0] - b[0]).hypot(a[2] - b[2])
}
/// Check the native ground footprint, including narrow water/protection features
/// that can disappear between coarse route samples.
#[cfg(test)]
fn ground_clear(
    layers: &Layers,
    alpha: &[u8],
    settings: Settings,
    center: [f32; 2],
    radius: f32,
    owner: Option<(EntrancePolicy, &[Obstacle], Option<u64>)>,
) -> bool {
    ground_clear_with_water(layers, alpha, settings, center, radius, owner, None)
}
fn ground_clear_with_water(
    layers: &Layers,
    alpha: &[u8],
    settings: Settings,
    center: [f32; 2],
    radius: f32,
    owner: Option<(EntrancePolicy, &[Obstacle], Option<u64>)>,
    water: Option<crate::road_water::Guide<'_>>,
) -> bool {
    ground_clear_with_water_report(
        layers,
        alpha,
        settings,
        center,
        radius,
        owner,
        water,
        |_| {},
    )
}
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct GroundBlock {
    pub position: [f32; 3],
    pub topology: i32,
    pub reason: &'static str,
}
#[allow(clippy::too_many_arguments)]
fn ground_clear_with_water_report(
    layers: &Layers,
    alpha: &[u8],
    settings: Settings,
    center: [f32; 2],
    radius: f32,
    owner: Option<(EntrancePolicy, &[Obstacle], Option<u64>)>,
    water: Option<crate::road_water::Guide<'_>>,
    mut blocked: impl FnMut(GroundBlock),
) -> bool {
    ground_clear_with_water_filter(layers, alpha, settings, center, radius, owner, water, |b| {
        blocked(b);
        true
    })
}
#[allow(clippy::too_many_arguments)]
fn ground_clear_with_water_filter(
    layers: &Layers,
    alpha: &[u8],
    settings: Settings,
    center: [f32; 2],
    radius: f32,
    owner: Option<(EntrancePolicy, &[Obstacle], Option<u64>)>,
    water: Option<crate::road_water::Guide<'_>>,
    blocked: impl FnMut(GroundBlock) -> bool,
) -> bool {
    ground_clear_with_water_filter_on_height(
        layers,
        &layers.height,
        None,
        1.,
        alpha,
        settings,
        center,
        radius,
        owner,
        water,
        |index| ground_axis(layers, index),
        None::<fn(usize, usize) -> u32>,
        blocked,
    )
}

fn ground_axis(layers: &Layers, index: usize) -> (f32, usize) {
    let intervals = layers.height_resolution - 1;
    let surface = layers.surface_resolution;
    (
        (index as f32 / intervals as f32 - 0.5) * layers.size as f32,
        (index * surface / intervals).min(surface - 1),
    )
}

fn ground_radius_band(radius: f32) -> [f64; 2] {
    if !radius.is_normal() || radius < 0. {
        return [f64::NEG_INFINITY, f64::INFINITY];
    }
    let squared = f64::from(radius).powi(2);
    // f64 retains the f32 inputs exactly. Leave room for rounding the
    // hypot result back to f32 around the inclusive circle boundary.
    let tolerance = 4. * f64::from(f32::EPSILON);
    [squared * (1. - tolerance), squared * (1. + tolerance)]
}

#[inline]
fn outside_ground_circle(
    dx: f32,
    dz: f32,
    dz_squared: f64,
    radius: f32,
    [near, far]: [f64; 2],
) -> bool {
    let distance_squared = f64::from(dx).powi(2) + dz_squared;
    distance_squared > far || (distance_squared >= near && dx.hypot(dz) > radius)
}

#[inline]
fn ground_topology(topology: i32, ocean: Option<&crate::ocean::Mask>, index: usize) -> i32 {
    ocean.map_or(topology, |mask| {
        (topology & !crate::ocean::OCEAN_BIT)
            | if mask.classification[index] == crate::ocean::OCEAN {
                crate::ocean::OCEAN_BIT
            } else {
                0
            }
    })
}

#[allow(clippy::too_many_arguments)]
fn ground_clear_with_water_filter_on_height(
    layers: &Layers,
    height: &[i16],
    ocean: Option<&crate::ocean::Mask>,
    minimum_dry_height: f32,
    alpha: &[u8],
    settings: Settings,
    center: [f32; 2],
    radius: f32,
    owner: Option<(EntrancePolicy, &[Obstacle], Option<u64>)>,
    water: Option<crate::road_water::Guide<'_>>,
    axis: impl Fn(usize) -> (f32, usize),
    mut candidate_word: Option<impl FnMut(usize, usize) -> u32>,
    mut blocked: impl FnMut(GroundBlock) -> bool,
) -> bool {
    let hr = layers.height_resolution;
    let sr = layers.surface_resolution;
    let size = layers.size as f32;
    let index = |v: f32| {
        ((v / size + 0.5) * (hr - 1) as f32)
            .floor()
            .clamp(0., (hr - 1) as f32) as usize
    };
    let first = index(center[0] - radius);
    let last = (index(center[0] + radius) + 1).min(hr - 1);
    let band = ground_radius_band(radius);
    for z in index(center[1] - radius)..=(index(center[1] + radius) + 1).min(hr - 1) {
        let (wz, sz) = axis(z);
        let dz = wz - center[1];
        let dz_squared = f64::from(dz).powi(2);
        let row = z * hr;
        let surface_row = sz * sr;
        let mut clear_at = |x| {
            let (wx, sx) = axis(x);
            if outside_ground_circle(wx - center[0], dz, dz_squared, radius, band) {
                return true;
            }
            let ground = decode(height[row + x]);
            let i = surface_row + sx;
            let topology = ground_topology(layers.topology[i], ocean, i);
            if ground <= minimum_dry_height
                && water.and_then(|w| w.minimum(layers, wx, wz)).is_none()
            {
                return !blocked(GroundBlock {
                    position: [wx, ground, wz],
                    topology,
                    reason: "water or low ground",
                });
            }
            if alpha[i] < 128 {
                return !blocked(GroundBlock {
                    position: [wx, ground, wz],
                    topology,
                    reason: "terrain hole",
                });
            }
            let mask = topology & settings.protected_topology;
            if mask != 0
                && !owner.is_some_and(|(policy, owners, id)| {
                    mask & !policy.owned_topology == 0
                        && owners
                            .iter()
                            .any(|o| Some(o.monument) == id && inside(o, wx, wz, 0.))
                        && !owners
                            .iter()
                            .any(|o| Some(o.monument) != id && inside(o, wx, wz, 0.))
                })
            {
                return !blocked(GroundBlock {
                    position: [wx, ground, wz],
                    topology,
                    reason: "unowned protected ground",
                });
            }
            true
        };
        if let Some(word) = &mut candidate_word {
            // Visit the marked columns in the same order as the full scan.
            for tile in first / 32..=last / 32 {
                let start = tile * 32;
                let left = first.saturating_sub(start);
                let right = (last - start).min(31);
                let mut bits = word(z, tile) & (u32::MAX << left) & (u32::MAX >> (31 - right));
                while bits != 0 {
                    let x = start + bits.trailing_zeros() as usize;
                    if !clear_at(x) {
                        return false;
                    }
                    bits &= bits - 1;
                }
            }
        } else if !(first..=last).all(clear_at) {
            return false;
        }
    }
    true
}
fn normalize(values: [f32; 8]) -> [u8; 8] {
    let total = values.iter().sum::<f32>();
    if total <= 0. {
        return [255, 0, 0, 0, 0, 0, 0, 0];
    }
    let scaled = values.map(|v| v / total * 255.);
    let mut bytes = scaled.map(|v| v.floor() as u8);
    let mut order: [usize; 8] = std::array::from_fn(|i| i);
    order.sort_by(|&a, &b| {
        (scaled[b] - bytes[b] as f32)
            .total_cmp(&(scaled[a] - bytes[a] as f32))
            .then(a.cmp(&b))
    });
    let remainder = 255 - bytes.iter().map(|&v| v as usize).sum::<usize>();
    for &i in order.iter().take(remainder) {
        bytes[i] += 1;
    }
    bytes
}
/// Iterate bounded corridor footprints; callbacks can reject before any candidate writes.
fn corridor(
    layers: &Layers,
    points: &[[f32; 3]],
    raster: (usize, bool),
    s: Settings,
    work: &mut usize,
    cancelled: &impl Fn() -> bool,
    visit: impl FnMut(usize, f32, f32, f32, f32) -> bool,
) -> Result<bool, String> {
    corridor_rows(layers, points, raster, s, work, cancelled, false, visit)
}
/// Conservative row intersection of a segment capsule. The final f32 distance
/// predicate remains authoritative; these bounds only skip impossible cells.
fn capsule_row(a: [f32; 3], b: [f32; 3], z: f32, radius: f64) -> Option<[f64; 2]> {
    let (ax, az, bx, bz, z) = (
        f64::from(a[0]),
        f64::from(a[2]),
        f64::from(b[0]),
        f64::from(b[2]),
        f64::from(z),
    );
    let (dx, dz) = (bx - ax, bz - az);
    let length2 = dx * dx + dz * dz;
    let mut bounds = [f64::INFINITY, f64::NEG_INFINITY];
    let mut include = |lo: f64, hi: f64| {
        if lo <= hi {
            bounds[0] = bounds[0].min(lo);
            bounds[1] = bounds[1].max(hi);
        }
    };
    for (x, cz) in [(ax, az), (bx, bz)] {
        let remaining = radius * radius - (z - cz) * (z - cz);
        if remaining >= 0. {
            let half = remaining.sqrt();
            include(x - half, x + half);
        }
    }
    if dz == 0. {
        if (z - az).abs() <= radius {
            include(ax.min(bx), ax.max(bx));
        }
    } else {
        let center = ax + dx * (z - az) / dz;
        let half = radius * length2.sqrt() / dz.abs();
        let (mut lo, mut hi) = (center - half, center + half);
        if dx == 0. {
            let projection = dz * (z - az);
            if projection < 0. || projection > length2 {
                lo = f64::INFINITY;
            }
        } else {
            let p = ax - dz * (z - az) / dx;
            let q = ax + (length2 - dz * (z - az)) / dx;
            lo = lo.max(p.min(q));
            hi = hi.min(p.max(q));
        }
        include(lo, hi);
    }
    (bounds[0] <= bounds[1]).then_some(bounds)
}
#[allow(clippy::too_many_arguments)]
fn corridor_rows(
    layers: &Layers,
    points: &[[f32; 3]],
    raster: (usize, bool),
    s: Settings,
    work: &mut usize,
    cancelled: &impl Fn() -> bool,
    clip_rows: bool,
    mut visit: impl FnMut(usize, f32, f32, f32, f32) -> bool,
) -> Result<bool, String> {
    let (n, centers) = raster;
    let size = layers.size as f32;
    let radius = s.width * 0.5 + s.shoulder;
    let scale = if centers { n as f32 } else { (n - 1) as f32 };
    let offset = if centers { 0.5 } else { 0. };
    let index = |v: f32| {
        ((v / size + 0.5) * scale - offset)
            .floor()
            .clamp(0., (n - 1) as f32) as usize
    };
    for segment in points.windows(2) {
        let [a, b] = [segment[0], segment[1]];
        let dx = b[0] - a[0];
        let dz = b[2] - a[2];
        let length2 = dx * dx + dz * dz;
        if length2 < 1e-8 {
            continue;
        }
        let minx = index(a[0].min(b[0]) - radius);
        let maxx = (index(a[0].max(b[0]) + radius) + 1).min(n - 1);
        let minz = index(a[2].min(b[2]) - radius);
        let maxz = (index(a[2].max(b[2]) + radius) + 1).min(n - 1);
        for z in minz..=maxz {
            if cancelled() {
                return Ok(false);
            }
            let wz = ((z as f32 + offset) / scale - 0.5) * size;
            let (first, last) = if clip_rows {
                // Cover f32 projection error, then retain an extra grid column
                // on each side to cover the inverse grid-coordinate rounding.
                let padding = 0.001 + f64::from(size) * f64::from(f32::EPSILON) * 8.;
                let Some([lo, hi]) = capsule_row(a, b, wz, f64::from(radius) + padding) else {
                    continue;
                };
                (
                    minx.max(index(lo as f32).saturating_sub(1)),
                    maxx.min((index(hi as f32) + 1).min(n - 1)),
                )
            } else {
                (minx, maxx)
            };
            for x in first..=last {
                *work += 1;
                if *work > MAX_WORK {
                    return Err("Road corridor work budget reached".into());
                }
                let wx = ((x as f32 + offset) / scale - 0.5) * size;
                let t = (((wx - a[0]) * dx + (wz - a[2]) * dz) / length2).clamp(0., 1.);
                let d = (wx - a[0] - t * dx).hypot(wz - a[2] - t * dz);
                if d > radius {
                    continue;
                }
                let y = a[1] + t * (b[1] - a[1]) + s.terrain_offset;
                if !visit(z * n + x, d, y, wx, wz) {
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}
/// The shaped revision projects each raster cell onto its nearest path segment.
/// Shared segment end caps must not accumulate paint or overwrite a closer
/// segment's target height. Legacy projections retain their original behavior.
fn corridor_mode(
    layers: &Layers,
    points: &[[f32; 3]],
    raster: (usize, bool, bool),
    s: Settings,
    work: &mut usize,
    cancelled: &impl Fn() -> bool,
    mut visit: impl FnMut(usize, f32, f32, f32, f32) -> bool,
) -> Result<bool, String> {
    let (n, centers, nearest) = raster;
    if !nearest {
        return corridor(layers, points, (n, centers), s, work, cancelled, visit);
    }
    let mut samples = std::collections::HashMap::<usize, [f32; 4]>::new();
    let mut budget = false;
    let complete = corridor(
        layers,
        points,
        (n, centers),
        s,
        work,
        cancelled,
        |i, d, y, x, z| {
            if let Some(old) = samples.get_mut(&i) {
                if d < old[0] {
                    *old = [d, y, x, z];
                }
            } else {
                if samples.len() >= MAX_CORRIDOR_CELLS {
                    budget = true;
                    return false;
                }
                samples.insert(i, [d, y, x, z]);
            }
            true
        },
    )?;
    if budget {
        return Err("Road corridor cell budget reached".into());
    }
    if !complete {
        return Ok(false);
    }
    let mut ordered: Vec<_> = samples.into_iter().collect();
    ordered.sort_unstable_by_key(|v| v.0);
    for (i, [d, y, x, z]) in ordered {
        if cancelled() {
            return Ok(false);
        }
        if *work >= MAX_WORK {
            return Err("Road corridor work budget reached".into());
        }
        *work += 1;
        if !visit(i, d, y, x, z) {
            return Ok(false);
        }
    }
    Ok(true)
}
/// Mutates only the caller's private composed revision. A cancelled/failed build
/// must be discarded by the caller; immutable base terrain is never passed here.
pub fn build(
    layers: &mut Layers,
    alpha: &[u8],
    sockets: &[Socket],
    obstacles: &[Obstacle],
    s: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    build_inner(layers, alpha, sockets, obstacles, s, None, None, &cancelled)
}
/// Connect authored sockets to each other and to sampled vertices of an existing
/// network. Existing road components are terminal destinations; this does not
/// certify or repair their connectivity to each other. Unreachable sockets remain
/// explicit even when they can connect to each other. Road indices in destinations
/// refer to `network`.
///
/// This independent candidate strategy is not the server's multi-source search.
/// New roads preserve existing Road-topology surface paint at junctions. The full
/// extension is transactional: cancellation/errors never change caller layers.
/// Source profiles/stage scheduling must opt in before using this in generation.
pub fn build_with_network(
    layers: &mut Layers,
    alpha: &[u8],
    sockets: &[Socket],
    obstacles: &[Obstacle],
    s: Settings,
    network: (&[Road], usize),
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    let targets = network_targets(network.0, network.1, layers.size, &cancelled)?;
    if cancelled() {
        return Ok(None);
    }
    let mut private = layers.clone();
    let result = build_inner(
        &mut private,
        alpha,
        sockets,
        obstacles,
        s,
        Some(&targets),
        None,
        &cancelled,
    )?;
    if result.is_some() && !cancelled() {
        *layers = private;
        Ok(result)
    } else {
        Ok(None)
    }
}
/// Source entrance heights and bounded owner-footprint exits; publishes atomically.
pub fn build_access(
    layers: &mut Layers,
    alpha: &[u8],
    sockets: &[Socket],
    obstacles: &[Obstacle],
    access: Access<'_>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    build_access_inner(
        layers, alpha, sockets, obstacles, access, None, None, false, None, None, cancelled,
    )
}
pub fn build_shaped_access(
    layers: &mut Layers,
    alpha: &[u8],
    sockets: &[Socket],
    obstacles: &[Obstacle],
    access: Access<'_>,
    shaping: ShapingPolicy,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    shaping.validate()?;
    build_access_inner(
        layers,
        alpha,
        sockets,
        obstacles,
        access,
        Some(shaping),
        None,
        false,
        None,
        None,
        cancelled,
    )
}
pub struct ProfiledAccess<'a> {
    pub access: Access<'a>,
    pub shaping: ShapingPolicy,
    pub traversal: TraversalPolicy,
}
pub struct LaunchedAccess<'a> {
    pub profile: ProfiledAccess<'a>,
    pub policy: LaunchPolicy,
    pub costs: Option<&'a [u32]>,
    pub constrained: bool,
    pub water: Option<crate::road_water::Guide<'a>>,
    /// Original terrain before any road family. Required only by revision 8.
    pub ground: Option<&'a [i16]>,
}
/// Independent checked launch revision. Existing entry points retain the exact
/// nearest-seed entrance chain used by saved legacy recipes.
pub fn build_launched_access(
    layers: &mut Layers,
    alpha: &[u8],
    sockets: &[Socket],
    obstacles: &[Obstacle],
    launch: LaunchedAccess<'_>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    launch.policy.validate()?;
    if (launch.policy.engine == LaunchEngine::GlobalEarthwork) != launch.ground.is_some()
        || launch
            .ground
            .is_some_and(|g| g.len() != layers.height.len())
    {
        return Err("Global road earthwork requires matching original terrain".into());
    }
    launch.profile.traversal.validate()?;
    launch.profile.shaping.validate()?;
    if launch.profile.traversal.traversable_topology & launch.profile.access.policy.owned_topology
        != 0
    {
        return Err("Authored monument ownership cannot become traversable terrain".into());
    }
    if launch
        .costs
        .is_some_and(|c| c.len() != GRID * GRID || c.iter().any(|v| *v > 1_000_000))
        || (launch.constrained && launch.costs.is_none())
        || (launch.water.is_some() && launch.costs.is_none())
    {
        return Err("Checked road launch requires valid matching routing inputs".into());
    }
    if let Some(water) = launch.water {
        if !water.validate(layers, &cancelled)? {
            return Ok(None);
        }
        if water.policy.topology & launch.profile.access.policy.owned_topology != 0 {
            return Err("Road water cannot override monument ownership".into());
        }
    }
    let settings = Settings {
        protected_topology: launch.profile.access.settings.protected_topology
            & !launch.profile.traversal.traversable_topology
            & !launch.water.map_or(0, |w| w.policy.topology),
        ..launch.profile.access.settings
    };
    let access = Access {
        settings,
        ..launch.profile.access
    };
    build_access_inner(
        layers,
        alpha,
        sockets,
        obstacles,
        access,
        Some(launch.profile.shaping),
        Some(launch.profile.traversal),
        true,
        Some(SourceCosts {
            costs: launch.costs,
            constrained: launch.constrained,
            water: launch.water,
            ground: launch.ground,
        }),
        Some(launch.policy),
        cancelled,
    )
}
#[derive(Clone, Copy)]
struct SourceCosts<'a> {
    costs: Option<&'a [u32]>,
    constrained: bool,
    water: Option<crate::road_water::Guide<'a>>,
    ground: Option<&'a [i16]>,
}
pub fn build_profiled_access(
    layers: &mut Layers,
    alpha: &[u8],
    sockets: &[Socket],
    obstacles: &[Obstacle],
    profile: ProfiledAccess<'_>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    build_profiled_access_inner(layers, alpha, sockets, obstacles, profile, false, cancelled)
}
pub fn build_component_access(
    layers: &mut Layers,
    alpha: &[u8],
    sockets: &[Socket],
    obstacles: &[Obstacle],
    profile: ProfiledAccess<'_>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    build_profiled_access_inner(layers, alpha, sockets, obstacles, profile, true, cancelled)
}
/// Source costs price only traversable coarse cells. Zero also participates in
/// connected-component and entrance-exit selection, never just the A* prices.
#[allow(clippy::too_many_arguments)]
pub fn build_source_cost_access(
    layers: &mut Layers,
    alpha: &[u8],
    sockets: &[Socket],
    obstacles: &[Obstacle],
    profile: ProfiledAccess<'_>,
    costs: &[u32],
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    build_source_cost_access_inner(
        layers, alpha, sockets, obstacles, profile, costs, false, None, cancelled,
    )
}
#[allow(clippy::too_many_arguments)]
pub fn build_constrained_source_access(
    layers: &mut Layers,
    alpha: &[u8],
    sockets: &[Socket],
    obstacles: &[Obstacle],
    profile: ProfiledAccess<'_>,
    costs: &[u32],
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    build_source_cost_access_inner(
        layers, alpha, sockets, obstacles, profile, costs, true, None, cancelled,
    )
}
/// Explicit collider-backed paved access revision. Source cost exclusions remain
/// authoritative, including the separate dirt-trail river exclusion.
#[allow(clippy::too_many_arguments)]
pub fn build_river_source_access(
    layers: &mut Layers,
    alpha: &[u8],
    sockets: &[Socket],
    obstacles: &[Obstacle],
    profile: ProfiledAccess<'_>,
    costs: &[u32],
    constrained: bool,
    water: crate::road_water::Guide<'_>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    if !water.validate(layers, &cancelled)? {
        return Ok(None);
    }
    if water.policy.topology & profile.access.policy.owned_topology != 0 {
        return Err("Road river clearance cannot override monument ownership".into());
    }
    let profile = ProfiledAccess {
        access: Access {
            settings: Settings {
                protected_topology: profile.access.settings.protected_topology
                    & !water.policy.topology,
                ..profile.access.settings
            },
            ..profile.access
        },
        ..profile
    };
    build_source_cost_access_inner(
        layers,
        alpha,
        sockets,
        obstacles,
        profile,
        costs,
        constrained,
        Some(water),
        cancelled,
    )
}
#[allow(clippy::too_many_arguments)]
fn build_source_cost_access_inner(
    layers: &mut Layers,
    alpha: &[u8],
    sockets: &[Socket],
    obstacles: &[Obstacle],
    profile: ProfiledAccess<'_>,
    costs: &[u32],
    constrained: bool,
    water: Option<crate::road_water::Guide<'_>>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    if costs.len() != GRID * GRID || costs.iter().any(|c| *c > 1_000_000) {
        return Err("Invalid source access cost grid".into());
    }
    profile.traversal.validate()?;
    profile.shaping.validate()?;
    if profile.traversal.traversable_topology & profile.access.policy.owned_topology != 0 {
        return Err("Authored monument ownership cannot become traversable terrain".into());
    }
    let access = Access {
        settings: Settings {
            protected_topology: profile.access.settings.protected_topology
                & !profile.traversal.traversable_topology,
            ..profile.access.settings
        },
        ..profile.access
    };
    build_access_inner(
        layers,
        alpha,
        sockets,
        obstacles,
        access,
        Some(profile.shaping),
        Some(profile.traversal),
        true,
        Some(SourceCosts {
            costs: Some(costs),
            constrained,
            water,
            ground: None,
        }),
        None,
        cancelled,
    )
}
#[allow(clippy::too_many_arguments)]
fn build_profiled_access_inner(
    layers: &mut Layers,
    alpha: &[u8],
    sockets: &[Socket],
    obstacles: &[Obstacle],
    profile: ProfiledAccess<'_>,
    component_search: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    profile.traversal.validate()?;
    profile.shaping.validate()?;
    if profile.traversal.traversable_topology & profile.access.policy.owned_topology != 0 {
        return Err("Authored monument ownership cannot become traversable terrain".into());
    }
    let access = Access {
        settings: Settings {
            protected_topology: profile.access.settings.protected_topology
                & !profile.traversal.traversable_topology,
            ..profile.access.settings
        },
        ..profile.access
    };
    build_access_inner(
        layers,
        alpha,
        sockets,
        obstacles,
        access,
        Some(profile.shaping),
        Some(profile.traversal),
        component_search,
        None,
        None,
        cancelled,
    )
}
#[allow(clippy::too_many_arguments)] // Keeps legacy entry points separate from profiled traversal.
fn build_access_inner(
    layers: &mut Layers,
    alpha: &[u8],
    sockets: &[Socket],
    obstacles: &[Obstacle],
    access: Access<'_>,
    shaping: Option<ShapingPolicy>,
    traversal: Option<TraversalPolicy>,
    component_search: bool,
    source_costs: Option<SourceCosts<'_>>,
    launch: Option<LaunchPolicy>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    access.policy.validate()?;
    if access.settings.protected_topology & access.policy.owned_topology
        != access.policy.owned_topology
    {
        return Err("Road ownership mask is not protected".into());
    }
    if access.owner_footprints.len() > 128
        || access.owner_footprints.iter().any(|o| {
            !o.yaw.is_finite()
                || o.center
                    .iter()
                    .chain(&o.extent)
                    .any(|v| !v.is_finite() || v.abs() > 10000.)
                || o.extent.iter().any(|v| *v < 0.)
        })
    {
        return Err("Invalid road owner footprints".into());
    }
    let mut ids = BTreeSet::new();
    if access
        .owner_footprints
        .iter()
        .any(|o| !ids.insert(o.monument))
    {
        return Err("Duplicate road owner footprint".into());
    }
    let targets = if let Some((roads, stride)) = access.network {
        Some(network_targets(roads, stride, layers.size, &cancelled)?)
    } else {
        None
    };
    if cancelled() {
        return Ok(None);
    }
    let mut private = layers.clone();
    let report = build_inner_profiled(
        &mut private,
        alpha,
        sockets,
        obstacles,
        access.settings,
        targets.as_deref(),
        Some((access.policy, access.owner_footprints, shaping)),
        traversal,
        component_search,
        source_costs,
        launch,
        &cancelled,
    )?;
    if report.is_some() && !cancelled() {
        *layers = private;
        Ok(report)
    } else {
        Ok(None)
    }
}
#[derive(Clone, Debug)]
struct Exit {
    cell: [usize; 2],
    points: Vec<[f32; 3]>,
}
#[allow(clippy::too_many_arguments)]
fn entrance_exit(
    layers: &Layers,
    alpha: &[u8],
    node: Node,
    obstacles: &[Obstacle],
    owners: &[Obstacle],
    settings: Settings,
    policy: EntrancePolicy,
    heights: &[f32],
    blocked: &[bool],
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Exit>, String> {
    entrance_exit_goal(
        layers, alpha, node, obstacles, owners, settings, policy, heights, blocked, None, None,
        cancelled,
    )
}
#[allow(clippy::too_many_arguments)]
fn entrance_exit_goal(
    layers: &Layers,
    alpha: &[u8],
    node: Node,
    obstacles: &[Obstacle],
    owners: &[Obstacle],
    settings: Settings,
    policy: EntrancePolicy,
    heights: &[f32],
    blocked: &[bool],
    goal: Option<(&[u32], u32)>,
    water: Option<crate::road_water::Guide<'_>>,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Exit>, String> {
    entrance_exit_goal_with_launch(
        layers, alpha, node, obstacles, owners, settings, policy, heights, blocked, goal, water,
        None, cancelled,
    )
}
#[allow(clippy::too_many_arguments)]
fn entrance_exit_goal_with_launch(
    layers: &Layers,
    alpha: &[u8],
    node: Node,
    obstacles: &[Obstacle],
    owners: &[Obstacle],
    settings: Settings,
    policy: EntrancePolicy,
    heights: &[f32],
    blocked: &[bool],
    goal: Option<(&[u32], u32)>,
    water: Option<crate::road_water::Guide<'_>>,
    launch: Option<LaunchPolicy>,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Exit>, String> {
    if let Some(launch) = launch {
        launch.validate()?;
    }
    let owner = owners.iter().find(|o| Some(o.monument) == node.monument());
    let spacing = layers.size as f32 / (GRID - 1) as f32;
    let point = |i: usize| {
        [
            (i % GRID) as f32 * spacing - layers.size as f32 * 0.5,
            heights[i],
            (i / GRID) as f32 * spacing - layers.size as f32 * 0.5,
        ]
    };
    let allowed_raw = |i: usize| {
        if heights[i] <= policy.minimum_height {
            return false;
        }
        if !blocked[i] {
            return true;
        }
        let p = point(i);
        let owned = !protected(layers, alpha, settings, p[0], p[2])
            || owner.is_some_and(|o| inside(o, p[0], p[2], 0.))
            || obstacles.iter().any(|o| {
                Some(o.monument) == node.monument() && inside(o, p[0], p[2], settings.width * 0.5)
            });
        if !owned
            || owners
                .iter()
                .any(|o| Some(o.monument) != node.monument() && inside(o, p[0], p[2], 0.))
            || obstacles.iter().any(|o| {
                Some(o.monument) != node.monument() && inside(o, p[0], p[2], settings.width * 0.5)
            })
        {
            return false;
        }
        let n = layers.surface_resolution;
        let cell = |v: f32| {
            ((v / layers.size as f32 + 0.5) * n as f32)
                .floor()
                .clamp(0., (n - 1) as f32) as usize
        };
        let index = cell(p[2]) * n + cell(p[0]);
        alpha[index] >= 128
            && layers.topology[index] & (settings.protected_topology & !policy.owned_topology) == 0
            && ground_clear_with_water(
                layers,
                alpha,
                settings,
                [p[0], p[2]],
                settings.width * 0.5 + settings.shoulder,
                Some((policy, owners, node.monument())),
                water,
            )
    };
    let mut checked = vec![None; GRID * GRID];
    let mut allowed = |i: usize| {
        if let Some(result) = checked[i] {
            return result;
        }
        let result = allowed_raw(i);
        checked[i] = Some(result);
        result
    };
    let gx = (node.position[0] / spacing + (GRID - 1) as f32 * 0.5).round() as isize;
    let gz = (node.position[2] / spacing + (GRID - 1) as f32 * 0.5).round() as isize;
    let mut seeds = Vec::new();
    let launch_range = launch.map_or(1, |p| {
        ((settings.width * p.minimum_run_widths / spacing).ceil() as isize + 1).min(64)
    });
    for dz in -launch_range..=launch_range {
        for dx in -launch_range..=launch_range {
            let (x, z) = (gx + dx, gz + dz);
            if x >= 0 && z >= 0 && x < GRID as isize && z < GRID as isize {
                let i = z as usize * GRID + x as usize;
                let p = point(i);
                let d = distance(p, node.position);
                if (launch.is_none() || d <= policy.maximum_exit_distance)
                    && (launch.is_some() || allowed(i))
                    && ((p[1] - node.position[1]).abs() <= settings.maximum_grade * d + 1e-5)
                {
                    seeds.push((d, i));
                }
            }
        }
    }
    seeds.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    if seeds.is_empty() {
        return Ok(None);
    }
    if let Some(launch) = launch {
        let mut checked_seeds = Vec::new();
        let mut work = 0_usize;
        let radius = settings.width * 0.5 + settings.shoulder;
        let cell = layers.size as f32 / (layers.height_resolution - 1) as f32;
        let visit_cost = ((radius * 2. / cell).ceil() as usize + 4).pow(2);
        let mut examined = 0;
        for (distance, i) in seeds {
            if cancelled() {
                return Ok(None);
            }
            if distance > 0.001 && distance < settings.width * launch.minimum_run_widths {
                continue;
            }
            if examined >= 64 {
                break;
            }
            examined += 1;
            work = work
                .checked_add(visit_cost)
                .filter(|w| *w <= launch.maximum_work)
                .ok_or("Road launch clearance work budget exceeded")?;
            if !allowed(i) {
                continue;
            }
            let target = point(i);
            let steps = (distance / launch.sample_spacing).ceil() as usize;
            let mut clear = true;
            for step in 0..=steps {
                if cancelled() {
                    return Ok(None);
                }
                work = work
                    .checked_add(visit_cost)
                    .filter(|w| *w <= launch.maximum_work)
                    .ok_or("Road launch clearance work budget exceeded")?;
                let t = if steps == 0 {
                    0.
                } else {
                    step as f32 / steps as f32
                };
                let p: [f32; 3] =
                    std::array::from_fn(|k| node.position[k] + (target[k] - node.position[k]) * t);
                if obstacles.iter().any(|o| {
                    Some(o.monument) != node.monument()
                        && inside(o, p[0], p[2], settings.width * 0.5)
                }) || !ground_clear_with_water(
                    layers,
                    alpha,
                    settings,
                    [p[0], p[2]],
                    radius,
                    Some((policy, owners, node.monument())),
                    water,
                ) {
                    clear = false;
                    break;
                }
            }
            if clear {
                checked_seeds.push((distance, i));
            }
        }
        seeds = checked_seeds;
        seeds.truncate(policy.maximum_exit_nodes);
    } else {
        seeds.truncate(1);
    }
    if seeds.is_empty() {
        return Ok(None);
    }
    let mut parent = vec![usize::MAX; GRID * GRID];
    let mut queue = std::collections::VecDeque::new();
    for &(_, start) in &seeds {
        parent[start] = start;
        queue.push_back(start);
    }
    let mut discovered = seeds.len();
    while let Some(i) = queue.pop_front() {
        if cancelled() {
            return Ok(None);
        }
        if !blocked[i] && goal.is_none_or(|(labels, wanted)| labels[i] == wanted) {
            let mut indices = vec![i];
            let mut current = i;
            while parent[current] != current {
                current = parent[current];
                indices.push(current);
            }
            indices.reverse();
            let mut points = vec![node.position];
            for j in indices {
                let p = point(j);
                if distance(*points.last().unwrap(), p) > 0.001 {
                    points.push(p);
                }
            }
            return Ok(Some(Exit {
                cell: [i % GRID, i / GRID],
                points,
            }));
        }
        let (x, z) = (i % GRID, i / GRID);
        for (dx, dz) in [
            (0, -1),
            (-1, 0),
            (1, 0),
            (0, 1),
            (-1, -1),
            (1, -1),
            (-1, 1),
            (1, 1),
        ] {
            let (nx, nz) = (x as isize + dx, z as isize + dz);
            if nx < 0 || nz < 0 || nx >= GRID as isize || nz >= GRID as isize {
                continue;
            }
            let j = nz as usize * GRID + nx as usize;
            if parent[j] != usize::MAX
                || !allowed(j)
                || distance(point(j), node.position) > policy.maximum_exit_distance
                || (point(j)[1] - point(i)[1]).abs()
                    > settings.maximum_grade * distance(point(j), point(i))
                || (dx != 0
                    && dz != 0
                    && (!allowed(z * GRID + nx as usize) || !allowed(nz as usize * GRID + x)))
            {
                continue;
            }
            if launch.is_some()
                && parent[i] == i
                && !launch_turn_clear(node.position, point(i), point(j), settings.width * 0.5)
            {
                continue;
            }
            if discovered >= policy.maximum_exit_nodes {
                return Ok(None);
            }
            parent[j] = i;
            discovered += 1;
            queue.push_back(j);
        }
    }
    Ok(None)
}
fn launch_turn_clear(a: [f32; 3], b: [f32; 3], c: [f32; 3], radius: f32) -> bool {
    let u = [
        f64::from(b[0]) - f64::from(a[0]),
        f64::from(b[2]) - f64::from(a[2]),
    ];
    let v = [
        f64::from(c[0]) - f64::from(b[0]),
        f64::from(c[2]) - f64::from(b[2]),
    ];
    let la = u[0].hypot(u[1]);
    let lb = v[0].hypot(v[1]);
    if la < 0.001 {
        return true;
    }
    if lb < 0.001 {
        return false;
    }
    let cosine = ((u[0] * v[0] + u[1] * v[1]) / (la * lb)).clamp(-1., 1.);
    cosine > -0.99999
        && f64::from(radius) * ((1. - cosine) / (1. + cosine)).sqrt() <= la.min(lb) + 1e-5
}
#[derive(Clone, Copy)]
struct Node {
    destination: Destination,
    group: usize,
    position: [f32; 3],
}
impl Node {
    fn monument(self) -> Option<u64> {
        match self.destination {
            Destination::Monument { monument, .. } => Some(monument),
            Destination::Network { .. } | Destination::RawNetwork(_) | Destination::Loop => None,
        }
    }
}
fn network_targets(
    roads: &[Road],
    stride: usize,
    size: u32,
    cancelled: &impl Fn() -> bool,
) -> Result<Vec<(Destination, [f32; 3])>, String> {
    if roads.is_empty()
        || roads.len() > 128
        || !(1..=1024).contains(&stride)
        || roads.iter().map(|r| r.points.len()).sum::<usize>() > MAX_NETWORK_POINTS
    {
        return Err("Invalid existing road network or sampling stride".into());
    }
    let mut targets = Vec::new();
    for (road, path) in roads.iter().enumerate() {
        if cancelled() {
            return Ok(Vec::new());
        }
        if path.points.len() < 2
            || path.points.iter().any(|p| {
                p.iter().any(|v| !v.is_finite() || v.abs() > 10000.)
                    || p[0].abs() > size as f32 * 0.5
                    || p[2].abs() > size as f32 * 0.5
            })
        {
            return Err("Invalid existing road geometry".into());
        }
        for point in (0..path.points.len()).step_by(stride).chain(
            (!(path.points.len() - 1).is_multiple_of(stride)).then_some(path.points.len() - 1),
        ) {
            if targets.len() == MAX_NETWORK_TARGETS {
                return Err("Existing road network target budget exceeded".into());
            }
            targets.push((Destination::Network { road, point }, path.points[point]));
        }
    }
    Ok(targets)
}
/// Derive conservative node-height intervals from every native corridor cell.
/// Interpolating two node deviations inside a cell's allowed deviation retains
/// that cell's cut/fill bound. The shared graph solver also bounds closing grade.
fn fit_ring_heights(
    layers: &Layers,
    points: &mut [[f32; 3]],
    settings: Settings,
    maximum_work: usize,
    water: Option<crate::road_water::Guide<'_>>,
    cancelled: &impl Fn() -> bool,
) -> Result<(usize, usize), crate::rail_grade_fit::Failure> {
    fit_road_heights(
        layers,
        points,
        settings,
        maximum_work,
        water,
        true,
        false,
        cancelled,
    )
}
#[allow(clippy::too_many_arguments)]
fn fit_road_heights(
    layers: &Layers,
    points: &mut [[f32; 3]],
    settings: Settings,
    maximum_work: usize,
    water: Option<crate::road_water::Guide<'_>>,
    closed: bool,
    preserve_protected: bool,
    cancelled: &impl Fn() -> bool,
) -> Result<(usize, usize), crate::rail_grade_fit::Failure> {
    fit_road_heights_on_ground(
        layers,
        points,
        settings,
        maximum_work,
        water,
        closed,
        preserve_protected,
        None,
        cancelled,
    )
}
#[allow(clippy::too_many_arguments)]
fn fit_road_heights_on_ground(
    layers: &Layers,
    points: &mut [[f32; 3]],
    settings: Settings,
    maximum_work: usize,
    water: Option<crate::road_water::Guide<'_>>,
    closed: bool,
    preserve_protected: bool,
    ground: Option<&[i16]>,
    cancelled: &impl Fn() -> bool,
) -> Result<(usize, usize), crate::rail_grade_fit::Failure> {
    use crate::rail_grade_fit::{self, Edge, Failure, Node};
    if points.len() < if closed { 5 } else { 2 }
        || points.len() > 65536
        || (closed && points.first() != points.last())
        || !(1..=MAX_WORK).contains(&maximum_work)
        || points.iter().flatten().any(|v| !v.is_finite())
        || ground.is_some_and(|g| g.len() != layers.height.len())
    {
        return Err(Failure::Invalid);
    }
    let n = points.len() - usize::from(closed);
    let start = MAX_WORK - maximum_work;
    let mut work = start;
    // The original per-family model bounds deviations about a desired profile.
    // For shared budgets, first put the soft inland profile inside the remaining
    // global interval. Keep open endpoint poses exact; all native footprint
    // constraints below still decide acceptance, including off-centre cells.
    let target_points = if let Some(ground) = ground {
        let mut targets = points.to_vec();
        let reserve = (settings.maximum_cut_fill * 0.5).min(0.001 + 1000. / 32766.);
        let allowance = settings.maximum_cut_fill - reserve;
        for (i, p) in targets[..n].iter_mut().enumerate() {
            if cancelled() {
                return Err(Failure::Cancelled);
            }
            work = work
                .checked_add(8)
                .filter(|&w| w <= MAX_WORK)
                .ok_or(Failure::Budget)?;
            if !closed && (i == 0 || i + 1 == n) {
                continue;
            }
            let base = crate::path_terrain::sample_height(
                crate::path_terrain::Grid {
                    size: layers.size as f32,
                    resolution: layers.height_resolution,
                    origin_y: -500.,
                    vertical_span: 1000.,
                    height: ground,
                },
                *p,
            )
            .map_err(|_| Failure::Invalid)?
                - settings.terrain_offset;
            p[1] = p[1].clamp(base - allowance, base + allowance);
        }
        if closed {
            targets[n] = targets[0];
        }
        std::borrow::Cow::Owned(targets)
    } else {
        std::borrow::Cow::Borrowed(&*points)
    };
    let mut nearest = std::collections::HashMap::<usize, (f32, usize, f32)>::new();
    for (segment, pair) in target_points.windows(2).enumerate() {
        let result = corridor_rows(
            layers,
            pair,
            (layers.height_resolution, false),
            settings,
            &mut work,
            cancelled,
            ground.is_some(),
            |i, d, y, _, _| {
                if nearest.len() >= MAX_CORRIDOR_CELLS && !nearest.contains_key(&i) {
                    return false;
                }
                let item = nearest.entry(i).or_insert((d, segment, y));
                if d < item.0 {
                    *item = (d, segment, y);
                }
                true
            },
        );
        match result {
            Err(_) => return Err(Failure::Budget),
            Ok(false) => {
                return Err(if cancelled() {
                    Failure::Cancelled
                } else {
                    Failure::Budget
                });
            }
            Ok(true) => {}
        }
    }
    let mut nodes: Vec<_> = target_points[..n]
        .iter()
        .map(|p| Node {
            desired: f64::from(p[1]),
            minimum: f64::from(1. - settings.terrain_offset + 1000. / 32766.),
            maximum: 500.,
        })
        .collect();
    let coupled = closed && ground.is_some() && !preserve_protected;
    let coupled_nodes = coupled.then(|| nodes.clone());
    let mut coupled_samples = Vec::new();
    let mut boxes_infeasible = false;
    // A small arithmetic margin protects the final f32 interpolation; the final
    // native audit, not the interval construction, remains authoritative.
    let allowance = f64::from(settings.maximum_cut_fill)
        - (f64::from(settings.maximum_cut_fill) * 0.5).min(0.001);
    for (i, (_, segment, target)) in nearest {
        if cancelled() {
            return Err(Failure::Cancelled);
        }
        if work >= MAX_WORK {
            return Err(Failure::Budget);
        }
        work += 1;
        let old = f64::from(decode(layers.height[i]));
        let hr = layers.height_resolution;
        let x = ((i % hr) as f32 / (hr - 1) as f32 - 0.5) * layers.size as f32;
        let z = ((i / hr) as f32 / (hr - 1) as f32 - 0.5) * layers.size as f32;
        let minimum = water.and_then(|w| w.minimum(layers, x, z));
        if old <= 1. && minimum.is_none() {
            #[cfg(feature = "road-diagnostics")]
            if closed {
                capture_ring_fit(&target_points, &nodes, &[], "water_cell", Some(i))?;
            }
            return Err(Failure::Infeasible);
        }
        let cut_fill_ground = ground.map_or(old, |g| f64::from(decode(g[i])));
        let delta = cut_fill_ground - f64::from(target);
        if coupled {
            work = work
                .checked_add(16)
                .filter(|&w| w <= MAX_WORK)
                .ok_or(Failure::Budget)?;
            let a = target_points[segment];
            let b = target_points[segment + 1];
            let dx = b[0] - a[0];
            let dz = b[2] - a[2];
            let along = (((x - a[0]) * dx + (z - a[2]) * dz) / (dx * dx + dz * dz)).clamp(0., 1.);
            coupled_samples.push(crate::road_corridor_fit::Sample {
                a: segment,
                b: (segment + 1) % n,
                along: f64::from(along),
                minimum: (cut_fill_ground - allowance - f64::from(settings.terrain_offset))
                    .max(minimum.map_or(f64::NEG_INFINITY, f64::from)),
                maximum: cut_fill_ground + allowance - f64::from(settings.terrain_offset),
            });
        }
        for index in [segment, (segment + 1) % n] {
            let node = &mut nodes[index];
            node.minimum = node.minimum.max(node.desired + delta - allowance);
            if let Some(minimum) = minimum {
                node.minimum = node.minimum.max(
                    node.desired + f64::from(minimum + settings.terrain_offset) - f64::from(target),
                );
            }
            node.maximum = node.maximum.min(node.desired + delta + allowance);
            if node.minimum > node.maximum {
                if coupled {
                    boxes_infeasible = true;
                    continue;
                }
                #[cfg(feature = "road-diagnostics")]
                if closed {
                    capture_ring_fit(&target_points, &nodes, &[], "cell_interval", Some(i))?;
                }
                return Err(Failure::Infeasible);
            }
        }
    }
    if !closed {
        // Existing monument and network endpoint heights are immutable. Fitting
        // a crossing must never move a previously accepted road attachment.
        for i in [0, n - 1] {
            let node = &mut nodes[i];
            if node.desired < node.minimum || node.desired > node.maximum {
                return Err(Failure::Infeasible);
            }
            node.minimum = node.desired;
            node.maximum = node.desired;
        }
    }
    if preserve_protected {
        // The later terrain pass restores authored protected ground. A smooth
        // proposal cannot borrow cut/fill there to satisfy its grade limit.
        let sr = layers.surface_resolution;
        let cell = |w: f32| {
            ((w / layers.size as f32 + 0.5) * sr as f32)
                .floor()
                .clamp(0., (sr - 1) as f32) as usize
        };
        for (i, &point) in points[..n].iter().enumerate() {
            if cancelled() {
                return Err(Failure::Cancelled);
            }
            if work >= MAX_WORK {
                return Err(Failure::Budget);
            }
            work += 1;
            if (!closed && (i == 0 || i + 1 == n))
                || layers.topology[cell(point[2]) * sr + cell(point[0])]
                    & settings.protected_topology
                    == 0
            {
                continue;
            }
            // Layers uses this normalized-short world-height representation.
            let ground = f64::from(
                crate::path_terrain::sample_height(
                    crate::path_terrain::Grid {
                        size: layers.size as f32,
                        resolution: layers.height_resolution,
                        origin_y: -500.,
                        vertical_span: 1000.,
                        height: &layers.height,
                    },
                    point,
                )
                .map_err(|_| Failure::Invalid)?,
            );
            let node = &mut nodes[i];
            if ground < node.minimum || ground > node.maximum {
                return Err(Failure::Infeasible);
            }
            node.minimum = ground;
            node.maximum = ground;
        }
    }
    let mut edges = Vec::with_capacity(n);
    for i in 0..if closed { n } else { n - 1 } {
        let run = distance(points[i], points[(i + 1) % n]);
        if run < 1e-4 {
            return Err(Failure::Invalid);
        }
        edges.push(Edge {
            a: i,
            b: (i + 1) % n,
            maximum_difference: f64::from(run) * f64::from(settings.maximum_grade) * 0.999,
        });
    }
    let remaining = MAX_WORK - work;
    if remaining == 0 {
        return Err(Failure::Budget);
    }
    // Keep the established fast result whenever its sufficient boxes fit. A
    // failed sufficient model must not rule out the coupled corridor itself.
    // Reserve a disjoint fallback grant so failed search never resets work.
    let graph_grant = if coupled { remaining / 4 } else { remaining };
    let mut solved = if boxes_infeasible {
        Err(Failure::Infeasible)
    } else {
        rail_grade_fit::solve(&nodes, &edges, graph_grant, cancelled)
    };
    if coupled && matches!(solved, Err(Failure::Infeasible | Failure::Budget)) {
        solved = crate::road_corridor_fit::solve(
            coupled_nodes.as_deref().ok_or(Failure::Invalid)?,
            &edges,
            &coupled_samples,
            remaining - graph_grant,
            cancelled,
        )
        .map(|solution| {
            #[cfg(feature = "road-diagnostics")]
            if std::env::var_os("MAPPER_ROAD_RING_COST_DEBUG").is_some() {
                eprintln!(
                    "Coupled ring fit accepted: rows={}, work={}",
                    solution.rows, solution.work
                );
            }
            (solution.heights, graph_grant + solution.work)
        });
    }
    #[cfg(feature = "road-diagnostics")]
    if closed && solved.is_err() {
        capture_ring_fit_ground(&target_points, layers, settings, ground)?;
        capture_ring_fit(&target_points, &nodes, &edges, "graph", None)?;
    }
    let (heights, used) = solved?;
    if cancelled() {
        return Err(Failure::Cancelled);
    }
    drop(target_points);
    let mut changed = 0;
    for (point, height) in points[..n].iter_mut().zip(heights) {
        let height = height as f32;
        changed += usize::from(point[1] != height);
        point[1] = height;
    }
    if closed {
        points[n] = points[0];
    }
    Ok((work - start + used, changed))
}
/// Preserve the original raster for an independent check of the conservative
/// endpoint boxes against the actual interpolated corridor inequalities.
#[cfg(feature = "road-diagnostics")]
fn capture_ring_fit_ground(
    points: &[[f32; 3]],
    layers: &Layers,
    settings: Settings,
    ground: Option<&[i16]>,
) -> Result<(), crate::rail_grade_fit::Failure> {
    use crate::rail_grade_fit::Failure;
    use std::io::{BufWriter, Write};
    let Some(directory) = std::env::var_os("MAPPER_ROAD_RING_FIT_CAPTURE") else {
        return Ok(());
    };
    let directory = std::path::Path::new(&directory);
    let filename = format!("ring-ground-{}.i16le", points.len());
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join(&filename))
        .map_err(|_| Failure::Invalid)?;
    let mut writer = BufWriter::new(file);
    for height in ground.unwrap_or(&layers.height) {
        writer
            .write_all(&height.to_le_bytes())
            .map_err(|_| Failure::Invalid)?;
    }
    writer.flush().map_err(|_| Failure::Invalid)?;
    let document = serde_json::json!({
        "format": "mapper-private-ring-ground-witness-1",
        "ground_file": filename,
        "height_resolution": layers.height_resolution,
        "size": layers.size,
        "settings": {
            "width": settings.width,
            "shoulder": settings.shoulder,
            "terrain_offset": settings.terrain_offset,
            "maximum_cut_fill": settings.maximum_cut_fill,
            "maximum_grade": settings.maximum_grade,
        },
        "scope": "Original cut/fill raster; water and protected-ground constraints are not included",
    });
    let mut metadata = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join(format!("ring-ground-{}.json", points.len())))
        .map_err(|_| Failure::Invalid)?;
    metadata
        .write_all(&serde_json::to_vec(&document).map_err(|_| Failure::Invalid)?)
        .map_err(|_| Failure::Invalid)
}
/// Opt-in private numeric witness, excluded from ordinary application builds.
#[cfg(feature = "road-diagnostics")]
fn capture_ring_fit(
    points: &[[f32; 3]],
    nodes: &[crate::rail_grade_fit::Node],
    edges: &[crate::rail_grade_fit::Edge],
    reason: &str,
    cell: Option<usize>,
) -> Result<(), crate::rail_grade_fit::Failure> {
    use crate::rail_grade_fit::Failure;
    use std::io::Write;
    let Some(directory) = std::env::var_os("MAPPER_ROAD_RING_FIT_CAPTURE") else {
        return Ok(());
    };
    let directory = std::path::Path::new(&directory);
    // Caller creates a fresh capture directory; never replace a previous witness.
    let name = directory.join(format!("ring-fit-{}-{reason}.json", points.len()));
    let document = serde_json::json!({"format":"mapper-private-ring-fit-witness-1",
        "reason":reason,"cell":cell,"points":points,
        "nodes":nodes.iter().map(|n| [n.desired,n.minimum,n.maximum]).collect::<Vec<_>>(),
        "edges":edges.iter().map(|e| serde_json::json!([e.a,e.b,e.maximum_difference])).collect::<Vec<_>>()});
    let bytes = serde_json::to_vec(&document).map_err(|_| Failure::Invalid)?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(Failure::Budget);
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(name)
        .map_err(|_| Failure::Invalid)?;
    file.write_all(&bytes).map_err(|_| Failure::Invalid)
}
fn traversal_cost_grid(
    layers: &Layers,
    sr: usize,
    s: Settings,
    policy: TraversalPolicy,
    cancelled: &impl Fn() -> bool,
) -> Option<Vec<u32>> {
    let spacing = layers.size as f32 / (GRID - 1) as f32;
    let mut costs = vec![policy.normal_cost; GRID * GRID];
    let cell = |w: f32| {
        ((w / layers.size as f32 + 0.5) * sr as f32)
            .floor()
            .clamp(0., (sr - 1) as f32) as usize
    };
    let radius = s.width * 0.5;
    for z in 0..GRID {
        if cancelled() {
            return None;
        }
        for x in 0..GRID {
            let (wx, wz) = (
                x as f32 * spacing - layers.size as f32 * 0.5,
                z as f32 * spacing - layers.size as f32 * 0.5,
            );
            if (cell(wz - radius)..=cell(wz + radius)).any(|sz| {
                (cell(wx - radius)..=cell(wx + radius))
                    .any(|sx| layers.topology[sz * sr + sx] & policy.traversable_topology != 0)
            }) {
                costs[z * GRID + x] = policy.traversable_cost;
            }
        }
    }
    Some(costs)
}
/// Generate a main road on a private terrain revision. A rejected ring publishes
/// its diagnostic but no fragments or terrain writes. Legacy access builds do
/// not enter this stage.
pub fn build_ring(
    layers: &mut Layers,
    alpha: &[u8],
    obstacles: &[Obstacle],
    settings: Settings,
    policy: crate::road_ring::Policy,
    shaping: ShapingPolicy,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    build_ring_inner(
        layers, alpha, obstacles, settings, policy, shaping, None, None, None, None, None,
        cancelled,
    )
}
/// Explicit alternate search; saved legacy rings retain their proposal order.
#[allow(clippy::too_many_arguments)]
pub fn build_ring_with_recovery(
    layers: &mut Layers,
    alpha: &[u8],
    obstacles: &[Obstacle],
    settings: Settings,
    policy: crate::road_ring::Policy,
    shaping: ShapingPolicy,
    traversal: Option<(TraversalPolicy, i32)>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    build_ring_inner(
        layers, alpha, obstacles, settings, policy, shaping, traversal, None, None, None, None,
        cancelled,
    )
}
/// Explicit source-cost revision. Legacy ring recipes never enter this path.
#[allow(clippy::too_many_arguments)]
pub fn build_ring_with_source_costs(
    layers: &mut Layers,
    alpha: &[u8],
    obstacles: &[Obstacle],
    settings: Settings,
    policy: crate::road_ring::Policy,
    shaping: ShapingPolicy,
    traversal: (TraversalPolicy, i32),
    costs: &[u32],
    joins: Option<&crate::road_ring_join::Policy>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    build_ring_with_river_costs(
        layers, alpha, obstacles, settings, policy, shaping, traversal, costs, joins, None,
        cancelled,
    )
}
/// Explicit water-aware ring revision. Unverified/imported local water is not inferred.
#[allow(clippy::too_many_arguments)]
pub fn build_ring_with_river_costs(
    layers: &mut Layers,
    alpha: &[u8],
    obstacles: &[Obstacle],
    settings: Settings,
    policy: crate::road_ring::Policy,
    shaping: ShapingPolicy,
    traversal: (TraversalPolicy, i32),
    costs: &[u32],
    joins: Option<&crate::road_ring_join::Policy>,
    water: Option<crate::road_water::Guide<'_>>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    if let Some(water) = water {
        if joins.and_then(|j| j.river_water.as_ref()) != Some(water.policy) {
            return Err("Road water guide differs from the saved policy".into());
        }
        if !water.validate(layers, &cancelled)? {
            return Ok(None);
        }
    }
    if costs.len() != GRID * GRID || costs.iter().any(|v| *v > 1_000_000) {
        return Err("Invalid source ring costs".into());
    }
    build_ring_inner(
        layers,
        alpha,
        obstacles,
        settings,
        policy,
        shaping,
        Some(traversal),
        Some(costs),
        joins,
        water,
        None,
        cancelled,
    )
}
/// Shared-earthwork recipes bound the soft closed-ring target before fitting.
/// All native-cell, protected-ground and final grade checks still apply.
#[allow(clippy::too_many_arguments)]
pub fn build_ring_with_global_earthwork(
    layers: &mut Layers,
    alpha: &[u8],
    obstacles: &[Obstacle],
    settings: Settings,
    policy: crate::road_ring::Policy,
    shaping: ShapingPolicy,
    traversal: Option<(TraversalPolicy, i32)>,
    costs: Option<&[u32]>,
    joins: Option<&crate::road_ring_join::Policy>,
    water: Option<crate::road_water::Guide<'_>>,
    launch: LaunchPolicy,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    launch.validate()?;
    if launch.engine != LaunchEngine::GlobalEarthwork {
        return Err("Main ring requires global earthwork launch policy".into());
    }
    if costs.is_some_and(|c| c.len() != GRID * GRID || c.iter().any(|v| *v > 1_000_000)) {
        return Err("Invalid source ring costs".into());
    }
    if joins.is_some() && costs.is_none() {
        return Err("Joined ring requires source costs".into());
    }
    if let Some(water) = water {
        if joins.and_then(|j| j.river_water.as_ref()) != Some(water.policy) {
            return Err("Road water guide differs from the saved policy".into());
        }
        if !water.validate(layers, &cancelled)? {
            return Ok(None);
        }
    }
    build_ring_inner(
        layers,
        alpha,
        obstacles,
        settings,
        policy,
        shaping,
        traversal,
        costs,
        joins,
        water,
        Some(launch),
        cancelled,
    )
}
/// Add fitting freedom without cutting corners or changing the horizontal route.
fn subdivide_ring(
    points: &[[f32; 3]],
    spacing: f32,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Vec<[f32; 3]>>, String> {
    if points.len() < 5
        || points.len() > 65536
        || points.first() != points.last()
        || !spacing.is_finite()
        || spacing <= 0.
        || points.iter().flatten().any(|v| !v.is_finite())
    {
        return Err("Invalid ring subdivision input".into());
    }
    let mut out = Vec::with_capacity(points.len());
    out.push(points[0]);
    for pair in points.windows(2) {
        let pieces = (distance(pair[0], pair[1]) / spacing).ceil().max(1.) as usize;
        if pieces > 65536 || out.len().checked_add(pieces).is_none_or(|v| v > 65536) {
            return Err("Ring subdivision point allowance reached".into());
        }
        for i in 1..=pieces {
            if cancelled() {
                return Ok(None);
            }
            // Preserve source endpoints exactly, including the closing join.
            out.push(if i == pieces {
                pair[1]
            } else {
                let t = i as f32 / pieces as f32;
                std::array::from_fn(|k| pair[0][k] + (pair[1][k] - pair[0][k]) * t)
            });
        }
    }
    Ok(Some(out))
}
#[allow(clippy::too_many_arguments)]
fn build_ring_inner(
    layers: &mut Layers,
    alpha: &[u8],
    obstacles: &[Obstacle],
    mut settings: Settings,
    policy: crate::road_ring::Policy,
    shaping: ShapingPolicy,
    traversal: Option<(TraversalPolicy, i32)>,
    source_costs: Option<&[u32]>,
    joins: Option<&crate::road_ring_join::Policy>,
    water: Option<crate::road_water::Guide<'_>>,
    global_launch: Option<LaunchPolicy>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    let global_earthwork = global_launch.is_some();
    policy.validate()?;
    shaping.validate()?;
    let recovering = traversal.is_some();
    if let Some((p, owned)) = traversal {
        p.validate()?;
        if owned == 0 || p.traversable_topology & owned != 0 {
            return Err("Authored monument ownership cannot become traversable terrain".into());
        }
        settings.protected_topology &= !p.traversable_topology;
    }
    if let Some(water) = water {
        if traversal.is_some_and(|(_, owned)| water.policy.topology & owned != 0) {
            return Err("Road river policy cannot clear monument ownership protection".into());
        }
        // The old protection mask deliberately denied unimplemented rivers.
        // Explicit collider-backed clearance replaces only that water denial.
        settings.protected_topology &= !water.policy.topology;
    }
    settings.width = policy.width;
    let (hr, sr) = validate_inputs(layers, alpha, &[], obstacles, settings)?;
    if cancelled() {
        return Ok(None);
    }
    let mut report = Report {
        settings: Some(settings),
        ..Report::default()
    };
    if let Some(water) = water {
        report.river_water = Some(serde_json::json!({"policy":water.policy,
            "sampling":"nearest surface-grid cell from generated river collision triangles",
            "no_hit_height":0.,"terrain_offset":settings.terrain_offset,
            "scope":"main road ring only; other water body families incomplete",
            "physics_cooking_validated":false}));
    }
    if layers.size < policy.minimum_world_size {
        report.ring = Some(crate::road_ring::Audit {
            engine: if let Some(joins) = joins {
                joins.version()
            } else if recovering {
                crate::road_ring::RECOVERY_VERSION
            } else {
                crate::road_ring::VERSION
            },
            status: "skipped",
            reason: Some("World below source ring minimum"),
            policy,
            route_attempts: 0,
            waypoint_moves: 0,
            waypoints: vec![],
            smoothed: false,
            shaping_fallback: None,
            grade_fit_work: 0,
            fitted_height_samples: 0,
            joins: None,
        });
        return Ok(Some(report));
    }
    let mut heights: Vec<_> = layers
        .sample_heights(GRID)?
        .into_iter()
        .map(decode)
        .collect();
    if let Some(water) = water {
        for z in 0..GRID {
            if cancelled() {
                return Ok(None);
            }
            for x in 0..GRID {
                let wx = (x as f32 / (GRID - 1) as f32 - 0.5) * layers.size as f32;
                let wz = (z as f32 / (GRID - 1) as f32 - 0.5) * layers.size as f32;
                let i = z * GRID + x;
                heights[i] = water.desired(layers, wx, wz, heights[i]);
            }
        }
    }
    let mut blocked = vec![false; GRID * GRID];
    let spacing = layers.size as f32 / (GRID - 1) as f32;
    let radius = settings.width * 0.5 + settings.shoulder + spacing * 0.5_f32.sqrt();
    for z in 0..GRID {
        if cancelled() {
            return Ok(None);
        }
        for x in 0..GRID {
            let (wx, wz) = (
                x as f32 * spacing - layers.size as f32 * 0.5,
                z as f32 * spacing - layers.size as f32 * 0.5,
            );
            blocked[z * GRID + x] = heights[z * GRID + x] <= 1.
                || source_costs.is_some_and(|c| c[z * GRID + x] == 0)
                || obstacles.iter().any(|o| inside(o, wx, wz, radius))
                || !ground_clear_with_water(layers, alpha, settings, [wx, wz], radius, None, water);
        }
    }
    let grid = routing::Grid {
        resolution: GRID,
        size: layers.size as f32,
        heights: &heights,
        blocked: &blocked,
    };
    let route_settings = routing::Settings {
        seed: settings.seed,
        maximum_grade: settings.maximum_grade,
        slope_penalty: 4.,
        minimum_height: 1.,
        maximum_expansions: GRID * GRID,
    };
    let proposal = if let Some(costs) = source_costs {
        // Routing prices are positive; source zero sentinels have already been
        // transferred to the immutable blocked grid above.
        let minimum = costs.iter().copied().filter(|v| *v > 0).min().unwrap_or(1);
        let positive: Vec<_> = costs
            .iter()
            .map(|v| if *v == 0 { minimum } else { *v })
            .collect();
        if let Some(joins) = joins {
            crate::road_ring_join::propose(
                grid,
                policy,
                joins,
                route_settings,
                &positive,
                if joins.fit_heights_in_search {
                    settings.maximum_cut_fill
                } else {
                    0.
                },
                &cancelled,
            )?
        } else {
            crate::road_ring::propose_recovering_with_costs(
                grid,
                policy,
                route_settings,
                &positive,
                &cancelled,
            )?
        }
    } else if let Some((p, _)) = traversal {
        let Some(costs) = traversal_cost_grid(layers, sr, settings, p, &cancelled) else {
            return Ok(None);
        };
        crate::road_ring::propose_recovering_with_costs(
            grid,
            policy,
            route_settings,
            &costs,
            &cancelled,
        )?
    } else {
        crate::road_ring::propose(grid, policy, route_settings, &cancelled)?
    };
    let Some(mut proposal) = proposal else {
        return Ok(None);
    };
    report.attempts = proposal.audit.route_attempts;
    if proposal.points.is_empty() {
        report.budget_exhausted = proposal.audit.status == "budget_exceeded";
        report.ring = Some(proposal.audit);
        return Ok(Some(report));
    }
    let raw = proposal.points;
    // Access-road shaping preserves authored endpoints. A closed road has no
    // fixed endpoint: restoring its pre-smoothing origin creates a closing spike.
    let shape = crate::path_geometry::build(
        &raw,
        crate::path_geometry::Settings {
            horizontal_iterations: shaping.horizontal_iterations,
            vertical_iterations: shaping.vertical_iterations,
            resample_distance: shaping.resample_distance,
            maximum_work: shaping.maximum_work,
        },
        None,
        &cancelled,
    );
    let mut candidates = Vec::new();
    match shape {
        Ok(Some(shaped)) => {
            let mut points = shaped.points().to_vec();
            // Resampling omits its final remainder. Restore the exact loop join.
            if let Some(first) = points.first().copied() {
                if distance(*points.last().unwrap(), first) < 0.01 {
                    *points.last_mut().unwrap() = first;
                } else if points.last() != Some(&first) {
                    points.push(first);
                }
            }
            candidates.push((points, true, None));
        }
        Ok(None) => return Ok(None),
        Err(_) => {
            proposal.audit.shaping_fallback = Some("path_shaping_failed");
        }
    }
    if global_earthwork && let Some(costs) = source_costs {
        let needs_recovery = match candidates.first() {
            Some((points, _, _)) => {
                match source_shape_clear(points, costs, layers.size as f32, &cancelled) {
                    Some(clear) => !clear,
                    None => return Ok(None),
                }
            }
            None => true,
        };
        if needs_recovery {
            // A constrained proposal keeps the raw cost-safe knots while
            // improving bends without provisional height constraints. The
            // complete native corridor/earthwork fit still decides acceptance.
            let mut used = 0usize;
            for (multiplier, margin_factor) in [
                (1u16, 0.),
                (1, 1.),
                (2, 1.),
                (4, 1.),
                (1, 2.),
                (2, 2.),
                (4, 2.),
            ] {
                let Some(remaining) = shaping.maximum_work.checked_sub(used).filter(|v| *v > 0)
                else {
                    break;
                };
                let proposal = crate::road_smoothing::propose_ring_geometry(
                    &raw,
                    costs,
                    GRID,
                    layers.size as f32,
                    ShapingPolicy {
                        horizontal_iterations: shaping
                            .horizontal_iterations
                            .saturating_mul(multiplier)
                            .min(256),
                        maximum_work: remaining,
                        ..shaping
                    },
                    global_launch.map_or(0., |p| p.sample_spacing * margin_factor),
                    &cancelled,
                );
                match proposal {
                    Ok(Some(mut constrained)) => {
                        used = used
                            .checked_add(constrained.audit.work)
                            .ok_or("Ring shape work overflow")?;
                        constrained.audit.work = used;
                        candidates.push((constrained.points, true, Some(constrained.audit)));
                    }
                    Ok(None) => return Ok(None),
                    Err(_) => break,
                }
            }
        }
    }
    if global_earthwork {
        let Some(subdivided) = subdivide_ring(&raw, shaping.resample_distance, &cancelled)? else {
            return Ok(None);
        };
        candidates.push((subdivided, false, None));
    }
    candidates.push((raw, false, None));
    let mut work = 0;
    let mut accepted = None;
    for (mut points, smoothed, constrained) in candidates {
        if cancelled() {
            return Ok(None);
        }
        // Engine 8 must not smooth a source-priced loop across zero-cost cells.
        // Older saved engines retain their established candidate selection.
        if global_earthwork && let Some(costs) = source_costs {
            match source_shape_clear(&points, costs, layers.size as f32, &cancelled) {
                None => return Ok(None),
                Some(false) => {
                    proposal.audit.shaping_fallback = Some("source_cost_corridor");
                    continue;
                }
                Some(true) => {}
            }
        }
        if smoothed || water.is_some() || global_earthwork {
            // Coupled earthwork is a separate bounded primitive from horizontal
            // shaping. It uses the existing 32M primitive cap, covered by the
            // shared layout-preparation reservation. Legacy fitting retains its
            // shaping grant. No grade or cut/fill allowance changes here.
            let fit = if global_earthwork {
                fit_road_heights_on_ground(
                    layers,
                    &mut points,
                    settings,
                    MAX_WORK,
                    water,
                    true,
                    false,
                    Some(&layers.height),
                    &cancelled,
                )
            } else {
                fit_ring_heights(
                    layers,
                    &mut points,
                    settings,
                    shaping.maximum_work,
                    water,
                    &cancelled,
                )
            };
            match fit {
                Ok((work, changed)) => {
                    proposal.audit.grade_fit_work = work;
                    proposal.audit.fitted_height_samples = changed;
                }
                Err(crate::rail_grade_fit::Failure::Cancelled) => return Ok(None),
                Err(error) => {
                    #[cfg(feature = "road-diagnostics")]
                    if std::env::var_os("MAPPER_ROAD_RING_COST_DEBUG").is_some() {
                        eprintln!(
                            "Ring native footprint fit rejected: {error:?}; controls={}, constrained={}",
                            points.len(),
                            constrained.is_some()
                        );
                    }
                    #[cfg(feature = "road-diagnostics")]
                    if std::env::var_os("MAPPER_ROAD_RING_FIT_CAPTURE").is_some() {
                        capture_ring_fit(&points, &[], &[], &format!("failed-{error:?}"), None)
                            .map_err(|e| format!("Ring fit capture: {e:?}"))?;
                    }
                    #[cfg(not(feature = "road-diagnostics"))]
                    let _ = error;
                    proposal.audit.shaping_fallback = Some("grade_fit_failed");
                    continue;
                }
            }
        }
        if global_earthwork {
            let Some(path) = crate::path_geometry::Path::from_controls(
                &points,
                shaping.maximum_work,
                &cancelled,
            )
            .map_err(|e| format!("Main ring strip path: {e:?}"))?
            else {
                return Ok(None);
            };
            let mut strip_work = path.work();
            let clear = crate::road_strip::check(
                &path,
                &vec![true; points.len()],
                settings.width * 0.5,
                |n| {
                    strip_work = strip_work
                        .checked_add(n)
                        .filter(|v| *v <= MAX_WORK)
                        .ok_or("Main ring strip work exhausted")?;
                    Ok::<_, String>(())
                },
                &cancelled,
            )?;
            match clear {
                None => return Ok(None),
                Some(false) => {
                    proposal.audit.shaping_fallback = Some("ring_strip_folded");
                    continue;
                }
                Some(true) => {}
            }
            if let (Some(costs), Some(launch)) = (source_costs, global_launch) {
                match crate::road_smoothing::ring_curve_cost_clear(
                    &path,
                    costs,
                    layers.size,
                    launch.sample_spacing,
                    shaping.maximum_work,
                    &cancelled,
                ) {
                    Ok(None) => return Ok(None),
                    Ok(Some(true)) => {}
                    Ok(Some(false)) => {
                        proposal.audit.shaping_fallback = Some("source_curve_cost_corridor");
                        continue;
                    }
                    Err(_) => {
                        proposal.audit.shaping_fallback = Some("source_curve_cost_budget");
                        continue;
                    }
                }
            }
        }
        if points.len() < 5
            || points.first() != points.last()
            || points.windows(2).any(|w| {
                distance(w[0], w[1]) < 1e-4
                    || (w[0][1] - w[1][1]).abs()
                        > settings.maximum_grade * distance(w[0], w[1]) + 1e-5
            })
        {
            if smoothed {
                proposal.audit.shaping_fallback = Some("closed_path_or_grade");
            }
            continue;
        }
        if !crate::road_ring::simple_polyline(&points, layers.size as f32, &cancelled)? {
            if cancelled() {
                return Ok(None);
            }
            if smoothed {
                proposal.audit.shaping_fallback = Some("self_intersection");
            }
            continue;
        }
        let mut patch = Vec::new();
        let mut rejection = "terrain_corridor";
        let result = corridor_mode(
            layers,
            &points,
            (hr, false, true),
            settings,
            &mut work,
            &cancelled,
            |i, d, y, x, z| {
                let old = decode(layers.height[i]);
                let water_minimum = water.and_then(|w| w.minimum(layers, x, z));
                if old <= 1. && water_minimum.is_none() {
                    rejection = "water";
                    return false;
                }
                if water_minimum
                    .is_some_and(|minimum| y < minimum + settings.terrain_offset - 0.001)
                {
                    rejection = "river_clearance";
                    return false;
                }
                if protected(layers, alpha, settings, x, z) {
                    rejection = "protected_ground";
                    return false;
                }
                if obstacles.iter().any(|o| inside(o, x, z, 0.)) {
                    rejection = "monument_footprint";
                    return false;
                }
                if (old - y).abs() > settings.maximum_cut_fill {
                    rejection = "cut_fill";
                    return false;
                }
                let blend = ((settings.width * 0.5 + settings.shoulder - d) / settings.shoulder)
                    .clamp(0., 1.);
                let blend = blend * blend * (3. - 2. * blend);
                patch.push((i, encode(old + (y - old) * blend)));
                true
            },
        );
        match result {
            Err(_) => {
                report.budget_exhausted = true;
                break;
            }
            Ok(false) => {
                if cancelled() {
                    return Ok(None);
                }
                if smoothed {
                    proposal.audit.shaping_fallback = Some(rejection);
                }
                continue;
            }
            Ok(true) => {}
        }
        let mut surface = Vec::new();
        let result = corridor_mode(
            layers,
            &points,
            (sr, true, true),
            settings,
            &mut work,
            &cancelled,
            |i, d, _, x, z| {
                if protected(layers, alpha, settings, x, z)
                    || obstacles.iter().any(|o| inside(o, x, z, 0.))
                {
                    return false;
                }
                surface.push((i, d));
                true
            },
        );
        match result {
            Err(_) => {
                report.budget_exhausted = true;
                break;
            }
            Ok(false) => {
                if cancelled() {
                    return Ok(None);
                }
                continue;
            }
            Ok(true) => {}
        }
        accepted = Some((points, smoothed, patch, surface, constrained));
        break;
    }
    if let Some((points, smoothed, patch, surface, constrained)) = accepted {
        if let Some(audit) = constrained {
            report.shaping_policy = Some(shaping);
            report.shaping.work += audit.work;
            report.shaping.constrained = Some(audit);
        }
        if cancelled() {
            return Ok(None);
        }
        // Prepare every write before publishing. Cancellation never leaves a half ring.
        let mut private = layers.clone();
        for (i, h) in patch {
            report.changed_heights += usize::from(private.height[i] != h);
            private.height[i] = h;
        }
        let cells = sr * sr;
        for (i, d) in surface {
            let opacity = (settings.width * 0.5 + 1. - d).clamp(0., 1.);
            let bytes = normalize(std::array::from_fn(|c| {
                private.splat[c * cells + i] as f32 * (1. - opacity)
                    + if c == settings.splat_channel {
                        255. * opacity
                    } else {
                        0.
                    }
            }));
            for (c, value) in bytes.into_iter().enumerate() {
                private.splat[c * cells + i] = value;
            }
            private.topology[i] |= if d <= settings.width * 0.5 {
                settings.road_topology
            } else {
                settings.roadside_topology
            };
        }
        if cancelled() {
            return Ok(None);
        }
        *layers = private;
        proposal.audit.status = "found";
        proposal.audit.smoothed = smoothed;
        report.roads.push(Road {
            from: Destination::Loop,
            to: Destination::Loop,
            points,
        });
    } else {
        proposal.audit.status = if report.budget_exhausted {
            "budget_exceeded"
        } else {
            "rejected"
        };
        proposal.audit.reason =
            Some("Ring failed final terrain, grade or corridor allowance checks");
    }
    report.ring = Some(proposal.audit);
    Ok(Some(report))
}
fn validate_inputs(
    layers: &Layers,
    alpha: &[u8],
    sockets: &[Socket],
    obstacles: &[Obstacle],
    s: Settings,
) -> Result<(usize, usize), String> {
    let (hr, sr) = crate::native_layers::dimensions(layers.size)?;
    if layers.height_resolution != hr
        || layers.surface_resolution != sr
        || layers.height.len() != hr * hr
        || layers.splat.len() != sr * sr * 8
        || layers.topology.len() != sr * sr
        || alpha.len() != sr * sr
        || sockets.len() > 1024
        || obstacles.len() > 128
        || s.splat_channel >= 8
        || !s.width.is_finite()
        || !(1. ..=32.).contains(&s.width)
        || !s.shoulder.is_finite()
        || !(1. ..=32.).contains(&s.shoulder)
        || !s.maximum_grade.is_finite()
        || !(0.01..=0.5).contains(&s.maximum_grade)
        || !s.maximum_cut_fill.is_finite()
        || !(0. ..=24.).contains(&s.maximum_cut_fill)
        || !s.terrain_offset.is_finite()
        || s.terrain_offset.abs() > 2.
        || layers.height.iter().any(|h| !(0..=32766).contains(h))
        || sockets.iter().any(|p| {
            p.position
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 10000.)
        })
        || obstacles.iter().any(|o| {
            !o.yaw.is_finite()
                || o.center
                    .iter()
                    .chain(&o.extent)
                    .any(|v| !v.is_finite() || v.abs() > 10000.)
                || o.extent.iter().any(|v| *v < 0.)
        })
    {
        return Err("Invalid road inputs or source profile".into());
    }
    Ok((hr, sr))
}
struct RouteRaster {
    heights: Vec<f32>,
    blocked: Vec<bool>,
    components: Option<Vec<u32>>,
    exits: std::collections::BTreeMap<(usize, u32), Option<Exit>>,
    exit_points: usize,
    exhausted: bool,
}
#[allow(clippy::too_many_arguments)]
fn route_raster(
    layers: &Layers,
    alpha: &[u8],
    obstacles: &[Obstacle],
    s: Settings,
    access: bool,
    components: bool,
    source_costs: Option<&[u32]>,
    water: Option<crate::road_water::Guide<'_>>,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<RouteRaster>, String> {
    let spacing = layers.size as f32 / (GRID - 1) as f32;
    let mut heights: Vec<f32> = layers
        .sample_heights(GRID)?
        .into_iter()
        .map(decode)
        .collect();
    let mut blocked = vec![false; GRID * GRID];
    for z in 0..GRID {
        if cancelled() {
            return Ok(None);
        }
        for x in 0..GRID {
            let wx = x as f32 * spacing - layers.size as f32 / 2.;
            let wz = z as f32 * spacing - layers.size as f32 / 2.;
            if let Some(water) = water {
                heights[z * GRID + x] = water.desired(layers, wx, wz, heights[z * GRID + x]);
            }
            blocked[z * GRID + x] = protected(layers, alpha, s, wx, wz)
                || obstacles.iter().any(|o| inside(o, wx, wz, s.width * 0.5))
                || (access
                    && !ground_clear_with_water(
                        layers,
                        alpha,
                        s,
                        [wx, wz],
                        s.width * 0.5 + s.shoulder,
                        None,
                        water,
                    ));
        }
    }
    if access {
        // A centerline beside water/protected ground can be traversable while
        // its shoulders are not. Reserve coarse clearance before routing;
        // the full native raster audit below remains authoritative.
        let unsafe_cells: Vec<_> = blocked
            .iter()
            .zip(&heights)
            .map(|(&b, &h)| b || h <= 1.)
            .collect();
        let radius = s.width * 0.5 + s.shoulder + spacing * 0.5_f32.sqrt();
        let steps = (radius / spacing).ceil() as isize;
        for z in 0..GRID {
            if cancelled() {
                return Ok(None);
            }
            for x in 0..GRID {
                'neighbors: for dz in -steps..=steps {
                    for dx in -steps..=steps {
                        if (dx as f32).hypot(dz as f32) * spacing > radius {
                            continue;
                        }
                        let (nx, nz) = (x as isize + dx, z as isize + dz);
                        if nx < 0
                            || nz < 0
                            || nx >= GRID as isize
                            || nz >= GRID as isize
                            || unsafe_cells[nz as usize * GRID + nx as usize]
                        {
                            blocked[z * GRID + x] = true;
                            break 'neighbors;
                        }
                    }
                }
            }
        }
    }
    if let Some(costs) = source_costs {
        for (cell, &cost) in blocked.iter_mut().zip(costs) {
            *cell |= cost == 0;
        }
    }
    let components = if components {
        let Some(labels) = routing::components(
            routing::Grid {
                resolution: GRID,
                size: layers.size as f32,
                heights: &heights,
                blocked: &blocked,
            },
            routing::Settings {
                seed: s.seed,
                maximum_grade: s.maximum_grade,
                slope_penalty: 12.,
                minimum_height: 1.,
                maximum_expansions: GRID * GRID,
            },
            cancelled,
        )?
        else {
            return Ok(None);
        };
        Some(labels)
    } else {
        None
    };
    Ok(Some(RouteRaster {
        heights,
        blocked,
        components,
        exits: Default::default(),
        exit_points: 0,
        exhausted: false,
    }))
}
#[allow(clippy::too_many_arguments)]
fn cached_exit(
    raster: &mut RouteRaster,
    index: usize,
    node: Node,
    goal: Option<u32>,
    layers: &Layers,
    alpha: &[u8],
    obstacles: &[Obstacle],
    owners: &[Obstacle],
    s: Settings,
    policy: EntrancePolicy,
    water: Option<crate::road_water::Guide<'_>>,
    launch: Option<LaunchPolicy>,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Exit>, String> {
    let key = (index, goal.unwrap_or(0));
    if let Some(result) = raster.exits.get(&key) {
        return Ok(result.clone());
    }
    if raster.exits.len() >= 4096 {
        raster.exhausted = true;
        return Ok(None);
    }
    let exit = entrance_exit_goal_with_launch(
        layers,
        alpha,
        node,
        obstacles,
        owners,
        s,
        policy,
        &raster.heights,
        &raster.blocked,
        goal.zip(raster.components.as_deref()).map(|(g, c)| (c, g)),
        water,
        launch,
        cancelled,
    )?;
    if let Some(ref e) = exit {
        if raster.exit_points + e.points.len() > MAX_NETWORK_POINTS {
            raster.exhausted = true;
            return Ok(None);
        }
        raster.exit_points += e.points.len();
    }
    if !cancelled() {
        raster.exits.insert(key, exit.clone());
    }
    Ok(exit)
}
#[allow(clippy::too_many_arguments)] // Shared legacy, network and authored-entrance execution.
fn build_inner(
    layers: &mut Layers,
    alpha: &[u8],
    sockets: &[Socket],
    obstacles: &[Obstacle],
    s: Settings,
    network: Option<&[(Destination, [f32; 3])]>,
    access: Option<(EntrancePolicy, &[Obstacle], Option<ShapingPolicy>)>,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    build_inner_profiled(
        layers, alpha, sockets, obstacles, s, network, access, None, false, None, None, cancelled,
    )
}
#[allow(clippy::too_many_arguments)] // Optional traversal retains legacy contracts.
fn build_inner_profiled(
    layers: &mut Layers,
    alpha: &[u8],
    sockets: &[Socket],
    obstacles: &[Obstacle],
    s: Settings,
    network: Option<&[(Destination, [f32; 3])]>,
    access: Option<(EntrancePolicy, &[Obstacle], Option<ShapingPolicy>)>,
    traversal: Option<TraversalPolicy>,
    component_search: bool,
    source_costs: Option<SourceCosts<'_>>,
    launch: Option<LaunchPolicy>,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    let constrained = source_costs.is_some_and(|v| v.constrained);
    let water = source_costs.and_then(|v| v.water);
    let ground = source_costs.and_then(|v| v.ground);
    let source_costs = source_costs.and_then(|v| v.costs);
    let (hr, sr) = validate_inputs(layers, alpha, sockets, obstacles, s)?;
    if cancelled() {
        return Ok(None);
    }
    let original_height = match ground {
        Some(ground) => std::borrow::Cow::Borrowed(ground),
        None => std::borrow::Cow::Owned(layers.height.clone()),
    };
    let groups: BTreeSet<_> = sockets.iter().map(|s| s.monument).collect();
    if groups.len() > 128 {
        return Err("Too many road endpoint groups".into());
    }
    let mut report = Report {
        entrance_launch: launch,
        socket_count: sockets.len(),
        settings: Some(s),
        network_attachment_stage: network.is_some(),
        entrance_policy: access.map(|a| a.0),
        shaping_policy: access.and_then(|a| a.2),
        access_routing: traversal,
        access_search: component_search.then_some(COMPONENT_VERSION),
        access_river_water: water.map(|w| {
            serde_json::json!({"policy":w.policy,
            "scope":"paved monument access; source dirt-trail exclusions retained",
            "height_source":"generated river collider grid", "endpoint_heights_fixed":true})
        }),
        ..Report::default()
    };
    let mut ordered: Vec<_> = sockets.iter().collect();
    ordered.sort_by_key(|s| (s.monument, s.connector));
    if ordered
        .windows(2)
        .any(|w| (w[0].monument, w[0].connector) == (w[1].monument, w[1].connector))
    {
        return Err("Duplicate road connector identity".into());
    }
    let group_ids: Vec<_> = groups.iter().copied().collect();
    let mut ordered: Vec<_> = ordered
        .iter()
        .map(|p| Node {
            destination: Destination::Monument {
                monument: p.monument,
                connector: p.connector,
            },
            group: group_ids.binary_search(&p.monument).unwrap(),
            position: p.position,
        })
        .collect();
    if let Some((policy, _, _)) = access {
        for node in &mut ordered {
            node.position[1] = crate::spawn::sample_height(
                layers,
                node.position[0] / layers.size as f32 + 0.5,
                node.position[2] / layers.size as f32 + 0.5,
            )?
            .max(policy.minimum_height);
            if let Some(water) = water {
                node.position[1] =
                    water.desired(layers, node.position[0], node.position[2], node.position[1]);
            }
        }
    }
    if let Some(targets) = network {
        ordered.extend(targets.iter().map(|&(destination, position)| Node {
            destination,
            group: group_ids.len(),
            position,
        }));
    }
    let mut edges = Vec::new();
    for (i, a) in ordered.iter().enumerate() {
        for (j, b) in ordered.iter().enumerate().skip(i + 1) {
            if a.group != b.group {
                edges.push((distance(a.position, b.position), i, j));
            }
        }
    }
    edges.sort_by(|a, b| a.0.total_cmp(&b.0).then((a.1, a.2).cmp(&(b.1, b.2))));
    let mut connected: Vec<_> = (0..groups.len() + usize::from(network.is_some())).collect();
    let spacing = layers.size as f32 / (GRID - 1) as f32;
    let mut work = 0;
    let costs = if let Some(costs) = source_costs {
        Some(costs.iter().map(|v| (*v).max(1)).collect())
    } else if let Some(policy) = traversal {
        policy.validate()?;
        let Some(costs) = traversal_cost_grid(layers, sr, s, policy, &cancelled) else {
            return Ok(None);
        };
        Some(costs)
    } else {
        None
    };
    let mut cached: Option<(usize, RouteRaster)> = None;
    let mut examined_edges = 0;
    'routes: for (_, ai, bi) in edges {
        if cancelled() {
            return Ok(None);
        }
        let a = ordered[ai];
        let b = ordered[bi];
        let ag = a.group;
        let bg = b.group;
        if connected[ag] == connected[bg] {
            continue;
        }
        if !component_search {
            if report.attempts >= MAX_ATTEMPTS {
                report.budget_exhausted = true;
                break;
            }
            report.attempts += 1;
        } else {
            examined_edges += 1;
            if examined_edges > 32768 {
                report.budget_exhausted = true;
                break;
            }
        }
        if cached
            .as_ref()
            .is_none_or(|(revision, _)| !component_search || *revision != report.roads.len())
        {
            let Some(raster) = route_raster(
                layers,
                alpha,
                obstacles,
                s,
                access.is_some(),
                component_search,
                source_costs,
                water,
                cancelled,
            )?
            else {
                return Ok(None);
            };
            cached = Some((report.roads.len(), raster));
        }
        let raster = &mut cached.as_mut().unwrap().1;
        let exits = if let Some((policy, owners, _)) = access {
            let (mut first, mut last) = if component_search {
                (
                    cached_exit(
                        raster, ai, a, None, layers, alpha, obstacles, owners, s, policy, water,
                        launch, cancelled,
                    )?,
                    cached_exit(
                        raster, bi, b, None, layers, alpha, obstacles, owners, s, policy, water,
                        launch, cancelled,
                    )?,
                )
            } else {
                (
                    entrance_exit(
                        layers,
                        alpha,
                        a,
                        obstacles,
                        owners,
                        s,
                        policy,
                        &raster.heights,
                        &raster.blocked,
                        cancelled,
                    )?,
                    entrance_exit(
                        layers,
                        alpha,
                        b,
                        obstacles,
                        owners,
                        s,
                        policy,
                        &raster.heights,
                        &raster.blocked,
                        cancelled,
                    )?,
                )
            };
            if component_search && let (Some(f), Some(l)) = (&first, &last) {
                let labels = raster.components.as_ref().unwrap();
                let (fc, lc) = (
                    labels[f.cell[1] * GRID + f.cell[0]],
                    labels[l.cell[1] * GRID + l.cell[0]],
                );
                if fc != lc {
                    let alternate = cached_exit(
                        raster,
                        ai,
                        a,
                        Some(lc),
                        layers,
                        alpha,
                        obstacles,
                        owners,
                        s,
                        policy,
                        water,
                        launch,
                        cancelled,
                    )?;
                    if alternate.is_some() {
                        first = alternate;
                    } else {
                        let alternate = cached_exit(
                            raster,
                            bi,
                            b,
                            Some(fc),
                            layers,
                            alpha,
                            obstacles,
                            owners,
                            s,
                            policy,
                            water,
                            launch,
                            cancelled,
                        )?;
                        if alternate.is_some() {
                            last = alternate;
                        }
                    }
                }
            }
            if cancelled() {
                return Ok(None);
            }
            if raster.exhausted {
                report.budget_exhausted = true;
                break 'routes;
            }
            match (first, last) {
                (Some(first), Some(last)) => Some((first, last)),
                _ => {
                    report.reject("entrance_exit");
                    continue;
                }
            }
        } else {
            None
        };
        let heights = &raster.heights;
        let blocked = &raster.blocked;
        let snap = |p: [f32; 3]| -> Option<[usize; 2]> {
            let x = (p[0] / spacing + (GRID - 1) as f32 / 2.).round() as isize;
            let z = (p[2] / spacing + (GRID - 1) as f32 / 2.).round() as isize;
            let mut best: Option<(f32, [usize; 2])> = None;
            for dz in -3..=3 {
                for dx in -3..=3 {
                    let (gx, gz) = (x + dx, z + dz);
                    if gx < 0 || gz < 0 || gx >= GRID as isize || gz >= GRID as isize {
                        continue;
                    }
                    let i = gz as usize * GRID + gx as usize;
                    if blocked[i] || heights[i] <= 1. {
                        continue;
                    }
                    let candidate = [
                        gx as f32 * spacing - layers.size as f32 / 2.,
                        heights[i],
                        gz as f32 * spacing - layers.size as f32 / 2.,
                    ];
                    let d = distance(candidate, p);
                    if (d < 0.001 || (candidate[1] - p[1]).abs() / d <= s.maximum_grade)
                        && best.as_ref().is_none_or(|old| d < old.0)
                    {
                        best = Some((d, [gx as usize, gz as usize]));
                    }
                }
            }
            best.map(|v| v.1)
        };
        let (start, end) = if let Some((a, b)) = &exits {
            (a.cell, b.cell)
        } else {
            let (Some(start), Some(end)) = (snap(a.position), snap(b.position)) else {
                report.reject("endpoint_snap");
                continue;
            };
            (start, end)
        };
        if component_search {
            let labels = raster.components.as_ref().unwrap();
            if labels[start[1] * GRID + start[0]] != labels[end[1] * GRID + end[0]] {
                report.reject("disconnected_entrances");
                continue;
            }
            if report.attempts >= MAX_ATTEMPTS {
                report.budget_exhausted = true;
                break;
            }
            report.attempts += 1;
        }
        let path = routing::route_with_costs(
            routing::Grid {
                resolution: GRID,
                size: layers.size as f32,
                heights,
                blocked,
            },
            start,
            end,
            routing::Settings {
                seed: s.seed,
                maximum_grade: s.maximum_grade,
                slope_penalty: 12.,
                minimum_height: 1.,
                maximum_expansions: GRID * GRID,
            },
            costs.as_deref(),
            cancelled,
        )?;
        let mut points = match path {
            routing::Outcome::Found(p) => p,
            routing::Outcome::Cancelled => return Ok(None),
            _ => {
                report.reject("inland_route");
                continue;
            }
        };
        if let Some((first, last)) = &exits {
            let mut joined = first.points.clone();
            joined.extend(points.into_iter().skip(1));
            joined.extend(last.points.iter().rev().skip(1).copied());
            points = joined;
        } else {
            if distance(points[0], a.position) > 0.01 {
                points.insert(0, a.position);
            } else if network.is_some() || access.is_some() {
                points[0] = a.position;
            }
            if distance(*points.last().unwrap(), b.position) > 0.01 {
                points.push(b.position);
            } else if network.is_some() || access.is_some() {
                *points.last_mut().unwrap() = b.position;
            }
        }
        let mut proposal = None;
        if let Some(policy) = report.shaping_policy {
            report.shaping.attempted += 1;
            let (first, last) = if launch.is_some() {
                (0, points.len() - 1)
            } else {
                exits.as_ref().map_or((0, points.len() - 1), |(a, b)| {
                    (a.points.len() - 1, points.len() - b.points.len())
                })
            };
            match shaped_points(&points[first..=last], policy, &cancelled) {
                Ok(Some(middle)) => {
                    report.shaping.work += middle.work;
                    let clear = if let Some(costs) = source_costs {
                        source_shape_clear(&middle.points, costs, layers.size as f32, cancelled)
                    } else {
                        Some(true)
                    };
                    match clear {
                        None => return Ok(None),
                        Some(false) => report.shaping.fallback("source_cost_clearance_or_budget"),
                        Some(true) => {
                            let mut shaped = points[..first].to_vec();
                            shaped.extend(middle.points);
                            shaped.extend_from_slice(&points[last + 1..]);
                            proposal = Some(shaped);
                        }
                    }
                }
                Ok(None) => return Ok(None),
                Err(crate::path_geometry::Failure::Budget) => {
                    report.shaping.budget_fallbacks += 1;
                    report.shaping.fallback("path_budget");
                    report.shaping.work += policy.maximum_work;
                }
                Err(crate::path_geometry::Failure::Invalid) => {
                    report.shaping.fallback("path_invalid");
                }
            }
        }
        let raw_points = points;
        let mut refine = constrained;
        let fitted_launch = launch.is_some_and(|p| {
            matches!(
                p.engine,
                LaunchEngine::Fitted
                    | LaunchEngine::PreserveEntrances
                    | LaunchEngine::AdaptiveEntrances
                    | LaunchEngine::ProtectedEdges
                    | LaunchEngine::BedFeedback
                    | LaunchEngine::SurfaceChecked
                    | LaunchEngine::GlobalEarthwork
            )
        });
        let adaptive = launch.is_some_and(|p| {
            matches!(
                p.engine,
                LaunchEngine::AdaptiveEntrances
                    | LaunchEngine::ProtectedEdges
                    | LaunchEngine::BedFeedback
                    | LaunchEngine::SurfaceChecked
                    | LaunchEngine::GlobalEarthwork
            )
        });
        let mut recover_entrances = launch.is_some_and(|p| {
            matches!(
                p.engine,
                LaunchEngine::PreserveEntrances
                    | LaunchEngine::AdaptiveEntrances
                    | LaunchEngine::ProtectedEdges
                    | LaunchEngine::BedFeedback
                    | LaunchEngine::SurfaceChecked
                    | LaunchEngine::GlobalEarthwork
            )
        }) && exits.is_some();
        let (points, patch, surface) = 'shape: loop {
            if proposal.is_none() && refine {
                refine = false;
                report
                    .shaping
                    .constrained
                    .get_or_insert_with(Default::default)
                    .attempts += 1;
                let (first, last) = if launch.is_some() {
                    (0, raw_points.len() - 1)
                } else {
                    exits.as_ref().map_or((0, raw_points.len() - 1), |(a, b)| {
                        (a.points.len() - 1, raw_points.len() - b.points.len())
                    })
                };
                let refinement_costs = source_costs
                    .unwrap()
                    .iter()
                    .zip(blocked)
                    .map(|(&cost, &blocked)| if blocked { 0 } else { cost })
                    .collect::<Vec<_>>();
                match crate::road_smoothing::build_with_water(
                    &raw_points[first..=last],
                    layers,
                    &refinement_costs,
                    GRID,
                    s,
                    report.shaping_policy.unwrap(),
                    water,
                    cancelled,
                ) {
                    Ok(Some(refined)) => {
                        report.shaping.work += refined.audit.work;
                        report
                            .shaping
                            .constrained
                            .get_or_insert_with(Default::default)
                            .merge(&refined.audit);
                        let mut joined = raw_points[..first].to_vec();
                        joined.extend(refined.points);
                        joined.extend_from_slice(&raw_points[last + 1..]);
                        proposal = Some(joined);
                    }
                    Ok(None) => return Ok(None),
                    Err(crate::path_geometry::Failure::Budget) => {
                        report.shaping.budget_fallbacks += 1;
                        report.shaping.fallback("constrained_budget");
                        report.shaping.work += report.shaping_policy.unwrap().maximum_work;
                    }
                    Err(crate::path_geometry::Failure::Invalid) => {
                        report.shaping.fallback("constrained_invalid")
                    }
                }
            }
            let mut preserving_entrances = false;
            if proposal.is_none() && recover_entrances {
                recover_entrances = false;
                report
                    .shaping
                    .entrance_recovery
                    .get_or_insert_with(Default::default)
                    .attempts += 1;
                let (a, b) = exits.as_ref().unwrap();
                let policy = report.shaping_policy.unwrap();
                match shape_between_entrances(
                    layers,
                    &raw_points,
                    (a.points.len() - 1, raw_points.len() - b.points.len()),
                    s,
                    policy,
                    water,
                    adaptive,
                    ground,
                    cancelled,
                ) {
                    Ok(Some(recovered)) => {
                        report.shaping.work += recovered.work;
                        let clear = source_costs.map_or(Some(true), |costs| {
                            source_shape_clear(
                                &recovered.points,
                                costs,
                                layers.size as f32,
                                cancelled,
                            )
                        });
                        match clear {
                            None => return Ok(None),
                            Some(false) => report
                                .shaping
                                .fallback("entrance_recovery_source_clearance_or_budget"),
                            Some(true) => {
                                report.shaping.entrance_recovery.as_mut().unwrap().proposals += 1;
                                if recovered.saved_grade_fit {
                                    *report
                                        .shaping
                                        .entrance_recovery
                                        .as_mut()
                                        .unwrap()
                                        .saved_grade_proposals
                                        .get_or_insert(0) += 1;
                                }
                                proposal = Some(recovered.points);
                                preserving_entrances = true;
                            }
                        }
                    }
                    Ok(None) | Err(crate::rail_grade_fit::Failure::Cancelled) => return Ok(None),
                    Err(e) => {
                        report.shaping.work += policy.maximum_work;
                        let reason = match e {
                            crate::rail_grade_fit::Failure::Budget => {
                                report.shaping.budget_fallbacks += 1;
                                "entrance_recovery_budget"
                            }
                            crate::rail_grade_fit::Failure::Invalid => "entrance_recovery_invalid",
                            crate::rail_grade_fit::Failure::Infeasible => "entrance_recovery_fit",
                            crate::rail_grade_fit::Failure::Cancelled => unreachable!(),
                        };
                        report.shaping.fallback(reason);
                    }
                }
            }
            let using_shape = proposal.is_some();
            let mut points = proposal.take().unwrap_or_else(|| raw_points.clone());
            if fitted_launch {
                normalize_fitted_controls(&mut points, layers.size);
            }
            if !preserving_entrances && (water.is_some() || (fitted_launch && using_shape)) {
                let reason = if water.is_some() {
                    "river_bed_fit"
                } else {
                    "checked_bed_fit"
                };
                match fit_road_heights_on_ground(
                    layers,
                    &mut points,
                    if fitted_launch {
                        // Source-style terrain blending and the subsequent
                        // control resample do not preserve the proposal grade.
                        // Reserve a quarter of the saved limit for that pass;
                        // the final terrain and collision audits still decide.
                        Settings {
                            maximum_grade: s.maximum_grade * 0.75,
                            ..s
                        }
                    } else {
                        s
                    },
                    report.shaping_policy.unwrap().maximum_work,
                    water,
                    false,
                    fitted_launch,
                    ground,
                    cancelled,
                ) {
                    Ok((used, _)) => report.shaping.work += used,
                    Err(crate::rail_grade_fit::Failure::Cancelled) => return Ok(None),
                    Err(_) if using_shape => {
                        report.shaping.fallback(reason);
                        continue 'shape;
                    }
                    Err(_) => {
                        report.reject(reason);
                        continue 'routes;
                    }
                }
            }
            if adaptive
                && (using_shape
                    || launch.is_some_and(|p| {
                        matches!(
                            p.engine,
                            LaunchEngine::SurfaceChecked | LaunchEngine::GlobalEarthwork
                        )
                    }))
            {
                let allowance = report.shaping_policy.unwrap().maximum_work;
                match candidate_strip_clear(&points, s.width, allowance, cancelled) {
                    Ok(Some((true, used))) => report.shaping.work += used,
                    Ok(Some((false, used))) => {
                        report.shaping.work += used;
                        report.shaping.fallback("road_strip_fold");
                        if !using_shape {
                            report.reject("raw_road_strip_fold");
                            continue 'routes;
                        }
                        continue 'shape;
                    }
                    Ok(None) => return Ok(None),
                    Err(e) => {
                        report.shaping.work += allowance;
                        if e == crate::path_geometry::Failure::Budget {
                            report.shaping.budget_fallbacks += 1;
                            report.shaping.fallback("road_strip_budget");
                        } else {
                            report.shaping.fallback("road_strip_invalid");
                        }
                        if !using_shape {
                            report.reject("raw_road_strip_unverified");
                            continue 'routes;
                        }
                        continue 'shape;
                    }
                }
            }
            // Sampled terrain already includes the previous road's terrain offset.
            // Matching X/Z must not silently discard the original path's endpoint Y.
            // Recheck the resulting full join, not just the snapped grid-cell slope.
            if (network.is_some() || access.is_some())
                && points.windows(2).any(|w| {
                    let run = distance(w[0], w[1]);
                    (w[0][1] - w[1][1]).abs() > s.maximum_grade * run + 1e-5
                })
            {
                if using_shape {
                    report.shaping.fallback("joined_grade");
                    continue 'shape;
                }
                report.reject("joined_grade");
                continue 'routes;
            }
            // Assess every full-resolution terrain sample before committing the candidate.
            // Protected authored ground/holes are retained, not flattened over.
            let mut patch = Vec::new();
            let mut corridor_rejection = "full_width_corridor";
            let mut last_sample = ([0.; 3], 0);
            let checked = corridor_mode(
                layers,
                &points,
                (hr, false, report.shaping_policy.is_some()),
                s,
                &mut work,
                &cancelled,
                |i, d, y, x, z| {
                    let old = decode(layers.height[i]);
                    let cell = |w: f32| {
                        ((w / layers.size as f32 + 0.5) * sr as f32)
                            .floor()
                            .clamp(0., (sr - 1) as f32) as usize
                    };
                    last_sample = ([x, old, z], layers.topology[cell(z) * sr + cell(x)]);
                    let water_minimum = water.and_then(|w| w.minimum(layers, x, z));
                    if old <= 1. && water_minimum.is_none() {
                        corridor_rejection = "corridor_water";
                        return false;
                    }
                    if water_minimum.is_some_and(|minimum| y < minimum + s.terrain_offset - 0.001) {
                        corridor_rejection = "corridor_river_clearance";
                        return false;
                    }
                    if alpha[cell(z) * sr + cell(x)] < 128 {
                        corridor_rejection = "corridor_hole";
                        return false;
                    }
                    if d <= s.width * 0.5
                        && obstacles.iter().any(|o| {
                            Some(o.monument) != a.monument()
                                && Some(o.monument) != b.monument()
                                && inside(o, x, z, 0.)
                        })
                    {
                        corridor_rejection = "corridor_other_monument";
                        return false;
                    }
                    if protected(layers, alpha, s, x, z) {
                        if let Some((policy, owners, _)) = access {
                            corridor_rejection = "corridor_protected_ground";
                            let mask =
                                layers.topology[cell(z) * sr + cell(x)] & s.protected_topology;
                            return mask & !policy.owned_topology == 0
                                && !owners.iter().any(|o| {
                                    Some(o.monument) != a.monument()
                                        && Some(o.monument) != b.monument()
                                        && inside(o, x, z, 0.)
                                })
                                && owners.iter().any(|o| {
                                    (Some(o.monument) == a.monument()
                                        || Some(o.monument) == b.monument())
                                        && inside(o, x, z, 0.)
                                });
                        }
                        return true;
                    }
                    if (decode(original_height[i]) - y).abs() > s.maximum_cut_fill {
                        corridor_rejection = "corridor_cut_fill";
                        return false;
                    }
                    let blend = ((s.width * 0.5 + s.shoulder - d) / s.shoulder).clamp(0., 1.);
                    let blend = blend * blend * (3. - 2. * blend);
                    patch.push((i, encode(old + (y - old) * blend)));
                    true
                },
            );
            match checked {
                Err(_) => {
                    report.budget_exhausted = true;
                    break 'routes;
                }
                Ok(false) => {
                    if cancelled() {
                        return Ok(None);
                    }
                    if using_shape {
                        report.shaping.fallback(corridor_rejection);
                        continue 'shape;
                    }
                    report.reject(corridor_rejection);
                    if access.is_some() {
                        report.rejection_samples.push(GroundRejection {
                            position: last_sample.0,
                            topology: last_sample.1,
                            reason: corridor_rejection.into(),
                        });
                    }
                    continue 'routes;
                }
                Ok(true) => {}
            }
            // Prepare surface writes too, so a bounded rejection doesn't half-apply a road.
            let mut surface = Vec::new();
            let checked = corridor_mode(
                layers,
                &points,
                (sr, true, report.shaping_policy.is_some()),
                s,
                &mut work,
                &cancelled,
                |i, d, _, x, z| {
                    if !protected(layers, alpha, s, x, z)
                        && (network.is_none() || layers.topology[i] & s.road_topology == 0)
                    {
                        surface.push((i, d));
                    }
                    true
                },
            );
            match checked {
                Err(_) => {
                    report.budget_exhausted = true;
                    break 'routes;
                }
                Ok(false) => return Ok(None),
                Ok(true) => {}
            }
            if using_shape {
                report.shaping.accepted += 1;
                if preserving_entrances {
                    report.shaping.entrance_recovery.as_mut().unwrap().accepted += 1;
                }
            }
            break (points, patch, surface);
        };
        for (i, h) in patch {
            if layers.height[i] != h {
                report.changed_heights += 1;
                layers.height[i] = h;
            }
        }
        let cells = sr * sr;
        for (i, d) in surface {
            let opacity = ((s.width * 0.5 + 1. - d) / 1.).clamp(0., 1.);
            let values = std::array::from_fn(|c| {
                layers.splat[c * cells + i] as f32 * (1. - opacity)
                    + if c == s.splat_channel {
                        255. * opacity
                    } else {
                        0.
                    }
            });
            let bytes = normalize(values);
            for (c, value) in bytes.into_iter().enumerate() {
                layers.splat[c * cells + i] = value;
            }
            layers.topology[i] |= if d <= s.width * 0.5 {
                s.road_topology
            } else {
                s.roadside_topology
            };
        }
        let old = connected[bg];
        let new = connected[ag];
        for id in &mut connected {
            if *id == old {
                *id = new;
            }
        }
        if let Some((first, last)) = exits {
            for (node, exit) in [(a, first), (b, last)] {
                if let Destination::Monument {
                    monument,
                    connector,
                } = node.destination
                {
                    report.entrance_joins.push(EntranceJoin {
                        monument,
                        connector,
                        points: exit.points,
                    });
                }
            }
        }
        report.roads.push(Road {
            from: a.destination,
            to: b.destination,
            points,
        });
        if connected.iter().all(|id| *id == connected[0]) {
            break;
        }
    }
    // All endpoints in a disconnected component remain explicit. No complete network
    // is claimed merely because one component has more monuments than another.
    if network.is_some() {
        let root = connected[group_ids.len()];
        report.unresolved = group_ids
            .into_iter()
            .enumerate()
            .filter_map(|(i, id)| (connected[i] != root).then_some(id))
            .collect();
    } else if connected.iter().collect::<BTreeSet<_>>().len() > 1 {
        report.unresolved = group_ids;
    }
    Ok((!cancelled()).then_some(report))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ground_circle_preserves_inclusive_hypot_boundaries() {
        for dz_index in -64..=64 {
            let dz = dz_index as f32 * 0.493_17;
            let dz_squared = f64::from(dz).powi(2);
            for dx_index in -64..=64 {
                let dx = dx_index as f32 * 0.471_83;
                let distance = dx.hypot(dz);
                for radius in [
                    0.,
                    1.,
                    12.,
                    25.,
                    distance.next_down(),
                    distance,
                    distance.next_up(),
                ] {
                    assert_eq!(
                        outside_ground_circle(dx, dz, dz_squared, radius, ground_radius_band(radius)),
                        distance > radius,
                        "Point ({dx}, {dz}), radius {radius}",
                    );
                }
            }
        }
    }
    #[test]
    fn global_ground_supports_checked_native_routing_without_a_custom_cost_grid() {
        let base = terrain();
        let alpha = vec![255; base.surface_resolution * base.surface_resolution];
        let sockets = [socket(1, -150., 0.), socket(2, 150., 0.)];
        let build = |layers: &mut Layers, engine, ground: Option<&[i16]>| {
            build_launched_access(
                layers,
                &alpha,
                &sockets,
                &[],
                LaunchedAccess {
                    profile: ProfiledAccess {
                        access: Access {
                            settings: settings(),
                            policy: entrance_policy(),
                            owner_footprints: &[],
                            network: None,
                        },
                        shaping: shaping_policy(),
                        traversal: TraversalPolicy {
                            traversable_topology: 2,
                            normal_cost: 101,
                            traversable_cost: 5000,
                        },
                    },
                    policy: LaunchPolicy {
                        engine,
                        minimum_run_widths: 1.,
                        sample_spacing: 1.,
                        maximum_work: 2_000_000,
                    },
                    costs: None,
                    constrained: false,
                    water: None,
                    ground,
                },
                || false,
            )
        };
        let mut output = base.clone();
        let report = build(
            &mut output,
            LaunchEngine::GlobalEarthwork,
            Some(&base.height),
        )
        .unwrap()
        .unwrap();
        assert_eq!(report.roads.len(), 1);
        assert!(report.unresolved.is_empty());
        assert!(
            output
                .height
                .iter()
                .zip(&base.height)
                .all(|(&a, &b)| (decode(a) - decode(b)).abs()
                    <= settings().maximum_cut_fill + 1000. / 32766.)
        );
        assert!(build(&mut base.clone(), LaunchEngine::GlobalEarthwork, None).is_err());
        assert!(
            build(
                &mut base.clone(),
                LaunchEngine::SurfaceChecked,
                Some(&base.height)
            )
            .is_err()
        );
    }
    #[test]
    fn shared_ground_keeps_later_fits_within_the_original_earthwork_interval() {
        let mut layers = terrain();
        let ground = layers.height.clone();
        layers.height.fill(encode(26.)); // Earlier road family already raised by 6 m.
        let settings = Settings {
            maximum_cut_fill: 8.,
            ..settings()
        };
        let input = vec![[0., 26., -200.], [0., 32., 0.], [0., 26., 200.]];
        let mut legacy = input.clone();
        fit_road_heights(
            &layers,
            &mut legacy,
            settings,
            2_000_000,
            None,
            false,
            true,
            &|| false,
        )
        .unwrap();
        assert!(
            legacy[1][1] > 31.9,
            "A per-family allowance can exceed the original budget"
        );
        let mut cumulative = input.clone();
        fit_road_heights_on_ground(
            &layers,
            &mut cumulative,
            settings,
            2_000_000,
            None,
            false,
            true,
            Some(&ground),
            &|| false,
        )
        .unwrap();
        let terrain_target = cumulative[1][1] + settings.terrain_offset;
        assert!(
            terrain_target <= decode(ground[0]) + 8. && terrain_target > 27.,
            "{terrain_target}"
        );
        assert_eq!([cumulative[0], cumulative[2]], [input[0], input[2]]);
        // A third family sees the same original interval rather than another 8 m.
        layers.height.fill(encode(28.));
        let mut third = input.clone();
        fit_road_heights_on_ground(
            &layers,
            &mut third,
            settings,
            2_000_000,
            None,
            false,
            true,
            Some(&ground),
            &|| false,
        )
        .unwrap();
        assert!(third[1][1] + settings.terrain_offset <= decode(ground[0]) + 8.);
        assert_eq!(ground, vec![encode(20.); ground.len()]);
        let mut cancelled = input.clone();
        assert_eq!(
            fit_road_heights_on_ground(
                &layers,
                &mut cancelled,
                settings,
                2_000_000,
                None,
                false,
                true,
                Some(&ground),
                &|| true
            ),
            Err(crate::rail_grade_fit::Failure::Cancelled)
        );
        assert_eq!(cancelled, input);
        assert!(
            fit_road_heights_on_ground(
                &layers,
                &mut cancelled,
                settings,
                2_000_000,
                None,
                false,
                true,
                Some(&ground[..1]),
                &|| false
            )
            .is_err()
        );
    }
    #[test]
    fn captured_raw_fallback_folds_before_height_fitting_and_is_not_a_valid_strip() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/road-raw-fallback-fold.json"
        ))
        .unwrap();
        let points: Vec<[f32; 3]> = serde_json::from_value(fixture["controls"].clone()).unwrap();
        let width = fixture["width"].as_f64().unwrap() as f32;
        assert!(
            !candidate_strip_clear(&points, width, 2_000_000, &|| false)
                .unwrap()
                .unwrap()
                .0
        );
        assert!(
            candidate_strip_clear(&points, width, 2_000_000, &|| true)
                .unwrap()
                .is_none()
        );
        assert!(candidate_strip_clear(&points, width, 1, &|| false).is_err());
        let straight = [[0., 0., 0.], [0., 0., 20.], [0., 0., 40.]];
        assert!(
            candidate_strip_clear(&straight, width, 2_000_000, &|| false)
                .unwrap()
                .unwrap()
                .0
        );
    }
    #[test]
    fn adaptive_inland_fit_uses_saved_grade_when_headroom_is_infeasible() {
        let mut layers = terrain();
        let n = layers.height_resolution;
        for row in layers.height.chunks_exact_mut(n) {
            for (x, h) in row.iter_mut().enumerate() {
                *h = encode(100. + 0.19 * (x as f32 / (n - 1) as f32 - 0.5) * layers.size as f32);
            }
        }
        let raw = [
            [-130., 75.3, 0.],
            [-100., 81., 0.],
            [0., 100., 0.],
            [100., 119., 0.],
            [130., 124.7, 0.],
        ];
        let policy = ShapingPolicy {
            horizontal_iterations: 4,
            vertical_iterations: 16,
            resample_distance: 7.5,
            maximum_work: 2_000_000,
        };
        assert!(matches!(
            shape_between_entrances(
                &layers,
                &raw,
                (1, 3),
                settings(),
                policy,
                None,
                false,
                None,
                &|| false
            ),
            Err(crate::rail_grade_fit::Failure::Infeasible)
        ));
        let fit = shape_between_entrances(
            &layers,
            &raw,
            (1, 3),
            settings(),
            policy,
            None,
            true,
            None,
            &|| false,
        )
        .unwrap()
        .unwrap();
        assert!(fit.saved_grade_fit && fit.work <= policy.maximum_work);
        assert_eq!(&fit.points[..2], &raw[..2]);
        assert_eq!(&fit.points[fit.points.len() - 2..], &raw[3..]);
        assert!(
            fit.points.windows(2).all(|w| (w[0][1] - w[1][1]).abs()
                <= settings().maximum_grade * distance(w[0], w[1]) + 1e-5)
        );
        assert!(
            candidate_strip_clear(&fit.points, settings().width, policy.maximum_work, &|| {
                false
            })
            .unwrap()
            .unwrap()
            .0
        );
    }
    #[test]
    fn inland_recovery_preserves_both_entrance_chains_and_bounds_fitted_work() {
        let layers = terrain();
        let raw = vec![
            [-80., 20., 0.],
            [-70., 21.8, 0.],
            [-60., 20., 0.],
            [-40., 20., 20.],
            [-20., 20., 0.],
            [0., 20., 0.],
            [10., 21.8, 0.],
            [20., 20., 0.],
        ];
        let policy = ShapingPolicy {
            horizontal_iterations: 4,
            vertical_iterations: 16,
            resample_distance: 7.5,
            maximum_work: 2_000_000,
        };
        let result = shape_between_entrances(
            &layers,
            &raw,
            (2, 5),
            settings(),
            policy,
            None,
            false,
            None,
            &|| false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(&result.points[..3], &raw[..3]);
        assert_eq!(&result.points[result.points.len() - 3..], &raw[5..]);
        assert!(result.work > 0 && result.work <= policy.maximum_work);
        assert!(result.points.len() > raw.len());
        assert_ne!(&result.points[3..result.points.len() - 3], &raw[3..5]);
        assert!(
            result
                .points
                .windows(2)
                .all(|w| (w[0][1] - w[1][1]).abs()
                    <= settings().maximum_grade * distance(w[0], w[1]))
        );
        assert!(
            (result.points[1][1] - result.points[0][1]).abs()
                > settings().maximum_grade * 0.75 * distance(result.points[0], result.points[1])
        );
        assert!(
            shape_between_entrances(
                &layers,
                &raw,
                (2, 5),
                settings(),
                policy,
                None,
                false,
                None,
                &|| true
            )
            .unwrap()
            .is_none()
        );
        assert!(matches!(
            shape_between_entrances(
                &layers,
                &raw,
                (5, 2),
                settings(),
                policy,
                None,
                false,
                None,
                &|| false
            ),
            Err(crate::rail_grade_fit::Failure::Invalid)
        ));
        assert!(matches!(
            shape_between_entrances(
                &layers,
                &raw,
                (2, 5),
                settings(),
                ShapingPolicy {
                    maximum_work: 1,
                    ..policy
                },
                None,
                false,
                None,
                &|| false
            ),
            Err(crate::rail_grade_fit::Failure::Budget)
        ));
    }
    #[test]
    fn fitted_controls_remove_world_precision_remainders_keep_attachment_poses() {
        let first = [1800., 20., 1700.];
        let last = [1900., 21., 1800.];
        let mut points = vec![
            first,
            [1800.0001, 20., 1700.],
            [1850., 20.5, 1750.],
            [1900.0001, 21.000002, 1800.],
            last,
        ];
        normalize_fitted_controls(&mut points, 4000);
        assert_eq!(points, [first, [1850., 20.5, 1750.], last]);
        let once = points.clone();
        normalize_fitted_controls(&mut points, 4000);
        assert_eq!(points, once);
        // Coincident XZ with a real height discontinuity must remain visible.
        let kink = [first[0], first[1] + 0.01, first[2]];
        let mut vertical = vec![first, kink, last];
        normalize_fitted_controls(&mut vertical, 4000);
        assert_eq!(vertical, [first, kink, last]);
        // Both endpoint poses survive even when the entire path is tiny.
        let close = [first[0] + 0.0001, first[1], first[2]];
        let mut tiny = vec![first, first, close];
        normalize_fitted_controls(&mut tiny, 4000);
        assert_eq!(tiny, [first, close]);
    }
    #[test]
    fn fitted_open_heights_keep_joins_and_bound_grade_without_water() {
        let layers = terrain();
        let mut points = vec![[-30., 20., 0.], [-20., 24., 0.], [0., 20., 0.]];
        let endpoints = [points[0], points[2]];
        assert!((points[1][1] - points[0][1]) / distance(points[0], points[1]) > 0.2);
        let (_, changed) = fit_road_heights(
            &layers,
            &mut points,
            settings(),
            2_000_000,
            None,
            false,
            true,
            &|| false,
        )
        .unwrap();
        assert!(changed > 0);
        assert_eq!([points[0], points[2]], endpoints);
        assert!(
            points
                .windows(2)
                .all(|w| (w[0][1] - w[1][1]).abs()
                    <= settings().maximum_grade * distance(w[0], w[1]))
        );
    }
    #[test]
    fn fitted_heights_cannot_use_protected_ground_as_cut_fill_allowance() {
        let mut layers = terrain();
        layers.height.fill(encode(24.));
        layers.topology.fill(settings().protected_topology);
        let input = vec![[-30., 20., 0.], [-20., 20., 0.], [0., 20., 0.]];
        let settings = Settings {
            maximum_cut_fill: 8.,
            ..settings()
        };
        let mut unconstrained = input.clone();
        fit_road_heights(
            &layers,
            &mut unconstrained,
            settings,
            2_000_000,
            None,
            false,
            false,
            &|| false,
        )
        .unwrap();
        let mut protected = input.clone();
        assert_eq!(
            fit_road_heights(
                &layers,
                &mut protected,
                settings,
                2_000_000,
                None,
                false,
                true,
                &|| false
            ),
            Err(crate::rail_grade_fit::Failure::Infeasible)
        );
        assert_eq!(protected, input);
    }
    #[test]
    fn profiled_cliff_access_retains_water_hole_and_monument_guards() {
        let mut base = terrain();
        base.topology.fill(2 | 16);
        let alpha = vec![255; base.surface_resolution * base.surface_resolution];
        let sockets = [
            Socket {
                monument: 1,
                connector: 1,
                position: [-150., 20., 0.],
            },
            Socket {
                monument: 2,
                connector: 2,
                position: [150., 20., 0.],
            },
        ];
        let s = Settings {
            protected_topology: 1024 | 2,
            ..settings()
        };
        let access = || Access {
            settings: s,
            policy: EntrancePolicy {
                minimum_height: 1.,
                owned_topology: 1024,
                maximum_exit_distance: 512.,
                maximum_exit_nodes: 4096,
            },
            owner_footprints: &[],
            network: None,
        };
        let shaping = ShapingPolicy {
            horizontal_iterations: 4,
            vertical_iterations: 16,
            resample_distance: 7.5,
            maximum_work: 2_000_000,
        };
        let traversal = TraversalPolicy {
            traversable_topology: 2,
            normal_cost: 101,
            traversable_cost: 5000,
        };
        let profile = || ProfiledAccess {
            access: access(),
            shaping,
            traversal,
        };
        let old = build_shaped_access(
            &mut base.clone(),
            &alpha,
            &sockets,
            &[],
            access(),
            shaping,
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(old.roads.is_empty());
        let mut current = base.clone();
        let report =
            build_profiled_access(&mut current, &alpha, &sockets, &[], profile(), || false)
                .unwrap()
                .unwrap();
        assert_eq!(report.roads.len(), 1);
        assert!(report.unresolved.is_empty());
        assert_eq!(report.json()["engine"], COST_VERSION);
        assert!(current.topology.iter().all(|v| v & 2 != 0));
        for mode in 0..3 {
            let mut input = base.clone();
            let mut holes = alpha.clone();
            if mode == 0 {
                holes.fill(0);
            } else if mode == 1 {
                input.height.fill(encode(0.));
            } else {
                input.topology.fill(1024 | 2);
            }
            let before = input.clone();
            let rejected =
                build_profiled_access(&mut input, &holes, &sockets, &[], profile(), || false)
                    .unwrap()
                    .unwrap();
            assert!(rejected.roads.is_empty());
            assert_eq!(input.height, before.height);
            assert_eq!(input.splat, before.splat);
            assert_eq!(input.topology, before.topology);
        }
        let mut cancelled = base.clone();
        assert!(
            build_profiled_access(&mut cancelled, &alpha, &sockets, &[], profile(), || true)
                .unwrap()
                .is_none()
        );
        assert_eq!(cancelled.height, base.height);
    }
    #[test]
    fn native_ring_height_fit_bounds_all_cells_and_preserves_inputs_on_failure() {
        let layers = terrain();
        let original = [
            [-100., 20., -100.],
            [100., 25., -100.],
            [100., 20., 100.],
            [-100., 20., 100.],
            [-100., 20., -100.],
        ];
        let s = Settings {
            maximum_grade: 0.01,
            maximum_cut_fill: 8.,
            ..settings()
        };
        let mut fitted = original;
        let (work, changed) =
            fit_ring_heights(&layers, &mut fitted, s, 2_000_000, None, &|| false).unwrap();
        assert!(work > 0 && work <= 2_000_000 && changed > 0);
        assert_eq!(fitted.first(), fitted.last());
        for (p, old) in fitted.iter().zip(original) {
            assert_eq!([p[0], p[2]], [old[0], old[2]]);
        }
        assert!(fitted.windows(2).all(|w|(w[0][1]-w[1][1]).abs()<=s.maximum_grade*distance(w[0],w[1])+1e-5));
        let mut visits = 0;
        corridor_mode(
            &layers,
            &fitted,
            (layers.height_resolution, false, true),
            s,
            &mut visits,
            &|| false,
            |i, _, y, _, _| {
                assert!((decode(layers.height[i]) - y).abs() <= s.maximum_cut_fill);
                true
            },
        )
        .unwrap();
        for mode in 0..2 {
            let mut points = original;
            assert!(
                fit_ring_heights(
                    &layers,
                    &mut points,
                    s,
                    if mode == 0 { 1 } else { 2_000_000 },
                    None,
                    &|| mode == 1
                )
                .is_err()
            );
            assert_eq!(points, original);
        }
        assert_eq!(layers.height, terrain().height);
    }
    #[test]
    fn explicit_river_guide_crosses_low_terrain_without_opening_ocean_or_holes() {
        let mut base = terrain();
        let hr = base.height_resolution;
        let sr = base.surface_resolution;
        base.height.fill(encode(4.));
        for z in 0..hr {
            for x in 0..hr {
                let wx = (x as f32 / (hr - 1) as f32 - 0.5) * base.size as f32;
                if wx.abs() < 12. {
                    base.height[z * hr + x] = encode(-0.5);
                }
            }
        }
        for z in 0..sr {
            for x in 0..sr {
                let wx = ((x as f32 + 0.5) / sr as f32 - 0.5) * base.size as f32;
                if wx.abs() < 18. {
                    base.topology[z * sr + x] |= 49152;
                }
            }
        }
        let water_policy = crate::road_water::Policy {
            engine: crate::road_water::VERSION.into(),
            topology: 49152,
            clearance: 2.,
        };
        let levels = vec![-0.5; sr * sr];
        let water = crate::road_water::Guide {
            policy: &water_policy,
            heights: &levels,
        };
        let alpha = vec![255; sr * sr];
        assert!(!ground_clear(
            &base,
            &alpha,
            settings(),
            [0., 0.],
            14.,
            None
        ));
        assert!(ground_clear_with_water(
            &base,
            &alpha,
            settings(),
            [0., 0.],
            14.,
            None,
            Some(water)
        ));
        let mut ocean = base.clone();
        ocean.topology.fill(16);
        assert!(!ground_clear_with_water(
            &ocean,
            &alpha,
            settings(),
            [0., 0.],
            14.,
            None,
            Some(water)
        ));
        let mut holes = alpha.clone();
        holes.fill(0);
        assert!(!ground_clear_with_water(
            &base,
            &holes,
            settings(),
            [0., 0.],
            14.,
            None,
            Some(water)
        ));
        let policy = crate::road_ring::Policy {
            minimum_world_size: 1000,
            large_world_size: 5000,
            inset_divisor: 4,
            inward_step_cells: 1,
            maximum_attempts: 128,
            width: 12.,
            hierarchy: 0,
            network_sample_stride: 8,
        };
        let shape = ShapingPolicy {
            horizontal_iterations: 4,
            vertical_iterations: 16,
            resample_distance: 7.5,
            maximum_work: 2_000_000,
        };
        let joins = crate::road_ring_join::Policy {
            engine: crate::road_ring_join::REUSE_VERSION.into(),
            river_water: Some(water_policy.clone()),
            source_grid_spacing: 7.5,
            bridge_radius_cells: 15,
            trim_radius_cells: 1,
            directional_penalty: 10000,
            directional_radius_cells: 1,
            fit_heights_in_search: true,
            maximum_work: 8_000_000,
            maximum_points: 65536,
        };
        let run = |layers: &mut Layers, alpha: &[u8], cancel: fn() -> bool| {
            build_ring_with_river_costs(
                layers,
                alpha,
                &[],
                Settings {
                    protected_topology: 1024 | 16384 | 128,
                    ..settings()
                },
                policy,
                shape,
                (
                    TraversalPolicy {
                        traversable_topology: 2,
                        normal_cost: 101,
                        traversable_cost: 5000,
                    },
                    1024,
                ),
                &vec![101; GRID * GRID],
                Some(&joins),
                Some(water),
                cancel,
            )
        };
        let original = base.clone();
        let report = run(&mut base, &alpha, || false).unwrap().unwrap();
        assert_eq!(report.roads.len(), 1, "{:?}", report.ring);
        assert_eq!(report.ring.as_ref().unwrap().status, "found");
        let points = &report.roads[0].points;
        assert!(points.iter().any(|p| p[0] < -200.) && points.iter().any(|p| p[0] > 200.));
        for p in points {
            if let Some(minimum) = water.minimum(&original, p[0], p[2]) {
                assert!(p[1] >= minimum - 0.001);
            }
        }
        assert!(report.river_water.is_some());
        let mut protected = original.clone();
        protected.topology.iter_mut().for_each(|v| *v |= 128);
        let before = protected.height.clone();
        let denied = run(&mut protected, &alpha, || false).unwrap().unwrap();
        assert!(denied.roads.is_empty());
        assert_eq!(protected.height, before);
        let mut cancelled = original.clone();
        assert!(run(&mut cancelled, &alpha, || true).unwrap().is_none());
        assert_eq!(cancelled.height, original.height);
        assert!(
            base.height.iter().zip(&original.height).all(
                |(&a, &b)| (decode(a) - decode(b)).abs() <= settings().maximum_cut_fill + 0.031
            )
        );
    }

    #[test]
    fn global_ring_does_not_accept_cost_safe_candidates_with_folded_strips() {
        let base = terrain();
        let alpha = vec![255; base.surface_resolution.pow(2)];
        let mut costs = vec![0; GRID * GRID];
        for i in 64..=193 {
            for (x, z) in [(i, 64), (i, 193), (64, i), (193, i)] {
                costs[z * GRID + x] = 101;
            }
        }
        let policy = crate::road_ring::Policy {
            minimum_world_size: 1000,
            large_world_size: 5000,
            inset_divisor: 4,
            inward_step_cells: 1,
            maximum_attempts: 128,
            width: 12.,
            hierarchy: 0,
            network_sample_stride: 8,
        };
        let shape = ShapingPolicy {
            horizontal_iterations: 4,
            vertical_iterations: 16,
            resample_distance: 7.5,
            maximum_work: 2_000_000,
        };
        let mut output = base.clone();
        let ring = build_ring_with_global_earthwork(
            &mut output,
            &alpha,
            &[],
            settings(),
            policy,
            shape,
            None,
            Some(&costs),
            None,
            None,
            LaunchPolicy {
                engine: LaunchEngine::GlobalEarthwork,
                minimum_run_widths: 1.,
                sample_spacing: 1.,
                maximum_work: 2_000_000,
            },
            || false,
        )
        .unwrap()
        .unwrap();
        // This narrow fixture leaves the proposed 12 m strips folded at the
        // corners, even when a candidate's centreline stays in allowed cells.
        // Cost safety alone is not acceptance, nor is rejection proof that no
        // different route could ever be feasible here.
        assert!(ring.roads.is_empty(), "{:?}", ring.ring);
        assert_eq!(output.height, base.height);
        assert_eq!(
            ring.ring.as_ref().unwrap().shaping_fallback,
            Some("ring_strip_folded")
        );
        let mut old = base.clone();
        let legacy = build_ring_with_source_costs(
            &mut old,
            &alpha,
            &[],
            settings(),
            policy,
            shape,
            (
                TraversalPolicy {
                    traversable_topology: 2,
                    normal_cost: 101,
                    traversable_cost: 5000,
                },
                1024,
            ),
            &costs,
            None,
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(legacy.ring.as_ref().unwrap().smoothed);
        assert_eq!(
            source_shape_clear(&legacy.roads[0].points, &costs, 1000., &|| false),
            Some(false)
        );
        // The new gate must still admit ordinary, full-width rings. Paired
        // with the narrow fixture, this guards against rejecting every path.
        let mut broad = base.clone();
        let good = build_ring_with_global_earthwork(
            &mut broad,
            &alpha,
            &[],
            settings(),
            policy,
            shape,
            None,
            Some(&vec![101; GRID * GRID]),
            None,
            None,
            LaunchPolicy {
                engine: LaunchEngine::GlobalEarthwork,
                minimum_run_widths: 1.,
                sample_spacing: 1.,
                maximum_work: 2_000_000,
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(good.roads.len(), 1, "{:?}", good.ring);
        let path = crate::path_geometry::Path::from_controls(
            &good.roads[0].points,
            shape.maximum_work,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            crate::road_strip::check(
                &path,
                &vec![true; path.points().len()],
                settings().width * 0.5,
                |_| Ok::<_, String>(()),
                &|| false,
            )
            .unwrap(),
            Some(true)
        );
    }
    #[test]
    fn main_ring_composes_atomically_and_retains_explicit_loop_identity() {
        let base = terrain();
        let alpha = vec![255; base.surface_resolution * base.surface_resolution];
        let policy = crate::road_ring::Policy {
            minimum_world_size: 1000,
            large_world_size: 5000,
            inset_divisor: 4,
            inward_step_cells: 1,
            maximum_attempts: 128,
            width: 12.,
            hierarchy: 0,
            network_sample_stride: 8,
        };
        let shape = ShapingPolicy {
            horizontal_iterations: 4,
            vertical_iterations: 16,
            resample_distance: 7.5,
            maximum_work: 2_000_000,
        };
        let mut output = base.clone();
        let ring = build_ring(&mut output, &alpha, &[], settings(), policy, shape, || {
            false
        })
        .unwrap()
        .unwrap();
        assert_eq!(ring.ring.as_ref().unwrap().status, "found");
        assert!(ring.ring.as_ref().unwrap().smoothed);
        assert_eq!(ring.roads.len(), 1);
        assert_eq!(ring.roads[0].from, Destination::Loop);
        assert_eq!(ring.roads[0].to, Destination::Loop);
        assert_eq!(ring.roads[0].points.first(), ring.roads[0].points.last());
        let joined_policy = crate::road_ring_join::Policy {
            river_water: None,
            engine: crate::road_ring_join::VERSION.into(),
            source_grid_spacing: 7.5,
            bridge_radius_cells: 15,
            trim_radius_cells: 1,
            directional_penalty: 10000,
            directional_radius_cells: 1,
            fit_heights_in_search: true,
            maximum_work: 8_000_000,
            maximum_points: 65536,
        };
        let mut joined_output = base.clone();
        let joined = build_ring_with_source_costs(
            &mut joined_output,
            &alpha,
            &[],
            settings(),
            policy,
            shape,
            (
                TraversalPolicy {
                    traversable_topology: 2,
                    normal_cost: 101,
                    traversable_cost: 5000,
                },
                1024,
            ),
            &vec![101; GRID * GRID],
            Some(&joined_policy),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(joined.roads.len(), 1);
        assert_eq!(
            joined.ring.as_ref().unwrap().engine,
            crate::road_ring_join::VERSION
        );
        assert!(
            joined
                .ring
                .as_ref()
                .unwrap()
                .joins
                .as_ref()
                .unwrap()
                .bridges
                > 0
        );
        let mut blocked_output = base.clone();
        let denied = build_ring_with_source_costs(
            &mut blocked_output,
            &alpha,
            &[],
            settings(),
            policy,
            shape,
            (
                TraversalPolicy {
                    traversable_topology: 2,
                    normal_cost: 101,
                    traversable_cost: 5000,
                },
                1024,
            ),
            &vec![0; GRID * GRID],
            None,
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(denied.roads.is_empty());
        assert_eq!(blocked_output.height, base.height);
        assert_eq!(blocked_output.splat, base.splat);
        assert!(
            build_ring_with_source_costs(
                &mut blocked_output,
                &alpha,
                &[],
                settings(),
                policy,
                shape,
                (
                    TraversalPolicy {
                        traversable_topology: 2,
                        normal_cost: 101,
                        traversable_cost: 5000
                    },
                    1024
                ),
                &[0],
                None,
                || false
            )
            .is_err()
        );
        assert_ne!(output.height, base.height);
        assert_ne!(output.splat, base.splat);
        assert_eq!(
            ring.json()["roads"][0]["from"],
            serde_json::json!({"loop":true})
        );
        let cells = output.surface_resolution * output.surface_resolution;
        assert!((0..cells).all(|i| {
            (0..8)
                .map(|c| output.splat[c * cells + i] as u32)
                .sum::<u32>()
                == 255
        }));
        let mut replay = base.clone();
        let repeated = build_ring(&mut replay, &alpha, &[], settings(), policy, shape, || {
            false
        })
        .unwrap()
        .unwrap();
        assert_eq!(ring.json(), repeated.json());
        assert_eq!(output.height, replay.height);
        assert_eq!(output.splat, replay.splat);
        let priced = TraversalPolicy {
            traversable_topology: 2,
            normal_cost: 101,
            traversable_cost: 5000,
        };
        let mut cliff = base.clone();
        cliff.topology.fill(2);
        let protected = Settings {
            protected_topology: 1026,
            ..settings()
        };
        let legacy = build_ring(
            &mut cliff.clone(),
            &alpha,
            &[],
            protected,
            policy,
            shape,
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(legacy.roads.is_empty());
        let recovered = build_ring_with_recovery(
            &mut cliff,
            &alpha,
            &[],
            protected,
            policy,
            shape,
            Some((priced, 1024)),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            recovered.ring.as_ref().unwrap().engine,
            crate::road_ring::RECOVERY_VERSION
        );
        assert_eq!(recovered.roads.len(), 1);
        assert!(cliff.topology.iter().all(|t| t & 2 != 0));
        for mode in 0..3 {
            let mut blocked = base.clone();
            let mut mask = alpha.clone();
            if mode == 0 {
                mask.fill(0);
            }
            if mode == 1 {
                blocked.height.fill(encode(-5.));
            }
            if mode == 2 {
                blocked.topology.fill(1024);
            }
            let before = blocked.height.clone();
            let rejected = build_ring_with_recovery(
                &mut blocked,
                &mask,
                &[],
                protected,
                policy,
                shape,
                Some((priced, 1024)),
                || false,
            )
            .unwrap()
            .unwrap();
            assert!(rejected.roads.is_empty());
            assert_eq!(blocked.height, before);
        }
        assert!(
            build_ring_with_recovery(
                &mut cliff,
                &alpha,
                &[],
                protected,
                policy,
                shape,
                Some((priced, 2)),
                || false
            )
            .is_err()
        );
        let calls = std::cell::Cell::new(0);
        let mut cancelled = base.clone();
        assert!(
            build_ring(
                &mut cancelled,
                &alpha,
                &[],
                settings(),
                policy,
                shape,
                || {
                    calls.set(calls.get() + 1);
                    calls.get() > 300
                }
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(cancelled.height, base.height);
        assert_eq!(cancelled.splat, base.splat);
        assert_eq!(cancelled.topology, base.topology);
        let mut skipped = base.clone();
        let small = build_ring(
            &mut skipped,
            &alpha,
            &[],
            settings(),
            crate::road_ring::Policy {
                minimum_world_size: 4000,
                ..policy
            },
            shape,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(small.ring.unwrap().status, "skipped");
        assert!(small.roads.is_empty());
        assert_eq!(skipped.height, base.height);
        let mut blocked = base.clone();
        blocked.topology.fill(settings().protected_topology);
        let heights = blocked.height.clone();
        let paint = blocked.splat.clone();
        let rejected = build_ring(&mut blocked, &alpha, &[], settings(), policy, shape, || {
            false
        })
        .unwrap()
        .unwrap();
        assert_eq!(rejected.ring.unwrap().status, "unresolved");
        assert!(rejected.roads.is_empty());
        assert_eq!(blocked.height, heights);
        assert_eq!(blocked.splat, paint);
    }
    #[test]
    fn closed_shared_ground_fit_recovers_soft_targets_without_borrowing_earthwork() {
        let layers = terrain();
        let settings = Settings {
            maximum_cut_fill: 8.,
            ..settings()
        };
        let input = vec![
            [-200., 0., -200.],
            [200., 50., -200.],
            [200., 0., 200.],
            [-200., 50., 200.],
            [-200., 0., -200.],
        ];
        let mut legacy = input.clone();
        assert_eq!(
            fit_ring_heights(&layers, &mut legacy, settings, 2_000_000, None, &|| false),
            Err(crate::rail_grade_fit::Failure::Infeasible)
        );
        assert_eq!(legacy, input);
        let mut fitted = input.clone();
        fit_road_heights_on_ground(
            &layers,
            &mut fitted,
            settings,
            2_000_000,
            None,
            true,
            false,
            Some(&layers.height),
            &|| false,
        )
        .unwrap();
        assert_eq!(fitted.first(), fitted.last());
        assert!(
            fitted
                .iter()
                .zip(&input)
                .all(|(a, b)| a[0] == b[0] && a[2] == b[2])
        );
        let mut work = 0;
        assert!(
            corridor_mode(
                &layers,
                &fitted,
                (layers.height_resolution, false, true),
                settings,
                &mut work,
                &|| false,
                |i, _, height, _, _| {
                    (decode(layers.height[i]) - height).abs() <= settings.maximum_cut_fill
                }
            )
            .unwrap()
        );
        assert!(fitted.windows(2).all(|w| (w[0][1]-w[1][1]).abs()
            <= settings.maximum_grade * distance(w[0],w[1])));
        let mut cancelled = input.clone();
        assert_eq!(
            fit_road_heights_on_ground(
                &layers,
                &mut cancelled,
                settings,
                2_000_000,
                None,
                true,
                false,
                Some(&layers.height),
                &|| true
            ),
            Err(crate::rail_grade_fit::Failure::Cancelled)
        );
        assert_eq!(cancelled, input);
    }
    #[test]
    #[ignore = "requires owned, captured ring terrain; no game runtime is launched"]
    fn coupled_ring_replays_captured_linear_corridor() {
        let root = std::path::PathBuf::from(
            std::env::var_os("MAPPER_RING_COUPLED_FIXTURE").expect("capture directory"),
        );
        let meta: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("ring-ground-1873.json")).unwrap())
                .unwrap();
        let graph: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("ring-fit-1873-graph.json")).unwrap())
                .unwrap();
        let bytes = std::fs::read(root.join(meta["ground_file"].as_str().unwrap())).unwrap();
        let mut layers = terrain();
        layers.size = meta["size"].as_u64().unwrap() as u32;
        layers.height_resolution = meta["height_resolution"].as_u64().unwrap() as usize;
        layers.height = bytes
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        let settings = Settings {
            width: meta["settings"]["width"].as_f64().unwrap() as f32,
            shoulder: meta["settings"]["shoulder"].as_f64().unwrap() as f32,
            terrain_offset: meta["settings"]["terrain_offset"].as_f64().unwrap() as f32,
            maximum_grade: meta["settings"]["maximum_grade"].as_f64().unwrap() as f32,
            maximum_cut_fill: meta["settings"]["maximum_cut_fill"].as_f64().unwrap() as f32,
            ..settings()
        };
        let mut points: Vec<[f32; 3]> = serde_json::from_value(graph["points"].clone()).unwrap();
        let original = points.clone();
        let (used, changed) = fit_road_heights_on_ground(
            &layers,
            &mut points,
            settings,
            32_000_000,
            None,
            true,
            false,
            Some(&layers.height),
            &|| false,
        )
        .unwrap();
        assert!(used <= 32_000_000 && changed > 0);
        assert_eq!(points.first(), points.last());
        assert!(
            points
                .iter()
                .zip(original)
                .all(|(a, b)| a[0] == b[0] && a[2] == b[2])
        );
        assert!(points.windows(2).all(|p| (p[0][1] - p[1][1]).abs() <= settings.maximum_grade * distance(p[0], p[1])));
        let mut work = 0;
        assert!(
            corridor_mode(
                &layers,
                &points,
                (layers.height_resolution, false, true),
                settings,
                &mut work,
                &|| false,
                |i, _, height, _, _| (decode(layers.height[i]) - height).abs()
                    <= settings.maximum_cut_fill
            )
            .unwrap()
        );
        eprintln!("Captured coupled ring fit: work={used}, changed={changed}");
    }
    #[test]
    fn subdivided_ring_fits_local_hills_without_cutting_horizontal_corners() {
        let mut layers = terrain();
        let hr = layers.height_resolution;
        for z in 0..hr {
            for x in 0..hr {
                let wx = x as f32 / (hr - 1) as f32 * 1000. - 500.;
                layers.height[z * hr + x] = encode(20. + 20. * (1. - (wx / 200.).abs()).max(0.));
            }
        }
        let settings = Settings {
            maximum_cut_fill: 8.,
            ..settings()
        };
        let input = vec![
            [-200., 20., -200.],
            [200., 20., -200.],
            [200., 20., 200.],
            [-200., 20., 200.],
            [-200., 20., -200.],
        ];
        let mut sparse = input.clone();
        assert_eq!(
            fit_road_heights_on_ground(
                &layers,
                &mut sparse,
                settings,
                MAX_WORK,
                None,
                true,
                false,
                Some(&layers.height),
                &|| false
            ),
            Err(crate::rail_grade_fit::Failure::Infeasible)
        );
        let mut dense = subdivide_ring(&input, 7.5, &|| false).unwrap().unwrap();
        for corner in &input {
            assert!(dense.contains(corner));
        }
        assert_eq!(dense.first(), dense.last());
        assert!(dense.windows(2).all(|w| distance(w[0], w[1]) <= 7.501));
        let xz = dense.iter().map(|p| [p[0], p[2]]).collect::<Vec<_>>();
        fit_road_heights_on_ground(
            &layers,
            &mut dense,
            settings,
            MAX_WORK,
            None,
            true,
            false,
            Some(&layers.height),
            &|| false,
        )
        .unwrap();
        assert_eq!(xz, dense.iter().map(|p| [p[0], p[2]]).collect::<Vec<_>>());
        let mut work = 0;
        assert!(
            corridor_mode(
                &layers,
                &dense,
                (hr, false, true),
                settings,
                &mut work,
                &|| false,
                |i, _, height, _, _| (decode(layers.height[i]) - height).abs() <= 8.
            )
            .unwrap()
        );
        assert!(subdivide_ring(&input, 7.5, &|| true).unwrap().is_none());
        assert!(subdivide_ring(&input, f32::NAN, &|| false).is_err());
    }
    #[test]
    fn capsule_row_scan_preserves_exact_cells_values_order_and_stopping() {
        let mut layers = terrain();
        let mut total_old = 0;
        let mut total_new = 0;
        for size in [1000, 6000] {
            layers.size = size;
            for (n, centers) in [(1025, false), (512, true), (4097, false), (2048, true)] {
                for origin in [[0_f32, 0_f32], [0.48, 0.45], [-0.48, -0.45]] {
                    for angle in [
                        0_f32, 0.0001, 17., 45., 89.9999, 90., 135., 179.9999, 180., 241., 270.,
                        359.9999,
                    ] {
                        let (sin, cos) = angle.to_radians().sin_cos();
                        for run in [0.01, 5., 30., 240.] {
                            for reverse in [false, true] {
                                let mut points = [
                                    [size as f32 * origin[0], 20., size as f32 * origin[1]],
                                    [
                                        size as f32 * origin[0] + cos * run,
                                        27.,
                                        size as f32 * origin[1] + sin * run,
                                    ],
                                ];
                                if reverse {
                                    points.reverse();
                                }
                                let mut old = Vec::new();
                                let mut new = Vec::new();
                                let mut old_work = 0;
                                let mut new_work = 0;
                                let record =
                                    |out: &mut Vec<_>, i, d: f32, y: f32, x: f32, z: f32| {
                                        out.push((
                                            i,
                                            [d.to_bits(), y.to_bits(), x.to_bits(), z.to_bits()],
                                        ));
                                        true
                                    };
                                assert!(
                                    corridor(
                                        &layers,
                                        &points,
                                        (n, centers),
                                        settings(),
                                        &mut old_work,
                                        &|| false,
                                        |i, d, y, x, z| record(&mut old, i, d, y, x, z)
                                    )
                                    .unwrap()
                                );
                                assert!(
                                    corridor_rows(
                                        &layers,
                                        &points,
                                        (n, centers),
                                        settings(),
                                        &mut new_work,
                                        &|| false,
                                        true,
                                        |i, d, y, x, z| record(&mut new, i, d, y, x, z)
                                    )
                                    .unwrap()
                                );
                                assert_eq!(
                                    old, new,
                                    "size={size}, centers={centers}, angle={angle}, run={run}, reverse={reverse}"
                                );
                                assert!(new_work <= old_work);
                                total_old += old_work;
                                total_new += new_work;
                            }
                        }
                    }
                }
            }
        }
        assert!(
            total_new * 100 < total_old * 90,
            "old={total_old}, new={total_new}"
        );
        let points = [[-100., 20., -100.], [100., 25., 100.]];
        let mut work = 0;
        assert!(
            !corridor_rows(
                &layers,
                &points,
                (layers.height_resolution, false),
                settings(),
                &mut work,
                &|| true,
                true,
                |_, _, _, _, _| panic!("Cancelled scan visited a cell")
            )
            .unwrap()
        );
        assert!(
            !corridor_rows(
                &layers,
                &points,
                (layers.height_resolution, false),
                settings(),
                &mut work,
                &|| false,
                true,
                |_, _, _, _, _| false
            )
            .unwrap()
        );
        let mut work = MAX_WORK;
        assert!(
            corridor_rows(
                &layers,
                &points,
                (layers.height_resolution, false),
                settings(),
                &mut work,
                &|| false,
                true,
                |_, _, _, _, _| true
            )
            .is_err()
        );
        eprintln!("Exact row scan: {total_old} -> {total_new} bounding-cell checks");
    }
    fn terrain() -> Layers {
        let (hr, sr) = crate::native_layers::dimensions(1000).unwrap();
        let mut splat = vec![0; sr * sr * 8];
        splat[4 * sr * sr..5 * sr * sr].fill(255);
        Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![encode(20.); hr * hr],
            biome: vec![0; sr * sr * 5],
            splat,
            topology: vec![16; sr * sr],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        }
    }
    fn settings() -> Settings {
        Settings {
            seed: 123,
            width: 10.,
            shoulder: 8.,
            maximum_grade: 0.2,
            maximum_cut_fill: 4.,
            terrain_offset: -0.125,
            road_topology: 2048,
            roadside_topology: 4096,
            protected_topology: 1024,
            splat_channel: 7,
        }
    }
    fn socket(monument: u64, x: f32, z: f32) -> Socket {
        Socket {
            monument,
            connector: monument as i64,
            position: [x, decode(encode(20.)), z],
        }
    }
    fn paved_network(layers: &mut Layers, alpha: &[u8]) -> Report {
        build(
            layers,
            alpha,
            &[socket(1, -300., 0.), socket(2, 300., 0.)],
            &[],
            settings(),
            || false,
        )
        .unwrap()
        .unwrap()
    }
    fn dirt_settings() -> Settings {
        Settings {
            width: 4.,
            splat_channel: 0,
            ..settings()
        }
    }
    fn entrance_policy() -> EntrancePolicy {
        EntrancePolicy {
            minimum_height: 1.,
            owned_topology: 1024,
            maximum_exit_distance: 160.,
            maximum_exit_nodes: 4096,
        }
    }
    #[test]
    fn paved_river_access_fits_clearance_preserves_endpoints_and_keeps_denials_atomic() {
        let mut base = terrain();
        let hr = base.height_resolution;
        let sr = base.surface_resolution;
        base.height.fill(encode(2.5));
        for z in 0..hr {
            for x in 0..hr {
                let wx = (x as f32 / (hr - 1) as f32 - 0.5) * base.size as f32;
                if wx.abs() < 25. {
                    base.height[z * hr + x] = encode(-1.);
                }
            }
        }
        for z in 0..sr {
            for x in 0..sr {
                let wx = ((x as f32 + 0.5) / sr as f32 - 0.5) * base.size as f32;
                if wx.abs() < 40. {
                    base.topology[z * sr + x] |= 16384;
                }
            }
        }
        let alpha = vec![255; sr * sr];
        let costs = vec![101; GRID * GRID];
        let levels = vec![0.4; sr * sr];
        let policy = crate::road_water::Policy {
            engine: crate::road_water::VERSION.into(),
            topology: 49152,
            clearance: 2.,
        };
        let water = crate::road_water::Guide {
            policy: &policy,
            heights: &levels,
        };
        let sockets = [socket(1, -200., 0.), socket(2, 200., 0.)];
        let profile = || ProfiledAccess {
            access: Access {
                settings: Settings {
                    protected_topology: 1024 | 128 | 16384,
                    ..settings()
                },
                policy: entrance_policy(),
                owner_footprints: &[],
                network: None,
            },
            shaping: shaping_policy(),
            traversal: TraversalPolicy {
                traversable_topology: 2,
                normal_cost: 101,
                traversable_cost: 5000,
            },
        };
        let mut legacy = base.clone();
        let old = build_constrained_source_access(
            &mut legacy,
            &alpha,
            &sockets,
            &[],
            profile(),
            &costs,
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(old.roads.is_empty());
        assert_eq!(legacy.height, base.height);
        let mut generated = base.clone();
        let report = build_river_source_access(
            &mut generated,
            &alpha,
            &sockets,
            &[],
            profile(),
            &costs,
            true,
            water,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(report.roads.len(), 1, "{:?}", report.rejection_reasons);
        let points = &report.roads[0].points;
        let height = decode(encode(2.5));
        assert_eq!(points[0], [-200., height, 0.]);
        assert_eq!(*points.last().unwrap(), [200., height, 0.]);
        assert!(
            points.windows(2).all(|w| (w[0][1] - w[1][1]).abs()
                <= settings().maximum_grade * distance(w[0], w[1]) + 1e-5)
        );
        assert!(
            points
                .iter()
                .filter_map(|p| water.minimum(&base, p[0], p[2]).map(|h| (p, h)))
                .all(|(p, h)| p[1] >= h - 0.001)
        );
        assert!(report.access_river_water.is_some());
        let mut replay = base.clone();
        let repeated = build_river_source_access(
            &mut replay,
            &alpha,
            &sockets,
            &[],
            profile(),
            &costs,
            true,
            water,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(report.json(), repeated.json());
        assert_eq!(generated.height, replay.height);
        for variant in 0..4 {
            let mut denied = base.clone();
            let mut mask = alpha.clone();
            let mut prices = costs.clone();
            if variant == 0 {
                for z in 0..sr {
                    for x in 0..sr {
                        if denied.topology[z * sr + x] & 16384 != 0 {
                            mask[z * sr + x] = 0;
                        }
                    }
                }
            }
            if variant == 1 {
                for flags in &mut denied.topology {
                    if *flags & 16384 != 0 {
                        *flags |= 128;
                    }
                }
            }
            if variant == 2 {
                for h in &mut denied.height {
                    if decode(*h) < 0. {
                        *h = encode(-20.);
                    }
                }
            }
            if variant == 3 {
                for z in 0..GRID {
                    for x in 0..GRID {
                        if x.abs_diff(GRID / 2) < 16 {
                            prices[z * GRID + x] = 0;
                        }
                    }
                }
            }
            let before = denied.clone();
            let result = build_river_source_access(
                &mut denied,
                &mask,
                &sockets,
                &[],
                profile(),
                &prices,
                true,
                water,
                || false,
            )
            .unwrap()
            .unwrap();
            assert!(result.roads.is_empty(), "denial {variant}");
            assert_eq!(denied.height, before.height);
            assert_eq!(denied.splat, before.splat);
            assert_eq!(denied.topology, before.topology);
        }
        let calls = std::cell::Cell::new(0);
        let mut cancelled = base.clone();
        assert!(
            build_river_source_access(
                &mut cancelled,
                &alpha,
                &sockets,
                &[],
                profile(),
                &costs,
                true,
                water,
                || {
                    calls.set(calls.get() + 1);
                    calls.get() > 1500
                }
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(cancelled.height, base.height);
        assert_eq!(cancelled.splat, base.splat);
        assert_eq!(cancelled.topology, base.topology);
    }
    #[test]
    fn source_access_costs_block_components_price_routes_and_cancel_atomically() {
        let base = terrain();
        let alpha = vec![255; base.surface_resolution.pow(2)];
        let sockets = [socket(1, -300., 0.), socket(2, 300., 0.)];
        let profile = || ProfiledAccess {
            access: Access {
                settings: settings(),
                policy: entrance_policy(),
                owner_footprints: &[],
                network: None,
            },
            shaping: shaping_policy(),
            traversal: TraversalPolicy {
                traversable_topology: 2,
                normal_cost: 101,
                traversable_cost: 5000,
            },
        };
        let mut costs = vec![101; GRID * GRID];
        let baseline = build_source_cost_access(
            &mut base.clone(),
            &alpha,
            &sockets,
            &[],
            profile(),
            &costs,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(baseline.roads.len(), 1);
        for z in 118..=138 {
            for x in 80..=176 {
                costs[z * GRID + x] = 5000;
            }
        }
        let priced = build_source_cost_access(
            &mut base.clone(),
            &alpha,
            &sockets,
            &[],
            profile(),
            &costs,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(priced.roads.len(), 1);
        assert!(priced.roads[0].points.iter().any(|p| p[2].abs() > 40.));
        for z in 0..GRID {
            costs[z * GRID + 128] = 0;
        }
        let mut blocked = base.clone();
        let rejected = build_source_cost_access(
            &mut blocked,
            &alpha,
            &sockets,
            &[],
            profile(),
            &costs,
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(rejected.roads.is_empty());
        assert!(!rejected.unresolved.is_empty());
        assert_eq!(blocked.height, base.height);
        assert_eq!(blocked.splat, base.splat);
        assert_eq!(blocked.topology, base.topology);
        let mut cancelled = base.clone();
        let calls = std::cell::Cell::new(0);
        assert!(
            build_source_cost_access(
                &mut cancelled,
                &alpha,
                &sockets,
                &[],
                profile(),
                &costs,
                || {
                    calls.set(calls.get() + 1);
                    calls.get() > 30
                }
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(cancelled.height, base.height);
        assert_eq!(cancelled.splat, base.splat);
        assert!(
            build_source_cost_access(
                &mut cancelled,
                &alpha,
                &sockets,
                &[],
                profile(),
                &costs[..100],
                || false
            )
            .is_err()
        );
        costs[0] = 1_000_001;
        assert!(
            build_source_cost_access(
                &mut cancelled,
                &alpha,
                &sockets,
                &[],
                profile(),
                &costs,
                || false
            )
            .is_err()
        );
    }
    #[test]
    fn constrained_source_recovery_keeps_narrow_turns_inside_the_cost_corridor() {
        let base = terrain();
        let alpha = vec![255; base.surface_resolution.pow(2)];
        let mut costs = vec![0; GRID * GRID];
        for x in 40..=128 {
            costs[80 * GRID + x] = 101;
        }
        for z in 80..=176 {
            costs[z * GRID + 128] = 101;
        }
        for x in 128..=216 {
            costs[176 * GRID + x] = 101;
        }
        let world = |v: usize| v as f32 / 256. * 1000. - 500.;
        let sockets = [
            socket(1, world(40), world(80)),
            socket(2, world(216), world(176)),
        ];
        let profile = || ProfiledAccess {
            access: Access {
                settings: settings(),
                policy: entrance_policy(),
                owner_footprints: &[],
                network: None,
            },
            shaping: shaping_policy(),
            traversal: TraversalPolicy {
                traversable_topology: 2,
                normal_cost: 101,
                traversable_cost: 5000,
            },
        };
        let old = build_source_cost_access(
            &mut base.clone(),
            &alpha,
            &sockets,
            &[],
            profile(),
            &costs,
            || false,
        )
        .unwrap()
        .unwrap();
        let refined = build_constrained_source_access(
            &mut base.clone(),
            &alpha,
            &sockets,
            &[],
            profile(),
            &costs,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(old.roads.len(), 1);
        assert_eq!(refined.roads.len(), 1);
        let audit = refined.shaping.constrained.as_ref().unwrap();
        assert_eq!(audit.paths, 1);
        assert_eq!(audit.attempts, 1);
        assert!(audit.accepted_moves > 0);
        assert!(refined.shaping.accepted > old.shaping.accepted);
        assert_eq!(
            source_shape_clear(&refined.roads[0].points, &costs, 1000., &|| false),
            Some(true)
        );
        assert_eq!(refined.roads[0].points.first(), old.roads[0].points.first());
        assert_eq!(refined.roads[0].points.last(), old.roads[0].points.last());
    }
    #[test]
    fn source_shape_checks_exact_cells_and_blocked_corner_touches() {
        let mut costs = vec![101; GRID * GRID];
        costs[128 * GRID + 128] = 0;
        let point = |x: f32, z: f32| [(x / 256. - 0.5) * 1000., 20., (z / 256. - 0.5) * 1000.];
        assert_eq!(
            source_shape_clear(
                &[point(127., 128.), point(129., 128.)],
                &costs,
                1000.,
                &|| false
            ),
            Some(false)
        );
        assert_eq!(
            source_shape_clear(
                &[point(127., 128.), point(128., 129.)],
                &costs,
                1000.,
                &|| false
            ),
            Some(false)
        );
        assert_eq!(
            source_shape_clear(
                &[point(127., 127.), point(129., 127.)],
                &costs,
                1000.,
                &|| false
            ),
            Some(true)
        );
        assert_eq!(
            source_shape_clear(
                &[point(127., 127.), point(129., 127.)],
                &costs,
                1000.,
                &|| true
            ),
            None
        );
    }
    #[test]
    fn checked_launch_avoids_short_seed_hops_and_respects_full_width_holes_and_other_owners() {
        let layers = terrain();
        let n = layers.surface_resolution;
        let alpha = vec![255; n * n];
        let heights = vec![decode(encode(20.)); GRID * GRID];
        let blocked = vec![false; GRID * GRID];
        let node = Node {
            destination: Destination::Monument {
                monument: 1,
                connector: 1,
            },
            group: 0,
            position: [1., decode(encode(20.)), 0.],
        };
        let launch = LaunchPolicy {
            engine: LaunchEngine::Checked,
            minimum_run_widths: 1.,
            sample_spacing: 1.,
            maximum_work: 2_000_000,
        };
        let run = |alpha: &[u8], obstacles: &[Obstacle], policy, cancel| {
            entrance_exit_goal_with_launch(
                &layers,
                alpha,
                node,
                obstacles,
                &[],
                settings(),
                entrance_policy(),
                &heights,
                &blocked,
                None,
                None,
                policy,
                &|| cancel,
            )
        };
        let old = run(&alpha, &[], None, false).unwrap().unwrap();
        assert!(distance(old.points[0], old.points[1]) < 2.);
        let new = run(&alpha, &[], Some(launch), false).unwrap().unwrap();
        assert_eq!(new.points[0], node.position);
        assert!(distance(new.points[0], new.points[1]) >= settings().width);
        assert_eq!(
            new.points,
            run(&alpha, &[], Some(launch), false)
                .unwrap()
                .unwrap()
                .points
        );
        let mut hole = alpha.clone();
        hole[n / 2 * n + n / 2] = 0;
        assert!(run(&hole, &[], Some(launch), false).unwrap().is_none());
        let obstacle = Obstacle {
            monument: 2,
            center: [1., 0.],
            extent: [2., 2.],
            yaw: 0.,
        };
        assert!(
            run(&alpha, &[obstacle], Some(launch), false)
                .unwrap()
                .is_none()
        );
        assert!(
            run(
                &alpha,
                &[],
                Some(LaunchPolicy {
                    maximum_work: 1024,
                    ..launch
                }),
                false
            )
            .unwrap_err()
            .contains("budget")
        );
        assert!(run(&alpha, &[], Some(launch), true).unwrap().is_none());
        assert!(layers.height.iter().all(|&h| h == encode(20.)));
    }
    #[test]
    fn checked_launch_turns_reject_reversals_and_tight_width_transitions() {
        assert!(!launch_turn_clear(
            [0., 0., 0.],
            [1., 0., 0.],
            [1., 0., 10.],
            5.
        ));
        assert!(!launch_turn_clear(
            [0., 0., 0.],
            [10., 0., 0.],
            [0., 0., 0.],
            5.
        ));
        assert!(launch_turn_clear(
            [0., 0., 0.],
            [10., 0., 0.],
            [10., 0., 10.],
            5.
        ));
        assert!(launch_turn_clear(
            [0., 0., 0.],
            [10., 0., 0.],
            [20., 0., 0.],
            5.
        ));
    }
    #[test]
    fn connected_exit_continues_past_a_clear_but_disconnected_pocket() {
        let mut layers = terrain();
        let n = layers.surface_resolution;
        for z in 0..n {
            for x in 0..n {
                let wx =
                    (x as f32 + 0.5) / n as f32 * layers.size as f32 - layers.size as f32 * 0.5;
                if wx.abs() < 10. {
                    layers.topology[z * n + x] = 1024;
                }
            }
        }
        let alpha = vec![255; n * n];
        let original = layers.height.clone();
        let owner = Obstacle {
            monument: 1,
            center: [0., 0.],
            extent: [60., 60.],
            yaw: 0.,
        };
        let owners = [owner];
        let node = Node {
            destination: Destination::Monument {
                monument: 1,
                connector: 1,
            },
            group: 0,
            position: [-10., 20., 0.],
        };
        let raster = route_raster(
            &layers,
            &alpha,
            &[],
            settings(),
            true,
            true,
            None,
            None,
            &|| false,
        )
        .unwrap()
        .unwrap();
        let labels = raster.components.as_ref().unwrap();
        let desired = labels[(GRID / 2) * GRID + 220];
        assert!(desired > 0);
        let old = entrance_exit(
            &layers,
            &alpha,
            node,
            &[],
            &owners,
            settings(),
            entrance_policy(),
            &raster.heights,
            &raster.blocked,
            &|| false,
        )
        .unwrap()
        .unwrap();
        assert_ne!(labels[old.cell[1] * GRID + old.cell[0]], desired);
        let connected = entrance_exit_goal(
            &layers,
            &alpha,
            node,
            &[],
            &owners,
            settings(),
            entrance_policy(),
            &raster.heights,
            &raster.blocked,
            Some((labels, desired)),
            None,
            &|| false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            labels[connected.cell[1] * GRID + connected.cell[0]],
            desired
        );
        assert_eq!(connected.points[0], node.position);
        let launched = entrance_exit_goal_with_launch(
            &layers,
            &alpha,
            node,
            &[],
            &owners,
            settings(),
            entrance_policy(),
            &raster.heights,
            &raster.blocked,
            Some((labels, desired)),
            None,
            Some(LaunchPolicy {
                engine: LaunchEngine::Checked,
                minimum_run_widths: 1.,
                sample_spacing: 1.,
                maximum_work: 2_000_000,
            }),
            &|| false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(labels[launched.cell[1] * GRID + launched.cell[0]], desired);
        assert_eq!(launched.points[0], node.position);
        assert!(distance(launched.points[0], launched.points[1]) >= settings().width);
        assert_eq!(layers.height, original);
        assert!(
            entrance_exit_goal(
                &layers,
                &alpha,
                node,
                &[],
                &owners,
                settings(),
                entrance_policy(),
                &raster.heights,
                &raster.blocked,
                Some((labels, desired)),
                None,
                &|| true
            )
            .unwrap()
            .is_none()
        );
    }
    fn shaping_policy() -> ShapingPolicy {
        ShapingPolicy {
            horizontal_iterations: 4,
            vertical_iterations: 16,
            resample_distance: 7.5,
            maximum_work: 2_000_000,
        }
    }
    #[test]
    fn nearest_corridor_projection_is_unique_and_invariant_to_straight_subdivision() {
        let layers = terrain();
        let project = |points: &[[f32; 3]]| {
            let mut samples = std::collections::BTreeMap::new();
            assert!(
                corridor_mode(
                    &layers,
                    points,
                    (layers.surface_resolution, true, true),
                    settings(),
                    &mut 0,
                    &|| false,
                    |i, d, y, _, _| {
                        assert!(
                            samples.insert(i, (d, y)).is_none(),
                            "a cell must be visited only once"
                        );
                        true
                    }
                )
                .unwrap()
            );
            samples
        };
        let long = project(&[[-200., 20., 0.], [200., 20., 0.]]);
        let divided = project(&[
            [-200., 20., 0.],
            [-100., 20., 0.],
            [0., 20., 0.],
            [100., 20., 0.],
            [200., 20., 0.],
        ]);
        assert_eq!(long, divided);
        assert!(!long.is_empty());
    }
    #[test]
    fn shaped_routes_reduce_grid_turns_and_keep_exact_entrance_chains() {
        let base = terrain();
        let alpha = vec![255; base.surface_resolution.pow(2)];
        let sockets = [socket(1, -300., -120.), socket(2, 300., 120.)];
        let access = || Access {
            settings: settings(),
            policy: entrance_policy(),
            owner_footprints: &[],
            network: None,
        };
        let raw = build_access(&mut base.clone(), &alpha, &sockets, &[], access(), || false)
            .unwrap()
            .unwrap();
        let mut output = base.clone();
        let shaped = build_shaped_access(
            &mut output,
            &alpha,
            &sockets,
            &[],
            access(),
            shaping_policy(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(shaped.shaping.accepted, 1, "{}", shaped.json());
        assert_eq!(shaped.json()["engine"], SHAPED_VERSION);
        let turn = |p: &[[f32; 3]]| {
            p.windows(3)
                .map(|w| {
                    let a = [w[1][0] - w[0][0], w[1][2] - w[0][2]];
                    let b = [w[2][0] - w[1][0], w[2][2] - w[1][2]];
                    let d = a[0].hypot(a[1]) * b[0].hypot(b[1]);
                    if d > 1e-8 {
                        ((a[0] * b[0] + a[1] * b[1]) / d).clamp(-1., 1.).acos()
                    } else {
                        0.
                    }
                })
                .sum::<f32>()
        };
        assert!(turn(&shaped.roads[0].points) < turn(&raw.roads[0].points) * 0.8);
        assert_eq!(shaped.roads[0].points.first(), raw.roads[0].points.first());
        assert_eq!(shaped.roads[0].points.last(), raw.roads[0].points.last());
        assert_eq!(
            serde_json::to_value(&raw.entrance_joins).unwrap(),
            serde_json::to_value(&shaped.entrance_joins).unwrap()
        );
        let mut replay = base;
        let repeated = build_shaped_access(
            &mut replay,
            &alpha,
            &sockets,
            &[],
            access(),
            shaping_policy(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(shaped.json(), repeated.json());
        assert_eq!(output.height, replay.height);
        assert_eq!(output.splat, replay.splat);
    }
    #[test]
    fn shaping_budget_retains_original_route_geometry_and_valid_surface_weights() {
        let base = terrain();
        let alpha = vec![255; base.surface_resolution.pow(2)];
        let sockets = [socket(1, -300., -120.), socket(2, 300., 120.)];
        let access = || Access {
            settings: settings(),
            policy: entrance_policy(),
            owner_footprints: &[],
            network: None,
        };
        let mut raw_layers = base.clone();
        let raw = build_access(&mut raw_layers, &alpha, &sockets, &[], access(), || false)
            .unwrap()
            .unwrap();
        let mut policy = shaping_policy();
        policy.maximum_work = 1024;
        let mut output = base;
        let shaped =
            build_shaped_access(&mut output, &alpha, &sockets, &[], access(), policy, || {
                false
            })
            .unwrap()
            .unwrap();
        assert_eq!(shaped.shaping.budget_fallbacks, 1);
        assert_eq!(shaped.shaping.accepted, 0);
        assert_eq!(shaped.json()["roads"], raw.json()["roads"]);
        assert_eq!(shaped.rejected, 0);
        assert!(!shaped.budget_exhausted);
        let cells = output.surface_resolution.pow(2);
        for i in 0..cells {
            assert_eq!(
                (0..8)
                    .map(|c| output.splat[c * cells + i] as u16)
                    .sum::<u16>(),
                255
            );
        }
    }
    #[test]
    fn late_shaping_cancellation_rolls_back_the_complete_terrain_revision() {
        let (base, alpha, sockets, owners) = owned_terrain();
        let access = || Access {
            settings: settings(),
            policy: entrance_policy(),
            owner_footprints: &owners,
            network: None,
        };
        let ticks = std::cell::Cell::new(0);
        let mut completed = base.clone();
        let report = build_shaped_access(
            &mut completed,
            &alpha,
            &sockets,
            &owners,
            access(),
            shaping_policy(),
            || {
                ticks.set(ticks.get() + 1);
                false
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(report.shaping.accepted, 1);
        assert_ne!(completed.height, base.height);
        let limit = ticks.get() - 2;
        ticks.set(0);
        let mut output = base.clone();
        assert!(
            build_shaped_access(
                &mut output,
                &alpha,
                &sockets,
                &owners,
                access(),
                shaping_policy(),
                || {
                    let tick = ticks.get();
                    ticks.set(tick + 1);
                    tick >= limit
                }
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(output.height, base.height);
        assert_eq!(output.splat, base.splat);
        assert_eq!(output.topology, base.topology);
    }
    #[test]
    fn shaped_trail_keeps_the_exact_existing_paved_junction() {
        let mut layers = terrain();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let paved = paved_network(&mut layers, &alpha);
        let report = build_shaped_access(
            &mut layers,
            &alpha,
            &[socket(3, 100., 200.)],
            &[],
            Access {
                settings: dirt_settings(),
                policy: entrance_policy(),
                owner_footprints: &[],
                network: Some((&paved.roads, 8)),
            },
            shaping_policy(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(report.roads.len(), 1);
        let Destination::Network { road, point } = report.roads[0].to else {
            panic!("missing paved junction")
        };
        assert_eq!(
            report.roads[0].points.last(),
            Some(&paved.roads[road].points[point])
        );
        assert!(report.unresolved.is_empty());
    }
    fn owned_terrain() -> (Layers, Vec<u8>, Vec<Socket>, Vec<Obstacle>) {
        let mut l = terrain();
        let n = l.surface_resolution;
        for z in 0..n {
            for x in 0..n {
                let wx = (x as f32 / n as f32 - 0.5) * 1000.;
                let wz = (z as f32 / n as f32 - 0.5) * 1000.;
                if (wx + 200.).abs() < 50. && wz.abs() < 50.
                    || (wx - 200.).abs() < 50. && wz.abs() < 50.
                {
                    l.topology[z * n + x] |= 1024;
                }
            }
        }
        let mut sockets = vec![socket(1, -200., 0.), socket(2, 200., 0.)];
        for s in &mut sockets {
            s.position[1] = -1.;
        }
        let owners = vec![
            Obstacle {
                monument: 1,
                center: [-200., 0.],
                extent: [60., 60.],
                yaw: 0.,
            },
            Obstacle {
                monument: 2,
                center: [200., 0.],
                extent: [60., 60.],
                yaw: 0.,
            },
        ];
        (l, vec![255; n * n], sockets, owners)
    }
    #[test]
    fn terrain_height_entrances_exit_owned_ground_without_editing_authored_surfaces() {
        let (base, alpha, sockets, owners) = owned_terrain();
        let mut output = base.clone();
        let legacy = build(&mut output, &alpha, &sockets, &owners, settings(), || false)
            .unwrap()
            .unwrap();
        assert!(legacy.roads.is_empty());
        let mut output = base.clone();
        let report = build_access(
            &mut output,
            &alpha,
            &sockets,
            &owners,
            Access {
                settings: settings(),
                policy: entrance_policy(),
                owner_footprints: &owners,
                network: None,
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(report.roads.len(), 1);
        assert!(report.unresolved.is_empty());
        assert_eq!(report.entrance_joins.len(), 2);
        assert_eq!(report.json()["engine"], ACCESS_VERSION);
        for join in &report.entrance_joins {
            assert!(join.points.len() > 5);
            assert_eq!(join.points[0][1], decode(encode(20.)));
        }
        let cells = base.surface_resolution.pow(2);
        for i in 0..cells {
            if base.topology[i] & 1024 != 0 {
                for c in 0..8 {
                    assert_eq!(base.splat[c * cells + i], output.splat[c * cells + i]);
                }
            }
        }
        let hr = base.height_resolution;
        for z in 0..hr {
            for x in 0..hr {
                let wx = (x as f32 / (hr - 1) as f32 - 0.5) * 1000.;
                let wz = (z as f32 / (hr - 1) as f32 - 0.5) * 1000.;
                if (wx + 200.).abs() < 45. && wz.abs() < 45. {
                    assert_eq!(base.height[z * hr + x], output.height[z * hr + x]);
                }
            }
        }
        let mut replay = base.clone();
        let second = build_access(
            &mut replay,
            &alpha,
            &sockets,
            &owners,
            Access {
                settings: settings(),
                policy: entrance_policy(),
                owner_footprints: &owners,
                network: None,
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(report.json(), second.json());
        assert_eq!(output.height, replay.height);
    }
    #[test]
    fn source_declared_building_ground_at_owned_entrances_is_preserved() {
        let (mut base, alpha, sockets, owners) = owned_terrain();
        for value in &mut base.topology {
            if *value & 1024 != 0 {
                *value |= 2097152;
            }
        }
        let mut config = settings();
        config.protected_topology |= 2097152;
        let mut policy = entrance_policy();
        policy.owned_topology |= 2097152;
        let mut output = base.clone();
        let report = build_access(
            &mut output,
            &alpha,
            &sockets,
            &owners,
            Access {
                settings: config,
                policy,
                owner_footprints: &owners,
                network: None,
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(report.roads.len(), 1);
        let cells = base.surface_resolution.pow(2);
        for i in 0..cells {
            if base.topology[i] & 2097152 != 0 {
                assert_eq!(base.topology[i], output.topology[i]);
                for c in 0..8 {
                    assert_eq!(base.splat[c * cells + i], output.splat[c * cells + i]);
                }
            }
        }
    }
    #[test]
    fn entrance_cancellation_after_corridor_work_keeps_caller_layers_unchanged() {
        let (base, alpha, sockets, owners) = owned_terrain();
        let ticks = std::cell::Cell::new(0);
        let access = || Access {
            settings: settings(),
            policy: entrance_policy(),
            owner_footprints: &owners,
            network: None,
        };
        let mut completed = base.clone();
        let report = build_access(&mut completed, &alpha, &sockets, &owners, access(), || {
            ticks.set(ticks.get() + 1);
            false
        })
        .unwrap()
        .unwrap();
        assert_eq!(report.roads.len(), 1);
        assert_ne!(completed.height, base.height);
        let limit = ticks.get() - 2;
        ticks.set(0);
        let mut output = base.clone();
        assert!(
            build_access(&mut output, &alpha, &sockets, &owners, access(), || {
                let tick = ticks.get();
                ticks.set(tick + 1);
                tick >= limit
            })
            .unwrap()
            .is_none()
        );
        assert_eq!(output.height, base.height);
        assert_eq!(output.splat, base.splat);
        assert_eq!(output.topology, base.topology);
    }
    #[test]
    fn entrance_revision_reserves_shoulder_clearance_beside_water() {
        let mut base = terrain();
        let n = base.height_resolution;
        for z in 0..n {
            for x in 0..n {
                let wx = (x as f32 / (n - 1) as f32 - 0.5) * 1000.;
                let wz = (z as f32 / (n - 1) as f32 - 0.5) * 1000.;
                if wx.abs() < 100. && (5. ..25.).contains(&wz) {
                    base.height[z * n + x] = encode(-10.);
                }
            }
        }
        let alpha = vec![255; base.surface_resolution.pow(2)];
        let sockets = [socket(1, -200., 0.), socket(2, 200., 0.)];
        let mut output = base.clone();
        let report = build_access(
            &mut output,
            &alpha,
            &sockets,
            &[],
            Access {
                settings: settings(),
                policy: entrance_policy(),
                owner_footprints: &[],
                network: None,
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(report.roads.len(), 1, "{}", report.json());
        assert!(report.roads[0].points.iter().any(|p| p[2] < -10.));
        for (before, after) in base.height.iter().zip(&output.height) {
            if decode(*before) <= 1. {
                assert_eq!(before, after);
            }
        }
    }
    #[test]
    fn entrance_ownership_cannot_bypass_other_protection() {
        let (base, alpha, sockets, owners) = owned_terrain();
        for other_owner in [false, true] {
            let mut input = base.clone();
            let mut footprints = owners.clone();
            let mut config = settings();
            if other_owner {
                footprints.push(Obstacle {
                    monument: 3,
                    ..owners[0]
                });
            } else {
                config.protected_topology |= 8;
                let n = input.surface_resolution;
                for z in 0..n {
                    for x in 0..n {
                        let wx = (x as f32 / n as f32 - 0.5) * 1000.;
                        let wz = (z as f32 / n as f32 - 0.5) * 1000.;
                        if (wx + 200.).abs() < 50. && wz.abs() < 50. {
                            input.topology[z * n + x] |= 8;
                        }
                    }
                }
            }
            let mut output = input.clone();
            let report = build_access(
                &mut output,
                &alpha,
                &sockets,
                &owners,
                Access {
                    settings: config,
                    policy: entrance_policy(),
                    owner_footprints: &footprints,
                    network: None,
                },
                || false,
            )
            .unwrap()
            .unwrap();
            assert!(report.roads.is_empty());
            assert_eq!(input.height, output.height);
            assert_eq!(input.splat, output.splat);
            assert_eq!(input.topology, output.topology);
        }
    }
    #[test]
    fn entrance_holes_and_exit_budgets_do_not_turn_into_teleports_or_partial_paint() {
        let (base, mut alpha, sockets, owners) = owned_terrain();
        let n = base.surface_resolution;
        for z in 0..n {
            for x in 0..n {
                let wx = (x as f32 / n as f32 - 0.5) * 1000.;
                let wz = (z as f32 / n as f32 - 0.5) * 1000.;
                let r = (wx + 200.).abs().max(wz.abs());
                if (20. ..=35.).contains(&r) {
                    alpha[z * n + x] = 0;
                }
            }
        }
        let mut out = base.clone();
        let r = build_access(
            &mut out,
            &alpha,
            &sockets,
            &owners,
            Access {
                settings: settings(),
                policy: entrance_policy(),
                owner_footprints: &owners,
                network: None,
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(r.roads.is_empty());
        assert_eq!(out.height, base.height);
        assert_eq!(out.splat, base.splat);
        let mut policy = entrance_policy();
        policy.maximum_exit_nodes = 16;
        let mut out = base.clone();
        let r = build_access(
            &mut out,
            &vec![255; n * n],
            &sockets,
            &owners,
            Access {
                settings: settings(),
                policy,
                owner_footprints: &owners,
                network: None,
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(r.roads.is_empty());
        assert_eq!(out.height, base.height);
        assert!(
            build_access(
                &mut out,
                &alpha,
                &sockets,
                &owners,
                Access {
                    settings: settings(),
                    policy: entrance_policy(),
                    owner_footprints: &owners,
                    network: None
                },
                || true
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(out.splat, base.splat);
    }
    #[test]
    fn attaches_a_single_trail_socket_to_existing_road_and_retains_paved_material() {
        let mut layers = terrain();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let paved = paved_network(&mut layers, &alpha);
        let before = layers.clone();
        let trail = build_with_network(
            &mut layers,
            &alpha,
            &[socket(3, 0., 200.)],
            &[],
            dirt_settings(),
            (&paved.roads, 8),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(trail.roads.len(), 1);
        assert!(trail.unresolved.is_empty());
        let Destination::Network { road, point } = trail.roads[0].to else {
            panic!("missing network attachment")
        };
        assert_eq!(
            *trail.roads[0].points.last().unwrap(),
            paved.roads[road].points[point]
        );
        assert_eq!(trail.json()["engine"], NETWORK_VERSION);
        assert_eq!(trail.json()["roads"][0]["to"]["network_road"], road);
        assert!(trail.json()["roads"][0]["to"].get("monument").is_none());
        let cells = layers.surface_resolution.pow(2);
        let mut dirt_cells = 0;
        for i in 0..cells {
            let sum: usize = (0..8).map(|c| layers.splat[c * cells + i] as usize).sum();
            assert_eq!(sum, 255);
            if before.topology[i] & settings().road_topology != 0 {
                for c in 0..8 {
                    assert_eq!(layers.splat[c * cells + i], before.splat[c * cells + i]);
                }
            } else if layers.splat[i] > before.splat[i] {
                dirt_cells += 1;
            }
        }
        assert!(dirt_cells > 20);
        let mut replay = before.clone();
        let repeated = build_with_network(
            &mut replay,
            &alpha,
            &[socket(3, 0., 200.)],
            &[],
            dirt_settings(),
            (&paved.roads, 8),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(trail.json(), repeated.json());
        assert_eq!(layers.height, replay.height);
        assert_eq!(layers.splat, replay.splat);
    }
    #[test]
    fn coincident_grid_join_retains_network_height_and_rechecks_join_grade() {
        let base = terrain();
        let alpha = vec![255; base.surface_resolution.pow(2)];
        let make_network = |height| {
            vec![Road {
                from: Destination::Monument {
                    monument: 1,
                    connector: 1,
                },
                to: Destination::Monument {
                    monument: 2,
                    connector: 2,
                },
                points: vec![[0., height, 0.], [100., height, 0.]],
            }]
        };
        let mut layers = base.clone();
        let network = make_network(decode(encode(20.)) + 0.5);
        let report = build_with_network(
            &mut layers,
            &alpha,
            &[socket(3, 0., 100.)],
            &[],
            dirt_settings(),
            (&network, 8),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(report.roads.len(), 1);
        assert_eq!(
            *report.roads[0].points.last().unwrap(),
            network[0].points[0]
        );
        let mut layers = base.clone();
        let network = make_network(30.);
        let report = build_with_network(
            &mut layers,
            &alpha,
            &[socket(3, 0., 100.)],
            &[],
            dirt_settings(),
            (&network, 8),
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(report.roads.is_empty());
        assert_eq!(report.unresolved, vec![3]);
        assert_eq!(layers.height, base.height);
        assert_eq!(layers.splat, base.splat);
    }
    #[test]
    fn isolated_trail_component_stays_unresolved_when_water_blocks_road_access() {
        let mut layers = terrain();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let mut paved = paved_network(&mut layers, &alpha);
        // Shift the existing road south, put both trail sockets north of a sea channel.
        for p in &mut paved.roads[0].points {
            p[2] = -200.;
        }
        let hr = layers.height_resolution;
        for z in hr / 2 - 20..hr / 2 + 20 {
            layers.height[z * hr..(z + 1) * hr].fill(encode(-20.));
        }
        let sockets = [socket(3, -100., 200.), socket(4, 100., 200.)];
        let report = build_with_network(
            &mut layers,
            &alpha,
            &sockets,
            &[],
            dirt_settings(),
            (&paved.roads, 8),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(report.roads.len(), 1);
        assert_eq!(report.unresolved, vec![3, 4]);
        assert!(matches!(report.roads[0].to, Destination::Monument { .. }));
    }
    #[test]
    fn late_extension_cancellation_rolls_back_even_already_accepted_corridors() {
        let mut layers = terrain();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let paved = paved_network(&mut layers, &alpha);
        let before = layers.clone();
        let first = socket(3, 0., 50.);
        let ticks = std::cell::Cell::new(0);
        let targets = network_targets(&paved.roads, 8, layers.size, &|| false).unwrap();
        build_inner(
            &mut layers,
            &alpha,
            std::slice::from_ref(&first),
            &[],
            dirt_settings(),
            Some(&targets),
            None,
            &|| {
                ticks.set(ticks.get() + 1);
                false
            },
        )
        .unwrap()
        .unwrap();
        let limit = ticks.get();
        let sockets = [first, socket(4, 0., 350.)];
        // Prove this cancellation occurs after a corridor was committed internally.
        let run = |output: &mut Layers, transactional: bool| {
            let ticks = std::cell::Cell::new(0);
            let cancelled = || {
                let tick = ticks.get();
                ticks.set(tick + 1);
                tick >= limit
            };
            if transactional {
                build_with_network(
                    output,
                    &alpha,
                    &sockets,
                    &[],
                    dirt_settings(),
                    (&paved.roads, 8),
                    cancelled,
                )
            } else {
                build_inner(
                    output,
                    &alpha,
                    &sockets,
                    &[],
                    dirt_settings(),
                    Some(&targets),
                    None,
                    &cancelled,
                )
            }
        };
        let mut partial = before.clone();
        assert!(run(&mut partial, false).unwrap().is_none());
        assert_ne!(partial.splat, before.splat);
        let mut cancelled = before.clone();
        assert!(run(&mut cancelled, true).unwrap().is_none());
        assert_eq!(cancelled.height, before.height);
        assert_eq!(cancelled.splat, before.splat);
        assert_eq!(cancelled.topology, before.topology);
    }
    #[test]
    fn network_sampling_is_bounded_and_rejects_invalid_geometry_without_writes() {
        let mut layers = terrain();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let mut paved = paved_network(&mut layers, &alpha);
        let before = layers.clone();
        assert!(
            build_with_network(
                &mut layers,
                &alpha,
                &[],
                &[],
                dirt_settings(),
                (&paved.roads, 0),
                || false
            )
            .is_err()
        );
        paved.roads[0].points[0][0] = f32::NAN;
        assert!(
            build_with_network(
                &mut layers,
                &alpha,
                &[],
                &[],
                dirt_settings(),
                (&paved.roads, 8),
                || false
            )
            .is_err()
        );
        paved.roads[0].points = vec![[0., 20., 0.]; MAX_NETWORK_TARGETS + 1];
        assert!(
            build_with_network(
                &mut layers,
                &alpha,
                &[],
                &[],
                dirt_settings(),
                (&paved.roads, 1),
                || false
            )
            .is_err()
        );
        assert_eq!(layers.height, before.height);
        assert_eq!(layers.splat, before.splat);
        assert_eq!(layers.topology, before.topology);
    }
    #[test]
    fn connects_authored_sockets_deterministically_and_shapes_normalized_corridors() {
        let base = terrain();
        let mut first = base.clone();
        let mut second = base.clone();
        let alpha = vec![255; base.surface_resolution.pow(2)];
        let sockets = [
            socket(3, 0., 300.),
            socket(1, -300., 0.),
            socket(2, 300., 0.),
        ];
        let a = build(&mut first, &alpha, &sockets, &[], settings(), || false)
            .unwrap()
            .unwrap();
        let b = build(&mut second, &alpha, &sockets, &[], settings(), || false)
            .unwrap()
            .unwrap();
        assert_eq!(a.roads.len(), 2);
        assert!(a.unresolved.is_empty());
        assert!(a.changed_heights > 0);
        assert_eq!(a.json(), b.json());
        assert_eq!(first.height, second.height);
        assert_eq!(first.splat, second.splat);
        assert!(first.topology.iter().any(|v| v & 2048 != 0));
        assert!(first.topology.iter().any(|v| v & 4096 != 0));
        assert!(first.topology.iter().all(|v| v & 16 != 0));
        let cells = base.surface_resolution.pow(2);
        for i in 0..cells {
            assert_eq!(
                (0..8)
                    .map(|c| first.splat[c * cells + i] as usize)
                    .sum::<usize>(),
                255
            );
        }
        assert!(base.height.iter().all(|h| *h == encode(20.)));
        assert_eq!(
            first.height[20 * first.height_resolution + 20],
            base.height[20 * base.height_resolution + 20]
        );
    }
    #[test]
    fn water_holes_cut_fill_and_cancellation_never_commit_rejected_roads() {
        let base = terrain();
        let sockets = [socket(1, -300., 0.), socket(2, 300., 0.)];
        let alpha = vec![255; base.surface_resolution.pow(2)];
        let mut rejected = base.clone();
        let mut s = settings();
        s.maximum_cut_fill = 0.;
        let report = build(&mut rejected, &alpha, &sockets, &[], s, || false)
            .unwrap()
            .unwrap();
        assert!(report.roads.is_empty());
        assert_eq!(report.unresolved, vec![1, 2]);
        assert_eq!(rejected.height, base.height);
        assert_eq!(rejected.splat, base.splat);
        let mut water = base.clone();
        let n = water.height_resolution;
        for z in 0..n {
            for x in n / 2 - 40..n / 2 + 40 {
                water.height[z * n + x] = encode(-20.);
            }
        }
        let original = water.height.clone();
        let report = build(&mut water, &alpha, &sockets, &[], settings(), || false)
            .unwrap()
            .unwrap();
        assert!(report.roads.is_empty());
        assert_eq!(water.height, original);
        let mut holes = alpha.clone();
        let sr = base.surface_resolution;
        for z in 0..sr {
            holes[z * sr + sr / 2] = 0;
        }
        let mut output = base.clone();
        let report = build(&mut output, &holes, &sockets, &[], settings(), || false)
            .unwrap()
            .unwrap();
        assert!(report.roads.is_empty());
        assert_eq!(output.height, base.height);
        let mut output = base.clone();
        let ticks = std::cell::Cell::new(0);
        assert!(
            build(&mut output, &alpha, &sockets, &[], settings(), || {
                let n = ticks.get();
                ticks.set(n + 1);
                n > 2
            })
            .unwrap()
            .is_none()
        );
        assert_eq!(output.height, base.height);
    }
    #[test]
    fn routes_around_other_monuments_and_rejects_duplicate_socket_ids() {
        let mut layers = terrain();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let sockets = [socket(1, -300., 0.), socket(2, 300., 0.)];
        let obstacle = Obstacle {
            monument: 99,
            center: [0., 0.],
            extent: [50., 50.],
            yaw: 37.,
        };
        let report = build(
            &mut layers,
            &alpha,
            &sockets,
            &[obstacle],
            settings(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(report.roads.len(), 1);
        assert!(report.roads[0].points.iter().any(|p| p[2].abs() > 50.));
        let duplicate = [socket(1, -300., 0.), socket(1, 300., 0.)];
        assert!(build(&mut layers, &alpha, &duplicate, &[], settings(), || false).is_err());
    }
}
