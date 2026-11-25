//! Conservative swept-bed raster checks on cubic Hermite curves. No PhysX,
//! mesh-width, rolling-stock envelope, or authored-track acceptance is implied.
use crate::{
    native_layers::{self, Layers},
    path_geometry::Path,
};
use serde::{Deserialize, Serialize};
pub const VERSION: &str = "papyrus-rail-corridor-audit-1";
pub const REFINED_VERSION: &str = "papyrus-rail-corridor-audit-2";
type V = [f64; 3];
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Refinement {
    pub maximum_depth: u8,
    pub minimum_extent: f64,
}
impl Refinement {
    fn validate(self) -> Result<(), String> {
        if !(1..=12).contains(&self.maximum_depth)
            || !self.minimum_extent.is_finite()
            || !(0.01..=1.).contains(&self.minimum_extent)
        {
            return Err("Invalid rail support refinement limits".into());
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub maximum_work: usize,
    pub maximum_depth: u8,
    pub maximum_chord: f64,
    pub maximum_deviation: f64,
    pub maximum_ground_error: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ground_refinement: Option<Refinement>,
}
impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(r) = self.ground_refinement {
            r.validate()?;
        }
        if !(1..=128000000).contains(&self.maximum_work)
            || !(1..=24).contains(&self.maximum_depth)
            || !self.maximum_chord.is_finite()
            || !(0.25..=8.).contains(&self.maximum_chord)
            || !self.maximum_deviation.is_finite()
            || !(0.001..=0.25).contains(&self.maximum_deviation)
            || !self.maximum_ground_error.is_finite()
            || !(0.01..=8.).contains(&self.maximum_ground_error)
        {
            return Err("Invalid swept rail corridor limits".into());
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Policy {
    pub half_width: f64,
    pub terrain_offset: f64,
    pub blocked_topology: u32,
    pub protected_topology: u32,
    pub water_topology: u32,
}
#[derive(Debug, Default, Serialize)]
pub struct Audit {
    pub engine: &'static str,
    pub work: usize,
    pub leaves: usize,
    pub unresolved_geometry: usize,
    pub out_of_bounds_leaves: usize,
    pub covered_cells: usize,
    pub alpha_cells: usize,
    pub blocked_topology_cells: usize,
    pub protected_cells: usize,
    pub water_cells: usize,
    pub placement_cells: usize,
    pub placement_known: bool,
    pub ground_supported_cells: usize,
    pub ground_uncertain_cells: usize,
    pub ground_conflict_cells: usize,
    pub maximum_ground_error_bound: f64,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub support_refinement: Option<RefinementStats>,
}
#[derive(Debug, Default, Serialize)]
pub struct RefinementStats {
    pub rectangles: usize,
    pub work: usize,
}
const COVER: u16 = 128;
const ALPHA: u16 = 1;
const BLOCKED: u16 = 2;
const PROTECTED: u16 = 4;
const WATER: u16 = 8;
const PLACEMENT: u16 = 16;
const UNCERTAIN: u16 = 32;
const CONFLICT: u16 = 64;
fn midpoint(a: V, b: V) -> V {
    std::array::from_fn(|i| (a[i] + b[i]) * 0.5)
}
fn point_distance(p: V, a: V, b: V) -> f64 {
    let dx = b[0] - a[0];
    let dz = b[2] - a[2];
    let len = dx * dx + dz * dz;
    let t = if len > 0. {
        ((p[0] - a[0]) * dx + (p[2] - a[2]) * dz) / len
    } else {
        0.
    }
    .clamp(0., 1.);
    (p[0] - a[0] - t * dx).hypot(p[2] - a[2] - t * dz)
}
/// Exact segment-to-cell distance in XZ (up to arithmetic roundoff).
fn rectangle_distance(a: V, b: V, lo: [f64; 2], hi: [f64; 2]) -> f64 {
    let mut enter: f64 = 0.;
    let mut leave: f64 = 1.;
    let mut possible = true;
    for (axis, j) in [0, 2].into_iter().enumerate() {
        let delta = b[j] - a[j];
        if delta == 0. {
            if a[j] < lo[axis] || a[j] > hi[axis] {
                possible = false;
            }
        } else {
            let u = (lo[axis] - a[j]) / delta;
            let v = (hi[axis] - a[j]) / delta;
            enter = enter.max(u.min(v));
            leave = leave.min(u.max(v));
        }
    }
    if possible && enter <= leave {
        return 0.;
    }
    let endpoint = |p: V| (p[0] - p[0].clamp(lo[0], hi[0])).hypot(p[2] - p[2].clamp(lo[1], hi[1]));
    let mut d = endpoint(a).min(endpoint(b));
    for x in [lo[0], hi[0]] {
        for z in [lo[1], hi[1]] {
            d = d.min(point_distance([x, 0., z], a, b));
        }
    }
    d
}
fn visit(a: &mut Audit, s: Settings, cancelled: &impl Fn() -> bool) -> Result<bool, String> {
    if cancelled() {
        return Ok(false);
    }
    if a.work >= s.maximum_work {
        return Err("Rail corridor work allowance reached".into());
    }
    a.work += 1;
    Ok(true)
}
/// Bilinear extrema on a clipped rectangle occur at its corners and internal
/// height-grid crossings. Whole surrounding vertices need not be included.
fn clipped_ground_bounds(
    layers: &Layers,
    lo: [f64; 2],
    hi: [f64; 2],
    audit: &mut Audit,
    settings: Settings,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<(f64, f64)>, String> {
    let h = layers.height_resolution;
    let size = f64::from(layers.size);
    let coordinate = |v: f64| ((v / size + 0.5) * (h - 1) as f64).clamp(0., (h - 1) as f64);
    let locations = |low: f64, high: f64| {
        let mut points = vec![low];
        let mut grid = low.floor() + 1.;
        while grid < high {
            points.push(grid);
            grid += 1.;
        }
        points.push(high);
        points
    };
    let xs = locations(coordinate(lo[0]), coordinate(hi[0]));
    let zs = locations(coordinate(lo[1]), coordinate(hi[1]));
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for &z in &zs {
        for &x in &xs {
            if !visit(audit, settings, cancelled)? {
                return Ok(None);
            }
            let a = x.floor() as usize;
            let b = z.floor() as usize;
            let c = (a + 1).min(h - 1);
            let d = (b + 1).min(h - 1);
            let fx = x - a as f64;
            let fz = z - b as f64;
            let at = |x, z| -500. + f64::from(layers.height[z * h + x]) / 32766. * 1000.;
            let value = at(a, b) * (1. - fx) * (1. - fz)
                + at(c, b) * fx * (1. - fz)
                + at(a, d) * (1. - fx) * fz
                + at(c, d) * fx * fz;
            min = min.min(value);
            max = max.max(value);
        }
    }
    Ok(Some((min, max)))
}
#[allow(clippy::too_many_arguments)]
fn refined_support(
    layers: &Layers,
    lo: [f64; 2],
    hi: [f64; 2],
    a: V,
    b: V,
    radius: f64,
    ymin: f64,
    ymax: f64,
    audit: &mut Audit,
    settings: Settings,
    refinement: Refinement,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<(u16, f64)>, String> {
    let start = audit.work;
    let mut result = 0u16;
    let mut maximum: f64 = 0.;
    let mut pending = vec![(lo, hi, 0u8)];
    let mut rectangles = 0;
    while let Some((low, high, depth)) = pending.pop() {
        if !visit(audit, settings, cancelled)? {
            return Ok(None);
        }
        rectangles += 1;
        if rectangle_distance(a, b, low, high) > radius {
            continue;
        }
        let Some((min, max)) =
            clipped_ground_bounds(layers, low, high, audit, settings, cancelled)?
        else {
            return Ok(None);
        };
        let upper = (min - ymax).abs().max((max - ymin).abs()) + 0.0001;
        let lower = (min - ymax).max(ymin - max).max(0.) - 0.0001;
        if upper <= settings.maximum_ground_error {
            maximum = maximum.max(upper);
            continue;
        }
        if lower > settings.maximum_ground_error {
            result |= CONFLICT;
            maximum = maximum.max(upper);
            continue;
        }
        let extents = [high[0] - low[0], high[1] - low[1]];
        let axis = usize::from(extents[1] > extents[0]);
        if depth >= refinement.maximum_depth || extents[axis] <= refinement.minimum_extent {
            result |= UNCERTAIN;
            maximum = maximum.max(upper);
            continue;
        }
        let middle = (low[axis] + high[axis]) * 0.5;
        let mut first_high = high;
        first_high[axis] = middle;
        let mut second_low = low;
        second_low[axis] = middle;
        pending.push((second_low, high, depth + 1));
        pending.push((low, first_high, depth + 1));
    }
    let stats = audit
        .support_refinement
        .get_or_insert_with(Default::default);
    stats.rectangles += rectangles;
    stats.work += audit.work - start;
    Ok(Some((result, maximum)))
}
/// The cubic lies within its Bézier control hull. A chord capsule expanded by
/// the maximum control-to-chord distance therefore encloses its swept width.
/// Tile ground bounds include all height vertices affecting bilinear sampling.
pub fn analyze(
    layers: &Layers,
    alpha: &[u8],
    placement: Option<&[u8]>,
    path: &Path,
    policy: Policy,
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Audit>, String> {
    settings.validate()?;
    Auditor::new(layers, alpha, placement)?.analyze(path, policy, settings, cancelled)
}

/// Audits several tracks against one immutable terrain revision. Only the
/// coverage cells touched by each audit need clearing before the next one.
pub struct Auditor<'a> {
    layers: &'a Layers,
    alpha: &'a [u8],
    placement: Option<&'a [u8]>,
    flags: Vec<u16>,
    touched: Vec<usize>,
}

impl<'a> Auditor<'a> {
    pub fn new(
        layers: &'a Layers,
        alpha: &'a [u8],
        placement: Option<&'a [u8]>,
    ) -> Result<Self, String> {
        let (h, n) = native_layers::dimensions(layers.size)?;
        let cells = n * n;
        if layers.height_resolution != h
            || layers.surface_resolution != n
            || layers.height.len() != h * h
            || layers.topology.len() != cells
            || alpha.len() != cells
            || placement.is_some_and(|p| p.len() != cells || p.iter().any(|&v| v > 1))
            || layers.height.iter().any(|v| !(0..=32766).contains(v))
        {
            return Err("Invalid swept rail corridor input".into());
        }
        Ok(Self {
            layers,
            alpha,
            placement,
            flags: vec![0; cells],
            touched: Vec::new(),
        })
    }

    pub fn analyze(
        &mut self,
        path: &Path,
        policy: Policy,
        settings: Settings,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Audit>, String> {
        let result = analyze_track(self, path, policy, settings, cancelled);
        for cell in self.touched.drain(..) {
            self.flags[cell] = 0;
        }
        result
    }
}

fn analyze_track(
    auditor: &mut Auditor<'_>,
    path: &Path,
    policy: Policy,
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Audit>, String> {
    settings.validate()?;
    if !policy.half_width.is_finite()
        || !(0.05..=32.).contains(&policy.half_width)
        || !policy.terrain_offset.is_finite()
        || policy.terrain_offset.abs() > 8.
        || path.range() != (0, path.points().len() - 1)
    {
        return Err("Invalid swept rail corridor input".into());
    }
    let Auditor {
        layers,
        alpha,
        placement,
        flags,
        touched,
    } = auditor;
    let placement = *placement;
    let h = layers.height_resolution;
    let n = layers.surface_resolution;
    let mut audit = Audit {
        engine: if settings.ground_refinement.is_some() {
            REFINED_VERSION
        } else {
            VERSION
        },
        placement_known: placement.is_some(),
        ..Default::default()
    };
    let size = f64::from(layers.size);
    let half = size * 0.5;
    for segment in 0..path.points().len() - 1 {
        let a = path.points()[segment].map(f64::from);
        let b = path.points()[segment + 1].map(f64::from);
        let ta = path.tangents()[segment].map(f64::from);
        let tb = path.tangents()[segment + 1].map(f64::from);
        let step = f64::from(path.step_size()) / 3.;
        let controls = [
            a,
            std::array::from_fn(|j| a[j] + ta[j] * step),
            std::array::from_fn(|j| b[j] - tb[j] * step),
            b,
        ];
        let mut pending = vec![(controls, 0u8)];
        while let Some((p, depth)) = pending.pop() {
            if !visit(&mut audit, settings, &cancelled)? {
                return Ok(None);
            }
            let deviation = point_distance(p[1], p[0], p[3]).max(point_distance(p[2], p[0], p[3]));
            let chord = (p[3][0] - p[0][0]).hypot(p[3][2] - p[0][2]);
            if chord > settings.maximum_chord || deviation > settings.maximum_deviation {
                if depth >= settings.maximum_depth {
                    audit.unresolved_geometry += 1;
                    continue;
                }
                let ab = midpoint(p[0], p[1]);
                let bc = midpoint(p[1], p[2]);
                let cd = midpoint(p[2], p[3]);
                let abc = midpoint(ab, bc);
                let bcd = midpoint(bc, cd);
                let mid = midpoint(abc, bcd);
                pending.push(([mid, bcd, cd, p[3]], depth + 1));
                pending.push(([p[0], ab, abc, mid], depth + 1));
                continue;
            }
            audit.leaves += 1;
            let radius = policy.half_width + deviation + 1e-9;
            let lo = [p[0][0].min(p[3][0]) - radius, p[0][2].min(p[3][2]) - radius];
            let hi = [p[0][0].max(p[3][0]) + radius, p[0][2].max(p[3][2]) + radius];
            if lo.iter().any(|v| *v < -half) || hi.iter().any(|v| *v > half) {
                audit.out_of_bounds_leaves += 1;
            }
            let index = |v: f64| {
                ((v / size + 0.5) * n as f64)
                    .floor()
                    .clamp(0., (n - 1) as f64) as usize
            };
            let ymin = p.iter().map(|v| v[1]).fold(f64::INFINITY, f64::min)
                + policy.terrain_offset.min(0.);
            let ymax = p.iter().map(|v| v[1]).fold(f64::NEG_INFINITY, f64::max)
                + policy.terrain_offset.max(0.);
            for z in index(lo[1])..=index(hi[1]) {
                for x in index(lo[0])..=index(hi[0]) {
                    if !visit(&mut audit, settings, &cancelled)? {
                        return Ok(None);
                    }
                    let low = [
                        x as f64 / n as f64 * size - half,
                        z as f64 / n as f64 * size - half,
                    ];
                    let high = [
                        (x + 1) as f64 / n as f64 * size - half,
                        (z + 1) as f64 / n as f64 * size - half,
                    ];
                    if rectangle_distance(p[0], p[3], low, high) > radius {
                        continue;
                    }
                    let i = z * n + x;
                    if flags[i] & COVER == 0 {
                        touched.push(i);
                    }
                    flags[i] |= COVER;
                    let topology = layers.topology[i] as u32;
                    if alpha[i] < 128 {
                        flags[i] |= ALPHA;
                    }
                    if topology & policy.protected_topology != 0 {
                        flags[i] |= PROTECTED;
                    }
                    if topology & (policy.blocked_topology & !policy.protected_topology) != 0 {
                        flags[i] |= BLOCKED;
                    }
                    if topology & policy.water_topology != 0 {
                        flags[i] |= WATER;
                    }
                    if placement.is_some_and(|p| p[i] != 0) {
                        flags[i] |= PLACEMENT;
                    }
                    if let Some(refinement) = settings.ground_refinement {
                        let clipped_lo = [low[0].max(lo[0]), low[1].max(lo[1])];
                        let clipped_hi = [high[0].min(hi[0]), high[1].min(hi[1])];
                        if clipped_lo[0] > clipped_hi[0] || clipped_lo[1] > clipped_hi[1] {
                            return Err("Invalid clipped support rectangle".into());
                        }
                        let Some((support, maximum)) = refined_support(
                            layers, clipped_lo, clipped_hi, p[0], p[3], radius, ymin, ymax,
                            &mut audit, settings, refinement, &cancelled,
                        )?
                        else {
                            return Ok(None);
                        };
                        flags[i] |= support;
                        audit.maximum_ground_error_bound =
                            audit.maximum_ground_error_bound.max(maximum);
                        continue;
                    }
                    let hx0 = (x as f64 / n as f64 * (h - 1) as f64).floor() as usize;
                    let hx1 =
                        (((x + 1) as f64 / n as f64 * (h - 1) as f64).ceil() as usize).min(h - 1);
                    let hz0 = (z as f64 / n as f64 * (h - 1) as f64).floor() as usize;
                    let hz1 =
                        (((z + 1) as f64 / n as f64 * (h - 1) as f64).ceil() as usize).min(h - 1);
                    let mut ground_min = f64::INFINITY;
                    let mut ground_max = f64::NEG_INFINITY;
                    for hz in hz0..=hz1 {
                        for hx in hx0..=hx1 {
                            if !visit(&mut audit, settings, &cancelled)? {
                                return Ok(None);
                            }
                            let height =
                                -500. + f64::from(layers.height[hz * h + hx]) / 32766. * 1000.;
                            ground_min = ground_min.min(height);
                            ground_max = ground_max.max(height);
                        }
                    }
                    let upper_error =
                        (ground_min - ymax).abs().max((ground_max - ymin).abs()) + 0.0001;
                    let lower_error = (ground_min - ymax).max(ymin - ground_max).max(0.) - 0.0001;
                    audit.maximum_ground_error_bound =
                        audit.maximum_ground_error_bound.max(upper_error);
                    if lower_error > settings.maximum_ground_error {
                        flags[i] |= CONFLICT;
                    } else if upper_error > settings.maximum_ground_error {
                        flags[i] |= UNCERTAIN;
                    }
                }
            }
        }
    }
    for &i in touched.iter() {
        let f = flags[i];
        if !visit(&mut audit, settings, &cancelled)? {
            return Ok(None);
        }
        if f & COVER == 0 {
            continue;
        }
        audit.covered_cells += 1;
        audit.alpha_cells += usize::from(f & ALPHA != 0);
        audit.blocked_topology_cells += usize::from(f & BLOCKED != 0);
        audit.protected_cells += usize::from(f & PROTECTED != 0);
        audit.water_cells += usize::from(f & WATER != 0);
        audit.placement_cells += usize::from(f & PLACEMENT != 0);
        audit.ground_conflict_cells += usize::from(f & CONFLICT != 0);
        audit.ground_uncertain_cells += usize::from(f & UNCERTAIN != 0 && f & CONFLICT == 0);
        audit.ground_supported_cells += usize::from(f & (UNCERTAIN | CONFLICT) == 0);
    }
    audit.status = if audit.alpha_cells
        + audit.blocked_topology_cells
        + audit.placement_cells
        + audit.ground_conflict_cells
        + audit.out_of_bounds_leaves
        > 0
    {
        "potential_conflicts"
    } else if audit.unresolved_geometry
        + audit.ground_uncertain_cells
        + audit.protected_cells
        + audit.water_cells
        > 0
        || !audit.placement_known
    {
        "needs_validation"
    } else {
        "raster_clear"
    };
    Ok(Some(audit))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Layers, Vec<u8>, Vec<u8>, Path) {
        let (h, n) = native_layers::dimensions(1000).unwrap();
        (
            Layers {
                size: 1000,
                terrain_seed: 1,
                biome_seed: 1,
                height_resolution: h,
                surface_resolution: n,
                height: vec![16383; h * h],
                topology: vec![1; n * n],
                biome: vec![],
                splat: vec![],
                normalized_biome_cells: 0,
                normalized_splat_cells: 0,
            },
            vec![255; n * n],
            vec![0; n * n],
            Path::from_controls(&[[-100., 0., 0.], [100., 0., 0.]], 10000, || false)
                .unwrap()
                .unwrap(),
        )
    }
    fn settings() -> Settings {
        Settings {
            maximum_work: 16000000,
            maximum_depth: 16,
            maximum_chord: 2.,
            maximum_deviation: 0.01,
            maximum_ground_error: 0.5,
            ground_refinement: None,
        }
    }
    fn policy() -> Policy {
        Policy {
            half_width: 2.,
            terrain_offset: 0.,
            blocked_topology: 2,
            protected_topology: 4,
            water_topology: 8,
        }
    }
    #[test]
    fn prepared_audit_clears_coverage_when_a_track_is_cancelled() {
        let (layers, alpha, placement, path) = fixture();
        let expected = analyze(
            &layers,
            &alpha,
            Some(&placement),
            &path,
            policy(),
            settings(),
            || false,
        )
        .unwrap()
        .unwrap();
        let mut auditor = Auditor::new(&layers, &alpha, Some(&placement)).unwrap();
        let checks = std::cell::Cell::new(0);
        assert!(
            auditor
                .analyze(&path, policy(), settings(), || {
                    checks.set(checks.get() + 1);
                    checks.get() == 200
                })
                .unwrap()
                .is_none()
        );
        assert!(auditor.touched.is_empty());
        assert!(auditor.flags.iter().all(|&flag| flag == 0));
        for _ in 0..2 {
            let actual = auditor
                .analyze(&path, policy(), settings(), || false)
                .unwrap()
                .unwrap();
            assert_eq!(
                serde_json::to_value(actual).unwrap(),
                serde_json::to_value(&expected).unwrap()
            );
        }
    }

    #[test]
    fn full_width_finds_edge_holes_and_all_known_raster_exclusions() {
        let (mut l, mut alpha, mut placement, p) = fixture();
        let n = l.surface_resolution;
        let audit = analyze(
            &l,
            &alpha,
            Some(&placement),
            &p,
            policy(),
            settings(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(audit.status, "raster_clear");
        assert!(audit.covered_cells > 0);
        assert_eq!(audit.ground_supported_cells, audit.covered_cells);
        let unknown = analyze(&l, &alpha, None, &p, policy(), settings(), || false)
            .unwrap()
            .unwrap();
        assert_eq!(unknown.status, "needs_validation");
        // This tile's center is ~2.93m from the curve, but its near edge is
        // ~1.95m away and intersects the 2m half-width corridor.
        let edge = (n / 2 + 1) * n + n / 2;
        alpha[edge] = 0;
        l.topology[edge] = 2 | 4 | 8;
        placement[edge] = 1;
        let audit = analyze(
            &l,
            &alpha,
            Some(&placement),
            &p,
            policy(),
            settings(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(audit.alpha_cells, 1);
        assert_eq!(audit.blocked_topology_cells, 1);
        assert_eq!(audit.protected_cells, 1);
        assert_eq!(audit.water_cells, 1);
        assert_eq!(audit.placement_cells, 1);
        assert_eq!(audit.status, "potential_conflicts");
    }
    #[test]
    fn curved_sweep_detects_holes_between_controls_and_separates_ground_uncertainty() {
        let (mut l, mut alpha, placement, _) = fixture();
        let n = l.surface_resolution;
        let p = Path::from_controls(
            &[[-50., 0., -50.], [0., 0., 0.], [50., 0., 0.]],
            10000,
            || false,
        )
        .unwrap()
        .unwrap();
        let point = p.point(p.length() * 0.75).unwrap();
        assert!(point[2] > 2.);
        let index = |v: f32| ((v / 1000. + 0.5) * n as f32) as usize;
        alpha[index(point[2]) * n + index(point[0])] = 0;
        let audit = analyze(
            &l,
            &alpha,
            Some(&placement),
            &p,
            Policy {
                half_width: 0.1,
                ..policy()
            },
            settings(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(audit.alpha_cells, 1);
        assert_eq!(audit.unresolved_geometry, 0);
        alpha.fill(255);
        l.height.fill(crate::erosion::encode(3.));
        let audit = analyze(
            &l,
            &alpha,
            Some(&placement),
            &p,
            policy(),
            settings(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(audit.ground_conflict_cells, audit.covered_cells);
        let audit = analyze(&l, &alpha, None, &p, policy(), settings(), || false)
            .unwrap()
            .unwrap();
        assert!(!audit.placement_known);
    }
    #[test]
    fn dense_independent_curve_offsets_are_contained_by_the_swept_raster() {
        let (l, mut alpha, placement, _) = fixture();
        let n = l.surface_resolution;
        let p = Path::from_controls(
            &[
                [-200., 0., -100.],
                [-40., 0., 100.],
                [60., 0., -80.],
                [200., 0., 50.],
            ],
            10000,
            || false,
        )
        .unwrap()
        .unwrap();
        let mut holes = std::collections::BTreeSet::new();
        for j in 0..=32 {
            let point = p.point(p.length() * j as f32 / 32.).unwrap();
            for k in 0..8 {
                let angle = k as f64 * std::f64::consts::TAU / 8.;
                let x = f64::from(point[0]) + 1.99 * angle.cos();
                let z = f64::from(point[2]) + 1.99 * angle.sin();
                let col = ((x / 1000. + 0.5) * n as f64).floor() as usize;
                let row = ((z / 1000. + 0.5) * n as f64).floor() as usize;
                holes.insert(row * n + col);
            }
        }
        for &i in &holes {
            alpha[i] = 0;
        }
        let audit = analyze(
            &l,
            &alpha,
            Some(&placement),
            &p,
            policy(),
            settings(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(audit.unresolved_geometry, 0);
        assert_eq!(audit.alpha_cells, holes.len());
    }
    #[test]
    fn clipped_refinement_resolves_off_corridor_ground_without_waiving_real_conflicts() {
        let (mut l, alpha, placement, _) = fixture();
        let h = l.height_resolution;
        for z in 0..h {
            l.height[z * h + h / 2 + 2] = crate::erosion::encode(3.);
        }
        let p = Path::from_controls(&[[0.1, 0., -20.], [0.1, 0., 20.]], 10000, || false)
            .unwrap()
            .unwrap();
        let policy = Policy {
            half_width: 0.05,
            ..policy()
        };
        let coarse = analyze(&l, &alpha, Some(&placement), &p, policy, settings(), || {
            false
        })
        .unwrap()
        .unwrap();
        assert!(coarse.ground_uncertain_cells > 0);
        let refined_settings = Settings {
            ground_refinement: Some(Refinement {
                maximum_depth: 8,
                minimum_extent: 0.05,
            }),
            ..settings()
        };
        let refined = analyze(
            &l,
            &alpha,
            Some(&placement),
            &p,
            policy,
            refined_settings,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(refined.covered_cells, coarse.covered_cells);
        assert_eq!(refined.status, "raster_clear");
        assert_eq!(refined.engine, REFINED_VERSION);
        assert_eq!(refined.ground_uncertain_cells, 0);
        assert!(refined.support_refinement.unwrap().rectangles > 0);
        for z in 0..h {
            l.height[z * h + h / 2 + 1] = crate::erosion::encode(3.);
        }
        let p = Path::from_controls(&[[0.4, 0., -20.], [0.4, 0., 20.]], 10000, || false)
            .unwrap()
            .unwrap();
        let refined = analyze(
            &l,
            &alpha,
            Some(&placement),
            &p,
            policy,
            refined_settings,
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(refined.ground_conflict_cells > 0);
        assert_eq!(refined.status, "potential_conflicts");
    }
    #[test]
    fn clipped_bilinear_bounds_include_interior_height_grid_peaks() {
        let (mut l, _, _, _) = fixture();
        let h = l.height_resolution;
        let peak = crate::erosion::encode(10.);
        l.height[(h / 2 + 1) * h + h / 2 + 1] = peak;
        let world = |index: f64| (index / (h - 1) as f64 - 0.5) * 1000.;
        let low = world(h as f64 / 2. + 0.25 - 0.5);
        let high = world(h as f64 / 2. + 1.75 - 0.5);
        let mut audit = Audit::default();
        let (min, max) =
            clipped_ground_bounds(&l, [low; 2], [high; 2], &mut audit, settings(), &|| false)
                .unwrap()
                .unwrap();
        let expected = -500. + f64::from(peak) / 32766. * 1000.;
        assert!((max - expected).abs() < 1e-9);
        assert!(min >= 0. && min < expected);
        // The four rectangle corners miss the peak. Independent dense bilinear
        // samples (including the interior grid point) must remain bounded.
        for x in 0..=100 {
            for z in 0..=100 {
                let gx = 0.25 + 1.5 * x as f64 / 100.;
                let gz = 0.25 + 1.5 * z as f64 / 100.;
                let value = expected * (1. - (gx - 1.).abs()) * (1. - (gz - 1.).abs());
                assert!(value >= min - 1e-9 && value <= max + 1e-9);
            }
        }
        assert!(
            clipped_ground_bounds(
                &l,
                [low; 2],
                [high; 2],
                &mut Audit::default(),
                Settings {
                    maximum_work: 1,
                    ..settings()
                },
                &|| false
            )
            .is_err()
        );
        assert!(
            clipped_ground_bounds(
                &l,
                [low; 2],
                [high; 2],
                &mut Audit::default(),
                settings(),
                &|| true
            )
            .unwrap()
            .is_none()
        );
    }
    #[test]
    fn cancellation_budget_depth_and_map_edges_never_imply_clearance() {
        let (l, alpha, placement, p) = fixture();
        let before = l.height.clone();
        assert!(
            analyze(
                &l,
                &alpha,
                Some(&placement),
                &p,
                policy(),
                settings(),
                || true
            )
            .unwrap()
            .is_none()
        );
        assert!(
            analyze(
                &l,
                &alpha,
                Some(&placement),
                &p,
                policy(),
                Settings {
                    maximum_work: 1,
                    ..settings()
                },
                || false
            )
            .is_err()
        );
        let audit = analyze(
            &l,
            &alpha,
            Some(&placement),
            &p,
            policy(),
            Settings {
                maximum_depth: 1,
                ..settings()
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(audit.unresolved_geometry > 0);
        assert_ne!(audit.status, "raster_clear");
        let outside = Path::from_controls(&[[-499., 0., 0.], [-499., 0., 10.]], 10000, || false)
            .unwrap()
            .unwrap();
        let audit = analyze(
            &l,
            &alpha,
            Some(&placement),
            &outside,
            policy(),
            settings(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(audit.out_of_bounds_leaves > 0);
        assert_ne!(audit.status, "raster_clear");
        assert_eq!(l.height, before);
        assert!(
            analyze(
                &l,
                &alpha[..1],
                Some(&placement),
                &p,
                policy(),
                settings(),
                || false
            )
            .is_err()
        );
    }
}
