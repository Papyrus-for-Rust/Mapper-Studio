//! Source-template river water geometry and raw triangle registration.
//! Does not cook PhysX colliders or instantiate WaterBody/RiverInfo objects.
use crate::{path_geometry::Path, path_mesh, river_layout, water_mesh};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
pub const VERSION: &str = "papyrus-river-water-1";
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub chunk_repetitions: usize,
    pub normal_smoothing: f32,
    pub round_vertices: u32,
    pub snap_to_terrain: bool,
    pub snap_open_endpoints: bool,
    pub top_aligned: bool,
    pub mesh_offset: f32,
    pub two_sided: bool,
    pub water_body_type: i32,
    pub add_topology: bool,
    pub clearance_rule: String,
}

impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=32).contains(&self.chunk_repetitions)
            || !self.normal_smoothing.is_finite()
            || !(0. ..=1.).contains(&self.normal_smoothing)
            || self.round_vertices > 6
            || self.snap_to_terrain
            || !self.mesh_offset.is_finite()
            || self.mesh_offset.abs() > 32.
            || self.water_body_type != 2
            || self.add_topology
            || self.clearance_rule != "inverse_world_z"
        {
            return Err("Unsupported river water policy".into());
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub maximum_work: usize,
    pub maximum_chunks: usize,
    pub maximum_geometry_bytes: usize,
}
impl Settings {
    pub fn validate(self) -> Result<(), String> {
        if !(1..=512_000_000).contains(&self.maximum_work)
            || !(1..=4096).contains(&self.maximum_chunks)
            || !(1..=512 * 1024 * 1024).contains(&self.maximum_geometry_bytes)
        {
            Err("Invalid river water resource allowance".into())
        } else {
            Ok(())
        }
    }
}
pub struct Grid<'a> {
    pub size: f32,
    pub resolution: usize,
    pub height: &'a [i16],
    pub existing_water: &'a [i16],
}
pub struct Prepared<'a> {
    paths: &'a [Path],
    template: &'a path_mesh::Template,
    shape: &'a river_layout::Policy,
    policy: &'a Policy,
    settings: Settings,
    queries: Vec<Vec<f32>>,
    preparation_work: usize,
}
#[derive(Serialize)]
pub struct PlacedChunk {
    pub river: usize,
    pub chunk: usize,
    pub geometry: path_mesh::Chunk,
}
pub struct Output {
    pub water: Vec<i16>,
    pub chunks: Vec<PlacedChunk>,
    pub report: Report,
}
#[derive(Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub rivers: usize,
    pub chunks: usize,
    pub vertices: usize,
    pub triangles: usize,
    pub geometry_bytes: usize,
    pub changed_samples: usize,
    pub rejected_intersections: usize,
    pub clearance_metres: f32,
    pub work: usize,
    pub physics_cooking_validated: bool,
    pub runtime_bodies_created: bool,
}
fn settings(
    path: &Path,
    shape: &river_layout::Policy,
    policy: &Policy,
    work: usize,
) -> path_mesh::Settings {
    path_mesh::Settings {
        width: shape.width,
        chunk_repetitions: policy.chunk_repetitions,
        maximum_work: work.min(32_000_000),
        snap_start: policy.snap_open_endpoints && !path.is_circular(),
        snap_end: policy.snap_open_endpoints && !path.is_circular(),
        mesh_offset: policy.mesh_offset,
        top_aligned: policy.top_aligned,
    }
}
pub fn prepare<'a>(
    paths: &'a [Path],
    template: &'a path_mesh::Template,
    shape: &'a river_layout::Policy,
    policy: &'a Policy,
    limits: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Prepared<'a>>, String> {
    if cancelled() {
        return Ok(None);
    }
    limits.validate()?;
    policy.validate()?;
    shape.validate()?;
    template.validate()?;
    if paths.len() > 4096 {
        return Err("Excessive river water path roster".into());
    }
    let mut queries = Vec::new();
    let mut work = 0;
    let mut chunks = 0;
    let mut bytes = 0;
    let mut samples = 0;
    for path in paths {
        if cancelled() {
            return Ok(None);
        }
        let remaining = limits.maximum_work - work;
        if remaining == 0 {
            return Err("River water preparation work exhausted".into());
        }
        let s = settings(path, shape, policy, remaining);
        let plan = path_mesh::Plan::new(path, template, s)
            .map_err(|e| format!("River water mesh plan failed: {e:?}"))?;
        let per_chunk = policy.chunk_repetitions.min(plan.repetitions);
        if per_chunk * template.vertices.len() > water_mesh::MAX_VERTICES
            || per_chunk * template.triangles.len() > water_mesh::MAX_TRIANGLES
        {
            return Err("River water chunk exceeds raw surface limits".into());
        }
        chunks += plan.chunks;
        bytes += plan.repetitions * (template.vertices.len() * 48 + template.triangles.len() * 12);
        if chunks > limits.maximum_chunks || bytes > limits.maximum_geometry_bytes {
            return Err("River water geometry allowance exceeded".into());
        }
        let Some(q) = path_mesh::surface_queries(path, &[template], s, &cancelled)
            .map_err(|e| format!("River water noise plan failed: {e:?}"))?
        else {
            return Ok(None);
        };
        work += q.work;
        samples += q.distances.len();
        if samples > 1_000_000 {
            return Err("River water noise allowance exceeded".into());
        }
        queries.push(q.distances);
    }
    Ok((!cancelled()).then_some(Prepared {
        paths,
        template,
        shape,
        policy,
        settings: limits,
        queries,
        preparation_work: work,
    }))
}
fn height01(h: &[i16], n: usize, u: f32, v: f32) -> f32 {
    let x = u.clamp(0., 1.) * (n - 1) as f32;
    let z = v.clamp(0., 1.) * (n - 1) as f32;
    let a = x as usize;
    let b = z as usize;
    let c = (a + 1).min(n - 1);
    let d = (b + 1).min(n - 1);
    let at = |x: usize, z: usize| h[z * n + x] as f32 * 3.051944E-5;
    let low = at(a, b) + (at(c, b) - at(a, b)) * x.fract();
    let high = at(a, d) + (at(c, d) - at(a, d)) * x.fract();
    low + (high - low) * z.fract()
}
impl Prepared<'_> {
    pub fn noise_coordinates(&self) -> Vec<f32> {
        self.queries
            .iter()
            .flatten()
            .map(|d| d * self.shape.noise_frequency)
            .collect()
    }
    pub fn build(
        &self,
        g: Grid<'_>,
        noise: &[f32],
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Output>, String> {
        if cancelled() {
            return Ok(None);
        }
        let n = g.resolution;
        if !(2..=4097).contains(&n)
            || n.checked_mul(n) != Some(g.height.len())
            || g.existing_water.len() != g.height.len()
            || !g.size.is_finite()
            || !(1. ..=6000.).contains(&g.size)
            || noise.len() != self.queries.iter().map(Vec::len).sum::<usize>()
            || noise.iter().any(|v| !v.is_finite() || v.abs() > 2.)
        {
            return Err("Invalid river water grids or native noise batch".into());
        }
        let mut work = self.preparation_work;
        for chunk in g.height.chunks(4096) {
            if cancelled() {
                return Ok(None);
            }
            if chunk.len() > self.settings.maximum_work - work {
                return Err("River water work exhausted".into());
            }
            work += chunk.len();
            if chunk.iter().any(|h| !(0..=32766).contains(h)) {
                return Err("Invalid river ground heights".into());
            }
        }
        let mut chunks = Vec::new();
        let mut at = 0;
        for (river, (path, queries)) in self.paths.iter().zip(&self.queries).enumerate() {
            if cancelled() {
                return Ok(None);
            }
            let radii: BTreeMap<_, _> = queries
                .iter()
                .zip(&noise[at..at + queries.len()])
                .map(|(&d, &noise)| (d.to_bits(), self.shape.radius(d, path.length(), noise)))
                .collect();
            at += queries.len();
            let remaining = self.settings.maximum_work - work;
            if remaining == 0 {
                return Err("River water work exhausted".into());
            }
            let s = settings(path, self.shape, self.policy, remaining);
            let plan = path_mesh::Plan::new(path, self.template, s)
                .map_err(|e| format!("River water plan failed: {e:?}"))?;
            let radius = |d: f32| radii.get(&d.to_bits()).copied();
            for chunk in 0..plan.chunks {
                if cancelled() {
                    return Ok(None);
                }
                let remaining = self.settings.maximum_work - work;
                if remaining == 0 {
                    return Err("River water work exhausted".into());
                }
                let s = settings(path, self.shape, self.policy, remaining);
                let Some(geometry) = path_mesh::deform_finished_surface_chunk(
                    path,
                    &[self.template],
                    0,
                    path_mesh::FinishedSurface {
                        surface: path_mesh::Surface {
                            settings: s,
                            snap_to_terrain: false,
                            radius: &radius,
                        },
                        finishing: path_mesh::Finishing {
                            normal_smoothing: self.policy.normal_smoothing,
                            round_vertices: self.policy.round_vertices,
                        },
                    },
                    chunk,
                    |x, z| {
                        Some(
                            -500.
                                + height01(g.height, n, x / g.size + 0.5, z / g.size + 0.5) * 1000.,
                        )
                    },
                    &cancelled,
                )
                .map_err(|e| format!("River water deformation failed: {e:?}"))?
                else {
                    return Ok(None);
                };
                work += geometry.work;
                chunks.push(PlacedChunk {
                    river,
                    chunk,
                    geometry,
                });
            }
        }
        let mut world = Vec::with_capacity(chunks.len());
        for c in &chunks {
            if cancelled() {
                return Ok(None);
            }
            if c.geometry.mesh.vertices.len() > self.settings.maximum_work - work {
                return Err("River water conversion work exhausted".into());
            }
            work += c.geometry.mesh.vertices.len();
            world.push(
                c.geometry
                    .mesh
                    .vertices
                    .iter()
                    .map(|p| std::array::from_fn(|i| f64::from(p[i] + c.geometry.origin[i])))
                    .collect::<Vec<_>>(),
            );
        }
        let surfaces: Vec<_> = chunks
            .iter()
            .zip(&world)
            .map(|(c, v)| water_mesh::Surface::new(v, &c.geometry.mesh.triangles))
            .collect::<Result<_, _>>()?;
        let surfaces: Vec<_> = surfaces
            .iter()
            .map(|s| water_mesh::RasterSurface {
                surface: s,
                two_sided: self.policy.two_sided,
            })
            .collect();
        let remaining = self.settings.maximum_work - work;
        if remaining == 0 {
            return Err("River water registration work exhausted".into());
        }
        let mut rejected = 0;
        let Some(result) = water_mesh::rasterize_mapped(
            water_mesh::RasterGrid {
                size: g.size as f64,
                resolution: n,
                existing: g.existing_water,
            },
            &surfaces,
            remaining.min(128_000_000),
            |x, z, y| {
                let normalized = ((y as f32) + 500.) * (1. / 1000.);
                if !(0. ..=1.).contains(&normalized) {
                    return Err("Unencodable river water height".into());
                }
                let u = (x as f32 + 0.5) / n as f32;
                let v = (z as f32 + 0.5) / n as f32;
                if normalized < height01(g.height, n, u, v) - 1. / g.size {
                    rejected += 1;
                    return Ok(None);
                }
                Ok(Some((normalized * 32766. + 0.5) as i16))
            },
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        work += result.report.work;
        let vertices = chunks
            .iter()
            .map(|c| c.geometry.mesh.vertices.len())
            .sum::<usize>();
        let triangles = chunks
            .iter()
            .map(|c| c.geometry.mesh.triangles.len())
            .sum::<usize>();
        let report = Report {
            engine: VERSION,
            rivers: self.paths.len(),
            chunks: chunks.len(),
            vertices,
            triangles,
            geometry_bytes: vertices * 48 + triangles * 12,
            changed_samples: result.report.changed_samples,
            rejected_intersections: rejected,
            clearance_metres: 1000. / g.size,
            work,
            physics_cooking_validated: false,
            runtime_bodies_created: false,
        };
        Ok((!cancelled()).then_some(Output {
            water: result.water,
            chunks,
            report,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn shape() -> river_layout::Policy {
        let v: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/rivers/server-bd19c4ac-layout.json"
        ))
        .unwrap();
        let mut p: river_layout::Policy = serde_json::from_value(v["policy"].clone()).unwrap();
        p.random_scale = 1.;
        p.radius_growth = [1., 1., 1000.];
        p.radius_mouth = [1., 1., 100.];
        p
    }
    fn policy() -> Policy {
        let v: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/rivers/client-0f703d43-water-mesh.json"
        ))
        .unwrap();
        let mut p: Policy = serde_json::from_value(v["policy"].clone()).unwrap();
        p.snap_open_endpoints = false;
        p
    }
    fn template() -> path_mesh::Template {
        path_mesh::Template {
            vertices: vec![[-1., 0., 0.], [-1., 0., 2.], [1., 0., 0.], [1., 0., 2.]],
            normals: vec![[0., 1., 0.]; 4],
            tangents: vec![[1., 0., 0., 1.]; 4],
            uv: vec![[0., 0.], [0., 1.], [1., 0.], [1., 1.]],
            triangles: vec![[0, 1, 2], [2, 1, 3]],
            center: [0., 0., 1.],
            extents: [1., 0., 1.],
        }
    }
    fn limits() -> Settings {
        Settings {
            maximum_work: 2_000_000,
            maximum_chunks: 100,
            maximum_geometry_bytes: 1024 * 1024,
        }
    }
    fn path(yaw: f32) -> Path {
        let (s, c) = yaw.to_radians().sin_cos();
        Path::from_controls(
            &[[-24. * c, 10., -24. * s], [24. * c, 10., 24. * s]],
            1000,
            || false,
        )
        .unwrap()
        .unwrap()
    }
    #[test]
    fn template_water_follows_rotated_paths_and_replays_without_altering_inputs() {
        let h = vec![16383; 65 * 65];
        let water = vec![0; 65 * 65];
        let mesh = template();
        let shape = shape();
        let p = policy();
        for yaw in [0., 37., 123., 271.] {
            let paths = [path(yaw)];
            let plan = prepare(&paths, &mesh, &shape, &p, limits(), || false)
                .unwrap()
                .unwrap();
            let noise = vec![0.; plan.noise_coordinates().len()];
            let run = || {
                plan.build(
                    Grid {
                        size: 128.,
                        resolution: 65,
                        height: &h,
                        existing_water: &water,
                    },
                    &noise,
                    || false,
                )
                .unwrap()
                .unwrap()
            };
            let a = run();
            let b = run();
            assert_eq!(a.water, b.water);
            assert!(a.report.changed_samples > 0);
            assert_eq!(a.report.chunks, 2);
            assert!(crate::erosion::decode(a.water[32 * 65 + 32]) > 9.4);
            assert!(!a.report.runtime_bodies_created);
            assert!(!a.report.physics_cooking_validated);
            assert_eq!(a.water[0], 0);
        }
        assert!(h.iter().all(|&v| v == 16383));
        assert!(water.iter().all(|&v| v == 0));
    }
    #[test]
    fn registration_uses_source_world_z_clearance_and_retains_higher_water() {
        let paths = [path(0.)];
        let mesh = template();
        let shape = shape();
        let p = policy();
        let plan = prepare(&paths, &mesh, &shape, &p, limits(), || false)
            .unwrap()
            .unwrap();
        let noise = vec![0.; plan.noise_coordinates().len()];
        let water = vec![0; 129 * 129];
        for (ground, wet) in [(10.4, true), (10.6, false)] {
            let h = vec![crate::erosion::encode(ground); 129 * 129];
            let a = plan
                .build(
                    Grid {
                        size: 1000.,
                        resolution: 129,
                        height: &h,
                        existing_water: &water,
                    },
                    &noise,
                    || false,
                )
                .unwrap()
                .unwrap();
            assert_eq!(a.report.changed_samples > 0, wet);
            assert_eq!(a.report.clearance_metres, 1.);
            if !wet {
                assert!(a.report.rejected_intersections > 0);
            }
        }
        let h = vec![16383; 129 * 129];
        let high = vec![crate::erosion::encode(50.); 129 * 129];
        let a = plan
            .build(
                Grid {
                    size: 1000.,
                    resolution: 129,
                    height: &h,
                    existing_water: &high,
                },
                &noise,
                || false,
            )
            .unwrap()
            .unwrap();
        assert_eq!(a.water, high);
        assert_eq!(a.report.changed_samples, 0);
    }
    #[test]
    fn cancellation_limits_and_bad_batches_cannot_publish_partial_water() {
        let paths = [path(37.)];
        let mesh = template();
        let shape = shape();
        let p = policy();
        let plan = prepare(&paths, &mesh, &shape, &p, limits(), || false)
            .unwrap()
            .unwrap();
        let noise = vec![0.; plan.noise_coordinates().len()];
        let h = vec![16383; 65 * 65];
        let water = vec![0; 65 * 65];
        let grid = || Grid {
            size: 128.,
            resolution: 65,
            height: &h,
            existing_water: &water,
        };
        let calls = std::cell::Cell::new(0);
        assert!(
            plan.build(grid(), &noise, || {
                calls.set(calls.get() + 1);
                calls.get() > 100
            })
            .unwrap()
            .is_none()
        );
        assert!(plan.build(grid(), &[], || false).is_err());
        let mut low = limits();
        low.maximum_chunks = 1;
        assert!(prepare(&paths, &mesh, &shape, &p, low, || false).is_err());
        low = limits();
        low.maximum_geometry_bytes = 1;
        assert!(prepare(&paths, &mesh, &shape, &p, low, || false).is_err());
        let mut invalid = p.clone();
        invalid.water_body_type = 4;
        assert!(prepare(&paths, &mesh, &shape, &invalid, limits(), || false).is_err());
        assert!(water.iter().all(|&h| h == 0));
    }
}
