//! Immutable environment frames and atomic stage replacement. Completion here
//! covers declared providers only, never playable-world or PhysX certification.
use super::*;
use std::{
    collections::BTreeMap,
    sync::{Arc, Weak},
};

pub const VERSION: &str = "papyrus-environment-scene-1";
mod prefix;
pub use prefix::{Prefix, PrefixSummary};
const MAX_INSTANCES: usize = 65536;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Context {
    pub request_sha256: String,
    pub sources_sha256: String,
}
#[derive(Clone, Debug)]
pub struct Requirement {
    pub stage: i64,
    pub inputs_sha256: String,
    pub dependencies: Vec<i64>,
}
/// Includes serialized/inactive actors with zero currently registered volumes.
/// The owning stage adapter supplies only fully prepared, source-bound results.
#[derive(Clone, Debug)]
pub struct Instance {
    pub id: u64,
    pub volumes: Vec<Volume>,
}
pub struct Replacement {
    pub stage: i64,
    pub inputs_sha256: String,
    pub instances: Vec<Instance>,
}
struct Plan {
    context: Context,
    requirements: BTreeMap<i64, Requirement>,
    order: Vec<i64>,
    inherited: Coverage,
    execution: Option<Vec<i64>>,
}
struct Stage {
    instances: Vec<Instance>,
    volume_count: usize,
}
struct Frame {
    plan: Arc<Plan>,
    revision: u64,
    stages: BTreeMap<i64, Arc<Stage>>,
    world: World,
    instances: usize,
    volumes: usize,
    unresolved: usize,
}
pub struct Scene {
    owner: Arc<()>,
    head: Arc<Frame>,
}
#[derive(Clone)]
pub struct Snapshot {
    owner: Arc<()>,
    frame: Arc<Frame>,
}
pub struct PreparedUpdate {
    owner: Weak<()>,
    base: Weak<Frame>,
    next: Arc<Frame>,
    work: usize,
}
#[derive(Clone, Debug, Serialize)]
pub struct Summary {
    pub engine: &'static str,
    pub request_sha256: String,
    pub sources_sha256: String,
    pub revision: u64,
    pub completed_stages: Vec<i64>,
    pub pending_stages: Vec<i64>,
    pub inherited_coverage_complete: bool,
    pub declared_coverage_complete: bool,
    pub instances: usize,
    pub volumes: usize,
    pub unresolved_geometry: usize,
}
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}
impl Scene {
    /// Inherited coverage is an explicit ceiling. Finishing all declared stages
    /// cannot turn an incomplete upstream world into a complete one. The caller
    /// must derive required providers from its verified effective generation plan.
    pub fn new(
        context: Context,
        requirements: Vec<Requirement>,
        inherited: Coverage,
    ) -> Result<Self, String> {
        Self::create(context, requirements, inherited, None)
    }
    fn create(
        context: Context,
        requirements: Vec<Requirement>,
        inherited: Coverage,
        execution: Option<Vec<i64>>,
    ) -> Result<Self, String> {
        if !hash(&context.request_sha256)
            || !hash(&context.sources_sha256)
            || requirements.is_empty()
            || requirements.len() > 256
        {
            return Err("Invalid environment scene inputs or provider plan".into());
        }
        let mut required = BTreeMap::new();
        for mut r in requirements {
            if r.stage == 0
                || !hash(&r.inputs_sha256)
                || r.dependencies.len() > 64
                || r.dependencies.contains(&r.stage)
                || r.dependencies.contains(&0)
                || r.dependencies.iter().collect::<BTreeSet<_>>().len() != r.dependencies.len()
            {
                return Err("Invalid environment provider dependencies".into());
            }
            r.dependencies.sort_unstable();
            if required.insert(r.stage, r).is_some() {
                return Err("Duplicate environment provider".into());
            }
        }
        if required
            .values()
            .any(|r| r.dependencies.iter().any(|id| !required.contains_key(id)))
        {
            return Err("Environment provider dependency is absent".into());
        }
        let mut order = Vec::new();
        let mut done = BTreeSet::new();
        while done.len() < required.len() {
            let before = done.len();
            for (&id, r) in &required {
                if !done.contains(&id) && r.dependencies.iter().all(|id| done.contains(id)) {
                    done.insert(id);
                    order.push(id);
                }
            }
            if done.len() == before {
                return Err("Environment provider plan contains a cycle".into());
            }
        }
        let world = World::build(vec![], Coverage::Incomplete, 100, || false)?
            .map_err(|v| format!("Empty environment index: {v:?}"))?;
        let plan = Arc::new(Plan {
            context,
            requirements: required,
            order,
            inherited,
            execution,
        });
        Ok(Self {
            owner: Arc::new(()),
            head: Arc::new(Frame {
                plan,
                revision: 0,
                stages: BTreeMap::new(),
                world,
                instances: 0,
                volumes: 0,
                unresolved: 0,
            }),
        })
    }
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            owner: self.owner.clone(),
            frame: self.head.clone(),
        }
    }
    /// The final publication point. A stale/cross-document update or cancelled
    /// worker cannot replace the accepted frame. Older snapshots stay readable.
    pub fn publish(
        &mut self,
        update: PreparedUpdate,
        cancelled: impl Fn() -> bool,
    ) -> Result<bool, String> {
        if cancelled() {
            return Ok(false);
        }
        if !Weak::ptr_eq(&update.owner, &Arc::downgrade(&self.owner))
            || !Weak::ptr_eq(&update.base, &Arc::downgrade(&self.head))
        {
            return Err("Environment stage update is stale or belongs to another document".into());
        }
        if cancelled() {
            return Ok(false);
        }
        self.head = update.next;
        Ok(true)
    }
}
impl Snapshot {
    pub fn world(&self) -> &World {
        &self.frame.world
    }
    pub fn context(&self) -> &Context {
        &self.frame.plan.context
    }
    pub fn revision(&self) -> u64 {
        self.frame.revision
    }
    pub fn summary(&self) -> Summary {
        let f = &self.frame;
        let pending_stages = f
            .plan
            .requirements
            .keys()
            .filter(|id| !f.stages.contains_key(id))
            .copied()
            .collect::<Vec<_>>();
        Summary {
            engine: VERSION,
            request_sha256: f.plan.context.request_sha256.clone(),
            sources_sha256: f.plan.context.sources_sha256.clone(),
            revision: f.revision,
            completed_stages: f.stages.keys().copied().collect(),
            declared_coverage_complete: f.plan.inherited == Coverage::Complete
                && pending_stages.is_empty(),
            inherited_coverage_complete: f.plan.inherited == Coverage::Complete,
            pending_stages,
            instances: f.instances,
            volumes: f.volumes,
            unresolved_geometry: f.unresolved,
        }
    }
    /// Each supplied result replaces the stage's entire actor set. Downstream
    /// providers are retired transitively unless rebuilt in this same batch.
    /// Candidate checks may keep using this snapshot throughout preparation.
    pub fn prepare(
        &self,
        context: &Context,
        changes: &[Replacement],
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Result<PreparedUpdate, Verdict>, String> {
        if context != self.context()
            || changes.is_empty()
            || changes.len() > 256
            || !(1..=128_000_000).contains(&maximum_work)
        {
            return Err("Environment update has stale inputs or an invalid allowance".into());
        }
        let mut meter = Meter {
            used: 0,
            limit: maximum_work,
            cancelled: &cancelled,
        };
        if let Err(v) = meter.spend(1) {
            return Ok(Err(v));
        }
        let mut supplied = BTreeMap::new();
        for change in changes {
            if let Err(v) = meter.spend(1) {
                return Ok(Err(v));
            }
            let required = self
                .frame
                .plan
                .requirements
                .get(&change.stage)
                .ok_or("Unknown environment provider")?;
            if change.inputs_sha256 != required.inputs_sha256
                || change.instances.len() > MAX_INSTANCES
                || supplied.insert(change.stage, change).is_some()
            {
                return Err("Duplicate, stale or excessive environment stage result".into());
            }
        }
        let mut retired = BTreeSet::new();
        for id in &self.frame.plan.order {
            let required = &self.frame.plan.requirements[id];
            if let Err(v) = meter.spend(required.dependencies.len() + 1) {
                return Ok(Err(v));
            }
            if supplied.contains_key(id)
                || required.dependencies.iter().any(|id| retired.contains(id))
            {
                retired.insert(*id);
            }
        }
        if let Err(v) = meter.spend(self.frame.stages.len() + 1) {
            return Ok(Err(v));
        }
        let mut stages = self
            .frame
            .stages
            .iter()
            .filter(|(id, _)| !retired.contains(id))
            .map(|(&id, s)| (id, s.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut total_instances = stages.values().map(|s| s.instances.len()).sum::<usize>();
        let mut total_volumes = stages.values().map(|s| s.volume_count).sum::<usize>();
        for (&id, change) in &supplied {
            let required = &self.frame.plan.requirements[&id];
            if required
                .dependencies
                .iter()
                .any(|dep| !stages.contains_key(dep) && !supplied.contains_key(dep))
            {
                return Err("Environment stage dependency has not completed".into());
            }
            total_instances = total_instances
                .checked_add(change.instances.len())
                .ok_or("Environment instance count overflow")?;
            if total_instances > MAX_INSTANCES {
                return Err("Excessive environment scene instances".into());
            }
            let volume_count = change
                .instances
                .iter()
                .try_fold(0usize, |sum, o| sum.checked_add(o.volumes.len()))
                .ok_or("Environment volume count overflow")?;
            total_volumes = total_volumes
                .checked_add(volume_count)
                .ok_or("Environment volume count overflow")?;
            if total_volumes > MAX_VOLUMES {
                return Err("Excessive environment stage volumes".into());
            }
            if let Err(v) = meter.spend(change.instances.len() + volume_count + 1) {
                return Ok(Err(v));
            }
            stages.insert(
                id,
                Arc::new(Stage {
                    instances: change.instances.clone(),
                    volume_count,
                }),
            );
        }
        let mut identities = BTreeSet::new();
        let mut volumes = Vec::new();
        let mut unresolved = 0usize;
        for stage in stages.values() {
            for object in &stage.instances {
                if let Err(v) = meter.spend(object.volumes.len() + 1) {
                    return Ok(Err(v));
                }
                if object.id == 0
                    || !identities.insert(object.id)
                    || identities.len() > MAX_INSTANCES
                    || object.volumes.iter().any(|v| v.key.instance != object.id)
                {
                    return Err("Duplicate or mismatched environment instance ownership".into());
                }
                if volumes.len().saturating_add(object.volumes.len()) > MAX_VOLUMES {
                    return Err("Excessive environment scene volumes".into());
                }
                unresolved += object
                    .volumes
                    .iter()
                    .filter(|v| matches!(v.shape, Shape::Unresolved { .. }))
                    .count();
                volumes.extend(object.volumes.iter().cloned());
            }
        }
        let count = volumes.len();
        let coverage = if self.frame.plan.inherited == Coverage::Complete
            && stages.len() == self.frame.plan.requirements.len()
        {
            Coverage::Complete
        } else {
            Coverage::Incomplete
        };
        let world = match World::build_with_meter(volumes, coverage, &mut meter)? {
            Ok(world) => world,
            Err(v) => return Ok(Err(v)),
        };
        let revision = self
            .frame
            .revision
            .checked_add(1)
            .ok_or("Environment revision overflow")?;
        if let Err(v) = meter.spend(1) {
            return Ok(Err(v));
        }
        let next = Arc::new(Frame {
            plan: self.frame.plan.clone(),
            revision,
            stages,
            world,
            instances: identities.len(),
            volumes: count,
            unresolved,
        });
        Ok(Ok(PreparedUpdate {
            owner: Arc::downgrade(&self.owner),
            base: Arc::downgrade(&self.frame),
            next,
            work: meter.used,
        }))
    }
}
impl PreparedUpdate {
    pub fn work(&self) -> usize {
        self.work
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ordered(coverage: Coverage) -> Scene {
        Scene::new_ordered(context(), requirements(), vec![3, 1, 2], coverage).unwrap()
    }
    fn prior(snapshot: &Snapshot, stage: i64) -> Prefix {
        snapshot
            .before(
                snapshot.context(),
                stage,
                &change(stage, vec![]).inputs_sha256,
                1_000_000,
                || false,
            )
            .unwrap()
            .unwrap()
    }
    fn prefix_verdict(prefix: &Prefix, x: f32) -> Verdict {
        prefix
            .world()
            .query(query(x), 1, 0., 100000, || false)
            .unwrap()
            .verdict
    }
    #[test]
    fn ordered_prefix_excludes_self_and_future_without_completing_the_full_plan() {
        let mut scene = ordered(Coverage::Complete);
        let first = prior(&scene.snapshot(), 3);
        assert_eq!(prefix_verdict(&first, 0.), Verdict::Clear);
        assert!(first.summary().required_stages.is_empty());
        assert_eq!(
            prefix_verdict(&prior(&scene.snapshot(), 1), 100.),
            Verdict::Incomplete
        );
        commit(&mut scene, &[change(3, vec![object(300, 30.)])]);
        let before = prior(&scene.snapshot(), 1);
        assert_eq!(before.summary().required_stages, vec![3]);
        assert!(before.summary().declared_prefix_complete);
        assert!(matches!(prefix_verdict(&before, 30.), Verdict::Blocked(_)));
        assert_eq!(prefix_verdict(&before, 0.), Verdict::Clear);
        assert!(!scene.snapshot().summary().declared_coverage_complete);
        commit(
            &mut scene,
            &[
                change(1, vec![object(100, 0.)]),
                change(2, vec![object(200, 10.)]),
            ],
        );
        let before = prior(&scene.snapshot(), 1);
        assert_eq!(prefix_verdict(&before, 0.), Verdict::Clear);
        assert_eq!(prefix_verdict(&before, 10.), Verdict::Clear);
        assert!(matches!(prefix_verdict(&before, 30.), Verdict::Blocked(_)));
        assert_eq!(
            (before.summary().instances, before.summary().volumes),
            (1, 1)
        );
        assert!(scene.snapshot().summary().declared_coverage_complete);
    }
    #[test]
    fn ordered_replacement_retires_every_later_provider_and_old_prefixes_stay_readable() {
        let mut scene = ordered(Coverage::Complete);
        assert!(
            scene
                .snapshot()
                .prepare(&context(), &[change(1, vec![])], 100000, || false)
                .is_err()
        );
        commit(
            &mut scene,
            &[
                change(3, vec![object(300, 30.)]),
                change(1, vec![object(100, 0.)]),
                change(2, vec![object(200, 10.)]),
            ],
        );
        let snapshot = scene.snapshot();
        let old = prior(&snapshot, 2);
        assert!(old.matches(&snapshot, 2, &change(2, vec![]).inputs_sha256));
        commit(&mut scene, &[change(3, vec![object(300, 60.)])]);
        drop(snapshot);
        let current = scene.snapshot();
        assert_eq!(current.summary().pending_stages, vec![1, 2]);
        assert!(!old.matches(&current, 2, &change(2, vec![]).inputs_sha256));
        assert!(matches!(prefix_verdict(&old, 0.), Verdict::Blocked(_)));
        assert_eq!(prior(&current, 2).summary().pending_stages, vec![1]);
        assert_eq!(prefix_verdict(&prior(&current, 2), 0.), Verdict::Incomplete);
        assert!(matches!(
            prefix_verdict(&prior(&current, 2), 60.),
            Verdict::Blocked(_)
        ));
    }
    #[test]
    fn prefix_never_clears_inherited_gaps_or_unknown_geometry() {
        let mut scene = ordered(Coverage::Incomplete);
        let empty = prior(&scene.snapshot(), 3);
        assert_eq!(prefix_verdict(&empty, 100.), Verdict::Incomplete);
        commit(&mut scene, &[change(3, vec![])]);
        let prefix = prior(&scene.snapshot(), 1);
        assert!(!prefix.summary().declared_prefix_complete);
        assert!(prefix.summary().pending_stages.is_empty());
        assert_eq!(prefix_verdict(&prefix, 100.), Verdict::Incomplete);
        let mut scene = ordered(Coverage::Complete);
        let mut unknown = object(300, 0.);
        unknown.volumes[0].shape = Shape::Unresolved { bounds: None };
        commit(&mut scene, &[change(3, vec![unknown])]);
        let prefix = prior(&scene.snapshot(), 1);
        assert!(prefix.summary().declared_prefix_complete);
        assert_eq!(prefix.summary().unresolved_geometry, 1);
        assert!(matches!(
            prefix_verdict(&prefix, 1000.),
            Verdict::Unresolved(_)
        ));
    }
    #[test]
    fn prefix_work_limits_and_cancellation_do_not_publish_or_hide_partial_upstream_data() {
        let mut scene = ordered(Coverage::Complete);
        commit(
            &mut scene,
            &[change(
                3,
                (1..=1000).map(|id| object(id, id as f32)).collect(),
            )],
        );
        let snapshot = scene.snapshot();
        let prefix = prior(&snapshot, 1);
        let work = prefix.summary().work;
        let input = change(1, vec![]).inputs_sha256;
        assert!(work > 1000);
        assert_eq!(
            snapshot
                .before(&context(), 1, &input, work, || false)
                .unwrap()
                .unwrap()
                .summary()
                .work,
            work
        );
        assert!(matches!(
            snapshot
                .before(&context(), 1, &input, work - 1, || false)
                .unwrap(),
            Err(Verdict::Budget)
        ));
        assert!(matches!(
            snapshot
                .before(&context(), 1, &input, 100000, || true)
                .unwrap(),
            Err(Verdict::Cancelled)
        ));
        let polls = std::cell::Cell::new(0);
        assert!(matches!(
            snapshot
                .before(&context(), 1, &input, 100000, || {
                    polls.set(polls.get() + 1);
                    polls.get() > 150
                })
                .unwrap(),
            Err(Verdict::Cancelled)
        ));
        assert_eq!(scene.snapshot().revision(), snapshot.revision());
        assert_eq!(scene.snapshot().summary().pending_stages, vec![1, 2]);
    }
    #[test]
    fn prefix_binds_stage_inputs_document_and_revision_and_requires_explicit_order() {
        let scene = ordered(Coverage::Complete);
        let snapshot = scene.snapshot();
        let prefix = prior(&snapshot, 3);
        let other = ordered(Coverage::Complete).snapshot();
        let input = change(3, vec![]).inputs_sha256;
        assert!(!prefix.matches(&other, 3, &input));
        assert!(!prefix.matches(&snapshot, 1, &input));
        assert!(!prefix.matches(&snapshot, 3, &"f".repeat(64)));
        assert!(
            snapshot
                .before(&context(), 3, &"f".repeat(64), 10000, || false)
                .is_err()
        );
        let mut changed = context();
        changed.request_sha256 = "d".repeat(64);
        assert!(
            snapshot
                .before(&changed, 3, &input, 10000, || false)
                .is_err()
        );
        assert!(
            snapshot
                .before(&context(), 99, &input, 10000, || false)
                .is_err()
        );
        assert!(
            self::scene(Coverage::Complete)
                .snapshot()
                .before(&context(), 3, &input, 10000, || false)
                .is_err()
        );
    }
    #[test]
    fn ordered_plans_reject_omissions_duplicates_unknown_ids_and_inverted_dependencies() {
        for order in [
            vec![],
            vec![1, 2],
            vec![3, 1, 1],
            vec![3, 1, 99],
            vec![3, 2, 1],
        ] {
            assert!(
                Scene::new_ordered(context(), requirements(), order, Coverage::Complete).is_err()
            );
        }
        let mut invalid = requirements();
        invalid[0].dependencies = vec![3; 65];
        assert!(Scene::new_ordered(context(), invalid, vec![3, 1, 2], Coverage::Complete).is_err());
    }
    fn context() -> Context {
        Context {
            request_sha256: "a".repeat(64),
            sources_sha256: "b".repeat(64),
        }
    }
    fn requirements() -> Vec<Requirement> {
        vec![
            Requirement {
                stage: 1,
                inputs_sha256: "1".repeat(64),
                dependencies: vec![],
            },
            Requirement {
                stage: 2,
                inputs_sha256: "2".repeat(64),
                dependencies: vec![1],
            },
            Requirement {
                stage: 3,
                inputs_sha256: "3".repeat(64),
                dependencies: vec![],
            },
        ]
    }
    fn scene(inherited: Coverage) -> Scene {
        Scene::new(context(), requirements(), inherited).unwrap()
    }
    fn query(x: f32) -> Box3 {
        Box3 {
            center: [x, 0., 0.],
            rotation: [0., 0., 0., 1.],
            extents: [0.1; 3],
        }
    }
    fn object(id: u64, x: f32) -> Instance {
        Instance {
            id,
            volumes: vec![Volume {
                key: Key {
                    instance: id,
                    component: 10,
                },
                mask: 1,
                shape: Shape::Box(Box3 {
                    extents: [1.; 3],
                    ..query(x)
                }),
            }],
        }
    }
    fn change(stage: i64, instances: Vec<Instance>) -> Replacement {
        Replacement {
            stage,
            inputs_sha256: stage.to_string().repeat(64),
            instances,
        }
    }
    fn commit(scene: &mut Scene, changes: &[Replacement]) {
        let snapshot = scene.snapshot();
        let update = snapshot
            .prepare(snapshot.context(), changes, 1_000_000, || false)
            .unwrap()
            .unwrap();
        assert!(scene.publish(update, || false).unwrap());
    }
    fn verdict(snapshot: &Snapshot, x: f32) -> Verdict {
        snapshot
            .world()
            .query(query(x), 1, 0., 10000, || false)
            .unwrap()
            .verdict
    }
    #[test]
    fn upstream_replacement_retires_dependents_without_changing_old_snapshots() {
        let mut scene = scene(Coverage::Complete);
        let initial = scene.snapshot();
        assert_eq!(verdict(&initial, 0.), Verdict::Incomplete);
        commit(
            &mut scene,
            &[
                change(1, vec![object(100, 0.)]),
                change(2, vec![object(200, 20.)]),
                change(3, vec![object(300, 40.)]),
            ],
        );
        let accepted = scene.snapshot();
        assert!(accepted.summary().declared_coverage_complete);
        assert!(matches!(verdict(&accepted, 20.), Verdict::Blocked(_)));
        commit(&mut scene, &[change(1, vec![object(100, 5.)])]);
        let edited = scene.snapshot();
        assert_eq!(edited.summary().pending_stages, vec![2]);
        assert_eq!(edited.summary().instances, 2);
        assert_eq!(verdict(&edited, 20.), Verdict::Incomplete);
        assert!(matches!(verdict(&edited, 40.), Verdict::Blocked(_)));
        assert!(matches!(verdict(&accepted, 0.), Verdict::Blocked(_)));
        assert!(matches!(verdict(&accepted, 20.), Verdict::Blocked(_)));
        assert_eq!(verdict(&initial, 0.), Verdict::Incomplete);
        commit(&mut scene, &[change(2, vec![])]);
        assert!(scene.snapshot().summary().declared_coverage_complete);
        assert_eq!(verdict(&scene.snapshot(), 20.), Verdict::Clear);
    }
    #[test]
    fn independent_providers_survive_an_atomic_multi_stage_rebuild_and_removal() {
        let mut scene = scene(Coverage::Complete);
        commit(
            &mut scene,
            &[
                change(1, vec![object(100, 0.)]),
                change(2, vec![object(200, 20.)]),
                change(3, vec![object(300, 40.)]),
            ],
        );
        commit(
            &mut scene,
            &[
                change(1, vec![object(100, 5.)]),
                change(2, vec![object(200, 25.)]),
            ],
        );
        let accepted = scene.snapshot();
        assert!(accepted.summary().pending_stages.is_empty());
        assert_eq!(verdict(&accepted, 20.), Verdict::Clear);
        assert!(matches!(verdict(&accepted, 25.), Verdict::Blocked(_)));
        commit(&mut scene, &[change(1, vec![])]);
        assert_eq!(scene.snapshot().summary().instances, 1);
        assert!(matches!(
            verdict(&scene.snapshot(), 40.),
            Verdict::Blocked(_)
        ));
        assert_eq!(scene.snapshot().summary().pending_stages, vec![2]);
    }
    #[test]
    fn failed_stale_foreign_and_cancelled_publication_preserve_the_accepted_revision() {
        let mut scene = scene(Coverage::Complete);
        let base = scene.snapshot();
        let make = || {
            base.prepare(
                base.context(),
                &[change(1, vec![object(100, 0.)])],
                100000,
                || false,
            )
            .unwrap()
            .unwrap()
        };
        let stale = make();
        let foreign = make();
        let mut other = Scene::new(context(), requirements(), Coverage::Complete).unwrap();
        assert!(other.publish(foreign, || false).is_err());
        assert_eq!(other.snapshot().revision(), 0);
        let polls = std::cell::Cell::new(0);
        assert!(
            !scene
                .publish(make(), || {
                    polls.set(polls.get() + 1);
                    polls.get() > 1
                })
                .unwrap()
        );
        assert_eq!(scene.snapshot().revision(), 0);
        assert!(scene.publish(make(), || false).unwrap());
        assert!(scene.publish(stale, || false).is_err());
        assert_eq!(scene.snapshot().revision(), 1);
        let current = scene.snapshot();
        let invalid = change(3, vec![object(100, 10.)]);
        assert!(
            current
                .prepare(current.context(), &[invalid], 100000, || false)
                .is_err()
        );
        assert_eq!(scene.snapshot().revision(), 1);
        assert!(matches!(
            verdict(&scene.snapshot(), 0.),
            Verdict::Blocked(_)
        ));
    }
    #[test]
    fn shared_budget_covers_index_construction_and_mid_prepare_cancellation() {
        let scene = scene(Coverage::Complete);
        let base = scene.snapshot();
        let changes = [change(
            1,
            (1..=100).map(|id| object(id, id as f32 * 4.)).collect(),
        )];
        let complete = base
            .prepare(base.context(), &changes, 1_000_000, || false)
            .unwrap()
            .unwrap();
        let work = complete.work();
        assert!(work > 1000);
        assert_eq!(
            base.prepare(base.context(), &changes, work, || false)
                .unwrap()
                .unwrap()
                .work(),
            work
        );
        assert!(matches!(
            base.prepare(base.context(), &changes, work - 1, || false)
                .unwrap(),
            Err(Verdict::Budget)
        ));
        let polls = std::cell::Cell::new(0);
        let cancelled = base
            .prepare(base.context(), &changes, 1_000_000, || {
                polls.set(polls.get() + 1);
                polls.get() > 150
            })
            .unwrap();
        assert!(matches!(cancelled, Err(Verdict::Cancelled)));
        assert_eq!(scene.snapshot().revision(), 0);
    }
    #[test]
    fn incomplete_upstream_and_unknown_geometry_cannot_become_clearance() {
        let mut scene = scene(Coverage::Incomplete);
        commit(
            &mut scene,
            &[change(1, vec![]), change(2, vec![]), change(3, vec![])],
        );
        let summary = scene.snapshot().summary();
        assert!(summary.pending_stages.is_empty());
        assert!(!summary.declared_coverage_complete);
        assert_eq!(verdict(&scene.snapshot(), 0.), Verdict::Incomplete);
        let mut unknown = object(100, 0.);
        unknown.volumes[0].shape = Shape::Unresolved { bounds: None };
        commit(&mut scene, &[change(3, vec![unknown])]);
        assert_eq!(scene.snapshot().summary().unresolved_geometry, 1);
        assert!(matches!(
            verdict(&scene.snapshot(), 1000.),
            Verdict::Unresolved(_)
        ));
    }
    #[test]
    fn dependency_retirement_is_transitive_and_requires_every_rebuilt_intermediate() {
        let mut plan = requirements();
        plan.push(Requirement {
            stage: 4,
            inputs_sha256: "4".repeat(64),
            dependencies: vec![2],
        });
        let mut scene = Scene::new(context(), plan, Coverage::Complete).unwrap();
        commit(
            &mut scene,
            &[
                change(1, vec![object(100, 0.)]),
                change(2, vec![object(200, 20.)]),
                change(3, vec![object(300, 40.)]),
                change(4, vec![object(400, 60.)]),
            ],
        );
        let accepted = scene.snapshot();
        assert!(
            accepted
                .prepare(
                    accepted.context(),
                    &[change(1, vec![]), change(4, vec![])],
                    100000,
                    || false
                )
                .is_err()
        );
        commit(&mut scene, &[change(1, vec![])]);
        assert_eq!(scene.snapshot().summary().pending_stages, vec![2, 4]);
        assert_eq!(scene.snapshot().summary().instances, 1);
        assert_eq!(verdict(&scene.snapshot(), 60.), Verdict::Incomplete);
        assert!(matches!(verdict(&accepted, 60.), Verdict::Blocked(_)));
    }
    #[test]
    fn invalid_plans_missing_dependencies_and_mismatched_inputs_are_rejected() {
        let mut cyclic = requirements();
        cyclic[0].dependencies.push(2);
        assert!(Scene::new(context(), cyclic, Coverage::Complete).is_err());
        assert!(Scene::new(context(), vec![], Coverage::Complete).is_err());
        let mut missing = requirements();
        missing[0].dependencies.push(99);
        assert!(Scene::new(context(), missing, Coverage::Complete).is_err());
        let scene = scene(Coverage::Complete);
        let base = scene.snapshot();
        assert!(
            base.prepare(base.context(), &[change(2, vec![])], 100000, || false)
                .is_err()
        );
        let mut wrong = change(1, vec![]);
        wrong.inputs_sha256 = "f".repeat(64);
        assert!(
            base.prepare(base.context(), &[wrong], 100000, || false)
                .is_err()
        );
        let mut wrong_context = context();
        wrong_context.request_sha256 = "c".repeat(64);
        assert!(
            base.prepare(&wrong_context, &[change(1, vec![])], 100000, || false)
                .is_err()
        );
        let mut mismatch = object(100, 0.);
        mismatch.volumes[0].key.instance = 200;
        assert!(
            base.prepare(base.context(), &[change(1, vec![mismatch])], 100000, || {
                false
            })
            .is_err()
        );
    }
}
