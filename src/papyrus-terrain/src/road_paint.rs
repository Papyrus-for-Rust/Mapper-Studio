//! Private PathList-style road topology/material passes over retained geometry.
//! Height fitting, roadside dilation and runtime access validation are separate.
use crate::{path_geometry::Path, path_surface};
use serde::{Deserialize, Serialize};
pub const VERSION: &str = "papyrus-road-path-paint-1";
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Family {
    pub hierarchy: u8,
    pub width: f32,
    /// Inner then outer padding, from this installation's PathList construction.
    pub padding: [f32; 2],
    pub random_scale: f32,
    pub topology_mask: i32,
    pub splat_channel: Option<usize>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub sample_spacing: f32,
    pub noise_frequency: f32,
    pub noise_unsigned_scale: f32,
    pub noise_unsigned_bias: f32,
    pub topology_threshold: f32,
    pub families: Vec<Family>,
}
impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        let mut ids = std::collections::BTreeSet::new();
        if ![0.125, 0.25, 0.5, 1., 2., 4., 8.].contains(&self.sample_spacing)
            || !self.noise_frequency.is_finite()
            || !(0.000001..=1.).contains(&self.noise_frequency)
            || [self.noise_unsigned_scale, self.noise_unsigned_bias]
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 8.)
            || !self.topology_threshold.is_finite()
            || !(0. ..=1.).contains(&self.topology_threshold)
            || self.families.is_empty()
            || self.families.len() > 16
            || self.families.iter().any(|f| {
                !ids.insert(f.hierarchy)
                    || f.hierarchy > 15
                    || !f.width.is_finite()
                    || !(0.1..=256.).contains(&f.width)
                    || f.padding
                        .iter()
                        .any(|v| !v.is_finite() || !(0. ..=256.).contains(v))
                    || !f.random_scale.is_finite()
                    || !(0.01..=8.).contains(&f.random_scale)
                    || f.splat_channel.is_some_and(|c| c >= 8)
            })
        {
            return Err("Invalid road path-paint policy".into());
        }
        Ok(())
    }
}
pub struct Road<'a> {
    pub path: &'a Path,
    pub hierarchy: u8,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pass {
    Topology,
    Texture,
}
pub struct Grid<'a> {
    pub geometry: path_surface::Grid,
    pub splat: &'a [u8],
    pub topology: &'a [i32],
}
pub struct Prepared {
    paths: Vec<Path>,
    families: Vec<usize>,
    policy: Policy,
    noise_count: usize,
    controls: usize,
    maximum_work: usize,
}
#[derive(Debug, Serialize)]
pub struct FamilyReport {
    pub hierarchy: u8,
    pub paths: usize,
    pub sweeps: usize,
    pub callbacks: usize,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub pass: Pass,
    pub families: Vec<FamilyReport>,
    pub sweeps: usize,
    pub callbacks: usize,
    pub changed_cells: usize,
    pub minimum_splat_sum: u32,
    pub maximum_splat_sum: u32,
    pub non_normalized_cells: usize,
    pub work: usize,
}
pub struct Output {
    pub splat: Option<Vec<u8>>,
    pub topology: Option<Vec<i32>>,
    pub report: Report,
}

fn summarize_rasters(
    report: &mut Report,
    grid: Grid<'_>,
    splat: Option<&[u8]>,
    topology: Option<&[i32]>,
    cancelled: &impl Fn() -> bool,
) -> Option<()> {
    const CHUNK: usize = 4096;
    let cells = grid.topology.len();
    let values = splat.unwrap_or(grid.splat);
    let mut sums = [0_u16; CHUNK];
    let mut changed = [0_u8; CHUNK];
    for start in (0..cells).step_by(CHUNK) {
        if cancelled() {
            return None;
        }
        let end = (start + CHUNK).min(cells);
        let sums = &mut sums[..end - start];
        let changed = &mut changed[..end - start];
        sums.fill(0);
        changed.fill(0);
        // Read each material plane contiguously so the inner loops can
        // accumulate several cells at once. Eight bytes always fit in u16.
        for (channel, plane) in values.chunks_exact(cells).enumerate() {
            let row = &plane[start..end];
            if splat.is_some() {
                let base = channel * cells;
                let original = &grid.splat[base + start..base + end];
                for ((sum, difference), (&next, &previous)) in sums
                    .iter_mut()
                    .zip(changed.iter_mut())
                    .zip(row.iter().zip(original))
                {
                    *sum += u16::from(next);
                    *difference |= next ^ previous;
                }
            } else {
                for (sum, &value) in sums.iter_mut().zip(row) {
                    *sum += u16::from(value);
                }
            }
        }
        for &sum in sums.iter() {
            let sum = u32::from(sum);
            report.minimum_splat_sum = report.minimum_splat_sum.min(sum);
            report.maximum_splat_sum = report.maximum_splat_sum.max(sum);
            report.non_normalized_cells += usize::from(sum != 255);
        }
        report.changed_cells += if let Some(topology) = topology {
            topology[start..end]
                .iter()
                .zip(&grid.topology[start..end])
                .filter(|(next, previous)| next != previous)
                .count()
        } else {
            changed
                .iter()
                .filter(|&&difference| difference != 0)
                .count()
        };
    }
    Some(())
}

pub fn prepare(
    roads: &[Road<'_>],
    policy: &Policy,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Prepared>, String> {
    policy.validate()?;
    let controls = roads.iter().map(|r| r.path.points().len()).sum::<usize>();
    if roads.len() > 4096
        || controls > 1_048_576
        || !(1..=512_000_000).contains(&maximum_work)
        || controls > maximum_work
        || roads.iter().any(|r| {
            !r.path.length().is_finite()
                || r.path.length() <= 0.
                || r.path.length() > 40000.
                || r.path.range() != (0, r.path.points().len() - 1)
        })
    {
        return Err("Invalid retained road paths or paint allowance".into());
    }
    let mut paths = Vec::new();
    let mut families = Vec::new();
    let mut noise_count = 0;
    for road in roads {
        if cancelled() {
            return Ok(None);
        }
        let family = policy
            .families
            .iter()
            .position(|f| f.hierarchy == road.hierarchy)
            .ok_or("Road has no captured paint family")?;
        noise_count = noise_count.max(
            ((road.path.length() + policy.sample_spacing) / policy.sample_spacing).ceil() as usize,
        );
        paths.push(road.path.clone());
        families.push(family);
    }
    Ok((!cancelled()).then_some(Prepared {
        paths,
        families,
        policy: policy.clone(),
        noise_count,
        controls,
        maximum_work,
    }))
}
impl Prepared {
    pub fn noise_coordinates(&self) -> Vec<f32> {
        (0..self.noise_count)
            .map(|i| i as f32 * self.policy.sample_spacing * self.policy.noise_frequency)
            .collect()
    }
    pub fn apply(
        &self,
        g: Grid<'_>,
        noise: &[f32],
        pass: Pass,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Output>, String> {
        if cancelled() {
            return Ok(None);
        }
        let n = g.geometry.resolution;
        if !(2..=2048).contains(&n)
            || noise.len() != self.noise_count
            || noise.iter().any(|v| !v.is_finite() || v.abs() > 2.)
            || g.splat.len() != n * n * 8
            || g.topology.len() != n * n
        {
            return Err("Invalid road paint grids or noise".into());
        }
        let cells = n * n;
        let overhead = self.controls + cells * 3;
        let available = self
            .maximum_work
            .checked_sub(overhead)
            .filter(|v| *v > 0)
            .ok_or("Road path-paint work budget exceeded")?;
        let mut splat = (pass == Pass::Texture).then(|| g.splat.to_vec());
        let mut topology = (pass == Pass::Topology).then(|| g.topology.to_vec());
        let active: Vec<_> = self
            .paths
            .iter()
            .enumerate()
            .filter(|(i, _)| {
                let f = &self.policy.families[self.families[*i]];
                if pass == Pass::Texture {
                    f.splat_channel.is_some()
                } else {
                    f.topology_mask != 0
                }
            })
            .map(|(i, _)| i)
            .collect();
        let paths: Vec<_> = active
            .iter()
            .map(|&i| {
                let family = &self.policy.families[self.families[i]];
                path_surface::PathSpec {
                    path: &self.paths[i],
                    width: family.width,
                    padding: family.padding,
                }
            })
            .collect();
        let mut sweeps = vec![0; self.policy.families.len()];
        let mut callbacks = vec![0; self.policy.families.len()];
        let Some(walk) = path_surface::walk(
            &paths,
            g.geometry,
            self.policy.sample_spacing,
            available,
            |index, step, _, _| {
                let family = self.families[active[index]];
                sweeps[family] += 1;
                let f = &self.policy.families[family];
                let radius = f.width * 0.5;
                let factor = (noise[step] * self.policy.noise_unsigned_scale
                    + self.policy.noise_unsigned_bias)
                    .clamp(0., 1.);
                Ok(radius + (radius * f.random_scale - radius) * factor)
            },
            &cancelled,
            |index, cell, opacity| {
                let family = self.families[active[index]];
                callbacks[family] += 1;
                let f = &self.policy.families[family];
                if let Some(splat) = &mut splat {
                    path_surface::set_splat(
                        splat,
                        cells,
                        cell,
                        f.splat_channel.expect("active material family"),
                        opacity,
                    );
                }
                if let Some(topology) = &mut topology
                    && opacity > self.policy.topology_threshold
                {
                    topology[cell] |= f.topology_mask;
                }
            },
        )?
        else {
            return Ok(None);
        };
        let mut report = Report {
            engine: VERSION,
            pass,
            families: self
                .policy
                .families
                .iter()
                .enumerate()
                .map(|(i, f)| FamilyReport {
                    hierarchy: f.hierarchy,
                    paths: active.iter().filter(|&&p| self.families[p] == i).count(),
                    sweeps: sweeps[i],
                    callbacks: callbacks[i],
                })
                .collect(),
            sweeps: walk.sweeps,
            callbacks: walk.callbacks,
            changed_cells: 0,
            minimum_splat_sum: u32::MAX,
            maximum_splat_sum: 0,
            non_normalized_cells: 0,
            work: overhead + walk.work,
        };
        if summarize_rasters(
            &mut report,
            g,
            splat.as_deref(),
            topology.as_deref(),
            &cancelled,
        )
        .is_none()
        {
            return Ok(None);
        }
        Ok((!cancelled()).then_some(Output {
            splat,
            topology,
            report,
        }))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn policy() -> Policy {
        Policy {
            sample_spacing: 1.,
            noise_frequency: 0.005,
            noise_unsigned_scale: 0.5,
            noise_unsigned_bias: 1.,
            topology_threshold: 0.3,
            families: vec![
                Family {
                    hierarchy: 1,
                    width: 10.,
                    padding: [1., 1.],
                    random_scale: 0.75,
                    topology_mask: 2048,
                    splat_channel: Some(7),
                },
                Family {
                    hierarchy: 2,
                    width: 4.,
                    padding: [0.4, 1.],
                    random_scale: 0.75,
                    topology_mask: 2048,
                    splat_channel: Some(0),
                },
            ],
        }
    }
    fn path(points: &[[f32; 3]]) -> Path {
        Path::from_controls(points, 1000, || false)
            .unwrap()
            .unwrap()
    }
    fn geometry(n: usize) -> path_surface::Grid {
        path_surface::Grid {
            size: 128.,
            resolution: n,
            origin_y: -500.,
            vertical_span: 1000.,
        }
    }
    fn splat(n: usize) -> Vec<u8> {
        let mut data = vec![0; n * n * 8];
        data[n * n..n * n * 2].fill(255);
        data
    }
    #[test]
    fn paved_and_dirt_share_source_geometry_but_keep_family_materials_and_reverse_order() {
        let paved = path(&[[-40., 10., 0.], [40., 10., 0.]]);
        let dirt = path(&[[0., 10., -40.], [0., 10., 40.]]);
        let prepared = prepare(
            &[
                Road {
                    path: &paved,
                    hierarchy: 1,
                },
                Road {
                    path: &dirt,
                    hierarchy: 2,
                },
            ],
            &policy(),
            1_000_000,
            || false,
        )
        .unwrap()
        .unwrap();
        let n = 64;
        let cells = n * n;
        let material = splat(n);
        let topology = vec![1; cells];
        let noise = vec![0.; prepared.noise_coordinates().len()];
        let input = || Grid {
            geometry: geometry(n),
            splat: &material,
            topology: &topology,
        };
        let texture = prepared
            .apply(input(), &noise, Pass::Texture, || false)
            .unwrap()
            .unwrap();
        let replay = prepared
            .apply(input(), &noise, Pass::Texture, || false)
            .unwrap()
            .unwrap();
        assert_eq!(texture.splat, replay.splat);
        assert!(texture.topology.is_none());
        let values = texture.splat.as_ref().unwrap();
        assert_eq!(values[7 * cells + 32 * n + 32], 255); // paved follows dirt at the crossing
        assert_eq!(values[7 * cells + 32 * n + 42], 255);
        assert_eq!(values[42 * n + 32], 255);
        assert!(
            texture
                .report
                .families
                .iter()
                .all(|f| f.paths == 1 && f.sweeps > 0 && f.callbacks > 0)
        );
        let topo = prepared
            .apply(input(), &noise, Pass::Topology, || false)
            .unwrap()
            .unwrap();
        assert!(topo.splat.is_none());
        assert_eq!(topo.topology.as_ref().unwrap()[32 * n + 32], 2049);
        assert!(topo.topology.as_ref().unwrap().iter().all(|v| v & 1 == 1));
        assert_eq!(texture.report.callbacks, topo.report.callbacks);
        assert_eq!(material, splat(n));
        assert_eq!(topology, vec![1; cells]);
    }
    #[test]
    fn captured_noise_changes_width_without_changing_path_indices_or_endpoints() {
        let p = path(&[[-40., 10., 0.], [0., 10., 0.], [40., 10., 0.]]);
        let before = serde_json::to_vec(&p).unwrap();
        let prepared = prepare(
            &[Road {
                path: &p,
                hierarchy: 1,
            }],
            &policy(),
            1_000_000,
            || false,
        )
        .unwrap()
        .unwrap();
        let n = 128;
        let material = splat(n);
        let topology = vec![0; n * n];
        let input = || Grid {
            geometry: geometry(n),
            splat: &material,
            topology: &topology,
        };
        let count = prepared.noise_coordinates().len();
        let wide = prepared
            .apply(input(), &vec![-2.; count], Pass::Texture, || false)
            .unwrap()
            .unwrap();
        let narrow = prepared
            .apply(input(), &vec![2.; count], Pass::Texture, || false)
            .unwrap()
            .unwrap();
        assert!(wide.report.changed_cells > narrow.report.changed_cells);
        assert_eq!(serde_json::to_vec(&p).unwrap(), before);
        assert_eq!(prepared.noise_coordinates()[20], 20. * 0.005);
    }
    #[test]
    fn source_paint_accepts_empty_cells_and_preserves_unvisited_materials() {
        let path = path(&[[-40., 10., 0.], [40., 10., 0.]]);
        let roads = [Road {
            path: &path,
            hierarchy: 1,
        }];
        let prepared = prepare(&roads, &policy(), 1_000_000, || false)
            .unwrap()
            .unwrap();
        let n = 65;
        let cells = n * n;
        let mut material = vec![0; cells * 8];
        for channel in 0..8 {
            material[channel * cells + cells - 1] = 255;
        }
        let topology = vec![0; cells];
        let input = || Grid {
            geometry: geometry(n),
            splat: &material,
            topology: &topology,
        };
        let noise = vec![0.; prepared.noise_coordinates().len()];
        let output = prepared
            .apply(input(), &noise, Pass::Texture, || false)
            .unwrap()
            .unwrap();
        let painted = output.splat.unwrap();
        assert_eq!(painted[7 * cells + 32 * n + 32], 255);
        assert!((0..8).all(|c| painted[c * cells] == 0));
        let changed = (0..cells)
            .filter(|&i| (0..8).any(|c| painted[c * cells + i] != material[c * cells + i]))
            .count();
        assert_eq!(output.report.changed_cells, changed);
        assert_eq!(output.report.minimum_splat_sum, 0);
        assert_eq!(output.report.maximum_splat_sum, 2040);
        let marked = prepared
            .apply(input(), &noise, Pass::Topology, || false)
            .unwrap()
            .unwrap();
        assert_eq!(marked.report.minimum_splat_sum, 0);
        assert_eq!(marked.report.maximum_splat_sum, 2040);
        assert_eq!(marked.report.non_normalized_cells, cells);
        assert_eq!(
            marked.report.changed_cells,
            marked.topology.unwrap().iter().filter(|&&v| v != 0).count()
        );
        let empty = prepare(&[], &policy(), 1_000_000, || false)
            .unwrap()
            .unwrap();
        let output = empty
            .apply(input(), &[], Pass::Texture, || false)
            .unwrap()
            .unwrap();
        assert_eq!(output.splat.unwrap(), material);
    }
    #[test]
    fn bad_policies_noise_cancellation_and_work_limits_cannot_publish_partial_layers() {
        let p = path(&[[-40., 10., 0.], [40., 10., 0.]]);
        let roads = [Road {
            path: &p,
            hierarchy: 1,
        }];
        let mut bad = policy();
        bad.families[0].splat_channel = Some(8);
        assert!(prepare(&roads, &bad, 1_000_000, || false).is_err());
        let mut bad = policy();
        bad.families.push(bad.families[0].clone());
        assert!(bad.validate().is_err());
        assert!(
            prepare(
                &[Road {
                    path: &p,
                    hierarchy: 3
                }],
                &policy(),
                1_000_000,
                || false
            )
            .is_err()
        );
        let prepared = prepare(&roads, &policy(), 1_000_000, || false)
            .unwrap()
            .unwrap();
        let n = 64;
        let material = splat(n);
        let topology = vec![1; n * n];
        let input = || Grid {
            geometry: geometry(n),
            splat: &material,
            topology: &topology,
        };
        let noise = vec![0.; prepared.noise_coordinates().len()];
        assert!(
            prepared
                .apply(input(), &[], Pass::Texture, || false)
                .is_err()
        );
        assert!(
            prepared
                .apply(input(), &vec![f32::NAN; noise.len()], Pass::Texture, || {
                    false
                })
                .is_err()
        );
        let calls = std::cell::Cell::new(0);
        assert!(
            prepared
                .apply(input(), &noise, Pass::Texture, || {
                    calls.set(calls.get() + 1);
                    calls.get() > 20
                })
                .unwrap()
                .is_none()
        );
        let limited = prepare(&roads, &policy(), 100, || false).unwrap().unwrap();
        assert!(
            limited
                .apply(input(), &noise, Pass::Texture, || false)
                .is_err()
        );
        assert_eq!(material, splat(n));
        assert_eq!(topology, vec![1; n * n]);
    }
}
