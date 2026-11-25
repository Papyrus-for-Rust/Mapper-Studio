use papyrus_terrain::terrain_graph::{Heightmap, MAX_HEIGHTMAP_EDGE, MAX_HEIGHTMAP_PIXELS};
use std::{
    io::{Cursor, Read},
    path::Path,
};

pub(super) fn read(path: &Path) -> Result<Heightmap, String> {
    const MAX_PNG_BYTES: u64 = 2 * 1024 * 1024;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|error| error.to_string())?
        .take(MAX_PNG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_PNG_BYTES {
        return Err("Heightmap PNG exceeds 2 MiB".into());
    }
    let name = path
        .file_stem()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .unwrap_or("Heightmap");
    let end = name
        .char_indices()
        .find(|(i, ch)| i + ch.len_utf8() > 128)
        .map_or(name.len(), |(i, _)| i);
    decode(&bytes, &name[..end])
}

fn decode(bytes: &[u8], label: &str) -> Result<Heightmap, String> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_limits(png::Limits {
        bytes: 4 * 1024 * 1024,
    });
    // Heightmaps contain numeric samples. Retain the source bit depth and
    // ignore color management, gamma and transparency metadata.
    let mut reader = decoder.read_info().map_err(|error| error.to_string())?;
    let info = reader.info();
    let [w, h] = [info.width, info.height];
    let edge = MAX_HEIGHTMAP_EDGE as u32;
    if !(2..=edge).contains(&w) || !(2..=edge).contains(&h) {
        return Err(format!(
            "Heightmaps must be between 2 and {edge} pixels on each side"
        ));
    }
    if info.color_type != png::ColorType::Grayscale
        || !matches!(
            info.bit_depth,
            png::BitDepth::Eight | png::BitDepth::Sixteen
        )
    {
        return Err("Use an 8-bit or 16-bit grayscale PNG".into());
    }
    let length = reader
        .output_buffer_size()
        .filter(|&size| size <= MAX_HEIGHTMAP_PIXELS * 2)
        .ok_or("Heightmap pixel buffer exceeds its limit")?;
    let mut decoded = vec![0; length];
    let frame = reader
        .next_frame(&mut decoded)
        .map_err(|error| error.to_string())?;
    let bytes = &decoded[..frame.buffer_size()];
    let samples: Vec<_> = if frame.bit_depth == png::BitDepth::Sixteen {
        bytes
            .chunks_exact(2)
            .map(|pixel| u16::from_be_bytes([pixel[0], pixel[1]]))
            .collect()
    } else {
        bytes.iter().map(|&pixel| u16::from(pixel) * 257).collect()
    };
    if samples.len() != (w * h) as usize || frame.width != w || frame.height != h {
        return Err("Heightmap frame does not cover the image".into());
    }
    Ok(Heightmap {
        label: label.into(),
        dimensions: [w as u16, h as u16],
        samples: samples.into(),
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn decodes_height_values_without_losing_sixteen_bit_precision() {
        let mut bytes = Vec::new();
        let samples = [0_u16, 1097, 32769, 65535];
        {
            let mut encoder = png::Encoder::new(&mut bytes, 2, 2);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Sixteen);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(
                    &samples
                        .into_iter()
                        .flat_map(u16::to_be_bytes)
                        .collect::<Vec<_>>(),
                )
                .unwrap();
        }
        let image = super::decode(&bytes, "Survey").unwrap();
        assert_eq!(&*image.samples, &samples);
    }
}
