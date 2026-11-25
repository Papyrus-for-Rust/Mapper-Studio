//! Streaming, explicitly version-10 container output for composed native grids.
//! Caller owns atomic publication and compatibility/readiness checks.
use crate::{
    export::{begin_v10, bytes, float, layer_header, number, valid_heights, vector},
    native_layers::{Layers, dimensions},
    prefab_records::Record,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    io::{self, Write},
    time::Instant,
};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Path {
    pub name: String,
    pub spline: bool,
    pub start: bool,
    pub end: bool,
    pub width: f32,
    pub inner_padding: f32,
    pub outer_padding: f32,
    pub inner_fade: f32,
    pub outer_fade: f32,
    pub random_scale: f32,
    pub mesh_offset: f32,
    pub terrain_offset: f32,
    pub splat: i32,
    pub topology: i32,
    pub nodes: Vec<[f32; 3]>,
    pub hierarchy: i32,
}
pub struct World<'a> {
    pub layers: &'a Layers,
    pub height: &'a [i16],
    /// No local water grid means the required water plane is all zero.
    pub water: Option<&'a [i16]>,
    pub alpha: &'a [u8],
    pub prefabs: &'a [Record],
    pub paths: &'a [Path],
}
fn error(s: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, s)
}
// Write a layer without duplicating its little-endian data before it reaches
// the legacy block buffers. Preserve cancellation checks between chunks.
fn write_layer(
    out: &mut impl Write,
    name: &str,
    data: &[u8],
    heights: bool,
    validation_ms: &mut f64,
    cancel: &impl Fn() -> bool,
) -> io::Result<bool> {
    layer_header(out, name, data.len())?;
    for chunk in data.chunks(65536) {
        if cancel() {
            return Ok(false);
        }
        if heights {
            let clock = Instant::now();
            if !valid_heights(chunk) {
                return Err(error("invalid export terrain height"));
            }
            *validation_ms += clock.elapsed().as_secs_f64() * 1000.0;
        }
        out.write_all(chunk)?;
    }
    Ok(true)
}

fn write_words<T: bytemuck::Pod, const N: usize>(
    out: &mut impl Write,
    name: &str,
    values: &[T],
    encode: impl Fn(T) -> [u8; N],
    heights: bool,
    validation_ms: &mut f64,
    cancel: &impl Fn() -> bool,
) -> io::Result<bool> {
    #[cfg(target_endian = "little")]
    {
        let _ = encode;
        write_layer(
            out,
            name,
            bytemuck::cast_slice(values),
            heights,
            validation_ms,
            cancel,
        )
    }
    #[cfg(target_endian = "big")]
    {
        layer_header(out, name, values.len() * N)?;
        let mut buffer = [0; 65536];
        for values in values.chunks(buffer.len() / N) {
            if cancel() {
                return Ok(false);
            }
            let clock = Instant::now();
            let encoded = &mut buffer[..values.len() * N];
            for (bytes, &value) in encoded.chunks_exact_mut(N).zip(values) {
                bytes.copy_from_slice(&encode(value));
            }
            if heights && !valid_heights(encoded) {
                return Err(error("invalid export terrain height"));
            }
            *validation_ms += clock.elapsed().as_secs_f64() * 1000.0;
            out.write_all(encoded)?;
        }
        Ok(true)
    }
}

/// Layers are borrowed. Only bounded record/header data and word/compression
/// buffers are allocated; the complete world is never duplicated as an IR blob.
pub fn write_v10(
    world: World<'_>,
    timestamp: u64,
    output: impl Write,
    cancel: impl Fn() -> bool,
) -> io::Result<Option<crate::export::Profile>> {
    write_v10_with_pool(world, timestamp, output, cancel, None)
}

/// A supplied pool can compress consecutive blocks concurrently. The
/// compressed blocks still publish in their original order.
pub fn write_v10_with_pool(
    world: World<'_>,
    timestamp: u64,
    output: impl Write,
    cancel: impl Fn() -> bool,
    pool: Option<&rayon::ThreadPool>,
) -> io::Result<Option<crate::export::Profile>> {
    write_v10_with_options(world, timestamp, output, cancel, pool, Options::default())
}

#[derive(Default)]
pub struct Options<'a> {
    pub extra_layers: &'a [(&'a str, &'a [u8])],
    /// Clear the native Road bit in the exported map. Rail and all other
    /// topology flags still participate in the game's placement checks.
    pub allow_building_on_roads: bool,
}

pub fn write_v10_with_options(
    world: World<'_>,
    timestamp: u64,
    output: impl Write,
    cancel: impl Fn() -> bool,
    pool: Option<&rayon::ThreadPool>,
    options: Options<'_>,
) -> io::Result<Option<crate::export::Profile>> {
    if cancel() {
        return Ok(None);
    }
    let clock = Instant::now();
    let l = world.layers;
    let (hr, n) = dimensions(l.size).map_err(|e| error(&e))?;
    let cells = n * n;
    let extra = options.extra_layers;
    if l.height_resolution != hr
        || l.surface_resolution != n
        || l.height.len() != hr * hr
        || world.water.is_some_and(|water| water.len() != hr * hr)
        || world.height.len() != hr * hr
        || l.topology.len() != cells
        || world.alpha.len() != cells
        || l.splat.len() != 8 * cells
        || l.biome.len() != 5 * cells
        || world.prefabs.len() > 65536
        || world.paths.len() > 4096
        || timestamp > i64::MAX as u64
        || extra.len() > 2
        || extra.iter().enumerate().any(|(index, &(name, data))| {
            !matches!(name, "papyrus.cargo-route" | "papyrus.world-rules")
                || data.is_empty()
                || data.len() > 1024 * 1024
                || extra[..index].iter().any(|(previous, _)| *previous == name)
        })
    {
        return Err(error("Invalid composed map shape or record limits"));
    }
    let mut records = Vec::new();
    let mut record = Vec::new();
    for p in world.prefabs {
        if cancel() {
            return Ok(None);
        }
        if p.id == 0
            || p.category.is_empty()
            || p.category.len() > 128
            || p.category.chars().any(char::is_control)
            || p.position
                .iter()
                .chain(&p.rotation)
                .any(|v| !v.is_finite() || v.abs() > 10000.)
            || p.scale
                .iter()
                .any(|v| !v.is_finite() || *v == 0. || v.abs() > 100.)
        {
            return Err(error("Invalid prefab serialization record"));
        }
        record.clear();
        bytes(&mut record, 1, p.category.as_bytes())?;
        number(&mut record, 2, u64::from(p.id))?;
        vector(&mut record, 3, p.position)?;
        vector(&mut record, 4, p.rotation)?;
        vector(&mut record, 5, p.scale)?;
        bytes(&mut records, 3, &record)?;
    }
    let mut names = HashSet::new();
    let mut nodes = 0usize;
    for p in world.paths {
        if cancel() {
            return Ok(None);
        }
        nodes = nodes
            .checked_add(p.nodes.len())
            .ok_or_else(|| error("Path node overflow"))?;
        if p.name.is_empty()
            || p.name.len() > 128
            || p.name.chars().any(char::is_control)
            || !names.insert(&p.name)
            || !(2..=65536).contains(&p.nodes.len())
            || nodes > 262144
            || [
                p.width,
                p.inner_padding,
                p.outer_padding,
                p.inner_fade,
                p.outer_fade,
                p.random_scale,
                p.mesh_offset,
                p.terrain_offset,
            ]
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 128.)
            || p.width < 0.
            || [
                p.inner_padding,
                p.outer_padding,
                p.inner_fade,
                p.outer_fade,
                p.random_scale,
            ]
            .iter()
            .any(|v| *v < 0.)
            || !(0..=16).contains(&p.hierarchy)
            || p.nodes
                .iter()
                .flatten()
                .any(|v| !v.is_finite() || v.abs() > 10000.)
        {
            return Err(error("Invalid full path serialization record"));
        }
        record.clear();
        bytes(&mut record, 1, p.name.as_bytes())?;
        for (field, flag) in [(2, p.spline), (3, p.start), (4, p.end)] {
            number(&mut record, field, u64::from(flag))?;
        }
        for (field, value) in [
            (5, p.width),
            (6, p.inner_padding),
            (7, p.outer_padding),
            (8, p.inner_fade),
            (9, p.outer_fade),
            (10, p.random_scale),
            (11, p.mesh_offset),
            (12, p.terrain_offset),
        ] {
            float(&mut record, field, value)?;
        }
        number(&mut record, 13, p.splat as i64 as u64)?;
        number(&mut record, 14, p.topology as i64 as u64)?;
        for &node in &p.nodes {
            if cancel() {
                return Ok(None);
            }
            vector(&mut record, 15, node)?;
        }
        number(&mut record, 16, p.hierarchy as i64 as u64)?;
        bytes(&mut records, 4, &record)?;
    }

    let mut out = begin_v10(output, timestamp, u64::from(l.size), pool)?;
    let mut validation_ms = 0.;
    for (name, data) in [("height", world.height), ("terrain", l.height.as_slice())] {
        if !write_words(
            &mut out,
            name,
            data,
            i16::to_le_bytes,
            true,
            &mut validation_ms,
            &cancel,
        )? {
            return Ok(None);
        }
    }
    if let Some(water) = world.water {
        if !write_words(
            &mut out,
            "water",
            water,
            i16::to_le_bytes,
            true,
            &mut validation_ms,
            &cancel,
        )? {
            return Ok(None);
        }
    } else {
        static ZERO: [u8; 65536] = [0; 65536];
        let length = hr * hr * std::mem::size_of::<i16>();
        layer_header(&mut out, "water", length)?;
        for offset in (0..length).step_by(ZERO.len()) {
            if cancel() {
                return Ok(None);
            }
            out.write_all(&ZERO[..(length - offset).min(ZERO.len())])?;
        }
    }
    // The native source independently quantizes channels. Preserve its planes
    // verbatim even when a cell's integer weights do not sum to 255.
    for (name, data) in [("splat", l.splat.as_slice()), ("biome", l.biome.as_slice())] {
        if !write_layer(&mut out, name, data, false, &mut validation_ms, &cancel)? {
            return Ok(None);
        }
    }
    let topology_written = if options.allow_building_on_roads {
        const ROAD: i32 = 0x800;
        let mut buffer = [0_i32; 4096];
        layer_header(&mut out, "topology", l.topology.len() * size_of::<i32>())?;
        for cells in l.topology.chunks(buffer.len()) {
            if cancel() {
                return Ok(None);
            }
            let words = &mut buffer[..cells.len()];
            for (word, &flags) in words.iter_mut().zip(cells) {
                *word = (flags & !ROAD).to_le();
            }
            out.write_all(bytemuck::cast_slice(words))?;
        }
        true
    } else {
        write_words(
            &mut out,
            "topology",
            &l.topology,
            i32::to_le_bytes,
            false,
            &mut validation_ms,
            &cancel,
        )?
    };
    if !topology_written
        || !write_layer(
            &mut out,
            "alpha",
            world.alpha,
            false,
            &mut validation_ms,
            &cancel,
        )?
    {
        return Ok(None);
    }
    for &(name, data) in extra {
        if !write_layer(&mut out, name, data, false, &mut validation_ms, &cancel)? {
            return Ok(None);
        }
    }
    for chunk in records.chunks(65536) {
        if cancel() {
            return Ok(None);
        }
        out.write_all(chunk)?;
    }
    let mut result = out.finish()?;
    result.validation_ms = validation_ms;
    result.total_ms = clock.elapsed().as_secs_f64() * 1000.0;
    Ok((!cancel()).then_some(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn allowing_road_building_clears_only_road_topology() {
        let mut layers = layers();
        let last = layers.topology.len() - 1;
        layers.topology[0] = 0x800 | 0x80000 | 0x1000;
        layers.topology[last] = i32::MIN | 0x800 | 0x400;
        let alpha = vec![255; layers.topology.len()];
        let paths = [path()];
        let write = |allow_building_on_roads| {
            let mut bytes = Vec::new();
            assert!(
                write_v10_with_options(
                    World {
                        layers: &layers,
                        height: &layers.height,
                        water: None,
                        alpha: &alpha,
                        prefabs: &[],
                        paths: &paths,
                    },
                    1,
                    &mut bytes,
                    || false,
                    None,
                    Options {
                        allow_building_on_roads,
                        ..Default::default()
                    },
                )
                .unwrap()
                .is_some()
            );
            crate::content::read_map(&bytes, || false).unwrap()
        };
        let baseline = write(false);
        let allowed = write(true);
        assert_eq!(baseline.paths, allowed.paths);
        for (baseline, allowed) in baseline.layers.iter().zip(&allowed.layers) {
            assert_eq!(baseline.name, allowed.name);
            if allowed.name == "topology" {
                let expected = layers
                    .topology
                    .iter()
                    .flat_map(|flags| (flags & !0x800).to_le_bytes());
                assert!(allowed.data.iter().copied().eq(expected));
            } else {
                assert_eq!(baseline.data, allowed.data);
            }
        }
        assert_eq!(layers.topology[0], 0x81800);
    }

    #[test]
    fn absent_water_matches_an_explicit_zero_plane() {
        let layers = layers();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let water = vec![0; layers.height.len()];
        let write = |water| {
            let mut bytes = Vec::new();
            let result = write_v10(
                World {
                    layers: &layers,
                    height: &layers.height,
                    water,
                    alpha: &alpha,
                    prefabs: &[],
                    paths: &[],
                },
                1,
                &mut bytes,
                || false,
            )?;
            assert!(result.is_some());
            Ok::<_, io::Error>(bytes)
        };
        assert_eq!(write(None).unwrap(), write(Some(&water)).unwrap());
        assert!(write(Some(&[])).is_err());
    }
    fn layers() -> Layers {
        let (hr, n) = dimensions(1000).unwrap();
        let mut l = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: n,
            height: vec![16383; hr * hr],
            biome: vec![0; n * n * 5],
            splat: vec![0; n * n * 8],
            topology: vec![i32::MIN; n * n],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        l.biome[..n * n].fill(255);
        l.splat[..n * n].fill(255);
        l
    }
    fn path() -> Path {
        Path {
            name: "Road 0".into(),
            spline: true,
            start: true,
            end: false,
            width: 4.,
            inner_padding: 1.,
            outer_padding: 8.,
            inner_fade: 0.5,
            outer_fade: 4.,
            random_scale: 1.25,
            mesh_offset: 0.05,
            terrain_offset: -0.125,
            splat: 1,
            topology: i32::MIN,
            hierarchy: 2,
            nodes: vec![[-10., 2., 3.], [20., 4., 30.]],
        }
    }
    #[test]
    fn full_paths_and_more_than_4096_prefabs_survive_container_roundtrip() {
        let mut l = layers();
        let cells = l.surface_resolution.pow(2);
        l.splat[0] = 250;
        l.splat[cells + 1] = 4;
        l.biome[0] = 254;
        l.biome[cells + 1] = 1;
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let mut water = vec![0; l.height.len()];
        water[l.height.len() - 1] = 0x1234;
        l.topology[cells - 1] = 0x0123_4567;
        let paths = [path()];
        let gameplay_height = vec![16400; l.height.len()];
        let prefabs: Vec<_> = (0..4097)
            .map(|_| Record {
                category: "Decor".into(),
                path: "assets/a.prefab".into(),
                id: 7,
                position: [1., 2., 3.],
                rotation: [12., 34., 56.],
                scale: [1.; 3],
            })
            .collect();
        let mut bytes = Vec::new();
        assert!(
            write_v10(
                World {
                    layers: &l,
                    height: &gameplay_height,
                    alpha: &alpha,
                    water: Some(&water),
                    paths: &paths,
                    prefabs: &prefabs
                },
                123,
                &mut bytes,
                || false
            )
            .unwrap()
            .is_some()
        );
        let map = crate::content::read_map(&bytes, || false).unwrap();
        assert_eq!(map.size, 1000);
        assert_eq!(map.prefabs.len(), 4097);
        assert_eq!(map.prefabs[0].id, 7);
        assert_eq!(map.prefabs[0].rotation.y, 34.);
        let p = &map.paths[0];
        assert_eq!(p.hierarchy, 2);
        assert_eq!(p.inner_padding, 1.);
        assert_eq!(p.outer_padding, 8.);
        assert_eq!(p.inner_fade, 0.5);
        assert_eq!(p.outer_fade, 4.);
        assert_eq!(p.random_scale, 1.25);
        assert_eq!(p.mesh_offset, 0.05);
        assert_eq!(p.terrain_offset, -0.125);
        assert_eq!(p.topology, i32::MIN);
        assert_eq!(p.unknown_fields, 0);
        assert_eq!(map.layers.len(), 7);
        assert_eq!(
            map.layers.iter().find(|l| l.name == "splat").unwrap().data,
            l.splat
        );
        assert_eq!(
            map.layers.iter().find(|l| l.name == "biome").unwrap().data,
            l.biome
        );
        let first = |name: &str| {
            i16::from_le_bytes(
                map.layers.iter().find(|l| l.name == name).unwrap().data[..2]
                    .try_into()
                    .unwrap(),
            )
        };
        assert_eq!(first("height"), 16400);
        assert_eq!(first("terrain"), 16383);
        let data = |name: &str| {
            map.layers
                .iter()
                .find(|l| l.name == name)
                .unwrap()
                .data
                .as_slice()
        };
        assert!(data("water").ends_with(&[0x34, 0x12]));
        assert!(data("topology").ends_with(&[0x67, 0x45, 0x23, 0x01]));
        assert_eq!(
            map.layers
                .iter()
                .find(|x| x.name == "height")
                .unwrap()
                .data
                .len(),
            l.height.len() * 2
        );
    }
    #[test]
    fn utility_path_defaults_roundtrip_with_zero_width_and_negative_width_is_rejected() {
        let layers = layers();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let water = vec![0; layers.height.len()];
        let mut utility = Path {
            name: "Powerline 0".into(),
            spline: true,
            start: true,
            end: true,
            width: 0.,
            inner_padding: 0.,
            outer_padding: 0.,
            inner_fade: 0.,
            outer_fade: 0.,
            random_scale: 0.,
            mesh_offset: 0.,
            terrain_offset: 0.,
            splat: 0,
            topology: 0,
            hierarchy: 0,
            nodes: path().nodes,
        };
        let export = |path: &Path| {
            let mut output = Vec::new();
            write_v10(
                World {
                    layers: &layers,
                    height: &layers.height,
                    alpha: &alpha,
                    water: Some(&water),
                    paths: std::slice::from_ref(path),
                    prefabs: &[],
                },
                123,
                &mut output,
                || false,
            )
            .map(|_| output)
        };
        let bytes = export(&utility).unwrap();
        let map = crate::content::read_map(&bytes, || false).unwrap();
        let path = &map.paths[0];
        assert_eq!(path.name, "Powerline 0");
        assert_eq!(path.width, 0.);
        assert!(path.spline && path.start && path.end);
        assert_eq!(path.random_scale, 0.);
        assert_eq!(path.hierarchy, 0);
        assert_eq!(path.topology, 0);
        utility.width = -1.;
        assert!(export(&utility).is_err());
    }
    #[test]
    fn one_shot_cancel_is_latched_and_invalid_input_does_not_succeed() {
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let water = vec![0; l.height.len()];
        let paths = [path()];
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(3)
            .build()
            .unwrap();
        for workers in [None, Some(&pool)] {
            let calls = Cell::new(0);
            let mut out = Vec::new();
            assert!(
                write_v10_with_pool(
                    World {
                        layers: &l,
                        height: &l.height,
                        alpha: &alpha,
                        water: Some(&water),
                        paths: &paths,
                        prefabs: &[]
                    },
                    0,
                    &mut out,
                    || {
                        calls.set(calls.get() + 1);
                        calls.get() == 30
                    },
                    workers,
                )
                .unwrap()
                .is_none()
            );
        }
        assert!(
            write_v10(
                World {
                    layers: &l,
                    height: &l.height,
                    alpha: &alpha[..1],
                    water: Some(&water),
                    paths: &paths,
                    prefabs: &[]
                },
                0,
                &mut Vec::new(),
                || false
            )
            .is_err()
        );
        let mut bad = path();
        bad.hierarchy = -1;
        assert!(
            write_v10(
                World {
                    layers: &l,
                    height: &l.height,
                    alpha: &alpha,
                    water: Some(&water),
                    paths: &[bad],
                    prefabs: &[]
                },
                0,
                &mut Vec::new(),
                || false
            )
            .is_err()
        );
        let mut bad_water = water;
        *bad_water.last_mut().unwrap() = i16::MAX;
        assert_eq!(
            write_v10(
                World {
                    layers: &l,
                    height: &l.height,
                    alpha: &alpha,
                    water: Some(&bad_water),
                    paths: &paths,
                    prefabs: &[],
                },
                0,
                io::sink(),
                || false,
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::InvalidData
        );
    }
}
