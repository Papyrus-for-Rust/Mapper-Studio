//! Transactional sparse fitting of terrain-snapped collision-mesh grades.
//! A passing proposal is not terrain-clearance, PhysX or world acceptance.
use crate::{
    path_mesh::{Chunk, HeightBinding, Template},
    path_terrain::{self, Grid, Layout},
    road_grade::Junction,
    road_mesh_audit,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
pub const VERSION: &str = "papyrus-road-bed-fit-1";
pub const COUPLED_VERSION: &str = "papyrus-road-bed-fit-2";
pub const ENVELOPE_VERSION: &str = "papyrus-road-bed-fit-3";
pub const CONTROLS_VERSION: &str = "papyrus-road-bed-controls-1";
pub const REBUILD_VERSION: &str = "papyrus-road-bed-rebuild-feedback-1";
pub const QUANTIZED_REBUILD_VERSION: &str = "papyrus-road-bed-quantized-rebuild-2";
pub const LOCAL_REBUILD_VERSION: &str = "papyrus-road-bed-local-rebuild-1";
const MAX_VARIABLES: usize = 2_000_000;
const MAX_ROWS: usize = 1_000_000;
const MAX_TERMS: usize = 8_000_000;
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub maximum_grade: f32,
    pub maximum_cut_fill: f32,
    pub minimum_up_normal: f32,
    pub maximum_iterations: usize,
    pub maximum_work: usize,
    /// Reserve in encoded-short units for rounding the free grid variables.
    pub quantization_reserve: f64,
    /// Optional model headroom; exact quantized acceptance is always required.
    #[serde(default = "default_roundoff_reserve")]
    pub reserve_roundoff: bool,
}
impl Settings {
    pub fn validate(self) -> Result<(), Failure> {
        if !self.maximum_grade.is_finite()
            || !(0.001..=1.).contains(&self.maximum_grade)
            || !self.maximum_cut_fill.is_finite()
            || !(0. ..=64.).contains(&self.maximum_cut_fill)
            || !self.minimum_up_normal.is_finite()
            || !(0. ..=1.).contains(&self.minimum_up_normal)
            || !self.quantization_reserve.is_finite()
            || !(0. ..=2.).contains(&self.quantization_reserve)
            || !(1..=4096).contains(&self.maximum_iterations)
            || !(1..=2_000_000_000).contains(&self.maximum_work)
        {
            Err(Failure::Invalid("settings or buffers"))
        } else {
            Ok(())
        }
    }
}
fn default_roundoff_reserve() -> bool {
    true
}
fn roundoff_margin(settings: Settings, span: f32, scale: f64) -> f64 {
    if settings.reserve_roundoff {
        16. * f64::from(f32::EPSILON) * f64::from(span) * scale
    } else {
        0.
    }
}
pub struct Collision<'a> {
    pub road: usize,
    pub chunk_index: usize,
    pub geometry: &'a Chunk,
    pub template: &'a Template,
    pub heights: &'a [HeightBinding],
}
/// A captured continuous-clearance witness. Passing these witnesses alone is
/// not continuous surface clearance; regenerate and audit for new maxima.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClearanceProbe {
    pub road: usize,
    pub chunk: usize,
    pub triangle: usize,
    pub position: [f64; 2],
    pub maximum_terrain_excess: f32,
}
#[derive(Clone, Copy)]
pub struct Input<'a> {
    pub grid: Grid<'a>,
    pub ground: &'a [i16],
    pub topology: &'a [i32],
    pub surface_resolution: usize,
    pub protected_topology: i32,
    pub controls: &'a [Vec<[f32; 3]>],
    pub junctions: &'a [Junction],
    pub collisions: &'a [Collision<'a>],
}
/// Opt-in bounds from a receipt-covered source road-height pass. Only samples
/// changed by that pass, or covered by its explicit lower-quantization receipt,
/// with no other protected flags, may move. Receipts are bound by the caller.
#[derive(Clone, Copy)]
pub struct PaintEnvelope<'a> {
    pub source_painted: &'a [i16],
    pub source_rounding: Option<&'a crate::road_terrain::SourceCutRounding>,
    pub allowed_topology: i32,
}
impl PaintEnvelope<'_> {
    pub fn protected_bounds(self, ground: i16, flags: i32, cell: usize) -> Option<(i32, i32)> {
        let painted = i32::from(*self.source_painted.get(cell)?);
        let ground = i32::from(ground);
        let lower = self
            .source_rounding
            .and_then(|r| r.lower(self.source_painted, cell))
            .map_or(painted, i32::from);
        (flags != 0
            && flags & !self.allowed_topology == 0
            && (ground != painted || lower != painted))
            .then_some((ground.min(lower), ground.max(painted)))
    }
}
#[derive(Debug, PartialEq, Serialize)]
pub enum Failure {
    Invalid(&'static str),
    HorizontalGeometry {
        road: usize,
        chunk: usize,
        triangle: usize,
        inverted: usize,
        degenerate: usize,
    },
    Budget,
    Cancelled,
    Infeasible {
        road: usize,
        chunk: usize,
        triangle: usize,
        minimum: f64,
        maximum: f64,
        limit: f64,
    },
    NoConvergence {
        iterations: usize,
        remaining: usize,
        maximum_grade: f64,
        remaining_clearance: usize,
        maximum_terrain_excess: f64,
    },
    ClearanceInfeasible {
        /// Metered fitting work, including model construction before rejection.
        work: usize,
        road: usize,
        chunk: usize,
        triangle: usize,
        position: [f64; 2],
        topology: i32,
        free_variables: usize,
        minimum: f64,
        limit: f64,
    },
    ControlInfeasible {
        road: usize,
        segment: usize,
        minimum: f64,
        maximum: f64,
        limit: f64,
    },
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retained_sample_entries: Option<usize>,
    pub settings: Settings,
    pub variables: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repair_sample_limit: Option<usize>,
    pub constraints: usize,
    pub terms: usize,
    pub headroom_clipped_rows: usize,
    pub iterations: usize,
    pub projection_work_limit_reached: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quantized_polish: Option<PolishReport>,
    pub changed_samples: usize,
    pub maximum_raise: f32,
    pub maximum_lower: f32,
    pub maximum_repair: f32,
    pub maximum_mesh_grade_before: f64,
    pub maximum_mesh_grade_after: f64,
    pub violations_before: usize,
    pub violations_after: usize,
    pub clearance_probes_checked: usize,
    pub clearance_probe_violations_before: usize,
    pub clearance_probe_violations_after: usize,
    pub maximum_probe_terrain_excess_before: f64,
    pub maximum_probe_terrain_excess_after: f64,
    pub control_supports_frozen: bool,
    pub control_heights_resampled: usize,
    pub maximum_control_grade: f64,
    pub control_horizontal_and_junctions_preserved: bool,
    pub source_paths_must_be_rebuilt: bool,
    pub controls_checked: usize,
    pub junctions_checked: usize,
    pub protected_samples_checked: usize,
    pub protected_envelope_samples: usize,
    pub protected_samples_changed: usize,
    pub maximum_protected_repair: f32,
    pub protected_paint_envelope_validated: bool,
    pub work: usize,
    pub exact_quantized_mesh_grades_validated: bool,
    pub controls_and_junctions_unchanged: bool,
    pub protected_samples_unchanged: bool,
    pub full_grid_earthwork_validated: bool,
    pub terrain_surface_clearance_validated: bool,
    pub water_clearance_validated: bool,
    pub runtime_access_validated: bool,
}
pub struct Output {
    pub height: Vec<i16>,
    pub controls: Vec<Vec<[f32; 3]>>,
    pub report: Report,
}
/// Private-generation candidate. The caller must rebuild meshes and independently
/// pass all final grade/clearance checks before publishing any of these buffers.
pub struct RebuildCandidate {
    pub height: Vec<i16>,
    pub controls: Vec<Vec<[f32; 3]>>,
    pub report: Report,
    pub mesh_fit_passed: bool,
}
#[derive(Debug, Serialize)]
pub struct PolishReport {
    pub passes: usize,
    pub moves: usize,
    pub work: usize,
    pub adopted: bool,
    pub work_limit_reached: bool,
}
struct QuantizedState {
    values: Vec<i16>,
    assessment: (usize, f64, usize, f64),
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Face {
    pub road: usize,
    pub chunk: usize,
    pub triangle: usize,
}
#[derive(Clone, Debug, Serialize)]
pub struct Progress {
    pub phase: &'static str,
    pub iteration: usize,
    pub variables: usize,
    pub constraints: usize,
    pub headroom_clipped_rows: usize,
    pub work: usize,
    pub linear_maximum_excess: f64,
    pub last_exact_violations: usize,
    pub last_exact_maximum_grade: f64,
    pub last_exact_clearance_violations: usize,
    pub last_exact_maximum_terrain_excess: f64,
    pub worst: Option<Face>,
}
struct Meter<'a, F: Fn() -> bool> {
    work: usize,
    limit: usize,
    cancel: &'a F,
}
impl<F: Fn() -> bool> Meter<'_, F> {
    fn audit_allowance(&self) -> Result<usize, Failure> {
        self.limit
            .checked_sub(self.work)
            .filter(|v| *v > 0)
            .map(|v| v.min(32_000_000))
            .ok_or(Failure::Budget)
    }
    fn spend(&mut self, n: usize) -> Result<(), Failure> {
        if (self.cancel)() {
            return Err(Failure::Cancelled);
        }
        self.work = self
            .work
            .checked_add(n)
            .filter(|v| *v <= self.limit)
            .ok_or(Failure::Budget)?;
        Ok(())
    }
}
#[derive(Clone)]
struct Variable {
    grid: usize,
    value: f64,
    minimum: f64,
    maximum: f64,
    box_correction: f64,
}
struct Row {
    face: Face,
    start: usize,
    end: usize,
    constant: f64,
    lower: f64,
    limit: f64,
    norm: f64,
    raw_lower: f64,
    raw_limit: f64,
    correction: f64,
}
struct Model<'a> {
    variables: Vec<Variable>,
    indices: HashMap<usize, Option<usize>>,
    rows: Vec<Row>,
    terms: Vec<(usize, f64)>,
    probes: BTreeMap<(usize, usize), Vec<PreparedProbe>>,
    resample_controls: bool,
    envelope: Option<PaintEnvelope<'a>>,
    allowed_samples: Option<&'a BTreeSet<usize>>,
    headroom_clipped: usize,
}
struct PreparedProbe {
    vertices: [usize; 3],
    weights: [f64; 3],
    position: [f32; 2],
    maximum_terrain_excess: f32,
}
/// Optional rounding headroom cannot be stronger than a row's box envelope.
/// Real saved bounds and the exact quantized acceptance gate never change.
fn bounded_headroom(ideal: f64, raw: f64, nearest: f64) -> (f64, bool) {
    if nearest > ideal && nearest <= raw {
        (nearest + (raw - nearest) * 0.5, true)
    } else {
        (ideal, false)
    }
}
fn polish_values<F: Fn() -> bool>(
    model: &Model<'_>,
    values: &mut [i16],
    meter: &mut Meter<'_, F>,
) -> Result<(usize, usize), Failure> {
    meter.spend(0)?;
    let mut adjacency = vec![Vec::<(usize, f64)>::new(); model.variables.len()];
    let mut row_values = Vec::with_capacity(model.rows.len());
    for (r, row) in model.rows.iter().enumerate() {
        meter.spend((row.end - row.start) * 2 + 1)?;
        let mut value = row.constant;
        for &(i, c) in &model.terms[row.start..row.end] {
            adjacency[i].push((r, c));
            value += f64::from(values[i]) * c;
        }
        row_values.push(value);
    }
    let violation = |row: &Row, value: f64| {
        (value - (row.raw_limit - 1e-5))
            .max((row.raw_lower + 1e-5) - value)
            .max(0.)
    };
    let mut moves = 0;
    for pass in 0..64 {
        let mut active = BTreeSet::new();
        for (row, value) in model.rows.iter().zip(&row_values) {
            meter.spend(1)?;
            if violation(row, *value) > 0. {
                meter.spend(row.end - row.start)?;
                active.extend(model.terms[row.start..row.end].iter().map(|&(i, _)| i));
            }
        }
        let mut changed = false;
        for i in active {
            let variable = &model.variables[i];
            let mut best = (0., 0_i32);
            for delta in [-1_i32, 1] {
                let candidate = i32::from(values[i]) + delta;
                if f64::from(candidate) < variable.minimum
                    || f64::from(candidate) > variable.maximum
                {
                    continue;
                }
                meter.spend(adjacency[i].len() * 2 + 1)?;
                let gain = adjacency[i]
                    .iter()
                    .map(|&(r, c)| {
                        let before = violation(&model.rows[r], row_values[r]);
                        let after = violation(&model.rows[r], row_values[r] + c * f64::from(delta));
                        before * before - after * after
                    })
                    .sum::<f64>();
                if gain > best.0 + 1e-16 {
                    best = (gain, delta);
                }
            }
            if best.1 != 0 {
                meter.spend(adjacency[i].len() + 1)?;
                values[i] = (i32::from(values[i]) + best.1) as i16;
                for &(r, c) in &adjacency[i] {
                    row_values[r] += c * f64::from(best.1);
                }
                moves += 1;
                changed = true;
            }
        }
        if !changed {
            // Quantized samples can be coupled by an almost-tight neighbour:
            // neither one-unit coordinate move helps, while a simultaneous
            // pair does. Search a bounded set from the worst residual rows.
            let mut worst = Vec::new();
            for (r, row) in model.rows.iter().enumerate() {
                meter.spend(1)?;
                let residual = violation(row, row_values[r]);
                if residual > 0. {
                    worst.push((residual, r));
                }
            }
            meter.spend(
                worst
                    .len()
                    .saturating_mul((worst.len().max(1).ilog2() + 1) as usize),
            )?;
            worst.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
            let mut pairs = BTreeSet::new();
            'rows: for (_, r) in worst {
                let row = &model.rows[r];
                let terms = &model.terms[row.start..row.end];
                for (index, &(i, _)) in terms.iter().enumerate() {
                    meter.spend(1)?;
                    if model.variables[i].minimum == model.variables[i].maximum {
                        continue;
                    }
                    for &(j, _) in &terms[index + 1..] {
                        meter.spend(1)?;
                        if i == j || model.variables[j].minimum == model.variables[j].maximum {
                            continue;
                        }
                        pairs.insert((i.min(j), i.max(j)));
                        if pairs.len() == 4096 {
                            break 'rows;
                        }
                    }
                    // The compensating sample may belong only to a nearly
                    // tight neighbouring row, outside the currently bad face.
                    for &(neighbor, coefficient) in &adjacency[i] {
                        meter.spend(1)?;
                        let other = &model.rows[neighbor];
                        let slack = (row_values[neighbor] - other.raw_lower)
                            .min(other.raw_limit - row_values[neighbor]);
                        if slack > coefficient.abs() + 1e-5 {
                            continue;
                        }
                        for &(j, _) in &model.terms[other.start..other.end] {
                            meter.spend(1)?;
                            if i == j || model.variables[j].minimum == model.variables[j].maximum {
                                continue;
                            }
                            pairs.insert((i.min(j), i.max(j)));
                            if pairs.len() == 4096 {
                                break 'rows;
                            }
                        }
                    }
                }
            }
            let mut best = (0., None);
            for (i, j) in pairs {
                meter.spend((adjacency[i].len() + adjacency[j].len()) * 2 + 1)?;
                // Adjacencies are already ordered by row. Merge once so a
                // shared row sees both changes together, not two separate gains.
                let mut rows = Vec::with_capacity(adjacency[i].len() + adjacency[j].len());
                let (mut a, mut b) = (0, 0);
                while a < adjacency[i].len() || b < adjacency[j].len() {
                    match (adjacency[i].get(a), adjacency[j].get(b)) {
                        (Some(&(r, c)), Some(&(s, d))) if r == s => {
                            rows.push((r, c, d));
                            a += 1;
                            b += 1;
                        }
                        (Some(&(r, c)), Some(&(s, _))) if r < s => {
                            rows.push((r, c, 0.));
                            a += 1;
                        }
                        (Some(_), Some(&(s, d))) => {
                            rows.push((s, 0., d));
                            b += 1;
                        }
                        (Some(&(r, c)), None) => {
                            rows.push((r, c, 0.));
                            a += 1;
                        }
                        (None, Some(&(s, d))) => {
                            rows.push((s, 0., d));
                            b += 1;
                        }
                        (None, None) => break,
                    }
                }
                for di in [-1_i32, 1] {
                    for dj in [-1_i32, 1] {
                        let vi = i32::from(values[i]) + di;
                        let vj = i32::from(values[j]) + dj;
                        if f64::from(vi) < model.variables[i].minimum
                            || f64::from(vi) > model.variables[i].maximum
                            || f64::from(vj) < model.variables[j].minimum
                            || f64::from(vj) > model.variables[j].maximum
                        {
                            continue;
                        }
                        meter.spend(rows.len() * 2 + 1)?;
                        let gain = rows
                            .iter()
                            .map(|&(r, c, d)| {
                                let before = violation(&model.rows[r], row_values[r]);
                                let after = violation(
                                    &model.rows[r],
                                    row_values[r] + c * f64::from(di) + d * f64::from(dj),
                                );
                                before * before - after * after
                            })
                            .sum::<f64>();
                        if gain > best.0 + 1e-16 {
                            best = (gain, Some((i, j, di, dj)));
                        }
                    }
                }
            }
            let Some((i, j, di, dj)) = best.1 else {
                return Ok((pass + 1, moves));
            };
            meter.spend(adjacency[i].len() + adjacency[j].len() + 2)?;
            for (index, delta) in [(i, di), (j, dj)] {
                values[index] = (i32::from(values[index]) + delta) as i16;
                for &(r, c) in &adjacency[index] {
                    row_values[r] += c * f64::from(delta);
                }
            }
            moves += 2;
        }
    }
    Ok((64, moves))
}
fn layout(grid: Grid<'_>) -> Layout {
    Layout {
        size: grid.size,
        resolution: grid.resolution,
        origin_y: grid.origin_y,
        vertical_span: grid.vertical_span,
    }
}
fn protected_flags(input: &Input<'_>, i: usize) -> i32 {
    let n = input.grid.resolution;
    let sn = input.surface_resolution;
    let size = input.grid.size;
    let [x, z] = [i % n, i / n].map(|v| {
        let p = v as f32 / (n - 1) as f32 * size - size * 0.5;
        (((p + size * 0.5) * (1. / size)) * sn as f32).clamp(0., (sn - 1) as f32) as usize
    });
    input.topology[z * sn + x] & input.protected_topology
}
fn protected(input: &Input<'_>, i: usize) -> bool {
    protected_flags(input, i) != 0
}
fn envelope_bounds(
    input: &Input<'_>,
    envelope: Option<PaintEnvelope<'_>>,
    i: usize,
) -> Option<(i32, i32)> {
    let flags = protected_flags(input, i);
    let e = envelope?;
    e.protected_bounds(input.ground[i], flags, i)
}
fn add_query(
    terms: &mut BTreeMap<usize, f64>,
    grid: Grid<'_>,
    p: [f32; 2],
    weight: f64,
) -> Result<(), Failure> {
    let support = path_terrain::height_support(layout(grid), [p[0], 0., p[1]])
        .map_err(|_| Failure::Invalid("query support"))?;
    let quantum = f64::from(path_terrain::SHORT_TO_FLOAT) * f64::from(grid.vertical_span);
    for (i, w) in support.indices.into_iter().zip(support.weights()) {
        *terms.entry(i).or_default() += w * weight * quantum;
    }
    Ok(())
}
impl Model<'_> {
    fn vertex(
        &mut self,
        input: &Input<'_>,
        binding: &HeightBinding,
        p: [f32; 3],
        origin: f32,
        frozen: &BTreeSet<usize>,
        allowance: i32,
    ) -> Result<Vec<(usize, f64)>, Failure> {
        let height = binding
            .resolve(|x, z| path_terrain::sample_height(input.grid, [x, 0., z]).ok())
            .ok_or(Failure::Invalid("height binding"))?;
        if (height - origin).to_bits() != p[1].to_bits() {
            return Err(Failure::Invalid("foreign height binding"));
        }
        let mut terms = BTreeMap::new();
        match *binding {
            HeightBinding::Endpoint { position } => {
                add_query(&mut terms, input.grid, position, 1.)?
            }
            HeightBinding::Span {
                left,
                right,
                across,
                ..
            } => {
                add_query(&mut terms, input.grid, left, 1. - f64::from(across))?;
                add_query(&mut terms, input.grid, right, f64::from(across))?;
            }
        }
        self.variable_terms(input, terms, frozen, allowance)
    }
    fn variable_terms(
        &mut self,
        input: &Input<'_>,
        terms: BTreeMap<usize, f64>,
        frozen: &BTreeSet<usize>,
        allowance: i32,
    ) -> Result<Vec<(usize, f64)>, Failure> {
        let mut output = Vec::with_capacity(terms.len());
        for (i, coefficient) in terms {
            if coefficient == 0. {
                continue;
            }
            let index = if let Some(index) = self.indices.get(&i) {
                *index
            } else {
                let envelope = envelope_bounds(input, self.envelope, i);
                let index = if frozen.contains(&i)
                    || self
                        .allowed_samples
                        .is_some_and(|allowed| !allowed.contains(&i))
                    || (protected(input, i) && envelope.is_none())
                {
                    None
                } else {
                    if self.variables.len() == MAX_VARIABLES {
                        return Err(Failure::Budget);
                    }
                    let ground = i32::from(input.ground[i]);
                    let minimum = (ground - allowance).max(0).max(envelope.map_or(0, |b| b.0));
                    let maximum = (ground + allowance)
                        .min(32766)
                        .min(envelope.map_or(32766, |b| b.1));
                    if minimum == maximum {
                        None
                    } else {
                        let index = self.variables.len();
                        self.variables.push(Variable {
                            grid: i,
                            value: f64::from(input.grid.height[i]),
                            minimum: f64::from(minimum),
                            maximum: f64::from(maximum),
                            box_correction: 0.,
                        });
                        Some(index)
                    }
                };
                self.indices.insert(i, index);
                index
            };
            if let Some(index) = index {
                output.push((index, coefficient));
            }
        }
        Ok(output)
    }
    fn read(&self, input: &Input<'_>, values: &[i16], i: usize) -> i16 {
        self.indices
            .get(&i)
            .and_then(|i| *i)
            .map_or(input.grid.height[i], |i| values[i])
    }
}
fn audit_error(error: road_mesh_audit::Failure) -> Failure {
    match error {
        road_mesh_audit::Failure::Budget => Failure::Budget,
        road_mesh_audit::Failure::Cancelled => Failure::Cancelled,
        road_mesh_audit::Failure::Invalid => Failure::Invalid("collision geometry"),
    }
}
fn add_clearance_probes<F: Fn() -> bool>(
    model: &mut Model,
    input: &Input<'_>,
    settings: Settings,
    probes: &[ClearanceProbe],
    frozen: &BTreeSet<usize>,
    allowance: i32,
    meter: &mut Meter<'_, F>,
) -> Result<(), Failure> {
    let collisions: BTreeMap<_, _> = input
        .collisions
        .iter()
        .map(|c| ((c.road, c.chunk_index), c))
        .collect();
    let mut seen = BTreeSet::new();
    for probe in probes {
        meter.spend(512)?;
        if probe
            .position
            .iter()
            .any(|v| !v.is_finite() || v.abs() > f64::from(input.grid.size) * 0.5)
            || !probe.maximum_terrain_excess.is_finite()
            || !(-1. ..=1.).contains(&probe.maximum_terrain_excess)
            || !seen.insert((
                probe.road,
                probe.chunk,
                probe.triangle,
                probe.position[0].to_bits(),
                probe.position[1].to_bits(),
            ))
        {
            return Err(Failure::Invalid("clearance probe"));
        }
        let collision = collisions
            .get(&(probe.road, probe.chunk))
            .ok_or(Failure::Invalid("clearance collision"))?;
        let face = collision
            .geometry
            .mesh
            .triangles
            .get(probe.triangle)
            .ok_or(Failure::Invalid("clearance triangle"))?
            .map(|i| i as usize);
        let authored =
            collision.template.triangles[probe.triangle % collision.template.triangles.len()];
        if authored
            .iter()
            .any(|&i| collision.template.normals[i as usize][1] < settings.minimum_up_normal)
        {
            return Err(Failure::Invalid("clearance driving surface"));
        }
        let world = face.map(|i| {
            std::array::from_fn::<_, 3, _>(|j| {
                f64::from(collision.geometry.mesh.vertices[i][j] + collision.geometry.origin[j])
            })
        });
        let [a, b, c] = world;
        let ab = [b[0] - a[0], b[2] - a[2]];
        let ac = [c[0] - a[0], c[2] - a[2]];
        let point = [probe.position[0] - a[0], probe.position[1] - a[2]];
        let det = ab[0] * ac[1] - ab[1] * ac[0];
        if det.abs() < 1e-10 {
            return Err(Failure::Invalid("clearance projected triangle"));
        }
        let wb = (point[0] * ac[1] - point[1] * ac[0]) / det;
        let wc = (ab[0] * point[1] - ab[1] * point[0]) / det;
        let weights = [1. - wb - wc, wb, wc];
        if weights
            .iter()
            .any(|v| !v.is_finite() || *v < -1e-6 || *v > 1. + 1e-6)
        {
            return Err(Failure::Invalid("clearance witness outside triangle"));
        }
        let position = probe.position.map(|v| v as f32);
        let mut raw = BTreeMap::new();
        add_query(&mut raw, input.grid, position, 1.)?;
        let mut terms: BTreeMap<_, _> = model
            .variable_terms(input, raw, frozen, allowance)?
            .into_iter()
            .collect();
        for (id, weight) in face.into_iter().zip(weights) {
            let edge = model.vertex(
                input,
                &collision.heights[id],
                collision.geometry.mesh.vertices[id],
                collision.geometry.origin[1],
                frozen,
                allowance,
            )?;
            for (index, value) in edge {
                *terms.entry(index).or_default() -= weight * value;
            }
        }
        terms.retain(|_, v| v.abs() > 1e-14);
        let terrain = path_terrain::sample_height(input.grid, [position[0], 0., position[1]])
            .map_err(|_| Failure::Invalid("clearance terrain sample"))?;
        let road: f64 = world.into_iter().zip(weights).map(|(p, w)| p[1] * w).sum();
        let constant = f64::from(terrain)
            - road
            - terms
                .iter()
                .map(|(&i, &c)| c * model.variables[i].value)
                .sum::<f64>();
        let norm = terms.values().map(|v| v * v).sum::<f64>();
        let margin = if norm == 0. {
            0.
        } else {
            settings.quantization_reserve * terms.values().map(|v| v.abs()).sum::<f64>()
                + roundoff_margin(
                    settings,
                    input.grid.vertical_span,
                    1. + weights.iter().map(|v| v.abs()).sum::<f64>(),
                )
        };
        let raw_limit = f64::from(probe.maximum_terrain_excess);
        let minimum = constant
            + terms
                .iter()
                .map(|(&i, &c)| {
                    c * if c >= 0. {
                        model.variables[i].minimum
                    } else {
                        model.variables[i].maximum
                    }
                })
                .sum::<f64>();
        if minimum > raw_limit + 1e-9 {
            return Err(Failure::ClearanceInfeasible {
                work: meter.work,
                road: probe.road,
                chunk: probe.chunk,
                triangle: probe.triangle,
                position: probe.position,
                topology: {
                    let n = input.surface_resolution;
                    let [x, z] = position.map(|v| {
                        (((v + input.grid.size * 0.5) * (1. / input.grid.size)) * n as f32)
                            .clamp(0., (n - 1) as f32) as usize
                    });
                    input.topology[z * n + x]
                },
                free_variables: terms
                    .keys()
                    .filter(|&&i| model.variables[i].minimum < model.variables[i].maximum)
                    .count(),
                minimum,
                limit: raw_limit,
            });
        }
        let (limit, clipped) = bounded_headroom(raw_limit - margin, raw_limit, minimum);
        model.headroom_clipped += usize::from(clipped);
        if norm > 0. {
            if model.rows.len() == MAX_ROWS || model.terms.len() + terms.len() > MAX_TERMS {
                return Err(Failure::Budget);
            }
            let start = model.terms.len();
            model.terms.extend(terms);
            model.rows.push(Row {
                face: Face {
                    road: probe.road,
                    chunk: probe.chunk,
                    triangle: probe.triangle,
                },
                start,
                end: model.terms.len(),
                constant,
                lower: f64::NEG_INFINITY,
                raw_lower: f64::NEG_INFINITY,
                raw_limit,
                limit,
                norm,
                correction: 0.,
            });
        }
        model
            .probes
            .entry((probe.road, probe.chunk))
            .or_default()
            .push(PreparedProbe {
                vertices: face,
                weights,
                position,
                maximum_terrain_excess: probe.maximum_terrain_excess,
            });
    }
    Ok(())
}
fn add_control_grade_rows<F: Fn() -> bool>(
    model: &mut Model,
    input: &Input<'_>,
    settings: Settings,
    frozen: &BTreeSet<usize>,
    allowance: i32,
    meter: &mut Meter<'_, F>,
) -> Result<(), Failure> {
    for (road, points) in input.controls.iter().enumerate() {
        for (segment, pair) in points.windows(2).enumerate() {
            meter.spend(128)?;
            let run = f64::from(pair[1][0] - pair[0][0]).hypot(f64::from(pair[1][2] - pair[0][2]));
            if !run.is_finite() || run <= 1e-8 {
                return Err(Failure::Invalid("control horizontal run"));
            }
            let mut raw = BTreeMap::new();
            for (p, weight) in pair.iter().zip([-1., 1.]) {
                add_query(&mut raw, input.grid, [p[0], p[2]], weight)?;
            }
            let mut terms: BTreeMap<_, _> = model
                .variable_terms(input, raw, frozen, allowance)?
                .into_iter()
                .collect();
            terms.retain(|_, v| v.abs() > 1e-14);
            let constant = f64::from(pair[1][1])
                - f64::from(pair[0][1])
                - terms
                    .iter()
                    .map(|(&i, &c)| c * model.variables[i].value)
                    .sum::<f64>();
            let norm = terms.values().map(|v| v * v).sum::<f64>();
            let margin = if norm == 0. {
                0.
            } else {
                settings.quantization_reserve * terms.values().map(|v| v.abs()).sum::<f64>()
                    + roundoff_margin(settings, input.grid.vertical_span, 2.)
            };
            let raw_limit = f64::from(settings.maximum_grade) * run;
            let minimum = constant
                + terms
                    .iter()
                    .map(|(&i, &c)| {
                        c * if c >= 0. {
                            model.variables[i].minimum
                        } else {
                            model.variables[i].maximum
                        }
                    })
                    .sum::<f64>();
            let maximum = constant
                + terms
                    .iter()
                    .map(|(&i, &c)| {
                        c * if c >= 0. {
                            model.variables[i].maximum
                        } else {
                            model.variables[i].minimum
                        }
                    })
                    .sum::<f64>();
            if minimum > raw_limit + 1e-9 || maximum < -raw_limit - 1e-9 {
                return Err(Failure::ControlInfeasible {
                    road,
                    segment,
                    minimum,
                    maximum,
                    limit: raw_limit,
                });
            }
            let (limit, clipped) = bounded_headroom(
                (raw_limit - margin).max(0.),
                raw_limit,
                minimum.max(-maximum).max(0.),
            );
            model.headroom_clipped += usize::from(clipped);
            if norm > 0. {
                if model.rows.len() == MAX_ROWS || model.terms.len() + terms.len() > MAX_TERMS {
                    return Err(Failure::Budget);
                }
                let start = model.terms.len();
                model.terms.extend(terms);
                // No mesh triangle owns a control constraint; MAX marks this
                // explicitly in the diagnostic worst-row address.
                model.rows.push(Row {
                    face: Face {
                        road,
                        chunk: usize::MAX,
                        triangle: segment,
                    },
                    start,
                    end: model.terms.len(),
                    constant,
                    lower: -limit,
                    raw_lower: -raw_limit,
                    raw_limit,
                    limit,
                    norm,
                    correction: 0.,
                });
            }
        }
    }
    Ok(())
}
fn assess<F: Fn() -> bool>(
    input: &Input<'_>,
    settings: Settings,
    model: &Model,
    values: &[i16],
    meter: &mut Meter<'_, F>,
) -> Result<(usize, f64, usize, f64), Failure> {
    let mut violations = 0;
    let mut maximum = 0_f64;
    let mut clearance_violations = 0;
    let mut maximum_excess = f64::NEG_INFINITY;
    for collision in input.collisions {
        let mut chunk = collision.geometry.clone();
        for (point, binding) in chunk.mesh.vertices.iter_mut().zip(collision.heights) {
            meter.spend(64)?;
            let value = binding
                .resolve(|x, z| {
                    path_terrain::sample_height_with(layout(input.grid), [x, 0., z], |i| {
                        Some(model.read(input, values, i))
                    })
                    .ok()
                })
                .ok_or(Failure::Invalid("candidate binding"))?;
            point[1] = value - chunk.origin[1];
        }
        let lo = chunk
            .mesh
            .vertices
            .iter()
            .map(|p| p[1])
            .fold(f32::INFINITY, f32::min);
        let hi = chunk
            .mesh
            .vertices
            .iter()
            .map(|p| p[1])
            .fold(f32::NEG_INFINITY, f32::max);
        chunk.mesh.center[1] = (lo + hi) * 0.5;
        chunk.mesh.extents[1] = (hi - lo) * 0.5;
        let report = road_mesh_audit::audit(
            &chunk,
            collision.template,
            road_mesh_audit::Settings {
                maximum_grade: settings.maximum_grade,
                minimum_up_normal: settings.minimum_up_normal,
                maximum_work: meter.audit_allowance()?,
            },
            meter.cancel,
        )
        .map_err(audit_error)?;
        meter.spend(report.work)?;
        if report.inverted_world_triangles != 0 || report.degenerate_world_triangles != 0 {
            return Err(Failure::Invalid("candidate horizontal geometry"));
        }
        violations += report.longitudinal_violations;
        maximum = maximum.max(report.maximum_longitudinal_grade);
        for probe in model
            .probes
            .get(&(collision.road, collision.chunk_index))
            .into_iter()
            .flatten()
        {
            meter.spend(64)?;
            let terrain = path_terrain::sample_height_with(
                layout(input.grid),
                [probe.position[0], 0., probe.position[1]],
                |i| Some(model.read(input, values, i)),
            )
            .map_err(|_| Failure::Invalid("candidate clearance sample"))?;
            let road: f64 = probe
                .vertices
                .into_iter()
                .zip(probe.weights)
                .map(|(i, w)| f64::from(chunk.mesh.vertices[i][1] + chunk.origin[1]) * w)
                .sum();
            let excess = f64::from(terrain) - road;
            maximum_excess = maximum_excess.max(excess);
            clearance_violations += usize::from(excess > f64::from(probe.maximum_terrain_excess));
        }
    }
    if model.resample_controls {
        for road in input.controls {
            for pair in road.windows(2) {
                meter.spend(64)?;
                let run =
                    f64::from(pair[1][0] - pair[0][0]).hypot(f64::from(pair[1][2] - pair[0][2]));
                let mut heights = [0_f32; 2];
                for (height, p) in heights.iter_mut().zip(pair) {
                    *height = path_terrain::sample_height_with(layout(input.grid), *p, |i| {
                        Some(model.read(input, values, i))
                    })
                    .map_err(|_| Failure::Invalid("candidate control sampling"))?;
                }
                let grade = f64::from((heights[1] - heights[0]).abs()) / run;
                if !grade.is_finite() {
                    return Err(Failure::Invalid("candidate control run"));
                }
                violations += usize::from(grade > f64::from(settings.maximum_grade));
            }
        }
    }
    Ok((
        violations,
        maximum,
        clearance_violations,
        if maximum_excess.is_finite() {
            maximum_excess
        } else {
            0.
        },
    ))
}
/// Fits only a private height proposal. Failure/cancellation returns no partial
/// terrain. Every control support and authored protected grid vertex stays fixed.
pub fn fit(
    input: Input<'_>,
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Output, Failure> {
    fit_with_progress(input, settings, |_| {}, cancelled)
}
pub fn fit_with_progress(
    input: Input<'_>,
    settings: Settings,
    progress: impl FnMut(Progress),
    cancelled: impl Fn() -> bool,
) -> Result<Output, Failure> {
    fit_with_probes(input, settings, &[], progress, cancelled)
}
pub fn fit_with_probes(
    input: Input<'_>,
    settings: Settings,
    probes: &[ClearanceProbe],
    progress: impl FnMut(Progress),
    cancelled: impl Fn() -> bool,
) -> Result<Output, Failure> {
    fit_inner(
        input,
        settings,
        probes,
        ControlMode::Frozen.into(),
        None,
        progress,
        cancelled,
    )
}
/// Opt-in source-style control Y resampling. X/Z and coincident junctions stay
/// exact, but source paths and all meshes must be rebuilt from returned controls.
pub fn fit_resampling_controls(
    input: Input<'_>,
    settings: Settings,
    probes: &[ClearanceProbe],
    progress: impl FnMut(Progress),
    cancelled: impl Fn() -> bool,
) -> Result<Output, Failure> {
    fit_inner(
        input,
        settings,
        probes,
        ControlMode::Resample.into(),
        None,
        progress,
        cancelled,
    )
}
pub fn fit_in_paint_envelope(
    input: Input<'_>,
    settings: Settings,
    probes: &[ClearanceProbe],
    envelope: PaintEnvelope<'_>,
    progress: impl FnMut(Progress),
    cancelled: impl Fn() -> bool,
) -> Result<Output, Failure> {
    fit_inner(
        input,
        settings,
        probes,
        ControlMode::Resample.into(),
        Some(envelope),
        progress,
        cancelled,
    )
}
/// Fit sampled control grades before mesh generation. Authored/fixed bilinear
/// supports remain immutable. This never certifies collision-mesh grades.
pub fn fit_control_samples(
    input: Input<'_>,
    settings: Settings,
    fixed_samples: &[usize],
    cancelled: impl Fn() -> bool,
) -> Result<Output, Failure> {
    fit_inner(
        input,
        settings,
        &[],
        ControlMode::FixedSamples(fixed_samples).into(),
        None,
        |_| {},
        cancelled,
    )
}
/// Bounded nonlinear rebuild feedback. A non-converged inner fit is explicitly
/// provisional; strict fitting APIs continue to reject the same candidate.
pub fn fit_for_rebuild(
    input: Input<'_>,
    settings: Settings,
    probes: &[ClearanceProbe],
    envelope: PaintEnvelope<'_>,
    progress: impl FnMut(Progress),
    cancelled: impl Fn() -> bool,
) -> Result<RebuildCandidate, Failure> {
    let output = fit_inner(
        input,
        settings,
        probes,
        ControlMode::RebuildFeedback.into(),
        Some(envelope),
        progress,
        cancelled,
    )?;
    let mesh_fit_passed = output.report.exact_quantized_mesh_grades_validated;
    Ok(RebuildCandidate {
        height: output.height,
        controls: output.controls,
        report: output.report,
        mesh_fit_passed,
    })
}
#[derive(Clone, Copy)]
pub enum RebuildMode<'a> {
    Feedback,
    Quantized,
    FrozenControls,
    Local(&'a [usize]),
}
pub struct RebuildPass<'a> {
    pub mode: RebuildMode<'a>,
    pub envelope: PaintEnvelope<'a>,
}
/// Explicit fixed terrain dependencies remain hard constraints in every
/// rebuild mode, including provisional projection and integer polishing.
/// Binding their indices to the retained geometry/revision is the caller's job.
pub fn fit_retaining_for_rebuild(
    input: Input<'_>,
    settings: Settings,
    probes: &[ClearanceProbe],
    pass: RebuildPass<'_>,
    retained_samples: &[usize],
    progress: impl FnMut(Progress),
    cancelled: impl Fn() -> bool,
) -> Result<RebuildCandidate, Failure> {
    let control = match pass.mode {
        RebuildMode::Feedback => ControlMode::RebuildFeedback,
        RebuildMode::Quantized => ControlMode::QuantizedRebuild,
        RebuildMode::FrozenControls => ControlMode::RebuildFrozen,
        RebuildMode::Local(samples) => ControlMode::LocalRebuild(samples),
    };
    let output = fit_inner(
        input,
        settings,
        probes,
        FitMode {
            control,
            retained: retained_samples,
        },
        Some(pass.envelope),
        progress,
        cancelled,
    )?;
    Ok(RebuildCandidate {
        mesh_fit_passed: output.report.exact_quantized_mesh_grades_validated,
        height: output.height,
        controls: output.controls,
        report: output.report,
    })
}
/// Experimental bounded integer-coordinate polishing of a private proposal.
/// The ordinary fitter is unchanged. Exact source-height and mesh/probe/control
/// audits decide adoption; callers still need fresh source mesh generation.
pub fn fit_quantized_for_rebuild(
    input: Input<'_>,
    settings: Settings,
    probes: &[ClearanceProbe],
    envelope: PaintEnvelope<'_>,
    progress: impl FnMut(Progress),
    cancelled: impl Fn() -> bool,
) -> Result<RebuildCandidate, Failure> {
    let output = fit_inner(
        input,
        settings,
        probes,
        ControlMode::QuantizedRebuild.into(),
        Some(envelope),
        progress,
        cancelled,
    )?;
    Ok(RebuildCandidate {
        mesh_fit_passed: output.report.exact_quantized_mesh_grades_validated,
        height: output.height,
        controls: output.controls,
        report: output.report,
    })
}
/// Repair clearance without changing sampled controls or the path's arc length.
/// The caller must still rebuild and independently audit the resulting meshes.
pub fn fit_clearance_for_rebuild(
    input: Input<'_>,
    settings: Settings,
    probes: &[ClearanceProbe],
    envelope: PaintEnvelope<'_>,
    progress: impl FnMut(Progress),
    cancelled: impl Fn() -> bool,
) -> Result<RebuildCandidate, Failure> {
    let output = fit_inner(
        input,
        settings,
        probes,
        ControlMode::RebuildFrozen.into(),
        Some(envelope),
        progress,
        cancelled,
    )?;
    Ok(RebuildCandidate {
        mesh_fit_passed: output.report.exact_quantized_mesh_grades_validated,
        height: output.height,
        controls: output.controls,
        report: output.report,
    })
}
/// Limit a private rebuild proposal to explicitly selected terrain samples.
/// All supplied mesh/probe/control constraints and final grid guards still run.
#[allow(clippy::too_many_arguments)]
pub fn fit_local_for_rebuild(
    input: Input<'_>,
    settings: Settings,
    probes: &[ClearanceProbe],
    envelope: PaintEnvelope<'_>,
    allowed_samples: &[usize],
    progress: impl FnMut(Progress),
    cancelled: impl Fn() -> bool,
) -> Result<RebuildCandidate, Failure> {
    let output = fit_inner(
        input,
        settings,
        probes,
        ControlMode::LocalRebuild(allowed_samples).into(),
        Some(envelope),
        progress,
        cancelled,
    )?;
    Ok(RebuildCandidate {
        mesh_fit_passed: output.report.exact_quantized_mesh_grades_validated,
        height: output.height,
        controls: output.controls,
        report: output.report,
    })
}
pub struct RepairSamples {
    pub samples: Vec<usize>,
    pub violated_probes: usize,
    pub work: usize,
}
/// Select supports of currently failing witnesses and their mesh vertices.
/// This is a metered proposal region, not a clearance or mesh certificate.
pub fn select_clearance_samples(
    input: Input<'_>,
    probes: &[ClearanceProbe],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<RepairSamples, Failure> {
    let mut meter = Meter {
        work: 0,
        limit: maximum_work,
        cancel: &cancelled,
    };
    meter.spend(0)?;
    let n = input.grid.resolution;
    if !(2..=4097).contains(&n)
        || input.grid.height.len() != n * n
        || probes.len() > 131072
        || input.collisions.len() > 8192
        || !(1..=2_000_000_000).contains(&maximum_work)
    {
        return Err(Failure::Invalid("local selection bounds"));
    }
    let mut collisions = BTreeMap::new();
    for collision in input.collisions {
        meter.spend(4)?;
        if collisions
            .insert((collision.road, collision.chunk_index), collision)
            .is_some()
        {
            return Err(Failure::Invalid("local collision coverage"));
        }
    }
    let mut selected = BTreeSet::new();
    let mut violated = 0;
    for probe in probes {
        meter.spend(512)?;
        if probe
            .position
            .iter()
            .any(|v| !v.is_finite() || v.abs() > f64::from(input.grid.size) * 0.5)
            || !probe.maximum_terrain_excess.is_finite()
            || !(-1. ..=1.).contains(&probe.maximum_terrain_excess)
        {
            return Err(Failure::Invalid("local probe"));
        }
        let collision = collisions
            .get(&(probe.road, probe.chunk))
            .ok_or(Failure::Invalid("local collision"))?;
        let face = collision
            .geometry
            .mesh
            .triangles
            .get(probe.triangle)
            .ok_or(Failure::Invalid("local triangle"))?;
        let mut world = [[0_f64; 3]; 3];
        for (point, &i) in world.iter_mut().zip(face) {
            let vertex = collision
                .geometry
                .mesh
                .vertices
                .get(i as usize)
                .ok_or(Failure::Invalid("local vertex"))?;
            *point = std::array::from_fn(|k| f64::from(vertex[k] + collision.geometry.origin[k]));
        }
        let [a, b, c] = world;
        let ab = [b[0] - a[0], b[2] - a[2]];
        let ac = [c[0] - a[0], c[2] - a[2]];
        let det = ab[0] * ac[1] - ab[1] * ac[0];
        if !det.is_finite() || det.abs() < 1e-10 {
            return Err(Failure::Invalid("local projected triangle"));
        }
        let p = [probe.position[0] - a[0], probe.position[1] - a[2]];
        let wb = (p[0] * ac[1] - p[1] * ac[0]) / det;
        let wc = (ab[0] * p[1] - ab[1] * p[0]) / det;
        if [1. - wb - wc, wb, wc]
            .iter()
            .any(|v| !v.is_finite() || *v < -1e-6 || *v > 1. + 1e-6)
        {
            return Err(Failure::Invalid("local witness outside triangle"));
        }
        let road = a[1] * (1. - wb - wc) + b[1] * wb + c[1] * wc;
        let terrain = path_terrain::sample_height(
            input.grid,
            [probe.position[0] as f32, 0., probe.position[1] as f32],
        )
        .map_err(|_| Failure::Invalid("local terrain sample"))?;
        if f64::from(terrain) - road <= f64::from(probe.maximum_terrain_excess) {
            continue;
        }
        violated += 1;
        let mut include = |position: [f32; 2]| -> Result<(), Failure> {
            meter.spend(8)?;
            selected.extend(
                path_terrain::height_support(layout(input.grid), [position[0], 0., position[1]])
                    .map_err(|_| Failure::Invalid("local support"))?
                    .indices,
            );
            if selected.len() > MAX_VARIABLES {
                return Err(Failure::Budget);
            }
            Ok(())
        };
        include(probe.position.map(|v| v as f32))?;
        for &i in face {
            match *collision
                .heights
                .get(i as usize)
                .ok_or(Failure::Invalid("local height binding"))?
            {
                HeightBinding::Endpoint { position } => include(position)?,
                HeightBinding::Span { left, right, .. } => {
                    include(left)?;
                    include(right)?;
                }
            }
        }
    }
    Ok(RepairSamples {
        samples: selected.into_iter().collect(),
        violated_probes: violated,
        work: meter.work,
    })
}
/// Select failing clearance supports plus complete failing mesh-chunk supports.
/// Keeping a whole chunk lets neighboring driving faces share a smooth repair;
/// all constraints outside this proposal region still run in the local fitter.
/// This does not loosen topology, paint, earthwork, control or final mesh gates.
pub fn select_mesh_repair_samples(
    input: Input<'_>,
    settings: Settings,
    probes: &[ClearanceProbe],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<RepairSamples, Failure> {
    settings.validate()?;
    let clearance = select_clearance_samples(input, probes, maximum_work, &cancelled)?;
    let mut meter = Meter {
        work: clearance.work,
        limit: maximum_work,
        cancel: &cancelled,
    };
    let mut selected: BTreeSet<_> = clearance.samples.into_iter().collect();
    for collision in input.collisions {
        if collision.heights.len() != collision.geometry.mesh.vertices.len() {
            return Err(Failure::Invalid("local mesh trace coverage"));
        }
        let report = road_mesh_audit::audit(
            collision.geometry,
            collision.template,
            road_mesh_audit::Settings {
                maximum_grade: settings.maximum_grade,
                minimum_up_normal: settings.minimum_up_normal,
                maximum_work: meter.audit_allowance()?,
            },
            &cancelled,
        )
        .map_err(audit_error)?;
        meter.spend(report.work)?;
        if report.degenerate_world_triangles != 0 || report.inverted_world_triangles != 0 {
            return Err(Failure::Invalid("local mesh horizontal geometry"));
        }
        if report.longitudinal_violations == 0 {
            continue;
        }
        for binding in collision.heights {
            let mut include = |position: [f32; 2]| -> Result<(), Failure> {
                meter.spend(8)?;
                selected.extend(
                    path_terrain::height_support(
                        layout(input.grid),
                        [position[0], 0., position[1]],
                    )
                    .map_err(|_| Failure::Invalid("local mesh support"))?
                    .indices,
                );
                if selected.len() > MAX_VARIABLES {
                    return Err(Failure::Budget);
                }
                Ok(())
            };
            match *binding {
                HeightBinding::Endpoint { position } => include(position)?,
                HeightBinding::Span { left, right, .. } => {
                    include(left)?;
                    include(right)?;
                }
            }
        }
    }
    Ok(RepairSamples {
        samples: selected.into_iter().collect(),
        violated_probes: clearance.violated_probes,
        work: meter.work,
    })
}
#[derive(Clone, Copy)]
enum ControlMode<'a> {
    Frozen,
    Resample,
    FixedSamples(&'a [usize]),
    RebuildFeedback,
    QuantizedRebuild,
    RebuildFrozen,
    LocalRebuild(&'a [usize]),
}
struct FitMode<'a> {
    control: ControlMode<'a>,
    retained: &'a [usize],
}
impl<'a> From<ControlMode<'a>> for FitMode<'a> {
    fn from(control: ControlMode<'a>) -> Self {
        Self {
            control,
            retained: &[],
        }
    }
}
fn fit_inner(
    input: Input<'_>,
    settings: Settings,
    probes: &[ClearanceProbe],
    mode: FitMode<'_>,
    envelope: Option<PaintEnvelope<'_>>,
    mut progress: impl FnMut(Progress),
    cancelled: impl Fn() -> bool,
) -> Result<Output, Failure> {
    let control_mode = mode.control;
    let retained_samples = mode.retained;
    let freeze_controls = matches!(
        control_mode,
        ControlMode::Frozen | ControlMode::RebuildFrozen
    );
    let allow_provisional = matches!(
        control_mode,
        ControlMode::RebuildFeedback
            | ControlMode::QuantizedRebuild
            | ControlMode::RebuildFrozen
            | ControlMode::LocalRebuild(_)
    );
    let fixed_samples = match control_mode {
        ControlMode::FixedSamples(v) => Some(v),
        _ => None,
    };
    let control_only = fixed_samples.is_some();
    let local_samples = match control_mode {
        ControlMode::LocalRebuild(v) => Some(v),
        _ => None,
    };
    let mut meter = Meter {
        work: 0,
        limit: settings.maximum_work,
        cancel: &cancelled,
    };
    meter.spend(0)?;
    settings.validate()?;
    let n = input.grid.resolution;
    let sn = input.surface_resolution;
    if !(2..=4097).contains(&n)
        || input.grid.height.len() != n * n
        || input.ground.len() != n * n
        || !(2..=2048).contains(&sn)
        || input.topology.len() != sn * sn
        || input.controls.len() > 256
        || input.controls.iter().map(Vec::len).sum::<usize>() > 131072
        || (input.collisions.is_empty() && !control_only)
        || (control_only
            && (input.controls.is_empty() || !input.collisions.is_empty() || !probes.is_empty()))
        || fixed_samples.is_some_and(|v| v.len() > 524288 || v.iter().any(|&i| i >= n * n))
        || retained_samples.len() > 262144
        || retained_samples.iter().any(|&i| i >= n * n)
        || local_samples.is_some_and(|v| {
            v.is_empty() || v.len() > MAX_VARIABLES || v.iter().any(|&i| i >= n * n)
        })
        || input.collisions.len() > 8192
        || probes.len() > 131072
        || envelope.is_some_and(|e| {
            e.source_painted.len() != n * n
                || e.source_rounding.is_some_and(|r| r.cells() != n * n)
                || e.allowed_topology <= 0
                || e.allowed_topology & !input.protected_topology != 0
        })
    {
        return Err(Failure::Invalid("settings or buffers"));
    }
    path_terrain::height_support(layout(input.grid), [0.; 3])
        .map_err(|_| Failure::Invalid("height layout"))?;
    let allowed_samples = local_samples
        .map(|samples| {
            let mut allowed = BTreeSet::new();
            for &i in samples {
                meter.spend(8)?;
                allowed.insert(i);
            }
            Ok::<_, Failure>(allowed)
        })
        .transpose()?;
    let quantum = path_terrain::SHORT_TO_FLOAT * input.grid.vertical_span;
    let allowance = (f64::from(settings.maximum_cut_fill) / f64::from(quantum)).floor() as i32;
    let mut protected_count = 0;
    let mut protected_envelope_count = 0;
    for i in 0..n * n {
        if i % 4096 == 0 {
            meter.spend((n * n - i).min(4096))?;
        }
        if !(0..=32766).contains(&input.grid.height[i]) || !(0..=32766).contains(&input.ground[i]) {
            return Err(Failure::Invalid("height encoding"));
        }
        if (i32::from(input.grid.height[i]) - i32::from(input.ground[i])).abs() > allowance {
            return Err(Failure::Invalid("initial earthwork"));
        }
        protected_count += usize::from(protected(&input, i));
        if let Some(e) = envelope {
            if !(0..=32766).contains(&e.source_painted[i]) {
                return Err(Failure::Invalid("source paint encoding"));
            }
            if let Some((minimum, maximum)) = envelope_bounds(&input, envelope, i) {
                protected_envelope_count += 1;
                if !(minimum..=maximum).contains(&i32::from(input.grid.height[i])) {
                    return Err(Failure::Invalid("initial protected paint envelope"));
                }
            }
        }
    }
    let mut frozen = BTreeSet::new();
    for &i in retained_samples {
        meter.spend(8)?;
        frozen.insert(i);
    }
    if let Some(samples) = fixed_samples {
        for &i in samples {
            meter.spend(8)?;
            frozen.insert(i);
        }
    }
    let mut controls = 0;
    for road in input.controls {
        if !(2..=65536).contains(&road.len()) {
            return Err(Failure::Invalid("controls"));
        }
        for &p in road {
            meter.spend(8)?;
            if path_terrain::sample_height(input.grid, p)
                .map_err(|_| Failure::Invalid("control sampling"))?
                .to_bits()
                != p[1].to_bits()
            {
                return Err(Failure::Invalid("foreign controls"));
            }
            if freeze_controls {
                frozen.extend(
                    path_terrain::height_support(layout(input.grid), p)
                        .map_err(|_| Failure::Invalid("control support"))?
                        .pinned_indices(),
                );
            }
            controls += 1;
        }
    }
    for junction in input.junctions {
        meter.spend(1)?;
        let get = |(r, p): (usize, usize)| input.controls.get(r).and_then(|r| r.get(p));
        let (Some(a), Some(b)) = (get(junction.a), get(junction.b)) else {
            return Err(Failure::Invalid("junction"));
        };
        if a.iter().zip(b).any(|(a, b)| a.to_bits() != b.to_bits()) {
            return Err(Failure::Invalid("junction mismatch"));
        }
    }
    let mut model = Model {
        variables: Vec::new(),
        indices: HashMap::new(),
        rows: Vec::new(),
        terms: Vec::new(),
        probes: BTreeMap::new(),
        resample_controls: !freeze_controls,
        envelope,
        allowed_samples: allowed_samples.as_ref(),
        headroom_clipped: 0,
    };
    let mut before = 0;
    let mut maximum_before = 0_f64;
    let mut seen = BTreeSet::new();
    for collision in input.collisions {
        if collision.road >= input.controls.len()
            || !seen.insert((collision.road, collision.chunk_index))
            || collision.heights.len() != collision.geometry.mesh.vertices.len()
        {
            return Err(Failure::Invalid("collision coverage"));
        }
        let report = road_mesh_audit::audit(
            collision.geometry,
            collision.template,
            road_mesh_audit::Settings {
                maximum_grade: settings.maximum_grade,
                minimum_up_normal: settings.minimum_up_normal,
                maximum_work: meter.audit_allowance()?,
            },
            &cancelled,
        )
        .map_err(audit_error)?;
        meter.spend(report.work)?;
        if report.driving_triangles == 0
            || report.inverted_world_triangles != 0
            || report.degenerate_world_triangles != 0
        {
            if let Some(triangle) = report.first_invalid_triangle {
                return Err(Failure::HorizontalGeometry {
                    road: collision.road,
                    chunk: collision.chunk_index,
                    triangle,
                    inverted: report.inverted_world_triangles,
                    degenerate: report.degenerate_world_triangles,
                });
            }
            return Err(Failure::Invalid("horizontal geometry"));
        }
        before += report.longitudinal_violations;
        maximum_before = maximum_before.max(report.maximum_longitudinal_grade);
        let mut vertices = Vec::with_capacity(collision.heights.len());
        for (binding, &p) in collision
            .heights
            .iter()
            .zip(&collision.geometry.mesh.vertices)
        {
            meter.spend(64)?;
            vertices.push(model.vertex(
                &input,
                binding,
                p,
                collision.geometry.origin[1],
                &frozen,
                allowance,
            )?);
        }
        for (triangle, face) in collision.geometry.mesh.triangles.iter().enumerate() {
            meter.spend(64)?;
            let authored = collision.template.triangles
                [triangle % collision.template.triangles.len()]
            .map(|i| i as usize);
            let [a, b, c] = authored.map(|i| collision.template.vertices[i].map(f64::from));
            let ab = [b[0] - a[0], b[2] - a[2]];
            let ac = [c[0] - a[0], c[2] - a[2]];
            let det = ab[0] * ac[1] - ac[0] * ab[1];
            if det.abs() < 1e-10
                || authored
                    .iter()
                    .any(|&i| collision.template.normals[i][1] < settings.minimum_up_normal)
            {
                continue;
            }
            let ids = face.map(|i| i as usize);
            let [p, q, r] = ids.map(|i| collision.geometry.mesh.vertices[i].map(f64::from));
            let along: [f64; 3] =
                std::array::from_fn(|i| ((r[i] - p[i]) * ab[0] - (q[i] - p[i]) * ac[0]) / det);
            let raw_limit = f64::from(settings.maximum_grade) * along[0].hypot(along[2]);
            let coefficients = [(ac[0] - ab[0]) / det, -ac[0] / det, ab[0] / det];
            let mut terms = BTreeMap::<usize, f64>::new();
            for (id, scale) in ids.into_iter().zip(coefficients) {
                for &(index, value) in &vertices[id] {
                    *terms.entry(index).or_default() += scale * value;
                }
            }
            terms.retain(|_, v| v.abs() > 1e-14);
            let constant = along[1]
                - terms
                    .iter()
                    .map(|(&i, &c)| model.variables[i].value * c)
                    .sum::<f64>();
            let norm = terms.values().map(|v| v * v).sum::<f64>();
            let margin = if norm == 0. {
                0.
            } else {
                settings.quantization_reserve * terms.values().map(|v| v.abs()).sum::<f64>()
                    + roundoff_margin(
                        settings,
                        input.grid.vertical_span,
                        coefficients.iter().map(|v| v.abs()).sum::<f64>(),
                    )
            };
            let mut minimum = constant;
            let mut maximum = constant;
            for (&i, &c) in &terms {
                let v = &model.variables[i];
                minimum += c * if c >= 0. { v.minimum } else { v.maximum };
                maximum += c * if c >= 0. { v.maximum } else { v.minimum };
            }
            if minimum > raw_limit + 1e-9 || maximum < -raw_limit - 1e-9 {
                return Err(Failure::Infeasible {
                    road: collision.road,
                    chunk: collision.chunk_index,
                    triangle,
                    minimum,
                    maximum,
                    limit: raw_limit,
                });
            }
            let (limit, clipped) = bounded_headroom(
                (raw_limit - margin).max(0.),
                raw_limit,
                minimum.max(-maximum).max(0.),
            );
            model.headroom_clipped += usize::from(clipped);
            if norm == 0. {
                continue;
            }
            if model.rows.len() == MAX_ROWS || model.terms.len() + terms.len() > MAX_TERMS {
                return Err(Failure::Budget);
            }
            let start = model.terms.len();
            model.terms.extend(terms);
            model.rows.push(Row {
                face: Face {
                    road: collision.road,
                    chunk: collision.chunk_index,
                    triangle,
                },
                start,
                end: model.terms.len(),
                constant,
                lower: -limit,
                raw_lower: -raw_limit,
                raw_limit,
                limit,
                norm,
                correction: 0.,
            });
        }
    }
    add_clearance_probes(
        &mut model, &input, settings, probes, &frozen, allowance, &mut meter,
    )?;
    if !freeze_controls {
        add_control_grade_rows(&mut model, &input, settings, &frozen, allowance, &mut meter)?;
    }
    let original_values: Vec<_> = model
        .variables
        .iter()
        .map(|v| input.grid.height[v.grid])
        .collect();
    let initial_clearance = if probes.is_empty() {
        (before, maximum_before, 0, 0.)
    } else {
        assess(&input, settings, &model, &original_values, &mut meter)?
    };
    let mut last = initial_clearance;
    let mut accepted = None;
    let mut best: Option<QuantizedState> = None;
    progress(Progress {
        phase: "model",
        iteration: 0,
        variables: model.variables.len(),
        constraints: model.rows.len(),
        headroom_clipped_rows: model.headroom_clipped,
        work: meter.work,
        linear_maximum_excess: 0.,
        last_exact_violations: last.0,
        last_exact_maximum_grade: last.1,
        last_exact_clearance_violations: last.2,
        last_exact_maximum_terrain_excess: last.3,
        worst: None,
    });
    let full_work_limit = meter.limit;
    let polish_allowance = if matches!(control_mode, ControlMode::QuantizedRebuild) {
        full_work_limit / 4
    } else {
        0
    };
    if allow_provisional {
        let reserve = n
            .checked_mul(n)
            .and_then(|v| v.checked_add(controls * 8))
            .and_then(|v| v.checked_add(input.junctions.len() + 1))
            .ok_or(Failure::Budget)?;
        meter.limit = full_work_limit
            .checked_sub(reserve)
            .and_then(|v| v.checked_sub(polish_allowance))
            .filter(|&v| v >= meter.work)
            .ok_or(Failure::Budget)?;
    }
    let mut projection_work_limit_reached = false;
    let mut completed_iterations = 0;
    for iteration in 1..=settings.maximum_iterations {
        let step: Result<bool, Failure> = (|| {
            for row in &mut model.rows {
                meter.spend((row.end - row.start) * 3 + 1)?;
                let mut value = row.constant;
                for &(i, c) in &model.terms[row.start..row.end] {
                    model.variables[i].value += row.correction * c;
                    value += model.variables[i].value * c;
                }
                let delta = (value - value.clamp(row.lower, row.limit)) / row.norm;
                if !delta.is_finite() {
                    return Err(Failure::Invalid("projection arithmetic"));
                }
                for &(i, c) in &model.terms[row.start..row.end] {
                    model.variables[i].value -= delta * c;
                }
                row.correction = delta;
            }
            for v in &mut model.variables {
                meter.spend(1)?;
                let old = v.value + v.box_correction;
                if !old.is_finite() {
                    return Err(Failure::Invalid("box arithmetic"));
                }
                v.value = old.clamp(v.minimum, v.maximum);
                v.box_correction = old - v.value;
            }
            let mut excess = 0_f64;
            let mut worst = None;
            for row in &model.rows {
                meter.spend(row.end - row.start + 1)?;
                let value = row.constant
                    + model.terms[row.start..row.end]
                        .iter()
                        .map(|&(i, c)| model.variables[i].value * c)
                        .sum::<f64>();
                let violation = (value - row.limit).max(row.lower - value);
                if violation > excess {
                    excess = violation;
                    worst = Some(row.face);
                }
            }
            if iteration == 1
                || iteration % 8 == 0
                || excess <= 1e-7
                || iteration == settings.maximum_iterations
            {
                progress(Progress {
                    phase: "auditing",
                    iteration,
                    variables: model.variables.len(),
                    constraints: model.rows.len(),
                    headroom_clipped_rows: model.headroom_clipped,
                    work: meter.work,
                    linear_maximum_excess: excess,
                    last_exact_violations: last.0,
                    last_exact_maximum_grade: last.1,
                    last_exact_clearance_violations: last.2,
                    last_exact_maximum_terrain_excess: last.3,
                    worst,
                });
                let values: Vec<_> = model
                    .variables
                    .iter()
                    .map(|v| v.value.round().clamp(v.minimum, v.maximum) as i16)
                    .collect();
                last = assess(&input, settings, &model, &values, &mut meter)?;
                progress(Progress {
                    phase: "projecting",
                    iteration,
                    variables: model.variables.len(),
                    constraints: model.rows.len(),
                    headroom_clipped_rows: model.headroom_clipped,
                    work: meter.work,
                    linear_maximum_excess: excess,
                    last_exact_violations: last.0,
                    last_exact_maximum_grade: last.1,
                    last_exact_clearance_violations: last.2,
                    last_exact_maximum_terrain_excess: last.3,
                    worst,
                });
                if last.0 == 0 && last.2 == 0 {
                    accepted = Some((iteration, values));
                    return Ok(true);
                }
                if allow_provisional
                    && best.as_ref().is_none_or(|b| {
                        (last.0 + last.2)
                            .cmp(&(b.assessment.0 + b.assessment.2))
                            .then_with(|| last.1.total_cmp(&b.assessment.1))
                            .then_with(|| last.3.total_cmp(&b.assessment.3))
                            .is_lt()
                    })
                {
                    best = Some(QuantizedState {
                        values,
                        assessment: last,
                    });
                }
            }
            Ok(false)
        })();
        match step {
            Ok(done) => {
                completed_iterations = iteration;
                if done {
                    break;
                }
            }
            Err(Failure::Budget) if allow_provisional && best.is_some() => {
                projection_work_limit_reached = true;
                break;
            }
            Err(error) => return Err(error),
        }
    }
    meter.limit = full_work_limit;
    let mut model_passed = true;
    let (iterations, mut values) = if let Some(value) = accepted {
        value
    } else if let Some(candidate) = best.filter(|_| allow_provisional) {
        model_passed = false;
        last = candidate.assessment;
        (completed_iterations, candidate.values)
    } else {
        return Err(Failure::NoConvergence {
            iterations: settings.maximum_iterations,
            remaining: last.0,
            maximum_grade: last.1,
            remaining_clearance: last.2,
            maximum_terrain_excess: last.3,
        });
    };
    let mut quantized_polish = None;
    if polish_allowance > 0 && !model_passed {
        let mut candidate = values.clone();
        let mut polish_meter = Meter {
            work: 0,
            limit: polish_allowance,
            cancel: &cancelled,
        };
        let result = polish_values(&model, &mut candidate, &mut polish_meter).and_then(|counts| {
            assess(&input, settings, &model, &candidate, &mut polish_meter).map(|a| (counts, a))
        });
        let mut detail = PolishReport {
            passes: 0,
            moves: 0,
            work: polish_meter.work,
            adopted: false,
            work_limit_reached: false,
        };
        match result {
            Ok(((passes, moves), assessment)) => {
                detail.passes = passes;
                detail.moves = moves;
                let better = (assessment.0 + assessment.2)
                    .cmp(&(last.0 + last.2))
                    .then_with(|| assessment.1.total_cmp(&last.1))
                    .then_with(|| assessment.3.total_cmp(&last.3))
                    .is_lt();
                if better {
                    values = candidate;
                    last = assessment;
                    model_passed = last.0 == 0 && last.2 == 0;
                    detail.adopted = true;
                }
            }
            Err(Failure::Budget) => {
                detail.work = polish_allowance;
                detail.work_limit_reached = true;
            }
            Err(error) => return Err(error),
        }
        meter.spend(detail.work)?;
        quantized_polish = Some(detail);
    }
    let mut height = input.grid.height.to_vec();
    let mut changed = 0;
    let mut repair = 0_f32;
    for (v, value) in model.variables.iter().zip(values) {
        if height[v.grid] != value {
            changed += 1;
            repair =
                repair.max((i32::from(value) - i32::from(height[v.grid])).abs() as f32 * quantum);
            height[v.grid] = value;
        }
    }
    let mut raised = 0_f32;
    let mut lowered = 0_f32;
    let mut protected_changed = 0;
    let mut protected_repair = 0_f32;
    for (i, &value) in height.iter().enumerate() {
        if i % 4096 == 0 {
            meter.spend((n * n - i).min(4096))?;
        }
        let delta = i32::from(value) - i32::from(input.ground[i]);
        if delta.abs() > allowance || (frozen.contains(&i) && value != input.grid.height[i]) {
            return Err(Failure::Invalid("final terrain invariants"));
        }
        if value != input.grid.height[i]
            && allowed_samples
                .as_ref()
                .is_some_and(|allowed| !allowed.contains(&i))
        {
            return Err(Failure::Invalid("repair outside selected samples"));
        }
        if protected(&input, i) {
            if let Some((minimum, maximum)) = envelope_bounds(&input, envelope, i) {
                if !(minimum..=maximum).contains(&i32::from(value)) {
                    return Err(Failure::Invalid("final protected paint envelope"));
                }
            } else if value != input.grid.height[i] {
                return Err(Failure::Invalid("final protected sample"));
            }
            if value != input.grid.height[i] {
                protected_changed += 1;
                protected_repair = protected_repair.max(
                    (i32::from(value) - i32::from(input.grid.height[i])).abs() as f32 * quantum,
                );
            }
        }
        raised = raised.max(delta as f32 * quantum);
        lowered = lowered.max(-delta as f32 * quantum);
    }
    let mut output_controls = input.controls.to_vec();
    let mut resampled = 0;
    let mut maximum_control_grade = 0_f64;
    for road in &mut output_controls {
        for p in road.iter_mut() {
            meter.spend(8)?;
            let value = path_terrain::sample_height(
                Grid {
                    height: &height,
                    ..input.grid
                },
                *p,
            )
            .map_err(|_| Failure::Invalid("final control"))?;
            if freeze_controls && value.to_bits() != p[1].to_bits() {
                return Err(Failure::Invalid("changed control"));
            }
            resampled += usize::from(value.to_bits() != p[1].to_bits());
            p[1] = value;
        }
        for pair in road.windows(2) {
            let run = f64::from(pair[1][0] - pair[0][0]).hypot(f64::from(pair[1][2] - pair[0][2]));
            let grade = f64::from((pair[1][1] - pair[0][1]).abs()) / run;
            maximum_control_grade = maximum_control_grade.max(grade);
            if !freeze_controls
                && (!grade.is_finite()
                    || (model_passed && grade > f64::from(settings.maximum_grade)))
            {
                return Err(Failure::Invalid("final control grade"));
            }
        }
    }
    for junction in input.junctions {
        let a = output_controls[junction.a.0][junction.a.1];
        let b = output_controls[junction.b.0][junction.b.1];
        if a.iter().zip(b).any(|(a, b)| a.to_bits() != b.to_bits()) {
            return Err(Failure::Invalid("final junction"));
        }
    }
    if cancelled() {
        return Err(Failure::Cancelled);
    }
    Ok(Output {
        height,
        controls: output_controls,
        report: Report {
            retained_sample_entries: (!retained_samples.is_empty())
                .then_some(retained_samples.len()),
            engine: if matches!(control_mode, ControlMode::QuantizedRebuild) {
                QUANTIZED_REBUILD_VERSION
            } else if local_samples.is_some() {
                LOCAL_REBUILD_VERSION
            } else if allow_provisional {
                REBUILD_VERSION
            } else if control_only {
                CONTROLS_VERSION
            } else if envelope.is_some() {
                ENVELOPE_VERSION
            } else if probes.is_empty() && freeze_controls {
                VERSION
            } else {
                COUPLED_VERSION
            },
            settings,
            variables: model.variables.len(),
            repair_sample_limit: allowed_samples.as_ref().map(BTreeSet::len),
            constraints: model.rows.len(),
            terms: model.terms.len(),
            headroom_clipped_rows: model.headroom_clipped,
            iterations,
            projection_work_limit_reached,
            quantized_polish,
            changed_samples: changed,
            maximum_raise: raised,
            maximum_lower: lowered,
            maximum_repair: repair,
            maximum_mesh_grade_before: maximum_before,
            maximum_mesh_grade_after: last.1,
            violations_before: before,
            violations_after: last.0,
            clearance_probes_checked: probes.len(),
            clearance_probe_violations_before: initial_clearance.2,
            clearance_probe_violations_after: last.2,
            maximum_probe_terrain_excess_before: initial_clearance.3,
            maximum_probe_terrain_excess_after: last.3,
            control_supports_frozen: freeze_controls,
            control_heights_resampled: resampled,
            maximum_control_grade,
            control_horizontal_and_junctions_preserved: true,
            source_paths_must_be_rebuilt: !freeze_controls,
            controls_checked: controls,
            junctions_checked: input.junctions.len(),
            protected_samples_checked: protected_count,
            protected_envelope_samples: protected_envelope_count,
            protected_samples_changed: protected_changed,
            maximum_protected_repair: protected_repair,
            protected_paint_envelope_validated: envelope.is_some(),
            work: meter.work,
            exact_quantized_mesh_grades_validated: !control_only && model_passed,
            controls_and_junctions_unchanged: resampled == 0,
            protected_samples_unchanged: protected_changed == 0,
            full_grid_earthwork_validated: true,
            terrain_surface_clearance_validated: false,
            water_clearance_validated: false,
            runtime_access_validated: false,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn settings() -> Settings {
        Settings {
            maximum_grade: 0.2,
            maximum_cut_fill: 8.,
            minimum_up_normal: 0.5,
            maximum_iterations: 128,
            maximum_work: 200_000,
            quantization_reserve: 0.51,
            reserve_roundoff: true,
        }
    }
    type Fixture = (
        Vec<i16>,
        Vec<i32>,
        Vec<Vec<[f32; 3]>>,
        Template,
        Chunk,
        Vec<HeightBinding>,
    );
    fn fixture() -> Fixture {
        let n = 33;
        let mut height = Vec::new();
        for z in 0..n {
            for _ in 0..n {
                height.push(crate::erosion::encode(
                    20. + 0.3 * (z as f32 / (n - 1) as f32 * 1000. - 500.),
                ));
            }
        }
        let grid = Grid {
            size: 1000.,
            resolution: n,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &height,
        };
        let point = |x, z| [x, path_terrain::sample_height(grid, [x, 0., z]).unwrap(), z];
        let template = Template {
            vertices: vec![[0., 0., 0.], [10., 0., 0.], [0., 0., 10.]],
            normals: vec![[0., 1., 0.]; 3],
            tangents: vec![[1., 0., 0., 1.]; 3],
            uv: vec![[0., 0.]; 3],
            triangles: vec![[0, 1, 2]],
            center: [5., 0., 5.],
            extents: [5., 0., 5.],
        };
        let points = vec![point(-5., 0.), point(5., 0.), point(-5., 20.)];
        let low = points.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min);
        let high = points
            .iter()
            .map(|p| p[1])
            .fold(f32::NEG_INFINITY, f32::max);
        let mut mesh = template.clone();
        mesh.vertices = points;
        mesh.center = [0., (low + high) * 0.5, 10.];
        mesh.extents = [5., (high - low) * 0.5, 10.];
        let controls = vec![vec![point(-500., -500.), point(-468.75, -500.)]];
        let chunk = Chunk {
            first_repetition: 0,
            repetitions: 1,
            origin: [0.; 3],
            mesh,
            work: 4,
        };
        let bindings = chunk
            .mesh
            .vertices
            .iter()
            .map(|p| HeightBinding::Endpoint {
                position: [p[0], p[2]],
            })
            .collect();
        (height, vec![0; n * n], controls, template, chunk, bindings)
    }
    #[test]
    fn source_cut_rounding_repairs_only_covered_cells_and_cannot_override_retention() {
        let (mut height, _, mut controls, template, mut chunk, _) = fixture();
        height.fill(10000);
        let level = -500. + 10000. * path_terrain::SHORT_TO_FLOAT * 1000.;
        for p in controls.iter_mut().flatten() {
            p[1] = level;
        }
        chunk.mesh.vertices = vec![
            [-62.5, level, -62.5],
            [62.5, level, -62.5],
            [0., level, 62.5],
        ];
        chunk.mesh.center = [0., level, 0.];
        chunk.mesh.extents = [62.5, 0., 62.5];
        let bindings = chunk
            .mesh
            .vertices
            .iter()
            .map(|p| HeightBinding::Endpoint {
                position: [p[0], p[2]],
            })
            .collect::<Vec<_>>();
        let cell = 16 * 33 + 16;
        height[cell] += 1;
        let topology = vec![1024; height.len()];
        let collisions = [Collision {
            road: 0,
            chunk_index: 0,
            geometry: &chunk,
            template: &template,
            heights: &bindings,
        }];
        let input = Input {
            grid: Grid {
                size: 1000.,
                resolution: 33,
                origin_y: -500.,
                vertical_span: 1000.,
                height: &height,
            },
            ground: &height,
            topology: &topology,
            surface_resolution: 33,
            protected_topology: 1024,
            controls: &controls,
            junctions: &[],
            collisions: &collisions,
        };
        let mut rounding = crate::road_terrain::SourceCutRounding::new(height.len());
        let before = height[cell];
        rounding.record(
            false,
            path_terrain::CellWrite {
                cell,
                opacity: 0.05,
                before,
                target_normalized: (before as f32 - 0.25) * path_terrain::SHORT_TO_FLOAT,
                raw_encoded: before as f32 - 0.25,
                after: before,
            },
        );
        let envelope = PaintEnvelope {
            source_painted: &height,
            source_rounding: Some(&rounding),
            allowed_topology: 1024,
        };
        assert_eq!(
            envelope.protected_bounds(before, 1024, cell),
            Some((i32::from(before) - 1, i32::from(before)))
        );
        assert_eq!(envelope.protected_bounds(before, 1024 | 2048, cell), None);
        assert_eq!(envelope.protected_bounds(height[0], 1024, 0), None);
        let probes = [ClearanceProbe {
            road: 0,
            chunk: 0,
            triangle: 0,
            position: [0., 0.],
            maximum_terrain_excess: 0.01,
        }];
        let settings = Settings {
            maximum_iterations: 512,
            maximum_work: 2_000_000,
            ..settings()
        };
        let run = |envelope, retained: &[usize]| {
            fit_retaining_for_rebuild(
                input,
                settings,
                &probes,
                RebuildPass {
                    mode: RebuildMode::Quantized,
                    envelope,
                },
                retained,
                |_| {},
                || false,
            )
        };
        assert!(
            run(
                PaintEnvelope {
                    source_rounding: None,
                    ..envelope
                },
                &[]
            )
            .is_err()
        );
        let output = run(envelope, &[]).unwrap();
        assert_eq!(output.height[cell], before - 1);
        assert!(output.mesh_fit_passed && output.report.protected_paint_envelope_validated);
        assert!(
            output
                .height
                .iter()
                .zip(&height)
                .enumerate()
                .all(|(i, (a, b))| i == cell || a == b)
        );
        assert!(
            run(envelope, &[cell]).is_err(),
            "retained query supports stay immutable"
        );
        assert_eq!(height[cell], before);
    }
    #[test]
    fn integer_polish_crosses_a_coupled_barrier_without_changing_fixed_variables() {
        // x+y=2 and 2x-2y=0, starting at x=y=0. A single +1 move
        // reduces the first squared residual by 3 but adds 4 to the second;
        // the simultaneous +1,+1 move satisfies both constraints.
        let mut model = Model {
            variables: (0..3)
                .map(|grid| Variable {
                    grid,
                    value: 0.,
                    minimum: 0.,
                    maximum: if grid == 2 { 0. } else { 1. },
                    box_correction: 0.,
                })
                .collect(),
            indices: HashMap::new(),
            rows: vec![
                Row {
                    face: Face {
                        road: 0,
                        chunk: 0,
                        triangle: 0,
                    },
                    start: 0,
                    end: 3,
                    constant: 0.,
                    lower: 2.,
                    limit: 2.,
                    raw_lower: 2.,
                    raw_limit: 2.,
                    norm: 3.,
                    correction: 0.,
                },
                Row {
                    face: Face {
                        road: 0,
                        chunk: 0,
                        triangle: 1,
                    },
                    start: 3,
                    end: 5,
                    constant: 0.,
                    lower: 0.,
                    limit: 0.,
                    raw_lower: 0.,
                    raw_limit: 0.,
                    norm: 8.,
                    correction: 0.,
                },
            ],
            terms: vec![(0, 1.), (1, 1.), (2, 1.), (0, 2.), (1, -2.)],
            probes: BTreeMap::new(),
            resample_controls: true,
            envelope: None,
            allowed_samples: None,
            headroom_clipped: 0,
        };
        let mut meter = Meter {
            work: 0,
            limit: 10000,
            cancel: &|| false,
        };
        let mut values = vec![0, 0, 0];
        let (_, moves) = polish_values(&model, &mut values, &mut meter).unwrap();
        assert_eq!(values, [1, 1, 0]);
        assert_eq!(moves, 2);
        assert!(meter.work > 0 && meter.work < 10000);
        // Removing either free degree of freedom must not move a fixed sample
        // to obtain the otherwise desirable pair.
        model.variables[1].maximum = 0.;
        let mut values = vec![0, 0, 0];
        let mut meter = Meter {
            work: 0,
            limit: 10000,
            cancel: &|| false,
        };
        assert_eq!(polish_values(&model, &mut values, &mut meter).unwrap().1, 0);
        assert_eq!(values, [0, 0, 0]);
        // x=1 with 2x-2y in [-0.1,0.1]. The bad row mentions only x;
        // y is found through a neighbouring row that is initially satisfied.
        model.variables[1].maximum = 1.;
        model.terms = vec![(0, 1.), (0, 2.), (1, -2.)];
        model.rows[0].end = 1;
        model.rows[0].raw_lower = 1.;
        model.rows[0].raw_limit = 1.;
        model.rows[1].start = 1;
        model.rows[1].end = 3;
        model.rows[1].raw_lower = -0.1;
        model.rows[1].raw_limit = 0.1;
        let mut meter = Meter {
            work: 0,
            limit: 10000,
            cancel: &|| false,
        };
        assert_eq!(polish_values(&model, &mut values, &mut meter).unwrap().1, 2);
        assert_eq!(values, [1, 1, 0]);
    }
    #[test]
    fn integer_polish_chooses_neighboring_encoded_values_and_respects_boxes_and_meter() {
        let variables = (0..2)
            .map(|grid| Variable {
                grid,
                value: 0.5,
                minimum: 0.,
                maximum: 1.,
                box_correction: 0.,
            })
            .collect();
        let model = Model {
            variables,
            indices: HashMap::new(),
            rows: vec![Row {
                face: Face {
                    road: 0,
                    chunk: 0,
                    triangle: 0,
                },
                start: 0,
                end: 2,
                constant: 0.,
                lower: 1.,
                limit: 1.,
                raw_lower: 1.,
                raw_limit: 1.,
                norm: 2.,
                correction: 0.,
            }],
            terms: vec![(0, 1.), (1, 1.)],
            probes: BTreeMap::new(),
            resample_controls: true,
            envelope: None,
            allowed_samples: None,
            headroom_clipped: 0,
        };
        let cancel = || false;
        let mut meter = Meter {
            work: 0,
            limit: 1000,
            cancel: &cancel,
        };
        let mut values = vec![1, 1];
        let (passes, moves) = polish_values(&model, &mut values, &mut meter).unwrap();
        assert!(passes >= 1 && moves == 1);
        assert_eq!(values, [0, 1]);
        assert!(meter.work > 0);
        assert!(values.iter().all(|&v| (0..=1).contains(&v)));
        let mut limited = Meter {
            work: 0,
            limit: 1,
            cancel: &cancel,
        };
        assert_eq!(
            polish_values(&model, &mut [1, 1], &mut limited),
            Err(Failure::Budget)
        );
        let cancel = || true;
        let mut cancelled = Meter {
            work: 0,
            limit: 1000,
            cancel: &cancel,
        };
        assert_eq!(
            polish_values(&model, &mut [1, 1], &mut cancelled),
            Err(Failure::Cancelled)
        );
        let mut fixed = model;
        for v in &mut fixed.variables {
            v.minimum = 1.;
        }
        let mut meter = Meter {
            work: 0,
            limit: 1000,
            cancel: &|| false,
        };
        let mut values = vec![1, 1];
        assert_eq!(polish_values(&fixed, &mut values, &mut meter).unwrap().1, 0);
        assert_eq!(values, [1, 1]);
    }
    #[test]
    fn quantized_rebuild_retains_exact_grade_earthwork_and_protected_sample_gates() {
        let (height, topology, controls, template, chunk, bindings) = fixture();
        let collisions = [Collision {
            road: 0,
            chunk_index: 0,
            geometry: &chunk,
            template: &template,
            heights: &bindings,
        }];
        let input = Input {
            grid: Grid {
                size: 1000.,
                resolution: 33,
                origin_y: -500.,
                vertical_span: 1000.,
                height: &height,
            },
            ground: &height,
            topology: &topology,
            surface_resolution: 33,
            protected_topology: 1024,
            controls: &controls,
            junctions: &[],
            collisions: &collisions,
        };
        let settings = Settings {
            maximum_work: 2_000_000,
            ..settings()
        };
        let candidate = fit_quantized_for_rebuild(
            input,
            settings,
            &[],
            PaintEnvelope {
                source_painted: &height,
                source_rounding: None,
                allowed_topology: 1024,
            },
            |_| {},
            || false,
        )
        .unwrap();
        assert_eq!(candidate.report.engine, QUANTIZED_REBUILD_VERSION);
        assert!(candidate.mesh_fit_passed && candidate.report.full_grid_earthwork_validated);
        assert_eq!(candidate.report.violations_after, 0);
        assert!(candidate.report.work <= settings.maximum_work);
        assert!(!candidate.report.terrain_surface_clearance_validated);
        let protected = vec![1024; height.len()];
        let input = Input {
            topology: &protected,
            ..input
        };
        assert!(matches!(
            fit_quantized_for_rebuild(
                input,
                settings,
                &[],
                PaintEnvelope {
                    source_painted: &height,
                    source_rounding: None,
                    allowed_topology: 1024
                },
                |_| {},
                || false
            ),
            Err(Failure::Infeasible { .. })
        ));
        assert_eq!(input.grid.height, height);
    }
    #[test]
    fn mesh_local_selection_repairs_bad_grades_without_clearance_witnesses() {
        let (height, topology, controls, template, chunk, bindings) = fixture();
        let collisions = [Collision {
            road: 0,
            chunk_index: 0,
            geometry: &chunk,
            template: &template,
            heights: &bindings,
        }];
        let input = Input {
            grid: Grid {
                size: 1000.,
                resolution: 33,
                origin_y: -500.,
                vertical_span: 1000.,
                height: &height,
            },
            ground: &height,
            topology: &topology,
            surface_resolution: 33,
            protected_topology: 1024,
            controls: &controls,
            junctions: &[],
            collisions: &collisions,
        };
        assert!(
            select_clearance_samples(input, &[], 1_000_000, || false)
                .unwrap()
                .samples
                .is_empty()
        );
        let region =
            select_mesh_repair_samples(input, settings(), &[], 1_000_000, || false).unwrap();
        assert!(!region.samples.is_empty());
        assert_eq!(region.violated_probes, 0);
        assert!(matches!(
            select_mesh_repair_samples(input, settings(), &[], region.work - 1, || false),
            Err(Failure::Budget)
        ));
        assert!(matches!(
            select_mesh_repair_samples(input, settings(), &[], 1_000_000, || true),
            Err(Failure::Cancelled)
        ));
        let candidate = fit_local_for_rebuild(
            input,
            settings(),
            &[],
            PaintEnvelope {
                source_painted: &height,
                source_rounding: None,
                allowed_topology: 1024,
            },
            &region.samples,
            |_| {},
            || false,
        )
        .unwrap();
        assert!(candidate.mesh_fit_passed);
        assert_eq!(candidate.report.violations_after, 0);
        assert!(candidate.report.maximum_mesh_grade_before > f64::from(settings().maximum_grade));
        assert!(candidate.report.maximum_mesh_grade_after <= f64::from(settings().maximum_grade));
        for (i, &value) in candidate.height.iter().enumerate() {
            if !region.samples.contains(&i) {
                assert_eq!(value, height[i]);
            }
        }
        assert!(candidate.report.full_grid_earthwork_validated);
        assert!(candidate.report.protected_samples_unchanged);
        assert!(!candidate.report.terrain_surface_clearance_validated);
    }
    #[test]
    fn control_only_fit_is_explicit_bounded_and_never_certifies_missing_meshes() {
        let (height, topology, controls, _, _, _) = fixture();
        let input = || Input {
            grid: Grid {
                size: 1000.,
                resolution: 33,
                origin_y: -500.,
                vertical_span: 1000.,
                height: &height,
            },
            ground: &height,
            topology: &topology,
            surface_resolution: 33,
            protected_topology: 1024,
            controls: &controls,
            junctions: &[],
            collisions: &[],
        };
        assert!(matches!(
            fit(input(), settings(), || false),
            Err(Failure::Invalid(_))
        ));
        assert!(matches!(
            fit_resampling_controls(input(), settings(), &[], |_| {}, || false),
            Err(Failure::Invalid(_))
        ));
        let output = fit_control_samples(input(), settings(), &[], || false).unwrap();
        assert_eq!(output.height, height);
        assert_eq!(output.report.engine, CONTROLS_VERSION);
        assert!(!output.report.exact_quantized_mesh_grades_validated);
        assert!(output.report.full_grid_earthwork_validated);
        assert!(matches!(
            fit_control_samples(input(), settings(), &[height.len()], || false),
            Err(Failure::Invalid(_))
        ));
        assert!(matches!(
            fit_control_samples(input(), settings(), &[], || true),
            Err(Failure::Cancelled)
        ));
    }
    #[test]
    fn protected_paint_envelope_limits_repairs_to_source_changed_samples_and_excludes_other_flags()
    {
        let (mut height, _, mut controls, template, mut chunk, bindings) = fixture();
        height.fill(10000);
        let ground = height.clone();
        height[16 * 33 + 16] += 164;
        let painted = height.clone();
        let topology = vec![1024; 33 * 33];
        let grid = Grid {
            size: 1000.,
            resolution: 33,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &height,
        };
        for p in controls.iter_mut().flatten() {
            p[1] = path_terrain::sample_height(grid, *p).unwrap();
        }
        for p in &mut chunk.mesh.vertices {
            p[1] = path_terrain::sample_height(grid, *p).unwrap();
        }
        let lo = chunk
            .mesh
            .vertices
            .iter()
            .map(|p| p[1])
            .fold(f32::INFINITY, f32::min);
        let hi = chunk
            .mesh
            .vertices
            .iter()
            .map(|p| p[1])
            .fold(f32::NEG_INFINITY, f32::max);
        chunk.mesh.center[1] = (lo + hi) * 0.5;
        chunk.mesh.extents[1] = (hi - lo) * 0.5;
        let collisions = [Collision {
            road: 0,
            chunk_index: 0,
            geometry: &chunk,
            template: &template,
            heights: &bindings,
        }];
        let input = |topology| Input {
            grid,
            ground: &ground,
            topology,
            surface_resolution: 33,
            protected_topology: 1024 | 16384,
            controls: &controls,
            junctions: &[],
            collisions: &collisions,
        };
        let probes = [ClearanceProbe {
            road: 0,
            chunk: 0,
            triangle: 0,
            position: [-5. / 3., 20. / 3.],
            maximum_terrain_excess: 0.01,
        }];
        let settings = Settings {
            maximum_iterations: 512,
            maximum_work: 2_000_000,
            ..settings()
        };
        let envelope = PaintEnvelope {
            source_painted: &painted,
            source_rounding: None,
            allowed_topology: 1024,
        };
        let output = fit_in_paint_envelope(
            input(&topology),
            settings,
            &probes,
            envelope,
            |_| {},
            || false,
        )
        .unwrap();
        assert_eq!(output.report.engine, ENVELOPE_VERSION);
        let selected = [16 * 33 + 16];
        // This source paint envelope permits exactly the raised centre sample
        // to repair the clearance witness. Retaining it must beat that envelope
        // in every mode, including provisional and integer-coordinate repair.
        for mode in [
            RebuildMode::Feedback,
            RebuildMode::Quantized,
            RebuildMode::FrozenControls,
            RebuildMode::Local(&selected),
        ] {
            let legacy = match mode {
                RebuildMode::Feedback => fit_for_rebuild(
                    input(&topology),
                    settings,
                    &probes,
                    envelope,
                    |_| {},
                    || false,
                ),
                RebuildMode::Quantized => fit_quantized_for_rebuild(
                    input(&topology),
                    settings,
                    &probes,
                    envelope,
                    |_| {},
                    || false,
                ),
                RebuildMode::FrozenControls => fit_clearance_for_rebuild(
                    input(&topology),
                    settings,
                    &probes,
                    envelope,
                    |_| {},
                    || false,
                ),
                RebuildMode::Local(region) => fit_local_for_rebuild(
                    input(&topology),
                    settings,
                    &probes,
                    envelope,
                    region,
                    |_| {},
                    || false,
                ),
            }
            .unwrap();
            let run = |samples: &[usize], settings, cancelled: fn() -> bool| {
                fit_retaining_for_rebuild(
                    input(&topology),
                    settings,
                    &probes,
                    RebuildPass { mode, envelope },
                    samples,
                    |_| {},
                    cancelled,
                )
            };
            let empty = run(&[], settings, || false).unwrap();
            assert_eq!(legacy.height, empty.height);
            assert_eq!(legacy.controls, empty.controls);
            assert_eq!(
                serde_json::to_vec(&legacy.report).unwrap(),
                serde_json::to_vec(&empty.report).unwrap()
            );
            let retained = run(&[0], settings, || false).unwrap();
            assert!(retained.mesh_fit_passed);
            assert_eq!(retained.height[0], height[0]);
            assert_eq!(retained.report.retained_sample_entries, Some(1));
            assert!(retained.report.changed_samples > 0);
            assert!(matches!(
                run(&selected, settings, || false),
                Err(Failure::ClearanceInfeasible { .. })
            ));
            assert!(matches!(
                run(&[height.len()], settings, || false),
                Err(Failure::Invalid(_))
            ));
            assert!(matches!(
                run(&[0], settings, || true),
                Err(Failure::Cancelled)
            ));
            assert!(matches!(
                run(
                    &[0],
                    Settings {
                        maximum_work: 1,
                        ..settings
                    },
                    || false
                ),
                Err(Failure::Budget)
            ));
        }
        let region =
            select_clearance_samples(input(&topology), &probes, 1_000_000, || false).unwrap();
        assert_eq!(region.violated_probes, 1);
        assert!(region.samples.contains(&selected[0]));
        assert!(region.work > 0);
        assert!(matches!(
            select_clearance_samples(input(&topology), &probes, region.work - 1, || false),
            Err(Failure::Budget)
        ));
        assert!(matches!(
            select_clearance_samples(input(&topology), &probes, 1_000_000, || true),
            Err(Failure::Cancelled)
        ));
        let local = fit_local_for_rebuild(
            input(&topology),
            settings,
            &probes,
            envelope,
            &selected,
            |_| {},
            || false,
        )
        .unwrap();
        assert!(local.mesh_fit_passed);
        assert_eq!(local.report.engine, LOCAL_REBUILD_VERSION);
        assert_eq!(local.report.repair_sample_limit, Some(1));
        assert_eq!(local.report.variables, 1);
        assert_eq!(local.report.clearance_probe_violations_after, 0);
        assert!(local.report.full_grid_earthwork_validated);
        for (i, &value) in local.height.iter().enumerate() {
            if !selected.contains(&i) {
                assert_eq!(value, height[i]);
            }
        }
        assert!(matches!(
            fit_local_for_rebuild(
                input(&topology),
                settings,
                &probes,
                envelope,
                &[0],
                |_| {},
                || false
            ),
            Err(Failure::ClearanceInfeasible { .. })
        ));
        assert!(matches!(
            fit_local_for_rebuild(
                input(&topology),
                settings,
                &probes,
                envelope,
                &[],
                |_| {},
                || false
            ),
            Err(Failure::Invalid(_))
        ));
        assert!(matches!(
            fit_local_for_rebuild(
                input(&topology),
                settings,
                &probes,
                envelope,
                &[height.len()],
                |_| {},
                || false
            ),
            Err(Failure::Invalid(_))
        ));
        assert!(matches!(
            fit_local_for_rebuild(
                input(&topology),
                settings,
                &probes,
                envelope,
                &selected,
                |_| {},
                || true
            ),
            Err(Failure::Cancelled)
        ));
        let frozen = fit_clearance_for_rebuild(
            input(&topology),
            settings,
            &probes,
            envelope,
            |_| {},
            || false,
        )
        .unwrap();
        assert!(frozen.mesh_fit_passed);
        assert_eq!(frozen.controls, controls);
        assert!(frozen.report.control_supports_frozen);
        assert_eq!(frozen.report.control_heights_resampled, 0);
        assert_eq!(frozen.report.clearance_probe_violations_after, 0);
        assert!(frozen.report.full_grid_earthwork_validated);
        assert!(frozen.report.protected_paint_envelope_validated);
        assert!(matches!(
            fit_clearance_for_rebuild(
                input(&topology),
                settings,
                &probes,
                envelope,
                |_| {},
                || true
            ),
            Err(Failure::Cancelled)
        ));
        assert_eq!(output.report.protected_envelope_samples, 1);
        assert_eq!(output.report.protected_samples_changed, 1);
        assert!(
            output.report.protected_paint_envelope_validated
                && !output.report.protected_samples_unchanged
        );
        for (i, &value) in output.height.iter().enumerate() {
            assert!((ground[i].min(painted[i])..=ground[i].max(painted[i])).contains(&value));
            if i != 16 * 33 + 16 {
                assert_eq!(value, height[i]);
            }
        }
        let unreserved = fit_in_paint_envelope(
            input(&topology),
            Settings {
                quantization_reserve: 0.,
                reserve_roundoff: false,
                ..settings
            },
            &probes,
            envelope,
            |_| {},
            || false,
        );
        // The real-valued projection converges, but nearest-short rounding
        // leaves a 0.0102539 m intersection. It must never publish a grid.
        assert!(
            matches!(unreserved,Err(Failure::NoConvergence {remaining:0,remaining_clearance:1,maximum_terrain_excess,..}) if maximum_terrain_excess>f64::from(probes[0].maximum_terrain_excess))
        );
        let provisional = fit_for_rebuild(
            input(&topology),
            Settings {
                quantization_reserve: 0.,
                reserve_roundoff: false,
                ..settings
            },
            &probes,
            envelope,
            |_| {},
            || false,
        )
        .unwrap();
        assert!(!provisional.mesh_fit_passed);
        assert!(!provisional.report.exact_quantized_mesh_grades_validated);
        assert_eq!(provisional.report.engine, REBUILD_VERSION);
        assert_eq!(provisional.report.clearance_probe_violations_after, 1);
        assert_eq!(provisional.report.iterations, settings.maximum_iterations);
        assert!(!provisional.report.projection_work_limit_reached);
        assert!(provisional.report.full_grid_earthwork_validated);
        assert!(provisional.report.protected_paint_envelope_validated);
        let budgeted = fit_for_rebuild(
            input(&topology),
            Settings {
                maximum_work: 10_000,
                quantization_reserve: 0.,
                reserve_roundoff: false,
                ..settings
            },
            &probes,
            envelope,
            |_| {},
            || false,
        )
        .unwrap();
        assert!(!budgeted.mesh_fit_passed);
        assert!(budgeted.report.projection_work_limit_reached);
        assert!(budgeted.report.work <= 10_000);
        assert!(budgeted.report.full_grid_earthwork_validated);
        assert!(budgeted.report.protected_paint_envelope_validated);
        assert!(matches!(
            fit_for_rebuild(
                input(&topology),
                Settings {
                    maximum_work: 16,
                    ..settings
                },
                &probes,
                envelope,
                |_| {},
                || false
            ),
            Err(Failure::Budget)
        ));
        for (i, &value) in provisional.height.iter().enumerate() {
            assert!((ground[i].min(painted[i])..=ground[i].max(painted[i])).contains(&value));
        }
        assert!(matches!(
            fit_for_rebuild(
                input(&topology),
                settings,
                &probes,
                envelope,
                |_| {},
                || true
            ),
            Err(Failure::Cancelled)
        ));
        let (limit, clipped) = bounded_headroom(0.003459, 0.01, 0.007322);
        assert!(clipped && limit > 0.007322 && limit < 0.01);
        let water = vec![1024 | 16384; 33 * 33];
        assert!(matches!(
            fit_in_paint_envelope(input(&water), settings, &probes, envelope, |_| {}, || false),
            Err(Failure::ClearanceInfeasible { .. })
        ));
        assert!(matches!(
            fit_for_rebuild(input(&water), settings, &probes, envelope, |_| {}, || false),
            Err(Failure::ClearanceInfeasible { .. })
        ));
        let water_output = fit_in_paint_envelope(
            input(&water),
            settings,
            &probes,
            PaintEnvelope {
                source_painted: &painted,
                source_rounding: None,
                allowed_topology: 1024 | 16384,
            },
            |_| {},
            || false,
        )
        .unwrap();
        assert_eq!(water_output.height, output.height);
        assert!(water_output.report.protected_paint_envelope_validated);
        let mut insufficient = painted.clone();
        insufficient[16 * 33 + 16] -= 1;
        assert_eq!(
            fit_in_paint_envelope(
                input(&topology),
                settings,
                &probes,
                PaintEnvelope {
                    source_painted: &insufficient,
                    source_rounding: None,
                    ..envelope
                },
                |_| {},
                || false
            )
            .err(),
            Some(Failure::Invalid("initial protected paint envelope"))
        );
        assert_eq!(
            fit_in_paint_envelope(
                input(&topology),
                settings,
                &probes,
                PaintEnvelope {
                    allowed_topology: 2048,
                    ..envelope
                },
                |_| {},
                || false
            )
            .err(),
            Some(Failure::Invalid("settings or buffers"))
        );
    }
    #[test]
    fn grid_aligned_pins_allow_inert_neighbor_repairs_but_keep_monuments_protected() {
        let (mut height, topology, _, template, mut chunk, bindings) = fixture();
        let peak = 16 * 33 + 16;
        height[peak] += 164;
        let grid = Grid {
            size: 1024.,
            resolution: 33,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &height,
        };
        let point = |x, z| [x, path_terrain::sample_height(grid, [x, 0., z]).unwrap(), z];
        let controls = vec![vec![point(0., -32.), point(32., -32.)]];
        let support = path_terrain::height_support(layout(grid), controls[0][0]).unwrap();
        assert!(support.indices.contains(&peak));
        assert!(!support.pinned_indices().any(|i| i == peak));
        for p in &mut chunk.mesh.vertices {
            p[1] = path_terrain::sample_height(grid, *p).unwrap();
        }
        let lo = chunk
            .mesh
            .vertices
            .iter()
            .map(|p| p[1])
            .fold(f32::INFINITY, f32::min);
        let hi = chunk
            .mesh
            .vertices
            .iter()
            .map(|p| p[1])
            .fold(f32::NEG_INFINITY, f32::max);
        chunk.mesh.center[1] = (lo + hi) * 0.5;
        chunk.mesh.extents[1] = (hi - lo) * 0.5;
        let collisions = [Collision {
            road: 0,
            chunk_index: 0,
            geometry: &chunk,
            template: &template,
            heights: &bindings,
        }];
        let input = |topology| Input {
            grid,
            ground: &height,
            topology,
            surface_resolution: 33,
            protected_topology: 1024,
            controls: &controls,
            junctions: &[],
            collisions: &collisions,
        };
        let probes = [ClearanceProbe {
            road: 0,
            chunk: 0,
            triangle: 0,
            position: [-5. / 3., 20. / 3.],
            maximum_terrain_excess: 0.01,
        }];
        let settings = Settings {
            maximum_iterations: 512,
            maximum_work: 2_000_000,
            ..settings()
        };
        let output =
            fit_with_probes(input(&topology), settings, &probes, |_| {}, || false).unwrap();
        assert_ne!(output.height[peak], height[peak]);
        assert_eq!(output.controls, controls);
        assert_eq!(output.report.control_heights_resampled, 0);
        assert_eq!(output.report.clearance_probe_violations_after, 0);
        assert!(output.report.full_grid_earthwork_validated);
        let protected = vec![1024; height.len()];
        assert!(matches!(
            fit_with_probes(input(&protected), settings, &probes, |_| {}, || false),
            Err(Failure::Infeasible { .. } | Failure::ClearanceInfeasible { .. })
        ));
    }
    #[test]
    fn jointly_fits_clearance_and_grades_and_rejects_invalid_or_frozen_witnesses() {
        let (mut height, topology, controls, template, mut chunk, bindings) = fixture();
        height[16 * 33 + 16] += 164;
        let original = height.clone();
        let grid = Grid {
            size: 1000.,
            resolution: 33,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &height,
        };
        for p in &mut chunk.mesh.vertices {
            p[1] = path_terrain::sample_height(grid, *p).unwrap();
        }
        let lo = chunk
            .mesh
            .vertices
            .iter()
            .map(|p| p[1])
            .fold(f32::INFINITY, f32::min);
        let hi = chunk
            .mesh
            .vertices
            .iter()
            .map(|p| p[1])
            .fold(f32::NEG_INFINITY, f32::max);
        chunk.mesh.center[1] = (lo + hi) * 0.5;
        chunk.mesh.extents[1] = (hi - lo) * 0.5;
        let collisions = [Collision {
            road: 0,
            chunk_index: 0,
            geometry: &chunk,
            template: &template,
            heights: &bindings,
        }];
        let input = || Input {
            grid,
            ground: &height,
            topology: &topology,
            surface_resolution: 33,
            protected_topology: 1024,
            controls: &controls,
            junctions: &[],
            collisions: &collisions,
        };
        let probe = ClearanceProbe {
            road: 0,
            chunk: 0,
            triangle: 0,
            position: [-5. / 3., 20. / 3.],
            maximum_terrain_excess: 0.01,
        };
        let settings = Settings {
            maximum_iterations: 512,
            maximum_work: 2_000_000,
            ..settings()
        };
        let output = fit_with_probes(
            input(),
            settings,
            std::slice::from_ref(&probe),
            |_| {},
            || false,
        )
        .unwrap();
        assert!(output.report.clearance_probe_violations_before > 0);
        assert_eq!(output.report.clearance_probe_violations_after, 0);
        assert_eq!(output.report.engine, COUPLED_VERSION);
        assert!(output.report.maximum_mesh_grade_after <= f64::from(settings.maximum_grade));
        assert!(
            output.report.maximum_probe_terrain_excess_after
                <= f64::from(probe.maximum_terrain_excess)
        );
        assert!(!output.report.terrain_surface_clearance_validated);
        assert_eq!(height, original);
        assert_eq!(
            fit_with_probes(
                input(),
                settings,
                &[probe.clone(), probe.clone()],
                |_| {},
                || false
            )
            .err(),
            Some(Failure::Invalid("clearance probe"))
        );
        assert_eq!(
            fit_with_probes(
                input(),
                settings,
                &[ClearanceProbe {
                    position: [100., 100.],
                    ..probe.clone()
                }],
                |_| {},
                || false
            )
            .err(),
            Some(Failure::Invalid("clearance witness outside triangle"))
        );
        let frozen_settings = Settings {
            maximum_grade: 1.,
            maximum_cut_fill: 0.,
            ..settings
        };
        let rejected = fit_with_probes(
            input(),
            frozen_settings,
            std::slice::from_ref(&probe),
            |_| {},
            || false,
        )
        .err()
        .unwrap();
        let Failure::ClearanceInfeasible { work, .. } = rejected else {
            panic!("Expected frozen clearance rejection: {rejected:?}");
        };
        assert!(work > 0 && work < frozen_settings.maximum_work);
        // The rejection meter includes construction, not just the last probe.
        // A grant below that reported cost must stop before the same rejection.
        assert_eq!(
            fit_with_probes(
                input(),
                Settings {
                    maximum_work: work - 1,
                    ..frozen_settings
                },
                &[probe],
                |_| {},
                || false,
            )
            .err(),
            Some(Failure::Budget)
        );
    }
    #[test]
    fn control_resampling_keeps_exact_shared_junctions_and_does_not_override_authored_protection() {
        let (mut height, topology, _, template, mut chunk, bindings) = fixture();
        height[16 * 33 + 16] += 164;
        let grid = Grid {
            size: 1000.,
            resolution: 33,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &height,
        };
        for p in &mut chunk.mesh.vertices {
            p[1] = path_terrain::sample_height(grid, *p).unwrap();
        }
        let lo = chunk
            .mesh
            .vertices
            .iter()
            .map(|p| p[1])
            .fold(f32::INFINITY, f32::min);
        let hi = chunk
            .mesh
            .vertices
            .iter()
            .map(|p| p[1])
            .fold(f32::NEG_INFINITY, f32::max);
        chunk.mesh.center[1] = (lo + hi) * 0.5;
        chunk.mesh.extents[1] = (hi - lo) * 0.5;
        let controls = vec![
            vec![chunk.mesh.vertices[0], chunk.mesh.vertices[1]],
            vec![chunk.mesh.vertices[1], chunk.mesh.vertices[2]],
        ];
        let junctions = [Junction {
            a: (0, 1),
            b: (1, 0),
        }];
        let collisions = [Collision {
            road: 0,
            chunk_index: 0,
            geometry: &chunk,
            template: &template,
            heights: &bindings,
        }];
        let input = |topology| Input {
            grid,
            ground: &height,
            topology,
            surface_resolution: 33,
            protected_topology: 1024,
            controls: &controls,
            junctions: &junctions,
            collisions: &collisions,
        };
        let probes = [ClearanceProbe {
            road: 0,
            chunk: 0,
            triangle: 0,
            position: [-5. / 3., 20. / 3.],
            maximum_terrain_excess: 0.01,
        }];
        let settings = Settings {
            maximum_iterations: 512,
            maximum_work: 2_000_000,
            ..settings()
        };
        assert!(matches!(
            fit_with_probes(input(&topology), settings, &probes, |_| {}, || false),
            Err(Failure::ClearanceInfeasible { .. })
        ));
        let output =
            fit_resampling_controls(input(&topology), settings, &probes, |_| {}, || false).unwrap();
        assert!(output.report.control_heights_resampled > 0);
        assert!(
            !output.report.control_supports_frozen && output.report.source_paths_must_be_rebuilt
        );
        assert!(output.report.maximum_control_grade <= f64::from(settings.maximum_grade));
        assert!(output.report.control_horizontal_and_junctions_preserved);
        assert_eq!(output.controls[0][1], output.controls[1][0]);
        for (old, new) in controls
            .iter()
            .flatten()
            .zip(output.controls.iter().flatten())
        {
            assert_eq!([old[0], old[2]], [new[0], new[2]]);
        }
        let protected_topology = vec![1024; topology.len()];
        assert!(matches!(
            fit_resampling_controls(
                input(&protected_topology),
                settings,
                &probes,
                |_| {},
                || false
            ),
            Err(Failure::ClearanceInfeasible { .. })
        ));
    }
    #[test]
    fn folded_mesh_rejects_with_exact_road_chunk_and_face_without_changing_inputs() {
        let (height, topology, controls, template, mut chunk, bindings) = fixture();
        for p in &mut chunk.mesh.vertices {
            p[0] = -p[0];
        }
        let before = height.clone();
        let collisions = [Collision {
            road: 0,
            chunk_index: 7,
            geometry: &chunk,
            template: &template,
            heights: &bindings,
        }];
        let result = fit(
            Input {
                grid: Grid {
                    size: 1000.,
                    resolution: 33,
                    origin_y: -500.,
                    vertical_span: 1000.,
                    height: &height,
                },
                ground: &height,
                topology: &topology,
                surface_resolution: 33,
                protected_topology: 1024,
                controls: &controls,
                junctions: &[],
                collisions: &collisions,
            },
            settings(),
            || false,
        );
        assert!(matches!(
            result,
            Err(Failure::HorizontalGeometry {
                road: 0,
                chunk: 7,
                triangle: 0,
                inverted: 1,
                degenerate: 0
            })
        ));
        assert_eq!(height, before);
    }
    #[test]
    fn fits_quantized_edge_heights_without_moving_controls_or_unrelated_terrain() {
        let (height, topology, controls, template, chunk, bindings) = fixture();
        let collisions = [Collision {
            road: 0,
            chunk_index: 0,
            geometry: &chunk,
            template: &template,
            heights: &bindings,
        }];
        let input = || Input {
            grid: Grid {
                size: 1000.,
                resolution: 33,
                origin_y: -500.,
                vertical_span: 1000.,
                height: &height,
            },
            ground: &height,
            topology: &topology,
            surface_resolution: 33,
            protected_topology: 1024,
            controls: &controls,
            junctions: &[],
            collisions: &collisions,
        };
        let output = fit(input(), settings(), || false).unwrap();
        assert_eq!(output.report.violations_before, 1);
        assert_eq!(output.report.violations_after, 0);
        assert!(output.report.changed_samples > 0 && output.report.changed_samples <= 8);
        assert_eq!(output.height[0], height[0]);
        assert!(output.report.maximum_mesh_grade_after <= f64::from(settings().maximum_grade));
        assert!(
            output.report.protected_samples_unchanged
                && output.report.controls_and_junctions_unchanged
        );
        assert!(!output.report.terrain_surface_clearance_validated);
        assert_eq!(
            output.height,
            fit(input(), settings(), || false).unwrap().height
        );
        assert_eq!(
            fit(
                input(),
                Settings {
                    maximum_work: 1,
                    ..settings()
                },
                || false
            )
            .err(),
            Some(Failure::Budget)
        );
        assert_eq!(
            fit(input(), settings(), || true).err(),
            Some(Failure::Cancelled)
        );
        let calls = std::cell::Cell::new(0usize);
        assert_eq!(
            fit(input(), settings(), || {
                let n = calls.get();
                calls.set(n + 1);
                n > 14
            })
            .err(),
            Some(Failure::Cancelled)
        );
    }
    #[test]
    fn immutable_or_insufficient_earthwork_returns_no_partial_height() {
        let (height, mut topology, controls, template, chunk, bindings) = fixture();
        let original = height.clone();
        topology.fill(1024);
        let collisions = [Collision {
            road: 0,
            chunk_index: 0,
            geometry: &chunk,
            template: &template,
            heights: &bindings,
        }];
        let input = || Input {
            grid: Grid {
                size: 1000.,
                resolution: 33,
                origin_y: -500.,
                vertical_span: 1000.,
                height: &height,
            },
            ground: &height,
            topology: &topology,
            surface_resolution: 33,
            protected_topology: 1024,
            controls: &controls,
            junctions: &[],
            collisions: &collisions,
        };
        assert!(matches!(
            fit(input(), settings(), || false),
            Err(Failure::Infeasible { .. })
        ));
        let mut editable = input();
        editable.protected_topology = 0;
        assert!(matches!(
            fit(
                editable,
                Settings {
                    maximum_cut_fill: 0.,
                    ..settings()
                },
                || false
            ),
            Err(Failure::Infeasible { .. })
        ));
        assert_eq!(height, original);
    }
}
