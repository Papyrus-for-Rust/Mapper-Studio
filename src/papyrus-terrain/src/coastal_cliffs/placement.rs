use super::{Parameters, Prefab, Program};
use crate::{
    decor::{self, Candidate, Stage, StageReport},
    decor_fit::{
        self, CoastalRequest, OrientedOutcome, OrientedPose, OrientedRejection, StaticRequest,
        ValidatedStage,
    },
    decor_live::{Context, Instance, Report},
    spawn, terrain_normal,
};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};

pub const VERSION: &str = "papyrus-coastal-cliffs-1";
type V = [f32; 3];
const OFFSETS: [V; 5] = [
    [0.; 3],
    [-0.75, 0., 0.],
    [0.75, 0., 0.],
    [0., 0., -0.75],
    [0., 0., 0.75],
];

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Male,
    Female,
}
impl Side {
    fn parent(self, prefab: &Prefab) -> Option<V> {
        match self {
            Self::Male => prefab.female,
            Self::Female => prefab.male,
        }
    }
    fn child(self, prefab: &Prefab) -> Option<V> {
        match self {
            Self::Male => prefab.male,
            Self::Female => prefab.female,
        }
    }
}
#[derive(Serialize)]
pub struct Join {
    pub parent: usize,
    pub child: usize,
    pub side: Side,
    pub gap: f32,
}
#[derive(Default, Serialize)]
pub struct PlacementReport {
    pub roots: usize,
    pub joins: Vec<Join>,
    pub rejected_suffixes: usize,
    pub admission_rejections: BTreeMap<&'static str, usize>,
    pub memo_hits: usize,
    pub family_memo_hits: usize,
}
#[derive(Debug)]
enum Stop {
    Cancelled,
    Error(String),
}
impl From<String> for Stop {
    fn from(value: String) -> Self {
        Self::Error(value)
    }
}
struct Chain {
    index: usize,
    pose: OrientedPose,
    next: Option<Box<Chain>>,
    count: u32,
    score: u32,
}
fn add(a: V, b: V) -> V {
    std::array::from_fn(|i| a[i] + b[i])
}
fn sub(a: V, b: V) -> V {
    std::array::from_fn(|i| a[i] - b[i])
}
fn mul(a: V, b: V) -> V {
    std::array::from_fn(|i| a[i] * b[i])
}
fn socket(pose: OrientedPose, point: V) -> V {
    add(
        pose.position,
        decor_fit::rotate(pose.rotation, mul(point, pose.scale)),
    )
}
fn scales(parameters: &Parameters) -> Vec<f32> {
    let delta = ((parameters.maximum_scale - parameters.minimum_scale) / 10.).max(0.1);
    let mut value = parameters.maximum_scale;
    let mut out = Vec::with_capacity(12);
    while value >= parameters.minimum_scale {
        out.push(value);
        value -= delta;
    }
    out
}
struct Runner<'p, 'ctx, 'context, 'terrain, F: Fn() -> bool> {
    program: &'p Program,
    placements: &'p BTreeMap<String, Prefab>,
    initial_token: ValidatedStage<'p>,
    repeat_token: ValidatedStage<'p>,
    repeat: &'p Stage,
    context: &'ctx mut Context<'context, 'terrain>,
    cancelled: F,
    seed: u32,
    random: u64,
    scales: Vec<Vec<f32>>,
    initial: Vec<usize>,
    male: Vec<usize>,
    female: Vec<usize>,
    memo: HashMap<[u32; 11], Option<OrientedPose>>,
    family_memo: HashMap<[u32; 9], Option<(OrientedPose, i32)>>,
    report: Report,
}
impl<F: Fn() -> bool> Runner<'_, '_, '_, '_, F> {
    fn tick(&mut self) -> Result<(), Stop> {
        if (self.cancelled)() {
            return Err(Stop::Cancelled);
        }
        self.context.attempts.take().map_err(|error| Stop::Error(format!(
            "Coastal cliff search {} exhausted its allowance after {} root attempts, {} fitting trials and {} family memo hits. {error}",
            self.program.stage.id, self.report.sampling.attempted, self.report.fitting_attempts,
            self.report.coastal.as_ref().unwrap().family_memo_hits,
        )))?;
        Ok(())
    }
    fn parameters(&self, index: usize) -> &Parameters {
        self.placements[&self.program.stage.prefabs[index]]
            .parameters
            .as_ref()
            .unwrap_or(&self.program.parameters)
    }
    fn pivot(
        &mut self,
        position: V,
        index: usize,
        repeat: bool,
        threshold: f32,
    ) -> Result<Option<V>, Stop> {
        let view = self.context.transaction.view();
        let u = position[0] / view.size as f32 + 0.5;
        let v = position[2] / view.size as f32 + 0.5;
        if !(0. ..=1.).contains(&u) || !(0. ..=1.).contains(&v) {
            self.report.sampling.rejected_bounds += 1;
            return Ok(None);
        }
        let n = view.surface_resolution;
        let cell = ((v * n as f32) as usize).min(n - 1) * n + ((u * n as f32) as usize).min(n - 1);
        if self.context.alpha[cell] < 128 {
            self.report.sampling.rejected_holes += 1;
            return Ok(None);
        }
        let stage = if repeat {
            self.repeat
        } else {
            &self.program.stage
        };
        if stage.filter_mode & 1 != 0 {
            let factor = if view.placement_blocked(u, v) {
                0.
            } else {
                spawn::factor_view(view, &stage.filter, u, v)
            };
            if factor * factor < threshold {
                self.report.sampling.rejected_filter += 1;
                return Ok(None);
            }
        }
        let p = self.parameters(index);
        let height = spawn::height_view(view, u, v);
        if height < p.minimum_terrain_height as f32 || height > p.maximum_terrain_height as f32 {
            return Ok(None);
        }
        let normal =
            terrain_normal::sample(view.size, view.height_resolution, view.height, [u, v])?;
        let slope = normal[1].clamp(-1., 1.).acos().to_degrees();
        let cutoff = if repeat {
            p.cutoff_slope_repeat
        } else {
            p.cutoff_slope_initial
        };
        Ok((slope >= cutoff as f32).then_some(normal))
    }
    fn fit(
        &mut self,
        index: usize,
        pose: OrientedPose,
        repeat: bool,
    ) -> Result<Option<OrientedPose>, Stop> {
        self.report.fitting_attempts += 1;
        let stage = if repeat {
            self.repeat
        } else {
            &self.program.stage
        };
        let candidate = Candidate {
            stage: stage.order,
            prefab: index,
            position: pose.position,
        };
        let result = self.context.fitting.try_fit_coastal_cliff(
            self.context.transaction.view(),
            CoastalRequest {
                request: StaticRequest {
                    candidate: &candidate,
                    stage,
                    prefab: &self.context.prefabs[&stage.prefabs[index]],
                    seed: self.seed,
                    water: self.context.water,
                },
                pose,
                repeat,
            },
            if repeat {
                &self.repeat_token
            } else {
                &self.initial_token
            },
            &self.cancelled,
        )?;
        match result {
            None => Err(Stop::Cancelled),
            Some(OrientedOutcome::Fitted(pose)) => {
                let p = self.parameters(index);
                Ok((pose.position[1] >= p.minimum_cliff_height as f32
                    && pose.position[1] <= p.maximum_cliff_height as f32)
                    .then_some(pose))
            }
            Some(OrientedOutcome::Rejected(OrientedRejection::Budget)) => {
                Err(Stop::Error(format!(
                    "Coastal cliff fitting exhausted its allowance in {} after {} samples",
                    stage.name,
                    self.context.fitting.samples()
                )))
            }
            Some(OrientedOutcome::Rejected(reason)) => {
                reason.record(&mut self.report.rejected_fits);
                Ok(None)
            }
        }
    }
    fn repeat_pose(
        &mut self,
        index: usize,
        pose: OrientedPose,
    ) -> Result<Option<OrientedPose>, Stop> {
        self.tick()?;
        let mut key = [0; 11];
        key[0] = index as u32;
        key[1..4].copy_from_slice(&pose.position.map(f32::to_bits));
        key[4..8].copy_from_slice(&pose.rotation.map(f32::to_bits));
        key[8..11].copy_from_slice(&pose.scale.map(f32::to_bits));
        if let Some(result) = self.memo.get(&key) {
            self.report.coastal.as_mut().unwrap().memo_hits += 1;
            return Ok(*result);
        }
        let result = if self.pivot(pose.position, index, true, 0.5)?.is_some() {
            self.fit(index, pose, true)?
        } else {
            None
        };
        if self.memo.len() < 65536 {
            self.memo.insert(key, result);
        }
        Ok(result)
    }
    fn next_pose(
        &mut self,
        side: Side,
        parent: OrientedPose,
        parent_socket: V,
        index: usize,
    ) -> Result<Option<(OrientedPose, i32)>, Stop> {
        self.tick()?;
        let mut key = [0; 9];
        key[0] = index as u32;
        key[1] = matches!(side, Side::Female) as u32;
        key[2..5].copy_from_slice(&parent_socket.map(f32::to_bits));
        key[5..9].copy_from_slice(&parent.rotation.map(f32::to_bits));
        if let Some(result) = self.family_memo.get(&key) {
            self.report.coastal.as_mut().unwrap().family_memo_hits += 1;
            return Ok(*result);
        }
        let result = self.search_family(side, parent, parent_socket, index)?;
        if self.family_memo.len() < 65536 {
            self.family_memo.insert(key, result);
        }
        Ok(result)
    }
    fn search_family(
        &mut self,
        side: Side,
        parent: OrientedPose,
        parent_socket: V,
        index: usize,
    ) -> Result<Option<(OrientedPose, i32)>, Stop> {
        let child = side
            .child(&self.placements[&self.program.stage.prefabs[index]])
            .unwrap();
        let root_scale = self.context.prefabs[&self.program.stage.prefabs[index]]
            .metadata()
            .scale;
        for scale_index in 0..self.scales[index].len() {
            let scale = root_scale.map(|v| v * self.scales[index][scale_index]);
            for angle in (10..=60).step_by(10) {
                for sign in [-1, 1] {
                    let rotation = decor_fit::qmul(
                        decor_fit::euler([0., (sign * angle) as f32, 0.]),
                        parent.rotation,
                    );
                    for offset in OFFSETS {
                        let position = sub(
                            parent_socket,
                            decor_fit::rotate(rotation, add(mul(child, scale), offset)),
                        );
                        if let Some(pose) = self.repeat_pose(
                            index,
                            OrientedPose {
                                position,
                                rotation,
                                scale,
                            },
                        )? {
                            return Ok(Some((pose, angle)));
                        }
                    }
                }
            }
        }
        Ok(None)
    }
    fn chain(
        &mut self,
        side: Side,
        parent_index: usize,
        parent: OrientedPose,
        angle: i32,
        count: u32,
        score: u32,
    ) -> Result<Option<Box<Chain>>, Stop> {
        if angle > 160 {
            return Ok(None);
        }
        self.tick()?;
        let pool_len = match side {
            Side::Male => self.male.len(),
            Side::Female => self.female.len(),
        };
        if pool_len == 0 {
            return Ok(None);
        }
        let start = ((decor::unit(&mut self.random) * pool_len as f32) as usize).min(pool_len - 1);
        let parent_socket = socket(
            parent,
            side.parent(&self.placements[&self.program.stage.prefabs[parent_index]])
                .unwrap(),
        );
        let mut best: Option<Box<Chain>> = None;
        for offset in 0..pool_len {
            let index = match side {
                Side::Male => self.male[(start + offset) % pool_len],
                Side::Female => self.female[(start + offset) % pool_len],
            };
            if index == parent_index {
                continue;
            }
            let continues = side
                .parent(&self.placements[&self.program.stage.prefabs[index]])
                .is_some();
            if continues
                && best.as_ref().is_some_and(|b| {
                    b.count > self.program.target_count && b.score > self.program.target_length
                })
            {
                continue;
            }
            let Some((pose, delta)) = self.next_pose(side, parent, parent_socket, index)? else {
                continue;
            };
            let mut child_count = count + 1;
            let mut child_score = score
                + (parent.position[0] - pose.position[0])
                    .hypot(parent.position[2] - pose.position[2])
                    .ceil() as u32;
            let next = if continues {
                self.chain(side, index, pose, angle + delta, child_count, child_score)?
            } else {
                child_score *= 2;
                None
            };
            if let Some(next) = &next {
                child_count = next.count;
                child_score = next.score;
            }
            if best.as_ref().is_none_or(|b| child_score > b.score) {
                best = Some(Box::new(Chain {
                    index,
                    pose,
                    next,
                    count: child_count,
                    score: child_score,
                }));
            }
        }
        Ok(best)
    }
    fn admit(
        &mut self,
        index: usize,
        pose: OrientedPose,
        repeat: bool,
    ) -> Result<Option<usize>, Stop> {
        self.tick()?;
        let id = *self.context.next_pose;
        *self.context.next_pose = id
            .checked_add(1)
            .ok_or_else(|| Stop::Error("Coastal admission ID overflow".into()))?;
        let path = &self.program.stage.prefabs[index];
        let accepted = self.context.transaction.try_apply(
            id,
            if repeat {
                self.repeat
            } else {
                &self.program.stage
            },
            path,
            &decor_fit::Pose {
                candidate: id,
                position: pose.position,
                rotation: pose.rotation,
                scale: pose.scale,
            },
            &self.cancelled,
        )?;
        match accepted {
            None => Err(Stop::Cancelled),
            Some(false) => {
                self.report.rejected_admissions += 1;
                let reason = self
                    .context
                    .transaction
                    .last_rejection()
                    .unwrap_or("unknown");
                *self
                    .report
                    .coastal
                    .as_mut()
                    .unwrap()
                    .admission_rejections
                    .entry(reason)
                    .or_default() += 1;
                Ok(None)
            }
            Some(true) => {
                if self.report.admitted.len() >= 100000 {
                    return Err(Stop::Error(
                        "Coastal cliff instance allowance exhausted".into(),
                    ));
                }
                self.report.admitted.push(Instance {
                    transaction_pose: id,
                    prefab: path.clone(),
                    transform: pose,
                });
                Ok(Some(id))
            }
        }
    }
    fn admit_chain(
        &mut self,
        side: Side,
        root_index: usize,
        root_pose: OrientedPose,
        root_id: usize,
        mut chain: Option<Box<Chain>>,
    ) -> Result<usize, Stop> {
        let (mut parent_index, mut parent_pose, mut parent_id) = (root_index, root_pose, root_id);
        let mut count = 0;
        while let Some(node) = chain {
            let Some(id) = self.admit(node.index, node.pose, true)? else {
                self.report.coastal.as_mut().unwrap().rejected_suffixes += 1;
                break;
            };
            let a = socket(
                parent_pose,
                side.parent(&self.placements[&self.program.stage.prefabs[parent_index]])
                    .unwrap(),
            );
            let b = socket(
                node.pose,
                side.child(&self.placements[&self.program.stage.prefabs[node.index]])
                    .unwrap(),
            );
            let gap = sub(a, b).iter().map(|v| v * v).sum::<f32>().sqrt();
            self.report.coastal.as_mut().unwrap().joins.push(Join {
                parent: parent_id,
                child: id,
                side,
                gap,
            });
            count += 1;
            (parent_index, parent_pose, parent_id) = (node.index, node.pose, id);
            chain = node.next;
        }
        Ok(count)
    }
    fn run(&mut self) -> Result<(), Stop> {
        let mut progress = 0;
        let half = self.context.transaction.view().size as f32 * 0.5;
        while progress < self.report.sampling.requested_attempts {
            self.tick()?;
            progress += 1;
            self.report.sampling.attempted += 1;
            let x = (decor::unit(&mut self.random) * 2. - 1.) * half;
            let z = (decor::unit(&mut self.random) * 2. - 1.) * half;
            let threshold = decor::unit(&mut self.random);
            let choice = ((decor::unit(&mut self.random) * self.initial.len() as f32) as usize)
                .min(self.initial.len() - 1);
            let index = self.initial[choice];
            let Some(normal) = self.pivot([x, 0., z], index, false, threshold)? else {
                continue;
            };
            self.report.sampling.candidates += 1;
            let view = self.context.transaction.view();
            let height = spawn::height_view(view, x / (half * 2.) + 0.5, z / (half * 2.) + 0.5);
            let root = self.context.prefabs[&self.program.stage.prefabs[index]].metadata();
            let Some(basis) = decor_fit::look(normal, [0., 1., 0.]) else {
                continue;
            };
            let rotation = decor_fit::qmul(basis, root.rotation);
            let root_scale = root.scale;
            for scale_index in 0..self.scales[index].len() {
                self.tick()?;
                let scale = root_scale.map(|v| v * self.scales[index][scale_index]);
                let Some(pose) = self.fit(
                    index,
                    OrientedPose {
                        position: [x, height, z],
                        rotation,
                        scale,
                    },
                    false,
                )?
                else {
                    continue;
                };
                self.report.sampling.passing_candidates += 1;
                self.memo.clear();
                self.family_memo.clear();
                let male = self.chain(Side::Male, index, pose, 0, 0, 0)?;
                let female = self.chain(Side::Female, index, pose, 0, 0, 0)?;
                self.context.transaction.begin_connected_group()?;
                if let Some(id) = self.admit(index, pose, false)? {
                    self.report.coastal.as_mut().unwrap().roots += 1;
                    progress += self.admit_chain(Side::Male, index, pose, id, male)?;
                    progress += self.admit_chain(Side::Female, index, pose, id, female)?;
                }
                self.context.transaction.end_connected_group()?;
                break;
            }
        }
        Ok(())
    }
}

fn emit_inner(
    program: &Program,
    placements: &BTreeMap<String, Prefab>,
    seed: u32,
    realm: u32,
    context: &mut Context<'_, '_>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    if cancelled() {
        context.transaction.cancel();
        return Ok(None);
    }
    program.validate()?;
    let mut repeat = program.stage.clone();
    repeat.anchor_mode = program.repeat_anchor_mode;
    repeat.filter_mode = program.repeat_filter_mode;
    let initial_token = ValidatedStage::new(&program.stage)?;
    let repeat_token = ValidatedStage::new(&repeat)?;
    let mut initial = Vec::new();
    let mut male = Vec::new();
    let mut female = Vec::new();
    let mut all_scales = Vec::new();
    for (index, path) in program.stage.prefabs.iter().enumerate() {
        let p = placements
            .get(path)
            .ok_or("Missing coastal socket metadata")?;
        p.validate()?;
        if !context.prefabs.contains_key(path) {
            return Err("Missing coastal fitting metadata".into());
        }
        if p.male.is_some() {
            male.push(index);
        }
        if p.female.is_some() {
            female.push(index);
        }
        if p.male.is_some() && p.female.is_some() {
            initial.push(index);
        }
        all_scales.push(scales(p.parameters.as_ref().unwrap_or(&program.parameters)));
    }
    let skipped = if !program.stage.enabled {
        Some("Disabled in source".into())
    } else if realm == 0 {
        return Err("Invalid coastal generation realm".into());
    } else if program.stage.mode as u32 & realm == 0 {
        Some("Source realm excludes generation".into())
    } else if initial.is_empty() {
        Some("Missing initial socket pair".into())
    } else {
        None
    };
    let size = context.transaction.view().size as f32;
    let attempts =
        (size * size * 0.001 * program.retry_multiplier as f32).round_ties_even() as usize;
    let stage_seed = decor::stage_seed(seed, &program.stage.id);
    let report = Report {
        coastal: Some(PlacementReport::default()),
        cliff_reservations: None,
        engine: VERSION,
        sampling: StageReport {
            id: program.stage.id.clone(),
            name: program.stage.name.clone(),
            seed: stage_seed,
            requested_attempts: attempts,
            attempted: 0,
            candidates: 0,
            passing_candidates: 0,
            rejected_filter: 0,
            rejected_bounds: 0,
            rejected_holes: 0,
            skipped,
            budget_exhausted: false,
            prefabs: program.stage.prefabs.clone(),
        },
        fitting_attempts: 0,
        rejected_fits: BTreeMap::new(),
        rejected_admissions: 0,
        admitted: Vec::new(),
        assembled_instances: 0,
        physics_validated: false,
    };
    if report.sampling.skipped.is_some() {
        return Ok(Some(report));
    }
    let mut runner = Runner {
        program,
        placements,
        initial_token,
        repeat_token,
        repeat: &repeat,
        context,
        cancelled,
        seed,
        random: stage_seed,
        scales: all_scales,
        initial,
        male,
        female,
        memo: HashMap::new(),
        family_memo: HashMap::new(),
        report,
    };
    match runner.run() {
        Ok(()) if (runner.cancelled)() => Ok(None),
        Ok(()) => Ok(Some(runner.report)),
        Err(Stop::Cancelled) => {
            runner.context.transaction.cancel();
            Ok(None)
        }
        Err(Stop::Error(error)) => {
            runner.context.transaction.invalidate();
            Err(error)
        }
    }
}

pub fn emit(
    program: &Program,
    placements: &BTreeMap<String, Prefab>,
    seed: u32,
    realm: u32,
    mut context: Context<'_, '_>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    let result = emit_inner(program, placements, seed, realm, &mut context, &cancelled);
    match &result {
        Ok(None) => context.transaction.cancel(),
        Err(_) => context.transaction.invalidate(),
        _ => {}
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        decor_fit::{OrientedBudget, PreparedPrefab},
        decor_stream::Budget,
        footprints::{self, RoadTransaction},
        native_layers::{Layers, dimensions},
    };
    const A: &str = "assets/bundled/prefabs/autospawn/decor/coast/a.prefab";
    const B: &str = "assets/bundled/prefabs/autospawn/decor/coast/b.prefab";
    const C: &str = "assets/bundled/prefabs/autospawn/decor/coast/c.prefab";
    fn layers() -> Layers {
        let (hr, n) = dimensions(1000).unwrap();
        Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: n,
            height: vec![crate::erosion::encode(20.); hr * hr],
            topology: vec![1; n * n],
            biome: vec![51; n * n * 5],
            splat: vec![32; n * n * 8],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        }
    }
    fn program() -> Program {
        serde_json::from_value(serde_json::json!({
            "stage":{"id":"604","order":0,"name":"Coast","kind":"white_noise","folder":"decor/coast",
                "enabled":true,"mode":-1,"filter":{"SplatType":-1,"BiomeType":-1,"TopologyAny":1,"TopologyAll":0,"TopologyNot":0},
                "filter_mode":1,"anchor_mode":2,"distance":0.,"dithering":0.,"density":0.,"cluster":null,"prefabs":[A,B,C]},
            "parameters":{"cutoff_slope_initial":0,"cutoff_slope_repeat":0,"minimum_terrain_height":0,"maximum_terrain_height":50,
                "minimum_cliff_height":0,"maximum_cliff_height":50,"minimum_scale":1.,"maximum_scale":1.},
            "retry_multiplier":1,"target_count":8,"target_length":0,"repeat_anchor_mode":1,"repeat_filter_mode":1
        })).unwrap()
    }
    fn placements() -> BTreeMap<String, Prefab> {
        BTreeMap::from([
            (
                A.into(),
                Prefab {
                    male: Some([5., 0., 0.]),
                    female: Some([-5., 0., 0.]),
                    parameters: None,
                },
            ),
            (
                B.into(),
                Prefab {
                    male: Some([5., -5., 0.]),
                    female: None,
                    parameters: None,
                },
            ),
            (
                C.into(),
                Prefab {
                    male: None,
                    female: Some([-5., -5., 0.]),
                    parameters: None,
                },
            ),
        ])
    }
    fn fitting() -> BTreeMap<String, PreparedPrefab> {
        [A, B, C]
            .into_iter()
            .map(|path| {
                (
                    path.into(),
                    PreparedPrefab::new(decor_fit::Prefab {
                        rotation: [0., 0., 0., 1.],
                        scale: [1.; 3],
                        transforms: vec![],
                        anchors: vec![],
                        filters: vec![],
                        unsupported: vec![],
                        pending: vec![],
                    })
                    .unwrap(),
                )
            })
            .collect()
    }
    fn transaction<'a>(l: &'a Layers, alpha: &'a [u8]) -> RoadTransaction<'a> {
        let p = footprints::Prefab {
            support: vec![],
            blockers: vec![],
            raises: vec![],
            topology: Some(crate::decor_topology::Prefab {
                modifiers: vec![crate::decor_topology::Modifier {
                    position: [0.; 3],
                    radius: 1.,
                    fade: 0.,
                    mask: 2,
                    operation: crate::decor_topology::Operation::Add,
                }],
                queries: vec![],
                anchors: vec![],
            }),
            materials: Some(vec![]),
            height_modifiers: Some(vec![crate::decor_height::Modifier {
                position: [0.; 3],
                radius: 1.,
                fade: 0.,
                opacity: 1.,
                mode: crate::decor_height::Mode::Add { delta: 0.5 },
            }]),
            contributors: Some(vec![]),
            footprint: None,
            unsupported: vec![],
            pending: vec![],
        };
        RoadTransaction::new_objects(
            l,
            alpha,
            [A, B, C]
                .into_iter()
                .map(|s| (s.into(), p.clone()))
                .collect(),
            11,
            8,
            || false,
        )
        .unwrap()
        .unwrap()
    }
    #[test]
    fn bidirectional_chains_keep_sloped_socket_heights_and_repeat_deterministically() {
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let program = program();
        let run = |seed, overrides| {
            let mut placements = placements();
            if overrides {
                let mut p = program.parameters.clone();
                p.minimum_cliff_height = 26;
                placements.get_mut(B).unwrap().parameters = Some(p);
            }
            let prefabs = fitting();
            let mut tx = transaction(&l, &alpha);
            let mut fitter = OrientedBudget::new(&l, &alpha).unwrap();
            let mut attempts = Budget::new(100000).unwrap();
            let mut next = 0;
            let report = emit(
                &program,
                &placements,
                seed,
                2,
                Context {
                    transaction: &mut tx,
                    alpha: &alpha,
                    fitting: &mut fitter,
                    prefabs: &prefabs,
                    attempts: &mut attempts,
                    next_pose: &mut next,
                    water: None,
                },
                || false,
            )
            .unwrap()
            .unwrap();
            let joins = &report.coastal.as_ref().unwrap().joins;
            assert!(report.coastal.as_ref().unwrap().roots > 0 && !joins.is_empty());
            assert!(joins.iter().all(|j| j.gap < 0.001));
            for join in joins {
                let parent = report
                    .admitted
                    .iter()
                    .find(|p| p.transaction_pose == join.parent)
                    .unwrap();
                let child = report
                    .admitted
                    .iter()
                    .find(|p| p.transaction_pose == join.child)
                    .unwrap();
                assert_eq!(parent.prefab, A);
                assert!(
                    (child.transform.position[1] - parent.transform.position[1] - 5.).abs() < 0.001
                );
            }
            if overrides {
                assert!(report.admitted.iter().all(|p| p.prefab != B));
            } else {
                assert!(joins.iter().any(|j| matches!(j.side, Side::Male)));
                assert!(joins.iter().any(|j| matches!(j.side, Side::Female)));
            }
            let value = serde_json::to_value(&report).unwrap();
            let output = tx.finish(|| false).unwrap().unwrap();
            assert!(
                output
                    .topology
                    .as_ref()
                    .unwrap()
                    .iter()
                    .any(|&v| v & 2 != 0)
            );
            value
        };
        assert_eq!(run(42, false), run(42, false));
        assert_ne!(run(42, false), run(43, false));
        let _ = run(42, true);
    }
    #[test]
    fn family_memo_reuses_positive_and_negative_searches_and_checks_cancellation() {
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let p = program();
        let placements = placements();
        let prefabs = fitting();
        let mut first = transaction(&l, &alpha);
        let mut fitting = OrientedBudget::new(&l, &alpha).unwrap();
        let mut attempts = Budget::new(1_000_000).unwrap();
        let mut next = 0;
        let report = emit(
            &p,
            &placements,
            42,
            2,
            Context {
                transaction: &mut first,
                alpha: &alpha,
                fitting: &mut fitting,
                prefabs: &prefabs,
                attempts: &mut attempts,
                next_pose: &mut next,
                water: None,
            },
            || false,
        )
        .unwrap()
        .unwrap();
        let mut tx = transaction(&l, &alpha);
        let mut context = Context {
            transaction: &mut tx,
            alpha: &alpha,
            fitting: &mut fitting,
            prefabs: &prefabs,
            attempts: &mut attempts,
            next_pose: &mut next,
            water: None,
        };
        let repeat = p.stage.clone();
        let cancelled = std::cell::Cell::new(false);
        let mut runner = Runner {
            program: &p,
            placements: &placements,
            initial_token: ValidatedStage::new(&p.stage).unwrap(),
            repeat_token: ValidatedStage::new(&repeat).unwrap(),
            repeat: &repeat,
            context: &mut context,
            cancelled: || cancelled.get(),
            seed: 42,
            random: 42,
            scales: vec![vec![1.]; 3],
            initial: vec![0],
            male: vec![0, 1],
            female: vec![0, 2],
            memo: HashMap::new(),
            family_memo: HashMap::new(),
            report,
        };
        let parent = OrientedPose {
            position: [0., 20., 0.],
            rotation: [0., 0., 0., 1.],
            scale: [1.; 3],
        };
        let socket = [0., 20., 0.];
        let positive = runner.next_pose(Side::Male, parent, socket, 0).unwrap();
        assert!(positive.is_some());
        let before = runner.context.attempts.used();
        let trials = runner.report.fitting_attempts;
        let cached = runner.next_pose(Side::Male, parent, socket, 0).unwrap();
        assert_eq!(
            serde_json::to_value(positive).unwrap(),
            serde_json::to_value(cached).unwrap()
        );
        assert_eq!(runner.context.attempts.used(), before + 1);
        assert_eq!(runner.report.fitting_attempts, trials);
        runner.family_memo.clear();
        runner.memo.clear();
        let uncached = runner.next_pose(Side::Male, parent, socket, 0).unwrap();
        assert_eq!(
            serde_json::to_value(positive).unwrap(),
            serde_json::to_value(uncached).unwrap()
        );
        let opposite = runner.next_pose(Side::Female, parent, socket, 0).unwrap();
        assert_ne!(
            serde_json::to_value(positive).unwrap(),
            serde_json::to_value(opposite).unwrap()
        );
        let outside = [2000., 20., 0.];
        assert!(
            runner
                .next_pose(Side::Male, parent, outside, 0)
                .unwrap()
                .is_none()
        );
        let before = runner.context.attempts.used();
        assert!(
            runner
                .next_pose(Side::Male, parent, outside, 0)
                .unwrap()
                .is_none()
        );
        assert_eq!(runner.context.attempts.used(), before + 1);
        cancelled.set(true);
        assert!(matches!(
            runner.next_pose(Side::Male, parent, socket, 0),
            Err(Stop::Cancelled)
        ));
    }
    #[test]
    fn connected_reservations_allow_joined_edits_and_protect_other_groups() {
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let program = program();
        let pose = |id| decor_fit::Pose {
            candidate: id,
            position: [0., 20., 0.],
            rotation: [0., 0., 0., 1.],
            scale: [1.; 3],
        };
        for grouped in [false, true] {
            let mut tx = transaction(&l, &alpha);
            if grouped {
                tx.begin_connected_group().unwrap();
            }
            assert_eq!(
                tx.try_apply(0, &program.stage, A, &pose(0), || false)
                    .unwrap(),
                Some(true)
            );
            assert_eq!(
                tx.try_apply(1, &program.stage, A, &pose(1), || false)
                    .unwrap(),
                Some(grouped)
            );
            if grouped {
                tx.end_connected_group().unwrap();
            }
            assert_eq!(
                tx.try_apply(2, &program.stage, A, &pose(2), || false)
                    .unwrap(),
                Some(false)
            );
            let output = tx.finish(|| false).unwrap().unwrap();
            assert_eq!(output.report.counts.accepted, if grouped { 2 } else { 1 });
        }
        let mut unfinished = transaction(&l, &alpha);
        unfinished.begin_connected_group().unwrap();
        assert!(unfinished.finish(|| false).is_err());
        let mut cancelled = transaction(&l, &alpha);
        cancelled.begin_connected_group().unwrap();
        cancelled.cancel();
        assert!(cancelled.finish(|| false).unwrap().is_none());
        let mut nested = transaction(&l, &alpha);
        nested.begin_connected_group().unwrap();
        assert!(nested.begin_connected_group().is_err());
        assert!(nested.finish(|| false).is_err());
    }
    #[test]
    fn coastal_cancellation_budget_and_invalid_metadata_prevent_publication() {
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let p = program();
        let prefabs = fitting();
        for mode in 0..3 {
            let mut data = placements();
            if mode == 2 {
                data.remove(A);
            }
            let mut tx = transaction(&l, &alpha);
            let mut fitter = OrientedBudget::new(&l, &alpha).unwrap();
            let mut attempts = Budget::new(if mode == 0 { 1 } else { 100000 }).unwrap();
            let mut next = 0;
            let polls = std::cell::Cell::new(0);
            let result = emit(
                &p,
                &data,
                42,
                2,
                Context {
                    transaction: &mut tx,
                    alpha: &alpha,
                    fitting: &mut fitter,
                    prefabs: &prefabs,
                    attempts: &mut attempts,
                    next_pose: &mut next,
                    water: None,
                },
                || {
                    polls.set(polls.get() + 1);
                    mode == 1 && polls.get() == 200
                },
            );
            if mode == 1 {
                assert!(result.unwrap().is_none());
                assert!(tx.finish(|| false).unwrap().is_none());
            } else {
                assert!(result.is_err());
                assert!(tx.finish(|| false).is_err());
            }
        }
    }
}
