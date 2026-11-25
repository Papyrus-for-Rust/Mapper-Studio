//! Bounded version-10 map content reader and monument-local composition.
//! Original source bytes must be retained by callers when unknown extension data matters.
use std::{collections::HashSet, io::Read};

pub const MAX_SOURCE: usize = 64 * 1024 * 1024;
pub const MAX_DECODED: usize = 256 * 1024 * 1024;
pub const MAX_PREFABS: usize = 65536;
const MAX_BLOCK: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Vector {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}
impl Vector {
    fn valid(self) -> bool {
        [self.x, self.y, self.z]
            .iter()
            .all(|v| v.is_finite() && v.abs() <= 1_000_000.)
    }
    fn sub(self, b: Self) -> Self {
        Self {
            x: self.x - b.x,
            y: self.y - b.y,
            z: self.z - b.z,
        }
    }
    fn add(self, b: Self) -> Self {
        Self {
            x: self.x + b.x,
            y: self.y + b.y,
            z: self.z + b.z,
        }
    }
    fn yaw(self, degrees: f32) -> Self {
        let (s, c) = degrees.to_radians().sin_cos();
        Self {
            x: self.x * c + self.z * s,
            y: self.y,
            z: -self.x * s + self.z * c,
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct Prefab {
    pub category: String,
    pub id: u32,
    pub position: Vector,
    pub rotation: Vector,
    pub scale: Vector,
    pub unknown_fields: usize,
}
#[derive(Clone, Debug)]
pub struct Layer {
    pub name: String,
    pub data: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Path {
    pub inner_padding: f32,
    pub outer_padding: f32,
    pub inner_fade: f32,
    pub outer_fade: f32,
    pub random_scale: f32,
    pub mesh_offset: f32,
    pub terrain_offset: f32,
    pub hierarchy: i32,
    pub name: String,
    pub nodes: Vec<Vector>,
    pub width: f32,
    pub spline: bool,
    pub start: bool,
    pub end: bool,
    pub splat: i32,
    pub topology: i32,
    pub unknown_fields: usize,
}
#[derive(Clone, Debug)]
pub struct MapContent {
    pub size: u32,
    pub prefabs: Vec<Prefab>,
    pub paths: Vec<Path>,
    pub layers: Vec<Layer>,
    pub unknown_fields: usize,
}

enum Wire<'a> {
    Number(u64),
    Bytes(&'a [u8]),
    Float(f32),
    Other,
}
struct Proto<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Proto<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }
    fn varint(&mut self) -> Result<u64, String> {
        let mut value = 0_u64;
        for i in 0..10 {
            let b = *self.bytes.get(self.at).ok_or("Truncated varint")?;
            self.at += 1;
            if i == 9 && b > 1 {
                return Err("Varint overflow".into());
            }
            value |= u64::from(b & 127) << (i * 7);
            if b & 128 == 0 {
                return Ok(value);
            }
        }
        Err("Varint overflow".into())
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(n).ok_or("Length overflow")?;
        let b = self.bytes.get(self.at..end).ok_or("Truncated message")?;
        self.at = end;
        Ok(b)
    }
    fn next(&mut self) -> Result<Option<(u32, Wire<'a>)>, String> {
        if self.at == self.bytes.len() {
            return Ok(None);
        }
        let tag = self.varint()?;
        let field = u32::try_from(tag >> 3).map_err(|_| "Invalid protobuf field")?;
        if field == 0 || field > 0x1fffffff {
            return Err("Invalid protobuf field".into());
        }
        let wire = match tag & 7 {
            0 => Wire::Number(self.varint()?),
            1 => {
                self.take(8)?;
                Wire::Other
            }
            2 => {
                let len = usize::try_from(self.varint()?).map_err(|_| "Message too large")?;
                Wire::Bytes(self.take(len)?)
            }
            5 => Wire::Float(f32::from_le_bytes(
                self.take(4)?.try_into().map_err(|_| "Invalid float")?,
            )),
            _ => return Err("Unsupported protobuf wire type".into()),
        };
        Ok(Some((field, wire)))
    }
}
fn text(bytes: &[u8]) -> Result<String, String> {
    if bytes.len() > 4096 {
        return Err("Text exceeds 4096 bytes".into());
    }
    let s = std::str::from_utf8(bytes).map_err(|_| "Invalid UTF-8")?;
    if s.contains('\0') {
        return Err("Text contains NUL".into());
    }
    Ok(s.to_string())
}
fn vector(bytes: &[u8]) -> Result<Vector, String> {
    let mut r = Proto::new(bytes);
    let mut v = Vector::default();
    while let Some((field, wire)) = r.next()? {
        match (field, wire) {
            (1, Wire::Float(f)) => v.x = f,
            (2, Wire::Float(f)) => v.y = f,
            (3, Wire::Float(f)) => v.z = f,
            (1..=3, _) => return Err("Invalid vector component wire type".into()),
            _ => {}
        }
    }
    if !v.valid() {
        return Err("Non-finite or excessive transform".into());
    }
    Ok(v)
}
fn prefab(bytes: &[u8]) -> Result<Prefab, String> {
    let mut p = Prefab {
        category: String::new(),
        id: 0,
        position: Vector::default(),
        rotation: Vector::default(),
        scale: Vector::default(),
        unknown_fields: 0,
    };
    let mut r = Proto::new(bytes);
    while let Some((field, wire)) = r.next()? {
        match (field, wire) {
            (1, Wire::Bytes(b)) => p.category = text(b)?,
            (2, Wire::Number(v)) => p.id = u32::try_from(v).map_err(|_| "Prefab ID overflow")?,
            (3, Wire::Bytes(b)) => p.position = vector(b)?,
            (4, Wire::Bytes(b)) => p.rotation = vector(b)?,
            (5, Wire::Bytes(b)) => p.scale = vector(b)?,
            (1..=5, _) => return Err("Invalid prefab field wire type".into()),
            _ => p.unknown_fields += 1,
        }
    }
    if p.id == 0 {
        return Err("Prefab record has no asset ID".into());
    }
    Ok(p)
}
fn layer(bytes: &[u8]) -> Result<Layer, String> {
    let mut r = Proto::new(bytes);
    let mut name = String::new();
    let mut data = None;
    while let Some((field, wire)) = r.next()? {
        match (field, wire) {
            (1, Wire::Bytes(b)) => name = text(b)?,
            (2, Wire::Bytes(b)) => {
                if data.is_some() {
                    return Err("Duplicate layer payload".into());
                }
                data = Some(b.to_vec());
            }
            (1..=2, _) => return Err("Invalid layer field".into()),
            _ => {}
        }
    }
    if name.is_empty() {
        return Err("Unnamed map layer".into());
    }
    Ok(Layer {
        name,
        data: data.ok_or("Missing layer data")?,
    })
}
fn path(bytes: &[u8]) -> Result<Path, String> {
    let mut r = Proto::new(bytes);
    let mut p = Path {
        inner_padding: 0.,
        outer_padding: 0.,
        inner_fade: 0.,
        outer_fade: 0.,
        random_scale: 0.,
        mesh_offset: 0.,
        terrain_offset: 0.,
        hierarchy: 0,
        name: String::new(),
        nodes: Vec::new(),
        width: 0.,
        spline: false,
        start: false,
        end: false,
        splat: 0,
        topology: 0,
        unknown_fields: 0,
    };
    while let Some((field, wire)) = r.next()? {
        match (field, wire) {
            (1, Wire::Bytes(b)) => p.name = text(b)?,
            (5, Wire::Float(v)) => p.width = v,
            (6, Wire::Float(v)) => p.inner_padding = v,
            (7, Wire::Float(v)) => p.outer_padding = v,
            (8, Wire::Float(v)) => p.inner_fade = v,
            (9, Wire::Float(v)) => p.outer_fade = v,
            (10, Wire::Float(v)) => p.random_scale = v,
            (11, Wire::Float(v)) => p.mesh_offset = v,
            (12, Wire::Float(v)) => p.terrain_offset = v,
            (16, Wire::Number(v)) => p.hierarchy = v as i32,
            (2, Wire::Number(v)) if v <= 1 => p.spline = v == 1,
            (3, Wire::Number(v)) if v <= 1 => p.start = v == 1,
            (4, Wire::Number(v)) if v <= 1 => p.end = v == 1,
            (13, Wire::Number(v)) => p.splat = v as i32,
            (14, Wire::Number(v)) => p.topology = v as i32,
            (15, Wire::Bytes(b)) => {
                if p.nodes.len() >= 262144 {
                    return Err("Too many path nodes".into());
                }
                p.nodes.push(vector(b)?);
            }
            (1..=16, _) => return Err("Invalid path field".into()),
            _ => p.unknown_fields += 1,
        }
    }
    if [
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
    .any(|v| !v.is_finite() || v.abs() > 65536.)
        || p.width < 0.
    {
        return Err("Invalid path width".into());
    }
    Ok(p)
}

fn decode_blocks(bytes: &[u8], cancelled: &impl Fn() -> bool) -> Result<Vec<u8>, String> {
    let mut blocks = Proto::new(bytes);
    let mut decoded = Vec::new();
    while blocks.at < blocks.bytes.len() {
        if cancelled() {
            return Err("Content import cancelled".into());
        }
        let flags = blocks.varint()?;
        if flags & !3 != 0 {
            return Err("Unsupported legacy LZ4 flags".into());
        }
        let length = usize::try_from(blocks.varint()?).map_err(|_| "Block too large")?;
        if length == 0 || length > MAX_BLOCK || length > MAX_DECODED - decoded.len() {
            return Err("Invalid or excessive decoded content block".into());
        }
        let compressed = if flags & 1 != 0 {
            usize::try_from(blocks.varint()?).map_err(|_| "Block too large")?
        } else {
            length
        };
        if compressed == 0 || compressed > length || compressed > MAX_SOURCE {
            return Err("Invalid compressed block size".into());
        }
        let payload = blocks.take(compressed)?;
        if flags & 1 != 0 {
            let start = decoded.len();
            decoded.resize(start + length, 0);
            let written = lz4_flex::block::decompress_into(payload, &mut decoded[start..])
                .map_err(|e| format!("Invalid LZ4 block: {e}"))?;
            if written != length {
                return Err("Decoded block length mismatch".into());
            }
        } else {
            decoded.extend_from_slice(payload);
        }
    }
    Ok(decoded)
}

pub fn read_map(bytes: &[u8], cancelled: impl Fn() -> bool) -> Result<MapContent, String> {
    if bytes.len() > MAX_SOURCE {
        return Err("Content source exceeds 64 MiB".into());
    }
    if bytes.len() < 13 || bytes[..4] != 10_u32.to_le_bytes() {
        return Err("Only verified version-10 Rust .map containers are supported.".into());
    }
    let decoded = decode_blocks(&bytes[12..], &cancelled)?;
    let mut content = MapContent {
        size: 0,
        prefabs: Vec::new(),
        paths: Vec::new(),
        layers: Vec::new(),
        unknown_fields: 0,
    };
    let mut r = Proto::new(&decoded);
    let mut names = HashSet::new();
    let mut nodes = 0;
    while let Some((field, wire)) = r.next()? {
        if cancelled() {
            return Err("Content import cancelled".into());
        }
        match (field, wire) {
            (1, Wire::Number(size)) => {
                content.size = u32::try_from(size).map_err(|_| "World size overflow")?
            }
            (2, Wire::Bytes(b)) => {
                if content.layers.len() >= 64 {
                    return Err("Too many content layers".into());
                }
                let l = layer(b)?;
                if !names.insert(l.name.clone()) {
                    return Err("Duplicate content layer".into());
                }
                content.layers.push(l);
            }
            (3, Wire::Bytes(b)) => {
                if content.prefabs.len() >= MAX_PREFABS {
                    return Err("Too many content prefabs".into());
                }
                content.prefabs.push(prefab(b)?);
            }
            (4, Wire::Bytes(b)) => {
                if content.paths.len() >= 4096 {
                    return Err("Too many content paths".into());
                }
                let p = path(b)?;
                nodes += p.nodes.len();
                if nodes > 262144 {
                    return Err("Content path node budget exceeded".into());
                }
                content.paths.push(p);
            }
            (1..=4, _) => return Err("Invalid world field wire type".into()),
            _ => content.unknown_fields += 1,
        }
    }
    if !(1..=65536).contains(&content.size) {
        return Err("Invalid authoring map size".into());
    }
    Ok(content)
}

#[derive(Debug)]
pub struct RustEditPrefab {
    pub prefabs: Vec<Prefab>,
    /// Original terrain descriptor bytes. Their interpretation also depends
    /// on the image sidecars from the same export.
    pub terrain_descriptor: Vec<u8>,
    pub other_fields: usize,
}

/// RustEdit 1 version-1 custom prefabs reuse the game's prefab record inside
/// a different protobuf root and omit the Rust world header timestamp.
pub fn read_rustedit_prefab(
    bytes: &[u8],
    cancelled: impl Fn() -> bool,
) -> Result<RustEditPrefab, String> {
    if bytes.len() > MAX_SOURCE {
        return Err("Content source exceeds 64 MiB".into());
    }
    if !bytes.starts_with(&1_u32.to_le_bytes()) {
        return Err("Expected a version-1 RustEdit custom prefab".into());
    }
    let decoded = decode_blocks(&bytes[4..], &cancelled)?;
    let mut reader = Proto::new(&decoded);
    let mut prefabs = Vec::new();
    let mut terrain_descriptor = None;
    let mut other_fields = 0;
    while let Some((field, wire)) = reader.next()? {
        if cancelled() {
            return Err("Content import cancelled".into());
        }
        match (field, wire) {
            (1, Wire::Bytes(bytes)) if terrain_descriptor.is_none() && bytes.len() <= 65536 => {
                terrain_descriptor = Some(bytes.to_vec());
            }
            (3, Wire::Bytes(bytes)) if prefabs.len() < MAX_PREFABS => prefabs.push(prefab(bytes)?),
            (1 | 3, _) => return Err("Invalid RustEdit prefab field or limit".into()),
            (_, Wire::Bytes([])) => {}
            _ => other_fields += 1,
        }
    }
    let terrain_descriptor = terrain_descriptor.ok_or("Missing RustEdit prefab descriptor")?;
    if prefabs.is_empty() && terrain_descriptor.is_empty() {
        return Err("RustEdit prefab has no objects or terrain descriptor".into());
    }
    Ok(RustEditPrefab { prefabs, terrain_descriptor, other_fields })
}

/// Reference-map add-on removes exactly the selected stock anchor; standalone removes nothing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Composition {
    AddOn { anchor: usize },
    Standalone { pivot: Vector, authoring_yaw: f32 },
}
#[derive(Clone, Debug)]
pub struct Composed {
    pub prefabs: Vec<Prefab>,
    pub paths: Vec<Path>,
    pub anchor_id: Option<u32>,
}
pub fn compose(
    content: &MapContent,
    mode: Composition,
    position: Vector,
    yaw: f32,
) -> Result<Composed, String> {
    if !position.valid() || !yaw.is_finite() || yaw.abs() > 360000. {
        return Err("Invalid destination transform".into());
    }
    let (pivot, authoring_yaw, anchor) = match mode {
        Composition::AddOn { anchor } => {
            let p = content
                .prefabs
                .get(anchor)
                .ok_or("Anchor index outside source prefab list")?;
            if p.rotation.x.abs() > 0.001
                || p.rotation.z.abs() > 0.001
                || [p.scale.x, p.scale.y, p.scale.z]
                    .iter()
                    .any(|s| (*s - 1.).abs() > 0.001)
            {
                return Err("Stock anchors must be upright and unit scale; arbitrary anchor pitch/roll and scaling need a verified transform adapter".into());
            }
            (p.position, p.rotation.y, Some(anchor))
        }
        Composition::Standalone {
            pivot,
            authoring_yaw,
        } => {
            if !pivot.valid() || !authoring_yaw.is_finite() || authoring_yaw.abs() > 360000. {
                return Err("Invalid authoring pivot".into());
            }
            (pivot, authoring_yaw, None)
        }
    };
    let delta = yaw - authoring_yaw;
    let transform = |v: Vector| v.sub(pivot).yaw(delta).add(position);
    let mut prefabs = Vec::with_capacity(content.prefabs.len());
    for (i, p) in content.prefabs.iter().enumerate() {
        if Some(i) == anchor {
            continue;
        }
        let mut child = p.clone();
        child.position = transform(p.position);
        child.rotation.y = (p.rotation.y + delta).rem_euclid(360.);
        if !child.position.valid() || !child.rotation.valid() {
            return Err("Composed transform exceeds bounds".into());
        }
        prefabs.push(child);
    }
    let mut paths = content.paths.clone();
    for p in &mut paths {
        for node in &mut p.nodes {
            *node = transform(*node);
            if !node.valid() {
                return Err("Composed path exceeds bounds".into());
            }
        }
    }
    Ok(Composed {
        prefabs,
        paths,
        anchor_id: anchor.map(|i| content.prefabs[i].id),
    })
}

/// Native import paths use this bounded reader rather than File::read_to_end without limits.
pub fn read_source(mut source: impl Read) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    source
        .by_ref()
        .take(MAX_SOURCE as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_SOURCE {
        return Err("Content source exceeds 64 MiB".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    fn v(out: &mut Vec<u8>, mut n: u64) {
        while n >= 128 {
            out.push(n as u8 | 128);
            n >>= 7;
        }
        out.push(n as u8);
    }
    fn number(out: &mut Vec<u8>, field: u64, n: u64) {
        v(out, field << 3);
        v(out, n);
    }
    fn b(out: &mut Vec<u8>, field: u64, data: &[u8]) {
        v(out, field << 3 | 2);
        v(out, data.len() as u64);
        out.extend(data);
    }
    fn vector_bytes(x: f32, y: f32, z: f32) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (field, n) in [(1, x), (2, y), (3, z)] {
            v(&mut bytes, field << 3 | 5);
            bytes.extend(n.to_le_bytes());
        }
        bytes
    }
    pub fn fixture() -> Vec<u8> {
        let mut world = Vec::new();
        number(&mut world, 1, 1000);
        for (id, x) in [(100, 0.), (200, 10.)] {
            let mut p = Vec::new();
            b(&mut p, 1, b"Monument");
            number(&mut p, 2, id);
            b(&mut p, 3, &vector_bytes(x, 0., 0.));
            b(&mut p, 5, &vector_bytes(1., 1., 1.));
            b(&mut world, 3, &p);
        }
        let mut layer = Vec::new();
        b(&mut layer, 1, b"custom-editor-data");
        b(&mut layer, 2, b"preserve-me");
        b(&mut world, 2, &layer);
        let mut map = 10_u32.to_le_bytes().to_vec();
        map.extend(0_i64.to_le_bytes());
        v(&mut map, 0);
        v(&mut map, world.len() as u64);
        map.extend(world);
        map
    }
    #[test]
    fn reads_raw_and_compressed_blocks_and_preserves_layers() {
        let raw = fixture();
        let map = read_map(&raw, || false).unwrap();
        assert_eq!(map.prefabs.len(), 2);
        assert_eq!(map.layers[0].data, b"preserve-me");
        let mut r = Proto::new(&raw[12..]);
        r.varint().unwrap();
        let len = r.varint().unwrap() as usize;
        let block = r.take(len).unwrap();
        let compressed = lz4_flex::block::compress(block);
        let mut encoded = raw[..12].to_vec();
        v(&mut encoded, 1);
        v(&mut encoded, len as u64);
        v(&mut encoded, compressed.len() as u64);
        encoded.extend(compressed);
        assert_eq!(read_map(&encoded, || false).unwrap().prefabs, map.prefabs);
        encoded[12] = 3;
        assert_eq!(read_map(&encoded, || false).unwrap().prefabs, map.prefabs);
        let mut high_raw = raw;
        high_raw[12] = 2;
        assert_eq!(read_map(&high_raw, || false).unwrap().prefabs, map.prefabs);
    }
    #[test]
    fn source_origin_anchor_and_rotation_do_not_duplicate_stock_monument() {
        let map = read_map(&fixture(), || false).unwrap();
        let original = map.prefabs.clone();
        let result = compose(
            &map,
            Composition::AddOn { anchor: 0 },
            Vector {
                x: 100.,
                y: 20.,
                z: 200.,
            },
            90.,
        )
        .unwrap();
        assert_eq!(result.anchor_id, Some(100));
        assert_eq!(result.prefabs.len(), 1);
        assert!((result.prefabs[0].position.x - 100.).abs() < 0.001);
        assert!((result.prefabs[0].position.z - 190.).abs() < 0.001);
        assert_eq!(result.prefabs[0].position.y, 20.);
        assert_eq!(map.prefabs, original);
        let result = compose(
            &map,
            Composition::Standalone {
                pivot: Vector::default(),
                authoring_yaw: 0.,
            },
            Vector::default(),
            0.,
        )
        .unwrap();
        assert_eq!(result.prefabs.len(), 2);
    }
    #[test]
    fn rejects_truncation_bad_transforms_and_unverified_prefab_formats() {
        let source = fixture();
        for end in 0..source.len() {
            assert!(read_map(&source[..end], || false).is_err());
        }
        assert!(read_map(b"RustEdit unknown prefab", || false).is_err());
        assert!(read_map(&source, || true).is_err());
        let mut map = read_map(&source, || false).unwrap();
        map.prefabs[0].rotation.x = 30.;
        assert!(
            compose(
                &map,
                Composition::AddOn { anchor: 0 },
                Vector::default(),
                0.
            )
            .is_err()
        );
        assert!(
            compose(
                &map,
                Composition::Standalone {
                    pivot: Vector::default(),
                    authoring_yaw: 0.
                },
                Vector {
                    x: f32::NAN,
                    y: 0.,
                    z: 0.
                },
                0.
            )
            .is_err()
        );
    }
    #[test]
    fn overflow_varints_and_unsupported_flags_are_rejected() {
        let mut p = Proto::new(&[255; 10]);
        assert!(p.varint().is_err());
        let mut source = fixture();
        source[12] = 4;
        assert!(read_map(&source, || false).is_err());
        assert!(read_source(Cursor::new(vec![0; MAX_SOURCE + 1])).is_err());
    }

    #[test]
    fn rotated_nonzero_anchor_transforms_paths_and_preserves_child_details() {
        let mut map = read_map(&fixture(), || false).unwrap();
        map.prefabs[0].position = Vector {
            x: 10.,
            y: 2.,
            z: 30.,
        };
        map.prefabs[0].rotation.y = 90.;
        map.prefabs[1].position = Vector {
            x: 10.,
            y: 5.,
            z: 20.,
        };
        map.prefabs[1].rotation = Vector {
            x: 15.,
            y: 110.,
            z: 25.,
        };
        map.prefabs[1].scale = Vector {
            x: 2.,
            y: 3.,
            z: 4.,
        };
        map.paths.push(Path {
            inner_padding: 0.,
            outer_padding: 0.,
            inner_fade: 0.,
            outer_fade: 0.,
            random_scale: 0.,
            mesh_offset: 0.,
            terrain_offset: 0.,
            hierarchy: 0,
            name: "road".into(),
            nodes: vec![map.prefabs[1].position],
            width: 8.,
            spline: true,
            start: false,
            end: true,
            splat: 2,
            topology: 1024,
            unknown_fields: 1,
        });
        let result = compose(
            &map,
            Composition::AddOn { anchor: 0 },
            Vector {
                x: 100.,
                y: 20.,
                z: 200.,
            },
            0.,
        )
        .unwrap();
        let child = &result.prefabs[0];
        assert!((child.position.x - 110.).abs() < 0.001);
        assert!((child.position.z - 200.).abs() < 0.001);
        assert_eq!(child.position.y, 23.);
        assert_eq!(
            child.rotation,
            Vector {
                x: 15.,
                y: 20.,
                z: 25.
            }
        );
        assert_eq!(child.scale, map.prefabs[1].scale);
        assert_eq!(result.paths[0].nodes[0], child.position);
        let mut expected = map.paths[0].clone();
        expected.nodes = result.paths[0].nodes.clone();
        assert_eq!(result.paths[0], expected);
    }
}
