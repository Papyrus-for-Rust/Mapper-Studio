//! Machine-local, content-addressed monument artwork. Never changes map recipes.
mod builtin;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Cursor, Read, Write},
    path::Path,
};
pub const SIZE: usize = 64;
pub type Overrides = BTreeMap<String, String>;
pub fn validate(overrides: &Overrides) -> Result<(), String> {
    if overrides.len() > 256
        || overrides.iter().any(|(path, hash)| {
            !path.starts_with("assets/")
                || !path.ends_with(".prefab")
                || path.len() > 512
                || path.split('/').any(|p| p == "..")
                || path.chars().any(char::is_control)
                || hash.len() != 64
                || !hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    {
        return Err("Invalid monument icon preferences".into());
    }
    Ok(())
}
pub fn directory(settings: &Path) -> Result<std::path::PathBuf, String> {
    Ok(settings
        .parent()
        .ok_or("Settings folder unavailable")?
        .join("monument-icons"))
}
fn decode(bytes: &[u8], maximum: u32) -> Result<(usize, usize, Vec<u8>), String> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    decoder.set_limits(png::Limits {
        bytes: 8 * 1024 * 1024,
    });
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("Cannot read PNG: {e}"))?;
    let info = reader.info();
    if info.width == 0 || info.height == 0 || info.width > maximum || info.height > maximum {
        return Err(format!(
            "Choose a PNG no larger than {maximum} × {maximum} pixels"
        ));
    }
    let mut buffer = vec![
        0;
        reader
            .output_buffer_size()
            .filter(|&s| s <= 4 * 1024 * 1024)
            .ok_or("PNG exceeds image limit")?
    ];
    let output = reader.next_frame(&mut buffer).map_err(|e| e.to_string())?;
    let mut rgba = Vec::with_capacity(output.width as usize * output.height as usize * 4);
    let channels = output.color_type.samples();
    for p in buffer[..output.buffer_size()].chunks_exact(channels) {
        let pixel = match output.color_type {
            png::ColorType::Rgba => [p[0], p[1], p[2], p[3]],
            png::ColorType::Rgb => [p[0], p[1], p[2], 255],
            png::ColorType::Grayscale => [p[0], p[0], p[0], 255],
            png::ColorType::GrayscaleAlpha => [p[0], p[0], p[0], p[1]],
            _ => return Err("Unsupported PNG color format".into()),
        };
        rgba.extend_from_slice(&pixel);
    }
    Ok((output.width as usize, output.height as usize, rgba))
}
fn bounded_read(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("Icon file exceeds size limit".into());
    }
    Ok(bytes)
}
/// Resolve default artwork using the installation's own display name.
pub fn default_key(name: &str) -> Option<&'static str> {
    builtin::key(name)
}

/// Named V3 formations matched to the prefab poses of a published Rust world.
pub fn rock_marker(path: &str) -> Option<(&'static str, &'static str)> {
    match path.strip_prefix("assets/bundled/prefabs/autospawn/decor/v3_rock_formations_large/")? {
        "rock_formation_a.prefab" => Some(("Large God Rock", "god-rock")),
        "rock_formation_b.prefab" => Some(("Anvil Rock", "anvil-rock")),
        "rock_formation_e.prefab" => Some(("Medium God Rock", "god-rock")),
        "rock_formation_f.prefab" => Some(("Tiny God Rock", "god-rock")),
        _ => None,
    }
}

pub fn default_key_for_prefab(name:&str,path:&str)->Option<&'static str> {
    if let Some((_, key)) = rock_marker(path) { return Some(key); }
    if path.contains("/monument/medium/apartments_complex_") {return Some("apartments");}
    if path.contains("/autospawn/tunnel-upwards/") {return Some("train-tunnel-rails");}
    if path.contains("/autospawn/tunnel-entrance/") {return Some("train-tunnel");}
    if path.contains("/unique_environment/canyon/") || path.contains("/autospawn/canyon/") {return Some("canyon");}
    if path.contains("/unique_environment/lake/") {return Some("lake");}
    if path.contains("/unique_environment/oasis/") {return Some("oasis");}
    if path.contains("/unique_environment/jungle/ue_jungle_swamp_") {return Some("jungle-swamp");}
    if path.contains("/monument/jungle_ruins/jungle_ruins_") {return Some("jungle-ruins");}
    if path.contains("/monument/cave/") {return Some("underground-cave");}
    if path.contains("/monument/swamp/swamp_a.") || path.contains("/monument/swamp/swamp_b.") {return Some("wild-swamp");}
    if path.ends_with("/military_tunnel_1.prefab") {return Some("military-tunnels");}
    default_key(name)
}
pub fn default_rgba(key: &str) -> Result<Vec<u8>, String> {
    let bytes = builtin::png(key).ok_or("Unknown default monument icon")?;
    let (width, height, rgba) = decode(bytes, 1024)?;
    Ok(normalize(width, height, &rgba))
}
fn normalize(width: usize, height: usize, pixels: &[u8]) -> Vec<u8> {
    let scale = SIZE as f32 / width.max(height) as f32;
    let w = (width as f32 * scale).round().max(1.) as usize;
    let h = (height as f32 * scale).round().max(1.) as usize;
    let mut rgba = vec![0; SIZE * SIZE * 4];
    // Area averaging in premultiplied alpha preserves transparent icon edges.
    for y in 0..h {
        for x in 0..w {
            let x0 = x * width / w;
            let x1 = ((x + 1) * width / w).max(x0 + 1).min(width);
            let y0 = y * height / h;
            let y1 = ((y + 1) * height / h).max(y0 + 1).min(height);
            let mut color = [0u64; 4];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let i = (sy * width + sx) * 4;
                    let a = u64::from(pixels[i + 3]);
                    for c in 0..3 {
                        color[c] += u64::from(pixels[i + c]) * a;
                    }
                    color[3] += a;
                }
            }
            let i = (((SIZE - h) / 2 + y) * SIZE + (SIZE - w) / 2 + x) * 4;
            for c in 0..3 {
                rgba[i + c] = color[c].checked_div(color[3]).unwrap_or(0) as u8;
            }
            rgba[i + 3] = (color[3] / ((x1 - x0) * (y1 - y0)) as u64) as u8;
        }
    }
    rgba
}
pub fn import(folder: &Path, source: &Path) -> Result<String, String> {
    let (width, height, pixels) = decode(&bounded_read(source, 4 * 1024 * 1024)?, 1024)?;
    let rgba = normalize(width, height, &pixels);
    let mut encoded = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut encoded, SIZE as u32, SIZE as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .map_err(|e| e.to_string())?
            .write_image_data(&rgba)
            .map_err(|e| e.to_string())?;
    }
    let hash = format!("{:x}", Sha256::digest(&encoded));
    std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    let destination = folder.join(format!("{hash}.png"));
    if load(folder, &hash).is_err() {
        let mut file = tempfile::NamedTempFile::new_in(folder).map_err(|e| e.to_string())?;
        file.write_all(&encoded).map_err(|e| e.to_string())?;
        file.persist(&destination).map_err(|e| e.to_string())?;
    }
    load(folder, &hash)?;
    Ok(hash)
}
pub fn load(folder: &Path, hash: &str) -> Result<Vec<u8>, String> {
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("Invalid icon identity".into());
    }
    let bytes = bounded_read(&folder.join(format!("{hash}.png")), 128 * 1024)?;
    if format!("{:x}", Sha256::digest(&bytes)) != hash {
        return Err("Icon file changed; choose the image again".into());
    }
    let (w, h, rgba) = decode(&bytes, SIZE as u32)?;
    if w != SIZE || h != SIZE {
        return Err("Invalid cached icon dimensions".into());
    }
    Ok(rgba)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authored_defaults_decode_and_follow_installation_display_names() {
        let keys: Vec<_> = builtin::all_keys().collect();
        assert_eq!(keys.len(), 56);
        for key in keys {
            let rgba = default_rgba(key).unwrap();
            assert_eq!(rgba.len(), SIZE * SIZE * 4);
            assert!(rgba.chunks_exact(4).any(|p| p[3] > 0), "Empty icon {key}");
        }
        assert_eq!(default_key("Power Plant"), Some("nuclear-powerplant"));
        assert_eq!(
            default_key("Large Fishing Village"),
            Some("large-fishing-village")
        );
        assert_eq!(default_key("Sulfur Quarry"), Some("sulfur-quarry"));
        assert_eq!(default_key("Jungle Ziggurat"), Some("ziggaurat"));
        assert_eq!(default_key("Unknown custom monument"), None);
        for (name,key) in [("The Dome","dome"),("Abandoned Supermarket","supermarket"),
            ("Radtown","radtown"),("Apartments","apartments"),("Abandoned Military Base","military-base"),
            ("Ferry Terminal","ferry-terminal"),("Ice Lake 4","ice-lake"),("Train Tunnel","train-tunnel"),
            ("Train Tunnel Rails","train-tunnel-rails")] {
            assert_eq!(default_key(name),Some(key));
        }
        assert_eq!(default_key_for_prefab("Radtown","assets/bundled/prefabs/autospawn/monument/medium/apartments_complex_1.prefab"),Some("apartments"));
        assert_eq!(default_key_for_prefab("Train Tunnel","assets/bundled/prefabs/autospawn/tunnel-upwards/intersection-b6-w.prefab"),Some("train-tunnel-rails"));
        assert_eq!(default_key_for_prefab("entrance bunker d","assets/bundled/prefabs/autospawn/tunnel-entrance/entrance_bunker_d.prefab"),Some("train-tunnel"));
    }

    #[test]
    fn natural_landmark_artwork_follows_prefab_families() {
        let root = "assets/bundled/prefabs/autospawn";
        for (name, path, key) in [
            ("ue canyon c", "unique_environment/canyon/ue_canyon_c.prefab", "canyon"),
            ("ue lake c", "unique_environment/lake/ue_lake_c.prefab", "lake"),
            ("ue oasis b", "unique_environment/oasis/ue_oasis_b.prefab", "oasis"),
            ("Ice Lake 4", "unique_environment/icelake/ice_lake_4.prefab", "ice-lake"),
            ("Jungle Ruin", "monument/jungle_ruins/jungle_ruins_a.prefab", "jungle-ruins"),
            ("Jungle Ziggurat", "monument/jungle/jungle_ziggurat.prefab", "ziggaurat"),
            ("Jungle Swamp", "unique_environment/jungle/ue_jungle_swamp_a.prefab", "jungle-swamp"),
            ("Cave Small", "monument/cave/cave_small_hard.prefab", "underground-cave"),
            ("Swamp A", "monument/swamp/swamp_a.prefab", "wild-swamp"),
            ("Abandoned Cabins", "monument/swamp/swamp_c.prefab", "abandoned-cabins"),
            ("Formation A", "decor/v3_rock_formations_large/rock_formation_a.prefab", "god-rock"),
            ("Formation B", "decor/v3_rock_formations_large/rock_formation_b.prefab", "anvil-rock"),
            ("Formation E", "decor/v3_rock_formations_large/rock_formation_e.prefab", "god-rock"),
            ("Formation F", "decor/v3_rock_formations_large/rock_formation_f.prefab", "god-rock"),
        ] {
            assert_eq!(default_key_for_prefab(name, &format!("{root}/{path}")), Some(key), "{path}");
        }
        assert_eq!(rock_marker(&format!("{root}/decor/v3_rock_formations_large/rock_formation_c.prefab")), None);
        assert_eq!(rock_marker(&format!("{root}/decor/v2_rockformation/rock_formation_b.prefab")), None);
    }
    fn png(path: &Path, width: u32, height: u32, color: png::ColorType, bytes: &[u8]) {
        let file = std::fs::File::create(path).unwrap();
        let mut encoder = png::Encoder::new(file, width, height);
        encoder.set_color(color);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(bytes)
            .unwrap();
    }
    #[test]
    fn imported_icons_preserve_alpha_aspect_and_survive_original_removal_and_reload() {
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("icon.png");
        png(
            &original,
            2,
            1,
            png::ColorType::Rgba,
            &[255, 50, 10, 255, 0, 255, 0, 0],
        );
        let settings = temp.path().join("settings.json");
        let folder = directory(&settings).unwrap();
        let hash = import(&folder, &original).unwrap();
        assert_eq!(import(&folder, &original).unwrap(), hash);
        std::fs::remove_file(&original).unwrap();
        let pixels = load(&folder, &hash).unwrap();
        assert_eq!(pixels.len(), SIZE * SIZE * 4);
        assert!(pixels[..SIZE * 16 * 4].iter().all(|&b| b == 0));
        let red = (32 * SIZE + 8) * 4;
        assert_eq!(&pixels[red..red + 4], &[255, 50, 10, 255]);
        let transparent = (32 * SIZE + 48) * 4;
        assert_eq!(pixels[transparent + 3], 0);
        let mut prefs = crate::preferences::Preferences::default();
        prefs
            .monument_icons
            .insert("assets/compound.prefab".into(), hash.clone());
        prefs.save(&settings).unwrap();
        assert_eq!(
            crate::preferences::Preferences::load(&settings)
                .unwrap()
                .monument_icons,
            prefs.monument_icons
        );
        std::fs::write(folder.join(format!("{hash}.png")), b"changed").unwrap();
        assert!(load(&folder, &hash).is_err());
        let before = std::fs::read(&settings).unwrap();
        prefs
            .monument_icons
            .insert("assets/bad.prefab".into(), "../bad".into());
        assert!(prefs.save(&settings).is_err());
        assert_eq!(std::fs::read(settings).unwrap(), before);
    }
    #[test]
    fn malformed_oversized_and_grayscale_images_are_handled_without_partial_icons() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("input.png");
        let folder = temp.path().join("icons");
        std::fs::write(&input, b"not an image").unwrap();
        assert!(import(&folder, &input).is_err());
        assert!(!folder.exists());
        png(&input, 1025, 1, png::ColorType::Grayscale, &vec![127; 1025]);
        assert!(import(&folder, &input).is_err());
        assert!(!folder.exists());
        png(&input, 1, 1, png::ColorType::GrayscaleAlpha, &[120, 80]);
        let hash = import(&folder, &input).unwrap();
        let rgba = load(&folder, &hash).unwrap();
        assert!(rgba.chunks_exact(4).all(|p| p == [120, 120, 120, 80]));
        assert!(load(&folder, "../../bad").is_err());
    }
}
