//! Bounded downward queries and water-map rasterization of raw source triangles.
//! This does not emulate PhysX cooking, terrain clearance or water-body topology rules.
pub const MAX_VERTICES: usize = 65_536;
pub const MAX_TRIANGLES: usize = 262_144;
pub const RASTER_VERSION: &str = "papyrus-water-surface-raster-1";

/// A raw world-space surface and its explicit downward-ray face policy.
pub struct RasterSurface<'a, 'mesh> {
    pub surface: &'a Surface<'mesh>,
    pub two_sided: bool,
}

pub struct RasterGrid<'a> {
    pub size: f64,
    pub resolution: usize,
    /// Encoded -500..500 metre heights; zero is the dry sentinel.
    pub existing: &'a [i16],
}

pub struct Rasterized {
    pub water: Vec<i16>,
    pub report: RasterReport,
}

#[derive(Debug, serde::Serialize)]
pub struct RasterReport {
    pub engine: &'static str,
    pub surfaces: usize,
    pub triangles: usize,
    pub changed_samples: usize,
    pub work: usize,
    pub physics_cooking_validated: bool,
}

/// Raise-only water heights at TerrainMap-style cell centres, including on the
/// height-resolution grid. Terrain heights instead use endpoint sampling.
/// Triangle bounds limit work; holes and arbitrary rotations remain geometric.
/// Private output is discarded on cancellation, invalid input or budget failure.
/// The caller must supply eligible water surfaces: this primitive deliberately
/// does not perform AddToWaterMap terrain-clearance, lake or topology decisions.
pub fn rasterize(
    grid: RasterGrid<'_>,
    surfaces: &[RasterSurface<'_, '_>],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Rasterized>, String> {
    rasterize_impl(grid, surfaces, maximum_work, None, cancelled)
}
/// Raw highest collider heights at cell centres. NaN means no triangle hit;
/// callers choose an explicit no-hit policy rather than confusing it with zero.
pub struct Heights {
    pub heights: Vec<f32>,
    pub report: RasterReport,
}
pub fn rasterize_heights(
    size: f64,
    resolution: usize,
    surfaces: &[RasterSurface<'_, '_>],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Heights>, String> {
    if cancelled() {
        return Ok(None);
    }
    if !(2..=4097).contains(&resolution)
        || !size.is_finite()
        || !(0. ..=6000.).contains(&size)
        || size == 0.
        || surfaces.len() > 4096
        || !(1..=128_000_000).contains(&maximum_work)
    {
        return Err("Invalid raw water raster grid or limits".into());
    }
    let cells = resolution * resolution;
    let preparation = cells * 2;
    let remaining = maximum_work
        .checked_sub(preparation)
        .filter(|v| *v > 0)
        .ok_or("Raw water raster preparation budget exceeded")?;
    let mut heights = Vec::with_capacity(cells);
    let mut dry = Vec::with_capacity(cells);
    while heights.len() < cells {
        if cancelled() {
            return Ok(None);
        }
        let count = (cells - heights.len()).min(4096);
        heights.extend(std::iter::repeat_n(f32::NAN, count));
        dry.extend(std::iter::repeat_n(0, count));
    }
    let Some(result) = rasterize_mapped(
        RasterGrid {
            size,
            resolution,
            existing: &dry,
        },
        surfaces,
        remaining,
        |x, z, y| {
            let h = &mut heights[z * resolution + x];
            *h = if h.is_nan() {
                y as f32
            } else {
                h.max(y as f32)
            };
            Ok(None)
        },
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let mut report = result.report;
    report.engine = "papyrus-water-collider-height-raster-1";
    report.work += preparation;
    report.changed_samples = 0;
    for chunk in heights.chunks(4096) {
        if cancelled() {
            return Ok(None);
        }
        report.work += chunk.len();
        if report.work > maximum_work {
            return Err("Raw water raster work budget exceeded".into());
        }
        report.changed_samples += chunk.iter().filter(|v| v.is_finite()).count();
    }
    Ok(Some(Heights { heights, report }))
}
type HeightMapper<'a> = dyn FnMut(usize, usize, f64) -> Result<Option<i16>, String> + 'a;
/// Internal registration hook: evaluate raw intersections before quantization.
/// The mapped height must remain monotonic in raw Y for highest-hit semantics.
pub(crate) fn rasterize_mapped(
    grid: RasterGrid<'_>,
    surfaces: &[RasterSurface<'_, '_>],
    maximum_work: usize,
    mut mapper: impl FnMut(usize, usize, f64) -> Result<Option<i16>, String>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Rasterized>, String> {
    rasterize_impl(grid, surfaces, maximum_work, Some(&mut mapper), cancelled)
}
fn rasterize_impl(
    grid: RasterGrid<'_>,
    surfaces: &[RasterSurface<'_, '_>],
    maximum_work: usize,
    mut mapper: Option<&mut HeightMapper<'_>>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Rasterized>, String> {
    if cancelled() {
        return Ok(None);
    }
    let n = grid.resolution;
    if !(2..=4097).contains(&n)
        || n.checked_mul(n) != Some(grid.existing.len())
        || !grid.size.is_finite()
        || !(0. ..=6000.).contains(&grid.size)
        || grid.size == 0.
        || surfaces.len() > 4096
        || !(1..=128_000_000).contains(&maximum_work)
    {
        return Err("Invalid water raster grid or limits".into());
    }
    let mut work = 0;
    let mut visit = || -> Result<(), String> {
        if work >= maximum_work {
            return Err("Water raster work budget exceeded".into());
        }
        work += 1;
        Ok(())
    };
    for chunk in grid.existing.chunks(4096) {
        if cancelled() {
            return Ok(None);
        }
        for &h in chunk {
            visit()?;
            if !(0..=32766).contains(&h) {
                return Err("Invalid existing water height".into());
            }
        }
    }
    let mut water = grid.existing.to_vec();
    let mut triangles = 0;
    // Inclusive bounds in sample-index space. Clip only after rejecting a
    // completely outside triangle, so it cannot paint a terrain-edge cell.
    let range = |lo: f64, hi: f64| {
        let a = (lo / grid.size + 0.5) * n as f64 - 0.5;
        let b = (hi / grid.size + 0.5) * n as f64 - 0.5;
        if b < -1e-9 || a > (n - 1) as f64 + 1e-9 {
            return None;
        }
        let first = (a - 1e-9).ceil().max(0.);
        let last = (b + 1e-9).floor().min((n - 1) as f64);
        (first <= last).then_some(first as usize..=last as usize)
    };
    for entry in surfaces {
        if cancelled() {
            return Ok(None);
        }
        for chunk in entry.surface.vertices.chunks(4096) {
            if cancelled() {
                return Ok(None);
            }
            for v in chunk {
                visit()?;
                if !(-500. ..=500.).contains(&v[1]) {
                    return Err("Water surface exceeds encodable height range".into());
                }
            }
        }
        for triangle in entry.surface.triangles {
            if cancelled() {
                return Ok(None);
            }
            visit()?;
            triangles += 1;
            let [a, b, c] = triangle.map(|i| entry.surface.vertices[i as usize]);
            let dx = b[0] - a[0];
            let dz = b[2] - a[2];
            let ex = c[0] - a[0];
            let ez = c[2] - a[2];
            let det = dx * ez - dz * ex;
            let scale = dx.abs().max(dz.abs()).max(ex.abs()).max(ez.abs());
            if det.abs() <= scale * scale * 1e-12 || (!entry.two_sided && det >= 0.) {
                continue;
            }
            let Some(xs) = range(a[0].min(b[0]).min(c[0]), a[0].max(b[0]).max(c[0])) else {
                continue;
            };
            let Some(zs) = range(a[2].min(b[2]).min(c[2]), a[2].max(b[2]).max(c[2])) else {
                continue;
            };
            for z in zs {
                if cancelled() {
                    return Ok(None);
                }
                let pz = ((z as f64 + 0.5) / n as f64 - 0.5) * grid.size - a[2];
                for x in xs.clone() {
                    visit()?;
                    let px = ((x as f64 + 0.5) / n as f64 - 0.5) * grid.size - a[0];
                    let u = (px * ez - pz * ex) / det;
                    let v = (dx * pz - dz * px) / det;
                    if u < -1e-9 || v < -1e-9 || u + v > 1. + 1e-9 {
                        continue;
                    }
                    let y = a[1] + u * (b[1] - a[1]) + v * (c[1] - a[1]);
                    // Clamp only roundoff at a validated triangle's extrema.
                    let encoded = if let Some(mapper) = mapper.as_mut() {
                        visit()?;
                        let Some(encoded) = mapper(x, z, y)? else {
                            continue;
                        };
                        if !(0..=32766).contains(&encoded) {
                            return Err("Invalid mapped water height".into());
                        }
                        encoded
                    } else {
                        crate::erosion::encode(y.clamp(-500., 500.) as f32)
                    };
                    let h = &mut water[z * n + x];
                    *h = (*h).max(encoded);
                }
            }
        }
    }
    let mut changed_samples = 0;
    for (a, b) in water.chunks(4096).zip(grid.existing.chunks(4096)) {
        if cancelled() {
            return Ok(None);
        }
        for (a, b) in a.iter().zip(b) {
            visit()?;
            changed_samples += usize::from(a != b);
        }
    }
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(Rasterized {
        water,
        report: RasterReport {
            engine: RASTER_VERSION,
            surfaces: surfaces.len(),
            triangles,
            changed_samples,
            work,
            physics_cooking_validated: false,
        },
    }))
}

pub struct Surface<'a> {
    vertices: &'a [[f64; 3]],
    triangles: &'a [[u32; 3]],
    pub minimum: [f64; 3],
    pub maximum: [f64; 3],
}
impl<'a> Surface<'a> {
    pub fn new(vertices: &'a [[f64; 3]], triangles: &'a [[u32; 3]]) -> Result<Self, String> {
        if vertices.is_empty()
            || vertices.len() > MAX_VERTICES
            || triangles.is_empty()
            || triangles.len() > MAX_TRIANGLES
        {
            return Err("Water collision mesh exceeds vertex/triangle bounds".into());
        }
        let mut minimum = [f64::INFINITY; 3];
        let mut maximum = [f64::NEG_INFINITY; 3];
        for v in vertices {
            for axis in 0..3 {
                if !v[axis].is_finite() || v[axis].abs() > 1_000_000. {
                    return Err("Invalid water collision vertex".into());
                }
                minimum[axis] = minimum[axis].min(v[axis]);
                maximum[axis] = maximum[axis].max(v[axis]);
            }
        }
        if triangles
            .iter()
            .flatten()
            .any(|i| *i as usize >= vertices.len())
        {
            return Err("Water triangle references a missing vertex".into());
        }
        Ok(Self {
            vertices,
            triangles,
            minimum,
            maximum,
        })
    }
    /// Highest raw triangle intersection along -Y, from above the mesh bounds.
    /// Explicit face policy; double-sided results must not be presented as Unity raycast parity.
    /// `work` is shared across queries, bounding future raster loops as well as one query.
    pub fn highest(
        &self,
        x: f64,
        z: f64,
        two_sided: bool,
        work: &mut usize,
        limit: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<f64>, String> {
        if !x.is_finite() || !z.is_finite() {
            return Err("Invalid water surface query".into());
        }
        if cancelled() {
            return Err("Water surface query cancelled".into());
        }
        if x < self.minimum[0] || x > self.maximum[0] || z < self.minimum[2] || z > self.maximum[2]
        {
            return Ok(None);
        }
        let mut height: Option<f64> = None;
        for (i, t) in self.triangles.iter().enumerate() {
            if i % 64 == 0 && cancelled() {
                return Err("Water surface query cancelled".into());
            }
            if *work >= limit {
                return Err("Water surface triangle budget exceeded".into());
            }
            *work += 1;
            let [a, b, c] = t.map(|j| self.vertices[j as usize]);
            let dx = b[0] - a[0];
            let dz = b[2] - a[2];
            let ex = c[0] - a[0];
            let ez = c[2] - a[2];
            let determinant = dx * ez - dz * ex;
            let scale = dx.abs().max(dz.abs()).max(ex.abs()).max(ez.abs());
            if determinant.abs() <= scale * scale * 1e-12 || (!two_sided && determinant >= 0.) {
                continue;
            }
            let px = x - a[0];
            let pz = z - a[2];
            let u = (px * ez - pz * ex) / determinant;
            let v = (dx * pz - dz * px) / determinant;
            if u >= -1e-9 && v >= -1e-9 && u + v <= 1. + 1e-9 {
                let y = a[1] + u * (b[1] - a[1]) + v * (c[1] - a[1]);
                height = Some(height.map_or(y, |old| old.max(y)));
            }
        }
        Ok(height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::erosion::encode;
    #[test]
    fn mapped_registration_filters_raw_intersections_before_quantization() {
        let a = [[-1., 10.0001, -1.], [-1., 10.0001, 1.], [1., 10.0001, -1.]];
        let b = a.map(|[x, _, z]| [x, 10.0002, z]);
        let t = [[0, 1, 2]];
        let water = vec![0; 81];
        assert_eq!(encode(10.0001), encode(10.0002));
        for (v, accepted) in [(&a, false), (&b, true)] {
            let surface = Surface::new(v, &t).unwrap();
            let result = rasterize_mapped(
                RasterGrid {
                    size: 9.,
                    resolution: 9,
                    existing: &water,
                },
                &[RasterSurface {
                    surface: &surface,
                    two_sided: false,
                }],
                1000,
                |_, _, y| Ok((y >= 10.00015).then_some(encode(y as f32))),
                || false,
            )
            .unwrap()
            .unwrap();
            assert_eq!(result.report.changed_samples > 0, accepted);
        }
        assert!(water.iter().all(|&v| v == 0));
    }

    #[test]
    fn raster_matches_independent_downward_queries_at_arbitrary_rotations() {
        let triangles = [[0, 1, 2], [2, 1, 3], [3, 1, 2], [0, 0, 0]];
        let n = 65;
        let original = vec![encode(-30.); n * n];
        for yaw in [0_f64, 13., 37., 90., 143., 271., 359.] {
            let (s, c) = yaw.to_radians().sin_cos();
            let vertices: Vec<_> = [[-7., -3.], [-7., 3.], [7., -3.], [7., 3.]]
                .map(|[x, z]| [5. + x * c - z * s, 9. + 0.2 * x, -4. + x * s + z * c])
                .into();
            let surface = Surface::new(&vertices, &triangles).unwrap();
            for two_sided in [false, true] {
                let result = rasterize(
                    RasterGrid {
                        size: 65.,
                        resolution: n,
                        existing: &original,
                    },
                    &[RasterSurface {
                        surface: &surface,
                        two_sided,
                    }],
                    100_000,
                    || false,
                )
                .unwrap()
                .unwrap();
                let mut query_work = 0;
                let expected: Vec<_> = (0..n * n)
                    .map(|i| {
                        let x = (i % n) as f64 - 32.;
                        let z = (i / n) as f64 - 32.;
                        surface
                            .highest(x, z, two_sided, &mut query_work, 100_000, || false)
                            .unwrap()
                            .map_or(original[i], |y| original[i].max(encode(y as f32)))
                    })
                    .collect();
                let raw = rasterize_heights(
                    65.,
                    n,
                    &[RasterSurface {
                        surface: &surface,
                        two_sided,
                    }],
                    100_000,
                    || false,
                )
                .unwrap()
                .unwrap();
                for (i, &h) in raw.heights.iter().enumerate() {
                    let hit = surface
                        .highest(
                            (i % n) as f64 - 32.,
                            (i / n) as f64 - 32.,
                            two_sided,
                            &mut query_work,
                            200_000,
                            || false,
                        )
                        .unwrap();
                    match hit {
                        Some(y) => assert!((h - y as f32).abs() < 0.00001),
                        None => assert!(h.is_nan()),
                    }
                }
                assert!(!raw.report.physics_cooking_validated);
                assert_eq!(result.water, expected, "yaw {yaw}, two sided {two_sided}");
                assert!(result.report.changed_samples > 0);
                assert!(!result.report.physics_cooking_validated);
                // Bounding triangles avoids a full grid-by-triangle scan.
                assert!(result.report.work < 2 * n * n + 1200);
            }
        }
        assert!(original.iter().all(|&h| h == encode(-30.)));
    }

    #[test]
    fn raw_heights_keep_negative_hits_and_holes_and_obey_limits() {
        let vertices = [[-2., -0.5, -2.], [-2., -0.5, 2.], [2., -0.5, -2.]];
        let triangles = [[0, 1, 2]];
        let surface = Surface::new(&vertices, &triangles).unwrap();
        let entries = [RasterSurface {
            surface: &surface,
            two_sided: false,
        }];
        let run =
            |budget, cancelled: fn() -> bool| rasterize_heights(9., 9, &entries, budget, cancelled);
        let raw = run(1000, || false).unwrap().unwrap();
        assert_eq!(raw.heights[40], -0.5);
        assert!(raw.heights[60].is_nan());
        assert_eq!(
            raw.report.changed_samples,
            raw.heights.iter().filter(|v| v.is_finite()).count()
        );
        assert!(raw.report.work <= 1000);
        assert!(run(1, || false).is_err());
        assert!(run(1000, || true).unwrap().is_none());
        let calls = std::cell::Cell::new(0);
        assert!(
            rasterize_heights(9., 9, &entries, 1000, || {
                calls.set(calls.get() + 1);
                calls.get() > 5
            })
            .unwrap()
            .is_none()
        );
        assert!(rasterize_heights(0., 9, &entries, 1000, || false).is_err());
    }

    #[test]
    fn raster_preserves_holes_handles_stacks_and_clips_outside_geometry() {
        let v = [[-2., 2., -2.], [-2., 2., 2.], [2., 2., -2.]];
        let t = [[0, 1, 2]];
        let high = v.map(|[x, _, z]| [x, 8., z]);
        let outside = v.map(|[x, y, z]| [x + 100., y, z]);
        let lower = Surface::new(&v, &t).unwrap();
        let upper = Surface::new(&high, &t).unwrap();
        let outside = Surface::new(&outside, &t).unwrap();
        let original = vec![0; 81];
        let run = |surfaces: &[RasterSurface<'_, '_>]| {
            rasterize(
                RasterGrid {
                    size: 9.,
                    resolution: 9,
                    existing: &original,
                },
                surfaces,
                1000,
                || false,
            )
            .unwrap()
            .unwrap()
        };
        let a = run(&[
            RasterSurface {
                surface: &lower,
                two_sided: false,
            },
            RasterSurface {
                surface: &upper,
                two_sided: false,
            },
            RasterSurface {
                surface: &outside,
                two_sided: false,
            },
        ]);
        let b = run(&[
            RasterSurface {
                surface: &outside,
                two_sided: false,
            },
            RasterSurface {
                surface: &upper,
                two_sided: false,
            },
            RasterSurface {
                surface: &lower,
                two_sided: false,
            },
        ]);
        assert_eq!(a.water, b.water);
        assert_eq!(a.water[3 * 9 + 3], encode(8.));
        assert_eq!(a.water[5 * 9 + 5], 0); // Missing half of the quad.
        assert_eq!(a.water[0], 0);
        let mut existing = a.water.clone();
        existing[3 * 9 + 3] = encode(20.);
        let raised = rasterize(
            RasterGrid {
                size: 9.,
                resolution: 9,
                existing: &existing,
            },
            &[RasterSurface {
                surface: &lower,
                two_sided: false,
            }],
            1000,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(raised.water, existing);
        assert_eq!(raised.report.changed_samples, 0);
    }

    #[test]
    fn raster_face_policy_verticals_and_partial_map_overlap_are_geometric() {
        // Both surfaces cross the map boundary. The upper surface faces down.
        let v = [
            [-10., -2., -10.],
            [-10., -2., 10.],
            [10., -2., -10.],
            [-10., 5., -10.],
            [10., 5., -10.],
            [-10., 5., 10.],
            [0., -4., 0.],
            [0., 9., 0.],
            [0., -4., 2.],
        ];
        let t = [[0, 1, 2], [3, 4, 5], [6, 7, 8], [0, 0, 0]];
        let mesh = Surface::new(&v, &t).unwrap();
        let existing = vec![0; 81];
        for (two_sided, y) in [(false, -2.), (true, 5.)] {
            let result = rasterize(
                RasterGrid {
                    size: 9.,
                    resolution: 9,
                    existing: &existing,
                },
                &[RasterSurface {
                    surface: &mesh,
                    two_sided,
                }],
                1000,
                || false,
            )
            .unwrap()
            .unwrap();
            assert_eq!(result.water[0], encode(y));
            assert_eq!(result.water[4 * 9 + 4], encode(y));
            assert_eq!(result.water[8 * 9 + 8], 0);
        }
    }

    #[test]
    fn raster_budget_cancellation_invalid_values_and_cell_centres_are_explicit() {
        let v = [[-10., 2., -10.], [-10., 2., 10.], [10., 2., -10.]];
        let t = [[0, 1, 2]];
        let mesh = Surface::new(&v, &t).unwrap();
        let surfaces = [RasterSurface {
            surface: &mesh,
            two_sided: false,
        }];
        let existing = vec![0; 81];
        let grid = || RasterGrid {
            size: 9.,
            resolution: 9,
            existing: &existing,
        };
        assert!(rasterize(grid(), &surfaces, 90, || false).is_err());
        let calls = std::cell::Cell::new(0);
        assert!(
            rasterize(grid(), &surfaces, 1000, || {
                calls.set(calls.get() + 1);
                calls.get() > 8
            })
            .unwrap()
            .is_none()
        );
        assert!(existing.iter().all(|&h| h == 0));
        assert!(
            rasterize(
                RasterGrid {
                    size: f64::NAN,
                    ..grid()
                },
                &[],
                1000,
                || false
            )
            .is_err()
        );
        let invalid = vec![-1; 81];
        assert!(
            rasterize(
                RasterGrid {
                    existing: &invalid,
                    ..grid()
                },
                &[],
                1000,
                || false
            )
            .is_err()
        );
        let out_of_range = v.map(|[x, _, z]| [x, 501., z]);
        let mesh = Surface::new(&out_of_range, &t).unwrap();
        assert!(
            rasterize(
                grid(),
                &[RasterSurface {
                    surface: &mesh,
                    two_sided: false
                }],
                1000,
                || false
            )
            .is_err()
        );
        // A patch at the terrain endpoint does not cover the first cell centre.
        let edge = [[-4.5, 1., -4.5], [-4.5, 1., -4.3], [-4.3, 1., -4.5]];
        let mesh = Surface::new(&edge, &t).unwrap();
        let result = rasterize(
            grid(),
            &[RasterSurface {
                surface: &mesh,
                two_sided: false,
            }],
            1000,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.water, existing);
        assert_eq!(result.report.changed_samples, 0);
    }

    #[test]
    fn arbitrary_yaw_tilt_scale_and_translation_preserve_surface_queries() {
        for degrees in [0_f64, 13., 47.5, 90., 143., 180., 270., 359.] {
            let (s, c) = degrees.to_radians().sin_cos();
            let vertices: Vec<_> = [[-2., 2., -1.], [-2., 2., 1.], [2., 2., -1.], [2., 2., 1.]]
                .into_iter()
                .map(|[x, y, z]| {
                    // Nonuniform scale and a sloping top face, followed by arbitrary yaw.
                    let y = y + 0.3 * x + 0.2 * z;
                    [10. + x * c - z * s, y + 7., -4. + x * s + z * c]
                })
                .collect();
            let triangles = [[0, 1, 2], [2, 1, 3]];
            let surface = Surface::new(&vertices, &triangles).unwrap();
            let mut work = 0;
            assert!(
                (surface
                    .highest(10., -4., false, &mut work, 10, || false)
                    .unwrap()
                    .unwrap()
                    - 9.)
                    .abs()
                    < 1e-8
            );
            let (x, z) = (10. + 0.5 * c - 0.25 * s, -4. + 0.5 * s + 0.25 * c);
            assert!(
                (surface
                    .highest(x, z, false, &mut work, 10, || false)
                    .unwrap()
                    .unwrap()
                    - 9.2)
                    .abs()
                    < 1e-8
            );
            assert_eq!(
                surface
                    .highest(100., 100., true, &mut work, 10, || false)
                    .unwrap(),
                None
            );
        }
    }
    #[test]
    fn highest_layer_backfaces_edges_verticals_and_holes_are_explicit() {
        let v = [
            [0., 2., 0.],
            [0., 2., 2.],
            [2., 2., 0.],
            [0., 5., 0.],
            [2., 5., 0.],
            [0., 5., 2.],
            [0., 0., 0.],
            [0., 10., 0.],
            [0., 0., 2.],
        ];
        let t = [[0, 1, 2], [3, 4, 5], [6, 7, 8], [0, 0, 0]];
        let mesh = Surface::new(&v, &t).unwrap();
        assert_eq!(
            mesh.highest(0.5, 0.5, false, &mut 0, 20, || false).unwrap(),
            Some(2.)
        );
        assert_eq!(
            mesh.highest(0.5, 0.5, true, &mut 0, 20, || false).unwrap(),
            Some(5.)
        );
        assert_eq!(
            mesh.highest(1.9, 1.9, true, &mut 0, 20, || false).unwrap(),
            None
        );
        assert_eq!(
            mesh.highest(0., 0., true, &mut 0, 20, || false).unwrap(),
            Some(5.)
        );
    }
    #[test]
    fn malformed_cancelled_and_budget_exhausted_queries_cannot_return_partial_heights() {
        let v = [[0., 2., 0.], [0., 2., 2.], [2., 2., 0.]];
        let t = [[0, 1, 2]];
        assert!(Surface::new(&v, &[[0, 1, 3]]).is_err());
        assert!(Surface::new(&[[f64::NAN; 3]], &[[0, 0, 0]]).is_err());
        let mesh = Surface::new(&v, &t).unwrap();
        assert!(mesh.highest(0.5, 0.5, true, &mut 0, 1, || true).is_err());
        let mut work = 0;
        assert_eq!(
            mesh.highest(0.5, 0.5, true, &mut work, 1, || false)
                .unwrap(),
            Some(2.)
        );
        assert!(
            mesh.highest(0.5, 0.5, true, &mut work, 1, || false)
                .is_err()
        );
        assert!(
            mesh.highest(f64::NAN, 0., true, &mut 0, 1, || false)
                .is_err()
        );
    }
}
