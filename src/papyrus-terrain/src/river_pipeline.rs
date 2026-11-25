//! One transactional river derivation from immutable native terrain.
//! Callers verify installation policies/templates and provide exact native noise.
//! No installation adapter, filesystem cache or Unity runtime is implied here.
use crate::{
    native_layers::Layers, path_geometry::Path, path_mesh, river_carve, river_layout,
    river_surface, river_water,
};
use serde::{Deserialize, Serialize};

pub const VERSION: &str = "papyrus-river-pipeline-1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Layout,
    Topology,
    Carve,
    Texture,
    Water,
}

pub struct Policies<'a> {
    pub layout: &'a river_layout::Policy,
    pub carve: &'a river_carve::Policy,
    pub surface: &'a river_surface::Policy,
    pub water: &'a river_water::Policy,
    pub template: &'a path_mesh::Template,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub seed: Option<u32>,
    /// Each stage has this ceiling, including its own preparation work.
    pub maximum_stage_work: usize,
    pub maximum_candidates: usize,
    pub maximum_controls: usize,
    pub water: river_water::Settings,
}
impl Settings {
    pub fn validate(self) -> Result<(), String> {
        self.water.validate()?;
        if self.seed == Some(0)
            || !(1..=512_000_000).contains(&self.maximum_stage_work)
            || !(1..=4096).contains(&self.maximum_candidates)
            || !(2..=1_048_576).contains(&self.maximum_controls)
            || self.water.maximum_work > self.maximum_stage_work
        {
            return Err("Invalid river pipeline resource limits".into());
        }
        Ok(())
    }
}
#[derive(Serialize)]
pub struct Report {
    pub engine: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_sha256: Option<String>,
    pub layout: river_layout::Report,
    pub topology: river_surface::Report,
    pub carve: river_carve::Report,
    pub texture: river_surface::Report,
    pub water: river_water::Report,
}
pub struct Output {
    pub layout_policy: river_layout::Policy,
    pub water_policy: river_water::Policy,
    pub layers: Layers,
    /// Smoothed paths used by carving, painting and water, for downstream export.
    pub paths: Vec<Path>,
    pub local_water: Vec<i16>,
    pub water_chunks: Vec<river_water::PlacedChunk>,
    pub report: Report,
}
/// River world data retained with derived terrain through composition/export.
#[derive(Serialize)]
pub struct World {
    pub layout_policy: river_layout::Policy,
    pub water_policy: river_water::Policy,
    pub paths: Vec<Path>,
    pub local_water: Vec<i16>,
    pub water_chunks: Vec<river_water::PlacedChunk>,
    pub report: Report,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub objects: Option<Objects>,
}
#[derive(Serialize)]
pub struct Objects {
    pub source_sha256: String,
    pub seed: u32,
    pub instances: Vec<Object>,
    pub fitting: serde_json::Value,
    pub terrain: crate::prefab_terrain_effects::Report,
}
#[derive(Serialize)]
pub struct Object {
    pub river: usize,
    pub prefab: String,
    pub position: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
}
impl World {
    /// Source-profile sea-level fallback applies only when no raw river collider
    /// is hit. Registered water is unsuitable here: it excludes dry river banks
    /// and quantizes the surface. This grid is a triangle proxy, not PhysX parity.
    pub fn collider_heights(
        &self,
        size: u32,
        resolution: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<crate::water_mesh::Heights>, String> {
        use crate::water_mesh::{self, RasterSurface, Surface};
        if cancelled() {
            return Ok(None);
        }
        self.water_policy.validate()?;
        if self.water_chunks.len() > 4096 {
            return Err("Too many river collider chunks".into());
        }
        let mut world = Vec::with_capacity(self.water_chunks.len());
        let mut conversion_work = 0;
        for chunk in &self.water_chunks {
            if cancelled() {
                return Ok(None);
            }
            let geometry = &chunk.geometry;
            conversion_work += geometry.mesh.vertices.len() + geometry.mesh.triangles.len();
            if conversion_work > 4_194_304 {
                return Err("River collider geometry budget exceeded".into());
            }
            world.push(
                geometry
                    .mesh
                    .vertices
                    .iter()
                    .map(|p| std::array::from_fn(|i| f64::from(p[i] + geometry.origin[i])))
                    .collect::<Vec<_>>(),
            );
        }
        let surfaces = self
            .water_chunks
            .iter()
            .zip(&world)
            .map(|(c, v)| Surface::new(v, &c.geometry.mesh.triangles))
            .collect::<Result<Vec<_>, _>>()?;
        let entries = surfaces
            .iter()
            .map(|surface| RasterSurface {
                surface,
                two_sided: self.water_policy.two_sided,
            })
            .collect::<Vec<_>>();
        let Some(mut result) = water_mesh::rasterize_heights(
            size as f64,
            resolution,
            &entries,
            128_000_000 - conversion_work,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        result.report.work += conversion_work;
        for chunk in result.heights.chunks_mut(4096) {
            if cancelled() {
                return Ok(None);
            }
            result.report.work += chunk.len();
            if result.report.work > 128_000_000 {
                return Err("River collider fallback work budget exceeded".into());
            }
            for h in chunk {
                if h.is_nan() {
                    *h = 0.;
                }
            }
        }
        Ok(Some(result))
    }
}
impl Output {
    pub fn into_parts(self) -> (Layers, World) {
        (
            self.layers,
            World {
                layout_policy: self.layout_policy,
                water_policy: self.water_policy,
                paths: self.paths,
                local_water: self.local_water,
                water_chunks: self.water_chunks,
                report: self.report,
                objects: None,
            },
        )
    }
}

fn sample(
    stage: Stage,
    coordinates: &[f32],
    sampler: &mut impl FnMut(Stage, &[f32]) -> Result<Option<Vec<f32>>, String>,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Vec<f32>>, String> {
    if cancelled() {
        return Ok(None);
    }
    let Some(noise) = (if coordinates.is_empty() {
        Some(vec![])
    } else {
        sampler(stage, coordinates)?
    }) else {
        return Ok(None);
    };
    if cancelled() {
        return Ok(None);
    }
    if noise.len() != coordinates.len() || noise.iter().any(|v| !v.is_finite() || v.abs() > 2.) {
        return Err(format!("Invalid native noise for river {stage:?} stage"));
    }
    Ok(Some(noise))
}

/// Publish only after all five stages succeed. Existing water is merged by the
/// source's raise-only rule. Neither input buffer is modified, even on failure.
/// Native transport uses the fixed -500..500 m vertical frame.
pub fn generate(
    source: &Layers,
    existing_water: &[i16],
    policies: Policies<'_>,
    settings: Settings,
    sampler: impl FnMut(Stage, &[f32]) -> Result<Option<Vec<f32>>, String>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    generate_with_topology(
        source,
        &source.topology,
        existing_water,
        policies,
        settings,
        sampler,
        cancelled,
    )
}

/// Route and paint rivers using classified topology while borrowing the remaining
/// immutable terrain channels directly from the source.
pub fn generate_with_topology(
    source: &Layers,
    classified_topology: &[i32],
    existing_water: &[i16],
    policies: Policies<'_>,
    settings: Settings,
    mut sampler: impl FnMut(Stage, &[f32]) -> Result<Option<Vec<f32>>, String>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    if cancelled() {
        return Ok(None);
    }
    settings.validate()?;
    policies.layout.validate()?;
    policies.carve.validate()?;
    policies.surface.validate()?;
    policies.water.validate()?;
    policies.template.validate()?;
    let (hr, sr) = crate::native_layers::dimensions(source.size)?;
    if source.height_resolution != hr
        || source.surface_resolution != sr
        || source.height.len() != hr * hr
        || existing_water.len() != hr * hr
        || source.splat.len() != sr * sr * 8
        || source.biome.len() != sr * sr * 5
        || classified_topology.len() != sr * sr
        || policies.surface.bank.radius_cells >= sr
    {
        return Err("Invalid river pipeline input grids".into());
    }
    for values in [source.height.as_slice(), existing_water] {
        for chunk in values.chunks(4096) {
            if cancelled() {
                return Ok(None);
            }
            if chunk.iter().any(|h| !(0..=32766).contains(h)) {
                return Err("Invalid river pipeline height or water encoding".into());
            }
        }
    }
    let Some(noise) = sample(
        Stage::Layout,
        &policies.layout.noise_coordinates()?,
        &mut sampler,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let mut terrain = source.view();
    terrain.topology = classified_topology;
    let Some(layout) = river_layout::generate_view(
        &terrain,
        policies.layout,
        &noise,
        &river_layout::Settings {
            seed: settings.seed,
            maximum_work: settings.maximum_stage_work,
            maximum_candidates: settings.maximum_candidates,
            maximum_controls: settings.maximum_controls,
        },
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let Some(plan) = river_surface::prepare(
        &layout.paths,
        policies.layout,
        policies.surface,
        settings.maximum_stage_work,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let Some(noise) = sample(
        Stage::Topology,
        &plan.noise_coordinates(),
        &mut sampler,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let Some((topology, topology_report)) = plan.apply_topology(
        river_surface::Grid {
            size: source.size as f32,
            height_resolution: hr,
            surface_resolution: sr,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &source.height,
            splat: &source.splat,
            topology: classified_topology,
        },
        &noise,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    drop(plan);
    let Some(plan) = river_carve::prepare(
        &layout.paths,
        policies.layout,
        policies.carve,
        settings.maximum_stage_work,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let Some(noise) = sample(
        Stage::Carve,
        &plan.noise_coordinates(),
        &mut sampler,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let Some(carved) = plan.carve(
        river_carve::Grid {
            size: source.size as f32,
            resolution: hr,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &source.height,
        },
        &noise,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    drop(plan);
    let Some(plan) = river_surface::prepare(
        &carved.paths,
        policies.layout,
        policies.surface,
        settings.maximum_stage_work,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let Some(noise) = sample(
        Stage::Texture,
        &plan.noise_coordinates(),
        &mut sampler,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let Some((splat, texture_report)) = plan.apply_texture(
        river_surface::Grid {
            size: source.size as f32,
            height_resolution: hr,
            surface_resolution: sr,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &carved.height,
            splat: &source.splat,
            topology: &topology,
        },
        &noise,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    drop(plan);
    let Some(plan) = river_water::prepare(
        &carved.paths,
        policies.template,
        policies.layout,
        policies.water,
        settings.water,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let Some(noise) = sample(
        Stage::Water,
        &plan.noise_coordinates(),
        &mut sampler,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let Some(water) = plan.build(
        river_water::Grid {
            size: source.size as f32,
            resolution: hr,
            height: &carved.height,
            existing_water,
        },
        &noise,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    // Biomes are the only unchanged large channel retained in the derived layers.
    // Copy in bounded pieces so cancellation also covers final publication work.
    let mut biome = Vec::with_capacity(source.biome.len());
    for chunk in source.biome.chunks(65536) {
        if cancelled() {
            return Ok(None);
        }
        biome.extend_from_slice(chunk);
    }
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(Output {
        layout_policy: policies.layout.clone(),
        water_policy: policies.water.clone(),
        layers: Layers {
            size: source.size,
            terrain_seed: source.terrain_seed,
            biome_seed: source.biome_seed,
            height_resolution: hr,
            surface_resolution: sr,
            height: carved.height,
            biome,
            splat,
            topology,
            normalized_biome_cells: source.normalized_biome_cells,
            normalized_splat_cells: source.normalized_splat_cells,
        },
        paths: carved.paths,
        local_water: water.water,
        water_chunks: water.chunks,
        report: Report {
            engine: VERSION,
            source_sha256: None,
            layout: layout.report,
            topology: topology_report,
            carve: carved.report,
            texture: texture_report,
            water: water.report,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    struct Fixture {
        layers: Layers,
        layout: river_layout::Policy,
        carve: river_carve::Policy,
        surface: river_surface::Policy,
        water: river_water::Policy,
        template: path_mesh::Template,
    }
    fn policy<T: serde::de::DeserializeOwned>(raw: &str) -> T {
        let value: serde_json::Value = serde_json::from_str(raw).unwrap();
        serde_json::from_value(value["policy"].clone()).unwrap()
    }
    impl Fixture {
        fn new() -> Self {
            let (hr, sr) = crate::native_layers::dimensions(1000).unwrap();
            let mut layers = Layers {
                size: 1000,
                terrain_seed: 1,
                biome_seed: 2,
                height_resolution: hr,
                surface_resolution: sr,
                height: vec![0; hr * hr],
                biome: vec![0; sr * sr * 5],
                splat: vec![0; sr * sr * 8],
                topology: vec![1; sr * sr],
                normalized_biome_cells: 0,
                normalized_splat_cells: 0,
            };
            layers.splat[..sr * sr].fill(255);
            layers.biome[sr * sr..2 * sr * sr].fill(255);
            for z in 0..hr {
                for x in 0..hr {
                    let wx = x as f32 / (hr - 1) as f32 * 1000. - 500.;
                    layers.height[z * hr + x] = crate::erosion::encode(150. - wx.abs() * 0.6);
                }
            }
            for z in 0..sr {
                for x in 0..sr {
                    let wx = (x as f32 + 0.5) / sr as f32 * 1000. - 500.;
                    if wx.abs() > 250. {
                        layers.topology[z * sr + x] = 128;
                    }
                }
            }
            let mut layout: river_layout::Policy = policy(include_str!(
                "../../../compatibility/rivers/server-bd19c4ac-layout.json"
            ));
            layout.scan = [40., 100., 120.];
            layout.minimum_controls = 6;
            layout.separation = 70.;
            Self {
                layers,
                layout,
                carve: policy(include_str!(
                    "../../../compatibility/rivers/server-bd19c4ac-terrain.json"
                )),
                surface: policy(include_str!(
                    "../../../compatibility/rivers/server-bd19c4ac-surface.json"
                )),
                water: policy(include_str!(
                    "../../../compatibility/rivers/client-0f703d43-water-mesh.json"
                )),
                template: path_mesh::Template {
                    vertices: vec![[-1., 0., 0.], [-1., 0., 18.], [1., 0., 0.], [1., 0., 18.]],
                    normals: vec![[0., 1., 0.]; 4],
                    tangents: vec![[1., 0., 0., 1.]; 4],
                    uv: vec![[0., 0.], [0., 1.], [1., 0.], [1., 1.]],
                    triangles: vec![[0, 1, 2], [2, 1, 3]],
                    center: [0., 0., 9.],
                    extents: [1., 0., 9.],
                },
            }
        }
        fn policies(&self) -> Policies<'_> {
            Policies {
                layout: &self.layout,
                carve: &self.carve,
                surface: &self.surface,
                water: &self.water,
                template: &self.template,
            }
        }
        fn settings() -> Settings {
            Settings {
                seed: Some(123),
                maximum_stage_work: 32_000_000,
                maximum_candidates: 128,
                maximum_controls: 65536,
                water: river_water::Settings {
                    maximum_work: 16_000_000,
                    maximum_chunks: 4096,
                    maximum_geometry_bytes: 16 * 1024 * 1024,
                },
            }
        }
    }
    #[test]
    fn publishes_matching_river_channels_paths_and_geometry_without_changing_source() {
        let f = Fixture::new();
        let before = f.layers.clone();
        let mut water = vec![0; before.height.len()];
        water[0] = 20000; // An existing independent water body must survive.
        let run = |settings| {
            generate(
                &f.layers,
                &water,
                f.policies(),
                settings,
                |_, coordinates| Ok(Some(vec![-0.5; coordinates.len()])),
                || false,
            )
            .unwrap()
            .unwrap()
        };
        let a = run(Fixture::settings());
        let mut unclassified = before.clone();
        unclassified.topology.fill(0);
        let b = generate_with_topology(
            &unclassified,
            &before.topology,
            &water,
            f.policies(),
            Fixture::settings(),
            |_, coordinates| Ok(Some(vec![-0.5; coordinates.len()])),
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(unclassified.topology.iter().all(|&topology| topology == 0));
        assert!(!a.paths.is_empty());
        assert_eq!(a.report.layout.selected, a.paths.len());
        assert!(a.report.carve.changed_samples > 0);
        assert!(a.report.texture.changed_splat_cells > 0);
        assert!(a.report.topology.changed_topology_cells > 0);
        assert_eq!(a.report.topology.changed_splat_cells, 0);
        assert_eq!(a.report.texture.changed_topology_cells, 0);
        assert!(a.report.water.changed_samples > 0);
        assert!(!a.water_chunks.is_empty());
        assert_eq!(a.local_water[0], 20000);
        assert_eq!(a.layers.biome, before.biome);
        assert_eq!(a.layers.biome, b.layers.biome);
        assert_eq!(a.layers.height, b.layers.height);
        assert_eq!(a.layers.splat, b.layers.splat);
        assert_eq!(a.layers.topology, b.layers.topology);
        assert_eq!(a.local_water, b.local_water);
        assert_eq!(
            serde_json::to_vec(&a.report).unwrap(),
            serde_json::to_vec(&b.report).unwrap()
        );
        assert_eq!(
            serde_json::to_vec(&a.paths).unwrap(),
            serde_json::to_vec(&b.paths).unwrap()
        );
        assert_eq!(
            serde_json::to_vec(&a.water_chunks).unwrap(),
            serde_json::to_vec(&b.water_chunks).unwrap()
        );
        let mut alternate = Fixture::settings();
        alternate.seed = Some(999);
        let c = run(alternate);
        // Different scan orders may converge on the same best feasible paths.
        assert_eq!(c.report.layout.seed, Some(999));
        assert_eq!(f.layers.height, before.height);
        assert_eq!(f.layers.splat, before.splat);
        assert_eq!(f.layers.topology, before.topology);
        assert_eq!(water.iter().filter(|&&v| v != 0).count(), 1);
    }
    #[test]
    fn retained_world_supplies_finite_collider_heights_with_ocean_fallback() {
        let f = Fixture::new();
        let output = generate(
            &f.layers,
            &vec![0; f.layers.height.len()],
            f.policies(),
            Fixture::settings(),
            |_, coordinates| Ok(Some(vec![-0.5; coordinates.len()])),
            || false,
        )
        .unwrap()
        .unwrap();
        let (layers, world) = output.into_parts();
        let raw = world
            .collider_heights(layers.size, layers.surface_resolution, || false)
            .unwrap()
            .unwrap();
        assert_eq!(
            raw.heights.len(),
            layers.surface_resolution * layers.surface_resolution
        );
        assert!(raw.heights.iter().all(|v| v.is_finite()));
        assert!(raw.heights.contains(&0.));
        assert!(raw.report.changed_samples > 0);
        assert!(raw.heights.iter().any(|v| *v != 0.));
        assert!(
            world
                .collider_heights(layers.size, layers.surface_resolution, || true)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn late_cancellation_and_noise_failure_discard_the_entire_derivation() {
        let f = Fixture::new();
        let before = f.layers.clone();
        let water = vec![0; before.height.len()];
        for fail in [false, true] {
            let stages = RefCell::new(Vec::new());
            let stopped = Cell::new(false);
            let result = generate(
                &f.layers,
                &water,
                f.policies(),
                Fixture::settings(),
                |stage, coordinates| {
                    stages.borrow_mut().push(stage);
                    if stage == Stage::Water {
                        if fail {
                            return Err("native worker failed".into());
                        }
                        stopped.set(true);
                    }
                    Ok(Some(vec![-0.5; coordinates.len()]))
                },
                || stopped.get(),
            );
            assert_eq!(
                *stages.borrow(),
                vec![
                    Stage::Layout,
                    Stage::Topology,
                    Stage::Carve,
                    Stage::Texture,
                    Stage::Water
                ]
            );
            if fail {
                assert!(result.is_err());
            } else {
                assert!(result.unwrap().is_none());
            }
            assert_eq!(f.layers.height, before.height);
            assert_eq!(f.layers.splat, before.splat);
            assert_eq!(f.layers.topology, before.topology);
            assert!(water.iter().all(|&v| v == 0));
        }
    }
    #[test]
    fn invalid_inputs_are_rejected_before_noise_and_empty_layout_is_valid() {
        let mut f = Fixture::new();
        let water = vec![0; f.layers.height.len()];
        let called = Cell::new(false);
        let mut settings = Fixture::settings();
        settings.maximum_stage_work = 0;
        assert!(
            generate(
                &f.layers,
                &water,
                f.policies(),
                settings,
                |_, _| {
                    called.set(true);
                    Ok(Some(vec![]))
                },
                || false
            )
            .is_err()
        );
        assert!(!called.get());
        assert!(
            generate(
                &f.layers,
                &water[..water.len() - 1],
                f.policies(),
                Fixture::settings(),
                |_, _| {
                    called.set(true);
                    Ok(Some(vec![]))
                },
                || false
            )
            .is_err()
        );
        assert!(!called.get());
        assert!(
            generate(
                &f.layers,
                &water,
                f.policies(),
                Fixture::settings(),
                |_, coordinates| Ok(Some(vec![f32::NAN; coordinates.len()])),
                || false
            )
            .is_err()
        );
        f.layout.maximum_rivers = 0;
        let empty = generate(
            &f.layers,
            &water,
            f.policies(),
            Fixture::settings(),
            |_, coordinates| Ok(Some(vec![-0.5; coordinates.len()])),
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(empty.paths.is_empty());
        assert!(empty.water_chunks.is_empty());
        assert_eq!(empty.layers.height, f.layers.height);
        assert_eq!(empty.layers.splat, f.layers.splat);
        assert_eq!(empty.local_water, water);
    }
}
