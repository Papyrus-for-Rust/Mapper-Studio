//! Portable prefab attachments for generated monument roots.
use crate::content_package::{Binding, Imported};
use papyrus_terrain::{
    content::{self, Vector},
    environment::{multiply, rotate},
    prefab_records::{self, Record},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
    sync::Arc,
};

const MAX_ADDONS: usize = 64;
const MAX_CHILDREN: usize = 4096;
const MAX_VERSIONS_PER_MONUMENT: usize = 256;

/// Selected library version per stock prefab ID.
pub type Choices = BTreeMap<u32, String>;

pub fn validate_choices(choices: &Choices, legacy: &[AddOn]) -> Result<(), String> {
    if choices.len() + legacy.len() > MAX_ADDONS
        || choices.iter().any(|(&anchor, key)| {
            anchor == 0 || !valid_key(key) || legacy.iter().any(|addon| addon.anchor_id == anchor)
        })
    {
        return Err("Invalid monument overlay choices in the recipe".into());
    }
    Ok(())
}

fn valid_key(key: &str) -> bool {
    key.len() == 64
        && key
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

pub struct Library(PathBuf);

pub struct Version {
    pub key: String,
    pub name: String,
    pub objects: usize,
}

impl Library {
    pub fn local() -> Result<Self, String> {
        let settings = crate::preferences::Preferences::path()?;
        Ok(Self(
            settings
                .parent()
                .ok_or("Mapper settings folder is unavailable")?
                .join("monument-overlays"),
        ))
    }

    fn directory(&self, anchor: u32) -> PathBuf {
        self.0.join(anchor.to_string())
    }

    fn key(addon: &AddOn) -> Result<String, String> {
        let data = serde_json::to_vec(&(addon.anchor_id, &addon.children))
            .map_err(|error| error.to_string())?;
        Ok(format!("{:x}", Sha256::digest(data)))
    }

    pub fn store(&self, addon: &AddOn) -> Result<String, String> {
        validate(std::slice::from_ref(addon))?;
        let key = Self::key(addon)?;
        let directory = self.directory(addon.anchor_id);
        let file = directory.join(format!("{key}.json"));
        if file.exists() {
            if self.load(addon.anchor_id, &key).is_err() {
                crate::profile::write_document(&file, addon, true)?;
            }
            return Ok(key);
        }
        if self.keys(addon.anchor_id)?.len() >= MAX_VERSIONS_PER_MONUMENT {
            return Err("This monument already has 256 overlay versions".into());
        }
        fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        if let Err(error) = crate::profile::write_document(&file, addon, false) {
            // An identical import can win publication in another process.
            if !file.exists() {
                return Err(error);
            }
            self.load(addon.anchor_id, &key)?;
        }
        Ok(key)
    }

    pub fn load(&self, anchor: u32, key: &str) -> Result<AddOn, String> {
        if anchor == 0 || !valid_key(key) {
            return Err("Invalid monument overlay reference".into());
        }
        let file = self.directory(anchor).join(format!("{key}.json"));
        let addon: AddOn = crate::profile::read_json(&file).map_err(|error| {
            format!("Overlay {} for prefab ID {anchor} is unavailable. Import its source map or choose another version. {error}", &key[..10])
        })?;
        validate(std::slice::from_ref(&addon))?;
        if addon.anchor_id != anchor || Self::key(&addon)? != key {
            return Err("Monument overlay library entry has changed. Import the original source as a new version.".into());
        }
        Ok(addon)
    }

    fn keys(&self, anchor: u32) -> Result<Vec<String>, String> {
        let files = match fs::read_dir(self.directory(anchor)) {
            Ok(files) => files,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.to_string()),
        };
        let mut keys = Vec::new();
        for file in files {
            let file = file.map_err(|error| error.to_string())?;
            let filename = file.file_name();
            let Some(key) = filename
                .to_str()
                .and_then(|name| name.strip_suffix(".json"))
                .filter(|name| valid_key(name))
            else {
                continue;
            };
            if keys.len() == MAX_VERSIONS_PER_MONUMENT {
                return Err("This monument's library exceeds 256 overlay versions".into());
            }
            keys.push(key.into());
        }
        Ok(keys)
    }

    pub fn versions(&self, anchor: u32) -> Result<Vec<Version>, String> {
        self.versions_cancellable(anchor, || false)
    }

    pub fn versions_cancellable(&self, anchor: u32, cancelled: impl Fn() -> bool) -> Result<Vec<Version>, String> {
        let mut versions = Vec::new();
        for key in self.keys(anchor)? {
            if cancelled() {
                return Err("Overlay library scan cancelled".into());
            }
            if let Ok(addon) = self.load(anchor, &key) {
                versions.push(Version {
                    key,
                    name: addon.name,
                    objects: addon.children.len(),
                });
            }
        }
        versions.sort_unstable_by(|a, b| a.name.cmp(&b.name).then(a.key.cmp(&b.key)));
        Ok(versions)
    }

    pub fn save_embedded(&self, project: &crate::Project, anchor: u32) -> Result<(), String> {
        for addon in project
            .monument_addons
            .iter()
            .filter(|addon| addon.anchor_id == anchor)
        {
            self.store(addon)?;
        }
        Ok(())
    }

    fn resolve(&self, project: &crate::Project) -> Result<Vec<AddOn>, OverlayIssue> {
        let mut addons = project.monument_addons.clone();
        for (&anchor, version) in &project.monument_overlay_choices {
            addons.push(self.load(anchor, version).map_err(|message| OverlayIssue {
                anchor: Some(anchor), message,
            })?);
        }
        validate(&addons).map_err(OverlayIssue::from)?;
        Ok(addons)
    }
}

#[derive(Clone, Debug)]
pub struct OverlayIssue {
    pub anchor: Option<u32>,
    pub message: String,
}

impl From<String> for OverlayIssue {
    fn from(message: String) -> Self {
        Self { anchor: None, message }
    }
}

pub fn resolve(project: &crate::Project) -> Result<Vec<AddOn>, String> {
    resolve_with_issue(project).map_err(|issue| issue.message)
}

fn resolve_with_issue(project: &crate::Project) -> Result<Vec<AddOn>, OverlayIssue> {
    if project.monument_overlay_choices.is_empty() {
        return Ok(project.monument_addons.clone());
    }
    Library::local()?.resolve(project)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddOn {
    pub name: String,
    pub source_sha256: String,
    pub anchor_index: usize,
    pub anchor_id: u32,
    pub children: Arc<[Child]>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Child {
    pub id: u32,
    pub category: String,
    pub offset: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: [f32; 3],
}

fn label(text: &str) -> bool {
    !text.trim().is_empty() && text.len() <= 128 && !text.chars().any(char::is_control)
}

fn vector(values: [f32; 3], limit: f32) -> bool {
    values.iter().all(|v| v.is_finite() && v.abs() <= limit)
}

fn stock_anchor(path: &str, catalog: &crate::prefab_assets::Catalog) -> bool {
    path.contains("/monument/") || catalog.entries.contains_key(path)
}

fn missing_anchor(addon: &AddOn) -> String {
    format!(
        "{} needs a stock monument anchor from the selected Rust installation",
        addon.name
    )
}

fn missing_child(addon: &AddOn, id: u32) -> String {
    format!(
        "{} references prefab ID {id} which is missing from the selected Rust installation",
        addon.name
    )
}

/// Compact, read-only installation index for checking recipes before generation.
pub struct Dependencies(Vec<(u32, bool)>);

impl Dependencies {
    pub fn check_recipe(&self, project: &crate::Project) -> Result<(), OverlayIssue> {
        for addon in resolve_with_issue(project)? {
            self.check(std::slice::from_ref(&addon)).map_err(|message| OverlayIssue {
                anchor: Some(addon.anchor_id), message,
            })?;
        }
        Ok(())
    }

    pub fn new(
        source: &crate::prefab_identities::Source,
        catalog: &crate::prefab_assets::Catalog,
    ) -> Self {
        let mut ids = source
            .prefabs
            .iter()
            .map(|(path, &id)| (id, stock_anchor(path, catalog)))
            .collect::<Vec<_>>();
        ids.sort_unstable_by_key(|&(id, _)| id);
        Self(ids)
    }

    pub fn check(&self, addons: &[AddOn]) -> Result<(), String> {
        let find = |id| {
            self.0
                .binary_search_by_key(&id, |&(candidate, _)| candidate)
        };
        for addon in addons {
            if !find(addon.anchor_id).is_ok_and(|index| self.0[index].1) {
                return Err(missing_anchor(addon));
            }
            for child in addon.children.iter() {
                if find(child.id).is_err() {
                    return Err(missing_child(addon, child.id));
                }
            }
        }
        Ok(())
    }
}

pub fn validate(addons: &[AddOn]) -> Result<(), String> {
    if addons.len() > MAX_ADDONS {
        return Err(format!(
            "A recipe can contain up to {MAX_ADDONS} monument add-ons"
        ));
    }
    let mut keys = BTreeSet::new();
    let mut count = 0;
    for addon in addons {
        if !label(&addon.name)
            || addon.source_sha256.len() != 64
            || !addon
                .source_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || addon.anchor_id == 0
            || addon.anchor_index >= content::MAX_PREFABS
            || addon.children.is_empty()
            || addon.children.len() > MAX_CHILDREN - count
        {
            return Err("Invalid monument add-on or recipe object limit exceeded".into());
        }
        if !keys.insert((&addon.source_sha256, addon.anchor_index)) {
            return Err("This monument add-on is already in the recipe".into());
        }
        count += addon.children.len();
        for child in addon.children.iter() {
            if child.id == 0
                || !label(&child.category)
                || !vector(child.offset, 10000.)
                || !vector(child.rotation, 360000.)
                || !vector(child.scale, 100.)
                || child.scale.contains(&0.)
            {
                return Err(format!(
                    "{} contains an invalid object transform or category",
                    addon.name
                ));
            }
        }
    }
    Ok(())
}

pub fn import(item: &Imported, name: String, anchor_index: usize) -> Result<AddOn, String> {
    if !(2..=MAX_CHILDREN + 1).contains(&item.map.prefabs.len()) {
        return Err(format!(
            "An add-on needs a stock anchor and 1–{MAX_CHILDREN} child objects"
        ));
    }
    if !item.map.paths.is_empty() {
        return Err(
            "Remove authored paths from the add-on map before adding it to a recipe".into(),
        );
    }
    if item.map.prefabs.iter().any(|p| p.unknown_fields != 0) {
        return Err("Add-on objects contain unsupported map fields".into());
    }
    let binding = Binding::AddOn { anchor_index };
    let local = content::compose(&item.map, binding.composition(), Vector::default(), 0.)?;
    let addon = AddOn {
        name,
        source_sha256: item.manifest.source_sha256.clone(),
        anchor_index,
        anchor_id: local.anchor_id.ok_or("Stock monument anchor is missing")?,
        children: local
            .prefabs
            .into_iter()
            .map(|p| Child {
                id: p.id,
                category: if p.category.is_empty() {
                    "Custom".into()
                } else {
                    p.category
                },
                offset: [p.position.x, p.position.y, p.position.z],
                rotation: [p.rotation.x, p.rotation.y, p.rotation.z],
                scale: [p.scale.x, p.scale.y, p.scale.z],
            })
            .collect(),
    };
    validate(std::slice::from_ref(&addon))?;
    Ok(addon)
}

/// Use the selected installation prefab as the overlay frame.
pub fn import_for_anchor(item: &Imported, anchor_id: u32) -> Result<AddOn, String> {
    let index = match item.manifest.binding {
        Binding::AddOn { anchor_index }
            if item
                .map
                .prefabs
                .get(anchor_index)
                .is_some_and(|prefab| prefab.id == anchor_id) =>
        {
            anchor_index
        }
        Binding::AddOn { .. } => {
            return Err("This package is anchored to a different monument".into());
        }
        Binding::Standalone { .. } => {
            let mut matching = item
                .map
                .prefabs
                .iter()
                .enumerate()
                .filter(|(_, prefab)| prefab.id == anchor_id);
            let (index, _) = matching
                .next()
                .ok_or("The overlay map is missing this monument's stock prefab")?;
            if matching.next().is_some() {
                return Err(
                    "The overlay map contains more than one copy of the selected monument".into(),
                );
            }
            index
        }
    };
    import(item, item.manifest.name.clone(), index)
}

/// Attach only to original stock roots. The returned records can be committed
/// atomically with the rest of the accepted map. A skipped optional monument
/// has no root and produces no children.
pub(crate) fn assemble(
    addons: &[AddOn],
    roots: &[Record],
    identities: &BTreeMap<String, u32>,
    catalog: &crate::prefab_assets::Catalog,
    cancel: impl Fn() -> bool,
) -> Result<Option<Vec<Record>>, String> {
    if addons.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let paths: BTreeMap<_, _> = identities.iter().map(|(path, &id)| (id, path)).collect();
    let mut result = Vec::new();
    for addon in addons {
        if cancel() {
            return Ok(None);
        }
        let path = paths
            .get(&addon.anchor_id)
            .filter(|path| stock_anchor(path, catalog))
            .ok_or_else(|| missing_anchor(addon))?;
        let children: Vec<_> = addon
            .children
            .iter()
            .map(|child| {
                let path = paths
                    .get(&child.id)
                    .ok_or_else(|| missing_child(addon, child.id))?;
                Ok((child, path))
            })
            .collect::<Result<_, String>>()?;
        for root in roots
            .iter()
            .filter(|root| root.category == "Monument" && root.path == path.as_str())
        {
            if children.len() > content::MAX_PREFABS.saturating_sub(roots.len() + result.len()) {
                return Err("Monument add-ons exceed the playable map prefab limit".into());
            }
            let [scale, y, z] = root.scale;
            if scale <= 0. || (y - scale).abs() > 0.000001 || (z - scale).abs() > 0.000001 {
                return Err(format!(
                    "{} requires an anchor with uniform positive scale",
                    addon.name
                ));
            }
            let rotation = prefab_records::quaternion(root.rotation)?;
            let mut instances = Vec::with_capacity(children.len());
            for (child, path) in &children {
                if cancel() {
                    return Ok(None);
                }
                let offset = rotate(rotation, child.offset.map(|v| v * scale));
                instances.push(prefab_records::Instance {
                    category: &child.category,
                    path,
                    position: std::array::from_fn(|i| root.position[i] + offset[i]),
                    rotation: multiply(rotation, prefab_records::quaternion(child.rotation)?),
                    scale: child.scale.map(|v| v * scale),
                });
            }
            let Some(records) =
                prefab_records::assemble(&instances, |p| identities.get(p).copied(), &cancel)?
            else {
                return Ok(None);
            };
            result.extend(records);
        }
    }
    Ok((!cancel()).then_some(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::installation::{Installation, InstallationKind, SourceFile};

    const STOCK: &str = "assets/bundled/prefabs/autospawn/monument/harbor/test.prefab";
    const PROP: &str = "assets/bundled/prefabs/objects/test.prefab";

    #[test]
    fn library_versions_survive_recipe_switches_and_legacy_migration() {
        let directory = tempfile::tempdir().unwrap();
        let library = Library(directory.path().to_owned());
        let mut recipe = crate::Project {
            installation: Some(Installation {
                kind: InstallationKind::Client,
                native: SourceFile {
                    relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                    bytes: 1,
                    sha256: "a".repeat(64),
                },
                scene_manifest: None,
            }),
            preview_engine: crate::native_preview::VERSION.into(),
            ..Default::default()
        };
        let old = AddOn {
            name: "Weekend Outpost".into(),
            source_sha256: "b".repeat(64),
            anchor_index: 0,
            anchor_id: 11,
            children: vec![Child {
                id: 12,
                category: "Custom".into(),
                offset: [10., 0., 0.],
                rotation: [0.; 3],
                scale: [1.; 3],
            }]
            .into(),
        };
        recipe.add_monument_addon(old.clone()).unwrap();
        library.save_embedded(&recipe, 11).unwrap();
        let first = library.store(&old).unwrap();
        let mut variant = old.clone();
        Arc::make_mut(&mut variant.children)[0].offset[0] = 20.;
        let second = library.store(&variant).unwrap();
        assert_ne!(first, second);
        assert_eq!(library.versions(11).unwrap().len(), 2);
        recipe
            .choose_monument_overlay(11, Some(first.clone()))
            .unwrap();
        let mut other = recipe.clone();
        other.choose_monument_overlay(11, Some(second)).unwrap();
        let saved = serde_json::to_value(&recipe).unwrap();
        assert!(saved.get("monument_addons").is_none());
        assert_eq!(saved["monument_overlay_choices"]["11"], first);
        let reopened = Library(directory.path().to_owned());
        assert_eq!(reopened.resolve(&recipe).unwrap(), [old]);
        assert_eq!(reopened.resolve(&other).unwrap(), [variant]);
        let missing = Library(directory.path().join("another-machine"))
            .resolve(&recipe).unwrap_err();
        assert_eq!(missing.anchor, Some(11));
        assert!(missing.message.contains("Import its source map"));
        other.choose_monument_overlay(11, None).unwrap();
        assert!(reopened.resolve(&other).unwrap().is_empty());
        assert_eq!(reopened.versions(11).unwrap().len(), 2);
    }

    #[test]
    fn imported_addon_replays_at_each_stock_pose_and_rejects_missing_dependencies() {
        fn field(out: &mut Vec<u8>, tag: u8, bytes: &[u8]) {
            out.extend([tag, bytes.len().try_into().unwrap()]);
            out.extend(bytes);
        }
        fn prefab(id: u8, position: [f32; 3], yaw: f32) -> Vec<u8> {
            let mut bytes = vec![16, id];
            for (tag, values) in [(26, position), (34, [0., yaw, 0.]), (42, [1.; 3])] {
                let mut vector = Vec::new();
                for (key, v) in [13, 21, 29].into_iter().zip(values) {
                    vector.push(key);
                    vector.extend(v.to_le_bytes());
                }
                field(&mut bytes, tag, &vector);
            }
            bytes
        }
        // Version 10 and a single raw legacy block. Anchor has its own offset
        // and yaw, so reproducing just the absolute child transform would fail.
        let mut source = 10_u32.to_le_bytes().to_vec();
        source.extend(0_i64.to_le_bytes());
        let mut world = vec![8, 232, 7];
        field(&mut world, 26, &prefab(11, [10., 2., 20.], 90.));
        field(&mut world, 26, &prefab(12, [10., 5., 18.], 120.));
        field(&mut source, 0, &world);
        let imported = Imported::read_bytes(&source, "Props", || false).unwrap();
        let addon = import(&imported, "Props".into(), 0).unwrap();
        assert_eq!(import_for_anchor(&imported, 11).unwrap(), addon);
        assert!(import_for_anchor(&imported, 13).is_err());
        assert_eq!(addon.anchor_id, 11);
        assert_eq!(addon.children.len(), 1);
        assert_eq!(addon.children[0].id, 12);

        let installation = Installation {
            kind: InstallationKind::Client,
            native: SourceFile {
                relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                bytes: 1,
                sha256: "a".repeat(64),
            },
            scene_manifest: None,
        };
        let mut recipe = crate::Project {
            installation: Some(installation.clone()),
            preview_engine: crate::native_preview::VERSION.into(),
            ..Default::default()
        };
        recipe.add_monument_addon(addon.clone()).unwrap();
        assert!(recipe.add_monument_addon(addon).is_err());
        let json = serde_json::to_vec(&recipe).unwrap();
        let restored: crate::Project = serde_json::from_slice(&json).unwrap();
        assert_eq!(restored, recipe);
        restored.validate().unwrap();

        let catalog = crate::prefab_assets::Catalog {
            sha256: String::new(),
            installation,
            entries: Default::default(),
            sources: Vec::new(),
        };
        let roots = [0., 90.].map(|yaw| Record {
            category: "Monument".into(),
            path: STOCK.into(),
            id: 11,
            position: [50., 10., 60.],
            rotation: [0., yaw, 0.],
            scale: [1.; 3],
        });
        let mut identities = BTreeMap::from([(STOCK.into(), 11), (PROP.into(), 12)]);
        let addons = &restored.monument_addons;
        let mut source = crate::prefab_identities::Source {
            prefabs: identities.clone(),
            source: SourceFile {
                relative_path: "Bundles/shared/content.bundle".into(),
                bytes: 1,
                sha256: "b".repeat(64),
            },
            sha256: "c".repeat(64),
        };
        let dependencies = Dependencies::new(&source, &catalog);
        dependencies.check(addons).unwrap();
        let mut invalid_anchor = addons[0].clone();
        invalid_anchor.anchor_id = 12;
        assert!(
            dependencies
                .check(&[invalid_anchor])
                .unwrap_err()
                .contains("stock monument anchor")
        );
        let records = assemble(addons, &roots, &identities, &catalog, || false)
            .unwrap()
            .unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].position, [52., 13., 60.]);
        assert_eq!(records[1].position, [50., 13., 58.]);
        for (record, yaw) in records.iter().zip([30., 120.]) {
            assert_eq!(record.id, 12);
            assert_eq!(record.path, PROP);
            assert!((record.rotation[1] - yaw).abs() < 0.0001);
        }
        assert!(
            assemble(addons, &[], &identities, &catalog, || false)
                .unwrap()
                .unwrap()
                .is_empty()
        );
        assert!(
            assemble(addons, &roots, &identities, &catalog, || true)
                .unwrap()
                .is_none()
        );
        identities.remove(PROP);
        source.prefabs.remove(PROP);
        let export_error = assemble(addons, &roots, &identities, &catalog, || false).unwrap_err();
        assert!(export_error.contains("prefab ID 12"));
        assert_eq!(
            Dependencies::new(&source, &catalog).check(addons),
            Err(export_error)
        );
    }
}
