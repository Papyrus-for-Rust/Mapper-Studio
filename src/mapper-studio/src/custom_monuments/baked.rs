//! Fixed-pose standalone packages authored against a baked Rust map.
use super::{Placement, in_bounds};
use crate::{
    Presence, Project,
    content_package::{Binding, Imported, Manifest},
    prefab_assets::Catalog,
};
use papyrus_terrain::{
    content::{self, MAX_PREFABS, Vector},
    content_stamp::Pose,
    monument_bounds::{Bounds, Footprint},
    monument_composition::Composed,
    placement::Solution,
    prefab_records::{self, Instance},
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

struct Identities<'a> {
    by_id: BTreeMap<u32, &'a str>,
    by_path: &'a BTreeMap<String, u32>,
}

impl<'a> Identities<'a> {
    fn new(by_path: &'a BTreeMap<String, u32>) -> Self {
        let by_id = by_path
            .iter()
            .map(|(path, &id)| (id, path.as_str()))
            .collect();
        Self { by_id, by_path }
    }
}

fn package_path(version: &str) -> Result<PathBuf, String> {
    let version = crate::rustedit_prefab::Version::parse(version)?;
    Ok(crate::storage::local_data()?
        .join("custom-monument-packages")
        .join(format!("{}.mapperasset", version.as_str())))
}

fn validate_recipe_binding(package: &Imported, binding: &Binding) -> Result<(), String> {
    if package.terrain_stamp(binding)?.is_none() {
        return Err("Choose a standalone package with a terrain region in Import content.".into());
    }
    if !package.map.paths.is_empty()
        || package.map.unknown_fields != 0
        || package
            .map
            .prefabs
            .iter()
            .any(|part| part.unknown_fields != 0)
        || package.map.layers.iter().any(|layer| {
            !matches!(
                layer.name.as_str(),
                "terrain" | "height" | "water" | "splat" | "biome" | "topology" | "alpha"
            )
        })
    {
        return Err(
            "This package contains paths or extra data that Mapper cannot place yet.".into(),
        );
    }
    Ok(())
}

pub fn add_package(
    project: &mut Project,
    package: &Imported,
    position: [f32; 3],
    yaw: f32,
    cancel: impl Fn() -> bool,
) -> Result<(), String> {
    add_package_with_manifest(project, package, &package.manifest, position, yaw, cancel)
}

pub fn add_package_with_manifest(
    project: &mut Project,
    package: &Imported,
    manifest: &Manifest,
    position: [f32; 3],
    yaw: f32,
    cancel: impl Fn() -> bool,
) -> Result<(), String> {
    validate_recipe_binding(package, &manifest.binding)?;
    let version = package.package_digest(manifest)?;
    let path = package_path(&version)?;
    if cancel() {
        return Err("Custom monument import cancelled".into());
    }
    super::add_placement(
        project,
        Placement {
            version,
            position,
            yaw,
            name: manifest.name.clone(),
            presence: Presence::Required,
            baked: true,
        },
    )?;
    let saved = (|| {
        std::fs::create_dir_all(
            path.parent()
                .ok_or("Missing custom monument library folder")?,
        )
        .map_err(|error| error.to_string())?;
        package.save(manifest, &path)
    })();
    let result = saved.and_then(|()| {
        if cancel() {
            Err("Custom monument import cancelled".into())
        } else {
            Ok(())
        }
    });
    if result.is_err() {
        project.custom_monuments.pop();
    }
    result
}

fn footprints(
    project: &Project,
    solution: &Solution,
    catalog: &Catalog,
) -> Result<Vec<Bounds>, String> {
    let mut occupied = Vec::new();
    for outcome in &solution.outcomes {
        let Some(position) = &outcome.position else {
            continue;
        };
        let asset = project
            .targets
            .iter()
            .find(|target| target.instance_id == outcome.id)
            .and_then(|target| target.prefab_path.as_ref())
            .and_then(|path| catalog.entries.get(path))
            .ok_or("Custom monument clearance needs the resolved monument catalog")?;
        let (bounds, _) = crate::placement_preview::bounds_for_recipe(project, asset)?;
        occupied.push(bounds[0].at([position.x, position.z], position.yaw));
    }
    Ok(occupied)
}

pub(crate) fn compose_packages(
    project: &Project,
    solution: &Solution,
    catalog: &Catalog,
    root: &Path,
    maps: &mut Composed,
    cancel: &impl Fn() -> bool,
) -> Result<bool, String> {
    let has_packages = project.performance.monuments
        && project
            .custom_monuments
            .iter()
            .any(|p| p.baked && p.presence != Presence::Excluded);
    if !has_packages {
        return Ok(!cancel());
    }
    let Some(identities) = crate::prefab_identities::Source::prepare(root, catalog, cancel)? else {
        return Ok(false);
    };
    let identities = Identities::new(&identities.prefabs);
    let mut occupied = footprints(project, solution, catalog)?;
    let mut cached: Option<(&str, Result<Imported, String>)> = None;
    for policy in [Presence::Required, Presence::Allowed] {
        for placement in project
            .custom_monuments
            .iter()
            .filter(|p| p.baked && p.presence == policy)
        {
            if cancel() {
                return Ok(false);
            }
            if !cached
                .as_ref()
                .is_some_and(|(key, _)| *key == placement.version.as_str())
            {
                // Release the previous decoded map before loading the next one.
                drop(cached.take());
                let loaded = (|| {
                    let package = Imported::read_file(&package_path(&placement.version)?, cancel)?;
                    if package.package_digest(&package.manifest)? != placement.version {
                        return Err("Saved package differs from the recipe version".into());
                    }
                    Ok(package)
                })();
                cached = Some((placement.version.as_str(), loaded));
            }
            let (_, package) = cached.as_ref().expect("package was loaded");
            let applied = package.as_ref().map_err(Clone::clone).and_then(|package| {
                apply_package(
                    project,
                    package,
                    placement,
                    &identities,
                    &mut occupied,
                    maps,
                    cancel,
                )
            });
            if cancel() {
                return Ok(false);
            }
            match applied {
                Ok(true) => {}
                Ok(false) => return Ok(false),
                Err(_) if policy == Presence::Allowed => {}
                Err(error) => return Err(format!("Custom monument {}: {error}", placement.name)),
            }
        }
    }
    Ok(true)
}

fn apply_package(
    project: &Project,
    package: &Imported,
    placement: &Placement,
    identities: &Identities<'_>,
    occupied: &mut Vec<Bounds>,
    maps: &mut Composed,
    cancel: &impl Fn() -> bool,
) -> Result<bool, String> {
    let binding = &package.manifest.binding;
    validate_recipe_binding(package, binding)?;
    let Binding::Standalone {
        pivot,
        authoring_yaw,
        terrain: Some(region),
    } = binding
    else {
        unreachable!("recipe binding was validated");
    };
    let [x, y, z] = placement.position;
    let bounds = Footprint {
        center: [region.center[0] - pivot[0], region.center[1] - pivot[2]],
        extent: region.size.map(|size| size * 0.5),
    }
    .at([x, z], *authoring_yaw - placement.yaw);
    if occupied.iter().any(|previous| previous.intersects(&bounds)) {
        return Err("Terrain region overlaps another monument".into());
    }
    if package.map.prefabs.len() > MAX_PREFABS.saturating_sub(maps.custom_baked.len()) {
        return Err("Custom monument packages exceed the map prefab limit".into());
    }
    let transformed = content::compose(
        &package.map,
        binding.composition(),
        Vector { x, y, z },
        placement.yaw,
    )?;
    let mut instances = Vec::with_capacity(transformed.prefabs.len());
    for part in &transformed.prefabs {
        if cancel() {
            return Ok(false);
        }
        let path = *identities.by_id.get(&part.id).ok_or_else(|| {
            format!(
                "Prefab ID {} is missing from the selected Rust installation",
                part.id
            )
        })?;
        if project.options.export_blocked(path) {
            return Err(format!("Recipe blocks prefab {path}"));
        }
        let position = [part.position.x, part.position.y, part.position.z];
        if !in_bounds(position, project.size) {
            return Err("A prefab leaves the map at this position".into());
        }
        instances.push(Instance {
            category: if part.category.is_empty() {
                "Custom"
            } else {
                &part.category
            },
            path,
            position,
            scale: [part.scale.x, part.scale.y, part.scale.z],
            rotation: prefab_records::quaternion([
                part.rotation.x,
                part.rotation.y,
                part.rotation.z,
            ])?,
        });
    }
    let Some(records) = prefab_records::assemble(
        &instances,
        |path| identities.by_path.get(path).copied(),
        cancel,
    )?
    else {
        return Ok(false);
    };
    let stamp = package
        .terrain_stamp(binding)?
        .ok_or("Missing custom monument terrain crop")?;
    if stamp
        .apply(
            &mut maps.layers,
            &mut maps.alpha,
            Pose {
                position: placement.position,
                yaw: placement.yaw,
            },
            cancel,
        )?
        .is_none()
    {
        return Ok(false);
    }
    occupied.push(bounds);
    maps.custom_baked.extend(records);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content_package::TerrainRegion;
    use papyrus_terrain::{
        map_container::{self, World},
        native_layers::{self, Layers},
        prefab_records::Record,
    };

    fn world() -> Composed {
        let (hr, sr) = native_layers::dimensions(1000).unwrap();
        let layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![16_383; hr * hr],
            splat: [255, 0, 0, 0, 0, 0, 0, 0].repeat(sr * sr),
            biome: [0, 255, 0, 0, 0].repeat(sr * sr),
            topology: vec![0; sr * sr],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        papyrus_terrain::monument_composition::compose(&layers, &vec![255; sr * sr], &[], || false)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn package_admits_alpha_and_prefabs_together_at_an_oblique_pose() {
        let mut authored = world();
        let side = authored.layers.surface_resolution;
        let cell = |x: f32, z: f32| {
            let index = |p: f32| ((p / 1000. + 0.5) * side as f32) as usize;
            index(z) * side + index(x)
        };
        for z in 8..32 {
            for x in -8..8 {
                authored.alpha[cell(x as f32, z as f32)] = 0;
            }
        }
        let prefab = Record {
            category: "Custom".into(),
            path: "assets/trigger.prefab".into(),
            id: 91,
            position: [0., 20., 20.],
            rotation: [0., 12., 0.],
            scale: [1.; 3],
        };
        let mut bytes = Vec::new();
        assert!(
            map_container::write_v10(
                World {
                    layers: &authored.layers,
                    height: &authored.layers.height,
                    water: None,
                    alpha: &authored.alpha,
                    prefabs: &[prefab],
                    paths: &[],
                },
                1,
                &mut bytes,
                || false
            )
            .unwrap()
            .is_some()
        );
        let mut package = Imported::read_bytes(&bytes, "Cutout", || false).unwrap();
        package.manifest.binding = Binding::Standalone {
            pivot: [0., 20., 20.],
            authoring_yaw: 12.,
            terrain: Some(TerrainRegion {
                center: [0., 20.],
                size: [100.; 2],
                fade: 10.,
                height: false,
                splat: false,
                biome: false,
                topology: false,
                alpha: true,
            }),
        };
        let mut maps = world();
        let placement = Placement {
            version: package.package_digest(&package.manifest).unwrap(),
            name: "Cutout".into(),
            position: [200., 30., -100.],
            yaw: 37.,
            baked: true,
            presence: Presence::Required,
        };
        let project = Project {
            size: 1000,
            ..Default::default()
        };
        let mut occupied = Vec::new();
        assert!(
            apply_package(
                &project,
                &package,
                &placement,
                &Identities::new(&BTreeMap::new()),
                &mut occupied,
                &mut maps,
                &|| false
            )
            .is_err()
        );
        assert!(maps.custom_baked.is_empty() && occupied.is_empty());
        assert!(maps.alpha.iter().all(|alpha| *alpha == 255));

        let paths = BTreeMap::from([("assets/trigger.prefab".to_owned(), 91)]);
        let identities = Identities::new(&paths);
        assert!(
            apply_package(
                &project,
                &package,
                &placement,
                &identities,
                &mut occupied,
                &mut maps,
                &|| false
            )
            .unwrap()
        );
        assert_eq!(maps.alpha[cell(200., -100.)], 0);
        assert_eq!(maps.alpha[cell(0., 20.)], 255);
        assert_eq!(maps.custom_baked[0].position, placement.position);
        assert!((maps.custom_baked[0].rotation[1] - 37.).abs() < 0.001);
        assert_eq!(maps.custom_baked[0].id, 91);
        assert!(
            apply_package(
                &project,
                &package,
                &placement,
                &identities,
                &mut occupied,
                &mut maps,
                &|| false
            )
            .is_err()
        );
        assert_eq!(maps.custom_baked.len(), 1);
    }
}
