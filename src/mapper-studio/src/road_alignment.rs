//! Bounded geometry proposals, never certificates of road suitability.
//! Selection, source painting and complete mesh/route audits follow each trial.
use crate::road_refinement::AlignmentWitness;
use papyrus_terrain::{
    path_terrain,
    road_network::{Destination, Report},
};

pub struct Proposal {
    pub road: usize,
    pub before: Vec<[f32; 3]>,
    pub after: Vec<[f32; 3]>,
    pub work: usize,
    pub contact_controls: usize,
    pub circular: bool,
    /// Fixed authored endpoint whose generated neighboring control guides a
    /// tangent repair. The endpoint itself remains bitwise unchanged.
    pub endpoint_tangent: Option<usize>,
    pub portal_heading: Option<crate::road_portal_heading::Audit>,
    pub portal_mesh: Option<serde_json::Value>,
}
/// Geometry only, scoped to one bounded generation session. Exact source
/// controls are checked on replay, then fresh painting/fitting and audits apply.
/// It is never a cached fitting result or a map acceptance certificate.
#[derive(Clone)]
pub(crate) struct Hint {
    pub road: usize,
    from: Destination,
    to: Destination,
    width: u32,
    hierarchy: u32,
    before: Vec<[f32; 3]>,
    after: Vec<[f32; 3]>,
}
pub(crate) struct Replay {
    pub proposal: Option<Proposal>,
    pub work: usize,
    pub reason: &'static str,
}
fn exact_points(a: &[[f32; 3]], b: &[[f32; 3]]) -> bool {
    a.len() == b.len()
        && a.iter()
            .flatten()
            .zip(b.iter().flatten())
            .all(|(a, b)| a.to_bits() == b.to_bits())
}
impl Hint {
    pub fn references_monument(&self, monument: u64) -> bool {
        [self.from, self.to].iter().any(|d| {
            matches!(d,
            Destination::Monument { monument: id, .. } if *id == monument)
        })
    }
    pub fn capture(report: &Report, road: usize) -> Option<Self> {
        let r = report.roads.get(road)?;
        if !(2..=32768).contains(&r.points.len())
            || ![r.from, r.to]
                .iter()
                .any(|d| matches!(d, Destination::Monument { .. }))
        {
            return None;
        }
        let first = r.points.first()?;
        let last = r.points.last()?;
        if (first[0] - last[0]).hypot(first[2] - last[2]) < 0.1 {
            return None;
        }
        let family = report
            .families
            .iter()
            .find(|f| road >= f.first_road && road < f.first_road + f.road_count)?;
        Some(Self {
            road,
            from: r.from,
            to: r.to,
            width: family.width.to_bits(),
            hierarchy: family.hierarchy,
            before: r.points.clone(),
            after: r.points.clone(),
        })
    }
    pub fn candidate(&self, report: &Report) -> Option<Self> {
        let r = report.roads.get(self.road)?;
        if r.from != self.from
            || r.to != self.to
            || r.points.len() != self.before.len()
            || exact_points(&r.points, &self.before)
        {
            return None;
        }
        Some(Self {
            after: r.points.clone(),
            ..self.clone()
        })
    }
    pub fn collect(hints: &mut Vec<Self>, next: Self) {
        if let Some(old) = hints.iter_mut().find(|h| {
            h.from == next.from
                && h.to == next.to
                && h.width == next.width
                && h.hierarchy == next.hierarchy
                && exact_points(&h.before, &next.before)
        }) {
            *old = next;
        } else if hints.len() < 4
            && hints.iter().map(|h| h.before.len()).sum::<usize>() + next.before.len() <= 131072
        {
            hints.push(next);
        }
    }
    pub fn replay(
        &self,
        report: &Report,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Replay>, Failure> {
        if cancelled() {
            return Ok(None);
        }
        let total = report
            .roads
            .iter()
            .try_fold(0usize, |n, r| n.checked_add(r.points.len()))
            .ok_or("Hint work overflow")?;
        let joins = report
            .entrance_joins
            .iter()
            .try_fold(0usize, |n, j| n.checked_add(j.points.len()))
            .ok_or("Hint work overflow")?;
        let work = report
            .roads
            .len()
            .checked_mul(2)
            .and_then(|n| n.checked_add(joins))
            .and_then(|n| n.checked_add(96))
            .and_then(|n| n.checked_mul(self.before.len()))
            .and_then(|n| n.checked_add(total.checked_mul(8)?))
            .filter(|&n| n <= maximum_work)
            .ok_or("Hint work budget reached")?;
        let reject = |reason| {
            Ok(Some(Replay {
                proposal: None,
                work,
                reason,
            }))
        };
        let Some((index, road)) = report.roads.iter().enumerate().find(|(_, r)| {
            r.from == self.from && r.to == self.to && exact_points(&r.points, &self.before)
        }) else {
            return reject("road_changed");
        };
        let Some(family) = report
            .families
            .iter()
            .find(|f| index >= f.first_road && index < f.first_road + f.road_count)
        else {
            return reject("family_missing");
        };
        if family.width.to_bits() != self.width || family.hierarchy != self.hierarchy {
            return reject("family_changed");
        }
        if self.after.len() != road.points.len()
            || self.after.iter().flatten().any(|v| !v.is_finite())
        {
            return reject("invalid_controls");
        }
        let Some(pins) = attachment_pins(report, index, &road.points, false, &cancelled)? else {
            return Ok(None);
        };
        if pins
            .iter()
            .enumerate()
            .any(|(i, p)| *p && !exact_points(&self.before[i..=i], &self.after[i..=i]))
        {
            return reject("attachment_changed");
        }
        Ok(Some(Replay {
            proposal: Some(Proposal {
                road: index,
                before: road.points.clone(),
                after: self.after.clone(),
                work,
                contact_controls: 0,
                circular: false,
                endpoint_tangent: None,
                portal_heading: None,
                portal_mesh: None,
            }),
            work,
            reason: "applied",
        }))
    }
}
pub(crate) struct ContourAttempt {
    pub proposal: Option<Proposal>,
    pub report: papyrus_terrain::road_entrance_contour::Report,
    pub work: usize,
}
/// A fixed-count tail only: arbitrary graph attachments are never renumbered.
/// The source shaping profile is supplied by the installation-bound caller.
#[allow(clippy::too_many_arguments)]
pub(crate) fn propose_contour(
    report: &Report,
    witness: AlignmentWitness,
    grid: path_terrain::Grid<'_>,
    topology: &[i32],
    surface_resolution: usize,
    protected_topology: i32,
    maximum_grade: f64,
    shaping: papyrus_terrain::road_network::ShapingPolicy,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<ContourAttempt>, Failure> {
    use papyrus_terrain::road_entrance_contour as contour;
    if cancelled() {
        return Ok(None);
    }
    let road = report
        .roads
        .get(witness.road)
        .ok_or("Missing contour road")?;
    let n = road.points.len();
    if !matches!(witness.kind, crate::road_refinement::AlignmentKind::Grade)
        || !(10..=512).contains(&n)
        || !matches!(road.to, Destination::Monument { .. })
        || !matches!(road.from, Destination::Network { .. })
    {
        return Ok(None);
    }
    let start = n - 7;
    let closest = road
        .points
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            let d = |p: &[f32; 3]| {
                (f64::from(p[0]) - witness.position[0]).hypot(f64::from(p[2]) - witness.position[1])
            };
            d(a).total_cmp(&d(b))
        })
        .unwrap()
        .0;
    if closest <= start {
        return Ok(None);
    }
    let mut work = report
        .roads
        .iter()
        .try_fold(0_usize, |sum, r| sum.checked_add(r.points.len() * 8))
        .ok_or("Contour work overflow")?;
    spend(
        &mut work,
        report
            .roads
            .len()
            .checked_mul(n * 4)
            .ok_or("Contour work overflow")?,
        maximum_work,
    )?;
    for join in &report.entrance_joins {
        spend(
            &mut work,
            join.points
                .len()
                .checked_mul(n)
                .ok_or("Contour work overflow")?,
            maximum_work,
        )?;
    }
    let Some(pins) = attachment_pins(report, witness.road, &road.points, false, &cancelled)? else {
        return Ok(None);
    };
    if pins[start + 1..n - 1].iter().any(|v| *v) {
        return Ok(None);
    }
    let Some(result) = contour::propose(
        contour::Input {
            controls: &road.points,
            pinned: &pins,
            tail_start: start,
            grid,
            topology,
            surface_resolution,
            protected_topology,
        },
        contour::Settings {
            maximum_grade,
            search_grade: maximum_grade * 0.5,
            radius_cells: 48,
            maximum_expansions: 12000,
            maximum_work: maximum_work
                .checked_sub(work)
                .filter(|v| *v > 0)
                .ok_or("Contour budget reached")?,
            shaping: papyrus_terrain::path_geometry::Settings {
                horizontal_iterations: shaping.horizontal_iterations,
                vertical_iterations: shaping.vertical_iterations,
                resample_distance: shaping.resample_distance,
                maximum_work: shaping.maximum_work,
            },
        },
        &cancelled,
    )
    .map_err(|e| format!("Contour guidance: {e:?}"))?
    else {
        return Ok(None);
    };
    spend(&mut work, result.report.work, maximum_work)?;
    let proposal = result.proposal.map(|p| Proposal {
        road: witness.road,
        before: road.points.clone(),
        after: p.controls,
        work,
        contact_controls: 0,
        circular: false,
        endpoint_tangent: None,
        portal_heading: None,
        portal_mesh: None,
    });
    Ok(Some(ContourAttempt {
        proposal,
        report: result.report,
        work,
    }))
}
pub const OFFSETS: [f64; 6] = [0.5, -0.5, 1., -1., 2., -2.];
/// Relax a folded bend using the source's alternating smoothing sweeps.
/// Preserve control indices and graph attachments. Complete world audits follow.
pub(crate) fn propose_curvature(
    report: &Report,
    witness: AlignmentWitness,
    grid: path_terrain::Grid<'_>,
    branch: f64,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Proposal>, Failure> {
    if cancelled() {
        return Ok(None);
    }
    let road = report
        .roads
        .get(witness.road)
        .ok_or("Curvature road missing")?;
    let family = report
        .families
        .iter()
        .find(|f| {
            witness.road >= f.first_road && witness.road < f.first_road.saturating_add(f.road_count)
        })
        .ok_or("Curvature family missing")?;
    let branch = OFFSETS
        .iter()
        .position(|&value| value == branch)
        .ok_or("Invalid curvature branch")?;
    let points = &road.points;
    if !(7..=32768).contains(&points.len())
        || !family.width.is_finite()
        || !(0.1..=128.).contains(&family.width)
        || witness.position.iter().any(|v| !v.is_finite())
        || points
            .iter()
            .flatten()
            .any(|v| !v.is_finite() || v.abs() > 10000.)
    {
        return Err("Invalid curvature controls".into());
    }
    let circular = road.from == Destination::Loop
        && road.to == Destination::Loop
        && points.first() == points.last();
    let n = points.len() - usize::from(circular);
    let total = report
        .roads
        .iter()
        .try_fold(0usize, |sum, r| sum.checked_add(r.points.len()))
        .ok_or("Curvature work overflow")?;
    let endpoints = report
        .entrance_joins
        .iter()
        .try_fold(report.roads.len() * 2, |sum, j| {
            sum.checked_add(j.points.len())
        })
        .ok_or("Curvature attachment overflow")?;
    let mut work = (endpoints + 96)
        .checked_mul(points.len())
        .and_then(|value| value.checked_add(total.checked_mul(8)?))
        .filter(|&value| value <= maximum_work)
        .ok_or("Curvature work budget reached")?;
    let Some(pins) = attachment_pins(report, witness.road, points, circular, &cancelled)? else {
        return Ok(None);
    };
    let center = points[..n]
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            let distance = |p: &[f32; 3]| {
                (f64::from(p[0]) - witness.position[0]).hypot(f64::from(p[2]) - witness.position[1])
            };
            distance(a).total_cmp(&distance(b))
        })
        .unwrap()
        .0;
    let mut arc = vec![0.; points.len()];
    for i in 1..points.len() {
        arc[i] = arc[i - 1]
            + f64::from(points[i][0] - points[i - 1][0])
                .hypot(f64::from(points[i][2] - points[i - 1][2]));
    }
    let length = arc[points.len() - 1];
    if length <= 1e-6 {
        return Err("Degenerate curvature path".into());
    }
    let weights = (0..n)
        .map(|i| {
            let distance = (arc[i] - arc[center]).abs();
            let distance = if circular {
                distance.min(length - distance)
            } else {
                distance
            };
            if pins[i] {
                0.
            } else {
                (1. - (distance / (f64::from(family.width) * 12.)).min(1.)).powi(3) as f32
            }
        })
        .collect::<Vec<_>>();
    let mut after = points.clone();
    for _ in 0..(4usize << branch) {
        for parity in 0..2 {
            let start = usize::from(!circular) + parity;
            for i in (start..points.len() - 1).step_by(2) {
                if cancelled() {
                    return Ok(None);
                }
                spend(&mut work, 1, maximum_work)?;
                if weights[i] == 0. {
                    continue;
                }
                let previous = if i == 0 { n - 1 } else { i - 1 };
                for axis in [0, 2] {
                    let average = (after[previous][axis]
                        + after[i][axis]
                        + after[i][axis]
                        + after[i + 1][axis])
                        * 0.25;
                    after[i][axis] += (average - after[i][axis]) * weights[i];
                }
                if circular && i == 0 {
                    after[n] = after[0]
                }
            }
        }
    }
    let mut changed = false;
    for i in 0..n {
        if cancelled() {
            return Ok(None);
        }
        spend(&mut work, 1, maximum_work)?;
        if !same_xz(after[i], points[i]) {
            after[i][1] = path_terrain::sample_height(grid, after[i])
                .map_err(|e| format!("Curvature height: {e:?}"))?;
            changed = true;
        }
    }
    if !changed {
        return Err(Failure::PinnedAttachment);
    }
    if circular {
        after[n] = after[0]
    }
    Ok(Some(Proposal {
        road: witness.road,
        before: points.clone(),
        after,
        work,
        contact_controls: 0,
        circular,
        endpoint_tangent: None,
        portal_heading: None,
        portal_mesh: None,
    }))
}
#[derive(Debug)]
pub enum Failure {
    PinnedAttachment,
    Other(String),
}
impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self::Other(message)
    }
}
impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        Self::Other(message.into())
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PinnedAttachment => f.write_str("Alignment witness lies at a pinned attachment"),
            Self::Other(message) => f.write_str(message),
        }
    }
}
#[cfg(test)]
pub(crate) fn propose(
    report: &Report,
    witness: AlignmentWitness,
    grid: path_terrain::Grid<'_>,
    offset: f64,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Proposal>, String> {
    propose_with_retained(report, witness, grid, offset, maximum_work, None, cancelled)
        .map_err(|e| e.to_string())
}
struct Rows<'a> {
    samples: &'a [usize],
    ranges: Vec<(usize, usize, usize)>,
}
impl<'a> Rows<'a> {
    fn new(
        samples: &'a [usize],
        n: usize,
        work: &mut usize,
        maximum: usize,
    ) -> Result<Self, String> {
        spend(work, samples.len(), maximum)?;
        if samples.len() > 262144
            || samples.windows(2).any(|p| p[0] >= p[1])
            || samples.last().is_some_and(|i| *i >= n.saturating_mul(n))
        {
            return Err("Invalid retained alignment supports".into());
        }
        let mut ranges = Vec::new();
        for (i, &index) in samples.iter().enumerate() {
            let row = index / n;
            if let Some((last, _, end)) = ranges.last_mut()
                && *last == row
            {
                *end = i + 1;
                continue;
            }
            ranges.push((row, i, i + 1));
        }
        Ok(Self { samples, ranges })
    }
    fn near(
        &self,
        p: [f32; 3],
        grid: path_terrain::Grid<'_>,
        radius: f64,
        work: &mut usize,
        maximum: usize,
    ) -> Result<bool, String> {
        let scale = (grid.resolution - 1) as f64 / f64::from(grid.size);
        let index = |value: f64| {
            ((value + f64::from(grid.size) * 0.5) * scale).clamp(0., (grid.resolution - 1) as f64)
        };
        let [x0, x1, z0, z1] = [
            index(f64::from(p[0]) - radius).floor() as usize,
            index(f64::from(p[0]) + radius).ceil() as usize,
            index(f64::from(p[2]) - radius).floor() as usize,
            index(f64::from(p[2]) + radius).ceil() as usize,
        ];
        spend(work, self.ranges.len().max(1).ilog2() as usize + 1, maximum)?;
        let start = self.ranges.partition_point(|r| r.0 < z0);
        for &(z, a, b) in &self.ranges[start..] {
            if z > z1 {
                break;
            }
            spend(work, (b - a).max(1).ilog2() as usize + 2, maximum)?;
            let slice = &self.samples[a..b];
            let i = slice.partition_point(|&i| i < z * grid.resolution + x0);
            if slice.get(i).is_some_and(|&i| i <= z * grid.resolution + x1) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
fn spend(work: &mut usize, n: usize, maximum: usize) -> Result<(), String> {
    *work = work
        .checked_add(n)
        .filter(|w| *w <= maximum)
        .ok_or("Alignment work budget reached")?;
    Ok(())
}
fn same_xz(a: [f32; 3], b: [f32; 3]) -> bool {
    a[0].to_bits() == b[0].to_bits() && a[2].to_bits() == b[2].to_bits()
}
fn pin(pins: &mut [bool], point: usize, circular: bool) {
    let n = pins.len();
    if circular {
        for d in 0..5 {
            pins[(point % n + n - 2 + d) % n] = true;
        }
    } else {
        for p in &mut pins[point.saturating_sub(2)..(point + 3).min(n)] {
            *p = true;
        }
    }
}
fn attachment_pins(
    report: &Report,
    road_index: usize,
    points: &[[f32; 3]],
    circular: bool,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Vec<bool>>, Failure> {
    let n = points.len() - usize::from(circular);
    let road = &report.roads[road_index];
    let mut pins = vec![false; n];
    if !circular {
        for (index, destination) in [(0, road.from), (n - 1, road.to)] {
            if matches!(destination, Destination::Monument { .. }) {
                // Native layout anchors the authored position, not an authored
                // tangent. The generated approach may turn around steep fixed
                // terrain while the portal itself remains exactly in place.
                pins[index] = true;
            } else {
                pin(&mut pins, index, false);
            }
        }
    }
    for (other_index, other) in report.roads.iter().enumerate() {
        if cancelled() {
            return Ok(None);
        }
        for destination in [other.from, other.to] {
            if let Destination::Network { road, point } = destination
                && road == road_index
            {
                if point >= points.len() {
                    return Err("Alignment attachment index invalid".into());
                }
                pin(&mut pins, point, circular);
            }
        }
        // A closed path's duplicated seam is not an authored socket.
        if other_index == road_index {
            continue;
        }
        for endpoint in [other.points.first(), other.points.last()]
            .into_iter()
            .flatten()
        {
            for (i, p) in points[..n].iter().enumerate() {
                if same_xz(*endpoint, *p) {
                    pin(&mut pins, i, circular);
                }
            }
        }
    }
    for join in &report.entrance_joins {
        for endpoint in &join.points {
            for (i, p) in points[..n].iter().enumerate() {
                if same_xz(*endpoint, *p) {
                    pin(&mut pins, i, circular);
                }
            }
        }
    }
    Ok(Some(pins))
}
pub(crate) fn propose_with_retained(
    report: &Report,
    witness: AlignmentWitness,
    grid: path_terrain::Grid<'_>,
    offset_widths: f64,
    maximum_work: usize,
    retained: Option<&[usize]>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Proposal>, Failure> {
    propose_inner(
        report,
        witness,
        grid,
        offset_widths,
        maximum_work,
        retained,
        None,
        cancelled,
    )
}
#[allow(clippy::too_many_arguments)]
fn propose_inner(
    report: &Report,
    witness: AlignmentWitness,
    grid: path_terrain::Grid<'_>,
    offset_widths: f64,
    maximum_work: usize,
    retained: Option<&[usize]>,
    forced_portal_delta: Option<[f64; 2]>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Proposal>, Failure> {
    if cancelled() {
        return Ok(None);
    }
    let road = report
        .roads
        .get(witness.road)
        .ok_or("Alignment road missing")?;
    // A broad retained-contact proposal is for clearance along an existing
    // strip. Keep grade repairs local, including their previously checked frame.
    let retained =
        retained.filter(|_| witness.kind == crate::road_refinement::AlignmentKind::Clearance);
    let points = &road.points;
    let family = report
        .families
        .iter()
        .find(|f| {
            witness.road >= f.first_road && witness.road < f.first_road.saturating_add(f.road_count)
        })
        .ok_or("Alignment family missing")?;
    let width = f64::from(family.width);
    if points.len() < 7
        || !width.is_finite()
        || !(0.1..=128.).contains(&width)
        || !OFFSETS.contains(&offset_widths)
        || witness.position.iter().any(|v| !v.is_finite())
        || grid.resolution < 2
        || !grid.size.is_finite()
        || grid.size <= 0.
    {
        return Err("Invalid alignment inputs".into());
    }
    let circular = road.from == Destination::Loop
        && road.to == Destination::Loop
        && points
            .first()
            .unwrap()
            .iter()
            .zip(points.last().unwrap())
            .all(|(a, b)| a.to_bits() == b.to_bits());
    let n = points.len() - usize::from(circular);
    let total = report
        .roads
        .iter()
        .try_fold(0_usize, |sum, r| sum.checked_add(r.points.len()))
        .ok_or("Alignment work overflow")?;
    let endpoints = report
        .entrance_joins
        .iter()
        .try_fold(
            report
                .roads
                .len()
                .checked_mul(2)
                .ok_or("Alignment work overflow")?,
            |sum, j| sum.checked_add(j.points.len()),
        )
        .ok_or("Alignment work overflow")?;
    let mut work = endpoints
        .checked_add(96)
        .and_then(|v| v.checked_mul(points.len()))
        .and_then(|v| v.checked_add(total.checked_mul(8)?))
        .filter(|&w| w <= maximum_work)
        .ok_or("Alignment work budget reached")?;
    let Some(pins) = attachment_pins(report, witness.road, points, circular, &cancelled)? else {
        return Ok(None);
    };
    let mut arc = vec![0.; points.len()];
    for i in 1..points.len() {
        if cancelled() {
            return Ok(None);
        }
        if points[i - 1]
            .iter()
            .chain(&points[i])
            .any(|v| !v.is_finite())
        {
            return Err("Invalid alignment control".into());
        }
        arc[i] = arc[i - 1]
            + (f64::from(points[i][0]) - f64::from(points[i - 1][0]))
                .hypot(f64::from(points[i][2]) - f64::from(points[i - 1][2]));
    }
    let length = *arc.last().unwrap();
    if length <= 1e-6 {
        return Err("Degenerate alignment path".into());
    }
    let mut center = points[..n]
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            let distance = |p: &[f32; 3]| {
                (f64::from(p[0]) - witness.position[0]).hypot(f64::from(p[2]) - witness.position[1])
            };
            distance(a).total_cmp(&distance(b))
        })
        .unwrap()
        .0;
    let mut endpoint_tangent = None;
    if pins[center] {
        // A mesh witness beside an authored portal can be closest to the
        // immutable endpoint. Repair its approach heading, not the portal.
        // Only the immediate generated neighbor may become the proposal
        // center. Never jump over another attachment's protected controls.
        let adjacent = if !circular
            && center == 0
            && matches!(road.from, Destination::Monument { .. })
        {
            Some(1)
        } else if !circular && center == n - 1 && matches!(road.to, Destination::Monument { .. }) {
            Some(n - 2)
        } else {
            None
        };
        let distance = (f64::from(points[center][0]) - witness.position[0])
            .hypot(f64::from(points[center][2]) - witness.position[1]);
        match adjacent {
            Some(next) if !pins[next] && distance <= width => {
                endpoint_tangent = Some(center);
                center = next;
            }
            _ => return Err(Failure::PinnedAttachment),
        }
    }
    let portal_choice = if let Some(endpoint) = endpoint_tangent {
        let before = points[center.saturating_sub(1)];
        let after = points[(center + 1).min(n - 1)];
        let dx = f64::from(after[0]) - f64::from(before[0]);
        let dz = f64::from(after[2]) - f64::from(before[2]);
        let norm = dx.hypot(dz);
        if norm <= 1e-6 {
            return Err("Degenerate alignment heading".into());
        }
        let delta = [
            dz / norm * offset_widths * width,
            -dx / norm * offset_widths * width,
        ];
        if let Some(delta) = forced_portal_delta {
            let limit = offset_widths.abs() * width;
            if delta.iter().any(|v| !v.is_finite())
                || delta[0].hypot(delta[1]) > limit + 16. * f64::EPSILON * width.max(1.)
            {
                return Err("Portal displacement exceeds branch allowance".into());
            }
            Some((delta, None))
        } else {
            let Some(choice) = crate::road_portal_heading::choose(
                points[endpoint],
                points[center],
                delta,
                width,
                offset_widths.signum() * if endpoint == 0 { -1. } else { 1. },
                offset_widths.abs() * width,
                grid,
                &mut work,
                maximum_work,
                &cancelled,
            )?
            else {
                return Ok(None);
            };
            Some((choice.delta, Some(choice.audit)))
        }
    } else {
        None
    };
    let signed = |i: usize| {
        let d = arc[i] - arc[center];
        if circular {
            let f = d.rem_euclid(length);
            if f > length * 0.5 { f - length } else { f }
        } else {
            d
        }
    };
    let mut limits = if circular {
        [length * 0.5; 2]
    } else {
        [arc[center], length - arc[center]]
    };
    for (i, &pinned) in pins.iter().enumerate() {
        if pinned {
            let d = signed(i);
            let side = usize::from(d >= 0.);
            limits[side] = limits[side].min(d.abs());
        }
    }
    let mut contact = vec![false; n];
    if let Some(samples) = retained {
        let rows = Rows::new(samples, grid.resolution, &mut work, maximum_work)?;
        // Control-sampled support neighborhoods guide a proposal; they do not
        // replace the exact deformed-mesh footprint or final clearance audit.
        for (i, p) in points[..n].iter().enumerate() {
            if cancelled() {
                return Ok(None);
            }
            contact[i] = rows.near(*p, grid, width, &mut work, maximum_work)?;
        }
    }
    let mut core = [0.; 2];
    if contact[center] {
        for (side, dir) in [(0, -1_isize), (1, 1)] {
            let mut at = center;
            for _ in 1..n {
                let next = if circular {
                    (at as isize + dir).rem_euclid(n as isize) as usize
                } else {
                    let j = at as isize + dir;
                    if j < 0 || j >= n as isize {
                        break;
                    }
                    j as usize
                };
                let d = signed(next);
                if !contact[next] || pins[next] || usize::from(d >= 0.) != side {
                    break;
                }
                core[side] = d.abs();
                at = next;
            }
        }
    }
    let support = width * 12.;
    let whole_loop = circular && contact.iter().all(|&v| v) && !pins.iter().any(|&v| v);
    let mut result = points.clone();
    let mut changed = 0;
    for i in 0..n {
        if cancelled() {
            return Ok(None);
        }
        if pins[i] {
            continue;
        }
        let d = signed(i);
        let side = usize::from(d >= 0.);
        let distance = d.abs();
        if !whole_loop && distance >= limits[side] && distance > 0. {
            continue;
        }
        let plateau = core[side].min((limits[side] - support).max(0.));
        let taper = support.min(limits[side] - plateau);
        if taper <= 0. {
            continue;
        }
        let t = if whole_loop {
            0.
        } else {
            ((distance - plateau).max(0.) / taper).clamp(0., 1.)
        };
        if t >= 1. {
            continue;
        }
        let fade = (1. - t * t).powi(3);
        let frame = if retained.is_some() { i } else { center };
        let before = points[if frame == 0 && circular {
            n - 1
        } else {
            frame.saturating_sub(1)
        }];
        let after = points[if frame + 1 == n && circular {
            0
        } else {
            (frame + 1).min(n - 1)
        }];
        let dx = f64::from(after[0]) - f64::from(before[0]);
        let dz = f64::from(after[2]) - f64::from(before[2]);
        let norm = dx.hypot(dz);
        if norm <= 1e-6 {
            return Err("Degenerate alignment heading".into());
        }
        let offset = offset_widths * width * fade;
        let delta = portal_choice
            .as_ref()
            .map_or([dz / norm * offset, -dx / norm * offset], |choice| {
                choice.0.map(|v| v * fade)
            });
        let x = (f64::from(points[i][0]) + delta[0]) as f32;
        let z = (f64::from(points[i][2]) + delta[1]) as f32;
        if !x.is_finite()
            || !z.is_finite()
            || x.abs() > grid.size * 0.5
            || z.abs() > grid.size * 0.5
        {
            return Err("Alignment proposal leaves the terrain bounds".into());
        }
        let y = path_terrain::sample_height(grid, [x, 0., z])
            .map_err(|e| format!("Alignment height: {e:?}"))?;
        result[i] = [x, y, z];
        changed += usize::from(!same_xz(points[i], result[i]));
    }
    if changed == 0 {
        return Err("Alignment proposal did not move geometry".into());
    }
    if circular {
        result[n] = result[0];
    }
    Ok(Some(Proposal {
        road: witness.road,
        before: points.clone(),
        after: result,
        work,
        contact_controls: contact.iter().filter(|&&v| v).count(),
        circular,
        endpoint_tangent,
        portal_heading: portal_choice.and_then(|choice| choice.1),
        portal_mesh: None,
    }))
}

pub(crate) struct PortalEvaluation {
    /// None rejects folded/degenerate/empty geometry without a numeric score.
    pub excess: Option<f64>,
    pub clearance_passed: bool,
    pub work: usize,
    pub detail: serde_json::Value,
}
/// Native measurements select geometry only. Every winner still passes the
/// ordinary bound-plan transaction and full pristine-base composition.
#[allow(clippy::too_many_arguments)]
pub(crate) fn propose_with_native(
    report: &Report,
    witness: AlignmentWitness,
    grid: path_terrain::Grid<'_>,
    offset: f64,
    maximum_work: usize,
    retained: Option<&[usize]>,
    enabled: bool,
    rejected: &[Vec<[f32; 3]>],
    mut evaluate: impl FnMut(
        &[[f32; 3]],
        usize,
        usize,
    ) -> Result<Option<PortalEvaluation>, crate::road_portal_mesh::Failure>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Proposal>, Failure> {
    if rejected.len() > 12
        || rejected
            .iter()
            .try_fold(0usize, |sum, p| sum.checked_add(p.len()))
            .is_none_or(|sum| sum > 131072)
    {
        return Err("Rejected portal poses exceed allowance".into());
    }
    let Some(initial) = propose_with_retained(
        report,
        witness,
        grid,
        offset,
        maximum_work,
        retained,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let Some(endpoint) = initial.endpoint_tangent.filter(|_| enabled) else {
        return Ok(Some(initial));
    };
    let points = &initial.before;
    let neighbor = if endpoint == 0 { 1 } else { points.len() - 2 };
    let width = f64::from(
        report
            .families
            .iter()
            .find(|f| witness.road >= f.first_road && witness.road < f.first_road + f.road_count)
            .ok_or("Missing portal family")?
            .width,
    );
    let before = points[neighbor.saturating_sub(1)];
    let after = points[(neighbor + 1).min(points.len() - 1)];
    let dx = f64::from(after[0]) - f64::from(before[0]);
    let dz = f64::from(after[2]) - f64::from(before[2]);
    let norm = dx.hypot(dz);
    if norm <= 1e-6 {
        return Err("Degenerate portal heading".into());
    }
    let baseline = [dz / norm * offset * width, -dx / norm * offset * width];
    let d = [
        f64::from(points[neighbor][0]) - f64::from(points[endpoint][0]),
        f64::from(points[neighbor][2]) - f64::from(points[endpoint][2]),
    ];
    let length = d[0].hypot(d[1]);
    let angle = d[1].atan2(d[0]);
    let sign = offset.signum() * if endpoint == 0 { -1. } else { 1. };
    let mut work = initial.work;
    let initial_work = initial.work;
    let initial_turn = initial.portal_heading.as_ref().and_then(|a| a.turn_degrees);
    let coarse_guidance =
        serde_json::to_value(&initial.portal_heading).map_err(|e| e.to_string())?;
    let mut preferred = Some(initial);
    let mut best: Option<(f64, Proposal, Option<i32>, bool)> = None;
    let mut attempts = Vec::new();
    let mut exhausted = false;
    let precision = 8. * f64::from(f32::EPSILON) * f64::from(grid.vertical_span).max(1.);
    let baseline_vector = [d[0] + baseline[0], d[1] + baseline[1]];
    let baseline_turn = ((d[0] * baseline_vector[1] - d[1] * baseline_vector[0])
        .atan2(d[0] * baseline_vector[0] + d[1] * baseline_vector[1])
        .to_degrees()
        * sign)
        .round()
        .clamp(1., 120.) as i32;
    let guided = initial_turn.is_some();
    let mut directions = if guided {
        [0, 0, 0, 0, 15, 45, 75, 120]
    } else {
        [15, 45, 75, 120, 0, 0, 0, 0]
    };
    let local_start = if guided { 0 } else { 4 };
    let mut evaluated = Vec::with_capacity(8);
    if let Some(turn) = initial_turn {
        evaluated.push(turn.abs());
    }
    let mut measurements = 0;
    // Spend the first measurement on the already-prepared guidance winner.
    // Refine a guided heading before spending the remaining grant on coarse
    // alternatives or the lateral fallback. Keep the nine-measurement ceiling.
    for slot in 0..10 {
        if cancelled() {
            return Ok(None);
        }
        if measurements == 9 {
            break;
        }
        let (turn, candidate) = if slot == 0 {
            (
                initial_turn,
                preferred.take().ok_or("Missing prepared portal proposal")?,
            )
        } else {
            let (turn, delta) = if slot == 9 {
                if !guided {
                    continue;
                }
                (None, baseline)
            } else {
                let index = slot - 1;
                if index == local_start || index == local_start + 2 {
                    let center = best
                        .as_ref()
                        .and_then(|b| b.2)
                        .map_or(baseline_turn, i32::abs);
                    let step = if index == local_start { 5 } else { 1 };
                    directions[index] = (center - step).clamp(1, 120);
                    directions[index + 1] = (center + step).clamp(1, 120);
                }
                let degrees = directions[index];
                if evaluated.contains(&degrees) {
                    continue;
                }
                evaluated.push(degrees);
                let bearing = angle + sign * f64::from(degrees).to_radians();
                let delta = [length * bearing.cos() - d[0], length * bearing.sin() - d[1]];
                if delta[0].hypot(delta[1]) > offset.abs() * width {
                    continue;
                }
                (Some(degrees * sign as i32), delta)
            };
            let remaining = maximum_work.saturating_sub(work);
            if remaining <= initial_work {
                exhausted = true;
                break;
            }
            let Some(candidate) = propose_inner(
                report,
                witness,
                grid,
                offset,
                remaining,
                retained,
                Some(delta),
                &cancelled,
            )?
            else {
                return Ok(None);
            };
            work = work
                .checked_add(candidate.work)
                .filter(|v| *v <= maximum_work)
                .ok_or("Portal proposal work overflow")?;
            (turn, candidate)
        };
        let comparisons = rejected
            .iter()
            .try_fold(0usize, |sum, p| {
                sum.checked_add(
                    p.len()
                        .min(candidate.after.len())
                        .checked_mul(3)?
                        .checked_add(1)?,
                )
            })
            .ok_or("Portal comparison work overflow")?;
        if comparisons > maximum_work.saturating_sub(work) {
            work = maximum_work;
            exhausted = true;
            break;
        }
        work += comparisons;
        if rejected
            .iter()
            .any(|pose| exact_points(pose, &candidate.after))
        {
            attempts.push(
                serde_json::json!({"turn_degrees":turn,"already_rejected":true,
                "comparison_work":comparisons}),
            );
            continue;
        }
        measurements += 1;
        let measurement = match evaluate(&candidate.after, endpoint, maximum_work - work) {
            Ok(Some(v)) => v,
            Ok(None) => return Ok(None),
            Err(crate::road_portal_mesh::Failure::Budget) => {
                work = maximum_work;
                exhausted = true;
                attempts.push(serde_json::json!({"turn_degrees":turn,"budget_exhausted":true}));
                break;
            }
            Err(e) => return Err(e.to_string().into()),
        };
        work = work
            .checked_add(measurement.work)
            .filter(|v| *v <= maximum_work)
            .ok_or("Portal measurement exceeded allowance")?;
        if measurement.excess.is_some_and(|v| !v.is_finite()) {
            return Err("Nonfinite portal score".into());
        }
        if measurement.clearance_passed && measurement.excess.is_none() {
            return Err("Passing portal score lacks geometry".into());
        }
        attempts.push(
            serde_json::json!({"turn_degrees":turn,"maximum_terrain_excess":measurement.excess,"clearance_passed":measurement.clearance_passed,
            "measurement":measurement.detail}),
        );
        if let Some(excess) = measurement.excess {
            let score = excess.max(0.);
            if best.as_ref().is_none_or(|b| {
                (measurement.clearance_passed && !b.3)
                    || (measurement.clearance_passed == b.3 && score + precision < b.0)
            }) {
                best = Some((score, candidate, turn, measurement.clearance_passed));
            }
        }
    }
    if cancelled() {
        return Ok(None);
    }
    let (_, mut winner, turn, clearance_passed) =
        best.ok_or("No measured unfolded portal candidate within allowance")?;
    winner.work = work;
    winner.portal_mesh = Some(
        serde_json::json!({"engine":"papyrus-native-portal-ranking-4",
        "coarse_guidance":coarse_guidance,"selected_turn_degrees":turn,"selected_clearance_passed":clearance_passed,
        "evaluation_order":"prepared-guidance-and-local-first","measurement_attempts":measurements,"maximum_measurement_attempts":9,
        "attempts":attempts,"work":work,"maximum_work":maximum_work,"budget_exhausted":exhausted,
        "complete_road_validated":false,"full_map_validated":false}),
    );
    Ok(Some(winner))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    #[test]
    fn captured_fold_relaxes_without_moving_attachments_or_control_indices() {
        use papyrus_terrain::{path_geometry::Path, road_strip};
        let mut report = fixture();
        report.roads[0].points = vec![
            [1_859.753, 19.612923, -276.34753],
            [1866.6343, 18.546322, -279.20273],
            [1873.0615, 17.618956, -283.02417],
            [1878.792, 16.824223, -287.84937],
            [1884.4292, 16.024643, -292.7839],
            [1890.1298, 15.137395, -297.62585],
            [1896.0958, 14.05144, -302.08362],
            [1902.7294, 12.408302, -304.83792],
            [1907.1895, 10.016428, -301.10626],
            [1906.0813, 8.486675, -293.88235],
            [1903.5865, 7.4300756, -286.85156],
            [1901.3247, 6.4678273, -279.74265],
            [1900.0435, 5.591826, -272.3817],
            [1900.0502, 4.788834, -264.9491],
            [1902.0695, 4.1392174, -257.73642],
            [1905.1921, 3.6058733, -250.91083],
        ];
        report.families[0].width = 10.;
        let points = report.roads[0].points.clone();
        let path = Path::from_controls(&points, 10000, || false)
            .unwrap()
            .unwrap();
        let clear = |path: &Path| {
            road_strip::check(
                path,
                &vec![true; path.points().len()],
                5.,
                |_| Ok::<_, String>(()),
                &|| false,
            )
            .unwrap()
        };
        assert_eq!(clear(&path), Some(false));
        let heights = vec![16711; 129 * 129];
        let grid = path_terrain::Grid {
            size: 5000.,
            resolution: 129,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &heights,
        };
        let witness = AlignmentWitness {
            kind: crate::road_refinement::AlignmentKind::Curvature,
            road: 0,
            position: [1905.0059, -304.17053],
        };
        let proposal = propose_curvature(&report, witness, grid, 0.5, 1_000_000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(proposal.after.len(), points.len());
        assert_eq!(proposal.after.first(), points.first());
        assert_eq!(proposal.after.last(), points.last());
        assert_eq!(report.roads[0].points, points);
        let verdicts = OFFSETS[..4]
            .iter()
            .map(|&branch| {
                let candidate =
                    propose_curvature(&report, witness, grid, branch, 1_000_000, || false)
                        .unwrap()
                        .unwrap();
                assert_eq!(candidate.after.len(), points.len());
                assert_eq!(candidate.after.first(), points.first());
                assert_eq!(candidate.after.last(), points.last());
                assert_eq!(candidate.after[..3], points[..3]);
                assert_eq!(
                    candidate.after[points.len() - 3..],
                    points[points.len() - 3..]
                );
                let relaxed = Path::from_controls(&candidate.after, 10000, || false)
                    .unwrap()
                    .unwrap();
                clear(&relaxed)
            })
            .collect::<Vec<_>>();
        assert!(verdicts.contains(&Some(true)), "{verdicts:?}");
        assert!(
            propose_curvature(&report, witness, grid, 0.5, proposal.work - 1, || false).is_err()
        );
        assert!(
            propose_curvature(&report, witness, grid, 0.5, 1_000_000, || true)
                .unwrap()
                .is_none()
        );
        report.roads.push(papyrus_terrain::road_network::Road {
            from: Destination::Network { road: 0, point: 7 },
            to: Destination::Monument {
                monument: 2,
                connector: 3,
            },
            points: vec![points[7], [points[7][0] + 10., points[7][1], points[7][2]]],
        });
        let attached = propose_curvature(&report, witness, grid, 0.5, 1_000_000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(attached.after[5..10], points[5..10]);
    }
    #[test]
    fn contour_proposal_keeps_graph_indices_and_refuses_an_attached_tail() {
        let mut report = fixture();
        report.roads[0].points = (0..10).map(|i| [-50. + i as f32 * 9., 0., 0.]).collect();
        report.roads[0].from = Destination::Network { road: 1, point: 0 };
        report.roads[0].to = Destination::Monument {
            monument: 1,
            connector: 7,
        };
        report.roads.push(papyrus_terrain::road_network::Road {
            from: Destination::Loop,
            to: Destination::Loop,
            points: vec![[-50., 0., 0.], [-50., 0., 20.], [-50., 0., 0.]],
        });
        let before = report.roads[0].points.clone();
        let heights = vec![0; 129 * 129];
        let topology = vec![1024; 128 * 128];
        let grid = path_terrain::Grid {
            size: 128.,
            resolution: 129,
            origin_y: 0.,
            vertical_span: 100.,
            height: &heights,
        };
        let witness = AlignmentWitness {
            kind: crate::road_refinement::AlignmentKind::Grade,
            road: 0,
            position: [22., 0.],
        };
        let shaping = papyrus_terrain::road_network::ShapingPolicy {
            horizontal_iterations: 4,
            vertical_iterations: 16,
            resample_distance: 7.5,
            maximum_work: 100_000,
        };
        let output = propose_contour(
            &report,
            witness,
            grid,
            &topology,
            128,
            1024,
            0.2,
            shaping,
            1_000_000,
            || false,
        )
        .unwrap()
        .unwrap();
        let proposal = output.proposal.unwrap();
        assert_eq!(proposal.after.len(), before.len());
        assert_eq!(proposal.after[..4], before[..4]);
        assert_eq!(proposal.after.last(), before.last());
        assert_eq!(report.roads[0].points, before);
        assert!(output.work <= 1_000_000);
        report.roads.push(papyrus_terrain::road_network::Road {
            from: Destination::Network { road: 0, point: 8 },
            to: Destination::Monument {
                monument: 2,
                connector: 1,
            },
            points: vec![before[8], [before[8][0], 0., 10.]],
        });
        assert!(
            propose_contour(
                &report,
                witness,
                grid,
                &topology,
                128,
                1024,
                0.2,
                shaping,
                1_000_000,
                || false
            )
            .unwrap()
            .is_none()
        );
        assert!(
            propose_contour(
                &report,
                witness,
                grid,
                &topology,
                128,
                1024,
                0.2,
                shaping,
                1_000_000,
                || true
            )
            .unwrap()
            .is_none()
        );
    }
    fn loop_fixture() -> Report {
        let mut report = fixture();
        report.roads[0].points = (0..120)
            .map(|i| {
                let a = i as f64 * std::f64::consts::TAU / 120.;
                [(150. * a.cos()) as f32, 0., (150. * a.sin()) as f32]
            })
            .collect();
        let seam = report.roads[0].points[0];
        report.roads[0].points.push(seam);
        report
    }
    #[test]
    fn cyclic_seam_moves_without_an_anchor_and_is_invariant_under_reindexing() {
        use papyrus_terrain::path_geometry::Path;
        let mut report = loop_fixture();
        let height = vec![18000; 201 * 201];
        let grid = path_terrain::Grid {
            size: 1000.,
            resolution: 201,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &height,
        };
        let witness = AlignmentWitness {
            kind: crate::road_refinement::AlignmentKind::Grade,
            road: 0,
            position: [150., 0.],
        };
        let first = propose(&report, witness, grid, 0.5, 1_000_000, || false)
            .unwrap()
            .unwrap();
        assert!(first.circular);
        assert_ne!(first.after[0][0], first.before[0][0]);
        assert_eq!(first.after[0], first.after[120]);
        let path = Path::from_controls(&first.after, 10000, || false)
            .unwrap()
            .unwrap();
        assert!(path.is_circular());
        assert_eq!(path.tangents().first(), path.tangents().last());
        let original = report.roads[0].points.clone();
        report.roads[0].points = (0..120).map(|i| original[(i + 37) % 120]).collect();
        let seam = report.roads[0].points[0];
        report.roads[0].points.push(seam);
        let rotated = propose(&report, witness, grid, 0.5, 1_000_000, || false)
            .unwrap()
            .unwrap();
        for i in 0..120 {
            for c in 0..3 {
                assert!((first.after[(i + 37) % 120][c] - rotated.after[i][c]).abs() < 0.0001);
            }
        }
        // A real attachment to the duplicated seam freezes both copies and
        // both sides of its tangent neighborhood, even across index zero.
        report.roads[0].points = original;
        report.roads.push(papyrus_terrain::road_network::Road {
            from: Destination::Monument {
                monument: 1,
                connector: 1,
            },
            to: Destination::Network {
                road: 0,
                point: 120,
            },
            points: vec![[200., 0., 0.], [150., 0., 0.]],
        });
        assert!(propose(&report, witness, grid, 0.5, 1_000_000, || false).is_err());
        let p = report.roads[0].points[10];
        let anchored = propose(
            &report,
            AlignmentWitness {
                kind: crate::road_refinement::AlignmentKind::Grade,
                road: 0,
                position: [p[0] as f64, p[2] as f64],
            },
            grid,
            0.5,
            1_000_000,
            || false,
        )
        .unwrap()
        .unwrap();
        for i in [0, 1, 2, 118, 119, 120] {
            assert_eq!(anchored.after[i], anchored.before[i]);
        }
    }
    #[test]
    fn retained_contact_extends_a_plateau_across_the_corridor_and_keeps_endpoints() {
        let mut report = fixture();
        report.roads[0].points = (0..161).map(|i| [-400. + i as f32 * 5., 0., 0.]).collect();
        let height = vec![18000; 201 * 201];
        let grid = path_terrain::Grid {
            size: 1000.,
            resolution: 201,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &height,
        };
        let support = (60..=140).map(|x| 100 * 201 + x).collect::<Vec<_>>();
        let witness = AlignmentWitness {
            kind: crate::road_refinement::AlignmentKind::Clearance,
            road: 0,
            position: [0.; 2],
        };
        let candidate = propose_with_retained(
            &report,
            witness,
            grid,
            0.5,
            1_000_000,
            Some(&support),
            || false,
        )
        .unwrap()
        .unwrap();
        let local = propose(&report, witness, grid, 0.5, 1_000_000, || false)
            .unwrap()
            .unwrap();
        let grade = propose_with_retained(
            &report,
            AlignmentWitness {
                kind: crate::road_refinement::AlignmentKind::Grade,
                ..witness
            },
            grid,
            0.5,
            1_000_000,
            Some(&support),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(grade.contact_controls, 0);
        assert_eq!(grade.after, local.after);
        assert!(candidate.contact_controls > 80);
        assert_eq!(candidate.after[110][2], -5.); // X=150, beyond the old local bump.
        assert_eq!(local.after[110], local.before[110]);
        for i in [0, 1, 2, 158, 159, 160] {
            assert_eq!(candidate.after[i], candidate.before[i]);
        }
        assert!(
            propose_with_retained(&report, witness, grid, 0.5, 1000, Some(&support), || false)
                .is_err()
        );
        assert!(
            propose_with_retained(
                &report,
                witness,
                grid,
                0.5,
                1_000_000,
                Some(&[2, 1]),
                || false
            )
            .is_err()
        );
    }
    pub(crate) fn fixture() -> Report {
        let mut report = Report::default();
        report.roads.push(papyrus_terrain::road_network::Road {
            from: Destination::Loop,
            to: Destination::Loop,
            points: (0..61).map(|i| [(i as f32 - 30.) * 5., 0., 0.]).collect(),
        });
        report.families.push(papyrus_terrain::road_network::Family {
            infrastructure_type: 0,
            hierarchy: 0,
            seed: 1,
            first_road: 0,
            road_count: 1,
            socket_count: 0,
            unresolved: vec![],
            width: 10.,
            splat_channel: 7,
        });
        report
    }
    fn hint_fixture() -> (Report, Hint) {
        let mut report = fixture();
        report.roads[0].from = Destination::Network { road: 1, point: 0 };
        report.roads[0].to = Destination::Monument {
            monument: 1,
            connector: 7,
        };
        report.roads.push(papyrus_terrain::road_network::Road {
            from: Destination::Loop,
            to: Destination::Loop,
            points: vec![[-150., 0., 0.], [-150., 0., 40.]],
        });
        let original = Hint::capture(&report, 0).unwrap();
        let height = vec![18000; 25];
        let proposal = propose(
            &report,
            AlignmentWitness {
                kind: crate::road_refinement::AlignmentKind::Grade,
                road: 0,
                position: [140., 0.],
            },
            path_terrain::Grid {
                size: 1000.,
                resolution: 5,
                origin_y: -500.,
                vertical_span: 1000.,
                height: &height,
            },
            0.5,
            1_000_000,
            || false,
        )
        .unwrap()
        .unwrap();
        report.roads[0].points = proposal.after;
        let hint = original.candidate(&report).unwrap();
        report.roads[0].points = original.before;
        (report, hint)
    }
    #[test]
    fn geometry_hint_replays_only_exact_roads_and_preserves_current_attachments() {
        let (mut report, hint) = hint_fixture();
        let replay = hint.replay(&report, 1_000_000, || false).unwrap().unwrap();
        assert_eq!(replay.reason, "applied");
        assert!(replay.work > 0);
        let proposal = replay.proposal.unwrap();
        assert_eq!(proposal.after, hint.after);
        assert_eq!(report.roads[0].points, hint.before);
        report.roads.push(papyrus_terrain::road_network::Road {
            from: Destination::Monument {
                monument: 2,
                connector: 8,
            },
            to: Destination::Network { road: 0, point: 58 },
            points: vec![[140., 0., 40.], [140., 0., 0.]],
        });
        let replay = hint.replay(&report, 1_000_000, || false).unwrap().unwrap();
        assert_eq!(replay.reason, "attachment_changed");
        assert!(replay.proposal.is_none());
        assert_eq!(report.roads[0].points, hint.before);
    }
    #[test]
    fn geometry_hint_rejects_changed_height_endpoint_family_or_work_grant() {
        let (mut report, hint) = hint_fixture();
        report.roads[0].points[30][1] = f32::from_bits(report.roads[0].points[30][1].to_bits() + 1);
        assert_eq!(
            hint.replay(&report, 1_000_000, || false)
                .unwrap()
                .unwrap()
                .reason,
            "road_changed"
        );
        report.roads[0].points = hint.before.clone();
        report.roads[0].to = Destination::Monument {
            monument: 1,
            connector: 9,
        };
        assert_eq!(
            hint.replay(&report, 1_000_000, || false)
                .unwrap()
                .unwrap()
                .reason,
            "road_changed"
        );
        report.roads[0].to = hint.to;
        report.families[0].width += 1.;
        assert_eq!(
            hint.replay(&report, 1_000_000, || false)
                .unwrap()
                .unwrap()
                .reason,
            "family_changed"
        );
        assert!(hint.replay(&report, 1, || false).is_err());
        assert!(hint.replay(&report, 1_000_000, || true).unwrap().is_none());
    }
    #[test]
    fn geometry_hint_cache_is_bounded_and_replaces_only_matching_geometry_keys() {
        let (_, hint) = hint_fixture();
        let mut hints = vec![];
        for monument in 1..=6 {
            let mut next = hint.clone();
            next.to = Destination::Monument {
                monument,
                connector: 7,
            };
            Hint::collect(&mut hints, next);
        }
        assert_eq!(hints.len(), 4);
        let mut next = hint.clone();
        next.after[30][2] += 1.;
        Hint::collect(&mut hints, next.clone());
        assert_eq!(hints.len(), 4);
        assert_eq!(hints[0].after, next.after);
    }
    #[test]
    fn authored_entrance_stays_exact_while_its_generated_approach_can_turn() {
        let mut report = fixture();
        report.roads[0].to = Destination::Monument {
            monument: 1,
            connector: 7,
        };
        let height = vec![18000; 25];
        let grid = path_terrain::Grid {
            size: 1000.,
            resolution: 5,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &height,
        };
        for offset in OFFSETS {
            let p = report.roads[0].points[58];
            let proposal = propose_with_retained(
                &report,
                AlignmentWitness {
                    kind: crate::road_refinement::AlignmentKind::Grade,
                    road: 0,
                    position: [f64::from(p[0]), f64::from(p[2])],
                },
                grid,
                offset,
                1_000_000,
                None,
                || false,
            )
            .unwrap()
            .unwrap();
            assert_eq!(proposal.after[60], proposal.before[60]);
            assert_ne!(proposal.after[58][2], proposal.before[58][2]);
            assert_ne!(proposal.after[59][2], proposal.before[59][2]);
            for i in [0, 1, 2] {
                assert_eq!(proposal.after[i], proposal.before[i]);
            }
            let terminal = propose_with_retained(
                &report,
                AlignmentWitness {
                    kind: crate::road_refinement::AlignmentKind::Grade,
                    road: 0,
                    position: [150., 0.],
                },
                grid,
                offset,
                1_000_000,
                None,
                || false,
            )
            .unwrap()
            .unwrap();
            assert_eq!(terminal.endpoint_tangent, Some(60));
            assert_eq!(terminal.after[60], terminal.before[60]);
            assert_ne!(terminal.after[59][2], terminal.before[59][2]);
        }
        // An actual child junction at the same neighborhood still freezes its
        // tangent controls, even when this road terminates at a monument.
        report.roads.push(papyrus_terrain::road_network::Road {
            from: Destination::Monument {
                monument: 2,
                connector: 8,
            },
            to: Destination::Network { road: 0, point: 58 },
            points: vec![[140., 0., 40.], [140., 0., 0.]],
        });
        assert!(matches!(
            propose_with_retained(
                &report,
                AlignmentWitness {
                    kind: crate::road_refinement::AlignmentKind::Grade,
                    road: 0,
                    position: [140., 0.]
                },
                grid,
                0.5,
                1_000_000,
                None,
                || false,
            ),
            Err(Failure::PinnedAttachment)
        ));
    }
    #[test]
    fn portal_witness_turns_either_approach_but_cannot_bypass_another_attachment() {
        let height = vec![18000; 25];
        let grid = path_terrain::Grid {
            size: 1000.,
            resolution: 5,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &height,
        };
        for endpoint in [0, 60] {
            let mut report = fixture();
            let destination = Destination::Monument {
                monument: 1,
                connector: 7,
            };
            if endpoint == 0 {
                report.roads[0].from = destination;
            } else {
                report.roads[0].to = destination;
            }
            let original = report.roads[0].points.clone();
            let p = original[endpoint];
            let neighbor = if endpoint == 0 { 1 } else { 59 };
            let at = |report: &Report, position| {
                propose_with_retained(
                    report,
                    AlignmentWitness {
                        kind: crate::road_refinement::AlignmentKind::Clearance,
                        road: 0,
                        position,
                    },
                    grid,
                    0.5,
                    1_000_000,
                    None,
                    || false,
                )
            };
            let proposal = at(&report, [f64::from(p[0]), f64::from(p[2])])
                .unwrap()
                .unwrap();
            assert_eq!(proposal.endpoint_tangent, Some(endpoint));
            assert!(exact_points(
                &proposal.before[endpoint..=endpoint],
                &proposal.after[endpoint..=endpoint]
            ));
            assert_ne!(proposal.before[neighbor], proposal.after[neighbor]);
            assert_eq!(report.roads[0].points, original);
            // An unrelated distant witness must not become a portal repair.
            let far = f64::from(p[0]) + if endpoint == 0 { -20. } else { 20. };
            assert!(matches!(
                at(&report, [far, 0.]),
                Err(Failure::PinnedAttachment)
            ));
            // Keep network/child tangent neighborhoods exactly protected.
            report.roads.push(papyrus_terrain::road_network::Road {
                from: Destination::Monument {
                    monument: 2,
                    connector: 8,
                },
                to: Destination::Network {
                    road: 0,
                    point: neighbor,
                },
                points: vec![[original[neighbor][0], 0., 40.], original[neighbor]],
            });
            assert!(matches!(
                at(&report, [f64::from(p[0]), 0.]),
                Err(Failure::PinnedAttachment)
            ));
        }
    }
    #[test]
    fn portal_contour_choice_drives_the_proposal_without_moving_its_anchor() {
        let mut report = fixture();
        report.roads[0].to = Destination::Monument {
            monument: 1,
            connector: 7,
        };
        report.roads[0].points[59][0] = 142.5;
        let original = report.roads[0].points.clone();
        let height = (0usize..1001 * 1001)
            .map(|i| {
                let x = (i % 1001) as i32 - 650;
                (16383 - (x * x).min(8000)) as i16
            })
            .collect::<Vec<_>>();
        let grid = path_terrain::Grid {
            size: 1000.,
            resolution: 1001,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &height,
        };
        let proposal = propose_with_retained(
            &report,
            AlignmentWitness {
                kind: crate::road_refinement::AlignmentKind::Clearance,
                road: 0,
                position: [150., 0.],
            },
            grid,
            1.,
            1_000_000,
            None,
            || false,
        )
        .unwrap()
        .unwrap();
        let choice = proposal.portal_heading.as_ref().unwrap();
        assert!(matches!(choice.turn_degrees, Some(1..=15)));
        assert!(choice.selected_sampled_excess + 0.001 < choice.baseline_sampled_excess.unwrap());
        assert!(exact_points(&proposal.after[60..], &original[60..]));
        assert!(exact_points(&proposal.after[..3], &original[..3]));
        assert_ne!(proposal.after[59], original[59]);
        assert!(
            (f64::from(proposal.after[59][0]) - f64::from(original[59][0]))
                .hypot(f64::from(proposal.after[59][2]) - f64::from(original[59][2]))
                <= 10.
        );
        assert_eq!(report.roads[0].points, original);
    }
    #[test]
    fn native_portal_ranking_uses_mesh_scores_and_preserves_all_anchors() {
        let mut report = fixture();
        report.roads[0].to = Destination::Monument {
            monument: 1,
            connector: 7,
        };
        let original = report.roads[0].points.clone();
        let height = vec![16383; 201 * 201];
        let grid = path_terrain::Grid {
            size: 1000.,
            resolution: 201,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &height,
        };
        let witness = AlignmentWitness {
            kind: crate::road_refinement::AlignmentKind::Clearance,
            road: 0,
            position: [150., 0.],
        };
        let winner = propose_with_native(
            &report,
            witness,
            grid,
            1.,
            1_000_000,
            None,
            true,
            &[],
            |controls, endpoint, _| {
                assert_eq!(endpoint, 60);
                assert!(exact_points(&controls[60..], &original[60..]));
                Ok(Some(PortalEvaluation {
                    clearance_passed: false,
                    excess: Some((f64::from(controls[59][0]) - 150.).abs()),
                    work: 7,
                    detail: serde_json::json!({"scope":"test_mesh_score_only"}),
                }))
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(winner.after[59][0] > 149.);
        assert!(exact_points(&winner.after[..3], &original[..3]));
        assert!(exact_points(&winner.after[60..], &original[60..]));
        assert_ne!(winner.after[59], original[59]);
        assert!(winner.work <= 1_000_000);
        assert_eq!(report.roads[0].points, original);
        let bypass = propose_with_native(
            &report,
            witness,
            grid,
            1.,
            1_000_000,
            None,
            false,
            &[],
            |_, _, _| panic!("Non-mesh family invoked native geometry"),
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(bypass.portal_mesh.is_none());
    }
    #[test]
    fn native_portal_measures_prepared_guidance_with_only_one_measurement_grant() {
        let mut report = fixture();
        report.roads[0].to = Destination::Monument {
            monument: 1,
            connector: 7,
        };
        report.roads[0].points[59][0] = 142.5;
        let original = report.roads[0].points.clone();
        let height = (0usize..1001 * 1001)
            .map(|i| {
                let x = (i % 1001) as i32 - 650;
                (16383 - (x * x).min(8000)) as i16
            })
            .collect::<Vec<_>>();
        let grid = path_terrain::Grid {
            size: 1000.,
            resolution: 1001,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &height,
        };
        let witness = AlignmentWitness {
            kind: crate::road_refinement::AlignmentKind::Clearance,
            road: 0,
            position: [150., 0.],
        };
        let initial = propose_with_retained(&report, witness, grid, 1., 1_000_000, None, || false)
            .unwrap()
            .unwrap();
        let turn = initial
            .portal_heading
            .as_ref()
            .unwrap()
            .turn_degrees
            .unwrap();
        assert!(initial.work > 100);
        let limit = initial.work + 100;
        let mut calls = 0;
        let winner = propose_with_native(
            &report,
            witness,
            grid,
            1.,
            limit,
            None,
            true,
            &[],
            |points, endpoint, allowance| {
                calls += 1;
                assert_eq!(endpoint, 60);
                assert!(exact_points(points, &initial.after));
                assert_eq!(allowance, 100);
                Ok(Some(PortalEvaluation {
                    excess: Some(0.03),
                    clearance_passed: false,
                    work: allowance,
                    detail: serde_json::json!({"scope":"one_measurement_test_only"}),
                }))
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(winner.work, limit);
        assert!(exact_points(&winner.after, &initial.after));
        assert!(exact_points(&winner.after[60..], &original[60..]));
        assert_eq!(report.roads[0].points, original);
        let audit = winner.portal_mesh.unwrap();
        assert_eq!(audit["selected_turn_degrees"], turn);
        assert_eq!(audit["measurement_attempts"], 1);
        assert_eq!(audit["selected_clearance_passed"], false);
        assert_eq!(audit["full_map_validated"], false);
    }
    #[test]
    fn native_portal_refines_guidance_before_spending_a_lateral_fallback_measurement() {
        let mut report = fixture();
        report.roads[0].to = Destination::Monument {
            monument: 1,
            connector: 7,
        };
        report.roads[0].points[59][0] = 142.5;
        let original = report.roads[0].points.clone();
        let height = (0usize..1001 * 1001)
            .map(|i| {
                let x = (i % 1001) as i32 - 650;
                (16383 - (x * x).min(8000)) as i16
            })
            .collect::<Vec<_>>();
        let grid = path_terrain::Grid {
            size: 1000.,
            resolution: 1001,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &height,
        };
        let witness = AlignmentWitness {
            kind: crate::road_refinement::AlignmentKind::Clearance,
            road: 0,
            position: [150., 0.],
        };
        let mut calls = 0;
        let winner = propose_with_native(
            &report,
            witness,
            grid,
            1.,
            1_000_000,
            None,
            true,
            &[],
            |points, _, _| {
                calls += 1;
                if calls == 4 {
                    return Err(crate::road_portal_mesh::Failure::Budget);
                }
                let before = [
                    f64::from(original[59][0] - original[60][0]),
                    f64::from(original[59][2] - original[60][2]),
                ];
                let after = [
                    f64::from(points[59][0] - points[60][0]),
                    f64::from(points[59][2] - points[60][2]),
                ];
                let turn = (before[0] * after[1] - before[1] * after[0])
                    .atan2(before[0] * after[0] + before[1] * after[1])
                    .to_degrees();
                assert!((turn - [15., 10., 20.][calls - 1]).abs() < 0.001);
                assert!(exact_points(&points[60..], &original[60..]));
                Ok(Some(PortalEvaluation {
                    excess: Some(if calls == 3 { 0.019 } else { 0.03 }),
                    clearance_passed: calls == 3,
                    work: 100,
                    detail: serde_json::json!({"scope":"ordering_test_only"}),
                }))
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(calls, 4);
        assert_eq!(winner.work, 1_000_000);
        let audit = winner.portal_mesh.unwrap();
        assert_eq!(audit["selected_turn_degrees"], 20);
        assert_eq!(audit["selected_clearance_passed"], true);
        assert_eq!(audit["full_map_validated"], false);
        assert_eq!(report.roads[0].points, original);
    }
    #[test]
    fn native_portals_skip_exact_rejections_and_prioritize_passing_clearance() {
        let mut report = fixture();
        report.roads[0].to = Destination::Monument {
            monument: 1,
            connector: 7,
        };
        let original = report.roads[0].points.clone();
        let height = vec![16383; 201 * 201];
        let grid = path_terrain::Grid {
            size: 1000.,
            resolution: 201,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &height,
        };
        let witness = AlignmentWitness {
            kind: crate::road_refinement::AlignmentKind::Clearance,
            road: 0,
            position: [150., 0.],
        };
        let mut first_pose = None;
        let mut passing_pose = None;
        let mut calls = 0;
        let winner = propose_with_native(
            &report,
            witness,
            grid,
            1.,
            1_000_000,
            None,
            true,
            &[],
            |controls, _, _| {
                calls += 1;
                if calls == 1 {
                    first_pose = Some(controls.to_vec());
                }
                if calls == 2 {
                    passing_pose = Some(controls.to_vec());
                }
                Ok(Some(PortalEvaluation {
                    excess: Some(if calls == 1 { 0.0201 } else { 0.0199 }),
                    clearance_passed: calls > 1,
                    work: 7,
                    detail: serde_json::json!({}),
                }))
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert!((2..=9).contains(&calls));
        assert!(exact_points(&winner.after, passing_pose.as_ref().unwrap()));
        assert!(!exact_points(&winner.after, first_pose.as_ref().unwrap()));
        assert_eq!(
            winner.portal_mesh.as_ref().unwrap()["selected_clearance_passed"],
            true
        );
        let rejected = vec![winner.after.clone()];
        let mut measured = 0;
        let replacement = propose_with_native(
            &report,
            witness,
            grid,
            1.,
            1_000_000,
            None,
            true,
            &rejected,
            |controls, _, _| {
                measured += 1;
                assert!(!exact_points(controls, &rejected[0]));
                Ok(Some(PortalEvaluation {
                    excess: Some(0.03),
                    clearance_passed: false,
                    work: 7,
                    detail: serde_json::json!({}),
                }))
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert!((1..=9).contains(&measured));
        assert!(!exact_points(&replacement.after, &rejected[0]));
        assert!(
            replacement.portal_mesh.as_ref().unwrap()["attempts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["already_rejected"] == true)
        );
        assert!(exact_points(&replacement.after[..3], &original[..3]));
        assert!(exact_points(&replacement.after[60..], &original[60..]));
        assert_eq!(report.roads[0].points, original);
    }
    #[test]
    fn native_portal_limits_keep_only_complete_measurements_and_stop_on_cancel() {
        let mut report = fixture();
        report.roads[0].to = Destination::Monument {
            monument: 1,
            connector: 7,
        };
        let original = report.roads[0].points.clone();
        let height = vec![16383; 201 * 201];
        let grid = path_terrain::Grid {
            size: 1000.,
            resolution: 201,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &height,
        };
        let witness = AlignmentWitness {
            kind: crate::road_refinement::AlignmentKind::Clearance,
            road: 0,
            position: [150., 0.],
        };
        let mut calls = 0;
        let best = propose_with_native(
            &report,
            witness,
            grid,
            1.,
            1_000_000,
            None,
            true,
            &[],
            |_, _, _| {
                calls += 1;
                if calls == 2 {
                    return Err(crate::road_portal_mesh::Failure::Budget);
                }
                Ok(Some(PortalEvaluation {
                    clearance_passed: false,
                    excess: Some(0.03),
                    work: 7,
                    detail: serde_json::json!({}),
                }))
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(best.work, 1_000_000);
        assert_eq!(best.portal_mesh.as_ref().unwrap()["budget_exhausted"], true);
        assert!(
            propose_with_native(
                &report,
                witness,
                grid,
                1.,
                1_000_000,
                None,
                true,
                &[],
                |_, _, _| Ok(None),
                || false
            )
            .unwrap()
            .is_none()
        );
        assert!(
            propose_with_native(
                &report,
                witness,
                grid,
                1.,
                1_000_000,
                None,
                true,
                &[],
                |_, _, remaining| {
                    Ok(Some(PortalEvaluation {
                        clearance_passed: false,
                        excess: Some(0.),
                        work: remaining + 1,
                        detail: serde_json::json!({}),
                    }))
                },
                || false
            )
            .is_err()
        );
        assert!(
            propose_with_native(
                &report,
                witness,
                grid,
                1.,
                1_000_000,
                None,
                true,
                &[],
                |_, _, _| {
                    Ok(Some(PortalEvaluation {
                        clearance_passed: false,
                        excess: None,
                        work: 7,
                        detail: serde_json::json!({"folded":true}),
                    }))
                },
                || false
            )
            .is_err()
        );
        assert_eq!(report.roads[0].points, original);
    }
    #[test]
    fn local_offset_preserves_attachments_and_tangents_and_samples_new_height() {
        let mut report = fixture();
        report.roads.push(papyrus_terrain::road_network::Road {
            from: Destination::Monument {
                monument: 1,
                connector: 1,
            },
            to: Destination::Network { road: 0, point: 38 },
            points: vec![[40., 0., 40.], [40., 0., 0.]],
        });
        let height = vec![18000; 25];
        let grid = path_terrain::Grid {
            size: 1000.,
            resolution: 5,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &height,
        };
        let proposal = propose(
            &report,
            AlignmentWitness {
                kind: crate::road_refinement::AlignmentKind::Grade,
                road: 0,
                position: [0., 0.],
            },
            grid,
            0.5,
            1_000_000,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(proposal.after.len(), report.roads[0].points.len());
        for i in [0, 1, 2, 36, 37, 38, 39, 40, 58, 59, 60] {
            assert_eq!(proposal.after[i], proposal.before[i]);
        }
        assert_eq!(proposal.after[30][0], 0.);
        assert_eq!(proposal.after[30][2], -5.);
        assert_eq!(
            proposal.after[30][1],
            path_terrain::sample_height(grid, [0., 0., -5.]).unwrap()
        );
        let opposite = propose(
            &report,
            AlignmentWitness {
                kind: crate::road_refinement::AlignmentKind::Grade,
                road: 0,
                position: [0., 0.],
            },
            grid,
            -0.5,
            1_000_000,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(opposite.after[30][2], 5.);
        // Every offset shares the same pinned junction neighborhood. This
        // rejection is terminal for this witness, not just the first offset.
        for offset in OFFSETS {
            for index in [0, 1, 2, 36, 37, 38, 39, 40, 58, 59, 60] {
                let p = report.roads[0].points[index];
                assert!(matches!(
                    propose_with_retained(
                        &report,
                        AlignmentWitness {
                            kind: crate::road_refinement::AlignmentKind::Grade,
                            road: 0,
                            position: [f64::from(p[0]), f64::from(p[2])],
                        },
                        grid,
                        offset,
                        1_000_000,
                        None,
                        || false,
                    ),
                    Err(Failure::PinnedAttachment)
                ));
            }
        }
        assert!(
            propose(
                &report,
                AlignmentWitness {
                    kind: crate::road_refinement::AlignmentKind::Grade,
                    road: 0,
                    position: [0., 0.]
                },
                grid,
                0.5,
                1,
                || false
            )
            .is_err()
        );
        assert!(
            propose(
                &report,
                AlignmentWitness {
                    kind: crate::road_refinement::AlignmentKind::Grade,
                    road: 0,
                    position: [0., 0.]
                },
                grid,
                0.5,
                1_000_000,
                || true
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(report.roads[0].points[30], [0.; 3]);
    }
}
