//! Shared source-style PathList surface sweeps and absolute material writes.
//! Callbacks must write private candidate buffers; cancellation cannot undo a
//! caller's external effects. Public adapters own/discard those private buffers.
pub(crate) use crate::path_terrain::distance_xz;
use crate::{path_geometry::Path, rail_reservations::QuadCoverage};
pub const VERSION: &str = "papyrus-path-surface-sweeps-1";
pub(crate) const BYTE_TO_FLOAT: f32 = 0.003921569;
pub(crate) fn inverse(a: f32, b: f32, v: f32) -> f32 {
    if a == b {
        0.
    } else {
        ((v - a) / (b - a)).clamp(0., 1.)
    }
}
pub(crate) fn norm(v: [f32; 3]) -> Option<[f32; 3]> {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    (l > 0.00001).then(|| v.map(|v| v / l))
}
pub(crate) fn float_byte(v: f32) -> u8 {
    // Union32.b1 is FieldOffset(0), the lowest byte, despite its one-based name.
    let trimmed = f32::from_bits(v.to_bits() & 0xffffff00);
    // Positive bounded channel ratios can exceed one when source bytes do not
    // sum to 255. Preserve the inspected unchecked byte conversion, not Rust's
    // saturating float-to-u8 conversion.
    (((trimmed * 255. + 0.5) as u32) & 255) as u8
}
pub(crate) fn set_splat(splat: &mut [u8], cells: usize, i: usize, channel: usize, value: f32) {
    let old = splat[channel * cells + i] as f32 * BYTE_TO_FLOAT;
    if old >= 1. {
        return;
    }
    let factor = (1. - value) / (1. - old);
    for c in 0..8 {
        let q = c * cells + i;
        splat[q] = if c == channel {
            float_byte(value)
        } else {
            float_byte(factor * (splat[q] as f32 * BYTE_TO_FLOAT))
        };
    }
}

pub(crate) fn visit_quad(
    quad: [[i32; 2]; 4],
    w: &mut Work<'_>,
    mut callback: impl FnMut(i32, i32),
) -> Result<usize, Failure> {
    let mut visits = 0;
    let minx = quad.iter().map(|p| p[0]).min().unwrap();
    let maxx = quad.iter().map(|p| p[0]).max().unwrap();
    let minz = quad.iter().map(|p| p[1]).min().unwrap();
    let maxz = quad.iter().map(|p| p[1]).max().unwrap();
    let width = (maxx - minx + 1) as usize;
    let coverage = QuadCoverage::new(quad);
    for z in minz..=maxz {
        // Poll cancellation once per raster row. On a short budget, visit
        // the same prefix of the row before reporting the exhausted cell.
        let count = width.min(w.limit - w.used);
        w.visit(count)?;
        let row = coverage.row(z);
        for x in minx..minx + count as i32 {
            if row.contains(x) {
                callback(x, z);
                visits += 1;
            }
        }
        if count < width {
            w.visit(1)?;
        }
    }
    Ok(visits)
}
pub(crate) fn ray_distance(origin: [f32; 3], direction: [f32; 3], world: [f32; 3]) -> f32 {
    let dot = (world[0] - origin[0]) * direction[0]
        + (world[1] - origin[1]) * direction[1]
        + (world[2] - origin[2]) * direction[2];
    distance_xz(
        world,
        std::array::from_fn(|i| origin[i] + dot * direction[i]),
    )
}
#[derive(Debug)]
pub(crate) enum Failure {
    Cancelled,
    Error(String),
}

pub(crate) struct Work<'a> {
    pub(crate) used: usize,
    pub(crate) limit: usize,
    pub(crate) cancelled: &'a dyn Fn() -> bool,
}
impl Work<'_> {
    pub(crate) fn visit(&mut self, n: usize) -> Result<(), Failure> {
        if (self.cancelled)() {
            return Err(Failure::Cancelled);
        }
        if n > self.limit - self.used {
            return Err(Failure::Error("Path surface work budget exceeded".into()));
        }
        self.used += n;
        Ok(())
    }
}
pub struct PathSpec<'a> {
    pub path: &'a Path,
    pub width: f32,
    /// Inner then outer padding.
    pub padding: [f32; 2],
}
#[derive(Clone, Copy)]
pub struct Grid {
    pub size: f32,
    pub resolution: usize,
    pub origin_y: f32,
    pub vertical_span: f32,
}
#[derive(Debug, serde::Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub sweeps: usize,
    pub callbacks: usize,
    pub work: usize,
}
/// Ordered reverse-path sweeps with per-path authored shape and radius sampling.
/// Geometry/work is bounded independently of a river or road family policy.
#[allow(clippy::too_many_arguments)]
pub fn walk(
    paths: &[PathSpec<'_>],
    g: Grid,
    spacing: f32,
    maximum_work: usize,
    mut radius: impl FnMut(usize, usize, f32, f32) -> Result<f32, String>,
    cancelled: &dyn Fn() -> bool,
    mut callback: impl FnMut(usize, usize, f32),
) -> Result<Option<Report>, String> {
    if cancelled() {
        return Ok(None);
    }
    if paths.len() > 4096
        || paths.iter().map(|p| p.path.points().len()).sum::<usize>() > 1_048_576
        || !(2..=2048).contains(&g.resolution)
        || !g.size.is_finite()
        || !(1. ..=6000.).contains(&g.size)
        || !g.origin_y.is_finite()
        || g.origin_y.abs() > 10000.
        || !g.vertical_span.is_finite()
        || !(1. ..=20000.).contains(&g.vertical_span)
        || ![0.125, 0.25, 0.5, 1., 2., 4., 8.].contains(&spacing)
        || !(1..=512_000_000).contains(&maximum_work)
        || paths.iter().any(|s| {
            !s.width.is_finite()
                || !(0.0001..=10000.).contains(&s.width)
                || s.padding
                    .iter()
                    .any(|v| !v.is_finite() || !(0. ..=256.).contains(v))
                || !s.path.length().is_finite()
                || s.path.length() <= 0.
                || s.path.length() > 40000.
                || s.path.range() != (0, s.path.points().len() - 1)
        })
    {
        return Err("Invalid path surface geometry or resource allowance".into());
    }
    let mut w = Work {
        used: 0,
        limit: maximum_work,
        cancelled,
    };
    let mut report = Report {
        engine: VERSION,
        sweeps: 0,
        callbacks: 0,
        work: 0,
    };
    let n = g.resolution;
    let world_axis: Vec<_> = (0..n)
        .map(|cell| -g.size * 0.5 + ((cell as f32 + 0.5) / n as f32) * g.size)
        .collect();
    let index = |p: [f32; 3]| {
        [p[0], p[2]].map(|v| {
            (((v + g.size * 0.5) * (1. / g.size)) * n as f32).clamp(0., (n - 1) as f32) as i32
        })
    };
    let edges = |p: [f32; 3], t: [f32; 3], r: f32| -> Result<[[f32; 3]; 2], Failure> {
        let h = norm([t[0], 0., t[2]])
            .ok_or_else(|| Failure::Error("Vertical path surface tangent is unsupported".into()))?;
        let side = [h[2], 0., -h[0]];
        Ok([
            std::array::from_fn(|i| p[i] - side[i] * r),
            std::array::from_fn(|i| p[i] + side[i] * r),
        ])
    };
    let result = (|| -> Result<(), Failure> {
        for (path_index, spec) in paths.iter().enumerate().rev() {
            w.visit(1)?;
            let path = spec.path;
            let start = path.points()[0];
            let end = *path.points().last().expect("validated controls");
            let base = spec.width * 0.5;
            let mut previous = edges(start, path.tangents()[0], base + spec.padding[1])?;
            let count = ((path.length() + spacing) / spacing).ceil() as usize;
            for step in 0..count {
                w.visit(1)?;
                let distance = step as f32 * spacing;
                let (centre, tangent) = path
                    .surface_sample(distance)
                    .map_err(|e| Failure::Error(format!("Path surface tangent failed: {e:?}")))?;
                let ray = norm(tangent)
                    .ok_or_else(|| Failure::Error("Invalid path surface ray".into()))?;
                let r =
                    radius(path_index, step, distance, path.length()).map_err(Failure::Error)?;
                if !r.is_finite() || r <= 0. {
                    return Err(Failure::Error("Invalid path surface radius".into()));
                }
                let opacity = if path.is_circular() {
                    1.
                } else {
                    inverse(
                        0.,
                        base,
                        distance_xz(start, centre).min(distance_xz(end, centre)),
                    )
                };
                let current = edges(centre, tangent, r + spec.padding[1])?;
                let quad = [
                    index(previous[0]),
                    index(previous[1]),
                    index(current[0]),
                    index(current[1]),
                ];
                let wy = g.origin_y
                    + ((centre[1] - g.origin_y) * (1. / g.vertical_span)) * g.vertical_span;
                report.callbacks += visit_quad(quad, &mut w, |x, z| {
                    let world = [world_axis[x as usize], wy, world_axis[z as usize]];
                    let alpha = inverse(
                        r + spec.padding[1],
                        r - spec.padding[0],
                        ray_distance(centre, ray, world),
                    ) * opacity;
                    callback(path_index, z as usize * n + x as usize, alpha);
                })?;
                previous = current;
                report.sweeps += 1;
            }
        }
        w.visit(0)?;
        Ok(())
    })();
    match result {
        Ok(()) if cancelled() => Ok(None),
        Ok(()) => {
            report.work = w.used;
            Ok(Some(report))
        }
        Err(Failure::Cancelled) => Ok(None),
        Err(Failure::Error(e)) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quad_budget_preserves_order_and_partial_progress() {
        let quad = [[0, 0], [3, 0], [0, 2], [3, 2]];
        for limit in 0..=13 {
            let mut work = Work {
                used: 0,
                limit,
                cancelled: &|| false,
            };
            let mut cells = Vec::new();
            let result = visit_quad(quad, &mut work, |x, z| cells.push((x, z)));
            let expected: Vec<_> = (0..3)
                .flat_map(|z| (0..4).map(move |x| (x, z)))
                .take(limit)
                .collect();
            assert_eq!(cells, expected);
            assert_eq!(work.used, expected.len());
            match result {
                Ok(visits) => {
                    assert!(limit >= 12);
                    assert_eq!(visits, 12);
                }
                Err(Failure::Error(_)) => assert!(limit < 12),
                Err(Failure::Cancelled) => panic!("unexpected cancellation"),
            }
        }
    }

    #[test]
    fn source_absolute_writes_can_produce_and_paint_empty_material_cells() {
        let mut channels = [0; 8];
        channels[7] = 254;
        super::set_splat(&mut channels, 1, 0, 7, 0.);
        assert_eq!(channels, [0; 8]);
        super::set_splat(&mut channels, 1, 0, 7, 0.5);
        assert_eq!(channels[7], 128);
        assert!(channels[..7].iter().all(|&v| v == 0));
    }
}
