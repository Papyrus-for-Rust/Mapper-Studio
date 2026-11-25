//! Late AddToHeightMap over raw generated collider triangles, separate from terrain.
//! This does not reproduce PhysX cooking or supply missing scene contributors.
use crate::{decor_collider, decor_fit};
use serde::Serialize;

pub const VERSION: &str = "papyrus-gameplay-height-1";
pub struct Output {
    pub height: Vec<i16>,
    pub report: Report,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub contributors: usize,
    pub changed_samples: usize,
    pub work: usize,
    pub physics_cooking_validated: bool,
}
/// Independent raise-only passes combine by maximum without changing terrain.
/// Owned grids are discarded on cancellation or malformed output.
pub fn combine(
    terrain: &[i16],
    mut first: Output,
    second: Output,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    if first.height.len() != terrain.len() || second.height.len() != terrain.len() {
        return Err("Gameplay height merge dimensions differ".into());
    }
    let mut changed = 0;
    for (i, ((&base, a), b)) in terrain
        .iter()
        .zip(&mut first.height)
        .zip(second.height)
        .enumerate()
    {
        if i % 4096 == 0 && cancelled() {
            return Ok(None);
        }
        if !(0..=32766).contains(&base) || *a < base || *a > 32766 || b < base || b > 32766 {
            return Err("Invalid gameplay height merge sample".into());
        }
        *a = (*a).max(b);
        changed += usize::from(*a != base);
    }
    if cancelled() {
        return Ok(None);
    }
    first.report.contributors += second.report.contributors;
    first.report.work += second.report.work;
    first.report.changed_samples = changed;
    first.report.physics_cooking_validated = false;
    Ok(Some(first))
}
/// Build the road contribution from final, already source-bound surface meshes.
/// The server uses LOD 0 for collision; visual LODs and dirt roads do not enter it.
pub fn roads(
    layers: &crate::native_layers::Layers,
    roads: &crate::road_network::Report,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    if cancelled() {
        return Ok(None);
    }
    let surface = roads
        .surfaces
        .as_ref()
        .ok_or("Missing final road surfaces")?;
    if surface.chunks.len() > 8192 || surface.report["collision_lod"] != 0 {
        return Err("Unsupported road collision surface roster".into());
    }
    let mut contributors = Vec::new();
    let mut identities = std::collections::HashSet::new();
    for chunk in surface.chunks.iter().filter(|c| c.lod == 0) {
        if cancelled() {
            return Ok(None);
        }
        let families: Vec<_> = roads
            .families
            .iter()
            .filter(|f| chunk.road >= f.first_road && chunk.road - f.first_road < f.road_count)
            .collect();
        if chunk.road >= roads.roads.len()
            || families.len() != 1
            || families[0].hierarchy >= 2
            || !identities.insert((chunk.road, chunk.chunk_index))
        {
            return Err("Invalid, duplicate or dirt-road collision chunk".into());
        }
        chunk.geometry.mesh.validate()?;
        let origin = chunk.geometry.origin;
        contributors.push(decor_collider::Contributor {
            mesh: crate::height_mesh::Mesh {
                kind: "raw-triangle-mesh".into(),
                enabled: true,
                trigger: false,
                convex: false,
                cooking_options: 0,
                backfaces: false,
                vertices: chunk.geometry.mesh.vertices.clone(),
                triangles: chunk.geometry.mesh.triangles.clone(),
            },
            local_to_root: [
                [1., 0., 0., origin[0]],
                [0., 1., 0., origin[1]],
                [0., 0., 1., origin[2]],
                [0., 0., 0., 1.],
            ],
        });
    }
    generate(
        &layers.height,
        layers.height_resolution,
        layers.size as f32,
        &contributors,
        256_000_000,
        cancelled,
    )
}
/// All writes remain private until every contributor succeeds. Terrain is never
/// changed, including on cancellation, malformed geometry or exhausted work.
pub fn generate(
    terrain: &[i16],
    resolution: usize,
    size: f32,
    contributors: &[decor_collider::Contributor],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    if cancelled() {
        return Ok(None);
    }
    if !(2..=8193).contains(&resolution)
        || resolution.checked_mul(resolution) != Some(terrain.len())
        || terrain.iter().any(|h| !(0..=32766).contains(h))
        || !size.is_finite()
        || !(1. ..=10000.).contains(&size)
        || !(1..=256_000_000).contains(&maximum_work)
    {
        return Err("Invalid gameplay height inputs or work budget".into());
    }
    decor_collider::validate(contributors)?;
    let pose = decor_fit::Pose {
        candidate: 0,
        position: [0.; 3],
        rotation: [0., 0., 0., 1.],
        scale: [1.; 3],
    };
    let mut height = terrain.to_vec();
    let mut work = 0;
    for contributor in contributors {
        let Some(plan) = decor_collider::prepare(
            decor_collider::Input {
                heights: &height,
                resolution,
                size,
                initial: &[],
                contributors: std::slice::from_ref(contributor),
                pose: &pose,
            },
            &mut work,
            maximum_work,
            &cancelled,
        )
        .map_err(|e| format!("Gameplay height contributor failed: {e:?}"))?
        else {
            return Ok(None);
        };
        for (index, value) in plan.writes {
            height[index] = value;
        }
    }
    if cancelled() {
        return Ok(None);
    }
    let changed_samples = height.iter().zip(terrain).filter(|(a, b)| a != b).count();
    Ok(Some(Output {
        height,
        report: Report {
            engine: VERSION,
            contributors: contributors.len(),
            changed_samples,
            work,
            physics_cooking_validated: false,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decor_collider::Contributor, erosion::encode, height_mesh::Mesh};
    #[test]
    fn road_and_rail_height_results_combine_by_maximum_or_discard_on_failure() {
        let output = |height| Output {
            height,
            report: Report {
                engine: VERSION,
                contributors: 1,
                changed_samples: 1,
                work: 10,
                physics_cooking_validated: false,
            },
        };
        let base = vec![1; 3];
        let a = output(vec![2, 1, 4]);
        let b = output(vec![1, 3, 2]);
        let result = combine(&base, a, b, || false).unwrap().unwrap();
        assert_eq!(result.height, vec![2, 3, 4]);
        assert_eq!(result.report.changed_samples, 3);
        assert_eq!(result.report.contributors, 2);
        assert!(
            combine(&base, output(vec![2; 3]), output(vec![3; 3]), || true)
                .unwrap()
                .is_none()
        );
        assert!(combine(&base, output(vec![2; 3]), output(vec![0; 3]), || false).is_err());
        assert_eq!(base, vec![1; 3]);
    }
    fn surface(y: f32) -> Contributor {
        Contributor {
            mesh: Mesh {
                kind: "raw-triangle-mesh".into(),
                enabled: true,
                trigger: false,
                convex: false,
                cooking_options: 0,
                backfaces: false,
                vertices: vec![[-4., y, -4.], [4., y, -4.], [-4., y, 4.], [4., y, 4.]],
                triangles: vec![[0, 2, 1], [1, 2, 3]],
            },
            local_to_root: [
                [1., 0., 0., 0.],
                [0., 1., 0., 0.],
                [0., 0., 1., 0.],
                [0., 0., 0., 1.],
            ],
        }
    }
    #[test]
    fn roads_use_collision_lod_and_reject_dirt_or_duplicate_collision_chunks() {
        use crate::{
            native_layers::Layers,
            path_mesh::{Chunk, Template},
            road_network::{self, Destination},
        };
        let layers = Layers {
            size: 8,
            terrain_seed: 1,
            biome_seed: 1,
            height_resolution: 8,
            surface_resolution: 2,
            height: vec![encode(1.); 64],
            biome: vec![],
            splat: vec![],
            topology: vec![],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        let chunk = |lod, y| {
            let mesh = surface(y).mesh;
            road_network::SurfaceChunk {
                road: 0,
                lod,
                chunk_index: 0,
                geometry: Chunk {
                    first_repetition: 0,
                    repetitions: 1,
                    origin: [0.; 3],
                    work: 1,
                    mesh: Template {
                        vertices: mesh.vertices,
                        normals: vec![[0., 1., 0.]; 4],
                        tangents: vec![[1., 0., 0., 1.]; 4],
                        uv: vec![[0.; 2]; 4],
                        triangles: mesh.triangles,
                        center: [0., y, 0.],
                        extents: [4., 0., 4.],
                    },
                },
            }
        };
        let mut roads = road_network::Report {
            roads: vec![road_network::Road {
                from: Destination::Loop,
                to: Destination::Loop,
                points: vec![],
            }],
            families: vec![road_network::Family {
                infrastructure_type: 0,
                hierarchy: 0,
                seed: 1,
                first_road: 0,
                road_count: 1,
                socket_count: 0,
                unresolved: vec![],
                width: 8.,
                splat_channel: 0,
            }],
            surfaces: Some(road_network::SurfaceOutput {
                report: serde_json::json!({"collision_lod":0}),
                chunks: vec![chunk(0, 3.), chunk(1, 9.)],
            }),
            ..Default::default()
        };
        let height = super::roads(&layers, &roads, || false).unwrap().unwrap();
        assert_eq!(height.height, vec![encode(3.); 64]);
        assert_eq!(height.report.contributors, 1);
        roads.families[0].hierarchy = 2;
        assert!(super::roads(&layers, &roads, || false).is_err());
        roads.families[0].hierarchy = 0;
        roads.surfaces.as_mut().unwrap().chunks.push(chunk(0, 4.));
        assert!(super::roads(&layers, &roads, || false).is_err());
        assert_eq!(layers.height, vec![encode(1.); 64]);
    }
    #[test]
    fn separate_grid_raises_only_and_is_independent_of_contributor_order() {
        let mut terrain = vec![encode(1.); 64];
        terrain[0] = encode(5.);
        let before = terrain.clone();
        let a = generate(&terrain, 8, 8., &[surface(3.), surface(2.)], 10000, || {
            false
        })
        .unwrap()
        .unwrap();
        let b = generate(&terrain, 8, 8., &[surface(2.), surface(3.)], 10000, || {
            false
        })
        .unwrap()
        .unwrap();
        assert_eq!(terrain, before);
        let mut expected = vec![encode(3.); 64];
        expected[0] = encode(5.);
        assert_eq!(a.height, expected);
        assert_eq!(a.height, b.height);
        assert_eq!(a.report.changed_samples, 63);
    }
    #[test]
    fn cancellation_and_late_failure_publish_nothing() {
        let terrain = vec![encode(1.); 64];
        let work = generate(&terrain, 8, 8., &[surface(3.)], 10000, || false)
            .unwrap()
            .unwrap()
            .report
            .work;
        assert!(generate(&terrain, 8, 8., &[surface(3.), surface(4.)], work, || false).is_err());
        let first_calls = std::cell::Cell::new(0);
        generate(&terrain, 8, 8., &[surface(3.)], 10000, || {
            first_calls.set(first_calls.get() + 1);
            false
        })
        .unwrap()
        .unwrap();
        let count = std::cell::Cell::new(0);
        assert!(
            generate(&terrain, 8, 8., &[surface(3.), surface(4.)], 10000, || {
                count.set(count.get() + 1);
                count.get() == first_calls.get() + 2
            })
            .unwrap()
            .is_none()
        );
        let mut bad = surface(4.);
        bad.mesh.triangles[0][0] = 99;
        assert!(generate(&terrain, 8, 8., &[surface(3.), bad], 10000, || false).is_err());
        assert_eq!(terrain, vec![encode(1.); 64]);
    }
}
