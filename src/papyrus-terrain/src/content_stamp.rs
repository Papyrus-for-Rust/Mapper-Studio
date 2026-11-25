//! Apply a bounded, rotated crop from baked Rust terrain layers.
use crate::{content::MapContent, erosion, native_layers::Layers};

/// The crop is expressed in the source map's X/Z axes. Fade is measured
/// inward from the crop boundary, in metres.
pub struct Region {
    pub center: [f32; 2],
    pub size: [f32; 2],
    pub fade: f32,
    pub height: bool,
    pub splat: bool,
    pub biome: bool,
    pub topology_mask: u32,
    pub alpha: bool,
}

#[derive(Clone, Copy)]
pub struct Pose {
    pub position: [f32; 3],
    pub yaw: f32,
}

impl Pose {
    fn valid(self) -> bool {
        self.position
            .iter()
            .all(|value| value.is_finite() && value.abs() <= 65536.)
            && self.yaw.is_finite()
            && self.yaw.abs() <= 360000.
    }
}

struct Raster<'a> {
    bytes: &'a [u8],
    side: usize,
}

impl Raster<'_> {
    fn cell(&self, [u, v]: [f64; 2]) -> usize {
        let index = |uv: f64| ((uv * self.side as f64) as usize).min(self.side - 1);
        index(v) * self.side + index(u)
    }

    fn metres(&self, uv: [f64; 2]) -> Result<f64, String> {
        let [x, z] = uv.map(|unit| unit.clamp(0., 1.) * (self.side - 1) as f64);
        let [col, row] = [x as usize, z as usize];
        let read = |x, z| {
            let index = 2 * (z * self.side + x);
            let value = i16::from_le_bytes([self.bytes[index], self.bytes[index + 1]]);
            if !(0..=32766).contains(&value) {
                return Err("Invalid encoded height inside the custom monument crop".to_owned());
            }
            Ok(f64::from(erosion::decode(value)))
        };
        let next_col = (col + 1).min(self.side - 1);
        let next_row = (row + 1).min(self.side - 1);
        let fx = x - col as f64;
        let fz = z - row as f64;
        Ok(blend(
            blend(read(col, row)?, read(next_col, row)?, fx),
            blend(read(col, next_row)?, read(next_col, next_row)?, fx),
            fz,
        ))
    }
}

/// The raster payloads stay borrowed. Only the destination crop is buffered
/// before committing an edit.
pub struct Stamp<'a> {
    region: Region,
    source: Pose,
    world_size: f64,
    height: Option<Raster<'a>>,
    splat: Option<Raster<'a>>,
    biome: Option<(Raster<'a>, usize)>,
    topology: Option<Raster<'a>>,
    alpha: Option<Raster<'a>>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Report {
    pub height_vertices: usize,
    pub surface_cells: usize,
}

fn layer<'a>(map: &'a MapContent, name: &str) -> Result<&'a [u8], String> {
    map.layers
        .iter()
        .find(|layer| layer.name == name)
        .map(|layer| layer.data.as_slice())
        .ok_or_else(|| format!("Authoring map is missing its {name} layer"))
}

fn raster(bytes: &[u8], channels: usize, vertices: bool) -> Option<Raster<'_>> {
    let cells = bytes.len() / channels;
    let side = cells.isqrt();
    let intervals = side.checked_sub(usize::from(vertices))?;
    (bytes.len().is_multiple_of(channels)
        && side * side == cells
        && intervals.is_power_of_two()
        && (2..=4096).contains(&intervals))
    .then_some(Raster { bytes, side })
}

fn read_raster<'a>(
    map: &'a MapContent,
    name: &str,
    channels: usize,
    vertices: bool,
) -> Result<Raster<'a>, String> {
    raster(layer(map, name)?, channels, vertices)
        .ok_or_else(|| format!("Invalid authoring {name} raster dimensions"))
}

fn blend(a: f64, b: f64, weight: f64) -> f64 {
    a + (b - a) * weight
}

impl<'a> Stamp<'a> {
    pub fn new(map: &'a MapContent, region: Region, source: Pose) -> Result<Self, String> {
        let half = map.size as f32 * 0.5;
        if !source.valid()
            || map.size == 0
            || region
                .center
                .iter()
                .chain(&region.size)
                .any(|v| !v.is_finite())
            || region.size.iter().any(|&v| v <= 0.)
            || !(0. ..=region.size[0].min(region.size[1]) * 0.5).contains(&region.fade)
            || (0..2).any(|axis| region.center[axis].abs() + region.size[axis] * 0.5 > half)
            || !(region.height
                || region.splat
                || region.biome
                || region.topology_mask != 0
                || region.alpha)
        {
            return Err("Invalid custom monument terrain crop or source transform".into());
        }
        let height = region
            .height
            .then(|| read_raster(map, "terrain", 2, true))
            .transpose()?;
        let splat = region
            .splat
            .then(|| read_raster(map, "splat", 8, false))
            .transpose()?;
        let biome = region
            .biome
            .then(|| {
                let bytes = layer(map, "biome")?;
                [5, 4]
                    .into_iter()
                    .find_map(|channels| {
                        raster(bytes, channels, false).map(|raster| (raster, channels))
                    })
                    .ok_or_else(|| "Invalid authoring biome raster dimensions".to_owned())
            })
            .transpose()?;
        let topology = (region.topology_mask != 0)
            .then(|| read_raster(map, "topology", 4, false))
            .transpose()?;
        let alpha = region
            .alpha
            .then(|| read_raster(map, "alpha", 1, false))
            .transpose()?;
        Ok(Self {
            region,
            source,
            world_size: f64::from(map.size),
            height,
            splat,
            biome,
            topology,
            alpha,
        })
    }

    /// Apply one stamp to a private terrain variant before routing and object
    /// generation. Cancellation or an invalid sample leaves every layer intact.
    pub fn apply(
        &self,
        layers: &mut Layers,
        alpha: &mut [u8],
        destination: Pose,
        cancel: impl Fn() -> bool,
    ) -> Result<Option<Report>, String> {
        if cancel() {
            return Ok(None);
        }
        let (hr, sr) = crate::native_layers::dimensions(layers.size)?;
        if !destination.valid()
            || layers.height_resolution != hr
            || layers.surface_resolution != sr
            || layers.height.len() != hr * hr
            || layers.splat.len() != sr * sr * 8
            || layers.biome.len() != sr * sr * 5
            || layers.topology.len() != sr * sr
            || alpha.len() != sr * sr
        {
            return Err("Invalid custom monument terrain destination".into());
        }
        let transform = Transform::new(self, destination, layers.size)?;
        let height_grid = self
            .height
            .as_ref()
            .map(|_| Grid::new(&transform, hr, true));
        let surface_grid = (self.splat.is_some()
            || self.biome.is_some()
            || self.topology.is_some()
            || self.alpha.is_some())
        .then(|| Grid::new(&transform, sr, false));
        let visits = height_grid
            .iter()
            .chain(surface_grid.iter())
            .map(Grid::visits)
            .sum::<usize>();
        if visits > 2_000_000 {
            return Err("Custom monument terrain crop exceeds its sample budget".into());
        }
        if let (None, None, None, None, Some(raster), Some(grid)) = (
            &self.height,
            &self.splat,
            &self.biome,
            &self.topology,
            &self.alpha,
            &surface_grid,
        ) {
            return Self::apply_alpha(raster, grid, &transform, alpha, &cancel);
        }
        let mut heights = Vec::new();
        let mut surface = Vec::new();
        let height_offset = f64::from(destination.position[1]) - f64::from(self.source.position[1]);
        if let (Some(height), Some(grid)) = (&self.height, height_grid) {
            let complete = grid.scan(&transform, &cancel, |index, uv, weight| {
                let previous = layers.height[index];
                if !(0..=32766).contains(&previous) {
                    return Err("Invalid destination height inside the custom monument crop".into());
                }
                let metres = height.metres(uv)? + height_offset;
                let blended = blend(f64::from(erosion::decode(previous)), metres, weight);
                if !(-500. ..=500.).contains(&blended) {
                    return Err("Custom monument height is outside Rust's terrain range".into());
                }
                let value = erosion::encode(blended as f32);
                if value != previous {
                    heights.push((index, value));
                }
                Ok(())
            })?;
            if !complete {
                return Ok(None);
            }
        }
        if let Some(grid) = surface_grid {
            let complete = grid.scan(&transform, &cancel, |index, uv, weight| {
                let old_splat: [u8; 8] = layers.splat[index * 8..index * 8 + 8].try_into().unwrap();
                let old_biome: [u8; 5] = layers.biome[index * 5..index * 5 + 5].try_into().unwrap();
                let old_topology = layers.topology[index];
                let old_alpha = alpha[index];
                let splat = self.splat.as_ref().map_or(old_splat, |raster| {
                    weights(raster, uv, old_splat, 8, weight)
                });
                let biome = self.biome.as_ref().map_or(old_biome, |(raster, channels)| {
                    weights(raster, uv, old_biome, *channels, weight)
                });
                let mut topology = old_topology;
                let mut alpha = old_alpha;
                if weight >= 0.5 {
                    if let Some(raster) = &self.topology {
                        let at = raster.cell(uv) * 4;
                        let incoming =
                            u32::from_le_bytes(raster.bytes[at..at + 4].try_into().unwrap());
                        let mask = self.region.topology_mask;
                        topology = ((old_topology as u32 & !mask) | (incoming & mask)) as i32;
                    }
                    if let Some(raster) = &self.alpha {
                        alpha = raster.bytes[raster.cell(uv)];
                    }
                }
                if (splat, biome, topology, alpha)
                    != (old_splat, old_biome, old_topology, old_alpha)
                {
                    surface.push((index, splat, biome, topology, alpha));
                }
                Ok(())
            })?;
            if !complete {
                return Ok(None);
            }
        }
        if cancel() {
            return Ok(None);
        }
        let report = Report {
            height_vertices: heights.len(),
            surface_cells: surface.len(),
        };
        for (at, height) in heights {
            layers.height[at] = height;
        }
        for (index, splat, biome, topology, hole) in surface {
            layers.splat[index * 8..index * 8 + 8].copy_from_slice(&splat);
            layers.biome[index * 5..index * 5 + 5].copy_from_slice(&biome);
            layers.topology[index] = topology;
            alpha[index] = hole;
        }
        Ok(Some(report))
    }

    fn apply_alpha(
        raster: &Raster<'_>,
        grid: &Grid,
        transform: &Transform,
        alpha: &mut [u8],
        cancel: &impl Fn() -> bool,
    ) -> Result<Option<Report>, String> {
        let mut updates = Vec::new();
        let complete = grid.scan(transform, cancel, |index, uv, weight| {
            if weight >= 0.5 {
                let value = raster.bytes[raster.cell(uv)];
                if value != alpha[index] {
                    updates.push((index, value));
                }
            }
            Ok(())
        })?;
        if !complete || cancel() {
            return Ok(None);
        }
        let surface_cells = updates.len();
        for (index, value) in updates {
            alpha[index] = value;
        }
        Ok(Some(Report {
            height_vertices: 0,
            surface_cells,
        }))
    }
}

fn weights<const N: usize>(
    raster: &Raster<'_>,
    uv: [f64; 2],
    old: [u8; N],
    channels: usize,
    fade: f64,
) -> [u8; N] {
    let start = raster.cell(uv) * channels;
    std::array::from_fn(|i| {
        let incoming = if i < channels {
            raster.bytes[start + i]
        } else {
            0
        };
        blend(f64::from(old[i]), f64::from(incoming), fade).round() as u8
    })
}

struct Transform {
    source_pivot: [f64; 2],
    destination_pivot: [f64; 2],
    source_center: [f64; 2],
    half_extent: [f64; 2],
    fade: f64,
    sin: f64,
    cos: f64,
    source_size: f64,
    world_size: f64,
    bounds: [[f64; 2]; 2],
}

impl Transform {
    fn new(stamp: &Stamp<'_>, destination: Pose, world_size: u32) -> Result<Self, String> {
        let [sx, _, sz] = stamp.source.position.map(f64::from);
        let [dx, _, dz] = destination.position.map(f64::from);
        let [cx, cz] = stamp.region.center.map(f64::from);
        let [ex, ez] = stamp.region.size.map(|v| f64::from(v) * 0.5);
        let (sin, cos) = (f64::from(destination.yaw) - f64::from(stamp.source.yaw))
            .to_radians()
            .sin_cos();
        let center_x = cx - sx;
        let center_z = cz - sz;
        let world_center = [
            dx + cos * center_x + sin * center_z,
            dz - sin * center_x + cos * center_z,
        ];
        let world_extent = [
            cos.abs() * ex + sin.abs() * ez,
            sin.abs() * ex + cos.abs() * ez,
        ];
        let bounds = std::array::from_fn(|i| {
            [
                world_center[i] - world_extent[i],
                world_center[i] + world_extent[i],
            ]
        });
        let world_size = f64::from(world_size);
        if bounds
            .iter()
            .flatten()
            .any(|value| value.abs() > world_size * 0.5 + 1e-8)
        {
            return Err("The custom monument terrain crop extends outside the map".into());
        }
        Ok(Self {
            source_pivot: [sx, sz],
            destination_pivot: [dx, dz],
            source_center: [cx, cz],
            half_extent: [ex, ez],
            fade: f64::from(stamp.region.fade),
            sin,
            cos,
            source_size: stamp.world_size,
            world_size,
            bounds,
        })
    }

    fn sample(&self, x: f64, z: f64) -> Option<([f64; 2], f64)> {
        let x = x - self.destination_pivot[0];
        let z = z - self.destination_pivot[1];
        let u = self.source_pivot[0] + self.cos * x - self.sin * z;
        let v = self.source_pivot[1] + self.sin * x + self.cos * z;
        let edge_x = self.half_extent[0] - (u - self.source_center[0]).abs();
        let edge_z = self.half_extent[1] - (v - self.source_center[1]).abs();
        let edge = edge_x.min(edge_z);
        if edge < 0. {
            return None;
        }
        let weight = if self.fade == 0. {
            1.
        } else {
            (edge / self.fade).min(1.)
        };
        (weight > 0.).then_some((
            [u / self.source_size + 0.5, v / self.source_size + 0.5],
            weight,
        ))
    }
}

struct Grid {
    side: usize,
    start: [usize; 2],
    end: [usize; 2],
    step: f64,
    origin: f64,
}

impl Grid {
    fn new(transform: &Transform, side: usize, vertices: bool) -> Self {
        let step = transform.world_size / (side - usize::from(vertices)) as f64;
        let origin = -transform.world_size * 0.5 + if vertices { 0. } else { step * 0.5 };
        let index = |coord: f64| ((coord - origin) / step).clamp(0., (side - 1) as f64);
        let start = transform
            .bounds
            .map(|range| index(range[0]).floor() as usize);
        let end = transform
            .bounds
            .map(|range| index(range[1]).ceil() as usize);
        Self {
            side,
            start,
            end,
            step,
            origin,
        }
    }

    fn visits(&self) -> usize {
        (self.end[0] - self.start[0] + 1) * (self.end[1] - self.start[1] + 1)
    }

    fn scan(
        &self,
        transform: &Transform,
        cancel: &impl Fn() -> bool,
        mut sample: impl FnMut(usize, [f64; 2], f64) -> Result<(), String>,
    ) -> Result<bool, String> {
        for z in self.start[1]..=self.end[1] {
            if cancel() {
                return Ok(false);
            }
            let world_z = self.origin + z as f64 * self.step;
            for x in self.start[0]..=self.end[0] {
                let world_x = self.origin + x as f64 * self.step;
                if let Some((uv, weight)) = transform.sample(world_x, world_z) {
                    sample(z * self.side + x, uv, weight)?;
                }
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::Layer;
    use std::cell::Cell;

    fn source() -> MapContent {
        let terrain: Vec<u8> = (0..25)
            .flat_map(|_| erosion::encode(30.).to_le_bytes())
            .collect();
        MapContent {
            size: 1000,
            prefabs: vec![],
            paths: vec![],
            unknown_fields: 0,
            layers: [
                ("terrain", terrain),
                ("splat", [0, 0, 255, 0, 0, 0, 0, 0].repeat(16)),
                ("biome", [255, 0, 0, 0].repeat(16)),
                ("topology", 5_u32.to_le_bytes().repeat(16)),
                ("alpha", vec![0; 16]),
            ]
            .into_iter()
            .map(|(name, data)| Layer {
                name: name.into(),
                data,
            })
            .collect(),
        }
    }

    fn destination() -> (Layers, Vec<u8>) {
        let (hr, sr) = crate::native_layers::dimensions(1000).unwrap();
        let cells = sr * sr;
        (
            Layers {
                size: 1000,
                terrain_seed: 7,
                biome_seed: 11,
                height_resolution: hr,
                surface_resolution: sr,
                height: vec![erosion::encode(20.); hr * hr],
                splat: [255, 0, 0, 0, 0, 0, 0, 0].repeat(cells),
                biome: [0, 0, 0, 0, 255].repeat(cells),
                topology: vec![10; cells],
                normalized_biome_cells: 0,
                normalized_splat_cells: 0,
            },
            vec![255; cells],
        )
    }

    fn region() -> Region {
        Region {
            center: [60., -30.],
            size: [60., 40.],
            fade: 10.,
            height: true,
            splat: true,
            biome: true,
            topology_mask: 3,
            alpha: true,
        }
    }
    const SOURCE: Pose = Pose {
        position: [50., 10., -30.],
        yaw: 0.,
    };
    const TARGET: Pose = Pose {
        position: [100., 40., 0.],
        yaw: 90.,
    };

    #[test]
    fn rotated_crop_applies_every_layer_and_preserves_unselected_bits() {
        let map = source();
        let stamp = Stamp::new(&map, region(), SOURCE).unwrap();
        let (mut layers, mut alpha) = destination();
        let report = stamp
            .apply(&mut layers, &mut alpha, TARGET, || false)
            .unwrap()
            .unwrap();
        assert!(report.height_vertices > 0 && report.surface_cells > 0);
        let h = layers.height_resolution;
        let s = layers.surface_resolution;
        let cell = |x: f32, z: f32| {
            let index = |coord: f32| ((coord / 1000. + 0.5) * s as f32) as usize;
            index(z) * s + index(x)
        };
        let vertex = |x: f32, z: f32| {
            let index = |coord: f32| ((coord / 1000. + 0.5) * (h - 1) as f32).round() as usize;
            index(z) * h + index(x)
        };
        let center = cell(100., -10.);
        assert!((erosion::decode(layers.height[vertex(100., -10.)]) - 60.).abs() < 0.06);
        assert_eq!(
            &layers.splat[center * 8..center * 8 + 8],
            &[0, 0, 255, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            &layers.biome[center * 5..center * 5 + 5],
            &[255, 0, 0, 0, 0]
        );
        assert_eq!(layers.topology[center], 9);
        assert_eq!(alpha[center], 0);
        let edge = cell(118., -10.);
        assert!((1..128).contains(&layers.splat[edge * 8 + 2]));
        assert_eq!(layers.topology[edge], 10);
        assert_eq!(alpha[edge], 255);
        let outside = cell(125., -10.);
        assert_eq!(layers.splat[outside * 8], 255);
        assert_eq!(layers.height[vertex(125., -10.)], erosion::encode(20.));
    }

    #[test]
    fn oblique_alpha_only_cutout_roundtrips_through_playable_map_layers() {
        use crate::map_container::{World, write_v10};

        let mut map = source();
        map.layers.retain(|layer| layer.name == "alpha");
        map.layers[0].data.fill(255);
        map.layers[0].data[5] = 117;
        map.layers[0].data[6] = 0;
        let stamp = Stamp::new(
            &map,
            Region {
                center: [0., -100.],
                size: [180., 120.],
                fade: 12.,
                height: false,
                splat: false,
                biome: false,
                topology_mask: 0,
                alpha: true,
            },
            SOURCE,
        )
        .unwrap();
        let (base, mut base_alpha) = destination();
        base_alpha[0] = 0;
        let mut layers = base.clone();
        let mut alpha = base_alpha.clone();
        let pose = Pose { yaw: 37., ..TARGET };
        let calls = Cell::new(0);
        assert!(
            stamp
                .apply(&mut layers, &mut alpha, pose, || {
                    calls.set(calls.get() + 1);
                    calls.get() == 9
                })
                .unwrap()
                .is_none()
        );
        assert_eq!(alpha, base_alpha);

        let report = stamp
            .apply(&mut layers, &mut alpha, pose, || false)
            .unwrap()
            .unwrap();
        assert_eq!(report.height_vertices, 0);
        assert!(report.surface_cells > 0);
        assert_eq!(layers, base);
        assert_eq!(alpha[0], 0);
        let (sin, cos) = pose.yaw.to_radians().sin_cos();
        let cell_for = |source_x: f32, source_z: f32| {
            let x = source_x - SOURCE.position[0];
            let z = source_z - SOURCE.position[2];
            let side = layers.surface_resolution;
            let index = |coord| ((coord / 1000. + 0.5) * side as f32) as usize;
            let col = index(pose.position[0] + cos * x + sin * z);
            let row = index(pose.position[2] - sin * x + cos * z);
            row * side + col
        };
        assert_eq!(alpha[cell_for(-55., -100.)], 117);
        assert_eq!(alpha[cell_for(55., -100.)], 0);
        assert_eq!(alpha[cell_for(110., -100.)], 255);

        let mut bytes = Vec::new();
        assert!(
            write_v10(
                World {
                    layers: &layers,
                    height: &layers.height,
                    water: None,
                    alpha: &alpha,
                    prefabs: &[],
                    paths: &[],
                },
                1,
                &mut bytes,
                || false,
            )
            .unwrap()
            .is_some()
        );
        let exported = crate::content::read_map(&bytes, || false).unwrap();
        assert_eq!(layer(&exported, "alpha").unwrap(), alpha);
    }

    #[test]
    fn cancellation_and_invalid_rotated_stamps_leave_terrain_untouched() {
        let map = source();
        let stamp = Stamp::new(&map, region(), SOURCE).unwrap();
        let (mut layers, mut alpha) = destination();
        let (original, original_alpha) = (layers.clone(), alpha.clone());
        let calls = Cell::new(0);
        assert!(
            stamp
                .apply(&mut layers, &mut alpha, TARGET, || {
                    calls.set(calls.get() + 1);
                    calls.get() == 9
                })
                .unwrap()
                .is_none()
        );
        let edge = Pose {
            position: [499., 20., 0.],
            yaw: 37.,
        };
        assert!(
            stamp
                .apply(&mut layers, &mut alpha, edge, || false)
                .is_err()
        );
        let high = Pose {
            position: [100., 600., 0.],
            yaw: 37.,
        };
        assert!(
            stamp
                .apply(&mut layers, &mut alpha, high, || false)
                .is_err()
        );
        assert_eq!(layers, original);
        assert_eq!(alpha, original_alpha);
        let non_right_angle = Pose {
            position: [100., 40., 0.],
            yaw: 37.,
        };
        let report = stamp
            .apply(&mut layers, &mut alpha, non_right_angle, || false)
            .unwrap()
            .unwrap();
        assert!(report.height_vertices > 0 && report.surface_cells > 0);
        let mut bad = source();
        bad.layers.retain(|layer| layer.name != "alpha");
        assert!(
            Stamp::new(&bad, region(), SOURCE)
                .err()
                .unwrap()
                .contains("alpha")
        );
    }
}
