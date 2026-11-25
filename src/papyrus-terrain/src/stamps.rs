//! Ordered prefab terrain composition for the independent generator.
use crate::{
    erosion::{decode, encode},
    monuments::{Entry, Placement, Reader},
};
use std::{
    collections::HashSet,
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct Stamp {
    pub entry: usize,
    pub width: usize,
    pub height: usize,
    pub size: [f32; 3],
    pub offset: [f32; 3],
    pub splat_mask: i32,
    pub topology_mask: i32,
    pub flags: u32,
    /// Blend, splat0, splat1, height, alpha, topology; relative to the cache root.
    pub textures: [String; 6],
}

#[derive(Clone, Debug, Default)]
pub struct Request {
    pub root: String,
    pub stamps: Vec<Stamp>,
}

impl Request {
    pub fn validate(&self, entries: &[Entry]) -> Result<(), String> {
        if self.root.len() > 4096
            || self.root.contains('\0')
            || self.stamps.len() > 65536
            || (!self.stamps.is_empty() && !Path::new(&self.root).is_absolute())
        {
            return Err("invalid stamp cache root or count".into());
        }
        let mut seen = HashSet::new();
        for stamp in &self.stamps {
            if stamp.entry >= entries.len()
                || entries[stamp.entry].resolved != stamp.entry
                || !seen.insert(stamp.entry)
                || !(1..=4096).contains(&stamp.width)
                || !(1..=4096).contains(&stamp.height)
                || stamp.flags > 15
                || stamp
                    .size
                    .iter()
                    .chain(&stamp.offset)
                    .any(|v| !v.is_finite() || v.abs() > 1_000_000.0)
                || stamp.size[0] <= 0.0
                || stamp.size[2] <= 0.0
            {
                return Err("invalid stamp descriptor".into());
            }
            for path in &stamp.textures {
                if path.len() > 4096
                    || path.contains('\0')
                    || (!path.is_empty()
                        && Path::new(path)
                            .components()
                            .any(|c| !matches!(c, Component::Normal(_))))
                {
                    return Err("stamp texture must be a relative cache path".into());
                }
            }
        }
        Ok(())
    }

    pub(crate) fn read(reader: &mut Reader<'_>) -> Result<Self, String> {
        if reader.word()? != u32::from_le_bytes(*b"MAPS") || reader.word()? != 1 {
            return Err("invalid stamp input header".into());
        }
        let root = reader.text()?;
        let count = reader.count(65536)?;
        let mut stamps = Vec::with_capacity(count);
        for _ in 0..count {
            stamps.push(Stamp {
                entry: reader.word()? as usize,
                width: reader.word()? as usize,
                height: reader.word()? as usize,
                size: [reader.float()?, reader.float()?, reader.float()?],
                offset: [reader.float()?, reader.float()?, reader.float()?],
                splat_mask: reader.word()? as i32,
                topology_mask: reader.word()? as i32,
                flags: reader.word()?,
                textures: [
                    reader.text()?,
                    reader.text()?,
                    reader.text()?,
                    reader.text()?,
                    reader.text()?,
                    reader.text()?,
                ],
            });
        }
        Ok(Self { root, stamps })
    }
}

pub struct Grid<'a> {
    pub height: &'a mut [i16],
    pub splat: &'a mut [u8],
    pub alpha: &'a mut [u8],
    pub topology: &'a mut [i32],
    pub height_resolution: usize,
    pub surface_resolution: usize,
    pub world_size: usize,
}

pub fn apply(
    request: &Request,
    entries: &[Entry],
    prefabs: &[Placement],
    grid: Grid<'_>,
) -> Result<(), String> {
    request.validate(entries)?;
    let n = grid.surface_resolution;
    let h = grid.height_resolution;
    if !(32..=2048).contains(&n)
        || !n.is_power_of_two()
        || !(33..=4097).contains(&h)
        || !(h - 1).is_power_of_two()
        || !(1000..=6000).contains(&grid.world_size)
        || grid.height.len() != h * h
        || grid.splat.len() != n * n * 8
        || grid.alpha.len() != n * n
        || grid.topology.len() != n * n
    {
        return Err("invalid stamp destination grid".into());
    }
    for prefab in prefabs {
        if prefab.entry >= entries.len()
            || ![
                prefab.x,
                prefab.y,
                prefab.z,
                prefab.yaw,
                prefab.yaw * std::f32::consts::PI,
            ]
            .iter()
            .all(|v| v.is_finite())
        {
            return Err("invalid prefab stamp transform".into());
        }
    }
    // Preserve the C# pipeline's four global passes: bounds, splat, normalize, other layers.
    for prefab in prefabs {
        mark_bounds(
            grid.topology,
            n,
            grid.world_size,
            prefab,
            &entries[prefab.entry],
        );
    }
    let mut descriptors = vec![None; entries.len()];
    for stamp in &request.stamps {
        descriptors[stamp.entry] = Some(stamp);
    }
    let loader = Loader::new(&request.root)?;
    for prefab in prefabs {
        if let Some(stamp) = descriptors[prefab.entry] {
            splat_stamp(grid.splat, n, grid.world_size, prefab, stamp, &loader)?;
        }
    }
    normalize(grid.splat, 8);
    for prefab in prefabs {
        let Some(stamp) = descriptors[prefab.entry] else {
            continue;
        };
        let Some(blend) = loader.load(&stamp.textures[0], stamp.bytes())? else {
            continue;
        };
        let transform = Transform::new(prefab, stamp, grid.world_size);
        if stamp.flags & 1 != 0
            && let Some(source) = loader.load(&stamp.textures[3], stamp.bytes())?
        {
            transform.for_each(h, stamp, &blend, |cell, pixel, opacity| {
                let target = -500.0 + source[pixel] as f32 / 255.0 * 1000.0 + stamp.offset[1];
                let current = decode(grid.height[cell]);
                grid.height[cell] =
                    encode((current + (target - current) * opacity).clamp(-500.0, 500.0));
            });
        }
        if stamp.flags & 2 != 0
            && let Some(source) = loader.load(&stamp.textures[4], stamp.bytes())?
        {
            transform.for_each(n, stamp, &blend, |cell, pixel, opacity| {
                grid.alpha[cell] = rounded_byte(
                    grid.alpha[cell] as f32
                        + (source[pixel + 3] as f32 - grid.alpha[cell] as f32) * opacity,
                );
            });
        }
        if let Some(source) = loader.load(&stamp.textures[5], stamp.bytes())? {
            transform.for_each(n, stamp, &blend, |cell, pixel, opacity| {
                if opacity <= 0.5 {
                    return;
                }
                let value = i32::from_le_bytes([
                    source[pixel],
                    source[pixel + 1],
                    source[pixel + 2],
                    source[pixel + 3],
                ]);
                if value == 0 {
                    return;
                }
                let applied = if stamp.topology_mask == -1 {
                    value
                } else {
                    value & stamp.topology_mask
                };
                grid.topology[cell] = if stamp.flags & 4 != 0 && stamp.topology_mask != -1 {
                    (grid.topology[cell] & !stamp.topology_mask) | applied
                } else {
                    grid.topology[cell] | applied
                };
            });
        }
    }
    Ok(())
}

impl Stamp {
    fn bytes(&self) -> usize {
        self.width * self.height * 4
    }
}

struct Loader {
    root: Option<PathBuf>,
}
impl Loader {
    fn new(root: &str) -> Result<Self, String> {
        if root.is_empty() {
            return Ok(Self { root: None });
        }
        let root = match std::fs::canonicalize(root) {
            Ok(root) => Some(root),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(format!("cannot resolve stamp cache: {error}")),
        };
        Ok(Self { root })
    }
    fn load(&self, relative: &str, expected: usize) -> Result<Option<Vec<u8>>, String> {
        let Some(root) = &self.root else {
            return Ok(None);
        };
        if relative.is_empty() {
            return Ok(None);
        }
        let path = match std::fs::canonicalize(root.join(relative)) {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("cannot resolve stamp texture: {error}")),
        };
        if !path.starts_with(root) {
            return Err("stamp texture escapes its cache directory".into());
        }
        let mut file = File::open(path).map_err(|e| format!("cannot open stamp texture: {e}"))?;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.len() != expected as u64 {
            return Ok(None);
        }
        let mut bytes = vec![0; expected];
        file.read_exact(&mut bytes).map_err(|e| e.to_string())?;
        let mut trailing = [0];
        if file.read(&mut trailing).map_err(|e| e.to_string())? != 0 {
            return Ok(None);
        }
        Ok(Some(bytes))
    }
}

fn mark_bounds(topology: &mut [i32], n: usize, size: usize, prefab: &Placement, entry: &Entry) {
    let center_x = (prefab.x / size as f32 + 0.5) * (n - 1) as f32;
    let center_z = (prefab.z / size as f32 + 0.5) * (n - 1) as f32;
    let half_x = (entry.extent_x * (n - 1) as f32 / size as f32).max(1.0);
    let half_z = (entry.extent_z * (n - 1) as f32 / size as f32).max(1.0);
    let angle = prefab.yaw * std::f32::consts::PI / 180.0;
    let cosine = angle.cos();
    let sine = angle.sin();
    let radius = (half_x * half_x + half_z * half_z).sqrt().ceil() as i64;
    // Widen bounds arithmetic so malformed/extreme metadata cannot wrap the scan range.
    let min_x = (center_x as i64).saturating_sub(radius).max(0);
    let max_x = (center_x as i64).saturating_add(radius).min(n as i64 - 1);
    let min_z = (center_z as i64).saturating_sub(radius).max(0);
    let max_z = (center_z as i64).saturating_add(radius).min(n as i64 - 1);
    for z in min_z..=max_z {
        for x in min_x..=max_x {
            let dx = x as f32 - center_x;
            let dz = z as f32 - center_z;
            let local_x = cosine * dx + sine * dz;
            let local_z = -sine * dx + cosine * dz;
            if local_x.abs() > half_x || local_z.abs() > half_z {
                continue;
            }
            let index = z as usize * n + x as usize;
            topology[index] =
                (topology[index] & !(32 | 64 | 512 | (1 << 24))) | (1 << 10) | (1 << 21);
        }
    }
}

fn splat_stamp(
    destination: &mut [u8],
    n: usize,
    size: usize,
    prefab: &Placement,
    stamp: &Stamp,
    loader: &Loader,
) -> Result<(), String> {
    if stamp.flags & 8 == 0 {
        return Ok(());
    }
    let Some(first) = loader.load(&stamp.textures[1], stamp.bytes())? else {
        return Ok(());
    };
    let Some(second) = loader.load(&stamp.textures[2], stamp.bytes())? else {
        return Ok(());
    };
    let Some(blend) = loader.load(&stamp.textures[0], stamp.bytes())? else {
        return Ok(());
    };
    let center_x = (prefab.x / size as f32 + 0.5) * (n - 1) as f32;
    let center_z = (prefab.z / size as f32 + 0.5) * (n - 1) as f32;
    let half_x = stamp.size[0] * (n - 1) as f32 / size as f32 / 2.0;
    let half_z = stamp.size[2] * (n - 1) as f32 / size as f32 / 2.0;
    let angle = prefab.yaw * std::f32::consts::PI / 180.0;
    let cosine = angle.cos();
    let sine = angle.sin();
    let min_x = (center_x - half_x - half_z).floor().max(0.0) as i32;
    let max_x = (center_x + half_x + half_z).ceil().min((n - 1) as f32) as i32;
    let min_z = (center_z - half_x - half_z).floor().max(0.0) as i32;
    let max_z = (center_z + half_x + half_z).ceil().min((n - 1) as f32) as i32;
    for z in min_z..=max_z {
        for x in min_x..=max_x {
            let dx = x as f32 - center_x;
            let dz = z as f32 - center_z;
            let u = (cosine * dx + sine * dz) / (2.0 * half_x) + 0.5;
            let v = (-sine * dx + cosine * dz) / (2.0 * half_z) + 0.5;
            if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
                continue;
            }
            let pixel = pixel(stamp, u, v);
            let opacity = blend[pixel + 3] as f32 / 255.0;
            if opacity == 0.0 {
                continue;
            }
            let mut values = [0.0; 8];
            let mut sum = 0.0;
            for (channel, value) in values.iter_mut().enumerate() {
                *value = if stamp.splat_mask & (1 << channel) == 0 {
                    0.0
                } else if channel < 4 {
                    first[pixel + channel] as f32 / 255.0
                } else {
                    second[pixel + channel - 4] as f32 / 255.0
                };
                sum += *value;
            }
            if sum == 0.0 {
                continue;
            }
            let cell = z as usize * n + x as usize;
            let retain = 1.0 - opacity * sum.min(1.0);
            for (channel, value) in values.iter().enumerate() {
                let index = channel * n * n + cell;
                destination[index] = rounded_byte(
                    (destination[index] as f32 / 255.0 * retain + value * opacity) * 255.0,
                );
            }
        }
    }
    Ok(())
}

fn normalize(values: &mut [u8], channels: usize) {
    let cells = values.len() / channels;
    for cell in 0..cells {
        let sum: u32 = (0..channels).map(|c| values[c * cells + cell] as u32).sum();
        if sum == 255 {
            continue;
        }
        if sum == 0 {
            values[cell] = 255;
            continue;
        }
        let mut remaining = 255;
        for channel in 0..channels {
            let value = if channel == channels - 1 {
                remaining
            } else {
                values[channel * cells + cell] as u32 * 255 / sum
            };
            values[channel * cells + cell] = value as u8;
            remaining -= value;
        }
    }
}

fn rounded_byte(value: f32) -> u8 {
    value.round_ties_even().clamp(0.0, 255.0) as u8
}
fn pixel(stamp: &Stamp, u: f32, v: f32) -> usize {
    (((1.0 - v) * (stamp.height - 1) as f32) as usize * stamp.width
        + (u * (stamp.width - 1) as f32) as usize)
        * 4
}

struct Transform {
    center_x: f32,
    center_z: f32,
    half_x: f32,
    half_z: f32,
    cosine: f32,
    sine: f32,
    size: usize,
}
impl Transform {
    fn new(prefab: &Placement, stamp: &Stamp, size: usize) -> Self {
        let angle = prefab.yaw * std::f32::consts::PI / 180.0;
        let cosine = angle.cos();
        let sine = angle.sin();
        Self {
            center_x: prefab.x + cosine * stamp.offset[0] + sine * stamp.offset[2],
            center_z: prefab.z - sine * stamp.offset[0] + cosine * stamp.offset[2],
            half_x: stamp.size[0] / 2.0,
            half_z: stamp.size[2] / 2.0,
            cosine,
            sine,
            size,
        }
    }
    fn for_each(
        &self,
        n: usize,
        stamp: &Stamp,
        blend: &[u8],
        mut action: impl FnMut(usize, usize, f32),
    ) {
        let center_x =
            ((self.center_x / self.size as f32 + 0.5) * (n - 1) as f32).round_ties_even() as i32;
        let center_z =
            ((self.center_z / self.size as f32 + 0.5) * (n - 1) as f32).round_ties_even() as i32;
        let radius = ((self.half_x * self.half_x + self.half_z * self.half_z).sqrt()
            * (n - 1) as f32
            / self.size as f32)
            .ceil() as i32;
        for z in (center_z - radius).max(0)..=(center_z + radius).min(n as i32 - 1) {
            for x in (center_x - radius).max(0)..=(center_x + radius).min(n as i32 - 1) {
                let world_x = (x as f32 / (n - 1) as f32 - 0.5) * self.size as f32;
                let world_z = (z as f32 / (n - 1) as f32 - 0.5) * self.size as f32;
                let dx = world_x - self.center_x;
                let dz = world_z - self.center_z;
                let u = (self.cosine * dx + self.sine * dz) / (2.0 * self.half_x) + 0.5;
                let v = (-self.sine * dx + self.cosine * dz) / (2.0 * self.half_z) + 0.5;
                if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
                    continue;
                }
                let source = pixel(stamp, u, v);
                let opacity = blend[source + 3] as f32 / 255.0;
                if opacity > 0.0 {
                    action(z as usize * n + x as usize, source, opacity);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let root = std::env::temp_dir().join(format!(
                "mapper-stamp-rust-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }
        fn rgba(&self, name: &str, pixel: [u8; 4]) {
            std::fs::write(self.0.join(name), pixel.repeat(4)).unwrap();
        }
        fn request(&self) -> Request {
            self.rgba("blend", [0, 0, 0, 255]);
            self.rgba("first", [255, 0, 0, 0]);
            self.rgba("second", [0; 4]);
            self.rgba("height", [128, 0, 0, 0]);
            self.rgba("alpha", [0; 4]);
            self.rgba("topology", [32, 0, 0, 0]);
            Request {
                root: self.0.to_str().unwrap().into(),
                stamps: vec![Stamp {
                    entry: 0,
                    width: 2,
                    height: 2,
                    size: [300.0, 20.0, 300.0],
                    offset: [0.0, 10.0, 0.0],
                    splat_mask: -1,
                    topology_mask: 16,
                    flags: 15,
                    textures: ["blend", "first", "second", "height", "alpha", "topology"]
                        .map(str::to_owned),
                }],
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let path = self.0.canonicalize().unwrap();
            assert!(path.starts_with(std::env::temp_dir().canonicalize().unwrap()));
            std::fs::remove_dir_all(path).unwrap();
        }
    }

    struct Destination {
        height: Vec<i16>,
        splat: Vec<u8>,
        alpha: Vec<u8>,
        topology: Vec<i32>,
    }
    impl Destination {
        fn new() -> Self {
            let mut splat = vec![0; 32 * 32 * 8];
            splat[4 * 32 * 32..5 * 32 * 32].fill(255);
            Self {
                height: vec![16383; 33 * 33],
                splat,
                alpha: vec![255; 32 * 32],
                topology: vec![16 | 32 | 64 | 512 | (1 << 24); 32 * 32],
            }
        }
        fn grid(&mut self) -> Grid<'_> {
            Grid {
                height: &mut self.height,
                splat: &mut self.splat,
                alpha: &mut self.alpha,
                topology: &mut self.topology,
                height_resolution: 33,
                surface_resolution: 32,
                world_size: 1000,
            }
        }
    }
    fn entries() -> Vec<Entry> {
        vec![Entry {
            group: 0,
            resolved: 0,
            land: true,
            minimum_size: 0,
            extent_x: 20.0,
            extent_z: 20.0,
            priority: 1,
            minimum_yaw: -180.0,
            maximum_yaw: 180.0,
        }]
    }
    fn prefabs() -> Vec<Placement> {
        vec![Placement {
            entry: 0,
            x: 0.0,
            y: 20.0,
            z: 0.0,
            yaw: 30.0,
        }]
    }

    #[test]
    fn ordered_passes_modify_each_layer_and_apply_masked_removal() {
        let fixture = Fixture::new();
        let request = fixture.request();
        let mut destination = Destination::new();
        apply(&request, &entries(), &prefabs(), destination.grid()).unwrap();
        let center = 16 * 32 + 16;
        assert_eq!(
            destination.height[16 * 33 + 16],
            encode(-500.0 + 128.0 / 255.0 * 1000.0 + 10.0)
        );
        assert_eq!(destination.alpha[center], 0);
        assert_eq!(destination.splat[center], 255);
        assert_eq!(destination.splat[4 * 32 * 32 + center], 0);
        assert_eq!(destination.topology[center], (1 << 10) | (1 << 21));
    }

    #[test]
    fn missing_or_wrong_length_textures_leave_optional_layers_unchanged() {
        let fixture = Fixture::new();
        let request = fixture.request();
        let mut destination = Destination::new();
        std::fs::remove_file(fixture.0.join("first")).unwrap();
        std::fs::write(fixture.0.join("height"), [0; 3]).unwrap();
        apply(&request, &entries(), &prefabs(), destination.grid()).unwrap();
        assert!(destination.height.iter().all(|&h| h == 16383));
        assert_eq!(destination.splat[4 * 32 * 32 + 16 * 32 + 16], 255);
        assert!(destination.alpha.contains(&0));
    }

    #[test]
    fn full_topology_mask_is_union_even_when_remove_flag_is_set() {
        let fixture = Fixture::new();
        let mut request = fixture.request();
        let mut destination = Destination::new();
        request.stamps[0].topology_mask = -1;
        apply(&request, &entries(), &prefabs(), destination.grid()).unwrap();
        assert_eq!(
            destination.topology[16 * 32 + 16],
            16 | 32 | (1 << 10) | (1 << 21)
        );
        assert_eq!(rounded_byte(2.5), 2);
        assert_eq!(rounded_byte(3.5), 4);
        let mut values = [1, 1, 0, 0, 0, 0, 0, 0];
        normalize(&mut values, 8);
        assert_eq!(values, [127, 127, 0, 0, 0, 0, 0, 1]);
    }

    #[test]
    fn invalid_stamp_paths_dimensions_and_transforms_are_rejected() {
        let fixture = Fixture::new();
        let good = fixture.request();
        let mut bad = good.clone();
        bad.stamps[0].textures[0] = "../outside".into();
        assert!(bad.validate(&entries()).is_err());
        bad = good.clone();
        bad.stamps[0].width = 4097;
        assert!(bad.validate(&entries()).is_err());
        bad = good.clone();
        bad.stamps[0].size[0] = 0.0;
        assert!(bad.validate(&entries()).is_err());
        bad = good.clone();
        bad.stamps[0].offset[1] = f32::NAN;
        assert!(bad.validate(&entries()).is_err());
        let mut destination = Destination::new();
        let mut placed = prefabs();
        placed[0].yaw = f32::INFINITY;
        assert!(apply(&good, &entries(), &placed, destination.grid()).is_err());
        assert!(destination.height.iter().all(|&h| h == 16383));
    }
}
