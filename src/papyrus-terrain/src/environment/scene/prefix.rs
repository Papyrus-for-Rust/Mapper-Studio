//! Generation-prefix queries are distinct from full-plan completion. A caller
//! must derive the execution order and inherited ceiling from verified sources.
use super::*;
pub const VERSION: &str = "papyrus-environment-prefix-1";
pub struct Prefix {
    owner: Weak<()>,
    base: Weak<Frame>,
    world: World,
    summary: PrefixSummary,
}
#[derive(Clone, Debug, Serialize)]
pub struct PrefixSummary {
    pub engine: &'static str,
    pub request_sha256: String,
    pub sources_sha256: String,
    pub revision: u64,
    pub before_stage: i64,
    pub inputs_sha256: String,
    pub required_stages: Vec<i64>,
    pub completed_stages: Vec<i64>,
    pub pending_stages: Vec<i64>,
    pub inherited_coverage_complete: bool,
    pub declared_prefix_complete: bool,
    pub instances: usize,
    pub volumes: usize,
    pub unresolved_geometry: usize,
    pub work: usize,
}
impl Scene {
    /// Explicit execution order, never inferred from numeric stage IDs or a
    /// topological sort. Every requirement must occur exactly once. Linking
    /// adjacent stages makes replacement retire all subsequent results.
    pub fn new_ordered(
        context: Context,
        mut requirements: Vec<Requirement>,
        execution: Vec<i64>,
        inherited: Coverage,
    ) -> Result<Self, String> {
        if requirements.is_empty()
            || requirements.len() > 256
            || execution.len() != requirements.len()
        {
            return Err("Invalid ordered environment provider plan".into());
        }
        let positions = execution
            .iter()
            .enumerate()
            .map(|(i, &id)| (id, i))
            .collect::<BTreeMap<_, _>>();
        let ids = requirements
            .iter()
            .map(|r| r.stage)
            .collect::<BTreeSet<_>>();
        if positions.len() != execution.len()
            || ids.len() != requirements.len()
            || ids != positions.keys().copied().collect()
        {
            return Err(
                "Environment execution order must contain every provider exactly once".into(),
            );
        }
        for r in &mut requirements {
            if r.dependencies.len() > 64 {
                return Err("Excessive ordered environment dependencies".into());
            }
            let index = positions[&r.stage];
            if r.dependencies
                .iter()
                .any(|id| positions.get(id).is_none_or(|&i| i >= index))
            {
                return Err("Environment execution order contradicts provider dependencies".into());
            }
            if index > 0 && !r.dependencies.contains(&execution[index - 1]) {
                r.dependencies.push(execution[index - 1]);
            }
        }
        Self::create(context, requirements, inherited, Some(execution))
    }
}
impl Snapshot {
    /// Environment as it exists before this stage: include all preceding
    /// providers, exclude the stage itself and later providers. Missing upstream
    /// results and inherited gaps stay incomplete. This does not publish or
    /// change the full-world completion summary.
    pub fn before(
        &self,
        context: &Context,
        stage: i64,
        inputs_sha256: &str,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Result<Prefix, Verdict>, String> {
        if context != self.context() || !(1..=128_000_000).contains(&maximum_work) {
            return Err("Stale environment prefix request or invalid allowance".into());
        }
        let plan = &self.frame.plan;
        let required = plan
            .requirements
            .get(&stage)
            .ok_or("Unknown environment prefix stage")?;
        if required.inputs_sha256 != inputs_sha256 {
            return Err("Environment prefix stage inputs differ".into());
        }
        let order = plan
            .execution
            .as_ref()
            .ok_or("Environment plan has no explicit execution order")?;
        let mut meter = Meter {
            used: 0,
            limit: maximum_work,
            cancelled: &cancelled,
        };
        if let Err(v) = meter.spend(order.len() + 1) {
            return Ok(Err(v));
        }
        let end = order
            .iter()
            .position(|&id| id == stage)
            .ok_or("Environment stage missing from execution order")?;
        let required_stages = order[..end].to_vec();
        let mut pending = vec![];
        let mut completed = vec![];
        let mut volumes = vec![];
        let mut instances = 0;
        let mut unresolved = 0;
        for &id in &required_stages {
            if let Err(v) = meter.spend(1) {
                return Ok(Err(v));
            }
            let Some(result) = self.frame.stages.get(&id) else {
                pending.push(id);
                continue;
            };
            completed.push(id);
            for object in &result.instances {
                if let Err(v) = meter.spend(object.volumes.len() + 1) {
                    return Ok(Err(v));
                }
                instances += 1;
                unresolved += object
                    .volumes
                    .iter()
                    .filter(|v| matches!(v.shape, Shape::Unresolved { .. }))
                    .count();
                volumes.extend(object.volumes.iter().cloned());
            }
        }
        let count = volumes.len();
        let inherited = plan.inherited == Coverage::Complete;
        let complete = inherited && pending.is_empty();
        let world = match World::build_with_meter(
            volumes,
            if complete {
                Coverage::Complete
            } else {
                Coverage::Incomplete
            },
            &mut meter,
        )? {
            Ok(world) => world,
            Err(v) => return Ok(Err(v)),
        };
        if let Err(v) = meter.spend(1) {
            return Ok(Err(v));
        }
        let summary = PrefixSummary {
            engine: VERSION,
            request_sha256: context.request_sha256.clone(),
            sources_sha256: context.sources_sha256.clone(),
            revision: self.revision(),
            before_stage: stage,
            inputs_sha256: inputs_sha256.into(),
            required_stages,
            completed_stages: completed,
            pending_stages: pending,
            inherited_coverage_complete: inherited,
            declared_prefix_complete: complete,
            instances,
            volumes: count,
            unresolved_geometry: unresolved,
            work: meter.used,
        };
        Ok(Ok(Prefix {
            owner: Arc::downgrade(&self.owner),
            base: Arc::downgrade(&self.frame),
            world,
            summary,
        }))
    }
}
impl Prefix {
    pub fn world(&self) -> &World {
        &self.world
    }
    pub fn summary(&self) -> &PrefixSummary {
        &self.summary
    }
    /// Retained prefix queries remain readable after a revision changes, but
    /// their results must not prepare an update against a different base frame.
    pub fn matches(&self, snapshot: &Snapshot, stage: i64, inputs_sha256: &str) -> bool {
        Weak::ptr_eq(&self.owner, &Arc::downgrade(&snapshot.owner))
            && Weak::ptr_eq(&self.base, &Arc::downgrade(&snapshot.frame))
            && self.summary.before_stage == stage
            && self.summary.inputs_sha256 == inputs_sha256
            && self.summary.request_sha256 == snapshot.context().request_sha256
            && self.summary.sources_sha256 == snapshot.context().sources_sha256
    }
}
