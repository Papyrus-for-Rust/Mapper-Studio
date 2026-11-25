//! Session-owned render data for completed tabs. Large terrain grids stay on disk.
use papyrus_terrain::{monument_composition::Composed, native_layers};
use std::{
    fs::File,
    io::{BufReader, BufWriter, Read, Write},
    sync::Arc,
};

const MAGIC: &[u8; 8] = b"MIMG0001";
pub struct Snapshot {
    file: Arc<tempfile::TempPath>,
}
fn write(
    writer: &mut impl Write,
    bytes: &[u8],
    cancel: &impl Fn() -> bool,
) -> Result<bool, String> {
    for chunk in bytes.chunks(65536) {
        if cancel() {
            return Ok(false);
        }
        writer.write_all(chunk).map_err(|e| e.to_string())?;
    }
    Ok(true)
}
fn words<T: bytemuck::Pod, const N: usize>(
    writer: &mut impl Write,
    values: &[T],
    encode: impl Fn(T) -> [u8; N],
    cancel: &impl Fn() -> bool,
) -> Result<bool, String> {
    #[cfg(target_endian = "little")]
    {
        let _ = encode;
        write(writer, bytemuck::cast_slice(values), cancel)
    }
    #[cfg(target_endian = "big")]
    {
        for chunk in values.chunks(16384) {
            if cancel() {
                return Ok(false);
            }
            let bytes: Vec<_> = chunk.iter().flat_map(|&v| encode(v)).collect();
            writer.write_all(&bytes).map_err(|e| e.to_string())?;
        }
        Ok(true)
    }
}
/// Decode the historical full water plane without allocating it for dry maps.
/// Zero is both the dry sentinel and the shoreline sampler's origin height.
fn read_local_water(
    reader: &mut impl Read,
    count: usize,
    maximum_samples: usize,
    cancel: &impl Fn() -> bool,
) -> Result<Option<Vec<i16>>, String> {
    let mut buffer = [0_i16; 32768];
    let mut result: Option<Vec<i16>> = None;
    for offset in (0..count).step_by(buffer.len()) {
        if cancel() {
            return Err("Image render cancelled".into());
        }
        let n = (count - offset).min(buffer.len());
        let searching = result.is_none();
        let chunk = match &mut result {
            Some(result) => &mut result[offset..offset + n],
            None => &mut buffer[..n],
        };
        reader
            .read_exact(bytemuck::cast_slice_mut(chunk))
            .map_err(|e| e.to_string())?;
        #[cfg(target_endian = "big")]
        for word in &mut *chunk {
            *word = i16::from_le(*word);
        }
        if searching && chunk.iter().any(|&sample| sample != 0) {
            if count > maximum_samples {
                return Err("Retained image water exceeds the image memory limit in Settings".into());
            }
            let mut water = vec![0; count];
            water[offset..offset + n].copy_from_slice(chunk);
            result = Some(water);
        }
    }
    Ok(result)
}

fn discard_encoded_plane(
    reader: &mut impl Read,
    bytes: usize,
    cancel: &impl Fn() -> bool,
) -> Result<(), String> {
    let mut buffer = [0; 65536];
    for offset in (0..bytes).step_by(buffer.len()) {
        if cancel() {
            return Err("Image render cancelled".into());
        }
        let count = (bytes - offset).min(buffer.len());
        reader.read_exact(&mut buffer[..count]).map_err(|e| e.to_string())?;
    }
    Ok(())
}
impl Snapshot {
    pub fn retained_file(&self) -> Arc<tempfile::TempPath> {
        self.file.clone()
    }
    pub fn from_retained_file(
        file: Arc<tempfile::TempPath>,
        size: u32,
    ) -> Result<Arc<Self>, String> {
        if std::fs::metadata(file.as_ref())
            .map_err(|e| e.to_string())?
            .len()
            > 256 * 1024 * 1024
        {
            return Err("Retained map render exceeds 256 MiB".into());
        }
        let input = File::open(file.as_ref()).map_err(|e| e.to_string())?;
        let mut decoder = flate2::read::GzDecoder::new(BufReader::new(input));
        let mut header = [0; 24];
        decoder.read_exact(&mut header).map_err(|e| e.to_string())?;
        if &header[..8] != MAGIC {
            return Err("Invalid retained map render".into());
        }
        let native = u64::from_le_bytes(header[8..16].try_into().unwrap());
        let total = u64::from_le_bytes(header[16..24].try_into().unwrap());
        if native > 128 * 1024 * 1024 || total > 256 * 1024 * 1024 || total < native {
            return Err("Invalid retained map render lengths".into());
        }
        let mut layers = [0; 24];
        decoder.read_exact(&mut layers).map_err(|e| e.to_string())?;
        let (hr, sr) = native_layers::dimensions(size)?;
        if &layers[..4] != b"PNF1"
            || u32::from_le_bytes(layers[4..8].try_into().unwrap()) != size
            || u32::from_le_bytes(layers[16..20].try_into().unwrap()) as usize != hr
            || u32::from_le_bytes(layers[20..24].try_into().unwrap()) as usize != sr
            || native != (24 + hr * hr * 2 + sr * sr * 17) as u64
        {
            return Err("Saved map render dimensions differ from its recipe".into());
        }
        Ok(Arc::new(Self { file }))
    }
    pub fn capture(
        maps: &Composed,
        cancel: impl Fn() -> bool,
    ) -> Result<Option<Arc<Self>>, String> {
        if cancel() {
            return Ok(None);
        }
        let layers = &maps.layers;
        let (hr, sr) = native_layers::dimensions(layers.size)?;
        let height = maps
            .gameplay_height
            .as_ref()
            .map_or(layers.height.as_slice(), |h| &h.height);
        if layers.height_resolution != hr
            || layers.surface_resolution != sr
            || height.len() != hr * hr
            || layers.biome.len() != sr * sr * 5
            || layers.splat.len() != sr * sr * 8
            || layers.topology.len() != sr * sr
            || maps.alpha.len() != sr * sr
        {
            return Err("Invalid retained image layers".into());
        }
        let mut file = tempfile::Builder::new()
            .prefix("mapper-render-")
            .tempfile()
            .map_err(|e| e.to_string())?;
        {
            let mut writer = flate2::write::GzEncoder::new(
                BufWriter::new(file.as_file_mut()),
                flate2::Compression::fast(),
            );
            writer.write_all(MAGIC).map_err(|e| e.to_string())?;
            let length = (24 + hr * hr * 2 + sr * sr * 17) as u64;
            writer
                .write_all(&length.to_le_bytes())
                .map_err(|e| e.to_string())?;
            let retained_bytes = 24
                + length
                + maps.alpha.len() as u64
                + 1
                + if maps.ocean.is_some() {
                    (sr * sr * 5 + hr * hr * 2 + 24) as u64
                } else {
                    0
                };
            writer
                .write_all(&retained_bytes.to_le_bytes())
                .map_err(|e| e.to_string())?;
            writer.write_all(b"PNF1").map_err(|e| e.to_string())?;
            for word in [
                layers.size,
                layers.terrain_seed,
                layers.biome_seed,
                hr as u32,
                sr as u32,
            ] {
                writer
                    .write_all(&word.to_le_bytes())
                    .map_err(|e| e.to_string())?;
            }
            if !words(&mut writer, height, |v| v.to_le_bytes(), &cancel)?
                || !write(&mut writer, &layers.biome, &cancel)?
                || !write(&mut writer, &layers.splat, &cancel)?
                || !words(&mut writer, &layers.topology, |v| v.to_le_bytes(), &cancel)?
                || !write(&mut writer, &maps.alpha, &cancel)?
            {
                return Ok(None);
            }
            writer
                .write_all(&[u8::from(maps.ocean.is_some())])
                .map_err(|e| e.to_string())?;
            if let Some(ocean) = &maps.ocean {
                if ocean.resolution != sr
                    || ocean.classification.len() != sr * sr
                    || ocean.topology.len() != sr * sr
                    || ocean.local_water.len() != hr * hr
                {
                    return Err("Invalid retained image water".into());
                }
                if !write(&mut writer, &ocean.classification, &cancel)?
                    || !words(&mut writer, &ocean.topology, |v| v.to_le_bytes(), &cancel)?
                    || !words(
                        &mut writer,
                        &ocean.local_water,
                        |v| v.to_le_bytes(),
                        &cancel,
                    )?
                {
                    return Ok(None);
                }
                for count in [
                    ocean.ocean_cells,
                    ocean.enclosed_cells,
                    ocean.changed_topology_cells,
                ] {
                    writer
                        .write_all(&(count as u64).to_le_bytes())
                        .map_err(|e| e.to_string())?;
                }
            }
            writer
                .finish()
                .map_err(|e| e.to_string())?
                .flush()
                .map_err(|e| e.to_string())?;
        }
        if cancel() {
            return Ok(None);
        }
        Ok(Some(Arc::new(Self {
            file: Arc::new(file.into_temp_path()),
        })))
    }
    pub fn render(
        &self,
        resolution: usize,
        palette: Option<&crate::terrain_palette::Palette>,
        cancel: impl Fn() -> bool,
    ) -> Result<Option<eframe::egui::ColorImage>, String> {
        if cancel() {
            return Ok(None);
        }
        let file = File::open(self.file.as_ref()).map_err(|e| e.to_string())?;
        let length = file.metadata().map_err(|e| e.to_string())?.len();
        let budget = crate::generation_resources::current().image_memory_mib as u64 * 1024 * 1024;
        if !(2..=4096).contains(&resolution) || length > 256 * 1024 * 1024 {
            return Err("Retained image render exceeds the image memory limit in Settings".into());
        }
        let mut reader = flate2::read::GzDecoder::new(BufReader::new(file));
        let mut header = [0; 24];
        reader.read_exact(&mut header).map_err(|e| e.to_string())?;
        let native_bytes = u64::from_le_bytes(header[8..16].try_into().unwrap());
        let retained_bytes = u64::from_le_bytes(header[16..24].try_into().unwrap());
        if &header[..8] != MAGIC
            || native_bytes > 128 * 1024 * 1024
            || retained_bytes > 256 * 1024 * 1024
            || retained_bytes < native_bytes
        {
            return Err("Invalid retained image header".into());
        }
        // Rendering uses the terrain topology already present in the native
        // planes. The archive's duplicate ocean topology is only consumed.
        let image_bytes = resolution as u64 * resolution as u64 * 4;
        let fixed_bytes = native_bytes + 65536 + image_bytes;
        if fixed_bytes > budget {
            return Err("Retained image data exceeds the image memory limit in Settings".into());
        }
        let layers = native_layers::read_raw((&mut reader).take(native_bytes), &cancel)?;
        let hr = layers.height_resolution;
        let sr = layers.surface_resolution;
        let fixed_bytes = fixed_bytes + (sr * sr) as u64;
        if fixed_bytes > budget {
            return Err("Retained image data exceeds the image memory limit in Settings".into());
        }
        let mut alpha = vec![0; sr * sr];
        reader.read_exact(&mut alpha).map_err(|e| e.to_string())?;
        let mut flag = [0];
        reader.read_exact(&mut flag).map_err(|e| e.to_string())?;
        let expected = 24
            + native_bytes
            + alpha.len() as u64
            + 1
            + if flag[0] == 1 {
                (sr * sr * 5 + hr * hr * 2 + 24) as u64
            } else {
                0
            };
        if retained_bytes != expected {
            return Err("Retained image length differs from its layers".into());
        }
        let (classification, local_water) = match flag[0] {
            0 => (None, None),
            1 => {
                let fixed_bytes = fixed_bytes + (sr * sr) as u64;
                if fixed_bytes > budget {
                    return Err("Retained image water exceeds the image memory limit in Settings".into());
                }
                let mut classification = vec![0; sr * sr];
                reader
                    .read_exact(&mut classification)
                    .map_err(|e| e.to_string())?;
                discard_encoded_plane(&mut reader, sr * sr * 4, &cancel)?;
                let local_water = read_local_water(
                    &mut reader, hr * hr, ((budget - fixed_bytes) / 2) as usize, &cancel,
                )?;
                discard_encoded_plane(&mut reader, 24, &cancel)?;
                (Some(classification), local_water)
            }
            _ => return Err("Invalid retained image water flag".into()),
        };
        if reader.read(&mut flag).map_err(|e| e.to_string())? != 0 {
            return Err("Trailing retained image data".into());
        }
        drop(reader);
        if cancel() {
            return Ok(None);
        }
        let reserved = native_bytes as usize + 65536 + alpha.len()
            + classification.as_ref().map_or(0, Vec::len)
            + local_water.as_ref().map_or(0, |water| water.len() * 2);
        crate::map_image::render_retained(
            &layers,
            &alpha,
            classification.as_deref().map(|classes| (sr, classes)),
            local_water.as_deref(),
            resolution,
            palette,
            reserved,
            cancel,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dry_water_planes_need_no_allocation_and_late_water_is_retained_exactly() {
        let mut encoded = vec![0; 65540];
        assert!(read_local_water(&mut &encoded[..], 32770, 0, &|| false).unwrap().is_none());
        encoded[65536..65538].copy_from_slice(&16500_i16.to_le_bytes());
        assert!(read_local_water(&mut &encoded[..], 32770, 32769, &|| false).is_err());
        let actual = read_local_water(&mut &encoded[..], 32770, 32770, &|| false)
            .unwrap().unwrap();
        assert!(actual[..32768].iter().all(|&n| n == 0));
        assert_eq!(&actual[32768..], &[16500, 0]);
    }

    #[test]
    fn retained_render_preserves_final_heights_holes_water_and_releases_its_file() {
        let (hr, sr) = native_layers::dimensions(1000).unwrap();
        let cells = sr * sr;
        let mut layers = native_layers::Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![16000; hr * hr],
            biome: vec![0; cells * 5],
            splat: vec![0; cells * 8],
            topology: vec![0; cells],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        layers.biome[cells..2 * cells].fill(255);
        layers.splat[4 * cells..5 * cells].fill(255);
        let mut maps = papyrus_terrain::monument_composition::compose_with_water(
            &layers,
            &vec![255; cells],
            &[],
            || false,
        )
        .unwrap()
        .unwrap();
        maps.layers.biome[cells..2*cells].fill(200);
        maps.layers.biome[3*cells..4*cells].fill(54);
        maps.layers.splat[..cells].fill(82);
        maps.layers.splat[4*cells..5*cells].fill(172);
        maps.alpha[cells / 2..].fill(0);
        let mut height = maps.layers.height.clone();
        for row in height.chunks_mut(hr) {
            row[hr / 3..2 * hr / 3].fill(17000);
        }
        maps.gameplay_height = Some(papyrus_terrain::gameplay_height::Output {
            height,
            report: papyrus_terrain::gameplay_height::Report {
                engine: papyrus_terrain::gameplay_height::VERSION,
                contributors: 1,
                changed_samples: 0,
                work: 0,
                physics_cooking_validated: false,
            },
        });
        maps.ocean.as_mut().unwrap().local_water.fill(16500);
        let (installation, mut document) = crate::terrain_palette::tests::fixture(5);
        document["cartography"] =
            serde_json::to_value(crate::map_colors::tests::fixture()).unwrap();
        let palette = crate::terrain_palette::Palette::decode(
            &serde_json::to_vec(&document).unwrap(),
            &installation,
        )
        .unwrap();
        let mut dry = maps;
        dry.ocean.as_mut().unwrap().local_water.fill(0);
        let maps = Arc::new(dry);
        let dry_snapshot = Snapshot::capture(&maps, || false).unwrap().unwrap();
        for palette in [None, Some(&palette)] {
            let original = crate::map_image::render_composed_owned(&maps, 64, palette, || false)
                .unwrap().unwrap().image;
            let retained = dry_snapshot.render(64, palette, || false).unwrap().unwrap();
            assert_eq!(original.pixels, retained.pixels);
        }
        let mut maps = Arc::into_inner(maps).unwrap();
        maps.ocean.as_mut().unwrap().local_water.fill(16500);
        let maps = Arc::new(maps);
        let snapshot = Snapshot::capture(&maps, || false).unwrap().unwrap();
        let path = snapshot.file.to_path_buf();
        for palette in [None, Some(&palette)] {
            let original = crate::map_image::render_composed_owned(&maps, 64, palette, || false)
                .unwrap()
                .unwrap()
                .image;
            let retained = snapshot.render(64, palette, || false).unwrap().unwrap();
            assert_eq!(original.pixels, retained.pixels);
        }
        assert!(Snapshot::capture(&maps, || true).unwrap().is_none());
        assert!(snapshot.render(64, None, || true).unwrap().is_none());
        drop(snapshot);
        assert!(!path.exists());
    }
}
