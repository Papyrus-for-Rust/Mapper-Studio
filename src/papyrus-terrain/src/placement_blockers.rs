//! Authored TerrainPlacementBlocked circles, independent of topology/occupancy.
use crate::{decor_fit, native_layers::dimensions};
use serde::Serialize;
pub const VERSION: &str = "papyrus-placement-blockers-1";
#[derive(Clone, Debug)]
pub struct Modifier {
    pub position: [f32; 3],
    pub radius: f32,
    pub fade: f32,
}
pub struct Instance<'a> {
    pub position: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
    pub modifiers: &'a [Modifier],
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub instances: usize,
    pub modifiers: usize,
    pub clipped_modifiers: usize,
    pub sample_visits: usize,
    pub blocked_cells: usize,
}
pub struct Raster {
    pub cells: Vec<u8>,
    pub report: Report,
}
pub fn generate(
    size: u32,
    resolution: usize,
    instances: &[Instance<'_>],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Raster>, String> {
    let (_, n) = dimensions(size)?;
    if resolution != n
        || instances.len() > 128
        || !(1..=128_000_000).contains(&maximum_work)
        || instances.iter().map(|i| i.modifiers.len()).sum::<usize>() > 65536
    {
        return Err("Invalid placement blocker bounds".into());
    }
    let size = size as f32;
    let mut cells = vec![0; n * n];
    let mut report = Report {
        engine: VERSION,
        instances: instances.len(),
        modifiers: 0,
        clipped_modifiers: 0,
        sample_visits: 0,
        blocked_cells: 0,
    };
    let index = |w: f32| ((w / size + 0.5) * n as f32).clamp(0., (n - 1) as f32) as usize;
    for instance in instances {
        if cancelled() {
            return Ok(None);
        }
        if instance
            .position
            .iter()
            .chain(&instance.rotation)
            .chain(&instance.scale)
            .any(|v| !v.is_finite())
            || instance.position.iter().any(|v| v.abs() > 10000.)
            || instance.scale.iter().any(|v| *v <= 0. || *v > 100.)
            || (instance.rotation.iter().map(|v| v * v).sum::<f32>() - 1.).abs() > 0.001
        {
            return Err("Invalid placement blocker pose".into());
        }
        for m in instance.modifiers {
            if cancelled() {
                return Ok(None);
            }
            if m.position
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 10000.)
                || !m.radius.is_finite()
                || !(0. ..=2048.).contains(&m.radius)
                || !m.fade.is_finite()
                || !(0. ..=2048.).contains(&m.fade)
            {
                return Err("Invalid placement blocker modifier".into());
            }
            let local = decor_fit::rotate(
                instance.rotation,
                std::array::from_fn(|i| m.position[i] * instance.scale[i]),
            );
            let center: [f32; 3] = std::array::from_fn(|i| instance.position[i] + local[i]);
            let radius = m.radius * instance.scale[1];
            let fade = m.fade * instance.scale[1];
            if center.iter().any(|v| !v.is_finite()) || !radius.is_finite() || !fade.is_finite() {
                return Err("Invalid transformed blocker".into());
            }
            report.modifiers += 1;
            report.clipped_modifiers += usize::from(
                center[0].abs() + radius > size * 0.5 || center[2].abs() + radius > size * 0.5,
            );
            let cx = (center[0] / size + 0.5) * n as f32;
            let cz = (center[2] / size + 0.5) * n as f32;
            let r = radius / size * n as f32;
            let f = fade / size * n as f32;
            for z in index(center[2] - radius)..=index(center[2] + radius) {
                if cancelled() {
                    return Ok(None);
                }
                for x in index(center[0] - radius)..=index(center[0] + radius) {
                    if report.sample_visits == maximum_work {
                        return Err("Placement blocker work allowance exhausted".into());
                    }
                    report.sample_visits += 1;
                    let distance = (x as f32 + 0.5 - cx).hypot(z as f32 + 0.5 - cz);
                    let applies = if f == 0. {
                        distance < r
                    } else {
                        ((r - distance) / f).clamp(0., 1.) > 0.5
                    };
                    if applies {
                        let cell = &mut cells[z * n + x];
                        if *cell == 0 {
                            *cell = 1;
                            report.blocked_cells += 1;
                        }
                    }
                }
            }
        }
    }
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(Raster { cells, report }))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_pose_scaled_fade_clipping_and_overlap_match_cell_center_predicates() {
        let size = 1000;
        let (_, n) = dimensions(size).unwrap();
        let a = 0.73_f32;
        let modifiers = [
            Modifier {
                position: [20., 0., -10.],
                radius: 35.,
                fade: 12.,
            },
            Modifier {
                position: [-500., 0., 0.],
                radius: 20.,
                fade: 0.,
            },
        ];
        let pose = Instance {
            position: [7., 2., -3.],
            rotation: [0., (a * 0.5).sin(), 0., (a * 0.5).cos()],
            scale: [1.2, 2., 0.7],
            modifiers: &modifiers,
        };
        let got = generate(size, n, &[pose], 1_000_000, || false)
            .unwrap()
            .unwrap();
        let mut expected = vec![0; n * n];
        for m in &modifiers {
            let local = decor_fit::rotate(
                [0., (a * 0.5).sin(), 0., (a * 0.5).cos()],
                [m.position[0] * 1.2, 0., m.position[2] * 0.7],
            );
            let center = [7. + local[0], -3. + local[2]];
            for z in 0..n {
                for x in 0..n {
                    let wx = ((x as f32 + 0.5) / n as f32 - 0.5) * size as f32;
                    let wz = ((z as f32 + 0.5) / n as f32 - 0.5) * size as f32;
                    let d = (wx - center[0]).hypot(wz - center[1]);
                    if if m.fade == 0. {
                        d < m.radius * 2.
                    } else {
                        ((m.radius * 2. - d) / (m.fade * 2.)).clamp(0., 1.) > 0.5
                    } {
                        expected[z * n + x] = 1;
                    }
                }
            }
        }
        assert_eq!(got.cells, expected);
        assert_eq!(
            got.report.blocked_cells,
            expected.iter().filter(|v| **v == 1).count()
        );
        let clipped = [Modifier {
            position: [500., 0., 0.],
            radius: 40.,
            fade: 0.,
        }];
        let result = generate(
            size,
            n,
            &[Instance {
                position: [0.; 3],
                rotation: [0., 0., 0., 1.],
                scale: [1.; 3],
                modifiers: &clipped,
            }],
            100000,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.report.clipped_modifiers, 1);
        assert!(result.report.blocked_cells > 0);
    }
    #[test]
    fn cancellation_budget_and_invalid_data_do_not_return_partial_rasters() {
        let (_, n) = dimensions(1000).unwrap();
        let m = [Modifier {
            position: [0.; 3],
            radius: 30.,
            fade: 0.,
        }];
        let instances = [Instance {
            position: [0.; 3],
            rotation: [0., 0., 0., 1.],
            scale: [1.; 3],
            modifiers: &m,
        }];
        assert!(generate(1000, n, &instances, 1, || false).is_err());
        let count = std::cell::Cell::new(0);
        assert!(
            generate(1000, n, &instances, 100000, || {
                count.set(count.get() + 1);
                count.get() == 4
            })
            .unwrap()
            .is_none()
        );
        assert!(generate(1000, n - 1, &instances, 100000, || false).is_err());
        assert!(
            generate(
                1000,
                n,
                &[Instance {
                    position: [0.; 3],
                    rotation: [0.; 4],
                    scale: [1.; 3],
                    modifiers: &m
                }],
                100000,
                || false
            )
            .is_err()
        );
    }
}
