//! Full-resolution native terrain transport. It contains no proprietary algorithm or assets.
use std::io::Read;

pub const VERSION: &str = "papyrus-native-layers-1";
/// Borrowed terrain channels. Live placement can expose its private destination
/// without copying full grids or changing the immutable cached source.
#[derive(Clone, Copy)]
pub struct View<'a> {
    pub size: u32,
    pub height_resolution: usize,
    pub surface_resolution: usize,
    pub height: &'a [i16],
    pub biome: &'a [u8],
    pub splat: &'a [u8],
    pub topology: &'a [i32],
    pub placement_blockers: Option<&'a [u8]>,
    pub topology_tiles: Option<crate::topology_query::Tiles<'a>>,
}
impl View<'_> {
    /// Placement is a separate boolean map; missing legacy maps impose no restriction.
    pub fn placement_blocked(self, u: f32, v: f32) -> bool {
        let Some(cells) = self.placement_blockers else {
            return false;
        };
        let n = self.surface_resolution;
        if n == 0 || cells.len() != n.saturating_mul(n) || !u.is_finite() || !v.is_finite() {
            return true;
        }
        let x = (u * n as f32).clamp(0., (n - 1) as f32) as usize;
        let z = (v * n as f32).clamp(0., (n - 1) as f32) as usize;
        cells[z * n + x] != 0
    }

    /// Use an already resolved index in this view's surface grid.
    pub(crate) fn placement_blocked_at(self, cell: usize) -> bool {
        self.placement_blockers.is_some_and(|cells| {
            cells.len() != self.surface_resolution.saturating_mul(self.surface_resolution)
                || cells.get(cell).is_none_or(|&value| value != 0)
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layers {
    pub size: u32,
    pub terrain_seed: u32,
    pub biome_seed: u32,
    pub height_resolution: usize,
    pub surface_resolution: usize,
    pub height: Vec<i16>,
    pub biome: Vec<u8>,
    pub splat: Vec<u8>,
    pub topology: Vec<i32>,
    pub normalized_biome_cells: usize,
    pub normalized_splat_cells: usize,
}
impl Layers {
    pub fn view(&self) -> View<'_> {
        View {
            size: self.size,
            height_resolution: self.height_resolution,
            surface_resolution: self.surface_resolution,
            height: &self.height,
            biome: &self.biome,
            splat: &self.splat,
            topology: &self.topology,
            placement_blockers: None,
            topology_tiles: None,
        }
    }
    pub fn bytes(&self) -> usize {
        self.height.len() * 2 + self.biome.len() + self.splat.len() + self.topology.len() * 4
    }
    pub fn sample_heights(&self, output: usize) -> Result<Vec<i16>, String> {
        if !(2..=513).contains(&output)
            || !(33..=4097).contains(&self.height_resolution)
            || self.height.len() != self.height_resolution * self.height_resolution
        {
            return Err("Invalid native height sample resolution".into());
        }
        let n = self.height_resolution;
        if output == n {
            return Ok(self.height.clone());
        }
        // Both axes use the same sample coordinates. Resolve each division
        // once, then gather the requested vertices one source row at a time.
        let mut offsets = [0; 513];
        let offsets = &mut offsets[..output];
        for (index, offset) in offsets.iter_mut().enumerate() {
            *offset = index * (n - 1) / (output - 1);
        }
        let mut heights = Vec::with_capacity(output * output);
        for &z in offsets.iter() {
            let row = &self.height[z * n..(z + 1) * n];
            heights.extend(offsets.iter().map(|&x| row[x]));
        }
        Ok(heights)
    }
}
pub fn dimensions(size: u32) -> Result<(usize, usize), String> {
    if !(1000..=6000).contains(&size) {
        return Err("Invalid native world size".into());
    }
    let size = size as usize;
    let upper = size.next_power_of_two();
    Ok((
        (if size - upper / 2 < upper - size {
            upper / 2
        } else {
            upper
        })
        .min(4096)
            + 1,
        (size / 2).next_power_of_two().min(2048),
    ))
}
fn exact(
    reader: &mut impl Read,
    bytes: &mut [u8],
    cancel: &impl Fn() -> bool,
) -> Result<(), String> {
    for chunk in bytes.chunks_mut(65536) {
        if cancel() {
            return Err("Native layer import cancelled".into());
        }
        reader
            .read_exact(chunk)
            .map_err(|e| format!("Truncated native layers: {e}"))?;
    }
    Ok(())
}
fn normalize(
    data: &mut [u8],
    channels: usize,
    cancel: &impl Fn() -> bool,
) -> Result<usize, String> {
    let cells = data.len() / channels;
    // Accumulate small runs of each contiguous channel into the same scratch
    // buffer. This keeps validation of the common already-normalized cells in
    // sequential loops, leaving individual writes to the cells that need them.
    let mut sums = [0_u16; 4096];
    let mut changed = 0;
    for start in (0..cells).step_by(sums.len()) {
        if cancel() {
            return Err("Native layer import cancelled".into());
        }
        let end = (start + sums.len()).min(cells);
        let sums = &mut sums[..end - start];
        sums.fill(0);
        for channel in data.chunks_exact(cells) {
            for (sum, &value) in sums.iter_mut().zip(&channel[start..end]) {
                *sum += u16::from(value);
            }
        }
        for (offset, &sum) in sums.iter().enumerate() {
            if sum == 255 {
                continue;
            }
            let i = start + offset;
            changed += 1;
            if sum == 0 {
                data[i] = 255;
                continue;
            }
            let mut remaining = 255;
            for c in 0..channels {
                let value = if c == channels - 1 {
                    remaining
                } else {
                    u32::from(data[c * cells + i]) * 255 / u32::from(sum)
                };
                data[c * cells + i] = value as u8;
                remaining -= value;
            }
        }
    }
    Ok(changed)
}
fn require_normalized<const N: usize>(data: &[u8], cancel: &impl Fn() -> bool) -> Result<(), String> {
    let cells = data.len() / N;
    for start in (0..cells).step_by(4096) {
        if cancel() {
            return Err("Native layer import cancelled".into());
        }
        let end = (start + 4096).min(cells);
        let channels: [_; N] = std::array::from_fn(|c| &data[c * cells + start..c * cells + end]);
        // Combine mismatches across a block so the sum has no per-cell branch.
        let mut invalid = 0;
        for i in 0..end - start {
            let total = channels.iter().map(|channel| u16::from(channel[i])).sum::<u16>();
            invalid |= total ^ 255;
        }
        if invalid != 0 {
            return Err("Invalid normalized native surface weights".into());
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum SurfaceWeights {
    Normalize,
    RequireNormalized,
    Raw,
}

pub fn read(reader: impl Read, cancel: impl Fn() -> bool) -> Result<Layers, String> {
    read_impl(reader, cancel, SurfaceWeights::Normalize)
}

/// Read an immutable terrain snapshot whose surface channels already sum to 255.
pub fn read_normalized(reader: impl Read, cancel: impl Fn() -> bool) -> Result<Layers, String> {
    read_impl(reader, cancel, SurfaceWeights::RequireNormalized)
}
/// Retain independently quantized final map channels exactly, including sums
/// that differ from 255. Native generation imports use `read` to normalize.
pub fn read_raw(reader: impl Read, cancel: impl Fn() -> bool) -> Result<Layers, String> {
    read_impl(reader, cancel, SurfaceWeights::Raw)
}
fn read_impl(
    mut reader: impl Read,
    cancel: impl Fn() -> bool,
    surface: SurfaceWeights,
) -> Result<Layers, String> {
    let mut header = [0; 24];
    exact(&mut reader, &mut header, &cancel)?;
    if &header[..4] != b"PNF1" {
        return Err("Unsupported native full-layer transport".into());
    }
    let words: Vec<_> = header[4..]
        .chunks_exact(4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    let (size, terrain_seed, biome_seed) = (words[0], words[1], words[2]);
    let (hr, sr) = dimensions(size)?;
    if words[3] as usize != hr
        || words[4] as usize != sr
        || terrain_seed == 0
        || terrain_seed > i32::MAX as u32
        || biome_seed == 0
        || biome_seed > i32::MAX as u32
    {
        return Err("Invalid native full-layer dimensions or seeds".into());
    }
    let mut height = vec![0_i16; hr * hr];
    for chunk in height.chunks_mut(32768) {
        exact(&mut reader, bytemuck::cast_slice_mut(chunk), &cancel)?;
        #[cfg(target_endian = "big")]
        for word in &mut *chunk {
            *word = i16::from_le(*word);
        }
        if chunk.iter().any(|h| !(0..=32766).contains(h)) {
            return Err("Invalid native full-resolution height".into());
        }
    }
    let cells = sr * sr;
    let mut biome = vec![0; cells * 5];
    exact(&mut reader, &mut biome, &cancel)?;
    if matches!(surface, SurfaceWeights::RequireNormalized) {
        require_normalized::<5>(&biome, &cancel)?;
    }
    let mut splat = vec![0; cells * 8];
    exact(&mut reader, &mut splat, &cancel)?;
    if matches!(surface, SurfaceWeights::RequireNormalized) {
        require_normalized::<8>(&splat, &cancel)?;
    }
    let mut topology = vec![0_i32; cells];
    for chunk in topology.chunks_mut(16384) {
        exact(&mut reader, bytemuck::cast_slice_mut(chunk), &cancel)?;
        #[cfg(target_endian = "big")]
        for word in chunk {
            *word = i32::from_le(*word);
        }
    }
    let mut end = [0];
    if reader.read(&mut end).map_err(|e| e.to_string())? != 0 {
        return Err("Trailing native full-layer data".into());
    }
    let (normalized_biome_cells, normalized_splat_cells) = if matches!(surface, SurfaceWeights::Normalize) {
        (normalize(&mut biome, 5, &cancel)?, normalize(&mut splat, 8, &cancel)?)
    } else {
        (0, 0)
    };
    Ok(Layers {
        size,
        terrain_seed,
        biome_seed,
        height_resolution: hr,
        surface_resolution: sr,
        height,
        biome,
        splat,
        topology,
        normalized_biome_cells,
        normalized_splat_cells,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Vec<u8> {
        let (hr, sr) = dimensions(1000).unwrap();
        let cells = sr * sr;
        let mut bytes = b"PNF1".to_vec();
        for w in [1000, 123, 456, hr as u32, sr as u32] {
            bytes.extend(w.to_le_bytes());
        }
        for i in 0..hr * hr {
            bytes.extend(((i % 32767) as i16).to_le_bytes());
        }
        let mut biome = vec![0; cells * 5];
        biome[..cells].fill(128);
        biome[cells..2 * cells].fill(128);
        bytes.extend(biome);
        bytes.resize(bytes.len() + cells * 8, 0);
        for _ in 0..cells {
            bytes.extend(0x80000180_u32.to_le_bytes());
        }
        bytes
    }
    #[test]
    fn raw_final_layers_keep_quantized_weights_and_zero_channels_exactly() {
        let bytes=fixture();let layers=read_raw(bytes.as_slice(),||false).unwrap();let cells=512*512;
        assert_eq!(layers.biome[0],128);assert_eq!(layers.biome[cells],128);
        assert!(layers.splat.iter().all(|&value|value==0));
        assert_eq!(layers.normalized_biome_cells,0);assert_eq!(layers.normalized_splat_cells,0);
        assert!(read_raw(bytes.as_slice(),||true).is_err());
        assert!(read_raw(&bytes[..bytes.len()-1],||false).is_err());
    }
    #[test]
    fn immutable_surfaces_check_weights_before_reading_the_remaining_layers() {
        let mut bytes = fixture();
        let (hr, sr) = dimensions(1000).unwrap();
        let cells = sr * sr;
        let biome = 24 + hr * hr * 2;
        let splat = biome + cells * 5;
        {
            let mut input = std::io::Cursor::new(&bytes);
            assert_eq!(
                read_normalized(&mut input, || false).err().as_deref(),
                Some("Invalid normalized native surface weights")
            );
            assert_eq!(input.position(), splat as u64);
        }
        bytes[biome..biome + cells].fill(127);
        {
            let mut input = std::io::Cursor::new(&bytes);
            assert_eq!(
                read_normalized(&mut input, || false).err().as_deref(),
                Some("Invalid normalized native surface weights")
            );
            assert_eq!(input.position(), (splat + cells * 8) as u64);
        }
        bytes[splat..splat + cells].fill(255);
        let layers = read_normalized(bytes.as_slice(), || false).unwrap();
        assert_eq!(layers.normalized_biome_cells, 0);
        assert_eq!(layers.normalized_splat_cells, 0);
        assert_eq!(layers.biome, &bytes[biome..splat]);
        assert_eq!(layers.splat, &bytes[splat..splat + cells * 8]);
    }
    #[test]
    fn full_resolution_layers_are_preserved_and_weights_normalized() {
        let layers = read(fixture().as_slice(), || false).unwrap();
        let cells = 512 * 512;
        assert_eq!(layers.height_resolution, 1025);
        assert_eq!(layers.surface_resolution, 512);
        assert_eq!(layers.height[32768], 1);
        assert_eq!(*layers.height.last().unwrap(), ((1025 * 1025 - 1) % 32767) as i16);
        assert_eq!(layers.topology[0] as u32, 0x80000180);
        assert_eq!(*layers.topology.last().unwrap() as u32, 0x80000180);
        assert_eq!(layers.normalized_biome_cells, cells);
        assert_eq!(layers.normalized_splat_cells, cells);
        for i in [0, 123, cells - 1] {
            assert_eq!(
                (0..5)
                    .map(|c| u32::from(layers.biome[c * cells + i]))
                    .sum::<u32>(),
                255
            );
            assert_eq!(layers.splat[i], 255);
        }
        let sample = layers.sample_heights(513).unwrap();
        assert_eq!(sample[1], layers.height[2]);
        assert_eq!(sample[512 * 513 + 512], *layers.height.last().unwrap());
    }
    #[test]
    fn height_samples_keep_integer_grid_coordinates_for_any_output_size() {
        let mut layers = read(fixture().as_slice(), || false).unwrap();
        for n in [33, 513, 1025] {
            layers.height_resolution = n;
            layers.height = (0..n * n).map(|index| (index % 32767) as i16).collect();
            for output in [2, 33, 104, 257, 512, 513] {
                let expected: Vec<_> = (0..output * output)
                    .map(|i| {
                        layers.height[(i / output) * (n - 1) / (output - 1) * n
                            + (i % output) * (n - 1) / (output - 1)]
                    })
                    .collect();
                assert_eq!(layers.sample_heights(output).unwrap(), expected, "{n} to {output}");
            }
        }
    }
    #[test]
    fn malformed_or_cancelled_inputs_are_rejected_before_unbounded_allocation() {
        let mut bytes = fixture();
        assert!(read(&bytes[..23], || false).is_err());
        assert!(read(bytes.as_slice(), || true).is_err());
        bytes[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(read(bytes.as_slice(), || false).is_err());
        bytes[16..20].copy_from_slice(&1025_u32.to_le_bytes());
        bytes[24..26].copy_from_slice(&(-1_i16).to_le_bytes());
        assert!(read(bytes.as_slice(), || false).is_err());
        bytes[24..26].fill(0);
        bytes.push(0);
        assert!(read(bytes.as_slice(), || false).is_err());
        bytes.truncate(bytes.len() - 2);
        assert!(read(bytes.as_slice(), || false).is_err());
        assert!(dimensions(u32::MAX).is_err());
    }
}
