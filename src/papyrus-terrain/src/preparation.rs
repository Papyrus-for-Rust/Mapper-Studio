//! Read-only earthwork assessment. This is not an asset stamp or an editable-world commit.
use crate::erosion::decode;
pub const VERSION: &str = "papyrus-preparation-assessment-1";
#[derive(Clone, Copy, Debug)]
pub struct Footprint {
    pub x: f32,
    pub z: f32,
    pub yaw: f32,
    pub extent_x: f32,
    pub extent_z: f32,
    pub fade: f32,
    pub maximum_change: f32,
}
#[derive(Debug, PartialEq)]
pub struct Assessment {
    pub plane_height: f32,
    pub minimum_height: f32,
    pub maximum_height: f32,
    pub maximum_cut: f32,
    pub maximum_fill: f32,
    pub cut_volume: f64,
    pub fill_volume: f64,
    pub sampled_cells: usize,
    pub low_core_samples: usize,
    pub submerged_blend_samples: usize,
    pub within_budget: bool,
}
pub fn assess(
    height: &[i16],
    resolution: usize,
    size: usize,
    footprint: Footprint,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Assessment>, String> {
    if !(33..=4097).contains(&resolution)
        || !(resolution - 1).is_power_of_two()
        || height.len() != resolution * resolution
        || !(1000..=6000).contains(&size)
        || [
            footprint.x,
            footprint.z,
            footprint.yaw,
            footprint.extent_x,
            footprint.extent_z,
            footprint.fade,
            footprint.maximum_change,
        ]
        .iter()
        .any(|n| !n.is_finite())
        || !(0.1..=1000.).contains(&footprint.extent_x)
        || !(0.1..=1000.).contains(&footprint.extent_z)
        || !(1. ..=128.).contains(&footprint.fade)
        || !(0.1..=100.).contains(&footprint.maximum_change)
    {
        return Err("Invalid terrain-preparation grid or footprint".into());
    }
    if cancelled() {
        return Ok(None);
    }
    let (s, c) = footprint.yaw.rem_euclid(360.).to_radians().sin_cos();
    let half = size as f32 / 2.;
    let step = size as f32 / (resolution - 1) as f32;
    let bx = c.abs() * (footprint.extent_x + footprint.fade)
        + s.abs() * (footprint.extent_z + footprint.fade);
    let bz = s.abs() * (footprint.extent_x + footprint.fade)
        + c.abs() * (footprint.extent_z + footprint.fade);
    if footprint.x.abs() + bx > half || footprint.z.abs() + bz > half {
        return Err("Preparation footprint and fade reach outside world bounds".into());
    }
    let index = |world: f32| ((world + half) / step).clamp(0., (resolution - 1) as f32);
    let gx = index(footprint.x);
    let gz = index(footprint.z);
    let ix = gx.floor() as usize;
    let iz = gz.floor() as usize;
    let sample = |x: usize, z: usize| -> Result<f32, String> {
        let value = height[z * resolution + x];
        if !(0..=32766).contains(&value) {
            Err("Invalid preparation source height".into())
        } else {
            Ok(decode(value))
        }
    };
    let low = sample(ix, iz)? + (sample(ix + 1, iz)? - sample(ix, iz)?) * gx.fract();
    let high = sample(ix, iz + 1)? + (sample(ix + 1, iz + 1)? - sample(ix, iz + 1)?) * gx.fract();
    let plane = low + (high - low) * gz.fract();
    let x0 = index(footprint.x - bx).floor() as usize;
    let x1 = index(footprint.x + bx).ceil() as usize;
    let z0 = index(footprint.z - bz).floor() as usize;
    let z1 = index(footprint.z + bz).ceil() as usize;
    if (x1 - x0 + 1) * (z1 - z0 + 1) > 1_048_576 {
        return Err("Preparation assessment exceeds one million samples".into());
    }
    let mut result = Assessment {
        plane_height: plane,
        minimum_height: f32::INFINITY,
        maximum_height: f32::NEG_INFINITY,
        maximum_cut: 0.,
        maximum_fill: 0.,
        cut_volume: 0.,
        fill_volume: 0.,
        sampled_cells: 0,
        low_core_samples: 0,
        submerged_blend_samples: 0,
        within_budget: false,
    };
    let area = f64::from(step).powi(2);
    for z in z0..=z1 {
        if cancelled() {
            return Ok(None);
        }
        for x in x0..=x1 {
            let dx = x as f32 * step - half - footprint.x;
            let dz = z as f32 * step - half - footprint.z;
            let lx = c * dx + s * dz;
            let lz = -s * dx + c * dz;
            let outside = (lx.abs() - footprint.extent_x)
                .max(0.)
                .max((lz.abs() - footprint.extent_z).max(0.));
            if outside > footprint.fade {
                continue;
            }
            let original = sample(x, z)?;
            if outside == 0. {
                result.low_core_samples += usize::from(original <= 4.);
            } else {
                result.submerged_blend_samples += usize::from(original <= 0.);
            }
            let t = (1. - outside / footprint.fade).clamp(0., 1.);
            let weight = t * t * (3. - 2. * t);
            let delta = (plane - original) * weight;
            result.minimum_height = result.minimum_height.min(original);
            result.maximum_height = result.maximum_height.max(original);
            result.maximum_cut = result.maximum_cut.max(-delta);
            result.maximum_fill = result.maximum_fill.max(delta);
            if delta < 0. {
                result.cut_volume += f64::from(-delta) * area;
            } else {
                result.fill_volume += f64::from(delta) * area;
            }
            result.sampled_cells += 1;
        }
    }
    if result.sampled_cells == 0 {
        return Err("Preparation footprint is too small for this grid".into());
    }
    result.within_budget = result.low_core_samples == 0
        && result.submerged_blend_samples == 0
        && result.maximum_cut <= footprint.maximum_change
        && result.maximum_fill <= footprint.maximum_change;
    Ok(Some(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::erosion::encode;
    use std::cell::Cell;
    fn pad() -> Footprint {
        Footprint {
            x: 0.,
            z: 0.,
            yaw: 0.,
            extent_x: 50.,
            extent_z: 35.,
            fade: 25.,
            maximum_change: 20.,
        }
    }
    #[test]
    fn flat_and_sloping_terrain_produce_repeatable_nonmutating_earthwork_estimates() {
        let n = 129;
        let flat = vec![encode(20.); n * n];
        let result = assess(&flat, n, 1000, pad(), || false).unwrap().unwrap();
        assert!(result.within_budget);
        assert_eq!(result.maximum_cut, 0.);
        assert_eq!(result.maximum_fill, 0.);
        assert_eq!(result.cut_volume, 0.);
        let heights: Vec<_> = (0..n * n)
            .map(|i| encode(20. + ((i % n) as f32 / (n - 1) as f32 * 1000. - 500.) * 0.1))
            .collect();
        let original = heights.clone();
        let slope = assess(&heights, n, 1000, pad(), || false).unwrap().unwrap();
        assert!(slope.within_budget);
        assert!(slope.maximum_cut > 4.);
        assert!(slope.maximum_fill > 4.);
        assert!(slope.cut_volume > 0.);
        assert!(slope.fill_volume > 0.);
        assert_eq!(heights, original);
        assert_eq!(
            slope,
            assess(&heights, n, 1000, pad(), || false).unwrap().unwrap()
        );
        let mut limited = pad();
        limited.maximum_change = 1.;
        assert!(
            !assess(&heights, n, 1000, limited, || false)
                .unwrap()
                .unwrap()
                .within_budget
        );
    }
    #[test]
    fn coastline_bounds_rotation_and_cancellation_are_explicit() {
        let n = 129;
        let heights = vec![encode(-1.); n * n];
        let wet = assess(&heights, n, 1000, pad(), || false).unwrap().unwrap();
        assert!(!wet.within_budget);
        assert!(wet.low_core_samples > 0);
        assert!(wet.submerged_blend_samples > 0);
        let mut edge = pad();
        edge.x = 450.;
        assert!(assess(&heights, n, 1000, edge, || false).is_err());
        let calls = Cell::new(0);
        assert!(
            assess(&heights, n, 1000, pad(), || {
                calls.set(calls.get() + 1);
                calls.get() > 3
            })
            .unwrap()
            .is_none()
        );
        let mut rotated = pad();
        rotated.yaw = 90.;
        let flat = vec![encode(20.); n * n];
        assert!(
            assess(&flat, n, 1000, rotated, || false)
                .unwrap()
                .unwrap()
                .within_budget
        );
        rotated.yaw = f32::NAN;
        assert!(assess(&flat, n, 1000, rotated, || false).is_err());
        assert!(assess(&[], usize::MAX, 1000, pad(), || false).is_err());
    }
}
