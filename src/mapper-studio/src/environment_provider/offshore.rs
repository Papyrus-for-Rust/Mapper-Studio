//! Integrated independent offshore search with a read-only proposal boundary.
//! The spawning adapter consumes winners and supplies their final instance frames.
use super::*;
use crate::{
    monument_attributes::{WaterBody, WaterTrigger},
    offshore_decor, offshore_source,
    placement_water::{LookupState, Selection, Trigger},
};
use papyrus_terrain::{offshore_search as search, offshore_tiers as tiers};
use std::cell::{Cell, RefCell};
pub const VERSION: &str = "papyrus-offshore-stage-reference-1";

pub struct Execution {
    pub size: u32,
    pub realm_mask: i32,
    /// Whole-world cached execution, never a hit in Mapper's terrain cache.
    pub world_cached: bool,
    pub seed: u32,
    pub passes: usize,
    pub attempts_per_prefab: usize,
}
pub enum Preparation {
    Ready(Stage),
    Skipped,
    Stopped(Verdict),
}
struct Entry {
    prefab: search::Prefab,
    path: String,
    decor: offshore_decor::Program,
    bounds: tiers::Bounds,
    tier_mask: u32,
    minimum_corners: usize,
    environment: Candidate,
    water: WaterCandidate,
}
pub struct Stage {
    context: scene::Context,
    stage: i64,
    inputs_sha256: String,
    stage_source_sha256: String,
    size: u32,
    seed: u32,
    policy: search::Policy,
    entries: Vec<Entry>,
    preparation_work: usize,
}
#[derive(Clone, Copy)]
pub struct Terrain<'a> {
    pub rect: search::TerrainRect,
    pub topology: &'a [i32],
    pub resolution: usize,
    pub minimum_y: f32,
}
/// Disjoint allowances keep search work plus callbacks' modeled validation work
/// below their combined cap. Callback implementations must bound their own work.
#[derive(Clone, Copy)]
pub struct Limits {
    pub prefix: usize,
    pub search: usize,
    pub checks: usize,
}
pub trait Sampling {
    fn height(&mut self, point: [f32; 2]) -> Result<f32, String>;
    /// Source-shaped water query without waves or volume promotion.
    fn surface_water(&mut self, prefab: &str, point: [f32; 3]) -> Result<f32, String>;
    fn lookup_state(&mut self, _prefab: &str) -> LookupState {
        LookupState::Unmodeled
    }
    fn selection(&mut self, _prefab: &str) -> Selection {
        Selection::Unknown
    }
    fn body_y(&mut self, _prefab: &str, _body: &WaterBody) -> Result<Option<f32>, String> {
        Ok(None)
    }
    fn trigger(&mut self, _prefab: &str, _reference: &WaterTrigger) -> Result<Trigger, String> {
        Ok(Trigger::Unknown)
    }
}
#[derive(Debug)]
pub enum Failure {
    Invalid(String),
    Sampling(String),
    Missing(&'static str),
    Environment(Verdict),
}
#[derive(Debug)]
pub struct Run {
    pub engine: &'static str,
    pub stage: i64,
    pub stage_source_sha256: String,
    pub preparation_work: usize,
    pub prefix_work: usize,
    pub checks_work: usize,
    pub tier_rejections: usize,
    pub lake_rejections: usize,
    pub environment_rejections: usize,
    pub outcome: search::Outcome,
}
struct Meter {
    used: Cell<usize>,
    limit: usize,
}
impl Meter {
    fn remaining(&self) -> Result<usize, Failure> {
        self.limit
            .checked_sub(self.used.get())
            .filter(|n| *n > 0)
            .ok_or(Failure::Environment(Verdict::Budget))
    }
    fn spend(&self, n: usize) -> Result<(), Failure> {
        let next = self
            .used
            .get()
            .checked_add(n)
            .filter(|v| *v <= self.limit)
            .ok_or(Failure::Environment(Verdict::Budget))?;
        self.used.set(next);
        Ok(())
    }
}
impl Source {
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_offshore(
        &self,
        catalog: &Catalog,
        stages: &offshore_source::Source,
        request: &Request,
        execution: Execution,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Preparation, String> {
        if request.context.sources_sha256 != self.sha256
            || catalog.sha256 != self.catalog_sha256
            || !(1..=MAX_WORK).contains(&maximum_work)
            || execution.seed == 0
            || execution.seed > i32::MAX as u32
        {
            return Err("Offshore preparation source, request or allowance differs".into());
        }
        let stage = stages
            .stages
            .iter()
            .find(|s| s.object_id == request.stage)
            .ok_or("Unknown offshore stage")?;
        let policy = stage.policy(execution.passes, execution.attempts_per_prefab)?;
        let runs = stage.runs(execution.size, execution.realm_mask)?;
        if cancelled() {
            return Ok(Preparation::Stopped(Verdict::Cancelled));
        }
        if execution.world_cached || !runs {
            return Ok(Preparation::Skipped);
        }
        if self.water.is_none() {
            return Err("No inspected offshore water/placement runtime adapter".into());
        }
        let roster = stages.roster(request.stage, catalog)?;
        if roster.is_empty() {
            return Err("Offshore roster coverage is unavailable".into());
        }
        let mut work = 0;
        let mut entries = vec![];
        for entry in roster {
            if let Err(v) = charge(&mut work, 1, maximum_work, &cancelled) {
                return Ok(Preparation::Stopped(v));
            }
            let prefab = self
                .templates
                .prefabs
                .get(&entry.path)
                .ok_or("Offshore prefab capture is missing")?;
            prefab.rules.validate()?;
            if !prefab.rules.unsupported.is_empty() {
                return Err("Offshore prefab has unsupported source attributes".into());
            }
            let bounds = catalog
                .entries
                .get(&entry.path)
                .and_then(|a| a.bounds.as_ref())
                .ok_or("Offshore authored bounds are missing")?;
            let decor = offshore_decor::Program::new(self.attributes(&entry.path)?)?;
            if work == maximum_work {
                return Ok(Preparation::Stopped(Verdict::Budget));
            }
            let environment =
                match self.prepare_candidate(&entry.path, maximum_work - work, &cancelled)? {
                    Ok(value) => value,
                    Err(v) => return Ok(Preparation::Stopped(v)),
                };
            if let Err(v) = charge(
                &mut work,
                environment.preparation_work_reserved(),
                maximum_work,
                &cancelled,
            ) {
                return Ok(Preparation::Stopped(v));
            }
            let water = self
                .prepare_water(&entry.path)?
                .ok_or("Missing offshore water adapter")?;
            entries.push(Entry {
                prefab: entry.prefab,
                path: entry.path,
                decor,
                bounds: tiers::Bounds {
                    center: bounds.center,
                    extent: bounds.extent,
                },
                tier_mask: prefab.rules.tier_mask,
                minimum_corners: prefab.rules.min_corners,
                environment,
                water,
            });
        }
        if cancelled() {
            return Ok(Preparation::Stopped(Verdict::Cancelled));
        }
        Ok(Preparation::Ready(Stage {
            context: request.context.clone(),
            stage: request.stage,
            inputs_sha256: request.inputs_sha256.clone(),
            stage_source_sha256: stages.sha256.clone(),
            size: execution.size,
            seed: execution.seed,
            policy,
            entries,
            preparation_work: work,
        }))
    }
}
impl Stage {
    pub fn prefab_path(&self, id: u64) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| e.prefab.id == id)
            .map(|e| e.path.as_str())
    }
    /// The owner binds terrain revision, installed stage receipt, seed and execution
    /// policy into its generation request. Winners go to the spawning adapter.
    pub fn generate(
        &self,
        snapshot: &scene::Snapshot,
        request: &Request,
        terrain: Terrain<'_>,
        limits: Limits,
        sampling: &mut impl Sampling,
        cancelled: impl Fn() -> bool,
    ) -> Result<Run, Failure> {
        if request.context != self.context
            || snapshot.context() != &self.context
            || request.stage != self.stage
            || request.inputs_sha256 != self.inputs_sha256
            || terrain.rect.size != self.size
            || !(1..=4096).contains(&terrain.resolution)
            || terrain.topology.len() != terrain.resolution * terrain.resolution
            || !terrain.minimum_y.is_finite()
            || terrain.minimum_y.abs() > 1_000_000.
            || limits.prefix == 0
            || limits.search == 0
            || limits.checks == 0
            || limits.search > 64_000_000
            || limits
                .prefix
                .checked_add(limits.search)
                .and_then(|n| n.checked_add(limits.checks))
                .is_none_or(|n| n > MAX_WORK)
        {
            return Err(Failure::Invalid(
                "Offshore stage inputs or disjoint allowances differ".into(),
            ));
        }
        let prefix = request
            .query_prefix(snapshot, limits.prefix, &cancelled)
            .map_err(Failure::Invalid)?
            .map_err(Failure::Environment)?;
        if !prefix.summary().declared_prefix_complete {
            return Err(Failure::Environment(Verdict::Incomplete));
        }
        let meter = Meter {
            used: Cell::new(0),
            limit: limits.checks,
        };
        let sampling = RefCell::new(sampling);
        let mut tier_rejections = 0;
        let mut lake_rejections = 0;
        let mut environment_rejections = 0;
        let prefabs = self.entries.iter().map(|e| e.prefab).collect::<Vec<_>>();
        let outcome = search::try_search(
            terrain.rect,
            self.policy,
            self.seed,
            &prefabs,
            limits.search,
            &cancelled,
            |point| {
                meter.spend(4)?;
                sampling
                    .borrow_mut()
                    .height(point)
                    .map_err(Failure::Sampling)
            },
            |prefab, position| {
                let entry = self
                    .entries
                    .iter()
                    .find(|e| e.prefab.id == prefab.id)
                    .ok_or(Failure::Missing("Unknown offshore prefab identity"))?;
                meter.spend(entry.decor.required_work())?;
                let pose = entry
                    .decor
                    .apply(
                        position,
                        terrain.minimum_y,
                        entry.decor.required_work(),
                        &cancelled,
                        |point, index| Ok(draw(self.seed, prefab.id, point, index)),
                        |point| sampling.borrow_mut().surface_water(&entry.path, point),
                    )
                    .map_err(Failure::Sampling)?
                    .map_err(Failure::Environment)?;
                let frame = Transform {
                    position: pose.position,
                    rotation: pose.rotation,
                    scale: pose.scale,
                };
                meter.spend(8)?;
                let corners =
                    tiers::midplane_corners(entry.bounds, frame).map_err(Failure::Invalid)?;
                let tier = tiers::assess(
                    terrain.rect,
                    terrain.topology,
                    terrain.resolution,
                    corners,
                    entry.tier_mask,
                    entry.minimum_corners,
                    &cancelled,
                )
                .map_err(Failure::Invalid)?
                .ok_or(Failure::Environment(Verdict::Cancelled))?;
                if !tier.accepted() {
                    tier_rejections += 1;
                    return Ok(search::Check::Rejected);
                }
                let state = sampling.borrow_mut().lookup_state(&entry.path);
                let selection = sampling.borrow_mut().selection(&entry.path);
                let water = entry
                    .water
                    .assess(
                        request,
                        state,
                        selection,
                        meter.remaining()?,
                        &cancelled,
                        |body| sampling.borrow_mut().body_y(&entry.path, body),
                        |reference| sampling.borrow_mut().trigger(&entry.path, reference),
                    )
                    .map_err(Failure::Sampling)?
                    .map_err(Failure::Environment)?;
                meter.spend(water.work)?;
                match tiers::water_verdict(pose.position[1], water.water)
                    .map_err(Failure::Invalid)?
                {
                    tiers::PlacementVerdict::Passed => {}
                    tiers::PlacementVerdict::LakeRejected => {
                        lake_rejections += 1;
                        return Ok(search::Check::Rejected);
                    }
                    _ => {
                        return Err(Failure::Missing(
                            "Runtime water lookup or bounds are unavailable",
                        ));
                    }
                }
                // This reference pipeline is restricted by prepare_offshore to
                // the reviewed client runtime (active offshore forbidden mask 33).
                let environment = entry
                    .environment
                    .assess(
                        snapshot,
                        request,
                        &prefix,
                        frame,
                        33,
                        0.,
                        meter.remaining()?,
                        &cancelled,
                    )
                    .map_err(Failure::Invalid)?;
                meter.spend(environment.work)?;
                match environment.verdict {
                    Verdict::Clear => Ok(search::Check::Accepted(pose)),
                    Verdict::Blocked(_) => {
                        environment_rejections += 1;
                        Ok(search::Check::Rejected)
                    }
                    other => Err(Failure::Environment(other)),
                }
            },
        )
        .map_err(|error| match error {
            search::SearchError::Invalid(s) => Failure::Invalid(s),
            search::SearchError::Callback(e) => e,
        })?;
        Ok(Run {
            engine: VERSION,
            stage: self.stage,
            stage_source_sha256: self.stage_source_sha256.clone(),
            preparation_work: self.preparation_work,
            prefix_work: prefix.summary().work,
            checks_work: meter.used.get(),
            tier_rejections,
            lake_rejections,
            environment_rejections,
            outcome,
        })
    }
}
fn draw(seed: u32, id: u64, point: [f32; 3], index: usize) -> f32 {
    let mut hash = Sha256::new();
    hash.update(VERSION.as_bytes());
    hash.update(seed.to_le_bytes());
    hash.update(id.to_le_bytes());
    for value in point {
        hash.update(value.to_bits().to_le_bytes());
    }
    hash.update((index as u64).to_le_bytes());
    let value = u32::from_le_bytes(hash.finalize()[..4].try_into().unwrap());
    (value >> 8) as f32 / 16_777_216.
}
#[cfg(test)]
mod tests;
