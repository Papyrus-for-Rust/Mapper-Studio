//! Atomic PNG export of map pixels, without editor decorations.
use eframe::egui::ColorImage;
use std::{io::{BufWriter, Write}, path::Path};
pub fn write_png(
    path: &Path,
    image: &ColorImage,
    cancelled: impl Fn() -> bool,
) -> Result<bool, String> {
    write_png_with_metadata(path, image, None, cancelled)
}
pub fn write_png_with_metadata(
    path: &Path,
    image: &ColorImage,
    metadata: Option<&crate::image_render::Metadata>,
    cancelled: impl Fn() -> bool,
) -> Result<bool, String> {
    write_png_with_publish_gate(path, image, metadata, &cancelled, || !cancelled())
}

/// The publication gate runs after the temporary PNG is flushed.
pub fn write_png_with_publish_gate(
    path: &Path,
    image: &ColorImage,
    metadata: Option<&crate::image_render::Metadata>,
    cancelled: impl Fn() -> bool,
    begin_publish: impl FnOnce() -> bool,
) -> Result<bool, String> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let Some(temp) = build_png(
        Some(parent),
        image,
        metadata,
        png::Compression::Balanced,
        &cancelled,
    )?
    else {
        return Ok(false);
    };
    if cancelled() {
        return Ok(false);
    }
    temp.as_file().sync_all().map_err(|e| e.to_string())?;
    if !begin_publish() {
        return Ok(false);
    }
    temp.persist(path).map_err(|e| e.error.to_string())?;
    Ok(true)
}

pub fn retain_overview(
    image: &ColorImage,
    cancelled: impl Fn() -> bool,
) -> Result<Option<tempfile::TempPath>, String> {
    Ok(
        build_png(None, image, None, png::Compression::Fast, &cancelled)?
            .map(tempfile::NamedTempFile::into_temp_path),
    )
}

fn build_png(
    parent: Option<&Path>,
    image: &ColorImage,
    metadata: Option<&crate::image_render::Metadata>,
    compression: png::Compression,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<tempfile::NamedTempFile>, String> {
    let [width, height] = image.size;
    if width == 0
        || height == 0
        || width > 4096
        || height > 4096
        || image.pixels.len() != width * height
    {
        return Err("Invalid export image dimensions".into());
    }
    if cancelled() {
        return Ok(None);
    }
    let mut temp = match parent {
        Some(parent) => tempfile::NamedTempFile::new_in(parent),
        None => tempfile::Builder::new()
            .prefix("mapper-overview-")
            .suffix(".png")
            .tempfile(),
    }
    .map_err(|e| e.to_string())?;
    {
        // Coalesce the streaming encoder's small chunk writes. Writer::finish
        // flushes this buffer before the temporary file is handed to publication.
        let output = BufWriter::with_capacity(64 * 1024, temp.as_file_mut());
        let mut encoder = png::Encoder::new(output, width as u32, height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(compression);
        if let Some(metadata) = metadata {
            if metadata.image_pixels != image.size {
                return Err("Image metadata dimensions differ".into());
            }
            encoder
                .add_itxt_chunk(crate::image_render::METADATA_KEY.into(), metadata.json()?)
                .map_err(|e| e.to_string())?;
        }
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        {
            let mut stream = writer.stream_writer().map_err(|e| e.to_string())?;
            let mut converted = Vec::new();
            for pixels in image.pixels.chunks_exact(width) {
                if cancelled() {
                    return Ok(None);
                }
                // Opaque and zero-alpha pixels already match egui's straight RGBA conversion.
                let row = if pixels.iter().all(|p| matches!(p.a(), 0 | 255)) {
                    bytemuck::cast_slice(pixels)
                } else {
                    converted.clear();
                    converted.reserve(width * 4);
                    for p in pixels {
                        converted.extend_from_slice(&p.to_srgba_unmultiplied());
                    }
                    &converted
                };
                stream.write_all(row).map_err(|e| e.to_string())?;
            }
            stream.finish().map_err(|e| e.to_string())?;
        }
        writer.finish().map_err(|e| e.to_string())?;
    }
    Ok((!cancelled()).then_some(temp))
}
#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Color32, ColorImage};

    #[test]
    fn declined_png_publication_keeps_the_existing_destination() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("map.png");
        std::fs::write(&path, b"original").unwrap();
        let image = ColorImage::filled([2, 2], Color32::BLUE);
        assert!(!write_png_with_publish_gate(&path, &image, None, || false, || false).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        assert_eq!(directory.path().read_dir().unwrap().count(), 1);
    }

    #[test]
    fn optional_utf8_metadata_preserves_pixels_and_validates_before_publication() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.png");
        let translucent = Color32::from_rgba_unmultiplied(40, 80, 120, 128);
        let image = ColorImage::new(
            [2, 2],
            vec![
                Color32::RED,
                Color32::TRANSPARENT,
                translucent,
                Color32::BLUE,
            ],
        );
        let mut data = crate::image_render::Metadata::new(
            &crate::Project::default(),
            [2, 2],
            eframe::egui::Rect::from_min_max(eframe::egui::Pos2::ZERO, eframe::egui::pos2(2., 2.)),
            Default::default(),
        );
        data.add(1 << 62, "Île du Port".into(), None, [0., 0., 0.], 45.)
            .unwrap();
        assert!(write_png_with_metadata(&path, &image, Some(&data), || false).unwrap());
        let original = std::fs::read(&path).unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(&original))
            .read_info()
            .unwrap();
        let text = reader
            .info()
            .utf8_text
            .iter()
            .find(|c| c.keyword == crate::image_render::METADATA_KEY)
            .unwrap()
            .get_text()
            .unwrap();
        let decoded: crate::image_render::Metadata = serde_json::from_str(&text).unwrap();
        assert_eq!(decoded.monuments[0].name, "Île du Port");
        assert_eq!(decoded.monuments[0].id, (1_u64 << 62).to_string());
        let mut bytes = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut bytes).unwrap();
        let expected: Vec<_> = image
            .pixels
            .iter()
            .flat_map(Color32::to_srgba_unmultiplied)
            .collect();
        assert_eq!(bytes, expected);
        data.image_pixels = [3, 3];
        assert!(write_png_with_metadata(&path, &image, Some(&data), || false).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
    #[test]
    fn png_roundtrip_and_cancel_preserve_existing_output() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("map.png");
        let image = ColorImage::new(
            [2, 2],
            vec![Color32::RED, Color32::GREEN, Color32::BLUE, Color32::WHITE],
        );
        assert!(write_png(&path, &image, || false).unwrap());
        let original = std::fs::read(&path).unwrap();
        let retained = retain_overview(&image, || false).unwrap().unwrap();
        let retained = std::fs::read(retained).unwrap();
        let decoder = png::Decoder::new(std::io::Cursor::new(&retained));
        let mut reader = decoder.read_info().unwrap();
        let mut bytes = vec![0; reader.output_buffer_size().unwrap()];
        let frame = reader.next_frame(&mut bytes).unwrap();
        assert_eq!((frame.width, frame.height), (2, 2));
        assert_eq!(
            &bytes[..16],
            &[
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255
            ]
        );
        let calls = std::cell::Cell::new(0);
        assert!(
            !write_png(&path, &image, || {
                calls.set(calls.get() + 1);
                calls.get() > 2
            })
            .unwrap()
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
