//! Version-10 map serialization. Layer bytes stream through bounded legacy LZ4 blocks.
//! Protobuf field numbers follow the retained independent C# codec.
use rayon::prelude::*;
use std::{
    collections::HashSet,
    io::{self, Read, Write},
    time::Instant,
};

const BLOCK: usize = 1024 * 1024;
const MAX_BYTES: u64 = 512 * 1024 * 1024;

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn varint(out: &mut impl Write, mut value: u64) -> io::Result<()> {
    while value >= 128 {
        out.write_all(&[(value as u8 & 127) | 128])?;
        value >>= 7;
    }
    out.write_all(&[value as u8])
}
pub(crate) fn bytes(out: &mut impl Write, field: u8, value: &[u8]) -> io::Result<()> {
    varint(out, (u64::from(field) << 3) | 2)?;
    varint(out, value.len() as u64)?;
    out.write_all(value)
}
pub(crate) fn number(out: &mut impl Write, field: u8, value: u64) -> io::Result<()> {
    if value != 0 {
        varint(out, u64::from(field) << 3)?;
        varint(out, value)?;
    }
    Ok(())
}
pub(crate) fn float(out: &mut impl Write, field: u8, value: f32) -> io::Result<()> {
    if value != 0.0 {
        varint(out, (u64::from(field) << 3) | 5)?;
        out.write_all(&value.to_le_bytes())?;
    }
    Ok(())
}
fn varint_size(mut value: u64) -> u64 {
    let mut n = 1;
    while value >= 128 {
        value >>= 7;
        n += 1;
    }
    n
}

pub(crate) fn valid_heights(bytes: &[u8]) -> bool {
    let mut words = bytes.chunks_exact(8);
    // Four little-endian u16 samples per word: reject a sign bit or the reserved 32767.
    // The packed zero test reports whether any lane equals 32767 after XOR.
    let invalid = words.any(|word| {
        let bits = u64::from_le_bytes(word.try_into().expect("eight-byte chunk"));
        let x = bits ^ 0x7fff_7fff_7fff_7fff;
        bits & 0x8000_8000_8000_8000 != 0
            || x.wrapping_sub(0x0001_0001_0001_0001) & !x & 0x8000_8000_8000_8000 != 0
    });
    !invalid
        && words
            .remainder()
            .chunks_exact(2)
            .all(|v| u16::from_le_bytes([v[0], v[1]]) <= 32766)
}

struct CompressionBlock {
    raw: Vec<u8>,
    encoded: Vec<u8>,
    encoded_length: usize,
}
impl CompressionBlock {
    fn new() -> Self {
        Self {
            raw: Vec::with_capacity(BLOCK),
            encoded: vec![0; lz4_flex::block::get_maximum_output_size(BLOCK)],
            encoded_length: 0,
        }
    }

    fn compress(&mut self) -> io::Result<()> {
        self.encoded_length = lz4_flex::block::compress_into(&self.raw, &mut self.encoded)
            .map_err(|e| invalid(&e.to_string()))?;
        Ok(())
    }
}

pub(crate) struct Legacy<'a, W> {
    inner: W,
    pool: Option<&'a rayon::ThreadPool>,
    batch_size: usize,
    blocks: Vec<CompressionBlock>,
    pending: usize,
    written: u64,
    compression_ms: f64,
    output_ms: f64,
}
impl<'a, W: Write> Legacy<'a, W> {
    fn new(inner: W, pool: Option<&'a rayon::ThreadPool>) -> Self {
        // Independent legacy blocks need no cross-block dictionary. Bound
        // scratch space to four input and four compressed buffers.
        let batch_size = pool.map_or(1, |pool| pool.current_num_threads().min(4));
        Self {
            inner,
            pool,
            batch_size,
            blocks: Vec::with_capacity(batch_size),
            pending: 0,
            written: 0,
            compression_ms: 0.0,
            output_ms: 0.0,
        }
    }
    fn write_pending(&mut self) -> io::Result<()> {
        if self.pending == 0 {
            return Ok(());
        }
        let clock = Instant::now();
        let blocks = &mut self.blocks[..self.pending];
        if let Some(pool) = self.pool
            && blocks.len() > 1
        {
            pool.install(|| {
                blocks
                    .par_iter_mut()
                    .try_for_each(CompressionBlock::compress)
            })?;
        } else {
            for block in blocks.iter_mut() {
                block.compress()?;
            }
        }
        self.compression_ms += clock.elapsed().as_secs_f64() * 1000.0;
        let clock = Instant::now();
        for block in blocks {
            let compressed = block.encoded_length < block.raw.len();
            varint(&mut self.inner, u64::from(compressed))?;
            varint(&mut self.inner, block.raw.len() as u64)?;
            if compressed {
                varint(&mut self.inner, block.encoded_length as u64)?;
                self.inner
                    .write_all(&block.encoded[..block.encoded_length])?;
            } else {
                self.inner.write_all(&block.raw)?;
            }
            block.raw.clear();
        }
        self.pending = 0;
        self.output_ms += clock.elapsed().as_secs_f64() * 1000.0;
        Ok(())
    }
    fn flush_blocks(&mut self) -> io::Result<()> {
        if self
            .blocks
            .get(self.pending)
            .is_some_and(|block| !block.raw.is_empty())
        {
            self.pending += 1;
        }
        self.write_pending()?;
        let clock = Instant::now();
        self.inner.flush()?;
        self.output_ms += clock.elapsed().as_secs_f64() * 1000.0;
        Ok(())
    }
    pub(crate) fn finish(mut self) -> io::Result<Profile> {
        self.flush_blocks()?;
        Ok(Profile {
            compression_ms: self.compression_ms,
            output_ms: self.output_ms,
            ..Profile::default()
        })
    }
}

pub(crate) fn begin_v10(
    mut output: impl Write,
    timestamp: u64,
    size: u64,
    pool: Option<&rayon::ThreadPool>,
) -> io::Result<Legacy<'_, impl Write>> {
    output.write_all(&10_u32.to_le_bytes())?;
    output.write_all(&timestamp.to_le_bytes())?;
    let mut out = Legacy::new(output, pool);
    number(&mut out, 1, size)?;
    Ok(out)
}

pub(crate) fn layer_header(out: &mut impl Write, name: &str, length: usize) -> io::Result<()> {
    let nested = 1
        + varint_size(name.len() as u64)
        + name.len() as u64
        + 1
        + varint_size(length as u64)
        + length as u64;
    varint(out, 18)?;
    varint(out, nested)?;
    bytes(out, 1, name.as_bytes())?;
    varint(out, 18)?;
    varint(out, length as u64)
}

pub(crate) fn vector(out: &mut impl Write, field: u8, coordinates: [f32; 3]) -> io::Result<()> {
    let mut encoded = [0; 15];
    let mut remaining = encoded.as_mut_slice();
    for (index, value) in coordinates.into_iter().enumerate() {
        if !value.is_finite() {
            return Err(invalid("nonfinite export float"));
        }
        float(&mut remaining, index as u8 + 1, value)?;
    }
    let length = 15 - remaining.len();
    bytes(out, field, &encoded[..length])
}
impl<W: Write> Write for Legacy<'_, W> {
    fn write(&mut self, mut bytes: &[u8]) -> io::Result<usize> {
        let total = bytes.len();
        self.written += total as u64;
        if self.written > MAX_BYTES {
            return Err(invalid("export protobuf exceeds 512 MiB"));
        }
        while !bytes.is_empty() {
            if self.pending == self.blocks.len() {
                self.blocks.push(CompressionBlock::new());
            }
            let block = &mut self.blocks[self.pending];
            let n = bytes.len().min(BLOCK - block.raw.len());
            block.raw.extend_from_slice(&bytes[..n]);
            bytes = &bytes[n..];
            if block.raw.len() == BLOCK {
                self.pending += 1;
                if self.pending == self.batch_size {
                    self.write_pending()?;
                }
            }
        }
        Ok(total)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.flush_blocks()
    }
}

struct Input<R> {
    inner: R,
    consumed: u64,
    read_ms: f64,
}
impl<R: Read> Input<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<()> {
        self.consumed += bytes.len() as u64;
        if self.consumed > MAX_BYTES {
            return Err(invalid("export input exceeds 512 MiB"));
        }
        let clock = Instant::now();
        let result = self.inner.read_exact(bytes);
        self.read_ms += clock.elapsed().as_secs_f64() * 1000.0;
        result
    }
    fn word(&mut self) -> io::Result<u32> {
        let mut b = [0; 4];
        self.read(&mut b)?;
        Ok(u32::from_le_bytes(b))
    }
    fn count(&mut self, max: u32) -> io::Result<usize> {
        let n = self.word()?;
        if n > max {
            return Err(invalid("export count exceeds limit"));
        }
        Ok(n as usize)
    }
    fn text<'a>(&mut self, buffer: &'a mut Vec<u8>) -> io::Result<&'a str> {
        let n = self.count(4096)?;
        buffer.resize(n, 0);
        self.read(buffer)?;
        let text = std::str::from_utf8(buffer).map_err(|_| invalid("invalid export UTF-8"))?;
        if text.contains('\0') {
            return Err(invalid("invalid export text"));
        }
        Ok(text)
    }
    fn float(&mut self) -> io::Result<f32> {
        let value = f32::from_bits(self.word()?);
        if !value.is_finite() {
            return Err(invalid("nonfinite export float"));
        }
        Ok(value)
    }
    fn boolean(&mut self) -> io::Result<bool> {
        match self.word()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid("invalid export boolean")),
        }
    }
    fn vector(&mut self, out: &mut impl Write, field: u8) -> io::Result<()> {
        let mut raw = [0; 12];
        self.read(&mut raw)?;
        vector(
            out,
            field,
            std::array::from_fn(|index| {
                let offset = index * 4;
                f32::from_le_bytes(
                    raw[offset..offset + 4]
                        .try_into()
                        .expect("four-byte coordinate"),
                )
            }),
        )
    }
}

/// Wall-clock measurements of streaming work, including time blocked on input/output.
#[derive(Default, Debug)]
pub struct Profile {
    pub compression_ms: f64,
    pub output_ms: f64,
    pub read_ms: f64,
    pub validation_ms: f64,
    pub total_ms: f64,
}

/// MAPX v1/v2 require normalized weights. V3 carries source-authored raw byte
/// planes with the full v2 record/path layout. Output is always version 10.
/// Callers publish only after successful EOF/exit.
pub fn encode(input: impl Read, output: impl Write) -> io::Result<Profile> {
    encode_with_pool(input, output, None)
}

pub(crate) fn encode_with_pool(
    input: impl Read,
    output: impl Write,
    pool: Option<&rayon::ThreadPool>,
) -> io::Result<Profile> {
    let clock = Instant::now();
    let mut validation_ms = 0.0;
    let mut r = Input {
        inner: input,
        consumed: 0,
        read_ms: 0.0,
    };
    if r.word()? != u32::from_le_bytes(*b"MAPX") {
        return Err(invalid("invalid export header"));
    }
    let format = r.word()?;
    if ![1, 2, 3].contains(&format) {
        return Err(invalid("invalid export header"));
    }
    let timestamp = u64::from(r.word()?) | (u64::from(r.word()?) << 32);
    let size = r.word()?;
    let count = r.count(64)?;
    if timestamp > i64::MAX as u64 || !(1000..=6000).contains(&size) || count < 7 {
        return Err(invalid("invalid export timestamp, size or layers"));
    }
    let mut out = begin_v10(output, timestamp, u64::from(size), pool)?;
    let mut names = HashSet::new();
    let mut height_resolution = None;
    let mut surface_resolution = None;
    let mut buffer = vec![0; 65536];
    let mut text_buffer = Vec::new();
    for _ in 0..count {
        let name = r.text(&mut text_buffer)?;
        let resolution = r.word()? as usize;
        let channels = r.word()? as usize;
        let length = r.count(64 * 1024 * 1024)?;
        if name.is_empty() || !names.insert(name.to_owned()) {
            return Err(invalid("duplicate or empty export layer"));
        }
        let layer_format = match name {
            "height" | "terrain" | "water" => Some((1, 2, true)),
            "splat" => Some((8, 1, false)),
            "biome" => Some((5, 1, false)),
            "topology" => Some((1, 4, false)),
            "alpha" => Some((1, 1, false)),
            _ => None,
        };
        let mut sums = Vec::new();
        let mut cells = 0;
        if let Some((expected, bytes, height)) = layer_format {
            let valid = if height {
                (33..=4097).contains(&resolution) && (resolution - 1).is_power_of_two()
            } else {
                (32..=2048).contains(&resolution) && resolution.is_power_of_two()
            };
            if !valid
                || channels != expected
                || length != resolution * resolution * channels * bytes
            {
                return Err(invalid("invalid export layer dimensions"));
            }
            let previous = if height {
                &mut height_resolution
            } else {
                &mut surface_resolution
            };
            if previous.is_some_and(|v| v != resolution) {
                return Err(invalid("inconsistent export layer dimensions"));
            }
            *previous = Some(resolution);
            cells = resolution * resolution;
            if format < 3 && (name == "splat" || name == "biome") {
                sums = vec![0_u16; cells];
            }
        }
        layer_header(&mut out, name, length)?;
        let mut offset = 0;
        while offset < length {
            let n = (length - offset).min(buffer.len());
            let chunk = &mut buffer[..n];
            r.read(chunk)?;
            let validation_clock = Instant::now();
            if !sums.is_empty() {
                let mut source: &[u8] = chunk;
                let mut start = offset & (cells - 1);
                while !source.is_empty() {
                    let n = source.len().min(cells - start);
                    // Contiguous planes let LLVM vectorize additions without per-byte indexing.
                    for (sum, &value) in sums[start..start + n].iter_mut().zip(&source[..n]) {
                        *sum += u16::from(value);
                    }
                    source = &source[n..];
                    start = 0;
                }
            }
            if matches!(name, "height" | "terrain" | "water") && !valid_heights(chunk) {
                return Err(invalid("invalid export terrain height"));
            }
            validation_ms += validation_clock.elapsed().as_secs_f64() * 1000.0;
            out.write_all(chunk)?;
            offset += n;
        }
        if sums.iter().any(|&v| v != 255) {
            return Err(invalid("unnormalized export weights"));
        }
    }
    if [
        "height", "terrain", "water", "splat", "biome", "topology", "alpha",
    ]
    .iter()
    .any(|&s| !names.contains(s))
    {
        return Err(invalid("missing required export layer"));
    }
    let count = r.count(if format >= 2 { 65536 } else { 4096 })?;
    let mut record = Vec::new();
    for _ in 0..count {
        record.clear();
        bytes(&mut record, 1, r.text(&mut text_buffer)?.as_bytes())?;
        number(&mut record, 2, u64::from(r.word()?))?;
        for f in 3..=5 {
            r.vector(&mut record, f)?;
        }
        bytes(&mut out, 3, &record)?;
    }
    let count = r.count(4096)?;
    let mut total_nodes = 0;
    for _ in 0..count {
        record.clear();
        bytes(&mut record, 1, r.text(&mut text_buffer)?.as_bytes())?;
        let width = r.float()?;
        if width < 0.0 {
            return Err(invalid("invalid export path width"));
        }
        for f in 2..=4 {
            number(&mut record, f, u64::from(r.boolean()?))?;
        }
        float(&mut record, 5, width)?;
        if format >= 2 {
            for f in 6..=12 {
                float(&mut record, f, r.float()?)?;
            }
        }
        for f in 13..=14 {
            number(&mut record, f, r.word()? as i32 as i64 as u64)?;
        }
        let count = r.count(1048576)?;
        total_nodes += count;
        if count < 2 || total_nodes > 1048576 {
            return Err(invalid("invalid export node budget"));
        }
        for _ in 0..count {
            r.vector(&mut record, 15)?;
        }
        if format >= 2 {
            number(&mut record, 16, r.word()? as i32 as i64 as u64)?;
        }
        bytes(&mut out, 4, &record)?;
    }
    if r.inner.read(&mut [0])? != 0 {
        return Err(invalid("trailing export input"));
    }
    let mut result = out.finish()?;
    result.read_ms = r.read_ms;
    result.validation_ms = validation_ms;
    result.total_ms = clock.elapsed().as_secs_f64() * 1000.0;
    Ok(result)
}

/// Serialize generated arrays directly, without an MAPX round trip through the host.
pub fn generated(
    settings: crate::world::Settings,
    layers: &crate::world::Layers,
    registry: &crate::registry::Prepared,
    timestamp: u64,
    output: impl Write,
) -> io::Result<Profile> {
    let clock = Instant::now();
    settings.validate().map_err(|e| invalid(&e))?;
    registry.request.validate().map_err(|e| invalid(&e))?;
    let heights = settings.height_resolution().pow(2);
    let cells = settings.surface_resolution().pow(2);
    if layers.height.len() != heights
        || layers.water.len() != heights
        || layers.topology.len() != cells
        || layers.alpha.len() != cells
        || layers.prefabs.len() > 4096
        || layers.paths.len() > 4096
    {
        return Err(invalid("invalid generated map dimensions or budgets"));
    }
    let mut nodes = 0;
    for path in &layers.paths {
        nodes += path.points.len();
        if path.points.len() < 2
            || nodes > 1048576
            || !path.width.is_finite()
            || path.width <= 0.0
            || path
                .points
                .iter()
                .any(|p| !p.x.is_finite() || !p.z.is_finite())
        {
            return Err(invalid("invalid generated paths"));
        }
    }
    if timestamp > i64::MAX as u64 {
        return Err(invalid("invalid map timestamp"));
    }
    let mut out = begin_v10(output, timestamp, settings.size as u64, None)?;
    let mut buffer = vec![0; 65536];
    for (name, values) in [
        ("height", &layers.height),
        ("terrain", &layers.height),
        ("water", &layers.water),
    ] {
        layer_header(&mut out, name, values.len() * 2)?;
        for values in values.chunks(buffer.len() / 2) {
            let encoded = &mut buffer[..values.len() * 2];
            for (bytes, &value) in encoded.chunks_exact_mut(2).zip(values) {
                bytes.copy_from_slice(&value.to_le_bytes());
            }
            if !valid_heights(encoded) {
                return Err(invalid("invalid generated heights"));
            }
            out.write_all(encoded)?;
        }
    }
    let cells = settings.surface_resolution().pow(2);
    for (name, data, channels) in [("splat", &layers.splat, 8), ("biome", &layers.biome, 5)] {
        if data.len() != cells * channels {
            return Err(invalid("invalid generated weights"));
        }
        let mut sums = vec![0_u16; cells];
        for plane in data.chunks_exact(cells) {
            for (sum, &value) in sums.iter_mut().zip(plane) {
                *sum += u16::from(value);
            }
        }
        if sums.iter().any(|&v| v != 255) {
            return Err(invalid("unnormalized generated weights"));
        }
        layer_header(&mut out, name, data.len())?;
        out.write_all(data)?;
    }
    layer_header(&mut out, "topology", layers.topology.len() * 4)?;
    for values in layers.topology.chunks(buffer.len() / 4) {
        let encoded = &mut buffer[..values.len() * 4];
        for (bytes, &value) in encoded.chunks_exact_mut(4).zip(values) {
            bytes.copy_from_slice(&value.to_le_bytes());
        }
        out.write_all(encoded)?;
    }
    layer_header(&mut out, "alpha", layers.alpha.len())?;
    out.write_all(&layers.alpha)?;
    for p in &layers.prefabs {
        let id = *registry
            .ids
            .get(p.entry)
            .ok_or_else(|| invalid("invalid generated prefab index"))?;
        let mut prefab = Vec::new();
        bytes(&mut prefab, 1, b"Monument")?;
        number(&mut prefab, 2, u64::from(id))?;
        for (field, values) in [
            (3, [p.x, p.y, p.z]),
            (4, [0.0, p.yaw, 0.0]),
            (5, [1.0, 1.0, 1.0]),
        ] {
            let mut vector = Vec::new();
            for (index, value) in values.into_iter().enumerate() {
                if !value.is_finite() {
                    return Err(invalid("invalid generated prefab transform"));
                }
                float(&mut vector, index as u8 + 1, value)?;
            }
            bytes(&mut prefab, field, &vector)?;
        }
        bytes(&mut out, 3, &prefab)?;
    }
    for p in &layers.paths {
        let (name, splat, topology) = match p.kind {
            0 => ("River", 0, 16384),
            1 => ("Road", 1, 0),
            2 => ("Ring Road", 1, 0),
            _ => return Err(invalid("invalid generated path")),
        };
        let mut path = Vec::new();
        bytes(&mut path, 1, name.as_bytes())?;
        for f in 2..=4 {
            number(&mut path, f, 1)?;
        }
        float(&mut path, 5, p.width)?;
        number(&mut path, 13, splat)?;
        number(&mut path, 14, topology)?;
        for node in &p.points {
            let mut vector = Vec::new();
            float(&mut vector, 1, node.x)?;
            float(&mut vector, 3, node.z)?;
            bytes(&mut path, 15, &vector)?;
        }
        bytes(&mut out, 4, &path)?;
    }
    let mut result = out.finish()?;
    result.total_ms = clock.elapsed().as_secs_f64() * 1000.0;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_blocks_round_trip_compressed_and_incompressible_payloads() {
        fn read_var(r: &mut &[u8]) -> u64 {
            let mut v = 0;
            let mut shift = 0;
            loop {
                let b = r[0];
                *r = &r[1..];
                v |= u64::from(b & 127) << shift;
                if b < 128 {
                    return v;
                }
                shift += 7;
            }
        }
        for payload in [
            vec![0; BLOCK + 31],
            (0..BLOCK + 31)
                .map(|i| ((i as u32).wrapping_mul(1664525).wrapping_add(1013904223) >> 16) as u8)
                .collect(),
            vec![1, 2, 3, 4, 5],
        ] {
            let mut bytes = vec![];
            let mut stream = Legacy::new(&mut bytes, None);
            stream.write_all(&payload).unwrap();
            stream.finish().unwrap();
            let mut r = bytes.as_slice();
            let mut restored = vec![];
            while !r.is_empty() {
                let flags = read_var(&mut r);
                let n = read_var(&mut r) as usize;
                let m = if flags == 1 {
                    read_var(&mut r) as usize
                } else {
                    n
                };
                let block = &r[..m];
                restored.extend(if flags == 1 {
                    lz4_flex::block::decompress(block, n).unwrap()
                } else {
                    block.to_vec()
                });
                r = &r[m..];
            }
            assert_eq!(payload, restored);
        }
    }
    #[test]
    fn protobuf_handles_negative_int32_and_default_floats() {
        let mut bytes = vec![];
        number(&mut bytes, 13, (-1_i32) as i64 as u64).unwrap();
        float(&mut bytes, 1, -0.0).unwrap();
        assert_eq!(bytes.len(), 11);
        assert_eq!(bytes[0], 104);
        assert_eq!(*bytes.last().unwrap(), 1);
        let mut output = vec![];
        assert!(encode(&b"MAPX"[..], &mut output).is_err());
        assert!(output.is_empty());
    }

    fn fixture() -> Vec<u8> {
        let mut input = b"MAPX".to_vec();
        for v in [1_u32, 123456, 0, 1000, 7] {
            input.extend(v.to_le_bytes());
        }
        for (name, channels, width) in [
            ("height", 1, 2),
            ("terrain", 1, 2),
            ("water", 1, 2),
            ("splat", 8, 1),
            ("biome", 5, 1),
            ("topology", 1, 4),
            ("alpha", 1, 1),
        ] {
            let resolution: u32 = if width == 2 { 33 } else { 32 };
            input.extend((name.len() as u32).to_le_bytes());
            input.extend(name.as_bytes());
            for v in [
                resolution,
                channels,
                resolution * resolution * channels * width,
            ] {
                input.extend(v.to_le_bytes());
            }
            let mut data = vec![0; (resolution * resolution * channels * width) as usize];
            if name == "splat" || name == "biome" {
                data[..(resolution * resolution) as usize].fill(255);
            }
            input.extend(data);
        }
        input.extend([0; 8]);
        input
    }

    #[test]
    fn raw_byte_contract_preserves_splat_quantization_without_changing_legacy_validation() {
        let mut input = fixture();
        let mut normalized_v2 = input.clone();
        normalized_v2[4..8].copy_from_slice(&2_u32.to_le_bytes());
        let mut normalized_v3 = input.clone();
        normalized_v3[4..8].copy_from_slice(&3_u32.to_le_bytes());
        let (mut old, mut current) = (Vec::new(), Vec::new());
        encode(normalized_v2.as_slice(), &mut old).unwrap();
        encode(normalized_v3.as_slice(), &mut current).unwrap();
        assert_eq!(
            old, current,
            "Normalized v2 inputs must retain byte-identical v3 output"
        );
        let start = input.windows(5).position(|v| v == b"splat").unwrap() + 5 + 12;
        input[start] = 254;
        input[start + 1] = 250;
        input[start + 1024 + 2] = 4; // independently quantized channels, sum 259
        for version in [1_u32, 2] {
            input[4..8].copy_from_slice(&version.to_le_bytes());
            assert!(encode(input.as_slice(), io::sink()).is_err());
        }
        input[4..8].copy_from_slice(&3_u32.to_le_bytes());
        let mut output = Vec::new();
        encode(input.as_slice(), &mut output).unwrap();
        let map = crate::content::read_map(&output, || false).unwrap();
        let splat = map.layers.iter().find(|l| l.name == "splat").unwrap();
        assert_eq!(splat.data, &input[start..start + 8 * 1024]);
        let mut trailing = input.clone();
        trailing.push(0);
        assert!(encode(trailing.as_slice(), io::sink()).is_err());
        let mut invalid = input.clone();
        invalid[34..38].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(encode(invalid.as_slice(), io::sink()).is_err());
    }
    #[test]
    fn export_rejects_truncation_invalid_layers_weights_and_trailing_data() {
        let valid = fixture();
        let mut output = vec![];
        encode(valid.as_slice(), &mut output).unwrap();
        assert_eq!(&output[..4], &10_u32.to_le_bytes());
        assert_eq!(&output[4..12], &123456_u64.to_le_bytes());
        for n in [0, 4, 8, 16, 24, 46, valid.len() - 1] {
            assert!(encode(&valid[..n], io::sink()).is_err());
        }
        let mut trailing = valid.clone();
        trailing.push(0);
        let mut dimension = valid.clone();
        dimension[34..38].copy_from_slice(&u32::MAX.to_le_bytes());
        let mut height = valid.clone();
        height[46..48].fill(255);
        let mut weights = valid.clone();
        let i = weights.windows(5).position(|v| v == b"splat").unwrap() + 5 + 12;
        weights[i] = 254;
        let mut duplicate = valid.clone();
        let i = duplicate.windows(5).position(|v| v == b"alpha").unwrap();
        duplicate[i..i + 5].copy_from_slice(b"water");
        let mut count = valid.clone();
        let n = count.len();
        count[n - 8..n - 4].copy_from_slice(&4097_u32.to_le_bytes());
        for bad in [trailing, dimension, height, weights, duplicate, count] {
            assert!(encode(bad.as_slice(), io::sink()).is_err());
        }
    }

    #[test]
    fn protobuf_limit_is_enforced_before_compression() {
        let mut stream = Legacy::new(io::sink(), None);
        stream.written = MAX_BYTES;
        assert!(stream.write_all(&[1]).is_err());
        assert!(stream.blocks.is_empty());
    }

    #[test]
    fn parallel_blocks_match_serial_block_boundaries_and_output_order() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(3)
            .build()
            .unwrap();
        let mut expected = Vec::new();
        let mut actual = Vec::new();
        let zero = vec![0; BLOCK + 3];
        let noisy = (0..BLOCK * 3 + 17)
            .scan(0xf23b0543_u32, |state, _| {
                *state ^= *state << 13;
                *state ^= *state >> 17;
                *state ^= *state << 5;
                Some(*state as u8)
            })
            .collect::<Vec<_>>();
        for (output, workers) in [(&mut expected, None), (&mut actual, Some(&pool))] {
            let mut writer = Legacy::new(output, workers);
            writer.write_all(&zero).unwrap();
            writer.flush().unwrap();
            writer.write_all(&noisy).unwrap();
            writer.write_all(&zero).unwrap();
            writer.finish().unwrap();
        }
        assert_eq!(actual, expected);
    }

    #[test]
    fn packed_height_validation_matches_scalar_for_every_value_and_lane() {
        for value in 0..=u16::MAX {
            for lane in 0..4 {
                let mut bytes = [0xfe, 0x7f, 0, 0, 0xfe, 0x7f, 0, 0, 0, 0];
                bytes[lane * 2..lane * 2 + 2].copy_from_slice(&value.to_le_bytes());
                assert_eq!(
                    valid_heights(&bytes),
                    value <= 32766,
                    "value {value}, lane {lane}"
                );
            }
            assert_eq!(valid_heights(&value.to_le_bytes()), value <= 32766);
        }
    }
}
