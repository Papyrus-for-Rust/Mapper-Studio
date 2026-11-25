//! Local cache decoding and stable prefab identity for the independent generator.
use crate::{
    monuments::{Entry, MAX_INPUT_BYTES, Policy, Reader, Request},
    stamps,
};
use md5::{Digest, Md5};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
};
use unicode_general_category::{GeneralCategory, get_general_category};

const MAX_JSON: u64 = 64 * 1024 * 1024;
const ROOT: &str = "assets/bundled/prefabs/autospawn/monument/";
const CATALOG: &[(&str, i32, i32)] = &[
    ("small/sphere_tank.prefab", 1000, 8),
    ("small/satellite_dish.prefab", 1000, 7),
    ("small/mining_quarry_a.prefab", 1000, 4),
    ("small/mining_quarry_b.prefab", 1000, 4),
    ("small/mining_quarry_c.prefab", 1000, 4),
    ("small/stables_a.prefab", 1000, 3),
    ("small/stables_b.prefab", 1000, 3),
    ("roadside/gas_station_1.prefab", 1000, 7),
    ("roadside/supermarket_1.prefab", 1000, 7),
    ("roadside/warehouse.prefab", 1000, 6),
    ("roadside/radtown_1.prefab", 1000, 4),
    ("tiny/water_well_a.prefab", 1000, 3),
    ("tiny/water_well_b.prefab", 1000, 3),
    ("tiny/water_well_c.prefab", 1000, 3),
    ("medium/apartments_complex_1.prefab", 2000, 3),
    ("medium/junkyard_1.prefab", 2000, 3),
    ("medium/radtown_small_3.prefab", 2000, 4),
    ("large/airfield_1.prefab", 3000, 1),
    ("large/powerplant_1.prefab", 3000, 1),
    ("large/trainyard_1.prefab", 3000, 1),
    ("xlarge/launch_site_1.prefab", 4000, 1),
];

pub struct Prepared {
    pub request: Request,
    pub paths: Vec<String>,
    pub ids: Vec<u32>,
}

/// Independent-generator catalog estimates, not verified game asset footprints.
pub fn catalog_estimates() -> Vec<(String, Entry)> {
    CATALOG
        .iter()
        .map(|(suffix, _, _)| {
            let path = format!("{ROOT}{suffix}");
            let entry = fallback(&path);
            (path, entry)
        })
        .collect()
}

// .NET invariant casing uses a single scalar; U+0130 must not expand to i + dot.
fn lower(text: &str) -> String {
    text.chars()
        .map(|c| {
            let mut values = c.to_lowercase();
            let first = values.next().unwrap_or(c);
            if values.next().is_none() { first } else { c }
        })
        .collect()
}
fn key(text: &str) -> String {
    text.chars()
        .map(|c| {
            // OrdinalIgnoreCase keeps these separate from ASCII I and S.
            if c == '\u{131}' || c == '\u{17f}' {
                return c;
            }
            // Simple uppercase retains Greek prosgegrammeni instead of expanding to two scalars.
            match c as u32 {
                0x1f80..=0x1f87 | 0x1f90..=0x1f97 | 0x1fa0..=0x1fa7 => {
                    return char::from_u32(c as u32 + 8).unwrap_or(c);
                }
                0x1fb3 => return '\u{1fbc}',
                0x1fc3 => return '\u{1fcc}',
                0x1ff3 => return '\u{1ffc}',
                _ => {}
            }
            let mut values = c.to_uppercase();
            let first = values.next().unwrap_or(c);
            if values.next().is_none() { first } else { c }
        })
        .collect()
}
pub fn prefab_id(path: &str) -> u32 {
    if path.trim().is_empty() {
        return 0;
    }
    let digest = Md5::digest(lower(path).as_bytes());
    u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]])
}
fn normalize(text: &str) -> String {
    lower(
        &text
            .chars()
            .filter(|&c| {
                // The baseline calls char.IsLetter on UTF-16 code units.
                c as u32 <= 0xffff
                    && matches!(
                        get_general_category(c),
                        GeneralCategory::UppercaseLetter
                            | GeneralCategory::LowercaseLetter
                            | GeneralCategory::TitlecaseLetter
                            | GeneralCategory::ModifierLetter
                            | GeneralCategory::OtherLetter
                    )
            })
            .collect::<String>(),
    )
}
fn land(path: &str) -> bool {
    let path = lower(&path.replace('\\', "/"));
    ![
        "/roadside/",
        "/offshore/",
        "/cave/",
        "/underwater_lab/",
        "/fishing_village/",
        "/maintainables/",
        "/lighthouse/",
    ]
    .iter()
    .any(|s| path.contains(s))
}
fn fallback(path: &str) -> Entry {
    let p = lower(path);
    let catalog = CATALOG
        .iter()
        .find(|(name, _, _)| p == format!("{ROOT}{name}"));
    let extent = if p.contains("launch_site") {
        150.0
    } else if ["airfield", "powerplant", "trainyard"]
        .iter()
        .any(|s| p.contains(s))
    {
        115.0
    } else if p.contains("apartments") || p.contains("junkyard") {
        80.0
    } else if ["warehouse", "supermarket", "gas_station"]
        .iter()
        .any(|s| p.contains(s))
    {
        55.0
    } else if p.contains("water_well") {
        20.0
    } else {
        45.0
    };
    Entry {
        group: 0,
        resolved: 0,
        land: land(path),
        minimum_size: catalog.map_or(0, |v| v.1),
        extent_x: extent,
        extent_z: extent,
        priority: catalog.map_or(1, |v| v.2),
        minimum_yaw: -180.0,
        maximum_yaw: 180.0,
    }
}
fn integer(v: &Value) -> Result<i32, String> {
    v.as_i64()
        .and_then(|v| i32::try_from(v).ok())
        .ok_or("invalid cache integer".into())
}
fn float(v: &Value) -> Result<f32, String> {
    v.as_f64()
        .map(|v| v as f32)
        .filter(|v| v.is_finite())
        .ok_or("invalid cache float".into())
}
fn boolean(v: &Value) -> Result<bool, String> {
    v.as_bool().ok_or("invalid cache boolean".into())
}
fn vector(v: &Value) -> Result<[f32; 3], String> {
    let a = v
        .as_array()
        .filter(|a| a.len() == 3)
        .ok_or("invalid cache vector")?;
    Ok([float(&a[0])?, float(&a[1])?, float(&a[2])?])
}
fn json(path: &Path) -> Result<Option<Value>, String> {
    let file = match File::open(path) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cache open failed: {e}")),
    };
    if file.metadata().map_err(|e| e.to_string())?.len() > MAX_JSON {
        return Err("cache JSON exceeds 64 MiB".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_JSON + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_JSON {
        return Err("cache JSON exceeds 64 MiB".into());
    }
    // File.ReadAllText accepts a UTF-8 BOM.
    let bytes = bytes.strip_prefix(&[239, 187, 191]).unwrap_or(&bytes);
    serde_json::from_slice(bytes)
        .map(Some)
        .map_err(|e| format!("invalid cache JSON: {e}"))
}

impl Prepared {
    pub fn read(bytes: &[u8], settings: crate::world::Settings) -> Result<Self, String> {
        settings.validate()?;
        if bytes.len() > MAX_INPUT_BYTES || bytes.get(..4) != Some(b"MAPR") {
            return Err("invalid registry input header".into());
        }
        let mut r = Reader { bytes, cursor: 4 };
        if r.word()? != 1 {
            return Err("unsupported registry input version".into());
        }
        let mut request = Request::disabled();
        request.enabled = r.boolean()?;
        request.seed = r.word()? as i32;
        request.target = r.word()? as usize;
        request.include_large = r.boolean()?;
        let budget = r.word()? as u64 | ((r.word()? as u64) << 32);
        let root = r.text()?;
        if root.contains('\0') || !Path::new(&root).is_absolute() {
            return Err("invalid registry cache root".into());
        }
        let count = r.count(4096)?;
        let mut policies = Vec::with_capacity(count);
        for _ in 0..count {
            let path = r.text()?;
            if path.trim().is_empty() || path.contains('\0') {
                return Err("invalid prefab path".into());
            }
            policies.push((path, r.word()?, r.word()? as usize));
        }
        if r.cursor != bytes.len() {
            return Err("trailing registry input".into());
        }
        let mut prepared = Self {
            request,
            paths: vec![],
            ids: vec![],
        };
        let mut resolved = HashMap::new();
        if prepared.request.enabled || policies.iter().any(|p| p.1 == 1) {
            let modifiers = json(&Path::new(&root).join("prefab-modifiers.json"))?;
            if let Some(modifiers) = modifiers.as_ref().and_then(|v| v["modifiers"].as_object()) {
                for (path, value) in modifiers {
                    let bounds = &value["monumentBounds"];
                    if bounds.is_null() {
                        continue;
                    }
                    let extent = vector(&bounds["extent"])?;
                    let placement = &value["placement"];
                    let rotation = &value["rotation"];
                    if !placement.is_null() {
                        // Authored counts remain unused by the independent placement baseline.
                        integer(&placement["count"])?;
                    }
                    let entry = Entry {
                        group: 0,
                        resolved: 0,
                        land: land(path),
                        minimum_size: integer(&bounds["minWorldSize"])?,
                        extent_x: extent[0],
                        extent_z: extent[2],
                        priority: if placement.is_null() {
                            0
                        } else {
                            integer(&placement["priority"])?
                        },
                        minimum_yaw: if rotation.is_null() {
                            -180.0
                        } else {
                            vector(&rotation["min"])?[1]
                        },
                        maximum_yaw: if rotation.is_null() {
                            180.0
                        } else {
                            vector(&rotation["max"])?[1]
                        },
                    };
                    let k = key(path);
                    if let Some(&index) = resolved.get(&k) {
                        prepared.request.entries[index] = entry;
                        prepared.paths[index] = path.clone();
                    } else {
                        let index = prepared.push(path.clone(), entry)?;
                        resolved.insert(k, index);
                        prepared.request.authored.push(index);
                    }
                }
            }
            for &(name, _, _) in CATALOG {
                let path = format!("{ROOT}{name}");
                let entry = fallback(&path);
                let k = key(&path);
                let index = prepared.push(path, entry)?;
                resolved.entry(k).or_insert(index);
                prepared.request.catalog.push(index);
            }
            for (path, preference, count) in policies {
                let k = key(&path);
                let index = if let Some(&index) = resolved.get(&k) {
                    index
                } else {
                    let entry = fallback(&path);
                    let index = prepared.push(path, entry)?;
                    resolved.insert(k, index);
                    index
                };
                prepared.request.policies.push(Policy {
                    entry: index,
                    preference,
                    count,
                });
            }
            let mut groups = HashMap::new();
            for (i, path) in prepared.paths.iter().enumerate() {
                let k = key(path);
                let next = groups.len();
                let group = *groups.entry(k.clone()).or_insert(next);
                prepared.request.entries[i].group = group;
                prepared.request.entries[i].resolved = resolved[&k];
            }
            prepared.request.stamps = load_stamps(Path::new(&root), modifiers.as_ref(), &prepared)?;
        } else if policies.iter().any(|p| p.1 > 2 || p.2 == 0 || p.2 > 4096) {
            return Err("invalid monument policy".into());
        }
        prepared.request.validate()?;
        let textures = prepared
            .request
            .stamps
            .stamps
            .iter()
            .map(|s| s.width as u64 * s.height as u64 * 12)
            .max()
            .unwrap_or(0);
        let estimate = 160 * 1024 * 1024
            + 8 * (settings.height_resolution() as u64).pow(2)
            + 24 * (settings.surface_resolution() as u64).pow(2)
            + textures;
        if budget != 0 && estimate > budget {
            return Err(format!(
                "map and prefab stamps need an estimated {} MiB; increase the memory limit",
                estimate / 1048576
            ));
        }
        prepared.ids = prepared.paths.iter().map(|p| prefab_id(p)).collect();
        Ok(prepared)
    }
    fn push(&mut self, path: String, entry: Entry) -> Result<usize, String> {
        if self.paths.len() >= 65536 || path.len() > 4096 || path.contains('\0') {
            return Err("prefab registry exceeds limits".into());
        }
        let index = self.paths.len();
        self.paths.push(path);
        self.request.entries.push(entry);
        Ok(index)
    }
}

fn manifest(root: &Path) -> Result<Option<PathBuf>, String> {
    // Breadth first, like Directory.EnumerateFiles; do not follow directory links.
    let mut pending = std::collections::VecDeque::from([(root.to_path_buf(), 0)]);
    let mut visited = 0;
    while let Some((dir, depth)) = pending.pop_front() {
        if depth > 64 {
            return Err("cache directory depth exceeds limit".into());
        }
        for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            visited += 1;
            if visited > 65536 {
                return Err("cache directory count exceeds limit".into());
            }
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_dir() {
                pending.push_back((entry.path(), depth + 1));
            } else if kind.is_file()
                && entry
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case("manifest.json")
            {
                return Ok(Some(entry.path()));
            }
        }
    }
    Ok(None)
}
fn load_stamps(
    root: &Path,
    modifiers: Option<&Value>,
    prepared: &Prepared,
) -> Result<stamps::Request, String> {
    let Some(modifiers) = modifiers.and_then(|v| v["modifiers"].as_object()) else {
        return Ok(stamps::Request::default());
    };
    let Some(manifest) = manifest(root)? else {
        return Ok(stamps::Request::default());
    };
    let textures = json(&manifest)?.ok_or("cache manifest disappeared")?;
    let entries = textures["entries"]
        .as_object()
        .ok_or("invalid texture manifest entries")?;
    let groups: Vec<_> = entries
        .iter()
        .map(|(p, v)| (normalize(p.rsplit(['/', '\\']).next().unwrap_or(p)), v))
        .collect();
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let directory = manifest
        .parent()
        .ok_or("invalid manifest location")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let mut stamps = vec![];
    for (index, path) in prepared.paths.iter().enumerate() {
        if prepared.request.entries[index].resolved != index {
            continue;
        }
        let Some(modifier) = modifiers.get(path) else {
            continue;
        };
        let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
        let name = normalize(file.rsplit_once('.').map_or(file, |v| v.0));
        let matches: Vec<_> = groups
            .iter()
            .filter(|(n, _)| n == &name || n.starts_with(&name) || name.starts_with(n))
            .take(2)
            .collect();
        if matches.len() != 1 {
            continue;
        }
        let group = matches[0].1;
        let files = group["textures"]
            .as_object()
            .ok_or("invalid texture references")?;
        if !files.contains_key("blendtexture") {
            continue;
        }
        let width = integer(&group["width"])?;
        let height = integer(&group["height"])?;
        if !(1..=4096).contains(&width) || !(1..=4096).contains(&height) {
            return Err("invalid stamp texture dimensions".into());
        }
        let mut references: [String; 6] = Default::default();
        for (i, k) in [
            "blendtexture",
            "splattexture0",
            "splattexture1",
            "heighttexture",
            "alphatexture",
            "topologytexture",
        ]
        .iter()
        .enumerate()
        {
            let Some(value) = files.get(*k) else {
                continue;
            };
            let text = value
                .as_str()
                .ok_or("invalid texture reference")?
                .replace('\\', "/");
            // Resolve lexical parents before checking containment, including missing files.
            let mut full = directory.to_path_buf();
            for component in Path::new(&text).components() {
                match component {
                    Component::Normal(v) => full.push(v),
                    Component::CurDir => {}
                    Component::ParentDir => {
                        full.pop();
                    }
                    _ => return Err("prefab texture reference escapes its cache directory".into()),
                }
            }
            let relative = full
                .strip_prefix(&root)
                .map_err(|_| "prefab texture reference escapes its cache directory")?;
            references[i] = relative.to_string_lossy().replace('\\', "/");
        }
        let flags = u32::from(boolean(&modifier["heightMap"])?)
            | (u32::from(boolean(&modifier["alphaMap"])?) << 1)
            | (u32::from(boolean(&modifier["removeExistingTopology"])?) << 2)
            | (u32::from(
                files.contains_key("splattexture0") && files.contains_key("splattexture1"),
            ) << 3);
        stamps.push(stamps::Stamp {
            entry: index,
            width: width as usize,
            height: height as usize,
            size: vector(&modifier["size"])?,
            offset: vector(&modifier["offset"])?,
            splat_mask: integer(&modifier["splatMask"])?,
            topology_mask: integer(&modifier["topologyMask"])?,
            flags,
            textures: references,
        });
    }
    Ok(stamps::Request {
        root: root.to_string_lossy().into_owned(),
        stamps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn settings() -> crate::world::Settings {
        crate::world::Settings {
            size: 1000,
            seed: 12345,
            threads: 1,
            biomes: [40, 15, 15, 30],
            erode: false,
            rivers: crate::water::Rivers {
                count: 0,
                width: 8.0,
                maximum_length: 0,
            },
            roads: crate::paths::Roads {
                enabled: false,
                ring: false,
                branches: 0,
                width: 12.0,
            },
        }
    }
    fn text(bytes: &mut Vec<u8>, s: &str) {
        bytes.extend((s.len() as u32).to_le_bytes());
        bytes.extend(s.as_bytes());
    }
    fn frame(enabled: bool, policies: &[(&str, u32, u32)], budget: u64) -> Vec<u8> {
        let mut bytes = b"MAPR".to_vec();
        for word in [
            1,
            u32::from(enabled),
            0,
            0,
            1,
            budget as u32,
            (budget >> 32) as u32,
        ] {
            bytes.extend(word.to_le_bytes());
        }
        text(
            &mut bytes,
            &std::env::temp_dir()
                .join("mapper-absent-registry-fixture")
                .to_string_lossy(),
        );
        bytes.extend((policies.len() as u32).to_le_bytes());
        for (path, preference, count) in policies {
            text(&mut bytes, path);
            bytes.extend(preference.to_le_bytes());
            bytes.extend(count.to_le_bytes());
        }
        bytes
    }
    #[test]
    fn ordered_fallback_registry_and_identity_are_stable() {
        let p = Prepared::read(
            &frame(
                true,
                &[
                    (
                        "ASSETS/BUNDLED/PREFABS/AUTOSPAWN/MONUMENT/SMALL/SPHERE_TANK.PREFAB",
                        1,
                        1,
                    ),
                    ("assets/custom.prefab", 1, 1),
                ],
                0,
            ),
            settings(),
        )
        .unwrap();
        assert_eq!(p.paths.len(), 22);
        assert!(p.request.authored.is_empty());
        assert_eq!(p.request.catalog.len(), 21);
        assert_eq!(p.request.policies[0].entry, 0);
        assert_eq!(p.request.entries[0].priority, 8);
        assert_eq!(p.request.entries[0].minimum_size, 1000);
        assert_eq!(p.request.entries[20].extent_x, 150.0);
        assert_eq!(p.request.entries[21].extent_x, 45.0);
        assert_eq!(p.ids[0], prefab_id(&p.paths[0].to_uppercase()));
        assert!(p.request.stamps.stamps.is_empty());
    }
    #[test]
    fn malformed_and_excessive_requests_fail_before_generation() {
        let valid = frame(false, &[], 0);
        assert!(Prepared::read(&valid, settings()).unwrap().paths.is_empty());
        for end in 0..valid.len() {
            assert!(Prepared::read(&valid[..end], settings()).is_err());
        }
        let mut trailing = valid.clone();
        trailing.push(0);
        assert!(Prepared::read(&trailing, settings()).is_err());
        for policies in [
            vec![("assets/a", 1, 4097)],
            vec![("assets/a", 1, 3000), ("assets/b", 1, 3000)],
            vec![("assets/a", 3, 1)],
            vec![("", 1, 1)],
            vec![("assets/a", 1, 0)],
        ] {
            assert!(Prepared::read(&frame(true, &policies, 0), settings()).is_err());
        }
        assert!(
            Prepared::read(&frame(false, &[], 1), settings())
                .err()
                .unwrap()
                .contains("memory")
        );
    }
    #[test]
    fn invariant_casing_and_letter_filter_match_baseline_rules() {
        assert_eq!(lower("İΣẞ𐐀"), "İσß𐐨");
        assert_ne!(key("ſ"), key("s"));
        assert_ne!(key("ı"), key("i"));
        assert_eq!(key("éσ"), key("ÉΣ"));
        assert_eq!(key("ᾀᾳῳ"), key("ᾈᾼῼ"));
        assert_eq!(normalize("Terrain_12-é𐐀\u{345}"), "terrainé");
        assert_eq!(prefab_id("  "), 0);
    }
}
