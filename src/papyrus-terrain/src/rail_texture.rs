//! Source-style late rail material sweep. No height/topology mutation.
use crate::{path_geometry::Path, path_surface};
use serde::{Deserialize, Serialize};
pub const VERSION: &str = "papyrus-rail-path-texture-1";
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub width: f32,
    pub padding: [f32; 2],
    pub sample_spacing: f32,
    pub random_scale: f32,
    pub splat_channel: usize,
}
impl Policy {
    pub fn validate(self) -> Result<(), String> {
        if !self.width.is_finite()
            || !(0.1..=256.).contains(&self.width)
            || self
                .padding
                .iter()
                .any(|v| !v.is_finite() || !(0. ..=256.).contains(v))
            || self.sample_spacing != 1.
            || self.random_scale != 1.
            || self.splat_channel >= 8
        {
            return Err("Unsupported rail texture shape, spacing or radius policy".into());
        }
        Ok(())
    }
}
#[derive(Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub policy: Policy,
    pub tracks: usize,
    pub controls: usize,
    pub sweep: path_surface::Report,
    pub changed_cells: usize,
    pub work: usize,
    pub runtime_validated: bool,
}
pub struct Output {
    pub splat: Vec<u8>,
    pub report: Report,
}
pub fn paint(
    grid: path_surface::Grid,
    splat: &[u8],
    tracks: &[&Path],
    policy: Policy,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    if cancelled() {
        return Ok(None);
    }
    policy.validate()?;
    let n = grid.resolution;
    let controls = tracks.iter().map(|p| p.points().len()).sum::<usize>();
    if !(2..=2048).contains(&n)
        || splat.len() != n * n * 8
        || tracks.len() > 4096
        || controls > 1_048_576
        || !(1..=512_000_000).contains(&maximum_work)
    {
        return Err("Invalid rail texture rasters, paths or work grant".into());
    }
    let cells = n * n;
    let overhead = controls
        .checked_add(cells * 3)
        .filter(|&v| v < maximum_work)
        .ok_or("Rail texture work exhausted")?;
    let paths = tracks
        .iter()
        .map(|path| path_surface::PathSpec {
            path,
            width: policy.width,
            padding: policy.padding,
        })
        .collect::<Vec<_>>();
    let mut output = splat.to_vec();
    let Some(sweep) = path_surface::walk(
        &paths,
        grid,
        policy.sample_spacing,
        maximum_work - overhead,
        |_, _, _, _| Ok(policy.width * 0.5),
        &cancelled,
        |_, i, opacity| {
            path_surface::set_splat(&mut output, cells, i, policy.splat_channel, opacity)
        },
    )?
    else {
        return Ok(None);
    };
    let mut changed = 0;
    for start in (0..cells).step_by(4096) {
        if cancelled() {
            return Ok(None);
        }
        changed += (start..(start + 4096).min(cells))
            .filter(|&i| (0..8).any(|c| output[c * cells + i] != splat[c * cells + i]))
            .count();
    }
    let work = overhead + sweep.work;
    Ok(Some(Output {
        splat: output,
        report: Report {
            engine: VERSION,
            policy,
            tracks: tracks.len(),
            controls,
            sweep,
            changed_cells: changed,
            work,
            runtime_validated: false,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reverse_path_order_keeps_absolute_source_weights_and_cancellation_private() {
        let grid = path_surface::Grid {
            size: 128.,
            resolution: 128,
            origin_y: -500.,
            vertical_span: 1000.,
        };
        let policy = Policy {
            width: 4.,
            padding: [1., 1.],
            sample_spacing: 1.,
            random_scale: 1.,
            splat_channel: 7,
        };
        let make = |z| {
            Path::from_controls(
                &[[-40., 20., z], [0., 20., z], [40., 20., z]],
                10000,
                || false,
            )
            .unwrap()
            .unwrap()
        };
        let a = make(-1.5);
        let b = make(3.);
        let cells = 128 * 128;
        let mut splat = vec![0; cells * 8];
        splat[..cells].fill(255);
        let forward = paint(grid, &splat, &[&a, &b], policy, 2_000_000, || false)
            .unwrap()
            .unwrap();
        let reverse = paint(grid, &splat, &[&b, &a], policy, 2_000_000, || false)
            .unwrap()
            .unwrap();
        let cell = 64 * 128 + 64;
        assert_eq!(forward.splat[7 * cells + cell], 128);
        assert_eq!(reverse.splat[7 * cells + cell], 64);
        // Adjacent quads can revisit a cell. Repeated absolute writes rescale
        // already quantized bytes, so their sum need not remain exactly 255.
        assert!(
            (0..8)
                .map(|c| u32::from(forward.splat[c * cells + cell]))
                .sum::<u32>()
                > 255
        );
        let calls = std::cell::Cell::new(0);
        assert!(
            paint(grid, &splat, &[&a, &b], policy, 2_000_000, || {
                calls.set(calls.get() + 1);
                calls.get() == 30
            })
            .unwrap()
            .is_none()
        );
        assert_eq!(splat[cell], 255);
        assert!(
            paint(
                grid,
                &splat,
                &[&a, &b],
                policy,
                forward.report.work - 1,
                || false
            )
            .is_err()
        );
        let empty = paint(grid, &splat, &[], policy, 2_000_000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(empty.splat, splat);
        assert_eq!(empty.report.changed_cells, 0);
    }
    #[test]
    fn late_rail_texture_restores_gravel_after_dirt_and_retains_controls_and_inputs() {
        let grid = path_surface::Grid {
            size: 128.,
            resolution: 128,
            origin_y: -500.,
            vertical_span: 1000.,
        };
        let policy = Policy {
            width: 4.,
            padding: [1., 1.],
            sample_spacing: 1.,
            random_scale: 1.,
            splat_channel: 7,
        };
        let path = Path::from_controls(
            &[[-40., 20., 0.], [0., 20., 0.], [40., 20., 0.]],
            10000,
            || false,
        )
        .unwrap()
        .unwrap();
        let geometry = serde_json::to_vec(&path).unwrap();
        let cells = 128 * 128;
        let mut splat = vec![0; cells * 8];
        splat[..cells].fill(255);
        let original = splat.clone();
        let out = paint(grid, &splat, &[&path], policy, 2_000_000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(out.splat[7 * cells + 64 * 128 + 64], 255);
        assert_eq!(out.splat[64 * 128 + 64], 0);
        assert_eq!(out.splat[0], 255);
        assert_eq!(out.report.tracks, 1);
        assert!(out.report.changed_cells > 0 && out.report.work <= 2_000_000);
        assert_eq!(splat, original);
        assert_eq!(serde_json::to_vec(&path).unwrap(), geometry);
        assert!(paint(grid, &splat, &[&path], policy, 1, || false).is_err());
        assert!(
            paint(grid, &splat, &[&path], policy, 2_000_000, || true)
                .unwrap()
                .is_none()
        );
        assert!(
            paint(
                grid,
                &splat,
                &[&path],
                Policy {
                    random_scale: 0.75,
                    ..policy
                },
                2_000_000,
                || false
            )
            .is_err()
        );
        splat[0] = 0;
        let center = 64 * 128 + 64;
        splat[center] = 0;
        let before = splat.clone();
        let painted = paint(grid, &splat, &[&path], policy, 2_000_000, || false)
            .unwrap()
            .unwrap();
        assert!((0..8).all(|c| painted.splat[c * cells] == 0));
        assert_eq!(painted.splat[7 * cells + center], 255);
        assert_eq!(splat, before);
        let empty = paint(grid, &splat, &[], policy, 2_000_000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(empty.splat, splat);
    }
}
