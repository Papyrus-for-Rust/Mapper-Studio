//! Installation-bound river attachments, published before monument placement.
use crate::{
    infrastructure,
    installation::Installation,
    prefab_assets,
    river_generation::Key,
    road_object_placement::{self, Road, Settings},
    road_object_source::{PathFamily, Source},
};
use papyrus_terrain::{
    dungeon_base::Pose,
    native_layers::Layers,
    prefab_terrain_effects as terrain,
    river_pipeline::{Object, Objects, World},
    spawn::Filter,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Read, path::Path, sync::Arc};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub source_sha256: String,
    pub infrastructure_sha256: String,
    pub catalog_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documents: Option<Documents>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Documents {
    catalog: Arc<str>,
    infrastructure: Arc<str>,
    objects: Arc<str>,
}
const DOCUMENT_LIMIT: usize = 16 * 1024 * 1024;
impl Recipe {
    pub fn validate(&self) -> Result<(), String> {
        for hash in [
            &self.source_sha256,
            &self.infrastructure_sha256,
            &self.catalog_sha256,
        ] {
            if hash.len() != 64
                || !hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err("Invalid river object source binding".into());
            }
        }
        if let Some(documents) = &self.documents {
            for (raw, expected) in [
                (&documents.catalog, &self.catalog_sha256),
                (&documents.infrastructure, &self.infrastructure_sha256),
                (&documents.objects, &self.source_sha256),
            ] {
                if raw.is_empty()
                    || raw.len() > DOCUMENT_LIMIT
                    || format!("{:x}", Sha256::digest(raw.as_bytes())) != *expected
                {
                    return Err("River object document differs from its saved identity".into());
                }
            }
        }
        Ok(())
    }
    pub fn bind(source: &Source, parent: &infrastructure::Source) -> Result<Self, String> {
        if source.path_family != PathFamily::River || source.infrastructure_sha256 != parent.sha256
        {
            return Err("River objects differ from their prepared infrastructure".into());
        }
        let recipe = Self {
            source_sha256: source.sha256.clone(),
            infrastructure_sha256: parent.sha256.clone(),
            catalog_sha256: parent.catalog_sha256.clone(),
            documents: None,
        };
        recipe.validate()?;
        Ok(recipe)
    }
    pub fn prepare_export(&self) -> Result<Self, String> {
        self.validate()?;
        if self.documents.is_some() {
            return Ok(self.clone());
        }
        let local = crate::storage::local_data()?;
        let read = |path: &Path| -> Result<Arc<str>, String> {
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .map_err(|e| format!("Missing saved river object data {}: {e}", path.display()))?
                .take((DOCUMENT_LIMIT + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > DOCUMENT_LIMIT {
                return Err("River object document exceeds 16 MiB".into());
            }
            Ok(String::from_utf8(bytes).map_err(|e| e.to_string())?.into())
        };
        let documents = Documents {
            catalog: read(&crate::asset_discovery::catalog_path(
                &local.join("papyrus-prefab-cache"),
                &self.catalog_sha256,
            )?)?,
            infrastructure: read(
                &crate::client_cache::pinned_document(
                    &local,
                    "papyrus-infrastructure-cache",
                    &self.infrastructure_sha256,
                    "infrastructure.json",
                )?,
            )?,
            objects: read(
                &crate::client_cache::pinned_document(
                    &local,
                    "papyrus-river-object-cache",
                    &self.source_sha256,
                    "objects.json",
                )?,
            )?,
        };
        let mut recipe = self.clone();
        recipe.documents = Some(documents);
        recipe.validate()?;
        Ok(recipe)
    }
    pub(crate) fn load(
        &self,
        root: &Path,
        installation: &Installation,
        cancel: &impl Fn() -> bool,
    ) -> Result<Source, String> {
        self.validate()?;
        let local = crate::storage::local_data()?;
        let catalog = if let Some(documents) = &self.documents {
            prefab_assets::load_document(root, documents.catalog.as_bytes(), installation, cancel)?
        } else {
            let path = crate::asset_discovery::catalog_path(
                &local.join("papyrus-prefab-cache"),
                &self.catalog_sha256,
            )?;
            prefab_assets::load(root, &path, installation, cancel)?
        };
        if catalog.sha256 != self.catalog_sha256 {
            return Err("River object catalog changed".into());
        }
        let parent = if let Some(documents) = &self.documents {
            infrastructure::Source::load_document(
                documents.infrastructure.as_bytes(),
                &catalog,
                cancel,
            )?
        } else {
            infrastructure::Source::load(
                &crate::client_cache::pinned_document(
                    &local,
                    "papyrus-infrastructure-cache",
                    &self.infrastructure_sha256,
                    "infrastructure.json",
                )?,
                &catalog,
                cancel,
            )?
        };
        if parent.sha256 != self.infrastructure_sha256 {
            return Err("River object infrastructure changed".into());
        }
        let source = if let Some(documents) = &self.documents {
            Source::load_document(root, documents.objects.as_bytes(), &parent, cancel)?
        } else {
            Source::load(
                root,
                &crate::client_cache::pinned_document(
                    &local,
                    "papyrus-river-object-cache",
                    &self.source_sha256,
                    "objects.json",
                )?,
                &parent,
                cancel,
            )?
        };
        if source.sha256 != self.source_sha256 || source.path_family != PathFamily::River {
            return Err("River object source changed".into());
        }
        Ok(source)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Attribute {
    kind: String,
    #[serde(rename = "root")]
    _root: bool,
    position: [f32; 3],
    local_to_root: [[f32; 4]; 4],
    fields: Topology,
    references: Vec<serde_json::Value>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct Topology {
    opacity: f32,
    radius: f32,
    fade: f32,
    topology_type: i32,
}
fn topology_independent(filter: &Filter) -> bool {
    filter.topology_any == -1 && filter.topology_all == 0 && filter.topology_not == 0
}
fn effects(source: &Source) -> Result<BTreeMap<String, terrain::Prepared>, String> {
    // These reviewed river templates only write topology. Their path rules have
    // no topology dependency, so ordered effects can follow fitting without
    // changing any later admission. Reject new semantics until modeled.
    for stage in &source.stages {
        if stage
            .start
            .iter()
            .chain(&stage.end)
            .any(|r| !topology_independent(&r.filter))
            || stage.path.iter().any(|r| !topology_independent(&r.filter))
            || stage.side.iter().any(|r| !topology_independent(&r.filter))
        {
            return Err("River object rules require live topology-dependent fitting".into());
        }
    }
    let environments = source
        .environments
        .as_ref()
        .ok_or("Missing river environment roster")?;
    let mut prepared = BTreeMap::new();
    for (path, prefab) in &source.prefabs {
        let environment = environments
            .get(path)
            .ok_or("Missing river prefab environment")?;
        if !environment.volumes.is_empty()
            || !environment.checks.is_empty()
            || !environment.issues.is_empty()
            || environment
                .water_placement
                .as_ref()
                .is_some_and(|w| !w.bodies.is_empty() || !w.issues.is_empty())
        {
            return Err(format!(
                "River prefab requires environment registration: {path}"
            ));
        }
        let mut compiled = Vec::new();
        for raw in &prefab.attributes {
            let attribute: Attribute = serde_json::from_value(raw.clone())
                .map_err(|e| format!("Unsupported river terrain attribute: {e}"))?;
            let fields = attribute.fields;
            if !attribute.references.is_empty()
                || !fields.opacity.is_finite()
                || !(0. ..=1.).contains(&fields.opacity)
                || attribute
                    .local_to_root
                    .iter()
                    .flatten()
                    .any(|v| !v.is_finite())
            {
                return Err("Invalid river terrain attribute".into());
            }
            let effect = match attribute.kind.as_str() {
                "TerrainTopologyAdd" => terrain::Effect::AddTopology {
                    position: attribute.position,
                    mask: fields.topology_type as u32,
                    radius: fields.radius,
                    fade: fields.fade,
                },
                "TerrainTopologySet" => terrain::Effect::SetTopology {
                    position: attribute.position,
                    mask: fields.topology_type as u32,
                    radius: fields.radius,
                    fade: fields.fade,
                },
                _ => {
                    return Err(format!(
                        "Unsupported river terrain effect: {}",
                        attribute.kind
                    ));
                }
            };
            compiled.push(effect);
        }
        prepared.insert(path.clone(), terrain::Prepared::new(compiled)?);
    }
    Ok(prepared)
}

pub fn generate(
    layers: &mut Layers,
    world: &mut World,
    root: &Path,
    installation: &Installation,
    key: &Key,
    cancel: impl Fn() -> bool,
) -> Result<bool, String> {
    let Some(binding) = &key.recipe.objects else {
        return Ok(true);
    };
    key.validate()?;
    if world.objects.is_some() {
        return Err("River objects have already been placed".into());
    }
    if cancel() {
        return Ok(false);
    }
    let source = binding.load(root, installation, &cancel);
    if cancel() {
        return Ok(false);
    }
    let source = source?;
    let prepared = effects(&source)?;
    let paths = world
        .paths
        .iter()
        .map(|path| Road {
            hierarchy: 0,
            width: world.layout_policy.width,
            start: true,
            end: true,
            points: path.points().to_vec(),
        })
        .collect::<Vec<_>>();
    let alpha = vec![255; layers.surface_resolution.pow(2)];
    let Some(report) = road_object_placement::fit(
        layers,
        &alpha,
        &paths,
        &source,
        Settings {
            seed: key.seed,
            maximum_work: 2_000_000,
            maximum_poses: 100_000,
        },
        &cancel,
    )?
    else {
        return Ok(false);
    };
    if report.budget_exhausted || report.prefabs.values().any(|r| !r.unsupported.is_empty()) {
        return Err("River object placement is incomplete".into());
    }
    let mut objects = Vec::with_capacity(report.provisional_poses.len());
    let mut instances = Vec::with_capacity(report.provisional_poses.len());
    for pose in &report.provisional_poses {
        if pose.road >= world.paths.len() || pose.transform.scale != [1.; 3] {
            return Err("Invalid river object path or terrain scale".into());
        }
        instances.push(terrain::Instance {
            prefab_id: u32::try_from(pose.road + 1).map_err(|e| e.to_string())?,
            pose: Pose {
                position: pose.transform.position,
                rotation: pose.transform.rotation,
            },
            effects: prepared
                .get(&pose.prefab)
                .ok_or("Missing river terrain effects")?,
        });
        objects.push(Object {
            river: pose.road,
            prefab: pose.prefab.clone(),
            position: pose.transform.position,
            rotation: pose.transform.rotation,
            scale: pose.transform.scale,
        });
    }
    let fitting = serde_json::to_value(&report).map_err(|e| e.to_string())?;
    let edit = terrain::prepare(layers, &instances, 64_000_000, &cancel);
    if cancel() {
        return Ok(false);
    }
    let terrain = edit
        .map_err(|e| format!("River terrain effects failed: {e:?}"))?
        .commit();
    world.objects = Some(Objects {
        source_sha256: binding.source_sha256.clone(),
        seed: key.seed,
        instances: objects,
        fitting,
        terrain,
    });
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn visual_and_audio_components_do_not_block_river_attachments() {
        let path = "assets/bundled/prefabs/autospawn/decor/riversound/emitter.prefab";
        let source: Source = serde_json::from_value(serde_json::json!({
            "format":"papyrus-road-object-source", "schema":4, "adapter":"papyrus-path-objects-1",
            "path_family":"river", "infrastructure_sha256":"a".repeat(64), "sources":[],
            "extractor_sha256":"b".repeat(64), "stages":[], "issues":[],
            "environments":{path:{"root_transform":null,"volumes":[],"checks":[],"issues":[],
                "water_placement":{"version":1,"bodies":[],"issues":[]}}},
            "prefabs":{path:{"rotation":[0.,0.,0.,1.],"scale":[1.,1.,1.],"attributes":[],
                "components":{"AmbienceEmitter":1,"Poolable":1,"RendererLOD":1},"issues":[]}}
        }))
        .unwrap();
        assert!(effects(&source).unwrap()[path].effects().is_empty());
    }
    #[test]
    fn portable_documents_roundtrip_and_reject_changed_or_missing_bindings() {
        let documents = Documents {
            catalog: "catalog".into(),
            infrastructure: "infrastructure".into(),
            objects: "objects".into(),
        };
        let hash = |raw: &str| format!("{:x}", Sha256::digest(raw.as_bytes()));
        let recipe = Recipe {
            catalog_sha256: hash(&documents.catalog),
            infrastructure_sha256: hash(&documents.infrastructure),
            source_sha256: hash(&documents.objects),
            documents: Some(documents),
        };
        recipe.validate().unwrap();
        assert_eq!(recipe.prepare_export().unwrap(), recipe);
        let restored: Recipe =
            serde_json::from_slice(&serde_json::to_vec(&recipe).unwrap()).unwrap();
        assert_eq!(restored, recipe);
        for field in ["catalog_sha256", "infrastructure_sha256", "source_sha256"] {
            let mut value = serde_json::to_value(&recipe).unwrap();
            value[field] = "f".repeat(64).into();
            let invalid: Recipe = serde_json::from_value(value).unwrap();
            assert!(invalid.validate().is_err());
        }
    }
}
