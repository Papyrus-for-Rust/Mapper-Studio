//! Versioned, bit-preserving final topology overrides. No world geometry changes.
use serde::{Deserialize, Serialize};
pub const VERSION: &str = "papyrus-topology-overrides-1";
pub const RESTORE_VERSION: &str = "papyrus-topology-overrides-2";
pub const BRUSH_VERSION: &str = "papyrus-topology-overrides-3";
const LIMIT: usize = 64_000_000;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Add,
    Remove,
    Restore,
    Invert,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    #[default]
    Round,
    Square,
}
impl Shape {
    fn is_round(&self) -> bool {
        *self == Self::Round
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stroke {
    pub operation: Operation,
    pub mask: u32,
    pub radius: f32,
    pub points: Vec<[f32; 2]>,
    #[serde(default, skip_serializing_if = "Shape::is_round")]
    pub shape: Shape,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edits {
    pub engine: String,
    pub source_sha256: String,
    pub size: u32,
    pub enabled: bool,
    pub strokes: Vec<Stroke>,
}
impl PartialEq for Edits {
    fn eq(&self, other: &Self) -> bool {
        // Recipe snapshots often share the same validated layer.
        std::ptr::eq(self, other)
            || (self.engine == other.engine
                && self.source_sha256 == other.source_sha256
                && self.size == other.size
                && self.enabled == other.enabled
                && self.strokes == other.strokes)
    }
}
impl Edits {
    pub fn validate(&self, size: u32) -> Result<(), String> {
        if ![VERSION, RESTORE_VERSION, BRUSH_VERSION].contains(&self.engine.as_str())
            || self.engine != BRUSH_VERSION
                && self
                    .strokes
                    .iter()
                    .any(|s| s.shape != Shape::Round || s.operation == Operation::Invert)
            || self.engine == VERSION
                && self
                    .strokes
                    .iter()
                    .any(|s| s.operation == Operation::Restore)
            || self.size != size
            || !(1000..=6000).contains(&size)
            || self.source_sha256.len() != 64
            || !self
                .source_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || self.strokes.len() > 256
            || self.strokes.iter().map(|s| s.points.len()).sum::<usize>() > 16384
        {
            return Err(
                "Unsupported topology override recipe, source, size or stroke budget".into(),
            );
        }
        for s in &self.strokes {
            if s.mask == 0
                || !s.radius.is_finite()
                || !(0.5..=256.).contains(&s.radius)
                || s.points.is_empty()
                || s.points.len() > 4096
                || s.points
                    .iter()
                    .flatten()
                    .any(|v| !v.is_finite() || v.abs() > size as f32 * 0.5)
            {
                return Err("Invalid topology stroke; keep its points inside the world and radius within 0.5–256 m".into());
            }
        }
        let (_, n) = crate::native_layers::dimensions(size)?;
        let mut work = 0usize;
        for stroke in &self.strokes {
            let r = stroke.radius as f64;
            let size = size as f64;
            for (a, b) in stroke
                .points
                .windows(2)
                .map(|p| (p[0], p[1]))
                .chain((stroke.points.len() == 1).then(|| (stroke.points[0], stroke.points[0])))
            {
                let lo_x = index(n, size, f64::from(a[0].min(b[0])) - r);
                let hi_x = index(n, size, f64::from(a[0].max(b[0])) + r);
                let lo_z = index(n, size, f64::from(a[1].min(b[1])) - r);
                let hi_z = index(n, size, f64::from(a[1].max(b[1])) + r);
                work = work.saturating_add((hi_x - lo_x + 1) * (hi_z - lo_z + 1));
                if work > LIMIT {
                    return Err(
                        "Topology painting work budget reached; split or simplify the edit layer"
                            .into(),
                    );
                }
            }
        }
        Ok(())
    }
    /// Forget manual writes to these bits everywhere, preserving all other edits.
    /// Returns false when the selected mask has no saved manual operations.
    pub fn reset_mask(&mut self, mask: u32) -> bool {
        let changed = self.strokes.iter().any(|s| s.mask & mask != 0);
        for stroke in &mut self.strokes {
            stroke.mask &= !mask;
        }
        self.strokes.retain(|s| s.mask != 0);
        changed
    }
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub source_sha256: String,
    pub strokes: usize,
    pub changed_cells: usize,
    pub skipped_hole_visits: usize,
    pub sample_visits: usize,
    pub object_filters_revalidated: bool,
}
pub struct Output {
    pub topology: Vec<i32>,
    pub report: Report,
}
fn index(n: usize, size: f64, x: f64) -> usize {
    ((x / size + 0.5) * n as f64).clamp(0., (n - 1) as f64) as usize
}
fn distance2(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let d = [b[0] - a[0], b[1] - a[1]];
    let len = d[0] * d[0] + d[1] * d[1];
    let t = if len == 0. {
        0.
    } else {
        (((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / len).clamp(0., 1.)
    };
    (p[0] - a[0] - d[0] * t).powi(2) + (p[1] - a[1] - d[1] * t).powi(2)
}
// Intersect the center segment with an axis-aligned square around the sample.
// This is the exact continuous sweep of a square brush, independent of events.
fn square_contains(p: [f64; 2], a: [f64; 2], b: [f64; 2], radius: f64) -> bool {
    let (mut enter, mut exit) = (0_f64, 1_f64);
    for axis in 0..2 {
        let d = b[axis] - a[axis];
        if d == 0. {
            if (p[axis] - a[axis]).abs() > radius {
                return false;
            }
        } else {
            let lo = (p[axis] - radius - a[axis]) / d;
            let hi = (p[axis] + radius - a[axis]) / d;
            enter = enter.max(lo.min(hi));
            exit = exit.min(lo.max(hi));
            if enter > exit {
                return false;
            }
        }
    }
    true
}
pub fn apply(
    base: &[i32],
    alpha: &[u8],
    resolution: usize,
    edits: &Edits,
    known_mask: u32,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    edits.validate(edits.size)?;
    let (_, expected) = crate::native_layers::dimensions(edits.size)?;
    if resolution != expected
        || base.len() != resolution * resolution
        || alpha.len() != base.len()
        || known_mask == 0
        || edits.strokes.iter().any(|s| s.mask & !known_mask != 0)
    {
        return Err("Topology override grid or source mask mismatch".into());
    }
    let mut topology = base.to_vec();
    let mut visits = 0;
    let mut holes = 0;
    if edits.enabled {
        let size = edits.size as f64;
        let n = resolution;
        for stroke in &edits.strokes {
            let radius = stroke.radius as f64;
            // Invert each covered cell once per gesture, even at self-crossings
            // and overlapping segments. The bounded bitset costs <= 512 KiB.
            let mut touched = if stroke.operation == Operation::Invert {
                vec![0_u64; base.len().div_ceil(64)]
            } else {
                Vec::new()
            };
            // A continuous swept circle avoids gaps and event-rate-dependent dot spacing.
            for pair in stroke
                .points
                .windows(2)
                .map(|p| (p[0], p[1]))
                .chain((stroke.points.len() == 1).then(|| (stroke.points[0], stroke.points[0])))
            {
                let a = pair.0.map(f64::from);
                let b = pair.1.map(f64::from);
                for z in index(n, size, a[1].min(b[1]) - radius)
                    ..=index(n, size, a[1].max(b[1]) + radius)
                {
                    if cancelled() {
                        return Ok(None);
                    }
                    for x in index(n, size, a[0].min(b[0]) - radius)
                        ..=index(n, size, a[0].max(b[0]) + radius)
                    {
                        if visits == LIMIT {
                            return Err("Topology painting work budget reached; split long strokes or reduce their radius".into());
                        }
                        visits += 1;
                        let point = [
                            ((x as f64 + 0.5) / n as f64 - 0.5) * size,
                            ((z as f64 + 0.5) / n as f64 - 0.5) * size,
                        ];
                        let inside = match stroke.shape {
                            Shape::Round => distance2(point, a, b) <= radius * radius,
                            Shape::Square => square_contains(point, a, b, radius),
                        };
                        if !inside {
                            continue;
                        }
                        let i = z * n + x;
                        if alpha[i] < 128 {
                            holes += 1;
                            continue;
                        }
                        if stroke.operation == Operation::Invert {
                            let bit = 1_u64 << (i % 64);
                            if touched[i / 64] & bit != 0 {
                                continue;
                            }
                            touched[i / 64] |= bit;
                        }
                        let old = topology[i] as u32;
                        topology[i] = match stroke.operation {
                            Operation::Add => old | stroke.mask,
                            Operation::Remove => old & !stroke.mask,
                            Operation::Restore => {
                                (old & !stroke.mask) | (base[i] as u32 & stroke.mask)
                            }
                            Operation::Invert => old ^ stroke.mask,
                        } as i32;
                    }
                }
            }
        }
    }
    if cancelled() {
        return Ok(None);
    }
    let changed_cells = topology.iter().zip(base).filter(|(a, b)| a != b).count();
    Ok(Some(Output {
        topology,
        report: Report {
            engine: if edits.engine == BRUSH_VERSION {
                BRUSH_VERSION
            } else if edits.engine == RESTORE_VERSION {
                RESTORE_VERSION
            } else {
                VERSION
            },
            source_sha256: edits.source_sha256.clone(),
            strokes: if edits.enabled {
                edits.strokes.len()
            } else {
                0
            },
            changed_cells,
            skipped_hole_visits: holes,
            sample_visits: visits,
            object_filters_revalidated: false,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn recipe(points: Vec<[f32; 2]>) -> Edits {
        Edits {
            engine: VERSION.into(),
            source_sha256: "a".repeat(64),
            size: 1000,
            enabled: true,
            strokes: vec![Stroke {
                shape: Default::default(),
                operation: Operation::Add,
                mask: 2,
                radius: 12.,
                points,
            }],
        }
    }
    fn run(e: &Edits, base: &[i32], alpha: &[u8]) -> Output {
        apply(base, alpha, 512, e, 3, || false).unwrap().unwrap()
    }
    #[test]
    fn strokes_preserve_unrelated_and_unknown_bits_and_follow_saved_order() {
        let n = 512;
        let base = vec![i32::MIN | 1; n * n];
        let alpha = vec![255; n * n];
        let mut e = recipe(vec![[-30., 0.], [30., 0.]]);
        e.strokes.push(Stroke {
            shape: Default::default(),
            operation: Operation::Remove,
            mask: 1,
            radius: 5.,
            points: vec![[0., 0.]],
        });
        let out = run(&e, &base, &alpha);
        assert!(out.report.changed_cells > 0);
        assert_eq!(out.topology[n / 2 * n + n / 2], i32::MIN | 2);
        assert!(
            out.topology
                .iter()
                .zip(&base)
                .all(|(a, b)| (*a as u32 & !3) == (*b as u32 & !3))
        );
        assert_eq!(base[0], i32::MIN | 1);
        assert!(!out.report.object_filters_revalidated);
    }
    #[test]
    fn continuous_sweeps_match_collinear_event_subdivision_and_round_trip() {
        let base = vec![0; 512 * 512];
        let alpha = vec![255; base.len()];
        let coarse = recipe(vec![[-100., -50.], [100., 50.]]);
        let fine = recipe(vec![
            [-100., -50.],
            [-50., -25.],
            [0., 0.],
            [50., 25.],
            [100., 50.],
        ]);
        assert_eq!(
            run(&coarse, &base, &alpha).topology,
            run(&fine, &base, &alpha).topology
        );
        let restored: Edits = serde_json::from_slice(&serde_json::to_vec(&fine).unwrap()).unwrap();
        assert_eq!(restored, fine);
    }
    #[test]
    fn holes_are_preserved_and_edge_brushes_clip_without_moving_the_grid() {
        let base = vec![0; 512 * 512];
        let mut alpha = vec![255; base.len()];
        alpha[256 * 512 + 256] = 0;
        let e = recipe(vec![[0., 0.]]);
        let out = run(&e, &base, &alpha);
        assert_eq!(out.topology[256 * 512 + 256], 0);
        assert!(out.report.skipped_hole_visits > 0);
        let edge = recipe(vec![[500., 500.]]);
        assert!(run(&edge, &base, &alpha).report.changed_cells > 0);
        assert_eq!(base.len(), 512 * 512);
    }
    #[test]
    fn disabled_layers_replay_from_clean_input_and_cancellation_publishes_nothing() {
        let base = vec![1; 512 * 512];
        let alpha = vec![255; base.len()];
        let mut e = recipe(vec![[0., 0.]]);
        let painted = run(&e, &base, &alpha);
        assert_ne!(painted.topology, base);
        e.enabled = false;
        assert_eq!(run(&e, &base, &alpha).topology, base);
        e.enabled = true;
        assert!(apply(&base, &alpha, 512, &e, 3, || true).unwrap().is_none());
        assert!(base.iter().all(|v| *v == 1));
    }
    #[test]
    fn restore_uses_generated_bits_in_saved_order_and_preserves_holes() {
        let n = 512;
        let base: Vec<_> = (0..n * n)
            .map(|i| i32::MIN | 1 | if i % n < n / 2 { 2 } else { 0 })
            .collect();
        let mut alpha = vec![255; base.len()];
        alpha[n / 2 * n + n / 2] = 0;
        let mut edits = recipe(vec![[-40., 0.], [40., 0.]]);
        edits.engine = RESTORE_VERSION.into();
        edits.strokes[0].mask = 3;
        edits.strokes[0].radius = 30.;
        edits.strokes.push(Stroke {
            shape: Default::default(),
            operation: Operation::Remove,
            mask: 3,
            radius: 25.,
            points: vec![[-30., 0.], [30., 0.]],
        });
        let painted = run(&edits, &base, &alpha);
        edits.strokes.push(Stroke {
            shape: Default::default(),
            operation: Operation::Restore,
            mask: 2,
            radius: 20.,
            points: vec![[-20., 0.], [20., 0.]],
        });
        let restored = run(&edits, &base, &alpha);
        assert_eq!(restored.report.engine, RESTORE_VERSION);
        assert_ne!(restored.topology, painted.topology);
        assert_eq!(
            restored.topology[n / 2 * n + n / 2],
            base[n / 2 * n + n / 2]
        );
        for (i, &cell) in restored.topology.iter().enumerate() {
            assert_eq!(cell as u32 & !2, painted.topology[i] as u32 & !2);
        }
        // Restore writes each cell's baseline, rather than a constant add/remove.
        for x in [n / 2 - 3, n / 2 + 3] {
            let i = n / 2 * n + x;
            assert_eq!(restored.topology[i] & 2, base[i] & 2);
            assert_eq!(restored.topology[i] & 1, 0);
        }
        let mut later = edits.strokes.last().unwrap().clone();
        later.operation = Operation::Add;
        edits.strokes.push(later);
        assert_eq!(
            run(&edits, &base, &alpha).topology[n / 2 * n + n / 2 + 3] & 2,
            2
        );
        let json = serde_json::to_vec(&edits).unwrap();
        assert_eq!(serde_json::from_slice::<Edits>(&json).unwrap(), edits);
        edits.engine = VERSION.into();
        assert!(edits.validate(1000).is_err());
    }
    #[test]
    fn reset_selected_mask_keeps_other_operations_and_disabled_state() {
        let mut edits = recipe(vec![[0., 0.]]);
        edits.enabled = false;
        edits.engine = RESTORE_VERSION.into();
        edits.strokes[0].mask = 3;
        edits.strokes.push(Stroke {
            shape: Default::default(),
            operation: Operation::Restore,
            mask: 2,
            radius: 10.,
            points: vec![[2., 3.]],
        });
        assert!(!edits.reset_mask(4));
        assert!(edits.reset_mask(2));
        assert!(!edits.enabled);
        assert_eq!(edits.strokes.len(), 1);
        assert_eq!(edits.strokes[0].mask, 1);
        edits.validate(1000).unwrap();
        assert!(edits.reset_mask(1));
        assert!(edits.strokes.is_empty());
    }
    #[test]
    fn invalid_masks_dimensions_sizes_and_excess_work_fail_before_publication() {
        let base = vec![0; 512 * 512];
        let alpha = vec![255; base.len()];
        let mut e = recipe(vec![[0., 0.]]);
        assert!(apply(&base, &alpha, 512, &e, 1, || false).is_err());
        assert!(apply(&base, &alpha, 513, &e, 3, || false).is_err());
        e.strokes[0].points[0] = [501., 0.];
        assert!(e.validate(1000).is_err());
        e.strokes[0].points[0] = [0., 0.];
        assert!(e.validate(2000).is_err());
        let mut huge = recipe(
            (0..1000)
                .map(|i| {
                    if i % 2 == 0 {
                        [-500., -500.]
                    } else {
                        [500., 500.]
                    }
                })
                .collect(),
        );
        huge.strokes[0].radius = 256.;
        assert!(huge.validate(1000).is_err());
    }

    #[test]
    fn square_brush_is_continuous_and_preserves_old_round_recipes() {
        let base = vec![i32::MIN; 512 * 512];
        let alpha = vec![255; base.len()];
        let old = recipe(vec![[0., 0.]]);
        let bytes = serde_json::to_string(&old).unwrap();
        assert!(!bytes.contains("shape"));
        assert_eq!(
            serde_json::from_str::<Edits>(&bytes).unwrap().strokes[0].shape,
            Shape::Round
        );
        let mut square = old.clone();
        square.engine = BRUSH_VERSION.into();
        square.strokes[0].shape = Shape::Square;
        square.strokes[0].mask = 3;
        let round = run(&old, &base, &alpha);
        let painted = run(&square, &base, &alpha);
        assert!(painted.report.changed_cells > round.report.changed_cells);
        assert!(
            painted
                .topology
                .iter()
                .all(|v| *v == i32::MIN || *v == i32::MIN | 3)
        );
        for shape in [Shape::Round, Shape::Square] {
            let mut coarse = square.clone();
            coarse.strokes[0].shape = shape;
            coarse.strokes[0].points = vec![[-60., -30.], [60., 30.]];
            let mut fine = coarse.clone();
            fine.strokes[0].points.insert(1, [0., 0.]);
            assert_eq!(
                run(&coarse, &base, &alpha).topology,
                run(&fine, &base, &alpha).topology
            );
            fine.strokes[0].points.reverse();
            assert_eq!(
                run(&coarse, &base, &alpha).topology,
                run(&fine, &base, &alpha).topology
            );
        }
        let bytes = serde_json::to_vec(&square).unwrap();
        assert_eq!(serde_json::from_slice::<Edits>(&bytes).unwrap(), square);
        for version in [VERSION, RESTORE_VERSION] {
            square.engine = version.into();
            assert!(square.validate(1000).is_err());
        }
    }

    #[test]
    fn invert_flips_multiple_flags_once_per_stroke_including_self_overlap() {
        let base: Vec<_> = (0..512 * 512)
            .map(|i| i32::MIN | if i % 2 == 0 { 1 } else { 2 })
            .collect();
        let mut alpha = vec![255; base.len()];
        alpha[256 * 512 + 256] = 0;
        for shape in [Shape::Round, Shape::Square] {
            let mut edits = recipe(vec![[-40., 0.], [40., 0.]]);
            edits.engine = BRUSH_VERSION.into();
            edits.strokes[0].shape = shape;
            edits.strokes[0].mask = 3;
            let covered = run(&edits, &vec![0; base.len()], &alpha);
            edits.strokes[0].operation = Operation::Invert;
            edits.strokes[0].points = vec![[-40., 0.], [0., 0.], [40., 0.], [-40., 0.]];
            let output = run(&edits, &base, &alpha);
            assert_eq!(output.report.engine, BRUSH_VERSION);
            for (i, &cell) in output.topology.iter().enumerate() {
                assert_eq!(
                    cell,
                    if covered.topology[i] != 0 {
                        base[i] ^ 3
                    } else {
                        base[i]
                    }
                );
            }
            assert_eq!(output.topology[256 * 512 + 256], base[256 * 512 + 256]);
            edits.strokes.push(edits.strokes[0].clone());
            assert_eq!(run(&edits, &base, &alpha).topology, base);
            assert!(
                apply(&base, &alpha, 512, &edits, 3, || true)
                    .unwrap()
                    .is_none()
            );
            edits.engine = RESTORE_VERSION.into();
            assert!(edits.validate(1000).is_err());
        }
    }
}
