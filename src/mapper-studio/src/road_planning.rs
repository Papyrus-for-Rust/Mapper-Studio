//! Retained road geometry for stages that run before public terrain painting.
//! Existing routing fits a private scratch grid; its terrain writes are discarded.
use crate::{Project, infrastructure, prefab_assets::Catalog, prefab_textures::Bindings};
use papyrus_terrain::{
    monument_composition::{self, Composed},
    path_geometry::Path,
    placement::Solution,
    road_network,
    roadside_monuments::Road,
};
use sha2::{Digest, Sha256};
use std::borrow::Cow;
pub const VERSION: &str = "papyrus-retained-road-plan-1";
pub const STRAIGHTENING_VERSION: &str = "papyrus-roadside-road-straightening-1";
#[derive(Debug)]
pub(crate) struct Failure {
    pub message: String,
    pub entrances: Vec<road_network::socket_recovery::Key>,
    pub rejected_retained_layout: bool,
}
impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self {
            message,
            entrances: Vec::new(),
            rejected_retained_layout: false,
        }
    }
}
impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}
#[derive(Debug, serde::Serialize, PartialEq)]
pub struct StraightenedSpan {
    pub request: u64,
    pub road: usize,
    pub start_index: usize,
    pub end_index: usize,
}
#[derive(Debug, serde::Serialize, PartialEq)]
pub struct JunctionMove {
    pub road: usize,
    pub endpoint: usize,
    pub target_road: usize,
    pub target_point: usize,
    pub before: [f32; 3],
    pub after: [f32; 3],
}
/// Private adjusted geometry, not painted terrain or certified road access.
/// Road numbers and control indices still refer to the retained input plan.
#[derive(Debug, serde::Serialize)]
pub struct StraightenedPaths<'a> {
    pub engine: &'static str,
    paths: Vec<Cow<'a, Path>>,
    hierarchy: Vec<u8>,
    pub spans: Vec<StraightenedSpan>,
    pub work: usize,
    pub changed_controls: usize,
    pub junction_moves: Vec<JunctionMove>,
    pub final_access_validated: bool,
}
impl StraightenedPaths<'_> {
    pub fn roads(&self) -> Vec<Road<'_>> {
        self.paths
            .iter()
            .zip(&self.hierarchy)
            .map(|(path, &hierarchy)| Road {
                path: path.as_ref(),
                hierarchy,
            })
            .collect()
    }
}
pub struct Plan {
    report: road_network::Report,
    curves: Vec<Path>,
    hierarchy: Vec<u8>,
    ring_source_costs: Option<Vec<u32>>,
    ring_lattice: Option<papyrus_terrain::road_costmap::CellLattice>,
    source_family_costs: Vec<crate::source_road_planning::FamilyCosts>,
    context: Option<std::sync::Arc<InputContext>>,
    rail_height_guard: Option<crate::retained_rail_height::RetainedHeight>,
}
struct SocketBackup {
    curves: Vec<Path>,
    controls: Vec<Vec<[f32; 3]>>,
    joins: Vec<road_network::EntranceJoin>,
    audits: Vec<road_network::socket_recovery::Audit>,
}
/// A private retry view. Dropping it restores the exact preceding geometry;
/// only explicit commit retains the checked proposal. Old selection seals fail.
pub struct SocketTrial<'a> {
    plan: &'a mut Plan,
    _maps: &'a Composed,
    backup: Option<SocketBackup>,
    audit: road_network::socket_recovery::Audit,
}
/// A full validation charge is retained once per immutable recovery session.
pub(crate) const ALIGNMENT_BASE_VALIDATION_WORK: usize = 1_000_000;
/// Owns the validated context and borrows the exact map, preventing mutation
/// for this token's lifetime. It cannot authorize a different input context.
#[derive(Clone)]
pub(crate) struct AlignmentBase<'m> {
    maps: &'m Composed,
    context: std::sync::Arc<InputContext>,
}
impl AlignmentBase<'_> {
    fn matches(&self, context: &std::sync::Arc<InputContext>, maps: &Composed) -> bool {
        std::sync::Arc::ptr_eq(context, &self.context) && std::ptr::eq(maps, self.maps)
    }

    pub(crate) fn rail_receipt(&self) -> Option<&crate::retained_rail_scene::Receipt> {
        self.context.rails.as_ref()
    }
}
pub(crate) struct AlignmentTrial<'a, 'm> {
    plan: &'a mut Plan,
    _base: AlignmentBase<'m>,
    curves: Option<Vec<Path>>,
    controls: Vec<Vec<[f32; 3]>>,
}
impl std::ops::Deref for AlignmentTrial<'_, '_> {
    type Target = Plan;
    fn deref(&self) -> &Plan {
        self.plan
    }
}
impl std::ops::DerefMut for AlignmentTrial<'_, '_> {
    fn deref_mut(&mut self) -> &mut Plan {
        self.plan
    }
}
impl AlignmentTrial<'_, '_> {
    pub fn commit(mut self, cancelled: impl Fn() -> bool) -> bool {
        if cancelled() {
            return false;
        }
        self.curves = None;
        true
    }
}
impl Drop for AlignmentTrial<'_, '_> {
    fn drop(&mut self) {
        if let Some(curves) = self.curves.take() {
            self.plan.curves = curves;
            for (road, controls) in self
                .plan
                .report
                .roads
                .iter_mut()
                .zip(std::mem::take(&mut self.controls))
            {
                road.points = controls;
            }
        }
    }
}
impl std::ops::Deref for SocketTrial<'_> {
    type Target = Plan;
    fn deref(&self) -> &Plan {
        self.plan
    }
}
impl std::ops::DerefMut for SocketTrial<'_> {
    fn deref_mut(&mut self) -> &mut Plan {
        self.plan
    }
}
impl SocketTrial<'_> {
    pub fn commit(
        mut self,
        cancelled: impl Fn() -> bool,
    ) -> Option<road_network::socket_recovery::Audit> {
        if cancelled() {
            return None;
        }
        let audit = self.audit.clone();
        self.backup = None;
        Some(audit)
    }
}
impl Drop for SocketTrial<'_> {
    fn drop(&mut self) {
        if let Some(backup) = self.backup.take() {
            self.plan.curves = backup.curves;
            for (road, points) in self.plan.report.roads.iter_mut().zip(backup.controls) {
                road.points = points;
            }
            self.plan.report.entrance_joins = backup.joins;
            self.plan.report.socket_geometry_recovery = backup.audits;
        }
    }
}
struct InputContext {
    rails: Option<crate::retained_rail_scene::Receipt>,
    project: Project,
    solution: Solution,
    catalog: String,
    bindings: String,
    revision: String,
    rivers: Option<std::sync::Arc<papyrus_terrain::river_pipeline::World>>,
}

/// Lives inside the retry loop for one immutable world. Coupled monument or
/// terrain recovery starts another world and builds its own road inputs.
pub(crate) struct RetryWorld<'m> {
    maps: &'m Composed,
    context: Option<std::sync::Arc<InputContext>>,
    ordered: Option<crate::source_road_planning::RetryInputs<'m>>,
    pub(crate) roadside_earthwork: std::cell::RefCell<crate::roadside_placement::EarthworkCache>,
    pub(crate) roadside_terrain: crate::roadside_stamps::TerrainCache,
}

impl<'m> RetryWorld<'m> {
    pub(crate) fn new(maps: &'m Composed) -> Self {
        Self {
            maps,
            context: None,
            ordered: None,
            roadside_earthwork: Default::default(),
            roadside_terrain: Default::default(),
        }
    }

    pub(crate) fn maps(&self) -> &'m Composed {
        self.maps
    }
}

impl InputContext {
    fn check_rails(
        &self,
        maps: &Composed,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Option<()>, String> {
        match (&self.rails, &maps.rails) {
            (None, None) => Ok(Some(())),
            (Some(expected), Some(scene)) => {
                let recipe = self
                    .project
                    .rails
                    .as_ref()
                    .ok_or("Retained rail scene has no saved recipe")?;
                let Some(current) = crate::retained_rail_scene::Receipt::capture(
                    scene,
                    recipe,
                    maps.layers.surface_resolution,
                    cancelled,
                )?
                else {
                    return Ok(None);
                };
                if &current != expected {
                    return Err("Retained road plan rail revision changed".into());
                }
                Ok(Some(()))
            }
            _ => Err("Retained road plan rail presence changed".into()),
        }
    }
}
pub(crate) fn hash_raster_words<T: bytemuck::Pod, const N: usize>(
    hash: &mut Sha256,
    words: &[T],
    _to_le_bytes: impl Fn(&T) -> [u8; N],
    cancelled: &impl Fn() -> bool,
) -> Option<()> {
    #[cfg(target_endian = "big")]
    let mut bytes = Vec::with_capacity(4096 * N);
    for chunk in words.chunks(4096) {
        if cancelled() {
            return None;
        }
        #[cfg(target_endian = "little")]
        hash.update(bytemuck::cast_slice(chunk));
        #[cfg(target_endian = "big")]
        {
            bytes.clear();
            bytes.extend(chunk.iter().flat_map(&_to_le_bytes));
            hash.update(&bytes);
        }
    }
    Some(())
}

fn revision(maps: &Composed, cancelled: &impl Fn() -> bool) -> Option<String> {
    let mut hash = Sha256::new();
    hash.update(b"papyrus-road-plan-input-1");
    for value in [
        u64::from(maps.layers.size),
        u64::from(maps.layers.terrain_seed),
        u64::from(maps.layers.biome_seed),
        maps.layers.height_resolution as u64,
        maps.layers.surface_resolution as u64,
    ] {
        hash.update(value.to_le_bytes());
    }
    hash_raster_words(&mut hash, &maps.layers.height, |v| v.to_le_bytes(), cancelled)?;
    for values in [&maps.layers.biome, &maps.layers.splat, &maps.alpha] {
        hash.update((values.len() as u64).to_le_bytes());
        for chunk in values.chunks(65536) {
            if cancelled() {
                return None;
            }
            hash.update(chunk);
        }
    }
    hash_raster_words(&mut hash, &maps.layers.topology, |v| v.to_le_bytes(), cancelled)?;
    if let Some(ocean) = &maps.ocean {
        hash.update([1]);
        hash.update((ocean.resolution as u64).to_le_bytes());
        hash.update(&ocean.classification);
        hash_raster_words(&mut hash, &ocean.topology, |v| v.to_le_bytes(), cancelled)?;
        hash_raster_words(&mut hash, &ocean.local_water, |v| v.to_le_bytes(), cancelled)?;
    } else {
        hash.update([0]);
    }
    (!cancelled()).then(|| format!("{:x}", hash.finalize()))
}
fn family_hierarchy(report: &road_network::Report) -> Result<Vec<u8>, String> {
    if report.roads.len() > 4096 || report.families.len() > 4096 {
        return Err("Retained road plan exceeds path allowance".into());
    }
    let mut hierarchy = vec![None; report.roads.len()];
    for family in &report.families {
        let end = family
            .first_road
            .checked_add(family.road_count)
            .filter(|&n| n <= hierarchy.len())
            .ok_or("Road family exceeds retained controls")?;
        if family.hierarchy > 2 {
            return Err("Unsupported retained road hierarchy".into());
        }
        for h in &mut hierarchy[family.first_road..end] {
            if h.replace(family.hierarchy as u8).is_some() {
                return Err("Overlapping retained road families".into());
            }
        }
    }
    let hierarchy = hierarchy
        .into_iter()
        .map(|h| h.ok_or_else(|| "Retained road has no source family".to_owned()))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(hierarchy)
}
impl Plan {
    /// Retain a typed source handoff without rebuilding its shaped curves.
    /// This plan is deliberately unbound: source/recipe/map binding must occur
    /// in the normal preparation transaction before downstream stages can run.
    /// No legacy entrance or terrain acceptance certificates are fabricated.
    pub fn from_source_network(
        network: crate::source_road_handoff::Network,
        families: Vec<road_network::Family>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        let (roads, paths, hierarchy, receipt) = network.into_parts();
        if families.len() > 3
            || families.iter().any(|f| {
                !f.width.is_finite()
                    || !(0.1..=100.).contains(&f.width)
                    || f.splat_channel >= 8
                    || !(0..=64).contains(&f.infrastructure_type)
                    || !f.unresolved.is_empty()
            })
        {
            return Err("Invalid source handoff family parameters".into());
        }
        let socket_count = roads
            .iter()
            .flat_map(|r| [r.from, r.to])
            .filter_map(|d| match d {
                road_network::Destination::Monument {
                    monument,
                    connector,
                } => Some((monument, connector)),
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        let trails_enabled = Some(families.iter().any(|f| f.hierarchy == 2));
        let report = road_network::Report {
            roads,
            families,
            socket_count,
            trails_enabled,
            network_attachment_stage: true,
            source_layout: Some(serde_json::to_value(receipt).map_err(|e| e.to_string())?),
            ..Default::default()
        };
        if cancelled() {
            return Ok(None);
        }
        if family_hierarchy(&report)? != hierarchy {
            return Err("Source handoff road families do not match shaped hierarchy".into());
        }
        let mut points = 0usize;
        for path in &paths {
            if cancelled() {
                return Ok(None);
            }
            points = points
                .checked_add(path.points().len())
                .filter(|&n| n <= 1_000_000)
                .ok_or("Retained road control budget reached")?;
        }
        Ok(Some(Self {
            report,
            curves: paths,
            hierarchy,
            ring_source_costs: None,
            ring_lattice: None,
            source_family_costs: Vec::new(),
            context: None,
            rail_height_guard: None,
        }))
    }

    pub(crate) fn alignment_base<'m>(
        &self,
        maps: &'m Composed,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<AlignmentBase<'m>>, String> {
        if cancelled() {
            return Ok(None);
        }
        let context = self
            .context
            .as_ref()
            .ok_or("Alignment requires a bound plan")?;
        if context.check_rails(maps, &cancelled)?.is_none() {
            return Ok(None);
        }
        let Some(current) = revision(maps, &cancelled) else {
            return Ok(None);
        };
        if current != context.revision {
            return Err("Alignment map revision changed".into());
        }
        Ok(Some(AlignmentBase {
            maps,
            context: context.clone(),
        }))
    }

    /// A conservative scalar-copy/validation count, including both rollback
    /// snapshots and the largest prospective curve rebuild. Map hashing is
    /// excluded because the immutable base token already validated that data.
    pub(crate) fn alignment_preparation_work(&self) -> Result<usize, String> {
        if self.curves.len() != self.report.roads.len() {
            return Err("Alignment curve count differs".into());
        }
        let mut work = 64usize;
        let mut largest = 0usize;
        for (path, road) in self.curves.iter().zip(&self.report.roads) {
            largest = largest.max(road.points.len());
            work = work
                .checked_add(
                    path.points()
                        .len()
                        .checked_mul(6)
                        .ok_or("Alignment copy overflow")?,
                )
                .and_then(|n| n.checked_add(road.points.len().checked_mul(3)?))
                .and_then(|n| n.checked_add(32))
                .ok_or("Alignment copy overflow")?;
        }
        work.checked_add(
            largest
                .checked_mul(12)
                .ok_or("Alignment preparation overflow")?,
        )
        .ok_or("Alignment preparation overflow".into())
    }

    #[cfg(test)]
    pub(crate) fn alignment_trial<'a, 'm>(
        &'a mut self,
        proposal: crate::road_alignment::Proposal,
        maps: &'m Composed,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<AlignmentTrial<'a, 'm>>, String> {
        let Some(base) = self.alignment_base(maps, &cancelled)? else {
            return Ok(None);
        };
        self.alignment_trial_checked(
            proposal,
            &base,
            self.alignment_preparation_work()?,
            cancelled,
        )
    }

    pub(crate) fn alignment_trial_checked<'a, 'm>(
        &'a mut self,
        proposal: crate::road_alignment::Proposal,
        base: &AlignmentBase<'m>,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<AlignmentTrial<'a, 'm>>, String> {
        if cancelled() {
            return Ok(None);
        }
        let context = self
            .context
            .as_ref()
            .ok_or("Alignment requires a bound plan")?;
        if !std::sync::Arc::ptr_eq(context, &base.context) {
            return Err("Alignment validation belongs to another plan context".into());
        }
        let preparation = self.alignment_preparation_work()?;
        if preparation > maximum_work {
            return Err("Alignment preparation budget exhausted".into());
        }
        let road = self
            .report
            .roads
            .get(proposal.road)
            .ok_or("Alignment road missing")?;
        if road.points != proposal.before || proposal.after.len() != road.points.len() {
            return Err("Stale alignment proposal".into());
        }
        let Some(path) = Path::from_controls(&proposal.after, preparation, &cancelled)
            .map_err(|e| format!("Alignment path: {e:?}"))?
        else {
            return Ok(None);
        };
        // Allocate and check everything before changing the retained plan.
        let mut curves = self.curves.clone();
        curves[proposal.road] = path;
        let controls = self.report.roads.iter().map(|r| r.points.clone()).collect();
        if cancelled() {
            return Ok(None);
        }
        let curves = std::mem::replace(&mut self.curves, curves);
        self.report.roads[proposal.road].points = proposal.after;
        Ok(Some(AlignmentTrial {
            plan: self,
            _base: base.clone(),
            curves: Some(curves),
            controls,
        }))
    }
    pub(crate) fn protect_rail_height(
        &mut self,
        guard: crate::retained_rail_height::RetainedHeight,
    ) {
        self.rail_height_guard = Some(guard);
    }
    pub(crate) fn rail_height_guard(&self) -> Option<&crate::retained_rail_height::RetainedHeight> {
        self.rail_height_guard.as_ref()
    }
    /// Binds a provisional selection to exact indexed controls and tangents.
    /// This seal protects geometry handoff, not final-world terrain acceptance.
    pub(crate) fn quality_feedback_binding(
        &self,
        source: &str,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<String>, String> {
        let context = self
            .context
            .as_ref()
            .ok_or("Quality feedback requires a bound plan")?;
        let Some(geometry) = self.selection_binding(&[], &cancelled)? else {
            return Ok(None);
        };
        let mut hash = Sha256::new();
        hash.update(b"papyrus-road-quality-feedback-1");
        for value in [
            &context.catalog,
            &context.bindings,
            &context.revision,
            &geometry,
        ] {
            hash.update(value.as_bytes());
        }
        hash.update(source.as_bytes());
        hash.update(serde_json::to_vec(&context.project).map_err(|e| e.to_string())?);
        hash.update((context.solution.required_missing as u64).to_le_bytes());
        hash.update((context.solution.attempts as u64).to_le_bytes());
        hash.update((context.solution.outcomes.len() as u64).to_le_bytes());
        for outcome in &context.solution.outcomes {
            hash.update(outcome.id.to_le_bytes());
            hash.update([u8::from(outcome.within_radius)]);
            hash.update((outcome.reason.len() as u64).to_le_bytes());
            hash.update(outcome.reason.as_bytes());
            if let Some(distance) = outcome.distance {
                hash.update([1]);
                hash.update(distance.to_le_bytes());
            } else {
                hash.update([0]);
            }
            if let Some(p) = &outcome.position {
                hash.update([1]);
                for v in [p.x, p.y, p.z, p.yaw, p.clearance] {
                    hash.update(v.to_le_bytes());
                }
            } else {
                hash.update([0]);
            }
        }
        hash.update(serde_json::to_vec(&self.report.json()).map_err(|e| e.to_string())?);
        Ok((!cancelled()).then(|| format!("{:x}", hash.finalize())))
    }
    pub(crate) fn selection_binding(
        &self,
        selected: &[papyrus_terrain::roadside_selection::Selected],
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<String>, String> {
        if selected.len() > 128 {
            return Err("Roadside selection exceeds span allowance".into());
        }
        let mut hash = Sha256::new();
        hash.update(b"papyrus-roadside-geometry-binding-1");
        if let Some(rails) = self.context.as_ref().and_then(|c| c.rails.as_ref()) {
            hash.update(b"papyrus-retained-rail-selection-1");
            hash.update(rails.scene_sha256.as_bytes());
        }
        hash.update(serde_json::to_vec(selected).map_err(|e| e.to_string())?);
        hash.update((self.curves.len() as u64).to_le_bytes());
        for (index, (path, &hierarchy)) in self.curves.iter().zip(&self.hierarchy).enumerate() {
            if cancelled() {
                return Ok(None);
            }
            hash.update([hierarchy]);
            for destination in [self.report.roads[index].from, self.report.roads[index].to] {
                match destination {
                    road_network::Destination::Loop => hash.update([0]),
                    road_network::Destination::Monument {
                        monument,
                        connector,
                    } => {
                        hash.update([1]);
                        hash.update(monument.to_le_bytes());
                        hash.update(connector.to_le_bytes());
                    }
                    road_network::Destination::Network { road, point } => {
                        hash.update([2]);
                        hash.update((road as u64).to_le_bytes());
                        hash.update((point as u64).to_le_bytes());
                    }
                    road_network::Destination::RawNetwork(connection) => {
                        hash.update([3]);
                        hash.update(serde_json::to_vec(&connection).map_err(|e| e.to_string())?);
                    }
                }
            }
            hash.update((path.points().len() as u64).to_le_bytes());
            let (a, b) = path.range();
            hash.update((a as u64).to_le_bytes());
            hash.update((b as u64).to_le_bytes());
            hash.update(path.length().to_le_bytes());
            hash.update(path.step_size().to_le_bytes());
            for values in [path.points(), path.tangents()] {
                for chunk in values.chunks(64) {
                    if cancelled() {
                        return Ok(None);
                    }
                    for value in chunk.iter().flatten() {
                        hash.update(value.to_le_bytes());
                    }
                }
            }
        }
        if cancelled() {
            return Ok(None);
        }
        Ok(Some(format!("{:x}", hash.finalize())))
    }
    pub(crate) fn straighten_selected(
        &self,
        selected: &[papyrus_terrain::roadside_selection::Selected],
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<StraightenedPaths<'_>>, String> {
        if selected.len() > 128 || !(1..=32_000_000).contains(&maximum_work) {
            return Err("Invalid roadside straightening allowance".into());
        }
        let mut requests = std::collections::BTreeSet::new();
        for span in selected {
            let path = self
                .curves
                .get(span.road)
                .ok_or("Unknown roadside road index")?;
            if span.request == 0
                || !requests.insert(span.request)
                || span.start_index >= span.end_index
                || span.start_index < path.range().0
                || span.end_index > path.range().1
            {
                return Err("Invalid or repeated roadside span".into());
            }
        }
        let mut paths = Vec::with_capacity(self.curves.len());
        let mut work = 0;
        for (index, path) in self.curves.iter().enumerate() {
            if cancelled() {
                return Ok(None);
            }
            let spans: Vec<_> = selected
                .iter()
                .filter(|s| s.road == index)
                .map(|s| (s.start_index, s.end_index))
                .collect();
            let adjusted = if spans.is_empty() {
                work += path.points().len() * 2;
                if work > maximum_work {
                    return Err("Road straightening work budget reached".into());
                }
                Cow::Borrowed(path)
            } else {
                if work >= maximum_work {
                    return Err("Road straightening work budget reached".into());
                }
                let Some(adjusted) = path
                    .straighten(&spans, maximum_work - work, &cancelled)
                    .map_err(|e| format!("Road {index} straightening failed: {e:?}"))?
                else {
                    return Ok(None);
                };
                work += adjusted.work();
                Cow::Owned(adjusted)
            };
            paths.push(adjusted);
        }
        // Independent routing records explicit attachments to earlier roads.
        // Source-style span edits can move those controls. Reattach in graph
        // order so later dirt/paved branches observe the corrected parent.
        let mut junction_moves = Vec::new();
        for (index, road) in self.report.roads.iter().enumerate() {
            if cancelled() {
                return Ok(None);
            }
            let mut endpoints = [None, None];
            for (side, destination) in [road.from, road.to].into_iter().enumerate() {
                if let road_network::Destination::RawNetwork(connection) = destination {
                    let endpoint = if side == 0 {
                        0
                    } else {
                        paths[index].points().len() - 1
                    };
                    if !connection.matches(
                        index,
                        side,
                        paths.len(),
                        paths[index].points()[endpoint],
                    ) {
                        return Err(
                            "Roadside edit moved a raw source endpoint; rebuild its source network"
                                .into(),
                        );
                    }
                    continue;
                }
                let road_network::Destination::Network {
                    road: target,
                    point,
                } = destination
                else {
                    continue;
                };
                if target >= index {
                    return Err("Road attachment is not in dependency order".into());
                }
                let expected = *self.curves[target]
                    .points()
                    .get(point)
                    .ok_or("Road attachment control is missing")?;
                let endpoint = if side == 0 {
                    0
                } else {
                    self.curves[index].points().len() - 1
                };
                if self.curves[index].points()[endpoint]
                    .iter()
                    .zip(expected)
                    .any(|(a, b)| (*a - b).abs() > 0.0001)
                {
                    return Err("Retained road attachment was already disconnected".into());
                }
                let next = paths[target].points()[point];
                let before = paths[index].points()[endpoint];
                if before
                    .iter()
                    .zip(next)
                    .any(|(a, b)| a.to_bits() != b.to_bits())
                {
                    if selected.iter().any(|s| {
                        s.road == index && s.start_index <= endpoint && endpoint <= s.end_index
                    }) {
                        return Err("Moved junction reaches a selected monument span; re-selection is required".into());
                    }
                    endpoints[side] = Some(next);
                    junction_moves.push(JunctionMove {
                        road: index,
                        endpoint,
                        target_road: target,
                        target_point: point,
                        before,
                        after: next,
                    });
                }
            }
            if endpoints.iter().any(Option::is_some) {
                if work >= maximum_work {
                    return Err("Road junction work budget reached".into());
                }
                let Some(adjusted) = paths[index]
                    .reattach_endpoints(endpoints, maximum_work - work, &cancelled)
                    .map_err(|e| format!("Road {index} junction repair failed: {e:?}"))?
                else {
                    return Ok(None);
                };
                work += adjusted.work();
                paths[index] = Cow::Owned(adjusted);
            }
        }
        let mut changed_controls = 0;
        for (before, after) in self.curves.iter().zip(&paths) {
            if cancelled() {
                return Ok(None);
            }
            if matches!(after, Cow::Borrowed(_)) {
                continue;
            }
            changed_controls += before
                .points()
                .iter()
                .zip(after.points())
                .filter(|(a, b)| {
                    a.iter()
                        .zip(b.iter())
                        .any(|(a, b)| a.to_bits() != b.to_bits())
                })
                .count();
        }
        if cancelled() {
            return Ok(None);
        }
        Ok(Some(StraightenedPaths {
            engine: STRAIGHTENING_VERSION,
            paths,
            hierarchy: self.hierarchy.clone(),
            work,
            changed_controls,
            junction_moves,
            final_access_validated: false,
            spans: selected
                .iter()
                .map(|s| StraightenedSpan {
                    request: s.request,
                    road: s.road,
                    start_index: s.start_index,
                    end_index: s.end_index,
                })
                .collect(),
        }))
    }
    pub fn check_inputs(
        &self,
        project: &Project,
        solution: &Solution,
        catalog: &Catalog,
        bindings: &Bindings,
        maps: &Composed,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<()>, String> {
        self.check_inputs_with_base(project, solution, catalog, bindings, maps, None, cancelled)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn check_inputs_with_base(
        &self,
        project: &Project,
        solution: &Solution,
        catalog: &Catalog,
        bindings: &Bindings,
        maps: &Composed,
        prepared: Option<&AlignmentBase<'_>>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<()>, String> {
        if cancelled() {
            return Ok(None);
        }
        if maps.roads.is_some()
            || (maps.rails.is_some() && self.context.as_ref().is_none_or(|c| c.rails.is_none()))
            || maps.objects.is_some()
            || maps.decoration.is_some()
            || maps.gameplay_height.is_some()
            || maps.topology_before_overrides.is_some()
            || maps.topology_overrides.is_some()
            || maps.spawn_audit.is_some()
        {
            return Err("Road plan requires the intermediate monument revision".into());
        }
        let context = self
            .context
            .as_ref()
            .ok_or("Road plan is missing its input context")?;
        if prepared.is_none() && context.check_rails(maps, &cancelled)?.is_none() {
            return Ok(None);
        }
        let same_rivers = match (&context.rivers, &maps.rivers) {
            (None, None) => true,
            (Some(a), Some(b)) => std::sync::Arc::ptr_eq(a, b),
            _ => false,
        };
        if context.project != *project
            || context.solution != *solution
            || context.catalog != catalog.sha256
            || context.bindings != bindings.sha256
            || !same_rivers
        {
            return Err(
                "Retained road plan belongs to a different project or layout revision".into(),
            );
        }
        if let Some(prepared) = prepared {
            if !prepared.matches(context, maps) {
                return Err("Prepared road base belongs to a different plan or map".into());
            }
            return Ok(Some(()));
        }
        let Some(current) = revision(maps, &cancelled) else {
            return Ok(None);
        };
        if current != context.revision {
            return Err("Retained road plan terrain revision changed".into());
        }
        Ok(Some(()))
    }
    /// Checked geometry trial on this plan's unchanged pre-road map revision.
    /// Every retry must perform new roadside selection and fresh road painting.
    pub fn socket_trial<'a>(
        &'a mut self,
        proposal: road_network::socket_recovery::Proposal,
        maps: &'a Composed,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<SocketTrial<'a>>, String> {
        if cancelled() {
            return Ok(None);
        }
        let context = self
            .context
            .as_ref()
            .ok_or("Socket trial requires a bound retained plan")?;
        if context.check_rails(maps, &cancelled)?.is_none() {
            return Ok(None);
        }
        let Some(current) = revision(maps, &cancelled) else {
            return Ok(None);
        };
        if current != context.revision {
            return Err("Socket trial map revision changed".into());
        }
        let mut curves = Vec::with_capacity(proposal.controls().len());
        let mut work = 0_usize;
        for points in proposal.controls() {
            work = work
                .checked_add(points.len())
                .filter(|w| *w <= 1_000_000)
                .ok_or("Socket trial path budget reached")?;
            let Some(path) = Path::from_controls(points, 1_000_000 - work, &cancelled)
                .map_err(|e| format!("Socket trial path: {e:?}"))?
            else {
                return Ok(None);
            };
            work = work
                .checked_add(path.work())
                .filter(|w| *w <= 1_000_000)
                .ok_or("Socket trial path budget reached")?;
            curves.push(path);
        }
        let controls = self.report.roads.iter().map(|r| r.points.clone()).collect();
        let joins = self
            .report
            .entrance_joins
            .iter()
            .map(|j| road_network::EntranceJoin {
                monument: j.monument,
                connector: j.connector,
                points: j.points.clone(),
            })
            .collect();
        let audits = self.report.socket_geometry_recovery.clone();
        let Some(audit) = proposal
            .apply(&mut self.report, &cancelled)
            .map_err(|e| format!("Socket trial rejected: {e:?}"))?
        else {
            return Ok(None);
        };
        let curves = std::mem::replace(&mut self.curves, curves);
        Ok(Some(SocketTrial {
            plan: self,
            _maps: maps,
            backup: Some(SocketBackup {
                curves,
                controls,
                joins,
                audits,
            }),
            audit,
        }))
    }
    pub fn roads(&self) -> Vec<Road<'_>> {
        self.curves
            .iter()
            .zip(&self.hierarchy)
            .map(|(path, &hierarchy)| Road { hierarchy, path })
            .collect()
    }
    /// Diagnostic of scratch routing, not terrain writes committed to the world.
    pub(crate) fn checked_ring_costs(&self) -> Option<(road_network::LaunchPolicy, &[u32])> {
        self.report
            .entrance_launch
            .zip(self.ring_source_costs.as_deref())
    }
    pub(crate) fn source_cost_grid(
        &self,
        hierarchy: u32,
        seed: u32,
    ) -> Result<Option<(papyrus_terrain::road_costmap::CellLattice, &[u32])>, String> {
        if self.report.source_layout.is_none() {
            return Ok(None);
        }
        if !self
            .report
            .families
            .iter()
            .any(|f| f.hierarchy == hierarchy && f.seed == seed)
        {
            return Err("Source cost family seed differs".into());
        }
        if hierarchy == 0 {
            return self
                .ring_lattice
                .zip(self.ring_source_costs.as_deref())
                .map(Some)
                .ok_or("Missing retained source ring grid".into());
        }
        let mut matching = self
            .source_family_costs
            .iter()
            .filter(|g| g.hierarchy == hierarchy);
        let grid = matching
            .next()
            .ok_or("Missing retained source family grid")?;
        if grid.grid.grid.engine != papyrus_terrain::road_costmap::CELL_VERSION
            || grid.grid.grid.resolution != grid.grid.lattice.resolution()
            || grid.grid.grid.costs.len() != grid.grid.lattice.resolution().pow(2)
        {
            return Err("Invalid retained source lattice".into());
        }
        if matching.next().is_some()
            || grid.grid.grid.seed != seed
            || grid.grid.grid.trail != (hierarchy == 2)
        {
            return Err("Ambiguous or mismatched source cost grid".into());
        }
        Ok(Some((grid.grid.lattice, &grid.grid.grid.costs)))
    }
    #[cfg(test)]
    pub(crate) fn bind_uniform_test_costs(
        &mut self,
        lattice: papyrus_terrain::road_costmap::CellLattice,
    ) {
        use papyrus_terrain::road_costmap::{CELL_VERSION, CellGrid, Counts, Grid};
        self.report.source_layout =
            Some(serde_json::json!({"engine": crate::source_road_handoff::VERSION}));
        let resolution = lattice.resolution();
        let costs = vec![101; resolution.pow(2)];
        self.ring_lattice = Some(lattice);
        self.ring_source_costs = Some(costs.clone());
        self.source_family_costs = self
            .report
            .families
            .iter()
            .filter(|family| family.hierarchy != 0)
            .map(|family| crate::source_road_planning::FamilyCosts {
                hierarchy: family.hierarchy,
                grid: CellGrid {
                    lattice,
                    grid: Grid {
                        engine: CELL_VERSION,
                        resolution,
                        seed: family.seed,
                        trail: family.hierarchy == 2,
                        counts: Counts::default(),
                        work: 0,
                        costs: costs.clone(),
                    },
                },
            })
            .collect();
    }
    pub(crate) fn check_ring_curve(
        &self,
        path: &Path,
        size: u32,
        spacing: f32,
        maximum_work: usize,
        cancelled: &impl Fn() -> bool,
    ) -> Result<
        Option<papyrus_terrain::road_smoothing::CurveCostAudit>,
        papyrus_terrain::path_geometry::Failure,
    > {
        let costs = self
            .ring_source_costs
            .as_deref()
            .ok_or(papyrus_terrain::path_geometry::Failure::Invalid)?;
        if let Some(lattice) = self.ring_lattice {
            if lattice.size() != size as f32 {
                return Err(papyrus_terrain::path_geometry::Failure::Invalid);
            }
            papyrus_terrain::road_smoothing::curve_cell_cost_check(
                path,
                costs,
                lattice,
                spacing,
                maximum_work,
                cancelled,
            )
        } else {
            papyrus_terrain::road_smoothing::curve_cost_check(
                path,
                costs,
                size,
                spacing,
                maximum_work,
                cancelled,
            )
        }
    }
    pub fn planning_report(&self) -> &road_network::Report {
        &self.report
    }

    pub(crate) fn retain_source_layout(
        &mut self,
        layout: Option<std::sync::Arc<papyrus_terrain::source_road_layout::AcceptedNetwork>>,
    ) {
        self.report.source_route_layout = layout;
    }
    pub(crate) fn apply_path_edits(
        &mut self,
        edits: &[crate::path_edit::Change],
        cancelled: &impl Fn() -> bool,
    ) -> Result<bool, String> {
        let Some(edits) = crate::path_edit::prepare_road_edits(&self.report, edits, cancelled)?
        else { return Ok(false); };
        let mut curves = Vec::with_capacity(edits.points.len());
        for (&index, points) in &edits.points {
            let Some(curve) = Path::from_controls(points, 32_000_000, cancelled)
                .map_err(|error| format!("Invalid edited road spline: {error:?}"))?
            else { return Ok(false); };
            curves.push((index, curve));
        }
        if cancelled() { return Ok(false); }
        edits.commit(&mut self.report);
        for (index, curve) in curves {
            self.curves[index] = curve;
        }
        Ok(true)
    }
    pub(crate) fn into_report(self) -> road_network::Report {
        self.report
    }
    pub(crate) fn from_report(
        report: road_network::Report,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        let hierarchy = family_hierarchy(&report)?;
        let mut curves = Vec::with_capacity(report.roads.len());
        let mut work = 0;
        for road in &report.roads {
            if cancelled() {
                return Ok(None);
            }
            work += road.points.len();
            if work > 1_000_000 {
                return Err("Retained road control budget reached".into());
            }
            let Some(path) = Path::from_controls(&road.points, 1_000_000, &cancelled)
                .map_err(|e| format!("Invalid retained road controls: {e:?}"))?
            else {
                return Ok(None);
            };
            curves.push(path);
        }
        if cancelled() {
            return Ok(None);
        }
        let ring_source_costs = if report
            .entrance_launch
            .is_some_and(|p| p.engine == road_network::LaunchEngine::GlobalEarthwork)
        {
            if let Some(source) = &report.ring_costmap {
                let grid = &source["grid"];
                let values = grid["costs"]
                    .as_array()
                    .filter(|v| v.len() == 257 * 257)
                    .ok_or("Invalid retained ring cost grid")?;
                if grid["resolution"] != 257 {
                    return Err("Invalid retained ring cost resolution".into());
                }
                let mut costs = Vec::with_capacity(values.len());
                for (i, v) in values.iter().enumerate() {
                    if i.is_multiple_of(4096) && cancelled() {
                        return Ok(None);
                    }
                    costs.push(
                        v.as_u64()
                            .filter(|v| *v <= 1_000_000)
                            .ok_or("Invalid retained ring cost cell")?
                            as u32,
                    );
                }
                Some(costs)
            } else {
                None
            }
        } else {
            None
        };
        Ok(Some(Self {
            report,
            curves,
            hierarchy,
            ring_source_costs,
            ring_lattice: None,
            source_family_costs: Vec::new(),
            context: None,
            rail_height_guard: None,
        }))
    }
}
/// Generates road routes against a private scratch revision, retaining the input
/// stamp/terrain arrays for roadside eligibility. Does not publish painted maps,
/// meshes, topology, ocean updates or a final-world report.
pub fn prepare(
    project: &Project,
    solution: &Solution,
    catalog: &Catalog,
    bindings: &Bindings,
    maps: &Composed,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Plan>, String> {
    prepare_inner(
        project,
        solution,
        catalog,
        bindings,
        &mut RetryWorld::new(maps),
        false,
        &[],
        cancelled,
    )
    .map(|prepared| prepared.map(|(plan, _)| plan))
    .map_err(|error| error.message)
}
/// Explicit pre-mesh rail input; normal legacy planning still rejects rails.
pub fn prepare_retaining_rails(
    project: &Project,
    solution: &Solution,
    catalog: &Catalog,
    bindings: &Bindings,
    maps: &Composed,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Plan>, String> {
    prepare_inner(
        project,
        solution,
        catalog,
        bindings,
        &mut RetryWorld::new(maps),
        true,
        &[],
        cancelled,
    )
    .map(|prepared| prepared.map(|(plan, _)| plan))
    .map_err(|error| error.message)
}
pub(crate) fn prepare_replanned<'m>(
    project: &Project,
    solution: &Solution,
    catalog: &Catalog,
    bindings: &Bindings,
    world: &mut RetryWorld<'m>,
    preferences: &[crate::road_replanning::Marker],
    cancelled: impl Fn() -> bool,
) -> Result<Option<(Plan, AlignmentBase<'m>)>, Failure> {
    let retaining = world.maps().rails.is_some();
    prepare_inner(
        project,
        solution,
        catalog,
        bindings,
        world,
        retaining,
        preferences,
        cancelled,
    )
}
#[allow(clippy::too_many_arguments)]
fn prepare_inner<'m>(
    project: &Project,
    solution: &Solution,
    catalog: &Catalog,
    bindings: &Bindings,
    world: &mut RetryWorld<'m>,
    retaining: bool,
    preferences: &[crate::road_replanning::Marker],
    cancelled: impl Fn() -> bool,
) -> Result<Option<(Plan, AlignmentBase<'m>)>, Failure> {
    if cancelled() {
        return Ok(None);
    }
    let maps = world.maps();
    if maps.roads.is_some()
        || (maps.rails.is_some() && !retaining)
        || maps.objects.is_some()
        || maps.decoration.is_some()
        || maps.gameplay_height.is_some()
        || maps.topology_before_overrides.is_some()
        || maps.topology_overrides.is_some()
        || maps.spawn_audit.is_some()
    {
        return Err("Road planning requires the intermediate monument revision".into());
    }
    let context = if let Some(context) = &world.context {
        if context.project != *project || context.solution != *solution
            || context.catalog != catalog.sha256 || context.bindings != bindings.sha256
            || context.rails.is_some() != retaining
        {
            return Err("Road retry inputs belong to a different recipe or placement".into());
        }
        context.clone()
    } else {
        let rails = if retaining {
            let scene = maps.rails.as_ref().ok_or("Retained road planning requires prepared rails")?;
            let recipe = project.rails.as_ref().ok_or("Prepared rails require their saved recipe")?;
            let Some(receipt) = crate::retained_rail_scene::Receipt::capture(
                scene, recipe, maps.layers.surface_resolution, &cancelled,
            )? else {
                return Ok(None);
            };
            Some(receipt)
        } else {
            None
        };
        let Some(revision) = revision(maps, &cancelled) else {
            return Ok(None);
        };
        let context = std::sync::Arc::new(InputContext {
            rails,
            project: project.clone(),
            solution: solution.clone(),
            catalog: catalog.sha256.clone(),
            bindings: bindings.sha256.clone(),
            revision,
            rivers: maps.rivers.clone(),
        });
        world.context = Some(context.clone());
        context
    };
    let rails = context.rails.as_ref();
    let recipe = project
        .roads
        .as_ref()
        .ok_or("Road planning requires a source road recipe")?;
    if recipe.engine != road_network::COST_VERSION {
        return Err("Retained road planning requires source-cost road generation".into());
    }
    let source = infrastructure::Source::cached_shared(catalog, recipe, &cancelled)?;
    let mut plan = if let Some(policy) = recipe.staged.as_ref().and_then(|s| s.layout.as_ref()) {
        let Some(prepared) = crate::source_road_planning::prepare(
            project,
            solution,
            catalog,
            bindings,
            maps,
            &source,
            rails,
            policy,
            &mut world.ordered,
            preferences,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        let mut plan = prepared.plan;
        plan.source_family_costs = prepared.family_costs;
        if !prepared.terrain_entrances.is_empty() {
            plan.report
                .source_layout
                .as_mut()
                .ok_or("Missing source layout receipt")?["terrain_entrances"] =
                serde_json::json!(prepared.terrain_entrances);
        }
        if !prepared.routing_preferences.is_empty() {
            plan.report
                .source_layout
                .as_mut()
                .ok_or("Missing source layout receipt")?["routing_preferences"] =
                serde_json::json!(prepared.routing_preferences);
        }
        if !prepared.family_geometry.is_empty() {
            plan.report.source_layout.as_mut().ok_or("Missing source layout receipt")?["family_geometry_preflight"] =
                serde_json::json!(prepared.family_geometry);
        }
        if let Some(clearance) = prepared.ring_clearance {
            plan.report
                .source_layout
                .as_mut()
                .ok_or("Missing source layout receipt")?["ring_clearance"] = clearance;
        }
        plan.report.entrance_launch = recipe.entrance_launch;
        if let Some(grid) = prepared.ring_costs {
            plan.report.ring_costmap = Some(
                serde_json::json!({"engine":papyrus_terrain::road_costmap::CELL_VERSION,
                "lattice":grid.lattice,"counts":grid.grid.counts,"work":grid.grid.work,
                "terrain_and_mesh_validated":false}),
            );
            plan.ring_lattice = Some(grid.lattice);
            plan.ring_source_costs = Some(grid.grid.costs);
        }
        plan
    } else {
        let ring_costs = if let Some(profile) = &recipe.ring_costmap {
            let Some(blockers) = profile
                .prepare_blockers(project, solution, catalog, bindings, &source, &cancelled)?
            else {
                return Ok(None);
            };
            let Some(blockers) = crate::road_cost_source::with_retained_placement(
                project,
                maps,
                rails,
                blockers,
                &cancelled,
            )?
            else {
                return Ok(None);
            };
            let Some(grid) = profile.build_family(
                &maps.layers,
                &blockers,
                project.seeds[3].value,
                false,
                &cancelled,
            )?
            else {
                return Ok(None);
            };
            Some(grid)
        } else {
            None
        };
        let placement = if let Some(profile) = &recipe.access_costmap {
            let Some(raster) = profile
                .prepare_blockers(project, solution, catalog, bindings, &source, &cancelled)?
            else {
                return Ok(None);
            };
            let Some(raster) = crate::road_cost_source::with_retained_placement(
                project,
                maps,
                rails,
                raster,
                &cancelled,
            )?
            else {
                return Ok(None);
            };
            Some(raster)
        } else {
            None
        };
        let Some(mut scratch) =
            monument_composition::compose(&maps.layers, &maps.alpha, &[], &cancelled)?
        else {
            return Ok(None);
        };
        scratch.rivers = maps.rivers.clone();
        scratch.ocean = maps.ocean.clone();
        let Some(report) = infrastructure::generate_with_preferences(
            project,
            solution,
            catalog,
            &source,
            bindings.installation_root(),
            &mut scratch,
            ring_costs.as_ref(),
            placement.as_deref(),
            preferences,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        let Some(plan) = Plan::from_report(report, &cancelled)? else {
            return Ok(None);
        };
        plan
    };
    if let Some(rails) = rails {
        let provenance = serde_json::json!({"scene_sha256":rails.scene_sha256,"phase":"pre-road retained rail reservations","policy":"source placement cost, traversable"});
        if let Some(ring) = &mut plan.report.ring_costmap {
            ring["retained_rail_placement"] = provenance.clone();
        }
        for family in &mut plan.report.source_access_costmaps {
            family["retained_rail_placement"] = provenance.clone();
        }
    }
    let base = AlignmentBase { maps, context: context.clone() };
    plan.context = Some(context);
    Ok(Some((plan, base)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raster_digest_preserves_signed_little_endian_words_and_cancellation() {
        let height = (0..4097).map(|i| i as i16 - 2300).collect::<Vec<_>>();
        let topology = (0..8193).map(|i| i * -131_071_i32).collect::<Vec<_>>();
        let mut expected = Sha256::new();
        for &v in &height {
            expected.update(v.to_le_bytes());
        }
        for &v in &topology {
            expected.update(v.to_le_bytes());
        }
        let mut hash = Sha256::new();
        hash_raster_words(&mut hash, &height, |v| v.to_le_bytes(), &|| false).unwrap();
        hash_raster_words(&mut hash, &topology, |v| v.to_le_bytes(), &|| false).unwrap();
        assert_eq!(hash.finalize(), expected.finalize());
        let calls = std::cell::Cell::new(0);
        let mut cancelled = Sha256::new();
        assert!(hash_raster_words(&mut cancelled, &height, |v| v.to_le_bytes(), &|| {
            calls.set(calls.get() + 1);
            calls.get() == 2
        }).is_none());
        assert_eq!(calls.get(), 2);
    }
    fn alignment_fixture() -> (Plan, Composed, papyrus_terrain::native_layers::Layers) {
        use papyrus_terrain::native_layers::Layers;
        let (hr, sr) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![18000; hr * hr],
            biome: vec![0; sr * sr * 5],
            splat: vec![0; sr * sr * 8],
            topology: vec![0; sr * sr],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        let maps = monument_composition::compose(&layers, &vec![255; sr * sr], &[], || false)
            .unwrap()
            .unwrap();
        let mut report = crate::road_alignment::tests::fixture();
        report.roads.push(road_network::Road {
            from: road_network::Destination::Loop,
            to: road_network::Destination::Loop,
            points: report.roads[0]
                .points
                .iter()
                .map(|p| [p[0], p[1], p[2] + 100.])
                .collect(),
        });
        report.families[0].road_count = 2;
        let mut plan = Plan::from_report(report, || false).unwrap().unwrap();
        plan.context = Some(std::sync::Arc::new(InputContext {
            rails: None,
            project: Project::default(),
            solution: Solution {
                outcomes: vec![],
                required_missing: 0,
                attempts: 0,
            },
            catalog: "c".repeat(64),
            bindings: "b".repeat(64),
            revision: revision(&maps, &|| false).unwrap(),
            rivers: None,
        }));
        (plan, maps, layers)
    }
    #[test]
    fn immutable_alignment_base_reuses_validation_without_authorizing_stale_contexts_or_proposals()
    {
        use std::cell::Cell;
        let (mut plan, mut maps, layers) = alignment_fixture();
        let make = |plan: &Plan| {
            crate::road_alignment::propose(
                plan.planning_report(),
                crate::road_refinement::AlignmentWitness {
                    kind: crate::road_refinement::AlignmentKind::Grade,
                    road: 0,
                    position: [0.; 2],
                },
                papyrus_terrain::path_terrain::Grid {
                    size: 1000.,
                    resolution: layers.height_resolution,
                    origin_y: -500.,
                    vertical_span: 1000.,
                    height: &layers.height,
                },
                0.5,
                1_000_000,
                || false,
            )
            .unwrap()
            .unwrap()
        };
        let validation_calls = Cell::new(0);
        let token = plan
            .alignment_base(&maps, || {
                validation_calls.set(validation_calls.get() + 1);
                false
            })
            .unwrap()
            .unwrap();
        assert!(validation_calls.get() > 100);
        assert!(std::sync::Arc::get_mut(plan.context.as_mut().unwrap()).is_none());
        assert!(token.matches(plan.context.as_ref().unwrap(), &maps));
        let preparation = plan.alignment_preparation_work().unwrap();
        assert!(preparation < ALIGNMENT_BASE_VALIDATION_WORK);
        let original = plan.report.json();
        assert!(
            plan.alignment_trial_checked(make(&plan), &token, preparation - 1, || false)
                .is_err()
        );
        assert_eq!(plan.report.json(), original);
        let (mut other, other_maps, _) = alignment_fixture();
        assert!(!token.matches(plan.context.as_ref().unwrap(), &other_maps));
        assert!(!token.matches(other.context.as_ref().unwrap(), &maps));
        assert!(
            other
                .alignment_trial_checked(make(&other), &token, preparation, || false)
                .is_err()
        );
        let stale = make(&plan);
        let cached_calls = Cell::new(0);
        let trial = plan
            .alignment_trial_checked(make(&plan), &token, preparation, || {
                cached_calls.set(cached_calls.get() + 1);
                false
            })
            .unwrap()
            .unwrap();
        assert!(cached_calls.get() < validation_calls.get() / 8);
        assert!(trial.commit(|| false));
        let committed = plan.report.json();
        assert_ne!(committed, original);
        assert!(
            plan.alignment_trial_checked(stale, &token, preparation, || false)
                .is_err()
        );
        let trial = plan
            .alignment_trial_checked(make(&plan), &token, preparation, || false)
            .unwrap()
            .unwrap();
        drop(trial);
        assert_eq!(plan.report.json(), committed);
        assert!(
            plan.alignment_trial_checked(make(&plan), &token, preparation, || true)
                .unwrap()
                .is_none()
        );
        drop(token);
        assert!(std::sync::Arc::get_mut(plan.context.as_mut().unwrap()).is_some());
        maps.layers.height[0] += 1;
        assert!(plan.alignment_base(&maps, || false).is_err());
        assert_eq!(plan.report.json(), committed);
    }
    #[test]
    fn alignment_trial_rolls_back_geometry_seals_and_cancelled_commit() {
        use papyrus_terrain::path_terrain::Grid;
        let (mut plan, mut maps, layers) = alignment_fixture();
        let hr = layers.height_resolution;
        let original = plan.report.json();
        let seal = plan.selection_binding(&[], || false).unwrap().unwrap();
        for cancel_commit in [false, true] {
            let proposal = crate::road_alignment::propose(
                plan.planning_report(),
                crate::road_refinement::AlignmentWitness {
                    kind: crate::road_refinement::AlignmentKind::Grade,
                    road: 0,
                    position: [0.; 2],
                },
                Grid {
                    size: 1000.,
                    resolution: hr,
                    origin_y: -500.,
                    vertical_span: 1000.,
                    height: &layers.height,
                },
                0.5,
                1_000_000,
                || false,
            )
            .unwrap()
            .unwrap();
            let trial = plan
                .alignment_trial(proposal, &maps, || false)
                .unwrap()
                .unwrap();
            assert_ne!(
                trial.selection_binding(&[], || false).unwrap().unwrap(),
                seal
            );
            if cancel_commit {
                assert!(!trial.commit(|| true));
            } else {
                drop(trial);
            }
            assert_eq!(plan.report.json(), original);
            assert_eq!(
                plan.selection_binding(&[], || false).unwrap().unwrap(),
                seal
            );
        }
        let make = |report: &road_network::Report| {
            crate::road_alignment::propose(
                report,
                crate::road_refinement::AlignmentWitness {
                    kind: crate::road_refinement::AlignmentKind::Grade,
                    road: 0,
                    position: [0.; 2],
                },
                Grid {
                    size: 1000.,
                    resolution: hr,
                    origin_y: -500.,
                    vertical_span: 1000.,
                    height: &layers.height,
                },
                0.5,
                1_000_000,
                || false,
            )
            .unwrap()
            .unwrap()
        };
        let proposal = make(plan.planning_report());
        {
            let mut parent = plan
                .alignment_trial(proposal, &maps, || false)
                .unwrap()
                .unwrap();
            let child_proposal = crate::road_alignment::propose(
                parent.planning_report(),
                crate::road_refinement::AlignmentWitness {
                    kind: crate::road_refinement::AlignmentKind::Grade,
                    road: 1,
                    position: [0., 100.],
                },
                Grid {
                    size: 1000.,
                    resolution: hr,
                    origin_y: -500.,
                    vertical_span: 1000.,
                    height: &layers.height,
                },
                0.5,
                1_000_000,
                || false,
            )
            .unwrap()
            .unwrap();
            assert!(
                parent
                    .alignment_trial(child_proposal, &maps, || false)
                    .unwrap()
                    .unwrap()
                    .commit(|| false)
            );
            assert_ne!(parent.report.roads[1].points[30], [0., 0., 100.]);
        }
        assert_eq!(plan.report.json(), original);
        assert_eq!(
            plan.selection_binding(&[], || false).unwrap().unwrap(),
            seal
        );
        let proposal = make(plan.planning_report());
        maps.layers.height[0] += 1;
        assert!(plan.alignment_trial(proposal, &maps, || false).is_err());
        maps.layers.height[0] -= 1;
        let proposal = make(plan.planning_report());
        assert!(
            plan.alignment_trial(proposal, &maps, || false)
                .unwrap()
                .unwrap()
                .commit(|| false)
        );
        assert_ne!(plan.report.json(), original);
        assert_ne!(
            plan.selection_binding(&[], || false).unwrap().unwrap(),
            seal
        );
    }
    fn report() -> road_network::Report {
        let mut report = road_network::Report::default();
        for h in 0..3 {
            report.roads.push(road_network::Road {
                from: road_network::Destination::Loop,
                to: road_network::Destination::Loop,
                points: vec![[h as f32 * 50., 10., 0.], [h as f32 * 50., 10., 100.]],
            });
            report.families.push(road_network::Family {
                infrastructure_type: if h == 2 { 2 } else { 0 },
                hierarchy: h,
                seed: 1,
                first_road: h as usize,
                road_count: 1,
                socket_count: 0,
                unresolved: vec![],
                width: 10.,
                splat_channel: 7,
            });
        }
        report
    }
    #[test]
    fn source_cell_cost_receipts_retain_family_identity_and_reject_missing_or_foreign_grids() {
        use papyrus_terrain::road_costmap::{CELL_VERSION, CellGrid, CellLattice, Counts, Grid};
        let mut plan = Plan::from_report(report(), || false).unwrap().unwrap();
        assert!(plan.source_cost_grid(1, 1).unwrap().is_none());
        plan.report.source_layout =
            Some(serde_json::json!({"engine":crate::source_road_handoff::VERSION}));
        assert!(plan.source_cost_grid(1, 1).is_err());
        let lattice = CellLattice::new(1000., 125.).unwrap();
        let make = |hierarchy| crate::source_road_planning::FamilyCosts {
            hierarchy,
            grid: CellGrid {
                lattice,
                grid: Grid {
                    engine: CELL_VERSION,
                    resolution: 8,
                    seed: 1,
                    trail: hierarchy == 2,
                    counts: Counts::default(),
                    work: 0,
                    costs: vec![1; 64],
                },
            },
        };
        plan.source_family_costs = vec![make(1), make(2)];
        plan.ring_lattice = Some(lattice);
        plan.ring_source_costs = Some(vec![1; 64]);
        for h in 0..3 {
            let (l, c) = plan.source_cost_grid(h, 1).unwrap().unwrap();
            assert_eq!(l.resolution(), 8);
            assert_eq!(c.len(), 64);
        }
        assert!(plan.source_cost_grid(1, 2).is_err());
        plan.source_family_costs[1].grid.grid.trail = false;
        assert!(plan.source_cost_grid(2, 1).is_err());
        plan.source_family_costs[1].grid.grid.trail = true;
        plan.source_family_costs.push(make(1));
        assert!(plan.source_cost_grid(1, 1).is_err());
        plan.source_family_costs.pop();
        plan.source_family_costs[0].grid.grid.resolution = 9;
        assert!(plan.source_cost_grid(1, 1).is_err());
    }
    #[test]
    fn socket_trial_rolls_back_selection_seals_and_commits_only_explicitly() {
        socket_trial_case(false);
    }
    #[test]
    fn retained_rail_scene_seals_socket_trials_and_detects_reservation_changes() {
        socket_trial_case(true);
    }
    fn socket_trial_case(retaining: bool) {
        use papyrus_terrain::{
            erosion,
            native_layers::Layers,
            road_network::{Destination, LaunchEngine, LaunchPolicy, socket_recovery},
        };
        let (hr, sr) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let h = 17039_i16;
        let y = erosion::decode(h);
        let layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![h; hr * hr],
            biome: vec![0; sr * sr * 5],
            splat: vec![0; sr * sr * 8],
            topology: vec![0; sr * sr],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        let alpha = vec![255; sr * sr];
        let mut maps = monument_composition::compose(&layers, &alpha, &[], || false)
            .unwrap()
            .unwrap();
        let p = |x, z| [x, y, z];
        let mut r = report();
        r.roads[0].points = vec![p(-200., 0.), p(200., 0.)];
        r.roads[1].points = vec![p(0., 0.), p(40., 0.), p(80., 0.), p(120., 0.), p(200., 0.)];
        r.roads[1].from = Destination::Monument {
            monument: 1,
            connector: 11,
        };
        r.roads[1].to = Destination::Network { road: 0, point: 1 };
        r.roads[2].points = vec![
            p(-100., 100.),
            p(-60., 100.),
            p(-20., 100.),
            p(20., 100.),
            p(80., 80.),
            p(140., 40.),
            p(200., 0.),
        ];
        r.roads[2].from = Destination::Monument {
            monument: 2,
            connector: 22,
        };
        r.roads[2].to = Destination::Network { road: 0, point: 1 };
        r.families[2].hierarchy = 1;
        let launch = LaunchPolicy {
            engine: LaunchEngine::GlobalEarthwork,
            minimum_run_widths: 1.,
            sample_spacing: 1.,
            maximum_work: 2_000_000,
        };
        r.entrance_launch = Some(launch);
        r.entrance_joins.push(road_network::EntranceJoin {
            monument: 1,
            connector: 11,
            points: r.roads[1].points[..3].to_vec(),
        });
        r.entrance_joins.push(road_network::EntranceJoin {
            monument: 2,
            connector: 22,
            points: r.roads[2].points[..3].to_vec(),
        });
        let mut plan = Plan::from_report(r, || false).unwrap().unwrap();
        plan.context = Some(std::sync::Arc::new(InputContext {
            rails: None,
            project: Project::default(),
            solution: Solution {
                outcomes: vec![],
                required_missing: 0,
                attempts: 0,
            },
            catalog: "c".repeat(64),
            bindings: "b".repeat(64),
            revision: revision(&maps, &|| false).unwrap(),
            rivers: None,
        }));
        if retaining {
            let (recipe, scene) = crate::retained_rail_scene::tests::fixture(sr);
            let receipt =
                crate::retained_rail_scene::Receipt::capture(&scene, &recipe, sr, || false)
                    .unwrap()
                    .unwrap();
            maps.rails = Some(scene);
            let context = std::sync::Arc::get_mut(plan.context.as_mut().unwrap()).unwrap();
            context.project.rails = Some(recipe);
            context.rails = Some(receipt);
            assert!(context.check_rails(&maps, &|| false).unwrap().is_some());
            maps.rails.as_mut().unwrap().reservations.raster[0] = 1;
            assert!(context.check_rails(&maps, &|| false).is_err());
            maps.rails.as_mut().unwrap().reservations.raster[0] = 0;
            maps.rails.as_mut().unwrap().tracks[0].preserve_start = true;
            assert!(context.check_rails(&maps, &|| false).is_err());
            maps.rails.as_mut().unwrap().tracks[0].preserve_start = false;
            assert!(context.check_rails(&maps, &|| false).unwrap().is_some());
        }
        let owners = [
            road_network::Obstacle {
                monument: 1,
                center: [0., 0.],
                extent: [30., 30.],
                yaw: 0.,
            },
            road_network::Obstacle {
                monument: 2,
                center: [-100., 100.],
                extent: [30., 30.],
                yaw: 0.,
            },
        ];
        let environment = socket_recovery::Environment {
            height_origin: -500.,
            vertical_span: 1000.,
            layers: &layers,
            alpha: &alpha,
            obstacles: &[],
            owners: &owners,
            settings: road_network::Settings {
                seed: 1,
                width: 10.,
                shoulder: 2.,
                maximum_grade: 0.2,
                maximum_cut_fill: 8.,
                terrain_offset: 0.,
                road_topology: 2048,
                roadside_topology: 4096,
                protected_topology: 1024,
                splat_channel: 7,
            },
            ownership: road_network::EntrancePolicy {
                minimum_height: 1.,
                owned_topology: 1024,
                maximum_exit_distance: 512.,
                maximum_exit_nodes: 4096,
            },
            launch,
            costs: None,
            cost_lattice: None,
            water: None,
        };
        let key = socket_recovery::Key {
            monument: 1,
            connector: 11,
        };
        let original = plan.report.json();
        let seal = plan.selection_binding(&[], || false).unwrap().unwrap();
        // A child commit must remain private to its parent's rollback scope.
        // Test two distinct sockets, both authored join chains, selection seals,
        // child cancellation and cancellation after the child committed.
        for mode in 0..4 {
            let proposal = socket_recovery::propose(
                plan.planning_report(),
                &environment,
                key,
                -6.,
                2_000_000,
                || false,
            )
            .unwrap()
            .unwrap();
            let mut parent = plan
                .socket_trial(proposal, &maps, || false)
                .unwrap()
                .unwrap();
            let parent_report = parent.planning_report().json();
            let parent_seal = parent.selection_binding(&[], || false).unwrap().unwrap();
            let second = socket_recovery::Key {
                monument: 2,
                connector: 22,
            };
            let proposal = socket_recovery::propose(
                parent.planning_report(),
                &environment,
                second,
                2.,
                2_000_000,
                || false,
            )
            .unwrap()
            .unwrap();
            {
                let child = parent
                    .socket_trial(proposal, &maps, || false)
                    .unwrap()
                    .unwrap();
                assert_eq!(child.planning_report().socket_geometry_recovery.len(), 2);
                assert_ne!(
                    child.selection_binding(&[], || false).unwrap().unwrap(),
                    parent_seal
                );
                if mode == 1 || mode == 3 {
                    assert!(child.commit(|| false).is_some());
                } else if mode == 2 {
                    assert!(child.commit(|| true).is_none());
                }
            }
            if mode == 0 || mode == 2 {
                assert_eq!(parent.planning_report().json(), parent_report);
                assert_eq!(
                    parent.selection_binding(&[], || false).unwrap().unwrap(),
                    parent_seal
                );
            } else {
                assert_eq!(parent.planning_report().socket_geometry_recovery.len(), 2);
            }
            if mode == 3 {
                assert!(parent.commit(|| true).is_none());
            } else {
                drop(parent);
            }
            assert_eq!(plan.report.json(), original);
            assert_eq!(
                plan.selection_binding(&[], || false).unwrap().unwrap(),
                seal
            );
        }
        let proposal = socket_recovery::propose(
            plan.planning_report(),
            &environment,
            key,
            -6.,
            2_000_000,
            || false,
        )
        .unwrap()
        .unwrap();
        {
            let trial = plan
                .socket_trial(proposal, &maps, || false)
                .unwrap()
                .unwrap();
            assert_ne!(
                trial.selection_binding(&[], || false).unwrap().unwrap(),
                seal
            );
            assert_eq!(trial.planning_report().socket_geometry_recovery.len(), 1);
        }
        assert_eq!(plan.report.json(), original);
        assert_eq!(
            plan.selection_binding(&[], || false).unwrap().unwrap(),
            seal
        );
        let proposal = socket_recovery::propose(
            plan.planning_report(),
            &environment,
            key,
            -6.,
            2_000_000,
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(
            plan.socket_trial(proposal, &maps, || false)
                .unwrap()
                .unwrap()
                .commit(|| true)
                .is_none()
        );
        assert_eq!(plan.report.json(), original);
        let proposal = socket_recovery::propose(
            plan.planning_report(),
            &environment,
            key,
            -6.,
            2_000_000,
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(
            plan.socket_trial(proposal, &maps, || false)
                .unwrap()
                .unwrap()
                .commit(|| false)
                .is_some()
        );
        assert_ne!(plan.report.json(), original);
        assert_ne!(
            plan.selection_binding(&[], || false).unwrap().unwrap(),
            seal
        );
        let proposal = socket_recovery::propose(
            plan.planning_report(),
            &environment,
            key,
            2.,
            2_000_000,
            || false,
        )
        .unwrap()
        .unwrap();
        maps.layers.height[0] += 1;
        let before = plan.report.json();
        assert!(plan.socket_trial(proposal, &maps, || false).is_err());
        assert_eq!(plan.report.json(), before);
    }
    #[test]
    fn retained_ring_costs_are_validated_once_and_legacy_plans_keep_their_policy() {
        let make = |engine| {
            let mut report = report();
            report.entrance_launch = Some(road_network::LaunchPolicy {
                engine,
                minimum_run_widths: 1.,
                sample_spacing: 1.,
                maximum_work: 2_000_000,
            });
            report.ring_costmap =
                Some(serde_json::json!({"grid":{"resolution":257,"costs":vec![101;257*257]}}));
            report
        };
        let plan = Plan::from_report(make(road_network::LaunchEngine::GlobalEarthwork), || false)
            .unwrap()
            .unwrap();
        let (launch, costs) = plan.checked_ring_costs().unwrap();
        assert_eq!(launch.sample_spacing, 1.);
        assert_eq!(costs.len(), 257 * 257);
        assert!(costs.iter().all(|v| *v == 101));
        let mut bad = make(road_network::LaunchEngine::GlobalEarthwork);
        bad.ring_costmap.as_mut().unwrap()["grid"]["costs"][0] = serde_json::json!(1_000_001);
        assert!(Plan::from_report(bad, || false).is_err());
        let mut legacy = make(road_network::LaunchEngine::SurfaceChecked);
        legacy.ring_costmap = Some(serde_json::json!({"legacy":"opaque"}));
        assert!(
            Plan::from_report(legacy, || false)
                .unwrap()
                .unwrap()
                .checked_ring_costs()
                .is_none()
        );
        let plain = Plan::from_report(report(), || false).unwrap().unwrap();
        assert!(plain.checked_ring_costs().is_none());
    }
    #[test]
    fn retained_indices_and_source_families_reach_roadside_candidates_without_reordering() {
        let plan = Plan::from_report(report(), || false).unwrap().unwrap();
        let roads = plan.roads();
        assert_eq!(
            roads.iter().map(|r| r.hierarchy).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        for (road, original) in roads.iter().zip(&plan.planning_report().roads) {
            assert_eq!(road.path.points(), original.points);
        }
        let proposals = papyrus_terrain::roadside_monuments::generate(
            &[[-10., 0., 10.], [10., 0., 10.]],
            &roads,
            papyrus_terrain::roadside_monuments::RoadMode::SideRoadOrRingRoad,
            papyrus_terrain::roadside_monuments::Settings {
                sample_spacing: 5.,
                end_padding: 5.,
                maximum_work: 1000,
                maximum_candidates: 1000,
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(proposals.candidates.iter().any(|c| c.road == 0));
        assert!(proposals.candidates.iter().any(|c| c.road == 1));
        assert!(proposals.candidates.iter().all(|c| c.road != 2));
    }
    #[test]
    fn straightening_keeps_road_families_joins_indices_and_private_inputs() {
        let mut input = report();
        input.roads[0].points = vec![
            [0., 10., 0.],
            [10., 13., 8.],
            [20., 12., 4.],
            [30., 10., 0.],
        ];
        let plan = Plan::from_report(input, || false).unwrap().unwrap();
        let span = papyrus_terrain::roadside_selection::Selected {
            group: 10,
            request: 42,
            candidate: 0,
            road: 0,
            start_index: 0,
            end_index: 3,
            position: [0.; 3],
            yaw: 0.,
            score: 1.,
        };
        let selected = vec![span];
        let before = serde_json::to_vec(&plan.curves).unwrap();
        let binding = plan
            .selection_binding(&selected, || false)
            .unwrap()
            .unwrap();
        let adjusted = plan
            .straighten_selected(&selected, 1000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(adjusted.paths.len(), 3);
        assert_eq!(adjusted.hierarchy, plan.hierarchy);
        assert_eq!(adjusted.changed_controls, 2);
        assert_eq!(adjusted.paths[0].points()[1], [10., 10., 0.]);
        assert_eq!(adjusted.paths[0].points()[2], [20., 10., 0.]);
        for i in [1, 2] {
            assert!(std::ptr::eq(adjusted.paths[i].as_ref(), &plan.curves[i]));
            assert_eq!(
                serde_json::to_vec(&adjusted.paths[i]).unwrap(),
                serde_json::to_vec(&plan.curves[i]).unwrap()
            );
        }
        assert_eq!(serde_json::to_vec(&plan.curves).unwrap(), before);
        let mut changed = selected.clone();
        changed[0].yaw = 1.;
        assert_ne!(
            plan.selection_binding(&changed, || false).unwrap().unwrap(),
            binding
        );
        let mut changed_plan = Plan::from_report(report(), || false).unwrap().unwrap();
        changed_plan.curves[0] = adjusted.paths[0].as_ref().clone();
        assert_ne!(
            changed_plan
                .selection_binding(&selected, || false)
                .unwrap()
                .unwrap(),
            binding
        );
        assert_eq!(
            plan.selection_binding(&selected, || false)
                .unwrap()
                .unwrap(),
            binding
        );
        assert!(
            plan.selection_binding(&selected, || true)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn bad_or_cancelled_spans_never_publish_a_partial_route_set() {
        let plan = Plan::from_report(report(), || false).unwrap().unwrap();
        let span = papyrus_terrain::roadside_selection::Selected {
            group: 10,
            request: 42,
            candidate: 0,
            road: 0,
            start_index: 0,
            end_index: 1,
            position: [0.; 3],
            yaw: 0.,
            score: 1.,
        };
        let before = serde_json::to_vec(&plan.curves).unwrap();
        assert!(
            plan.straighten_selected(&[span.clone(), span.clone()], 1000, || false)
                .is_err()
        );
        let mut bad = span.clone();
        bad.road = 3;
        assert!(plan.straighten_selected(&[bad], 1000, || false).is_err());
        let mut bad = span.clone();
        bad.end_index = 2;
        assert!(
            plan.straighten_selected(&[span.clone(), bad], 1000, || false)
                .is_err()
        );
        assert!(
            plan.straighten_selected(std::slice::from_ref(&span), 4, || false)
                .is_err()
        );
        let calls = std::cell::Cell::new(0);
        assert!(
            plan.straighten_selected(&[span], 1000, || {
                calls.set(calls.get() + 1);
                calls.get() > 5
            })
            .unwrap()
            .is_none()
        );
        assert_eq!(serde_json::to_vec(&plan.curves).unwrap(), before);
    }
    #[test]
    fn moved_junctions_follow_dependency_order_without_moving_monument_endpoints() {
        let mut input = report();
        input.roads[0].points = vec![
            [0., 10., 0.],
            [10., 10., 8.],
            [20., 10., 4.],
            [30., 10., 8.],
            [40., 10., 0.],
        ];
        input.roads[1].points = vec![[100., 10., 100.], [60., 10., 70.], [20., 10., 4.]];
        input.roads[1].from = road_network::Destination::Monument {
            monument: 777,
            connector: 1,
        };
        input.roads[1].to = road_network::Destination::Network { road: 0, point: 2 };
        input.roads[2].points = vec![[0., 10., 100.], [10., 10., 50.], [20., 10., 4.]];
        input.roads[2].to = road_network::Destination::Network { road: 1, point: 2 };
        let mut plan = Plan::from_report(input, || false).unwrap().unwrap();
        let original = serde_json::to_vec(&plan.curves).unwrap();
        let span = papyrus_terrain::roadside_selection::Selected {
            group: 10,
            request: 42,
            candidate: 0,
            road: 0,
            start_index: 0,
            end_index: 4,
            position: [0.; 3],
            yaw: 0.,
            score: 1.,
        };
        let adjusted = plan
            .straighten_selected(std::slice::from_ref(&span), 1000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(adjusted.junction_moves.len(), 2);
        assert_eq!(adjusted.junction_moves[0].road, 1);
        assert_eq!(adjusted.junction_moves[1].road, 2);
        assert_eq!(adjusted.paths[1].points()[2], adjusted.paths[0].points()[2]);
        assert_eq!(adjusted.paths[2].points()[2], adjusted.paths[1].points()[2]);
        assert_eq!(adjusted.paths[1].points()[0], plan.curves[1].points()[0]);
        assert_eq!(adjusted.paths[1].points()[1], plan.curves[1].points()[1]);
        assert_eq!(adjusted.changed_controls, 5);
        assert!(!adjusted.final_access_validated);
        let mut conflict = span.clone();
        conflict.request = 43;
        conflict.road = 1;
        conflict.start_index = 0;
        conflict.end_index = 2;
        assert!(
            plan.straighten_selected(&[span.clone(), conflict], 1000, || false)
                .unwrap_err()
                .contains("re-selection")
        );
        assert!(
            plan.straighten_selected(std::slice::from_ref(&span), adjusted.work - 1, || false)
                .is_err()
        );
        let binding = plan
            .selection_binding(std::slice::from_ref(&span), || false)
            .unwrap()
            .unwrap();
        plan.report.roads[2].to = road_network::Destination::Network { road: 2, point: 0 };
        assert_ne!(
            plan.selection_binding(std::slice::from_ref(&span), || false)
                .unwrap()
                .unwrap(),
            binding
        );
        assert!(
            plan.straighten_selected(std::slice::from_ref(&span), 1000, || false)
                .is_err()
        );
        plan.report.roads[2].to = road_network::Destination::Network { road: 0, point: 0 };
        assert!(plan.straighten_selected(&[span], 1000, || false).is_err());
        assert_eq!(serde_json::to_vec(&plan.curves).unwrap(), original);
    }
    #[test]
    fn malformed_family_coverage_and_cancellation_cannot_publish_partial_plans() {
        let mut bad = report();
        bad.families[1].first_road = 0;
        assert!(Plan::from_report(bad, || false).is_err());
        let mut bad = report();
        bad.families.pop();
        assert!(Plan::from_report(bad, || false).is_err());
        let mut bad = report();
        bad.families[2].road_count = 99;
        assert!(Plan::from_report(bad, || false).is_err());
        assert!(Plan::from_report(report(), || true).unwrap().is_none());
    }
}
