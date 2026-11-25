//! Deterministic path geometry from the independent C# baseline.
//! River paths retain that baseline's separate coarse-field trace; this does not
//! claim alignment with carved channels or official game's path components.
use crate::{Field, noise, water::Rivers};
use std::collections::HashSet;

pub const MAX_PATHS: usize = 4096;
pub const MAX_TOTAL_NODES: usize = 1_048_576;

#[derive(Clone, Copy, Debug)]
pub struct Roads {
    pub enabled: bool,
    pub ring: bool,
    pub branches: usize,
    pub width: f64,
}

impl Roads {
    pub fn validate(&self) -> Result<(), String> {
        if self.branches > MAX_PATHS - 1
            || !self.width.is_finite()
            || self.width <= 0.0
            || self.width > f32::MAX as f64
        {
            return Err("invalid road settings: at most 4095 branches and a finite positive f32 width are supported".into());
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub struct Point {
    pub x: f32,
    pub z: f32,
}

#[derive(Debug, PartialEq)]
pub struct Path {
    /// 0 river, 1 road, 2 ring road.
    pub kind: u32,
    pub width: f32,
    pub points: Vec<Point>,
}

pub fn generate(
    source: &Field,
    resolution: usize,
    size: usize,
    seed: i32,
    rivers: Rivers,
    roads: Roads,
) -> Result<Vec<Path>, String> {
    roads.validate()?;
    if !(33..=513).contains(&resolution)
        || !(resolution - 1).is_power_of_two()
        || source.heights.len() != resolution * resolution
        || !(1000..=6000).contains(&size)
        || seed < 1
    {
        return Err("invalid path source".into());
    }
    rivers.validate(size)?;
    if source
        .heights
        .iter()
        .any(|h| !h.is_finite() || !(0.0..=1.0).contains(h))
    {
        return Err("invalid path height field".into());
    }
    let mut paths = Vec::new();
    if rivers.count > 0 {
        generate_rivers(source, resolution, size, seed, rivers, &mut paths)?;
    }
    if roads.enabled {
        generate_roads(source, resolution, size, seed, roads, &mut paths)?;
    }
    Ok(paths)
}

fn append_path(paths: &mut Vec<Path>, path: Path) -> Result<(), String> {
    let nodes = paths.iter().map(|p| p.points.len()).sum::<usize>();
    if paths.len() >= MAX_PATHS || path.points.len() > MAX_TOTAL_NODES - nodes {
        return Err("path output exceeds the supported path/node budget".into());
    }
    paths.push(path);
    Ok(())
}

fn generate_rivers(
    source: &Field,
    resolution: usize,
    size: usize,
    seed: i32,
    settings: Rivers,
    paths: &mut Vec<Path>,
) -> Result<(), String> {
    let heights = &source.heights;
    let mut candidates: Vec<(usize, f64)> = heights
        .iter()
        .enumerate()
        .filter(|(_, h)| **h > 0.42)
        .map(|(i, &h)| {
            (
                i,
                h as f64
                    + noise(
                        (i % resolution) as f64,
                        (i / resolution) as f64,
                        seed ^ 0x715E,
                    ) * 0.08,
            )
        })
        .collect();
    // LINQ OrderByDescending is stable: equal scores retain ascending cell index.
    candidates.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let maximum = if settings.maximum_length == 0 {
        resolution * 2
    } else {
        ((settings.maximum_length as f64 / size as f64 * resolution as f64) as usize).max(8)
    }
    .min(heights.len());
    let mut used = Vec::new();
    for (source, _) in candidates {
        if used.len() >= settings.count {
            break;
        }
        if used
            .iter()
            .any(|&other| cell_distance(source, other, resolution) < resolution as f64 / 8.0)
        {
            continue;
        }
        let points = trace_downhill(source, heights, resolution, maximum, seed);
        if points.len() < 8 {
            continue;
        }
        used.push(source);
        append_path(
            paths,
            Path {
                kind: 0,
                width: settings.width as f32,
                points: points
                    .into_iter()
                    .map(|i| world_point(i, resolution, size))
                    .collect(),
            },
        )?;
    }
    Ok(())
}

fn trace_downhill(
    source: usize,
    heights: &[f32],
    resolution: usize,
    maximum: usize,
    seed: i32,
) -> Vec<usize> {
    let mut result = vec![source];
    let mut seen = HashSet::from([source]);
    let mut current = source;
    for step in 0..maximum {
        if heights[current] <= 0.08 {
            break;
        }
        let x = (current % resolution) as i32;
        let z = (current / resolution) as i32;
        let mut next = None;
        let mut score = f64::MAX;
        for dz in -1..=1 {
            for dx in -1..=1 {
                if dx == 0 && dz == 0 {
                    continue;
                }
                let nx = x + dx;
                let nz = z + dz;
                if nx < 0 || nz < 0 || nx >= resolution as i32 || nz >= resolution as i32 {
                    continue;
                }
                let candidate = nz as usize * resolution + nx as usize;
                if seen.contains(&candidate) {
                    continue;
                }
                let cx = nx as f64 - resolution as f64 / 2.0;
                let cz = nz as f64 - resolution as f64 / 2.0;
                let candidate_score = heights[candidate] as f64
                    + noise(nx as f64, nz as f64, seed.wrapping_add(step as i32)) * 0.012
                    + (cx * cx + cz * cz).sqrt() * -0.00001;
                if candidate_score < score {
                    score = candidate_score;
                    next = Some(candidate);
                }
            }
        }
        let Some(next) = next else {
            break;
        };
        if heights[next] > heights[current] + 0.025 {
            break;
        }
        current = next;
        seen.insert(current);
        result.push(current);
    }
    result
}

fn generate_roads(
    source: &Field,
    resolution: usize,
    size: usize,
    seed: i32,
    settings: Roads,
    paths: &mut Vec<Path>,
) -> Result<(), String> {
    let radius = resolution as f64 * 0.30;
    let center = (resolution - 1) as f64 / 2.0;
    if settings.ring {
        let points = (0..=128)
            .map(|step| {
                let angle = step as f64 / 128.0 * std::f64::consts::PI * 2.0;
                let x = (center + angle.cos() * radius).round_ties_even() as i32;
                let z = (center + angle.sin() * radius).round_ties_even() as i32;
                world_point(
                    nearest_land(x, z, &source.heights, resolution),
                    resolution,
                    size,
                )
            })
            .collect();
        append_path(
            paths,
            Path {
                kind: 2,
                width: settings.width as f32,
                points,
            },
        )?;
    }
    for branch in 0..settings.branches {
        let angle = (branch as f64 + noise(branch as f64, seed as f64, seed) * 0.45)
            / settings.branches.max(1) as f64
            * std::f64::consts::PI
            * 2.0;
        let points = (0..=32)
            .map(|step| {
                let distance = radius * (0.18 + 0.82 * step as f64 / 32.0);
                let x = (center + angle.cos() * distance).round_ties_even() as i32;
                let z = (center + angle.sin() * distance).round_ties_even() as i32;
                world_point(
                    nearest_land(x, z, &source.heights, resolution),
                    resolution,
                    size,
                )
            })
            .collect();
        append_path(
            paths,
            Path {
                kind: 1,
                width: settings.width as f32,
                points,
            },
        )?;
    }
    Ok(())
}

fn nearest_land(x: i32, z: i32, heights: &[f32], resolution: usize) -> usize {
    let x = x.clamp(0, resolution as i32 - 1);
    let z = z.clamp(0, resolution as i32 - 1);
    let best = z as usize * resolution + x as usize;
    if heights[best] > 0.08 {
        return best;
    }
    for radius in 1..(resolution / 6) as i32 {
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                let nx = x + dx;
                let nz = z + dz;
                if nx >= 0 && nz >= 0 && nx < resolution as i32 && nz < resolution as i32 {
                    let index = nz as usize * resolution + nx as usize;
                    if heights[index] > 0.08 {
                        return index;
                    }
                }
            }
        }
    }
    best
}

fn world_point(index: usize, resolution: usize, size: usize) -> Point {
    Point {
        x: ((index % resolution) as f32 / (resolution - 1) as f32 - 0.5) * size as f32,
        z: ((index / resolution) as f32 / (resolution - 1) as f32 - 0.5) * size as f32,
    }
}

fn cell_distance(a: usize, b: usize, resolution: usize) -> f64 {
    let dx = (a % resolution) as f64 - (b % resolution) as f64;
    let dz = (a / resolution) as f64 - (b / resolution) as f64;
    (dx * dx + dz * dz).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(height: f32) -> Field {
        Field {
            heights: vec![height; 65 * 65],
            biomes: vec![0; 65 * 65],
        }
    }
    fn rivers() -> Rivers {
        Rivers {
            count: 0,
            width: 8.0,
            maximum_length: 0,
        }
    }
    fn roads() -> Roads {
        Roads {
            enabled: true,
            ring: true,
            branches: 3,
            width: 12.125,
        }
    }

    #[test]
    fn roads_are_deterministic_closed_and_bounded_even_at_maximum_seed() {
        let field = source(0.25);
        let one = generate(&field, 65, 6000, i32::MAX, rivers(), roads()).unwrap();
        let repeat = generate(&field, 65, 6000, i32::MAX, rivers(), roads()).unwrap();
        assert_eq!(one, repeat);
        assert_eq!(one.len(), 4);
        assert_eq!(one[0].kind, 2);
        assert_eq!(one[0].points.len(), 129);
        assert_eq!(one[0].points.first(), one[0].points.last());
        assert!(one[1..].iter().all(|p| p.kind == 1 && p.points.len() == 33));
        assert!(one.iter().all(|p| {
            p.width == 12.125
                && p.points
                    .iter()
                    .all(|v| v.x.abs() <= 3000.0 && v.z.abs() <= 3000.0)
        }));
    }

    #[test]
    fn empty_and_all_ocean_fields_handle_road_toggles() {
        let field = source(0.0);
        assert!(
            generate(
                &field,
                65,
                1000,
                1,
                rivers(),
                Roads {
                    enabled: false,
                    ..roads()
                }
            )
            .unwrap()
            .is_empty()
        );
        assert!(
            generate(
                &field,
                65,
                1000,
                1,
                rivers(),
                Roads {
                    ring: false,
                    branches: 0,
                    ..roads()
                }
            )
            .unwrap()
            .is_empty()
        );
        let ring = generate(
            &field,
            65,
            1000,
            1,
            rivers(),
            Roads {
                branches: 0,
                ..roads()
            },
        )
        .unwrap();
        assert_eq!(ring.len(), 1);
        assert_eq!(ring[0].points.first(), ring[0].points.last());
    }

    #[test]
    fn invalid_sources_settings_and_output_budgets_are_rejected() {
        assert!(generate(&source(f32::NAN), 65, 1000, 1, rivers(), roads()).is_err());
        for settings in [
            Roads {
                branches: MAX_PATHS,
                ..roads()
            },
            Roads {
                width: f64::NAN,
                ..roads()
            },
            Roads {
                width: 0.0,
                ..roads()
            },
        ] {
            assert!(generate(&source(0.25), 65, 1000, 1, rivers(), settings).is_err());
        }
        let mut full: Vec<Path> = (0..MAX_PATHS)
            .map(|_| Path {
                kind: 1,
                width: 1.0,
                points: vec![],
            })
            .collect();
        assert!(
            append_path(
                &mut full,
                Path {
                    kind: 1,
                    width: 1.0,
                    points: vec![]
                }
            )
            .is_err()
        );
    }
}
