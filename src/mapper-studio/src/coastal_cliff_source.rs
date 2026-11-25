//! Installation-bound metadata for the separate coastal cliff-chain programs.
use crate::{
    decor_source,
    installation::{self, SourceFile},
    metadata_runtime::{self, Script},
    prefab_assets::Catalog,
    road_object_source,
};
use papyrus_terrain::{decor::Kind, decor_material::Removal};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path};

const LIMIT: u64 = 16 * 1024 * 1024;
const PROFILES: [&str; 3] = [
    include_str!("../../../compatibility/cliffs/client-0f703d43-coastal.json"),
    include_str!("../../../compatibility/cliffs/client-8dd4e235-coastal.json"),
    include_str!("../../../compatibility/cliffs/client-58f3263f-coastal.json"),
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    format: String,
    schema: u32,
    id: String,
    native_sha256: String,
    code_sources: Vec<SourceFile>,
    process_sha256: String,
    chain_sha256: String,
    repeat_fitting_sha256: Option<String>,
    material_apply_sha256: String,
    material_remove_sha256: String,
    material_callback_sha256: String,
    topology_flags: std::collections::BTreeMap<String, u32>,
    collider_backfaces: bool,
    material_removal: Removal,
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
impl Profile {
    fn validate(&self) -> Result<(), String> {
        self.material_removal.validate()?;
        if self.format != "papyrus-coastal-cliff-profile"
            || self.schema != 1
            || self.id.is_empty()
            || self.id.len() > 128
            || [
                &self.native_sha256,
                &self.process_sha256,
                &self.chain_sha256,
                &self.material_apply_sha256,
                &self.material_remove_sha256,
                &self.material_callback_sha256,
            ]
            .iter()
            .any(|value| !hash(value))
            || self
                .repeat_fitting_sha256
                .as_ref()
                .is_some_and(|value| !hash(value))
            || self.code_sources.len() != 2
            || self.code_sources[0].relative_path != "GameAssembly.dll"
            || self.code_sources[1].relative_path
                != "RustClient_Data/il2cpp_data/Metadata/global-metadata.dat"
            || self
                .code_sources
                .iter()
                .any(|r| !hash(&r.sha256) || r.bytes == 0 || r.bytes > 1024 * 1024 * 1024)
            || self.topology_flags.len() > 32
            || self
                .topology_flags
                .values()
                .any(|flag| flag.count_ones() != 1)
            || self
                .topology_flags
                .values()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.topology_flags.len()
            || !["Building", "Mountain"].iter().any(|role|
                self.topology_flags.get(*role) == Some(&self.material_removal.excluded_topology))
        {
            return Err("Invalid coastal cliff compatibility profile".into());
        }
        Ok(())
    }
}
fn selected(native: &str) -> Result<(Profile, String), String> {
    for raw in PROFILES {
        let profile: Profile = serde_json::from_str(raw).map_err(|e| e.to_string())?;
        profile.validate()?;
        if profile.native_sha256 == native {
            return Ok((profile, digest(raw.as_bytes())));
        }
    }
    Err("Coastal cliffs need a compatible selected Rust installation".into())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    format: String,
    schema: u32,
    profile_sha256: String,
    metadata: Box<serde_json::value::RawValue>,
    programs: Vec<papyrus_terrain::coastal_cliffs::Program>,
}
pub(crate) struct Source {
    metadata: decor_source::Source,
    programs: Vec<papyrus_terrain::coastal_cliffs::Program>,
    sha256: String,
}

fn verify_stages(
    metadata: &decor_source::Source,
    road: &road_object_source::Source,
) -> Result<(), String> {
    metadata.verify_road_binding(road)?;
    let entries: Vec<_> = road
        .procedural_order()?
        .iter()
        .filter(|entry| entry.kind == "PlaceCliffs")
        .collect();
    if entries.len() != metadata.stages.len() || entries.is_empty() || entries.len() > 16 {
        return Err("Coastal cliff roster differs from the procedural workflow".into());
    }
    for (stage, entry) in metadata.stages.iter().zip(entries) {
        if stage.kind != Kind::WhiteNoise
            || stage.id.parse::<i64>().ok() != Some(entry.object_id)
            || stage.name != entry.description
            || stage.mode != entry.mode
            || stage.enabled != (entry.hierarchy_active && !entry.disabled.unwrap_or(false))
        {
            return Err("Coastal cliff stage differs from its source workflow entry".into());
        }
    }
    Ok(())
}

impl Source {
    pub(crate) fn metadata(&self) -> &decor_source::Source {
        &self.metadata
    }
    pub(crate) fn programs(&self) -> &[papyrus_terrain::coastal_cliffs::Program] {
        &self.programs
    }
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }

    #[allow(clippy::too_many_arguments)]
    fn load(
        root: &Path,
        path: &Path,
        catalog: &Catalog,
        road: &road_object_source::Source,
        profile: &Profile,
        profile_sha: &str,
        topology_sha: &str,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Self, String> {
        if cancelled() {
            return Err("Coastal cliff preparation cancelled".into());
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > LIMIT {
            return Err("Coastal cliff source exceeds 16 MiB".into());
        }
        let doc: Document = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if doc.format != "papyrus-coastal-cliff-source"
            || doc.schema != 1
            || doc.profile_sha256 != profile_sha
        {
            return Err("Coastal cliff source differs from its compatibility profile".into());
        }
        let metadata = decor_source::Source::from_bytes_bound(
            root,
            doc.metadata.get().as_bytes(),
            catalog,
            None,
            cancelled,
        )?;
        if !metadata.preparation_matches(topology_sha, &profile.code_sources)
            || metadata.material_removal() != Some(profile.material_removal)
            || metadata.producer_sha256() != metadata_runtime::coastal_cliff_extractor_sha256()
        {
            return Err(
                "Coastal cliff metadata has a different producer or material policy".into(),
            );
        }
        verify_stages(&metadata, road)?;
        if doc.programs.len() != metadata.stages.len() {
            return Err("Coastal cliff program count differs from metadata".into());
        }
        for (program, stage) in doc.programs.iter().zip(&metadata.stages) {
            program.validate()?;
            if serde_json::to_value(&program.stage).map_err(|e| e.to_string())?
                != serde_json::to_value(stage).map_err(|e| e.to_string())?
            {
                return Err("Coastal cliff program differs from its captured stage".into());
            }
        }
        Ok(Self {
            metadata,
            programs: doc.programs,
            sha256: digest(&bytes),
        })
    }
    fn verify_ready(&self, catalog: &Catalog) -> Result<(), String> {
        let prefabs = self.metadata.coastal_prefabs()?;
        for program in &self.programs {
            let roster: Vec<_> = program
                .stage
                .prefabs
                .iter()
                .map(|path| prefabs.get(path).ok_or("Missing coastal cliff prefab"))
                .collect::<Result<_, _>>()?;
            if !roster
                .iter()
                .any(|p| p.male.is_some() && p.female.is_some())
            {
                return Err("Coastal cliff stage lacks an initial socket pair".into());
            }
        }
        let prepared = self.metadata.prepare_authored_live(catalog)?;
        for (path, fitting) in &prepared.fitting {
            if !fitting.metadata().unsupported.is_empty() {
                return Err(format!("Unsupported cliff fitting metadata for {path}"));
            }
        }
        for (path, terrain) in &prepared.terrain {
            if !terrain.unsupported.is_empty() {
                return Err(format!(
                    "Unsupported cliff terrain effects for {path}: {}",
                    terrain.unsupported.join(", ")
                ));
            }
        }
        Ok(())
    }
}

pub(crate) fn prepare(
    root: &Path,
    catalog: &Catalog,
    road: &road_object_source::Source,
    cancelled: impl Fn() -> bool,
) -> Result<Source, String> {
    resolve(root, catalog, road, None, true, cancelled)
}
pub(crate) fn available(
    root: &Path,
    catalog: &Catalog,
    road: &road_object_source::Source,
    cancelled: impl Fn() -> bool,
) -> Result<Source, String> {
    resolve(root, catalog, road, None, false, cancelled)
}
pub(crate) fn cached(
    root: &Path,
    catalog: &Catalog,
    road: &road_object_source::Source,
    pin: &str,
    cancelled: impl Fn() -> bool,
) -> Result<Source, String> {
    if !hash(pin) {
        return Err("Invalid coastal cliff source fingerprint".into());
    }
    resolve(root, catalog, road, Some(pin), false, cancelled)
}
fn resolve(
    root: &Path,
    catalog: &Catalog,
    road: &road_object_source::Source,
    pin: Option<&str>,
    capture: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Source, String> {
    let _verification = installation::VerificationScope::default();
    if cancelled() {
        return Err("Coastal cliff preparation cancelled".into());
    }
    let (profile, profile_sha) = selected(&catalog.installation.native.sha256)?;
    for source in &profile.code_sources {
        if installation::hash_file(root, &source.relative_path, 1024 * 1024 * 1024, &cancelled)?
            != *source
        {
            return Err(
                "Coastal cliff gameplay code differs from the selected installation".into(),
            );
        }
    }
    let scene = catalog
        .sources
        .iter()
        .find(|r| r.relative_path == "Bundles/shared/assetscenes.bundle")
        .ok_or("Missing coastal cliff scene source")?;
    let mut topology = serde_json::json!({
        "format":"papyrus-topology-profile", "schema":1, "id":profile.id,
        "native_sha256":profile.native_sha256, "scene_sha256":scene.sha256,
        "code_sources":profile.code_sources, "flags":profile.topology_flags,
        "collider_sampling":{"backfaces":profile.collider_backfaces},
        "material_removal":profile.material_removal, "cliff_profile_sha256":profile_sha,
    });
    if let Some(layout) = road.stage_layout() {
        topology["stage_layout"] = layout.into();
    }
    let topology = serde_json::to_vec(&topology).map_err(|e| e.to_string())?;
    let topology_sha = digest(&topology);
    let local = crate::storage::local_data()?;
    let cache = local.join("papyrus-coastal-cliff-cache");
    for path in crate::object_extract::documents(&cache, "coastal.json")? {
        if cancelled() {
            return Err("Coastal cliff preparation cancelled".into());
        }
        if pin.is_some_and(|pin| {
            path.parent()
                .and_then(Path::file_name)
                .and_then(|n| n.to_str())
                != Some(pin)
        }) {
            continue;
        }
        if let Ok(source) = Source::load(
            root,
            &path,
            catalog,
            road,
            &profile,
            &profile_sha,
            &topology_sha,
            &cancelled,
        ) && path
            .parent()
            .and_then(Path::file_name)
            .and_then(|p| p.to_str())
            == Some(source.sha256.as_str())
        {
            source.verify_ready(catalog)?;
            return Ok(source);
        }
    }
    if !capture {
        return Err("Prepared coastal cliff data is unavailable. Prepare the selected Rust installation again.".into());
    }
    std::fs::create_dir_all(&local).map_err(|e| e.to_string())?;
    let temporary = tempfile::Builder::new()
        .prefix(".cliff-prepare-")
        .tempdir_in(&local)
        .map_err(|e| e.to_string())?;
    let profile_path = temporary.path().join("profile.json");
    std::fs::write(&profile_path, topology).map_err(|e| e.to_string())?;
    let output = temporary.path().join("output");
    if !metadata_runtime::run(
        Script::CoastalCliffs,
        &[
            "--rust-root".into(),
            root.as_os_str().to_owned(),
            "--topology-profile".into(),
            profile_path.into_os_string(),
            "--cache-root".into(),
            output.as_os_str().to_owned(),
        ],
        &cancelled,
    )? {
        return Err("Coastal cliff preparation cancelled".into());
    }
    let document = metadata_runtime::document(&output, "coastal.json")?;
    let source = Source::load(
        root,
        &document,
        catalog,
        road,
        &profile,
        &profile_sha,
        &topology_sha,
        &cancelled,
    )?;
    source.verify_ready(catalog)?;
    if cancelled() {
        return Err("Coastal cliff preparation cancelled".into());
    }
    metadata_runtime::publish(&document, &cache, &source.sha256, LIMIT)?;
    Ok(source)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profiles_bind_code_and_removal_roles_without_requiring_fixed_prefab_rosters() {
        for raw in PROFILES {
            let profile: Profile = serde_json::from_str(raw).unwrap();
            let (selected, sha) = selected(&profile.native_sha256).unwrap();
            let role = if profile.id.starts_with("client-58f3263f") { "Mountain" } else { "Building" };
            assert_eq!(profile.material_removal.excluded_topology, profile.topology_flags[role]);
            assert_eq!(sha, digest(raw.as_bytes()));
            assert_eq!(selected.material_removal, profile.material_removal);
            let original: serde_json::Value = serde_json::from_str(raw).unwrap();
            for field in [
                "native_sha256",
                "process_sha256",
                "chain_sha256",
                "repeat_fitting_sha256",
                "material_callback_sha256",
            ] {
                let mut changed = original.clone();
                changed[field] = "invalid".into();
                assert!(
                    serde_json::from_value::<Profile>(changed)
                        .unwrap()
                        .validate()
                        .is_err()
                );
            }
            let mut changed = original;
            changed["material_removal"]["replacement_channels"][0] = 8.into();
            assert!(
                serde_json::from_value::<Profile>(changed)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
        assert!(selected(&"0".repeat(64)).is_err());
    }
    #[test]
    fn cliff_program_binding_rejects_wrong_ids_names_order_and_activation() {
        use crate::road_object_placement::tests::{source, source_value};
        let mut value = source_value();
        value["schema"] = 2.into();
        value["stages"][0]["scene_order"] = 0.into();
        value["workflow"] = serde_json::json!({
            "format":"papyrus-procedural-order", "schema":1, "stages":[
                {"object_id":1,"order":0,"kind":"PlaceRoadObjects","description":"Objects",
                 "mode":-1,"disabled":false,"component_enabled":true,"hierarchy_active":true},
                {"object_id":2,"order":1,"kind":"PlaceCliffs","description":"Cliffs",
                 "mode":-1,"disabled":false,"component_enabled":false,"hierarchy_active":true},
            ],
        });
        let road = source(value);
        let metadata = || -> decor_source::Source {
            serde_json::from_value(serde_json::json!({
                "format":"papyrus-decor-source", "schema":5,
                "adapter":"papyrus-static-decor-collider-5", "scene":"fixture", "sources":[],
                "extractor_sha256":"a".repeat(64), "prefabs":{}, "issues":[],
                "stages":[{"id":"2","order":0,"name":"Cliffs","kind":"white_noise",
                    "folder":"decor/cliffs","enabled":true,"mode":-1,
                    "filter":{"SplatType":-1,"BiomeType":-1,"TopologyAny":-1,"TopologyAll":0,"TopologyNot":0},
                    "filter_mode":1,"anchor_mode":2,"distance":3.,"dithering":0.,
                    "density":0.,"cluster":null,"prefabs":[]}],
            })).unwrap()
        };
        verify_stages(&metadata(), &road).unwrap();
        for field in 0..5 {
            let mut changed = metadata();
            match field {
                0 => changed.stages[0].id = "3".into(),
                1 => changed.stages[0].name = "Other".into(),
                2 => changed.stages[0].enabled = false,
                3 => changed.stages[0].mode = 1,
                _ => changed.stages.clear(),
            }
            assert!(verify_stages(&changed, &road).is_err());
        }
    }
    #[test]
    fn captured_socket_roles_overrides_and_component_counts_are_verified() {
        let path = "assets/bundled/prefabs/autospawn/decor/cliff_coastal/segment.prefab";
        let value = serde_json::json!({
            "format":"papyrus-decor-source","schema":4,"adapter":"papyrus-static-decor-collider-4",
            "scene":"fixture","sources":[],"extractor_sha256":"a".repeat(64),"stages":[],"issues":[],
            "prefabs":{path:{"rotation":[0.,0.,0.,1.],"scale":[1.,1.,1.],"issues":[],
                "components":{"DecorSocketMale":1,"DecorSocketFemale":1,"PlaceCliffParameters":1},
                "attributes":[
                    {"kind":"DecorSocketMale","position":[15.,-5.,0.],"fields":{}},
                    {"kind":"DecorSocketFemale","position":[-15.,0.,0.],"fields":{}},
                    {"kind":"PlaceCliffParameters","position":[0.,0.,0.],"fields":{
                        "CutoffSlopeInitial":45,"CutoffSlopeRepeat":10,"MinTerrainHeight":0,"MaxTerrainHeight":20,
                        "MinCliffHeight":0,"MaxCliffHeight":500,"MinCliffScale":0.85,"MaxCliffScale":1.
                    }}
                ]
            }}
        });
        let source: decor_source::Source = serde_json::from_value(value.clone()).unwrap();
        let data = source.coastal_prefabs().unwrap();
        assert_eq!(data[path].male, Some([15., -5., 0.]));
        assert_eq!(data[path].female, Some([-15., 0., 0.]));
        assert_eq!(data[path].parameters.as_ref().unwrap().minimum_scale, 0.85);
        for change in 0..5 {
            let mut bad = value.clone();
            let prefab = &mut bad["prefabs"][path];
            match change {
                0 => prefab["components"]["DecorSocketMale"] = 2.into(),
                1 => prefab["attributes"][0]["fields"]["FutureSocketRule"] = 1.into(),
                2 => prefab["attributes"][2]["fields"]["MinTerrainHeight"] = 21.into(),
                3 => prefab["attributes"][0]["position"][0] = 10001.into(),
                _ => {
                    let repeated = prefab["attributes"][0].clone();
                    prefab["attributes"].as_array_mut().unwrap().push(repeated);
                }
            }
            let source: decor_source::Source = serde_json::from_value(bad).unwrap();
            assert!(source.coastal_prefabs().is_err());
        }
    }
}
