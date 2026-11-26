//! Fixed placements of standalone RustEdit prefab assemblies.
use crate::{
    Presence, Project,
    rustedit_prefab::{Source, Version, read_version},
};
use papyrus_terrain::{
    content::{MAX_PREFABS, Prefab},
    decor_fit::rotate,
    prefab_records::{self, Instance, Record},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

mod baked;
pub(crate) use baked::compose_packages;
pub use baked::{add_package, add_package_with_manifest};

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Placement {
    pub version: String,
    pub position: [f32; 3],
    pub yaw: f32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default = "required", skip_serializing_if = "is_required")]
    pub presence: Presence,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub baked: bool,
}

fn required() -> Presence {
    Presence::Required
}
fn is_required(presence: &Presence) -> bool {
    *presence == Presence::Required
}

pub fn has_enabled_placements(project: &Project) -> bool {
    project
        .custom_monuments
        .iter()
        .any(|placement| placement.presence != Presence::Excluded)
}

pub fn add_placement(project: &mut Project, placement: Placement) -> Result<(), String> {
    if project.custom_monuments.iter().any(|old| {
        old.version == placement.version
            && old.baked == placement.baked
            && old.position == placement.position
            && old.yaw == placement.yaw
    }) {
        return Err("This custom monument is already placed at that position and rotation.".into());
    }
    project.custom_monuments.push(placement);
    if let Err(error) = validate(project) {
        project.custom_monuments.pop();
        return Err(error);
    }
    Ok(())
}

pub fn validate(project: &Project) -> Result<(), String> {
    if project.custom_monuments.is_empty() {
        return Ok(());
    }
    if !crate::native_preview::is_native(&project.preview_engine)
        || project.installation.is_none()
        || project.composition_engine.is_none()
    {
        return Err("Custom monuments require installation-backed generation".into());
    }
    if project.custom_monuments.len() > 256 {
        return Err("Custom monument placement limit exceeded".into());
    }
    for placement in &project.custom_monuments {
        Version::parse(&placement.version)?;
        if placement.name.len() > 128 || placement.name.chars().any(char::is_control) {
            return Err("Invalid custom monument name".into());
        }
        if !in_bounds(placement.position, project.size) || !(0. ..360.).contains(&placement.yaw) {
            return Err(
                "Custom monument requires an in-bounds position and a yaw from 0° to under 360°"
                    .into(),
            );
        }
    }
    Ok(())
}

fn in_bounds([x, y, z]: [f32; 3], size: u32) -> bool {
    let half = size as f32 * 0.5;
    (-half..=half).contains(&x) && (-half..=half).contains(&z) && (-10000. ..=10000.).contains(&y)
}

pub fn check_objects_only(source: &Source) -> Result<(), String> {
    if !source.terrain.is_empty() || !source.prefab.terrain_descriptor.is_empty() {
        return Err("This custom monument uses terrain modifiers. Standalone terrain stamping is not supported yet.".into());
    }
    if source.prefab.other_fields != 0
        || source
            .prefab
            .prefabs
            .iter()
            .any(|part| part.unknown_fields != 0)
    {
        return Err("This custom monument has RustEdit data that Mapper cannot replay yet.".into());
    }
    Ok(())
}

pub(crate) fn assemble(
    project: &Project,
    earlier_records: usize,
    installation_ids: &BTreeMap<String, u32>,
    cancel: impl Fn() -> bool,
) -> Result<Option<Vec<Record>>, String> {
    if !project.performance.monuments
        || !project
            .custom_monuments
            .iter()
            .any(|placement| !placement.baked && placement.presence != Presence::Excluded)
    {
        return Ok(Some(Vec::new()));
    }
    let paths: BTreeMap<_, _> = installation_ids
        .iter()
        .map(|(path, &id)| (id, path.as_str()))
        .collect();
    let mut loaded: BTreeMap<&str, Result<Vec<Prefab>, String>> = BTreeMap::new();
    let mut records = Vec::new();
    for placement in [Presence::Required, Presence::Allowed]
        .iter()
        .flat_map(|policy| {
            project
                .custom_monuments
                .iter()
                .filter(move |placement| !placement.baked && placement.presence == *policy)
        })
    {
        if cancel() {
            return Ok(None);
        }
        let parts = loaded.entry(&placement.version).or_insert_with(|| {
            let source = read_version(&Version::parse(&placement.version)?, &cancel)?;
            check_objects_only(&source)?;
            Ok(source.prefab.prefabs)
        });
        if cancel() {
            return Ok(None);
        }
        let result = parts.as_ref().map_err(Clone::clone).and_then(|parts| {
            let remaining = MAX_PREFABS.saturating_sub(earlier_records + records.len());
            place(
                project,
                placement,
                parts,
                &paths,
                installation_ids,
                remaining,
                &cancel,
            )
        });
        match result {
            Ok(Some(placed)) => records.extend(placed),
            Ok(None) => return Ok(None),
            Err(_) if placement.presence == Presence::Allowed => {}
            Err(error) => return Err(error),
        }
    }
    Ok((!cancel()).then_some(records))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_placement_policy_defaults_to_required_and_handles_missing_prefabs() {
        let legacy = serde_json::json!({
            "version": "not-a-version",
            "position": [0, 0, 0],
            "yaw": 0,
        });
        let placement: Placement = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(placement.presence, Presence::Required);
        assert_eq!(
            serde_json::to_value(&placement).unwrap()["version"],
            legacy["version"]
        );
        assert!(
            serde_json::to_value(&placement)
                .unwrap()
                .get("presence")
                .is_none()
        );

        let mut project = Project::default();
        project.performance.monuments = true;
        project.custom_monuments.push(placement);
        assert!(assemble(&project, 0, &BTreeMap::new(), || false).is_err());
        project.custom_monuments[0].presence = Presence::Allowed;
        assert!(has_enabled_placements(&project));
        assert!(
            assemble(&project, 0, &BTreeMap::new(), || false)
                .unwrap()
                .unwrap()
                .is_empty()
        );
        project.custom_monuments[0].presence = Presence::Excluded;
        assert!(!has_enabled_placements(&project));
        assert!(
            assemble(&project, 0, &BTreeMap::new(), || false)
                .unwrap()
                .unwrap()
                .is_empty()
        );
    }
}

fn place(
    project: &Project,
    placement: &Placement,
    parts: &[Prefab],
    paths: &BTreeMap<u32, &str>,
    installation_ids: &BTreeMap<String, u32>,
    remaining: usize,
    cancel: &impl Fn() -> bool,
) -> Result<Option<Vec<Record>>, String> {
    if parts.len() > remaining {
        return Err("Custom monuments exceed the playable map prefab limit".into());
    }
    let root = prefab_records::quaternion([0., placement.yaw, 0.])?;
    let mut instances = Vec::with_capacity(parts.len());
    for part in parts {
        if cancel() {
            return Ok(None);
        }
        let path = paths.get(&part.id).ok_or_else(|| {
            format!(
                "Custom monument {} needs prefab ID {} from the selected Rust installation",
                &placement.version[..8],
                part.id
            )
        })?;
        if project.options.export_blocked(path) {
            return Err(format!(
                "Custom monument {} needs a prefab blocked by this recipe. {path}",
                &placement.version[..8]
            ));
        }
        let offset = rotate(root, [part.position.x, part.position.y, part.position.z]);
        let position = std::array::from_fn(|i| placement.position[i] + offset[i]);
        if !in_bounds(position, project.size) {
            return Err(format!(
                "Custom monument {} leaves the map at this position",
                &placement.version[..8]
            ));
        }
        instances.push(Instance {
            category: if part.category.is_empty() {
                "Custom"
            } else {
                &part.category
            },
            path,
            position,
            // Unity's Euler convention applies world yaw last.
            rotation: prefab_records::quaternion([
                part.rotation.x,
                part.rotation.y + placement.yaw,
                part.rotation.z,
            ])?,
            scale: [part.scale.x, part.scale.y, part.scale.z],
        });
    }
    prefab_records::assemble(
        &instances,
        |path| installation_ids.get(path).copied(),
        cancel,
    )
}
