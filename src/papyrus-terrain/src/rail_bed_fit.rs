//! Connected height fitting used only by explicit constrained-bed requests.
use crate::{
    native_layers::Layers,
    rail_bed::{FittedTrack, Policy, Shape, Track},
    rail_grade,
    rail_grade_fit::{self, Edge, Node, Settings},
};
use serde::Serialize;
mod support;
use support::Support;
pub const VERSION: &str = "papyrus-connected-rail-bed-fit-3";
/// Control-height allowance after bed offset and encoded-height headroom.
pub fn control_allowance(maximum_cut_fill: f32, terrain_offset: f32) -> f64 {
    f64::from((maximum_cut_fill - terrain_offset.abs() - 0.031).max(0.))
}
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub struct Join {
    pub a_track: u64,
    pub a_control: usize,
    pub b_track: u64,
    pub b_control: usize,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub attempts: u8,
    pub effective_edge_grade: f64,
    pub work: usize,
    pub nodes: usize,
    pub edges: usize,
    pub maximum_control_change: f64,
    pub continuous_grades: Vec<rail_grade::Audit>,
}
fn root(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply(
    base: &Layers,
    alpha: &[u8],
    water: Option<&[f32]>,
    original: &[Track<'_>],
    fitted: &mut [FittedTrack],
    joins: &[Join],
    policy: &Policy,
    shape: Shape,
    maximum_cut_fill: f32,
    settings: Settings,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    settings.validate()?;
    if cancelled() {
        return Ok(None);
    }
    let mut offsets = vec![0];
    for t in fitted.iter() {
        offsets.push(offsets.last().unwrap() + t.path.points().len());
    }
    let count = *offsets.last().unwrap();
    if count > 131072 || original.len() != fitted.len() || joins.len() > 512 {
        return Err("Invalid connected bed graph size".into());
    }
    let mut parent: Vec<usize> = (0..count).collect();
    let mut connected: Vec<usize> = (0..fitted.len()).collect();
    let mut merge = |a, b| {
        let a = root(&mut parent, a);
        let b = root(&mut parent, b);
        parent[a.max(b)] = a.min(b);
    };
    for (i, t) in fitted.iter().enumerate() {
        if t.path.is_circular() {
            merge(offsets[i], offsets[i + 1] - 1);
        }
    }
    let lookup = |id, control| -> Result<(usize, [f32; 3]), String> {
        let i = fitted
            .iter()
            .position(|t| t.id == id)
            .ok_or("Unknown rail join path")?;
        let point = *fitted[i]
            .path
            .points()
            .get(control)
            .ok_or("Unknown rail join control")?;
        Ok((offsets[i] + control, point))
    };
    for join in joins {
        let (a, pa) = lookup(join.a_track, join.a_control)?;
        let (b, pb) = lookup(join.b_track, join.b_control)?;
        if (pa[0] - pb[0]).hypot(pa[2] - pb[2]) > 0.1 {
            return Err("Rail join has incompatible horizontal controls".into());
        }
        merge(a, b);
        let ai = fitted.iter().position(|t| t.id == join.a_track).unwrap();
        let bi = fitted.iter().position(|t| t.id == join.b_track).unwrap();
        let ar = root(&mut connected, ai);
        let br = root(&mut connected, bi);
        connected[ar.max(br)] = ar.min(br);
    }
    for i in 0..fitted.len() {
        if root(&mut connected, i) != root(&mut connected, 0) {
            return Err("Connected grade fit contains a disconnected rail path".into());
        }
    }
    // Every monument approach must name its primary-network endpoint explicitly.
    for t in fitted.iter().filter(|t| t.preserve_start) {
        let last = t.path.points().len() - 1;
        if !joins.iter().any(|j| {
            (j.a_track == t.id && j.a_control == last && j.b_track != t.id)
                || (j.b_track == t.id && j.b_control == last && j.a_track != t.id)
        }) {
            return Err("Connected grade fitting lacks an approach endpoint join".into());
        }
    }
    let mut indices = std::collections::BTreeMap::new();
    let mut mapping = Vec::with_capacity(count);
    for i in 0..count {
        let r = root(&mut parent, i);
        let next = indices.len();
        mapping.push(*indices.entry(r).or_insert(next));
    }
    let mut nodes = vec![
        Node {
            desired: 0.,
            minimum: f64::NEG_INFINITY,
            maximum: f64::INFINITY
        };
        indices.len()
    ];
    let mut weights = vec![0usize; nodes.len()];
    let n = base.surface_resolution;
    let support = Support::new(
        base,
        alpha,
        shape,
        control_allowance(maximum_cut_fill, shape.terrain_offset),
        f64::from(settings.maximum_grade),
        f64::from(policy.sample_spacing),
    );
    let mut work = 0;
    for (i, t) in fitted.iter().enumerate() {
        let horizontal_step = t
            .path
            .points()
            .windows(2)
            .map(|pair| {
                f64::from(pair[1][0] - pair[0][0]).hypot(f64::from(pair[1][2] - pair[0][2]))
            })
            .sum::<f64>()
            // Path::refit_heights uses total control length / control count.
            / t.path.points().len() as f64;
        work += t.path.points().len() - 1;
        if work >= settings.maximum_work {
            return Err("Connected rail height bounds reached their work allowance".into());
        }
        for (j, &p) in t.path.points().iter().enumerate() {
            if cancelled() {
                return Ok(None);
            }
            let u = p[0] / base.size as f32 + 0.5;
            let v = p[2] / base.size as f32 + 0.5;
            if !(0. ..=1.).contains(&u) || !(0. ..=1.).contains(&v) {
                return Err("Grade control leaves terrain".into());
            }
            let cell = (v * n as f32).clamp(0., (n - 1) as f32) as usize * n
                + (u * n as f32).clamp(0., (n - 1) as f32) as usize;
            let mask = base.topology[cell] as u32;
            let Some((mut lo, mut hi)) = support.bounds(
                &t.path,
                j,
                horizontal_step,
                &mut work,
                settings.maximum_work,
                cancelled,
            )?
            else {
                return Ok(None);
            };
            if mask & (shape.river | shape.bank) != 0 {
                let level = water
                    .and_then(|w| w.get(cell))
                    .copied()
                    .filter(|v| v.is_finite() && *v > -500. && *v <= 500.)
                    .ok_or("Connected rail grade fit needs verified crossing water")?;
                lo = lo.max(f64::from(level + policy.water_clearance));
            }
            if mask & shape.preserve_topology != 0 || (t.preserve_start && j < 2) {
                let fixed = f64::from(original[i].path.points()[j][1]);
                lo = lo.max(fixed);
                hi = hi.min(fixed);
            }
            if lo > hi {
                return Err(format!(
                    "Connected rail fixed heights conflict with terrain/earthwork bounds at track {}, control {j}, position {p:?}, interval {lo:.3}..{hi:.3}",
                    t.id
                ));
            }
            let node = &mut nodes[mapping[offsets[i] + j]];
            node.desired += f64::from(p[1]);
            node.minimum = node.minimum.max(lo);
            node.maximum = node.maximum.min(hi);
            weights[mapping[offsets[i] + j]] += 1;
        }
    }
    for (node, weight) in nodes.iter_mut().zip(weights) {
        node.desired /= weight as f64;
        if node.minimum > node.maximum {
            return Err(
                "Connected rail fixed heights conflict with terrain/earthwork bounds".into(),
            );
        }
    }
    let mut local = None;
    let mut last_feasible_grade = f64::from(settings.maximum_grade);
    let mut violations = Vec::new();
    for attempt in 0..settings.maximum_attempts {
        // Try the requested grade before reserving interpolation headroom.
        // Only a failed continuous-curve audit justifies tighter control bounds.
        let headroom = if attempt == 0 {
            1.
        } else {
            0.75 * 0.5f64.powi(i32::from(attempt) - 1)
        };
        let edge_grade = f64::from(settings.maximum_grade) * headroom;
        let mut edges = Vec::new();
        for (i, t) in fitted.iter().enumerate() {
            for (j, w) in t.path.points().windows(2).enumerate() {
                edges.push(Edge {
                    a: mapping[offsets[i] + j],
                    b: mapping[offsets[i] + j + 1],
                    maximum_difference: f64::from(w[1][0] - w[0][0])
                        .hypot(f64::from(w[1][2] - w[0][2]))
                        * local
                            .as_ref()
                            .map_or(edge_grade, |limits: &Vec<f64>| limits[edges.len()]),
                });
            }
        }
        let remaining = settings.maximum_work.saturating_sub(work);
        if remaining == 0 {
            return Err("Connected rail grade fit allowance reached".into());
        }
        let (projection, conflict, projection_work) =
            rail_grade_fit::solve_measured(&nodes, &edges, remaining, cancelled);
        let (heights, used) = match projection {
            Ok(v) => v,
            Err(rail_grade_fit::Failure::Cancelled) => return Ok(None),
            Err(rail_grade_fit::Failure::Infeasible)
                if local.is_none()
                    && !violations.is_empty()
                    && attempt + 1 < settings.maximum_attempts =>
            {
                // Global headroom can make a distant fixed approach infeasible.
                // Keep the last feasible projection's headroom on the rest
                // of the network and tighten only the curves that failed it.
                work += projection_work;
                let mut limits = vec![last_feasible_grade; edges.len()];
                tighten_segments(&mut limits, &violations, f64::from(settings.maximum_grade));
                local = Some(limits);
                continue;
            }
            Err(rail_grade_fit::Failure::Infeasible)
                if local.is_some()
                    && last_feasible_grade < f64::from(settings.maximum_grade)
                    && attempt + 1 < settings.maximum_attempts =>
            {
                // Some fixed endpoints need the remaining grade allowance
                // once their neighbouring segments have been tightened.
                work += projection_work;
                last_feasible_grade = f64::from(settings.maximum_grade);
                let mut limits = vec![last_feasible_grade; edges.len()];
                tighten_segments(&mut limits, &violations, last_feasible_grade);
                local = Some(limits);
                continue;
            }
            Err(e) => {
                let controls =
                    conflict.map(|c| {
                        fitted
                            .iter()
                            .enumerate()
                            .flat_map(|(i, track)| {
                                let mapping = &mapping;
                                let offsets = &offsets;
                                track.path.points().iter().enumerate().filter_map(
                                    move |(j, point)| {
                                        (mapping[offsets[i] + j] == c.node)
                                            .then_some((track.id, j, point))
                                    },
                                )
                            })
                            .collect::<Vec<_>>()
                    });
                return Err(format!(
                    "Connected rail height constraints failed: {e:?}, attempt {attempt}, grade {edge_grade}, conflict {conflict:?}, controls {controls:?}"
                ));
            }
        };
        work += used;
        let mut paths = Vec::new();
        let mut audits = Vec::new();
        let mut maximum_change: f64 = 0.;
        let mut passes = true;
        let mut next_violations = Vec::new();
        let mut first_edge = 0;
        for (i, t) in fitted.iter().enumerate() {
            let y: Vec<f32> = (offsets[i]..offsets[i + 1])
                .map(|j| heights[mapping[j]] as f32)
                .collect();
            for (&y, p) in y.iter().zip(t.path.points()) {
                maximum_change = maximum_change.max(f64::from((y - p[1]).abs()));
            }
            let Some(path) = t
                .path
                .refit_heights(
                    &y,
                    0,
                    &vec![1.; y.len()],
                    settings.maximum_work.saturating_sub(work).min(32000000),
                    cancelled,
                )
                .map_err(|e| format!("Connected rail refit failed: {e:?}"))?
            else {
                return Ok(None);
            };
            work += path.work();
            let remaining = settings.maximum_work.saturating_sub(work);
            if remaining == 0 {
                return Err("Connected rail grade analysis allowance reached".into());
            }
            let maximum_grade = f64::from(settings.maximum_grade);
            let edge_count = path.points().len() - 1;
            let Some(audit) = rail_grade::analyze_segments(
                &path,
                rail_grade::Settings {
                    absolute_tolerance: 0.00001,
                    maximum_work: remaining.min(4000000),
                    maximum_depth: 24,
                },
                |index, bound| {
                    if bound.is_none_or(|value| value > maximum_grade) {
                        let first = first_edge + index.saturating_sub(1);
                        let last = first_edge + (index + 1).min(edge_count - 1);
                        next_violations.push((first..=last, bound));
                    }
                },
                cancelled,
            )
            .map_err(|e| format!("Connected rail continuous grade check failed: {e:?}"))?
            else {
                return Ok(None);
            };
            work += audit.work;
            passes &= audit
                .maximum_grade_upper_bound
                .is_some_and(|v| v <= f64::from(settings.maximum_grade));
            paths.push(path);
            audits.push(audit);
            first_edge += edge_count;
        }
        if passes {
            for (track, path) in fitted.iter_mut().zip(paths) {
                track.path = path;
            }
            return Ok(Some(Report {
                engine: VERSION,
                attempts: attempt + 1,
                effective_edge_grade: local.as_ref().map_or(edge_grade, |limits| {
                    limits.iter().copied().fold(f64::INFINITY, f64::min)
                }),
                work,
                nodes: nodes.len(),
                edges: edges.len(),
                maximum_control_change: maximum_change,
                continuous_grades: audits,
            }));
        }
        violations = next_violations;
        if let Some(limits) = &mut local {
            tighten_segments(limits, &violations, f64::from(settings.maximum_grade));
        } else {
            last_feasible_grade = edge_grade;
        }
    }
    let failed_spans: Vec<_> = violations
        .iter()
        .take(8)
        .filter_map(|(edges, bound)| {
            let first = *edges.start();
            // Each track contributes one edge fewer than its control count.
            let track = (0..fitted.len()).rfind(|&i| offsets[i] - i <= first)?;
            let first_edge = offsets[track] - track;
            Some(serde_json::json!({
                "track": fitted[track].id,
                "control_spans": [first - first_edge, *edges.end() - first_edge],
                "grade_upper_bound": bound,
            }))
        })
        .collect();
    let detail = serde_json::json!({
        "maximum_grade": settings.maximum_grade,
        "failed_span_count": violations.len(),
        "failed_spans": failed_spans,
    });
    Err(format!(
        "Connected rail fit could not satisfy the requested continuous grade within its limits: {detail}"
    ))
}

fn tighten_segments(
    limits: &mut [f64],
    violations: &[(std::ops::RangeInclusive<usize>, Option<f64>)],
    grade: f64,
) {
    // Adjacent failing curves share tangents. Tighten an edge only once per
    // pass even if several of those curves requested the same control.
    let mut factors = vec![1_f64; limits.len()];
    for (edges, bound) in violations {
        let factor = bound.map_or(0.5, |value| (grade / value * 0.9).clamp(0.45, 0.8));
        for edge in edges.clone() {
            factors[edge] = factors[edge].min(factor);
        }
    }
    for (limit, factor) in limits.iter_mut().zip(factors) {
        *limit *= factor;
    }
}
