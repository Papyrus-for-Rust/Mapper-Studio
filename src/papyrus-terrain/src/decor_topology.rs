//! Transactional authored topology modifiers for the versioned Papyrus policy.
//! Numeric masks come from the pinned installation, never from display labels.
use crate::{decor, decor_fit, spawn};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
pub const VERSION: &str = "papyrus-decor-topology-5";
#[derive(Clone, Debug)]
pub enum Operation {
    Add,
    Set,
}
#[derive(Clone, Debug)]
pub struct Modifier {
    pub position: [f32; 3],
    pub radius: f32,
    pub fade: f32,
    pub mask: u32,
    pub operation: Operation,
}
#[derive(Clone, Debug)]
pub struct Query {
    pub check_placement: bool,
    pub position: [f32; 3],
    pub radius: f32,
    pub filter: spawn::Filter,
}
#[derive(Clone, Debug, Default)]
pub struct Prefab {
    pub modifiers: Vec<Modifier>,
    pub queries: Vec<Query>,
    pub anchors: Vec<[f32; 3]>,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub known_mask: u32,
    pub instances: usize,
    pub operations: usize,
    pub changed_cells: usize,
    pub read_locked_cells: usize,
    pub filter_rejections: usize,
    pub read_conflicts: usize,
}
pub(crate) struct State {
    pub cells: Vec<i32>,
    locks: Vec<bool>,
    group: Option<Group>,
    pub report: Report,
}
#[derive(Default)]
struct Group {
    original: BTreeMap<usize, i32>,
    reads: BTreeSet<usize>,
}
pub(crate) struct Plan {
    pub writes: Vec<(usize, i32)>,
    pub reads: Vec<usize>,
    pub operations: usize,
}
#[derive(Debug)]
pub(crate) enum Failure {
    Filter,
    Conflict,
    Unsupported,
    Outside,
    Budget,
}
#[derive(Debug)]
pub(crate) enum CheckedFailure<E> {
    Topology(Failure),
    Cell(E),
}
impl<E> From<Failure> for CheckedFailure<E> {
    fn from(value: Failure) -> Self {
        Self::Topology(value)
    }
}
fn valid(p: &[f32]) -> bool {
    p.iter().all(|v| v.is_finite() && v.abs() <= 10000.)
}
fn world(p: [f32; 3], pose: &decor_fit::Pose) -> [f32; 3] {
    let v = decor_fit::rotate(pose.rotation, std::array::from_fn(|i| p[i] * pose.scale[i]));
    std::array::from_fn(|i| pose.position[i] + v[i])
}
fn index(n: usize, size: f32, w: f32) -> usize {
    ((w / size + 0.5) * n as f32).clamp(0., (n - 1) as f32) as usize
}
pub fn validate(p: &Prefab, known: u32, protected: u32) -> Result<(), String> {
    if known == 0 || protected & !known != 0 {
        return Err("Unbound topology policy".into());
    }
    if p.modifiers.len() > 4096 || p.queries.len() > 4096 || p.anchors.len() > 4096 {
        return Err("Too many topology attributes".into());
    }
    for m in &p.modifiers {
        if !valid(&m.position)
            || !valid(&[m.radius, m.fade])
            || !(0. ..=128.).contains(&m.radius)
            || !(0. ..=128.).contains(&m.fade)
            || m.mask & !known != 0
        {
            return Err("Invalid or unbound topology modifier".into());
        }
    }
    for q in &p.queries {
        q.filter.validate()?;
        if !valid(&q.position) || !q.radius.is_finite() || !(0. ..=128.).contains(&q.radius) {
            return Err("Invalid topology query".into());
        }
    }
    if p.anchors.iter().any(|p| !valid(p)) {
        return Err("Invalid topology anchor".into());
    }
    Ok(())
}
impl State {
    /// Reject a guaranteed add-only write before collecting query dependencies.
    /// Set operations can restore an earlier value, so they use the full planner.
    #[allow(clippy::too_many_arguments)]
    pub fn precheck_additions<E>(
        &self,
        prefab: &Prefab,
        pose: &decor_fit::Pose,
        n: usize,
        size: f32,
        visits: &mut usize,
        limit: usize,
        cancelled: &impl Fn() -> bool,
        mut check: impl FnMut(usize) -> Result<(), E>,
    ) -> Result<Option<()>, CheckedFailure<E>> {
        if cancelled() {
            return Ok(None);
        }
        if !prefab
            .modifiers
            .iter()
            .all(|m| matches!(m.operation, Operation::Add))
        {
            return Ok(Some(()));
        }
        for m in &prefab.modifiers {
            if cancelled() {
                return Ok(None);
            }
            let c = world(m.position, pose);
            let r = m.radius * pose.scale[1];
            let f = m.fade * pose.scale[1];
            if !valid(&c)
                || !r.is_finite()
                || !f.is_finite()
                || !(0. ..=256.).contains(&r)
                || !(0. ..=256.).contains(&f)
                || c[0].abs() + r > size * 0.5
                || c[2].abs() + r > size * 0.5
                || m.mask & !self.report.known_mask != 0
            {
                // Leave invalid modifiers to the normal planner.
                continue;
            }
            let cx = (c[0] / size + 0.5) * n as f32;
            let cz = (c[2] / size + 0.5) * n as f32;
            let radius = r / size * n as f32;
            let fade = f / size * n as f32;
            for [dx, dz] in [
                [0., 0.],
                [-0.5, 0.],
                [0.5, 0.],
                [0., -0.5],
                [0., 0.5],
                [-0.5, -0.5],
                [0.5, -0.5],
                [-0.5, 0.5],
                [0.5, 0.5],
            ] {
                let x = index(n, size, c[0] + dx * r);
                let z = index(n, size, c[2] + dz * r);
                let i = z * n + x;
                if self.cells[i] as u32 & m.mask == m.mask {
                    continue;
                }
                let px = x as f32 + 0.5 - cx;
                let pz = z as f32 + 0.5 - cz;
                let distance = (px * px + pz * pz).sqrt();
                let applies = if fade == 0. {
                    distance < radius
                } else {
                    ((radius - distance) / fade).clamp(0., 1.) > 0.5
                };
                if !applies {
                    continue;
                }
                if let Err(error) = check(i) {
                    if *visits >= limit {
                        return Err(Failure::Budget.into());
                    }
                    *visits += 1;
                    return Err(CheckedFailure::Cell(error));
                }
            }
        }
        Ok(Some(()))
    }
    pub fn new(cells: &[i32], known_mask: u32) -> Self {
        Self {
            cells: cells.to_vec(),
            locks: vec![false; cells.len()],
            group: None,
            report: Report {
                known_mask,
                instances: 0,
                operations: 0,
                changed_cells: 0,
                read_locked_cells: 0,
                filter_rejections: 0,
                read_conflicts: 0,
            },
        }
    }
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &self,
        prefab: &Prefab,
        pose: &decor_fit::Pose,
        stage: &decor::Stage,
        n: usize,
        size: f32,
        visits: &mut usize,
        limit: usize,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Option<Plan>, Failure> {
        self.prepare_checked(
            prefab,
            pose,
            stage,
            n,
            size,
            visits,
            limit,
            cancelled,
            |_| Ok::<_, std::convert::Infallible>(()),
        )
        .map_err(|error| match error {
            CheckedFailure::Topology(error) => error,
            CheckedFailure::Cell(never) => match never {},
        })
    }
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_checked<E>(
        &self,
        prefab: &Prefab,
        pose: &decor_fit::Pose,
        stage: &decor::Stage,
        n: usize,
        size: f32,
        visits: &mut usize,
        limit: usize,
        cancelled: &impl Fn() -> bool,
        mut check: impl FnMut(usize) -> Result<(), E>,
    ) -> Result<Option<Plan>, CheckedFailure<E>> {
        let mut visit = || {
            if *visits >= limit {
                Err(Failure::Budget)
            } else {
                *visits += 1;
                Ok(())
            }
        };
        let mut queries = prefab.queries.clone();
        if stage.filter_mode & 1 != 0 {
            queries.push(Query {
                check_placement: false,
                position: [0.; 3],
                radius: 0.,
                filter: stage.filter.clone(),
            });
        }
        if stage.filter_mode & 2 != 0 {
            queries.extend(prefab.anchors.iter().map(|p| Query {
                check_placement: false,
                position: *p,
                radius: 0.,
                filter: stage.filter.clone(),
            }));
        }
        let mut reads = Vec::new();
        for q in &queries {
            if cancelled() {
                return Ok(None);
            }
            // Unrestricted topology has no dependency to lock.
            if q.filter.topology_any == -1
                && q.filter.topology_all == 0
                && q.filter.topology_not == 0
            {
                continue;
            }
            let p = world(q.position, pose);
            if !valid(&p) || p[0].abs() > size * 0.5 || p[2].abs() > size * 0.5 {
                return Err(Failure::Outside.into());
            }
            let xm = index(n, size, p[0]);
            let zm = index(n, size, p[2]);
            let mut mask = 0;
            let u = p[0] / size + 0.5;
            let v = p[2] / size + 0.5;
            let r = q.radius / size;
            let normalized_index = |a: f32| (a * n as f32).clamp(0., (n - 1) as f32) as usize;
            for z in normalized_index(v - r)..=normalized_index(v + r) {
                if cancelled() {
                    return Ok(None);
                }
                for x in normalized_index(u - r)..=normalized_index(u + r) {
                    visit()?;
                    let dx = x as f32 - xm as f32;
                    let dz = z as f32 - zm as f32;
                    // Matches the inspected integer-index circle within world-radius bounds.
                    if q.radius == 0. || dx * dx + dz * dz <= q.radius * q.radius {
                        let i = z * n + x;
                        let value = self
                            .group
                            .as_ref()
                            .and_then(|g| g.original.get(&i))
                            .copied()
                            .unwrap_or(self.cells[i]);
                        mask |= value as u32;
                        // OR accumulation cannot remove a forbidden bit. Reject
                        // immediately while retaining every read on accepted plans.
                        if mask & q.filter.topology_not as u32 != 0 {
                            return Err(Failure::Filter.into());
                        }
                        reads.push(i);
                    }
                }
            }
            if reads.len() > 262144 {
                return Err(Failure::Unsupported.into());
            }
            if !q.filter.topology_matches(mask) {
                return Err(Failure::Filter.into());
            }
        }
        // Add-only plans are monotone. A changed cell cannot be restored by a
        // later modifier, so its final reservation can be checked immediately.
        // Ordered Set plans retain the final-write check below.
        let monotone = prefab
            .modifiers
            .iter()
            .all(|m| matches!(m.operation, Operation::Add));
        let mut writes = BTreeMap::new();
        for m in &prefab.modifiers {
            let c = world(m.position, pose);
            let r = m.radius * pose.scale[1];
            let f = m.fade * pose.scale[1];
            if !valid(&c) || !r.is_finite() || !f.is_finite() || r > 256. || f > 256. {
                return Err(Failure::Unsupported.into());
            }
            if c[0].abs() + r > size * 0.5 || c[2].abs() + r > size * 0.5 {
                return Err(Failure::Outside.into());
            }
            let cx = (c[0] / size + 0.5) * n as f32;
            let cz = (c[2] / size + 0.5) * n as f32;
            let radius = r / size * n as f32;
            let fade = f / size * n as f32;
            for z in index(n, size, c[2] - r)..=index(n, size, c[2] + r) {
                if cancelled() {
                    return Ok(None);
                }
                for x in index(n, size, c[0] - r)..=index(n, size, c[0] + r) {
                    visit()?;
                    let dx = x as f32 + 0.5 - cx;
                    let dz = z as f32 + 0.5 - cz;
                    let d = (dx * dx + dz * dz).sqrt();
                    let applies = if fade == 0. {
                        d < radius
                    } else {
                        ((radius - d) / fade).clamp(0., 1.) > 0.5
                    };
                    if !applies {
                        continue;
                    }
                    let i = z * n + x;
                    let old = *writes.get(&i).unwrap_or(&self.cells[i]) as u32;
                    // Set cannot erase unknown future bits silently: reject the whole proposal.
                    if matches!(m.operation, Operation::Set) && old & !self.report.known_mask != 0 {
                        return Err(Failure::Unsupported.into());
                    }
                    let next = match m.operation {
                        Operation::Add => old | m.mask,
                        Operation::Set => m.mask,
                    } as i32;
                    if monotone && next != self.cells[i] {
                        check(i).map_err(CheckedFailure::Cell)?;
                        if self.locks[i] {
                            return Err(Failure::Conflict.into());
                        }
                    }
                    writes.insert(i, next);
                    if writes.len() > 262144 {
                        return Err(Failure::Unsupported.into());
                    }
                }
            }
        }
        writes.retain(|i, v| *v != self.cells[*i]);
        if writes.keys().any(|i| self.locks[*i]) {
            return Err(Failure::Conflict.into());
        }
        if !monotone {
            for &cell in writes.keys() {
                check(cell).map_err(CheckedFailure::Cell)?;
            }
        }
        reads.sort_unstable();
        reads.dedup();
        Ok(Some(Plan {
            writes: writes.into_iter().collect(),
            reads,
            operations: prefab.modifiers.len(),
        }))
    }
    /// Chain placement checks every piece before publishing any of its modifiers.
    /// Keep earlier objects locked while deferring this chain's read dependencies.
    pub(crate) fn begin_group(&mut self) {
        debug_assert!(self.group.is_none());
        self.group = Some(Group::default());
    }
    pub(crate) fn end_group(&mut self) {
        let group = self.group.take().expect("active connected topology group");
        self.lock_reads(group.reads);
    }
    pub fn commit(&mut self, plan: Plan) {
        if plan.operations > 0 {
            self.report.instances += 1;
        }
        self.report.operations += plan.operations;
        self.report.changed_cells += plan.writes.len();
        for (i, v) in plan.writes {
            if let Some(group) = &mut self.group {
                group.original.entry(i).or_insert(self.cells[i]);
            }
            self.cells[i] = v;
        }
        if let Some(group) = &mut self.group {
            group.reads.extend(plan.reads);
            return;
        }
        self.lock_reads(plan.reads);
    }
    fn lock_reads(&mut self, reads: impl IntoIterator<Item = usize>) {
        for i in reads {
            if !self.locks[i] {
                self.locks[i] = true;
                self.report.read_locked_cells += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn addition_probes_only_check_cells_in_the_literal_final_write_plan() {
        let cells: Vec<_> = (0..256).map(|i| if i % 3 == 0 { 3 } else { 1 }).collect();
        let state = State::new(&cells, 255);
        for angle in [0_f32, 17., 89., 133.] {
            for fade in [0., 1., 3., 6.] {
                let mut pose = pose();
                let half = angle.to_radians() * 0.5;
                pose.rotation = [0., half.sin(), 0., half.cos()];
                pose.scale = [0.8, 1.1, 1.2];
                let mut modifier = modifier(Operation::Add, 2);
                modifier.position = [0.7, 0., -0.4];
                modifier.fade = fade;
                let prefab = Prefab {
                    modifiers: vec![modifier],
                    ..Default::default()
                };
                let plan = state
                    .prepare(&prefab, &pose, &stage(), 16, 16., &mut 0, 100000, &|| false)
                    .unwrap()
                    .unwrap();
                let mut checked = Vec::new();
                let mut visits = 0;
                state
                    .precheck_additions(
                        &prefab,
                        &pose,
                        16,
                        16.,
                        &mut visits,
                        100000,
                        &|| false,
                        |cell| {
                            checked.push(cell);
                            Ok::<_, &str>(())
                        },
                    )
                    .unwrap()
                    .unwrap();
                assert!(
                    checked
                        .iter()
                        .all(|cell| plan.writes.iter().any(|(written, _)| written == cell))
                );
                assert_eq!(visits, 0);
                assert_eq!(state.cells, cells);
                assert!(state.locks.iter().all(|locked| !locked));
            }
        }
    }
    #[test]
    fn addition_probes_preserve_restoration_noops_cancellation_and_bounds() {
        let state = State::new(&[3; 256], 255);
        for modifiers in [
            vec![modifier(Operation::Add, 2)],
            vec![modifier(Operation::Add, 4), modifier(Operation::Set, 3)],
        ] {
            let prefab = Prefab {
                modifiers,
                ..Default::default()
            };
            state
                .precheck_additions(&prefab, &pose(), 16, 16., &mut 0, 1, &|| false, |_| {
                    Err("must not check restored or unchanged cells")
                })
                .unwrap()
                .unwrap();
        }
        let state = State::new(&[1; 256], 255);
        let prefab = Prefab {
            modifiers: vec![modifier(Operation::Add, 2)],
            ..Default::default()
        };
        assert!(
            state
                .precheck_additions(&prefab, &pose(), 16, 16., &mut 0, 1, &|| true, |_| Err(
                    "cancelled"
                ))
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            state.precheck_additions(&prefab, &pose(), 16, 16., &mut 0, 0, &|| false, |_| Err(
                "occupied"
            )),
            Err(CheckedFailure::Topology(Failure::Budget))
        ));
        let mut outside = pose();
        outside.position[0] = 7.;
        state
            .precheck_additions(&prefab, &outside, 16, 16., &mut 0, 1, &|| false, |_| {
                Err("normal planner validates bounds")
            })
            .unwrap()
            .unwrap();
        assert!(matches!(
            state.prepare(&prefab, &outside, &stage(), 16, 16., &mut 0, 10000, &|| {
                false
            }),
            Err(Failure::Outside)
        ));
    }
    #[test]
    fn addition_probes_reject_before_large_query_dependency_scans() {
        let state = State::new(&vec![1; 64 * 64], 255);
        let prefab = Prefab {
            modifiers: vec![modifier(Operation::Add, 2)],
            queries: vec![Query {
                check_placement: false,
                position: [0.; 3],
                radius: 20.,
                filter: filter(),
            }],
            ..Default::default()
        };
        let mut visits = 0;
        assert!(matches!(
            state.precheck_additions(&prefab, &pose(), 64, 64., &mut visits, 1, &|| false, |_| {
                Err("occupied")
            }),
            Err(CheckedFailure::Cell("occupied"))
        ));
        assert_eq!(visits, 1);
        assert!(matches!(
            state.prepare(&prefab, &pose(), &stage(), 64, 64., &mut 0, 1, &|| false),
            Err(Failure::Budget)
        ));
        assert!(state.locks.iter().all(|locked| !locked));
    }
    fn stage() -> decor::Stage {
        serde_json::from_value(serde_json::json!({"id":"one","order":0,"name":"Rocks","kind":"white_noise","folder":"decor/rocks","enabled":true,"mode":-1,"filter":{"SplatType":-1,"BiomeType":-1,"TopologyAny":-1,"TopologyAll":0,"TopologyNot":0},"filter_mode":0,"anchor_mode":0,"distance":0.,"dithering":0.,"density":10.,"cluster":null,"prefabs":["a"]})).unwrap()
    }
    fn pose() -> decor_fit::Pose {
        decor_fit::Pose {
            candidate: 0,
            position: [0.; 3],
            rotation: [0., 0., 0., 1.],
            scale: [1.; 3],
        }
    }
    fn modifier(operation: Operation, mask: u32) -> Modifier {
        Modifier {
            position: [0.; 3],
            radius: 3.,
            fade: 0.,
            mask,
            operation,
        }
    }
    fn plan(s: &State, p: &Prefab) -> Result<Option<Plan>, Failure> {
        s.prepare(p, &pose(), &stage(), 16, 16., &mut 0, 100000, &|| false)
    }
    fn filter() -> spawn::Filter {
        spawn::Filter {
            splat_type: -1,
            biome_type: -1,
            topology_any: -1,
            topology_all: 1,
            topology_not: 2,
        }
    }
    #[test]
    fn connected_groups_read_original_topology_and_preserve_prior_locks() {
        let mut state = State::new(&[1; 256], 255);
        let piece = Prefab {
            modifiers: vec![modifier(Operation::Add, 2)],
            queries: vec![Query {
                check_placement: false,
                position: [0.; 3],
                radius: 2.,
                filter: filter(),
            }],
            ..Default::default()
        };
        state.begin_group();
        state.commit(plan(&state, &piece).unwrap().unwrap());
        assert!(state.cells.contains(&3));
        assert_eq!(state.report.read_locked_cells, 0);
        // This query forbids the bit authored by the preceding chain piece.
        state.commit(plan(&state, &piece).unwrap().unwrap());
        state.end_group();
        assert!(state.report.read_locked_cells > 0);
        assert!(matches!(plan(&state, &piece), Err(Failure::Filter)));
        let change = Prefab {
            modifiers: vec![modifier(Operation::Add, 4)],
            ..Default::default()
        };
        state.begin_group();
        assert!(matches!(plan(&state, &change), Err(Failure::Conflict)));
        state.end_group();
        // Ending a group with no writes still preserves its accepted reads.
        let mut empty = State::new(&[1; 256], 255);
        empty.begin_group();
        let query = Prefab {
            modifiers: vec![],
            ..piece
        };
        empty.commit(plan(&empty, &query).unwrap().unwrap());
        empty.end_group();
        assert!(matches!(plan(&empty, &change), Err(Failure::Conflict)));
    }
    #[test]
    fn add_preserves_existing_bits_and_set_obeys_recorded_component_order() {
        let mut s = State::new(&[1; 256], 255);
        let mut p = Prefab {
            modifiers: vec![modifier(Operation::Set, 4), modifier(Operation::Add, 2)],
            ..Default::default()
        };
        let q = plan(&s, &p).ok().flatten().unwrap();
        assert!(!q.writes.is_empty());
        assert!(q.writes.iter().all(|(_, v)| *v == 6));
        s.commit(q);
        p.modifiers.reverse();
        let q = plan(&s, &p).ok().flatten().unwrap();
        assert!(q.writes.iter().all(|(_, v)| *v == 4));
        let s = State::new(&[i32::MIN | 1; 256], 255);
        p.modifiers = vec![modifier(Operation::Add, 2)];
        let q = plan(&s, &p).ok().flatten().unwrap();
        assert!(q.writes.iter().all(|(_, v)| *v == i32::MIN | 3));
        p.modifiers = vec![modifier(Operation::Set, 2)];
        assert!(matches!(plan(&s, &p), Err(Failure::Unsupported)));
    }
    #[test]
    fn later_candidates_see_committed_masks_and_cannot_invalidate_earlier_reads() {
        let mut s = State::new(&[1; 256], 255);
        let p = Prefab {
            queries: vec![Query {
                check_placement: false,
                position: [0.; 3],
                radius: 2.,
                filter: filter(),
            }],
            ..Default::default()
        };
        let q = plan(&s, &p).ok().flatten().unwrap();
        s.commit(q);
        assert!(s.report.read_locked_cells > 1);
        let effect = Prefab {
            modifiers: vec![modifier(Operation::Add, 2)],
            ..Default::default()
        };
        assert!(matches!(plan(&s, &effect), Err(Failure::Conflict)));
        assert!(s.cells.iter().all(|v| *v == 1));
        let mut s = State::new(&[1; 256], 255);
        s.commit(plan(&s, &effect).ok().flatten().unwrap());
        assert!(matches!(plan(&s, &p), Err(Failure::Filter)));
    }
    #[test]
    fn failed_plans_cancel_and_budget_do_not_commit_reads_or_writes() {
        let s = State::new(&[1; 256], 255);
        let p = Prefab {
            modifiers: vec![modifier(Operation::Add, 2)],
            ..Default::default()
        };
        assert!(
            s.prepare(&p, &pose(), &stage(), 16, 16., &mut 0, 1000, &|| true)
                .ok()
                .flatten()
                .is_none()
        );
        assert!(matches!(
            s.prepare(&p, &pose(), &stage(), 16, 16., &mut 0, 2, &|| false),
            Err(Failure::Budget)
        ));
        assert!(s.cells.iter().all(|v| *v == 1));
        assert_eq!(s.report.changed_cells, 0);
        assert_eq!(s.report.read_locked_cells, 0);
    }
    #[test]
    fn forbidden_query_bits_reject_early_without_committing_partial_reads() {
        let mut cells = [1; 256];
        cells[6 * 16 + 8] = 3;
        let s = State::new(&cells, 255);
        let p = Prefab {
            queries: vec![Query {
                check_placement: false,
                position: [0.; 3],
                radius: 2.,
                filter: filter(),
            }],
            modifiers: vec![modifier(Operation::Add, 4)],
            ..Default::default()
        };
        let mut visits = 0;
        assert!(matches!(
            s.prepare(&p, &pose(), &stage(), 16, 16., &mut visits, 3, &|| false),
            Err(Failure::Filter)
        ));
        assert_eq!(visits, 3);
        assert_eq!(s.cells, cells);
        assert!(s.locks.iter().all(|locked| !locked));
        assert_eq!(s.report.changed_cells, 0);
    }
    #[test]
    fn successful_query_keeps_every_sampled_dependency() {
        let s = State::new(&[1; 256], 255);
        let p = Prefab {
            queries: vec![Query {
                check_placement: false,
                position: [0.; 3],
                radius: 2.,
                filter: filter(),
            }],
            ..Default::default()
        };
        let plan = plan(&s, &p).ok().flatten().unwrap();
        let expected: Vec<_> = (6_usize..=10)
            .flat_map(|z| (6_usize..=10).map(move |x| (x, z)))
            .filter(|(x, z)| x.abs_diff(8).pow(2) + z.abs_diff(8).pow(2) <= 4)
            .map(|(x, z)| z * 16 + x)
            .collect();
        assert_eq!(plan.reads, expected);
        assert!(plan.writes.is_empty());
    }
    #[test]
    fn add_only_plans_stop_at_a_rejected_final_reservation() {
        let s = State::new(&[1; 256], 255);
        let p = Prefab {
            modifiers: vec![modifier(Operation::Add, 2)],
            ..Default::default()
        };
        let mut visits = 0;
        let mut checked = 0;
        let result = s.prepare_checked(
            &p,
            &pose(),
            &stage(),
            16,
            16.,
            &mut visits,
            10,
            &|| false,
            |_| {
                checked += 1;
                Err("occupied")
            },
        );
        assert!(matches!(result, Err(CheckedFailure::Cell("occupied"))));
        assert_eq!(checked, 1);
        assert!(visits <= 10);
        assert!(s.cells.iter().all(|value| *value == 1));
        assert!(s.locks.iter().all(|locked| !locked));
    }
    #[test]
    fn checked_plans_preserve_final_ordered_writes_including_restoration() {
        let s = State::new(&[1; 256], 255);
        for modifiers in [
            vec![modifier(Operation::Add, 2), modifier(Operation::Add, 4)],
            vec![modifier(Operation::Add, 2), modifier(Operation::Set, 1)],
            vec![modifier(Operation::Set, 2), modifier(Operation::Add, 4)],
        ] {
            let p = Prefab {
                modifiers,
                ..Default::default()
            };
            let expected = plan(&s, &p).ok().flatten().unwrap();
            let mut checked = Vec::new();
            let actual = s
                .prepare_checked(
                    &p,
                    &pose(),
                    &stage(),
                    16,
                    16.,
                    &mut 0,
                    100000,
                    &|| false,
                    |cell| {
                        checked.push(cell);
                        Ok::<_, ()>(())
                    },
                )
                .ok()
                .flatten()
                .unwrap();
            checked.sort_unstable();
            checked.dedup();
            assert_eq!(actual.writes, expected.writes);
            assert_eq!(actual.reads, expected.reads);
            assert_eq!(actual.operations, expected.operations);
            assert_eq!(
                checked,
                expected
                    .writes
                    .iter()
                    .map(|(cell, _)| *cell)
                    .collect::<Vec<_>>()
            );
        }
        let restored = Prefab {
            modifiers: vec![modifier(Operation::Add, 2), modifier(Operation::Set, 1)],
            ..Default::default()
        };
        let result = s
            .prepare_checked(
                &restored,
                &pose(),
                &stage(),
                16,
                16.,
                &mut 0,
                100000,
                &|| false,
                |_| Err("must not reserve restored cells"),
            )
            .ok()
            .flatten()
            .unwrap();
        assert!(result.writes.is_empty());
    }
    #[test]
    fn rotated_component_centers_and_y_scaled_fade_use_strict_radial_threshold() {
        let s = State::new(&[1; 256], 255);
        let mut p = Prefab {
            modifiers: vec![modifier(Operation::Add, 2)],
            ..Default::default()
        };
        p.modifiers[0].position = [1., 0., 0.];
        p.modifiers[0].radius = 2.;
        p.modifiers[0].fade = 2.;
        let mut pose = pose();
        let a = 90_f32.to_radians() / 2.;
        pose.rotation = [0., a.sin(), 0., a.cos()];
        pose.scale = [3., 2., 4.];
        let q = s
            .prepare(&p, &pose, &stage(), 16, 16., &mut 0, 10000, &|| false)
            .ok()
            .flatten()
            .unwrap();
        let cells: Vec<_> = q.writes.iter().map(|(i, _)| *i).collect();
        for z in 0..16 {
            for x in 0..16 {
                let d = (x as f32 + 0.5 - 8.).hypot(z as f32 + 0.5 - 5.);
                assert_eq!(cells.contains(&(z * 16 + x)), d < 2.);
            }
        }
        p.modifiers[0].radius = 1.;
        p.modifiers[0].fade = 2.;
        pose.position = [0.5, 0., 0.5];
        pose.rotation = [0., 0., 0., 1.];
        pose.scale = [1.; 3];
        p.modifiers[0].position = [0.; 3];
        assert!(
            s.prepare(&p, &pose, &stage(), 16, 16., &mut 0, 1000, &|| false)
                .ok()
                .flatten()
                .unwrap()
                .writes
                .is_empty()
        );
    }
    #[test]
    fn masks_are_bound_but_authored_protection_bits_are_allowed_on_free_ground() {
        let mut p = Prefab {
            modifiers: vec![modifier(Operation::Add, 256)],
            ..Default::default()
        };
        assert!(validate(&p, 255, 128).is_err());
        p.modifiers[0].mask = 128;
        assert!(validate(&p, 255, 128).is_ok());
        p.modifiers[0].mask = 2;
        assert!(validate(&p, 255, 128).is_ok());
        p.queries.push(Query {
            check_placement: false,
            position: [0.; 3],
            radius: 129.,
            filter: filter(),
        });
        assert!(validate(&p, 255, 128).is_err());
    }
}
