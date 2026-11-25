//! Independent constrained recovery for source-cost road smoothing failures.
use crate::{
    native_layers::Layers,
    path_geometry::Failure,
    road_network::{Settings, ShapingPolicy},
};
pub const VERSION: &str = "papyrus-constrained-road-shaping-1";
#[derive(Clone, Debug, serde::Serialize)]
pub struct Audit {
    pub engine: &'static str,
    pub paths: usize,
    pub attempts: usize,
    pub accepted_moves: usize,
    pub backtracked_moves: usize,
    pub held_moves: usize,
    pub moved_controls: usize,
    pub output_controls: usize,
    pub work: usize,
}
impl Default for Audit {
    fn default() -> Self {
        Self {
            engine: VERSION,
            paths: 0,
            attempts: 0,
            accepted_moves: 0,
            backtracked_moves: 0,
            held_moves: 0,
            moved_controls: 0,
            output_controls: 0,
            work: 0,
        }
    }
}
impl Audit {
    pub fn merge(&mut self, other: &Self) {
        self.paths += other.paths;
        self.attempts += other.attempts;
        self.accepted_moves += other.accepted_moves;
        self.backtracked_moves += other.backtracked_moves;
        self.held_moves += other.held_moves;
        self.moved_controls += other.moved_controls;
        self.output_controls += other.output_controls;
        self.work += other.work;
    }
}
pub(crate) struct Shaped {
    pub points: Vec<[f32; 3]>,
    pub audit: Audit,
}
#[derive(Clone, Copy, Debug)]
pub struct CurveCostAudit {
    pub clear: bool,
    pub work: usize,
}
pub(crate) fn ring_curve_cost_clear(
    path: &crate::path_geometry::Path,
    costs: &[u32],
    size: u32,
    spacing: f32,
    maximum_work: usize,
    cancel: &impl Fn() -> bool,
) -> Result<Option<bool>, Failure> {
    curve_cost_check(path, costs, size, spacing, maximum_work, cancel).map(|v| v.map(|a| a.clear))
}
/// Source Hermite curve clearance, including a sampling-motion margin. A safe
/// control polygon alone does not bound the interpolated curve at sharp bends.
pub fn curve_cost_check(
    path: &crate::path_geometry::Path,
    costs: &[u32],
    size: u32,
    spacing: f32,
    maximum_work: usize,
    cancel: &impl Fn() -> bool,
) -> Result<Option<CurveCostAudit>, Failure> {
    curve_cost_check_grid(path, costs, size, 257, false, spacing, maximum_work, cancel)
}
/// Check the native source lattice without reinterpreting cell centres as
/// endpoint samples. This preserves the full source resolution and obstacles.
pub fn curve_cell_cost_check(
    path: &crate::path_geometry::Path,
    costs: &[u32],
    lattice: crate::road_costmap::CellLattice,
    spacing: f32,
    maximum_work: usize,
    cancel: &impl Fn() -> bool,
) -> Result<Option<CurveCostAudit>, Failure> {
    if lattice.size().fract() != 0. {
        return Err(Failure::Invalid);
    }
    curve_cost_check_grid(
        path,
        costs,
        lattice.size() as u32,
        lattice.resolution(),
        true,
        spacing,
        maximum_work,
        cancel,
    )
}
#[allow(clippy::too_many_arguments)]
fn curve_cost_check_grid(
    path: &crate::path_geometry::Path,
    costs: &[u32],
    size: u32,
    n: usize,
    centres: bool,
    spacing: f32,
    maximum_work: usize,
    cancel: &impl Fn() -> bool,
) -> Result<Option<CurveCostAudit>, Failure> {
    if !(2..=1024).contains(&n)
        || costs.len() != n * n
        || !(1000..=6000).contains(&size)
        || !spacing.is_finite()
        || !(0.5..=4.).contains(&spacing)
        || path.points().len() < 2
        || maximum_work == 0
    {
        return Err(Failure::Invalid);
    }
    let mut meter = Meter {
        work: 0,
        maximum: maximum_work,
    };
    let scale = if centres { n } else { n - 1 } as f64;
    let offset = if centres { 0.5 } else { 0. };
    let cell = f64::from(size) / scale;
    let radius = f64::from(spacing) / cell;
    let step = path.length() / (path.points().len() - 1) as f32;
    for (i, pair) in path.points().windows(2).enumerate() {
        let run = (0..3)
            .map(|a| (pair[1][a] - pair[0][a]).powi(2))
            .sum::<f32>()
            .sqrt();
        // Same conservative speed bound as complete source-family auditing.
        let count = ((1.5 * run + 2. * step) / spacing).ceil().max(2.) as usize;
        if count > 65536 {
            return Err(Failure::Budget);
        }
        for j in 0..=count {
            if cancel() {
                return Ok(None);
            }
            meter.tick()?;
            let p = path.point((i as f32 + j as f32 / count as f32) * step)?;
            let q = [p[0], p[2]].map(|v| (f64::from(v) / f64::from(size) + 0.5) * scale - offset);
            if centres
                && q.iter()
                    .any(|v| !v.is_finite() || *v - radius < -0.5 || *v + radius > n as f64 - 0.5)
            {
                return Ok(Some(CurveCostAudit {
                    clear: false,
                    work: meter.work,
                }));
            }
            let lo = q.map(|v| (v - radius - 0.5).ceil().clamp(0., (n - 1) as f64) as usize);
            let hi = q.map(|v| (v + radius + 0.5).floor().clamp(0., (n - 1) as f64) as usize);
            for z in lo[1]..=hi[1] {
                for x in lo[0]..=hi[0] {
                    meter.tick()?;
                    if costs[z * n + x] == 0 {
                        let nearest = [
                            q[0].clamp(x as f64 - 0.5, x as f64 + 0.5),
                            q[1].clamp(z as f64 - 0.5, z as f64 + 0.5),
                        ];
                        if (nearest[0] - q[0]).hypot(nearest[1] - q[1]) <= radius {
                            return Ok(Some(CurveCostAudit {
                                clear: false,
                                work: meter.work,
                            }));
                        }
                    }
                }
            }
        }
    }
    Ok(Some(CurveCostAudit {
        clear: true,
        work: meter.work,
    }))
}
/// Closed-ring geometry proposal only. No grade, earthwork or terrain acceptance
/// is implied: callers must fit the complete native corridor afterwards. This
/// avoids freezing a sharp horizontal bend merely because a provisional height
/// cannot satisfy a local test before the coupled fit has run.
pub(crate) fn propose_ring_geometry(
    points: &[[f32; 3]],
    costs: &[u32],
    n: usize,
    size: f32,
    policy: ShapingPolicy,
    clearance: f32,
    cancel: &impl Fn() -> bool,
) -> Result<Option<Shaped>, Failure> {
    if !clearance.is_finite()
        || !(0. ..=16.).contains(&clearance)
        || policy.validate().is_err()
        || !(5..=32768).contains(&points.len())
        || points.first() != points.last()
        || !(2..=1024).contains(&n)
        || costs.len() != n * n
        || !size.is_finite()
        || size <= 0.
        || points
            .iter()
            .flatten()
            .any(|v| !v.is_finite() || v.abs() > 10000.)
    {
        return Err(Failure::Invalid);
    }
    let mut meter = Meter {
        work: 0,
        maximum: policy.maximum_work,
    };
    match clear_with_margin(points, costs, n, size, clearance, &mut meter, cancel)? {
        None => return Ok(None),
        Some(false) => return Err(Failure::Invalid),
        Some(true) => {}
    }
    let mut p = points.to_vec();
    let last = p.len() - 1;
    let mut audit = Audit {
        engine: "papyrus-ring-cost-shape-proposal-1",
        paths: 1,
        ..Default::default()
    };
    let mut moved = vec![false; last];
    for _ in 0..policy.horizontal_iterations {
        for parity in 0..2 {
            for i in (parity..last).step_by(2) {
                if cancel() {
                    return Ok(None);
                }
                meter.tick()?;
                let before = if i == 0 { last - 1 } else { i - 1 };
                let after = if i + 1 == last { 0 } else { i + 1 };
                let average = std::array::from_fn::<_, 3, _>(|axis| {
                    if axis == 1 {
                        p[i][axis]
                    } else {
                        (p[before][axis] + 2. * p[i][axis] + p[after][axis]) * 0.25
                    }
                });
                if average == p[i] {
                    continue;
                }
                audit.attempts += 1;
                let mut accepted = false;
                for step in 0..=8 {
                    meter.tick()?;
                    let fraction = 1. / (1u32 << step) as f32;
                    let candidate = std::array::from_fn(|axis| {
                        p[i][axis] + fraction * (average[axis] - p[i][axis])
                    });
                    if let Some(clear) = clear_with_margin(
                        &[p[before], candidate, p[after]],
                        costs,
                        n,
                        size,
                        clearance,
                        &mut meter,
                        cancel,
                    )? {
                        if !clear {
                            continue;
                        }
                        p[i] = candidate;
                        if i == 0 {
                            p[last] = p[0];
                        }
                        audit.accepted_moves += 1;
                        audit.backtracked_moves += usize::from(step > 0);
                        moved[i] = true;
                        accepted = true;
                        break;
                    } else {
                        return Ok(None);
                    }
                }
                if !accepted {
                    audit.held_moves += 1;
                }
            }
        }
    }
    let mut output = Vec::with_capacity(p.len());
    output.push(p[0]);
    for pair in p.windows(2) {
        if cancel() {
            return Ok(None);
        }
        meter.tick()?;
        let run = (pair[1][0] - pair[0][0]).hypot(pair[1][2] - pair[0][2]);
        let count = (run / policy.resample_distance).ceil().max(1.) as usize;
        if output.len().checked_add(count).is_none_or(|v| v > 65536) {
            return Err(Failure::Budget);
        }
        for i in 1..count {
            meter.tick()?;
            let t = i as f32 / count as f32;
            output.push(std::array::from_fn(|axis| {
                pair[0][axis] + t * (pair[1][axis] - pair[0][axis])
            }));
        }
        output.push(pair[1]);
    }
    match clear_with_margin(&output, costs, n, size, clearance, &mut meter, cancel)? {
        None => return Ok(None),
        Some(false) => return Err(Failure::Invalid),
        Some(true) => {}
    }
    audit.moved_controls = moved.into_iter().filter(|v| *v).count();
    audit.output_controls = output.len();
    audit.work = meter.work;
    Ok(Some(Shaped {
        points: output,
        audit,
    }))
}
struct Meter {
    work: usize,
    maximum: usize,
}
impl Meter {
    fn tick(&mut self) -> Result<(), Failure> {
        if self.work >= self.maximum {
            return Err(Failure::Budget);
        }
        self.work += 1;
        Ok(())
    }
}
fn clear(
    points: &[[f32; 3]],
    costs: &[u32],
    n: usize,
    size: f32,
    meter: &mut Meter,
    cancel: &impl Fn() -> bool,
) -> Result<Option<bool>, Failure> {
    clear_with_margin(points, costs, n, size, 0., meter, cancel)
}
fn clear_with_margin(
    points: &[[f32; 3]],
    costs: &[u32],
    n: usize,
    size: f32,
    clearance: f32,
    meter: &mut Meter,
    cancel: &impl Fn() -> bool,
) -> Result<Option<bool>, Failure> {
    let padding = f64::from(clearance) / f64::from(size) * (n - 1) as f64;
    for segment in points.windows(2) {
        if cancel() {
            return Ok(None);
        }
        let xy = |p: [f32; 3]| {
            [
                (f64::from(p[0]) / f64::from(size) + 0.5) * (n - 1) as f64,
                (f64::from(p[2]) / f64::from(size) + 0.5) * (n - 1) as f64,
            ]
        };
        let a = xy(segment[0]);
        let b = xy(segment[1]);
        let lo = std::array::from_fn::<_, 2, _>(|i| {
            ((a[i].min(b[i]) - 0.5 - padding).ceil() as isize).clamp(0, (n - 1) as isize) as usize
        });
        let hi = std::array::from_fn::<_, 2, _>(|i| {
            ((a[i].max(b[i]) + 0.5 + padding).floor() as isize).clamp(0, (n - 1) as isize) as usize
        });
        for z in lo[1]..=hi[1] {
            for x in lo[0]..=hi[0] {
                meter.tick()?;
                if meter.work.is_multiple_of(4096) && cancel() {
                    return Ok(None);
                }
                if costs[z * n + x] != 0 {
                    continue;
                }
                let mut enter = 0_f64;
                let mut exit = 1_f64;
                for (axis, cell) in [x, z].into_iter().enumerate() {
                    let delta = b[axis] - a[axis];
                    let lower = cell as f64 - 0.5 - padding;
                    let upper = cell as f64 + 0.5 + padding;
                    if delta == 0. {
                        if a[axis] < lower || a[axis] > upper {
                            enter = 2.;
                            break;
                        }
                    } else {
                        let p = (lower - a[axis]) / delta;
                        let q = (upper - a[axis]) / delta;
                        enter = enter.max(p.min(q));
                        exit = exit.min(p.max(q));
                    }
                }
                if enter <= exit {
                    return Ok(Some(false));
                }
            }
        }
    }
    Ok(Some(true))
}
pub(crate) fn source_clear(
    points: &[[f32; 3]],
    costs: &[u32],
    n: usize,
    size: f32,
    cancel: &impl Fn() -> bool,
) -> Option<bool> {
    match clear(
        points,
        costs,
        n,
        size,
        &mut Meter {
            work: 0,
            maximum: 2_000_000,
        },
        cancel,
    ) {
        Ok(v) => v,
        Err(_) => Some(false),
    }
}
fn grade(points: &[[f32; 3]], maximum: f32) -> bool {
    points.windows(2).all(|w| {
        let run = (w[0][0] - w[1][0]).hypot(w[0][2] - w[1][2]);
        run > 1e-4 && (w[0][1] - w[1][1]).abs() <= maximum * run + 1e-5
    })
}
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(crate) fn build(
    points: &[[f32; 3]],
    layers: &Layers,
    costs: &[u32],
    n: usize,
    settings: Settings,
    policy: ShapingPolicy,
    cancel: &impl Fn() -> bool,
) -> Result<Option<Shaped>, Failure> {
    build_with_water(points, layers, costs, n, settings, policy, None, cancel)
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_with_water(
    points: &[[f32; 3]],
    layers: &Layers,
    costs: &[u32],
    n: usize,
    settings: Settings,
    policy: ShapingPolicy,
    water: Option<crate::road_water::Guide<'_>>,
    cancel: &impl Fn() -> bool,
) -> Result<Option<Shaped>, Failure> {
    if cancel() {
        return Ok(None);
    }
    if policy.validate().is_err()
        || !(2..=32768).contains(&points.len())
        || !(2..=1024).contains(&n)
        || costs.len() != n * n
        || points
            .iter()
            .flatten()
            .any(|v| !v.is_finite() || v.abs() > 10000.)
        || !settings.maximum_grade.is_finite()
        || settings.maximum_grade <= 0.
        || !settings.maximum_cut_fill.is_finite()
        || settings.maximum_cut_fill < 0.
    {
        return Err(Failure::Invalid);
    }
    let mut meter = Meter {
        work: 0,
        maximum: policy.maximum_work,
    };
    match clear(points, costs, n, layers.size as f32, &mut meter, cancel)? {
        None => return Ok(None),
        Some(false) => return Err(Failure::Invalid),
        Some(true) => {}
    }
    let mut p = points.to_vec();
    let mut moved = vec![false; p.len()];
    let mut audit = Audit {
        paths: 1,
        ..Default::default()
    };
    // Retain source alternating-index averaging and captured pass counts, but
    // backtrack each local move instead of discarding unrelated clear bends.
    for (iterations, axes) in [
        (policy.horizontal_iterations, [true, false, true]),
        (policy.vertical_iterations, [false, true, false]),
    ] {
        for _ in 0..iterations {
            for parity in 0..2 {
                for i in (1 + parity..p.len() - 1).step_by(2) {
                    if cancel() {
                        return Ok(None);
                    }
                    meter.tick()?;
                    let average: [f32; 3] = std::array::from_fn(|axis| {
                        if axes[axis] {
                            (p[i - 1][axis] + p[i][axis] + p[i][axis] + p[i + 1][axis]) * 0.25
                        } else {
                            p[i][axis]
                        }
                    });
                    if average == p[i] {
                        continue;
                    }
                    let mut accepted = false;
                    for step in 0..=8 {
                        meter.tick()?;
                        let fraction = 1. / (1u32 << step) as f32;
                        let candidate = std::array::from_fn(|axis| {
                            p[i][axis] + (average[axis] - p[i][axis]) * fraction
                        });
                        let local = [p[i - 1], candidate, p[i + 1]];
                        if !grade(&local, settings.maximum_grade) {
                            continue;
                        }
                        let h = crate::spawn::sample_height(
                            layers,
                            candidate[0] / layers.size as f32 + 0.5,
                            candidate[2] / layers.size as f32 + 0.5,
                        )
                        .map_err(|_| Failure::Invalid)?;
                        let minimum =
                            water.and_then(|w| w.minimum(layers, candidate[0], candidate[2]));
                        if (h <= 1. && minimum.is_none())
                            || minimum.is_some_and(|v| candidate[1] < v - 0.001)
                            || (h - candidate[1] - settings.terrain_offset).abs()
                                > settings.maximum_cut_fill
                        {
                            continue;
                        }
                        match clear(&local, costs, n, layers.size as f32, &mut meter, cancel)? {
                            None => return Ok(None),
                            Some(false) => continue,
                            Some(true) => {}
                        }
                        p[i] = candidate;
                        moved[i] = true;
                        audit.accepted_moves += 1;
                        if step > 0 {
                            audit.backtracked_moves += 1;
                        }
                        accepted = true;
                        break;
                    }
                    if !accepted {
                        audit.held_moves += 1;
                    }
                }
            }
        }
    }
    // Insert samples without dropping constrained knots or introducing shortcuts.
    let mut output = Vec::with_capacity(p.len());
    output.push(p[0]);
    for pair in p.windows(2) {
        if cancel() {
            return Ok(None);
        }
        meter.tick()?;
        let length = (pair[1][0] - pair[0][0]).hypot(pair[1][2] - pair[0][2]);
        let count = (length / policy.resample_distance).ceil().max(1.) as usize;
        if count > 65536 || output.len() + count > 65536 {
            return Err(Failure::Budget);
        }
        for j in 1..count {
            meter.tick()?;
            let t = j as f32 / count as f32;
            output.push(std::array::from_fn(|a| {
                pair[0][a] + (pair[1][a] - pair[0][a]) * t
            }));
        }
        output.push(pair[1]);
    }
    if !grade(&output, settings.maximum_grade) {
        return Err(Failure::Invalid);
    }
    match clear(&output, costs, n, layers.size as f32, &mut meter, cancel)? {
        None => return Ok(None),
        Some(false) => return Err(Failure::Invalid),
        Some(true) => {}
    }
    audit.moved_controls = moved.into_iter().filter(|v| *v).count();
    audit.output_controls = output.len();
    audit.work = meter.work;
    Ok(Some(Shaped {
        points: output,
        audit,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_cell_clearance_preserves_obstacles_registration_and_bounds() {
        let lattice = crate::road_costmap::CellLattice::new(1000., 10.).unwrap();
        let path = crate::path_geometry::Path::from_controls(
            &[[-95., 20., 5.], [95., 20., 5.]],
            10000,
            || false,
        )
        .unwrap()
        .unwrap();
        let mut costs = vec![101; 100 * 100];
        let check = |costs: &[u32], budget, cancelled| {
            curve_cell_cost_check(&path, costs, lattice, 1., budget, &move || cancelled)
        };
        assert!(check(&costs, 100000, false).unwrap().unwrap().clear);
        // Native cell [50,50] is centred at [+5,+5], not at the origin.
        // A blocker in that cell intersects the curve even at its far edge.
        costs[50 * 100 + 50] = 0;
        assert!(!check(&costs, 100000, false).unwrap().unwrap().clear);
        costs[50 * 100 + 50] = 101;
        costs[49 * 100 + 50] = 0;
        assert!(check(&costs, 100000, false).unwrap().unwrap().clear);
        assert!(check(&costs, 100000, true).unwrap().is_none());
        assert_eq!(check(&costs, 1, false).unwrap_err(), Failure::Budget);
        assert_eq!(
            check(&costs[..9999], 100000, false).unwrap_err(),
            Failure::Invalid
        );
        let edge = crate::path_geometry::Path::from_controls(
            &[[499.5, 20., 0.], [499.5, 20., 50.]],
            10000,
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(
            !curve_cell_cost_check(&edge, &vec![101; 10000], lattice, 1., 100000, &|| false)
                .unwrap()
                .unwrap()
                .clear
        );
    }
    #[test]
    fn ring_cost_checks_follow_the_curve_instead_of_only_its_chords() {
        let points = [
            [-100., 20., 0.],
            [0., 20., 0.],
            [0., 20., 100.],
            [100., 20., 100.],
        ];
        let path = crate::path_geometry::Path::from_controls(&points, 10000, || false)
            .unwrap()
            .unwrap();
        let mut costs = vec![101; 257 * 257];
        assert_eq!(
            ring_curve_cost_clear(&path, &costs, 1000, 1., 100000, &|| false).unwrap(),
            Some(true)
        );
        costs[149 * 257 + 127] = 0;
        assert_eq!(
            source_clear(&points, &costs, 257, 1000., &|| false),
            Some(true)
        );
        assert_eq!(
            ring_curve_cost_clear(&path, &costs, 1000, 1., 100000, &|| false).unwrap(),
            Some(false)
        );
        let audit = curve_cost_check(&path, &costs, 1000, 1., 100000, &|| false)
            .unwrap()
            .unwrap();
        assert!(!audit.clear && audit.work > 0 && audit.work <= 100000);
        assert!(
            ring_curve_cost_clear(&path, &costs, 1000, 1., 100000, &|| true)
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            ring_curve_cost_clear(&path, &costs, 1000, 1., 1, &|| false),
            Err(Failure::Budget)
        ));
    }
    #[test]
    fn ring_proposals_preserve_height_knots_costs_and_inputs_and_are_bounded() {
        let n = 33;
        let mut costs = vec![101; n * n];
        for z in 12..=20 {
            for x in 12..=20 {
                costs[z * n + x] = 0;
            }
        }
        let mut points = [(8., 8.), (24., 8.), (24., 24.), (8., 24.)]
            .into_iter()
            .enumerate()
            .map(|(i, (x, z))| {
                [
                    (x / 32. - 0.5) * 1000.,
                    20. + i as f32 * 100.,
                    (z / 32. - 0.5) * 1000.,
                ]
            })
            .collect::<Vec<_>>();
        points.push(points[0]);
        let before = points.clone();
        let policy = ShapingPolicy {
            horizontal_iterations: 4,
            vertical_iterations: 16,
            resample_distance: 7.5,
            maximum_work: 2_000_000,
        };
        let result = propose_ring_geometry(&points, &costs, n, 1000., policy, 0., &|| false)
            .unwrap()
            .unwrap();
        assert_eq!(result.points.first(), result.points.last());
        assert_eq!(
            source_clear(&result.points, &costs, n, 1000., &|| false),
            Some(true)
        );
        assert!(result.audit.accepted_moves > 0);
        assert!(result.audit.work <= policy.maximum_work);
        // Horizontal proposals must not silently enforce a provisional grade.
        // The caller fits these deliberately steep heights against real ground.
        for knot in &points {
            assert!(result.points.iter().any(|p| p[1] == knot[1]));
        }
        let replay = propose_ring_geometry(&points, &costs, n, 1000., policy, 0., &|| false)
            .unwrap()
            .unwrap();
        assert_eq!(result.points, replay.points);
        assert_eq!(result.audit.work, replay.audit.work);
        assert!(
            propose_ring_geometry(&points, &costs, n, 1000., policy, 0., &|| true)
                .unwrap()
                .is_none()
        );
        assert!(
            propose_ring_geometry(
                &points,
                &costs,
                n,
                1000.,
                ShapingPolicy {
                    maximum_work: 1,
                    ..policy
                },
                0.,
                &|| false
            )
            .is_err()
        );
        assert_eq!(points, before);
        points[1][0] = f32::NAN;
        assert!(propose_ring_geometry(&points, &costs, n, 1000., policy, 0., &|| false).is_err());
    }
    fn fixture() -> (Layers, Vec<u32>, Vec<[f32; 3]>, Settings, ShapingPolicy) {
        let (hr, sr) = crate::native_layers::dimensions(1000).unwrap();
        let layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![17038; hr * hr],
            biome: vec![0; sr * sr * 5],
            splat: vec![0; sr * sr * 8],
            topology: vec![16; sr * sr],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        let mut costs = vec![101; 33 * 33];
        for z in 13..=19 {
            for x in 13..=19 {
                costs[z * 33 + x] = 0;
            }
        }
        let points = [
            (3., 16.),
            (12., 16.),
            (12., 20.),
            (20., 20.),
            (20., 16.),
            (29., 16.),
        ]
        .map(|(x, z)| [(x / 32. - 0.5) * 1000., 20., (z / 32. - 0.5) * 1000.])
        .to_vec();
        let settings = Settings {
            seed: 1,
            width: 10.,
            shoulder: 8.,
            maximum_grade: 0.2,
            maximum_cut_fill: 8.,
            terrain_offset: -0.125,
            road_topology: 2048,
            roadside_topology: 4096,
            protected_topology: 1024,
            splat_channel: 7,
        };
        let policy = ShapingPolicy {
            horizontal_iterations: 4,
            vertical_iterations: 16,
            resample_distance: 7.5,
            maximum_work: 2_000_000,
        };
        (layers, costs, points, settings, policy)
    }
    fn bends(points: &[[f32; 3]]) -> f32 {
        points
            .windows(3)
            .map(|p| {
                let a = [p[1][0] - p[0][0], p[1][2] - p[0][2]];
                let b = [p[2][0] - p[1][0], p[2][2] - p[1][2]];
                1. - (a[0] * b[0] + a[1] * b[1]) / (a[0].hypot(a[1]) * b[0].hypot(b[1]))
            })
            .sum()
    }
    #[test]
    fn constrained_moves_smooth_safe_bends_without_cutting_the_blocked_rectangle() {
        let (layers, costs, points, settings, policy) = fixture();
        let before = points.clone();
        let source = crate::path_geometry::build(
            &points,
            crate::path_geometry::Settings {
                horizontal_iterations: 4,
                vertical_iterations: 16,
                resample_distance: 7.5,
                maximum_work: 2_000_000,
            },
            None,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            source_clear(source.points(), &costs, 33, 1000., &|| false),
            Some(false)
        );
        let result = build(&points, &layers, &costs, 33, settings, policy, &|| false)
            .unwrap()
            .unwrap();
        assert_eq!(result.points.first(), points.first());
        assert_eq!(result.points.last(), points.last());
        assert_eq!(
            source_clear(&result.points, &costs, 33, 1000., &|| false),
            Some(true)
        );
        assert!(grade(&result.points, settings.maximum_grade));
        assert!(bends(&result.points) < bends(&points));
        assert!(result.audit.accepted_moves > 0 && result.audit.backtracked_moves > 0);
        assert!(result.audit.work <= policy.maximum_work);
        assert_eq!(points, before);
        let replay = build(&points, &layers, &costs, 33, settings, policy, &|| false)
            .unwrap()
            .unwrap();
        assert_eq!(result.points, replay.points);
    }
    #[test]
    fn cancellation_budget_and_invalid_input_never_return_partial_paths() {
        let (layers, costs, points, settings, mut policy) = fixture();
        let original = points.clone();
        let calls = std::cell::Cell::new(0);
        assert!(
            build(&points, &layers, &costs, 33, settings, policy, &|| {
                calls.set(calls.get() + 1);
                calls.get() > 20
            })
            .unwrap()
            .is_none()
        );
        policy.maximum_work = 1024;
        policy.horizontal_iterations = 64;
        assert!(matches!(
            build(&points, &layers, &costs, 33, settings, policy, &|| false),
            Err(Failure::Budget)
        ));
        assert!(matches!(
            build(
                &points,
                &layers,
                &costs[..20],
                33,
                settings,
                policy,
                &|| false
            ),
            Err(Failure::Invalid)
        ));
        assert_eq!(points, original);
        assert_eq!(layers.height[0], 17038);
    }
}
