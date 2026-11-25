//! Native-frame height normals with inspected diagonal-corner sampling.
//! Spherical interpolation uses Rust math, not Unity's native Slerp entry point.
pub const VERSION: &str = "papyrus-native-terrain-normal-1";

/// TerrainHeightMap.GetSlope uses the angle between the sampled normal and up.
pub fn slope(size: u32, n: usize, height: &[i16], uv: [f32; 2]) -> Result<f32, String> {
    let normal = sample(size, n, height, uv)?;
    Ok(slope_degrees(normal))
}

pub(crate) fn slope_degrees(normal: [f32; 3]) -> f32 {
    let length = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
    (normal[1] / length).clamp(-1., 1.).acos() * 57.29578
}

/// At an exact vertex, each axis of spherical interpolation normalizes the
/// first operand. The other corners can only affect signs of zero, which the
/// slope calculation squares. The caller validates the full height raster.
pub(crate) fn slope_at_vertex(
    n: usize,
    norm_y: f32,
    height: &[i16],
    x: usize,
    z: usize,
) -> Result<f32, String> {
    normal_at_vertex(n, norm_y, x, z, |x, z| Some(height[z * n + x])).map(slope_degrees)
}

/// Vertex normal for slope calculations on a borrowed or projected height grid.
pub(crate) fn normal_at_vertex(
    n: usize,
    norm_y: f32,
    x: usize,
    z: usize,
    read: impl FnMut(usize, usize) -> Option<i16>,
) -> Result<[f32; 3], String> {
    let normal = normal_at(n, norm_y, x, z, read)?;
    let unit = crate::rail_costmap::unit;
    Ok(unit(unit(normal)))
}

/// Sample the fixed native -500..500 m transport frame. Only sampled height
/// values are inspected, so a coastal proposal does bounded work on large maps.
pub fn sample(size: u32, n: usize, height: &[i16], uv: [f32; 2]) -> Result<[f32; 3], String> {
    if !(1000..=6000).contains(&size)
        || !(2..=4097).contains(&n)
        || n.checked_mul(n) != Some(height.len())
        || uv.iter().any(|v| !v.is_finite() || !(0. ..=1.).contains(v))
    {
        return Err("Invalid native terrain normal input".into());
    }
    sample_with(size, n, uv, |x, z| Some(height[z * n + x]))
}
/// The same bounded normal query over a private/lazy height revision.
pub fn sample_with(
    size: u32,
    n: usize,
    uv: [f32; 2],
    mut read: impl FnMut(usize, usize) -> Option<i16>,
) -> Result<[f32; 3], String> {
    if !(1000..=6000).contains(&size)
        || !(2..=4097).contains(&n)
        || uv.iter().any(|v| !v.is_finite() || !(0. ..=1.).contains(v))
    {
        return Err("Invalid native terrain normal input".into());
    }
    let [x, z] = uv.map(|v| v * (n - 1) as f32);
    let (a, b) = (x as usize, z as usize);
    let (c, d) = ((a + 1).min(n - 1), (b + 1).min(n - 1));
    let norm_y = size as f32 / 1000. / n as f32;
    let mut normal = |x, z| normal_at(n, norm_y, x, z, &mut read);
    let low = crate::rail_costmap::slerp(normal(a, b)?, normal(c, b)?, x.fract());
    let high = crate::rail_costmap::slerp(normal(a, d)?, normal(c, d)?, x.fract());
    Ok(crate::rail_costmap::slerp(low, high, z.fract()))
}

/// Integer-grid primitive shared with the late erosion slope pass. Callers
/// validate dimensions and coordinates before entering their sampling loop.
pub(crate) fn normal_at(
    n: usize,
    norm_y: f32,
    x: usize,
    z: usize,
    mut read: impl FnMut(usize, usize) -> Option<i16>,
) -> Result<[f32; 3], String> {
    let (l, r, f, k) = (
        x.saturating_sub(1),
        (x + 1).min(n - 1),
        z.saturating_sub(1),
        (z + 1).min(n - 1),
    );
    let mut at = |x, z| -> Result<i16, String> {
        let h = read(x, z).ok_or("Missing native height sample")?;
        if !(0..=32766).contains(&h) {
            return Err("Invalid sampled native height encoding".into());
        }
        Ok(h)
    };
    Ok(normal_from_heights(norm_y, at(l, f)?, at(r, f)?, at(l, k)?))
}

/// Diagonal samples from a height raster whose encoding has already been checked.
#[inline]
pub(crate) fn normal_from_heights(norm_y: f32, base: i16, right: i16, lower: i16) -> [f32; 3] {
    let [base, right, lower] = [base, right, lower].map(|height| height as f32 * 3.051944E-5);
    let v = [-(right - base) * 0.5, norm_y, -(lower - base) * 0.5];
    let length = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    v.map(|v| v / length)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vertex_slope_matches_interpolated_slope_bit_for_bit() {
        let n = 129;
        let mut height: Vec<i16> = (0..n * n).map(|i| ((i * 17_839) % 32_767) as i16).collect();
        height[0..3 * n].fill(16_000);
        height[5 * n..6 * n].fill(15_000);
        for size in [1000, 3072, 4000, 6000] {
            let norm_y = size as f32 / 1000. / n as f32;
            for z in 0..n {
                for x in 0..n {
                    let uv = [x, z].map(|coord| coord as f32 / (n - 1) as f32);
                    let full = slope(size, n, &height, uv).unwrap();
                    let vertex = slope_at_vertex(n, norm_y, &height, x, z).unwrap();
                    assert_eq!(full.to_bits(), vertex.to_bits(), "{size} at {x}, {z}");
                }
            }
        }
    }

    #[test]
    fn diagonal_corners_differ_from_central_derivatives_and_keep_edge_clamping() {
        let mut h = vec![16000; 5 * 5];
        h[8] = 18000;
        h[3 * 5 + 1] = 17000;
        // The central row/column is flat. The inspected source samples the
        // diagonal corners, producing a nonzero slope in both axes.
        let n = sample(1000, 5, &h, [0.5, 0.5]).unwrap();
        let dx: f32 = -(18000. * 3.051944E-5 - 16000. * 3.051944E-5) * 0.5;
        let dz: f32 = -(17000. * 3.051944E-5 - 16000. * 3.051944E-5) * 0.5;
        let length = (dx * dx + 0.2 * 0.2 + dz * dz).sqrt();
        assert!((n[0] - dx / length).abs() < 1e-6);
        assert!((n[2] - dz / length).abs() < 1e-6);
        for uv in [[0., 0.], [1., 1.], [0., 1.], [1., 0.], [0.37, 0.61]] {
            let v = sample(1000, 5, &h, uv).unwrap();
            assert!(v.iter().all(|v| v.is_finite()));
            assert!((v.iter().map(|v| v * v).sum::<f32>() - 1.).abs() < 1e-5);
            assert!(v[1] > 0.);
        }
        assert_eq!(
            sample(1000, 5, &[16000; 25], [0.31, 0.72]).unwrap(),
            [0., 1., 0.]
        );
    }
    #[test]
    fn integer_grid_normals_match_calls_to_the_owned_server_assembly() {
        #[derive(serde::Deserialize)]
        struct Case {
            size: u32,
            resolution: usize,
            heights: Vec<i16>,
            x: usize,
            z: usize,
            normal: [f32; 3],
        }
        #[derive(serde::Deserialize)]
        struct Fixture {
            assembly_sha256: String,
            cases: Vec<Case>,
        }
        let f: Fixture = serde_json::from_str(include_str!(
            "../tests/fixtures/terrain-normal-bd19c4ac.json"
        ))
        .unwrap();
        assert_eq!(
            f.assembly_sha256,
            "bd19c4ac705edd18ff556f5dbb3130e098b380577cc2ab0d73a2ec89ee289cc9"
        );
        assert_eq!(f.cases.len(), 54);
        for c in f.cases {
            let actual = sample(
                c.size,
                c.resolution,
                &c.heights,
                [
                    c.x as f32 / (c.resolution - 1) as f32,
                    c.z as f32 / (c.resolution - 1) as f32,
                ],
            )
            .unwrap();
            assert!(
                actual
                    .iter()
                    .zip(c.normal)
                    .all(|(a, b)| (*a - b).abs() < 2e-6),
                "{actual:?} != {:?}",
                c.normal
            );
        }
    }
    #[test]
    fn invalid_coordinates_dimensions_and_sampled_encodings_are_rejected() {
        let mut h = vec![16000; 25];
        for uv in [[f32::NAN, 0.5], [-0.1, 0.5], [0.5, 1.1]] {
            assert!(sample(1000, 5, &h, uv).is_err());
        }
        assert!(sample(0, 5, &h, [0., 0.]).is_err());
        assert!(sample(1000, 6, &h, [0., 0.]).is_err());
        h[0] = -1;
        assert!(sample(1000, 5, &h, [0., 0.]).is_err());
    }
}
