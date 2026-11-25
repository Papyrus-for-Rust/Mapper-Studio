//! Safe, independent terrain generation. No game libraries or assets are needed.
use std::thread;

pub mod alpha_composition;
pub mod authored_spline;
pub mod bank_topology;
pub mod boat_navigation;
pub mod clutter_topology;
pub mod cliff_topology;
pub mod uniform_cliffs;
pub mod coastal_cliffs;
pub mod content;
pub mod content_stamp;
pub mod decor;
pub mod decor_collider;
pub mod decor_fit;
pub mod decor_height;
pub mod decor_live;
pub mod decor_material;
pub mod decor_stream;
pub mod decor_topology;
pub mod erosion;
pub mod erosion_bounds;
pub mod erosion_grid;
pub mod erosion_pipeline;
pub mod erosion_splat;
pub mod environment;
pub mod dungeon_base;
pub mod tunnel_grid;
pub mod tunnel_layout;
pub mod tunnel_depth;
pub mod tunnel_links;
pub mod tunnel_stations;
pub mod tunnel_entrances;
pub mod tunnel_network;
pub mod tunnel_portals;
pub mod tunnel_spawn;
pub mod tunnel_scene;
pub mod prefab_terrain;
pub mod prefab_terrain_effects;
pub mod export;
pub mod footprints;
pub mod height_mesh;
pub mod landscape;
pub mod monument_bounds;
pub mod monument_composition;
pub mod monument_eligibility;
pub mod monument_stage;
pub mod monuments;
pub mod native_layers;
pub mod ocean;
pub mod offshore_search;
pub mod offshore_tiers;
pub mod shore_field;
pub mod shore_mainland;
pub mod path_geometry;
pub mod path_resources;
pub mod path_mesh;
pub mod paths;
pub mod placement;
pub mod placement_blockers;
pub mod powerline_costmap;
pub mod powerline_layout;
pub mod powerline_sequence;
pub mod preparation;
pub mod rail_approaches;
pub mod rail_bed;
pub mod rail_bed_fit;
pub mod rail_corridor;
pub mod rail_costmap;
pub mod road_land_access;
pub mod rail_grade;
pub mod rail_grade_fit;
pub mod rail_ring;
pub mod rail_track_graph;
pub mod registry;
pub mod river_carve;
pub mod river_layout;
pub mod river_pipeline;
pub mod river_surface;
pub mod river_water;
pub mod road_network;
pub mod road_reservations;
pub mod road_ring;
pub mod road_topology;
pub mod routing;
pub mod spawn;
pub mod stamps;
pub mod terrain_normal;
pub mod terrain_slope;
pub mod topology_paint;
pub mod topology_query;
pub mod water;
pub mod water_map_query;
pub mod water_mesh;
pub mod world;

pub const MAX_RESOLUTION: usize = 513;

#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub seed: i32,
    pub resolution: usize,
    pub threads: usize,
    /// Arid, temperate, tundra and arctic percentages, summing to 100.
    pub biomes: [u32; 4],
}

impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if self.seed < 1 {
            return Err("seed must be a positive 32-bit integer".into());
        }
        if !(33..=MAX_RESOLUTION).contains(&self.resolution)
            || !(self.resolution - 1).is_power_of_two()
        {
            return Err("resolution must be 2^n + 1, between 33 and 513".into());
        }
        if !(1..=1024).contains(&self.threads) {
            return Err("threads must be between 1 and 1024".into());
        }
        if self.biomes.iter().any(|&p| p > 100) || self.biomes.iter().sum::<u32>() != 100 {
            return Err("biome percentages must be between 0 and 100 and total 100".into());
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub struct Field {
    pub heights: Vec<f32>,
    /// 0 ocean, 1 arid, 2 temperate, 3 tundra, 4 arctic.
    pub biomes: Vec<u8>,
}

pub fn generate(settings: Settings) -> Result<Field, String> {
    settings.validate()?;
    let resolution = settings.resolution;
    let mut heights = vec![0.0; resolution * resolution];
    // Each worker owns disjoint rows. Math and biome ordering do not depend on scheduling.
    let workers = settings.threads.min(resolution);
    let rows_per_worker = resolution.div_ceil(workers);
    thread::scope(|scope| {
        for (chunk_index, rows) in heights.chunks_mut(rows_per_worker * resolution).enumerate() {
            scope.spawn(move || {
                let first_z = chunk_index * rows_per_worker;
                for (index, height) in rows.iter_mut().enumerate() {
                    let x = index % resolution;
                    let z = first_z + index / resolution;
                    let nx = x as f64 / (resolution - 1) as f64 * 2.0 - 1.0;
                    let nz = z as f64 / (resolution - 1) as f64 * 2.0 - 1.0;
                    let distance = (nx * nx + nz * nz).sqrt();
                    let falloff = smooth_step(1.0, 0.68, distance);
                    let continental = fbm(nx * 1.35, nz * 1.35, settings.seed, 5, 0.52);
                    let detail = fbm(nx * 5.5, nz * 5.5, settings.seed ^ 0x51F15E, 4, 0.46);
                    let ridges = 1.0
                        - (2.0 * fbm(nx * 3.1, nz * 3.1, settings.seed ^ 0x2C9277, 4, 0.5) - 1.0)
                            .abs();
                    *height = ((continental * 0.58 + detail * 0.12 + ridges * 0.30) * falloff
                        - 0.16)
                        .clamp(0.0, 1.0) as f32;
                }
            });
        }
    });
    let biomes = generate_biomes(settings, &heights);
    Ok(Field { heights, biomes })
}

fn generate_biomes(settings: Settings, heights: &[f32]) -> Vec<u8> {
    let resolution = settings.resolution;
    let mut biomes = vec![0; heights.len()];
    let mut land = Vec::with_capacity(heights.len());
    for (index, &height) in heights.iter().enumerate() {
        if height <= 0.08_f32 {
            continue;
        }
        let x = index % resolution;
        let z = index / resolution;
        let latitude = z as f64 / (resolution - 1) as f64;
        let climate = latitude * 0.62
            + noise(x as f64 / 29.0, z as f64 / 29.0, settings.seed ^ 0xB10B1E) * 0.38;
        land.push((index, climate));
    }
    land.sort_unstable_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
    let mut counts = settings
        .biomes
        .map(|percentage| land.len() * percentage as usize / 100);
    let remainder = land.len() - counts.iter().sum::<usize>();
    for index in 0..remainder {
        counts[index % 4] += 1;
    }
    let mut cursor = 0;
    for (biome, count) in counts.into_iter().enumerate() {
        for &(index, _) in &land[cursor..cursor + count] {
            biomes[index] = (biome + 1) as u8;
        }
        cursor += count;
    }
    biomes
}

fn smooth_step(edge0: f64, edge1: f64, x: f64) -> f64 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn fbm(x: f64, y: f64, seed: i32, octaves: i32, persistence: f64) -> f64 {
    let (mut value, mut amplitude, mut frequency, mut total) = (0.0, 1.0, 1.0, 0.0);
    for octave in 0..octaves {
        value += noise(
            x * frequency,
            y * frequency,
            seed.wrapping_add(octave * 1013),
        ) * amplitude;
        total += amplitude;
        amplitude *= persistence;
        frequency *= 2.0;
    }
    value / total
}

pub(crate) fn noise(x: f64, y: f64, seed: i32) -> f64 {
    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let mut tx = x - x0 as f64;
    let mut ty = y - y0 as f64;
    tx = tx * tx * (3.0 - 2.0 * tx);
    ty = ty * ty * (3.0 - 2.0 * ty);
    let a = hash(x0, y0, seed);
    let b = hash(x0.wrapping_add(1), y0, seed);
    let c = hash(x0, y0.wrapping_add(1), seed);
    let d = hash(x0.wrapping_add(1), y0.wrapping_add(1), seed);
    lerp(lerp(a, b, tx), lerp(c, d, tx), ty)
}

fn hash(x: i32, y: i32, seed: i32) -> f64 {
    let mut value = x
        .wrapping_mul(374761393)
        .wrapping_add(y.wrapping_mul(668265263)) as u32
        ^ (seed as u32).wrapping_mul(2246822519);
    value = (value ^ (value >> 13)).wrapping_mul(1274126177);
    (value ^ (value >> 16)) as f64 / u32::MAX as f64
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings {
            seed: 12345,
            resolution: 65,
            threads: 1,
            biomes: [10, 20, 30, 40],
        }
    }

    #[test]
    fn worker_counts_and_overflowing_seed_are_deterministic() {
        for seed in [1, 12345, i32::MAX] {
            let one = generate(Settings { seed, ..settings() }).unwrap();
            let many = generate(Settings {
                seed,
                threads: 4,
                ..settings()
            })
            .unwrap();
            assert_eq!(one, many);
            assert!(
                one.heights
                    .iter()
                    .all(|h| h.is_finite() && (0.0..=1.0).contains(h))
            );
            assert!(one.heights.iter().any(|&h| h > 0.08));
            assert_eq!(one.heights[0], 0.0);
        }
    }

    #[test]
    fn biome_counts_and_ocean_follow_the_field() {
        let field = generate(settings()).unwrap();
        let land_count = field.biomes.iter().filter(|&&b| b != 0).count();
        for (index, &height) in field.heights.iter().enumerate() {
            assert_eq!(field.biomes[index] == 0, height <= 0.08);
        }
        for (index, percentage) in settings().biomes.into_iter().enumerate() {
            let count = field
                .biomes
                .iter()
                .filter(|&&b| b == (index + 1) as u8)
                .count();
            assert!((count as f64 - land_count as f64 * percentage as f64 / 100.0).abs() <= 1.0);
        }
    }

    #[test]
    fn invalid_requests_are_rejected_before_allocation() {
        for resolution in [0, 32, 64, 514, usize::MAX] {
            assert!(
                generate(Settings {
                    resolution,
                    ..settings()
                })
                .is_err()
            );
        }
        assert!(
            generate(Settings {
                seed: 0,
                ..settings()
            })
            .is_err()
        );
        assert!(
            generate(Settings {
                threads: 0,
                ..settings()
            })
            .is_err()
        );
        assert!(
            generate(Settings {
                biomes: [u32::MAX; 4],
                ..settings()
            })
            .is_err()
        );
    }
}

pub mod object_scene;

pub mod prefab_records;

pub mod gameplay_height;
pub mod height_reservations;
pub mod map_container;
pub mod rail_branching;
pub mod rail_reservations;
pub mod rail_scene;
pub mod rail_siding;
pub mod road_corridor_fit;
pub mod road_costmap;
pub mod road_ring_join;
pub mod road_smoothing;
pub mod road_strip;
pub mod source_road_junctions;
pub mod source_road_layout;
pub mod source_road_ring;
pub mod source_road_ring_cycle;
pub mod source_road_route;

pub mod monument_spacing;
pub mod path_surface;
pub mod path_terrain;
pub mod placement_map;
pub mod rail_texture;
pub mod rail_topology;
pub mod road_bed;
pub mod road_clearance;
pub mod road_entrance_contour;
pub mod road_grade;
pub mod road_grade_terrain;
pub mod road_mesh_audit;
pub mod road_paint;
pub mod road_surface_contact;
pub mod road_terrain;
pub mod road_water;
pub mod roadside_monuments;
pub mod roadside_decor;
pub mod roadside_live;
pub mod water_treatment_clearance;
pub mod powerline_wires;
pub mod roadside_selection;
pub mod source_stamp_raster;
pub mod source_texture;
pub mod terrain_graph;
