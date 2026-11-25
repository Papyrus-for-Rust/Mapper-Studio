//! Independent, transactional correction of already generated road terrain.
//! Adds a tapered height-difference field; source-style road carving is retained.
use crate::{
    path_geometry::Path,
    path_terrain::{self, Grid},
    road_bed,
    road_grade::{self, Junction},
};
use serde::Serialize;
use std::collections::BTreeMap;
pub const VERSION: &str = "papyrus-road-grade-terrain-3";
pub const COUPLED_VERSION: &str = "papyrus-road-grade-terrain-4";
pub const RESERVE_RECOVERY_VERSION: &str = "papyrus-road-grade-terrain-5";
#[derive(Serialize)]
pub struct ProposalRecovery {
    pub attempts: usize,
    pub initial_height_reserve: f32,
    pub accepted_height_reserve: f32,
    pub work_before_accepted_attempt: usize,
}
#[derive(Debug, PartialEq)]
pub enum Failure {
    Invalid,
    Budget,
    Cancelled,
    Infeasible(&'static str),
    ControlProposal(road_grade::ControlConflict),
    ProtectedSourceEarthwork {
        sample: usize,
        world: [f32; 2],
        ground_units: i16,
        proposed_units: i16,
        topology: i32,
        retained: bool,
    },
    ControlGrade(f64),
    ControlFit(road_bed::Failure),
}
pub struct Road<'a> {
    pub path: &'a Path,
    pub radius: f32,
    pub shoulder: f32,
}
#[derive(Clone, Copy)]
pub struct Input<'a> {
    pub grid: Grid<'a>,
    /// Stamped terrain before source-style road height loops.
    pub ground: &'a [i16],
    pub topology: &'a [i32],
    pub surface_resolution: usize,
    pub protected_topology: i32,
    pub roads: &'a [Road<'a>],
    pub junctions: &'a [Junction],
}
#[derive(Serialize)]
pub struct Report {
    pub engine: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retained_sample_entries: Option<usize>,
    pub control_grade_attempts: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampled_control_fit: Option<road_bed::Report>,
    pub proposal: road_grade::Report,
    pub changed_samples: usize,
    pub source_earthwork_overruns: usize,
    pub source_earthwork_clamped: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correction_earthwork_clamped: Option<usize>,
    pub maximum_repair: f32,
    pub maximum_raise: f32,
    pub maximum_lower: f32,
    pub maximum_control_grade: f64,
    pub unchanged_controls_checked: usize,
    pub junctions_checked: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moved_junctions: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal_recovery: Option<ProposalRecovery>,
    pub work: usize,
    pub control_grades_validated: bool,
    pub full_grid_earthwork_validated: bool,
    pub protected_samples_unchanged: bool,
    pub water_clearance_validated: bool,
    pub cubic_grades_validated: bool,
    pub runtime_access_validated: bool,
}
pub struct Output {
    pub height: Vec<i16>,
    pub roads: Vec<Path>,
    pub report: Report,
}
fn charge(
    work: &mut usize,
    limit: usize,
    n: usize,
    cancel: &impl Fn() -> bool,
) -> Result<(), Failure> {
    if cancel() {
        return Err(Failure::Cancelled);
    }
    *work = work
        .checked_add(n)
        .filter(|v| *v <= limit)
        .ok_or(Failure::Budget)?;
    Ok(())
}
fn topology(input: &Input<'_>, p: [f32; 3]) -> i32 {
    let n = input.surface_resolution;
    let [x, z] = [p[0], p[2]].map(|v| {
        (((v + input.grid.size * 0.5) * (1. / input.grid.size)) * n as f32)
            .clamp(0., (n - 1) as f32) as usize
    });
    input.topology[z * n + x]
}
fn support(grid: Grid<'_>, p: [f32; 3]) -> [usize; 4] {
    let n = grid.resolution;
    let [x, z] = [p[0], p[2]].map(|v| ((v + grid.size * 0.5) * (1. / grid.size)) * (n - 1) as f32);
    let a = (x as i32).clamp(0, (n - 1) as i32) as usize;
    let b = (z as i32).clamp(0, (n - 1) as i32) as usize;
    let c = usize::from(x < (n - 1) as f32);
    let d = if z < (n - 1) as f32 { n } else { 0 };
    let i = b * n + a;
    [i, i + c, i + d, i + c + d]
}
struct HeightAudit {
    changed: usize,
    repair: f32,
    raised: f32,
    lowered: f32,
}
fn audit_height(
    input: Input<'_>,
    height: &[i16],
    protected: &[u64],
    maximum_cut_fill: f32,
    maximum_work: usize,
    work: &mut usize,
    cancelled: &impl Fn() -> bool,
) -> Result<HeightAudit, Failure> {
    let n = input.grid.resolution;
    let quantum = path_terrain::SHORT_TO_FLOAT * input.grid.vertical_span;
    let mut audit = HeightAudit {
        changed: 0,
        repair: 0.,
        raised: 0.,
        lowered: 0.,
    };
    for i in 0..n * n {
        if i % 4096 == 0 {
            charge(work, maximum_work, (n * n - i).min(4096), cancelled)?;
        }
        if !(0..=32766).contains(&height[i]) {
            return Err(Failure::Invalid);
        }
        let earthwork = (i32::from(height[i]) - i32::from(input.ground[i])) as f32 * quantum;
        if earthwork.abs() > maximum_cut_fill {
            return Err(Failure::Infeasible("final earthwork"));
        }
        if protected[i / 64] & (1 << (i % 64)) != 0 && height[i] != input.grid.height[i] {
            return Err(Failure::Invalid);
        }
        audit.changed += usize::from(height[i] != input.grid.height[i]);
        audit.repair = audit.repair.max(
            (i32::from(height[i]) - i32::from(input.grid.height[i])).unsigned_abs() as f32
                * quantum,
        );
        audit.raised = audit.raised.max(earthwork);
        audit.lowered = audit.lowered.max(-earthwork);
    }
    Ok(audit)
}
/// Publish only after sampled controls and the full quantized grid pass their
/// bounds. Unchanged control supports stay fixed during sampled-grid repair.
pub fn correct(
    input: Input<'_>,
    maximum_grade: f32,
    maximum_cut_fill: f32,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Output, Failure> {
    correct_retaining_samples(
        input,
        &[],
        maximum_grade,
        maximum_cut_fill,
        maximum_work,
        cancelled,
    )
}
/// Pinned-junction fitting with immutable external height dependencies.
pub fn correct_retaining_samples(
    input: Input<'_>,
    retained_samples: &[usize],
    maximum_grade: f32,
    maximum_cut_fill: f32,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Output, Failure> {
    correct_attempt(
        input,
        retained_samples,
        maximum_grade,
        maximum_cut_fill,
        path_terrain::SHORT_TO_FLOAT * input.grid.vertical_span * 4.,
        true,
        false,
        maximum_work,
        cancelled,
    )
    .0
}
/// Joint initial terrain repair: derived network junctions may move together,
/// while authored endpoints and protected samples remain fixed.
pub fn correct_coupled_junctions(
    input: Input<'_>,
    maximum_grade: f32,
    maximum_cut_fill: f32,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Output, Failure> {
    correct_coupled_retaining_samples(
        input,
        &[],
        maximum_grade,
        maximum_cut_fill,
        maximum_work,
        cancelled,
    )
}
/// Initial coupled road fitting while retaining explicit height-grid samples.
/// The caller binds these indices to its retained geometry and input revision.
/// Every repair and final audit keeps their original encoded height exactly.
pub fn correct_coupled_retaining_samples(
    input: Input<'_>,
    retained_samples: &[usize],
    maximum_grade: f32,
    maximum_cut_fill: f32,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Output, Failure> {
    let reserve = path_terrain::SHORT_TO_FLOAT * input.grid.vertical_span * 4.;
    let mut spent = 0usize;
    // Retain as much rounding margin as the graph can support. Only an
    // infeasible proposal advances to the next encoding-derived reserve;
    // terrain, protection, convergence and final-grade failures remain errors.
    for (index, candidate) in [reserve, reserve * 0.75, reserve * 0.5, reserve * 0.25, 0.]
        .into_iter()
        .enumerate()
    {
        let remaining = if index == 0 {
            maximum_work
        } else {
            maximum_work
                .checked_sub(spent)
                .filter(|&v| v > 0)
                .ok_or(Failure::Budget)?
        };
        let previous = spent;
        let (result, used) = correct_attempt(
            input,
            retained_samples,
            maximum_grade,
            maximum_cut_fill,
            candidate,
            true,
            true,
            remaining,
            &cancelled,
        );
        spent = spent
            .checked_add(used)
            .filter(|&v| v <= maximum_work)
            .ok_or(Failure::Budget)?;
        match result {
            Ok(mut output) => {
                if index > 0 {
                    output.report.work = spent;
                    output.report.engine = RESERVE_RECOVERY_VERSION;
                    output.report.proposal_recovery = Some(ProposalRecovery {
                        attempts: index + 1,
                        initial_height_reserve: reserve,
                        accepted_height_reserve: candidate,
                        work_before_accepted_attempt: previous,
                    });
                }
                return Ok(output);
            }
            Err(Failure::Infeasible("control proposal") | Failure::ControlProposal(_))
                if index < 4 => {}
            Err(error) => return Err(error),
        }
    }
    unreachable!("The last proposal returns its result")
}
#[allow(clippy::too_many_arguments)]
fn correct_attempt(
    input: Input<'_>,
    retained_samples: &[usize],
    maximum_grade: f32,
    maximum_cut_fill: f32,
    height_reserve: f32,
    repair_controls: bool,
    coupled_junctions: bool,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> (Result<Output, Failure>, usize) {
    let mut work = 0;
    let result = (|| {
        let grid = input.grid;
        let n = grid.resolution;
        if !(2..=4097).contains(&n)
            || grid.height.len() != n * n
            || input.ground.len() != n * n
            || !(2..=2048).contains(&input.surface_resolution)
            || input.topology.len() != input.surface_resolution.pow(2)
            || !(1..=512_000_000).contains(&maximum_work)
            || (input.roads.is_empty()
                && (input.grid.height != input.ground || !input.junctions.is_empty()))
            || input.roads.len() > 4096
            || retained_samples.len() > 262144
            || retained_samples.iter().any(|&i| i >= n * n)
            || input
                .roads
                .iter()
                .map(|r| r.path.points().len())
                .sum::<usize>()
                > 131072
            || !maximum_grade.is_finite()
            || !(0.001..=1.).contains(&maximum_grade)
            || !maximum_cut_fill.is_finite()
            || !(0. ..=64.).contains(&maximum_cut_fill)
            || input.roads.iter().any(|r| {
                !r.radius.is_finite()
                    || !(0.1..=128.).contains(&r.radius)
                    || !r.shoulder.is_finite()
                    || !(0.1..=256.).contains(&r.shoulder)
            })
        {
            return Err(Failure::Invalid);
        }
        path_terrain::sample_height(grid, [0.; 3]).map_err(|_| Failure::Invalid)?;
        charge(&mut work, maximum_work, 0, &cancelled)?;
        let quantum = path_terrain::SHORT_TO_FLOAT * grid.vertical_span;
        let mut protected = vec![0_u64; (n * n).div_ceil(64)];
        for &i in retained_samples {
            charge(&mut work, maximum_work, 1, &cancelled)?;
            protected[i / 64] |= 1 << (i % 64);
        }
        let mut overruns = Vec::new();
        for i in 0..n * n {
            if i % 4096 == 0 {
                charge(&mut work, maximum_work, (n * n - i).min(4096), &cancelled)?;
            }
            if !(0..=32766).contains(&grid.height[i]) || !(0..=32766).contains(&input.ground[i]) {
                return Err(Failure::Invalid);
            }
            let earthwork =
                (i32::from(grid.height[i]) - i32::from(input.ground[i])) as f32 * quantum;
            let p = [
                (i % n) as f32 / (n - 1) as f32 * grid.size - grid.size * 0.5,
                0.,
                (i / n) as f32 / (n - 1) as f32 * grid.size - grid.size * 0.5,
            ];
            let retained = protected[i / 64] & (1 << (i % 64)) != 0;
            let flags = topology(&input, p);
            if flags & input.protected_topology != 0 {
                protected[i / 64] |= 1 << (i % 64);
            }
            if earthwork.abs() > maximum_cut_fill {
                if protected[i / 64] & (1 << (i % 64)) != 0 {
                    return Err(Failure::ProtectedSourceEarthwork {
                        sample: i,
                        world: [p[0], p[2]],
                        ground_units: input.ground[i],
                        proposed_units: grid.height[i],
                        topology: flags,
                        retained,
                    });
                }
                if overruns.len() >= 2_000_000 {
                    return Err(Failure::Budget);
                }
                overruns.push(i);
            }
        }
        let mut grounds = Vec::with_capacity(input.roads.len());
        let mut fixed = Vec::with_capacity(input.roads.len());
        for road in input.roads {
            let mut g = Vec::with_capacity(road.path.points().len());
            let mut f = Vec::with_capacity(g.capacity());
            for &p in road.path.points() {
                charge(&mut work, maximum_work, 1, &cancelled)?;
                if path_terrain::sample_height(grid, p)
                    .map_err(|_| Failure::Invalid)?
                    .to_bits()
                    != p[1].to_bits()
                {
                    return Err(Failure::Invalid);
                }
                g.push(
                    path_terrain::sample_height(
                        Grid {
                            height: input.ground,
                            ..grid
                        },
                        p,
                    )
                    .map_err(|_| Failure::Invalid)?,
                );
                // A partly fixed bilinear support cannot move independently of
                // the retained geometry. Pin this graph control conservatively;
                // the later quantized repair also retains the exact samples.
                f.push(
                    topology(&input, p) & input.protected_topology != 0
                        || (!retained_samples.is_empty()
                            && support(grid, p)
                                .iter()
                                .any(|&i| protected[i / 64] & (1 << (i % 64)) != 0)),
                );
            }
            grounds.push(g);
            fixed.push(f);
        }
        let roads: Vec<_> = input
            .roads
            .iter()
            .enumerate()
            .map(|(i, r)| road_grade::Road {
                path: r.path,
                ground: &grounds[i],
                fixed: &fixed[i],
            })
            .collect();
        let proposal_allowance = maximum_work
            .checked_sub(work)
            .filter(|v| *v > 0)
            .ok_or(Failure::Budget)?
            .min(32_000_000);
        // The graph API does not return consumed work on failure. Reserve its
        // entire allowance so a failed retry cannot silently reset that work.
        // Successful calls refund the reservation and charge their exact report.
        if coupled_junctions {
            charge(&mut work, maximum_work, proposal_allowance, &cancelled)?;
        }
        let (proposal, conflict) = road_grade::propose_diagnosed(
            &roads,
            input.junctions,
            maximum_grade,
            maximum_cut_fill,
            // Leave room for endpoint quantization and the tapered field's
            // interpolation across the four sampled height vertices.
            height_reserve,
            if coupled_junctions {
                road_grade::JunctionMode::CoupledNetwork
            } else {
                road_grade::JunctionMode::Pinned
            },
            proposal_allowance,
            &cancelled,
        );
        let proposal = proposal.map_err(|e| match e {
            crate::rail_grade_fit::Failure::Invalid => Failure::Invalid,
            crate::rail_grade_fit::Failure::Budget => Failure::Budget,
            crate::rail_grade_fit::Failure::Cancelled => Failure::Cancelled,
            crate::rail_grade_fit::Failure::Infeasible => conflict.map_or(
                Failure::Infeasible("control proposal"),
                Failure::ControlProposal,
            ),
        })?;
        if coupled_junctions {
            work -= proposal_allowance;
        }
        charge(&mut work, maximum_work, proposal.report.work, &cancelled)?;
        let mut unchanged = 0;
        for (road, target) in input.roads.iter().zip(&proposal.roads) {
            for (&p, q) in road.path.points().iter().zip(target.points()) {
                charge(&mut work, maximum_work, 1, &cancelled)?;
                if p[1].to_bits() == q[1].to_bits() {
                    unchanged += 1;
                    for i in support(grid, p) {
                        protected[i / 64] |= 1 << (i % 64);
                    }
                }
            }
        }
        // At most two million affected vertices; no dense float correction grid.
        let mut corrections = BTreeMap::<usize, (f64, f64)>::new();
        let cell = f64::from(grid.size) / (n - 1) as f64;
        let half = f64::from(grid.size) * 0.5;
        for (road, target) in input.roads.iter().zip(&proposal.roads) {
            let core = f64::from(road.radius).max(cell * 2_f64.sqrt());
            let outer = core + f64::from(road.shoulder);
            for (s, pair) in road.path.points().windows(2).enumerate() {
                let da = f64::from(target.points()[s][1]) - f64::from(pair[0][1]);
                let db = f64::from(target.points()[s + 1][1]) - f64::from(pair[1][1]);
                if da == 0. && db == 0. {
                    continue;
                }
                let [a, b] = [pair[0], pair[1]].map(|p| [f64::from(p[0]), f64::from(p[2])]);
                let v = [b[0] - a[0], b[1] - a[1]];
                let length = v[0] * v[0] + v[1] * v[1];
                if length < 1e-8 {
                    continue;
                }
                let bounds = |axis: usize| {
                    (
                        (((a[axis].min(b[axis]) - outer + half) / cell).floor() as i64)
                            .clamp(0, (n - 1) as i64) as usize,
                        (((a[axis].max(b[axis]) + outer + half) / cell).ceil() as i64)
                            .clamp(0, (n - 1) as i64) as usize,
                    )
                };
                let (minx, maxx) = bounds(0);
                let (minz, maxz) = bounds(1);
                for z in minz..=maxz {
                    for x in minx..=maxx {
                        charge(&mut work, maximum_work, 1, &cancelled)?;
                        let i = z * n + x;
                        if protected[i / 64] & (1 << (i % 64)) != 0 {
                            continue;
                        }
                        let p = [x as f64 * cell - half, z as f64 * cell - half];
                        let t =
                            (((p[0] - a[0]) * v[0] + (p[1] - a[1]) * v[1]) / length).clamp(0., 1.);
                        let distance = (p[0] - a[0] - v[0] * t).hypot(p[1] - a[1] - v[1] * t);
                        if distance >= outer {
                            continue;
                        }
                        let fade = ((outer - distance) / (outer - core)).clamp(0., 1.);
                        let delta = (da + (db - da) * t) * fade * fade * (3. - 2. * fade);
                        if delta == 0. {
                            continue;
                        }
                        if !corrections.contains_key(&i) && corrections.len() >= 2_000_000 {
                            return Err(Failure::Budget);
                        }
                        let entry = corrections.entry(i).or_insert((distance, delta));
                        if distance < entry.0 {
                            *entry = (distance, delta);
                        }
                    }
                }
            }
        }
        charge(&mut work, maximum_work, n * n, &cancelled)?;
        let mut height = grid.height.to_vec();
        let allowance = (f64::from(maximum_cut_fill) / f64::from(quantum)).floor() as i32;
        let mut correction_clamped = 0usize;
        for (i, (_, delta)) in corrections {
            charge(&mut work, maximum_work, 1, &cancelled)?;
            let value = (f64::from(grid.height[i]) + delta / f64::from(quantum)).round();
            // A control's interval does not bound its off-centre tapered field.
            // Only this road-supported correction may be projected into the
            // original-ground interval. Protected/fixed samples were excluded
            // above, and actual sampled grades are audited/repaired below.
            let value = if coupled_junctions {
                let base = i32::from(input.ground[i]);
                let bounded = value.clamp(
                    f64::from((base - allowance).max(0)),
                    f64::from((base + allowance).min(32766)),
                );
                correction_clamped += usize::from(value != bounded);
                bounded
            } else {
                value
            };
            if !(0. ..=32766.).contains(&value) {
                return Err(Failure::Infeasible("height encoding"));
            }
            height[i] = value as i16;
        }
        let mut clamped = 0;
        for &i in &overruns {
            charge(&mut work, maximum_work, 1, &cancelled)?;
            if protected[i / 64] & (1 << (i % 64)) != 0 {
                return Err(Failure::Infeasible("fixed source earthwork"));
            }
            let p = [(i % n) as f64 * cell - half, (i / n) as f64 * cell - half];
            let mut supported = false;
            'roads: for road in input.roads {
                let outer =
                    f64::from(road.radius).max(cell * 2_f64.sqrt()) + f64::from(road.shoulder);
                for pair in road.path.points().windows(2) {
                    charge(&mut work, maximum_work, 1, &cancelled)?;
                    let a = [f64::from(pair[0][0]), f64::from(pair[0][2])];
                    let v = [f64::from(pair[1][0]) - a[0], f64::from(pair[1][2]) - a[1]];
                    let length = v[0] * v[0] + v[1] * v[1];
                    let t = if length > 0. {
                        (((p[0] - a[0]) * v[0] + (p[1] - a[1]) * v[1]) / length).clamp(0., 1.)
                    } else {
                        0.
                    };
                    if (p[0] - a[0] - v[0] * t).hypot(p[1] - a[1] - v[1] * t) <= outer {
                        supported = true;
                        break 'roads;
                    }
                }
            }
            if !supported {
                return Err(Failure::Infeasible(
                    "source earthwork outside road corridor",
                ));
            }
            let base = i32::from(input.ground[i]);
            let value = i32::from(height[i])
                .clamp((base - allowance).max(0), (base + allowance).min(32766))
                as i16;
            clamped += usize::from(value != height[i]);
            height[i] = value;
        }
        let mut audit = audit_height(
            input,
            &height,
            &protected,
            maximum_cut_fill,
            maximum_work,
            &mut work,
            &cancelled,
        )?;
        let mut fitted = Vec::with_capacity(input.roads.len());
        let mut grade = 0_f64;
        for (road, target) in input.roads.iter().zip(&proposal.roads) {
            let mut heights = Vec::with_capacity(road.path.points().len());
            for (&p, q) in road.path.points().iter().zip(target.points()) {
                charge(&mut work, maximum_work, 1, &cancelled)?;
                let h = path_terrain::sample_height(
                    Grid {
                        height: &height,
                        ..grid
                    },
                    p,
                )
                .map_err(|_| Failure::Invalid)?;
                if p[1].to_bits() == q[1].to_bits() && p[1].to_bits() != h.to_bits() {
                    return Err(Failure::Infeasible("protected control"));
                }
                heights.push(h);
            }
            for (i, pair) in road.path.points().windows(2).enumerate() {
                let distance = (f64::from(pair[1][0]) - f64::from(pair[0][0]))
                    .hypot(f64::from(pair[1][2]) - f64::from(pair[0][2]));
                let difference = (f64::from(heights[i + 1]) - f64::from(heights[i])).abs();
                if distance < 1e-4 {
                    if difference != 0. {
                        return Err(Failure::Infeasible("coincident controls"));
                    }
                } else {
                    grade = grade.max(difference / distance);
                }
            }
            let path = road
                .path
                .refit_heights(
                    &heights,
                    0,
                    &vec![1.; heights.len()],
                    maximum_work
                        .checked_sub(work)
                        .filter(|v| *v > 0)
                        .ok_or(Failure::Budget)?
                        .min(32_000_000),
                    &cancelled,
                )
                .map_err(|e| match e {
                    crate::path_geometry::Failure::Invalid => Failure::Invalid,
                    crate::path_geometry::Failure::Budget => Failure::Budget,
                })?
                .ok_or(Failure::Cancelled)?;
            charge(&mut work, maximum_work, path.work(), &cancelled)?;
            fitted.push(path);
        }
        let mut control_fit = None;
        if grade > f64::from(maximum_grade) {
            if !repair_controls {
                return Err(Failure::ControlGrade(grade));
            }
            charge(&mut work, maximum_work, retained_samples.len(), &cancelled)?;
            let mut fixed_samples = retained_samples.to_vec();
            for (road, target) in input.roads.iter().zip(&proposal.roads) {
                for (&p, q) in road.path.points().iter().zip(target.points()) {
                    charge(&mut work, maximum_work, 8, &cancelled)?;
                    if p[1].to_bits() == q[1].to_bits() {
                        fixed_samples.extend(support(grid, p));
                    }
                }
            }
            let controls = fitted
                .iter()
                .map(|p| p.points().to_vec())
                .collect::<Vec<_>>();
            let result = road_bed::fit_control_samples(
                road_bed::Input {
                    grid: Grid {
                        height: &height,
                        ..grid
                    },
                    ground: input.ground,
                    topology: input.topology,
                    surface_resolution: input.surface_resolution,
                    protected_topology: input.protected_topology,
                    controls: &controls,
                    junctions: input.junctions,
                    collisions: &[],
                },
                road_bed::Settings {
                    maximum_grade,
                    maximum_cut_fill,
                    minimum_up_normal: 0.5,
                    maximum_iterations: 512,
                    maximum_work: maximum_work
                        .checked_sub(work)
                        .filter(|&v| v > 0)
                        .ok_or(Failure::Budget)?,
                    quantization_reserve: 0.51,
                    reserve_roundoff: true,
                },
                &fixed_samples,
                &cancelled,
            )
            .map_err(|e| match e {
                road_bed::Failure::Cancelled => Failure::Cancelled,
                road_bed::Failure::Budget => Failure::Budget,
                other => Failure::ControlFit(other),
            })?;
            charge(&mut work, maximum_work, result.report.work, &cancelled)?;
            height = result.height;
            for (path, controls) in fitted.iter_mut().zip(result.controls) {
                let ys = controls.iter().map(|p| p[1]).collect::<Vec<_>>();
                let next = path
                    .refit_heights(
                        &ys,
                        0,
                        &vec![1.; ys.len()],
                        maximum_work
                            .checked_sub(work)
                            .filter(|&v| v > 0)
                            .ok_or(Failure::Budget)?
                            .min(32_000_000),
                        &cancelled,
                    )
                    .map_err(|e| match e {
                        crate::path_geometry::Failure::Invalid => Failure::Invalid,
                        crate::path_geometry::Failure::Budget => Failure::Budget,
                    })?
                    .ok_or(Failure::Cancelled)?;
                charge(&mut work, maximum_work, next.work(), &cancelled)?;
                *path = next;
            }
            grade = 0.;
            for path in &fitted {
                for pair in path.points().windows(2) {
                    charge(&mut work, maximum_work, 2, &cancelled)?;
                    let run = (f64::from(pair[1][0]) - f64::from(pair[0][0]))
                        .hypot(f64::from(pair[1][2]) - f64::from(pair[0][2]));
                    grade = grade.max((f64::from(pair[1][1]) - f64::from(pair[0][1])).abs() / run);
                }
            }
            if !grade.is_finite() || grade > f64::from(maximum_grade) {
                return Err(Failure::ControlGrade(grade));
            }
            audit = audit_height(
                input,
                &height,
                &protected,
                maximum_cut_fill,
                maximum_work,
                &mut work,
                &cancelled,
            )?;
            control_fit = Some(result.report);
        }
        let mut moved_junctions = 0;
        for j in input.junctions {
            charge(&mut work, maximum_work, 1, &cancelled)?;
            for (r, p) in [j.a, j.b] {
                let changed = fitted[r].points()[p] != input.roads[r].path.points()[p];
                if !coupled_junctions && changed {
                    return Err(Failure::Infeasible("network junction"));
                }
                if coupled_junctions
                    && [0, 2].iter().any(|&i| {
                        fitted[r].points()[p][i].to_bits()
                            != input.roads[r].path.points()[p][i].to_bits()
                    })
                {
                    return Err(Failure::Infeasible("network junction coordinates"));
                }
            }
            if fitted[j.a.0].points()[j.a.1] != fitted[j.b.0].points()[j.b.1] {
                return Err(Failure::Infeasible("network junction equality"));
            }
            moved_junctions += usize::from(
                fitted[j.a.0].points()[j.a.1] != input.roads[j.a.0].path.points()[j.a.1],
            );
        }
        Ok(Output {
            height,
            roads: fitted,
            report: Report {
                engine: if coupled_junctions && correction_clamped > 0 {
                    RESERVE_RECOVERY_VERSION
                } else if coupled_junctions {
                    COUPLED_VERSION
                } else {
                    VERSION
                },
                moved_junctions: coupled_junctions.then_some(moved_junctions),
                proposal_recovery: None,
                retained_sample_entries: (!retained_samples.is_empty())
                    .then_some(retained_samples.len()),
                control_grade_attempts: if control_fit.is_some() { 2 } else { 1 },
                sampled_control_fit: control_fit,
                proposal: proposal.report,
                changed_samples: audit.changed,
                source_earthwork_overruns: overruns.len(),
                source_earthwork_clamped: clamped,
                correction_earthwork_clamped: (coupled_junctions && correction_clamped > 0)
                    .then_some(correction_clamped),
                maximum_repair: audit.repair,
                maximum_raise: audit.raised,
                maximum_lower: audit.lowered,
                maximum_control_grade: grade,
                unchanged_controls_checked: unchanged,
                junctions_checked: input.junctions.len(),
                work,
                control_grades_validated: true,
                full_grid_earthwork_validated: true,
                protected_samples_unchanged: true,
                water_clearance_validated: false,
                cubic_grades_validated: false,
                runtime_access_validated: false,
            },
        })
    })();
    (result, work)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coupled_field_bounds_off_centre_corrections_before_exact_grade_audit() {
        let mut height = vec![crate::erosion::encode(10.); 65 * 65];
        for z in 0..65 {
            height[z * 65 + 32] = crate::erosion::encode(13.);
            height[z * 65 + 37] = crate::erosion::encode(13.9);
        }
        let mut ground = height.clone();
        let off_centre = 31 * 65 + 32;
        ground[off_centre] = crate::erosion::encode(20.5);
        let points = [-10., 0., 10.].map(|x| {
            let mut p = [x, 0., 0.];
            p[1] = path_terrain::sample_height(grid(&height), p).unwrap();
            p
        });
        let path = Path::from_controls(&points, 10000, || false)
            .unwrap()
            .unwrap();
        let roads = [Road {
            path: &path,
            radius: 4.,
            shoulder: 8.,
        }];
        let topology = vec![0; 32 * 32];
        let input = Input {
            grid: grid(&height),
            ground: &ground,
            topology: &topology,
            surface_resolution: 32,
            protected_topology: 1024,
            roads: &roads,
            junctions: &[],
        };
        let (unbounded, _) = correct_attempt(
            input,
            &[],
            0.2,
            8.,
            path_terrain::SHORT_TO_FLOAT * 1000.,
            true,
            false,
            200_000_000,
            || false,
        );
        assert!(matches!(
            unbounded,
            Err(Failure::Infeasible("final earthwork"))
        ));
        let out = correct_coupled_junctions(input, 0.2, 8., 200_000_000, || false).unwrap();
        assert!(out.report.correction_earthwork_clamped.unwrap() > 0);
        assert_eq!(out.roads[0].points().first(), path.points().first());
        assert_eq!(out.roads[0].points().last(), path.points().last());
        assert!(out.report.maximum_control_grade <= f64::from(0.2_f32));
        assert!(out.height.iter().zip(&ground).all(|(&a, &b)| {
            (i32::from(a) - i32::from(b)).unsigned_abs() as f32
                * path_terrain::SHORT_TO_FLOAT
                * 1000.
                <= 8.
        }));
        assert_eq!(height[off_centre], crate::erosion::encode(13.));
        assert_eq!(ground[off_centre], crate::erosion::encode(20.5));
    }
    #[test]
    fn coupled_proposal_reserve_recovery_keeps_actual_limits_and_shared_work() {
        let mut height = vec![crate::erosion::encode(10.); 65 * 65];
        for z in 0..65 {
            height[z * 65 + 32] = crate::erosion::encode(13.);
            height[z * 65 + 37] = crate::erosion::encode(13.9);
        }
        let original = height.clone();
        let points = [-10., 0., 10.].map(|x| {
            let mut p = [x, 0., 0.];
            p[1] = path_terrain::sample_height(grid(&height), p).unwrap();
            p
        });
        let path = Path::from_controls(&points, 10000, || false)
            .unwrap()
            .unwrap();
        let roads = [Road {
            path: &path,
            radius: 4.,
            shoulder: 8.,
        }];
        let topology = vec![0; 32 * 32];
        let input = Input {
            grid: grid(&height),
            ground: &height,
            topology: &topology,
            surface_resolution: 32,
            protected_topology: 1024,
            roads: &roads,
            junctions: &[],
        };
        assert!(matches!(
            correct(input, 0.2, 8., 200_000_000, || false),
            Err(Failure::ControlProposal(_))
        ));
        let out = correct_coupled_junctions(input, 0.2, 8., 200_000_000, || false).unwrap();
        assert_eq!(out.report.engine, RESERVE_RECOVERY_VERSION);
        let recovery = out.report.proposal_recovery.as_ref().unwrap();
        assert_eq!(recovery.attempts, 4);
        assert!(recovery.accepted_height_reserve > 0.);
        assert!(recovery.accepted_height_reserve < recovery.initial_height_reserve);
        assert!(recovery.work_before_accepted_attempt >= 32_000_000);
        assert!(out.report.work > recovery.work_before_accepted_attempt);
        assert!(out.report.maximum_control_grade <= f64::from(0.2_f32));
        assert!(out.report.full_grid_earthwork_validated && out.report.protected_samples_unchanged);
        assert_ne!(out.height, height);
        assert_eq!(out.roads[0].points().first(), path.points().first());
        assert_eq!(out.roads[0].points().last(), path.points().last());
        for pair in out.roads[0].points().windows(2) {
            let ys = [pair[0], pair[1]].map(|p| {
                path_terrain::sample_height(
                    Grid {
                        height: &out.height,
                        ..input.grid
                    },
                    p,
                )
                .unwrap()
            });
            assert_eq!(ys, [pair[0][1], pair[1][1]]);
            let run = f64::from(pair[1][0] - pair[0][0]).hypot(f64::from(pair[1][2] - pair[0][2]));
            assert!(f64::from((ys[1] - ys[0]).abs()) / run <= f64::from(0.2_f32));
        }
        assert!(out.height.iter().zip(&height).all(|(&a, &b)| {
            (i32::from(a) - i32::from(b)).unsigned_abs() as f32
                * path_terrain::SHORT_TO_FLOAT
                * 1000.
                <= 8.
        }));
        assert!(matches!(
            correct_coupled_junctions(input, 0.2, 8., out.report.work - 1, || false),
            Err(Failure::Budget)
        ));
        assert!(matches!(
            correct_coupled_junctions(input, 0.2, 8., 200_000_000, || true),
            Err(Failure::Cancelled)
        ));
        // Exact authored endpoints alone exceed the grade limit: retry cannot
        // make them movable or approve an impossible quantized road.
        let denied_points = [points[0], points[2]];
        let denied_path = Path::from_controls(&denied_points, 10000, || false)
            .unwrap()
            .unwrap();
        let denied_roads = [Road {
            path: &denied_path,
            radius: 4.,
            shoulder: 8.,
        }];
        assert!(matches!(
            correct_coupled_junctions(
                Input {
                    roads: &denied_roads,
                    ..input
                },
                0.1,
                8.,
                200_000_000,
                || false
            ),
            Err(Failure::ControlProposal(_))
        ));
        assert_eq!(height, original);
    }
    #[test]
    fn quantized_control_grades_are_repaired_without_relaxing_limits_or_resetting_work_budget() {
        let ground = vec![crate::erosion::encode(10.); 65 * 65];
        let topology = vec![0; 32 * 32];
        let amplitude = 7.2;
        let offset = 0.85;
        let height = (0..65 * 65)
            .map(|i| {
                let x = (i % 65) as f32 * 2. - 64.;
                let z = (i / 65) as f32 * 2. - 64.;
                crate::erosion::encode(
                    10. + (1. - x.abs() / 20.).max(0.) * amplitude + (z * 0.2).sin() * 0.6,
                )
            })
            .collect::<Vec<_>>();
        let zs = [0., 4., -4., 6., -6., 4., -4., 4., 0.];
        let points = (0..9)
            .map(|i| {
                let mut p = [-40. + i as f32 * 10. + offset, 0., zs[i] + offset];
                p[1] = path_terrain::sample_height(grid(&height), p).unwrap();
                p
            })
            .collect::<Vec<_>>();
        let path = Path::from_controls(&points, 10000, || false)
            .unwrap()
            .unwrap();
        let roads = [Road {
            path: &path,
            radius: 4.,
            shoulder: 8.,
        }];
        let input = Input {
            grid: grid(&height),
            ground: &ground,
            topology: &topology,
            surface_resolution: 32,
            protected_topology: 1024,
            roads: &roads,
            junctions: &[],
        };
        let original = height.clone();
        let (first, first_work) = correct_attempt(
            input,
            &[],
            0.2,
            8.,
            path_terrain::SHORT_TO_FLOAT * 1000. * 4.,
            false,
            false,
            2_000_000,
            || false,
        );
        assert!(matches!(first,Err(Failure::ControlGrade(g)) if g > 0.2));
        let out = correct(input, 0.2, 8., 2_000_000, || false).unwrap();
        assert_eq!(out.report.control_grade_attempts, 2);
        let fit = out.report.sampled_control_fit.as_ref().unwrap();
        assert_eq!(fit.engine, road_bed::CONTROLS_VERSION);
        assert!(!fit.exact_quantized_mesh_grades_validated);
        assert!(!fit.runtime_access_validated && !fit.water_clearance_validated);
        assert!(out.report.work > first_work);
        assert!(out.report.maximum_control_grade <= f64::from(0.2f32));
        assert_eq!(out.roads[0].points().first(), path.points().first());
        assert_eq!(out.roads[0].points().last(), path.points().last());
        let final_grid = Grid {
            height: &out.height,
            ..input.grid
        };
        for pair in out.roads[0].points().windows(2) {
            let heights =
                [pair[0], pair[1]].map(|p| path_terrain::sample_height(final_grid, p).unwrap());
            assert_eq!(heights, [pair[0][1], pair[1][1]]);
            let run = f64::from(pair[1][0] - pair[0][0]).hypot(f64::from(pair[1][2] - pair[0][2]));
            assert!(f64::from((heights[1] - heights[0]).abs()) / run <= f64::from(0.2f32));
        }
        assert!(out.height.iter().zip(&ground).all(|(&a, &b)| {
            (i32::from(a) - i32::from(b)).unsigned_abs() as f32
                * path_terrain::SHORT_TO_FLOAT
                * 1000.
                <= 8.
        }));
        assert!(matches!(
            correct(input, 0.2, 8., out.report.work - 1, || false),
            Err(Failure::Budget)
        ));
        assert_eq!(height, original);
    }
    #[test]
    fn empty_network_preserves_valid_ground_and_rejects_unexplained_height_writes() {
        let ground = vec![10000; 65 * 65];
        let topology = vec![0; 32 * 32];
        let input = |height| Input {
            grid: grid(height),
            ground: &ground,
            topology: &topology,
            surface_resolution: 32,
            protected_topology: 1024,
            roads: &[],
            junctions: &[],
        };
        let output = correct(input(&ground), 0.2, 8., 100000, || false).unwrap();
        assert_eq!(output.height, ground);
        assert!(output.roads.is_empty());
        assert_eq!(output.report.changed_samples, 0);
        assert_eq!(output.report.proposal.controls, 0);
        assert!(output.report.full_grid_earthwork_validated);
        assert!(!output.report.runtime_access_validated);
        let mut changed = ground.clone();
        changed[100] += 1;
        assert!(matches!(
            correct(input(&changed), 0.2, 8., 100000, || false),
            Err(Failure::Invalid)
        ));
        let mut invalid = ground.clone();
        invalid[100] = -1;
        let bad = Input {
            ground: &invalid,
            ..input(&invalid)
        };
        assert!(matches!(
            correct(bad, 0.2, 8., 100000, || false),
            Err(Failure::Invalid)
        ));
        assert!(matches!(
            correct(input(&ground), 0.2, 8., 100000, || true),
            Err(Failure::Cancelled)
        ));
        assert!(matches!(
            correct(input(&ground), 0.2, 8., 100, || false),
            Err(Failure::Budget)
        ));
    }
    fn grid(height: &[i16]) -> Grid<'_> {
        Grid {
            size: 128.,
            resolution: 65,
            origin_y: -500.,
            vertical_span: 1000.,
            height,
        }
    }
    fn scene() -> (Vec<i16>, Vec<i16>, Path) {
        let ground = vec![crate::erosion::encode(10.); 65 * 65];
        let mut height = ground.clone();
        for z in 0..65 {
            for x in 0..65 {
                let world = x as f32 * 2. - 64.;
                height[z * 65 + x] =
                    crate::erosion::encode(10. + (1. - world.abs() / 20.).max(0.) * 5.);
            }
        }
        let points = [-40., -20., 0., 20., 40.].map(|x| {
            let mut p = [x, 0., 0.];
            p[1] = path_terrain::sample_height(grid(&height), p).unwrap();
            p
        });
        let path = Path::from_controls(&points, 10000, || false)
            .unwrap()
            .unwrap();
        (ground, height, path)
    }
    #[test]
    fn explicit_retained_samples_survive_road_shaping_and_keep_legacy_results() {
        let (ground, height, path) = scene();
        let original = height.clone();
        let topology = vec![0; 32 * 32];
        let roads = [Road {
            path: &path,
            radius: 4.,
            shoulder: 8.,
        }];
        let input = Input {
            grid: grid(&height),
            ground: &ground,
            topology: &topology,
            surface_resolution: 32,
            protected_topology: 1024,
            roads: &roads,
            junctions: &[],
        };
        let baseline = correct_coupled_junctions(input, 0.2, 8., 200_000_000, || false).unwrap();
        let legacy =
            correct_coupled_retaining_samples(input, &[], 0.2, 8., 200_000_000, || false).unwrap();
        assert_eq!(
            serde_json::to_vec(&baseline.report).unwrap(),
            serde_json::to_vec(&legacy.report).unwrap()
        );
        assert_eq!(baseline.height, legacy.height);
        let retained = 35 * 65 + 32;
        assert_ne!(baseline.height[retained], height[retained]);
        let output =
            correct_coupled_retaining_samples(input, &[retained], 0.2, 8., 200_000_000, || false)
                .unwrap();
        assert_eq!(output.height[retained], height[retained]);
        assert!(output.report.changed_samples > 0);
        assert!(output.report.maximum_control_grade <= f64::from(0.2_f32));
        assert!(output.report.protected_samples_unchanged);
        assert_eq!(height, original);
        let frozen = path
            .points()
            .iter()
            .flat_map(|&p| support(input.grid, p))
            .collect::<Vec<_>>();
        assert!(matches!(
            correct_coupled_retaining_samples(input, &frozen, 0.2, 8., 200_000_000, || false),
            Err(Failure::ControlProposal(_))
        ));
        assert!(matches!(
            correct_coupled_retaining_samples(input, &[height.len()], 0.2, 8., 200_000_000, || {
                false
            }),
            Err(Failure::Invalid)
        ));
        assert!(matches!(
            correct_coupled_retaining_samples(input, &[retained], 0.2, 8., 1, || false),
            Err(Failure::Budget)
        ));
        assert!(matches!(
            correct_coupled_retaining_samples(input, &[retained], 0.2, 8., 200_000_000, || true),
            Err(Failure::Cancelled)
        ));
    }
    #[test]
    fn repairs_quantized_terrain_and_preserves_fixed_support_and_unrelated_samples() {
        let (ground, height, path) = scene();
        let mut topology = vec![0; 32 * 32];
        topology[18 * 32 + 16] = 1024;
        let roads = [Road {
            path: &path,
            radius: 4.,
            shoulder: 8.,
        }];
        let input = || Input {
            grid: grid(&height),
            ground: &ground,
            topology: &topology,
            surface_resolution: 32,
            protected_topology: 1024,
            roads: &roads,
            junctions: &[],
        };
        let output = correct(input(), 0.2, 8., 1_000_000, || false).unwrap();
        assert!(output.report.sampled_control_fit.is_none());
        assert!(output.report.changed_samples > 0);
        assert!(output.report.maximum_control_grade <= 0.2);
        assert!(output.report.maximum_raise <= 8. && output.report.maximum_lower <= 8.);
        assert_eq!(output.roads[0].points()[0], path.points()[0]);
        assert_eq!(output.roads[0].points()[4], path.points()[4]);
        for (i, &h) in output.height.iter().enumerate() {
            let z = (i / 65) as f32 * 2. - 64.;
            if z.abs() > 12. {
                assert_eq!(h, height[i]);
            }
            let p = [(i % 65) as f32 * 2. - 64., 0., z];
            if super::topology(&input(), p) & 1024 != 0 {
                assert_eq!(h, height[i]);
            }
        }
        assert_eq!(
            correct(input(), 0.2, 8., 1_000_000, || false)
                .unwrap()
                .height,
            output.height
        );
        let final_grid = Grid {
            height: &output.height,
            ..grid(&height)
        };
        for &p in output.roads[0].points() {
            assert_eq!(path_terrain::sample_height(final_grid, p).unwrap(), p[1]);
        }
        assert!(
            !output.report.water_clearance_validated && !output.report.runtime_access_validated
        );
        assert!(matches!(
            correct(input(), 0.2, 8., output.report.work - 1, || false),
            Err(Failure::Budget)
        ));
    }
    #[test]
    fn infeasible_protection_limits_and_cancellation_never_mutate_inputs() {
        let (ground, height, path) = scene();
        let original = height.clone();
        let protected = vec![1024; 32 * 32];
        let roads = [Road {
            path: &path,
            radius: 4.,
            shoulder: 8.,
        }];
        let input = || Input {
            grid: grid(&height),
            ground: &ground,
            topology: &protected,
            surface_resolution: 32,
            protected_topology: 1024,
            roads: &roads,
            junctions: &[],
        };
        assert!(matches!(
            correct(input(), 0.2, 8., 1_000_000, || false),
            Err(Failure::ControlProposal(_))
        ));
        assert!(matches!(
            correct(input(), 0.2, 8., 1, || false),
            Err(Failure::Budget)
        ));
        assert!(matches!(
            correct(input(), 0.2, 8., 1_000_000, || true),
            Err(Failure::Cancelled)
        ));
        let error = correct(input(), 0.2, 1., 1_000_000, || false)
            .err()
            .unwrap();
        let Failure::ProtectedSourceEarthwork {
            sample,
            ground_units,
            proposed_units,
            topology,
            retained,
            ..
        } = error
        else {
            panic!("Expected a protected earthwork witness: {error:?}");
        };
        assert_eq!(ground_units, ground[sample]);
        assert_eq!(proposed_units, height[sample]);
        assert_eq!(topology, 1024);
        assert!(!retained);
        assert!(matches!(
            correct_retaining_samples(input(), &[sample], 0.2, 1., 1_000_000, || false),
            Err(Failure::ProtectedSourceEarthwork { retained: true, .. })
        ));
        assert_eq!(height, original);
        let calls = std::cell::Cell::new(0);
        assert!(matches!(
            correct(input(), 0.2, 8., 1_000_000, || {
                calls.set(calls.get() + 1);
                calls.get() > 20
            }),
            Err(Failure::Cancelled)
        ));
    }
    #[test]
    fn source_overruns_are_corrected_only_in_unprotected_road_corridors() {
        let (ground, mut height, path) = scene();
        height[37 * 65 + 32] = crate::erosion::encode(18.25);
        let topology = vec![0; 32 * 32];
        let roads = [Road {
            path: &path,
            radius: 4.,
            shoulder: 8.,
        }];
        let run = |height: &[i16]| {
            correct(
                Input {
                    grid: grid(height),
                    ground: &ground,
                    topology: &topology,
                    surface_resolution: 32,
                    protected_topology: 1024,
                    roads: &roads,
                    junctions: &[],
                },
                0.2,
                8.,
                1_000_000,
                || false,
            )
        };
        let corrected = run(&height).unwrap();
        assert_eq!(corrected.report.source_earthwork_overruns, 1);
        assert_eq!(corrected.report.source_earthwork_clamped, 1);
        assert!(corrected.report.maximum_raise <= 8.);
        assert_eq!(height[37 * 65 + 32], crate::erosion::encode(18.25));
        height[64 * 65 + 32] = crate::erosion::encode(18.25);
        assert!(matches!(
            run(&height),
            Err(Failure::Infeasible(
                "source earthwork outside road corridor"
            ))
        ));
    }
}
